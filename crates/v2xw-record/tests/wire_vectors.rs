//! The §9 worked example, byte for byte.
//!
//! `docs/protocol/vwp-v1.md` §9 prints an annotated hex dump of a `Hello`, a `Keyframe`
//! and a `Delta` and says: "Use it as a golden test in both implementations." This file
//! does exactly that, and it reads the dump **out of the specification** rather than from
//! a transcription of it, so the test cannot quietly diverge from the document it is
//! checking: if someone edits a byte in §9, this test fails.
//!
//! Conformance items exercised: F1, F3, F4, F6, Q1, Q4, Q6, C7.

use std::collections::BTreeMap;
use std::path::PathBuf;

use v2xw_record::wire::hello::{ChannelRow, ClassRow, HELLO_LIVE, HelloBody, NodeRow, WorldRef};
use v2xw_record::wire::snapshot::{
    ActorRow, DeltaBody, KeyframeBody, MFLAG_LANE_CHANGED, MovedRow, ST_ATTACKER, ST_EQUIPPED,
    SignalRow,
};
use v2xw_record::wire::{Frame, MsgType, StrTable, U32_NONE};
use v2xw_record::{grid, quant};

/// The specification, as this repository holds it.
fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/protocol/vwp-v1.md")
        .canonicalize()
        .expect("the wire specification is in the repository")
}

/// Every `` ```text `` block of the specification that is a hex dump, parsed into
/// `offset -> byte` and then flattened.
///
/// The dump's format is fixed by §9: eight hex digits of offset, two spaces, then
/// space-separated byte pairs, then the field's name. Continuation lines repeat the
/// offset, so reassembling by offset rather than by order is both simpler and stricter —
/// a gap or an overlap shows up as a missing byte.
fn spec_frames() -> Vec<Vec<u8>> {
    let text = std::fs::read_to_string(spec_path()).expect("the specification is readable");
    let mut out = Vec::new();
    let mut in_block = false;
    let mut bytes: BTreeMap<usize, u8> = BTreeMap::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```text") {
            in_block = true;
            bytes.clear();
            continue;
        }
        if in_block && line.trim_start().starts_with("```") {
            in_block = false;
            if !bytes.is_empty() {
                let lo = *bytes.keys().next().expect("non-empty");
                let hi = *bytes.keys().next_back().expect("non-empty");
                assert_eq!(
                    bytes.len(),
                    hi - lo + 1,
                    "the hex dump at {lo:#010x} has a hole in it"
                );
                out.push(bytes.values().copied().collect());
            }
            continue;
        }
        if !in_block {
            continue;
        }
        let Some((offset, rest)) = line.split_once("  ") else {
            continue;
        };
        if offset.len() != 8 || !offset.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let at = usize::from_str_radix(offset, 16).expect("eight hex digits");
        for (i, token) in rest.split_whitespace().enumerate() {
            if token.len() != 2 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
                break;
            }
            let b = u8::from_str_radix(token, 16).expect("a hex pair");
            bytes.insert(at + i, b);
        }
    }
    out
}

/// The three frames of §9, each as header bytes followed by body bytes.
fn spec_example() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let blocks = spec_frames();
    assert_eq!(
        blocks.len(),
        6,
        "§9 prints three frames as a header block and a body block each"
    );
    let join = |a: &[u8], b: &[u8]| {
        let mut v = a.to_vec();
        v.extend_from_slice(b);
        v
    };
    (
        join(&blocks[0], &blocks[1]),
        join(&blocks[2], &blocks[3]),
        join(&blocks[4], &blocks[5]),
    )
}

