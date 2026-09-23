//! **Worked example — the `Propagation` seam.** Log-distance path loss with per-link
//! log-normal shadowing.
//!
//! This crate exists to be read. It is the smallest thing that is a real propagation
//! model: one equation, three parameters, one random draw, and a model card that cites
//! every number. Nothing in the engine depends on it, so you can break it freely.
//!
//! # What it computes
//!
//! ```text
//! PL(d)[dB] = PL0 + 10·n·log10(d / d0) + X_sigma,     X_sigma ~ N(0, sigma²)
//! ```
//!
//! with `d0 = 10 m` and `PL0` the Friis free-space loss at `d0`, so the curve is anchored
//! to physics at the reference distance and to a measured exponent beyond it. This is the
//! log-distance model of `04-models.md` §3.2 in its single-slope form.
//!
//! # What it is *not*
//!
//! The engine already ships `radio/propagation/log-distance-shadowing`, which is a
//! *better* model than this one: it is dual-slope, it carries the Abbas 2015 environment
//! presets, and its shadowing is spatially correlated along the link. This example is
//! deliberately the simpler thing, because the point is the seam and not the physics. Do
//! not use it for a study; use it to learn where your own model plugs in. The differences
//! are listed on the card as [`ModelCard::ignores`] and [`ModelCard::limitations`], which
//! is exactly where a reader of a result looks for them.
//!
//! # The five rules this crate demonstrates
//!
//! 1. **A model card with a source for every default.** See [`card`]. Two of the three
//!    parameters are cited to a design-document table; the third — the reference distance
//!    — is a convention and says so.
//! 2. **Never a standard-library transcendental.** [`v2xw_core::math::log10`], never
//!    `f64::log10`: the standard library routes to the platform's libm, and two machines
//!    that disagree in the last bit of a path loss will eventually disagree about whether
//!    a frame was received (ADR 0004).
//! 3. **Randomness from a keyed stream.** The shadowing draw comes from
//!    `(RngDomain::Shadow, EntityRef::Link(..))`, so one link's realisation cannot depend
//!    on what any other link drew, on evaluation order, or on the thread count.
//! 4. **The total is computed, not asserted.** [`LossBreakdown::new`] sums the terms with
//!    `v2xw_core::math::sum_ordered`, so `total_db` cannot disagree with its parts.
//! 5. **A test that has been shown to fail.** See the module-level tests: one of them
//!    pins the 10·n dB-per-decade slope, which is the only assertion here that
//!    distinguishes this model from free space.
//!
//! # Where to go next
//!
//! `docs/site/content/tutorial.md` walks the detector example end to end, including the
//! conformance kit. The propagation walkthrough in prose is
//! `docs/site/content/extending.md`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use serde_json::json;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::LinkKey;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::weather::WeatherState;
use v2xw_radio::traits::Propagation;
use v2xw_radio::types::{LosResult, LossBreakdown, RadioEndpoint};

/// The model's stable id.
///
/// Lower case, hyphen and slash separated (the card schema enforces
/// `^[a-z0-9][a-z0-9-]*(/[a-z0-9-]+)*$`), and never changed once published: a scenario
/// names the model by it and a run manifest pins it.
///
/// The `example/` prefix is deliberate. It makes an example visible in the registry
/// listing and on the generated model reference, so nobody mistakes one of these for a
/// model the project stands behind.
pub const MODEL_ID: &str = "example/propagation/log-distance";

/// The model's own version, semver. Bump it when the *numbers change*, because the card's
/// content hash is what a manifest pins and what a reader compares two runs by.
pub const MODEL_VERSION: &str = "1.0.0";

/// The reference distance the exponent is measured from, metres.
///
/// 10 m, the convention `04-models.md` §3.2 states for this model family (Karedal 2011
/// Eq. 5, Abbas 2015 Eq. 4). It is a convention rather than a measurement, which is why
/// its card entry cites the design document's choice and not a physical result.
pub const D0_M: f64 = 10.0;

/// Speed of light in vacuum, m/s. A defined constant, not a parameter: nothing calibrates
/// it, so it does not appear on the card.
const C_MPS: f64 = 299_792_458.0;

