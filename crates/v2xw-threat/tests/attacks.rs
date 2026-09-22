//! Each attack renders the documented deviation — and nothing else.
//!
//! "Nothing else" is the half that matters. An attack that quietly perturbs a second field
//! makes every per-family detection number a lie, because the recall attributed to the
//! position family would partly be the speed family's. Each case below therefore asserts
//! the intended edit *and* that the fields the legacy rendering leaves alone are
//! bit-identical to the honest claim.

mod common;

use common::{SPEED_MPS, Scenario, run};
use v2xw_core::time::{NS_PER_S, ns_to_secs};
use v2xw_threat::attack::{AttackKind, Emission, HonestClaim, is_falsified};
use v2xw_threat::attack_legacy::LegacyAttackerParams;
use v2xw_threat::obs::StationType;

/// Where the attack schedule opens, in the harness.
const ONSET_S: u64 = 5;

fn at(out: &common::RunOut, i: usize) -> (&Emission, &HonestClaim) {
    (&out.emissions[i], &out.honest[i])
}

/// Every index at or after the onset.
fn attacking(out: &common::RunOut) -> impl Iterator<Item = usize> + '_ {
    (ONSET_S as usize..out.emissions.len()).filter(move |i| !out.emissions[*i].suppressed)
}

fn assert_pos_untouched(e: &Emission, h: &HonestClaim) {
    assert_eq!(e.x_m, h.x_m, "x was edited");
    assert_eq!(e.y_m, h.y_m, "y was edited");
}

fn assert_speed_untouched(e: &Emission, h: &HonestClaim) {
    assert_eq!(e.speed_mps, h.speed_mps, "speed was edited");
}

fn assert_heading_untouched(e: &Emission, h: &HonestClaim) {
    assert_eq!(e.heading_rad, h.heading_rad, "heading was edited");
}

/// The envelope and timing fields a content-only attack must not touch.
fn assert_envelope_clean(e: &Emission, i: usize, dt_s: f64) {
    assert!(e.signature_valid, "signature was invalidated");
    assert_eq!(e.repetitions, 1, "extra copies were put on the air");
    assert_eq!(
        e.station_type,
        StationType::Vehicle,
        "station type was edited"
    );
    assert_eq!(
        e.generation_time,
        v2xw_core::time::secs_to_ns(i as f64 * dt_s),
        "generation time was edited"
    );
    assert!(!e.suppressed);
}

#[test]
fn before_the_onset_every_attacker_is_an_ordinary_node() {
    for k in AttackKind::ALL {
        let out = run(&Scenario::attacking(k));
        for i in 0..ONSET_S as usize {
            let (e, h) = at(&out, i);
            assert_pos_untouched(e, h);
            assert_speed_untouched(e, h);
            assert_heading_untouched(e, h);
            assert_envelope_clean(e, i, 1.0);
            assert!(
                !is_falsified(
                    h,
                    e,
                    v2xw_core::time::secs_to_ns(i as f64),
                    StationType::Vehicle
                ),
                "{k} falsified before its schedule opened"
            );
        }
    }
}

#[test]
fn const_pos_freezes_at_the_first_claim_it_makes() {
    let out = run(&Scenario::attacking(AttackKind::ConstPos));
    let frozen = (
        out.emissions[ONSET_S as usize].x_m,
        out.emissions[ONSET_S as usize].y_m,
    );
    assert_eq!(frozen.0, out.honest[ONSET_S as usize].x_m);
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        assert_eq!((e.x_m, e.y_m), frozen, "the frozen claim moved");
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
        assert_envelope_clean(e, i, 1.0);
    }
}

#[test]
fn const_pos_offset_adds_twenty_five_metres_to_both_axes() {
    let out = run(&Scenario::attacking(AttackKind::ConstPosOffset));
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        assert_eq!(e.x_m, h.x_m + 25.0);
        assert_eq!(e.y_m, h.y_m + 25.0);
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
        assert_envelope_clean(e, i, 1.0);
    }
}

