//! Large-scale propagation: `propagation/free-space`, `propagation/two-ray-ground`,
//! `propagation/log-distance-shadowing` and `propagation/tr37885`
//! (04-models.md §3.1-§3.3, §3.6).
//!
//! Every model here returns the full [`LossBreakdown`] so the inspector can show each
//! term, and every constant carries its citation in the model card. The carrier is
//! 5.9 GHz (λ ≈ 0.0508 m); the 3GPP evaluation model uses its own `fc` and the TRs use
//! 6 GHz as a proxy.
//!
//! # The three parts of a loss
//!
//! * **Deterministic distance law.** Friis, the two-ray asymptote, the dual-slope
//!   log-distance law, or one of the TR 37.885 state formulas.
//! * **Shadowing.** A log-normal realisation that is *spatially correlated*: it is an
//!   AR(1) process over distance moved, per link, so a vehicle driving through a shadow
//!   stays in it for a decorrelation distance instead of re-rolling a fresh number every
//!   frame. This is [`ShadowProcess`], and it is the reason
//!   [`crate::traits::Propagation::loss_db`] takes `&mut self`.
//! * **Terms the other families own.** Obstacle shadowing (the [`crate::obstacle`]
//!   models), fast fading ([`crate::fading`]) and the antenna gains. The building term is
//!   an exception: the Sommer formula is deterministic in the geometry the
//!   [`LosResult`] already carries, so a propagation model can include it directly, which
//!   is what 04-models.md §3.5 means by "`ObstacleModel` **+ Propagation term**".
//!
//! # Presets, and the ones that are not here
//!
//! The Abbas 2015 dual-slope LOS and OLOS rows are shipped as defaults with every
//! constant printed in 04-models.md §3.2. The Kunisch and Cheng rows are shipped with the
//! constants the table prints and `todo-calibrate` stand-ins for the ones it marks
//! UNVERIFIED or leaves blank, and they register `validation.status = unvalidated`, which
//! is the registry rule the design document states for them. Two rows are **not** shipped
//! at all, because shipping them would mean inventing their numbers:
//!
//! * `karedal-*` — "numeric grid UNVERIFIED (extraction failed on three copies); **not
//!   shippable until read**" (§3.2). Nothing but the qualitative statement `n < 2` is
//!   available.
//! * `abbas-nlos-intersection` — the table gives the exponent 2.69 and σ 4.1 but no
//!   `PL0`, and the row is a reference to the Mangel NLOS-intersection model, a different
//!   functional form from the dual-slope law implemented here. A building-obstructed link
//!   is served by `propagation/tr37885`'s NLOS formula or by the Sommer building term on
//!   top of a LOS law.
//! * `two-ray-interference` — its ground permittivity ε_r is UNVERIFIED (not in cache).
//!
//! `propagation/winner-plus-b1` is likewise absent: 04-models.md §3.3 records that all of
//! its coefficients are `TODO: calibrate` because WINNER II D1.1.2 is not cached.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::geom::Vec3;
use v2xw_core::ids::LinkKey;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::{Duration, SimTime};
use v2xw_core::weather::{WeatherKind, WeatherState};
use v2xw_world::model::EnvClass;

use crate::numeric;
use crate::traits::Propagation;
use crate::types::{LosResult, LossBreakdown, RadioEndpoint};

/// The reference distance `d0` of the log-distance law, 10 m [Karedal 2011 Eq. 5;
/// Abbas 2015 Eq. 4].
pub const D0_M: f64 = 10.0;

/// The distance below which every model here evaluates its loss at a fixed floor.
///
/// A Friis loss goes to minus infinity as `d → 0`, and a conformance requirement of
/// 03-interfaces.md §17 is that loss is monotone non-decreasing in distance. One metre is
/// the floor: two antennas closer than that are inside each other's near field, where
/// none of these far-field models applies at all, and the loss at 1 m is the largest
/// honest answer available.
pub const D_MIN_M: f64 = 1.0;

// =========================================================================================
// §3.1 Free space and two-ray ground
// =========================================================================================

/// The Friis constant in the kilometre/megahertz form of the free-space law,
/// `180 + 20·log10(4π/c)` with `c` the defined speed of light: 32.447783 dB.
///
/// 04-models.md §3.1 and most textbooks print the rounded 32.44, which is 0.007783 dB
/// optimistic on *every* free-space loss the crate computes — nearly eight times the
/// 1e-3 dB quantum recorded decibels are rounded to ([`crate::numeric::Q_DB`]), so the
/// rounding survives quantisation and shows up in a recorded breakdown. It also makes
/// [`two_ray_ground_loss_db`] discontinuous at its crossover, because the `d⁻⁴` branch is
/// derived exactly while the Friis branch is not. The exact value is used here and
/// `friis_is_monotone_and_matches_a_hand_computation` pins it against
/// `20·log10(4π·d·f/c)` evaluated directly in metres and hertz.
pub const FRIIS_CONST_DB: f64 = 32.447_783_221_883_356;

/// Friis free-space path loss, dB:
/// `20·log10(d_km) + 20·log10(f_MHz) + 32.447783` [Sommer 2011 Eq. 1-2,
/// 04-models.md §3.1, with the constant carried to full precision — see
/// [`FRIIS_CONST_DB`]].
///
/// Equivalent to `P_r = P_t·G_t·G_r·(λ/4πd)²` with the gains handled separately. `d_m`
/// below [`D_MIN_M`] is evaluated at [`D_MIN_M`].
#[must_use]
pub fn friis_loss_db(d_m: f64, f_hz: f64) -> f64 {
    let d_km = d_m.max(D_MIN_M) / 1_000.0;
    let f_mhz = f_hz / 1e6;
    20.0 * math::log10(d_km) + 20.0 * math::log10(f_mhz) + FRIIS_CONST_DB
}

/// The two-ray crossover distance `d_c = 4π·h_t·h_r/λ`
/// [ns-3 `TwoRayGroundPropagationLossModel`, 04-models.md §3.1].
#[must_use]
pub fn crossover_distance_m(h_t_m: f64, h_r_m: f64, lambda_m: f64) -> f64 {
    4.0 * core::f64::consts::PI * h_t_m * h_r_m / lambda_m
}

/// The Fresnel-corrected breakpoint `d_b = (4·h_t·h_r − λ²/4)/λ`, the variant
/// 04-models.md §3.1 records alongside the crossover distance.
///
/// It gives 161 m for `h = 1.47 m` at 5.6 GHz, which is the figure the design document
/// quotes, while Abbas used 104 m to fit the measured data — which is why the dual-slope
/// presets carry 104 m as a *fitted* breakpoint rather than a computed one.
#[must_use]
pub fn fresnel_breakpoint_m(h_t_m: f64, h_r_m: f64, lambda_m: f64) -> f64 {
    (4.0 * h_t_m * h_r_m - lambda_m * lambda_m / 4.0) / lambda_m
}

/// Two-ray ground-reflection path loss, dB.
///
/// Below the crossover distance the Friis loss; above it the `d⁻⁴` asymptote
/// `L[dB] = 10·log10(d⁴·L_sys/(h_t²·h_r²))` [ns-3, 04-models.md §3.1]. `l_sys` is the
/// system loss, 1.0 in ns-3's model.
#[must_use]
pub fn two_ray_ground_loss_db(d_m: f64, h_t_m: f64, h_r_m: f64, f_hz: f64, l_sys: f64) -> f64 {
    let d = d_m.max(D_MIN_M);
    let lambda = numeric::wavelength_m(f_hz);
    let d_c = crossover_distance_m(h_t_m, h_r_m, lambda);
    if d <= d_c || h_t_m <= 0.0 || h_r_m <= 0.0 {
        friis_loss_db(d, f_hz)
    } else {
        let numerator = math::pow(d, 4.0) * l_sys;
        let denominator = h_t_m * h_t_m * h_r_m * h_r_m;
        10.0 * math::log10(numerator / denominator)
    }
}

// =========================================================================================
// §3.2 Dual-slope log-distance law and its presets
// =========================================================================================

/// The dual-slope log-distance parameters of one environment preset (04-models.md §3.2).
///
/// `PL(d) = PL0 + 10·n1·log10(d/d0)` up to the breakpoint, then
/// `PL(d_b) + 10·n2·log10(d/d_b)` above it. A preset with no fitted near slope (the
/// Abbas highway OLOS row, "not modeled: too few short-range samples") sets `n1` to
/// `None`, and the model then uses `n2` over the whole range, which is what a single
/// fitted slope means.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DualSlope {
    /// Path-loss exponent below the breakpoint. `None` when the source fitted only one
    /// slope.
    pub n1: Option<f64>,
    /// Path-loss exponent above the breakpoint (and everywhere, when `n1` is `None`).
    pub n2: f64,
    /// Path loss at `d0` = 10 m, dB.
    pub pl0_db: f64,
    /// Shadowing standard deviation, dB.
    pub sigma_db: f64,
    /// The fitted breakpoint, metres.
    pub d_b_m: f64,
}

impl DualSlope {
    /// The deterministic path loss at `d_m`, dB (no shadowing).
    #[must_use]
    pub fn path_loss_db(&self, d_m: f64) -> f64 {
        let d = d_m.max(D_MIN_M);
        match self.n1 {
            None => self.pl0_db + 10.0 * self.n2 * math::log10(d / D0_M),
            Some(n1) => {
                if d <= self.d_b_m {
                    self.pl0_db + 10.0 * n1 * math::log10(d / D0_M)
                } else {
                    let at_break = self.pl0_db + 10.0 * n1 * math::log10(self.d_b_m / D0_M);
                    at_break + 10.0 * self.n2 * math::log10(d / self.d_b_m)
                }
            }
        }
    }
}

/// An environment preset of `propagation/log-distance-shadowing` (04-models.md §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogDistancePreset {
    /// Abbas 2015 highway LOS: n1 1.66, n2 2.88, PL0 66.1 dB, σ 3.95 dB, d_b 104 m.
    /// VERIFIED.
    AbbasLosHighway,
    /// Abbas 2015 urban LOS: n1 1.81, n2 2.85, PL0 63.9 dB, σ 4.15 dB, d_b 104 m.
    /// VERIFIED.
    AbbasLosUrban,
    /// Abbas 2015 highway OLOS (vehicle obstructed): far slope 3.18, PL0 76.1 dB,
    /// σ 6.12 dB, all VERIFIED; the near slope is not printed ("too few short-range
    /// samples") and is borrowed from `abbas-los-highway`, which makes the preset
    /// `unvalidated` (see [`LogDistancePreset::near_slope_is_borrowed`]).
    AbbasOlosHighway,
    /// Abbas 2015 urban OLOS: n1 1.93, n2 2.74, PL0 72.3 dB, σ 6.67 dB, d_b 104 m.
    /// VERIFIED.
    AbbasOlosUrban,
    /// Kunisch and Pamp highway LOS as quoted by Karedal 2011: n 1.85, σ 3.2 dB.
    /// Secondary; PL0 is not printed and is `todo-calibrate`.
    KunischHighway,
    /// Kunisch and Pamp urban LOS: n 1.61, σ 3.4 dB. Same caveat.
    KunischUrban,
    /// Cheng 2007 highway LOS: n1 1.9, d_b 220 m; n2 and σ UNVERIFIED.
    ChengHighway,
    /// Cheng 2007 suburban A: n1 in 2.0-2.1, d_b 100 m; n2 and σ UNVERIFIED.
    ChengSuburbanA,
    /// Cheng 2007 suburban B: n1 2.3, d_b 226 m; n2 and σ UNVERIFIED.
    ChengSuburbanB,
}

