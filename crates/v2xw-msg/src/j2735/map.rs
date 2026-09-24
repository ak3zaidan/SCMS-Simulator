//! The SAE J2735 `MapData` message: intersection topology, hand-encoded.
//!
//! # Why this is hand-written
//!
//! Build decision D2, as for [`crate::j2735::bsm`] and [`crate::j2735::spat`]:
//! `rasn-compiler` cannot compile the J2735 modules, so a J2735 message the simulator
//! needs on the wire is written against the ASN.1 by hand over
//! [`crate::j2735::uper`].
//!
//! # Read this before quoting a MAP size: the structural assumptions are named, not hidden
//!
//! The J2735 modules are git-ignored (build decision D3) and **are not in this checkout**,
//! and the `pycrate` oracle environment is not on this machine either. A MAP is much more
//! structure than a SPaT — nested `SEQUENCE`s, two `CHOICE`s, five bit strings — and a
//! single wrong extension marker shifts every bit after it. Rather than pretend to a
//! certainty this module does not have, every structural choice that rests on recall
//! alone is a **named constant** in the [`assumptions`] module, used by the encoder and the
//! decoder alike, so one oracle run can settle each of them with a one-line change and a
//! reviewer can see the whole list without reading the code.
//!
//! What *is* corroborated by artefacts in this repository:
//!
//! | Element | Evidence |
//! |---|---|
//! | `MapData` preamble and the `IntersectionGeometry` `id`/`refPoint`/`laneWidth` layout | the derivation table in [`crate::size_model`], written while the modules were on disk: "MessageFrame 4 B + `MapData` preamble + `IntersectionGeometry` id and `refPoint` (lat 32 b + long 32 b + elevation 16 b) + `laneWidth` 16 b + list determinants" |
//! | `IntersectionReferenceID`: 1 optional bit, no extension bit | same table, "`id` 1+16" — the same type this module and [`crate::j2735::spat`] share |
//! | `Latitude`, `Longitude`, `Elevation` ranges | the oracle-validated constants of [`crate::j2735::bsm`], reused here rather than restated |
//! | `MsgCount` 7 bits | likewise |
//! | The `Node-XY-20b` … `Node-XY-32b` widths | self-corroborating: the type names state the total width, and 2 × `Offset-B10` = 20 b, 2 × `Offset-B11` = 22 b, … 2 × `Offset-B16` = 32 b all match |
//! | `DSRCmsgID mapData(18)` | recalled, anchored by the oracle-validated `basicSafetyMessage(20)`; 18, 19 and 20 are consecutive in one list |
//!
//! Everything else — `LaneWidth (0..32767)`, `LaneID (0..255)`, `ApproachID (0..15)`, the
//! bit-string sizes, the `CHOICE` alternative counts and every extension marker — is
//! recalled from SAE J2735 2024-09 and **not re-read**. [`crate::evidence`] records the
//! consequence: a MAP from this codec is real UPER of a real structure, and it is *not*
//! byte-exactness-proven the way a BSM or a CAM is.
//!
//! # The shape of a MAP, as encoded here
//!
//! ```text
//! MapData                     extensible SEQUENCE, 8 optional fields
//! ├── timeStamp     OPTIONAL   MinuteOfTheYear
//! ├── msgIssueRevision         MsgCount
//! ├── layerType     OPTIONAL   refused
//! ├── layerID       OPTIONAL   refused
//! ├── intersections OPTIONAL   1..32 IntersectionGeometry
//! │   ├── name      OPTIONAL   refused
//! │   ├── id                   IntersectionReferenceID
//! │   ├── revision             MsgCount
//! │   ├── refPoint             Position3D { lat, long, elevation OPTIONAL }
//! │   ├── laneWidth OPTIONAL   LaneWidth, centimetres
//! │   ├── speedLimits OPT      refused
//! │   ├── laneSet              1..255 GenericLane
//! │   │   ├── laneID           LaneID
//! │   │   ├── name  OPTIONAL   refused
//! │   │   ├── ingressApproach OPT  ApproachID
//! │   │   ├── egressApproach  OPT  ApproachID
//! │   │   ├── laneAttributes   { directionalUse, sharedWith, laneType: vehicle only }
//! │   │   ├── maneuvers OPT    AllowedManeuvers
//! │   │   ├── nodeList         NodeListXY: nodes only (2..63 NodeXY), computed refused
//! │   │   ├── connectsTo OPT   1..16 Connection
//! │   │   ├── overlays  OPT    refused
//! │   │   └── regional  OPT    refused
//! │   ├── preemptPriorityData OPT  refused
//! │   └── regional  OPTIONAL   refused
//! ├── roadSegments  OPTIONAL   refused
//! ├── dataParameters OPTIONAL  refused
//! ├── restrictionList OPTIONAL refused
//! └── regional      OPTIONAL   refused
//! ```
//!
//! *Refused* means [`CodecError::UnsupportedConstruct`] on decode and never written on
//! encode. Nothing is skipped: a PER decoder that stepped over an element it did not model
//! would misread every element after it.
//!
//! One consequence worth stating: `layerType` and `layerID` are refused, and CTI 4501
//! expects both in a US deployment's MAP, so a message from this codec is a conformant
//! `MapData` *encoding* but not a CTI-conformant *message*. The model card says so.

use v2xw_core::math;
use v2xw_core::time::{SimTime, WallClock};

use crate::codec::{Encoded, MsgType};
use crate::error::CodecError;
use crate::j2735::bsm::{
    ELEVATION_MAX, ELEVATION_MIN, LATITUDE_MAX, LATITUDE_MIN, LONGITUDE_MAX, LONGITUDE_MIN,
    MSG_COUNT_MAX, MSG_COUNT_MIN,
};
use crate::j2735::spat::{
    IntersectionReferenceId, MINUTE_OF_THE_YEAR_MAX, MINUTE_OF_THE_YEAR_MIN, SIGNAL_GROUP_ID_MAX,
    SIGNAL_GROUP_ID_MIN,
};
use crate::j2735::uper::{
    BitReader, BitWriter, Field, UperError, read_choice_index, read_constrained_int,
    read_constrained_length, read_fixed_bit_string, read_open_type, read_preamble,
    write_choice_index, write_constrained_int, write_constrained_length, write_fixed_bit_string,
    write_open_type, write_preamble,
};

/// The structural choices that rest on recall rather than on a readable module.
///
/// Each is used by both the encoder and the decoder, so the two can never disagree, and
/// each is one line to change once an oracle run says what the ASN.1 really declares. A
/// `true` here means "this `SEQUENCE` or `CHOICE` carries `...`, so its encoding starts
/// with an extension bit".
///
/// Why constants rather than just getting it right: a wrong extension marker is not a
/// wrong field, it is a one-bit shift of everything after it, and it is invisible to a
/// round-trip test because the decoder makes the same assumption as the encoder. Naming
/// the assumption is the only way a reader can tell which parts of a MAP's byte count are
/// certain.
pub mod assumptions {
    /// `MapData` carries `...`. High confidence: every top-level J2735 message does, and
    /// the size-model derivation counted an extension bit for `SPAT`.
    pub const EXT_MAP_DATA: bool = true;
    /// `IntersectionGeometry` carries `...`. High confidence, by analogy with
    /// `IntersectionState`, whose 1 + 6 preamble the size-model derivation records.
    pub const EXT_INTERSECTION_GEOMETRY: bool = true;
    /// `Position3D` carries `...`. **Recalled.** Medium confidence.
    pub const EXT_POSITION_3D: bool = true;
    /// `GenericLane` carries `...`. **Recalled.** Medium-high confidence.
    pub const EXT_GENERIC_LANE: bool = true;
    /// `LaneAttributes` carries `...`. **Recalled, and the least certain entry here.** The
    /// small shared DSRC `SEQUENCE`s tend not to: `IntersectionReferenceID` demonstrably
    /// does not (its preamble is 1 bit for 1 optional field), and this is modelled the
    /// same way.
    pub const EXT_LANE_ATTRIBUTES: bool = false;
    /// `NodeXY` carries `...`. **Recalled.** Medium confidence.
    pub const EXT_NODE_XY: bool = true;
    /// `NodeListXY`, a `CHOICE`, carries `...`. **Recalled.** Medium confidence.
    pub const EXT_NODE_LIST_XY: bool = true;
    /// `NodeOffsetPointXY`, a `CHOICE`, carries `...`. **Recalled**: its eighth
    /// alternative is `regional`, which is how J2735 makes a `CHOICE` extensible without
    /// an extension marker, so this is modelled as non-extensible with eight root
    /// alternatives.
    pub const EXT_NODE_OFFSET_POINT_XY: bool = false;
    /// `LaneTypeAttributes`, a `CHOICE`, carries `...`. **Recalled.** Medium confidence.
    pub const EXT_LANE_TYPE_ATTRIBUTES: bool = true;
    /// `Connection` carries `...`. **Recalled.** Modelled as non-extensible.
    pub const EXT_CONNECTION: bool = false;
    /// `ConnectingLane` carries `...`. **Recalled.** Modelled as non-extensible.
    pub const EXT_CONNECTING_LANE: bool = false;
    /// `Node-LLmD-64b` declares `lon` before `lat`, the opposite order from `Position3D`.
    /// **Recalled**, and a known oddity of the module rather than a typo here.
    pub const NODE_LATLON_ENCODES_LON_FIRST: bool = true;
}

use assumptions::*;

// =========================================================================================
// Constraints — SAE J2735 2024-09
// =========================================================================================

/// `DSRCmsgID mapData(18)`, the `MessageFrame` selector for a MAP.
pub const MAP_MESSAGE_ID: u16 = 18;

/// `LaneWidth ::= INTEGER (0..32767)`, lower bound. The unit is one centimetre.
pub const LANE_WIDTH_MIN: i64 = 0;
/// `LaneWidth`, upper bound: 327.67 m.
pub const LANE_WIDTH_MAX: i64 = 32_767;

/// `LaneID ::= INTEGER (0..255)`, lower bound. Zero means "no lane".
pub const LANE_ID_MIN: i64 = 0;
/// `LaneID`, upper bound.
pub const LANE_ID_MAX: i64 = 255;

