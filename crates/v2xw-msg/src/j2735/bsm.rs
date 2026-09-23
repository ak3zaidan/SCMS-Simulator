//! The SAE J2735 Basic Safety Message: Part I in full, plus the Part II container the
//! simulator fills.
//!
//! # Why this is hand-written
//!
//! Build decision D2. `rasn-compiler` 0.16 generates Rust for all 43 SAE J2735 2024-09
//! modules and that Rust does not compile — 170 errors, essentially all from
//! `RegionalExtension {REG-EXT-ID-AND-TYPE : Set}`, a parameterised type over an
//! information object class. Deleting the 55 optional `regional` fields still leaves 105
//! errors. So the BSM is written by hand against the 2024-09 ASN.1, over the bit-level
//! engine in [`crate::j2735::uper`].
//!
//! The J2735 modules themselves are never committed (build decision D3: they carry an SAE
//! licence forbidding redistribution). What is committed is this: the constraints,
//! sentinels and field order as *code*, with each field's range in a doc comment beside it.
//!
//! # The shape of a BSM
//!
//! ```text
//! BasicSafetyMessage          extensible SEQUENCE, 2 optional fields
//! ├── coreData                BSMcoreData — 14 mandatory fields, 290 bits, always sent
//! ├── partII       OPTIONAL   1..8 containers, each an id and an open type
//! │   └── id 0               VehicleSafetyExtensions — events, path history,
//! │                          path prediction, exterior lights
//! └── regional     OPTIONAL   not implemented; refused rather than guessed
//! ```
//!
//! A Part I-only BSM is **37 octets**; inside a `MessageFrame` it is **40**. Both are
//! measured by [`encode_bsm`] and checked against the `pycrate` oracle, not estimated.
//!
//! # What is real and what is refused
//!
//! | Element | Status |
//! |---|---|
//! | `BSMcoreData`, every field | encoded and decoded |
//! | `VehicleSafetyExtensions`: `events`, `pathPrediction`, `lights` | encoded and decoded |
//! | `VehicleSafetyExtensions`: `pathHistory.crumbData`, `currGNSSstatus` | encoded and decoded |
//! | `PathHistory.initialPosition` (`FullPositionVector`) | [`CodecError::UnsupportedConstruct`] |
//! | Part II containers 1 (special vehicle) and 2 (supplemental vehicle) | carried verbatim as [`PartIIValue::Opaque`] |
//! | `BasicSafetyMessage.regional` | [`CodecError::UnsupportedConstruct`] |
//! | Any extension addition (a set extension bit) | [`CodecError::UnsupportedConstruct`] |
//! | A bit above a `BIT STRING`'s fixed size (`BrakeAppliedStatus`, `GNSSstatus`) | [`CodecError::OutOfRange`] |
//! | A bit above an extensible `BIT STRING`'s root (`VehicleEventFlags`, `ExteriorLights`) | [`CodecError::UnsupportedConstruct`] |
//! | The same `partII-Id` twice | [`CodecError::UnsupportedConstruct`], on **encode and decode alike** |
//! | An open type whose length determinant is zero (X.691 11.2.2) | [`CodecError::Decode`] naming the clause |
//!
//! The last two rows differ because the standards do: a bit above `SIZE(5)` is not a value
//! of `BrakeAppliedStatus` at all and is *out of range*, while a bit above the 13-bit root
//! of `VehicleEventFlags` is a legal value this codec cannot yet encode — the size
//! constraint's extension addition — and is *unsupported*.
//!
//! Nothing in that table is a silent approximation. A container this codec does not model
//! is either passed through byte for byte or refused by name — never re-encoded from a
//! guess, because a wrong bit in a PER encoding shifts every field after it.
//!
//! # Units are the standard's, not the simulator's
//!
//! Every field here holds the **wire integer**: a latitude in tenths of a microdegree, a
//! speed in 0.02 m/s, an acceleration in 0.01 m/s². [`BsmInput`] and [`build_bsm`] are the
//! one place simulator quantities are converted, and they quantise on the grids of build
//! decision D9 first, exactly as [`crate::units`] does for the ETSI side. The `unavailable`
//! sentinel each type declares is used for a missing quantity — never a plausible zero.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::Dims;
use v2xw_core::math;
use v2xw_core::time::{SimTime, WallClock};

use crate::codec::{Encoded, MsgType};
use crate::error::CodecError;
use crate::j2735::uper::{
    BitReader, BitWriter, Field, UperError, read_constrained_int, read_constrained_length,
    read_enumerated, read_extensible_bit_string, read_fixed_bit_string, read_fixed_octet_string,
    read_open_type, read_preamble, write_constrained_int, write_constrained_length,
    write_enumerated, write_extensible_bit_string, write_fixed_bit_string,
    write_fixed_octet_string, write_open_type, write_preamble,
};

// =========================================================================================
// Constants from the ASN.1
// =========================================================================================

/// `DSRCmsgID` of the BSM in a `MessageFrame`: `basicSafetyMessage ::= 20`.
pub const BSM_MESSAGE_ID: u16 = 20;

/// Upper bound of `DSRCmsgID`, `INTEGER (0..32767)`.
pub const DSRC_MSG_ID_MAX: u16 = 32_767;

/// `PartII-Id ::= 0` — `VehicleSafetyExtensions`, the only container this codec models.
pub const PART_II_VEHICLE_SAFETY: u8 = 0;

/// `PartII-Id ::= 1` — `SpecialVehicleExtensions`, carried opaquely.
pub const PART_II_SPECIAL_VEHICLE: u8 = 1;

/// `PartII-Id ::= 2` — `SupplementalVehicleExtensions`, carried opaquely.
pub const PART_II_SUPPLEMENTAL_VEHICLE: u8 = 2;

/// `PartII-Id ::= INTEGER (0..63)`.
pub const PART_II_ID_MAX: u8 = 63;

/// `partII SEQUENCE (SIZE(1..8)) OF PartIIcontent`.
pub const MAX_PART_II_CONTAINERS: usize = 8;

/// `PathHistoryPointList ::= SEQUENCE (SIZE(1..23)) OF PathHistoryPoint`.
pub const MAX_PATH_HISTORY_POINTS: usize = 23;

/// Size of a Part I-only BSM in octets: 3 preamble bits plus 290 bits of `BSMcoreData`,
/// padded to an octet boundary.
///
/// Asserted by a test rather than assumed, and cross-checked against `pycrate`.
pub const PART_I_ONLY_SIZE_B: u32 = 37;

/// Size of a Part I-only BSM inside a `MessageFrame`, in octets.
pub const PART_I_ONLY_MESSAGE_FRAME_SIZE_B: u32 = 40;

/// Standard gravity, m/s², used only to convert a vertical acceleration into the `0.02 G`
/// steps `VerticalAcceleration` is declared in. Exact by definition (CGPM 1901).
pub const STANDARD_GRAVITY_MPS2: f64 = 9.806_65;

// -- BSMcoreData field ranges and sentinels ----------------------------------------------
// Each pair below is one ASN.1 type's constraint. They are consts rather than literals at
// the call site because the encoder, the decoder, the range check and the builder's clamp
// must agree by construction; three of the four agreeing is the classic asymmetry defect.

/// `MsgCount ::= INTEGER (0..127)`.
pub const MSG_COUNT_MIN: i64 = 0;
/// See [`MSG_COUNT_MIN`].
pub const MSG_COUNT_MAX: i64 = 127;

/// `DSecond ::= INTEGER (0..65535)`, milliseconds.
///
/// J2735 fills `secMark` with the millisecond within the current UTC minute, so a valid
/// reading is `0..=59_999`; the ASN.1 admits the whole 16-bit range and the top value is
/// what an ITS-S sends when it has no time. Only the ASN.1 range is enforced here, because
/// enforcing the tighter one would reject conformant messages from other stacks.
pub const D_SECOND_MIN: i64 = 0;
/// See [`D_SECOND_MIN`].
pub const D_SECOND_MAX: i64 = 65_535;
/// `secMark` value meaning "no time available".
pub const D_SECOND_UNAVAILABLE: u16 = 65_535;
/// Milliseconds in the minute `secMark` counts within.
pub const MS_PER_MINUTE: u64 = 60_000;

/// `Latitude ::= INTEGER (-900000000..900000001)`, LSB 1/10 microdegree.
pub const LATITUDE_MIN: i64 = -900_000_000;
/// See [`LATITUDE_MIN`].
pub const LATITUDE_MAX: i64 = 900_000_001;
/// `Latitude` value meaning "unavailable": the one value above ±90°.
pub const LATITUDE_UNAVAILABLE: i32 = 900_000_001;

/// `Longitude ::= INTEGER (-1799999999..1800000001)`, LSB 1/10 microdegree.
///
/// The lower bound is **not** −1 800 000 000. The range is asymmetric in the standard, and
/// an encoder that assumed symmetry would offset every longitude by one LSB and produce a
/// 32-bit field whose every value is wrong by 11 mm — a defect no round-trip test of its
/// own encoder could ever find. This is exactly what the `pycrate` oracle is for.
pub const LONGITUDE_MIN: i64 = -1_799_999_999;
// REGRESSION GUARD, 2026-09-22. This constant was changed to -1_800_000_000 during a
// later repair wave, against the warning in the doc comment immediately above, which had
// already named the exact consequence. Every round-trip test still passed, because an
// encoder and a decoder sharing a wrong lower bound agree with each other perfectly. It
// was caught only by the hand-pinned, oracle-validated byte vectors in this module's
// tests. Do not "fix" the asymmetry: -180.0000000 and +180.0000000 are the same meridian
// and the standard excludes one of them.
/// See [`LONGITUDE_MIN`].
pub const LONGITUDE_MAX: i64 = 1_800_000_001;
/// `Longitude` value meaning "unavailable".
pub const LONGITUDE_UNAVAILABLE: i32 = 1_800_000_001;

/// `Elevation ::= INTEGER (-4096..61439)`, LSB 10 cm above or below the reference
/// ellipsoid, so −409.6 m to +6 143.9 m.
pub const ELEVATION_MIN: i64 = -4_096;
/// See [`ELEVATION_MIN`].
pub const ELEVATION_MAX: i64 = 61_439;
/// `Elevation` value meaning "unknown"; also the bottom of the range.
pub const ELEVATION_UNKNOWN: i32 = -4_096;

/// `SemiMajorAxisAccuracy` / `SemiMinorAxisAccuracy ::= INTEGER (0..255)`, LSB 0.05 m.
pub const SEMI_AXIS_MIN: i64 = 0;
/// See [`SEMI_AXIS_MIN`].
pub const SEMI_AXIS_MAX: i64 = 255;
/// Semi-axis value meaning "12.70 m or more".
pub const SEMI_AXIS_OUT_OF_RANGE: u8 = 254;
/// Semi-axis value meaning "unavailable".
pub const SEMI_AXIS_UNAVAILABLE: u8 = 255;

/// `SemiMajorAxisOrientation ::= INTEGER (0..65535)`, LSB 360/65535 degree from true
/// north.
pub const ORIENTATION_MIN: i64 = 0;
/// See [`ORIENTATION_MIN`].
pub const ORIENTATION_MAX: i64 = 65_535;
/// Orientation value meaning "unavailable"; 65534 is the largest bearing expressible.
pub const ORIENTATION_UNAVAILABLE: u16 = 65_535;

/// `Speed ::= INTEGER (0..8191)`, LSB 0.02 m/s.
pub const SPEED_MIN: i64 = 0;
/// See [`SPEED_MIN`].
pub const SPEED_MAX: i64 = 8_191;
/// `Speed` value meaning "unavailable"; 8190 is 163.8 m/s.
pub const SPEED_UNAVAILABLE: u16 = 8_191;

/// `Heading ::= INTEGER (0..28800)`, LSB 0.0125 degree clockwise from true north.
///
/// The type's own comment gives the meaningful span as 0 to 359.9875°, which is
/// `0..=28799`; the extra top value carries no bearing and is what an ITS-S with no heading
/// sends.
pub const HEADING_MIN: i64 = 0;
/// See [`HEADING_MIN`].
pub const HEADING_MAX: i64 = 28_800;
/// `Heading` value meaning "unavailable".
pub const HEADING_UNAVAILABLE: u16 = 28_800;

/// `SteeringWheelAngle ::= INTEGER (-126..127)`, LSB 1.5 degree.
pub const STEERING_WHEEL_ANGLE_MIN: i64 = -126;
/// See [`STEERING_WHEEL_ANGLE_MIN`].
pub const STEERING_WHEEL_ANGLE_MAX: i64 = 127;
/// `SteeringWheelAngle` value meaning "unavailable"; ±126 saturate at ±189°.
pub const STEERING_WHEEL_ANGLE_UNAVAILABLE: i8 = 127;

/// `Acceleration ::= INTEGER (-2000..2001)`, LSB 0.01 m/s².
pub const ACCELERATION_MIN: i64 = -2_000;
/// See [`ACCELERATION_MIN`].
pub const ACCELERATION_MAX: i64 = 2_001;
/// `Acceleration` value meaning "unavailable"; ±2000 saturate at ±20 m/s².
pub const ACCELERATION_UNAVAILABLE: i16 = 2_001;

/// `VerticalAcceleration ::= INTEGER (-127..127)`, LSB 0.02 G.
pub const VERTICAL_ACCELERATION_MIN: i64 = -127;
/// See [`VERTICAL_ACCELERATION_MIN`].
pub const VERTICAL_ACCELERATION_MAX: i64 = 127;
/// `VerticalAcceleration` value meaning "unavailable" — the *bottom* of the range, not the
/// top, unlike every other sentinel in Part I.
pub const VERTICAL_ACCELERATION_UNAVAILABLE: i8 = -127;

/// `YawRate ::= INTEGER (-32767..32767)`, LSB 0.01 degree per second.
///
/// The only Part I dynamics field with no `unavailable` value: the type declares none, so a
/// missing yaw rate has to be sent as zero and cannot be distinguished from a genuine zero.
/// That is a property of the standard, recorded in the model card as a limitation.
pub const YAW_RATE_MIN: i64 = -32_767;
/// See [`YAW_RATE_MIN`].
pub const YAW_RATE_MAX: i64 = 32_767;

