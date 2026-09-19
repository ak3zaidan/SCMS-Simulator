//! Building, encoding and decoding a real ETSI DENM, with the EN 302 637-3 lifecycle.
//!
//! The message is ETSI TS 103 831 (DENM, Release 2) over ETSI TS 102 894-2 Release 2. A
//! DENM is not a periodic broadcast like a CAM: it announces an *event*, lives for a
//! declared validity, may be repeated for as long as the application says, and ends by
//! being cancelled by its originator or negated by someone else. This module holds both
//! halves — the encoder and the lifecycle — because the lifecycle is what decides which
//! fields the encoder has to fill.
//!
//! # The rules, from 04-models.md §8.1 and EN 302 637-3 V1.3.1
//!
//! | Rule | Clause | Where |
//! |---|---|---|
//! | A new DENM gets an unused `actionId` | §8.2.1.5 | [`ActionIdAllocator`] |
//! | An update keeps the `actionId` and advances `referenceTime` | §6.1.2 | [`DenmService::update`] |
//! | Repetition happens only if the application supplies `repetitionInterval` **and** `repetitionDuration`; neither is carried in the message | §6.1.4.2 | [`Repetition`] |
//! | `T_Repetition = repetitionInterval`, bounded by `validityDuration` | §6.1.4.2 | [`DenmService::poll`] |
//! | Default validity is 600 s from `detectionTime` | ASN.1 `defaultValidity INTEGER ::= 600` | [`DEFAULT_VALIDITY`] |
//! | Termination is a cancellation (originator) or a negation (other ITS-S), transmitted at least once | §8.3.2.5 | [`DenmService::cancel`], [`DenmService::negate`] |
//! | Keep-alive forwarding at `2 × transmissionInterval + U(0, 150 ms)`, capped at validity | Annex B | [`forwarding_delay`] |
//!
//! # The one structural constraint the generated code cannot express
//!
//! `DenmPayload`'s ASN.1 carries an inner-subtyping constraint:
//!
//! ```asn1
//! ((WITH COMPONENTS {..., management (WITH COMPONENTS {..., termination ABSENT}),
//!                         situation PRESENT, location PRESENT}) |
//!  (WITH COMPONENTS {..., management (WITH COMPONENTS {..., termination PRESENT}),
//!                         situation ABSENT, location ABSENT, alacarte ABSENT}))
//! ```
//!
//! — an ordinary DENM must carry a situation and a location container, and a terminating
//! one must carry neither. `rasn-compiler` does not carry `WITH COMPONENTS` inner subtyping
//! into the generated types (the same gap that stops ETSI TS 102 941 compiling, build
//! decision D5), so the constraint is enforced here instead: [`build_denm`] fills both
//! containers and [`build_termination_denm`] fills neither, and
//! [`tests::a_termination_denm_carries_no_situation_or_location`] pins it.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geo::GeoOrigin;
use v2xw_core::time::{Duration, SimTime};

use crate::asn1::cdd::{
    ActionId, Altitude, AltitudeConfidence, AltitudeValue, CauseCodeChoice, CauseCodeV2,
    DeltaTimeMilliSecondPositive, DeltaTimeSecond, HeadingValue, InformationQuality, ItsPduHeader,
    Latitude, Longitude, MessageId, OrdinalNumber1B, Path, PosConfidenceEllipse, ReferencePosition,
    SemiAxisLength, SequenceNumber, StandardLength3b, StationId, StationType, TimestampIts, Traces,
    TrafficParticipantType,
};
use crate::asn1::denm_asn1::{
    DENM, DenmPayload, LocationContainer, ManagementContainer, SituationContainer, Termination,
};
use crate::codec::{Encoded, MsgType, uper_decode, uper_encode};
use crate::error::CodecError;
use crate::units;

/// ITS PDU protocol version of a Release 2 DENM, from the ASN.1's own inner subtyping.
pub const DENM_PROTOCOL_VERSION: u8 = 2;

/// `MessageId` of a DENM: `denm(1)`.
pub const DENM_MESSAGE_ID: u8 = 1;

/// `defaultValidity INTEGER ::= 600` in `DENM-PDU-Descriptions.asn`: 600 seconds from
/// `detectionTime`, which is also what 04-models.md §8.1 records.
pub const DEFAULT_VALIDITY: Duration = Duration::from_secs(600);

/// `DeltaTimeSecond ::= INTEGER (0..86400)` — the longest validity a DENM can declare.
pub const MAX_VALIDITY_S: u32 = 86_400;

/// Keep-alive forwarding jitter bound, `U(0, 150 ms)` (EN 302 637-3 Annex B).
pub const FORWARDING_JITTER: Duration = Duration::from_millis(150);

