//! The normative binary snapshot stream: `Keyframe` and `Delta` frames
//! (`docs/protocol/vwp-v1.md` §3.3, §3.4).
//!
//! # Why this module exists
//!
//! The Phase 1 build recorded only the serde record path. `RecordingWriter::write_frame`
//! was called from nowhere, so a recording held `gt.kinematics` rows and no snapshot
//! frames at all: a browser could not replay a run, and §7.2's byte-identity guarantee
//! between a live stream and a replayed one was unexercised because there was no live
//! stream to be identical to. This is the producer side of that path.
//!
//! # It does not encode anything itself
//!
//! Every quantised integer, every column layout and the whole keyframe/delta decision is
//! [`v2xw_record::SnapshotEncoder`]'s. This module's whole job is to turn the engine's
//! own state — the actor table, the node table and the Phase 2 attacker set — into the
//! [`v2xw_record::Snapshot`] the encoder takes, once per mobility step. A second encoder
//! here would be a second answer to "what are the bytes", which is the one thing §7.2
//! forbids; `v2xw-record`'s pose quantisation is the one that is measured against the
//! 0.0005 m bound over 100,000 steps, so it is the one that is used.
//!
//! # Slots, and why they are allocated here
//!
//! §3.3.1 indexes keyframe rows **by slot**, not by actor id, and §3.3.1's slot rule —
//! lowest free slot, no reuse until one keyframe period after a despawn — is
//! [`v2xw_record::SlotAllocator`]'s. The engine holds the allocator because it is the
//! engine that knows when an actor appeared and when it left.
//!
//! # Cadence
//!
//! One delta per mobility step and one keyframe per [`v2xw_record::Cadence`]
//! `keyframe_period`; the encoder decides which of the two a step produces. The default
//! is a keyframe every simulated second, which is §3.1.1's default and what the finding
//! this module closes asks for. [`snapshot_cadence`] derives it from the scenario's
//! mobility step, because `Cadence::new` requires the keyframe period to be a whole
//! number of steps and a 30 ms step does not divide a second.
//!
//! # What is not filled in yet, stated rather than stubbed
//!
//! * **Spawn and despawn causes** (§3.4.5, §3.4.6) are written as `0xFFFF`, "unknown".
//!   The engine publishes an actor's disappearance one step after it leaves its own
//!   table, so the cause would have to be carried across a step to reach the frame that
//!   reports it; nothing reads the field yet, and an invented cause code is worse than an
//!   honest "unknown".
//! * **Signals.** The mobility update carries signal states and the world carries signal
//!   plans, but nothing in this build advances a signal controller, so every frame's
//!   signal block is empty rather than carrying a constant phase that would read as a
//!   measurement.
//! * **`verified_neighbors`** is the node's own count of peers in state *verified*
//!   ([`v2xw_node::NeighborTable::counts`]), saturating at 255 as §3.3.2 requires. An
//!   unequipped actor has none and the column is zero.

