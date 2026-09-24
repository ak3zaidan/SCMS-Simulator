//! The event-driven and infrastructure message services: DENM, SPaT, MAP, SRM and SSM.
//!
//! The awareness messages (CAM, BSM) are periodic or dynamics-triggered and live in
//! [`crate::generate`]. The messages here are sent because something happened or because a
//! roadside unit's controller said so, and each one's trigger is stated below with where it
//! comes from and how sure it is.
//!
//! | Message | Who sends it | Trigger | Source |
//! |---|---|---|---|
//! | DENM `dangerousSituation(99)` | a vehicle running `denm` | its own longitudinal deceleration reaches 0.4 g (3.92 m/s²) | the J2945/1 hard-braking threshold 04-models.md §8.1 and §11 mark VERIFIED ([`crate::safety::EEBL_DECEL_THRESHOLD_MPS2`]); the DEN basic service (EN 302 637-3) leaves triggering to the application |
//! | SPaT / SPATEM | a roadside unit running `spat` | every [`crate::generate::SPAT_INTERVAL`] | CTI 4501 via 04-models.md §8.1 |
//! | MAP / MAPEM | a roadside unit running `map` | every [`crate::generate::MAP_INTERVAL`] | likewise |
//! | SRM / SREM | a vehicle running `srm` (the wiring gives it to emergency vehicles) | a junction whose MAP it heard is within [`SRM_RANGE_M`] ahead; repeated at [`SRM_INTERVAL`] | J2735's signal request; the range and interval are this build's choice |
//! | SSM / SSEM | a roadside unit running `ssm` | a signal request it received; at most one per [`SSM_MIN_INTERVAL`] | J2735's signal status |
//!
//! # What a unit is told, and what it decides
//!
//! A roadside unit's SPaT and MAP payloads are installed by its controller feed
//! ([`EventServices::set_infra_payload`]): the unit signs and sends what the traffic signal
//! controller and the operator's survey gave it, which is what a deployed RSU does. A
//! vehicle's own deceleration arrives the same way ([`EventServices::set_own_acceleration`])
//! — it is the vehicle's own accelerometer, not a view of anyone else.
//!
//! # What is not triggered, and why
//!
//! * `stationaryVehicle(94)`. Its triggering condition needs a breakdown or a hazard-light
//!   input, and no mobility model here has one: the only stationary vehicles are queued at
//!   a red or behind one, and a DENM from each of them would be a false hazard report.
//! * The DENM's repetition (every 100 ms for 2 s) and validity (2 s) are this build's
//!   choice. The C2C-CC triggering-condition documents that set them for the emergency
//!   brake light are not in this repository.
//! * SRM and SSM have no real encoder (build decision D2): their payloads are the validated
//!   size model's placeholder of the modelled length (`codec/size-model/j2735`), which is
//!   why `messages.codec_tier` must be `size-model` to select them. A unit acknowledges a
//!   request; nothing here grants priority, because no signal controller in this build
//!   changes its plan for one.

use std::collections::BTreeMap;

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geom::Vec3;
use v2xw_core::time::{Duration, SimTime};
use v2xw_msg::MsgType;
use v2xw_msg::codec::{Message, MessageCodec};
use v2xw_msg::denm::{self, DenmAction, DenmCause, DenmInput, DenmService, EventId, Repetition};
use v2xw_msg::size_model::{ContentProfile, J2735SizeCodec, SizeRequest};

use crate::generate::ServiceSet;
use crate::runtime::{NodeConfig, VerifiedMessage};
use crate::stores::{CredentialHandle, VerificationState};

/// How far ahead a junction whose MAP a vehicle heard may be for it to request priority,
/// metres. This build's choice: about twelve seconds at an emergency vehicle's urban speed.
pub const SRM_RANGE_M: f64 = 300.0;

/// How often a vehicle repeats its signal request while it approaches. This build's choice.
pub const SRM_INTERVAL: Duration = Duration::from_secs(1);

/// The closest together two signal statuses from one unit may be. This build's choice.
pub const SSM_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// How long a heard MAP stays usable for a priority request without being heard again.
pub const MAP_MEMORY: Duration = Duration::from_secs(5);

/// The DENM repetition interval and duration for a hard-braking event (this build's
/// choice, see the module notes), and its validity.
pub const DENM_REPETITION: Duration = Duration::from_millis(100);
/// See [`DENM_REPETITION`].
pub const DENM_VALIDITY: Duration = Duration::from_secs(2);

/// ETSI `MessageId` of a SREM, `srem(9)` (ETSI TS 102 894-2).
pub const SREM_MESSAGE_ID: u8 = 9;
/// ETSI `MessageId` of a SSEM, `ssem(10)` (ETSI TS 102 894-2).
pub const SSEM_MESSAGE_ID: u8 = 10;

