//! What a roadside unit at a signalised junction broadcasts: SAE J2735 SPaT and MAP, and
//! their ETSI SPATEM and MAPEM wrappings, built from the world the run is simulating.
//!
//! # Where the content comes from, and why that is not a firewall breach
//!
//! A deployed RSU does not measure the signal: the traffic signal controller tells it,
//! over NTCIP 1202 or the TSCBM block on a wire in the cabinet, and the RSU encodes what
//! it was told. The junction's geometry is a survey the operator loaded into it. So the
//! two inputs here are the RSU's own infrastructure feeds — the controller state and the
//! surveyed map — and handing them to the unit is the same act as handing a vehicle its
//! GNSS fix. The node runtime still only signs and sends bytes; it never sees the world.
//!
//! # The signal state is the engine's own
//!
//! [`IntersectionFeed::spat_at`] evaluates the junction's [`SignalPlan`] at the instant the
//! message is generated with [`SignalPlan::phase_at`] — the function the mobility model's
//! fixed-time controller (`mobility/intersection/signal-fixed-time`) runs to decide who may
//! enter — and reports each signal *group* the way [`SignalPlan::group_timelines`] does,
//! which is what the page colours each lamp with. So a receiver's view of a light, the
//! lamp on the page and the rule the drivers obey are one function of one plan at one
//! instant. Each group's time to change (`minEndTime`, and `maxEndTime` equal to it: a
//! fixed-time plan's changes are certain) is the time to that group's next state change,
//! read ahead through the plan's own phases.
//!
//! # The geometry
//!
//! One `IntersectionGeometry` per junction, with its reference point at the junction's
//! centre. Every approach lane that feeds a controlled movement is an ingress lane, every
//! lane a controlled movement leaves on an egress lane, and each ingress lane connects to
//! its egress lanes under the signal group of the lamp that faces it (the plan's heads).
//! Node lists run from the stop line outwards, as J2735 lays them, for up to
//! [`LANE_SPAN_M`] along the centreline.
//!
//! # What is uncertain, stated
//!
//! * The SPaT and MAP encoders are hand-written against J2735 and are **not
//!   oracle-validated** (`v2xw_msg::evidence`); the bytes are real UPER of the structure
//!   the codec models.
//! * SPATEM and MAPEM are an ETSI `ItsPduHeader` (message ids `spatem(4)` and `mapem(5)`,
//!   protocol version 2) followed by the SPAT and MAP of ISO TS 19091's DSRC module, whose
//!   structure for the fields filled here is J2735's. The header is 48 bits, octet
//!   aligned, so the wrapping is the two encodings back to back. The protocol version and
//!   the identity of the two structures are recalled, not re-read (TS 103 301 and ISO TS
//!   19091 are not in this repository).
//! * `layerType` and `layerID` are not written, so a MAP here is a conformant `MapData`
//!   encoding but not a CTI 4501-conformant message (`v2xw_msg::j2735::map`).
//! * Which junction a unit serves: the nearest signalised junction within
//!   [`SERVICE_RADIUS_M`] of its mast — a unit is wired to one controller, and one RSU per
//!   intersection is the connected-intersection deployment pattern. A unit with a SPaT or
//!   MAP role and no junction in reach is a scenario error, reported at build.

use std::collections::BTreeMap;

use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{JunctionId, LaneId};
use v2xw_core::time::{Duration, SimTime, WallClock};
use v2xw_msg::j2735::map::{
    Connection, GenericLane, IntersectionGeometry, LaneAttributes, LaneDirection, MapData, NodeXy,
    Position3D, lane_width_cm, xy_offset,
};
use v2xw_msg::j2735::spat::{
    IntersectionReferenceId, IntersectionState, IntersectionStatus, MovementEvent,
    MovementPhaseState, MovementState, Spat, TimeChangeDetails, d_second, minute_of_the_year,
    time_mark,
};
use v2xw_world::{SignalPlan, SignalState, World};

/// How far from its mast a roadside unit may stand from the junction it serves, metres.
///
/// A unit is mounted on the junction's own signal pole or a pole beside it; 60 m is half
/// the length of a short Manhattan block, so it reaches the junction a unit stands at and
/// never the next one. This build's choice, stated.
pub const SERVICE_RADIUS_M: f64 = 60.0;

