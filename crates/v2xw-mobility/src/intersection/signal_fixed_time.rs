//! `mobility/intersection/signal-fixed-time` — pre-timed signal control (04-models.md §2.3).
//!
//! Two jobs, and §2.3 gives both:
//!
//! 1. **Run** the world's [`SignalPlan`]: read the phase running at `t`, turn the ego's
//!    movement's [`SignalState`] into an [`EntryDecision`], and publish the controller's
//!    state each step.
//! 2. **Generate** a plan for a junction whose source gave only signal *presence* — which
//!    is all an OSM import gives (04-models.md §1.2) — from the `netconvert` defaults §2.3
//!    tabulates.
//!
//! # The generated plan
//!
//! | Parameter | Default | Source |
//! |---|---|---|
//! | cycle length | 90 s | `netconvert --tls.cycle.time` [R10 §B10] |
//! | green per phase | 31 s | `netconvert --tls.green.time` |
//! | red with no conflicting flow | 5 s | `netconvert --tls.red.time` |
//! | yellow | from the approach kinematics, `--tls.yellow.min-decel 3 m/s²` | FHWA Signal Timing Manual 2008 Ch. 5 guidance 3-6 s |
//! | all-red | 0 s | `netconvert --tls.allred.time` |
//! | pedestrian minimum green, clearance | 4 s, 5 s | `--tls.crossing-min.time`, `--tls.crossing-clearance.time` |
//! | left-turn phase | 6 s | `--tls.left-green.time` |
//! | variable phase min, max | 5 s, 50 s | `--tls.min-dur`, `--tls.max-dur` |
//!
//! The yellow interval uses the ITE formula §2.3 quotes, `y = t + v/(2a)`, with `t ≈ 1 s`
//! and `a` the `--tls.yellow.min-decel` value, on the flat-grade assumption `G = 0`, and is
//! clamped to the FHWA guidance range of 3 to 6 s. §2.3 marks the ITE constants
//! **secondary**, and so does the card.
//!
//! # What it does not do
//!
//! Actuation. §2.3's assumption line is "pre-timed, no actuation", and the `min-dur` and
//! `max-dur` parameters are carried on the card for the actuated controller that will use
//! them rather than being read here.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::{LaneId, SignalId};
use v2xw_core::weather::WeatherState;
use v2xw_world::{
    Junction, SignalPhase, SignalPlan, SignalState, TurnDirection, World, quant::Q_TIME_S,
    quant::quantise,
};

use crate::intersection::{STOP_LINE_OFFSET_M, on_east_west_axis};
use crate::traits::IntersectionControl;
use crate::views::{ConflictView, EntryDecision, JunctionView, PhaseState, VehicleView};

/// The model id.
pub const MODEL_ID: &str = "mobility/intersection/signal-fixed-time";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The acceleration a permissive turn assumes the opposing stream — which has the same
/// green — pulls away with, m/s²: the Kesting 2010 IDM passenger-car maximum
/// acceleration `a` (04-models.md §2.1), the same number the car-following model drives
/// those vehicles with.
pub const OPPOSING_START_ACCEL_MPS2: f64 = 1.4;

