//! The SAE J2735 `SPAT` message: signal phase and timing, hand-encoded.
//!
//! # Why this is hand-written
//!
//! Build decision D2, for the same reason as the BSM: `rasn-compiler` cannot compile the
//! J2735 modules (170 errors from the `RegionalExtension {REG-EXT-ID-AND-TYPE : Set}`
//! parameterised type), so anything in J2735 the simulator needs on the wire is written
//! against the ASN.1 by hand over the bit engine in [`crate::j2735::uper`].
//!
//! # Read this before quoting a SPaT size: the evidence is weaker than the BSM's
//!
//! The BSM codec ships with a `pycrate` oracle that compiled the real ASN.1 and agreed
//! byte for byte on 235 vectors. **This codec has no such run yet**, and the reason is
//! worth stating plainly rather than burying: the J2735 modules are git-ignored (build
//! decision D3) and **are not in this checkout** — `third_party/asn1/j2735/` does not
//! exist — so the ASN.1 could not be re-read while this module was written, and the
//! oracle's Python environment is not on this machine either.
//!
//! What the structure below rests on instead, field by field:
//!
//! | Element | Evidence |
//! |---|---|
//! | `SPAT` preamble: 1 extension bit + 3 optional bits (`timeStamp`, `name`, `regional`) | corroborated: the derivation table in [`crate::size_model`], written while the modules *were* on disk, states "SPAT preamble (1 extension + 3 optional bits)" |
//! | `IntersectionStateList` `SIZE(1..32)` → 5-bit determinant | corroborated: same table, "`IntersectionStateList` length 5 b" |
//! | `IntersectionState` preamble: 1 + 6 optional bits | corroborated: same table, "one `IntersectionState`: 1+6 preamble" |
//! | `IntersectionReferenceID`: 1 optional bit + 16-bit `IntersectionID` | corroborated: same table, "`id` 1+16" |
//! | `revision` `MsgCount` 7 bits, `status` 16 bits, `moy` 20 bits, `timeStamp` 16 bits | corroborated: same table, and `MsgCount`/`DSecond` are the oracle-validated constants [`crate::j2735::bsm::MSG_COUNT_MAX`] and [`crate::j2735::bsm::D_SECOND_MAX`] |
//! | `MovementList` `SIZE(1..255)` → 8-bit determinant | corroborated: same table, "`MovementList` length 8" |
//! | `MovementState`: 1 + 3 optional bits, `signalGroup` 8 bits, `MovementEventList` 4-bit determinant | corroborated: same table, "`MovementState` 1+3+8+4 b" |
//! | `MovementEvent`: 1 + 3 optional bits | corroborated: same table, "`MovementEvent` 1+3+…" |
//! | `MovementPhaseState`: 10 root values, **4 bits** | **recalled, and it contradicts the size model**, which counted 5 bits for `eventState`. A non-extensible 10-value `ENUMERATED` is 4 bits under X.691 clause 14.3, so either the type carries an extension marker (making it 1 + 4) or the size model's derivation is one bit out. See [`MOVEMENT_PHASE_STATE_WIDTH_IS_DISPUTED`]. |
//! | `TimeChangeDetails`: 5 optional bits, no extension bit; `TimeMark` 16 bits | corroborated: same table, "`TimeChangeDetails` 5 optional bits + three `TimeMark` at 16 b" |
//! | `MinuteOfTheYear (0..527040)`, `TimeMark (0..36001)`, `SignalGroupID (0..255)`, `IntersectionID`/`RoadRegulatorID (0..65535)`, `TimeIntervalConfidence (0..15)` | recalled from SAE J2735 2024-09; **not re-read** from the module |
//! | `DSRCmsgID signalPhaseAndTimingMessage(19)` | recalled; anchored by `basicSafetyMessage(20)` in [`crate::j2735::bsm::BSM_MESSAGE_ID`], which the oracle validated, and 18/19/20 are consecutive in the same list |
//!
//! So: the bytes this module produces are **real UPER of a real structure**, not a fill
//! pattern, and every field is a genuine encoding of the value handed to it. They are not
//! yet *proven* conformant. [`crate::evidence`] is the machine-readable form of that
//! distinction, and the model card of [`crate::j2735::infra::J2735InfraCodec`] states it in
//! words. Do not upgrade either without an oracle run.
//!
//! # The shape of a SPaT, as encoded here
//!
//! ```text
//! SPAT                        extensible SEQUENCE, 3 optional fields
//! ├── timeStamp     OPTIONAL  MinuteOfTheYear
//! ├── name          OPTIONAL  DescriptiveName — refused, never emitted
//! ├── intersections           1..32 IntersectionState
//! │   ├── name      OPTIONAL  refused
//! │   ├── id                  IntersectionReferenceID { region OPTIONAL, id }
//! │   ├── revision            MsgCount
//! │   ├── status              IntersectionStatusObject, BIT STRING (SIZE(16))
//! │   ├── moy       OPTIONAL  MinuteOfTheYear
//! │   ├── timeStamp OPTIONAL  DSecond
//! │   ├── enabledLanes OPT    refused
//! │   ├── states              1..255 MovementState
//! │   │   ├── movementName OPT  refused
//! │   │   ├── signalGroup      SignalGroupID
//! │   │   ├── state-time-speed 1..16 MovementEvent
//! │   │   │   ├── eventState   MovementPhaseState
//! │   │   │   ├── timing  OPT  TimeChangeDetails
//! │   │   │   └── speeds  OPT  refused
//! │   │   └── maneuverAssistList OPT  refused
//! │   └── maneuverAssistList OPT  refused
//! └── regional      OPTIONAL  refused
//! ```
//!
//! Everything marked *refused* is [`CodecError::UnsupportedConstruct`] on decode and is
//! never written on encode, so its preamble bit is always zero. Nothing is skipped: PER
//! carries no tags, so stepping over an element the codec does not model would misread
//! every element after it.
//!
//! # Units are the standard's
//!
//! Every field holds the wire integer. [`minute_of_the_year`], [`d_second`] and
//! [`time_mark`] are the only conversions, and they quantise on the grids of build
//! decision D9 before scaling, exactly as [`crate::j2735::bsm`] does.

use v2xw_core::math;
use v2xw_core::time::{CivilDateTime, SimTime, WallClock};

use crate::codec::{Encoded, MsgType};
use crate::error::CodecError;
use crate::j2735::bsm::{D_SECOND_MAX, D_SECOND_MIN, MSG_COUNT_MAX, MSG_COUNT_MIN};
use crate::j2735::uper::{
    BitReader, BitWriter, Field, UperError, read_constrained_int, read_constrained_length,
    read_enumerated, read_fixed_bit_string, read_open_type, read_preamble, write_constrained_int,
    write_constrained_length, write_enumerated, write_fixed_bit_string, write_open_type,
    write_preamble,
};

// =========================================================================================
// Constraints and sentinels — SAE J2735 2024-09
// =========================================================================================

/// `DSRCmsgID signalPhaseAndTimingMessage(19)`, the `MessageFrame` selector for a SPaT.
pub const SPAT_MESSAGE_ID: u16 = 19;

/// `MinuteOfTheYear ::= INTEGER (0..527040)`, lower bound.
pub const MINUTE_OF_THE_YEAR_MIN: i64 = 0;
/// `MinuteOfTheYear`, upper bound. 527 040 = 366 × 1 440, which the standard reserves for
/// "unknown"; a real minute of a leap year reaches at most 527 039.
pub const MINUTE_OF_THE_YEAR_MAX: i64 = 527_040;
/// The `MinuteOfTheYear` value that means the sender does not know the time.
pub const MINUTE_OF_THE_YEAR_UNKNOWN: u32 = 527_040;

