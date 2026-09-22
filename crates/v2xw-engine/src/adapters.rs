//! One context adapter per family (build decision D12.2).
//!
//! # The problem D12.2 solved
//!
//! [`v2xw_core::ctx::Ctx`] has three associated types, and one of them — `Payload` — is
//! the engine's concrete [`Event`](crate::Event), which lives *here*, above every model
//! crate. A model crate therefore cannot spell `dyn Ctx<…>`: naming the payload would
//! invert the dependency the whole layering exists to keep straight.
//!
//! Two answers were tried in the Phase 1 build and D12.2 arbitrated between them:
//!
//! | Approach | Crate | Dyn-compatible? | Verdict |
//! |---|---|---|---|
//! | A narrowed per-family context trait plus an adapter | `v2xw-mobility` (`MobCtx`), `v2xw-node` (`NodeCtx`) | yes | **chosen** |
//! | Generic over `C: Ctx + ?Sized` | `v2xw-sec`, `v2xw-radio` | only while no method is generic over the context | should converge |
//!
//! The narrowed trait wins because ADR 0007 §8 requires in-process plug-ins to be trait
//! objects, and the generic form loses dyn-compatibility the moment any method takes a
//! second generic parameter. It also states, in the trait itself, exactly what a family
//! may touch — which is how `v2xw-node`'s [`NodeCtx`](v2xw_node::NodeCtx) manages to have
//! no `world()` at all, making invariant I-C2 a property of the signature rather than of a
//! reviewer's attention.
//!
//! # What this module is
//!
//! The engine's half of the bargain: one adapter per family, so no phase driver writes
//! `v2xw_mobility::CoreCtx(&mut ctx)` by hand and no family is adapted two different ways
//! in two different phases. Each function here is one line; the value is that there is
//! exactly one of them per family and that this table is where a reader finds out which
//! families are narrowed and which are still generic.
//!
//! | Family | How it takes a context | Adapter |
//! |---|---|---|
//! | Mobility, car-following, lane change, routing, demand, VRU, GNSS, clock | `&mut dyn MobCtx` | [`mobility`] |
//! | Node runtime, verification policy, generator | `&mut dyn NodeCtx` | [`node`] |
//! | Propagation, fading, obstacle, PHY, MAC, DCC | `C: Ctx + ?Sized` | [`radio`] |
//! | Crypto backend, security envelope | `C: Ctx + ?Sized` | [`security`] |
//!
//! The last two rows are the convergence D12.2 asks for and this crate does not own; see
//! [`SEC_CONVERGENCE`] for what the change costs, measured against the code rather than
//! estimated.

use crate::ctx::EngineCtx;

/// The mobility family's view of the engine context.
pub type MobilityAdapter<'a, 'e> = v2xw_mobility::CoreCtx<'a, EngineCtx<'e>>;

/// The node family's view of the engine context.
pub type NodeAdapter<'a, 'e> = v2xw_node::CoreCtx<'a, EngineCtx<'e>>;

/// Adapts the engine context for a mobility-family call.
///
/// The narrowing is real: [`v2xw_mobility::MobCtx`] exposes `now`, `rng` and `world`, so a
/// car-following model cannot schedule an event or emit a record even though the context
/// underneath can do both. Mobility is a periodic phase (ADR 0004 decision 2) and its
/// records are emitted by the phase driver, so it needs neither.
pub fn mobility<'a, 'e>(ctx: &'a mut EngineCtx<'e>) -> MobilityAdapter<'a, 'e> {
    v2xw_mobility::CoreCtx(ctx)
}

/// Adapts the engine context for a node-family call.
///
/// The narrowing is the strongest one in the system: [`v2xw_node::NodeCtx`] has `now`,
/// `rng` and `emit_erased` and **no** `world` or `actors`, so the node runtime — the one
/// piece of code with a legitimate reason to hold a node and a context at once — cannot
/// reach ground truth at all. That is invariant I-C2 one level above
/// [`v2xw_core::nodeview::NodeView`], and the adapter cannot forward what the trait does
/// not declare.
pub fn node<'a, 'e>(ctx: &'a mut EngineCtx<'e>) -> NodeAdapter<'a, 'e> {
    v2xw_node::CoreCtx(ctx)
}

/// The radio family takes the context generically, so the adapter is the identity.
///
/// It exists so that a phase driver names the family it is calling into, and so that this
/// module's table has a row for every family rather than a gap a reader has to interpret.
/// When `v2xw-radio` converges on a narrowed `RadioCtx`, this is the one place that
/// changes.
pub fn radio<'a, 'e>(ctx: &'a mut EngineCtx<'e>) -> &'a mut EngineCtx<'e> {
    ctx
}

