//! Building, encoding and decoding a real ETSI VAM — the VRU awareness message a
//! pedestrian's or a cyclist's device sends (ETSI TS 103 300-3).
//!
//! The ASN.1 is `VAM-PDU-Descriptions` V2.2.1 from the ETSI forge (BSD-3-Clause, pinned in
//! `third_party/asn1/etsi/PROVENANCE.md`), generated with the rest of the facilities layer
//! over the Release 2 common data dictionary, so the bytes are real UPER exactly as a CAM's
//! are.
//!
//! # What goes in
//!
//! Everything comes from the device's **belief** (invariant I-C2): the reference position,
//! the heading and the speed are its GNSS model's output, never ground truth.
//!
//! | Container | Filled with |
//! |---|---|
//! | `header` | protocol version 3, `messageId vam(16)`, the pseudonym's station id |
//! | `basicContainer` | `stationType` pedestrian(1) or cyclist(2), the reference position and its confidence ellipse — the CAM's own builder |
//! | `vruHighFrequencyContainer` | heading, speed, longitudinal acceleration (`unavailable` when the device does not measure it) |
//! | `vruLowFrequencyContainer` | the profile: `pedestrian: ordinary-pedestrian(1)` or `bicyclistAndLightVruVehicle: bicyclist(1)` — sent when TS 103 300-3's low-frequency rule says so |
//!
//! The cluster containers and the motion-prediction container are absent: clustering is
//! not modelled (a limitation the VRU device's card states), and a VAM without them is a
//! complete, valid VAM.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geo::GeoOrigin;

use crate::asn1::cdd::{
    AccelerationConfidence, BasicContainer, ItsPduHeader, LongitudinalAcceleration,
    LongitudinalAccelerationValue, MessageId, OrdinalNumber1B, Speed, SpeedConfidence, SpeedValue,
    StationId, TimestampIts, TrafficParticipantType, VruProfileAndSubprofile,
    VruSubProfileBicyclist, VruSubProfilePedestrian, Wgs84Angle, Wgs84AngleConfidence,
    Wgs84AngleValue,
};
use crate::asn1::vam_asn1::{
    ItsPduHeaderVam, VAM, VamParameters, VruAwareness, VruHighFrequencyContainer,
    VruLowFrequencyContainer,
};
use crate::cam::{ParticipantType, generation_delta_time};
use crate::codec::{Encoded, MsgType, uper_decode, uper_encode};
use crate::error::CodecError;
use crate::units;

/// ITS PDU protocol version of a V2.2.1 VAM: `ItsPduHeaderVam ::= ItsPduHeader (WITH
/// COMPONENTS {..., protocolVersion(3), messageId(vam)})`.
pub const VAM_PROTOCOL_VERSION: u8 = 3;

/// `MessageId` of a VAM: `vam(16)` in `ETSI-ITS-CDD.asn`.
pub const VAM_MESSAGE_ID: u8 = 16;

/// The codec's id, for a payload's provenance.
pub const VAM_CODEC_ID: &str = "codec/uper/etsi-vam";

/// Which VRU profile the device's user is (TS 103 300-3 `VruProfileAndSubprofile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VruProfile {
    /// Profile 1, an ordinary pedestrian.
    Pedestrian,
    /// Profile 2, a bicyclist.
    Bicyclist,
}

impl VruProfile {
    /// The `stationType` the basic container carries.
    pub const fn station_type(self) -> ParticipantType {
        match self {
            VruProfile::Pedestrian => ParticipantType::Pedestrian,
            VruProfile::Bicyclist => ParticipantType::Cyclist,
        }
    }
}

/// What a VRU device hands over to have a VAM built.
#[derive(Debug, Clone, PartialEq)]
pub struct VamInput {
    /// The station id: the active pseudonym's.
    pub station_id: u32,
    /// Pedestrian or bicyclist.
    pub profile: VruProfile,
    /// The device's position belief.
    pub position: PositionEstimate,
    /// The anchor of the ENU frame `position` is in.
    pub origin: GeoOrigin,
    /// The instant the message claims, as a `TimestampIts`.
    pub generation_time: TimestampIts,
    /// Longitudinal acceleration, m/s², when the device measures it.
    pub longitudinal_acceleration_mps2: Option<f64>,
    /// Whether this VAM carries the low-frequency container (TS 103 300-3: the first VAM,
    /// then at most every 2,000 ms — the device's schedule decides).
    pub include_low_frequency: bool,
}

