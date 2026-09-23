//! The roadside-unit runtime of 06-node-models.md §3: roles, failure states, and
//! store-and-forward, end to end.
//!
//! # Faults injected to prove these checks can fail
//!
//! 1. The `roles.contains(RsuRole::SpatMapBroadcast)` test in `RsuRuntime::broadcast`
//!    removed — `a_unit_with_no_roles_broadcasts_nothing` fails, because every unit then
//!    broadcasts SPaT at 10 Hz whether the scenario asked for it or not.
//! 2. `RsuRuntime::step` made to call `broadcast` regardless of `state.transmits()` —
//!    `a_down_unit_transmits_nothing` fails.
//! 3. `Backhaul::is_up` made to ignore the state —
//!    `a_report_is_held_while_the_backhaul_is_down_and_goes_when_it_returns` fails, because
//!    the report leaves a unit whose backhaul is gone.
//! 4. The `NodeState::Compromised` branch in `step` removed —
//!    `a_compromised_unit_neither_broadcasts_nor_forwards` fails.
//! 5. The verification test in `run_verifications`'s report-forwarding branch relaxed to
//!    accept `Unverified` — `an_unverified_report_is_not_forwarded` fails, which is the
//!    check that stops the infrastructure being used as an amplifier.

use v2xw_core::belief::{FixQuality, PositionEstimate};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::rng::RngRegistry;
use v2xw_core::time::{NS_PER_MS, NS_PER_S};
use v2xw_msg::MsgType;
use v2xw_node::ctx::NodeRuntimeCtx;
use v2xw_node::rsu::{
    Backhaul, ForwardKind, RsuConfig, RsuRole, RsuRoles, RsuRuntime, RsuStepOutcome,
};
use v2xw_node::runtime::RxFrame;
use v2xw_node::stores::{CredState, CredentialHandle, pseudo_signer};
use v2xw_node::telemetry::NodeState;

const UNIT: NodeId = NodeId::new(500);
const VEHICLE: NodeId = NodeId::new(42);

/// A surveyed mast: a position with no GNSS error in it, handed in from outside.
fn surveyed(pos: Vec3) -> PositionEstimate {
    let mut p = PositionEstimate::no_fix(0);
    p.pos = pos;
    p.heading_rad = 0.0;
    p.semi_major_m = 0.0;
    p.semi_minor_m = 0.0;
    p.fix = FixQuality::Rtk;
    p
}

fn credential(node: NodeId, j: u32) -> CredentialHandle {
    CredentialHandle {
        digest: pseudo_signer(node, j),
        cert_coer: vec![0u8; 120],
        key: v2xw_sec::KeyId(u64::from(j)),
        i_period: 100,
        j_index: j,
        valid_from: 0,
        valid_until: 10_000 * NS_PER_S,
        state: CredState::Active,
    }
}

fn unit(roles: RsuRoles) -> RsuRuntime {
    let profile = v2xw_node::profiles::get(v2xw_node::profiles::DEFAULT_RSU)
        .expect("the default roadside profile")
        .clone();
    let mut u = RsuRuntime::new(
        UNIT,
        profile,
        RsuConfig::with_roles(roles),
        Backhaul::fibre(),
        0,
    );
    u.stores_mut().crl.set_period(100);
    u.stores_mut().certs.insert(credential(UNIT, 0));
    u.set_belief(surveyed(Vec3::new(100.0, 100.0, 6.0)));
    u
}

/// Runs the unit for `steps` 100 ms ticks and returns everything it produced.
fn run(u: &mut RsuRuntime, steps: u64) -> Vec<RsuStepOutcome> {
    let rng = RngRegistry::new(51);
    (0..=steps)
        .map(|k| {
            let mut ctx = NodeRuntimeCtx::new(k * 100 * NS_PER_MS, &rng);
            u.step(&mut ctx, Vec::new())
        })
        .collect()
}

fn count(outcomes: &[RsuStepOutcome], ty: MsgType) -> usize {
    outcomes
        .iter()
        .flat_map(|o| o.transmissions.iter())
        .filter(|t| t.msg_type == ty)
        .count()
}