/// The hazards this crate can name, mapped onto `CauseCodeV2`.
///
/// A deliberate subset. `CauseCodeChoice` has some forty alternatives, most of which no
/// simulated scenario produces; each variant here exists because a model in 04-models.md
/// can actually raise it. Adding one is a line in [`DenmCause::to_cause_code`] plus a line
/// here — and a scenario that needs an unusual cause can build the `CauseCodeV2` itself and
/// use [`DenmInput::cause_code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DenmCause {
    /// `trafficCondition(1)` — congestion ahead.
    TrafficCondition,
    /// `accident(2)`.
    Accident,
    /// `roadworks(3)`.
    Roadworks,
    /// `adhesion(6)` — a slippery surface, which is what the weather model raises.
    Adhesion,
    /// `hazardousLocation-ObstacleOnTheRoad(10)`.
    ObstacleOnTheRoad,
    /// `humanPresenceOnTheRoad(12)` — the VRU hazard.
    HumanPresenceOnTheRoad,
    /// `dangerousEndOfQueue(27)`.
    DangerousEndOfQueue,
    /// `emergencyVehicleApproaching(95)`.
    EmergencyVehicleApproaching,
    /// `stationaryVehicle(94)`.
    StationaryVehicle,
    /// `dangerousSituation(99)` — the generic "hard braking ahead" cause, which is what the
    /// legacy `legacy-brake` preset of 04-models.md §8.1 emits.
    DangerousSituation,
}

impl DenmCause {
    /// The `CauseCodeV2` for this cause, with sub-cause `0` (`unavailable`).
    ///
    /// Sub-causes are left at `unavailable` on purpose: the simulator's hazard models do
    /// not distinguish, say, a multi-vehicle accident from a heavy-accident, and encoding a
    /// specific sub-cause would be inventing detail the model does not have.
    pub fn to_cause_code(self) -> CauseCodeV2 {
        use crate::asn1::cdd as c;
        let choice = match self {
            DenmCause::TrafficCondition => {
                CauseCodeChoice::trafficCondition1(c::TrafficConditionSubCauseCode(0))
            }
            DenmCause::Accident => CauseCodeChoice::accident2(c::AccidentSubCauseCode(0)),
            DenmCause::Roadworks => CauseCodeChoice::roadworks3(c::RoadworksSubCauseCode(0)),
            DenmCause::Adhesion => CauseCodeChoice::adhesion6(c::AdhesionSubCauseCode(0)),
            DenmCause::ObstacleOnTheRoad => CauseCodeChoice::hazardousLocation_ObstacleOnTheRoad10(
                c::HazardousLocationObstacleOnTheRoadSubCauseCode(0),
            ),
            DenmCause::HumanPresenceOnTheRoad => {
                CauseCodeChoice::humanPresenceOnTheRoad12(c::HumanPresenceOnTheRoadSubCauseCode(0))
            }
            DenmCause::DangerousEndOfQueue => {
                CauseCodeChoice::dangerousEndOfQueue27(c::DangerousEndOfQueueSubCauseCode(0))
            }
            DenmCause::StationaryVehicle => {
                CauseCodeChoice::stationaryVehicle94(c::StationaryVehicleSubCauseCode(0))
            }
            DenmCause::EmergencyVehicleApproaching => {
                CauseCodeChoice::emergencyVehicleApproaching95(
                    c::EmergencyVehicleApproachingSubCauseCode(0),
                )
            }
            DenmCause::DangerousSituation => {
                CauseCodeChoice::dangerousSituation99(c::DangerousSituationSubCauseCode(0))
            }
        };
        CauseCodeV2::new(choice)
    }
}

/// How far from the event position the DENM is relevant (`StandardLength3b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AwarenessDistance {
    /// Less than 50 m.
    LessThan50m,
    /// Less than 100 m.
    LessThan100m,
    /// Less than 200 m.
    LessThan200m,
    /// Less than 500 m.
    LessThan500m,
    /// Less than 1,000 m.
    LessThan1000m,
    /// Less than 5 km.
    LessThan5km,
    /// Less than 10 km.
    LessThan10km,
    /// Over 10 km.
    Over10km,
}

impl AwarenessDistance {
    fn to_cdd(self) -> StandardLength3b {
        match self {
            AwarenessDistance::LessThan50m => StandardLength3b::lessThan50m,
            AwarenessDistance::LessThan100m => StandardLength3b::lessThan100m,
            AwarenessDistance::LessThan200m => StandardLength3b::lessThan200m,
            AwarenessDistance::LessThan500m => StandardLength3b::lessThan500m,
            AwarenessDistance::LessThan1000m => StandardLength3b::lessThan1000m,
            AwarenessDistance::LessThan5km => StandardLength3b::lessThan5km,
            AwarenessDistance::LessThan10km => StandardLength3b::lessThan10km,
            AwarenessDistance::Over10km => StandardLength3b::over10km,
        }
    }
}

/// `actionId`: the originating station plus a sequence number. Unique per event, and
/// carried unchanged through every update and the termination (§8.2.1.5).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct EventId {
    /// The station that first announced the event.
    pub originating_station_id: u32,
    /// `SequenceNumber ::= INTEGER (0..65535)`.
    pub sequence_number: u16,
}

