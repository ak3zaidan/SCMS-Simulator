//! The main loop and the phase-parallel structure of ADR 0004.
//!
//! # Shape of a run
//!
//! One single-threaded event loop over one heap, and a small number of *phases* that are
//! pure maps merged in id order. The loop never runs two events at once; the phases never
//! touch the heap while they are mapping. That split is ADR 0004 decision 5, and it is
//! what makes thread count change wall time and nothing else.
//!
//! ```text
//! pop (time, priority, seq)
//!   ├─ Control      p0  scenario timeline: weather, outage, demand, parameter change
//!   ├─ MobilityStep p1  ── phase ──► map over actors (mobility provider), merge by ActorId
//!   │                    publish Kinematics, rebuild the snapshot, spawn/retire nodes
//!   ├─ PhyEnd       p3  ── phase ──► map over that frame's receivers, merge by NodeId
//!   ├─ PhyStart     p5  one frame goes on the air; its receiver set is resolved at PhyEnd
//!   ├─ NodePhase    p6  ── phase ──► map over nodes, merge by NodeId; node-local queues
//!   │                    are drained *inside* the map and never reach the heap
//!   └─ Observe      p9  metric flush, and the end-of-run sentinel
//! ```
//!
//! # Between mobility steps
//!
//! Mobility is periodic (ADR 0004 decision 2). A radio event at `t′` strictly inside a
//! step reads a position from the **published extrapolation rule**,
//! `pos(t′) = pos(t) + vel(t)·(t′ − t)` — [`v2xw_core::kinematics::Kinematics::extrapolate`],
//! which is part of the interface contract (03-interfaces.md §3, invariant I-M4) and not
//! an engine convenience. [`Engine::position_at`] is the one place that rule is applied, so
//! a second, subtly different extrapolation cannot appear in a second phase.
//!
//! # Time dilation
//!
//! 02-architecture.md §5.4: a scenario may declare windows in which only the backend and
//! the abstract mobility tiers run, so a 24-hour credential experiment finishes in
//! minutes. The engine implements that by **not generating radio events** inside a window:
//! `PhyStart` is not scheduled, so no frame, no reception and no verification cost exist
//! for that period. The windows are in the manifest, and [`RunReport::suppressed_frames`]
//! counts what was skipped, because a metric that silently reads zero inside a window is
//! indistinguishable from a channel that was quiet.
//!
//! # What this loop does not do yet
//!
//! Stated rather than stubbed:
//!
//! * **Interference and capture.** A reception outcome here is a link-budget decision
//!   against the noise floor, so SINR is SNR: concurrent frames do not raise each other's
//!   denominator. That is `v2xw-radio`'s `OfdmPhy` high tier and a `MacTimer`-driven MAC,
//!   and both are scheduled through the event classes this enum already carries.
//! * **The MAC.** A frame reaches the air after the signing latency plus one AIFS; there
//!   is no backoff, no CBR measurement and no DCC gate. `Event::MacTimer` is the class
//!   those live at.
//! * **Backend, credential protocol and detection.** `Event::NetDeliver` and
//!   `Event::FlowTimer` are their classes; nothing schedules them here.
//!
//! Each gap is a missing *model*, not a missing seam: the event class, the phase and the
//! record channel for each already exist.

use std::collections::BTreeMap;

use rayon::prelude::*;
use v2xw_core::card::Tier;
use v2xw_core::event::{EventClass, Scheduler};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{ActorId, FrameSeq, LinkKey, NodeId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::manifest::Manifest;
use v2xw_core::provenance::ProvenanceLog;
use v2xw_core::registry::{ParamSet, Registry};
use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};
use v2xw_core::time::{Duration, SimTime, WallClock};
use v2xw_core::weather::WeatherState;
use v2xw_metrics::channels::{RxOutcome, SignerId};
use v2xw_mobility::{
    ActorSnapshot, DriverProfile, GnssEnv, GnssModel, Mobility, MobilityUpdate, VehicleClass,
    VehicleView,
};
use v2xw_msg::generator::DccState;
use v2xw_node::{NodeConfig, ObuRuntime, RxFrame, StepOutcome, Transmission};
use v2xw_radio::{LosResult, PerModel, RadioEndpoint};
use v2xw_world::World;

use crate::adapters::{BoxedFading, BoxedPropagation};
use crate::ctx::{EngineCtx, RunRecorder};
use crate::error::{EngineError, Result};
use crate::event::{Event, Observe};
use crate::records::{GtKinematics, NodeTx, PhyRx};
use crate::scenario::Scenario;

/// The 5.9 GHz safety channel, and the frequency the link budget is evaluated at.
///
/// Channel 172 is the SAE J2945/1 safety channel in the US band plan; 5.86–5.93 GHz maps
/// it to 5.860 GHz + 5 MHz × (n − 172) with 10 MHz channels, which puts 172 at 5.860 GHz.
const SAFETY_CHANNEL: u16 = 172;
/// The centre frequency of [`SAFETY_CHANNEL`], hertz.
const SAFETY_FREQ_HZ: f64 = 5.860e9;
/// The AIFS a safety frame waits before the PHY may start it.
///
/// EDCA AC_VI on a 10 MHz OCB channel: `AIFS = SIFS + 2·slot = 32 µs + 2·13 µs = 58 µs`
/// [IEEE 802.11-2020 Table 9-155, 10 MHz timing]. It is the *floor* on access delay, and
/// with no contention window modelled it is the whole of it — which is why the run report
/// counts frames rather than claiming a channel-access distribution.
const AIFS: Duration = Duration::from_micros(58);
/// The receiver's thermal noise floor on a 10 MHz channel, dBm.
///
/// `−174 dBm/Hz + 10·log10(10 MHz) + NF`, with a 9 dB noise figure — the value
/// `v2xw-radio`'s own sensitivity presets are built on.
const NOISE_FLOOR_DBM: f64 = -174.0 + 70.0 + 9.0;
/// How far a candidate receiver may be. The grid cell size equals this (ADR 0004
/// decision 6), so a neighbour query touches at most nine cells.
const MAX_RANGE_M: f64 = 1000.0;

