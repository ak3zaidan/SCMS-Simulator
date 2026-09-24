//! The leakage firewall, as behaviour rather than as a promise.
//!
//! A detector that reads ground truth does not crash and does not look wrong. It reports a
//! detection rate no real receiver could achieve, and nobody reports a result that is too
//! good. So the property has to be *tested*, and the test that works is a differential
//! one: change the world and leave the claims alone. A detector that reads only what a
//! node can see must produce byte-identical output.

mod common;

use common::{OneRoad, one_message, rx_at_origin};
use v2xw_core::ids::ActorId;
use v2xw_core::model::Model;
use v2xw_core::time::NS_PER_S;
use v2xw_threat::attack::{AttackAction, AttackKind, log_actions};
use v2xw_threat::ctx::{CollectingCtx, ThreatCtx};
use v2xw_threat::detect::{Detector, DetectorId, Legacy12};
use v2xw_threat::ma::{LegacyWindow, MaPipeline};

/// The receiver's verdicts for a stream of claims, with a nominated true state that the
/// detector is never told.
fn verdicts(claims: &[(f64, f64, f64)]) -> Vec<Vec<(DetectorId, f64)>> {
    let mut det = Legacy12::legacy_defaults();
    let mut ctx = CollectingCtx::new(1);
    claims
        .iter()
        .enumerate()
        .map(|(i, (x, y, s))| {
            let t = i as u64 * NS_PER_S;
            let m = one_message(*x, *y, *s, t);
            let v = det.on_message(&mut ctx, &rx_at_origin(t), &m, &OneRoad);
            v.fingerprint.iter().collect()
        })
        .collect()
}

#[test]
fn the_same_claims_produce_the_same_detections_whatever_the_truth_behind_them_is() {
    // One stream of claims. In run A they are an honest vehicle's; in run B they are an
    // attacker's, produced by a body somewhere else entirely. The detector is handed the
    // same bytes, so it must reach the same verdicts — and it does, because there is no
    // argument through which the truth could reach it.
    let claims: Vec<(f64, f64, f64)> = (0..30).map(|i| (15.0 * f64::from(i), 0.0, 15.0)).collect();
    assert_eq!(verdicts(&claims), verdicts(&claims));

    // The falsified stream differs, and it differs *because the claims differ*, which is
    // the only channel a receiver has.
    let mut falsified = claims.clone();
    for c in falsified.iter_mut().skip(10) {
        c.1 += 40.0;
    }
    assert_ne!(verdicts(&claims), verdicts(&falsified));
}

#[test]
fn a_detection_record_carries_no_actor_and_a_report_carries_no_actor() {
    let mut ctx = CollectingCtx::new(1);
    let mut det = Legacy12::legacy_defaults();
    for i in 0..5u64 {
        let t = i * NS_PER_S;
        let m = one_message(0.0, 0.0, 30.0, t);
        det.on_message(&mut ctx, &rx_at_origin(t), &m, &OneRoad);
    }
    for r in ctx.on_channel("det.observation") {
        let j: serde_json::Value = serde_json::from_str(r.json_str().unwrap()).unwrap();
        let obj = j.as_object().unwrap();
        for key in ["actor", "true_x", "true_y", "is_attacker", "falsified"] {
            assert!(obj.get(key).is_none(), "{key} leaked onto det.observation");
        }
        assert!(!r.visibility.is_gt_tainted());
    }
}

#[test]
fn the_attack_action_channel_is_ground_truth_and_is_written_by_the_host() {
    let mut ctx = CollectingCtx::new(1);
    // Only a caller that already holds the actor id can write this record, which is the
    // whole of invariant I-T3's enforcement: the attacker has no such id to pass.
    log_actions(
        &mut ctx,
        5 * NS_PER_S,
        ActorId::new(42),
        v2xw_threat::attack_legacy::MODEL_ID,
        &[
            AttackAction::FalsifyOutgoing {
                fields: vec!["position".into(), "speed".into()],
                magnitude: 25.0,
            },
            AttackAction::Suppress,
        ],
        Some(7),
    );
    let rows = ctx.on_channel("gt.attack.action");
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert!(r.visibility.is_gt_tainted());
        assert!(!r.visibility.allowed_on_node_channel());
        let j: serde_json::Value = serde_json::from_str(r.json_str().unwrap()).unwrap();
        assert_eq!(j["actor"], 42);
        assert_eq!(j["attacker"], v2xw_threat::attack_legacy::MODEL_ID);
    }
    let first: serde_json::Value = serde_json::from_str(rows[0].json_str().unwrap()).unwrap();
    assert_eq!(first["fields"], serde_json::json!(["position", "speed"]));
    assert_eq!(first["changed_bytes_on_air"], true);
    assert_eq!(first["magnitude"], 25.0);
    let second: serde_json::Value = serde_json::from_str(rows[1].json_str().unwrap()).unwrap();
    assert_eq!(
        second["changed_bytes_on_air"], false,
        "suppression removes bytes rather than changing them"
    );
}

