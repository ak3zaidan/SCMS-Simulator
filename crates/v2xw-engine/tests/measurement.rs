//! The measurement layer over a real run, not a fixture.
//!
//! `v2xw-metrics` proves each consistency rule against synthetic streams and shows each
//! can fail. This file runs the engine and holds the *engine's* output to the same rules,
//! which is the only place a producer defect — a stamp taken on the wrong clock, an
//! attempt that never gets a fate, a header counted twice — can show up:
//!
//! * every `phy.rx` attempt has exactly one `node.rx` fate (M-RX1), so delivered + lost +
//!   in flight = attempts;
//! * every delivered message's stages tile its generation-to-delivery interval (M-LAT1);
//! * every frame's layers add up to its octets on the air (M-BYTE1);
//! * the per-bucket byte rates sum to the total (M-BYTE2) and the stage shares to one
//!   (M-SHARE);
//! * no metric reports a value its definition calls impossible (M-RANGE);
//! * and the numbers are the right order of magnitude for 802.11p, which no invariant can
//!   say.

use std::path::{Path, PathBuf};

use v2xw_core::registry::Registry;
use v2xw_engine::{Engine, MemoryRecorder, RunReport, Scenario};
use v2xw_metrics::channels::{NodeRxView, NodeTxView, RxFate, decode};
use v2xw_metrics::{EventLedger, MetricDef, MetricSample};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

/// The ten-vehicle rung of the scaling ladder on the real Manhattan extract, with every
/// metric provider installed and three simulated seconds so several metric windows close.
fn scenario() -> Scenario {
    let mut s = Scenario::load(scenarios().join("scale/10.yaml")).expect("loads");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    if let v2xw_world::WorldSourceSpec::OsmXml { path, .. } = &mut s.world.source
        && Path::new(path).is_relative()
    {
        *path = root.join(&*path).to_string_lossy().into_owned();
    }
    s.metrics = vec!["all".to_string()];
    s.time.duration_s = 3.0;
    // Open air. Since the radio track made buildings obstruct, ten vehicles scattered over
    // Midtown deliver 23 of 1,134 receptions in three seconds (measured), too few for any
    // one-second window to reach the 30 samples a latency percentile needs, so the
    // headline latency is "insufficient" in every window. With obstruction off the same
    // run delivers 1,059 and every window has an estimate. This file checks the
    // measurement layer, which needs deliveries to measure; buildings are the radio
    // track's tests' business (radio_access.rs).
    s.world.buildings.enabled = false;
    s
}

fn run() -> (RunReport, MemoryRecorder, Vec<MetricDef>) {
    let s = scenario();
    let catalog = v2xw_engine::wiring::build_metrics(&s, &mut Registry::new())
        .expect("providers build")
        .catalog();
    let mut engine = Engine::build(s, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (report, recorder, catalog)
}

fn samples(recorder: &MemoryRecorder) -> Vec<MetricSample> {
    recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == "metric.sample")
        .map(|(_, r)| serde_json::from_slice(&r.json).expect("a metric sample decodes"))
        .collect()
}

#[test]
fn a_real_run_satisfies_every_measurement_invariant() {
    let (report, recorder, catalog) = run();
    let mut ledger = EventLedger::new();
    for (_, r) in recorder.records() {
        ledger.ingest(r);
    }
    assert_eq!(
        ledger.decode_failure_total(),
        0,
        "records that do not decode as their own channel's view: {:?}",
        ledger.decode_failures
    );
    let samples = samples(&recorder);
    let checks = v2xw_metrics::check_all(&ledger, &samples);
    checks.assert_all().unwrap_or_else(|e| panic!("{e}"));
    for id in ["M-RX1", "M-LAT1", "M-PRR", "M-BYTE1", "M-BYTE2", "M-SHARE"] {
        let o = checks
            .outcomes
            .iter()
            .find(|o| o.invariant == id)
            .unwrap_or_else(|| panic!("{id} did not run"));
        assert!(o.skipped.is_none(), "{id} was skipped: {:?}", o.skipped);
        assert!(o.checked > 0, "{id} examined nothing");
    }
    let ranges = v2xw_metrics::invariants::check_metric_ranges(&samples, &catalog);
    assert!(ranges.held(), "{:?}", ranges.violations);
    assert!(
        ranges.checked > 100,
        "only {} values range-checked",
        ranges.checked
    );

    // One fate per attempt, and the counts are the run report's own.
    assert_eq!(ledger.node_rx.len() as u64, report.reception_attempts);
    assert_eq!(ledger.rx.len() as u64, report.reception_attempts);
    let delivered = ledger
        .node_rx
        .iter()
        .filter(|v| v.outcome == RxFate::Delivered)
        .count();
    assert!(delivered > 0, "no message reached an application");
}

/// A message reaches a receiver's applications when its signature check finishes — not
/// when the check starts, and not at the receiver's next periodic step. Every delivered
/// attempt is resolved (`t`, the instant the node handed it over) at its own
/// `t_delivered`, the instant its check finished, to within a microsecond of clock
/// arithmetic; before 2026-09-24 it was handed over when the check started, up to one
/// verification service time before `t_delivered`.
#[test]
fn a_message_is_handed_to_the_applications_when_its_verification_finishes() {
    let (_, recorder, _) = run();
    let mut ledger = EventLedger::new();
    for (_, r) in recorder.records() {
        ledger.ingest(r);
    }
    let mut checked = 0;
    let mut early = 0;
    let mut late = 0;
    for v in &ledger.node_rx {
        if v.outcome != RxFate::Delivered {
            continue;
        }
        let d = v.t_delivered.expect("a delivery carries its instant");
        checked += 1;
        if v.t + 1_000 < d {
            early += 1;
        }
        if v.t > d + 1_000 {
            late += 1;
        }
    }
    assert!(checked > 100, "only {checked} deliveries to check");
    assert_eq!(early, 0, "{early} of {checked} handed over before their check finished");
    assert_eq!(
        late, 0,
        "{late} of {checked} handed over after their check had finished (waiting for a step)"
    );
}