/// `VehicleWidth ::= INTEGER (0..1023)`, LSB 1 cm.
pub const VEHICLE_WIDTH_MIN: i64 = 0;
/// See [`VEHICLE_WIDTH_MIN`].
pub const VEHICLE_WIDTH_MAX: i64 = 1_023;

/// `VehicleLength ::= INTEGER (0..4095)`, LSB 1 cm.
pub const VEHICLE_LENGTH_MIN: i64 = 0;
/// See [`VEHICLE_LENGTH_MIN`].
pub const VEHICLE_LENGTH_MAX: i64 = 4_095;

// -- Part II field ranges ----------------------------------------------------------------

/// `OffsetLL-B18 ::= INTEGER (-131072..131071)`, LSB 0.1 microdegree.
pub const OFFSET_LL_B18_MIN: i64 = -131_072;
/// See [`OFFSET_LL_B18_MIN`].
pub const OFFSET_LL_B18_MAX: i64 = 131_071;
/// `OffsetLL-B18` value meaning "unknown"; ±131071 saturate.
pub const OFFSET_LL_B18_UNKNOWN: i32 = -131_072;

/// `VertOffset-B12 ::= INTEGER (-2048..2047)`, LSB 10 cm.
pub const VERT_OFFSET_B12_MIN: i64 = -2_048;
/// See [`VERT_OFFSET_B12_MIN`].
pub const VERT_OFFSET_B12_MAX: i64 = 2_047;
/// `VertOffset-B12` value meaning "unavailable".
pub const VERT_OFFSET_B12_UNAVAILABLE: i16 = -2_048;

/// `TimeOffset ::= INTEGER (1..65535)`, LSB 10 ms. Note the lower bound is one, not zero.
pub const TIME_OFFSET_MIN: i64 = 1;
/// See [`TIME_OFFSET_MIN`].
pub const TIME_OFFSET_MAX: i64 = 65_535;
/// `TimeOffset` value meaning "unavailable"; 65534 saturates at 655.34 s.
pub const TIME_OFFSET_UNAVAILABLE: u16 = 65_535;

/// `CoarseHeading ::= INTEGER (0..240)`, LSB 1.5 degree.
pub const COARSE_HEADING_MIN: i64 = 0;
/// See [`COARSE_HEADING_MIN`].
pub const COARSE_HEADING_MAX: i64 = 240;
/// `CoarseHeading` value meaning "unavailable".
pub const COARSE_HEADING_UNAVAILABLE: u8 = 240;

/// `RadiusOfCurvature ::= INTEGER (-32767..32767)`, LSB 10 cm; 32767 means "straight".
pub const RADIUS_OF_CURVATURE_MIN: i64 = -32_767;
/// See [`RADIUS_OF_CURVATURE_MIN`].
pub const RADIUS_OF_CURVATURE_MAX: i64 = 32_767;
/// `RadiusOfCurvature` value meaning "a straight path".
pub const RADIUS_OF_CURVATURE_STRAIGHT: i16 = 32_767;

/// `Confidence ::= INTEGER (0..200)`, LSB 0.5 %.
pub const CONFIDENCE_MIN: i64 = 0;
/// See [`CONFIDENCE_MIN`].
pub const CONFIDENCE_MAX: i64 = 200;

/// Root length of `VehicleEventFlags ::= BIT STRING (SIZE(13, ..., 14))`.
pub const VEHICLE_EVENT_FLAGS_BITS: u32 = 13;
/// Root length of `ExteriorLights ::= BIT STRING (SIZE(9, ...))`.
pub const EXTERIOR_LIGHTS_BITS: u32 = 9;
/// Length of `GNSSstatus ::= BIT STRING (SIZE(8))`, which is not extensible.
pub const GNSS_STATUS_BITS: u32 = 8;
/// Length of `BrakeAppliedStatus ::= BIT STRING (SIZE(5))`, which is not extensible.
pub const BRAKE_APPLIED_STATUS_BITS: u32 = 5;

// =========================================================================================
// Enumerations
// =========================================================================================

/// `TransmissionState ::= ENUMERATED` — the gear selector, 8 values, 3 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum TransmissionState {
    /// `neutral(0)`.
    Neutral,
    /// `park(1)`.
    Park,
    /// `forwardGears(2)`.
    ForwardGears,
    /// `reverseGears(3)`.
    ReverseGears,
    /// `reserved1(4)`.
    Reserved1,
    /// `reserved2(5)`.
    Reserved2,
    /// `reserved3(6)`.
    Reserved3,
    /// `unavailable(7)` — not equipped, or no reading. The default: a model that has not
    /// said which gear it is in must not claim neutral.
    #[default]
    Unavailable,
}

impl TransmissionState {
    /// Values in the root enumeration.
    pub const COUNT: u64 = 8;

    /// The PER index, which for this type equals the ASN.1 number.
    pub const fn index(self) -> u64 {
        self as u64
    }

    /// The value at a PER index.
    pub const fn from_index(index: u64) -> Option<Self> {
        Some(match index {
            0 => Self::Neutral,
            1 => Self::Park,
            2 => Self::ForwardGears,
            3 => Self::ReverseGears,
            4 => Self::Reserved1,
            5 => Self::Reserved2,
            6 => Self::Reserved3,
            7 => Self::Unavailable,
            _ => return None,
        })
    }
}

/// `AntiLockBrakeStatus`, `TractionControlStatus` and `StabilityControlStatus` all share
/// this shape: 4 values, 2 bits, `unavailable` first.
///
/// One type for three ASN.1 types because their value sets are identical and a separate
/// three-variant copy of the same enumeration adds nothing but three more `from_index`
/// functions to keep in step. [`AuxiliaryBrakeStatus`] is *not* folded in: its fourth value
/// is `reserved`, not `engaged`, and pretending otherwise would let a caller send
/// "engaged" for a brake that has no such state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ControlStatus {
    /// `unavailable(0)` — not equipped, or no reading.
    #[default]
    Unavailable,
    /// `off(1)`.
    Off,
    /// `on(2)` — equipped and active, but not intervening.
    On,
    /// `engaged(3)` — intervening now.
    Engaged,
}

impl ControlStatus {
    /// Values in the root enumeration.
    pub const COUNT: u64 = 4;

    /// The PER index.
    pub const fn index(self) -> u64 {
        self as u64
    }

    /// The value at a PER index.
    pub const fn from_index(index: u64) -> Option<Self> {
        Some(match index {
            0 => Self::Unavailable,
            1 => Self::Off,
            2 => Self::On,
            3 => Self::Engaged,
            _ => return None,
        })
    }
}

/// `BrakeBoostApplied ::= ENUMERATED` — 3 values, 2 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum BrakeBoostApplied {
    /// `unavailable(0)` — not equipped, or no reading.
    #[default]
    Unavailable,
    /// `off(1)`.
    Off,
    /// `on(2)` — brake boost applied.
    On,
}

impl BrakeBoostApplied {
    /// Values in the root enumeration. Three values still take 2 bits, and index 3 is
    /// invalid — a decoder that treated the width as the value count would accept it.
    pub const COUNT: u64 = 3;

    /// The PER index.
    pub const fn index(self) -> u64 {
        self as u64
    }

    /// The value at a PER index.
    pub const fn from_index(index: u64) -> Option<Self> {
        Some(match index {
            0 => Self::Unavailable,
            1 => Self::Off,
            2 => Self::On,
            _ => return None,
        })
    }
}

/// `AuxiliaryBrakeStatus ::= ENUMERATED` — 4 values, 2 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum AuxiliaryBrakeStatus {
    /// `unavailable(0)` — not equipped, or no reading.
    #[default]
    Unavailable,
    /// `off(1)`.
    Off,
    /// `on(2)` — auxiliary brakes engaged.
    On,
    /// `reserved(3)`.
    Reserved,
}

impl AuxiliaryBrakeStatus {
    /// Values in the root enumeration.
    pub const COUNT: u64 = 4;

    /// The PER index.
    pub const fn index(self) -> u64 {
        self as u64
    }

    /// The value at a PER index.
    pub const fn from_index(index: u64) -> Option<Self> {
        Some(match index {
            0 => Self::Unavailable,
            1 => Self::Off,
            2 => Self::On,
            3 => Self::Reserved,
            _ => return None,
        })
    }
}

// =========================================================================================
// Bit-string flag sets
// =========================================================================================

/// `BrakeAppliedStatus ::= BIT STRING (SIZE(5))` — which wheels have brakes applied.
///
/// Held right-aligned, bit `(0)` of the ASN.1 string in the most significant of the five
/// used bits, which is the convention [`crate::j2735::uper::write_fixed_bit_string`]
/// documents. The same convention as [`crate::cam::ExteriorLightMask`], for the same
/// reason: a mask a caller can `|` together reads better than five booleans.
///
/// The newtype is a `u8` and the string is five bits, so a value with bit 5, 6 or 7 set is
/// constructible and is not a `BrakeAppliedStatus`. The encoder refuses it
/// ([`CodecError::OutOfRange`]) rather than dropping the bit: the size constraint has no
/// extension marker, so unlike [`VehicleEventFlags`] there is no wider encoding to reach
/// for, and a truncated write would emit a canonical message claiming different brakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BrakeAppliedStatus(pub u8);

impl BrakeAppliedStatus {
    /// No bit set: brakes known to be off on every wheel.
    ///
    /// Not the same as [`BrakeAppliedStatus::UNAVAILABLE`], which says the status is not
    /// known — the distinction the `unavailable(0)` bit exists to make.
    pub const NONE: Self = Self(0);
    /// `unavailable(0)`: the brake applied status is not available.
    pub const UNAVAILABLE: Self = Self(0b1_0000);
    /// `leftFront(1)`.
    pub const LEFT_FRONT: Self = Self(0b0_1000);
    /// `leftRear(2)`.
    pub const LEFT_REAR: Self = Self(0b0_0100);
    /// `rightFront(3)`.
    pub const RIGHT_FRONT: Self = Self(0b0_0010);
    /// `rightRear(4)`.
    pub const RIGHT_REAR: Self = Self(0b0_0001);
    /// Every wheel.
    pub const ALL_WHEELS: Self = Self(0b0_1111);

    /// The union of two masks.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every bit of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// `VehicleEventFlags ::= BIT STRING (SIZE(13, ..., 14))` — the Part II event bits.
///
/// The root is 13 bits; `eventJackKnife(13)` is a 14th bit reachable only through the size
/// constraint's extension and is not encoded here. This is why the type is not just a
/// `u16`: the length is part of the encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct VehicleEventFlags(pub u16);

impl VehicleEventFlags {
    /// No event.
    pub const NONE: Self = Self(0);
    /// `eventHazardLights(0)`.
    pub const HAZARD_LIGHTS: Self = Self(1 << 12);
    /// `eventStopLineViolation(1)`.
    pub const STOP_LINE_VIOLATION: Self = Self(1 << 11);
    /// `eventABSactivated(2)`.
    pub const ABS_ACTIVATED: Self = Self(1 << 10);
    /// `eventTractionControlLoss(3)`.
    pub const TRACTION_CONTROL_LOSS: Self = Self(1 << 9);
    /// `eventStabilityControlactivated(4)`.
    pub const STABILITY_CONTROL_ACTIVATED: Self = Self(1 << 8);
    /// `eventHazardousMaterials(5)`.
    pub const HAZARDOUS_MATERIALS: Self = Self(1 << 7);
    /// `eventReserved1(6)`.
    pub const RESERVED1: Self = Self(1 << 6);
    /// `eventHardBraking(7)` — what [`crate::generator::AppEventKind::HardBraking`] sets.
    pub const HARD_BRAKING: Self = Self(1 << 5);
    /// `eventLightsChanged(8)`.
    pub const LIGHTS_CHANGED: Self = Self(1 << 4);
    /// `eventWipersChanged(9)`.
    pub const WIPERS_CHANGED: Self = Self(1 << 3);
    /// `eventFlatTire(10)`.
    pub const FLAT_TIRE: Self = Self(1 << 2);
    /// `eventDisabledVehicle(11)`.
    pub const DISABLED_VEHICLE: Self = Self(1 << 1);
    /// `eventAirBagDeployment(12)`.
    pub const AIR_BAG_DEPLOYMENT: Self = Self(1);

    /// Largest value the 13-bit root can hold.
    pub const ROOT_MASK: u16 = (1 << VEHICLE_EVENT_FLAGS_BITS) - 1;

    /// The union of two masks.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every bit of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when no bit outside the 13-bit root is set.
    pub const fn fits_root(self) -> bool {
        self.0 & !Self::ROOT_MASK == 0
    }
}

/// `ExteriorLights ::= BIT STRING (SIZE(9, ...))` — Part II light state.
///
/// A separate type from [`crate::cam::ExteriorLightMask`], which is the ETSI CDD's
/// `ExteriorLights`: the two have different bit orders (`leftTurnSignalOn` is bit 2 here
/// and bit 4 there) and different lengths. Sharing one mask between them would be a
/// silent mis-encoding on one side of the Atlantic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ExteriorLights(pub u16);

impl ExteriorLights {
    /// All lights off, which J2735 states as no bit set.
    pub const NONE: Self = Self(0);
    /// `lowBeamHeadlightsOn(0)`.
    pub const LOW_BEAM: Self = Self(1 << 8);
    /// `highBeamHeadlightsOn(1)`.
    pub const HIGH_BEAM: Self = Self(1 << 7);
    /// `leftTurnSignalOn(2)`.
    pub const LEFT_TURN: Self = Self(1 << 6);
    /// `rightTurnSignalOn(3)`.
    pub const RIGHT_TURN: Self = Self(1 << 5);
    /// `hazardSignalOn(4)`.
    pub const HAZARD: Self = Self(1 << 4);
    /// `automaticLightControlOn(5)`.
    pub const AUTOMATIC_LIGHT_CONTROL: Self = Self(1 << 3);
    /// `daytimeRunningLightsOn(6)`.
    pub const DAYTIME_RUNNING: Self = Self(1 << 2);
    /// `fogLightOn(7)`.
    pub const FOG: Self = Self(1 << 1);
    /// `parkingLightsOn(8)`.
    pub const PARKING: Self = Self(1);