/// The three numbers this model reads.
///
/// Every field appears on the card with a unit, a default and a source (invariant I-C3).
/// A scenario overrides one by name; anything it does not name keeps the default here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogDistanceParams {
    /// Path-loss exponent `n`, dimensionless. `2.0` is free space; larger is worse.
    pub path_loss_exponent: f64,
    /// Shadowing standard deviation `sigma`, dB. `0.0` disables the draw entirely, which
    /// is what makes this model usable in a determinism test.
    pub shadowing_sigma_db: f64,
    /// The reference distance, metres. Overridable, but see [`D0_M`] before changing it:
    /// `n` and `PL0` were measured together at one reference and do not transfer.
    pub d0_m: f64,
}

impl Default for LogDistanceParams {
    /// The legacy abstract-radio constants of `04-models.md` §3.2: `n = 2.7`,
    /// `sigma = 4.0 dB`.
    ///
    /// They are cited as `code (legacy)` there — the reference engine's own values, read
    /// out of `legacy/scms_sim_ref/mock_pipeline/run.py` L340-355 — which makes them a
    /// real citation with a known provenance and a known weakness: they parameterise an
    /// *abstract* radio tier, not a measured urban channel. The card says so.
    fn default() -> Self {
        Self {
            path_loss_exponent: 2.7,
            shadowing_sigma_db: 4.0,
            d0_m: D0_M,
        }
    }
}

impl LogDistanceParams {
    /// The deterministic part of the loss at distance `d_m` and carrier `f_hz`, dB.
    ///
    /// Separated from [`Propagation::loss_db`] so that a test can assert on the equation
    /// without building a context. That separation is worth copying: the part of a model
    /// that is arithmetic should be callable without an engine.
    ///
    /// Distances below the reference distance return the loss *at* the reference distance.
    /// Extrapolating the fitted exponent inwards is not supported by the measurement it
    /// came from, and letting `log10` run towards minus infinity would make the loss
    /// decrease without bound — which the conformance kit's monotonicity property
    /// (03-interfaces.md §17) rejects, correctly.
    #[must_use]
    pub fn path_loss_db(&self, d_m: f64, f_hz: f64) -> f64 {
        let pl0 = friis_loss_db(self.d0_m, f_hz);
        if d_m <= self.d0_m {
            return pl0;
        }
        // `math::log10`, not `d.log10()`. See rule 2 in the crate documentation.
        pl0 + 10.0 * self.path_loss_exponent * math::log10(d_m / self.d0_m)
    }
}

/// Friis free-space loss between isotropic antennas, dB.
///
/// `20·log10(4·pi·d·f / c)`. The constant form `20·log10(d_km) + 20·log10(f_MHz) + 32.4478`
/// is the same number; `04-models.md` §3.1 records that the textbook's rounded `32.44` is
/// 0.0078 dB optimistic on every link, which is eight times the 1e-3 dB recording quantum,
/// so the closed form is used here instead of the rounded constant.
#[must_use]
pub fn friis_loss_db(d_m: f64, f_hz: f64) -> f64 {
    let d = d_m.max(f64::MIN_POSITIVE);
    20.0 * math::log10(4.0 * core::f64::consts::PI * d * f_hz / C_MPS)
}

/// The model.
///
/// Holds its card and its parameters and nothing else. In particular it holds **no
/// per-link state**: the shadowing realisation is drawn fresh from the link's own stream
/// on every call, which is what lets the model be `Sync` and lets the engine evaluate
/// links in any order on any number of threads. A model that needed per-link state would
/// keep it in a `BTreeMap` keyed by [`LinkKey`] — never a `HashMap`, whose iteration
/// order would reach an output.
#[derive(Debug, Clone)]
pub struct LogDistance {
    card: ModelCard,
    params: LogDistanceParams,
    tier: Tier,
}

impl LogDistance {
    /// A model for `tier` with `params`.
    ///
    /// The card is built here, once, from the same parameters the model will compute with.
    /// A model that built its card lazily, or from different values than it uses, would
    /// make the manifest's pin a lie.
    #[must_use]
    pub fn new(tier: Tier, params: LogDistanceParams) -> Self {
        Self {
            card: card(&params),
            params,
            tier,
        }
    }

