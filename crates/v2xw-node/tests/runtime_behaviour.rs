//! End-to-end behaviour of [`ObuRuntime`]: the phenomenon the simulator exists to study,
//! and the behavioural half of the ground-truth firewall.
//!
//! # Faults injected to prove these checks can fail
//!
//! * `self.belief.pos = truth_pos;` inside `ObuRuntime::step` —
//!   `ground_truth_changes_nothing_a_node_does` fails, because the transmissions then
//!   differ between two runs whose only difference was the truth.
//! * `ProfileServiceModel::service_time` made to return `Some(Duration::ZERO)` for an
//!   uncosted operation — `a_node_that_cannot_keep_up_falls_behind` fails, because the
//!   HSM never saturates.
//! * The `active()` check in `ObuRuntime::generate` removed —
//!   `a_revoked_node_stops_transmitting` fails.

use v2xw_core::belief::{FixQuality, PositionEstimate};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::nodeview::NodeView;
use v2xw_core::rng::RngRegistry;
use v2xw_core::time::{Duration, NS_PER_MS, NS_PER_S, SimTime};
use v2xw_msg::MsgType;
use v2xw_node::ctx::NodeRuntimeCtx;
use v2xw_node::generate::ServiceSet;
use v2xw_node::policy::{OnDemand, Prioritized, VerifyAll};
use v2xw_node::profile::HardwareProfile;
use v2xw_node::queue::DropCause;
use v2xw_node::runtime::{NodeConfig, ObuRuntime, RxDisposition, RxFrame, RxStamp, StepOutcome};
use v2xw_node::stores::{CredState, CredentialHandle, VerificationState, pseudo_signer};
use v2xw_node::telemetry::NodeState;

/// The encoded §3.5.2 record, for comparisons that must survive the `NaN` fields.
fn encode(t: &v2xw_record::wire::telemetry::NodeTelemetry) -> Vec<u8> {
    v2xw_record::wire::telemetry::TelemetryBody::new(0, 0, vec![*t]).encode()
}

fn profile(id: &str) -> HardwareProfile {
    v2xw_node::profiles::get(id)
        .expect("shipped profile")
        .clone()
}

