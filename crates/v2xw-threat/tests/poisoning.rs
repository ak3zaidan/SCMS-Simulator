//! Report poisoning, the compromised road-side unit, and what the authority's two
//! defences are actually worth against them.
//!
//! Every test here is a measurement rather than a demonstration. The interesting results
//! are the negative ones: the legacy correlation gate at k = 3 does **not** stop three
//! colluders, and an authority that exempts infrastructure from its reporter gate has no
//! defence against a unit on the reporting path. Both are asserted, because a threat model
//! that only contains attacks its own defences stop is a threat model that measures
//! nothing.

use v2xw_core::ids::NodeId;
use v2xw_core::time::{NS_PER_S, SimTime};
use v2xw_threat::attack::{AttackAction, Attacker, AttackerView, Emission, HonestClaim};
use v2xw_threat::attack_rsu::{CompromisedRsu, ForwardDecision, RsuAttackKind, RsuAttackParams};
use v2xw_threat::capability::{AttackSchedule, Capabilities};
use v2xw_threat::ctx::CollectingCtx;
use v2xw_threat::detect::{DetectorId, Fingerprint, Observation, Verdict};
use v2xw_threat::ma::{LegacyWindow, MaAction, MaParams, MaPipeline};
use v2xw_threat::obs::{
    ObservedKind, ObservedMessage, SelfBelief, StationType, VerificationState,
};
use v2xw_threat::poison::{PoisonParams, ReportPoisoner};
use v2xw_threat::records::MaCaseRecord;
use v2xw_threat::report::{Evidence, MisbehaviourReport};
use v2xw_threat::resolve::{Authority, CaseOutcome, ResolutionParams, TwoAuthorityResolution};

const VICTIM: &str = "aaaaaaaaaaaaaaaa";
const V_DIGEST: [u8; 8] = [0xAA; 8];

fn always() -> AttackSchedule {
    AttackSchedule {
        from: 0,
        to: u64::MAX,
        ..AttackSchedule::default()
    }
}

fn heard(signer: [u8; 8], t: SimTime, verification: VerificationState) -> ObservedMessage {
    ObservedMessage {
        signer,
        kind: ObservedKind::Beacon,
        received_at: t,
        claimed_generation_time: t,
        claimed_x_m: 30.0,
        claimed_y_m: 0.0,
        claimed_speed_mps: 15.0,
        claimed_heading_rad: 0.0,
        claimed_pos_confidence_m: 2.0,
        repetitions: 1,
        cert_valid_from: 0,
        cert_valid_to: u64::MAX,
        station_type: StationType::Vehicle,
        verification,
    }
}

fn view<'a>(rx: &'a [ObservedMessage], t: SimTime) -> AttackerView<'a> {
    let honest = HonestClaim {
        x_m: 0.0,
        y_m: 0.0,
        speed_mps: 15.0,
        heading_rad: 0.0,
    };
    AttackerView {
        own_rx: rx,
        own_credentials: &[],
        crl_revocations_seen: None,
        own_belief: SelfBelief {
            node: NodeId::new(2),
            believed_time: t,
            x_m: 0.0,
            y_m: 0.0,
            radio_range_m: 500.0,
        },
        honest,
        believed_time: t,
    }
}

/// A genuine report: a real detector verdict against a real subject.
fn genuine_report(id: &str, reporter: u32, subject: &str, t: SimTime) -> MisbehaviourReport {
    let mut f = Fingerprint::default();
    f.set(DetectorId::PositionJump, 2.0);
    let v = Verdict {
        subject: subject.to_string(),
        fingerprint: f,
        fired: vec![Observation {
            detector: DetectorId::PositionJump,
            score: 2.0,
            subject: subject.to_string(),
            at: t,
        }],
    };
    MisbehaviourReport::from_verdict(
        id,
        NodeId::new(reporter),
        format!("rep{reporter:04}"),
        &v,
        &Evidence::at(t, t, 2.0),
    )
    .expect("something fired")
}