/// `IntersectionID ::= INTEGER (0..65535)`, lower bound.
pub const INTERSECTION_ID_MIN: i64 = 0;
/// `IntersectionID`, upper bound.
pub const INTERSECTION_ID_MAX: i64 = 65_535;

/// `RoadRegulatorID ::= INTEGER (0..65535)`, lower bound.
pub const ROAD_REGULATOR_ID_MIN: i64 = 0;
/// `RoadRegulatorID`, upper bound.
pub const ROAD_REGULATOR_ID_MAX: i64 = 65_535;

/// `SignalGroupID ::= INTEGER (0..255)`, lower bound. Zero is reserved by the standard for
/// "no signal group", which a conforming SPaT does not send in a `MovementState`.
pub const SIGNAL_GROUP_ID_MIN: i64 = 0;
/// `SignalGroupID`, upper bound.
pub const SIGNAL_GROUP_ID_MAX: i64 = 255;

/// `TimeMark ::= INTEGER (0..36001)`, lower bound. The unit is tenths of a second within
/// the current or the next hour.
pub const TIME_MARK_MIN: i64 = 0;
/// `TimeMark`, upper bound.
pub const TIME_MARK_MAX: i64 = 36_001;
/// `TimeMark` value for "the time is not known", per the standard's comment on the type.
pub const TIME_MARK_UNKNOWN: u16 = 36_001;
/// Tenths of a second in an hour: the largest *meaningful* [`TimeMark`], one past which is
/// [`TIME_MARK_UNKNOWN`].
pub const TIME_MARK_TENTHS_PER_HOUR: u16 = 36_000;

/// `TimeIntervalConfidence ::= INTEGER (0..15)`, lower bound.
pub const TIME_INTERVAL_CONFIDENCE_MIN: i64 = 0;
/// `TimeIntervalConfidence`, upper bound.
pub const TIME_INTERVAL_CONFIDENCE_MAX: i64 = 15;

/// `IntersectionStatusObject ::= BIT STRING (SIZE(16))`.
pub const INTERSECTION_STATUS_BITS: u32 = 16;

/// `IntersectionStateList ::= SEQUENCE (SIZE(1..32)) OF IntersectionState`.
pub const MAX_INTERSECTION_STATES: usize = 32;
/// `MovementList ::= SEQUENCE (SIZE(1..255)) OF MovementState`.
pub const MAX_MOVEMENT_STATES: usize = 255;
/// `MovementEventList ::= SEQUENCE (SIZE(1..16)) OF MovementEvent`.
pub const MAX_MOVEMENT_EVENTS: usize = 16;

/// Bytes of the smallest SPaT this codec emits: one intersection, one movement state, one
/// movement event, no optional field anywhere.
///
/// 88 bits exactly: 1 + 3 preamble, 5 list determinant, 1 + 6 state preamble, 1 + 16 id,
/// 7 revision, 16 status, 8 movement determinant, 1 + 3 movement preamble, 8 signal group,
/// 4 event determinant, 1 + 3 event preamble, 4 event state. A test asserts it, because a
/// size that moves means a preamble changed.
pub const MINIMAL_SPAT_SIZE_B: u32 = 11;

/// The same message inside a `MessageFrame`: 1 extension bit + 15-bit `DSRCmsgID` + an
/// 8-bit length determinant + the 11 octets above.
pub const MINIMAL_SPAT_MESSAGE_FRAME_SIZE_B: u32 = 14;

/// The one width in this module that no on-disk artefact corroborates, recorded as a
/// constant so a reviewer can find it and an oracle run can settle it.
///
/// `MovementPhaseState` has ten values. Encoded as a non-extensible `ENUMERATED` (X.691
/// clause 14.3) that is 4 bits, which is what [`MOVEMENT_PHASE_STATE_BITS`] says and what
/// this codec writes. The size-model derivation in [`crate::size_model`] — written while
/// the ASN.1 *was* readable — counted 5 bits for the same field, which is what an
/// *extensible* ten-value enumeration costs (one extension bit plus a 4-bit index).
///
/// One of the two is wrong and the module is not here to arbitrate. If the oracle run
/// disagrees with this codec, the fix is one line: encode the extension bit first. Until
/// then every SPaT this codec emits is one bit per movement event smaller than the size
/// model says, which is exactly the kind of discrepancy that must not be papered over.
pub const MOVEMENT_PHASE_STATE_WIDTH_IS_DISPUTED: bool = true;

/// Root values of `MovementPhaseState`, hence the width of its `ENUMERATED` index.
pub const MOVEMENT_PHASE_STATE_COUNT: u64 = 10;

/// Bits the `MovementPhaseState` index occupies, per X.691 clause 14.3 for ten
/// non-extensible root values.
pub const MOVEMENT_PHASE_STATE_BITS: u32 = 4;

/// Seconds quantum for the conversions in this module (build decision D9).
const Q_S: f64 = 1e-3;

// =========================================================================================
// Enumerations
// =========================================================================================

/// `MovementPhaseState ::= ENUMERATED { … }` — what a signal group is doing.
///
/// The ten root values in declaration order, which is also the order their indices are
/// encoded in (X.691 clause 14.2 encodes the value's *position*, not its number; here the
/// two coincide because the ASN.1 numbers them 0 to 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum MovementPhaseState {
    /// `unavailable(0)`: this signal group's state is not known.
    #[default]
    Unavailable,
    /// `dark(1)`: the signal is not lit.
    Dark,
    /// `stop-Then-Proceed(2)`: flashing red.
    StopThenProceed,
    /// `stop-And-Remain(3)`: solid red.
    StopAndRemain,
    /// `pre-Movement(4)`: red and amber together, a European phase.
    PreMovement,
    /// `permissive-Movement-Allowed(5)`: green with conflicting traffic possible.
    PermissiveMovementAllowed,
    /// `protected-Movement-Allowed(6)`: green with no conflicting traffic.
    ProtectedMovementAllowed,
    /// `permissive-clearance(7)`: amber, conflicting traffic possible.
    PermissiveClearance,
    /// `protected-clearance(8)`: amber, protected.
    ProtectedClearance,
    /// `caution-Conflicting-Traffic(9)`: flashing amber.
    CautionConflictingTraffic,
}

impl MovementPhaseState {
    /// Index in the root list, which is what the encoding carries.
    pub const fn index(self) -> u64 {
        match self {
            MovementPhaseState::Unavailable => 0,
            MovementPhaseState::Dark => 1,
            MovementPhaseState::StopThenProceed => 2,
            MovementPhaseState::StopAndRemain => 3,
            MovementPhaseState::PreMovement => 4,
            MovementPhaseState::PermissiveMovementAllowed => 5,
            MovementPhaseState::ProtectedMovementAllowed => 6,
            MovementPhaseState::PermissiveClearance => 7,
            MovementPhaseState::ProtectedClearance => 8,
            MovementPhaseState::CautionConflictingTraffic => 9,
        }
    }

    /// The value at `index`, or `None` when the index is not in the root list.
    pub const fn from_index(index: u64) -> Option<Self> {
        Some(match index {
            0 => MovementPhaseState::Unavailable,
            1 => MovementPhaseState::Dark,
            2 => MovementPhaseState::StopThenProceed,
            3 => MovementPhaseState::StopAndRemain,
            4 => MovementPhaseState::PreMovement,
            5 => MovementPhaseState::PermissiveMovementAllowed,
            6 => MovementPhaseState::ProtectedMovementAllowed,
            7 => MovementPhaseState::PermissiveClearance,
            8 => MovementPhaseState::ProtectedClearance,
            9 => MovementPhaseState::CautionConflictingTraffic,
            _ => return None,
        })
    }