fn belief(pos: Vec3, speed: f64) -> PositionEstimate {
    let mut p = PositionEstimate::no_fix(0);
    p.pos = pos;
    p.vel = Vec3::new(speed, 0.0, 0.0);
    p.heading_rad = 0.0;
    p.semi_major_m = 5.0;
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

fn node_on(profile_id: &str, services: ServiceSet) -> ObuRuntime {
    let node = NodeId::new(1);
    let mut rt = ObuRuntime::new(
        node,
        profile(profile_id),
        Box::new(VerifyAll::new()),
        NodeConfig {
            services,
            ..NodeConfig::default()
        },
        0,
    );
    rt.stores_mut().crl.set_period(100);
    for j in 0..4 {
        rt.stores_mut().certs.insert(credential(node, j));
    }
    rt.set_belief(belief(Vec3::ZERO, 10.0));
    rt
}

fn frame(from: u32, pos: Vec3, at: SimTime, valid: bool) -> RxFrame {
    RxFrame {
        signer: Some(pseudo_signer(NodeId::new(from), 0)),
        msg_type: MsgType::Cam,
        bytes: 300,
        claimed_pos: Some(pos),
        claimed_speed_mps: 12.0,
        claimed_heading_rad: 0.0,
        claimed_generation_time: at,
        full_certificate: false,
        signature_valid: valid,
        claimed_cert_period: 100,
        claimed_linkage: None,
        spdu: None,
    }
}

/// Runs ticks `first..first + steps` of `dt`, feeding `inbox(k)` at tick `k`.
///
/// `first` exists because simulated time only goes forwards: a second `run` over the same
/// runtime has to continue the timeline, not restart it. A test that restarted it would
/// pass for the wrong reason — the node would sit with every timer in its own future and
/// do nothing at all, which looks exactly like the behaviour under test.
fn run_from(
    rt: &mut ObuRuntime,
    first: u64,
    steps: u64,
    dt: SimTime,
    mut inbox: impl FnMut(u64) -> Vec<RxFrame>,
) -> Vec<StepOutcome> {
    let reg = RngRegistry::new(7);
    let mut out = Vec::new();
    for k in first..first + steps {
        let now = k * dt;
        let mut ctx = NodeRuntimeCtx::new(now, &reg);
        out.push(rt.step(&mut ctx, inbox(k), 0.0));
    }
    out
}

/// Runs `steps` ticks of `dt` from the start of the run.
fn run(
    rt: &mut ObuRuntime,
    steps: u64,
    dt: SimTime,
    inbox: impl FnMut(u64) -> Vec<RxFrame>,
) -> Vec<StepOutcome> {
    run_from(rt, 0, steps, dt, inbox)
}

// -------------------------------------------------------------------------------------
// The step function
// -------------------------------------------------------------------------------------

/// The whole loop in one test: a node generates on its own schedule, signs, receives,
/// verifies, updates its neighbour table and reports telemetry.
#[test]
fn a_node_generates_receives_verifies_and_reports() {
    let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    let outcomes = run(&mut rt, 101, 10 * NS_PER_MS, |k| {
        if k % 10 == 0 {
            vec![frame(
                9,
                Vec3::new(50.0, 0.0, 0.0),
                k * 10 * NS_PER_MS,
                true,
            )]
        } else {
            Vec::new()
        }
    });

    let transmissions: usize = outcomes.iter().map(|o| o.transmissions.len()).sum();
    assert_eq!(transmissions, 11, "10 Hz for one second, plus the first");
    let delivered: usize = outcomes.iter().map(|o| o.delivered.len()).sum();
    assert_eq!(delivered, 11);

    // The peer is in the neighbour table, verified, with the position it claimed.
    let peer = rt
        .neighbors()
        .get(&pseudo_signer(NodeId::new(9), 0))
        .expect("the peer was heard");
    assert_eq!(peer.state, VerificationState::Verified);
    assert_eq!(peer.claimed_pos, Vec3::new(50.0, 0.0, 0.0));
    assert_eq!(peer.messages, 11);

    // One telemetry frame closed at the one-second boundary.
    let telemetry: Vec<_> = outcomes
        .iter()
        .filter_map(|o| o.telemetry.as_ref())
        .collect();
    assert_eq!(telemetry.len(), 1);
    let t = telemetry[0];
    assert_eq!(t.node_id, 1);
    assert_eq!(t.nbr_total, 1);
    assert_eq!(t.nbr_verified, 1);
    assert_eq!(t.msgs_out_per_s, 11.0);
    assert_eq!(t.msgs_in_per_s, 11.0);
    assert_eq!(t.verifications_per_s, 11.0);
    assert_eq!(t.unverified_ratio_pm, 0);
    assert_eq!(t.node_state, NodeState::Active.code());
    assert_eq!(t.verify_policy, 0);
}

/// A bad signature is never delivered as a neighbour: the node learns the message was
/// invalid and the table stays empty.
#[test]
fn an_invalid_signature_never_reaches_the_neighbour_table() {
    let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    let outcomes = run(&mut rt, 3, 100 * NS_PER_MS, |k| {
        vec![frame(
            9,
            Vec3::new(50.0, 0.0, 0.0),
            k * 100 * NS_PER_MS,
            false,
        )]
    });
    let delivered: Vec<_> = outcomes.iter().flat_map(|o| o.delivered.iter()).collect();
    assert_eq!(delivered.len(), 3);
    assert!(
        delivered
            .iter()
            .all(|m| m.verification == VerificationState::Invalid)
    );
    assert_eq!(rt.neighbors().len(), 0);
}

// -------------------------------------------------------------------------------------
// The phenomenon: a node that cannot keep up
// -------------------------------------------------------------------------------------

/// **The phenomenon the simulator exists to study.** One profile, two loads: below its
/// published verification rate the node keeps up, and above it the queue fills, the waits
/// grow and messages are dropped.
///
/// The rate is the profile's, not this test's. 06-node-models.md §7.2 publishes
/// ">2500 ECDSA NIST P-256 verifications/s ('line-rate')" for the CRATON2's on-chip
/// engine, and the two loads straddle it: 20 neighbours at 10 Hz is 200 verifications a
/// second, and 300 neighbours at 10 Hz is 3,000 — which is also the regime NDSS 2024
/// §VII-B is about, since it puts the requirement for 100 neighbours at >= 1 kHz.
#[test]
fn a_node_that_cannot_keep_up_falls_behind() {
    let load = |neighbours: u32| {
        move |k: u64| -> Vec<RxFrame> {
            (0..neighbours)
                .map(|n| {
                    frame(
                        n + 100,
                        Vec3::new(f64::from(n), 0.0, 0.0),
                        k * 100 * NS_PER_MS,
                        true,
                    )
                })
                .collect()
        }
    };
    let window = |neighbours: u32| {
        let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
        let outcomes = run(&mut rt, 11, 100 * NS_PER_MS, load(neighbours));
        outcomes
            .iter()
            .filter_map(|o| o.telemetry.as_ref())
            .next()
            .copied()
            .expect("a window closed")
    };

    // 200/s against a 2,500/s engine: 400 us each, 80 ms of work per second.
    let light = window(20);
    // 220 verifies at 400 us is 88 ms of engine time; 11 signatures at 9 ms is 99 ms of
    // eHSM time. §3.5.2 has one security-hardware field, so the binding engine is the one
    // reported — here the signing path, not the verification path.
    assert_eq!(light.hsm_util_pm, 99);
    // The harness delivers a whole tick's arrivals at one instant, so even the light load
    // queues for the length of the burst: twenty verifications at 400 us is 7.6 ms for the
    // last of them. That is a property of the arrival pattern, not of overload — nothing
    // is dropped and the backlog clears inside the tick.
    assert!(
        light.verify_wait_p95_ms < 10.0,
        "the burst should clear inside one tick, p95 was {} ms",
        light.verify_wait_p95_ms
    );
    assert_eq!(light.drop_verify_overflow, 0);
    assert_eq!(light.unverified_ratio_pm, 0, "everything is verified");

    // 3,000/s against the same engine. What happens is the thing the simulator exists to
    // show, and it is not "the engine saturates": the verify queue fills first, sheds the
    // excess, and the engine ends up working on the fraction that got in. A node under
    // this load is not slow — it is *deaf*, to 79 % of what it heard, and the telemetry
    // says so in `drop_verify_overflow` and `unverified_ratio_pm` rather than in a
    // utilisation figure that looks merely busy.
    let heavy = window(300);
    assert!(
        heavy.hsm_util_pm > light.hsm_util_pm,
        "the engine should be busier: {} vs {} per mille",
        heavy.hsm_util_pm,
        light.hsm_util_pm
    );
    assert!(
        heavy.q_verify_p95 >= 60,
        "the verify queue should sit near its configured depth of 64, p95 was {}",
        heavy.q_verify_p95
    );
    // The tail wait triples, bounded by the queue depth: 64 entries at 400 us is 25.6 ms,
    // which is what the queue depth buys and what it costs. A deeper queue would trade
    // this latency for fewer drops, and that trade is the design question the telemetry
    // exists to inform.
    assert!(
        heavy.verify_wait_p95_ms > 3.0 * light.verify_wait_p95_ms,
        "the tail wait should grow with the backlog: {} ms vs {} ms",
        heavy.verify_wait_p95_ms,
        light.verify_wait_p95_ms
    );
    assert!(
        heavy.verify_wait_p95_ms < 26.0,
        "and it is bounded by the queue depth, 64 x 400 us = 25.6 ms; was {} ms",
        heavy.verify_wait_p95_ms
    );
    // Which counter moves is itself the finding, and it is the verification queue's. The
    // receive queue has no server in front of it — parsing costs nothing in any shipped
    // profile (`app_task_us` is uncalibrated) — so no frame ever waits in it, and the
    // backlog forms where the work is: in front of the verification engine, which sheds
    // the excess at the instant it is full. (This used to assert the opposite: a tick's
    // arrivals were pushed into the receive queue in one burst before any was processed,
    // so it "overflowed" whatever the parse cost. `the_receive_queue_overflows_only_when_
    // parsing_costs_time` is where the receive queue earns its drops.) Six counters exist
    // precisely so that which queue overflowed is visible rather than collapsed into one
    // "dropped" number.
    assert!(
        heavy.drop_verify_overflow > 0,
        "an overloaded node must drop somewhere"
    );
    assert_eq!(
        heavy.drop_rx_overflow, 0,
        "a queue with no server in front of it cannot back up"
    );
    assert!(heavy.q_verify_p95 > light.q_verify_p95);
    // Most of what this node heard never reached an application.
    let heard = 300 * 11;
    let shed = heavy.drop_rx_overflow + heavy.drop_verify_overflow + heavy.drop_verify_policy_skip;
    assert!(
        shed > heard * 3 / 4,
        "expected most of {heard} arrivals to be shed, {shed} were"
    );
}

/// Where the work is charged is the profile's business, not the runtime's: the reference
/// OBU verifies on a dedicated engine outside its HSM boundary, and the software-only
/// profile charges the same work to its CPU cores.
#[test]
fn the_profile_decides_which_server_is_charged() {
    let arrivals = |k: u64| -> Vec<RxFrame> {
        (0..40)
            .map(|n| {
                frame(
                    n + 100,
                    Vec3::new(f64::from(n), 0.0, 0.0),
                    k * 100 * NS_PER_MS,
                    true,
                )
            })
            .collect()
    };
    let window = |profile_id: &str| {
        let mut rt = node_on(profile_id, ServiceSet::SAE);
        let outcomes = run(&mut rt, 11, 100 * NS_PER_MS, arrivals);
        outcomes
            .iter()
            .filter_map(|o| o.telemetry.as_ref())
            .next()
            .copied()
            .expect("a window closed")
    };

    let hardware = window(v2xw_node::profiles::REFERENCE_OBU);
    assert!(
        hardware.hsm_util_pm > 0,
        "the security hardware did the work"
    );
    assert_eq!(hardware.cpu_util_pm, 0, "and the CPU did none of it");

    let software = window("obu/generic-automotive-soc-no-hsm");
    assert!(software.cpu_util_pm > 0, "rule H2: the CPU does it all");
    assert_eq!(software.hsm_util_pm, 0, "there is no security hardware");
}

/// A profile that costs no verification verifies nothing, rather than verifying
/// everything instantly. The queue fills and the telemetry says so — which is what makes
/// an unconfigured run visibly unconfigured instead of silently optimistic.
#[test]
fn an_uncosted_profile_verifies_nothing_rather_than_everything() {
    // obu/cohda-mk5 publishes a verify rate, so use the RSU profile whose every crypto
    // figure is not-published.
    let mut rt = node_on("rsu/cohda-mk5-rsu", ServiceSet::SAE);
    let outcomes = run(&mut rt, 11, 100 * NS_PER_MS, |k| {
        vec![frame(
            9,
            Vec3::new(50.0, 0.0, 0.0),
            k * 100 * NS_PER_MS,
            true,
        )]
    });
    let delivered: usize = outcomes.iter().map(|o| o.delivered.len()).sum();
    assert_eq!(
        delivered, 0,
        "nothing can be verified, so nothing is delivered"
    );
    let t = outcomes
        .iter()
        .filter_map(|o| o.telemetry.as_ref())
        .next()
        .unwrap();
    assert_eq!(t.verifications_per_s, 0.0);
    assert!(t.msgs_in_per_s > 0.0, "the frames did arrive");
}

// -------------------------------------------------------------------------------------
// Credentials
// -------------------------------------------------------------------------------------

/// A node whose own certificate is on the CRL stops transmitting
/// [CAMP-EE §2.2.10.2].
#[test]
fn a_revoked_node_stops_transmitting() {
    let node = NodeId::new(1);
    let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    let before = run(&mut rt, 6, 100 * NS_PER_MS, |_| Vec::new());
    assert!(
        before
            .iter()
            .filter(|o| !o.transmissions.is_empty())
            .count()
            >= 2,
        "the node was transmitting before it was revoked"
    );

    // Revoked by the digest the store actually holds, which after the first transmission
    // is the digest of the real certificate the node provisioned for that pseudonym — not
    // the `pseudo_signer` stand-in it started with. Revoking the stand-in would revoke a
    // certificate that was never on the air, and the node would carry on transmitting:
    // that is the second fault injected to prove this test can fail.
    let mine: Vec<_> = rt
        .stores()
        .certs
        .credentials()
        .iter()
        .map(|c| c.digest.clone())
        .collect();
    assert_eq!(mine.len(), 4);
    for d in &mine {
        assert_ne!(
            *d,
            pseudo_signer(node, 0),
            "the node must be using a certificate it can prove, not a stand-in digest"
        );
        rt.stores_mut().crl.revoke_own(d);
    }
    let after = run_from(&mut rt, 6, 6, 100 * NS_PER_MS, |_| Vec::new());
    assert!(
        after.iter().all(|o| o.transmissions.is_empty()),
        "a revoked node must not transmit"
    );
}

/// The verification policy is swappable and the telemetry says which one is in force,
/// so a run's `unverified_ratio` can be attributed.
#[test]
fn the_policy_is_swappable_and_reported() {
    for (policy, code, expect_unverified) in [
        (
            Box::new(VerifyAll::new()) as Box<dyn v2xw_node::policy::VerificationPolicy>,
            0u8,
            false,
        ),
        (Box::new(OnDemand::new(0.5)), 1, true),
        (Box::new(Prioritized::new(300.0)), 2, false),
    ] {
        let node = NodeId::new(1);
        let mut rt = ObuRuntime::new(
            node,
            profile(v2xw_node::profiles::REFERENCE_OBU),
            policy,
            NodeConfig {
                services: ServiceSet::SAE,
                ..NodeConfig::default()
            },
            0,
        );
        rt.stores_mut().crl.set_period(100);
        rt.stores_mut().certs.insert(credential(node, 0));
        rt.set_belief(belief(Vec3::ZERO, 10.0));

        // The first message makes the peer known; the rest are routine updates, which is
        // what on-demand skips.
        let outcomes = run(&mut rt, 11, 100 * NS_PER_MS, |k| {
            vec![frame(
                9,
                Vec3::new(50.0, 0.0, 0.0),
                k * 100 * NS_PER_MS,
                true,
            )]
        });
        let t = outcomes
            .iter()
            .filter_map(|o| o.telemetry.as_ref())
            .next()
            .unwrap();
        assert_eq!(t.verify_policy, code);
        if expect_unverified {
            assert!(
                t.unverified_ratio_pm > 0,
                "on-demand should deliver routine updates unverified"
            );
            assert!(t.drop_verify_policy_skip > 0);
        } else {
            assert_eq!(t.unverified_ratio_pm, 0);
            assert_eq!(t.drop_verify_policy_skip, 0);
        }
    }
}

// -------------------------------------------------------------------------------------
// The behavioural half of the firewall
// -------------------------------------------------------------------------------------

/// **The firewall, behaviourally.** The engine hands the node a ground-truth position
/// error for the one §3.5.2 field that needs it. Changing *only* that number must change
/// *only* that field: the same transmissions, the same deliveries, the same neighbour
/// table, the same fifty-five other telemetry values.
///
/// `tests/firewall_sentinel.rs` checks the same property structurally, by reading the
/// source. This one checks it by running the node, which catches a leak the text scan
/// would miss — a ground-truth value copied into another field and used from there.
#[test]
fn ground_truth_changes_nothing_a_node_does() {
    let scenario = |truth_error: f32| {
        let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::BOTH);
        rt.observe_truth(truth_error);
        let outcomes = run(&mut rt, 21, 50 * NS_PER_MS, |k| {
            if k % 2 == 0 {
                vec![frame(
                    9,
                    Vec3::new(50.0, 0.0, 0.0),
                    k * 50 * NS_PER_MS,
                    true,
                )]
            } else {
                Vec::new()
            }
        });
        let telemetry = outcomes
            .iter()
            .filter_map(|o| o.telemetry)
            .next()
            .expect("a window closed");
        let tx: Vec<_> = outcomes
            .iter()
            .flat_map(|o| o.transmissions.clone())
            .collect();
        let rx: Vec<_> = outcomes.iter().flat_map(|o| o.delivered.clone()).collect();
        (tx, rx, telemetry, rt.neighbors().len())
    };

    let (tx_a, rx_a, t_a, n_a) = scenario(0.0);
    let (tx_b, rx_b, t_b, n_b) = scenario(97.5);

    assert_eq!(tx_a, tx_b, "the truth changed what the node transmitted");
    assert_eq!(rx_a, rx_b, "the truth changed what the node delivered");
    assert_eq!(n_a, n_b, "the truth changed the neighbour table");
    assert!(
        !tx_a.is_empty() && !rx_a.is_empty(),
        "the scenario did something"
    );

    // Exactly one telemetry field differs, and it is the one §3.5.2 marks GT. The
    // comparison is over the encoded bytes rather than the struct, because the record
    // carries `NaN` in the fields this runtime does not model and `NaN != NaN` would make
    // a struct comparison fail for two identical records.
    assert_eq!(t_a.pos_error_m, 0.0);
    assert_eq!(t_b.pos_error_m, 97.5);
    let mut normalised = t_b;
    normalised.pos_error_m = t_a.pos_error_m;
    assert_eq!(
        encode(&t_a),
        encode(&normalised),
        "a ground-truth value reached a field other than pos_error_m"
    );
    assert_ne!(
        encode(&t_a),
        encode(&t_b),
        "the GT field itself must still differ, or the check above is vacuous"
    );
}

