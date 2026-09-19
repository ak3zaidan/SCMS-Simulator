//! The engine context handed to every plug-in call, and the records a plug-in emits.
//!
//! [`Ctx`] is 03-interfaces.md §1.1 in code. It is the only thing a model sees of the
//! engine: the clock, its RNG streams, the event heap, the world and actor index, the
//! recorder, the provenance log and its own resolved parameters. Every family trait in
//! `v2xw-world`, `-mobility`, `-radio`, `-net`, `-msg`, `-node`, `-proto`, `-threat` and
//! `-metrics` is declared against it, so they all share one context instead of each crate
//! inventing an incompatible one. The kernel in this crate owns the pieces it is built
//! from ([`Scheduler`], [`RngRegistry`], [`ProvenanceLog`], [`ParamSet`]); the world and
//! the actor index live in `v2xw-world`, which is why they are associated types rather
//! than concrete ones.
//!
//! # Shape
//!
//! ```text
//! &self      now, rng, world, actors, params      — readable while anything else is borrowed
//! &mut self  schedule, cancel, emit, why          — mutate the kernel
//! ```
//!
//! [`Ctx::rng`] takes `&self` and returns an [`RngGuard`] rather than taking `&mut self`
//! and returning `&mut RngStream`. That is not a detail: `&mut self` would make
//!
//! ```text
//! let noise = ctx.rng(Mobility, actor).normal(0.0, sigma);   // &mut borrow ends here
//! let leader = ctx.world().leader_of(actor);                 // ok
//! ctx.schedule(t, MobilityStep, payload);                    // ok
//! ```
//!
//! fine but
//!
//! ```text
//! let d = ctx.world().lane(l).length_m;       // &self borrow, still live below
//! let jitter = ctx.rng(Mobility, actor).f64() * d;   // &mut self — will not compile
//! ```
//!
//! fail, which is the first thing real model code does; and it would make the whole
//! context unusable inside the phase-parallel maps of 02-architecture.md §6.4, where the
//! context is shared by reference across `rayon` tasks. With `&self` both compile, and
//! the borrow checker still stops a guard being held across a `&mut self` call — which is
//! what we want, because that call may schedule an event and the guard must be back in
//! the registry before the next task asks for it.
//!
//! # Dyn-compatible on purpose
//!
//! In-process Rust plug-ins are trait objects (ADR 0007 §8), so a family trait method
//! takes `&mut dyn Ctx<World = …, Actors = …, Payload = …>`. That requires [`Ctx`] itself
//! to be dyn-compatible, which is why the generic `emit<R: Record>` of 03-interfaces.md
//! §1.1 is split: [`Ctx::emit_erased`] is the object-safe primitive an engine implements,
//! and [`CtxExt::emit`] is the generic convenience every caller uses. `CtxExt` is
//! blanket-implemented, including for `dyn Ctx`, so the ergonomics are the published ones.

use serde::Serialize;

use crate::error::Result;
use crate::event::{EventClass, EventHandle};
use crate::provenance::ProvSubject;
use crate::registry::{ModelRef, ParamSet, ParamSetId};
use crate::rng::{EntityRef, RngDomain, RngGuard};
use crate::time::{Duration, SimTime};

/// Who may see a recorded value (03-interfaces.md §14).
///
/// The recorder refuses a ground-truth record on a node channel, and the `NODE-only`
/// replay profile strips the GT-tagged parts of a mixed record. The tag travels with the
/// record rather than with the channel so a single record type can be honest about
/// carrying both (`phy.rx` carries the receiver's measurements *and* the transmitter's
/// identity, which is ground truth).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Visibility {
    /// Ground truth: the simulator's own state, which no node could know
    /// (`gt.kinematics`, `gt.attack.action`, `gt.spawn`).
    Gt,
    /// What a node itself observed or did (`node.tx`, `mac.cbr`, `det.observation`).
    /// This is the only class a detector, a dataset exporter or an ML feature may read.
    Node,
    /// Node-visible fields plus ground-truth fields in one record (`phy.rx`). Exporters
    /// project the GT fields out for NODE-only outputs.
    NodeAndGt,
    /// Published to everyone in the world by design (`proto.revocation`: a CRL is public).
    Public,
    /// Computed from other records rather than observed (`metric.sample`).
    Derived,
    /// A UI stream that mixes both and is filtered on replay (`snapshot.keyframe`).
    Mixed,
    /// Run metadata (`manifest`).
    Meta,
}

