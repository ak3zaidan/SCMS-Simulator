//! `mobility/demand/poisson-thinned` — the arrival process (04-models.md §2.4).
//!
//! # The process
//!
//! Candidate arrivals form a Poisson process at rate `arrival_rate · cand_boost`, and each
//! candidate is **kept** with probability `m(f) / cand_boost`, where `m` is the time-of-day
//! shape ([`DemandProfile`]) and `f = t / duration`. Thinning a homogeneous process by an
//! acceptance probability is an exact simulation of the inhomogeneous process of rate
//! `arrival_rate · m(f)`, which is why the boost exists: a scenario whose event multipliers
//! can push the rate above the base rate raises the candidate rate once, and the thinning
//! takes it back down everywhere else. With no events the boost is 1 and the draw sequence
//! is the legacy engine's [`run.py` L1705-1730].
//!
//! # Why the cursor is state
//!
//! `spawns_in` is called once per mobility step with consecutive windows, but the arrival
//! process does not know about windows: it has a *next candidate time*, which may be many
//! windows away. Keeping that cursor in the model is what makes the draw sequence
//! independent of how the caller chopped time up — the property invariant I-M2 needs, and
//! the one a model that redrew per window would lose.
//!
//! # Draw count
//!
//! Per candidate: one exponential for the next arrival time, one coin for the thinning,
//! and — only if the candidate survives — one class draw, three origin-destination draws
//! and one lane-offset draw, all from the actor-independent `spawn` stream. The desired
//! speed comes from a *per-trip* `desired-speed` stream, so a change to the fleet mix
//! cannot shift the speeds of unrelated trips.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::{Duration, SimTime, secs_to_ns};
use v2xw_world::World;

use crate::ctx::MobCtx;
use crate::demand::{
    DemandProfile, FleetMix, LEGACY_ARRIVAL_RATE_PER_S, OdModel, OdParams, SpeedLaw,
    shared_parameters,
};
use crate::error::Result;
use crate::traits::Demand;
use crate::views::TripRequest;

/// The model id.
pub const MODEL_ID: &str = "mobility/demand/poisson-thinned";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The arrival process's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoissonParams {
    /// Base arrival rate, trips per second.
    pub arrival_rate_per_s: f64,
    /// Candidate-rate boost. At least 1; a scenario with demand surges sets it to the
    /// largest multiplier any event applies, so the thinning can reproduce the surge.
    pub candidate_boost: f64,
    /// Cap on the total number of trips, or `0` for no cap (the legacy
    /// `max_total_vehicles`).
    pub max_total_vehicles: u64,
    /// The time-of-day shape.
    pub profile: DemandProfile,
    /// The run duration, which turns an instant into the fraction `f` the shape takes.
    pub duration: Duration,
    /// Which classes the fleet is drawn from.
    pub fleet: FleetMix,
    /// How a trip's desired speed is drawn.
    pub speed: SpeedLaw,
}

impl Default for PoissonParams {
    fn default() -> Self {
        Self {
            arrival_rate_per_s: LEGACY_ARRIVAL_RATE_PER_S,
            candidate_boost: 1.0,
            max_total_vehicles: 0,
            profile: DemandProfile::Uniform,
            duration: Duration::from_secs(600),
            fleet: FleetMix::CarsOnly,
            speed: SpeedLaw::default(),
        }
    }
}

impl PoissonParams {
    /// The candidate rate, trips per second.
    pub fn candidate_rate_per_s(&self) -> f64 {
        self.arrival_rate_per_s * self.candidate_boost.max(1.0)
    }
}

/// The thinned-Poisson demand model.
#[derive(Debug, Clone)]
pub struct PoissonDemand {
    params: PoissonParams,
    od: OdModel,
    /// The next candidate arrival time, once the process has started.
    next: Option<SimTime>,
    /// How many trips have been produced, which is also the next trip's `seq` (I-M2).
    seq: u64,
    card: ModelCard,
}

impl PoissonDemand {
    /// The model over `world`.
    ///
    /// # Errors
    ///
    /// Whatever [`OdModel::build`] refuses — a world with no lane the fleet can use.
    pub fn new(world: &World, params: PoissonParams, od: OdParams) -> Result<Self> {
        let od = OdModel::build(world, od)?;
        Ok(Self {
            card: card(&params, od.params()),
            params,
            od,
            next: None,
            seq: 0,
        })
    }