impl EventId {
    fn to_cdd(self) -> ActionId {
        ActionId::new(
            StationId(self.originating_station_id),
            SequenceNumber(self.sequence_number),
        )
    }
}

/// Hands out unused `actionId`s for one station.
///
/// §8.2.1.5 requires an *unused* identifier. The counter is monotonic and wraps at 65,536,
/// which is what the ASN.1 range permits; a station that wrapped would have to have raised
/// 65,536 distinct events, at which point the earliest have long since exceeded their
/// validity and their identifiers are free again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionIdAllocator {
    station_id: u32,
    next: u16,
}

impl ActionIdAllocator {
    /// An allocator for `station_id`, starting at sequence number 0.
    pub const fn new(station_id: u32) -> Self {
        Self {
            station_id,
            next: 0,
        }
    }

    /// The station this allocator belongs to.
    pub const fn station_id(&self) -> u32 {
        self.station_id
    }

    /// The next unused identifier.
    pub fn allocate(&mut self) -> EventId {
        let id = EventId {
            originating_station_id: self.station_id,
            sequence_number: self.next,
        };
        self.next = self.next.wrapping_add(1);
        id
    }
}

/// The application's repetition instruction (§6.1.4.2).
///
/// **Neither field is carried in the DENM.** They are parameters of the `DENM.request`
/// primitive, so a receiver cannot observe them — which is why they live in the sender's
/// state here and nowhere near [`build_denm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Repetition {
    /// `repetitionInterval`: how often to repeat.
    pub interval: Duration,
    /// `repetitionDuration`: for how long, measured from `detectionTime`.
    pub duration: Duration,
}

impl Repetition {
    /// A repetition instruction. Both fields must be non-zero for repetition to happen at
    /// all (§6.1.4.2: "only when the application supplies both").
    pub const fn new(interval: Duration, duration: Duration) -> Self {
        Self { interval, duration }
    }
}

/// Everything [`build_denm`] needs.
#[derive(Debug, Clone, PartialEq)]
pub struct DenmInput {
    /// The event's identifier.
    pub event_id: EventId,
    /// The station sending *this* DENM — the originator for an original or an update, and
    /// a different station for a negation.
    pub sender_station_id: u32,
    /// What kind of station the sender is.
    pub sender_station_type: crate::cam::ParticipantType,
    /// When the event was detected.
    pub detection_time: TimestampIts,
    /// When this particular DENM was generated. Advances on every update (§6.1.2).
    pub reference_time: TimestampIts,
    /// Where the event is, as the sender believes it to be.
    pub event_position: PositionEstimate,
    /// The world's geodetic origin.
    pub origin: GeoOrigin,
    /// What happened.
    pub cause: DenmCause,
    /// Overrides [`DenmCause::to_cause_code`] when a scenario needs a cause this crate does
    /// not name.
    pub cause_code: Option<CauseCodeV2>,
    /// `informationQuality`, 0..7; 0 means "unavailable".
    pub information_quality: u8,
    /// How far the DENM is relevant.
    pub awareness_distance: Option<AwarenessDistance>,
    /// `validityDuration`, seconds from `detectionTime`.
    pub validity: Duration,
    /// `transmissionInterval`, the interval the originator says it is transmitting at. It
    /// **is** carried in the message, unlike the repetition parameters.
    pub transmission_interval: Option<Duration>,
}

impl DenmInput {
    /// A DENM for `cause` at `position`, with the standard's default validity.
    pub fn new(
        event_id: EventId,
        sender_station_id: u32,
        sender_station_type: crate::cam::ParticipantType,
        detection_time: TimestampIts,
        event_position: PositionEstimate,
        origin: GeoOrigin,
        cause: DenmCause,
    ) -> Self {
        Self {
            event_id,
            sender_station_id,
            sender_station_type,
            reference_time: detection_time.clone(),
            detection_time,
            event_position,
            origin,
            cause,
            cause_code: None,
            information_quality: 3,
            awareness_distance: Some(AwarenessDistance::LessThan500m),
            validity: DEFAULT_VALIDITY,
            transmission_interval: None,
        }
    }
}

/// Fills an ordinary (non-terminating) DENM: management, situation and location
/// containers, as the payload's inner subtyping requires.
pub fn build_denm(input: &DenmInput) -> Result<DENM, CodecError> {
    let header = ItsPduHeader::new(
        OrdinalNumber1B(DENM_PROTOCOL_VERSION),
        MessageId(DENM_MESSAGE_ID),
        StationId(input.sender_station_id),
    );

    let management = management_container(input, None)?;
    let situation = SituationContainer::new(
        InformationQuality(input.information_quality.min(7)),
        input
            .cause_code
            .clone()
            .unwrap_or_else(|| input.cause.to_cause_code()),
        None,
        None,
        None,
        None,
    );
    // `detectionZonesToEventPosition` is mandatory and is `SEQUENCE SIZE(1..7) OF Path`.
    // The simulator does not model the trace a detecting vehicle drove, so the one Path is
    // empty — which `Path ::= SEQUENCE (SIZE(0..40)) OF PathPoint` allows, and which is the
    // truthful encoding of "no trace recorded".
    let location = LocationContainer::new(None, None, Traces(vec![Path(Vec::new())]), None, None);

    Ok(DENM::new(
        header,
        DenmPayload::new(management, Some(situation), Some(location), None),
    ))
}

