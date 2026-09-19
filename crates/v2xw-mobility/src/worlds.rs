//! Small synthetic worlds this crate builds for its own validation runs.
//!
//! 04-models.md §2.9 asks the fundamental-diagram check to run "on the `tr36885-freeway`
//! and `todisco` highway worlds". Neither exists yet — the importers that would produce
//! them are §1.2's business — so the check runs on the geometry the measurement actually
//! needs: a **closed single-lane ring**, which is the classical setting for a fundamental
//! diagram because the density is exactly `N / L` and stays there, with no inflow boundary
//! condition to argue about.
//!
//! [`ring`] builds one as a real [`World`]: junctions on a circle, one edge between
//! consecutive junctions, permitted straight-on connections all the way round. Everything
//! downstream — the spatial index, the leader search, the router — then works on it exactly
//! as it works on an imported city, which is the point: a validation run that used a
//! special-cased geometry would validate the special case.
//!
//! [`rebuild`] is the other half: it copies a world, lets a test edit its lanes and
//! connections (to ban a turn or mis-tag a sidewalk) and rebuilds it through
//! [`v2xw_world::WorldBuilder`], so the content hash is recomputed rather than invalidated.

use v2xw_core::geom::Vec3;
use v2xw_core::ids::{EdgeId, JunctionId, LaneId};
use v2xw_core::math;
use v2xw_world::{
    ClassMask, ConflictMatrix, Connection, Crossing, Edge, GeoOrigin, Junction, JunctionControl,
    Lane, LaneKind, RoadClass, RoadNetwork, SignalPlan, TurnDirection, World, WorldProvenance,
    WorldSourceKind,
};

use crate::error::{MobError, Result};

/// A ring road's shape.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RingParams {
    /// How long the ring is, metres, measured along the driving lane's centreline. The
    /// generated polygon is fitted so its perimeter is this length to within a millimetre
    /// per vertex.
    pub circumference_m: f64,
    /// How many junctions (and therefore edges) the ring is cut into. More edges mean more
    /// lane boundaries for the leader search to cross, which is the interesting case.
    pub segments: u32,
    /// How many points to put on each edge's centreline. The polygon approximates a circle,
    /// so more points mean a rounder ring; the measurement uses the true polyline length
    /// either way.
    pub points_per_segment: u32,
    /// Lanes per direction of travel.
    pub lanes: u32,
    /// Lane width, metres.
    pub lane_width_m: f64,
    /// Speed limit, m/s.
    pub speed_limit_mps: f64,
}

impl Default for RingParams {
    /// A one-kilometre single-lane ring at 33.3 m/s — the Kesting 2010 car's desired speed,
    /// so the free branch of the diagram is the model's own free-road behaviour and not an
    /// artefact of the speed limit.
    fn default() -> Self {
        Self {
            circumference_m: 1000.0,
            segments: 8,
            points_per_segment: 16,
            lanes: 1,
            lane_width_m: 3.5,
            speed_limit_mps: 33.3,
        }
    }
}

impl RingParams {
    /// A ring of a given length, everything else as the default.
    pub fn of_length(circumference_m: f64) -> Self {
        Self {
            circumference_m,
            ..Self::default()
        }
    }

    /// The radius of the circle whose *inscribed polygon* of
    /// `segments · points_per_segment` sides has the requested perimeter.
    ///
    /// The lanes are polylines, so their length is the polygon's perimeter, not the
    /// circle's circumference. Solving for the radius here is what makes
    /// [`World::counts`]-level bookkeeping agree with the ring's nominal length: the
    /// density in the fundamental diagram is `N / L`, and an `L` that was 0.3 % off would
    /// shift every point of the diagram by the same 0.3 %.
    pub fn radius_m(&self) -> f64 {
        let sides = f64::from(self.segments * self.points_per_segment.max(1));
        // perimeter = 2·n·R·sin(π/n)  ⇒  R = perimeter / (2·n·sin(π/n))
        self.circumference_m / (2.0 * sides * math::sin(core::f64::consts::PI / sides))
    }
}