/// What one run produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct RunReport {
    /// How many events of each class were dispatched, in class order.
    pub events_by_class: BTreeMap<String, u64>,
    /// How many mobility steps ran.
    pub mobility_steps: u64,
    /// How many actors were ever spawned.
    pub actors_spawned: u64,
    /// How many nodes were ever created.
    pub nodes_created: u64,
    /// How many frames went on the air.
    pub frames_transmitted: u64,
    /// How many reception attempts were evaluated.
    pub reception_attempts: u64,
    /// How many frames were successfully received by at least one node.
    pub frames_received: u64,
    /// How many frames were *not* generated because the instant fell in a time-dilation
    /// window (02-architecture.md §5.4).
    pub suppressed_frames: u64,
    /// How many records were emitted.
    pub records: u64,
    /// How many records the context refused, by the visibility rule.
    pub records_refused: u64,
    /// The instant the loop stopped at.
    pub end_ns: SimTime,
}

impl RunReport {
    fn count(&mut self, class: EventClass) {
        *self.events_by_class.entry(class.to_string()).or_insert(0) += 1;
    }
}

/// Everything the engine knows about one actor that the mobility update does not repeat.
#[derive(Debug, Clone)]
struct ActorRecord {
    class: VehicleClass,
    driver: DriverProfile,
    node: Option<NodeId>,
    last: Kinematics,
}

/// A frame on the air.
#[derive(Debug, Clone)]
struct FrameState {
    tx: NodeId,
    tx_pos: Vec3,
    bytes: u32,
    msg_type: v2xw_msg::MsgType,
    signer: v2xw_msg::sec_types::HashedId8,
    full_certificate: bool,
    generation_time: SimTime,
    claimed_pos: Vec3,
    claimed_speed_mps: f64,
    claimed_heading_rad: f64,
    start: SimTime,
    air: Duration,
}

/// A configured run, ready to be driven.
pub struct Engine {
    scenario: Scenario,
    world: World,
    registry: Registry,
    manifest: Manifest,
    scheduler: Scheduler<Event>,
    rng: RngRegistry,
    provenance: ProvenanceLog,
    params: ParamSet,
    wall: WallClock,
    snapshot: ActorSnapshot,
    mobility: Box<dyn Mobility>,
    gnss: Box<dyn GnssModel>,
    propagation: Box<dyn BoxedPropagation>,
    fading: Box<dyn BoxedFading>,
    per: PerModel,
    weather: WeatherState,
    actors: BTreeMap<ActorId, ActorRecord>,
    nodes: BTreeMap<NodeId, ObuRuntime>,
    inboxes: BTreeMap<NodeId, Vec<RxFrame>>,
    frames: BTreeMap<FrameSeq, FrameState>,
    next_node: u32,
    next_frame: u32,
    providers: v2xw_metrics::ProviderSet,
    metric_period: Duration,
    reverse_node_walk: bool,
    report: RunReport,
}

