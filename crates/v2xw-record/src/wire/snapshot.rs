//! `Keyframe` (§3.3) and `Delta` (§3.4) — the two snapshot frames.
//!
//! Both are struct-of-arrays: every column is a contiguous array of the same length, so a
//! client can build typed-array views over the frame with no copy and no parsing (§3.0
//! reason 1). The encoders here lay the columns out in the order the specification's
//! tables list them and nothing else; the decoders read them back for verification, for
//! the `NODE-only` transformation and for tests.

use crate::error::{RecordError, Result};
use crate::wire::{
    Frame, MsgType, U32_NONE, ceil4, get_f64, get_i16, get_i32, get_u8, get_u16, get_u32, get_u64,
    put_f64, put_i16, put_i32, put_u8, put_u16, put_u32, put_u64,
};
use v2xw_core::time::SimTime;

/// The `Keyframe` prefix is 64 bytes (§3.3.1).
pub const KEYFRAME_PREFIX_BYTES: usize = 64;
/// Bytes per keyframe actor row (§3.3.2).
pub const ACTOR_STRIDE: usize = 28;
/// Bytes per signal row (§3.3.3), shared by both frames.
pub const SIGNAL_STRIDE: usize = 8;

/// The `Delta` prefix is 64 bytes (§3.4.1).
pub const DELTA_PREFIX_BYTES: usize = 64;
/// Bytes per moved row (§3.4.2).
pub const MOVED_STRIDE: usize = 20;
/// Bytes per absolute-escape entry (§3.4.3).
pub const ABS_STRIDE: usize = 12;
/// Bytes per lane-block entry (§3.4.4).
pub const LANE_STRIDE: usize = 4;
/// Bytes per spawn row (§3.4.5).
pub const SPAWN_STRIDE: usize = 36;
/// Bytes per despawn row (§3.4.6).
pub const DESPAWN_STRIDE: usize = 8;

/// `state` bit 0 — the actor is running an `Attacker` plug-in. **Ground truth** (§3.3.4).
pub const ST_ATTACKER: u8 = 0x01;
/// `state` bit 1 — a misbehaviour report naming this actor reached the MA.
pub const ST_REPORTED: u8 = 0x02;
/// `state` bit 2 — on a published CRL or blocklist.
pub const ST_REVOKED: u8 = 0x04;
/// `state` bit 3 — carries an OBU or a VRU device.
pub const ST_EQUIPPED: u8 = 0x08;
/// `state` bit 4 — transmitted at least once in the last mobility step.
pub const ST_TRANSMITTING: u8 = 0x10;
/// `state` bit 5 — parked, radio off.
pub const ST_PARKED: u8 = 0x20;
/// `state` bit 6 — GNSS fix worse than 3D, or an active outage.
pub const ST_GNSS_DEGRADED: u8 = 0x40;
/// `state` bit 7 — a safety application on this node is warning.
pub const ST_WARNING_ACTIVE: u8 = 0x80;

/// `mflags` bit 0 — ignore `dx/dy/dz`, this row has an absolute-block entry (§3.4.2.1).
pub const MFLAG_ABSOLUTE: u8 = 0x01;
/// `mflags` bit 1 — this row has a lane-block entry (§3.4.2.1).
pub const MFLAG_LANE_CHANGED: u8 = 0x02;

/// `Keyframe.profile` — the full stream (§3.3.1).
pub const PROFILE_FULL: u16 = 0;
/// `Keyframe.profile` — the `node` profile (§3.3.1, §5).
pub const PROFILE_NODE: u16 = 1;

