//! The seam between this crate and the simulation engine.
//!
//! `v2xw-engine` (build decision D8) is being built in parallel with this crate, so the
//! transport is written against the trait it needs rather than against the engine it does
//! not yet have. Everything below is defined here, in this crate, and there are two
//! implementations in-tree:
//!
//! * [`crate::stub::StubEngine`] — a deterministic synthetic run on a procedurally
//!   generated grid world. It is what the tests and the `--stub` binary mode drive, and
//!   it is a real implementation of the trait, not a panicking placeholder.
//! * [`crate::replay::ReplayEngine`] — a run served from an MCAP recording through
//!   `v2xw-record`'s reader, which is the replay mode of §7.
//!
//! When the engine crate lands, wiring it in is one `impl Engine for …` block plus the
//! one line in [`crate::ServerOptions`] that chooses which implementation the run holds.
//! Nothing in [`crate::session`], [`crate::rpc`] or [`crate::http`] refers to either
//! implementation by name.
//!
//! # Why the engine hands over state and not frames
//!
//! A connection's stream depends on the connection: its profile blanks ground-truth
//! columns at the producer (§5.3), its `view.follow` subscription decides which nodes are
//! in a `Telemetry` frame (§6.7), and its `events.set` subscription decides which channels
//! are in an `Event` frame (§6.12). The engine therefore produces *state* — one
//! [`StepOutput`] per mobility step, ground truth included — and the session encodes the
//! frames it is allowed to send. There is one encoder, `v2xw-record`'s, on both paths.
//!
//! # No wall clock
//!
//! Nothing in this trait reads a clock. [`Engine::step`] advances by exactly one mobility
//! step of simulated time whenever it is called; how often it is called is the transport's
//! business, and the transport is the only place in this crate that looks at wall time
//! (§1.5's pacing, §1.2's ping interval).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use v2xw_core::ids::NodeId;
use v2xw_core::time::SimTime;
use v2xw_record::encoder::{Cadence, Snapshot};
use v2xw_record::wire::event::EventEntry;
use v2xw_record::wire::hello::HelloBody;
use v2xw_record::wire::metric::MetricRow;
use v2xw_record::wire::provenance::ProvenanceBody;
use v2xw_record::wire::telemetry::NodeTelemetry;
use v2xw_world::WorldPayload;

use crate::error::Result;

/// The run states of `#/$defs/RunState` (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    /// No run has been started.
    Idle,
    /// A scenario is being loaded.
    Loading,
    /// Advancing.
    Running,
    /// Exists but not advancing.
    Paused,
    /// Servicing a `run.seek`.
    Seeking,
    /// Reached `t_end_ns`.
    Finished,
    /// Aborted.
    Error,
}

impl RunState {
    /// The `#/$defs/RunState` token.
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Idle => "idle",
            RunState::Loading => "loading",
            RunState::Running => "running",
            RunState::Paused => "paused",
            RunState::Seeking => "seeking",
            RunState::Finished => "finished",
            RunState::Error => "error",
        }
    }
}

/// Everything about a run that does not change while it exists.
///
/// This is what a `Hello` is built from (§3.1). The engine owns it because every field in
/// it — the run id, the scenario digest, the node and class tables, the cadence — is a
/// property of the run, not of a connection.
#[derive(Debug, Clone)]
pub struct RunDescriptor {
    /// The run id as a 36-character UUID string, for `?run=` and every JSON-RPC result.
    pub run_id: String,
    /// The raw 16 UUID bytes `Hello.run_id` carries (§3.1.1).
    pub run_id_bytes: [u8; 16],
    /// A `Hello` body with every run-scoped field filled in and its own symbol table.
    ///
    /// The session copies it, overwrites the four connection-scoped fields
    /// (`hello_flags`, `resume_seq`, `sim_time_ns` and the session-token string) and
    /// sends that. §7.2's note that `Hello` is not covered by byte-identity is exactly
    /// this split.
    pub hello: HelloBody,
    /// The cadence (§3.1.1); also the GOP length the resume ring is sized from.
    pub cadence: Cadence,
    /// The quantisation origin, `floor(bbox_min)` per axis with `z = 0` (§3.3.1).
    pub origin_m: [f64; 3],
    /// The scenario duration; `run.status.t_end_ns`.
    pub duration: SimTime,
    /// Whether the stream is produced by a live engine (`true`) or a recording (`false`).
    /// Sets `HELLO_LIVE` or `HELLO_REPLAY` (§3.1.2).
    pub live: bool,
    /// Whether `run.seek` is available on this run (`HELLO_SEEKABLE`).
    pub seekable: bool,
    /// The scenario document `scenario.get` returns.
    pub scenario: Value,
    /// The scenario digest, hex, as `Hello.scenario_hash` and `scenario.get.hash`.
    pub scenario_hash_hex: String,
    /// The path of the recording this run writes, if it writes one.
    pub recording_path: Option<String>,
    /// The provenance every `prov_id` in the run resolves through (§3.8).
    ///
    /// It is run-scoped but delivered per connection: §3.8 requires at least one
    /// `Provenance` frame "immediately after the first `Keyframe`", and *first* means the
    /// first keyframe **this connection** received. A run-scoped delivery would send it
    /// once, to whichever client happened to be attached at step 0, and every later client
    /// would decode a `MetricSample` whose `prov_id` resolved to nothing — which is
    /// conformance C5 failing silently.
    pub provenance: Option<ProvenanceBody>,
}