    /// The ASN.1 identifier, for diagnostics and for the oracle's JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            MovementPhaseState::Unavailable => "unavailable",
            MovementPhaseState::Dark => "dark",
            MovementPhaseState::StopThenProceed => "stop-Then-Proceed",
            MovementPhaseState::StopAndRemain => "stop-And-Remain",
            MovementPhaseState::PreMovement => "pre-Movement",
            MovementPhaseState::PermissiveMovementAllowed => "permissive-Movement-Allowed",
            MovementPhaseState::ProtectedMovementAllowed => "protected-Movement-Allowed",
            MovementPhaseState::PermissiveClearance => "permissive-clearance",
            MovementPhaseState::ProtectedClearance => "protected-clearance",
            MovementPhaseState::CautionConflictingTraffic => "caution-Conflicting-Traffic",
        }
    }
}

/// `IntersectionStatusObject ::= BIT STRING (SIZE(16))` — the controller's own health.
///
/// Held right-aligned in a `u16`, bit `(0)` of the ASN.1 string in the most significant of
/// the sixteen bits, the convention [`crate::j2735::uper::write_fixed_bit_string`]
/// documents and the one [`crate::j2735::bsm::VehicleEventFlags`] uses.
///
/// The size constraint has no extension marker, so a value is exactly sixteen bits and
/// there is no wider encoding to reach for; the two spare bits (14 and 15) are unnamed in
/// the standard and encode as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct IntersectionStatus(pub u16);

impl IntersectionStatus {
    /// Nothing unusual: normal fixed-time or traffic-dependent operation.
    pub const NONE: Self = Self(0);
    /// `manualControlIsEnabled(0)`.
    pub const MANUAL_CONTROL_IS_ENABLED: Self = Self(1 << 15);
    /// `stopTimeIsActivated(1)`.
    pub const STOP_TIME_IS_ACTIVATED: Self = Self(1 << 14);
    /// `failureFlash(2)`.
    pub const FAILURE_FLASH: Self = Self(1 << 13);
    /// `preemptIsActive(3)`.
    pub const PREEMPT_IS_ACTIVE: Self = Self(1 << 12);
    /// `signalPriorityIsActive(4)`.
    pub const SIGNAL_PRIORITY_IS_ACTIVE: Self = Self(1 << 11);
    /// `fixedTimeOperation(5)`.
    pub const FIXED_TIME_OPERATION: Self = Self(1 << 10);
    /// `trafficDependentOperation(6)`.
    pub const TRAFFIC_DEPENDENT_OPERATION: Self = Self(1 << 9);
    /// `standbyOperation(7)`.
    pub const STANDBY_OPERATION: Self = Self(1 << 8);
    /// `failureMode(8)`.
    pub const FAILURE_MODE: Self = Self(1 << 7);
    /// `off(9)`.
    pub const OFF: Self = Self(1 << 6);
    /// `recentMAPmessageUpdate(10)`.
    pub const RECENT_MAP_MESSAGE_UPDATE: Self = Self(1 << 5);
    /// `recentChangeInMAPassignedLanesIDsUsed(11)`.
    pub const RECENT_CHANGE_IN_MAP_ASSIGNED_LANE_IDS: Self = Self(1 << 4);
    /// `noValidMAPisAvailableAtThisTime(12)`.
    pub const NO_VALID_MAP_AVAILABLE: Self = Self(1 << 3);
    /// `noValidSPATisAvailableAtThisTime(13)`.
    pub const NO_VALID_SPAT_AVAILABLE: Self = Self(1 << 2);

    /// The union of two masks.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every bit of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

// =========================================================================================
// Structures
// =========================================================================================

/// `IntersectionReferenceID ::= SEQUENCE { region RoadRegulatorID OPTIONAL, id IntersectionID }`
///
/// No extension marker, one optional field: the encoding is one preamble bit, then the
/// region if present, then the id. The same type appears in a MAP
/// ([`crate::j2735::map::IntersectionGeometry`]) and in a `Connection`, and the two must
/// carry the same value for a receiver to match a SPaT to its geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct IntersectionReferenceId {
    /// `region`: the road regulator that assigned `id`. `None` means the id is globally
    /// unique on its own, which is what a single-region deployment sends.
    pub region: Option<u16>,
    /// `id`: the intersection's identifier within `region`.
    pub id: u16,
}

impl IntersectionReferenceId {
    /// A reference with no region.
    pub const fn new(id: u16) -> Self {
        Self { region: None, id }
    }

    /// A reference qualified by a road regulator.
    pub const fn in_region(region: u16, id: u16) -> Self {
        Self {
            region: Some(region),
            id,
        }
    }
}

/// `TimeChangeDetails ::= SEQUENCE { … }` — when the current phase is expected to end.
///
/// Six fields, five of them optional, **no extension marker**: the encoding is a five-bit
/// bit-map and nothing in front of it. `minEndTime` is the only mandatory field, which is
/// the standard's way of saying that a SPaT must commit to *something*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TimeChangeDetails {
    /// `startTime`: when this phase began, in [`time_mark`] units.
    pub start_time: Option<u16>,
    /// `minEndTime`: the earliest the phase can end. Mandatory.
    pub min_end_time: u16,
    /// `maxEndTime`: the latest it can end.
    pub max_end_time: Option<u16>,
    /// `likelyTime`: the best estimate between the two.
    pub likely_time: Option<u16>,
    /// `confidence`: `TimeIntervalConfidence`, 0..15, in `likelyTime`.
    pub confidence: Option<u8>,
    /// `nextTime`: when this phase is next expected to be active.
    pub next_time: Option<u16>,
}

impl TimeChangeDetails {
    /// The fixed-time case: a phase that ends at a known moment, with no spread.
    ///
    /// `minEndTime` and `maxEndTime` both carry `end`, which is how a fixed-time controller
    /// states a deterministic plan — leaving `maxEndTime` absent would say the end is
    /// unbounded, not that it is certain.
    pub const fn fixed(start: u16, end: u16) -> Self {
        Self {
            start_time: Some(start),
            min_end_time: end,
            max_end_time: Some(end),
            likely_time: None,
            confidence: None,
            next_time: None,
        }
    }
}

/// `MovementEvent ::= SEQUENCE { eventState, timing OPTIONAL, speeds OPTIONAL, regional OPTIONAL, … }`
///
/// `speeds` (`AdvisorySpeedList`) and `regional` are not modelled: they are refused on
/// decode and never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MovementEvent {
    /// `eventState`: the phase itself.
    pub event_state: MovementPhaseState,
    /// `timing`: when it changes.
    pub timing: Option<TimeChangeDetails>,
}

impl MovementEvent {
    /// An event with a phase and no timing.
    pub const fn phase(event_state: MovementPhaseState) -> Self {
        Self {
            event_state,
            timing: None,
        }
    }

    /// An event with a phase and its timing.
    pub const fn timed(event_state: MovementPhaseState, timing: TimeChangeDetails) -> Self {
        Self {
            event_state,
            timing: Some(timing),
        }
    }
}

/// `MovementState ::= SEQUENCE { movementName OPTIONAL, signalGroup, state-time-speed, … }`
///
/// One signal group's current and upcoming phases. `movementName`,
/// `maneuverAssistList` and `regional` are not modelled.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MovementState {
    /// `signalGroup`: the id a MAP's `Connection` points at.
    pub signal_group: u8,
    /// `state-time-speed`: 1..16 events, the first being the current phase.
    pub events: Vec<MovementEvent>,
}