/// How much of each approach and exit a MAP describes, metres from the junction.
///
/// J2735 leaves the extent to the deployment. 60 m holds a queue of about eight cars and
/// is where a red-light warning has to begin at urban speeds; it is this build's choice.
pub const LANE_SPAN_M: f64 = 60.0;

/// The SPaT broadcast interval: 10 Hz (04-models.md §8.1's default; CTI 4501).
pub const SPAT_INTERVAL: Duration = Duration::from_millis(100);

/// The MAP broadcast interval: 1 Hz (04-models.md §8.1's default; CTI 4501).
pub const MAP_INTERVAL: Duration = Duration::from_secs(1);

/// A J2735 `SignalGroupID` for a plan's head group: `group + 1`, because `0` is
/// "unavailable" and `255` "permanently green" in J2735.
pub const fn signal_group_id(group: u16) -> u8 {
    if group >= 253 { 254 } else { (group + 1) as u8 }
}

/// The J2735 movement phase a world signal state is.
pub const fn movement_phase(state: SignalState) -> MovementPhaseState {
    match state {
        SignalState::Red => MovementPhaseState::StopAndRemain,
        SignalState::RedAmber => MovementPhaseState::PreMovement,
        SignalState::Green => MovementPhaseState::ProtectedMovementAllowed,
        SignalState::GreenYield => MovementPhaseState::PermissiveMovementAllowed,
        SignalState::Amber => MovementPhaseState::ProtectedClearance,
        SignalState::FlashingAmber => MovementPhaseState::CautionConflictingTraffic,
        SignalState::Off => MovementPhaseState::Dark,
    }
}

/// One signal group's timeline over the plan's cycle: `(state, duration)` segments with
/// adjacent equal states merged, as [`SignalPlan::group_timelines`] returns them.
#[derive(Debug, Clone)]
struct GroupTimeline {
    signal_group: u8,
    segments: Vec<(SignalState, f64)>,
}

impl GroupTimeline {
    /// The state at `into` seconds into the cycle and the seconds until it changes,
    /// reading across the cycle boundary when the last segment and the first are the same
    /// state (a green that runs over the top of the cycle ends where the next one ends).
    fn at(&self, into: f64, cycle_s: f64) -> (SignalState, f64) {
        let mut start = 0.0;
        for (i, (state, duration)) in self.segments.iter().enumerate() {
            if into < start + duration || i + 1 == self.segments.len() {
                let mut remaining = (start + duration - into).max(0.0);
                if i + 1 == self.segments.len()
                    && self.segments.len() > 1
                    && self.segments[0].0 == *state
                {
                    remaining += self.segments[0].1;
                } else if self.segments.len() == 1 {
                    // One state for the whole cycle: it never changes, and the time to
                    // change is reported as the end of this cycle.
                    remaining = (cycle_s - into).max(0.0);
                }
                return (*state, remaining);
            }
            start += duration;
        }
        (SignalState::Off, 0.0)
    }
}

/// Everything a unit needs to broadcast one junction's SPaT and MAP.
#[derive(Debug, Clone)]
pub struct IntersectionFeed {
    /// The junction.
    pub junction: JunctionId,
    /// Its position, world-local ENU metres — the MAP's reference point.
    pub position: Vec3,
    /// The index of its plan in [`World::signals`].
    plan: usize,
    /// The J2735 `IntersectionID`.
    pub intersection_id: u16,
    cycle_s: f64,
    offset_s: f64,
    groups: Vec<GroupTimeline>,
    map: MapData,
}

impl IntersectionFeed {
    /// The signalised junction a unit at `mast` serves: the nearest one within
    /// [`SERVICE_RADIUS_M`], horizontally, ties to the lower junction id. The index is into
    /// [`World::signals`].
    pub fn nearest_plan(world: &World, mast: Vec3) -> Option<usize> {
        let mut best: Option<(f64, u32, usize)> = None;
        for (i, plan) in world.signals.iter().enumerate() {
            let Some(j) = world.roads.junctions().get(plan.junction.as_usize()) else {
                continue;
            };
            let d = j.position.distance_2d(mast);
            if d > SERVICE_RADIUS_M {
                continue;
            }
            let key = (d, plan.junction.index(), i);
            if best.is_none_or(|b| (key.0, key.1) < (b.0, b.1)) {
                best = Some(key);
            }
        }
        best.map(|(_, _, i)| i)
    }

