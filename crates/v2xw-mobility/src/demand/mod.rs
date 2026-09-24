//! Demand — 04-models.md §2.4.
//!
//! * [`poisson`] — `mobility/demand/poisson-thinned`: the arrival process, candidates at a
//!   boosted rate thinned by the time-of-day multiplier.
//! * [`od`] — `mobility/demand/od-gravity`: where a trip starts and ends.
//! * [`tr36885`] — `mobility/demand/tr36885-drop`: the 3GPP evaluation drops, for radio
//!   validation runs.
//!
//! This module holds what all three share: the shape functions, the fleet mix and the
//! desired-speed law.

pub mod od;
pub mod poisson;
pub mod tr36885;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{Parameter, Source, SourceKind};
use v2xw_core::math;
use v2xw_core::rng::RngStream;

use crate::classes::{LegacyClass, VehicleClass};

pub use od::{OdLaw, OdModel, OdParams};
pub use poisson::{PoissonDemand, PoissonParams};
pub use tr36885::{DropModel, DropParams, DropVariant};

/// A demand model that produces nothing.
///
/// What a validation run wants: the fundamental-diagram check of 04-models.md §2.9 places
/// its own vehicles at prescribed positions, and a scenario that injects trips through
/// [`crate::views::MobilityCommand::Spawn`] wants the same. It is a model rather than an
/// `Option` because [`crate::traits::Mobility::init`] takes a demand model, and "no demand"
/// is a modelling decision that belongs on a card like any other.
#[derive(Debug, Clone)]
pub struct NoDemand {
    card: v2xw_core::card::ModelCard,
}

impl Default for NoDemand {
    fn default() -> Self {
        Self::new()
    }
}

impl NoDemand {
    /// The model.
    pub fn new() -> Self {
        let mut card = v2xw_core::card::ModelCard::new(
            "mobility/demand/none",
            v2xw_core::card::Family::Mobility,
            "1.0.0",
            "No demand at all: a run whose vehicles are placed by the scenario, such as the \
             fundamental-diagram validation of 04-models.md §2.9 or a 3GPP drop that has \
             already happened.",
        );
        card.tier = vec![
            v2xw_core::card::Tier::Abstract,
            v2xw_core::card::Tier::Medium,
            v2xw_core::card::Tier::High,
        ];
        card.assumptions = vec!["Vehicles arrive by another route than this one.".to_string()];
        card.validation =
            v2xw_core::card::Validation::new(v2xw_core::card::ValidationStatus::UnitTested);
        Self { card }
    }
}

impl v2xw_core::model::Model for NoDemand {
    fn card(&self) -> &v2xw_core::card::ModelCard {
        &self.card
    }
}

impl crate::traits::Demand for NoDemand {
    fn spawns_in(
        &mut self,
        _ctx: &mut dyn crate::ctx::MobCtx,
        _from: v2xw_core::time::SimTime,
        _to: v2xw_core::time::SimTime,
    ) -> Vec<crate::views::TripRequest> {
        Vec::new()
    }
}

/// The legacy arrival rate, candidate trips per second (`arrival_rate`).
pub const LEGACY_ARRIVAL_RATE_PER_S: f64 = 2.0;

/// The legacy trip-speed range, m/s (`trip_speed_min`, `trip_speed_max`).
pub const LEGACY_TRIP_SPEED_RANGE_MPS: (f64, f64) = (8.0, 18.0);

/// The time-of-day shape of the demand (04-models.md §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DemandProfile {
    /// `m(f) = 1`: the same rate all run long.
    #[default]
    Uniform,
    /// `m(f) = 0.2 + 0.8·min(1, exp(−((f − 0.25)/0.09)²) + exp(−((f − 0.75)/0.09)²))`:
    /// a morning and an evening peak with a quiet midday.
    Rush,
    /// `m(f) = 0.15 + 0.25·f`: sparse throughout, gently ramping.
    Night,
}

