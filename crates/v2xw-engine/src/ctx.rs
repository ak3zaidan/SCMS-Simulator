//! [`EngineCtx`] — the engine's implementation of [`v2xw_core::ctx::Ctx`], and the sink it
//! emits into.
//!
//! This is the concrete context 03-interfaces.md §1.1 describes and build decision D12.2
//! arbitrates around. It binds the three associated types the contract crate leaves open:
//!
//! | Associated type | Bound to | Why it could not be concrete in `v2xw-core` |
//! |---|---|---|
//! | `World` | [`v2xw_world::World`] | lives in `v2xw-world`, above the contract crate |
//! | `Actors` | [`ActorSnapshot`] | lives in `v2xw-mobility`, above both |
//! | `Payload` | [`crate::Event`] | lives here, above everything (D8) |
//!
//! # Borrowed, not owned
//!
//! `EngineCtx` holds borrows rather than owning the kernel, so the engine can hand a model
//! a context while holding that model mutably — `mobility.step(&mut ctx, dt)` with both
//! `mobility` and the scheduler living in the same `Engine`. An owning context would make
//! that a double borrow of `self` and every phase would need a `take`/`put` dance around
//! it, which is exactly the shape in which a model gets dropped on an early return.
//!
//! # `schedule` cross-checks its class
//!
//! [`Ctx::schedule`] takes an [`EventClass`] *and* a payload, and [`crate::Event::class`]
//! already answers the same question from the payload — by a match the compiler checks.
//! The engine panics when the two disagree rather than trusting the argument, because the
//! failure a wrong class produces is a silent reordering: a `NodeTask` scheduled at
//! `Control` priority runs before the mobility step of its own instant and reads the
//! previous step's kinematics, and nothing in the output says so.

use v2xw_core::ctx::{Ctx, ErasedRecord, OwnedRecord, Visibility};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::SimTime;
use v2xw_mobility::ActorSnapshot;
use v2xw_world::World;

use crate::event::Event;

/// Somewhere a run's records go.
///
/// The engine emits into this rather than into [`v2xw_record::RecordingWriter`] directly,
/// so a run can be driven with no files at all — which is what every test in this crate
/// does, and what the determinism comparison needs, since comparing two runs means
/// comparing two record streams and not two MCAP files (an MCAP carries a manifest with a
/// build timestamp in it).
pub trait RunRecorder {
    /// Writes one record, already erased, at the instant it was emitted.
    ///
    /// Infallible on purpose: a recorder that cannot write has to decide what that means
    /// for the run, and the engine has no useful answer in the middle of a phase. A
    /// failing backend counts the failure and reports it at [`RunRecorder::finish`].
    fn write(&mut self, at: SimTime, record: &OwnedRecord);

    /// Stores one VWP wire frame — a `Keyframe` or a `Delta` (vwp-v1 §3.3, §3.4).
    ///
    /// The engine's snapshot stream is *binary* and does not go through
    /// [`RunRecorder::write`]: build decision D11 item 5 keeps the two encodings apart,
    /// and §7.2's byte-identity guarantee attaches to the frame's own bytes, so a frame
    /// is handed to the recorder verbatim rather than re-serialised as a record.
    ///
    /// Defaulted to a discard, so a recorder that only wants the record stream need not
    /// know the binary path exists. **A recorder that wraps another one must forward
    /// this**, exactly as it forwards [`RunRecorder::write`]. A wrapper that forwards
    /// only `write` silently drops the normative binary stream — which is precisely the
    /// defect this method exists to close — and nothing in the resulting file says so.
    fn write_wire_frame(&mut self, frame: &v2xw_record::wire::Frame) {
        let _ = frame;
    }

