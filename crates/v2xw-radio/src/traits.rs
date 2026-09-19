//! The six plug-in seams of 03-interfaces.md §4: [`Propagation`], [`Fading`],
//! [`ObstacleModel`], [`Phy`], [`Mac`] and [`Dcc`].
//!
//! # Why every trait is generic over the context
//!
//! [`v2xw_core::ctx::Ctx`] carries three associated types — the world, the actor index
//! and the engine's event payload enum — so `&mut dyn Ctx` on its own does not name a
//! type. Each trait here therefore takes the context as a type parameter, exactly as
//! `v2xw_msg::generator::MessageGenerator` does. Once the engine fixes the three,
//! `dyn Phy<EngineCtx>` is an ordinary trait object, which is how in-process plug-ins are
//! called (ADR 0007 §8). Every implementation in this crate is written against `C: Ctx`
//! with no further bound, so a caller can supply any context: the engine's, or a test's.
//!
//! # Four deliberate divergences from the published signatures
//!
//! Each one is here because the published shape cannot be written yet, not because it is
//! inconvenient, and each is a one-line change if the engine decides otherwise.
//!
//! 1. **`begin_tx` returns the deadline instead of scheduling it.** 03-interfaces.md §4
//!    says `begin_tx` "schedules `end_tx`". A model cannot: `Ctx::schedule` takes a
//!    `Self::Payload`, and the concrete event enum lives in the engine crate (build
//!    decision D8), so no code here can construct one. [`Phy::begin_tx`] therefore
//!    computes the air time and returns a [`TxHandle`] carrying `end`, and the engine
//!    schedules the [`v2xw_core::event::EventClass::PhyEnd`] event for that instant. The
//!    message layer resolved the same problem the same way (a generator returns
//!    `GenRequest`s rather than scheduling them).
//! 2. **`begin_tx` returns a `Result`.** 04-models.md §4.6 requires it to reject a frame
//!    above the MSDU cap, which a signature returning `TxHandle` cannot express.
//! 3. **`Propagation::loss_db` takes `&mut self`.** The spatially correlated shadowing of
//!    04-models.md §3.2 is an AR(1) process with per-link state, and the TR 37.885 link
//!    state machine is re-evaluated every 100 ms; both mutate. `&self` would force
//!    interior mutability on a type the registry requires to be `Sync`, which is the
//!    shape ADR 0004 exists to keep out of the engine.
//! 4. **`Mac::cbr` takes the instant to measure at.** The channel busy ratio is busy time
//!    over the last `T_CBR` *ending somewhere*, and a MAC holds no clock. Without the
//!    parameter the only instant available is whenever the meter was last told something,
//!    so a node whose channel has fallen quiet reports the load it had when it was last
//!    busy and never decays. See [`Mac::cbr`].
//!
//! [`TxHandle`]: crate::types::TxHandle

use v2xw_core::card::Tier;
use v2xw_core::ctx::Ctx;
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{LinkKey, NodeId};
use v2xw_core::time::{Duration, SimTime};
use v2xw_core::weather::WeatherState;
use v2xw_world::model::{EnvClass, World};

use crate::types::{
    AccessCategory, ActorSet, CcaState, ChannelId, DccState, DropCause, FrameDescriptor,
    GateDecision, LosResult, LossBreakdown, MacSdu, Mcs, RadioEndpoint, Rat, ResourceModel,
    RxHandle, RxOutcome, TxGrant, TxHandle, TxRequest,
};

/// Large-scale propagation loss (03-interfaces.md §4, 04-models.md §3.1-§3.3).
pub trait Propagation<C: Ctx + ?Sized>: v2xw_core::model::Model {
    /// The fidelity tier this instance is configured for.
    fn tier(&self) -> Tier;

    /// The full loss breakdown for one link at one frequency.
    ///
    /// Deterministic given the context's RNG streams: every random term (the shadowing
    /// realisation, a link-state draw) comes from a stream keyed by
    /// `(RngDomain::Shadow, EntityRef::Link(..))`, so it does not depend on the order in
    /// which other models drew.
    fn loss_db(
        &mut self,
        ctx: &mut C,
        tx: &RadioEndpoint,
        rx: &RadioEndpoint,
        f_hz: f64,
        los: &LosResult,
        w: &WeatherState,
    ) -> LossBreakdown;