    /// Largest value the 9-bit root can hold.
    pub const ROOT_MASK: u16 = (1 << EXTERIOR_LIGHTS_BITS) - 1;

    /// The union of two masks.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every bit of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when no bit outside the 9-bit root is set.
    pub const fn fits_root(self) -> bool {
        self.0 & !Self::ROOT_MASK == 0
    }
}

/// `GNSSstatus ::= BIT STRING (SIZE(8))` — receiver health, in `PathHistory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct GnssStatus(pub u8);

impl GnssStatus {
    /// `unavailable(0)`: not equipped, or no status.
    pub const UNAVAILABLE: Self = Self(1 << 7);
    /// `isHealthy(1)`.
    pub const IS_HEALTHY: Self = Self(1 << 6);
    /// `isMonitored(2)`.
    pub const IS_MONITORED: Self = Self(1 << 5);
    /// `baseStationType(3)`: clear for a rover or moving base, set for a fixed base.
    pub const BASE_STATION_TYPE: Self = Self(1 << 4);
    /// `aPDOPofUnder5(4)`.
    pub const PDOP_UNDER_5: Self = Self(1 << 3);
    /// `inViewOfUnder5(5)`: fewer than five satellites in view.
    pub const IN_VIEW_OF_UNDER_5: Self = Self(1 << 2);
    /// `localCorrectionsPresent(6)`: DGPS-type corrections in use.
    pub const LOCAL_CORRECTIONS: Self = Self(1 << 1);
    /// `networkCorrectionsPresent(7)`: RTK-type corrections in use.
    pub const NETWORK_CORRECTIONS: Self = Self(1);

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
// Part I structures
// =========================================================================================

/// `PositionalAccuracy ::= SEQUENCE` — the GNSS error ellipse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionalAccuracy {
    /// `semiMajor SemiMajorAxisAccuracy`, LSB 0.05 m, `254` = ≥ 12.70 m, `255` =
    /// unavailable.
    pub semi_major: u8,
    /// `semiMinor SemiMinorAxisAccuracy`, same units and sentinels.
    pub semi_minor: u8,
    /// `orientation SemiMajorAxisOrientation`, LSB 360/65535 degree from true north,
    /// `65535` = unavailable.
    pub orientation: u16,
}

impl PositionalAccuracy {
    /// Every element at its `unavailable` sentinel — what a node with no fix sends.
    pub const UNAVAILABLE: Self = Self {
        semi_major: SEMI_AXIS_UNAVAILABLE,
        semi_minor: SEMI_AXIS_UNAVAILABLE,
        orientation: ORIENTATION_UNAVAILABLE,
    };
}

impl Default for PositionalAccuracy {
    fn default() -> Self {
        Self::UNAVAILABLE
    }
}

/// `AccelerationSet4Way ::= SEQUENCE` — three axes and the yaw rate.
///
/// Field names are the ASN.1's, and so are the units: this is a wire structure, not a
/// physics one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccelerationSet4Way {
    /// `long Acceleration`, along the vehicle's longitudinal axis, LSB 0.01 m/s²,
    /// `2001` = unavailable.
    pub long: i16,
    /// `lat Acceleration`, along the lateral axis, same units.
    pub lat: i16,
    /// `vert VerticalAcceleration`, LSB 0.02 G, `-127` = unavailable.
    pub vert: i8,
    /// `yaw YawRate`, LSB 0.01 degree per second. No `unavailable` value exists.
    pub yaw: i16,
}

impl AccelerationSet4Way {
    /// Every element that has an `unavailable` sentinel set to it; yaw rate, which has
    /// none, set to zero.
    pub const UNAVAILABLE: Self = Self {
        long: ACCELERATION_UNAVAILABLE,
        lat: ACCELERATION_UNAVAILABLE,
        vert: VERTICAL_ACCELERATION_UNAVAILABLE,
        yaw: 0,
    };
}

impl Default for AccelerationSet4Way {
    fn default() -> Self {
        Self::UNAVAILABLE
    }
}

/// `BrakeSystemStatus ::= SEQUENCE` — the bit-packed brake state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BrakeSystemStatus {
    /// `wheelBrakes BrakeAppliedStatus`.
    pub wheel_brakes: BrakeAppliedStatus,
    /// `traction TractionControlStatus`.
    pub traction: ControlStatus,
    /// `abs AntiLockBrakeStatus`.
    pub abs: ControlStatus,
    /// `scs StabilityControlStatus`.
    pub scs: ControlStatus,
    /// `brakeBoost BrakeBoostApplied`.
    pub brake_boost: BrakeBoostApplied,
    /// `auxBrakes AuxiliaryBrakeStatus`.
    pub aux_brakes: AuxiliaryBrakeStatus,
}

impl BrakeSystemStatus {
    /// Nothing known: the `unavailable` bit set and every enumeration at `unavailable`.
    pub const UNAVAILABLE: Self = Self {
        wheel_brakes: BrakeAppliedStatus::UNAVAILABLE,
        traction: ControlStatus::Unavailable,
        abs: ControlStatus::Unavailable,
        scs: ControlStatus::Unavailable,
        brake_boost: BrakeBoostApplied::Unavailable,
        aux_brakes: AuxiliaryBrakeStatus::Unavailable,
    };
}

/// `VehicleSize ::= SEQUENCE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VehicleSize {
    /// `width VehicleWidth`, LSB 1 cm, `0..=1023`.
    pub width: u16,
    /// `length VehicleLength`, LSB 1 cm, `0..=4095`.
    pub length: u16,
}

/// `BSMcoreData ::= SEQUENCE` — Part I, sent with every BSM.
///
/// Fourteen mandatory fields, no `OPTIONAL` and no extension marker, so the encoding has
/// **no preamble at all** and is exactly 290 bits. Field order below is the ASN.1's and is
/// load-bearing: PER carries no tags, so a swapped pair of same-width fields produces a
/// perfectly decodable message with the values in the wrong places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BsmCoreData {
    /// `msgCnt MsgCount`, `0..=127`, incremented per message and wrapping.
    pub msg_cnt: u8,
    /// `id TemporaryID`, `OCTET STRING (SIZE(4))` — the rotating pseudonym identifier.
    pub id: [u8; 4],
    /// `secMark DSecond`, milliseconds within the minute, `65535` = unavailable.
    pub sec_mark: u16,
    /// `lat Latitude`, LSB 1/10 microdegree, `900000001` = unavailable.
    pub lat: i32,
    /// `long Longitude`, LSB 1/10 microdegree, `1800000001` = unavailable. Lower bound
    /// `-1799999999`, see [`LONGITUDE_MIN`].
    pub lon: i32,
    /// `elev Elevation`, LSB 10 cm, `-4096` = unknown.
    pub elev: i32,
    /// `accuracy PositionalAccuracy`.
    pub accuracy: PositionalAccuracy,
    /// `transmission TransmissionState`.
    pub transmission: TransmissionState,
    /// `speed Speed`, LSB 0.02 m/s, `8191` = unavailable.
    pub speed: u16,
    /// `heading Heading`, LSB 0.0125 degree clockwise from true north, `28800` =
    /// unavailable.
    pub heading: u16,
    /// `angle SteeringWheelAngle`, LSB 1.5 degree, `127` = unavailable.
    pub angle: i8,
    /// `accelSet AccelerationSet4Way`.
    pub accel_set: AccelerationSet4Way,
    /// `brakes BrakeSystemStatus`.
    pub brakes: BrakeSystemStatus,
    /// `size VehicleSize`.
    pub size: VehicleSize,
}

impl BsmCoreData {
    /// Bits `BSMcoreData` occupies. Fixed, because the structure has no optional field and
    /// no extension marker.
    pub const ENCODED_BITS: usize = 290;

    /// A core data with every sentinel-bearing field at `unavailable` and a given
    /// identifier.
    ///
    /// The honest starting point for a builder: a zero-filled `BSMcoreData` would claim a
    /// vehicle at Null Island, stationary, in neutral, with working brakes.
    pub fn unavailable(id: [u8; 4]) -> Self {
        Self {
            msg_cnt: 0,
            id,
            sec_mark: D_SECOND_UNAVAILABLE,
            lat: LATITUDE_UNAVAILABLE,
            lon: LONGITUDE_UNAVAILABLE,
            elev: ELEVATION_UNKNOWN,
            accuracy: PositionalAccuracy::UNAVAILABLE,
            transmission: TransmissionState::Unavailable,
            speed: SPEED_UNAVAILABLE,
            heading: HEADING_UNAVAILABLE,
            angle: STEERING_WHEEL_ANGLE_UNAVAILABLE,
            accel_set: AccelerationSet4Way::UNAVAILABLE,
            brakes: BrakeSystemStatus::UNAVAILABLE,
            size: VehicleSize::default(),
        }
    }
}

// =========================================================================================
// Part II structures
// =========================================================================================

/// `PathHistoryPoint ::= SEQUENCE` — one breadcrumb, relative to the current position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathHistoryPoint {
    /// `latOffset OffsetLL-B18`, LSB 0.1 microdegree, `-131072` = unknown.
    pub lat_offset: i32,
    /// `lonOffset OffsetLL-B18`, same units.
    pub lon_offset: i32,
    /// `elevationOffset VertOffset-B12`, LSB 10 cm, `-2048` = unavailable.
    pub elevation_offset: i16,
    /// `timeOffset TimeOffset`, LSB 10 ms backwards in time, `1..=65535`, `65535` =
    /// unavailable. **Zero is not a legal value.**
    pub time_offset: u16,
    /// `speed Speed OPTIONAL`, LSB 0.02 m/s.
    pub speed: Option<u16>,
    /// `posAccuracy PositionalAccuracy OPTIONAL`.
    pub pos_accuracy: Option<PositionalAccuracy>,
    /// `heading CoarseHeading OPTIONAL`, LSB 1.5 degree, `240` = unavailable.
    pub heading: Option<u8>,
}

impl PathHistoryPoint {
    /// A point with only the four mandatory offsets.
    pub const fn new(
        lat_offset: i32,
        lon_offset: i32,
        elevation_offset: i16,
        time_offset: u16,
    ) -> Self {
        Self {
            lat_offset,
            lon_offset,
            elevation_offset,
            time_offset,
            speed: None,
            pos_accuracy: None,
            heading: None,
        }
    }
}

/// `PathHistory ::= SEQUENCE` — where the vehicle has been.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathHistory {
    /// `currGNSSstatus GNSSstatus OPTIONAL`.
    pub gnss_status: Option<GnssStatus>,
    /// `crumbData PathHistoryPointList`, `1..=23` points, mandatory.
    pub crumb_data: Vec<PathHistoryPoint>,
}

impl PathHistory {
    /// A path history over a list of points.
    pub fn new(crumb_data: Vec<PathHistoryPoint>) -> Self {
        Self {
            gnss_status: None,
            crumb_data,
        }
    }
}

/// `PathPrediction ::= SEQUENCE` — where it is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathPrediction {
    /// `radiusOfCurve RadiusOfCurvature`, LSB 10 cm, `32767` = straight.
    pub radius_of_curve: i16,
    /// `confidence Confidence`, LSB 0.5 %.
    pub confidence: u8,
}

impl PathPrediction {
    /// A straight path at a given confidence.
    pub const fn straight(confidence: u8) -> Self {
        Self {
            radius_of_curve: RADIUS_OF_CURVATURE_STRAIGHT,
            confidence,
        }
    }
}

/// `VehicleSafetyExtensions ::= SEQUENCE` — Part II container id 0.
///
/// Every field is `OPTIONAL`, and the container itself is only sent when the generation
/// rules ask for it (J2945/1 sends path history and prediction on every message, events
/// only when one fires).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VehicleSafetyExtensions {
    /// `events VehicleEventFlags OPTIONAL`.
    pub events: Option<VehicleEventFlags>,
    /// `pathHistory PathHistory OPTIONAL`.
    pub path_history: Option<PathHistory>,
    /// `pathPrediction PathPrediction OPTIONAL`.
    pub path_prediction: Option<PathPrediction>,
    /// `lights ExteriorLights OPTIONAL`.
    pub lights: Option<ExteriorLights>,
}

impl VehicleSafetyExtensions {
    /// True when no field is set, in which case the container should not be sent at all: an
    /// all-absent `VehicleSafetyExtensions` is five bits that say nothing.
    pub const fn is_empty(&self) -> bool {
        self.events.is_none()
            && self.path_history.is_none()
            && self.path_prediction.is_none()
            && self.lights.is_none()
    }
}

/// What a Part II container holds.
///
/// The `Opaque` variant is what makes this codec safe on real captures. `partII-Value` is
/// an ASN.1 open type — a length-prefixed, octet-padded island inside the encoding — so a
/// container this codec does not model can be carried through verbatim without any risk of
/// desynchronising the fields around it. That is strictly better than refusing the whole
/// message, and strictly better than guessing at its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PartIIValue {
    /// Container id 0, modelled field by field.
    VehicleSafety(VehicleSafetyExtensions),
    /// A container this codec does not model, held as the open type's octets.
    ///
    /// Re-encoded byte for byte. The octets are the *complete* inner encoding including its
    /// clause 11.1 padding, which is what makes the round trip exact.
    Opaque(Vec<u8>),
}

/// `PartIIcontent ::= SEQUENCE { partII-Id, partII-Value }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartIIContent {
    /// `partII-Id PartII-Id`, `0..=63`.
    pub id: u8,
    /// `partII-Value`, the open type.
    pub value: PartIIValue,
}

impl PartIIContent {
    /// The `VehicleSafetyExtensions` container, id 0.
    pub fn vehicle_safety(value: VehicleSafetyExtensions) -> Self {
        Self {
            id: PART_II_VEHICLE_SAFETY,
            value: PartIIValue::VehicleSafety(value),
        }
    }
}

/// `BasicSafetyMessage ::= SEQUENCE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicSafetyMessage {
    /// `coreData BSMcoreData`.
    pub core: BsmCoreData,
    /// `partII SEQUENCE (SIZE(1..8)) OF PartIIcontent OPTIONAL`.
    ///
    /// Empty means absent. The standard notes that a message may carry several containers
    /// but at most one of each type; that is a semantic rule rather than an encoding one,
    /// and [`encode_bsm`] enforces it because a duplicate would be accepted by the bits and
    /// rejected by any receiver.
    pub part_ii: Vec<PartIIContent>,
}

