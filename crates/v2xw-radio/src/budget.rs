//! How the pieces compose: one link's geometry, loss breakdown and received power
//! (04-models.md §3's tier table).
//!
//! The three families answer three different questions and the engine has to put them
//! together the same way every time:
//!
//! 1. [`classify`] asks every [`ObstacleModel`] what obstructs the link and merges the
//!    answers into one [`LosResult`];
//! 2. [`evaluate`] asks the [`Propagation`] model for the large-scale breakdown, adds
//!    each obstacle model's own loss term into `obstacle_db`, and adds the [`Fading`]
//!    sample;
//! 3. the result carries the received power and every term that produced it, which is
//!    what the inspector shows and what a `phy.rx` record writes.
//!
//! Composing here rather than inside a propagation model is what keeps the tier table
//! honest: `medium` passes [`crate::fading::NoFading`] and the two obstacle models,
//! `high` passes Nakagami fading and adds the diffraction models, and the difference
//! between the tiers is visible at the call site instead of hidden behind a flag.
//!
//! # Who owns obstacle loss
//!
//! **The obstacle stack does.** 04-models.md §3.5 registers `obstacle/building/sommer-2011`
//! as both an [`ObstacleModel`] and a `Propagation` term, and §3's tier table composes it
//! as the obstacle model at `medium` and `high`. A propagation model that also applied it
//! inside `loss_db` would have its term *added* to the stack's by [`evaluate`], so a link
//! crossing two walls with 40 m of in-building path would be attenuated by 68 dB instead
//! of 34 dB. [`crate::prop::LogDistanceShadowing`] therefore ships with its building term
//! off, [`crate::prop::LogDistanceShadowing::with_building_term`] is the explicit opt-in
//! for a standalone model with no obstacle stack, and [`evaluate`] carries a
//! `debug_assert!` that fires when both paths contribute at once. One path owns the term;
//! the other is empty.

use v2xw_core::ctx::Ctx;
use v2xw_core::ids::LinkKey;
use v2xw_core::math;
use v2xw_core::time::SimTime;
use v2xw_core::weather::WeatherState;
use v2xw_world::model::World;

use crate::traits::{Fading, ObstacleModel, Propagation};
use crate::types::{ActorSet, LosClass, LosResult, LossBreakdown, RadioEndpoint};

/// One link's evaluated budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkBudget {
    /// Every term of the large-scale loss, with the obstacle models' contributions
    /// folded into `obstacle_db`.
    pub breakdown: LossBreakdown,
    /// The fading gain, dB (negative in a fade).
    pub fading_db: f64,
    /// The received power, dBm: `P_tx − total_db + fading_db`.
    pub rx_power_dbm: f64,
    /// The straight-line distance, metres.
    pub distance_m: f64,
}

impl LinkBudget {
    /// What a recorder writes: every float on the dB grid (build decision D9).
    #[must_use]
    pub fn quantized(&self) -> Self {
        Self {
            breakdown: self.breakdown.quantized(),
            fading_db: crate::numeric::q_db(self.fading_db),
            rx_power_dbm: crate::numeric::q_db(self.rx_power_dbm),
            distance_m: math::q3(self.distance_m),
        }
    }
}

/// Merges the line-of-sight answers of several obstacle models into one.
///
/// Wall counts and in-building lengths add, edge lists concatenate in the order the
/// models were given, and the class is the union: a link blocked by a building *and* a
/// vehicle is [`LosClass::NlosBv`], which is a state the Abbas presets and the TR 37.885
/// state machine both need to be told about.
#[must_use]
pub fn merge_los(parts: &[LosResult]) -> LosResult {
    let mut out = LosResult::clear();
    let mut building = false;
    let mut vehicle = false;
    let mut terrain = false;
    for part in parts {
        building |= part.class.has_building();
        vehicle |= part.class.has_vehicle();
        terrain |= part.class == LosClass::NlosT;
        out.walls_crossed = out.walls_crossed.saturating_add(part.walls_crossed);
        out.obstructed_len_m += part.obstructed_len_m;
        out.knife_edges.extend(part.knife_edges.iter().copied());
        // One corner per link: the first model that traced one is the one that owns the
        // building geometry.
        if out.corner.is_none() {
            out.corner = part.corner;
        }
    }
    out.class = match (building, vehicle, terrain) {
        (true, true, _) => LosClass::NlosBv,
        (true, false, _) => LosClass::NlosB,
        (false, true, _) => LosClass::NlosV,
        (false, false, true) => LosClass::NlosT,
        (false, false, false) => LosClass::Los,
    };
    out
}

