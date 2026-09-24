//! Fragmentation in the run path (04-models.md §7.3, §7.4; `v2xw_engine::frag`).
//!
//! No signature this build can select makes a message larger than the MTU, so every test
//! here pads the signed message with `net.fragmenter.params.sdu_padding_bytes`, which
//! stands in for a post-quantum signature or certificate. Each test checks the run against
//! something that could disagree with it: the fragments on the air against the SDUs the
//! reassembly records name, the node's fate against the fragments' own PHY outcomes, the
//! realised loss against the loss its fragments' PHY success probabilities predict, and a
//! run with nothing to split against the same run without a fragmenter.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use v2xw_engine::scenario::ModelChoice;
use v2xw_engine::{DigestRecorder, Engine, MemoryRecorder, RunReport, Scenario};
use v2xw_metrics::EventLedger;
use v2xw_metrics::channels::{NetReassemblyView, NodeTxView, RxFate, rx_cause};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

/// The Phase 1 grid, open air, dense, short: enough SDUs that every reassembly outcome
/// happens, in a few seconds of simulated time.
fn grid(duration_s: f64) -> Scenario {
    let mut s = Scenario::load(scenarios().join("phase1-grid.yaml")).expect("loads");
    s.time.duration_s = duration_s;
    s.world.buildings.enabled = false;
    // Four avenues by six streets (1.1 km by 0.5 km) rather than 13 by 34: the same grid,
    // small enough that the fleet a few seconds of demand spawns is within radio range of
    // itself.
    if let v2xw_world::WorldSourceSpec::Procedural { params, .. } = &mut s.world.source {
        params["cols"] = serde_json::json!(4);
        params["rows"] = serde_json::json!(6);
    }
    s.actors.vehicles.demand.rate_veh_per_h = Some(20_000.0);
    s.metrics = vec!["all".to_string()];
    s
}

fn fragmenter(id: &str, params: serde_json::Value) -> Option<ModelChoice> {
    Some(ModelChoice {
        id: id.to_string(),
        params,
    })
}

fn run(s: Scenario) -> (RunReport, MemoryRecorder) {
    let mut engine = Engine::build(s, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (report, recorder)
}

fn ledger(recorder: &MemoryRecorder) -> EventLedger {
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
    ledger
}

fn samples(recorder: &MemoryRecorder) -> Vec<v2xw_metrics::MetricSample> {
    recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == "metric.sample")
        .map(|(_, r)| serde_json::from_slice(&r.json).expect("a metric sample decodes"))
        .collect()
}

/// Realised against predicted, over a run's reassembly records.
struct Amplification {
    groups: usize,
    sdu_loss: f64,
    fragment_loss: f64,
    predicted: f64,
}

fn amplification(groups: &[NetReassemblyView]) -> Amplification {
    let n = groups.len();
    let lost = groups.iter().filter(|g| g.received < g.fragments).count();
    let fragments: u64 = groups.iter().map(|g| u64::from(g.fragments)).sum();
    let missing: u64 = groups
        .iter()
        .map(|g| u64::from(g.fragments - g.received))
        .sum();
    let predicted: f64 = groups.iter().map(|g| g.predicted_loss).sum();
    Amplification {
        groups: n,
        sdu_loss: lost as f64 / n.max(1) as f64,
        fragment_loss: missing as f64 / fragments.max(1) as f64,
        predicted: predicted / n.max(1) as f64,
    }
}

