//! The SAE J2735 Personal Safety Message: what a pedestrian's or a cyclist's device sends
//! (SAE J2945/9).
//!
//! # The shape
//!
//! ```text
//! PersonalSafetyMessage     extensible SEQUENCE, 18 optional fields
//! ├── basicType             PersonalDeviceUserType  ENUMERATED {unavailable, aPEDESTRIAN,
//! │                         aPEDALCYCLIST, aPUBLICSAFETYWORKER, anANIMAL, ...}
//! ├── secMark               DSecond                 INTEGER (0..65535)
//! ├── msgCnt                MsgCount                INTEGER (0..127)
//! ├── id                    TemporaryID             OCTET STRING (SIZE(4))
//! ├── position              Position3D              extensible SEQUENCE {lat, long,
//! │                                                 elevation OPTIONAL, regional OPTIONAL}
//! ├── accuracy              PositionalAccuracy      SEQUENCE {semiMajor, semiMinor, orientation}
//! ├── speed                 Velocity                INTEGER (0..8191)
//! ├── heading               Heading                 INTEGER (0..28800)
//! └── 18 OPTIONAL fields    accelSet, pathHistory, pathPrediction, propulsion, useState,
//!                           crossRequest, crossState, clusterSize, clusterRadius,
//!                           eventResponderType, activityType, activitySubType, assistType,
//!                           sizing, attachment, attachmentRadius, animalType, regional
//! ```
//!
//! This codec encodes and decodes the eight mandatory fields and `position.elevation`;
//! every optional field is absent on encode and **refused** on decode
//! ([`CodecError::UnsupportedConstruct`]) rather than skipped, because skipping an optional
//! field in PER means knowing its encoding. The `MessageFrame` carries it under
//! `DSRCmsgID personalSafetyMessage(32)`.
//!
//! A mandatory-only PSM with an elevation is **28 octets** (220 bits); in a `MessageFrame`,
//! **31**.
//!
//! # Evidence — stated, not implied
//!
//! Written against the J2735 PSM definition (2016-03 onwards; the root of the SEQUENCE is
//! unchanged through 2024-09) with the types, ranges and sentinels the [`crate::j2735::bsm`]
//! codec already uses for the same data elements, and the field order and optional count
//! above. It has **not** been checked against the `pycrate` oracle the BSM was checked
//! against: the J2735 ASN.1 is not in this repository (build decision D3) and the oracle's
//! environment is not on this machine. So its bytes are real UPER of the structure above,
//! *not yet oracle-validated* — the claim `crate::evidence` makes for SPaT and MAP, and
//! weaker than the BSM's.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geo::GeoOrigin;

use crate::codec::{Encoded, MsgType};
use crate::error::CodecError;
use crate::j2735::bsm::{
    D_SECOND_MAX, D_SECOND_MIN, DSRC_MSG_ID_MAX, ELEVATION_MAX, ELEVATION_MIN, HEADING_MAX,
    HEADING_MIN, LATITUDE_MAX, LATITUDE_MIN, LONGITUDE_MAX, LONGITUDE_MIN, MSG_COUNT_MAX,
    MSG_COUNT_MIN, ORIENTATION_MAX, ORIENTATION_MIN, PositionalAccuracy, SEMI_AXIS_MAX,
    SEMI_AXIS_MIN, SPEED_MAX, SPEED_MIN, elevation, heading, latitude, longitude,
    semi_axis_accuracy, semi_major_orientation, speed,
};
use crate::j2735::uper::{
    BitReader, BitWriter, Field, UperError, read_bool, read_constrained_int, read_enumerated,
    read_fixed_octet_string, read_open_type, read_preamble, write_bool, write_constrained_int,
    write_enumerated, write_fixed_octet_string, write_open_type, write_preamble,
};

/// `DSRCmsgID` of the PSM in a `MessageFrame`: `personalSafetyMessage ::= 32`.
pub const PSM_MESSAGE_ID: u16 = 32;

/// How many OPTIONAL fields `PersonalSafetyMessage` declares (the preamble's bitmap width).
pub const PSM_OPTIONAL_FIELDS: usize = 18;

/// The encoded size of a mandatory-only PSM with an elevation, octets.
pub const MANDATORY_PSM_SIZE_B: u32 = 28;

/// The same inside a `MessageFrame`, octets.
pub const MANDATORY_PSM_FRAME_SIZE_B: u32 = 31;