    /// A model at the medium tier with the cited defaults.
    #[must_use]
    pub fn medium() -> Self {
        Self::new(Tier::Medium, LogDistanceParams::default())
    }

    /// The parameters this instance computes with.
    #[must_use]
    pub const fn params(&self) -> &LogDistanceParams {
        &self.params
    }
}

impl Model for LogDistance {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Propagation<C> for LogDistance {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn loss_db(
        &mut self,
        ctx: &mut C,
        tx: &RadioEndpoint,
        rx: &RadioEndpoint,
        f_hz: f64,
        // This model classifies nothing: it has one exponent for every link state, which
        // is precisely what makes it the simple example rather than a usable one. The
        // engine's own model branches on `los.class`; the card records the omission.
        _los: &LosResult,
        // Weather attenuation is negligible at 5.9 GHz (04-models.md §3.6: a fraction of
        // a decibel in a 50 mm/h downpour over 300 m), and the medium tier ignores it by
        // design. Naming the argument `_w` rather than silently using it is the honest
        // form: the term is zero because the tier says so, not because it was forgotten.
        _w: &WeatherState,
    ) -> LossBreakdown {
        let d = tx.pos.distance(rx.pos);
        let path = self.params.path_loss_db(d, f_hz);

        // One draw, from this directed link's own Shadow stream. The key is the whole
        // guarantee: `LinkKey(tx, rx)` is not `LinkKey(rx, tx)`, and neither is affected
        // by any other link's draws. The guard returns the stream to the registry when
        // it is dropped at the end of the statement.
        let shadow = if self.params.shadowing_sigma_db > 0.0 {
            let link = LinkKey(tx.node, rx.node);
            ctx.rng(RngDomain::Shadow, EntityRef::Link(link))
                .normal(0.0, self.params.shadowing_sigma_db)
        } else {
            0.0
        };

        // Antenna gain is a gain, not a loss: `LossBreakdown::new` subtracts it. Passing
        // the sum of both ends is what every propagation model in the engine does.
        LossBreakdown::new(path, shadow, 0.0, 0.0, tx.gain_dbi + rx.gain_dbi)
    }
}

/// Builds the card.
///
/// Read this before the model. The card is not documentation attached to the code; it is
/// the model's declared interface, and the registry refuses a model whose card does not
/// validate.
#[must_use]
pub fn card(params: &LogDistanceParams) -> ModelCard {
    // The two measured-ish defaults come from one place, so they cite one source.
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L340-355, via 04-models.md \
                    §3.2 (\"Legacy abstract radio constants\")"
            .to_string(),
        accessed: Some("2026-09-22".to_string()),
        note: Some(
            "The reference engine's own constants: pathloss_exponent 2.7, \
             shadowing_sigma_db 4.0. They parameterise an *abstract* radio tier rather \
             than a measured urban channel, so they are a real citation to a weak \
             source. A study wants the Abbas 2015 presets that \
             radio/propagation/log-distance-shadowing carries instead."
                .to_string(),
        ),
    };
    let convention = Source {
        kind: SourceKind::Paper,
        reference: "Karedal et al. 2011 Eq. 5 and Abbas et al. 2015 Eq. 4, via \
                    04-models.md §3.2"
            .to_string(),
        accessed: Some("2026-09-22".to_string()),
        note: Some(
            "Both fit the exponent against a 10 m reference distance, so 10 m is not a \
             free choice: n and PL0 were measured together at that reference and do not \
             transfer to another one."
                .to_string(),
        ),
    };
    let friis = Source::new(
        SourceKind::Paper,
        "C. Sommer et al. 2011 Eq. 1-2, via 04-models.md §3.1 (the Friis anchor at d0)",
    );

    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Propagation,
        MODEL_VERSION,
        "Worked example. Single-slope log-distance path loss anchored to Friis at a 10 m \
         reference distance, with an uncorrelated per-link log-normal shadowing term. \
         Written to be read rather than used: the engine's own \
         radio/propagation/log-distance-shadowing is dual-slope, carries measured \
         environment presets and correlates shadowing along the link.",
    );

    // A card with no tier describes a model no scenario can select, so `validate`
    // rejects an empty list. This one serves the two tiers that want a received power.
    card.tier = vec![Tier::Medium, Tier::High];

