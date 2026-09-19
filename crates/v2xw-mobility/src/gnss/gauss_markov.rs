//! `gnss/error/gauss-markov` — the measured-quantile GNSS model (04-models.md §3.8).
//!
//! # Structure
//!
//! Per axis, a first-order Gauss-Markov process plus white noise, the structure §2.10 gives
//! and §3.8 keeps:
//!
//! ```text
//! b_{k+1} = a·b_k + q·N(0,1),   a = exp(−Δt/τ),   q = σ_b·sqrt(1 − a²)
//! error   = b + N(0, σ_w)  (+ an outlier of magnitude m_o at a uniform angle, with
//!                            probability p_o)
//! ```
//!
//! The autocorrelation is `R(Δt) = σ²·exp(−|Δt|/τ)` [generic GNSS/INS formulation,
//! R3 §G.5], and `q` is exactly what keeps the process stationary: `var(b) = σ_b²` for every
//! Δt.
//!
//! # Where the numbers come from
//!
//! §3.8 prescribes the construction and this module performs it at *construction time*:
//!
//! 1. The **total** per-axis scale σ is fitted by least squares to the measured
//!    Reid 2019 percentile rows — horizontal 3.07 / 5.30 / 9.38 m through the Rayleigh
//!    factors (a horizontal error is a two-dimensional magnitude), vertical
//!    4.59 / 9.42 / 12.83 m through the folded-normal factors. The fit and its residual go
//!    on the card, because one Gaussian scale cannot reproduce three measured percentiles:
//!    the real distribution has heavier tails, and §3.8 asks for the disagreement to be
//!    recorded rather than hidden.
//! 2. The **white** part is the MathWorks `gpsSensor` default accuracy, 1.6 m horizontal
//!    and 3 m vertical [R3 §G.5].
//! 3. The **correlated** part is what is left: `σ_b = sqrt(max(0, σ² − σ_w²))` — DERIVED,
//!    and the arithmetic is right here.
//! 4. **τ** comes from the same `gpsSensor` default, a decay factor of 0.999 per sample:
//!    `τ = −Δt_sample / ln(0.999)`, which at the 1 Hz default sample rate is 999.5 s. The
//!    1 Hz assumption is recorded on the card, because R3 §G.5 gives the factor and not the
//!    rate.
//!
//! Nothing else is invented: the outlier rate and magnitude, the burst rate and factor and
//! the per-vehicle quality spread have no measured source in the sheets, so they keep the
//! frozen engine's values with `code (legacy)` and the shared `TODO: calibrate` plan of
//! §2.10.
//!
//! # Quality classes
//!
//! `rtk` uses the OxTS RT3000 rows (0.26 / 1.05 / 3.91 m horizontal) and a 60 s outage 95th
//! percentile; everything else uses the production single-frequency rows and 7 s. The
//! `deep-canyon` and `canyon-mitigated` environments scale σ by the measured ratio of their
//! mean error to the open-sky mean (§3.8).

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
use v2xw_core::time::{SimTime, ns_to_secs};
use v2xw_core::{FixQuality, PositionEstimate};

use crate::ctx::MobCtx;
use crate::gnss::common::{
    RAYLEIGH_95, confidence_95_m, env_scale, exponential_mean_for_p95, fit_scale,
    folded_normal_factor, rayleigh_factor,
};
use crate::traits::GnssModel;
use crate::views::{GnssEnv, SkyView};

/// The model id.
pub const MODEL_ID: &str = "gnss/error/gauss-markov";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// Reid 2019 horizontal error percentiles for a production automotive single-frequency
/// receiver under open sky, metres: 68 %, 95 %, 99 %.
pub const REID_HORIZONTAL_M: [(f64, f64); 3] = [(0.68, 3.07), (0.95, 5.30), (0.99, 9.38)];

/// Reid 2019 lateral error percentiles, metres.
pub const REID_LATERAL_M: [(f64, f64); 3] = [(0.68, 1.92), (0.95, 3.88), (0.99, 5.74)];

/// Reid 2019 longitudinal error percentiles, metres.
pub const REID_LONGITUDINAL_M: [(f64, f64); 3] = [(0.68, 2.11), (0.95, 4.44), (0.99, 7.95)];

/// Reid 2019 vertical error percentiles, metres.
pub const REID_VERTICAL_M: [(f64, f64); 3] = [(0.68, 4.59), (0.95, 9.42), (0.99, 12.83)];

/// Reid 2019 RTK (OxTS RT3000) horizontal error percentiles, metres.
pub const REID_RTK_HORIZONTAL_M: [(f64, f64); 3] = [(0.68, 0.26), (0.95, 1.05), (0.99, 3.91)];

/// The MathWorks `gpsSensor` default horizontal accuracy, metres [R3 §G.5].
pub const GPS_SENSOR_HORIZONTAL_M: f64 = 1.6;

/// The MathWorks `gpsSensor` default vertical accuracy, metres [R3 §G.5].
pub const GPS_SENSOR_VERTICAL_M: f64 = 3.0;

/// The MathWorks `gpsSensor` default decay factor, per sample [R3 §G.5].
pub const GPS_SENSOR_DECAY_FACTOR: f64 = 0.999;

/// The sample interval the decay factor is assumed to apply at, seconds.
///
/// R3 §G.5 gives the factor but not the rate; 1 Hz is `gpsSensor`'s own default sample rate
/// and the assumption is recorded on the card.
pub const GPS_SENSOR_SAMPLE_S: f64 = 1.0;

/// The SPS outage 95th percentile, seconds [Reid 2019].
pub const SPS_OUTAGE_P95_S: f64 = 7.0;

/// The RTK outage 95th percentile, seconds [Reid 2019: "can exceed 60 s"].
pub const RTK_OUTAGE_P95_S: f64 = 60.0;

/// The GPS SPS committed 95 % velocity accuracy, m/s [GPS SPS PS 2020 Table 3.8-3].
pub const SPS_VELOCITY_P95_MPS: f64 = 0.2;

