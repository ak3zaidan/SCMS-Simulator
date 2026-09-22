//! The resolved pose columns a seek lands on, laid out for a zero-copy handoff to
//! JavaScript.
//!
//! # What "zero copy" means here, precisely
//!
//! vwp-v1 §3.3.2 lays a keyframe's actor block out as a **struct of arrays** — `u32[A]`
//! actor ids, then `i32[A]` x, then `i32[A]` y, and so on — exactly so a client can point
//! a typed array at it and upload it to the GPU without parsing. This module keeps that
//! shape: one `Vec` per column, held across seeks, exposed to JavaScript as a pointer and
//! a length that a `Int32Array(memory.buffer, ptr, count)` reads in place. Nothing is
//! serialized, and nothing crosses the boundary but numbers.
//!
//! One copy does happen, and it is the one the format cannot avoid: a `Delta` is a sparse
//! update, so the column a renderer wants at time *t* is the keyframe's column **with the
//! deltas applied**, and that state has to live somewhere. It lives here, is reused
//! between seeks, and grows only when the actor count does.
//!
//! # Applying deltas is the reader's arithmetic, not a second decoder
//!
//! The frames are decoded by [`v2xw_record::wire::snapshot`]. What this module adds is
//! §3.4's application rule: `dx/dy/dz` accumulate against the **previously transmitted**
//! quantised value, heading, speed, acceleration, state and neighbour count are absolute,
//! `MFLAG_ABSOLUTE` replaces the position from the absolute block and `MFLAG_LANE_CHANGED`
//! consumes the next lane id. Because the reference is the transmitted integer and never
//! a float, the result is exactly the server's quantised pose — the property vwp-v1 §3.2
//! and 09-ui §7 rest on — and so it is checked by comparing against a native decode of
//! the same frames rather than against a tolerance.
//!
//! # Units
//!
//! Columns are the wire's integers, not metres: `x_mm`/`y_mm` are millimetres east and
//! north of [`Scene::origin`], `z_mm` is millimetres up (a keyframe's `z_cm` times ten,
//! §3.3.2 against §3.4.2), heading is binary radians, speed is 1/128 m/s and
//! acceleration 1/64 m/s². Converting them is the renderer's business and is a multiply.

use v2xw_core::time::SimTime;
use v2xw_record::error::{RecordError, Result};
use v2xw_record::wire::U32_NONE;
use v2xw_record::wire::snapshot::{
    DeltaBody, KeyframeBody, MFLAG_ABSOLUTE, MFLAG_LANE_CHANGED, SignalRow,
};

/// The largest slot index this reader will grow the columns to.
///
/// A spawn may take a slot above the keyframe's high-water mark (§3.3.1: `actor_count` is
/// the high-water mark *at the keyframe*, and [`v2xw_record::SlotAllocator`] hands out the
/// lowest free slot afterwards), so the columns have to grow. `slot` is a wire `u32`, so
/// without a ceiling a corrupt or hostile delta would ask for four billion rows — about
/// 90 GB across the columns — and the growth, not the file, would be the failure.
///
/// 2^22 slots is 4,194,304, which is 419× the 10,000-actor high tier the rendering budget
/// of 09-ui §4 is written against and about 100 MB of columns. It is a refusal threshold,
/// not a capacity target: no run this simulator is designed for comes near it, and a file
/// that does is refused rather than allocated for.
pub const MAX_SLOTS: usize = 1 << 22;

/// The resolved state of one simulated instant, column by column.
///
/// Every actor column has [`Scene::actor_count`] entries and is indexed by **slot**, not
/// by actor id (§3.3.1). An empty slot carries `actor_id == 0xFFFF_FFFF` and zeros.
#[derive(Debug, Default, Clone)]
pub struct Scene {
    origin: [f64; 3],
    sim_time_ns: SimTime,
    gop_index: u32,
    profile: u16,
    actor_id: Vec<u32>,
    x_mm: Vec<i32>,
    y_mm: Vec<i32>,
    z_mm: Vec<i32>,
    lane_id: Vec<u32>,
    heading_brad: Vec<u16>,
    speed_cq: Vec<i16>,
    accel_cq: Vec<i16>,
    class_idx: Vec<u8>,
    state: Vec<u8>,
    verified_neighbors: Vec<u8>,
    signal_id: Vec<u32>,
    signal_ttc_ds: Vec<u16>,
    signal_phase: Vec<u8>,
    generation: u32,
}

impl Scene {
    /// An empty scene.
    pub fn new() -> Self {
        Scene::default()
    }

