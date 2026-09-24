//! The belief-side inputs every attacker and detector in this crate consumes — and the
//! reason none of them can reach ground truth.
//!
//! # The seam
//!
//! 03-interfaces.md §9 spells a detector's arguments `VerifiedMessage`, `NeighborTable`
//! and `Detection`, which live in `v2xw-node`, `v2xw-net` and the perception crate. This
//! crate deliberately does not name those types. It defines the *belief* shapes it needs —
//! [`ObservedMessage`], [`PeerBelief`], [`SelfBelief`] — with exactly the fields
//! `v2xw_node::runtime::VerifiedMessage` and `v2xw_node::stores::Neighbor` carry, so the
//! node runtime supplies them through a thin `From` adapter that lives on the engine side
//! of the seam (see the crate documentation).
//!
//! Two reasons, and the second is the important one:
//!
//! 1. `v2xw-node` and `v2xw-engine` are being built concurrently with this crate, so
//!    naming their types would couple the threat model to their churn.
//! 2. **Every field here is something a receiver could have observed.** There is no
//!    [`v2xw_core::ids::ActorId`], no true position, no `is_attacker` flag and no
//!    simulator clock anywhere in this module. Invariant I-T2 is therefore a property of
//!    the *argument types* of [`crate::detect::Detector::on_message`] rather than a rule
//!    somebody has to remember: a detector that wanted ground truth could not ask for it
//!    and would not compile. The same holds for [`crate::attack::Attacker`] under I-T1.
//!
//! # Where the map comes from
//!
//! One detector — `mapOffRoad` — needs a map. A map is not ground truth: 07-threats §1
//! lists `map` as a *declared capability* and 06-node-models gives a node its own map
//! store, so a receiver legitimately has one. [`LocalEnvironment`] is that store seen from
//! the detector, and [`NoMap`] is the honest answer for a node without one: the detector
//! then scores zero forever rather than silently borrowing the world.

use v2xw_core::ids::NodeId;
use v2xw_core::time::SimTime;

/// The station type a beacon **declares** about itself.
///
/// Self-declared, not observed: a moving vehicle is free to put `Vru` here, which is
/// exactly the `VruImpersonation` attack, and the reason the detector suite needs the
/// two-armed impersonation check rather than trusting the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum StationType {
    /// A vehicle (CAM/BSM).
    #[default]
    Vehicle,
    /// A vulnerable road user: pedestrian or cyclist (VAM).
    Vru,
    /// Road-side infrastructure.
    Rsu,
}

impl StationType {
    /// The wire spelling, as the legacy engine's `station_type` field carried it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            StationType::Vehicle => "vehicle",
            StationType::Vru => "vru",
            StationType::Rsu => "rsu",
        }
    }
}

/// What a receiver concluded about a message's signature and certificate chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum VerificationState {
    /// Not checked yet (a verify-on-demand policy deferred it).
    #[default]
    Unverified,
    /// Signature verified against a certificate this node accepts.
    Valid,
    /// The signature did not verify.
    BadSignature,
    /// The signer's certificate is unknown to this node.
    UnknownCertificate,
}

impl VerificationState {
    /// True when the receiver has a verified signature, which is the precondition for
    /// trusting any *content* check at all (the legacy suite zeroes every plausibility
    /// detector on a bad signature and reports only the crypto failure).
    #[must_use]
    pub const fn is_valid(self) -> bool {
        matches!(self, VerificationState::Valid)
    }
}

/// Which kind of message a receiver heard.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ObservedKind {
    /// A periodic cooperative-awareness beacon (CAM or BSM).
    #[default]
    Beacon,
    /// A vulnerable-road-user awareness message.
    Vam,
    /// A decentralised environmental-notification message, with the event it announces,
    /// spelled as ETSI TS 102 894-2 `CauseCode` names it — e.g.
    /// `emergencyElectronicBrakeLight`, `stationaryVehicle`.
    Denm(String),
    /// A collective-perception message, with the objects it claims to perceive.
    ///
    /// The objects are what the *sender says* it sees, which is why a phantom object is
    /// an attack ([`crate::attack_ext::ExtendedAttackKind::FakeCpm`]) and why the class-4
    /// and class-5 cross-checks of [`crate::ts103759`] compare them against the
    /// receiver's **own** perception rather than against the world.
    Cpm(Vec<PerceivedObject>),
}

