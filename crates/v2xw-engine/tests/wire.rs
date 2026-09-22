//! The normative binary wire path, and the instant a ground-truth record is stamped at.
//!
//! Both are vertical-slice audit findings, and both are the kind that a
//! "does the run produce records" test passes over:
//!
//! * `RecordingWriter::write_frame` was called from nowhere, so a recording held JSON
//!   records and no `Keyframe` or `Delta` at all. A browser could not replay a run and
//!   vwp-v1 §7.2's byte-identity guarantee had no live stream to be identical to.
//! * `EngineCtx::emit_erased` stamped every record at the scheduler's instant, and a
//!   mobility step dispatched at `t` produces the world at `t + dt` — so every
//!   `gt.kinematics` record was filed one step before the state it described, against its
//!   own `t` field.
//!
//! The tests below decode the frames with `v2xw-record`'s own decoders rather than
//! re-deriving the layout, and cross-check the two paths against each other: a keyframe's
//! quantised position has to agree with the `gt.kinematics` record of the same instant,
//! which is a property neither path can satisfy alone by being self-consistent.

use std::path::{Path, PathBuf};

use v2xw_engine::{Engine, MemoryRecorder, Scenario};
use v2xw_metrics::channels::{GtKinematicsView, decode};
use v2xw_record::wire::MsgType;
use v2xw_record::wire::snapshot::{DeltaBody, KeyframeBody};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

fn traffic_scenario() -> Scenario {
    Scenario::load(scenarios().join("grid-traffic.yaml")).expect("loads")
}