    /// The octets of one frame that went on the air: the signed IEEE 1609.2 SPDU exactly as
    /// the node's security stack built it, handed over beside the `node.tx` record for the
    /// same frame (`msg` is that record's message id) and at the same instant.
    ///
    /// **A tap, not a record.** It is not written to a recording, it is not counted in the
    /// run report and it is not part of any digest, so the determinism contract is not
    /// touched: a recorder that ignores it (the default) sees a byte-identical record stream.
    /// It exists for a viewer — the chase view's message inspector decodes these octets with
    /// the real 1609.2 and J2735/ETSI decoders rather than printing what the sender believed
    /// it had encoded — and every frame's bytes are already in memory at this point, so the
    /// cost of calling it is one virtual call per transmitted frame.
    ///
    /// Only frames the node itself encoded and signed arrive here. A frame the engine sized
    /// from a protocol table (a misbehaviour report, a CRL broadcast) has no octets and is
    /// not tapped.
    fn tap_frame(&mut self, at: SimTime, node: v2xw_core::ids::NodeId, msg: u64, spdu: &[u8]) {
        let _ = (at, node, msg, spdu);
    }

    /// How many records were refused, for the run report.
    fn refused(&self) -> u64 {
        0
    }

    /// How many records the backend confirms it *stored*, when it can say.
    ///
    /// `None` means "this recorder cannot tell you", which is a different answer from
    /// zero and is reported as such: [`crate::RunReport::records`] counts what the engine
    /// emitted, and the two disagree whenever a write failed. A run report that quoted
    /// only the emitted count could contradict the artefact lying beside it.
    fn records_written(&self) -> Option<u64> {
        None
    }

    /// How many VWP frames the backend confirms it stored, when it can say.
    fn frames_written(&self) -> Option<u64> {
        None
    }

    /// Whether the run should end now, before its horizon.
    ///
    /// [`crate::Engine::run`] asks this once per dispatched event and stops at the first
    /// `true`, between two events, so a cancelled run never ends half-way through a
    /// phase. It is the only cancellation point the kernel has, and it exists because a
    /// run driven from a server must be stoppable: without it `run.stop` could only stop
    /// *listening*, and the kernel thread kept computing to the horizon beside the next
    /// run's — two simulations competing for the machine for every restart.
    ///
    /// Defaulted to `false`, so a batch recorder that always wants the whole run need not
    /// know it exists. A cancelled run is not a result: its report covers the events up
    /// to the cancellation and nothing after, and no caller should treat it as complete.
    fn cancelled(&self) -> bool {
        false
    }
}

/// A recorder that keeps everything in memory, in emission order.
#[derive(Debug, Default)]
pub struct MemoryRecorder {
    records: Vec<(SimTime, OwnedRecord)>,
    frames: Vec<v2xw_record::wire::Frame>,
}

impl MemoryRecorder {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything written, in emission order.
    pub fn records(&self) -> &[(SimTime, OwnedRecord)] {
        &self.records
    }

    /// How many records landed on `channel`.
    pub fn count_on(&self, channel: &str) -> usize {
        self.records
            .iter()
            .filter(|(_, r)| r.channel == channel)
            .count()
    }

    /// Every VWP frame written, in emission order.
    ///
    /// Kept separately from the records, and deliberately *not* part of
    /// [`MemoryRecorder::digest_hex`]: the record digest is what the determinism
    /// comparison is written against, and folding a second stream into it would change
    /// every published digest for a reason that has nothing to do with determinism.
    /// [`MemoryRecorder::frame_digest_hex`] covers the frames.
    pub fn frames(&self) -> &[v2xw_record::wire::Frame] {
        &self.frames
    }

    /// How many of the stored frames are keyframes rather than deltas.
    ///
    /// A frame whose header will not parse is counted as neither, which cannot happen for
    /// a frame this crate produced and is stated rather than unwrapped.
    pub fn keyframe_count(&self) -> usize {
        self.frames
            .iter()
            .filter(|f| {
                f.header()
                    .ok()
                    .and_then(|h| h.kind())
                    .is_some_and(|k| k == v2xw_record::wire::MsgType::Keyframe)
            })
            .count()
    }

    /// A digest over every record, in order: `SHA-256(t ‖ channel ‖ visibility ‖ json)*`.
    ///
    /// This is what "two runs of one scenario produce identical outputs" is asserted on.
    /// It covers the instant, the channel and the bytes, and it covers *order*, because
    /// two runs that emit the same records in a different order are not the same run
    /// (02-architecture.md §6.1).
    pub fn digest_hex(&self) -> String {
        let mut w = v2xw_core::hash::Sha256Writer::new();
        for (t, r) in &self.records {
            w.update(&t.to_le_bytes());
            w.update(r.channel.as_bytes());
            w.update(r.visibility.to_string().as_bytes());
            w.update(&r.json);
        }
        w.finish_hex()
    }

