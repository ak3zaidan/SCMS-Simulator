//! Visibility profiles and the `NODE-only` transformation — §5, and ADR 0008's
//! "ground-truth channels are tagged; a `NODE-only` replay profile strips them for blind
//! demonstrations".
//!
//! # What blind means here
//!
//! §5.2's exact ground-truth set is implemented field by field below. The principled rule
//! is the one §5.2 states: an unequipped actor transmits nothing, so no node-visible
//! source can know it exists, and its slot is left empty; an equipped actor keeps its
//! pose, because its own broadcasts assert exactly that. The profile strips labels,
//! identity linkage and the truth of a claim. It is not an occlusion simulator.
//!
//! # One blanking implementation, two callers
//!
//! The same functions serve the producer and the stripper, which is what makes
//! conformance V5 (a `full` recording replayed as `node` is byte-identical to a live
//! `node` run) reachable rather than aspirational: there is only one way to blank a
//! frame, so both paths produce the same bytes.
//!
//! # The two callers blank at different moments, and the stripper has to make up for it
//!
//! They are not symmetric, and the asymmetry was a leak. The live producer blanks its
//! *input* ([`crate::encoder::SnapshotEncoder::encode`] calls `blank_input` first), so the
//! lane and `ST_ATTACKER` are already gone when §3.4.2's "only actors whose quantised
//! pose, `state`, `verified_neighbors` or lane changed appear" decides whether to emit a
//! moved row. The stripper blanks a row that the *full* producer already decided to emit.
//!
//! So a parked actor that changes lane, or whose attacker bit toggles, gets a moved row in
//! the full stream; blanking it leaves `dx=dy=dz=0`, `mflags=0` and every other field
//! equal to the previous one, and a live `node` run emits no row at all. The stripped
//! stream is then not byte-identical to a live one (V5 fails), and worse, the surviving
//! all-zero row carries no information *except* "a §5.2 ground-truth field changed at this
//! step" — which is exactly what the profile exists to withhold, so it weakens V1 as well.
//! A blind demonstration would leak through the pattern of emitted rows.
//!
//! [`NodeProfileStripper`] therefore keeps the blanked `(state, verified_neighbors)` it
//! last transmitted per slot and drops a blanked moved row that is a no-op against it.
//! That is §3.4.2's own predicate, evaluated on blanked values, which is precisely what
//! the live producer evaluates. Heading, speed and acceleration deliberately do not enter
//! it: they ride along on a row and never trigger one, in either producer.
//!
//! # The stripper is stateful, and has to be
//!
//! A despawn row carries a slot and a cause, nothing else — so whether it belongs in a
//! blanked stream depends on whether the slot was ever occupied *in that stream*. A live
//! `node` run never allocates a slot for an unequipped actor, so its deltas have no such
//! despawn. [`NodeProfileStripper`] therefore tracks occupancy as it walks the recording
//! and drops rows for slots it did not keep.
//!
//! # It renumbers `seq`, and that is the right call
//!
//! A batch that was entirely ground truth is withheld whole, so a blind stream has fewer
//! canonical frames than the stream it came from. §1.4 requires `seq` to be dense and
//! assigned "by the *producer* … in canonical emission order", and under `profile=node`
//! the replay reader *is* that producer, so the stripper renumbers from zero rather than
//! leaving gaps. Two consequences, both wanted: the blind stream satisfies conformance H4
//! like any other, and its frames come out identical to a live `node` run's (V5), which
//! they could not if one stream had gaps the other did not. Nothing is lost by
//! renumbering, because the profile is fixed for the life of a connection (§5.3), so no
//! client ever holds a `seq` from one profile and resumes into the other.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Result;
use crate::wire::snapshot::{
    DeltaBody, KeyframeBody, MFLAG_LANE_CHANGED, PROFILE_NODE, ST_ATTACKER, ST_EQUIPPED,
};
use crate::wire::telemetry::TelemetryBody;
use crate::wire::{
    FLAG_NODE_ONLY, Frame, MsgType, U8_NONE, U16_NONE, U32_NONE, hello::HelloBody,
    metric::MetricBody, put_f32, put_u8, put_u32,
};

/// Which of the two streams a producer or a reader is on (§5, §0.1 "profile").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// Everything, ground truth included.
    #[default]
    Full,
    /// `NODE-only`: §5.2's ground-truth set is absent, not merely disabled.
    NodeOnly,
}

impl Profile {
    /// The `Keyframe.profile` value (§3.3.1).
    pub const fn wire_value(self) -> u16 {
        match self {
            Profile::Full => crate::wire::snapshot::PROFILE_FULL,
            Profile::NodeOnly => PROFILE_NODE,
        }
    }

