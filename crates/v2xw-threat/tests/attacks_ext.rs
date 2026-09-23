//! The attack families 07-threats-and-detection.md §2.2 adds: each renders the documented
//! mechanism, touches nothing else, and refuses visibly when it has not declared the
//! capability it needs.
//!
//! "Refuses visibly" is the half that matters. An attacker that silently renders nothing
//! is indistinguishable, in every metric the run produces, from an attack nobody detected —
//! so every refusal bumps a counter a test can assert on.

use std::collections::BTreeSet;

use v2xw_core::ids::NodeId;
use v2xw_core::time::{NS_PER_S, SimTime};
use v2xw_threat::attack::{AttackAction, Attacker, AttackerView, Emission, HonestClaim, JamProfile};
use v2xw_threat::attack_ext::{
    ExtendedAttackKind, ExtendedAttacker, ExtendedAttackerParams, LaneHint,
};
use v2xw_threat::capability::{AttackSchedule, Capabilities};
use v2xw_threat::ctx::CollectingCtx;
use v2xw_threat::obs::{
    ObservedKind, ObservedMessage, RegionId, SelfBelief, StationType, VerificationState,
};

const TX: NodeId = NodeId::new(2);
const SIGNER: [u8; 8] = [0xAA; 8];
const SPEED: f64 = 15.0;

fn always() -> AttackSchedule {
    AttackSchedule {
        from: 0,
        to: u64::MAX,
        ..AttackSchedule::default()
    }
}

fn ghost_signers(n: u8) -> Vec<[u8; 8]> {
    (0..n).map(|i| [0xB0 | i, 1, 2, 3, 4, 5, 6, 7]).collect()
}

fn honest_at(t: SimTime) -> HonestClaim {
    HonestClaim {
        x_m: SPEED * v2xw_core::time::ns_to_secs(t),
        y_m: 0.0,
        speed_mps: SPEED,
        heading_rad: 0.0,
    }
}

fn heard(signer: u8, x_m: f64, t: SimTime) -> ObservedMessage {
    ObservedMessage {
        signer: [signer, 9, 9, 9, 9, 9, 9, 9],
        kind: ObservedKind::Beacon,
        received_at: t,
        claimed_generation_time: t,
        claimed_x_m: x_m,
        claimed_y_m: 0.0,
        claimed_speed_mps: 12.0,
        claimed_heading_rad: 0.0,
        claimed_pos_confidence_m: 2.0,
        repetitions: 1,
        cert_valid_from: 0,
        cert_valid_to: u64::MAX,
        station_type: StationType::Vehicle,
        verification: VerificationState::Valid,
    }
}

struct Harness {
    ctx: CollectingCtx,
    attacker: ExtendedAttacker,
}

impl Harness {
    fn new(kind: ExtendedAttackKind, caps: Capabilities, lanes: Vec<LaneHint>) -> Self {
        Self::with(
            ExtendedAttackerParams::new(kind),
            caps,
            lanes,
            ghost_signers(6),
            always(),
        )
    }

    fn with(
        params: ExtendedAttackerParams,
        caps: Capabilities,
        lanes: Vec<LaneHint>,
        signers: Vec<[u8; 8]>,
        schedule: AttackSchedule,
    ) -> Self {
        Self {
            ctx: CollectingCtx::new(20_260_922),
            attacker: ExtendedAttacker::new(TX, params, caps, schedule, signers, lanes),
        }
    }

    /// One generation interval: `rx` is what this node heard since the last one.
    fn step(&mut self, t: SimTime, rx: &[ObservedMessage]) -> (Emission, Vec<AttackAction>) {
        let honest = honest_at(t);
        let me = SelfBelief {
            node: TX,
            believed_time: t,
            x_m: honest.x_m,
            y_m: honest.y_m,
            radio_range_m: 500.0,
        };
        let view = AttackerView {
            own_rx: rx,
            own_credentials: &[],
            crl_revocations_seen: None,
            own_belief: me,
            honest,
            believed_time: t,
        };
        self.ctx.set_now(t);
        let mut out = Emission::honest(SIGNER, honest, t, 0, u64::MAX);
        self.attacker.observe(&mut self.ctx, &view);
        let actions = self.attacker.act(&mut self.ctx, &view, &mut out);
        (out, actions)
    }
}