impl BasicSafetyMessage {
    /// A Part I-only message.
    pub fn part_i(core: BsmCoreData) -> Self {
        Self {
            core,
            part_ii: Vec::new(),
        }
    }

    /// This message with a `VehicleSafetyExtensions` container attached.
    pub fn with_vehicle_safety(mut self, value: VehicleSafetyExtensions) -> Self {
        self.part_ii.push(PartIIContent::vehicle_safety(value));
        self
    }
}

// =========================================================================================
// Encoding
// =========================================================================================

const F_MSG_CNT: Field = Field::new("bsm.coreData.msgCnt", "MsgCount");
const F_SEC_MARK: Field = Field::new("bsm.coreData.secMark", "DSecond");
const F_LAT: Field = Field::new("bsm.coreData.lat", "Latitude");
const F_LON: Field = Field::new("bsm.coreData.long", "Longitude");
const F_ELEV: Field = Field::new("bsm.coreData.elev", "Elevation");
const F_SEMI_MAJOR: Field = Field::new("bsm.coreData.accuracy.semiMajor", "SemiMajorAxisAccuracy");
const F_SEMI_MINOR: Field = Field::new("bsm.coreData.accuracy.semiMinor", "SemiMinorAxisAccuracy");
const F_ORIENTATION: Field = Field::new(
    "bsm.coreData.accuracy.orientation",
    "SemiMajorAxisOrientation",
);
const F_TRANSMISSION: Field = Field::new("bsm.coreData.transmission", "TransmissionState");
const F_SPEED: Field = Field::new("bsm.coreData.speed", "Speed");
const F_HEADING: Field = Field::new("bsm.coreData.heading", "Heading");
const F_ANGLE: Field = Field::new("bsm.coreData.angle", "SteeringWheelAngle");
const F_ACCEL_LONG: Field = Field::new("bsm.coreData.accelSet.long", "Acceleration");
const F_ACCEL_LAT: Field = Field::new("bsm.coreData.accelSet.lat", "Acceleration");
const F_ACCEL_VERT: Field = Field::new("bsm.coreData.accelSet.vert", "VerticalAcceleration");
const F_ACCEL_YAW: Field = Field::new("bsm.coreData.accelSet.yaw", "YawRate");
const F_TRACTION: Field = Field::new("bsm.coreData.brakes.traction", "TractionControlStatus");
const F_ABS: Field = Field::new("bsm.coreData.brakes.abs", "AntiLockBrakeStatus");
const F_SCS: Field = Field::new("bsm.coreData.brakes.scs", "StabilityControlStatus");
const F_BRAKE_BOOST: Field = Field::new("bsm.coreData.brakes.brakeBoost", "BrakeBoostApplied");
const F_AUX_BRAKES: Field = Field::new("bsm.coreData.brakes.auxBrakes", "AuxiliaryBrakeStatus");
const F_WHEEL_BRAKES: Field = Field::new("bsm.coreData.brakes.wheelBrakes", "BrakeAppliedStatus");
const F_WIDTH: Field = Field::new("bsm.coreData.size.width", "VehicleWidth");
const F_LENGTH: Field = Field::new("bsm.coreData.size.length", "VehicleLength");
const F_PART_II_LEN: Field = Field::new("bsm.partII", "SEQUENCE OF PartIIcontent");
const F_PART_II_ID: Field = Field::new("bsm.partII.partII-Id", "PartII-Id");
const F_GNSS_STATUS: Field =
    Field::new("vehicleSafetyExt.pathHistory.currGNSSstatus", "GNSSstatus");
const F_EVENTS: Field = Field::new("vehicleSafetyExt.events", "VehicleEventFlags");
const F_LIGHTS: Field = Field::new("vehicleSafetyExt.lights", "ExteriorLights");
const F_CRUMB_LEN: Field = Field::new(
    "vehicleSafetyExt.pathHistory.crumbData",
    "PathHistoryPointList",
);
const F_LAT_OFFSET: Field = Field::new("pathHistoryPoint.latOffset", "OffsetLL-B18");
const F_LON_OFFSET: Field = Field::new("pathHistoryPoint.lonOffset", "OffsetLL-B18");
const F_ELEV_OFFSET: Field = Field::new("pathHistoryPoint.elevationOffset", "VertOffset-B12");
const F_TIME_OFFSET: Field = Field::new("pathHistoryPoint.timeOffset", "TimeOffset");
const F_PH_SPEED: Field = Field::new("pathHistoryPoint.speed", "Speed");
const F_PH_HEADING: Field = Field::new("pathHistoryPoint.heading", "CoarseHeading");
const F_PH_SEMI_MAJOR: Field = Field::new(
    "pathHistoryPoint.posAccuracy.semiMajor",
    "SemiMajorAxisAccuracy",
);
const F_PH_SEMI_MINOR: Field = Field::new(
    "pathHistoryPoint.posAccuracy.semiMinor",
    "SemiMinorAxisAccuracy",
);
const F_PH_ORIENTATION: Field = Field::new(
    "pathHistoryPoint.posAccuracy.orientation",
    "SemiMajorAxisOrientation",
);
const F_RADIUS: Field = Field::new("pathPrediction.radiusOfCurve", "RadiusOfCurvature");
const F_CONFIDENCE: Field = Field::new("pathPrediction.confidence", "Confidence");
const F_MESSAGE_ID: Field = Field::new("messageFrame.messageId", "DSRCmsgID");

fn write_positional_accuracy(
    w: &mut BitWriter,
    a: &PositionalAccuracy,
    f_major: Field,
    f_minor: Field,
    f_orientation: Field,
) -> Result<(), UperError> {
    write_constrained_int(
        w,
        f_major,
        i64::from(a.semi_major),
        SEMI_AXIS_MIN,
        SEMI_AXIS_MAX,
    )?;
    write_constrained_int(
        w,
        f_minor,
        i64::from(a.semi_minor),
        SEMI_AXIS_MIN,
        SEMI_AXIS_MAX,
    )?;
    write_constrained_int(
        w,
        f_orientation,
        i64::from(a.orientation),
        ORIENTATION_MIN,
        ORIENTATION_MAX,
    )
}

fn read_positional_accuracy(
    r: &mut BitReader<'_>,
    f_major: Field,
    f_minor: Field,
    f_orientation: Field,
) -> Result<PositionalAccuracy, UperError> {
    Ok(PositionalAccuracy {
        semi_major: read_constrained_int(r, f_major, SEMI_AXIS_MIN, SEMI_AXIS_MAX)? as u8,
        semi_minor: read_constrained_int(r, f_minor, SEMI_AXIS_MIN, SEMI_AXIS_MAX)? as u8,
        orientation: read_constrained_int(r, f_orientation, ORIENTATION_MIN, ORIENTATION_MAX)?
            as u16,
    })
}

fn write_core_data(w: &mut BitWriter, c: &BsmCoreData) -> Result<(), UperError> {
    // No preamble: BSMcoreData has no OPTIONAL field and no extension marker.
    write_constrained_int(
        w,
        F_MSG_CNT,
        i64::from(c.msg_cnt),
        MSG_COUNT_MIN,
        MSG_COUNT_MAX,
    )?;
    write_fixed_octet_string(w, &c.id);
    write_constrained_int(
        w,
        F_SEC_MARK,
        i64::from(c.sec_mark),
        D_SECOND_MIN,
        D_SECOND_MAX,
    )?;
    write_constrained_int(w, F_LAT, i64::from(c.lat), LATITUDE_MIN, LATITUDE_MAX)?;
    write_constrained_int(w, F_LON, i64::from(c.lon), LONGITUDE_MIN, LONGITUDE_MAX)?;
    write_constrained_int(w, F_ELEV, i64::from(c.elev), ELEVATION_MIN, ELEVATION_MAX)?;
    write_positional_accuracy(w, &c.accuracy, F_SEMI_MAJOR, F_SEMI_MINOR, F_ORIENTATION)?;
    write_enumerated(
        w,
        F_TRANSMISSION,
        c.transmission.index(),
        TransmissionState::COUNT,
    )?;
    write_constrained_int(w, F_SPEED, i64::from(c.speed), SPEED_MIN, SPEED_MAX)?;
    write_constrained_int(w, F_HEADING, i64::from(c.heading), HEADING_MIN, HEADING_MAX)?;
    write_constrained_int(
        w,
        F_ANGLE,
        i64::from(c.angle),
        STEERING_WHEEL_ANGLE_MIN,
        STEERING_WHEEL_ANGLE_MAX,
    )?;

    // accelSet
    write_constrained_int(
        w,
        F_ACCEL_LONG,
        i64::from(c.accel_set.long),
        ACCELERATION_MIN,
        ACCELERATION_MAX,
    )?;
    write_constrained_int(
        w,
        F_ACCEL_LAT,
        i64::from(c.accel_set.lat),
        ACCELERATION_MIN,
        ACCELERATION_MAX,
    )?;
    write_constrained_int(
        w,
        F_ACCEL_VERT,
        i64::from(c.accel_set.vert),
        VERTICAL_ACCELERATION_MIN,
        VERTICAL_ACCELERATION_MAX,
    )?;
    write_constrained_int(
        w,
        F_ACCEL_YAW,
        i64::from(c.accel_set.yaw),
        YAW_RATE_MIN,
        YAW_RATE_MAX,
    )?;

    // brakes
    write_fixed_bit_string(
        w,
        F_WHEEL_BRAKES,
        u64::from(c.brakes.wheel_brakes.0),
        BRAKE_APPLIED_STATUS_BITS,
    )?;
    write_enumerated(
        w,
        F_TRACTION,
        c.brakes.traction.index(),
        ControlStatus::COUNT,
    )?;
    write_enumerated(w, F_ABS, c.brakes.abs.index(), ControlStatus::COUNT)?;
    write_enumerated(w, F_SCS, c.brakes.scs.index(), ControlStatus::COUNT)?;
    write_enumerated(
        w,
        F_BRAKE_BOOST,
        c.brakes.brake_boost.index(),
        BrakeBoostApplied::COUNT,
    )?;
    write_enumerated(
        w,
        F_AUX_BRAKES,
        c.brakes.aux_brakes.index(),
        AuxiliaryBrakeStatus::COUNT,
    )?;

    // size
    write_constrained_int(
        w,
        F_WIDTH,
        i64::from(c.size.width),
        VEHICLE_WIDTH_MIN,
        VEHICLE_WIDTH_MAX,
    )?;
    write_constrained_int(
        w,
        F_LENGTH,
        i64::from(c.size.length),
        VEHICLE_LENGTH_MIN,
        VEHICLE_LENGTH_MAX,
    )?;
    Ok(())
}

fn read_core_data(r: &mut BitReader<'_>) -> Result<BsmCoreData, UperError> {
    let msg_cnt = read_constrained_int(r, F_MSG_CNT, MSG_COUNT_MIN, MSG_COUNT_MAX)? as u8;
    let id_octets = read_fixed_octet_string(r, 4)?;
    let mut id = [0u8; 4];
    id.copy_from_slice(&id_octets);
    let sec_mark = read_constrained_int(r, F_SEC_MARK, D_SECOND_MIN, D_SECOND_MAX)? as u16;
    let lat = read_constrained_int(r, F_LAT, LATITUDE_MIN, LATITUDE_MAX)? as i32;
    let lon = read_constrained_int(r, F_LON, LONGITUDE_MIN, LONGITUDE_MAX)? as i32;
    let elev = read_constrained_int(r, F_ELEV, ELEVATION_MIN, ELEVATION_MAX)? as i32;
    let accuracy = read_positional_accuracy(r, F_SEMI_MAJOR, F_SEMI_MINOR, F_ORIENTATION)?;
    let transmission_index = read_enumerated(r, F_TRANSMISSION, TransmissionState::COUNT)?;
    let transmission =
        TransmissionState::from_index(transmission_index).ok_or(UperError::BadEnumIndex {
            asn1_type: "TransmissionState",
            index: transmission_index,
            count: TransmissionState::COUNT,
        })?;
    let speed = read_constrained_int(r, F_SPEED, SPEED_MIN, SPEED_MAX)? as u16;
    let heading = read_constrained_int(r, F_HEADING, HEADING_MIN, HEADING_MAX)? as u16;
    let angle = read_constrained_int(
        r,
        F_ANGLE,
        STEERING_WHEEL_ANGLE_MIN,
        STEERING_WHEEL_ANGLE_MAX,
    )? as i8;

    let accel_set = AccelerationSet4Way {
        long: read_constrained_int(r, F_ACCEL_LONG, ACCELERATION_MIN, ACCELERATION_MAX)? as i16,
        lat: read_constrained_int(r, F_ACCEL_LAT, ACCELERATION_MIN, ACCELERATION_MAX)? as i16,
        vert: read_constrained_int(
            r,
            F_ACCEL_VERT,
            VERTICAL_ACCELERATION_MIN,
            VERTICAL_ACCELERATION_MAX,
        )? as i8,
        yaw: read_constrained_int(r, F_ACCEL_YAW, YAW_RATE_MIN, YAW_RATE_MAX)? as i16,
    };

    let wheel_brakes =
        BrakeAppliedStatus(read_fixed_bit_string(r, BRAKE_APPLIED_STATUS_BITS)? as u8);
    let brakes = BrakeSystemStatus {
        wheel_brakes,
        traction: read_control_status(r, F_TRACTION)?,
        abs: read_control_status(r, F_ABS)?,
        scs: read_control_status(r, F_SCS)?,
        brake_boost: {
            let index = read_enumerated(r, F_BRAKE_BOOST, BrakeBoostApplied::COUNT)?;
            BrakeBoostApplied::from_index(index).ok_or(UperError::BadEnumIndex {
                asn1_type: "BrakeBoostApplied",
                index,
                count: BrakeBoostApplied::COUNT,
            })?
        },
        aux_brakes: {
            let index = read_enumerated(r, F_AUX_BRAKES, AuxiliaryBrakeStatus::COUNT)?;
            AuxiliaryBrakeStatus::from_index(index).ok_or(UperError::BadEnumIndex {
                asn1_type: "AuxiliaryBrakeStatus",
                index,
                count: AuxiliaryBrakeStatus::COUNT,
            })?
        },
    };

    let size = VehicleSize {
        width: read_constrained_int(r, F_WIDTH, VEHICLE_WIDTH_MIN, VEHICLE_WIDTH_MAX)? as u16,
        length: read_constrained_int(r, F_LENGTH, VEHICLE_LENGTH_MIN, VEHICLE_LENGTH_MAX)? as u16,
    };

    Ok(BsmCoreData {
        msg_cnt,
        id,
        sec_mark,
        lat,
        lon,
        elev,
        accuracy,
        transmission,
        speed,
        heading,
        angle,
        accel_set,
        brakes,
        size,
    })
}

