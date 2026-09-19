//! The engine context slice mobility models are handed.
//!
//! 03-interfaces.md §3 writes every mobility trait method as taking `&mut dyn Ctx`, where
//! [`v2xw_core::ctx::Ctx`] is the full plug-in context. That trait has three associated
//! types — `World`, `Actors` and `Payload` — and the last of them is the *kernel's* event
//! payload enum, which lives in a crate that does not exist yet (ADR 0010 puts the event
//! loop in `v2xw-server`). `dyn Ctx` therefore cannot be spelled from here without
//! inventing a payload type that the kernel would later have to adopt.
//!
//! So this crate takes the same route `v2xw-world` took with the `Model` supertrait: it
//! declares the *slice* of the context mobility actually uses — the instant, the
//! deterministic RNG streams and the world — as its own dyn-compatible trait, and supplies
//! an adapter ([`CoreCtx`]) that turns any real [`v2xw_core::ctx::Ctx`] over a
//! [`World`] into one. When the kernel crate lands, the migration is to replace the trait
//! bound in the signatures with `dyn Ctx<World = World, Actors = …, Payload = …>` and
//! delete the adapter; no model body changes, because no model body touches anything else.
//!
//! What is deliberately *not* here: `schedule`, `cancel`, `emit`, `why` and `params`.
//! Mobility is a periodic phase (ADR 0004 decision 2), so it schedules nothing of its own;
//! its records are emitted by the phase driver that owns the recorder; and every number a
//! model reads comes from its own typed parameter struct, which is what its model card
//! declares (invariant I-C3).

use v2xw_core::ctx::Ctx;
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::SimTime;
use v2xw_world::World;

/// The part of the engine context a mobility model may read.
///
/// Dyn-compatible on purpose: models are trait objects (ADR 0007 §8), so every method
/// takes `&self` or `&mut self` and returns a borrow or a `Copy` scalar.
pub trait MobCtx {
    /// The instant being dispatched — the start of the mobility step.
    fn now(&self) -> SimTime;

    /// The deterministic stream for `(domain, entity)` (ADR 0004 §3).
    ///
    /// A model never owns an RNG and never keeps a stream across calls: it asks for the
    /// stream of the entity it is acting for, draws, and drops the guard. That is what
    /// makes a model's draws independent of the order in which other models draw.
    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_>;

    /// The world: lanes, junctions, connections, signal plans, spatial indices.
    fn world(&self) -> &World;
}

/// A [`MobCtx`] over borrowed parts, for the phase driver and for tests.
///
/// The engine's mobility phase builds one of these per step. It holds no state of its own,
/// so two runs that hand it the same world and the same registry see the same context.
#[derive(Debug)]
pub struct MobilityCtx<'a> {
    now: SimTime,
    world: &'a World,
    rng: &'a RngRegistry,
}

impl<'a> MobilityCtx<'a> {
    /// A context for the step at `now`.
    pub fn new(now: SimTime, world: &'a World, rng: &'a RngRegistry) -> Self {
        Self { now, world, rng }
    }

    /// Moves the context to another instant, keeping the world and the registry.
    #[must_use]
    pub fn at(mut self, now: SimTime) -> Self {
        self.now = now;
        self
    }

    /// The registry this context draws from.
    pub fn registry(&self) -> &'a RngRegistry {
        self.rng
    }
}

impl MobCtx for MobilityCtx<'_> {
    fn now(&self) -> SimTime {
        self.now
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.rng.checkout(domain, entity)
    }

    fn world(&self) -> &World {
        self.world
    }
}

/// Adapts a real engine context ([`v2xw_core::ctx::Ctx`] over a [`World`]) to [`MobCtx`].
///
/// The engine writes `mobility.step(&mut CoreCtx(ctx), dt)`; nothing else is needed, and
/// the adapter disappears when the kernel's payload type is settled (see the module
/// documentation).
#[derive(Debug)]
pub struct CoreCtx<'a, C: ?Sized>(
    /// The engine context being adapted.
    pub &'a mut C,
);

impl<C: Ctx<World = World> + ?Sized> MobCtx for CoreCtx<'_, C> {
    fn now(&self) -> SimTime {
        (*self.0).now()
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        (*self.0).rng(domain, entity)
    }

    fn world(&self) -> &World {
        (*self.0).world()
    }
}