impl LogDistancePreset {
    /// Every shipped preset, in a fixed order.
    pub const ALL: [LogDistancePreset; 9] = [
        LogDistancePreset::AbbasLosHighway,
        LogDistancePreset::AbbasLosUrban,
        LogDistancePreset::AbbasOlosHighway,
        LogDistancePreset::AbbasOlosUrban,
        LogDistancePreset::KunischHighway,
        LogDistancePreset::KunischUrban,
        LogDistancePreset::ChengHighway,
        LogDistancePreset::ChengSuburbanA,
        LogDistancePreset::ChengSuburbanB,
    ];

    /// The preset's id as a scenario spells it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            LogDistancePreset::AbbasLosHighway => "abbas-los-highway",
            LogDistancePreset::AbbasLosUrban => "abbas-los-urban",
            LogDistancePreset::AbbasOlosHighway => "abbas-olos-highway",
            LogDistancePreset::AbbasOlosUrban => "abbas-olos-urban",
            LogDistancePreset::KunischHighway => "kunisch-highway",
            LogDistancePreset::KunischUrban => "kunisch-urban",
            LogDistancePreset::ChengHighway => "cheng-highway",
            LogDistancePreset::ChengSuburbanA => "cheng-suburban-a",
            LogDistancePreset::ChengSuburbanB => "cheng-suburban-b",
        }
    }

    /// The interim `PL0` for a preset whose source does not print one: the Friis loss at
    /// `d0` = 10 m at 5.9 GHz, 67.86 dB.
    ///
    /// This is the standard anchor for a single-slope log-distance law — the law reduces
    /// to free space at the reference distance — and it is a *derivation*, recorded as
    /// `todo-calibrate` on every preset that uses it, not a fitted value.
    #[must_use]
    pub fn friis_anchor_pl0_db() -> f64 {
        friis_loss_db(D0_M, 5.9e9)
    }

    /// The preset's parameters.
    #[must_use]
    pub fn params(self) -> DualSlope {
        let anchor = Self::friis_anchor_pl0_db();
        match self {
            LogDistancePreset::AbbasLosHighway => DualSlope {
                n1: Some(1.66),
                n2: 2.88,
                pl0_db: 66.1,
                sigma_db: 3.95,
                d_b_m: 104.0,
            },
            LogDistancePreset::AbbasLosUrban => DualSlope {
                n1: Some(1.81),
                n2: 2.85,
                pl0_db: 63.9,
                sigma_db: 4.15,
                d_b_m: 104.0,
            },
            LogDistancePreset::AbbasOlosHighway => DualSlope {
                // Abbas Table II prints no near slope for this row — "not modeled: too
                // few short-range samples" — so there is nothing to fit below the
                // breakpoint. Using the fitted far slope n2 = 3.18 from d0 = 10 m
                // instead, which is what a single-slope reading does, extrapolates the
                // fit into exactly the range the paper says it has no samples for: it
                // drives the LOS-to-OLOS offset to 25.20 dB at 100 m against the 8.6-10 dB
                // Abbas measured. The near slope is therefore BORROWED from the LOS row of
                // the same environment (abbas-los-highway, n1 = 1.66), which makes the
                // OLOS curve parallel to the LOS curve below the breakpoint and puts the
                // offset at exactly PL0_OLOS − PL0_LOS = 10.0 dB there, and confines
                // n2 = 3.18 to the range it was fitted on. The borrowed value is not a
                // printed constant, so the preset registers `unvalidated` and the card
                // carries the limitation and the calibration plan
                // (`the_olos_highway_offset_stays_near_the_measured_range`).
                n1: Some(1.66),
                n2: 3.18,
                pl0_db: 76.1,
                sigma_db: 6.12,
                d_b_m: 104.0,
            },
            LogDistancePreset::AbbasOlosUrban => DualSlope {
                n1: Some(1.93),
                n2: 2.74,
                pl0_db: 72.3,
                sigma_db: 6.67,
                d_b_m: 104.0,
            },
            LogDistancePreset::KunischHighway => DualSlope {
                n1: None,
                n2: 1.85,
                pl0_db: anchor,
                sigma_db: 3.2,
                d_b_m: f64::INFINITY,
            },
            LogDistancePreset::KunischUrban => DualSlope {
                n1: None,
                n2: 1.61,
                pl0_db: anchor,
                sigma_db: 3.4,
                d_b_m: f64::INFINITY,
            },
            LogDistancePreset::ChengHighway => DualSlope {
                n1: Some(1.9),
                // n2 UNVERIFIED: until it is read, the far slope equals the near one,
                // which is the same as saying the fit is single-slope.
                n2: 1.9,
                pl0_db: anchor,
                // σ UNVERIFIED: no shadowing is applied, and the card says so.
                sigma_db: 0.0,
                d_b_m: 220.0,
            },
            LogDistancePreset::ChengSuburbanA => DualSlope {
                // The cited range is 2.0-2.1; the midpoint is the interim default.
                n1: Some(2.05),
                n2: 2.05,
                pl0_db: anchor,
                sigma_db: 0.0,
                d_b_m: 100.0,
            },
            LogDistancePreset::ChengSuburbanB => DualSlope {
                n1: Some(2.3),
                n2: 2.3,
                pl0_db: anchor,
                sigma_db: 0.0,
                d_b_m: 226.0,
            },
        }
    }

    /// True when this preset's near slope is borrowed from another row rather than
    /// printed for its own.
    ///
    /// Only `abbas-olos-highway`: Abbas Table II does not model `n1` for highway OLOS,
    /// and this crate borrows the LOS row's 1.66 rather than extrapolating the fitted far
    /// slope into a range the paper says it has no samples for. A borrowed constant is
    /// not a cited one, so the preset registers `unvalidated` under the registry rule of
    /// 04-models.md §3.2 even though every *printed* constant of the row is verified.
    #[must_use]
    pub const fn near_slope_is_borrowed(self) -> bool {
        matches!(self, LogDistancePreset::AbbasOlosHighway)
    }

    /// True when every constant of this preset is printed in the design document's table.
    ///
    /// A preset for which this is false registers `validation.status = unvalidated`, which
    /// is the registry rule of 04-models.md §3.2. A preset for which this is true but
    /// [`LogDistancePreset::near_slope_is_borrowed`] is also true registers unvalidated
    /// as well: its printed constants are all cited, but one constant it uses is not
    /// printed at all.
    #[must_use]
    pub const fn is_fully_cited(self) -> bool {
        matches!(
            self,
            LogDistancePreset::AbbasLosHighway
                | LogDistancePreset::AbbasLosUrban
                | LogDistancePreset::AbbasOlosHighway
                | LogDistancePreset::AbbasOlosUrban
        )
    }

    /// The preset this environment and link state select by default.
    ///
    /// Only the Abbas rows are fully cited, so they are the ones a default run uses.
    /// `Suburban` maps to the urban rows and `Rural` to the highway rows, because those
    /// are the nearest measured environments in the cited table; the card records the
    /// mapping as a design choice.
    #[must_use]
    pub const fn for_environment(env: EnvClass, obstructed_by_vehicle: bool) -> LogDistancePreset {
        match (env, obstructed_by_vehicle) {
            (EnvClass::Urban | EnvClass::Suburban, false) => LogDistancePreset::AbbasLosUrban,
            (EnvClass::Urban | EnvClass::Suburban, true) => LogDistancePreset::AbbasOlosUrban,
            (EnvClass::Highway | EnvClass::Rural, false) => LogDistancePreset::AbbasLosHighway,
            (EnvClass::Highway | EnvClass::Rural, true) => LogDistancePreset::AbbasOlosHighway,
        }
    }
}

/// The decorrelation distance `D_corr` for spatially correlated shadowing, metres
/// [TR 36.885 Annex A.1.4 as read in R2c, 04-models.md §3.2]: 10 m urban, 25 m freeway
/// for V2V, 50 m for the infrastructure link.
#[must_use]
pub const fn decorrelation_distance_m(env: EnvClass) -> f64 {
    match env {
        EnvClass::Urban | EnvClass::Suburban => 10.0,
        EnvClass::Highway | EnvClass::Rural => 25.0,
    }
}

/// The decorrelation distance of an eNB-UE link, 50 m [TR 36.885, 04-models.md §3.2].
pub const DECORRELATION_INFRASTRUCTURE_M: f64 = 50.0;

/// The AR(1) shadowing process of one link (04-models.md §3.2).
///
/// `S(n) = exp(−D/D_corr)·S(n−1) + sqrt(1 − exp(−2·D/D_corr))·N(n)` where `D` is the
/// distance moved since the last update and `N(n) ~ N(0, σ²)`. The coefficients are the
/// ones that make the process stationary with standard deviation σ and autocorrelation
/// `exp(−D/D_corr)` at lag `D`, which is what
/// `shadowing_autocorrelation_matches_the_ar1_coefficient` measures.
///
/// `D` for a V2V link: TR 36.885 Annex A.1.4 defines the update for a moving UE against a
/// fixed eNB, and on a V2V link both ends move. The engine uses the **sum of the two
/// endpoint displacements**, which reduces to the TR's own rule when one end is fixed and
/// decorrelates a link twice as fast when both ends drive apart. The card records it as a
/// design choice with a plan to check the TR's V2V annex.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ShadowProcess {
    /// The current realisation, dB.
    pub s_db: f64,
    /// Where the transmitter was at the last update.
    pub last_tx: Vec3,
    /// Where the receiver was at the last update.
    pub last_rx: Vec3,
    /// False until the first draw has initialised `s_db` from the stationary
    /// distribution.
    pub initialised: bool,
}

impl ShadowProcess {
    /// A process that has not drawn yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            s_db: 0.0,
            last_tx: Vec3::ZERO,
            last_rx: Vec3::ZERO,
            initialised: false,
        }
    }

    /// The AR(1) coefficient `ρ = exp(−D/D_corr)` for a displacement of `moved_m`.
    #[must_use]
    pub fn rho(moved_m: f64, d_corr_m: f64) -> f64 {
        if d_corr_m <= 0.0 {
            return 0.0;
        }
        math::exp(-moved_m.max(0.0) / d_corr_m)
    }

    /// Advances the process to the new endpoint positions and returns the shadowing in
    /// dB.
    ///
    /// `draw` is a standard normal sample (the caller takes it from the `Shadow` stream
    /// of this link, so the value does not depend on what any other model drew).
    pub fn update(&mut self, tx: Vec3, rx: Vec3, sigma_db: f64, d_corr_m: f64, draw: f64) -> f64 {
        if sigma_db <= 0.0 {
            self.s_db = 0.0;
            self.last_tx = tx;
            self.last_rx = rx;
            self.initialised = true;
            return 0.0;
        }
        if !self.initialised {
            // The first value is drawn from the stationary distribution, so a link that
            // is only ever evaluated once still sees a correctly distributed shadowing.
            self.s_db = sigma_db * draw;
            self.last_tx = tx;
            self.last_rx = rx;
            self.initialised = true;
            return self.s_db;
        }
        let moved = self.last_tx.distance(tx) + self.last_rx.distance(rx);
        let rho = Self::rho(moved, d_corr_m);
        let innovation = math::sqrt((1.0 - rho * rho).max(0.0)) * sigma_db * draw;
        self.s_db = rho * self.s_db + innovation;
        self.last_tx = tx;
        self.last_rx = rx;
        self.s_db
    }
}

// =========================================================================================
// §3.5 The Sommer building term, as a Propagation term
// =========================================================================================

/// The Sommer 2011 building-shadowing coefficients: `L_obs = β·n + γ·d_m`
/// (04-models.md §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SommerCoefficients {
    /// dB per exterior wall crossed.
    pub beta_db_per_wall: f64,
    /// dB per metre of in-building path.
    pub gamma_db_per_m: f64,
}

impl SommerCoefficients {
    /// The default row — the majority of the fitted dataset: β 9 dB per wall, γ 0.4 dB/m
    /// [Sommer 2011, 04-models.md §3.5].
    pub const DEFAULT: SommerCoefficients = SommerCoefficients {
        beta_db_per_wall: 9.0,
        gamma_db_per_m: 0.4,
    };