#[test]
fn the_intensity_dial_and_the_per_type_scale_multiply_the_magnitude() {
    // k = intensity × scale, and the rendering is linear in k.
    let mut p = LegacyAttackerParams::new(AttackKind::ConstPosOffset);
    p.intensity = 2.0;
    let p = p
        .with_magnitude_scale(AttackKind::ConstPosOffset, 1.5)
        .unwrap();
    assert_eq!(p.k(), 3.0);
    // …and a type with no amplitude refuses a scale rather than ignoring it.
    for k in [
        AttackKind::ConstPos,
        AttackKind::ReversedHeading,
        AttackKind::DataReplay,
        AttackKind::VruImpersonation,
        AttackKind::FakeHazard,
    ] {
        assert!(
            LegacyAttackerParams::new(k)
                .with_magnitude_scale(k, 2.0)
                .is_err(),
            "{k} accepted a magnitude scale it has no magnitude for"
        );
    }
    // A zero or negative scale is refused too: a zero-magnitude attacker would be
    // labelled an attacker and emit no falsification, which mislabels the ground truth.
    assert!(
        LegacyAttackerParams::new(AttackKind::RandomPos)
            .with_magnitude_scale(AttackKind::RandomPos, 0.0)
            .is_err()
    );
}

#[test]
fn random_pos_stays_inside_its_half_range_and_touches_nothing_else() {
    let out = run(&Scenario::attacking(AttackKind::RandomPos));
    let mut distinct = 0;
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        assert!((e.x_m - h.x_m).abs() <= 60.0);
        assert!((e.y_m - h.y_m).abs() <= 60.0);
        if (e.x_m - h.x_m).abs() > 1e-9 {
            distinct += 1;
        }
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
        assert_envelope_clean(e, i, 1.0);
    }
    assert!(distinct > 40, "the offset should be redrawn every message");
}

#[test]
fn teleport_jumps_only_on_its_own_period() {
    let out = run(&Scenario::attacking(AttackKind::Teleport));
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        if i as u64 % 4 == 0 {
            assert_eq!(e.x_m, h.x_m + 150.0);
            assert_eq!(e.y_m, h.y_m + 80.0);
        } else {
            assert_pos_untouched(e, h);
        }
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
    }
}

#[test]
fn sine_wave_pos_oscillates_across_the_direction_of_travel_only() {
    let out = run(&Scenario::attacking(AttackKind::SineWavePos));
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        let t_s = ns_to_secs(i as u64 * NS_PER_S);
        let want = h.y_m + 20.0 * v2xw_core::math::sin(0.6 * t_s);
        assert_eq!(e.y_m, want);
        assert_eq!(e.x_m, h.x_m, "the along-track coordinate must be honest");
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
    }
}

#[test]
fn the_speed_family_edits_the_speed_and_nothing_else() {
    let off = run(&Scenario::attacking(AttackKind::ConstSpeedOffset));
    for i in attacking(&off) {
        let (e, h) = at(&off, i);
        assert_eq!(e.speed_mps, h.speed_mps + 12.0);
        assert_pos_untouched(e, h);
        assert_heading_untouched(e, h);
    }

    let rnd = run(&Scenario::attacking(AttackKind::RandomSpeed));
    for i in attacking(&rnd) {
        let (e, h) = at(&rnd, i);
        assert!((0.0..=40.0).contains(&e.speed_mps));
        assert_pos_untouched(e, h);
        assert_heading_untouched(e, h);
    }

    let sg = run(&Scenario::attacking(AttackKind::StopAndGo));
    for i in attacking(&sg) {
        let (e, h) = at(&sg, i);
        let want = if i as u64 % 2 == 0 { 0.0 } else { 35.0 };
        assert_eq!(e.speed_mps, want);
        assert_pos_untouched(e, h);
        assert_heading_untouched(e, h);
    }
}

