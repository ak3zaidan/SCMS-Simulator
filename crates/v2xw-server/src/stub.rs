//! A deterministic synthetic engine, so the transport is testable without `v2xw-engine`.
//!
//! This is not a placeholder that returns empty frames. It generates a real world through
//! [`v2xw_world::procedural::grid`], drives equipped and unequipped actors along its
//! driving lanes, reads the world's own fixed-time signal plans for the signal block, and
//! produces telemetry, events, metric samples and provenance on the real channels. That
//! matters because the acceptance test for this crate is an independently written
//! TypeScript client decoding what this server sends: a stub that sent nothing would pass
//! no test worth passing.
//!
//! # Determinism
//!
//! Every quantity below is a closed-form function of the step index, the actor index and
//! constants. There is no RNG in this module at all — not an ad-hoc one and not an
//! [`v2xw_core::RngRegistry`] stream — because the fixture's purpose is to produce the
//! same bytes on every platform and every run, and the cheapest way to guarantee that is
//! to have no random state to seed. `seed` selects a *phase offset*, so two seeds differ
//! but each is reproducible. Transcendentals go through [`v2xw_core::math`], never `std`.
//!
//! # What it is not
//!
//! It models no physics. The numbers it reports are plausible shapes, not measurements,
//! and the model card ([`card`]) says so for every one of them: they are all
//! `todo-calibrate` with the note that calibration means deleting this module and using
//! the engine. Nothing here should ever be cited as a result.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};
use v2xw_core::card::{Family, ModelCard, Parameter, Source, SourceKind};
use v2xw_core::ids::{ActorId, LaneId, NodeId, SignalId};
use v2xw_core::math;
use v2xw_core::time::{Duration, SimTime};
use v2xw_record::encoder::{ActorPose, Cadence, SignalState as WireSignal, Snapshot};
use v2xw_record::wire::event::EventEntry;
use v2xw_record::wire::hello::{
    ChannelRow, ClassRow, HELLO_LIVE, HELLO_SEEKABLE, HelloBody, NODE_HAS_HSM, NodeRow, WorldRef,
};
use v2xw_record::wire::metric::MetricRow;
use v2xw_record::wire::provenance::{PROV_FINAL, ProvEntry, ProvenanceBody};
use v2xw_record::wire::snapshot::{ST_EQUIPPED, ST_TRANSMITTING};
use v2xw_record::wire::telemetry::NodeTelemetry;
use v2xw_record::wire::{StrTable, U32_NONE};
use v2xw_world::model::{LaneKind, SignalState};
use v2xw_world::procedural::GridParams;
use v2xw_world::{ImportOptions, World, WorldPayload};

use crate::engine::{Control, ControlOutcome, Engine, Query, RunDescriptor, RunState, StepOutput};
use crate::error::{Result, ServerError};

/// How the synthetic run is shaped.
#[derive(Debug, Clone)]
pub struct StubOptions {
    /// How many actors to drive. Half of them are equipped with a node.
    pub actors: u32,
    /// The lattice size of the generated grid, in junctions per side.
    pub grid: u32,
    /// Junction spacing, metres.
    pub block_m: f64,
    /// The run's duration in simulated seconds.
    pub duration_s: u64,
    /// The phase offset seed. Reproducible: the same seed gives the same run.
    pub seed: u64,
    /// Start the run paused at `t = 0`.
    pub paused: bool,
    /// A human label for `Hello.str_run_label`.
    pub label: String,
    /// The initial speed multiple; `0` is unthrottled. The binary's `--speed` sets it,
    /// which it did not before: the usage line said "both" and the fixture ran at 1.
    pub speed: f64,
}

impl Default for StubOptions {
    fn default() -> Self {
        StubOptions {
            actors: 120,
            grid: 8,
            block_m: 150.0,
            duration_s: 600,
            seed: 20_260_918,
            paused: false,
            label: String::new(),
            speed: 1.0,
        }
    }
}

/// The actor classes of §3.1.4, in table order.
///
/// Dimensions are the ones `v2xw-world`'s own class table uses; they are rendering hints,
/// not a vehicle-dynamics input, and no result depends on them.
const CLASSES: [(&str, f32, f32, f32, u32, u8); 7] = [
    ("car", 4.5, 1.8, 1.5, 0x4C_9A_FF_FF, 0),
    ("truck", 10.0, 2.5, 3.5, 0xE0_7B_39_FF, 0),
    ("bus", 12.0, 2.55, 3.2, 0xF2_C0_4C_FF, 0),
    ("moto", 2.1, 0.8, 1.4, 0xB4_7B_FF_FF, 0),
    ("bicycle", 1.8, 0.6, 1.7, 0x5FD3_9BFF, 1),
    ("pedestrian", 0.5, 0.5, 1.75, 0xFF_8A_A8_FF, 1),
    ("rsu", 1.0, 1.0, 6.0, 0x9A_A5_B4_FF, 2),
];

/// One actor's fixed assignment: which lane loop it drives and how fast.
#[derive(Debug, Clone)]
struct ActorPlan {
    actor: ActorId,
    node: Option<NodeId>,
    class_idx: u8,
    route: Vec<LaneId>,
    route_len_m: f64,
    speed_mps: f64,
    s0_m: f64,
}

/// The deterministic synthetic engine.
#[derive(Debug)]
pub struct StubEngine {
    descriptor: RunDescriptor,
    world: Arc<WorldPayload>,
    geometry: Arc<World>,
    /// Every signal head group of the world, evaluated per frame.
    groups: Vec<v2xw_world::GroupSignal>,
    options: StubOptions,
    plans: Vec<ActorPlan>,
    /// Node ids in table order, for telemetry and `inspect.node`.
    nodes: Vec<NodeId>,
    rsu_nodes: usize,
    /// Reverse index into the `Hello` symbol table, so an event payload can name a
    /// string without appending to the table (§2.5's append-only rule).
    str_ids: BTreeMap<String, u32>,
    state: RunState,
    step_index: u64,
    speed: f64,
    client_sync: bool,
}

/// The RSU node-id base. Node ids below it belong to vehicles.
const RSU_NODE_BASE: u32 = 1;
/// The first vehicle node id, leaving room for the RSUs below it.
const VEHICLE_NODE_BASE: u32 = 1_000;