    card.equations = vec![
        Equation {
            name: "path loss".to_string(),
            latex_or_text: "PL(d)[dB] = PL0 + 10·n·log10(d/d0) + X_sigma,  d > d0"
                .to_string(),
            notes: Some(
                "PL0 is the Friis loss at d0. Below d0 the loss is held at PL0: the \
                 fitted exponent does not extrapolate inwards, and letting log10 run \
                 towards minus infinity would break the monotonicity property of \
                 03-interfaces.md §17."
                    .to_string(),
            ),
        },
        Equation {
            name: "shadowing".to_string(),
            latex_or_text: "X_sigma ~ N(0, sigma²), drawn per (tx, rx) link".to_string(),
            notes: Some(
                "Uncorrelated between successive evaluations of the same link, which is \
                 the model's largest simplification — see `ignores`."
                    .to_string(),
            ),
        },
    ];

    card.parameters = vec![
        Parameter {
            range: Some(vec![json!(1.5), json!(6.0)]),
            ..Parameter::new(
                "path_loss_exponent",
                "-",
                json!(params.path_loss_exponent),
                legacy.clone(),
            )
        },
        Parameter {
            range: Some(vec![json!(0.0), json!(12.0)]),
            ..Parameter::new(
                "shadowing_sigma_db",
                "dB",
                json!(params.shadowing_sigma_db),
                legacy,
            )
        },
        Parameter {
            range: Some(vec![json!(1.0), json!(100.0)]),
            ..Parameter::new("d0_m", "m", json!(params.d0_m), convention)
        },
    ];

    card.assumptions = vec![
        "One exponent describes every link state, so a line-of-sight link and a link \
         through a building are priced the same."
            .to_string(),
        "The exponent and the reference loss were fitted together at d0; changing d0_m \
         without refitting them makes the curve arbitrary."
            .to_string(),
    ];

    card.limitations = vec![
        "The default exponent and sigma come from an abstract-tier stand-in, not from a \
         5.9 GHz measurement campaign. Any result sensitive to their exact values should \
         be reproduced with radio/propagation/log-distance-shadowing before it is \
         believed."
            .to_string(),
        "The loss is flat below d0_m, so the model must not be used for links shorter \
         than 10 m."
            .to_string(),
    ];

    card.ignores = vec![
        "Spatial correlation of shadowing along a link: the engine's own model uses the \
         TR 36.885 Annex A.1.4 AR(1) update with a decorrelation distance, and this one \
         draws independently, which makes a moving vehicle's shadowing flicker rather \
         than fade."
            .to_string(),
        "The dual-slope break at 104 m that Abbas 2015 measured."
            .to_string(),
        "Building and vehicle obstruction: supply an obstacle/* model, whose term arrives \
         in LossBreakdown::obstacle_db."
            .to_string(),
        "Fast fading: supply a fading/* model.".to_string(),
        "Weather attenuation, which the medium tier ignores by design (04-models.md §3.6)."
            .to_string(),
    ];

    card.sources = vec![friis];

    // `unit-tested` and not a word more. The tests in this crate check the model against
    // its own equations; nothing outside this repository has confirmed the numbers, and
    // claiming `literature-checked` for a curve nobody compared against a published
    // figure is the flattery the validation page exists to prevent.
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "the_slope_is_ten_n_db_per_decade".to_string(),
            "the_reference_distance_is_the_friis_loss".to_string(),
            "loss_never_decreases_with_distance".to_string(),
            "two_links_do_not_share_a_shadowing_draw".to_string(),
        ],
    };

    // Declared, and checked by the conformance kit against what the model actually drew.
    // A card that said `uses_rng: false` here would fail that check, which is the point
    // of having it.
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::Shadow.as_str().to_string()],
    };

    card
}