/// One event message due now: what to build.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventRequest {
    /// A DENM for this lifecycle action.
    Denm(DenmAction),
    /// A signal request toward the junction whose reference point this is.
    Srm(Vec3),
    /// A signal status answering this many requests.
    Ssm(u32),
}

impl EventRequest {
    /// The message type it becomes.
    pub const fn msg_type(&self) -> MsgType {
        match self {
            EventRequest::Denm(_) => MsgType::Denm,
            EventRequest::Srm(_) => MsgType::Srm,
            EventRequest::Ssm(_) => MsgType::Ssm,
        }
    }
}

/// The node's event and infrastructure services' state.
#[derive(Debug, Clone, Default)]
pub struct EventServices {
    /// The SPaT and MAP payloads the unit's controller feed installed.
    spat: Option<Vec<u8>>,
    map: Option<Vec<u8>>,
    /// The vehicle's own longitudinal acceleration, m/s², from its own accelerometer.
    own_accel: Option<f64>,
    /// Whether the vehicle was already braking past the threshold at the last step, so one
    /// braking episode raises one event.
    braking: bool,
    /// The DEN basic service, created for the station id the first event is raised under.
    denm: Option<DenmService>,
    /// The detection instant and position of each live event, for building its DENM.
    denm_events: BTreeMap<EventId, (SimTime, PositionEstimate)>,
    /// Junctions whose MAP this vehicle heard: reference point → when last heard.
    maps_heard: BTreeMap<(i64, i64), (Vec3, SimTime)>,
    last_srm: Option<SimTime>,
    /// Signal requests received and not yet answered, and when the last status went.
    requests_pending: u32,
    last_ssm: Option<SimTime>,
    /// DENMs raised, for a test and a report.
    denm_raised: u64,
}

impl EventServices {
    /// Installs the payload the unit's controller feed produced for `ty` (SPaT or MAP).
    /// Any other type is ignored.
    pub fn set_infra_payload(&mut self, ty: MsgType, bytes: Vec<u8>) {
        match ty {
            MsgType::Spat => self.spat = Some(bytes),
            MsgType::Map => self.map = Some(bytes),
            _ => {}
        }
    }

    /// The installed payload for `ty`, when there is one.
    pub fn infra_payload(&self, ty: MsgType) -> Option<&[u8]> {
        match ty {
            MsgType::Spat => self.spat.as_deref(),
            MsgType::Map => self.map.as_deref(),
            _ => None,
        }
    }

    /// The vehicle's own longitudinal acceleration, m/s² (negative when braking).
    pub fn set_own_acceleration(&mut self, a_mps2: f64) {
        self.own_accel = Some(a_mps2);
    }

    /// How many DENM events this node has raised.
    pub fn denm_raised(&self) -> u64 {
        self.denm_raised
    }

    /// Learns from a message the applications received: a MAP is a junction a priority
    /// request can be addressed to, and a signal request is one to answer.
    pub fn on_delivered(&mut self, m: &VerifiedMessage) {
        if matches!(
            m.verification,
            VerificationState::Invalid | VerificationState::Revoked
        ) {
            return;
        }
        match m.msg_type {
            MsgType::Map => {
                if let Some(p) = m.claimed_pos {
                    let key = (p.x.round() as i64, p.y.round() as i64);
                    self.maps_heard.insert(key, (p, m.received_at));
                }
            }
            MsgType::Srm => self.requests_pending = self.requests_pending.saturating_add(1),
            _ => {}
        }
    }

    /// Every event message due at `now` on this node's clock. `station_id` is the identifier
    /// the node is transmitting under now, the one a new event's `actionId` carries.
    pub fn due(
        &mut self,
        now: SimTime,
        belief: &PositionEstimate,
        services: ServiceSet,
        station_id: Option<u32>,
    ) -> Vec<EventRequest> {
        let mut out = Vec::new();
        if services.denm {
            // A new pseudonym is a new station id for new events; an event already raised
            // keeps its `actionId` to the end (EN 302 637-3 §8.2.1.5), so the service is
            // replaced only when it has nothing alive.
            if let Some(sid) = station_id
                && self
                    .denm
                    .as_ref()
                    .is_none_or(|s| s.station_id() != sid && s.active_len() == 0)
            {
                self.denm = Some(DenmService::new(sid));
            }
            self.raise_on_hard_braking(now, belief);
            if let Some(service) = self.denm.as_mut() {
                for action in service.poll(now) {
                    out.push(EventRequest::Denm(action));
                }
                // Forget the events the service has dropped.
                let service = &*service;
                self.denm_events.retain(|id, _| service.is_active(*id));
            }
        }
        if services.srm
            && let Some(target) = self.priority_target(now, belief)
            && self
                .last_srm
                .is_none_or(|t| now.saturating_sub(t) >= SRM_INTERVAL.as_nanos())
        {
            self.last_srm = Some(now);
            out.push(EventRequest::Srm(target));
        }
        if services.ssm
            && self.requests_pending > 0
            && self
                .last_ssm
                .is_none_or(|t| now.saturating_sub(t) >= SSM_MIN_INTERVAL.as_nanos())
        {
            self.last_ssm = Some(now);
            out.push(EventRequest::Ssm(core::mem::take(
                &mut self.requests_pending,
            )));
        }
        out
    }