fn assert_own_claim_untouched(e: &Emission, t: SimTime) {
    let h = honest_at(t);
    assert_eq!(e.x_m, h.x_m, "the attacker's own claimed x moved");
    assert_eq!(e.y_m, h.y_m, "the attacker's own claimed y moved");
    assert_eq!(e.speed_mps, h.speed_mps, "its own claimed speed moved");
    assert_eq!(e.heading_rad, h.heading_rad, "its own claimed heading moved");
}

// ---------------------------------------------------------------------------------------
// Ghost vehicles
// ---------------------------------------------------------------------------------------

#[test]
fn ghosts_are_placed_along_the_lane_the_attacker_knows() {
    // A lane running north, an attacker claiming to head east: with map knowledge the
    // ghosts go along the LANE, which is what makes them survive an off-road check.
    let lane = LaneHint::new(0.0, -500.0, 0.0, 500.0);
    let mut h = Harness::new(
        ExtendedAttackKind::GhostVehicles,
        Capabilities::insider(20),
        vec![lane],
    );
    let (out, actions) = h.step(0, &[]);
    assert_eq!(out.ghosts.len(), 5, "F2MD SybilVehNumber is 5");
    assert_own_claim_untouched(&out, 0);
    let north = core::f64::consts::FRAC_PI_2;
    for (i, g) in out.ghosts.iter().enumerate() {
        let along = 5.0 * (i as f64 + 1.0);
        let lateral = if i % 2 == 0 { 2.0 } else { -2.0 };
        assert!(
            (g.y_m - along).abs() < 1e-9,
            "ghost {i} is not {along} m along the lane: {}",
            g.y_m
        );
        assert!((g.x_m + lateral).abs() < 1e-9, "ghost {i} lateral offset");
        assert!((g.heading_rad - north).abs() < 1e-9, "ghost {i} heading");
        assert!(g.ghosts.is_empty(), "a ghost may not carry ghosts");
    }
    let signers: BTreeSet<[u8; 8]> = out.ghosts.iter().map(|g| g.signer).collect();
    assert_eq!(signers.len(), 5, "every ghost is a distinct identity");
    assert!(!signers.contains(&SIGNER), "a ghost is not the attacker");
    assert_eq!(
        actions.iter().filter(|a| a.name() == "Ghost").count(),
        5,
        "every ghost is on the ground-truth channel"
    );
}

#[test]
fn a_mapless_attacker_places_its_ghosts_along_its_own_heading() {
    // No declared map: the best it can do is its own road. Worse at evading `mapOffRoad`,
    // which is the point of declaring map knowledge in the first place.
    let mut h = Harness::new(
        ExtendedAttackKind::GhostVehicles,
        Capabilities::outsider(),
        Vec::new(),
    );
    let (out, _) = h.step(0, &[]);
    assert_eq!(out.ghosts.len(), 5);
    assert!((out.ghosts[0].x_m - 5.0).abs() < 1e-9);
    assert!((out.ghosts[0].y_m - 2.0).abs() < 1e-9);
    assert!((out.ghosts[0].heading_rad - 0.0).abs() < 1e-9);
}

#[test]
fn a_ghost_attacker_with_no_credentials_refuses_visibly() {
    // No ghost credentials, no ghosts — and a counter rather than silence.
    let mut h = Harness::with(
        ExtendedAttackerParams::new(ExtendedAttackKind::GhostVehicles),
        Capabilities::outsider(),
        Vec::new(),
        Vec::new(),
        always(),
    );
    let (out, actions) = h.step(0, &[]);
    assert!(out.ghosts.is_empty());
    assert!(actions.is_empty());
    assert_eq!(h.attacker.refused(), 1);
}

// ---------------------------------------------------------------------------------------
// Relay (wormhole) replay
// ---------------------------------------------------------------------------------------

