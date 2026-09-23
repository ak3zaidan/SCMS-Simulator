//! A node-and-edge graph turned into a lane-level road network.
//!
//! The grid generator of the parent module can compute its geometry from the lattice
//! alone: every junction is a right-angled crossroads and every arm points along an axis.
//! The radial and random generators of 04-models.md §1.2 cannot — an arm leaves a spider's
//! centre at `2π·a/arms` and a random network's junctions have whatever geometry the draws
//! produced — so they share this builder instead, which takes the same input the legacy
//! engine's `CustomNetwork` took (a list of node positions and a list of undirected edges,
//! `roads.py`) and produces the full lane-level model: two directed edges per graph edge,
//! lanes offset for right-hand traffic, junction areas, turning connectors, conflict
//! matrices and optional fixed-time signals.
//!
//! It is the same shape of object [`crate::WorldSourceSpec::LegacyJson`] needs, which is
//! why it is public: `world/source/json-legacy` is a parser plus a call to this function.
//!
//! # Geometry
//!
//! Each node gets a **junction radius** `r`, and every street lane is trimmed to start `r`
//! from its own node and end `r` short of the other. A movement through a junction is a
//! connector from an approach lane's end to a departure lane's start, straight when the
//! two tangents are parallel and a quadratic Bézier through the tangent intersection
//! otherwise — the same construction the OSM importer uses, so junctions from the two
//! paths look alike.
//!
//! Right-hand traffic: lane index 0 is the rightmost in the direction of travel, offset
//! `(n − 0.5 − k)·w` to the driver's right of the street's centreline
//! (docs/protocol/vwp-v1.md §4.3).
//!
//! # Id assignment (deterministic, documented)
//!
//! 1. **Junctions**: one per input node, in input order, `id = node index`.
//! 2. **Street edges**: for each kept graph edge in input order, the `a → b` edge then
//!    the `b → a` edge.
//! 3. **Street lanes**: as their edge is created, index `0` first, `0` = rightmost.
//! 4. **Internal edges**: one per junction that has movements, in junction id order,
//!    after every street edge.
//! 5. **Internal lanes**: in movement order, after every street lane.
//! 6. **Movements** at a junction: by approach **arm** in the junction's arm order (by
//!    heading, counter-clockwise from due east, ties by input edge index), then by
//!    approach lane index from the rightmost, then by departure arm in the same arm
//!    order.
//! 7. **Signal plans**: by junction id, among signalised junctions only.
//!
//! No random number is drawn here: a generator that draws (the random one) draws its
//! *node positions*, and this builder is a pure function of them.

use serde::{Deserialize, Serialize};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{EdgeId, JunctionId, LaneId, SignalId};
use v2xw_core::math;

use crate::error::{Result, WorldError};
use crate::model::{
    ClassMask, ConflictMatrix, Connection, Edge, Junction, JunctionControl, Lane, LaneKind,
    RoadClass, SignalHead, SignalHeadKind, SignalPhase, SignalPlan, SignalState, SymbolTable,
    TurnDirection, convex_hull_ring, normalise_angle,
};
use crate::quant::{Q_ANGLE_RAD, Q_POSITION_M, quantise};

/// One undirected street between two nodes.
///
/// Every street is two-way with the same number of lanes each way, which is what the
/// legacy node-and-edge format expresses and what `netgenerate`'s `--default.lanenumber`
/// produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Index of the first node.
    pub a: usize,
    /// Index of the second node.
    pub b: usize,
    /// Lanes in each direction of travel.
    pub lanes_per_direction: u32,
    /// Lane width, metres.
    pub lane_width_m: f64,
    /// Speed limit, m/s.
    pub speed_limit_mps: f64,
    /// The functional class recorded on both directed edges.
    pub road_class: RoadClass,
    /// The street's name, interned into the world's symbol table.
    pub name: Option<String>,
}

/// The default lanes per direction: `netgenerate --default.lanenumber 2`, the value every
/// procedural recipe in the legacy SUMO tooling passed
/// (`legacy/reference/sumo/mapgen.py` L229-249).
pub const DEFAULT_LANES_PER_DIRECTION: u32 = 2;

/// The default lane width, metres: the legacy engine's `lane_width_m`
/// (04-models.md §1.2 preset `legacy`, from `run.py` L412-424).
pub const DEFAULT_LANE_WIDTH_M: f64 = 3.5;

/// The default speed limit, m/s.
///
/// **`TODO: calibrate`**, exactly as the grid generator's is: 13.89 m/s is 50 km/h, an
/// urban default that no source in the cache states for these generators.
pub const DEFAULT_SPEED_LIMIT_MPS: f64 = 13.89;

impl GraphEdge {
    /// A street between two nodes with the default lane count, width and speed.
    pub fn new(a: usize, b: usize) -> Self {
        Self {
            a,
            b,
            lanes_per_direction: DEFAULT_LANES_PER_DIRECTION,
            lane_width_m: DEFAULT_LANE_WIDTH_M,
            speed_limit_mps: DEFAULT_SPEED_LIMIT_MPS,
            road_class: RoadClass::Residential,
            name: None,
        }
    }