/// One keyframe actor row (§3.3.2). Rows are indexed **by slot**, not by actor id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorRow {
    /// `ActorId`; `0xFFFFFFFF` marks an empty slot.
    pub actor_id: u32,
    /// Millimetres east of `origin_x_m`.
    pub x_mm: i32,
    /// Millimetres north of `origin_y_m`.
    pub y_mm: i32,
    /// `LaneId`, `0xFFFFFFFF` off-lane or withheld. **Ground truth.**
    pub lane_id: u32,
    /// Centimetres up from `origin_z_m`.
    pub z_cm: i16,
    /// Heading in binary radians, ENU, 0 = east, counter-clockwise.
    pub heading_brad: u16,
    /// Speed in 1/128 m/s along the heading.
    pub speed_cq: i16,
    /// Longitudinal acceleration in 1/64 m/s². **Ground truth.**
    pub accel_cq: i16,
    /// Index into the class table.
    pub class_idx: u8,
    /// The state byte (§3.3.4).
    pub state: u8,
    /// Neighbours in state *verified*, saturating at 255.
    pub verified_neighbors: u8,
    /// Reserved, written as zero.
    pub flags8: u8,
}

impl ActorRow {
    /// The row an unoccupied slot carries: `actor_id = 0xFFFFFFFF` and zeros elsewhere
    /// (§3.3.1 `DECISION`).
    pub const EMPTY: ActorRow = ActorRow {
        actor_id: U32_NONE,
        x_mm: 0,
        y_mm: 0,
        lane_id: 0,
        z_cm: 0,
        heading_brad: 0,
        speed_cq: 0,
        accel_cq: 0,
        class_idx: 0,
        state: 0,
        verified_neighbors: 0,
        flags8: 0,
    };

    /// True if the slot is occupied.
    pub const fn is_occupied(&self) -> bool {
        self.actor_id != U32_NONE
    }
}

/// One signal row (§3.3.3), shared by `Keyframe` and `Delta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalRow {
    /// `SignalId`.
    pub signal_id: u32,
    /// Deciseconds to the next phase change, `0xFFFF` unknown.
    pub time_to_change_ds: u16,
    /// SAE J2735 `MovementPhaseState`, 0–9.
    pub phase: u8,
}

/// A decoded `Keyframe` body (§3.3).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframeBody {
    /// A mobility-step boundary.
    pub sim_time_ns: SimTime,
    /// The quantisation origin, `floor(bbox_min)` per axis, constant for the run.
    pub origin: [f64; 3],
    /// The keyframe ordinal from 0; deltas quote it.
    pub gop_index: u32,
    /// `0` full, `1` node-only.
    pub profile: u16,
    /// Actor rows, dense and indexed by slot.
    pub actors: Vec<ActorRow>,
    /// Signal rows.
    pub signals: Vec<SignalRow>,
}

impl KeyframeBody {
    /// The encoded body length in bytes: `64 + 28A + 8S` (Appendix B).
    pub fn encoded_len(&self) -> usize {
        KEYFRAME_PREFIX_BYTES
            + ACTOR_STRIDE * self.actors.len()
            + SIGNAL_STRIDE * self.signals.len()
    }