/// `ApproachID ::= INTEGER (0..15)`, lower bound. Zero means "unknown approach".
pub const APPROACH_ID_MIN: i64 = 0;
/// `ApproachID`, upper bound.
pub const APPROACH_ID_MAX: i64 = 15;

/// `RestrictionClassID ::= INTEGER (0..255)`, lower bound.
pub const RESTRICTION_CLASS_ID_MIN: i64 = 0;
/// `RestrictionClassID`, upper bound.
pub const RESTRICTION_CLASS_ID_MAX: i64 = 255;

/// `LaneConnectionID ::= INTEGER (0..255)`, lower bound.
pub const LANE_CONNECTION_ID_MIN: i64 = 0;
/// `LaneConnectionID`, upper bound.
pub const LANE_CONNECTION_ID_MAX: i64 = 255;

/// `LaneDirection ::= BIT STRING (SIZE(2))`.
pub const LANE_DIRECTION_BITS: u32 = 2;
/// `LaneSharing ::= BIT STRING (SIZE(10))`.
pub const LANE_SHARING_BITS: u32 = 10;
/// `LaneAttributes-Vehicle ::= BIT STRING (SIZE(8))`.
pub const LANE_ATTRIBUTES_VEHICLE_BITS: u32 = 8;
/// `AllowedManeuvers ::= BIT STRING (SIZE(12))`.
pub const ALLOWED_MANEUVERS_BITS: u32 = 12;

/// `IntersectionGeometryList ::= SEQUENCE (SIZE(1..32)) OF IntersectionGeometry`.
pub const MAX_INTERSECTION_GEOMETRIES: usize = 32;
/// `LaneList ::= SEQUENCE (SIZE(1..255)) OF GenericLane`.
pub const MAX_LANES: usize = 255;
/// `NodeSetXY ::= SEQUENCE (SIZE(2..63)) OF NodeXY`: a lane's centre line needs at least
/// two points to be a line at all.
pub const MIN_NODES: usize = 2;
/// `NodeSetXY`, upper bound.
pub const MAX_NODES: usize = 63;
/// `ConnectsToList ::= SEQUENCE (SIZE(1..16)) OF Connection`.
pub const MAX_CONNECTIONS: usize = 16;

/// Root alternatives of `NodeOffsetPointXY`: six `node-XY*`, `node-LatLon`, `regional`.
pub const NODE_OFFSET_ALTERNATIVES: u64 = 8;
/// Choice index of `node-LatLon` in `NodeOffsetPointXY`.
pub const NODE_OFFSET_LATLON_INDEX: u64 = 6;
/// Choice index of `regional` in `NodeOffsetPointXY` — refused, never written.
pub const NODE_OFFSET_REGIONAL_INDEX: u64 = 7;

/// Root alternatives of `NodeListXY`: `nodes` and `computed`.
pub const NODE_LIST_ALTERNATIVES: u64 = 2;
/// Choice index of `NodeListXY.nodes`.
pub const NODE_LIST_NODES_INDEX: u64 = 0;

/// Root alternatives of `LaneTypeAttributes`: vehicle, crosswalk, bikeLane, sidewalk,
/// median, striping, trackedVehicle, parking.
pub const LANE_TYPE_ALTERNATIVES: u64 = 8;
/// Choice index of `LaneTypeAttributes.vehicle`, the one alternative this codec models.
pub const LANE_TYPE_VEHICLE_INDEX: u64 = 0;

/// Bytes of the smallest MAP this codec emits: one intersection, one lane, two nodes, no
/// optional field anywhere.
///
/// 224 bits exactly, and the arithmetic is worth writing out because it is the one number
/// in this module a reviewer can check without the ASN.1:
///
/// | Part | Bits | Running |
/// |---|---:|---:|
/// | `MapData` preamble (1 extension + 8 optional) | 9 | 9 |
/// | `msgIssueRevision` `MsgCount` | 7 | 16 |
/// | `IntersectionGeometryList` determinant, `SIZE(1..32)` | 5 | 21 |
/// | `IntersectionGeometry` preamble (1 + 5) | 6 | 27 |
/// | `id`: 1 optional bit + `IntersectionID` | 17 | 44 |
/// | `revision` | 7 | 51 |
/// | `refPoint` preamble (1 + 2) | 3 | 54 |
/// | `lat` (range 1 800 000 002) | 31 | 85 |
/// | `long` (range 3 600 000 001) | 32 | 117 |
/// | `LaneList` determinant, `SIZE(1..255)` | 8 | 125 |
/// | `GenericLane` preamble (1 + 7) | 8 | 133 |
/// | `laneID` | 8 | 141 |
/// | `LaneAttributes` preamble (0 + 1) | 1 | 142 |
/// | `directionalUse` + `sharedWith` | 12 | 154 |
/// | `laneType` choice (1 + 3) + `vehicle` | 12 | 166 |
/// | `nodeList` choice (1 + 1) | 2 | 168 |
/// | `NodeSetXY` determinant, `SIZE(2..63)` | 6 | 174 |
/// | two `NodeXY`: (1 + 1) preamble + 3-bit choice + 20-bit offset | 50 | 224 |
///
/// Change any assumption in [`assumptions`] and this number moves, which is exactly what
/// the test that pins it is for.
pub const MINIMAL_MAP_SIZE_B: u32 = 28;

/// The same message inside a `MessageFrame`: 1 extension bit + 15-bit `DSRCmsgID` + an
/// 8-bit length determinant + the 28 octets above.
pub const MINIMAL_MAP_MESSAGE_FRAME_SIZE_B: u32 = 31;

// =========================================================================================
// Bit-string flag sets
// =========================================================================================

/// `LaneDirection ::= BIT STRING (SIZE(2))` — which way traffic runs in a lane.
///
/// Right-aligned in a `u8`, ASN.1 bit `(0)` in the more significant of the two used bits,
/// the convention [`crate::j2735::uper::write_fixed_bit_string`] documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LaneDirection(pub u8);

impl LaneDirection {
    /// Neither bit set: the lane carries no traffic in either direction.
    pub const NONE: Self = Self(0);
    /// `ingressPath(0)`: traffic approaches the intersection along this lane.
    pub const INGRESS: Self = Self(0b10);
    /// `egressPath(1)`: traffic leaves the intersection along this lane.
    pub const EGRESS: Self = Self(0b01);
    /// Both, which is how a bidirectional or reversible lane is described.
    pub const BOTH: Self = Self(0b11);

    /// The union of two masks.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// `LaneSharing ::= BIT STRING (SIZE(10))` — who else uses this lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LaneSharing(pub u16);

impl LaneSharing {
    /// No bit set: the lane is not shared and no overlapping description is provided.
    pub const NONE: Self = Self(0);
    /// `overlappingLaneDescriptionProvided(0)`.
    pub const OVERLAPPING_DESCRIPTION_PROVIDED: Self = Self(1 << 9);
    /// `multipleLanesTreatedAsOneLane(1)`.
    pub const MULTIPLE_LANES_AS_ONE: Self = Self(1 << 8);
    /// `otherNonMotorizedTrafficTypes(2)`.
    pub const OTHER_NON_MOTORIZED: Self = Self(1 << 7);
    /// `individualMotorizedVehicleTraffic(3)`.
    pub const INDIVIDUAL_MOTORIZED: Self = Self(1 << 6);
    /// `busVehicleTraffic(4)`.
    pub const BUS: Self = Self(1 << 5);
    /// `taxiVehicleTraffic(5)`.
    pub const TAXI: Self = Self(1 << 4);
    /// `pedestriansTraffic(6)`.
    pub const PEDESTRIANS: Self = Self(1 << 3);
    /// `cyclistVehicleTraffic(7)`.
    pub const CYCLISTS: Self = Self(1 << 2);
    /// `trackedVehicleTraffic(8)`.
    pub const TRACKED_VEHICLES: Self = Self(1 << 1);
    /// `pedestrianTraffic(9)`.
    pub const PEDESTRIAN_TRAFFIC: Self = Self(1);

    /// The union of two masks.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// `LaneAttributes-Vehicle ::= BIT STRING (SIZE(8))` — a motor-vehicle lane's properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct VehicleLaneAttributes(pub u8);

impl VehicleLaneAttributes {
    /// An ordinary through lane: no bit set.
    pub const NONE: Self = Self(0);
    /// `isVehicleRevocableLane(0)`.
    pub const REVOCABLE: Self = Self(1 << 7);
    /// `isVehicleFlyOverLane(1)`.
    pub const FLY_OVER: Self = Self(1 << 6);
    /// `hovLaneUseOnly(2)`.
    pub const HOV_ONLY: Self = Self(1 << 5);
    /// `restrictedToBusUse(3)`.
    pub const BUS_ONLY: Self = Self(1 << 4);
    /// `restrictedToTaxiUse(4)`.
    pub const TAXI_ONLY: Self = Self(1 << 3);
    /// `restrictedFromPublicUse(5)`.
    pub const NOT_PUBLIC: Self = Self(1 << 2);
    /// `hasIRbeaconCoverage(6)`.
    pub const IR_BEACON_COVERAGE: Self = Self(1 << 1);
    /// `permissionOnRequest(7)`.
    pub const PERMISSION_ON_REQUEST: Self = Self(1);