impl core::fmt::Debug for Engine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Engine")
            .field("scenario", &self.scenario.meta.name)
            .field("actors", &self.actors.len())
            .field("nodes", &self.nodes.len())
            .field("pending", &self.scheduler.len())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Builds a run from a validated scenario.
    ///
    /// `build_utc` is the caller's timestamp for the manifest; the engine may not read a
    /// clock for itself (02-architecture.md §6.1). Pass an empty string for a
    /// reproducibility comparison — the field is excluded from every digest either way.
    ///
    /// # Errors
    /// [`EngineError::World`] if the world cannot be built, [`EngineError::Mobility`] if
    /// the mobility provider refuses the world, [`EngineError::Registry`] if a model's
    /// card does not validate, and [`EngineError::Scenario`] if the scenario is invalid.
    pub fn build(scenario: Scenario, build_utc: &str) -> Result<Engine> {
        scenario.validate()?;

        let world = crate::wiring::build_world(&scenario)?;
        let mut registry = Registry::new();
        crate::wiring::register_all(&mut registry)?;

        let wall = WallClock::parse_rfc3339(&scenario.time.t0)
            .map_err(|e| EngineError::Core(v2xw_core::error::CoreError::Time(e)))?;
        let rng = RngRegistry::new(scenario.seed);
        let mut mobility = crate::wiring::build_mobility(&scenario);
        let gnss = crate::wiring::build_gnss(&scenario);
        let (propagation, fading) = crate::wiring::build_radio(&scenario, &world);

        // The mobility provider reads the world through a context, so it needs one before
        // the engine exists. Everything it can reach at this point is immutable state the
        // engine is about to own.
        {
            let mut scheduler: Scheduler<Event> = Scheduler::new();
            let mut provenance = ProvenanceLog::new();
            let params = ParamSet::new();
            let empty = ActorSnapshot::new(0, MAX_RANGE_M);
            let mut recorder = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                &mut scheduler,
                &rng,
                &world,
                &empty,
                &mut provenance,
                &params,
                &mut recorder,
            );
            let demand = crate::wiring::build_demand(&scenario, &world)?;
            mobility.init(&mut crate::adapters::mobility(&mut ctx), demand)?;
        }

        let providers = crate::wiring::build_metrics(&scenario, &mut registry)?;
        let manifest = crate::manifest::assemble(&scenario, &world, &registry, build_utc)?;
        let mut engine = Engine {
            snapshot: ActorSnapshot::new(0, MAX_RANGE_M),
            weather: crate::wiring::initial_weather(&scenario),
            scenario,
            world,
            registry,
            manifest,
            scheduler: Scheduler::new(),
            rng,
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            wall,
            mobility,
            gnss,
            propagation,
            fading,
            per: PerModel::new(v2xw_radio::PerPreset::Ideal),
            actors: BTreeMap::new(),
            nodes: BTreeMap::new(),
            inboxes: BTreeMap::new(),
            frames: BTreeMap::new(),
            next_node: 0,
            next_frame: 0,
            providers,
            metric_period: Duration::from_secs(1),
            reverse_node_walk: false,
            report: RunReport::default(),
        };
        engine.seed_timeline();
        Ok(engine)
    }

    /// The manifest this run will be recorded under.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The model registry, for a caller assembling a report.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The world.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The scenario being run.
    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    /// The wall clock `SimTime` zero maps to — scenario data, not a clock read.
    ///
    /// It is what a 1609.2 `generationTime` is stamped from
    /// ([`v2xw_sec::envelope::Envelope`] takes one), and what a UI labels its axis with.
    pub fn wall_clock(&self) -> WallClock {
        self.wall
    }

    /// The run report as it stands, for a caller inspecting a partial run.
    pub fn report(&self) -> &RunReport {
        &self.report
    }

    /// **A test hook, not a model parameter.** Walks the node phase in reverse id order.
    ///
    /// The phase reads each node's own inbox and writes only its own state, so the
    /// published result must be identical either way; this is the ADR 0004 purity property
    /// and the only way to check it without a second thread, which
    /// [`v2xw_node::ObuRuntime`] not being `Send` denies us. `v2xw-mobility` carries the
    /// same hook (`EngineParams::reverse_order`) for the same reason.
    pub fn set_reverse_node_walk(&mut self, reverse: bool) {
        self.reverse_node_walk = reverse;
    }

    /// Runs `f` with an engine context over this engine's state.
    ///
    /// The seam a caller outside the loop reaches the kernel through: a tool that wants to
    /// emit a record, schedule an event or draw from a stream in the engine's own frame
    /// uses this rather than rebuilding a context and getting the borrows wrong. It is
    /// also how this crate's tests exercise [`EngineCtx`] against a real engine instead of
    /// a fixture, which is the difference between testing the context and testing a copy
    /// of it.
    pub fn with_ctx<R>(
        &mut self,
        recorder: &mut dyn RunRecorder,
        f: impl FnOnce(&mut EngineCtx<'_>) -> R,
    ) -> R {
        let Engine {
            scheduler,
            rng,
            world,
            snapshot,
            provenance,
            params,
            ..
        } = self;
        let mut ctx = EngineCtx::new(
            scheduler, rng, world, snapshot, provenance, params, recorder,
        );
        f(&mut ctx)
    }

    /// The `why` service's accumulated entries.
    pub fn provenance(&self) -> &ProvenanceLog {
        &self.provenance
    }

    /// The position of an actor at an instant inside the current mobility step.
    ///
    /// The published extrapolation rule (02-architecture.md §5.2, invariant I-M4) and the
    /// only place it is applied.
    pub fn position_at(&self, actor: ActorId, t: SimTime) -> Option<Vec3> {
        self.actors.get(&actor).map(|a| a.last.extrapolate(t).pos)
    }

    /// True if `t` falls inside a declared time-dilation window.
    pub fn is_dilated(&self, t: SimTime) -> bool {
        self.manifest.is_dilated(t)
    }

    /// Puts the scheduled events that exist before the first dispatch on the heap.
    fn seed_timeline(&mut self) {
        let horizon = self.scenario.time.horizon_ns();
        self.scheduler
            .schedule(0, EventClass::MobilityStep, Event::MobilityStep);
        for (i, item) in self.scenario.events.iter().enumerate() {
            let at = (item.t * 1e9).round().max(0.0) as u64;
            if at <= horizon {
                self.scheduler.schedule(
                    at,
                    EventClass::Control,
                    Event::Control {
                        item: i as u32,
                        end: false,
                    },
                );
            }
            if let Some(until) = item.until {
                let end = (until * 1e9).round().max(0.0) as u64;
                if end <= horizon {
                    self.scheduler.schedule(
                        end,
                        EventClass::Control,
                        Event::Control {
                            item: i as u32,
                            end: true,
                        },
                    );
                }
            }
        }
        if !self.providers.is_empty() {
            let first = self.metric_period.after(0);
            if first <= horizon {
                self.scheduler.schedule(
                    first,
                    EventClass::Observe,
                    Event::Observe {
                        what: Observe::MetricFlush,
                    },
                );
            }
        }
        // The end-of-run sentinel is an `Observe`, so it is the last thing that happens at
        // the horizon: every metric flush and every keyframe at that instant runs first.
        self.scheduler.schedule(
            horizon,
            EventClass::Observe,
            Event::Observe {
                what: Observe::EndOfRun,
            },
        );
    }

    /// Runs to the horizon, writing into `recorder`.
    ///
    /// # Errors
    /// Whatever a phase returns: a mobility failure, a recorder failure.
    pub fn run(&mut self, recorder: &mut dyn RunRecorder) -> Result<RunReport> {
        let horizon = self.scenario.time.horizon_ns();
        let step = self.scenario.time.mobility_step();

        while let Some((key, event)) = self.scheduler.pop() {
            if key.time > horizon {
                break;
            }
            self.report.count(event.class());
            match event {
                Event::Control { item, end } => self.on_control(item as usize, end),
                Event::MobilityStep => {
                    self.on_mobility_step(recorder, step, horizon)?;
                }
                Event::NodePhase => self.on_node_phase(recorder, horizon),
                Event::PhyStart { frame, .. } => self.on_phy_start(frame),
                Event::PhyEnd { frame } => self.on_phy_end(recorder, frame),
                Event::Observe {
                    what: Observe::MetricFlush,
                } => self.on_metric_flush(recorder, horizon),
                Event::Observe {
                    what: Observe::EndOfRun,
                } => {
                    self.report.end_ns = key.time;
                    break;
                }
                // The remaining classes have no model scheduling them in this build; see
                // the module documentation's list of what is a missing model rather than a
                // missing seam. Counting them is what makes their absence visible in the
                // run report instead of silent.
                Event::SignalPhase { .. }
                | Event::MacTimer { .. }
                | Event::NodeTask { .. }
                | Event::NetDeliver { .. }
                | Event::FlowTimer { .. }
                | Event::Observe { .. } => {}
            }
            self.report.end_ns = key.time;
        }
        Ok(self.report.clone())
    }

    /// A scenario timeline item takes effect, or stops taking effect.
    fn on_control(&mut self, index: usize, end: bool) {
        let Some(item) = self.scenario.events.get(index).cloned() else {
            return;
        };
        use crate::scenario::TimelineKind;
        match item.kind {
            TimelineKind::WeatherFront => {
                if let Some(v) = item.params.get("value")
                    && let Ok(kind) = serde_json::from_value(v.clone())
                {
                    self.weather = crate::wiring::weather_of(kind, 1.0, None, None);
                }
            }
            TimelineKind::Outage => {
                // `target` names a node by index. An outage turns the node off, which is
                // the state `ObuRuntime::step` returns from immediately, so it stops both
                // transmitting and receiving without being removed.
                if let Some(node) = item
                    .params
                    .get("target")
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| NodeId::new(n as u32))
                    && let Some(runtime) = self.nodes.get_mut(&node)
                {
                    runtime.set_state(if end {
                        v2xw_node::NodeState::Active
                    } else {
                        v2xw_node::NodeState::Off
                    });
                }
            }
            // `demand.multiplier`, `attack.wave`, `param.change` and `closure` need a
            // demand model that takes a multiplier, an attacker set, a live parameter
            // store and a mobility command respectively. Each is a model this build does
            // not have; the events fire, are counted, and change nothing, which is what
            // the run report shows.
            TimelineKind::DemandMultiplier
            | TimelineKind::AttackWave
            | TimelineKind::ParamChange
            | TimelineKind::Closure => {}
        }
    }

    /// The mobility phase: map over actors, merge by [`ActorId`], publish, reindex.
    fn on_mobility_step(
        &mut self,
        recorder: &mut dyn RunRecorder,
        step: Duration,
        horizon: SimTime,
    ) -> Result<()> {
        let now = self.scheduler.now();

        let update = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                mobility,
                ..
            } = self;
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, recorder,
            );
            let mut mob = crate::adapters::mobility(&mut ctx);
            mobility.step(&mut mob, step).quantized()
        };

        self.absorb(&update, now);
        self.rebuild_snapshot(&update);
        self.publish(recorder, &update);
        self.update_beliefs(recorder, now);

        self.report.mobility_steps += 1;

        // The node phase at the same instant, at priority 6 — after any PhyEnd at this
        // instant (priority 3), which is the order 02-architecture.md §5.1 fixes.
        self.scheduler
            .schedule(now, EventClass::NodeTask, Event::NodePhase);

        let next = step.after(now);
        if next <= horizon {
            self.scheduler
                .schedule(next, EventClass::MobilityStep, Event::MobilityStep);
        }
        Ok(())
    }

    /// Takes the spawns and despawns out of an update, creating and retiring nodes.
    fn absorb(&mut self, update: &MobilityUpdate, now: SimTime) {
        for spawn in &update.spawned {
            self.report.actors_spawned += 1;
            // The equipped draw is keyed by the actor, so whether a vehicle carries an OBU
            // does not depend on how many vehicles spawned before it (ADR 0004 §3).
            let equipped = self
                .rng
                .checkout(RngDomain::Spawn, EntityRef::Actor(spawn.actor))
                .bool(self.scenario.actors.vehicles.equipped_fraction);
            let node = if equipped {
                let id = NodeId::new(self.next_node);
                self.next_node += 1;
                let runtime = crate::wiring::build_node(&self.scenario, id, now);
                self.nodes.insert(id, runtime);
                self.inboxes.insert(id, Vec::new());
                self.report.nodes_created += 1;
                Some(id)
            } else {
                None
            };
            self.actors.insert(
                spawn.actor,
                ActorRecord {
                    class: spawn.class,
                    driver: spawn.driver,
                    node,
                    last: spawn.kinematics,
                },
            );
        }
        for (actor, _) in &update.despawned {
            if let Some(rec) = self.actors.remove(actor)
                && let Some(node) = rec.node
            {
                self.nodes.remove(&node);
                self.inboxes.remove(&node);
            }
        }
        for (actor, k) in &update.states {
            if let Some(rec) = self.actors.get_mut(actor) {
                rec.last = *k;
            }
        }
    }

    /// Rebuilds the spatial index from the published states (ADR 0004 decision 6).
    fn rebuild_snapshot(&mut self, update: &MobilityUpdate) {
        let mut entries: Vec<(VehicleView, Kinematics)> = Vec::with_capacity(update.states.len());
        for (actor, k) in &update.states {
            let Some(rec) = self.actors.get(actor) else {
                continue;
            };
            let lane = k.lane.unwrap_or(v2xw_core::geom::LanePos::new(
                v2xw_core::ids::LaneId::new(0),
                0.0,
                0.0,
            ));
            entries.push((
                VehicleView {
                    actor: *actor,
                    class: rec.class,
                    lane: lane.lane,
                    lane_index: self
                        .world
                        .roads
                        .lanes()
                        .get(lane.lane.0 as usize)
                        .map_or(0, |l| l.index),
                    // The view's `s_m` is the front bumper; `Kinematics` references the
                    // rear axle (03-interfaces.md §1), and the conversion happens once.
                    s_m: lane.s_m + k.dims.length_m,
                    lateral_m: lane.d_m,
                    speed_mps: k.ground_speed_mps(),
                    accel_mps2: k.acc.x,
                    heading_rad: k.heading_rad,
                    dims: k.dims,
                    driver: rec.driver,
                },
                *k,
            ));
        }
        self.snapshot = ActorSnapshot::build(update.t, MAX_RANGE_M, entries);
    }

    /// Emits `gt.kinematics` for every published state, in actor order.
    fn publish(&mut self, recorder: &mut dyn RunRecorder, update: &MobilityUpdate) {
        for (actor, k) in &update.states {
            let class = self
                .actors
                .get(actor)
                .map_or("unknown", |a| a.class.as_str());
            let rec = GtKinematics::new(*actor, k, class);
            self.emit(recorder, &rec);
        }
    }

    /// Advances every node's belief from its ground truth through the GNSS model.
    ///
    /// Sequential and in node order, because the GNSS model is stateful per node and the
    /// state is advanced here; the draws are keyed by node, so the *values* would be the
    /// same in any order, and the ordering is about the model's `&mut self` rather than
    /// about determinism.
    fn update_beliefs(&mut self, recorder: &mut dyn RunRecorder, now: SimTime) {
        let pairs: Vec<(NodeId, Kinematics)> = self
            .actors
            .values()
            .filter_map(|a| a.node.map(|n| (n, a.last)))
            .collect();
        let env = GnssEnv {
            weather: self.weather,
            ..GnssEnv::OPEN_SKY
        };
        for (node, truth) in pairs {
            let belief = {
                let Engine {
                    scheduler,
                    rng,
                    world,
                    snapshot,
                    provenance,
                    params,
                    gnss,
                    ..
                } = self;
                let mut ctx = EngineCtx::new(
                    scheduler, rng, world, snapshot, provenance, params, recorder,
                );
                let mut mob = crate::adapters::mobility(&mut ctx);
                gnss.estimate(&mut mob, node, &truth, &env)
            };
            // `pos_error_m` is one of the two ground-truth values vwp-v1 §3.5.2 marks GT
            // and that a node cannot compute for itself. It enters through the one door
            // the firewall leaves open, from the engine, which knows both numbers.
            let error =
                v2xw_core::math::hypot(belief.pos.x - truth.pos.x, belief.pos.y - truth.pos.y);
            if let Some(runtime) = self.nodes.get_mut(&node) {
                runtime.set_belief(belief.quantized());
                runtime.observe_truth(error as f32);
                runtime.set_dcc(DccState::UNRESTRICTED, 0);
                let _ = now;
            }
        }
    }

    /// The node phase: a map over nodes, merged by [`NodeId`].
    ///
    /// Each node is handed its own inbox and its own context, steps, and returns what it
    /// produced. Nothing in the map touches the heap, the recorder or another node, so it
    /// is a **pure map in the ADR 0004 sense** and its result does not depend on the order
    /// the nodes are walked in — which `the_node_phase_result_does_not_depend_on_walk_order`
    /// checks by walking them backwards.
    ///
    /// It is nevertheless **executed sequentially**, and the reason is a type and not a
    /// choice: [`v2xw_node::ObuRuntime`] is not `Send`, because it holds a
    /// `Box<dyn VerificationPolicy>` and [`v2xw_core::model::Model`] — the supertrait every
    /// family extends — has no `Send + Sync` bound. `rayon` therefore cannot take `&mut`
    /// to two runtimes at once, whatever the map's purity. Adding `Send + Sync` to `Model`
    /// (or to the policy box) makes this one call `par_iter_mut`, and nothing else in this
    /// function changes; that is reported rather than worked around, because working
    /// around it would mean `unsafe`, which this crate forbids. The reception phase in
    /// [`Engine::on_phy_end`] *is* executed in parallel, so the structure is exercised by
    /// the run rather than only described by it.
    fn on_node_phase(&mut self, recorder: &mut dyn RunRecorder, horizon: SimTime) {
        let now = self.scheduler.now();
        let step_s = self.scenario.time.mobility_step().as_secs_f64();
        let mut inboxes = core::mem::take(&mut self.inboxes);
        let rng = &self.rng;

        let reverse = self.reverse_node_walk;
        let walk: Box<dyn Iterator<Item = (&NodeId, &mut ObuRuntime)>> = if reverse {
            Box::new(self.nodes.iter_mut().rev())
        } else {
            Box::new(self.nodes.iter_mut())
        };
        let mut results: Vec<(NodeId, StepOutcome, Vec<v2xw_core::ctx::OwnedRecord>, f64)> = walk
            .map(|(id, runtime)| {
                let inbox = inboxes.get(id).cloned().unwrap_or_default();
                let mut local = v2xw_node::NodeRuntimeCtx::new(now, rng);
                // Distance travelled since the last step drives distance-based pseudonym
                // rotation. It is the node's own odometry, not a ground-truth read: a
                // fielded receiver integrates its own speed the same way.
                let travelled = runtime
                    .state()
                    .transmits()
                    .then(|| v2xw_core::NodeView::position(runtime).ground_speed_mps() * step_s);
                let outcome = runtime.step(&mut local, inbox, travelled.unwrap_or(0.0));
                (*id, outcome, local.take_emitted(), travelled.unwrap_or(0.0))
            })
            .collect();

        // The merge (02-architecture.md §6.4). `par_iter_mut` over a `BTreeMap` yields in
        // key order but `collect` into a `Vec` does not promise to preserve it for an
        // unindexed parallel iterator, so the order is *re-established* here rather than
        // assumed. That is the difference between a run that is deterministic and one that
        // happens to be.
        results.sort_by_key(|(id, ..)| *id);

        for (_, _, records, _) in &results {
            for rec in records {
                self.providers.on_event(rec);
                recorder.write(now, rec);
                self.report.records += 1;
            }
        }

        for (id, outcome, _, _) in &results {
            for tx in &outcome.transmissions {
                self.launch(*id, tx, now, horizon);
            }
        }

        for inbox in inboxes.values_mut() {
            inbox.clear();
        }
        self.inboxes = inboxes;
    }

    /// Turns one transmission into a frame on the air.
    fn launch(&mut self, node: NodeId, tx: &Transmission, now: SimTime, horizon: SimTime) {
        // The signing latency is a *duration* on the node's own clock, so it is
        // independent of the node's clock offset: `ready_at` and the believed instant are
        // both on that clock and the difference between them is a real interval.
        let believed = self
            .nodes
            .get(&node)
            .map_or(now, |n| n.clock().believed_time(now));
        let signing = Duration::between(believed, tx.ready_at);
        let at = Duration::from_nanos(signing.as_nanos() + AIFS.as_nanos()).after(now);
        if at > horizon {
            return;
        }
        if self.is_dilated(at) {
            self.report.suppressed_frames += 1;
            return;
        }
        let Some(pos) = self
            .actors
            .values()
            .find(|a| a.node == Some(node))
            .map(|a| a.last.extrapolate(at).pos)
        else {
            return;
        };
        let belief = self.nodes.get(&node).map(v2xw_core::NodeView::position);
        let frame = FrameSeq::new(self.next_frame);
        self.next_frame += 1;
        self.frames.insert(
            frame,
            FrameState {
                tx: node,
                tx_pos: pos,
                bytes: tx.bytes,
                msg_type: tx.msg_type,
                signer: tx.signer.clone(),
                full_certificate: tx.full_certificate,
                generation_time: tx.generation_time,
                claimed_pos: belief.map_or(pos, |b| b.pos),
                claimed_speed_mps: belief
                    .map_or(0.0, v2xw_core::PositionEstimate::ground_speed_mps),
                claimed_heading_rad: belief.map_or(0.0, |b| b.heading_rad),
                start: at,
                air: v2xw_radio::air_time(tx.bytes, v2xw_radio::Mcs::R6Qpsk12),
            },
        );
        self.scheduler.schedule(
            at,
            EventClass::PhyStart,
            Event::PhyStart { frame, tx: node },
        );
    }

    /// A frame begins. The only thing that happens at the start of a frame in this build
    /// is that its end is scheduled; the receiver set is resolved there (invariant I-R2).
    fn on_phy_start(&mut self, frame: FrameSeq) {
        let Some(state) = self.frames.get(&frame) else {
            return;
        };
        let end = state.air.after(state.start);
        self.report.frames_transmitted += 1;
        self.scheduler
            .schedule(end, EventClass::PhyEnd, Event::PhyEnd { frame });
    }

    /// The reception phase (ADR 0004 decision 5, invariant I-R2).
    ///
    /// Three stages, and the split between them is the point:
    ///
    /// 1. **Candidates**, from the grid query the snapshot is sized for.
    /// 2. **Link budgets**, sequentially. The propagation and fading models are stateful
    ///    per link — a shadowing process is correlated along a trajectory, which is why
    ///    it has state at all — so this stage cannot be a pure map and is not pretended
    ///    to be one.
    /// 3. **Outcomes**, in parallel. Given the received power, each receiver's decision is
    ///    independent of every other receiver's, which is exactly what I-R2 states. The
    ///    map reads `&PerModel` and `&RngRegistry` and writes nothing shared, and the
    ///    results are **re-sorted** by [`NodeId`] afterwards rather than trusted to arrive
    ///    in order.
    ///
    /// The draw is keyed by `(link, frame)`, so a receiver's outcome depends on neither
    /// the thread that computed it nor the number of frames the link has already carried.
    /// `the_reception_phase_is_identical_on_one_and_eight_threads` is the check.
    fn on_phy_end(&mut self, recorder: &mut dyn RunRecorder, frame: FrameSeq) {
        let Some(state) = self.frames.remove(&frame) else {
            return;
        };
        let now = self.scheduler.now();

        // Stage 1: the candidate set — every equipped actor within the modelled range, by
        // the grid query ADR 0004 decision 6 sizes for exactly this.
        let mut candidates: Vec<(NodeId, Vec3)> = Vec::new();
        for actor in self.snapshot.actors_within(state.tx_pos, MAX_RANGE_M) {
            let Some(rec) = self.actors.get(&actor) else {
                continue;
            };
            let Some(node) = rec.node else { continue };
            if node == state.tx {
                continue;
            }
            candidates.push((node, rec.last.extrapolate(now).pos));
        }
        candidates.sort_by_key(|(n, _)| *n);

        // Stage 2: the link budgets, sequentially, because the models carry state.
        let budgets: Vec<(NodeId, f64, f64)> = candidates
            .iter()
            .map(|(rx, rx_pos)| {
                let (rssi, dist) = self.link_budget(&state, *rx, *rx_pos);
                (*rx, rssi, dist)
            })
            .collect();

        // Stage 3: the outcomes, as a pure map (ADR 0004 decision 5).
        let per_model = &self.per;
        let rng = &self.rng;
        let bytes = state.bytes;
        let tx = state.tx;
        let frame_index = u64::from(frame.index());
        let mut outcomes: Vec<LinkOutcome> = budgets
            .par_iter()
            .map(|(rx, rssi_dbm, distance_m)| {
                let sinr_db = rssi_dbm - NOISE_FLOOR_DBM;
                let per = per_model.per(bytes, v2xw_radio::Mcs::R6Qpsk12, sinr_db);
                // D10: the threshold comparison is made against a quantised value, so a
                // cross-engine comparison cannot flip on a last-bit difference in a
                // transcendental.
                let per_q = v2xw_core::math::quantize_to(per, 1e-6);
                let draw = rng
                    .checkout(
                        RngDomain::AbstractRx,
                        EntityRef::LinkFrame {
                            link: LinkKey::new(tx, *rx),
                            frame: frame_index,
                        },
                    )
                    .f64();
                LinkOutcome {
                    rx: *rx,
                    rssi_dbm: *rssi_dbm,
                    sinr_db,
                    distance_m: *distance_m,
                    received: draw >= per_q,
                }
            })
            .collect();

        // The merge. `par_iter` over a slice is indexed and `collect` does preserve order,
        // but the sort is here anyway: the guarantee the run depends on should be stated
        // by this code, not inherited from a library's iterator kind, which a later
        // refactor to an unindexed source would silently withdraw.
        outcomes.sort_by_key(|o| o.rx);

        let mut received_any = false;
        for outcome in outcomes {
            self.report.reception_attempts += 1;
            let record = PhyRx::new(
                state.start,
                now,
                state.tx,
                outcome.rx,
                frame_index,
                outcome.rssi_dbm,
                outcome.sinr_db,
                if outcome.received {
                    RxOutcome::Ok
                } else {
                    RxOutcome::Lost
                },
                (!outcome.received).then_some("per"),
                outcome.distance_m,
            );
            self.emit(recorder, &record);
            if outcome.received {
                received_any = true;
                if let Some(inbox) = self.inboxes.get_mut(&outcome.rx) {
                    inbox.push(RxFrame {
                        signer: Some(state.signer.clone()),
                        msg_type: state.msg_type,
                        bytes: state.bytes,
                        claimed_pos: Some(state.claimed_pos),
                        claimed_speed_mps: state.claimed_speed_mps,
                        claimed_heading_rad: state.claimed_heading_rad,
                        claimed_generation_time: state.generation_time,
                        full_certificate: state.full_certificate,
                        // Modelled crypto: the engine knows the sender's key is genuine,
                        // so the signature is valid. The receiver only learns it by
                        // *spending* the verification time, which `ObuRuntime::step`
                        // charges against its servers.
                        signature_valid: true,
                        claimed_cert_period: 0,
                        claimed_linkage: None,
                    });
                }
            }
        }
        if received_any {
            self.report.frames_received += 1;
        }

        let tx_record = NodeTx::new(
            state.start,
            state.tx,
            frame_index,
            msg_type_name(state.msg_type),
            u64::from(state.bytes),
            state.air.as_nanos() / 1000,
            crate::wiring::TX_POWER_DBM,
            SAFETY_CHANNEL,
            if state.full_certificate {
                SignerId::Certificate
            } else {
                SignerId::Digest
            },
            state.generation_time,
        );
        self.emit(recorder, &tx_record);
    }

    /// One link's received power and distance.
    ///
    /// Sequential by necessity: the shadowing process and the fading model are stateful
    /// per link. `rx_power = P_tx − total_loss + fading_gain`, summed with
    /// [`v2xw_core::math::sum_ordered`] so two builds cannot disagree about its last bit.
    fn link_budget(&mut self, state: &FrameState, rx: NodeId, rx_pos: Vec3) -> (f64, f64) {
        let now = self.scheduler.now();
        let link = LinkKey::new(state.tx, rx);
        let distance_m = state.tx_pos.distance(rx_pos);

        let (loss, fade) = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                propagation,
                fading,
                weather,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            let tx_end =
                RadioEndpoint::isotropic(state.tx, state.tx_pos, v2xw_radio::ActorClass::Car, now);
            let rx_end = RadioEndpoint::isotropic(rx, rx_pos, v2xw_radio::ActorClass::Car, now);
            // Line of sight is `clear` because no obstacle model is composed in this
            // build; `v2xw-radio`'s `BuildingShadowing` is the model that fills it, and
            // the seam is the `los` argument rather than a flag.
            let los = LosResult::clear();
            let loss =
                propagation.loss_db(&mut ctx, &tx_end, &rx_end, SAFETY_FREQ_HZ, &los, weather);
            let fade = fading.sample_db(&mut ctx, link, distance_m, now);
            (loss, fade)
        };

        let rssi_dbm =
            v2xw_core::math::sum_ordered([crate::wiring::TX_POWER_DBM, -loss.total_db, fade]);
        (rssi_dbm, distance_m)
    }

    /// The metric phase: every provider flushes its window, and the samples are recorded
    /// on `metric.sample`.
    ///
    /// `Observe` is priority 9 (02-architecture.md §5.1), so a flush sees an instant in
    /// which everything else has already happened — which is the whole reason the class
    /// exists and the reason a metric is not computed inside the phase that produced its
    /// inputs.
    fn on_metric_flush(&mut self, recorder: &mut dyn RunRecorder, horizon: SimTime) {
        let now = self.scheduler.now();
        let samples = self.providers.flush(now);
        for sample in samples {
            self.emit(recorder, &sample);
        }
        let next = self.metric_period.after(now);
        if next <= horizon {
            self.scheduler.schedule(
                next,
                EventClass::Observe,
                Event::Observe {
                    what: Observe::MetricFlush,
                },
            );
        }
    }

    /// Emits a record through a context, so the visibility rule applies to it.
    fn emit(&mut self, recorder: &mut dyn RunRecorder, record: &dyn v2xw_core::ctx::ErasedRecord) {
        let Engine {
            scheduler,
            rng,
            world,
            snapshot,
            provenance,
            params,
            providers,
            report,
            ..
        } = self;
        let mut tee = Tee {
            inner: recorder,
            providers,
        };
        let mut ctx = EngineCtx::new(
            scheduler, rng, world, snapshot, provenance, params, &mut tee,
        );
        v2xw_core::ctx::Ctx::emit_erased(&mut ctx, record);
        let refused = ctx.refused();
        report.records_refused += refused;
        if refused == 0 {
            report.records += 1;
        }
    }
}