/// The codec's id, for a payload's provenance.
pub const PSM_CODEC_ID: &str = "codec/uper/j2735-psm";

/// `PersonalDeviceUserType`: who is carrying the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PersonalDeviceUserType {
    /// `unavailable (0)`.
    Unavailable,
    /// `aPEDESTRIAN (1)`.
    Pedestrian,
    /// `aPEDALCYCLIST (2)`.
    Pedalcyclist,
    /// `aPUBLICSAFETYWORKER (3)`.
    PublicSafetyWorker,
    /// `anANIMAL (4)`.
    Animal,
}

impl PersonalDeviceUserType {
    /// Root enumerations of the type (it is extensible).
    pub const COUNT: u64 = 5;

    /// The enumeration index.
    pub const fn index(self) -> u64 {
        match self {
            Self::Unavailable => 0,
            Self::Pedestrian => 1,
            Self::Pedalcyclist => 2,
            Self::PublicSafetyWorker => 3,
            Self::Animal => 4,
        }
    }

    /// The value for an index.
    pub const fn from_index(i: u64) -> Option<Self> {
        Some(match i {
            0 => Self::Unavailable,
            1 => Self::Pedestrian,
            2 => Self::Pedalcyclist,
            3 => Self::PublicSafetyWorker,
            4 => Self::Animal,
            _ => return None,
        })
    }
}

/// A `PersonalSafetyMessage`, mandatory part — wire units throughout, as in
/// [`crate::j2735::bsm::BsmCoreData`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalSafetyMessage {
    /// `basicType`.
    pub basic_type: PersonalDeviceUserType,
    /// `secMark`, milliseconds within the minute, `65535` unavailable.
    pub sec_mark: u16,
    /// `msgCnt`.
    pub msg_cnt: u8,
    /// `id`, the four-octet temporary identifier.
    pub id: [u8; 4],
    /// `position.lat`, 1/10 µdeg.
    pub lat: i32,
    /// `position.long`, 1/10 µdeg.
    pub lon: i32,
    /// `position.elevation`, 10 cm; `None` omits the optional field.
    pub elev: Option<i32>,
    /// `accuracy`.
    pub accuracy: PositionalAccuracy,
    /// `speed`, 0.02 m/s.
    pub speed: u16,
    /// `heading`, 0.0125°.
    pub heading: u16,
}

const F_BASIC_TYPE: Field = Field::new("psm.basicType", "PersonalDeviceUserType");
const F_SEC_MARK: Field = Field::new("psm.secMark", "DSecond");
const F_MSG_CNT: Field = Field::new("psm.msgCnt", "MsgCount");
const F_LAT: Field = Field::new("psm.position.lat", "Latitude");
const F_LON: Field = Field::new("psm.position.long", "Longitude");
const F_ELEV: Field = Field::new("psm.position.elevation", "Elevation");
const F_SEMI_MAJOR: Field = Field::new("psm.accuracy.semiMajor", "SemiMajorAxisAccuracy");
const F_SEMI_MINOR: Field = Field::new("psm.accuracy.semiMinor", "SemiMinorAxisAccuracy");
const F_ORIENTATION: Field = Field::new("psm.accuracy.orientation", "SemiMajorAxisOrientation");
const F_SPEED: Field = Field::new("psm.speed", "Velocity");
const F_HEADING: Field = Field::new("psm.heading", "Heading");
const F_MESSAGE_ID: Field = Field::new("MessageFrame.messageId", "DSRCmsgID");

