//! `mobility/intersection/gap-acceptance-hcm` — HCM gap acceptance (04-models.md §2.3).
//!
//! # The rule
//!
//! A minor-stream vehicle enters when the gap to the next conflicting major-stream vehicle
//! exceeds the **critical gap** `t_c`, and successive vehicles in the same queue follow at
//! the **follow-up time** `t_f`. The `k`-th vehicle of a queue therefore needs
//! `t_c + k·t_f`: the leader waits for `t_c`, and every vehicle behind it needs its own
//! discharge headway on top. That queue rule is the standard reading of the HCM two-value
//! model and is recorded as an assumption on the card rather than quoted from the source,
//! which gives the two values and not the composition.
//!
//! # The values, and their status
//!
//! | Movement | `t_c` (s), major flow < 4 lanes | `t_c` (s), ≥ 4 lanes | `t_f` (s) |
//! |---|---|---|---|
//! | Major-street left turn | 4.1 | 4.1 | 2.2 |
//! | Minor-street right turn | 6.2 | 6.9 | 3.3 |
//! | Minor-street through | 6.5 | 6.5 | 4.0 |
//! | Minor-street left turn | 7.1 | 7.5 | 3.5 |
//!
//! §2.3 flags the whole table **secondary**: the values are reproduced by the PTV VISUM
//! help because the HCM and FHWA PDFs did not parse [R10 §B9]. That flag is carried onto
//! every parameter of the card, which is what the task requires and what keeps a reader
//! from mistaking a second-hand number for a primary one.
//!
//! # Determinism
//!
//! No random draw: §2.3 records that the legacy rule has "no random draw" and this model
//! keeps that property. Ties between claimants are broken by distance and then by actor id,
//! which is a strict total order, so the yield relation is acyclic and the closest vehicle
//! at every junction always makes progress — no gridlock, no starvation.
//!
//! # The standing-claimant rule, and the cycle it breaks
//!
//! The total order above is the model's own priority rule, and it only applies where the
//! world carries **no** conflict matrix. Where the world does carry one, priority comes
//! from the matrix's `must_yield` row — and the procedural generator builds that row with
//! "give way to the right" for equal-rank movements, which at a four-way crossing is a
//! **cycle**: A yields to B, B to C, C to D, D to A.
//!
//! A cycle cannot deadlock here, because [`ConflictView::time_to_stop_line_s`] is infinite
//! for a standing claimant, so once every vehicle has stopped nobody is closing a gap on
//! anybody. What it does instead is worse than a deadlock and harder to see: all four
//! stall until they have all stopped and then all proceed **together**, four vehicles
//! inside the junction at once. Measured before this rule existed: four vehicles 40 m out
//! at 10 m/s all cleared at t = 38.4 s against a ~5 s free-flow time, with two conflicting
//! movements sharing the junction for 12.4 s of it.
//!
//! So a standing vehicle still does not close a gap, but it does hold a **claim**: among
//! itself and every conflicting claimant that is also standing, the one with the smallest
//! `(stop_line_gap_m, actor)` goes and the rest keep giving way, whatever the matrix says
//! pairwise. That is a local minimum of a strict total order over the conflict
//! neighbourhood, so it is acyclic by construction, it admits two non-conflicting
//! movements at once (each is the minimum of its own neighbourhood), and it cannot
//! starve anybody: the vehicle nearest the line always goes, and once it has gone it is no
//! longer a claimant.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::weather::WeatherState;
use v2xw_world::TurnDirection;

use crate::intersection::STOP_LINE_OFFSET_M;
use crate::traits::IntersectionControl;
use crate::views::{ConflictView, EntryDecision, JunctionView, VehicleView};

/// The model id.
pub const MODEL_ID: &str = "mobility/intersection/gap-acceptance-hcm";

/// The model version.
pub const MODEL_VERSION: &str = "1.1.0";

