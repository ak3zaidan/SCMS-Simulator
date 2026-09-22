//! Each detector fires on its own attack and stays quiet on benign traffic.
//!
//! Every case runs at the legacy one-hertz generation interval with the legacy thresholds,
//! so the behaviour asserted here is the behaviour of the legacy corpus, and reads only
//! what the receiver heard.
//!
//! Three of the cases assert that a detector **does not** fire, which is the more useful
//! half of the suite:
//!
//! * `AlongRoadOffset` is the stealth family working as designed — the claim stays on the
//!   road and moves with the vehicle.
//! * `Teleport` at one hertz is a real recall gap in the legacy operating point: its
//!   violations are isolated in time, and the two-consecutive streak gate suppresses
//!   every one of them. At a realistic five- or ten-hertz beacon rate the same attack is
//!   caught, which the last test shows.
//! * `kalmanConsistency` exceeds its threshold on benign traffic, which is exactly why it
//!   is a soft feature and never a trigger.

mod common;

use std::collections::BTreeSet;

use common::{OneRoad, Scenario, one_message, run, rx_at_origin};
use v2xw_core::model::Model;
use v2xw_core::time::NS_PER_S;
use v2xw_threat::attack::AttackKind;
use v2xw_threat::ctx::CollectingCtx;
use v2xw_threat::detect::{Detector, DetectorId, DetectorParams, Legacy12};
use v2xw_threat::obs::{NoMap, StationType, VerificationState};

/// The check each attack is expected to trip, at the legacy operating point.
fn expected(kind: AttackKind) -> &'static [DetectorId] {
    use AttackKind as A;
    use DetectorId as D;
    match kind {
        A::ConstPos => &[
            D::ConstantPositionFrozen,
            D::PositionSpeedInconsistency,
            D::StaleOrReplay,
        ],
        A::ConstPosOffset => &[D::MapOffRoad, D::PositionSpeedInconsistency],
        A::RandomPos => &[D::PositionJump, D::PositionSpeedInconsistency],
        A::SineWavePos => &[D::MapOffRoad],
        A::ConstSpeedOffset => &[D::PositionSpeedInconsistency],
        A::RandomSpeed => &[D::ImplausibleAcceleration],
        A::StopAndGo => &[D::PositionSpeedInconsistency],
        A::ReversedHeading | A::HeadingOffset => &[D::HeadingInconsistency],
        A::DataReplay => &[D::StaleOrReplay],
        A::SlowDrift => &[D::PositionSpeedInconsistency],
        A::Sybil => &[D::SybilCoLocation],
        A::DoS => &[D::BeaconFrequency],
        A::DelayedMessages | A::OutOfOrder => &[D::StaleOrReplay],
        A::InvalidSignature => &[D::SignatureVerification],
        A::ExpiredCert | A::NotYetValid => &[D::CertValidity],
        A::DoSRandom => &[D::BeaconFrequency, D::PositionJump],
        A::Disruptive => &[D::PositionJump, D::PositionSpeedInconsistency],
        A::PosSpeedInconsistent => &[D::PositionSpeedInconsistency],
        A::PosHeadingInconsistent => &[D::MapOffRoad, D::PositionSpeedInconsistency],
        A::EventualStop => &[D::ConstantPositionFrozen],
        A::VruImpersonation | A::VruPositionSpoof => &[D::VruImpersonation],
        A::FakeHazard => &[D::DenmPlausibility],
        // Designed to evade, or not visible in a beacon at all.
        A::Teleport | A::AlongRoadOffset | A::SelectiveDrop => &[],
    }
}

#[test]
fn each_detector_fires_on_its_own_attack() {
    for kind in AttackKind::ALL {
        let out = run(&Scenario::attacking(kind));
        for d in expected(kind) {
            assert!(
                out.fired(*d),
                "{kind}: {d} did not fire (peak score {:.2})",
                out.peak(*d)
            );
        }
    }
}