/// The `Hello` of §9.1, built from the scenario table above the dump.
fn example_hello() -> HelloBody {
    let mut strings = StrTable::new();
    for s in [
        "v2xw 0.4.0+9f0649d",
        "single-intersection",
        "demo",
        "s_7f3a9c21",
        "/world/d172872bfdd10998babc1497334713546bbbb7dffa1b3b90fddb2d17d641e988.vwb",
        "veh_0000",
        "veh_0001",
        "rsu_north",
        "obu/cohda-mk5",
        "rsu/cohda-mk5-rsu",
        "car",
        "truck",
        "pedestrian",
        "node.tx",
        "phy.rx",
        "gt.kinematics",
    ] {
        strings.intern(s);
    }
    HelloBody {
        version_minor: 0,
        hello_flags: HELLO_LIVE,
        run_id: [
            0x01, 0x89, 0xd4, 0xc7, 0x9f, 0x3a, 0x7b, 0x21, 0x8e, 0x44, 0x5c, 0x6d, 0x7e, 0x8f,
            0x9a, 0x0b,
        ],
        scenario_hash: v2xw_core::sha256(b"vwp-example:scenario:single-intersection"),
        world_hash: v2xw_core::sha256(b"vwp-example:world:single-intersection"),
        t0_wall_ns: 1_804_143_600_000_000_000,
        sim_duration_ns: 600_000_000_000,
        mobility_step_ns: 100_000_000,
        keyframe_period_ns: 1_000_000_000,
        telemetry_period_ns: 1_000_000_000,
        metric_period_ns: 1_000_000_000,
        resume_seq: 0,
        sim_time_ns: 0,
        origin_lat_deg: 52.5163,
        origin_lon_deg: 13.3777,
        origin_alt_m: 34.0,
        bbox_m: [-500.0, -500.0, 500.0, 500.0],
        actor_capacity: 4096,
        nodes: vec![
            NodeRow {
                node_id: 0,
                actor_id: 0,
                pos_m: [0.0, 0.0, 0.0],
                str_label: 6,
                str_profile_id: 9,
                flags: 0x0001,
                kind: 0,
                class_idx: 0,
            },
            NodeRow {
                node_id: 1,
                actor_id: 1,
                pos_m: [0.0, 0.0, 0.0],
                str_label: 7,
                str_profile_id: 9,
                flags: 0x0001,
                kind: 0,
                class_idx: 1,
            },
            NodeRow {
                node_id: 2,
                actor_id: U32_NONE,
                pos_m: [12.0, -8.0, 6.0],
                str_label: 8,
                str_profile_id: 10,
                flags: 0x0001,
                kind: 2,
                class_idx: 0xFF,
            },
        ],
        classes: vec![
            ClassRow {
                str_name: 11,
                length_m: 4.5,
                width_m: 1.8,
                height_m: 1.5,
                color_rgba: 0x3b82_f6ff,
                category: 0,
            },
            ClassRow {
                str_name: 12,
                length_m: 12.0,
                width_m: 2.55,
                height_m: 3.6,
                color_rgba: 0xf59e_0bff,
                category: 0,
            },
            ClassRow {
                str_name: 13,
                length_m: 0.5,
                width_m: 0.5,
                height_m: 1.75,
                color_rgba: 0x10b9_81ff,
                category: 1,
            },
        ],
        channels: vec![
            ChannelRow {
                str_id: 14,
                channel_id: 10,
                visibility: 1,
                enabled: 1,
            },
            ChannelRow {
                str_id: 15,
                channel_id: 11,
                visibility: 3,
                enabled: 1,
            },
            ChannelRow {
                str_id: 16,
                channel_id: 1,
                visibility: 0,
                enabled: 1,
            },
        ],
        world_ref: WorldRef {
            mode: 0,
            format: 0,
            payload_bytes: 1_482_960,
            str_url: 5,
        },
        str_engine_version: 1,
        str_scenario_name: 2,
        str_run_label: 3,
        str_session_token: 4,
        strings,
    }
}