/// The node's belief is what it transmits and what it reasons from: a node whose GNSS
/// belief is wrong claims the wrong position, and a policy that orders by proximity
/// orders by the *believed* distance.
#[test]
fn the_node_reasons_from_its_belief() {
    let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    rt.set_belief(belief(Vec3::new(1000.0, 0.0, 0.0), 10.0));
    assert_eq!(rt.position().pos, Vec3::new(1000.0, 0.0, 0.0));

    // Its own believed time drives generation, and its own believed position drives the
    // claim. A NodeView is all a plug-in gets, and this is the whole of it.
    let view: &dyn NodeView<
        Neighbors = v2xw_node::stores::NeighborTable,
        Credential = CredentialHandle,
        Message = v2xw_node::runtime::VerifiedMessage,
    > = &rt;
    assert_eq!(view.node(), NodeId::new(1));
    assert_eq!(view.position().pos, Vec3::new(1000.0, 0.0, 0.0));
    assert_eq!(view.credentials().len(), 4);
    assert!(view.active_credential().is_some());
    assert_eq!(view.received().len(), 0);
}

/// A node whose clock has been stepped stamps its messages with the stepped time, which
/// is the symptom a clock attack produces at every receiver's freshness check — and which
/// a simulator that stamped the simulator's own clock would show as nothing at all.
///
/// Note what does *not* change: the cadence. Stepping a clock moves the instant, not the
/// rate, so the node still transmits every 100 ms of its own time. A drifting clock would
/// change the rate; a stepped one changes what the message claims.
#[test]
fn a_stepped_clock_is_visible_in_what_the_node_claims() {
    let mut honest = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    let a = run(&mut honest, 11, 100 * NS_PER_MS, |_| Vec::new());
    let honest_tx: Vec<_> = a.iter().flat_map(|o| o.transmissions.clone()).collect();

    let mut attacked = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    attacked.clock_mut().step_offset(NS_PER_S as i64);
    let b = run(&mut attacked, 11, 100 * NS_PER_MS, |_| Vec::new());
    let attacked_tx: Vec<_> = b.iter().flat_map(|o| o.transmissions.clone()).collect();

    // The rate is unchanged: a step moves the clock, it does not speed it up.
    assert_eq!(honest_tx.len(), attacked_tx.len());
    assert_eq!(honest_tx.len(), 11);

    // What changed is every generation time, by exactly the step.
    assert_eq!(honest_tx[0].generation_time, 0);
    assert_eq!(attacked_tx[0].generation_time, NS_PER_S);
    for (h, x) in honest_tx.iter().zip(attacked_tx.iter()) {
        assert_eq!(x.generation_time - h.generation_time, NS_PER_S);
    }

    // And the offset reaches the one ground-truth integer field of §3.5.2, which is how
    // an analyst sees a discrepancy the node itself cannot.
    let t = b
        .iter()
        .filter_map(|o| o.telemetry.as_ref())
        .next()
        .unwrap();
    assert_eq!(t.clock_offset_ns, NS_PER_S as i64);
    assert_eq!(attacked.clock().offset_ns(), NS_PER_S as i64);
}