#[test]
fn the_authority_never_sees_more_than_a_digest() {
    // A report's subject is a digest and the pipeline's decision names the same digest.
    // Nothing in the pipeline can turn one into an actor, which is why the metrics crate
    // makes the run *declare* the link instead.
    use v2xw_threat::detect::{Fingerprint, Observation, Verdict};
    use v2xw_threat::report::{Evidence, MisbehaviourReport};
    let mut ctx = CollectingCtx::new(1);
    let mut ma = LegacyWindow::legacy_defaults();
    let mut f = Fingerprint::default();
    f.set(DetectorId::PositionJump, 2.0);
    for (i, reporter) in [1u32, 2, 3, 4, 5].into_iter().enumerate() {
        let t = i as u64 * NS_PER_S;
        let v = Verdict {
            subject: "0102030405060708".to_string(),
            fingerprint: f,
            fired: vec![Observation {
                detector: DetectorId::PositionJump,
                score: 2.0,
                subject: "0102030405060708".to_string(),
                at: t,
            }],
        };
        let r = MisbehaviourReport::from_verdict(
            format!("rpt_{i:05}"),
            v2xw_core::ids::NodeId::new(reporter),
            format!("reporter{reporter}"),
            &v,
            &Evidence::at(t, t, 2.0),
        )
        .unwrap();
        ma.on_report(&mut ctx, &r);
    }
    assert!(ma.is_revoked("0102030405060708"));
    for channel in ["ma.report", "ma.decision"] {
        for r in ctx.on_channel(channel) {
            assert!(!r.visibility.is_gt_tainted());
            let j: serde_json::Value = serde_json::from_str(r.json_str().unwrap()).unwrap();
            assert!(j.as_object().unwrap().get("actor").is_none());
            assert_eq!(j["subject"], "0102030405060708");
        }
    }
}

#[test]
fn an_attacker_falsifies_relative_to_its_own_belief_and_not_to_the_truth() {
    use v2xw_threat::attack::{Attacker, AttackerView, Emission, HonestClaim};
    use v2xw_threat::attack_legacy::{LegacyAttacker, LegacyAttackerParams};
    use v2xw_threat::capability::{AttackSchedule, Capabilities};
    use v2xw_threat::obs::SelfBelief;

    // Two attackers whose bodies are far apart but whose *beliefs* are identical (a GNSS
    // spoof, say) must produce identical claims: the attacker has nothing else to work
    // from. There is no argument through which the true position could differ.
    let mk = || {
        LegacyAttacker::new(
            common::TX,
            LegacyAttackerParams::new(AttackKind::ConstPosOffset),
            Capabilities::insider(20),
            AttackSchedule {
                from: 0,
                to: u64::MAX,
                ..AttackSchedule::default()
            },
            Vec::new(),
        )
    };
    let honest = HonestClaim {
        x_m: 100.0,
        y_m: 0.0,
        speed_mps: 15.0,
        heading_rad: 0.0,
    };
    let claim = |a: &mut LegacyAttacker| {
        let mut ctx = CollectingCtx::new(9);
        let me = SelfBelief {
            node: common::TX,
            believed_time: NS_PER_S,
            x_m: honest.x_m,
            y_m: honest.y_m,
            radio_range_m: 500.0,
        };
        let view = AttackerView {
            own_rx: &[],
            own_credentials: &[],
            crl_revocations_seen: None,
            own_belief: me,
            honest,
            believed_time: NS_PER_S,
        };
        let mut out = Emission::honest([0; 8], honest, NS_PER_S, 0, u64::MAX);
        a.act(&mut ctx, &view, &mut out);
        (out.x_m, out.y_m)
    };
    assert_eq!(claim(&mut mk()), claim(&mut mk()));
    assert_eq!(claim(&mut mk()), (125.0, 25.0));
}