/// One fitted scale and the residual of its fit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuantileFit {
    /// What was fitted, for the card: `horizontal`, `vertical`, `rtk-horizontal`.
    pub name: String,
    /// The fitted per-axis standard deviation, metres.
    pub sigma_m: f64,
    /// The percentile rows the fit was made against, `(p, measured_m)`.
    pub rows: Vec<(f64, f64)>,
    /// `σ·k_p − measured_p` for each row, metres: what the single-scale model gets wrong.
    pub residuals_m: Vec<f64>,
}

impl QuantileFit {
    /// Fits a horizontal (two-dimensional magnitude) percentile set.
    pub fn horizontal(name: &str, rows: &[(f64, f64)]) -> Self {
        let pairs: Vec<(f64, f64)> = rows
            .iter()
            .map(|(p, v)| (rayleigh_factor(*p), *v))
            .collect();
        let (sigma_m, residuals_m) = fit_scale(&pairs);
        Self {
            name: name.to_string(),
            sigma_m,
            rows: rows.to_vec(),
            residuals_m,
        }
    }

    /// Fits a one-dimensional (signed axis) percentile set.
    ///
    /// Rows at percentiles other than 68, 95 and 99 are skipped, because the standard
    /// normal quantile is tabulated here for exactly those three (§3.8's rows).
    pub fn axis(name: &str, rows: &[(f64, f64)]) -> Self {
        let pairs: Vec<(f64, f64)> = rows
            .iter()
            .filter_map(|(p, v)| folded_normal_factor(*p).map(|z| (z, *v)))
            .collect();
        let (sigma_m, residuals_m) = fit_scale(&pairs);
        Self {
            name: name.to_string(),
            sigma_m,
            rows: rows.to_vec(),
            residuals_m,
        }
    }

    /// The largest absolute residual, metres — the number that says how well one Gaussian
    /// scale can stand in for the measured distribution.
    pub fn worst_residual_m(&self) -> f64 {
        self.residuals_m
            .iter()
            .fold(0.0f64, |worst, r| worst.max(r.abs()))
    }
}

/// The model's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GaussMarkovParams {
    /// White-noise standard deviation per horizontal axis, metres.
    pub sigma_white_h_m: f64,
    /// White-noise standard deviation vertically, metres.
    pub sigma_white_v_m: f64,
    /// The bias correlation time, seconds.
    pub tau_s: f64,
    /// Outlier probability per estimate.
    pub outlier_rate: f64,
    /// Outlier magnitude, metres.
    pub outlier_magnitude_m: f64,
    /// Probability per estimate of entering a degraded burst.
    pub burst_rate: f64,
    /// The σ multiplier during a burst.
    pub burst_factor: f64,
    /// Burst duration, seconds.
    pub burst_duration_s: f64,
    /// Probability per estimate of losing the fix entirely.
    pub outage_rate: f64,
    /// Whether the receiver is an RTK unit, which chooses the RTK quantile rows and the
    /// 60 s outage percentile.
    pub rtk: bool,
    /// The multiplier a faulty sensor's bias carries.
    pub faulty_bias_multiplier: f64,
    /// Velocity noise standard deviation, m/s.
    pub sigma_velocity_mps: f64,
    /// Heading noise standard deviation, radians.
    pub sigma_heading_rad: f64,
}

impl Default for GaussMarkovParams {
    fn default() -> Self {
        Self {
            sigma_white_h_m: GPS_SENSOR_HORIZONTAL_M,
            sigma_white_v_m: GPS_SENSOR_VERTICAL_M,
            tau_s: -GPS_SENSOR_SAMPLE_S / math::ln(GPS_SENSOR_DECAY_FACTOR),
            outlier_rate: 0.01,
            outlier_magnitude_m: 12.0,
            burst_rate: 0.006,
            burst_factor: 6.0,
            burst_duration_s: 3.0,
            outage_rate: 0.0,
            rtk: false,
            faulty_bias_multiplier: 5.0,
            // The GPS SPS committed 95 % velocity bound, 0.2 m/s, read as a 95 % figure of a
            // zero-mean Gaussian: σ = 0.2 / 1.96. §3.8's interim value.
            sigma_velocity_mps: SPS_VELOCITY_P95_MPS / 1.959963984540054,
            // `TODO: calibrate` — §3.8 has no heading figure at all. Zero, so the model adds
            // no heading error rather than an invented one.
            sigma_heading_rad: 0.0,
        }
    }
}

/// One node's error state.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct NodeState {
    bias: Vec3,
    /// When the state was last advanced.
    last: Option<SimTime>,
    /// When the current degraded burst ends.
    burst_until: SimTime,
    /// When the current outage ends.
    outage_until: SimTime,
}

/// The fitted Gauss-Markov GNSS model.
#[derive(Debug, Clone)]
pub struct GaussMarkovGnss {
    params: GaussMarkovParams,
    /// The fitted horizontal scale (RTK or SPS, per the parameters).
    horizontal: QuantileFit,
    /// The fitted vertical scale.
    vertical: QuantileFit,
    /// The lateral and longitudinal fits, for the card: they are not used by the sampler,
    /// which is isotropic in the horizontal plane, and their residuals are what say how
    /// much anisotropy the measurement shows.
    lateral: QuantileFit,
    longitudinal: QuantileFit,
    /// The correlated part of the horizontal scale, metres (DERIVED).
    sigma_bias_h_m: f64,
    /// The correlated part of the vertical scale, metres (DERIVED).
    sigma_bias_v_m: f64,
    state: BTreeMap<NodeId, NodeState>,
    card: ModelCard,
}

impl Default for GaussMarkovGnss {
    fn default() -> Self {
        GaussMarkovGnss::new(GaussMarkovParams::default())
    }
}

