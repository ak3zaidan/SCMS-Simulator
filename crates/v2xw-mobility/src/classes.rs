//! Vehicle classes and dimensions — 04-models.md §2.7.
//!
//! Three cited tables, transcribed and nothing else:
//!
//! * [`VehicleClass`] is `mobility/classes/sumo-vtypes`, the SUMO *Vehicle Type Parameter
//!   Defaults* table (R10 §B4). It is the default fleet: the `Dims` in every
//!   [`v2xw_core::kinematics::Kinematics`] come from here and the reference point is the
//!   rear-axle centre (03-interfaces.md §1).
//! * [`LegacyClass`] is `mobility/classes/legacy-fleet`, the frozen reference engine's four
//!   classes with their speed multipliers and IDM parameters
//!   (`code (legacy)` [`run.py` L142-146]).
//! * [`Tr37885Type`] is `mobility/classes/tr37885-types`, the three 3GPP evaluation vehicle
//!   types with their antenna heights (TR 37.885 §6.1.3, R2c).
//!
//! No value here is invented. Where a table gives km/h the conversion to m/s is written as
//! a division by 3.6 so the arithmetic is visible.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::geom::Dims;
use v2xw_world::ClassMask;

/// Converts km/h to m/s. Division only, so it is exact on every platform.
const fn kmh(v: f64) -> f64 {
    v / 3.6
}

/// One row of the SUMO vType defaults table (R10 §B4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClassSpec {
    /// Body length, metres.
    pub length_m: f64,
    /// Body width, metres.
    pub width_m: f64,
    /// Body height, metres.
    pub height_m: f64,
    /// Mass, kilograms.
    pub mass_kg: f64,
    /// Minimum standstill gap, metres (SUMO `minGap`, the IDM `s0`).
    pub min_gap_m: f64,
    /// Comfortable acceleration, m/s² (SUMO `accel`, the IDM `a_max`).
    pub accel_mps2: f64,
    /// Comfortable deceleration, m/s² (SUMO `decel`, the IDM `b`).
    pub decel_mps2: f64,
    /// Emergency deceleration, m/s² (SUMO `emergencyDecel`).
    pub emergency_decel_mps2: f64,
    /// Physical maximum speed, m/s (SUMO `maxSpeed`).
    pub max_speed_mps: f64,
    /// Desired maximum speed, m/s, where the table gives one distinct from `maxSpeed`
    /// (bicycle 20 km/h, pedestrian 5 km/h, scooter 20 km/h).
    pub desired_max_speed_mps: Option<f64>,
    /// Speed deviation (SUMO `speedDev`): the relative standard deviation of the
    /// per-driver desired-speed draw.
    pub speed_dev: f64,
}

impl ClassSpec {
    /// The body dimensions as the core geometry type.
    pub fn dims(&self) -> Dims {
        Dims::new(self.length_m, self.width_m, self.height_m)
    }

    /// The desired speed this class drives at when nothing else caps it: the desired
    /// maximum where the table gives one, else the physical maximum.
    pub fn desired_speed_mps(&self) -> f64 {
        self.desired_max_speed_mps.unwrap_or(self.max_speed_mps)
    }
}

/// A SUMO vClass — `mobility/classes/sumo-vtypes` (04-models.md §2.7, R10 §B4).
///
/// The non-road classes (tram, rail, ship) exist in SUMO and are out of scope, so they are
/// not variants here.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum VehicleClass {
    /// `passenger`: an ordinary car.
    #[default]
    Passenger,
    /// `emergency`.
    Emergency,
    /// `delivery`.
    Delivery,
    /// `truck`.
    Truck,
    /// `trailer`: a truck with a trailer.
    Trailer,
    /// `bus`.
    Bus,
    /// `coach`.
    Coach,
    /// `motorcycle`.
    Motorcycle,
    /// `moped`.
    Moped,
    /// `bicycle`.
    Bicycle,
    /// `pedestrian`.
    Pedestrian,
    /// `scooter`: an e-scooter.
    Scooter,
}