/// A node that is off does nothing at all — and says, of every frame it was handed, that
/// it was off, so a reception attempt at a dead radio is accounted for rather than lost
/// between the PHY and the application.
#[test]
fn an_off_node_does_nothing() {
    let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::BOTH);
    rt.set_state(NodeState::Off);
    let outcomes = run(&mut rt, 20, 100 * NS_PER_MS, |k| {
        vec![frame(9, Vec3::ZERO, k * 100 * NS_PER_MS, true)]
    });
    for o in &outcomes {
        assert!(o.transmissions.is_empty());
        assert!(o.delivered.is_empty());
        assert!(o.telemetry.is_none());
        assert_eq!(o.rx_reports.len(), 1);
        assert_eq!(o.rx_reports[0].disposition, RxDisposition::NodeOff);
    }
    assert_eq!(rt.neighbors().len(), 0);
}

/// The receive queue backs up only when parsing takes time: 300 frames arriving in one
/// instant at a node that spends 1 ms parsing each fill its 64 slots, while the same 300
/// spread over a tenth of a second at 0.2 ms each never wait at all.
#[test]
fn the_receive_queue_overflows_only_when_parsing_costs_time() {
    let run_with = |parse: Duration, spread: bool| {
        let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
        rt.set_app_task_cost(parse);
        let reg = RngRegistry::new(7);
        let now = 100 * NS_PER_MS;
        let inbox: Vec<(RxFrame, RxStamp)> = (0..300u64)
            .map(|n| {
                let at = if spread { n * 300_000 } else { 50 * NS_PER_MS };
                (
                    frame(n as u32 + 100, Vec3::new(n as f64, 0.0, 0.0), at, true),
                    RxStamp {
                        token: n,
                        arrived_at: Some(at),
                    },
                )
            })
            .collect();
        let mut ctx = NodeRuntimeCtx::new(now, &reg);
        let out = rt.step_timed(&mut ctx, inbox, 0.0);
        out.rx_reports
            .iter()
            .filter(|r| r.disposition == RxDisposition::Dropped(DropCause::RxOverflow))
            .count()
    };
    assert!(run_with(Duration::from_millis(1), false) > 200);
    assert_eq!(run_with(Duration::from_micros(200), true), 0);
}