impl MovementState {
    /// One signal group in one phase.
    pub fn current(signal_group: u8, event: MovementEvent) -> Self {
        Self {
            signal_group,
            events: vec![event],
        }
    }
}

/// `IntersectionState ::= SEQUENCE { … }` — one intersection's signal state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntersectionState {
    /// `id`: which intersection, matching the MAP's `IntersectionGeometry`.
    pub id: IntersectionReferenceId,
    /// `revision`: `MsgCount`, bumped when the *plan* changes, not per message.
    pub revision: u8,
    /// `status`: the controller's health and mode.
    pub status: IntersectionStatus,
    /// `moy`: minute of the year the state was determined in.
    pub moy: Option<u32>,
    /// `timeStamp`: `DSecond`, the millisecond within that minute.
    pub time_stamp: Option<u16>,
    /// `states`: 1..255 movement states, one per signal group.
    pub states: Vec<MovementState>,
}

/// The `SPAT` PDU, as this codec models it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Spat {
    /// `timeStamp`: minute of the year the message was assembled in.
    pub time_stamp: Option<u32>,
    /// `intersections`: 1..32 intersection states.
    pub intersections: Vec<IntersectionState>,
}

impl Spat {
    /// A SPaT for one intersection.
    pub fn one(intersection: IntersectionState) -> Self {
        Self {
            time_stamp: None,
            intersections: vec![intersection],
        }
    }
}

// =========================================================================================
// Field descriptors
// =========================================================================================

const F_SPAT_TIME_STAMP: Field = Field::new("spat.timeStamp", "MinuteOfTheYear");
const F_INTERSECTIONS_LEN: Field = Field::new(
    "spat.intersections",
    "SEQUENCE (SIZE(1..32)) OF IntersectionState",
);
const F_REGION: Field = Field::new("intersectionState.id.region", "RoadRegulatorID");
const F_INTERSECTION_ID: Field = Field::new("intersectionState.id.id", "IntersectionID");
const F_REVISION: Field = Field::new("intersectionState.revision", "MsgCount");
const F_STATUS: Field = Field::new("intersectionState.status", "IntersectionStatusObject");
const F_MOY: Field = Field::new("intersectionState.moy", "MinuteOfTheYear");
const F_STATE_TIME_STAMP: Field = Field::new("intersectionState.timeStamp", "DSecond");
const F_STATES_LEN: Field = Field::new(
    "intersectionState.states",
    "SEQUENCE (SIZE(1..255)) OF MovementState",
);
const F_SIGNAL_GROUP: Field = Field::new("movementState.signalGroup", "SignalGroupID");
const F_EVENTS_LEN: Field = Field::new(
    "movementState.state-time-speed",
    "SEQUENCE (SIZE(1..16)) OF MovementEvent",
);
const F_EVENT_STATE: Field = Field::new("movementEvent.eventState", "MovementPhaseState");
const F_START_TIME: Field = Field::new("timeChangeDetails.startTime", "TimeMark");
const F_MIN_END_TIME: Field = Field::new("timeChangeDetails.minEndTime", "TimeMark");
const F_MAX_END_TIME: Field = Field::new("timeChangeDetails.maxEndTime", "TimeMark");
const F_LIKELY_TIME: Field = Field::new("timeChangeDetails.likelyTime", "TimeMark");
const F_CONFIDENCE: Field = Field::new("timeChangeDetails.confidence", "TimeIntervalConfidence");
const F_NEXT_TIME: Field = Field::new("timeChangeDetails.nextTime", "TimeMark");
const F_MESSAGE_ID: Field = Field::new("messageFrame.messageId", "DSRCmsgID");

// =========================================================================================
// Encoding
// =========================================================================================

fn write_reference_id(w: &mut BitWriter, id: &IntersectionReferenceId) -> Result<(), UperError> {
    // IntersectionReferenceID has no extension marker and one OPTIONAL field.
    write_preamble(w, false, &[id.region.is_some()]);
    if let Some(region) = id.region {
        write_constrained_int(
            w,
            F_REGION,
            i64::from(region),
            ROAD_REGULATOR_ID_MIN,
            ROAD_REGULATOR_ID_MAX,
        )?;
    }
    write_constrained_int(
        w,
        F_INTERSECTION_ID,
        i64::from(id.id),
        INTERSECTION_ID_MIN,
        INTERSECTION_ID_MAX,
    )
}

fn read_reference_id(r: &mut BitReader<'_>) -> Result<IntersectionReferenceId, UperError> {
    let pre = read_preamble(r, "IntersectionReferenceID", false, 1)?;
    let region = if pre.has(0) {
        Some(
            read_constrained_int(r, F_REGION, ROAD_REGULATOR_ID_MIN, ROAD_REGULATOR_ID_MAX)? as u16,
        )
    } else {
        None
    };
    let id = read_constrained_int(
        r,
        F_INTERSECTION_ID,
        INTERSECTION_ID_MIN,
        INTERSECTION_ID_MAX,
    )? as u16;
    Ok(IntersectionReferenceId { region, id })
}

/// `IntersectionReferenceID` as the MAP module needs it.
///
/// The type is declared in the same J2735 module and appears in both messages, and a
/// receiver matches a SPaT to its geometry by comparing the two values, so there is one
/// encoder for it rather than two that could drift apart. Crate-visible rather than
/// public: the structure is [`IntersectionReferenceId`], and callers outside the crate
/// encode it by encoding the message that carries it.
pub(crate) fn write_reference_id_for(
    w: &mut BitWriter,
    id: &IntersectionReferenceId,
) -> Result<(), UperError> {
    write_reference_id(w, id)
}

/// `IntersectionReferenceID` as the MAP module needs it, decoding.
pub(crate) fn read_reference_id_for(
    r: &mut BitReader<'_>,
) -> Result<IntersectionReferenceId, UperError> {
    read_reference_id(r)
}

fn write_time_change_details(w: &mut BitWriter, t: &TimeChangeDetails) -> Result<(), UperError> {
    // No extension marker; five OPTIONAL fields in declaration order.
    write_preamble(
        w,
        false,
        &[
            t.start_time.is_some(),
            t.max_end_time.is_some(),
            t.likely_time.is_some(),
            t.confidence.is_some(),
            t.next_time.is_some(),
        ],
    );
    if let Some(v) = t.start_time {
        write_constrained_int(w, F_START_TIME, i64::from(v), TIME_MARK_MIN, TIME_MARK_MAX)?;
    }
    write_constrained_int(
        w,
        F_MIN_END_TIME,
        i64::from(t.min_end_time),
        TIME_MARK_MIN,
        TIME_MARK_MAX,
    )?;
    if let Some(v) = t.max_end_time {
        write_constrained_int(
            w,
            F_MAX_END_TIME,
            i64::from(v),
            TIME_MARK_MIN,
            TIME_MARK_MAX,
        )?;
    }
    if let Some(v) = t.likely_time {
        write_constrained_int(w, F_LIKELY_TIME, i64::from(v), TIME_MARK_MIN, TIME_MARK_MAX)?;
    }
    if let Some(v) = t.confidence {
        write_constrained_int(
            w,
            F_CONFIDENCE,
            i64::from(v),
            TIME_INTERVAL_CONFIDENCE_MIN,
            TIME_INTERVAL_CONFIDENCE_MAX,
        )?;
    }
    if let Some(v) = t.next_time {
        write_constrained_int(w, F_NEXT_TIME, i64::from(v), TIME_MARK_MIN, TIME_MARK_MAX)?;
    }
    Ok(())
}