/// One mobility step of engine state: everything a connection could be shown.
///
/// Ground truth is present in every field; the session blanks what its profile forbids.
#[derive(Debug, Clone, Default)]
pub struct StepOutput {
    /// The step's simulated time.
    pub sim_time: SimTime,
    /// Actor poses and signal states for the step (§3.3, §3.4).
    pub snapshot: Snapshot,
    /// One record per node the engine models, in node-id order (§3.5.2). The session
    /// sends the subscribed subset.
    pub telemetry: Vec<NodeTelemetry>,
    /// Every event the step produced, in `(sim_time, channel_id)` order (§3.6.1). The
    /// session sends the subscribed channels.
    pub events: Vec<EventEntry>,
    /// Metric samples whose bin ended at this step (§3.7).
    pub metrics: Vec<MetricRow>,
    /// A provenance frame to send before anything that references its ids (§3.8, C5).
    pub provenance: Option<ProvenanceBody>,
    /// Set on the last step of the run; the session sets `FLAG_END_OF_RUN`.
    pub end_of_run: bool,
    /// Canonical frames to forward **verbatim** instead of encoding the state above.
    ///
    /// Non-empty only in replay (§7). §7.2's guarantee is that a canonical frame's header
    /// (masked to `CANONICAL_FLAG_MASK`) and body are byte-identical live and replayed,
    /// and the only way to keep that is to not re-encode: the reader hands back what the
    /// recorder stored and the session passes it to the socket. When this is non-empty the
    /// session ignores `snapshot`, `telemetry`, `events` and `metrics`.
    pub recorded: Vec<v2xw_record::wire::Frame>,
}

/// A run-control command: the engine-facing half of §6.6.
#[derive(Debug, Clone)]
pub enum Control {
    /// `run.start`.
    Start {
        /// Start paused at `t = 0`.
        paused: bool,
        /// Multiple of real time; `0` is unthrottled.
        speed: f64,
        /// Seed override.
        seed: Option<u64>,
    },
    /// `run.pause`.
    Pause,
    /// `run.resume`.
    Resume,
    /// `run.speed`.
    Speed {
        /// Multiple of real time.
        speed: f64,
        /// `false` = `free`, `true` = pace the producer to the calling connection.
        client_sync: bool,
    },
    /// `run.stop`.
    Stop {
        /// Finalise exports before replying.
        finalize_exports: bool,
    },
}

/// What a [`Control`] did.
#[derive(Debug, Clone)]
pub struct ControlOutcome {
    /// The state afterwards.
    pub state: RunState,
    /// The sim time the reply reports.
    pub t_ns: u64,
    /// Extra members the specific method's result schema names (`digest`, `files`, …).
    pub extra: BTreeMap<String, Value>,
}