impl GaussMarkovGnss {
    /// Fits the model to the measured quantiles and builds it.
    pub fn new(params: GaussMarkovParams) -> Self {
        let horizontal = if params.rtk {
            QuantileFit::horizontal("rtk-horizontal", &REID_RTK_HORIZONTAL_M)
        } else {
            QuantileFit::horizontal("horizontal", &REID_HORIZONTAL_M)
        };
        let vertical = QuantileFit::axis("vertical", &REID_VERTICAL_M);
        let lateral = QuantileFit::axis("lateral", &REID_LATERAL_M);
        let longitudinal = QuantileFit::axis("longitudinal", &REID_LONGITUDINAL_M);
        // The correlated part is what the white part does not explain.
        let sigma_bias_h_m = math::sqrt(
            (horizontal.sigma_m * horizontal.sigma_m
                - params.sigma_white_h_m * params.sigma_white_h_m)
                .max(0.0),
        );
        let sigma_bias_v_m = math::sqrt(
            (vertical.sigma_m * vertical.sigma_m - params.sigma_white_v_m * params.sigma_white_v_m)
                .max(0.0),
        );
        Self {
            card: card(
                &params,
                &[&horizontal, &vertical, &lateral, &longitudinal],
                sigma_bias_h_m,
                sigma_bias_v_m,
            ),
            params,
            horizontal,
            vertical,
            lateral,
            longitudinal,
            sigma_bias_h_m,
            sigma_bias_v_m,
            state: BTreeMap::new(),
        }
    }

    /// An RTK receiver: the OxTS RT3000 rows and the 60 s outage percentile.
    pub fn rtk() -> Self {
        Self::new(GaussMarkovParams {
            rtk: true,
            ..GaussMarkovParams::default()
        })
    }

    /// The parameters in force.
    pub fn params(&self) -> &GaussMarkovParams {
        &self.params
    }

    /// The fits, in the order `[horizontal, vertical, lateral, longitudinal]`.
    pub fn fits(&self) -> [&QuantileFit; 4] {
        [
            &self.horizontal,
            &self.vertical,
            &self.lateral,
            &self.longitudinal,
        ]
    }

    /// The correlated (bias) standard deviations, `(horizontal, vertical)`, metres.
    pub fn sigma_bias_m(&self) -> (f64, f64) {
        (self.sigma_bias_h_m, self.sigma_bias_v_m)
    }

    /// The outage 95th percentile this receiver class carries, seconds.
    pub fn outage_p95_s(&self) -> f64 {
        if self.params.rtk {
            RTK_OUTAGE_P95_S
        } else {
            SPS_OUTAGE_P95_S
        }
    }

    /// The fix quality this receiver reports when it has one.
    fn nominal_quality(&self) -> FixQuality {
        if self.params.rtk {
            FixQuality::Rtk
        } else {
            FixQuality::ThreeD
        }
    }

    /// The horizontal white-noise σ in force, given the environment and the burst state.
    fn sigma_white_h(&self, env: &GnssEnv, in_burst: bool) -> f64 {
        let weather = crate::weather::legacy_driving_effects(&env.weather);
        // §2.10 injects the weather through the same legacy multiplier table the abstract
        // tier uses; a GNSS-specific measured table does not exist in the sheets. The
        // table's *GNSS* column is 1.0 / 1.5 / 2.0 / 2.5 for clear / rain / fog / snow, and
        // `legacy_driving_effects` carries the *speed* column, so the GNSS column is read
        // from `weather_error_multiplier` instead. The call above is kept only to make the
        // shared provenance visible.
        let _ = weather;
        let mult = weather_error_multiplier(env.weather.kind);
        let env_scale = env_scale(env.sky).unwrap_or(1.0);
        let burst = if in_burst {
            self.params.burst_factor
        } else {
            1.0
        };
        self.params.sigma_white_h_m * mult * env_scale * burst
    }

    /// Advances one node's bias by `dt` and returns it.
    ///
    /// `cold` starts the process **at its stationary variance** — the initial bias is drawn
    /// from `N(0, σ_b²)` rather than from zero — because a receiver that has just been
    /// looked at has been running for a while. Starting at zero would make the first
    /// estimate of every node systematically better than the steady state, by a factor
    /// `√(1 − exp(−2Δt/τ))`, and the fitted quantiles would then be reproduced only after
    /// a few correlation times.
    fn advance_bias(
        &mut self,
        ctx: &mut dyn MobCtx,
        node: NodeId,
        dt_s: f64,
        env: &GnssEnv,
        cold: bool,
    ) -> Vec3 {
        let tau = self.params.tau_s.max(1e-9);
        let a = if cold { 0.0 } else { math::exp(-dt_s / tau) };
        let faulty = if env.faulty_sensor {
            self.params.faulty_bias_multiplier
        } else {
            1.0
        };
        let scale = env_scale(env.sky).unwrap_or(1.0) * faulty;
        // At a cold start the innovation carries the whole stationary variance; afterwards
        // it carries exactly what the decay removed.
        let innovation = math::sqrt((1.0 - a * a).max(0.0));
        let q_h = self.sigma_bias_h_m * scale * innovation;
        let q_v = self.sigma_bias_v_m * scale * innovation;
        let (dx, dy, dz) = {
            let mut rng = ctx.rng(RngDomain::Gnss, EntityRef::Node(node));
            (
                rng.normal(0.0, 1.0),
                rng.normal(0.0, 1.0),
                rng.normal(0.0, 1.0),
            )
        };
        let state = self.state.entry(node).or_default();
        state.bias = Vec3::new(
            a * state.bias.x + q_h * dx,
            a * state.bias.y + q_h * dy,
            a * state.bias.z + q_v * dz,
        );
        state.bias
    }
}

/// The GNSS error multiplier of the legacy weather table (§2.6, `WEATHER_MULT`):
/// clear 1.0, rain 1.5, fog 2.0, snow 2.5.
///
/// Sleet takes snow's multiplier (the worse of its two neighbours) and wind takes 1.0, both
/// recorded as design choices — the table has four states.
pub fn weather_error_multiplier(kind: v2xw_core::weather::WeatherKind) -> f64 {
    use v2xw_core::weather::WeatherKind as K;
    match kind {
        K::Clear | K::Wind => 1.0,
        K::Rain => 1.5,
        K::Fog => 2.0,
        K::Snow | K::Sleet => 2.5,
        _ => 1.0,
    }
}