/// One message this node received: every field a claim or an observation of its own.
///
/// Field for field the belief half of `v2xw_node::runtime::VerifiedMessage`, plus the
/// three fields the legacy detector suite reads that the node runtime carries elsewhere
/// (the repetition count from the MAC, the certificate validity window from the envelope,
/// and the broadcast position confidence from the payload).
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedMessage {
    /// The signer's certificate digest — the only identity a receiver has. `HashedId8`.
    pub signer: [u8; 8],
    /// What kind of message it is.
    pub kind: ObservedKind,
    /// When this node believes it arrived, on the node's own clock.
    pub received_at: SimTime,
    /// The generation time the message claims.
    pub claimed_generation_time: SimTime,
    /// The east coordinate it claims, world-local ENU metres.
    pub claimed_x_m: f64,
    /// The north coordinate it claims, world-local ENU metres.
    pub claimed_y_m: f64,
    /// The speed it claims, m/s.
    pub claimed_speed_mps: f64,
    /// The heading it claims, ENU radians, `0 = east`, counter-clockwise.
    pub claimed_heading_rad: f64,
    /// The horizontal position confidence the message **broadcasts**, metres.
    ///
    /// The sender's own claim about its uncertainty, which is why a sustained residual
    /// under an understated confidence reads as misbehaviour.
    pub claimed_pos_confidence_m: f64,
    /// How many copies of this message the sender put on the air in the current
    /// generation interval, as the receiver counted them. `1` for a conforming sender;
    /// a flooding attacker's burst size otherwise.
    pub repetitions: u32,
    /// The start of the signer's certificate validity window, as the envelope states it.
    pub cert_valid_from: SimTime,
    /// The end of the signer's certificate validity window, as the envelope states it.
    pub cert_valid_to: SimTime,
    /// The station type the message declares about its sender.
    pub station_type: StationType,
    /// What this node concluded about the signature.
    pub verification: VerificationState,
}

impl ObservedMessage {
    /// The signer digest as lowercase hex — the subject id a report and a
    /// `det.observation` record name.
    #[must_use]
    pub fn signer_hex(&self) -> String {
        v2xw_core::hash::hex_encode(&self.signer)
    }
}

/// One neighbour-table entry as a node knows one.
///
/// Mirrors `v2xw_node::stores::Neighbor`. Present so a detector that wants the node's
/// aggregate picture — how many distinct signers it is tracking, when it last heard one —
/// does not have to re-derive it from [`crate::detect::DetectorSuite`]'s own history.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerBelief {
    /// The signer's certificate digest.
    pub signer: [u8; 8],
    /// The last position this peer claimed, metres.
    pub claimed_x_m: f64,
    /// The last north coordinate this peer claimed, metres.
    pub claimed_y_m: f64,
    /// The last speed it claimed, m/s.
    pub claimed_speed_mps: f64,
    /// The last heading it claimed, ENU radians.
    pub claimed_heading_rad: f64,
    /// When this node believes it last heard this peer.
    pub last_heard: SimTime,
    /// How many messages this node has had from this signer.
    pub messages: u32,
}

/// What a node believes about *itself*, as far as a detector needs it.
///
/// This is the node's GNSS belief, not its true state: under spoofing or during an outage
/// it is far from the truth, and a detector that used the truth here would never see a
/// GNSS attack at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelfBelief {
    /// This node's id.
    pub node: NodeId,
    /// The instant this node believes it is, from its own clock.
    pub believed_time: SimTime,
    /// The east coordinate this node believes it is at, metres.
    pub x_m: f64,
    /// The north coordinate this node believes it is at, metres.
    pub y_m: f64,
    /// The range this node's own receiver is configured for, metres.
    ///
    /// A node property, not a world property: it is what the `acceptanceRangeThreshold`
    /// check compares against, and the legacy engine's own note is that an RSU with a
    /// longer range must use *its* range or it flags honest distant senders.
    pub radio_range_m: f64,
}

