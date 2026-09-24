//! The real engine behind the [`crate::engine::Engine`] seam: a live `v2xw-engine` run.
//!
//! [`crate::stub::StubEngine`] exists because this module could not: the transport was
//! written before the kernel. The kernel is here now, and this module is what binds it.
//!
//! # What the binding has to bridge
//!
//! `v2xw_engine::Engine` is not the same shape as [`crate::engine::Engine`], and the
//! difference is not cosmetic. Three facts about the kernel decide this module's design,
//! and each one is stated here because each one is a constraint a reader will otherwise
//! think was a choice:
//!
//! 1. **The kernel is not `Send`.** `v2xw_engine::Engine` holds `Box<dyn Mobility>`,
//!    `Box<dyn GnssModel>`, the two radio boxes and a `BTreeMap<NodeId, ObuRuntime>`, and
//!    none of those trait objects carries a `Send` bound — `v2xw_core::model::Model`, the
//!    supertrait of every family, does not require one (`v2xw-engine`'s `on_node_phase`
//!    documents the same consequence for `rayon`). The kernel therefore cannot be moved
//!    into a task, and it cannot live behind the `Mutex<Box<dyn Engine>>` [`crate::Run`]
//!    holds, because that box is `Send`. So the kernel is **built on its own thread and
//!    never leaves it**: [`spawn_host`] sends a `Scenario` (which is `Send`) across the
//!    boundary and the engine is constructed on the far side.
//! 2. **The run loop runs to the horizon.** `v2xw_engine::Engine::run` is one
//!    `while let Some(..) = scheduler.pop()` to the end of the scenario; the scheduler is
//!    private, so there is no `run_until` and no way to advance one step from outside. The
//!    step boundary this module needs is therefore taken from the **record stream**:
//!    [`StepRecorder`] watches the instant each record is emitted at and closes a step
//!    when that instant crosses a mobility-step boundary. Backpressure is a bounded
//!    channel — when the transport stops draining, the recorder's `send` blocks and the
//!    kernel stops inside it. Blocking there is safe precisely because the kernel reads no
//!    wall clock: stopping it for a second changes nothing it computes.
//! 3. **A record stream is not a scene.** `gt.kinematics`, `node.tx`, `phy.rx`,
//!    `node.verify` and `metric.sample` carry what happened; a `Keyframe` needs a *scene*.
//!    [`Projector`] rebuilds one: it holds the actor table, allocates the `u32` slots
//!    §0.1 requires to be stable for an actor's lifetime, and turns each channel's
//!    reader-side view into the §3.6 payload of its wire channel. Nothing is invented — a
//!    field the records do not carry is written as the §3.5.2 sentinel for *unknown*,
//!    which is the honest encoding and the one the client is required to handle.
//!
//! # The one thing the record stream does not carry
//!
//! **Which actor a node is mounted on.** `gt.kinematics` names an actor; `node.tx` names a
//! node; no channel joins them, and `v2xw_engine::Engine` exposes no accessor for the map
//! it keeps internally. [`Projector::equip`] reconstructs it from the three published
//! facts that decide it, and [`Projector::mapping_is_consistent`] reports whether every
//! node the run actually named was one the reconstruction accounted for. That check is the
//! reason the coupling is tolerable and not the reason it is a good idea: the engine should
//! publish the map, and this module says so in one place rather than being quietly wrong
//! in many.
//!
//! The three facts, each of which the reconstruction got wrong at least once:
//!
//! 1. **The draw.** `RngRegistry::checkout(RngDomain::Spawn, EntityRef::Actor(a))
//!    .bool(equipped_fraction)` — keyed by the actor, so it does not depend on how many
//!    spawned before it.
//! 2. **The counter starts after the masts.** One counter serves both minting sites, and
//!    `Engine::create_rsus` runs first, at build. A scenario with `n` roadside units puts
//!    them at `0..n` and its first vehicle at `n`; a reconstruction counting from zero
//!    shifts every vehicle's node id by `n`. See [`roadside_node_count`].
//! 3. **A node is named before its actor is.** The mobility phase publishes the state of
//!    the instant it is *dispatched* at, and a spawn's state belongs to the next instant,
//!    so an equipped vehicle's first frame lands one mobility step before its first
//!    `gt.kinematics` row. An id named ahead of its actor is held, not condemned: see
//!    [`Projector::unmapped_nodes`].
//!
//! # No wall clock
//!
//! Nothing here reads one. The host thread's pacing is the transport's
//! ([`crate::http::producer`]); the engine's own timeline is `SimTime` throughout, and the
//! manifest timestamp is [`LiveOptions::build_utc`], supplied by the caller exactly as
//! `v2xw_engine::Engine::build` requires.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use serde_json::{Value, json};
use v2xw_core::card::Family;
use v2xw_core::ctx::OwnedRecord;
use v2xw_core::ids::{ActorId, LaneId, NodeId, SignalId};
use v2xw_core::time::{Duration, SimTime};
use v2xw_engine::{Scenario, run::RunReport};
use v2xw_metrics::channels::{
    DetObservationView, GtKinematicsView, MacCbrView, NodeRxView, NodeTelemetryView, NodeTxView,
    NodeVerifyView, PhyRxView, ProtoRevocationView, RxOutcome, SecCertView, SignerId,
    VerifyOutcome, decode,
};
use v2xw_metrics::def::MetricSample;
use v2xw_mobility::VehicleClass;
use v2xw_record::encoder::{
    ActorPose, Cadence, SignalState as WireSignal, SlotAllocator, Snapshot,
};
use v2xw_record::wire::event::EventEntry;
use v2xw_record::wire::hello::{
    ChannelRow, ClassRow, HELLO_LIVE, HELLO_SEEKABLE, HelloBody, NODE_HAS_HSM, WorldRef,
};
use v2xw_record::wire::metric::MetricRow;
use v2xw_record::wire::provenance::{PROV_FINAL, ProvEntry, ProvenanceBody};
use v2xw_record::wire::snapshot::{ST_EQUIPPED, ST_TRANSMITTING};
use v2xw_record::wire::telemetry::NodeTelemetry;
use v2xw_record::wire::{StrTable, U32_NONE};
use v2xw_world::WorldPayload;
use v2xw_world::model::SignalState;

use crate::engine::{
    Control, ControlOutcome, Engine, Query, RunDescriptor, RunState, ScenarioSource, StageRequest,
    Staged, StepOutput,
};
use crate::error::{ParamError, Result, ServerError};
use crate::introspect::{Introspect, MetricInfo};

/// How a live run is started.
#[derive(Debug, Clone)]
pub struct LiveOptions {
    /// The manifest build timestamp. The engine may not read a clock, so the caller
    /// supplies this; it is excluded from every digest, and `""` is a legitimate choice.
    pub build_utc: String,
    /// Start the run paused at `t = 0`.
    pub paused: bool,
    /// The initial speed multiple; `0` is unthrottled.
    pub speed: f64,
    /// A human label for `Hello.str_run_label`.
    pub label: String,
    /// The session token, interned into the `Hello` table up front.
    ///
    /// It is interned here and not only in [`crate::session::Session::hello_frame`] so
    /// that the symbol-table size the §3.8 extension is based on is the size the client
    /// computes. See [`crate::session`]'s rebase for the general case.
    pub session_token: String,
    /// Where to write the MCAP recording, if anywhere.
    pub recording: Option<std::path::PathBuf>,
    /// How many produced-but-unread mobility steps the host thread may run ahead.
    ///
    /// The kernel simulates far faster than real time (the Phase 1 Manhattan scenario is
    /// 60 simulated seconds in 27 ms), so this is what stops it from running a whole
    /// scenario into memory before the first client connects.
    pub lookahead_steps: usize,
    /// How many produced steps to retain for `run.seek` (§6.6: a live run seeks backwards
    /// into recorded time).
    pub retain_steps: usize,
    /// `Hello.actor_capacity` (§3.1.1), and the **bound this run enforces on slot ids**.
    ///
    /// §3.1.1 calls the field "max concurrent actor slots for the run; a preallocation
    /// hint", and those two readings are not the same thing. `@vwp/protocol`, written from
    /// the specification without reference to any server, enforces the first: a slot at or
    /// beyond `actor_capacity` is a `ProtocolError` and the client stops applying the
    /// frame. So the stricter reading is the one that interoperates, and this server obeys
    /// it — [`Projector::absorb`] refuses a slot past this number rather than sending one
    /// the client is required to reject.
    pub actor_capacity: u32,
}

impl Default for LiveOptions {
    fn default() -> Self {
        LiveOptions {
            build_utc: String::new(),
            paused: false,
            speed: 1.0,
            label: String::new(),
            session_token: String::new(),
            recording: None,
            // 64 steps is 6.4 s at the default cadence: enough that the kernel never waits
            // on a transport hiccup, small enough that a paused run stops the kernel.
            lookahead_steps: 64,
            // 36 000 steps is an hour of simulated time at the default cadence. A step is
            // the scene plus that step's events, so this is bounded by the scenario's
            // actor count rather than by its length.
            retain_steps: 36_000,
            // `@vwp/protocol`'s own ceiling (`MAX_ACTOR_SLOTS`, 1 << 20) clamps anything
            // larger, so this is the largest number that means anything on the wire. A
            // scenario that knows its own fleet size should set it smaller: it is what a
            // client preallocates.
            actor_capacity: 1 << 20,
        }
    }
}

// --- the host thread ---------------------------------------------------------------

/// One mobility step of raw engine output: the records emitted inside it, in emission
/// order.
#[derive(Debug)]
struct RawStep {
    /// The step index; `sim_time = index · mobility_step`.
    index: u64,
    /// Every record the engine emitted in this step, in the order it emitted them.
    records: Vec<OwnedRecord>,
}

/// What the host thread reports.
#[derive(Debug)]
enum HostMsg {
    /// One step of output.
    Step(RawStep),
    /// The run reached its horizon. Carries the kernel's own report.
    Done(Box<RunReport>),
    /// The run aborted.
    Failed(String),
    /// What the scenario's exporters wrote, or why they could not.
    Exported(std::result::Result<Vec<v2xw_engine::export::Exported>, String>),
}

/// Everything about the run the kernel knows before its first step.
///
/// Assembled on the host thread, where the engine lives, and sent across once. Every
/// field of it is `Send`, which is the reason the split exists: the engine is not.
#[derive(Debug)]
struct Setup {
    /// The imported world in the engine's exact native form, for the next run's kernel.
    world_memo: Option<WorldMemo>,
    world_payload: WorldPayload,
    world_json: String,
    hello: HelloBody,
    cadence: Cadence,
    origin_m: [f64; 3],
    duration: SimTime,
    scenario_doc: Value,
    scenario_hash_hex: String,
    manifest: Value,
    provenance: ProvenanceBody,
    /// The same entries as `provenance`, resolved to strings for `explain` (§6.9).
    prov_chain: Vec<Value>,
    catalogue: Vec<MetricInfo>,
    /// Series names that are another series under a second spelling: a breakdown with a
    /// single declared value is the metric's headline (`bytes_air[air]` is `bytes_air`).
    metric_aliases: Vec<(String, String)>,
    /// Class names by `class_idx`, so the projector can map `gt.kinematics.class`.
    class_names: Vec<String>,
    /// Signal plans as `(signal id, phase boundaries)`, evaluated by the projector.
    signals: Vec<SignalPlan>,
    equipped_fraction: f64,
    /// `actors.vru.device_fraction`: what the kernel draws a pedestrian's or a cyclist's
    /// device with, instead of `equipped_fraction`.
    vru_device_fraction: f64,
    /// How many node ids the kernel mints before the first vehicle's.
    ///
    /// `v2xw_engine::Engine::build` calls `create_rsus` before it seeds the timeline, and
    /// that site and vehicle spawn take ids from **one** counter, so the first vehicle's
    /// node id is the roadside count rather than zero. See [`roadside_node_count`].
    roadside_nodes: u32,
    actor_capacity: u32,
    seed: u64,
    run_id_bytes: [u8; 16],
    obu_profile: String,
    recording_path: Option<String>,
    /// The `prov_id` a `MetricSample` resolves through: the registered metric-family
    /// model, or `0` when the run installed none.
    metric_prov: u32,
}

/// The last world this process imported, kept for the next run of the same world.
///
/// Kept as `v2xw_world::serde_native` bytes, whose round trip is exact (same content hash,
/// same lane graph), under `v2xw_engine::wiring::world_cache_key`, which covers every input
/// of the import. A run on a kept world is therefore the same run as one on a fresh import.
#[derive(Debug)]
struct WorldMemo {
    key: String,
    bytes: Vec<u8>,
}

/// Where a vehicle's body is centred, from the reference point mobility publishes.
///
/// `gt.kinematics` carries the rear-axle reference, taken at the rear bumper
/// (`v2xw_mobility::engine`, "The reference point"), with the heading along the body. A
/// renderer draws a body centred on the pose it is given, so streaming the reference point
/// as it is drew every vehicle half a length behind where it is: stopped well short of its
/// stop line, and swung outwards through a turn. The stream's pose is therefore the body's
/// centre, half the class's length ahead of the reference along the heading. The recording
/// and every metric keep the reference point; this is the display projection only.
fn body_centre(reference: [f64; 3], heading_rad: f64, class_idx: u8) -> [f64; 3] {
    let half = VehicleClass::ALL
        .get(usize::from(class_idx))
        .map_or(0.0, |c| c.spec().length_m * 0.5);
    [
        reference[0] + half * v2xw_core::math::cos(heading_rad),
        reference[1] + half * v2xw_core::math::sin(heading_rad),
        reference[2],
    ]
}

/// Every signal *group*'s timeline, flattened for evaluation without the world: one entry
/// per group of heads, keyed by [`v2xw_world::signal_group_wire_id`].
///
/// The stream used to carry one state per controller — its *first* movement's — and the
/// renderer keyed every head of a junction by the controller id, so all but one head of a
/// junction stayed dark and the one that lit showed whichever approach happened to be
/// movement 0: half the junctions' lights looked wrong. Each group now carries the state
/// its own heads show (`SignalPlan::group_timelines`), and its time to change is the time
/// to that group's next change. A plan with no heads — nothing to draw — keeps the old
/// controller-wide entry under its plain id.
fn group_signal_plans(world: &v2xw_world::World) -> Vec<SignalPlan> {
    let mut approach_of: BTreeMap<LaneId, LaneId> = BTreeMap::new();
    for c in world.roads.connections() {
        if let Some(via) = c.via {
            approach_of.entry(via).or_insert(c.from_lane);
        }
    }
    let mut out = Vec::new();
    for plan in &world.signals {
        if plan.heads.is_empty() {
            out.push(SignalPlan {
                signal: SignalId::new(plan.id.index()),
                phases: plan
                    .phases
                    .iter()
                    .map(|p| {
                        (
                            p.states.first().copied().unwrap_or(SignalState::Off),
                            p.duration_s,
                        )
                    })
                    .collect(),
                cycle_s: plan.cycle_s,
                offset_s: plan.offset_s,
            });
            continue;
        }
        for (group, timeline) in plan.group_timelines(|l| approach_of.get(&l).copied()) {
            out.push(SignalPlan {
                signal: SignalId::new(v2xw_world::signal_group_wire_id(plan.id, group)),
                phases: timeline,
                cycle_s: plan.cycle_s,
                offset_s: plan.offset_s,
            });
        }
    }
    out
}

/// One signal controller's fixed-time plan, flattened for evaluation without the world.
#[derive(Debug, Clone)]
struct SignalPlan {
    signal: SignalId,
    /// `(dominant state, duration)` per phase, in plan order.
    phases: Vec<(SignalState, f64)>,
    cycle_s: f64,
    offset_s: f64,
}

impl SignalPlan {
    /// The phase at `t_s` and how long it has left, or `None` for a degenerate plan.
    fn at(&self, t_s: f64) -> Option<(SignalState, f64)> {
        if self.cycle_s <= 0.0 || self.phases.is_empty() {
            return None;
        }
        let mut into = (t_s - self.offset_s) % self.cycle_s;
        if into < 0.0 {
            into += self.cycle_s;
        }
        for (state, duration) in &self.phases {
            if into < *duration {
                return Some((*state, (*duration - into).max(0.0)));
            }
            into -= *duration;
        }
        self.phases.last().map(|(s, d)| (*s, *d))
    }
}

/// How many kernel threads exist in this process right now.
///
/// Counted by the thread itself — incremented as its body starts, decremented by a guard
/// as it returns, however it returns — so the number is the threads that are actually
/// alive, not the handles somebody remembered to drop. `run.status` publishes it, and the
/// lifecycle tests hold it to one: before the kernel had a cancellation point, every
/// restart left the previous kernel computing to its horizon beside the new one.
static KERNEL_THREADS: AtomicUsize = AtomicUsize::new(0);

/// How many kernel threads have ever been started in this process.
static KERNEL_THREADS_STARTED: AtomicUsize = AtomicUsize::new(0);

/// The number of kernel threads alive in this process.
pub fn kernel_threads() -> usize {
    KERNEL_THREADS.load(Ordering::SeqCst)
}

/// Decrements [`KERNEL_THREADS`] when the kernel thread's body ends, by any path.
struct KernelThreadGuard;

impl KernelThreadGuard {
    fn enter() -> Self {
        KERNEL_THREADS.fetch_add(1, Ordering::SeqCst);
        KERNEL_THREADS_STARTED.fetch_add(1, Ordering::SeqCst);
        KernelThreadGuard
    }
}