    /// The union of two masks.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// `AllowedManeuvers ::= BIT STRING (SIZE(12))` — what a vehicle may do from a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct AllowedManeuvers(pub u16);

impl AllowedManeuvers {
    /// No manoeuvre allowed, which is not the same as "not stated": the field is optional
    /// and its absence is what says nothing is known.
    pub const NONE: Self = Self(0);
    /// `maneuverStraightAllowed(0)`.
    pub const STRAIGHT: Self = Self(1 << 11);
    /// `maneuverLeftAllowed(1)`.
    pub const LEFT: Self = Self(1 << 10);
    /// `maneuverRightAllowed(2)`.
    pub const RIGHT: Self = Self(1 << 9);
    /// `maneuverUTurnAllowed(3)`.
    pub const U_TURN: Self = Self(1 << 8);
    /// `maneuverLeftTurnOnRedAllowed(4)`.
    pub const LEFT_ON_RED: Self = Self(1 << 7);
    /// `maneuverRightTurnOnRedAllowed(5)`.
    pub const RIGHT_ON_RED: Self = Self(1 << 6);
    /// `maneuverLaneChangeAllowed(6)`.
    pub const LANE_CHANGE: Self = Self(1 << 5);
    /// `maneuverNoStoppingAllowed(7)`.
    pub const NO_STOPPING: Self = Self(1 << 4);
    /// `yieldAllwaysRequired(8)` — the standard's spelling.
    pub const YIELD_ALWAYS_REQUIRED: Self = Self(1 << 3);
    /// `goWithHalt(9)`.
    pub const GO_WITH_HALT: Self = Self(1 << 2);
    /// `caution(10)`.
    pub const CAUTION: Self = Self(1 << 1);
    /// `reserved1(11)`.
    pub const RESERVED1: Self = Self(1);

    /// The union of two masks.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

// =========================================================================================
// Node offsets
// =========================================================================================

/// Which `node-XY*` alternative of `NodeOffsetPointXY` a centimetre offset is carried in.
///
/// The alternative is part of the value, not a detail of the encoding: a decoded node
/// keeps the alternative it arrived in so that a decode followed by an encode reproduces
/// the bytes exactly. Two encodings of the same offset in different alternatives are both
/// legal, and a codec that silently re-chose would break the canonical-form check at the
/// [`crate::MessageCodec`] seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum XyAlternative {
    /// `node-XY1 Node-XY-20b`: two `Offset-B10`, ±5.12 m.
    Xy1,
    /// `node-XY2 Node-XY-22b`: two `Offset-B11`, ±10.24 m.
    Xy2,
    /// `node-XY3 Node-XY-24b`: two `Offset-B12`, ±20.48 m.
    Xy3,
    /// `node-XY4 Node-XY-26b`: two `Offset-B13`, ±40.96 m.
    Xy4,
    /// `node-XY5 Node-XY-28b`: two `Offset-B14`, ±81.92 m.
    Xy5,
    /// `node-XY6 Node-XY-32b`: two `Offset-B16`, ±327.68 m.
    Xy6,
}

impl XyAlternative {
    /// Every alternative, narrowest first.
    pub const ALL: [XyAlternative; 6] = [
        XyAlternative::Xy1,
        XyAlternative::Xy2,
        XyAlternative::Xy3,
        XyAlternative::Xy4,
        XyAlternative::Xy5,
        XyAlternative::Xy6,
    ];

    /// The `CHOICE` index, which is also the position in [`XyAlternative::ALL`].
    pub const fn index(self) -> u64 {
        match self {
            XyAlternative::Xy1 => 0,
            XyAlternative::Xy2 => 1,
            XyAlternative::Xy3 => 2,
            XyAlternative::Xy4 => 3,
            XyAlternative::Xy5 => 4,
            XyAlternative::Xy6 => 5,
        }
    }

    /// The alternative at `index`, or `None` for an index that is not a `node-XY*`.
    pub const fn from_index(index: u64) -> Option<Self> {
        Some(match index {
            0 => XyAlternative::Xy1,
            1 => XyAlternative::Xy2,
            2 => XyAlternative::Xy3,
            3 => XyAlternative::Xy4,
            4 => XyAlternative::Xy5,
            5 => XyAlternative::Xy6,
            _ => return None,
        })
    }

    /// Bits each of the two axes occupies.
    pub const fn bits_per_axis(self) -> u32 {
        match self {
            XyAlternative::Xy1 => 10,
            XyAlternative::Xy2 => 11,
            XyAlternative::Xy3 => 12,
            XyAlternative::Xy4 => 13,
            XyAlternative::Xy5 => 14,
            XyAlternative::Xy6 => 16,
        }
    }

    /// Lowest centimetre offset the alternative can carry: −2^(n−1).
    pub const fn min_cm(self) -> i64 {
        -(1i64 << (self.bits_per_axis() - 1))
    }

    /// Highest centimetre offset the alternative can carry: 2^(n−1) − 1.
    pub const fn max_cm(self) -> i64 {
        (1i64 << (self.bits_per_axis() - 1)) - 1
    }

    /// Whether both axes fit.
    pub const fn fits(self, x_cm: i32, y_cm: i32) -> bool {
        let (x, y) = (x_cm as i64, y_cm as i64);
        x >= self.min_cm() && x <= self.max_cm() && y >= self.min_cm() && y <= self.max_cm()
    }

    /// The narrowest alternative that carries both axes, or `None` beyond ±327.68 m —
    /// where the standard's answer is `node-LatLon`, not a wider offset.
    pub fn narrowest_for(x_cm: i32, y_cm: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.fits(x_cm, y_cm))
    }

    /// The ASN.1 alternative identifier, for diagnostics and the oracle's JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            XyAlternative::Xy1 => "node-XY1",
            XyAlternative::Xy2 => "node-XY2",
            XyAlternative::Xy3 => "node-XY3",
            XyAlternative::Xy4 => "node-XY4",
            XyAlternative::Xy5 => "node-XY5",
            XyAlternative::Xy6 => "node-XY6",
        }
    }
}

/// A `node-XY*` offset: centimetres east and north of the previous node, in the
/// alternative that carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct XyOffset {
    /// `x`: centimetres east. The standard's axes are the reference point's, and the
    /// simulator's world is ENU (build decision D6), so x is east and y is north.
    pub x_cm: i32,
    /// `y`: centimetres north.
    pub y_cm: i32,
    /// Which alternative carries them. [`XyOffset::narrowest`] picks the smallest.
    pub alternative: XyAlternative,
}

impl XyOffset {
    /// The offset in the narrowest alternative that holds it, or `None` beyond ±327.68 m.
    pub fn narrowest(x_cm: i32, y_cm: i32) -> Option<Self> {
        XyAlternative::narrowest_for(x_cm, y_cm).map(|alternative| Self {
            x_cm,
            y_cm,
            alternative,
        })
    }
}

/// `NodeOffsetPointXY` — where a node is, relative to the previous one or absolutely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeOffset {
    /// One of the six `node-XY*` alternatives: a centimetre offset from the previous node,
    /// or from the intersection reference point for the first node in a lane.
    Xy(XyOffset),
    /// `node-LatLon Node-LLmD-64b`: an absolute position, which the standard provides for
    /// offsets too large for the widest `node-XY*`.
    LatLon {
        /// `lon`, in tenths of a microdegree. Encoded **before** `lat`, per
        /// [`assumptions::NODE_LATLON_ENCODES_LON_FIRST`].
        lon: i32,
        /// `lat`, in tenths of a microdegree.
        lat: i32,
    },
}

/// `NodeXY ::= SEQUENCE { delta NodeOffsetPointXY, attributes NodeAttributeSetXY OPTIONAL }`
///
/// `attributes` is not modelled: it is refused on decode and never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeXy {
    /// `delta`: the offset itself.
    pub delta: NodeOffset,
}

impl NodeXy {
    /// A node at a centimetre offset, in the narrowest alternative that holds it.
    pub fn offset(x_cm: i32, y_cm: i32) -> Option<Self> {
        XyOffset::narrowest(x_cm, y_cm).map(|delta| Self {
            delta: NodeOffset::Xy(delta),
        })
    }
}

// =========================================================================================
// Lane and intersection structures
// =========================================================================================

/// `LaneAttributes ::= SEQUENCE { directionalUse, sharedWith, laneType, regional OPTIONAL }`
///
/// `laneType` is modelled as `vehicle` only: the other seven alternatives (crosswalk,
/// bikeLane, sidewalk, median, striping, trackedVehicle, parking) are refused on decode
/// and cannot be constructed here, because a codec that guessed at a 16-bit bit string it
/// had never read would shift every bit after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LaneAttributes {
    /// `directionalUse`.
    pub directional_use: LaneDirection,
    /// `sharedWith`.
    pub shared_with: LaneSharing,
    /// `laneType`, restricted to the `vehicle` alternative.
    pub vehicle: VehicleLaneAttributes,
}

impl LaneAttributes {
    /// An ordinary motor-vehicle lane running one way.
    pub const fn vehicle(directional_use: LaneDirection) -> Self {
        Self {
            directional_use,
            shared_with: LaneSharing::NONE,
            vehicle: VehicleLaneAttributes::NONE,
        }
    }
}

/// `ConnectingLane ::= SEQUENCE { lane LaneID, maneuver AllowedManeuvers OPTIONAL }`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ConnectingLane {
    /// `lane`: the egress lane this connection leads to.
    pub lane: u8,
    /// `maneuver`: what a vehicle does to take it.
    pub maneuver: Option<AllowedManeuvers>,
}

/// `Connection ::= SEQUENCE { … }` — one movement out of a lane.
///
/// `signalGroup` is what ties a MAP to a SPaT: it names the
/// [`crate::j2735::spat::MovementState`] that says whether this movement may proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Connection {
    /// `connectingLane`.
    pub connecting_lane: ConnectingLane,
    /// `remoteIntersection`: for a connection that leaves this intersection's map.
    pub remote_intersection: Option<IntersectionReferenceId>,
    /// `signalGroup`: the SPaT movement state that governs this connection.
    pub signal_group: Option<u8>,
    /// `userClass`: `RestrictionClassID`, which class of user the connection applies to.
    pub user_class: Option<u8>,
    /// `connectionID`: `LaneConnectionID`, referenced by a `ManeuverAssistList`.
    pub connection_id: Option<u8>,
}

impl Connection {
    /// A connection to `lane` governed by `signal_group`.
    pub const fn signalised(lane: u8, signal_group: u8) -> Self {
        Self {
            connecting_lane: ConnectingLane {
                lane,
                maneuver: None,
            },
            remote_intersection: None,
            signal_group: Some(signal_group),
            user_class: None,
            connection_id: None,
        }
    }
}