    /// The quantisation origin in metres, read from the keyframe rather than cached from
    /// `Hello` (§3.3.1 `DECISION`).
    pub const fn origin(&self) -> [f64; 3] {
        self.origin
    }

    /// The simulated time the scene is resolved to, in nanoseconds.
    pub const fn sim_time_ns(&self) -> SimTime {
        self.sim_time_ns
    }

    /// The GOP the scene belongs to.
    pub const fn gop_index(&self) -> u32 {
        self.gop_index
    }

    /// `0` full, `1` node-only (§5).
    pub const fn profile(&self) -> u16 {
        self.profile
    }

    /// The number of actor slots, which is the length of every actor column.
    pub fn actor_count(&self) -> usize {
        self.actor_id.len()
    }

    /// The number of signal heads.
    pub fn signal_count(&self) -> usize {
        self.signal_id.len()
    }

    /// Bumped whenever a column buffer may have moved in linear memory.
    ///
    /// A JavaScript typed array over `WebAssembly.Memory` is invalidated both by the
    /// column growing (the `Vec` reallocates) and by linear memory growing (the whole
    /// `ArrayBuffer` is detached). A client rebuilds its views when this number changes;
    /// while it does not change, the views it already holds stay valid across seeks.
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// `u32[A]` actor ids; `0xFFFF_FFFF` marks an empty slot.
    pub fn actor_id(&self) -> &[u32] {
        &self.actor_id
    }

    /// `i32[A]` millimetres east of `origin[0]`.
    pub fn x_mm(&self) -> &[i32] {
        &self.x_mm
    }

    /// `i32[A]` millimetres north of `origin[1]`.
    pub fn y_mm(&self) -> &[i32] {
        &self.y_mm
    }

    /// `i32[A]` millimetres up from `origin[2]`.
    pub fn z_mm(&self) -> &[i32] {
        &self.z_mm
    }

    /// `u32[A]` lane ids. **Ground truth**: all `0xFFFF_FFFF` under the `node` profile.
    pub fn lane_id(&self) -> &[u32] {
        &self.lane_id
    }

    /// `u16[A]` headings in binary radians, ENU, 0 = east, counter-clockwise.
    pub fn heading_brad(&self) -> &[u16] {
        &self.heading_brad
    }

    /// `i16[A]` speeds in 1/128 m/s along the heading.
    pub fn speed_cq(&self) -> &[i16] {
        &self.speed_cq
    }

    /// `i16[A]` longitudinal accelerations in 1/64 m/s². **Ground truth.**
    pub fn accel_cq(&self) -> &[i16] {
        &self.accel_cq
    }

    /// `u8[A]` class-table indices.
    pub fn class_idx(&self) -> &[u8] {
        &self.class_idx
    }

    /// `u8[A]` state bytes (§3.3.4).
    pub fn state(&self) -> &[u8] {
        &self.state
    }

    /// `u8[A]` verified-neighbour counts, saturating at 255.
    pub fn verified_neighbors(&self) -> &[u8] {
        &self.verified_neighbors
    }

    /// `u32[S]` signal ids.
    pub fn signal_id(&self) -> &[u32] {
        &self.signal_id
    }

    /// `u16[S]` deciseconds to the next phase change, `0xFFFF` unknown.
    pub fn signal_ttc_ds(&self) -> &[u16] {
        &self.signal_ttc_ds
    }

    /// `u8[S]` SAE J2735 `MovementPhaseState` values.
    pub fn signal_phase(&self) -> &[u8] {
        &self.signal_phase
    }

    /// Resets the scene to a keyframe.
    ///
    /// A keyframe is a complete state, so every column is overwritten and any slot the
    /// previous keyframe had beyond this one's `actor_count` is dropped.
    pub fn load_keyframe(&mut self, kf: &KeyframeBody) {
        let a = kf.actors.len();
        let before = self.buffer_addresses();
        self.origin = kf.origin;
        self.sim_time_ns = kf.sim_time_ns;
        self.gop_index = kf.gop_index;
        self.profile = kf.profile;

        self.actor_id.clear();
        self.x_mm.clear();
        self.y_mm.clear();
        self.z_mm.clear();
        self.lane_id.clear();
        self.heading_brad.clear();
        self.speed_cq.clear();
        self.accel_cq.clear();
        self.class_idx.clear();
        self.state.clear();
        self.verified_neighbors.clear();
        self.actor_id.reserve(a);
        for row in &kf.actors {
            self.actor_id.push(row.actor_id);
            self.x_mm.push(row.x_mm);
            self.y_mm.push(row.y_mm);
            // §3.3.2 carries z on the centimetre grid and §3.4.2 moves it in
            // millimetres; the column is millimetres so the two are one number.
            self.z_mm.push(i32::from(row.z_cm) * 10);
            self.lane_id.push(row.lane_id);
            self.heading_brad.push(row.heading_brad);
            self.speed_cq.push(row.speed_cq);
            self.accel_cq.push(row.accel_cq);
            self.class_idx.push(row.class_idx);
            self.state.push(row.state);
            self.verified_neighbors.push(row.verified_neighbors);
        }

        self.signal_id.clear();
        self.signal_ttc_ds.clear();
        self.signal_phase.clear();
        for row in &kf.signals {
            self.push_signal(row);
        }
        self.note_moves(before);
    }