impl Drop for KernelThreadGuard {
    fn drop(&mut self) {
        KERNEL_THREADS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A handle on the thread the kernel runs on.
#[derive(Debug)]
struct Host {
    steps: Receiver<HostMsg>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Host {
    /// Tells the kernel to stop, without waiting for it.
    ///
    /// The kernel checks the flag between every two events (`RunRecorder::cancelled`), and
    /// a recorder blocked on a full channel checks it every 2 ms, so the thread ends within
    /// one event's work of this call.
    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stops the kernel and waits for its thread to end.
    ///
    /// The wait is bounded by one event's work plus one world import: a kernel still
    /// building its world when it is told to stop finishes the build, then sees the flag
    /// before its first event. Draining the channel while waiting is what keeps a recorder
    /// blocked on a full channel from waiting on us while we wait on it.
    fn shutdown(&mut self) {
        self.stop();
        if let Some(join) = self.join.take() {
            while !join.is_finished() {
                while self.steps.try_recv().is_ok() {}
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let _ = join.join();
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Joined, now that the kernel can be stopped: a dropped run must not leave a
        // simulation computing on its own thread (the defect `KERNEL_THREADS` exists to
        // catch).
        self.shutdown();
    }
}

/// The kernel's sink: groups records into mobility steps and hands each step over.
///
/// This is the whole of the step-boundary machinery. `v2xw_engine::Engine::run` dispatches
/// in `(time, priority, seq)` order, so the instant a record is emitted at never goes
/// backwards, and a step closes exactly when that instant crosses into the next one.
///
/// Steps with no records are emitted too, empty. That is not a detail: a scenario whose
/// demand model has not produced a vehicle yet emits nothing at all for its first steps,
/// and a stream that skipped them would open with a `Keyframe` at `t = 6.1 s` and a client
/// with no idea why.
struct StepRecorder {
    step_ns: u64,
    /// The step being accumulated, and its records.
    current: Option<(u64, Vec<OwnedRecord>)>,
    /// The last step index that was handed over, so the empty ones between can be filled.
    emitted_through: Option<u64>,
    /// The final step index of the run, from the scenario horizon.
    last_index: u64,
    tx: SyncSender<HostMsg>,
    stop: Arc<AtomicBool>,
    /// Set once the transport has gone away or asked to stop; the recorder then only feeds
    /// the recording, if there is one.
    closed: bool,
    recording: Option<v2xw_record::RecordingWriter<std::io::BufWriter<std::fs::File>>>,
    refused: u64,
}

impl StepRecorder {
    /// Hands `index` over as a complete step, with every empty step before it.
    fn hand_over(&mut self, index: u64, records: Vec<OwnedRecord>) {
        let first = self.emitted_through.map_or(0, |e| e + 1);
        for empty in first..index {
            if !self.send(RawStep {
                index: empty,
                records: Vec::new(),
            }) {
                return;
            }
        }
        let _ = self.send(RawStep { index, records });
    }

    /// Blocks until the transport takes the step. Returns `false` once closed.
    ///
    /// The block is the backpressure: a paused run stops draining, the channel fills, and
    /// the kernel stops inside this call. It reads no clock, so being stopped here is
    /// indistinguishable, to the run, from not having been started yet.
    fn send(&mut self, step: RawStep) -> bool {
        if self.closed {
            return false;
        }
        self.emitted_through = Some(step.index);
        let mut pending = HostMsg::Step(step);
        loop {
            if self.stop.load(Ordering::Relaxed) {
                self.closed = true;
                return false;
            }
            match self.tx.try_send(pending) {
                Ok(()) => return true,
                Err(TrySendError::Full(back)) => {
                    pending = back;
                    // A short park rather than a blocking `send`, so the stop flag is
                    // still checked while the transport is not draining. The kernel is
                    // idle here by construction; no simulated quantity depends on it.
                    std::thread::yield_now();
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.closed = true;
                    return false;
                }
            }
        }
    }

    /// Closes the last step and every empty step up to the horizon.
    fn finish(&mut self) {
        if let Some((index, records)) = self.current.take() {
            self.hand_over(index, records);
        }
        let first = self.emitted_through.map_or(0, |e| e + 1);
        for empty in first..=self.last_index {
            if !self.send(RawStep {
                index: empty,
                records: Vec::new(),
            }) {
                break;
            }
        }
        if let Some(writer) = self.recording.take() {
            let _ = writer.finish();
        }
    }
}

impl v2xw_engine::RunRecorder for StepRecorder {
    fn write(&mut self, at: SimTime, record: &OwnedRecord) {
        if let Some(writer) = &mut self.recording
            && v2xw_record::RecordingWriter::write_record(writer, at, record).is_err()
        {
            self.refused += 1;
        }
        if self.closed {
            return;
        }
        let index = at / self.step_ns.max(1);
        match &mut self.current {
            Some((current, records)) if *current == index => records.push(record.clone()),
            Some(_) => {
                let (current, records) = self.current.take().unwrap_or((index, Vec::new()));
                self.hand_over(current, records);
                self.current = Some((index, vec![record.clone()]));
            }
            None => self.current = Some((index, vec![record.clone()])),
        }
    }

    // Forwarded, not defaulted. `RunRecorder::write_wire_frame` discards by default so a
    // record-only recorder need not know the binary path exists, but a WRAPPER that
    // forwards `write` and not this one silently drops the normative binary stream and
    // nothing in the resulting file says so. That is exactly what happened here: every
    // run wrote zero keyframes and zero deltas while reporting success.
    fn write_wire_frame(&mut self, frame: &v2xw_record::wire::Frame) {
        if let Some(writer) = &mut self.recording {
            let _ = v2xw_record::RecordingWriter::write_frame(writer, frame);
        }
    }

    fn refused(&self) -> u64 {
        self.refused
    }

    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// Builds the kernel on a dedicated thread and streams its steps back.
///
/// The `Scenario` crosses the thread boundary; the engine does not, because it cannot
/// (see the module header). Returns once the engine is built, so a caller that gets an
/// `Ok` has a world, a `Hello` and a run id — and a caller that gets an `Err` has the
/// scenario's or the importer's own error rather than a server that is listening on a run
/// that failed to load.
fn spawn_host(
    scenario: Scenario,
    options: &LiveOptions,
    lookahead: usize,
    memo: Option<WorldMemo>,
) -> Result<(Box<Setup>, Host)> {
    let (setup_tx, setup_rx) =
        std::sync::mpsc::channel::<std::result::Result<Box<Setup>, String>>();
    let (step_tx, step_rx) = std::sync::mpsc::sync_channel::<HostMsg>(lookahead.max(1));
    let stop = Arc::new(AtomicBool::new(false));

    let build_utc = options.build_utc.clone();
    let actor_capacity = options.actor_capacity.max(1);
    let label = options.label.clone();
    let token = options.session_token.clone();
    // Naming an exporter makes the run record: every exporter reads the recording, so the
    // recording is written to `runs/<meta.name>/` (the command line's convention) when the
    // server was not given a path of its own.
    let exporters = scenario.exporters.clone();
    let recording = options.recording.clone().or_else(|| {
        (!exporters.is_empty()).then(|| {
            std::path::PathBuf::from("runs")
                .join(&scenario.meta.name)
                .join("recording.mcap")
        })
    });
    let host_stop = Arc::clone(&stop);
    let export_stop = Arc::clone(&stop);
    let join = std::thread::Builder::new()
        .name("v2xw-engine".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let _alive = KernelThreadGuard::enter();
            // The world: the one this process imported last time when every input of the
            // import is unchanged (the key covers them all, source file contents included),
            // a fresh import otherwise. Every Run is a fresh kernel, and before this every one
            // of them imported the map again — on Manhattan, most of the wait after Run.
            let key = v2xw_engine::wiring::world_cache_key(&scenario).ok();
            let kept = match (memo, key.as_ref()) {
                (Some(m), Some(k)) if &m.key == k => v2xw_world::serde_native::from_bytes(&m.bytes)
                    .ok()
                    .map(|w| (w, m.bytes)),
                _ => None,
            };
            let (world, bytes) = match kept {
                Some((world, bytes)) => (world, Some(bytes)),
                None => match v2xw_engine::wiring::build_world(&scenario) {
                    Ok(world) => {
                        let bytes = v2xw_world::serde_native::to_bytes(&world).ok();
                        (world, bytes)
                    }
                    Err(e) => {
                        let _ = setup_tx.send(Err(e.to_string()));
                        return;
                    }
                },
            };
            // `world.cache` names a directory to keep the world in across processes; a kept
            // world still belongs there, so it is written when the entry is missing.
            if let (Some(dir), Some(k), Some(b)) = (
                scenario
                    .world
                    .cache
                    .as_deref()
                    .map(str::trim)
                    .filter(|d| !d.is_empty()),
                key.as_ref(),
                bytes.as_ref(),
            ) {
                let entry = std::path::Path::new(dir).join(format!("{k}.v2xwworld"));
                if !entry.exists() && std::fs::create_dir_all(dir).is_ok() {
                    let partial = entry.with_extension(format!("{}.partial", std::process::id()));
                    if std::fs::write(&partial, b).is_ok()
                        && std::fs::rename(&partial, &entry).is_err()
                    {
                        let _ = std::fs::remove_file(&partial);
                    }
                }
            }
            let world_memo = key.zip(bytes).map(|(key, bytes)| WorldMemo { key, bytes });
            let mut engine =
                match v2xw_engine::Engine::build_with_world(scenario, world, &build_utc) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = setup_tx.send(Err(e.to_string()));
                        return;
                    }
                };
            let setup = match assemble_setup(
                &engine,
                &label,
                &token,
                recording.as_deref(),
                actor_capacity,
            ) {
                Ok(mut s) => {
                    s.world_memo = world_memo;
                    s
                }
                Err(e) => {
                    let _ = setup_tx.send(Err(e.to_string()));
                    return;
                }
            };
            let step_ns = setup.cadence.mobility_step.as_nanos().max(1);
            let last_index = setup.duration / step_ns;
            let writer = recording.as_deref().and_then(|path| {
                open_recording(path, setup.cadence, &setup.manifest, &setup.scenario_doc).ok()
            });
            if setup_tx.send(Ok(setup)).is_err() {
                return;
            }
            let mut recorder = StepRecorder {
                step_ns,
                current: None,
                emitted_through: None,
                last_index,
                tx: step_tx.clone(),
                stop: host_stop,
                closed: false,
                recording: writer,
                refused: 0,
            };
            let outcome = engine.run(&mut recorder);
            recorder.finish();
            // The exporters, on this thread so the transport stays responsive, and only for a
            // run that reached its end: a stopped run's recording is a fragment.
            if outcome.is_ok()
                && !exporters.is_empty()
                && !export_stop.load(Ordering::SeqCst)
                && let Some(path) = recording.as_deref()
            {
                let out_dir = path.parent().unwrap_or(std::path::Path::new("."));
                let result = v2xw_engine::export::run_exporters(&exporters, path, out_dir)
                    .map_err(|e| e.to_string());
                let _ = step_tx.send(HostMsg::Exported(result));
            }
            let _ = match outcome {
                Ok(report) => step_tx.send(HostMsg::Done(Box::new(report))),
                Err(e) => step_tx.send(HostMsg::Failed(e.to_string())),
            };
        })
        .map_err(|e| ServerError::Io {
            path: "v2xw-engine thread".to_string(),
            errno: e.to_string(),
        })?;

    let setup = match setup_rx.recv() {
        Ok(Ok(setup)) => setup,
        Ok(Err(message)) => return Err(ServerError::Internal(message)),
        Err(_) => {
            return Err(ServerError::Internal(
                "the engine thread ended before it reported a run".to_string(),
            ));
        }
    };
    Ok((
        setup,
        Host {
            steps: step_rx,
            stop,
            join: Some(join),
        },
    ))
}

/// The run-scoped facts a `Hello` is built from, for the run `setup` describes.
fn descriptor_of(setup: &Setup) -> RunDescriptor {
    RunDescriptor {
        run_id: crate::stub::uuid_string(&setup.run_id_bytes),
        run_id_bytes: setup.run_id_bytes,
        hello: setup.hello.clone(),
        cadence: setup.cadence,
        origin_m: setup.origin_m,
        duration: setup.duration,
        live: true,
        // §3.1.2 sets `HELLO_SEEKABLE` on a live run "once ≥ 1 keyframe is recorded".
        // The first keyframe is step 0, which is produced before any client can have
        // connected, so the flag is set from the start rather than flipped later —
        // which it could not be anyway, because `Run` snapshots this descriptor per run.
        seekable: true,
        scenario: setup.scenario_doc.clone(),
        scenario_hash_hex: setup.scenario_hash_hex.clone(),
        recording_path: setup.recording_path.clone(),
        provenance: Some(setup.provenance.clone()),
    }
}

/// Opens the MCAP recording a live run writes, declaring the cadence and attaching the
/// scenario, exactly as `v2xw run` does.
fn open_recording(
    path: &std::path::Path,
    cadence: Cadence,
    manifest: &Value,
    scenario: &Value,
) -> Result<v2xw_record::RecordingWriter<std::io::BufWriter<std::fs::File>>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ServerError::Io {
            path: parent.display().to_string(),
            errno: e.to_string(),
        })?;
    }
    let mut writer = v2xw_record::RecordingWriter::create(
        path,
        v2xw_record::RecordingOptions {
            cadence,
            profile: v2xw_record::Profile::Full,
            ..v2xw_record::RecordingOptions::default()
        },
    )?;
    writer.write_manifest(&serde_json::to_string_pretty(manifest).unwrap_or_default())?;
    writer.attach(
        "scenario.json",
        "application/json",
        serde_json::to_string_pretty(scenario)
            .unwrap_or_default()
            .as_bytes(),
    )?;
    Ok(writer)
}

/// The model-card schema's `family` enum as §3.8's index, in declaration order.
///
/// `Family` is `#[non_exhaustive]`-free but closed by policy (ADR 0007), and it publishes
/// no index of its own, so the mapping is written out. A variant added without updating
/// this table is a compile error, which is the point of the exhaustive match.
const fn family_code(family: Family) -> u16 {
    match family {
        Family::World => 0,
        Family::Mobility => 1,
        Family::Vru => 2,
        Family::Weather => 3,
        Family::Gnss => 4,
        Family::Clock => 5,
        Family::Propagation => 6,
        Family::Fading => 7,
        Family::Obstacle => 8,
        Family::Phy => 9,
        Family::Mac => 10,
        Family::Dcc => 11,
        Family::Net => 12,
        Family::Fragmenter => 13,
        Family::Backhaul => 14,
        Family::Cellular => 15,
        Family::BackendNet => 16,
        Family::Codec => 17,
        Family::Generator => 18,
        Family::Envelope => 19,
        Family::Primitive => 20,
        Family::CryptoBackend => 21,
        Family::VerificationPolicy => 22,
        Family::SafetyApp => 23,
        Family::Protocol => 24,
        Family::ServiceModel => 25,
        Family::HardwareProfile => 26,
        Family::Perception => 27,
        Family::Attacker => 28,
        Family::Detector => 29,
        Family::MaPipeline => 30,
        Family::Responder => 31,
        Family::Metric => 32,
        Family::Exporter => 33,
    }
}

/// `subject_kind` of §3.8 for a family: what a value produced by it is *about*.
const fn subject_kind(family: Family) -> u16 {
    match family {
        Family::Metric => 4,
        Family::Propagation | Family::Fading | Family::Obstacle | Family::Phy => 2,
        Family::World => 6,
        Family::Mobility | Family::Vru | Family::Attacker => 3,
        _ => 1,
    }
}

/// Everything the `Hello`, the manifest and the introspection answers are built from,
/// read off the kernel once it exists.
fn assemble_setup(
    engine: &v2xw_engine::Engine,
    label: &str,
    session_token: &str,
    recording: Option<&std::path::Path>,
    actor_capacity: u32,
) -> Result<Box<Setup>> {
    let world = engine.world();
    let scenario = engine.scenario();
    let payload = v2xw_world::serde_vwp::write(world)?;
    let world_json = v2xw_world::serde_vwp::to_json_string(world)?;
    let manifest = engine.manifest();

    let mobility_step = scenario.time.mobility_step();
    let cadence = Cadence::new(Duration::from_millis(1000), mobility_step)?;
    let duration = scenario.time.horizon_ns();

    let scenario_doc: Value = serde_json::from_str(
        &scenario
            .to_json()
            .map_err(|e| ServerError::Internal(e.to_string()))?,
    )
    .map_err(|e| ServerError::Internal(e.to_string()))?;
    let scenario_hash_hex = scenario
        .content_hash()
        .map_err(|e| ServerError::Internal(e.to_string()))?;
    let manifest_json: Value = serde_json::from_str(
        &manifest
            .to_json_pretty()
            .map_err(|e| ServerError::Internal(e.to_string()))?,
    )
    .map_err(|e| ServerError::Internal(e.to_string()))?;

    // The run id is a function of what the run *is*: the scenario digest and the seed.
    // There is no clock and no randomness to draw a UUIDv7 from, and a run id that
    // changed between two identical runs would make a recording unidentifiable.
    let run_id_bytes = run_id_of(&scenario_hash_hex, scenario.seed);

    let mut strings = StrTable::new();
    let str_engine_version = strings.intern(&format!(
        "v2xw {} ({})",
        v2xw_engine::manifest::ENGINE_VERSION,
        v2xw_engine::manifest::GIT_COMMIT
    ));
    let str_scenario_name = strings.intern(&scenario.meta.name);
    let str_run_label = strings.intern(label);
    // Interned before the session needs it, so the §2.5 table the client builds has the
    // size this module assumed when it numbered the §3.8 extension.
    let str_session_token = strings.intern(session_token);
    let str_url = strings.intern(&payload.url_path());

    let mut class_names = Vec::with_capacity(VehicleClass::ALL.len());
    let classes: Vec<ClassRow> = VehicleClass::ALL
        .iter()
        .map(|class| {
            let spec = class.spec();
            class_names.push(class.as_str().to_string());
            ClassRow {
                str_name: strings.intern(class.as_str()),
                length_m: spec.length_m as f32,
                width_m: spec.width_m as f32,
                height_m: spec.height_m as f32,
                color_rgba: class_colour(*class),
                category: class_category(*class),
            }
        })
        .collect();

    let channels: Vec<ChannelRow> = v2xw_record::CHANNELS
        .iter()
        .filter_map(|c| c.wire_id.map(|id| (c, id)))
        .map(|(c, id)| ChannelRow {
            str_id: strings.intern(c.name),
            channel_id: id,
            visibility: crate::visibility_code(c.visibility),
            enabled: 0,
        })
        .collect();

    // The metric catalogue, from the same provider set the run installed, so a name the
    // stream can carry is a name the symbol table already holds (§2.5 is append-only and
    // a `MetricSample` has nowhere to put an extension).
    let mut registry = v2xw_core::registry::Registry::new();
    v2xw_engine::wiring::register_all(&mut registry)
        .map_err(|e| ServerError::Internal(e.to_string()))?;
    let providers = v2xw_engine::wiring::build_metrics(scenario, &mut registry)
        .map_err(|e| ServerError::Internal(e.to_string()))?;
    // One catalogue row per *series* the stream can carry, each interned now: §2.5's table
    // is append-only and a `MetricSample` has nowhere to put an extension, so every name a
    // sample can be sent under has to be in the table before the first frame. A metric is
    // its headline series; a distribution adds its three percentiles; a metric that
    // declares a breakdown adds one series per declared value (see `metric_series`).
    let mut catalogue: Vec<MetricInfo> = Vec::new();
    let mut metric_aliases: Vec<(String, String)> = Vec::new();
    for def in providers.catalog() {
        let visibility = crate::visibility_name(def.visibility).to_string();
        let source = def
            .source
            .as_ref()
            .map(|s| s.reference.clone())
            .unwrap_or_default();
        for series in metric_series(&def) {
            match series {
                Series::Alias { from, to } => metric_aliases.push((from, to)),
                Series::Row { name, agg, note } => catalogue.push(MetricInfo {
                    str_id: strings.intern(&name),
                    agg_code: agg_code(&agg),
                    name,
                    unit: def.unit.clone(),
                    agg,
                    visibility: visibility.clone(),
                    definition_md: if note.is_empty() {
                        def.definition_md.clone()
                    } else {
                        format!("{note} {}", def.definition_md)
                    },
                    dims: def.dims.iter().map(ToString::to_string).collect(),
                    not_accounted: def.not_accounted.clone(),
                    source: source.clone(),
                    base: def.name.clone(),
                }),
            }
        }
    }

    // §3.8: one provenance entry per registered model, so every `prov_id` the stream
    // references resolves to a real card rather than to a fixture's invention.
    let mut extension = StrTable {
        strings: Vec::new(),
    };
    let base = u32::try_from(strings.strings.len()).unwrap_or(0);
    let mut ext_id = |s: &str| base + extension.intern(s);
    let mut entries = Vec::new();
    let mut prov_chain = Vec::new();
    let mut metric_prov = 0u32;
    for (i, (_, model)) in engine.registry().iter_by_id().enumerate() {
        let prov_id = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let card = &model.card;
        if card.family == Family::Metric && metric_prov == 0 {
            metric_prov = prov_id;
        }
        prov_chain.push(json!({
            "prov_id": prov_id,
            "model_id": card.id,
            "model_version": card.version,
            "family": card.family.to_string(),
            "param_set_id": format!("sha256:{}", &model.content_hash_hex()[..16]),
            "card_url": format!("/cards/{}", card.id.replace('/', "-")),
            "card_hash": model.content_hash_hex(),
            "tier": card.tier.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "purpose": card.purpose,
        }));
        entries.push(ProvEntry {
            prov_id,
            str_model_id: ext_id(&card.id),
            str_model_version: ext_id(&card.version),
            // The parameter set is identified by the card's own content hash: this build
            // binds parameters at registration, so the card *is* the parameter set. A
            // build with live parameter overrides must put its `ParamSetId` here instead.
            str_param_set_id: ext_id(&format!("sha256:{}", &model.content_hash_hex()[..16])),
            str_card_url: ext_id(&format!("/cards/{}", card.id.replace('/', "-"))),
            family: family_code(card.family),
            subject_kind: subject_kind(card.family),
        });
    }
    let provenance = ProvenanceBody {
        sim_time_ns: 0,
        entries,
        dims: Vec::new(),
        strings: Some(extension),
        flags: PROV_FINAL,
    };

    let signals = group_signal_plans(&world);

    let bbox = world.bbox;
    let mut scenario_hash = [0u8; 32];
    for (i, byte) in hex_bytes(&scenario_hash_hex)
        .into_iter()
        .take(32)
        .enumerate()
    {
        scenario_hash[i] = byte;
    }
    let hello = HelloBody {
        version_minor: 0,
        hello_flags: HELLO_LIVE | HELLO_SEEKABLE,
        run_id: run_id_bytes,
        scenario_hash,
        world_hash: payload.content_hash,
        t0_wall_ns: i64::try_from(engine.wall_clock().unix_nanos_at(0)).unwrap_or(0),
        sim_duration_ns: duration,
        mobility_step_ns: mobility_step.as_nanos(),
        keyframe_period_ns: cadence.keyframe_period.as_nanos(),
        telemetry_period_ns: 1_000_000_000,
        metric_period_ns: 1_000_000_000,
        resume_seq: 0,
        sim_time_ns: 0,
        origin_lat_deg: world.origin.lat_deg,
        origin_lon_deg: world.origin.lon_deg,
        origin_alt_m: world.origin.alt_m,
        bbox_m: [bbox.min.x, bbox.min.y, bbox.max.x, bbox.max.y],
        actor_capacity,
        nodes: Vec::new(),
        classes,
        channels,
        world_ref: WorldRef {
            mode: 0,
            format: 0,
            payload_bytes: u32::try_from(payload.bytes.len()).unwrap_or(u32::MAX),
            str_url,
        },
        str_engine_version,
        str_scenario_name,
        str_run_label,
        str_session_token,
        strings,
    };

    Ok(Box::new(Setup {
        world_memo: None,
        world_payload: payload,
        world_json,
        hello,
        cadence,
        origin_m: [bbox.min.x.floor(), bbox.min.y.floor(), 0.0],
        duration,
        scenario_doc,
        scenario_hash_hex,
        manifest: manifest_json,
        provenance,
        prov_chain,
        catalogue,
        metric_aliases,
        class_names,
        signals,
        equipped_fraction: scenario.actors.vehicles.equipped_fraction,
        vru_device_fraction: scenario.actors.vru.device_fraction,
        roadside_nodes: roadside_node_count(scenario),
        actor_capacity,
        seed: scenario.seed,
        run_id_bytes,
        obu_profile: scenario.nodes.default_obu.clone(),
        recording_path: recording.map(|p| p.display().to_string()),
        metric_prov,
    }))
}