    /// A digest over every VWP frame, in order: `SHA-256(frame bytes)*`.
    ///
    /// The frames are the normative wire stream (vwp-v1 §7.2), so this is the digest that
    /// says two runs put the same bytes on the air.
    pub fn frame_digest_hex(&self) -> String {
        let mut w = v2xw_core::hash::Sha256Writer::new();
        for f in &self.frames {
            w.update(f.as_bytes());
        }
        w.finish_hex()
    }
}

impl RunRecorder for MemoryRecorder {
    fn write(&mut self, at: SimTime, record: &OwnedRecord) {
        self.records.push((at, record.clone()));
    }

    fn write_wire_frame(&mut self, frame: &v2xw_record::wire::Frame) {
        self.frames.push(frame.clone());
    }

    fn records_written(&self) -> Option<u64> {
        Some(self.records.len() as u64)
    }

    fn frames_written(&self) -> Option<u64> {
        Some(self.frames.len() as u64)
    }
}

/// A recorder that keeps only the digest of what it was handed.
///
/// It exists because the determinism comparison and the scale measurement pull in
/// opposite directions: the comparison needs the record stream's digest, and
/// [`MemoryRecorder`] holds every record to produce one, which at ten thousand nodes is
/// tens of gigabytes of `phy.rx`. This hashes the same bytes in the same order and keeps
/// none of them, so "identical content digest across repeated runs" is checkable at any
/// size.
///
/// `digest_hex` agrees with [`MemoryRecorder::digest_hex`] record for record; the two are
/// checked against each other in this module's tests, because a digest that is only
/// self-consistent proves nothing about the stream it claims to cover.
pub struct DigestRecorder {
    hasher: v2xw_core::hash::Sha256Writer,
    frame_hasher: v2xw_core::hash::Sha256Writer,
    written: u64,
    frames: u64,
    per_channel: std::collections::BTreeMap<String, (u64, u64)>,
}

impl core::fmt::Debug for DigestRecorder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `Sha256Writer` is not `Debug`, and the running state is not something a reader
        // of a debug print can use anyway; the counts are.
        f.debug_struct("DigestRecorder")
            .field("written", &self.written)
            .field("frames", &self.frames)
            .field("channels", &self.per_channel.len())
            .finish_non_exhaustive()
    }
}

impl Default for DigestRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl DigestRecorder {
    /// An empty recorder.
    pub fn new() -> Self {
        Self {
            hasher: v2xw_core::hash::Sha256Writer::new(),
            frame_hasher: v2xw_core::hash::Sha256Writer::new(),
            written: 0,
            frames: 0,
            per_channel: std::collections::BTreeMap::new(),
        }
    }

    /// How many records it was handed.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// How many records and how many JSON bytes landed on each channel, by channel name.
    pub fn per_channel(&self) -> &std::collections::BTreeMap<String, (u64, u64)> {
        &self.per_channel
    }

    /// How many VWP frames it was handed.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The digest over every record, in order: `SHA-256(t ‖ channel ‖ visibility ‖ json)*`.
    pub fn digest_hex(&self) -> String {
        self.hasher.clone().finish_hex()
    }

    /// The digest over every VWP frame, in order: `SHA-256(frame bytes)*`.
    ///
    /// Kept apart from [`DigestRecorder::digest_hex`] for the reason
    /// [`MemoryRecorder::frame_digest_hex`] gives: the record digest is a published
    /// number and adding a second stream to it would move it for no determinism reason.
    pub fn frame_digest_hex(&self) -> String {
        self.frame_hasher.clone().finish_hex()
    }
}

impl RunRecorder for DigestRecorder {
    fn write(&mut self, at: SimTime, record: &OwnedRecord) {
        self.hasher.update(&at.to_le_bytes());
        self.hasher.update(record.channel.as_bytes());
        self.hasher.update(record.visibility.to_string().as_bytes());
        self.hasher.update(&record.json);
        self.written += 1;
        let entry = self
            .per_channel
            .entry(record.channel.to_string())
            .or_insert((0, 0));
        entry.0 += 1;
        entry.1 += record.json.len() as u64;
    }