/// Four reports from three distinct reporters over four distinct seconds: exactly the
/// legacy operating point's revocation condition.
fn a_coalitions_worth_of_evidence(subject: &str) -> Vec<MisbehaviourReport> {
    vec![
        genuine_report("c0", 1, subject, 0),
        genuine_report("c1", 2, subject, NS_PER_S),
        genuine_report("c2", 3, subject, 2 * NS_PER_S),
        genuine_report("c3", 1, subject, 3 * NS_PER_S),
    ]
}

fn cases(ctx: &CollectingCtx) -> Vec<MaCaseRecord> {
    ctx.on_channel("ma.case")
        .iter()
        .map(|r| serde_json::from_slice::<MaCaseRecord>(&r.json).expect("ma.case decodes"))
        .collect()
}

// ---------------------------------------------------------------------------------------
// The poisoner
// ---------------------------------------------------------------------------------------

fn poisoner(digest: &str, params: PoisonParams, caps: Capabilities) -> ReportPoisoner {
    ReportPoisoner::new(NodeId::new(2), params, caps, always()).tap_digest(digest)
}

/// A tiny extension so the tests can set the digest after construction without repeating
/// the four-argument constructor.
trait TapDigest {
    fn tap_digest(self, digest: &str) -> Self;
}

impl TapDigest for ReportPoisoner {
    fn tap_digest(mut self, digest: &str) -> Self {
        self.set_own_digest(digest);
        self
    }
}

#[test]
fn a_poisoner_files_only_against_victims_it_has_heard() {
    let mut ctx = CollectingCtx::new(11);
    let mut a = poisoner(
        "cafecafecafecafe",
        PoisonParams {
            victim_pct: 1.0,
            report_prob: 1.0,
            ..PoisonParams::default()
        },
        Capabilities::insider(20),
    );
    // Nothing heard: nothing filed, and no claim that anything happened.
    let mut out = Emission::honest([0xCA; 8], view(&[], 0).honest, 0, 0, u64::MAX);
    let actions = a.act(&mut ctx, &view(&[], 0), &mut out);
    assert!(actions.is_empty());
    assert_eq!(a.filed(), 0);

    // Three senders heard, three reports filed, one per victim per interval.
    let rx = vec![
        heard([1; 8], NS_PER_S, VerificationState::Valid),
        heard([2; 8], NS_PER_S, VerificationState::Valid),
        heard([3; 8], NS_PER_S, VerificationState::Valid),
    ];
    let v = view(&rx, NS_PER_S);
    a.observe(&mut ctx, &v);
    assert_eq!(a.heard(), 3);
    let actions = a.act(&mut ctx, &v, &mut out);
    assert_eq!(actions.len(), 3);
    assert_eq!(a.filed(), 3);
    for action in &actions {
        match action {
            AttackAction::ForgeReport { subject } => {
                assert_eq!(subject.len(), 16, "a subject is a digest in hex");
            }
            other => panic!("unexpected action {other:?}"),
        }
    }
    let reports = a.take_reports();
    assert_eq!(reports.len(), 3);
    assert!(a.take_reports().is_empty(), "the queue is drained");
    // Subjects in digest order, so the report sequence is reproducible.
    let subjects: Vec<&str> = reports
        .iter()
        .map(|r| r.subject_cert_digest.as_str())
        .collect();
    let mut sorted = subjects.clone();
    sorted.sort_unstable();
    assert_eq!(subjects, sorted);
}

#[test]
fn a_poisoner_behaves_perfectly_on_the_air() {
    // The whole point: nothing the local detector suite can see. If a poisoner also
    // falsified its claims, the run would credit the authority's defences with a
    // robustness the detectors had actually provided.
    let mut ctx = CollectingCtx::new(11);
    let mut a = poisoner(
        "cafecafecafecafe",
        PoisonParams {
            victim_pct: 1.0,
            report_prob: 1.0,
            ..PoisonParams::default()
        },
        Capabilities::insider(20),
    );
    let rx = vec![heard(V_DIGEST, 0, VerificationState::Valid)];
    let v = view(&rx, 0);
    a.observe(&mut ctx, &v);
    let honest = v.honest;
    let mut out = Emission::honest([0xCA; 8], honest, 0, 0, u64::MAX);
    let before = out.clone();
    a.act(&mut ctx, &v, &mut out);
    assert_eq!(out, before, "the poisoner edited the air");
    assert!(!v2xw_threat::is_falsified(
        &honest,
        &out,
        0,
        StationType::Vehicle
    ));
}

