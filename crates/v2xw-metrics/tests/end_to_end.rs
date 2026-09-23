//! A run end to end: register every provider, feed a synthetic recording, flush, check the
//! invariants, write the Arrow table and summarise for the manifest.
//!
//! 08-measurement-and-data.md §8: "Metric providers have unit tests with synthetic event
//! streams and known answers". The per-module tests do that one metric at a time; this is
//! the same discipline applied to the whole layer at once, plus the four properties that are
//! only visible from the outside:
//!
//! * a run's digest is a function of the run and not of the order independent events arrived
//!   in;
//! * the runtime diagnostics cannot reach that digest;
//! * the invariant checks pass on a well-formed run and name the culprit on a broken one;
//! * every float in the Arrow table sits on its declared grid.

use arrow::array::{Array, Float64Array};
use serde_json::json;
use v2xw_core::ctx::{OwnedRecord, Visibility};
use v2xw_core::registry::Registry;
use v2xw_core::time::Duration;
use v2xw_metrics::arrow_out::{samples_batch, to_ipc};
use v2xw_metrics::channels::allowed_visibilities;
use v2xw_metrics::invariants::check_all;
use v2xw_metrics::{
    DigestSet, EventLedger, MetricSample, ProviderSet, Quantum, RunSummary, metric_digest,
};

/// Builds a record, tagging it with the visibility 03-interfaces.md §14 declares for its
/// channel — so the fixture itself cannot accidentally break invariant I-T2 and make the
/// test about the fixture instead of about the code.
fn rec(channel: &'static str, body: serde_json::Value) -> OwnedRecord {
    let visibility = allowed_visibilities(channel)
        .and_then(|v| v.first().copied())
        .unwrap_or(Visibility::Node);
    OwnedRecord {
        channel,
        visibility,
        json: serde_json::to_vec(&body).unwrap(),
    }
}