    /// The canonical flag bit every frame of this profile carries (§5.3).
    pub const fn frame_flag(self) -> u16 {
        match self {
            Profile::Full => 0,
            Profile::NodeOnly => FLAG_NODE_ONLY,
        }
    }

    /// True if ground truth must be withheld.
    pub const fn is_node_only(self) -> bool {
        matches!(self, Profile::NodeOnly)
    }
}

/// Blanks a keyframe in place, per §5.2's `Keyframe` table.
///
/// Returns the slots that remain occupied, which is the occupancy the following deltas
/// are filtered against.
pub fn blank_keyframe(kf: &mut KeyframeBody) -> BTreeSet<u32> {
    kf.profile = PROFILE_NODE;
    let mut occupied = BTreeSet::new();
    for (slot, row) in kf.actors.iter_mut().enumerate() {
        if !row.is_occupied() {
            continue;
        }
        if row.state & ST_EQUIPPED == 0 {
            // "rows for actors without ST_EQUIPPED — the slot is left empty".
            *row = crate::wire::snapshot::ActorRow::EMPTY;
            continue;
        }
        row.lane_id = U32_NONE;
        row.accel_cq = 0;
        row.state &= !ST_ATTACKER;
        occupied.insert(slot as u32);
    }
    // `actor_count` is the slot high-water mark plus one (§3.3.1), and blanking can empty
    // the highest slot. Truncating the trailing empties is what makes a stripped keyframe
    // identical to the one a live `node`-profile producer would have written, rather than
    // the same frame with a tail of dead rows.
    while kf.actors.last().is_some_and(|r| !r.is_occupied()) {
        kf.actors.pop();
    }
    occupied
}

/// Blanks a delta in place, per §5.2's `Delta` rules, against the occupancy the blanked
/// keyframe and the earlier blanked deltas established.
///
/// `occupied` is updated: spawns that survive are added, despawns that survive are
/// removed.
///
/// This blanks *fields*. It does not decide which rows a blind producer would have emitted
/// at all, because that needs the previously transmitted blanked state, which only
/// [`NodeProfileStripper`] tracks — see the module note on the two callers. Use the
/// stripper rather than this function directly for a stream that has to satisfy
/// conformance V5.
pub fn blank_delta(d: &mut DeltaBody, occupied: &mut BTreeSet<u32>) {
    // Moved rows, with the absolute and lane blocks re-correlated as rows are dropped.
    let mut moved = Vec::with_capacity(d.moved.len());
    let mut abs = Vec::with_capacity(d.abs.len());
    let mut abs_cursor = 0usize;
    let mut lane_cursor = 0usize;
    for row in &d.moved {
        let has_abs = row.mflags & crate::wire::snapshot::MFLAG_ABSOLUTE != 0;
        let has_lane = row.mflags & MFLAG_LANE_CHANGED != 0;
        let this_abs = if has_abs {
            let e = d.abs.get(abs_cursor).copied();
            abs_cursor += 1;
            e
        } else {
            None
        };
        if has_lane {
            lane_cursor += 1;
        }
        if !occupied.contains(&row.slot) {
            continue;
        }
        let mut row = *row;
        row.accel_cq = 0;
        row.state &= !ST_ATTACKER;
        row.mflags &= !MFLAG_LANE_CHANGED;
        if let Some(e) = this_abs {
            abs.push(e);
        }
        moved.push(row);
    }
    debug_assert_eq!(
        lane_cursor,
        d.lanes.len(),
        "the lane block must have one entry per MFLAG_LANE_CHANGED row (§3.4.4)"
    );
    d.moved = moved;
    d.abs = abs;
    d.lanes.clear();

    let mut spawns = Vec::with_capacity(d.spawns.len());
    for s in &d.spawns {
        if s.state & ST_EQUIPPED == 0 {
            continue;
        }
        let mut s = *s;
        s.lane_id = U32_NONE;
        s.cause = U16_NONE;
        s.state &= !ST_ATTACKER;
        occupied.insert(s.slot);
        spawns.push(s);
    }
    d.spawns = spawns;

    let mut despawns = Vec::with_capacity(d.despawns.len());
    for x in &d.despawns {
        if !occupied.remove(&x.slot) {
            continue;
        }
        let mut x = *x;
        x.cause = U16_NONE;
        despawns.push(x);
    }
    d.despawns = despawns;
}

/// Blanks telemetry in place, per §5.2's `Telemetry` table.
pub fn blank_telemetry(t: &mut TelemetryBody) {
    for r in &mut t.records {
        r.clock_offset_ns = 0;
        r.pos_error_m = f32::NAN;
        if r.node_state == 6 {
            // "compromised" is reported as "active".
            r.node_state = 2;
        }
    }
}