/// The node's own map store, seen from a detector.
///
/// A declared capability (07-threats §1, "Knowledge: map"), not a window on the world:
/// the implementation is the node's local HD map or LDM, and what it answers is a
/// distance from a *claimed* coordinate to the nearest drivable lane.
pub trait LocalEnvironment {
    /// The distance from `(x_m, y_m)` to the nearest drivable lane centre, metres; `0.0`
    /// when the point is on a lane.
    fn distance_to_road_m(&self, x_m: f64, y_m: f64) -> f64;
}

/// A node with no map: every point is on the road.
///
/// The honest answer for a node whose declared knowledge does not include a map. The
/// `mapOffRoad` detector then scores zero for every message, which is a detector that
/// cannot fire rather than a detector that quietly reads the world — and it is visible in
/// the scenario, because the node declared no map.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoMap;

impl LocalEnvironment for NoMap {
    fn distance_to_road_m(&self, _x_m: f64, _y_m: f64) -> f64 {
        0.0
    }
}

/// An identified region, as IEEE 1609.2 `IdentifiedRegion` and the ETSI authorization
/// ticket's region restriction carry one: an ISO 3166-1 numeric country code.
///
/// A certificate states the region it is valid in; using it elsewhere is the
/// certificate-misuse attack of 07-threats-and-detection.md §2.2, and the receiver's own
/// region is a node property (its own map and its own position), not a world one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RegionId(pub u16);

impl core::fmt::Display for RegionId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "region{}", self.0)
    }
}

/// One object a **received** collective-perception message claims its sender perceives.
///
/// Wire units, not floats: ETSI TS 103 324 encodes a perceived object's offsets and speed
/// as integers in hundredths of a metre (and of a metre per second), so an integer field
/// here is what a receiver actually decoded rather than a float somebody rounded. It also
/// keeps [`ObservedKind`] `Eq`, which a float field would not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PerceivedObject {
    /// The sender's own id for the object, as TS 103 324 `objectId` carries it.
    pub object_id: u16,
    /// The object's east offset from the world origin, hundredths of a metre.
    pub x_cm: i64,
    /// Its north offset, hundredths of a metre.
    pub y_cm: i64,
    /// Its speed, hundredths of a metre per second.
    pub speed_cm_s: i64,
    /// The sender's stated perception quality, `0..=15`
    /// (TS 103 324 §7.1.8.6 `objectPerceptionQuality`).
    pub quality: u8,
}

impl PerceivedObject {
    /// An object at a metric position, converted to the wire's hundredths.
    ///
    /// Rounds half away from zero, which is what [`v2xw_core::math::grid_index`] does for
    /// every other quantised field in this workspace.
    #[must_use]
    pub fn from_metres(object_id: u16, x_m: f64, y_m: f64, speed_mps: f64, quality: u8) -> Self {
        Self {
            object_id,
            x_cm: v2xw_core::math::grid_index(x_m, 0.01),
            y_cm: v2xw_core::math::grid_index(y_m, 0.01),
            speed_cm_s: v2xw_core::math::grid_index(speed_mps, 0.01),
            quality,
        }
    }

    /// The east offset in metres.
    #[must_use]
    pub fn x_m(&self) -> f64 {
        self.x_cm as f64 / 100.0
    }

    /// The north offset in metres.
    #[must_use]
    pub fn y_m(&self) -> f64 {
        self.y_cm as f64 / 100.0
    }

    /// The speed in metres per second.
    #[must_use]
    pub fn speed_mps(&self) -> f64 {
        self.speed_cm_s as f64 / 100.0
    }
}