/// How many node ids the kernel hands out before the first vehicle's.
///
/// A roadside unit is a node and **not** an actor: `v2xw_engine::Engine::create_rsus` runs
/// at build time, before the timeline is seeded, and it takes its ids from the same counter
/// vehicle spawn takes them from. So a scenario with `n` roadside units puts them at
/// `0..n` and its first vehicle at `n`.
///
/// This is the second published fact the actor→node reconstruction rests on, and it was
/// the one the projector did not model: counting from zero shifted every vehicle's node id
/// by the roadside count, which made the whole mapping wrong for any scenario with a mast
/// in it — silently, because the node ids still existed and still looked dense.
fn roadside_node_count(scenario: &Scenario) -> u32 {
    u32::try_from(scenario.actors.rsus.len()).unwrap_or(u32::MAX)
}

/// The 16 run-id bytes, stamped with the UUIDv7 version and variant nibbles (RFC 9562).
fn run_id_of(scenario_hash_hex: &str, seed: u64) -> [u8; 16] {
    let digest = v2xw_core::hash::sha256(format!("v2xw/run/{scenario_hash_hex}/{seed}").as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out[6] = (out[6] & 0x0F) | 0x70;
    out[8] = (out[8] & 0x3F) | 0x80;
    out
}

/// Parses a lower-case hex digest into bytes; a malformed digit reads as zero.
fn hex_bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
            let lo = pair
                .get(1)
                .and_then(|c| (*c as char).to_digit(16))
                .unwrap_or(0) as u8;
            (hi << 4) | lo
        })
        .collect()
}

/// One series a metric contributes to the stream's catalogue.
enum Series {
    /// A row of its own: a name to intern, its aggregation tag, and a note on what the
    /// series is, prefixed to the metric's definition.
    Row {
        name: String,
        agg: String,
        note: String,
    },
    /// A second spelling of another series.
    Alias { from: String, to: String },
}

/// The series one metric is streamed as.
///
/// The headline (no dimension); for a distribution, its p50, p95 and p99; and for a metric
/// that declares a breakdown (`MetricDef::breakdown`), one `name[value]` series per
/// declared value — or, when it declares exactly one value, an alias of the headline. A
/// metric declared `breakdown_only` has no headline (every sample carries its value), so
/// it offers only the per-value series rather than a row that would never receive data.
fn metric_series(def: &v2xw_metrics::MetricDef) -> Vec<Series> {
    let mut out = Vec::new();
    let distribution = matches!(def.agg, v2xw_metrics::Agg::Distribution);
    if !def.breakdown_only {
        out.push(Series::Row {
            name: def.name.clone(),
            agg: if distribution {
                "mean".to_string()
            } else {
                def.agg.tag()
            },
            note: if distribution {
                "Mean over the window.".to_string()
            } else {
                String::new()
            },
        });
    }
    if distribution && !def.breakdown_only {
        for q in ["p50", "p95", "p99"] {
            out.push(Series::Row {
                name: format!("{}.{q}", def.name),
                agg: q.to_string(),
                note: format!("The {q} (type-7 quantile) over the window."),
            });
        }
    }
    if let Some((dim, values)) = def.breakdown.as_ref() {
        if values.len() == 1 {
            out.push(Series::Alias {
                from: format!("{}[{}]", def.name, values[0]),
                to: def.name.clone(),
            });
        } else {
            for v in values {
                out.push(Series::Row {
                    name: format!("{}[{v}]", def.name),
                    agg: if matches!(def.agg, v2xw_metrics::Agg::Distribution) {
                        "mean".to_string()
                    } else {
                        def.agg.tag()
                    },
                    note: format!("For {dim} = {v}."),
                });
            }
        }
    }
    out
}

/// `MetricAgg` of §3.7 for an aggregation's tag.
fn agg_code(tag: &str) -> u16 {
    match tag {
        "sum" => 0,
        "mean" => 1,
        "p50" => 2,
        "p95" => 3,
        "p99" => 4,
        "ratio" => 5,
        "rate" => 6,
        "max" => 7,
        "min" => 8,
        // `count` and `distribution` have no §3.7 code; a count is a sum of ones and is
        // reported as one, which is what the exported table's `agg` column also says.
        _ => 0,
    }
}

/// The §3.1.4 `category` for a class: `0` vehicle, `1` VRU, `2` infrastructure.
const fn class_category(class: VehicleClass) -> u8 {
    match class {
        VehicleClass::Bicycle | VehicleClass::Pedestrian | VehicleClass::Scooter => 1,
        _ => 0,
    }
}

/// A renderer hint. Nothing depends on it and it is not a measurement.
const fn class_colour(class: VehicleClass) -> u32 {
    match class {
        VehicleClass::Passenger => 0x4C_9A_FF_FF,
        VehicleClass::Emergency => 0xFF_4C_4C_FF,
        VehicleClass::Delivery => 0xF2_C0_4C_FF,
        VehicleClass::Truck | VehicleClass::Trailer => 0xE0_7B_39_FF,
        VehicleClass::Bus | VehicleClass::Coach => 0xF2_D0_5C_FF,
        VehicleClass::Motorcycle | VehicleClass::Moped => 0xB4_7B_FF_FF,
        VehicleClass::Bicycle | VehicleClass::Scooter => 0x5F_D3_9B_FF,
        VehicleClass::Pedestrian => 0xFF_8A_A8_FF,
    }
}

// --- the projector -----------------------------------------------------------------

/// One actor as the record stream describes it.
#[derive(Debug, Clone)]
struct LiveActor {
    class_idx: u8,
    node: Option<NodeId>,
    pos_m: [f64; 3],
    heading_rad: f64,
    speed_mps: f64,
    accel_mps2: f64,
    lane: Option<LaneId>,
    /// The last step this actor published kinematics in.
    last_step: u64,
}

/// What a node's own records say about it over the current telemetry window.
///
/// Every field starts absent, and an absent field is written as §3.5.2's *unknown*
/// sentinel rather than as a zero. That distinction is the whole value of the frame: a
/// client that reads `verifications_per_s = 0` is told the node verified nothing, and a
/// client that reads the sentinel is told the server does not know — which is the truth
/// for every counter this build's node runtime does not publish.
#[derive(Debug, Clone, Default)]
struct WindowCounters {
    tx_msgs: u32,
    tx_airtime_us: u64,
    full_cert_msgs: u32,
    rx_attempts: u32,
    rx_ok: u32,
    verifies: u32,
    verify_waits_ns: Vec<u64>,
    cbr: Option<f64>,
    dcc_state: Option<u16>,
    tx_power_cdbm: Option<i16>,
    certs_seen: u32,
    /// A `node.telemetry` record, if the engine ever emits one: it wins outright.
    reported: Option<NodeTelemetryView>,
}

/// How many reception attempts per node pair the projector keeps for `inspect.link`.
const LINK_HISTORY: usize = 512;

/// One reception attempt, kept so `inspect.link` can answer from measurements.
#[derive(Debug, Clone, Copy)]
struct LinkObservation {
    t: SimTime,
    rssi_dbm: Option<f64>,
    sinr_db: Option<f64>,
    dist_m: Option<f64>,
    received: bool,
}

/// Turns the kernel's record stream into the scene and the frames of §3.
#[derive(Debug)]
struct Projector {
    step_ns: u64,
    steps_per_second: u64,
    class_index: BTreeMap<String, u8>,
    slots: SlotAllocator,
    actors: BTreeMap<ActorId, LiveActor>,
    /// node → actor, as reconstructed. See the module header.
    nodes: BTreeMap<NodeId, ActorId>,
    rng: v2xw_core::rng::RngRegistry,
    equipped_fraction: f64,
    /// `actors.vru.device_fraction`: what the kernel draws a pedestrian's or a cyclist's
    /// device with, instead of `equipped_fraction`.
    vru_device_fraction: f64,
    /// The node ids `0..roadside_nodes`, which the kernel gave to masts and not to actors.
    /// No reconstruction is owed for them and none is possible: a roadside unit has no
    /// `gt.kinematics` record because it has no actor.
    roadside_nodes: u32,
    next_node: u32,
    /// Every node id the reconstruction has assigned, kept after the actor retires.
    ///
    /// Separate from `nodes`, which is dropped on retirement so that a pose or an
    /// `inspect` lookup answers about live actors only. A node id the kernel never reuses
    /// stays explained here, so a record that trails its actor's last kinematics row is
    /// not reported as an unreconstructed node.
    assigned_nodes: BTreeSet<u32>,
    signals: Vec<SignalPlan>,
    /// Interned strings from the `Hello` table, for the payloads that carry a string id.
    str_ids: BTreeMap<String, u32>,
    /// metric name → (string id, `MetricAgg`, visibility code).
    metric_ids: BTreeMap<String, (u32, u16, u8)>,
    metric_prov: u32,
    counters: BTreeMap<NodeId, WindowCounters>,
    /// §3.1.1's `actor_capacity`, enforced: a slot at or beyond it is never sent.
    actor_capacity: u32,
    /// Actors refused a slot because the run is at `actor_capacity`.
    over_capacity: BTreeSet<u32>,
    /// Node ids the records named that the reconstruction has not accounted for.
    ///
    /// An id is *removed* again when the reconstruction reaches it, because a node
    /// transmits in the same mobility step it spawns in while its actor's first
    /// `gt.kinematics` row is stamped one step later — the mobility phase publishes the
    /// state of the instant it is dispatched at, and a spawn's state is the state of the
    /// *next* instant. So the first frame of every equipped vehicle names a node whose
    /// actor the projector has not seen yet, and treating that as a disagreement reported
    /// every run as inconsistent. What is left here at the end of a run is what the
    /// reconstruction never explained, which is the disagreement worth reporting.
    unmapped_nodes: BTreeSet<u32>,
    /// Metric names the stream carried that the symbol table does not hold.
    unnamed_metrics: BTreeSet<String>,
    /// Per node, the most recent frames it put on the air and the most recent receptions
    /// it resolved, as the evidence `inspect.node`'s `messages` section shows for a
    /// followed vehicle. Bounded at [`MESSAGE_LOG`] each.
    messages: BTreeMap<
        NodeId,
        (
            std::collections::VecDeque<Value>,
            std::collections::VecDeque<Value>,
        ),
    >,
    /// Channels seen in the stream that this projector has no §3.6 payload for.
    unprojected_channels: BTreeSet<String>,
    /// Channels whose records the channel's own reader-side view could not decode.
    ///
    /// This is separate from `unprojected_channels`, and the separation is the point: a
    /// channel with no arm here is a gap in *this* module, while a channel whose records
    /// its declared view refuses is a disagreement between the producer and the reader —
    /// the defect `v2xw-engine::records` exists to prevent, caught only when someone
    /// actually decodes. Counting it is what stops it from being a silently empty stream.
    undecodable_channels: BTreeMap<String, u64>,
    /// Reception attempts per `(tx, rx)` node pair, for `inspect.link` (§6.8). Bounded:
    /// the newest [`LINK_HISTORY`] observations of each pair, which is what an averaging
    /// window over the recent past needs and all it needs.
    links: BTreeMap<(u32, u32), std::collections::VecDeque<LinkObservation>>,
    last_index: u64,
}