/// Drops every ground-truth metric sample, per §5.2's `MetricSample` rule.
pub fn blank_metric(m: &mut MetricBody) {
    m.samples.retain(|s| s.visibility != 0);
}

/// Blanks a `Hello` in place, per §5.2's `Hello` rules: the attacker bit goes, the
/// ground-truth channels leave the table entirely, and `HELLO_NODE_ONLY` is set.
pub fn blank_hello(h: &mut HelloBody) {
    h.hello_flags |= crate::wire::hello::HELLO_NODE_ONLY;
    for n in &mut h.nodes {
        n.flags &= !crate::wire::hello::NODE_IS_ATTACKER;
    }
    // Visibility 0 is GT (§3.1.5); such a channel must be *absent*, not disabled.
    h.channels.retain(|c| c.visibility != 0);
}

/// What the `NODE-only` profile does to one event payload (§5.2's event table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadVerdict {
    /// The record is withheld entirely.
    Drop,
    /// The record survives, with the ground-truth fields blanked in place.
    Keep,
}

/// Blanks one event payload in place and says whether it survives (§5.2).
///
/// The offsets come straight from the payload tables of §3.6; a channel this build does
/// not know is kept untouched, because §3.6.2 reserves 1000–65534 for plug-in channels
/// and a container must not silently mangle one. A plug-in channel that carries ground
/// truth declares [`v2xw_core::Visibility::Gt`] and is stripped whole by the channel
/// filter instead.
pub fn blank_event_payload(channel_id: u16, payload: &mut [u8]) -> PayloadVerdict {
    match channel_id {
        // gt.kinematics, gt.attack.action, gt.spawn, gt.despawn.
        1..=4 => PayloadVerdict::Drop,
        // phy.rx: tx_node, distance_m, los_class.
        11 => {
            if payload.len() >= 48 {
                put_u32(payload, 20, U32_NONE);
                put_f32(payload, 36, f32::NAN);
                put_u8(payload, 42, U8_NONE);
            }
            PayloadVerdict::Keep
        }
        // node.neighbor: peer_actor_id.
        16 => {
            if payload.len() >= 32 {
                put_u32(payload, 24, U32_NONE);
            }
            PayloadVerdict::Keep
        }
        // proto.revocation: stages 0..=4 are withheld entirely (§3.6.10, decision 24).
        22 => {
            let stage = payload.get(28).copied().unwrap_or(0);
            if stage <= 4 {
                PayloadVerdict::Drop
            } else {
                PayloadVerdict::Keep
            }
        }
        // det.observation: subject_actor_id.
        30 => {
            if payload.len() >= 32 {
                put_u32(payload, 20, U32_NONE);
            }
            PayloadVerdict::Keep
        }
        // ma.report / ma.case / ma.decision: subject_actor_id.
        31..=33 => {
            if payload.len() >= 40 {
                put_u32(payload, 28, U32_NONE);
            }
            PayloadVerdict::Keep
        }
        // app.warning: truth, subject_actor_id.
        40 => {
            if payload.len() >= 32 {
                put_u8(payload, 26, 0);
                put_u32(payload, 28, U32_NONE);
            }
            PayloadVerdict::Keep
        }
        _ => PayloadVerdict::Keep,
    }
}

/// Rewrites a `full` stream into a `NODE-only` one, frame by frame, in order.
///
/// Stateful on purpose: see the module note. Feed it every VWP frame of a recording in
/// canonical order; it returns the blanked frame, or `None` for one the profile withholds
/// entirely.
#[derive(Debug, Default)]
pub struct NodeProfileStripper {
    occupied: BTreeSet<u32>,
    /// The `(state, verified_neighbors)` last *transmitted* in the blanked stream, per
    /// slot. §3.4.2's change predicate is evaluated against this, which is what the live
    /// blind producer evaluates it against (see the module note).
    refs: BTreeMap<u32, (u8, u8)>,
    next_seq: u64,
}

impl NodeProfileStripper {
    /// A stripper with no slots yet occupied.
    pub fn new() -> Self {
        Self::default()
    }

    /// The slots currently occupied in the blanked stream.
    pub fn occupied(&self) -> &BTreeSet<u32> {
        &self.occupied
    }

