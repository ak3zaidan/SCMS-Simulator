//! `mobility/intersection/two-coloring-legacy` — the abstract tier (04-models.md §2.3).
//!
//! Every junction carries a stable 2-colouring phase; each axis is green for half of
//! `light_cycle_s` = 24 s, offset by the phase, so adjacent junctions alternate. On a grid
//! the colouring equals `(i + j) mod 2`, which is the legacy engine's historical
//! checkerboard [`run.py` L2267-2272, L2466-2470]. Vehicles halt 2 m before the stop line,
//! and a bend of at least 40° within the lookahead caps the speed at 6 m/s
//! [`run.py` L2479-2483].
//!
//! §2.3 is explicit about what this ignores: yellow, all-red, actuation and pedestrians. It
//! is kept because it is the abstract tier's intersection control *and* the parity oracle
//! for a run against the frozen corpus.
//!
//! # "Stable on any topology"
//!
//! The legacy comment claims a stable 2-colouring on any topology. What it is is a
//! breadth-first colouring: junctions are visited in id order, each uncoloured junction
//! starts a component at colour 0, and its neighbours alternate. On a bipartite graph — a
//! grid, a tree, any even-cycle topology — that is a proper 2-colouring. On a
//! non-bipartite one (an odd ring of streets) some adjacent pair necessarily shares a
//! colour, because no proper 2-colouring exists; the colouring is still *stable* (a pure
//! function of the world) and still alternates almost everywhere, and this limitation is on
//! the card rather than hidden in the claim.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::JunctionId;
use v2xw_core::weather::WeatherState;
use v2xw_world::{SignalState, TurnDirection, World};

use crate::intersection::{STOP_LINE_OFFSET_M, on_east_west_axis};
use crate::traits::IntersectionControl;
use crate::views::{ConflictView, EntryDecision, JunctionView, VehicleView};

/// The model id.
pub const MODEL_ID: &str = "mobility/intersection/two-coloring-legacy";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The legacy cycle length, seconds (`light_cycle_s`).
pub const LEGACY_LIGHT_CYCLE_S: f64 = 24.0;

/// The legacy turn-speed cap, m/s (`turn_speed_mps`).
pub const LEGACY_TURN_SPEED_MPS: f64 = 6.0;

/// The legacy bend threshold, degrees (`turn_min_angle_deg`).
pub const LEGACY_TURN_MIN_ANGLE_DEG: f64 = 40.0;

/// The parameters of the legacy two-colouring.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TwoColoringParams {
    /// Cycle length, seconds: each axis is green for half of it.
    pub light_cycle_s: f64,
    /// Speed cap through a bend, m/s.
    pub turn_speed_mps: f64,
    /// The bend angle at which the cap applies, degrees.
    pub turn_min_angle_deg: f64,
    /// Whether a *slight* turn also takes the cap. The world discretises a bend into
    /// [`TurnDirection`] bands, and the slight band spans 22.5° to 67.5°, which straddles
    /// the 40° threshold; off by default, so only a full turn (67.5° and beyond, always
    /// past the threshold) is capped.
    pub cap_slight_turns: bool,
    /// How far before the stop line a vehicle halts, metres.
    pub stop_line_offset_m: f64,
}

impl Default for TwoColoringParams {
    fn default() -> Self {
        Self {
            light_cycle_s: LEGACY_LIGHT_CYCLE_S,
            turn_speed_mps: LEGACY_TURN_SPEED_MPS,
            turn_min_angle_deg: LEGACY_TURN_MIN_ANGLE_DEG,
            cap_slight_turns: false,
            stop_line_offset_m: STOP_LINE_OFFSET_M,
        }
    }
}

/// The legacy two-colouring controller.
#[derive(Debug, Clone)]
pub struct TwoColoring {
    params: TwoColoringParams,
    phase: BTreeMap<JunctionId, u8>,
    card: ModelCard,
}

impl TwoColoring {
    /// Colours `world`'s junctions and builds the controller.
    ///
    /// The colouring is a pure function of the world: junctions are visited in id order and
    /// each component is coloured breadth-first, so two engines that load the same world
    /// get the same colours.
    pub fn new(world: &World, params: TwoColoringParams) -> Self {
        Self {
            phase: colour_junctions(world),
            card: card(&params),
            params,
        }
    }

    /// The controller with the legacy parameters.
    pub fn legacy(world: &World) -> Self {
        Self::new(world, TwoColoringParams::default())
    }

    /// The parameters in force.
    pub fn params(&self) -> &TwoColoringParams {
        &self.params
    }