fn read_control_status(r: &mut BitReader<'_>, field: Field) -> Result<ControlStatus, UperError> {
    let index = read_enumerated(r, field, ControlStatus::COUNT)?;
    ControlStatus::from_index(index).ok_or(UperError::BadEnumIndex {
        asn1_type: field.asn1_type,
        index,
        count: ControlStatus::COUNT,
    })
}

fn write_path_history_point(w: &mut BitWriter, p: &PathHistoryPoint) -> Result<(), UperError> {
    write_preamble(
        w,
        true,
        &[
            p.speed.is_some(),
            p.pos_accuracy.is_some(),
            p.heading.is_some(),
        ],
    );
    write_constrained_int(
        w,
        F_LAT_OFFSET,
        i64::from(p.lat_offset),
        OFFSET_LL_B18_MIN,
        OFFSET_LL_B18_MAX,
    )?;
    write_constrained_int(
        w,
        F_LON_OFFSET,
        i64::from(p.lon_offset),
        OFFSET_LL_B18_MIN,
        OFFSET_LL_B18_MAX,
    )?;
    write_constrained_int(
        w,
        F_ELEV_OFFSET,
        i64::from(p.elevation_offset),
        VERT_OFFSET_B12_MIN,
        VERT_OFFSET_B12_MAX,
    )?;
    write_constrained_int(
        w,
        F_TIME_OFFSET,
        i64::from(p.time_offset),
        TIME_OFFSET_MIN,
        TIME_OFFSET_MAX,
    )?;
    if let Some(speed) = p.speed {
        write_constrained_int(w, F_PH_SPEED, i64::from(speed), SPEED_MIN, SPEED_MAX)?;
    }
    if let Some(accuracy) = p.pos_accuracy {
        write_positional_accuracy(
            w,
            &accuracy,
            F_PH_SEMI_MAJOR,
            F_PH_SEMI_MINOR,
            F_PH_ORIENTATION,
        )?;
    }
    if let Some(heading) = p.heading {
        write_constrained_int(
            w,
            F_PH_HEADING,
            i64::from(heading),
            COARSE_HEADING_MIN,
            COARSE_HEADING_MAX,
        )?;
    }
    Ok(())
}

fn read_path_history_point(r: &mut BitReader<'_>) -> Result<PathHistoryPoint, UperError> {
    let pre = read_preamble(r, "PathHistoryPoint", true, 3)?;
    let lat_offset =
        read_constrained_int(r, F_LAT_OFFSET, OFFSET_LL_B18_MIN, OFFSET_LL_B18_MAX)? as i32;
    let lon_offset =
        read_constrained_int(r, F_LON_OFFSET, OFFSET_LL_B18_MIN, OFFSET_LL_B18_MAX)? as i32;
    let elevation_offset =
        read_constrained_int(r, F_ELEV_OFFSET, VERT_OFFSET_B12_MIN, VERT_OFFSET_B12_MAX)? as i16;
    let time_offset =
        read_constrained_int(r, F_TIME_OFFSET, TIME_OFFSET_MIN, TIME_OFFSET_MAX)? as u16;
    let speed = if pre.has(0) {
        Some(read_constrained_int(r, F_PH_SPEED, SPEED_MIN, SPEED_MAX)? as u16)
    } else {
        None
    };
    let pos_accuracy = if pre.has(1) {
        Some(read_positional_accuracy(
            r,
            F_PH_SEMI_MAJOR,
            F_PH_SEMI_MINOR,
            F_PH_ORIENTATION,
        )?)
    } else {
        None
    };
    let heading = if pre.has(2) {
        Some(read_constrained_int(r, F_PH_HEADING, COARSE_HEADING_MIN, COARSE_HEADING_MAX)? as u8)
    } else {
        None
    };
    Ok(PathHistoryPoint {
        lat_offset,
        lon_offset,
        elevation_offset,
        time_offset,
        speed,
        pos_accuracy,
        heading,
    })
}

fn write_path_history(w: &mut BitWriter, h: &PathHistory) -> Result<(), UperError> {
    if h.crumb_data.is_empty() || h.crumb_data.len() > MAX_PATH_HISTORY_POINTS {
        return Err(UperError::OutOfRange {
            field: F_CRUMB_LEN.path,
            asn1_type: F_CRUMB_LEN.asn1_type,
            value: h.crumb_data.len() as i64,
            min: 1,
            max: MAX_PATH_HISTORY_POINTS as i64,
        });
    }
    // initialPosition is never written: it is a FullPositionVector, which this codec does
    // not implement. Its preamble bit is therefore always zero.
    write_preamble(w, true, &[false, h.gnss_status.is_some()]);
    if let Some(status) = h.gnss_status {
        write_fixed_bit_string(w, F_GNSS_STATUS, u64::from(status.0), GNSS_STATUS_BITS)?;
    }
    write_constrained_length(
        w,
        F_CRUMB_LEN,
        h.crumb_data.len(),
        1,
        MAX_PATH_HISTORY_POINTS,
    )?;
    for point in &h.crumb_data {
        write_path_history_point(w, point)?;
    }
    Ok(())
}

fn read_path_history(r: &mut BitReader<'_>) -> Result<PathHistory, UperError> {
    let pre = read_preamble(r, "PathHistory", true, 2)?;
    if pre.has(0) {
        return Err(UperError::Unsupported {
            construct: "PathHistory.initialPosition",
            detail: "a FullPositionVector is present; this codec models the crumb list and \
                     the GNSS status only, and cannot skip the field without losing bit \
                     synchronisation",
        });
    }
    let gnss_status = if pre.has(1) {
        Some(GnssStatus(read_fixed_bit_string(r, GNSS_STATUS_BITS)? as u8))
    } else {
        None
    };
    let count = read_constrained_length(r, F_CRUMB_LEN, 1, MAX_PATH_HISTORY_POINTS)?;
    let mut crumb_data = Vec::with_capacity(count);
    for _ in 0..count {
        crumb_data.push(read_path_history_point(r)?);
    }
    Ok(PathHistory {
        gnss_status,
        crumb_data,
    })
}

fn write_vehicle_safety_extensions(
    w: &mut BitWriter,
    v: &VehicleSafetyExtensions,
) -> Result<(), UperError> {
    if let Some(events) = v.events
        && !events.fits_root()
    {
        return Err(UperError::Unsupported {
            construct: "VehicleEventFlags",
            detail: "a bit above the 13-bit extension root is set (eventJackKnife); \
                     encoding it needs the size constraint's extension addition",
        });
    }
    if let Some(lights) = v.lights
        && !lights.fits_root()
    {
        return Err(UperError::Unsupported {
            construct: "ExteriorLights",
            detail: "a bit above the 9-bit extension root is set; encoding it needs the \
                     size constraint's extension addition",
        });
    }
    write_preamble(
        w,
        true,
        &[
            v.events.is_some(),
            v.path_history.is_some(),
            v.path_prediction.is_some(),
            v.lights.is_some(),
        ],
    );
    if let Some(events) = v.events {
        write_extensible_bit_string(w, F_EVENTS, u64::from(events.0), VEHICLE_EVENT_FLAGS_BITS)?;
    }
    if let Some(history) = &v.path_history {
        write_path_history(w, history)?;
    }
    if let Some(prediction) = v.path_prediction {
        // PathPrediction is extensible with no optional field: an extension bit, then the
        // two mandatory values.
        write_preamble(w, true, &[]);
        write_constrained_int(
            w,
            F_RADIUS,
            i64::from(prediction.radius_of_curve),
            RADIUS_OF_CURVATURE_MIN,
            RADIUS_OF_CURVATURE_MAX,
        )?;
        write_constrained_int(
            w,
            F_CONFIDENCE,
            i64::from(prediction.confidence),
            CONFIDENCE_MIN,
            CONFIDENCE_MAX,
        )?;
    }
    if let Some(lights) = v.lights {
        write_extensible_bit_string(w, F_LIGHTS, u64::from(lights.0), EXTERIOR_LIGHTS_BITS)?;
    }
    Ok(())
}

fn read_vehicle_safety_extensions(
    r: &mut BitReader<'_>,
) -> Result<VehicleSafetyExtensions, UperError> {
    let pre = read_preamble(r, "VehicleSafetyExtensions", true, 4)?;
    let events = if pre.has(0) {
        Some(VehicleEventFlags(read_extensible_bit_string(
            r,
            "VehicleEventFlags",
            VEHICLE_EVENT_FLAGS_BITS,
        )? as u16))
    } else {
        None
    };
    let path_history = if pre.has(1) {
        Some(read_path_history(r)?)
    } else {
        None
    };
    let path_prediction = if pre.has(2) {
        read_preamble(r, "PathPrediction", true, 0)?;
        Some(PathPrediction {
            radius_of_curve: read_constrained_int(
                r,
                F_RADIUS,
                RADIUS_OF_CURVATURE_MIN,
                RADIUS_OF_CURVATURE_MAX,
            )? as i16,
            confidence: read_constrained_int(r, F_CONFIDENCE, CONFIDENCE_MIN, CONFIDENCE_MAX)?
                as u8,
        })
    } else {
        None
    };
    let lights = if pre.has(3) {
        Some(ExteriorLights(
            read_extensible_bit_string(r, "ExteriorLights", EXTERIOR_LIGHTS_BITS)? as u16,
        ))
    } else {
        None
    };
    Ok(VehicleSafetyExtensions {
        events,
        path_history,
        path_prediction,
        lights,
    })
}

/// The one statement of what a legal Part II container *set* is.
///
/// Called by both [`write_bsm`] and [`read_bsm`], so encoder and decoder cannot drift apart
/// about what a legal message is — which they had: the writer refused a repeated
/// `partII-Id` and the reader returned it, so anything that decoded, inspected and
/// re-encoded a crafted message got the error at the far end instead of at the message that
/// was actually wrong.
///
/// `PartIIcontent` names its extension by `partII-Id` and the standard's information object
/// set admits at most one value of each extension type per message; two containers sharing
/// an id give a receiver no rule for choosing between them. The check is quadratic in the
/// container count, which is bounded by [`MAX_PART_II_CONTAINERS`] = 8, so it is at most 28
/// comparisons.
fn check_part_ii_ids(part_ii: &[PartIIContent]) -> Result<(), UperError> {
    for (i, content) in part_ii.iter().enumerate() {
        if part_ii[..i].iter().any(|other| other.id == content.id) {
            return Err(UperError::Unsupported {
                construct: "BasicSafetyMessage.partII",
                detail: "the same Part II container id appears twice; the standard admits \
                         at most one instance of each extension type per message",
            });
        }
    }
    Ok(())
}

/// The `BasicSafetyMessage` PDU, without the `MessageFrame` wrapper.
fn write_bsm(w: &mut BitWriter, bsm: &BasicSafetyMessage) -> Result<(), UperError> {
    if bsm.part_ii.len() > MAX_PART_II_CONTAINERS {
        return Err(UperError::OutOfRange {
            field: F_PART_II_LEN.path,
            asn1_type: F_PART_II_LEN.asn1_type,
            value: bsm.part_ii.len() as i64,
            min: 1,
            max: MAX_PART_II_CONTAINERS as i64,
        });
    }
    check_part_ii_ids(&bsm.part_ii)?;

    // regional is never written: no Reg-BasicSafetyMessage extension is modelled, so its
    // preamble bit is always zero.
    write_preamble(w, true, &[!bsm.part_ii.is_empty(), false]);
    write_core_data(w, &bsm.core)?;

    if !bsm.part_ii.is_empty() {
        write_constrained_length(
            w,
            F_PART_II_LEN,
            bsm.part_ii.len(),
            1,
            MAX_PART_II_CONTAINERS,
        )?;
        for content in &bsm.part_ii {
            write_constrained_int(
                w,
                F_PART_II_ID,
                i64::from(content.id),
                0,
                i64::from(PART_II_ID_MAX),
            )?;
            match &content.value {
                PartIIValue::VehicleSafety(value) => {
                    if content.id != PART_II_VEHICLE_SAFETY {
                        return Err(UperError::Unsupported {
                            construct: "BasicSafetyMessage.partII",
                            detail: "a VehicleSafetyExtensions value is attached to a \
                                     Part II id other than 0, which no receiver would \
                                     decode as one",
                        });
                    }
                    let mut inner = BitWriter::with_capacity(48);
                    write_vehicle_safety_extensions(&mut inner, value)?;
                    write_open_type(w, "partII-Value", &inner.into_bytes())?;
                }
                PartIIValue::Opaque(octets) => {
                    write_open_type(w, "partII-Value", octets)?;
                }
            }
        }
    }
    Ok(())
}

fn read_bsm(r: &mut BitReader<'_>) -> Result<BasicSafetyMessage, UperError> {
    let pre = read_preamble(r, "BasicSafetyMessage", true, 2)?;
    let core = read_core_data(r)?;
    let mut part_ii = Vec::new();
    if pre.has(0) {
        let count = read_constrained_length(r, F_PART_II_LEN, 1, MAX_PART_II_CONTAINERS)?;
        for _ in 0..count {
            let id = read_constrained_int(r, F_PART_II_ID, 0, i64::from(PART_II_ID_MAX))? as u8;
            let octets = read_open_type(r, "partII-Value")?;
            let value = if id == PART_II_VEHICLE_SAFETY {
                let mut inner = BitReader::new(&octets);
                let value = read_vehicle_safety_extensions(&mut inner)?;
                inner.finish()?;
                PartIIValue::VehicleSafety(value)
            } else {
                PartIIValue::Opaque(octets)
            };
            part_ii.push(PartIIContent { id, value });
        }
        check_part_ii_ids(&part_ii)?;
    }
    if pre.has(1) {
        return Err(UperError::Unsupported {
            construct: "BasicSafetyMessage.regional",
            detail: "a regional extension is present; no Reg-BasicSafetyMessage object is \
                     modelled, and its open type cannot be interpreted",
        });
    }
    Ok(BasicSafetyMessage { core, part_ii })
}