/// A unit with the SPaT/MAP role broadcasts at the rates 04-models.md §8.1 gives as the
/// defaults: SPaT 10 Hz, MAP 1 Hz.
///
/// Eleven SPaTs and two MAPs over a second of 100 ms ticks, because both endpoints of the
/// interval are inside it: `t = 0, 100, …, 1000` is eleven SPaT instants and `t = 0, 1000`
/// is two MAP instants.
#[test]
fn a_unit_with_the_spat_map_role_broadcasts_at_ten_and_one_hertz() {
    let mut u = unit(RsuRoles::NONE.with(RsuRole::SpatMapBroadcast));
    let outcomes = run(&mut u, 10);
    assert_eq!(count(&outcomes, MsgType::Spat), 11, "SPaT at 10 Hz");
    assert_eq!(count(&outcomes, MsgType::Map), 2, "MAP at 1 Hz");
    assert_eq!(count(&outcomes, MsgType::Crl), 0, "no list is installed");
    assert_eq!(count(&outcomes, MsgType::Wsa), 0, "the role was not given");

    // The payload lengths differ, because they are two different messages with two
    // different size-model rows — and every one of them is a modelled length, which the
    // counter says out loud.
    let spat_bytes: Vec<u32> = outcomes
        .iter()
        .flat_map(|o| o.transmissions.iter())
        .filter(|t| t.msg_type == MsgType::Spat)
        .map(|t| t.bytes)
        .collect();
    let map_bytes: Vec<u32> = outcomes
        .iter()
        .flat_map(|o| o.transmissions.iter())
        .filter(|t| t.msg_type == MsgType::Map)
        .map(|t| t.bytes)
        .collect();
    assert!(spat_bytes.iter().all(|b| *b > 0));
    assert_ne!(spat_bytes[0], map_bytes[0]);
    assert_eq!(u.modelled_broadcasts(), 13);
    assert_eq!(u.unsized_broadcasts(), 0);

    // And every frame really is signed: the envelope is real even where the payload is a
    // placeholder.
    for tx in outcomes.iter().flat_map(|o| o.transmissions.iter()) {
        let signed = tx.signed.as_ref().expect("a signed frame");
        assert!(signed.envelope_bytes() > 0, "a real 1609.2 envelope");
        assert_eq!(tx.bytes, signed.bytes_on_wire());
    }
}

/// A unit with no roles broadcasts nothing at all, however long it runs.
///
/// This is the behaviour the Phase 2 engine wiring had by accident — an OBU runtime with
/// its message services switched off — and now has on purpose.
#[test]
fn a_unit_with_no_roles_broadcasts_nothing() {
    let mut u = unit(RsuRoles::NONE);
    let outcomes = run(&mut u, 30);
    assert!(
        outcomes.iter().all(|o| o.transmissions.is_empty()),
        "a role-less unit is a receiver"
    );
    assert_eq!(u.unsized_broadcasts(), 0);
    assert_eq!(u.modelled_broadcasts(), 0);
}

/// A unit with the WSA role broadcasts nothing and counts every attempt, because no WSA
/// encoder and no WSA size-model row exists.
///
/// A modelling gap reported rather than filled in: 04-models.md §8.1 records the IEEE
/// 1609.3 `RepeatRate` semantics as UNVERIFIED and the size model covers BSM, SPaT, MAP,
/// PSM, SRM and SSM — not the WSA.
#[test]
fn the_wsa_role_is_declared_and_unsizable() {
    let mut u = unit(RsuRoles::NONE.with(RsuRole::WsaBroadcast));
    let outcomes = run(&mut u, 60);
    assert_eq!(count(&outcomes, MsgType::Wsa), 0);
    assert!(
        u.unsized_broadcasts() >= 2,
        "one attempt per 5 s over 6 s: {}",
        u.unsized_broadcasts()
    );

    // Give it a length and it broadcasts, which is what makes the refusal above about the
    // missing model and not about the role.
    let mut sized = unit(RsuRoles::NONE.with(RsuRole::WsaBroadcast));
    sized.set_payload(MsgType::Wsa, vec![0xA5; 40]);
    let outcomes = run(&mut sized, 60);
    assert!(count(&outcomes, MsgType::Wsa) >= 2);
    assert_eq!(sized.unsized_broadcasts(), 0);
    assert_eq!(
        sized.modelled_broadcasts(),
        0,
        "supplied octets are not a modelled length"
    );
}