    /// The loss for `walls` exterior walls and `len_m` metres inside buildings, dB.
    #[must_use]
    pub fn loss_db(&self, walls: u16, len_m: f64) -> f64 {
        self.beta_db_per_wall * f64::from(walls) + self.gamma_db_per_m * len_m.max(0.0)
    }
}

// =========================================================================================
// §3.6 Weather attenuation
// =========================================================================================

/// Which polarisation the ITU-R P.838-3 rain coefficients are taken for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Polarization {
    /// Horizontal: k 0.0007056, α 1.5900 at 6 GHz.
    Horizontal,
    /// Vertical: k 0.0004878, α 1.5728 at 6 GHz. The default, because an ITS-G5 antenna
    /// is vertically polarised.
    Vertical,
}

impl Polarization {
    /// The `(k, α)` pair of the 6 GHz row, the closest to 5.9 GHz
    /// [ITU-R P.838-3 Table 5, 04-models.md §3.6].
    #[must_use]
    pub const fn rain_coefficients_6ghz(self) -> (f64, f64) {
        match self {
            Polarization::Horizontal => (0.000_705_6, 1.590_0),
            Polarization::Vertical => (0.000_487_8, 1.572_8),
        }
    }
}

/// Specific rain attenuation `γ_R = k·R^α`, dB/km [ITU-R P.838-3, 04-models.md §3.6].
#[must_use]
pub fn rain_specific_attenuation_db_km(rain_mm_h: f64, pol: Polarization) -> f64 {
    if rain_mm_h <= 0.0 {
        return 0.0;
    }
    let (k, alpha) = pol.rain_coefficients_6ghz();
    k * math::pow(rain_mm_h, alpha)
}

/// The weather attenuation term of a link, dB.
///
/// Rain only: fog and gaseous absorption are UNVERIFIED at 5.9 GHz (the P.840-8 liquid
/// specific attenuation `K_l(5.9 GHz)` was not recovered and the P.676-12 value was not
/// computed), and the design document's conclusion is that all three are negligible at
/// this carrier — a fraction of a dB in a 50 mm/h downpour over 300 m, against the 6-10 dB
/// a single obstructing truck costs. The term is kept so the breakdown is honest and so a
/// future mmWave RAT can reuse it (04-models.md §3.6).
///
/// `rain_rate_max_mm_h` maps [`WeatherState::intensity`] onto a rain rate: the design
/// document's own worked example is a 50 mm/h downpour, and nothing cited maps the
/// abstract intensity onto mm/h, so the mapping is linear in intensity and the parameter
/// is `todo-calibrate`.
#[must_use]
pub fn weather_attenuation_db(
    w: &WeatherState,
    d_m: f64,
    pol: Polarization,
    rain_rate_max_mm_h: f64,
) -> f64 {
    let rain_mm_h = match w.kind {
        WeatherKind::Rain | WeatherKind::Sleet => w.intensity * rain_rate_max_mm_h,
        // Snow, fog, wind and clear contribute nothing this model can cite.
        _ => 0.0,
    };
    rain_specific_attenuation_db_km(rain_mm_h, pol) * (d_m / 1_000.0)
}

// =========================================================================================
// `propagation/free-space`
// =========================================================================================

/// `propagation/free-space` — Friis, the LOS floor at every tier.
#[derive(Debug, Clone)]
pub struct FreeSpace {
    card: ModelCard,
    tier: Tier,
}

impl FreeSpace {
    /// The model's id.
    pub const ID: &'static str = "propagation/free-space";

    /// The model at `tier`.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self {
            card: free_space_card(),
            tier,
        }
    }
}

impl Default for FreeSpace {
    fn default() -> Self {
        Self::new(Tier::Medium)
    }
}

impl Model for FreeSpace {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Propagation<C> for FreeSpace {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn loss_db(
        &mut self,
        _ctx: &mut C,
        tx: &RadioEndpoint,
        rx: &RadioEndpoint,
        f_hz: f64,
        _los: &LosResult,
        _w: &WeatherState,
    ) -> LossBreakdown {
        let d = tx.pos.distance(rx.pos);
        LossBreakdown::new(
            friis_loss_db(d, f_hz),
            0.0,
            0.0,
            0.0,
            tx.gain_dbi + rx.gain_dbi,
        )
    }
}

fn free_space_card() -> ModelCard {
    let sommer = Source::new(
        SourceKind::Paper,
        "C. Sommer et al. 2011 Eq. 1-2 (R3 §A.1), via 04-models.md §3.1",
    );
    let mut card = ModelCard::new(
        FreeSpace::ID,
        Family::Propagation,
        "1.0.0",
        "Friis free-space path loss: the line-of-sight floor every tier uses.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![Equation {
        name: "path loss".to_string(),
        latex_or_text: "L_fs[dB] = 20·log10(d_km) + 20·log10(f_MHz) + 32.44".to_string(),
        notes: Some("Equivalently P_r = P_t·G_t·G_r·(λ/4πd)².".to_string()),
    }];
    card.parameters = vec![Parameter {
        name: "d_min_m".to_string(),
        unit: "m".to_string(),
        default: serde_json::json!(D_MIN_M),
        range: Some(vec![serde_json::json!(0.1), serde_json::json!(10.0)]),
        source: Source {
            kind: SourceKind::TodoCalibrate,
            reference: "near-field floor, chosen so that loss is monotone non-decreasing \
                        in distance (03-interfaces.md §17)"
                .to_string(),
            accessed: None,
            note: Some(
                "No cited near-field bound exists for a 5.9 GHz vehicle antenna; 1 m is \
                 well outside the λ/2π = 8 mm reactive region and inside every measured \
                 dataset's shortest link."
                    .to_string(),
            ),
        },
        calibration: Some(
            "Replace with the far-field distance of the shipped antenna once a pattern is \
             digitized (the plan 04-models.md §3.7 records for the pattern model)."
                .to_string(),
        ),
    }];
    card.assumptions = vec![
        "Isotropic radiators in an unobstructed medium with no ground reflection.".to_string(),
    ];
    card.limitations = vec![
        "Optimistic beyond the two-ray crossover distance, where the ground reflection \
         turns the law into d^-4."
            .to_string(),
    ];
    card.sources = vec![sommer];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec!["friis_is_monotone_and_matches_a_hand_computation".to_string()],
    };
    card
}

// =========================================================================================
// `propagation/two-ray-ground`
// =========================================================================================

/// `propagation/two-ray-ground` — Friis below the crossover distance, the `d⁻⁴`
/// asymptote above it.
#[derive(Debug, Clone)]
pub struct TwoRayGround {
    card: ModelCard,
    tier: Tier,
    l_sys: f64,
}

impl TwoRayGround {
    /// The model's id.
    pub const ID: &'static str = "propagation/two-ray-ground";

    /// The model at `tier` with ns-3's system loss of 1.0.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self {
            card: two_ray_card(),
            tier,
            l_sys: 1.0,
        }
    }

    /// The crossover distance for two antenna heights at `f_hz`.
    #[must_use]
    pub fn crossover_m(&self, h_t_m: f64, h_r_m: f64, f_hz: f64) -> f64 {
        crossover_distance_m(h_t_m, h_r_m, numeric::wavelength_m(f_hz))
    }
}

impl Default for TwoRayGround {
    fn default() -> Self {
        Self::new(Tier::Medium)
    }
}

impl Model for TwoRayGround {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Propagation<C> for TwoRayGround {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn loss_db(
        &mut self,
        _ctx: &mut C,
        tx: &RadioEndpoint,
        rx: &RadioEndpoint,
        f_hz: f64,
        _los: &LosResult,
        _w: &WeatherState,
    ) -> LossBreakdown {
        let d = tx.pos.distance(rx.pos);
        let path = two_ray_ground_loss_db(d, tx.height_m(), rx.height_m(), f_hz, self.l_sys);
        LossBreakdown::new(path, 0.0, 0.0, 0.0, tx.gain_dbi + rx.gain_dbi)
    }
}

fn two_ray_card() -> ModelCard {
    let ns3 = Source {
        kind: SourceKind::Code,
        reference: "ns-3 TwoRayGroundPropagationLossModel (R3 §A.1, crossover at line 406), \
                    via 04-models.md §3.1"
            .to_string(),
        accessed: None,
        note: Some(
            "Karedal 2011 Eq. 3 found the two-ray model valid in rural settings for \
             d >= 20 m."
                .to_string(),
        ),
    };
    let mut card = ModelCard::new(
        TwoRayGround::ID,
        Family::Propagation,
        "1.0.0",
        "Two-ray ground-reflection path loss: Friis below the crossover distance, the \
         d^-4 asymptote above it.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new("crossover distance", "d_c = 4π·h_t·h_r/λ"),
        Equation {
            name: "path loss above d_c".to_string(),
            latex_or_text: "L[dB] = 10·log10(d⁴·L_sys/(h_t²·h_r²))".to_string(),
            notes: Some(
                "Below d_c the Friis loss. The Fresnel-corrected breakpoint variant \
                 d_b = (4·h_t·h_r − λ²/4)/λ is available as fresnel_breakpoint_m; Abbas \
                 used 104 m rather than the computed 161 m to fit measured data, which is \
                 why the dual-slope presets carry a fitted breakpoint."
                    .to_string(),
            ),
        },
    ];
    card.parameters = vec![
        Parameter::new("l_sys", "-", serde_json::json!(1.0), ns3.clone()),
        Parameter {
            name: "h_tx_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(1.5),
            range: Some(vec![serde_json::json!(0.1), serde_json::json!(15.0)]),
            source: Source {
                kind: SourceKind::Standard,
                reference: "3GPP TR 36.885 (vehicle antenna 1.5 m), 04-models.md §3.1".to_string(),
                accessed: None,
                note: Some(
                    "Veins defaults antennaOffsetZ to 1.895 m; the endpoint's own z is \
                     used when the caller supplies one."
                        .to_string(),
                ),
            },
            calibration: None,
        },
        Parameter {
            name: "h_rx_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(1.5),
            range: Some(vec![serde_json::json!(0.1), serde_json::json!(15.0)]),
            source: Source::new(
                SourceKind::Standard,
                "3GPP TR 36.885 (vehicle 1.5 m) and TR 36.885 §F.3 (RSU 5 m), \
                 04-models.md §3.1",
            ),
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "A flat, perfectly reflecting ground plane between the two antennas.".to_string(),
        "Both antenna heights are above zero; a zero height falls back to Friis.".to_string(),
    ];
    card.limitations = vec![
        "No phase-accurate interference pattern: the Veins two-ray interference variant \
         with a reflection coefficient is not shipped, because its default ground \
         permittivity is UNVERIFIED (04-models.md §3.1)."
            .to_string(),
    ];
    card.ignores = vec![
        "Shadowing, obstacles, fading and weather: this is a deterministic distance law."
            .to_string(),
    ];
    card.sources = vec![ns3];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "Abbas 2015 (R3 §A.1): the Fresnel breakpoint is 161 m for h = 1.47 m at \
             5.6 GHz, and 104 m was the fitted value",
        )],
        tests: vec!["the_two_ray_crossover_and_fresnel_breakpoint_match_the_document".to_string()],
    };
    card
}

// =========================================================================================
// `propagation/log-distance-shadowing`
// =========================================================================================

