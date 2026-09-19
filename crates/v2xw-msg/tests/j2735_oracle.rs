//! Cross-validation of the hand-written J2735 BSM codec against `pycrate`.
//!
//! # Why a hand-written codec needs an oracle
//!
//! A codec that is only ever tested against itself proves consistency, not correctness. If
//! this crate misread `Longitude ::= INTEGER (-1799999999..1800000001)` as symmetric about
//! zero, every encode/decode round trip in the crate's own suite would still pass, and
//! every byte on the wire would be wrong. Only an independent implementation of the same
//! ASN.1 can catch that, and build decision D2 says the codec does not ship without one.
//!
//! `pycrate` is that implementation: it compiles the real SAE J2735 2024-09 modules at run
//! time and encodes from the compiled grammar, so it embodies the standard rather than
//! anyone's reading of it.
//!
//! # Three directions, because each catches a different defect
//!
//! | Direction | Catches |
//! |---|---|
//! | Rust encodes → bytes compared to `pycrate`'s | a misread constraint, a wrong field width, a missing extension bit |
//! | Rust encodes → `pycrate` decodes → fields compared | a field written in the wrong place that happens to be the right width |
//! | `pycrate` encodes → Rust decodes → fields compared | a decoder that is wrong in the same way as the encoder |
//!
//! A fourth check covers the Part II containers this codec deliberately does not model:
//! `pycrate` builds messages carrying them, and the Rust codec must reproduce those bytes
//! exactly after a decode and re-encode.
//!
//! # Running it
//!
//! The J2735 modules are not in this repository (build decision D3), so the oracle needs an
//! environment prepared outside it:
//!
//! ```sh
//! uv venv --python 3.12 /tmp/j2735-oracle/.venv
//! VIRTUAL_ENV=/tmp/j2735-oracle/.venv uv pip install pycrate
//! V2XW_J2735_ASN1_DIR=<the J2735 .asn directory> V2XW_J2735_ORACLE_DIR=/tmp/j2735-oracle \
//!   /tmp/j2735-oracle/.venv/bin/python crates/v2xw-msg/tests/oracle/compile_j2735.py
//! V2XW_J2735_ORACLE_DIR=/tmp/j2735-oracle cargo test -p v2xw-msg --test j2735_oracle
//! ```
//!
//! Without `V2XW_J2735_ORACLE_DIR` the tests **skip with a message**, which is how they
//! behave in CI. They do not fail, and they do not silently pass: the message says exactly
//! what is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::{Value, json};
use v2xw_core::rng::RngStream;
use v2xw_msg::j2735::bsm::{
    self, AccelerationSet4Way, AuxiliaryBrakeStatus, BasicSafetyMessage, BrakeAppliedStatus,
    BrakeBoostApplied, BrakeSystemStatus, BsmCoreData, ControlStatus, ExteriorLights, GnssStatus,
    PartIIValue, PathHistory, PathHistoryPoint, PathPrediction, PositionalAccuracy,
    TransmissionState, VehicleEventFlags, VehicleSafetyExtensions, VehicleSize,
};

/// How many pseudo-random vectors to generate. The boundary vectors are extra.
const RANDOM_VECTORS: usize = 200;

/// The seed the vectors are drawn from. Fixed, so a failure is reproducible and a passing
/// run means the same 200 messages passed.
const SEED: [u8; 32] = *b"v2xw-j2735-bsm-oracle-seed-2026\0";

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

fn octets(value: &[u8]) -> Value {
    json!({ "__hex__": hex(value) })
}

fn transmission_name(t: TransmissionState) -> &'static str {
    match t {
        TransmissionState::Neutral => "neutral",
        TransmissionState::Park => "park",
        TransmissionState::ForwardGears => "forwardGears",
        TransmissionState::ReverseGears => "reverseGears",
        TransmissionState::Reserved1 => "reserved1",
        TransmissionState::Reserved2 => "reserved2",
        TransmissionState::Reserved3 => "reserved3",
        TransmissionState::Unavailable => "unavailable",
    }
}

fn control_name(s: ControlStatus) -> &'static str {
    match s {
        ControlStatus::Unavailable => "unavailable",
        ControlStatus::Off => "off",
        ControlStatus::On => "on",
        ControlStatus::Engaged => "engaged",
    }
}

