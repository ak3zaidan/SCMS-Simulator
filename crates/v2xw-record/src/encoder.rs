//! The snapshot producer: cadence, slots, and the quantisation state machine.
//!
//! This is the half of the byte-identity guarantee that lives *before* the file. §7.2
//! item 1 is determinism of production, and everything here is a pure function of the
//! snapshots it is fed: the sequence numbers, the GOP and step indices, the slot
//! assignment and every quantised integer. Feed the same snapshots twice and the frames
//! are identical, which is what makes "store the bytes" a guarantee rather than a hope.
//!
//! # Cadence is a scenario field
//!
//! [`Cadence`] carries `keyframe_period` and `mobility_step` and is constructed from the
//! scenario, not from a constant (ADR 0008 consequence: "keyframe cadence is a scenario
//! field"). [`Cadence::DEFAULT`] is the 1 s / 100 ms pair `Hello` defaults to (§3.1.1),
//! and nothing else in this crate hard-codes either number.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::ids::{ActorId, LaneId, NodeId, SignalId};
use v2xw_core::time::{Duration, NS_PER_MS, NS_PER_S, SimTime};

use crate::error::{RecordError, Result};
use crate::profile::Profile;
use crate::quant::{self, PoseRef};
use crate::wire::snapshot::{
    AbsRow, ActorRow, DeltaBody, DespawnRow, KeyframeBody, MFLAG_ABSOLUTE, MFLAG_LANE_CHANGED,
    MovedRow, ST_ATTACKER, ST_EQUIPPED, SignalRow, SpawnRow,
};
use crate::wire::{FLAG_RESYNC, Frame, U16_NONE, U32_NONE};

/// The snapshot cadence, a scenario field (§3.1.1, ADR 0008).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    /// How often a full keyframe is emitted, in simulated time.
    pub keyframe_period: Duration,
    /// The mobility step: one delta per step.
    pub mobility_step: Duration,
}

impl Cadence {
    /// The defaults `Hello` carries when a scenario says nothing: 1 s keyframes, 100 ms
    /// steps (§3.1.1).
    pub const DEFAULT: Cadence = Cadence {
        keyframe_period: Duration::from_nanos(NS_PER_S),
        mobility_step: Duration::from_nanos(100 * NS_PER_MS),
    };

    /// A cadence from a scenario.
    ///
    /// # Errors
    /// [`RecordError::Malformed`] if either period is zero or the keyframe period is not
    /// a whole number of mobility steps. The second condition is what makes §7.3's "at
    /// most `keyframe_period_ns / mobility_step_ns` deltas" a bound rather than an
    /// estimate, and what keeps a keyframe landing on a step boundary (§3.3.1).
    pub fn new(keyframe_period: Duration, mobility_step: Duration) -> Result<Self> {
        if keyframe_period.is_zero() || mobility_step.is_zero() {
            return Err(RecordError::malformed(
                "cadence",
                "keyframe_period and mobility_step must both be non-zero",
            ));
        }
        if keyframe_period.as_nanos() % mobility_step.as_nanos() != 0 {
            return Err(RecordError::malformed(
                "cadence",
                format!(
                    "keyframe_period {} ns is not a whole number of {} ns mobility steps",
                    keyframe_period.as_nanos(),
                    mobility_step.as_nanos()
                ),
            ));
        }
        Ok(Cadence {
            keyframe_period,
            mobility_step,
        })
    }

    /// The largest number of deltas a GOP can hold — §7.3's bound, and the one
    /// conformance P4 checks.
    pub fn max_deltas_per_gop(&self) -> u64 {
        self.keyframe_period.as_nanos() / self.mobility_step.as_nanos()
    }
}

impl Default for Cadence {
    fn default() -> Self {
        Cadence::DEFAULT
    }
}