    /// The environment preset this point falls in, from the world's land use.
    ///
    /// Defaulted to the world's own answer ([`World::env_class_at`]), which is the
    /// land-use lookup 04-models.md §3.2 asks for; a model that classifies differently
    /// (a measurement-derived classifier, say) overrides it.
    fn environment(&self, world: &World, p: Vec3) -> EnvClass {
        world.env_class_at(p)
    }
}

/// Small-scale fading (03-interfaces.md §4, 04-models.md §3.4).
pub trait Fading<C: Ctx + ?Sized>: v2xw_core::model::Model {
    /// The fading **gain** in dB for one frame on one link: 0 dB for no fading, negative
    /// in a fade, positive in a constructive realisation.
    ///
    /// The sign convention is the one the link budget wants: the PHY *adds* this to the
    /// received power. A fading model that returned a loss would make `fading/none`
    /// return `-0.0` and every caller negate it.
    ///
    /// The draw is keyed by `(RngDomain::Fading, EntityRef::LinkFrame { link, frame })`
    /// where `frame` is `t`, the frame's start instant on that link in nanoseconds: two
    /// frames cannot start at the same nanosecond on one directed link, and the key
    /// therefore identifies the frame without a counter that reordering could disturb.
    /// `LinkFrame` is `is_single_use`, so the stream is derived, used and dropped rather
    /// than interned per frame per link (03-interfaces.md §1.1).
    fn sample_db(&mut self, ctx: &mut C, link: LinkKey, d_m: f64, t: SimTime) -> f64;
}

/// Line-of-sight geometry and the obstacle shadowing terms (03-interfaces.md §2,
/// 04-models.md §3.5).
pub trait ObstacleModel<C: Ctx + ?Sized>: v2xw_core::model::Model {
    /// The fidelity tier this instance is configured for.
    fn tier(&self) -> Tier;

    /// Classifies the link `a → b` and returns the obstruction geometry.
    ///
    /// Pure: same world, same endpoints, same actors, same answer, with no RNG. The
    /// random part of an obstacle model (the NLOSv log-normal draw) lives in
    /// [`ObstacleModel::obstacle_loss_db`].
    fn los(&self, world: &World, a: Vec3, b: Vec3, actors: Option<&ActorSet>) -> LosResult;

    /// The obstacle loss for a classified link, dB, non-negative.
    ///
    /// Takes the two endpoints rather than a bare distance, which 03-interfaces.md §4's
    /// `LossBreakdown` shape does not spell out: the TR 37.885 NLOSv model selects one of
    /// three cases by comparing the two **antenna heights** against the blocker's height
    /// (04-models.md §3.5), and the knife-edge models need the same geometry. The link
    /// key is `LinkKey(tx.node, rx.node)`, which is how the random draw is keyed.
    ///
    /// Defaulted to zero so that a model which only classifies (a pure LOS tester) does
    /// not have to implement it.
    fn obstacle_loss_db(
        &mut self,
        _ctx: &mut C,
        _tx: &RadioEndpoint,
        _rx: &RadioEndpoint,
        _los: &LosResult,
        _f_hz: f64,
    ) -> f64 {
        0.0
    }
}

/// The physical layer (03-interfaces.md §4, 04-models.md §4.2, §4.7).
pub trait Phy<C: Ctx + ?Sized>: v2xw_core::model::Model {
    /// The fidelity tier this instance is configured for.
    fn tier(&self) -> Tier;

    /// The radio access technology.
    fn rat(&self) -> Rat;

    /// Begins a transmission: computes the air time and returns the handle whose `end`
    /// the engine schedules a `PhyEnd` event for.
    ///
    /// # Errors
    ///
    /// [`crate::error::RadioError::FrameTooLarge`] when the frame exceeds the MSDU cap
    /// (04-models.md §4.6: the fragmenter must have acted first).
    fn begin_tx(
        &mut self,
        ctx: &mut C,
        tx: NodeId,
        f: &FrameDescriptor,
    ) -> crate::error::Result<TxHandle>;

    /// Exact air time for a frame, per 04-models.md §4.2.
    fn air_time(&self, bytes: u32, mcs: Mcs) -> Duration;

    /// The clear-channel assessment state of one channel at one node.
    fn cca(&self, ctx: &C, node: NodeId, ch: ChannelId) -> CcaState;

    /// Evaluates one arrival at the end of its reception and reports the outcome with at
    /// most one loss cause (invariant I-R3).
    fn finish_rx(&mut self, ctx: &mut C, rx: NodeId, h: RxHandle) -> RxOutcome;

