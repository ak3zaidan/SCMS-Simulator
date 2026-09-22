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
//! it keeps internally. [`Projector::equip`] reconstructs it from the two published facts
//! that decide it — the equipped draw is
//! `RngRegistry::checkout(RngDomain::Spawn, EntityRef::Actor(a)).bool(equipped_fraction)`,
//! and node ids are handed out in spawn order — and [`Projector::mapping_is_consistent`]
//! reports whether every node the run actually transmitted from was one the
//! reconstruction predicted. That check is the reason the coupling is tolerable and not
//! the reason it is a good idea: the engine should publish the map, and this module says
//! so in one place rather than being quietly wrong in many.
//!
//! # No wall clock
//!
//! Nothing here reads one. The host thread's pacing is the transport's
//! ([`crate::http::producer`]); the engine's own timeline is `SimTime` throughout, and the
//! manifest timestamp is [`LiveOptions::build_utc`], supplied by the caller exactly as
//! `v2xw_engine::Engine::build` requires.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use serde_json::{Value, json};
use v2xw_core::card::Family;
use v2xw_core::ctx::OwnedRecord;
use v2xw_core::ids::{ActorId, LaneId, NodeId, SignalId};
use v2xw_core::time::{Duration, SimTime};
use v2xw_engine::{Scenario, run::RunReport};
use v2xw_metrics::channels::{
    DetObservationView, GtKinematicsView, MacCbrView, NodeTelemetryView, NodeTxView,
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

use crate::engine::{Control, ControlOutcome, Engine, Query, RunDescriptor, RunState, StepOutput};
use crate::error::{Result, ServerError};
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
}

