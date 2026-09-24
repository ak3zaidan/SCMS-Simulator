//! `mobility/car-following/idm` — the Intelligent Driver Model (04-models.md §2.1).
//!
//! # The equations, as §2.1 states them
//!
//! ```text
//! a = a_max · [ 1 − (v/v0)^δ − (s*(v,Δv)/s)² ]
//! s*(v,Δv) = s0 + s1·√(v/v0) + max(0, T·v + v·Δv / (2·√(a_max·b)))   (s1 = 0 in the base model)
//! ```
//!
//! with `s` the net gap (leader rear to ego front) and `Δv = v − v_lead`. On a free road
//! the second term of `s*` never appears and the acceleration is `a_max(1 − (v/v0)^δ)`.
//!
//! ## Why the `max(0, ·)`
//!
//! §2.1 and R10 §B1 print the interaction term without it, and a reader who stops there
//! will not expect it here. It is nonetheless what the model means, for a reason that is
//! visible in the definition rather than in any one printing: `s*` is the **desired
//! minimum gap**, a distance, and the acceleration depends on it only through `(s*/s)²`.
//!
//! Three thresholds, all in ordinary traffic and all reached by a leader simply
//! accelerating away. The two forms are identical while the interaction term is
//! non-negative, that is while the leader is less than `T·2·√(a_max·b)` faster —
//! **5.0 m/s** for the [`IdmPreset::Kesting2010`] car and 4.7 m/s for its truck,
//! whatever the ego's own speed. Past `(s0 + T·v)·2·√(a_max·b)/v` — 6.4 m/s at
//! v = 5 m/s, 5.4 m/s at v = 20 m/s — `s*` is a *negative distance*, which the model has
//! no reading for. And past `(2·s0 + T·v)·2·√(a_max·b)/v` — 7.7 m/s at v = 5 m/s — the
//! square exceeds the floored one and grows without bound as the leader accelerates
//! away, so the ego brakes **harder the faster its leader goes**. At v = 5 m/s with a
//! leader 25 m ahead at 30 m/s the unfloored form returns −0.34 m/s² where a free road
//! gives +1.40: the vehicle decelerates out of a queue because the vehicle in front of
//! it is leaving.
//!
//! The frozen reference engine's port floors it, and that is checked rather than
//! remembered: `s_star = cfg.idm_min_gap + max(0.0, v_cur * cfg.idm_time_headway + v_cur *
//! dv / (2 * math.sqrt(a_max * b_dec)))` [`run.py` L2280-2281]. The physical-layer defect
//! register records that Treiber's own reference implementation writes the same floor —
//! **UNVERIFIED here**: no copy of that implementation is in this repository, so it is
//! carried as a second-hand statement and the argument above does not rest on it.
//! [`IdmParams::clamp_s_star`] is **on in every preset**. Turning it off reproduces the
//! literal printed expression, artefact included.
//!
//! # Parameter sets
//!
//! Four, all from §2.1's table and none invented:
//!
//! | Preset | Source |
//! |---|---|
//! | [`IdmPreset::Kesting2010`] (**default**) | Kesting, Treiber and Helbing 2010, car and truck columns [R10 §B2] |
//! | [`IdmPreset::Treiber2000`] | Treiber, Hennecke and Helbing 2000, freeway calibration [R10 §B1] |
//! | [`IdmPreset::Kesting2007`] | the IDM set the MOBIL study used [R10 §B3] |
//! | [`IdmPreset::Legacy`] | the frozen reference engine [`run.py` L142-146, L2274-2283] |
//!
//! # The clamps
//!
//! The reference engine's port of the IDM carries four clamps that the printed equation
//! does not have, and §2.1 records each of them: the gap is floored at 0.5 m *after* the
//! leader's length is subtracted, `v0` is floored at 0.1 m/s, the interaction term of `s*`
//! is floored at zero (so `s*` never falls below `s0`), and the result is clamped to
//! `[−6.0, a_max]`.
//!
//! **All four are in force in every preset, not only in [`IdmPreset::Legacy`]** — a
//! reader who expects "the legacy clamps" to be legacy-only would be wrong about all of
//! them. Three carry the source `code (legacy)`, because they are the reference engine's
//! numbers and a parity run needs them, and because the `−6 m/s²` floor is the only
//! deceleration limit the model has until the `TODO: calibrate` plan of §2.1 settles one.
//! The fourth, [`IdmParams::clamp_s_star`], is not a legacy number at all but the model's
//! own domain — see "Why the `max(0, ·)`" above — and is sourced accordingly.
//!
//! # Determinism
//!
//! `accel` draws no random numbers and reads no clock: it is a pure function of the view it
//! is handed. Driver heterogeneity arrives as a per-vehicle [`DriverProfile`], drawn once at
//! spawn from the actor's own [`v2xw_core::rng::RngDomain::DesiredSpeed`] stream. Every
//! transcendental goes through [`v2xw_core::math`].

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::math;
use v2xw_core::weather::WeatherState;

use crate::classes::{LegacyClass, VehicleClass};
use crate::traits::CarFollowing;
use crate::views::{DriverProfile, LaneView, LeaderView, VehicleView};
use crate::weather::{self, RoadContext, WeatherResponse};

/// The model id.
pub const MODEL_ID: &str = "mobility/car-following/idm";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The legacy acceleration floor, m/s² ([`run.py` L2283]).
pub const LEGACY_A_MIN_MPS2: f64 = -6.0;

/// The legacy desired-speed floor, m/s ([`run.py` L2275]).
pub const LEGACY_V0_FLOOR_MPS: f64 = 0.1;

/// The legacy net-gap floor, m ([`run.py` L2278]).
pub const LEGACY_GAP_FLOOR_M: f64 = 0.5;

/// The legacy lookahead, m (`idm_lookahead_m`).
pub const LEGACY_LOOKAHEAD_M: f64 = 70.0;

/// The legacy trip-speed range, m/s (`trip_speed_min`, `trip_speed_max`).
pub const LEGACY_TRIP_SPEED_RANGE_MPS: (f64, f64) = (8.0, 18.0);