    /// Builds the feed for `world.signals[plan]`.
    ///
    /// # Errors
    /// A message naming what the junction lacks: an id beyond J2735's 16-bit
    /// `IntersectionID`, a reference point outside the `Latitude`/`Longitude` ranges, or no
    /// lane a controlled movement uses.
    pub fn build(
        world: &World,
        plan: usize,
        origin: GeoOrigin,
    ) -> Result<IntersectionFeed, String> {
        let p = world
            .signals
            .get(plan)
            .ok_or_else(|| format!("no signal plan {plan}"))?;
        let junction = world
            .roads
            .junctions()
            .get(p.junction.as_usize())
            .ok_or_else(|| format!("plan {plan} names a junction the world does not have"))?;
        let intersection_id = u16::try_from(junction.id.index()).map_err(|_| {
            format!(
                "junction {} is beyond J2735's 16-bit IntersectionID",
                junction.id.index()
            )
        })?;

        // Which approach lane each controlled connector leaves, and which lane it enters.
        let mut approach_of: BTreeMap<LaneId, LaneId> = BTreeMap::new();
        let mut exit_of: BTreeMap<LaneId, LaneId> = BTreeMap::new();
        for c in world.roads.connections() {
            if let Some(via) = c.via {
                approach_of.entry(via).or_insert(c.from_lane);
                exit_of.entry(via).or_insert(c.to_lane);
            }
        }
        let groups: Vec<GroupTimeline> = p
            .group_timelines(|l| approach_of.get(&l).copied())
            .into_iter()
            .map(|(group, segments)| GroupTimeline {
                signal_group: signal_group_id(group),
                segments,
            })
            .collect();

        let map = build_map(world, p, junction.position, origin, &approach_of, &exit_of).map(
            |mut g| {
                g.id = IntersectionReferenceId::new(intersection_id);
                MapData {
                    // Left out: a MAP whose bytes changed every minute would be a MAP a
                    // receiver re-parses every minute for nothing. The geometry is static.
                    time_stamp: None,
                    msg_issue_revision: 1,
                    intersections: vec![g],
                }
            },
        )?;
        Ok(IntersectionFeed {
            junction: junction.id,
            position: junction.position,
            plan,
            intersection_id,
            cycle_s: p.cycle_s,
            offset_s: p.offset_s,
            groups,
            map,
        })
    }

    /// The MAP this unit broadcasts.
    pub fn map(&self) -> &MapData {
        &self.map
    }

    /// The index of the plan in [`World::signals`].
    pub fn plan(&self) -> usize {
        self.plan
    }

    /// The SPaT for this junction at `t`: every signal group's state and when it changes.
    pub fn spat_at(&self, t: SimTime, wall: WallClock) -> Spat {
        let t_s = (t as f64) * 1e-9;
        let mut into = if self.cycle_s > 0.0 {
            (t_s - self.offset_s) % self.cycle_s
        } else {
            0.0
        };
        if into < 0.0 {
            into += self.cycle_s;
        }
        // Seconds into the current UTC hour, the frame `TimeMark` counts in.
        let civil = wall.civil_at(t);
        let ms_in_minute = d_second(wall, t);
        let hour_s = f64::from(civil.minute) * 60.0 + f64::from(ms_in_minute) / 1000.0;
        let states: Vec<MovementState> = self
            .groups
            .iter()
            .map(|g| {
                let (state, remaining) = g.at(into, self.cycle_s);
                let end = time_mark(hour_s + remaining);
                MovementState::current(
                    g.signal_group,
                    MovementEvent::timed(
                        movement_phase(state),
                        TimeChangeDetails {
                            start_time: None,
                            min_end_time: end,
                            max_end_time: Some(end),
                            likely_time: None,
                            confidence: None,
                            next_time: None,
                        },
                    ),
                )
            })
            .collect();
        Spat {
            time_stamp: Some(minute_of_the_year(wall, t)),
            intersections: vec![IntersectionState {
                id: IntersectionReferenceId::new(self.intersection_id),
                revision: 1,
                status: IntersectionStatus::FIXED_TIME_OPERATION,
                moy: Some(minute_of_the_year(wall, t)),
                time_stamp: Some(ms_in_minute),
                states,
            }],
        }
    }