/// Everything about the run the kernel knows before its first step.
///
/// Assembled on the host thread, where the engine lives, and sent across once. Every
/// field of it is `Send`, which is the reason the split exists: the engine is not.
#[derive(Debug)]
struct Setup {
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
    /// Class names by `class_idx`, so the projector can map `gt.kinematics.class`.
    class_names: Vec<String>,
    /// Signal plans as `(signal id, phase boundaries)`, evaluated by the projector.
    signals: Vec<SignalPlan>,
    equipped_fraction: f64,
    actor_capacity: u32,
    seed: u64,
    run_id_bytes: [u8; 16],
    obu_profile: String,
    recording_path: Option<String>,
    /// The `prov_id` a `MetricSample` resolves through: the registered metric-family
    /// model, or `0` when the run installed none.
    metric_prov: u32,
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

/// A handle on the thread the kernel runs on.
#[derive(Debug)]
struct Host {
    steps: Receiver<HostMsg>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Host {
    /// Tells the kernel to stop producing.
    ///
    /// It does not stop the kernel: `v2xw_engine::Engine::run` has no cancellation point,
    /// so the thread runs to the scenario horizon with its output discarded and then ends.
    /// That is stated rather than worked around — the alternative is unwinding through the
    /// kernel from a recorder callback, which is not a thing to do to a simulation.
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
        // Not joined: see `Host::stop`. The thread holds nothing the process needs and
        // exits on its own; joining here would block a shutdown for the length of a run.
        let _ = self.join.take();
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
) -> Result<(Box<Setup>, Host)> {
    let (setup_tx, setup_rx) =
        std::sync::mpsc::channel::<std::result::Result<Box<Setup>, String>>();
    let (step_tx, step_rx) = std::sync::mpsc::sync_channel::<HostMsg>(lookahead.max(1));
    let stop = Arc::new(AtomicBool::new(false));

    let build_utc = options.build_utc.clone();
    let actor_capacity = options.actor_capacity.max(1);
    let label = options.label.clone();
    let token = options.session_token.clone();
    let recording = options.recording.clone();
    let host_stop = Arc::clone(&stop);
    let join = std::thread::Builder::new()
        .name("v2xw-engine".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let mut engine = match v2xw_engine::Engine::build(scenario, &build_utc) {
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
                Ok(s) => s,
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
    let catalogue: Vec<MetricInfo> = providers
        .catalog()
        .into_iter()
        .map(|def| {
            let visibility = crate::visibility_name(def.visibility).to_string();
            MetricInfo {
                str_id: strings.intern(&def.name),
                agg_code: agg_code(&def.agg.tag()),
                name: def.name,
                unit: def.unit,
                agg: def.agg.tag(),
                visibility,
                definition_md: def.definition_md,
                dims: def.dims.iter().map(ToString::to_string).collect(),
                not_accounted: def.not_accounted,
            }
        })
        .collect();

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

    let signals: Vec<SignalPlan> = world
        .signals
        .iter()
        .map(|plan| SignalPlan {
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
        })
        .collect();

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
        class_names,
        signals,
        equipped_fraction: scenario.actors.vehicles.equipped_fraction,
        actor_capacity,
        seed: scenario.seed,
        run_id_bytes,
        obu_profile: scenario.nodes.default_obu.clone(),
        recording_path: recording.map(|p| p.display().to_string()),
        metric_prov,
    }))
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
    next_node: u32,
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
    /// Node ids the records named that the reconstruction did not predict.
    unmapped_nodes: BTreeSet<u32>,
    /// Metric names the stream carried that the symbol table does not hold.
    unnamed_metrics: BTreeSet<String>,
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
        let metric_ids = setup
            .catalogue
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    (m.str_id, m.agg_code, visibility_code_of(&m.visibility)),
                )
            })
            .collect();
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
            next_node: 0,
            signals: setup.signals.clone(),
            str_ids,
            metric_ids,
            metric_prov: setup.metric_prov,
            counters: BTreeMap::new(),
            actor_capacity: setup.actor_capacity,
            over_capacity: BTreeSet::new(),
            unmapped_nodes: BTreeSet::new(),
            unnamed_metrics: BTreeSet::new(),
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
    /// **This is the reconstruction the module header names.** Two published facts decide
    /// it: the draw is keyed by `(RngDomain::Spawn, EntityRef::Actor)` so it does not
    /// depend on spawn order, and node ids are dense and ascending in spawn order. The
    /// caller equips a step's new actors in `ActorId` order, which is spawn order, because
    /// the kernel assigns `ActorId`s ascending at spawn.
    fn equip(&mut self, actor: ActorId) -> Option<NodeId> {
        let equipped = self
            .rng
            .checkout(
                v2xw_core::rng::RngDomain::Spawn,
                v2xw_core::rng::EntityRef::Actor(actor),
            )
            .bool(self.equipped_fraction);
        if !equipped {
            return None;
        }
        let node = NodeId::new(self.next_node);
        self.next_node += 1;
        self.nodes.insert(node, actor);
        Some(node)
    }

    /// True if every node the records named was one the reconstruction predicted.
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
                pos_m: live.pos_m,
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
        }
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
        if !self.nodes.contains_key(&node) {
            self.unmapped_nodes.insert(node.index());
        }
    }

    /// Takes the step's kinematics as the authoritative actor set.
    fn absorb(&mut self, index: u64, t: SimTime, kinematics: &[GtKinematicsView]) {
        let mut present: BTreeSet<ActorId> = BTreeSet::new();
        let mut fresh: Vec<ActorId> = Vec::new();
        for view in kinematics {
            present.insert(view.actor);
            if !self.actors.contains_key(&view.actor) {
                fresh.push(view.actor);
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
            let node = self.equip(actor);
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
    fn push_metric(&mut self, sample: &MetricSample, out: &mut Vec<MetricRow>) {
        let Some((str_metric, agg, visibility)) = self.metric_ids.get(&sample.metric).copied()
        else {
            self.unnamed_metrics.insert(sample.metric.clone());
            return;
        };
        let Some(value) = sample.value.point() else {
            // An insufficient sample is a refusal, not a zero, and §3.7 has no encoding
            // for one. Dropping it is right: the client sees a gap, which is what
            // "insufficient" means.
            return;
        };
        let node_id = match &sample.dims.iter().find(|(d, _)| d.to_string() == "node") {
            Some((_, v)) => v.to_string().parse::<u32>().unwrap_or(U32_NONE),
            None => U32_NONE,
        };
        out.push(MetricRow {
            value,
            str_metric,
            // The dimension dictionary is a §3.8 `Provenance` structure and this build's
            // one metric carries no dimensions; `0` is §3.7's "no dimensions".
            dim_key: 0,
            node_id,
            count: u32::try_from(sample.value.n()).unwrap_or(u32::MAX),
            agg,
            visibility,
            prov_id: self.metric_prov,
        });
        out.sort_by_key(|r| (r.str_metric, r.node_id));
    }
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
    // The digest itself is in the envelope, not in the record; the record names the signer
    // *kind*. Eight zero bytes is §0's "absent" for a byte array.
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
        Self::new(scenario, options)
    }

    /// Builds the kernel from a scenario already in hand.
    ///
    /// # Errors
    /// As [`LiveEngine::open`].
    pub fn new(scenario: Scenario, options: LiveOptions) -> Result<Self> {
        let (setup, host) = spawn_host(scenario.clone(), &options, options.lookahead_steps)?;
        let projector = Projector::new(&setup);
        let metric_names = setup
            .catalogue
            .iter()
            .map(|m| (m.str_id, m.name.clone()))
            .collect();
        let descriptor = RunDescriptor {
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
            // which it could not be anyway, because `Run` snapshots this descriptor.
            seekable: true,
            scenario: setup.scenario_doc.clone(),
            scenario_hash_hex: setup.scenario_hash_hex.clone(),
            recording_path: setup.recording_path.clone(),
            provenance: Some(setup.provenance.clone()),
        };
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
            // Retention is the only bound on how far the kernel may run ahead once a
            // client is attached: past it, the channel stays full and the kernel stops in
            // `StepRecorder::send`.
            if self.timeline.len() >= self.options.retain_steps && self.base_index >= self.cursor {
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

    /// Restarts the run from `t = 0` on a fresh kernel thread.
    fn restart(&mut self) -> Result<()> {
        let (setup, host) = spawn_host(
            self.scenario.clone(),
            &self.options,
            self.options.lookahead_steps,
        )?;
        self.projector = Projector::new(&setup);
        self.setup = setup;
        self.host = host;
        self.timeline.clear();
        self.base_index = 0;
        self.cursor = 0;
        self.produced = 0;
        self.report = None;
        self.failure = None;
        self.history.clear();
        self.last_telemetry.clear();
        // The string table is *not* cleared: a restart reuses this run's id and its
        // descriptor, so an id a client already resolved must keep meaning what it meant.
        Ok(())
    }
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
        self.cursor.saturating_mul(self.step_ns())
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
            } => {
                if self.state == RunState::Running {
                    return Err(ServerError::RunAlreadyRunning);
                }
                if let Some(seed) = seed
                    && seed != self.scenario.seed
                {
                    return Err(ServerError::NotSupportedHere(format!(
                        "this run is pinned to seed {}: a different seed is a different run, \
                         and the run id, the `Hello` and the world are fixed when the server \
                         binds. Start the server on the scenario with `--seed {seed}`.",
                        self.scenario.seed
                    )));
                }
                self.restart()?;
                self.speed = speed;
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
                self.host.stop();
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
