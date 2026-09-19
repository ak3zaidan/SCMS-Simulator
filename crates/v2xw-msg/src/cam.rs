//! Building, encoding and decoding a real ETSI CAM.
//!
//! The message is ETSI TS 103 900 (CAM, Release 2) over ETSI TS 102 894-2 Release 2. This
//! module turns the simulator's own state — a node's [`PositionEstimate`], its station
//! type, its body dimensions and a few vehicle dynamics — into a `CAM` the generated
//! bindings can encode, and encodes it with real UPER.
//!
//! # What goes in
//!
//! Everything comes from *belief*, never from ground truth: [`CamInput::position`] is the
//! output of the node's GNSS model, which is where the position error, the outage and the
//! spoofing live. A generator that reached past it into the world would make every
//! position-plausibility detector trivially perfect (invariant I-C2,
//! [`v2xw_core::nodeview::NodeView`]).
//!
//! # What comes out
//!
//! A `CAM` with the two mandatory containers — basic and high-frequency — and optionally
//! the low-frequency container, whose cadence EN 302 637-2 §6.1.3 fixes at "the first CAM,
//! then every ≥ 500 ms" and which [`crate::generator::CamGenerator`] schedules.
//!
//! # Size
//!
//! A typical unsecured CAM of this shape encodes to about 60 bytes of UPER
//! ([`tests::typical_cam_size_is_consistent_with_the_field_range`] pins the exact number).
//! 04-models.md §8.2 reports *secured, captured* CAMs of 182–199 B at minimum and 357 B on
//! average; the gap is the security envelope and the lower layers, and the test does that
//! arithmetic explicitly rather than leaving the reader to wonder whether 60 B is wrong.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::Dims;
use v2xw_core::time::{SimTime, TimeError, WallClock};

use crate::asn1::cam_asn1::{
    BasicVehicleContainerHighFrequency, BasicVehicleContainerLowFrequency, CAM, CamParameters,
    CamPayload, HighFrequencyContainer, LowFrequencyContainer,
};
use crate::asn1::cdd::{
    AccelerationComponent, AccelerationConfidence, AccelerationValue, Altitude, AltitudeConfidence,
    AltitudeValue, BasicContainer, Curvature, CurvatureCalculationMode, CurvatureConfidence,
    CurvatureValue, DriveDirection as CddDriveDirection, ExteriorLights, GenerationDeltaTime,
    Heading, HeadingConfidence, HeadingValue, ItsPduHeader, Latitude, Longitude, MessageId,
    OrdinalNumber1B, Path, PathPoint, PositionConfidenceEllipse, ReferencePositionWithConfidence,
    SemiAxisLength, Speed, SpeedConfidence, SpeedValue, StationId, TimestampIts,
    TrafficParticipantType, VehicleLength, VehicleLengthConfidenceIndication, VehicleLengthValue,
    VehicleRole as CddVehicleRole, VehicleWidth, Wgs84AngleValue, YawRate, YawRateConfidence,
    YawRateValue,
};
use crate::codec::{Encoded, MsgType, uper_decode, uper_encode};
use crate::error::CodecError;
use crate::units;

/// ITS PDU protocol version of a Release 2 CAM.
///
/// Pinned by the ASN.1 itself: `CAM ::= SEQUENCE { header ItsPduHeader (WITH COMPONENTS
/// {…, protocolVersion (2), messageId(cam)}), … }`. `rasn-compiler` does not carry inner
/// subtyping into the generated type, so the constraint is honoured here instead.
pub const CAM_PROTOCOL_VERSION: u8 = 2;

/// `MessageId` of a CAM: `cam(2)` in `ETSI-ITS-CDD.asn`.
pub const CAM_MESSAGE_ID: u8 = 2;

/// Largest number of points a CDD `Path` can carry: `Path ::= SEQUENCE (SIZE(0..40)) OF
/// PathPoint`. A longer path history is truncated to the most recent 40 points.
pub const MAX_PATH_POINTS: usize = 40;

/// What kind of traffic participant the sending station is (`TrafficParticipantType`).
///
/// Re-declared rather than re-exported from the generated bindings so that a caller
/// (`v2xw-node`, a scenario file) never has to touch ASN.1 types to say "this is a bus".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParticipantType {
    /// Not known.
    Unknown,
    /// On foot.
    Pedestrian,
    /// On a bicycle.
    Cyclist,
    /// A moped.
    Moped,
    /// A motorcycle.
    Motorcycle,
    /// A passenger car — the simulator's default vehicle.
    PassengerCar,
    /// A bus.
    Bus,
    /// A light truck.
    LightTruck,
    /// A heavy truck.
    HeavyTruck,
    /// A trailer.
    Trailer,
    /// A special vehicle.
    SpecialVehicle,
    /// A tram.
    Tram,
    /// A light VRU vehicle (e-scooter and the like).
    LightVruVehicle,
    /// An animal.
    Animal,
    /// An agricultural vehicle.
    Agricultural,
    /// Roadside infrastructure — what an RSU sends as.
    Infrastructure,
}