/// One actor's ground-truth pose at a mobility step, as the producer holds it.
///
/// Floats here are the engine's unquantised state; every one of them is quantised on its
/// way into a frame and never reaches a file in this form (D9).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActorPose {
    /// The stable row index for this actor's lifetime (§0.1 "slot").
    pub slot: u32,
    /// The actor.
    pub actor: ActorId,
    /// The node mounted on it, if it is equipped.
    pub node: Option<NodeId>,
    /// Position in world-local ENU metres.
    pub pos_m: [f64; 3],
    /// Heading in radians, ENU, 0 = east, counter-clockwise.
    pub heading_rad: f64,
    /// Speed along the heading, m/s.
    pub speed_mps: f64,
    /// Longitudinal acceleration, m/s². **Ground truth.**
    pub accel_mps2: f64,
    /// The lane, if the actor is on one. **Ground truth.**
    pub lane: Option<LaneId>,
    /// Index into the class table.
    pub class_idx: u8,
    /// The state byte (§3.3.4).
    pub state: u8,
    /// Neighbours in state *verified*, saturating at 255.
    pub verified_neighbors: u8,
}

impl ActorPose {
    /// A minimal equipped pose, for tests and for a producer that fills the rest in.
    pub fn new(slot: u32, actor: ActorId, pos_m: [f64; 3]) -> Self {
        ActorPose {
            slot,
            actor,
            node: None,
            pos_m,
            heading_rad: 0.0,
            speed_mps: 0.0,
            accel_mps2: 0.0,
            lane: None,
            class_idx: 0,
            state: ST_EQUIPPED,
            verified_neighbors: 0,
        }
    }
}

/// One signal head's state at a mobility step (§3.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalState {
    /// The signal controller.
    pub signal: SignalId,
    /// SAE J2735 `MovementPhaseState`, 0–9.
    pub phase: u8,
    /// Time to the next phase change, `None` if unknown.
    pub time_to_change: Option<Duration>,
}

/// Everything the producer needs for one mobility step.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// The step's simulated time; must be a mobility-step boundary.
    pub sim_time: SimTime,
    /// The occupied slots, in any order; the encoder sorts by slot.
    pub actors: Vec<ActorPose>,
    /// The signals whose state the producer knows.
    pub signals: Vec<SignalState>,
    /// Why a newly appearing slot appeared (§3.4.5's `cause`). **Ground truth.**
    pub spawn_causes: BTreeMap<u32, u16>,
    /// Why a disappearing slot disappeared (§3.4.6's `cause`). **Ground truth.**
    pub despawn_causes: BTreeMap<u32, u16>,
}

impl Snapshot {
    /// A snapshot at `sim_time` with these actors and no signals.
    pub fn new(sim_time: SimTime, actors: Vec<ActorPose>) -> Self {
        Snapshot {
            sim_time,
            actors,
            ..Default::default()
        }
    }
}

/// Which frame the encoder produced for a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotFrame {
    /// A full snapshot; opens a GOP.
    Keyframe(Frame),
    /// One step of change within the current GOP.
    Delta(Frame),
}

impl SnapshotFrame {
    /// The frame, whichever kind it is.
    pub fn frame(&self) -> &Frame {
        match self {
            SnapshotFrame::Keyframe(f) | SnapshotFrame::Delta(f) => f,
        }
    }

    /// The frame, consuming the wrapper.
    pub fn into_frame(self) -> Frame {
        match self {
            SnapshotFrame::Keyframe(f) | SnapshotFrame::Delta(f) => f,
        }
    }

    /// True for a keyframe.
    pub fn is_keyframe(&self) -> bool {
        matches!(self, SnapshotFrame::Keyframe(_))
    }
}

/// The per-slot transmitted state the encoder keeps: the pose reference plus the absolute
/// fields whose change triggers a moved row.
#[derive(Debug, Clone, Copy)]
struct SlotRef {
    pose: PoseRef,
}

