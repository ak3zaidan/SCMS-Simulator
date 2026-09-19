//! `mobility/lane-change/mobil` — MOBIL (04-models.md §2.2).
//!
//! # The criteria, as §2.2 states them
//!
//! ```text
//! safety:     ã_n ≥ −b_safe                                        (new follower after the change)
//! incentive:  (ã_c − a_c) + p·[(ã_n − a_n) + (ã_o − a_o)] + Δa_bias > Δa_th
//! ```
//!
//! `Δa_bias` is zero unless the asymmetric (European) rule is enabled, and then it is
//! `+right_bias` for a change **to the right** and `−right_bias` for one to the left — on
//! the *left-hand side*, so a bias makes moving right **easier** and moving left harder,
//! which is what the keep-right rule of Kesting 2007 says. Written on the right-hand side
//! against the threshold it would read the other way round and make moving right harder,
//! which is what an earlier printing of this card said and the code never did.
//!
//! `c` is the ego, `n` the vehicle that would follow it in the target lane and `o` the
//! vehicle following it in the lane it leaves. A tilde is the acceleration *after* the
//! hypothetical change. Every one of those six accelerations comes from the car-following
//! model — MOBIL is defined on top of one, which is the point of the paper's title — and
//! each is evaluated with **that vehicle's own** driver parameters, not the ego's.
//!
//! # The six gaps
//!
//! With net gaps (leader rear to follower front) and the ego's length `L_c`:
//!
//! ```text
//! ego after       : gap to the target lane's leader                = tgt_leader.gap
//! new follower after : gap to the ego                              = tgt_follower.gap
//! new follower before: gap to the target leader, ego absent        = tgt_follower.gap + L_c + tgt_leader.gap
//! old follower before: gap to the ego                              = cur_follower.gap
//! old follower after : gap to the old leader, ego gone             = cur_follower.gap + L_c + cur_leader.gap
//! ```
//!
//! The two "with the ego absent/gone" gaps are the ones that make the politeness term mean
//! anything, and they are why the neighbour query has to return followers as well as
//! leaders.
//!
//! # One neighbour query
//!
//! §2.2 requires the neighbour classification to share the car-following leader search.
//! It does: [`Mobil::decide`] reads a [`LaneNeighbors`] that
//! [`crate::snapshot::ActorSnapshot::neighbors`] filled once for this actor this step, and
//! it performs no search of its own. The lateral window and heading filter the legacy
//! engine used to classify neighbours geometrically [`run.py` L2306-2318] are replaced by
//! the lane graph itself, which cannot mistake an oncoming vehicle for a neighbour.
//!
//! # State
//!
//! MOBIL is stateless: `decide` takes `&self`. The two pieces of per-vehicle state §2.2
//! lists — the cooldown and the lateral transition — belong to the actor and live in
//! [`crate::engine`], which asks this model only when the cooldown has expired.
//! [`transition_duration`] and [`smoothstep`] are the shape functions it uses, kept here
//! with the parameters they come from.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::math;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::Duration;
use v2xw_core::weather::WeatherState;

use crate::ctx::MobCtx;
use crate::traits::{CarFollowing, LaneChange};
use crate::views::{
    LaneChangeDecision, LaneNeighbors, LaneView, LeaderView, Side, SideNeighbors, VehicleView,
};

/// The model id.
pub const MODEL_ID: &str = "mobility/lane-change/mobil";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The legacy heading-deviation cap for a lane-change transition, degrees
/// (`_LC_MAXDEV_TAN` = tan 12°, [`run.py` L2258]).
pub const LEGACY_MAX_HEADING_DEV_DEG: f64 = 12.0;

/// MOBIL's parameters (§2.2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MobilParams {
    /// Politeness `p`: how much the ego weighs the acceleration it imposes on others.
    /// `0` is egoistic, `1` is "ideal MOBIL", negative is malicious.
    pub politeness: f64,
    /// Changing threshold `Δa_th`, m/s².
    pub threshold_mps2: f64,
    /// Safety limit `b_safe`, m/s²: the new follower may not be forced to brake harder.
    pub b_safe_mps2: f64,
    /// Right-lane bias `Δa_bias`, m/s², for the asymmetric (European) rule.
    pub right_bias_mps2: f64,
    /// Whether to apply the asymmetric rule at all. The legacy engine is symmetric.
    pub asymmetric: bool,
    /// Minimum speed for a discretionary change, m/s.
    pub min_speed_mps: f64,
    /// Whether a change requires the ego's own lane to be blocked by a leader within the
    /// lookahead, as the legacy engine required [`run.py` L2327].
    pub require_blocked_leader: bool,
    /// Nominal lateral transition duration, seconds.
    pub transition_s: f64,
    /// Cap on the heading deviation the transition may produce, radians.
    pub max_heading_dev_rad: f64,
    /// Cooldown factor: the cooldown is `max(factor · transition_s, factor · duration)`.
    pub cooldown_factor: f64,
    /// Reconsideration rate, per second: the probability of reconsidering in one step is
    /// `min(1, rate · step_s)`.
    pub reconsider_rate_per_s: f64,
    /// The mobility step length, seconds, which the reconsideration probability needs.
    /// Set by the engine from its own step so the two cannot disagree.
    pub step_s: f64,
}