    /// Encodes the body (§3.3).
    pub fn encode(&self) -> Vec<u8> {
        let a = self.actors.len();
        let s = self.signals.len();
        let mut out = vec![0u8; self.encoded_len()];
        let off_actors = if a > 0 { KEYFRAME_PREFIX_BYTES } else { 0 };
        let off_signals = if s > 0 {
            KEYFRAME_PREFIX_BYTES + ACTOR_STRIDE * a
        } else {
            0
        };
        put_u64(&mut out, 0, self.sim_time_ns);
        put_f64(&mut out, 8, self.origin[0]);
        put_f64(&mut out, 16, self.origin[1]);
        put_f64(&mut out, 24, self.origin[2]);
        put_u32(&mut out, 32, a as u32);
        put_u32(&mut out, 36, s as u32);
        put_u32(&mut out, 40, off_actors as u32);
        put_u32(&mut out, 44, off_signals as u32);
        put_u32(&mut out, 48, self.gop_index);
        put_u16(&mut out, 52, self.profile);
        // 54..64 stay zero: `reserved` and `reserved64` (§8.5 keeps them for growth).

        let mut p = KEYFRAME_PREFIX_BYTES;
        for r in &self.actors {
            put_u32(&mut out, p, r.actor_id);
            p += 4;
        }
        for r in &self.actors {
            put_i32(&mut out, p, r.x_mm);
            p += 4;
        }
        for r in &self.actors {
            put_i32(&mut out, p, r.y_mm);
            p += 4;
        }
        for r in &self.actors {
            put_u32(&mut out, p, r.lane_id);
            p += 4;
        }
        for r in &self.actors {
            put_i16(&mut out, p, r.z_cm);
            p += 2;
        }
        for r in &self.actors {
            put_u16(&mut out, p, r.heading_brad);
            p += 2;
        }
        for r in &self.actors {
            put_i16(&mut out, p, r.speed_cq);
            p += 2;
        }
        for r in &self.actors {
            put_i16(&mut out, p, r.accel_cq);
            p += 2;
        }
        for r in &self.actors {
            put_u8(&mut out, p, r.class_idx);
            p += 1;
        }
        for r in &self.actors {
            put_u8(&mut out, p, r.state);
            p += 1;
        }
        for r in &self.actors {
            put_u8(&mut out, p, r.verified_neighbors);
            p += 1;
        }
        for r in &self.actors {
            put_u8(&mut out, p, r.flags8);
            p += 1;
        }
        encode_signals(&mut out, p, &self.signals);
        out
    }

    /// The whole frame: header plus body (§2.1).
    ///
    /// # Errors
    /// [`RecordError::Unrepresentable`] for a body larger than `u32::MAX`.
    pub fn to_frame(&self, seq: u64, flags: u16) -> Result<Frame> {
        Frame::new(MsgType::Keyframe, seq, flags, &self.encode())
    }