/// `GenericLane ::= SEQUENCE { … }` — one lane's identity, attributes and centre line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GenericLane {
    /// `laneID`, unique within the intersection.
    pub lane_id: u8,
    /// `ingressApproach`: which approach the lane belongs to, if it is an ingress lane.
    pub ingress_approach: Option<u8>,
    /// `egressApproach`.
    pub egress_approach: Option<u8>,
    /// `laneAttributes`.
    pub attributes: LaneAttributes,
    /// `maneuvers`.
    pub maneuvers: Option<AllowedManeuvers>,
    /// `nodeList`: 2..63 nodes, the `nodes` alternative of `NodeListXY`. The first offset
    /// is from the intersection's reference point and each later one from its predecessor.
    pub nodes: Vec<NodeXy>,
    /// `connectsTo`: 0..16 connections. Empty means the field is absent, which is how a
    /// MAP says nothing about where the lane leads.
    pub connects_to: Vec<Connection>,
}

/// `Position3D ::= SEQUENCE { lat, long, elevation OPTIONAL, regional OPTIONAL }`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position3D {
    /// `lat`, tenths of a microdegree, over the `Latitude` range
    /// [`crate::j2735::bsm::LATITUDE_MIN`] to [`crate::j2735::bsm::LATITUDE_MAX`].
    pub lat: i32,
    /// `long`, tenths of a microdegree.
    pub lon: i32,
    /// `elevation`, decimetres, over the `Elevation` range.
    pub elevation: Option<i32>,
}

/// `IntersectionGeometry ::= SEQUENCE { … }` — one intersection's topology.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntersectionGeometry {
    /// `id`: the same value the SPaT's `IntersectionState` carries.
    pub id: IntersectionReferenceId,
    /// `revision`: `MsgCount`, bumped when the geometry changes.
    pub revision: u8,
    /// `refPoint`: the origin every node offset in this intersection is measured from.
    pub ref_point: Position3D,
    /// `laneWidth`: the default lane width in centimetres.
    pub lane_width_cm: Option<u16>,
    /// `laneSet`: 1..255 lanes.
    pub lanes: Vec<GenericLane>,
}

/// The `MapData` PDU, as this codec models it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MapData {
    /// `timeStamp`: minute of the year the map was issued in.
    pub time_stamp: Option<u32>,
    /// `msgIssueRevision`: `MsgCount`, bumped when the map changes. A receiver matches it
    /// against the SPaT's `revision` to know the two describe the same layout.
    pub msg_issue_revision: u8,
    /// `intersections`: 1..32 geometries. Empty means the field is absent, which is a
    /// `MapData` that describes nothing — legal, and refused here, because nothing in the
    /// simulator has a reason to send one.
    pub intersections: Vec<IntersectionGeometry>,
}

// =========================================================================================
// Field descriptors
// =========================================================================================

const F_MAP_TIME_STAMP: Field = Field::new("mapData.timeStamp", "MinuteOfTheYear");
const F_MSG_ISSUE_REVISION: Field = Field::new("mapData.msgIssueRevision", "MsgCount");
const F_INTERSECTIONS_LEN: Field = Field::new(
    "mapData.intersections",
    "SEQUENCE (SIZE(1..32)) OF IntersectionGeometry",
);
const F_IG_REVISION: Field = Field::new("intersectionGeometry.revision", "MsgCount");
const F_LAT: Field = Field::new("position3D.lat", "Latitude");
const F_LON: Field = Field::new("position3D.long", "Longitude");
const F_ELEV: Field = Field::new("position3D.elevation", "Elevation");
const F_LANE_WIDTH: Field = Field::new("intersectionGeometry.laneWidth", "LaneWidth");
const F_LANES_LEN: Field = Field::new(
    "intersectionGeometry.laneSet",
    "SEQUENCE (SIZE(1..255)) OF GenericLane",
);
const F_LANE_ID: Field = Field::new("genericLane.laneID", "LaneID");
const F_INGRESS_APPROACH: Field = Field::new("genericLane.ingressApproach", "ApproachID");
const F_EGRESS_APPROACH: Field = Field::new("genericLane.egressApproach", "ApproachID");
const F_DIRECTIONAL_USE: Field = Field::new("laneAttributes.directionalUse", "LaneDirection");
const F_SHARED_WITH: Field = Field::new("laneAttributes.sharedWith", "LaneSharing");
const F_LANE_TYPE: Field = Field::new("laneAttributes.laneType", "LaneTypeAttributes");
const F_LANE_TYPE_VEHICLE: Field =
    Field::new("laneAttributes.laneType.vehicle", "LaneAttributes-Vehicle");
const F_MANEUVERS: Field = Field::new("genericLane.maneuvers", "AllowedManeuvers");
const F_NODE_LIST: Field = Field::new("genericLane.nodeList", "NodeListXY");
const F_NODES_LEN: Field = Field::new("nodeListXY.nodes", "SEQUENCE (SIZE(2..63)) OF NodeXY");
const F_NODE_OFFSET: Field = Field::new("nodeXY.delta", "NodeOffsetPointXY");
const F_NODE_X: Field = Field::new("nodeXY.delta.x", "Offset-B10..B16");
const F_NODE_Y: Field = Field::new("nodeXY.delta.y", "Offset-B10..B16");
const F_NODE_LAT: Field = Field::new("nodeXY.delta.node-LatLon.lat", "Latitude");
const F_NODE_LON: Field = Field::new("nodeXY.delta.node-LatLon.lon", "Longitude");
const F_CONNECTIONS_LEN: Field = Field::new(
    "genericLane.connectsTo",
    "SEQUENCE (SIZE(1..16)) OF Connection",
);
const F_CONNECTING_LANE: Field = Field::new("connection.connectingLane.lane", "LaneID");
const F_CONNECTING_MANEUVER: Field =
    Field::new("connection.connectingLane.maneuver", "AllowedManeuvers");
const F_CONNECTION_SIGNAL_GROUP: Field = Field::new("connection.signalGroup", "SignalGroupID");
const F_USER_CLASS: Field = Field::new("connection.userClass", "RestrictionClassID");
const F_CONNECTION_ID: Field = Field::new("connection.connectionID", "LaneConnectionID");
const F_MESSAGE_ID: Field = Field::new("messageFrame.messageId", "DSRCmsgID");

// =========================================================================================
// Encoding
// =========================================================================================

fn write_position_3d(w: &mut BitWriter, p: &Position3D) -> Result<(), UperError> {
    // regional is never written.
    write_preamble(w, EXT_POSITION_3D, &[p.elevation.is_some(), false]);
    write_constrained_int(w, F_LAT, i64::from(p.lat), LATITUDE_MIN, LATITUDE_MAX)?;
    write_constrained_int(w, F_LON, i64::from(p.lon), LONGITUDE_MIN, LONGITUDE_MAX)?;
    if let Some(elevation) = p.elevation {
        write_constrained_int(
            w,
            F_ELEV,
            i64::from(elevation),
            ELEVATION_MIN,
            ELEVATION_MAX,
        )?;
    }
    Ok(())
}

fn read_position_3d(r: &mut BitReader<'_>) -> Result<Position3D, UperError> {
    let pre = read_preamble(r, "Position3D", EXT_POSITION_3D, 2)?;
    let lat = read_constrained_int(r, F_LAT, LATITUDE_MIN, LATITUDE_MAX)? as i32;
    let lon = read_constrained_int(r, F_LON, LONGITUDE_MIN, LONGITUDE_MAX)? as i32;
    let elevation = if pre.has(0) {
        Some(read_constrained_int(r, F_ELEV, ELEVATION_MIN, ELEVATION_MAX)? as i32)
    } else {
        None
    };
    if pre.has(1) {
        return Err(UperError::Unsupported {
            construct: "Position3D.regional",
            detail: "a regional extension is present; no Reg-Position3D object is modelled, \
                     and its open type cannot be interpreted",
        });
    }
    Ok(Position3D {
        lat,
        lon,
        elevation,
    })
}

fn write_node_offset(w: &mut BitWriter, offset: &NodeOffset) -> Result<(), UperError> {
    match offset {
        NodeOffset::Xy(xy) => {
            if !xy.alternative.fits(xy.x_cm, xy.y_cm) {
                // The alternative is part of the value, so a caller can build one that is
                // too narrow for its own offsets. Refuse rather than widen: widening would
                // change the bytes a caller believed it had chosen.
                return Err(UperError::OutOfRange {
                    field: F_NODE_OFFSET.path,
                    asn1_type: xy.alternative.as_str(),
                    // `unsigned_abs`, not `abs`: `i32::MIN.abs()` panics in a debug
                    // build, and the offset is caller-supplied.
                    value: i64::from(xy.x_cm.unsigned_abs().max(xy.y_cm.unsigned_abs())),
                    min: xy.alternative.min_cm(),
                    max: xy.alternative.max_cm(),
                });
            }
            write_choice_index(
                w,
                F_NODE_OFFSET,
                EXT_NODE_OFFSET_POINT_XY,
                xy.alternative.index(),
                NODE_OFFSET_ALTERNATIVES,
            )?;
            let bits = xy.alternative.bits_per_axis();
            let (min, max) = (xy.alternative.min_cm(), xy.alternative.max_cm());
            debug_assert_eq!(super::uper::constrained_width(1u64 << bits), bits);
            write_constrained_int(w, F_NODE_X, i64::from(xy.x_cm), min, max)?;
            write_constrained_int(w, F_NODE_Y, i64::from(xy.y_cm), min, max)?;
            Ok(())
        }
        NodeOffset::LatLon { lon, lat } => {
            write_choice_index(
                w,
                F_NODE_OFFSET,
                EXT_NODE_OFFSET_POINT_XY,
                NODE_OFFSET_LATLON_INDEX,
                NODE_OFFSET_ALTERNATIVES,
            )?;
            // Node-LLmD-64b declares lon before lat, unlike Position3D.
            debug_assert!(NODE_LATLON_ENCODES_LON_FIRST);
            write_constrained_int(w, F_NODE_LON, i64::from(*lon), LONGITUDE_MIN, LONGITUDE_MAX)?;
            write_constrained_int(w, F_NODE_LAT, i64::from(*lat), LATITUDE_MIN, LATITUDE_MAX)?;
            Ok(())
        }
    }
}