/// The model-wide constants: everything that is not per vehicle.
///
/// The per-vehicle numbers — desired speed, `a_max`, `b`, `T`, `s0` — are a
/// [`DriverProfile`] and reach the model through the [`VehicleView`], because they are
/// drawn per driver.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdmParams {
    /// The free-acceleration exponent `δ`. 4 in every cited set.
    pub delta: f64,
    /// The `s1` term of `s*`, metres. 0 in the base model (Treiber 2000 sets `s1 = 0`).
    pub s1_m: f64,
    /// Lower clamp on the result, m/s². The legacy floor, and the only deceleration limit
    /// the model has until §2.1's calibration plan settles one.
    pub a_min_mps2: f64,
    /// Lower clamp on `v0`, m/s, so a stopped driver with a zero desired speed does not
    /// divide by zero (`code (legacy)`).
    pub v0_floor_mps: f64,
    /// Lower clamp on the net gap, metres (`code (legacy)`).
    pub gap_floor_m: f64,
    /// Whether to floor the interaction term of `s*` at zero, so `s*` never falls below
    /// `s0`.
    ///
    /// **On in every preset.** `s*` is a desired minimum *gap* and the equation uses it
    /// only as `(s*/s)²`, so an unfloored negative value makes a vehicle brake because
    /// its leader is pulling away — harder, the faster the leader goes. See the module
    /// documentation. Setting it false reproduces the literally printed expression of
    /// §2.1 and R10 §B1, artefact and all, and is offered only for that comparison.
    pub clamp_s_star: bool,
    /// Whether to run the Kesting 2010 enhancement (the constant-acceleration heuristic,
    /// the paper's "ACC model"), which removes the base model's over-reaction to a cut-in.
    pub enhanced: bool,
    /// The enhancement's coolness factor `c`, dimensionless.
    pub coolness: f64,
    /// How far ahead the leader search looks, metres. Not part of the equation: it is the
    /// range the neighbour query is given, and it is declared here because the model's
    /// behaviour depends on it (a leader beyond it is a free road).
    pub lookahead_m: f64,
    /// Which cited weather table the model applies (04-models.md §2.6).
    pub weather_response: WeatherResponse,
    /// Whether the FHWA freeway or arterial rows apply.
    pub road_context: RoadContext,
}

impl Default for IdmParams {
    /// The base model with the clamps: `δ = 4`, `s1 = 0`, `s*` floored at `s0`, no
    /// enhancement.
    fn default() -> Self {
        Self {
            delta: 4.0,
            s1_m: 0.0,
            a_min_mps2: LEGACY_A_MIN_MPS2,
            v0_floor_mps: LEGACY_V0_FLOOR_MPS,
            gap_floor_m: LEGACY_GAP_FLOOR_M,
            clamp_s_star: true,
            enhanced: false,
            coolness: 0.99,
            lookahead_m: LEGACY_LOOKAHEAD_M,
            weather_response: WeatherResponse::Fhwa,
            // The FHWA arterial rows on a city street and the freeway rows on an
            // expressway, by the lane's limit: one world can hold both.
            road_context: RoadContext::BySpeedLimit,
        }
    }
}

/// One of the four cited parameter sets of §2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdmPreset {
    /// Kesting, Treiber and Helbing 2010, car and truck columns [R10 §B2]. **The default
    /// set for the native medium tier**, as §2.1 prescribes.
    #[default]
    Kesting2010,
    /// Treiber, Hennecke and Helbing 2000, the freeway calibration [R10 §B1]. The set the
    /// fundamental-diagram targets of §2.9 were measured with.
    Treiber2000,
    /// The IDM set underlying the MOBIL study [R10 §B3].
    Kesting2007,
    /// The frozen reference engine's set, with its clamps
    /// (`code (legacy)` [`run.py` L142-146, L2274-2283]).
    Legacy,
}

impl IdmPreset {
    /// Every preset, in the order §2.1's table lists them.
    pub const ALL: [IdmPreset; 4] = [
        IdmPreset::Kesting2010,
        IdmPreset::Treiber2000,
        IdmPreset::Kesting2007,
        IdmPreset::Legacy,
    ];

    /// A stable label, also the parameter-set name a scenario selects.
    pub const fn label(self) -> &'static str {
        match self {
            IdmPreset::Kesting2010 => "kesting-2010",
            IdmPreset::Treiber2000 => "treiber-2000",
            IdmPreset::Kesting2007 => "kesting-2007",
            IdmPreset::Legacy => "legacy",
        }
    }

    /// The citation for this set.
    pub fn source(self) -> Source {
        match self {
            IdmPreset::Kesting2010 => Source {
                kind: SourceKind::Paper,
                reference: "Kesting, Treiber & Helbing 2010, Phil. Trans. R. Soc. A 368, 4585 \
                            (arXiv:0912.3613), car and truck columns [R10 §B2]"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: None,
            },
            IdmPreset::Treiber2000 => Source {
                kind: SourceKind::Paper,
                reference: "Treiber, Hennecke & Helbing 2000, Phys. Rev. E 62, 1805 \
                            (arXiv:cond-mat/0002177), freeway calibration [R10 §B1]"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: None,
            },
            IdmPreset::Kesting2007 => Source {
                kind: SourceKind::Paper,
                reference: "Kesting, Treiber & Helbing 2007, Transportation Research Record \
                            1999, 86-94 (DOI 10.3141/1999-10), the IDM set of the MOBIL study \
                            [R10 §B3]"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: None,
            },
            IdmPreset::Legacy => Source {
                kind: SourceKind::Code,
                reference: "legacy/scms_sim_ref/mock_pipeline/run.py L142-146, L2274-2283"
                    .to_string(),
                accessed: Some("2026-09-18".to_string()),
                note: Some("the frozen reference engine's IDM port and its clamps".to_string()),
            },
        }
    }

    /// The model-wide constants this set comes with.
    pub fn params(self) -> IdmParams {
        let base = IdmParams::default();
        match self {
            IdmPreset::Kesting2010 | IdmPreset::Treiber2000 | IdmPreset::Kesting2007 => base,
            // `clamp_s_star` is already true in `base`; the legacy set differs only in
            // which weather table it reads.
            IdmPreset::Legacy => IdmParams {
                weather_response: WeatherResponse::Legacy,
                ..base
            },
        }
    }

    /// The driver this set gives a vehicle of `class`.
    ///
    /// The cited sets have a car column and a truck column, so the twelve SUMO classes fold
    /// onto the two: anything heavier than a delivery van drives the truck column. The fold
    /// is a design choice and is recorded on the card; the alternative — inventing a
    /// motorcycle column — would put uncited numbers in the model.
    ///
    /// The legacy set is per class by construction (its four classes carry their own `a`
    /// and `b`), and its desired speed is a per-trip uniform draw in
    /// `[8, 18] m/s × speed_mult`; with no trip to draw for, the midpoint 13 m/s is used
    /// (DERIVED from the cited range) and the demand model overrides it per trip.
    pub fn profile(self, class: VehicleClass) -> DriverProfile {
        let heavy = matches!(
            class,
            VehicleClass::Truck
                | VehicleClass::Trailer
                | VehicleClass::Bus
                | VehicleClass::Coach
                | VehicleClass::Delivery
        );
        match self {
            IdmPreset::Kesting2010 if heavy => DriverProfile {
                desired_speed_mps: 23.6,
                max_accel_mps2: 0.7,
                comfort_decel_mps2: 2.0,
                time_headway_s: 2.0,
                min_gap_m: 4.0,
            },
            IdmPreset::Kesting2010 => DriverProfile {
                desired_speed_mps: 33.3,
                max_accel_mps2: 1.4,
                comfort_decel_mps2: 2.0,
                time_headway_s: 1.5,
                min_gap_m: 2.0,
            },
            IdmPreset::Treiber2000 => DriverProfile {
                desired_speed_mps: 33.3,
                max_accel_mps2: 0.73,
                comfort_decel_mps2: 1.67,
                time_headway_s: 1.6,
                min_gap_m: 2.0,
            },
            IdmPreset::Kesting2007 if heavy => DriverProfile {
                desired_speed_mps: 22.2,
                max_accel_mps2: 1.5,
                comfort_decel_mps2: 2.0,
                time_headway_s: 1.2,
                min_gap_m: 2.0,
            },
            IdmPreset::Kesting2007 => DriverProfile {
                desired_speed_mps: 33.3,
                max_accel_mps2: 1.5,
                comfort_decel_mps2: 2.0,
                time_headway_s: 1.2,
                min_gap_m: 2.0,
            },
            IdmPreset::Legacy => {
                let legacy = match class {
                    VehicleClass::Motorcycle | VehicleClass::Moped | VehicleClass::Scooter => {
                        LegacyClass::Motorcycle
                    }
                    VehicleClass::Truck | VehicleClass::Trailer | VehicleClass::Delivery => {
                        LegacyClass::Truck
                    }
                    VehicleClass::Bus | VehicleClass::Coach => LegacyClass::Bus,
                    _ => LegacyClass::Car,
                };
                let spec = legacy.spec();
                let midpoint =
                    0.5 * (LEGACY_TRIP_SPEED_RANGE_MPS.0 + LEGACY_TRIP_SPEED_RANGE_MPS.1);
                DriverProfile {
                    desired_speed_mps: midpoint * spec.speed_mult,
                    max_accel_mps2: spec.accel_mps2,
                    comfort_decel_mps2: spec.decel_mps2,
                    time_headway_s: 1.3,
                    min_gap_m: 2.5,
                }
            }
        }
    }
}

