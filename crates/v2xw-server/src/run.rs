//! A run: the engine behind a lock, the world bytes, and the step stream every connection
//! reads from.
//!
//! One [`Run`] per engine process, which is what §1.1's "one WebSocket endpoint per engine
//! process" implies. The engine is stepped from exactly one place at a time — the producer
//! task while the run is advancing, the RPC dispatcher while it is paused — and the lock
//! makes that a compile-time-checked fact rather than a convention.
//!
//! # The stream is a broadcast of state, not of frames
//!
//! Every connection gets its own [`tokio::sync::broadcast`] subscription to
//! [`crate::engine::StepOutput`] and encodes its own frames from it. That is not an
//! optimisation, it is a requirement: profile blanking happens at the producer (§5.3), and
//! `Telemetry` and `Event` frames carry only what the connection subscribed to (§6.7,
//! §6.12), so there is no single frame that would be correct for two connections.
//!
//! The channel is bounded. A connection that falls far enough behind gets a
//! `RecvError::Lagged`, which the transport turns into exactly what §1.5 prescribes for a
//! drop: a resync keyframe and a `stream.drop` notification.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};
use v2xw_core::time::SimTime;
use v2xw_world::WorldPayload;

use crate::engine::{Control, ControlOutcome, Engine, Query, RunDescriptor, RunState, StepOutput};
use crate::error::Result;

/// How many steps of backlog a connection may accumulate before it is told it lagged.
///
/// Sized from §1.5's `max_queued_frames` of 64: a connection that cannot keep 64 frames
/// moving is already shedding load in its own send queue, so a deeper broadcast buffer
/// would only delay the resync that §1.5 wants within `resync_deadline_ms`.
pub const STEP_CHANNEL_CAPACITY: usize = 64;

/// How many run-scoped notifications a connection may fall behind by. They are rare — a
/// handful per run — so a connection that misses 64 has stopped reading altogether.
pub const NOTICE_CHANNEL_CAPACITY: usize = 64;

/// Everything about a run that a `run.start` can replace.
///
/// Swapped as one value, under one lock, so a reader never sees a descriptor from one run
/// beside a world from another.
#[derive(Debug)]
struct Shape {
    descriptor: Arc<RunDescriptor>,
    world: Arc<WorldPayload>,
    world_json: Arc<String>,
    node_ids: Vec<u32>,
    node_positions: BTreeMap<u32, [f32; 3]>,
}

impl Shape {
    fn of(descriptor: RunDescriptor, world: Arc<WorldPayload>, world_json: Arc<String>) -> Self {
        let node_ids: Vec<u32> = descriptor.hello.nodes.iter().map(|n| n.node_id).collect();
        let node_positions = descriptor
            .hello
            .nodes
            .iter()
            .map(|n| (n.node_id, n.pos_m))
            .collect();
        Shape {
            descriptor: Arc::new(descriptor),
            world,
            world_json,
            node_ids,
            node_positions,
        }
    }
}

/// A live or replayed run.
///
/// # Generations
///
/// One process serves one run at a time and many runs over its life: every `run.start`
/// begins a new one, possibly on another scenario, seed and world. Each is a
/// *generation*, numbered from 0. A step is stamped with the generation that produced
/// it, and every connection is told when the number moves, so it can send a fresh `Hello`
/// (§6.6: "the server sends a fresh `Hello` on this connection before the first
/// `Keyframe` of the new run") and never encode one run's step against another run's
/// tables.
#[derive(Debug)]
pub struct Run {
    engine: Mutex<Box<dyn Engine>>,
    shape: RwLock<Arc<Shape>>,
    tx: broadcast::Sender<Arc<StepOutput>>,
    /// Run-scoped notifications (§6.14 `run.state`), fanned out to every connection.
    notices: broadcast::Sender<Value>,
    generation: watch::Sender<u64>,
    experiments: Mutex<BTreeMap<String, usize>>,
}

impl Run {
    /// Wraps an engine as a run, serialising its world to both payload forms up front.
    ///
    /// # Errors
    /// [`ServerError::Internal`] if the world's JSON form cannot be produced.
    pub fn new(engine: Box<dyn Engine>, world_json: String) -> Result<Arc<Self>> {
        let descriptor = engine.descriptor().clone();
        let world = Arc::clone(engine.world());
        let (tx, _) = broadcast::channel(STEP_CHANNEL_CAPACITY);
        let (notices, _) = broadcast::channel(NOTICE_CHANNEL_CAPACITY);
        let (generation, _) = watch::channel(0);
        Ok(Arc::new(Run {
            engine: Mutex::new(engine),
            shape: RwLock::new(Arc::new(Shape::of(descriptor, world, Arc::new(world_json)))),
            tx,
            notices,
            generation,
            experiments: Mutex::new(BTreeMap::new()),
        }))
    }