/// The security family takes the context generically; see [`SEC_CONVERGENCE`].
pub fn security<'a, 'e>(ctx: &'a mut EngineCtx<'e>) -> &'a mut EngineCtx<'e> {
    ctx
}

/// What converging `v2xw-sec` on a narrowed context trait would cost, measured.
///
/// D12.2 says `v2xw-sec` "should converge" and leaves the size of that change open. The
/// measurement, taken against the crate as it stands:
///
/// * `CryptoBackend<C: Ctx + ?Sized>` and `SecurityEnvelope<C: Ctx + ?Sized>` are the only
///   two generic traits.
/// * Between them they touch **two** context methods: `ctx.rng(RngDomain::Crypto,
///   EntityRef::Node(owner))` in `crypto::draw_seed`, and `ctx.now()` in
///   `<Envelope as SecurityEnvelope<C>>::sign`. Nothing reads `world`, `actors`,
///   `schedule`, `cancel`, `emit` or `params`.
/// * So the narrowed trait is `trait SecCtx { fn now(&self) -> SimTime; fn rng(&self,
///   domain: RngDomain, entity: EntityRef) -> RngGuard<'_>; }` — a strict subset of
///   [`v2xw_node::NodeCtx`], and a blanket `impl<C: Ctx + ?Sized> SecCtx for CoreCtx<'_, C>`
///   is the same five lines `v2xw-mobility` and `v2xw-node` already carry.
///
/// **Verdict: a small change, not its own pass.** It is a mechanical substitution of
/// `&mut dyn SecCtx` for `ctx: &mut C` across two trait definitions, their two
/// implementations (`Modeled`, `Real`, `Envelope`) and the crate's `TestCtx`, which
/// already implements the full `Ctx` and would keep doing so. The one thing to watch is
/// that `SecurityEnvelope::sign` takes `crypto: &mut dyn CryptoBackend<C>`, so the two
/// traits have to be converted together or the bound does not type-check halfway.
///
/// It is not done here because this crate does not own `crates/v2xw-sec`, and because the
/// generic form is *correct today*: `EngineCtx` satisfies `C: Ctx`, both traits are still
/// dyn-compatible as written (no method is generic over anything but the trait's own
/// parameter), and the conformance test in this module proves it by building the trait
/// objects.
pub const SEC_CONVERGENCE: &str = "small: two traits, two context methods (now, rng), one blanket impl; convert \
     CryptoBackend and SecurityEnvelope together because sign() names the other's bound";

/// A [`v2xw_radio::Propagation`] the engine can **store**.
///
/// Worth spelling out, because it is the strongest argument in the D12.2 convergence and
/// it is not the dyn-compatibility one. `Propagation<C>` is generic over the context, and
/// the engine's context is [`EngineCtx<'a>`] — a type with a lifetime. So
/// `Box<dyn Propagation<EngineCtx<'?>>>` has no lifetime to write: `'static` is wrong
/// (the engine never has a `&mut EngineCtx<'static>`), and a borrowed lifetime cannot
/// outlive the phase that created the context. A scenario-selected propagation model
/// therefore **cannot be a boxed trait object at all** while the family is generic over
/// the context.
///
/// This trait is the escape: it fixes the context at the engine's own type with a
/// higher-ranked blanket implementation, so any model that is `Propagation` over *every*
/// `EngineCtx` lifetime becomes one storable trait object. A narrowed `RadioCtx`, as
/// `v2xw-mobility` and `v2xw-node` already have, would delete this trait and its
/// implementation outright.
pub trait BoxedPropagation {
    /// The tier this instance is configured for.
    fn tier(&self) -> v2xw_core::card::Tier;

    /// The loss breakdown for one link.
    fn loss_db(
        &mut self,
        ctx: &mut EngineCtx<'_>,
        tx: &v2xw_radio::RadioEndpoint,
        rx: &v2xw_radio::RadioEndpoint,
        f_hz: f64,
        los: &v2xw_radio::LosResult,
        weather: &v2xw_core::weather::WeatherState,
    ) -> v2xw_radio::LossBreakdown;

    /// The model's card, for the registry and the manifest.
    fn card(&self) -> &v2xw_core::card::ModelCard;
}

impl<P> BoxedPropagation for P
where
    P: for<'a> v2xw_radio::Propagation<EngineCtx<'a>>,
{
    fn tier(&self) -> v2xw_core::card::Tier {
        v2xw_radio::Propagation::<EngineCtx<'_>>::tier(self)
    }

    fn loss_db(
        &mut self,
        ctx: &mut EngineCtx<'_>,
        tx: &v2xw_radio::RadioEndpoint,
        rx: &v2xw_radio::RadioEndpoint,
        f_hz: f64,
        los: &v2xw_radio::LosResult,
        weather: &v2xw_core::weather::WeatherState,
    ) -> v2xw_radio::LossBreakdown {
        v2xw_radio::Propagation::loss_db(self, ctx, tx, rx, f_hz, los, weather)
    }

    fn card(&self) -> &v2xw_core::card::ModelCard {
        v2xw_core::model::Model::card(self)
    }
}

