//! Cross-validation of the hand-written J2735 SPaT and MAP codecs against `pycrate`.
//!
//! # Read this before trusting a SPaT or MAP byte count
//!
//! **This test cannot run on the machine it was written on, and it says so rather than
//! passing.** Two things it needs are absent:
//!
//! * `third_party/asn1/j2735/` — the SAE modules, git-ignored by build decision D3. The
//!   directory does not exist in this checkout, so the ASN.1 could not even be re-read
//!   while [`v2xw_msg::j2735::spat`] and [`v2xw_msg::j2735::map`] were written.
//! * The `pycrate` environment that `tests/oracle/compile_j2735.py` builds. It was present
//!   when the BSM codec was validated (235 vectors, byte-identical) and is not on disk now.
//!
//! So the vectors below are written, complete and unexecuted. Until someone runs them, the
//! SPaT and MAP bytes are `real UPER, not yet oracle-validated` in
//! [`v2xw_msg::evidence`], the codec's card says the same, and no report may call them
//! byte-exact. Running this test is what changes that, and nothing else does — in
//! particular, the crate's own round-trip tests cannot: encoder and decoder share every
//! assumption, so a misread constraint round-trips perfectly. The standing example is the
//! asymmetric `Longitude` lower bound, which passed 122 of this crate's unit tests and
//! failed 235 of 235 oracle vectors.
//!
//! # What it checks, and why each direction earns its place
//!
//! | Direction | Catches |
//! |---|---|
//! | Rust encodes → bytes compared to `pycrate`'s | a misread constraint, a wrong field width, a missing extension bit — every assumption in `j2735::map::assumptions` |
//! | Rust encodes → `pycrate` decodes → fields compared | a field written in the wrong place that happens to be the right width |
//! | `pycrate` encodes → Rust decodes → fields compared | a decoder that is wrong in the same way as the encoder |
//! | `pycrate` builds a message using an element the codec refuses | that the refusal happens, rather than a plausible message assembled from misaligned bits |
//!
//! The last one is the reason the codecs refuse rather than skip, and it is the only way to
//! test it: the Rust side cannot construct a `ComputedLane` or a `DescriptiveName`, so the
//! bytes have to come from an implementation that can.
//!
//! # Running it
//!
//! ```sh
//! uv venv --python 3.12 /tmp/j2735-oracle/.venv
//! VIRTUAL_ENV=/tmp/j2735-oracle/.venv uv pip install pycrate
//! V2XW_J2735_ASN1_DIR=<the J2735 .asn directory> V2XW_J2735_ORACLE_DIR=/tmp/j2735-oracle \
//!   /tmp/j2735-oracle/.venv/bin/python crates/v2xw-msg/tests/oracle/compile_j2735.py
//! V2XW_J2735_ORACLE_DIR=/tmp/j2735-oracle cargo test -p v2xw-msg --test j2735_infra_oracle
//! ```
//!
//! # When it fails
//!
//! A failure here is information, not a defect to paper over. The likely causes, in order:
//!
//! 1. An extension marker in [`v2xw_msg::j2735::map::assumptions`] is wrong. Flip the
//!    constant, re-run, and fix [`v2xw_msg::j2735::map::MINIMAL_MAP_SIZE_B`] and its test.
//! 2. `MovementPhaseState` is extensible after all, so `eventState` costs 5 bits and not
//!    4 — the disagreement [`v2xw_msg::j2735::spat::MOVEMENT_PHASE_STATE_WIDTH_IS_DISPUTED`]
//!    records. The fix is one `write_bit(false)` before the index.
//! 3. A constraint bound is wrong (`LaneWidth`, `TimeMark`, `ApproachID`). The error names
//!    the field.
//!
//! Whatever the cause, the card and [`v2xw_msg::evidence`] move to `oracle-validated` only
//! once every vector passes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::{Map, Value, json};
use v2xw_core::rng::RngStream;

use v2xw_msg::j2735::map::{
    self, AllowedManeuvers, Connection, ConnectingLane, GenericLane, IntersectionGeometry,
    LaneAttributes, LaneDirection, LaneSharing, MapData, NodeOffset, NodeXy, Position3D,
    VehicleLaneAttributes, XyAlternative, XyOffset,
};
use v2xw_msg::j2735::spat::{
    self, IntersectionReferenceId, IntersectionState, IntersectionStatus, MovementEvent,
    MovementPhaseState, MovementState, Spat, TimeChangeDetails,
};

/// How many pseudo-random vectors of each message to generate. Boundary vectors are extra.
const RANDOM_VECTORS: usize = 100;

/// Fixed seed, so a failure is reproducible and a pass means the same messages passed.
const SEED: [u8; 32] = *b"v2xw-j2735-infra-oracle-seed-26\0";

// =========================================================================================
// Canonical JSON: the form pycrate's values take
// =========================================================================================

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex"))
        .collect()
}

/// A `BIT STRING` as `pycrate` reports it: `(value, length)`, value right-aligned.
fn bits(value: u64, len: u32) -> Value {
    json!({ "__bits__": [value, len] })
}

/// A `CHOICE` as `pycrate` reports it: `(alternative name, value)`. The same tuple shape as
/// an open type, and the oracle script's `to_py` turns both into a Python tuple.
fn choice(name: &str, value: Value) -> Value {
    json!({ "__open__": [name, value] })
}

fn reference_id_json(id: &IntersectionReferenceId) -> Value {
    let mut map = Map::new();
    if let Some(region) = id.region {
        map.insert("region".into(), json!(region));
    }
    map.insert("id".into(), json!(id.id));
    Value::Object(map)
}