/// Every frame handed to a node comes back as exactly one report, with its instants in
/// order: arrived ≤ parsed ≤ verification start ≤ verification done.
#[test]
fn every_received_frame_is_reported_once_with_ordered_instants() {
    let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    let reg = RngRegistry::new(7);
    let mut reports = Vec::new();
    let mut handed = 0u64;
    for k in 1..=10u64 {
        let now = k * 100 * NS_PER_MS;
        let inbox: Vec<(RxFrame, RxStamp)> = (0..150u64)
            .map(|n| {
                // In pairs: two frames end at the same instant every 1.2 ms, so the second
                // of each pair waits for the first's check.
                let at = now - 100 * NS_PER_MS + (n / 2) * 1_200_000;
                handed += 1;
                (
                    frame(n as u32 + 100, Vec3::new(n as f64, 0.0, 0.0), at, true),
                    RxStamp {
                        token: k * 1_000 + n,
                        arrived_at: Some(at),
                    },
                )
            })
            .collect();
        let mut ctx = NodeRuntimeCtx::new(now, &reg);
        reports.extend(rt.step_timed(&mut ctx, inbox, 0.0).rx_reports);
    }
    let mut tokens: Vec<u64> = reports.iter().map(|r| r.token).collect();
    tokens.sort_unstable();
    let before = tokens.len();
    tokens.dedup();
    assert_eq!(before, tokens.len(), "a frame reported twice");
    // What is not reported is still waiting in the verification queue, never lost.
    assert!(reports.len() as u64 <= handed);
    assert!(
        handed - reports.len() as u64 <= 64,
        "more missing than the queue holds"
    );
    for r in &reports {
        assert!(r.arrived <= r.parsed, "{r:?}");
        if let (Some(s), Some(d)) = (r.verify_start, r.verify_done) {
            assert!(r.parsed <= s && s < d, "{r:?}");
        }
    }
    // 1,500 frames a second against a 2,500/s engine: the queue waits are real but short.
    let waits: Vec<u64> = reports
        .iter()
        .filter_map(|r| r.verify_start.map(|s| s - r.parsed))
        .collect();
    assert!(!waits.is_empty());
    assert!(waits.iter().any(|w| *w > 0), "no check ever waited");
}