/// An installed revocation list goes out once, and only once, unless a scenario sets a
/// repeat period.
#[test]
fn an_installed_list_goes_out_once_and_then_on_a_repeat_period() {
    let mut u = unit(RsuRoles::NONE.with(RsuRole::CrlDistribution));
    u.install_crl(4_000, 0);
    let outcomes = run(&mut u, 60);
    assert_eq!(
        count(&outcomes, MsgType::Crl),
        1,
        "no clause fixes a cadence, so one broadcast and then silence"
    );
    // The bytes on the air are the backend's length plus a real envelope.
    let tx = outcomes
        .iter()
        .flat_map(|o| o.transmissions.iter())
        .find(|t| t.msg_type == MsgType::Crl)
        .expect("the broadcast");
    assert!(tx.bytes > 4_000, "the envelope is on top of the list");

    // With a repeat period it goes out again.
    let mut repeating = RsuRuntime::new(
        UNIT,
        v2xw_node::profiles::get(v2xw_node::profiles::DEFAULT_RSU)
            .expect("profile")
            .clone(),
        RsuConfig {
            crl_repeat: Some(v2xw_core::time::Duration::from_secs(2)),
            ..RsuConfig::with_roles(RsuRoles::NONE.with(RsuRole::CrlDistribution))
        },
        Backhaul::fibre(),
        0,
    );
    repeating.stores_mut().crl.set_period(100);
    repeating.stores_mut().certs.insert(credential(UNIT, 0));
    repeating.set_belief(surveyed(Vec3::new(100.0, 100.0, 6.0)));
    let outcomes = run(&mut repeating, 60);
    assert!(
        count(&outcomes, MsgType::Crl) >= 3,
        "one on installation and one per 2 s over 6 s: {}",
        count(&outcomes, MsgType::Crl)
    );
}

/// A report is held while the backhaul is down and goes when it comes back, with its
/// arrival stamped by the link's own delay.
#[test]
fn a_report_is_held_while_the_backhaul_is_down_and_goes_when_it_returns() {
    let rng = RngRegistry::new(52);
    let mut u = unit(RsuRoles::NONE.with(RsuRole::ReportForwarding));
    u.set_backhaul(Backhaul::none());
    assert!(u.accept_uplink(ForwardKind::MisbehaviourReport, 1_200, 0));

    // No backhaul: held.
    let mut ctx = NodeRuntimeCtx::new(100 * NS_PER_MS, &rng);
    let out = u.step(&mut ctx, Vec::new());
    assert!(out.forwarded.is_empty());
    assert_eq!(u.forward_queue().len(), 1);

    // A backhaul appears: it goes, with a departure and an arrival.
    u.set_backhaul(Backhaul::cellular());
    let mut ctx = NodeRuntimeCtx::new(200 * NS_PER_MS, &rng);
    let out = u.step(&mut ctx, Vec::new());
    assert_eq!(out.forwarded.len(), 1);
    assert_eq!(out.forwarded[0].kind, ForwardKind::MisbehaviourReport);
    assert_eq!(out.forwarded[0].stored_at, 0);
    assert_eq!(out.forwarded[0].forwarded_at, Some(200 * NS_PER_MS));
    let arrival = out.forwarded[0].arrives_at.expect("an arrival");
    assert!(
        arrival > 200 * NS_PER_MS,
        "the link has a latency and a capacity"
    );
    assert!(u.forward_queue().is_empty());
    assert_eq!(u.forward_queue().forwarded(), 1);

    // A `down` unit holds it too, which is the state the design defines as "backhaul
    // lost" rather than as a slower link.
    assert!(u.accept_uplink(ForwardKind::MisbehaviourReport, 900, 300 * NS_PER_MS));
    u.set_state(NodeState::Down);
    let mut ctx = NodeRuntimeCtx::new(400 * NS_PER_MS, &rng);
    assert!(u.step(&mut ctx, Vec::new()).forwarded.is_empty());
    assert_eq!(u.forward_queue().len(), 1);
    u.set_state(NodeState::Active);
    let mut ctx = NodeRuntimeCtx::new(500 * NS_PER_MS, &rng);
    assert_eq!(u.step(&mut ctx, Vec::new()).forwarded.len(), 1);
}

