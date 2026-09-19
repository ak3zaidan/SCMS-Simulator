//! `v2xw-mobility` — the native mobility tiers: ground-truth kinematics, demand, signal
//! state, and the node-side GNSS and clock beliefs.
//!
//! This crate implements 04-models.md §2 (mobility and actors) and §3.8 (GNSS error and
//! clock drift) against the traits of 03-interfaces.md §3. It never touches radio,
//! security or messages (02-architecture.md §2, ADR 0010); it produces the positions
//! everything else is measured at.
//!
//! # Where to look
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | The traits every model implements | [`traits`] | 03-interfaces.md §3 |
//! | The values they exchange | [`views`] | — (03-interfaces names them, this crate defines them) |
//! | The context slice a model may read | [`ctx`] | 03-interfaces.md §1.1 |
//! | The start-of-step snapshot and the one neighbour query | [`snapshot`] | ADR 0004 §5-6 |
//! | Vehicle classes and dimensions | [`classes`] | 04-models.md §2.7 |
//! | Car-following (IDM) | [`carfollowing`] | 04-models.md §2.1 |
//! | Lane change (MOBIL) | [`lanechange`] | 04-models.md §2.2 |
//! | Intersections (gap acceptance, fixed-time signals, two-colouring) | [`intersection`] | 04-models.md §2.3 |
//! | Routing (Dijkstra, dynamic rerouting) | [`routing`] | 04-models.md §2.4 |
//! | Demand (thinned Poisson, origin-destination, 3GPP drops) | [`demand`] | 04-models.md §2.4 |
//! | VRUs (social force) | [`vru`] | 04-models.md §2.5 |
//! | Weather's effect on driving | [`weather`] | 04-models.md §2.6 |
//! | GNSS error | [`gnss`] | 04-models.md §2.10, §3.8 |
//! | Clock drift | [`clock`] | 04-models.md §2.10, §3.8 |
//! | The abstract tier | [`kinematic`] | 04-models.md §2.1 |
//! | The engine that ties them together | [`engine`] | 03-interfaces.md §3 |
//! | The fundamental-diagram validation | [`fd`] | 04-models.md §2.9 |
//! | Synthetic worlds for validation runs | [`worlds`] | 04-models.md §2.9 |
//!
//! # The four rules this crate keeps
//!
//! 1. **Every step is a Jacobi update.** Each step freezes every actor's state into an
//!    [`snapshot::ActorSnapshot`]; every decision reads only the frozen copy and every
//!    result is buffered until the end of the step. The published output therefore does not
//!    depend on the order the actors are visited in, which ADR 0004 requires and which
//!    `engine::tests::the_jacobi_update_is_order_independent` asserts directly by running a
//!    whole simulation backwards and comparing it bit for bit.
//! 2. **One neighbour query per actor per step.** The car-following leader search and the
//!    lane-change neighbour classification are the same walk of the lane graph
//!    ([`snapshot::ActorSnapshot::neighbors`]), as 04-models.md §2.2 requires. The walk
//!    follows the *directed* lane graph, so everything it finds is travelling the same way
//!    as the ego — "same-direction only" by construction rather than by a heading window.
//! 3. **Every random number comes from a keyed stream.** `(domain, entity)`, through
//!    [`ctx::MobCtx::rng`]: never a thread-local, never an ad-hoc generator, and never a
//!    stream shared between two models — the clock model draws from its own plug-in domain
//!    precisely so its draws cannot depend on how often the GNSS model has drawn for the
//!    same node.
//! 4. **Every default is cited.** Every number in this crate comes from 04-models.md with
//!    its source on the model's card, or it is written as a `TODO: calibrate` parameter with
//!    a calibration plan. Where a value had to be chosen — the light/heavy weather
//!    threshold, the perimeter band of the origin sampler, the fold of twelve vehicle
//!    classes onto two parameter columns — the choice is on the card as a choice, not
//!    dressed as a measurement.
//!
//! # No standard-library transcendental
//!
//! Every `sin`, `cos`, `exp`, `ln`, `pow`, `tan` and `atan2` in this crate goes through
//! [`v2xw_core::math`], which is the pure-Rust `libm` port. The standard library's
//! delegate to a platform libm whose last bit differs between platforms (ADR 0003), and
//! ADR 0004's own evidence is a corpus whose golden digests broke for exactly that reason.
//!
//! # Getting one running
//!
//! ```no_run
//! use v2xw_core::rng::RngRegistry;
//! use v2xw_mobility::ctx::MobilityCtx;
//! use v2xw_mobility::demand::{OdParams, PoissonDemand, PoissonParams};
//! use v2xw_mobility::engine::{EngineParams, NativeMobility};
//! use v2xw_mobility::traits::Mobility;
//! use v2xw_world::{ImportOptions, procedural::GridParams};
//!
//! let world = v2xw_world::procedural::grid(&GridParams::legacy(), &ImportOptions::default())?;
//! let rng = RngRegistry::new(0xC0FFEE);
//! let params = EngineParams::default();
//! let mut mobility = NativeMobility::new(params);
//! let demand = PoissonDemand::new(&world, PoissonParams::default(), OdParams::default())?;
//!
//! let mut ctx = MobilityCtx::new(0, &world, &rng);
//! mobility.init(&mut ctx, Box::new(demand))?;
//! let update = mobility.step(&mut ctx, params.step);
//! assert_eq!(update.t, params.step.as_nanos());
//! # Ok::<(), v2xw_mobility::error::MobError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod carfollowing;
pub mod classes;
pub mod clock;
pub mod ctx;
pub mod demand;
pub mod engine;
pub mod error;
pub mod fd;
pub mod gnss;
pub mod intersection;
pub mod kinematic;
pub mod lanechange;
pub mod routing;
pub mod snapshot;
pub mod traits;
pub mod views;
pub mod vru;
pub mod weather;
pub mod worlds;

