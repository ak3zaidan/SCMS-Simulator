//! `mobility/demand/tr36885-drop` — the 3GPP evaluation drops (04-models.md §2.4).
//!
//! A *drop* is not an arrival process: it is one instantaneous placement of vehicles, which
//! is how the 3GPP evaluation methodology defines a scenario. Every vehicle exists from the
//! start of the run, so a radio validation figure is measured on a known, reproducible
//! spatial configuration rather than on whatever a demand process happened to produce.
//!
//! # The two variants
//!
//! | Variant | Spacing | Source |
//! |---|---|---|
//! | TR 36.885 | spatial Poisson, mean inter-vehicle distance = **2.5 s × speed** | TR 36.885 Table A.1.2-1 [R2c] |
//! | TR 37.885 | bumper-to-bumper gap = **max{2 m, Exp(mean = 2 s × speed)}** | TR 37.885 §6.1.2 [R2c] |
//!
//! # The cited speeds
//!
//! TR 36.885: urban 15 or 60 km/h, freeway 70 or 140 km/h, pedestrians 3 km/h.
//! TR 37.885: highway Option A 140 km/h (70 optional), Option B per lane
//! 80/100/140/40/30/20 km/h, Option C clustered Type-3 platoons of 6 with 2 m gaps; urban
//! Option A 60 km/h, Option B east-west lanes 60/50/25/15 km/h.
//!
//! The validation densities of [R2d] — 50, 100 and 200 veh/km (Todisco) and 60 and
//! 120 veh/km (Molina-Masegosa) — are carried as [`VALIDATION_DENSITIES_VEH_KM`], because a
//! C-V2X validation run has to be able to ask for them by name.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::SimTime;
use v2xw_world::LaneKind;

use crate::classes::VehicleClass;
use crate::ctx::MobCtx;
use crate::traits::Demand;
use crate::views::TripRequest;

/// The model id.
pub const MODEL_ID: &str = "mobility/demand/tr36885-drop";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The densities C-V2X validation runs use, veh/km [R2d].
pub const VALIDATION_DENSITIES_VEH_KM: [(f64, &str); 5] = [
    (50.0, "todisco"),
    (100.0, "todisco"),
    (200.0, "todisco"),
    (60.0, "molina-masegosa"),
    (120.0, "molina-masegosa"),
];

/// Pedestrian speed in the TR 36.885 drops, m/s (3 km/h).
pub const TR36885_PEDESTRIAN_SPEED_MPS: f64 = 3.0 / 3.6;

/// Which spacing law the drop uses.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "variant", rename_all = "kebab-case")]
pub enum DropVariant {
    /// TR 36.885: spatial Poisson with mean inter-vehicle distance `2.5 s × speed`.
    Tr36885 {
        /// The mean gap, expressed as a time, seconds.
        mean_gap_time_s: f64,
    },
    /// TR 37.885: bumper-to-bumper gap `max{min_gap_m, Exp(mean = mean_gap_time_s × speed)}`.
    Tr37885 {
        /// The floor on the gap, metres.
        min_gap_m: f64,
        /// The mean of the exponential, expressed as a time, seconds.
        mean_gap_time_s: f64,
    },
    /// TR 37.885 Option C: platoons of `size` Type-3 vehicles at `gap_m`, the platoons
    /// themselves spaced by the TR 37.885 law.
    Tr37885OptionC {
        /// Vehicles per platoon.
        size: u32,
        /// Bumper-to-bumper gap inside a platoon, metres.
        gap_m: f64,
        /// The gap between platoons, expressed as a time, seconds.
        mean_gap_time_s: f64,
    },
}

impl Default for DropVariant {
    /// TR 36.885, the variant §2.4 names the model after.
    fn default() -> Self {
        DropVariant::Tr36885 {
            mean_gap_time_s: 2.5,
        }
    }
}

impl DropVariant {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            DropVariant::Tr36885 { .. } => "tr36885",
            DropVariant::Tr37885 { .. } => "tr37885",
            DropVariant::Tr37885OptionC { .. } => "tr37885-option-c",
        }
    }
}