#[test]
fn a_poisoner_without_credentials_files_nothing() {
    let mut ctx = CollectingCtx::new(11);
    let mut a = poisoner(
        "cafecafecafecafe",
        PoisonParams {
            victim_pct: 1.0,
            report_prob: 1.0,
            ..PoisonParams::default()
        },
        Capabilities::outsider(),
    );
    let rx = vec![heard(V_DIGEST, 0, VerificationState::Valid)];
    let v = view(&rx, 0);
    a.observe(&mut ctx, &v);
    let mut out = Emission::honest([0xCA; 8], v.honest, 0, 0, u64::MAX);
    assert!(a.act(&mut ctx, &v, &mut out).is_empty());
    assert_eq!(a.filed(), 0, "an outsider's report is discarded anyway");
}

#[test]
fn a_sender_whose_signature_did_not_verify_is_not_worth_framing() {
    let mut ctx = CollectingCtx::new(11);
    let mut a = poisoner(
        "cafecafecafecafe",
        PoisonParams {
            victim_pct: 1.0,
            report_prob: 1.0,
            ..PoisonParams::default()
        },
        Capabilities::insider(20),
    );
    let rx = vec![
        heard([1; 8], 0, VerificationState::BadSignature),
        heard([2; 8], 0, VerificationState::Unverified),
        heard([3; 8], 0, VerificationState::UnknownCertificate),
    ];
    let v = view(&rx, 0);
    a.observe(&mut ctx, &v);
    assert_eq!(a.heard(), 0, "only verified senders are in the victim pool");
}

#[test]
fn a_forged_report_is_not_separable_by_structural_zeros() {
    let mut ctx = CollectingCtx::new(11);
    let mut a = poisoner(
        "cafecafecafecafe",
        PoisonParams {
            victim_pct: 1.0,
            report_prob: 1.0,
            ..PoisonParams::default()
        },
        Capabilities::insider(20),
    );
    let rx = vec![heard(V_DIGEST, 0, VerificationState::Valid)];
    let v = view(&rx, 0);
    a.observe(&mut ctx, &v);
    let mut out = Emission::honest([0xCA; 8], v.honest, 0, 0, u64::MAX);
    a.act(&mut ctx, &v, &mut out);
    let report = a.take_reports().remove(0);
    let columns: std::collections::BTreeMap<&str, f64> = report
        .detnorm
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    assert!(columns["positionSpeedInconsistency"] >= 1.05);
    assert!(columns["sybilCoLocation"] > 0.0, "a free collusion oracle");
    assert!(columns["beaconFrequency"] > 0.0, "a free collusion oracle");
    assert!(columns["staleOrReplay"] > 0.0);
    assert_eq!(report.subject_cert_digest, VICTIM);
    assert_eq!(report.reporter_cert_digest, "cafecafecafecafe");
}

#[test]
fn a_flooding_poisoner_files_the_multiple_it_declared() {
    let mut ctx = CollectingCtx::new(11);
    let mut a = poisoner(
        "cafecafecafecafe",
        PoisonParams {
            victim_pct: 1.0,
            report_prob: 1.0,
            ..PoisonParams::default()
        }
        .flooding(5),
        Capabilities::insider(20),
    );
    let rx = vec![heard(V_DIGEST, 0, VerificationState::Valid)];
    let v = view(&rx, 0);
    a.observe(&mut ctx, &v);
    let mut out = Emission::honest([0xCA; 8], v.honest, 0, 0, u64::MAX);
    let actions = a.act(&mut ctx, &v, &mut out);
    assert_eq!(actions.len(), 5);
    assert_eq!(a.take_reports().len(), 5);
}