/// A padded BSM goes out as two generic pieces; each receiver gets one attempt at the
/// message, which reaches its node only whole, and the loss the pieces amplify is
/// recorded beside the loss their PHY success probabilities predict.
#[test]
fn a_padded_message_goes_out_in_pieces_and_reaches_the_node_only_whole() {
    let mut s = grid(8.0);
    s.net.fragmenter = fragmenter(
        v2xw_net::FRAGMENTER_GENERIC_ID,
        serde_json::json!({"sdu_padding_bytes": 1_600}),
    );
    let (report, recorder) = run(s);
    let l = ledger(&recorder);

    assert!(report.sdus_fragmented > 100, "{report:?}");
    assert_eq!(
        report.fragments_sent,
        2 * report.sdus_fragmented,
        "a ~1,800-octet SDU over a 1,400-octet MTU is two pieces"
    );
    assert_eq!(report.net_mtu_refusals, 0);

    // Every piece on the air is a fragment of an SDU the node.rx side follows as one.
    let pieces: Vec<_> = l.rx.iter().filter(|r| r.sdu.is_some()).collect();
    assert_eq!(pieces.len(), l.rx.len(), "every BSM went out in pieces");
    let attempts: BTreeSet<(u64, u32)> = pieces
        .iter()
        .map(|r| (r.sdu.unwrap_or(0), r.rx.index()))
        .collect();
    assert_eq!(
        l.node_rx.len(),
        attempts.len(),
        "one node.rx fate per (SDU, receiver), not per piece"
    );
    assert!(l.node_rx.len() < l.rx.len());

    // The measurement invariants hold over it, M-RX1's grouping included.
    let checks = v2xw_metrics::check_all(&l, &samples(&recorder));
    checks.assert_all().unwrap_or_else(|e| panic!("{e}"));

    // A message delivered to an application is a whole SDU: its size is both pieces'.
    let delivered: Vec<_> = l
        .node_rx
        .iter()
        .filter(|v| v.outcome == RxFate::Delivered)
        .collect();
    assert!(
        !delivered.is_empty(),
        "no reassembled message was delivered"
    );
    for v in &delivered {
        assert!(
            v.bytes_on_wire.unwrap_or(0) > 1_800,
            "a delivered SDU of {:?} octets on the air",
            v.bytes_on_wire
        );
    }

    // The reassembly records agree with node.rx: a complete group is a message the node
    // took (delivered, or lost above the PHY); a lost one is lost with its fragment's cause.
    let fates: BTreeMap<(u64, u32), &v2xw_metrics::channels::NodeRxView> = l
        .node_rx
        .iter()
        .map(|v| ((v.msg.unwrap_or(0), v.rx.index()), v))
        .collect();
    assert!(!l.reassembly.is_empty());
    for g in &l.reassembly {
        assert_eq!(g.kind, "message");
        assert_eq!(g.fragments, 2);
        let fate = fates
            .get(&(g.sdu, g.rx.index()))
            .unwrap_or_else(|| panic!("a resolved group with no node.rx fate: {g:?}"));
        if g.outcome == "complete" {
            assert!(
                fate.outcome != RxFate::Lost
                    || !fate.cause.as_deref().is_some_and(rx_cause::is_phy),
                "a reassembled SDU blamed on the PHY: {fate:?}"
            );
        } else {
            assert_eq!(fate.outcome, RxFate::Lost);
            assert_eq!(
                fate.cause, g.cause,
                "the SDU's cause is its missing piece's"
            );
        }
    }

    // Loss amplification: an SDU in two pieces is lost when either is, so its loss is at
    // least the pieces' own — whatever the correlation between them.
    let a = amplification(&l.reassembly);
    let uncertain = l
        .reassembly
        .iter()
        .filter(|g| g.predicted_loss > 0.01 && g.predicted_loss < 0.99)
        .count();
    println!(
        "  groups whose prediction is neither near 0 nor near 1: {uncertain} of {}",
        l.reassembly.len()
    );
    println!(
        "generic-sdu, 2 pieces: {} groups, SDU loss {:.4}, fragment loss {:.4}, predicted \
         1 - prod(1 - p_i) {:.4}, independent 1 - (1 - p)^2 {:.4}",
        a.groups,
        a.sdu_loss,
        a.fragment_loss,
        a.predicted,
        1.0 - (1.0 - a.fragment_loss).powi(2)
    );
    assert!(
        a.sdu_loss >= a.fragment_loss,
        "no amplification: {:.4} < {:.4}",
        a.sdu_loss,
        a.fragment_loss
    );
    assert!(a.sdu_loss > 0.0 && a.sdu_loss < 1.0);
    assert!(a.predicted > 0.0 && a.predicted < 1.0);
    assert_eq!(
        report.reassembly_complete + report.reassembly_lost,
        l.reassembly.len() as u64
    );

    // The metric provider reports the same realised loss the records hold.
    let s = samples(&recorder);
    let windows: Vec<_> = s
        .iter()
        .filter(|x| x.metric == "frag_sdu_loss" && x.dims.is_empty())
        .collect();
    assert!(!windows.is_empty(), "frag_sdu_loss was never reported");
    // Every group the provider was handed counts; one it refused would be missing here.
    // The groups resolved after the last metric window closes are the only ones allowed
    // to be missing.
    let counted: u64 = windows.iter().map(|x| x.value.n()).sum();
    let last_flush = windows.iter().map(|x| x.t).max().unwrap_or(0);
    let before: u64 = l.reassembly.iter().filter(|g| g.t <= last_flush).count() as u64;
    assert_eq!(
        counted, before,
        "the provider did not count every reassembly record"
    );
    let independent: Vec<f64> = s
        .iter()
        .filter(|x| x.metric == "frag_sdu_loss_independent" && x.dims.is_empty())
        .filter_map(|x| x.value.point())
        .collect();
    println!(
        "  independence baseline 1 - (1 - p)^2 per window: {:?}",
        independent
            .iter()
            .map(|v| format!("{v:.4}"))
            .collect::<Vec<_>>()
    );
}