#[test]
fn the_heading_family_edits_the_heading_and_nothing_else() {
    let rev = run(&Scenario::attacking(AttackKind::ReversedHeading));
    for i in attacking(&rev) {
        let (e, h) = at(&rev, i);
        assert!(
            (v2xw_threat::capability::angle_diff_rad(e.heading_rad, h.heading_rad)
                - core::f64::consts::PI)
                .abs()
                < 1e-12
        );
        assert_pos_untouched(e, h);
        assert_speed_untouched(e, h);
    }

    let off = run(&Scenario::attacking(AttackKind::HeadingOffset));
    for i in attacking(&off) {
        let (e, h) = at(&off, i);
        let d = v2xw_threat::capability::angle_diff_rad(e.heading_rad, h.heading_rad).to_degrees();
        assert!((d - 45.0).abs() < 1e-9, "heading offset was {d} deg");
        assert_pos_untouched(e, h);
        assert_speed_untouched(e, h);
    }
}

#[test]
fn data_replay_retransmits_the_claim_five_messages_back_with_its_old_generation_time() {
    let out = run(&Scenario::attacking(AttackKind::DataReplay));
    for i in (ONSET_S as usize + 5)..out.emissions.len() {
        let e = &out.emissions[i];
        // The claim five emissions back — which is what the legacy history index reads.
        let old = &out.emissions[i - 5];
        assert_eq!(e.x_m, old.x_m, "replay at {i}");
        assert_eq!(e.y_m, old.y_m);
        assert_eq!(e.speed_mps, old.speed_mps);
        assert_eq!(
            e.generation_time,
            (i as u64 * NS_PER_S).saturating_sub(5 * NS_PER_S),
            "the replayed frame must carry its old generation time"
        );
        assert!(e.signature_valid);
        assert_eq!(e.repetitions, 1);
    }
}

#[test]
fn slow_drift_ramps_monotonically_up_to_its_ceiling_along_one_axis() {
    let out = run(&Scenario::attacking(AttackKind::SlowDrift));
    let mut prev = 0.0;
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        let drift = e.x_m - h.x_m;
        assert!(drift >= prev - 1e-12, "the drift must not go backwards");
        prev = drift;
        assert_eq!(e.y_m, h.y_m, "the drift is along one axis only");
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
    }
    // The rate ceiling is 5 m/s, so after 55 s the drift is far short of an unbounded ramp.
    let last = out.emissions.len() - 1;
    let total = out.emissions[last].x_m - out.honest[last].x_m;
    assert!(total > 100.0 && total < 5.0 * 56.0, "drift was {total} m");
}

#[test]
fn along_road_offset_stays_on_the_road() {
    let out = run(&Scenario::attacking(AttackKind::AlongRoadOffset));
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        // Heading is due east in the harness, so the whole offset lands on x.
        assert!((e.x_m - (h.x_m + 30.0)).abs() < 1e-9);
        assert!(
            (e.y_m - h.y_m).abs() < 1e-9,
            "an along-road offset stays on the road"
        );
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
    }
}

#[test]
fn sybil_transmits_from_its_own_concurrent_credentials_at_nearly_one_point() {
    let out = run(&Scenario::attacking(AttackKind::Sybil));
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        // The attacker's own beacon stays honest: the lie is the extra identities.
        assert_pos_untouched(e, h);
        assert_speed_untouched(e, h);
        assert_eq!(e.ghosts.len(), 6, "sybil_ghosts = 6");
        let mut signers: Vec<[u8; 8]> = e.ghosts.iter().map(|g| g.signer).collect();
        signers.sort_unstable();
        signers.dedup();
        assert_eq!(signers.len(), 6, "ghosts must be distinct identities");
        for g in &e.ghosts {
            assert!((g.x_m - e.x_m).abs() <= 1.0);
            assert!((g.y_m - e.y_m).abs() <= 1.0);
            assert_eq!(g.speed_mps, e.speed_mps);
            assert_eq!(g.heading_rad, e.heading_rad);
            assert_ne!(g.signer, e.signer);
        }
    }
}