/// Fills a VAM from the device's state.
///
/// # Errors
/// [`CodecError::OutOfRange`] when the belief cannot be put on the globe.
pub fn build_vam(input: &VamInput) -> Result<VAM, CodecError> {
    let header = ItsPduHeaderVam(ItsPduHeader::new(
        OrdinalNumber1B(VAM_PROTOCOL_VERSION),
        MessageId(VAM_MESSAGE_ID),
        StationId(input.station_id),
    ));
    let basic = BasicContainer::new(
        TrafficParticipantType(input.profile.station_type().code()),
        crate::cam::reference_position(&input.position, &input.origin)?,
    );
    let p = &input.position;
    let high = VruHighFrequencyContainer::new(
        Wgs84Angle::new(
            Wgs84AngleValue(units::wgs84_angle(p.heading_rad)),
            // The belief has no heading accuracy: `unavailable(127)`.
            Wgs84AngleConfidence(units::heading_confidence(None)),
        ),
        Speed::new(
            SpeedValue(units::speed_value(p.ground_speed_mps())),
            SpeedConfidence(units::speed_confidence(None)),
        ),
        LongitudinalAcceleration::new(
            LongitudinalAccelerationValue(units::acceleration_value(
                input.longitudinal_acceleration_mps2,
            )),
            AccelerationConfidence(units::acceleration_confidence(None)),
        ),
        None, // curvature
        None, // curvatureCalculationMode
        None, // yawRate
        None, // lateralAcceleration
        None, // verticalAcceleration
        None, // vruLanePosition
        None, // environment
        None, // movementControl
        None, // orientation
        None, // rollAngle
        None, // deviceUsage
    );
    let low = input.include_low_frequency.then(|| {
        VruLowFrequencyContainer::new(
            match input.profile {
                // `ordinary-pedestrian (1)` and `bicyclist (1)` in the CDD.
                VruProfile::Pedestrian => {
                    VruProfileAndSubprofile::pedestrian(VruSubProfilePedestrian(1))
                }
                VruProfile::Bicyclist => {
                    VruProfileAndSubprofile::bicyclistAndLightVruVehicle(VruSubProfileBicyclist(1))
                }
            },
            None, // sizeClass
            None, // exteriorLights
        )
    });
    let parameters = VamParameters::new(basic, high, low, None, None, None);
    Ok(VAM::new(
        header,
        VruAwareness::new(generation_delta_time(&input.generation_time), parameters),
    ))
}

/// UPER-encodes a VAM. The bytes are the wire bytes and the size is exact.
pub fn encode_vam(vam: &VAM) -> Result<Encoded, CodecError> {
    Ok(Encoded::uper(uper_encode(MsgType::Vam, vam)?))
}

/// UPER-decodes a VAM.
pub fn decode_vam(bytes: &[u8]) -> Result<VAM, CodecError> {
    uper_decode(MsgType::Vam, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::geom::Vec3;

    fn input(low: bool) -> VamInput {
        let mut position = PositionEstimate::no_fix(0);
        position.pos = Vec3::new(120.0, -40.0, 0.0);
        position.vel = Vec3::new_2d(1.2, 0.5);
        position.heading_rad = 0.4;
        position.semi_major_m = 3.0;
        position.semi_minor_m = 2.0;
        VamInput {
            station_id: 0xDEAD_BEEF,
            profile: VruProfile::Pedestrian,
            position,
            origin: GeoOrigin::new(40.7440, -73.9900, 0.0),
            generation_time: TimestampIts(726_000_000_000),
            longitudinal_acceleration_mps2: None,
            include_low_frequency: low,
        }
    }

    #[test]
    fn a_vam_round_trips_through_real_uper() {
        for low in [false, true] {
            let vam = build_vam(&input(low)).unwrap();
            let e = encode_vam(&vam).unwrap();
            assert_eq!(decode_vam(&e.bytes).unwrap(), vam);
            assert_eq!(vam.header.0.message_id.0, VAM_MESSAGE_ID);
            assert_eq!(vam.header.0.protocol_version.0, VAM_PROTOCOL_VERSION);
        }
        // The low-frequency container costs bytes; a VAM with it is longer.
        let short = encode_vam(&build_vam(&input(false)).unwrap()).unwrap();
        let long = encode_vam(&build_vam(&input(true)).unwrap()).unwrap();
        assert!(long.bytes.len() > short.bytes.len());
        // A VAM of this shape is a few tens of octets, well under the 235-350 B published
        // for *secured* VAMs, whose envelope the device adds on top.
        assert!(short.bytes.len() < 60, "{} B", short.bytes.len());
    }

    #[test]
    fn a_cyclist_says_so_and_a_pedestrian_says_so() {
        let mut i = input(true);
        i.profile = VruProfile::Bicyclist;
        let vam = build_vam(&i).unwrap();
        assert_eq!(vam.vam.vam_parameters.basic_container.station_type.0, 2);
        assert!(matches!(
            vam.vam
                .vam_parameters
                .vru_low_frequency_container
                .as_ref()
                .map(|c| &c.profile_and_subprofile),
            Some(VruProfileAndSubprofile::bicyclistAndLightVruVehicle(_))
        ));
        let vam = build_vam(&input(true)).unwrap();
        assert_eq!(vam.vam.vam_parameters.basic_container.station_type.0, 1);
    }

    #[test]
    fn truncated_bytes_do_not_decode() {
        let e = encode_vam(&build_vam(&input(true)).unwrap()).unwrap();
        assert!(decode_vam(&e.bytes[..e.bytes.len() / 2]).is_err());
    }
}
