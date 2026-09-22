//! The live engine: a real `v2xw-engine` run behind the [`Engine`] seam.
//!
//! Every test here drives the kernel, not a fixture. They are deliberately about the two
//! things that can only be wrong once a real engine is attached — the step boundary the
//! record stream is cut on, and the actor→node map the record stream does not carry — plus
//! the properties the transport promises about a live run's `Hello`, its seek range and
//! its slot bound.
//!
//! `phase1-grid.yaml` is the scenario throughout: its world is procedural, so a test run
//! costs no OSM import, and it is a real Phase 1 scenario rather than a fixture.

use std::path::{Path, PathBuf};

use v2xw_server::engine::{Control, Engine, RunState};
use v2xw_server::live::{LiveEngine, LiveOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

/// The Phase 1 grid scenario with `edit` applied to its YAML, written to a scratch file.
///
/// Editing the text rather than the parsed document is deliberate: the scenario loader is
/// what a server is handed in production, so a test that changes a scenario should change
/// the document the loader reads.
fn scenario(name: &str, edits: &[(&str, &str)]) -> PathBuf {
    let source = repo_root().join("scenarios/phase1-grid.yaml");
    let mut text = std::fs::read_to_string(&source).expect("read phase1-grid.yaml");
    for (from, to) in edits {
        assert!(text.contains(from), "`{from}` is not in phase1-grid.yaml");
        text = text.replace(from, to);
    }
    let dir = std::env::temp_dir().join("v2xw-server-live-tests");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(format!("{name}.yaml"));
    std::fs::write(&path, text).expect("write scenario");
    path
}

fn options() -> LiveOptions {
    LiveOptions {
        // Fixed, because the manifest timestamp is the caller's and a test must not read a
        // clock either. It is excluded from every digest.
        build_utc: "2026-09-22T00:00:00Z".to_string(),
        // Paused: every test here drives `step` itself, and a producer racing it would
        // make the assertions about `sim_time` depend on wall time.
        paused: true,
        ..LiveOptions::default()
    }
}

/// Drains `engine` to the end of its run, returning every step.
fn drain(engine: &mut LiveEngine) -> Vec<v2xw_server::StepOutput> {
    let mut out = Vec::new();
    // The bound is the step count the scenario implies plus slack for the transport's own
    // `Ok(None)` retries; a run that needs more than this has not ended and the assertion
    // on the last step's `end_of_run` is what catches it.
    for _ in 0..20_000 {
        match engine.step().expect("step") {
            Some(step) => out.push(step),
            None if engine.state() == RunState::Finished => break,
            None => continue,
        }
    }
    out
}

/// A digest over a projected stream: the instant, the scene and the events, in order.
///
/// This is the live analogue of `MemoryRecorder::digest_hex` — it covers *order*, because
/// two runs that produce the same steps in a different order are not the same run.
fn digest(steps: &[v2xw_server::StepOutput]) -> String {
    let mut w = v2xw_core::hash::Sha256Writer::new();
    for step in steps {
        w.update(&step.sim_time.to_le_bytes());
        w.update(&(step.snapshot.actors.len() as u64).to_le_bytes());
        for pose in &step.snapshot.actors {
            w.update(&pose.slot.to_le_bytes());
            w.update(&pose.actor.index().to_le_bytes());
            w.update(&pose.node.map_or(u32::MAX, |n| n.index()).to_le_bytes());
            w.update(&pose.pos_m[0].to_le_bytes());
            w.update(&pose.pos_m[1].to_le_bytes());
            w.update(&pose.speed_mps.to_le_bytes());
            w.update(&[pose.state, pose.class_idx]);
        }
        for event in &step.events {
            w.update(&event.sim_time_ns.to_le_bytes());
            w.update(&event.channel_id.to_le_bytes());
            w.update(&event.payload);
        }
        for row in &step.metrics {
            w.update(&row.value.to_le_bytes());
            w.update(&row.str_metric.to_le_bytes());
        }
    }
    w.finish_hex()
}

/// The stream is one step per mobility step, from zero to the horizon, and the last one
/// says so.
///
/// `FLAG_END_OF_RUN` rides on `end_of_run`, and a stream that ends without it leaves a
/// conforming client reconnecting with backoff for ever (§1.4).
#[test]
fn a_live_run_is_one_step_per_mobility_step_and_the_last_one_ends_it() {
    let path = scenario("horizon", &[("duration_s: 60.0", "duration_s: 6.0")]);
    let mut engine = LiveEngine::open(&path, options()).expect("build");
    let step_ns = engine.descriptor().cadence.mobility_step.as_nanos();
    let steps = drain(&mut engine);

    assert_eq!(
        steps.len() as u64,
        6_000_000_000 / step_ns + 1,
        "a 6 s run at {step_ns} ns per step is {} steps, from 0 to the horizon inclusive",
        6_000_000_000 / step_ns + 1
    );
    for (i, step) in steps.iter().enumerate() {
        assert_eq!(
            step.sim_time,
            (i as u64) * step_ns,
            "step {i} is not at its own instant"
        );
    }
    assert!(
        steps.last().expect("steps").end_of_run,
        "the last step must carry end_of_run, or the client never sees FLAG_END_OF_RUN"
    );
    assert!(
        !steps[..steps.len() - 1].iter().any(|s| s.end_of_run),
        "no step but the last may carry end_of_run"
    );
    assert_eq!(engine.state(), RunState::Finished);
}

/// Two engines on one scenario produce one stream, byte for byte.
///
/// This is the determinism contract reaching the transport: the projector holds an RNG, a
/// slot allocator and an actor table, and any of them going through a `HashMap` or being
/// seeded from anything but the scenario would show up here.
#[test]
fn two_live_engines_on_one_scenario_produce_the_same_stream() {
    let path = scenario("determinism", &[("duration_s: 60.0", "duration_s: 8.0")]);
    let mut a = LiveEngine::open(&path, options()).expect("build a");
    let mut b = LiveEngine::open(&path, options()).expect("build b");
    let left = drain(&mut a);
    let right = drain(&mut b);

    assert!(
        left.iter().any(|s| !s.snapshot.actors.is_empty()),
        "the scenario produced no actors at all, so this test would pass on an empty stream"
    );
    assert_eq!(left.len(), right.len());
    assert_eq!(
        digest(&left),
        digest(&right),
        "two runs of one scenario produced different streams"
    );
    assert_eq!(
        a.descriptor().run_id,
        b.descriptor().run_id,
        "the run id is a function of the scenario digest and the seed"
    );
}

/// Every node the record stream names is one the actor→node reconstruction predicted.
///
/// The reconstruction is the one thing in the live path that is not read from the kernel
/// but recomputed from its published rules (see `v2xw_server::live`'s header), so it needs
/// a check that can actually go red. The second half of this test makes it go red: an
/// engine told the wrong equipped fraction draws a different subset, and the mismatch is
/// reported rather than silently producing a stream with the wrong node identities.
#[test]
fn the_actor_to_node_reconstruction_agrees_with_the_nodes_the_records_name() {
    let path = scenario(
        "mapping",
        &[
            ("duration_s: 60.0", "duration_s: 12.0"),
            ("rate_veh_per_h: 30.0", "rate_veh_per_h: 6000.0"),
        ],
    );
    let mut engine = LiveEngine::open(&path, options()).expect("build");
    let steps = drain(&mut engine);
    let nodes: std::collections::BTreeSet<u32> = steps
        .iter()
        .flat_map(|s| s.snapshot.actors.iter())
        .filter_map(|p| p.node.map(|n| n.index()))
        .collect();
    assert!(
        nodes.len() > 3,
        "only {} node(s) were streamed; this test needs a fleet to say anything",
        nodes.len()
    );
    assert!(
        engine.mapping_is_consistent(),
        "the reconstruction did not predict every node the kernel's records named"
    );

    // The check goes red when the reconstruction is wrong. `equipped_fraction: 0.0` makes
    // it predict that nothing is equipped, while the kernel — reading the same scenario —
    // equips nothing either, so that would agree. `0.5` is the case that matters: the
    // kernel equips everything (the scenario says 1.0) and a projector told 0.5 predicts a
    // subset, so nodes appear in the records that it never assigned.
    let wrong = scenario(
        "mapping-wrong",
        &[
            ("duration_s: 60.0", "duration_s: 12.0"),
            ("rate_veh_per_h: 30.0", "rate_veh_per_h: 6000.0"),
        ],
    );
    let mut engine = LiveEngine::open(&wrong, options()).expect("build");
    engine.mis_predict_equipped_fraction_for_test(0.5);
    let _ = drain(&mut engine);
    assert!(
        !engine.mapping_is_consistent(),
        "a deliberately wrong reconstruction was reported as consistent, so the \
         consistency check cannot fail and pins nothing"
    );
}

/// A live run seeks backwards into its own recorded time, and seeking pauses it (§6.6).
#[test]
fn seeking_a_live_run_returns_a_gop_of_recorded_time_and_pauses_it() {
    let path = scenario("seek", &[("duration_s: 60.0", "duration_s: 10.0")]);
    let mut engine = LiveEngine::open(&path, options()).expect("build");
    let step_ns = engine.descriptor().cadence.mobility_step.as_nanos();
    for _ in 0..80 {
        let _ = engine.step().expect("step");
    }
    let before = engine.sim_time();
    assert!(
        before > 0,
        "nothing was streamed, so there is nothing to seek in"
    );

    let (min_ns, max_ns) = engine.seek_range();
    assert_eq!(
        min_ns, 0,
        "nothing has been dropped yet, so the range starts at 0"
    );
    assert!(
        max_ns + step_ns >= before,
        "the range must cover what was streamed"
    );

    let target = step_ns * 25;
    let outputs = engine.seek(target).expect("seek");
    assert_eq!(
        outputs.last().expect("outputs").sim_time,
        target,
        "the last step of a seek is the target instant"
    );
    assert!(
        outputs
            .windows(2)
            .all(|w| w[1].sim_time == w[0].sim_time + step_ns),
        "a seek returns consecutive steps, oldest first"
    );
    let per_gop = engine.descriptor().cadence.max_deltas_per_gop();
    assert!(
        outputs.len() as u64 <= per_gop + 1,
        "conformance P4: at most one keyframe plus {per_gop} deltas, got {}",
        outputs.len()
    );
    assert_eq!(
        engine.state(),
        RunState::Paused,
        "§6.6: seeking pauses a live run"
    );
    assert_eq!(engine.sim_time(), target + step_ns);

    // Past the end of recorded time is out of range, with the range in the error.
    let err = engine
        .seek(max_ns + step_ns * 1_000)
        .expect_err("beyond recorded time");
    assert!(
        matches!(err, v2xw_server::ServerError::SeekOutOfRange { .. }),
        "expected -32003, got {err}"
    );
}

/// No slot reaches the wire at or beyond `Hello.actor_capacity` (§3.1.1).
///
/// `@vwp/protocol` treats the field as a bound and refuses such a slot with a
/// `ProtocolError`, which stops it applying the frame at all, so a server that treats the
/// field as a hint produces a stream the client cannot render. The capacity is set absurdly
/// low here so the bound is actually reached.
#[test]
fn no_slot_reaches_the_wire_at_or_beyond_the_actor_capacity() {
    let path = scenario(
        "capacity",
        &[
            ("duration_s: 60.0", "duration_s: 12.0"),
            ("rate_veh_per_h: 30.0", "rate_veh_per_h: 6000.0"),
        ],
    );
    let mut engine = LiveEngine::open(
        &path,
        LiveOptions {
            actor_capacity: 3,
            ..options()
        },
    )
    .expect("build");
    let steps = drain(&mut engine);
    let peak = steps
        .iter()
        .map(|s| s.snapshot.actors.len())
        .max()
        .unwrap_or(0);
    assert!(
        peak > 0,
        "no actor was streamed, so the bound was never tested"
    );
    for step in &steps {
        for pose in &step.snapshot.actors {
            assert!(
                pose.slot < 3,
                "slot {} is at or beyond actor_capacity 3 at t={}",
                pose.slot,
                step.sim_time
            );
        }
    }
    assert_eq!(engine.descriptor().hello.actor_capacity, 3);
}

/// Pause holds the stream where it is; resume continues from the same step.
#[test]
fn pause_holds_the_stream_and_resume_continues_from_the_same_step() {
    let path = scenario("control", &[("duration_s: 60.0", "duration_s: 10.0")]);
    let mut engine = LiveEngine::open(
        &path,
        LiveOptions {
            paused: false,
            ..options()
        },
    )
    .expect("build");
    for _ in 0..30 {
        let _ = engine.step().expect("step");
    }
    let outcome = engine.control(Control::Pause).expect("pause");
    assert_eq!(outcome.state, RunState::Paused);
    let held = engine.sim_time();
    assert_eq!(outcome.t_ns, held);

    // A second pause is -32002: the state machine of §6.6, not a no-op.
    let err = engine.control(Control::Pause).expect_err("already paused");
    assert!(matches!(err, v2xw_server::ServerError::RunNotRunning(_)));

    engine.control(Control::Resume).expect("resume");
    assert_eq!(engine.state(), RunState::Running);
    assert_eq!(engine.sim_time(), held, "resuming does not move the stream");
    let next = engine.step().expect("step").expect("a step");
    assert_eq!(next.sim_time, held, "the next step is the one pause held");
}

/// Every channel the kernel emits is either projected onto a §3.6 payload or *reported*.
///
/// The failure this closes is a stream that quietly carries less than the run produced:
/// a channel with no arm, or one whose own reader-side view refuses its records. Both are
/// counted, and both reach a client through `explain`'s caveats.
#[test]
fn every_channel_the_kernel_emits_is_projected_or_reported() {
    let path = scenario(
        "coverage",
        &[
            ("duration_s: 60.0", "duration_s: 12.0"),
            ("rate_veh_per_h: 30.0", "rate_veh_per_h: 6000.0"),
        ],
    );
    let mut engine = LiveEngine::open(&path, options()).expect("build");
    let _ = drain(&mut engine);

    let (unprojected, unnamed) = engine.unprojected();
    assert!(
        unprojected.is_empty(),
        "the kernel emitted channels this server has no §3.6 payload for: {unprojected:?}"
    );
    assert!(
        unnamed.is_empty(),
        "the stream carried metrics the symbol table does not hold: {unnamed:?}"
    );

    // `node.verify` is the one channel whose producer and declared reader disagree today
    // (`v2xw_node::VerifyDecisionRecord` against `v2xw_metrics::channels::NodeVerifyView`).
    // The point of this assertion is not that the number is zero — it is not — but that
    // whatever is lost is *counted*, so the day the schemas agree the count goes to zero
    // and the day another channel drifts it does not.
    let lost = engine.undecodable_channels().clone();
    for (channel, count) in &lost {
        assert!(*count > 0, "a channel with no losses must not be listed");
        assert!(
            v2xw_record::CHANNELS.iter().any(|c| c.name == channel),
            "`{channel}` is not a declared channel at all"
        );
    }
}
