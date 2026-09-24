//! The fixture engine: determinism, the model card, and the frame types it exercises.
//!
//! The fixture's whole value is that it is reproducible, so that a difference a client
//! sees is a difference in the transport. That is a property worth a test rather than a
//! comment.

use v2xw_core::card::SourceKind;
use v2xw_server::engine::{Control, Engine, RunState};
use v2xw_server::{StubEngine, StubOptions};

fn options() -> StubOptions {
    StubOptions {
        actors: 20,
        grid: 4,
        duration_s: 15,
        ..StubOptions::default()
    }
}

#[test]
fn two_engines_with_the_same_options_produce_the_same_stream() {
    let mut a = StubEngine::new(options()).expect("a");
    let mut b = StubEngine::new(options()).expect("b");
    assert_eq!(a.descriptor().run_id, b.descriptor().run_id);
    assert_eq!(
        a.world().content_hash,
        b.world().content_hash,
        "the generated world is reproducible (conformance W5)"
    );
    for step in 0..40 {
        let x = a.step().expect("a step").expect("more");
        let y = b.step().expect("b step").expect("more");
        assert_eq!(x.sim_time, y.sim_time, "step {step}");
        assert_eq!(
            x.snapshot, y.snapshot,
            "the snapshot at step {step} differs between two identical runs"
        );
        assert_eq!(x.telemetry, y.telemetry, "telemetry at step {step}");
        assert_eq!(x.events, y.events, "events at step {step}");
        assert_eq!(x.metrics, y.metrics, "metrics at step {step}");
    }
}

#[test]
fn a_different_seed_produces_a_different_stream() {
    // Otherwise the determinism test above would pass on an engine that ignored its input.
    let mut a = StubEngine::new(options()).expect("a");
    let mut b = StubEngine::new(StubOptions {
        seed: 7,
        ..options()
    })
    .expect("b");
    let x = a.step().expect("step").expect("more");
    let y = b.step().expect("step").expect("more");
    assert_ne!(x.snapshot, y.snapshot, "the seed must reach the stream");
}

#[test]
fn a_step_is_exactly_one_mobility_step_of_simulated_time() {
    let mut engine = StubEngine::new(options()).expect("engine");
    let step = engine.descriptor().cadence.mobility_step.as_nanos();
    let mut previous = None;
    for _ in 0..12 {
        let out = engine.step().expect("step").expect("more");
        if let Some(prev) = previous {
            assert_eq!(out.sim_time - prev, step);
        }
        previous = Some(out.sim_time);
    }
}

#[test]
fn the_run_ends_at_its_duration_and_the_last_step_says_so() {
    let mut engine = StubEngine::new(StubOptions {
        duration_s: 2,
        ..options()
    })
    .expect("engine");
    let mut last_flagged = false;
    let mut steps = 0;
    while let Some(out) = engine.step().expect("step") {
        last_flagged = out.end_of_run;
        steps += 1;
        assert!(steps < 1_000, "the run must terminate");
    }
    assert!(steps >= 20, "two seconds at 100 ms steps");
    assert!(last_flagged, "the last step sets end_of_run");
    assert_eq!(engine.state(), RunState::Finished);
}

#[test]
fn every_step_carries_actors_signals_telemetry_and_events() {
    let mut engine = StubEngine::new(options()).expect("engine");
    let out = engine.step().expect("step").expect("more");
    assert!(!out.snapshot.actors.is_empty(), "actors");
    assert!(
        !out.snapshot.signals.is_empty(),
        "signals from the world's plans"
    );
    assert!(!out.telemetry.is_empty(), "telemetry");
    assert!(!out.events.is_empty(), "events");
    assert!(
        out.snapshot.actors.iter().any(|a| a.node.is_none()),
        "some actors are unequipped, so the node profile has something to withhold"
    );
    assert!(
        out.snapshot
            .actors
            .iter()
            .any(|a| a.accel_mps2.abs() > 1e-6),
        "the ground-truth acceleration column is not a constant zero, so blanking it is \
         an observable change"
    );
    assert!(
        out.recorded.is_empty(),
        "a live engine forwards no recorded frames"
    );
}