#[test]
fn every_legacy_check_is_exercised_by_some_attack() {
    // A check that no attack in the catalog trips is a check nobody has shown can go red.
    let mut covered: BTreeSet<DetectorId> = BTreeSet::new();
    for kind in AttackKind::ALL {
        covered.extend(run(&Scenario::attacking(kind)).fired);
    }
    // `acceptanceRangeThreshold` needs a claim beyond the receiver's range, which no
    // legacy rendering produces on a 2 km receiver; it has its own case below.
    covered.insert(DetectorId::AcceptanceRangeThreshold);
    for d in DetectorId::ALL {
        if !d.is_hard() {
            continue;
        }
        assert!(covered.contains(&d), "{d} was never observed to fire");
    }
}

#[test]
fn benign_traffic_with_realistic_gnss_noise_stays_quiet() {
    for sigma in [0.0, 0.6, 1.2, 2.0] {
        let out = run(&Scenario {
            steps: 300,
            gnss_sigma_m: sigma,
            ..Scenario::default()
        });
        assert!(
            out.fired.is_empty(),
            "sigma = {sigma} m produced false positives: {:?}",
            out.fired.iter().map(|d| d.as_str()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_soft_tracker_feature_exceeds_its_threshold_on_benign_traffic_and_never_fires() {
    let out = run(&Scenario {
        steps: 300,
        gnss_sigma_m: 1.2,
        ..Scenario::default()
    });
    assert!(
        out.peak(DetectorId::KalmanConsistency) > 1.0,
        "the whole point of keeping it soft is that it crosses 1 on honest traffic"
    );
    assert!(!out.fired(DetectorId::KalmanConsistency));
    assert!(!DetectorId::KalmanConsistency.is_hard());
}

#[test]
fn the_along_road_offset_is_designed_to_evade_the_map_check() {
    let out = run(&Scenario::attacking(AttackKind::AlongRoadOffset));
    assert!(
        !out.fired(DetectorId::MapOffRoad),
        "an along-road offset must stay on the road"
    );
    assert_eq!(out.peak(DetectorId::MapOffRoad), 0.0);
}

#[test]
fn teleport_at_one_hertz_is_a_recall_gap_the_streak_gate_creates() {
    let slow = run(&Scenario::attacking(AttackKind::Teleport));
    // Every check screams, and none of them fires, because no violation has a successor.
    assert!(slow.peak(DetectorId::PositionSpeedInconsistency) > 20.0);
    assert!(slow.peak(DetectorId::MapOffRoad) > 5.0);
    assert!(
        slow.fired.is_empty(),
        "at 1 Hz the two-consecutive gate suppresses an isolated jump: {:?}",
        slow.fired.iter().map(|d| d.as_str()).collect::<Vec<_>>()
    );

    // At a realistic beacon rate the same attack persists across messages and is caught.
    for dt in [0.1, 0.2] {
        let fast = run(&Scenario {
            attack: Some(AttackKind::Teleport),
            steps: (60.0 / dt) as u64,
            dt_s: dt,
            ..Scenario::default()
        });
        assert!(
            fast.fired(DetectorId::PositionJump)
                && fast.fired(DetectorId::PositionSpeedInconsistency),
            "teleport went undetected at {dt} s intervals"
        );
        // …and the same rate must not manufacture false positives.
        let benign = run(&Scenario {
            steps: (60.0 / dt) as u64,
            dt_s: dt,
            gnss_sigma_m: 1.2,
            ..Scenario::default()
        });
        assert!(
            benign.fired.is_empty(),
            "false positives at {dt} s intervals"
        );
    }
}

#[test]
fn a_claim_beyond_this_receivers_range_trips_the_acceptance_range_check() {
    let mut det = Legacy12::legacy_defaults();
    let mut ctx = CollectingCtx::new(1);
    // The receiver's own range is 500 m and the tolerance 150 m, so a claim at 800 m
    // scores (800 − 500) / 150 = 2.0 and fires on the second message.
    for i in 0..3u64 {
        let t = i * NS_PER_S;
        let m = one_message(800.0, 0.0, 0.0, t);
        let v = det.on_message(&mut ctx, &rx_at_origin(t), &m, &NoMap);
        let s = v.fingerprint.get(DetectorId::AcceptanceRangeThreshold);
        assert!((s - 2.0).abs() < 1e-9, "score was {s}");
        if i >= 1 {
            assert!(
                v.fired
                    .iter()
                    .any(|o| o.detector == DetectorId::AcceptanceRangeThreshold)
            );
        }
    }
    // A claim inside the range never trips it, however far the honest vehicle is.
    let mut det = Legacy12::legacy_defaults();
    for i in 0..5u64 {
        let t = i * NS_PER_S;
        let m = one_message(480.0, 0.0, 0.0, t);
        let v = det.on_message(&mut ctx, &rx_at_origin(t), &m, &NoMap);
        assert_eq!(v.fingerprint.get(DetectorId::AcceptanceRangeThreshold), 0.0);
    }
}

#[test]
fn a_node_with_no_map_scores_zero_on_the_map_check_rather_than_reaching_for_the_world() {
    let mut det = Legacy12::legacy_defaults();
    let mut ctx = CollectingCtx::new(1);
    for i in 0..5u64 {
        let t = i * NS_PER_S;
        // A claim a kilometre off the road, which a node with a map would flag at once.
        let m = one_message(10.0 * i as f64, 1000.0, 10.0, t);
        let v = det.on_message(&mut ctx, &rx_at_origin(t), &m, &NoMap);
        assert_eq!(v.fingerprint.get(DetectorId::MapOffRoad), 0.0);
    }
    // With the node's own map, the same claim scores 1000 / 15.
    let mut det = Legacy12::legacy_defaults();
    let t = 0;
    let m = one_message(0.0, 1000.0, 10.0, t);
    let v = det.on_message(&mut ctx, &rx_at_origin(t), &m, &OneRoad);
    assert!((v.fingerprint.get(DetectorId::MapOffRoad) - 1000.0 / 15.0).abs() < 1e-9);
}

#[test]
fn a_declared_vru_suppresses_the_vehicle_checks_and_the_impersonation_arms_close_the_hole() {
    let params = DetectorParams {
        station_types_in_play: true,
        ..DetectorParams::default()
    };
    // A genuine slow, off-road pedestrian: the motion and map checks must stay silent and
    // the impersonation check must not fire either.
    let mut det = Legacy12::new(params.clone());
    let mut ctx = CollectingCtx::new(1);
    for i in 0..20u64 {
        let t = i * NS_PER_S;
        let mut m = one_message(1.4 * i as f64, 40.0, 1.4, t);
        m.station_type = StationType::Vru;
        let v = det.on_message(&mut ctx, &rx_at_origin(t), &m, &OneRoad);
        assert!(
            v.fired.is_empty(),
            "a genuine VRU was flagged: {:?}",
            v.fired
                .iter()
                .map(|o| o.detector.as_str())
                .collect::<Vec<_>>()
        );
        for d in DetectorId::MOTION {
            assert_eq!(v.fingerprint.get(d), 0.0);
        }
        assert_eq!(v.fingerprint.get(DetectorId::MapOffRoad), 0.0);
    }

    // A vehicle wearing the declaration: the speed arm catches it even though every
    // suppressed check is silent.
    let mut det = Legacy12::new(params);
    for i in 0..5u64 {
        let t = i * NS_PER_S;
        let mut m = one_message(15.0 * i as f64, 0.0, 15.0, t);
        m.station_type = StationType::Vru;
        let v = det.on_message(&mut ctx, &rx_at_origin(t), &m, &OneRoad);
        assert!((v.fingerprint.get(DetectorId::VruImpersonation) - 1.5).abs() < 1e-9);
        if i >= 1 {
            assert!(
                v.fired
                    .iter()
                    .any(|o| o.detector == DetectorId::VruImpersonation)
            );
        }
    }
}

#[test]
fn a_genuine_event_message_is_never_flagged_and_a_phantom_one_is() {
    let params = DetectorParams {
        event_messages_in_play: true,
        ..DetectorParams::default()
    };
    let mut ctx = CollectingCtx::new(1);
    let denm = |speed: f64, event: &str| {
        let mut m = one_message(0.0, 0.0, speed, NS_PER_S);
        m.kind = v2xw_threat::obs::ObservedKind::Denm(event.to_string());
        m
    };
    let mut det = Legacy12::new(params.clone());
    // A real emergency brake: the sender has braked to at most 4 m/s, and the bound is
    // 4.5, so a genuine announcement stays below 1 with margin.
    for v in [0.0, 2.0, 4.0] {
        let out = det.on_message(
            &mut ctx,
            &rx_at_origin(NS_PER_S),
            &denm(v, "emergencyElectronicBrakeLight"),
            &NoMap,
        );
        assert!(!out.fired(), "a genuine brake DENM at {v} m/s was flagged");
    }
    // A phantom brake from a sender still cruising fires on the first message, with no
    // streak gate: an event message is a one-off, not a stream.
    let out = det.on_message(
        &mut ctx,
        &rx_at_origin(NS_PER_S),
        &denm(15.0, "emergencyElectronicBrakeLight"),
        &NoMap,
    );
    assert!(out.fired());
    assert_eq!(
        out.leading().unwrap().detector,
        DetectorId::DenmPlausibility
    );

    // A phantom brake from a sender crawling in congestion — above the derived brake
    // bound of 4.5 m/s but below the generic 6 m/s line — is caught by the
    // event-type-aware bound and would slip past the generic one.
    let mut det = Legacy12::new(params);
    assert!(
        det.on_message(
            &mut ctx,
            &rx_at_origin(NS_PER_S),
            &denm(5.0, "emergencyElectronicBrakeLight"),
            &NoMap,
        )
        .fired()
    );
    assert!(
        !det.on_message(
            &mut ctx,
            &rx_at_origin(NS_PER_S),
            &denm(5.0, "stationaryVehicle"),
            &NoMap,
        )
        .fired(),
        "the generic bound is 6 m/s, so 5 m/s must not trip a non-brake hazard"
    );
}

#[test]
fn an_unverifiable_message_reports_only_the_crypto_failure() {
    let mut det = Legacy12::legacy_defaults();
    let mut ctx = CollectingCtx::new(1);
    for i in 0..3u64 {
        let t = i * NS_PER_S;
        // A claim that would trip several plausibility checks, with a bad signature.
        let mut m = one_message(5000.0, 5000.0, 90.0, t);
        m.verification = VerificationState::BadSignature;
        let v = det.on_message(&mut ctx, &rx_at_origin(t), &m, &OneRoad);
        for d in DetectorId::ALL {
            if d == DetectorId::SignatureVerification {
                assert_eq!(v.fingerprint.get(d), 1.5);
            } else {
                assert_eq!(
                    v.fingerprint.get(d),
                    0.0,
                    "{d} scored on unverified content"
                );
            }
        }
    }
}

#[test]
fn detections_land_on_the_channel_the_metrics_crate_reads() {
    let mut ctx = CollectingCtx::new(1);
    let mut det = Legacy12::legacy_defaults();
    for i in 0..4u64 {
        let t = i * NS_PER_S;
        let mut m = one_message(0.0, 0.0, 20.0, t);
        m.claimed_generation_time = 0;
        det.on_message(&mut ctx, &rx_at_origin(t), &m, &NoMap);
    }
    let rows = ctx.on_channel("det.observation");
    assert!(!rows.is_empty(), "nothing reached det.observation");
    for r in rows {
        let j: serde_json::Value = serde_json::from_str(r.json_str().unwrap()).unwrap();
        assert!(j["t"].is_u64());
        assert!(j["detector"].is_string());
        assert!(j["subject"].is_string());
        assert!(j["score"].as_f64().unwrap() >= 1.0);
        // The channel is node-visible: nothing ground-truth-tainted may ride on it.
        assert!(r.visibility.allowed_on_node_channel());
        assert!(
            j.get("actor").is_none(),
            "an actor id reached a NODE channel"
        );
    }
}

#[test]
fn the_detector_is_deterministic_and_draws_no_randomness() {
    let a = run(&Scenario::attacking(AttackKind::RandomPos));
    let b = run(&Scenario::attacking(AttackKind::RandomPos));
    assert_eq!(a.fired, b.fired);
    let fa: Vec<f64> = a.verdicts.iter().map(|v| v.fingerprint.max()).collect();
    let fb: Vec<f64> = b.verdicts.iter().map(|v| v.fingerprint.max()).collect();
    assert_eq!(fa, fb);
    assert!(!Legacy12::legacy_defaults().card().determinism.uses_rng);
}