#[test]
fn an_attacker_with_no_ghost_credentials_transmits_no_ghosts() {
    // The declared capability is what bounds the attack: an attacker the engine issued no
    // extra credentials to cannot conjure them.
    use v2xw_threat::attack::{Attacker, AttackerView};
    use v2xw_threat::capability::{AttackSchedule, Capabilities};
    use v2xw_threat::ctx::CollectingCtx;
    use v2xw_threat::obs::SelfBelief;
    let mut ctx = CollectingCtx::new(3);
    let mut a = v2xw_threat::attack_legacy::LegacyAttacker::new(
        common::TX,
        LegacyAttackerParams::new(AttackKind::Sybil),
        Capabilities::outsider(),
        AttackSchedule {
            from: 0,
            to: u64::MAX,
            ..AttackSchedule::default()
        },
        Vec::new(),
    );
    let honest = HonestClaim {
        x_m: 0.0,
        y_m: 0.0,
        speed_mps: SPEED_MPS,
        heading_rad: 0.0,
    };
    let me = SelfBelief {
        node: common::TX,
        believed_time: NS_PER_S,
        x_m: 0.0,
        y_m: 0.0,
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
    assert!(out.ghosts.is_empty());
}

#[test]
fn the_timing_and_credential_families_edit_the_envelope_and_not_the_claim() {
    for (kind, check) in [
        (
            AttackKind::DoS,
            (|e: &Emission, i: usize| {
                assert_eq!(e.repetitions, 12);
                assert_eq!(e.generation_time, i as u64 * NS_PER_S);
                assert!(e.signature_valid);
            }) as fn(&Emission, usize),
        ),
        (AttackKind::DelayedMessages, |e, i| {
            assert_eq!(
                e.generation_time,
                (i as u64 * NS_PER_S).saturating_sub(6 * NS_PER_S)
            );
            assert_eq!(e.repetitions, 1);
        }),
        (AttackKind::InvalidSignature, |e, _i| {
            assert!(!e.signature_valid);
            assert_eq!(e.repetitions, 1);
        }),
        (AttackKind::ExpiredCert, |e, i| {
            assert_eq!(
                e.cert_valid_to,
                (i as u64 * NS_PER_S).saturating_sub(5 * NS_PER_S)
            );
        }),
        (AttackKind::NotYetValid, |e, i| {
            assert_eq!(e.cert_valid_from, i as u64 * NS_PER_S + 5 * NS_PER_S);
        }),
        (AttackKind::OutOfOrder, |e, i| {
            let lag = ns_to_secs((i as u64 * NS_PER_S).saturating_sub(e.generation_time));
            // The draw is uniform on [delay_s, 2 delay_s] = [6, 12] s, clamped at the
            // start of the run because a generation time cannot precede t0.
            assert!(
                lag <= 12.0 && (lag >= 6.0 || e.generation_time == 0),
                "out-of-order lag was {lag} s"
            );
        }),
    ] {
        let out = run(&Scenario::attacking(kind));
        for i in attacking(&out) {
            let (e, h) = at(&out, i);
            check(e, i);
            // Whatever the envelope says, the claim itself is honest for these types.
            assert_pos_untouched(e, h);
            assert_speed_untouched(e, h);
            assert_heading_untouched(e, h);
            assert_eq!(e.station_type, StationType::Vehicle);
        }
    }
}

#[test]
fn dos_random_both_floods_and_randomises() {
    let out = run(&Scenario::attacking(AttackKind::DoSRandom));
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        assert_eq!(e.repetitions, 12);
        assert!((e.x_m - h.x_m).abs() <= 60.0);
        assert_speed_untouched(e, h);
    }
}