/// Two identical runs produce identical output, which is the determinism contract for
/// this crate's share of a run (ADR 0004): nothing here depends on iteration order, on a
/// hash seed or on wall-clock time. The telemetry is compared as encoded bytes, because
/// the record carries `NaN` for what this runtime does not model.
#[test]
fn two_identical_runs_agree() {
    let go = || {
        let mut rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::BOTH);
        let outcomes = run(&mut rt, 31, 50 * NS_PER_MS, |k| {
            (0..7)
                .map(|n| {
                    frame(
                        n + 20,
                        Vec3::new(f64::from(n) * 11.0, 0.0, 0.0),
                        k * 50 * NS_PER_MS,
                        n % 3 != 0,
                    )
                })
                .collect()
        });
        let tx: Vec<_> = outcomes
            .iter()
            .flat_map(|o| o.transmissions.clone())
            .collect();
        let rx: Vec<_> = outcomes.iter().flat_map(|o| o.delivered.clone()).collect();
        let tel: Vec<Vec<u8>> = outcomes
            .iter()
            .filter_map(|o| o.telemetry.as_ref())
            .map(encode)
            .collect();
        (tx, rx, tel)
    };
    let a = go();
    assert!(!a.0.is_empty() && !a.1.is_empty() && !a.2.is_empty());
    assert_eq!(a, go());
}