#[test]
fn the_decomposed_latency_is_the_right_size_for_802_11p() {
    let (_, recorder, _) = run();
    let mut e2e: Vec<u64> = Vec::new();
    let mut checked = 0;
    for (_, r) in recorder.records() {
        if r.channel != "node.rx" {
            continue;
        }
        let v: NodeRxView = decode(r).expect("decodes");
        if v.outcome != RxFate::Delivered {
            continue;
        }
        let trace = v.latency_trace().expect("a delivered message decomposes");
        let st = trace.stage_ns();
        // Air time of a 150-400 octet PSDU at 6 Mbit/s: 40 µs of preamble and SIGNAL plus
        // 8 µs per 24 octets — between 250 µs and 600 µs.
        assert!(
            (250_000..=600_000).contains(&st["airtime"]),
            "air time {} ns",
            st["airtime"]
        );
        // 1 km at the speed of light is 3.34 µs; the candidate range is 1 km.
        assert!(
            st["propagation"] <= 3_400,
            "propagation {} ns",
            st["propagation"]
        );
        // AC_VO's AIFS is 58 µs, unless the frame was granted in less.
        assert!(st["mac_aifs"] <= 58_000);
        e2e.push(trace.total_ns().expect("a total"));
        checked += 1;
    }
    assert!(checked > 0);
    e2e.sort_unstable();
    let median = e2e[e2e.len() / 2];
    // Signing (ms on an HSM), channel access, air time and verification: well inside the
    // 100 ms cooperative-awareness budget, and never below the air time alone.
    assert!(
        (250_000..100_000_000).contains(&median),
        "median end-to-end latency {median} ns"
    );
}

#[test]
fn every_frame_on_the_air_carries_its_headers() {
    let (_, recorder, _) = run();
    let mut frames = 0;
    for (_, r) in recorder.records() {
        if r.channel != "node.tx" {
            continue;
        }
        let v: NodeTxView = decode(r).expect("decodes");
        assert_eq!(v.layers_consistent(), Some(true), "{v:?}");
        // WSMP for a BSM: 5 octets; LLC/SNAP 8, the QoS Data MAC header 26 and the FCS 4.
        assert_eq!(v.net_header_bytes, Some(5));
        assert_eq!(v.link_bytes, Some(38));
        frames += 1;
    }
    assert!(frames > 0);
}

/// `messages.generator` reaches the air: the same fleet with the BSM interval doubled to
/// 200 ms puts about half as many frames on the air, and the European stack's 44-octet
/// GeoNetworking/BTP header replaces WSMP's 5 on every frame.
#[test]
fn the_generator_and_the_network_layer_change_what_goes_on_the_air() {
    let frames = |s: Scenario| {
        let mut engine = Engine::build(s, "").expect("builds");
        let mut recorder = MemoryRecorder::new();
        let report = engine.run(&mut recorder).expect("runs");
        (report.frames_transmitted, recorder)
    };
    let (baseline, _) = frames(scenario());
    let mut slow = scenario();
    let mut g = v2xw_engine::scenario::ModelChoice::new(v2xw_msg::generator::BSM_GENERATOR_ID);
    g.params = serde_json::json!({"nominal_itt_ms": 200.0});
    slow.messages.generator = Some(g);
    let (halved, _) = frames(slow);
    let ratio = halved as f64 / baseline as f64;
    assert!(
        (0.35..=0.65).contains(&ratio),
        "{halved} frames at 5 Hz against {baseline} at 10 Hz (ratio {ratio})"
    );

    let mut eu = scenario();
    eu.net.layer = "gn-btp".to_string();
    let (_, recorder) = frames(eu);
    let mut checked = 0;
    for (_, r) in recorder.records() {
        if r.channel == "node.tx" {
            let v: NodeTxView = decode(r).expect("decodes");
            assert_eq!(v.net_header_bytes, Some(44), "GN SHB 40 + BTP-B 4");
            assert_eq!(v.layers_consistent(), Some(true));
            checked += 1;
        }
    }
    assert!(checked > 0);
}

#[test]
fn every_metric_of_the_communication_families_is_sampled() {
    let (_, recorder, _) = run();
    let samples = samples(&recorder);
    for name in [
        "pdr",
        "pdr_all_pairs",
        "cbr",
        "e2e_latency",
        "latency_stage",
        "latency_stage_share",
        "aoi",
        "aoi_peak",
        "nar",
        "delivery_ratio",
        "channel_occupancy",
        "channel_load",
        "offered_load",
        "carried_load",
        "collision_rate",
        "half_duplex_rate",
        "mac_queue_depth",
        "mac_drops",
        "mac_access_delay",
        "security_overhead",
        "net_header_overhead",
        "link_overhead",
        "cert_bytes_share",
        "air_bytes_per_payload_byte",
        "bytes_per_vehicle_hour",
        "bytes_total",
        "airtime_per_node",
    ] {
        assert!(
            samples.iter().any(|s| s.metric == name),
            "{name} was never sampled"
        );
    }
    // And the headline numbers are estimates, not refusals, where a ten-vehicle run has
    // the samples for one.
    let point = |name: &str| {
        samples
            .iter()
            .filter(|s| s.metric == name && s.dims.is_empty())
            .filter_map(|s| s.value.point())
            .next_back()
    };
    for name in ["pdr", "e2e_latency", "channel_load", "security_overhead"] {
        assert!(
            point(name).is_some(),
            "{name} has no estimate in any window"
        );
    }
}