impl Visibility {
    /// True if the record contains anything no node could have known.
    ///
    /// The predicate behind the recorder's refusal and the leakage linter's rule: a
    /// dataset built for misbehaviour detection must not contain a GT-tainted field
    /// (08-measurement-and-data.md).
    pub const fn is_gt_tainted(self) -> bool {
        matches!(
            self,
            Visibility::Gt | Visibility::NodeAndGt | Visibility::Mixed
        )
    }

    /// True if the record may be written to a NODE channel unmodified.
    pub const fn allowed_on_node_channel(self) -> bool {
        matches!(self, Visibility::Node | Visibility::Public)
    }
}

impl core::fmt::Display for Visibility {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Visibility::Gt => "gt",
            Visibility::Node => "node",
            Visibility::NodeAndGt => "node-and-gt",
            Visibility::Public => "public",
            Visibility::Derived => "derived",
            Visibility::Mixed => "mixed",
            Visibility::Meta => "meta",
        })
    }
}

/// The stable id of a recording channel, as a type (03-interfaces.md §10, §14).
///
/// It is exactly [`Record::CHANNEL`] — `"node.tx"`, `"gt.kinematics"`, `"metric.sample"` —
/// wrapped so that a channel can be a map key, a set member and a subscription without every
/// crate agreeing informally that a bare `&'static str` means *this* kind of string.
///
/// **Not to be confused with a radio channel.** 03-interfaces.md §4's `ChannelId` is a
/// 5.9 GHz channel number; §10's subscription list is this. Two different things with one
/// name across two crates is a bug waiting for a `use` statement, so the recording one is
/// named for what it is.
///
/// [`Ord`] is the string order, so a `BTreeMap<ChannelName, _>` iterates deterministically —
/// which is why a subscription table is keyed by this rather than by a hash of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ChannelName(pub &'static str);

impl ChannelName {
    /// The channel of the record type `R`, without an instance of it.
    ///
    /// `ChannelName::of::<Cbr>()` is how a metric provider's `subscribe()` (§10) names the
    /// channels it wants: the name comes from the record type itself, so it cannot be
    /// misspelled in a string literal.
    pub const fn of<R: Record>() -> Self {
        ChannelName(R::CHANNEL)
    }

    /// The channel id as a string.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl core::fmt::Display for ChannelName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

impl From<ChannelName> for &'static str {
    fn from(c: ChannelName) -> &'static str {
        c.0
    }
}

/// A record detached from the call that produced it: channel, visibility, encoded bytes.
///
/// [`Ctx::emit_erased`] hands a recorder a `&dyn ErasedRecord` whose lifetime ends with the
/// call, which is right for a recorder that writes straight through and wrong for every
/// recorder that does not. A recorder that batches, that shards by channel, that hands
/// records to a writer thread or that buffers a second of history for the UI cannot keep
/// that borrow, and its only channel-agnostic escape was to encode JSON on the hot path or
/// to downcast every channel it knows through [`ErasedRecord::as_any`] — which inverts the
/// plug-in architecture, since the recorder would then have to be rebuilt for every new
/// channel a plug-in invents.
///
/// [`ErasedRecord::to_owned_record`] produces one of these instead: owned, `Send`-able,
/// storable in a queue, and still carrying the two pieces of metadata the recorder's own
/// rules are written against (the channel it belongs to, and whether it is ground truth).
/// It is the recorded form 03-interfaces.md §10 calls an [`EventRecord`].
///
/// The encoding is JSON, the lowest common denominator of §10's backends. A backend with a
/// faster path (Arrow batches, Parquet columns) still recognises its own channels through `as_any`
/// before falling back to this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedRecord {
    /// The channel's stable id ([`Record::CHANNEL`]).
    pub channel: &'static str,
    /// The record's visibility tag, which decides what may be written where.
    pub visibility: Visibility,
    /// The record's JSON encoding.
    pub json: Vec<u8>,
}