/// A `TimeMark`, which `TimeChangeDetails` carries up to four of.
///
/// A free function rather than a closure inside the reader: a closure with an explicitly
/// typed `&mut` parameter gets one inferred lifetime for every call site, which would hold
/// a reborrow of the reader across the whole decode and collide with the fields read
/// between the marks.
fn read_time_mark(r: &mut BitReader<'_>, field: Field) -> Result<u16, UperError> {
    Ok(read_constrained_int(r, field, TIME_MARK_MIN, TIME_MARK_MAX)? as u16)
}

fn read_time_change_details(r: &mut BitReader<'_>) -> Result<TimeChangeDetails, UperError> {
    let pre = read_preamble(r, "TimeChangeDetails", false, 5)?;
    let start_time = if pre.has(0) {
        Some(read_time_mark(r, F_START_TIME)?)
    } else {
        None
    };
    let min_end_time = read_time_mark(r, F_MIN_END_TIME)?;
    let max_end_time = if pre.has(1) {
        Some(read_time_mark(r, F_MAX_END_TIME)?)
    } else {
        None
    };
    let likely_time = if pre.has(2) {
        Some(read_time_mark(r, F_LIKELY_TIME)?)
    } else {
        None
    };
    let confidence = if pre.has(3) {
        Some(read_constrained_int(
            r,
            F_CONFIDENCE,
            TIME_INTERVAL_CONFIDENCE_MIN,
            TIME_INTERVAL_CONFIDENCE_MAX,
        )? as u8)
    } else {
        None
    };
    let next_time = if pre.has(4) {
        Some(read_time_mark(r, F_NEXT_TIME)?)
    } else {
        None
    };
    Ok(TimeChangeDetails {
        start_time,
        min_end_time,
        max_end_time,
        likely_time,
        confidence,
        next_time,
    })
}

fn write_movement_event(w: &mut BitWriter, e: &MovementEvent) -> Result<(), UperError> {
    // speeds and regional are never written, so their preamble bits are always zero.
    write_preamble(w, true, &[e.timing.is_some(), false, false]);
    write_enumerated(
        w,
        F_EVENT_STATE,
        e.event_state.index(),
        MOVEMENT_PHASE_STATE_COUNT,
    )?;
    if let Some(timing) = &e.timing {
        write_time_change_details(w, timing)?;
    }
    Ok(())
}

fn read_movement_event(r: &mut BitReader<'_>) -> Result<MovementEvent, UperError> {
    let pre = read_preamble(r, "MovementEvent", true, 3)?;
    let index = read_enumerated(r, F_EVENT_STATE, MOVEMENT_PHASE_STATE_COUNT)?;
    let event_state = MovementPhaseState::from_index(index).ok_or(UperError::BadEnumIndex {
        asn1_type: F_EVENT_STATE.asn1_type,
        index,
        count: MOVEMENT_PHASE_STATE_COUNT,
    })?;
    let timing = if pre.has(0) {
        Some(read_time_change_details(r)?)
    } else {
        None
    };
    if pre.has(1) {
        return Err(UperError::Unsupported {
            construct: "MovementEvent.speeds",
            detail: "an AdvisorySpeedList is present; this codec models the phase and its \
                     timing only, and cannot skip the list without losing bit \
                     synchronisation",
        });
    }
    if pre.has(2) {
        return Err(UperError::Unsupported {
            construct: "MovementEvent.regional",
            detail: "a regional extension is present; no Reg-MovementEvent object is \
                     modelled, and its open type cannot be interpreted",
        });
    }
    Ok(MovementEvent {
        event_state,
        timing,
    })
}

fn write_movement_state(w: &mut BitWriter, s: &MovementState) -> Result<(), UperError> {
    if s.events.is_empty() || s.events.len() > MAX_MOVEMENT_EVENTS {
        return Err(UperError::OutOfRange {
            field: F_EVENTS_LEN.path,
            asn1_type: F_EVENTS_LEN.asn1_type,
            value: s.events.len() as i64,
            min: 1,
            max: MAX_MOVEMENT_EVENTS as i64,
        });
    }
    // movementName, maneuverAssistList and regional are never written.
    write_preamble(w, true, &[false, false, false]);
    write_constrained_int(
        w,
        F_SIGNAL_GROUP,
        i64::from(s.signal_group),
        SIGNAL_GROUP_ID_MIN,
        SIGNAL_GROUP_ID_MAX,
    )?;
    write_constrained_length(w, F_EVENTS_LEN, s.events.len(), 1, MAX_MOVEMENT_EVENTS)?;
    for event in &s.events {
        write_movement_event(w, event)?;
    }
    Ok(())
}

fn read_movement_state(r: &mut BitReader<'_>) -> Result<MovementState, UperError> {
    let pre = read_preamble(r, "MovementState", true, 3)?;
    if pre.has(0) {
        return Err(UperError::Unsupported {
            construct: "MovementState.movementName",
            detail: "a DescriptiveName is present; this engine encodes no character \
                     strings, and an IA5String cannot be stepped over without losing bit \
                     synchronisation",
        });
    }
    let signal_group =
        read_constrained_int(r, F_SIGNAL_GROUP, SIGNAL_GROUP_ID_MIN, SIGNAL_GROUP_ID_MAX)? as u8;
    let count = read_constrained_length(r, F_EVENTS_LEN, 1, MAX_MOVEMENT_EVENTS)?;
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        events.push(read_movement_event(r)?);
    }
    if pre.has(1) {
        return Err(UperError::Unsupported {
            construct: "MovementState.maneuverAssistList",
            detail: "a ManeuverAssistList is present; this codec does not model queue and \
                     available-storage data and cannot skip the list",
        });
    }
    if pre.has(2) {
        return Err(UperError::Unsupported {
            construct: "MovementState.regional",
            detail: "a regional extension is present; no Reg-MovementState object is \
                     modelled, and its open type cannot be interpreted",
        });
    }
    Ok(MovementState {
        signal_group,
        events,
    })
}

fn write_intersection_state(w: &mut BitWriter, s: &IntersectionState) -> Result<(), UperError> {
    if s.states.is_empty() || s.states.len() > MAX_MOVEMENT_STATES {
        return Err(UperError::OutOfRange {
            field: F_STATES_LEN.path,
            asn1_type: F_STATES_LEN.asn1_type,
            value: s.states.len() as i64,
            min: 1,
            max: MAX_MOVEMENT_STATES as i64,
        });
    }
    // Optional fields in declaration order: name, moy, timeStamp, enabledLanes,
    // maneuverAssistList, regional. Only moy and timeStamp are ever written.
    write_preamble(
        w,
        true,
        &[
            false,
            s.moy.is_some(),
            s.time_stamp.is_some(),
            false,
            false,
            false,
        ],
    );
    write_reference_id(w, &s.id)?;
    write_constrained_int(
        w,
        F_REVISION,
        i64::from(s.revision),
        MSG_COUNT_MIN,
        MSG_COUNT_MAX,
    )?;
    write_fixed_bit_string(w, F_STATUS, u64::from(s.status.0), INTERSECTION_STATUS_BITS)?;
    if let Some(moy) = s.moy {
        write_constrained_int(
            w,
            F_MOY,
            i64::from(moy),
            MINUTE_OF_THE_YEAR_MIN,
            MINUTE_OF_THE_YEAR_MAX,
        )?;
    }
    if let Some(ts) = s.time_stamp {
        write_constrained_int(
            w,
            F_STATE_TIME_STAMP,
            i64::from(ts),
            D_SECOND_MIN,
            D_SECOND_MAX,
        )?;
    }
    write_constrained_length(w, F_STATES_LEN, s.states.len(), 1, MAX_MOVEMENT_STATES)?;
    for state in &s.states {
        write_movement_state(w, state)?;
    }
    Ok(())
}