fn boost_name(s: BrakeBoostApplied) -> &'static str {
    match s {
        BrakeBoostApplied::Unavailable => "unavailable",
        BrakeBoostApplied::Off => "off",
        BrakeBoostApplied::On => "on",
    }
}

fn aux_name(s: AuxiliaryBrakeStatus) -> &'static str {
    match s {
        AuxiliaryBrakeStatus::Unavailable => "unavailable",
        AuxiliaryBrakeStatus::Off => "off",
        AuxiliaryBrakeStatus::On => "on",
        AuxiliaryBrakeStatus::Reserved => "reserved",
    }
}

fn accuracy_json(a: &PositionalAccuracy) -> Value {
    json!({
        "semiMajor": a.semi_major,
        "semiMinor": a.semi_minor,
        "orientation": a.orientation,
    })
}

fn core_json(c: &BsmCoreData) -> Value {
    json!({
        "msgCnt": c.msg_cnt,
        "id": octets(&c.id),
        "secMark": c.sec_mark,
        "lat": c.lat,
        "long": c.lon,
        "elev": c.elev,
        "accuracy": accuracy_json(&c.accuracy),
        "transmission": transmission_name(c.transmission),
        "speed": c.speed,
        "heading": c.heading,
        "angle": c.angle,
        "accelSet": {
            "long": c.accel_set.long,
            "lat": c.accel_set.lat,
            "vert": c.accel_set.vert,
            "yaw": c.accel_set.yaw,
        },
        "brakes": {
            "wheelBrakes": bits(u64::from(c.brakes.wheel_brakes.0), bsm::BRAKE_APPLIED_STATUS_BITS),
            "traction": control_name(c.brakes.traction),
            "abs": control_name(c.brakes.abs),
            "scs": control_name(c.brakes.scs),
            "brakeBoost": boost_name(c.brakes.brake_boost),
            "auxBrakes": aux_name(c.brakes.aux_brakes),
        },
        "size": { "width": c.size.width, "length": c.size.length },
    })
}

fn point_json(p: &PathHistoryPoint) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("latOffset".into(), json!(p.lat_offset));
    map.insert("lonOffset".into(), json!(p.lon_offset));
    map.insert("elevationOffset".into(), json!(p.elevation_offset));
    map.insert("timeOffset".into(), json!(p.time_offset));
    if let Some(speed) = p.speed {
        map.insert("speed".into(), json!(speed));
    }
    if let Some(accuracy) = p.pos_accuracy {
        map.insert("posAccuracy".into(), accuracy_json(&accuracy));
    }
    if let Some(heading) = p.heading {
        map.insert("heading".into(), json!(heading));
    }
    Value::Object(map)
}

fn vse_json(v: &VehicleSafetyExtensions) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(events) = v.events {
        map.insert(
            "events".into(),
            bits(u64::from(events.0), bsm::VEHICLE_EVENT_FLAGS_BITS),
        );
    }
    if let Some(history) = &v.path_history {
        let mut h = serde_json::Map::new();
        if let Some(status) = history.gnss_status {
            h.insert(
                "currGNSSstatus".into(),
                bits(u64::from(status.0), bsm::GNSS_STATUS_BITS),
            );
        }
        h.insert(
            "crumbData".into(),
            Value::Array(history.crumb_data.iter().map(point_json).collect()),
        );
        map.insert("pathHistory".into(), Value::Object(h));
    }
    if let Some(prediction) = v.path_prediction {
        map.insert(
            "pathPrediction".into(),
            json!({
                "radiusOfCurve": prediction.radius_of_curve,
                "confidence": prediction.confidence,
            }),
        );
    }
    if let Some(lights) = v.lights {
        map.insert(
            "lights".into(),
            bits(u64::from(lights.0), bsm::EXTERIOR_LIGHTS_BITS),
        );
    }
    Value::Object(map)
}