// =========================================================================================
// Public codec entry points
// =========================================================================================

/// Turns a bit-level failure into a [`CodecError`], which is where the message type is
/// known.
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
            ty: MsgType::Bsm,
            construct,
            detail,
        },
        other => CodecError::Encode {
            ty: MsgType::Bsm,
            detail: other.to_string(),
        },
    }
}

fn on_decode(len: usize) -> impl Fn(UperError) -> CodecError {
    move |e| match e {
        UperError::Unsupported { construct, detail } => CodecError::UnsupportedConstruct {
            ty: MsgType::Bsm,
            construct,
            detail,
        },
        other => CodecError::Decode {
            ty: MsgType::Bsm,
            len,
            detail: other.to_string(),
        },
    }
}

/// UPER-encodes a `BasicSafetyMessage` PDU.
///
/// The bytes are real: [`Encoded::size_source`] is [`crate::SizeSource::Uper`] and the
/// size is the measured length, not a model. A Part I-only message is
/// [`PART_I_ONLY_SIZE_B`] octets.
pub fn encode_bsm(bsm: &BasicSafetyMessage) -> Result<Encoded, CodecError> {
    let mut w = BitWriter::with_capacity(64);
    write_bsm(&mut w, bsm).map_err(on_encode)?;
    Ok(Encoded::uper(w.into_bytes()))
}

/// Decodes a `BasicSafetyMessage` PDU.
///
/// Refuses trailing data beyond X.691's at-most-seven zero padding bits, so a truncated or
/// concatenated payload is an error rather than a partially filled message.
pub fn decode_bsm(bytes: &[u8]) -> Result<BasicSafetyMessage, CodecError> {
    let map = on_decode(bytes.len());
    let mut r = BitReader::new(bytes);
    let bsm = read_bsm(&mut r).map_err(&map)?;
    r.finish().map_err(&map)?;
    Ok(bsm)
}

/// UPER-encodes a `MessageFrame` carrying this BSM — what actually goes in a WSM payload.
///
/// `MessageFrame` is an extensible `SEQUENCE` of a 15-bit `DSRCmsgID` and an open type, so
/// wrapping costs three octets for a Part I-only message: [`PART_I_ONLY_MESSAGE_FRAME_SIZE_B`]
/// against [`PART_I_ONLY_SIZE_B`]. The J2735 size model charges a flat
/// [`crate::size_model::MESSAGE_FRAME_B`] for the same wrapper.
pub fn encode_message_frame(bsm: &BasicSafetyMessage) -> Result<Encoded, CodecError> {
    let mut inner = BitWriter::with_capacity(64);
    write_bsm(&mut inner, bsm).map_err(on_encode)?;
    let inner = inner.into_bytes();

    let mut w = BitWriter::with_capacity(inner.len() + 4);
    write_preamble(&mut w, true, &[]);
    write_constrained_int(
        &mut w,
        F_MESSAGE_ID,
        i64::from(BSM_MESSAGE_ID),
        0,
        i64::from(DSRC_MSG_ID_MAX),
    )
    .map_err(on_encode)?;
    write_open_type(&mut w, "MessageFrame.value", &inner).map_err(on_encode)?;
    Ok(Encoded::uper(w.into_bytes()))
}

/// Decodes a `MessageFrame` and returns the BSM inside it.
///
/// Refuses any `DSRCmsgID` other than [`BSM_MESSAGE_ID`]: this codec implements one
/// message, and a `MessageFrame` holding a SPaT is not a BSM with odd fields.
pub fn decode_message_frame(bytes: &[u8]) -> Result<BasicSafetyMessage, CodecError> {
    let map = on_decode(bytes.len());
    let mut r = BitReader::new(bytes);
    read_preamble(&mut r, "MessageFrame", true, 0).map_err(&map)?;
    let id = read_constrained_int(&mut r, F_MESSAGE_ID, 0, i64::from(DSRC_MSG_ID_MAX))
        .map_err(&map)? as u16;
    if id != BSM_MESSAGE_ID {
        return Err(CodecError::UnsupportedConstruct {
            ty: MsgType::Bsm,
            construct: "MessageFrame.messageId",
            detail: "the frame carries a DSRCmsgID other than basicSafetyMessage(20); this \
                     codec implements the BSM only",
        });
    }
    let inner = read_open_type(&mut r, "MessageFrame.value").map_err(&map)?;
    r.finish().map_err(&map)?;

    let mut ir = BitReader::new(&inner);
    let bsm = read_bsm(&mut ir).map_err(&map)?;
    ir.finish().map_err(&map)?;
    Ok(bsm)
}

// =========================================================================================
// From simulator quantities to wire units
// =========================================================================================

/// Scales to a wire unit with round-half-away-from-zero, after quantising on the field's
/// declared grid (build decision D9).
fn scaled(value: f64, quantum: f64, unit: f64) -> f64 {
    (math::quantize_to(value, quantum) / unit).round()
}

/// Clamps into `[min, max]`, mapping a non-finite input to `unavailable`.
fn clamp_or(value: f64, min: i64, max: i64, unavailable: i64) -> i64 {
    if !value.is_finite() {
        return unavailable;
    }
    (value as i64).clamp(min, max)
}

/// `Latitude`, LSB 1/10 microdegree. `None` when the projection produced a latitude outside
/// ±90°, which is a broken projection rather than a missing sensor.
pub fn latitude(deg: f64) -> Option<i32> {
    if !deg.is_finite() || !(-90.0..=90.0).contains(&deg) {
        return None;
    }
    Some(clamp_or(
        scaled(deg, crate::units::Q_DEG, 1e-7),
        LATITUDE_MIN,
        LATITUDE_MAX - 1,
        i64::from(LATITUDE_UNAVAILABLE),
    ) as i32)
}

/// `Longitude`, LSB 1/10 microdegree. Clamps at [`LONGITUDE_MIN`], which is one LSB above
/// −180°.
pub fn longitude(deg: f64) -> Option<i32> {
    if !deg.is_finite() || !(-180.0..=180.0).contains(&deg) {
        return None;
    }
    Some(clamp_or(
        scaled(deg, crate::units::Q_DEG, 1e-7),
        LONGITUDE_MIN,
        LONGITUDE_MAX - 1,
        i64::from(LONGITUDE_UNAVAILABLE),
    ) as i32)
}

/// `Elevation`, LSB 10 cm, `-4096` when not finite.
pub fn elevation(m: f64) -> i32 {
    clamp_or(
        scaled(m, crate::units::Q_M, 0.1),
        ELEVATION_MIN + 1,
        ELEVATION_MAX,
        i64::from(ELEVATION_UNKNOWN),
    ) as i32
}

/// `SemiMajorAxisAccuracy` / `SemiMinorAxisAccuracy`, LSB 0.05 m.
///
/// A no-fix [`PositionEstimate`] carries an infinite semi-axis, which lands on
/// `unavailable(255)` — exactly what a vehicle with no fix should send.
pub fn semi_axis_accuracy(m: f64) -> u8 {
    if !m.is_finite() || m < 0.0 {
        return SEMI_AXIS_UNAVAILABLE;
    }
    let steps = (math::quantize_to(m, crate::units::Q_M) / 0.05).ceil();
    clamp_or(
        steps,
        SEMI_AXIS_MIN,
        i64::from(SEMI_AXIS_OUT_OF_RANGE),
        i64::from(SEMI_AXIS_UNAVAILABLE),
    ) as u8
}

/// `SemiMajorAxisOrientation`, LSB 360/65535 degree clockwise from true north.
pub fn semi_major_orientation(rad: f64) -> u16 {
    let bearing = crate::units::enu_heading_to_wgs84_bearing_deg(rad);
    if !bearing.is_finite() {
        return ORIENTATION_UNAVAILABLE;
    }
    let steps = (bearing / (360.0 / 65_535.0)).round();
    let v = clamp_or(steps, 0, 65_534, i64::from(ORIENTATION_UNAVAILABLE));
    // 65535 is unavailable and 360° is 0°, so fold the top of the circle onto zero.
    if v >= 65_535 { 0 } else { v as u16 }
}

/// `Speed`, LSB 0.02 m/s, `8191` for a non-finite or negative ground speed.
pub fn speed(mps: f64) -> u16 {
    if !mps.is_finite() || mps < 0.0 {
        return SPEED_UNAVAILABLE;
    }
    clamp_or(
        scaled(mps, crate::units::Q_MPS, 0.02),
        SPEED_MIN,
        SPEED_MAX - 1,
        i64::from(SPEED_UNAVAILABLE),
    ) as u16
}

/// `Heading`, LSB 0.0125 degree clockwise from true north, from an ENU heading in radians.
pub fn heading(rad: f64) -> u16 {
    let bearing = crate::units::enu_heading_to_wgs84_bearing_deg(rad);
    if !bearing.is_finite() {
        return HEADING_UNAVAILABLE;
    }
    let steps = (bearing / 0.0125).round();
    let v = clamp_or(
        steps,
        HEADING_MIN,
        HEADING_MAX - 1,
        i64::from(HEADING_UNAVAILABLE),
    );
    if v >= HEADING_MAX { 0 } else { v as u16 }
}

/// `SteeringWheelAngle`, LSB 1.5 degree; `None` becomes `unavailable(127)`.
pub fn steering_wheel_angle(rad: Option<f64>) -> i8 {
    let Some(rad) = rad else {
        return STEERING_WHEEL_ANGLE_UNAVAILABLE;
    };
    let deg = crate::units::rad_to_deg(math::quantize_to(rad, crate::units::Q_RAD));
    clamp_or(
        (deg / 1.5).round(),
        STEERING_WHEEL_ANGLE_MIN,
        STEERING_WHEEL_ANGLE_MAX - 1,
        i64::from(STEERING_WHEEL_ANGLE_UNAVAILABLE),
    ) as i8
}

/// `Acceleration`, LSB 0.01 m/s²; `None` becomes `unavailable(2001)`.
pub fn acceleration(mps2: Option<f64>) -> i16 {
    let Some(a) = mps2 else {
        return ACCELERATION_UNAVAILABLE;
    };
    clamp_or(
        scaled(a, crate::units::Q_MPS2, 0.01),
        ACCELERATION_MIN,
        ACCELERATION_MAX - 1,
        i64::from(ACCELERATION_UNAVAILABLE),
    ) as i16
}

/// `VerticalAcceleration`, LSB 0.02 G; `None` becomes `unavailable(-127)`.
///
/// Takes m/s² like every other acceleration in the simulator and converts with
/// [`STANDARD_GRAVITY_MPS2`]; the standard's unit is the odd one here, not ours.
pub fn vertical_acceleration(mps2: Option<f64>) -> i8 {
    let Some(a) = mps2 else {
        return VERTICAL_ACCELERATION_UNAVAILABLE;
    };
    clamp_or(
        scaled(a, crate::units::Q_MPS2, 0.02 * STANDARD_GRAVITY_MPS2),
        VERTICAL_ACCELERATION_MIN + 1,
        VERTICAL_ACCELERATION_MAX,
        i64::from(VERTICAL_ACCELERATION_UNAVAILABLE),
    ) as i8
}

/// `YawRate`, LSB 0.01 degree per second.
///
/// `None` becomes zero, because `YawRate` declares no `unavailable` value. That
/// indistinguishability is the standard's, and the model card records it.
pub fn yaw_rate(rad_per_s: Option<f64>) -> i16 {
    let Some(rate) = rad_per_s else { return 0 };
    let deg = crate::units::rad_to_deg(math::quantize_to(rate, crate::units::Q_RAD_S));
    clamp_or((deg / 0.01).round(), YAW_RATE_MIN, YAW_RATE_MAX, 0) as i16
}

/// `VehicleWidth`, LSB 1 cm.
pub fn vehicle_width(m: f64) -> u16 {
    clamp_or(
        scaled(m, crate::units::Q_M, 0.01),
        VEHICLE_WIDTH_MIN,
        VEHICLE_WIDTH_MAX,
        VEHICLE_WIDTH_MIN,
    ) as u16
}

/// `VehicleLength`, LSB 1 cm.
pub fn vehicle_length(m: f64) -> u16 {
    clamp_or(
        scaled(m, crate::units::Q_M, 0.01),
        VEHICLE_LENGTH_MIN,
        VEHICLE_LENGTH_MAX,
        VEHICLE_LENGTH_MIN,
    ) as u16
}

/// `secMark`: the millisecond within the current UTC minute.
///
/// Wall-clock derived, like [`crate::cam::timestamp_its`], so two nodes in the same run
/// agree and a recorded run can be replayed against real time.
pub fn sec_mark(clock: WallClock, t: SimTime) -> u16 {
    let ms = clock.unix_nanos_at(t).div_euclid(1_000_000);
    ms.rem_euclid(MS_PER_MINUTE as i128) as u16
}