#[test]
fn a_relay_retransmits_a_captured_frame_verbatim() {
    let mut h = Harness::new(
        ExtendedAttackKind::RelayReplay,
        Capabilities::insider(20),
        Vec::new(),
    );
    // Nothing captured yet: the relay says so instead of relaying nothing quietly.
    let (out, actions) = h.step(0, &[]);
    assert!(out.ghosts.is_empty());
    assert!(actions.is_empty());
    assert_eq!(h.attacker.refused(), 1);

    // Seven distinct senders heard; the relay replays the one six receptions back.
    let rx: Vec<ObservedMessage> = (0..7u8)
        .map(|i| heard(i, 100.0 + f64::from(i), NS_PER_S))
        .collect();
    let (out, actions) = h.step(NS_PER_S, &rx);
    assert_eq!(h.attacker.captured(), 7);
    assert_eq!(out.ghosts.len(), 1);
    let relayed = &out.ghosts[0];
    let source = &rx[7 - 6];
    assert_eq!(relayed.signer, source.signer, "the captured signer is kept");
    assert_eq!(relayed.x_m, source.claimed_x_m);
    assert_eq!(relayed.speed_mps, source.claimed_speed_mps);
    assert_eq!(
        relayed.generation_time, source.claimed_generation_time,
        "a verbatim replay keeps the original generation time"
    );
    assert!(
        relayed.replayed,
        "the host must retransmit the stored frame, not re-sign it"
    );
    assert!(relayed.signature_valid, "the original bytes still verify");
    assert_own_claim_untouched(&out, NS_PER_S);
    assert!(matches!(actions[0], AttackAction::Replay { .. }));
}

#[test]
fn a_relay_does_not_capture_one_reception_twice() {
    let mut h = Harness::new(
        ExtendedAttackKind::RelayReplay,
        Capabilities::insider(20),
        Vec::new(),
    );
    let rx = vec![heard(1, 100.0, NS_PER_S)];
    h.step(NS_PER_S, &rx);
    h.step(2 * NS_PER_S, &rx);
    assert_eq!(h.attacker.captured(), 1, "same signer, same generation time");
}

// ---------------------------------------------------------------------------------------
// Oversized flooding, region misuse, phantom perception
// ---------------------------------------------------------------------------------------

#[test]
fn an_oversized_flood_claims_the_msdu_cap_and_the_f2md_burst() {
    let mut h = Harness::new(
        ExtendedAttackKind::OversizedFlood,
        Capabilities::insider(20),
        Vec::new(),
    );
    let (out, actions) = h.step(0, &[]);
    assert_eq!(out.payload_bytes, Some(v2xw_threat::MAX_MSDU_BYTES));
    assert_eq!(out.payload_bytes, Some(2_304));
    assert_eq!(out.repetitions, 4, "F2MD DosMultipleFreq");
    assert_own_claim_untouched(&out, 0);
    match &actions[0] {
        AttackAction::FalsifyOutgoing { fields, magnitude } => {
            assert_eq!(fields, &["payload".to_string(), "repetitions".to_string()]);
            assert_eq!(*magnitude, 2_304.0);
        }
        other => panic!("unexpected action {other:?}"),
    }
}

#[test]
fn certificate_region_misuse_states_a_region_and_edits_nothing_else() {
    let mut h = Harness::new(
        ExtendedAttackKind::WrongRegionCert,
        Capabilities::insider(20),
        Vec::new(),
    );
    let (out, actions) = h.step(0, &[]);
    assert_eq!(out.cert_region, Some(RegionId(840)));
    assert_own_claim_untouched(&out, 0);
    assert!(out.signature_valid, "the credential is valid; its region is not");
    assert_eq!(out.cert_valid_from, 0);
    assert_eq!(out.cert_valid_to, u64::MAX);
    assert_eq!(actions.len(), 1);
    // The frozen label rule has no term for this, which is why the extended rule exists.
    let honest = honest_at(0);
    assert!(!v2xw_threat::is_falsified(
        &honest,
        &out,
        0,
        StationType::Vehicle
    ));
    assert!(v2xw_threat::is_falsified_extended(
        &honest,
        &out,
        0,
        StationType::Vehicle,
        Some(RegionId(276)),
        v2xw_threat::MAX_MSDU_BYTES
    ));
}