/// Builds a closed ring road (04-models.md §2.9's validation geometry).
///
/// Ids are assigned in a documented order: junction `i` is at angle `2πi/segments`
/// counter-clockwise from the positive `x` axis, edge `i` runs from junction `i` to
/// junction `i+1 mod segments`, and edge `i`'s lanes are `i·lanes …`, index `0` outermost
/// (the rightmost lane for counter-clockwise travel).
///
/// # Errors
///
/// [`MobError::InvalidParameter`] if the parameters cannot make a ring, and whatever the
/// world model rejects.
pub fn ring(params: &RingParams) -> Result<World> {
    if params.segments < 2 {
        return Err(MobError::InvalidParameter {
            name: "segments",
            value: params.segments.to_string(),
            why: "a ring needs at least two segments",
        });
    }
    if params.lanes == 0 {
        return Err(MobError::InvalidParameter {
            name: "lanes",
            value: params.lanes.to_string(),
            why: "a ring needs at least one lane",
        });
    }
    let segments = params.segments as usize;
    let per_segment = params.points_per_segment.max(1) as usize;
    let radius = params.radius_m();
    let lanes_per_edge = params.lanes as usize;

    let angle_of = |vertex: usize| {
        core::f64::consts::TAU * (vertex as f64) / ((segments * per_segment) as f64)
    };
    let point_at = |vertex: usize, r: f64| {
        let a = angle_of(vertex);
        let (s, c) = math::sin_cos(a);
        Vec3::new(r * c, r * s, 0.0)
    };

    let mut junctions: Vec<Junction> = Vec::with_capacity(segments);
    for j in 0..segments {
        junctions.push(Junction {
            id: JunctionId::new(j as u32),
            position: point_at(j * per_segment, radius),
            shape: Vec::new(),
            incoming: Vec::new(),
            outgoing: Vec::new(),
            internal: Vec::new(),
            control: JunctionControl::Uncontrolled,
            conflicts: ConflictMatrix::new(0),
            name: None,
        });
    }

    let mut lanes: Vec<Lane> = Vec::with_capacity(segments * lanes_per_edge);
    let mut edges: Vec<Edge> = Vec::with_capacity(segments);
    for e in 0..segments {
        let edge_id = EdgeId::new(e as u32);
        let from = JunctionId::new(e as u32);
        let to = JunctionId::new(((e + 1) % segments) as u32);
        let mut edge_lanes = Vec::with_capacity(lanes_per_edge);
        for index in 0..lanes_per_edge {
            // Index 0 is the rightmost lane, which on a counter-clockwise ring is the
            // outermost one.
            let r = radius
                + (0.5 * f64::from(params.lanes as i32 - 1) - index as f64) * params.lane_width_m;
            let centreline: Vec<Vec3> = (0..=per_segment)
                .map(|k| point_at(e * per_segment + k, r))
                .collect();
            let id = LaneId::new(lanes.len() as u32);
            lanes.push(Lane::new(
                id,
                edge_id,
                None,
                u8::try_from(index).unwrap_or(u8::MAX),
                LaneKind::Driving,
                centreline,
                params.lane_width_m,
                params.speed_limit_mps,
                ClassMask::MOTOR_TRAFFIC,
            )?);
            edge_lanes.push(id);
            junctions[e].outgoing.push(id);
            junctions[(e + 1) % segments].incoming.push(id);
        }
        edges.push(Edge {
            id: edge_id,
            from,
            to,
            lanes: edge_lanes,
            name: None,
            road_class: RoadClass::Motorway,
        });
    }
    for j in &mut junctions {
        j.incoming.sort_unstable();
        j.outgoing.sort_unstable();
    }

    // One straight-on connection per lane, from each edge to the next: no internal
    // connectors, because a ring has no movements to arbitrate.
    let mut connections: Vec<Connection> = Vec::with_capacity(segments * lanes_per_edge);
    for e in 0..segments {
        let next = (e + 1) % segments;
        for index in 0..lanes_per_edge {
            connections.push(Connection {
                from_lane: LaneId::new((e * lanes_per_edge + index) as u32),
                to_lane: LaneId::new((next * lanes_per_edge + index) as u32),
                via: None,
                direction: TurnDirection::Straight,
                permitted: true,
            });
        }
    }

    let roads = RoadNetwork::new(lanes, edges, junctions, connections, Vec::new())?;
    let provenance = WorldProvenance::new(
        WorldSourceKind::Synthetic,
        format!(
            "v2xw-mobility::worlds::ring(circumference_m={}, segments={}, lanes={})",
            params.circumference_m, params.segments, params.lanes
        ),
        "",
        GeoOrigin::NULL_ISLAND,
    );
    Ok(World::builder(GeoOrigin::NULL_ISLAND)
        .roads(roads)
        .provenance(provenance)
        .bbox_margin_m(10.0)
        .build()?)
}

