//! [`NodeCtx`] — the narrowed engine context a node runtime is driven through.
//!
//! Build decision D12.2 settled the shape: a crate below `v2xw-engine` cannot name the
//! engine's concrete event type, so it declares the slice of
//! [`v2xw_core::ctx::Ctx`] it needs as its own dyn-compatible trait and the engine
//! supplies one blanket adapter. `v2xw-mobility` does it with `MobCtx`; this is the same
//! pattern for the node families.
//!
//! What the slice deliberately leaves out is the whole of the ground-truth surface:
//! there is no `world()` and no `actors()` here, and there is no way to get one from what
//! is here. That is not a convenience — it is invariant I-C2 applied one level up from
//! [`v2xw_core::nodeview::NodeView`]. The view stops a *plug-in* reading the truth; this
//! trait stops the *runtime that builds the view* reading it, which is where the leak
//! would otherwise be introduced, because the runtime is the only code with a legitimate
//! reason to hold both a node and a context at once.
//!
//! [`NodeCtx::now`] is the one true-time accessor, and it is here on purpose: a node's
//! clock model has to be driven by the real instant in order to produce a believed one
//! that differs from it. Nothing else in this crate reads it. The conformance sentinel in
//! [`crate::firewall`] is what keeps that true.

use v2xw_core::ctx::{Ctx, ErasedRecord, Record};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::SimTime;

/// Everything the node runtime may ask the engine for.
pub trait NodeCtx {
    /// The instant being dispatched, on the simulator's own clock.
    ///
    /// **Not** what the node believes the time is: that is
    /// [`v2xw_core::nodeview::NodeView::believed_time`], and it is what every message
    /// field and every freshness check uses. This one exists so that
    /// [`crate::clock::ClockModel`] has something to drift away from.
    fn now(&self) -> SimTime;

    /// The deterministic stream for `(domain, entity)` (ADR 0004 §3).
    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_>;

    /// Records an event on its declared channel.
    fn emit_erased(&mut self, record: &dyn ErasedRecord);
}

/// The typed spelling of [`NodeCtx::emit_erased`], blanket-implemented so that
/// [`NodeCtx`] stays dyn-compatible.
pub trait NodeCtxExt: NodeCtx {
    /// Records `record` on the channel its type declares.
    fn emit<R: Record>(&mut self, record: R) {
        self.emit_erased(&record);
    }
}

impl<T: NodeCtx + ?Sized> NodeCtxExt for T {}

/// A [`NodeCtx`] over borrowed parts, for the engine's node phase and for tests.
///
/// It collects emitted records rather than writing them, which is what lets a test assert
/// on what a node said without standing up a recorder.
#[derive(Debug)]
pub struct NodeRuntimeCtx<'a> {
    now: SimTime,
    rng: &'a RngRegistry,
    emitted: Vec<v2xw_core::ctx::OwnedRecord>,
}

impl<'a> NodeRuntimeCtx<'a> {
    /// A context for the step at `now`.
    pub fn new(now: SimTime, rng: &'a RngRegistry) -> Self {
        NodeRuntimeCtx {
            now,
            rng,
            emitted: Vec::new(),
        }
    }

    /// Moves the context to another instant, keeping the registry and the records.
    #[must_use]
    pub fn at(mut self, now: SimTime) -> Self {
        self.now = now;
        self
    }

    /// Sets the instant in place.
    pub fn set_now(&mut self, now: SimTime) {
        self.now = now;
    }

    /// What has been emitted so far, oldest first.
    pub fn emitted(&self) -> &[v2xw_core::ctx::OwnedRecord] {
        &self.emitted
    }

    /// Takes the emitted records, leaving the context empty.
    pub fn take_emitted(&mut self) -> Vec<v2xw_core::ctx::OwnedRecord> {
        core::mem::take(&mut self.emitted)
    }
}

impl NodeCtx for NodeRuntimeCtx<'_> {
    fn now(&self) -> SimTime {
        self.now
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.rng.checkout(domain, entity)
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        if let Ok(owned) = record.to_owned_record() {
            self.emitted.push(owned);
        }
    }
}

/// Adapts a real engine context to [`NodeCtx`].
///
/// The engine writes `runtime.step(&mut CoreCtx(ctx), …)`. Note what the adapter does
/// *not* forward: `world()` and `actors()` are not on [`NodeCtx`], so the adapter cannot
/// pass them through even though the thing it wraps has them.
#[derive(Debug)]
pub struct CoreCtx<'a, C: ?Sized>(
    /// The engine context being adapted.
    pub &'a mut C,
);

impl<C: Ctx + ?Sized> NodeCtx for CoreCtx<'_, C> {
    fn now(&self) -> SimTime {
        (*self.0).now()
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        (*self.0).rng(domain, entity)
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        (*self.0).emit_erased(record);
    }
}