fn timing_json(t: &TimeChangeDetails) -> Value {
    let mut map = Map::new();
    if let Some(v) = t.start_time {
        map.insert("startTime".into(), json!(v));
    }
    map.insert("minEndTime".into(), json!(t.min_end_time));
    if let Some(v) = t.max_end_time {
        map.insert("maxEndTime".into(), json!(v));
    }
    if let Some(v) = t.likely_time {
        map.insert("likelyTime".into(), json!(v));
    }
    if let Some(v) = t.confidence {
        map.insert("confidence".into(), json!(v));
    }
    if let Some(v) = t.next_time {
        map.insert("nextTime".into(), json!(v));
    }
    Value::Object(map)
}

fn movement_event_json(e: &MovementEvent) -> Value {
    let mut map = Map::new();
    map.insert("eventState".into(), json!(e.event_state.as_str()));
    if let Some(timing) = &e.timing {
        map.insert("timing".into(), timing_json(timing));
    }
    Value::Object(map)
}

fn movement_state_json(s: &MovementState) -> Value {
    json!({
        "signalGroup": s.signal_group,
        "state-time-speed": s.events.iter().map(movement_event_json).collect::<Vec<_>>(),
    })
}

fn intersection_state_json(s: &IntersectionState) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), reference_id_json(&s.id));
    map.insert("revision".into(), json!(s.revision));
    map.insert(
        "status".into(),
        bits(u64::from(s.status.0), spat::INTERSECTION_STATUS_BITS),
    );
    if let Some(moy) = s.moy {
        map.insert("moy".into(), json!(moy));
    }
    if let Some(ts) = s.time_stamp {
        map.insert("timeStamp".into(), json!(ts));
    }
    map.insert(
        "states".into(),
        Value::Array(s.states.iter().map(movement_state_json).collect()),
    );
    Value::Object(map)
}

fn spat_json(spat: &Spat) -> Value {
    let mut map = Map::new();
    if let Some(ts) = spat.time_stamp {
        map.insert("timeStamp".into(), json!(ts));
    }
    map.insert(
        "intersections".into(),
        Value::Array(
            spat
                .intersections
                .iter()
                .map(intersection_state_json)
                .collect(),
        ),
    );
    Value::Object(map)
}

fn node_offset_json(offset: &NodeOffset) -> Value {
    match offset {
        NodeOffset::Xy(xy) => choice(
            xy.alternative.as_str(),
            json!({ "x": xy.x_cm, "y": xy.y_cm }),
        ),
        // Node-LLmD-64b declares lon before lat; the JSON is a mapping, so the order here
        // is documentation rather than encoding — the encoder is what fixes the order.
        NodeOffset::LatLon { lon, lat } => {
            choice("node-LatLon", json!({ "lon": lon, "lat": lat }))
        }
    }
}

fn lane_attributes_json(a: &LaneAttributes) -> Value {
    json!({
        "directionalUse": bits(u64::from(a.directional_use.0), map::LANE_DIRECTION_BITS),
        "sharedWith": bits(u64::from(a.shared_with.0), map::LANE_SHARING_BITS),
        "laneType": choice(
            "vehicle",
            bits(u64::from(a.vehicle.0), map::LANE_ATTRIBUTES_VEHICLE_BITS),
        ),
    })
}

fn connection_json(c: &Connection) -> Value {
    let mut lane = Map::new();
    lane.insert("lane".into(), json!(c.connecting_lane.lane));
    if let Some(maneuver) = c.connecting_lane.maneuver {
        lane.insert(
            "maneuver".into(),
            bits(u64::from(maneuver.0), map::ALLOWED_MANEUVERS_BITS),
        );
    }
    let mut out = Map::new();
    out.insert("connectingLane".into(), Value::Object(lane));
    if let Some(remote) = &c.remote_intersection {
        out.insert("remoteIntersection".into(), reference_id_json(remote));
    }
    if let Some(group) = c.signal_group {
        out.insert("signalGroup".into(), json!(group));
    }
    if let Some(class) = c.user_class {
        out.insert("userClass".into(), json!(class));
    }
    if let Some(id) = c.connection_id {
        out.insert("connectionID".into(), json!(id));
    }
    Value::Object(out)
}

fn lane_json(lane: &GenericLane) -> Value {
    let mut out = Map::new();
    out.insert("laneID".into(), json!(lane.lane_id));
    if let Some(approach) = lane.ingress_approach {
        out.insert("ingressApproach".into(), json!(approach));
    }
    if let Some(approach) = lane.egress_approach {
        out.insert("egressApproach".into(), json!(approach));
    }
    out.insert(
        "laneAttributes".into(),
        lane_attributes_json(&lane.attributes),
    );
    if let Some(maneuvers) = lane.maneuvers {
        out.insert(
            "maneuvers".into(),
            bits(u64::from(maneuvers.0), map::ALLOWED_MANEUVERS_BITS),
        );
    }
    out.insert(
        "nodeList".into(),
        choice(
            "nodes",
            Value::Array(
                lane.nodes
                    .iter()
                    .map(|n| json!({ "delta": node_offset_json(&n.delta) }))
                    .collect(),
            ),
        ),
    );
    if !lane.connects_to.is_empty() {
        out.insert(
            "connectsTo".into(),
            Value::Array(lane.connects_to.iter().map(connection_json).collect()),
        );
    }
    Value::Object(out)
}