    fn write_wire_frame(&mut self, frame: &v2xw_record::wire::Frame) {
        self.frame_hasher.update(frame.as_bytes());
        self.frames += 1;
    }

    fn records_written(&self) -> Option<u64> {
        Some(self.written)
    }

    fn frames_written(&self) -> Option<u64> {
        Some(self.frames)
    }
}

/// A recorder that drops everything, for a run measuring only its metrics.
#[derive(Debug, Default)]
pub struct NullRecorder {
    written: u64,
    frames: u64,
}

impl NullRecorder {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many records it was handed.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// How many VWP frames it was handed.
    pub fn frames(&self) -> u64 {
        self.frames
    }
}

impl RunRecorder for NullRecorder {
    fn write(&mut self, _at: SimTime, _record: &OwnedRecord) {
        self.written += 1;
    }

    fn write_wire_frame(&mut self, _frame: &v2xw_record::wire::Frame) {
        self.frames += 1;
    }

    /// A recorder that drops everything nevertheless *accepted* everything: nothing was
    /// refused, so the count it reports is the count it was handed. That is what makes a
    /// run report over a null recorder read the same as one over a real file.
    fn records_written(&self) -> Option<u64> {
        Some(self.written)
    }

    fn frames_written(&self) -> Option<u64> {
        Some(self.frames)
    }
}

impl<W: std::io::Write + std::io::Seek> RunRecorder for v2xw_record::RecordingWriter<W> {
    fn write(&mut self, at: SimTime, record: &OwnedRecord) {
        // `write_record` fails on a channel the recording does not declare or on an I/O
        // error. Neither is something a phase can act on, so what the run report carries
        // is the container's own count of what it stored — `records_written` below — and
        // not the engine's count of what it handed over.
        let _ = v2xw_record::RecordingWriter::write_record(self, at, record);
    }

    fn write_wire_frame(&mut self, frame: &v2xw_record::wire::Frame) {
        let _ = v2xw_record::RecordingWriter::write_frame(self, frame);
    }

    fn records_written(&self) -> Option<u64> {
        Some(v2xw_record::RecordingWriter::summary(self).record_count)
    }

    fn frames_written(&self) -> Option<u64> {
        Some(v2xw_record::RecordingWriter::summary(self).frame_count)
    }
}

/// The engine context handed to every model call.
///
/// Built fresh for each phase from borrows of the [`crate::Engine`]'s own state; it holds
/// no state of its own, so two runs that reach a phase with the same state build the same
/// context.
pub struct EngineCtx<'a> {
    /// The one event heap.
    pub(crate) scheduler: &'a mut Scheduler<Event>,
    /// The deterministic streams.
    pub(crate) rng: &'a RngRegistry,
    /// The world.
    pub(crate) world: &'a World,
    /// The actor snapshot the last mobility step published.
    pub(crate) actors: &'a ActorSnapshot,
    /// The `why` service.
    pub(crate) provenance: &'a mut ProvenanceLog,
    /// The parameters of the model being called.
    pub(crate) params: &'a ParamSet,
    /// Where emitted records go.
    pub(crate) recorder: &'a mut dyn RunRecorder,
    /// Records refused because their visibility is not allowed on their channel.
    pub(crate) refused: u64,
    /// The instant a record emitted through this context is *stamped* at, when that is
    /// not the scheduler's current instant.
    ///
    /// Mobility is the one phase where the two differ. A mobility step dispatched at `t`
    /// advances the world to `t + dt` and publishes the states it produced, so the
    /// scheduler's instant is the start of the step and the state describes its end;
    /// stamping such a record at `Ctx::now` filed every ground-truth kinematics record
    /// one mobility step early, against its own `t` field. This is how the engine says
    /// "the time of this record is the time of the state it describes".
    ///
    /// It never changes [`Ctx::now`]: a model that asks what time it is still gets the
    /// scheduler's answer, because that is the instant it is being run at.
    pub(crate) emit_at: Option<SimTime>,
}