impl ParticipantType {
    /// The CDD code, from `TrafficParticipantType ::= INTEGER { unknown(0), … }`.
    pub const fn code(self) -> u8 {
        match self {
            ParticipantType::Unknown => 0,
            ParticipantType::Pedestrian => 1,
            ParticipantType::Cyclist => 2,
            ParticipantType::Moped => 3,
            ParticipantType::Motorcycle => 4,
            ParticipantType::PassengerCar => 5,
            ParticipantType::Bus => 6,
            ParticipantType::LightTruck => 7,
            ParticipantType::HeavyTruck => 8,
            ParticipantType::Trailer => 9,
            ParticipantType::SpecialVehicle => 10,
            ParticipantType::Tram => 11,
            ParticipantType::LightVruVehicle => 12,
            ParticipantType::Animal => 13,
            ParticipantType::Agricultural => 14,
            ParticipantType::Infrastructure => 15,
        }
    }

    /// True for the station types that send an RSU high-frequency container rather than a
    /// vehicle one (EN 302 637-2 §6.1.2).
    pub const fn is_infrastructure(self) -> bool {
        matches!(self, ParticipantType::Infrastructure)
    }
}

/// Which way the vehicle is moving along its own longitudinal axis.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DriveDirection {
    /// Moving forwards.
    #[default]
    Forward,
    /// Reversing.
    Backward,
    /// Not known.
    Unavailable,
}

impl DriveDirection {
    fn to_cdd(self) -> CddDriveDirection {
        match self {
            DriveDirection::Forward => CddDriveDirection::forward,
            DriveDirection::Backward => CddDriveDirection::backward,
            DriveDirection::Unavailable => CddDriveDirection::unavailable,
        }
    }
}

/// The role a vehicle plays, carried in the low-frequency container (`VehicleRole`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum VehicleRole {
    /// An ordinary vehicle.
    #[default]
    Default,
    /// Public transport.
    PublicTransport,
    /// Special transport.
    SpecialTransport,
    /// Carrying dangerous goods.
    DangerousGoods,
    /// Road works.
    RoadWork,
    /// Rescue.
    Rescue,
    /// Emergency.
    Emergency,
    /// Safety car.
    SafetyCar,
    /// Agriculture.
    Agriculture,
    /// Commercial.
    Commercial,
    /// Military.
    Military,
    /// Road operator.
    RoadOperator,
    /// Taxi.
    Taxi,
}

impl VehicleRole {
    fn to_cdd(self) -> CddVehicleRole {
        match self {
            VehicleRole::Default => CddVehicleRole::default,
            VehicleRole::PublicTransport => CddVehicleRole::publicTransport,
            VehicleRole::SpecialTransport => CddVehicleRole::specialTransport,
            VehicleRole::DangerousGoods => CddVehicleRole::dangerousGoods,
            VehicleRole::RoadWork => CddVehicleRole::roadWork,
            VehicleRole::Rescue => CddVehicleRole::rescue,
            VehicleRole::Emergency => CddVehicleRole::emergency,
            VehicleRole::SafetyCar => CddVehicleRole::safetyCar,
            VehicleRole::Agriculture => CddVehicleRole::agriculture,
            VehicleRole::Commercial => CddVehicleRole::commercial,
            VehicleRole::Military => CddVehicleRole::military,
            VehicleRole::RoadOperator => CddVehicleRole::roadOperator,
            VehicleRole::Taxi => CddVehicleRole::taxi,
        }
    }
}

/// The eight exterior-light switches of `ExteriorLights`, as a bit mask.
///
/// Bit numbering is the CDD's: `lowBeamHeadlightsOn(0)` … `parkingLightsOn(7)`, with bit 0
/// the **most significant** bit of the encoded octet, which is what `BIT STRING` means and
/// what [`ExteriorLightMask::to_cdd`] implements.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ExteriorLightMask(pub u8);

impl ExteriorLightMask {
    /// No light switch on — what an ITS-S sends when it has no light information.
    pub const NONE: ExteriorLightMask = ExteriorLightMask(0);
    /// `lowBeamHeadlightsOn(0)`.
    pub const LOW_BEAM: ExteriorLightMask = ExteriorLightMask(1 << 0);
    /// `highBeamHeadlightsOn(1)`.
    pub const HIGH_BEAM: ExteriorLightMask = ExteriorLightMask(1 << 1);
    /// `leftTurnSignalOn(2)`.
    pub const LEFT_TURN: ExteriorLightMask = ExteriorLightMask(1 << 2);
    /// `rightTurnSignalOn(3)`.
    pub const RIGHT_TURN: ExteriorLightMask = ExteriorLightMask(1 << 3);
    /// `daytimeRunningLightsOn(4)`.
    pub const DAYTIME_RUNNING: ExteriorLightMask = ExteriorLightMask(1 << 4);
    /// `reverseLightOn(5)`.
    pub const REVERSE: ExteriorLightMask = ExteriorLightMask(1 << 5);
    /// `fogLightOn(6)`.
    pub const FOG: ExteriorLightMask = ExteriorLightMask(1 << 6);
    /// `parkingLightsOn(7)`.
    pub const PARKING: ExteriorLightMask = ExteriorLightMask(1 << 7);
    /// Hazard warning: the CDD says it is both turn signals at once.
    pub const HAZARD: ExteriorLightMask = ExteriorLightMask(Self::LEFT_TURN.0 | Self::RIGHT_TURN.0);