fn geometry_json(g: &IntersectionGeometry) -> Value {
    let mut ref_point = Map::new();
    ref_point.insert("lat".into(), json!(g.ref_point.lat));
    ref_point.insert("long".into(), json!(g.ref_point.lon));
    if let Some(elevation) = g.ref_point.elevation {
        ref_point.insert("elevation".into(), json!(elevation));
    }

    let mut map = Map::new();
    map.insert("id".into(), reference_id_json(&g.id));
    map.insert("revision".into(), json!(g.revision));
    map.insert("refPoint".into(), Value::Object(ref_point));
    if let Some(width) = g.lane_width_cm {
        map.insert("laneWidth".into(), json!(width));
    }
    map.insert(
        "laneSet".into(),
        Value::Array(g.lanes.iter().map(lane_json).collect()),
    );
    Value::Object(map)
}

fn map_json(data: &MapData) -> Value {
    let mut map = Map::new();
    if let Some(ts) = data.time_stamp {
        map.insert("timeStamp".into(), json!(ts));
    }
    map.insert(
        "msgIssueRevision".into(),
        json!(data.msg_issue_revision),
    );
    map.insert(
        "intersections".into(),
        Value::Array(data.intersections.iter().map(geometry_json).collect()),
    );
    Value::Object(map)
}

// =========================================================================================
// Vector generation
// =========================================================================================

/// A mid-range SPaT: nothing at a boundary, so a vector that changes one field tests that
/// field alone.
fn nominal_spat() -> Spat {
    Spat {
        time_stamp: Some(123_456),
        intersections: vec![IntersectionState {
            id: IntersectionReferenceId::in_region(7, 1_234),
            revision: 3,
            status: IntersectionStatus::FIXED_TIME_OPERATION,
            moy: Some(123_456),
            time_stamp: Some(43_210),
            states: vec![
                MovementState::current(
                    1,
                    MovementEvent::timed(
                        MovementPhaseState::ProtectedMovementAllowed,
                        TimeChangeDetails::fixed(120, 275),
                    ),
                ),
                MovementState::current(
                    2,
                    MovementEvent::phase(MovementPhaseState::StopAndRemain),
                ),
            ],
        }],
    }
}

fn nominal_map() -> MapData {
    MapData {
        time_stamp: Some(123_456),
        msg_issue_revision: 3,
        intersections: vec![IntersectionGeometry {
            id: IntersectionReferenceId::in_region(7, 1_234),
            revision: 3,
            ref_point: Position3D {
                lat: 407_440_000,
                lon: -739_900_000,
                elevation: Some(125),
            },
            lane_width_cm: Some(350),
            lanes: vec![GenericLane {
                lane_id: 1,
                ingress_approach: Some(2),
                egress_approach: None,
                attributes: LaneAttributes {
                    directional_use: LaneDirection::INGRESS,
                    shared_with: LaneSharing::BUS,
                    vehicle: VehicleLaneAttributes::NONE,
                },
                maneuvers: Some(AllowedManeuvers::STRAIGHT.with(AllowedManeuvers::RIGHT)),
                nodes: vec![
                    NodeXy::offset(0, 0).expect("fits"),
                    NodeXy::offset(120, 3_400).expect("fits"),
                    NodeXy::offset(-90, 4_000).expect("fits"),
                ],
                connects_to: vec![Connection::signalised(5, 1)],
            }],
        }],
    }
}

/// One SPaT vector per extreme of every field this codec writes.
///
/// These are the values an encoder is most likely to get wrong — an off-by-one bound shows
/// up at the minimum and nowhere else — so they are enumerated rather than left to the
/// random draw to stumble on.
fn spat_boundary_vectors() -> Vec<(String, Spat)> {
    let mut out = Vec::new();
    let mut add = |name: &str, f: &dyn Fn(&mut Spat)| {
        let mut spat = nominal_spat();
        f(&mut spat);
        out.push((format!("spat/{name}"), spat));
    };

    add("minimal", &|s| {
        s.time_stamp = None;
        s.intersections[0].id = IntersectionReferenceId::new(0);
        s.intersections[0].revision = 0;
        s.intersections[0].status = IntersectionStatus::NONE;
        s.intersections[0].moy = None;
        s.intersections[0].time_stamp = None;
        s.intersections[0].states = vec![MovementState::current(
            0,
            MovementEvent::phase(MovementPhaseState::Unavailable),
        )];
    });
    add("timestamp-min", &|s| s.time_stamp = Some(0));
    add("timestamp-unknown", &|s| {
        s.time_stamp = Some(spat::MINUTE_OF_THE_YEAR_UNKNOWN)
    });
    add("moy-max", &|s| {
        s.intersections[0].moy = Some(spat::MINUTE_OF_THE_YEAR_MAX as u32)
    });
    add("dsecond-max", &|s| {
        s.intersections[0].time_stamp = Some(65_535)
    });
    add("region-max", &|s| {
        s.intersections[0].id = IntersectionReferenceId::in_region(
            spat::ROAD_REGULATOR_ID_MAX as u16,
            spat::INTERSECTION_ID_MAX as u16,
        )
    });
    add("no-region", &|s| {
        s.intersections[0].id = IntersectionReferenceId::new(65_535)
    });
    add("revision-max", &|s| s.intersections[0].revision = 127);
    add("status-all-named-bits", &|s| {
        // Bits 0..13 are named; 14 and 15 are unnamed and stay clear.
        s.intersections[0].status = IntersectionStatus(0xfffc)
    });
    add("signal-group-max", &|s| {
        s.intersections[0].states[0].signal_group = 255
    });
    add("all-phase-states", &|s| {
        s.intersections[0].states = (0..spat::MOVEMENT_PHASE_STATE_COUNT)
            .map(|i| {
                MovementState::current(
                    (i + 1) as u8,
                    MovementEvent::phase(
                        MovementPhaseState::from_index(i).expect("in the root list"),
                    ),
                )
            })
            .collect()
    });
    add("timing-every-field", &|s| {
        s.intersections[0].states[0].events[0].timing = Some(TimeChangeDetails {
            start_time: Some(0),
            min_end_time: 1,
            max_end_time: Some(spat::TIME_MARK_TENTHS_PER_HOUR),
            likely_time: Some(100),
            confidence: Some(spat::TIME_INTERVAL_CONFIDENCE_MAX as u8),
            next_time: Some(spat::TIME_MARK_UNKNOWN),
        })
    });
    add("timing-min-end-only", &|s| {
        s.intersections[0].states[0].events[0].timing = Some(TimeChangeDetails {
            start_time: None,
            min_end_time: spat::TIME_MARK_MIN as u16,
            max_end_time: None,
            likely_time: None,
            confidence: None,
            next_time: None,
        })
    });
    add("sixteen-events", &|s| {
        s.intersections[0].states[0].events = (0..spat::MAX_MOVEMENT_EVENTS)
            .map(|i| {
                MovementEvent::timed(
                    MovementPhaseState::PermissiveMovementAllowed,
                    TimeChangeDetails::fixed(i as u16, (i as u16) + 10),
                )
            })
            .collect()
    });
    add("thirty-two-intersections", &|s| {
        s.intersections = vec![s.intersections[0].clone(); spat::MAX_INTERSECTION_STATES]
    });
    out
}