/// `propagation/log-distance-shadowing` — the dual-slope log-distance law with spatially
/// correlated log-normal shadowing (04-models.md §3.2).
///
/// State: one [`ShadowProcess`] per [`LinkKey`], in a `BTreeMap` so that iteration (a
/// debug dump, a state digest) can never depend on a hash order.
///
/// # The building term is off by default
///
/// 04-models.md §3.5 registers `obstacle/building/sommer-2011` as *both* an
/// [`crate::traits::ObstacleModel`] and a `Propagation` term, and every tier of the §3
/// tier table composes it as the obstacle model. A model that also applied it inside
/// `loss_db` would be counted twice by [`crate::budget::evaluate`], which adds the
/// obstacle stack's loss to the propagation breakdown's own `obstacle_db` — 34 dB of
/// extra attenuation on a two-wall, 40 m-in-building link. **The obstacle stack owns
/// obstacle loss.** This model ships with the term off and
/// [`LogDistanceShadowing::with_building_term`] is the explicit opt-in for the standalone
/// case (no [`crate::obstacle::BuildingShadowing`] in the stack).
#[derive(Debug, Clone)]
pub struct LogDistanceShadowing {
    card: ModelCard,
    tier: Tier,
    preset: LogDistancePreset,
    /// When set, the preset is chosen per call from the environment and the link state
    /// rather than fixed.
    auto_preset: bool,
    env: EnvClass,
    d_corr_m: f64,
    sommer: Option<SommerCoefficients>,
    polarization: Polarization,
    rain_rate_max_mm_h: f64,
    shadows: BTreeMap<(u32, u32), ShadowProcess>,
}

impl LogDistanceShadowing {
    /// The model's id.
    pub const ID: &'static str = "propagation/log-distance-shadowing";

    /// The model with one fixed preset.
    #[must_use]
    pub fn new(tier: Tier, preset: LogDistancePreset, env: EnvClass) -> Self {
        Self {
            card: log_distance_card(preset, false, false),
            tier,
            preset,
            auto_preset: false,
            env,
            d_corr_m: decorrelation_distance_m(env),
            // Off by default: obstacle loss belongs to the ObstacleModel stack, which is
            // what every tier of 04-models.md §3 composes. See the type's own
            // documentation and [`crate::budget::evaluate`].
            sommer: None,
            polarization: Polarization::Vertical,
            rain_rate_max_mm_h: 50.0,
            shadows: BTreeMap::new(),
        }
    }

    /// The model choosing its preset per link from the environment class and whether a
    /// vehicle obstructs the link — the shape a scenario uses when its world spans
    /// several environments.
    #[must_use]
    pub fn auto(tier: Tier, env: EnvClass) -> Self {
        let mut m = Self::new(tier, LogDistancePreset::for_environment(env, false), env);
        m.auto_preset = true;
        m.card = log_distance_card(m.preset, true, m.sommer.is_some());
        m
    }

    /// Overrides the decorrelation distance (the infrastructure link's 50 m, say).
    #[must_use]
    pub fn with_decorrelation_m(mut self, d_corr_m: f64) -> Self {
        self.d_corr_m = d_corr_m;
        self
    }

    /// Turns the Sommer building term **on** inside this propagation model.
    ///
    /// Off by default, and this is the only way to switch it on. Exactly one path may own
    /// the obstacle term (see the type's documentation): use this **only** when the model
    /// is evaluated without a [`crate::obstacle::BuildingShadowing`] in the obstacle
    /// stack — a standalone `loss_db` call, a unit test of the law itself, or a study
    /// that deliberately has no obstacle models at all. Composing it with the stack
    /// double-counts every wall, and [`crate::budget::evaluate`] asserts against that in
    /// a debug build.
    #[must_use]
    pub fn with_building_term(mut self) -> Self {
        self.sommer = Some(SommerCoefficients::DEFAULT);
        self.card = log_distance_card(self.preset, self.auto_preset, true);
        self
    }

    /// The same, with caller-chosen Sommer coefficients (a building class other than the
    /// default row of 04-models.md §3.5).
    #[must_use]
    pub fn with_building_coefficients(mut self, c: SommerCoefficients) -> Self {
        self.sommer = Some(c);
        self.card = log_distance_card(self.preset, self.auto_preset, true);
        self
    }

    /// Whether this instance carries its own building term.
    ///
    /// [`crate::budget::evaluate`] does not read this — it reads the `obstacle_db` the
    /// model actually returned — but an engine composing a stack can.
    #[must_use]
    pub const fn has_building_term(&self) -> bool {
        self.sommer.is_some()
    }

    /// The preset this instance uses (the fixed one, or the environment default).
    #[must_use]
    pub const fn preset(&self) -> LogDistancePreset {
        self.preset
    }

    /// The decorrelation distance in use, metres.
    #[must_use]
    pub const fn decorrelation_m(&self) -> f64 {
        self.d_corr_m
    }

    /// The shadowing realisation currently held for a link, dB, if the link has been
    /// evaluated.
    #[must_use]
    pub fn shadow_db(&self, link: LinkKey) -> Option<f64> {
        self.shadows
            .get(&(link.tx().index(), link.rx().index()))
            .filter(|s| s.initialised)
            .map(|s| s.s_db)
    }

    /// The parameters in force for one call.
    fn params_for(&self, los: &LosResult) -> DualSlope {
        if self.auto_preset {
            LogDistancePreset::for_environment(self.env, los.class.has_vehicle()).params()
        } else {
            self.preset.params()
        }
    }
}