impl Projector {
    fn new(setup: &Setup) -> Self {
        let class_index = setup
            .class_names
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), u8::try_from(i).unwrap_or(0)))
            .collect();
        let str_ids = setup
            .hello
            .strings
            .strings
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), u32::try_from(i).unwrap_or(0)))
            .collect();
        let mut metric_ids: BTreeMap<String, (u32, u16, u8)> = setup
            .catalogue
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    (m.str_id, m.agg_code, visibility_code_of(&m.visibility)),
                )
            })
            .collect();
        for (from, to) in &setup.metric_aliases {
            if let Some(ids) = metric_ids.get(to).copied() {
                metric_ids.insert(from.clone(), ids);
            }
        }
        let step_ns = setup.cadence.mobility_step.as_nanos().max(1);
        Projector {
            step_ns,
            steps_per_second: (1_000_000_000 / step_ns).max(1),
            class_index,
            slots: SlotAllocator::new(setup.cadence.keyframe_period),
            actors: BTreeMap::new(),
            nodes: BTreeMap::new(),
            rng: v2xw_core::rng::RngRegistry::new(setup.seed),
            equipped_fraction: setup.equipped_fraction,
            vru_device_fraction: setup.vru_device_fraction,
            roadside_nodes: setup.roadside_nodes,
            // Not zero: the masts hold `0..roadside_nodes` (see `roadside_node_count`).
            next_node: setup.roadside_nodes,
            assigned_nodes: BTreeSet::new(),
            signals: setup.signals.clone(),
            str_ids,
            metric_ids,
            metric_prov: setup.metric_prov,
            counters: BTreeMap::new(),
            actor_capacity: setup.actor_capacity,
            over_capacity: BTreeSet::new(),
            unmapped_nodes: BTreeSet::new(),
            unnamed_metrics: BTreeSet::new(),
            messages: BTreeMap::new(),
            unprojected_channels: BTreeSet::new(),
            undecodable_channels: BTreeMap::new(),
            links: BTreeMap::new(),
            last_index: setup.duration / step_ns,
        }
    }

    /// The string id of `s`, or `0` (the mandatory empty string) when the table has none.
    fn str_id(&self, s: &str) -> u32 {
        self.str_ids.get(s).copied().unwrap_or(0)
    }

    /// Assigns the node the kernel would have given this actor, if it equips it.
    ///
    /// **This is the reconstruction the module header names**, and all three of its facts
    /// are here: the draw is keyed by `(RngDomain::Spawn, EntityRef::Actor)` so it does not
    /// depend on spawn order; node ids are dense and ascending in spawn order *from
    /// `roadside_nodes`*, because the masts took the counter's first values at build time;
    /// and the id counts as accounted for the moment it is handed out, even if a record
    /// named it a step earlier. The caller equips a step's new actors in `ActorId` order,
    /// which is spawn order, because the kernel assigns `ActorId`s ascending at spawn.
    fn equip(&mut self, actor: ActorId, vru: bool) -> Option<NodeId> {
        let fraction = if vru {
            self.vru_device_fraction
        } else {
            self.equipped_fraction
        };
        let equipped = self
            .rng
            .checkout(
                v2xw_core::rng::RngDomain::Spawn,
                v2xw_core::rng::EntityRef::Actor(actor),
            )
            .bool(fraction);
        if !equipped {
            return None;
        }
        let node = NodeId::new(self.next_node);
        self.next_node += 1;
        self.nodes.insert(node, actor);
        // The id is now explained, whether or not a record has already named it: see
        // `unmapped_nodes` for why a record can arrive one step ahead of the actor.
        self.assigned_nodes.insert(node.index());
        self.unmapped_nodes.remove(&node.index());
        Some(node)
    }

    /// True if every node the records named is one the reconstruction accounted for.
    ///
    /// Asked at the end of a run this is the question it reads as. Asked in the middle it
    /// may also be answering "not yet", because an equipped vehicle's first frame precedes
    /// its first `gt.kinematics` row by one mobility step; `explain`'s caveat says so with
    /// the ids, which is the honest form of a mid-run answer.
    fn mapping_is_consistent(&self) -> bool {
        self.unmapped_nodes.is_empty()
    }

    /// Projects one raw step into the state a connection encodes its frames from.
    fn project(&mut self, raw: &RawStep) -> StepOutput {
        let t: SimTime = raw.index.saturating_mul(self.step_ns);
        let mut events: Vec<EventEntry> = Vec::new();
        let mut metrics: Vec<MetricRow> = Vec::new();
        let mut kinematics: Vec<GtKinematicsView> = Vec::new();
        let mut transmitting: BTreeSet<NodeId> = BTreeSet::new();

        // Two passes, and the order is load-bearing. `gt.kinematics` is what creates an
        // actor and therefore what assigns its node, and a node transmits in the *same*
        // step it spawns in: reading `node.tx` first would find a node id the actor table
        // does not hold yet and report every run's first transmission as an
        // unreconstructed node.
        for record in &raw.records {
            if record.channel == "gt.kinematics"
                && let Ok(view) = decode::<GtKinematicsView>(record)
            {
                events.push(EventEntry {
                    sim_time_ns: t,
                    channel_id: 1,
                    payload: gt_kinematics_payload(&view),
                });
                kinematics.push(view);
            }
        }
        if !kinematics.is_empty() {
            self.absorb(raw.index, t, &kinematics);
        }

        for record in &raw.records {
            match record.channel {
                "gt.kinematics" => {}
                "node.tx" => match decode::<NodeTxView>(record) {
                    Err(_) => self.undecodable(record.channel),
                    Ok(view) => {
                        self.note_node(view.node);
                        transmitting.insert(view.node);
                        let counters = self.counters.entry(view.node).or_default();
                        counters.tx_msgs += 1;
                        counters.tx_airtime_us += view.airtime_us.unwrap_or(0);
                        if view.signer == Some(SignerId::Certificate) {
                            counters.full_cert_msgs += 1;
                        }
                        if let Some(power) = view.power_dbm {
                            counters.tx_power_cdbm = Some((power * 100.0).round() as i16);
                        }
                        self.log_message(view.node, true, sent_json(&view));
                        events.push(EventEntry {
                            sim_time_ns: t,
                            channel_id: 10,
                            payload: node_tx_payload(&view),
                        });
                    }
                },
                "phy.rx" => match decode::<PhyRxView>(record) {
                    Err(_) => self.undecodable(record.channel),
                    Ok(view) => {
                        self.note_node(view.rx);
                        if let Some(tx) = view.tx {
                            self.note_node(tx);
                        }
                        let counters = self.counters.entry(view.rx).or_default();
                        counters.rx_attempts += 1;
                        if view.outcome == RxOutcome::Ok {
                            counters.rx_ok += 1;
                        }
                        if let Some(tx) = view.tx {
                            let key = (tx.index(), view.rx.index());
                            let history = self.links.entry(key).or_default();
                            if history.len() >= LINK_HISTORY {
                                history.pop_front();
                            }
                            history.push_back(LinkObservation {
                                t: view.t_end,
                                rssi_dbm: view.rssi_dbm,
                                sinr_db: view.sinr_db,
                                dist_m: view.dist_m,
                                received: view.outcome == RxOutcome::Ok,
                            });
                        }
                        events.push(EventEntry {
                            sim_time_ns: t,
                            channel_id: 11,
                            payload: phy_rx_payload(&view),
                        });
                    }
                },
                "node.verify" => match decode::<NodeVerifyView>(record) {
                    Err(_) => self.undecodable(record.channel),
                    Ok(view) => {
                        self.note_node(view.node);
                        let counters = self.counters.entry(view.node).or_default();
                        counters.verifies += 1;
                        if let (Some(start), done) = (view.t_start, view.t_done) {
                            let _ = done;
                            counters
                                .verify_waits_ns
                                .push(start.saturating_sub(view.t_enqueue));
                        }
                        events.push(EventEntry {
                            sim_time_ns: t,
                            channel_id: 14,
                            payload: node_verify_payload(&view),
                        });
                    }
                },
                "mac.cbr" => match decode::<MacCbrView>(record) {
                    Err(_) => self.undecodable(record.channel),
                    Ok(view) => {
                        self.note_node(view.node);
                        let counters = self.counters.entry(view.node).or_default();
                        counters.cbr = Some(view.cbr);
                        events.push(EventEntry {
                            sim_time_ns: t,
                            channel_id: 12,
                            payload: mac_cbr_payload(&view),
                        });
                    }
                },
                "sec.cert" => match decode::<SecCertView>(record) {
                    Err(_) => self.undecodable(record.channel),
                    Ok(view) => {
                        self.note_node(view.node);
                        let counters = self.counters.entry(view.node).or_default();
                        counters.certs_seen += 1;
                        events.push(EventEntry {
                            sim_time_ns: t,
                            channel_id: 20,
                            payload: sec_cert_payload(&view),
                        });
                    }
                },
                "det.observation" => match decode::<DetObservationView>(record) {
                    Err(_) => self.undecodable(record.channel),
                    Ok(view) => {
                        self.note_node(view.node);
                        let detector = self.str_id(&view.detector);
                        let subject = self
                            .nodes
                            .get(&view.node)
                            .map_or(U32_NONE, |actor| actor.index());
                        events.push(EventEntry {
                            sim_time_ns: t,
                            channel_id: 30,
                            payload: det_observation_payload(
                                &view,
                                detector,
                                subject,
                                self.metric_prov,
                            ),
                        });
                    }
                },
                "proto.revocation" => match decode::<ProtoRevocationView>(record) {
                    Err(_) => self.undecodable(record.channel),
                    Ok(view) => {
                        events.push(EventEntry {
                            sim_time_ns: t,
                            channel_id: 22,
                            payload: revocation_payload(&view),
                        });
                    }
                },
                "node.telemetry" => match decode::<NodeTelemetryView>(record) {
                    Err(_) => self.undecodable(record.channel),
                    Ok(view) => {
                        let node = view.node;
                        self.note_node(node);
                        self.counters.entry(node).or_default().reported = Some(view);
                    }
                },
                // One reception attempt's fate, a backend flow's latency trace and a
                // transfer's byte accounting are recording and metric channels: the stream
                // carries what they measure as metric samples, not as §3.6 payloads.
                "node.rx" => {
                    // The evidence is what the radio decoded: a delivery, or a loss above the
                    // PHY. The PHY's own losses — most of them a distant frame below
                    // sensitivity — are on `phy.rx` and in the link view.
                    if let Ok(view) = decode::<NodeRxView>(record)
                        && (view.outcome == v2xw_metrics::channels::RxFate::Delivered
                            || !view
                                .cause
                                .as_deref()
                                .is_some_and(v2xw_metrics::channels::rx_cause::is_phy))
                    {
                        self.log_message(view.rx, false, received_json(&view));
                    }
                }
                "msg.latency" | "net.bytes" => {}
                "metric.sample" => match serde_json::from_slice::<MetricSample>(&record.json) {
                    Ok(sample) => self.push_metric(&sample, &mut metrics),
                    Err(_) => self.undecodable(record.channel),
                },
                other => {
                    self.unprojected_channels.insert(other.to_string());
                }
            }
        }

        // The scene. A step that published kinematics decided who exists (above); a step
        // that published none carries the previous set forward, because `gt.despawn` is in
        // the channel table and nothing in this build emits it.
        let mut poses: Vec<ActorPose> = Vec::with_capacity(self.actors.len());
        for (actor, live) in &self.actors {
            let Some(slot) = self.slots.slot_of(*actor) else {
                continue;
            };
            let mut state = 0u8;
            if let Some(node) = live.node {
                state |= ST_EQUIPPED;
                if transmitting.contains(&node) {
                    state |= ST_TRANSMITTING;
                }
            }
            poses.push(ActorPose {
                slot,
                actor: *actor,
                node: live.node,
                pos_m: body_centre(live.pos_m, live.heading_rad, live.class_idx),
                heading_rad: live.heading_rad,
                speed_mps: live.speed_mps,
                accel_mps2: live.accel_mps2,
                lane: live.lane,
                class_idx: live.class_idx,
                state,
                // The node's neighbour table is in `ObuRuntime` and is not on any channel
                // this build emits, so the count is not known here. Zero is the §3.3.2
                // encoding and the honest one: no neighbour is *known* to be verified.
                verified_neighbors: 0,
            });
        }
        let mut snapshot = Snapshot::new(t, poses);
        snapshot.signals = self.signals_at(t);

        let telemetry = if raw.index > 0 && raw.index % self.steps_per_second == 0 {
            self.flush_telemetry()
        } else {
            Vec::new()
        };

        events.sort_by_key(|e| (e.sim_time_ns, e.channel_id));
        StepOutput {
            sim_time: t,
            snapshot,
            telemetry,
            events,
            metrics,
            provenance: None,
            end_of_run: raw.index >= self.last_index,
            recorded: Vec::new(),
            generation: 0,
        }
    }

    /// Appends one message to a node's evidence log, dropping the oldest past the bound.
    fn log_message(&mut self, node: NodeId, sent: bool, entry: Value) {
        let (tx, rx) = self.messages.entry(node).or_default();
        let log = if sent { tx } else { rx };
        if log.len() >= MESSAGE_LOG {
            log.pop_front();
        }
        log.push_back(entry);
    }

    /// Counts one record its channel's own reader-side view refused.
    fn undecodable(&mut self, channel: &str) {
        *self
            .undecodable_channels
            .entry(channel.to_string())
            .or_insert(0) += 1;
    }

    /// Records that a node id appeared in the stream, and whether it was predicted.
    fn note_node(&mut self, node: NodeId) {
        // A mast is a node the kernel minted at build time and no actor carries, so there
        // is nothing to reconstruct and nothing to disagree about.
        if node.index() < self.roadside_nodes {
            return;
        }
        if self.assigned_nodes.contains(&node.index()) {
            return;
        }
        self.unmapped_nodes.insert(node.index());
    }

    /// Takes the step's kinematics as the authoritative actor set.
    fn absorb(&mut self, index: u64, t: SimTime, kinematics: &[GtKinematicsView]) {
        let mut present: BTreeSet<ActorId> = BTreeSet::new();
        let mut fresh: Vec<ActorId> = Vec::new();
        let mut vru: BTreeSet<ActorId> = BTreeSet::new();
        for view in kinematics {
            present.insert(view.actor);
            if !self.actors.contains_key(&view.actor) {
                fresh.push(view.actor);
            }
            if matches!(view.class.as_deref(), Some("pedestrian" | "bicycle")) {
                vru.insert(view.actor);
            }
        }
        // Spawn order is `ActorId` order, and the node ids the kernel hands out are dense
        // and ascending in it. Sorting here rather than trusting the record order is what
        // makes the reconstruction independent of the mobility provider's iteration.
        fresh.sort_unstable();
        for actor in fresh {
            // The equipped draw happens for every actor whether or not it gets a slot, so
            // that a run at capacity still assigns the node ids the kernel assigned: the
            // draw is what keeps the reconstruction aligned with the kernel's own.
            // The kernel draws a pedestrian's or a cyclist's device with
            // `actors.vru.device_fraction` (v2xw_engine::wiring::is_vru_class), so the
            // reconstruction does too; drawing them with the vehicles' fraction minted
            // node ids the kernel never did and shifted every later vehicle's.
            let node = self.equip(actor, vru.contains(&actor));
            let slot = self.slots.allocate(actor, t);
            if slot >= self.actor_capacity {
                // §3.1.1: the client refuses a slot at or beyond `actor_capacity`, so
                // sending one would stop it applying the frame at all. The actor is kept
                // in the table — it still exists, and its node still transmits — but it
                // holds no slot and therefore no pose reaches the wire. It is counted, and
                // `over_capacity` reaches a client through the caveats rather than the
                // actor silently not being drawn.
                self.slots.release(actor, t);
                self.over_capacity.insert(actor.index());
            }
            self.actors.insert(
                actor,
                LiveActor {
                    class_idx: 0,
                    node,
                    pos_m: [0.0, 0.0, 0.0],
                    heading_rad: 0.0,
                    speed_mps: 0.0,
                    accel_mps2: 0.0,
                    lane: None,
                    last_step: index,
                },
            );
        }
        for view in kinematics {
            let class_idx = view
                .class
                .as_deref()
                .and_then(|c| self.class_index.get(c).copied())
                .unwrap_or(0);
            if let Some(live) = self.actors.get_mut(&view.actor) {
                live.class_idx = class_idx;
                live.pos_m = [view.x_m, view.y_m, view.z_m.unwrap_or(0.0)];
                live.heading_rad = view.heading_rad.unwrap_or(live.heading_rad);
                live.speed_mps = view.speed_mps;
                live.accel_mps2 = view.acc_mps2.unwrap_or(0.0);
                live.lane = view.lane.map(LaneId::new);
                live.last_step = index;
            }
        }
        let gone: Vec<ActorId> = self
            .actors
            .keys()
            .copied()
            .filter(|a| !present.contains(a))
            .collect();
        for actor in gone {
            if let Some(live) = self.actors.remove(&actor)
                && let Some(node) = live.node
            {
                self.nodes.remove(&node);
                self.counters.remove(&node);
            }
            self.slots.release(actor, t);
        }
    }

    /// The signal states of §3.3.3, from the world's own fixed-time plans.
    ///
    /// The kernel schedules no `Event::SignalPhase` in this build, so what a client sees
    /// here is the plan the world was imported with, evaluated at `t` — real scenario
    /// data, but a plan and not a controller: nothing in the run reads it back.
    fn signals_at(&self, t: SimTime) -> Vec<WireSignal> {
        let t_s = (t as f64) * 1e-9;
        self.signals
            .iter()
            .filter_map(|plan| {
                let (state, remaining) = plan.at(t_s)?;
                Some(WireSignal {
                    signal: plan.signal,
                    phase: movement_phase(state),
                    time_to_change: Some(Duration::from_nanos((remaining * 1e9) as u64)),
                })
            })
            .collect()
    }

    /// Turns the window's counters into one `NodeTelemetry` per node and clears them.
    fn flush_telemetry(&mut self) -> Vec<NodeTelemetry> {
        let mut out = Vec::with_capacity(self.counters.len());
        let nodes: Vec<NodeId> = self.counters.keys().copied().collect();
        for node in nodes {
            let Some(counters) = self.counters.get_mut(&node) else {
                continue;
            };
            let mut row = NodeTelemetry::unknown(node.index());
            if let Some(reported) = &counters.reported {
                // The node published its own report, which is the only source that can
                // fill the fields a record stream cannot see. Take it whole.
                apply_reported(&mut row, reported);
            }
            row.msgs_out_per_s = counters.tx_msgs as f32;
            row.msgs_in_per_s = counters.rx_ok as f32;
            row.verifications_per_s = counters.verifies as f32;
            row.full_cert_msgs = counters.full_cert_msgs;
            row.airtime_ms_per_s = (counters.tx_airtime_us as f32) * 1e-3;
            if !counters.verify_waits_ns.is_empty() {
                let mut waits = core::mem::take(&mut counters.verify_waits_ns);
                waits.sort_unstable();
                row.verify_wait_p50_ms = percentile_ms(&waits, 0.50);
                row.verify_wait_p95_ms = percentile_ms(&waits, 0.95);
            }
            if let Some(cbr) = counters.cbr {
                row.cbr_pm = (cbr * 1000.0).round().clamp(0.0, 65534.0) as u16;
            }
            if let Some(dcc) = counters.dcc_state {
                row.dcc_state = dcc;
            }
            if let Some(power) = counters.tx_power_cdbm {
                row.tx_power_cdbm = power;
            }
            if counters.certs_seen > 0 {
                row.cert_stored = counters.certs_seen;
            }
            // §3.5.2's `node_state`: the run only produces records for a node that is
            // running, so `2` (active) is what has been observed. A node the scenario
            // turned off stops appearing and keeps its last row.
            row.node_state = 2;
            out.push(row.quantised());
            *counters = WindowCounters {
                reported: counters.reported.clone(),
                ..WindowCounters::default()
            };
        }
        out.sort_by_key(|r| r.node_id);
        out
    }

    /// Appends the §3.7 rows one metric sample produces.
    ///
    /// A sample is sent under the series its dimensions name (see `metric_series`): no
    /// dimension is the headline, plus the three percentiles of a distribution; one
    /// declared breakdown dimension is `name[value]`. A per-node sample is not a series —
    /// one plot line per node would be thousands of lines — and the metric's headline
    /// carries the across-node figure. A sample under no interned series is counted in
    /// `unnamed_metrics` rather than sent under a name the client cannot resolve, and
    /// before this every dimensioned sample was sent under its bare name, so a metric's
    /// plot interleaved its distance bins, its causes and its nodes into one zig-zag line.
    fn push_metric(&mut self, sample: &MetricSample, out: &mut Vec<MetricRow>) {
        if sample.dims.iter().any(|(d, _)| d.to_string() == "node") {
            return;
        }
        let dims: Vec<String> = sample
            .dims
            .iter()
            .filter(|(d, _)| d.to_string() != "t")
            .map(|(_, v)| v.to_string())
            .collect();
        let mut rows: Vec<(String, Option<f64>)> = Vec::new();
        match dims.as_slice() {
            [] => {
                rows.push((sample.metric.clone(), sample.value.point()));
                if let v2xw_metrics::SampleValue::Distribution(d) = &sample.value {
                    for (suffix, q) in [
                        ("p50", v2xw_metrics::Percentile::P50),
                        ("p95", v2xw_metrics::Percentile::P95),
                        ("p99", v2xw_metrics::Percentile::P99),
                    ] {
                        rows.push((format!("{}.{suffix}", sample.metric), d.quantile(q)));
                    }
                }
            }
            [value] => rows.push((format!("{}[{value}]", sample.metric), sample.value.point())),
            _ => return,
        }
        for (series, value) in rows {
            let Some((str_metric, agg, visibility)) = self.metric_ids.get(&series).copied() else {
                // A breakdown the metric does not declare is left to the recording and
                // `metrics.json` on purpose. Only a metric the table does not know at all is
                // the defect `unnamed_metrics` exists to surface.
                if !self.metric_ids.contains_key(&sample.metric) {
                    self.unnamed_metrics.insert(sample.metric.clone());
                }
                continue;
            };
            // An insufficient sample is a refusal, not a zero, and §3.7 has no encoding for
            // one. Dropping it is right: the client sees a gap, which is what
            // "insufficient" means.
            let Some(value) = value else { continue };
            out.push(MetricRow {
                value,
                str_metric,
                // The dimension is in the series name, so the row carries none; `0` is
                // §3.7's "no dimensions".
                dim_key: 0,
                node_id: U32_NONE,
                count: u32::try_from(sample.value.n()).unwrap_or(u32::MAX),
                agg,
                visibility,
                prov_id: self.metric_prov,
            });
        }
        out.sort_by_key(|r| (r.str_metric, r.node_id));
        out.dedup_by_key(|r| (r.str_metric, r.node_id));
    }
}