fn write_psm(w: &mut BitWriter, m: &PersonalSafetyMessage) -> Result<(), UperError> {
    write_preamble(w, true, &[false; PSM_OPTIONAL_FIELDS]);
    // basicType: an extensible ENUMERATED — the extension bit, then the root index.
    write_bool(w, false);
    write_enumerated(
        w,
        F_BASIC_TYPE,
        m.basic_type.index(),
        PersonalDeviceUserType::COUNT,
    )?;
    write_constrained_int(
        w,
        F_SEC_MARK,
        i64::from(m.sec_mark),
        D_SECOND_MIN,
        D_SECOND_MAX,
    )?;
    write_constrained_int(
        w,
        F_MSG_CNT,
        i64::from(m.msg_cnt),
        MSG_COUNT_MIN,
        MSG_COUNT_MAX,
    )?;
    write_fixed_octet_string(w, &m.id);
    // Position3D: extensible, elevation and regional optional.
    write_preamble(w, true, &[m.elev.is_some(), false]);
    write_constrained_int(w, F_LAT, i64::from(m.lat), LATITUDE_MIN, LATITUDE_MAX)?;
    write_constrained_int(w, F_LON, i64::from(m.lon), LONGITUDE_MIN, LONGITUDE_MAX)?;
    if let Some(e) = m.elev {
        write_constrained_int(w, F_ELEV, i64::from(e), ELEVATION_MIN, ELEVATION_MAX)?;
    }
    write_constrained_int(
        w,
        F_SEMI_MAJOR,
        i64::from(m.accuracy.semi_major),
        SEMI_AXIS_MIN,
        SEMI_AXIS_MAX,
    )?;
    write_constrained_int(
        w,
        F_SEMI_MINOR,
        i64::from(m.accuracy.semi_minor),
        SEMI_AXIS_MIN,
        SEMI_AXIS_MAX,
    )?;
    write_constrained_int(
        w,
        F_ORIENTATION,
        i64::from(m.accuracy.orientation),
        ORIENTATION_MIN,
        ORIENTATION_MAX,
    )?;
    write_constrained_int(w, F_SPEED, i64::from(m.speed), SPEED_MIN, SPEED_MAX)?;
    write_constrained_int(w, F_HEADING, i64::from(m.heading), HEADING_MIN, HEADING_MAX)
}

fn read_psm(r: &mut BitReader<'_>) -> Result<PersonalSafetyMessage, UperError> {
    let pre = read_preamble(r, "PersonalSafetyMessage", true, PSM_OPTIONAL_FIELDS)?;
    if pre.present != 0 {
        return Err(UperError::Unsupported {
            construct: "PersonalSafetyMessage optional fields",
            detail: "this codec models the mandatory PSM; an optional field is present",
        });
    }
    if read_bool(r)? {
        return Err(UperError::Unsupported {
            construct: "PersonalDeviceUserType",
            detail: "an extension value of the user type",
        });
    }
    let t = read_enumerated(r, F_BASIC_TYPE, PersonalDeviceUserType::COUNT)?;
    let basic_type = PersonalDeviceUserType::from_index(t).ok_or(UperError::BadEnumIndex {
        asn1_type: F_BASIC_TYPE.asn1_type,
        index: t,
        count: PersonalDeviceUserType::COUNT,
    })?;
    let sec_mark = read_constrained_int(r, F_SEC_MARK, D_SECOND_MIN, D_SECOND_MAX)? as u16;
    let msg_cnt = read_constrained_int(r, F_MSG_CNT, MSG_COUNT_MIN, MSG_COUNT_MAX)? as u8;
    let id_v = read_fixed_octet_string(r, 4)?;
    let mut id = [0u8; 4];
    id.copy_from_slice(&id_v);
    let pos = read_preamble(r, "Position3D", true, 2)?;
    if pos.has(1) {
        return Err(UperError::Unsupported {
            construct: "Position3D.regional",
            detail: "regional extensions are not modelled",
        });
    }
    let lat = read_constrained_int(r, F_LAT, LATITUDE_MIN, LATITUDE_MAX)? as i32;
    let lon = read_constrained_int(r, F_LON, LONGITUDE_MIN, LONGITUDE_MAX)? as i32;
    let elev = if pos.has(0) {
        Some(read_constrained_int(r, F_ELEV, ELEVATION_MIN, ELEVATION_MAX)? as i32)
    } else {
        None
    };
    let accuracy = PositionalAccuracy {
        semi_major: read_constrained_int(r, F_SEMI_MAJOR, SEMI_AXIS_MIN, SEMI_AXIS_MAX)? as u8,
        semi_minor: read_constrained_int(r, F_SEMI_MINOR, SEMI_AXIS_MIN, SEMI_AXIS_MAX)? as u8,
        orientation: read_constrained_int(r, F_ORIENTATION, ORIENTATION_MIN, ORIENTATION_MAX)?
            as u16,
    };
    let speed = read_constrained_int(r, F_SPEED, SPEED_MIN, SPEED_MAX)? as u16;
    let heading = read_constrained_int(r, F_HEADING, HEADING_MIN, HEADING_MAX)? as u16;
    Ok(PersonalSafetyMessage {
        basic_type,
        sec_mark,
        msg_cnt,
        id,
        lat,
        lon,
        elev,
        accuracy,
        speed,
        heading,
    })
}

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
            ty: MsgType::Psm,
            construct,
            detail,
        },
        other => CodecError::Encode {
            ty: MsgType::Psm,
            detail: other.to_string(),
        },
    }
}