impl StubEngine {
    /// Builds the run: generates the world, serialises the payload, plans the actors.
    ///
    /// # Errors
    /// [`ServerError::Internal`] if the world generator or the payload writer refuses the
    /// parameters, which it does only for a degenerate lattice.
    pub fn new(options: StubOptions) -> Result<Self> {
        let grid = options.grid.max(2);
        let params = GridParams {
            cols: grid,
            rows: grid,
            block_x_m: options.block_m,
            block_y_m: options.block_m,
            lanes_per_direction: 2,
            signalised: true,
            crossings: true,
            block_buildings: true,
            rsu_at_junctions: true,
            ..GridParams::legacy()
        };
        let world = v2xw_world::procedural::grid(&params, &ImportOptions::default())?;
        let payload = v2xw_world::serde_vwp::write(&world)?;
        let world = Arc::new(world);
        let payload = Arc::new(payload);

        let plans = plan_actors(&world, &options);
        let rsu_nodes = world.sites.len();
        let mut nodes: Vec<NodeId> = (0..rsu_nodes)
            .map(|i| NodeId::new(RSU_NODE_BASE + u32::try_from(i).unwrap_or(u32::MAX)))
            .collect();
        nodes.extend(plans.iter().filter_map(|p| p.node));

        let run_id_bytes = run_id_bytes(options.seed, options.actors);
        let run_id = uuid_string(&run_id_bytes);
        let scenario = scenario_document(&options, grid);
        let scenario_hash_hex = v2xw_core::hash::sha256_hex(
            &v2xw_core::hash::canonical_json(&scenario).unwrap_or_default(),
        );
        let hello = build_hello(
            &world,
            &payload,
            &plans,
            &nodes,
            rsu_nodes,
            &options,
            run_id_bytes,
            &scenario_hash_hex,
        );
        let str_ids: BTreeMap<String, u32> = hello
            .strings
            .strings
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), u32::try_from(i).unwrap_or(0)))
            .collect();
        let hello_strings = hello.strings.clone();
        let bbox = world.bbox;
        let descriptor = RunDescriptor {
            run_id,
            run_id_bytes,
            hello,
            cadence: Cadence::DEFAULT,
            origin_m: [bbox.min.x.floor(), bbox.min.y.floor(), 0.0],
            duration: (options.duration_s.saturating_mul(1_000_000_000)),
            live: true,
            seekable: true,
            scenario,
            scenario_hash_hex,
            recording_path: None,
            provenance: Some(provenance_body(0, &hello_strings)),
        };

        let options_speed = options.speed;
        Ok(StubEngine {
            descriptor,
            world: payload,
            groups: world.group_signals(),
            geometry: world,
            state: if options.paused {
                RunState::Paused
            } else {
                RunState::Running
            },
            options,
            plans,
            nodes,
            rsu_nodes,
            str_ids,
            step_index: 0,
            speed: options_speed,
            client_sync: false,
        })
    }

    /// The generated world, for `inspect.*` answers that quote geometry.
    pub fn geometry(&self) -> &Arc<World> {
        &self.geometry
    }

    /// The shape this run was built with.
    pub fn options(&self) -> &StubOptions {
        &self.options
    }

    fn step_ns(&self) -> u64 {
        self.descriptor.cadence.mobility_step.as_nanos()
    }

    fn time_of(&self, step: u64) -> SimTime {
        step.saturating_mul(self.step_ns())
    }

    /// The state of the world at `step`, with no side effects.
    ///
    /// Pure in `step`: this is what makes seek and normal production produce the same
    /// bytes for the same instant, which is the live/replay parity of §7.2 applied to a
    /// fixture.
    fn output_at(&self, step: u64) -> StepOutput {
        let t = self.time_of(step);
        let t_s = (step as f64) * (self.step_ns() as f64) * 1e-9;
        let mut actors = Vec::with_capacity(self.plans.len());
        for (i, plan) in self.plans.iter().enumerate() {
            let Some(pose) = self.pose_of(plan, t_s) else {
                continue;
            };
            let slot = u32::try_from(i).unwrap_or(u32::MAX);
            let mut state = 0u8;
            if plan.node.is_some() {
                state |= ST_EQUIPPED;
                // One transmission per 100 ms step for an equipped node, offset by index
                // so the bit is not in lockstep across the fleet.
                if (step + i as u64) % 2 == 0 {
                    state |= ST_TRANSMITTING;
                }
            }
            actors.push(ActorPose {
                slot,
                actor: plan.actor,
                node: plan.node,
                pos_m: [pose.0, pose.1, pose.2],
                heading_rad: pose.3,
                speed_mps: plan.speed_mps,
                accel_mps2: pose.4,
                lane: Some(pose.5),
                class_idx: plan.class_idx,
                state,
                verified_neighbors: verified_neighbours(i, step),
            });
        }

        let signals = self.signals_at(t_s);
        let mut snapshot = Snapshot::new(t, actors);
        snapshot.signals = signals;

        let telemetry = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| self.telemetry_of(i, *n, step))
            .collect();
        let events = self.events_at(step, t);
        let metrics = if step > 0 && step % self.steps_per_second() == 0 {
            self.metrics_at(step)
        } else {
            Vec::new()
        };

        StepOutput {
            sim_time: t,
            snapshot,
            telemetry,
            events,
            metrics,
            provenance: None,
            // The *last* produced step, not the first one past the end: `step` stops
            // before `duration`, so a `t >= duration` test here would never fire and the
            // stream would end without `FLAG_END_OF_RUN` and without a `Bye{reason=0}` —
            // leaving a conforming client reconnecting with backoff forever (§1.4's last
            // paragraph).
            end_of_run: t.saturating_add(self.step_ns()) >= self.descriptor.duration,
            recorded: Vec::new(),
            generation: 0,
        }
    }

    fn steps_per_second(&self) -> u64 {
        (1_000_000_000 / self.step_ns()).max(1)
    }

    /// `(x, y, z, heading, accel, lane)` for an actor at `t_s`, or `None` before it spawns.
    fn pose_of(&self, plan: &ActorPlan, t_s: f64) -> Option<(f64, f64, f64, f64, f64, LaneId)> {
        if plan.route.is_empty() || plan.route_len_m <= 0.0 {
            return None;
        }
        let mut s = (plan.s0_m + plan.speed_mps * t_s) % plan.route_len_m;
        if s < 0.0 {
            s += plan.route_len_m;
        }
        for lane_id in &plan.route {
            let lane = self.geometry.roads.try_lane(*lane_id)?;
            if s < lane.length_m {
                let (p, heading) = lane.pose_at(s);
                // A gentle, closed-form longitudinal acceleration so the GT column is not
                // a constant zero — the one thing that would make the node-profile
                // blanking test vacuous.
                let accel = 0.6 * math::sin(0.25 * t_s + f64::from(plan.actor.index()) * 0.37);
                return Some((p.x, p.y, p.z, heading, accel, *lane_id));
            }
            s -= lane.length_m;
        }
        let last = *plan.route.last()?;
        let lane = self.geometry.roads.try_lane(last)?;
        let (p, heading) = lane.pose_at(lane.length_m);
        Some((p.x, p.y, p.z, heading, 0.0, last))
    }

    /// One row per signal head group (§3.3.3), each the state its own heads show.
    ///
    /// It used to send one row per controller carrying the controller's *first*
    /// movement's state, so every head of a junction lit the same colour — cross streets
    /// green together (14,874 green head-samples against 1,406 red on the fixture run).
    fn signals_at(&self, t_s: f64) -> Vec<WireSignal> {
        self.groups
            .iter()
            .filter_map(|g| {
                let (state, remaining) = g.at(t_s)?;
                Some(WireSignal {
                    signal: SignalId::new(g.wire_id),
                    phase: movement_phase(state),
                    time_to_change: Some(Duration::from_nanos((remaining * 1e9) as u64)),
                })
            })
            .collect()
    }

    fn telemetry_of(&self, i: usize, node: NodeId, step: u64) -> NodeTelemetry {
        let is_rsu = i < self.rsu_nodes;
        let phase = (i as f64) * 0.61 + (step as f64) * 0.02;
        let wobble = 0.5 + 0.5 * math::sin(phase);
        let mut t = NodeTelemetry::unknown(node.index());
        t.storage_total_b = if is_rsu { 8 << 20 } else { 2 << 20 };
        t.storage_used_b = t.storage_total_b / 3 + (step % 97) * 512;
        t.next_topup_ns = 600_000_000_000;
        t.crl_bytes = 4_096 + (step % 13) * 128;
        t.outbox_bytes = (step % 7) * 96;
        t.clock_offset_ns = ((i as i64) % 11 - 5) * 1_200;
        t.ram_used_kib = 8_192 + u32::try_from(step % 512).unwrap_or(0);
        t.ram_total_kib = if is_rsu { 262_144 } else { 65_536 };
        t.drop_rx_overflow = u32::try_from(step / 40).unwrap_or(0);
        t.drop_verify_policy_skip = u32::try_from(step / 25).unwrap_or(0);
        t.drop_verify_overflow = 0;
        t.drop_tx_overflow = 0;
        t.drop_reassembly_timeout = 0;
        t.drop_crl_backlog = 0;
        t.cert_stored = 20;
        t.crl_entries = 12;
        t.outbox_msgs = u32::try_from(step % 4).unwrap_or(0);
        t.peer_cache_entries = 40 + u32::try_from(step % 20).unwrap_or(0);
        t.p2pcd_requests = u32::try_from(step % 3).unwrap_or(0);
        t.full_cert_msgs = u32::try_from(step % 10).unwrap_or(0);
        t.msgs_in_per_s = (60.0 + 90.0 * wobble) as f32;
        t.msgs_out_per_s = 10.0;
        t.verifications_per_s = (50.0 + 80.0 * wobble) as f32;
        t.verify_wait_p50_ms = (0.4 + 1.2 * wobble) as f32;
        t.verify_wait_p95_ms = (1.1 + 3.5 * wobble) as f32;
        t.gnss_hdop = (0.8 + 0.6 * wobble) as f32;
        t.gnss_sigma_m = (1.2 + 0.9 * wobble) as f32;
        t.clock_drift_ppm = ((i % 9) as f32) - 4.0;
        t.pos_error_m = (0.6 + 1.4 * wobble) as f32;
        t.airtime_ms_per_s = (6.0 + 12.0 * wobble) as f32;
        t.cpu_util_pm = (120.0 + 600.0 * wobble) as u16;
        t.hsm_util_pm = (80.0 + 400.0 * wobble) as u16;
        t.q_rx_p50 = (2.0 + 8.0 * wobble) as u16;
        t.q_rx_p95 = (6.0 + 20.0 * wobble) as u16;
        t.q_verify_p50 = (1.0 + 6.0 * wobble) as u16;
        t.q_verify_p95 = (4.0 + 18.0 * wobble) as u16;
        t.q_app_p50 = 1;
        t.q_app_p95 = 3;
        t.q_tx_p50 = 1;
        t.q_tx_p95 = 2;
        t.q_crl_p50 = 0;
        t.q_crl_p95 = 1;
        t.dcc_state = if wobble > 0.75 { 1 } else { 0 };
        t.cbr_pm = (120.0 + 500.0 * wobble) as u16;
        t.tx_power_cdbm = 2_000;
        t.nbr_total = (12.0 + 40.0 * wobble) as u16;
        t.nbr_verified = t.nbr_total * 3 / 4;
        t.nbr_unverified = t.nbr_total - t.nbr_verified;
        t.nbr_revoked = 0;
        t.cert_active = 20;
        t.crl_expansion_pm = 1_000;
        t.unverified_ratio_pm = (100.0 + 200.0 * wobble) as u16;
        t.gnss_fix = if wobble > 0.9 { 1 } else { 2 };
        t.node_state = 2;
        t.verify_policy = 2;
        t.quantised()
    }

    fn events_at(&self, step: u64, t: SimTime) -> Vec<EventEntry> {
        let mut out: Vec<EventEntry> = Vec::new();
        let equipped: Vec<&ActorPlan> = self.plans.iter().filter(|p| p.node.is_some()).collect();
        if equipped.is_empty() {
            return out;
        }
        // A handful of nodes transmit each step, rotating so every node is covered.
        let per_step = equipped.len().min(8);
        for k in 0..per_step {
            let idx = ((step as usize) * per_step + k) % equipped.len();
            let plan = equipped[idx];
            let Some(node) = plan.node else { continue };
            let msg_id =
                u32::try_from((step * 64 + k as u64) % u64::from(U32_NONE - 1)).unwrap_or(0);
            out.push(EventEntry {
                sim_time_ns: t,
                channel_id: 10,
                payload: node_tx_payload(node.index(), msg_id),
            });
            // One reception of it by the next equipped node along.
            let rx = equipped[(idx + 1) % equipped.len()];
            if let Some(rx_node) = rx.node {
                out.push(EventEntry {
                    sim_time_ns: t,
                    channel_id: 11,
                    payload: phy_rx_payload(t, rx_node.index(), node.index(), msg_id, idx),
                });
                out.push(EventEntry {
                    sim_time_ns: t,
                    channel_id: 14,
                    payload: node_verify_payload(t, rx_node.index(), msg_id),
                });
            }
        }
        if step % 10 == 0 {
            let plan = equipped[(step as usize / 10) % equipped.len()];
            if let Some(node) = plan.node {
                out.push(EventEntry {
                    sim_time_ns: t,
                    channel_id: 20,
                    payload: sec_cert_payload(node.index(), step),
                });
                out.push(EventEntry {
                    sim_time_ns: t,
                    channel_id: 40,
                    payload: app_warning_payload(
                        node.index(),
                        plan.actor.index(),
                        self.str_id("fcw"),
                        step,
                    ),
                });
            }
        }
        if step % 50 == 25 {
            let plan = equipped[(step as usize / 50) % equipped.len()];
            if let Some(node) = plan.node {
                out.push(EventEntry {
                    sim_time_ns: t,
                    channel_id: 30,
                    payload: det_observation_payload(
                        node.index(),
                        plan.actor.index(),
                        self.str_id("detector/plausibility/position-jump"),
                    ),
                });
                out.push(EventEntry {
                    sim_time_ns: t,
                    channel_id: 22,
                    payload: revocation_payload(node.index(), step),
                });
            }
        }
        // §3.6.1: the index MUST be sorted by (sim_time_ns, channel_id). Every entry in a
        // batch shares `t`, so this is a sort on the channel id alone — but it is done
        // with the pair, because that is the rule, and a later producer that batches two
        // steps must not have to remember to change it.
        out.sort_by_key(|e| (e.sim_time_ns, e.channel_id));
        out
    }

    fn metrics_at(&self, step: u64) -> Vec<MetricRow> {
        let t_s = (step as f64) * (self.step_ns() as f64) * 1e-9;
        let wobble = 0.5 + 0.5 * math::sin(t_s * 0.1);
        let row = |name: u32, value: f64, agg: u16, visibility: u8, prov: u32| MetricRow {
            value: math::quantize(value, 6),
            str_metric: name,
            dim_key: 0,
            node_id: U32_NONE,
            count: 100,
            agg,
            visibility,
            prov_id: prov,
        };
        vec![
            row(self.str_id("pdr"), 0.82 + 0.12 * wobble, 5, 3, 1),
            row(self.str_id("cbr"), 0.18 + 0.30 * wobble, 1, 1, 2),
            row(self.str_id("pir_p95_s"), 0.28 + 0.20 * wobble, 3, 1, 1),
            row(
                self.str_id("verify_wait_p95_ms"),
                1.2 + 3.0 * wobble,
                3,
                1,
                3,
            ),
            row(self.str_id("ttc_min"), 2.4 + 1.5 * wobble, 8, 0, 4),
        ]
    }

    /// The id of a string the run's `Hello` table already holds, or `0` (`""`).
    fn str_id(&self, s: &str) -> u32 {
        self.str_ids.get(s).copied().unwrap_or(0)
    }
}