impl Model for LogDistanceShadowing {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Propagation<C> for LogDistanceShadowing {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn loss_db(
        &mut self,
        ctx: &mut C,
        tx: &RadioEndpoint,
        rx: &RadioEndpoint,
        // The dual-slope presets are measured fits for the 5.9 GHz band, so the carrier
        // is not a parameter of the law: a run at another frequency needs another fit,
        // which the card records as an assumption rather than silently scaling.
        _f_hz: f64,
        los: &LosResult,
        w: &WeatherState,
    ) -> LossBreakdown {
        let d = tx.pos.distance(rx.pos);
        let params = self.params_for(los);
        let path = params.path_loss_db(d);

        // One normal draw from this link's Shadow stream. The key embeds the link, so the
        // value cannot depend on what any other link or model drew.
        let link = LinkKey(tx.node, rx.node);
        let draw = if params.sigma_db > 0.0 {
            ctx.rng(RngDomain::Shadow, EntityRef::Link(link))
                .normal(0.0, 1.0)
        } else {
            0.0
        };
        let process = self
            .shadows
            .entry((tx.node.index(), rx.node.index()))
            .or_default();
        let shadow = process.update(tx.pos, rx.pos, params.sigma_db, self.d_corr_m, draw);

        let obstacle = match (&self.sommer, los.class.has_building()) {
            (Some(c), true) => c.loss_db(los.walls_crossed, los.obstructed_len_m),
            _ => 0.0,
        };
        let weather = if self.tier == Tier::High {
            weather_attenuation_db(w, d, self.polarization, self.rain_rate_max_mm_h)
        } else {
            // 04-models.md §3 tier table: medium ignores weather attenuation.
            0.0
        };
        LossBreakdown::new(path, shadow, obstacle, weather, tx.gain_dbi + rx.gain_dbi)
    }
}

fn abbas() -> Source {
    Source {
        kind: SourceKind::Paper,
        reference: "T. Abbas et al., \"Measurement Based Shadow Fading Model for \
                    Vehicle-to-Vehicle Network Simulations\" 2015, Table II and Eq. 4 \
                    (R3 §A.5), via 04-models.md §3.2"
            .to_string(),
        accessed: None,
        note: Some(
            "The table prints channel-gain exponents; the path-loss exponents are their \
             magnitudes. LOS-to-OLOS offset measured at 8.6-10 dB."
                .to_string(),
        ),
    }
}

fn log_distance_card(preset: LogDistancePreset, auto: bool, building_term: bool) -> ModelCard {
    let p = preset.params();
    let tr36885 = Source::new(
        SourceKind::Standard,
        "3GPP TR 36.885 Annex A.1.4 (spatially correlated shadowing update and the 10 / 25 \
         / 50 m decorrelation distances), via R2c and 04-models.md §3.2",
    );
    let mut card = ModelCard::new(
        LogDistanceShadowing::ID,
        Family::Propagation,
        "1.0.0",
        "Dual-slope log-distance path loss with spatially correlated log-normal \
         shadowing, the Abbas 2015 LOS and OLOS presets, and an opt-in Sommer building \
         term for use without an obstacle stack.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "path loss".to_string(),
            latex_or_text: "PL(d) = PL0 + 10·n1·log10(d/d0) for d <= d_b; \
                            PL(d_b) + 10·n2·log10(d/d_b) above; d0 = 10 m"
                .to_string(),
            notes: Some(
                "A preset with no fitted near slope uses n2 over the whole range.".to_string(),
            ),
        },
        Equation {
            name: "spatially correlated shadowing".to_string(),
            latex_or_text: "S(n) = exp(−D/D_corr)·S(n−1) + sqrt(1 − exp(−2D/D_corr))·N(n), \
                            N ~ N(0, σ²)"
                .to_string(),
            notes: Some(
                "D is the distance moved since the last update: the sum of the two \
                 endpoints' displacements on a V2V link. The first value is drawn from \
                 the stationary distribution."
                    .to_string(),
            ),
        },
        Equation {
            name: "building term (opt-in)".to_string(),
            latex_or_text:
                "L_obs[dB] = β·n + γ·d_m (Sommer 2011; n walls crossed, d_m metres inside)"
                    .to_string(),
            notes: Some(
                "Off unless with_building_term() was called: the obstacle stack owns \
                 obstacle loss (04-models.md §3 tier table)."
                    .to_string(),
            ),
        },
    ];
    let preset_source = if preset.is_fully_cited() {
        abbas()
    } else {
        Source {
            kind: SourceKind::TodoCalibrate,
            reference: format!(
                "preset {} carries constants 04-models.md §3.2 marks UNVERIFIED or leaves \
                 blank",
                preset.label()
            ),
            accessed: None,
            note: Some(
                "Shipped so the preset id resolves, registered unvalidated per the \
                 registry rule of §3.2."
                    .to_string(),
            ),
        }
    };
    let n_source = if preset.is_fully_cited() {
        abbas()
    } else {
        preset_source.clone()
    };
    card.parameters = vec![
        Parameter {
            name: "preset".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(preset.label()),
            range: Some(
                LogDistancePreset::ALL
                    .iter()
                    .map(|p| serde_json::json!(p.label()))
                    .collect(),
            ),
            source: preset_source.clone(),
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some(
                    "Read the primary source for this row (Cheng 2007 for the cheng-* \
                     rows, Karedal 2011 Table I for the kunisch-* PL0) and replace n2, σ \
                     and PL0."
                        .to_string(),
                )
            },
        },
        Parameter {
            name: "n1".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(p.n1),
            range: None,
            source: if preset.near_slope_is_borrowed() {
                Source {
                    kind: SourceKind::TodoCalibrate,
                    reference: "Abbas 2015 Table II does not model n1 for highway OLOS \
                                (\"too few short-range samples\"); the value here is \
                                BORROWED from the abbas-los-highway row (1.66) so that \
                                the fitted far slope is not extrapolated below the \
                                breakpoint"
                        .to_string(),
                    accessed: None,
                    note: Some(
                        "The borrow makes the LOS-to-OLOS offset exactly PL0_OLOS − \
                         PL0_LOS = 10.0 dB below d_b, inside the 8.6-10 dB Abbas \
                         measured; the single-slope reading gave 25.20 dB at 100 m. \
                         Above d_b the two fitted far slopes (3.18 against 2.88) carry \
                         the offset to 11.76 dB at 400 m and 12.95 dB at 1 km."
                            .to_string(),
                    ),
                }
            } else {
                n_source.clone()
            },
            calibration: if preset.near_slope_is_borrowed() {
                Some(
                    "Fit n1 for highway OLOS against a measurement set that has \
                     short-range samples, or confirm from the primary Abbas text that no \
                     near slope is intended and that the far slope is meant to run from \
                     d0; replace the borrow either way."
                        .to_string(),
                )
            } else if preset.is_fully_cited() {
                None
            } else {
                Some("As for `preset`.".to_string())
            },
        },
        Parameter {
            name: "n2".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(p.n2),
            range: Some(vec![serde_json::json!(0.5), serde_json::json!(6.0)]),
            source: n_source.clone(),
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some("As for `preset`.".to_string())
            },
        },
        Parameter {
            name: "pl0_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(p.pl0_db),
            range: Some(vec![serde_json::json!(30.0), serde_json::json!(120.0)]),
            source: if preset.is_fully_cited() {
                abbas()
            } else {
                Source {
                    kind: SourceKind::TodoCalibrate,
                    reference: "PL0 is not printed for this row; the interim value is the \
                                Friis loss at d0 = 10 m and 5.9 GHz (67.86 dB), the \
                                standard anchor for a single-slope log-distance law"
                        .to_string(),
                    accessed: None,
                    note: Some("DERIVED, not fitted.".to_string()),
                }
            },
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some(
                    "Read PL0 at d0 = 10 m from the row's primary source and replace the \
                     Friis anchor."
                        .to_string(),
                )
            },
        },
        Parameter {
            name: "sigma_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(p.sigma_db),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(12.0)]),
            source: if preset.is_fully_cited() {
                abbas()
            } else if matches!(
                preset,
                LogDistancePreset::KunischHighway | LogDistancePreset::KunischUrban
            ) {
                Source {
                    kind: SourceKind::Paper,
                    reference: "Kunisch and Pamp as quoted by Karedal 2011 (R3 §A.4), via \
                                04-models.md §3.2; secondary"
                        .to_string(),
                    accessed: None,
                    note: None,
                }
            } else {
                Source {
                    kind: SourceKind::TodoCalibrate,
                    reference: "σ UNVERIFIED for this row; zero until it is read, so the \
                                preset applies no shadowing at all"
                        .to_string(),
                    accessed: None,
                    note: Some(
                        "A high-tier run on this preset gets the unvalidated-model warning \
                         (registry rule R2)."
                            .to_string(),
                    ),
                }
            },
            calibration: if preset.is_fully_cited()
                || matches!(
                    preset,
                    LogDistancePreset::KunischHighway | LogDistancePreset::KunischUrban
                ) {
                None
            } else {
                Some("Read σ from Cheng 2007 and replace the zero.".to_string())
            },
        },
        Parameter {
            name: "d_b_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(if p.d_b_m.is_finite() { p.d_b_m } else { -1.0 }),
            range: None,
            source: n_source,
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some("As for `preset`.".to_string())
            },
        },
        Parameter {
            name: "d0_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(D0_M),
            range: None,
            source: abbas(),
            calibration: None,
        },
        Parameter {
            name: "d_corr_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(10.0),
            range: Some(vec![serde_json::json!(1.0), serde_json::json!(100.0)]),
            source: tr36885.clone(),
            calibration: None,
        },
        Parameter {
            name: "building_term".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(if building_term { "on" } else { "off" }),
            range: Some(vec![serde_json::json!("off"), serde_json::json!("on")]),
            source: Source {
                kind: SourceKind::Paper,
                reference: "Sommer 2011 (R3 §C.1), via 04-models.md §3.5, which registers \
                            obstacle/building/sommer-2011 as an ObstacleModel *and* as a \
                            Propagation term"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Off by default: the §3 tier table composes the term as the obstacle \
                     model at every tier that has one, and budget::evaluate adds the \
                     obstacle stack's loss to this model's obstacle_db. Exactly one path \
                     owns the term, and it is the ObstacleModel."
                        .to_string(),
                ),
            },
            calibration: None,
        },
        Parameter {
            name: "beta_db_per_wall".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(SommerCoefficients::DEFAULT.beta_db_per_wall),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(20.0)]),
            source: Source {
                kind: SourceKind::Paper,
                reference: "Sommer 2011 (R3 §C.1), via 04-models.md §3.5: 9 dB per wall \
                            for the majority of the fitted dataset"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Applies only when building_term is on; the obstacle stack carries \
                     the same coefficients otherwise."
                        .to_string(),
                ),
            },
            calibration: None,
        },
        Parameter {
            name: "gamma_db_per_m".to_string(),
            unit: "dB/m".to_string(),
            default: serde_json::json!(SommerCoefficients::DEFAULT.gamma_db_per_m),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(2.0)]),
            source: Source {
                kind: SourceKind::Paper,
                reference: "Sommer 2011 (R3 §C.1), via 04-models.md §3.5: 0.4 dB/m"
                    .to_string(),
                accessed: None,
                note: Some("Applies only when building_term is on.".to_string()),
            },
            calibration: None,
        },
        Parameter {
            name: "rain_rate_max_mm_h".to_string(),
            unit: "mm/h".to_string(),
            default: serde_json::json!(50.0),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(200.0)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "the rain rate WeatherState::intensity = 1 stands for; \
                            04-models.md §3.6's worked example is a 50 mm/h downpour"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Nothing cited maps the abstract intensity onto mm/h; the mapping here \
                     is linear. The whole term is a fraction of a dB at 5.9 GHz."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Define the intensity-to-rain-rate mapping with the weather family \
                 (04-models.md §2.6) and take the rate from the scenario's weather series."
                    .to_string(),
            ),
        },
    ];
    card.assumptions = vec![
        "One shadowing process per directed link, updated on every evaluation.".to_string(),
        "The shadowing innovation is drawn from the Shadow domain keyed by LinkKey, so a \
         link's realisation does not depend on the order models ran in."
            .to_string(),
        if auto {
            "The preset is chosen per call from the land-use environment class and \
             whether a vehicle obstructs the link."
                .to_string()
        } else {
            format!("One fixed preset, {}.", preset.label())
        },
    ];
    card.limitations = vec![
        "Suburban maps to the urban rows and rural to the highway rows: the cited table \
         has four measured environments, not six (design choice, recorded here)."
            .to_string(),
        "D on a V2V link is the sum of the two endpoint displacements; TR 36.885's own \
         rule is stated for a moving UE against a fixed eNB."
            .to_string(),
        "abbas-nlos-intersection and the karedal-* rows are not shipped: the first has no \
         PL0 and is a different functional form (Mangel), the second's numeric grid is \
         UNVERIFIED and 04-models.md §3.2 marks it not shippable."
            .to_string(),
        if building_term {
            "The Sommer building term is ON inside this propagation model. It must not be \
             composed with obstacle/building/sommer-2011 in the obstacle stack: \
             budget::evaluate adds the two, and a two-wall link would be attenuated \
             twice."
                .to_string()
        } else {
            "The Sommer building term is off: obstacle loss belongs to the ObstacleModel \
             stack (04-models.md §3 tier table), and exactly one path owns it."
                .to_string()
        },
        if preset.near_slope_is_borrowed() {
            "abbas-olos-highway has no printed near slope (Abbas Table II: \"too few \
             short-range samples\"). n1 is borrowed from abbas-los-highway, so the \
             LOS-to-OLOS offset is exactly 10.0 dB below the breakpoint and reaches \
             11.76 dB at 400 m and 12.95 dB at 1 km, against the 8.6-10 dB Abbas \
             measured; the preset is registered unvalidated until n1 is fitted."
                .to_string()
        } else {
            "Every slope of this preset is a value its own source prints.".to_string()
        },
    ];
    card.ignores = vec![
        "Fast fading (the Fading family), terrain diffraction, antenna patterns; at the \
         medium tier also weather attenuation (04-models.md §3 tier table)."
            .to_string(),
    ];
    card.sources = vec![
        abbas(),
        tr36885,
        Source::new(
            SourceKind::Paper,
            "J. Karedal et al., \"Path loss modeling for vehicle-to-vehicle \
             communications\", IEEE TVT 60(1), 2011, Eq. 5 (R3 §A.4)",
        ),
        Source::new(
            SourceKind::Standard,
            "ITU-R P.838-3 Table 5 (rain coefficients, 6 GHz row), via 04-models.md §3.6",
        ),
    ];
    card.validation = Validation {
        status: if preset.is_fully_cited() && !preset.near_slope_is_borrowed() {
            ValidationStatus::LiteratureChecked
        } else {
            // The registry rule of 04-models.md §3.2: any UNVERIFIED constant makes the
            // preset unvalidated, and a borrowed near slope is not a cited one either.
            ValidationStatus::Unvalidated
        },
        references: vec![abbas()],
        tests: vec![
            "the_abbas_presets_match_the_table".to_string(),
            "the_olos_highway_offset_stays_near_the_measured_range".to_string(),
            "shadowing_autocorrelation_matches_the_ar1_coefficient".to_string(),
            "shadowing_is_stationary_with_the_presets_sigma".to_string(),
            "the_dual_slope_law_is_continuous_at_the_breakpoint".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["shadow".to_string()],
    };
    card
}

// =========================================================================================
// `propagation/tr37885`
// =========================================================================================

/// The link state of the TR 37.885 evaluation model (04-models.md §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tr37885State {
    /// Line of sight.
    Los,
    /// Same street, blocked by a vehicle.
    Nlosv,
    /// Different streets: geometric, decided by the world rather than by a draw.
    Nlos,
}

/// The probability of line of sight on an urban link:
/// `P(LOS) = min{1, 1.05·exp(−0.0114·d)}` [TR 37.885 §6.2, 04-models.md §3.3].
#[must_use]
pub fn p_los_urban(d_m: f64) -> f64 {
    (1.05 * math::exp(-0.0114 * d_m.max(0.0))).min(1.0)
}

/// The probability of line of sight on a highway link [TR 37.885 §6.2]:
/// `d ≤ 475 m: min{1, 2.1013e−6·d² − 0.002·d + 1.0193}`, above 475 m
/// `max{0, 0.54 − 0.001·(d − 475)}`.
#[must_use]
pub fn p_los_highway(d_m: f64) -> f64 {
    let d = d_m.max(0.0);
    if d <= 475.0 {
        (2.101_3e-6 * d * d - 0.002 * d + 1.019_3).clamp(0.0, 1.0)
    } else {
        (0.54 - 0.001 * (d - 475.0)).max(0.0)
    }
}

/// Highway LOS and NLOSv path loss, dB:
/// `32.4 + 20.0·log10(d3D) + 20.0·log10(fc_GHz)` [TR 37.885 Table 6.2.1-1].
#[must_use]
pub fn tr37885_highway_los_db(d3d_m: f64, fc_ghz: f64) -> f64 {
    32.4 + 20.0 * math::log10(d3d_m.max(D_MIN_M)) + 20.0 * math::log10(fc_ghz)
}

/// Urban LOS and NLOSv path loss, dB:
/// `38.77 + 16.7·log10(d3D) + 18.2·log10(fc_GHz)` [TR 37.885 Table 6.2.1-1].
#[must_use]
pub fn tr37885_urban_los_db(d3d_m: f64, fc_ghz: f64) -> f64 {
    38.77 + 16.7 * math::log10(d3d_m.max(D_MIN_M)) + 18.2 * math::log10(fc_ghz)
}

/// NLOS path loss, dB: `36.85 + 30.0·log10(d3D) + 18.9·log10(fc_GHz)`
/// [TR 37.885 Table 6.2.1-1].
#[must_use]
pub fn tr37885_nlos_db(d3d_m: f64, fc_ghz: f64) -> f64 {
    36.85 + 30.0 * math::log10(d3d_m.max(D_MIN_M)) + 18.9 * math::log10(fc_ghz)
}

/// The shadowing σ of one TR 37.885 state, dB: 3 dB for LOS and NLOSv, 4 dB for NLOS
/// [TR 37.885 Table 6.2.1-1].
#[must_use]
pub const fn tr37885_sigma_db(state: Tr37885State) -> f64 {
    match state {
        Tr37885State::Los | Tr37885State::Nlosv => 3.0,
        Tr37885State::Nlos => 4.0,
    }
}

/// The per-link state of the TR 37.885 model: the drawn link state and when it was last
/// re-evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct LinkState {
    state: Tr37885State,
    evaluated_at: SimTime,
    shadow: ShadowProcess,
}