/// Produces `Keyframe` and `Delta` frames from snapshots (§3.3, §3.4).
#[derive(Debug)]
pub struct SnapshotEncoder {
    origin: [f64; 3],
    cadence: Cadence,
    profile: Profile,
    next_seq: u64,
    gops_emitted: u32,
    step_index: u32,
    last_keyframe_time: Option<SimTime>,
    last_time: Option<SimTime>,
    refs: BTreeMap<u32, SlotRef>,
    signals: BTreeMap<u32, (u8, u16)>,
    force_keyframe: bool,
}

impl SnapshotEncoder {
    /// A producer writing about `origin` (`floor(bbox_min)` per axis, zero for z — §3.3.1)
    /// at `cadence`, in `profile`, starting at seq `first_seq`.
    pub fn new(origin: [f64; 3], cadence: Cadence, profile: Profile, first_seq: u64) -> Self {
        SnapshotEncoder {
            origin,
            cadence,
            profile,
            next_seq: first_seq,
            gops_emitted: 0,
            step_index: 0,
            last_keyframe_time: None,
            last_time: None,
            refs: BTreeMap::new(),
            signals: BTreeMap::new(),
            force_keyframe: false,
        }
    }

    /// The quantisation origin.
    pub fn origin(&self) -> [f64; 3] {
        self.origin
    }

    /// The cadence.
    pub fn cadence(&self) -> Cadence {
        self.cadence
    }

    /// The profile.
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The seq the next frame will carry.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Requests a keyframe at the next step, whatever the cadence says.
    ///
    /// The simulated instant of the last snapshot encoded, if any.
    pub fn last_time(&self) -> Option<SimTime> {
        self.last_time
    }

    /// §1.5's resync path: a keyframe is an idempotent snapshot, so synthesising one is
    /// always legal. It still consumes a `seq` and still carries the canonical body, so a
    /// client recording the stream gets a valid if denser GOP structure.
    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// Encodes one mobility step.
    ///
    /// # Errors
    /// [`RecordError::Malformed`] if time goes backwards, if a slot appears twice in one
    /// snapshot, or if the step is not on a mobility-step boundary relative to the
    /// previous one.
    pub fn encode(&mut self, snap: &Snapshot) -> Result<SnapshotFrame> {
        if let Some(prev) = self.last_time {
            if snap.sim_time <= prev {
                return Err(RecordError::malformed(
                    "snapshot",
                    format!(
                        "sim_time {} does not advance on the previous step's {prev}",
                        snap.sim_time
                    ),
                ));
            }
            let gap = snap.sim_time - prev;
            if gap % self.cadence.mobility_step.as_nanos() != 0 {
                return Err(RecordError::malformed(
                    "snapshot",
                    format!(
                        "sim_time {} is {gap} ns after the previous step, not a whole number of {} ns steps",
                        snap.sim_time,
                        self.cadence.mobility_step.as_nanos()
                    ),
                ));
            }
        }
        let blanked = self.blank_input(snap);
        let rows = self.rows_of(&blanked)?;
        let want_keyframe = self.force_keyframe
            || self.last_keyframe_time.is_none_or(|t| {
                blanked.sim_time.saturating_sub(t) >= self.cadence.keyframe_period.as_nanos()
            });
        let frame = if want_keyframe {
            SnapshotFrame::Keyframe(self.encode_keyframe(&blanked, &rows)?)
        } else {
            SnapshotFrame::Delta(self.encode_delta(&blanked, &rows)?)
        };
        self.last_time = Some(blanked.sim_time);
        self.force_keyframe = false;
        Ok(frame)
    }

    /// §5.3's producer-side blanking: under the `node` profile the ground truth is gone
    /// before quantisation, so the reference state the deltas are computed against is
    /// itself blind.
    fn blank_input(&self, snap: &Snapshot) -> Snapshot {
        if !self.profile.is_node_only() {
            return snap.clone();
        }
        Snapshot {
            sim_time: snap.sim_time,
            actors: snap
                .actors
                .iter()
                .filter(|a| a.state & ST_EQUIPPED != 0)
                .map(|a| ActorPose {
                    lane: None,
                    accel_mps2: 0.0,
                    state: a.state & !ST_ATTACKER,
                    ..*a
                })
                .collect(),
            signals: snap.signals.clone(),
            spawn_causes: BTreeMap::new(),
            despawn_causes: BTreeMap::new(),
        }
    }