    /// Decodes a body (§3.3).
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if a column runs off the end, or
    /// [`RecordError::Malformed`] if a section offset contradicts its count.
    pub fn decode(body: &[u8]) -> Result<Self> {
        const WHAT: &str = "vwp Keyframe";
        let sim_time_ns = get_u64(body, 0, WHAT)?;
        let origin = [
            get_f64(body, 8, WHAT)?,
            get_f64(body, 16, WHAT)?,
            get_f64(body, 24, WHAT)?,
        ];
        let a = get_u32(body, 32, WHAT)? as usize;
        let s = get_u32(body, 36, WHAT)? as usize;
        let off_actors = get_u32(body, 40, WHAT)? as usize;
        let off_signals = get_u32(body, 44, WHAT)? as usize;
        let gop_index = get_u32(body, 48, WHAT)?;
        let profile = get_u16(body, 52, WHAT)?;
        check_section(WHAT, "actors", a, off_actors, ACTOR_STRIDE, body.len())?;
        check_section(WHAT, "signals", s, off_signals, SIGNAL_STRIDE, body.len())?;

        let mut actors = vec![ActorRow::EMPTY; a];
        let mut p = off_actors;
        for r in actors.iter_mut() {
            r.actor_id = get_u32(body, p, WHAT)?;
            p += 4;
        }
        for r in actors.iter_mut() {
            r.x_mm = get_i32(body, p, WHAT)?;
            p += 4;
        }
        for r in actors.iter_mut() {
            r.y_mm = get_i32(body, p, WHAT)?;
            p += 4;
        }
        for r in actors.iter_mut() {
            r.lane_id = get_u32(body, p, WHAT)?;
            p += 4;
        }
        for r in actors.iter_mut() {
            r.z_cm = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in actors.iter_mut() {
            r.heading_brad = get_u16(body, p, WHAT)?;
            p += 2;
        }
        for r in actors.iter_mut() {
            r.speed_cq = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in actors.iter_mut() {
            r.accel_cq = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in actors.iter_mut() {
            r.class_idx = get_u8(body, p, WHAT)?;
            p += 1;
        }
        for r in actors.iter_mut() {
            r.state = get_u8(body, p, WHAT)?;
            p += 1;
        }
        for r in actors.iter_mut() {
            r.verified_neighbors = get_u8(body, p, WHAT)?;
            p += 1;
        }
        for r in actors.iter_mut() {
            r.flags8 = get_u8(body, p, WHAT)?;
            p += 1;
        }
        let signals = decode_signals(body, off_signals, s, WHAT)?;
        Ok(KeyframeBody {
            sim_time_ns,
            origin,
            gop_index,
            profile,
            actors,
            signals,
        })
    }
}

/// One moved row of a `Delta` (§3.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovedRow {
    /// The slot, strictly ascending across the block.
    pub slot: u32,
    /// Change in millimetres since the previous frame of the GOP.
    pub dx_mm: i16,
    /// Change in millimetres since the previous frame of the GOP.
    pub dy_mm: i16,
    /// Change in millimetres since the previous frame of the GOP.
    pub dz_mm: i16,
    /// Absolute heading in binary radians.
    pub heading_brad: u16,
    /// Absolute speed in 1/128 m/s.
    pub speed_cq: i16,
    /// Absolute acceleration in 1/64 m/s². **Ground truth.**
    pub accel_cq: i16,
    /// Absolute state byte.
    pub state: u8,
    /// Absolute verified-neighbour count.
    pub verified_neighbors: u8,
    /// Row flags (§3.4.2.1).
    pub mflags: u8,
}

/// One absolute-escape entry (§3.4.3), in the order of the moved rows that set
/// [`MFLAG_ABSOLUTE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsRow {
    /// Millimetres east of the GOP keyframe's origin.
    pub x_mm: i32,
    /// Millimetres north of the GOP keyframe's origin.
    pub y_mm: i32,
    /// Centimetres up from the GOP keyframe's origin.
    pub z_cm: i16,
}

/// One spawn row (§3.4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnRow {
    /// The newly occupied slot.
    pub slot: u32,
    /// The actor.
    pub actor_id: u32,
    /// The node mounted on it, `0xFFFFFFFF` if unequipped.
    pub node_id: u32,
    /// Absolute x in millimetres from the GOP keyframe's origin.
    pub x_mm: i32,
    /// Absolute y in millimetres from the GOP keyframe's origin.
    pub y_mm: i32,
    /// Lane. **Ground truth.**
    pub lane_id: u32,
    /// Absolute z in centimetres.
    pub z_cm: i16,
    /// Heading in binary radians.
    pub heading_brad: u16,
    /// Speed in 1/128 m/s.
    pub speed_cq: i16,
    /// Why the actor appeared. **Ground truth.**
    pub cause: u16,
    /// Class table index.
    pub class_idx: u8,
    /// State byte.
    pub state: u8,
    /// Verified-neighbour count.
    pub verified_neighbors: u8,
}

/// One despawn row (§3.4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DespawnRow {
    /// The slot being released.
    pub slot: u32,
    /// Why the actor left. **Ground truth.**
    pub cause: u16,
}

/// A decoded `Delta` body (§3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaBody {
    /// The mobility step this delta lands on.
    pub sim_time_ns: SimTime,
    /// The GOP this delta belongs to; must equal the current keyframe's.
    pub gop_index: u32,
    /// 1-based index within the GOP.
    pub step_index: u32,
    /// Rows whose quantised pose, state, neighbour count or lane changed.
    pub moved: Vec<MovedRow>,
    /// Absolute-escape entries, in moved-row order.
    pub abs: Vec<AbsRow>,
    /// Lane ids, in moved-row order. **Ground truth**; absent under the `node` profile.
    pub lanes: Vec<u32>,
    /// Newly occupied slots.
    pub spawns: Vec<SpawnRow>,
    /// Released slots.
    pub despawns: Vec<DespawnRow>,
    /// Signals whose phase or countdown changed.
    pub signals: Vec<SignalRow>,
}