/// The fixed-time plan generator's parameters (§2.3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignalPlanParams {
    /// Cycle length, seconds (`--tls.cycle.time`).
    pub cycle_s: f64,
    /// Green per phase, seconds (`--tls.green.time`).
    pub green_s: f64,
    /// Red with no conflicting flow, seconds (`--tls.red.time`).
    pub red_s: f64,
    /// A fixed yellow, seconds; `None` computes it from the approach speed.
    pub yellow_s: Option<f64>,
    /// The deceleration the yellow interval assumes, m/s² (`--tls.yellow.min-decel`).
    pub yellow_min_decel_mps2: f64,
    /// The reaction time in the ITE yellow formula, seconds (**secondary**).
    pub yellow_reaction_s: f64,
    /// The guidance range the computed yellow is clamped to, seconds.
    pub yellow_range_s: (f64, f64),
    /// All-red, seconds (`--tls.allred.time`).
    pub all_red_s: f64,
    /// Pedestrian minimum green, seconds (`--tls.crossing-min.time`).
    pub crossing_min_green_s: f64,
    /// Pedestrian clearance, seconds (`--tls.crossing-clearance.time`).
    pub crossing_clearance_s: f64,
    /// Left-turn phase, seconds (`--tls.left-green.time`).
    pub left_green_s: f64,
    /// Variable phase minimum, seconds (`--tls.min-dur`), for an actuated controller.
    pub min_dur_s: f64,
    /// Variable phase maximum, seconds (`--tls.max-dur`), for an actuated controller.
    pub max_dur_s: f64,
    /// Whether to stretch the greens so the phases sum to [`SignalPlanParams::cycle_s`].
    /// With this off, the cycle is whatever `green + yellow + all-red` per phase adds up
    /// to.
    pub match_cycle: bool,
    /// Whether a permitted left turn gets [`SignalState::GreenYield`] rather than
    /// [`SignalState::Green`].
    pub permissive_left: bool,
    /// How far before the stop line a vehicle halts, metres.
    pub stop_line_offset_m: f64,
}

impl Default for SignalPlanParams {
    fn default() -> Self {
        Self {
            cycle_s: 90.0,
            green_s: 31.0,
            red_s: 5.0,
            yellow_s: None,
            yellow_min_decel_mps2: 3.0,
            yellow_reaction_s: 1.0,
            yellow_range_s: (3.0, 6.0),
            all_red_s: 0.0,
            crossing_min_green_s: 4.0,
            crossing_clearance_s: 5.0,
            left_green_s: 6.0,
            min_dur_s: 5.0,
            max_dur_s: 50.0,
            match_cycle: true,
            permissive_left: true,
            stop_line_offset_m: STOP_LINE_OFFSET_M,
        }
    }
}

impl SignalPlanParams {
    /// The yellow interval for an approach whose speed limit is `v_mps`, seconds.
    ///
    /// `y = t + v/(2a)`, clamped to the guidance range — the ITE formula §2.3 quotes with
    /// the grade term `G` zero (flat), which is the assumption recorded on the card.
    pub fn yellow_for(&self, v_mps: f64) -> f64 {
        if let Some(fixed) = self.yellow_s {
            return fixed;
        }
        let y = self.yellow_reaction_s + v_mps / (2.0 * self.yellow_min_decel_mps2);
        y.clamp(self.yellow_range_s.0, self.yellow_range_s.1)
    }
}

/// Fixed-time signal control.
#[derive(Debug, Clone)]
pub struct FixedTimeSignals {
    params: SignalPlanParams,
    card: ModelCard,
}

impl Default for FixedTimeSignals {
    fn default() -> Self {
        FixedTimeSignals::new(SignalPlanParams::default())
    }
}