fn verified_neighbours(i: usize, step: u64) -> u8 {
    let v = 8.0 + 10.0 * (0.5 + 0.5 * math::sin((i as f64) * 0.3 + (step as f64) * 0.05));
    v as u8
}

/// The SAE J2735 `MovementPhaseState` code for a world signal state (§3.3.3).
fn movement_phase(s: SignalState) -> u8 {
    s.j2735_phase()
}

fn plan_actors(world: &World, options: &StubOptions) -> Vec<ActorPlan> {
    let driving: Vec<LaneId> = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Driving && l.length_m > 5.0)
        .map(|l| l.id)
        .collect();
    if driving.is_empty() {
        return Vec::new();
    }
    let mut plans = Vec::with_capacity(options.actors as usize);
    let mut next_node = VEHICLE_NODE_BASE;
    for i in 0..options.actors {
        let start = driving[(i as usize + (options.seed as usize % 7)) % driving.len()];
        // A route of up to four lanes, following the successor graph where it exists and
        // falling back to the next lane in id order when it does not. Deterministic either
        // way, and always non-empty.
        let mut route = vec![start];
        let mut cursor = start;
        for _ in 0..3 {
            let next = world
                .roads
                .successors(cursor)
                .iter()
                .filter(|c| c.permitted)
                .map(|c| c.to_lane)
                .find(|id| world.roads.try_lane(*id).is_some_and(|l| l.length_m > 1.0));
            let next = next.unwrap_or_else(|| {
                driving
                    [(driving.iter().position(|d| *d == cursor).unwrap_or(0) + 1) % driving.len()]
            });
            if route.contains(&next) {
                break;
            }
            route.push(next);
            cursor = next;
        }
        let route_len_m: f64 = route
            .iter()
            .filter_map(|id| world.roads.try_lane(*id))
            .map(|l| l.length_m)
            .sum();
        // Every third actor is an unequipped pedestrian, which is what gives the node
        // profile something to withhold (§5.2's last rule).
        let equipped = i % 3 != 2;
        let class_idx = if equipped {
            match i % 8 {
                0 => 1, // truck
                3 => 2, // bus
                5 => 3, // moto
                _ => 0, // car
            }
        } else {
            5 // pedestrian
        };
        let node = if equipped {
            let n = NodeId::new(next_node);
            next_node += 1;
            Some(n)
        } else {
            None
        };
        let base = if equipped { 9.5 } else { 1.3 };
        let speed_mps = base + f64::from(i % 7) * 0.45;
        plans.push(ActorPlan {
            actor: ActorId::new(i),
            node,
            class_idx,
            s0_m: (f64::from(i) * 17.3 + (options.seed % 991) as f64) % route_len_m.max(1.0),
            route,
            route_len_m,
            speed_mps,
        });
    }
    plans
}