/// `propagation/tr37885` — the 3GPP evaluation channel model with its LOS, NLOSv and
/// NLOS formulas and the link-state machine re-evaluated every 100 ms
/// (04-models.md §3.3).
#[derive(Debug, Clone)]
pub struct Tr37885 {
    card: ModelCard,
    tier: Tier,
    env: EnvClass,
    /// How often the link state is re-evaluated [TR 37.885 §6.2: at every 100 ms location
    /// update].
    restate_interval: Duration,
    d_corr_m: f64,
    links: BTreeMap<(u32, u32), LinkState>,
}

impl Tr37885 {
    /// The model's id.
    pub const ID: &'static str = "propagation/tr37885";

    /// The interval at which the link state is re-evaluated, 100 ms
    /// [TR 37.885 §6.2].
    pub const RESTATE_INTERVAL: Duration = Duration::from_millis(100);

    /// The model at `tier` for an environment class.
    #[must_use]
    pub fn new(tier: Tier, env: EnvClass) -> Self {
        Self {
            card: tr37885_card(),
            tier,
            env,
            restate_interval: Self::RESTATE_INTERVAL,
            d_corr_m: decorrelation_distance_m(env),
            links: BTreeMap::new(),
        }
    }

    /// Whether this environment uses the urban or the highway formulas and LOS
    /// probability.
    #[must_use]
    pub const fn is_urban(&self) -> bool {
        matches!(self.env, EnvClass::Urban | EnvClass::Suburban)
    }

    /// The probability of LOS at this distance in this environment.
    #[must_use]
    pub fn p_los(&self, d_m: f64) -> f64 {
        if self.is_urban() {
            p_los_urban(d_m)
        } else {
            p_los_highway(d_m)
        }
    }

    /// The link state currently held for a link, if it has been evaluated.
    #[must_use]
    pub fn state_of(&self, link: LinkKey) -> Option<Tr37885State> {
        self.links
            .get(&(link.tx().index(), link.rx().index()))
            .map(|s| s.state)
    }

    /// The deterministic path loss of one state at one distance, dB.
    #[must_use]
    pub fn path_loss_db(&self, state: Tr37885State, d3d_m: f64, fc_ghz: f64) -> f64 {
        match state {
            Tr37885State::Nlos => tr37885_nlos_db(d3d_m, fc_ghz),
            Tr37885State::Los | Tr37885State::Nlosv => {
                if self.is_urban() {
                    tr37885_urban_los_db(d3d_m, fc_ghz)
                } else {
                    tr37885_highway_los_db(d3d_m, fc_ghz)
                }
            }
        }
    }

    /// Re-evaluates the link state if the re-evaluation interval has elapsed, and returns
    /// it.
    ///
    /// A link the world reports as building-obstructed is [`Tr37885State::Nlos`] with no
    /// draw at all: "NLOS is geometric (different streets)". Otherwise the state is drawn
    /// against `P(LOS)`, from the `Shadow` domain of this link — the same stream the
    /// shadowing innovation comes from, which is deliberate: both are large-scale
    /// properties of the same link, and one stream per (domain, entity) is what keeps a
    /// draw sequence independent of what other models did.
    fn link_state<C: Ctx + ?Sized>(
        &mut self,
        ctx: &mut C,
        link: LinkKey,
        d_m: f64,
        los: &LosResult,
    ) -> Tr37885State {
        let now = ctx.now();
        let key = (link.tx().index(), link.rx().index());
        if los.class.has_building() {
            let entry = self.links.entry(key).or_insert(LinkState {
                state: Tr37885State::Nlos,
                evaluated_at: now,
                shadow: ShadowProcess::new(),
            });
            entry.state = Tr37885State::Nlos;
            entry.evaluated_at = now;
            return Tr37885State::Nlos;
        }
        let due = match self.links.get(&key) {
            None => true,
            Some(s) => {
                Duration::between(s.evaluated_at, now).as_nanos()
                    >= self.restate_interval.as_nanos()
            }
        };
        if !due {
            return self.links[&key].state;
        }
        let p = self.p_los(d_m);
        let draw = ctx.rng(RngDomain::Shadow, EntityRef::Link(link)).f64();
        // The vehicle-obstructed state is the complement of LOS on a same-street link
        // (04-models.md §3.3); a caller that already knows a vehicle blocks the link gets
        // NLOSv whatever the draw says.
        let state = if los.class.has_vehicle() || draw >= p {
            Tr37885State::Nlosv
        } else {
            Tr37885State::Los
        };
        let entry = self.links.entry(key).or_insert(LinkState {
            state,
            evaluated_at: now,
            shadow: ShadowProcess::new(),
        });
        entry.state = state;
        entry.evaluated_at = now;
        state
    }
}