fn map_boundary_vectors() -> Vec<(String, MapData)> {
    let mut out = Vec::new();
    let mut add = |name: &str, f: &dyn Fn(&mut MapData)| {
        let mut data = nominal_map();
        f(&mut data);
        out.push((format!("map/{name}"), data));
    };

    add("minimal", &|m| {
        m.time_stamp = None;
        m.msg_issue_revision = 0;
        let g = &mut m.intersections[0];
        g.id = IntersectionReferenceId::new(0);
        g.revision = 0;
        g.ref_point.elevation = None;
        g.lane_width_cm = None;
        g.lanes[0].ingress_approach = None;
        g.lanes[0].egress_approach = None;
        g.lanes[0].maneuvers = None;
        g.lanes[0].attributes = LaneAttributes::vehicle(LaneDirection::NONE);
        g.lanes[0].nodes = vec![
            NodeXy::offset(0, 0).expect("fits"),
            NodeXy::offset(0, 0).expect("fits"),
        ];
        g.lanes[0].connects_to.clear();
    });
    add("position-extremes", &|m| {
        m.intersections[0].ref_point = Position3D {
            lat: v2xw_msg::j2735::bsm::LATITUDE_MIN as i32,
            lon: v2xw_msg::j2735::bsm::LONGITUDE_MIN as i32,
            elevation: Some(v2xw_msg::j2735::bsm::ELEVATION_MIN as i32),
        }
    });
    add("position-maxima", &|m| {
        m.intersections[0].ref_point = Position3D {
            lat: v2xw_msg::j2735::bsm::LATITUDE_MAX as i32,
            lon: v2xw_msg::j2735::bsm::LONGITUDE_MAX as i32,
            elevation: Some(v2xw_msg::j2735::bsm::ELEVATION_MAX as i32),
        }
    });
    add("lane-width-max", &|m| {
        m.intersections[0].lane_width_cm = Some(map::LANE_WIDTH_MAX as u16)
    });
    add("approach-ids-max", &|m| {
        m.intersections[0].lanes[0].ingress_approach = Some(map::APPROACH_ID_MAX as u8);
        m.intersections[0].lanes[0].egress_approach = Some(map::APPROACH_ID_MAX as u8);
    });
    add("all-attribute-bits", &|m| {
        m.intersections[0].lanes[0].attributes = LaneAttributes {
            directional_use: LaneDirection::BOTH,
            shared_with: LaneSharing(0x3ff),
            vehicle: VehicleLaneAttributes(0xff),
        };
        m.intersections[0].lanes[0].maneuvers = Some(AllowedManeuvers(0xfff));
    });
    add("every-node-alternative", &|m| {
        m.intersections[0].lanes[0].nodes = XyAlternative::ALL
            .into_iter()
            .map(|alternative| NodeXy {
                delta: NodeOffset::Xy(XyOffset {
                    x_cm: alternative.min_cm() as i32,
                    y_cm: alternative.max_cm() as i32,
                    alternative,
                }),
            })
            .collect()
    });
    add("node-latlon", &|m| {
        m.intersections[0].lanes[0].nodes = vec![
            NodeXy::offset(0, 0).expect("fits"),
            NodeXy {
                delta: NodeOffset::LatLon {
                    lon: -739_890_000,
                    lat: 407_441_000,
                },
            },
        ]
    });
    add("sixty-three-nodes", &|m| {
        m.intersections[0].lanes[0].nodes =
            vec![NodeXy::offset(1, 1).expect("fits"); map::MAX_NODES]
    });
    add("sixteen-connections", &|m| {
        m.intersections[0].lanes[0].connects_to = (0..map::MAX_CONNECTIONS)
            .map(|i| Connection {
                connecting_lane: ConnectingLane {
                    lane: (i + 1) as u8,
                    maneuver: Some(AllowedManeuvers::LEFT),
                },
                remote_intersection: Some(IntersectionReferenceId::in_region(1, 2)),
                signal_group: Some((i + 1) as u8),
                user_class: Some(255),
                connection_id: Some(255),
            })
            .collect()
    });
    add("connection-minimal", &|m| {
        m.intersections[0].lanes[0].connects_to = vec![Connection {
            connecting_lane: ConnectingLane {
                lane: 0,
                maneuver: None,
            },
            remote_intersection: None,
            signal_group: None,
            user_class: None,
            connection_id: None,
        }]
    });
    add("many-lanes", &|m| {
        let lane = m.intersections[0].lanes[0].clone();
        m.intersections[0].lanes = (0..16)
            .map(|i| GenericLane {
                lane_id: (i + 1) as u8,
                ..lane.clone()
            })
            .collect()
    });
    out
}