fn run() -> (MemoryRecorder, v2xw_engine::RunReport) {
    let mut engine = Engine::build(traffic_scenario(), "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (recorder, report)
}

/// The run writes the binary snapshot stream: one frame per mobility step, opening with a
/// keyframe and carrying a keyframe every cadence period after it.
///
/// This is the check whose absence the audit found. It asserts on *frames decoded from
/// their own bytes*, not on a counter the producer incremented, because a counter is
/// exactly what a producer that writes nothing still increments.
#[test]
fn the_run_writes_keyframes_and_deltas() {
    let (recorder, report) = run();

    assert!(
        report.keyframes > 0,
        "no keyframe was produced: the binary wire path is still dead"
    );
    assert!(report.deltas > 0, "no delta was produced");
    assert_eq!(
        report.frames_written,
        Some(report.keyframes + report.deltas),
        "the recorder did not store every frame the engine produced"
    );
    assert_eq!(
        recorder.frames().len() as u64,
        report.keyframes + report.deltas
    );

    // One frame per mobility step. The run covers the closed interval [0, duration], so
    // the step count includes both endpoints and so does the frame count.
    assert_eq!(
        recorder.frames().len() as u64,
        report.mobility_steps,
        "one snapshot frame per mobility step"
    );

    let mut kinds = Vec::new();
    for frame in recorder.frames() {
        let header = frame.header().expect("a frame this crate wrote has a header");
        let kind = header.kind().expect("a known message type");
        assert!(
            matches!(kind, MsgType::Keyframe | MsgType::Delta),
            "the snapshot stream carried a {kind:?}"
        );
        kinds.push(kind);
    }
    assert_eq!(
        kinds[0],
        MsgType::Keyframe,
        "a stream opens with a keyframe (§3.3)"
    );

    // The cadence: 1 s keyframes over the scenario's 100 ms step is one keyframe in ten.
    let period = 10usize;
    for (i, kind) in kinds.iter().enumerate() {
        let want = if i % period == 0 {
            MsgType::Keyframe
        } else {
            MsgType::Delta
        };
        assert_eq!(*kind, want, "frame {i} is a {kind:?}, not a {want:?}");
    }
}

/// Every frame decodes, its `sim_time_ns` is the instant it was written at, and the GOP
/// and step indices are dense.
///
/// The properties `Reader::verify` checks over a recording, checked here over the live
/// stream — so a producer defect is found in the engine's own tests rather than by the
/// reader of a file that was already written.
#[test]
fn every_frame_decodes_and_its_indices_are_dense() {
    let (recorder, _) = run();
    let step_ns = traffic_scenario().time.mobility_step().as_nanos();

    let mut expect_time = 0u64;
    let mut gop = 0u32;
    let mut step_index = 0u32;
    let mut first = true;

    for (i, frame) in recorder.frames().iter().enumerate() {
        let kind = frame.header().expect("header").kind().expect("known kind");
        match kind {
            MsgType::Keyframe => {
                let body = KeyframeBody::decode(frame.body()).expect("keyframe decodes");
                assert_eq!(body.sim_time_ns, expect_time, "keyframe {i} is out of step");
                if first {
                    assert_eq!(body.gop_index, 0, "the first GOP is 0");
                    first = false;
                } else {
                    gop += 1;
                    assert_eq!(body.gop_index, gop, "gop_index is not dense at frame {i}");
                }
                assert_eq!(
                    body.profile,
                    v2xw_record::wire::snapshot::PROFILE_FULL,
                    "a full-profile run wrote a node-profile keyframe"
                );
                step_index = 0;
            }
            MsgType::Delta => {
                let body = DeltaBody::decode(frame.body()).expect("delta decodes");
                assert_eq!(body.sim_time_ns, expect_time, "delta {i} is out of step");
                assert_eq!(body.gop_index, gop, "delta {i} quotes the wrong GOP");
                step_index += 1;
                assert_eq!(
                    body.step_index, step_index,
                    "step_index is not dense at frame {i}"
                );
            }
            other => panic!("frame {i} is a {other:?}"),
        }
        expect_time += step_ns;
    }
}

/// **The timestamp fix.** A `gt.kinematics` record is stamped at the instant the state
/// describes, and the position it carries is the position of that mobility step — which
/// the keyframe of the same instant, encoded by a different path, agrees with.
///
/// Injected control at the end: shifting the expectation by one step must fail, so the
/// assertion above is not one that would pass whatever the stamp was.
#[test]
fn a_kinematics_record_names_the_step_whose_position_it_carries() {
    let (recorder, _) = run();

    // Every ground-truth record, keyed by the instant it was stamped at.
    let mut stamped_wrong = 0usize;
    let mut by_instant: std::collections::BTreeMap<(u64, u32), (f64, f64)> =
        std::collections::BTreeMap::new();
    for (at, rec) in recorder.records() {
        if rec.channel != "gt.kinematics" {
            continue;
        }
        let view: GtKinematicsView = decode(rec).expect("a gt.kinematics record decodes");
        if view.t != *at {
            stamped_wrong += 1;
        }
        by_instant.insert((view.t, view.actor.index()), (view.x_m, view.y_m));
    }
    assert!(
        !by_instant.is_empty(),
        "no ground-truth record was written, so this test proves nothing"
    );
    assert_eq!(
        stamped_wrong, 0,
        "{stamped_wrong} kinematics records are filed at an instant other than the one \
         their own `t` names"
    );

    // And the other path agrees. A keyframe's rows are absolute millimetres about the
    // frame's own origin, so this compares two independently produced numbers and not a
    // value against itself.
    let mut compared = 0usize;
    for frame in recorder.frames() {
        let Some(MsgType::Keyframe) = frame.header().expect("header").kind() else {
            continue;
        };
        let body = KeyframeBody::decode(frame.body()).expect("keyframe decodes");
        for row in &body.actors {
            if !row.is_occupied() {
                continue;
            }
            let Some((x, y)) = by_instant.get(&(body.sim_time_ns, row.actor_id)) else {
                panic!(
                    "keyframe at {} carries actor {} and no gt.kinematics record of that \
                     instant does",
                    body.sim_time_ns, row.actor_id
                );
            };
            let frame_x = body.origin[0] + f64::from(row.x_mm) / 1000.0;
            let frame_y = body.origin[1] + f64::from(row.y_mm) / 1000.0;
            // Both paths quantise to the millimetre; half a millimetre is the bound §3.2's
            // rounding rule guarantees, and 1e-6 m of slack covers the binary
            // representation of the sum with the origin.
            assert!(
                (frame_x - x).abs() <= 0.0005 + 1e-6 && (frame_y - y).abs() <= 0.0005 + 1e-6,
                "at {} actor {} is at ({x}, {y}) in the record and ({frame_x}, {frame_y}) \
                 in the keyframe",
                body.sim_time_ns,
                row.actor_id
            );
            compared += 1;
        }
    }
    assert!(
        compared > 0,
        "no keyframe row was compared against a record, so the cross-check did not run"
    );

    // The injected fault. If the record stream were stamped one mobility step away from
    // the frame stream, the lookup above would miss — and an actor that has moved would
    // be at a different place. Shifting by one step must therefore *disagree* somewhere;
    // a comparison that still passed would be comparing a value with itself.
    let step_ns = traffic_scenario().time.mobility_step().as_nanos();
    let mut disagreements = 0usize;
    for frame in recorder.frames() {
        let Some(MsgType::Keyframe) = frame.header().expect("header").kind() else {
            continue;
        };
        let body = KeyframeBody::decode(frame.body()).expect("keyframe decodes");
        for row in &body.actors {
            if !row.is_occupied() {
                continue;
            }
            let shifted = body.sim_time_ns.saturating_sub(step_ns);
            let Some((x, _)) = by_instant.get(&(shifted, row.actor_id)) else {
                disagreements += 1;
                continue;
            };
            let frame_x = body.origin[0] + f64::from(row.x_mm) / 1000.0;
            if (frame_x - x).abs() > 0.0005 + 1e-6 {
                disagreements += 1;
            }
        }
    }
    assert!(
        disagreements > 0,
        "shifting the comparison by one mobility step changed nothing, so the check \
         cannot tell a correctly stamped record from a late one"
    );
}

/// The run report says what the *recorder* kept, not only what the engine handed over.
#[test]
fn the_report_counts_what_the_recorder_wrote() {
    let (recorder, report) = run();
    assert_eq!(
        report.records_written,
        Some(recorder.records().len() as u64),
        "the report's written count disagrees with the recorder's own"
    );
    assert_eq!(
        report.records_written,
        Some(report.records),
        "a memory recorder refuses nothing, so the two counts must agree here; a \
         disagreement means the engine is counting something it never handed over"
    );
}