    /// The state and seconds-to-change of every signal group at `t`, in group order — the
    /// same numbers [`IntersectionFeed::spat_at`] encodes, for a test to compare against
    /// the plan.
    pub fn group_states_at(&self, t: SimTime) -> Vec<(u8, SignalState, f64)> {
        let t_s = (t as f64) * 1e-9;
        let mut into = if self.cycle_s > 0.0 {
            (t_s - self.offset_s) % self.cycle_s
        } else {
            0.0
        };
        if into < 0.0 {
            into += self.cycle_s;
        }
        self.groups
            .iter()
            .map(|g| {
                let (s, r) = g.at(into, self.cycle_s);
                (g.signal_group, s, r)
            })
            .collect()
    }
}

/// The J2735 node list of one lane: the point nearest the junction first, then the rest
/// outwards, for up to [`LANE_SPAN_M`], each offset from its predecessor (the first from
/// the reference point). `toward` is true for an ingress lane, whose centreline runs
/// *toward* the junction and is therefore read from its end.
fn node_list(points: &[Vec3], toward: bool, reference: Vec3) -> Option<Vec<NodeXy>> {
    let ordered: Vec<Vec3> = if toward {
        points.iter().rev().copied().collect()
    } else {
        points.to_vec()
    };
    let mut nodes = Vec::new();
    let mut prev = reference;
    let mut walked = 0.0;
    let mut last: Option<Vec3> = None;
    for p in ordered {
        if let Some(l) = last {
            walked += l.distance_2d(p);
            if walked > LANE_SPAN_M && nodes.len() >= 2 {
                break;
            }
        }
        let off = xy_offset(p.x - prev.x, p.y - prev.y)?;
        nodes.push(NodeXy {
            delta: v2xw_msg::j2735::map::NodeOffset::Xy(off),
        });
        prev = p;
        last = Some(p);
        if nodes.len() == 63 {
            break;
        }
    }
    (nodes.len() >= 2).then_some(nodes)
}

/// The geometry of one signalised junction.
fn build_map(
    world: &World,
    plan: &SignalPlan,
    reference: Vec3,
    origin: GeoOrigin,
    approach_of: &BTreeMap<LaneId, LaneId>,
    exit_of: &BTreeMap<LaneId, LaneId>,
) -> Result<IntersectionGeometry, String> {
    // The lanes, in id order: every approach and every exit a controlled movement uses.
    let mut ingress: BTreeMap<LaneId, Vec<(LaneId, u8)>> = BTreeMap::new();
    let mut egress: Vec<LaneId> = Vec::new();
    let group_of = |approach: LaneId| {
        plan.heads
            .iter()
            .find(|h| h.lane == approach)
            .map(|h| signal_group_id(h.group))
    };
    for via in &plan.controlled {
        let (Some(from), Some(to)) = (approach_of.get(via), exit_of.get(via)) else {
            continue;
        };
        let Some(sg) = group_of(*from) else {
            continue;
        };
        ingress.entry(*from).or_default().push((*to, sg));
        if !egress.contains(to) {
            egress.push(*to);
        }
    }
    egress.sort_unstable();
    if ingress.is_empty() {
        return Err(format!(
            "junction {} has a signal plan with no controlled movement from a lane with a \
             signal head",
            plan.junction.index()
        ));
    }
    // LaneID 1.. in (ingress, egress) order; J2735 allows 255 lanes and 16 connections.
    let mut lane_id: BTreeMap<LaneId, u8> = BTreeMap::new();
    let mut next: u16 = 1;
    for l in ingress.keys().chain(egress.iter()) {
        if next > 255 {
            break;
        }
        lane_id.entry(*l).or_insert_with(|| {
            let id = next as u8;
            next += 1;
            id
        });
    }
    let mut lanes = Vec::new();
    let mut widths: Vec<f64> = Vec::new();
    for (from, moves) in &ingress {
        let Some(&id) = lane_id.get(from) else {
            continue;
        };
        let Some(lane) = world.roads.try_lane(*from) else {
            continue;
        };
        let Some(nodes) = node_list(&lane.centreline, true, reference) else {
            continue;
        };
        widths.push(lane.width_m);
        let mut connects_to: Vec<Connection> = moves
            .iter()
            .filter_map(|(to, sg)| lane_id.get(to).map(|t| Connection::signalised(*t, *sg)))
            .collect();
        connects_to.sort_unstable();
        connects_to.dedup();
        connects_to.truncate(16);
        lanes.push(GenericLane {
            lane_id: id,
            ingress_approach: None,
            egress_approach: None,
            attributes: LaneAttributes::vehicle(LaneDirection::INGRESS),
            maneuvers: None,
            nodes,
            connects_to,
        });
    }
    for to in &egress {
        let Some(&id) = lane_id.get(to) else { continue };
        let Some(lane) = world.roads.try_lane(*to) else {
            continue;
        };
        let Some(nodes) = node_list(&lane.centreline, false, reference) else {
            continue;
        };
        widths.push(lane.width_m);
        lanes.push(GenericLane {
            lane_id: id,
            ingress_approach: None,
            egress_approach: None,
            attributes: LaneAttributes::vehicle(LaneDirection::EGRESS),
            maneuvers: None,
            nodes,
            connects_to: Vec::new(),
        });
    }
    if lanes.is_empty() {
        return Err(format!(
            "junction {} has no lane whose geometry a MAP can carry",
            plan.junction.index()
        ));
    }
    widths.sort_by(f64::total_cmp);
    let lane_width = widths[widths.len() / 2];
    let (lat, lon, alt) = origin.to_geodetic(reference);
    let lat =
        v2xw_msg::j2735::bsm::latitude(lat).ok_or("the junction's latitude is out of range")?;
    let lon =
        v2xw_msg::j2735::bsm::longitude(lon).ok_or("the junction's longitude is out of range")?;
    Ok(IntersectionGeometry {
        id: IntersectionReferenceId::new(0),
        revision: 1,
        ref_point: Position3D {
            lat,
            lon,
            elevation: Some(v2xw_msg::j2735::bsm::elevation(alt)),
        },
        lane_width_cm: Some(lane_width_cm(lane_width)),
        lanes,
    })
}