/// A degraded unit still forwards, and it forwards slower — the multiplier of
/// 06-node-models.md §3 applied to the backhaul's latency.
#[test]
fn a_degraded_unit_forwards_slower() {
    let rng = RngRegistry::new(53);
    fn arrival(state: NodeState, rng: &RngRegistry) -> u64 {
        let mut u = unit(RsuRoles::NONE.with(RsuRole::ReportForwarding));
        u.set_state(state);
        u.accept_uplink(ForwardKind::MisbehaviourReport, 1_200, 0);
        let mut ctx = NodeRuntimeCtx::new(100 * NS_PER_MS, rng);
        let out = u.step(&mut ctx, Vec::new());
        out.forwarded[0].arrives_at.expect("an arrival")
    }
    let nominal = arrival(NodeState::Active, &rng);
    let degraded = arrival(NodeState::Degraded, &rng);
    assert!(
        degraded > nominal,
        "degraded {degraded} must be slower than nominal {nominal}"
    );
}

/// A `down` unit transmits nothing, whatever roles it carries.
#[test]
fn a_down_unit_transmits_nothing() {
    let mut u = unit(RsuRoles::all());
    u.install_crl(4_000, 0);
    u.set_state(NodeState::Down);
    let outcomes = run(&mut u, 30);
    assert!(outcomes.iter().all(|o| o.transmissions.is_empty()));
    // And it is not counted as a broadcast that could not be sized: it was never due.
    assert_eq!(u.unsized_broadcasts(), 0);
}

/// A compromised unit neither broadcasts nor forwards on its own: the attacker decides
/// what it puts on the air, and the withholding is counted so a vanished report is
/// attributable.
#[test]
fn a_compromised_unit_neither_broadcasts_nor_forwards() {
    let rng = RngRegistry::new(54);
    let mut u = unit(
        RsuRoles::NONE
            .with(RsuRole::SpatMapBroadcast)
            .with(RsuRole::ReportForwarding),
    );
    u.accept_uplink(ForwardKind::MisbehaviourReport, 1_200, 0);
    u.set_state(NodeState::Compromised);

    let outcomes = run(&mut u, 10);
    assert!(
        outcomes.iter().all(|o| o.transmissions.is_empty()),
        "the runtime withholds; the attacker plug-in transmits"
    );
    assert!(outcomes.iter().all(|o| o.forwarded.is_empty()));
    assert_eq!(u.forward_queue().len(), 1, "the report is still held");
    assert!(u.compromised_suppressed() > 0);

    // The same unit, uncompromised, does both — so the assertions above are about the
    // state and not about the roles.
    let mut honest = unit(
        RsuRoles::NONE
            .with(RsuRole::SpatMapBroadcast)
            .with(RsuRole::ReportForwarding),
    );
    honest.accept_uplink(ForwardKind::MisbehaviourReport, 1_200, 0);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let out = honest.step(&mut ctx, Vec::new());
    assert!(!out.transmissions.is_empty());
    assert_eq!(out.forwarded.len(), 1);
    assert_eq!(honest.compromised_suppressed(), 0);
}