impl OwnedRecord {
    /// The channel as a [`ChannelName`], for keying a subscription table.
    pub const fn channel_name(&self) -> ChannelName {
        ChannelName(self.channel)
    }

    /// The JSON encoding as a string.
    ///
    /// # Errors
    /// [`crate::error::CoreError::Contract`] if the bytes are not UTF-8. They always are
    /// when they came from [`ErasedRecord::to_owned_record`]; the field is public, so the
    /// case is checked rather than assumed.
    pub fn json_str(&self) -> Result<&str> {
        core::str::from_utf8(&self.json).map_err(|e| {
            crate::error::CoreError::Contract(format!(
                "record on channel {} is not valid UTF-8: {e}",
                self.channel
            ))
        })
    }
}

/// The recorded form of a plug-in's [`Record`] — 03-interfaces.md §10's `EventRecord`.
///
/// §10 declares `MetricProvider::on_event(&mut self, ev: &EventRecord)`,
/// `Exporter::on_event` and `Recorder::write` against this name; it is
/// [`OwnedRecord`], so a metric provider can keep the record it was handed.
pub type EventRecord = OwnedRecord;

/// Something a plug-in emits into the recorder.
///
/// One Rust type per recording channel (03-interfaces.md §14), carrying the channel's
/// stable id and its visibility tag. `Serialize` is the lowest common denominator every
/// backend can consume; the recorder is free to recognise the concrete type through
/// [`ErasedRecord::as_any`] and take a faster path (Arrow batches, Parquet columns) for the channels
/// it knows.
pub trait Record: Serialize + 'static {
    /// The channel's stable id, e.g. `"node.tx"` or `"metric.sample"`.
    const CHANNEL: &'static str;

    /// The channel's visibility tag.
    const VISIBILITY: Visibility;

    /// This record's visibility.
    ///
    /// Defaults to [`Record::VISIBILITY`]; override only for a record type whose tag
    /// genuinely varies per instance (a `phy.rx` written without the transmitter's
    /// identity is [`Visibility::Node`], with it [`Visibility::NodeAndGt`]).
    fn visibility(&self) -> Visibility {
        Self::VISIBILITY
    }
}

/// A [`Record`] seen through a trait object, so [`Ctx`] can stay dyn-compatible.
///
/// Blanket-implemented for every `Record`; engines consume it, plug-ins never name it.
pub trait ErasedRecord {
    /// The record's channel id ([`Record::CHANNEL`]).
    fn channel(&self) -> &'static str;

    /// The record's visibility tag ([`Record::visibility`]).
    fn visibility(&self) -> Visibility;

    /// Appends the record's JSON encoding to `out`.
    ///
    /// The fallback every recorder can implement; a recorder that knows the channel should
    /// downcast through [`ErasedRecord::as_any`] instead of paying for JSON on a hot path.
    fn write_json(&self, out: &mut Vec<u8>) -> Result<()>;

    /// The record as [`core::any::Any`], so a recorder can downcast to a channel it knows.
    fn as_any(&self) -> &dyn core::any::Any;

    /// This record as an owned [`OwnedRecord`], for a recorder that cannot keep the borrow.
    ///
    /// The escape from `emit_erased`'s call-scoped lifetime: a recorder that batches, that
    /// shards by channel or that hands work to a writer thread calls this once and keeps
    /// the result. See [`OwnedRecord`].
    ///
    /// Defaulted in terms of [`ErasedRecord::channel`], [`ErasedRecord::visibility`] and
    /// [`ErasedRecord::write_json`], so it costs an implementer nothing and every existing
    /// record type has it.
    ///
    /// # Errors
    /// Whatever [`ErasedRecord::write_json`] returns — a record whose `Serialize` fails.
    fn to_owned_record(&self) -> Result<OwnedRecord> {
        let mut json = Vec::new();
        self.write_json(&mut json)?;
        Ok(OwnedRecord {
            channel: self.channel(),
            visibility: self.visibility(),
            json,
        })
    }
}