impl v2xw_core::model::Model for GaussMarkovGnss {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl GnssModel for GaussMarkovGnss {
    fn estimate(
        &mut self,
        ctx: &mut dyn MobCtx,
        node: NodeId,
        gt: &Kinematics,
        env: &GnssEnv,
    ) -> PositionEstimate {
        let now = ctx.now();
        let (dt_s, cold) = {
            let state = self.state.entry(node).or_default();
            let cold = state.last.is_none();
            let dt = match state.last {
                Some(last) => ns_to_secs(now.saturating_sub(last)),
                None => 0.0,
            };
            state.last = Some(now);
            (dt, cold)
        };

        // An obstructed receiver and a jammed one have no fix at all, and neither has a
        // receiver inside an outage.
        let outage_until = self.state.get(&node).map_or(0, |s| s.outage_until);
        if env.jammed || env.sky == SkyView::Obstructed || now < outage_until {
            return PositionEstimate::no_fix(now);
        }
        // Does an outage start now?
        if self.params.outage_rate > 0.0 {
            let (coin, duration) = {
                let mut rng = ctx.rng(RngDomain::Gnss, EntityRef::Node(node));
                let coin = rng.f64();
                let mean = exponential_mean_for_p95(self.outage_p95_s());
                let duration = rng.exponential(1.0 / mean.max(f64::EPSILON));
                (coin, duration)
            };
            if coin < self.params.outage_rate {
                let state = self.state.entry(node).or_default();
                state.outage_until = now.saturating_add(v2xw_core::time::secs_to_ns(duration));
                return PositionEstimate::no_fix(now);
            }
        }
        // Does a degraded burst start now?
        {
            let coin = ctx.rng(RngDomain::Gnss, EntityRef::Node(node)).f64();
            let state = self.state.entry(node).or_default();
            if now >= state.burst_until && coin < self.params.burst_rate {
                state.burst_until =
                    now.saturating_add(v2xw_core::time::secs_to_ns(self.params.burst_duration_s));
            }
        }
        let in_burst = self.state.get(&node).is_some_and(|s| now < s.burst_until);

        let bias = self.advance_bias(ctx, node, dt_s, env, cold);
        let sigma_h = self.sigma_white_h(env, in_burst);
        let sigma_v =
            sigma_h * (self.params.sigma_white_v_m / self.params.sigma_white_h_m.max(1e-9));
        let (nx, ny, nz, outlier_coin, angle, dvx, dvy, dh) = {
            let mut rng = ctx.rng(RngDomain::Gnss, EntityRef::Node(node));
            (
                rng.normal(0.0, sigma_h),
                rng.normal(0.0, sigma_h),
                rng.normal(0.0, sigma_v),
                rng.f64(),
                rng.uniform(0.0, core::f64::consts::TAU),
                rng.normal(0.0, self.params.sigma_velocity_mps),
                rng.normal(0.0, self.params.sigma_velocity_mps),
                rng.normal(0.0, self.params.sigma_heading_rad),
            )
        };
        let mut pos = Vec3::new(
            gt.pos.x + bias.x + nx,
            gt.pos.y + bias.y + ny,
            gt.pos.z + bias.z + nz,
        );
        if outlier_coin < self.params.outlier_rate {
            let (s, c) = math::sin_cos(angle);
            pos = Vec3::new(
                pos.x + self.params.outlier_magnitude_m * c,
                pos.y + self.params.outlier_magnitude_m * s,
                pos.z,
            );
        }
        // The confidence the receiver may honestly report: **its own** noise parameters,
        // never the bias it happens to be carrying (the §2.10 correction). The nominal σ is
        // used rather than the burst-inflated one, because a real receiver under-reports
        // during a multipath spike — which is what makes the sustained residual detectable.
        //
        // BOTH halves carry the environment scale, because `advance_bias` scales the bias
        // innovation by it exactly as `sigma_white_h` scales the white term. Leaving it off
        // the bias half reported a radius fitted to open sky in every environment, so a
        // nominal 95 % circle held 73 % of the model's own errors with NLOS exclusion
        // working and 68 % in a deep canyon — and a misbehaviour detector reads this
        // number. `faulty_bias_multiplier` is deliberately *not* applied to either half: a
        // receiver does not know it is faulty, and the whole point of the faulty class is
        // that its reported confidence stays honest-looking while its error grows.
        let sigma_nominal = self.sigma_white_h(env, false);
        let bias_nominal = self.sigma_bias_h_m * env_scale(env.sky).unwrap_or(1.0);
        let radius = confidence_95_m(sigma_nominal, bias_nominal, RAYLEIGH_95);
        PositionEstimate {
            pos,
            vel: Vec3::new(gt.vel.x + dvx, gt.vel.y + dvy, gt.vel.z),
            heading_rad: gt.heading_rad + dh,
            semi_major_m: radius,
            semi_minor_m: radius,
            orientation_rad: 0.0,
            time_ns: now,
            fix: self.nominal_quality(),
        }
    }
}

/// The model card, including the fits and their residuals (§3.8).
pub fn card(
    params: &GaussMarkovParams,
    fits: &[&QuantileFit],
    sigma_bias_h_m: f64,
    sigma_bias_v_m: f64,
) -> ModelCard {
    let reid = Source {
        kind: SourceKind::Paper,
        reference: "Reid et al. 2019, measured automotive GNSS error percentiles [R3 §G.2]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let mathworks = Source {
        kind: SourceKind::Datasheet,
        reference: "MathWorks `gpsSensor` defaults: DecayFactor 0.999 per sample, horizontal \
                    accuracy 1.6 m, vertical 3 m [R3 §G.5]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: Some(
            "the sample rate the decay factor applies at is not in the sheet; 1 Hz is \
             `gpsSensor`'s own default and is the assumption recorded here"
                .to_string(),
        ),
    };
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L297-313".to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: None,
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Gnss,
        MODEL_VERSION,
        "A node's belief about its own position: a first-order Gauss-Markov bias per axis \
         plus white noise, with outliers, degraded bursts and outages, whose scales are \
         fitted at construction to the measured Reid 2019 error percentiles. The reported \
         confidence comes from the receiver's own noise parameters and never from the bias \
         it is carrying, which is the defect 04-models.md §2.10 records in the legacy model.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "Gauss-Markov bias".to_string(),
            latex_or_text: "b_{k+1} = a·b_k + q·N(0,1),  a = exp(−Δt/τ),  q = σ_b·√(1 − a²)"
                .to_string(),
            notes: Some(
                "q keeps the process stationary at var(b) = σ_b² for every Δt; \
                 R(Δt) = σ²·exp(−|Δt|/τ) [R3 §G.5]"
                    .to_string(),
            ),
        },
        Equation {
            name: "estimate".to_string(),
            latex_or_text: "pos = truth + b + N(0, σ_w)  (+ m_o at a uniform angle with \
                            probability p_o)"
                .to_string(),
            notes: None,
        },
        Equation {
            name: "reported confidence".to_string(),
            latex_or_text: "semi_major = semi_minor = 2.4477·√(σ_nom² + σ_b²)".to_string(),
            notes: Some(
                "the receiver's own parameters; the legacy model used the realised bias, \
                 which is an oracle quantity (§2.10)"
                    .to_string(),
            ),
        },
        Equation {
            name: "scale fit".to_string(),
            latex_or_text: "σ = Σ k_p·v_p / Σ k_p²,  residual_p = σ·k_p − v_p".to_string(),
            notes: Some(
                "k_p is the Rayleigh factor √(−2 ln(1−p)) for a horizontal magnitude and \
                 the folded-normal factor Φ⁻¹((1+p)/2) for a single axis"
                    .to_string(),
            ),
        },
        Equation {
            name: "bias-white split".to_string(),
            latex_or_text: "σ_b = √(max(0, σ² − σ_w²))".to_string(),
            notes: Some("DERIVED from the fitted total and the cited white part".to_string()),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "reid_horizontal_percentiles",
            "m",
            serde_json::json!(REID_HORIZONTAL_M),
            reid.clone(),
        ),
        Parameter::new(
            "reid_lateral_percentiles",
            "m",
            serde_json::json!(REID_LATERAL_M),
            reid.clone(),
        ),
        Parameter::new(
            "reid_longitudinal_percentiles",
            "m",
            serde_json::json!(REID_LONGITUDINAL_M),
            reid.clone(),
        ),
        Parameter::new(
            "reid_vertical_percentiles",
            "m",
            serde_json::json!(REID_VERTICAL_M),
            reid.clone(),
        ),
        Parameter::new(
            "reid_rtk_horizontal_percentiles",
            "m",
            serde_json::json!(REID_RTK_HORIZONTAL_M),
            reid.clone(),
        ),
        Parameter::new(
            "sigma_white_horizontal",
            "m",
            serde_json::json!(params.sigma_white_h_m),
            mathworks.clone(),
        ),
        Parameter::new(
            "sigma_white_vertical",
            "m",
            serde_json::json!(params.sigma_white_v_m),
            mathworks.clone(),
        ),
        Parameter::new(
            "tau",
            "s",
            serde_json::json!(params.tau_s),
            mathworks.clone(),
        ),
        Parameter::new(
            "sigma_bias_horizontal",
            "m",
            serde_json::json!(sigma_bias_h_m),
            Source {
                kind: SourceKind::Paper,
                reference: "DERIVED: √(σ_fitted² − σ_white²) from the Reid fit and the \
                            gpsSensor white part"
                    .to_string(),
                accessed: None,
                note: Some("the arithmetic is in `GaussMarkovGnss::new`".to_string()),
            },
        ),
        Parameter::new(
            "sigma_bias_vertical",
            "m",
            serde_json::json!(sigma_bias_v_m),
            Source::new(
                SourceKind::Paper,
                "DERIVED: √(σ_fitted² − σ_white²) for the vertical axis",
            ),
        ),
        Parameter::new(
            "outage_p95",
            "s",
            serde_json::json!({"sps": SPS_OUTAGE_P95_S, "rtk": RTK_OUTAGE_P95_S}),
            reid.clone(),
        ),
        Parameter::new(
            "env_scale",
            "1",
            serde_json::json!({
                "open-sky": 1.0,
                "canyon-mitigated": 9.57 / 3.07,
                "deep-canyon": 31.02 / 3.07,
            }),
            Source {
                kind: SourceKind::Paper,
                reference: "Wen & Hsu urban-canyon means against the Reid open-sky mean \
                            [R3 §G.3]"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: Some(
                    "§3.8 marks the *choice of class* `TODO: calibrate` (a sky-view metric \
                     has still to be defined); the ratios themselves are measured"
                        .to_string(),
                ),
            },
        ),
        Parameter::new(
            "sigma_velocity",
            "m/s",
            serde_json::json!(params.sigma_velocity_mps),
            Source {
                kind: SourceKind::Standard,
                reference: "GPS SPS PS 2020 Table 3.8-3: velocity ≤ 0.2 m/s at 95 % \
                            [R3 §G.1]"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: Some(
                    "read as the 95 % point of a zero-mean Gaussian: σ = 0.2/1.96".to_string(),
                ),
            },
        ),
        Parameter {
            name: "sigma_heading".to_string(),
            unit: "rad".to_string(),
            default: serde_json::json!(params.sigma_heading_rad),
            range: None,
            source: Source::todo_calibrate("heading noise has no figure in §3.8 at all"),
            calibration: Some(
                "04-models.md §3.8's plan, unchanged: fit the heading noise from the Reid \
                 dataset if it is released. Until then the default is zero, so the model \
                 adds no heading error rather than an invented one."
                    .to_string(),
            ),
        },
        Parameter {
            name: "outlier_rate".to_string(),
            unit: "1".to_string(),
            default: serde_json::json!(params.outlier_rate),
            range: None,
            source: Source::todo_calibrate(
                "the multipath outlier rate has no measured source in the sheets; the \
                 frozen engine's value (run.py L300) is used",
            ),
            calibration: Some(
                "04-models.md §2.10's shared plan: fit σ_w, σ_b, τ_b and the outlier \
                 process against the Rayleigh calibration the dataset toolchain already \
                 runs (`datagen/calibration.py`) on a public GNSS error trace."
                    .to_string(),
            ),
        },
        Parameter::new(
            "outlier_magnitude",
            "m",
            serde_json::json!(params.outlier_magnitude_m),
            legacy.clone(),
        ),
        Parameter::new(
            "burst_rate",
            "1",
            serde_json::json!(params.burst_rate),
            legacy.clone(),
        ),
        Parameter::new(
            "burst_factor",
            "1",
            serde_json::json!(params.burst_factor),
            legacy.clone(),
        ),
        Parameter::new(
            "burst_duration",
            "s",
            serde_json::json!(params.burst_duration_s),
            legacy.clone(),
        ),
        Parameter::new(
            "outage_rate",
            "1",
            serde_json::json!(params.outage_rate),
            legacy.clone(),
        ),
        Parameter::new(
            "faulty_bias_multiplier",
            "1",
            serde_json::json!(params.faulty_bias_multiplier),
            legacy.clone(),
        ),
        Parameter::new(
            "weather_error_multiplier",
            "1",
            serde_json::json!({"clear": 1.0, "rain": 1.5, "fog": 2.0, "snow": 2.5}),
            legacy,
        ),
    ];
    // The fits themselves, so a reader can see how well one scale stands in for three
    // measured percentiles.
    for fit in fits {
        card.parameters.push(Parameter::new(
            format!("fit.{}", fit.name),
            "m",
            serde_json::json!({
                "sigma_m": fit.sigma_m,
                "rows": fit.rows,
                "residuals_m": fit.residuals_m,
                "worst_residual_m": fit.worst_residual_m(),
            }),
            reid.clone(),
        ));
    }
    card.assumptions = vec![
        "The horizontal error is isotropic: one scale for both horizontal axes. The \
         lateral and longitudinal fits are on the card so a reader can see how much \
         anisotropy the measurement actually shows (about 10 %)."
            .to_string(),
        "One Gaussian scale cannot reproduce three measured percentiles; the fit residuals \
         are on this card rather than hidden."
            .to_string(),
        "The reported confidence uses the nominal σ, not the burst-inflated one: a real \
         receiver under-reports during a multipath spike, which is what makes the \
         sustained residual detectable (§2.10)."
            .to_string(),
        "The `gpsSensor` decay factor is applied at a 1 Hz sample interval.".to_string(),
    ];
    card.limitations = vec![
        "No satellite geometry, no multipath as a function of the real building geometry — \
         only an environment class (§3.8 ignores)."
            .to_string(),
        "Receiver filtering dynamics are not modelled.".to_string(),
    ];
    card.ignores = vec![
        "Satellite geometry, true-geometry multipath and receiver filtering dynamics \
         (04-models.md §3.8)."
            .to_string(),
    ];
    card.sources = vec![reid, mathworks];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::Gnss.as_str().to_string()],
    };
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "Reid et al. 2019 percentiles [R3 §G.2]",
        )],
        tests: vec![
            "gnss::gauss_markov::tests::the_sampled_quantiles_are_near_the_measured_ones"
                .to_string(),
            "gnss::gauss_markov::tests::the_confidence_never_reads_the_realised_bias".to_string(),
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
    use v2xw_core::weather::{SurfaceCondition, WeatherKind, WeatherState};
    use v2xw_world::World;

    fn world() -> World {
        ring(&RingParams::default()).expect("a ring")
    }

    #[test]
    fn the_fit_reproduces_the_measured_rows_to_its_recorded_residual() {
        let m = GaussMarkovGnss::default();
        let h = &m.fits()[0];
        assert_eq!(h.name, "horizontal");
        // The fitted scale is between the three per-row implied scales.
        assert!(h.sigma_m > 2.0 && h.sigma_m < 3.2, "σ = {}", h.sigma_m);
        assert_eq!(h.residuals_m.len(), 3);
        // The 68 % and 95 % rows are matched closely; the 99 % row is where the single
        // Gaussian's light tail shows, and the residual records it.
        assert!(
            h.worst_residual_m() > 0.1,
            "a perfect fit would be suspicious"
        );
        assert!(h.worst_residual_m() < 2.0, "{}", h.worst_residual_m());
        // The white part is the cited gpsSensor figure and the bias part is the remainder.
        let (bias_h, bias_v) = m.sigma_bias_m();
        assert!(
            (bias_h * bias_h + GPS_SENSOR_HORIZONTAL_M * GPS_SENSOR_HORIZONTAL_M
                - h.sigma_m * h.sigma_m)
                .abs()
                < 1e-9
        );
        assert!(bias_v > 0.0);
        // τ comes from the decay factor: −1 s / ln 0.999 ≈ 999.5 s.
        assert!(
            (m.params().tau_s - 999.5).abs() < 0.5,
            "τ = {}",
            m.params().tau_s
        );
    }

    #[test]
    fn the_sampled_quantiles_are_near_the_measured_ones() {
        let w = world();
        let rng = RngRegistry::new(2024);
        let mut m = GaussMarkovGnss::new(GaussMarkovParams {
            outlier_rate: 0.0,
            burst_rate: 0.0,
            ..GaussMarkovParams::default()
        });
        // Sample one estimate per node, so each is an independent draw from the stationary
        // distribution (a cold start begins at the stationary variance).
        let gt = Kinematics::at_rest(0, Vec3::new(100.0, 100.0, 0.0));
        let mut errors: Vec<f64> = Vec::new();
        for id in 0..4000u32 {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            let e = m.estimate(&mut ctx, NodeId::new(id), &gt, &GnssEnv::OPEN_SKY);
            errors.push(e.pos.distance_2d(gt.pos));
        }
        v2xw_core::math::sort_total_order(&mut errors);
        let q = |p: f64| v2xw_core::math::quantile_sorted(&errors, p);
        // The fit's own prediction for each percentile, and the measured row it was fitted
        // to. The sampled quantiles must match the *fit*, which is the model's claim, to
        // within sampling error; the measured row is reproduced to the fit residual.
        let h = m.fits()[0].clone();
        for (i, (p, measured)) in h.rows.iter().enumerate() {
            let predicted = h.sigma_m * crate::gnss::common::rayleigh_factor(*p);
            let sampled = q(*p);
            // The tolerance is the quantile estimator's own sampling error: at p = 0.99
            // only 1 % of 4,000 draws sit above the point, so its standard error is about
            // 0.14 m and three of those is 0.42 m.
            assert!(
                (sampled - predicted).abs() < 0.45,
                "p={p}: sampled {sampled} against the fit's {predicted}"
            );
            let residual = h.residuals_m[i];
            assert!(
                (sampled - measured).abs() < residual.abs() + 0.45,
                "p={p}: sampled {sampled} against the measured {measured} \
                 (recorded residual {residual})"
            );
        }
    }

    #[test]
    fn the_reported_95_percent_circle_really_holds_95_percent_of_the_errors() {
        // The defect this pins: `sigma_white_h` multiplies by the environment scale and
        // `advance_bias` scales the bias innovation by it too, but the reported radius was
        // built from the RAW open-sky bias σ. The circle was therefore right in open sky
        // and too small everywhere else — a nominal 95 % ellipse holding 68 % of the
        // model's own errors, which is a misbehaviour-detection input.
        //
        // Measured empirically rather than argued: one cold-start estimate per node, which
        // is an independent draw from the stationary distribution, with outliers and
        // bursts off so the population is the one the confidence claims to describe.
        let w = world();
        let rng = RngRegistry::new(20260919);
        let gt = Kinematics::at_rest(0, Vec3::new(50.0, -20.0, 3.0));
        for sky in [
            SkyView::OpenSky,
            SkyView::CanyonMitigated,
            SkyView::DeepCanyon,
        ] {
            let mut m = GaussMarkovGnss::new(GaussMarkovParams {
                outlier_rate: 0.0,
                burst_rate: 0.0,
                ..GaussMarkovParams::default()
            });
            let env = GnssEnv {
                sky,
                ..GnssEnv::OPEN_SKY
            };
            let n = 20_000u32;
            let mut inside = 0u32;
            let mut radius = 0.0f64;
            let mut errors: Vec<f64> = Vec::with_capacity(n as usize);
            for id in 0..n {
                let mut ctx = MobilityCtx::new(0, &w, &rng);
                let e = m.estimate(&mut ctx, NodeId::new(id), &gt, &env);
                radius = e.semi_major_m;
                let err = e.pos.distance_2d(gt.pos);
                errors.push(err);
                if err <= e.semi_major_m {
                    inside += 1;
                }
            }
            let coverage = f64::from(inside) / f64::from(n);
            v2xw_core::math::sort_total_order(&mut errors);
            let true_p95 = v2xw_core::math::quantile_sorted(&errors, 0.95);
            println!(
                "{}: reported {radius:.2} m, true p95 {true_p95:.2} m, containment {:.1} %",
                sky.label(),
                100.0 * coverage
            );
            // The Rayleigh 95 % factor is exact for a circular Gaussian, so the only error
            // left is the binomial sampling one: σ = √(0.95·0.05/20000) = 0.0015, and four
            // of those is 0.6 %.
            assert!(
                (coverage - 0.95).abs() < 0.01,
                "{}: a nominal 95 % circle of {radius:.2} m held {:.1} % of the errors \
                 (true p95 {true_p95:.2} m)",
                sky.label(),
                100.0 * coverage
            );
            // And the reported radius is the true 95th percentile, not merely a number
            // that happens to contain the right count.
            assert!(
                (radius - true_p95).abs() < 0.03 * true_p95,
                "{}: reported {radius:.2} m against a true p95 of {true_p95:.2} m",
                sky.label()
            );
        }
    }

    #[test]
    fn a_faulty_receiver_does_not_confess_in_its_confidence() {
        // The other half of the same decision: the environment scale belongs in the
        // reported radius because the receiver knows which environment it is in, and the
        // fault multiplier does not, because it does not know it is faulty. That asymmetry
        // is what makes a sustained residual detectable at all, so it is pinned.
        let w = world();
        let rng = RngRegistry::new(99);
        let mut m = GaussMarkovGnss::default();
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let radius = |m: &mut GaussMarkovGnss, env: GnssEnv, id: u32| {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            m.estimate(&mut ctx, NodeId::new(id), &gt, &env)
                .semi_major_m
        };
        let healthy = radius(&mut m, GnssEnv::OPEN_SKY, 1);
        let faulty = radius(
            &mut m,
            GnssEnv {
                faulty_sensor: true,
                ..GnssEnv::OPEN_SKY
            },
            2,
        );
        assert_eq!(healthy, faulty, "a faulty receiver inflated its own circle");
        // The environment, by contrast, does move it, and by the scale the error moves by.
        for sky in [SkyView::CanyonMitigated, SkyView::DeepCanyon] {
            let there = radius(
                &mut m,
                GnssEnv {
                    sky,
                    ..GnssEnv::OPEN_SKY
                },
                3,
            );
            let scale = crate::gnss::common::env_scale(sky).expect("a scale");
            assert!(
                (there - healthy * scale).abs() < 1e-9,
                "{}: {there} m against {healthy} m × {scale}",
                sky.label()
            );
        }
    }

    #[test]
    fn the_confidence_never_reads_the_realised_bias() {
        // Two nodes with wildly different realised errors report the same confidence,
        // because the confidence is a function of the parameters only. That is the §2.10
        // correction, stated as a property.
        let w = world();
        let rng = RngRegistry::new(8);
        let mut m = GaussMarkovGnss::default();
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let mut radii: Vec<f64> = Vec::new();
        let mut spread: Vec<f64> = Vec::new();
        for id in 0..200u32 {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            let e = m.estimate(&mut ctx, NodeId::new(id), &gt, &GnssEnv::OPEN_SKY);
            radii.push(e.semi_major_m);
            spread.push(e.pos.norm_2d());
        }
        let first = radii[0];
        assert!(
            radii.iter().all(|r| (r - first).abs() < 1e-12),
            "the radius varies"
        );
        let max_error = spread.iter().fold(0.0f64, |a, b| a.max(*b));
        assert!(
            max_error > first,
            "some node's error exceeds the reported radius, as it must"
        );
    }

    #[test]
    fn a_canyon_is_worse_than_open_sky() {
        let w = world();
        let rng = RngRegistry::new(12);
        let mut m = GaussMarkovGnss::new(GaussMarkovParams {
            outlier_rate: 0.0,
            burst_rate: 0.0,
            ..GaussMarkovParams::default()
        });
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let mean_error = |m: &mut GaussMarkovGnss, env: GnssEnv, base: u32| {
            let mut total = 0.0;
            let n = 500;
            for id in 0..n {
                let mut ctx = MobilityCtx::new(0, &w, &rng);
                let e = m.estimate(&mut ctx, NodeId::new(base + id), &gt, &env);
                total += e.pos.norm_2d();
            }
            total / f64::from(n)
        };
        let open = mean_error(&mut m, GnssEnv::OPEN_SKY, 0);
        let canyon = mean_error(
            &mut m,
            GnssEnv {
                sky: SkyView::DeepCanyon,
                ..GnssEnv::OPEN_SKY
            },
            10_000,
        );
        assert!(
            canyon > 5.0 * open,
            "canyon {canyon} m against open sky {open} m"
        );
        // And the reported confidence grows with it, because the environment scale is a
        // parameter the receiver knows.
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let a = m.estimate(&mut ctx, NodeId::new(1), &gt, &GnssEnv::OPEN_SKY);
        let b = m.estimate(
            &mut ctx,
            NodeId::new(2),
            &gt,
            &GnssEnv {
                sky: SkyView::DeepCanyon,
                ..GnssEnv::OPEN_SKY
            },
        );
        assert!(b.semi_major_m > a.semi_major_m);
    }

    #[test]
    fn an_obstructed_or_jammed_receiver_has_no_fix() {
        let w = world();
        let rng = RngRegistry::new(4);
        let mut m = GaussMarkovGnss::default();
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let tunnel = m.estimate(
            &mut ctx,
            NodeId::new(1),
            &gt,
            &GnssEnv {
                sky: SkyView::Obstructed,
                ..GnssEnv::OPEN_SKY
            },
        );
        assert_eq!(tunnel.fix, FixQuality::NoFix);
        assert!(tunnel.semi_major_m.is_infinite());
        let jammed = m.estimate(
            &mut ctx,
            NodeId::new(2),
            &gt,
            &GnssEnv {
                jammed: true,
                ..GnssEnv::OPEN_SKY
            },
        );
        assert_eq!(jammed.fix, FixQuality::NoFix);
    }

    #[test]
    fn weather_scales_the_error() {
        assert_eq!(weather_error_multiplier(WeatherKind::Clear), 1.0);
        assert_eq!(weather_error_multiplier(WeatherKind::Rain), 1.5);
        assert_eq!(weather_error_multiplier(WeatherKind::Fog), 2.0);
        assert_eq!(weather_error_multiplier(WeatherKind::Snow), 2.5);
        let w = world();
        let rng = RngRegistry::new(6);
        let mut m = GaussMarkovGnss::default();
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let clear = m.estimate(&mut ctx, NodeId::new(1), &gt, &GnssEnv::OPEN_SKY);
        let snowy = m.estimate(
            &mut ctx,
            NodeId::new(2),
            &gt,
            &GnssEnv {
                weather: WeatherState {
                    kind: WeatherKind::Snow,
                    intensity: 0.8,
                    visibility_m: 200.0,
                    surface: SurfaceCondition::Snow,
                },
                ..GnssEnv::OPEN_SKY
            },
        );
        assert!(snowy.semi_major_m > clear.semi_major_m);
    }

    #[test]
    fn the_bias_is_correlated_in_time() {
        // Two consecutive estimates a tenth of a second apart are far more alike than two
        // estimates a correlation time apart — the property τ exists for.
        let w = world();
        let rng = RngRegistry::new(1);
        let mut m = GaussMarkovGnss::new(GaussMarkovParams {
            outlier_rate: 0.0,
            burst_rate: 0.0,
            sigma_white_h_m: 0.0, // isolate the bias
            ..GaussMarkovParams::default()
        });
        let gt = Kinematics::at_rest(0, Vec3::ZERO);
        let node = NodeId::new(1);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let a = m.estimate(&mut ctx, node, &gt, &GnssEnv::OPEN_SKY);
        let mut ctx = MobilityCtx::new(NS_PER_S / 10, &w, &rng);
        let b = m.estimate(&mut ctx, node, &gt, &GnssEnv::OPEN_SKY);
        let close = a.pos.distance_2d(b.pos);
        let mut ctx = MobilityCtx::new(NS_PER_S / 10 + 4000 * NS_PER_S, &w, &rng);
        let c = m.estimate(&mut ctx, node, &gt, &GnssEnv::OPEN_SKY);
        let far = b.pos.distance_2d(c.pos);
        assert!(close < far, "0.1 s apart: {close} m; four τ apart: {far} m");
        assert!(close < 0.5, "the bias barely moves in 0.1 s: {close} m");
    }

    #[test]
    fn one_nodes_draws_do_not_depend_on_another_nodes() {
        // Node 7's estimate is the same whether or not other nodes were sampled first,
        // which is what per-entity streams are for (ADR 0004 §3).
        let w = world();
        let gt = Kinematics::at_rest(0, Vec3::new(5.0, 5.0, 0.0));
        let alone = {
            let rng = RngRegistry::new(99);
            let mut m = GaussMarkovGnss::default();
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            m.estimate(&mut ctx, NodeId::new(7), &gt, &GnssEnv::OPEN_SKY)
        };
        let after_others = {
            let rng = RngRegistry::new(99);
            let mut m = GaussMarkovGnss::default();
            for id in [3u32, 1, 9, 2] {
                let mut ctx = MobilityCtx::new(0, &w, &rng);
                m.estimate(&mut ctx, NodeId::new(id), &gt, &GnssEnv::OPEN_SKY);
            }
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            m.estimate(&mut ctx, NodeId::new(7), &gt, &GnssEnv::OPEN_SKY)
        };
        assert_eq!(alone, after_others);
    }

    #[test]
    fn rtk_is_an_order_of_magnitude_better() {
        let sps = GaussMarkovGnss::default();
        let rtk = GaussMarkovGnss::rtk();
        assert!(rtk.fits()[0].sigma_m < sps.fits()[0].sigma_m / 2.0);
        assert_eq!(rtk.outage_p95_s(), RTK_OUTAGE_P95_S);
        assert_eq!(sps.outage_p95_s(), SPS_OUTAGE_P95_S);
    }

    #[test]
    fn the_card_validates_and_carries_the_fit() {
        let m = GaussMarkovGnss::default();
        m.card().validate().expect("validates");
        assert!(
            m.card()
                .parameters
                .iter()
                .any(|p| p.name.starts_with("fit.") && p.name.contains("horizontal")),
            "the fit is on the card"
        );
        for p in &m.card().parameters {
            if p.source.kind == SourceKind::TodoCalibrate {
                assert!(p.calibration.as_ref().is_some_and(|c| !c.trim().is_empty()));
            }
        }
    }
}