/// Boundary-biased draw: a quarter at each end of the range, half uniform inside it.
fn pick(rng: &mut RngStream, min: i64, max: i64) -> i64 {
    match rng.below(4) {
        0 => min,
        1 => max,
        _ => min + rng.below((max - min + 1) as u64) as i64,
    }
}

/// Half the time, a value drawn by `f`; the other half, `None`.
///
/// Takes a closure rather than a value because the value itself usually needs the same
/// `rng`, and `maybe(rng, pick(rng, …))` would borrow it twice in one call expression.
fn maybe<T>(rng: &mut RngStream, f: impl FnOnce(&mut RngStream) -> T) -> Option<T> {
    if rng.below(2) == 1 { Some(f(rng)) } else { None }
}

/// A `TimeMark`, boundary-biased.
fn time_mark(rng: &mut RngStream) -> u16 {
    pick(rng, spat::TIME_MARK_MIN, spat::TIME_MARK_MAX) as u16
}

/// A `MinuteOfTheYear`, boundary-biased.
fn moy(rng: &mut RngStream) -> u32 {
    pick(
        rng,
        spat::MINUTE_OF_THE_YEAR_MIN,
        spat::MINUTE_OF_THE_YEAR_MAX,
    ) as u32
}

fn random_timing(rng: &mut RngStream) -> TimeChangeDetails {
    TimeChangeDetails {
        start_time: maybe(rng, time_mark),
        min_end_time: time_mark(rng),
        max_end_time: maybe(rng, time_mark),
        likely_time: maybe(rng, time_mark),
        confidence: maybe(rng, |r| {
            pick(
                r,
                spat::TIME_INTERVAL_CONFIDENCE_MIN,
                spat::TIME_INTERVAL_CONFIDENCE_MAX,
            ) as u8
        }),
        next_time: maybe(rng, time_mark),
    }
}

fn random_spat(rng: &mut RngStream) -> Spat {
    let state_count = 1 + rng.below(4);
    let mut states = Vec::with_capacity(state_count as usize);
    for _ in 0..state_count {
        let event_count = 1 + rng.below(3);
        let mut events = Vec::with_capacity(event_count as usize);
        for _ in 0..event_count {
            let index = rng.below(spat::MOVEMENT_PHASE_STATE_COUNT);
            let event_state =
                MovementPhaseState::from_index(index).expect("drawn from the root list");
            let timing = maybe(rng, random_timing);
            events.push(MovementEvent {
                event_state,
                timing,
            });
        }
        let signal_group =
            pick(rng, spat::SIGNAL_GROUP_ID_MIN, spat::SIGNAL_GROUP_ID_MAX) as u8;
        states.push(MovementState {
            signal_group,
            events,
        });
    }

    let time_stamp = maybe(rng, moy);
    let region = maybe(rng, |r| {
        pick(
            r,
            spat::ROAD_REGULATOR_ID_MIN,
            spat::ROAD_REGULATOR_ID_MAX,
        ) as u16
    });
    let id = pick(rng, spat::INTERSECTION_ID_MIN, spat::INTERSECTION_ID_MAX) as u16;
    let revision = pick(rng, 0, 127) as u8;
    // Bits 14 and 15 of IntersectionStatusObject are unnamed and stay clear, so a vector
    // is never testing a bit the standard does not define.
    let status = IntersectionStatus((rng.u32() & 0xfffc) as u16);
    let moy_value = maybe(rng, moy);
    let d_second = maybe(rng, |r| pick(r, 0, 65_535) as u16);

    Spat {
        time_stamp,
        intersections: vec![IntersectionState {
            id: IntersectionReferenceId { region, id },
            revision,
            status,
            moy: moy_value,
            time_stamp: d_second,
            states,
        }],
    }
}

fn random_node(rng: &mut RngStream) -> NodeXy {
    let alternative = XyAlternative::from_index(rng.below(6)).expect("six alternatives");
    let x_cm = pick(rng, alternative.min_cm(), alternative.max_cm()) as i32;
    let y_cm = pick(rng, alternative.min_cm(), alternative.max_cm()) as i32;
    NodeXy {
        delta: NodeOffset::Xy(XyOffset {
            x_cm,
            y_cm,
            alternative,
        }),
    }
}

