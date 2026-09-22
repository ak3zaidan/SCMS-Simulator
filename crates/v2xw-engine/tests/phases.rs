//! The phase-parallel structure of ADR 0004 decision 5.
//!
//! Two properties, and both are checked with an injected counterexample rather than only
//! asserted:
//!
//! * **Thread count changes wall time and nothing else.** The reception phase is a pure
//!   map over receivers; running it on one thread and on eight must produce the same
//!   bytes.
//! * **A pure map does not depend on the order it is walked in.** The node phase is walked
//!   forwards and backwards and must publish the same thing.

use std::path::Path;

use v2xw_engine::{Engine, MemoryRecorder, Scenario};

fn traffic_scenario() -> Scenario {
    Scenario::load(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scenarios")
            .join("grid-traffic.yaml"),
    )
    .expect("loads")
}

fn run_on(threads: usize, reverse_nodes: bool) -> (String, v2xw_engine::RunReport) {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool");
    pool.install(|| {
        let mut engine = Engine::build(traffic_scenario(), "").expect("builds");
        engine.set_reverse_node_walk(reverse_nodes);
        let mut recorder = MemoryRecorder::new();
        let report = engine.run(&mut recorder).expect("runs");
        (recorder.digest_hex(), report)
    })
}

/// The reception phase runs on whatever `rayon` gives it, and the run is byte-identical.
#[test]
fn the_run_is_identical_on_one_and_eight_threads() {
    let (one, report_one) = run_on(1, false);
    let (eight, report_eight) = run_on(8, false);
    assert!(
        report_one.reception_attempts > 0,
        "the parallel phase never ran, so this comparison proves nothing"
    );
    assert_eq!(report_one, report_eight);
    assert_eq!(one, eight, "the record stream depends on the thread count");
}

/// The node phase is a pure map: walking the nodes backwards publishes the same run.
#[test]
fn the_node_phase_result_does_not_depend_on_walk_order() {
    let (forward, report_forward) = run_on(4, false);
    let (backward, report_backward) = run_on(4, true);
    assert!(
        report_forward.nodes_created > 1,
        "one node cannot show an ordering property"
    );
    assert_eq!(report_forward, report_backward);
    assert_eq!(
        forward, backward,
        "the node phase reads or writes something outside its own node"
    );
}