/// What a node needs to hand over to have a Part I BSM built.
///
/// `position` is a **belief**, never ground truth: invariant I-C2. It is the only source of
/// position, speed and heading here, exactly as in [`crate::cam::CamInput`], so a spoofing
/// attacker that perturbs its own GNSS belief perturbs what it transmits and nothing else.
#[derive(Debug, Clone)]
pub struct BsmInput {
    /// `msgCnt`, the sender's own rolling counter.
    pub msg_cnt: u8,
    /// `id`, the four-octet rotating identifier.
    pub id: [u8; 4],
    /// The node's own position belief.
    pub position: PositionEstimate,
    /// Origin of the local tangent plane `position.pos` is expressed in.
    pub origin: GeoOrigin,
    /// Vehicle dimensions, for `size`.
    pub dims: Dims,
    /// `secMark`; [`sec_mark`] computes it from the run's wall clock.
    pub sec_mark: u16,
    /// `transmission`.
    pub transmission: TransmissionState,
    /// Steering wheel angle, radians, positive counter-clockwise. `None` → `unavailable`.
    pub steering_wheel_angle_rad: Option<f64>,
    /// Longitudinal acceleration, m/s². `None` → `unavailable`.
    pub longitudinal_acceleration_mps2: Option<f64>,
    /// Lateral acceleration, m/s². `None` → `unavailable`.
    pub lateral_acceleration_mps2: Option<f64>,
    /// Vertical acceleration, m/s². `None` → `unavailable`.
    pub vertical_acceleration_mps2: Option<f64>,
    /// Yaw rate, rad/s. `None` → zero, because the type has no sentinel.
    pub yaw_rate_rad_s: Option<f64>,
    /// `brakes`.
    pub brakes: BrakeSystemStatus,
}

impl BsmInput {
    /// An input with every optional dynamics quantity absent and the brakes unavailable —
    /// the honest default for a mobility model that reports nothing but kinematics.
    pub fn new(
        msg_cnt: u8,
        id: [u8; 4],
        position: PositionEstimate,
        origin: GeoOrigin,
        dims: Dims,
        sec_mark: u16,
    ) -> Self {
        Self {
            msg_cnt,
            id,
            position,
            origin,
            dims,
            sec_mark,
            transmission: TransmissionState::Unavailable,
            steering_wheel_angle_rad: None,
            longitudinal_acceleration_mps2: None,
            lateral_acceleration_mps2: None,
            vertical_acceleration_mps2: None,
            yaw_rate_rad_s: None,
            brakes: BrakeSystemStatus::UNAVAILABLE,
        }
    }
}