impl VehicleClass {
    /// Every class, in the order the §2.7 table lists them.
    pub const ALL: [VehicleClass; 12] = [
        VehicleClass::Passenger,
        VehicleClass::Emergency,
        VehicleClass::Delivery,
        VehicleClass::Truck,
        VehicleClass::Trailer,
        VehicleClass::Bus,
        VehicleClass::Coach,
        VehicleClass::Motorcycle,
        VehicleClass::Moped,
        VehicleClass::Bicycle,
        VehicleClass::Pedestrian,
        VehicleClass::Scooter,
    ];

    /// The SUMO vClass name.
    pub const fn as_str(self) -> &'static str {
        match self {
            VehicleClass::Passenger => "passenger",
            VehicleClass::Emergency => "emergency",
            VehicleClass::Delivery => "delivery",
            VehicleClass::Truck => "truck",
            VehicleClass::Trailer => "trailer",
            VehicleClass::Bus => "bus",
            VehicleClass::Coach => "coach",
            VehicleClass::Motorcycle => "motorcycle",
            VehicleClass::Moped => "moped",
            VehicleClass::Bicycle => "bicycle",
            VehicleClass::Pedestrian => "pedestrian",
            VehicleClass::Scooter => "scooter",
        }
    }

    /// This class's row of the SUMO vType defaults table (R10 §B4), verbatim.
    pub const fn spec(self) -> ClassSpec {
        match self {
            VehicleClass::Passenger => ClassSpec {
                length_m: 5.0,
                width_m: 1.8,
                height_m: 1.5,
                mass_kg: 1500.0,
                min_gap_m: 2.5,
                accel_mps2: 2.6,
                decel_mps2: 4.5,
                emergency_decel_mps2: 9.0,
                max_speed_mps: kmh(200.0),
                desired_max_speed_mps: None,
                speed_dev: 0.1,
            },
            VehicleClass::Emergency => ClassSpec {
                length_m: 6.5,
                width_m: 2.16,
                height_m: 2.86,
                mass_kg: 5000.0,
                min_gap_m: 2.5,
                accel_mps2: 2.6,
                decel_mps2: 4.5,
                emergency_decel_mps2: 9.0,
                max_speed_mps: kmh(200.0),
                desired_max_speed_mps: None,
                speed_dev: 0.0,
            },
            VehicleClass::Delivery => ClassSpec {
                length_m: 6.5,
                width_m: 2.16,
                height_m: 2.86,
                mass_kg: 5000.0,
                min_gap_m: 2.5,
                accel_mps2: 2.6,
                decel_mps2: 4.5,
                emergency_decel_mps2: 9.0,
                max_speed_mps: kmh(200.0),
                desired_max_speed_mps: None,
                speed_dev: 0.05,
            },
            VehicleClass::Truck => ClassSpec {
                length_m: 7.1,
                width_m: 2.4,
                height_m: 2.4,
                mass_kg: 4500.0,
                min_gap_m: 2.5,
                accel_mps2: 1.3,
                decel_mps2: 4.0,
                emergency_decel_mps2: 7.0,
                max_speed_mps: kmh(130.0),
                desired_max_speed_mps: None,
                speed_dev: 0.05,
            },
            VehicleClass::Trailer => ClassSpec {
                length_m: 16.5,
                width_m: 2.55,
                height_m: 4.0,
                mass_kg: 13000.0,
                min_gap_m: 2.5,
                accel_mps2: 1.0,
                decel_mps2: 4.0,
                emergency_decel_mps2: 7.0,
                max_speed_mps: kmh(130.0),
                desired_max_speed_mps: None,
                speed_dev: 0.05,
            },
            VehicleClass::Bus => ClassSpec {
                length_m: 12.0,
                width_m: 2.5,
                height_m: 3.4,
                mass_kg: 12000.0,
                min_gap_m: 2.5,
                accel_mps2: 1.2,
                decel_mps2: 4.0,
                emergency_decel_mps2: 7.0,
                max_speed_mps: kmh(85.0),
                desired_max_speed_mps: None,
                speed_dev: 0.1,
            },
            VehicleClass::Coach => ClassSpec {
                length_m: 14.0,
                width_m: 2.6,
                height_m: 4.0,
                mass_kg: 25000.0,
                min_gap_m: 2.5,
                accel_mps2: 2.0,
                decel_mps2: 4.0,
                emergency_decel_mps2: 7.0,
                max_speed_mps: kmh(100.0),
                desired_max_speed_mps: None,
                speed_dev: 0.05,
            },
            VehicleClass::Motorcycle => ClassSpec {
                length_m: 2.2,
                width_m: 0.9,
                height_m: 1.5,
                mass_kg: 200.0,
                min_gap_m: 2.5,
                accel_mps2: 6.0,
                decel_mps2: 10.0,
                emergency_decel_mps2: 10.0,
                max_speed_mps: kmh(200.0),
                desired_max_speed_mps: None,
                speed_dev: 0.1,
            },
            VehicleClass::Moped => ClassSpec {
                length_m: 2.1,
                width_m: 0.8,
                height_m: 1.7,
                mass_kg: 80.0,
                min_gap_m: 2.5,
                accel_mps2: 1.1,
                decel_mps2: 7.0,
                emergency_decel_mps2: 10.0,
                max_speed_mps: kmh(45.0),
                desired_max_speed_mps: None,
                speed_dev: 0.1,
            },
            VehicleClass::Bicycle => ClassSpec {
                length_m: 1.6,
                width_m: 0.65,
                height_m: 1.7,
                mass_kg: 10.0,
                min_gap_m: 0.5,
                accel_mps2: 1.2,
                decel_mps2: 3.0,
                emergency_decel_mps2: 7.0,
                max_speed_mps: kmh(50.0),
                desired_max_speed_mps: Some(kmh(20.0)),
                speed_dev: 0.1,
            },
            VehicleClass::Pedestrian => ClassSpec {
                length_m: 0.215,
                width_m: 0.478,
                height_m: 1.719,
                mass_kg: 70.0,
                min_gap_m: 0.25,
                accel_mps2: 1.5,
                decel_mps2: 2.0,
                emergency_decel_mps2: 5.0,
                max_speed_mps: kmh(37.58),
                desired_max_speed_mps: Some(kmh(5.0)),
                speed_dev: 0.1,
            },
            VehicleClass::Scooter => ClassSpec {
                length_m: 1.2,
                width_m: 0.5,
                height_m: 1.7,
                mass_kg: 10.0,
                min_gap_m: 0.5,
                accel_mps2: 1.2,
                decel_mps2: 3.0,
                emergency_decel_mps2: 7.0,
                max_speed_mps: kmh(25.0),
                desired_max_speed_mps: Some(kmh(20.0)),
                speed_dev: 0.1,
            },
        }
    }

    /// The body dimensions of this class.
    pub fn dims(self) -> Dims {
        self.spec().dims()
    }

    /// The AASHTO design vehicle whose turning path stands for this class, with its
    /// wheelbase and minimum centreline turning radius (the path of the front axle's
    /// centre at full lock), metres — *A Policy on Geometric Design of Highways and
    /// Streets* (AASHTO Green Book, 7th ed., 2018), Tables 2-1b and 2-2b.
    ///
    /// **Transcribed, not re-verified against the printed tables**: the values below are
    /// the ones the Green Book is widely quoted with (P: 3.4 m wheelbase, 6.4 m (21 ft)
    /// centreline radius; SU-9: 6.1 m, 11.6 m; CITY-BUS: 7.6 m, 11.5 m; WB-12: 3.8 m
    /// tractor, 10.8 m; BUS-14: 8.1 m, 12.4 m) and the card says so. A van (SUMO's
    /// `delivery`, `emergency`) is between P and SU-9 and takes P, the less demanding; a
    /// two-wheeler has no design vehicle and takes this crate's 2.5 m.
    pub const fn design_turn(self) -> (&'static str, f64, f64) {
        match self {
            VehicleClass::Passenger | VehicleClass::Emergency | VehicleClass::Delivery => {
                ("P", 3.4, 6.4)
            }
            VehicleClass::Truck => ("SU-9", 6.1, 11.6),
            VehicleClass::Trailer => ("WB-12", 3.8, 10.8),
            VehicleClass::Bus => ("CITY-BUS", 7.6, 11.5),
            VehicleClass::Coach => ("BUS-14", 8.1, 12.4),
            VehicleClass::Motorcycle
            | VehicleClass::Moped
            | VehicleClass::Scooter
            | VehicleClass::Bicycle
            | VehicleClass::Pedestrian => ("none", 1.0, 2.5),
        }
    }

    /// The tightest radius this class's *reference point* (the rear-axle centre) can
    /// follow, metres: `sqrt(R_ctr² − L²)` from [`VehicleClass::design_turn`]'s
    /// centreline radius `R_ctr` and wheelbase `L` — the kinematic bicycle model, in which
    /// the rear axle turns about the same centre on the smaller circle. 5.42 m for P.
    ///
    /// The body's heading turns at `v / R` on a path of radius `R`, so this is the bound
    /// on a vehicle's yaw rate at a given speed that the traffic auditor holds it to.
    pub fn min_path_radius_m(self) -> f64 {
        let (_, wheelbase, centreline) = self.design_turn();
        v2xw_core::math::sqrt((centreline * centreline - wheelbase * wheelbase).max(0.25))
    }

    /// Which lane-access bit this class occupies in the world's [`ClassMask`].
    ///
    /// The world's mask has eight bits (car, truck, bus, moto, bicycle, pedestrian,
    /// emergency, rail), so the twelve SUMO classes fold onto them: a delivery van and a
    /// trailer are `TRUCK`, a coach is `BUS`, a moped and an e-scooter are `MOTO`.
    /// Folding rather than widening keeps the world format unchanged, and the fold is
    /// the one the OSM importer already assumes when it sets a lane's mask.
    pub const fn class_mask(self) -> ClassMask {
        match self {
            VehicleClass::Passenger => ClassMask::CAR,
            VehicleClass::Emergency => ClassMask::EMERGENCY,
            VehicleClass::Delivery | VehicleClass::Truck | VehicleClass::Trailer => {
                ClassMask::TRUCK
            }
            VehicleClass::Bus | VehicleClass::Coach => ClassMask::BUS,
            VehicleClass::Motorcycle | VehicleClass::Moped | VehicleClass::Scooter => {
                ClassMask::MOTO
            }
            VehicleClass::Bicycle => ClassMask::BICYCLE,
            VehicleClass::Pedestrian => ClassMask::PEDESTRIAN,
        }
    }

    /// True if this class is a vulnerable road user (04-models.md §2.5): it walks or rides
    /// rather than drives, and it is served by `VruMobility` rather than `CarFollowing`.
    pub const fn is_vru(self) -> bool {
        matches!(
            self,
            VehicleClass::Pedestrian
                | VehicleClass::Bicycle
                | VehicleClass::Scooter
                | VehicleClass::Moped
        )
    }
}