#[test]
fn the_event_index_is_sorted_by_time_then_channel() {
    // §3.6.1's MUST, and conformance C4.
    let mut engine = StubEngine::new(options()).expect("engine");
    for _ in 0..30 {
        let out = engine.step().expect("step").expect("more");
        for pair in out.events.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                (a.sim_time_ns, a.channel_id) <= (b.sim_time_ns, b.channel_id),
                "event index out of order: {:?} then {:?}",
                (a.sim_time_ns, a.channel_id),
                (b.sim_time_ns, b.channel_id)
            );
        }
    }
}

#[test]
fn run_control_refuses_what_section_6_6_says_it_should() {
    let mut engine = StubEngine::new(options()).expect("engine");
    assert_eq!(engine.state(), RunState::Running);
    assert_eq!(
        engine
            .control(Control::Resume)
            .expect_err("not paused")
            .code(),
        -32002
    );
    engine.control(Control::Pause).expect("pause");
    assert_eq!(
        engine
            .control(Control::Pause)
            .expect_err("already paused")
            .code(),
        -32002
    );
    engine.control(Control::Resume).expect("resume");
    assert_eq!(
        engine
            .control(Control::Start {
                paused: false,
                speed: Some(1.0),
                seed: None,
                scenario: None,
            })
            .expect_err("already running")
            .code(),
        -32001
    );
}

#[test]
fn the_model_card_declares_every_number_as_todo_calibrate_with_a_plan() {
    // Registry rule R1. This module invents every value it reports, so a card that let
    // one of them pass as sourced would be the worst kind of defect here: a fixture
    // number cited as a measurement.
    let card = v2xw_server::stub::card();
    card.validate().expect("the card validates");
    assert!(!card.parameters.is_empty());
    for parameter in &card.parameters {
        assert_eq!(
            parameter.source.kind,
            SourceKind::TodoCalibrate,
            "`{}` claims a source it does not have",
            parameter.name
        );
        let plan = parameter
            .calibration
            .as_deref()
            .unwrap_or_else(|| panic!("`{}` has no calibration plan", parameter.name));
        assert!(!plan.trim().is_empty());
    }
    assert!(!card.limitations.is_empty(), "the card states its limits");
    assert!(!card.assumptions.is_empty());
}

#[test]
fn the_seek_range_is_the_whole_run_and_a_seek_repositions_it() {
    let mut engine = StubEngine::new(options()).expect("engine");
    let (min_ns, max_ns) = engine.seek_range();
    assert_eq!(min_ns, 0);
    assert_eq!(max_ns, 15_000_000_000);
    let outputs = engine.seek(5_500_000_000).expect("seek");
    assert!(!outputs.is_empty());
    assert_eq!(
        outputs.last().expect("last").sim_time,
        5_500_000_000,
        "the last step is the target"
    );
    assert_eq!(
        engine.state(),
        RunState::Paused,
        "§6.6: a seek pauses a live run"
    );
    assert_eq!(
        engine.seek(max_ns + 1).expect_err("past the end").code(),
        -32003
    );
}

#[test]
fn a_seek_is_a_pure_function_of_its_target() {
    // The property that makes a seek and normal production agree: `output_at` has no
    // hidden state, so seeking twice to the same instant yields the same bytes.
    let mut a = StubEngine::new(options()).expect("a");
    let mut b = StubEngine::new(options()).expect("b");
    for _ in 0..20 {
        b.step().expect("step");
    }
    let from_seek = a.seek(3_000_000_000).expect("seek");
    let from_seek_again = b.seek(3_000_000_000).expect("seek");
    assert_eq!(from_seek.len(), from_seek_again.len());
    for (x, y) in from_seek.iter().zip(from_seek_again.iter()) {
        assert_eq!(x.sim_time, y.sim_time);
        assert_eq!(x.snapshot, y.snapshot);
    }
}