impl Model for Tr37885 {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Propagation<C> for Tr37885 {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn loss_db(
        &mut self,
        ctx: &mut C,
        tx: &RadioEndpoint,
        rx: &RadioEndpoint,
        f_hz: f64,
        los: &LosResult,
        _w: &WeatherState,
    ) -> LossBreakdown {
        let d3d = tx.pos.distance(rx.pos);
        let link = LinkKey(tx.node, rx.node);
        let state = self.link_state(ctx, link, d3d, los);
        let fc_ghz = f_hz / 1e9;
        let path = self.path_loss_db(state, d3d, fc_ghz);
        let sigma = tr37885_sigma_db(state);
        let draw = ctx
            .rng(RngDomain::Shadow, EntityRef::Link(link))
            .normal(0.0, 1.0);
        let key = (tx.node.index(), rx.node.index());
        let entry = self.links.get_mut(&key).expect("link_state inserted it");
        let shadow = entry
            .shadow
            .update(tx.pos, rx.pos, sigma, self.d_corr_m, draw);
        LossBreakdown::new(path, shadow, 0.0, 0.0, tx.gain_dbi + rx.gain_dbi)
    }
}

fn tr37885_card() -> ModelCard {
    let tr = Source {
        kind: SourceKind::Standard,
        reference: "3GPP TR 37.885 §6.2, Tables 6.2-1 and 6.2.1-1 (R2c and R3 §D.4), via \
                    04-models.md §3.3"
            .to_string(),
        accessed: None,
        note: Some(
            "The TRs use 6 GHz as the carrier proxy for the 5.9 GHz band \
             (TR 37.885 Table 6.1.1-1 note)."
                .to_string(),
        ),
    };
    let mut card = ModelCard::new(
        Tr37885::ID,
        Family::Propagation,
        "1.0.0",
        "The 3GPP TR 37.885 evaluation channel model: LOS, NLOSv and NLOS path loss, the \
         LOS-probability functions, and the link-state machine re-evaluated every 100 ms.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "P(LOS) urban",
            "min{1, 1.05·exp(−0.0114·d)}; P(NLOSv) = 1 − P(LOS) on a same-street link; \
             NLOS is geometric",
        ),
        Equation::new(
            "P(LOS) highway",
            "d <= 475 m: min{1, 2.1013e−6·d² − 0.002·d + 1.0193}; d > 475 m: \
             max{0, 0.54 − 0.001·(d − 475)}; no NLOS state",
        ),
        Equation::new(
            "path loss",
            "highway LOS/NLOSv: 32.4 + 20.0·log10(d3D) + 20.0·log10(fc_GHz); urban \
             LOS/NLOSv: 38.77 + 16.7·log10(d3D) + 18.2·log10(fc_GHz); NLOS: \
             36.85 + 30.0·log10(d3D) + 18.9·log10(fc_GHz)",
        ),
    ];
    card.parameters = vec![
        Parameter::new(
            "restate_interval_ms",
            "ms",
            serde_json::json!(100),
            tr.clone(),
        ),
        Parameter {
            name: "sigma_los_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(3.0),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(12.0)]),
            source: tr.clone(),
            calibration: None,
        },
        Parameter {
            name: "sigma_nlos_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(4.0),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(12.0)]),
            source: tr.clone(),
            calibration: None,
        },
        Parameter {
            name: "d_corr_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(10.0),
            range: Some(vec![serde_json::json!(1.0), serde_json::json!(100.0)]),
            source: Source::new(
                SourceKind::Standard,
                "3GPP TR 36.885 Annex A.1.4 (10 m urban, 25 m freeway), via \
                 04-models.md §3.2",
            ),
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "The link state is re-evaluated at the 100 ms location update, not per frame.".to_string(),
        "A building-obstructed link is NLOS by geometry, with no draw.".to_string(),
        "NLOSv adds the vehicle blockage loss of the obstacle family, not of this model."
            .to_string(),
    ];
    card.limitations = vec![
        "The infrastructure links (B2V, B2R) reuse TR 38.901 UMa and RMa, which is not \
         cached: those coefficients are TODO: calibrate and are not implemented here \
         (04-models.md §3.3)."
            .to_string(),
        "No Markov state-transition memory: the state is re-drawn independently every \
         interval, so a link can flip LOS-NLOSv-LOS in 200 ms. Abbas 2015's Markov chain \
         is the cited alternative."
            .to_string(),
    ];
    card.ignores = vec![
        "Fast fading, weather attenuation and antenna patterns (04-models.md §3 tier \
         table)."
            .to_string(),
    ];
    card.sources = vec![tr];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Standard,
            "3GPP TR 37.885 Tables 6.2-1 and 6.2.1-1",
        )],
        tests: vec![
            "tr37885_los_probabilities_match_the_document".to_string(),
            "tr37885_path_loss_formulas_match_the_document".to_string(),
            "the_link_state_is_re_evaluated_every_100_ms".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["shadow".to_string()],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testctx::TestCtx;
    use crate::types::ActorClass;
    use v2xw_core::ids::NodeId;

    fn endpoint(id: u32, x: f64, y: f64, h: f64) -> RadioEndpoint {
        RadioEndpoint {
            node: NodeId::new(id),
            pos: Vec3::new(x, y, h),
            gain_dbi: 0.0,
            pattern: None,
            pos_time: 0,
            class: ActorClass::Car,
        }
    }

    #[test]
    fn friis_is_monotone_and_matches_a_hand_computation() {
        // The constant is 180 + 20·log10(4π/c) with c the defined speed of light, which
        // is 32.447783…, not the textbook's rounded 32.44. Pinned against the metre/hertz
        // form, which involves no rounding at all.
        let exact = 180.0
            + 20.0 * math::log10(4.0 * core::f64::consts::PI / numeric::SPEED_OF_LIGHT_M_S);
        assert!(
            (FRIIS_CONST_DB - exact).abs() < 1e-12,
            "{FRIIS_CONST_DB} vs {exact}"
        );
        // 100 m at 5.9 GHz: 20·log10(0.1) + 20·log10(5900) + 32.447783…
        //                 = −20 + 75.41704… + 32.44778… = 87.864823… dB.
        // The rounded constant gives 87.857040 dB — a fixed −0.007783 dB bias on every
        // free-space loss, which is 7.8 times the 1e-3 dB quantum recorded decibels are
        // rounded to, so it survives quantisation.
        let l = friis_loss_db(100.0, 5.9e9);
        assert!((l - 87.864_823_454_726_23).abs() < 1e-9, "{l}");
        assert!(
            (l - 87.857_040_232_842_88).abs() > 7e-3,
            "the rounded 32.44 is back: {l}"
        );
        // And it agrees with 20·log10(4π·d·f/c) evaluated directly, to a picodecibel.
        let direct = 20.0
            * math::log10(
                4.0 * core::f64::consts::PI * 100.0 * 5.9e9 / numeric::SPEED_OF_LIGHT_M_S,
            );
        assert!((l - direct).abs() < 1e-12, "{l} vs {direct}");
        // Doubling the distance costs exactly 6.0206 dB.
        let l2 = friis_loss_db(200.0, 5.9e9);
        assert!((l2 - l - 6.020_599_913_279_624).abs() < 1e-9);
        // Monotone non-decreasing, including through the near-field floor
        // (03-interfaces.md §17).
        let mut previous = f64::NEG_INFINITY;
        let mut d = 0.0;
        while d <= 2_000.0 {
            let now = friis_loss_db(d, 5.9e9);
            assert!(now >= previous - 1e-12, "fell at {d} m");
            previous = now;
            d += 0.5;
        }
    }

    #[test]
    fn the_two_ray_crossover_and_fresnel_breakpoint_match_the_document() {
        // The document's own worked figure: h = 1.47 m at 5.6 GHz (λ = 0.0536 m) gives a
        // Fresnel breakpoint of 161 m.
        let lambda = numeric::wavelength_m(5.6e9);
        let d_b = fresnel_breakpoint_m(1.47, 1.47, lambda);
        assert!((d_b - 161.0).abs() < 1.0, "{d_b}");
        // And the crossover distance for two 1.5 m antennas at 5.9 GHz.
        let d_c = crossover_distance_m(1.5, 1.5, numeric::wavelength_m(5.9e9));
        assert!(
            (d_c - 4.0 * core::f64::consts::PI * 2.25 / 0.050_812).abs() < 0.5,
            "{d_c}"
        );
        // Below the crossover the model is exactly Friis; above it is steeper.
        let below = two_ray_ground_loss_db(d_c * 0.5, 1.5, 1.5, 5.9e9, 1.0);
        assert!((below - friis_loss_db(d_c * 0.5, 5.9e9)).abs() < 1e-12);
        // And the two branches MEET at the crossover: the d⁻⁴ branch is derived exactly
        // from 4π·h_t·h_r/λ, so a rounded Friis constant shows up here as a step. With
        // the rounded 32.44 the step was +0.007783 dB for every antenna-height pair,
        // which is 7.8 times the recording quantum.
        for (h_t, h_r) in [(1.5, 1.5), (1.5, 5.0), (3.0, 3.0)] {
            let d = crossover_distance_m(h_t, h_r, numeric::wavelength_m(5.9e9));
            let lo = two_ray_ground_loss_db(d, h_t, h_r, 5.9e9, 1.0);
            let hi = two_ray_ground_loss_db(d * (1.0 + 1e-12), h_t, h_r, 5.9e9, 1.0);
            assert!((hi - lo).abs() < 1e-9, "step of {} dB at {d} m", hi - lo);
        }
        // Both distances must be above the crossover, which is 557 m for two 1.5 m
        // antennas at 5.9 GHz, for the asymptote's slope to be what is measured.
        let a = two_ray_ground_loss_db(1_000.0, 1.5, 1.5, 5.9e9, 1.0);
        let b = two_ray_ground_loss_db(2_000.0, 1.5, 1.5, 5.9e9, 1.0);
        // d^-4: doubling the distance costs 12.04 dB, not 6.02.
        assert!((b - a - 12.041_199_826_559_248).abs() < 1e-9, "{a} {b}");
    }

    #[test]
    fn the_abbas_presets_match_the_table() {
        let rows: [(LogDistancePreset, Option<f64>, f64, f64, f64, f64); 4] = [
            (
                LogDistancePreset::AbbasLosHighway,
                Some(1.66),
                2.88,
                66.1,
                3.95,
                104.0,
            ),
            (
                LogDistancePreset::AbbasLosUrban,
                Some(1.81),
                2.85,
                63.9,
                4.15,
                104.0,
            ),
            (
                // n1 is not printed for this row; 1.66 is borrowed from the LOS row of
                // the same environment (see `near_slope_is_borrowed`).
                LogDistancePreset::AbbasOlosHighway,
                Some(1.66),
                3.18,
                76.1,
                6.12,
                104.0,
            ),
            (
                LogDistancePreset::AbbasOlosUrban,
                Some(1.93),
                2.74,
                72.3,
                6.67,
                104.0,
            ),
        ];
        for (preset, n1, n2, pl0, sigma, d_b) in rows {
            let p = preset.params();
            assert_eq!(p.n1, n1, "{}", preset.label());
            assert_eq!(p.n2, n2, "{}", preset.label());
            assert_eq!(p.pl0_db, pl0, "{}", preset.label());
            assert_eq!(p.sigma_db, sigma, "{}", preset.label());
            assert_eq!(p.d_b_m, d_b, "{}", preset.label());
            assert!(preset.is_fully_cited());
            // At the reference distance the law is PL0 by construction.
            assert!((p.path_loss_db(D0_M) - pl0).abs() < 1e-12);
        }
        // The OLOS rows sit 8.6-10 dB above the LOS rows at the reference distance, which
        // is the measured LOS-to-OLOS offset the document quotes.
        let offset_urban = LogDistancePreset::AbbasOlosUrban.params().pl0_db
            - LogDistancePreset::AbbasLosUrban.params().pl0_db;
        let offset_highway = LogDistancePreset::AbbasOlosHighway.params().pl0_db
            - LogDistancePreset::AbbasLosHighway.params().pl0_db;
        assert!((8.0..=10.5).contains(&offset_urban), "{offset_urban}");
        assert!((8.0..=10.5).contains(&offset_highway), "{offset_highway}");
    }

    /// The LOS-to-OLOS offset must stay near the 8.6-10 dB Abbas measured over the whole
    /// range a link is evaluated at, not only at the reference distance where it is
    /// 10 dB by construction.
    ///
    /// Before the fix `abbas-olos-highway` ran its single fitted far slope (3.18) from
    /// `d0` = 10 m, which is the range Abbas Table II declines to model, and the offset
    /// climbed to 20.62 dB at 50 m, 25.20 dB at 100 m and 28.12 dB at 800 m: a highway
    /// OLOS link at 100 m came out about 15 dB too weak. The urban pair, where both
    /// slopes are fitted, was never wrong, and it is the control here.
    #[test]
    fn the_olos_highway_offset_stays_near_the_measured_range() {
        let los_hw = LogDistancePreset::AbbasLosHighway.params();
        let olos_hw = LogDistancePreset::AbbasOlosHighway.params();
        let los_urb = LogDistancePreset::AbbasLosUrban.params();
        let olos_urb = LogDistancePreset::AbbasOlosUrban.params();

        // Below the breakpoint the two highway curves are parallel — the borrowed near
        // slope is the LOS row's — so the offset is exactly PL0_OLOS − PL0_LOS.
        for d in [10.0, 20.0, 50.0, 100.0, 104.0] {
            let offset = olos_hw.path_loss_db(d) - los_hw.path_loss_db(d);
            assert!(
                (offset - 10.0).abs() < 1e-9,
                "highway offset {offset} dB at {d} m, expected exactly 10.0"
            );
        }
        // The single-slope reading this replaced put 25.2 dB at 100 m. Guard the number
        // itself so a revert is visible.
        let at_100 = olos_hw.path_loss_db(100.0) - los_hw.path_loss_db(100.0);
        assert!(at_100 < 12.0, "the extrapolated near slope is back: {at_100}");

        // Above the breakpoint the gap grows only with the difference between the two
        // FITTED far slopes (3.18 against 2.88), which is the paper's own data: 10.85 dB
        // at 200 m, 11.76 dB at 400 m, 12.95 dB at 1 km. That residual is recorded on the
        // card as a limitation, and it is why the preset stays unvalidated.
        for (d, expected) in [
            (200.0, 10.851_989_969_095_612),
            (400.0, 11.755_079_956_087_542),
            (1_000.0, 12.948_899_982_103_669),
        ] {
            let offset = olos_hw.path_loss_db(d) - los_hw.path_loss_db(d);
            assert!(
                (offset - expected).abs() < 1e-6,
                "highway offset {offset} dB at {d} m, expected {expected}"
            );
            assert!(offset < 13.0, "{offset} dB at {d} m");
        }

        // The urban pair — both slopes fitted — stays inside the measured band the whole
        // way, which is what a correctly fitted OLOS row looks like.
        let mut d = 10.0;
        while d <= 800.0 {
            let offset = olos_urb.path_loss_db(d) - los_urb.path_loss_db(d);
            assert!(
                (8.39..=9.7).contains(&offset),
                "urban offset {offset} dB at {d} m"
            );
            d += 5.0;
        }

        // And the borrow is declared, so the preset registers unvalidated.
        assert!(LogDistancePreset::AbbasOlosHighway.near_slope_is_borrowed());
        assert!(!LogDistancePreset::AbbasOlosUrban.near_slope_is_borrowed());
        let m = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasOlosHighway,
            EnvClass::Highway,
        );
        assert_eq!(m.card().validation.status, ValidationStatus::Unvalidated);
    }

    #[test]
    fn the_dual_slope_law_is_continuous_at_the_breakpoint() {
        for preset in LogDistancePreset::ALL {
            let p = preset.params();
            if !p.d_b_m.is_finite() {
                continue;
            }
            let below = p.path_loss_db(p.d_b_m - 1e-6);
            let above = p.path_loss_db(p.d_b_m + 1e-6);
            assert!(
                (above - below).abs() < 1e-6,
                "{}: {below} {above}",
                preset.label()
            );
            // And monotone in distance either side of it.
            let mut previous = f64::NEG_INFINITY;
            let mut d = 1.0;
            while d < 1_000.0 {
                let now = p.path_loss_db(d);
                assert!(now >= previous - 1e-12, "{} fell at {d}", preset.label());
                previous = now;
                d += 0.5;
            }
        }
    }

    #[test]
    fn unshipped_presets_are_not_reachable_and_the_shipped_unverified_ones_are_unvalidated() {
        // The karedal-* rows and abbas-nlos-intersection are absent by design.
        for preset in LogDistancePreset::ALL {
            assert!(!preset.label().starts_with("karedal"));
            assert_ne!(preset.label(), "abbas-nlos-intersection");
        }
        // Every preset carrying an UNVERIFIED or blank constant registers unvalidated.
        for preset in LogDistancePreset::ALL {
            let m = LogDistanceShadowing::new(Tier::Medium, preset, EnvClass::Urban);
            m.card().validate().expect("card validates");
            m.card().check_api_version().expect("api version");
            if preset.is_fully_cited() && !preset.near_slope_is_borrowed() {
                assert_eq!(
                    m.card().validation.status,
                    ValidationStatus::LiteratureChecked,
                    "{}",
                    preset.label()
                );
            } else {
                assert_eq!(
                    m.card().validation.status,
                    ValidationStatus::Unvalidated,
                    "{}",
                    preset.label()
                );
            }
        }
    }

    /// The property the AR(1) update exists for: a link that moves `Δd` between two
    /// evaluations must see a shadowing sequence whose lag-1 autocorrelation is
    /// `exp(−Δd/D_corr)`.
    #[test]
    fn shadowing_autocorrelation_matches_the_ar1_coefficient() {
        for (step_m, d_corr_m) in [(1.0, 10.0), (5.0, 10.0), (5.0, 25.0), (25.0, 25.0)] {
            let mut ctx = TestCtx::new(0xA11C_0DE5);
            let mut model = LogDistanceShadowing::new(
                Tier::Medium,
                LogDistancePreset::AbbasLosUrban,
                EnvClass::Urban,
            )
            .with_decorrelation_m(d_corr_m);
            let n = 40_000;
            let mut series = Vec::with_capacity(n);
            let rx = endpoint(1, 0.0, 200.0, 1.5);
            for i in 0..n {
                // Only the transmitter moves, one step per evaluation.
                let tx = endpoint(0, i as f64 * step_m, 0.0, 1.5);
                let b = model.loss_db(
                    &mut ctx,
                    &tx,
                    &rx,
                    5.9e9,
                    &LosResult::clear(),
                    &WeatherState::CLEAR,
                );
                series.push(b.shadow_db);
            }
            let mean = math::sum_ordered(series.iter().copied()) / n as f64;
            let var = math::sum_ordered(series.iter().map(|s| (s - mean) * (s - mean))) / n as f64;
            let cov = math::sum_ordered(series.windows(2).map(|w| (w[0] - mean) * (w[1] - mean)))
                / (n - 1) as f64;
            let measured = cov / var;
            let expected = math::exp(-step_m / d_corr_m);
            assert!(
                (measured - expected).abs() < 0.02,
                "step {step_m} m, D_corr {d_corr_m} m: measured {measured:.4}, expected \
                 {expected:.4}"
            );
        }
    }

    #[test]
    fn shadowing_is_stationary_with_the_presets_sigma() {
        let mut ctx = TestCtx::new(7);
        let preset = LogDistancePreset::AbbasLosUrban;
        let mut model = LogDistanceShadowing::new(Tier::Medium, preset, EnvClass::Urban);
        let n = 40_000;
        let mut series = Vec::with_capacity(n);
        for i in 0..n {
            let tx = endpoint(0, i as f64 * 3.0, 0.0, 1.5);
            let rx = endpoint(1, i as f64 * 3.0, 150.0, 1.5);
            let b = model.loss_db(
                &mut ctx,
                &tx,
                &rx,
                5.9e9,
                &LosResult::clear(),
                &WeatherState::CLEAR,
            );
            series.push(b.shadow_db);
        }
        let mean = math::sum_ordered(series.iter().copied()) / n as f64;
        let var = math::sum_ordered(series.iter().map(|s| (s - mean) * (s - mean))) / n as f64;
        let sd = math::sqrt(var);
        assert!(mean.abs() < 0.15, "mean {mean}");
        assert!(
            (sd - preset.params().sigma_db).abs() < 0.15,
            "sd {sd} against σ {}",
            preset.params().sigma_db
        );
    }

    #[test]
    fn a_link_that_does_not_move_keeps_its_shadowing() {
        let mut ctx = TestCtx::new(11);
        let mut model = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasLosUrban,
            EnvClass::Urban,
        );
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 100.0, 0.0, 1.5);
        let first = model
            .loss_db(
                &mut ctx,
                &tx,
                &rx,
                5.9e9,
                &LosResult::clear(),
                &WeatherState::CLEAR,
            )
            .shadow_db;
        for _ in 0..50 {
            let again = model
                .loss_db(
                    &mut ctx,
                    &tx,
                    &rx,
                    5.9e9,
                    &LosResult::clear(),
                    &WeatherState::CLEAR,
                )
                .shadow_db;
            // ρ = exp(0) = 1, so the innovation has zero weight: the realisation is held.
            assert!((again - first).abs() < 1e-12, "{first} -> {again}");
        }
        assert_eq!(
            model.shadow_db(LinkKey(NodeId::new(0), NodeId::new(1))),
            Some(first)
        );
    }

    #[test]
    fn the_building_term_is_the_sommer_formula() {
        let mut ctx = TestCtx::new(3);
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 150.0, 0.0, 1.5);
        let los = LosResult::blocked_by_buildings(2, 18.0);

        // Off by default: obstacle loss belongs to the ObstacleModel stack, and
        // budget::evaluate adds the two, so a model that applied it here as well would
        // double-count every wall.
        let mut default_model = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasLosUrban,
            EnvClass::Urban,
        );
        assert!(!default_model.has_building_term());
        let none = default_model.loss_db(&mut ctx, &tx, &rx, 5.9e9, &los, &WeatherState::CLEAR);
        assert_eq!(none.obstacle_db, 0.0, "{}", none.obstacle_db);

        // And the opt-in form, for a standalone model with no obstacle stack, is the
        // Sommer formula: β·n + γ·d = 9·2 + 0.4·18 = 25.2 dB.
        let mut model = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasLosUrban,
            EnvClass::Urban,
        )
        .with_building_term();
        let b = model.loss_db(&mut ctx, &tx, &rx, 5.9e9, &los, &WeatherState::CLEAR);
        assert!((b.obstacle_db - 25.2).abs() < 1e-9, "{}", b.obstacle_db);
        assert!((b.total_db - (b.path_db + b.shadow_db + b.obstacle_db)).abs() < 1e-9);
        // A caller-chosen building class uses its own coefficients.
        let mut light = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasLosUrban,
            EnvClass::Urban,
        )
        .with_building_coefficients(SommerCoefficients {
            beta_db_per_wall: 2.4,
            gamma_db_per_m: 0.63,
        });
        let l = light.loss_db(&mut ctx, &tx, &rx, 5.9e9, &los, &WeatherState::CLEAR);
        assert!(
            (l.obstacle_db - (2.4 * 2.0 + 0.63 * 18.0)).abs() < 1e-9,
            "{}",
            l.obstacle_db
        );
        // The card says which way it is configured, both ways.
        assert!(
            model
                .card()
                .parameters
                .iter()
                .any(|p| p.name == "building_term" && p.default == serde_json::json!("on"))
        );
        assert!(
            default_model
                .card()
                .parameters
                .iter()
                .any(|p| p.name == "building_term" && p.default == serde_json::json!("off"))
        );
    }

    #[test]
    fn weather_attenuation_is_a_fraction_of_a_decibel_at_this_carrier() {
        // 04-models.md §3.6's own worked example: R = 50 mm/h over 300 m gives about
        // 0.11 dB horizontal and 0.07 dB vertical.
        let h = rain_specific_attenuation_db_km(50.0, Polarization::Horizontal);
        let v = rain_specific_attenuation_db_km(50.0, Polarization::Vertical);
        assert!((h - 0.35).abs() < 0.02, "{h} dB/km");
        assert!((v - 0.23).abs() < 0.02, "{v} dB/km");
        let w = WeatherState::new(
            WeatherKind::Rain,
            1.0,
            300.0,
            v2xw_core::weather::SurfaceCondition::Wet,
        );
        let db = weather_attenuation_db(&w, 300.0, Polarization::Horizontal, 50.0);
        assert!((db - 0.105).abs() < 0.01, "{db} dB");
        // Clear weather costs nothing.
        assert_eq!(
            weather_attenuation_db(&WeatherState::CLEAR, 300.0, Polarization::Vertical, 50.0),
            0.0
        );
    }

    #[test]
    fn tr37885_los_probabilities_match_the_document() {
        // Urban: 1.05·exp(−0.0114·d), capped at 1. The cap binds below 4.3 m.
        assert_eq!(p_los_urban(0.0), 1.0);
        assert!((p_los_urban(100.0) - 1.05 * math::exp(-1.14)).abs() < 1e-12);
        assert!(p_los_urban(400.0) < 0.012);
        // Highway: the quadratic to 475 m, then the linear tail, and they meet at 475 m.
        let at_475_quadratic = 2.101_3e-6 * 475.0 * 475.0 - 0.002 * 475.0 + 1.019_3;
        assert!((p_los_highway(475.0) - at_475_quadratic).abs() < 1e-12);
        assert!((at_475_quadratic - 0.54).abs() < 0.01, "{at_475_quadratic}");
        assert_eq!(p_los_highway(1_500.0), 0.0);
        // Both are monotone decreasing.
        let mut previous = 1.1;
        let mut d = 0.0;
        while d < 1_000.0 {
            let now = p_los_highway(d);
            assert!(now <= previous + 1e-12, "highway rose at {d}");
            previous = now;
            d += 1.0;
        }
    }

    #[test]
    fn tr37885_path_loss_formulas_match_the_document() {
        // At d3D = 1 m every log10 term vanishes except the frequency one.
        assert!((tr37885_highway_los_db(1.0, 1.0) - 32.4).abs() < 1e-12);
        assert!((tr37885_urban_los_db(1.0, 1.0) - 38.77).abs() < 1e-12);
        assert!((tr37885_nlos_db(1.0, 1.0) - 36.85).abs() < 1e-12);
        // At 6 GHz, 100 m: the three rows of Table 6.2.1-1.
        let fc = 6.0;
        let highway = tr37885_highway_los_db(100.0, fc);
        let urban = tr37885_urban_los_db(100.0, fc);
        let nlos = tr37885_nlos_db(100.0, fc);
        assert!((highway - (32.4 + 40.0 + 20.0 * math::log10(6.0))).abs() < 1e-12);
        assert!((urban - (38.77 + 33.4 + 18.2 * math::log10(6.0))).abs() < 1e-12);
        assert!((nlos - (36.85 + 60.0 + 18.9 * math::log10(6.0))).abs() < 1e-12);
        // NLOS is the most attenuating of the three at a useful range.
        assert!(nlos > urban && nlos > highway);
        assert_eq!(tr37885_sigma_db(Tr37885State::Los), 3.0);
        assert_eq!(tr37885_sigma_db(Tr37885State::Nlosv), 3.0);
        assert_eq!(tr37885_sigma_db(Tr37885State::Nlos), 4.0);
    }

    #[test]
    fn the_link_state_is_re_evaluated_every_100_ms() {
        let mut ctx = TestCtx::new(42);
        let mut model = Tr37885::new(Tier::High, EnvClass::Urban);
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 120.0, 0.0, 1.5);
        let link = LinkKey(NodeId::new(0), NodeId::new(1));
        let first = {
            model.loss_db(
                &mut ctx,
                &tx,
                &rx,
                5.9e9,
                &LosResult::clear(),
                &WeatherState::CLEAR,
            );
            model.state_of(link).expect("state was drawn")
        };
        // Inside the interval the state is held, however many frames are evaluated.
        for step in 1..=9u64 {
            ctx.set_now(step * 10_000_000);
            model.loss_db(
                &mut ctx,
                &tx,
                &rx,
                5.9e9,
                &LosResult::clear(),
                &WeatherState::CLEAR,
            );
            assert_eq!(model.state_of(link), Some(first), "at {step}0 ms");
        }
        // Over many 100 ms updates the drawn LOS fraction tracks P(LOS).
        let p = model.p_los(120.0);
        let mut los_count = 0u32;
        let trials = 4_000u32;
        for i in 0..u64::from(trials) {
            ctx.set_now(100_000_000 * (i + 1));
            model.loss_db(
                &mut ctx,
                &tx,
                &rx,
                5.9e9,
                &LosResult::clear(),
                &WeatherState::CLEAR,
            );
            if model.state_of(link) == Some(Tr37885State::Los) {
                los_count += 1;
            }
        }
        let fraction = f64::from(los_count) / f64::from(trials);
        assert!(
            (fraction - p).abs() < 0.03,
            "drew {fraction:.3}, P(LOS) = {p:.3}"
        );
        // A building-obstructed link is NLOS by geometry, with no draw at all.
        ctx.set_now(10_000_000_000);
        model.loss_db(
            &mut ctx,
            &tx,
            &rx,
            5.9e9,
            &LosResult::blocked_by_buildings(2, 10.0),
            &WeatherState::CLEAR,
        );
        assert_eq!(model.state_of(link), Some(Tr37885State::Nlos));
    }

    #[test]
    fn every_model_here_registers_and_its_card_validates() {
        let mut registry = v2xw_core::registry::Registry::new();
        for card in [
            free_space_card(),
            two_ray_card(),
            log_distance_card(LogDistancePreset::AbbasLosUrban, true, false),
            tr37885_card(),
        ] {
            card.validate().expect("card validates");
            registry.register(card).expect("registers");
        }
        assert_eq!(registry.len(), 4);
        assert!(registry.contains(FreeSpace::ID));
        assert!(registry.contains(TwoRayGround::ID));
        assert!(registry.contains(LogDistanceShadowing::ID));
        assert!(registry.contains(Tr37885::ID));
    }

    #[test]
    fn a_propagation_model_can_be_used_as_a_trait_object() {
        // What the engine does: hold the family behind a trait object once the context
        // type is fixed.
        let mut ctx = TestCtx::new(1);
        let mut model: Box<dyn Propagation<TestCtx>> = Box::new(FreeSpace::default());
        let b = model.loss_db(
            &mut ctx,
            &endpoint(0, 0.0, 0.0, 1.5),
            &endpoint(1, 100.0, 0.0, 1.5),
            5.9e9,
            &LosResult::clear(),
            &WeatherState::CLEAR,
        );
        assert!((b.total_db - friis_loss_db(100.0, 5.9e9)).abs() < 1e-12);
        assert_eq!(model.id(), FreeSpace::ID);
    }
}