fn read_intersection_state(r: &mut BitReader<'_>) -> Result<IntersectionState, UperError> {
    let pre = read_preamble(r, "IntersectionState", true, 6)?;
    if pre.has(0) {
        return Err(UperError::Unsupported {
            construct: "IntersectionState.name",
            detail: "a DescriptiveName is present; this engine encodes no character \
                     strings, and an IA5String cannot be stepped over without losing bit \
                     synchronisation",
        });
    }
    let id = read_reference_id(r)?;
    let revision = read_constrained_int(r, F_REVISION, MSG_COUNT_MIN, MSG_COUNT_MAX)? as u8;
    let status = IntersectionStatus(read_fixed_bit_string(r, INTERSECTION_STATUS_BITS)? as u16);
    let moy = if pre.has(1) {
        Some(read_constrained_int(r, F_MOY, MINUTE_OF_THE_YEAR_MIN, MINUTE_OF_THE_YEAR_MAX)? as u32)
    } else {
        None
    };
    let time_stamp = if pre.has(2) {
        Some(read_constrained_int(r, F_STATE_TIME_STAMP, D_SECOND_MIN, D_SECOND_MAX)? as u16)
    } else {
        None
    };
    if pre.has(3) {
        return Err(UperError::Unsupported {
            construct: "IntersectionState.enabledLanes",
            detail: "an EnabledLaneList is present; this codec does not model revocable \
                     lanes and cannot skip the list",
        });
    }
    let count = read_constrained_length(r, F_STATES_LEN, 1, MAX_MOVEMENT_STATES)?;
    let mut states = Vec::with_capacity(count);
    for _ in 0..count {
        states.push(read_movement_state(r)?);
    }
    if pre.has(4) {
        return Err(UperError::Unsupported {
            construct: "IntersectionState.maneuverAssistList",
            detail: "a ManeuverAssistList is present; this codec does not model queue and \
                     available-storage data and cannot skip the list",
        });
    }
    if pre.has(5) {
        return Err(UperError::Unsupported {
            construct: "IntersectionState.regional",
            detail: "a regional extension is present; no Reg-IntersectionState object is \
                     modelled, and its open type cannot be interpreted",
        });
    }
    Ok(IntersectionState {
        id,
        revision,
        status,
        moy,
        time_stamp,
        states,
    })
}

fn write_spat(w: &mut BitWriter, spat: &Spat) -> Result<(), UperError> {
    if spat.intersections.is_empty() || spat.intersections.len() > MAX_INTERSECTION_STATES {
        return Err(UperError::OutOfRange {
            field: F_INTERSECTIONS_LEN.path,
            asn1_type: F_INTERSECTIONS_LEN.asn1_type,
            value: spat.intersections.len() as i64,
            min: 1,
            max: MAX_INTERSECTION_STATES as i64,
        });
    }
    // name and regional are never written.
    write_preamble(w, true, &[spat.time_stamp.is_some(), false, false]);
    if let Some(ts) = spat.time_stamp {
        write_constrained_int(
            w,
            F_SPAT_TIME_STAMP,
            i64::from(ts),
            MINUTE_OF_THE_YEAR_MIN,
            MINUTE_OF_THE_YEAR_MAX,
        )?;
    }
    write_constrained_length(
        w,
        F_INTERSECTIONS_LEN,
        spat.intersections.len(),
        1,
        MAX_INTERSECTION_STATES,
    )?;
    for state in &spat.intersections {
        write_intersection_state(w, state)?;
    }
    Ok(())
}

fn read_spat(r: &mut BitReader<'_>) -> Result<Spat, UperError> {
    let pre = read_preamble(r, "SPAT", true, 3)?;
    let time_stamp = if pre.has(0) {
        Some(read_constrained_int(
            r,
            F_SPAT_TIME_STAMP,
            MINUTE_OF_THE_YEAR_MIN,
            MINUTE_OF_THE_YEAR_MAX,
        )? as u32)
    } else {
        None
    };
    if pre.has(1) {
        return Err(UperError::Unsupported {
            construct: "SPAT.name",
            detail: "a DescriptiveName is present; this engine encodes no character \
                     strings, and an IA5String cannot be stepped over without losing bit \
                     synchronisation",
        });
    }
    let count = read_constrained_length(r, F_INTERSECTIONS_LEN, 1, MAX_INTERSECTION_STATES)?;
    let mut intersections = Vec::with_capacity(count);
    for _ in 0..count {
        intersections.push(read_intersection_state(r)?);
    }
    if pre.has(2) {
        return Err(UperError::Unsupported {
            construct: "SPAT.regional",
            detail: "a regional extension is present; no Reg-SPAT object is modelled, and \
                     its open type cannot be interpreted",
        });
    }
    Ok(Spat {
        time_stamp,
        intersections,
    })
}

// =========================================================================================
// Public codec entry points
// =========================================================================================

fn on_encode(e: UperError) -> CodecError {
    match e {
        UperError::OutOfRange {
            field,
            asn1_type,
            value,
            min,
            max,
        } => CodecError::OutOfRange {
            field,
            asn1_type,
            value,
            min,
            max,
        },
        UperError::Unsupported { construct, detail } => CodecError::UnsupportedConstruct {
            ty: MsgType::Spat,
            construct,
            detail,
        },
        other => CodecError::Encode {
            ty: MsgType::Spat,
            detail: other.to_string(),
        },
    }
}

fn on_decode(len: usize) -> impl Fn(UperError) -> CodecError {
    move |e| match e {
        UperError::Unsupported { construct, detail } => CodecError::UnsupportedConstruct {
            ty: MsgType::Spat,
            construct,
            detail,
        },
        other => CodecError::Decode {
            ty: MsgType::Spat,
            len,
            detail: other.to_string(),
        },
    }
}

/// UPER-encodes a `SPAT` PDU.
///
/// The bytes are real UPER of the subset documented at the top of this module —
/// [`Encoded::size_source`] is [`crate::SizeSource::Uper`] and the size is measured, not
/// modelled — but they are not yet oracle-validated. See [`crate::evidence`].
pub fn encode_spat(spat: &Spat) -> Result<Encoded, CodecError> {
    let mut w = BitWriter::with_capacity(64);
    write_spat(&mut w, spat).map_err(on_encode)?;
    Ok(Encoded::uper(w.into_bytes()))
}

/// Decodes a `SPAT` PDU, refusing trailing data beyond X.691's padding.
pub fn decode_spat(bytes: &[u8]) -> Result<Spat, CodecError> {
    let map = on_decode(bytes.len());
    let mut r = BitReader::new(bytes);
    let spat = read_spat(&mut r).map_err(&map)?;
    r.finish().map_err(&map)?;
    Ok(spat)
}

/// UPER-encodes a `MessageFrame` carrying this SPaT — what goes in a WSM payload.
pub fn encode_message_frame(spat: &Spat) -> Result<Encoded, CodecError> {
    let mut inner = BitWriter::with_capacity(64);
    write_spat(&mut inner, spat).map_err(on_encode)?;
    let inner = inner.into_bytes();

    let mut w = BitWriter::with_capacity(inner.len() + 4);
    write_preamble(&mut w, true, &[]);
    write_constrained_int(
        &mut w,
        F_MESSAGE_ID,
        i64::from(SPAT_MESSAGE_ID),
        0,
        i64::from(crate::j2735::bsm::DSRC_MSG_ID_MAX),
    )
    .map_err(on_encode)?;
    write_open_type(&mut w, "MessageFrame.value", &inner).map_err(on_encode)?;
    Ok(Encoded::uper(w.into_bytes()))
}