fn on_decode(len: usize) -> impl Fn(UperError) -> CodecError {
    move |e| match e {
        UperError::Unsupported { construct, detail } => CodecError::UnsupportedConstruct {
            ty: MsgType::Psm,
            construct,
            detail,
        },
        other => CodecError::Decode {
            ty: MsgType::Psm,
            len,
            detail: other.to_string(),
        },
    }
}

/// UPER-encodes a `PersonalSafetyMessage` PDU.
pub fn encode_psm(m: &PersonalSafetyMessage) -> Result<Encoded, CodecError> {
    let mut w = BitWriter::with_capacity(32);
    write_psm(&mut w, m).map_err(on_encode)?;
    Ok(Encoded::uper(w.into_bytes()))
}

/// Decodes a `PersonalSafetyMessage` PDU, refusing trailing data.
pub fn decode_psm(bytes: &[u8]) -> Result<PersonalSafetyMessage, CodecError> {
    let map = on_decode(bytes.len());
    let mut r = BitReader::new(bytes);
    let m = read_psm(&mut r).map_err(&map)?;
    r.finish().map_err(&map)?;
    Ok(m)
}

/// UPER-encodes a `MessageFrame` carrying this PSM — what goes in the WSM payload.
pub fn encode_message_frame(m: &PersonalSafetyMessage) -> Result<Encoded, CodecError> {
    let mut inner = BitWriter::with_capacity(32);
    write_psm(&mut inner, m).map_err(on_encode)?;
    let inner = inner.into_bytes();
    let mut w = BitWriter::with_capacity(inner.len() + 4);
    write_preamble(&mut w, true, &[]);
    write_constrained_int(
        &mut w,
        F_MESSAGE_ID,
        i64::from(PSM_MESSAGE_ID),
        0,
        i64::from(DSRC_MSG_ID_MAX),
    )
    .map_err(on_encode)?;
    write_open_type(&mut w, "MessageFrame.value", &inner).map_err(on_encode)?;
    Ok(Encoded::uper(w.into_bytes()))
}

/// Decodes a `MessageFrame` and returns the PSM inside it; any other `DSRCmsgID` is
/// refused.
pub fn decode_message_frame(bytes: &[u8]) -> Result<PersonalSafetyMessage, CodecError> {
    let map = on_decode(bytes.len());
    let mut r = BitReader::new(bytes);
    read_preamble(&mut r, "MessageFrame", true, 0).map_err(&map)?;
    let id = read_constrained_int(&mut r, F_MESSAGE_ID, 0, i64::from(DSRC_MSG_ID_MAX))
        .map_err(&map)? as u16;
    if id != PSM_MESSAGE_ID {
        return Err(CodecError::UnsupportedConstruct {
            ty: MsgType::Psm,
            construct: "MessageFrame.messageId",
            detail: "the frame carries a DSRCmsgID other than personalSafetyMessage(32)",
        });
    }
    let inner = read_open_type(&mut r, "MessageFrame.value").map_err(&map)?;
    r.finish().map_err(&map)?;
    let mut ir = BitReader::new(&inner);
    let m = read_psm(&mut ir).map_err(&map)?;
    ir.finish().map_err(&map)?;
    Ok(m)
}

/// What a device hands over to have a PSM built. `position` is the device's **belief**
/// (invariant I-C2), never ground truth.
#[derive(Debug, Clone, Copy)]
pub struct PsmInput {
    /// Who carries the device.
    pub basic_type: PersonalDeviceUserType,
    /// `msgCnt`.
    pub msg_cnt: u8,
    /// `id`: the first four octets of the active pseudonym's digest, as a vehicle's is.
    pub id: [u8; 4],
    /// The device's position belief.
    pub position: PositionEstimate,
    /// The anchor of the ENU frame `position` is in.
    pub origin: GeoOrigin,
    /// `secMark` ([`crate::j2735::bsm::sec_mark`]).
    pub sec_mark: u16,
}