/// Fills a terminating DENM — a cancellation or a negation.
///
/// Carries the management container only, with `termination` present, because the payload's
/// inner subtyping forbids the other three.
pub fn build_termination_denm(
    input: &DenmInput,
    kind: TerminationKind,
) -> Result<DENM, CodecError> {
    let header = ItsPduHeader::new(
        OrdinalNumber1B(DENM_PROTOCOL_VERSION),
        MessageId(DENM_MESSAGE_ID),
        StationId(input.sender_station_id),
    );
    let management = management_container(input, Some(kind.to_cdd()))?;
    Ok(DENM::new(
        header,
        DenmPayload::new(management, None, None, None),
    ))
}

/// Which way a DENM ends (§8.3.2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminationKind {
    /// The originator says the event is over.
    Cancellation,
    /// Another ITS-S says the event is not there.
    Negation,
}

impl TerminationKind {
    fn to_cdd(self) -> Termination {
        match self {
            TerminationKind::Cancellation => Termination::isCancellation,
            TerminationKind::Negation => Termination::isNegation,
        }
    }
}

fn management_container(
    input: &DenmInput,
    termination: Option<Termination>,
) -> Result<ManagementContainer, CodecError> {
    let p = &input.event_position;
    let (lat_deg, lon_deg, alt_m) = input.origin.to_geodetic(p.pos);
    let latitude = units::latitude(lat_deg).ok_or(CodecError::OutOfRange {
        field: "denm.management.eventPosition.latitude",
        asn1_type: "Latitude",
        value: lat_deg as i64,
        min: -90,
        max: 90,
    })?;
    let longitude = units::longitude(lon_deg).ok_or(CodecError::OutOfRange {
        field: "denm.management.eventPosition.longitude",
        asn1_type: "Longitude",
        value: lon_deg as i64,
        min: -180,
        max: 180,
    })?;

    let event_position = ReferencePosition::new(
        Latitude(latitude),
        Longitude(longitude),
        PosConfidenceEllipse::new(
            SemiAxisLength(units::semi_axis_length(p.semi_major_m)),
            SemiAxisLength(units::semi_axis_length(p.semi_minor_m)),
            HeadingValue(units::wgs84_angle(p.orientation_rad)),
        ),
        Altitude::new(
            AltitudeValue(units::altitude(alt_m)),
            AltitudeConfidence::unavailable,
        ),
    );

    let validity_s = u32::try_from(input.validity.as_nanos() / 1_000_000_000)
        .unwrap_or(MAX_VALIDITY_S)
        .min(MAX_VALIDITY_S);

    Ok(ManagementContainer::new(
        input.event_id.to_cdd(),
        input.detection_time.clone(),
        input.reference_time.clone(),
        termination,
        event_position,
        input.awareness_distance.map(|d| d.to_cdd()),
        None,
        DeltaTimeSecond(validity_s),
        input.transmission_interval.map(|d| {
            // `DeltaTimeMilliSecondPositive ::= INTEGER (1..10000)`.
            DeltaTimeMilliSecondPositive((d.as_nanos() / 1_000_000).clamp(1, 10_000) as u16)
        }),
        StationType(TrafficParticipantType(input.sender_station_type.code())),
    ))
}

/// UPER-encodes a DENM.
pub fn encode_denm(denm: &DENM) -> Result<Encoded, CodecError> {
    Ok(Encoded::uper(uper_encode(MsgType::Denm, denm)?))
}

/// UPER-decodes a DENM.
pub fn decode_denm(bytes: &[u8]) -> Result<DENM, CodecError> {
    uper_decode(MsgType::Denm, bytes)
}

/// The keep-alive forwarding delay of EN 302 637-3 Annex B:
/// `T_Forwarding = 2 × transmissionInterval + U(0, 150 ms)`, capped at the remaining
/// validity.
///
/// `u` is a uniform draw in `[0, 1)`. It is a parameter rather than something this function
/// draws itself, so the caller can take it from the node's own
/// [`v2xw_core::rng::RngStream`] on the [`v2xw_core::rng::RngDomain::Backend`] stream and
/// keep the draw where the determinism contract can see it (ADR 0004 §3). A function that
/// reached for a global RNG would be the one place a run stopped being reproducible.
pub fn forwarding_delay(
    transmission_interval: Duration,
    remaining_validity: Duration,
    u: f64,
) -> Duration {
    let jitter_ns = if u.is_finite() && (0.0..1.0).contains(&u) {
        (u * FORWARDING_JITTER.as_nanos() as f64) as u64
    } else {
        0
    };
    let base = transmission_interval
        .saturating_mul(2)
        .as_nanos()
        .saturating_add(jitter_ns);
    Duration::from_nanos(base).min(remaining_validity)
}

