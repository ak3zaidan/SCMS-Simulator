//! The main loop end to end: event order, determinism, the manifest, and the phases.

use std::path::{Path, PathBuf};

use v2xw_core::event::{EventClass, EventKey, Scheduler};
use v2xw_engine::event::{Event, NodeTask, Observe};
use v2xw_engine::{Engine, MemoryRecorder, Scenario};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

fn traffic_scenario() -> Scenario {
    Scenario::load(scenarios().join("grid-traffic.yaml")).expect("loads")
}

/// A run over the traffic scenario, returning the recorder and the report.
fn run_once(
    scenario: Scenario,
    build_utc: &str,
) -> (MemoryRecorder, v2xw_engine::RunReport, String) {
    let mut engine = Engine::build(scenario, build_utc).expect("builds");
    let manifest = engine.manifest().to_json_pretty().expect("manifest json");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (recorder, report, manifest)
}

/// The run reaches its horizon, moves vehicles, equips them, and puts frames on the air.
///
/// Without this the determinism test below would pass over an empty run, which is the
/// classic check that cannot fail: two identical nothings are identical.
#[test]
fn the_engine_runs_a_scenario_end_to_end() {
    let (recorder, report, _) = run_once(traffic_scenario(), "");
    assert_eq!(report.end_ns, 10_000_000_000, "the run reached its horizon");
    // 101, not 100: the run covers the closed interval [0, duration], so there is a step
    // at t = 0 and a step at t = 10 s. The horizon instant is observed rather than cut
    // off, which is what lets an `Observe` at the horizon see a settled world.
    assert_eq!(
        report.mobility_steps, 101,
        "10 s at 100 ms, endpoints included"
    );
    assert!(report.actors_spawned > 0, "no vehicle ever spawned");
    assert!(report.nodes_created > 0, "no vehicle was ever equipped");
    assert!(report.frames_transmitted > 0, "no frame went on the air");
    assert!(
        report.reception_attempts > 0,
        "no reception was ever evaluated"
    );
    assert!(report.frames_received > 0, "no frame was ever received");
    assert!(recorder.count_on("gt.kinematics") > 0);
    assert!(recorder.count_on("node.tx") > 0);
    assert!(recorder.count_on("phy.rx") > 0);
    assert_eq!(report.records_refused, 0);
}

/// Acceptance criterion: two runs of one scenario produce identical manifests and
/// identical outputs.
///
/// The two runs differ in the one field that is allowed to differ — the caller-supplied
/// build timestamp, which 02-architecture.md §6.1 excludes from every digest — so the test
/// also shows that the exclusion works rather than assuming it.
#[test]
fn two_runs_of_one_scenario_produce_identical_manifests_and_outputs() {
    let (a_records, a_report, a_manifest) = run_once(traffic_scenario(), "2026-09-22T00:00:00Z");
    let (b_records, b_report, b_manifest) = run_once(traffic_scenario(), "2031-01-01T12:00:00Z");

    assert_eq!(a_report, b_report, "the run reports differ");
    assert_eq!(
        a_records.digest_hex(),
        b_records.digest_hex(),
        "the record streams differ"
    );
    assert_eq!(
        a_records.records().len(),
        b_records.records().len(),
        "different record counts"
    );

    let a: serde_json::Value = serde_json::from_str(&a_manifest).expect("json");
    let b: serde_json::Value = serde_json::from_str(&b_manifest).expect("json");
    assert_ne!(a["build_utc"], b["build_utc"], "the fixture is not honest");
    let mut a_stripped = a.clone();
    let mut b_stripped = b.clone();
    a_stripped["build_utc"] = serde_json::Value::Null;
    b_stripped["build_utc"] = serde_json::Value::Null;
    assert_eq!(
        a_stripped, b_stripped,
        "the manifests differ in something other than the build timestamp"
    );
}

