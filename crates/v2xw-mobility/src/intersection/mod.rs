//! Intersection control — 04-models.md §2.3.
//!
//! Three models, one per row of §2.3 that the native tiers own:
//!
//! * [`gap_acceptance`] — `mobility/intersection/gap-acceptance-hcm`, medium tier.
//! * [`signal_fixed_time`] — `mobility/intersection/signal-fixed-time`, medium tier, which
//!   also *generates* a fixed-time plan for a junction whose source gave only signal
//!   presence (which is all an OSM import gives, 04-models.md §1.2).
//! * [`two_coloring`] — `mobility/intersection/two-coloring-legacy`, abstract tier.
//!
//! `roundabout-fhwa` is not implemented: §2.3 gives its capacity figures as *validation
//! targets*, not behavioural parameters, and its behaviour is "yield at entry with the gap
//! acceptance model above" — which is [`gap_acceptance`] applied to a roundabout junction.
//! The FHWA capacity ceilings are carried in [`roundabout`] as the targets they are.
//!
//! # How a decision reaches a vehicle
//!
//! Every model here returns an [`EntryDecision`], and the engine turns it into a *virtual
//! leader* for the car-following model (see [`crate::views::LeaderView`]). A red light is a
//! stationary vehicle 2 m before the stop line; a junction the ego must yield at is the
//! same. One equation therefore produces every deceleration, which is both the legacy
//! engine's design [`run.py` L2466-2483] and the reason a vehicle decelerates smoothly for
//! a signal instead of stopping dead on it.

pub mod gap_acceptance;
pub mod signal_fixed_time;
pub mod two_coloring;

pub use gap_acceptance::{GapAcceptance, HcmGaps, Movement};
pub use signal_fixed_time::{FixedTimeSignals, SignalPlanParams};
pub use two_coloring::{TwoColoring, TwoColoringParams};

use v2xw_core::math;

/// The legacy stop-line offset: a vehicle halts this far before the line
/// ([`run.py` L2467`], `code (legacy)`).
pub const STOP_LINE_OFFSET_M: f64 = 2.0;

/// The heading window that makes two movements conflict, radians
/// (04-models.md §2.3, `code (legacy)` [`run.py` L2415-2440]).
///
/// Cross traffic is a heading difference in `(45°, 135°)`: below 45° the two are going the
/// same way and the car-following model already handles them; at or above 135° they are
/// near-opposing through movements, which pass side by side rather than crossing.
pub const CONFLICT_HEADING_WINDOW_RAD: (f64, f64) = (
    45.0 * core::f64::consts::PI / 180.0,
    135.0 * core::f64::consts::PI / 180.0,
);

/// The absolute difference between two headings, radians, in `[0, π]`.
pub fn heading_difference(a_rad: f64, b_rad: f64) -> f64 {
    let d = v2xw_world::model::normalise_angle(a_rad - b_rad);
    d.abs()
}

/// True if two headings are cross traffic by the legacy rule.
pub fn headings_conflict(a_rad: f64, b_rad: f64) -> bool {
    let d = heading_difference(a_rad, b_rad);
    d > CONFLICT_HEADING_WINDOW_RAD.0 && d < CONFLICT_HEADING_WINDOW_RAD.1
}

/// True if two headings are the same direction of travel (within 45°), which is the window
/// the leader search and the neighbour classification used geometrically before the lane
/// graph replaced them.
pub fn headings_same_direction(a_rad: f64, b_rad: f64) -> bool {
    heading_difference(a_rad, b_rad) <= CONFLICT_HEADING_WINDOW_RAD.0
}

/// Whether a vehicle travelling on `heading_rad` is on the east-west axis, which is the
/// axis test the legacy two-colouring uses ([`run.py` L2269]).
pub fn on_east_west_axis(heading_rad: f64) -> bool {
    math::cos(heading_rad).abs() >= math::sin(heading_rad).abs()
}