/// What [`DenmService::poll`] wants transmitted now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenmAction {
    /// The first transmission of a newly created event.
    Original(EventId),
    /// A repetition of an event that is still within its repetition duration and validity.
    Repetition(EventId),
    /// An update the application asked for; the `referenceTime` has already advanced.
    Update(EventId),
    /// The event's termination, which is transmitted at least once (§8.3.2.5).
    Termination(EventId, TerminationKind),
}

impl DenmAction {
    /// Which event this is about.
    pub const fn event(&self) -> EventId {
        match self {
            DenmAction::Original(id)
            | DenmAction::Repetition(id)
            | DenmAction::Update(id)
            | DenmAction::Termination(id, _) => *id,
        }
    }
}

/// One event the originator is keeping alive.
#[derive(Debug, Clone, PartialEq)]
struct ActiveEvent {
    id: EventId,
    detection_time: SimTime,
    validity: Duration,
    repetition: Option<Repetition>,
    /// When the next transmission is due, if any.
    next_tx: Option<SimTime>,
    /// Set once the application cancels or another station negates; transmitted once and
    /// then the event is dropped.
    pending_termination: Option<TerminationKind>,
    /// Set when the application updates the event; the next poll reports an update.
    pending_update: bool,
    /// Whether the original has gone out yet.
    sent_original: bool,
}

/// The originator-side DENM lifecycle: create, update, repeat, terminate.
///
/// One per station. It holds no message: it decides *which event* should be transmitted
/// *when*, and the node runtime builds the DENM for it with [`build_denm`] or
/// [`build_termination_denm`]. Keeping the timetable and the encoder apart is what lets the
/// repetition rules be tested without an ASN.1 encoder, exactly as with the CAM triggers.
#[derive(Debug, Clone, PartialEq)]
pub struct DenmService {
    allocator: ActionIdAllocator,
    events: Vec<ActiveEvent>,
}

impl DenmService {
    /// A service for `station_id`.
    pub fn new(station_id: u32) -> Self {
        Self {
            allocator: ActionIdAllocator::new(station_id),
            events: Vec::new(),
        }
    }

    /// The station this service speaks for.
    pub const fn station_id(&self) -> u32 {
        self.allocator.station_id()
    }

    /// How many events are still alive.
    pub fn active_len(&self) -> usize {
        self.events.len()
    }

    /// Whether `id` is still alive here.
    pub fn is_active(&self, id: EventId) -> bool {
        self.events.iter().any(|e| e.id == id)
    }

    /// Announces a new event, returning its freshly allocated identifier.
    ///
    /// `repetition` is the application's instruction; `None` means the DENM is transmitted
    /// once and then only kept for updates and termination, which is what §6.1.4.2 says
    /// happens when the application supplies no repetition parameters.
    pub fn create(
        &mut self,
        now: SimTime,
        validity: Duration,
        repetition: Option<Repetition>,
    ) -> EventId {
        let id = self.allocator.allocate();
        self.events.push(ActiveEvent {
            id,
            detection_time: now,
            validity,
            // A repetition instruction with a zero interval or a zero duration is not a
            // repetition instruction (§6.1.4.2 needs both).
            repetition: repetition.filter(|r| !r.interval.is_zero() && !r.duration.is_zero()),
            next_tx: Some(now),
            pending_termination: None,
            pending_update: false,
            sent_original: false,
        });
        id
    }

    /// Marks `id` as updated, so the next [`DenmService::poll`] reports an update.
    ///
    /// The caller advances `referenceTime` when it builds the message — §6.1.2 says the
    /// update increments it, and the value is a [`TimestampIts`] the node reads from its own
    /// clock, not something this timetable can invent.
    pub fn update(&mut self, id: EventId, now: SimTime) -> bool {
        let Some(event) = self.events.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        event.pending_update = true;
        event.next_tx = Some(now);
        true
    }

    /// Cancels `id` as its originator. The termination goes out on the next poll.
    pub fn cancel(&mut self, id: EventId, now: SimTime) -> bool {
        self.terminate(id, now, TerminationKind::Cancellation)
    }

    /// Negates `id`: this station is not the originator and says the event is not there.
    pub fn negate(&mut self, id: EventId, now: SimTime) -> bool {
        self.terminate(id, now, TerminationKind::Negation)
    }

    fn terminate(&mut self, id: EventId, now: SimTime, kind: TerminationKind) -> bool {
        let Some(event) = self.events.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        event.pending_termination = Some(kind);
        event.next_tx = Some(now);
        true
    }