impl<R: Record> ErasedRecord for R {
    fn channel(&self) -> &'static str {
        R::CHANNEL
    }

    fn visibility(&self) -> Visibility {
        Record::visibility(self)
    }

    fn write_json(&self, out: &mut Vec<u8>) -> Result<()> {
        serde_json::to_writer(out, self)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// The engine context handed to every plug-in call (03-interfaces.md §1.1).
///
/// See the module documentation for why `rng` takes `&self` and why `emit` is split.
pub trait Ctx {
    /// The world model, `v2xw_world::World` in a real engine.
    type World;
    /// The spatial index over the current actor kinematics, `v2xw_world::ActorIndex`.
    type Actors;
    /// The kernel's event payload enum, which the kernel crate instantiates
    /// [`crate::event::Scheduler`] with.
    type Payload;

    /// The instant being dispatched.
    fn now(&self) -> SimTime;

    /// The deterministic stream for `(domain, entity)` (02-architecture.md §6.2).
    ///
    /// A plug-in never owns an RNG; it asks for the stream of the entity it is acting for.
    /// The guard returns the stream to the registry when it is dropped, so the usual shape
    /// is one statement per draw: `let x = ctx.rng(d, e).normal(mu, sigma);`.
    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_>;

    /// Schedules an event, returning the handle that cancels it.
    ///
    /// # Panics
    /// If `at` is before [`Ctx::now`], or (in debug builds) if the event would sit earlier
    /// in the total order than the event being dispatched — see
    /// [`crate::event::Scheduler::schedule`].
    fn schedule(&mut self, at: SimTime, class: EventClass, payload: Self::Payload) -> EventHandle;

    /// Schedules an event `delay` after [`Ctx::now`] — "now plus this much", which is what
    /// a timer, a backoff, a service time or a repetition interval is.
    ///
    /// Defaulted in terms of [`Ctx::schedule`], and saturating, so no model has to write
    /// `ctx.now() + delay` (and get it wrong) itself. See
    /// [`crate::event::Scheduler::schedule_after`]; an engine whose scheduler is that one
    /// need not override this.
    fn schedule_after(
        &mut self,
        delay: Duration,
        class: EventClass,
        payload: Self::Payload,
    ) -> EventHandle {
        self.schedule(delay.after(self.now()), class, payload)
    }

    /// Cancels a scheduled event; `true` if it was still pending.
    fn cancel(&mut self, handle: EventHandle) -> bool;

    /// The world: lanes, junctions, buildings, terrain, signals, sites.
    fn world(&self) -> &Self::World;

    /// The spatial index over the current actor kinematics.
    fn actors(&self) -> &Self::Actors;

    /// Emits an already-erased record. Callers use [`CtxExt::emit`].
    fn emit_erased(&mut self, record: &dyn ErasedRecord);

    /// Records that `model` with `params` produced `subject` — the `why` service.
    ///
    /// Cheap enough for a hot path: it stores three handles, deduplicated by the whole
    /// triple (02-architecture.md §6.5).
    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId);

    /// This plug-in instance's resolved parameters: its card's defaults merged with the
    /// scenario's overrides. Every number a model reads comes from here and is declared on
    /// its card (invariant I-C3).
    fn params(&self) -> &ParamSet;
}

/// The generic conveniences on top of the dyn-compatible [`Ctx`].
///
/// Blanket-implemented for every `Ctx`, including `dyn Ctx`, so a plug-in holding
/// `&mut dyn Ctx<…>` writes `ctx.emit(record)` exactly as 03-interfaces.md §1.1 says.
pub trait CtxExt: Ctx {
    /// Emits a typed record into the recorder.
    fn emit<R: Record>(&mut self, record: R) {
        self.emit_erased(&record);
    }
}