impl DeltaBody {
    /// The encoded body length in bytes (Appendix B).
    pub fn encoded_len(&self) -> usize {
        DELTA_PREFIX_BYTES
            + MOVED_STRIDE * self.moved.len()
            + ABS_STRIDE * self.abs.len()
            + LANE_STRIDE * self.lanes.len()
            + SPAWN_STRIDE * self.spawns.len()
            + DESPAWN_STRIDE * self.despawns.len()
            + SIGNAL_STRIDE * self.signals.len()
    }

    /// Encodes the body (§3.4).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.encoded_len()];
        let mut at = DELTA_PREFIX_BYTES;
        let off = |at: &mut usize, n: usize, stride: usize| -> u32 {
            let o = if n > 0 { *at as u32 } else { 0 };
            *at += stride * n;
            o
        };
        let off_moved = off(&mut at, self.moved.len(), MOVED_STRIDE);
        let off_abs = off(&mut at, self.abs.len(), ABS_STRIDE);
        let off_lanes = off(&mut at, self.lanes.len(), LANE_STRIDE);
        let off_spawns = off(&mut at, self.spawns.len(), SPAWN_STRIDE);
        let off_despawns = off(&mut at, self.despawns.len(), DESPAWN_STRIDE);
        let off_signals = off(&mut at, self.signals.len(), SIGNAL_STRIDE);

        put_u64(&mut out, 0, self.sim_time_ns);
        put_u32(&mut out, 8, self.gop_index);
        put_u32(&mut out, 12, self.step_index);
        put_u32(&mut out, 16, self.moved.len() as u32);
        put_u32(&mut out, 20, self.abs.len() as u32);
        put_u32(&mut out, 24, self.lanes.len() as u32);
        put_u32(&mut out, 28, self.spawns.len() as u32);
        put_u32(&mut out, 32, self.despawns.len() as u32);
        put_u32(&mut out, 36, self.signals.len() as u32);
        put_u32(&mut out, 40, off_moved);
        put_u32(&mut out, 44, off_abs);
        put_u32(&mut out, 48, off_lanes);
        put_u32(&mut out, 52, off_spawns);
        put_u32(&mut out, 56, off_despawns);
        put_u32(&mut out, 60, off_signals);

        let mut p = DELTA_PREFIX_BYTES;
        for r in &self.moved {
            put_u32(&mut out, p, r.slot);
            p += 4;
        }
        for r in &self.moved {
            put_i16(&mut out, p, r.dx_mm);
            p += 2;
        }
        for r in &self.moved {
            put_i16(&mut out, p, r.dy_mm);
            p += 2;
        }
        for r in &self.moved {
            put_i16(&mut out, p, r.dz_mm);
            p += 2;
        }
        for r in &self.moved {
            put_u16(&mut out, p, r.heading_brad);
            p += 2;
        }
        for r in &self.moved {
            put_i16(&mut out, p, r.speed_cq);
            p += 2;
        }
        for r in &self.moved {
            put_i16(&mut out, p, r.accel_cq);
            p += 2;
        }
        for r in &self.moved {
            put_u8(&mut out, p, r.state);
            p += 1;
        }
        for r in &self.moved {
            put_u8(&mut out, p, r.verified_neighbors);
            p += 1;
        }
        for r in &self.moved {
            put_u8(&mut out, p, r.mflags);
            p += 1;
        }
        p += self.moved.len(); // the per-row `reserved` byte, already zero