    /// Union of two masks.
    pub const fn with(self, other: ExteriorLightMask) -> ExteriorLightMask {
        ExteriorLightMask(self.0 | other.0)
    }

    /// Whether every bit of `other` is set here.
    pub const fn contains(self, other: ExteriorLightMask) -> bool {
        self.0 & other.0 == other.0
    }

    fn to_cdd(self) -> ExteriorLights {
        let mut bits = rasn::types::FixedBitString::<8usize>::ZERO;
        for i in 0..8usize {
            bits.set(i, self.0 & (1 << i) != 0);
        }
        ExteriorLights(bits)
    }
}

/// One point of a CAM path history, expressed the way the simulator holds it.
///
/// The CDD carries path history as *deltas from the reference position*, which is what
/// [`build_cam`] computes; a caller supplies absolute past positions in the world's ENU
/// frame and lets the builder do the subtraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathHistoryPoint {
    /// The past position, world ENU metres.
    pub pos: v2xw_core::geom::Vec3,
    /// How long before the current CAM the vehicle was there.
    pub age: v2xw_core::time::Duration,
}

/// The contents of the CAM low-frequency container.
#[derive(Debug, Clone, PartialEq)]
pub struct CamLowFrequency {
    /// The vehicle's role.
    pub vehicle_role: VehicleRole,
    /// Which exterior light switches are on.
    pub exterior_lights: ExteriorLightMask,
    /// Recent positions, most recent first. Truncated to [`MAX_PATH_POINTS`].
    pub path_history: Vec<PathHistoryPoint>,
}

impl Default for CamLowFrequency {
    /// An ordinary vehicle with no lights on and no path history — the smallest valid
    /// low-frequency container.
    fn default() -> Self {
        Self {
            vehicle_role: VehicleRole::Default,
            exterior_lights: ExteriorLightMask::NONE,
            path_history: Vec::new(),
        }
    }
}

/// Everything [`build_cam`] needs, in the simulator's own units and frames.
///
/// Angles are ENU radians (D6), lengths metres, speeds m/s, accelerations m/s², yaw rate
/// rad/s, curvature m⁻¹. `Option::None` on a dynamics field means "this station does not
/// measure it", and encodes as the element's `unavailable` sentinel — not as a zero.
#[derive(Debug, Clone, PartialEq)]
pub struct CamInput {
    /// The sending station's id, which is its current pseudonym.
    pub station_id: u32,
    /// What kind of station it is.
    pub station_type: ParticipantType,
    /// The station's *belief* about where and when it is.
    pub position: PositionEstimate,
    /// The world's geodetic origin, to turn ENU metres back into latitude and longitude.
    pub origin: GeoOrigin,
    /// The body's dimensions.
    pub dims: Dims,
    /// Which way it is moving.
    pub drive_direction: DriveDirection,
    /// Longitudinal acceleration, m/s², positive forwards.
    pub longitudinal_acceleration_mps2: Option<f64>,
    /// Lateral acceleration, m/s², positive to the left. Optional in the ASN.1 too.
    pub lateral_acceleration_mps2: Option<f64>,
    /// Yaw rate, rad/s, positive anti-clockwise (a left turn).
    pub yaw_rate_rad_s: Option<f64>,
    /// Path curvature, m⁻¹, positive to the left.
    pub curvature_inv_m: Option<f64>,
    /// Whether the curvature was computed with the yaw rate.
    pub curvature_from_yaw_rate: bool,
    /// One-sigma speed accuracy, m/s, if the station reports one.
    pub speed_accuracy_mps: Option<f64>,
    /// One-sigma heading accuracy, radians, if the station reports one.
    pub heading_accuracy_rad: Option<f64>,
    /// One-sigma acceleration accuracy, m/s², if the station reports one.
    pub acceleration_accuracy_mps2: Option<f64>,
    /// The instant the reference position is valid at, as a `TimestampIts`.
    pub generation_time: TimestampIts,
    /// The low-frequency container, when this CAM carries one.
    pub low_frequency: Option<CamLowFrequency>,
}

impl CamInput {
    /// A minimal input: a passenger car with no dynamics measurements and no
    /// low-frequency container.
    ///
    /// Everything optional is `None`, so an unfilled field encodes as its `unavailable`
    /// sentinel rather than as a plausible zero. A caller fills in what its node models.
    pub fn new(
        station_id: u32,
        station_type: ParticipantType,
        position: PositionEstimate,
        origin: GeoOrigin,
        dims: Dims,
        generation_time: TimestampIts,
    ) -> Self {
        Self {
            station_id,
            station_type,
            position,
            origin,
            dims,
            drive_direction: DriveDirection::Forward,
            longitudinal_acceleration_mps2: None,
            lateral_acceleration_mps2: None,
            yaw_rate_rad_s: None,
            curvature_inv_m: None,
            curvature_from_yaw_rate: false,
            speed_accuracy_mps: None,
            heading_accuracy_rad: None,
            acceleration_accuracy_mps2: None,
            generation_time,
            low_frequency: None,
        }
    }
}