#[test]
fn a_phantom_collective_perception_message_carries_objects_that_are_not_there() {
    let lane = LaneHint::new(-500.0, 0.0, 500.0, 0.0);
    let mut h = Harness::new(
        ExtendedAttackKind::FakeCpm,
        Capabilities::insider(20),
        vec![lane],
    );
    let (out, actions) = h.step(0, &[]);
    assert_eq!(out.perceived.len(), 5);
    assert_own_claim_untouched(&out, 0);
    // Placed along the lane the attacker knows, in the wire's hundredths of a metre.
    assert_eq!(out.perceived[0].x_cm, 500);
    assert_eq!(out.perceived[0].y_cm, 200);
    assert_eq!(out.perceived[0].quality, 15, "an attacker asserts the best");
    assert_eq!(out.perceived[1].x_cm, 1_000);
    assert_eq!(out.perceived[1].y_cm, -200);
    assert!(matches!(
        actions[0],
        AttackAction::ForgeObject { count: 5 }
    ));
    // One CPM, however many objects: the label counts messages.
    assert_eq!(
        v2xw_threat::falsified_count(&honest_at(0), &out, 0, StationType::Vehicle),
        1
    );
}

// ---------------------------------------------------------------------------------------
// Jamming
// ---------------------------------------------------------------------------------------

#[test]
fn a_jammer_without_the_capability_refuses_visibly() {
    for kind in [
        ExtendedAttackKind::ConstantJamming,
        ExtendedAttackKind::ReactiveJamming,
        ExtendedAttackKind::RandomDutyJamming,
    ] {
        let mut h = Harness::new(kind, Capabilities::insider(20), Vec::new());
        let (out, actions) = h.step(0, &[heard(1, 50.0, 0)]);
        assert!(out.raw_energy.is_none(), "{kind} jammed without declaring it");
        assert!(actions.is_empty());
        assert_eq!(h.attacker.refused(), 1, "{kind} refused silently");
    }
}

#[test]
fn a_constant_jammer_declares_the_measured_power_inside_its_envelope() {
    let mut h = Harness::new(
        ExtendedAttackKind::ConstantJamming,
        Capabilities::insider(20).jamming(),
        Vec::new(),
    );
    let (out, actions) = h.step(0, &[]);
    let burst = out.raw_energy.expect("a constant jammer always transmits");
    assert_eq!(burst.profile, JamProfile::Constant);
    // Puñal 2012's measured WARP jammer at 5.9 GHz, under the 23 dBm ITS-G5A envelope.
    assert_eq!(burst.power_dbm, 16.75);
    assert_eq!(burst.duration_us, 1_000_000);
    assert_eq!(burst.trigger_dbm, None);
    assert_own_claim_untouched(&out, 0);
    match &actions[0] {
        AttackAction::TransmitRaw { profile, power_dbm } => {
            assert_eq!(profile, "constant");
            assert_eq!(*power_dbm, 16.75);
        }
        other => panic!("unexpected action {other:?}"),
    }
}

#[test]
fn a_jammer_cannot_exceed_the_power_it_declared() {
    let mut caps = Capabilities::insider(20).jamming();
    caps.radio.max_power_dbm = 10.0;
    let mut h = Harness::new(ExtendedAttackKind::ConstantJamming, caps, Vec::new());
    let (out, _) = h.step(0, &[]);
    assert_eq!(out.raw_energy.unwrap().power_dbm, 10.0);
}

