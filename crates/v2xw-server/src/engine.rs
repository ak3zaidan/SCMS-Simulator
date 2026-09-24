//! The seam between this crate and the simulation engine.
//!
//! This trait is defined here, in the transport, rather than in `v2xw-engine`, because
//! what the transport needs is *state per mobility step* and what the kernel offers is a
//! run loop and a record stream. There are three implementations in-tree:
//!
//! * [`crate::live::LiveEngine`] — a real `v2xw-engine` run (build decision D8). This is
//!   the one that matters; the other two exist around it.
//! * [`crate::stub::StubEngine`] — a deterministic synthetic run on a procedurally
//!   generated grid world, kept because it is what lets the framing and session tests run
//!   without importing a city, and because it is the fixture a client developer can point
//!   at with no scenario at all.
//! * [`crate::replay::ReplayEngine`] — a run served from an MCAP recording through
//!   `v2xw-record`'s reader, which is the replay mode of §7.
//!
//! Nothing in [`crate::session`], [`crate::rpc`] or [`crate::http`] refers to any of them
//! by name.
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
    /// Which run of this process produced the step: [`crate::Run`] stamps it with the
    /// generation current when the step was taken.
    ///
    /// A connection compares it with the generation its own `Hello` described and never
    /// encodes a step of another run against that `Hello`'s tables. Before this existed a
    /// rewind handed the new run's step 0 to a snapshot encoder still holding the old run's
    /// last step; the encoder refused a step that did not advance, and the transport ended
    /// the connection — the page's socket died on every "Run again" and came back only by
    /// reconnecting. Engines leave it `0`; only the run sets it.
    pub generation: u64,
}

impl StepOutput {
    /// Roughly how much memory this step occupies, in bytes.
    ///
    /// An estimate, and it says so: it counts the fixed size of every row plus the length
    /// of every variable-length payload, and it does not count an allocator's slack or a
    /// `Vec`'s spare capacity. It exists because the retained-history window has to be
    /// bounded in **bytes** and not only in steps — a step is one scene plus that step's
    /// events, so its size scales with the fleet, and 36 000 steps of a thousand vehicles
    /// is not the same quantity of memory as 36 000 steps of ten. Under-counting by a
    /// constant factor is acceptable for a budget; not counting at all is not.
    pub fn approx_bytes(&self) -> usize {
        use core::mem::size_of;
        let poses = self.snapshot.actors.len() * size_of::<v2xw_record::encoder::ActorPose>();
        let signals = self.snapshot.signals.len() * size_of::<v2xw_record::encoder::SignalState>();
        // A `BTreeMap` node is bigger than its entries, so a per-entry estimate of the
        // key, the value and two pointers is a floor rather than a figure.
        let causes = (self.snapshot.spawn_causes.len() + self.snapshot.despawn_causes.len())
            * (size_of::<u32>() + size_of::<u16>() + 2 * size_of::<usize>());
        let telemetry = self.telemetry.len() * size_of::<NodeTelemetry>();
        let events: usize = self
            .events
            .iter()
            .map(|e| size_of::<EventEntry>() + e.payload.len())
            .sum();
        let metrics = self.metrics.len() * size_of::<MetricRow>();
        let recorded: usize = self
            .recorded
            .iter()
            .map(|f| size_of::<v2xw_record::wire::Frame>() + f.as_bytes().len())
            .sum();
        size_of::<StepOutput>() + poses + signals + causes + telemetry + events + metrics + recorded
    }
}

/// What history a run is keeping, and what it has already let go of.
///
/// `run.status` publishes this because 13-product-direction.md §3 asks for a retention
/// policy that "does not silently discard what the user wants to seek to". It cannot be
/// unbounded — a run of minutes at a useful fleet size would fill the machine — so the
/// honest alternative is that it is bounded and the bound is *visible*: the page can show
/// how far back the run is still scrubbable, and it can tell the difference between "that
/// instant was never produced" and "that instant has been dropped".
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Retention {
    /// Steps currently held.
    pub retained_steps: u64,
    /// Roughly how many bytes they occupy; see [`StepOutput::approx_bytes`].
    pub retained_bytes: u64,
    /// The step-count ceiling (`--retain`).
    pub limit_steps: u64,
    /// The byte ceiling (`--retain-bytes`); `0` for no byte ceiling.
    pub limit_bytes: u64,
    /// Steps dropped from the back of the window since the run started.
    ///
    /// Non-zero means the seekable floor has moved: an instant the page could once seek
    /// to is gone. It is a count rather than a flag so the page can say how much.
    pub dropped_steps: u64,
    /// Which ceiling the window is against now: `"steps"`, `"bytes"` or `""`.
    pub binding: &'static str,
}