    /// Raises a `dangerousSituation` event on the rising edge of hard braking.
    fn raise_on_hard_braking(&mut self, now: SimTime, belief: &PositionEstimate) {
        let Some(a) = self.own_accel else {
            return;
        };
        let hard = a <= -crate::safety::EEBL_DECEL_THRESHOLD_MPS2;
        if hard
            && !self.braking
            && belief.fix.has_position()
            && let Some(service) = self.denm.as_mut()
        {
            let id = service.create(
                now,
                DENM_VALIDITY,
                Some(Repetition::new(DENM_REPETITION, DENM_VALIDITY)),
            );
            self.denm_events.insert(id, (now, *belief));
            self.denm_raised += 1;
        }
        self.braking = hard;
    }

    /// The nearest junction ahead whose MAP was heard recently, within [`SRM_RANGE_M`].
    fn priority_target(&mut self, now: SimTime, belief: &PositionEstimate) -> Option<Vec3> {
        self.maps_heard
            .retain(|_, (_, heard)| now.saturating_sub(*heard) <= MAP_MEMORY.as_nanos());
        if !belief.fix.has_position() {
            return None;
        }
        let (hx, hy) = (
            v2xw_core::math::cos(belief.heading_rad),
            v2xw_core::math::sin(belief.heading_rad),
        );
        let mut best: Option<(f64, Vec3)> = None;
        for (p, _) in self.maps_heard.values() {
            let (dx, dy) = (p.x - belief.pos.x, p.y - belief.pos.y);
            let d2 = dx * dx + dy * dy;
            let ahead = dx * hx + dy * hy > 0.0;
            if ahead && d2 <= SRM_RANGE_M * SRM_RANGE_M && best.is_none_or(|(b, _)| d2 < b) {
                best = Some((d2, *p));
            }
        }
        best.map(|(_, p)| p)
    }

    /// The payload for one event request, or `None` when it cannot be built.
    ///
    /// The station id is the first four octets of the active pseudonym's digest, as for a
    /// CAM, so the identifier on the air changes with the pseudonym.
    pub fn encode(
        &self,
        request: &EventRequest,
        now: SimTime,
        cred: &CredentialHandle,
        config: &NodeConfig,
    ) -> Option<Vec<u8>> {
        let mut id = [0u8; 4];
        id.copy_from_slice(&cred.digest.0[..4]);
        let station_id = u32::from_be_bytes(id);
        match request {
            EventRequest::Denm(action) => {
                let event = action.event();
                let (detected_at, position) = self.denm_events.get(&event).copied()?;
                let mut input = DenmInput::new(
                    event,
                    station_id,
                    config.station_type,
                    v2xw_msg::cam::timestamp_its(config.wall, detected_at).ok()?,
                    position,
                    config.origin,
                    DenmCause::DangerousSituation,
                );
                input.reference_time = v2xw_msg::cam::timestamp_its(config.wall, now).ok()?;
                input.validity = DENM_VALIDITY;
                input.transmission_interval = Some(DENM_REPETITION);
                input.awareness_distance = Some(denm::AwarenessDistance::LessThan200m);
                let message = match action {
                    DenmAction::Termination(_, kind) => denm::build_termination_denm(&input, *kind),
                    _ => denm::build_denm(&input),
                }
                .ok()?;
                Some(denm::encode_denm(&message).ok()?.bytes)
            }
            EventRequest::Srm(_) => sized(MsgType::Srm, 1, config, station_id),
            EventRequest::Ssm(n) => sized(MsgType::Ssm, (*n).clamp(1, 8), config, station_id),
        }
    }
}

/// A size-model payload: the validated modelled length of a J2735 SRM or SSM, with the ETSI
/// `ItsPduHeader` in front on the ETSI stack.
fn sized(ty: MsgType, elements: u32, config: &NodeConfig, station_id: u32) -> Option<Vec<u8>> {
    let encoded = J2735SizeCodec::new()
        .encode(&Message::Modeled(SizeRequest {
            ty,
            profile: ContentProfile::Typical,
            elements,
        }))
        .ok()?;
    if !config.etsi_facilities {
        return Some(encoded.bytes);
    }
    let message_id = if ty == MsgType::Srm {
        SREM_MESSAGE_ID
    } else {
        SSEM_MESSAGE_ID
    };
    v2xw_msg::j2735::infra::its_wrap(ty, message_id, station_id, &encoded.bytes).ok()
}
