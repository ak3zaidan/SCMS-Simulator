//! The attacks and the detectors, in a running simulation.
//!
//! Four questions, each answered by a run rather than by a fixture:
//!
//! 1. Does an attacker change what goes on the air, and does the ground-truth channel say
//!    what it changed? (`tests/common/sim.rs` is the host; invariant I-T3.)
//! 2. Do the detectors run on what a node *received*, and do they land on the channel the
//!    metrics crate scores into a confusion matrix?
//! 3. What are the rates, on a scenario with a known attacker fraction?
//! 4. Is the whole thing reproducible?
//!
//! Every number these tests print is measured. The assertions are floors and structural
//! facts rather than pinned rates: a rate pinned to three digits is a test of the machine,
//! not of the model. The one place a number *is* pinned is a falsification magnitude,
//! because that one is a ported constant and drifting it is exactly the failure
//! `tests/legacy_conformance.rs` exists to catch.

mod common;

use std::collections::BTreeMap;

use common::sim::{self, SimOptions};
use v2xw_threat::attack::AttackKind;
use v2xw_threat::records::{DetObservation, GtAttackAction};

/// The attack reaches the channel, and the ground-truth channel says what it changed.
#[test]
fn an_attacker_changes_what_goes_on_the_air_and_the_ground_truth_channel_records_it() {
    let opts = SimOptions::new(1, 14_400.0, 0.25).with_attack(AttackKind::ConstPosOffset);
    let out = sim::run(&opts);

    eprintln!("{}", sim::summarize(&out));

    assert!(out.nodes >= 10, "the fleet must be big enough: {}", out.nodes);
    assert!(out.attackers > 0, "the run must have attackers");
    assert!(
        out.falsified_frames > 0,
        "an attacker must have put a falsified frame on the air"
    );

    let actions: Vec<GtAttackAction> = out.decode("gt.attack.action");
    assert!(
        !actions.is_empty(),
        "every action that changed bytes on the air must be logged (I-T3)"
    );
    // The record names the true actor, carries the changed fields and the magnitude, and
    // lives on a ground-truth channel.
    let a = &actions[0];
    assert_eq!(a.action, "FalsifyOutgoing");
    assert_eq!(a.fields, vec!["position".to_string()]);
    assert!(a.changed_bytes_on_air);
    // 25 m on both axes at intensity 1 — `run.py :: attack_claim`, ConstPosOffset.
    assert_eq!(a.magnitude, Some(35.355));
    assert!(
        out.records
            .iter()
            .all(|(_, r)| r.channel != "gt.attack.action"
                || r.visibility == v2xw_core::ctx::Visibility::Gt),
        "the attack channel is ground-truth tainted and must stay so"
    );

    // And it is visible in a recording: the harness writes one, and reading it back finds
    // the attack channel intact.
    let dir = std::env::temp_dir().join("v2xw-threat-attack-recording");
    let path = dir.join("recording.mcap");
    let mut with_record = opts.clone();
    with_record.record = Some(path.clone());
    let recorded = sim::run(&with_record);
    let bytes = std::fs::read(&path).expect("the recording must be readable");
    let mut reader = v2xw_record::Reader::open_bytes(bytes).expect("the recording must open");
    let verify = reader.verify().expect("the recording must verify");
    eprintln!(
        "recording: {} records, integrity {}, gt.attack.action {}, det.observation {}",
        verify.records,
        verify.integrity_verified(),
        recorded.count_on("gt.attack.action"),
        recorded.count_on("det.observation"),
    );
    assert!(verify.integrity_verified(), "the recording must verify");
    assert_eq!(
        recorded.count_on("gt.attack.action"),
        out.count_on("gt.attack.action"),
        "recording a run must not change it"
    );
    assert!(recorded.count_on("gt.attack.action") > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every falsification family reaches the air with the fields its rendering claims.
#[test]
fn every_attack_family_reaches_the_air() {
    let families = [
        (AttackKind::ConstPos, "position"),
        (AttackKind::RandomPos, "position"),
        (AttackKind::Teleport, "position"),
        (AttackKind::SineWavePos, "position"),
        (AttackKind::ConstSpeedOffset, "speed"),
        (AttackKind::RandomSpeed, "speed"),
        (AttackKind::StopAndGo, "speed"),
        (AttackKind::ReversedHeading, "heading"),
        (AttackKind::HeadingOffset, "heading"),
        (AttackKind::DelayedMessages, "generation_time"),
        (AttackKind::DataReplay, "generation_time"),
        (AttackKind::OutOfOrder, "generation_time"),
        (AttackKind::DoS, "repetitions"),
        (AttackKind::DoSRandom, "repetitions"),
        (AttackKind::InvalidSignature, "signature"),
        (AttackKind::ExpiredCert, "certificate"),
        (AttackKind::NotYetValid, "certificate"),
        (AttackKind::Disruptive, "position"),
        (AttackKind::EventualStop, "position"),
        (AttackKind::SlowDrift, "position"),
        (AttackKind::AlongRoadOffset, "position"),
        (AttackKind::VruImpersonation, "station_type"),
    ];
    for (kind, field) in families {
        let mut opts = SimOptions::new(2, 14_400.0, 0.5).with_attack(kind);
        opts.duration_s = 20.0;
        let out = sim::run(&opts);
        let actions: Vec<GtAttackAction> = out.decode("gt.attack.action");
        assert!(
            !actions.is_empty(),
            "{kind} must produce at least one logged action"
        );
        let touched: std::collections::BTreeSet<String> = actions
            .iter()
            .flat_map(|a| a.fields.iter().cloned())
            .collect();
        assert!(
            touched.contains(field),
            "{kind} must touch {field}; it touched {touched:?}"
        );
        eprintln!("{kind:24} actions {:6}  fields {touched:?}", actions.len());
    }
    // Sybil is the one whose signature is extra *frames* rather than an edited field: the
    // ghosts are separate transmissions from the attacker's other pseudonyms.
    let mut opts = SimOptions::new(2, 14_400.0, 0.5).with_attack(AttackKind::Sybil);
    opts.duration_s = 20.0;
    let sybil = sim::run(&opts);
    let honest = sim::run(&{
        let mut o = SimOptions::new(2, 14_400.0, 0.0);
        o.duration_s = 20.0;
        o.attacks.clear();
        o
    });
    eprintln!(
        "Sybil: {} frames against {} for the same fleet running honestly",
        sybil.frames, honest.frames
    );
    assert!(
        sybil.frames > honest.frames,
        "a Sybil attacker must put its ghosts on the air"
    );
}

/// The detectors run on received messages and produce the records the metrics crate
/// scores, and the rates come out of that provider rather than out of a second counter.
#[test]
fn the_detectors_run_on_receptions_and_produce_real_rates() {
    for (label, opts) in [
        (
            "realistic",
            SimOptions::new(3, 14_400.0, 0.25).with_attack(AttackKind::ConstPosOffset),
        ),
        (
            "ideal",
            SimOptions::new(3, 14_400.0, 0.25)
                .with_attack(AttackKind::ConstPosOffset)
                .ideal(),
        ),
    ] {
        let out = sim::run(&opts);
        eprintln!("=== {label} ===\n{}", sim::summarize(&out));

        assert!(out.delivered > 0, "nodes must have delivered messages");
        let obs: Vec<DetObservation> = out.decode("det.observation");
        assert!(
            !obs.is_empty(),
            "the detector suite must have written det.observation records"
        );
        // Every observation names its subject by pseudonym digest and nothing else: there
        // is no actor id on the channel, which is what makes the join a declaration.
        assert!(obs.iter().all(|o| o.subject.len() == 16));
        assert!(out.reports > 0, "reports must have been filed");

        let (report, vehicle) = sim::score(&out);
        let level = v2xw_metrics::stats::ConfidenceLevel::P95;
        eprintln!(
            "{label} report-level  recall {:?}  fpr {:?}  precision {:?}  ({} subjects)",
            report.recall(1, level).point(),
            report.fpr(1, level).point(),
            report.precision(1, level).point(),
            report.tp + report.fp + report.fn_ + report.tn,
        );
        eprintln!(
            "{label} vehicle-level recall {:?}  fpr {:?}",
            vehicle.recall(1, level).point(),
            vehicle.fpr(1, level).point(),
        );
        assert!(report.tp > 0, "an attacker must have been reported");
        assert_eq!(
            report.tp + report.fp + report.fn_ + report.tn,
            out.subject_actor.len() as u64,
            "the population must be every identity that transmitted"
        );
    }
}

/// Per-attack detection rates at a known attacker fraction, printed as measured.
///
/// The ideal regime: noise-free belief, a hard reception disc and no verification budget,
/// so what the numbers measure is the detector suite against the attacker's lie rather
/// than the GNSS model or the channel. `legacy_engine_compare.rs` is where these are put
/// beside the frozen engine's.
#[test]
fn detection_rates_per_attack_type() {
    let mut detected = 0usize;
    let mut table: Vec<String> = Vec::new();
    for kind in AttackKind::LEGACY_CATALOG {
        let mut opts = SimOptions::new(4, 14_400.0, 0.25).with_attack(kind).ideal();
        opts.duration_s = 30.0;
        let out = sim::run(&opts);
        let (report, vehicle) = sim::score(&out);
        let level = v2xw_metrics::stats::ConfidenceLevel::P95;
        let recall = report.recall(1, level).point().unwrap_or(0.0);
        let fpr = report.fpr(1, level).point().unwrap_or(0.0);
        let lead = out
            .firings_on_attacker
            .iter()
            .max_by_key(|(_, v)| **v)
            .map(|(k, v)| format!("{k}={v}"))
            .unwrap_or_else(|| "-".to_string());
        table.push(format!(
            "{kind:20} att {:2}/{:2}  recall {recall:5.3}  fpr {fpr:5.3}  reports {:6}  \
             revoked {:2}  lead {lead}",
            out.attackers,
            out.nodes,
            out.reports,
            vehicle.tp + vehicle.fp,
        ));
        if recall > 0.0 {
            detected += 1;
        }
    }
    for line in &table {
        eprintln!("{line}");
    }
    assert!(
        detected >= 17,
        "most of the legacy catalog must be detectable: {detected}/21"
    );
}

/// An honest fleet: nothing is falsified, nothing is logged on the attack channel, and
/// nobody is revoked.
///
/// This is the check the detection tests cannot make. A detector that fired on everything
/// would pass every assertion above and fail here, which is the point.
#[test]
fn an_honest_fleet_is_not_accused_wholesale() {
    let mut opts = SimOptions::new(5, 14_400.0, 0.0).ideal();
    opts.attacks.clear();
    let out = sim::run(&opts);
    eprintln!("{}", sim::summarize(&out));
    assert_eq!(out.attackers, 0);
    assert_eq!(out.falsified_frames, 0);
    assert_eq!(out.count_on("gt.attack.action"), 0);
    let (report, vehicle) = sim::score(&out);
    assert_eq!(report.tp, 0);
    assert_eq!(vehicle.tp, 0);
    eprintln!(
        "honest fleet: {} nodes, {} receptions, {} reports, {} revocations, \
         report-level fp {} tn {}",
        out.nodes,
        out.receptions,
        out.reports,
        out.revocations,
        report.fp,
        report.tn
    );
    assert_eq!(
        out.revocations, 0,
        "an honest fleet must not be revoked: {} revocations",
        out.revocations
    );
}

/// **A diagnosis, not a tuning.** `acceptanceRangeThreshold` compares a claimed position
/// against the *receiver's configured range*; the medium-tier propagation model closes
/// links well beyond the 500 m the legacy port carries as that range, so honest distant
/// senders score above 1.
///
/// The run measures how far frames actually travelled, then re-runs with the range set to
/// that reach. If the diagnosis is right the false accusations disappear and no true
/// positive is lost; if it is wrong, they do not.
#[test]
fn the_acceptance_range_check_accuses_honest_senders_when_its_range_is_below_the_radio_reach() {
    let opts = SimOptions::new(6, 14_400.0, 0.25).with_attack(AttackKind::ConstPosOffset);
    let before = sim::run(&opts);
    let art = "acceptanceRangeThreshold";
    let benign_before = before.firings_on_benign.get(art).copied().unwrap_or(0);
    eprintln!(
        "nominal range {:.0} m; frames were received out to {:.0} m ({} of {} receptions \
         beyond the nominal range); {art} fired {benign_before} times against honest \
         vehicles",
        before.nominal_range_m,
        before.max_rx_distance_m,
        before.rx_beyond_nominal_range,
        before.receptions,
    );
    assert!(
        before.max_rx_distance_m > before.nominal_range_m,
        "the propagation model must actually reach beyond the configured range for this \
         to be the phenomenon it claims to be"
    );
    assert!(
        benign_before > 0,
        "and the check must actually be accusing honest senders"
    );

    let mut after_opts = opts.clone();
    after_opts.radio_range_m = before.max_rx_distance_m.ceil();
    let after = sim::run(&after_opts);
    let benign_after = after.firings_on_benign.get(art).copied().unwrap_or(0);
    let (rb, _) = sim::score(&before);
    let (ra, _) = sim::score(&after);
    eprintln!(
        "range {:.0} m -> {:.0} m: {art} against honest {benign_before} -> {benign_after}; \
         report-level fp {} -> {}, tp {} -> {}",
        before.nominal_range_m, after_opts.radio_range_m, rb.fp, ra.fp, rb.tp, ra.tp
    );
    assert_eq!(
        benign_after, 0,
        "setting the check's range to the radio's actual reach must remove the false \
         accusations it was making"
    );
    assert!(
        ra.tp >= rb.tp,
        "and must not cost a true positive: {} -> {}",
        rb.tp,
        ra.tp
    );
}

/// Two runs of one configuration produce the same record stream, byte for byte.
#[test]
fn the_loop_is_reproducible() {
    let mut opts = SimOptions::new(7, 14_400.0, 0.25).with_attack(AttackKind::RandomPos);
    opts.duration_s = 30.0;
    let a = sim::run(&opts);
    let b = sim::run(&opts);
    assert_eq!(a.digest, b.digest, "two runs must agree");
    assert_eq!(a.records.len(), b.records.len());
    assert_eq!(a.reports, b.reports);
    eprintln!(
        "digest {} over {} records, {} reports",
        &a.digest[..16],
        a.records.len(),
        a.reports
    );
}

/// The attacker fraction is what it says, and an honest fleet has no attack actions.
#[test]
fn the_attacker_fraction_is_what_it_says() {
    let mut counts: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    for f in [0.0, 0.1, 0.25, 0.5, 1.0] {
        let mut opts = SimOptions::new(8, 14_400.0, f).with_attack(AttackKind::ConstPosOffset);
        opts.duration_s = 20.0;
        let out = sim::run(&opts);
        counts.insert(
            format!("{f:.2}"),
            (out.attackers, out.nodes, out.falsified_frames),
        );
        eprintln!(
            "fraction {f:.2}: {} of {} nodes are attackers, {} falsified frames, \
             {} attack actions logged",
            out.attackers,
            out.nodes,
            out.falsified_frames,
            out.count_on("gt.attack.action"),
        );
    }
    assert_eq!(counts["0.00"].0, 0);
    assert_eq!(counts["0.00"].2, 0);
    assert_eq!(counts["1.00"].0, counts["1.00"].1);
    assert!(counts["0.50"].0 > counts["0.10"].0);
}