/// The Intelligent Driver Model.
#[derive(Debug, Clone)]
pub struct Idm {
    params: IdmParams,
    preset: IdmPreset,
    card: ModelCard,
}

impl Default for Idm {
    fn default() -> Self {
        Idm::new(IdmPreset::default())
    }
}

impl Idm {
    /// The model with one of the cited parameter sets.
    pub fn new(preset: IdmPreset) -> Self {
        let params = preset.params();
        Self {
            card: card(preset, &params),
            params,
            preset,
        }
    }

    /// The model with a parameter set of its own, labelled by the preset it derives from.
    pub fn with_params(preset: IdmPreset, params: IdmParams) -> Self {
        Self {
            card: card(preset, &params),
            params,
            preset,
        }
    }

    /// The model-wide constants in force.
    pub fn params(&self) -> &IdmParams {
        &self.params
    }

    /// Which cited set this instance carries.
    pub fn preset(&self) -> IdmPreset {
        self.preset
    }

    /// The driver this instance's set gives a vehicle of `class`.
    pub fn profile(&self, class: VehicleClass) -> DriverProfile {
        self.preset.profile(class)
    }

    /// The acceleration equation itself, on bare numbers.
    ///
    /// Exposed because it is the thing worth testing against a hand computation, and
    /// because MOBIL needs to evaluate it for a *neighbour* rather than for the ego
    /// (04-models.md §2.2: the safety and incentive criteria are differences of
    /// accelerations of three different vehicles).
    ///
    /// `gap_m` is the net gap, leader rear to ego front; [`f64::INFINITY`] is a free road.
    pub fn accel_of(
        &self,
        v_mps: f64,
        v0_mps: f64,
        gap_m: f64,
        v_lead_mps: f64,
        driver: &DriverProfile,
    ) -> f64 {
        self.accel_full(v_mps, v0_mps, gap_m, v_lead_mps, 0.0, driver)
    }

    /// The equation with the leader's acceleration supplied, which the Kesting 2010
    /// enhancement needs.
    ///
    /// [`Idm::accel_of`] is this with `a_lead = 0`, which is what a model that cannot
    /// observe the leader's acceleration assumes; the trait path passes the real value from
    /// the snapshot.
    pub fn accel_full(
        &self,
        v_mps: f64,
        v0_mps: f64,
        gap_m: f64,
        v_lead_mps: f64,
        a_lead_mps2: f64,
        driver: &DriverProfile,
    ) -> f64 {
        let p = &self.params;
        let v0 = v0_mps.max(p.v0_floor_mps);
        let a_max = driver.max_accel_mps2;
        let free = 1.0 - math::pow(v_mps / v0, p.delta);
        let raw = if gap_m.is_infinite() {
            a_max * free
        } else {
            let gap = gap_m.max(p.gap_floor_m);
            let dv = v_mps - v_lead_mps;
            let interaction = driver.time_headway_s * v_mps
                + v_mps * dv / (2.0 * math::sqrt(a_max * driver.comfort_decel_mps2));
            let interaction = if p.clamp_s_star {
                interaction.max(0.0)
            } else {
                interaction
            };
            let s_star = driver.min_gap_m + p.s1_m * math::sqrt(v_mps / v0) + interaction;
            let idm = a_max * (free - (s_star / gap) * (s_star / gap));
            if p.enhanced {
                self.enhanced(idm, v_mps, gap, v_lead_mps, a_lead_mps2, driver)
            } else {
                idm
            }
        };
        raw.clamp(p.a_min_mps2, a_max)
    }