fn random_connection(rng: &mut RngStream, index: u64) -> Connection {
    let lane = pick(rng, map::LANE_ID_MIN, map::LANE_ID_MAX) as u8;
    let maneuver = maybe(rng, |r| AllowedManeuvers((r.u32() & 0xfff) as u16));
    let remote_intersection = maybe(rng, |_| IntersectionReferenceId::new(index as u16));
    let signal_group = maybe(rng, |r| {
        pick(r, spat::SIGNAL_GROUP_ID_MIN, spat::SIGNAL_GROUP_ID_MAX) as u8
    });
    let user_class = maybe(rng, |r| {
        pick(
            r,
            map::RESTRICTION_CLASS_ID_MIN,
            map::RESTRICTION_CLASS_ID_MAX,
        ) as u8
    });
    let connection_id = maybe(rng, |r| {
        pick(r, map::LANE_CONNECTION_ID_MIN, map::LANE_CONNECTION_ID_MAX) as u8
    });
    Connection {
        connecting_lane: ConnectingLane { lane, maneuver },
        remote_intersection,
        signal_group,
        user_class,
        connection_id,
    }
}

fn random_lane(rng: &mut RngStream, lane_id: u8) -> GenericLane {
    let node_count = map::MIN_NODES as u64 + rng.below(4);
    let mut nodes = Vec::with_capacity(node_count as usize);
    for _ in 0..node_count {
        nodes.push(random_node(rng));
    }
    let connection_count = rng.below(3);
    let mut connects_to = Vec::with_capacity(connection_count as usize);
    for j in 0..connection_count {
        connects_to.push(random_connection(rng, j));
    }
    let ingress_approach = maybe(rng, |r| {
        pick(r, map::APPROACH_ID_MIN, map::APPROACH_ID_MAX) as u8
    });
    let egress_approach = maybe(rng, |r| {
        pick(r, map::APPROACH_ID_MIN, map::APPROACH_ID_MAX) as u8
    });
    let attributes = LaneAttributes {
        directional_use: LaneDirection((rng.u32() & 0b11) as u8),
        shared_with: LaneSharing((rng.u32() & 0x3ff) as u16),
        vehicle: VehicleLaneAttributes((rng.u32() & 0xff) as u8),
    };
    let maneuvers = maybe(rng, |r| AllowedManeuvers((r.u32() & 0xfff) as u16));
    GenericLane {
        lane_id,
        ingress_approach,
        egress_approach,
        attributes,
        maneuvers,
        nodes,
        connects_to,
    }
}

fn random_map(rng: &mut RngStream) -> MapData {
    let lane_count = 1 + rng.below(4);
    let mut lanes = Vec::with_capacity(lane_count as usize);
    for i in 0..lane_count {
        lanes.push(random_lane(rng, (i + 1) as u8));
    }

    let time_stamp = maybe(rng, moy);
    let msg_issue_revision = pick(rng, 0, 127) as u8;
    let id = pick(rng, spat::INTERSECTION_ID_MIN, spat::INTERSECTION_ID_MAX) as u16;
    let revision = pick(rng, 0, 127) as u8;
    let lat = pick(
        rng,
        v2xw_msg::j2735::bsm::LATITUDE_MIN,
        v2xw_msg::j2735::bsm::LATITUDE_MAX,
    ) as i32;
    let lon = pick(
        rng,
        v2xw_msg::j2735::bsm::LONGITUDE_MIN,
        v2xw_msg::j2735::bsm::LONGITUDE_MAX,
    ) as i32;
    let elevation = maybe(rng, |r| {
        pick(
            r,
            v2xw_msg::j2735::bsm::ELEVATION_MIN,
            v2xw_msg::j2735::bsm::ELEVATION_MAX,
        ) as i32
    });
    let lane_width_cm = maybe(rng, |r| {
        pick(r, map::LANE_WIDTH_MIN, map::LANE_WIDTH_MAX) as u16
    });

    MapData {
        time_stamp,
        msg_issue_revision,
        intersections: vec![IntersectionGeometry {
            id: IntersectionReferenceId::new(id),
            revision,
            ref_point: Position3D {
                lat,
                lon,
                elevation,
            },
            lane_width_cm,
            lanes,
        }],
    }
}

// =========================================================================================
// Running the oracle
// =========================================================================================

struct OracleRun {
    spats: Vec<(String, Spat)>,
    maps: Vec<(String, MapData)>,
    report: Value,
    stdout: String,
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn oracle_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("V2XW_J2735_ORACLE_DIR")?);
    dir.join("j2735_all.py").is_file().then_some(dir)
}