    /// Applies one delta, in `step_index` order within the GOP.
    ///
    /// # Errors
    /// [`RecordError::Inconsistent`] if the delta names a slot the keyframe did not
    /// declare, if it belongs to another GOP, or if it sets `MFLAG_ABSOLUTE` or
    /// `MFLAG_LANE_CHANGED` on more rows than its absolute or lane block has entries —
    /// each of which is a file that cannot be applied, not a frame to guess at.
    pub fn apply_delta(&mut self, d: &DeltaBody) -> Result<()> {
        let before = self.buffer_addresses();
        let outcome = self.apply_delta_inner(d);
        // Unconditionally, including on the error path: a delta that failed half-way may
        // still have grown a column, and a stale view over a moved buffer is worse than
        // a refused frame.
        self.note_moves(before);
        outcome
    }

    fn apply_delta_inner(&mut self, d: &DeltaBody) -> Result<()> {
        if d.gop_index != self.gop_index {
            return Err(RecordError::Inconsistent {
                at: d.sim_time_ns,
                frames: 0,
                detail: format!(
                    "delta quotes GOP {} but the keyframe in hand is GOP {} (§3.4)",
                    d.gop_index, self.gop_index
                ),
            });
        }
        // Spawns first: §3.4.5 occupies a slot, and a moved row in the same delta may
        // then refer to it.
        for s in &d.spawns {
            // A spawn is the one row that may extend the dense array.
            self.grow_to(s.slot as usize + 1, d.sim_time_ns)?;
            let slot = self.slot(s.slot as usize, d.sim_time_ns, "spawn")?;
            self.actor_id[slot] = s.actor_id;
            self.x_mm[slot] = s.x_mm;
            self.y_mm[slot] = s.y_mm;
            self.z_mm[slot] = i32::from(s.z_cm) * 10;
            self.lane_id[slot] = s.lane_id;
            self.heading_brad[slot] = s.heading_brad;
            self.speed_cq[slot] = s.speed_cq;
            self.accel_cq[slot] = 0;
            self.class_idx[slot] = s.class_idx;
            self.state[slot] = s.state;
            self.verified_neighbors[slot] = s.verified_neighbors;
        }

        let mut abs = d.abs.iter();
        let mut lanes = d.lanes.iter();
        for m in &d.moved {
            let slot = self.slot(m.slot as usize, d.sim_time_ns, "moved")?;
            if m.mflags & MFLAG_ABSOLUTE != 0 {
                let a = abs
                    .next()
                    .ok_or_else(|| Self::short(d.sim_time_ns, "absolute block", m.slot))?;
                self.x_mm[slot] = a.x_mm;
                self.y_mm[slot] = a.y_mm;
                self.z_mm[slot] = i32::from(a.z_cm) * 10;
            } else {
                // The reference is the previously *transmitted* integer, so this is
                // exact and bounded rather than accumulating (§3.2).
                self.x_mm[slot] = self.x_mm[slot].wrapping_add(i32::from(m.dx_mm));
                self.y_mm[slot] = self.y_mm[slot].wrapping_add(i32::from(m.dy_mm));
                self.z_mm[slot] = self.z_mm[slot].wrapping_add(i32::from(m.dz_mm));
            }
            if m.mflags & MFLAG_LANE_CHANGED != 0 {
                self.lane_id[slot] = *lanes
                    .next()
                    .ok_or_else(|| Self::short(d.sim_time_ns, "lane block", m.slot))?;
            }
            self.heading_brad[slot] = m.heading_brad;
            self.speed_cq[slot] = m.speed_cq;
            self.accel_cq[slot] = m.accel_cq;
            self.state[slot] = m.state;
            self.verified_neighbors[slot] = m.verified_neighbors;
        }

        for r in &d.despawns {
            let slot = self.slot(r.slot as usize, d.sim_time_ns, "despawn")?;
            // §3.3.1: an empty slot is `0xFFFFFFFF` and zeros everywhere else.
            self.actor_id[slot] = U32_NONE;
            self.x_mm[slot] = 0;
            self.y_mm[slot] = 0;
            self.z_mm[slot] = 0;
            self.lane_id[slot] = 0;
            self.heading_brad[slot] = 0;
            self.speed_cq[slot] = 0;
            self.accel_cq[slot] = 0;
            self.class_idx[slot] = 0;
            self.state[slot] = 0;
            self.verified_neighbors[slot] = 0;
        }

        for row in &d.signals {
            match self.signal_id.iter().position(|id| *id == row.signal_id) {
                Some(i) => {
                    self.signal_ttc_ds[i] = row.time_to_change_ds;
                    self.signal_phase[i] = row.phase;
                }
                None => self.push_signal(row),
            }
        }

        self.sim_time_ns = d.sim_time_ns;
        Ok(())
    }