/// A one-second recording of three transmitters, their receptions, their verifications,
/// two nodes' channel measurements, a byte attribution, a revocation reaching `published`,
/// an attack action with its transmission, and a frame of kinematics.
///
/// Every id is distinct and every record is internally consistent, so a well-formed run is
/// what the invariant checks should find.
fn recording() -> Vec<OwnedRecord> {
    let mut out = Vec::new();

    // Three transmitters, one frame each, all with airtime and a payload/envelope split.
    for node in 1..=3u32 {
        out.push(rec(
            "node.tx",
            json!({
                "t": u64::from(node) * 1_000_000,
                "node": node,
                "msg": node,
                "msg_type": "bsm",
                "bytes_on_wire": 400,
                "payload_bytes": 300,
                "envelope_bytes": 100,
                "airtime_us": 600,
                "channel": 180,
                "signer": if node == 1 { "certificate" } else { "digest" },
                "t_generated": u64::from(node) * 1_000_000,
                // The per-layer split: this fixture's frames carry no headers, which is
                // unrealistic and consistent — the layers add up to the octets on the air.
                "spdu_bytes": 400,
                "net_header_bytes": 0,
                "link_bytes": 0
            }),
        ));
    }

    // Forty candidate receptions in the first distance bin, thirty-two of them delivered,
    // so the bin has enough trials for a point estimate at the default threshold of thirty.
    // Each attempt is its own message (a broadcast every millisecond from node 1), and each
    // is followed to its fate on `node.rx` with the stamps of its whole journey.
    for i in 0..40u64 {
        let delivered = i % 5 != 0;
        let msg = 1_000 + i;
        let t0 = 9_000_000 + i * 1_000_000;
        let t_end = 10_500_000 + i * 1_000_000;
        out.push(rec(
            "phy.rx",
            json!({
                "t_start": 10_000_000 + i * 1_000_000,
                "t_end": t_end,
                "tx": 1,
                "rx": 2 + (i % 3) as u32,
                "msg": msg,
                "dist_m": 12.5,
                "outcome": if delivered { "ok" } else { "lost" },
                "cause": if delivered { serde_json::Value::Null } else { json!("collision") },
                "payload_bytes": if delivered { json!(300) } else { serde_json::Value::Null }
            }),
        ));
        let mut fate = json!({
            "t": t_end + 700_000,
            "rx": 2 + (i % 3) as u32,
            "tx": 1,
            "msg": msg,
            "msg_type": "bsm",
            "outcome": if delivered { "delivered" } else { "lost" },
            "cause": if delivered { serde_json::Value::Null } else { json!("collision") },
            "verification": if delivered { json!("verified") } else { serde_json::Value::Null },
            "rssi_dbm": -60.0,
            "dist_m": 12.5,
            "bytes_on_wire": 400,
            "airtime_us": 500,
            "payload_bytes": 300,
            "t_generated": t0,
            "t_sign_start": t0 + 100_000,
            "t_signed": t0 + 400_000,
            "mac_aifs_ns": 58_000,
            "mac_backoff_ns": 39_000,
            "t_tx_start": 10_000_000 + i * 1_000_000,
            "t_tx_end": t_end,
            "t_arrival": t_end + 42
        });
        if delivered {
            fate["t_rx_done"] = json!(t_end + 42);
            fate["t_verify_start"] = json!(t_end + 200_000);
            fate["t_verify_done"] = json!(t_end + 700_000);
            fate["t_delivered"] = json!(t_end + 700_000);
        }
        out.push(rec("node.rx", fate));
    }

    // A backend flow's trace: a misbehaviour report relayed over an RSU's backhaul.
    out.push(rec(
        "msg.latency",
        json!({"flow": "mbr", "msg_type": "mbr", "msg": 7, "t_origin": 300_000_000,
        "spans": [
            {"stage": "sign", "hop": 0, "start": 300_000_000, "end": 300_900_000},
            {"stage": "airtime", "hop": 0, "start": 300_900_000, "end": 301_400_000},
            {"stage": "backhaul", "hop": 1, "start": 301_400_000, "end": 321_400_000}
        ]}),
    ));

    // Verifications: thirty-two valid (matching the deliveries) and one skipped.
    for i in 0..32u64 {
        out.push(rec(
            "node.verify",
            json!({
                "t_enqueue": 11_000_000 + i * 1_000_000,
                "t_start": 11_200_000 + i * 1_000_000,
                "t_done": 11_500_000 + i * 1_000_000,
                "node": 2,
                "primitive": "ecdsa-p256",
                "cost_us": 1_200,
                "outcome": "valid",
                "msg": 1,
                "queue_depth": i % 4
            }),
        ));
    }
    out.push(rec(
        "node.verify",
        json!({"t_enqueue": 50_000_000, "node": 3, "outcome": "skipped"}),
    ));

    // Channel measurements at two nodes: busy time covers each node's own transmissions.
    for node in 1..=3u32 {
        out.push(rec(
            "mac.cbr",
            json!({"t": 100_000_000, "node": node, "channel": 180,
                   "cbr": 0.25, "busy_us": 25_000, "window_us": 100_000}),
        ));
    }

    // One backhaul attribution, with the id I-N1 needs.
    out.push(rec(
        "net.bytes",
        json!({"t": 200_000_000, "id": 1_000, "bucket": "backhaul", "bytes_on_wire": 2_048}),
    ));

    // A revocation that reaches `published`, with every stage I-P4 requires.
    for (stage, t) in [
        ("detect", 300_000_000u64),
        ("report_sent", 320_000_000),
        ("report_received", 400_000_000),
        ("decision", 500_000_000),
        ("issued", 600_000_000),
        ("published", 700_000_000),
    ] {
        out.push(rec(
            "proto.revocation",
            json!({"t": t, "stage": stage, "id": "rev-1", "entries": 3, "size_bytes": 512}),
        ));
    }

    // An attacker: one action on the air, with the transmission it produced.
    out.push(rec(
        "node.tx",
        json!({"t": 800_000_000, "node": 9, "msg": 99, "bytes_on_wire": 400,
               "payload_bytes": 300, "envelope_bytes": 100, "airtime_us": 600,
               "channel": 180, "signer": "digest"}),
    ));
    out.push(rec(
        "gt.attack.action",
        json!({"t": 800_000_000, "actor": 9, "attacker": "ghost",
               "action": "false-position", "msg": 99}),
    ));
    out.push(rec(
        "mac.cbr",
        json!({"t": 900_000_000, "node": 9, "channel": 180,
               "cbr": 0.25, "busy_us": 25_000, "window_us": 100_000}),
    ));

    // The authority reports and revokes the attacker.
    out.push(rec(
        "ma.report",
        json!({"t": 820_000_000, "reporter": 2, "subject": "p9", "detector": "art"}),
    ));
    out.push(rec(
        "ma.decision",
        json!({"t": 850_000_000, "subject": "p9", "decision": "revoke"}),
    ));

    // One frame of kinematics, in actor order, on a lane the run declares below.
    for (actor, pos, speed) in [(1u32, 0.0, 20.0), (2, 30.0, 10.0), (3, 75.0, 15.0)] {
        out.push(rec(
            "gt.kinematics",
            json!({"t": 900_000_000, "actor": actor, "x_m": pos, "y_m": 0.0,
                   "speed_mps": speed, "acc_mps2": 0.0, "lane": 7, "lane_pos_m": pos}),
        ));
    }
    out
}

/// Registers every provider, feeds `records`, and returns the flushed samples.
fn run(records: &[OwnedRecord]) -> Vec<MetricSample> {
    let mut registry = Registry::new();
    let mut set = ProviderSet::new();
    v2xw_metrics::register_all(&mut registry, &mut set, 0).expect("registration");
    for r in records {
        set.on_event(r);
    }
    set.flush(1_000_000_000)
}