/// The check interval the engine must schedule is the standards' own floor, and it is not
/// zero — a schedule that answered zero would busy-loop the engine.
#[test]
fn the_check_interval_is_the_standards_floor() {
    let rt = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::BOTH);
    assert_eq!(rt.schedule().check_interval(), Duration::from_millis(100));
}

/// Peer-to-peer certificate distribution: a digest signer identifier this node has never
/// seen costs a P2PCD request, and a message that attaches the full certificate pays
/// bytes instead and stops the requests.
///
/// The counters are the two §3.5.2 fields at offsets 96 and 100, and the trade between
/// them is what the certificate-attachment cadence of 05-protocols.md §2.4 is choosing.
#[test]
fn an_unknown_signer_costs_a_p2pcd_request_and_a_full_certificate_stops_it() {
    let mut digests_only = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    let a = run(&mut digests_only, 11, 100 * NS_PER_MS, |k| {
        vec![frame(9, Vec3::ZERO, k * 100 * NS_PER_MS, true)]
    });
    let ta = a
        .iter()
        .filter_map(|o| o.telemetry.as_ref())
        .next()
        .unwrap();
    assert_eq!(ta.p2pcd_requests, 11, "every message missed the cache");
    assert_eq!(ta.peer_cache_entries, 0, "and nothing was learned");

    let mut attaches = node_on(v2xw_node::profiles::REFERENCE_OBU, ServiceSet::SAE);
    let b = run(&mut attaches, 11, 100 * NS_PER_MS, |k| {
        let mut f = frame(9, Vec3::ZERO, k * 100 * NS_PER_MS, true);
        // The first message carries the certificate, as the attachment cadence requires
        // periodically; the rest name it by digest.
        f.full_certificate = k == 0;
        vec![f]
    });
    let tb = b
        .iter()
        .filter_map(|o| o.telemetry.as_ref())
        .next()
        .unwrap();
    assert_eq!(
        tb.p2pcd_requests, 0,
        "one attachment served the whole second"
    );
    assert_eq!(tb.peer_cache_entries, 1);
}