/// How one unit's intersection messages go on the air: the J2735 `MessageFrame` of the US
/// stack, or the ETSI SPATEM and MAPEM of the European one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfraFraming {
    /// A J2735 `MessageFrame` (WSMP).
    J2735,
    /// An ETSI `ItsPduHeader` with the unit's station id in front of the bare PDU
    /// (GeoNetworking/BTP).
    Etsi {
        /// The `stationId` the header carries.
        station_id: u32,
    },
}

impl IntersectionFeed {
    /// The SPaT at `t`, encoded for the air.
    ///
    /// # Errors
    /// The encoder's refusal, which names the field.
    pub fn spat_bytes(
        &self,
        t: SimTime,
        wall: WallClock,
        framing: InfraFraming,
    ) -> Result<Vec<u8>, String> {
        let spat = self.spat_at(t, wall);
        match framing {
            InfraFraming::J2735 => v2xw_msg::j2735::spat::encode_message_frame(&spat)
                .map(|e| e.bytes)
                .map_err(|e| e.to_string()),
            InfraFraming::Etsi { station_id } => {
                let body = v2xw_msg::j2735::spat::encode_spat(&spat).map_err(|e| e.to_string())?;
                v2xw_msg::j2735::infra::its_wrap(
                    v2xw_msg::MsgType::Spat,
                    v2xw_msg::j2735::infra::SPATEM_MESSAGE_ID,
                    station_id,
                    &body.bytes,
                )
                .map_err(|e| e.to_string())
            }
        }
    }

    /// The MAP, encoded for the air.
    ///
    /// # Errors
    /// The encoder's refusal, which names the field.
    pub fn map_bytes(&self, framing: InfraFraming) -> Result<Vec<u8>, String> {
        match framing {
            InfraFraming::J2735 => v2xw_msg::j2735::map::encode_message_frame(&self.map)
                .map(|e| e.bytes)
                .map_err(|e| e.to_string()),
            InfraFraming::Etsi { station_id } => {
                let body =
                    v2xw_msg::j2735::map::encode_map(&self.map).map_err(|e| e.to_string())?;
                v2xw_msg::j2735::infra::its_wrap(
                    v2xw_msg::MsgType::Map,
                    v2xw_msg::j2735::infra::MAPEM_MESSAGE_ID,
                    station_id,
                    &body.bytes,
                )
                .map_err(|e| e.to_string())
            }
        }
    }
}
