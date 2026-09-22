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