/// A group the PHY lost a piece of is given up on at the reassembly timeout, not before:
/// a receiver cannot know a broadcast piece is never coming until its timer runs out.
#[test]
fn a_missing_piece_is_lost_at_the_reassembly_timeout_with_its_phy_cause() {
    let mut s = grid(6.0);
    s.net.fragmenter = fragmenter(
        v2xw_net::FRAGMENTER_GENERIC_ID,
        serde_json::json!({"sdu_padding_bytes": 1_600, "reassembly_timeout_ms": 400}),
    );
    let (report, recorder) = run(s);
    let l = ledger(&recorder);
    let end = report.end_ns;
    let mut timed_out = 0;
    for v in &l.node_rx {
        if v.outcome != RxFate::Lost || v.t >= end {
            continue;
        }
        let Some(cause) = v.cause.as_deref() else {
            continue;
        };
        if !(rx_cause::is_phy(cause) || cause == rx_cause::REASSEMBLY_FAILED) {
            continue;
        }
        let start = v.t_tx_start.expect("a piece's attempt carries its journey");
        assert!(
            v.t >= start + 400_000_000,
            "an SDU given up on {} ns after its first piece went on the air, before the \
             400 ms timeout",
            v.t - start
        );
        timed_out += 1;
    }
    assert!(
        timed_out > 0,
        "no SDU was lost to a missing piece, so this proves nothing"
    );
}

/// With nothing large enough to split, a splitting fragmenter changes nothing: the same
/// frames, receptions and fates as the run without one.
#[test]
fn with_nothing_to_split_a_fragmenter_changes_no_frame() {
    let plain = grid(4.0);
    let mut split = grid(4.0);
    split.net.fragmenter = fragmenter(v2xw_net::FRAGMENTER_GENERIC_ID, serde_json::Value::Null);
    let (a_report, a) = run(plain);
    let (b_report, b) = run(split);
    assert_eq!(b_report.sdus_fragmented, 0);
    let stream = |r: &MemoryRecorder| -> Vec<Vec<u8>> {
        r.records()
            .iter()
            .filter(|(_, r)| matches!(r.channel, "node.tx" | "phy.rx" | "node.rx"))
            .map(|(_, r)| r.json.clone())
            .collect()
    };
    assert!(!stream(&a).is_empty());
    assert!(
        stream(&a) == stream(&b),
        "a fragmenter with nothing to split changed the air"
    );
    assert_eq!(a_report.reception_attempts, b_report.reception_attempts);
}

/// Facilities-layer segments are messages of their own: each one reaches the node as it
/// arrives, a lost one loses only its share of the content, and nothing is followed as
/// one attempt.
#[test]
fn facilities_segments_are_messages_of_their_own() {
    let mut s = grid(6.0);
    s.net.layer = "gn-btp".to_string();
    s.messages.sets = vec!["cam".to_string()];
    s.net.fragmenter = fragmenter(
        v2xw_net::FRAGMENTER_FACILITIES_ID,
        serde_json::json!({"sdu_padding_bytes": 1_600}),
    );
    let (report, recorder) = run(s);
    let l = ledger(&recorder);
    assert!(report.sdus_fragmented > 0, "{report:?}");
    assert!(
        l.rx.iter().all(|r| r.sdu.is_none()),
        "a segment is not a piece"
    );
    assert_eq!(l.node_rx.len(), l.rx.len(), "one fate per segment");
    let checks = v2xw_metrics::check_all(&l, &samples(&recorder));
    checks.assert_all().unwrap_or_else(|e| panic!("{e}"));
    assert!(!l.reassembly.is_empty());
    for g in &l.reassembly {
        assert_eq!(g.kind, "segments");
        assert!(g.fragments >= 2);
    }
    // Content loss is the segments' own loss, weighted by size: no amplification.
    let lost_bytes: u64 = l
        .reassembly
        .iter()
        .map(|g| g.bytes - g.bytes_received)
        .sum();
    let bytes: u64 = l.reassembly.iter().map(|g| g.bytes).sum();
    let a = amplification(&l.reassembly);
    let content_loss = lost_bytes as f64 / bytes as f64;
    println!(
        "facilities, {} groups: content loss {:.4}, segment loss {:.4}, any-segment loss {:.4}",
        a.groups, content_loss, a.fragment_loss, a.sdu_loss
    );
    assert!(
        (content_loss - a.fragment_loss).abs() < 0.02,
        "equal segments lose content in proportion to segments: {content_loss} vs {}",
        a.fragment_loss
    );
}