/// The whole message in the canonical form, ready to compare against `pycrate`'s.
fn bsm_json(b: &BasicSafetyMessage) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("coreData".into(), core_json(&b.core));
    if !b.part_ii.is_empty() {
        let items: Vec<Value> = b
            .part_ii
            .iter()
            .map(|content| match &content.value {
                PartIIValue::VehicleSafety(value) => json!({
                    "partII-Id": content.id,
                    "partII-Value": { "__open__": ["VehicleSafetyExtensions", vse_json(value)] },
                }),
                // Opaque containers have no canonical JSON form — pycrate decodes them
                // structurally — so they travel the other way round, as python-origin
                // vectors. `PartIIValue` is `#[non_exhaustive]`, so the wildcard is
                // required from outside the crate and also catches a variant added later.
                _ => panic!("opaque containers are covered by the python-origin vectors"),
            })
            .collect();
        map.insert("partII".into(), Value::Array(items));
    }
    Value::Object(map)
}

// =========================================================================================
// Vector generation
// =========================================================================================

/// A mid-range message: nothing at a boundary, so a vector that changes one field is
/// testing that field alone.
fn nominal() -> BsmCoreData {
    BsmCoreData {
        msg_cnt: 42,
        id: [0x0a, 0x0b, 0x0c, 0x0d],
        sec_mark: 12_345,
        lat: 407_440_000,
        lon: -739_900_000,
        elev: 125,
        accuracy: PositionalAccuracy {
            semi_major: 36,
            semi_minor: 22,
            orientation: 12_345,
        },
        transmission: TransmissionState::ForwardGears,
        speed: 694,
        heading: 3_600,
        angle: 2,
        accel_set: AccelerationSet4Way {
            long: -125,
            lat: 30,
            vert: 50,
            yaw: -1_200,
        },
        brakes: BrakeSystemStatus {
            wheel_brakes: BrakeAppliedStatus::LEFT_FRONT.with(BrakeAppliedStatus::RIGHT_FRONT),
            traction: ControlStatus::On,
            abs: ControlStatus::Engaged,
            scs: ControlStatus::Off,
            brake_boost: BrakeBoostApplied::On,
            aux_brakes: AuxiliaryBrakeStatus::Off,
        },
        size: VehicleSize {
            width: 180,
            length: 450,
        },
    }
}