/// Builds the PSM from a device's belief, with the BSM's unit conversions.
///
/// # Errors
/// [`CodecError::OutOfRange`] when the projection puts the belief off the globe.
pub fn build_psm(input: &PsmInput) -> Result<PersonalSafetyMessage, CodecError> {
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
    Ok(PersonalSafetyMessage {
        basic_type: input.basic_type,
        sec_mark: input.sec_mark,
        msg_cnt: input.msg_cnt.min(MSG_COUNT_MAX as u8),
        id: input.id,
        lat,
        lon,
        elev: Some(elevation(alt_m)),
        accuracy: PositionalAccuracy {
            semi_major: semi_axis_accuracy(input.position.semi_major_m),
            semi_minor: semi_axis_accuracy(input.position.semi_minor_m),
            orientation: semi_major_orientation(input.position.orientation_rad),
        },
        speed: speed(input.position.ground_speed_mps()),
        heading: heading(input.position.heading_rad),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn psm() -> PersonalSafetyMessage {
        PersonalSafetyMessage {
            basic_type: PersonalDeviceUserType::Pedestrian,
            sec_mark: 41_250,
            msg_cnt: 17,
            id: [0xde, 0xad, 0xbe, 0xef],
            lat: 407_527_000,
            lon: -739_792_000,
            elev: Some(120),
            accuracy: PositionalAccuracy {
                semi_major: 60,
                semi_minor: 40,
                orientation: 1_000,
            },
            speed: 67,
            heading: 4_800,
        }
    }

    #[test]
    fn a_mandatory_psm_round_trips_at_its_computed_size() {
        let m = psm();
        let e = encode_psm(&m).unwrap();
        // 19 preamble bits + 4 (type) + 16 + 7 + 32 + (3 + 31 + 32 + 16) + 32 + 13 + 15.
        assert_eq!(e.bytes.len() as u32, MANDATORY_PSM_SIZE_B);
        assert_eq!(decode_psm(&e.bytes).unwrap(), m);
        let f = encode_message_frame(&m).unwrap();
        assert_eq!(f.bytes.len() as u32, MANDATORY_PSM_FRAME_SIZE_B);
        assert_eq!(decode_message_frame(&f.bytes).unwrap(), m);
    }

    #[test]
    fn every_user_type_and_an_absent_elevation_round_trip() {
        for t in 0..PersonalDeviceUserType::COUNT {
            let m = PersonalSafetyMessage {
                basic_type: PersonalDeviceUserType::from_index(t).unwrap(),
                elev: None,
                ..psm()
            };
            let e = encode_psm(&m).unwrap();
            assert_eq!(e.bytes.len(), 26);
            assert_eq!(decode_psm(&e.bytes).unwrap(), m);
        }
    }

    #[test]
    fn a_frame_of_another_message_and_a_truncated_psm_are_refused() {
        // A frame labelled basicSafetyMessage(20) holding a PSM body.
        let e = encode_psm(&psm()).unwrap();
        let mut w = BitWriter::with_capacity(40);
        write_preamble(&mut w, true, &[]);
        write_constrained_int(&mut w, F_MESSAGE_ID, 20, 0, i64::from(DSRC_MSG_ID_MAX)).unwrap();
        write_open_type(&mut w, "MessageFrame.value", &e.bytes).unwrap();
        assert!(decode_message_frame(&w.into_bytes()).is_err());
        assert!(decode_psm(&e.bytes[..e.bytes.len() - 3]).is_err());
    }

    #[test]
    fn a_belief_becomes_wire_units() {
        let origin = GeoOrigin::new(40.7527, -73.9772, 0.0);
        let mut position = PositionEstimate::no_fix(0);
        position.pos = v2xw_core::geom::Vec3::new(10.0, 20.0, 0.0);
        position.vel = v2xw_core::geom::Vec3::new_2d(1.0, 1.0);
        position.semi_major_m = 3.0;
        position.semi_minor_m = 2.0;
        let m = build_psm(&PsmInput {
            basic_type: PersonalDeviceUserType::Pedestrian,
            msg_cnt: 3,
            id: [1, 2, 3, 4],
            position,
            origin,
            sec_mark: 500,
        })
        .unwrap();
        assert_eq!(m.speed, speed(2f64.sqrt()));
        assert_eq!(m.accuracy.semi_major, 60);
        assert!((f64::from(m.lat) * 1e-7 - 40.7527).abs() < 1e-3);
        assert_eq!(decode_psm(&encode_psm(&m).unwrap().bytes).unwrap(), m);
    }
}