/// The injected counterexample for both tests above.
///
/// If the run were insensitive to *everything* — an empty run, or one where the recorder
/// never saw a record — the two comparisons would pass while proving nothing. This shows
/// they are sensitive: one extra vehicle changes the digest.
#[test]
fn the_digest_comparison_is_sensitive_to_a_real_change() {
    let (baseline, _) = run_on(4, false);

    let mut scenario = traffic_scenario();
    scenario.actors.vehicles.demand.rate_veh_per_h = Some(7200.0);
    let mut engine = Engine::build(scenario, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    engine.run(&mut recorder).expect("runs");

    assert_ne!(
        baseline,
        recorder.digest_hex(),
        "doubling the demand changed nothing, so the digest is not reading the run"
    );
}

/// A time-dilation window suppresses radio events and says so, rather than leaving a
/// quiet channel indistinguishable from a disabled one (02-architecture.md §5.4).
#[test]
fn a_time_dilation_window_suppresses_radio_events_and_counts_them() {
    let mut scenario = traffic_scenario();
    let mut engine = Engine::build(scenario.clone(), "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let undilated = engine.run(&mut recorder).expect("runs");
    assert!(undilated.frames_transmitted > 0);
    assert_eq!(undilated.suppressed_frames, 0);

    // The whole run is a window, so no frame may be generated at all.
    scenario.time.time_dilation = vec![v2xw_engine::scenario::DilationWindow {
        from_s: 0.0,
        to_s: scenario.time.duration_s,
    }];
    let mut engine = Engine::build(scenario, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let dilated = engine.run(&mut recorder).expect("runs");

    assert_eq!(
        dilated.frames_transmitted, 0,
        "a frame went on the air inside a time-dilation window"
    );
    assert!(
        dilated.suppressed_frames > 0,
        "frames were suppressed without being counted, so a metric reading zero inside \
         the window is indistinguishable from a quiet channel"
    );
    assert_eq!(recorder.count_on("phy.rx"), 0);
    // Mobility still runs inside a window: that is the whole point of dilating only the
    // radio (02-architecture.md §5.4).
    assert_eq!(dilated.mobility_steps, undilated.mobility_steps);
    assert!(recorder.count_on("gt.kinematics") > 0);

    // And the manifest records the window, so a reader of the outputs can tell.
    let engine = Engine::build(
        {
            let mut s = traffic_scenario();
            s.time.time_dilation = vec![v2xw_engine::scenario::DilationWindow {
                from_s: 1.0,
                to_s: 2.0,
            }];
            s
        },
        "",
    )
    .expect("builds");
    assert_eq!(engine.manifest().time_dilation_windows.len(), 1);
    assert!(engine.manifest().is_dilated(1_500_000_000));
    assert!(!engine.manifest().is_dilated(2_500_000_000));
}

/// The published extrapolation rule is the one in the contract, and the engine applies it
/// in exactly one place (02-architecture.md §5.2, invariant I-M4).
#[test]
fn positions_between_mobility_steps_come_from_the_published_rule() {
    let mut engine = Engine::build(traffic_scenario(), "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    engine.run(&mut recorder).expect("runs");

    // After the run every actor has despawned or is still held; take whichever remains.
    let sample = recorder
        .records()
        .iter()
        .rev()
        .find(|(_, r)| r.channel == "gt.kinematics")
        .expect("a published state");
    let view: v2xw_metrics::channels::GtKinematicsView =
        v2xw_metrics::channels::decode(&sample.1).expect("decodes");

    let at = sample.0;
    if let Some(p0) = engine.position_at(view.actor, at) {
        let dt_ns = 50_000_000u64;
        let p1 = engine
            .position_at(view.actor, at + dt_ns)
            .expect("extrapolates");
        // pos(t') = pos(t) + vel(t)·(t' − t): the displacement is linear in dt, so half
        // the interval is exactly half the displacement.
        let half = engine
            .position_at(view.actor, at + dt_ns / 2)
            .expect("extrapolates");
        let full_dx = p1.x - p0.x;
        let half_dx = half.x - p0.x;
        assert!(
            (full_dx - 2.0 * half_dx).abs() < 1e-9,
            "the extrapolation is not constant-velocity: {full_dx} vs 2 x {half_dx}"
        );
    }
}

/// The reception phase's results are merged in `NodeId` order, whatever order the parallel
/// map produced them in (02-architecture.md §6.4).
///
/// Checked on the record stream rather than inside the phase, because the record stream is
/// what a consumer batching by key actually sees. `phy.rx` records that share a `t_start`
/// belong to one frame, and within a frame they must ascend by receiver.
#[test]
fn reception_outcomes_reach_the_recorder_in_receiver_order() {
    let mut engine = Engine::build(traffic_scenario(), "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    assert!(report.reception_attempts > 0);

    let mut per_frame: std::collections::BTreeMap<(u64, u64), Vec<u32>> =
        std::collections::BTreeMap::new();
    for (_, rec) in recorder.records() {
        if rec.channel != "phy.rx" {
            continue;
        }
        let v: v2xw_metrics::channels::PhyRxView =
            v2xw_metrics::channels::decode(rec).expect("decodes");
        per_frame
            .entry((v.t_start, v.msg.unwrap_or(0)))
            .or_default()
            .push(v.rx.index());
    }
    let multi = per_frame.values().filter(|v| v.len() > 1).count();
    assert!(
        multi > 0,
        "no frame had more than one receiver, so receiver ordering cannot be observed"
    );
    for (frame, receivers) in &per_frame {
        let mut sorted = receivers.clone();
        sorted.sort_unstable();
        assert_eq!(
            receivers, &sorted,
            "frame {frame:?} reached the recorder out of receiver order"
        );
    }
}