/// A structured introspection request: the engine-facing half of §6.8–§6.13.
///
/// The RPC layer validates every parameter before building one of these, so an engine
/// implementation never sees a malformed request and never has to produce a `-32602`.
#[derive(Debug, Clone)]
pub enum Query {
    /// `inspect.node` (§6.8).
    Node {
        /// The node.
        node: NodeId,
        /// The sim time, defaulting to now.
        t_ns: Option<u64>,
        /// Which sections to include.
        include: Vec<String>,
        /// Row cap for list-valued sections.
        limit: usize,
    },
    /// `inspect.link` (§6.8), radio form.
    Link {
        /// Transmitter.
        tx: NodeId,
        /// Receiver.
        rx: NodeId,
        /// The sim time, defaulting to now.
        t_ns: Option<u64>,
        /// The averaging window.
        window_ns: u64,
    },
    /// `inspect.link` (§6.8), named-link form.
    NamedLink {
        /// The backend or backhaul link id.
        link: String,
        /// The sim time, defaulting to now.
        t_ns: Option<u64>,
        /// The averaging window.
        window_ns: u64,
    },
    /// `inspect.entity` (§6.8).
    Entity {
        /// Role or instance id.
        entity: String,
        /// The sim time, defaulting to now.
        t_ns: Option<u64>,
        /// Row cap.
        limit: usize,
    },
    /// `explain` (§6.9).
    Explain {
        /// The `ValueRef` as the caller sent it.
        subject: Value,
        /// How many upstream hops to follow.
        depth: u8,
        /// `json` or `markdown`.
        markdown: bool,
    },
    /// `metrics.query` (§6.12) with an explicit metric list.
    Metrics {
        /// The metric names.
        metrics: Vec<String>,
        /// Inclusive lower time bound.
        t_from_ns: Option<u64>,
        /// Inclusive upper time bound.
        t_to_ns: Option<u64>,
        /// Bin width.
        bin_ns: u64,
        /// Grouping columns.
        group_by: Vec<String>,
        /// Row cap.
        limit: usize,
    },
    /// `metrics.query` with no metric list: return the catalogue (§6.12).
    MetricCatalogue,
    /// `metrics.plot` (§6.12).
    Plot {
        /// The metrics to plot.
        metrics: Vec<String>,
        /// The x axis.
        x: String,
        /// The chart kind.
        kind: String,
    },
    /// `export.dataset` (§6.12).
    ExportDataset {
        /// Which exporter.
        exporter: String,
        /// Output directory.
        out_dir: Option<String>,
        /// `node`, `gt` or `both`.
        visibility: String,
    },
    /// `export.recording` (§6.12).
    ExportRecording {
        /// Output path.
        path: Option<String>,
        /// `full` or `node`.
        profile: String,
    },
}

/// What the server needs from a simulation engine or a recording.
///
/// Implementations hold their own state and are driven from one place at a time: the run
/// holds the engine behind a mutex and only the producer task and the RPC dispatcher touch
/// it, never concurrently.
pub trait Engine: Send + std::fmt::Debug {
    /// The run-scoped facts a `Hello` and a `run.status` are built from.
    fn descriptor(&self) -> &RunDescriptor;

    /// The `vwp-world/1` payload this run's world serialises to (§4).
    ///
    /// Shared rather than copied: several connections and every HTTP `GET /world/{hash}`
    /// serve the same bytes, and the payload digest is what `Hello.world_hash` carries.
    fn world(&self) -> &Arc<WorldPayload>;

    /// The current run state.
    fn state(&self) -> RunState;

    /// The current stream position in simulated time.
    fn sim_time(&self) -> SimTime;

    /// The speed multiple and whether the producer is paced to a client (§6.6).
    fn speed(&self) -> (f64, bool);

    /// Counts for `run.status`: `(actors, nodes)`.
    fn counts(&self) -> (u32, u32);

    /// Applies a run-control command (§6.6).
    ///
    /// # Errors
    /// The `-32001` / `-32002` / `-32009` cases of §6.4, as the method schemas list them.
    fn control(&mut self, command: Control) -> Result<ControlOutcome>;

    /// Advances one mobility step and returns what it produced.
    ///
    /// `Ok(None)` means the run has reached its end; the caller stops asking. Time
    /// advances by exactly [`RunDescriptor::cadence`]`.mobility_step`, which is what makes
    /// the produced stream a function of the step index and nothing else.
    ///
    /// # Errors
    /// [`crate::ServerError::Internal`] if the engine aborted.
    fn step(&mut self) -> Result<Option<StepOutput>>;

    /// Positions the run at `t` and returns the state to send, oldest first (§7.3).
    ///
    /// The returned vector is the keyframe-bearing step followed by every step up to and
    /// including `t`, so the session can emit a `FLAG_SEEK_RESULT | FLAG_RESYNC` keyframe
    /// and the deltas after it. The engine guarantees the first element is at or before
    /// `t` and that there are at most `keyframe_period / mobility_step` elements
    /// (conformance P4).
    ///
    /// # Errors
    /// [`crate::ServerError::SeekOutOfRange`] outside the run, or
    /// [`crate::ServerError::NotSupportedHere`] on a live run with no recorded time.
    fn seek(&mut self, t: SimTime) -> Result<Vec<StepOutput>>;

    /// The seekable sim-time range, `(min_ns, max_ns)`, for `-32003`'s `data`.
    fn seek_range(&self) -> (u64, u64);

    /// Answers an introspection query (§6.8–§6.13) as the JSON its result schema names.
    ///
    /// # Errors
    /// `-32006` for an unknown id, `-32007` for an unknown metric, `-32008` for a failed
    /// export, `-32009` where the query does not apply to this kind of run.
    fn query(&mut self, query: &Query) -> Result<Value>;
}