impl DemandProfile {
    /// Every profile.
    pub const ALL: [DemandProfile; 3] = [
        DemandProfile::Uniform,
        DemandProfile::Rush,
        DemandProfile::Night,
    ];

    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            DemandProfile::Uniform => "uniform",
            DemandProfile::Rush => "rush",
            DemandProfile::Night => "night",
        }
    }

    /// The multiplier at `f = t / duration ∈ [0, 1]`, exactly as §2.4 writes it.
    ///
    /// The two Gaussians are evaluated through [`v2xw_core::math::exp`], never
    /// `f64::exp`: the standard library's delegates to the platform libm, whose last bit
    /// differs between platforms (ADR 0003), and this value gates a coin flip that decides
    /// whether a vehicle exists.
    pub fn multiplier(self, f: f64) -> f64 {
        match self {
            DemandProfile::Uniform => 1.0,
            DemandProfile::Rush => {
                let morning = math::exp(-sq((f - 0.25) / 0.09));
                let evening = math::exp(-sq((f - 0.75) / 0.09));
                0.2 + 0.8 * (morning + evening).min(1.0)
            }
            DemandProfile::Night => 0.15 + 0.25 * f,
        }
    }

    /// The largest value the multiplier can take, which is what the candidate rate must be
    /// boosted by for the thinning to be exact.
    ///
    /// `1.0` for all three profiles: `uniform` is constantly one, `rush` reaches
    /// `0.2 + 0.8·1 = 1` at each peak, and `night` tops out at `0.15 + 0.25 = 0.4`. Stated
    /// as a function rather than a constant so a new profile cannot forget it.
    pub fn supremum(self) -> f64 {
        match self {
            DemandProfile::Uniform => 1.0,
            DemandProfile::Rush => 1.0,
            DemandProfile::Night => 0.4,
        }
    }
}

fn sq(x: f64) -> f64 {
    x * x
}

/// Which classes the fleet is drawn from (04-models.md §2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FleetMix {
    /// Every vehicle is a car. The legacy `car` fleet.
    #[default]
    CarsOnly,
    /// The legacy `mixed` fleet: car 0.75, motorcycle 0.08, truck 0.10, bus 0.07
    /// ([`run.py` L142-146]).
    LegacyMixed,
    /// The MOBIL study's mix: 20 % trucks, the rest cars [Kesting 2007, R10 §B3].
    Kesting2007,
    /// The scenario's own shares (`actors.vehicles.classes`), in parts per million, one
    /// per [`VehicleClass::ALL`] entry in that order.
    Shares([u32; 12]),
}

impl FleetMix {
    /// A mix from `(class, fraction)` pairs, fractions summing to 1.
    pub fn from_shares(shares: &[(VehicleClass, f64)]) -> FleetMix {
        let mut ppm = [0u32; 12];
        for (class, fraction) in shares {
            if let Some(i) = VehicleClass::ALL.iter().position(|c| c == class) {
                ppm[i] = (fraction.clamp(0.0, 1.0) * 1e6).round() as u32;
            }
        }
        FleetMix::Shares(ppm)
    }
}

impl FleetMix {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            FleetMix::CarsOnly => "cars-only",
            FleetMix::LegacyMixed => "legacy-mixed",
            FleetMix::Kesting2007 => "kesting-2007",
            FleetMix::Shares(_) => "scenario-shares",
        }
    }

    /// The classes and their weights, in a fixed order.
    pub fn weights(self) -> Vec<(VehicleClass, f64)> {
        match self {
            FleetMix::CarsOnly => vec![(VehicleClass::Passenger, 1.0)],
            FleetMix::LegacyMixed => LegacyClass::mixed_fleet()
                .iter()
                .map(|(c, w)| (c.sumo_equivalent(), *w))
                .collect(),
            FleetMix::Kesting2007 => {
                vec![(VehicleClass::Passenger, 0.8), (VehicleClass::Truck, 0.2)]
            }
            FleetMix::Shares(ppm) => VehicleClass::ALL
                .iter()
                .zip(ppm)
                .filter(|(_, w)| *w > 0)
                .map(|(c, w)| (*c, f64::from(w) * 1e-6))
                .collect(),
        }
    }

    /// Draws a class from `rng`.
    ///
    /// One draw, always, whatever the mix: a sampler whose draw count depended on the mix
    /// would shift every later draw in the stream when the mix changed.
    pub fn draw(self, rng: &mut RngStream) -> VehicleClass {
        let weights = self.weights();
        let values: Vec<f64> = weights.iter().map(|(_, w)| *w).collect();
        let index = rng.choose_index(&values);
        weights[index].0
    }
}

/// How a trip's desired speed is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "law", rename_all = "kebab-case")]
pub enum SpeedLaw {
    /// The legacy law: uniform in `[min, max]`, multiplied by the class's speed multiplier
    /// ([`run.py` L1740], `trip_speed_min` 8, `trip_speed_max` 18).
    LegacyUniform {
        /// Lower bound, m/s.
        min_mps: f64,
        /// Upper bound, m/s.
        max_mps: f64,
    },
    /// The SUMO law: a normal draw about the class's desired speed with relative standard
    /// deviation `speedDev` (R10 §B4), clamped to `[0.2·mean, maxSpeed]`.
    ///
    /// The clamp is this crate's: SUMO's own truncation rule is not in the research sheets,
    /// and an unclamped normal draw can be negative. It is recorded on the card.
    ClassSpeedDev,
}