    fn shape(&self) -> Arc<Shape> {
        Arc::clone(&self.shape.read())
    }

    /// The run-scoped facts of the current generation.
    pub fn descriptor(&self) -> Arc<RunDescriptor> {
        Arc::clone(&self.shape().descriptor)
    }

    /// The `vwp-world/1` binary payload of the current generation.
    pub fn world(&self) -> Arc<WorldPayload> {
        Arc::clone(&self.shape().world)
    }

    /// The `vwp-world/1` JSON payload (§4.6) of the current generation.
    pub fn world_json(&self) -> Arc<String> {
        Arc::clone(&self.shape().world_json)
    }

    /// The current generation: how many runs this process has started.
    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }

    /// A receiver that wakes whenever a new run starts.
    pub fn watch_generation(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    /// A subscription to the run-scoped notifications.
    pub fn subscribe_notices(&self) -> broadcast::Receiver<Value> {
        self.notices.subscribe()
    }

    /// Sends a notification to every connection.
    pub fn notify(&self, notice: Value) {
        // No receiver is not a failure: nobody is watching.
        let _ = self.notices.send(notice);
    }

    /// Re-reads the run-scoped facts from the engine and moves to the next generation.
    ///
    /// Called with the engine lock held, so a step of the new run cannot be broadcast
    /// under the old generation number: [`Run::tick`] stamps under the same lock.
    fn begin_generation(&self, engine: &dyn Engine) {
        let previous = self.shape();
        let world_json = match engine.world_json() {
            Some(json) => Arc::new(json),
            None => Arc::clone(&previous.world_json),
        };
        let shape = Shape::of(
            engine.descriptor().clone(),
            Arc::clone(engine.world()),
            world_json,
        );
        *self.shape.write() = Arc::new(shape);
        self.generation.send_modify(|g| *g += 1);
    }

    /// A subscription to the step stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StepOutput>> {
        self.tx.subscribe()
    }

    /// The current run state.
    pub fn state(&self) -> RunState {
        self.engine.lock().state()
    }

    /// The current stream position.
    pub fn sim_time(&self) -> SimTime {
        self.engine.lock().sim_time()
    }

    /// The speed multiple and whether the producer is paced to a client.
    pub fn speed(&self) -> (f64, bool) {
        self.engine.lock().speed()
    }

    /// `(actors, nodes)` for `run.status`.
    pub fn counts(&self) -> (u32, u32) {
        self.engine.lock().counts()
    }

    /// The seekable range, for `-32003`.
    pub fn seek_range(&self) -> (u64, u64) {
        self.engine.lock().seek_range()
    }

    /// Applies a run-control command.
    ///
    /// # Errors
    /// Whatever the engine refuses: `-32001`, `-32002` or `-32009`.
    pub fn control(&self, command: Control) -> Result<ControlOutcome> {
        let starts = matches!(command, Control::Start { .. });
        let mut engine = self.engine.lock();
        let outcome = engine.control(command)?;
        if starts {
            self.begin_generation(engine.as_ref());
        }
        drop(engine);
        Ok(outcome)
    }

    /// Answers an introspection query.
    ///
    /// # Errors
    /// Whatever the engine refuses: `-32006`, `-32007`, `-32008` or `-32009`.
    pub fn query(&self, query: &Query) -> Result<Value> {
        self.engine.lock().query(query)
    }

    /// The followed node's message feed; see [`crate::engine::Engine::node_feed`].
    pub fn node_feed(
        &self,
        node: u32,
        after: Option<SimTime>,
        limits: &crate::feed::FeedLimits,
        gt: bool,
    ) -> Option<Value> {
        self.engine.lock().node_feed(node, after, limits, gt)
    }

    /// Advances one step and broadcasts what it produced.
    ///
    /// Returns `false` at the end of the run. Called from the producer task while the run
    /// is advancing, and from `run.step` while it is paused; never from both at once,
    /// because the lock is held across the whole operation.
    ///
    /// # Errors
    /// [`ServerError::Internal`] if the engine aborted.
    pub fn tick(&self) -> Result<bool> {
        let mut engine = self.engine.lock();
        match engine.step()? {
            Some(mut output) => {
                output.generation = self.generation();
                let finished = output.end_of_run;
                let t_ns = engine.sim_time();
                drop(engine);
                // A send error means nobody is listening, which is not a failure: the run
                // advances whether or not anyone is watching (§1.5's "a slow client does
                // not slow the engine", taken to its limit).
                let _ = self.tx.send(Arc::new(output));
                if finished {
                    self.notify_state(RunState::Finished, t_ns, "end of run");
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Tells every connection the run's state changed (§6.14 `run.state`).
    pub fn notify_state(&self, state: RunState, t_ns: u64, reason: &str) {
        let run_id = self.descriptor().run_id.clone();
        self.notify(crate::rpc::notification(
            "run.state",
            json!({"state": state.as_str(), "t_ns": t_ns, "run_id": run_id,
                   "reason": reason, "generation": self.generation()}),
        ));
    }

    /// The engine's own facts for `run.status` (see [`Engine::diagnostics`]).
    pub fn diagnostics(&self) -> Value {
        self.engine.lock().diagnostics()
    }

    /// The state at the stream position, for a connection attaching to a run that is not
    /// advancing (see [`Engine::current`]), stamped with the current generation.
    pub fn current(&self) -> Option<StepOutput> {
        let engine = self.engine.lock();
        let mut out = engine.current()?;
        out.generation = self.generation();
        Some(out)
    }

    /// Holds a scenario for the next run (`scenario.set`, `scenario.load`).
    ///
    /// # Errors
    /// Whatever the engine refuses.
    pub fn stage(&self, request: crate::engine::StageRequest) -> Result<crate::engine::Staged> {
        self.engine.lock().stage(request)
    }

    /// The scenario held for the next run, if any.
    pub fn staged(&self) -> Option<crate::engine::Staged> {
        self.engine.lock().staged()
    }

    /// The loader's verdict on a document, when the engine has a loader.
    pub fn validate_document(
        &self,
        doc: &Value,
    ) -> Option<(Vec<crate::error::ParamError>, Vec<crate::error::ParamError>)> {
        self.engine.lock().validate_document(doc)
    }

    /// The scenarios this engine can run.
    pub fn presets(&self) -> Vec<Value> {
        self.engine.lock().presets()
    }

    /// Steps `n` times, for `run.step`. Returns `(stepped, t_ns)`.
    ///
    /// # Errors
    /// As [`Run::tick`].
    pub fn advance(&self, n: u64) -> Result<(u64, u64)> {
        let mut stepped = 0u64;
        for _ in 0..n {
            if !self.tick()? {
                break;
            }
            stepped += 1;
        }
        Ok((stepped, self.sim_time()))
    }

    /// Positions the run at `t` (§7.3).
    ///
    /// # Errors
    /// [`ServerError::SeekOutOfRange`] or [`ServerError::NotSupportedHere`].
    pub fn seek(&self, t: SimTime) -> Result<Vec<StepOutput>> {
        self.engine.lock().seek(t)
    }

    /// The metric catalogue (§6.12), for the RPC layer's visibility checks.
    pub fn metric_catalogue(&self) -> Vec<crate::introspect::MetricInfo> {
        self.engine.lock().metric_catalogue()
    }

    /// The node table as of now: the engine's, for a run whose nodes appear during it,
    /// and the `Hello` table otherwise.
    pub fn nodes(&self) -> Vec<crate::engine::NodeFacts> {
        if let Some(live) = self.engine.lock().live_nodes() {
            return live;
        }
        let descriptor = self.descriptor();
        let hello = &descriptor.hello;
        hello
            .nodes
            .iter()
            .map(|row| crate::engine::NodeFacts {
                node_id: row.node_id,
                actor_id: row.actor_id,
                pos_m: row.pos_m,
                label: hello.strings.get(row.str_label).unwrap_or("").to_string(),
                profile_id: hello
                    .strings
                    .get(row.str_profile_id)
                    .unwrap_or("")
                    .to_string(),
                flags: row.flags,
                kind: row.kind,
                class_idx: row.class_idx,
            })
            .collect()
    }

    /// The node table for a run whose nodes appear during it, together with the strings
    /// the run has appended to the symbol table; `None` when the `Hello` table is already
    /// the whole set.
    pub fn live_node_table(&self) -> Option<(Vec<crate::engine::NodeFacts>, Vec<String>)> {
        let engine = self.engine.lock();
        let nodes = engine.live_nodes()?;
        Some((nodes, engine.live_strings()))
    }

    /// True if the run has this node.
    ///
    /// A live run is asked, because its node set grows: the Phase 1 Manhattan scenario has
    /// no node at all until its demand model produces a vehicle, and a `view.follow` on
    /// the node that then appears must not be refused because the `Hello` table was empty
    /// when the server bound.
    pub fn has_node(&self, node: u32) -> bool {
        if self.engine.lock().live_nodes().is_some() {
            return self.nodes().iter().any(|r| r.node_id == node);
        }
        let shape = self.shape();
        shape.node_ids.binary_search(&node).is_ok() || shape.node_ids.contains(&node)
    }

    /// The nodes within `radius_m` of `node`, in id order, `node` included.
    ///
    /// Used by `view.follow`'s `radius_m` (§6.7). A live run is asked against the poses it
    /// holds *now*; a fixture or a replay answers from the `Hello` table, which holds a
    /// mobile node's position at `t0`.
    pub fn nodes_within(&self, node: u32, radius_m: f64) -> Vec<u32> {
        let positions: BTreeMap<u32, [f32; 3]> = match self.engine.lock().live_nodes() {
            Some(live) => live.into_iter().map(|r| (r.node_id, r.pos_m)).collect(),
            None => self.shape().node_positions.clone(),
        };
        let Some(origin) = positions.get(&node).copied() else {
            return Vec::new();
        };
        let r2 = radius_m * radius_m;
        positions
            .iter()
            .filter(|(_, p)| {
                let dx = f64::from(p[0] - origin[0]);
                let dy = f64::from(p[1] - origin[1]);
                dx * dx + dy * dy <= r2
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Answers `world.generate` (§6.11) by generating the world and reporting its digest.
    ///
    /// Generation is deterministic in its parameters, so the digest is reproducible, which
    /// is what conformance W5 asks for. The generated world is *described*, not installed:
    /// swapping the world under a running stream would invalidate every client's geometry,
    /// and §6.6 makes `run.start` the method that changes a run's world.
    ///
    /// # Errors
    /// [`ServerError::Internal`] if the generator refuses the parameters.
    pub fn generate_world(
        &self,
        kind: &str,
        block_m: f64,
        lanes_per_direction: u32,
        lane_width_m: f64,
        seed: u64,
    ) -> Result<Value> {
        let cols = match kind {
            "intersection" => 2,
            "highway" | "ring" => 2,
            _ => 6,
        };
        let params = v2xw_world::procedural::GridParams {
            cols,
            rows: cols,
            block_x_m: block_m,
            block_y_m: block_m,
            lanes_per_direction,
            lane_width_m,
            signalised: true,
            crossings: true,
            block_buildings: true,
            rsu_at_junctions: true,
            ..v2xw_world::procedural::GridParams::legacy()
        };
        let world = v2xw_world::procedural::grid(&params, &v2xw_world::ImportOptions::default())?;
        let payload = v2xw_world::serde_vwp::write(&world)?;
        let bbox = world.bbox;
        Ok(json!({
            "world_hash": payload.content_hash_hex(),
            "url": payload.url_path(),
            "bbox_m": {"min_x": bbox.min.x, "min_y": bbox.min.y,
                       "max_x": bbox.max.x, "max_y": bbox.max.y},
            "origin": {"lat_deg": world.origin.lat_deg, "lon_deg": world.origin.lon_deg,
                       "alt_m": world.origin.alt_m},
            "lanes": world.roads.lanes().len(),
            "buildings": world.buildings.len(),
            "junctions": world.roads.junctions().len(),
            "signals": world.signals.len(),
            "bytes": payload.bytes.len(),
            "cached": payload.content_hash == self.world().content_hash,
            "licence": "n/a (generated)",
            "warnings": payload.precision_warnings.iter().map(|w| json!({
                "path": "/", "message": w, "severity": "warning"
            })).collect::<Vec<_>>(),
            "provenance": {"source": "procedural", "kind": kind, "seed": seed},
        }))
    }

    /// Registers an experiment definition and returns its id.
    pub fn define_experiment(&self, name: &str, cells: usize) -> String {
        let id = format!(
            "exp-{}",
            v2xw_core::hash::sha256_hex(name.as_bytes())
                .chars()
                .take(12)
                .collect::<String>()
        );
        self.experiments.lock().insert(id.clone(), cells);
        id
    }

    /// How many cells a defined experiment has, or `None` if it was never defined.
    pub fn experiment_cells(&self, id: &str) -> Option<usize> {
        self.experiments.lock().get(id).copied()
    }

    /// Whether `run_id` names this run; `latest` always does (§1.1).
    pub fn matches(&self, run_id: &str) -> bool {
        run_id == "latest" || run_id == self.descriptor().run_id
    }
}