/// The driving lanes of a ring, in travel order starting from lane `0` of edge `0`.
///
/// The order is the order a vehicle drives them, which is what a route is.
pub fn ring_cycle(world: &World, lane_index: u8) -> Vec<LaneId> {
    let mut out = Vec::new();
    let mut lane = world
        .roads
        .lanes()
        .iter()
        .find(|l| l.index == lane_index)
        .map(|l| l.id);
    let start = lane;
    while let Some(current) = lane {
        out.push(current);
        let next = world
            .successors(current)
            .iter()
            .find(|c| c.permitted)
            .map(|c| c.via.unwrap_or(c.to_lane));
        if next == start || next.is_none() {
            break;
        }
        lane = next;
        if out.len() > world.roads.lanes().len() {
            break; // not a simple cycle; stop rather than spin
        }
    }
    out
}

/// The total length of a lane cycle, metres, summed in id order (ADR 0004 decision 4).
pub fn cycle_length_m(world: &World, cycle: &[LaneId]) -> f64 {
    let mut pairs: Vec<(LaneId, f64)> = cycle
        .iter()
        .map(|l| (*l, world.lane(*l).length_m))
        .collect();
    pairs.sort_by_key(|(l, _)| *l);
    math::sum_sorted_by_key(pairs)
}

/// A world's mutable parts, for a test or a scenario that needs to edit geometry.
///
/// Everything a caller may change is a plain `Vec`; [`rebuild_with`] puts it back together
/// through [`v2xw_world::WorldBuilder`], which re-quantises, re-validates and re-hashes.
#[derive(Debug, Clone)]
pub struct WorldEdit {
    /// The lanes, in id order.
    pub lanes: Vec<Lane>,
    /// The edges, in id order.
    pub edges: Vec<Edge>,
    /// The junctions, in id order.
    pub junctions: Vec<Junction>,
    /// The connections; [`v2xw_world::RoadNetwork::new`] re-sorts them.
    pub connections: Vec<Connection>,
    /// The crossings, in id order.
    pub crossings: Vec<Crossing>,
    /// The signal plans, in id order.
    pub signals: Vec<SignalPlan>,
}

/// Copies `world`, lets `edit` change any of its geometry, and rebuilds it.
///
/// [`World::from_parts`] cannot be used for this: it verifies the stored content hash
/// against the geometry, which is exactly what an edit invalidates. Going back through the
/// builder recomputes the hash, so the result is a world like any other.
///
/// The bounding box is **recomputed** from the geometry rather than copied, because an edit
/// may have moved geometry outside the old one. A world whose original box carried a margin
/// therefore comes back with a tighter box and a different content hash even when nothing
/// was edited; the road network itself is unchanged, which is what a caller of this
/// function is after.
///
/// # Errors
///
/// Whatever the world model rejects about the edited geometry.
pub fn rebuild_with(world: &World, edit: impl FnOnce(&mut WorldEdit)) -> Result<World> {
    let mut parts = WorldEdit {
        lanes: world.roads.lanes().to_vec(),
        edges: world.roads.edges().to_vec(),
        junctions: world.roads.junctions().to_vec(),
        connections: world.roads.connections().to_vec(),
        crossings: world.roads.crossings().to_vec(),
        signals: world.signals.clone(),
    };
    edit(&mut parts);
    let roads = RoadNetwork::new(
        parts.lanes,
        parts.edges,
        parts.junctions,
        parts.connections,
        parts.crossings,
    )?;
    let mut builder = World::builder(world.origin)
        .roads(roads)
        .buildings(world.buildings.clone())
        .signals(parts.signals)
        .sites(world.sites.clone())
        .landuse(world.landuse.clone())
        .symbols(world.symbols.clone())
        .default_env(world.default_env)
        .index_options(world.index_options)
        .provenance(world.provenance.clone());
    if let Some(terrain) = world.terrain.clone() {
        builder = builder.terrain(terrain);
    }
    Ok(builder.build()?)
}