impl core::fmt::Display for VehicleClass {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The model card for `mobility/classes/sumo-vtypes` (data, all tiers).
pub fn sumo_vtypes_card() -> ModelCard {
    let src = Source {
        kind: SourceKind::Dataset,
        reference: "SUMO Vehicle Type Parameter Defaults (R10 §B4, cache vtype_defaults.md)"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: Some("transcribed verbatim; km/h converted by division by 3.6".to_string()),
    };
    let mut card = ModelCard::new(
        "mobility/classes/sumo-vtypes",
        Family::Mobility,
        "1.0.0",
        "The SUMO vType defaults: length, width, height, mass, minimum gap, acceleration, \
         deceleration, emergency deceleration, maximum speed and speed deviation for each \
         vehicle class. Data only — it is read by the car-following, lane-change and demand \
         models and supplies the `Dims` every `Kinematics` carries.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.parameters = VehicleClass::ALL
        .iter()
        .map(|c| {
            let s = c.spec();
            Parameter::new(
                c.as_str(),
                "m, kg, m/s, m/s²",
                serde_json::json!({
                    "length_m": s.length_m,
                    "width_m": s.width_m,
                    "height_m": s.height_m,
                    "mass_kg": s.mass_kg,
                    "min_gap_m": s.min_gap_m,
                    "accel_mps2": s.accel_mps2,
                    "decel_mps2": s.decel_mps2,
                    "emergency_decel_mps2": s.emergency_decel_mps2,
                    "max_speed_mps": s.max_speed_mps,
                    "desired_max_speed_mps": s.desired_max_speed_mps,
                    "speed_dev": s.speed_dev,
                }),
                src.clone(),
            )
        })
        .collect();
    card.assumptions = vec![
        "The reference point of a vehicle is the rear-axle centre (03-interfaces.md §1)."
            .to_string(),
        "The twelve SUMO classes fold onto the world's eight lane-access bits; \
         `VehicleClass::class_mask` documents the fold."
            .to_string(),
    ];
    card.ignores =
        vec!["Non-road classes (tram, rail variants, ship), which are out of scope.".to_string()];
    card.sources = vec![src];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec!["classes::tests::sumo_table_matches_the_document".to_string()],
    };
    card
}

/// One of the frozen reference engine's four classes — `mobility/classes/legacy-fleet`
/// (`code (legacy)` [`run.py` L142-146]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyClass {
    /// `car`.
    Car,
    /// `motorcycle`.
    Motorcycle,
    /// `truck`.
    Truck,
    /// `bus`.
    Bus,
}

