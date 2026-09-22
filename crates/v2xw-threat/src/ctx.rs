//! [`ThreatCtx`] — the narrow slice of [`v2xw_core::ctx::Ctx`] this crate needs, and the
//! blanket implementation that makes every engine context one.
//!
//! # Why a second trait
//!
//! [`v2xw_core::ctx::Ctx`] has three associated types — `Payload`, `World`, `Actors` — so
//! a trait object over it must name all three, and naming `World` and `Actors` is exactly
//! what invariants I-T1 and I-T2 forbid the models in this crate from doing. Writing
//! `&mut dyn Ctx<World = …>` in an attacker's signature would put the ground truth one
//! method call away and make the firewall a matter of discipline.
//!
//! `ThreatCtx` keeps the three capabilities a node-resident plug-in legitimately has —
//! know what time its *host* thinks it is for scheduling, draw from its own deterministic
//! stream, emit a record — and drops the rest. The blanket implementation below means the
//! engine passes its own `Ctx` unchanged and nothing has to be adapted; there is simply no
//! way, from inside this crate, to get at `world()` or `actors()`.
//!
//! Note what `now()` is *not* used for. It is the host's scheduling clock, and this crate
//! uses it for exactly one thing: the timestamp on a record, which the recorder needs in
//! simulator time to be joinable with every other channel. Every *decision* — a freshness
//! check, a detector's interval, an attacker's duty cycle — reads the node's believed
//! time from [`crate::obs::SelfBelief::believed_time`] or
//! [`crate::attack::AttackerView::believed_time`] instead, which is what makes a clock
//! attack visible at all.

use v2xw_core::ctx::{Ctx, ErasedRecord, Record};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard};
use v2xw_core::time::SimTime;

/// What a threat-model plug-in may do: read the host clock for a timestamp, draw from a
/// keyed deterministic stream, and emit a record.
pub trait ThreatCtx {
    /// The host's current simulated instant, for timestamping records only.
    fn now(&self) -> SimTime;

    /// A deterministic stream keyed by `(domain, entity)` (ADR 0004 §3).
    ///
    /// Every random draw in this crate comes from here. There is no other generator, no
    /// thread-local and no wall clock, so an attacker's falsification magnitudes and a
    /// report's sampling coin are reproducible from the seed alone.
    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_>;

    /// Emits a record on its channel.
    fn emit_erased(&mut self, record: &dyn ErasedRecord);
}

/// Sugar for [`ThreatCtx::emit_erased`] with a concrete record type.
pub trait ThreatCtxExt: ThreatCtx {
    /// Emits `record` on [`Record::CHANNEL`].
    fn emit<R: Record>(&mut self, record: R) {
        self.emit_erased(&record);
    }
}

impl<T: ThreatCtx + ?Sized> ThreatCtxExt for T {}

impl<C: Ctx> ThreatCtx for C {
    fn now(&self) -> SimTime {
        Ctx::now(self)
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        Ctx::rng(self, domain, entity)
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        Ctx::emit_erased(self, record);
    }
}

/// A [`ThreatCtx`] that keeps its records in a vector: the test host, and the shape an
/// offline replay or an evaluation harness wants.
///
/// It owns an [`v2xw_core::rng::RngRegistry`], so a test draws from the same keyed streams
/// the engine would and asserts on the same numbers.
#[derive(Debug)]
pub struct CollectingCtx {
    now: SimTime,
    rng: v2xw_core::rng::RngRegistry,
    records: Vec<v2xw_core::ctx::OwnedRecord>,
}

impl CollectingCtx {
    /// A context at instant zero with the given master seed.
    #[must_use]
    pub fn new(master_seed: u64) -> Self {
        Self {
            now: 0,
            rng: v2xw_core::rng::RngRegistry::new(master_seed),
            records: Vec::new(),
        }
    }

    /// Moves the host clock to `t`.
    pub fn set_now(&mut self, t: SimTime) {
        self.now = t;
    }

    /// Everything emitted so far, in emission order.
    #[must_use]
    pub fn records(&self) -> &[v2xw_core::ctx::OwnedRecord] {
        &self.records
    }

    /// The records emitted on one channel, in emission order.
    #[must_use]
    pub fn on_channel(&self, channel: &str) -> Vec<&v2xw_core::ctx::OwnedRecord> {
        self.records
            .iter()
            .filter(|r| r.channel_name().as_str() == channel)
            .collect()
    }

    /// Drops every record collected so far.
    pub fn clear(&mut self) {
        self.records.clear();
    }
}

impl ThreatCtx for CollectingCtx {
    fn now(&self) -> SimTime {
        self.now
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.rng.checkout(domain, entity)
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        if let Ok(owned) = record.to_owned_record() {
            self.records.push(owned);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::DetObservation;

    #[test]
    fn the_collecting_context_keeps_records_by_channel() {
        let mut ctx = CollectingCtx::new(7);
        ctx.set_now(1_500_000_000);
        ctx.emit(DetObservation {
            t: ctx.now(),
            node: v2xw_core::ids::NodeId::new(3),
            detector: "positionJump".to_string(),
            subject: "aabb".to_string(),
            score: Some(1.25),
        });
        assert_eq!(ctx.on_channel("det.observation").len(), 1);
        assert_eq!(ctx.on_channel("ma.report").len(), 0);
    }

    #[test]
    fn two_contexts_with_one_seed_draw_the_same_numbers() {
        let a = CollectingCtx::new(42);
        let b = CollectingCtx::new(42);
        let ea = EntityRef::Node(v2xw_core::ids::NodeId::new(1));
        let draw = |c: &CollectingCtx| {
            let mut g = c.rng(RngDomain::Attack, ea);
            g.f64()
        };
        let x: Vec<f64> = (0..4).map(|_| draw(&a)).collect();
        let y: Vec<f64> = (0..4).map(|_| draw(&b)).collect();
        assert_eq!(x, y);
    }
}