/// The injected fault for the determinism check: change one bit of the seed and the two
/// runs must stop agreeing. A digest comparison that passes for every scenario is not
/// evidence of determinism, it is evidence of a constant.
#[test]
fn a_different_seed_produces_a_different_run() {
    let base = traffic_scenario();
    let mut perturbed = base.clone();
    perturbed.seed ^= 1;

    let (a, _, a_manifest) = run_once(base, "");
    let (b, _, b_manifest) = run_once(perturbed, "");
    assert_ne!(
        a.digest_hex(),
        b.digest_hex(),
        "flipping one bit of the master seed changed nothing, so the run is not reading it"
    );
    assert_ne!(
        a_manifest, b_manifest,
        "the manifest does not record the seed it ran with"
    );
}

/// Acceptance criterion: the manifest records everything Phase 1 criterion 6 lists.
#[test]
fn the_manifest_records_everything_the_criterion_lists() {
    let scenario = traffic_scenario();
    let expected_scenario_hash = scenario.content_hash().expect("hash");
    let expected_seed = scenario.seed;

    let engine = Engine::build(scenario, "2026-09-22T00:00:00Z").expect("builds");
    let m = engine.manifest();

    // engine version and hash
    assert_eq!(m.engine_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(m.build_hash.len(), 64);
    // the scenario hash, over canonical JSON
    assert_eq!(m.scenario_hash, expected_scenario_hash);
    // the master seed
    assert_eq!(m.master_seed, expected_seed);
    // the world hash
    assert_eq!(m.world_hash.len(), 64);
    assert_eq!(
        m.world_hash,
        v2xw_core::hash::hex_encode(&engine.world().provenance.content_hash)
    );
    // the compiler version, and the platform
    let rustc = m.crate_versions.get("rustc").expect("no compiler recorded");
    assert!(rustc.starts_with("rustc "), "compiler version is {rustc}");
    assert!(!m.platform.is_empty() && m.platform != "unknown");
    // every plug-in with its hash, in id order, and every model card version
    assert!(m.plugins.len() >= 13, "only {} plug-ins", m.plugins.len());
    for p in &m.plugins {
        assert!(!p.id.is_empty());
        assert!(!p.version.is_empty());
        assert_eq!(p.content_hash.len(), 64, "{} has no content hash", p.id);
    }
    let ids: Vec<&String> = m.plugins.iter().map(|p| &p.id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "the plug-in list is not in id order");
    assert_eq!(m.model_cards.len(), engine.registry().len());
    // the crypto mode and the time-dilation windows
    assert_eq!(m.crypto_mode, v2xw_core::manifest::CryptoMode::Modeled);
    assert!(m.time_dilation_windows.is_empty());
    // and it survives its own round trip
    let json = m.to_json_pretty().expect("json");
    let back = v2xw_core::manifest::Manifest::from_json(&json).expect("parses");
    assert_eq!(&back, m);
}

/// The injected fault for the manifest check: register one fewer model and the plug-in
/// list shrinks, so the assertion above is reading the registry rather than a constant.
#[test]
fn the_manifest_plugin_list_tracks_the_registry() {
    let scenario = traffic_scenario();
    let world = v2xw_engine::wiring::build_world(&scenario).expect("world");

    let mut full = v2xw_core::registry::Registry::new();
    v2xw_engine::wiring::register_all(&mut full).expect("registers");
    let with_all = v2xw_engine::manifest::assemble(&scenario, &world, &full, "").expect("manifest");

    let empty = v2xw_core::registry::Registry::new();
    let with_none =
        v2xw_engine::manifest::assemble(&scenario, &world, &empty, "").expect("manifest");

    assert!(!with_all.plugins.is_empty());
    assert!(with_none.plugins.is_empty());
    assert!(with_none.model_cards.is_empty());
}

/// The dispatch order is the documented total order: ascending time, then ascending
/// priority, then ascending sequence.
#[test]
fn the_dispatch_order_is_the_documented_total_order() {
    let mut scheduler: Scheduler<Event> = Scheduler::new();
    // Scheduled in the *reverse* of the order they must come out in, at one instant, so
    // that a scheduler ordering by insertion would fail.
    for event in Event::one_of_each().into_iter().rev() {
        scheduler.schedule_reentrant(1_000, event.class(), event);
    }
    // And a second copy of the lowest-priority class, to pin the `seq` tie-break.
    scheduler.schedule_reentrant(
        1_000,
        EventClass::Observe,
        Event::Observe {
            what: Observe::Keyframe,
        },
    );

    let mut keys: Vec<EventKey> = Vec::new();
    let mut classes: Vec<EventClass> = Vec::new();
    while let Some((key, event)) = scheduler.pop() {
        keys.push(key);
        classes.push(event.class());
    }
    assert_eq!(classes.len(), EventClass::ALL.len() + 1);
    assert_eq!(&classes[..EventClass::ALL.len()], &EventClass::ALL[..]);
    for pair in keys.windows(2) {
        assert!(
            pair[0] < pair[1],
            "the dispatch order is not ascending: {:?} then {:?}",
            pair[0],
            pair[1]
        );
        assert_eq!(pair[0].time, pair[1].time);
    }
    // The two `Observe` events tie on time and priority, so `seq` breaks the tie in
    // scheduling order.
    let last_two = &keys[keys.len() - 2..];
    assert_eq!(last_two[0].priority, last_two[1].priority);
    assert!(last_two[0].seq < last_two[1].seq);
}

/// A zero-delay schedule at an earlier priority than the instant being dispatched cannot
/// violate monotonicity: the kernel refuses it.
///
/// The check that matters is the *pair*: the refused case panics, and the permitted case
/// — the same zero-delay schedule at a *later* priority — does not. Without the second
/// half this would pass for a scheduler that panicked on everything.
#[test]
fn a_zero_delay_back_priority_schedule_cannot_violate_monotonicity() {
    let mut scheduler: Scheduler<Event> = Scheduler::new();
    scheduler.schedule(1_000, EventClass::NodeTask, Event::NodePhase);
    let (key, _) = scheduler.pop().expect("one event");
    assert_eq!(key.priority, EventClass::NodeTask.priority());

    // Forward at the same instant: legal, and the ordinary way a node phase schedules a
    // delivery.
    scheduler.schedule(
        1_000,
        EventClass::Observe,
        Event::Observe {
            what: Observe::MetricFlush,
        },
    );

    // Backward at the same instant: refused.
    let refused = std::panic::catch_unwind(move || {
        let mut s: Scheduler<Event> = Scheduler::new();
        s.schedule(1_000, EventClass::NodeTask, Event::NodePhase);
        let _ = s.pop();
        s.schedule(1_000, EventClass::MobilityStep, Event::MobilityStep);
    });
    assert!(
        refused.is_err(),
        "a zero-delay MobilityStep injected from inside a NodeTask was accepted, so the \
         key sequence can go backwards and every consumer that batches by key is wrong"
    );
}

/// The engine's own `schedule` cross-checks the class against the payload, because a
/// payload dispatched at the wrong priority reorders an instant silently.
#[test]
fn scheduling_a_payload_at_the_wrong_class_is_refused() {
    let scenario = traffic_scenario();
    let mut engine = Engine::build(scenario, "").expect("builds");
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut recorder = MemoryRecorder::new();
        engine.with_ctx(&mut recorder, |ctx| {
            v2xw_core::ctx::Ctx::schedule(
                ctx,
                5_000,
                EventClass::Control,
                Event::NodeTask {
                    node: v2xw_core::NodeId::new(0),
                    task: NodeTask::Step,
                },
            );
        });
    }));
    assert!(
        refused.is_err(),
        "a NodeTask payload was accepted at Control priority"
    );
}