impl Default for SpeedLaw {
    fn default() -> Self {
        SpeedLaw::LegacyUniform {
            min_mps: LEGACY_TRIP_SPEED_RANGE_MPS.0,
            max_mps: LEGACY_TRIP_SPEED_RANGE_MPS.1,
        }
    }
}

impl SpeedLaw {
    /// Draws a desired speed for `class` from `rng`. Exactly one draw either way.
    pub fn draw(self, class: VehicleClass, rng: &mut RngStream) -> f64 {
        match self {
            SpeedLaw::LegacyUniform { min_mps, max_mps } => {
                let base = rng.uniform(min_mps, max_mps);
                let mult = match class {
                    VehicleClass::Motorcycle | VehicleClass::Moped | VehicleClass::Scooter => {
                        LegacyClass::Motorcycle.spec().speed_mult
                    }
                    VehicleClass::Truck | VehicleClass::Trailer | VehicleClass::Delivery => {
                        LegacyClass::Truck.spec().speed_mult
                    }
                    VehicleClass::Bus | VehicleClass::Coach => LegacyClass::Bus.spec().speed_mult,
                    _ => LegacyClass::Car.spec().speed_mult,
                };
                base * mult
            }
            SpeedLaw::ClassSpeedDev => {
                let spec = class.spec();
                let mean = spec.desired_speed_mps();
                let drawn = rng.normal(mean, spec.speed_dev * mean);
                drawn.clamp(0.2 * mean, spec.max_speed_mps)
            }
        }
    }
}