    /// Everything due for transmission at `now`, in event-identifier order.
    ///
    /// The order is fixed rather than insertion order because two nodes that raised the
    /// same events in a different order must still transmit them in the same order, or the
    /// run's event log depends on the arrival order of hazards (02-architecture.md §6.4).
    ///
    /// Events past their validity are dropped here, which is also where a repetition stops:
    /// §6.1.4.2 bounds `T_Repetition` by `validityDuration`, so an event whose repetition
    /// duration outlives its validity still stops at the validity.
    pub fn poll(&mut self, now: SimTime) -> Vec<DenmAction> {
        let mut actions = Vec::new();
        let mut keep = Vec::with_capacity(self.events.len());

        // Sorting by identifier makes the output independent of the order events were
        // created in. `EventId` is `Ord`, so this is a total order.
        self.events.sort_by_key(|e| e.id);

        for mut event in std::mem::take(&mut self.events) {
            let expires_at = event.validity.after(event.detection_time);

            if let Some(kind) = event.pending_termination
                && event.next_tx.is_some_and(|t| t <= now)
            {
                // §8.3.2.5: transmitted at least once, and then the event is gone.
                actions.push(DenmAction::Termination(event.id, kind));
                continue;
            }

            if now >= expires_at {
                // Expired without a termination: it simply stops (§6.1.4.2).
                continue;
            }

            if event.next_tx.is_some_and(|t| t <= now) {
                let action = if !event.sent_original {
                    event.sent_original = true;
                    DenmAction::Original(event.id)
                } else if event.pending_update {
                    event.pending_update = false;
                    DenmAction::Update(event.id)
                } else {
                    DenmAction::Repetition(event.id)
                };
                actions.push(action);

                event.next_tx = event.repetition.and_then(|r| {
                    let next = r.interval.after(now);
                    let repetition_ends = r.duration.after(event.detection_time);
                    (next < repetition_ends && next < expires_at).then_some(next)
                });
            }

            keep.push(event);
        }

        self.events = keep;
        actions
    }