/// One object the receiving node's **own** sensors detected.
///
/// This is the node's perception output — a belief, with the sensor model's error already
/// in it — and not a window on the world. A detector that compared a claim against the
/// true object list would report a cross-check accuracy no real receiver could reach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensedObject {
    /// The tracker's own id for the object.
    pub object_id: u32,
    /// Where this node's sensors believe the object is, east metres.
    pub x_m: f64,
    /// The same, north metres.
    pub y_m: f64,
    /// The speed its sensors estimate, m/s.
    pub speed_mps: f64,
    /// The detection's confidence in `[0, 1]`.
    pub confidence: f64,
    /// When this node's sensors last updated the object, on the node's own clock.
    pub at: SimTime,
}

/// The node's own perception, seen from a detector — the class-4 input of
/// ETSI TS 103 759 ("inconsistency with on-board sensors").
///
/// A declared capability (07-threats-and-detection.md §1, "Knowledge: sensing"), and the
/// same shape as [`LocalEnvironment`]: what it answers is what this node's sensors
/// believe, so the cross-check is the one a real receiver could run.
///
/// [`NoPerception`] is the honest answer for a node without sensors. It reports
/// [`LocalPerception::available`] `false`, and the cross-check then **counts the message
/// as unchecked** instead of scoring it zero — see
/// [`crate::ts103759::Ts103759Suite::class4_skipped_no_perception`]. A check that silently
/// cannot fire is worse than no check, because the run still reports a recall for it.
pub trait LocalPerception {
    /// Whether this node has a perception sensor at all.
    fn available(&self) -> bool;

    /// The sensor's maximum range, metres.
    fn range_m(&self) -> f64;

    /// Half the horizontal field of view, radians, measured about the boresight.
    fn half_fov_rad(&self) -> f64;

    /// The boresight direction, ENU radians — this node's own heading for a
    /// forward-looking sensor.
    fn boresight_rad(&self) -> f64;

    /// The objects the node's tracker currently holds.
    fn objects(&self) -> &[SensedObject];

    /// Whether the line of sight from this node to `(x_m, y_m)` is blocked.
    ///
    /// The default is `false`: `perception/disc-sensor` (04-models.md §12.1, the medium
    /// tier) models no occlusion, and `perception/occluded-sensor` (high tier) overrides
    /// this with the obstacle model's line-of-sight answer.
    fn occluded(&self, _x_m: f64, _y_m: f64) -> bool {
        false
    }

    /// Whether `(x_m, y_m)` is inside this node's sensor coverage: in range, inside the
    /// field of view, and not occluded.
    ///
    /// The cross-check may only conclude anything about a claim inside coverage. Outside
    /// it, the absence of a sensed object is the sensor's blind spot rather than evidence.
    fn covers(&self, me: &SelfBelief, x_m: f64, y_m: f64) -> bool {
        if !self.available() {
            return false;
        }
        let (dx, dy) = (x_m - me.x_m, y_m - me.y_m);
        let d = v2xw_core::math::hypot(dx, dy);
        if d > self.range_m() {
            return false;
        }
        if d > 0.0 {
            let bearing = v2xw_core::math::atan2(dy, dx);
            if crate::capability::angle_diff_rad(bearing, self.boresight_rad())
                > self.half_fov_rad()
            {
                return false;
            }
        }
        !self.occluded(x_m, y_m)
    }

    /// The distance from `(x_m, y_m)` to the nearest object this node's sensors hold, or
    /// `None` when it holds none.
    ///
    /// Iterated in the tracker's own order and reduced with a strict `<`, so the answer
    /// does not depend on iteration order beyond which of two exactly equal distances is
    /// reported — and the distance, not the object, is what the caller uses.
    fn nearest_object_m(&self, x_m: f64, y_m: f64) -> Option<f64> {
        let mut best: Option<f64> = None;
        for o in self.objects() {
            let d = v2xw_core::math::hypot(o.x_m - x_m, o.y_m - y_m);
            if best.is_none_or(|b| d < b) {
                best = Some(d);
            }
        }
        best
    }
}

/// A node with no perception: every cross-check is *unchecked*, not passed.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPerception;

impl LocalPerception for NoPerception {
    fn available(&self) -> bool {
        false
    }

    fn range_m(&self) -> f64 {
        0.0
    }

    fn half_fov_rad(&self) -> f64 {
        0.0
    }