/// Records refused by the visibility rule are counted, and the rule fires on a real
/// violation: a ground-truth record on a `node.` channel.
#[test]
fn a_ground_truth_record_is_refused_on_a_node_channel() {
    #[derive(serde::Serialize)]
    struct Leak {
        actor: u32,
    }
    impl v2xw_core::ctx::Record for Leak {
        const CHANNEL: &'static str = "node.telemetry";
        const VISIBILITY: v2xw_core::ctx::Visibility = v2xw_core::ctx::Visibility::Gt;
    }
    #[derive(serde::Serialize)]
    struct Fine {
        node: u32,
    }
    impl v2xw_core::ctx::Record for Fine {
        const CHANNEL: &'static str = "node.telemetry";
        const VISIBILITY: v2xw_core::ctx::Visibility = v2xw_core::ctx::Visibility::Node;
    }

    let mut engine = Engine::build(traffic_scenario(), "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let (refused_gt, refused_node) = engine.with_ctx(&mut recorder, |ctx| {
        use v2xw_core::ctx::CtxExt;
        ctx.emit(Leak { actor: 1 });
        let after_leak = ctx.refused();
        ctx.emit(Fine { node: 1 });
        (after_leak, ctx.refused())
    });
    assert_eq!(refused_gt, 1, "a GT record reached a node channel");
    assert_eq!(
        refused_node, 1,
        "a legitimate node record was refused too, so the rule is not discriminating"
    );
    assert_eq!(recorder.count_on("node.telemetry"), 1);
}