#[test]
fn no_victims_means_no_reports() {
    let mut ctx = CollectingCtx::new(11);
    let mut a = poisoner(
        "cafecafecafecafe",
        PoisonParams {
            victim_pct: 0.0,
            report_prob: 1.0,
            ..PoisonParams::default()
        },
        Capabilities::insider(20),
    );
    let rx = vec![heard(V_DIGEST, 0, VerificationState::Valid)];
    let v = view(&rx, 0);
    a.observe(&mut ctx, &v);
    let mut out = Emission::honest([0xCA; 8], v.honest, 0, 0, u64::MAX);
    assert!(a.act(&mut ctx, &v, &mut out).is_empty());
    assert!(a.victims().is_empty());
}

// ---------------------------------------------------------------------------------------
// What the authority's defences are worth
// ---------------------------------------------------------------------------------------

#[test]
fn one_reporter_cannot_revoke_however_many_reports_it_files() {
    // The rate limit and the k-of-n gate, doing their jobs: forty reports from one
    // certificate are one reporter.
    let mut ctx = CollectingCtx::new(1);
    let mut ma = LegacyWindow::legacy_defaults();
    for i in 0..40u64 {
        let r = genuine_report(&format!("s{i}"), 1, VICTIM, (i % 10) * NS_PER_S);
        ma.on_report(&mut ctx, &r);
    }
    assert!(!ma.is_revoked(VICTIM));
    assert!(!ma.trusted("rep0001"), "the report budget caught the spray");
}

#[test]
fn three_colluders_defeat_the_legacy_correlation_gate_on_their_own() {
    // The negative result that matters: k = 3 distinct trusted reporters over four
    // distinct seconds is exactly what three colluders can produce, so the legacy gate
    // alone does not resist report poisoning. This is what the resolution stage is for.
    let mut ctx = CollectingCtx::new(1);
    let mut ma = LegacyWindow::legacy_defaults();
    let mut decided = Vec::new();
    for r in a_coalitions_worth_of_evidence(VICTIM) {
        decided.extend(ma.on_report(&mut ctx, &r));
    }
    assert_eq!(
        decided,
        vec![MaAction::Revoke {
            subject: VICTIM.to_string()
        }]
    );
    assert!(ma.is_revoked(VICTIM), "an honest vehicle was revoked");
}

#[test]
fn the_second_authority_is_what_stops_the_revocation() {
    // The same four reports, the same correlation gate — and a resolution stage that needs
    // both linkage authorities. With only one able to resolve the subject, the case stalls
    // at an investigation and no revocation issues.
    let mut ctx = CollectingCtx::new(1);
    let mut ma = TwoAuthorityResolution::scms_defaults();
    ma.declare_linkage(VICTIM, Authority::La1);
    let mut decided = Vec::new();
    for r in a_coalitions_worth_of_evidence(VICTIM) {
        decided.extend(ma.on_report(&mut ctx, &r));
    }
    assert_eq!(
        decided,
        vec![MaAction::Investigate {
            subject: VICTIM.to_string()
        }]
    );
    assert!(!ma.is_revoked(VICTIM));
    assert_eq!(ma.pending_cases(), vec![VICTIM]);
    assert_eq!(ma.ingested(), 4);
    assert_eq!(ma.dismissed(), 0);
    let recorded = cases(&ctx);
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].outcome,
        CaseOutcome::UnresolvedSingleAuthority.as_str()
    );
    assert_eq!(recorded[0].authorities_resolved, 1);
    assert_eq!(recorded[0].authorities_required, 2);
    assert_eq!(recorded[0].trusted_reporters, 3);

    // The same run with a single-authority protocol revokes, which is the comparison the
    // knob exists for.
    let mut ctx = CollectingCtx::new(1);
    let mut single = TwoAuthorityResolution::new(ResolutionParams::single_authority());
    single.declare_linkage(VICTIM, Authority::La1);
    let mut decided = Vec::new();
    for r in a_coalitions_worth_of_evidence(VICTIM) {
        decided.extend(single.on_report(&mut ctx, &r));
    }
    assert_eq!(
        decided,
        vec![MaAction::Revoke {
            subject: VICTIM.to_string()
        }]
    );
    assert!(single.is_revoked(VICTIM));
}

