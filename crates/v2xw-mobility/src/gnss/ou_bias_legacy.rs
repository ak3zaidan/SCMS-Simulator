//! `gnss/error/ou-bias-legacy` — the frozen reference engine's GNSS model
//! (04-models.md §2.10).
//!
//! The port the inventory asked for, with its three documented fixes:
//!
//! 1. **The confidence no longer uses the true bias.** The legacy line
//!    `conf = 2.448·sqrt(σ_nom² + bias_x² + bias_y²)` [`run.py` L1895] reads the realised
//!    bias, which a receiver cannot know. Here it is `2.448·sqrt(σ_nom² + σ_b²)` — the
//!    bias *variance*, which is a property of the receiver.
//! 2. **Weather arrives through [`GnssEnv`]** instead of a module-level global.
//! 3. **Speed and heading get noise**, which the legacy model did not give them.
//!
//! Everything else is the legacy model exactly: the same Ornstein-Uhlenbeck recursion, the
//! same per-vehicle quality draw `0.5 + Exp(λ = 1.2)`, the same outlier and burst processes,
//! the same numbers. It is kept for two reasons §2.10 gives: it is the abstract tier's
//! preset, and it is the parity oracle for a run against the frozen corpus.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::kinematics::Kinematics;
use v2xw_core::math;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::{SimTime, ns_to_secs, secs_to_ns};
use v2xw_core::{FixQuality, PositionEstimate};

use crate::ctx::MobCtx;
use crate::gnss::common::confidence_95_m;
use crate::gnss::gauss_markov::weather_error_multiplier;
use crate::traits::GnssModel;
use crate::views::{GnssEnv, SkyView};

/// The model id.
pub const MODEL_ID: &str = "gnss/error/ou-bias-legacy";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The literal the legacy engine writes for the 95 % circle factor [`run.py` L1895].
///
/// Kept as the literal `2.448` rather than the exact `sqrt(−2 ln 0.05)` so a parity run
/// against the frozen corpus compares like with like; [`crate::gnss::common::RAYLEIGH_95`]
/// is the exact value the sourced model uses.
pub const LEGACY_CIRCLE_FACTOR: f64 = 2.448;

/// The legacy model's parameters, every one `code (legacy)` (§2.10).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LegacyGnssParams {
    /// `gps_sigma_m`: white per-axis noise, metres (L297).
    pub sigma_m: f64,
    /// `gps_bias_sigma_m`: the OU bias amplitude, metres (L298).
    pub bias_sigma_m: f64,
    /// `gps_bias_tau_s`: the bias correlation time, seconds (L299).
    pub bias_tau_s: f64,
    /// `gps_outlier_rate` (L300).
    pub outlier_rate: f64,
    /// `gps_outlier_mag_m`, metres (L301).
    pub outlier_magnitude_m: f64,
    /// `gps_degrade_rate`, per step (L302).
    pub degrade_rate: f64,
    /// `gps_degrade_factor` (L303).
    pub degrade_factor: f64,
    /// `gps_degrade_dur_s`, seconds (L304).
    pub degrade_duration_s: f64,
    /// `gps_jam_rate`, per step (L305).
    pub jam_rate: f64,
    /// `gps_jam_dur_s`, seconds (L306).
    pub jam_duration_s: f64,
    /// `gps_quality_floor`: the best achievable per-vehicle quality (L310).
    pub quality_floor: f64,
    /// `gps_quality_lambda`: the rate of the exponential quality tail (L311).
    pub quality_lambda: f64,
    /// `faulty_bias_mult` (L313).
    pub faulty_bias_multiplier: f64,
    /// Speed noise, m/s — **the inventory's addition**, not in the legacy model.
    pub sigma_velocity_mps: f64,
    /// Heading noise, radians — **the inventory's addition**.
    pub sigma_heading_rad: f64,
}

impl Default for LegacyGnssParams {
    fn default() -> Self {
        Self {
            sigma_m: 1.2,
            bias_sigma_m: 1.5,
            bias_tau_s: 20.0,
            outlier_rate: 0.01,
            outlier_magnitude_m: 12.0,
            degrade_rate: 0.006,
            degrade_factor: 6.0,
            degrade_duration_s: 3.0,
            jam_rate: 0.0,
            jam_duration_s: 4.0,
            quality_floor: 0.5,
            quality_lambda: 1.2,
            faulty_bias_multiplier: 5.0,
            sigma_velocity_mps: 0.2 / 1.959963984540054,
            sigma_heading_rad: 0.0,
        }
    }
}