        for r in &self.abs {
            put_i32(&mut out, p, r.x_mm);
            put_i32(&mut out, p + 4, r.y_mm);
            put_i16(&mut out, p + 8, r.z_cm);
            p += ABS_STRIDE;
        }
        for lane in &self.lanes {
            put_u32(&mut out, p, *lane);
            p += 4;
        }
        for r in &self.spawns {
            put_u32(&mut out, p, r.slot);
            p += 4;
        }
        for r in &self.spawns {
            put_u32(&mut out, p, r.actor_id);
            p += 4;
        }
        for r in &self.spawns {
            put_u32(&mut out, p, r.node_id);
            p += 4;
        }
        for r in &self.spawns {
            put_i32(&mut out, p, r.x_mm);
            p += 4;
        }
        for r in &self.spawns {
            put_i32(&mut out, p, r.y_mm);
            p += 4;
        }
        for r in &self.spawns {
            put_u32(&mut out, p, r.lane_id);
            p += 4;
        }
        for r in &self.spawns {
            put_i16(&mut out, p, r.z_cm);
            p += 2;
        }
        for r in &self.spawns {
            put_u16(&mut out, p, r.heading_brad);
            p += 2;
        }
        for r in &self.spawns {
            put_i16(&mut out, p, r.speed_cq);
            p += 2;
        }
        for r in &self.spawns {
            put_u16(&mut out, p, r.cause);
            p += 2;
        }
        for r in &self.spawns {
            put_u8(&mut out, p, r.class_idx);
            p += 1;
        }
        for r in &self.spawns {
            put_u8(&mut out, p, r.state);
            p += 1;
        }
        for r in &self.spawns {
            put_u8(&mut out, p, r.verified_neighbors);
            p += 1;
        }
        p += self.spawns.len(); // per-row `reserved`

        for r in &self.despawns {
            put_u32(&mut out, p, r.slot);
            p += 4;
        }
        for r in &self.despawns {
            put_u16(&mut out, p, r.cause);
            p += 2;
        }
        p += 2 * self.despawns.len(); // per-row `reserved`