#[test]
fn a_subject_no_authority_can_resolve_is_reported_rather_than_assumed() {
    let mut ctx = CollectingCtx::new(1);
    let mut ma = TwoAuthorityResolution::scms_defaults();
    for r in a_coalitions_worth_of_evidence(VICTIM) {
        ma.on_report(&mut ctx, &r);
    }
    assert!(!ma.is_revoked(VICTIM));
    assert!(ma.undeclared_subjects().contains(VICTIM));
    assert_eq!(
        cases(&ctx)[0].outcome,
        CaseOutcome::UnresolvedUndeclared.as_str()
    );
}

#[test]
fn a_late_resolution_still_revokes() {
    // An investigation is not a dismissal: when the second authority answers, the case
    // completes on the next tick.
    let mut ctx = CollectingCtx::new(1);
    let mut ma = TwoAuthorityResolution::scms_defaults();
    ma.declare_linkage(VICTIM, Authority::La1);
    for r in a_coalitions_worth_of_evidence(VICTIM) {
        ma.on_report(&mut ctx, &r);
    }
    assert!(!ma.is_revoked(VICTIM));
    ma.declare_linkage(VICTIM, Authority::La2);
    let acted = ma.on_tick(&mut ctx, 4 * NS_PER_S);
    assert_eq!(
        acted,
        vec![MaAction::Revoke {
            subject: VICTIM.to_string()
        }]
    );
    assert!(ma.is_revoked(VICTIM));
    assert!(ma.pending_cases().is_empty());
}

#[test]
fn a_report_below_its_own_firing_threshold_is_dismissed_at_ingestion() {
    let mut ctx = CollectingCtx::new(1);
    let mut ma = TwoAuthorityResolution::scms_defaults();
    let mut weak = genuine_report("w0", 1, VICTIM, 0);
    weak.detector_score = 0.4;
    let acted = ma.on_report(&mut ctx, &weak);
    assert_eq!(
        acted,
        vec![MaAction::Dismiss {
            subject: VICTIM.to_string()
        }]
    );
    assert_eq!(ma.dismissed(), 1);
    assert_eq!(ma.ingested(), 0);
    assert_eq!(
        cases(&ctx)[0].outcome,
        CaseOutcome::DismissedBelowThreshold.as_str()
    );
    // The report still reached the authority: `ma.report` is the transport's record.
    assert_eq!(ctx.on_channel("ma.report").len(), 1);
}

#[test]
fn a_report_that_attaches_nothing_is_dismissed_at_ingestion() {
    let mut ctx = CollectingCtx::new(1);
    let mut ma = TwoAuthorityResolution::scms_defaults();
    let mut empty = genuine_report("e0", 1, VICTIM, 0);
    empty.evidence_msg_refs.clear();
    ma.on_report(&mut ctx, &empty);
    assert_eq!(
        cases(&ctx)[0].outcome,
        CaseOutcome::DismissedNoEvidence.as_str()
    );

    let mut ctx = CollectingCtx::new(1);
    let mut ma = TwoAuthorityResolution::scms_defaults();
    let mut no_reason = genuine_report("e1", 1, VICTIM, 0);
    no_reason.reason_codes.clear();
    ma.on_report(&mut ctx, &no_reason);
    assert_eq!(
        cases(&ctx)[0].outcome,
        CaseOutcome::DismissedNoEvidence.as_str()
    );
}

#[test]
fn turning_the_reporter_gate_off_is_how_a_run_measures_it() {
    // With the gate off a single revoked reporter's evidence still counts, which is the
    // baseline every collusion number should be reported against.
    let mut off = LegacyWindow::new(MaParams {
        defence: false,
        ..MaParams::default()
    });
    off.trust_infrastructure("rsu0");
    assert!(off.trusted("anything-at-all"));
    let on = LegacyWindow::legacy_defaults();
    assert!(on.trusted("never-seen"), "an unknown reporter starts trusted");
}

// ---------------------------------------------------------------------------------------
// The compromised road-side unit
// ---------------------------------------------------------------------------------------