    fn boresight_rad(&self) -> f64 {
        0.0
    }

    fn objects(&self) -> &[SensedObject] {
        &[]
    }
}

/// `perception/disc-sensor` as a detector input: a range, a field of view, no occlusion
/// (04-models.md §12.1, the medium tier).
///
/// The two constructors carry the only two sensor envelopes 04-models.md §12.1 records as
/// VERIFIED. The detection-probability curve is UNVERIFIED there for every sensor, so this
/// type holds whatever objects the node's perception model produced and adds no curve of
/// its own.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscSensor {
    /// The maximum range, metres.
    pub range_m: f64,
    /// Half the horizontal field of view, radians.
    pub half_fov_rad: f64,
    /// The boresight, ENU radians.
    pub boresight_rad: f64,
    /// What the node's tracker holds.
    pub objects: Vec<SensedObject>,
}

impl DiscSensor {
    /// The VSC-A reference forward-looking radar: 3–150 m, ±7.5°, 10 Hz
    /// (04-models.md §12.1, VERIFIED against VSC-A Table 3).
    #[must_use]
    pub fn vsc_a_flr(boresight_rad: f64, objects: Vec<SensedObject>) -> Self {
        Self {
            range_m: 150.0,
            half_fov_rad: 7.5_f64.to_radians(),
            boresight_rad,
            objects,
        }
    }

    /// An ARS 408-class front radar in far-range mode: 250 m, field of view
    /// `todo-calibrate` (04-models.md §12.1 records no FOV for it on the cached page).
    ///
    /// `half_fov_rad` is therefore the caller's, and the card of any model that builds one
    /// must declare it as an uncalibrated parameter.
    #[must_use]
    pub fn ars408_far(boresight_rad: f64, half_fov_rad: f64, objects: Vec<SensedObject>) -> Self {
        Self {
            range_m: 250.0,
            half_fov_rad,
            boresight_rad,
            objects,
        }
    }
}

impl LocalPerception for DiscSensor {
    fn available(&self) -> bool {
        true
    }

    fn range_m(&self) -> f64 {
        self.range_m
    }

    fn half_fov_rad(&self) -> f64 {
        self.half_fov_rad
    }

    fn boresight_rad(&self) -> f64 {
        self.boresight_rad
    }

    fn objects(&self) -> &[SensedObject] {
        &self.objects
    }
}

/// The two envelope fields a receiver reads off a frame that [`ObservedMessage`] does not
/// carry, for the checks that need them.
///
/// They are separate rather than fields on [`ObservedMessage`] because that type is the
/// frozen seam the node runtime fills in (see the module documentation): a receiver that
/// does not record the SPDU's size or the certificate's region simply has no answer, and
/// `None` here says exactly that. A check whose input is missing is *unchecked*, which is
/// how [`crate::ts103759`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EnvelopeExtras {
    /// The signed-message payload's size in bytes, as the receiver measured it.
    pub payload_bytes: Option<u32>,
    /// The region the signer's certificate states it is valid in.
    pub cert_region: Option<RegionId>,
}

impl EnvelopeExtras {
    /// Extras carrying only a measured payload size.
    #[must_use]
    pub const fn with_payload_bytes(payload_bytes: u32) -> Self {
        Self {
            payload_bytes: Some(payload_bytes),
            cert_region: None,
        }
    }