/// Fills a Part I BSM from a node's belief.
///
/// Fails only when the belief's position cannot be interpreted as a geodetic coordinate at
/// all; every other out-of-range quantity saturates to the sentinel its ASN.1 type
/// declares, which is what a conformant OBU does.
pub fn build_bsm(input: &BsmInput) -> Result<BasicSafetyMessage, CodecError> {
    let (lat_deg, lon_deg, alt_m) = input.origin.to_geodetic(input.position.pos);
    let lat = latitude(lat_deg).ok_or(CodecError::OutOfRange {
        field: F_LAT.path,
        asn1_type: F_LAT.asn1_type,
        value: (lat_deg * 1e7) as i64,
        min: LATITUDE_MIN,
        max: LATITUDE_MAX,
    })?;
    let lon = longitude(lon_deg).ok_or(CodecError::OutOfRange {
        field: F_LON.path,
        asn1_type: F_LON.asn1_type,
        value: (lon_deg * 1e7) as i64,
        min: LONGITUDE_MIN,
        max: LONGITUDE_MAX,
    })?;

    let core = BsmCoreData {
        msg_cnt: input.msg_cnt.min(MSG_COUNT_MAX as u8),
        id: input.id,
        sec_mark: input.sec_mark,
        lat,
        lon,
        elev: elevation(alt_m),
        accuracy: PositionalAccuracy {
            semi_major: semi_axis_accuracy(input.position.semi_major_m),
            semi_minor: semi_axis_accuracy(input.position.semi_minor_m),
            orientation: semi_major_orientation(input.position.orientation_rad),
        },
        transmission: input.transmission,
        speed: speed(input.position.ground_speed_mps()),
        heading: heading(input.position.heading_rad),
        angle: steering_wheel_angle(input.steering_wheel_angle_rad),
        accel_set: AccelerationSet4Way {
            long: acceleration(input.longitudinal_acceleration_mps2),
            lat: acceleration(input.lateral_acceleration_mps2),
            vert: vertical_acceleration(input.vertical_acceleration_mps2),
            yaw: yaw_rate(input.yaw_rate_rad_s),
        },
        brakes: input.brakes,
        size: VehicleSize {
            width: vehicle_width(input.dims.width_m),
            length: vehicle_length(input.dims.length_m),
        },
    };
    Ok(BasicSafetyMessage::part_i(core))
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::belief::FixQuality;
    use v2xw_core::geom::Vec3;

    fn core() -> BsmCoreData {
        BsmCoreData {
            msg_cnt: 42,
            id: [0x0a, 0x0b, 0x0c, 0x0d],
            sec_mark: 12_345,
            lat: 407_440_000,
            lon: -739_900_000,
            elev: 125,
            accuracy: PositionalAccuracy {
                semi_major: 36,
                semi_minor: 22,
                orientation: 12_345,
            },
            transmission: TransmissionState::ForwardGears,
            speed: 694,
            heading: 3_600,
            angle: 2,
            accel_set: AccelerationSet4Way {
                long: -125,
                lat: 30,
                vert: 50,
                yaw: -1_200,
            },
            brakes: BrakeSystemStatus {
                wheel_brakes: BrakeAppliedStatus::LEFT_FRONT.with(BrakeAppliedStatus::RIGHT_FRONT),
                traction: ControlStatus::On,
                abs: ControlStatus::Engaged,
                scs: ControlStatus::Off,
                brake_boost: BrakeBoostApplied::On,
                aux_brakes: AuxiliaryBrakeStatus::Off,
            },
            size: VehicleSize {
                width: 180,
                length: 450,
            },
        }
    }

    /// The size follows from the field widths and is the first thing an independent
    /// implementation would disagree about, so it is asserted rather than observed.
    #[test]
    fn a_part_i_bsm_is_thirty_seven_octets() {
        let e = encode_bsm(&BasicSafetyMessage::part_i(core())).expect("encodes");
        assert_eq!(e.size, PART_I_ONLY_SIZE_B);
        assert_eq!(e.bytes.len(), 37);
        assert_eq!(e.size_source, crate::SizeSource::Uper);
        // 3 preamble bits + 290 core bits = 293, so the last three bits are padding.
        assert_eq!(BsmCoreData::ENCODED_BITS + 3, 293);
    }

    /// `BrakeAppliedStatus ::= BIT STRING (SIZE(5))` — J2735-Common-2024 rel v1.1.2,
    /// line 911. Five bits, no extension marker, so bit 5 of the `u8` newtype is not a
    /// value of the type and has no encoding.
    ///
    /// The encoder used to write only the low five bits and return `Ok`, emitting a
    /// *canonical* BSM that claimed different brakes from the one the caller built. A
    /// round-trip test cannot see that — the decoder reads back the truncated value and
    /// agrees — and neither can the codec's own canonical-form check, so this test pins
    /// the bytes instead.
    ///
    /// The five bits sit at offsets 256..261 of the PDU: 3 preamble + msgCnt 7 + id 32 +
    /// secMark 16 + lat 31 + long 32 + elev 16 + accuracy 32 + transmission 3 + speed 13 +
    /// heading 15 + angle 8 + accelSet 48. That is the top five bits of octet 32, which is
    /// the only octet that differs between the three vectors below.
    #[test]
    fn a_wheel_brakes_bit_above_the_five_bit_string_is_refused_not_truncated() {
        let encode = |mask: BrakeAppliedStatus| {
            let mut c = core();
            c.brakes.wheel_brakes = mask;
            encode_bsm(&BasicSafetyMessage::part_i(c))
        };

        // The three in-range vectors, byte for byte. They differ in octet 32 and nowhere
        // else, which is what makes the truncation invisible without pinned bytes.
        const PREFIX: [u8; 32] = [
            0x0a, 0x82, 0x82, 0xc3, 0x03, 0x4c, 0x0e, 0x66, 0xf6, 0xf9, 0xc0, 0x1f, 0x97, 0xeb,
            0xcf, 0x88, 0x3e, 0x92, 0x0b, 0x18, 0x1c, 0xa1, 0x5b, 0x0e, 0x10, 0x80, 0x75, 0x37,
            0xee, 0xb1, 0x7b, 0x4f,
        ];
        const SUFFIX: [u8; 4] = [0xb2, 0x5a, 0x0e, 0x10];
        let expected = |octet32: u8| {
            let mut v = PREFIX.to_vec();
            v.push(octet32);
            v.extend_from_slice(&SUFFIX);
            v
        };
        for (mask, octet32) in [
            (BrakeAppliedStatus::NONE, 0x05u8),
            (
                BrakeAppliedStatus::LEFT_FRONT.with(BrakeAppliedStatus::RIGHT_REAR),
                0x4d,
            ),
            (BrakeAppliedStatus(0b1_1111), 0xfd),
        ] {
            let e = encode(mask).expect("every five-bit value encodes");
            assert_eq!(e.bytes, expected(octet32), "wheelBrakes = {:#07b}", mask.0);
            assert_eq!(
                e.bytes[32] >> 3,
                mask.0,
                "the five bits are octet 32's top five"
            );
            assert_eq!(
                decode_bsm(&e.bytes)
                    .expect("decodes")
                    .core
                    .brakes
                    .wheel_brakes,
                mask
            );
        }

        // And a bit above the string is an error naming the field, not a silent rewrite.
        for over in [0b10_0000u8, 0b10_1010, 0xff] {
            let err = encode(BrakeAppliedStatus(over)).expect_err("bit 5 has no encoding");
            let text = err.to_string();
            assert!(
                text.contains("bsm.coreData.brakes.wheelBrakes")
                    && text.contains("BrakeAppliedStatus")
                    && text.contains("0..=31"),
                "{err}"
            );
            assert!(
                matches!(err, CodecError::OutOfRange { .. }),
                "a value outside a fixed size constraint is out of range, not unsupported: \
                 {err}"
            );
        }
        // The truncating encoder would have produced the wheelBrakes = 00000 vector for
        // 0b10_0000 and the all-wheels vector for 0xff. Neither is reachable now.
    }

    #[test]
    fn a_message_frame_costs_three_more_octets() {
        let e = encode_message_frame(&BasicSafetyMessage::part_i(core())).expect("encodes");
        assert_eq!(e.size, PART_I_ONLY_MESSAGE_FRAME_SIZE_B);
        let back = decode_message_frame(&e.bytes).expect("decodes");
        assert_eq!(back.core, core());
    }

    #[test]
    fn part_i_round_trips_exactly() {
        let bsm = BasicSafetyMessage::part_i(core());
        let e = encode_bsm(&bsm).expect("encodes");
        assert_eq!(decode_bsm(&e.bytes).expect("decodes"), bsm);
    }

    #[test]
    fn every_sentinel_round_trips() {
        let bsm = BasicSafetyMessage::part_i(BsmCoreData::unavailable([0xff; 4]));
        let e = encode_bsm(&bsm).expect("encodes");
        let back = decode_bsm(&e.bytes).expect("decodes");
        assert_eq!(back, bsm);
        assert_eq!(back.core.lat, LATITUDE_UNAVAILABLE);
        assert_eq!(back.core.lon, LONGITUDE_UNAVAILABLE);
        assert_eq!(back.core.speed, SPEED_UNAVAILABLE);
        assert_eq!(back.core.accel_set.vert, VERTICAL_ACCELERATION_UNAVAILABLE);
    }

    #[test]
    fn the_extremes_of_every_field_round_trip() {
        for (name, c) in [
            (
                "minimum",
                BsmCoreData {
                    msg_cnt: 0,
                    id: [0; 4],
                    sec_mark: 0,
                    lat: LATITUDE_MIN as i32,
                    lon: LONGITUDE_MIN as i32,
                    elev: ELEVATION_MIN as i32,
                    accuracy: PositionalAccuracy {
                        semi_major: 0,
                        semi_minor: 0,
                        orientation: 0,
                    },
                    transmission: TransmissionState::Neutral,
                    speed: 0,
                    heading: 0,
                    angle: STEERING_WHEEL_ANGLE_MIN as i8,
                    accel_set: AccelerationSet4Way {
                        long: ACCELERATION_MIN as i16,
                        lat: ACCELERATION_MIN as i16,
                        vert: VERTICAL_ACCELERATION_MIN as i8,
                        yaw: YAW_RATE_MIN as i16,
                    },
                    brakes: BrakeSystemStatus {
                        wheel_brakes: BrakeAppliedStatus::NONE,
                        traction: ControlStatus::Unavailable,
                        abs: ControlStatus::Unavailable,
                        scs: ControlStatus::Unavailable,
                        brake_boost: BrakeBoostApplied::Unavailable,
                        aux_brakes: AuxiliaryBrakeStatus::Unavailable,
                    },
                    size: VehicleSize {
                        width: 0,
                        length: 0,
                    },
                },
            ),
            (
                "maximum",
                BsmCoreData {
                    msg_cnt: MSG_COUNT_MAX as u8,
                    id: [0xff; 4],
                    sec_mark: D_SECOND_MAX as u16,
                    lat: LATITUDE_MAX as i32,
                    lon: LONGITUDE_MAX as i32,
                    elev: ELEVATION_MAX as i32,
                    accuracy: PositionalAccuracy {
                        semi_major: 255,
                        semi_minor: 255,
                        orientation: 65_535,
                    },
                    transmission: TransmissionState::Unavailable,
                    speed: SPEED_MAX as u16,
                    heading: HEADING_MAX as u16,
                    angle: STEERING_WHEEL_ANGLE_MAX as i8,
                    accel_set: AccelerationSet4Way {
                        long: ACCELERATION_MAX as i16,
                        lat: ACCELERATION_MAX as i16,
                        vert: VERTICAL_ACCELERATION_MAX as i8,
                        yaw: YAW_RATE_MAX as i16,
                    },
                    brakes: BrakeSystemStatus {
                        wheel_brakes: BrakeAppliedStatus::UNAVAILABLE
                            .with(BrakeAppliedStatus::ALL_WHEELS),
                        traction: ControlStatus::Engaged,
                        abs: ControlStatus::Engaged,
                        scs: ControlStatus::Engaged,
                        brake_boost: BrakeBoostApplied::On,
                        aux_brakes: AuxiliaryBrakeStatus::Reserved,
                    },
                    size: VehicleSize {
                        width: VEHICLE_WIDTH_MAX as u16,
                        length: VEHICLE_LENGTH_MAX as u16,
                    },
                },
            ),
        ] {
            let bsm = BasicSafetyMessage::part_i(c);
            let e = encode_bsm(&bsm).unwrap_or_else(|err| panic!("{name} encodes: {err}"));
            assert_eq!(e.size, PART_I_ONLY_SIZE_B, "{name}");
            assert_eq!(decode_bsm(&e.bytes).expect("decodes"), bsm, "{name}");
        }
    }

    #[test]
    fn part_ii_vehicle_safety_round_trips() {
        let vse = VehicleSafetyExtensions {
            events: Some(VehicleEventFlags::HARD_BRAKING.with(VehicleEventFlags::HAZARD_LIGHTS)),
            path_history: Some(PathHistory {
                gnss_status: Some(GnssStatus::IS_HEALTHY.with(GnssStatus::IS_MONITORED)),
                crumb_data: vec![
                    PathHistoryPoint::new(1_000, -1_000, 5, 10),
                    PathHistoryPoint {
                        speed: Some(500),
                        pos_accuracy: Some(PositionalAccuracy {
                            semi_major: 3,
                            semi_minor: 4,
                            orientation: 5,
                        }),
                        heading: Some(120),
                        ..PathHistoryPoint::new(
                            OFFSET_LL_B18_MIN as i32,
                            OFFSET_LL_B18_MAX as i32,
                            VERT_OFFSET_B12_MIN as i16,
                            TIME_OFFSET_MAX as u16,
                        )
                    },
                ],
            }),
            path_prediction: Some(PathPrediction::straight(200)),
            lights: Some(ExteriorLights::LOW_BEAM.with(ExteriorLights::PARKING)),
        };
        let bsm = BasicSafetyMessage::part_i(core()).with_vehicle_safety(vse);
        let e = encode_bsm(&bsm).expect("encodes");
        assert!(e.size > PART_I_ONLY_SIZE_B);
        assert_eq!(decode_bsm(&e.bytes).expect("decodes"), bsm);
    }

    /// A full path history pushes the Part II open type past 127 octets, which is where
    /// X.691's length determinant changes from one octet to two.
    ///
    /// That boundary is a real cliff — an encoder that only ever emitted the short form
    /// would be correct for every small message and wrong for every large one — so this
    /// asserts the large case is reachable from a *valid* message rather than only from a
    /// contrived one. The oracle's `boundary/partII-full` vector is this message, and it
    /// matched pycrate byte for byte.
    #[test]
    fn a_full_path_history_crosses_the_two_octet_length_boundary() {
        let vse = VehicleSafetyExtensions {
            events: Some(VehicleEventFlags(VehicleEventFlags::ROOT_MASK)),
            path_history: Some(PathHistory {
                gnss_status: Some(GnssStatus(0xff)),
                crumb_data: (0..MAX_PATH_HISTORY_POINTS)
                    .map(|i| PathHistoryPoint {
                        speed: Some(i as u16),
                        pos_accuracy: Some(PositionalAccuracy::UNAVAILABLE),
                        heading: Some(COARSE_HEADING_UNAVAILABLE),
                        ..PathHistoryPoint::new(1, -1, 1, TIME_OFFSET_MAX as u16)
                    })
                    .collect(),
            }),
            path_prediction: Some(PathPrediction::straight(200)),
            lights: Some(ExteriorLights(ExteriorLights::ROOT_MASK)),
        };
        let bsm = BasicSafetyMessage::part_i(core()).with_vehicle_safety(vse);
        let e = encode_bsm(&bsm).expect("encodes");
        // Everything past the 37-octet Part I is the container plus its length
        // determinant, so this is a lower bound on the inner encoding.
        assert!(
            e.size - PART_I_ONLY_SIZE_B > 127,
            "a full path history should need the two-octet length form, got {} octets",
            e.size
        );
        assert_eq!(decode_bsm(&e.bytes).expect("decodes"), bsm);
    }

    #[test]
    fn an_opaque_part_ii_container_survives_byte_for_byte() {
        let opaque = vec![0x12, 0x34, 0x56];
        let bsm = BasicSafetyMessage {
            core: core(),
            part_ii: vec![
                PartIIContent::vehicle_safety(VehicleSafetyExtensions {
                    lights: Some(ExteriorLights::HIGH_BEAM),
                    ..Default::default()
                }),
                PartIIContent {
                    id: PART_II_SUPPLEMENTAL_VEHICLE,
                    value: PartIIValue::Opaque(opaque.clone()),
                },
            ],
        };
        let e = encode_bsm(&bsm).expect("encodes");
        let back = decode_bsm(&e.bytes).expect("decodes");
        assert_eq!(back, bsm);
        assert_eq!(
            back.part_ii[1].value,
            PartIIValue::Opaque(opaque),
            "an unmodelled container must come back unchanged"
        );
    }

    #[test]
    fn a_duplicate_part_ii_id_is_refused() {
        let bsm = BasicSafetyMessage {
            core: core(),
            part_ii: vec![
                PartIIContent::vehicle_safety(VehicleSafetyExtensions::default()),
                PartIIContent::vehicle_safety(VehicleSafetyExtensions::default()),
            ],
        };
        assert!(matches!(
            encode_bsm(&bsm),
            Err(CodecError::UnsupportedConstruct { .. })
        ));
    }

    /// Hand-builds a `BasicSafetyMessage` encoding with `count` Part II containers, each
    /// `(id, inner octets)`, writing the length determinant verbatim so a test can emit one
    /// no conforming encoder would.
    ///
    /// `raw_zero_length` writes the one-octet determinant 0x00 instead of calling
    /// [`write_open_type`], which is the whole point: the writer cannot produce that byte.
    fn hand_built_part_ii(containers: &[(u8, &[u8])], raw_zero_length: bool) -> Vec<u8> {
        let mut w = BitWriter::with_capacity(64);
        write_preamble(&mut w, true, &[true, false]);
        write_core_data(&mut w, &core()).expect("the core data is in range");
        write_constrained_length(
            &mut w,
            F_PART_II_LEN,
            containers.len(),
            1,
            MAX_PART_II_CONTAINERS,
        )
        .expect("the container count is in range");
        for (id, inner) in containers {
            write_constrained_int(
                &mut w,
                F_PART_II_ID,
                i64::from(*id),
                0,
                i64::from(PART_II_ID_MAX),
            )
            .expect("the id is in range");
            if raw_zero_length {
                // The one-octet length determinant, value 0 (X.691 clause 11.9.3.6). No
                // conforming encoder emits it for an open type; clause 11.2.2 makes the
                // empty inner encoding a single zero octet instead.
                w.write_bits(0, 8);
            } else {
                write_open_type(&mut w, "partII-Value", inner).expect("writes the container");
            }
        }
        w.into_bytes()
    }

    /// A zero-length open type is not an empty container: X.691 clause 11.2.2 makes an open
    /// type at least one octet. The decoder used to return `Opaque([])`, which re-encodes
    /// one octet longer, so the seam reported "38 octets in, 39 octets out" and named
    /// nothing. The refusal now names the determinant, at the message that is wrong.
    #[test]
    fn a_zero_length_part_ii_open_type_is_refused_at_the_decode() {
        let bytes = hand_built_part_ii(&[(1, &[])], true);
        let err = decode_bsm(&bytes).expect_err("clause 11.2.2 forbids a zero-length open type");
        let text = err.to_string();
        assert!(text.contains("11.2.2"), "{text}");
        assert!(text.contains("partII-Value"), "{text}");
        assert!(
            !text.contains("canonical UPER form"),
            "the reason must be named here, not inferred from a byte count later: {text}"
        );

        // The legal one-octet form of the same container still decodes, and round-trips.
        let legal = hand_built_part_ii(&[(1, &[0x00])], false);
        let decoded = decode_bsm(&legal).expect("one zero octet is the legal empty form");
        assert_eq!(
            decoded.part_ii,
            vec![PartIIContent {
                id: 1,
                value: PartIIValue::Opaque(vec![0x00])
            }]
        );
        assert_eq!(
            encode_bsm(&decoded).expect("re-encodes").bytes,
            legal,
            "the legal form must be canonical"
        );
        assert_eq!(
            bytes.len() + 1,
            legal.len(),
            "the malformed form is one octet shorter"
        );
    }

    /// The writer has always refused a repeated `partII-Id`; the reader used to return one,
    /// so `decode_bsm` produced a message `encode_bsm` refuses to produce. Anything that
    /// decoded, inspected and re-encoded got the error at the far end instead of here.
    #[test]
    fn a_duplicate_part_ii_id_is_refused_by_the_reader_as_well_as_the_writer() {
        let bytes = hand_built_part_ii(&[(1, &[0xaa]), (1, &[0xbb])], false);
        let err = decode_bsm(&bytes).expect_err("the same container id twice");
        assert!(
            matches!(
                err,
                CodecError::UnsupportedConstruct {
                    construct: "BasicSafetyMessage.partII",
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("appears twice"), "{err}");

        // Encoder and decoder must refuse with the *same* words: one rule, one statement.
        let writable = BasicSafetyMessage {
            core: core(),
            part_ii: vec![
                PartIIContent {
                    id: 1,
                    value: PartIIValue::Opaque(vec![0xaa]),
                },
                PartIIContent {
                    id: 1,
                    value: PartIIValue::Opaque(vec![0xbb]),
                },
            ],
        };
        let write_err = encode_bsm(&writable).expect_err("the writer refuses it too");
        assert_eq!(write_err.to_string(), err.to_string());

        // Distinct ids in the same message are still fine, in both directions.
        let ok = hand_built_part_ii(&[(1, &[0xaa]), (2, &[0xbb])], false);
        let decoded = decode_bsm(&ok).expect("distinct ids are legal");
        assert_eq!(decoded.part_ii.len(), 2);
        assert_eq!(encode_bsm(&decoded).expect("re-encodes").bytes, ok);
    }

    #[test]
    fn an_out_of_range_field_names_itself() {
        let mut c = core();
        c.lat = 900_000_002;
        let err = encode_bsm(&BasicSafetyMessage::part_i(c)).expect_err("out of range");
        let text = err.to_string();
        assert!(
            text.contains("bsm.coreData.lat") && text.contains("Latitude"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_crumb_list_is_refused_rather_than_encoded_as_zero() {
        let bsm = BasicSafetyMessage::part_i(core()).with_vehicle_safety(VehicleSafetyExtensions {
            path_history: Some(PathHistory::new(Vec::new())),
            ..Default::default()
        });
        assert!(matches!(
            encode_bsm(&bsm),
            Err(CodecError::OutOfRange { .. })
        ));
    }

    #[test]
    fn truncated_bytes_do_not_decode_into_a_plausible_message() {
        let e = encode_bsm(&BasicSafetyMessage::part_i(core())).expect("encodes");
        for cut in 1..e.bytes.len() {
            assert!(
                decode_bsm(&e.bytes[..cut]).is_err(),
                "{cut} bytes should not decode"
            );
        }
        let mut longer = e.bytes.clone();
        longer.push(0);
        assert!(
            decode_bsm(&longer).is_err(),
            "a spare octet is not X.691 padding"
        );
    }

    #[test]
    fn the_builder_uses_the_belief_and_nothing_else() {
        let origin = GeoOrigin::new(40.7440, -73.9900, 0.0);
        let mut belief = PositionEstimate::no_fix(0);
        belief.pos = Vec3::new(120.0, 80.0, 12.5);
        belief.vel = Vec3::new(10.0, 0.0, 0.0);
        belief.heading_rad = 0.0; // due east in ENU
        belief.semi_major_m = 1.8;
        belief.semi_minor_m = 1.1;
        belief.orientation_rad = 0.0;
        belief.fix = FixQuality::ThreeD;

        let input = BsmInput::new(7, [1, 2, 3, 4], belief, origin, Dims::CAR, 4_321);
        let bsm = build_bsm(&input).expect("builds");

        // Due east in ENU is a bearing of 90°, which is 7200 in 0.0125° steps.
        assert_eq!(bsm.core.heading, 7_200);
        // 10 m/s in 0.02 m/s steps.
        assert_eq!(bsm.core.speed, 500);
        // 4.5 m and 1.8 m in centimetres.
        assert_eq!(bsm.core.size.length, 450);
        assert_eq!(bsm.core.size.width, 180);
        // 1.8 m of semi-major in 0.05 m steps.
        assert_eq!(bsm.core.accuracy.semi_major, 36);
        // Nothing was said about the dynamics, so nothing is claimed.
        assert_eq!(bsm.core.accel_set.long, ACCELERATION_UNAVAILABLE);
        assert_eq!(bsm.core.angle, STEERING_WHEEL_ANGLE_UNAVAILABLE);
        assert_eq!(bsm.core.transmission, TransmissionState::Unavailable);
        // And it encodes.
        assert_eq!(encode_bsm(&bsm).expect("encodes").size, PART_I_ONLY_SIZE_B);
    }

    #[test]
    fn a_node_with_no_fix_sends_unavailable_not_zero() {
        let origin = GeoOrigin::new(40.7440, -73.9900, 0.0);
        let belief = PositionEstimate::no_fix(0);
        let input = BsmInput::new(0, [0; 4], belief, origin, Dims::CAR, 0);
        let bsm = build_bsm(&input).expect("builds");
        assert_eq!(bsm.core.accuracy.semi_major, SEMI_AXIS_UNAVAILABLE);
        assert_eq!(bsm.core.accuracy.semi_minor, SEMI_AXIS_UNAVAILABLE);
    }

    #[test]
    fn sec_mark_counts_milliseconds_within_the_minute() {
        let clock = WallClock::parse_rfc3339("2026-09-18T12:00:30Z").expect("parses");
        assert_eq!(sec_mark(clock, 0), 30_000);
        assert_eq!(sec_mark(clock, 500_000_000), 30_500);
        // And it wraps at the minute rather than growing without bound.
        assert_eq!(sec_mark(clock, 30 * 1_000_000_000), 0);
    }

    #[test]
    fn the_two_exterior_light_masks_are_not_interchangeable() {
        // A reminder in test form: J2735 bit 2 is the left turn signal, CDD bit 4 is.
        assert_ne!(
            u16::from(crate::cam::ExteriorLightMask::LEFT_TURN.0),
            ExteriorLights::LEFT_TURN.0
        );
    }
}