fn rsu(kind: RsuAttackKind) -> CompromisedRsu {
    CompromisedRsu::new(
        NodeId::new(7),
        RsuAttackParams::new(kind, "rsu0"),
        always(),
    )
}

#[test]
fn a_compromised_unit_retargets_a_report_at_an_innocent_subject() {
    let mut ctx = CollectingCtx::new(5);
    let mut unit = rsu(RsuAttackKind::PoisonForwardedReports);
    // It has heard one innocent vehicle.
    let rx = vec![heard(V_DIGEST, 0, VerificationState::Valid)];
    unit.observe(&mut ctx, &view(&rx, 0));
    assert_eq!(unit.heard(), 1);

    let genuine = genuine_report("g0", 1, "beefbeefbeefbeef", NS_PER_S);
    let (decision, actions) = unit.on_forward(&mut ctx, &genuine, NS_PER_S);
    match decision {
        ForwardDecision::Replace(replacement) => {
            assert_eq!(replacement.subject_cert_digest, VICTIM);
            assert_eq!(replacement.reporter_cert_digest, "rsu0");
            assert!(replacement.detector_score >= 1.05, "plausible evidence");
        }
        other => panic!("unexpected decision {other:?}"),
    }
    assert_eq!(unit.retargeted(), 1);
    assert_eq!(unit.dropped(), 0);
    assert!(matches!(actions[0], AttackAction::ForgeReport { .. }));
}

#[test]
fn a_compromised_unit_with_nobody_to_frame_forwards() {
    // Nothing heard, so nobody to frame: it forwards, and the counters say the attack did
    // not fire rather than the run reporting a poisoning that never happened.
    let mut ctx = CollectingCtx::new(5);
    let mut unit = rsu(RsuAttackKind::PoisonForwardedReports);
    let genuine = genuine_report("g0", 1, "beefbeefbeefbeef", 0);
    let (decision, actions) = unit.on_forward(&mut ctx, &genuine, 0);
    assert_eq!(decision, ForwardDecision::Forward);
    assert!(actions.is_empty());
    assert_eq!(unit.retargeted(), 0);
    assert_eq!(unit.forwarded(), 1);
}

#[test]
fn a_compromised_unit_never_frames_the_subject_of_the_report_it_was_given() {
    let mut ctx = CollectingCtx::new(5);
    let mut unit = rsu(RsuAttackKind::PoisonForwardedReports);
    // The only digest it has heard IS the report's subject, so there is nobody else.
    let rx = vec![heard(V_DIGEST, 0, VerificationState::Valid)];
    unit.observe(&mut ctx, &view(&rx, 0));
    let genuine = genuine_report("g0", 1, VICTIM, 0);
    let (decision, _) = unit.on_forward(&mut ctx, &genuine, 0);
    assert_eq!(decision, ForwardDecision::Forward);
}

#[test]
fn a_compromised_unit_suppresses_the_evidence_that_would_have_revoked_an_attacker() {
    // Four reports that revoke when they arrive, and do not when the unit on the path
    // drops them. The attacker they name is never revoked — and no detector anywhere was
    // wrong.
    let subject = "beefbeefbeefbeef";
    let mut ctx = CollectingCtx::new(5);
    let mut direct = LegacyWindow::legacy_defaults();
    for r in a_coalitions_worth_of_evidence(subject) {
        direct.on_report(&mut ctx, &r);
    }
    assert!(direct.is_revoked(subject), "the evidence is sufficient");

    let mut ctx = CollectingCtx::new(5);
    let mut unit = rsu(RsuAttackKind::SuppressForwardedReports);
    let behind_the_unit = LegacyWindow::legacy_defaults();
    for r in a_coalitions_worth_of_evidence(subject) {
        let (decision, actions) = unit.on_forward(&mut ctx, &r, r.ingest_time);
        match decision {
            ForwardDecision::Drop => {
                assert!(matches!(actions[0], AttackAction::SuppressReport { .. }));
                assert!(
                    !actions[0].changed_bytes_on_air(),
                    "suppression removes bytes, it does not change them"
                );
            }
            other => panic!("unexpected decision {other:?}"),
        }
    }
    assert_eq!(unit.dropped(), 4);
    assert!(
        !behind_the_unit.is_revoked(subject),
        "the authority never saw the evidence"
    );
}

