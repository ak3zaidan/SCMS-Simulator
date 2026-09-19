//! A minimal [`Ctx`] for this crate's tests and for its integration tests.
//!
//! Public on purpose, and the reason is narrow: the headline acceptance test for
//! invariant I-S1 lives in `tests/equivalence.rs`, an integration test, which cannot see
//! a `#[cfg(test)]` item. The alternatives were to duplicate a context stub in every test
//! file, or to move the I-S1 test into a unit module where a reader would not find it.
//! Neither is better than a documented test double.
//!
//! It is a *stub*, not a simulation: no world, no actors, and a scheduler that records
//! what it was handed. Nothing in this crate needs any of those — a crypto backend needs
//! `now()` and `rng()`, and an envelope needs `now()` — which is itself worth knowing,
//! because it means the whole security layer is testable without an engine.

use v2xw_core::ctx::{Ctx, ErasedRecord, Visibility};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::ids::ActorId;
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::SimTime;

/// A context stub: a clock, the deterministic RNG streams, and a record of what was
/// emitted.
#[derive(Debug)]
pub struct TestCtx {
    now: SimTime,
    scheduler: Scheduler<u64>,
    rng: RngRegistry,
    provenance: ProvenanceLog,
    params: ParamSet,
    world: (),
    actors: Vec<ActorId>,
    /// Every record emitted through this context, as `(channel, visibility, json)`.
    pub emitted: Vec<(&'static str, Visibility, String)>,
}

impl TestCtx {
    /// A context at simulated time zero, with `master_seed`.
    pub fn new(master_seed: u64) -> TestCtx {
        TestCtx {
            now: 0,
            scheduler: Scheduler::new(),
            rng: RngRegistry::new(master_seed),
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            world: (),
            actors: Vec::new(),
            emitted: Vec::new(),
        }
    }

    /// Moves the clock to `t`.
    pub fn set_now(&mut self, t: SimTime) {
        self.now = t;
    }

    /// The master seed the RNG streams derive from.
    pub fn master_seed(&self) -> u64 {
        self.rng.master_seed()
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

    fn schedule(&mut self, at: SimTime, class: EventClass, payload: Self::Payload) -> EventHandle {
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

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        let mut bytes = Vec::new();
        record.write_json(&mut bytes).expect("record serialises");
        self.emitted.push((
            record.channel(),
            record.visibility(),
            String::from_utf8(bytes).expect("json is utf-8"),
        ));
    }

    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) {
        self.provenance.record(subject, model, params);
    }

    fn params(&self) -> &ParamSet {
        &self.params
    }
}