    /// The quantised target for every occupied slot, keyed by slot so iteration is
    /// ordered.
    fn rows_of(&self, snap: &Snapshot) -> Result<BTreeMap<u32, Target>> {
        let mut out = BTreeMap::new();
        for a in &snap.actors {
            let target = Target {
                actor_id: a.actor.index(),
                node_id: a.node.map_or(U32_NONE, NodeId::index),
                x_mm: quant::x_mm(a.pos_m[0], self.origin[0]),
                y_mm: quant::x_mm(a.pos_m[1], self.origin[1]),
                z_mm: quant::z_mm(a.pos_m[2], self.origin[2]),
                heading_brad: quant::heading_brad(a.heading_rad),
                speed_cq: quant::speed_cq(a.speed_mps),
                accel_cq: quant::accel_cq(a.accel_mps2),
                lane_id: a.lane.map_or(U32_NONE, LaneId::index),
                class_idx: a.class_idx,
                state: a.state,
                verified_neighbors: a.verified_neighbors,
            };
            if out.insert(a.slot, target).is_some() {
                return Err(RecordError::malformed(
                    "snapshot",
                    format!("slot {} appears twice in one snapshot", a.slot),
                ));
            }
        }
        Ok(out)
    }

    fn encode_keyframe(&mut self, snap: &Snapshot, rows: &BTreeMap<u32, Target>) -> Result<Frame> {
        // §3.3.1: rows are dense and indexed by slot, `actor_count` = high-water + 1.
        let actor_count = rows.keys().next_back().map_or(0, |s| *s as usize + 1);
        let mut actors = vec![ActorRow::EMPTY; actor_count];
        self.refs.clear();
        for (slot, t) in rows {
            let z_cm = quant::escape_z_cm(t.z_mm);
            actors[*slot as usize] = ActorRow {
                actor_id: t.actor_id,
                x_mm: t.x_mm,
                y_mm: t.y_mm,
                lane_id: t.lane_id,
                z_cm,
                heading_brad: t.heading_brad,
                speed_cq: t.speed_cq,
                accel_cq: t.accel_cq,
                class_idx: t.class_idx,
                state: t.state,
                verified_neighbors: t.verified_neighbors,
                flags8: 0,
            };
            self.refs.insert(
                *slot,
                SlotRef {
                    pose: PoseRef {
                        x_mm: t.x_mm,
                        y_mm: t.y_mm,
                        // A keyframe transmits z on the centimetre grid, so that is the
                        // reference the following deltas difference against.
                        z_mm: i64::from(z_cm) * 10,
                        heading_brad: t.heading_brad,
                        speed_cq: t.speed_cq,
                        accel_cq: t.accel_cq,
                        state: t.state,
                        verified_neighbors: t.verified_neighbors,
                        lane_id: t.lane_id,
                    },
                },
            );
        }
        let signals = self.all_signals(snap);
        let body = KeyframeBody {
            sim_time_ns: snap.sim_time,
            origin: self.origin,
            gop_index: self.gops_emitted,
            profile: self.profile.wire_value(),
            actors,
            signals,
        };
        self.gops_emitted += 1;
        self.step_index = 0;
        self.last_keyframe_time = Some(snap.sim_time);
        let seq = self.take_seq();
        // A keyframe re-seeds interpolation state, which is what FLAG_RESYNC means
        // (§2.3). It is a transport flag, so the recorder clears it (§7.2 item 3).
        body.to_frame(seq, self.profile.frame_flag() | FLAG_RESYNC)
    }