#[test]
fn the_combined_family_falsifies_several_fields_at_once() {
    let d = run(&Scenario::attacking(AttackKind::Disruptive));
    for i in attacking(&d) {
        let (e, h) = at(&d, i);
        assert_ne!((e.x_m, e.y_m), (h.x_m, h.y_m));
        assert_ne!(e.speed_mps, h.speed_mps);
        assert_ne!(e.heading_rad, h.heading_rad);
    }

    let ps = run(&Scenario::attacking(AttackKind::PosSpeedInconsistent));
    for i in attacking(&ps) {
        let (e, h) = at(&ps, i);
        assert_eq!(e.speed_mps, (h.speed_mps - 20.0).max(0.0));
        assert_pos_untouched(e, h);
        assert_heading_untouched(e, h);
    }

    let ph = run(&Scenario::attacking(AttackKind::PosHeadingInconsistent));
    for i in attacking(&ph) {
        let (e, h) = at(&ph, i);
        assert_heading_untouched(e, h);
        assert_speed_untouched(e, h);
        // The swing is perpendicular to travel, so it lands entirely on y.
        assert!(
            (e.x_m - h.x_m).abs() < 1e-9,
            "the swing must be perpendicular"
        );
        let t_s = ns_to_secs(i as u64 * NS_PER_S);
        let want = h.y_m + 30.0 * v2xw_core::math::sin(1.3 * t_s);
        assert!((e.y_m - want).abs() < 1e-9);
    }

    let es = run(&Scenario::attacking(AttackKind::EventualStop));
    for i in attacking(&es) {
        let (e, h) = at(&es, i);
        if (i as u64) < ONSET_S + 6 {
            assert_pos_untouched(e, h);
            assert_speed_untouched(e, h);
        } else {
            assert_eq!(e.speed_mps, 0.8 + 2.0, "the residual creeping speed");
            assert_eq!((e.x_m, e.y_m), (es.emissions[11].x_m, es.emissions[11].y_m));
        }
    }
}

#[test]
fn the_identity_spoof_family_declares_the_wrong_station_type() {
    let imp = run(&Scenario::attacking(AttackKind::VruImpersonation));
    for i in attacking(&imp) {
        let (e, h) = at(&imp, i);
        assert_eq!(e.station_type, StationType::Vru);
        // It drives honestly and at vehicle speed: the falsification is the declaration.
        assert_pos_untouched(e, h);
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
        assert!(is_falsified(
            h,
            e,
            i as u64 * NS_PER_S,
            StationType::Vehicle
        ));
    }

    let spoof = run(&Scenario::attacking(AttackKind::VruPositionSpoof));
    for i in attacking(&spoof) {
        let (e, h) = at(&spoof, i);
        assert_eq!(e.station_type, StationType::Vru);
        assert_eq!(e.speed_mps, 2.0, "it claims a plausible walking pace");
        let r = v2xw_core::math::hypot(e.x_m - h.x_m, e.y_m - h.y_m);
        assert!(
            (r - 60.0).abs() < 1e-6,
            "the claim walks a 60 m circle, r = {r}"
        );
    }
}

#[test]
fn fake_hazard_emits_a_phantom_event_alongside_an_honest_beacon() {
    let out = run(&Scenario::attacking(AttackKind::FakeHazard));
    let mut events = 0;
    for i in attacking(&out) {
        let (e, h) = at(&out, i);
        assert_pos_untouched(e, h);
        assert_speed_untouched(e, h);
        assert_heading_untouched(e, h);
        assert_eq!(e.station_type, StationType::Vehicle);
        for ev in &e.events {
            assert_eq!(ev.event_type, "emergencyElectronicBrakeLight");
            assert_eq!(
                ev.claimed_speed_mps, h.speed_mps,
                "the phantom brake carries the sender's real cruising speed, which is \
                 what makes it contradict itself"
            );
            events += 1;
        }
    }
    // 40 fake events per 100 s at 1 Hz over 55 eligible seconds: about 22.
    assert!(events > 5, "only {events} phantom events in 55 s");
}

#[test]
fn selective_drop_removes_messages_rather_than_editing_them() {
    let out = run(&Scenario::attacking(AttackKind::SelectiveDrop));
    let dropped = out.emissions.iter().filter(|e| e.suppressed).count();
    assert!(
        dropped > 5,
        "only {dropped} of 55 messages dropped at p = 0.5"
    );
    for i in ONSET_S as usize..out.emissions.len() {
        let (e, h) = at(&out, i);
        if !e.suppressed {
            assert_pos_untouched(e, h);
            assert_speed_untouched(e, h);
            assert_heading_untouched(e, h);
        }
    }
}