/// One node as the engine knows it *now*, with its strings unresolved.
///
/// §3.1.3's node table is "the set known at connect time", and for a live run that set
/// grows: the Phase 1 Manhattan scenario has no vehicle at all until its demand model
/// produces one. [`RunDescriptor::hello`] is fixed when the run is wrapped, so a run whose
/// nodes appear later reports them through [`Engine::live_nodes`] instead, and the session
/// interns the two strings into the table its own `Hello` establishes.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeFacts {
    /// The node id.
    pub node_id: u32,
    /// The actor it is mounted on, or `0xFFFF_FFFF`.
    pub actor_id: u32,
    /// Its position now (a mobile node's position at `t0` in a static table).
    pub pos_m: [f32; 3],
    /// The label, e.g. `veh_0421`.
    pub label: String,
    /// The hardware profile id, e.g. `obu/unex-obu-301-craton2`.
    pub profile_id: String,
    /// §3.1.3's `flags`.
    pub flags: u16,
    /// §3.1.3's `kind`.
    pub kind: u8,
    /// Index into the class table, `0xFF` if not an actor.
    pub class_idx: u8,
}

/// Where the scenario a `run.start` runs comes from (§6.6's `scenario` parameter).
#[derive(Debug, Clone)]
pub enum ScenarioSource {
    /// An inline scenario document.
    Document(Value),
    /// A preset id from `scenario.list`, or a path the engine can read.
    Preset(String),
}

/// What `scenario.set` / `scenario.load` asks an engine to hold for its next run.
#[derive(Debug, Clone)]
pub enum StageRequest {
    /// A whole scenario document.
    Document(Value),
    /// An RFC 6902 patch against the scenario the next run would otherwise use.
    Patch(Vec<Value>),
    /// A preset id or a path, as `scenario.load` names one.
    Preset(String),
}

/// A scenario an engine is holding for its next run: the answer to `scenario.set`.
#[derive(Debug, Clone)]
pub struct Staged {
    /// The resolved document, as `scenario.get` would return it once it runs.
    pub document: Value,
    /// Its digest, which is the next run's `Hello.scenario_hash`.
    pub hash: String,
    /// Every JSON Pointer whose value differs from the running scenario's, sorted.
    pub changed: Vec<String>,
    /// Loader errors; empty means the next `run.start` will run it.
    pub errors: Vec<crate::error::ParamError>,
}