impl<C: Ctx + ?Sized> CtxExt for C {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Scheduler;
    use crate::ids::{ActorId, NodeId};
    use crate::provenance::ProvenanceLog;
    use crate::registry::ParamSetStore;
    use crate::rng::RngRegistry;
    use crate::time::NS_PER_MS;

    /// A record type as a plug-in would declare one.
    #[derive(Debug, Serialize, serde::Deserialize, PartialEq)]
    struct Kinematics {
        t: SimTime,
        actor: ActorId,
        x_m: f64,
    }

    impl Record for Kinematics {
        const CHANNEL: &'static str = "gt.kinematics";
        const VISIBILITY: Visibility = Visibility::Gt;
    }

    #[derive(Debug, Serialize)]
    struct Cbr {
        node: NodeId,
        cbr: f64,
    }

    impl Record for Cbr {
        const CHANNEL: &'static str = "mac.cbr";
        const VISIBILITY: Visibility = Visibility::Node;
    }

    /// The minimum an engine has to hold to implement [`Ctx`]. A real kernel has more,
    /// but the borrow structure is this one.
    struct TestCtx {
        scheduler: Scheduler<&'static str>,
        rng: RngRegistry,
        provenance: ProvenanceLog,
        params: ParamSet,
        world: Vec<f64>,
        actors: Vec<ActorId>,
        emitted: Vec<(&'static str, Visibility, String)>,
    }

    impl TestCtx {
        fn new() -> Self {
            let mut params = ParamSet::new();
            params.insert("sigma_db", serde_json::json!(4.0));
            Self {
                scheduler: Scheduler::new(),
                rng: RngRegistry::new(42),
                provenance: ProvenanceLog::new(),
                params,
                world: vec![100.0, 250.0],
                actors: vec![ActorId::new(0), ActorId::new(1)],
                emitted: Vec::new(),
            }
        }
    }

    impl Ctx for TestCtx {
        type World = Vec<f64>;
        type Actors = Vec<ActorId>;
        type Payload = &'static str;