/// Which cited speed table applies, and how it maps onto lane indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpeedProfile {
    /// TR 36.885 urban, 15 km/h.
    Tr36885UrbanSlow,
    /// TR 36.885 urban, 60 km/h.
    Tr36885Urban,
    /// TR 36.885 freeway, 70 km/h.
    Tr36885FreewaySlow,
    /// TR 36.885 freeway, 140 km/h.
    Tr36885Freeway,
    /// TR 37.885 highway Option A, 140 km/h.
    Tr37885HighwayA,
    /// TR 37.885 highway Option B: per lane 80/100/140/40/30/20 km/h.
    Tr37885HighwayB,
    /// TR 37.885 urban Option A, 60 km/h.
    Tr37885UrbanA,
    /// TR 37.885 urban Option B: 60/50/25/15 km/h by lane.
    Tr37885UrbanB,
}

impl SpeedProfile {
    /// The speed for a lane of index `index`, m/s.
    ///
    /// A per-lane table repeats when the world has more lanes than the table has entries,
    /// which is recorded on the card: the TRs specify a fixed lane count and a wider road
    /// has to reuse the table rather than invent an entry.
    pub fn speed_mps(self, index: u8) -> f64 {
        let kmh = match self {
            SpeedProfile::Tr36885UrbanSlow => 15.0,
            SpeedProfile::Tr36885Urban => 60.0,
            SpeedProfile::Tr36885FreewaySlow => 70.0,
            SpeedProfile::Tr36885Freeway => 140.0,
            SpeedProfile::Tr37885HighwayA => 140.0,
            SpeedProfile::Tr37885HighwayB => {
                const TABLE: [f64; 6] = [80.0, 100.0, 140.0, 40.0, 30.0, 20.0];
                TABLE[usize::from(index) % TABLE.len()]
            }
            SpeedProfile::Tr37885UrbanA => 60.0,
            SpeedProfile::Tr37885UrbanB => {
                const TABLE: [f64; 4] = [60.0, 50.0, 25.0, 15.0];
                TABLE[usize::from(index) % TABLE.len()]
            }
        };
        kmh / 3.6
    }

    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            SpeedProfile::Tr36885UrbanSlow => "tr36885-urban-15",
            SpeedProfile::Tr36885Urban => "tr36885-urban-60",
            SpeedProfile::Tr36885FreewaySlow => "tr36885-freeway-70",
            SpeedProfile::Tr36885Freeway => "tr36885-freeway-140",
            SpeedProfile::Tr37885HighwayA => "tr37885-highway-a",
            SpeedProfile::Tr37885HighwayB => "tr37885-highway-b",
            SpeedProfile::Tr37885UrbanA => "tr37885-urban-a",
            SpeedProfile::Tr37885UrbanB => "tr37885-urban-b",
        }
    }
}

/// The drop's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DropParams {
    /// Which spacing law.
    pub variant: DropVariant,
    /// Which speed table.
    pub speeds: SpeedProfile,
    /// Which class the drop places (Type 1/2 are cars, Type 3 a truck or bus).
    pub class: VehicleClass,
    /// When the drop happens.
    pub at: SimTime,
}

impl Default for DropParams {
    fn default() -> Self {
        Self {
            variant: DropVariant::default(),
            speeds: SpeedProfile::Tr36885Freeway,
            class: VehicleClass::Passenger,
            at: 0,
        }
    }
}

/// The 3GPP evaluation drop model.
#[derive(Debug, Clone)]
pub struct DropModel {
    params: DropParams,
    /// The trips, produced once and handed out when the window containing `at` arrives.
    pending: Option<Vec<TripRequest>>,
    card: ModelCard,
}