fn read_node_offset(r: &mut BitReader<'_>) -> Result<NodeOffset, UperError> {
    let index = read_choice_index(
        r,
        F_NODE_OFFSET,
        "NodeOffsetPointXY",
        EXT_NODE_OFFSET_POINT_XY,
        NODE_OFFSET_ALTERNATIVES,
    )?;
    if let Some(alternative) = XyAlternative::from_index(index) {
        let (min, max) = (alternative.min_cm(), alternative.max_cm());
        let x_cm = read_constrained_int(r, F_NODE_X, min, max)? as i32;
        let y_cm = read_constrained_int(r, F_NODE_Y, min, max)? as i32;
        return Ok(NodeOffset::Xy(XyOffset {
            x_cm,
            y_cm,
            alternative,
        }));
    }
    if index == NODE_OFFSET_LATLON_INDEX {
        let lon = read_constrained_int(r, F_NODE_LON, LONGITUDE_MIN, LONGITUDE_MAX)? as i32;
        let lat = read_constrained_int(r, F_NODE_LAT, LATITUDE_MIN, LATITUDE_MAX)? as i32;
        return Ok(NodeOffset::LatLon { lon, lat });
    }
    debug_assert_eq!(index, NODE_OFFSET_REGIONAL_INDEX);
    Err(UperError::Unsupported {
        construct: "NodeOffsetPointXY.regional",
        detail: "the node offset selects the regional alternative; no \
                 Reg-NodeOffsetPointXY object is modelled, and its open type cannot be \
                 interpreted",
    })
}

fn write_node_xy(w: &mut BitWriter, node: &NodeXy) -> Result<(), UperError> {
    // attributes is never written.
    write_preamble(w, EXT_NODE_XY, &[false]);
    write_node_offset(w, &node.delta)
}

fn read_node_xy(r: &mut BitReader<'_>) -> Result<NodeXy, UperError> {
    let pre = read_preamble(r, "NodeXY", EXT_NODE_XY, 1)?;
    let delta = read_node_offset(r)?;
    if pre.has(0) {
        return Err(UperError::Unsupported {
            construct: "NodeXY.attributes",
            detail: "a NodeAttributeSetXY is present; this codec models the offset only, \
                     and the attribute set cannot be skipped without losing bit \
                     synchronisation",
        });
    }
    Ok(NodeXy { delta })
}

fn write_lane_attributes(w: &mut BitWriter, a: &LaneAttributes) -> Result<(), UperError> {
    // regional is never written.
    write_preamble(w, EXT_LANE_ATTRIBUTES, &[false]);
    write_fixed_bit_string(
        w,
        F_DIRECTIONAL_USE,
        u64::from(a.directional_use.0),
        LANE_DIRECTION_BITS,
    )?;
    write_fixed_bit_string(
        w,
        F_SHARED_WITH,
        u64::from(a.shared_with.0),
        LANE_SHARING_BITS,
    )?;
    write_choice_index(
        w,
        F_LANE_TYPE,
        EXT_LANE_TYPE_ATTRIBUTES,
        LANE_TYPE_VEHICLE_INDEX,
        LANE_TYPE_ALTERNATIVES,
    )?;
    write_fixed_bit_string(
        w,
        F_LANE_TYPE_VEHICLE,
        u64::from(a.vehicle.0),
        LANE_ATTRIBUTES_VEHICLE_BITS,
    )
}

fn read_lane_attributes(r: &mut BitReader<'_>) -> Result<LaneAttributes, UperError> {
    let pre = read_preamble(r, "LaneAttributes", EXT_LANE_ATTRIBUTES, 1)?;
    let directional_use = LaneDirection(read_fixed_bit_string(r, LANE_DIRECTION_BITS)? as u8);
    let shared_with = LaneSharing(read_fixed_bit_string(r, LANE_SHARING_BITS)? as u16);
    let index = read_choice_index(
        r,
        F_LANE_TYPE,
        "LaneTypeAttributes",
        EXT_LANE_TYPE_ATTRIBUTES,
        LANE_TYPE_ALTERNATIVES,
    )?;
    if index != LANE_TYPE_VEHICLE_INDEX {
        return Err(UperError::Unsupported {
            construct: "LaneAttributes.laneType",
            detail: "the lane type is not `vehicle`; this codec models motor-vehicle lanes \
                     only, and the other alternatives' bit strings differ in length, so \
                     reading on would desynchronise every field after them",
        });
    }
    let vehicle =
        VehicleLaneAttributes(read_fixed_bit_string(r, LANE_ATTRIBUTES_VEHICLE_BITS)? as u8);
    if pre.has(0) {
        return Err(UperError::Unsupported {
            construct: "LaneAttributes.regional",
            detail: "a regional extension is present; no Reg-LaneAttributes object is \
                     modelled, and its open type cannot be interpreted",
        });
    }
    Ok(LaneAttributes {
        directional_use,
        shared_with,
        vehicle,
    })
}

fn write_connection(w: &mut BitWriter, c: &Connection) -> Result<(), UperError> {
    write_preamble(
        w,
        EXT_CONNECTION,
        &[
            c.remote_intersection.is_some(),
            c.signal_group.is_some(),
            c.user_class.is_some(),
            c.connection_id.is_some(),
        ],
    );
    // ConnectingLane: one optional field, its own preamble.
    write_preamble(
        w,
        EXT_CONNECTING_LANE,
        &[c.connecting_lane.maneuver.is_some()],
    );
    write_constrained_int(
        w,
        F_CONNECTING_LANE,
        i64::from(c.connecting_lane.lane),
        LANE_ID_MIN,
        LANE_ID_MAX,
    )?;
    if let Some(maneuver) = c.connecting_lane.maneuver {
        write_fixed_bit_string(
            w,
            F_CONNECTING_MANEUVER,
            u64::from(maneuver.0),
            ALLOWED_MANEUVERS_BITS,
        )?;
    }
    if let Some(remote) = &c.remote_intersection {
        crate::j2735::spat::write_reference_id_for(w, remote)?;
    }
    if let Some(group) = c.signal_group {
        write_constrained_int(
            w,
            F_CONNECTION_SIGNAL_GROUP,
            i64::from(group),
            SIGNAL_GROUP_ID_MIN,
            SIGNAL_GROUP_ID_MAX,
        )?;
    }
    if let Some(class) = c.user_class {
        write_constrained_int(
            w,
            F_USER_CLASS,
            i64::from(class),
            RESTRICTION_CLASS_ID_MIN,
            RESTRICTION_CLASS_ID_MAX,
        )?;
    }
    if let Some(id) = c.connection_id {
        write_constrained_int(
            w,
            F_CONNECTION_ID,
            i64::from(id),
            LANE_CONNECTION_ID_MIN,
            LANE_CONNECTION_ID_MAX,
        )?;
    }
    Ok(())
}

fn read_connection(r: &mut BitReader<'_>) -> Result<Connection, UperError> {
    let pre = read_preamble(r, "Connection", EXT_CONNECTION, 4)?;
    let lane_pre = read_preamble(r, "ConnectingLane", EXT_CONNECTING_LANE, 1)?;
    let lane = read_constrained_int(r, F_CONNECTING_LANE, LANE_ID_MIN, LANE_ID_MAX)? as u8;
    let maneuver = if lane_pre.has(0) {
        Some(AllowedManeuvers(
            read_fixed_bit_string(r, ALLOWED_MANEUVERS_BITS)? as u16,
        ))
    } else {
        None
    };
    let remote_intersection = if pre.has(0) {
        Some(crate::j2735::spat::read_reference_id_for(r)?)
    } else {
        None
    };
    let signal_group = if pre.has(1) {
        Some(read_constrained_int(
            r,
            F_CONNECTION_SIGNAL_GROUP,
            SIGNAL_GROUP_ID_MIN,
            SIGNAL_GROUP_ID_MAX,
        )? as u8)
    } else {
        None
    };
    let user_class = if pre.has(2) {
        Some(read_constrained_int(
            r,
            F_USER_CLASS,
            RESTRICTION_CLASS_ID_MIN,
            RESTRICTION_CLASS_ID_MAX,
        )? as u8)
    } else {
        None
    };
    let connection_id = if pre.has(3) {
        Some(read_constrained_int(
            r,
            F_CONNECTION_ID,
            LANE_CONNECTION_ID_MIN,
            LANE_CONNECTION_ID_MAX,
        )? as u8)
    } else {
        None
    };
    Ok(Connection {
        connecting_lane: ConnectingLane { lane, maneuver },
        remote_intersection,
        signal_group,
        user_class,
        connection_id,
    })
}