fn report_frame(valid: bool) -> RxFrame {
    RxFrame {
        signer: Some(pseudo_signer(VEHICLE, 0)),
        msg_type: MsgType::Mbr,
        bytes: 1_200,
        claimed_pos: Some(Vec3::new(120.0, 100.0, 1.5)),
        claimed_speed_mps: 12.0,
        claimed_heading_rad: 0.0,
        claimed_generation_time: 0,
        full_certificate: true,
        signature_valid: valid,
        claimed_cert_period: 100,
        claimed_linkage: None,
        spdu: None,
    }
}

/// A verified misbehaviour report taken off the air goes into store-and-forward, on a unit
/// that carries the role.
#[test]
fn a_verified_report_off_the_air_is_forwarded() {
    let rng = RngRegistry::new(55);
    let mut u = unit(RsuRoles::NONE.with(RsuRole::ReportForwarding));
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let out = u.step(&mut ctx, vec![report_frame(true)]);
    assert_eq!(out.delivered.len(), 1);
    assert_eq!(out.forwarded.len(), 1, "verified, so it goes: {out:?}");
    assert_eq!(out.forwarded[0].bytes, 1_200);
    assert_eq!(u.uplink_accepted(), 1);

    // A unit without the role delivers it to its own applications and forwards nothing.
    let mut bare = unit(RsuRoles::NONE);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let out = bare.step(&mut ctx, vec![report_frame(true)]);
    assert_eq!(out.delivered.len(), 1);
    assert!(out.forwarded.is_empty());
    assert_eq!(bare.uplink_accepted(), 0);
}

/// An unverified report is not forwarded. A relay that forwarded what it had not checked
/// would let an attacker use the infrastructure as an amplifier.
#[test]
fn an_unverified_report_is_not_forwarded() {
    let rng = RngRegistry::new(56);
    let mut u = unit(RsuRoles::NONE.with(RsuRole::ReportForwarding));
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let out = u.step(&mut ctx, vec![report_frame(false)]);
    assert_eq!(out.delivered.len(), 1, "it is still delivered, and flagged");
    assert_eq!(
        out.delivered[0].verification,
        v2xw_node::stores::VerificationState::Invalid
    );
    assert!(out.forwarded.is_empty());
    assert_eq!(u.uplink_accepted(), 0);
    assert!(u.forward_queue().is_empty());
}

/// The unit's telemetry carries the store-and-forward depth in the §3.5.2 outbox fields,
/// which is what puts a held report in front of a reader without a second pair of fields.
#[test]
fn the_telemetry_reports_the_held_queue() {
    let rng = RngRegistry::new(57);
    let mut u = unit(RsuRoles::NONE.with(RsuRole::ReportForwarding));
    u.set_backhaul(Backhaul::none());
    for k in 0..3u32 {
        u.accept_uplink(ForwardKind::MisbehaviourReport, 1_000 + k, 0);
    }
    let mut telemetry = None;
    for k in 0..=11u64 {
        let mut ctx = NodeRuntimeCtx::new(k * 100 * NS_PER_MS, &rng);
        if let Some(t) = u.step(&mut ctx, Vec::new()).telemetry {
            telemetry = Some(t);
        }
    }
    let t = telemetry.expect("a window closed inside 1.1 s");
    assert_eq!(t.node_id, UNIT.index());
    assert_eq!(t.outbox_msgs, 3);
    assert_eq!(t.outbox_bytes, 3_003);
    assert_eq!(t.node_state, NodeState::Active.code());
    // `verify-all`, whatever the vehicles run: a unit that forwards a report has to have
    // verified it.
    assert_eq!(t.verify_policy, 0);
}

/// Two runs of the same unit produce the same broadcasts, to the nanosecond.
#[test]
fn the_unit_is_deterministic() {
    fn go() -> Vec<(MsgType, u32, u64)> {
        let mut u = unit(RsuRoles::all());
        u.install_crl(4_000, 0);
        run(&mut u, 25)
            .into_iter()
            .flat_map(|o| o.transmissions)
            .map(|t| (t.msg_type, t.bytes, t.ready_at))
            .collect()
    }
    let a = go();
    assert!(!a.is_empty());
    assert_eq!(a, go());
}