#[allow(clippy::too_many_arguments)]
fn build_hello(
    world: &World,
    payload: &WorldPayload,
    plans: &[ActorPlan],
    nodes: &[NodeId],
    rsu_nodes: usize,
    options: &StubOptions,
    run_id: [u8; 16],
    scenario_hash_hex: &str,
) -> HelloBody {
    let mut strings = StrTable::new();
    let engine_version = strings.intern(concat!("v2xw-server ", env!("CARGO_PKG_VERSION")));
    let scenario_name = strings.intern("stub/grid");
    let run_label = strings.intern(&options.label);
    let session_token = strings.intern("");
    let url = strings.intern(&payload.url_path());

    let classes: Vec<ClassRow> = CLASSES
        .iter()
        .map(|(name, l, w, h, rgba, cat)| ClassRow {
            str_name: strings.intern(name),
            length_m: *l,
            width_m: *w,
            height_m: *h,
            color_rgba: *rgba,
            category: *cat,
        })
        .collect();

    let mut node_rows = Vec::with_capacity(nodes.len());
    for (i, site) in world.sites.iter().enumerate().take(rsu_nodes) {
        node_rows.push(NodeRow {
            node_id: RSU_NODE_BASE + u32::try_from(i).unwrap_or(0),
            actor_id: U32_NONE,
            pos_m: [
                site.position.x as f32,
                site.position.y as f32,
                (site.position.z + site.antenna_height_m) as f32,
            ],
            str_label: strings.intern(&format!("rsu_{i:04}")),
            str_profile_id: strings.intern("rsu/cohda-mk5-rsu"),
            flags: NODE_HAS_HSM,
            kind: 2,
            class_idx: 6,
        });
    }
    for plan in plans {
        let Some(node) = plan.node else { continue };
        let lane = plan.route.first().and_then(|id| world.roads.try_lane(*id));
        let (p, _) = lane.map_or((v2xw_core::Vec3::ZERO, 0.0), |l| {
            l.pose_at(plan.s0_m.min(l.length_m))
        });
        node_rows.push(NodeRow {
            node_id: node.index(),
            actor_id: plan.actor.index(),
            pos_m: [p.x as f32, p.y as f32, (p.z + 1.5) as f32],
            str_label: strings.intern(&format!("veh_{:04}", plan.actor.index())),
            str_profile_id: strings.intern("obu/cohda-mk5"),
            flags: NODE_HAS_HSM,
            kind: 0,
            class_idx: plan.class_idx,
        });
    }

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

    // Metric names the stream will reference by string id, interned up front so
    // `MetricSample.str_metric` resolves without a symbol-table extension (§2.5).
    for name in [
        "pdr",
        "cbr",
        "pir_p95_s",
        "verify_wait_p95_ms",
        "ttc_min",
        "fcw",
        "eebl",
        "ima",
        "vru",
        "detector/plausibility/position-jump",
    ] {
        strings.intern(name);
    }

    let mut scenario_hash = [0u8; 32];
    for (i, b) in scenario_hash_hex.as_bytes().chunks(2).take(32).enumerate() {
        let hi = (b[0] as char).to_digit(16).unwrap_or(0) as u8;
        let lo = b
            .get(1)
            .and_then(|c| (*c as char).to_digit(16))
            .unwrap_or(0) as u8;
        scenario_hash[i] = (hi << 4) | lo;
    }

    let bbox = world.bbox;
    HelloBody {
        version_minor: 0,
        hello_flags: HELLO_LIVE | HELLO_SEEKABLE,
        run_id,
        scenario_hash,
        world_hash: payload.content_hash,
        t0_wall_ns: 0,
        sim_duration_ns: options.duration_s.saturating_mul(1_000_000_000),
        mobility_step_ns: Cadence::DEFAULT.mobility_step.as_nanos(),
        keyframe_period_ns: Cadence::DEFAULT.keyframe_period.as_nanos(),
        telemetry_period_ns: 1_000_000_000,
        metric_period_ns: 1_000_000_000,
        resume_seq: 0,
        sim_time_ns: 0,
        origin_lat_deg: world.origin.lat_deg,
        origin_lon_deg: world.origin.lon_deg,
        origin_alt_m: world.origin.alt_m,
        bbox_m: [bbox.min.x, bbox.min.y, bbox.max.x, bbox.max.y],
        actor_capacity: options.actors.max(1),
        nodes: node_rows,
        classes,
        channels,
        world_ref: WorldRef {
            mode: 0,
            format: 0,
            payload_bytes: u32::try_from(payload.body().len() + 16).unwrap_or(u32::MAX),
            str_url: url,
        },
        str_engine_version: engine_version,
        str_scenario_name: scenario_name,
        str_run_label: run_label,
        str_session_token: session_token,
        strings,
    }
}