/// The same run with every provider's insufficiency threshold lowered to one sample.
///
/// The default threshold is thirty ([`v2xw_metrics::stats::DEFAULT_MIN_SAMPLES`]), and this
/// fixture is deliberately small, so most of its metrics report insufficient — which is the
/// behaviour the default run below asserts. The hand-computed values still have to be
/// checked, so they are checked here, where the provider has been told that one sample is
/// enough. Lowering the threshold changes what is *reported*, never what is computed.
fn run_sensitive(records: &[OwnedRecord]) -> Vec<MetricSample> {
    let mut registry = Registry::new();
    let mut set = ProviderSet::new();
    set.register(
        &mut registry,
        Box::new(v2xw_metrics::comms::CommsProvider::new(0).with_min_samples(1)),
    )
    .unwrap();
    set.register(
        &mut registry,
        Box::new(v2xw_metrics::security::SecurityProvider::new(0).with_min_samples(1)),
    )
    .unwrap();
    set.register(
        &mut registry,
        Box::new(v2xw_metrics::detection::DetectionProvider::new().with_min_samples(1)),
    )
    .unwrap();
    set.register(
        &mut registry,
        Box::new(v2xw_metrics::safety::SafetyProvider::new(0).with_min_samples(1)),
    )
    .unwrap();
    for r in records {
        set.on_event(r);
    }
    set.flush(1_000_000_000)
}

/// A whole run's digest: the diagnostics partitioned out, because a run registers the
/// runtime provider and its samples are machine-dependent by construction.
fn digest(samples: Vec<MetricSample>) -> String {
    let (digested, removed) = DigestSet::partition(samples);
    assert_eq!(
        removed.len(),
        3,
        "the three diagnostics of 08-measurement §2"
    );
    digested.digest_hex().unwrap()
}

fn find<'a>(samples: &'a [MetricSample], key: &str) -> &'a MetricSample {
    samples.iter().find(|s| s.key() == key).unwrap_or_else(|| {
        panic!(
            "no sample with key {key}; have {:?}",
            samples.iter().map(MetricSample::key).collect::<Vec<_>>()
        )
    })
}