#[test]
fn the_air_facing_kinds_declare_which_infrastructure_message_they_falsified() {
    for (kind, message, field) in [
        (RsuAttackKind::FalseSpat, "spat", "phase"),
        (RsuAttackKind::FalseMap, "map", "lane-connection"),
        (RsuAttackKind::FalseCrl, "crl", "entries"),
        (RsuAttackKind::FalseCtl, "ctl", "entries"),
    ] {
        let mut ctx = CollectingCtx::new(5);
        let mut unit = rsu(kind);
        let v = view(&[], 0);
        let mut out = Emission::honest([0x77; 8], v.honest, 0, 0, u64::MAX);
        let actions = unit.act(&mut ctx, &v, &mut out);
        assert_eq!(out.station_type, StationType::Rsu, "{kind}");
        assert_eq!(out.infra.len(), 1, "{kind}");
        assert_eq!(out.infra[0].message, message);
        assert_eq!(out.infra[0].field, field);
        match &actions[0] {
            AttackAction::FalsifyInfrastructure {
                message: m,
                field: f,
                ..
            } => {
                assert_eq!(m, message);
                assert_eq!(f, field);
            }
            other => panic!("unexpected action {other:?}"),
        }
        // Its own claim is honest: a unit does not move, and this attack is not about its
        // position.
        assert_eq!(out.x_m, v.honest.x_m);
        assert_eq!(out.y_m, v.honest.y_m);
    }
}

#[test]
fn a_reporting_path_kind_does_nothing_on_the_air_but_still_declares_its_station_type() {
    for kind in [
        RsuAttackKind::SuppressForwardedReports,
        RsuAttackKind::PoisonForwardedReports,
    ] {
        let mut ctx = CollectingCtx::new(5);
        let mut unit = rsu(kind);
        let v = view(&[], 0);
        let mut out = Emission::honest([0x77; 8], v.honest, 0, 0, u64::MAX);
        let actions = unit.act(&mut ctx, &v, &mut out);
        assert!(actions.is_empty(), "{kind}");
        assert!(out.infra.is_empty(), "{kind}");
        assert_eq!(out.station_type, StationType::Rsu);
    }
}

#[test]
fn an_authority_that_exempts_infrastructure_exempts_a_compromised_unit_too() {
    // The finding, stated as a test rather than as a comment: `trust_infrastructure` makes
    // a reporter immune to the budget and the reputation cap, and a compromised unit is
    // infrastructure. It still cannot reach k = 3 on its own — one unit is one certificate —
    // so the exemption is survivable alone and not in a coalition.
    let mut ctx = CollectingCtx::new(1);
    let mut ma = LegacyWindow::legacy_defaults();
    ma.trust_infrastructure("rsu0");
    for i in 0..40u64 {
        let mut r = genuine_report(&format!("r{i}"), 7, VICTIM, (i % 10) * NS_PER_S);
        r.reporter_cert_digest = "rsu0".to_string();
        ma.on_report(&mut ctx, &r);
    }
    assert!(ma.trusted("rsu0"), "infrastructure is never rate-limited");
    assert!(
        !ma.is_revoked(VICTIM),
        "one certificate is one reporter, however trusted"
    );

    // Two colluders plus the unit, and the exemption is what closes the gate.
    let mut ctx = CollectingCtx::new(1);
    let mut ma = LegacyWindow::legacy_defaults();
    ma.trust_infrastructure("rsu0");
    for (i, digest) in ["rsu0", "rep0002", "rep0003", "rsu0"].into_iter().enumerate() {
        let mut r = genuine_report(&format!("m{i}"), 9, VICTIM, i as u64 * NS_PER_S);
        r.reporter_cert_digest = digest.to_string();
        ma.on_report(&mut ctx, &r);
    }
    assert!(
        ma.is_revoked(VICTIM),
        "three certificates over four seconds, one of them a compromised unit"
    );
}