        fn now(&self) -> SimTime {
            self.scheduler.now()
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

    /// A plug-in body written against `&mut dyn Ctx`, which is how in-process Rust models
    /// are called (ADR 0007 §8). If [`Ctx`] stopped being dyn-compatible this would not
    /// compile — and neither would any family trait declared with trait objects.
    fn mobility_step(
        ctx: &mut dyn Ctx<World = Vec<f64>, Actors = Vec<ActorId>, Payload = &'static str>,
        model: ModelRef,
        params: ParamSetId,
    ) {
        let t = ctx.now();
        for i in 0..ctx.actors().len() {
            let actor = ctx.actors()[i];
            // The shape that a `&mut self` rng accessor would reject: a `&self` borrow of
            // the world held across the draw.
            let lane_len = ctx.world()[i % ctx.world().len()];
            let sigma = ctx.params().get_f64("sigma_db").unwrap_or(1.0);
            let noise = ctx
                .rng(RngDomain::Mobility, EntityRef::Actor(actor))
                .normal(0.0, sigma);
            let x_m = crate::math::q3(lane_len + noise);

            // …and then the &mut self calls, once the guard has been dropped.
            ctx.emit(Kinematics { t, actor, x_m });
            ctx.why(
                crate::provenance::ProvSubject::actor(actor, "x_m"),
                model,
                params,
            );
        }
        let h = ctx.schedule(t + 100 * NS_PER_MS, EventClass::MobilityStep, "step");
        assert!(ctx.cancel(h));
    }

    #[test]
    fn a_plugin_can_be_written_against_a_dyn_ctx() {
        let mut ctx = TestCtx::new();
        let mut store = ParamSetStore::new();
        let params = store.intern(ctx.params.clone());
        mobility_step(&mut ctx, ModelRef::new(0), params);

        assert_eq!(ctx.emitted.len(), 2);
        let (channel, visibility, json) = &ctx.emitted[0];
        assert_eq!(*channel, "gt.kinematics");
        assert_eq!(*visibility, Visibility::Gt);
        let back: Kinematics = serde_json::from_str(json).unwrap();
        assert_eq!(back.actor, ActorId::new(0));
        assert!(crate::math::is_quantized(back.x_m, 3), "{}", back.x_m);
        assert_eq!(ctx.provenance.len(), 2);
        assert!(ctx.scheduler.is_empty(), "the step event was cancelled");
    }

    /// The defaulted "now plus this much" is reachable through a trait object — it has to
    /// be, because that is how every in-process model is called — and it agrees with the
    /// absolute form it is defined in terms of.
    #[test]
    fn schedule_after_works_through_a_dyn_ctx() {
        use crate::time::Duration;

        let mut ctx = TestCtx::new();
        let dyn_ctx: &mut dyn Ctx<World = Vec<f64>, Actors = Vec<ActorId>, Payload = &'static str> =
            &mut ctx;
        dyn_ctx.schedule(2 * NS_PER_MS, EventClass::MacTimer, "anchor");
        let h = dyn_ctx.schedule_after(Duration::from_millis(5), EventClass::NodeTask, "later");
        assert!(dyn_ctx.cancel(h));

        // From t = 0 the two spellings coincide; after a dispatch the relative one moves.
        assert_eq!(ctx.scheduler.pop().unwrap().0.time, 2 * NS_PER_MS);
        let dyn_ctx: &mut dyn Ctx<World = Vec<f64>, Actors = Vec<ActorId>, Payload = &'static str> =
            &mut ctx;
        assert_eq!(dyn_ctx.now(), 2 * NS_PER_MS);
        dyn_ctx.schedule_after(Duration::from_millis(5), EventClass::NodeTask, "relative");
        assert_eq!(ctx.scheduler.peek_key().unwrap().time, 7 * NS_PER_MS);
    }

    /// The context is a pure function of its inputs: the same engine state replays the
    /// same records, because every draw came from a keyed stream.
    #[test]
    fn two_runs_of_a_plugin_emit_identical_records() {
        let run = || {
            let mut ctx = TestCtx::new();
            mobility_step(&mut ctx, ModelRef::new(0), ParamSetId::new(0));
            ctx.emitted
        };
        assert_eq!(run(), run());
    }

    /// A model may draw for one entity from several domains in one call; the streams are
    /// independent and neither guard outlives its statement.
    #[test]
    fn several_domains_for_one_entity_compose() {
        let ctx = TestCtx::new();
        let actor = EntityRef::Actor(ActorId::new(3));
        let noise = ctx.rng(RngDomain::Mobility, actor).normal(0.0, 1.0);
        let speed = ctx.rng(RngDomain::DesiredSpeed, actor).uniform(25.0, 35.0);
        let again = ctx.rng(RngDomain::Mobility, actor).normal(0.0, 1.0);
        assert!((25.0..35.0).contains(&speed));
        assert_ne!(
            noise, again,
            "the mobility stream advanced, it did not restart"
        );
    }

    #[test]
    fn visibility_tags_classify_records() {
        assert!(Visibility::Gt.is_gt_tainted());
        assert!(Visibility::NodeAndGt.is_gt_tainted());
        assert!(Visibility::Mixed.is_gt_tainted());
        assert!(!Visibility::Node.is_gt_tainted());
        assert!(!Visibility::Derived.is_gt_tainted());
        assert!(Visibility::Node.allowed_on_node_channel());
        assert!(Visibility::Public.allowed_on_node_channel());
        assert!(!Visibility::Gt.allowed_on_node_channel());
        assert_eq!(Visibility::NodeAndGt.to_string(), "node-and-gt");
        assert_eq!(
            serde_json::to_string(&Visibility::NodeAndGt).unwrap(),
            "\"node-and-gt\""
        );
    }

    #[test]
    fn erased_records_carry_their_channel_and_can_be_downcast() {
        let r = Cbr {
            node: NodeId::new(7),
            cbr: 0.42,
        };
        let erased: &dyn ErasedRecord = &r;
        assert_eq!(erased.channel(), "mac.cbr");
        assert_eq!(erased.visibility(), Visibility::Node);
        let mut bytes = Vec::new();
        erased.write_json(&mut bytes).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"node":7,"cbr":0.42}"#
        );
        // A recorder that knows the channel takes the fast path instead of parsing JSON.
        assert_eq!(erased.as_any().downcast_ref::<Cbr>().unwrap().cbr, 0.42);
        assert!(erased.as_any().downcast_ref::<Kinematics>().is_none());
    }