impl Default for MobilParams {
    fn default() -> Self {
        MobilPreset::Kesting2007.params()
    }
}

/// The two cited parameter sets of §2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MobilPreset {
    /// Kesting, Treiber and Helbing 2007 [R10 §B3], with the politeness default §2.2
    /// prescribes (0.2, `code (legacy)`, `TODO: calibrate`).
    #[default]
    Kesting2007,
    /// The frozen reference engine's set, including its transition, cooldown and
    /// reconsideration rules (`code (legacy)` [`run.py` L2251-2258, L2361-2401]).
    Legacy,
}

impl MobilPreset {
    /// Both presets.
    pub const ALL: [MobilPreset; 2] = [MobilPreset::Kesting2007, MobilPreset::Legacy];

    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            MobilPreset::Kesting2007 => "kesting-2007",
            MobilPreset::Legacy => "legacy",
        }
    }

    /// The parameters of this set.
    pub fn params(self) -> MobilParams {
        let max_dev = LEGACY_MAX_HEADING_DEV_DEG * core::f64::consts::PI / 180.0;
        match self {
            MobilPreset::Kesting2007 => MobilParams {
                politeness: 0.2,
                threshold_mps2: 0.1,
                b_safe_mps2: 4.0,
                right_bias_mps2: 0.3,
                asymmetric: false,
                min_speed_mps: 3.0,
                require_blocked_leader: false,
                transition_s: 2.5,
                max_heading_dev_rad: max_dev,
                cooldown_factor: 2.0,
                reconsider_rate_per_s: 0.25,
                step_s: 0.1,
            },
            MobilPreset::Legacy => MobilParams {
                politeness: 0.2,
                threshold_mps2: 0.2,
                b_safe_mps2: 4.0,
                right_bias_mps2: 0.0,
                asymmetric: false,
                min_speed_mps: 3.0,
                require_blocked_leader: true,
                transition_s: 2.5,
                max_heading_dev_rad: max_dev,
                cooldown_factor: 2.0,
                reconsider_rate_per_s: 0.25,
                step_s: 1.0,
            },
        }
    }
}

/// The smoothstep profile `p²(3 − 2p)` the lateral transition follows
/// ([`run.py` L2361-2401]).
///
/// Zero lateral velocity at both ends, so a lane change starts and finishes without a
/// sideways jerk, and a peak lateral velocity of `1.5·Δ/T` in the middle — the number
/// [`transition_duration`] solves against the heading-deviation cap.
pub fn smoothstep(p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

/// How long a lateral displacement of `lateral_m` takes at `speed_mps` (§2.2).
///
/// The nominal duration, stretched just enough that the smoothstep's peak lateral velocity
/// `1.5·Δ/T` keeps the heading deviation within the cap:
/// `T = max(transition_s, 1.5·Δ / (tan(dev_max) · max(v, 0.5)))`. A slow vehicle therefore
/// changes lane gently over several seconds instead of lurching sideways, which is the
/// behaviour the legacy engine's comment describes and the reason the rule exists.
pub fn transition_duration(params: &MobilParams, lateral_m: f64, speed_mps: f64) -> Duration {
    let tan_dev = math::tan(params.max_heading_dev_rad);
    let stretched = 1.5 * lateral_m.abs() / (tan_dev * speed_mps.max(0.5));
    Duration::from_secs_f64(params.transition_s.max(stretched))
}

/// MOBIL, on top of a car-following model.
#[derive(Clone)]
pub struct Mobil {
    params: MobilParams,
    preset: MobilPreset,
    cf: Arc<dyn CarFollowing + Send + Sync>,
    card: ModelCard,
}

impl core::fmt::Debug for Mobil {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mobil")
            .field("preset", &self.preset)
            .field("params", &self.params)
            .field("car_following", &self.cf.card().id)
            .finish()
    }
}