fn write_generic_lane(w: &mut BitWriter, lane: &GenericLane) -> Result<(), UperError> {
    if lane.nodes.len() < MIN_NODES || lane.nodes.len() > MAX_NODES {
        return Err(UperError::OutOfRange {
            field: F_NODES_LEN.path,
            asn1_type: F_NODES_LEN.asn1_type,
            value: lane.nodes.len() as i64,
            min: MIN_NODES as i64,
            max: MAX_NODES as i64,
        });
    }
    if lane.connects_to.len() > MAX_CONNECTIONS {
        return Err(UperError::OutOfRange {
            field: F_CONNECTIONS_LEN.path,
            asn1_type: F_CONNECTIONS_LEN.asn1_type,
            value: lane.connects_to.len() as i64,
            min: 1,
            max: MAX_CONNECTIONS as i64,
        });
    }
    // Optional fields in declaration order: name, ingressApproach, egressApproach,
    // maneuvers, connectsTo, overlays, regional. name, overlays and regional are never
    // written.
    write_preamble(
        w,
        EXT_GENERIC_LANE,
        &[
            false,
            lane.ingress_approach.is_some(),
            lane.egress_approach.is_some(),
            lane.maneuvers.is_some(),
            !lane.connects_to.is_empty(),
            false,
            false,
        ],
    );
    write_constrained_int(
        w,
        F_LANE_ID,
        i64::from(lane.lane_id),
        LANE_ID_MIN,
        LANE_ID_MAX,
    )?;
    if let Some(approach) = lane.ingress_approach {
        write_constrained_int(
            w,
            F_INGRESS_APPROACH,
            i64::from(approach),
            APPROACH_ID_MIN,
            APPROACH_ID_MAX,
        )?;
    }
    if let Some(approach) = lane.egress_approach {
        write_constrained_int(
            w,
            F_EGRESS_APPROACH,
            i64::from(approach),
            APPROACH_ID_MIN,
            APPROACH_ID_MAX,
        )?;
    }
    write_lane_attributes(w, &lane.attributes)?;
    if let Some(maneuvers) = lane.maneuvers {
        write_fixed_bit_string(
            w,
            F_MANEUVERS,
            u64::from(maneuvers.0),
            ALLOWED_MANEUVERS_BITS,
        )?;
    }
    // nodeList: the `nodes` alternative of an extensible CHOICE.
    write_choice_index(
        w,
        F_NODE_LIST,
        EXT_NODE_LIST_XY,
        NODE_LIST_NODES_INDEX,
        NODE_LIST_ALTERNATIVES,
    )?;
    write_constrained_length(w, F_NODES_LEN, lane.nodes.len(), MIN_NODES, MAX_NODES)?;
    for node in &lane.nodes {
        write_node_xy(w, node)?;
    }
    if !lane.connects_to.is_empty() {
        write_constrained_length(
            w,
            F_CONNECTIONS_LEN,
            lane.connects_to.len(),
            1,
            MAX_CONNECTIONS,
        )?;
        for connection in &lane.connects_to {
            write_connection(w, connection)?;
        }
    }
    Ok(())
}

fn read_generic_lane(r: &mut BitReader<'_>) -> Result<GenericLane, UperError> {
    let pre = read_preamble(r, "GenericLane", EXT_GENERIC_LANE, 7)?;
    if pre.has(0) {
        return Err(UperError::Unsupported {
            construct: "GenericLane.name",
            detail: "a DescriptiveName is present; this engine encodes no character \
                     strings, and an IA5String cannot be stepped over without losing bit \
                     synchronisation",
        });
    }
    let lane_id = read_constrained_int(r, F_LANE_ID, LANE_ID_MIN, LANE_ID_MAX)? as u8;
    let ingress_approach = if pre.has(1) {
        Some(read_constrained_int(r, F_INGRESS_APPROACH, APPROACH_ID_MIN, APPROACH_ID_MAX)? as u8)
    } else {
        None
    };
    let egress_approach = if pre.has(2) {
        Some(read_constrained_int(r, F_EGRESS_APPROACH, APPROACH_ID_MIN, APPROACH_ID_MAX)? as u8)
    } else {
        None
    };
    let attributes = read_lane_attributes(r)?;
    let maneuvers = if pre.has(3) {
        Some(AllowedManeuvers(
            read_fixed_bit_string(r, ALLOWED_MANEUVERS_BITS)? as u16,
        ))
    } else {
        None
    };
    let list_index = read_choice_index(
        r,
        F_NODE_LIST,
        "NodeListXY",
        EXT_NODE_LIST_XY,
        NODE_LIST_ALTERNATIVES,
    )?;
    if list_index != NODE_LIST_NODES_INDEX {
        return Err(UperError::Unsupported {
            construct: "NodeListXY.computed",
            detail: "the lane's geometry is a ComputedLane (an offset from another lane); \
                     this codec models explicit node lists only",
        });
    }
    let count = read_constrained_length(r, F_NODES_LEN, MIN_NODES, MAX_NODES)?;
    let mut nodes = Vec::with_capacity(count);
    for _ in 0..count {
        nodes.push(read_node_xy(r)?);
    }
    let mut connects_to = Vec::new();
    if pre.has(4) {
        let count = read_constrained_length(r, F_CONNECTIONS_LEN, 1, MAX_CONNECTIONS)?;
        connects_to.reserve(count);
        for _ in 0..count {
            connects_to.push(read_connection(r)?);
        }
    }
    if pre.has(5) {
        return Err(UperError::Unsupported {
            construct: "GenericLane.overlays",
            detail: "an OverlayLaneList is present; this codec does not model overlapping \
                     lanes and cannot skip the list",
        });
    }
    if pre.has(6) {
        return Err(UperError::Unsupported {
            construct: "GenericLane.regional",
            detail: "a regional extension is present; no Reg-GenericLane object is \
                     modelled, and its open type cannot be interpreted",
        });
    }
    Ok(GenericLane {
        lane_id,
        ingress_approach,
        egress_approach,
        attributes,
        maneuvers,
        nodes,
        connects_to,
    })
}

fn write_intersection_geometry(
    w: &mut BitWriter,
    g: &IntersectionGeometry,
) -> Result<(), UperError> {
    if g.lanes.is_empty() || g.lanes.len() > MAX_LANES {
        return Err(UperError::OutOfRange {
            field: F_LANES_LEN.path,
            asn1_type: F_LANES_LEN.asn1_type,
            value: g.lanes.len() as i64,
            min: 1,
            max: MAX_LANES as i64,
        });
    }
    // Optional fields in declaration order: name, laneWidth, speedLimits,
    // preemptPriorityData, regional. Only laneWidth is ever written.
    write_preamble(
        w,
        EXT_INTERSECTION_GEOMETRY,
        &[false, g.lane_width_cm.is_some(), false, false, false],
    );
    crate::j2735::spat::write_reference_id_for(w, &g.id)?;
    write_constrained_int(
        w,
        F_IG_REVISION,
        i64::from(g.revision),
        MSG_COUNT_MIN,
        MSG_COUNT_MAX,
    )?;
    write_position_3d(w, &g.ref_point)?;
    if let Some(width) = g.lane_width_cm {
        write_constrained_int(
            w,
            F_LANE_WIDTH,
            i64::from(width),
            LANE_WIDTH_MIN,
            LANE_WIDTH_MAX,
        )?;
    }
    write_constrained_length(w, F_LANES_LEN, g.lanes.len(), 1, MAX_LANES)?;
    for lane in &g.lanes {
        write_generic_lane(w, lane)?;
    }
    Ok(())
}

fn read_intersection_geometry(r: &mut BitReader<'_>) -> Result<IntersectionGeometry, UperError> {
    let pre = read_preamble(r, "IntersectionGeometry", EXT_INTERSECTION_GEOMETRY, 5)?;
    if pre.has(0) {
        return Err(UperError::Unsupported {
            construct: "IntersectionGeometry.name",
            detail: "a DescriptiveName is present; this engine encodes no character \
                     strings, and an IA5String cannot be stepped over without losing bit \
                     synchronisation",
        });
    }
    let id = crate::j2735::spat::read_reference_id_for(r)?;
    let revision = read_constrained_int(r, F_IG_REVISION, MSG_COUNT_MIN, MSG_COUNT_MAX)? as u8;
    let ref_point = read_position_3d(r)?;
    let lane_width_cm = if pre.has(1) {
        Some(read_constrained_int(r, F_LANE_WIDTH, LANE_WIDTH_MIN, LANE_WIDTH_MAX)? as u16)
    } else {
        None
    };
    if pre.has(2) {
        return Err(UperError::Unsupported {
            construct: "IntersectionGeometry.speedLimits",
            detail: "a SpeedLimitList is present; this codec does not model regulatory \
                     speed limits and cannot skip the list",
        });
    }
    let count = read_constrained_length(r, F_LANES_LEN, 1, MAX_LANES)?;
    let mut lanes = Vec::with_capacity(count);
    for _ in 0..count {
        lanes.push(read_generic_lane(r)?);
    }
    if pre.has(3) {
        return Err(UperError::Unsupported {
            construct: "IntersectionGeometry.preemptPriorityData",
            detail: "preemption and priority data is present; this codec does not model it \
                     and cannot skip it",
        });
    }
    if pre.has(4) {
        return Err(UperError::Unsupported {
            construct: "IntersectionGeometry.regional",
            detail: "a regional extension is present; no Reg-IntersectionGeometry object is \
                     modelled, and its open type cannot be interpreted",
        });
    }
    Ok(IntersectionGeometry {
        id,
        revision,
        ref_point,
        lane_width_cm,
        lanes,
    })
}

fn write_map(w: &mut BitWriter, map: &MapData) -> Result<(), UperError> {
    if map.intersections.is_empty() || map.intersections.len() > MAX_INTERSECTION_GEOMETRIES {
        return Err(UperError::OutOfRange {
            field: F_INTERSECTIONS_LEN.path,
            asn1_type: F_INTERSECTIONS_LEN.asn1_type,
            value: map.intersections.len() as i64,
            min: 1,
            max: MAX_INTERSECTION_GEOMETRIES as i64,
        });
    }
    // Optional fields in declaration order: timeStamp, layerType, layerID, intersections,
    // roadSegments, dataParameters, restrictionList, regional.
    write_preamble(
        w,
        EXT_MAP_DATA,
        &[
            map.time_stamp.is_some(),
            false,
            false,
            true,
            false,
            false,
            false,
            false,
        ],
    );
    if let Some(ts) = map.time_stamp {
        write_constrained_int(
            w,
            F_MAP_TIME_STAMP,
            i64::from(ts),
            MINUTE_OF_THE_YEAR_MIN,
            MINUTE_OF_THE_YEAR_MAX,
        )?;
    }
    write_constrained_int(
        w,
        F_MSG_ISSUE_REVISION,
        i64::from(map.msg_issue_revision),
        MSG_COUNT_MIN,
        MSG_COUNT_MAX,
    )?;
    write_constrained_length(
        w,
        F_INTERSECTIONS_LEN,
        map.intersections.len(),
        1,
        MAX_INTERSECTION_GEOMETRIES,
    )?;
    for geometry in &map.intersections {
        write_intersection_geometry(w, geometry)?;
    }
    Ok(())
}