#[test]
fn every_provider_registers_through_the_core_registry_with_a_valid_card() {
    let mut registry = Registry::new();
    let mut set = ProviderSet::new();
    v2xw_metrics::register_all(&mut registry, &mut set, 0).unwrap();
    assert_eq!(
        set.len(),
        9,
        "communication, latency, awareness, load, overhead, security, detection, safety, \
         runtime"
    );
    assert_eq!(registry.len(), 9);
    // One catalogue, so one name per metric: two providers defining the same name would
    // put two different definitions behind one series in the page and the recording.
    let catalog = set.catalog();
    let mut names: Vec<&str> = catalog.iter().map(|d| d.name.as_str()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "a metric name is defined twice");
    for (_, registered) in registry.iter() {
        registered.card.validate().unwrap();
        registered.card.check_api_version().unwrap();
        assert_eq!(registered.card.family, v2xw_core::card::Family::Metric);
        assert!(
            !registered.card.sources.is_empty(),
            "{} cites no source for what it computes",
            registered.card.id
        );
        assert!(
            !registered.card.equations.is_empty() || registered.card.id.contains("runtime"),
            "{} states no formula",
            registered.card.id
        );
    }
    // Every definition in the catalog validates, and every one declares an omission.
    let catalog = set.catalog();
    assert!(catalog.len() > 30, "only {} metrics defined", catalog.len());
    for d in &catalog {
        d.validate().unwrap();
        assert!(!d.not_accounted.is_empty(), "{}", d.name);
    }
    // …and the catalog is sorted by name, so the generated page is stable.
    let names: Vec<&str> = catalog.iter().map(|d| d.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
}

#[test]
fn a_whole_run_produces_the_hand_computed_headline_numbers() {
    let samples = run(&recording());

    // 32 of 40 candidate receptions in the 0–25 m bin: 0.8, with a Wilson interval.
    let pdr = find(&samples, "pdr|dist_bin=0-25");
    assert_eq!(pdr.value.point(), Some(0.8));
    assert_eq!(pdr.value.n(), 40);
    match &pdr.value {
        v2xw_metrics::SampleValue::Ratio(r) => {
            let (lo, hi) = r.interval().expect("a proportion carries its interval");
            assert!(lo < 0.8 && 0.8 < hi, "{lo}..{hi}");
            // 32/40 at 95 % is (0.652424, 0.895000).
            assert!((lo - 0.652_424).abs() < 1e-5, "{lo}");
            assert!((hi - 0.895_000).abs() < 1e-5, "{hi}");
        }
        other => panic!("pdr must be a ratio: {other:?}"),
    }
    // PER is the complement over the same denominator.
    assert_eq!(find(&samples, "per").value.point(), Some(0.2));
    // Eight losses is below the default threshold of thirty, so the share of losses per
    // cause is reported as insufficient rather than as a confident 1.0 — which is rule 3 of
    // this crate's statistics, visible from outside.
    let by_cause = find(&samples, "pdr_by_cause|cause=collision");
    assert!(by_cause.value.is_insufficient(), "{:?}", by_cause.value);
    assert_eq!(by_cause.value.n(), 8);
    // Four transmitters at 600 µs each: 2.4 ms of air in a 1 s window, per node 0.6 ms/s.
    assert_eq!(
        find(&samples, "airtime_per_node|node=1").value.point(),
        Some(0.6)
    );
    // 4 × 400 B of air and 2,048 B of backhaul in one second.
    assert_eq!(
        find(&samples, "bytes_air|bucket=air").value.point(),
        Some(1_600.0)
    );
    assert_eq!(
        find(&samples, "bytes_backhaul|bucket=backhaul")
            .value
            .point(),
        Some(2_048.0)
    );
    // Envelope overhead: 100 B over 300 B on four messages.
    assert_eq!(
        find(&samples, "envelope_overhead").value.point(),
        Some(0.3333)
    );
    // Four messages is also below the threshold, so the attachment rate refuses too.
    assert!(find(&samples, "full_cert_share").value.is_insufficient());
    // 32 verifications at 1.2 ms in one second.
    assert_eq!(
        find(&samples, "verify_rate|primitive=ecdsa-p256")
            .value
            .point(),
        Some(32.0)
    );
    assert_eq!(find(&samples, "verify_cost").value.point(), Some(1.2));
    // 32 verified, 1 skipped: one unverified delivery in 33.
    assert_eq!(
        find(&samples, "unverified_ratio").value.point(),
        Some(0.0303)
    );
    // The revocation's stage latencies, in seconds.
    assert_eq!(
        find(&samples, "revocation_latency_stage|stage=decision->issued")
            .value
            .point(),
        Some(0.1)
    );
    assert_eq!(
        find(&samples, "crl_entries").value,
        v2xw_metrics::SampleValue::count(3)
    );
    // Safety: one closing pair in one frame is one sample, so it refuses too, and reports
    // the count that made it refuse.
    assert!(find(&samples, "ttc_min").value.is_insufficient());
    assert_eq!(find(&samples, "ttc_min").value.n(), 1);
    assert_eq!(find(&samples, "headway_distance").value.n(), 2);
    // Detection: no subject was declared to the provider, so the matrix is empty and every
    // summary refuses. The undeclared subjects are counted, not guessed at.
    assert!(
        find(&samples, "det_recall|level=vehicle")
            .value
            .is_insufficient(),
        "an empty matrix is not a recall"
    );
    assert_eq!(
        find(&samples, "det_tp|level=vehicle|cell=tp").value,
        v2xw_metrics::SampleValue::count(0)
    );
}

/// The hand-computed values of the metrics whose samples the default threshold refuses to
/// summarise — checked with the threshold lowered, so both the arithmetic and the refusal
/// are covered.
#[test]
fn the_thin_metrics_hold_their_hand_computed_values_once_the_threshold_allows_them() {
    let samples = run_sensitive(&recording());
    // All eight losses were collisions, so that cause's share is 1.0.
    assert_eq!(
        find(&samples, "pdr_by_cause|cause=collision").value.point(),
        Some(1.0)
    );
    // One of four messages carried a full certificate.
    assert_eq!(find(&samples, "full_cert_share").value.point(), Some(0.25));
    // The frame has three vehicles on one lane and therefore two consecutive pairs:
    // (0 m at 20 m/s behind 30 m at 10 m/s) and (30 m at 10 m/s behind 75 m at 15 m/s).
    // Only the first is closing, so there is one TTC: 30 m / 10 m/s = 3 s. Both pairs have a
    // time headway: 30/20 = 1.5 s and 45/10 = 4.5 s, mean 3 s.
    assert_eq!(find(&samples, "ttc_min").value.point(), Some(3.0));
    assert_eq!(find(&samples, "ttc_min").value.n(), 1);
    assert_eq!(find(&samples, "headway_time").value.point(), Some(3.0));
    assert_eq!(find(&samples, "headway_time").value.n(), 2);
    // Speeds 20, 10 and 15 m/s: mean 15.
    assert_eq!(find(&samples, "speed").value.point(), Some(15.0));
    // Receptions are 1 ms apart and cycle over three receivers, so the shortest gap between
    // two receptions from the same transmitter at the same receiver is three milliseconds.
    match &find(&samples, "pir").value {
        v2xw_metrics::SampleValue::Distribution(v2xw_metrics::DistributionSummary::Summary {
            min,
            n,
            ..
        }) => {
            assert_eq!(*min, 0.003);
            assert!(*n > 20, "only {n} gaps");
        }
        other => panic!("pir must be a distribution: {other:?}"),
    }
    // Three receivers, so the 32 deliveries of a 300 B payload are 9,600 B in one second.
    assert_eq!(find(&samples, "goodput").value.point(), Some(9_600.0));
}

#[test]
fn nothing_in_a_whole_runs_output_is_a_nan_or_an_infinity() {
    for s in run(&recording()) {
        for f in s.floats() {
            assert!(f.is_finite(), "{}: {f}", s.key());
        }
    }
    // …and the same on an empty run, which is where a division would show up.
    for s in run(&[]) {
        for f in s.floats() {
            assert!(f.is_finite(), "{}: {f}", s.key());
        }
    }
}

#[test]
fn a_run_with_no_events_reports_insufficient_rather_than_zero() {
    let samples = run(&[]);
    let insufficient = samples.iter().filter(|s| s.value.is_insufficient()).count();
    assert!(
        insufficient >= 10,
        "only {insufficient} of {} samples refused to estimate over no data",
        samples.len()
    );
    // A count of zero is a measurement and is reported as one.
    assert_eq!(
        find(&samples, "ttc_conflicts").value,
        v2xw_metrics::SampleValue::count(0)
    );
}

#[test]
fn the_digest_does_not_depend_on_the_order_independent_events_arrived_in() {
    let base = recording();
    let forward = digest(run(&base));

    // The four transmissions are independent of one another: different nodes, different
    // message ids, no ordering relation. Swapping them is the reordering a change of thread
    // count produces, and it must not move a bit.
    let mut reordered = base.clone();
    let tx_positions: Vec<usize> = reordered
        .iter()
        .enumerate()
        .filter(|(_, r)| r.channel == "node.tx")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(tx_positions.len(), 4);
    reordered.swap(tx_positions[0], tx_positions[2]);
    reordered.swap(tx_positions[1], tx_positions[3]);
    assert_eq!(forward, digest(run(&reordered)));

    // And the same run twice is the same digest, which is invariant I-C1 at this layer.
    assert_eq!(forward, digest(run(&base)));
}

#[test]
fn the_runtime_diagnostics_are_provably_outside_the_digest() {
    let base = recording();

    // Two runs of the same recording, differing only in the machine-dependent measurements
    // the harness hands the runtime provider.
    // The four data providers, plus one runtime provider this test keeps a handle on so it
    // can hand it the machine-dependent measurements a harness would.
    let digest_with = |wall_ms: u64, memory: u64| {
        let mut registry = Registry::new();
        let mut set = ProviderSet::new();
        set.register(
            &mut registry,
            Box::new(v2xw_metrics::comms::CommsProvider::new(0)),
        )
        .unwrap();
        set.register(
            &mut registry,
            Box::new(v2xw_metrics::security::SecurityProvider::new(0)),
        )
        .unwrap();
        set.register(
            &mut registry,
            Box::new(v2xw_metrics::detection::DetectionProvider::new()),
        )
        .unwrap();
        set.register(
            &mut registry,
            Box::new(v2xw_metrics::safety::SafetyProvider::new(0)),
        )
        .unwrap();
        for r in &base {
            set.on_event(r);
        }
        let mut runtime = v2xw_metrics::runtime::RuntimeProvider::new();
        for r in &base {
            v2xw_metrics::MetricProvider::on_event(&mut runtime, r);
        }
        runtime.observe_elapsed(Duration::from_millis(wall_ms), Duration::from_secs(1));
        runtime.observe_memory(memory);
        let mut samples = set.flush(1_000_000_000);
        samples.extend(v2xw_metrics::MetricProvider::flush(
            &mut runtime,
            1_000_000_000,
        ));
        let (digested, removed) = DigestSet::partition(samples.clone());
        (digested.digest_hex().unwrap(), removed.len(), samples)
    };

    let (fast, removed_fast, samples_fast) = digest_with(50, 1 << 20);
    let (slow, removed_slow, samples_slow) = digest_with(50_000, 8 << 30);
    assert_eq!(
        fast, slow,
        "a machine a thousand times slower must produce the same digest"
    );
    assert_eq!(removed_fast, removed_slow);
    assert_eq!(
        removed_fast, 3,
        "the three diagnostics of 08-measurement §2"
    );

    // The fourth mechanism, and the one that was missing: the *file* digest a run manifest
    // records for `metrics/summary.json`. `RunSummary::digest` was already independent of
    // the diagnostics; `file_digest` hashes the whole canonical document, which serialised
    // them, so two machines disagreed on the manifest entry — and on the `data_digest`
    // `Manifest::finalize` derives from every file digest — for a reason that has nothing to
    // do with the simulation.
    let summary_fast = RunSummary::new(samples_fast.clone()).unwrap();
    let summary_slow = RunSummary::new(samples_slow.clone()).unwrap();
    assert_eq!(summary_fast.diagnostics.len(), 3);
    assert_ne!(
        summary_fast.diagnostics["wall_clock_per_sim_second"]
            .value
            .point(),
        summary_slow.diagnostics["wall_clock_per_sim_second"]
            .value
            .point(),
        "the two runs must genuinely differ in their machine-dependent numbers"
    );
    assert_eq!(summary_fast.digest, summary_slow.digest);
    let document = |s: &RunSummary| String::from_utf8(s.to_canonical_json().unwrap()).unwrap();
    assert_eq!(
        document(&summary_fast),
        document(&summary_slow),
        "the summary document itself must not carry a machine-dependent number"
    );
    assert_eq!(
        summary_fast.file_digest("metrics/summary.json").unwrap(),
        summary_slow.file_digest("metrics/summary.json").unwrap(),
        "the digest the run manifest records for the summary file"
    );
    // A manifest built from those digests agrees too, which is the property the
    // cross-platform determinism gate actually compares.
    let data_digest = |s: &RunSummary| {
        let mut m = v2xw_core::manifest::Manifest::new("0.1.0", 1);
        m.files.push(s.file_digest("metrics/summary.json").unwrap());
        m.finalize()
    };
    assert_eq!(data_digest(&summary_fast), data_digest(&summary_slow));
    // …and the numbers are not lost: they leave in their own document, which differs.
    assert_ne!(
        summary_fast.diagnostics_json().unwrap(),
        summary_slow.diagnostics_json().unwrap()
    );
    assert_eq!(
        summary_fast.diagnostics().summary_digest,
        summary_fast.digest
    );

    // The explicit check refuses the unfiltered list by name.
    let e = metric_digest(&samples_fast).unwrap_err();
    assert!(
        matches!(e, v2xw_metrics::MetricError::DiagnosticInDigest { .. }),
        "{e}"
    );

    // …and the deterministic event count is *not* excluded, because it is a property of the
    // run: two runs that processed different numbers of events did not run the same thing.
    let mut registry = Registry::new();
    let mut set = ProviderSet::new();
    v2xw_metrics::register_all(&mut registry, &mut set, 0).unwrap();
    for r in &base {
        set.on_event(r);
    }
    let samples = set.flush(1_000_000_000);
    let events = find(&samples, "events_processed");
    assert!(!events.diagnostic);
    let (digested, _) = DigestSet::partition(samples);
    assert!(digested.keys().contains(&"events_processed"));
}

#[test]
fn the_invariants_hold_on_a_well_formed_run() {
    let records = recording();
    let mut ledger = EventLedger::new();
    ledger.ingest_all(&records);
    let samples = run(&records);
    let report = check_all(&ledger, &samples);
    report.assert_all().unwrap_or_else(|e| panic!("{e}"));
    assert!(report.held());
    assert!(
        report.skipped().is_empty(),
        "every check had data to run on, but these were skipped: {:?}",
        report.skipped()
    );
    // Each check examined something, so "zero violations" is not "nothing looked at".
    for o in &report.outcomes {
        assert!(o.checked > 0, "{} examined nothing", o.invariant);
    }
}

#[test]
fn each_invariant_check_catches_a_violation_injected_into_the_run() {
    /// One injected violation: the invariant it should trip, and how to break the run.
    type Case = (&'static str, Box<dyn Fn(&mut Vec<OwnedRecord>)>);

    // Every entry is (the invariant, a mutation of the well-formed recording).
    let cases: Vec<Case> = vec![
        (
            "I-R3",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // A loss with no cause.
                r.push(rec(
                    "phy.rx",
                    json!({"t_start": 0, "t_end": 1, "tx": 1, "rx": 2, "outcome": "lost"}),
                ));
            }),
        ),
        (
            "I-N1",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // Frame 1's bytes attributed to the backhaul as well as to the air.
                r.push(rec(
                    "net.bytes",
                    json!({"t": 1, "id": 1, "bucket": "backhaul", "bytes_on_wire": 400}),
                ));
            }),
        ),
        (
            "I-M1",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // A kinematics record out of actor order within its frame.
                r.push(rec(
                    "gt.kinematics",
                    json!({"t": 900_000_000, "actor": 0, "x_m": 0.0, "y_m": 0.0,
                           "speed_mps": 1.0}),
                ));
            }),
        ),
        (
            "I-P4",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // A second revocation published with no decision behind it.
                r.push(rec(
                    "proto.revocation",
                    json!({"t": 950_000_000, "stage": "published", "id": "rev-2"}),
                ));
            }),
        ),
        (
            "I-T2",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // A ground-truth record on a NODE channel.
                r.push(OwnedRecord {
                    channel: "node.tx",
                    visibility: Visibility::Gt,
                    json: serde_json::to_vec(&json!({"t": 1, "node": 1, "bytes_on_wire": 100}))
                        .unwrap(),
                });
            }),
        ),
        (
            "I-T3",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // An attack action on the air naming a message nobody transmitted.
                r.push(rec(
                    "gt.attack.action",
                    json!({"t": 960_000_000, "actor": 9, "attacker": "ghost",
                           "action": "replay", "msg": 4_242}),
                ));
            }),
        ),
        (
            "M-RX1",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // One attempt's fate goes missing: delivered + lost falls short of attempts.
                let i = r.iter().position(|x| x.channel == "node.rx").unwrap();
                r.remove(i);
            }),
        ),
        (
            "M-LAT1",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // A delivery whose signature "finished" before its message was generated.
                r.push(rec(
                    "node.rx",
                    json!({"t": 5, "rx": 2, "tx": 1, "msg": 5_000, "outcome": "delivered",
                           "t_generated": 100, "t_sign_start": 50, "t_signed": 60,
                           "t_tx_start": 70, "t_tx_end": 80, "t_arrival": 81,
                           "t_rx_done": 82, "t_delivered": 82}),
                ));
                r.push(rec(
                    "phy.rx",
                    json!({"t_start": 70, "t_end": 80, "tx": 1, "rx": 2, "msg": 5_000,
                           "outcome": "ok"}),
                ));
            }),
        ),
        (
            "M-BYTE1",
            Box::new(|r: &mut Vec<OwnedRecord>| {
                // A frame whose layers add up to more than went on the air.
                r.push(rec(
                    "node.tx",
                    json!({"t": 1, "node": 4, "msg": 44, "bytes_on_wire": 100,
                           "spdu_bytes": 90, "net_header_bytes": 5, "link_bytes": 38}),
                ));
            }),
        ),
    ];

    for (invariant, inject) in cases {
        let mut records = recording();
        inject(&mut records);
        let mut ledger = EventLedger::new();
        ledger.ingest_all(&records);
        let report = check_all(&ledger, &run(&records));
        assert!(
            report.failed().contains(&invariant),
            "{invariant} did not catch its injected violation; failures were {:?}",
            report.failed()
        );
        let e = report.assert_all().unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains(invariant), "{msg}");
        // The message carries numbers, not just a verdict.
        assert!(msg.contains('='), "{msg} has no numbers in it");
    }

    let ledger = {
        let mut l = EventLedger::new();
        l.ingest_all(&recording());
        l
    };
    // M-BYTE2 and M-SHARE are over samples: a bucket rate that no longer sums to the
    // total, and a stage share that no longer sums to one.
    let mut samples = run(&recording());
    let total = samples
        .iter_mut()
        .find(|s| s.metric == "bytes_total")
        .expect("bytes_total is sampled");
    total.value =
        v2xw_metrics::SampleValue::Scalar(v2xw_metrics::Estimate::Value { point: 1.0, n: 1 });
    assert!(check_all(&ledger, &samples).failed().contains(&"M-BYTE2"));
    let mut samples = run(&recording());
    let share = samples
        .iter_mut()
        .find(|s| s.metric == "latency_stage_share" && !s.value.is_insufficient())
        .expect("a stage share is sampled");
    share.value =
        v2xw_metrics::SampleValue::Scalar(v2xw_metrics::Estimate::Value { point: 0.9, n: 1 });
    assert!(check_all(&ledger, &samples).failed().contains(&"M-SHARE"));

    // D9's scan, the seventh check, is over samples rather than records. Two injections:
    // a raw float, and — the case the check could not see before — a float quantised onto
    // the wrong declared grid.
    let ledger = {
        let mut l = EventLedger::new();
        l.ingest_all(&recording());
        l
    };
    let mut samples = run(&recording());
    samples[0].value = v2xw_metrics::SampleValue::Scalar(v2xw_metrics::Estimate::Value {
        point: 1.0 / 3.0,
        n: 1,
    });
    let report = check_all(&ledger, &samples);
    assert!(report.failed().contains(&"D9"), "{:?}", report.failed());

    // A real sample of a real metric, re-quantised onto a grid that is not its own. Every
    // sample in the run declares a grid coarser than or equal to 1e-6, so the value below is
    // off its own grid and on the probability grid — the shape the old guard swallowed.
    let mut samples = run(&recording());
    let target = samples
        .iter_mut()
        .find(|s| s.quantum != Quantum::PROBABILITY && !s.value.is_insufficient())
        .expect("the run has at least one estimated sample on a grid coarser than 1e-6");
    assert!(
        Quantum::PROBABILITY.quantise(1.0 / 3.0) == 0.333_333 && !target.quantum.holds(0.333_333),
        "the injected value must be off {:?}",
        target.quantum
    );
    target.value = v2xw_metrics::SampleValue::Scalar(v2xw_metrics::Estimate::Value {
        point: 0.333_333,
        n: 1,
    });
    let report = check_all(&ledger, &samples);
    assert!(
        report.failed().contains(&"D9"),
        "a value quantised onto the wrong grid went undetected; failures were {:?}",
        report.failed()
    );
    let detail = report
        .violations()
        .find(|v| v.invariant == "D9")
        .map(|v| v.detail.clone())
        .unwrap_or_default();
    assert!(detail.contains("0.333333"), "{detail}");

    // A non-finite value is on no grid at all, and `is_on_grid` cannot see it.
    let mut samples = run(&recording());
    samples[0].value = v2xw_metrics::SampleValue::Scalar(v2xw_metrics::Estimate::Value {
        point: f64::NAN,
        n: 1,
    });
    let report = check_all(&ledger, &samples);
    assert!(report.failed().contains(&"D9"), "{:?}", report.failed());
}