impl FixedTimeSignals {
    /// The model with the given parameters.
    pub fn new(params: SignalPlanParams) -> Self {
        Self {
            card: card(&params),
            params,
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &SignalPlanParams {
        &self.params
    }

    /// The controller state of `plan` at `t_s` seconds after `t0`.
    pub fn phase_state(&self, plan: &SignalPlan, t_s: f64) -> Option<PhaseState> {
        let (phase, elapsed) = plan.phase_at(t_s)?;
        let p = &plan.phases[phase];
        Some(PhaseState {
            phase,
            elapsed_s: elapsed,
            remaining_s: (p.duration_s - elapsed).max(0.0),
            states: p.states.clone(),
        })
    }

    /// The state `plan` is showing to the movement on `movement_lane` at `t_s`.
    pub fn state_for(
        &self,
        plan: &SignalPlan,
        movement_lane: LaneId,
        t_s: f64,
    ) -> Option<SignalState> {
        let index = plan.controlled.iter().position(|l| *l == movement_lane)?;
        plan.states_at(t_s)?.get(index).copied()
    }

    /// Generates a fixed-time plan for `junction` from the `netconvert` defaults.
    ///
    /// The movements are split into two phase groups by the axis of their approach — the
    /// east-west movements and the north-south ones — which is the two-phase plan
    /// `netconvert` generates for an ordinary crossroads. A junction whose movements all
    /// fall on one axis gets a single-phase plan, which is what an uncontrolled
    /// continuation deserves.
    ///
    /// Returns `None` when the junction has no internal connectors to control.
    pub fn generate_plan(
        &self,
        world: &World,
        junction: &Junction,
        id: SignalId,
    ) -> Option<SignalPlan> {
        if junction.internal.is_empty() {
            return None;
        }
        // Which phase group each movement belongs to, and the fastest approach in each.
        let mut group_of: Vec<usize> = Vec::with_capacity(junction.internal.len());
        let mut turn_of: Vec<TurnDirection> = Vec::with_capacity(junction.internal.len());
        let mut approach_speed = [0.0f64; 2];
        for internal in &junction.internal {
            let (heading, speed, turn) = self.approach_of(world, junction, *internal);
            let group = usize::from(!on_east_west_axis(heading));
            approach_speed[group] = approach_speed[group].max(speed);
            group_of.push(group);
            turn_of.push(turn);
        }
        let groups: Vec<usize> = {
            let mut g: Vec<usize> = group_of.clone();
            g.sort_unstable();
            g.dedup();
            g
        };
        let yellow: Vec<f64> = groups
            .iter()
            .map(|g| quantise(self.params.yellow_for(approach_speed[*g]), Q_TIME_S))
            .collect();
        let all_red = quantise(self.params.all_red_s, Q_TIME_S);
        let green = if self.params.match_cycle {
            let fixed: f64 = yellow.iter().sum::<f64>() + all_red * groups.len() as f64;
            quantise(
                ((self.params.cycle_s - fixed) / groups.len() as f64).max(self.params.min_dur_s),
                Q_TIME_S,
            )
        } else {
            quantise(self.params.green_s, Q_TIME_S)
        };

        let mut phases: Vec<SignalPhase> = Vec::new();
        for (k, group) in groups.iter().enumerate() {
            // Green for this group.
            phases.push(SignalPhase {
                duration_s: green,
                states: group_of
                    .iter()
                    .zip(&turn_of)
                    .map(|(g, turn)| {
                        if g != group {
                            SignalState::Red
                        } else if self.params.permissive_left && turn.crosses_opposing_traffic() {
                            SignalState::GreenYield
                        } else {
                            SignalState::Green
                        }
                    })
                    .collect(),
                name: None,
            });
            // Amber for this group.
            phases.push(SignalPhase {
                duration_s: yellow[k],
                states: group_of
                    .iter()
                    .map(|g| {
                        if g == group {
                            SignalState::Amber
                        } else {
                            SignalState::Red
                        }
                    })
                    .collect(),
                name: None,
            });
            if all_red > 0.0 {
                phases.push(SignalPhase {
                    duration_s: all_red,
                    states: vec![SignalState::Red; group_of.len()],
                    name: None,
                });
            }
        }
        let cycle = quantise(phases.iter().map(|p| p.duration_s).sum::<f64>(), Q_TIME_S);
        Some(SignalPlan {
            id,
            junction: junction.id,
            cycle_s: cycle,
            offset_s: 0.0,
            controlled: junction.internal.clone(),
            phases,
            heads: Vec::new(),
        })
    }

    /// The heading, speed limit and turn of the approach that feeds `internal`.
    fn approach_of(
        &self,
        world: &World,
        junction: &Junction,
        internal: LaneId,
    ) -> (f64, f64, TurnDirection) {
        for from in &junction.incoming {
            for c in world.successors(*from) {
                if c.via == Some(internal) {
                    let lane = world.lane(*from);
                    return (
                        lane.heading_at(lane.length_m),
                        lane.speed_limit_mps,
                        c.direction,
                    );
                }
            }
        }
        // No approach found (a malformed junction): the internal lane's own heading is the
        // best available answer and the movement is treated as straight on.
        match world.try_lane(internal) {
            Some(lane) => (
                lane.heading_at(0.0),
                lane.speed_limit_mps,
                TurnDirection::Straight,
            ),
            None => (0.0, 0.0, TurnDirection::Straight),
        }
    }
}

impl v2xw_core::model::Model for FixedTimeSignals {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl IntersectionControl for FixedTimeSignals {
    fn may_enter(
        &self,
        ego: &VehicleView,
        j: &JunctionView,
        conflicts: &[ConflictView],
        _w: &WeatherState,
    ) -> EntryDecision {
        if j.stop_line_gap_m <= 0.0 {
            return EntryDecision::Proceed;
        }
        let gap = (j.stop_line_gap_m - self.params.stop_line_offset_m).max(0.0);
        match j.signal {
            // No signal here at all: this model has nothing to say, and the junction's
            // other controller (gap acceptance) decides.
            None => EntryDecision::Proceed,
            Some(SignalState::Green) => EntryDecision::Proceed,
            Some(SignalState::Red) | Some(SignalState::RedAmber) => {
                EntryDecision::Stop { gap_m: gap }
            }
            Some(SignalState::Amber) => {
                // Stop if it can be done at the assumed deceleration, else clear the
                // junction — the dilemma zone the yellow interval is sized for.
                let stopping_distance = ego.speed_mps * ego.speed_mps
                    / (2.0 * self.params.yellow_min_decel_mps2.max(f64::EPSILON));
                if stopping_distance <= j.stop_line_gap_m {
                    EntryDecision::Stop { gap_m: gap }
                } else {
                    EntryDecision::Proceed
                }
            }
            // A permissive green: proceed only if no conflicting claimant the ego must
            // give way to is closing on the junction. The critical gap is the HCM
            // major-street left-turn value, which is the movement a permissive green is.
            Some(SignalState::GreenYield) => {
                let critical = crate::intersection::gap_acceptance::HcmGaps::of(
                    crate::intersection::gap_acceptance::Movement::MajorLeft,
                )
                .critical_gap_s(j.major_lanes);
                let closing = conflicts
                    .iter()
                    .filter(|c| c.conflicts && c.ego_must_yield)
                    .map(|c| c.time_to_stop_line_accelerating_s(OPPOSING_START_ACCEL_MPS2))
                    .fold(f64::INFINITY, f64::min);
                if closing >= critical {
                    EntryDecision::Proceed
                } else {
                    EntryDecision::Stop { gap_m: gap }
                }
            }
            // Flashing amber and a dark head both mean "no right of way here": approach at
            // a speed from which the vehicle can stop within the remaining gap.
            Some(SignalState::FlashingAmber) | Some(SignalState::Off) => EntryDecision::SlowTo {
                gap_m: gap,
                speed_mps: v2xw_core::math::sqrt(
                    2.0 * self.params.yellow_min_decel_mps2 * gap.max(0.0),
                ),
            },
        }
    }
}

/// The model card.
pub fn card(params: &SignalPlanParams) -> ModelCard {
    let netconvert = Source {
        kind: SourceKind::Code,
        reference: "SUMO `netconvert` traffic-light defaults (`--tls.*`) [04-models.md §2.3, \
                    R10 §B10]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let fhwa = Source {
        kind: SourceKind::Standard,
        reference: "FHWA Signal Timing Manual 2008 Ch. 5 (yellow 3-6 s guidance; ITE formula \
                    y = t + v/(2a + 2Gg) with t ≈ 1 s, a ≈ 10 ft/s²) [R10 §B10]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: Some("**secondary**, not re-verified against the primary document".to_string()),
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "Pre-timed signal control: runs the world's fixed-time plan, turns the state shown \
         to a vehicle's movement into a stop-or-go decision, and generates a plan from the \
         netconvert defaults for a junction whose source gave only signal presence.",
    );
    card.tier = vec![Tier::Medium];
    card.equations = vec![
        Equation {
            name: "phase".to_string(),
            latex_or_text: "into = (t − offset) mod cycle;  the phase is the first whose \
                            cumulative duration exceeds `into`"
                .to_string(),
            notes: Some(
                "`%` on f64 is exact in IEEE-754, so two engines agree on a phase boundary \
                 to the last bit (build decision D10)"
                    .to_string(),
            ),
        },
        Equation {
            name: "yellow interval".to_string(),
            latex_or_text: "y = clamp(t_react + v/(2·a), 3 s, 6 s)".to_string(),
            notes: Some(
                "the ITE formula of §2.3 with the grade term G = 0 (flat); the constants are \
                 **secondary**"
                    .to_string(),
            ),
        },
        Equation {
            name: "amber decision".to_string(),
            latex_or_text: "stop ⟺ v²/(2a) ≤ gap to the stop line".to_string(),
            notes: Some("the dilemma zone the yellow interval is sized for".to_string()),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "cycle_time",
            "s",
            serde_json::json!(params.cycle_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "green_time",
            "s",
            serde_json::json!(params.green_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "red_time",
            "s",
            serde_json::json!(params.red_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "yellow_min_decel",
            "m/s²",
            serde_json::json!(params.yellow_min_decel_mps2),
            netconvert.clone(),
        ),
        Parameter::new(
            "yellow_reaction",
            "s",
            serde_json::json!(params.yellow_reaction_s),
            fhwa.clone(),
        ),
        Parameter::new(
            "yellow_range",
            "s",
            serde_json::json!([params.yellow_range_s.0, params.yellow_range_s.1]),
            fhwa.clone(),
        ),
        Parameter::new(
            "allred_time",
            "s",
            serde_json::json!(params.all_red_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "crossing_min_time",
            "s",
            serde_json::json!(params.crossing_min_green_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "crossing_clearance_time",
            "s",
            serde_json::json!(params.crossing_clearance_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "left_green_time",
            "s",
            serde_json::json!(params.left_green_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "min_dur",
            "s",
            serde_json::json!(params.min_dur_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "max_dur",
            "s",
            serde_json::json!(params.max_dur_s),
            netconvert.clone(),
        ),
        Parameter::new(
            "match_cycle",
            "-",
            serde_json::json!(params.match_cycle),
            netconvert.clone(),
        ),
        Parameter::new(
            "permissive_left",
            "-",
            serde_json::json!(params.permissive_left),
            netconvert,
        ),
        Parameter::new(
            "stop_line_offset",
            "m",
            serde_json::json!(params.stop_line_offset_m),
            Source::new(
                SourceKind::Code,
                "legacy/scms_sim_ref/mock_pipeline/run.py L2467 (halt 2 m before the line)",
            ),
        ),
    ];
    card.assumptions = vec![
        "Pre-timed, no actuation (04-models.md §2.3).".to_string(),
        "The generated plan splits movements into two phase groups by the axis of their \
         approach, which is the two-phase plan netconvert generates for a crossroads."
            .to_string(),
        "The yellow interval assumes a flat grade (G = 0 in the ITE formula).".to_string(),
        "A permitted left turn shows GreenYield and gives way by the HCM major-street \
         left-turn critical gap."
            .to_string(),
    ];
    card.ignores = vec![
        "Actuated and coordinated control, and SUMO's `request` conflict evaluation inside \
         the junction (medium relative to high, 04-models.md §2.3)."
            .to_string(),
    ];
    card.sources = vec![fhwa];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "intersection::signal_fixed_time::tests::a_generated_plan_is_a_valid_world_plan"
                .to_string(),
            "engine::tests::a_vehicle_stops_at_red_and_goes_on_green".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classes::VehicleClass;
    use crate::views::DriverProfile;
    use v2xw_core::geom::{Dims, Vec3};
    use v2xw_core::ids::{ActorId, JunctionId};
    use v2xw_core::model::Model;
    use v2xw_world::{ImportOptions, JunctionControl, procedural::GridParams};

    fn ego(speed: f64) -> VehicleView {
        VehicleView {
            actor: ActorId::new(0),
            class: VehicleClass::Passenger,
            lane: LaneId::new(0),
            lane_index: 0,
            s_m: 0.0,
            lateral_m: 0.0,
            speed_mps: speed,
            accel_mps2: 0.0,
            heading_rad: 0.0,
            dims: Dims::new(5.0, 1.8, 1.5),
            driver: DriverProfile {
                desired_speed_mps: 13.89,
                max_accel_mps2: 1.4,
                comfort_decel_mps2: 2.0,
                time_headway_s: 1.5,
                min_gap_m: 2.0,
            },
        }
    }

    fn junction(signal: Option<SignalState>, gap: f64) -> JunctionView {
        JunctionView {
            id: JunctionId::new(0),
            position: Vec3::ZERO,
            control: JunctionControl::Signalised {
                plan: SignalId::new(0),
            },
            stop_line_gap_m: gap,
            movement: TurnDirection::Straight,
            movement_lane: Some(LaneId::new(1)),
            signal,
            major_lanes: 2,
        }
    }

    #[test]
    fn red_stops_and_green_goes() {
        let m = FixedTimeSignals::default();
        assert_eq!(
            m.may_enter(
                &ego(10.0),
                &junction(Some(SignalState::Red), 30.0),
                &[],
                &WeatherState::CLEAR
            ),
            EntryDecision::Stop { gap_m: 28.0 }
        );
        assert_eq!(
            m.may_enter(
                &ego(10.0),
                &junction(Some(SignalState::Green), 30.0),
                &[],
                &WeatherState::CLEAR
            ),
            EntryDecision::Proceed
        );
        assert_eq!(
            m.may_enter(
                &ego(10.0),
                &junction(Some(SignalState::RedAmber), 30.0),
                &[],
                &WeatherState::CLEAR
            ),
            EntryDecision::Stop { gap_m: 28.0 }
        );
    }

    #[test]
    fn amber_stops_only_when_stopping_is_possible() {
        let m = FixedTimeSignals::default();
        // 10 m/s needs 100/6 ≈ 16.7 m to stop at 3 m/s².
        assert_eq!(
            m.may_enter(
                &ego(10.0),
                &junction(Some(SignalState::Amber), 30.0),
                &[],
                &WeatherState::CLEAR
            ),
            EntryDecision::Stop { gap_m: 28.0 }
        );
        assert_eq!(
            m.may_enter(
                &ego(10.0),
                &junction(Some(SignalState::Amber), 10.0),
                &[],
                &WeatherState::CLEAR
            ),
            EntryDecision::Proceed,
            "inside the dilemma zone it clears the junction"
        );
    }

    #[test]
    fn a_permissive_turn_yields_to_a_queue_about_to_pull_away() {
        // The opposing through stream has the same green. A vehicle of it standing at the
        // line when the light changes is about to move; taken at its speed of zero it was
        // "never arriving", and the left turner cut across it.
        let m = FixedTimeSignals::default();
        let mut j = junction(Some(SignalState::GreenYield), 20.0);
        j.movement = TurnDirection::Left;
        let opposing = |gap: f64, speed: f64| ConflictView {
            actor: ActorId::new(9),
            stop_line_gap_m: gap,
            speed_mps: speed,
            heading_rad: core::f64::consts::PI,
            movement: TurnDirection::Straight,
            movement_lane: Some(LaneId::new(2)),
            conflicts: true,
            ego_must_yield: true,
        };
        // Standing 3 m from the line: it reaches it in sqrt(2·3/1.4) ≈ 2.1 s < 4.1 s.
        assert_eq!(
            m.may_enter(&ego(5.0), &j, &[opposing(3.0, 0.0)], &WeatherState::CLEAR),
            EntryDecision::Stop { gap_m: 18.0 }
        );
        // Standing 60 m back: 9.3 s away, a gap the turn can take.
        assert_eq!(
            m.may_enter(&ego(5.0), &j, &[opposing(60.0, 0.0)], &WeatherState::CLEAR),
            EntryDecision::Proceed
        );
    }

    #[test]
    fn the_yellow_interval_follows_the_ite_formula() {
        let p = SignalPlanParams::default();
        // 13.89 m/s: 1 + 13.89/6 = 3.315 s, inside the guidance range.
        assert!((p.yellow_for(13.89) - (1.0 + 13.89 / 6.0)).abs() < 1e-12);
        // 8 m/s: 1 + 1.33 = 2.33 s, below the 3 s floor.
        assert_eq!(p.yellow_for(8.0), 3.0);
        // 40 m/s: 1 + 6.67 = 7.67 s, above the 6 s ceiling.
        assert_eq!(p.yellow_for(40.0), 6.0);
        // And a fixed value overrides the formula.
        let fixed = SignalPlanParams {
            yellow_s: Some(4.0),
            ..p
        };
        assert_eq!(fixed.yellow_for(30.0), 4.0);
    }

    #[test]
    fn a_generated_plan_is_a_valid_world_plan() {
        let params = GridParams {
            signalised: false,
            ..GridParams::legacy()
        };
        let world = v2xw_world::procedural::grid(&params, &ImportOptions::default()).expect("grid");
        let m = FixedTimeSignals::default();
        // Generate a plan for the first junction that has internal connectors.
        let junction = world
            .roads
            .junctions()
            .iter()
            .find(|j| !j.internal.is_empty())
            .cloned()
            .expect("a junction with movements");
        let plan = m
            .generate_plan(&world, &junction, SignalId::new(0))
            .expect("a plan");
        assert_eq!(plan.controlled, junction.internal);
        assert!(!plan.phases.is_empty());
        assert!(
            (plan.total_phase_duration_s() - plan.cycle_s).abs() <= Q_TIME_S,
            "phases must sum to the cycle"
        );
        for phase in &plan.phases {
            assert_eq!(phase.states.len(), plan.controlled.len());
        }
        // The default cycle is matched to 90 s.
        assert!((plan.cycle_s - 90.0).abs() < 0.5, "cycle {}", plan.cycle_s);
        // Every movement gets a green at some point in the cycle.
        for i in 0..plan.controlled.len() {
            assert!(
                plan.phases.iter().any(|p| p.states[i].permits_entry()),
                "movement {i} never gets a green"
            );
        }
        // And the world accepts it: attach it to its junction and rebuild.
        let id = junction.id;
        let signalised = crate::worlds::rebuild_with(&world, |parts| {
            parts.signals.push(plan);
            parts.junctions[id.as_usize()].control = JunctionControl::Signalised {
                plan: SignalId::new(0),
            };
        })
        .expect("a world with a generated plan");
        signalised.validate().expect("the generated plan validates");
        assert_eq!(signalised.signals.len(), 1);
    }

    #[test]
    fn the_phase_state_reports_elapsed_and_remaining() {
        let params = GridParams::legacy().with_signals(true);
        let world = v2xw_world::procedural::grid(&params, &ImportOptions::default()).expect("grid");
        let m = FixedTimeSignals::default();
        let plan = world.signals.first().expect("the grid is signalised");
        let state = m.phase_state(plan, 0.0).expect("a phase");
        assert_eq!(state.phase, 0);
        assert!((state.elapsed_s - 0.0).abs() < 1e-12);
        assert!(state.remaining_s > 0.0);
        assert_eq!(state.states.len(), plan.controlled.len());
        // A whole cycle later, the same phase.
        let wrapped = m.phase_state(plan, plan.cycle_s).expect("a phase");
        assert_eq!(wrapped.phase, 0);
    }

    #[test]
    fn the_card_validates() {
        FixedTimeSignals::default()
            .card()
            .validate()
            .expect("validates");
    }
}