/// The metric path is composed into the run: providers subscribe to the recorded stream,
/// the `Observe` flush samples them, and the samples land on `metric.sample`.
///
/// Asserting on the *values* rather than on the channel's existence: a provider that
/// subscribed to nothing would still flush an empty window forever, which a count-of-
/// records check would happily accept.
#[test]
fn metric_providers_are_fed_from_the_recorded_stream_and_flushed_at_observe() {
    let (recorder, report, _) = run_once(traffic_scenario(), "");
    assert!(report.frames_transmitted > 0);

    let samples: Vec<v2xw_metrics::MetricSample> = recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == "metric.sample")
        .map(|(_, r)| serde_json::from_slice(&r.json).expect("a metric sample"))
        .collect();
    assert!(!samples.is_empty(), "no metric was ever sampled");

    // The assertion that matters is on a *value* that only the recorded stream can
    // produce. Asserting that several metric names appeared would pass even with the
    // providers disconnected, because a provider flushes every definition it has every
    // window, insufficient or not — which is precisely the check that cannot fail.
    // `airtime_per_node` is airtime in ms/s, computed from `node.tx`, so a positive value
    // is only reachable if transmissions reached the provider.
    let airtime_measured = samples.iter().any(|s| {
        s.metric == "airtime_per_node"
            && matches!(
                &s.value,
                v2xw_metrics::SampleValue::Scalar(
                    v2xw_metrics::stats::Estimate::Value { point, .. }
                ) if *point > 0.0
            )
    });
    assert!(
        airtime_measured,
        "airtime_per_node was reported but never measured across {} samples, so no \
         node.tx record reached the providers",
        samples.len()
    );
    // `pdr` counts reception attempts, so its *trials* are the evidence that `phy.rx`
    // reached the provider — whether or not the window had enough of them for the
    // definition's confidence requirement.
    let pdr_trials: u64 = samples
        .iter()
        .filter(|s| s.metric == "pdr")
        .map(|s| match &s.value {
            v2xw_metrics::SampleValue::Ratio(v2xw_metrics::stats::RatioEstimate::Proportion {
                trials,
                ..
            })
            | v2xw_metrics::SampleValue::Ratio(
                v2xw_metrics::stats::RatioEstimate::Insufficient { trials, .. },
            ) => *trials,
            _ => 0,
        })
        .sum();
    assert!(
        pdr_trials > 0,
        "pdr was reported with no trials at all, so no phy.rx record reached the providers"
    );
    // The flush happens at `Observe`, priority 9, so every sample's instant is one where
    // everything else at that instant has already run.
    for s in &samples {
        assert!(
            s.t > 0 && s.t <= report.end_ns,
            "sample outside the run: {s:?}"
        );
    }
}

/// The counterpart: a scenario that asks for no metric installs no provider and writes no
/// samples, so the test above is reading the scenario rather than a constant.
#[test]
fn a_scenario_that_asks_for_no_metric_writes_no_samples() {
    let mut scenario = traffic_scenario();
    scenario.metrics.clear();
    let (recorder, _, _) = run_once(scenario, "");
    assert_eq!(recorder.count_on("metric.sample"), 0);
}