/// `TimestampIts` at simulated time `t`: milliseconds since 2004-01-01T00:00:00Z.
///
/// Derived from [`WallClock::time64`], which counts microseconds from the same epoch, so
/// the two never disagree about what instant a message claims. ETSI defines `TimestampIts`
/// against TAI including leap seconds; the engine's wall clock is a plain offset from a
/// civil `t0`, so the value is off by the accumulated leap seconds of the scenario date
/// (37 s as of 2026). That matters for comparing against a real capture and not at all for
/// comparing two simulated messages, and it is recorded as a card assumption rather than
/// silently fixed with a table that would itself need maintaining.
pub fn timestamp_its(clock: WallClock, t: SimTime) -> Result<TimestampIts, TimeError> {
    Ok(TimestampIts(clock.time64(t)? / 1_000))
}

/// `GenerationDeltaTime` is `TimestampIts mod 65 536` (CDD, `GenerationDeltaTime`).
pub fn generation_delta_time(ts: &TimestampIts) -> GenerationDeltaTime {
    GenerationDeltaTime((ts.0 % 65_536) as u16)
}

/// Fills a `CAM` from the simulator's state.
///
/// Fails only when the position cannot be interpreted as a geodetic coordinate at all —
/// every other out-of-range quantity saturates to the sentinel its ASN.1 type declares,
/// which is what a conformant ITS-S does (see [`crate::units`]).
pub fn build_cam(input: &CamInput) -> Result<CAM, CodecError> {
    let header = ItsPduHeader::new(
        OrdinalNumber1B(CAM_PROTOCOL_VERSION),
        MessageId(CAM_MESSAGE_ID),
        StationId(input.station_id),
    );

    let basic = BasicContainer::new(
        TrafficParticipantType(input.station_type.code()),
        reference_position(&input.position, &input.origin)?,
    );

    let high_frequency = if input.station_type.is_infrastructure() {
        // EN 302 637-2 §6.1.2: an RSU sends `rsuContainerHighFrequency`, whose only field
        // is the optional protected-zone list. Modelling those zones is not this crate's
        // job, so the container is present and empty — which is both valid and honest.
        HighFrequencyContainer::rsuContainerHighFrequency(
            crate::asn1::cam_asn1::RSUContainerHighFrequency::new(None),
        )
    } else {
        HighFrequencyContainer::basicVehicleContainerHighFrequency(vehicle_high_frequency(input))
    };

    let low_frequency = input
        .low_frequency
        .as_ref()
        .map(|lf| low_frequency_container(lf, input));

    let parameters = CamParameters::new(basic, high_frequency, low_frequency, None, None);
    let payload = CamPayload::new(generation_delta_time(&input.generation_time), parameters);
    Ok(CAM::new(header, payload))
}

fn reference_position(
    p: &PositionEstimate,
    origin: &GeoOrigin,
) -> Result<ReferencePositionWithConfidence, CodecError> {
    let (lat_deg, lon_deg, alt_m) = origin.to_geodetic(p.pos);
    let latitude = units::latitude(lat_deg).ok_or(CodecError::OutOfRange {
        field: "cam.camParameters.basicContainer.referencePosition.latitude",
        asn1_type: "Latitude",
        value: lat_deg as i64,
        min: -90,
        max: 90,
    })?;
    let longitude = units::longitude(lon_deg).ok_or(CodecError::OutOfRange {
        field: "cam.camParameters.basicContainer.referencePosition.longitude",
        asn1_type: "Longitude",
        value: lon_deg as i64,
        min: -180,
        max: 180,
    })?;

    Ok(ReferencePositionWithConfidence::new(
        Latitude(latitude),
        Longitude(longitude),
        PositionConfidenceEllipse::new(
            SemiAxisLength(units::semi_axis_length(p.semi_major_m)),
            SemiAxisLength(units::semi_axis_length(p.semi_minor_m)),
            Wgs84AngleValue(units::wgs84_angle(p.orientation_rad)),
        ),
        Altitude::new(
            AltitudeValue(units::altitude(alt_m)),
            // The GNSS belief carries a horizontal ellipse only, so the vertical accuracy
            // is genuinely not known here. `unavailable` is the truthful encoding.
            AltitudeConfidence::unavailable,
        ),
    ))
}