/// How many sent and how many received messages a node's evidence log keeps.
///
/// The log is filled as steps are absorbed, which is up to [`LiveOptions::lookahead_steps`]
/// ahead of the stream plus the host channel's own lookahead, and read at the stream's
/// instant, so it has to hold that lead and some history behind it: 384 frames is 38 s of
/// a 10 Hz sender, and 384 receptions about 13 s at the 30 a second a dense street gives.
const MESSAGE_LOG: usize = 384;

/// One transmitted frame as a followed vehicle's evidence: what it was and every octet
/// of it by layer, and when it was generated, signed and put on the air.
fn sent_json(v: &NodeTxView) -> Value {
    json!({
        "t_ns": v.t,
        "msg": v.msg,
        "msg_type": v.msg_type,
        "bytes_on_wire": v.bytes_on_wire,
        "payload_bytes": v.payload_bytes,
        "envelope_bytes": v.envelope_bytes,
        "cert_bytes": v.cert_bytes,
        "net_header_bytes": v.net_header_bytes,
        "link_bytes": v.link_bytes,
        "airtime_us": v.airtime_us,
        "power_dbm": v.power_dbm,
        "signer": v.signer,
        "t_generated_ns": v.t_generated,
        "t_signed_ns": v.t_signed,
        "channel": v.channel,
        "pseudonym": v.pseudonym,
        "content": v.content,
    })
}

/// One reception as a followed vehicle's evidence: from whom, what it measured, what
/// became of it, and — when it reached an application — its delay stage by stage, in ms.
fn received_json(v: &NodeRxView) -> Value {
    let stages: serde_json::Map<String, Value> = v
        .latency_trace()
        .map(|t| {
            t.stage_ns()
                .into_iter()
                .map(|(k, ns)| (k, json!((ns as f64) / 1e6)))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "t_ns": v.t,
        "msg": v.msg,
        "from": v.tx,
        "msg_type": v.msg_type,
        "outcome": v.outcome,
        "cause": v.cause,
        "verification": v.verification,
        "rssi_dbm": v.rssi_dbm,
        "sinr_db": v.sinr_db,
        "dist_m": v.dist_m,
        "bytes_on_wire": v.bytes_on_wire,
        "e2e_ms": v.e2e_ns().map(|ns| (ns as f64) / 1e6),
        "stages_ms": stages,
    })
}

/// `MovementPhaseState` (SAE J2735) for a world signal state.
const fn movement_phase(state: SignalState) -> u8 {
    match state {
        SignalState::Red => 3,
        SignalState::RedAmber => 4,
        SignalState::Amber => 8,
        SignalState::Green => 6,
        SignalState::GreenYield => 5,
        SignalState::FlashingAmber => 7,
        SignalState::Off => 0,
    }
}

/// The nearest-rank percentile of a sorted nanosecond list, in milliseconds.
///
/// Nearest rank rather than interpolation: the samples are integers on a known grid, and
/// an interpolated value would be a number no observation took.
fn percentile_ms(sorted: &[u64], q: f64) -> f32 {
    if sorted.is_empty() {
        return f32::NAN;
    }
    let rank = ((sorted.len() as f64) * q).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    (sorted[index] as f32) * 1e-6
}

/// Copies a node's own published telemetry into the wire row, field by field.
///
/// `node.telemetry` carries six fields; §3.5.2 has fifty-one. The rest stay at their
/// unknown sentinels, which is the difference between "the node reports no certificate
/// store" and "this build does not publish one".
fn apply_reported(row: &mut NodeTelemetry, view: &NodeTelemetryView) {
    if let Some(bytes) = view.storage_bytes {
        row.storage_used_b = bytes;
    }
    if let Some(bytes) = view.ram_bytes {
        row.ram_used_kib = u32::try_from(bytes / 1024).unwrap_or(u32::MAX);
    }
    if let Some(cpu) = view.cpu {
        row.cpu_util_pm = (cpu * 1000.0).round().clamp(0.0, 65534.0) as u16;
    }
    if let Some(hsm) = view.hsm {
        row.hsm_util_pm = (hsm * 1000.0).round().clamp(0.0, 65534.0) as u16;
    }
    if let Some(depth) = view.verify_queue_depth {
        row.q_verify_p50 = u16::try_from(depth).unwrap_or(u16::MAX);
        row.q_verify_p95 = u16::try_from(depth).unwrap_or(u16::MAX);
    }
    // The queues' own window percentiles, when the node published its window.
    if let Some([p50, p95]) = view.q_rx {
        (row.q_rx_p50, row.q_rx_p95) = (p50, p95);
    }
    if let Some([p50, p95]) = view.q_verify {
        (row.q_verify_p50, row.q_verify_p95) = (p50, p95);
    }
    if let Some([p50, p95]) = view.q_app {
        (row.q_app_p50, row.q_app_p95) = (p50, p95);
    }
    if let Some([p50, p95]) = view.q_tx {
        (row.q_tx_p50, row.q_tx_p95) = (p50, p95);
    }
    if let Some([p50, p95]) = view.q_crl {
        (row.q_crl_p50, row.q_crl_p95) = (p50, p95);
    }
    if let Some(ms) = view.verify_wait_p95_ms {
        row.verify_wait_p95_ms = v2xw_record::grid::quantise_f32(ms, 1e-3);
    }
    if let Some(n) = view.cert_active {
        row.cert_active = n;
    }
    if let Some(n) = view.nbr_total {
        row.nbr_total = n;
    }
    if let Some(n) = view.nbr_verified {
        row.nbr_verified = n;
    }
}

/// The §6.5 `Visibility` code for a token.
fn visibility_code_of(name: &str) -> u8 {
    match name {
        "NODE" => 1,
        "PUBLIC" => 2,
        "MIXED" => 3,
        "DERIVED" => 4,
        "META" => 5,
        _ => 0,
    }
}

// --- §3.6 payload encoders ---------------------------------------------------------
//
// One function per channel, writing the exact byte layout §3.6 names, from the reader-side
// view of the record the kernel emitted. A field the record does not carry is written as
// that field's §0 sentinel: `0xFFFF_FFFF` for a `u32` id, `0xFF` for a `u8`, `NaN` for a
// float. Nothing here invents a value.