    /// One junction's colour, `0` or `1`.
    pub fn phase_of(&self, junction: JunctionId) -> u8 {
        self.phase.get(&junction).copied().unwrap_or(0)
    }

    /// Every junction's colour, in junction-id order.
    pub fn phases(&self) -> impl Iterator<Item = (JunctionId, u8)> + '_ {
        self.phase.iter().map(|(j, c)| (*j, *c))
    }

    /// Whether the east-west axis is green at `junction` at `t_s`.
    ///
    /// The legacy expression, unchanged: `offset = (phase mod 2)·half_cycle`, the
    /// east-west axis is green while `floor((t + offset)/half_cycle)` is even.
    pub fn east_west_green(&self, junction: JunctionId, t_s: f64) -> bool {
        let half = 0.5 * self.params.light_cycle_s;
        let offset = f64::from(self.phase_of(junction) % 2) * half;
        let slot = ((t_s + offset) / half).floor();
        (slot as i64).rem_euclid(2) == 0
    }

    /// The state a vehicle travelling on `heading_rad` sees at `junction` at `t_s`.
    ///
    /// Two states only: this tier has no yellow, which is the first thing §2.3 says it
    /// ignores.
    pub fn state_at(&self, junction: JunctionId, heading_rad: f64, t_s: f64) -> SignalState {
        let axis_x = on_east_west_axis(heading_rad);
        if self.east_west_green(junction, t_s) == axis_x {
            SignalState::Green
        } else {
            SignalState::Red
        }
    }

    /// Whether `movement` is a bend the cap applies to.
    fn is_capped_turn(&self, movement: TurnDirection) -> bool {
        match movement {
            TurnDirection::Left | TurnDirection::Right | TurnDirection::UTurn => true,
            TurnDirection::SlightLeft | TurnDirection::SlightRight => self.params.cap_slight_turns,
            TurnDirection::Straight => false,
        }
    }
}