/// Registers the model with a registry.
///
/// One call. `register_model` validates the card, hashes its canonical bytes and stores
/// the registration; it refuses an invalid card, a duplicate id, and a copyleft licence
/// for an in-process model.
///
/// # Errors
/// Whatever the registry refused, by name.
pub fn register(
    registry: &mut v2xw_core::registry::Registry,
) -> Result<v2xw_core::registry::ModelRef, v2xw_core::registry::RegistryError> {
    let model: v2xw_core::model::ModelHandle = std::sync::Arc::new(LogDistance::medium());
    registry.register_model(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::event::{EventClass, EventHandle, Scheduler};
    use v2xw_core::ids::{ActorId, NodeId};
    use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
    use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId, Registry};
    use v2xw_core::rng::{RngGuard, RngRegistry};
    use v2xw_core::time::SimTime;
    use v2xw_radio::types::ActorClass;

    /// The 5.9 GHz control channel's centre frequency, Hz.
    const F_HZ: f64 = 5_900e6;

    /// The smallest thing that is a `Ctx`.
    ///
    /// A plug-in's tests need one, and the engine's own is not public. Copy this into your
    /// crate; the only field that matters to a propagation model is the registry.
    struct TestCtx {
        now: SimTime,
        scheduler: Scheduler<u64>,
        rng: RngRegistry,
        provenance: ProvenanceLog,
        params: ParamSet,
        world: (),
        actors: Vec<ActorId>,
    }

    impl TestCtx {
        fn new(seed: u64) -> Self {
            Self {
                now: 0,
                scheduler: Scheduler::new(),
                rng: RngRegistry::new(seed),
                provenance: ProvenanceLog::new(),
                params: ParamSet::new(),
                world: (),
                actors: Vec::new(),
            }
        }
    }

    impl Ctx for TestCtx {
        type World = ();
        type Actors = Vec<ActorId>;
        type Payload = u64;

        fn now(&self) -> SimTime {
            self.now
        }

        fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
            self.rng.checkout(domain, entity)
        }

        fn schedule(
            &mut self,
            at: SimTime,
            class: EventClass,
            payload: Self::Payload,
        ) -> EventHandle {
            self.scheduler.schedule(at, class, payload)
        }

        fn cancel(&mut self, handle: EventHandle) -> bool {
            self.scheduler.cancel(handle)
        }

        fn world(&self) -> &Self::World {
            &self.world
        }

        fn actors(&self) -> &Self::Actors {
            &self.actors
        }

        fn emit_erased(&mut self, _record: &dyn v2xw_core::ctx::ErasedRecord) {}

        fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) {
            self.provenance.record(subject, model, params);
        }

        fn params(&self) -> &ParamSet {
            &self.params
        }
    }

    fn endpoint(node: u32, x_m: f64) -> RadioEndpoint {
        RadioEndpoint::isotropic(
            NodeId::new(node),
            v2xw_core::geom::Vec3::new(x_m, 0.0, 1.5),
            ActorClass::Car,
            0,
        )
    }

    #[test]
    fn the_card_validates() {
        // The cheapest guard there is against a card the registry would refuse in the
        // middle of somebody else's run.
        card(&LogDistanceParams::default())
            .validate()
            .expect("the card must validate");
    }

    #[test]
    fn the_card_registers() {
        let mut registry = Registry::new();
        let reference = register(&mut registry).expect("registration must succeed");
        assert_eq!(registry.get_ref(reference).unwrap().card.id, MODEL_ID);
    }

    #[test]
    fn the_reference_distance_is_the_friis_loss() {
        let p = LogDistanceParams::default();
        let at_d0 = p.path_loss_db(p.d0_m, F_HZ);
        let friis = friis_loss_db(p.d0_m, F_HZ);
        assert!(
            (at_d0 - friis).abs() < 1e-9,
            "the curve must be anchored to Friis at d0: {at_d0} vs {friis}"
        );
    }

    /// The assertion that distinguishes this model from free space.
    ///
    /// **Shown to fail:** setting `path_loss_exponent` to 2.0 makes the slope 20 dB per
    /// decade and this test goes red, which is how it was checked. A test that passes for
    /// both models would be measuring nothing.
    #[test]
    fn the_slope_is_ten_n_db_per_decade() {
        let p = LogDistanceParams::default();
        let a = p.path_loss_db(100.0, F_HZ);
        let b = p.path_loss_db(1000.0, F_HZ);
        let expected = 10.0 * p.path_loss_exponent;
        assert!(
            (b - a - expected).abs() < 1e-6,
            "expected {expected} dB per decade, got {}",
            b - a
        );
        // …and it is *not* the free-space slope, stated as its own assertion so the
        // failure message says which of the two claims broke.
        assert!(
            (b - a - 20.0).abs() > 1.0,
            "a 20 dB/decade slope means the exponent is not being applied"
        );
    }

    #[test]
    fn loss_never_decreases_with_distance() {
        // 03-interfaces.md §17's conformance property for a propagation model.
        let p = LogDistanceParams::default();
        let mut previous = f64::NEG_INFINITY;
        let mut d = 1.0;
        while d <= 2000.0 {
            let loss = p.path_loss_db(d, F_HZ);
            assert!(
                loss >= previous - 1e-12,
                "loss fell from {previous} to {loss} at d = {d} m"
            );
            previous = loss;
            d *= 1.2;
        }
    }

    #[test]
    fn two_links_do_not_share_a_shadowing_draw() {
        // The property the stream key buys: the same geometry on two different links
        // gives two different realisations, and neither depends on the other.
        let mut model = LogDistance::medium();
        let mut ctx = TestCtx::new(0x5EED);
        let los = LosResult::clear();
        let weather = WeatherState::CLEAR;

        let a = model
            .loss_db(
                &mut ctx,
                &endpoint(0, 0.0),
                &endpoint(1, 200.0),
                F_HZ,
                &los,
                &weather,
            )
            .shadow_db;
        let b = model
            .loss_db(
                &mut ctx,
                &endpoint(2, 0.0),
                &endpoint(3, 200.0),
                F_HZ,
                &los,
                &weather,
            )
            .shadow_db;
        assert_ne!(a, b, "two links drew the same shadowing value");
    }

    #[test]
    fn a_reversed_link_is_a_different_link() {
        // `LinkKey(a, b)` and `LinkKey(b, a)` are different keys, because the two
        // directions have different antennas and different environments. A model that
        // sorted the pair would silently make them one link.
        let mut model = LogDistance::medium();
        let mut ctx = TestCtx::new(7);
        let los = LosResult::clear();
        let weather = WeatherState::CLEAR;
        let forward = model
            .loss_db(
                &mut ctx,
                &endpoint(0, 0.0),
                &endpoint(1, 150.0),
                F_HZ,
                &los,
                &weather,
            )
            .shadow_db;
        let backward = model
            .loss_db(
                &mut ctx,
                &endpoint(1, 150.0),
                &endpoint(0, 0.0),
                F_HZ,
                &los,
                &weather,
            )
            .shadow_db;
        assert_ne!(forward, backward);
    }

    #[test]
    fn the_total_is_the_sum_of_the_terms_less_the_gain() {
        let mut model = LogDistance::new(
            Tier::Medium,
            LogDistanceParams {
                shadowing_sigma_db: 0.0,
                ..LogDistanceParams::default()
            },
        );
        let mut ctx = TestCtx::new(1);
        let breakdown = model.loss_db(
            &mut ctx,
            &endpoint(0, 0.0),
            &endpoint(1, 300.0),
            F_HZ,
            &LosResult::clear(),
            &WeatherState::CLEAR,
        );
        assert_eq!(breakdown.shadow_db, 0.0, "sigma 0 must draw nothing");
        let expected = breakdown.path_db - breakdown.antenna_db;
        assert!((breakdown.total_db - expected).abs() < 1e-9);
    }

    #[test]
    fn a_run_reproduces_from_the_seed_alone() {
        let los = LosResult::clear();
        let weather = WeatherState::CLEAR;
        let once = {
            let mut model = LogDistance::medium();
            let mut ctx = TestCtx::new(99);
            model
                .loss_db(
                    &mut ctx,
                    &endpoint(4, 0.0),
                    &endpoint(5, 250.0),
                    F_HZ,
                    &los,
                    &weather,
                )
                .total_db
        };
        let twice = {
            let mut model = LogDistance::medium();
            let mut ctx = TestCtx::new(99);
            model
                .loss_db(
                    &mut ctx,
                    &endpoint(4, 0.0),
                    &endpoint(5, 250.0),
                    F_HZ,
                    &los,
                    &weather,
                )
                .total_db
        };
        assert_eq!(once, twice);
    }
}