/// FHWA roundabout capacity and operations figures — §2.3, R10 §B17.
///
/// These are **validation targets**, not behavioural parameters: a roundabout junction runs
/// [`gap_acceptance`], and these are the numbers a capacity check compares its throughput
/// against. They are carried here so the check has one place to read them from and so they
/// appear on a model card rather than in a test.
pub mod roundabout {
    /// Maximum circulating flow on a single-lane approach before a second lane is needed,
    /// veh/h.
    pub const MAX_CIRCULATING_SINGLE_LANE_VEH_H: f64 = 1800.0;

    /// Single-lane exit flow ceiling, veh/h (practical range 1,200-1,300; theoretical about
    /// 1,400).
    pub const MAX_EXIT_SINGLE_LANE_VEH_H: f64 = 1200.0;

    /// Design ceiling on the degree of saturation.
    pub const DESIGN_DEGREE_OF_SATURATION: f64 = 0.85;

    /// Passenger-car equivalents: car, single-unit truck or bus, truck with trailer,
    /// bicycle or motorcycle.
    pub const PASSENGER_CAR_EQUIVALENTS: [(&str, f64); 4] = [
        ("car", 1.0),
        ("single-unit-truck-or-bus", 1.5),
        ("truck-with-trailer", 2.0),
        ("bicycle-or-motorcycle", 0.5),
    ];

    /// Short-lane capacity multiplier by the number of vehicle spaces `n_f`.
    pub const SHORT_LANE_MULTIPLIER: [(u32, f64); 7] = [
        (0, 0.500),
        (1, 0.707),
        (2, 0.794),
        (4, 0.871),
        (6, 0.906),
        (8, 0.926),
        (10, 0.939),
    ];

    /// The queue estimate `L = v·d/3600`, metres, for flow `v` (veh/h) and delay `d` (s).
    pub fn queue_estimate_m(flow_veh_h: f64, delay_s: f64) -> f64 {
        flow_veh_h * delay_s / 3600.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conflict_window_is_forty_five_to_one_hundred_and_thirty_five_degrees() {
        let deg = |d: f64| d * core::f64::consts::PI / 180.0;
        assert!(!headings_conflict(0.0, deg(30.0)), "same direction");
        assert!(headings_conflict(0.0, deg(90.0)), "crossing");
        assert!(!headings_conflict(0.0, deg(180.0)), "opposing");
        assert!(!headings_conflict(0.0, deg(150.0)), "near-opposing");
        assert!(headings_same_direction(0.0, deg(44.0)));
        assert!(!headings_same_direction(0.0, deg(46.0)));
        // Wrapping: 350° and 10° are 20° apart, not 340°.
        assert!(headings_same_direction(deg(350.0), deg(10.0)));
    }

    #[test]
    fn the_axis_test_splits_the_compass() {
        let deg = |d: f64| d * core::f64::consts::PI / 180.0;
        assert!(on_east_west_axis(0.0));
        assert!(on_east_west_axis(deg(180.0)));
        assert!(!on_east_west_axis(deg(90.0)));
        assert!(!on_east_west_axis(deg(270.0)));
    }

    #[test]
    fn the_roundabout_targets_are_the_document_values() {
        assert_eq!(roundabout::MAX_CIRCULATING_SINGLE_LANE_VEH_H, 1800.0);
        assert_eq!(roundabout::MAX_EXIT_SINGLE_LANE_VEH_H, 1200.0);
        assert_eq!(roundabout::DESIGN_DEGREE_OF_SATURATION, 0.85);
        assert_eq!(roundabout::SHORT_LANE_MULTIPLIER[0], (0, 0.500));
        assert_eq!(roundabout::SHORT_LANE_MULTIPLIER[6], (10, 0.939));
        assert!((roundabout::queue_estimate_m(1800.0, 20.0) - 10.0).abs() < 1e-12);
    }
}