pub use carfollowing::idm::{Idm, IdmParams, IdmPreset};
pub use classes::{LegacyClass, Tr37885Type, VehicleClass};
pub use clock::{DriftFreeClock, Oscillator, OscillatorClock};
pub use ctx::{CoreCtx, MobCtx, MobilityCtx};
pub use demand::{DemandProfile, FleetMix, NoDemand, OdModel, PoissonDemand, SpeedLaw};
pub use engine::{EngineParams, IntersectionMode, NativeMobility};
pub use error::{MobError, Result};
pub use fd::{FdParams, FdResult};
pub use gnss::{GaussMarkovGnss, LegacyGnss};
pub use intersection::{FixedTimeSignals, GapAcceptance, TwoColoring};
pub use kinematic::KinematicLaneFollow;
pub use lanechange::{Mobil, MobilPreset};
pub use routing::{Dijkstra, DynamicCost, DynamicReroute, FreeFlowCost};
pub use snapshot::{ActorSnapshot, NeighborOptions};
pub use traits::{
    CarFollowing, ClockModel, Demand, GnssModel, IntersectionControl, LaneChange, Mobility, Router,
    VruMobility,
};
pub use views::{
    ActorSpawn, ConflictView, DespawnCause, DriverProfile, EdgeCost, EntryDecision, GnssEnv,
    JunctionView, LaneChangeDecision, LaneNeighbors, LaneView, LeaderView, MobilityCommand,
    MobilityUpdate, PhaseState, ReroutePolicy, Route, Side, SideNeighbors, SkyView, TripRequest,
    VehicleView,
};
pub use vru::SocialForce;
pub use weather::{RoadContext, WeatherResponse};

