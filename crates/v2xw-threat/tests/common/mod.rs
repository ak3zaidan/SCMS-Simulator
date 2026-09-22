//! A one-road test world: an honest or attacking vehicle driving east along `y = 0`, and
//! one static receiver at the origin running the detector suite.
//!
//! Everything the receiver is given is belief: the claims on the messages it "hears" and
//! its own position. The harness never hands the detector the vehicle's true state, which
//! is why `tests/firewall.rs` can assert that two runs whose ground truth differs but
//! whose claims agree produce identical detections.

#![allow(dead_code)]

pub mod sim;

use std::collections::BTreeSet;

use v2xw_core::ids::NodeId;
use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};
use v2xw_core::time::{NS_PER_S, SimTime, secs_to_ns};
use v2xw_threat::attack::{AttackKind, Attacker, AttackerView, Emission, HonestClaim};
use v2xw_threat::attack_legacy::{LegacyAttacker, LegacyAttackerParams};
use v2xw_threat::capability::{AttackSchedule, Capabilities};
use v2xw_threat::ctx::CollectingCtx;
use v2xw_threat::detect::{Detector, DetectorId, DetectorParams, Legacy12, Verdict};
use v2xw_threat::obs::{
    LocalEnvironment, ObservedKind, ObservedMessage, SelfBelief, StationType, VerificationState,
};

/// The world's only road: the east-west line `y = 0`.
pub struct OneRoad;

impl LocalEnvironment for OneRoad {
    fn distance_to_road_m(&self, _x_m: f64, y_m: f64) -> f64 {
        y_m.abs()
    }
}

/// The receiver's node id.
pub const RX: NodeId = NodeId::new(1);
/// The transmitting vehicle's node id.
pub const TX: NodeId = NodeId::new(2);
/// The transmitting vehicle's signer digest.
pub const TX_SIGNER: [u8; 8] = [0xAA; 8];
/// The vehicle's constant speed.
pub const SPEED_MPS: f64 = 15.0;
/// The receiver's configured range.
///
/// Deliberately wider than any run's track, so `acceptanceRangeThreshold` is exercised by
/// its own crafted case ([`rx_at_origin`], a 500 m receiver) and never by the honest
/// vehicle simply driving out of range — which would otherwise show up as a benign false
/// positive that is an artefact of the harness rather than of the detector.
pub const RX_RANGE_M: f64 = 10_000.0;

/// What one run produced.
pub struct RunOut {
    /// Every check that fired at least once.
    pub fired: BTreeSet<DetectorId>,
    /// Every emission the transmitter produced, in order.
    pub emissions: Vec<Emission>,
    /// The honest claim behind each emission, in the same order.
    pub honest: Vec<HonestClaim>,
    /// Every verdict the receiver reached, in order.
    pub verdicts: Vec<Verdict>,
}

impl RunOut {
    /// Whether `d` fired at least once.
    pub fn fired(&self, d: DetectorId) -> bool {
        self.fired.contains(&d)
    }

    /// The highest score `d` ever reached.
    pub fn peak(&self, d: DetectorId) -> f64 {
        self.verdicts
            .iter()
            .map(|v| v.fingerprint.get(d))
            .fold(0.0, f64::max)
    }
}

/// How a run is configured.
pub struct Scenario {
    /// The attack the vehicle runs, or `None` for an honest vehicle.
    pub attack: Option<AttackKind>,
    /// How many messages to send.
    pub steps: u64,
    /// The generation interval, seconds.
    pub dt_s: f64,
    /// Per-axis GNSS noise, metres. Zero for a noise-free run.
    pub gnss_sigma_m: f64,
    /// The master seed.
    pub seed: u64,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            attack: None,
            steps: 60,
            dt_s: 1.0,
            gnss_sigma_m: 0.0,
            seed: 20_260_922,
        }
    }
}

impl Scenario {
    /// A sixty-second run of one attack at the legacy one-hertz interval.
    pub fn attacking(kind: AttackKind) -> Self {
        Self {
            attack: Some(kind),
            ..Self::default()
        }
    }
}