impl Mobil {
    /// MOBIL with a cited preset, scoring accelerations with `cf`.
    pub fn new(preset: MobilPreset, cf: Arc<dyn CarFollowing + Send + Sync>) -> Self {
        Self::with_params(preset, preset.params(), cf)
    }

    /// MOBIL with parameters of its own.
    pub fn with_params(
        preset: MobilPreset,
        params: MobilParams,
        cf: Arc<dyn CarFollowing + Send + Sync>,
    ) -> Self {
        Self {
            card: card(preset, &params, cf.card().id.clone()),
            params,
            preset,
            cf,
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &MobilParams {
        &self.params
    }

    /// Which cited set this instance carries.
    pub fn preset(&self) -> MobilPreset {
        self.preset
    }

    /// The cooldown after a change of duration `dur` (§2.2: `max(2·T_lc, 2·dur)`).
    pub fn cooldown(&self, dur: Duration) -> Duration {
        let by_nominal = self.params.cooldown_factor * self.params.transition_s;
        let by_actual = self.params.cooldown_factor * dur.as_secs_f64();
        Duration::from_secs_f64(by_nominal.max(by_actual))
    }

    /// The probability of reconsidering the lane in one step: `min(1, rate · step_s)`.
    pub fn reconsider_probability(&self) -> f64 {
        (self.params.reconsider_rate_per_s * self.params.step_s).clamp(0.0, 1.0)
    }

    /// The acceleration `veh` would have behind `leader` on a lane of limit
    /// `speed_limit_mps`.
    fn accel_of(
        &self,
        veh: &VehicleView,
        leader: Option<&LeaderView>,
        lane: &LaneView,
        w: &WeatherState,
    ) -> f64 {
        self.cf.accel(veh, leader, lane, w)
    }

    /// The incentive and safety verdict for one side, or `None` when the change is unsafe
    /// or impossible.
    ///
    /// Returns the total incentive including the asymmetric bias; the caller compares it
    /// against the threshold.
    fn score(
        &self,
        ego: &VehicleView,
        nbrs: &LaneNeighbors,
        target: &SideNeighbors,
        ego_lane: &LaneView,
        target_lane: &LaneView,
        w: &WeatherState,
    ) -> Option<f64> {
        let length = ego.dims.length_m;

        // --- the ego itself ------------------------------------------------
        let a_c = self.accel_of(ego, nbrs.leader.as_ref(), ego_lane, w);
        let mut ego_after = *ego;
        ego_after.lane = target.lane;
        ego_after.lane_index = match target.side {
            Side::Left => ego.lane_index.saturating_add(1),
            Side::Right => ego.lane_index.saturating_sub(1),
        };
        let a_c_after = self.accel_of(&ego_after, target.leader.as_ref(), target_lane, w);

        // --- the new follower, in the target lane -------------------------
        let mut d_new = 0.0;
        if let Some(follower) = target.follower.as_ref().and_then(|f| f.vehicle.as_ref()) {
            let gap_to_ego = target.follower.as_ref().expect("checked above").gap_m;
            let ego_as_leader = LeaderView::of(*ego, gap_to_ego);
            let a_n_after = self.accel_of(follower, Some(&ego_as_leader), target_lane, w);
            // The safety criterion. §2.2: ã_n ≥ −b_safe.
            if a_n_after < -self.params.b_safe_mps2 {
                return None;
            }
            let a_n_before = match target.leader.as_ref() {
                Some(l) => {
                    let gap_before = gap_to_ego + length + l.gap_m;
                    self.accel_of(follower, Some(&l.at_gap(gap_before)), target_lane, w)
                }
                None => self.accel_of(follower, None, target_lane, w),
            };
            d_new = a_n_after - a_n_before;
        }

        // --- the old follower, in the lane the ego leaves -----------------
        let mut d_old = 0.0;
        if let Some(follower) = nbrs.follower.as_ref().and_then(|f| f.vehicle.as_ref()) {
            let gap_to_ego = nbrs.follower.as_ref().expect("checked above").gap_m;
            let ego_as_leader = LeaderView::of(*ego, gap_to_ego);
            let a_o_before = self.accel_of(follower, Some(&ego_as_leader), ego_lane, w);
            let a_o_after = match nbrs.leader.as_ref() {
                Some(l) => {
                    let gap_after = gap_to_ego + length + l.gap_m;
                    self.accel_of(follower, Some(&l.at_gap(gap_after)), ego_lane, w)
                }
                None => self.accel_of(follower, None, ego_lane, w),
            };
            d_old = a_o_after - a_o_before;
        }

        let bias = if self.params.asymmetric {
            match target.side {
                Side::Right => self.params.right_bias_mps2,
                Side::Left => -self.params.right_bias_mps2,
            }
        } else {
            0.0
        };
        Some((a_c_after - a_c) + self.params.politeness * (d_new + d_old) + bias)
    }
}

impl v2xw_core::model::Model for Mobil {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl LaneChange for Mobil {
    fn decide(
        &self,
        ctx: &mut dyn MobCtx,
        ego: &VehicleView,
        nbrs: &LaneNeighbors,
        w: &WeatherState,
    ) -> LaneChangeDecision {
        // A dead crawl is not the place for a discretionary change: the heading swing
        // would be implausible and the incentive is dominated by noise.
        if ego.speed_mps < self.params.min_speed_mps {
            return LaneChangeDecision::Stay;
        }
        if nbrs.left.is_none() && nbrs.right.is_none() {
            return LaneChangeDecision::Stay;
        }
        // The legacy set only reconsiders a blocked lane.
        if self.params.require_blocked_leader && nbrs.leader.is_none() {
            return LaneChangeDecision::Stay;
        }
        // Drivers reconsider intermittently, which staggers changes instead of having the
        // whole platoon decide on the same step. One draw from this actor's own
        // lane-change stream, so the outcome does not depend on how many other actors
        // drew this step.
        let p = self.reconsider_probability();
        if p < 1.0 {
            let keep = ctx
                .rng(RngDomain::LaneChange, EntityRef::Actor(ego.actor))
                .bool(p);
            if !keep {
                return LaneChangeDecision::Stay;
            }
        }

        let world = ctx.world();
        let Some(lane) = world.try_lane(ego.lane) else {
            return LaneChangeDecision::Stay;
        };
        let ego_lane = LaneView::of(lane);
        let mut best: Option<(f64, Side, LaneView)> = None;
        // Left before right, and the tie goes to the lane the earlier side found: a total,
        // documented order, so two engines pick the same lane when the incentives are
        // numerically equal.
        for side in [Side::Left, Side::Right] {
            let Some(target) = nbrs.side(side) else {
                continue;
            };
            let Some(target_lane) = world.try_lane(target.lane).map(LaneView::of) else {
                continue;
            };
            let Some(incentive) = self.score(ego, nbrs, target, &ego_lane, &target_lane, w) else {
                continue;
            };
            if incentive <= self.params.threshold_mps2 {
                continue;
            }
            if best.as_ref().is_none_or(|(b, _, _)| incentive > *b) {
                best = Some((incentive, side, target_lane));
            }
        }
        match best {
            None => LaneChangeDecision::Stay,
            Some((incentive, side, target_lane)) => {
                let lateral = 0.5 * (ego_lane.width_m + target_lane.width_m);
                LaneChangeDecision::Change {
                    to: target_lane.id,
                    side,
                    incentive_mps2: incentive,
                    duration: transition_duration(&self.params, lateral, ego.speed_mps),
                }
            }
        }
    }
}

/// The model card.
pub fn card(preset: MobilPreset, params: &MobilParams, car_following: String) -> ModelCard {
    let kesting = Source {
        kind: SourceKind::Paper,
        reference: "Kesting, Treiber & Helbing 2007, Transportation Research Record 1999, \
                    86-94 (DOI 10.3141/1999-10) [R10 §B3]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L2251-2258, L2306-2401".to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: Some("the reference engine's MOBIL, transition and cooldown".to_string()),
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "Discretionary lane changes by MOBIL: a safety criterion on the acceleration the \
         change imposes on the new follower, and an incentive criterion that weighs the \
         ego's own gain against a politeness-weighted sum of the two followers' losses. \
         Every acceleration comes from the car-following model this instance was built \
         with, evaluated with each vehicle's own driver parameters.",
    );
    card.tier = vec![Tier::Medium];
    card.equations = vec![
        Equation {
            name: "safety".to_string(),
            latex_or_text: "ã_n ≥ −b_safe".to_string(),
            notes: Some("the new follower's acceleration after the change".to_string()),
        },
        Equation {
            name: "incentive".to_string(),
            latex_or_text: "(ã_c − a_c) + p·[(ã_n − a_n) + (ã_o − a_o)] + Δa_bias > Δa_th"
                .to_string(),
            notes: Some(
                "c ego, n new follower, o old follower. Δa_bias sits on the LEFT, with the \
                 gain: it is zero unless the asymmetric rule is enabled, and then it is \
                 +right_bias for a change to the right and −right_bias for one to the \
                 left, so the bias makes moving right EASIER — the keep-right rule of \
                 Kesting 2007. Printed on the right against the threshold it would read \
                 the other way round."
                    .to_string(),
            ),
        },
        Equation {
            name: "lateral transition".to_string(),
            latex_or_text: "d(t) = d0 + (d1 − d0)·p²(3 − 2p),  p = elapsed/T,  \
                            T = max(T_lc, 1.5·Δ/(tan(dev_max)·max(v, 0.5)))"
                .to_string(),
            notes: Some(
                "smoothstep, so the lateral velocity is zero at both ends; the stretch \
                 keeps the peak lateral velocity 1.5·Δ/T inside the heading-deviation cap"
                    .to_string(),
            ),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "preset",
            "-",
            serde_json::json!(preset.label()),
            kesting.clone(),
        ),
        Parameter::new(
            "car_following",
            "-",
            serde_json::json!(car_following),
            Source::new(
                SourceKind::Paper,
                "Kesting 2007: MOBIL is defined on top of a car-following model [R10 §B3]",
            ),
        ),
        Parameter {
            name: "politeness".to_string(),
            unit: "1".to_string(),
            default: serde_json::json!(params.politeness),
            range: Some(vec![serde_json::json!(-1.0), serde_json::json!(1.0)]),
            source: Source::todo_calibrate(
                "Kesting 2007 sweeps p from 0 to 1 and states no single default; 0.2 is the \
                 reference engine's value (`lane_change_politeness`)",
            ),
            calibration: Some(
                "04-models.md §2.2's plan, unchanged: match the lane-change rate band of \
                 450-1400 changes/h/km at 10-15 veh/km/lane that Kesting 2007 reports \
                 (about 1100-1400 at p = 0 against 450-600 at p = 1), by sweeping p on the \
                 fundamental-diagram ring of §2.9 and counting changes per hour per \
                 kilometre."
                    .to_string(),
            ),
        },
        Parameter::new(
            "threshold",
            "m/s²",
            serde_json::json!(params.threshold_mps2),
            if preset == MobilPreset::Legacy {
                legacy.clone()
            } else {
                kesting.clone()
            },
        ),
        Parameter::new(
            "b_safe",
            "m/s²",
            serde_json::json!(params.b_safe_mps2),
            kesting.clone(),
        ),
        Parameter::new(
            "right_bias",
            "m/s²",
            serde_json::json!(params.right_bias_mps2),
            kesting.clone(),
        ),
        Parameter::new(
            "asymmetric",
            "-",
            serde_json::json!(params.asymmetric),
            kesting.clone(),
        ),
        Parameter::new(
            "min_speed",
            "m/s",
            serde_json::json!(params.min_speed_mps),
            legacy.clone(),
        ),
        Parameter::new(
            "require_blocked_leader",
            "-",
            serde_json::json!(params.require_blocked_leader),
            legacy.clone(),
        ),
        Parameter::new(
            "transition",
            "s",
            serde_json::json!(params.transition_s),
            legacy.clone(),
        ),
        Parameter::new(
            "max_heading_dev",
            "deg",
            serde_json::json!(LEGACY_MAX_HEADING_DEV_DEG),
            legacy.clone(),
        ),
        Parameter::new(
            "cooldown_factor",
            "1",
            serde_json::json!(params.cooldown_factor),
            legacy.clone(),
        ),
        Parameter::new(
            "reconsider_rate",
            "1/s",
            serde_json::json!(params.reconsider_rate_per_s),
            legacy.clone(),
        ),
        Parameter::new("step", "s", serde_json::json!(params.step_s), legacy),
    ];
    card.assumptions = vec![
        "The change is instantaneous for the incentive test and is then integrated \
         laterally over the transition (04-models.md §2.2)."
            .to_string(),
        "Adjacency is within one edge, and an edge is one direction of travel, so a \
         'neighbour' is never an oncoming vehicle."
            .to_string(),
        "The cooldown and the lateral transition are actor state and live in the mobility \
         engine; this model is stateless."
            .to_string(),
    ];
    card.limitations = vec![
        "No cooperative or strategic (route-driven) changes: a vehicle that needs another \
         lane to follow its route is served by the engine's mandatory-change rule, not by \
         this incentive."
            .to_string(),
    ];
    card.ignores = vec![
        "SUMO's cooperative helping, keep-right rule, speed-gain lookahead and sublane \
         model (medium relative to high, 04-models.md §2.2)."
            .to_string(),
    ];
    card.sources = vec![kesting];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::LaneChange.as_str().to_string()],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "lanechange::mobil::tests::a_blocked_lane_with_a_clear_neighbour_invites_a_change"
                .to_string(),
            "lanechange::mobil::tests::an_unsafe_change_is_vetoed".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::carfollowing::idm::{Idm, IdmPreset};
    use crate::classes::VehicleClass;
    use crate::ctx::MobilityCtx;
    use crate::snapshot::{ActorSnapshot, NeighborOptions};
    use crate::views::DriverProfile;
    use v2xw_core::geom::Dims;
    use v2xw_core::ids::{ActorId, LaneId};
    use v2xw_core::model::Model;
    use v2xw_core::rng::RngRegistry;
    use v2xw_world::{ImportOptions, procedural::GridParams};

    fn mobil(preset: MobilPreset) -> Mobil {
        let idm: Arc<dyn CarFollowing + Send + Sync> = Arc::new(Idm::new(IdmPreset::Kesting2010));
        let mut params = preset.params();
        params.step_s = 1.0;
        params.reconsider_rate_per_s = 1.0; // always reconsider, so the test is not a coin flip
        Mobil::with_params(preset, params, idm)
    }

    fn driver() -> DriverProfile {
        IdmPreset::Kesting2010.profile(VehicleClass::Passenger)
    }

    fn vehicle(actor: u32, lane: LaneId, index: u8, s_m: f64, speed: f64) -> VehicleView {
        VehicleView {
            actor: ActorId::new(actor),
            class: VehicleClass::Passenger,
            lane,
            lane_index: index,
            s_m,
            lateral_m: 0.0,
            speed_mps: speed,
            accel_mps2: 0.0,
            heading_rad: 0.0,
            dims: Dims::new(5.0, 1.8, 1.5),
            driver: driver(),
        }
    }

    /// A two-lane-per-direction grid world, so there is a left neighbour to move into.
    fn world() -> v2xw_world::World {
        let params = GridParams {
            lanes_per_direction: 2,
            ..GridParams::legacy()
        };
        v2xw_world::procedural::grid(&params, &ImportOptions::default()).expect("grid")
    }

    /// The first drivable lane of the world with a left neighbour, and that neighbour.
    fn two_lanes(w: &v2xw_world::World) -> (LaneId, LaneId) {
        for lane in w.roads.lanes() {
            if lane.kind != v2xw_world::LaneKind::Driving || lane.index != 0 {
                continue;
            }
            let edge = w.edge(lane.edge);
            if let Some(left) = edge.lanes.iter().find(|l| w.lane(**l).index == 1) {
                return (lane.id, *left);
            }
        }
        panic!("the two-lane grid has no edge with two lanes");
    }

    #[test]
    fn a_blocked_lane_with_a_clear_neighbour_invites_a_change() {
        let w = world();
        let (right, left) = two_lanes(&w);
        let rng = RngRegistry::new(7);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let m = mobil(MobilPreset::Legacy);

        // The ego is stuck behind a slow leader in its own lane; the left lane is empty.
        // Speeds inside the grid world's 13.89 m/s limit, so the free term is not
        // saturated and the incentive is the gap's doing rather than the clamp's.
        let ego = vehicle(0, right, 0, 20.0, 12.0);
        let leader = vehicle(1, right, 0, 28.0, 3.0);
        let snap = ActorSnapshot::build(
            0,
            80.0,
            [
                (
                    ego,
                    v2xw_core::kinematics::Kinematics::at_rest(
                        0,
                        w.to_xyz(&v2xw_core::geom::LanePos::centred(right, 20.0)),
                    ),
                ),
                (
                    leader,
                    v2xw_core::kinematics::Kinematics::at_rest(
                        0,
                        w.to_xyz(&v2xw_core::geom::LanePos::centred(right, 28.0)),
                    ),
                ),
            ],
        );
        let nbrs = snap.neighbors(&w, &ego, &[], 0, NeighborOptions::default());
        assert!(nbrs.leader.is_some(), "the leader is in front");
        assert_eq!(nbrs.left.map(|l| l.lane), Some(left));
        match m.decide(&mut ctx, &ego, &nbrs, &WeatherState::CLEAR) {
            LaneChangeDecision::Change {
                to,
                side,
                incentive_mps2,
                duration,
            } => {
                assert_eq!(to, left);
                assert_eq!(side, Side::Left);
                assert!(incentive_mps2 > m.params().threshold_mps2);
                assert!(duration.as_secs_f64() >= m.params().transition_s);
            }
            other => panic!("expected a change, got {other:?}"),
        }
    }

    #[test]
    fn an_unsafe_change_is_vetoed() {
        let w = world();
        let (right, left) = two_lanes(&w);
        let rng = RngRegistry::new(7);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let m = mobil(MobilPreset::Legacy);

        // Same blocked ego, but a fast vehicle is right behind it in the left lane: the
        // change would force that vehicle to brake harder than b_safe.
        let ego = vehicle(0, right, 0, 20.0, 10.0);
        let leader = vehicle(1, right, 0, 26.0, 2.0);
        let closing = vehicle(2, left, 1, 14.0, 33.0);
        let k = |s: f64, lane: LaneId| {
            v2xw_core::kinematics::Kinematics::at_rest(
                0,
                w.to_xyz(&v2xw_core::geom::LanePos::centred(lane, s)),
            )
        };
        let snap = ActorSnapshot::build(
            0,
            80.0,
            [
                (ego, k(20.0, right)),
                (leader, k(26.0, right)),
                (closing, k(14.0, left)),
            ],
        );
        let nbrs = snap.neighbors(&w, &ego, &[], 0, NeighborOptions::default());
        let side = nbrs.left.expect("a left lane");
        assert!(
            side.follower.is_some(),
            "the fast vehicle is the would-be follower"
        );
        assert_eq!(
            m.decide(&mut ctx, &ego, &nbrs, &WeatherState::CLEAR),
            LaneChangeDecision::Stay,
            "the safety criterion vetoes it"
        );
    }

    #[test]
    fn an_empty_neighbourhood_never_moves() {
        let w = world();
        let (right, _) = two_lanes(&w);
        let rng = RngRegistry::new(1);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let m = mobil(MobilPreset::Legacy);
        let ego = vehicle(0, right, 0, 20.0, 20.0);
        let nbrs = crate::views::LaneNeighbors::empty(right);
        assert_eq!(
            m.decide(&mut ctx, &ego, &nbrs, &WeatherState::CLEAR),
            LaneChangeDecision::Stay
        );
    }

    #[test]
    fn a_crawling_vehicle_never_moves() {
        let w = world();
        let (right, _) = two_lanes(&w);
        let rng = RngRegistry::new(1);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let m = mobil(MobilPreset::Legacy);
        let ego = vehicle(0, right, 0, 20.0, 1.0);
        let nbrs = crate::views::LaneNeighbors::empty(right);
        assert_eq!(
            m.decide(&mut ctx, &ego, &nbrs, &WeatherState::CLEAR),
            LaneChangeDecision::Stay
        );
    }

    #[test]
    fn the_smoothstep_has_zero_velocity_at_both_ends() {
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
        assert!((smoothstep(0.5) - 0.5).abs() < 1e-12);
        // Symmetric, and monotone.
        for i in 0..100 {
            let p = f64::from(i) / 100.0;
            assert!(smoothstep(p) <= smoothstep(p + 0.01) + 1e-12);
            assert!((smoothstep(p) + smoothstep(1.0 - p) - 1.0).abs() < 1e-12);
        }
        // The derivative at the ends is zero: the finite difference shrinks quadratically.
        assert!(smoothstep(0.001) < 1e-5);
        assert!(1.0 - smoothstep(0.999) < 1e-5);
    }

    #[test]
    fn a_slow_vehicle_gets_a_stretched_transition() {
        let p = MobilPreset::Legacy.params();
        let fast = transition_duration(&p, 3.5, 30.0);
        let slow = transition_duration(&p, 3.5, 2.0);
        assert!(
            (fast.as_secs_f64() - 2.5).abs() < 1e-9,
            "a fast vehicle takes the nominal time"
        );
        assert!(
            slow.as_secs_f64() > 2.5,
            "a slow one is stretched: {slow:?}"
        );
        // The peak lateral velocity never exceeds the heading cap.
        for v in [0.5, 2.0, 5.0, 15.0, 33.0] {
            let dur = transition_duration(&p, 3.5, v).as_secs_f64();
            let peak = 1.5 * 3.5 / dur;
            let dev = math::atan2(peak, v.max(0.5));
            assert!(dev <= p.max_heading_dev_rad + 1e-9, "v={v} dev={dev}");
        }
    }

    #[test]
    fn the_cooldown_follows_the_document() {
        let m = mobil(MobilPreset::Legacy);
        // max(2·T_lc, 2·dur).
        assert!((m.cooldown(Duration::from_secs_f64(2.0)).as_secs_f64() - 5.0).abs() < 1e-9);
        assert!((m.cooldown(Duration::from_secs_f64(9.0)).as_secs_f64() - 18.0).abs() < 1e-9);
    }

    #[test]
    fn the_cards_incentive_equation_has_the_bias_on_the_side_the_code_puts_it() {
        // The card printed `… > Δa_th + Δa_bias`, which taken literally makes a change to
        // the right HARDER. The code adds +right_bias to the left-hand side for a change to
        // the right, which makes it EASIER — Kesting 2007's asymmetric European keep-right
        // rule, and the right answer. The code was never wrong; the card was, in the one
        // place a reader would look.
        //
        // First the measurement, so the card is checked against behaviour and not against
        // another sentence.
        let w = world();
        let (right, left) = two_lanes(&w);
        let ego = vehicle(0, right, 0, 20.0, 12.0);
        let leader = vehicle(1, right, 0, 28.0, 3.0);
        let snap = ActorSnapshot::build(
            0,
            80.0,
            [
                (
                    ego,
                    v2xw_core::kinematics::Kinematics::at_rest(
                        0,
                        w.to_xyz(&v2xw_core::geom::LanePos::centred(right, 20.0)),
                    ),
                ),
                (
                    leader,
                    v2xw_core::kinematics::Kinematics::at_rest(
                        0,
                        w.to_xyz(&v2xw_core::geom::LanePos::centred(right, 28.0)),
                    ),
                ),
            ],
        );
        let nbrs = snap.neighbors(&w, &ego, &[], 0, NeighborOptions::default());
        let target = nbrs.left.expect("a left lane");
        assert_eq!(target.lane, left);
        let ego_lane = LaneView::of(w.lane(right));
        let target_lane = LaneView::of(w.lane(left));

        let scored = |asymmetric: bool| {
            let idm: Arc<dyn CarFollowing + Send + Sync> =
                Arc::new(Idm::new(IdmPreset::Kesting2010));
            let params = MobilParams {
                asymmetric,
                right_bias_mps2: 0.3,
                step_s: 1.0,
                ..MobilPreset::Kesting2007.params()
            };
            let m = Mobil::with_params(MobilPreset::Kesting2007, params, idm);
            m.score(
                &ego,
                &nbrs,
                &target,
                &ego_lane,
                &target_lane,
                &WeatherState::CLEAR,
            )
            .expect("a safe change")
        };
        let symmetric = scored(false);
        let to_the_left = scored(true);
        // A change to the LEFT is scored 0.3 m/s² lower when the rule is on: the bias is
        // subtracted from the gain, so moving left is harder.
        assert!(
            (to_the_left - (symmetric - 0.3)).abs() < 1e-12,
            "to the left: symmetric {symmetric}, asymmetric {to_the_left}"
        );

        // And the card says exactly that.
        let card = mobil(MobilPreset::Kesting2007).card().clone();
        let incentive = card
            .equations
            .iter()
            .find(|e| e.name == "incentive")
            .expect("an incentive equation");
        let (lhs, rhs) = incentive
            .latex_or_text
            .split_once('>')
            .expect("an inequality");
        assert!(
            lhs.contains("Δa_bias"),
            "the bias belongs with the gain, on the left: {}",
            incentive.latex_or_text
        );
        assert!(
            !rhs.contains("Δa_bias"),
            "the bias is printed against the threshold, which reverses its sense: {}",
            incentive.latex_or_text
        );
        assert!(rhs.contains("Δa_th"));
        let notes = incentive.notes.as_deref().unwrap_or_default();
        assert!(
            notes.contains("right EASIER") || notes.contains("right easier"),
            "the note does not say which way the bias points: {notes}"
        );
    }

    #[test]
    fn both_presets_have_valid_cards() {
        for p in MobilPreset::ALL {
            let m = mobil(p);
            m.card().validate().expect("card validates");
            assert_eq!(m.card().id, MODEL_ID);
            assert!(m.card().determinism.uses_rng);
        }
    }

    #[test]
    fn the_kesting_parameters_are_the_published_ones() {
        let p = MobilPreset::Kesting2007.params();
        assert_eq!(p.threshold_mps2, 0.1);
        assert_eq!(p.b_safe_mps2, 4.0);
        assert_eq!(p.right_bias_mps2, 0.3);
        assert_eq!(p.politeness, 0.2);
        let l = MobilPreset::Legacy.params();
        assert_eq!(l.threshold_mps2, 0.2);
        assert_eq!(l.b_safe_mps2, 4.0);
        assert_eq!(l.min_speed_mps, 3.0);
        assert_eq!(l.transition_s, 2.5);
    }
}