/// The Partially-Hybrid certificate cycle: a 3,000-octet hybrid certificate over a
/// 1,400-octet MTU needs α = 3 of each five SPDUs, which are larger by its fragment; the
/// certificate is reassembled at each receiver per cycle.
#[test]
fn a_hybrid_certificate_rides_in_the_first_alpha_spdus_of_each_cycle() {
    let mut s = grid(6.0);
    s.net.fragmenter = fragmenter(
        v2xw_net::FRAGMENTER_CERT_CYCLE_ID,
        serde_json::json!({"hybrid_cert_bytes": 3_000}),
    );
    let (_, recorder) = run(s);
    let l = ledger(&recorder);
    // Per sender, in order, the SPDUs' sizes: three large, two small, repeating.
    let mut by_node: BTreeMap<u32, Vec<&NodeTxView>> = BTreeMap::new();
    for t in &l.tx {
        by_node.entry(t.node.index()).or_default().push(t);
    }
    let mut cycles_checked = 0;
    for frames in by_node.values() {
        for cycle in frames.chunks_exact(5) {
            let sizes: Vec<u64> = cycle.iter().map(|t| t.bytes_on_wire).collect();
            let small = sizes[3].max(sizes[4]);
            assert!(
                sizes[..3].iter().all(|&b| b >= small + 900),
                "the first three SPDUs of a cycle carry a ~1,000-octet fragment: {sizes:?}"
            );
            cycles_checked += 1;
        }
    }
    assert!(cycles_checked > 10);
    assert!(!l.reassembly.is_empty());
    for g in &l.reassembly {
        assert_eq!(g.kind, "certificate");
        assert_eq!(g.fragments, 3);
        assert_eq!(g.bytes, 3_000);
    }
    let a = amplification(&l.reassembly);
    println!(
        "cert-cycle, alpha = 3: {} groups, certificate loss {:.4}, fragment loss {:.4}, \
         predicted {:.4}",
        a.groups, a.sdu_loss, a.fragment_loss, a.predicted
    );
    assert!(a.sdu_loss >= a.fragment_loss);
    assert!(
        l.reassembly.iter().any(|g| g.outcome == "complete"),
        "no certificate was ever reassembled"
    );
}

/// The fragmented run is deterministic: the same records twice.
#[test]
fn a_fragmented_run_is_deterministic() {
    let digest = || {
        let mut s = grid(3.0);
        s.net.fragmenter = fragmenter(
            v2xw_net::FRAGMENTER_GENERIC_ID,
            serde_json::json!({"sdu_padding_bytes": 1_600}),
        );
        let mut engine = Engine::build(s, "").expect("builds");
        let mut recorder = DigestRecorder::new();
        engine.run(&mut recorder).expect("runs");
        recorder.digest_hex()
    };
    assert_eq!(digest(), digest());
}

/// Padding with no splitting fragmenter is refused by name: every padded message above
/// the MTU would be dropped, which is a scenario that measures nothing.
#[test]
fn padding_without_a_splitting_fragmenter_is_refused() {
    let mut s = grid(1.0);
    s.net.fragmenter = fragmenter(
        v2xw_net::FRAGMENTER_NONE_ID,
        serde_json::json!({"sdu_padding_bytes": 1_600}),
    );
    let errors = v2xw_engine::scenario::validate(&s);
    assert!(
        errors
            .iter()
            .any(|e| e.field() == Some("net.fragmenter.params.sdu_padding_bytes")),
        "{errors:#?}"
    );
}
