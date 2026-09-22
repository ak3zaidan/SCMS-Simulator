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

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::broadcast;
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

/// A live or replayed run.
#[derive(Debug)]
pub struct Run {
    engine: Mutex<Box<dyn Engine>>,
    descriptor: RunDescriptor,
    world: Arc<WorldPayload>,
    world_json: Arc<String>,
    node_ids: Vec<u32>,
    node_positions: BTreeMap<u32, [f32; 3]>,
    tx: broadcast::Sender<Arc<StepOutput>>,
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
        let node_ids: Vec<u32> = descriptor.hello.nodes.iter().map(|n| n.node_id).collect();
        let node_positions = descriptor
            .hello
            .nodes
            .iter()
            .map(|n| (n.node_id, n.pos_m))
            .collect();
        let (tx, _) = broadcast::channel(STEP_CHANNEL_CAPACITY);
        Ok(Arc::new(Run {
            engine: Mutex::new(engine),
            descriptor,
            world,
            world_json: Arc::new(world_json),
            node_ids,
            node_positions,
            tx,
            experiments: Mutex::new(BTreeMap::new()),
        }))
    }

    /// The run-scoped facts.
    pub fn descriptor(&self) -> &RunDescriptor {
        &self.descriptor
    }

    /// The `vwp-world/1` binary payload.
    pub fn world(&self) -> &Arc<WorldPayload> {
        &self.world
    }

    /// The `vwp-world/1` JSON payload (§4.6).
    pub fn world_json(&self) -> &Arc<String> {
        &self.world_json
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
        self.engine.lock().control(command)
    }

    /// Answers an introspection query.
    ///
    /// # Errors
    /// Whatever the engine refuses: `-32006`, `-32007`, `-32008` or `-32009`.
    pub fn query(&self, query: &Query) -> Result<Value> {
        self.engine.lock().query(query)
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
            Some(output) => {
                drop(engine);
                // A send error means nobody is listening, which is not a failure: the run
                // advances whether or not anyone is watching (§1.5's "a slow client does
                // not slow the engine", taken to its limit).
                let _ = self.tx.send(Arc::new(output));
                Ok(true)
            }
            None => Ok(false),
        }
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

    /// True if the run has this node.
    pub fn has_node(&self, node: u32) -> bool {
        self.node_ids.binary_search(&node).is_ok() || self.node_ids.contains(&node)
    }

    /// The nodes within `radius_m` of `node`, in id order, `node` included.
    ///
    /// Used by `view.follow`'s `radius_m` (§6.7). Positions come from the node table,
    /// which holds a mobile node's position at `t0`; a live engine answers this against
    /// the current pose instead, which is why the engine trait, not this function, is
    /// where it belongs once there is one.
    pub fn nodes_within(&self, node: u32, radius_m: f64) -> Vec<u32> {
        let Some(origin) = self.node_positions.get(&node) else {
            return Vec::new();
        };
        let r2 = radius_m * radius_m;
        self.node_positions
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
            "cached": payload.content_hash == self.world.content_hash,
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
        run_id == "latest" || run_id == self.descriptor.run_id
    }
}