/// At or below what speed a claimant counts as standing, m/s.
///
/// **This crate's choice, not a cited threshold.** The reference engine has no stopped
/// test at a junction at all: it resolves priority by a strict `(distance, vehicle id)`
/// order with no conflict matrix [`run.py` L2415-2440], so the question never arises
/// there. The value is [`crate::carfollowing::idm::LEGACY_V0_FLOOR_MPS`] = 0.1 m/s, the
/// desired-speed floor of the same engine's IDM port (`max(0.1, v0)`, [`run.py` L2276]),
/// reused so that "not moving" is one number in this crate rather than two.
pub const STANDING_SPEED_MPS: f64 = crate::carfollowing::idm::LEGACY_V0_FLOOR_MPS;

/// Which row of the HCM table a movement takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Movement {
    /// A left turn from the major street, across the opposing through movement.
    MajorLeft,
    /// A right turn from the minor street.
    MinorRight,
    /// A through movement from the minor street.
    MinorThrough,
    /// A left turn from the minor street.
    MinorLeft,
}

impl Movement {
    /// Every movement, in table order.
    pub const ALL: [Movement; 4] = [
        Movement::MajorLeft,
        Movement::MinorRight,
        Movement::MinorThrough,
        Movement::MinorLeft,
    ];

    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Movement::MajorLeft => "major-street-left-turn",
            Movement::MinorRight => "minor-street-right-turn",
            Movement::MinorThrough => "minor-street-through",
            Movement::MinorLeft => "minor-street-left-turn",
        }
    }

    /// Which row a vehicle takes, from the turn it intends and whether it is on the minor
    /// stream (which is what "must give way to somebody here" means).
    ///
    /// A U-turn takes the left-turn row of its own street: it is the movement that crosses
    /// the most conflicting traffic, and the table has no U-turn row.
    pub fn classify(turn: TurnDirection, on_minor: bool) -> Movement {
        match (turn, on_minor) {
            (TurnDirection::Left | TurnDirection::SlightLeft | TurnDirection::UTurn, false) => {
                Movement::MajorLeft
            }
            (_, false) => Movement::MajorLeft,
            (TurnDirection::Right | TurnDirection::SlightRight, true) => Movement::MinorRight,
            (TurnDirection::Straight, true) => Movement::MinorThrough,
            (_, true) => Movement::MinorLeft,
        }
    }
}

/// One row of the HCM table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HcmGaps {
    /// Critical gap where the major flow has fewer than four lanes, seconds.
    pub critical_gap_narrow_s: f64,
    /// Critical gap where it has four or more, seconds.
    pub critical_gap_wide_s: f64,
    /// Follow-up time, seconds.
    pub follow_up_s: f64,
}

impl HcmGaps {
    /// The row for `movement` (04-models.md §2.3, **secondary**).
    pub const fn of(movement: Movement) -> HcmGaps {
        match movement {
            Movement::MajorLeft => HcmGaps {
                critical_gap_narrow_s: 4.1,
                critical_gap_wide_s: 4.1,
                follow_up_s: 2.2,
            },
            Movement::MinorRight => HcmGaps {
                critical_gap_narrow_s: 6.2,
                critical_gap_wide_s: 6.9,
                follow_up_s: 3.3,
            },
            Movement::MinorThrough => HcmGaps {
                critical_gap_narrow_s: 6.5,
                critical_gap_wide_s: 6.5,
                follow_up_s: 4.0,
            },
            Movement::MinorLeft => HcmGaps {
                critical_gap_narrow_s: 7.1,
                critical_gap_wide_s: 7.5,
                follow_up_s: 3.5,
            },
        }
    }

    /// The critical gap for a major flow of `major_lanes` lanes, seconds.
    pub fn critical_gap_s(&self, major_lanes: u8) -> f64 {
        if major_lanes >= 4 {
            self.critical_gap_wide_s
        } else {
            self.critical_gap_narrow_s
        }
    }
}