    /// Extras carrying only a stated certificate region.
    #[must_use]
    pub const fn with_region(region: RegionId) -> Self {
        Self {
            payload_bytes: None,
            cert_region: Some(region),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compile-time half of invariant I-T2: if any ground-truth-bearing field ever
    /// appeared on these types, this test would have to name it to keep compiling.
    #[test]
    fn a_belief_input_is_constructible_without_any_ground_truth() {
        let m = ObservedMessage {
            signer: [1, 2, 3, 4, 5, 6, 7, 8],
            kind: ObservedKind::Beacon,
            received_at: 1_000_000_000,
            claimed_generation_time: 1_000_000_000,
            claimed_x_m: 10.0,
            claimed_y_m: 20.0,
            claimed_speed_mps: 15.0,
            claimed_heading_rad: 0.0,
            claimed_pos_confidence_m: 2.0,
            repetitions: 1,
            cert_valid_from: 0,
            cert_valid_to: 100_000_000_000,
            station_type: StationType::Vehicle,
            verification: VerificationState::Valid,
        };
        assert_eq!(m.signer_hex(), "0102030405060708");
        assert!(m.verification.is_valid());
        assert_eq!(StationType::Vru.as_str(), "vru");
        assert_eq!(NoMap.distance_to_road_m(1e6, -1e6), 0.0);
    }
}

#[cfg(test)]
mod perception_tests {
    use super::*;
    use v2xw_core::ids::NodeId;

    fn me() -> SelfBelief {
        SelfBelief {
            node: NodeId::new(1),
            believed_time: 1_000_000_000,
            x_m: 0.0,
            y_m: 0.0,
            radio_range_m: 500.0,
        }
    }

    #[test]
    fn a_perceived_object_round_trips_through_the_wire_hundredths() {
        let o = PerceivedObject::from_metres(7, 12.345, -3.5, 8.9, 12);
        assert_eq!(o.x_cm, 1235);
        assert_eq!(o.y_cm, -350);
        assert_eq!(o.speed_cm_s, 890);
        assert_eq!(o.x_m(), 12.35);
        assert_eq!(o.y_m(), -3.5);
        assert_eq!(o.speed_mps(), 8.9);
        assert_eq!(o.quality, 12);
    }

    #[test]
    fn no_perception_covers_nothing_and_holds_nothing() {
        // The honest answer for a node without sensors: not "the claim checks out".
        assert!(!NoPerception.available());
        assert!(!NoPerception.covers(&me(), 1.0, 0.0));
        assert_eq!(NoPerception.nearest_object_m(0.0, 0.0), None);
    }

    #[test]
    fn a_disc_sensor_covers_its_range_and_field_of_view_only() {
        let s = DiscSensor::vsc_a_flr(0.0, Vec::new());
        assert_eq!(s.range_m, 150.0);
        // Straight ahead, in range.
        assert!(s.covers(&me(), 100.0, 0.0));
        // Straight ahead, beyond range.
        assert!(!s.covers(&me(), 200.0, 0.0));
        // In range but outside the ±7.5° field of view (45° off the boresight).
        assert!(!s.covers(&me(), 70.0, 70.0));
    }

    #[test]
    fn the_nearest_sensed_object_is_a_distance_from_the_claim() {
        let s = DiscSensor::vsc_a_flr(
            0.0,
            vec![
                SensedObject {
                    object_id: 1,
                    x_m: 50.0,
                    y_m: 0.0,
                    speed_mps: 10.0,
                    confidence: 0.9,
                    at: 1_000_000_000,
                },
                SensedObject {
                    object_id: 2,
                    x_m: 90.0,
                    y_m: 3.0,
                    speed_mps: 12.0,
                    confidence: 0.8,
                    at: 1_000_000_000,
                },
            ],
        );
        let d = s.nearest_object_m(52.0, 0.0).unwrap();
        assert!((d - 2.0).abs() < 1e-9);
    }

    #[test]
    fn envelope_extras_say_none_rather_than_a_default_number() {
        let e = EnvelopeExtras::default();
        assert_eq!(e.payload_bytes, None);
        assert_eq!(e.cert_region, None);
        assert_eq!(
            EnvelopeExtras::with_payload_bytes(2304).payload_bytes,
            Some(2304)
        );
        assert_eq!(
            EnvelopeExtras::with_region(RegionId(840)).cert_region,
            Some(RegionId(840))
        );
        assert_eq!(RegionId(840).to_string(), "region840");
    }

    #[test]
    fn a_collective_perception_message_carries_its_objects() {
        let objects = vec![PerceivedObject::from_metres(1, 10.0, 0.0, 5.0, 10)];
        let k = ObservedKind::Cpm(objects.clone());
        assert_eq!(k, ObservedKind::Cpm(objects));
        assert_ne!(k, ObservedKind::Beacon);
    }
}