/// One row of the legacy fleet table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LegacySpec {
    /// Multiplier on the trip's drawn desired speed.
    pub speed_mult: f64,
    /// Body length, metres.
    pub length_m: f64,
    /// IDM `a_max`, m/s².
    pub accel_mps2: f64,
    /// IDM `b`, m/s².
    pub decel_mps2: f64,
    /// Share of the `mixed` fleet.
    pub weight: f64,
}

impl LegacyClass {
    /// Every class, in the order `VEHICLE_TYPES` lists them.
    pub const ALL: [LegacyClass; 4] = [
        LegacyClass::Car,
        LegacyClass::Motorcycle,
        LegacyClass::Truck,
        LegacyClass::Bus,
    ];

    /// The legacy name.
    pub const fn as_str(self) -> &'static str {
        match self {
            LegacyClass::Car => "car",
            LegacyClass::Motorcycle => "motorcycle",
            LegacyClass::Truck => "truck",
            LegacyClass::Bus => "bus",
        }
    }

    /// This class's row of `VEHICLE_TYPES`, verbatim.
    pub const fn spec(self) -> LegacySpec {
        match self {
            LegacyClass::Car => LegacySpec {
                speed_mult: 1.00,
                length_m: 4.5,
                accel_mps2: 1.8,
                decel_mps2: 2.5,
                weight: 0.75,
            },
            LegacyClass::Motorcycle => LegacySpec {
                speed_mult: 1.10,
                length_m: 2.2,
                accel_mps2: 2.5,
                decel_mps2: 3.0,
                weight: 0.08,
            },
            LegacyClass::Truck => LegacySpec {
                speed_mult: 0.80,
                length_m: 12.0,
                accel_mps2: 0.8,
                decel_mps2: 1.5,
                weight: 0.10,
            },
            LegacyClass::Bus => LegacySpec {
                speed_mult: 0.85,
                length_m: 12.0,
                accel_mps2: 0.9,
                decel_mps2: 1.6,
                weight: 0.07,
            },
        }
    }

    /// The nearest SUMO class, for the `Dims` a `Kinematics` carries when a scenario runs
    /// the legacy fleet: the legacy table gives a length but no width or height.
    pub const fn sumo_equivalent(self) -> VehicleClass {
        match self {
            LegacyClass::Car => VehicleClass::Passenger,
            LegacyClass::Motorcycle => VehicleClass::Motorcycle,
            LegacyClass::Truck => VehicleClass::Truck,
            LegacyClass::Bus => VehicleClass::Bus,
        }
    }

    /// The `mixed` fleet composition, as `(class, weight)` in class order.
    ///
    /// The four weights sum to 1.0 in the source table; a sampler normalises anyway.
    pub fn mixed_fleet() -> [(LegacyClass, f64); 4] {
        [
            (LegacyClass::Car, LegacyClass::Car.spec().weight),
            (
                LegacyClass::Motorcycle,
                LegacyClass::Motorcycle.spec().weight,
            ),
            (LegacyClass::Truck, LegacyClass::Truck.spec().weight),
            (LegacyClass::Bus, LegacyClass::Bus.spec().weight),
        ]
    }
}