/// A [`v2xw_radio::Fading`] the engine can store; see [`BoxedPropagation`] for why the
/// generic form cannot be boxed.
pub trait BoxedFading {
    /// The fading gain in dB for one frame on one link.
    fn sample_db(
        &mut self,
        ctx: &mut EngineCtx<'_>,
        link: v2xw_core::ids::LinkKey,
        d_m: f64,
        t: v2xw_core::time::SimTime,
    ) -> f64;

    /// The model's card.
    fn card(&self) -> &v2xw_core::card::ModelCard;
}

impl<F> BoxedFading for F
where
    F: for<'a> v2xw_radio::Fading<EngineCtx<'a>>,
{
    fn sample_db(
        &mut self,
        ctx: &mut EngineCtx<'_>,
        link: v2xw_core::ids::LinkKey,
        d_m: f64,
        t: v2xw_core::time::SimTime,
    ) -> f64 {
        v2xw_radio::Fading::sample_db(self, ctx, link, d_m, t)
    }

    fn card(&self) -> &v2xw_core::card::ModelCard {
        v2xw_core::model::Model::card(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ctx::Ctx;
    use v2xw_core::event::Scheduler;
    use v2xw_core::provenance::ProvenanceLog;
    use v2xw_core::registry::ParamSet;
    use v2xw_core::rng::RngRegistry;
    use v2xw_mobility::{ActorSnapshot, MobCtx};
    use v2xw_node::NodeCtx;

    use crate::ctx::MemoryRecorder;

    /// Builds a context over throwaway state.
    fn with_ctx<R>(f: impl FnOnce(&mut EngineCtx<'_>) -> R) -> R {
        let mut scheduler: Scheduler<crate::Event> = Scheduler::new();
        let rng = RngRegistry::new(7);
        let world = v2xw_world::procedural::grid(
            &v2xw_world::procedural::GridParams::legacy().with_size(2, 2),
            &v2xw_world::ImportOptions::default().imported_at("2026-09-22"),
        )
        .expect("grid world");
        let actors = ActorSnapshot::new(0, 1000.0);
        let mut provenance = ProvenanceLog::new();
        let params = ParamSet::new();
        let mut recorder = MemoryRecorder::new();
        let mut ctx = EngineCtx::new(
            &mut scheduler,
            &rng,
            &world,
            &actors,
            &mut provenance,
            &params,
            &mut recorder,
        );
        f(&mut ctx)
    }

    /// The mobility adapter is a `MobCtx` and forwards the three methods that trait has.
    #[test]
    fn the_mobility_adapter_is_a_mob_ctx() {
        with_ctx(|ctx| {
            let world_lanes = ctx.world().roads.lanes().len();
            let mut adapted = mobility(ctx);
            let dynamic: &mut dyn MobCtx = &mut adapted;
            assert_eq!(dynamic.now(), 0);
            assert_eq!(dynamic.world().roads.lanes().len(), world_lanes);
        });
    }

    /// The node adapter is a `NodeCtx`, and it is a trait object — which is the property
    /// ADR 0007 §8 needs and the generic form would lose.
    #[test]
    fn the_node_adapter_is_a_dyn_node_ctx() {
        with_ctx(|ctx| {
            let mut adapted = node(ctx);
            let dynamic: &mut dyn NodeCtx = &mut adapted;
            assert_eq!(dynamic.now(), 0);
        });
    }

    /// The generic families accept the engine context as a trait object too, which is
    /// what `SEC_CONVERGENCE` claims and what makes the convergence a cleanup rather than
    /// a repair.
    #[test]
    fn the_generic_families_are_still_dyn_compatible_over_the_engine_context() {
        with_ctx(|ctx| {
            let mut modeled = v2xw_sec::crypto::Modeled::new();
            let backend: &mut dyn v2xw_sec::CryptoBackend<EngineCtx<'_>> = &mut modeled;
            assert!(!backend.backend_id().is_empty());

            let mut prop = v2xw_radio::FreeSpace::new(v2xw_core::card::Tier::Medium);
            let propagation: &mut dyn v2xw_radio::Propagation<EngineCtx<'_>> = &mut prop;
            assert_eq!(propagation.tier(), v2xw_core::card::Tier::Medium);

            let _ = radio(ctx);
            let _ = security(ctx);
        });
    }
}
