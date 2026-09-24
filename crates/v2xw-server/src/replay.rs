//! Replay: the same stream, served from an MCAP recording instead of a live engine (§7).
//!
//! The whole point of §7.2 is that the client cannot tell the difference, and the way to
//! keep that promise is to not re-encode anything. [`v2xw_record::Reader`] hands back the
//! frames the recorder stored, this module groups them by simulated time, and the session
//! forwards them to the socket with only the transport flag bits changed — which §7.2
//! item 3 explicitly allows.
//!
//! There is therefore no replay-specific decoder here, and no second copy of the §3
//! layouts. The one thing this module does decode is the recorded `Hello`, because the
//! connection-scoped fields in it (`hello_flags`, `resume_seq`, `sim_time_ns`) are
//! connection state and §7.2 says so.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};
use v2xw_core::time::SimTime;
use v2xw_record::wire::hello::HelloBody;
use v2xw_record::wire::{Frame, MsgType};
use v2xw_record::{Reader, RecordedFrame};
use v2xw_world::WorldPayload;

use crate::engine::{Control, ControlOutcome, Engine, Query, RunDescriptor, RunState, StepOutput};
use crate::error::{Result, ServerError};

/// A run served from a recording.
#[derive(Debug)]
pub struct ReplayEngine {
    descriptor: RunDescriptor,
    world: Arc<WorldPayload>,
    /// Canonical frames grouped by simulated time, in recorded order within a group.
    steps: Vec<(SimTime, Vec<Frame>)>,
    cursor: usize,
    state: RunState,
    speed: f64,
    client_sync: bool,
    nodes: usize,
    actors: usize,
}

impl ReplayEngine {
    /// Opens a recording and prepares it for streaming.
    ///
    /// The world is not in the recording's frames — `Hello.world_ref` points at an HTTP
    /// URL — so the caller supplies the payload it will serve, normally from the
    /// recording's `world.vwb` attachment (§7.1) or from the same world the run used.
    ///
    /// # Errors
    /// [`ServerError::Internal`] if the file cannot be read, or
    /// [`ServerError::WorldNotFound`] if the recording holds no `Hello`.
    pub fn open(path: impl AsRef<std::path::Path>, world: Arc<WorldPayload>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut reader = Reader::open(&path).map_err(|e| ServerError::Io {
            path: path.display().to_string(),
            errno: e.to_string(),
        })?;
        let frames = reader.replay()?;
        Self::from_frames(frames, world, &path.display().to_string())
    }

    /// Builds a replay run from frames already in memory, for tests and for the WASM
    /// reader of §7.5, which produces the same stream through an in-process channel.
    ///
    /// # Errors
    /// [`ServerError::WorldNotFound`] if there is no `Hello` in the stream.
    pub fn from_frames(
        frames: Vec<RecordedFrame>,
        world: Arc<WorldPayload>,
        label: &str,
    ) -> Result<Self> {
        let mut hello: Option<HelloBody> = None;
        let mut grouped: BTreeMap<u64, Vec<Frame>> = BTreeMap::new();
        for recorded in frames {
            let header = recorded.frame.header()?;
            match MsgType::from_id(header.msg_type) {
                Some(MsgType::Hello) => {
                    if hello.is_none() {
                        hello = Some(HelloBody::decode(recorded.frame.body())?);
                    }
                }
                // §2.1: a reader ignores a frame whose `msg_type` it does not know, and
                // forwarding an unknown canonical frame is exactly what §8.4's additive
                // rule wants — so unknown ids are kept, not dropped.
                Some(t) if t.is_canonical() => {
                    grouped
                        .entry(recorded.sim_time)
                        .or_default()
                        .push(recorded.frame);
                }
                None => {
                    grouped
                        .entry(recorded.sim_time)
                        .or_default()
                        .push(recorded.frame);
                }
                Some(_) => {}
            }
        }
        let hello = hello
            .ok_or_else(|| ServerError::WorldNotFound(format!("{label} holds no Hello frame")))?;
        let steps: Vec<(SimTime, Vec<Frame>)> = grouped.into_iter().collect();

        let cadence = v2xw_record::encoder::Cadence::new(
            v2xw_core::time::Duration::from_nanos(hello.keyframe_period_ns.max(1)),
            v2xw_core::time::Duration::from_nanos(hello.mobility_step_ns.max(1)),
        )?;
        let duration: SimTime = steps.last().map_or(hello.sim_duration_ns, |(t, _)| *t);
        let nodes = hello.nodes.len();
        let run_id = crate::stub::uuid_string(&hello.run_id);
        let scenario_hash_hex = v2xw_core::hash::hex_encode(&hello.scenario_hash);
        let origin_m = hello.keyframe_origin();
        let descriptor = RunDescriptor {
            run_id,
            run_id_bytes: hello.run_id,
            hello,
            cadence,
            origin_m,
            duration,
            live: false,
            seekable: true,
            scenario: json!({"schema": "v2xw/scenario/1",
                             "meta": {"name": "replay", "source": label},
                             "seed": 0,
                             "time": {"duration_s": duration / 1_000_000_000}}),
            scenario_hash_hex,
            recording_path: Some(label.to_string()),
            // A recording carries its own `Provenance` frames on the `vwp/provenance`
            // topic, and they are forwarded verbatim like every other canonical frame.
            provenance: None,
        };
        Ok(ReplayEngine {
            descriptor,
            world,
            steps,
            cursor: 0,
            state: RunState::Paused,
            speed: 1.0,
            client_sync: false,
            nodes,
            actors: 0,
        })
    }
}