/// The model card for `mobility/classes/legacy-fleet` (data, all tiers).
pub fn legacy_fleet_card() -> ModelCard {
    let src = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L142-146 (`VEHICLE_TYPES`)"
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: Some("the frozen reference engine's heterogeneous fleet".to_string()),
    };
    let mut card = ModelCard::new(
        "mobility/classes/legacy-fleet",
        Family::Mobility,
        "1.0.0",
        "The frozen reference engine's four vehicle classes: a desired-speed multiplier, a \
         length and the IDM acceleration and deceleration each class drives with, plus the \
         `mixed` fleet weights. Kept as a preset so a parity run against the legacy corpus \
         uses the same fleet.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium];
    card.parameters = LegacyClass::ALL
        .iter()
        .map(|c| {
            let s = c.spec();
            Parameter::new(
                c.as_str(),
                "1, m, m/s²",
                serde_json::json!({
                    "speed_mult": s.speed_mult,
                    "length_m": s.length_m,
                    "accel_mps2": s.accel_mps2,
                    "decel_mps2": s.decel_mps2,
                    "weight": s.weight,
                }),
                src.clone(),
            )
        })
        .collect();
    card.limitations = vec![
        "The table gives no width or height; `LegacyClass::sumo_equivalent` supplies them \
         from the SUMO table, which is a design choice recorded here."
            .to_string(),
    ];
    card.sources = vec![src];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec!["classes::tests::legacy_table_matches_the_document".to_string()],
    };
    card
}