    /// Where every exposed column currently starts in linear memory.
    ///
    /// Comparing this before and after a mutation is the only honest way to know whether
    /// a JavaScript view over a column is still valid: a `Vec` moves when it reallocates,
    /// and which of the eleven columns reallocates first depends on element size.
    fn buffer_addresses(&self) -> [usize; 14] {
        [
            self.actor_id.as_ptr() as usize,
            self.x_mm.as_ptr() as usize,
            self.y_mm.as_ptr() as usize,
            self.z_mm.as_ptr() as usize,
            self.lane_id.as_ptr() as usize,
            self.heading_brad.as_ptr() as usize,
            self.speed_cq.as_ptr() as usize,
            self.accel_cq.as_ptr() as usize,
            self.class_idx.as_ptr() as usize,
            self.state.as_ptr() as usize,
            self.verified_neighbors.as_ptr() as usize,
            self.signal_id.as_ptr() as usize,
            self.signal_ttc_ds.as_ptr() as usize,
            self.signal_phase.as_ptr() as usize,
        ]
    }

    fn note_moves(&mut self, before: [usize; 14]) {
        if self.buffer_addresses() != before {
            self.generation = self.generation.wrapping_add(1);
        }
    }

    fn push_signal(&mut self, row: &SignalRow) {
        self.signal_id.push(row.signal_id);
        self.signal_ttc_ds.push(row.time_to_change_ds);
        self.signal_phase.push(row.phase);
    }

    /// Extends every actor column to `len` empty slots (§3.3.1), or refuses.
    ///
    /// # Errors
    /// [`RecordError::ImplausibleCount`] past [`MAX_SLOTS`].
    fn grow_to(&mut self, len: usize, at: SimTime) -> Result<()> {
        if len <= self.actor_id.len() {
            return Ok(());
        }
        if len > MAX_SLOTS {
            return Err(RecordError::implausible_count(
                "actor slots",
                format!(
                    "a spawn at t = {at} asks for slot {} but this reader grows to at most {MAX_SLOTS} slots",
                    len - 1
                ),
            ));
        }
        self.actor_id.resize(len, U32_NONE);
        self.x_mm.resize(len, 0);
        self.y_mm.resize(len, 0);
        self.z_mm.resize(len, 0);
        self.lane_id.resize(len, 0);
        self.heading_brad.resize(len, 0);
        self.speed_cq.resize(len, 0);
        self.accel_cq.resize(len, 0);
        self.class_idx.resize(len, 0);
        self.state.resize(len, 0);
        self.verified_neighbors.resize(len, 0);
        Ok(())
    }

    /// A slot index that is inside the keyframe's dense array, or a refusal.
    ///
    /// A recording is routinely read half-written and sometimes read hostile, so a slot
    /// past the end is rejected rather than silently grown: growing it would invent an
    /// actor the keyframe never declared, and indexing with it would panic.
    fn slot(&self, slot: usize, at: SimTime, what: &'static str) -> Result<usize> {
        if slot < self.actor_id.len() {
            Ok(slot)
        } else {
            Err(RecordError::Inconsistent {
                at,
                frames: 0,
                detail: format!(
                    "{what} row names slot {slot} but the keyframe declared {} slots (§3.3.1)",
                    self.actor_id.len()
                ),
            })
        }
    }

    fn short(at: SimTime, block: &'static str, slot: u32) -> RecordError {
        RecordError::Inconsistent {
            at,
            frames: 0,
            detail: format!("moved row for slot {slot} needs an entry the {block} does not have"),
        }
    }
}