    /// A recorder that batches cannot keep `emit_erased`'s borrow, and its only
    /// channel-agnostic escape was to encode JSON on the hot path or to downcast every
    /// channel it knows — which inverts the plug-in architecture, since the recorder would
    /// then need rebuilding for every channel a plug-in invents. `to_owned_record` is the
    /// owned form it can queue instead.
    #[test]
    fn a_record_can_be_detached_from_the_call_that_emitted_it() {
        /// A recorder that keeps what it is given, as a batching one must.
        struct Batching {
            queue: Vec<OwnedRecord>,
        }
        impl Batching {
            fn write(&mut self, r: &dyn ErasedRecord) {
                self.queue
                    .push(r.to_owned_record().expect("record serialises"));
            }
        }

        let mut rec = Batching { queue: Vec::new() };
        rec.write(&Cbr {
            node: NodeId::new(7),
            cbr: 0.42,
        });
        rec.write(&Kinematics {
            t: 5,
            actor: ActorId::new(1),
            x_m: 12.5,
        });

        // The records outlive the calls that produced them, with their metadata intact.
        assert_eq!(rec.queue.len(), 2);
        assert_eq!(rec.queue[0].channel, "mac.cbr");
        assert_eq!(rec.queue[0].visibility, Visibility::Node);
        assert_eq!(rec.queue[0].json_str().unwrap(), r#"{"node":7,"cbr":0.42}"#);
        assert_eq!(rec.queue[1].channel, "gt.kinematics");
        assert!(rec.queue[1].visibility.is_gt_tainted());
        assert!(!rec.queue[1].visibility.allowed_on_node_channel());

        // A channel-keyed table over them is ordered, not hashed.
        let mut by_channel: std::collections::BTreeMap<ChannelName, usize> =
            std::collections::BTreeMap::new();
        for r in &rec.queue {
            *by_channel.entry(r.channel_name()).or_default() += 1;
        }
        assert_eq!(
            by_channel.keys().copied().collect::<Vec<_>>(),
            vec![ChannelName::of::<Kinematics>(), ChannelName::of::<Cbr>()],
            "gt.kinematics sorts before mac.cbr, deterministically"
        );

        // …and the owned record is `Send`, so a writer thread can take it.
        fn assert_send<T: Send>(_: &T) {}
        assert_send(&rec.queue);
    }

    /// The recording channel's name is a type, so it cannot be confused with §4's radio
    /// `ChannelId` and cannot be misspelled in a string literal.
    #[test]
    fn a_channel_name_comes_from_the_record_type() {
        const CBR: ChannelName = ChannelName::of::<Cbr>();
        assert_eq!(CBR.as_str(), "mac.cbr");
        assert_eq!(CBR.to_string(), "mac.cbr");
        assert_eq!(CBR, ChannelName(Cbr::CHANNEL));
        assert_ne!(CBR, ChannelName::of::<Kinematics>());
        assert!(
            ChannelName::of::<Kinematics>() < CBR,
            "Ord is the string order"
        );
        assert_eq!(
            serde_json::to_string(&CBR).unwrap(),
            "\"mac.cbr\"",
            "a newtype serialises as its string"
        );
        let s: &'static str = CBR.into();
        assert_eq!(s, "mac.cbr");

        // `EventRecord` is §10's name for the owned form.
        let ev: EventRecord = Cbr {
            node: NodeId::new(1),
            cbr: 0.1,
        }
        .to_owned_record()
        .unwrap();
        assert_eq!(ev.channel_name(), CBR);
    }
}