/// A 3GPP evaluation vehicle type — `mobility/classes/tr37885-types`
/// (TR 37.885 §6.1.3, R2c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tr37885Type {
    /// Type 1: a car with a roof-edge antenna 0.75 m up.
    Type1Car,
    /// Type 2: the same car with the antenna at 1.6 m.
    Type2Car,
    /// Type 3: a truck or bus, antenna at 3 m.
    Type3TruckBus,
}

impl Tr37885Type {
    /// Every type, in table order.
    pub const ALL: [Tr37885Type; 3] = [
        Tr37885Type::Type1Car,
        Tr37885Type::Type2Car,
        Tr37885Type::Type3TruckBus,
    ];

    /// The type's label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Tr37885Type::Type1Car => "type-1-car",
            Tr37885Type::Type2Car => "type-2-car",
            Tr37885Type::Type3TruckBus => "type-3-truck-bus",
        }
    }

    /// Body dimensions, metres.
    pub const fn dims(self) -> Dims {
        match self {
            Tr37885Type::Type1Car | Tr37885Type::Type2Car => Dims::new(5.0, 2.0, 1.6),
            Tr37885Type::Type3TruckBus => Dims::new(13.0, 2.6, 3.0),
        }
    }

    /// Antenna height above the ground, metres.
    pub const fn antenna_height_m(self) -> f64 {
        match self {
            Tr37885Type::Type1Car => 0.75,
            Tr37885Type::Type2Car => 1.6,
            Tr37885Type::Type3TruckBus => 3.0,
        }
    }
}

/// TR 36.885's single vehicle antenna height, metres (R2c).
pub const TR36885_ANTENNA_HEIGHT_M: f64 = 1.5;