/// Classifies a link with every obstacle model in the stack.
#[must_use]
pub fn classify<C: Ctx + ?Sized>(
    world: &World,
    tx: &RadioEndpoint,
    rx: &RadioEndpoint,
    actors: Option<&ActorSet>,
    obstacles: &[&dyn ObstacleModel<C>],
) -> LosResult {
    let parts: Vec<LosResult> = obstacles
        .iter()
        .map(|m| m.los(world, tx.pos, rx.pos, actors))
        .collect();
    merge_los(&parts)
}

/// Evaluates one link: the large-scale breakdown, the obstacle terms and the fading
/// sample.
///
/// `los` is what [`classify`] returned, passed in rather than recomputed so that a
/// caller evaluating one transmission at many receivers does the geometry once.
///
/// Eleven arguments, and every one of them is a different question the link budget needs
/// an answer to: three model stacks, two endpoints, the carrier, the geometry, the
/// weather, the transmit power and the instant. Bundling them into a struct would move
/// the same eleven values one call earlier without making any of them optional, so the
/// lint is allowed rather than worked around.
#[allow(clippy::too_many_arguments)]
pub fn evaluate<C: Ctx + ?Sized>(
    ctx: &mut C,
    propagation: &mut dyn Propagation<C>,
    obstacles: &mut [&mut dyn ObstacleModel<C>],
    fading: &mut dyn Fading<C>,
    tx: &RadioEndpoint,
    rx: &RadioEndpoint,
    f_hz: f64,
    los: &LosResult,
    weather: &WeatherState,
    tx_power_dbm: f64,
    t: SimTime,
) -> LinkBudget {
    let distance_m = tx.pos.distance(rx.pos);
    let mut breakdown = propagation.loss_db(ctx, tx, rx, f_hz, los, weather);
    // Each obstacle model's own term. Summed in the order the stack was given, which the
    // caller fixes once per scenario.
    let mut extra = Vec::with_capacity(obstacles.len());
    for model in obstacles.iter_mut() {
        extra.push(model.obstacle_loss_db(ctx, tx, rx, los, f_hz));
    }
    let from_stack = math::sum_ordered(extra);
    // One path owns obstacle loss (see the module docs). A propagation model that carries
    // its own obstacle term must not be composed with an obstacle stack that also
    // produces one, because the two are added here and the link is attenuated twice.
    debug_assert!(
        breakdown.obstacle_db == 0.0 || from_stack == 0.0,
        "obstacle loss counted twice: the propagation model returned {} dB and the \
         obstacle stack {} dB. Drop the stack's building model or build the propagation \
         model without `with_building_term()` (04-models.md §3.5).",
        breakdown.obstacle_db,
        from_stack,
    );
    let obstacle_db = breakdown.obstacle_db + from_stack;
    // Rain, once, whatever the law (`weather/attenuation/itu-r-p838`): no law in this
    // crate charges it itself, and the abstract tier is free space by definition.
    let weather_db = if propagation.tier() == v2xw_core::card::Tier::Abstract {
        breakdown.weather_db
    } else {
        breakdown.weather_db + crate::prop::rain_attenuation_db(weather, distance_m, f_hz)
    };
    breakdown = LossBreakdown::new(
        breakdown.path_db,
        breakdown.shadow_db,
        obstacle_db,
        weather_db,
        breakdown.antenna_db,
    );
    let fading_db = fading.sample_db(ctx, LinkKey(tx.node, rx.node), distance_m, t);
    LinkBudget {
        rx_power_dbm: breakdown.rx_power_dbm(tx_power_dbm) + fading_db,
        breakdown,
        fading_db,
        distance_m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fading::{NakagamiFading, NakagamiPreset, NoFading};
    use crate::obstacle::{BuildingShadowing, VehicleBlockage};
    use crate::prop::{FreeSpace, LogDistancePreset, LogDistanceShadowing, friis_loss_db};
    use crate::testctx::{TestCtx, world_with_building};
    use crate::types::{ActorClass, ActorObstacle, EdgeSource, KnifeEdge};
    use v2xw_core::card::Tier;
    use v2xw_core::geom::{Dims, Vec3};
    use v2xw_core::ids::{ActorId, NodeId};
    use v2xw_world::model::EnvClass;

    fn endpoint(id: u32, x: f64, y: f64, h: f64) -> RadioEndpoint {
        RadioEndpoint {
            node: NodeId::new(id),
            pos: Vec3::new(x, y, h),
            gain_dbi: 3.0,
            pattern: None,
            pos_time: 0,
            class: ActorClass::Car,
        }
    }

    #[test]
    fn merging_says_what_blocks_the_link() {
        let building = LosResult::blocked_by_buildings(2, 12.0);
        let vehicle = LosResult {
            class: LosClass::NlosV,
            walls_crossed: 0,
            obstructed_len_m: 0.0,
            knife_edges: vec![KnifeEdge {
                d1_m: 50.0,
                d2_m: 50.0,
                h_m: 1.0,
                source: EdgeSource::Vehicle {
                    actor: ActorId::new(3),
                },
            }],
            corner: None,
        };
        let merged = merge_los(&[building.clone(), vehicle.clone()]);
        assert_eq!(merged.class, LosClass::NlosBv);
        assert_eq!(merged.walls_crossed, 2);
        assert!((merged.obstructed_len_m - 12.0).abs() < 1e-12);
        assert_eq!(merged.knife_edges.len(), 1);
        // One part alone keeps its own class.
        assert_eq!(merge_los(&[building]).class, LosClass::NlosB);
        assert_eq!(merge_los(&[vehicle]).class, LosClass::NlosV);
        assert_eq!(merge_los(&[]).class, LosClass::Los);
        assert_eq!(merge_los(&[LosResult::clear()]).class, LosClass::Los);
    }

    #[test]
    fn a_medium_tier_stack_composes_into_one_budget() {
        let mut ctx = TestCtx::new(31);
        let world = world_with_building(40.0, -5.0, 50.0, 5.0, 12.0);
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 100.0, 0.0, 1.5);
        let buildings = BuildingShadowing::new(Tier::Medium);
        let vehicles = VehicleBlockage::new(Tier::Medium);
        let actors = ActorSet::from_iter_sorted([ActorObstacle {
            actor: ActorId::new(0),
            pos: Vec3::new(70.0, 0.0, 0.0),
            dims: Dims {
                length_m: 13.0,
                width_m: 2.6,
                height_m: 3.0,
            },
            heading_rad: 0.0,
            class: ActorClass::Truck,
        }]);

        // Geometry first, with both models.
        let los = classify::<TestCtx>(&world, &tx, &rx, Some(&actors), &[&buildings, &vehicles]);
        assert_eq!(los.class, LosClass::NlosBv, "a building and a truck");
        assert_eq!(los.walls_crossed, 2);

        // Then the budget. The propagation model carries no building term of its own —
        // that is the default — so the building loss comes from the obstacle model and
        // nothing is counted twice.
        let mut propagation = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasLosUrban,
            EnvClass::Urban,
        );
        let mut fading = NoFading::new();
        let mut buildings = BuildingShadowing::new(Tier::Medium);
        let mut vehicles = VehicleBlockage::new(Tier::Medium);
        let mut stack: Vec<&mut dyn ObstacleModel<TestCtx>> = vec![&mut buildings, &mut vehicles];
        let budget = evaluate(
            &mut ctx,
            &mut propagation,
            &mut stack,
            &mut fading,
            &tx,
            &rx,
            5.9e9,
            &los,
            &WeatherState::CLEAR,
            23.0,
            0,
        );
        assert!((budget.distance_m - 100.0).abs() < 1e-9);
        assert_eq!(budget.fading_db, 0.0, "the medium tier ignores fast fading");
        // The building term is 9·2 + 0.4·10 = 22 dB, and the truck adds its own.
        assert!(
            budget.breakdown.obstacle_db > 22.0,
            "{:?}",
            budget.breakdown
        );
        assert!(budget.breakdown.antenna_db > 0.0);
        // The total is the sum of the terms, and the received power follows from it.
        let terms = budget.breakdown.path_db
            + budget.breakdown.shadow_db
            + budget.breakdown.obstacle_db
            + budget.breakdown.weather_db
            - budget.breakdown.antenna_db;
        assert!((budget.breakdown.total_db - terms).abs() < 1e-9);
        assert!((budget.rx_power_dbm - (23.0 - budget.breakdown.total_db)).abs() < 1e-9);
        // An obstructed 100 m link cannot be as strong as a clear one.
        assert!(budget.rx_power_dbm < -70.0, "{}", budget.rx_power_dbm);
        // And the quantised form is what a recorder writes.
        let q = budget.quantized();
        assert!(v2xw_core::math::is_on_grid(
            q.rx_power_dbm,
            crate::numeric::Q_DB
        ));
        assert!(v2xw_core::math::is_on_grid(
            q.breakdown.total_db,
            crate::numeric::Q_DB
        ));
    }

    /// The exact medium-tier stack 04-models.md §3.5 names, on the link the validator
    /// measured: a 150 m link crossing one building, two exterior walls and 40 m of
    /// in-building path. The Sommer loss is `9·2 + 0.4·40 = 34.0 dB` and it must appear
    /// **once**.
    ///
    /// Before the fix `LogDistanceShadowing::new` shipped `sommer: Some(DEFAULT)`, so
    /// `evaluate` added the propagation model's 34 dB to the obstacle stack's 34 dB and
    /// this assertion read 68.0 dB — 34 dB of received power, enough to turn every
    /// NLOS-building link in a city from marginal to dead.
    #[test]
    fn the_documented_medium_tier_stack_counts_the_building_term_once() {
        let mut ctx = TestCtx::new(77);
        // A building spanning x in [40, 80] on the line of the link: two walls, 40 m
        // inside.
        let world = world_with_building(40.0, -10.0, 80.0, 10.0, 12.0);
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 150.0, 0.0, 1.5);

        let geometry = BuildingShadowing::new(Tier::Medium);
        let los = classify::<TestCtx>(&world, &tx, &rx, None, &[&geometry]);
        assert_eq!(los.class, LosClass::NlosB);
        assert_eq!(los.walls_crossed, 2, "two exterior walls");
        assert!(
            (los.obstructed_len_m - 40.0).abs() < 1e-9,
            "{}",
            los.obstructed_len_m
        );

        // The stack of 04-models.md §3 tier table: path loss + shadowing from the
        // propagation model, buildings and vehicles from the obstacle models, no fading.
        let mut propagation = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasLosUrban,
            EnvClass::Urban,
        );
        assert!(
            !propagation.has_building_term(),
            "the obstacle stack owns obstacle loss"
        );
        let mut fading = NoFading::new();
        let mut buildings = BuildingShadowing::new(Tier::Medium);
        let mut vehicles = VehicleBlockage::new(Tier::Medium);
        let mut stack: Vec<&mut dyn ObstacleModel<TestCtx>> = vec![&mut buildings, &mut vehicles];
        let budget = evaluate(
            &mut ctx,
            &mut propagation,
            &mut stack,
            &mut fading,
            &tx,
            &rx,
            5.9e9,
            &los,
            &WeatherState::CLEAR,
            23.0,
            0,
        );
        // Sommer, once: 9 dB per wall and 0.4 dB per metre inside.
        assert!(
            (budget.breakdown.obstacle_db - 34.0).abs() < 1e-9,
            "expected the Sommer loss once (34.0 dB), got {} dB",
            budget.breakdown.obstacle_db
        );

        // And the standalone case — no obstacle stack at all — reaches the same figure
        // through the opt-in term on the propagation model, not through both at once.
        let mut standalone = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasLosUrban,
            EnvClass::Urban,
        )
        .with_building_term();
        assert!(standalone.has_building_term());
        let mut empty: Vec<&mut dyn ObstacleModel<TestCtx>> = Vec::new();
        let alone = evaluate(
            &mut ctx,
            &mut standalone,
            &mut empty,
            &mut fading,
            &tx,
            &rx,
            5.9e9,
            &los,
            &WeatherState::CLEAR,
            23.0,
            0,
        );
        assert!(
            (alone.breakdown.obstacle_db - 34.0).abs() < 1e-9,
            "{}",
            alone.breakdown.obstacle_db
        );
        // The two routes agree on the obstacle term exactly, which is what "exactly one
        // path owns it" has to mean numerically.
        assert!((alone.breakdown.obstacle_db - budget.breakdown.obstacle_db).abs() < 1e-12);
    }

    /// Composing both paths is a programming error, and a debug build says so rather than
    /// silently doubling the loss.
    #[test]
    #[should_panic(expected = "obstacle loss counted twice")]
    #[cfg(debug_assertions)]
    fn composing_both_obstacle_paths_is_caught() {
        let mut ctx = TestCtx::new(78);
        let world = world_with_building(40.0, -10.0, 80.0, 10.0, 12.0);
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 150.0, 0.0, 1.5);
        let geometry = BuildingShadowing::new(Tier::Medium);
        let los = classify::<TestCtx>(&world, &tx, &rx, None, &[&geometry]);
        let mut propagation = LogDistanceShadowing::new(
            Tier::Medium,
            LogDistancePreset::AbbasLosUrban,
            EnvClass::Urban,
        )
        .with_building_term();
        let mut fading = NoFading::new();
        let mut buildings = BuildingShadowing::new(Tier::Medium);
        let mut stack: Vec<&mut dyn ObstacleModel<TestCtx>> = vec![&mut buildings];
        let _ = evaluate(
            &mut ctx,
            &mut propagation,
            &mut stack,
            &mut fading,
            &tx,
            &rx,
            5.9e9,
            &los,
            &WeatherState::CLEAR,
            23.0,
            0,
        );
    }

    #[test]
    fn a_high_tier_stack_adds_fading_and_nothing_else_changes() {
        let mut ctx = TestCtx::new(32);
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 150.0, 0.0, 1.5);
        let mut propagation = FreeSpace::new(Tier::High);
        let mut fading = NakagamiFading::new(NakagamiPreset::FixedLow);
        let mut stack: Vec<&mut dyn ObstacleModel<TestCtx>> = Vec::new();
        let budget = evaluate(
            &mut ctx,
            &mut propagation,
            &mut stack,
            &mut fading,
            &tx,
            &rx,
            5.9e9,
            &LosResult::clear(),
            &WeatherState::CLEAR,
            23.0,
            1_000_000,
        );
        // Free space with 3 dBi at each end.
        let expected_loss = friis_loss_db(150.0, 5.9e9) - 6.0;
        assert!((budget.breakdown.total_db - expected_loss).abs() < 1e-9);
        // The fading sample is not zero and moves the received power by exactly itself.
        assert_ne!(budget.fading_db, 0.0);
        assert!((budget.rx_power_dbm - (23.0 - expected_loss + budget.fading_db)).abs() < 1e-9);
    }
}
