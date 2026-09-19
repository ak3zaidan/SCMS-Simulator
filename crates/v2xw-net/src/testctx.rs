//! A minimal [`Ctx`] for this crate's own tests.
//!
//! The family traits here take the context as a type parameter (see
//! [`crate::netlayer::NetLayer`]), so a test that calls one through the trait has to name a
//! concrete context type. This is that type: a clock the test sets, a real
//! [`Scheduler`] and [`RngRegistry`] so the shapes are the engine's, and a list of the
//! records the model emitted so a test can assert on them.
//!
//! It is `#[cfg(test)]`: the engine's own context is `v2xw-core`'s contract, and a test
//! double has no business in the published API.

use v2xw_core::ctx::{Ctx, ErasedRecord, Visibility};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::SimTime;

/// A test double for the engine context.
pub struct TestCtx {
    now: SimTime,
    scheduler: Scheduler<&'static str>,
    rng: RngRegistry,
    provenance: ProvenanceLog,
    params: ParamSet,
    world: (),
    actors: (),
    /// Every record the code under test emitted: channel, visibility, JSON.
    pub records: Vec<(&'static str, Visibility, String)>,
}

impl TestCtx {
    /// A context at `t = 0` with an empty parameter set.
    pub fn new() -> Self {
        Self {
            now: 0,
            scheduler: Scheduler::new(),
            rng: RngRegistry::new(0xC0FFEE),
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            world: (),
            actors: (),
            records: Vec::new(),
        }
    }

    /// Moves the clock to `t`.
    pub fn set_now(&mut self, t: SimTime) {
        self.now = t;
    }

    /// The JSON of every record emitted on `channel`, in emission order.
    pub fn records_on(&self, channel: &str) -> Vec<&str> {
        self.records
            .iter()
            .filter(|(c, _, _)| *c == channel)
            .map(|(_, _, j)| j.as_str())
            .collect()
    }
}

impl Ctx for TestCtx {
    type World = ();
    type Actors = ();
    type Payload = &'static str;

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
        self.records.push((
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