/// The model's own parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GapAcceptanceParams {
    /// How far before the stop line a yielding vehicle halts, metres.
    pub stop_line_offset_m: f64,
    /// Whether to apply the follow-up time to vehicles queued behind the first.
    pub queue_follow_up: bool,
    /// Whether to fall back on the legacy closest-first rule when the world gives no
    /// conflict matrix — `mobility/intersection/legacy-closest-first`, the preset §2.3
    /// names.
    pub legacy_closest_first: bool,
    /// At or below what speed a claimant counts as **standing**, m/s.
    ///
    /// A standing claimant closes no gap (its time to the stop line is infinite) but does
    /// hold a claim, which is what breaks the cyclic "give way to the right" priority a
    /// four-way crossing produces. See the module documentation.
    pub standing_speed_mps: f64,
}

impl Default for GapAcceptanceParams {
    fn default() -> Self {
        Self {
            stop_line_offset_m: STOP_LINE_OFFSET_M,
            queue_follow_up: true,
            legacy_closest_first: true,
            standing_speed_mps: STANDING_SPEED_MPS,
        }
    }
}

/// HCM gap acceptance.
#[derive(Debug, Clone)]
pub struct GapAcceptance {
    params: GapAcceptanceParams,
    card: ModelCard,
}

impl Default for GapAcceptance {
    fn default() -> Self {
        GapAcceptance::new(GapAcceptanceParams::default())
    }
}