/// Every model this crate registers, as `(id, card)` in id order.
///
/// A scenario loader walks this to fill the registry, and the manifest's model list comes
/// from it. Every card here validates ([`v2xw_core::card::ModelCard::validate`]), which the
/// crate's own test asserts, so a registry built from it cannot fail on rule R1 (a
/// `todo-calibrate` parameter without a calibration plan).
pub fn model_cards() -> Vec<(String, v2xw_core::card::ModelCard)> {
    let engine_params = engine::EngineParams::default();
    let mut cards = vec![
        classes::sumo_vtypes_card(),
        classes::legacy_fleet_card(),
        classes::tr37885_types_card(),
        carfollowing::idm::card(IdmPreset::Kesting2010, &IdmPreset::Kesting2010.params()),
        lanechange::mobil::card(
            MobilPreset::Kesting2007,
            &MobilPreset::Kesting2007.params(),
            carfollowing::idm::MODEL_ID.to_string(),
        ),
        intersection::gap_acceptance::card(
            &intersection::gap_acceptance::GapAcceptanceParams::default(),
        ),
        intersection::signal_fixed_time::card(
            &intersection::signal_fixed_time::SignalPlanParams::default(),
        ),
        intersection::two_coloring::card(&intersection::two_coloring::TwoColoringParams::default()),
        routing::dijkstra::card(
            &routing::dijkstra::DijkstraParams::default(),
            views::ReroutePolicy::STATIC,
        ),
        routing::dijkstra::card(
            &routing::dijkstra::DijkstraParams::default(),
            views::ReroutePolicy::ON_CLOSURE,
        ),
        demand::poisson::card(
            &demand::poisson::PoissonParams::default(),
            &demand::od::OdParams::default(),
        ),
        demand::od::card(&demand::od::OdParams::default()),
        demand::tr36885::card(&demand::tr36885::DropParams::default()),
        vru::social_force::card(&vru::social_force::SocialForceParams::default()),
        gnss::gauss_markov::card(
            &gnss::gauss_markov::GaussMarkovParams::default(),
            &[],
            0.0,
            0.0,
        ),
        gnss::ou_bias_legacy::card(&gnss::ou_bias_legacy::LegacyGnssParams::default()),
        clock::tcxo_ocxo::card(&clock::tcxo_ocxo::OscillatorParams::default()),
        clock::none::card(),
        kinematic::card(&kinematic::LaneFollowParams::default()),
        engine::card(&engine_params, carfollowing::idm::MODEL_ID),
    ];
    cards.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.version.cmp(&b.version)));
    cards.into_iter().map(|c| (c.id.clone(), c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::card::SourceKind;

    #[test]
    fn every_card_validates_and_every_uncited_default_has_a_plan() {
        let cards = model_cards();
        assert!(cards.len() >= 18, "{} cards", cards.len());
        for (id, card) in &cards {
            card.validate().unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(!card.purpose.trim().is_empty(), "{id} has no purpose");
            assert!(!card.tier.is_empty(), "{id} implements no tier");
            for p in &card.parameters {
                if p.source.kind == SourceKind::TodoCalibrate {
                    assert!(
                        p.calibration.as_ref().is_some_and(|c| !c.trim().is_empty()),
                        "{id}: parameter {} needs a calibration plan",
                        p.name
                    );
                } else {
                    assert!(
                        !p.source.reference.trim().is_empty(),
                        "{id}: parameter {} has an empty source",
                        p.name
                    );
                }
            }
        }
    }

    #[test]
    fn the_todo_calibrate_report_is_short_and_explicit() {
        // Every uncited default in the crate, in one place: this is the list ADR 0007 §3
        // generates a page from, and seeing it grow is the point of the test.
        let mut todos: Vec<String> = Vec::new();
        for (id, card) in model_cards() {
            for p in card.todo_calibrate() {
                todos.push(format!("{id}:{}", p.name));
            }
        }
        todos.sort();
        // Each of these is a parameter 04-models.md itself marks `TODO: calibrate`, or one
        // this crate refused to invent a value for.
        let expected = [
            "gnss/error/gauss-markov:outlier_rate",
            "gnss/error/gauss-markov:sigma_heading",
            "gnss/error/ou-bias-legacy:sigma_heading",
            "mobility/car-following/idm:a_min",
            "mobility/car-following/idm:weather.heavy_intensity_threshold",
            "mobility/car-following/idm:weather.max_decel_mps2",
            "mobility/demand/poisson-thinned:wall_clock_mapping",
            "mobility/lane-change/mobil:politeness",
            "vru/pedestrian/social-force:fluctuation",
        ];
        for want in expected {
            assert!(
                todos.iter().any(|t| t == want),
                "{want} is missing from the todo-calibrate report: {todos:?}"
            );
        }
    }
}