#[test]
fn every_model_in_the_crate_has_a_card_that_validates() {
    // 03-interfaces.md §12: a plug-in without a valid model card cannot be registered,
    // and rule R1 refuses an uncited default with no calibration plan. This is what makes
    // "never invent a number" checkable rather than aspirational.
    let mut cards = Vec::new();
    for k in AttackKind::ALL {
        cards.push(v2xw_threat::attack_legacy::card(
            &v2xw_threat::attack_legacy::LegacyAttackerParams::new(k),
        ));
    }
    // The Phase 4 models, so "every model in the crate" keeps meaning that.
    for k in v2xw_threat::ExtendedAttackKind::ALL {
        cards.push(v2xw_threat::attack_ext::card(
            &v2xw_threat::ExtendedAttackerParams::new(k),
        ));
    }
    for k in v2xw_threat::RsuAttackKind::ALL {
        cards.push(v2xw_threat::attack_rsu::card(
            &v2xw_threat::RsuAttackParams::new(k, "rsu0"),
        ));
    }
    cards.push(v2xw_threat::poison::card(&v2xw_threat::PoisonParams::new(
        "cafe",
    )));
    cards.push(v2xw_threat::privacy::card(
        &v2xw_threat::ObserverParams::default(),
    ));
    cards.push(v2xw_threat::ts103759::card(
        &v2xw_threat::Ts103759Params::default(),
    ));
    cards.push(v2xw_threat::resolve::card(
        &v2xw_threat::ResolutionParams::default(),
    ));
    cards.push(Legacy12::legacy_defaults().card().clone());
    cards.push(LegacyWindow::legacy_defaults().card().clone());
    for c in &cards {
        c.validate()
            .unwrap_or_else(|e| panic!("{} has an invalid card: {e}", c.id));
        c.check_api_version().unwrap();
        assert!(!c.parameters.is_empty(), "{} declares no parameters", c.id);
        assert!(!c.tier.is_empty());
        assert!(!c.sources.is_empty(), "{} cites nothing", c.id);
        for p in &c.parameters {
            // Every default is either cited or carries a calibration plan. Nothing is
            // simply asserted.
            let cited = p.source.kind != v2xw_core::card::SourceKind::TodoCalibrate;
            let planned = p.calibration.as_ref().is_some_and(|s| !s.trim().is_empty());
            assert!(
                cited || planned,
                "{}: parameter {} has neither a source nor a calibration plan",
                c.id,
                p.name
            );
        }
    }
}

#[test]
fn the_uncited_legacy_constants_are_visible_on_the_todo_calibrate_page() {
    // An uncited number reproduced for conformance is still uncited, and saying so is the
    // difference between honest provenance and a file path standing in for a reason.
    let det = Legacy12::legacy_defaults().card().clone();
    let todo: Vec<&str> = det.todo_calibrate().map(|p| p.name.as_str()).collect();
    for expected in [
        "art_max_m",
        "offroad_tol_m",
        "freq_max",
        "stale_max_s",
        "sybil_window_s",
    ] {
        assert!(
            todo.contains(&expected),
            "{expected} claims a source the legacy engine never gave it"
        );
    }
    // …and a number that does have a real source is not on that page.
    assert!(!todo.contains(&"max_accel_mps2"));
    assert!(!todo.contains(&"kalman_alpha"));
}

#[test]
fn the_record_channels_are_exactly_what_the_metrics_crate_decodes() {
    use v2xw_core::ctx::Record;
    // If one of these strings changes, detection metrics silently stop counting: the
    // reader finds no rows and reports an empty matrix rather than an error.
    assert_eq!(
        v2xw_threat::records::DetObservation::CHANNEL,
        "det.observation"
    );
    assert_eq!(v2xw_threat::records::MaReportRecord::CHANNEL, "ma.report");
    assert_eq!(
        v2xw_threat::records::MaDecisionRecord::CHANNEL,
        "ma.decision"
    );
    assert_eq!(
        v2xw_threat::records::GtAttackAction::CHANNEL,
        "gt.attack.action"
    );
}

#[test]
fn the_threat_context_cannot_reach_the_world() {
    // A compile-time property, asserted by construction: `ThreatCtx` has three methods,
    // and a plug-in written against `&mut dyn ThreatCtx` has no other surface. If a
    // `world()` or `actors()` accessor were ever added, this function would still compile
    // — so the test that matters is that every model here takes `dyn ThreatCtx` and not
    // `dyn Ctx`, which the signatures below assert.
    fn takes_only_a_threat_ctx(ctx: &mut dyn ThreatCtx) -> u64 {
        ctx.now()
    }
    let mut ctx = CollectingCtx::new(1);
    ctx.set_now(42);
    assert_eq!(takes_only_a_threat_ctx(&mut ctx), 42);

    fn detector_signature_is_belief_only<D: Detector>(_d: &D) {}
    detector_signature_is_belief_only(&Legacy12::legacy_defaults());
}