fn vehicle_high_frequency(input: &CamInput) -> BasicVehicleContainerHighFrequency {
    let p = &input.position;
    BasicVehicleContainerHighFrequency::new(
        Heading::new(
            HeadingValue(units::heading_value(p.heading_rad)),
            HeadingConfidence(units::heading_confidence(input.heading_accuracy_rad)),
        ),
        Speed::new(
            SpeedValue(units::speed_value(p.ground_speed_mps())),
            SpeedConfidence(units::speed_confidence(input.speed_accuracy_mps)),
        ),
        input.drive_direction.to_cdd(),
        VehicleLength::new(
            VehicleLengthValue(units::vehicle_length_value(input.dims.length_m)),
            // The simulator models no trailers, so "no trailer present" is the accurate
            // answer rather than "unknown".
            VehicleLengthConfidenceIndication::noTrailerPresent,
        ),
        VehicleWidth(units::vehicle_width(input.dims.width_m)),
        AccelerationComponent::new(
            AccelerationValue(units::acceleration_value(
                input.longitudinal_acceleration_mps2,
            )),
            AccelerationConfidence(units::acceleration_confidence(
                input.acceleration_accuracy_mps2,
            )),
        ),
        Curvature::new(
            CurvatureValue(units::curvature_value(input.curvature_inv_m)),
            if input.curvature_inv_m.is_some() {
                // The CDD's confidence ladder is coarse; 0,002 m^-1 is the bucket a
                // simulated curvature derived from a modelled trajectory belongs in.
                CurvatureConfidence::onePerMeter_0_002
            } else {
                CurvatureConfidence::unavailable
            },
        ),
        if input.curvature_from_yaw_rate {
            CurvatureCalculationMode::yawRateUsed
        } else {
            CurvatureCalculationMode::yawRateNotUsed
        },
        YawRate::new(
            YawRateValue(units::yaw_rate_value(input.yaw_rate_rad_s)),
            if input.yaw_rate_rad_s.is_some() {
                YawRateConfidence::degSec_001_00
            } else {
                YawRateConfidence::unavailable
            },
        ),
        None, // accelerationControl
        None, // lanePosition
        None, // steeringWheelAngle
        input.lateral_acceleration_mps2.map(|a| {
            AccelerationComponent::new(
                AccelerationValue(units::acceleration_value(Some(a))),
                AccelerationConfidence(units::acceleration_confidence(
                    input.acceleration_accuracy_mps2,
                )),
            )
        }),
        None, // verticalAcceleration
        None, // performanceClass
        None, // cenDsrcTollingZone
    )
}

fn low_frequency_container(lf: &CamLowFrequency, input: &CamInput) -> LowFrequencyContainer {
    let (ref_lat, ref_lon, ref_alt) = input.origin.to_geodetic(input.position.pos);
    let mut points: Vec<PathPoint> = Vec::with_capacity(lf.path_history.len().min(MAX_PATH_POINTS));
    for point in lf.path_history.iter().take(MAX_PATH_POINTS) {
        let (lat, lon, alt) = input.origin.to_geodetic(point.pos);
        points.push(PathPoint::new(
            crate::asn1::cdd::DeltaReferencePosition::new(
                crate::asn1::cdd::DeltaLatitude(units::delta_degrees(lat - ref_lat)),
                crate::asn1::cdd::DeltaLongitude(units::delta_degrees(lon - ref_lon)),
                crate::asn1::cdd::DeltaAltitude(units::delta_altitude(alt - ref_alt)),
            ),
            // `PathDeltaTime ::= INTEGER (1..65535, ...)`, unit 10 ms. Zero is not
            // expressible, so a point captured at the reference instant carries 1.
            Some(crate::asn1::cdd::PathDeltaTime(rasn::types::Integer::from(
                (point.age.as_nanos() / 10_000_000).clamp(1, 65_535),
            ))),
        ));
    }

    LowFrequencyContainer::basicVehicleContainerLowFrequency(
        BasicVehicleContainerLowFrequency::new(
            lf.vehicle_role.to_cdd(),
            lf.exterior_lights.to_cdd(),
            Path(points),
        ),
    )
}

/// UPER-encodes a CAM. The bytes are the wire bytes and the size is exact (I-S2).
pub fn encode_cam(cam: &CAM) -> Result<Encoded, CodecError> {
    Ok(Encoded::uper(uper_encode(MsgType::Cam, cam)?))
}