#[test]
fn a_crl_aware_attacker_goes_dormant_after_it_sees_a_revocation() {
    use v2xw_threat::attack::{Attacker, AttackerView};
    use v2xw_threat::capability::{AttackSchedule, Capabilities};
    use v2xw_threat::ctx::CollectingCtx;
    use v2xw_threat::obs::SelfBelief;

    let mut ctx = CollectingCtx::new(5);
    let schedule = AttackSchedule {
        from: 0,
        to: u64::MAX,
        ..AttackSchedule::default()
    };
    let mut a = v2xw_threat::attack_legacy::LegacyAttacker::new(
        common::TX,
        LegacyAttackerParams::new(AttackKind::ConstPosOffset),
        Capabilities::insider(20).crl_aware(),
        schedule,
        Vec::new(),
    );
    let honest = HonestClaim {
        x_m: 0.0,
        y_m: 0.0,
        speed_mps: SPEED_MPS,
        heading_rad: 0.0,
    };
    let step = |a: &mut v2xw_threat::attack_legacy::LegacyAttacker,
                ctx: &mut CollectingCtx,
                t: u64,
                seen: Option<u32>| {
        let me = SelfBelief {
            node: common::TX,
            believed_time: t,
            x_m: 0.0,
            y_m: 0.0,
            radio_range_m: 500.0,
        };
        let view = AttackerView {
            own_rx: &[],
            own_credentials: &[],
            crl_revocations_seen: seen,
            own_belief: me,
            honest,
            believed_time: t,
        };
        a.observe(ctx, &view);
        let mut out = Emission::honest([0; 8], honest, t, 0, u64::MAX);
        a.act(ctx, &view, &mut out);
        out
    };

    // Acting normally.
    assert_eq!(step(&mut a, &mut ctx, NS_PER_S, Some(0)).x_m, 25.0);
    // An accomplice is revoked: it lies low for the 45 s dormancy.
    let out = step(&mut a, &mut ctx, 2 * NS_PER_S, Some(1));
    assert_eq!(out.x_m, 0.0, "it should broadcast honestly while dormant");
    assert_eq!(a.dormant_until(), 2 * NS_PER_S + 45 * NS_PER_S);
    assert_eq!(step(&mut a, &mut ctx, 40 * NS_PER_S, Some(1)).x_m, 0.0);
    // Once the heat dies down it resumes.
    assert_eq!(step(&mut a, &mut ctx, 48 * NS_PER_S, Some(1)).x_m, 25.0);

    // An attacker that did not declare CRL knowledge cannot evade this way, even if the
    // view were to carry the number.
    let mut blind = v2xw_threat::attack_legacy::LegacyAttacker::new(
        common::TX,
        LegacyAttackerParams::new(AttackKind::ConstPosOffset),
        Capabilities::insider(20),
        AttackSchedule {
            from: 0,
            to: u64::MAX,
            ..AttackSchedule::default()
        },
        Vec::new(),
    );
    assert_eq!(step(&mut blind, &mut ctx, NS_PER_S, Some(7)).x_m, 25.0);
    assert_eq!(blind.dormant_until(), 0);
}

#[test]
fn every_attack_that_changes_the_air_is_labelled_falsified() {
    // 07-threats-and-detection.md §4's label rule, applied to every rendering.
    // SelectiveDrop removes bytes rather than changing them, so it has no falsified
    // message by construction; the attack shows up as a Suppress action instead.
    let honest_only = [AttackKind::SelectiveDrop];
    for k in AttackKind::ALL {
        if honest_only.contains(&k) {
            continue;
        }
        let out = run(&Scenario::attacking(k));
        let labelled: usize = (ONSET_S as usize..out.emissions.len())
            .map(|i| {
                v2xw_threat::attack::falsified_count(
                    &out.honest[i],
                    &out.emissions[i],
                    i as u64 * NS_PER_S,
                    StationType::Vehicle,
                )
            })
            .sum();
        assert!(
            labelled > 0,
            "{k} produced no message the label rule calls falsified"
        );
    }
}