fn scenario_document(options: &StubOptions, grid: u32) -> Value {
    json!({
        "schema": "v2xw/scenario/1",
        "meta": {"name": "stub/grid", "description": "the server's synthetic fixture run"},
        "seed": options.seed,
        "time": {"t0": "1970-01-01T00:00:00Z", "duration_s": options.duration_s,
                 "mobility_step_ms": 100, "keyframe_period_ms": 1000},
        "world": {"source": "procedural", "kind": "grid", "cols": grid, "rows": grid,
                  "block_m": options.block_m},
        "traffic": {"actors": options.actors, "equipped_fraction": 2.0 / 3.0},
        "radio": {"tiers": {"phy": "abstract", "mac": "abstract"}},
        "security": {"tiers": {"crypto": "abstract"}}
    })
}

/// A run id derived from the run's own parameters, so a restart with the same parameters
/// is the same run id — there is no clock and no randomness to draw one from.
fn run_id_bytes(seed: u64, actors: u32) -> [u8; 16] {
    let digest = v2xw_core::hash::sha256(format!("v2xw-server/stub/{seed}/{actors}").as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    // Stamp the UUIDv7 version and variant nibbles (RFC 9562) so the 36-character form
    // satisfies `#/$defs/RunId`'s `format: uuid`.
    out[6] = (out[6] & 0x0F) | 0x70;
    out[8] = (out[8] & 0x3F) | 0x80;
    out
}

/// The 36-character hyphenated form of 16 UUID bytes.
pub fn uuid_string(b: &[u8; 16]) -> String {
    let hex = v2xw_core::hash::hex_encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

// --- event payload encoders (§3.6.4 onwards) ---------------------------------------
// These write the exact byte layouts of §3.6. They are here rather than in `v2xw-record`
// because they are fixture content, not wire structure: the layouts they fill are the
// specification's, but the values are this module's invention.

fn node_tx_payload(node_id: u32, msg_id: u32) -> Vec<u8> {
    let mut p = vec![0u8; 40];
    p[0..4].copy_from_slice(&node_id.to_le_bytes());
    p[4..8].copy_from_slice(&msg_id.to_le_bytes());
    p[8..12].copy_from_slice(&350u32.to_le_bytes());
    p[12..16].copy_from_slice(&0.52f32.to_le_bytes());
    p[16..18].copy_from_slice(&1u16.to_le_bytes()); // BSM
    p[18..20].copy_from_slice(&2000i16.to_le_bytes());
    p[20..22].copy_from_slice(&172u16.to_le_bytes());
    p[22] = 2;
    p[23] = 0; // AC_VO
    p[24] = 0; // DCC RELAXED
    p[25] = 0; // signer: digest
    p[26..28].copy_from_slice(&118u16.to_le_bytes());
    p[28..36].copy_from_slice(&digest8(node_id));
    p[36..40].copy_from_slice(&(node_id * 7 + 1).to_le_bytes());
    p
}

fn phy_rx_payload(t_ns: u64, rx: u32, tx: u32, msg_id: u32, k: usize) -> Vec<u8> {
    let mut p = vec![0u8; 48];
    p[0..8].copy_from_slice(&t_ns.to_le_bytes());
    p[8..16].copy_from_slice(&(t_ns + 520_000).to_le_bytes());
    p[16..20].copy_from_slice(&rx.to_le_bytes());
    p[20..24].copy_from_slice(&tx.to_le_bytes());
    p[24..28].copy_from_slice(&msg_id.to_le_bytes());
    p[28..32].copy_from_slice(&(-71.5f32 - (k % 20) as f32).to_le_bytes());
    p[32..36].copy_from_slice(&(14.0f32 - (k % 9) as f32).to_le_bytes());
    p[36..40].copy_from_slice(&(35.0f32 + (k % 40) as f32).to_le_bytes());
    p[40] = 0; // ok
    p[41] = 0; // no loss cause
    p[42] = if k % 4 == 0 { 1 } else { 0 };
    p
}

fn node_verify_payload(t_ns: u64, node_id: u32, msg_id: u32) -> Vec<u8> {
    let mut p = vec![0u8; 48];
    p[0..8].copy_from_slice(&t_ns.to_le_bytes());
    p[8..16].copy_from_slice(&(t_ns + 100_000).to_le_bytes());
    p[16..24].copy_from_slice(&(t_ns + 420_000).to_le_bytes());
    p[24..28].copy_from_slice(&node_id.to_le_bytes());
    p[28..32].copy_from_slice(&msg_id.to_le_bytes());
    p[32..36].copy_from_slice(&320.0f32.to_le_bytes());
    p[36..38].copy_from_slice(&1u16.to_le_bytes()); // ecdsa-p256-verify
    p[38] = 0; // valid
    p[39] = 0; // admit now
    p[40] = 0;
    p[41] = 0; // cpu
    p[42..44].copy_from_slice(&3u16.to_le_bytes());
    p
}

fn sec_cert_payload(node_id: u32, step: u64) -> Vec<u8> {
    let mut p = vec![0u8; 40];
    p[0..8].copy_from_slice(&(step * 100_000_000).to_le_bytes());
    p[8..16].copy_from_slice(&(step * 100_000_000 + 300_000_000_000).to_le_bytes());
    p[16..20].copy_from_slice(&node_id.to_le_bytes());
    p[20..24].copy_from_slice(&(node_id * 13 + 5).to_le_bytes());
    p[24..32].copy_from_slice(&digest8(node_id));
    p[32] = 0; // change
    p[33] = 0; // pseudonym
    p[34..36].copy_from_slice(&u16::try_from(step / 600).unwrap_or(0).to_le_bytes());
    p[36..38].copy_from_slice(&1u16.to_le_bytes());
    p[38..40].copy_from_slice(&1u16.to_le_bytes());
    p
}

fn app_warning_payload(node_id: u32, actor_id: u32, str_app: u32, step: u64) -> Vec<u8> {
    let mut p = vec![0u8; 32];
    p[0..4].copy_from_slice(&node_id.to_le_bytes());
    p[4..8].copy_from_slice(&str_app.to_le_bytes());
    p[8..16].copy_from_slice(&digest8(node_id + 1));
    p[16..20].copy_from_slice(&(2.4f32 + (step % 5) as f32).to_le_bytes());
    p[20..24].copy_from_slice(&(28.0f32 + (step % 17) as f32).to_le_bytes());
    p[24] = 0; // issue
    p[25] = 2; // warning
    p[26] = 1; // true positive
    p[28..32].copy_from_slice(&actor_id.to_le_bytes());
    p
}

fn det_observation_payload(node_id: u32, actor_id: u32, str_detector: u32) -> Vec<u8> {
    let mut p = vec![0u8; 32];
    p[0..4].copy_from_slice(&node_id.to_le_bytes());
    p[4..8].copy_from_slice(&str_detector.to_le_bytes());
    p[8..16].copy_from_slice(&digest8(node_id + 2));
    p[16..20].copy_from_slice(&0.63f32.to_le_bytes());
    p[20..24].copy_from_slice(&actor_id.to_le_bytes());
    p[24..26].copy_from_slice(&12u16.to_le_bytes());
    p[26] = 0; // plausibility
    p[28..32].copy_from_slice(&5u32.to_le_bytes());
    p
}

fn revocation_payload(node_id: u32, step: u64) -> Vec<u8> {
    let mut p = vec![0u8; 32];
    p[0..4].copy_from_slice(&node_id.to_le_bytes());
    p[4..8].copy_from_slice(&u32::try_from(step / 50).unwrap_or(0).to_le_bytes());
    p[8..16].copy_from_slice(&digest8(node_id + 3));
    p[16..24].copy_from_slice(&4_096u64.to_le_bytes());
    p[24..28].copy_from_slice(&U32_NONE.to_le_bytes());
    // Stage 5 `issued`, the earliest stage the `node` profile may see (§5.2).
    p[28] = 5;
    p[29] = 0; // active CRL
    p[30..32].copy_from_slice(&12u16.to_le_bytes());
    p
}

fn digest8(seed: u32) -> [u8; 8] {
    let d = v2xw_core::hash::sha256(&seed.to_le_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&d[..8]);
    out
}

impl Engine for StubEngine {
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
        self.time_of(self.step_index)
    }

    fn speed(&self) -> (f64, bool) {
        (self.speed, self.client_sync)
    }

    fn counts(&self) -> (u32, u32) {
        (
            u32::try_from(self.plans.len()).unwrap_or(u32::MAX),
            u32::try_from(self.nodes.len()).unwrap_or(u32::MAX),
        )
    }

    fn control(&mut self, command: Control) -> Result<ControlOutcome> {
        let t_ns = self.sim_time();
        match command {
            Control::Start { paused, speed, .. } => {
                if self.state == RunState::Running {
                    return Err(ServerError::RunAlreadyRunning);
                }
                // `run.start` rewinds (§6.6: it "begins producing the stream" of a run).
                // It used to only set the state, so on a finished fixture run it reported
                // `running` and produced nothing: the page's Run-again did nothing at all.
                self.step_index = 0;
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
            Control::Stop { .. } => {
                self.state = RunState::Finished;
            }
        }
        Ok(ControlOutcome {
            state: self.state,
            t_ns,
            extra: BTreeMap::new(),
        })
    }

    fn step(&mut self) -> Result<Option<StepOutput>> {
        if self.time_of(self.step_index) >= self.descriptor.duration {
            self.state = RunState::Finished;
            return Ok(None);
        }
        let out = self.output_at(self.step_index);
        self.step_index += 1;
        Ok(Some(out))
    }

    fn seek(&mut self, t: SimTime) -> Result<Vec<StepOutput>> {
        let (min_ns, max_ns) = self.seek_range();
        if t > max_ns || t < min_ns {
            return Err(ServerError::SeekOutOfRange { min_ns, max_ns });
        }
        let step_ns = self.step_ns();
        let target = t / step_ns;
        let per_gop = self.descriptor.cadence.max_deltas_per_gop();
        // Start at the keyframe boundary of the GOP holding the target, so the caller can
        // emit a keyframe and at most `per_gop` deltas (conformance P4).
        let first = target - (target % per_gop);
        let outputs = (first..=target).map(|s| self.output_at(s)).collect();
        self.step_index = target + 1;
        self.state = RunState::Paused;
        Ok(outputs)
    }

    fn seek_range(&self) -> (u64, u64) {
        (0, self.descriptor.duration)
    }

    fn metric_catalogue(&self) -> Vec<crate::introspect::MetricInfo> {
        FIXTURE_METRICS
            .iter()
            .map(
                |(name, unit, agg, visibility)| crate::introspect::MetricInfo {
                    source: String::new(),
                    base: name.to_string(),
                    name: (*name).to_string(),
                    unit: (*unit).to_string(),
                    agg: (*agg).to_string(),
                    visibility: (*visibility).to_string(),
                    definition_md: format!("`{name}` as defined in 08-measurement."),
                    dims: vec!["t".to_string()],
                    not_accounted: vec![
                        "nothing: this is a fixture, not a measurement".to_string(),
                    ],
                    str_id: self.str_id(name),
                    agg_code: match *agg {
                        "ratio" => 5,
                        "mean" => 1,
                        "p95" => 3,
                        "min" => 8,
                        _ => 0,
                    },
                },
            )
            .collect()
    }

    fn live_nodes(&self) -> Option<Vec<crate::engine::NodeFacts>> {
        None
    }

    fn query(&mut self, query: &Query) -> Result<Value> {
        crate::introspect::answer(self, query)
    }
}

/// The metric catalogue the fixture answers `metrics.query` with.
///
/// `visibility` follows §5.2: `ttc_min` is ground truth and therefore refused on a
/// `node`-profile connection, which is what conformance V3 checks.
pub const FIXTURE_METRICS: [(&str, &str, &str, &str); 5] = [
    ("pdr", "-", "ratio", "DERIVED"),
    ("cbr", "-", "mean", "NODE"),
    ("pir_p95_s", "s", "p95", "NODE"),
    ("verify_wait_p95_ms", "ms", "p95", "NODE"),
    ("ttc_min", "s", "min", "GT"),
];

impl crate::introspect::Introspect for StubEngine {
    fn node_list(&self) -> Vec<crate::engine::NodeFacts> {
        let hello = &self.descriptor.hello;
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

    fn telemetry_of(&self, node: u32) -> Option<NodeTelemetry> {
        let index = self.nodes.iter().position(|n| n.index() == node)?;
        Some(self.telemetry_of(index, NodeId::new(node), self.step_index))
    }

    fn metric_series(
        &self,
        name: &str,
        from: u64,
        to: u64,
        bin: u64,
        limit: usize,
    ) -> Vec<(u64, Option<f64>)> {
        let bin = bin.max(1);
        let mut out = Vec::new();
        let mut t = from;
        while t <= to && out.len() < limit {
            let phase = (t as f64) * 1e-9 * 0.1;
            out.push((t, Some(fixture_metric_value(name, phase))));
            t = t.saturating_add(bin);
        }
        out
    }

    fn provenance_chain(&self) -> Vec<Value> {
        vec![
            json!({"prov_id": 1, "model_id": "net/delivery/pdr-from-phy-rx",
                   "model_version": "0.1.0", "param_set_id": "b3:stub-fixture",
                   "family": "net", "card_url": "/cards/net-delivery-pdr-from-phy-rx",
                   "assumptions": ["a transport fixture, not a model"]}),
            json!({"prov_id": 2, "model_id": "radio/mac/cbr-window", "model_version": "0.1.0",
                   "param_set_id": "b3:stub-fixture", "family": "mac",
                   "card_url": "/cards/radio-mac-cbr-window"}),
            json!({"prov_id": 3, "model_id": "node/hsm/ecdsa-p256-service-time",
                   "model_version": "0.1.0", "param_set_id": "b3:stub-fixture",
                   "family": "primitive",
                   "card_url": "/cards/node-hsm-ecdsa-p256-service-time"}),
        ]
    }

    fn caveats(&self) -> Vec<String> {
        vec![
            "produced by the server's synthetic fixture, not by an engine".to_string(),
            "every number this run reports is a plausible shape with no source".to_string(),
        ]
    }

    fn node_section(&self, node: u32, section: &str, limit: usize) -> Option<Value> {
        match section {
            "queues" => Some(json!({
                "rx": {"depth": 4, "p50": 2.0, "p95": 9.0, "policy": "drop-tail",
                       "drops": {"overflow": 0}},
                "verify": {"depth": 3, "p50": 1.0, "p95": 7.0, "policy": "prioritized",
                           "drops": {"overflow": 0, "policy_skip": 2}},
                "tx": {"depth": 1, "p50": 1.0, "p95": 2.0, "policy": "edca",
                       "drops": {"overflow": 0}}
            })),
            "stores" => Some(json!({
                "cert_store": {"entries": 20, "bytes": 8_192, "capacity": 100},
                "peer_cache": {"entries": 40, "bytes": 32_768, "capacity": 256},
                "crl_store": {"entries": 12, "bytes": 4_096},
                "trust_store": {"anchors": 2},
                "neighbor_table": {"entries": 24, "capacity": 128},
                "evidence_buffer": {"entries": 0, "bytes": 0},
                "report_outbox": {"entries": 0, "bytes": 0}
            })),
            "crl" => Some(json!({"entries": 12, "bytes": 4096, "i_period": 0})),
            "gnss" => Some(json!({"fix": "3D", "hdop": 1.1, "sigma_m": 1.6,
                                  "satellites": 11})),
            "clock" => Some(json!({"offset_ns": 0, "drift_ppm": 1.5,
                                   "source": "gnss-disciplined"})),
            "apps" => Some(json!([{"id": "fcw", "state": "armed", "warnings": 0},
                                  {"id": "eebl", "state": "armed", "warnings": 0}])),
            "detectors" => Some(json!([{"id": "detector/plausibility/position-jump",
                                        "observations": 3, "prov_id": 5}])),
            "neighbors" => Some(Value::Array(
                (0..limit.min(8))
                    .map(|i| {
                        let d = v2xw_core::hash::sha256(&(node + i as u32).to_le_bytes());
                        json!({
                            "digest": v2xw_core::hash::hex_encode(&d[..8]),
                            "verify_state": if i % 4 == 3 { "unverified" } else { "verified" },
                            "last_seen_ns": 0,
                            "distance_m": 35.0 + f64::from(i as u32) * 11.0,
                            "relevance": 0.7,
                            "messages": 10 + i,
                        })
                    })
                    .collect(),
            )),
            "certs" => Some(Value::Array(
                (0..limit.min(4))
                    .map(|i| {
                        let d = v2xw_core::hash::sha256(&(node * 31 + i as u32).to_le_bytes());
                        json!({
                            "cert_id": node * 13 + i as u32,
                            "digest": v2xw_core::hash::hex_encode(&d[..8]),
                            "kind": "pseudonym",
                            "valid_from_ns": 0,
                            "valid_until_ns": 300_000_000_000u64,
                            "i": 0, "j": i,
                        })
                    })
                    .collect(),
            )),
            _ => None,
        }
    }

    fn link_facts(&self, tx: u32, rx: u32, _t_ns: u64, _window_ns: u64) -> Option<Value> {
        let hello = &self.descriptor.hello;
        let a = hello.nodes.iter().find(|r| r.node_id == tx)?;
        let b = hello.nodes.iter().find(|r| r.node_id == rx)?;
        let distance = math::hypot(
            f64::from(a.pos_m[0] - b.pos_m[0]),
            f64::from(a.pos_m[1] - b.pos_m[1]),
        );
        // Free-space-shaped path loss at 5.9 GHz, so the number moves with distance
        // instead of being a constant. It is a fixture value, not a propagation model.
        let path_loss = 32.45
            + 20.0 * math::log10(5_900.0)
            + 20.0 * math::log10((distance / 1_000.0).max(1e-6));
        Some(json!({
            "distance_m": math::quantize(distance, 3),
            "los": {"class": "LOS", "walls_crossed": 0, "obstructed_len_m": 0.0},
            "path_loss_db": math::quantize(path_loss, 3),
            "shadowing_db": 0.0,
            "fading_db": 0.0,
            "rx_power_dbm": math::quantize(20.0 - path_loss, 3),
            "sinr_db": 12.0,
            "pdr": 0.86,
            "pir_p95_s": 0.31,
            "frames": 10,
            "bytes": 3_500,
            "latency_ms": {"p50": 0.5, "p95": 1.4},
        }))
    }

    fn export(&mut self, query: &Query) -> Result<Value> {
        match query {
            Query::ExportDataset {
                exporter,
                out_dir,
                visibility,
            } => Err(ServerError::ExportFailed {
                stage: "open".to_string(),
                detail: format!(
                    "the fixture engine records nothing, so exporter `{exporter}` \
                     (visibility `{visibility}`) has no run to read; out_dir was {:?}. \
                     A real run writes a recording and this succeeds.",
                    out_dir.as_deref().unwrap_or("(default)")
                ),
            }),
            Query::ExportRecording { path, profile } => Err(ServerError::ExportFailed {
                stage: "open".to_string(),
                detail: format!(
                    "no recording to copy from the fixture engine (requested profile \
                     `{profile}`, path {:?})",
                    path.as_deref().unwrap_or("(default)")
                ),
            }),
            _ => Err(ServerError::NotSupportedHere(
                "not an export query".to_string(),
            )),
        }
    }
}

/// The fixture's metric shapes: a smooth function of time so a chart has a line in it.
fn fixture_metric_value(name: &str, phase: f64) -> f64 {
    let w = 0.5 + 0.5 * math::sin(phase);
    match name {
        "pdr" => 0.82 + 0.12 * w,
        "cbr" => 0.18 + 0.30 * w,
        "pir_p95_s" => 0.28 + 0.20 * w,
        "verify_wait_p95_ms" => 1.2 + 3.0 * w,
        "ttc_min" => 2.4 + 1.5 * w,
        _ => f64::NAN,
    }
}

/// The `Provenance` frame the run sends after its first keyframe (§3.8, conformance C5).
///
/// Every `prov_id` a `MetricSample` or an event payload in this fixture references is in
/// it, and it carries `PROV_FINAL` because no more are coming.
fn provenance_body(at: SimTime, hello_strings: &StrTable) -> ProvenanceBody {
    let base = u32::try_from(hello_strings.strings.len()).unwrap_or(0);
    // §2.5: "its own table's first entry takes the id equal to the current table size".
    // `StrTable::new` seeds id 0 with the mandatory empty string, which is right for the
    // table `Hello` establishes and wrong for an extension: an extension that began with
    // `""` would spend the id at `base` on a string the connection already has at id 0,
    // and every id the client computed for the entries after it would still line up — so
    // the mistake is invisible until something resolves `base` itself. Hence an empty
    // `strings`, not `StrTable::new()`.
    let mut ext = StrTable {
        strings: Vec::new(),
    };
    let mut id = |s: &str| base + ext.intern(s);
    let entries = vec![
        ProvEntry {
            prov_id: 1,
            str_model_id: id("net/delivery/pdr-from-phy-rx"),
            str_model_version: id("0.1.0"),
            str_param_set_id: id("b3:stub-fixture"),
            str_card_url: id("/cards/net-delivery-pdr-from-phy-rx"),
            family: 6,
            subject_kind: 4,
        },
        ProvEntry {
            prov_id: 2,
            str_model_id: id("radio/mac/cbr-window"),
            str_model_version: id("0.1.0"),
            str_param_set_id: id("b3:stub-fixture"),
            str_card_url: id("/cards/radio-mac-cbr-window"),
            family: 3,
            subject_kind: 4,
        },
        ProvEntry {
            prov_id: 3,
            str_model_id: id("node/hsm/ecdsa-p256-service-time"),
            str_model_version: id("0.1.0"),
            str_param_set_id: id("b3:stub-fixture"),
            str_card_url: id("/cards/node-hsm-ecdsa-p256-service-time"),
            family: 5,
            subject_kind: 1,
        },
        ProvEntry {
            prov_id: 4,
            str_model_id: id("metrics/safety/ttc-min"),
            str_model_version: id("0.1.0"),
            str_param_set_id: id("b3:stub-fixture"),
            str_card_url: id("/cards/metrics-safety-ttc-min"),
            family: 8,
            subject_kind: 4,
        },
        ProvEntry {
            prov_id: 5,
            str_model_id: id("detector/plausibility/position-jump"),
            str_model_version: id("0.1.0"),
            str_param_set_id: id("b3:stub-fixture"),
            str_card_url: id("/cards/detector-plausibility-position-jump"),
            family: 7,
            subject_kind: 1,
        },
    ];
    ProvenanceBody {
        sim_time_ns: at,
        entries,
        dims: Vec::new(),
        strings: Some(ext),
        flags: PROV_FINAL,
    }
}

/// The model card of `server/fixture/stub-engine`.
///
/// Every default here is `todo-calibrate`, and the calibration plan is the same sentence
/// in each case: this module is a transport fixture, and "calibrating" it means replacing
/// it with `v2xw-engine`. Registering the card anyway is the point — the registry's
/// `todo-calibrate` report then lists the fixture, so a number that came out of it can
/// never be mistaken for a measurement.
pub fn card() -> ModelCard {
    let plan = "not a model: replace this fixture with `v2xw-engine` (build decision D8). \
                Until then every number it reports is a plausible shape with no source, \
                and the calibration step is deletion, not measurement.";
    let todo = |name: &str, unit: &str, default: Value| {
        let mut p = Parameter::new(
            name.to_string(),
            unit.to_string(),
            default,
            Source::todo_calibrate(format!("stub-engine {name}")),
        );
        p.calibration = Some(plan.to_string());
        p
    };
    let mut card = ModelCard::new(
        "server/fixture/stub-engine",
        Family::Mobility,
        env!("CARGO_PKG_VERSION"),
        "A deterministic synthetic run that exercises every VWP v1 frame type, so the \
         transport can be tested without an engine.",
    );
    card.parameters = vec![
        todo("actors", "-", json!(120)),
        todo("grid", "junctions", json!(8)),
        todo("block_m", "m", json!(150.0)),
        todo("speed_base_mps", "m/s", json!(9.5)),
        todo("equipped_fraction", "-", json!(2.0 / 3.0)),
        todo("msgs_in_per_s", "1/s", json!(60.0)),
        todo("verify_service_us", "us", json!(320.0)),
        todo("cbr_pm", "per-mille", json!(120)),
        todo("pdr", "-", json!(0.82)),
    ];
    card.assumptions = vec![
        "Actors travel at a constant speed along a fixed lane loop; there is no \
         car-following, no gap acceptance and no signal compliance."
            .to_string(),
        "Telemetry, events and metric samples are closed-form functions of the step \
         index. They have the right shape and the right units and no physical content."
            .to_string(),
    ];
    card.limitations = vec![
        "Produces no result that may be reported. It exists to give the transport \
         something real to serialise."
            .to_string(),
    ];
    card.sources = vec![Source::new(
        SourceKind::Code,
        "docs/protocol/vwp-v1.md §3 — the layouts this fixture fills",
    )];
    card
}