fn read_map(r: &mut BitReader<'_>) -> Result<MapData, UperError> {
    let pre = read_preamble(r, "MapData", EXT_MAP_DATA, 8)?;
    let time_stamp = if pre.has(0) {
        Some(read_constrained_int(
            r,
            F_MAP_TIME_STAMP,
            MINUTE_OF_THE_YEAR_MIN,
            MINUTE_OF_THE_YEAR_MAX,
        )? as u32)
    } else {
        None
    };
    let msg_issue_revision =
        read_constrained_int(r, F_MSG_ISSUE_REVISION, MSG_COUNT_MIN, MSG_COUNT_MAX)? as u8;
    if pre.has(1) {
        return Err(UperError::Unsupported {
            construct: "MapData.layerType",
            detail: "a LayerType is present; this codec does not model the layer \
                     enumeration, whose width it has not verified, and a wrong width would \
                     desynchronise every field after it",
        });
    }
    if pre.has(2) {
        return Err(UperError::Unsupported {
            construct: "MapData.layerID",
            detail: "a LayerID is present; this codec does not model it and cannot skip it",
        });
    }
    let mut intersections = Vec::new();
    if pre.has(3) {
        let count =
            read_constrained_length(r, F_INTERSECTIONS_LEN, 1, MAX_INTERSECTION_GEOMETRIES)?;
        intersections.reserve(count);
        for _ in 0..count {
            intersections.push(read_intersection_geometry(r)?);
        }
    }
    for (index, construct, detail) in [
        (
            4usize,
            "MapData.roadSegments",
            "a RoadSegmentList is present; this codec models intersections only",
        ),
        (
            5,
            "MapData.dataParameters",
            "DataParameters are present; they carry character strings this engine cannot \
             read",
        ),
        (
            6,
            "MapData.restrictionList",
            "a RestrictionClassList is present; this codec does not model user-class \
             restrictions",
        ),
        (
            7,
            "MapData.regional",
            "a regional extension is present; no Reg-MapData object is modelled, and its \
             open type cannot be interpreted",
        ),
    ] {
        if pre.has(index) {
            return Err(UperError::Unsupported { construct, detail });
        }
    }
    if intersections.is_empty() {
        return Err(UperError::Unsupported {
            construct: "MapData.intersections",
            detail: "the message carries no intersection geometry; it is a legal MapData \
                     that describes nothing, and this codec has no representation for it",
        });
    }
    Ok(MapData {
        time_stamp,
        msg_issue_revision,
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
            ty: MsgType::Map,
            construct,
            detail,
        },
        other => CodecError::Encode {
            ty: MsgType::Map,
            detail: other.to_string(),
        },
    }
}

fn on_decode(len: usize) -> impl Fn(UperError) -> CodecError {
    move |e| match e {
        UperError::Unsupported { construct, detail } => CodecError::UnsupportedConstruct {
            ty: MsgType::Map,
            construct,
            detail,
        },
        other => CodecError::Decode {
            ty: MsgType::Map,
            len,
            detail: other.to_string(),
        },
    }
}

/// UPER-encodes a `MapData` PDU.
///
/// The bytes are real UPER of the subset documented at the top of this module, and its
/// size is measured rather than modelled — but the structure rests on the recalled
/// assumptions in [`assumptions`] and has not been oracle-validated. See
/// [`crate::evidence`].
pub fn encode_map(map: &MapData) -> Result<Encoded, CodecError> {
    let mut w = BitWriter::with_capacity(128);
    write_map(&mut w, map).map_err(on_encode)?;
    Ok(Encoded::uper(w.into_bytes()))
}

/// Decodes a `MapData` PDU, refusing trailing data beyond X.691's padding.
pub fn decode_map(bytes: &[u8]) -> Result<MapData, CodecError> {
    let map_err = on_decode(bytes.len());
    let mut r = BitReader::new(bytes);
    let map = read_map(&mut r).map_err(&map_err)?;
    r.finish().map_err(&map_err)?;
    Ok(map)
}

/// UPER-encodes a `MessageFrame` carrying this MAP.
pub fn encode_message_frame(map: &MapData) -> Result<Encoded, CodecError> {
    let mut inner = BitWriter::with_capacity(128);
    write_map(&mut inner, map).map_err(on_encode)?;
    let inner = inner.into_bytes();

    let mut w = BitWriter::with_capacity(inner.len() + 4);
    write_preamble(&mut w, true, &[]);
    write_constrained_int(
        &mut w,
        F_MESSAGE_ID,
        i64::from(MAP_MESSAGE_ID),
        0,
        i64::from(crate::j2735::bsm::DSRC_MSG_ID_MAX),
    )
    .map_err(on_encode)?;
    write_open_type(&mut w, "MessageFrame.value", &inner).map_err(on_encode)?;
    Ok(Encoded::uper(w.into_bytes()))
}

/// Decodes a `MessageFrame` and returns the MAP inside it.
///
/// Refuses any `DSRCmsgID` other than [`MAP_MESSAGE_ID`].
pub fn decode_message_frame(bytes: &[u8]) -> Result<MapData, CodecError> {
    let map_err = on_decode(bytes.len());
    let mut r = BitReader::new(bytes);
    read_preamble(&mut r, "MessageFrame", true, 0).map_err(&map_err)?;
    let id = read_constrained_int(
        &mut r,
        F_MESSAGE_ID,
        0,
        i64::from(crate::j2735::bsm::DSRC_MSG_ID_MAX),
    )
    .map_err(&map_err)? as u16;
    if id != MAP_MESSAGE_ID {
        return Err(CodecError::UnsupportedConstruct {
            ty: MsgType::Map,
            construct: "MessageFrame.messageId",
            detail: "the frame carries a DSRCmsgID other than mapData(18); this entry point \
                     decodes a MAP only",
        });
    }
    let inner = read_open_type(&mut r, "MessageFrame.value").map_err(&map_err)?;
    r.finish().map_err(&map_err)?;

    let mut ir = BitReader::new(&inner);
    let map = read_map(&mut ir).map_err(&map_err)?;
    ir.finish().map_err(&map_err)?;
    Ok(map)
}

// =========================================================================================
// From simulator quantities to wire units
// =========================================================================================

/// `MinuteOfTheYear` for a simulated instant — the same conversion the SPaT uses.
pub fn minute_of_the_year(clock: WallClock, t: SimTime) -> u32 {
    crate::j2735::spat::minute_of_the_year(clock, t)
}

/// `LaneWidth` in centimetres from a width in metres.
///
/// Quantises on the D9 metre grid before scaling, and clamps into the type's range rather
/// than wrapping: a negative or absurd width is a broken world file, and the clamp keeps
/// the message encodable while the world importer's own validation reports the cause.
pub fn lane_width_cm(m: f64) -> u16 {
    if !m.is_finite() || m <= 0.0 {
        return 0;
    }
    let cm = (math::quantize_to(m, crate::units::Q_M) / 0.01).round();
    (cm as i64).clamp(LANE_WIDTH_MIN, LANE_WIDTH_MAX) as u16
}