impl Engine for ReplayEngine {
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
        self.steps
            .get(self.cursor.saturating_sub(1))
            .map_or(0, |(t, _)| *t)
    }

    fn speed(&self) -> (f64, bool) {
        (self.speed, self.client_sync)
    }

    fn counts(&self) -> (u32, u32) {
        (
            u32::try_from(self.actors).unwrap_or(u32::MAX),
            u32::try_from(self.nodes).unwrap_or(u32::MAX),
        )
    }

    fn control(&mut self, command: Control) -> Result<ControlOutcome> {
        let t_ns = self.sim_time();
        match command {
            Control::Start { paused, speed, .. } => {
                // A replay started again plays from its first step.
                self.cursor = 0;
                if let Some(speed) = speed {
                    self.speed = speed;
                }
                self.state = if paused {
                    RunState::Paused
                } else {
                    RunState::Running
                };
            }
            Control::Pause => {
                if self.state != RunState::Running {
                    return Err(ServerError::RunNotRunning(
                        "the replay is not advancing".to_string(),
                    ));
                }
                self.state = RunState::Paused;
            }
            Control::Resume => {
                if self.state != RunState::Paused {
                    return Err(ServerError::RunNotRunning(
                        "the replay is not paused".to_string(),
                    ));
                }
                self.state = RunState::Running;
            }
            Control::Speed { speed, client_sync } => {
                self.speed = speed;
                self.client_sync = client_sync;
            }
            Control::Stop { .. } => self.state = RunState::Finished,
        }
        Ok(ControlOutcome {
            state: self.state,
            t_ns,
            extra: BTreeMap::new(),
        })
    }

    fn step(&mut self) -> Result<Option<StepOutput>> {
        let Some((t, frames)) = self.steps.get(self.cursor) else {
            self.state = RunState::Finished;
            return Ok(None);
        };
        self.cursor += 1;
        Ok(Some(StepOutput {
            sim_time: *t,
            end_of_run: self.cursor >= self.steps.len(),
            recorded: frames.clone(),
            ..Default::default()
        }))
    }

    fn seek(&mut self, t: SimTime) -> Result<Vec<StepOutput>> {
        let (min_ns, max_ns) = self.seek_range();
        if t < min_ns || t > max_ns {
            return Err(ServerError::SeekOutOfRange { min_ns, max_ns });
        }
        // §7.3 steps 6-7: the last keyframe at or before `t`, then every frame after it up
        // to and including `t`.
        let end = self
            .steps
            .iter()
            .rposition(|(step_t, _)| *step_t <= t)
            .unwrap_or(0);
        let start = self.steps[..=end]
            .iter()
            .rposition(|(_, frames)| {
                frames.iter().any(|f| {
                    f.header()
                        .is_ok_and(|h| h.msg_type == MsgType::Keyframe.id())
                })
            })
            .unwrap_or(0);
        self.cursor = end + 1;
        self.state = RunState::Paused;
        Ok(self.steps[start..=end]
            .iter()
            .map(|(step_t, frames)| StepOutput {
                sim_time: *step_t,
                recorded: frames.clone(),
                ..Default::default()
            })
            .collect())
    }

    fn seek_range(&self) -> (u64, u64) {
        (
            self.steps.first().map_or(0, |(t, _)| *t),
            self.steps.last().map_or(0, |(t, _)| *t),
        )
    }

    fn query(&mut self, query: &Query) -> Result<Value> {
        match query {
            Query::Node { node, .. } => {
                let row = self
                    .descriptor
                    .hello
                    .nodes
                    .iter()
                    .find(|r| r.node_id == node.index())
                    .ok_or_else(|| ServerError::UnknownId {
                        kind: "node",
                        id: node.index().to_string(),
                    })?;
                Ok(json!({
                    "node": row.node_id,
                    "t_ns": self.sim_time(),
                    "kind": "obu",
                    "label": self.descriptor.hello.strings.get(row.str_label).unwrap_or(""),
                    "profile_id": self.descriptor.hello.strings
                        .get(row.str_profile_id).unwrap_or(""),
                }))
            }
            // §6.4 `-32009` is the code for "replay-only or live-only restriction", and
            // the inverse holds here: a recording carries frames, not the engine state
            // these queries read.
            _ => Err(ServerError::NotSupportedHere(
                "this run is a replay; inspection beyond the node table needs a live engine"
                    .to_string(),
            )),
        }
    }
}