/// A run-control command: the engine-facing half of §6.6.
#[derive(Debug, Clone)]
pub enum Control {
    /// `run.start`.
    Start {
        /// Start paused at `t = 0`.
        paused: bool,
        /// Multiple of real time; `0` is unthrottled. `None` keeps the speed the run
        /// has now, so a rewind of a server started with `--speed 0` stays unthrottled.
        speed: Option<f64>,
        /// Seed override.
        seed: Option<u64>,
        /// The scenario to run instead of the staged or current one.
        scenario: Option<ScenarioSource>,
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
        /// `where`: the other dimensions a grouped sample must carry, exactly.
        filter: std::collections::BTreeMap<String, String>,
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

    /// The metric catalogue this run can answer for (§6.12).
    ///
    /// On [`Engine`] rather than on [`crate::introspect::Introspect`] because the RPC
    /// layer needs it before it dispatches: §6.9 and §6.12 refuse a ground-truth metric on
    /// a `node`-profile connection, and that check is a visibility rule, not an
    /// introspection answer.
    fn metric_catalogue(&self) -> Vec<crate::introspect::MetricInfo> {
        Vec::new()
    }

    /// The node table as of now, for an engine whose nodes appear during the run.
    ///
    /// `None` means the run's nodes are exactly [`RunDescriptor::hello`]`.nodes` and were
    /// known when it started, which is true of the fixture and of every replay. A live
    /// engine returns `Some`, and the transport uses it for `Hello`'s node table, for
    /// `view.follow`'s radius query and for `inspect.node`.
    fn live_nodes(&self) -> Option<Vec<NodeFacts>> {
        None
    }

    /// Strings this run has appended to [`RunDescriptor::hello`]'s symbol table since it
    /// started, in the order it appended them.
    ///
    /// A run whose actors appear during it needs a string per node label, and §2.5 makes
    /// the symbol table append-only: an id must mean the same string on every connection
    /// of the run, or a client that cached one is wrong. So the labels are kept run-scoped
    /// and **append-only even across a despawn** — a node that leaves does not free its
    /// label's id — and the session appends this list to the table its `Hello`
    /// establishes. Without that, two connections made either side of a despawn would
    /// disagree about what every id after it meant.
    fn live_strings(&self) -> Vec<String> {
        Vec::new()
    }

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

    /// Lets the run compute ahead towards `t` for up to `budget` of wall time, and returns
    /// how far the seekable range now reaches, in simulated nanoseconds.
    ///
    /// A live kernel runs only a bounded distance ahead of the stream (see
    /// `LiveOptions::lookahead_steps`), so a seek past that distance has nothing to show
    /// until the kernel has been let run there. The transport calls this in slices, with the
    /// run lock released between them, and reports progress to the client as it goes.
    ///
    /// The default is the seekable range as it is: a replay and the fixture already reach
    /// the end of the run. `budget` is a **transport** wait, like §1.5's stall timeout: no
    /// simulated quantity depends on it.
    fn extend_to(&mut self, _t: SimTime, _budget: std::time::Duration) -> u64 {
        self.seek_range().1
    }

    /// The end of the run, simulated nanoseconds: how far a seek may ever reach.
    fn horizon_ns(&self) -> u64 {
        self.descriptor().duration
    }

    /// What history this run is keeping (§6.5's `retention`).
    ///
    /// The default is "keeps everything", which is true of a replay — the recording is on
    /// disk and nothing is evicted — and of the synthetic fixture, which recomputes rather
    /// than retains. Only a live run has a window to report.
    fn retention(&self) -> Retention {
        Retention::default()
    }

    /// Answers an introspection query (§6.8–§6.13) as the JSON its result schema names.
    ///
    /// # Errors
    /// `-32006` for an unknown id, `-32007` for an unknown metric, `-32008` for a failed
    /// export, `-32009` where the query does not apply to this kind of run.
    fn query(&mut self, query: &Query) -> Result<Value>;

    /// The `vwp-world/1` JSON form of [`Engine::world`], when this engine can produce it.
    ///
    /// Asked after every `run.start`, because a started run may be on a different world:
    /// `None` keeps the form the run was wrapped with, which is right for an engine whose
    /// world never changes.
    fn world_json(&self) -> Option<String> {
        None
    }

    /// The state at the stream position, for a connection that attaches to a run that is
    /// not advancing.
    ///
    /// §1.3 rule 4 has a server send nothing after `Hello` until the run moves, which is
    /// right for a run that has not started and wrong for one that is paused half-way or
    /// finished: the page that attaches to it — a reload, a second tab, a reconnect after
    /// the engine restarted — would show an empty city until somebody pressed play.
    /// `None` (the default) keeps §1.3's behaviour.
    fn current(&self) -> Option<StepOutput> {
        None
    }

    /// Holds a scenario for the next `run.start` (`scenario.set`, `scenario.load`).
    ///
    /// # Errors
    /// `-32009` for an engine that runs no scenario (the default), `-32602` for a
    /// document that is not a scenario at all.
    fn stage(&mut self, request: StageRequest) -> Result<Staged> {
        let _ = request;
        Err(crate::error::ServerError::NotSupportedHere(
            "this engine runs no scenario document, so there is nothing to set; start a \
             server with `--scenario` to edit one"
                .to_string(),
        ))
    }

    /// The scenario held for the next run, if one differs from the running one.
    fn staged(&self) -> Option<Staged> {
        None
    }

    /// Checks a scenario document with the loader's own rules: `(errors, warnings)`.
    ///
    /// `None` (the default) means the engine has no loader, and the RPC layer falls back
    /// to its structural checks.
    fn validate_document(
        &self,
        doc: &Value,
    ) -> Option<(Vec<crate::error::ParamError>, Vec<crate::error::ParamError>)> {
        let _ = doc;
        None
    }

    /// The scenarios `scenario.list` offers and `scenario.load` / `run.start` accept, as
    /// `#/$defs/ScenarioListItem` objects.
    fn presets(&self) -> Vec<Value> {
        Vec::new()
    }

    /// Engine facts for `run.status` beyond the schema's required members.
    fn diagnostics(&self) -> Value {
        Value::Object(serde_json::Map::new())
    }
}