/// Runs the scenario and returns what the receiver saw.
pub fn run(s: &Scenario) -> RunOut {
    let mut ctx = CollectingCtx::new(s.seed);
    let mut params = DetectorParams {
        generation_interval_s: s.dt_s,
        station_types_in_play: true,
        event_messages_in_play: true,
        ..DetectorParams::default()
    };
    // The suite is otherwise at the legacy defaults; only the two gates and the
    // generation interval move, and each is a declared parameter.
    params.min_consecutive = DetectorParams::default().min_consecutive;
    let mut det = Legacy12::new(params);

    let mut attacker = s.attack.map(|kind| {
        let mut p = LegacyAttackerParams::new(kind);
        p.dt_s = s.dt_s;
        LegacyAttacker::new(
            TX,
            p,
            Capabilities::insider(20),
            AttackSchedule {
                from: 5 * NS_PER_S,
                to: u64::MAX,
                ..AttackSchedule::default()
            },
            ghost_signers(6),
        )
    });

    let noise = RngRegistry::new(s.seed ^ 0x5eed);
    let env = OneRoad;
    let mut out = RunOut {
        fired: BTreeSet::new(),
        emissions: Vec::new(),
        honest: Vec::new(),
        verdicts: Vec::new(),
    };

    for i in 0..s.steps {
        let t: SimTime = secs_to_ns(i as f64 * s.dt_s);
        ctx.set_now(t);
        let true_x = SPEED_MPS * (i as f64) * s.dt_s;
        let (nx, ny) = if s.gnss_sigma_m > 0.0 {
            let mut r = noise.checkout(RngDomain::Gnss, EntityRef::Node(TX));
            (r.normal(0.0, s.gnss_sigma_m), r.normal(0.0, s.gnss_sigma_m))
        } else {
            (0.0, 0.0)
        };
        let honest = HonestClaim {
            x_m: true_x + nx,
            y_m: ny,
            speed_mps: SPEED_MPS,
            heading_rad: 0.0,
        };
        // The broadcast position confidence: the legacy 2.448 σ circle.
        let conf = 2.448 * s.gnss_sigma_m.max(0.8);

        let me = SelfBelief {
            node: TX,
            believed_time: t,
            x_m: honest.x_m,
            y_m: honest.y_m,
            radio_range_m: RX_RANGE_M,
        };
        let mut em = Emission::honest(TX_SIGNER, honest, t, 0, u64::MAX);
        if let Some(a) = attacker.as_mut() {
            let view = AttackerView {
                own_rx: &[],
                own_credentials: &[],
                crl_revocations_seen: None,
                own_belief: me,
                honest,
                believed_time: t,
            };
            a.observe(&mut ctx, &view);
            a.act(&mut ctx, &view, &mut em);
        }
        out.honest.push(honest);

        if !em.suppressed {
            for m in messages_from(&em, t, conf) {
                let rx_belief = SelfBelief {
                    node: RX,
                    believed_time: t,
                    x_m: 0.0,
                    y_m: 0.0,
                    radio_range_m: RX_RANGE_M,
                };
                let v = det.on_message(&mut ctx, &rx_belief, &m, &env);
                for o in &v.fired {
                    out.fired.insert(o.detector);
                }
                out.verdicts.push(v);
            }
        }
        out.emissions.push(em);
    }
    out
}

/// Every message an emission puts on the air: the beacon, its ghosts and its event
/// messages.
pub fn messages_from(em: &Emission, t: SimTime, conf: f64) -> Vec<ObservedMessage> {
    let mut v = vec![beacon(em, t, conf)];
    for g in &em.ghosts {
        v.push(beacon(g, t, conf));
    }
    for e in &em.events {
        v.push(ObservedMessage {
            signer: em.signer,
            kind: ObservedKind::Denm(e.event_type.clone()),
            received_at: t,
            claimed_generation_time: em.generation_time,
            claimed_x_m: e.x_m,
            claimed_y_m: e.y_m,
            claimed_speed_mps: e.claimed_speed_mps,
            claimed_heading_rad: em.heading_rad,
            claimed_pos_confidence_m: conf,
            repetitions: 1,
            cert_valid_from: em.cert_valid_from,
            cert_valid_to: em.cert_valid_to,
            station_type: em.station_type,
            verification: VerificationState::Valid,
        });
    }
    v
}

fn beacon(em: &Emission, t: SimTime, conf: f64) -> ObservedMessage {
    ObservedMessage {
        signer: em.signer,
        kind: ObservedKind::Beacon,
        received_at: t,
        claimed_generation_time: em.generation_time,
        claimed_x_m: em.x_m,
        claimed_y_m: em.y_m,
        claimed_speed_mps: em.speed_mps,
        claimed_heading_rad: em.heading_rad,
        claimed_pos_confidence_m: conf,
        repetitions: em.repetitions,
        cert_valid_from: em.cert_valid_from,
        cert_valid_to: em.cert_valid_to,
        station_type: em.station_type,
        verification: if em.signature_valid {
            VerificationState::Valid
        } else {
            VerificationState::BadSignature
        },
    }
}

/// `n` distinct ghost credential digests.
pub fn ghost_signers(n: u8) -> Vec<[u8; 8]> {
    (0..n).map(|i| [0xB0 | i, 1, 2, 3, 4, 5, 6, 7]).collect()
}

/// A single message, for the checks that need a crafted claim rather than a whole run.
pub fn one_message(
    claimed_x_m: f64,
    claimed_y_m: f64,
    claimed_speed_mps: f64,
    received_at: SimTime,
) -> ObservedMessage {
    ObservedMessage {
        signer: TX_SIGNER,
        kind: ObservedKind::Beacon,
        received_at,
        claimed_generation_time: received_at,
        claimed_x_m,
        claimed_y_m,
        claimed_speed_mps,
        claimed_heading_rad: 0.0,
        claimed_pos_confidence_m: 2.0,
        repetitions: 1,
        cert_valid_from: 0,
        cert_valid_to: u64::MAX,
        station_type: StationType::Vehicle,
        verification: VerificationState::Valid,
    }
}

/// The receiver's belief at the origin.
pub fn rx_at_origin(t: SimTime) -> SelfBelief {
    SelfBelief {
        node: RX,
        believed_time: t,
        x_m: 0.0,
        y_m: 0.0,
        radio_range_m: 500.0,
    }
}
