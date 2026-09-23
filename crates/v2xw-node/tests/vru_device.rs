//! The VRU device runtime: the two constraints that decide whether a pedestrian is heard.
//!
//! # Faults injected to prove these checks can fail
//!
//! 1. `DutyCycle::gate` made to return `Transmit` unconditionally —
//!    `the_duty_cycle_cap_is_what_stops_a_burst` and
//!    `a_psm_and_a_vam_cannot_go_back_to_back` both fail.
//! 2. `PowerBudget::spend` made to ignore `battery_j` —
//!    `a_device_with_a_battery_goes_quiet_when_it_is_spent` fails.
//! 3. `vam_gen_params` given EN 302 637-2's 1,000 ms `T_GenCamMax` —
//!    `a_stationary_pedestrian_heart_beats_at_t_gen_vam_max` fails, counting five VAMs
//!    where the standard allows one.
//! 4. The `kind.receives()` branch in `VruDeviceRuntime::step` removed —
//!    `a_beacon_has_no_receiver_and_says_so` fails.

use v2xw_core::belief::{FixQuality, PositionEstimate};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::rng::RngRegistry;
use v2xw_core::time::{Duration, NS_PER_MS, NS_PER_S, SimTime};
use v2xw_msg::MsgType;
use v2xw_node::ctx::NodeRuntimeCtx;
use v2xw_node::runtime::RxFrame;
use v2xw_node::stores::{CredState, CredentialHandle, pseudo_signer};
use v2xw_node::vru::{
    PayloadProvenance, PowerBudget, VruConfig, VruDeviceKind, VruDeviceRuntime, VruServices,
};

const DEVICE: NodeId = NodeId::new(700);