/// A `node-XY*` offset from a displacement in metres, east and north.
///
/// `None` when the displacement is beyond ±327.68 m in either axis, which is the point at
/// which the standard's own answer is `node-LatLon` rather than a wider offset — so a
/// caller that gets `None` should emit [`NodeOffset::LatLon`], not clamp.
///
/// Quantises on the D9 metre grid before scaling to centimetres.
pub fn xy_offset(east_m: f64, north_m: f64) -> Option<XyOffset> {
    if !east_m.is_finite() || !north_m.is_finite() {
        return None;
    }
    let to_cm = |v: f64| (math::quantize_to(v, crate::units::Q_M) / 0.01).round();
    let (x, y) = (to_cm(east_m), to_cm(north_m));
    if !(-2_147_483_648.0..=2_147_483_647.0).contains(&x)
        || !(-2_147_483_648.0..=2_147_483_647.0).contains(&y)
    {
        return None;
    }
    XyOffset::narrowest(x as i32, y as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_node_lane() -> GenericLane {
        GenericLane {
            lane_id: 1,
            ingress_approach: None,
            egress_approach: None,
            attributes: LaneAttributes::vehicle(LaneDirection::INGRESS),
            maneuvers: None,
            nodes: vec![
                NodeXy::offset(0, 0).expect("fits"),
                NodeXy::offset(120, 340).expect("fits"),
            ],
            connects_to: Vec::new(),
        }
    }

    fn minimal() -> MapData {
        MapData {
            time_stamp: None,
            msg_issue_revision: 0,
            intersections: vec![IntersectionGeometry {
                id: IntersectionReferenceId::new(1),
                revision: 0,
                ref_point: Position3D {
                    lat: 407_440_000,
                    lon: -739_900_000,
                    elevation: None,
                },
                lane_width_cm: None,
                lanes: vec![two_node_lane()],
            }],
        }
    }

    fn rich() -> MapData {
        let mut lane = two_node_lane();
        lane.ingress_approach = Some(3);
        lane.egress_approach = Some(4);
        lane.maneuvers = Some(AllowedManeuvers::STRAIGHT.with(AllowedManeuvers::RIGHT));
        lane.attributes = LaneAttributes {
            directional_use: LaneDirection::BOTH,
            shared_with: LaneSharing::BUS.with(LaneSharing::TAXI),
            vehicle: VehicleLaneAttributes::HOV_ONLY,
        };
        lane.nodes.push(NodeXy {
            delta: NodeOffset::Xy(XyOffset {
                x_cm: -20_000,
                y_cm: 30_000,
                alternative: XyAlternative::Xy6,
            }),
        });
        lane.nodes.push(NodeXy {
            delta: NodeOffset::LatLon {
                lon: -739_890_000,
                lat: 407_441_000,
            },
        });
        lane.connects_to = vec![
            Connection::signalised(7, 2),
            Connection {
                connecting_lane: ConnectingLane {
                    lane: 9,
                    maneuver: Some(AllowedManeuvers::LEFT),
                },
                remote_intersection: Some(IntersectionReferenceId::in_region(11, 22)),
                signal_group: Some(3),
                user_class: Some(4),
                connection_id: Some(5),
            },
        ];
        MapData {
            time_stamp: Some(12_345),
            msg_issue_revision: 7,
            intersections: vec![IntersectionGeometry {
                id: IntersectionReferenceId::in_region(7, 1_234),
                revision: 2,
                ref_point: Position3D {
                    lat: 407_440_000,
                    lon: -739_900_000,
                    elevation: Some(125),
                },
                lane_width_cm: Some(lane_width_cm(3.5)),
                lanes: vec![lane, two_node_lane()],
            }],
        }
    }

    /// The bit arithmetic of [`MINIMAL_MAP_SIZE_B`], asserted. If any assumption in
    /// [`assumptions`] changes, this number moves — which is the point of pinning it.
    #[test]
    fn a_minimal_map_is_twenty_eight_octets() {
        let map = minimal();
        let encoded = encode_map(&map).expect("encodes");
        assert_eq!(encoded.size, MINIMAL_MAP_SIZE_B);
        assert!(encoded.is_real());
        assert_eq!(
            encode_message_frame(&map).expect("frames").size,
            MINIMAL_MAP_MESSAGE_FRAME_SIZE_B
        );
    }

    #[test]
    fn round_trip_is_exact() {
        for map in [minimal(), rich()] {
            let bytes = encode_map(&map).expect("encodes").bytes;
            assert_eq!(decode_map(&bytes).expect("decodes"), map);
            let framed = encode_message_frame(&map).expect("frames").bytes;
            assert_eq!(decode_message_frame(&framed).expect("decodes"), map);
        }
    }

    /// A node keeps the alternative it arrived in, so decode-then-encode is byte-exact
    /// even for an offset a narrower alternative could have carried.
    #[test]
    fn a_node_offset_keeps_its_alternative_so_the_bytes_are_stable() {
        let mut map = minimal();
        map.intersections[0].lanes[0].nodes[1] = NodeXy {
            delta: NodeOffset::Xy(XyOffset {
                // 1 cm fits node-XY1, but this value says node-XY6 and must stay there.
                x_cm: 1,
                y_cm: 1,
                alternative: XyAlternative::Xy6,
            }),
        };
        let bytes = encode_map(&map).expect("encodes").bytes;
        let decoded = decode_map(&bytes).expect("decodes");
        assert_eq!(decoded, map);
        assert_eq!(encode_map(&decoded).expect("re-encodes").bytes, bytes);
        // And it really is wider than the narrowest encoding.
        let mut narrow = map.clone();
        narrow.intersections[0].lanes[0].nodes[1] = NodeXy::offset(1, 1).expect("fits");
        assert!(
            encode_map(&narrow).expect("encodes").size < encode_map(&map).expect("encodes").size
        );
    }

    #[test]
    fn the_narrowest_alternative_is_chosen_and_the_widest_bounds_are_honoured() {
        assert_eq!(XyAlternative::narrowest_for(0, 0), Some(XyAlternative::Xy1));
        assert_eq!(
            XyAlternative::narrowest_for(511, -512),
            Some(XyAlternative::Xy1)
        );
        assert_eq!(
            XyAlternative::narrowest_for(512, 0),
            Some(XyAlternative::Xy2)
        );
        assert_eq!(
            XyAlternative::narrowest_for(32_767, -32_768),
            Some(XyAlternative::Xy6)
        );
        assert_eq!(XyAlternative::narrowest_for(32_768, 0), None);
        for a in XyAlternative::ALL {
            assert_eq!(a.max_cm() - a.min_cm() + 1, 1 << a.bits_per_axis());
            assert_eq!(XyAlternative::from_index(a.index()), Some(a));
        }
        assert!(XyAlternative::from_index(NODE_OFFSET_LATLON_INDEX).is_none());
    }

    #[test]
    fn an_offset_too_wide_for_its_own_alternative_is_refused() {
        let mut map = minimal();
        map.intersections[0].lanes[0].nodes[1] = NodeXy {
            delta: NodeOffset::Xy(XyOffset {
                x_cm: 5_000,
                y_cm: 0,
                alternative: XyAlternative::Xy1,
            }),
        };
        let err = encode_map(&map).expect_err("5 000 cm does not fit ten bits");
        assert!(err.to_string().contains("nodeXY.delta"), "{err}");
    }

    #[test]
    fn lists_outside_their_size_constraints_are_refused_by_name() {
        let mut map = minimal();
        map.intersections.clear();
        assert!(encode_map(&map).is_err());

        let mut map = minimal();
        map.intersections[0].lanes.clear();
        let err = encode_map(&map).expect_err("SIZE(1..255) admits no empty lane set");
        assert!(err.to_string().contains("laneSet"), "{err}");

        let mut map = minimal();
        map.intersections[0].lanes[0].nodes.pop();
        let err = encode_map(&map).expect_err("a lane needs two nodes");
        assert!(err.to_string().contains("nodes"), "{err}");

        let mut map = minimal();
        map.intersections[0].lanes[0].connects_to =
            vec![Connection::signalised(1, 1); MAX_CONNECTIONS + 1];
        assert!(encode_map(&map).is_err());
    }

    #[test]
    fn an_unmodelled_element_is_refused_rather_than_skipped() {
        // MapData.layerType, the first refused optional field.
        let mut w = BitWriter::new();
        write_preamble(
            &mut w,
            EXT_MAP_DATA,
            &[false, true, false, false, false, false, false, false],
        );
        write_constrained_int(
            &mut w,
            F_MSG_ISSUE_REVISION,
            0,
            MSG_COUNT_MIN,
            MSG_COUNT_MAX,
        )
        .expect("revision");
        let bytes = w.into_bytes();
        let err = decode_map(&bytes).expect_err("a layer type cannot be read");
        assert!(
            matches!(err, CodecError::UnsupportedConstruct { construct, .. } if construct == "MapData.layerType"),
            "{err}"
        );

        // A lane type other than `vehicle`.
        let mut w = BitWriter::new();
        write_preamble(&mut w, EXT_LANE_ATTRIBUTES, &[false]);
        write_fixed_bit_string(&mut w, F_DIRECTIONAL_USE, 0b10, LANE_DIRECTION_BITS)
            .expect("direction");
        write_fixed_bit_string(&mut w, F_SHARED_WITH, 0, LANE_SHARING_BITS).expect("sharing");
        write_choice_index(
            &mut w,
            F_LANE_TYPE,
            EXT_LANE_TYPE_ATTRIBUTES,
            1, // crosswalk
            LANE_TYPE_ALTERNATIVES,
        )
        .expect("choice");
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let err = read_lane_attributes(&mut r).expect_err("a crosswalk is not modelled");
        assert!(
            matches!(err, UperError::Unsupported { construct, .. } if construct == "LaneAttributes.laneType"),
            "{err}"
        );
    }

    #[test]
    fn a_map_with_no_intersections_is_refused_on_decode() {
        let mut w = BitWriter::new();
        write_preamble(
            &mut w,
            EXT_MAP_DATA,
            &[false, false, false, false, false, false, false, false],
        );
        write_constrained_int(
            &mut w,
            F_MSG_ISSUE_REVISION,
            1,
            MSG_COUNT_MIN,
            MSG_COUNT_MAX,
        )
        .expect("revision");
        let bytes = w.into_bytes();
        let err = decode_map(&bytes).expect_err("nothing to describe");
        assert!(
            matches!(err, CodecError::UnsupportedConstruct { construct, .. } if construct == "MapData.intersections"),
            "{err}"
        );
    }

    #[test]
    fn an_extension_addition_stops_the_decode() {
        let mut w = BitWriter::new();
        w.write_bit(true);
        let bytes = w.into_bytes();
        assert!(matches!(
            decode_map(&bytes),
            Err(CodecError::UnsupportedConstruct { .. })
        ));
    }

    #[test]
    fn a_frame_carrying_another_message_is_refused() {
        let framed = encode_message_frame(&minimal()).expect("frames").bytes;
        assert!(crate::j2735::spat::decode_message_frame(&framed).is_err());
    }

    #[test]
    fn trailing_data_is_refused() {
        let mut bytes = encode_map(&minimal()).expect("encodes").bytes;
        bytes.push(0);
        assert!(decode_map(&bytes).is_err());
    }

    #[test]
    fn lane_width_and_offsets_quantise_before_they_scale() {
        assert_eq!(lane_width_cm(3.5), 350);
        assert_eq!(lane_width_cm(0.0), 0);
        assert_eq!(lane_width_cm(-1.0), 0);
        assert_eq!(lane_width_cm(f64::INFINITY), 0);
        assert_eq!(lane_width_cm(1_000.0), LANE_WIDTH_MAX as u16);

        let offset = xy_offset(1.2, -3.4).expect("fits");
        assert_eq!((offset.x_cm, offset.y_cm), (120, -340));
        assert_eq!(offset.alternative, XyAlternative::Xy1);
        // 5.12 m east is one centimetre past Offset-B10, so it steps up one alternative.
        assert_eq!(
            xy_offset(5.12, 0.0).expect("fits").alternative,
            XyAlternative::Xy2
        );
        assert!(xy_offset(400.0, 0.0).is_none());
        assert!(xy_offset(f64::NAN, 0.0).is_none());
    }

    /// The assumptions are load-bearing, so they are asserted rather than merely
    /// documented: a change to any of them is a change to the wire format and must be a
    /// deliberate edit to this test as well.
    #[test]
    fn the_structural_assumptions_are_the_ones_the_module_documents() {
        assert!(EXT_MAP_DATA);
        assert!(EXT_INTERSECTION_GEOMETRY);
        assert!(EXT_POSITION_3D);
        assert!(EXT_GENERIC_LANE);
        assert!(!EXT_LANE_ATTRIBUTES);
        assert!(EXT_NODE_XY);
        assert!(EXT_NODE_LIST_XY);
        assert!(!EXT_NODE_OFFSET_POINT_XY);
        assert!(EXT_LANE_TYPE_ATTRIBUTES);
        assert!(!EXT_CONNECTION);
        assert!(!EXT_CONNECTING_LANE);
        assert!(NODE_LATLON_ENCODES_LON_FIRST);
    }
}