    /// The earliest instant at which [`DenmService::poll`] could produce anything.
    ///
    /// The node runtime schedules its next DENM timer here rather than polling blindly at a
    /// fixed rate.
    pub fn next_deadline(&self) -> Option<SimTime> {
        self.events.iter().filter_map(|e| e.next_tx).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::ParticipantType;
    use crate::codec::{EtsiUperCodec, Message, MessageCodec, SizeSource};
    use v2xw_core::geom::Vec3;
    use v2xw_core::time::NS_PER_MS;

    fn ms(n: u64) -> SimTime {
        n * NS_PER_MS
    }

    fn origin() -> GeoOrigin {
        GeoOrigin::new(40.7440, -73.9900, 0.0)
    }

    fn position() -> PositionEstimate {
        PositionEstimate {
            pos: Vec3::new(900.0, 1_100.0, 8.0),
            vel: Vec3::ZERO,
            heading_rad: 0.0,
            semi_major_m: 2.5,
            semi_minor_m: 1.5,
            orientation_rad: 0.3,
            time_ns: 0,
            fix: v2xw_core::belief::FixQuality::ThreeD,
        }
    }

    fn input() -> DenmInput {
        DenmInput::new(
            EventId {
                originating_station_id: 0x0102_0304,
                sequence_number: 7,
            },
            0x0102_0304,
            ParticipantType::PassengerCar,
            TimestampIts(720_000_000_000),
            position(),
            origin(),
            DenmCause::DangerousSituation,
        )
    }

    #[test]
    fn round_trip_is_exact() {
        let denm = build_denm(&input()).expect("builds");
        let encoded = encode_denm(&denm).expect("encodes");
        assert_eq!(encoded.size_source, SizeSource::Uper);
        assert_eq!(decode_denm(&encoded.bytes).expect("decodes"), denm);
    }

    #[test]
    fn the_header_carries_the_values_the_asn1_constrains_it_to() {
        let denm = build_denm(&input()).expect("builds");
        assert_eq!(denm.header.protocol_version.0, 2);
        assert_eq!(denm.header.message_id.0, 1);
    }

    /// The payload's inner-subtyping constraint, enforced here because the generated types
    /// cannot express it.
    #[test]
    fn an_ordinary_denm_carries_a_situation_and_a_location() {
        let denm = build_denm(&input()).expect("builds");
        assert!(denm.denm.management.termination.is_none());
        assert!(denm.denm.situation.is_some());
        assert!(denm.denm.location.is_some());
    }

    #[test]
    fn a_termination_denm_carries_no_situation_or_location() {
        for kind in [TerminationKind::Cancellation, TerminationKind::Negation] {
            let denm = build_termination_denm(&input(), kind).expect("builds");
            assert!(denm.denm.management.termination.is_some(), "{kind:?}");
            assert!(denm.denm.situation.is_none(), "{kind:?}");
            assert!(denm.denm.location.is_none(), "{kind:?}");
            assert!(denm.denm.alacarte.is_none(), "{kind:?}");
            let bytes = encode_denm(&denm).expect("encodes").bytes;
            assert_eq!(decode_denm(&bytes).expect("decodes"), denm);
        }
    }

    #[test]
    fn the_default_validity_is_the_six_hundred_seconds_the_asn1_declares() {
        assert_eq!(DEFAULT_VALIDITY, Duration::from_secs(600));
        let denm = build_denm(&input()).expect("builds");
        assert_eq!(denm.denm.management.validity_duration.0, 600);
    }

    #[test]
    fn a_validity_beyond_the_asn1_range_saturates() {
        let mut i = input();
        i.validity = Duration::from_secs(1_000_000);
        let denm = build_denm(&i).expect("builds");
        assert_eq!(denm.denm.management.validity_duration.0, MAX_VALIDITY_S);
    }

    #[test]
    fn every_named_cause_encodes_and_decodes() {
        for cause in [
            DenmCause::TrafficCondition,
            DenmCause::Accident,
            DenmCause::Roadworks,
            DenmCause::Adhesion,
            DenmCause::ObstacleOnTheRoad,
            DenmCause::HumanPresenceOnTheRoad,
            DenmCause::DangerousEndOfQueue,
            DenmCause::StationaryVehicle,
            DenmCause::EmergencyVehicleApproaching,
            DenmCause::DangerousSituation,
        ] {
            let mut i = input();
            i.cause = cause;
            let denm = build_denm(&i).unwrap_or_else(|e| panic!("{cause:?}: {e}"));
            let bytes = encode_denm(&denm).unwrap().bytes;
            assert_eq!(decode_denm(&bytes).unwrap(), denm, "{cause:?}");
        }
    }

    #[test]
    fn action_ids_are_unique_and_wrap_at_the_asn1_range() {
        let mut a = ActionIdAllocator::new(42);
        let first = a.allocate();
        let second = a.allocate();
        assert_eq!(first.originating_station_id, 42);
        assert_ne!(first, second);
        assert_eq!(second.sequence_number, 1);

        let mut a = ActionIdAllocator::new(1);
        for _ in 0..65_536 {
            a.allocate();
        }
        assert_eq!(a.allocate().sequence_number, 0, "wraps at 65 536");
    }

    // --- lifecycle -----------------------------------------------------------------

    #[test]
    fn without_repetition_parameters_a_denm_is_transmitted_once() {
        let mut s = DenmService::new(1);
        let id = s.create(0, DEFAULT_VALIDITY, None);
        assert_eq!(s.poll(0), vec![DenmAction::Original(id)]);
        for t in [1, 100, 5_000, 100_000] {
            assert!(s.poll(ms(t)).is_empty(), "nothing more at {t} ms");
        }
        assert!(
            s.is_active(id),
            "it is still alive for updates and cancellation"
        );
    }

    #[test]
    fn repetition_runs_at_the_interval_until_the_duration_ends() {
        let mut s = DenmService::new(1);
        let id = s.create(
            0,
            DEFAULT_VALIDITY,
            Some(Repetition::new(
                Duration::from_millis(500),
                Duration::from_secs(2),
            )),
        );
        let mut seen = Vec::new();
        for step in 0..=10u64 {
            for action in s.poll(ms(step * 250)) {
                seen.push((step * 250, action));
            }
        }
        assert_eq!(
            seen,
            vec![
                (0, DenmAction::Original(id)),
                (500, DenmAction::Repetition(id)),
                (1_000, DenmAction::Repetition(id)),
                (1_500, DenmAction::Repetition(id)),
            ],
            "the last repetition before repetitionDuration (2 s) is at 1,5 s"
        );
    }

    /// §6.1.4.2 bounds `T_Repetition` by `validityDuration`: a repetition duration longer
    /// than the validity does not extend the event.
    #[test]
    fn validity_bounds_the_repetition_even_when_the_duration_is_longer() {
        let mut s = DenmService::new(1);
        let id = s.create(
            0,
            Duration::from_secs(1),
            Some(Repetition::new(
                Duration::from_millis(400),
                Duration::from_secs(60),
            )),
        );
        let mut seen = Vec::new();
        for step in 0..=10u64 {
            seen.extend(s.poll(ms(step * 200)).into_iter().map(|a| (step * 200, a)));
        }
        assert_eq!(
            seen,
            vec![
                (0, DenmAction::Original(id)),
                (400, DenmAction::Repetition(id)),
                (800, DenmAction::Repetition(id)),
            ],
            "the 1 s validity stops it, not the 60 s repetition duration"
        );
        assert!(!s.is_active(id), "and the event is dropped once it expires");
    }

    #[test]
    fn an_update_is_reported_once_and_keeps_the_action_id() {
        let mut s = DenmService::new(1);
        let id = s.create(
            0,
            DEFAULT_VALIDITY,
            Some(Repetition::new(
                Duration::from_secs(1),
                Duration::from_secs(10),
            )),
        );
        assert_eq!(s.poll(0), vec![DenmAction::Original(id)]);
        assert!(s.update(id, ms(300)));
        assert_eq!(s.poll(ms(300)), vec![DenmAction::Update(id)]);
        assert_eq!(s.poll(ms(1_300)), vec![DenmAction::Repetition(id)]);
        assert!(!s.update(
            EventId {
                originating_station_id: 1,
                sequence_number: 99
            },
            ms(400)
        ));
    }

    #[test]
    fn a_cancellation_goes_out_once_and_ends_the_event() {
        let mut s = DenmService::new(1);
        let id = s.create(0, DEFAULT_VALIDITY, None);
        s.poll(0);
        assert!(s.cancel(id, ms(1_000)));
        assert_eq!(
            s.poll(ms(1_000)),
            vec![DenmAction::Termination(id, TerminationKind::Cancellation)]
        );
        assert!(!s.is_active(id));
        assert!(s.poll(ms(2_000)).is_empty());
    }

    #[test]
    fn a_negation_goes_out_once_and_ends_the_event() {
        let mut s = DenmService::new(1);
        let id = s.create(0, DEFAULT_VALIDITY, None);
        s.poll(0);
        assert!(s.negate(id, ms(500)));
        assert_eq!(
            s.poll(ms(500)),
            vec![DenmAction::Termination(id, TerminationKind::Negation)]
        );
        assert!(!s.is_active(id));
    }

    /// The output order must be a function of the identifiers, not of the order the
    /// hazards happened to arrive (02-architecture.md §6.4).
    #[test]
    fn poll_output_is_ordered_by_action_id() {
        let mut s = DenmService::new(1);
        let a = s.create(0, DEFAULT_VALIDITY, None);
        let b = s.create(0, DEFAULT_VALIDITY, None);
        let c = s.create(0, DEFAULT_VALIDITY, None);
        let actions = s.poll(0);
        assert_eq!(
            actions,
            vec![
                DenmAction::Original(a),
                DenmAction::Original(b),
                DenmAction::Original(c)
            ]
        );
        assert!(a < b && b < c);
    }

    #[test]
    fn the_next_deadline_is_the_earliest_pending_transmission() {
        let mut s = DenmService::new(1);
        s.create(
            0,
            DEFAULT_VALIDITY,
            Some(Repetition::new(
                Duration::from_secs(2),
                Duration::from_secs(60),
            )),
        );
        s.create(
            0,
            DEFAULT_VALIDITY,
            Some(Repetition::new(
                Duration::from_millis(500),
                Duration::from_secs(60),
            )),
        );
        assert_eq!(s.next_deadline(), Some(0));
        s.poll(0);
        assert_eq!(s.next_deadline(), Some(ms(500)));
    }

    #[test]
    fn forwarding_delay_is_twice_the_interval_plus_bounded_jitter() {
        let interval = Duration::from_millis(500);
        let validity = Duration::from_secs(600);
        assert_eq!(
            forwarding_delay(interval, validity, 0.0),
            Duration::from_millis(1_000)
        );
        let jittered = forwarding_delay(interval, validity, 0.5);
        assert!(
            jittered > Duration::from_millis(1_000)
                && jittered < Duration::from_millis(1_000 + 150),
            "{jittered:?}"
        );
        // Capped at the remaining validity.
        assert_eq!(
            forwarding_delay(interval, Duration::from_millis(200), 0.9),
            Duration::from_millis(200)
        );
        // A draw outside [0, 1) contributes no jitter rather than a wild delay.
        assert_eq!(
            forwarding_delay(interval, validity, f64::NAN),
            Duration::from_millis(1_000)
        );
    }

    #[test]
    fn the_codec_seam_handles_a_denm() {
        let denm = build_denm(&input()).unwrap();
        let codec = EtsiUperCodec::new();
        let encoded = codec
            .encode(&Message::Denm(Box::new(denm.clone())))
            .unwrap();
        assert_eq!(encoded, encode_denm(&denm).unwrap());
        let Message::Denm(back) = codec.decode(&encoded.bytes, MsgType::Denm).unwrap() else {
            panic!("a DENM decodes to a DENM");
        };
        assert_eq!(*back, denm);
    }

    #[test]
    fn encoded_size_and_bytes_are_stable() {
        let first = encode_denm(&build_denm(&input()).unwrap()).unwrap();
        for _ in 0..16 {
            assert_eq!(encode_denm(&build_denm(&input()).unwrap()).unwrap(), first);
        }
    }

    /// 04-models.md §8.2 records "DENM typical: about 300 (100-800+)" and marks it
    /// UNVERIFIED (an unreviewed source). This records what *we* actually encode, secured,
    /// so the number in the report is reproducible and the comparison is explicit about
    /// which side is weak.
    #[test]
    fn typical_denm_size_is_recorded_against_the_unverified_anchor() {
        let payload = encode_denm(&build_denm(&input()).unwrap()).unwrap().size;
        // TS 103 097 §7.1.2: a DENM's signer is always a certificate, and
        // `generationLocation` is present (04-models.md §9.1: +10 B).
        const ENVELOPE_CERT_AND_LOCATION: u32 = 87 + 90 + 10;
        const LOWER_LAYERS: u32 = 52; // GN SHB + BTP-B + LLC/SNAP, §9.3
        let secured = payload + ENVELOPE_CERT_AND_LOCATION + LOWER_LAYERS;
        assert!(
            (100..=800).contains(&secured),
            "payload {payload} B secured is {secured} B, outside the 100-800 B range the \
             (UNVERIFIED) anchor gives"
        );
    }
}