fn python(dir: &Path) -> PathBuf {
    std::env::var_os("V2XW_J2735_ORACLE_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join(".venv/bin/python"))
}

const SKIP_MESSAGE: &str = "\
SKIPPED: the pycrate oracle needs a Python environment this machine does not have, and
  the SAE J2735 modules, which are git-ignored (build decision D3) and are not in this
  checkout at all — third_party/asn1/j2735/ does not exist.
  Set V2XW_J2735_ORACLE_DIR to a directory holding j2735_all.py and .venv/bin/python; the
  module documentation in tests/j2735_infra_oracle.rs has the three commands that build it.
  UNTIL THIS RUNS, the SPaT and MAP bytes are NOT validated: v2xw_msg::evidence calls them
  `real UPER, not yet oracle-validated`, the codec's card says so, and no result may
  describe them as byte-exact. The crate's own round-trip tests cannot close that gap,
  because the encoder and the decoder share every assumption.";

/// Runs the oracle once for the whole test binary; every test reads the same result.
fn oracle() -> Option<&'static OracleRun> {
    static RUN: OnceLock<Option<OracleRun>> = OnceLock::new();
    RUN.get_or_init(|| {
        let dir = oracle_dir()?;
        let python = python(&dir);
        if !python.is_file() {
            eprintln!("{SKIP_MESSAGE}\n  (no interpreter at {})", python.display());
            return None;
        }

        let mut rng = RngStream::from_key(SEED);
        let mut spats = spat_boundary_vectors();
        let mut maps = map_boundary_vectors();
        for i in 0..RANDOM_VECTORS {
            spats.push((format!("spat/random/{i:03}"), random_spat(&mut rng)));
            maps.push((format!("map/random/{i:03}"), random_map(&mut rng)));
        }

        let spat_payload: Vec<Value> = spats
            .iter()
            .map(|(name, message)| {
                json!({
                    "name": name,
                    "kind": "spat",
                    "value": spat_json(message),
                    "rust_hex": hex(&spat::encode_spat(message).expect("encodes").bytes),
                    "rust_frame_hex": hex(
                        &spat::encode_message_frame(message).expect("frames").bytes
                    ),
                })
            })
            .collect();
        let map_payload: Vec<Value> = maps
            .iter()
            .map(|(name, message)| {
                json!({
                    "name": name,
                    "kind": "map",
                    "value": map_json(message),
                    "rust_hex": hex(&map::encode_map(message).expect("encodes").bytes),
                    "rust_frame_hex": hex(
                        &map::encode_message_frame(message).expect("frames").bytes
                    ),
                })
            })
            .collect();

        let mut payload = spat_payload;
        payload.extend(map_payload);

        let work = std::env::temp_dir().join("v2xw-j2735-infra-oracle");
        std::fs::create_dir_all(&work).expect("scratch directory");
        let vectors_path = work.join("vectors.json");
        let results_path = work.join("results.json");
        std::fs::write(
            &vectors_path,
            serde_json::to_vec(&Value::Array(payload)).expect("serialises"),
        )
        .expect("writes vectors");

        let output = Command::new(&python)
            .arg(manifest_dir().join("tests/oracle/infra_oracle.py"))
            .arg(&vectors_path)
            .arg(&results_path)
            .env("V2XW_J2735_ORACLE_DIR", &dir)
            .output()
            .expect("runs the oracle");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "the oracle failed: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let report: Value =
            serde_json::from_slice(&std::fs::read(&results_path).expect("reads results"))
                .expect("parses results");
        Some(OracleRun {
            spats,
            maps,
            report,
            stdout,
        })
    })
    .as_ref()
}

/// A short account of a mismatch: the first differing key, not a wall of JSON.
fn first_difference(expected: &Value, actual: &Value, path: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in a {
                match b.get(k) {
                    None => return Some(format!("{path}.{k}: missing from pycrate's value")),
                    Some(other) => {
                        if let Some(d) = first_difference(v, other, &format!("{path}.{k}")) {
                            return Some(d);
                        }
                    }
                }
            }
            for k in b.keys() {
                if !a.contains_key(k) {
                    return Some(format!("{path}.{k}: pycrate has it, we do not"));
                }
            }
            None
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{path}: {} items vs {}", a.len(), b.len()));
            }
            a.iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (x, y))| first_difference(x, y, &format!("{path}[{i}]")))
        }
        _ if expected == actual => None,
        _ => Some(format!("{path}: {expected} vs {actual}")),
    }
}

/// The results for one kind, keyed by name, so a test can look its own vectors up.
fn results_for(run: &OracleRun, kind: &str) -> Vec<Value> {
    run.report["pycrate_results"]
        .as_array()
        .expect("a result per vector")
        .iter()
        .filter(|r| r["kind"] == json!(kind))
        .cloned()
        .collect()
}