#[test]
fn the_arrow_table_is_on_grid_and_round_trips_through_ipc() {
    let samples = run(&recording());
    let batch = samples_batch(&samples).unwrap();
    assert_eq!(batch.num_rows(), samples.len());

    // Every float in the table sits on the row's declared grid (build decision D9).
    let quanta: Vec<f64> = {
        let i = batch.schema().index_of("quantum").unwrap();
        let a = batch
            .column(i)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        (0..a.len()).map(|r| a.value(r)).collect()
    };
    for name in ["value", "min", "max", "mean", "p50", "p95", "p99"] {
        let i = batch.schema().index_of(name).unwrap();
        let a = batch
            .column(i)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        for (row, quantum) in quanta.iter().enumerate() {
            if a.is_null(row) {
                continue;
            }
            let q = Quantum::new(*quantum);
            assert!(
                q.holds(a.value(row)),
                "{name}[{row}] = {} is off the grid {quantum}",
                a.value(row)
            );
        }
    }

    // The same samples produce the same bytes, twice.
    let a = to_ipc(std::slice::from_ref(&batch)).unwrap();
    let b = to_ipc(&[samples_batch(&samples).unwrap()]).unwrap();
    assert_eq!(a, b);

    // And the stream reads back.
    let reader = arrow::ipc::reader::StreamReader::try_new(a.as_slice(), None).unwrap();
    let read: Vec<_> = reader.map(|x| x.unwrap()).collect();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].num_rows(), batch.num_rows());
}