    /// The Kesting 2010 enhancement: the constant-acceleration heuristic blended into the
    /// IDM result by the coolness factor.
    ///
    /// The paper's "ACC model": where the IDM would brake harder than a driver who
    /// *assumes the leader keeps its current acceleration* would,
    /// `a = (1 − c)·a_IDM + c·[a_CAH + b·tanh((a_IDM − a_CAH)/b)]`, and where it would
    /// not, `a = a_IDM`. The CAH itself is
    /// `a_CAH = v²ã_l / (v_l² − 2·s·ã_l)` when `v_l(v − v_l) ≤ −2·s·ã_l`, and
    /// `ã_l − (v − v_l)²Θ(v − v_l)/(2s)` otherwise, with `ã_l = min(a_l, a_max)`.
    ///
    /// **Status: UNVERIFIED at the research-sheet level.** R10 §B2 records the *parameter*
    /// table of Kesting 2010 (including `c = 0.99`) but not this expression, so the
    /// structure here is transcribed from the paper and has not been checked against a
    /// primary copy by the sheets. The enhancement is therefore **off by default**; a
    /// scenario that turns it on gets a model card whose equation carries this note.
    fn enhanced(
        &self,
        idm: f64,
        v: f64,
        gap: f64,
        v_lead: f64,
        a_lead: f64,
        driver: &DriverProfile,
    ) -> f64 {
        let a_max = driver.max_accel_mps2;
        let b = driver.comfort_decel_mps2;
        let a_tilde = a_lead.min(a_max);
        let dv = v - v_lead;
        let cah = if v_lead * dv <= -2.0 * gap * a_tilde {
            let denom = v_lead * v_lead - 2.0 * gap * a_tilde;
            if denom.abs() < f64::EPSILON {
                idm
            } else {
                v * v * a_tilde / denom
            }
        } else {
            let step = if dv > 0.0 { 1.0 } else { 0.0 };
            a_tilde - dv * dv * step / (2.0 * gap.max(f64::EPSILON))
        };
        if idm >= cah {
            idm
        } else {
            let c = self.params.coolness;
            (1.0 - c) * idm + c * (cah + b * math::tanh((idm - cah) / b))
        }
    }
}

impl v2xw_core::model::Model for Idm {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl CarFollowing for Idm {
    fn accel(
        &self,
        ego: &VehicleView,
        leader: Option<&LeaderView>,
        lane: &LaneView,
        w: &WeatherState,
    ) -> f64 {
        let road = self.params.road_context.resolve(lane.speed_limit_mps);
        let effects = weather::driving_effects(self.params.weather_response, w, road);
        // The order weather applies, recorded on the card: the legal limit and the
        // driver's wish are reconciled first, then the weather slows the result. Applying
        // the factor only to the driver's wish would leave an urban run where the limit
        // binds completely unaffected by snow.
        let mut v0 =
            effects.apply_to_desired_speed(ego.driver.desired_speed_mps.min(lane.speed_limit_mps));
        // In fog a driver keeps to a speed they can stop from within what they can see:
        // the AASHTO stopping sight distance, solved for speed.
        if self.params.weather_response == WeatherResponse::Fhwa {
            v0 = v0.min(weather::sight_limited_speed_mps(effects.visibility_m));
        }
        let driver = DriverProfile {
            desired_speed_mps: v0,
            max_accel_mps2: ego.driver.max_accel_mps2,
            comfort_decel_mps2: effects.cap_decel(ego.driver.comfort_decel_mps2),
            time_headway_s: effects.apply_to_headway(ego.driver.time_headway_s),
            min_gap_m: ego.driver.min_gap_m,
        };
        let a = match leader {
            None => self.accel_full(ego.speed_mps, v0, f64::INFINITY, 0.0, 0.0, &driver),
            Some(l) => self.accel_full(
                ego.speed_mps,
                v0,
                l.gap_m,
                l.speed_mps,
                l.accel_mps2,
                &driver,
            ),
        };
        // And no car brakes harder than its tyres grip on this surface.
        a.max(-effects.max_decel_mps2)
    }