impl DropModel {
    /// The model with the given parameters.
    pub fn new(params: DropParams) -> Self {
        Self {
            card: card(&params),
            params,
            pending: None,
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &DropParams {
        &self.params
    }

    /// Places vehicles on every drivable lane of the context's world and returns the
    /// trips.
    ///
    /// Lanes are walked in id order and each lane's own RNG stream supplies its gaps, so
    /// the drop is the same whatever order the lanes are visited in and adding a lane to
    /// the world cannot change another lane's placement.
    pub fn drop_on(&self, ctx: &mut dyn MobCtx) -> Vec<TripRequest> {
        // The lanes first, so the world borrow is finished before the RNG borrows begin.
        let lanes: Vec<(v2xw_core::ids::LaneId, u8, f64)> = ctx
            .world()
            .roads
            .lanes()
            .iter()
            .filter(|l| l.kind == LaneKind::Driving)
            .map(|l| (l.id, l.index, l.length_m))
            .collect();
        let mut out: Vec<TripRequest> = Vec::new();
        let mut seq = 0u64;
        for (lane, index, length_m) in lanes {
            let speed = self.params.speeds.speed_mps(index);
            let mut s = 0.0f64;
            let mut rng = ctx.rng(RngDomain::Spawn, EntityRef::Lane(lane));
            loop {
                let (gap, count) = match self.params.variant {
                    DropVariant::Tr36885 { mean_gap_time_s } => {
                        let mean = mean_gap_time_s * speed;
                        (rng.exponential(1.0 / mean.max(f64::EPSILON)), 1u32)
                    }
                    DropVariant::Tr37885 {
                        min_gap_m,
                        mean_gap_time_s,
                    } => {
                        let mean = mean_gap_time_s * speed;
                        let drawn = rng.exponential(1.0 / mean.max(f64::EPSILON));
                        (drawn.max(min_gap_m), 1)
                    }
                    DropVariant::Tr37885OptionC {
                        size,
                        gap_m,
                        mean_gap_time_s,
                    } => {
                        let mean = mean_gap_time_s * speed;
                        let drawn = rng.exponential(1.0 / mean.max(f64::EPSILON));
                        (drawn.max(gap_m), size.max(1))
                    }
                };
                s += gap;
                if s >= length_m {
                    break;
                }
                let class = match self.params.variant {
                    DropVariant::Tr37885OptionC { .. } => VehicleClass::Truck,
                    _ => self.params.class,
                };
                let body = class.spec().length_m;
                let inner_gap = match self.params.variant {
                    DropVariant::Tr37885OptionC { gap_m, .. } => gap_m,
                    _ => 0.0,
                };
                for k in 0..count {
                    let position = s + f64::from(k) * (body + inner_gap);
                    if position >= length_m {
                        break;
                    }
                    out.push(TripRequest {
                        seq,
                        t: self.params.at,
                        origin: lane,
                        origin_s_m: position,
                        destination: lane,
                        class,
                        desired_speed_mps: speed,
                    });
                    seq += 1;
                }
                if count > 1 {
                    s += f64::from(count - 1) * (body + inner_gap);
                }
            }
        }
        // `seq` must be the demand stream order (I-M2); lanes were walked in id order, so
        // it already is.
        out
    }
}

impl v2xw_core::model::Model for DropModel {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Demand for DropModel {
    fn spawns_in(&mut self, ctx: &mut dyn MobCtx, from: SimTime, to: SimTime) -> Vec<TripRequest> {
        if self.params.at < from || self.params.at >= to {
            return Vec::new();
        }
        if self.pending.is_none() {
            self.pending = Some(self.drop_on(ctx));
        }
        self.pending.clone().unwrap_or_default()
    }
}

/// The model card.
pub fn card(params: &DropParams) -> ModelCard {
    let tr36885 = Source {
        kind: SourceKind::Standard,
        reference: "3GPP TR 36.885 Table A.1.2-1 [R2c]".to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let tr37885 = Source {
        kind: SourceKind::Standard,
        reference: "3GPP TR 37.885 §6.1.2 [R2c]".to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "The 3GPP evaluation drops: one instantaneous placement of vehicles along every \
         drivable lane, spaced by the cited TR 36.885 or TR 37.885 law, for radio \
         validation runs that must reproduce a published configuration.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "TR 36.885 spacing".to_string(),
            latex_or_text: "gap ~ Exp(mean = 2.5 s × speed)".to_string(),
            notes: Some("spatial Poisson along the lane".to_string()),
        },
        Equation {
            name: "TR 37.885 spacing".to_string(),
            latex_or_text: "gap = max{2 m, Exp(mean = 2 s × speed)}".to_string(),
            notes: Some("bumper to bumper".to_string()),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "variant",
            "-",
            serde_json::json!(params.variant.label()),
            tr36885.clone(),
        ),
        Parameter::new(
            "mean_gap_time",
            "s",
            serde_json::json!(match params.variant {
                DropVariant::Tr36885 { mean_gap_time_s } => mean_gap_time_s,
                DropVariant::Tr37885 {
                    mean_gap_time_s, ..
                } => mean_gap_time_s,
                DropVariant::Tr37885OptionC {
                    mean_gap_time_s, ..
                } => mean_gap_time_s,
            }),
            tr36885.clone(),
        ),
        Parameter::new(
            "min_gap",
            "m",
            serde_json::json!(match params.variant {
                DropVariant::Tr37885 { min_gap_m, .. } => min_gap_m,
                DropVariant::Tr37885OptionC { gap_m, .. } => gap_m,
                DropVariant::Tr36885 { .. } => 0.0,
            }),
            tr37885.clone(),
        ),
        Parameter::new(
            "speeds",
            "km/h",
            serde_json::json!({
                "selected": params.speeds.label(),
                "tr36885_urban": [15.0, 60.0],
                "tr36885_freeway": [70.0, 140.0],
                "tr36885_pedestrian": 3.0,
                "tr37885_highway_option_b": [80.0, 100.0, 140.0, 40.0, 30.0, 20.0],
                "tr37885_urban_option_b": [60.0, 50.0, 25.0, 15.0],
            }),
            tr37885.clone(),
        ),
        Parameter::new(
            "option_c_platoon",
            "-",
            serde_json::json!({"size": 6, "gap_m": 2.0, "type": "type-3"}),
            tr37885,
        ),
        Parameter::new(
            "validation_densities",
            "veh/km",
            serde_json::json!(
                VALIDATION_DENSITIES_VEH_KM
                    .iter()
                    .map(|(d, who)| serde_json::json!({"density_veh_km": d, "source": who}))
                    .collect::<Vec<_>>()
            ),
            Source::new(
                SourceKind::Paper,
                "Todisco et al. and Molina-Masegosa & Gozalvez, the C-V2X validation \
                 densities of R2d",
            ),
        ),
    ];
    card.assumptions = vec![
        "A drop is instantaneous: every vehicle exists from `at`, and the model produces \
         nothing afterwards."
            .to_string(),
        "Each lane's gaps come from that lane's own RNG stream, so adding a lane to the \
         world cannot change another lane's placement."
            .to_string(),
        "A per-lane speed table repeats when the world has more lanes than the TR \
         specifies; the TRs fix the lane count and a wider road must reuse the table \
         rather than invent an entry."
            .to_string(),
    ];
    card.limitations = vec![
        "The drop places vehicles on the lanes the world has; it does not build the TR's \
         own highway or urban geometry, which is 04-models.md §1.2's business."
            .to_string(),
    ];
    card.sources = vec![tr36885];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::Spawn.as_str().to_string()],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "demand::tr36885::tests::the_mean_gap_matches_two_point_five_seconds".to_string(),
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
    use v2xw_world::World;

    fn world() -> World {
        ring(&RingParams {
            circumference_m: 4000.0,
            segments: 8,
            ..RingParams::default()
        })
        .expect("a ring")
    }

    #[test]
    fn the_mean_gap_matches_two_point_five_seconds() {
        let w = world();
        let rng = RngRegistry::new(31);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let speed = SpeedProfile::Tr36885Freeway.speed_mps(0);
        let m = DropModel::new(DropParams {
            speeds: SpeedProfile::Tr36885Freeway,
            ..DropParams::default()
        });
        let trips = m.drop_on(&mut ctx);
        assert!(!trips.is_empty());
        // The mean gap must be 2.5 s × speed = 2.5 × 38.9 ≈ 97 m.
        let want = 2.5 * speed;
        // Gaps within each lane.
        let mut gaps: Vec<f64> = Vec::new();
        for lane in w.roads.lanes() {
            let mut on_lane: Vec<f64> = trips
                .iter()
                .filter(|t| t.origin == lane.id)
                .map(|t| t.origin_s_m)
                .collect();
            on_lane.sort_by(f64::total_cmp);
            for pair in on_lane.windows(2) {
                gaps.push(pair[1] - pair[0]);
            }
        }
        assert!(gaps.len() > 20, "{} gaps", gaps.len());
        let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
        assert!(
            (mean - want).abs() / want < 0.35,
            "mean gap {mean} m against the expected {want} m"
        );
    }

    #[test]
    fn the_tr37885_law_floors_the_gap_at_two_metres() {
        let w = world();
        let rng = RngRegistry::new(17);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let m = DropModel::new(DropParams {
            variant: DropVariant::Tr37885 {
                min_gap_m: 2.0,
                mean_gap_time_s: 2.0,
            },
            speeds: SpeedProfile::Tr37885HighwayA,
            ..DropParams::default()
        });
        let trips = m.drop_on(&mut ctx);
        for lane in w.roads.lanes() {
            let mut on_lane: Vec<f64> = trips
                .iter()
                .filter(|t| t.origin == lane.id)
                .map(|t| t.origin_s_m)
                .collect();
            on_lane.sort_by(f64::total_cmp);
            for pair in on_lane.windows(2) {
                assert!(pair[1] - pair[0] >= 2.0 - 1e-9, "gap {}", pair[1] - pair[0]);
            }
        }
    }

    #[test]
    fn option_c_places_platoons_of_trucks() {
        let w = world();
        let rng = RngRegistry::new(5);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let m = DropModel::new(DropParams {
            variant: DropVariant::Tr37885OptionC {
                size: 6,
                gap_m: 2.0,
                mean_gap_time_s: 2.0,
            },
            speeds: SpeedProfile::Tr37885HighwayA,
            ..DropParams::default()
        });
        let trips = m.drop_on(&mut ctx);
        assert!(!trips.is_empty());
        assert!(trips.iter().all(|t| t.class == VehicleClass::Truck));
        // Inside a platoon the spacing is the truck's length plus 2 m.
        let step = VehicleClass::Truck.spec().length_m + 2.0;
        let mut on_lane: Vec<f64> = trips
            .iter()
            .filter(|t| t.origin == w.roads.lanes()[0].id)
            .map(|t| t.origin_s_m)
            .collect();
        on_lane.sort_by(f64::total_cmp);
        let inner = on_lane
            .windows(2)
            .filter(|p| (p[1] - p[0] - step).abs() < 1e-6)
            .count();
        assert!(inner > 0, "no platoon spacing found in {on_lane:?}");
    }

    #[test]
    fn the_cited_speeds_are_the_document_values() {
        assert!((SpeedProfile::Tr36885Urban.speed_mps(0) - 60.0 / 3.6).abs() < 1e-12);
        assert!((SpeedProfile::Tr36885UrbanSlow.speed_mps(0) - 15.0 / 3.6).abs() < 1e-12);
        assert!((SpeedProfile::Tr36885Freeway.speed_mps(0) - 140.0 / 3.6).abs() < 1e-12);
        assert!((SpeedProfile::Tr36885FreewaySlow.speed_mps(0) - 70.0 / 3.6).abs() < 1e-12);
        assert!((SpeedProfile::Tr37885HighwayB.speed_mps(0) - 80.0 / 3.6).abs() < 1e-12);
        assert!((SpeedProfile::Tr37885HighwayB.speed_mps(2) - 140.0 / 3.6).abs() < 1e-12);
        assert!((SpeedProfile::Tr37885UrbanB.speed_mps(3) - 15.0 / 3.6).abs() < 1e-12);
        assert!((TR36885_PEDESTRIAN_SPEED_MPS - 3.0 / 3.6).abs() < 1e-12);
        assert_eq!(VALIDATION_DENSITIES_VEH_KM[0].0, 50.0);
        assert_eq!(VALIDATION_DENSITIES_VEH_KM[4].0, 120.0);
    }

    #[test]
    fn a_drop_happens_once() {
        let w = world();
        let rng = RngRegistry::new(2);
        let mut m = DropModel::new(DropParams::default());
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let first = m.spawns_in(&mut ctx, 0, 100 * v2xw_core::time::NS_PER_MS);
        assert!(!first.is_empty());
        let mut ctx = MobilityCtx::new(100 * v2xw_core::time::NS_PER_MS, &w, &rng);
        let second = m.spawns_in(
            &mut ctx,
            100 * v2xw_core::time::NS_PER_MS,
            200 * v2xw_core::time::NS_PER_MS,
        );
        assert!(second.is_empty(), "the drop is instantaneous");
    }

    #[test]
    fn a_drop_is_a_pure_function_of_the_world_and_the_seed() {
        let w = world();
        let a = {
            let rng = RngRegistry::new(64);
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            DropModel::new(DropParams::default()).drop_on(&mut ctx)
        };
        let b = {
            let rng = RngRegistry::new(64);
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            DropModel::new(DropParams::default()).drop_on(&mut ctx)
        };
        assert_eq!(a, b);
    }

    #[test]
    fn the_card_validates() {
        DropModel::new(DropParams::default())
            .card()
            .validate()
            .expect("validates");
    }
}