impl v2xw_core::model::Model for TwoColoring {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl IntersectionControl for TwoColoring {
    fn may_enter(
        &self,
        ego: &VehicleView,
        j: &JunctionView,
        _conflicts: &[ConflictView],
        _w: &WeatherState,
    ) -> EntryDecision {
        if j.stop_line_gap_m <= 0.0 {
            return EntryDecision::Proceed;
        }
        let gap = (j.stop_line_gap_m - self.params.stop_line_offset_m).max(0.0);
        // The engine fills `signal` from `state_at`, so this model reads a state exactly as
        // the fixed-time one does and the two are interchangeable at the seam.
        if j.signal.is_some_and(|s| !s.permits_entry()) {
            return EntryDecision::Stop { gap_m: gap };
        }
        if self.is_capped_turn(j.movement) && ego.speed_mps > self.params.turn_speed_mps {
            return EntryDecision::SlowTo {
                gap_m: j.stop_line_gap_m.max(0.0),
                speed_mps: self.params.turn_speed_mps,
            };
        }
        EntryDecision::Proceed
    }
}

/// Breadth-first 2-colouring of a world's junction graph, in junction-id order.
fn colour_junctions(world: &World) -> BTreeMap<JunctionId, u8> {
    // Adjacency: two junctions are neighbours when an edge joins them.
    let mut adjacency: BTreeMap<JunctionId, Vec<JunctionId>> = BTreeMap::new();
    for edge in world.roads.edges() {
        if edge.from == edge.to {
            continue; // a junction's own internal edge
        }
        adjacency.entry(edge.from).or_default().push(edge.to);
        adjacency.entry(edge.to).or_default().push(edge.from);
    }
    for list in adjacency.values_mut() {
        list.sort_unstable();
        list.dedup();
    }
    let mut colour: BTreeMap<JunctionId, u8> = BTreeMap::new();
    for j in world.roads.junctions() {
        if colour.contains_key(&j.id) {
            continue;
        }
        colour.insert(j.id, 0);
        let mut queue: VecDeque<JunctionId> = VecDeque::new();
        queue.push_back(j.id);
        while let Some(current) = queue.pop_front() {
            let c = colour[&current];
            for next in adjacency.get(&current).map_or(&[][..], |v| v.as_slice()) {
                if !colour.contains_key(next) {
                    colour.insert(*next, 1 - c);
                    queue.push_back(*next);
                }
            }
        }
    }
    colour
}

/// The model card.
pub fn card(params: &TwoColoringParams) -> ModelCard {
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L2267-2272, L2466-2470, L2479-2483"
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: Some("the frozen reference engine's signal abstraction".to_string()),
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "The abstract tier's intersection control: a stable 2-colouring of the junction \
         graph, each axis green for half a 24 s cycle offset by the junction's colour, \
         vehicles halting 2 m before the line and a 6 m/s cap through a bend. Kept as the \
         parity oracle for a run against the frozen reference corpus.",
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![
        Equation {
            name: "green".to_string(),
            latex_or_text: "offset = (phase mod 2)·half;  east-west green ⟺ \
                            floor((t + offset)/half) is even,  half = cycle/2"
                .to_string(),
            notes: Some("arithmetic only — no transcendental, so it is exact".to_string()),
        },
        Equation {
            name: "colouring".to_string(),
            latex_or_text: "breadth-first 2-colouring of the junction graph, junctions \
                            visited in id order"
                .to_string(),
            notes: Some("on a grid this equals (i + j) mod 2".to_string()),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "light_cycle",
            "s",
            serde_json::json!(params.light_cycle_s),
            legacy.clone(),
        ),
        Parameter::new(
            "turn_speed",
            "m/s",
            serde_json::json!(params.turn_speed_mps),
            legacy.clone(),
        ),
        Parameter::new(
            "turn_min_angle",
            "deg",
            serde_json::json!(params.turn_min_angle_deg),
            legacy.clone(),
        ),
        Parameter::new(
            "cap_slight_turns",
            "-",
            serde_json::json!(params.cap_slight_turns),
            legacy.clone(),
        ),
        Parameter::new(
            "stop_line_offset",
            "m",
            serde_json::json!(params.stop_line_offset_m),
            legacy,
        ),
    ];
    card.assumptions = vec![
        "The colouring is breadth-first and therefore a proper 2-colouring exactly on a \
         bipartite topology; see the module documentation."
            .to_string(),
        "The bend threshold is applied through the world's `TurnDirection` bands rather \
         than to a measured angle, because that is the discretisation the world carries; \
         `cap_slight_turns` exposes the one band that straddles 40°."
            .to_string(),
    ];
    card.limitations = vec![
        "No yellow, no all-red, no actuation, no pedestrians — every one of them is \
         something §2.3 says this tier ignores."
            .to_string(),
        "On a non-bipartite junction graph some adjacent pair shares a colour, because no \
         proper 2-colouring exists."
            .to_string(),
    ];
    card.ignores = vec![
        "Everything the medium tier's signal and gap-acceptance models do \
         (04-models.md §2.3)."
            .to_string(),
    ];
    card.sources = vec![Source::new(
        SourceKind::Code,
        "legacy/scms_sim_ref/mock_pipeline/run.py",
    )];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "intersection::two_coloring::tests::a_grid_is_coloured_like_the_checkerboard"
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
    use v2xw_core::ids::{ActorId, LaneId};
    use v2xw_core::model::Model;
    use v2xw_world::{ImportOptions, JunctionControl, procedural::GridParams};

    fn world() -> World {
        v2xw_world::procedural::grid(&GridParams::legacy(), &ImportOptions::default())
            .expect("grid")
    }

    fn ego(speed: f64, heading: f64) -> VehicleView {
        VehicleView {
            actor: ActorId::new(0),
            class: VehicleClass::Passenger,
            lane: LaneId::new(0),
            lane_index: 0,
            s_m: 0.0,
            lateral_m: 0.0,
            speed_mps: speed,
            accel_mps2: 0.0,
            heading_rad: heading,
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

    fn junction(
        id: JunctionId,
        movement: TurnDirection,
        signal: Option<SignalState>,
        gap: f64,
    ) -> JunctionView {
        JunctionView {
            id,
            position: Vec3::ZERO,
            control: JunctionControl::Uncontrolled,
            stop_line_gap_m: gap,
            movement,
            movement_lane: None,
            signal,
            major_lanes: 2,
        }
    }

    #[test]
    fn a_grid_is_coloured_like_the_checkerboard() {
        let w = world();
        let c = TwoColoring::legacy(&w);
        // The procedural grid numbers junctions row-major, so the colour of junction
        // (i, j) must be (i + j) mod 2 up to a global flip.
        let cols = GridParams::legacy().cols as usize;
        let first = c.phase_of(JunctionId::new(0));
        for (index, j) in w.roads.junctions().iter().enumerate() {
            let (col, row) = (index % cols, index / cols);
            let want = ((col + row) % 2) as u8 ^ first;
            assert_eq!(c.phase_of(j.id), want, "junction {index} at ({col}, {row})");
        }
        // Adjacent junctions alternate, which is the property the model needs.
        for edge in w.roads.edges() {
            if edge.from == edge.to {
                continue;
            }
            assert_ne!(
                c.phase_of(edge.from),
                c.phase_of(edge.to),
                "edge {:?} joins two junctions of the same colour",
                edge.id
            );
        }
    }

    #[test]
    fn the_two_axes_are_green_in_turn() {
        let w = world();
        let c = TwoColoring::legacy(&w);
        let j = JunctionId::new(0);
        let half = 0.5 * LEGACY_LIGHT_CYCLE_S;
        // Whatever the phase, exactly one axis is green at any instant, and the state
        // flips every half cycle.
        for k in 0..8 {
            let t = f64::from(k) * half + 0.5;
            let ew = c.east_west_green(j, t);
            assert_ne!(
                c.state_at(j, 0.0, t) == SignalState::Green,
                c.state_at(j, core::f64::consts::FRAC_PI_2, t) == SignalState::Green,
                "both axes green at t = {t}"
            );
            assert_eq!(
                c.east_west_green(j, t + half),
                !ew,
                "it flips each half cycle"
            );
            assert_eq!(c.east_west_green(j, t + 2.0 * half), ew, "and is periodic");
        }
    }

    #[test]
    fn adjacent_junctions_are_offset() {
        let w = world();
        let c = TwoColoring::legacy(&w);
        let a = JunctionId::new(0);
        let b = w
            .roads
            .edges()
            .iter()
            .find(|e| e.from == a && e.to != a)
            .map(|e| e.to)
            .expect("a neighbour");
        assert_ne!(c.phase_of(a), c.phase_of(b));
        assert_ne!(c.east_west_green(a, 1.0), c.east_west_green(b, 1.0));
    }

    #[test]
    fn red_stops_two_metres_before_the_line() {
        let w = world();
        let c = TwoColoring::legacy(&w);
        let d = c.may_enter(
            &ego(10.0, 0.0),
            &junction(
                JunctionId::new(0),
                TurnDirection::Straight,
                Some(SignalState::Red),
                25.0,
            ),
            &[],
            &WeatherState::CLEAR,
        );
        assert_eq!(d, EntryDecision::Stop { gap_m: 23.0 });
    }

    #[test]
    fn a_bend_caps_the_speed() {
        let w = world();
        let c = TwoColoring::legacy(&w);
        let d = c.may_enter(
            &ego(12.0, 0.0),
            &junction(
                JunctionId::new(0),
                TurnDirection::Left,
                Some(SignalState::Green),
                20.0,
            ),
            &[],
            &WeatherState::CLEAR,
        );
        assert_eq!(
            d,
            EntryDecision::SlowTo {
                gap_m: 20.0,
                speed_mps: LEGACY_TURN_SPEED_MPS
            }
        );
        // A vehicle already slower than the cap is not slowed.
        let e = c.may_enter(
            &ego(5.0, 0.0),
            &junction(
                JunctionId::new(0),
                TurnDirection::Left,
                Some(SignalState::Green),
                20.0,
            ),
            &[],
            &WeatherState::CLEAR,
        );
        assert_eq!(e, EntryDecision::Proceed);
        // And a straight movement never is.
        let f = c.may_enter(
            &ego(12.0, 0.0),
            &junction(
                JunctionId::new(0),
                TurnDirection::Straight,
                Some(SignalState::Green),
                20.0,
            ),
            &[],
            &WeatherState::CLEAR,
        );
        assert_eq!(f, EntryDecision::Proceed);
    }

    #[test]
    fn the_colouring_is_a_pure_function_of_the_world() {
        let w = world();
        let a = TwoColoring::legacy(&w);
        let b = TwoColoring::legacy(&w);
        assert_eq!(
            a.phases().collect::<Vec<_>>(),
            b.phases().collect::<Vec<_>>()
        );
        // And of a fresh copy of the same world.
        let w2 = world();
        let c = TwoColoring::legacy(&w2);
        assert_eq!(
            a.phases().collect::<Vec<_>>(),
            c.phases().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_card_validates_and_is_abstract_tier() {
        let w = world();
        let c = TwoColoring::legacy(&w);
        c.card().validate().expect("validates");
        assert_eq!(c.card().tier, vec![Tier::Abstract]);
        assert!(matches!(
            w.roads.junctions()[0].control,
            JunctionControl::Uncontrolled | JunctionControl::Priority
        ));
    }
}