impl core::fmt::Debug for EngineCtx<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EngineCtx")
            .field("now", &self.scheduler.now())
            .field("pending", &self.scheduler.len())
            .field("actors", &self.actors.len())
            .finish_non_exhaustive()
    }
}

impl<'a> EngineCtx<'a> {
    /// The context for a phase.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        scheduler: &'a mut Scheduler<Event>,
        rng: &'a RngRegistry,
        world: &'a World,
        actors: &'a ActorSnapshot,
        provenance: &'a mut ProvenanceLog,
        params: &'a ParamSet,
        recorder: &'a mut dyn RunRecorder,
    ) -> Self {
        EngineCtx {
            scheduler,
            rng,
            world,
            actors,
            provenance,
            params,
            recorder,
            refused: 0,
            emit_at: None,
        }
    }

    /// Stamps every record emitted from now on at `at` rather than at [`Ctx::now`].
    ///
    /// `None` restores the default. See [`EngineCtx::emit_at`] for why the two can differ.
    pub fn stamp_records_at(&mut self, at: Option<SimTime>) {
        self.emit_at = at;
    }

    /// Schedules `payload` at its own class, which is the spelling the engine's own phases
    /// use: there is no class to get wrong.
    pub fn post(&mut self, at: SimTime, payload: Event) -> EventHandle {
        let class = payload.class();
        self.scheduler.schedule(at, class, payload)
    }

    /// How many records this context refused.
    pub fn refused(&self) -> u64 {
        self.refused
    }
}

impl Ctx for EngineCtx<'_> {
    type World = World;
    type Actors = ActorSnapshot;
    type Payload = Event;

    fn now(&self) -> SimTime {
        self.scheduler.now()
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.rng.checkout(domain, entity)
    }

    fn schedule(&mut self, at: SimTime, class: EventClass, payload: Event) -> EventHandle {
        assert_eq!(
            class,
            payload.class(),
            "event scheduled at class {class} but its payload {payload:?} is a \
             {} event; the payload's class is the compiler-checked one and a mismatch \
             silently reorders the instant (02-architecture.md §5.1)",
            payload.class()
        );
        self.scheduler.schedule(at, class, payload)
    }

    fn cancel(&mut self, handle: EventHandle) -> bool {
        self.scheduler.cancel(handle)
    }

    fn world(&self) -> &World {
        self.world
    }

    fn actors(&self) -> &ActorSnapshot {
        self.actors
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        let Ok(owned) = record.to_owned_record() else {
            self.refused += 1;
            return;
        };
        // The recorder's own rule (03-interfaces.md §14): a ground-truth record may not be
        // written to a NODE channel. The tag travels with the record, so this is checkable
        // here rather than only at the exporter, which is where a leak would otherwise be
        // found — after it had already been written.
        //
        // What a channel may carry is the channel table's to say (`v2xw_record::CHANNELS`,
        // the table the recorder itself admits records by). A declared channel refuses a
        // ground-truth record exactly when it is declared node-only; the `node.` prefix is
        // the rule for a channel the table does not declare. The prefix alone was the whole
        // rule, and it refused every `node.rx` record — a reception attempt's fate, which
        // like `phy.rx` names its true sender and is declared node-and-ground-truth.
        let node_only = match v2xw_record::channels::by_name(owned.channel) {
            Some(spec) => spec.visibility.allowed_on_node_channel(),
            None => owned.channel.starts_with("node."),
        };
        if node_only && !owned.visibility.allowed_on_node_channel() {
            self.refused += 1;
            return;
        }
        let at = match self.emit_at {
            Some(t) => t,
            None => self.scheduler.now(),
        };
        self.recorder.write(at, &owned);
    }

    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) {
        self.provenance.record(subject, model, params);
    }

    fn params(&self) -> &ParamSet {
        self.params
    }
}

/// The visibility a record must have to be written to a channel in the `node.` namespace.
///
/// Exposed so the conformance test can name the rule rather than restating it.
pub const NODE_CHANNEL_VISIBILITIES: [Visibility; 2] = [Visibility::Node, Visibility::Public];