        encode_signals(&mut out, p, &self.signals);
        out
    }

    /// The whole frame: header plus body.
    ///
    /// # Errors
    /// [`RecordError::Unrepresentable`] for a body larger than `u32::MAX`.
    pub fn to_frame(&self, seq: u64, flags: u16) -> Result<Frame> {
        Frame::new(MsgType::Delta, seq, flags, &self.encode())
    }

    /// Decodes a body (§3.4).
    ///
    /// # Errors
    /// [`RecordError::Truncated`] or [`RecordError::Malformed`] as for
    /// [`KeyframeBody::decode`], plus a check that the absolute and lane blocks are
    /// exactly as long as the moved rows' flags claim.
    pub fn decode(body: &[u8]) -> Result<Self> {
        const WHAT: &str = "vwp Delta";
        let sim_time_ns = get_u64(body, 0, WHAT)?;
        let gop_index = get_u32(body, 8, WHAT)?;
        let step_index = get_u32(body, 12, WHAT)?;
        let m = get_u32(body, 16, WHAT)? as usize;
        let abs_count = get_u32(body, 20, WHAT)? as usize;
        let lane_count = get_u32(body, 24, WHAT)? as usize;
        let spawn_count = get_u32(body, 28, WHAT)? as usize;
        let despawn_count = get_u32(body, 32, WHAT)? as usize;
        let signal_count = get_u32(body, 36, WHAT)? as usize;
        let off_moved = get_u32(body, 40, WHAT)? as usize;
        let off_abs = get_u32(body, 44, WHAT)? as usize;
        let off_lanes = get_u32(body, 48, WHAT)? as usize;
        let off_spawns = get_u32(body, 52, WHAT)? as usize;
        let off_despawns = get_u32(body, 56, WHAT)? as usize;
        let off_signals = get_u32(body, 60, WHAT)? as usize;
        check_section(WHAT, "moved", m, off_moved, MOVED_STRIDE, body.len())?;
        check_section(WHAT, "abs", abs_count, off_abs, ABS_STRIDE, body.len())?;
        check_section(
            WHAT,
            "lanes",
            lane_count,
            off_lanes,
            LANE_STRIDE,
            body.len(),
        )?;
        check_section(
            WHAT,
            "spawns",
            spawn_count,
            off_spawns,
            SPAWN_STRIDE,
            body.len(),
        )?;
        check_section(
            WHAT,
            "despawns",
            despawn_count,
            off_despawns,
            DESPAWN_STRIDE,
            body.len(),
        )?;
        check_section(
            WHAT,
            "signals",
            signal_count,
            off_signals,
            SIGNAL_STRIDE,
            body.len(),
        )?;

        let mut moved = vec![
            MovedRow {
                slot: 0,
                dx_mm: 0,
                dy_mm: 0,
                dz_mm: 0,
                heading_brad: 0,
                speed_cq: 0,
                accel_cq: 0,
                state: 0,
                verified_neighbors: 0,
                mflags: 0,
            };
            m
        ];
        let mut p = off_moved;
        for r in moved.iter_mut() {
            r.slot = get_u32(body, p, WHAT)?;
            p += 4;
        }
        for r in moved.iter_mut() {
            r.dx_mm = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in moved.iter_mut() {
            r.dy_mm = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in moved.iter_mut() {
            r.dz_mm = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in moved.iter_mut() {
            r.heading_brad = get_u16(body, p, WHAT)?;
            p += 2;
        }
        for r in moved.iter_mut() {
            r.speed_cq = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in moved.iter_mut() {
            r.accel_cq = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in moved.iter_mut() {
            r.state = get_u8(body, p, WHAT)?;
            p += 1;
        }
        for r in moved.iter_mut() {
            r.verified_neighbors = get_u8(body, p, WHAT)?;
            p += 1;
        }
        for r in moved.iter_mut() {
            r.mflags = get_u8(body, p, WHAT)?;
            p += 1;
        }

        let mut abs = Vec::with_capacity(abs_count);
        for i in 0..abs_count {
            let at = off_abs + i * ABS_STRIDE;
            abs.push(AbsRow {
                x_mm: get_i32(body, at, WHAT)?,
                y_mm: get_i32(body, at + 4, WHAT)?,
                z_cm: get_i16(body, at + 8, WHAT)?,
            });
        }
        let mut lanes = Vec::with_capacity(lane_count);
        for i in 0..lane_count {
            lanes.push(get_u32(body, off_lanes + 4 * i, WHAT)?);
        }

        let flagged_abs = moved
            .iter()
            .filter(|r| r.mflags & MFLAG_ABSOLUTE != 0)
            .count();
        if flagged_abs != abs_count {
            return Err(RecordError::malformed(
                WHAT,
                format!(
                    "{flagged_abs} rows set MFLAG_ABSOLUTE but the absolute block has {abs_count} entries"
                ),
            ));
        }
        let flagged_lane = moved
            .iter()
            .filter(|r| r.mflags & MFLAG_LANE_CHANGED != 0)
            .count();
        if flagged_lane != lane_count {
            return Err(RecordError::malformed(
                WHAT,
                format!(
                    "{flagged_lane} rows set MFLAG_LANE_CHANGED but the lane block has {lane_count} entries"
                ),
            ));
        }

        let mut spawns = vec![
            SpawnRow {
                slot: 0,
                actor_id: 0,
                node_id: U32_NONE,
                x_mm: 0,
                y_mm: 0,
                lane_id: U32_NONE,
                z_cm: 0,
                heading_brad: 0,
                speed_cq: 0,
                cause: 0,
                class_idx: 0,
                state: 0,
                verified_neighbors: 0,
            };
            spawn_count
        ];
        let mut p = off_spawns;
        for r in spawns.iter_mut() {
            r.slot = get_u32(body, p, WHAT)?;
            p += 4;
        }
        for r in spawns.iter_mut() {
            r.actor_id = get_u32(body, p, WHAT)?;
            p += 4;
        }
        for r in spawns.iter_mut() {
            r.node_id = get_u32(body, p, WHAT)?;
            p += 4;
        }
        for r in spawns.iter_mut() {
            r.x_mm = get_i32(body, p, WHAT)?;
            p += 4;
        }
        for r in spawns.iter_mut() {
            r.y_mm = get_i32(body, p, WHAT)?;
            p += 4;
        }
        for r in spawns.iter_mut() {
            r.lane_id = get_u32(body, p, WHAT)?;
            p += 4;
        }
        for r in spawns.iter_mut() {
            r.z_cm = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in spawns.iter_mut() {
            r.heading_brad = get_u16(body, p, WHAT)?;
            p += 2;
        }
        for r in spawns.iter_mut() {
            r.speed_cq = get_i16(body, p, WHAT)?;
            p += 2;
        }
        for r in spawns.iter_mut() {
            r.cause = get_u16(body, p, WHAT)?;
            p += 2;
        }
        for r in spawns.iter_mut() {
            r.class_idx = get_u8(body, p, WHAT)?;
            p += 1;
        }
        for r in spawns.iter_mut() {
            r.state = get_u8(body, p, WHAT)?;
            p += 1;
        }
        for r in spawns.iter_mut() {
            r.verified_neighbors = get_u8(body, p, WHAT)?;
            p += 1;
        }

        let mut despawns = Vec::with_capacity(despawn_count);
        for i in 0..despawn_count {
            despawns.push(DespawnRow {
                slot: get_u32(body, off_despawns + 4 * i, WHAT)?,
                cause: get_u16(body, off_despawns + 4 * despawn_count + 2 * i, WHAT)?,
            });
        }
        let signals = decode_signals(body, off_signals, signal_count, WHAT)?;

        Ok(DeltaBody {
            sim_time_ns,
            gop_index,
            step_index,
            moved,
            abs,
            lanes,
            spawns,
            despawns,
            signals,
        })
    }
}

fn encode_signals(out: &mut [u8], at: usize, signals: &[SignalRow]) {
    let mut p = at;
    for r in signals {
        put_u32(out, p, r.signal_id);
        p += 4;
    }
    for r in signals {
        put_u16(out, p, r.time_to_change_ds);
        p += 2;
    }
    for r in signals {
        put_u8(out, p, r.phase);
        p += 1;
    }
    // The per-row `reserved` byte is already zero.
}

fn decode_signals(body: &[u8], at: usize, n: usize, what: &'static str) -> Result<Vec<SignalRow>> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(SignalRow {
            signal_id: get_u32(body, at + 4 * i, what)?,
            time_to_change_ds: get_u16(body, at + 4 * n + 2 * i, what)?,
            phase: get_u8(body, at + 6 * n + i, what)?,
        });
    }
    Ok(out)
}

/// §2.2's rule that `0` means "section absent", checked against the count.
fn check_section(
    what: &'static str,
    name: &str,
    count: usize,
    off: usize,
    stride: usize,
    body_len: usize,
) -> Result<()> {
    if count == 0 {
        if off != 0 {
            return Err(RecordError::malformed(
                what,
                format!("{name} is empty but off_{name} = {off}, which must be 0 (§2.2)"),
            ));
        }
        return Ok(());
    }
    if off == 0 {
        return Err(RecordError::malformed(
            what,
            format!("{name} has {count} rows but off_{name} = 0, which means absent (§2.2)"),
        ));
    }
    let want = stride.saturating_mul(count);
    let end = off.saturating_add(want);
    if end > body_len {
        return Err(RecordError::Truncated {
            what,
            at: off,
            // The same product, saturated. Writing `stride * count` a second time here
            // would have been an unchecked multiplication of a wire count reached only on
            // the malformed path — a panic in debug and a nonsense number in the error
            // message in release, on exactly the input this branch exists to report.
            need: want,
            have: body_len.saturating_sub(off),
        });
    }
    Ok(())
}

/// The alignment rule of §2.2, as a predicate the tests use: every scalar array starts at
/// a body-relative offset that is a multiple of its element size.
pub const fn is_aligned(off: usize, element_size: usize) -> bool {
    off % element_size == 0
}

/// Padding to the next 4-byte boundary, kept public because the section formulas use it.
pub const fn pad4(n: usize) -> usize {
    ceil4(n) - n
}