/// A recorder that also feeds the run's metric providers.
///
/// A metric provider consumes the *recorded* stream (03-interfaces.md §10), so the split
/// between "what is written" and "what is measured" would be a second source of truth if
/// the engine fed providers from anywhere but the record path. Everything a provider sees
/// is something a recording also contains, which is what makes a metric reproducible from
/// a replay.
struct Tee<'a> {
    inner: &'a mut dyn RunRecorder,
    providers: &'a mut v2xw_metrics::ProviderSet,
}

impl RunRecorder for Tee<'_> {
    fn write(&mut self, at: SimTime, record: &v2xw_core::ctx::OwnedRecord) {
        self.providers.on_event(record);
        self.inner.write(at, record);
    }

    fn refused(&self) -> u64 {
        self.inner.refused()
    }
}

/// One receiver's evaluated outcome for one frame.
#[derive(Debug, Clone, Copy)]
struct LinkOutcome {
    rx: NodeId,
    rssi_dbm: f64,
    sinr_db: f64,
    distance_m: f64,
    received: bool,
}

/// The lower-case name a `node.tx` record carries for a message type.
fn msg_type_name(t: v2xw_msg::MsgType) -> &'static str {
    match t {
        v2xw_msg::MsgType::Bsm => "bsm",
        v2xw_msg::MsgType::Cam => "cam",
        v2xw_msg::MsgType::Denm => "denm",
        v2xw_msg::MsgType::Spat => "spat",
        v2xw_msg::MsgType::Map => "map",
        v2xw_msg::MsgType::Psm => "psm",
        v2xw_msg::MsgType::Vam => "vam",
        v2xw_msg::MsgType::Cpm => "cpm",
        v2xw_msg::MsgType::Srm => "srm",
        v2xw_msg::MsgType::Ssm => "ssm",
        v2xw_msg::MsgType::Wsa => "wsa",
        v2xw_msg::MsgType::Crl => "crl",
        v2xw_msg::MsgType::Mbr => "mbr",
    }
}

/// Re-exported so a caller can size a grid or a range the same way the engine does.
pub const CANDIDATE_RANGE_M: f64 = MAX_RANGE_M;

/// The tier the radio stack runs at, for a caller's report.
pub fn radio_tier(scenario: &Scenario) -> Tier {
    scenario.radio.tiers.phy
}

/// A configured [`Engine`] with `NodeConfig` defaults exposed, for a caller building one
/// node outside a run.
pub fn default_node_config() -> NodeConfig {
    NodeConfig::default()
}