#[test]
fn a_reactive_jammer_fires_only_after_it_hears_something() {
    let mut h = Harness::new(
        ExtendedAttackKind::ReactiveJamming,
        Capabilities::insider(20).jamming(),
        Vec::new(),
    );
    // Silence: nothing to react to.
    let (quiet, actions) = h.step(0, &[]);
    assert!(quiet.raw_energy.is_none());
    assert!(actions.is_empty());
    // A reception is the belief-side proxy for a busy channel; the real −75 dBm trigger
    // travels on the burst for the host to apply.
    let (loud, actions) = h.step(NS_PER_S, &[heard(1, 50.0, NS_PER_S)]);
    let burst = loud.raw_energy.expect("it heard something");
    assert_eq!(burst.profile, JamProfile::Reactive);
    assert_eq!(burst.trigger_dbm, Some(-75.0));
    assert_eq!(actions.len(), 1);
}

#[test]
fn a_random_duty_jammer_pulses_from_its_own_stream_and_repeats_exactly() {
    let schedule = AttackSchedule {
        from: 0,
        to: u64::MAX,
        duty_cycle: 0.5,
        ..AttackSchedule::default()
    };
    let run = || {
        let mut h = Harness::with(
            ExtendedAttackerParams::new(ExtendedAttackKind::RandomDutyJamming),
            Capabilities::insider(20).jamming(),
            Vec::new(),
            Vec::new(),
            schedule.clone(),
        );
        (0..40u64)
            .map(|i| h.step(i * NS_PER_S, &[]).0.raw_energy.is_some())
            .collect::<Vec<bool>>()
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "the same seed must pulse the same way");
    let on = a.iter().filter(|x| **x).count();
    assert!(on > 0 && on < 40, "a duty cycle pulses on AND off: {on}/40");
}

// ---------------------------------------------------------------------------------------
// Across the whole group
// ---------------------------------------------------------------------------------------

#[test]
fn no_new_family_edits_the_attackers_own_claim() {
    // Every one of these attacks works through extra identities, extra bytes, the
    // envelope's region or raw energy. None of them falsifies its own kinematics — that is
    // the ported catalogue's job, and a rendering that did both would make a per-family
    // detection number a blend of two families.
    for kind in ExtendedAttackKind::ALL {
        let mut h = Harness::new(kind, Capabilities::insider(20).jamming(), Vec::new());
        let (out, _) = h.step(NS_PER_S, &[heard(1, 50.0, NS_PER_S)]);
        assert_own_claim_untouched(&out, NS_PER_S);
        assert!(!out.suppressed, "{kind} suppressed a message");
        assert!(out.infra.is_empty(), "{kind} claimed to be infrastructure");
    }
}

#[test]
fn a_schedule_keeps_every_new_family_dormant_outside_its_window() {
    let schedule = AttackSchedule {
        from: 10 * NS_PER_S,
        to: 20 * NS_PER_S,
        ..AttackSchedule::default()
    };
    for kind in ExtendedAttackKind::ALL {
        let mut h = Harness::with(
            ExtendedAttackerParams::new(kind),
            Capabilities::insider(20).jamming(),
            Vec::new(),
            ghost_signers(6),
            schedule.clone(),
        );
        let (before, actions) = h.step(5 * NS_PER_S, &[heard(1, 50.0, 5 * NS_PER_S)]);
        assert!(actions.is_empty(), "{kind} acted before its window");
        assert!(before.ghosts.is_empty());
        assert!(before.raw_energy.is_none());
        assert_eq!(before.payload_bytes, None);
        assert_eq!(before.cert_region, None);
        assert!(before.perceived.is_empty());
    }
}

#[test]
fn every_new_family_has_a_card_that_validates_and_cites_or_plans_every_default() {
    for kind in ExtendedAttackKind::ALL {
        let card = v2xw_threat::attack_ext::card(&ExtendedAttackerParams::new(kind));
        card.validate()
            .unwrap_or_else(|e| panic!("{kind}: invalid card: {e}"));
        card.check_api_version().unwrap();
        assert!(!card.parameters.is_empty());
        assert!(!card.sources.is_empty());
        for p in &card.parameters {
            let cited = p.source.kind != v2xw_core::card::SourceKind::TodoCalibrate;
            let planned = p.calibration.as_ref().is_some_and(|s| !s.trim().is_empty());
            assert!(cited || planned, "{kind}: {} is neither", p.name);
        }
    }
}