/// The card parameters the shape functions, the fleet mix and the speed law need.
pub fn shared_parameters(profile: DemandProfile, mix: FleetMix, speed: SpeedLaw) -> Vec<Parameter> {
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L1705-1743 (`demand_mult`, the \
                    thinning loop and the trip-speed draw)"
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: None,
    };
    vec![
        Parameter::new(
            "demand_profile",
            "-",
            serde_json::json!(profile.label()),
            legacy.clone(),
        ),
        Parameter::new(
            "shape.rush",
            "1",
            serde_json::json!(
                "0.2 + 0.8·min(1, exp(−((f − 0.25)/0.09)²) + exp(−((f − 0.75)/0.09)²))"
            ),
            legacy.clone(),
        ),
        Parameter::new(
            "shape.night",
            "1",
            serde_json::json!("0.15 + 0.25·f"),
            legacy.clone(),
        ),
        Parameter::new("shape.uniform", "1", serde_json::json!("1"), legacy.clone()),
        Parameter::new(
            "fleet_mix",
            "-",
            serde_json::json!({
                "selected": mix.label(),
                "weights": mix.weights().iter().map(|(c, w)| (c.as_str(), *w))
                    .collect::<Vec<_>>(),
            }),
            legacy.clone(),
        ),
        Parameter::new(
            "speed_law",
            "-",
            serde_json::json!(match speed {
                SpeedLaw::LegacyUniform { min_mps, max_mps } => serde_json::json!({
                    "law": "legacy-uniform", "min_mps": min_mps, "max_mps": max_mps,
                }),
                SpeedLaw::ClassSpeedDev => serde_json::json!({"law": "class-speed-dev"}),
            }),
            legacy,
        ),
        Parameter {
            name: "wall_clock_mapping".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!("f = t / duration"),
            range: None,
            source: Source::todo_calibrate(
                "when t0 is a wall-clock date, §2.4 replaces f by clock hour / 24 so the \
                 rush peaks fall at 06:00 and 18:00",
            ),
            calibration: Some(
                "04-models.md §2.4's plan, unchanged: replace the two Gaussians by an hourly \
                 profile from a public count dataset for the Phase 2 city. Until then the \
                 mapping is a design choice, and `DemandProfile::multiplier` takes the \
                 fraction its caller computes rather than deciding the mapping itself."
                    .to_string(),
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};

    #[test]
    fn the_rush_shape_matches_the_documented_formula() {
        let p = DemandProfile::Rush;
        // At the morning peak f = 0.25 the first Gaussian is 1 and the second is
        // exp(−(0.5/0.09)²) ≈ exp(−30.9) ≈ 0, so min(1, …) = 1 and m = 1.0.
        assert!((p.multiplier(0.25) - 1.0).abs() < 1e-12);
        assert!((p.multiplier(0.75) - 1.0).abs() < 1e-12);
        // At f = 0 both Gaussians are tiny: exp(−(0.25/0.09)²) = exp(−7.716…).
        let sq = |x: f64| x * x;
        let want0 = 0.2
            + 0.8
                * (math::exp(-sq((0.0 - 0.25) / 0.09)) + math::exp(-sq((0.0 - 0.75) / 0.09)))
                    .min(1.0);
        assert!(
            (p.multiplier(0.0) - want0).abs() < 1e-12,
            "{}",
            p.multiplier(0.0)
        );
        assert!(
            (want0 - 0.2003564941).abs() < 1e-9,
            "the hand value: {want0}"
        );
        // Midday, f = 0.5: symmetric, both Gaussians exp(−(0.25/0.09)²).
        let want_mid = 0.2 + 0.8 * (2.0 * math::exp(-sq(0.25 / 0.09))).min(1.0);
        assert!((p.multiplier(0.5) - want_mid).abs() < 1e-12);
        assert!(
            (want_mid - 0.2007129882).abs() < 1e-9,
            "the hand value: {want_mid}"
        );
        // Between the trough and the peak it rises monotonically.
        assert!(p.multiplier(0.1) < p.multiplier(0.2));
        assert!(p.multiplier(0.2) <= p.multiplier(0.25));
        // The floor is 0.2 and the supremum 1.0.
        for k in 0..=100 {
            let f = f64::from(k) / 100.0;
            let m = p.multiplier(f);
            assert!((0.2 - 1e-12..=1.0 + 1e-12).contains(&m), "f={f} m={m}");
        }
    }

    #[test]
    fn the_night_shape_matches_the_documented_formula() {
        let p = DemandProfile::Night;
        assert!((p.multiplier(0.0) - 0.15).abs() < 1e-12);
        assert!((p.multiplier(0.5) - 0.275).abs() < 1e-12);
        assert!((p.multiplier(1.0) - 0.4).abs() < 1e-12);
        assert_eq!(p.supremum(), 0.4);
    }

    #[test]
    fn the_uniform_shape_is_one() {
        for k in 0..=10 {
            assert_eq!(DemandProfile::Uniform.multiplier(f64::from(k) / 10.0), 1.0);
        }
        assert_eq!(DemandProfile::Uniform.supremum(), 1.0);
    }

    #[test]
    fn the_supremum_bounds_the_multiplier() {
        for p in DemandProfile::ALL {
            for k in 0..=200 {
                let f = f64::from(k) / 200.0;
                assert!(
                    p.multiplier(f) <= p.supremum() + 1e-12,
                    "{} exceeds its supremum at f={f}",
                    p.label()
                );
            }
        }
    }

    #[test]
    fn the_legacy_fleet_mix_draws_the_documented_shares() {
        let registry = RngRegistry::new(11);
        let mut rng = registry.ephemeral(RngDomain::Spawn, EntityRef::Global);
        let mut cars = 0;
        let mut others = 0;
        for _ in 0..20_000 {
            match FleetMix::LegacyMixed.draw(&mut rng) {
                VehicleClass::Passenger => cars += 1,
                _ => others += 1,
            }
        }
        let share = f64::from(cars) / f64::from(cars + others);
        assert!((share - 0.75).abs() < 0.02, "car share {share}");
    }

    #[test]
    fn the_legacy_speed_law_draws_inside_its_range() {
        let registry = RngRegistry::new(3);
        let mut rng = registry.ephemeral(RngDomain::DesiredSpeed, EntityRef::Global);
        let law = SpeedLaw::default();
        for _ in 0..1000 {
            let v = law.draw(VehicleClass::Passenger, &mut rng);
            assert!((8.0..=18.0).contains(&v), "{v}");
        }
        // A truck's multiplier is 0.8, so its band is [6.4, 14.4].
        for _ in 0..1000 {
            let v = law.draw(VehicleClass::Truck, &mut rng);
            assert!((6.4..=14.4).contains(&v), "{v}");
        }
    }

    #[test]
    fn the_class_speed_law_stays_positive_and_below_the_physical_maximum() {
        let registry = RngRegistry::new(5);
        let mut rng = registry.ephemeral(RngDomain::DesiredSpeed, EntityRef::Global);
        for class in VehicleClass::ALL {
            for _ in 0..200 {
                let v = SpeedLaw::ClassSpeedDev.draw(class, &mut rng);
                assert!(v > 0.0 && v <= class.spec().max_speed_mps, "{class} {v}");
            }
        }
    }

    #[test]
    fn every_shared_parameter_declares_a_source() {
        for p in shared_parameters(
            DemandProfile::Rush,
            FleetMix::LegacyMixed,
            SpeedLaw::default(),
        ) {
            if p.source.kind == SourceKind::TodoCalibrate {
                assert!(p.calibration.as_ref().is_some_and(|c| !c.trim().is_empty()));
            } else {
                assert!(!p.source.reference.trim().is_empty());
            }
        }
    }
}