/// One vector per extreme of every Part I field: the minimum, the maximum and the
/// `unavailable` sentinel wherever one exists.
///
/// These are the values an encoder is most likely to get wrong — an off-by-one lower bound
/// shows up at the minimum and nowhere else — so they are enumerated rather than left to
/// the random draw to stumble on.
fn boundary_vectors() -> Vec<(String, BasicSafetyMessage)> {
    let mut out = Vec::new();
    let mut add = |name: &str, f: &dyn Fn(&mut BsmCoreData)| {
        let mut core = nominal();
        f(&mut core);
        out.push((format!("boundary/{name}"), BasicSafetyMessage::part_i(core)));
    };

    add("msgCnt-min", &|c| c.msg_cnt = 0);
    add("msgCnt-max", &|c| c.msg_cnt = bsm::MSG_COUNT_MAX as u8);
    add("id-zero", &|c| c.id = [0; 4]);
    add("id-ones", &|c| c.id = [0xff; 4]);
    add("secMark-min", &|c| c.sec_mark = 0);
    add("secMark-unavailable", &|c| {
        c.sec_mark = bsm::D_SECOND_UNAVAILABLE
    });
    add("lat-min", &|c| c.lat = bsm::LATITUDE_MIN as i32);
    add("lat-unavailable", &|c| c.lat = bsm::LATITUDE_UNAVAILABLE);
    add("lon-min", &|c| c.lon = bsm::LONGITUDE_MIN as i32);
    add("lon-unavailable", &|c| c.lon = bsm::LONGITUDE_UNAVAILABLE);
    add("elev-unknown", &|c| c.elev = bsm::ELEVATION_UNKNOWN);
    add("elev-max", &|c| c.elev = bsm::ELEVATION_MAX as i32);
    add("accuracy-unavailable", &|c| {
        c.accuracy = PositionalAccuracy::UNAVAILABLE
    });
    add("accuracy-zero", &|c| {
        c.accuracy = PositionalAccuracy {
            semi_major: 0,
            semi_minor: 0,
            orientation: 0,
        }
    });
    add("transmission-neutral", &|c| {
        c.transmission = TransmissionState::Neutral
    });
    add("transmission-unavailable", &|c| {
        c.transmission = TransmissionState::Unavailable
    });
    add("speed-zero", &|c| c.speed = 0);
    add("speed-unavailable", &|c| c.speed = bsm::SPEED_UNAVAILABLE);
    add("heading-zero", &|c| c.heading = 0);
    add("heading-unavailable", &|c| {
        c.heading = bsm::HEADING_UNAVAILABLE
    });
    add("angle-min", &|c| {
        c.angle = bsm::STEERING_WHEEL_ANGLE_MIN as i8
    });
    add("angle-unavailable", &|c| {
        c.angle = bsm::STEERING_WHEEL_ANGLE_UNAVAILABLE
    });
    add("accel-min", &|c| {
        c.accel_set = AccelerationSet4Way {
            long: bsm::ACCELERATION_MIN as i16,
            lat: bsm::ACCELERATION_MIN as i16,
            vert: bsm::VERTICAL_ACCELERATION_MIN as i8,
            yaw: bsm::YAW_RATE_MIN as i16,
        }
    });
    add("accel-unavailable", &|c| {
        c.accel_set = AccelerationSet4Way::UNAVAILABLE
    });
    add("accel-max", &|c| {
        c.accel_set = AccelerationSet4Way {
            long: bsm::ACCELERATION_MAX as i16,
            lat: bsm::ACCELERATION_MAX as i16,
            vert: bsm::VERTICAL_ACCELERATION_MAX as i8,
            yaw: bsm::YAW_RATE_MAX as i16,
        }
    });
    add("brakes-unavailable", &|c| {
        c.brakes = BrakeSystemStatus::UNAVAILABLE
    });
    add("brakes-all-set", &|c| {
        c.brakes = BrakeSystemStatus {
            wheel_brakes: BrakeAppliedStatus::UNAVAILABLE.with(BrakeAppliedStatus::ALL_WHEELS),
            traction: ControlStatus::Engaged,
            abs: ControlStatus::Engaged,
            scs: ControlStatus::Engaged,
            brake_boost: BrakeBoostApplied::On,
            aux_brakes: AuxiliaryBrakeStatus::Reserved,
        }
    });
    add("brakes-none", &|c| {
        c.brakes = BrakeSystemStatus {
            wheel_brakes: BrakeAppliedStatus::NONE,
            traction: ControlStatus::Unavailable,
            abs: ControlStatus::Unavailable,
            scs: ControlStatus::Unavailable,
            brake_boost: BrakeBoostApplied::Unavailable,
            aux_brakes: AuxiliaryBrakeStatus::Unavailable,
        }
    });
    add("size-zero", &|c| {
        c.size = VehicleSize {
            width: 0,
            length: 0,
        }
    });
    add("size-max", &|c| {
        c.size = VehicleSize {
            width: bsm::VEHICLE_WIDTH_MAX as u16,
            length: bsm::VEHICLE_LENGTH_MAX as u16,
        }
    });

    // Whole-message extremes, where every field is at the same end at once.
    let mut min_all = nominal();
    min_all.msg_cnt = 0;
    min_all.id = [0; 4];
    min_all.sec_mark = 0;
    min_all.lat = bsm::LATITUDE_MIN as i32;
    min_all.lon = bsm::LONGITUDE_MIN as i32;
    min_all.elev = bsm::ELEVATION_MIN as i32;
    min_all.accuracy = PositionalAccuracy {
        semi_major: 0,
        semi_minor: 0,
        orientation: 0,
    };
    min_all.transmission = TransmissionState::Neutral;
    min_all.speed = 0;
    min_all.heading = 0;
    min_all.angle = bsm::STEERING_WHEEL_ANGLE_MIN as i8;
    min_all.accel_set = AccelerationSet4Way {
        long: bsm::ACCELERATION_MIN as i16,
        lat: bsm::ACCELERATION_MIN as i16,
        vert: bsm::VERTICAL_ACCELERATION_MIN as i8,
        yaw: bsm::YAW_RATE_MIN as i16,
    };
    min_all.brakes = BrakeSystemStatus {
        wheel_brakes: BrakeAppliedStatus::NONE,
        traction: ControlStatus::Unavailable,
        abs: ControlStatus::Unavailable,
        scs: ControlStatus::Unavailable,
        brake_boost: BrakeBoostApplied::Unavailable,
        aux_brakes: AuxiliaryBrakeStatus::Unavailable,
    };
    min_all.size = VehicleSize {
        width: 0,
        length: 0,
    };
    out.push((
        "boundary/all-minimum".to_string(),
        BasicSafetyMessage::part_i(min_all),
    ));

    let mut max_all = nominal();
    max_all.msg_cnt = bsm::MSG_COUNT_MAX as u8;
    max_all.id = [0xff; 4];
    max_all.sec_mark = bsm::D_SECOND_MAX as u16;
    max_all.lat = bsm::LATITUDE_MAX as i32;
    max_all.lon = bsm::LONGITUDE_MAX as i32;
    max_all.elev = bsm::ELEVATION_MAX as i32;
    max_all.accuracy = PositionalAccuracy {
        semi_major: 255,
        semi_minor: 255,
        orientation: 65_535,
    };
    max_all.transmission = TransmissionState::Unavailable;
    max_all.speed = bsm::SPEED_MAX as u16;
    max_all.heading = bsm::HEADING_MAX as u16;
    max_all.angle = bsm::STEERING_WHEEL_ANGLE_MAX as i8;
    max_all.accel_set = AccelerationSet4Way {
        long: bsm::ACCELERATION_MAX as i16,
        lat: bsm::ACCELERATION_MAX as i16,
        vert: bsm::VERTICAL_ACCELERATION_MAX as i8,
        yaw: bsm::YAW_RATE_MAX as i16,
    };
    max_all.brakes = BrakeSystemStatus {
        wheel_brakes: BrakeAppliedStatus(0b1_1111),
        traction: ControlStatus::Engaged,
        abs: ControlStatus::Engaged,
        scs: ControlStatus::Engaged,
        brake_boost: BrakeBoostApplied::On,
        aux_brakes: AuxiliaryBrakeStatus::Reserved,
    };
    max_all.size = VehicleSize {
        width: bsm::VEHICLE_WIDTH_MAX as u16,
        length: bsm::VEHICLE_LENGTH_MAX as u16,
    };
    out.push((
        "boundary/all-maximum".to_string(),
        BasicSafetyMessage::part_i(max_all),
    ));

    out.push((
        "boundary/all-unavailable".to_string(),
        BasicSafetyMessage::part_i(BsmCoreData::unavailable([0x5a; 4])),
    ));

    // Part II extremes: the smallest and the largest crumb list, and every optional in a
    // path history point present or absent.
    let one_point = VehicleSafetyExtensions {
        path_history: Some(PathHistory::new(vec![PathHistoryPoint::new(
            0,
            0,
            0,
            bsm::TIME_OFFSET_MIN as u16,
        )])),
        ..Default::default()
    };
    out.push((
        "boundary/partII-one-crumb".to_string(),
        BasicSafetyMessage::part_i(nominal()).with_vehicle_safety(one_point),
    ));

    let full = VehicleSafetyExtensions {
        events: Some(VehicleEventFlags(VehicleEventFlags::ROOT_MASK)),
        path_history: Some(PathHistory {
            gnss_status: Some(GnssStatus(0xff)),
            crumb_data: (0..bsm::MAX_PATH_HISTORY_POINTS)
                .map(|i| PathHistoryPoint {
                    speed: Some(i as u16),
                    pos_accuracy: Some(PositionalAccuracy {
                        semi_major: i as u8,
                        semi_minor: 255 - i as u8,
                        orientation: (i as u16) * 1_000,
                    }),
                    heading: Some(bsm::COARSE_HEADING_UNAVAILABLE),
                    ..PathHistoryPoint::new(
                        bsm::OFFSET_LL_B18_MIN as i32,
                        bsm::OFFSET_LL_B18_MAX as i32,
                        bsm::VERT_OFFSET_B12_MIN as i16,
                        bsm::TIME_OFFSET_MAX as u16,
                    )
                })
                .collect(),
        }),
        path_prediction: Some(PathPrediction {
            radius_of_curve: bsm::RADIUS_OF_CURVATURE_MIN as i16,
            confidence: bsm::CONFIDENCE_MAX as u8,
        }),
        lights: Some(ExteriorLights(ExteriorLights::ROOT_MASK)),
    };
    out.push((
        "boundary/partII-full".to_string(),
        BasicSafetyMessage::part_i(nominal()).with_vehicle_safety(full),
    ));

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

fn random_core(rng: &mut RngStream) -> BsmCoreData {
    let mut id = [0u8; 4];
    rng.fill_bytes(&mut id);
    BsmCoreData {
        msg_cnt: pick(rng, bsm::MSG_COUNT_MIN, bsm::MSG_COUNT_MAX) as u8,
        id,
        sec_mark: pick(rng, bsm::D_SECOND_MIN, bsm::D_SECOND_MAX) as u16,
        lat: pick(rng, bsm::LATITUDE_MIN, bsm::LATITUDE_MAX) as i32,
        lon: pick(rng, bsm::LONGITUDE_MIN, bsm::LONGITUDE_MAX) as i32,
        elev: pick(rng, bsm::ELEVATION_MIN, bsm::ELEVATION_MAX) as i32,
        accuracy: PositionalAccuracy {
            semi_major: pick(rng, bsm::SEMI_AXIS_MIN, bsm::SEMI_AXIS_MAX) as u8,
            semi_minor: pick(rng, bsm::SEMI_AXIS_MIN, bsm::SEMI_AXIS_MAX) as u8,
            orientation: pick(rng, bsm::ORIENTATION_MIN, bsm::ORIENTATION_MAX) as u16,
        },
        transmission: TransmissionState::from_index(rng.below(TransmissionState::COUNT))
            .expect("in range"),
        speed: pick(rng, bsm::SPEED_MIN, bsm::SPEED_MAX) as u16,
        heading: pick(rng, bsm::HEADING_MIN, bsm::HEADING_MAX) as u16,
        angle: pick(
            rng,
            bsm::STEERING_WHEEL_ANGLE_MIN,
            bsm::STEERING_WHEEL_ANGLE_MAX,
        ) as i8,
        accel_set: AccelerationSet4Way {
            long: pick(rng, bsm::ACCELERATION_MIN, bsm::ACCELERATION_MAX) as i16,
            lat: pick(rng, bsm::ACCELERATION_MIN, bsm::ACCELERATION_MAX) as i16,
            vert: pick(
                rng,
                bsm::VERTICAL_ACCELERATION_MIN,
                bsm::VERTICAL_ACCELERATION_MAX,
            ) as i8,
            yaw: pick(rng, bsm::YAW_RATE_MIN, bsm::YAW_RATE_MAX) as i16,
        },
        brakes: BrakeSystemStatus {
            wheel_brakes: BrakeAppliedStatus(rng.below(32) as u8),
            traction: ControlStatus::from_index(rng.below(ControlStatus::COUNT)).expect("in range"),
            abs: ControlStatus::from_index(rng.below(ControlStatus::COUNT)).expect("in range"),
            scs: ControlStatus::from_index(rng.below(ControlStatus::COUNT)).expect("in range"),
            brake_boost: BrakeBoostApplied::from_index(rng.below(BrakeBoostApplied::COUNT))
                .expect("in range"),
            aux_brakes: AuxiliaryBrakeStatus::from_index(rng.below(AuxiliaryBrakeStatus::COUNT))
                .expect("in range"),
        },
        size: VehicleSize {
            width: pick(rng, bsm::VEHICLE_WIDTH_MIN, bsm::VEHICLE_WIDTH_MAX) as u16,
            length: pick(rng, bsm::VEHICLE_LENGTH_MIN, bsm::VEHICLE_LENGTH_MAX) as u16,
        },
    }
}

fn random_vse(rng: &mut RngStream) -> VehicleSafetyExtensions {
    loop {
        let vse = VehicleSafetyExtensions {
            events: (rng.below(2) == 1).then(|| {
                VehicleEventFlags(rng.below(u64::from(VehicleEventFlags::ROOT_MASK) + 1) as u16)
            }),
            path_history: (rng.below(2) == 1).then(|| PathHistory {
                gnss_status: (rng.below(2) == 1).then(|| GnssStatus(rng.below(256) as u8)),
                crumb_data: (0..=rng.below(bsm::MAX_PATH_HISTORY_POINTS as u64))
                    .map(|_| PathHistoryPoint {
                        lat_offset: pick(rng, bsm::OFFSET_LL_B18_MIN, bsm::OFFSET_LL_B18_MAX)
                            as i32,
                        lon_offset: pick(rng, bsm::OFFSET_LL_B18_MIN, bsm::OFFSET_LL_B18_MAX)
                            as i32,
                        elevation_offset: pick(
                            rng,
                            bsm::VERT_OFFSET_B12_MIN,
                            bsm::VERT_OFFSET_B12_MAX,
                        ) as i16,
                        time_offset: pick(rng, bsm::TIME_OFFSET_MIN, bsm::TIME_OFFSET_MAX) as u16,
                        speed: (rng.below(2) == 1)
                            .then(|| pick(rng, bsm::SPEED_MIN, bsm::SPEED_MAX) as u16),
                        pos_accuracy: (rng.below(2) == 1).then(|| PositionalAccuracy {
                            semi_major: rng.below(256) as u8,
                            semi_minor: rng.below(256) as u8,
                            orientation: rng.below(65_536) as u16,
                        }),
                        heading: (rng.below(2) == 1).then(|| {
                            pick(rng, bsm::COARSE_HEADING_MIN, bsm::COARSE_HEADING_MAX) as u8
                        }),
                    })
                    .collect(),
            }),
            path_prediction: (rng.below(2) == 1).then(|| PathPrediction {
                radius_of_curve: pick(
                    rng,
                    bsm::RADIUS_OF_CURVATURE_MIN,
                    bsm::RADIUS_OF_CURVATURE_MAX,
                ) as i16,
                confidence: pick(rng, bsm::CONFIDENCE_MIN, bsm::CONFIDENCE_MAX) as u8,
            }),
            lights: (rng.below(2) == 1).then(|| {
                ExteriorLights(rng.below(u64::from(ExteriorLights::ROOT_MASK) + 1) as u16)
            }),
        };
        // An all-absent container encodes to five bits that say nothing, and pycrate
        // refuses to build one, so redraw rather than compare an artefact neither side
        // would ever send.
        if !vse.is_empty() {
            return vse;
        }
    }
}

fn random_vectors(count: usize) -> Vec<(String, BasicSafetyMessage)> {
    let mut rng = RngStream::from_key(SEED);
    (0..count)
        .map(|i| {
            let core = random_core(&mut rng);
            let mut message = BasicSafetyMessage::part_i(core);
            if rng.below(2) == 1 {
                message = message.with_vehicle_safety(random_vse(&mut rng));
            }
            (format!("random/{i:03}"), message)
        })
        .collect()
}

// =========================================================================================
// Running the oracle
// =========================================================================================

struct OracleRun {
    vectors: Vec<(String, BasicSafetyMessage)>,
    report: Value,
    stdout: String,
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Where the prepared Python environment lives, if it has been prepared.
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
SKIPPED: the pycrate oracle needs a Python environment this machine does not have.
  Set V2XW_J2735_ORACLE_DIR to a directory holding j2735_all.py and .venv/bin/python.
  See the module documentation in tests/j2735_oracle.rs for the three commands that
  build it. The SAE J2735 modules are not in this repository (build decision D3), so
  CI cannot build it and skips here — the hand-written codec's own round-trip tests
  still run, they simply cannot prove conformance on their own.";

/// Runs the oracle once for the whole test binary; both tests read the same result.
fn oracle() -> Option<&'static OracleRun> {
    static RUN: OnceLock<Option<OracleRun>> = OnceLock::new();
    RUN.get_or_init(|| {
        let dir = oracle_dir()?;
        let python = python(&dir);
        if !python.is_file() {
            eprintln!("{SKIP_MESSAGE}\n  (no interpreter at {})", python.display());
            return None;
        }

        let mut vectors = boundary_vectors();
        vectors.extend(random_vectors(RANDOM_VECTORS));

        let payload: Vec<Value> = vectors
            .iter()
            .map(|(name, message)| {
                let encoded = bsm::encode_bsm(message).expect("the codec encodes its own vector");
                let framed =
                    bsm::encode_message_frame(message).expect("the codec frames its own vector");
                json!({
                    "name": name,
                    "value": bsm_json(message),
                    "rust_hex": hex(&encoded.bytes),
                    "rust_frame_hex": hex(&framed.bytes),
                })
            })
            .collect();

        let work = std::env::temp_dir().join("v2xw-j2735-oracle");
        std::fs::create_dir_all(&work).expect("scratch directory");
        let vectors_path = work.join("vectors.json");
        let results_path = work.join("results.json");
        std::fs::write(
            &vectors_path,
            serde_json::to_vec(&Value::Array(payload)).expect("serialises"),
        )
        .expect("writes vectors");

        let output = Command::new(&python)
            .arg(manifest_dir().join("tests/oracle/oracle.py"))
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
            vectors,
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

#[test]
fn rust_encodings_match_pycrate_byte_for_byte() {
    let Some(run) = oracle() else {
        eprintln!("{SKIP_MESSAGE}");
        return;
    };
    let results = run.report["pycrate_results"]
        .as_array()
        .expect("a result per vector");
    assert_eq!(results.len(), run.vectors.len());

    let mut failures = Vec::new();
    for (result, (name, message)) in results.iter().zip(&run.vectors) {
        assert_eq!(result["name"].as_str(), Some(name.as_str()));
        if let Some(error) = result["error"].as_str() {
            failures.push(format!("{name}: pycrate raised {error}"));
            continue;
        }
        if result["encode_match"] != json!(true) {
            let ours = hex(&bsm::encode_bsm(message).expect("encodes").bytes);
            failures.push(format!(
                "{name}: encoding differs\n    rust:    {ours}\n    pycrate: {}",
                result["py_hex"].as_str().unwrap_or("?")
            ));
        }
        if result["frame_match"] != json!(true) {
            failures.push(format!(
                "{name}: MessageFrame encoding differs\n    rust:    {}\n    pycrate: {}",
                hex(&bsm::encode_message_frame(message).expect("frames").bytes),
                result["py_frame_hex"].as_str().unwrap_or("?")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} vectors encoded differently from pycrate:\n{}",
        failures.len(),
        run.vectors.len(),
        failures.join("\n")
    );
    println!(
        "pycrate oracle: {} vectors, byte-identical encodings\n{}",
        run.vectors.len(),
        run.stdout.trim()
    );
}

#[test]
fn pycrate_encodings_decode_field_for_field() {
    let Some(run) = oracle() else {
        eprintln!("{SKIP_MESSAGE}");
        return;
    };
    let results = run.report["pycrate_results"]
        .as_array()
        .expect("a result per vector");

    let mut failures = Vec::new();
    for (result, (name, message)) in results.iter().zip(&run.vectors) {
        if result.get("error").is_some() {
            continue; // already reported by the encoding test
        }
        // Direction 2: pycrate read what we wrote.
        if result["decode_match"] != json!(true) {
            let difference = first_difference(&bsm_json(message), &result["py_decoded"], "bsm")
                .unwrap_or_else(|| "values compare equal but the test says otherwise".to_string());
            failures.push(format!(
                "{name}: pycrate decoded our bytes differently: {difference}"
            ));
        }
        // Direction 3: we read what pycrate wrote.
        let py_bytes = unhex(result["py_hex"].as_str().expect("pycrate's octets"));
        match bsm::decode_bsm(&py_bytes) {
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
        "{} of {} vectors disagreed:\n{}",
        failures.len(),
        run.vectors.len(),
        failures.join("\n")
    );
    println!(
        "pycrate oracle: {} vectors, field-identical in both directions",
        run.vectors.len()
    );
}

/// The containers the codec does not model must survive a decode and re-encode unchanged.
///
/// Nothing in a Rust-only test can check this, because the Rust side never constructs one:
/// the bytes have to come from an implementation that *does* model them.
#[test]
fn unmodelled_part_ii_containers_survive_pycrate_round_trip() {
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
    assert!(!vectors.is_empty());

    for vector in vectors {
        let name = vector["name"].as_str().expect("a name");
        let bytes = unhex(vector["hex"].as_str().expect("octets"));
        let decoded = bsm::decode_bsm(&bytes)
            .unwrap_or_else(|e| panic!("{name}: a message pycrate built did not decode: {e}"));
        assert!(
            decoded
                .part_ii
                .iter()
                .any(|c| matches!(c.value, PartIIValue::Opaque(_))),
            "{name}: the unmodelled container should have been kept opaque"
        );
        let re =
            bsm::encode_bsm(&decoded).unwrap_or_else(|e| panic!("{name}: re-encoding failed: {e}"));
        assert_eq!(
            hex(&re.bytes),
            vector["hex"].as_str().expect("octets"),
            "{name}: an opaque container did not survive the round trip"
        );
    }
    println!(
        "pycrate oracle: {} unmodelled-container vectors round-tripped byte for byte",
        vectors.len()
    );
}