    fn encode_delta(&mut self, snap: &Snapshot, rows: &BTreeMap<u32, Target>) -> Result<Frame> {
        let mut moved = Vec::new();
        let mut abs = Vec::new();
        let mut lanes = Vec::new();
        let mut spawns = Vec::new();
        let mut despawns = Vec::new();

        for (slot, t) in rows {
            let Some(r) = self.refs.get(slot).copied() else {
                spawns.push(SpawnRow {
                    slot: *slot,
                    actor_id: t.actor_id,
                    node_id: t.node_id,
                    x_mm: t.x_mm,
                    y_mm: t.y_mm,
                    lane_id: t.lane_id,
                    z_cm: quant::escape_z_cm(t.z_mm),
                    heading_brad: t.heading_brad,
                    speed_cq: t.speed_cq,
                    cause: snap.spawn_causes.get(slot).copied().unwrap_or(U16_NONE),
                    class_idx: t.class_idx,
                    state: t.state,
                    verified_neighbors: t.verified_neighbors,
                });
                let z_cm = quant::escape_z_cm(t.z_mm);
                self.refs.insert(
                    *slot,
                    SlotRef {
                        pose: PoseRef {
                            x_mm: t.x_mm,
                            y_mm: t.y_mm,
                            z_mm: i64::from(z_cm) * 10,
                            heading_brad: t.heading_brad,
                            speed_cq: t.speed_cq,
                            accel_cq: t.accel_cq,
                            state: t.state,
                            verified_neighbors: t.verified_neighbors,
                            lane_id: t.lane_id,
                        },
                    },
                );
                continue;
            };
            let step = r.pose.step(t.x_mm, t.y_mm, t.z_mm);
            let pose_changed = step.absolute || step.d_mm != [0, 0, 0];
            let lane_changed = t.lane_id != r.pose.lane_id;
            // §3.4.2: "Only actors whose quantised pose, `state`, `verified_neighbors` or
            // lane changed appear." Heading, speed and acceleration are absolute fields
            // that ride along on a row; they do not, per the specification, trigger one.
            let changed = pose_changed
                || lane_changed
                || t.state != r.pose.state
                || t.verified_neighbors != r.pose.verified_neighbors;
            let mut next = step.next;
            next.heading_brad = t.heading_brad;
            next.speed_cq = t.speed_cq;
            next.accel_cq = t.accel_cq;
            next.state = t.state;
            next.verified_neighbors = t.verified_neighbors;
            next.lane_id = t.lane_id;
            if !changed {
                self.refs.insert(*slot, SlotRef { pose: next });
                continue;
            }
            let mut mflags = 0u8;
            if step.absolute {
                mflags |= MFLAG_ABSOLUTE;
                abs.push(AbsRow {
                    x_mm: t.x_mm,
                    y_mm: t.y_mm,
                    z_cm: quant::escape_z_cm(t.z_mm),
                });
            }
            if lane_changed {
                mflags |= MFLAG_LANE_CHANGED;
                lanes.push(t.lane_id);
            }
            moved.push(MovedRow {
                slot: *slot,
                dx_mm: step.d_mm[0],
                dy_mm: step.d_mm[1],
                dz_mm: step.d_mm[2],
                heading_brad: t.heading_brad,
                speed_cq: t.speed_cq,
                accel_cq: t.accel_cq,
                state: t.state,
                verified_neighbors: t.verified_neighbors,
                mflags,
            });
            self.refs.insert(*slot, SlotRef { pose: next });
        }

        let gone: Vec<u32> = self
            .refs
            .keys()
            .copied()
            .filter(|s| !rows.contains_key(s))
            .collect();
        for slot in gone {
            self.refs.remove(&slot);
            despawns.push(DespawnRow {
                slot,
                cause: snap.despawn_causes.get(&slot).copied().unwrap_or(U16_NONE),
            });
        }

        let signals = self.changed_signals(snap);
        self.step_index += 1;
        let body = DeltaBody {
            sim_time_ns: snap.sim_time,
            gop_index: self.gops_emitted.saturating_sub(1),
            step_index: self.step_index,
            moved,
            abs,
            lanes,
            spawns,
            despawns,
            signals,
        };
        let seq = self.take_seq();
        body.to_frame(seq, self.profile.frame_flag())
    }