/// Decodes a `MessageFrame` and returns the SPaT inside it.
///
/// Refuses any `DSRCmsgID` other than [`SPAT_MESSAGE_ID`]: a frame carrying a MAP is not a
/// SPaT with odd fields.
pub fn decode_message_frame(bytes: &[u8]) -> Result<Spat, CodecError> {
    let map = on_decode(bytes.len());
    let mut r = BitReader::new(bytes);
    read_preamble(&mut r, "MessageFrame", true, 0).map_err(&map)?;
    let id = read_constrained_int(
        &mut r,
        F_MESSAGE_ID,
        0,
        i64::from(crate::j2735::bsm::DSRC_MSG_ID_MAX),
    )
    .map_err(&map)? as u16;
    if id != SPAT_MESSAGE_ID {
        return Err(CodecError::UnsupportedConstruct {
            ty: MsgType::Spat,
            construct: "MessageFrame.messageId",
            detail: "the frame carries a DSRCmsgID other than \
                     signalPhaseAndTimingMessage(19); this entry point decodes a SPaT only",
        });
    }
    let inner = read_open_type(&mut r, "MessageFrame.value").map_err(&map)?;
    r.finish().map_err(&map)?;

    let mut ir = BitReader::new(&inner);
    let spat = read_spat(&mut ir).map_err(&map)?;
    ir.finish().map_err(&map)?;
    Ok(spat)
}

// =========================================================================================
// From simulator quantities to wire units
// =========================================================================================

/// `MinuteOfTheYear` for a simulated instant: whole minutes since 1 January 00:00 UTC of
/// the year that instant falls in.
///
/// Derived from the scenario wall clock and [`SimTime`], never from a system clock: engine
/// facing code reads no wall clock (02-architecture.md), and a recorded run has to replay
/// to the same minute. Leap seconds are not modelled, which
/// [`crate::j2735::bsm::sec_mark`] already assumes for `secMark`.
///
/// Returns [`MINUTE_OF_THE_YEAR_UNKNOWN`] rather than a wrong minute if the arithmetic
/// lands outside the type's range, which can only happen for a scenario `t0` outside the
/// proleptic Gregorian range `CivilDateTime` covers.
pub fn minute_of_the_year(clock: WallClock, t: SimTime) -> u32 {
    let civil = clock.civil_at(t);
    let Ok(year_start) = CivilDateTime::new(civil.year, 1, 1, 0, 0, 0) else {
        return MINUTE_OF_THE_YEAR_UNKNOWN;
    };
    let minutes = (civil.to_unix_seconds() - year_start.to_unix_seconds()).div_euclid(60);
    if (MINUTE_OF_THE_YEAR_MIN..MINUTE_OF_THE_YEAR_MAX).contains(&minutes) {
        minutes as u32
    } else {
        MINUTE_OF_THE_YEAR_UNKNOWN
    }
}

/// `DSecond` for a simulated instant: the millisecond within the current UTC minute.
///
/// The same quantity `secMark` carries in a BSM, and computed by the same function, so a
/// SPaT and a BSM emitted at one instant agree.
pub fn d_second(clock: WallClock, t: SimTime) -> u16 {
    crate::j2735::bsm::sec_mark(clock, t)
}