use v2xw_core::geom::Bbox;
use v2xw_core::ids::{ActorId, NodeId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::time::Duration;
use v2xw_mobility::VehicleClass;
use v2xw_record::wire::Frame;
use v2xw_record::wire::snapshot::{ST_ATTACKER, ST_EQUIPPED, ST_TRANSMITTING};
use v2xw_record::{ActorPose, Cadence, Profile, SlotAllocator, Snapshot, SnapshotEncoder};

use crate::error::Result;

/// The keyframe period a run uses when nothing overrides it: one simulated second
/// (§3.1.1), rounded **up** to a whole number of mobility steps.
///
/// `Cadence::new` refuses a keyframe period that is not a whole number of steps, and
/// ADR 0004 decision 2 allows steps of 10–100 ms, three of which (30, 60, 70 ms and the
/// rest that do not divide 1000) would be refused. Rounding up keeps §7.3's
/// "at most `keyframe_period / mobility_step` deltas per GOP" a whole number and keeps a
/// keyframe on a step boundary, which §3.3.1 requires.
pub const DEFAULT_KEYFRAME_PERIOD: Duration = Duration::from_secs(1);

/// The cadence for a run with this mobility step, keyframes every `period`.
///
/// `period` is rounded up to the next whole number of mobility steps. A zero or
/// non-representable step falls back to the step itself, which makes every frame a
/// keyframe rather than failing a run over a cadence.
pub fn snapshot_cadence(mobility_step: Duration, period: Duration) -> Cadence {
    let step_ns = mobility_step.as_nanos().max(1);
    let want = period.as_nanos().max(step_ns);
    // Round up: `ceil(want / step) * step`.
    let steps = want.div_ceil(step_ns);
    let keyframe_ns = steps.saturating_mul(step_ns).max(step_ns);
    // `Cadence::new` can only fail on a zero period or a non-multiple, and the arithmetic
    // above excludes both; the fallback states that rather than unwrapping.
    Cadence::new(Duration::from_nanos(keyframe_ns), mobility_step).unwrap_or(Cadence {
        keyframe_period: mobility_step,
        mobility_step,
    })
}

/// The quantisation origin of §3.3.1: `floor(bbox_min)` in x and y, zero in z.
///
/// Zero in z because §3.3.2 gives keyframe z as `i16` centimetres, which spans ±327 m: a
/// world-local height is already within that of zero, and an origin that moved with the
/// terrain would make two runs of one world disagree about the same altitude.
pub fn origin_of(bbox: &Bbox) -> [f64; 3] {
    let floor_or_zero = |v: f64| if v.is_finite() { v.floor() } else { 0.0 };
    [floor_or_zero(bbox.min.x), floor_or_zero(bbox.min.y), 0.0]
}

/// One actor's contribution to a snapshot, as the engine knows it.
///
/// A plain argument struct: the engine has the actor table, the node table and the
/// attacker set in three different places, and passing them one at a time to a function
/// with ten parameters is how a heading ends up in the speed column.
#[derive(Debug, Clone, Copy)]
pub struct ActorState {
    /// The actor.
    pub actor: ActorId,
    /// The node riding it, if it is equipped.
    pub node: Option<NodeId>,
    /// Its published ground-truth kinematics.
    pub kinematics: Kinematics,
    /// Its vehicle class.
    pub class: VehicleClass,
    /// Whether an attacker plug-in is armed on it. **Ground truth** (§3.3.4 bit 0).
    pub attacker: bool,
    /// Whether it put a frame on the air during the last mobility step (§3.3.4 bit 4).
    pub transmitting: bool,
    /// How many peers its own neighbour table holds in state *verified*.
    pub verified_neighbors: u32,
}

/// The engine's producer of `Keyframe` and `Delta` frames.
#[derive(Debug)]
pub struct SnapshotStream {
    encoder: SnapshotEncoder,
    slots: SlotAllocator,
    cadence: Cadence,
    profile: Profile,
    keyframes: u64,
    deltas: u64,
}

impl SnapshotStream {
    /// A producer over `bbox`, at `cadence`, in `profile`.
    ///
    /// The first frame is always a keyframe: the encoder has no previous keyframe time,
    /// so §3.3's "a stream opens with a keyframe" holds by construction rather than by a
    /// flag set here.
    pub fn new(bbox: &Bbox, cadence: Cadence, profile: Profile) -> Self {
        SnapshotStream {
            encoder: SnapshotEncoder::new(origin_of(bbox), cadence, profile, 0),
            slots: SlotAllocator::new(cadence.keyframe_period),
            cadence,
            profile,
            keyframes: 0,
            deltas: 0,
        }
    }

    /// The cadence this stream runs at.
    pub fn cadence(&self) -> Cadence {
        self.cadence
    }

    /// The profile this stream is written in.
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// How many keyframes it has produced.
    pub fn keyframes(&self) -> u64 {
        self.keyframes
    }

    /// How many deltas it has produced.
    pub fn deltas(&self) -> u64 {
        self.deltas
    }

    /// Releases an actor's slot into its cooling-off period (§3.3.1, conformance Q4/Q5).
    ///
    /// Called when the engine retires an actor, which is one mobility step before the
    /// frame that reports the despawn: the slot is therefore released *before* the frame
    /// whose row set no longer contains it, which is exactly the order the allocator's
    /// cooling-off rule is written for.
    pub fn retire(&mut self, actor: ActorId, now: v2xw_core::time::SimTime) {
        self.slots.release(actor, now);
    }

    /// Encodes one mobility step and returns the frame to store.
    ///
    /// `states` is every live actor at `at`, in any order; the encoder sorts by slot.
    ///
    /// # Errors
    /// [`crate::EngineError::Record`] if the encoder refuses the step — time that does
    /// not advance, a slot that appears twice, or a body over `u32::MAX`. Each is an
    /// engine bug rather than a scenario error, and each is reported rather than
    /// swallowed: a run that silently stopped writing the wire stream is the defect this
    /// module was written to close.
    pub fn encode(&mut self, at: v2xw_core::time::SimTime, states: &[ActorState]) -> Result<Frame> {
        let mut actors: Vec<ActorPose> = Vec::with_capacity(states.len());
        for s in states {
            let slot = self.slots.allocate(s.actor, at);
            let k = &s.kinematics;
            actors.push(ActorPose {
                slot,
                actor: s.actor,
                node: s.node,
                pos_m: [k.pos.x, k.pos.y, k.pos.z],
                heading_rad: k.heading_rad,
                speed_mps: k.ground_speed_mps(),
                // Longitudinal acceleration: `Kinematics::acc` is a vector in the world
                // frame whose x component is the along-track term (03-interfaces.md §1),
                // and it is the same component `records::GtKinematics` publishes.
                accel_mps2: k.acc.x,
                lane: k.lane.map(|l| l.lane),
                class_idx: class_index(s.class),
                state: state_byte(s),
                verified_neighbors: u8::try_from(s.verified_neighbors).unwrap_or(u8::MAX),
            });
        }
        let snap = Snapshot::new(at, actors);
        let frame = self.encoder.encode(&snap)?;
        if frame.is_keyframe() {
            self.keyframes += 1;
        } else {
            self.deltas += 1;
        }
        Ok(frame.into_frame())
    }
}

/// The class table index §3.3.2's `class_idx` carries.
///
/// [`VehicleClass::ALL`] is "the order the §2.7 table lists them", and that order is the
/// class table a `Hello` would publish. A class the table does not hold is index 0, the
/// passenger car, which is the same fallback `VehicleClass::default` makes.
pub fn class_index(class: VehicleClass) -> u8 {
    VehicleClass::ALL
        .iter()
        .position(|c| *c == class)
        .and_then(|i| u8::try_from(i).ok())
        .unwrap_or(0)
}

/// The §3.3.4 state byte for one actor.
///
/// Three bits are set from state the engine actually holds. The other five —
/// `ST_REPORTED`, `ST_REVOKED`, `ST_PARKED`, `ST_GNSS_DEGRADED` and
/// `ST_WARNING_ACTIVE` — are left clear, because each would need a fact this build does
/// not carry per actor per step, and a bit that is clear because nothing computed it is
/// indistinguishable from a bit that is clear because the condition is false. That is
/// stated here rather than papered over with a plausible default.
pub fn state_byte(s: &ActorState) -> u8 {
    let mut bits = 0u8;
    if s.node.is_some() {
        bits |= ST_EQUIPPED;
    }
    if s.attacker {
        bits |= ST_ATTACKER;
    }
    if s.transmitting {
        bits |= ST_TRANSMITTING;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::geom::Vec3;

    /// The cadence rounds up to a whole number of mobility steps, which is what
    /// `Cadence::new` demands and what a 30 ms step would otherwise fail.
    #[test]
    fn the_cadence_is_a_whole_number_of_mobility_steps() {
        for step_ms in [10u64, 20, 25, 30, 40, 50, 60, 70, 75, 80, 90, 100] {
            let step = Duration::from_millis(step_ms);
            let c = snapshot_cadence(step, DEFAULT_KEYFRAME_PERIOD);
            assert_eq!(c.mobility_step, step, "step {step_ms} ms");
            assert_eq!(
                c.keyframe_period.as_nanos() % step.as_nanos(),
                0,
                "step {step_ms} ms leaves a keyframe period that is not a whole \
                 number of steps"
            );
            // Rounding is up, never down: a keyframe never comes sooner than asked.
            assert!(
                c.keyframe_period.as_nanos() >= DEFAULT_KEYFRAME_PERIOD.as_nanos(),
                "step {step_ms} ms rounded the keyframe period down"
            );
            assert!(
                c.keyframe_period.as_nanos() < DEFAULT_KEYFRAME_PERIOD.as_nanos() + step.as_nanos(),
                "step {step_ms} ms rounded up by more than one step"
            );
        }
    }

    /// The origin is `floor(bbox_min)` in x and y and zero in z (§3.3.1).
    #[test]
    fn the_origin_is_the_floor_of_the_bounding_box() {
        let bbox = Bbox::new(Vec3::new(-12.7, 3.2, 40.0), Vec3::new(500.0, 900.0, 80.0));
        assert_eq!(origin_of(&bbox), [-13.0, 3.0, 0.0]);
    }

    /// The state byte sets exactly the three bits the engine can answer for.
    #[test]
    fn the_state_byte_sets_only_what_the_engine_knows() {
        let base = ActorState {
            actor: ActorId::new(1),
            node: None,
            kinematics: Kinematics::at_rest(0, Vec3::ZERO),
            class: VehicleClass::Passenger,
            attacker: false,
            transmitting: false,
            verified_neighbors: 0,
        };
        assert_eq!(state_byte(&base), 0);
        let equipped = ActorState {
            node: Some(NodeId::new(4)),
            ..base
        };
        assert_eq!(state_byte(&equipped), ST_EQUIPPED);
        let attacking = ActorState {
            attacker: true,
            transmitting: true,
            ..equipped
        };
        assert_eq!(
            state_byte(&attacking),
            ST_EQUIPPED | ST_ATTACKER | ST_TRANSMITTING
        );
    }

    /// The class index is the position in the §2.7 table, so the passenger car is 0.
    #[test]
    fn the_class_index_follows_the_published_table() {
        assert_eq!(class_index(VehicleClass::Passenger), 0);
        assert_eq!(class_index(VehicleClass::Scooter), 11);
        for (i, c) in VehicleClass::ALL.iter().enumerate() {
            assert_eq!(usize::from(class_index(*c)), i);
        }
    }

    /// A stream opens with a keyframe and then produces deltas until the cadence comes
    /// round, which is §3.3's GOP structure. Encoded, not described: the frames are real
    /// and their headers are parsed back.
    #[test]
    fn a_stream_opens_with_a_keyframe_and_then_deltas() {
        let bbox = Bbox::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1000.0, 1000.0, 10.0));
        let step = Duration::from_millis(100);
        let cadence = snapshot_cadence(step, DEFAULT_KEYFRAME_PERIOD);
        let mut stream = SnapshotStream::new(&bbox, cadence, Profile::Full);
        let mut kinds = Vec::new();
        for i in 0..20u64 {
            let at = i * step.as_nanos();
            let state = ActorState {
                actor: ActorId::new(0),
                node: Some(NodeId::new(0)),
                kinematics: Kinematics::at_rest(at, Vec3::new(i as f64 * 1.3, 0.0, 0.0)),
                class: VehicleClass::Passenger,
                attacker: false,
                transmitting: true,
                verified_neighbors: 0,
            };
            let frame = stream.encode(at, &[state]).expect("encodes");
            let kind = frame.header().expect("header").kind().expect("known kind");
            kinds.push(kind);
        }
        assert_eq!(kinds[0], v2xw_record::wire::MsgType::Keyframe);
        assert_eq!(kinds[1], v2xw_record::wire::MsgType::Delta);
        // 1 s of keyframes over 100 ms steps: frames 0 and 10 are keyframes.
        assert_eq!(kinds[10], v2xw_record::wire::MsgType::Keyframe);
        assert_eq!(stream.keyframes(), 2);
        assert_eq!(stream.deltas(), 18);
    }
}