fn belief(pos: Vec3, speed: f64) -> PositionEstimate {
    let mut p = PositionEstimate::no_fix(0);
    p.pos = pos;
    p.vel = Vec3::new(speed, 0.0, 0.0);
    p.heading_rad = 0.0;
    p.semi_major_m = 4.0;
    p.semi_minor_m = 3.0;
    p.fix = FixQuality::ThreeD;
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

fn device(services: VruServices) -> VruDeviceRuntime {
    let mut d = VruDeviceRuntime::new(
        DEVICE,
        v2xw_node::profiles::get(v2xw_node::profiles::GENERIC_VRU_DEVICE)
            .expect("the shipped VRU-device profile")
            .clone(),
        VruConfig {
            services,
            ..VruConfig::default()
        },
        0,
    );
    d.stores_mut().crl.set_period(100);
    d.stores_mut().certs.insert(credential(DEVICE, 0));
    d.set_belief(belief(Vec3::ZERO, 1.4));
    d
}

/// The shipped profile is the only `vru-device` one and it publishes no compute figures —
/// which is the honest state of the design set, and worth failing on if it silently
/// acquires some.
#[test]
fn the_shipped_profile_publishes_no_compute_figures_and_still_signs() {
    let p = v2xw_node::profiles::get(v2xw_node::profiles::GENERIC_VRU_DEVICE)
        .expect("the shipped profile");
    assert_eq!(p.kind, v2xw_node::profile::NodeKind::VruDevice);
    assert_eq!(p.hsm.kind, v2xw_node::profile::HsmKind::None);
    assert!(p.hsm.ops.is_empty(), "rule H2");
    assert!(p.cpu.cores.is_missing(), "no core count is published");
    assert!(p.ram_bytes.is_missing());
    assert!(p.power_w.is_missing(), "the field the whole question turns on");

    // But the software cost table has the flagged proxy figures, so the device can sign —
    // and it signs on the CPU, because rule H2 sends every operation there.
    let (cost, where_) = p.op_cost("ecdsa-p256-sign").expect("a software cost");
    assert_eq!(cost, Duration::from_micros(244));
    assert_eq!(where_, v2xw_node::profile::RunsOn::Cpu);
    assert_eq!(
        p.software_crypto["ecdsa-p256-sign"].status.as_ref().map(
            v2xw_node::profile::FieldStatus::as_str
        ),
        Some("proxy"),
        "the figure is a class match, not a device match, and must say so"
    );

    // And the card validates, which means every unpublished field carries a plan.
    p.card().validate().expect("the profile card validates");
    assert!(p.todo_calibrate_count() > 5, "the gaps are real and listed");
}

/// A stationary pedestrian heart-beats at `T_GenVamMax` — five seconds — and not at the
/// CAM's one second.
///
/// This is the number that decides whether a pedestrian standing at a kerb is in a
/// vehicle's neighbour table at all: at 5 s and a 3 s `T_neighbor`, they are not.
#[test]
fn a_stationary_pedestrian_heart_beats_at_t_gen_vam_max() {
    let rng = RngRegistry::new(31);
    let mut d = device(VruServices::ETSI);
    d.set_belief(belief(Vec3::ZERO, 0.0));

    let mut sent = 0;
    for k in 0..=100u64 {
        let now = k * 100 * NS_PER_MS;
        let mut ctx = NodeRuntimeCtx::new(now, &rng);
        let out = d.step(&mut ctx, Vec::new(), 0.0);
        sent += out.transmissions.len();
        for tx in &out.transmissions {
            assert_eq!(tx.msg_type, MsgType::Vam);
        }
    }
    // Ten seconds: the first VAM, then t = 5 s and t = 10 s.
    assert_eq!(sent, 3, "T_GenVamMax is 5,000 ms, not 1,000");
    assert!(
        d.duty_cycle_refusals() == 0,
        "three frames in ten seconds cannot breach a 3 % duty cycle"
    );
}

/// A pedestrian who walks crosses the 4 m position threshold and transmits far more
/// often — the TS 103 300-3 dynamics trigger, evaluated against the device's own belief.
#[test]
fn a_walking_pedestrian_transmits_on_the_dynamics_trigger() {
    let rng = RngRegistry::new(32);
    let mut d = device(VruServices::ETSI);
    let mut sent = 0;
    for k in 0..=100u64 {
        let now = k * 100 * NS_PER_MS;
        // 1.4 m/s, the walking speed the social-force model uses: 4 m every 2.86 s.
        d.set_belief(belief(Vec3::new(1.4 * (now as f64) / 1e9, 0.0, 0.0), 1.4));
        let mut ctx = NodeRuntimeCtx::new(now, &rng);
        sent += d.step(&mut ctx, Vec::new(), 0.0).transmissions.len();
    }
    assert!(
        sent > 3,
        "a walking pedestrian must beat the 5 s heart-beat: {sent}"
    );
}

/// A PSM and a VAM cannot go out back to back: EN 302 571 demands 25 ms of idle between
/// two transmissions, and a dual-stack device that wants both at once gets one.
///
/// This is the constraint in its most visible form. It is not a queueing artefact — the
/// second message is *built and signed*, and the energy for building it is spent — and the
/// suppression is recorded with its cause.
#[test]
fn a_psm_and_a_vam_cannot_go_back_to_back() {
    let rng = RngRegistry::new(33);
    let mut d = device(VruServices::BOTH);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let out = d.step(&mut ctx, Vec::new(), 0.0);

    assert_eq!(out.transmissions.len(), 1, "one of the two goes: {out:?}");
    assert_eq!(out.suppressed.len(), 1);
    assert_eq!(out.suppressed[0].1, "duty-cycle-t-off");
    assert_eq!(d.duty_cycle_refusals(), 1);

    // The suppression is on the record, on `node.tx`, with the duty cycle at the moment of
    // the decision.
    let suppressions: Vec<&v2xw_core::ctx::OwnedRecord> = ctx
        .emitted()
        .iter()
        .filter(|r| r.channel == "node.tx")
        .collect();
    assert_eq!(suppressions.len(), 1);
    let json = suppressions[0].json_str().expect("utf-8");
    assert!(json.contains("duty-cycle-t-off"), "{json}");
    assert!(json.contains("\"sent\":false"), "{json}");

    // The PSM's own 1 Hz cadence is the next thing that comes due, well past the 25 ms
    // of idle, and it goes.
    let mut ctx = NodeRuntimeCtx::new(NS_PER_S, &rng);
    let out = d.step(&mut ctx, Vec::new(), 0.0);
    assert_eq!(out.transmissions.len(), 1, "{out:?}");
    assert_eq!(out.transmissions[0].msg_type, MsgType::Psm);
    assert!(out.suppressed.is_empty());
}

/// The 3 % duty cycle is what stops a burst, and the runtime honours it.
///
/// The cap is tightened for the test rather than the payload inflated, because what is
/// being checked here is the *wiring* — gate, suppression, cause, counter — and the
/// standard's own numbers are checked against the OFDM air time in `vru`'s own unit tests.
/// The second half of the test is the injected fault: with the floor disabled the identical
/// run transmits every message, so the assertion cannot be passing for another reason.
#[test]
fn the_duty_cycle_cap_is_what_stops_a_burst() {
    fn run(floor: v2xw_radio::En302571Floor) -> (usize, usize) {
        let rng = RngRegistry::new(34);
        let mut d = device(VruServices::ETSI);
        d.set_duty(v2xw_node::vru::DutyCycle::new(
            floor,
            v2xw_radio::Mcs::R6Qpsk12,
        ));
        let mut sent = 0;
        let mut suppressed = 0;
        for k in 0..=60u64 {
            let now = k * 100 * NS_PER_MS;
            // Move 5 m every step, so the dynamics trigger fires at every check and the
            // device asks for a VAM as often as T_GenVamMin allows.
            d.set_belief(belief(Vec3::new(5.0 * k as f64, 0.0, 0.0), 1.4));
            let mut ctx = NodeRuntimeCtx::new(now, &rng);
            let out = d.step(&mut ctx, Vec::new(), 0.0);
            sent += out.transmissions.len();
            suppressed += out
                .suppressed
                .iter()
                .filter(|(_, cause)| *cause == "duty-cycle-budget")
                .count();
        }
        (sent, suppressed)
    }

    // A 0.2 % cap: 2 ms of air time per second, which is three or four VAM-sized frames,
    // against ten a second requested. Tight enough to bind and loose enough to admit
    // some, which is what makes both halves of the assertion below meaningful.
    let tight = v2xw_radio::En302571Floor {
        duty_cycle_max: 0.002,
        ..v2xw_radio::En302571Floor::CONFORMANT
    };
    let (sent, suppressed) = run(tight);
    assert!(suppressed > 0, "the cap must bind");
    assert!(sent > 0, "and it must not refuse everything");

    let (unbounded, none_refused) = run(v2xw_radio::En302571Floor::disabled());
    assert_eq!(none_refused, 0, "a disabled floor refuses nothing");
    assert!(
        unbounded > sent,
        "the cap cost the device transmissions: {sent} against {unbounded}"
    );
}

/// A device with a battery goes quiet when it is spent, and the refusals say why.
#[test]
fn a_device_with_a_battery_goes_quiet_when_it_is_spent() {
    let rng = RngRegistry::new(35);
    let mut d = device(VruServices::ETSI);
    // Enough for a handful of frames and no more. One VAM at 23 dBm costs about 0.3 mJ.
    d.set_power(PowerBudget::accounting_only(23.0).with_battery_j(0.001));

    let mut sent = 0;
    let mut energy_refusals = 0;
    for k in 0..=200u64 {
        let now = k * 100 * NS_PER_MS;
        d.set_belief(belief(Vec3::new(5.0 * k as f64, 0.0, 0.0), 1.4));
        let mut ctx = NodeRuntimeCtx::new(now, &rng);
        let out = d.step(&mut ctx, Vec::new(), 0.0);
        sent += out.transmissions.len();
        energy_refusals += out
            .suppressed
            .iter()
            .filter(|(_, cause)| *cause == "energy")
            .count();
    }
    assert!(sent > 0, "it must transmit while it has charge");
    assert!(energy_refusals > 0, "and stop when it does not");
    assert_eq!(d.energy_refusals() as usize, energy_refusals);
    assert!(d.power().spent_j() > 0.0);
    assert!(
        d.power().remaining_j().expect("a configured battery") < 0.001,
        "the battery is drawn down"
    );

    // The same run with no battery configured transmits more, which is what makes the
    // battery the cause rather than something else in the loop.
    let mut unlimited = device(VruServices::ETSI);
    let mut unlimited_sent = 0;
    for k in 0..=200u64 {
        let now = k * 100 * NS_PER_MS;
        unlimited.set_belief(belief(Vec3::new(5.0 * k as f64, 0.0, 0.0), 1.4));
        let mut ctx = NodeRuntimeCtx::new(now, &rng);
        unlimited_sent += unlimited.step(&mut ctx, Vec::new(), 0.0).transmissions.len();
    }
    assert!(
        unlimited_sent > sent,
        "{sent} with a battery against {unlimited_sent} without"
    );
    assert_eq!(unlimited.energy_refusals(), 0);
    // And it still accounts the energy, so a run with no battery still reports what one
    // would have lost.
    assert!(unlimited.power().spent_j() > 0.0);
    assert_eq!(unlimited.power().remaining_j(), None);
}

/// Every frame carries the provenance of its payload length, and none of them claims to be
/// real wire bytes.
#[test]
fn every_frame_says_where_its_length_came_from() {
    let rng = RngRegistry::new(36);
    let mut d = device(VruServices::BOTH);
    let mut seen = 0;
    for k in 0..=40u64 {
        let now = k * 100 * NS_PER_MS;
        d.set_belief(belief(Vec3::new(5.0 * k as f64, 0.0, 0.0), 1.4));
        let mut ctx = NodeRuntimeCtx::new(now, &rng);
        let out = d.step(&mut ctx, Vec::new(), 0.0);
        assert_eq!(out.payload_provenance.len(), out.transmissions.len());
        for ((ty, p), tx) in out.payload_provenance.iter().zip(&out.transmissions) {
            assert_eq!(*ty, tx.msg_type);
            assert!(!p.is_real(), "neither payload is encoder output yet");
            match p {
                PayloadProvenance::SizeModel { model, .. } => assert!(!model.is_empty()),
                PayloadProvenance::Encoded { .. } => panic!("no encoder exists"),
            }
            seen += 1;
        }
    }
    assert!(seen > 0, "the run must have transmitted something");
}

/// A beacon has no receiver, and the frames it is handed are counted rather than silently
/// discarded.
#[test]
fn a_beacon_has_no_receiver_and_says_so() {
    let rng = RngRegistry::new(37);
    let mut beacon = VruDeviceRuntime::beacon(NodeId::new(701), 0);
    beacon.stores_mut().crl.set_period(100);
    beacon
        .stores_mut()
        .certs
        .insert(credential(NodeId::new(701), 0));
    beacon.set_belief(belief(Vec3::ZERO, 1.4));
    assert_eq!(beacon.config().kind, VruDeviceKind::Beacon);
    assert_eq!(beacon.config().services, VruServices::SAE);

    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let out = beacon.step(&mut ctx, vec![rx_frame(0)], 0.0);
    assert!(out.delivered.is_empty(), "a beacon delivers nothing");
    // It still transmits: that is the whole reason it exists.
    assert_eq!(out.transmissions.len(), 1);
    assert_eq!(out.transmissions[0].msg_type, MsgType::Psm);

    // A handset does deliver, which is what makes the assertion above about the kind.
    let rng = RngRegistry::new(38);
    let mut handset = device(VruServices::SAE);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let out = handset.step(&mut ctx, vec![rx_frame(0)], 0.0);
    assert_eq!(out.delivered.len(), 1);
}

fn rx_frame(j: u32) -> RxFrame {
    RxFrame {
        signer: Some(pseudo_signer(NodeId::new(42), j)),
        msg_type: MsgType::Cam,
        bytes: 300,
        claimed_pos: Some(Vec3::new(20.0, 0.0, 0.0)),
        claimed_speed_mps: 10.0,
        claimed_heading_rad: 0.0,
        claimed_generation_time: 0,
        full_certificate: true,
        signature_valid: true,
        claimed_cert_period: 100,
        claimed_linkage: None,
        spdu: None,
    }
}

/// The telemetry record a device closes carries a real air time, which is the one number a
/// vehicle's record cannot: a duty cycle is meaningless without it, so the VRU device
/// computes it.
#[test]
fn the_telemetry_carries_a_real_air_time() {
    let rng = RngRegistry::new(39);
    let mut d = device(VruServices::SAE);
    let mut telemetry = None;
    for k in 0..=12u64 {
        let now = k * 100 * NS_PER_MS;
        let mut ctx = NodeRuntimeCtx::new(now, &rng);
        if let Some(t) = d.step(&mut ctx, Vec::new(), 0.0).telemetry {
            telemetry = Some(t);
        }
    }
    let t = telemetry.expect("a window closed inside 1.2 s");
    assert_eq!(t.node_id, DEVICE.index());
    assert!(t.msgs_out_per_s > 0.0);
    assert!(
        t.airtime_ms_per_s.is_finite() && t.airtime_ms_per_s > 0.0,
        "a device that transmitted must report air time, and a vehicle's runtime reports \
         zero here because the PHY computes it: {}",
        t.airtime_ms_per_s
    );
    // And the transmit power is the pedestrian-UE assumption, in centi-dBm.
    assert_eq!(t.tx_power_cdbm, 2_300);
}

/// A device with no credential stops transmitting rather than sending unsigned, and says
/// which of the two it was.
#[test]
fn a_device_with_no_credential_stops_transmitting() {
    let rng = RngRegistry::new(40);
    let mut d = VruDeviceRuntime::handset(DEVICE, 0);
    d.set_belief(belief(Vec3::ZERO, 1.4));
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let out = d.step(&mut ctx, Vec::new(), 0.0);
    assert!(out.transmissions.is_empty());
    assert_eq!(out.suppressed.len(), 1);
    assert_eq!(out.suppressed[0].1, "no-credential");
}

/// Two runs of the same device produce the same transmissions: the schedule, the gate and
/// the budget are all functions of the believed time and the belief, and of nothing else.
#[test]
fn the_device_is_deterministic() {
    fn run() -> Vec<(MsgType, u32, SimTime)> {
        let rng = RngRegistry::new(41);
        let mut d = device(VruServices::BOTH);
        let mut out = Vec::new();
        for k in 0..=50u64 {
            let now = k * 100 * NS_PER_MS;
            d.set_belief(belief(Vec3::new(2.0 * k as f64, 0.0, 0.0), 1.4));
            let mut ctx = NodeRuntimeCtx::new(now, &rng);
            for tx in d.step(&mut ctx, Vec::new(), 0.0).transmissions {
                out.push((tx.msg_type, tx.bytes, tx.ready_at));
            }
        }
        out
    }
    let a = run();
    assert!(!a.is_empty());
    assert_eq!(a, run());
}