/// [`rebuild_with`] for the common case: edit the lanes and the connections only.
///
/// # Errors
///
/// Whatever the world model rejects about the edited geometry.
pub fn rebuild(
    world: &World,
    edit: impl FnOnce(&mut Vec<Lane>, &mut Vec<Connection>),
) -> Result<World> {
    rebuild_with(world, |parts| {
        edit(&mut parts.lanes, &mut parts.connections)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_is_a_closed_cycle_of_the_requested_length() {
        let params = RingParams::default();
        let w = ring(&params).expect("a ring");
        w.validate().expect("the ring is a valid world");
        let cycle = ring_cycle(&w, 0);
        assert_eq!(cycle.len(), params.segments as usize);
        let length = cycle_length_m(&w, &cycle);
        assert!(
            (length - params.circumference_m).abs() < 0.05,
            "ring length {length} m against the requested {} m",
            params.circumference_m
        );
        // It closes: following the successors from the last lane returns to the first.
        let last = *cycle.last().unwrap();
        let back = w
            .successors(last)
            .iter()
            .find(|c| c.permitted)
            .map(|c| c.via.unwrap_or(c.to_lane));
        assert_eq!(back, Some(cycle[0]));
    }

    #[test]
    fn a_multi_lane_ring_has_adjacent_lanes() {
        let params = RingParams {
            lanes: 2,
            ..RingParams::default()
        };
        let w = ring(&params).expect("a ring");
        w.validate().expect("valid");
        assert_eq!(w.roads.lanes().len(), 2 * params.segments as usize);
        let edge = w.edge(EdgeId::new(0));
        assert_eq!(edge.lanes.len(), 2);
        assert_eq!(w.lane(edge.lanes[0]).index, 0);
        assert_eq!(w.lane(edge.lanes[1]).index, 1);
        // The outer lane (index 0, the rightmost for counter-clockwise travel) is longer.
        assert!(w.lane(edge.lanes[0]).length_m > w.lane(edge.lanes[1]).length_m);
    }

    #[test]
    fn two_rings_with_the_same_parameters_are_identical() {
        let a = ring(&RingParams::default()).expect("a ring");
        let b = ring(&RingParams::default()).expect("a ring");
        assert_eq!(a.content_hash, b.content_hash);
    }

    #[test]
    fn a_rebuilt_world_keeps_its_geometry_and_rehashes() {
        let w = ring(&RingParams::default()).expect("a ring");
        let same = rebuild(&w, |_, _| {}).expect("rebuilt");
        assert_eq!(same.roads, w.roads, "a no-op edit changes no geometry");
        let edited = rebuild(&w, |_, connections| {
            connections[0].permitted = false;
        })
        .expect("rebuilt");
        assert_ne!(
            edited.content_hash, same.content_hash,
            "and a real edit changes the content hash"
        );
        assert!(!edited.roads.connections()[0].permitted);
    }

    #[test]
    fn a_ring_refuses_impossible_parameters() {
        assert!(
            ring(&RingParams {
                segments: 1,
                ..RingParams::default()
            })
            .is_err()
        );
        assert!(
            ring(&RingParams {
                lanes: 0,
                ..RingParams::default()
            })
            .is_err()
        );
    }
}