/// The model card for `mobility/classes/tr37885-types` (data, all tiers).
pub fn tr37885_types_card() -> ModelCard {
    let src = Source {
        kind: SourceKind::Standard,
        reference: "3GPP TR 37.885 §6.1.3 (R2c)".to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let mut card = ModelCard::new(
        "mobility/classes/tr37885-types",
        Family::Mobility,
        "1.0.0",
        "The three 3GPP evaluation vehicle types and their antenna heights, for radio \
         validation runs that must reproduce a TR 37.885 or TR 36.885 drop.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.parameters = Tr37885Type::ALL
        .iter()
        .map(|t| {
            let d = t.dims();
            Parameter::new(
                t.as_str(),
                "m",
                serde_json::json!({
                    "length_m": d.length_m,
                    "width_m": d.width_m,
                    "height_m": d.height_m,
                    "antenna_height_m": t.antenna_height_m(),
                }),
                src.clone(),
            )
        })
        .chain(core::iter::once(Parameter::new(
            "tr36885_antenna_height_m",
            "m",
            serde_json::json!(TR36885_ANTENNA_HEIGHT_M),
            Source::new(SourceKind::Standard, "3GPP TR 36.885 (R2c)"),
        )))
        .collect();
    card.sources = vec![src];
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sumo_table_matches_the_document() {
        // Spot-check every row against 04-models.md §2.7 (R10 §B4).
        let p = VehicleClass::Passenger.spec();
        assert_eq!(p.length_m, 5.0);
        assert_eq!(p.accel_mps2, 2.6);
        assert_eq!(p.decel_mps2, 4.5);
        assert_eq!(p.emergency_decel_mps2, 9.0);
        assert!((p.max_speed_mps - 200.0 / 3.6).abs() < 1e-12);
        let t = VehicleClass::Truck.spec();
        assert_eq!((t.length_m, t.width_m, t.height_m), (7.1, 2.4, 2.4));
        assert_eq!(t.mass_kg, 4500.0);
        let b = VehicleClass::Bicycle.spec();
        assert_eq!(b.min_gap_m, 0.5);
        assert!((b.desired_speed_mps() - 20.0 / 3.6).abs() < 1e-12);
        let ped = VehicleClass::Pedestrian.spec();
        assert!((ped.desired_speed_mps() - 5.0 / 3.6).abs() < 1e-12);
        assert!((ped.max_speed_mps - 37.58 / 3.6).abs() < 1e-12);
        assert_eq!(VehicleClass::ALL.len(), 12);
    }

    #[test]
    fn legacy_table_matches_the_document() {
        let c = LegacyClass::Car.spec();
        assert_eq!(
            (
                c.speed_mult,
                c.length_m,
                c.accel_mps2,
                c.decel_mps2,
                c.weight
            ),
            (1.00, 4.5, 1.8, 2.5, 0.75)
        );
        let m = LegacyClass::Motorcycle.spec();
        assert_eq!(
            (
                m.speed_mult,
                m.length_m,
                m.accel_mps2,
                m.decel_mps2,
                m.weight
            ),
            (1.10, 2.2, 2.5, 3.0, 0.08)
        );
        let t = LegacyClass::Truck.spec();
        assert_eq!(
            (
                t.speed_mult,
                t.length_m,
                t.accel_mps2,
                t.decel_mps2,
                t.weight
            ),
            (0.80, 12.0, 0.8, 1.5, 0.10)
        );
        let b = LegacyClass::Bus.spec();
        assert_eq!(
            (
                b.speed_mult,
                b.length_m,
                b.accel_mps2,
                b.decel_mps2,
                b.weight
            ),
            (0.85, 12.0, 0.9, 1.6, 0.07)
        );
        let total: f64 = LegacyClass::mixed_fleet().iter().map(|(_, w)| *w).sum();
        assert!((total - 1.0).abs() < 1e-12, "fleet weights sum to one");
    }

    #[test]
    fn class_masks_fold_onto_the_world_mask() {
        assert_eq!(VehicleClass::Passenger.class_mask(), ClassMask::CAR);
        assert_eq!(VehicleClass::Trailer.class_mask(), ClassMask::TRUCK);
        assert_eq!(VehicleClass::Coach.class_mask(), ClassMask::BUS);
        assert_eq!(VehicleClass::Scooter.class_mask(), ClassMask::MOTO);
        assert_eq!(VehicleClass::Pedestrian.class_mask(), ClassMask::PEDESTRIAN);
        assert!(VehicleClass::Pedestrian.is_vru());
        assert!(!VehicleClass::Passenger.is_vru());
    }

    #[test]
    fn tr37885_types_match_the_document() {
        assert_eq!(Tr37885Type::Type1Car.dims(), Dims::new(5.0, 2.0, 1.6));
        assert_eq!(Tr37885Type::Type1Car.antenna_height_m(), 0.75);
        assert_eq!(Tr37885Type::Type2Car.antenna_height_m(), 1.6);
        assert_eq!(Tr37885Type::Type3TruckBus.dims(), Dims::new(13.0, 2.6, 3.0));
        assert_eq!(Tr37885Type::Type3TruckBus.antenna_height_m(), 3.0);
        assert_eq!(TR36885_ANTENNA_HEIGHT_M, 1.5);
    }

    #[test]
    fn every_card_validates() {
        for card in [
            sumo_vtypes_card(),
            legacy_fleet_card(),
            tr37885_types_card(),
        ] {
            card.validate().expect("card validates");
        }
    }
}