    fn all_signals(&mut self, snap: &Snapshot) -> Vec<SignalRow> {
        let mut out = Vec::with_capacity(snap.signals.len());
        let mut seen: BTreeMap<u32, (u8, u16)> = BTreeMap::new();
        for s in &snap.signals {
            let ds = quant::time_to_change_ds(s.time_to_change);
            seen.insert(s.signal.index(), (s.phase, ds));
        }
        for (id, (phase, ds)) in &seen {
            out.push(SignalRow {
                signal_id: *id,
                time_to_change_ds: *ds,
                phase: *phase,
            });
        }
        self.signals = seen;
        out
    }

    fn changed_signals(&mut self, snap: &Snapshot) -> Vec<SignalRow> {
        let mut out = Vec::new();
        for s in &snap.signals {
            let id = s.signal.index();
            let ds = quant::time_to_change_ds(s.time_to_change);
            let now = (s.phase, ds);
            if self.signals.get(&id) != Some(&now) {
                self.signals.insert(id, now);
                out.push(SignalRow {
                    signal_id: id,
                    time_to_change_ds: ds,
                    phase: s.phase,
                });
            }
        }
        out.sort_by_key(|r| r.signal_id);
        out
    }

    fn take_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }
}

#[derive(Debug, Clone, Copy)]
struct Target {
    actor_id: u32,
    node_id: u32,
    x_mm: i32,
    y_mm: i32,
    z_mm: i64,
    heading_brad: u16,
    speed_cq: i16,
    accel_cq: i16,
    lane_id: u32,
    class_idx: u8,
    state: u8,
    verified_neighbors: u8,
}

/// Assigns actor slots the way §3.3.1 requires: the lowest free slot, and no reuse until
/// one full keyframe period after a despawn (conformance Q4, Q5).
///
/// The delay is the reason the rule exists: a delta that arrives after its actor's
/// despawn — because it was queued, or because a client resumed into the middle of a GOP
/// — would otherwise be applied to whichever actor inherited the slot.
#[derive(Debug)]
pub struct SlotAllocator {
    keyframe_period: Duration,
    by_actor: BTreeMap<ActorId, u32>,
    occupied: BTreeSet<u32>,
    cooling: BTreeMap<u32, SimTime>,
}

impl SlotAllocator {
    /// An allocator for a run at this keyframe period.
    pub fn new(keyframe_period: Duration) -> Self {
        SlotAllocator {
            keyframe_period,
            by_actor: BTreeMap::new(),
            occupied: BTreeSet::new(),
            cooling: BTreeMap::new(),
        }
    }

    /// The slot this actor holds, if any.
    pub fn slot_of(&self, actor: ActorId) -> Option<u32> {
        self.by_actor.get(&actor).copied()
    }

    /// The lowest slot that is neither occupied nor cooling off, assigned to `actor`.
    ///
    /// Idempotent: an actor that already holds a slot keeps it.
    pub fn allocate(&mut self, actor: ActorId, now: SimTime) -> u32 {
        if let Some(s) = self.slot_of(actor) {
            return s;
        }
        self.cooling
            .retain(|_, freed| now.saturating_sub(*freed) < self.keyframe_period.as_nanos());
        let mut slot = 0u32;
        while self.occupied.contains(&slot) || self.cooling.contains_key(&slot) {
            slot += 1;
        }
        self.occupied.insert(slot);
        self.by_actor.insert(actor, slot);
        slot
    }

    /// Releases the actor's slot into its cooling-off period.
    pub fn release(&mut self, actor: ActorId, now: SimTime) -> Option<u32> {
        let slot = self.by_actor.remove(&actor)?;
        self.occupied.remove(&slot);
        self.cooling.insert(slot, now);
        Some(slot)
    }

    /// The occupied slots, ascending.
    pub fn occupied(&self) -> impl Iterator<Item = u32> + '_ {
        self.occupied.iter().copied()
    }
}