/// One node's legacy error state.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NodeState {
    bias: Vec3,
    quality: f64,
    last: Option<SimTime>,
    degrade_until: SimTime,
    jam_until: SimTime,
}

/// The legacy OU-bias GNSS model.
#[derive(Debug, Clone)]
pub struct LegacyGnss {
    params: LegacyGnssParams,
    state: BTreeMap<NodeId, NodeState>,
    card: ModelCard,
}

impl Default for LegacyGnss {
    fn default() -> Self {
        LegacyGnss::new(LegacyGnssParams::default())
    }
}

impl LegacyGnss {
    /// The model with the given parameters.
    pub fn new(params: LegacyGnssParams) -> Self {
        Self {
            card: card(&params),
            params,
            state: BTreeMap::new(),
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &LegacyGnssParams {
        &self.params
    }

    /// One node's drawn quality, if it has one yet.
    pub fn quality_of(&self, node: NodeId) -> Option<f64> {
        self.state.get(&node).map(|s| s.quality)
    }

    /// Draws the per-vehicle quality `0.5 + Exp(λ = 1.2)` (L310-311).
    fn quality(&self, ctx: &mut dyn MobCtx, node: NodeId) -> f64 {
        let e = ctx
            .rng(RngDomain::Gnss, EntityRef::Node(node))
            .exponential(self.params.quality_lambda);
        self.params.quality_floor + e
    }
}

impl v2xw_core::model::Model for LegacyGnss {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl GnssModel for LegacyGnss {
    fn estimate(
        &mut self,
        ctx: &mut dyn MobCtx,
        node: NodeId,
        gt: &Kinematics,
        env: &GnssEnv,
    ) -> PositionEstimate {
        let now = ctx.now();
        // The quality is drawn once per vehicle, at its first estimate, exactly as the
        // legacy engine draws it at spawn.
        if !self.state.contains_key(&node) {
            let quality = self.quality(ctx, node);
            self.state.insert(
                node,
                NodeState {
                    bias: Vec3::ZERO,
                    quality,
                    last: None,
                    degrade_until: 0,
                    jam_until: 0,
                },
            );
        }
        let dt_s = {
            let state = self.state.get_mut(&node).expect("inserted above");
            let dt = match state.last {
                Some(last) => ns_to_secs(now.saturating_sub(last)),
                None => self.params.bias_tau_s,
            };
            state.last = Some(now);
            dt
        };

        // An outage: the legacy `gps_jam_rate`, or an environment that has no sky.
        let jammed_until = self.state[&node].jam_until;
        if env.jammed || env.sky == SkyView::Obstructed || now < jammed_until {
            return PositionEstimate::no_fix(now);
        }
        if self.params.jam_rate > 0.0 {
            let coin = ctx.rng(RngDomain::Gnss, EntityRef::Node(node)).f64();
            if coin < self.params.jam_rate {
                let state = self.state.get_mut(&node).expect("inserted above");
                state.jam_until = now.saturating_add(secs_to_ns(self.params.jam_duration_s));
                return PositionEstimate::no_fix(now);
            }
        }

        // The OU recursion, [`run.py` L1876-1880].
        let a = math::exp(-dt_s / self.params.bias_tau_s.max(1e-9));
        let amplitude = self.params.bias_sigma_m
            * if env.faulty_sensor {
                self.params.faulty_bias_multiplier
            } else {
                1.0
            };
        let q = amplitude * math::sqrt((1.0 - a * a).max(1e-9));
        let (gx, gy, degrade_coin) = {
            let mut rng = ctx.rng(RngDomain::Gnss, EntityRef::Node(node));
            (rng.normal(0.0, 1.0), rng.normal(0.0, 1.0), rng.f64())
        };
        {
            let state = self.state.get_mut(&node).expect("inserted above");
            state.bias = Vec3::new(a * state.bias.x + q * gx, a * state.bias.y + q * gy, 0.0);
            // A transient bad-GNSS burst (L1882-1884).
            if now >= state.degrade_until && degrade_coin < self.params.degrade_rate {
                state.degrade_until =
                    now.saturating_add(secs_to_ns(self.params.degrade_duration_s));
            }
        }
        let state = self.state[&node];
        let weather = weather_error_multiplier(env.weather.kind);
        let env_scale = crate::gnss::common::env_scale(env.sky).unwrap_or(1.0);
        let sigma_nominal = self.params.sigma_m * state.quality * weather * env_scale;
        let sigma = sigma_nominal
            * if now < state.degrade_until {
                self.params.degrade_factor
            } else {
                1.0
            };
        let (nx, ny, outlier_coin, angle, dvx, dvy, dh) = {
            let mut rng = ctx.rng(RngDomain::Gnss, EntityRef::Node(node));
            (
                rng.normal(0.0, sigma),
                rng.normal(0.0, sigma),
                rng.f64(),
                rng.f64(),
                rng.normal(0.0, self.params.sigma_velocity_mps),
                rng.normal(0.0, self.params.sigma_velocity_mps),
                rng.normal(0.0, self.params.sigma_heading_rad),
            )
        };
        let mut pos = Vec3::new(
            gt.pos.x + state.bias.x + nx,
            gt.pos.y + state.bias.y + ny,
            gt.pos.z,
        );
        if outlier_coin < self.params.outlier_rate {
            // The legacy engine draws the angle as `r.random() * 6.283`; the exact τ is
            // used here, and the difference is under a milliradian of a uniform angle.
            let a = angle * core::f64::consts::TAU;
            let (s, c) = math::sin_cos(a);
            pos = Vec3::new(
                pos.x + self.params.outlier_magnitude_m * c,
                pos.y + self.params.outlier_magnitude_m * s,
                pos.z,
            );
        }
        // **The fix.** The legacy line is
        // `conf = 2.448·sqrt(σ_nom² + bias_x² + bias_y²)`, which reads the realised bias.
        // Here the *bias variance* takes its place: a receiver knows its own noise figure
        // and does not know the bias it is carrying.
        let radius = confidence_95_m(sigma_nominal, amplitude, LEGACY_CIRCLE_FACTOR);
        PositionEstimate {
            pos,
            vel: Vec3::new(gt.vel.x + dvx, gt.vel.y + dvy, gt.vel.z),
            heading_rad: gt.heading_rad + dh,
            semi_major_m: radius,
            semi_minor_m: radius,
            orientation_rad: 0.0,
            time_ns: now,
            fix: FixQuality::ThreeD,
        }
    }
}

/// The model card.
pub fn card(params: &LegacyGnssParams) -> ModelCard {
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L297-313, L1874-1897".to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: Some("the frozen reference engine's `measure`".to_string()),
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Gnss,
        MODEL_VERSION,
        "The frozen reference engine's GNSS model: an Ornstein-Uhlenbeck bias per axis, \
         white noise scaled by a per-vehicle quality draw and the weather, multipath \
         outliers and transient degraded bursts. Kept as the abstract-tier preset and as \
         the parity oracle for the frozen corpus, with the three fixes 04-models.md §2.10 \
         records.",
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![
        Equation {
            name: "OU bias".to_string(),
            latex_or_text: "bias_{k+1} = a·bias_k + q·N(0,1),  a = exp(−dt/τ_b),  \
                            q = σ_b·√(1 − a²)"
                .to_string(),
            notes: None,
        },
        Equation {
            name: "noise scale".to_string(),
            latex_or_text: "σ = σ_w · quality · weather_mult · (degrade_factor if in a burst)"
                .to_string(),
            notes: Some("quality = 0.5 + Exp(λ = 1.2), drawn once per vehicle".to_string()),
        },
        Equation {
            name: "reported confidence (corrected)".to_string(),
            latex_or_text: "semi_major = semi_minor = 2.448·√(σ_nom² + σ_b²)".to_string(),
            notes: Some(
                "the legacy line used the realised bias — `bias_x² + bias_y²` — which is an \
                 oracle quantity; §2.10 records the replacement and this card is where it \
                 is visible"
                    .to_string(),
            ),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "gps_sigma_m",
            "m",
            serde_json::json!(params.sigma_m),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_bias_sigma_m",
            "m",
            serde_json::json!(params.bias_sigma_m),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_bias_tau_s",
            "s",
            serde_json::json!(params.bias_tau_s),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_outlier_rate",
            "1",
            serde_json::json!(params.outlier_rate),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_outlier_mag_m",
            "m",
            serde_json::json!(params.outlier_magnitude_m),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_degrade_rate",
            "1/step",
            serde_json::json!(params.degrade_rate),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_degrade_factor",
            "1",
            serde_json::json!(params.degrade_factor),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_degrade_dur_s",
            "s",
            serde_json::json!(params.degrade_duration_s),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_jam_rate",
            "1/step",
            serde_json::json!(params.jam_rate),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_jam_dur_s",
            "s",
            serde_json::json!(params.jam_duration_s),
            legacy.clone(),
        ),
        Parameter::new(
            "gps_quality",
            "1",
            serde_json::json!({"floor": params.quality_floor, "lambda": params.quality_lambda}),
            legacy.clone(),
        ),
        Parameter::new(
            "faulty_bias_mult",
            "1",
            serde_json::json!(params.faulty_bias_multiplier),
            legacy.clone(),
        ),
        Parameter::new(
            "weather_mult",
            "1",
            serde_json::json!({"clear": 1.0, "rain": 1.5, "fog": 2.0, "snow": 2.5}),
            legacy.clone(),
        ),
        Parameter::new(
            "circle_factor",
            "1",
            serde_json::json!(LEGACY_CIRCLE_FACTOR),
            legacy,
        ),
        Parameter::new(
            "sigma_velocity",
            "m/s",
            serde_json::json!(params.sigma_velocity_mps),
            Source {
                kind: SourceKind::Standard,
                reference: "GPS SPS PS 2020 Table 3.8-3: velocity ≤ 0.2 m/s at 95 % [R3 §G.1]"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: Some(
                    "the inventory's addition: the legacy model gave speed no noise at all"
                        .to_string(),
                ),
            },
        ),
        Parameter {
            name: "sigma_heading".to_string(),
            unit: "rad".to_string(),
            default: serde_json::json!(params.sigma_heading_rad),
            range: None,
            source: Source::todo_calibrate("no heading-noise figure exists in §2.10 or §3.8"),
            calibration: Some(
                "As §3.8: fit the heading noise from the Reid dataset if it is released. \
                 Zero until then, so no invented number reaches a heading."
                    .to_string(),
            ),
        },
    ];
    card.assumptions = vec![
        "The per-vehicle quality is drawn once, at the node's first estimate, from that \
         node's own stream — so it does not depend on how many other nodes were sampled \
         first."
            .to_string(),
        "The reported confidence is computed from the model's own parameters. This is the \
         one deliberate difference from the frozen engine's arithmetic, and §2.10 records \
         why."
            .to_string(),
    ];
    card.limitations = vec![
        "Every default is `code (legacy)` with no measured backing; \
         `gnss/error/gauss-markov` is the sourced replacement and the default for the \
         medium and high tiers."
            .to_string(),
        "The bias is horizontal only, as in the legacy model: there is no vertical error."
            .to_string(),
    ];
    card.ignores = vec!["Everything §3.8's measured model models.".to_string()];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::Gnss.as_str().to_string()],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "gnss::ou_bias_legacy::tests::the_confidence_does_not_leak_the_true_bias".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::MobilityCtx;
    use crate::worlds::{RingParams, ring};
    use v2xw_core::model::Model;
    use v2xw_core::rng::RngRegistry;
    use v2xw_core::time::NS_PER_S;
    use v2xw_world::World;

    fn world() -> World {
        ring(&RingParams::default()).expect("a ring")
    }

    #[test]
    fn the_defaults_are_the_legacy_values() {
        let p = LegacyGnssParams::default();
        assert_eq!(p.sigma_m, 1.2);
        assert_eq!(p.bias_sigma_m, 1.5);
        assert_eq!(p.bias_tau_s, 20.0);
        assert_eq!(p.outlier_rate, 0.01);
        assert_eq!(p.outlier_magnitude_m, 12.0);
        assert_eq!(p.degrade_rate, 0.006);
        assert_eq!(p.degrade_factor, 6.0);
        assert_eq!(p.degrade_duration_s, 3.0);
        assert_eq!(p.jam_rate, 0.0);
        assert_eq!(p.jam_duration_s, 4.0);
        assert_eq!(p.quality_floor, 0.5);
        assert_eq!(p.quality_lambda, 1.2);
        assert_eq!(p.faulty_bias_multiplier, 5.0);
        assert_eq!(LEGACY_CIRCLE_FACTOR, 2.448);
    }

    #[test]
    fn the_confidence_does_not_leak_the_true_bias() {
        // The defect §2.10 records: the legacy confidence grew with the realised bias, so a
        // detector could recover the bias from the broadcast. Here every node of the same
        // quality reports the same radius whatever bias it happens to carry, and the radius
        // is what the parameters say.
        let w = world();
        let rng = RngRegistry::new(3);
        let mut m = LegacyGnss::new(LegacyGnssParams {
            outlier_rate: 0.0,
            degrade_rate: 0.0,
            quality_lambda: 1e9, // a degenerate quality draw, so quality ≈ the floor
            ..LegacyGnssParams::default()
        });
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let mut radii = Vec::new();
        let mut biases = Vec::new();
        for id in 0..300u32 {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            let e = m.estimate(&mut ctx, NodeId::new(id), &gt, &GnssEnv::OPEN_SKY);
            radii.push(e.semi_major_m);
            biases.push(e.pos.norm_2d());
        }
        let first = radii[0];
        assert!(
            radii.iter().all(|r| (r - first).abs() < 1e-6),
            "the radius must not vary with the realised error"
        );
        // And it is the documented arithmetic, not a coincidence.
        let sigma_nominal = 1.2 * 0.5;
        let want = 2.448 * math::sqrt(sigma_nominal * sigma_nominal + 1.5 * 1.5);
        assert!((first - want).abs() < 2e-3, "{first} against {want}");
        // The realised errors do vary, so the constancy above is a property of the
        // arithmetic and not of a degenerate sample.
        let spread = biases.iter().fold(0.0f64, |a, b| a.max(*b));
        assert!(spread > 0.5, "the errors are not all zero: {spread}");
    }

    #[test]
    fn a_faulty_sensor_carries_a_larger_bias() {
        let w = world();
        let rng = RngRegistry::new(15);
        let mut m = LegacyGnss::new(LegacyGnssParams {
            outlier_rate: 0.0,
            degrade_rate: 0.0,
            sigma_m: 0.0, // isolate the bias
            ..LegacyGnssParams::default()
        });
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let mean = |m: &mut LegacyGnss, faulty: bool, base: u32| {
            let mut total = 0.0;
            let n = 400;
            for id in 0..n {
                let mut ctx = MobilityCtx::new(0, &w, &rng);
                let e = m.estimate(
                    &mut ctx,
                    NodeId::new(base + id),
                    &gt,
                    &GnssEnv {
                        faulty_sensor: faulty,
                        ..GnssEnv::OPEN_SKY
                    },
                );
                total += e.pos.norm_2d();
            }
            total / f64::from(n)
        };
        let healthy = mean(&mut m, false, 0);
        let faulty = mean(&mut m, true, 10_000);
        assert!(
            faulty > 3.0 * healthy,
            "faulty {faulty} m against healthy {healthy} m"
        );
    }

    #[test]
    fn a_burst_inflates_the_error_but_not_the_reported_confidence() {
        // The legacy comment's point, preserved: "a real receiver underreports uncertainty
        // during a burst, so the sustained residual reads as misbehaviour".
        let w = world();
        let rng = RngRegistry::new(77);
        let mut m = LegacyGnss::new(LegacyGnssParams {
            outlier_rate: 0.0,
            degrade_rate: 1.0, // every node enters a burst immediately
            ..LegacyGnssParams::default()
        });
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let node = NodeId::new(1);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let first = m.estimate(&mut ctx, node, &gt, &GnssEnv::OPEN_SKY);
        let mut ctx = MobilityCtx::new(NS_PER_S, &w, &rng);
        let during = m.estimate(&mut ctx, node, &gt, &GnssEnv::OPEN_SKY);
        assert!((during.semi_major_m - first.semi_major_m).abs() < 1e-9);
    }

    #[test]
    fn the_quality_draw_is_per_node_and_once() {
        let w = world();
        let rng = RngRegistry::new(6);
        let mut m = LegacyGnss::default();
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let node = NodeId::new(4);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        m.estimate(&mut ctx, node, &gt, &GnssEnv::OPEN_SKY);
        let q = m.quality_of(node).expect("drawn");
        for k in 1..5u64 {
            let mut ctx = MobilityCtx::new(k * NS_PER_S, &w, &rng);
            m.estimate(&mut ctx, node, &gt, &GnssEnv::OPEN_SKY);
        }
        assert_eq!(
            m.quality_of(node),
            Some(q),
            "drawn once, at the first estimate"
        );
        assert!(q >= 0.5, "the floor holds: {q}");
    }

    #[test]
    fn a_jammed_receiver_reports_no_fix() {
        let w = world();
        let rng = RngRegistry::new(2);
        let mut m = LegacyGnss::default();
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let e = m.estimate(
            &mut ctx,
            NodeId::new(1),
            &gt,
            &GnssEnv {
                jammed: true,
                ..GnssEnv::OPEN_SKY
            },
        );
        assert_eq!(e.fix, FixQuality::NoFix);
        assert!(e.semi_major_m.is_infinite());
    }

    #[test]
    fn the_card_validates_and_is_abstract_tier() {
        let m = LegacyGnss::default();
        m.card().validate().expect("validates");
        assert_eq!(m.card().tier, vec![Tier::Abstract]);
    }
}