/// The `Keyframe` of §9.2, quantised from the metre values in the scenario table rather
/// than from the integers in the dump — so the test checks the quantiser too (Q1).
fn example_keyframe() -> KeyframeBody {
    let origin = [-500.0, -500.0, 0.0];
    let row = |actor: u32,
               x: f64,
               y: f64,
               z: f64,
               lane: u32,
               heading: f64,
               speed: f64,
               accel: f64,
               class_idx: u8,
               state: u8,
               nbrs: u8| ActorRow {
        actor_id: actor,
        x_mm: quant::x_mm(x, origin[0]),
        y_mm: quant::x_mm(y, origin[1]),
        lane_id: lane,
        z_cm: quant::z_cm(z, origin[2]),
        heading_brad: quant::heading_brad(heading),
        speed_cq: quant::speed_cq(speed),
        accel_cq: quant::accel_cq(accel),
        class_idx,
        state,
        verified_neighbors: nbrs,
        flags8: 0,
    };
    KeyframeBody {
        sim_time_ns: 1_000_000_000,
        origin,
        gop_index: 1,
        profile: 0,
        actors: vec![
            row(
                0,
                12.345,
                -3.210,
                0.15,
                42,
                0.0,
                13.89,
                0.5,
                0,
                ST_EQUIPPED,
                7,
            ),
            row(
                1,
                -20.0,
                0.5,
                0.15,
                43,
                core::f64::consts::PI,
                11.0,
                -1.2,
                1,
                ST_EQUIPPED | ST_ATTACKER,
                5,
            ),
            row(
                2,
                3.0,
                41.25,
                0.10,
                U32_NONE,
                core::f64::consts::FRAC_PI_2,
                0.0,
                0.0,
                2,
                0,
                0,
            ),
        ],
        signals: vec![SignalRow {
            signal_id: 7,
            time_to_change_ds: 128,
            phase: 3,
        }],
    }
}

/// The `Delta` of §9.3.
fn example_delta() -> DeltaBody {
    DeltaBody {
        sim_time_ns: 1_100_000_000,
        gop_index: 1,
        step_index: 1,
        moved: vec![MovedRow {
            slot: 0,
            dx_mm: 1389,
            dy_mm: 0,
            dz_mm: 0,
            heading_brad: 0,
            speed_cq: 1784,
            accel_cq: 32,
            state: ST_EQUIPPED,
            verified_neighbors: 8,
            mflags: MFLAG_LANE_CHANGED,
        }],
        abs: Vec::new(),
        lanes: vec![44],
        spawns: Vec::new(),
        despawns: Vec::new(),
        signals: vec![SignalRow {
            signal_id: 7,
            time_to_change_ds: 118,
            phase: 3,
        }],
    }
}

fn diff(label: &str, got: &[u8], want: &[u8]) {
    assert_eq!(
        got.len(),
        want.len(),
        "{label}: produced {} bytes, the specification prints {}",
        got.len(),
        want.len()
    );
    if let Some(i) = (0..got.len()).find(|i| got[*i] != want[*i]) {
        let lo = i.saturating_sub(8);
        let hi = (i + 8).min(got.len());
        panic!(
            "{label}: first difference at offset {i:#06x}\n  got  {:02x?}\n  want {:02x?}",
            &got[lo..hi],
            &want[lo..hi]
        );
    }
}

#[test]
fn the_hello_of_section_9_1_is_reproduced_byte_for_byte() {
    let (want, _, _) = spec_example();
    let frame = example_hello()
        .to_frame(0, 0)
        .expect("the body fits a frame");
    diff("Hello", frame.as_bytes(), &want);
    assert_eq!(frame.as_bytes().len(), 796, "§9.1: 24 header + 772 body");
    // The same frame passes the writer-side grid scan: the geodetic origin is on 1e-7 and
    // every narrowed dimension is on 1e-3 (D9).
    grid::scan_frame(&frame).expect("every float in the example is on its declared grid");
}

#[test]
fn the_keyframe_of_section_9_2_is_reproduced_byte_for_byte() {
    let (_, want, _) = spec_example();
    let frame = example_keyframe()
        .to_frame(10, 0)
        .expect("the body fits a frame");
    diff("Keyframe", frame.as_bytes(), &want);
    assert_eq!(frame.as_bytes().len(), 180, "§9.2: 24 header + 156 body");
}

#[test]
fn the_delta_of_section_9_3_is_reproduced_byte_for_byte() {
    let (_, _, want) = spec_example();
    let frame = example_delta()
        .to_frame(11, 0)
        .expect("the body fits a frame");
    diff("Delta", frame.as_bytes(), &want);
    assert_eq!(frame.as_bytes().len(), 120, "§9.3: 24 header + 96 body");
}