    /// The same street with a lane count.
    #[must_use]
    pub fn with_lanes(mut self, lanes_per_direction: u32) -> Self {
        self.lanes_per_direction = lanes_per_direction;
        self
    }

    /// The same street with a speed limit.
    #[must_use]
    pub fn with_speed_mps(mut self, speed_limit_mps: f64) -> Self {
        self.speed_limit_mps = speed_limit_mps;
        self
    }

    /// The same street with a class.
    #[must_use]
    pub fn with_class(mut self, road_class: RoadClass) -> Self {
        self.road_class = road_class;
        self
    }

    /// The same street with a name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Half the carriageway width, metres: the junction's half-extent for this arm.
    pub fn half_width_m(&self) -> f64 {
        f64::from(self.lanes_per_direction) * self.lane_width_m
    }
}

/// Everything [`build_lane_graph`] reads beyond the graph itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphOptions {
    /// Give every junction with at least [`GraphOptions::min_signal_arms`] arms a
    /// fixed-time plan.
    pub signalised: bool,
    /// Signal cycle length, seconds.
    pub cycle_s: f64,
    /// Amber time at the end of each green, seconds.
    pub amber_s: f64,
    /// Height of a signal lantern above the lane, metres.
    pub signal_head_height_m: f64,
    /// How many arms a junction needs before it is worth signalising.
    pub min_signal_arms: usize,
    /// The junction radius as a multiple of the widest arm's half-carriageway.
    ///
    /// A crossroads of two streets `2w` wide needs a radius of about `w` to hold the
    /// crossing area; a multiple above 1 leaves room for the corner radii a real junction
    /// has. **`TODO: calibrate`** — no source in the cache states one.
    pub junction_radius_factor: f64,
    /// The smallest junction radius, metres.
    ///
    /// A radius below this makes every connector shorter than the 1 mm the wire format
    /// requires between successive points, so it is a floor rather than a preference.
    pub min_junction_radius_m: f64,
    /// The largest junction radius, metres — the OSM importer's own cap.
    pub max_junction_radius_m: f64,
    /// The shortest street lane the builder will produce, metres. A graph edge too short
    /// to leave this much between its two junction areas is dropped and counted.
    pub min_street_length_m: f64,
    /// How many points a curved connector is sampled at.
    pub turn_samples: usize,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            signalised: false,
            cycle_s: 60.0,
            amber_s: 3.0,
            signal_head_height_m: 5.0,
            min_signal_arms: 3,
            junction_radius_factor: 1.5,
            min_junction_radius_m: 1.0,
            max_junction_radius_m: 25.0,
            min_street_length_m: 5.0,
            turn_samples: 7,
        }
    }
}

impl GraphOptions {
    /// The options with signals on or off.
    #[must_use]
    pub fn with_signals(mut self, signalised: bool) -> Self {
        self.signalised = signalised;
        self
    }