    /// The parameters in force.
    pub fn params(&self) -> &PoissonParams {
        &self.params
    }

    /// The origin-destination model.
    pub fn od(&self) -> &OdModel {
        &self.od
    }

    /// How many trips it has produced.
    pub fn produced(&self) -> u64 {
        self.seq
    }

    /// The demand shape's value at `t`.
    pub fn multiplier_at(&self, t: SimTime) -> f64 {
        let duration = self.params.duration.as_secs_f64();
        let f = if duration > 0.0 {
            (v2xw_core::time::ns_to_secs(t) / duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.params.profile.multiplier(f)
    }

    /// The instant of the candidate after `t`.
    fn advance(&self, ctx: &mut dyn MobCtx, t: SimTime) -> SimTime {
        let rate = self.params.candidate_rate_per_s();
        if rate <= 0.0 {
            return SimTime::MAX;
        }
        let gap_s = ctx
            .rng(RngDomain::Spawn, EntityRef::Global)
            .exponential(rate);
        t.saturating_add(secs_to_ns(gap_s).max(1))
    }

    /// Turns a surviving candidate into a trip.
    fn make_trip(&mut self, ctx: &mut dyn MobCtx, t: SimTime) -> Option<TripRequest> {
        let multiplier = self.multiplier_at(t);
        let keep = multiplier / self.params.candidate_boost.max(1.0);
        if !ctx
            .rng(RngDomain::Spawn, EntityRef::Global)
            .bool(keep.clamp(0.0, 1.0))
        {
            return None;
        }
        if self.params.max_total_vehicles > 0 && self.seq >= self.params.max_total_vehicles {
            return None;
        }
        let class = {
            let mut rng = ctx.rng(RngDomain::Spawn, EntityRef::Global);
            self.params.fleet.draw(&mut rng)
        };
        let (origin, destination) = {
            let world = ctx.world();
            // The world borrow and the RNG borrow are both `&self`-shaped, so they coexist.
            let mut rng = ctx.rng(RngDomain::Spawn, EntityRef::Global);
            self.od.draw(world, &mut rng, multiplier)
        };
        let origin_s_m = {
            let length = ctx.world().lane(origin).length_m;
            let mut rng = ctx.rng(RngDomain::Spawn, EntityRef::Global);
            rng.uniform(0.0, length)
        };
        let seq = self.seq;
        // A per-trip stream, so a change to the fleet mix or the arrival rate cannot shift
        // the speed an unrelated trip was given.
        let desired_speed_mps = {
            let mut rng = ctx.rng(RngDomain::DesiredSpeed, EntityRef::custom(MODEL_ID, seq));
            self.params.speed.draw(class, &mut rng)
        };
        self.seq += 1;
        Some(TripRequest {
            seq,
            t,
            origin,
            origin_s_m,
            destination,
            class,
            desired_speed_mps,
        })
    }
}

impl v2xw_core::model::Model for PoissonDemand {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Demand for PoissonDemand {
    fn spawns_in(&mut self, ctx: &mut dyn MobCtx, from: SimTime, to: SimTime) -> Vec<TripRequest> {
        let mut out = Vec::new();
        if to <= from {
            return out;
        }
        if self.next.is_none() {
            self.next = Some(self.advance(ctx, from));
        }
        while let Some(t) = self.next {
            if t >= to {
                break;
            }
            if t >= from {
                if let Some(trip) = self.make_trip(ctx, t) {
                    out.push(trip);
                }
            }
            self.next = Some(self.advance(ctx, t));
        }
        out
    }
}

/// The model card.
pub fn card(params: &PoissonParams, od: &OdParams) -> ModelCard {
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L1705-1730 (the thinning loop)"
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: None,
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "Trip arrivals as a thinned Poisson process: candidates at a boosted constant rate, \
         kept with the probability the time-of-day shape gives, which is an exact \
         simulation of the inhomogeneous process.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "candidate process".to_string(),
            latex_or_text: "inter-arrival ~ Exp(arrival_rate · cand_boost)".to_string(),
            notes: None,
        },
        Equation {
            name: "thinning".to_string(),
            latex_or_text: "keep with probability m(f) / cand_boost,  f = t / duration".to_string(),
            notes: Some(
                "exact for the inhomogeneous process of rate arrival_rate·m(f) whenever \
                 cand_boost ≥ sup m"
                    .to_string(),
            ),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "arrival_rate",
            "1/s",
            serde_json::json!(params.arrival_rate_per_s),
            legacy.clone(),
        ),
        Parameter::new(
            "cand_boost",
            "1",
            serde_json::json!(params.candidate_boost),
            legacy.clone(),
        ),
        Parameter::new(
            "max_total_vehicles",
            "-",
            serde_json::json!(params.max_total_vehicles),
            legacy.clone(),
        ),
        Parameter::new(
            "duration",
            "s",
            serde_json::json!(params.duration.as_secs_f64()),
            legacy,
        ),
    ];
    card.parameters.extend(shared_parameters(
        params.profile,
        params.fleet,
        params.speed,
    ));
    card.parameters.push(Parameter::new(
        "od",
        "-",
        serde_json::json!(od.law.label()),
        Source::new(
            SourceKind::Code,
            "the origin-destination model, carded separately as \
             `mobility/demand/od-gravity`",
        ),
    ));
    card.assumptions = vec![
        "The arrival cursor is model state, so the draw sequence does not depend on how \
         the caller chopped time into windows (invariant I-M2)."
            .to_string(),
        "Role coins (attacker, faulty, colluder) are drawn in the threat layer, not here \
         (04-models.md §2.4)."
            .to_string(),
    ];
    card.limitations = vec![
        "A candidate boost below the shape's supremum under-represents the peaks; \
         `DemandProfile::supremum` is what a scenario should set it from."
            .to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![
            RngDomain::Spawn.as_str().to_string(),
            RngDomain::DesiredSpeed.as_str().to_string(),
        ],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "demand::poisson::tests::the_mean_arrival_rate_matches_the_parameter".to_string(),
            "demand::poisson::tests::the_stream_does_not_depend_on_the_window_size".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::MobilityCtx;
    use v2xw_core::model::Model;
    use v2xw_core::rng::RngRegistry;
    use v2xw_core::time::NS_PER_S;
    use v2xw_world::{ImportOptions, procedural::GridParams};

    fn world() -> World {
        v2xw_world::procedural::grid(&GridParams::legacy(), &ImportOptions::default())
            .expect("grid")
    }

    fn demand(world: &World, params: PoissonParams) -> PoissonDemand {
        PoissonDemand::new(world, params, OdParams::default()).expect("built")
    }

    /// Collects every trip in `[0, seconds)` using `step`-second windows.
    fn collect(
        model: &mut PoissonDemand,
        world: &World,
        rng: &RngRegistry,
        seconds: u64,
        step_ms: u64,
    ) -> Vec<TripRequest> {
        let mut out = Vec::new();
        let step = step_ms * v2xw_core::time::NS_PER_MS;
        let mut t = 0u64;
        while t < seconds * NS_PER_S {
            let mut ctx = MobilityCtx::new(t, world, rng);
            out.extend(model.spawns_in(&mut ctx, t, t + step));
            t += step;
        }
        out
    }

    #[test]
    fn the_mean_arrival_rate_matches_the_parameter() {
        let w = world();
        let rng = RngRegistry::new(1234);
        let mut m = demand(
            &w,
            PoissonParams {
                arrival_rate_per_s: 2.0,
                duration: Duration::from_secs(300),
                ..PoissonParams::default()
            },
        );
        let trips = collect(&mut m, &w, &rng, 300, 100);
        // 2 trips/s over 300 s is 600 trips; a Poisson count has standard deviation √600 ≈
        // 24.5, so a 5 % window is about two sigma.
        let n = trips.len() as f64;
        assert!((n - 600.0).abs() < 60.0, "{n} trips");
        // Times are inside the window and non-decreasing, and `seq` is dense from zero.
        for (i, trip) in trips.iter().enumerate() {
            assert_eq!(trip.seq, i as u64, "seq is the demand stream order (I-M2)");
            if i > 0 {
                assert!(trip.t >= trips[i - 1].t);
            }
        }
    }

    #[test]
    fn the_stream_does_not_depend_on_the_window_size() {
        // One registry per run: the registry is stateful, so a shared one would have the
        // second run continue the first one's streams.
        let w = world();
        let mut coarse = demand(&w, PoissonParams::default());
        let a = collect(&mut coarse, &w, &RngRegistry::new(99), 60, 1000);
        let mut fine = demand(&w, PoissonParams::default());
        let b = collect(&mut fine, &w, &RngRegistry::new(99), 60, 10);
        assert_eq!(a.len(), b.len(), "{} vs {}", a.len(), b.len());
        assert_eq!(a, b, "the trips are identical whatever the step");
    }

    #[test]
    fn the_rush_profile_peaks_where_the_shape_does() {
        let w = world();
        let rng = RngRegistry::new(7);
        let mut m = demand(
            &w,
            PoissonParams {
                arrival_rate_per_s: 4.0,
                profile: DemandProfile::Rush,
                duration: Duration::from_secs(1000),
                ..PoissonParams::default()
            },
        );
        let trips = collect(&mut m, &w, &rng, 1000, 100);
        // Count trips in the morning peak (f ∈ [0.2, 0.3]) against the trough
        // (f ∈ [0.45, 0.55]): the shape says about 1.0 against about 0.2.
        let count_in = |lo: f64, hi: f64| {
            trips
                .iter()
                .filter(|t| {
                    let f = v2xw_core::time::ns_to_secs(t.t) / 1000.0;
                    f >= lo && f < hi
                })
                .count()
        };
        let peak = count_in(0.2, 0.3);
        let trough = count_in(0.45, 0.55);
        assert!(
            peak > 3 * trough,
            "peak {peak} is not markedly above the trough {trough}"
        );
    }

    #[test]
    fn the_cap_bounds_the_total() {
        let w = world();
        let rng = RngRegistry::new(3);
        let mut m = demand(
            &w,
            PoissonParams {
                arrival_rate_per_s: 10.0,
                max_total_vehicles: 25,
                ..PoissonParams::default()
            },
        );
        let trips = collect(&mut m, &w, &rng, 60, 100);
        assert_eq!(trips.len(), 25);
        assert_eq!(m.produced(), 25);
    }

    #[test]
    fn the_boost_is_thinned_back_out() {
        // The same seed with and without a boost must give the same *rate*, because the
        // thinning is exact. (Not the same trips: the boost changes the draw sequence.)
        let w = world();
        let mut plain = demand(
            &w,
            PoissonParams {
                arrival_rate_per_s: 3.0,
                ..PoissonParams::default()
            },
        );
        let mut boosted = demand(
            &w,
            PoissonParams {
                arrival_rate_per_s: 3.0,
                candidate_boost: 4.0,
                ..PoissonParams::default()
            },
        );
        let a = collect(&mut plain, &w, &RngRegistry::new(55), 400, 100).len() as f64;
        let b = collect(&mut boosted, &w, &RngRegistry::new(55), 400, 100).len() as f64;
        assert!(
            (a - b).abs() / a < 0.15,
            "the boosted rate {b} differs from the plain rate {a} by more than 15 %"
        );
    }

    #[test]
    fn every_trip_is_usable() {
        let w = world();
        let rng = RngRegistry::new(8);
        let mut m = demand(
            &w,
            PoissonParams {
                fleet: FleetMix::LegacyMixed,
                ..PoissonParams::default()
            },
        );
        let trips = collect(&mut m, &w, &rng, 30, 100);
        assert!(!trips.is_empty());
        for trip in &trips {
            let origin = w.lane(trip.origin);
            assert!(origin.kind.is_motorised());
            assert!((0.0..=origin.length_m).contains(&trip.origin_s_m));
            assert!(trip.desired_speed_mps > 0.0);
            assert!(w.lane(trip.destination).kind.is_motorised());
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_trips() {
        let w = world();
        let rng_a = RngRegistry::new(4242);
        let rng_b = RngRegistry::new(4242);
        let mut a = demand(&w, PoissonParams::default());
        let mut b = demand(&w, PoissonParams::default());
        assert_eq!(
            collect(&mut a, &w, &rng_a, 120, 100),
            collect(&mut b, &w, &rng_b, 120, 100)
        );
    }

    #[test]
    fn the_card_validates() {
        let w = world();
        demand(&w, PoissonParams::default())
            .card()
            .validate()
            .expect("validates");
    }
}