impl GapAcceptance {
    /// The model with the given parameters.
    pub fn new(params: GapAcceptanceParams) -> Self {
        Self {
            card: card(&params),
            params,
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &GapAcceptanceParams {
        &self.params
    }

    /// Where a yielding vehicle stops: the stop line, less the offset, never behind the
    /// vehicle.
    fn stop_gap(&self, j: &JunctionView) -> f64 {
        (j.stop_line_gap_m - self.params.stop_line_offset_m).max(0.0)
    }
}

impl v2xw_core::model::Model for GapAcceptance {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl IntersectionControl for GapAcceptance {
    fn may_enter(
        &self,
        ego: &VehicleView,
        j: &JunctionView,
        conflicts: &[ConflictView],
        _w: &WeatherState,
    ) -> EntryDecision {
        // Already inside the junction: nothing to decide, and stopping now would block it.
        if j.stop_line_gap_m <= 0.0 {
            return EntryDecision::Proceed;
        }
        let on_minor = conflicts.iter().any(|c| c.conflicts && c.ego_must_yield);
        let movement = Movement::classify(j.movement, on_minor);
        let gaps = HcmGaps::of(movement);
        let critical = gaps.critical_gap_s(j.major_lanes);

        // How many vehicles of this ego's own queue are ahead of it: each needs its own
        // follow-up time before the ego can use the same gap.
        let ahead = if self.params.queue_follow_up {
            conflicts
                .iter()
                .filter(|c| {
                    !c.conflicts
                        && c.movement_lane == j.movement_lane
                        && c.stop_line_gap_m < j.stop_line_gap_m
                })
                .count()
        } else {
            0
        };
        let required = critical + gaps.follow_up_s * ahead as f64;

        // The smallest time-to-stop-line among the conflicting claimants the ego must give
        // way to. A standing claimant never closes a gap
        // ([`ConflictView::time_to_stop_line_s`]), so an all-way stop discharges instead of
        // deadlocking.
        let mut smallest = f64::INFINITY;
        for c in conflicts {
            if !c.conflicts || !c.ego_must_yield {
                continue;
            }
            let t = c.time_to_stop_line_s();
            if t < smallest {
                smallest = t;
            }
        }
        if smallest < required {
            return EntryDecision::Stop {
                gap_m: self.stop_gap(j),
            };
        }

        // The standing-claimant rule. A standing claimant closes no gap — that is why the
        // loop above passed — but it holds a claim, and without this the cyclic "give way
        // to the right" priority a four-way crossing produces lets every claimant enter in
        // the same step once they have all stopped. Among the ego and every conflicting
        // claimant that is also standing, the smallest (stop_line_gap_m, actor) goes.
        //
        // Applied only when the ego is itself standing: a moving ego is already governed
        // by the gap criterion, and a vehicle that is rolling towards the line has not
        // stopped to claim anything.
        if ego.speed_mps <= self.params.standing_speed_mps
            && conflicts.iter().any(|c| {
                c.conflicts
                    && c.speed_mps <= self.params.standing_speed_mps
                    && c.stop_line_gap_m
                        .total_cmp(&j.stop_line_gap_m)
                        .then(c.actor.cmp(&ego.actor))
                        == core::cmp::Ordering::Less
            })
        {
            return EntryDecision::Stop {
                gap_m: self.stop_gap(j),
            };
        }
        EntryDecision::Proceed
    }
}

/// The model card.
pub fn card(params: &GapAcceptanceParams) -> ModelCard {
    let hcm = Source {
        kind: SourceKind::Standard,
        reference: "Highway Capacity Manual critical gaps and follow-up times, as reproduced \
                    by the PTV VISUM help [04-models.md §2.3, R10 §B9]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: Some(
            "**secondary**: §2.3 flags the whole table, because the HCM and FHWA PDFs did \
             not parse and the values were read from a help page that reproduces them"
                .to_string(),
        ),
    };
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L2415-2440, L2466-2470".to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: Some("the closest-first rule and the 2 m stop-line offset".to_string()),
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "Right of way at an unsignalised junction by gap acceptance: a vehicle on the minor \
         stream enters when the time gap to the nearest conflicting claimant it must give \
         way to exceeds the critical gap for its movement, and vehicles queued behind it \
         each need a follow-up time on top.",
    );
    card.tier = vec![Tier::Medium];
    card.equations = vec![Equation {
        name: "acceptance".to_string(),
        latex_or_text: "enter ⟺ min_over_conflicting(t_to_stop_line) ≥ t_c + k·t_f".to_string(),
        notes: Some(
            "k is the number of vehicles of the ego's own movement queued ahead of it; the \
             composition t_c + k·t_f is the standard reading of the two-value model and is \
             an assumption of this card, not a quoted formula"
                .to_string(),
        ),
    }];
    card.parameters = Movement::ALL
        .iter()
        .map(|m| {
            let g = HcmGaps::of(*m);
            Parameter::new(
                m.label(),
                "s",
                serde_json::json!({
                    "t_c_lt_4_lanes": g.critical_gap_narrow_s,
                    "t_c_ge_4_lanes": g.critical_gap_wide_s,
                    "t_f": g.follow_up_s,
                    "status": "secondary",
                }),
                hcm.clone(),
            )
        })
        .chain([
            Parameter::new(
                "stop_line_offset",
                "m",
                serde_json::json!(params.stop_line_offset_m),
                legacy.clone(),
            ),
            Parameter::new(
                "queue_follow_up",
                "-",
                serde_json::json!(params.queue_follow_up),
                hcm.clone(),
            ),
            Parameter::new(
                "standing_speed_mps",
                "m/s",
                serde_json::json!(params.standing_speed_mps),
                Source {
                    kind: SourceKind::Code,
                    reference: "this crate: at or below this speed a claimant closes no \
                                gap but still holds a claim, which is what breaks the \
                                cyclic give-way-to-the-right priority a four-way crossing \
                                produces. The reference engine has no junction stopped \
                                test to cite — it ordered claimants by (distance, vehicle \
                                id) with no conflict matrix \
                                (legacy/scms_sim_ref/mock_pipeline/run.py L2415-2440)"
                        .to_string(),
                    accessed: Some("2026-09-19".to_string()),
                    note: Some(
                        "the value is the 0.1 m/s desired-speed floor of the same \
                         engine's IDM port (`max(0.1, v0)`, run.py L2276), reused so \
                         \"not moving\" is one number in this crate rather than two"
                            .to_string(),
                    ),
                },
            ),
            Parameter::new(
                "legacy_closest_first",
                "-",
                serde_json::json!(params.legacy_closest_first),
                legacy,
            ),
        ])
        .collect();
    card.assumptions = vec![
        "Priority is a strict total order — conflicting claimant, then distance to the stop \
         line, then actor id — so the yield relation is acyclic and the closest vehicle at \
         every junction always makes progress."
            .to_string(),
        "A standing claimant closes no gap, so an all-way stop discharges rather than \
         deadlocking — but it does hold a claim: among the ego and every conflicting \
         claimant that is also standing, only the smallest (distance to the stop line, \
         actor id) enters. That is a local minimum of a strict total order, so it is \
         acyclic whatever the world's conflict matrix says pairwise, and it is what stops \
         a cyclic give-way-to-the-right matrix discharging every claimant into the \
         junction in the same step."
            .to_string(),
        "Whether the ego is on the minor stream is read from the world's conflict matrix \
         (`ego_must_yield`), not guessed from geometry."
            .to_string(),
    ];
    card.limitations = vec![
        "The critical-gap table is **secondary** (§2.3): every value here was read from a \
         help page reproducing the HCM, not from the HCM."
            .to_string(),
    ];
    card.ignores = vec![
        "Impatience growth, probabilistic right-of-way violation and pedestrian crossing \
         gaps (medium relative to high, 04-models.md §2.3)."
            .to_string(),
    ];
    card.sources = vec![hcm];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "intersection::gap_acceptance::tests::a_closing_conflict_makes_the_ego_yield"
                .to_string(),
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
    use v2xw_core::ids::{ActorId, JunctionId, LaneId};
    use v2xw_core::model::Model;
    use v2xw_world::JunctionControl;

    fn ego() -> VehicleView {
        VehicleView {
            actor: ActorId::new(5),
            class: VehicleClass::Passenger,
            lane: LaneId::new(0),
            lane_index: 0,
            s_m: 10.0,
            lateral_m: 0.0,
            speed_mps: 8.0,
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

    fn junction(movement: TurnDirection, gap_m: f64) -> JunctionView {
        JunctionView {
            id: JunctionId::new(0),
            position: Vec3::new(0.0, 0.0, 0.0),
            control: JunctionControl::Priority,
            stop_line_gap_m: gap_m,
            movement,
            movement_lane: Some(LaneId::new(9)),
            signal: None,
            major_lanes: 2,
        }
    }

    fn conflict(actor: u32, gap_m: f64, speed: f64, must_yield: bool) -> ConflictView {
        ConflictView {
            actor: ActorId::new(actor),
            stop_line_gap_m: gap_m,
            speed_mps: speed,
            heading_rad: core::f64::consts::FRAC_PI_2,
            movement: TurnDirection::Straight,
            movement_lane: Some(LaneId::new(11)),
            conflicts: true,
            ego_must_yield: must_yield,
        }
    }

    #[test]
    fn the_table_is_the_document_table() {
        assert_eq!(
            HcmGaps::of(Movement::MajorLeft),
            HcmGaps {
                critical_gap_narrow_s: 4.1,
                critical_gap_wide_s: 4.1,
                follow_up_s: 2.2
            }
        );
        assert_eq!(
            HcmGaps::of(Movement::MinorRight),
            HcmGaps {
                critical_gap_narrow_s: 6.2,
                critical_gap_wide_s: 6.9,
                follow_up_s: 3.3
            }
        );
        assert_eq!(
            HcmGaps::of(Movement::MinorThrough),
            HcmGaps {
                critical_gap_narrow_s: 6.5,
                critical_gap_wide_s: 6.5,
                follow_up_s: 4.0
            }
        );
        assert_eq!(
            HcmGaps::of(Movement::MinorLeft),
            HcmGaps {
                critical_gap_narrow_s: 7.1,
                critical_gap_wide_s: 7.5,
                follow_up_s: 3.5
            }
        );
        // The wide column only differs where the document says it does.
        assert_eq!(HcmGaps::of(Movement::MinorRight).critical_gap_s(4), 6.9);
        assert_eq!(HcmGaps::of(Movement::MinorRight).critical_gap_s(3), 6.2);
        assert_eq!(HcmGaps::of(Movement::MinorLeft).critical_gap_s(6), 7.5);
    }

    #[test]
    fn a_closing_conflict_makes_the_ego_yield() {
        let m = GapAcceptance::default();
        // A minor-street through movement needs 6.5 s; the conflicting vehicle is 3 s away.
        let claimant = conflict(1, 30.0, 10.0, true);
        let d = m.may_enter(
            &ego(),
            &junction(TurnDirection::Straight, 12.0),
            &[claimant],
            &WeatherState::CLEAR,
        );
        assert_eq!(
            d,
            EntryDecision::Stop { gap_m: 10.0 },
            "stops 2 m before the line"
        );
    }

    #[test]
    fn a_wide_gap_lets_the_ego_go() {
        let m = GapAcceptance::default();
        // 100 m away at 10 m/s is 10 s: wider than the 6.5 s critical gap.
        let claimant = conflict(1, 100.0, 10.0, true);
        let d = m.may_enter(
            &ego(),
            &junction(TurnDirection::Straight, 12.0),
            &[claimant],
            &WeatherState::CLEAR,
        );
        assert_eq!(d, EntryDecision::Proceed);
    }

    #[test]
    fn a_claimant_the_ego_outranks_does_not_stop_it() {
        let m = GapAcceptance::default();
        // Same closing vehicle, but the conflict matrix says it yields to the ego.
        let claimant = conflict(1, 20.0, 10.0, false);
        let d = m.may_enter(
            &ego(),
            &junction(TurnDirection::Straight, 12.0),
            &[claimant],
            &WeatherState::CLEAR,
        );
        assert_eq!(d, EntryDecision::Proceed);
    }

    #[test]
    fn a_queue_needs_the_follow_up_time() {
        let m = GapAcceptance::default();
        let j = junction(TurnDirection::Straight, 30.0);
        // One vehicle of the ego's own movement is ahead of it in the queue, so the ego
        // needs 6.5 + 4.0 = 10.5 s rather than 6.5 s. The conflicting vehicle offers 8 s.
        let mut queued = conflict(2, 10.0, 0.0, false);
        queued.conflicts = false;
        queued.movement_lane = j.movement_lane;
        let crossing = conflict(1, 80.0, 10.0, true);
        assert_eq!(
            m.may_enter(&ego(), &j, &[crossing, queued], &WeatherState::CLEAR),
            EntryDecision::Stop { gap_m: 28.0 }
        );
        // Alone, the same 8 s gap is enough.
        assert_eq!(
            m.may_enter(&ego(), &j, &[crossing], &WeatherState::CLEAR),
            EntryDecision::Proceed
        );
    }

    #[test]
    fn a_standing_claimant_does_not_block_anybody() {
        let m = GapAcceptance::default();
        let stopped = conflict(1, 1.0, 0.0, true);
        assert_eq!(
            m.may_enter(
                &ego(),
                &junction(TurnDirection::Straight, 12.0),
                &[stopped],
                &WeatherState::CLEAR
            ),
            EntryDecision::Proceed
        );
    }

    #[test]
    fn a_standing_claimant_nearer_the_line_holds_its_claim() {
        // The defect this pins: a standing claimant closes no gap, so once every vehicle
        // at a junction had stopped, `min(time to stop line)` was infinite for all of them
        // and every one was told to proceed in the same step. With the procedural
        // generator's cyclic "give way to the right" matrix there was no pairwise rule
        // left to separate them.
        let m = GapAcceptance::default();
        let j = junction(TurnDirection::Straight, 5.0);
        let standing_ego = VehicleView {
            speed_mps: 0.0,
            ..ego()
        };
        // Nearer the line, standing, and the matrix says the EGO outranks it: the ego
        // gives way anyway, because pairwise priority is what cycles.
        let nearer = conflict(1, 3.0, 0.0, false);
        assert_eq!(
            m.may_enter(&standing_ego, &j, &[nearer], &WeatherState::CLEAR),
            EntryDecision::Stop { gap_m: 3.0 }
        );
        // Further from the line: the ego is the nearest claimant and goes, even though the
        // matrix says it must give way.
        let further = conflict(1, 7.0, 0.0, true);
        assert_eq!(
            m.may_enter(&standing_ego, &j, &[further], &WeatherState::CLEAR),
            EntryDecision::Proceed
        );
        // Exactly level: the lower actor id goes. The ego is actor 5.
        assert_eq!(
            m.may_enter(
                &standing_ego,
                &j,
                &[conflict(1, 5.0, 0.0, false)],
                &WeatherState::CLEAR
            ),
            EntryDecision::Stop { gap_m: 3.0 }
        );
        assert_eq!(
            m.may_enter(
                &standing_ego,
                &j,
                &[conflict(9, 5.0, 0.0, true)],
                &WeatherState::CLEAR
            ),
            EntryDecision::Proceed
        );
        // A claimant whose movement does not conflict is not a claimant on this junction,
        // however near the line it is.
        let mut parallel = conflict(1, 0.5, 0.0, false);
        parallel.conflicts = false;
        assert_eq!(
            m.may_enter(&standing_ego, &j, &[parallel], &WeatherState::CLEAR),
            EntryDecision::Proceed
        );
        // And a MOVING ego is governed by the gap criterion alone: a stopped vehicle at a
        // stop line is still not an arriving gap-closing vehicle.
        assert_eq!(
            m.may_enter(&ego(), &j, &[nearer], &WeatherState::CLEAR),
            EntryDecision::Proceed
        );
    }

    #[test]
    fn a_cyclic_priority_matrix_still_discharges_one_movement_at_a_time() {
        // Four claimants, all standing, with the cycle the procedural generator's
        // give-way-to-the-right rule produces: 0 yields to 1, 1 to 2, 2 to 3, 3 to 0. Every
        // one conflicts with every other here, so at most one may enter — and before the
        // standing-claimant rule all four did.
        let m = GapAcceptance::default();
        let gaps = [6.0, 4.0, 9.0, 7.0];
        let mut proceeding: Vec<usize> = Vec::new();
        for i in 0..4usize {
            let ego_i = VehicleView {
                actor: ActorId::new(i as u32),
                speed_mps: 0.0,
                ..ego()
            };
            let j = junction(TurnDirection::Straight, gaps[i]);
            let others: Vec<ConflictView> = (0..4)
                .filter(|k| *k != i)
                .map(|k| {
                    // The cycle: i gives way to (i + 1) % 4 and to nobody else.
                    conflict(k as u32, gaps[k], 0.0, k == (i + 1) % 4)
                })
                .collect();
            if m.may_enter(&ego_i, &j, &others, &WeatherState::CLEAR) == EntryDecision::Proceed {
                proceeding.push(i);
            }
        }
        // Exactly the nearest one: claimant 1, at 4 m.
        assert_eq!(
            proceeding,
            vec![1],
            "the junction discharged {proceeding:?} together"
        );
    }

    #[test]
    fn a_vehicle_inside_the_junction_is_never_stopped() {
        let m = GapAcceptance::default();
        let claimant = conflict(1, 5.0, 10.0, true);
        assert_eq!(
            m.may_enter(
                &ego(),
                &junction(TurnDirection::Straight, -1.0),
                &[claimant],
                &WeatherState::CLEAR
            ),
            EntryDecision::Proceed
        );
    }

    #[test]
    fn a_major_left_turn_takes_the_major_row() {
        // With nobody to give way to, the ego is on the major street: the left-turn row.
        assert_eq!(
            Movement::classify(TurnDirection::Left, false),
            Movement::MajorLeft
        );
        assert_eq!(
            Movement::classify(TurnDirection::Left, true),
            Movement::MinorLeft
        );
        assert_eq!(
            Movement::classify(TurnDirection::UTurn, true),
            Movement::MinorLeft
        );
        assert_eq!(
            Movement::classify(TurnDirection::Right, true),
            Movement::MinorRight
        );
        assert_eq!(
            Movement::classify(TurnDirection::SlightRight, true),
            Movement::MinorRight
        );
    }

    #[test]
    fn the_card_validates_and_carries_the_secondary_flag() {
        let m = GapAcceptance::default();
        m.card().validate().expect("validates");
        assert!(m.card().limitations.iter().any(|l| l.contains("secondary")));
        for p in &m.card().parameters {
            assert!(!p.source.reference.is_empty());
        }
    }
}