    /// Checks that the options describe a network that can exist.
    ///
    /// # Errors
    ///
    /// [`WorldError::InvalidParameter`], naming the parameter.
    pub fn validate(&self) -> Result<()> {
        let bad = |parameter: &str, problem: String| WorldError::InvalidParameter {
            parameter: parameter.to_string(),
            problem,
        };
        for (name, v) in [
            ("junction_radius_factor", self.junction_radius_factor),
            ("min_junction_radius_m", self.min_junction_radius_m),
            ("max_junction_radius_m", self.max_junction_radius_m),
            ("min_street_length_m", self.min_street_length_m),
        ] {
            if !(v.is_finite() && v > 0.0) {
                return Err(bad(name, format!("{v} is not positive")));
            }
        }
        if self.max_junction_radius_m < self.min_junction_radius_m {
            return Err(bad(
                "max_junction_radius_m",
                format!(
                    "{} m is below the minimum of {} m",
                    self.max_junction_radius_m, self.min_junction_radius_m
                ),
            ));
        }
        if self.turn_samples < 2 {
            return Err(bad(
                "turn_samples",
                format!("{} points cannot describe a curve", self.turn_samples),
            ));
        }
        if self.signalised {
            if !(self.amber_s.is_finite() && self.amber_s > 0.0) {
                return Err(bad(
                    "amber_s",
                    format!("{} s is not a positive amber time", self.amber_s),
                ));
            }
            if self.cycle_s <= 2.0 * self.amber_s {
                return Err(bad(
                    "cycle_s",
                    format!(
                        "a {} s cycle cannot hold two {} s ambers and two greens",
                        self.cycle_s, self.amber_s
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// What the builder did, and what it had to leave out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphReport {
    /// Nodes in the input graph, all of which become junctions.
    pub nodes: u64,
    /// Undirected edges in the input graph.
    pub graph_edges: u64,
    /// Edges dropped because both ends were the same node.
    pub self_loops_dropped: u64,
    /// Edges dropped because an earlier edge already joined the same two nodes.
    pub duplicate_edges_dropped: u64,
    /// Edges dropped because the two junction areas left no room for a lane between them.
    pub short_edges_dropped: u64,
    /// Junction radii cut back so that a short street still fitted a lane.
    pub radii_shrunk: u64,
    /// Movements skipped because their connector would have been shorter than a
    /// millimetre.
    pub degenerate_movements_dropped: u64,
    /// Movements skipped because they would have been a turnaround
    /// (`netgenerate --no-turnarounds`).
    pub turnarounds_skipped: u64,
    /// Connectors that came out straight.
    pub straight_connectors: u64,
    /// Connectors that came out curved.
    pub curved_connectors: u64,
    /// Junctions that got a signal plan.
    pub junctions_signalised: u64,
}

/// A lane-level network, ready to be handed to [`crate::model::RoadNetwork::new`].
#[derive(Debug, Clone, PartialEq)]
pub struct LaneGraph {
    /// Every lane, in id order.
    pub lanes: Vec<Lane>,
    /// Every edge, in id order.
    pub edges: Vec<Edge>,
    /// Every junction, in id order.
    pub junctions: Vec<Junction>,
    /// Every connection, unsorted — [`crate::model::RoadNetwork::new`] sorts them.
    pub connections: Vec<Connection>,
    /// Every signal plan, in id order.
    pub signals: Vec<SignalPlan>,
    /// The symbol table the street names were interned into.
    pub symbols: SymbolTable,
    /// The network's extent, `(east, north)` metres.
    pub extent_m: (f64, f64),
    /// What was added to every input coordinate to keep the world non-negative (D6).
    pub shift_m: (f64, f64),
    /// What the build did.
    pub report: GraphReport,
}

/// One arm of a junction: an undirected edge seen from one of its two ends.
#[derive(Debug, Clone, Copy)]
struct Arm {
    /// Index into the kept-edge list.
    edge: usize,
    /// The direction of travel away from this node, as an angle: what the arm order and
    /// the turn classification are computed from.
    heading: f64,
    /// Lanes per direction on this arm.
    lanes: u32,
    /// Lane width on this arm, metres.
    width_m: f64,
}

/// One movement through a junction: the geometry the conflict rules reason about.
///
/// Public because the SUMO importer builds the same list from a `net.xml`'s connections
/// and hands it to [`conflict_matrix`], rather than carrying a third copy of the
/// right-of-way rules.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovementGeometry {
    /// The approach lane the movement leaves.
    pub from_lane: LaneId,
    /// The departure lane it enters.
    pub to_lane: LaneId,
    /// Its connector inside the junction, which is also its row in the conflict matrix.
    pub internal: LaneId,
    /// Which way it turns.
    pub turn: TurnDirection,
    /// The heading of travel **into** the junction on the approach, radians.
    pub approach_heading: f64,
    /// Which of the two fixed-time phase groups it belongs to.
    pub phase_group: u16,
}

/// Turns a node-and-edge graph into a lane-level network.
///
/// `nodes` are positions in any frame; the builder translates them so that every
/// coordinate it produces — lane offsets and junction areas included — is non-negative, as
/// D6 requires, and reports the translation in [`LaneGraph::shift_m`]. `graph_edges` are
/// undirected; each becomes two directed edges.
///
/// # Errors
///
/// [`WorldError::InvalidParameter`] for fewer than two nodes, no edges, an edge that names
/// a node outside the list, or options [`GraphOptions::validate`] rejects, and whatever
/// the geometry itself rejects.
#[allow(clippy::too_many_lines)]
pub fn build_lane_graph(
    nodes: &[Vec3],
    graph_edges: &[GraphEdge],
    opts: &GraphOptions,
) -> Result<LaneGraph> {
    opts.validate()?;
    let bad = |parameter: &str, problem: String| WorldError::InvalidParameter {
        parameter: parameter.to_string(),
        problem,
    };
    if nodes.len() < 2 {
        return Err(bad(
            "nodes",
            format!("{} node(s); a network needs at least 2", nodes.len()),
        ));
    }
    if graph_edges.is_empty() {
        return Err(bad(
            "edges",
            "a network needs at least one edge".to_string(),
        ));
    }
    for (i, e) in graph_edges.iter().enumerate() {
        if e.a >= nodes.len() || e.b >= nodes.len() {
            return Err(bad(
                "edges",
                format!(
                    "edge {i} joins nodes {} and {}, but there are only {} nodes",
                    e.a,
                    e.b,
                    nodes.len()
                ),
            ));
        }
        if e.lanes_per_direction == 0 {
            return Err(bad(
                "edges",
                format!("edge {i} has no lanes in its direction of travel"),
            ));
        }
        if !(e.lane_width_m.is_finite() && e.lane_width_m > 0.0) {
            return Err(bad(
                "edges",
                format!("edge {i} has a lane width of {} m", e.lane_width_m),
            ));
        }
        if !(e.speed_limit_mps.is_finite() && e.speed_limit_mps > 0.0) {
            return Err(bad(
                "edges",
                format!("edge {i} has a speed limit of {} m/s", e.speed_limit_mps),
            ));
        }
        if !nodes[e.a].is_finite() || !nodes[e.b].is_finite() {
            return Err(WorldError::NonFinite {
                what: format!("node position of edge {i}"),
            });
        }
    }

    let mut report = GraphReport {
        nodes: nodes.len() as u64,
        graph_edges: graph_edges.len() as u64,
        ..GraphReport::default()
    };

    // --- drop self-loops and duplicates, in input order ---------------------------
    let mut kept: Vec<GraphEdge> = Vec::with_capacity(graph_edges.len());
    let mut seen: Vec<(usize, usize)> = Vec::with_capacity(graph_edges.len());
    for e in graph_edges {
        if e.a == e.b {
            report.self_loops_dropped += 1;
            continue;
        }
        let key = (e.a.min(e.b), e.a.max(e.b));
        if seen.contains(&key) {
            report.duplicate_edges_dropped += 1;
            continue;
        }
        seen.push(key);
        kept.push(e.clone());
    }

    // --- translate so that nothing is negative (D6) --------------------------------
    // The margin is the widest half-carriageway plus the largest junction radius, which
    // bounds how far any generated point can lie outside the node hull.
    let widest_half = kept.iter().map(GraphEdge::half_width_m).fold(0.0, f64::max);
    let margin = widest_half + opts.max_junction_radius_m;
    let min_x = nodes.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let min_y = nodes.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    // The shift is put on the position grid **before** it is applied, so that the value
    // the provenance records is the value the geometry actually used (D9): a random
    // generator's node positions are arbitrary doubles, and a shift recorded to the
    // millimetre but applied in full would make the record a near-miss.
    let shift = (
        quantise(margin - min_x, Q_POSITION_M),
        quantise(margin - min_y, Q_POSITION_M),
    );
    let position: Vec<Vec3> = nodes
        .iter()
        .map(|p| Vec3::new(p.x + shift.0, p.y + shift.1, p.z))
        .collect();

    // --- arms per node, ordered by heading -----------------------------------------
    let mut arms: Vec<Vec<Arm>> = vec![Vec::new(); position.len()];
    for (index, e) in kept.iter().enumerate() {
        for (from, to) in [(e.a, e.b), (e.b, e.a)] {
            let d = Vec3::new(
                position[to].x - position[from].x,
                position[to].y - position[from].y,
                0.0,
            );
            let length = d.norm_2d();
            if length <= 0.0 {
                continue;
            }
            arms[from].push(Arm {
                edge: index,
                heading: math::atan2(d.y, d.x),
                lanes: e.lanes_per_direction,
                width_m: e.lane_width_m,
            });
        }
    }
    for list in &mut arms {
        list.sort_by(|p, q| p.heading.total_cmp(&q.heading).then(p.edge.cmp(&q.edge)));
    }

    // --- junction radii -------------------------------------------------------------
    let mut radius: Vec<f64> = position
        .iter()
        .enumerate()
        .map(|(j, _)| {
            let widest = arms[j]
                .iter()
                .map(|a| f64::from(a.lanes) * a.width_m)
                .fold(0.0, f64::max);
            (widest * opts.junction_radius_factor)
                .clamp(opts.min_junction_radius_m, opts.max_junction_radius_m)
        })
        .collect();
    // A short street cannot hold two full junction areas and a lane between them. The
    // radii at its two ends are cut back proportionally, in edge order so the result is
    // reproducible, and never below the floor.
    for e in &kept {
        let length = position[e.a].distance_2d(position[e.b]);
        let available = length - opts.min_street_length_m;
        let want = radius[e.a] + radius[e.b];
        if available > 0.0 && want > available {
            let scale = available / want;
            let a_new = (radius[e.a] * scale).max(opts.min_junction_radius_m);
            let b_new = (radius[e.b] * scale).max(opts.min_junction_radius_m);
            if a_new < radius[e.a] || b_new < radius[e.b] {
                report.radii_shrunk += 1;
            }
            radius[e.a] = a_new;
            radius[e.b] = b_new;
        }
    }

    // --- street edges and their lanes ------------------------------------------------
    let mut symbols = SymbolTable::new();
    let mut lanes: Vec<Lane> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut junctions: Vec<Junction> = position
        .iter()
        .enumerate()
        .map(|(j, p)| Junction {
            id: JunctionId::new(j as u32),
            position: *p,
            shape: Vec::new(),
            incoming: Vec::new(),
            outgoing: Vec::new(),
            internal: Vec::new(),
            control: JunctionControl::Uncontrolled,
            conflicts: ConflictMatrix::new(0),
            name: None,
        })
        .collect();
    // For each kept edge: the directed edge ids `(a → b, b → a)`, or `None` if it was
    // dropped for being too short.
    let mut directed: Vec<Option<(EdgeId, EdgeId)>> = Vec::with_capacity(kept.len());

    for e in &kept {
        let length = position[e.a].distance_2d(position[e.b]);
        let usable = length - radius[e.a] - radius[e.b];
        if usable < opts.min_street_length_m {
            report.short_edges_dropped += 1;
            directed.push(None);
            continue;
        }
        let name = e.name.as_deref().and_then(|n| symbols.intern_optional(n));
        let mut pair: Vec<EdgeId> = Vec::with_capacity(2);
        for (from, to) in [(e.a, e.b), (e.b, e.a)] {
            let edge_id = EdgeId::new(edges.len() as u32);
            let d = Vec3::new(
                position[to].x - position[from].x,
                position[to].y - position[from].y,
                0.0,
            );
            let f = (d.x / length, d.y / length);
            let right = (f.1, -f.0);
            let mut lane_ids = Vec::with_capacity(e.lanes_per_direction as usize);
            for k in 0..e.lanes_per_direction {
                let offset =
                    (f64::from(e.lanes_per_direction) - 0.5 - f64::from(k)) * e.lane_width_m;
                let start = Vec3::new(
                    position[from].x + f.0 * radius[from] + right.0 * offset,
                    position[from].y + f.1 * radius[from] + right.1 * offset,
                    position[from].z,
                );
                let end = Vec3::new(
                    position[to].x - f.0 * radius[to] + right.0 * offset,
                    position[to].y - f.1 * radius[to] + right.1 * offset,
                    position[to].z,
                );
                let lane_id = LaneId::new(lanes.len() as u32);
                lanes.push(Lane::new(
                    lane_id,
                    edge_id,
                    None,
                    u8::try_from(k).unwrap_or(u8::MAX),
                    LaneKind::Driving,
                    vec![start, end],
                    e.lane_width_m,
                    e.speed_limit_mps,
                    ClassMask::MOTOR_TRAFFIC,
                )?);
                lane_ids.push(lane_id);
                junctions[to].incoming.push(lane_id);
                junctions[from].outgoing.push(lane_id);
            }
            edges.push(Edge {
                id: edge_id,
                from: JunctionId::new(from as u32),
                to: JunctionId::new(to as u32),
                lanes: lane_ids,
                name,
                road_class: e.road_class,
            });
            pair.push(edge_id);
        }
        directed.push(Some((pair[0], pair[1])));
    }

    // --- movements, internal lanes, connections, conflicts ----------------------------
    let mut connections: Vec<Connection> = Vec::new();
    let mut movements_by_junction: Vec<Vec<MovementGeometry>> = vec![Vec::new(); position.len()];

    for j in 0..position.len() {
        let internal_edge = EdgeId::new(edges.len() as u32);
        let mut internal_lane_ids: Vec<LaneId> = Vec::new();
        let mut movements: Vec<MovementGeometry> = Vec::new();
        let reference = arms[j].first().map(|a| a.heading).unwrap_or(0.0);

        for approach in &arms[j] {
            let Some((ab, ba)) = directed[approach.edge] else {
                continue;
            };
            // The edge arriving here is the one whose `to` is this junction.
            let in_edge = if edges[ab.as_usize()].to.as_usize() == j {
                ab
            } else {
                ba
            };
            let in_heading = normalise_angle(approach.heading + core::f64::consts::PI);
            let n_in = edges[in_edge.as_usize()].lanes.len();
            for k in 0..n_in {
                let from_lane = edges[in_edge.as_usize()].lanes[k];
                for departure in &arms[j] {
                    if departure.edge == approach.edge {
                        report.turnarounds_skipped += 1;
                        continue;
                    }
                    let Some((dab, dba)) = directed[departure.edge] else {
                        continue;
                    };
                    let out_edge = if edges[dab.as_usize()].from.as_usize() == j {
                        dab
                    } else {
                        dba
                    };
                    let turn = TurnDirection::from_heading_change(normalise_angle(
                        departure.heading - in_heading,
                    ));
                    if turn == TurnDirection::UTurn {
                        report.turnarounds_skipped += 1;
                        continue;
                    }
                    let n_out = edges[out_edge.as_usize()].lanes.len();
                    // The lane-assignment rule of the grid generator, applied to an
                    // arbitrary junction: right turns from the kerb lane, left turns from
                    // the median lane, straight on from every lane.
                    let out_k = match turn {
                        TurnDirection::Right | TurnDirection::SlightRight => {
                            if k != 0 {
                                continue;
                            }
                            0
                        }
                        TurnDirection::Left | TurnDirection::SlightLeft => {
                            if k + 1 != n_in {
                                continue;
                            }
                            n_out - 1
                        }
                        TurnDirection::Straight => k.min(n_out - 1),
                        TurnDirection::UTurn => continue,
                    };
                    let to_lane = edges[out_edge.as_usize()].lanes[out_k];
                    let start = lanes[from_lane.as_usize()].end();
                    let end = lanes[to_lane.as_usize()].start();
                    if start.distance_2d(end) < Q_POSITION_M {
                        report.degenerate_movements_dropped += 1;
                        continue;
                    }
                    let geometry =
                        connector_geometry(start, in_heading, end, departure.heading, opts);
                    if geometry.len() > 2 {
                        report.curved_connectors += 1;
                    } else {
                        report.straight_connectors += 1;
                    }
                    // A connector inherits the departure lane's limit, which is what a
                    // driver is accelerating to. Read before the push, so no borrow of
                    // `lanes` is alive across it.
                    let connector_speed_mps = lanes[to_lane.as_usize()].speed_limit_mps;
                    let internal = LaneId::new(lanes.len() as u32);
                    lanes.push(Lane::new(
                        internal,
                        internal_edge,
                        Some(JunctionId::new(j as u32)),
                        u8::try_from(internal_lane_ids.len()).unwrap_or(u8::MAX),
                        LaneKind::Internal,
                        geometry,
                        approach.width_m,
                        connector_speed_mps,
                        ClassMask::MOTOR_TRAFFIC,
                    )?);
                    internal_lane_ids.push(internal);
                    movements.push(MovementGeometry {
                        from_lane,
                        to_lane,
                        internal,
                        turn,
                        approach_heading: in_heading,
                        phase_group: phase_group(in_heading, reference),
                    });
                    connections.push(Connection {
                        from_lane,
                        to_lane,
                        via: Some(internal),
                        direction: turn,
                        permitted: true,
                    });
                    connections.push(Connection {
                        from_lane: internal,
                        to_lane,
                        via: None,
                        direction: turn,
                        permitted: true,
                    });
                }
            }
        }

        if !internal_lane_ids.is_empty() {
            edges.push(Edge {
                id: internal_edge,
                from: JunctionId::new(j as u32),
                to: JunctionId::new(j as u32),
                lanes: internal_lane_ids.clone(),
                name: None,
                road_class: RoadClass::Internal,
            });
        }
        junctions[j].internal = internal_lane_ids;
        junctions[j].conflicts = conflict_matrix(&movements, &lanes);
        movements_by_junction[j] = movements;
    }

    // --- control and shapes ------------------------------------------------------------
    let mut signals: Vec<SignalPlan> = Vec::new();
    for j in 0..position.len() {
        let movements = &movements_by_junction[j];
        let arm_count = arms[j]
            .iter()
            .filter(|a| directed[a.edge].is_some())
            .count();
        if movements.is_empty() {
            junctions[j].control = JunctionControl::Uncontrolled;
        } else if opts.signalised && arm_count >= opts.min_signal_arms {
            let plan_id = SignalId::new(signals.len() as u32);
            signals.push(fixed_time_plan(
                plan_id,
                JunctionId::new(j as u32),
                movements,
                &lanes,
                opts,
            ));
            junctions[j].control = JunctionControl::Signalised { plan: plan_id };
            report.junctions_signalised += 1;
        } else {
            junctions[j].control = JunctionControl::Priority;
        }

        // The junction area: the hull of its own position and every lane end that meets
        // it, which is the polygon 04-models.md §1.2 asks for.
        let mut hull_points = vec![junctions[j].position];
        for lane in &junctions[j].incoming {
            hull_points.push(lanes[lane.as_usize()].end());
        }
        for lane in &junctions[j].outgoing {
            hull_points.push(lanes[lane.as_usize()].start());
        }
        junctions[j].shape = convex_hull_ring(&hull_points);
        junctions[j].incoming.sort_unstable();
        junctions[j].incoming.dedup();
        junctions[j].outgoing.sort_unstable();
        junctions[j].outgoing.dedup();
    }

    // --- extent -------------------------------------------------------------------------
    let mut max_x: f64 = 0.0;
    let mut max_y: f64 = 0.0;
    for lane in &lanes {
        for p in &lane.centreline {
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
    }
    for j in &junctions {
        max_x = max_x.max(j.position.x);
        max_y = max_y.max(j.position.y);
    }

    Ok(LaneGraph {
        lanes,
        edges,
        junctions,
        connections,
        signals,
        symbols,
        extent_m: (max_x, max_y),
        shift_m: shift,
        report,
    })
}

/// The geometry of one movement: straight when the two tangents are parallel, a quadratic
/// Bézier through the tangent intersection otherwise.
///
/// The same construction the OSM importer uses. Polynomial arithmetic apart from the two
/// `sin_cos` calls, both of which go through [`v2xw_core::math`], so a connector is
/// bit-identical on every platform.
fn connector_geometry(
    start: Vec3,
    heading_in: f64,
    end: Vec3,
    heading_out: f64,
    opts: &GraphOptions,
) -> Vec<Vec3> {
    let turn = normalise_angle(heading_out - heading_in);
    if turn.abs() < 1e-3 {
        return vec![start, end];
    }
    let (sin_in, cos_in) = math::sin_cos(heading_in);
    let (sin_out, cos_out) = math::sin_cos(heading_out);
    let denominator = cos_in * sin_out - sin_in * cos_out;
    if denominator.abs() < 1e-9 {
        return vec![start, end];
    }
    let chord = Vec3::new(end.x - start.x, end.y - start.y, 0.0);
    let u = (chord.x * sin_out - chord.y * cos_out) / denominator;
    let control = Vec3::new(start.x + cos_in * u, start.y + sin_in * u, start.z);
    if !control.is_finite() || u <= 0.0 || u > 4.0 * chord.norm_2d() + 1.0 {
        return vec![start, end];
    }
    let n = opts.turn_samples.max(2);
    let curve: Vec<Vec3> = (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            let w = 1.0 - t;
            Vec3::new(
                w * w * start.x + 2.0 * w * t * control.x + t * t * end.x,
                w * w * start.y + 2.0 * w * t * control.y + t * t * end.y,
                w * w * start.z + 2.0 * w * t * control.z + t * t * end.z,
            )
        })
        .collect();
    // A sample pair closer than the wire format's 1 mm would make the lane invalid, so a
    // curve that bunches falls back to its chord rather than failing the build.
    if curve
        .windows(2)
        .any(|pair| pair[0].distance(pair[1]) < Q_POSITION_M)
    {
        return vec![start, end];
    }
    curve
}

/// Which phase group an approach belongs to, relative to the junction's reference axis.
///
/// The OSM importer's rule: approaches parallel or anti-parallel to the first arm share
/// the green, everything else gets the other one. A skewed or five-arm junction therefore
/// gets a plan that is safe rather than efficient, which is the right trade for a plan
/// nobody supplied.
fn phase_group(approach_heading: f64, reference: f64) -> u16 {
    let d = normalise_angle(approach_heading - reference).abs();
    let quarter = core::f64::consts::FRAC_PI_4;
    u16::from(!(d <= quarter || d >= 3.0 * quarter))
}

/// A two-phase fixed-time plan over a junction's movements.
///
/// Green, amber, green, amber, so the phases sum to the cycle exactly, which is what
/// [`crate::model::World::validate`] requires. A movement that crosses opposing traffic
/// gets a permissive green: the junction's conflict matrix says to whom it gives way.
fn fixed_time_plan(
    id: SignalId,
    junction: JunctionId,
    movements: &[MovementGeometry],
    lanes: &[Lane],
    opts: &GraphOptions,
) -> SignalPlan {
    let green_s = (opts.cycle_s - 2.0 * opts.amber_s) / 2.0;
    let phase = |group: u16, amber: bool, duration_s: f64| SignalPhase {
        duration_s,
        states: movements
            .iter()
            .map(|m| {
                if m.phase_group != group {
                    SignalState::Red
                } else if amber {
                    SignalState::Amber
                } else if m.turn.crosses_opposing_traffic() {
                    SignalState::GreenYield
                } else {
                    SignalState::Green
                }
            })
            .collect(),
        name: None,
    };
    let mut heads: Vec<SignalHead> = Vec::new();
    let mut seen: Vec<LaneId> = Vec::new();
    for m in movements {
        if seen.contains(&m.from_lane) {
            continue;
        }
        seen.push(m.from_lane);
        let end = lanes[m.from_lane.as_usize()].end();
        heads.push(SignalHead {
            lane: m.from_lane,
            position: Vec3::new(end.x, end.y, end.z + opts.signal_head_height_m),
            kind: SignalHeadKind::Vehicle,
            group: m.phase_group,
        });
    }
    SignalPlan {
        id,
        junction,
        cycle_s: opts.cycle_s,
        offset_s: 0.0,
        controlled: movements.iter().map(|m| m.internal).collect(),
        phases: vec![
            phase(0, false, green_s),
            phase(0, true, opts.amber_s),
            phase(1, false, green_s),
            phase(1, true, opts.amber_s),
        ],
        heads,
    }
}

/// Builds a junction's conflict matrix from its movements.
///
/// Row and column `i` is `movements[i]`, whose `internal` lane is the junction's
/// `internal[i]`: the caller must pass the movements in internal-lane order, which is what
/// [`build_lane_graph`] and the SUMO importer both do.
///
/// The rules are the OSM importer's, which are the grid generator's generalised to
/// arbitrary headings:
///
/// * two movements are **foes** when they end on the same lane (a merge) or their
///   connectors cross; two that start on the same lane are a divergence and never foes;
/// * of two foes, the one that crosses opposing traffic gives way to the one that does
///   not;
/// * of two of equal rank, the one whose partner approaches **from its right** gives way —
///   `b` is on `a`'s right when the heading change from `a`'s approach to `b`'s lies in
///   `(0, π)`;
/// * a pair the rules leave level, such as two opposing left turns, is recorded as a
///   conflict with no precedence either way, because no highway code settles it.
pub fn conflict_matrix(movements: &[MovementGeometry], lanes: &[Lane]) -> ConflictMatrix {
    let mut matrix = ConflictMatrix::new(movements.len());
    let rank = |m: &MovementGeometry| u8::from(!m.turn.crosses_opposing_traffic());
    let bbox = |m: &MovementGeometry| {
        let points = &lanes[m.internal.as_usize()].centreline;
        let mut min = points[0];
        let mut max = points[0];
        for p in points {
            min = Vec3::new(min.x.min(p.x), min.y.min(p.y), 0.0);
            max = Vec3::new(max.x.max(p.x), max.y.max(p.y), 0.0);
        }
        (min, max)
    };
    let boxes: Vec<(Vec3, Vec3)> = movements.iter().map(bbox).collect();
    for a in 0..movements.len() {
        for b in a + 1..movements.len() {
            let (ma, mb) = (&movements[a], &movements[b]);
            if ma.from_lane == mb.from_lane {
                continue;
            }
            let merges = ma.to_lane == mb.to_lane;
            let separated = boxes[a].1.x < boxes[b].0.x
                || boxes[b].1.x < boxes[a].0.x
                || boxes[a].1.y < boxes[b].0.y
                || boxes[b].1.y < boxes[a].0.y;
            let crosses = !separated
                && crate::index::polylines_cross(
                    &lanes[ma.internal.as_usize()].centreline,
                    &lanes[mb.internal.as_usize()].centreline,
                );
            if !(merges || crosses) {
                continue;
            }
            matrix.set_foe(a, b, true);
            let (ra, rb) = (rank(ma), rank(mb));
            if ra < rb {
                matrix.set_response(a, b, true);
            } else if rb < ra {
                matrix.set_response(b, a, true);
            } else {
                let delta = normalise_angle(mb.approach_heading - ma.approach_heading);
                if delta > Q_ANGLE_RAD && delta < core::f64::consts::PI - Q_ANGLE_RAD {
                    matrix.set_response(a, b, true);
                } else if delta < -Q_ANGLE_RAD && delta > -core::f64::consts::PI + Q_ANGLE_RAD {
                    matrix.set_response(b, a, true);
                }
            }
        }
    }
    matrix
}

/// Assembles a [`LaneGraph`] into a [`World`], with one urban land-use zone over its
/// extent and the quantisation transformation every generator records.
///
/// The caller supplies the provenance because the source id, the generator's parameters
/// and its licence are the generator's own; what is shared is the assembly, the zone and
/// the D9 record.
///
/// # Errors
///
/// Whatever the geometry or the world's invariants reject.
pub fn into_world(
    graph: LaneGraph,
    mut provenance: crate::model::WorldProvenance,
    opts: &crate::ImportOptions,
) -> Result<crate::model::World> {
    use crate::model::{
        EnvClass, GeoOrigin, LanduseClass, LanduseZone, Projection, RoadNetwork, Transformation,
        World, ZoneId,
    };

    let (w_m, h_m) = graph.extent_m;
    let landuse = vec![LanduseZone::new(
        ZoneId::new(0),
        [
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(w_m, 0.0),
            Vec3::new_2d(w_m, h_m),
            Vec3::new_2d(0.0, h_m),
        ],
        LanduseClass::Urban,
        EnvClass::Urban,
    )?];
    provenance.record(
        Transformation::new("lane-graph")
            .with("shift_x_m", graph.shift_m.0)
            .with("shift_y_m", graph.shift_m.1)
            .with("nodes", graph.report.nodes)
            .with("graph_edges", graph.report.graph_edges)
            .with("self_loops_dropped", graph.report.self_loops_dropped)
            .with(
                "duplicate_edges_dropped",
                graph.report.duplicate_edges_dropped,
            )
            .with("short_edges_dropped", graph.report.short_edges_dropped)
            .with("radii_shrunk", graph.report.radii_shrunk)
            .with(
                "degenerate_movements_dropped",
                graph.report.degenerate_movements_dropped,
            )
            .with("turnarounds_skipped", graph.report.turnarounds_skipped)
            .with("straight_connectors", graph.report.straight_connectors)
            .with("curved_connectors", graph.report.curved_connectors)
            .with("junctions_signalised", graph.report.junctions_signalised),
    );
    provenance.record(
        Transformation::new("local-tangent-plane")
            .with("projection", Projection::NAME)
            .with("origin_lat_deg", GeoOrigin::NULL_ISLAND.lat_deg)
            .with("origin_lon_deg", GeoOrigin::NULL_ISLAND.lon_deg)
            .with("note", "a procedural world has no real location (D6)"),
    );
    provenance.record(
        Transformation::new("quantise")
            .with("position_m", crate::quant::Q_POSITION_M)
            .with("height_m", crate::quant::Q_HEIGHT_M)
            .with("speed_mps", crate::quant::Q_SPEED_MPS)
            .with("time_s", crate::quant::Q_TIME_S)
            .with("db", crate::quant::Q_DB),
    );
    for layer in ["roads", "landuse"] {
        provenance
            .layers
            .push(crate::model::LayerLicence::new(layer, "Apache-2.0"));
    }
    provenance.notes.push(
        "Generated geometry: no third-party data, so no attribution is required.".to_string(),
    );
    provenance.record_dropped("building_holes", 0);

    let roads = RoadNetwork::new(
        graph.lanes,
        graph.edges,
        graph.junctions,
        graph.connections,
        Vec::new(),
    )?;
    World::builder(GeoOrigin::NULL_ISLAND)
        .roads(roads)
        .signals(graph.signals)
        .landuse(landuse)
        .default_env(opts.default_env)
        .symbols(graph.symbols)
        .provenance(provenance)
        .index_options(opts.index_options)
        .build()
}