fn put_u32(out: &mut [u8], at: usize, v: u32) {
    out[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(out: &mut [u8], at: usize, v: u64) {
    out[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(out: &mut [u8], at: usize, v: f32) {
    out[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u16(out: &mut [u8], at: usize, v: u16) {
    out[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_i16(out: &mut [u8], at: usize, v: i16) {
    out[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

/// A message id narrowed to the `u32` the wire carries, keeping `U32_NONE` free.
fn msg_id_of(msg: Option<u64>) -> u32 {
    match msg {
        Some(m) => u32::try_from(m % u64::from(U32_NONE)).unwrap_or(0),
        None => U32_NONE,
    }
}

/// §3.6.3's `MsgType` for the kernel's own spelling of it.
fn msg_type_code(name: Option<&str>) -> u16 {
    match name {
        Some("bsm") => 1,
        Some("cam") => 2,
        Some("denm") => 3,
        Some("spat") => 4,
        Some("map") => 5,
        Some("psm") => 6,
        Some("vam") => 7,
        Some("cpm") => 8,
        Some("srm") => 9,
        Some("ssm") => 10,
        Some("wsa") => 11,
        Some("crl") => 12,
        _ => 0,
    }
}

/// §3.6.11 `gt.kinematics`, 56 bytes.
fn gt_kinematics_payload(view: &GtKinematicsView) -> Vec<u8> {
    let mut p = vec![0u8; 56];
    put_u32(&mut p, 0, view.actor.index());
    put_u32(&mut p, 4, view.lane.unwrap_or(U32_NONE));
    put_f32(&mut p, 8, view.x_m as f32);
    put_f32(&mut p, 12, view.y_m as f32);
    put_f32(&mut p, 16, view.z_m.unwrap_or(0.0) as f32);
    // The record carries ground speed and heading, not the velocity vector; the components
    // are the published decomposition of the two and not a second source.
    let heading = view.heading_rad.unwrap_or(0.0);
    put_f32(
        &mut p,
        20,
        (view.speed_mps * v2xw_core::math::cos(heading)) as f32,
    );
    put_f32(
        &mut p,
        24,
        (view.speed_mps * v2xw_core::math::sin(heading)) as f32,
    );
    put_f32(&mut p, 28, 0.0);
    put_f32(&mut p, 32, view.acc_mps2.unwrap_or(0.0) as f32);
    put_f32(&mut p, 36, 0.0);
    put_f32(&mut p, 40, 0.0);
    put_f32(&mut p, 44, heading as f32);
    put_f32(&mut p, 48, f32::NAN);
    put_f32(&mut p, 52, view.lane_pos_m.unwrap_or(0.0) as f32);
    p
}

/// §3.6.4 `node.tx`, 40 bytes.
fn node_tx_payload(view: &NodeTxView) -> Vec<u8> {
    let mut p = vec![0u8; 40];
    put_u32(&mut p, 0, view.node.index());
    put_u32(&mut p, 4, msg_id_of(view.msg));
    put_u32(
        &mut p,
        8,
        u32::try_from(view.bytes_on_wire).unwrap_or(u32::MAX),
    );
    put_f32(
        &mut p,
        12,
        view.airtime_us.map_or(f32::NAN, |us| (us as f32) * 1e-3),
    );
    put_u16(&mut p, 16, msg_type_code(view.msg_type.as_deref()));
    put_i16(
        &mut p,
        18,
        view.power_dbm
            .map_or(i16::MIN, |dbm| (dbm * 100.0).round() as i16),
    );
    put_u16(&mut p, 20, view.channel.unwrap_or(0xFFFF));
    p[22] = view.mcs.unwrap_or(0xFF);
    p[23] = view.ac.unwrap_or(0xFF);
    p[24] = match view.dcc_state.as_deref() {
        Some("unrestricted") | Some("UNRESTRICTED") => 0,
        Some("active") => 1,
        Some("restrictive") => 2,
        _ => 0xFF,
    };
    p[25] = match view.signer {
        Some(SignerId::Digest) => 0,
        Some(SignerId::Certificate) => 1,
        Some(SignerId::SelfSigned) => 2,
        None => 0xFF,
    };
    put_u16(
        &mut p,
        26,
        view.payload_bytes
            .and_then(|b| u16::try_from(b).ok())
            .unwrap_or(0xFFFF),
    );
    // §3.6.10's `pseudonym_digest`: the HashedId8 of the signing certificate, which
    // `node.tx` now carries (it rotates on the air when the node changes pseudonym, and
    // the page's HUD shows it). Eight zero bytes, §0's "absent", when a record has none.
    if let Some(hex) = view.pseudonym.as_deref()
        && hex.len() == 16
    {
        for (i, byte) in p[28..36].iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0);
        }
    }
    put_u32(&mut p, 36, 0);
    p
}

/// §3.6.5 `phy.rx`, 48 bytes.
fn phy_rx_payload(view: &PhyRxView) -> Vec<u8> {
    let mut p = vec![0u8; 48];
    put_u64(&mut p, 0, view.t_start);
    put_u64(&mut p, 8, view.t_end);
    put_u32(&mut p, 16, view.rx.index());
    put_u32(&mut p, 20, view.tx.map_or(U32_NONE, |n| n.index()));
    put_u32(&mut p, 24, msg_id_of(view.msg));
    put_f32(&mut p, 28, view.rssi_dbm.unwrap_or(f64::NAN) as f32);
    put_f32(&mut p, 32, view.sinr_db.unwrap_or(f64::NAN) as f32);
    put_f32(&mut p, 36, view.dist_m.unwrap_or(f64::NAN) as f32);
    p[40] = match view.outcome {
        RxOutcome::Ok => 0,
        // §3.6.5's outcome codes name the *mechanism*; the kernel reports `lost` plus a
        // cause, and the cause is what picks the code. An unrecognised cause reads as
        // `5` (sinr-fail), which is the class a link-budget decision belongs to.
        RxOutcome::Lost => match view.cause.as_deref() {
            Some("per") | Some("fading") | Some("sinr") => 5,
            Some("below-sensitivity") | Some("path-loss") => 1,
            Some("collision") | Some("interference") => 2,
            Some("capture") => 3,
            Some("half-duplex") => 4,
            Some("crc") => 6,
            _ => 5,
        },
    };
    p[41] = match view.cause.as_deref() {
        None => 0,
        Some("path-loss") | Some("below-sensitivity") => 1,
        Some("shadowing") => 2,
        Some("per") | Some("fading") => 3,
        Some("interference") | Some("collision") => 4,
        Some("hidden-terminal") => 5,
        Some("half-duplex") => 6,
        Some("dcc-gate") => 7,
        Some("queue-drop") => 8,
        Some("out-of-range") => 9,
        Some(_) => 0,
    };
    // Line-of-sight class: `v2xw-engine` evaluates every link as `LosResult::clear()`
    // because no obstacle model is composed in this build, so `0` (LOS) is what the run
    // actually used — not a guess, and not the `0xFF` that would claim it is unknown.
    p[42] = 0;
    p
}

/// §3.6.6 `node.verify`, 48 bytes.
fn node_verify_payload(view: &NodeVerifyView) -> Vec<u8> {
    let mut p = vec![0u8; 48];
    put_u64(&mut p, 0, view.t_enqueue);
    put_u64(&mut p, 8, view.t_start.unwrap_or(u64::MAX));
    put_u64(&mut p, 16, view.t_done.unwrap_or(u64::MAX));
    put_u32(&mut p, 24, view.node.index());
    put_u32(&mut p, 28, msg_id_of(view.msg));
    put_f32(&mut p, 32, view.cost_us.map_or(f32::NAN, |us| us as f32));
    put_u16(
        &mut p,
        36,
        match view.primitive.as_deref() {
            Some("ecdsa-p256-verify") | Some("ecdsa-p256") => 1,
            Some("ecdsa-p256-sign") => 2,
            Some("ecdsa-p384-verify") => 3,
            Some("ml-dsa-65-verify") => 4,
            Some("ml-dsa-65-sign") => 5,
            Some("falcon-512-verify") => 6,
            Some("slh-dsa-shake-128s-verify") => 7,
            Some("ecqv-p256-reconstruct") => 8,
            Some("sha-256") => 9,
            Some("aes-128-ccm") => 10,
            _ => 0,
        },
    );
    p[38] = match view.outcome {
        VerifyOutcome::Valid => 0,
        VerifyOutcome::Invalid => 1,
        VerifyOutcome::Skipped => 5,
        VerifyOutcome::Dropped => 6,
    };
    p[39] = match view.policy.as_deref() {
        Some("now") | Some("admit") => 0,
        Some("deferred") => 1,
        Some("skipped") => 2,
        Some("evicted") => 3,
        _ => 0xFF,
    };
    p[40] = 0;
    // `where_run`: this build charges every verification against the node's HSM, which is
    // what the hardware profile's service rate describes.
    p[41] = 1;
    put_u16(
        &mut p,
        42,
        view.queue_depth
            .and_then(|d| u16::try_from(d).ok())
            .unwrap_or(0xFFFF),
    );
    p
}

/// §3.6.13 `mac.cbr`, 16 bytes.
fn mac_cbr_payload(view: &MacCbrView) -> Vec<u8> {
    let mut p = vec![0u8; 16];
    put_u32(&mut p, 0, view.node.index());
    put_f32(&mut p, 4, view.cbr as f32);
    put_u16(&mut p, 8, view.channel.unwrap_or(0xFFFF));
    put_u16(&mut p, 10, 0);
    put_i16(&mut p, 12, i16::MIN);
    p
}

/// §3.6.7 `sec.cert`, 40 bytes.
fn sec_cert_payload(view: &SecCertView) -> Vec<u8> {
    let mut p = vec![0u8; 40];
    put_u64(&mut p, 0, view.t);
    put_u64(&mut p, 8, u64::MAX);
    put_u32(&mut p, 16, view.node.index());
    put_u32(&mut p, 20, 0);
    if let Some(digest) = &view.digest {
        for (i, byte) in hex_bytes(digest).into_iter().take(8).enumerate() {
            p[24 + i] = byte;
        }
    }
    p[32] = match view.event.as_str() {
        "change" => 0,
        "expire" => 1,
        "topup-request" | "top-up" => 2,
        "topup-complete" => 3,
        "learn-p2pcd" | "learn" => 4,
        "learn-full-cert" => 5,
        "install" => 6,
        "evict" => 7,
        "revoked-self-detected" => 8,
        _ => 0,
    };
    p[33] = 0;
    put_u16(&mut p, 34, 0xFFFF);
    put_u16(&mut p, 36, 0xFFFF);
    put_u16(&mut p, 38, 1);
    p
}

/// §3.6.8 `det.observation`, 32 bytes.
fn det_observation_payload(
    view: &DetObservationView,
    str_detector: u32,
    subject_actor: u32,
    prov_id: u32,
) -> Vec<u8> {
    let mut p = vec![0u8; 32];
    put_u32(&mut p, 0, view.node.index());
    put_u32(&mut p, 4, str_detector);
    for (i, byte) in hex_bytes(&view.subject).into_iter().take(8).enumerate() {
        p[8 + i] = byte;
    }
    put_f32(&mut p, 16, view.score.unwrap_or(f64::NAN) as f32);
    put_u32(&mut p, 20, subject_actor);
    put_u16(&mut p, 24, 0);
    p[26] = 0;
    put_u32(&mut p, 28, prov_id);
    p
}

/// §3.6.10 `proto.revocation`, 32 bytes.
fn revocation_payload(view: &ProtoRevocationView) -> Vec<u8> {
    let mut p = vec![0u8; 32];
    put_u32(&mut p, 0, view.node.map_or(U32_NONE, |n| n.index()));
    put_u32(
        &mut p,
        4,
        u32::try_from(
            v2xw_core::hash::sha256(view.id.as_bytes())[..4]
                .iter()
                .fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
                % u64::from(U32_NONE),
        )
        .unwrap_or(0),
    );
    for (i, byte) in hex_bytes(&view.id).into_iter().take(8).enumerate() {
        p[8 + i] = byte;
    }
    put_u64(&mut p, 16, view.size_bytes.unwrap_or(0));
    put_u32(&mut p, 24, view.node.map_or(U32_NONE, |n| n.index()));
    p[28] = match view.stage.as_str() {
        "detect" => 0,
        "report_sent" => 1,
        "report_received" => 2,
        "decision" => 3,
        "resolved" => 4,
        "issued" => 5,
        "published" => 6,
        "downloaded" => 7,
        "processed" => 8,
        "enforced" => 9,
        "residual_harm" => 10,
        _ => 0,
    };
    p[29] = 0;
    put_u16(
        &mut p,
        30,
        view.entries
            .and_then(|e| u16::try_from(e).ok())
            .unwrap_or(0xFFFF),
    );
    p
}

// --- the engine --------------------------------------------------------------------

/// A live `v2xw-engine` run, served over VWP.
///
/// Holds the kernel at arm's length — on its own thread, behind a bounded channel — and
/// turns what it emits into the state a connection encodes frames from. See the module
/// header for why the arm's length is a requirement and not a preference.
#[derive(Debug)]
pub struct LiveEngine {
    scenario: Scenario,
    options: LiveOptions,
    descriptor: RunDescriptor,
    world: Arc<WorldPayload>,
    world_json: String,
    setup: Box<Setup>,
    projector: Projector,
    host: Host,
    /// Every step produced and not yet dropped, oldest first. This is the "recorded time"
    /// §6.6 lets a live run seek backwards into.
    timeline: std::collections::VecDeque<StepOutput>,
    /// The step index of `timeline.front()`.
    base_index: u64,
    /// The next step index the stream will emit.
    cursor: u64,
    /// One past the highest step index produced.
    produced: u64,
    state: RunState,
    speed: f64,
    client_sync: bool,
    report: Option<RunReport>,
    failure: Option<String>,
    /// Every metric sample produced, by name, for `metrics.query` (§6.12).
    history: BTreeMap<String, Vec<(SimTime, f64)>>,
    /// string id → metric name, for reading a produced row back.
    metric_names: BTreeMap<u32, String>,
    /// The most recent telemetry row per node, for `inspect.node` (§6.8).
    last_telemetry: BTreeMap<u32, NodeTelemetry>,
    /// Every string this run has appended to the symbol table, in append order.
    ///
    /// Append-only even across a despawn: see [`Engine::live_strings`]. It is the labels
    /// of every node the run has ever streamed, plus the one hardware-profile id.
    appended_strings: Vec<String>,
    /// Membership test for `appended_strings`, so the append is O(log n) and not O(n).
    appended_index: BTreeSet<String>,
    /// The scenario the next `run.start` runs, when `scenario.set` or `scenario.load` put
    /// one in place (§6.6 "or the already-set one").
    staged: Option<Scenario>,
    /// Where the scenario this engine was opened on came from, for resolving presets.
    source: Option<std::path::PathBuf>,
    /// A running SHA-256 over every step the kernel produced, in order: the run's output
    /// digest. Two runs with the same scenario and seed agree on it and two runs with
    /// different seeds do not, which is the reproducibility claim made checkable.
    digest: sha2::Sha256,
    /// How many steps the digest covers.
    digest_steps: u64,
    /// Set once the digest covers the whole run.
    digest_final: Option<String>,
    /// How many runs this engine has started, counting the first.
    runs_started: u64,
    /// Whole-run totals, accumulated as the kernel produces steps.
    stats: RunStats,
    /// What the scenario's exporters wrote after the run, or why they could not.
    exports: Option<std::result::Result<Vec<v2xw_engine::export::Exported>, String>>,
    /// The last imported world, for the next run's kernel (see [`WorldMemo`]).
    world_memo: Option<WorldMemo>,
}

/// Whole-run totals, read off the steps the kernel produced — not off what a connection
/// subscribed to — so they describe the run and not one viewer of it.
///
/// They are what makes a changed setting checkable from outside: a changed arrival rate
/// moves `actors_seen`, a changed propagation model moves `mean_rssi_dbm`, a changed message
/// set moves `tx_by_type`. Published under `run.status.engine.stats`.
#[derive(Debug, Default, Clone)]
struct RunStats {
    /// Distinct actors that appeared in any step.
    actors: BTreeSet<u32>,
    /// Frames put on the air, by message family.
    tx_by_type: BTreeMap<&'static str, u64>,
    /// Sum of the transmit power of the frames that reported one, dBm.
    tx_power_sum_dbm: f64,
    /// How many frames reported a transmit power.
    tx_power_n: u64,
    /// Reception attempts evaluated, and how many decoded.
    rx_attempts: u64,
    rx_ok: u64,
    /// Sum of the received power of the attempts that reported one, dBm.
    rssi_sum_dbm: f64,
    rssi_n: u64,
}

impl RunStats {
    fn absorb(&mut self, out: &StepOutput) {
        for pose in &out.snapshot.actors {
            self.actors.insert(pose.actor.index());
        }
        let tx = v2xw_record::channels::by_name("node.tx").and_then(|c| c.wire_id);
        let rx = v2xw_record::channels::by_name("phy.rx").and_then(|c| c.wire_id);
        for event in &out.events {
            let p = &event.payload;
            if Some(event.channel_id) == tx && p.len() >= 20 {
                let code = u16::from_le_bytes([p[16], p[17]]);
                *self.tx_by_type.entry(msg_type_name(code)).or_insert(0) += 1;
                let centi = i16::from_le_bytes([p[18], p[19]]);
                if centi != i16::MIN {
                    self.tx_power_sum_dbm += f64::from(centi) / 100.0;
                    self.tx_power_n += 1;
                }
            } else if Some(event.channel_id) == rx && p.len() >= 41 {
                self.rx_attempts += 1;
                if p[40] == 0 {
                    self.rx_ok += 1;
                }
                let rssi = f32::from_le_bytes([p[28], p[29], p[30], p[31]]);
                if rssi.is_finite() {
                    self.rssi_sum_dbm += f64::from(rssi);
                    self.rssi_n += 1;
                }
            }
        }
    }

    fn to_json(&self) -> Value {
        let mean = |sum: f64, n: u64| (n > 0).then(|| sum / n as f64);
        json!({
            "actors_seen": self.actors.len(),
            "tx_frames": self.tx_by_type.values().sum::<u64>(),
            "tx_by_type": self.tx_by_type,
            "mean_tx_power_dbm": mean(self.tx_power_sum_dbm, self.tx_power_n),
            "rx_attempts": self.rx_attempts,
            "rx_ok": self.rx_ok,
            "mean_rssi_dbm": mean(self.rssi_sum_dbm, self.rssi_n),
        })
    }
}

/// The message family a `node.tx` type code names (the inverse of `msg_type_code`).
fn msg_type_name(code: u16) -> &'static str {
    match code {
        1 => "bsm",
        2 => "cam",
        3 => "denm",
        4 => "spat",
        5 => "map",
        6 => "psm",
        7 => "vam",
        8 => "cpm",
        9 => "srm",
        10 => "ssm",
        11 => "wsa",
        12 => "crl",
        _ => "other",
    }
}

impl LiveEngine {
    /// Loads a scenario, builds the kernel and starts it.
    ///
    /// # Errors
    /// Whatever the scenario loader, the world importer or the kernel refuses, as
    /// [`ServerError::Internal`] carrying the engine's own message; or
    /// [`ServerError::Io`] if the host thread cannot be spawned.
    pub fn open(path: impl AsRef<std::path::Path>, options: LiveOptions) -> Result<Self> {
        let scenario = Scenario::load(path.as_ref())
            .map_err(|e| ServerError::Internal(format!("{}: {e}", path.as_ref().display())))?;
        let mut engine = Self::new(scenario, options)?;
        engine.source = Some(path.as_ref().to_path_buf());
        Ok(engine)
    }

    /// Builds the kernel from a scenario already in hand.
    ///
    /// # Errors
    /// As [`LiveEngine::open`].
    pub fn new(scenario: Scenario, options: LiveOptions) -> Result<Self> {
        let (mut setup, host) =
            spawn_host(scenario.clone(), &options, options.lookahead_steps, None)?;
        let world_memo = setup.world_memo.take();
        let projector = Projector::new(&setup);
        let metric_names = setup
            .catalogue
            .iter()
            .map(|m| (m.str_id, m.name.clone()))
            .collect();
        let descriptor = descriptor_of(&setup);
        let world = Arc::new(setup.world_payload.clone());
        let world_json = setup.world_json.clone();
        Ok(LiveEngine {
            scenario,
            state: if options.paused {
                RunState::Paused
            } else {
                RunState::Running
            },
            speed: options.speed,
            options,
            descriptor,
            world,
            world_json,
            setup,
            projector,
            host,
            timeline: std::collections::VecDeque::new(),
            base_index: 0,
            cursor: 0,
            produced: 0,
            client_sync: false,
            report: None,
            failure: None,
            history: BTreeMap::new(),
            metric_names,
            last_telemetry: BTreeMap::new(),
            appended_strings: Vec::new(),
            appended_index: BTreeSet::new(),
            staged: None,
            source: None,
            digest: <sha2::Sha256 as sha2::Digest>::new(),
            digest_steps: 0,
            digest_final: None,
            runs_started: 1,
            stats: RunStats::default(),
            exports: None,
            world_memo,
        })
    }

    /// The `vwp-world/1` JSON form, for [`crate::Run::new`].
    pub fn world_json(&self) -> &str {
        &self.world_json
    }

    /// The run manifest (02-architecture §6.5), for `run.status`.
    pub fn manifest(&self) -> &Value {
        &self.setup.manifest
    }

    /// The kernel's own report, once the run has finished.
    pub fn report(&self) -> Option<&RunReport> {
        self.report.as_ref()
    }

    /// True if every node the record stream named was one the actor→node reconstruction
    /// predicted. See the module header.
    pub fn mapping_is_consistent(&self) -> bool {
        self.projector.mapping_is_consistent()
    }

    /// **A test hook, not an option.** Makes the actor→node reconstruction draw against a
    /// different equipped fraction from the one the kernel is using.
    ///
    /// It exists so that [`LiveEngine::mapping_is_consistent`] can be shown to go red. A
    /// consistency check that has never failed pins nothing, and this reconstruction is
    /// the one part of the live path that is recomputed rather than read, so it is exactly
    /// the part that needs a check that can fail.
    pub fn mis_predict_equipped_fraction_for_test(&mut self, fraction: f64) {
        self.projector.equipped_fraction = fraction;
    }

    /// Channels the kernel emitted that this build has no §3.6 payload for, and metric
    /// names the symbol table does not hold — two ways a stream can silently lose content,
    /// reported rather than swallowed.
    pub fn unprojected(&self) -> (Vec<String>, Vec<String>) {
        (
            self.projector
                .unprojected_channels
                .iter()
                .cloned()
                .collect(),
            self.projector.unnamed_metrics.iter().cloned().collect(),
        )
    }

    /// Channels whose records their own declared reader-side view could not decode, with
    /// how many records each lost.
    ///
    /// A non-empty map is a producer/reader schema disagreement in the workspace, not a
    /// transport problem: the channel is in `v2xw-record`'s table, the producer wrote it,
    /// and `v2xw-metrics`' view of it refused the bytes. Reported through
    /// [`Introspect::caveats`] as well, so it reaches a client and not only a test.
    pub fn undecodable_channels(&self) -> &BTreeMap<String, u64> {
        &self.projector.undecodable_channels
    }

    fn step_ns(&self) -> u64 {
        self.descriptor.cadence.mobility_step.as_nanos().max(1)
    }

    /// The last step the stream emitted, which is what every published view is *as of*.
    ///
    /// The projector runs at the kernel's frontier, and the kernel outruns real time by
    /// three orders of magnitude: by the time a client has watched two simulated minutes
    /// the projector has finished the run. So the node table, the actor and node counts
    /// and the telemetry a client is answered with come from here — the step at
    /// `sim_time()` — and never from the projector's own tables, which describe a future
    /// the client has not been sent. Answering from the frontier was a real defect: a
    /// `view.follow` on the newest node in the `Hello` table subscribed to a node that
    /// does not exist yet at the stream's position, and no `Telemetry` frame ever arrived.
    fn emitted(&self) -> Option<&StepOutput> {
        self.at(self.cursor.checked_sub(1)?)
    }

    /// The node table as of the emitted step, for `Hello` (§3.1.3) and `inspect.node`.
    fn node_rows(&self) -> Vec<crate::engine::NodeFacts> {
        let Some(step) = self.emitted() else {
            return Vec::new();
        };
        let mut rows: Vec<crate::engine::NodeFacts> = step
            .snapshot
            .actors
            .iter()
            .filter_map(|pose| {
                let node = pose.node?;
                Some(crate::engine::NodeFacts {
                    node_id: node.index(),
                    actor_id: pose.actor.index(),
                    pos_m: [
                        pose.pos_m[0] as f32,
                        pose.pos_m[1] as f32,
                        // 1.5 m: the antenna height `v2xw-radio`'s isotropic endpoint uses
                        // for a car. A rendering offset, not a model input.
                        (pose.pos_m[2] + 1.5) as f32,
                    ],
                    label: format!("veh_{:04}", pose.actor.index()),
                    profile_id: self.setup.obu_profile.clone(),
                    flags: NODE_HAS_HSM,
                    kind: 0,
                    class_idx: pose.class_idx,
                })
            })
            .collect();
        // §3.1.3: "node_id ascending, dense where possible".
        rows.sort_by_key(|row| row.node_id);
        rows
    }

    /// Takes everything the host thread has ready, without waiting.
    fn pump(&mut self) {
        use std::sync::mpsc::TryRecvError;
        loop {
            // Retention bounds how much history is kept; `lookahead_steps` bounds how far
            // ahead of the stream the kernel may run. Once the stream is that far behind,
            // nothing more is taken off the channel, the channel fills, and the kernel
            // stops in `StepRecorder::send` — which is what the option's own documentation
            // promised ("small enough that a paused run stops the kernel"), and what this
            // loop did not do: it absorbed until the retention was full, so a client at 1x
            // saw a kernel that had already simulated the whole run. That cost memory
            // proportional to the run, made the followed vehicle's message log (filled at
            // absorb time, read at the stream's instant) show only the run's last seconds,
            // and made run.status report counters from the end of the run.
            if self.timeline.len() >= self.options.retain_steps && self.base_index >= self.cursor {
                break;
            }
            if self.produced
                >= self
                    .cursor
                    .saturating_add(self.options.lookahead_steps.max(1) as u64)
            {
                break;
            }
            match self.host.steps.try_recv() {
                Ok(message) => {
                    if !self.absorb(message) {
                        break;
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Waits up to `budget` for one more step, for `run.step` on a run the kernel has not
    /// produced yet.
    ///
    /// This is a **transport** wait, in the same sense as §1.5's stall timeout and §1.2's
    /// ping interval: it reads a wall clock, and no simulated or recorded quantity depends
    /// on how long it waits. The kernel's own timeline is unaffected — it is `SimTime`,
    /// and a step it has already computed is byte-identical whenever it is collected.
    fn pump_blocking(&mut self, budget: std::time::Duration) {
        self.pump();
        if self.has(self.cursor) || self.report.is_some() || self.failure.is_some() {
            return;
        }
        if let Ok(message) = self.host.steps.recv_timeout(budget) {
            let _ = self.absorb(message);
            self.pump();
        }
    }

    /// Takes one host message. Returns `false` when the stream from the host has ended.
    fn absorb(&mut self, message: HostMsg) -> bool {
        match message {
            HostMsg::Step(raw) => {
                let index = raw.index;
                let out = self.projector.project(&raw);
                self.digest_step(&out);
                self.stats.absorb(&out);
                for row in &out.metrics {
                    if let Some(name) = self.metric_names.get(&row.str_metric) {
                        self.history
                            .entry(name.clone())
                            .or_default()
                            .push((out.sim_time, row.value));
                    }
                }
                if self.timeline.is_empty() {
                    self.base_index = index;
                }
                self.timeline.push_back(out);
                self.produced = index + 1;
                while self.timeline.len() > self.options.retain_steps
                    && self.base_index < self.cursor
                {
                    self.timeline.pop_front();
                    self.base_index += 1;
                }
                true
            }
            HostMsg::Done(report) => {
                self.report = Some(*report);
                false
            }
            HostMsg::Exported(result) => {
                self.exports = Some(result);
                true
            }
            HostMsg::Failed(message) => {
                self.failure = Some(message);
                self.state = RunState::Error;
                false
            }
        }
    }

    /// True if step `index` is in the retained window.
    fn has(&self, index: u64) -> bool {
        index >= self.base_index && index < self.base_index + self.timeline.len() as u64
    }

    fn at(&self, index: u64) -> Option<&StepOutput> {
        if !self.has(index) {
            return None;
        }
        self.timeline
            .get(usize::try_from(index - self.base_index).unwrap_or(usize::MAX))
    }

    /// Appends any string the emitted step's node table needs and has not used before.
    fn intern_labels(&mut self, step: &StepOutput) {
        let mut wanted: Vec<String> = vec![self.setup.obu_profile.clone()];
        wanted.extend(
            step.snapshot
                .actors
                .iter()
                .filter(|pose| pose.node.is_some())
                .map(|pose| format!("veh_{:04}", pose.actor.index())),
        );
        for string in wanted {
            if self
                .setup
                .hello
                .strings
                .strings
                .iter()
                .any(|s| s == &string)
            {
                continue;
            }
            if self.appended_index.insert(string.clone()) {
                self.appended_strings.push(string);
            }
        }
    }

    /// Starts `scenario` from `t = 0` on a fresh kernel thread, after the previous one has
    /// stopped and been joined — so there is never more than one kernel in the process.
    ///
    /// On a build failure (a world that does not import, say) the previous run's streamed
    /// history is kept, so the page still shows what it showed, the state becomes `error`
    /// with the engine's own message, and the scenario is not changed: pressing Run again
    /// runs the last scenario that built.
    fn restart(&mut self, scenario: Scenario) -> Result<()> {
        self.host.shutdown();
        let (mut setup, host) = match spawn_host(
            scenario.clone(),
            &self.options,
            self.options.lookahead_steps,
            self.world_memo.take(),
        ) {
            Ok(pair) => pair,
            Err(e) => {
                self.failure = Some(e.to_string());
                self.state = RunState::Error;
                return Err(e);
            }
        };
        self.world_memo = setup.world_memo.take();
        self.metric_names = setup
            .catalogue
            .iter()
            .map(|m| (m.str_id, m.name.clone()))
            .collect();
        self.descriptor = descriptor_of(&setup);
        self.world = Arc::new(setup.world_payload.clone());
        self.world_json = setup.world_json.clone();
        self.projector = Projector::new(&setup);
        self.setup = setup;
        self.host = host;
        self.scenario = scenario;
        self.timeline.clear();
        self.base_index = 0;
        self.cursor = 0;
        self.produced = 0;
        self.report = None;
        self.failure = None;
        self.history.clear();
        self.last_telemetry.clear();
        // Every connection gets a fresh `Hello` for the new run (`Run` moves to a new
        // generation on every start), so the strings the previous run appended are
        // nobody's any more. Keeping them grew every `Hello` with every run.
        self.appended_strings.clear();
        self.appended_index.clear();
        self.digest = <sha2::Sha256 as sha2::Digest>::new();
        self.digest_steps = 0;
        self.digest_final = None;
        self.stats = RunStats::default();
        self.exports = None;
        self.runs_started += 1;
        Ok(())
    }

    /// The scenario `run.start` will run when it names none: the staged one, else the
    /// current one.
    fn next_scenario(&self) -> Scenario {
        self.staged.clone().unwrap_or_else(|| self.scenario.clone())
    }

    /// Resolves a preset id or a path to a loaded, validated scenario.
    fn load_preset(&self, id: &str) -> Result<Scenario> {
        let path = self
            .preset_paths()
            .into_iter()
            .find(|p| preset_id(p) == id)
            .unwrap_or_else(|| std::path::PathBuf::from(id));
        Scenario::load(&path).map_err(|e| {
            ServerError::param(
                "/scenario",
                &format!("{}: {e}", path.display()),
                "pick one of the scenarios scenario.list offers",
            )
        })
    }

    /// The scenario files beside the one this engine was opened on, sorted.
    fn preset_paths(&self) -> Vec<std::path::PathBuf> {
        let Some(dir) = self.source.as_ref().and_then(|p| p.parent()) else {
            return Vec::new();
        };
        let dir = if dir.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            dir
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut paths: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .is_some_and(|x| x == "yaml" || x == "yml" || x == "json")
            })
            .collect();
        paths.sort();
        paths
    }

    /// Parses, migrates and validates an inline document the way the loader does a file.
    ///
    /// `meta.base` is resolved against the directory of the scenario this engine was
    /// opened on, which is where a document the page edited came from.
    fn scenario_from_document(
        &self,
        doc: &Value,
    ) -> std::result::Result<Scenario, Vec<ParamError>> {
        let text = serde_json::to_string(doc)
            .map_err(|e| vec![ParamError::new("/", e.to_string(), "pass a JSON object")])?;
        let base = self.source.as_ref().and_then(|p| p.parent());
        match Scenario::parse(&text, base) {
            Ok(s) => Ok(s),
            Err(first) => {
                // `parse` stops at the first problem. Collect all of them when the
                // document at least deserialises, so a form can mark every bad field.
                let mut migrated = doc.clone();
                let _ = v2xw_engine::scenario::Chain::shipped().migrate(&mut migrated);
                match Scenario::from_document(migrated) {
                    Ok(s) => {
                        let all: Vec<ParamError> = v2xw_engine::scenario::validate(&s)
                            .iter()
                            .map(scenario_error)
                            .collect();
                        if all.is_empty() {
                            Err(vec![engine_error(&first)])
                        } else {
                            Err(all)
                        }
                    }
                    Err(_) => Err(vec![engine_error(&first)]),
                }
            }
        }
    }

    /// Records one produced step in the run's output digest.
    fn digest_step(&mut self, out: &StepOutput) {
        use sha2::Digest;
        let d = &mut self.digest;
        d.update(out.sim_time.to_le_bytes());
        d.update((out.snapshot.actors.len() as u64).to_le_bytes());
        for pose in &out.snapshot.actors {
            d.update(pose.actor.index().to_le_bytes());
            for v in pose.pos_m {
                d.update(v.to_bits().to_le_bytes());
            }
            d.update(pose.heading_rad.to_bits().to_le_bytes());
            d.update(pose.speed_mps.to_bits().to_le_bytes());
        }
        d.update((out.events.len() as u64).to_le_bytes());
        for event in &out.events {
            d.update(event.channel_id.to_le_bytes());
            d.update(event.sim_time_ns.to_le_bytes());
            d.update(&event.payload);
        }
        self.digest_steps += 1;
        if out.end_of_run {
            self.digest_final = Some(hex_of(&self.digest.clone().finalize()));
        }
    }
}

/// The id `scenario.list` gives a scenario file: its path as the engine was given it.
fn preset_id(path: &std::path::Path) -> String {
    path.display().to_string()
}

/// A loader error as a `{path, message, hint}` row (§6.4).
fn scenario_error(e: &v2xw_engine::ScenarioError) -> ParamError {
    let path = e
        .field()
        .map(|f| format!("/{}", f.replace('.', "/")))
        .unwrap_or_else(|| "/".to_string());
    ParamError::new(
        path,
        e.to_string(),
        "see the field's help text for its allowed values",
    )
}

/// An engine error from the loader as a `{path, message, hint}` row.
fn engine_error(e: &v2xw_engine::EngineError) -> ParamError {
    match e {
        v2xw_engine::EngineError::Scenario(inner) => scenario_error(inner),
        other => ParamError::new("/", other.to_string(), "check the scenario document"),
    }
}

/// Lower-case hex of a digest.
fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every JSON Pointer at which `a` and `b` differ, down to the leaves, sorted.
fn changed_pointers(a: &Value, b: &Value) -> Vec<String> {
    fn walk(a: Option<&Value>, b: Option<&Value>, at: &str, out: &mut Vec<String>) {
        match (a, b) {
            (Some(Value::Object(x)), Some(Value::Object(y))) => {
                let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
                for k in keys {
                    let child = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
                    walk(x.get(k), y.get(k), &child, out);
                }
            }
            (x, y) if x == y => {}
            // `274` and `274.0` are one value. A document that went through a browser comes
            // back with every whole float written as an integer (JavaScript has one number
            // type), and inside an opaque `Value` block — a world generator's parameters —
            // nothing re-types it, so a plain `==` reported untouched fields as edits.
            (Some(Value::Number(x)), Some(Value::Number(y))) if x.as_f64() == y.as_f64() => {}
            _ => out.push(if at.is_empty() {
                "/".to_string()
            } else {
                at.to_string()
            }),
        }
    }
    let mut out = Vec::new();
    walk(Some(a), Some(b), "", &mut out);
    out
}

/// Applies an RFC 6902 patch's `add`, `replace`, `remove` and `test` operations.
fn apply_patch(doc: &mut Value, ops: &[Value]) -> std::result::Result<(), ParamError> {
    for (i, op) in ops.iter().enumerate() {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
        let path = op.get("path").and_then(Value::as_str).ok_or_else(|| {
            ParamError::new(format!("/patch/{i}/path"), "required", "a JSON Pointer")
        })?;
        let (parent, key) = match path.rfind('/') {
            Some(at) => (
                &path[..at],
                path[at + 1..].replace("~1", "/").replace("~0", "~"),
            ),
            None => {
                return Err(ParamError::new(
                    format!("/patch/{i}/path"),
                    "must start with /",
                    "e.g. /time/duration_s",
                ));
            }
        };
        let value = op.get("value").cloned();
        match kind {
            "test" => {
                if doc.pointer(path) != value.as_ref() {
                    return Err(ParamError::new(
                        format!("/patch/{i}"),
                        format!("test failed at {path}"),
                        "the document changed under this patch",
                    ));
                }
            }
            "add" | "replace" | "remove" => {
                let target = doc.pointer_mut(parent).ok_or_else(|| {
                    ParamError::new(
                        format!("/patch/{i}/path"),
                        format!("`{parent}` does not exist"),
                        "add the enclosing object first",
                    )
                })?;
                match (target, kind) {
                    (Value::Object(map), "remove") => {
                        map.remove(&key);
                    }
                    (Value::Object(map), _) => {
                        map.insert(key, value.unwrap_or(Value::Null));
                    }
                    (Value::Array(items), _) => {
                        let index = if key == "-" {
                            items.len()
                        } else {
                            key.parse::<usize>().map_err(|_| {
                                ParamError::new(
                                    format!("/patch/{i}/path"),
                                    "not an array index",
                                    "use a number or -",
                                )
                            })?
                        };
                        match kind {
                            "remove" if index < items.len() => {
                                items.remove(index);
                            }
                            "replace" if index < items.len() => {
                                items[index] = value.unwrap_or(Value::Null);
                            }
                            "add" if index <= items.len() => {
                                items.insert(index, value.unwrap_or(Value::Null));
                            }
                            _ => {
                                return Err(ParamError::new(
                                    format!("/patch/{i}/path"),
                                    "index out of range",
                                    "check the array's length",
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(ParamError::new(
                            format!("/patch/{i}/path"),
                            format!("`{parent}` is not an object or an array"),
                            "patch a field inside an object",
                        ));
                    }
                }
            }
            other => {
                return Err(ParamError::new(
                    format!("/patch/{i}/op"),
                    format!("`{other}` is not supported here"),
                    "use add, replace, remove or test",
                ));
            }
        }
    }
    Ok(())
}

impl Engine for LiveEngine {
    fn descriptor(&self) -> &RunDescriptor {
        &self.descriptor
    }

    fn world(&self) -> &Arc<WorldPayload> {
        &self.world
    }

    fn state(&self) -> RunState {
        self.state
    }

    fn sim_time(&self) -> SimTime {
        // The instant of the last step the stream sent — the one on the client's screen,
        // and the one `emitted()`, the node table and the counts all describe. It was
        // `cursor · Δt`, one step ahead of all three, so a finished 5 s run reported
        // `t_ns = 5.1 s` against `t_end_ns = 5 s` and the page's clock read past the end.
        self.cursor.saturating_sub(1).saturating_mul(self.step_ns())
    }

    fn speed(&self) -> (f64, bool) {
        (self.speed, self.client_sync)
    }

    fn counts(&self) -> (u32, u32) {
        // As of the emitted step, for the same reason `emitted` exists: `run.status`
        // reports `t_ns` and the counts together, and they have to describe one instant.
        let Some(step) = self.emitted() else {
            return (0, 0);
        };
        (
            u32::try_from(step.snapshot.actors.len()).unwrap_or(u32::MAX),
            u32::try_from(
                step.snapshot
                    .actors
                    .iter()
                    .filter(|pose| pose.node.is_some())
                    .count(),
            )
            .unwrap_or(u32::MAX),
        )
    }

    fn live_nodes(&self) -> Option<Vec<crate::engine::NodeFacts>> {
        Some(self.node_rows())
    }

    fn live_strings(&self) -> Vec<String> {
        self.appended_strings.clone()
    }

    fn metric_catalogue(&self) -> Vec<MetricInfo> {
        self.setup.catalogue.clone()
    }

    fn control(&mut self, command: Control) -> Result<ControlOutcome> {
        let mut extra = BTreeMap::new();
        match command {
            Control::Start {
                paused,
                speed,
                seed,
                scenario,
            } => {
                // §6.6 refuses a start while the run is moving (-32001); the page pauses
                // first. From any other state the previous kernel is stopped and joined
                // before the next one is built, so there is never a second kernel.
                if self.state == RunState::Running {
                    return Err(ServerError::RunAlreadyRunning);
                }
                let mut next = match scenario {
                    Some(ScenarioSource::Document(doc)) => self
                        .scenario_from_document(&doc)
                        .map_err(ServerError::ScenarioInvalid)?,
                    Some(ScenarioSource::Preset(id)) => self.load_preset(&id)?,
                    None => self.next_scenario(),
                };
                if let Some(seed) = seed {
                    next.seed = seed;
                }
                self.restart(next)?;
                // Staged edits are consumed by the run that runs them; the form then shows
                // the running scenario, which is now the edited one.
                self.staged = None;
                if let Some(speed) = speed {
                    self.speed = speed;
                }
                self.state = if paused {
                    RunState::Paused
                } else {
                    RunState::Running
                };
                extra.insert("run_id".to_string(), json!(self.descriptor.run_id.clone()));
            }
            Control::Pause => {
                if self.state != RunState::Running {
                    return Err(ServerError::RunNotRunning(format!(
                        "state is {}",
                        self.state.as_str()
                    )));
                }
                self.state = RunState::Paused;
            }
            Control::Resume => {
                if self.state != RunState::Paused {
                    return Err(ServerError::RunNotRunning(format!(
                        "state is {}, not paused",
                        self.state.as_str()
                    )));
                }
                self.state = RunState::Running;
            }
            Control::Speed { speed, client_sync } => {
                self.speed = speed;
                self.client_sync = client_sync;
            }
            Control::Stop { finalize_exports } => {
                // Stopped and joined: the kernel thread is gone when this returns, not
                // computing to the horizon with nobody listening.
                self.host.shutdown();
                self.state = RunState::Finished;
                if let Some(path) = &self.descriptor.recording_path {
                    extra.insert("recording_path".to_string(), json!(path));
                    if finalize_exports {
                        // The recording is finished by the host thread when its run ends;
                        // the digest is over the file as it stands, which is what a caller
                        // asking for it wants to compare.
                        if let Ok(bytes) = std::fs::read(path) {
                            extra.insert(
                                "digest".to_string(),
                                json!(v2xw_core::hash::sha256_hex(&bytes)),
                            );
                            extra.insert(
                                "files".to_string(),
                                json!([{
                                    "path": path,
                                    "sha256": v2xw_core::hash::sha256_hex(&bytes),
                                    "bytes": bytes.len(),
                                }]),
                            );
                        }
                    }
                }
            }
        }
        Ok(ControlOutcome {
            state: self.state,
            t_ns: self.sim_time(),
            extra,
        })
    }

    fn step(&mut self) -> Result<Option<StepOutput>> {
        if let Some(message) = &self.failure {
            return Err(ServerError::Internal(message.clone()));
        }
        // 60 ms is longer than the 50 ms the producer sleeps on an empty step and shorter
        // than any client's stall deadline, so `run.step` gets its step and a producer
        // that finds nothing does not hold the run lock.
        self.pump_blocking(std::time::Duration::from_millis(60));
        if !self.has(self.cursor) {
            if self.report.is_some()
                || self.cursor.saturating_mul(self.step_ns()) >= self.descriptor.duration
            {
                self.state = RunState::Finished;
            }
            return Ok(None);
        }
        let out = self.at(self.cursor).cloned();
        self.cursor += 1;
        if let Some(out) = &out {
            for row in &out.telemetry {
                self.last_telemetry.insert(row.node_id, *row);
            }
            self.intern_labels(out);
        }
        if let Some(out) = &out
            && out.end_of_run
        {
            self.state = RunState::Finished;
        }
        Ok(out)
    }

    fn seek(&mut self, t: SimTime) -> Result<Vec<StepOutput>> {
        self.pump();
        let (min_ns, max_ns) = self.seek_range();
        if t < min_ns || t > max_ns {
            return Err(ServerError::SeekOutOfRange { min_ns, max_ns });
        }
        let step_ns = self.step_ns();
        let target = t / step_ns;
        let per_gop = self.descriptor.cadence.max_deltas_per_gop().max(1);
        let first = target.saturating_sub(target % per_gop).max(self.base_index);
        let outputs: Vec<StepOutput> = (first..=target)
            .filter_map(|index| self.at(index).cloned())
            .collect();
        if outputs.is_empty() {
            return Err(ServerError::SeekOutOfRange { min_ns, max_ns });
        }
        self.cursor = target + 1;
        for step in &outputs {
            for row in &step.telemetry {
                self.last_telemetry.insert(row.node_id, *row);
            }
        }
        if let Some(last) = outputs.last() {
            self.intern_labels(last);
        }
        // §6.6: "seeking a live run pauses it". The stream is now positioned inside
        // recorded time and the producer would otherwise race forward from it.
        if self.state == RunState::Running {
            self.state = RunState::Paused;
        }
        Ok(outputs)
    }

    fn seek_range(&self) -> (u64, u64) {
        let step_ns = self.step_ns();
        let first = self.base_index.saturating_mul(step_ns);
        let last = self.produced.saturating_sub(1).saturating_mul(step_ns);
        (first, last.max(first))
    }

    fn query(&mut self, query: &Query) -> Result<Value> {
        crate::introspect::answer(self, query)
    }

    fn world_json(&self) -> Option<String> {
        Some(self.world_json.clone())
    }

    fn current(&self) -> Option<StepOutput> {
        self.emitted().cloned()
    }

    fn stage(&mut self, request: StageRequest) -> Result<Staged> {
        let running = self.descriptor.scenario.clone();
        let (scenario, errors, document) = match request {
            StageRequest::Preset(id) => {
                let s = self.load_preset(&id)?;
                (Some(s), Vec::new(), None)
            }
            StageRequest::Document(doc) => match self.scenario_from_document(&doc) {
                Ok(s) => (Some(s), Vec::new(), None),
                Err(errors) => (None, errors, Some(doc)),
            },
            StageRequest::Patch(ops) => {
                let mut doc = match &self.staged {
                    Some(s) => {
                        serde_json::to_value(s).map_err(|e| ServerError::Internal(e.to_string()))?
                    }
                    None => running.clone(),
                };
                apply_patch(&mut doc, &ops).map_err(|e| ServerError::InvalidParams(vec![e]))?;
                match self.scenario_from_document(&doc) {
                    Ok(s) => (Some(s), Vec::new(), None),
                    Err(errors) => (None, errors, Some(doc)),
                }
            }
        };
        match scenario {
            Some(scenario) => {
                let document = serde_json::to_value(&scenario)
                    .map_err(|e| ServerError::Internal(e.to_string()))?;
                let hash = scenario
                    .content_hash()
                    .map_err(|e| ServerError::Internal(e.to_string()))?;
                let changed = changed_pointers(&running, &document);
                // Staging the scenario that is already running is un-staging: nothing
                // differs, and the form should say "no pending edits".
                self.staged = if changed.is_empty() {
                    None
                } else {
                    Some(scenario)
                };
                Ok(Staged {
                    document,
                    hash,
                    changed,
                    errors: Vec::new(),
                })
            }
            None => {
                // An invalid document is reported and not staged: the next run keeps
                // whatever was staged before, so a typo never replaces a good scenario.
                let document = document.unwrap_or(Value::Null);
                let changed = changed_pointers(&running, &document);
                Ok(Staged {
                    hash: String::new(),
                    document,
                    changed,
                    errors,
                })
            }
        }
    }

    fn staged(&self) -> Option<Staged> {
        let scenario = self.staged.as_ref()?;
        let document = serde_json::to_value(scenario).ok()?;
        let hash = scenario.content_hash().ok()?;
        let changed = changed_pointers(&self.descriptor.scenario, &document);
        Some(Staged {
            document,
            hash,
            changed,
            errors: Vec::new(),
        })
    }

    fn validate_document(&self, doc: &Value) -> Option<(Vec<ParamError>, Vec<ParamError>)> {
        Some(match self.scenario_from_document(doc) {
            Ok(_) => (Vec::new(), Vec::new()),
            Err(errors) => (errors, Vec::new()),
        })
    }

    fn presets(&self) -> Vec<Value> {
        let running = self.descriptor.scenario_hash_hex.clone();
        self.preset_paths()
            .iter()
            .map(|path| {
                let id = preset_id(path);
                match Scenario::load(path) {
                    Ok(s) => json!({
                        "id": id,
                        "kind": "preset",
                        "name": s.meta.name,
                        "description": s.meta.description,
                        "tags": s.meta.tags,
                        "hash": s.content_hash().unwrap_or_default(),
                        "running": s.content_hash().is_ok_and(|h| h == running),
                    }),
                    // Listed, not hidden: a scenario in the folder that does not load is
                    // something the user should hear about, with the loader's reason.
                    Err(e) => json!({
                        "id": id,
                        "kind": "preset",
                        "name": path.file_stem().map(|s| s.to_string_lossy().to_string()),
                        "description": format!("does not load: {e}"),
                        "tags": ["invalid"],
                    }),
                }
            })
            .collect()
    }

    fn diagnostics(&self) -> Value {
        json!({
            "kernel_threads": kernel_threads(),
            "kernel_threads_started": KERNEL_THREADS_STARTED.load(Ordering::SeqCst),
            "runs_started": self.runs_started,
            "produced_ns": self.produced.saturating_mul(self.step_ns()),
            "output_digest": self.digest_final,
            "digest_steps": self.digest_steps,
            "failure": self.failure,
            "stats": self.stats.to_json(),
            "finished": self.report.is_some(),
            // Frames not generated because they fell in a `time.time_dilation` window.
            "suppressed_frames": self.report.as_ref().map(|r| r.suppressed_frames),
            "exports": match &self.exports {
                None => Value::Null,
                Some(Ok(done)) => json!(done),
                Some(Err(why)) => json!({"error": why}),
            },
            "retained_steps": self.timeline.len(),
            "retain_limit_steps": self.options.retain_steps,
        })
    }
}

impl Introspect for LiveEngine {
    fn node_list(&self) -> Vec<crate::engine::NodeFacts> {
        self.node_rows()
    }

    fn telemetry_of(&self, node: u32) -> Option<NodeTelemetry> {
        self.last_telemetry.get(&node).copied()
    }

    fn metric_series(
        &self,
        name: &str,
        from: u64,
        to: u64,
        bin: u64,
        limit: usize,
    ) -> Vec<(u64, Option<f64>)> {
        let samples = self.history.get(name);
        // Never past the stream: the projector has already computed metric bins the
        // client's `Keyframe` has not reached, and answering from them would tell a live
        // viewer the future.
        let to = to.min(self.sim_time());
        let bin = bin.max(1);
        let mut out = Vec::new();
        let mut edge = from - (from % bin);
        while edge <= to && out.len() < limit {
            let upper = edge.saturating_add(bin);
            let value = samples.and_then(|rows| {
                // The bin's value is the mean of the samples whose instant falls in it,
                // reduced with `sum_ordered` so two builds agree to the last bit. A bin
                // with no sample is `None` and is reported as JSON `null`: the metric was
                // not observed there, which is not the same as being zero there.
                let inside: Vec<f64> = rows
                    .iter()
                    .filter(|(t, _)| *t >= edge && *t < upper)
                    .map(|(_, v)| *v)
                    .collect();
                if inside.is_empty() {
                    return None;
                }
                let n = inside.len() as f64;
                Some(v2xw_core::math::sum_ordered(inside) / n)
            });
            out.push((edge, value));
            edge = upper;
        }
        out
    }

    fn provenance_chain(&self) -> Vec<Value> {
        self.setup.prov_chain.clone()
    }

    fn caveats(&self) -> Vec<String> {
        let mut out = vec![
            format!(
                "produced by v2xw-engine {} (commit {}), scenario {} seed {:#x}",
                v2xw_engine::manifest::ENGINE_VERSION,
                v2xw_engine::manifest::GIT_COMMIT,
                self.scenario.meta.name,
                self.scenario.seed
            ),
            "reception outcomes are link-budget decisions against the noise floor: \
             concurrent frames do not raise each other's denominator, so SINR is SNR"
                .to_string(),
            "no MAC backoff, no CBR measurement and no DCC gate: a frame reaches the air \
             after the signing latency plus one AIFS"
                .to_string(),
            "signal phases are the world's imported fixed-time plans evaluated at t; the \
             kernel schedules no signal event, so nothing in the run reads them back"
                .to_string(),
        ];
        if !self.projector.over_capacity.is_empty() {
            out.push(format!(
                "{} actor(s) hold no slot because the run reached Hello.actor_capacity \
                 ({}): they exist and their nodes transmit, but no pose for them is on \
                 the wire (§3.1.1)",
                self.projector.over_capacity.len(),
                self.projector.actor_capacity
            ));
        }
        if !self.projector.undecodable_channels.is_empty() {
            out.push(format!(
                "records lost because the channel's own reader-side view refused them \
                 (a producer/reader schema disagreement, not a transport fault): {}",
                self.projector
                    .undecodable_channels
                    .iter()
                    .map(|(channel, n)| format!("{channel} ({n})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.projector.unprojected_channels.is_empty() {
            out.push(format!(
                "channels the run emitted and this server has no §3.6 payload for: {}",
                self.projector
                    .unprojected_channels
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.projector.mapping_is_consistent() {
            out.push(format!(
                "the actor→node reconstruction did not predict node(s) {:?}: node identity \
                 in this stream is unreliable",
                self.projector.unmapped_nodes
            ));
        }
        out
    }

    fn node_section(&self, node: u32, section: &str, limit: usize) -> Option<Value> {
        // `neighbors` is answered from the link history and not from a telemetry row, so
        // it is available for a node the telemetry window has not covered yet.
        if section == "neighbors" {
            let mut rows: Vec<Value> = Vec::new();
            for ((tx, rx), history) in &self.projector.links {
                if *rx != node {
                    continue;
                }
                let Some(last) = history.iter().rev().find(|o| o.t <= self.sim_time()) else {
                    continue;
                };
                rows.push(json!({
                    "node": tx,
                    "state": if last.received { "heard" } else { "lost" },
                    "last_seen_ns": last.t,
                    "rssi_dbm": last.rssi_dbm,
                    "distance_m": last.dist_m,
                }));
                if rows.len() >= limit {
                    break;
                }
            }
            return Some(Value::Array(rows));
        }
        // `messages`: what this node most recently sent and received, message by message —
        // each frame's layers and stamps, each reception's fate, cause and decomposed
        // delay. Answered from the projector's log, so it needs no telemetry window.
        if section == "messages" {
            let (sent, received) = self.projector.messages.get(&NodeId::new(node))?;
            let now = self.sim_time();
            let pick = |log: &std::collections::VecDeque<Value>| -> Vec<Value> {
                let mut v: Vec<Value> = log
                    .iter()
                    .filter(|e| e["t_ns"].as_u64().is_none_or(|t| t <= now))
                    .cloned()
                    .collect();
                let skip = v.len().saturating_sub(limit);
                v.drain(..skip);
                v
            };
            return Some(json!({ "sent": pick(sent), "received": pick(received) }));
        }
        let telemetry = self.last_telemetry.get(&node)?;
        match section {
            "queues" => {
                let depth = |p50: u16, p95: u16| {
                    (p50 != u16::MAX || p95 != u16::MAX).then(|| {
                        json!({
                            "p50": (p50 != u16::MAX).then_some(p50),
                            "p95": (p95 != u16::MAX).then_some(p95),
                        })
                    })
                };
                let mut out = serde_json::Map::new();
                for (name, value) in [
                    ("rx", depth(telemetry.q_rx_p50, telemetry.q_rx_p95)),
                    (
                        "verify",
                        depth(telemetry.q_verify_p50, telemetry.q_verify_p95),
                    ),
                    ("tx", depth(telemetry.q_tx_p50, telemetry.q_tx_p95)),
                ] {
                    if let Some(value) = value {
                        out.insert(name.to_string(), value);
                    }
                }
                (!out.is_empty()).then_some(Value::Object(out))
            }
            "stores" => {
                // §3.5.2's sentinels mean *unknown*, and an unknown store is omitted
                // rather than reported as 18 446 744 073 709 551 615 bytes. A section
                // with nothing known at all is absent, not empty.
                let mut out = serde_json::Map::new();
                if telemetry.cert_stored != u32::MAX {
                    out.insert("certs".to_string(), json!(telemetry.cert_stored));
                }
                if telemetry.crl_bytes != u64::MAX {
                    out.insert("crl_bytes".to_string(), json!(telemetry.crl_bytes));
                }
                if telemetry.storage_used_b != u64::MAX {
                    out.insert(
                        "storage_used_b".to_string(),
                        json!(telemetry.storage_used_b),
                    );
                }
                if telemetry.storage_total_b != u64::MAX {
                    out.insert(
                        "storage_total_b".to_string(),
                        json!(telemetry.storage_total_b),
                    );
                }
                (!out.is_empty()).then_some(Value::Object(out))
            }
            _ => None,
        }
    }

    fn link_facts(&self, tx: u32, rx: u32, t_ns: u64, window_ns: u64) -> Option<Value> {
        let history = self.projector.links.get(&(tx, rx))?;
        let lower = t_ns.saturating_sub(window_ns);
        let inside: Vec<&LinkObservation> = history
            .iter()
            .filter(|o| o.t >= lower && o.t <= t_ns)
            .collect();
        if inside.is_empty() {
            return None;
        }
        let n = inside.len() as f64;
        let mean = |values: Vec<f64>| {
            if values.is_empty() {
                f64::NAN
            } else {
                let count = values.len() as f64;
                v2xw_core::math::sum_ordered(values) / count
            }
        };
        let received = inside.iter().filter(|o| o.received).count();
        Some(json!({
            "frames": inside.len(),
            "pdr": v2xw_core::math::quantize((received as f64) / n, 6),
            "rssi_dbm": v2xw_core::math::quantize(
                mean(inside.iter().filter_map(|o| o.rssi_dbm).collect()), 2),
            "sinr_db": v2xw_core::math::quantize(
                mean(inside.iter().filter_map(|o| o.sinr_db).collect()), 2),
            "distance_m": v2xw_core::math::quantize(
                mean(inside.iter().filter_map(|o| o.dist_m).collect()), 3),
            "los": {"class": "LOS", "walls_crossed": 0, "obstructed_len_m": 0.0},
        }))
    }

    fn export(&mut self, query: &Query) -> Result<Value> {
        match query {
            Query::ExportRecording { path, profile } => {
                let Some(source) = self.descriptor.recording_path.clone() else {
                    return Err(ServerError::ExportFailed {
                        stage: "open".to_string(),
                        detail: "this run writes no recording: start the server with \
                                 --record <path>"
                            .to_string(),
                    });
                };
                if profile != "full" {
                    return Err(ServerError::NotSupportedHere(format!(
                        "the recording is written in the `full` profile; re-encoding it as \
                         `{profile}` is `v2xw-record`'s NodeProfileStripper on the replay \
                         path, not an export this server performs"
                    )));
                }
                let target = path.clone().unwrap_or_else(|| source.clone());
                if target != source {
                    std::fs::copy(&source, &target).map_err(|e| ServerError::ExportFailed {
                        stage: "copy".to_string(),
                        detail: format!("{source} -> {target}: {e}"),
                    })?;
                }
                let bytes = std::fs::metadata(&target)
                    .map(|m| m.len())
                    .unwrap_or_default();
                Ok(json!({
                    "path": target,
                    "bytes": bytes,
                    "profile": profile,
                    "finished": self.report.is_some(),
                }))
            }
            Query::ExportDataset {
                exporter,
                out_dir,
                visibility,
            } => Err(ServerError::ExportFailed {
                stage: "open".to_string(),
                detail: format!(
                    "exporter `{exporter}` (visibility `{visibility}`, out_dir {:?}) is a \
                     `v2xw-record` dataset writer over a finished recording, and this \
                     server does not host one: run `v2xw run` with `exporters: [{exporter}]` \
                     in the scenario, or `v2xw export` over {}.",
                    out_dir.as_deref().unwrap_or("(default)"),
                    self.descriptor
                        .recording_path
                        .as_deref()
                        .unwrap_or("the recording this run does not write")
                ),
            }),
            _ => Err(ServerError::NotSupportedHere(
                "not an export query".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::roadside_node_count;
    use v2xw_engine::Scenario;

    /// A scenario's path, from the repository root.
    fn scenario_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("scenarios")
            .join(name)
    }

    /// The roadside count the node-id reconstruction offsets by is the scenario's own,
    /// and it is zero exactly when the scenario declares no mast.
    ///
    /// Both cases are asserted, because a function that returned zero unconditionally
    /// would satisfy the grid case on its own and that is the case every other live test
    /// runs on.
    #[test]
    fn the_roadside_offset_is_the_number_of_units_the_scenario_declares() {
        let grid = Scenario::load(scenario_path("phase1-grid.yaml")).expect("the grid loads");
        assert!(
            grid.actors.rsus.is_empty(),
            "phase1-grid declares no mast, which is what makes it the zero case"
        );
        assert_eq!(roadside_node_count(&grid), 0);

        let phase2 =
            Scenario::load(scenario_path("phase2-manhattan.yaml")).expect("the phase 2 loads");
        assert_eq!(
            phase2.actors.rsus.len(),
            1,
            "phase2-manhattan declares the one mast this offset exists for"
        );
        assert_eq!(
            roadside_node_count(&phase2),
            1,
            "with one mast the first vehicle's node id is 1, not 0"
        );
    }
}

#[cfg(test)]
mod signal_stream_tests {
    use super::group_signal_plans;
    use std::collections::BTreeMap;
    use v2xw_core::ids::LaneId;
    use v2xw_mobility::FixedTimeSignals;
    use v2xw_world::model::SignalState;
    use v2xw_world::procedural::GridParams;

    fn rank(s: SignalState) -> u8 {
        match s {
            SignalState::Green => 6,
            SignalState::GreenYield => 5,
            SignalState::FlashingAmber => 4,
            SignalState::Amber => 3,
            SignalState::RedAmber => 2,
            SignalState::Red => 1,
            SignalState::Off => 0,
        }
    }

    /// What the page is shown for every head group equals what the mobility engine's own
    /// signal model shows that group's movements, at every 0.1 s step of two cycles — and
    /// in particular at every change.
    #[test]
    fn the_streamed_signal_state_is_the_engines_at_every_step() {
        let world = v2xw_world::procedural::grid(
            &GridParams {
                lanes_per_direction: 2,
                ..GridParams::legacy().with_signals(true)
            },
            &v2xw_world::ImportOptions::default(),
        )
        .expect("grid");
        let streamed = group_signal_plans(&world);
        let engine = FixedTimeSignals::default();
        let mut approach_of: BTreeMap<LaneId, LaneId> = BTreeMap::new();
        for c in world.roads.connections() {
            if let Some(via) = c.via {
                approach_of.entry(via).or_insert(c.from_lane);
            }
        }
        let mut changes = 0;
        for plan in &world.signals {
            let groups: Vec<u16> = {
                let mut g: Vec<u16> = plan.heads.iter().map(|h| h.group).collect();
                g.sort_unstable();
                g.dedup();
                g
            };
            for group in groups {
                let id = v2xw_world::signal_group_wire_id(plan.id, group);
                let entry = streamed
                    .iter()
                    .find(|e| e.signal.index() == id)
                    .expect("every head group is streamed");
                let mut last = None;
                for k in 0..(2.0 * plan.cycle_s * 10.0) as u64 {
                    let t_s = k as f64 * 0.1;
                    let engine_state = plan
                        .controlled
                        .iter()
                        .filter(|l| {
                            approach_of.get(l).is_some_and(|a| {
                                plan.heads.iter().any(|h| h.lane == *a && h.group == group)
                            })
                        })
                        .filter_map(|l| engine.state_for(plan, *l, t_s))
                        .max_by_key(|s| rank(*s))
                        .expect("the group controls a movement");
                    let (shown, _) = entry.at(t_s).expect("a state");
                    assert_eq!(
                        shown, engine_state,
                        "plan {} group {group} at {t_s} s",
                        plan.id
                    );
                    if last.is_some_and(|l| l != shown) {
                        changes += 1;
                    }
                    last = Some(shown);
                }
            }
        }
        assert!(changes > 100, "the check saw {changes} changes");
        // And no two head groups of a crossroads show a green together.
        let plan = world
            .signals
            .iter()
            .find(|p| world.junction(p.junction).incoming.len() >= 4)
            .expect("a crossroads");
        for k in 0..(plan.cycle_s * 10.0) as u64 {
            let t_s = k as f64 * 0.1;
            let greens = streamed
                .iter()
                .filter(|e| e.signal.index() / 65536 == plan.id.index() + 1)
                .filter(|e| {
                    matches!(
                        e.at(t_s),
                        Some((SignalState::Green | SignalState::GreenYield, _))
                    )
                })
                .count();
            assert!(greens <= 1, "{greens} groups green at {t_s} s");
        }
    }
}

#[cfg(test)]
mod body_centre_tests {
    use super::body_centre;
    use v2xw_mobility::VehicleClass;

    /// A car heading north-east draws half its length ahead of its reference point, and a
    /// longer class further ahead, so a car stopped with its bumper on the line is drawn
    /// short of it by nothing.
    #[test]
    fn the_streamed_pose_is_half_a_length_ahead_of_the_reference() {
        for (i, class) in VehicleClass::ALL.iter().enumerate() {
            let half = class.spec().length_m * 0.5;
            let h = core::f64::consts::FRAC_PI_4;
            let c = body_centre([10.0, 20.0, 1.5], h, u8::try_from(i).unwrap());
            assert!((c[0] - (10.0 + half * h.cos())).abs() < 1e-9, "{class:?} x");
            assert!((c[1] - (20.0 + half * h.sin())).abs() < 1e-9, "{class:?} y");
            assert!((c[2] - 1.5).abs() < 1e-12, "{class:?} keeps its height");
            assert!(half > 0.0, "{class:?} has a length");
        }
    }
}

#[cfg(test)]
mod node_tx_payload_tests {
    use super::node_tx_payload;
    use v2xw_metrics::channels::NodeTxView;

    /// §3.6.10's `pseudonym_digest` carries the signing certificate's HashedId8 when the
    /// record names one, and §0's eight zero bytes when it does not.
    #[test]
    fn the_pseudonym_digest_reaches_the_wire() {
        let mut v: NodeTxView = serde_json::from_value(serde_json::json!({
            "t": 0, "node": 3, "bytes_on_wire": 200, "pseudonym": "0123456789abcdef"
        }))
        .expect("a node.tx view");
        assert_eq!(
            &node_tx_payload(&v)[28..36],
            &[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
        );
        v.pseudonym = None;
        assert_eq!(&node_tx_payload(&v)[28..36], &[0u8; 8]);
    }
}