/// UPER-decodes a CAM.
pub fn decode_cam(bytes: &[u8]) -> Result<CAM, CodecError> {
    uper_decode(MsgType::Cam, bytes)
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! One representative vehicle, shared by this module's tests and the generator's.

    use super::*;
    use v2xw_core::geom::Vec3;

    /// The Phase 1 world's origin: the south-west corner of the Manhattan preset (D7).
    pub fn manhattan() -> GeoOrigin {
        GeoOrigin::new(40.7440, -73.9900, 0.0)
    }

    /// A passenger car doing 50 km/h north-east, 1,200 m east and 800 m north of the
    /// origin, with a metre-scale GNSS error ellipse.
    pub fn car(generation_time: TimestampIts) -> CamInput {
        let speed = 13.89;
        // ENU north-east, so each component is speed / sqrt(2).
        let heading = core::f64::consts::FRAC_PI_4;
        let component = speed * core::f64::consts::FRAC_1_SQRT_2;
        let position = PositionEstimate {
            pos: Vec3::new(1_200.0, 800.0, 12.5),
            vel: Vec3::new(component, component, 0.0),
            heading_rad: heading,
            semi_major_m: 1.8,
            semi_minor_m: 1.1,
            orientation_rad: 0.6,
            time_ns: 0,
            fix: v2xw_core::belief::FixQuality::ThreeD,
        };
        let mut input = CamInput::new(
            0x0A0B_0C0D,
            ParticipantType::PassengerCar,
            position,
            manhattan(),
            Dims::CAR,
            generation_time,
        );
        input.longitudinal_acceleration_mps2 = Some(0.8);
        input.yaw_rate_rad_s = Some(0.05);
        input.curvature_inv_m = Some(0.004);
        input.curvature_from_yaw_rate = true;
        input.speed_accuracy_mps = Some(0.25);
        input.heading_accuracy_rad = Some(0.02);
        input.acceleration_accuracy_mps2 = Some(0.3);
        input
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{EtsiUperCodec, Message, MessageCodec, SizeSource};
    use v2xw_core::geom::Vec3;

    fn ts() -> TimestampIts {
        // 2026-09-18T12:00:00Z expressed as ms since the 2004 epoch.
        let clock = WallClock::parse_rfc3339("2026-09-18T12:00:00Z").expect("parses");
        timestamp_its(clock, 0).expect("after the 1609.2 epoch")
    }

    #[test]
    fn round_trip_is_exact() {
        let cam = build_cam(&fixtures::car(ts())).expect("builds");
        let encoded = encode_cam(&cam).expect("encodes");
        assert_eq!(encoded.size_source, SizeSource::Uper);
        assert_eq!(encoded.size as usize, encoded.bytes.len());
        let back = decode_cam(&encoded.bytes).expect("decodes");
        assert_eq!(cam, back, "UPER round-trip must be lossless");
    }

    #[test]
    fn round_trip_is_exact_with_the_low_frequency_container() {
        let mut input = fixtures::car(ts());
        input.low_frequency = Some(CamLowFrequency {
            vehicle_role: VehicleRole::Taxi,
            exterior_lights: ExteriorLightMask::LOW_BEAM.with(ExteriorLightMask::LEFT_TURN),
            path_history: (1..=8u64)
                .map(|i| PathHistoryPoint {
                    pos: Vec3::new(1_200.0 - i as f64 * 13.0, 800.0 - i as f64 * 9.0, 12.5),
                    age: v2xw_core::time::Duration::from_millis(i * 500),
                })
                .collect(),
        });
        let cam = build_cam(&input).expect("builds");
        let bytes = encode_cam(&cam).expect("encodes").bytes;
        assert_eq!(decode_cam(&bytes).expect("decodes"), cam);
    }

    #[test]
    fn the_header_carries_the_values_the_asn1_constrains_it_to() {
        let cam = build_cam(&fixtures::car(ts())).expect("builds");
        assert_eq!(cam.header.protocol_version.0, 2);
        assert_eq!(cam.header.message_id.0, 2);
        assert_eq!(cam.header.station_id.0, 0x0A0B_0C0D);
    }

    #[test]
    fn the_position_survives_the_projection_round_trip() {
        let input = fixtures::car(ts());
        let cam = build_cam(&input).expect("builds");
        let rp = &cam.cam.cam_parameters.basic_container.reference_position;
        let (lat, lon, _) = input.origin.to_geodetic(input.position.pos);
        // 1/10 microdegree is 11 mm of latitude: the encoded value must be the geodetic
        // one to within one unit.
        assert!((f64::from(rp.latitude.0) / 1e7 - lat).abs() < 2e-7, "{lat}");
        assert!(
            (f64::from(rp.longitude.0) / 1e7 - lon).abs() < 2e-7,
            "{lon}"
        );
    }

    #[test]
    fn the_dynamics_land_in_the_right_wire_units() {
        let cam = build_cam(&fixtures::car(ts())).expect("builds");
        let HighFrequencyContainer::basicVehicleContainerHighFrequency(hf) =
            &cam.cam.cam_parameters.high_frequency_container
        else {
            panic!("a car sends a vehicle high-frequency container");
        };
        assert_eq!(hf.speed.speed_value.0, 1389, "13,89 m/s at 0,01 m/s");
        assert_eq!(
            hf.heading.heading_value.0, 450,
            "ENU pi/4 is a 45 deg bearing"
        );
        assert_eq!(hf.vehicle_length.vehicle_length_value.0, 45, "4,5 m");
        assert_eq!(hf.vehicle_width.0, 18, "1,8 m");
        assert_eq!(hf.longitudinal_acceleration.value.0, 8, "0,8 m/s^2");
        assert_eq!(hf.curvature.curvature_value.0, 40, "0,004 x 10 000");
        assert_eq!(hf.drive_direction, CddDriveDirection::forward);
    }

    #[test]
    fn a_station_with_no_measurements_encodes_unavailable_not_zero() {
        let input = CamInput::new(
            7,
            ParticipantType::PassengerCar,
            PositionEstimate::no_fix(0),
            fixtures::manhattan(),
            Dims::CAR,
            ts(),
        );
        let cam = build_cam(&input).expect("builds even with no fix");
        let HighFrequencyContainer::basicVehicleContainerHighFrequency(hf) =
            &cam.cam.cam_parameters.high_frequency_container
        else {
            panic!("vehicle container");
        };
        assert_eq!(hf.longitudinal_acceleration.value.0, 161, "unavailable");
        assert_eq!(hf.yaw_rate.yaw_rate_value.0, 32767, "unavailable");
        assert_eq!(hf.curvature.curvature_value.0, 1023, "unavailable");
        let ellipse = &cam
            .cam
            .cam_parameters
            .basic_container
            .reference_position
            .position_confidence_ellipse;
        assert_eq!(ellipse.semi_major_axis_length.0, 4095, "no fix, no ellipse");
        // And it still round-trips.
        let bytes = encode_cam(&cam).expect("encodes").bytes;
        assert_eq!(decode_cam(&bytes).expect("decodes"), cam);
    }

    #[test]
    fn an_rsu_sends_the_infrastructure_container() {
        let mut input = fixtures::car(ts());
        input.station_type = ParticipantType::Infrastructure;
        let cam = build_cam(&input).expect("builds");
        assert!(matches!(
            cam.cam.cam_parameters.high_frequency_container,
            HighFrequencyContainer::rsuContainerHighFrequency(_)
        ));
        let bytes = encode_cam(&cam).expect("encodes").bytes;
        assert_eq!(decode_cam(&bytes).expect("decodes"), cam);
    }

    #[test]
    fn a_position_outside_the_geodetic_frame_is_an_error_not_a_sentinel() {
        let mut input = fixtures::car(ts());
        // 20,000 km north of a mid-latitude origin is off the planet.
        input.position.pos = Vec3::new(0.0, 20_000_000.0, 0.0);
        let err = build_cam(&input).expect_err("cannot be a latitude");
        assert!(err.to_string().contains("Latitude"), "{err}");
    }

    /// The exterior-light mask must land on the bit numbering the CDD gives, bit 0 first.
    #[test]
    fn exterior_lights_use_the_cdd_bit_order() {
        let bits = ExteriorLightMask::LOW_BEAM
            .with(ExteriorLightMask::PARKING)
            .to_cdd();
        assert!(bits.0[0], "lowBeamHeadlightsOn is bit 0");
        assert!(bits.0[7], "parkingLightsOn is bit 7");
        assert_eq!(bits.0.count_ones(), 2);
        assert!(ExteriorLightMask::HAZARD.contains(ExteriorLightMask::LEFT_TURN));
        assert!(ExteriorLightMask::HAZARD.contains(ExteriorLightMask::RIGHT_TURN));
    }

    /// Determinism: the same input must produce the same bytes, every run, every process.
    #[test]
    fn encoded_size_and_bytes_are_stable() {
        let first = encode_cam(&build_cam(&fixtures::car(ts())).unwrap()).unwrap();
        for _ in 0..16 {
            let again = encode_cam(&build_cam(&fixtures::car(ts())).unwrap()).unwrap();
            assert_eq!(first.bytes, again.bytes);
            assert_eq!(first.size, again.size);
        }
    }

    /// 04-models.md §8.2 is about **secured** CAMs measured in the field: minimum 182 B
    /// (Renault) to 199 B (VW), overall mean 357 B with the certificate mix. This crate
    /// encodes the *unsecured facilities payload*, so the comparison only means something
    /// once the envelope and the lower layers are added back (04-models.md §9.1, §9.2,
    /// §9.3). That arithmetic is done here, so the number in any report is reproducible and
    /// the comparison is explicit about what it assumes.
    ///
    /// Two points are checked, because the field distribution has two published landmarks:
    ///
    /// * the **minimum** CAM — basic and high-frequency containers only, digest signer —
    ///   must land inside 182-199 B;
    /// * a **typical** CAM — with the low-frequency container and eight path-history points,
    ///   signed with an authorization ticket — must straddle the 357 B mean.
    #[test]
    fn typical_cam_size_is_consistent_with_the_field_range() {
        // 04-models.md §9.1: a 1609.2/TS 103 097 envelope is about 93 B with a digest
        // signer, and about 87 B plus the certificate with a certificate signer.
        // §9.2: an ETSI authorization ticket is 90-130 B.
        // §9.3: below-envelope headers for a CAM are GN SHB + BTP-B + LLC/SNAP = 52 B.
        const ENVELOPE_DIGEST: u32 = 93;
        const ENVELOPE_CERT_BASE: u32 = 87;
        const AT_MIN: u32 = 90;
        const AT_MAX: u32 = 130;
        const LOWER_LAYERS: u32 = 52;

        // --- the minimum CAM ---------------------------------------------------------
        let minimal = encode_cam(&build_cam(&fixtures::car(ts())).unwrap())
            .unwrap()
            .size;
        let minimal_secured = minimal + ENVELOPE_DIGEST + LOWER_LAYERS;
        assert!(
            (182..=199).contains(&minimal_secured),
            "a basic+HF CAM is {minimal} B of payload, which is {minimal_secured} B secured \
             with a digest signer — outside the 182-199 B field minimum of TR 2052 Table 6-2"
        );

        // --- a typical CAM -----------------------------------------------------------
        let mut input = fixtures::car(ts());
        input.low_frequency = Some(CamLowFrequency {
            vehicle_role: VehicleRole::Default,
            exterior_lights: ExteriorLightMask::LOW_BEAM,
            path_history: (1..=8u64)
                .map(|i| PathHistoryPoint {
                    pos: Vec3::new(1_200.0 - i as f64 * 13.0, 800.0 - i as f64 * 9.0, 12.5),
                    age: v2xw_core::time::Duration::from_millis(i * 500),
                })
                .collect(),
        });
        let typical = encode_cam(&build_cam(&input).unwrap()).unwrap().size;
        let low = typical + ENVELOPE_CERT_BASE + AT_MIN + LOWER_LAYERS;
        let high = typical + ENVELOPE_CERT_BASE + AT_MAX + LOWER_LAYERS;
        assert!(
            low <= 357 && 357 <= high,
            "a CAM with the low-frequency container and 8 path points is {typical} B of \
             payload, i.e. {low}-{high} B secured with an authorization ticket, which does \
             not straddle the 357 B field mean of TR 2052 Table 6-2"
        );
    }

    /// C2C-CC TR 2052 §3.1 measures CAM path history at **8-9 bytes per entry**. Our
    /// encoder is a real UPER encoder, so that is a number we can check rather than assume
    /// — and it is the one independent measurement in 04-models.md §8.2 that this crate can
    /// reproduce directly.
    #[test]
    fn the_path_history_increment_matches_the_measured_eight_to_nine_bytes() {
        let size_with = |points: u64| {
            let mut input = fixtures::car(ts());
            input.low_frequency = Some(CamLowFrequency {
                vehicle_role: VehicleRole::Default,
                exterior_lights: ExteriorLightMask::LOW_BEAM,
                path_history: (1..=points)
                    .map(|i| PathHistoryPoint {
                        pos: Vec3::new(1_200.0 - i as f64 * 13.0, 800.0 - i as f64 * 9.0, 12.5),
                        age: v2xw_core::time::Duration::from_millis(i * 500),
                    })
                    .collect(),
            });
            encode_cam(&build_cam(&input).unwrap()).unwrap().size
        };
        let empty = size_with(0);
        for points in [5u64, 10, 20, 40] {
            let per_entry = f64::from(size_with(points) - empty) / points as f64;
            assert!(
                (8.0..=9.0).contains(&per_entry),
                "{points} path points cost {per_entry:.2} B each, outside TR 2052's 8-9 B"
            );
        }
    }

    /// A `Path` is `SEQUENCE (SIZE(0..40)) OF PathPoint`, so a longer history must be
    /// truncated rather than make the encoder fail.
    #[test]
    fn a_path_history_longer_than_the_asn1_allows_is_truncated() {
        let mut input = fixtures::car(ts());
        input.low_frequency = Some(CamLowFrequency {
            vehicle_role: VehicleRole::Default,
            exterior_lights: ExteriorLightMask::NONE,
            path_history: (1..=100u64)
                .map(|i| PathHistoryPoint {
                    pos: Vec3::new(1_200.0 - i as f64 * 2.0, 800.0, 12.5),
                    age: v2xw_core::time::Duration::from_millis(i * 100),
                })
                .collect(),
        });
        let cam = build_cam(&input).expect("builds");
        let Some(LowFrequencyContainer::basicVehicleContainerLowFrequency(lf)) =
            &cam.cam.cam_parameters.low_frequency_container
        else {
            panic!("the low-frequency container is present");
        };
        assert_eq!(lf.path_history.0.len(), MAX_PATH_POINTS);
        let bytes = encode_cam(&cam).expect("encodes").bytes;
        assert_eq!(decode_cam(&bytes).expect("decodes"), cam);
    }

    #[test]
    fn the_codec_seam_encodes_the_same_bytes_as_the_direct_call() {
        let cam = build_cam(&fixtures::car(ts())).unwrap();
        let direct = encode_cam(&cam).unwrap();
        let codec = EtsiUperCodec::new();
        let via_seam = codec.encode(&Message::Cam(Box::new(cam.clone()))).unwrap();
        assert_eq!(direct, via_seam);
        let Message::Cam(decoded) = codec.decode(&via_seam.bytes, MsgType::Cam).unwrap() else {
            panic!("a CAM decodes to a CAM");
        };
        assert_eq!(*decoded, cam);
    }

    #[test]
    fn generation_delta_time_wraps_at_65536_ms() {
        assert_eq!(generation_delta_time(&TimestampIts(0)).0, 0);
        assert_eq!(generation_delta_time(&TimestampIts(65_535)).0, 65_535);
        assert_eq!(generation_delta_time(&TimestampIts(65_536)).0, 0);
        assert_eq!(generation_delta_time(&TimestampIts(65_537)).0, 1);
    }
}