#[test]
fn the_example_frames_decode_to_the_values_section_9_4_prints() {
    let (hello, keyframe, delta) = spec_example();

    let hello_frame = Frame::from_bytes(hello).expect("a valid frame");
    let h = HelloBody::decode(hello_frame.body()).expect("the Hello decodes");
    assert_eq!(
        h.strings.strings.len(),
        17,
        "§9.1: 17 strings, id 0 is empty"
    );
    assert_eq!(h.strings.get(0), Some(""));
    assert_eq!(h.strings.get(16), Some("gt.kinematics"));
    assert_eq!(
        h.keyframe_origin(),
        [-500.0, -500.0, 0.0],
        "§3.3.1's DECISION"
    );

    let kf_frame = Frame::from_bytes(keyframe).expect("a valid frame");
    let kf = KeyframeBody::decode(kf_frame.body()).expect("the Keyframe decodes");
    // §9.4: "Both decoders produce, for the example keyframe: …"
    assert_eq!(
        kf.actors.iter().map(|a| a.actor_id).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.x_mm).collect::<Vec<_>>(),
        vec![512_345, 480_000, 503_000]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.y_mm).collect::<Vec<_>>(),
        vec![496_790, 500_500, 541_250]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.lane_id).collect::<Vec<_>>(),
        vec![42, 43, 4_294_967_295]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.z_cm).collect::<Vec<_>>(),
        vec![15, 15, 10]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.heading_brad).collect::<Vec<_>>(),
        vec![0, 32_768, 16_384]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.speed_cq).collect::<Vec<_>>(),
        vec![1778, 1408, 0]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.accel_cq).collect::<Vec<_>>(),
        vec![32, -77, 0]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.class_idx).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        kf.actors.iter().map(|a| a.state).collect::<Vec<_>>(),
        vec![0x08, 0x09, 0x00]
    );
    assert_eq!(
        kf.actors
            .iter()
            .map(|a| a.verified_neighbors)
            .collect::<Vec<_>>(),
        vec![7, 5, 0]
    );

    // §9.3: applying the delta to the keyframe state.
    let d = DeltaBody::decode(Frame::from_bytes(delta).expect("a valid frame").body())
        .expect("the Delta decodes");
    let moved = d.moved[0];
    let x_mm = kf.actors[0].x_mm + i32::from(moved.dx_mm);
    assert_eq!(x_mm, 513_734);
    assert_eq!(quant::mm_to_m(i64::from(x_mm), kf.origin[0]), 13.734);
    assert_eq!(f64::from(moved.speed_cq) / 128.0, 13.9375);
    assert_eq!(f64::from(moved.accel_cq) / 64.0, 0.5);
    assert_eq!(d.lanes, vec![44]);
    assert_eq!(moved.mflags, MFLAG_LANE_CHANGED);
}

#[test]
fn a_frame_with_the_wrong_magic_is_rejected() {
    let (hello, _, _) = spec_example();
    let mut bad = hello;
    bad[0] ^= 0xFF;
    let err = Frame::from_bytes(bad).expect_err("F1: bad magic is rejected");
    assert!(
        matches!(err, v2xw_record::RecordError::BadMagic { .. }),
        "expected BadMagic, got {err}"
    );
}

#[test]
fn a_frame_whose_body_len_lies_is_rejected() {
    let (_, keyframe, _) = spec_example();
    let mut bad = keyframe;
    bad[8] = 0xFF; // body_len low byte
    let err = Frame::from_bytes(bad).expect_err("F3: body_len must match the bytes present");
    assert!(
        matches!(err, v2xw_record::RecordError::Malformed { .. }),
        "expected Malformed, got {err}"
    );
}

#[test]
fn an_unknown_message_type_is_recognised_as_unknown_rather_than_an_error() {
    // F2: a reader ignores a frame whose msg_type it does not know.
    let (_, keyframe, _) = spec_example();
    let mut odd = keyframe;
    odd[6] = 0x77;
    odd[7] = 0x00;
    let frame = Frame::from_bytes(odd).expect("the frame is still well formed");
    let header = frame.header().expect("the header parses");
    assert_eq!(header.msg_type, 0x0077);
    assert!(header.kind().is_none(), "this build does not know 0x0077");
    assert!(MsgType::from_id(0x0002) == Some(MsgType::Keyframe));
}