    /// The `seq` the next canonical frame of the blanked stream will carry.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Blanks one frame, or withholds it.
    ///
    /// Canonical frames are renumbered densely from zero (see the module note);
    /// `Hello` carries the seq the next canonical frame will have, as §2.4 requires.
    ///
    /// # Errors
    /// Whatever the frame's decoder returns for a malformed body.
    pub fn strip(&mut self, frame: &Frame) -> Result<Option<Frame>> {
        let h = frame.header()?;
        let flags = (h.flags & !crate::wire::TRANSPORT_FLAG_MASK) | FLAG_NODE_ONLY;
        let seq = self.next_seq;
        let out = match h.kind() {
            Some(MsgType::Hello) => {
                let mut b = HelloBody::decode(frame.body())?;
                blank_hello(&mut b);
                Some(b.to_frame(seq, h.flags)?)
            }
            Some(MsgType::Keyframe) => {
                let mut b = KeyframeBody::decode(frame.body())?;
                self.occupied = blank_keyframe(&mut b);
                // A keyframe transmits every occupied row, so it re-seeds the reference
                // the following deltas' change predicate is evaluated against.
                self.refs.clear();
                for (slot, row) in b.actors.iter().enumerate() {
                    if row.is_occupied() {
                        self.refs
                            .insert(slot as u32, (row.state, row.verified_neighbors));
                    }
                }
                Some(b.to_frame(seq, flags)?)
            }
            Some(MsgType::Delta) => {
                let mut b = DeltaBody::decode(frame.body())?;
                blank_delta(&mut b, &mut self.occupied);
                self.drop_rows_a_blind_producer_would_not_emit(&mut b);
                Some(b.to_frame(seq, flags)?)
            }
            Some(MsgType::Telemetry) => {
                let mut b = TelemetryBody::decode(frame.body())?;
                blank_telemetry(&mut b);
                Some(b.to_frame(seq, flags)?)
            }
            Some(MsgType::MetricSample) => {
                let mut b = MetricBody::decode(frame.body())?;
                blank_metric(&mut b);
                Some(b.to_frame(seq, flags)?)
            }
            Some(MsgType::Event) => {
                let mut b = crate::wire::event::EventBody::decode(frame.body())?;
                let mut kept = Vec::with_capacity(b.entries.len());
                for mut e in b.entries.drain(..) {
                    match blank_event_payload(e.channel_id, &mut e.payload) {
                        PayloadVerdict::Drop => {}
                        PayloadVerdict::Keep => kept.push(e),
                    }
                }
                if kept.is_empty() {
                    None
                } else {
                    b.entries = kept;
                    Some(b.to_frame(seq, flags)?)
                }
            }
            // Provenance carries model ids and parameter sets, which are not ground
            // truth: the `why` panel works in a blind demonstration too.
            _ => Some(frame.with_flags(flags).renumbered(seq)?),
        };
        if out.is_some() && h.kind().is_some_and(MsgType::is_canonical) {
            self.next_seq += 1;
        }
        Ok(out)
    }

    /// Drops a blanked moved row that says nothing, and keeps the per-slot reference.
    ///
    /// §3.4.2: "Only actors whose quantised pose, `state`, `verified_neighbors` or lane
    /// changed appear." In a blind stream the lane is always absent, so a row whose
    /// displacement is zero, whose `mflags` are clear and whose blanked `state` and
    /// `verified_neighbors` equal the last transmitted ones is a row the live blind
    /// producer never emitted — and emitting it announces that a withheld field changed.
    /// Conformance V5 and V1; see the module note.
    ///
    /// A dropped row can carry no absolute-block entry, because `MFLAG_ABSOLUTE` is one of
    /// the `mflags` the predicate requires to be clear; the block is re-correlated anyway
    /// so that the invariant is enforced rather than assumed.
    fn drop_rows_a_blind_producer_would_not_emit(&mut self, d: &mut DeltaBody) {
        let mut kept = Vec::with_capacity(d.moved.len());
        let mut abs = Vec::with_capacity(d.abs.len());
        let mut abs_cursor = 0usize;
        for row in &d.moved {
            let entry = if row.mflags & crate::wire::snapshot::MFLAG_ABSOLUTE != 0 {
                let e = d.abs.get(abs_cursor).copied();
                abs_cursor += 1;
                e
            } else {
                None
            };
            let says_nothing = row.mflags == 0
                && (row.dx_mm, row.dy_mm, row.dz_mm) == (0, 0, 0)
                && self.refs.get(&row.slot) == Some(&(row.state, row.verified_neighbors));
            if says_nothing {
                continue;
            }
            if let Some(e) = entry {
                abs.push(e);
            }
            self.refs
                .insert(row.slot, (row.state, row.verified_neighbors));
            kept.push(*row);
        }
        d.moved = kept;
        d.abs = abs;
        for s in &d.spawns {
            self.refs.insert(s.slot, (s.state, s.verified_neighbors));
        }
        for x in &d.despawns {
            self.refs.remove(&x.slot);
        }
    }
}