    fn profile(&self, class: VehicleClass) -> DriverProfile {
        self.preset.profile(class)
    }
}

/// The model card for one preset.
pub fn card(preset: IdmPreset, params: &IdmParams) -> ModelCard {
    let src = preset.source();
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L2274-2283".to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: Some("the reference engine's clamps, kept for parity".to_string()),
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "Longitudinal acceleration from the ego's speed, the net gap to the leader and the \
         speed difference, by the Intelligent Driver Model. Every stopping rule in the \
         crate — red signals, yielding at an unsignalised junction, curve-speed caps — is \
         expressed as a virtual leader, so this one equation produces every deceleration a \
         vehicle applies.",
    );
    card.tier = vec![Tier::Medium];
    card.equations = vec![
        Equation {
            name: "acceleration".to_string(),
            latex_or_text: "a = a_max · [ 1 − (v/v0)^δ − (s*(v,Δv)/s)² ]".to_string(),
            notes: Some(
                "s is the net gap (leader rear to ego front); on a free road the second \
                 interaction term is absent and a = a_max(1 − (v/v0)^δ)"
                    .to_string(),
            ),
        },
        Equation {
            name: "desired minimum gap".to_string(),
            latex_or_text: "s*(v,Δv) = s0 + s1·√(v/v0) + max(0, T·v + v·Δv / (2·√(a_max·b)))"
                .to_string(),
            notes: Some(
                "s1 = 0 in the base model (Treiber 2000 sets it so). The max(0, ·) is \
                 `clamp_s_star`: s* is a desired minimum *gap* and the equation reads it \
                 only as (s*/s)², so an unfloored negative value would brake a vehicle \
                 whose leader is pulling away"
                    .to_string(),
            ),
        },
        Equation {
            name: "weather".to_string(),
            latex_or_text:
                "v0 ← desired_speed_factor · min(v0_driver, v_limit);  T ← headway_factor · T;  \
                 b ← min(b, decel_cap)"
                    .to_string(),
            notes: Some(
                "the order is a design choice recorded here: the legal limit and the \
                 driver's wish are reconciled first, then the weather slows the result \
                 (04-models.md §2.6)"
                    .to_string(),
            ),
        },
    ];
    if params.enhanced {
        card.equations.push(Equation {
            name: "Kesting 2010 enhancement (ACC model)".to_string(),
            latex_or_text: "a = (1−c)·a_IDM + c·[a_CAH + b·tanh((a_IDM − a_CAH)/b)] when \
                            a_IDM < a_CAH, else a_IDM"
                .to_string(),
            notes: Some(
                "UNVERIFIED at the research-sheet level: R10 §B2 records the parameter table \
                 of Kesting 2010 but not this expression, so its structure is transcribed \
                 from the paper and has not been checked against a primary copy. Off by \
                 default."
                    .to_string(),
            ),
        });
    }
    let profile_car = preset.profile(VehicleClass::Passenger);
    let profile_truck = preset.profile(VehicleClass::Truck);
    card.parameters = vec![
        Parameter::new(
            "preset",
            "-",
            serde_json::json!(preset.label()),
            src.clone(),
        ),
        Parameter::new(
            "v0",
            "m/s",
            serde_json::json!({"car": profile_car.desired_speed_mps, "truck": profile_truck.desired_speed_mps}),
            src.clone(),
        ),
        Parameter::new(
            "T",
            "s",
            serde_json::json!({"car": profile_car.time_headway_s, "truck": profile_truck.time_headway_s}),
            src.clone(),
        ),
        Parameter::new(
            "a_max",
            "m/s²",
            serde_json::json!({"car": profile_car.max_accel_mps2, "truck": profile_truck.max_accel_mps2}),
            src.clone(),
        ),
        Parameter::new(
            "b",
            "m/s²",
            serde_json::json!({"car": profile_car.comfort_decel_mps2, "truck": profile_truck.comfort_decel_mps2}),
            src.clone(),
        ),
        Parameter::new(
            "s0",
            "m",
            serde_json::json!({"car": profile_car.min_gap_m, "truck": profile_truck.min_gap_m}),
            src.clone(),
        ),
        Parameter::new("delta", "1", serde_json::json!(params.delta), src.clone()),
        Parameter::new(
            "s1",
            "m",
            serde_json::json!(params.s1_m),
            Source::new(
                SourceKind::Paper,
                "Treiber, Hennecke & Helbing 2000: the base model sets s1 = 0 [R10 §B1]",
            ),
        ),
        Parameter {
            name: "a_min".to_string(),
            unit: "m/s²".to_string(),
            default: serde_json::json!(params.a_min_mps2),
            range: None,
            source: Source::todo_calibrate(
                "the reference engine's −6 m/s² acceleration floor (run.py L2283)",
            ),
            calibration: Some(
                "04-models.md §2.1's plan, unchanged: compare against SUMO's `emergencyDecel` \
                 (9 m/s² passenger, 7 m/s² truck [R10 §B4]) and against the FHWA \
                 yellow-interval deceleration assumption of about 10 ft/s² ≈ 3.05 m/s² \
                 [R10 §B10, secondary]. Until then the legacy value is in force and is the \
                 model's only deceleration limit."
                    .to_string(),
            ),
        },
        Parameter::new(
            "v0_floor",
            "m/s",
            serde_json::json!(params.v0_floor_mps),
            legacy.clone(),
        ),
        Parameter::new(
            "gap_floor",
            "m",
            serde_json::json!(params.gap_floor_m),
            legacy.clone(),
        ),
        Parameter {
            name: "clamp_s_star".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(params.clamp_s_star),
            range: None,
            source: Source {
                kind: SourceKind::Paper,
                reference: "Treiber, Hennecke & Helbing 2000: s* is the *desired minimum \
                            gap* [R10 §B1], and the acceleration reads it only as (s*/s)², \
                            so the model's domain is s* ≥ s0. The frozen engine's port \
                            writes the floor explicitly (run.py L2280-2281, checked); the \
                            physical-layer defect register reports that Treiber's own \
                            reference implementation does too, UNVERIFIED here"
                    .to_string(),
                accessed: Some("2026-09-19".to_string()),
                note: Some(
                    "the printed expression of §2.1 and R10 §B1 omits the max(0, ·); \
                     without it the two forms part company once the leader is T·2·√(a·b) \
                     faster (5.0 m/s, Kesting 2010 car), s* is a negative distance past \
                     (s0 + T·v)·2·√(a·b)/v (6.4 m/s at v = 5 m/s), and past \
                     (2·s0 + T·v)·2·√(a·b)/v (7.7 m/s at v = 5 m/s) a faster leader \
                     produces MORE braking, not less: at v = 5 m/s behind a leader 25 m \
                     ahead at 30 m/s the answer is −0.34 m/s² where a free road gives \
                     +1.40"
                        .to_string(),
                ),
            },
            calibration: None,
        },
        Parameter::new(
            "lookahead",
            "m",
            serde_json::json!(params.lookahead_m),
            legacy,
        ),
        Parameter::new(
            "enhanced",
            "-",
            serde_json::json!(params.enhanced),
            Source::new(
                SourceKind::Paper,
                "Kesting, Treiber & Helbing 2010: the ACC-model enhancement [R10 §B2]",
            ),
        ),
        Parameter::new(
            "coolness",
            "1",
            serde_json::json!(params.coolness),
            Source::new(
                SourceKind::Paper,
                "Kesting, Treiber & Helbing 2010: c = 0.99, realistic range 0.95-1.00 \
                 [R10 §B2]",
            ),
        ),
    ];
    card.parameters.extend(weather::fhwa_parameters());
    card.assumptions = vec![
        "One leader, no lateral coupling, instantaneous reaction (04-models.md §2.1).".to_string(),
        "The cited sets have a car column and a truck column, so the twelve vehicle classes \
         of §2.7 fold onto the two; `IdmPreset::profile` documents the fold."
            .to_string(),
        "Between mobility steps, positions follow the published constant-velocity \
         extrapolation rule (02-architecture.md §5.2), not this model's acceleration."
            .to_string(),
    ];
    card.limitations = vec![
        "Known over-reaction to a cut-in without the Kesting 2010 enhancement.".to_string(),
        format!(
            "`clamp_s_star` is {}. With it false the interaction term of `s*` goes \
             negative once the leader is T·2·√(a_max·b) faster — 5.0 m/s for the \
             Kesting 2010 car, whatever the ego's own speed — s* is a negative distance \
             past (s0 + T·v)·2·√(a_max·b)/v (6.4 m/s at v = 5 m/s), and past \
             (2·s0 + T·v)·2·√(a_max·b)/v (7.7 m/s at v = 5 m/s) a faster leader produces \
             more braking rather than less, down to −0.34 m/s² at v = 5 m/s behind a \
             leader 25 m ahead at 30 m/s. It is true in every shipped preset for that \
             reason.",
            params.clamp_s_star
        ),
        "No reaction time and no driver imperfection: both are the `high` tier's business \
         (SUMO's `sigma` and action-step length)."
            .to_string(),
    ];
    card.ignores = vec![
        "Driver imperfection (`sigma`), action step length and sub-second reaction models \
         (medium relative to high, 04-models.md §2.1)."
            .to_string(),
    ];
    card.sources = vec![src];
    card.determinism = v2xw_core::card::Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §2.9 fundamental-diagram targets [R10 §B15]",
        )],
        tests: vec![
            "carfollowing::idm::tests::kesting_2010_car_matches_a_hand_computation".to_string(),
            "fd::tests::fundamental_diagram_of_a_ring_matches_the_targets".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::geom::Dims;
    use v2xw_core::ids::{ActorId, LaneId};
    use v2xw_core::model::Model;
    use v2xw_world::{ClassMask, LaneKind};

    fn lane(limit: f64) -> LaneView {
        LaneView {
            id: LaneId::new(0),
            kind: LaneKind::Driving,
            speed_limit_mps: limit,
            width_m: 3.5,
            length_m: 500.0,
            allowed: ClassMask::MOTOR_TRAFFIC,
        }
    }

    fn ego(speed: f64, driver: DriverProfile) -> VehicleView {
        VehicleView {
            actor: ActorId::new(0),
            class: VehicleClass::Passenger,
            lane: LaneId::new(0),
            lane_index: 0,
            s_m: 100.0,
            lateral_m: 0.0,
            speed_mps: speed,
            accel_mps2: 0.0,
            heading_rad: 0.0,
            dims: Dims::new(5.0, 1.8, 1.5),
            driver,
        }
    }

    /// The IDM, written out once more, independently of the implementation: this is the
    /// "hand computation" the tests below compare against.
    #[allow(clippy::too_many_arguments)]
    fn by_hand(v: f64, v0: f64, gap: f64, v_lead: f64, a: f64, b: f64, t: f64, s0: f64) -> f64 {
        // Written with multiplications rather than `powi` or `powf`, so the check is
        // exactly the arithmetic a reader would do by hand and needs no library at all
        // beyond the exactly rounded `sqrt`. The `max(0, ·)` is the model's own domain
        // (`s*` is a gap); every state below has a positive interaction term anyway, so it
        // changes none of the pinned values.
        let s_star = s0 + (t * v + v * (v - v_lead) / (2.0 * math::sqrt(a * b))).max(0.0);
        a * (1.0 - sq(sq(v / v0)) - sq(s_star / gap))
    }

    /// `x²`, written out.
    fn sq(x: f64) -> f64 {
        x * x
    }

    #[test]
    fn kesting_2010_car_matches_a_hand_computation() {
        let m = Idm::new(IdmPreset::Kesting2010);
        let d = m.profile(VehicleClass::Passenger);
        assert_eq!(
            (
                d.desired_speed_mps,
                d.time_headway_s,
                d.max_accel_mps2,
                d.comfort_decel_mps2,
                d.min_gap_m
            ),
            (33.3, 1.5, 1.4, 2.0, 2.0)
        );
        // v = 20 m/s, net gap 30 m, leader at 18 m/s (Δv = 2 m/s).
        //   s* = 2.0 + 1.5·20 + 20·2/(2·√(1.4·2.0)) = 43.952 297 …
        //   a  = 1.4·(1 − (20/33.3)⁴ − (s*/30)²) = −1.787 195 …
        let want = by_hand(20.0, 33.3, 30.0, 18.0, 1.4, 2.0, 1.5, 2.0);
        assert!(
            (want - (-1.7871951713)).abs() < 1e-9,
            "the hand value itself: {want}"
        );
        let got = m.accel_of(20.0, 33.3, 30.0, 18.0, &d);
        assert!((got - want).abs() < 1e-12, "got {got}, want {want}");
        // And through the trait, on a motorway lane in clear weather.
        let got2 = m.accel(
            &ego(20.0, d),
            Some(&LeaderView::virtual_obstacle(30.0, 18.0)),
            &lane(33.3),
            &WeatherState::CLEAR,
        );
        assert!((got2 - want).abs() < 1e-12);
    }

    #[test]
    fn kesting_2010_truck_matches_a_hand_computation() {
        let m = Idm::new(IdmPreset::Kesting2010);
        let d = m.profile(VehicleClass::Truck);
        assert_eq!(
            (
                d.desired_speed_mps,
                d.time_headway_s,
                d.max_accel_mps2,
                d.comfort_decel_mps2,
                d.min_gap_m
            ),
            (23.6, 2.0, 0.7, 2.0, 4.0)
        );
        // v = 15 m/s, gap 40 m, leader 15 m/s (Δv = 0): s* = 4 + 30 = 34.
        //   a = 0.7·(1 − (15/23.6)⁴ − (34/40)²) = 0.080 010 …
        let want = by_hand(15.0, 23.6, 40.0, 15.0, 0.7, 2.0, 2.0, 4.0);
        assert!(
            (want - 0.0800108234).abs() < 1e-9,
            "the hand value itself: {want}"
        );
        assert!((m.accel_of(15.0, 23.6, 40.0, 15.0, &d) - want).abs() < 1e-12);
    }

    #[test]
    fn treiber_2000_matches_a_hand_computation() {
        let m = Idm::new(IdmPreset::Treiber2000);
        let d = m.profile(VehicleClass::Passenger);
        assert_eq!(
            (
                d.desired_speed_mps,
                d.time_headway_s,
                d.max_accel_mps2,
                d.comfort_decel_mps2,
                d.min_gap_m
            ),
            (33.3, 1.6, 0.73, 1.67, 2.0)
        );
        // v = 25 m/s, gap 60 m, leader 24 m/s (Δv = 1 m/s).
        let want = by_hand(25.0, 33.3, 60.0, 24.0, 0.73, 1.67, 1.6, 2.0);
        assert!(
            (want - (-0.0784293045)).abs() < 1e-9,
            "the hand value itself: {want}"
        );
        assert!((m.accel_of(25.0, 33.3, 60.0, 24.0, &d) - want).abs() < 1e-12);
    }

    #[test]
    fn kesting_2007_matches_a_hand_computation() {
        let m = Idm::new(IdmPreset::Kesting2007);
        let d = m.profile(VehicleClass::Passenger);
        assert_eq!(
            (
                d.desired_speed_mps,
                d.time_headway_s,
                d.max_accel_mps2,
                d.comfort_decel_mps2,
                d.min_gap_m
            ),
            (33.3, 1.2, 1.5, 2.0, 2.0)
        );
        // v = 30 m/s, gap 50 m, leader 28 m/s.
        let want = by_hand(30.0, 33.3, 50.0, 28.0, 1.5, 2.0, 1.2, 2.0);
        assert!(
            (want - (-1.3243116295)).abs() < 1e-9,
            "the hand value itself: {want}"
        );
        assert!((m.accel_of(30.0, 33.3, 50.0, 28.0, &d) - want).abs() < 1e-12);
    }

    #[test]
    fn the_legacy_preset_reproduces_the_legacy_port() {
        let m = Idm::new(IdmPreset::Legacy);
        let d = m.profile(VehicleClass::Passenger);
        assert_eq!(
            (
                d.time_headway_s,
                d.min_gap_m,
                d.max_accel_mps2,
                d.comfort_decel_mps2
            ),
            (1.3, 2.5, 1.8, 2.5)
        );
        // The legacy port, transcribed from run.py L2274-2283 for v = 12, gap 20,
        // leader 10, v0 = 13:
        //   gap_b  = max(0.5, 20) = 20
        //   s*     = 2.5 + max(0, 1.3·12 + 12·2/(2·√(1.8·2.5))) = 2.5 + 15.6 + 5.6568…
        //   a      = 1.8·(1 − (12/13)⁴ − (s*/20)²), clamped to [−6, 1.8]
        let s_star = 2.5 + (1.3 * 12.0 + 12.0 * 2.0 / (2.0 * math::sqrt(1.8 * 2.5))).max(0.0);
        let want = (1.8 * (1.0 - sq(sq(12.0 / 13.0)) - sq(s_star / 20.0))).clamp(-6.0, 1.8);
        assert!(
            (want - (-2.0465915557)).abs() < 1e-9,
            "the hand value itself: {want}"
        );
        assert!((m.accel_of(12.0, 13.0, 20.0, 10.0, &d) - want).abs() < 1e-12);
        // The legacy clamps: the gap floor, the v0 floor and the −6 floor.
        assert_eq!(m.params().gap_floor_m, 0.5);
        assert_eq!(m.params().v0_floor_mps, 0.1);
        assert!(m.params().clamp_s_star);
        let slam = m.accel_of(30.0, 13.0, 0.2, 0.0, &d);
        assert_eq!(slam, -6.0, "a closing gap saturates the legacy floor");
    }

    #[test]
    fn a_leader_pulling_away_never_makes_the_ego_brake() {
        // The defect this pins: `clamp_s_star` was false in `IdmParams::default()`, so the
        // three published presets ran the interaction term of `s*` unfloored. It goes
        // negative once the leader is far enough faster, and `(s*/s)²` then turns the
        // desire to close the gap back into braking. The state below is the one the
        // validator measured: ego at 5 m/s, leader 25 m ahead at 30 m/s, which returned
        // −0.338 m/s² with the shipped default.
        for preset in IdmPreset::ALL {
            let m = Idm::new(preset);
            assert!(
                m.params().clamp_s_star,
                "{preset:?} ships with s* unfloored"
            );
            for class in [VehicleClass::Passenger, VehicleClass::Truck] {
                let d = m.profile(class);
                let v0 = d.desired_speed_mps;
                let a = m.accel_of(5.0, v0, 25.0, 30.0, &d);
                assert!(
                    a > 0.0,
                    "{preset:?}/{class:?}: a = {a} m/s² with the leader 25 m ahead and \
                     25 m/s faster"
                );
                // With `s*` floored to `s0` the answer is exactly the free road less
                // `a_max·(s0/s)²` — which pins *where* the floor is applied, not just that
                // the sign came out right.
                let free = m.accel_of(5.0, v0, f64::INFINITY, 0.0, &d);
                let want = free - d.max_accel_mps2 * sq(d.min_gap_m / 25.0);
                assert!(
                    (a - want).abs() < 1e-12,
                    "{preset:?}/{class:?}: got {a}, want {want}"
                );
                // And the general property: a faster leader is never worse than a slower
                // one. Unfloored, the acceleration turns back down as the leader speeds up.
                let mut previous = f64::NEG_INFINITY;
                let mut v_lead = 5.0;
                while v_lead <= 45.0 {
                    let a = m.accel_of(5.0, v0, 25.0, v_lead, &d);
                    assert!(
                        a >= previous - 1e-12,
                        "{preset:?}/{class:?}: a leader at {v_lead} m/s gives {a} m/s², \
                         worse than the slower leader's {previous}"
                    );
                    previous = a;
                    v_lead += 0.5;
                }
            }
        }
    }

    #[test]
    fn the_unfloored_form_is_still_reachable_and_still_wrong() {
        // The flag is a parameter, not a constant, and the card says what turning it off
        // does. This is the artefact the default now avoids, kept as an executable
        // statement of it rather than as prose.
        let d = IdmPreset::Kesting2010.profile(VehicleClass::Passenger);
        let unfloored = Idm::with_params(
            IdmPreset::Kesting2010,
            IdmParams {
                clamp_s_star: false,
                ..IdmParams::default()
            },
        );
        // The hand computation, unfloored, at the state the validator measured.
        let s_star = 2.0 + 1.5 * 5.0 + 5.0 * (5.0 - 30.0) / (2.0 * math::sqrt(1.4 * 2.0));
        assert!(s_star < 0.0, "s* = {s_star} m, a negative gap");
        let want = 1.4 * (1.0 - sq(sq(5.0 / 33.3)) - sq(s_star / 25.0));
        assert!(
            (want - (-0.338)).abs() < 5e-4,
            "the measured −0.338: {want}"
        );
        let a = unfloored.accel_of(5.0, 33.3, 25.0, 30.0, &d);
        assert!((a - want).abs() < 1e-12, "got {a}, want {want}");
        // The three thresholds the card quotes, in the order a leader accelerating away
        // crosses them.
        let floored = Idm::new(IdmPreset::Kesting2010);
        let root = 2.0 * math::sqrt(1.4 * 2.0);
        // (1) the interaction term goes negative and the two forms part company. Does not
        //     depend on the ego's own speed.
        let parts_company = 1.5 * root;
        // (2) s* itself is a negative distance.
        let s_star_negative = |v: f64| (2.0 + 1.5 * v) * root / v;
        // (3) |s*| exceeds s0, so the unfloored form brakes HARDER than the floored one,
        //     and harder still the faster the leader goes.
        let brakes_harder = |v: f64| (2.0 * 2.0 + 1.5 * v) * root / v;
        assert!((parts_company - 5.0).abs() < 0.05, "{parts_company}");
        for (v, negative, harder) in [(5.0, 6.4, 7.7), (20.0, 5.4, 5.7)] {
            assert!(
                (s_star_negative(v) - negative).abs() < 0.05,
                "{}",
                s_star_negative(v)
            );
            assert!(
                (brakes_harder(v) - harder).abs() < 0.05,
                "{}",
                brakes_harder(v)
            );
            // Below the first threshold the floor is inactive and the two agree exactly.
            let below = v + parts_company - 0.1;
            assert!(
                (unfloored.accel_of(v, 33.3, 25.0, below, &d)
                    - floored.accel_of(v, 33.3, 25.0, below, &d))
                .abs()
                    < 1e-12,
                "v = {v}: the floor is active below Δv = −{parts_company}"
            );
            // Above it they differ.
            let above = v + parts_company + 0.1;
            assert!(
                (unfloored.accel_of(v, 33.3, 25.0, above, &d)
                    - floored.accel_of(v, 33.3, 25.0, above, &d))
                .abs()
                    > 1e-9,
                "v = {v}: the floor does nothing above Δv = −{parts_company}"
            );
            // At the second, s* is negative: a desired minimum gap of less than nothing.
            let dv = -(s_star_negative(v) + 0.1);
            let s_star = 2.0 + 1.5 * v + v * dv / root;
            assert!(s_star < 0.0, "v = {v}: s* = {s_star}");
            // Past the third the unfloored model turns down as the leader speeds up while
            // the floored one does not move at all: s* is pinned at s0.
            let inverted = v + brakes_harder(v) + 0.1;
            let a1 = unfloored.accel_of(v, 33.3, 25.0, inverted, &d);
            let a2 = unfloored.accel_of(v, 33.3, 25.0, inverted + 5.0, &d);
            assert!(a2 < a1, "v = {v}: {a2} is not worse than {a1}");
            assert!(
                floored.accel_of(v, 33.3, 25.0, inverted, &d) > a1,
                "v = {v}: the floor does not help here"
            );
            assert_eq!(
                floored.accel_of(v, 33.3, 25.0, inverted, &d),
                floored.accel_of(v, 33.3, 25.0, inverted + 5.0, &d)
            );
        }
    }

    #[test]
    fn the_free_road_limit_is_the_free_term() {
        let m = Idm::new(IdmPreset::Kesting2010);
        let d = m.profile(VehicleClass::Passenger);
        let want = 1.4 * (1.0 - sq(sq(20.0 / 33.3)));
        assert!((m.accel_of(20.0, 33.3, f64::INFINITY, 0.0, &d) - want).abs() < 1e-12);
        // At the desired speed a free road gives exactly zero.
        assert!(m.accel_of(33.3, 33.3, f64::INFINITY, 0.0, &d).abs() < 1e-12);
        // And a vehicle at rest on a free road accelerates at a_max.
        assert!((m.accel_of(0.0, 33.3, f64::INFINITY, 0.0, &d) - 1.4).abs() < 1e-12);
    }

    #[test]
    fn the_lane_limit_caps_the_desired_speed() {
        let m = Idm::new(IdmPreset::Kesting2010);
        let d = m.profile(VehicleClass::Passenger);
        // A 13.89 m/s urban lane, a driver who wants 33.3: at 13.89 the vehicle is already
        // at its effective desired speed, so a free road gives zero.
        let a = m.accel(&ego(13.89, d), None, &lane(13.89), &WeatherState::CLEAR);
        assert!(a.abs() < 1e-12, "got {a}");
    }

    #[test]
    fn weather_slows_the_driver_and_stretches_the_headway() {
        let m = Idm::new(IdmPreset::Kesting2010);
        let d = m.profile(VehicleClass::Passenger);
        let snow = WeatherState {
            kind: v2xw_core::weather::WeatherKind::Snow,
            intensity: 0.9,
            visibility_m: 120.0,
            surface: v2xw_core::weather::SurfaceCondition::Snow,
        };
        let clear = m.accel(&ego(25.0, d), None, &lane(33.3), &WeatherState::CLEAR);
        let snowy = m.accel(&ego(25.0, d), None, &lane(33.3), &snow);
        assert!(
            snowy < clear,
            "heavy snow lowers the desired speed: {snowy} vs {clear}"
        );
        let leader = LeaderView::virtual_obstacle(30.0, 25.0);
        let clear_gap = m.accel(
            &ego(25.0, d),
            Some(&leader),
            &lane(33.3),
            &WeatherState::CLEAR,
        );
        let snowy_gap = m.accel(&ego(25.0, d), Some(&leader), &lane(33.3), &snow);
        assert!(snowy_gap < clear_gap, "and stretches the headway");
    }

    #[test]
    fn fog_slows_the_driver_and_ice_limits_the_braking() {
        let m = Idm::new(IdmPreset::Kesting2010);
        let d = m.profile(VehicleClass::Passenger);
        // 40 m of fog on a 33 m/s road: a driver at 20 m/s is above the ≈ 10.3 m/s it can
        // stop from within 40 m, and slows.
        let fog = WeatherState::new(
            v2xw_core::weather::WeatherKind::Fog,
            0.5,
            40.0,
            v2xw_core::weather::SurfaceCondition::Dry,
        );
        assert!(m.accel(&ego(20.0, d), None, &lane(33.3), &WeatherState::CLEAR) > 0.0);
        assert!(m.accel(&ego(20.0, d), None, &lane(33.3), &fog) < 0.0);
        // On ice nothing brakes harder than μ·g ≈ 0.98 m/s², however close the obstacle.
        let ice = WeatherState::new(
            v2xw_core::weather::WeatherKind::Clear,
            0.0,
            f64::INFINITY,
            v2xw_core::weather::SurfaceCondition::Ice,
        );
        let obstacle = LeaderView::virtual_obstacle(5.0, 0.0);
        let dry = m.accel(
            &ego(15.0, d),
            Some(&obstacle),
            &lane(15.0),
            &WeatherState::CLEAR,
        );
        let icy = m.accel(&ego(15.0, d), Some(&obstacle), &lane(15.0), &ice);
        assert!(dry < -3.0, "{dry}");
        assert!((icy + 0.980_665).abs() < 1e-9, "{icy}");
    }

    #[test]
    fn the_enhancement_softens_a_cut_in() {
        let base = Idm::new(IdmPreset::Kesting2010);
        let mut params = *base.params();
        params.enhanced = true;
        let acc = Idm::with_params(IdmPreset::Kesting2010, params);
        let d = base.profile(VehicleClass::Passenger);
        // A leader cuts in 8 m ahead at the same speed: the base IDM brakes hard.
        let hard = base.accel_of(28.0, 33.3, 8.0, 28.0, &d);
        let soft = acc.accel_of(28.0, 33.3, 8.0, 28.0, &d);
        assert!(hard < -3.0, "the base model over-reacts: {hard}");
        assert!(
            soft > hard,
            "the enhancement is not harsher: {soft} vs {hard}"
        );
    }

    #[test]
    fn every_preset_has_a_valid_card_and_a_source() {
        for p in IdmPreset::ALL {
            let m = Idm::new(p);
            m.card().validate().expect("card validates");
            assert_eq!(m.card().id, MODEL_ID);
            assert!(!m.card().parameters.is_empty());
            for param in &m.card().parameters {
                if param.source.kind == SourceKind::TodoCalibrate {
                    assert!(
                        param
                            .calibration
                            .as_ref()
                            .is_some_and(|c| !c.trim().is_empty()),
                        "{} needs a plan",
                        param.name
                    );
                }
            }
        }
    }

    #[test]
    fn the_result_is_always_inside_the_clamps() {
        let m = Idm::new(IdmPreset::Kesting2010);
        let d = m.profile(VehicleClass::Passenger);
        for v in [0.0, 5.0, 20.0, 40.0] {
            for gap in [0.0, 0.3, 2.0, 50.0, 1e6] {
                for v_lead in [0.0, 10.0, 40.0] {
                    let a = m.accel_of(v, 33.3, gap, v_lead, &d);
                    assert!(a.is_finite(), "v={v} gap={gap} v_lead={v_lead}");
                    assert!((-6.0..=1.4).contains(&a), "a={a} out of the clamps");
                }
            }
        }
    }
}