    /// The noise floor of one channel at one node, dBm.
    fn noise_floor_dbm(&self, node: NodeId, ch: ChannelId) -> f64;
}

/// Medium access control (03-interfaces.md §4, 04-models.md §4.3, §4.5).
pub trait Mac<C: Ctx + ?Sized>: v2xw_core::model::Model {
    /// The fidelity tier this instance is configured for.
    fn tier(&self) -> Tier;

    /// Queues an SDU in one access category.
    ///
    /// # Errors
    ///
    /// [`DropCause`] when the frame is too large for the MSDU cap or the queue is full.
    fn enqueue(
        &mut self,
        ctx: &mut C,
        node: NodeId,
        sdu: MacSdu,
        ac: AccessCategory,
    ) -> core::result::Result<(), DropCause>;

    /// Tells the MAC that the medium changed state at this node.
    fn on_cca(&mut self, ctx: &mut C, node: NodeId, ch: ChannelId, state: CcaState);

    /// Tells the MAC that one of its transmissions finished.
    fn on_tx_done(&mut self, ctx: &mut C, node: NodeId, h: TxHandle);

    /// The channel busy ratio over the window ending at `now`, `0.0..=1.0`.
    ///
    /// `now` is a parameter because the measurement is "busy time in the last `T_CBR`",
    /// which is only defined relative to an instant, and the MAC holds no clock. The
    /// published signature has neither, and an implementation that ended the window at
    /// the last instant its own meter happened to know about reported the load of
    /// whenever the channel was last busy: a node that fell silent kept reporting
    /// `CBR = 1.0` for the rest of the run, and every [`Dcc`] model consuming it through
    /// [`Dcc::on_cbr`] pinned that node at `δ_min` — one frame per second — for ever.
    /// This is the fourth divergence from 03-interfaces.md §4, for the same reason as the
    /// other three: the published shape cannot express what the model needs.
    fn cbr(&self, node: NodeId, ch: ChannelId, now: SimTime) -> f64;

    /// How this MAC shares the medium.
    fn resource_model(&self) -> ResourceModel;

    /// When this node's access state machine next has something to do, if anything.
    ///
    /// The instant the earliest eligible frame's backoff expires — `resume_at +
    /// remaining × aSlotTime` for EDCA, `enqueued_at + slot × aSlotTime` for the slotted
    /// abstraction — or `None` when nothing is queued or every countdown is frozen on a
    /// busy medium. The engine schedules its `MacTimer` for this instant, so the
    /// transmission happens at the slot boundary the backoff computed rather than at
    /// whatever cadence the engine happens to poll on.
    ///
    /// Without it the poll cadence sets the collision structure instead of the 13 µs
    /// slot: two nodes that drew *different* slots are both past due at a coarse poll and
    /// are both granted in the same instant, which erases the medium tier's only
    /// collision mechanism. An instant in the past means the frame is already overdue,
    /// and [`TxGrant::at`] then reports when it should have gone.
    ///
    /// Defaulted to `None` — "poll me on your own cadence" — so a MAC with no timing of
    /// its own does not have to implement it.
    fn next_poll_at(&self, _node: NodeId, _ch: ChannelId) -> Option<SimTime> {
        None
    }

    /// Advances this node's access state machine to `ctx.now()` and returns the frame it
    /// may transmit, if any.
    ///
    /// This is the one method 03-interfaces.md §4 does not list, and it is here for the
    /// same reason [`Phy::begin_tx`] returns a deadline: the published shape has the MAC
    /// schedule its own `MacTimer` events, which needs the engine's payload enum. The
    /// engine calls `poll` from the `MacTimer` handler it owns; a test calls it directly,
    /// which is what makes the EDCA state machine testable without a kernel.
    fn poll(&mut self, ctx: &mut C, node: NodeId, ch: ChannelId) -> Option<TxGrant>;
}

/// Decentralised congestion control (03-interfaces.md §4, 04-models.md §6).
pub trait Dcc<C: Ctx + ?Sized>: v2xw_core::model::Model {
    /// Feeds one CBR measurement in, once per `T_CBR`.
    fn on_cbr(&mut self, ctx: &mut C, node: NodeId, cbr: f64);

    /// Decides whether this node may transmit this frame now, later, or not at all.
    fn gate(&mut self, ctx: &mut C, node: NodeId, req: &TxRequest) -> GateDecision;

    /// The state the HUD and the metrics export.
    fn state(&self, node: NodeId) -> DccState;
}