/// `TimeMark` from a phase boundary expressed in seconds since the top of the hour.
///
/// The unit is tenths of a second. Values at or beyond the hour fold into the next hour the
/// way the standard intends — 36 000 is "the top of the next hour" — and anything not
/// finite, negative, or beyond that becomes [`TIME_MARK_UNKNOWN`] rather than a plausible
/// wrong time.
///
/// Quantises on the D9 second grid before scaling, so the integer is a function of the
/// quantised value rather than of an `f64`'s last bit.
pub fn time_mark(seconds_into_hour: f64) -> u16 {
    if !seconds_into_hour.is_finite() || seconds_into_hour < 0.0 {
        return TIME_MARK_UNKNOWN;
    }
    let tenths = (math::quantize_to(seconds_into_hour, Q_S) / 0.1).round();
    if !(0.0..=f64::from(TIME_MARK_TENTHS_PER_HOUR)).contains(&tenths) {
        return TIME_MARK_UNKNOWN;
    }
    tenths as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nominal_event() -> MovementEvent {
        MovementEvent::timed(
            MovementPhaseState::ProtectedMovementAllowed,
            TimeChangeDetails::fixed(time_mark(12.0), time_mark(27.5)),
        )
    }

    fn nominal() -> Spat {
        Spat {
            time_stamp: Some(12_345),
            intersections: vec![IntersectionState {
                id: IntersectionReferenceId::in_region(7, 1_234),
                revision: 3,
                status: IntersectionStatus::FIXED_TIME_OPERATION,
                moy: Some(12_345),
                time_stamp: Some(43_210),
                states: vec![
                    MovementState::current(1, nominal_event()),
                    MovementState::current(
                        2,
                        MovementEvent::phase(MovementPhaseState::StopAndRemain),
                    ),
                ],
            }],
        }
    }

    /// The bit arithmetic of [`MINIMAL_SPAT_SIZE_B`], asserted. A change in any preamble
    /// moves this number, which is the point of pinning it.
    #[test]
    fn a_minimal_spat_is_eleven_octets() {
        let spat = Spat::one(IntersectionState {
            id: IntersectionReferenceId::new(1),
            revision: 0,
            status: IntersectionStatus::NONE,
            moy: None,
            time_stamp: None,
            states: vec![MovementState::current(
                1,
                MovementEvent::phase(MovementPhaseState::StopAndRemain),
            )],
        });
        let encoded = encode_spat(&spat).expect("encodes");
        assert_eq!(encoded.size, MINIMAL_SPAT_SIZE_B);
        assert_eq!(encoded.bytes.len(), MINIMAL_SPAT_SIZE_B as usize);
        assert!(encoded.is_real());

        let framed = encode_message_frame(&spat).expect("frames");
        assert_eq!(framed.size, MINIMAL_SPAT_MESSAGE_FRAME_SIZE_B);
    }

    #[test]
    fn round_trip_is_exact() {
        let spat = nominal();
        let bytes = encode_spat(&spat).expect("encodes").bytes;
        assert_eq!(decode_spat(&bytes).expect("decodes"), spat);

        let framed = encode_message_frame(&spat).expect("frames").bytes;
        assert_eq!(decode_message_frame(&framed).expect("decodes"), spat);
    }

    #[test]
    fn every_optional_field_round_trips_present_and_absent() {
        let mut spat = nominal();
        spat.time_stamp = None;
        spat.intersections[0].moy = None;
        spat.intersections[0].time_stamp = None;
        spat.intersections[0].states[0].events[0].timing = Some(TimeChangeDetails {
            start_time: None,
            min_end_time: 1,
            max_end_time: None,
            likely_time: Some(2),
            confidence: Some(15),
            next_time: Some(36_000),
        });
        let bytes = encode_spat(&spat).expect("encodes").bytes;
        assert_eq!(decode_spat(&bytes).expect("decodes"), spat);
    }

    #[test]
    fn the_extremes_of_every_field_round_trip() {
        let spat = Spat {
            time_stamp: Some(MINUTE_OF_THE_YEAR_UNKNOWN),
            intersections: vec![IntersectionState {
                id: IntersectionReferenceId::in_region(
                    ROAD_REGULATOR_ID_MAX as u16,
                    INTERSECTION_ID_MAX as u16,
                ),
                revision: MSG_COUNT_MAX as u8,
                status: IntersectionStatus::MANUAL_CONTROL_IS_ENABLED
                    .with(IntersectionStatus::NO_VALID_SPAT_AVAILABLE),
                moy: Some(MINUTE_OF_THE_YEAR_UNKNOWN),
                time_stamp: Some(crate::j2735::bsm::D_SECOND_UNAVAILABLE),
                states: vec![MovementState {
                    signal_group: SIGNAL_GROUP_ID_MAX as u8,
                    events: vec![
                        MovementEvent::timed(
                            MovementPhaseState::CautionConflictingTraffic,
                            TimeChangeDetails {
                                start_time: Some(TIME_MARK_MIN as u16),
                                min_end_time: TIME_MARK_UNKNOWN,
                                max_end_time: Some(TIME_MARK_UNKNOWN),
                                likely_time: Some(TIME_MARK_TENTHS_PER_HOUR),
                                confidence: Some(TIME_INTERVAL_CONFIDENCE_MAX as u8),
                                next_time: Some(TIME_MARK_TENTHS_PER_HOUR),
                            },
                        );
                        MAX_MOVEMENT_EVENTS
                    ],
                }],
            }],
        };
        let bytes = encode_spat(&spat).expect("encodes");
        assert_eq!(decode_spat(&bytes.bytes).expect("decodes"), spat);
    }

    #[test]
    fn a_list_outside_its_size_constraint_is_refused_by_name() {
        let mut spat = nominal();
        spat.intersections.clear();
        let err = encode_spat(&spat).expect_err("SIZE(1..32) admits no empty list");
        assert!(err.to_string().contains("intersections"), "{err}");

        let mut spat = nominal();
        spat.intersections[0].states[0].events.clear();
        let err = encode_spat(&spat).expect_err("SIZE(1..16) admits no empty list");
        assert!(err.to_string().contains("state-time-speed"), "{err}");

        let mut spat = nominal();
        spat.intersections = vec![spat.intersections[0].clone(); MAX_INTERSECTION_STATES + 1];
        assert!(encode_spat(&spat).is_err());
    }

    /// Every element the codec does not model must be refused on decode, never stepped
    /// over. The bits below are hand-built with each refused preamble bit set in turn.
    #[test]
    fn an_unmodelled_element_is_refused_rather_than_skipped() {
        // SPAT.name: extension bit 0, then timeStamp=0, name=1, regional=0.
        let mut w = BitWriter::new();
        write_preamble(&mut w, true, &[false, true, false]);
        let bytes = w.into_bytes();
        let err = decode_spat(&bytes).expect_err("a name cannot be read");
        assert!(
            matches!(err, CodecError::UnsupportedConstruct { construct, .. } if construct == "SPAT.name"),
            "{err}"
        );

        // SPAT.regional, reached only after the intersection list decodes.
        let spat = Spat::one(IntersectionState {
            id: IntersectionReferenceId::new(1),
            revision: 0,
            status: IntersectionStatus::NONE,
            moy: None,
            time_stamp: None,
            states: vec![MovementState::current(
                1,
                MovementEvent::phase(MovementPhaseState::Dark),
            )],
        });
        let mut w = BitWriter::new();
        write_preamble(&mut w, true, &[false, false, true]);
        write_constrained_length(&mut w, F_INTERSECTIONS_LEN, 1, 1, MAX_INTERSECTION_STATES)
            .expect("length");
        write_intersection_state(&mut w, &spat.intersections[0]).expect("state");
        let bytes = w.into_bytes();
        let err = decode_spat(&bytes).expect_err("a regional extension cannot be read");
        assert!(
            matches!(err, CodecError::UnsupportedConstruct { construct, .. } if construct == "SPAT.regional"),
            "{err}"
        );
    }

    /// A set `SEQUENCE` extension bit is a message from a later edition of the standard and
    /// must stop the decode.
    #[test]
    fn an_extension_addition_stops_the_decode() {
        let mut w = BitWriter::new();
        w.write_bit(true); // SPAT's extension bit
        let bytes = w.into_bytes();
        let err = decode_spat(&bytes).expect_err("extension additions are not interpreted");
        assert!(
            matches!(err, CodecError::UnsupportedConstruct { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_frame_carrying_another_message_is_refused() {
        let spat = nominal();
        let framed = encode_message_frame(&spat).expect("frames").bytes;
        // The BSM decoder must not accept a SPaT frame, and vice versa.
        assert!(crate::j2735::bsm::decode_message_frame(&framed).is_err());
        let bsm = crate::j2735::bsm::encode_message_frame(
            &crate::j2735::bsm::BasicSafetyMessage::part_i(
                crate::j2735::bsm::BsmCoreData::unavailable([1, 2, 3, 4]),
            ),
        )
        .expect("frames")
        .bytes;
        assert!(decode_message_frame(&bsm).is_err());
    }

    #[test]
    fn trailing_data_is_refused() {
        let spat = nominal();
        let mut bytes = encode_spat(&spat).expect("encodes").bytes;
        bytes.push(0);
        assert!(decode_spat(&bytes).is_err());
    }

    #[test]
    fn minute_of_the_year_counts_from_the_first_of_january() {
        let clock = WallClock::parse_rfc3339("2026-01-01T00:00:00Z").expect("parses");
        assert_eq!(minute_of_the_year(clock, 0), 0);
        assert_eq!(minute_of_the_year(clock, 60 * 1_000_000_000), 1);
        // 2026 is not a leap year: 365 x 1440 = 525 600 minutes, so the last minute of the
        // year is 525 599 and the value stays inside the type.
        let clock = WallClock::parse_rfc3339("2026-12-31T23:59:00Z").expect("parses");
        assert_eq!(minute_of_the_year(clock, 0), 525_599);
        // A leap year reaches one day further and still fits below the unknown value.
        let clock = WallClock::parse_rfc3339("2028-12-31T23:59:00Z").expect("parses");
        assert_eq!(minute_of_the_year(clock, 0), 527_039);
        assert!(i64::from(minute_of_the_year(clock, 0)) < MINUTE_OF_THE_YEAR_MAX);
    }

    #[test]
    fn a_time_mark_is_tenths_of_a_second_and_says_when_it_does_not_know() {
        assert_eq!(time_mark(0.0), 0);
        assert_eq!(time_mark(1.0), 10);
        assert_eq!(time_mark(27.5), 275);
        assert_eq!(time_mark(3_600.0), TIME_MARK_TENTHS_PER_HOUR);
        assert_eq!(time_mark(3_600.1), TIME_MARK_UNKNOWN);
        assert_eq!(time_mark(-1.0), TIME_MARK_UNKNOWN);
        assert_eq!(time_mark(f64::NAN), TIME_MARK_UNKNOWN);
    }

    /// The disputed width, pinned as a test so the oracle run has something to contradict.
    #[test]
    fn the_movement_phase_state_index_is_four_bits_here() {
        assert!(MOVEMENT_PHASE_STATE_WIDTH_IS_DISPUTED);
        let mut w = BitWriter::new();
        write_enumerated(
            &mut w,
            F_EVENT_STATE,
            MovementPhaseState::CautionConflictingTraffic.index(),
            MOVEMENT_PHASE_STATE_COUNT,
        )
        .expect("encodes");
        assert_eq!(w.bit_len(), MOVEMENT_PHASE_STATE_BITS as usize);
    }

    #[test]
    fn every_phase_state_maps_back_from_its_index() {
        for index in 0..MOVEMENT_PHASE_STATE_COUNT {
            let state = MovementPhaseState::from_index(index).expect("in the root list");
            assert_eq!(state.index(), index);
            assert!(!state.as_str().is_empty());
        }
        assert!(MovementPhaseState::from_index(MOVEMENT_PHASE_STATE_COUNT).is_none());
    }
}