#[test]
fn rust_spat_encodings_match_pycrate_byte_for_byte() {
    let Some(run) = oracle() else {
        eprintln!("{SKIP_MESSAGE}");
        return;
    };
    let results = results_for(run, "spat");
    assert_eq!(results.len(), run.spats.len());

    let mut failures = Vec::new();
    for (result, (name, message)) in results.iter().zip(&run.spats) {
        assert_eq!(result["name"].as_str(), Some(name.as_str()));
        if let Some(error) = result["error"].as_str() {
            failures.push(format!("{name}: pycrate raised {error}"));
            continue;
        }
        if result["encode_match"] != json!(true) {
            failures.push(format!(
                "{name}: encoding differs\n    rust:    {}\n    pycrate: {}",
                hex(&spat::encode_spat(message).expect("encodes").bytes),
                result["py_hex"].as_str().unwrap_or("?")
            ));
        }
        if result["frame_match"] != json!(true) {
            failures.push(format!(
                "{name}: MessageFrame encoding differs\n    rust:    {}\n    pycrate: {}",
                hex(&spat::encode_message_frame(message).expect("frames").bytes),
                result["py_frame_hex"].as_str().unwrap_or("?")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} SPaT vectors encoded differently from pycrate. The likely causes are \
         listed in this file's module documentation; the first is the MovementPhaseState \
         width dispute.\n{}",
        failures.len(),
        run.spats.len(),
        failures.join("\n")
    );
    println!(
        "pycrate oracle: {} SPaT vectors, byte-identical encodings\n{}",
        run.spats.len(),
        run.stdout.trim()
    );
}

#[test]
fn rust_map_encodings_match_pycrate_byte_for_byte() {
    let Some(run) = oracle() else {
        eprintln!("{SKIP_MESSAGE}");
        return;
    };
    let results = results_for(run, "map");
    assert_eq!(results.len(), run.maps.len());

    let mut failures = Vec::new();
    for (result, (name, message)) in results.iter().zip(&run.maps) {
        assert_eq!(result["name"].as_str(), Some(name.as_str()));
        if let Some(error) = result["error"].as_str() {
            failures.push(format!("{name}: pycrate raised {error}"));
            continue;
        }
        if result["encode_match"] != json!(true) {
            failures.push(format!(
                "{name}: encoding differs\n    rust:    {}\n    pycrate: {}",
                hex(&map::encode_map(message).expect("encodes").bytes),
                result["py_hex"].as_str().unwrap_or("?")
            ));
        }
        if result["frame_match"] != json!(true) {
            failures.push(format!(
                "{name}: MessageFrame encoding differs\n    rust:    {}\n    pycrate: {}",
                hex(&map::encode_message_frame(message).expect("frames").bytes),
                result["py_frame_hex"].as_str().unwrap_or("?")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} MAP vectors encoded differently from pycrate. Every structural \
         assumption is a named constant in v2xw_msg::j2735::map::assumptions; flip the one \
         the diff points at rather than adjusting the vector.\n{}",
        failures.len(),
        run.maps.len(),
        failures.join("\n")
    );
    println!(
        "pycrate oracle: {} MAP vectors, byte-identical encodings",
        run.maps.len()
    );
}

#[test]
fn pycrate_encodings_decode_field_for_field() {
    let Some(run) = oracle() else {
        eprintln!("{SKIP_MESSAGE}");
        return;
    };

    let mut failures = Vec::new();
    for (result, (name, message)) in results_for(run, "spat").iter().zip(&run.spats) {
        if result.get("error").is_some() {
            continue; // already reported by the encoding test
        }
        if result["decode_match"] != json!(true) {
            let difference = first_difference(&spat_json(message), &result["py_decoded"], "spat")
                .unwrap_or_else(|| "values compare equal but the test says otherwise".to_string());
            failures.push(format!(
                "{name}: pycrate decoded our bytes differently: {difference}"
            ));
        }
        let py_bytes = unhex(result["py_hex"].as_str().expect("pycrate's octets"));
        match spat::decode_spat(&py_bytes) {
            Ok(decoded) if decoded == *message => {}
            Ok(decoded) => failures.push(format!(
                "{name}: we decoded pycrate's bytes into a different message\n    \
                 expected: {message:?}\n    got:      {decoded:?}"
            )),
            Err(e) => failures.push(format!("{name}: we could not decode pycrate's bytes: {e}")),
        }
    }
    for (result, (name, message)) in results_for(run, "map").iter().zip(&run.maps) {
        if result.get("error").is_some() {
            continue;
        }
        if result["decode_match"] != json!(true) {
            let difference = first_difference(&map_json(message), &result["py_decoded"], "map")
                .unwrap_or_else(|| "values compare equal but the test says otherwise".to_string());
            failures.push(format!(
                "{name}: pycrate decoded our bytes differently: {difference}"
            ));
        }
        let py_bytes = unhex(result["py_hex"].as_str().expect("pycrate's octets"));
        match map::decode_map(&py_bytes) {
            Ok(decoded) if decoded == *message => {}
            Ok(decoded) => failures.push(format!(
                "{name}: we decoded pycrate's bytes into a different message\n    \
                 expected: {message:?}\n    got:      {decoded:?}"
            )),
            Err(e) => failures.push(format!("{name}: we could not decode pycrate's bytes: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} vectors disagreed in one of the two decode directions:\n{}",
        failures.len(),
        failures.join("\n")
    );
    println!("pycrate oracle: SPaT and MAP field-identical in both directions");
}

/// The elements the codecs refuse must be refused, not silently skipped.
///
/// Nothing in a Rust-only test can check this: the Rust side cannot construct a
/// `DescriptiveName`, a `ComputedLane`, a `NodeAttributeSetXY` or a regional extension, so
/// the bytes have to come from an implementation that can. A decoder that stepped over one
/// of these would return a plausible message built from misaligned bits, which is the one
/// failure mode this whole crate is arranged to prevent.
#[test]
fn refused_elements_are_refused_rather_than_misread() {
    let Some(run) = oracle() else {
        eprintln!("{SKIP_MESSAGE}");
        return;
    };
    if let Some(error) = run.report.get("python_origin_error") {
        panic!("the oracle could not build its own vectors: {error}");
    }
    let vectors = run.report["python_origin"]
        .as_array()
        .expect("python-origin vectors");
    assert!(
        !vectors.is_empty(),
        "the oracle built no refusal vectors, so nothing was checked"
    );

    for vector in vectors {
        let name = vector["name"].as_str().expect("a name");
        let kind = vector["kind"].as_str().expect("a kind");
        let bytes = unhex(vector["hex"].as_str().expect("octets"));
        let construct = vector["construct"].as_str().expect("what it carries");
        let error = match kind {
            "spat" => spat::decode_spat(&bytes).err(),
            "map" => map::decode_map(&bytes).err(),
            other => panic!("{name}: unknown vector kind {other}"),
        };
        let error = error.unwrap_or_else(|| {
            panic!(
                "{name}: a message carrying {construct} decoded successfully. Either the \
                 codec now models it — in which case say so in the card and in \
                 v2xw_msg::evidence — or it stepped over it and returned a message \
                 assembled from the wrong bits."
            )
        });
        let text = error.to_string();
        assert!(
            text.contains(construct)
                || matches!(error, v2xw_msg::CodecError::UnsupportedConstruct { .. }),
            "{name}: the refusal should name {construct}, not say {text}"
        );
    }
    println!(
        "pycrate oracle: {} unmodelled-element vectors were refused by name",
        vectors.len()
    );
}