#[test]
fn the_summary_is_a_manifest_sized_view_of_the_run() {
    let samples = run(&recording());
    let summary = RunSummary::new(samples.clone()).unwrap().with_rejected(0);
    assert_eq!(summary.schema, v2xw_metrics::summary::SUMMARY_SCHEMA);
    assert_eq!(summary.digest.len(), 64);
    assert_eq!(summary.diagnostics.len(), 3);
    assert_eq!(summary.point("pdr|dist_bin=0-25"), Some(0.8));
    assert!(
        summary.insufficient > 0,
        "a run this small has thin metrics, and the summary should say how many"
    );

    // The manifest carries the summary as a file digest over exactly the bytes written.
    let fd = summary.file_digest("metrics/summary.json").unwrap();
    assert_eq!(fd.path, "metrics/summary.json");
    assert_eq!(
        fd.sha256,
        v2xw_core::hash::sha256_hex(&summary.to_canonical_json().unwrap())
    );

    // A manifest built around it finalises, and the summary's own digest is reproducible.
    let again = RunSummary::new(samples).unwrap();
    assert_eq!(again.digest, summary.digest);
}

#[test]
fn the_providers_report_what_they_could_not_read_rather_than_swallowing_it() {
    let mut registry = Registry::new();
    let mut set = ProviderSet::new();
    v2xw_metrics::register_all(&mut registry, &mut set, 0).unwrap();
    set.on_event(&rec("phy.rx", json!({"nonsense": true})));
    set.on_event(&rec("node.tx", json!({"also": "nonsense"})));
    assert!(
        set.rejected_total() >= 2,
        "the set reported {} undecodable records",
        set.rejected_total()
    );
}

/// Every sample of the well-formed run lies in its metric's physical range, and one
/// impossible value — a delivery ratio above one, a negative latency — fails the check.
#[test]
fn an_impossible_metric_value_fails_the_range_check() {
    let mut registry = Registry::new();
    let mut set = ProviderSet::new();
    v2xw_metrics::register_all(&mut registry, &mut set, 0).unwrap();
    let catalog = set.catalog();
    let samples = run(&recording());
    let ok = v2xw_metrics::invariants::check_metric_ranges(&samples, &catalog);
    assert!(ok.held(), "{:?}", ok.violations);
    assert!(ok.checked > 20, "only {} values checked", ok.checked);

    for (metric, value) in [("pdr", 1.2), ("e2e_latency", -3.0), ("bytes_total", -1.0)] {
        let mut bad = samples.clone();
        let target = bad
            .iter_mut()
            .find(|s| s.metric == metric)
            .unwrap_or_else(|| panic!("{metric} is sampled"));
        target.value =
            v2xw_metrics::SampleValue::Scalar(v2xw_metrics::Estimate::Value { point: value, n: 1 });
        let o = v2xw_metrics::invariants::check_metric_ranges(&bad, &catalog);
        assert!(!o.held(), "{metric} = {value} went undetected");
    }
}
