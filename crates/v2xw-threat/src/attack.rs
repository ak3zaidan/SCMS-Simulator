//! The attacker interface: an attacker is a node whose behaviour differs, not a special
//! case threaded through the engine.
//!
//! # Shape
//!
//! A node's message stack builds what it is about to send — an [`Emission`] — and, if the
//! node is an attacker, hands it to [`Attacker::act`] *before signing*. The attacker edits
//! the claim, adds ghost transmissions, suppresses the message or leaves it alone, and
//! returns the [`AttackAction`]s it took. Everything downstream — signing, the MAC, DCC,
//! the channel — is the ordinary path, which is what makes a flooding attacker's rate
//! bounded by its own hardware and its own DCC rather than by a special case.
//!
//! # Who writes the ground-truth record
//!
//! Invariant I-T3 says every action that changes bytes on the air is logged with the
//! **true actor id**. An attacker does not know its actor id — that is invariant I-T1 —
//! so it cannot write that record, and [`AttackerView`] deliberately has no
//! [`v2xw_core::ids::ActorId`] on it. The attacker returns its actions; the host, which
//! knows which actor carries this node, calls [`log_actions`]. The split is the firewall:
//! the only code that can name the actor is code that never sees the attacker's inputs.
//!
//! # Magnitudes
//!
//! Every falsification magnitude in [`crate::attack_legacy`] is the legacy engine's, cited
//! to `legacy/scms_sim_ref/mock_pipeline/run.py :: attack_claim` and to the broadcast
//! pre-pass that renders the envelope-level and timing edits. 07-threats §2.1 requires the
//! renderings to be preserved as defaults because the legacy datasets were generated with
//! them and must stay reproducible.

use crate::capability::{AttackSchedule, Capabilities};
use crate::ctx::{ThreatCtx, ThreatCtxExt};
use crate::obs::{ObservedMessage, SelfBelief, StationType};
use crate::records::GtAttackAction;
use v2xw_core::ids::ActorId;
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;

/// Which behavioural family an attack belongs to.
///
/// The nine families are the legacy feature pipeline's own
/// (`legacy/scms_sim_ref/datagen/featurize.py :: _ATTACK_FAMILY`), so a ported run's
/// per-family breakdown is comparable with the legacy corpus, plus
/// [`AttackFamily::Suppression`] for the message-suppression family 07-threats §2.2 adds
/// and the legacy engine never had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttackFamily {
    /// Falsified position.
    Position,
    /// Falsified speed.
    Speed,
    /// Falsified heading.
    Heading,
    /// Several fields falsified mutually inconsistently.
    Combined,
    /// Replay, delay, reordering and flooding: the message's timing rather than content.
    Timing,
    /// Small offsets designed to stay under a plausibility threshold.
    Stealth,
    /// Sybil and impersonation: the sender's claimed identity or class.
    Identity,
    /// Envelope-level: bad signature, expired or not-yet-valid certificate.
    Credential,
    /// Event messages: phantom hazards.
    Event,
    /// Selective non-forwarding.
    Suppression,
    /// Raw energy on the channel rather than frames (07-threats §2.2, 04-models §12.3).
    Jamming,
    /// An attack mounted from infrastructure: a compromised road-side unit.
    Infrastructure,
    /// False misbehaviour reports: an attack on the authority rather than on the air.
    Poisoning,
    /// Passive tracking: an adversary that never transmits (07-threats §6).
    Privacy,
}

impl AttackFamily {
    /// The family's name, as the legacy feature tables spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AttackFamily::Position => "position",
            AttackFamily::Speed => "speed",
            AttackFamily::Heading => "heading",
            AttackFamily::Combined => "combined",
            AttackFamily::Timing => "timing",
            AttackFamily::Stealth => "stealth",
            AttackFamily::Identity => "identity",
            AttackFamily::Credential => "credential",
            AttackFamily::Event => "event",
            AttackFamily::Suppression => "suppression",
            AttackFamily::Jamming => "jamming",
            AttackFamily::Infrastructure => "infrastructure",
            AttackFamily::Poisoning => "poisoning",
            AttackFamily::Privacy => "privacy",
        }
    }

    /// The families the legacy feature pipeline knows, in its own order.
    ///
    /// The four that follow them are new with 07-threats §2.2 and §6, so a per-family
    /// table from a ported run and one from the legacy corpus line up on the first ten
    /// rows and the new families appear as new rows rather than displacing anything.
    pub const LEGACY: [AttackFamily; 9] = [
        AttackFamily::Position,
        AttackFamily::Speed,
        AttackFamily::Heading,
        AttackFamily::Combined,
        AttackFamily::Timing,
        AttackFamily::Stealth,
        AttackFamily::Identity,
        AttackFamily::Credential,
        AttackFamily::Event,
    ];

    /// Every family, legacy first.
    pub const ALL: [AttackFamily; 14] = [
        AttackFamily::Position,
        AttackFamily::Speed,
        AttackFamily::Heading,
        AttackFamily::Combined,
        AttackFamily::Timing,
        AttackFamily::Stealth,
        AttackFamily::Identity,
        AttackFamily::Credential,
        AttackFamily::Event,
        AttackFamily::Suppression,
        AttackFamily::Jamming,
        AttackFamily::Infrastructure,
        AttackFamily::Poisoning,
        AttackFamily::Privacy,
    ];
}

impl core::fmt::Display for AttackFamily {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One attack type, by the legacy name.
///
/// The names are load-bearing: a scenario file, a legacy dataset column and a foundry
/// genome all carry them as strings, so renaming one would break replay of the legacy
/// corpus. [`AttackKind::LEGACY_CATALOG`] is the frozen default round-robin, in the legacy
/// order, and [`AttackKind::ALL`] adds the opt-in families and the one new type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttackKind {
    /// Freeze the claimed position at the first value seen.
    ConstPos,
    /// Add a constant offset to the claimed position.
    ConstPosOffset,
    /// Add a uniform random offset to the claimed position, redrawn every message.
    RandomPos,
    /// Jump the claimed position every fourth second.
    Teleport,
    /// Oscillate the claimed position across the direction of travel.
    SineWavePos,
    /// Add a constant offset to the claimed speed.
    ConstSpeedOffset,
    /// Claim a uniformly random speed.
    RandomSpeed,
    /// Alternate the claimed speed between stopped and fast.
    StopAndGo,
    /// Claim the reciprocal heading.
    ReversedHeading,
    /// Add a constant offset to the claimed heading.
    HeadingOffset,
    /// Replay its own stored earlier state with its old generation time.
    DataReplay,
    /// A position drift whose rate ramps up from nothing.
    SlowDrift,
    /// Offset the claimed position along the road, where it stays on the map.
    AlongRoadOffset,
    /// Transmit from several concurrent valid pseudonyms at nearly one point.
    Sybil,
    /// Flood the channel with repetitions of an otherwise honest message.
    DoS,
    /// Stamp the message with a stale generation time.
    DelayedMessages,
    /// Sign with a key that does not match the attached certificate.
    InvalidSignature,
    /// Present a certificate past its validity window.
    ExpiredCert,
    /// Present a certificate whose validity has not begun.
    NotYetValid,
    /// Stamp messages with non-monotonic generation times.
    OutOfOrder,
    /// Flood the channel with randomised content.
    DoSRandom,
    /// Falsify position, speed and heading at once, mutually inconsistently.
    Disruptive,
    /// Keep the claimed position advancing while claiming to be nearly stopped.
    PosSpeedInconsistent,
    /// Swing the claimed position sideways while claiming a straight-ahead heading.
    PosHeadingInconsistent,
    /// Drive honestly, then freeze the claimed position while still claiming to creep.
    EventualStop,
    /// Declare the vulnerable-road-user station type while driving at vehicle speed.
    VruImpersonation,
    /// Declare the vulnerable-road-user station type, claim a walking speed, and
    /// teleport the claimed position.
    VruPositionSpoof,
    /// Emit an event message announcing a hazard the sender's own kinematics contradict.
    FakeHazard,
    /// Silently fail to forward messages this node was asked to relay.
    ///
    /// The one type with no legacy rendering: 07-threats §2.2 requires message
    /// suppression (multi-hop GeoNetworking, DENM keep-alive, CRL/CTL rebroadcast) and
    /// the legacy engine had no forwarding role at all, so its drop probability is a
    /// `todo-calibrate` parameter rather than a ported constant.
    SelectiveDrop,
}

impl AttackKind {
    /// The legacy default round-robin, in the legacy order.
    ///
    /// `legacy/scms_sim_ref/mock_pipeline/run.py :: ATTACK_CATALOG`, whose own comment
    /// states the order is frozen because the golden data digest depends on it.
    pub const LEGACY_CATALOG: [AttackKind; 21] = [
        AttackKind::ConstPos,
        AttackKind::ConstPosOffset,
        AttackKind::RandomPos,
        AttackKind::Teleport,
        AttackKind::SineWavePos,
        AttackKind::ConstSpeedOffset,
        AttackKind::RandomSpeed,
        AttackKind::StopAndGo,
        AttackKind::ReversedHeading,
        AttackKind::HeadingOffset,
        AttackKind::DataReplay,
        AttackKind::SlowDrift,
        AttackKind::AlongRoadOffset,
        AttackKind::Sybil,
        AttackKind::DoS,
        AttackKind::DelayedMessages,
        AttackKind::InvalidSignature,
        AttackKind::ExpiredCert,
        AttackKind::NotYetValid,
        AttackKind::OutOfOrder,
        AttackKind::DoSRandom,
    ];

    /// The opt-in "combined" family (`run.py :: COMBINED_ATTACKS`).
    pub const COMBINED: [AttackKind; 4] = [
        AttackKind::Disruptive,
        AttackKind::PosSpeedInconsistent,
        AttackKind::PosHeadingInconsistent,
        AttackKind::EventualStop,
    ];

    /// The opt-in identity-spoofing family (`run.py :: IDENTITY_SPOOF_ATTACKS`).
    pub const IDENTITY_SPOOF: [AttackKind; 2] =
        [AttackKind::VruImpersonation, AttackKind::VruPositionSpoof];

    /// The opt-in event-message family (`run.py :: DENM_ATTACKS`).
    pub const DENM: [AttackKind; 1] = [AttackKind::FakeHazard];

    /// Every type this crate renders: the legacy 28 plus [`AttackKind::SelectiveDrop`].
    pub const ALL: [AttackKind; 29] = [
        AttackKind::ConstPos,
        AttackKind::ConstPosOffset,
        AttackKind::RandomPos,
        AttackKind::Teleport,
        AttackKind::SineWavePos,
        AttackKind::ConstSpeedOffset,
        AttackKind::RandomSpeed,
        AttackKind::StopAndGo,
        AttackKind::ReversedHeading,
        AttackKind::HeadingOffset,
        AttackKind::DataReplay,
        AttackKind::SlowDrift,
        AttackKind::AlongRoadOffset,
        AttackKind::Sybil,
        AttackKind::DoS,
        AttackKind::DelayedMessages,
        AttackKind::InvalidSignature,
        AttackKind::ExpiredCert,
        AttackKind::NotYetValid,
        AttackKind::OutOfOrder,
        AttackKind::DoSRandom,
        AttackKind::Disruptive,
        AttackKind::PosSpeedInconsistent,
        AttackKind::PosHeadingInconsistent,
        AttackKind::EventualStop,
        AttackKind::VruImpersonation,
        AttackKind::VruPositionSpoof,
        AttackKind::FakeHazard,
        AttackKind::SelectiveDrop,
    ];

    /// The legacy name, as a scenario and a dataset column spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AttackKind::ConstPos => "ConstPos",
            AttackKind::ConstPosOffset => "ConstPosOffset",
            AttackKind::RandomPos => "RandomPos",
            AttackKind::Teleport => "Teleport",
            AttackKind::SineWavePos => "SineWavePos",
            AttackKind::ConstSpeedOffset => "ConstSpeedOffset",
            AttackKind::RandomSpeed => "RandomSpeed",
            AttackKind::StopAndGo => "StopAndGo",
            AttackKind::ReversedHeading => "ReversedHeading",
            AttackKind::HeadingOffset => "HeadingOffset",
            AttackKind::DataReplay => "DataReplay",
            AttackKind::SlowDrift => "SlowDrift",
            AttackKind::AlongRoadOffset => "AlongRoadOffset",
            AttackKind::Sybil => "Sybil",
            AttackKind::DoS => "DoS",
            AttackKind::DelayedMessages => "DelayedMessages",
            AttackKind::InvalidSignature => "InvalidSignature",
            AttackKind::ExpiredCert => "ExpiredCert",
            AttackKind::NotYetValid => "NotYetValid",
            AttackKind::OutOfOrder => "OutOfOrder",
            AttackKind::DoSRandom => "DoSRandom",
            AttackKind::Disruptive => "Disruptive",
            AttackKind::PosSpeedInconsistent => "PosSpeedInconsistent",
            AttackKind::PosHeadingInconsistent => "PosHeadingInconsistent",
            AttackKind::EventualStop => "EventualStop",
            AttackKind::VruImpersonation => "VruImpersonation",
            AttackKind::VruPositionSpoof => "VruPositionSpoof",
            AttackKind::FakeHazard => "FakeHazard",
            AttackKind::SelectiveDrop => "SelectiveDrop",
        }
    }

    /// Parses a legacy name. `None` for anything not in [`AttackKind::ALL`].
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        AttackKind::ALL.into_iter().find(|k| k.as_str() == name)
    }

    /// The behavioural family, as `featurize.py :: _ATTACK_FAMILY` maps it.
    #[must_use]
    pub const fn family(self) -> AttackFamily {
        match self {
            AttackKind::ConstPos
            | AttackKind::ConstPosOffset
            | AttackKind::RandomPos
            | AttackKind::Teleport
            | AttackKind::SineWavePos => AttackFamily::Position,
            AttackKind::ConstSpeedOffset | AttackKind::RandomSpeed | AttackKind::StopAndGo => {
                AttackFamily::Speed
            }
            AttackKind::ReversedHeading | AttackKind::HeadingOffset => AttackFamily::Heading,
            AttackKind::Disruptive
            | AttackKind::PosSpeedInconsistent
            | AttackKind::PosHeadingInconsistent
            | AttackKind::EventualStop => AttackFamily::Combined,
            AttackKind::DataReplay
            | AttackKind::DelayedMessages
            | AttackKind::OutOfOrder
            | AttackKind::DoS
            | AttackKind::DoSRandom => AttackFamily::Timing,
            AttackKind::SlowDrift | AttackKind::AlongRoadOffset => AttackFamily::Stealth,
            AttackKind::Sybil | AttackKind::VruImpersonation | AttackKind::VruPositionSpoof => {
                AttackFamily::Identity
            }
            AttackKind::InvalidSignature | AttackKind::ExpiredCert | AttackKind::NotYetValid => {
                AttackFamily::Credential
            }
            AttackKind::FakeHazard => AttackFamily::Event,
            AttackKind::SelectiveDrop => AttackFamily::Suppression,
        }
    }

    /// True when the type has no falsification amplitude to scale.
    ///
    /// `run.py :: _UNSCALABLE_MAGNITUDE_TYPES`: `ConstPos` freezes to the first-seen
    /// position, `ReversedHeading` is a fixed 180° flip, `DataReplay` replays real past
    /// state, and `VruImpersonation`/`FakeHazard` broadcast honest beacons. The legacy
    /// engine rejects a magnitude scale for these rather than silently ignoring it, and so
    /// does [`crate::attack_legacy::LegacyAttackerParams::with_magnitude_scale`].
    #[must_use]
    pub const fn is_magnitude_scalable(self) -> bool {
        !matches!(
            self,
            AttackKind::ConstPos
                | AttackKind::ReversedHeading
                | AttackKind::DataReplay
                | AttackKind::VruImpersonation
                | AttackKind::FakeHazard
        )
    }
}

impl core::fmt::Display for AttackKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the attacker's own stack would have put on the air: the node's measured state.
///
/// Node-visible by construction — it is this node's GNSS belief, which already carries
/// the error the GNSS model gave it. An attacker falsifies relative to *this*, not
/// relative to the truth, which is why a falsification magnitude in this crate is a
/// magnitude of the lie and not a distance from the world.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HonestClaim {
    /// East, world-local ENU metres.
    pub x_m: f64,
    /// North, world-local ENU metres.
    pub y_m: f64,
    /// Speed, m/s.
    pub speed_mps: f64,
    /// Heading, ENU radians, `0 = east`.
    pub heading_rad: f64,
}

/// One event message an attacker fabricated.
#[derive(Debug, Clone, PartialEq)]
pub struct EventClaim {
    /// The ETSI cause-code name the message announces, e.g.
    /// `emergencyElectronicBrakeLight`.
    pub event_type: String,
    /// The position it announces the event at, metres.
    pub x_m: f64,
    /// The north coordinate it announces the event at, metres.
    pub y_m: f64,
    /// The sender's own claimed speed, carried on the message — which is what makes a
    /// phantom brake announcement contradict itself.
    pub claimed_speed_mps: f64,
}

/// Raw energy an attacker puts on the channel instead of a frame (07-threats §2.2,
/// "PHY jamming and flooding"; 04-models §12.3 for the profiles and their anchors).
///
/// The attacker declares it and the host applies it: a jammer is an interferer in the
/// SINR sums (04-models §12.3), which is the radio crate's to add, so this type is the
/// declaration and the `TransmitRaw` action is its ground-truth record. What it is *not*
/// is a shortcut that deletes frames at the receiver — a jammer that did that would report
/// a blind area that owed nothing to propagation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawEnergy {
    /// The profile: `constant`, `reactive` or `random-duty` (04-models §12.3).
    pub profile: JamProfile,
    /// Transmit power, dBm. Bounded by the attacker's declared
    /// [`crate::capability::RadioCaps::max_power_dbm`]; the MAC/PHY reject more.
    pub power_dbm: f64,
    /// How long the burst lasts, microseconds.
    pub duration_us: u64,
    /// The RSSI a reactive jammer triggers on, dBm. `None` for the profiles that do not
    /// trigger.
    pub trigger_dbm: Option<f64>,
}

/// Which jammer profile (04-models §12.3, Puñal, Aguiar and Gross 2012).
///
/// The radio crate's `v2xw_radio::jamming::JammerKind` is the same three profiles seen
/// from the PHY, spelled `Constant`, `Pulsed` and `Reactive`; the engine maps
/// [`JamProfile::RandomDuty`] onto its `Pulsed`. Two spellings exist because the adversary
/// declares a profile before the radio has any windows to partition, and neither crate
/// depends on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JamProfile {
    /// Continuous OFDM-like noise in the channel.
    Constant,
    /// Transmits only when energy above the trigger threshold is sensed.
    Reactive,
    /// On for a fraction of each period, off for the rest.
    RandomDuty,
}

impl JamProfile {
    /// The profile's name, as the `gt.attack.action` record and the model id carry it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            JamProfile::Constant => "constant",
            JamProfile::Reactive => "reactive",
            JamProfile::RandomDuty => "random-duty",
        }
    }
}

impl core::fmt::Display for JamProfile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One infrastructure message a compromised road-side unit falsifies (07-threats §2.2,
/// "Compromised RSU").
///
/// A declaration, like [`RawEnergy`]: the SPaT/MAP and CRL/CTL generators are
/// `v2xw-msg`'s and `v2xw-proto`'s, so what this crate can do honestly is say which
/// message was falsified, in which field, by how much — which is what the ground-truth
/// channel needs and what a safety-application join later reads.
#[derive(Debug, Clone, PartialEq)]
pub struct InfraClaim {
    /// Which infrastructure message: `spat`, `map`, `crl` or `ctl`.
    pub message: String,
    /// The field falsified, e.g. `phase`, `lane-connection`, `entries`.
    pub field: String,
    /// The size of the falsification in that field's own units: seconds of fabricated
    /// green for a `spat` phase, a count of fabricated entries for a `crl`.
    pub magnitude: f64,
}

/// What a node is about to transmit, before signing: the attacker's canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct Emission {
    /// The signer digest this message will be signed with.
    pub signer: [u8; 8],
    /// The claimed east coordinate, metres.
    pub x_m: f64,
    /// The claimed north coordinate, metres.
    pub y_m: f64,
    /// The claimed speed, m/s.
    pub speed_mps: f64,
    /// The claimed heading, ENU radians.
    pub heading_rad: f64,
    /// The generation time the message will claim.
    pub generation_time: SimTime,
    /// The station type it will declare.
    pub station_type: StationType,
    /// How many copies go on the air this generation interval. `1` for a conforming
    /// sender; the MAC and DCC still bound what actually leaves.
    pub repetitions: u32,
    /// Whether the signature will verify. `false` renders a forged or tampered message.
    pub signature_valid: bool,
    /// The certificate validity window the envelope will state.
    pub cert_valid_from: SimTime,
    /// The end of that window.
    pub cert_valid_to: SimTime,
    /// Set by a suppression attack: the node drops this message instead of sending it.
    pub suppressed: bool,
    /// Extra transmissions from other identities this node holds — the Sybil ghosts.
    pub ghosts: Vec<Emission>,
    /// Event messages this node emits alongside the beacon.
    pub events: Vec<EventClaim>,
    /// The application payload's size in bytes, when the attacker sets it.
    ///
    /// `None` leaves the node's own generator in charge, which is the conforming case.
    /// `Some` is the oversized-message flood of 07-threats §2.2: the MAC's maximum MSDU is
    /// 2 304 B (04-models §4.6), and a sender that emits that at a high rate exhausts a
    /// receiver's verification budget and reassembly buffers without forging anything.
    pub payload_bytes: Option<u32>,
    /// The region the envelope's certificate states it is valid in.
    ///
    /// `None` is "whatever the node's own credential says". `Some` renders the
    /// certificate-misuse-across-regions attack (07-threats §2.2): a valid credential used
    /// outside its `IdentifiedRegion`.
    pub cert_region: Option<crate::obs::RegionId>,
    /// Objects this emission claims to perceive — a collective-perception payload.
    ///
    /// A phantom entry here is the false-CPM attack of 07-threats §2.2, and it is the
    /// thing the class-4 and class-5 cross-checks of [`crate::ts103759`] exist to catch.
    pub perceived: Vec<crate::obs::PerceivedObject>,
    /// Raw energy this node puts on the channel alongside (or instead of) the frame.
    pub raw_energy: Option<RawEnergy>,
    /// True when this emission is a **verbatim retransmission** of a frame this node
    /// captured, rather than something this node signed.
    ///
    /// The host must put the stored frame on the air unchanged instead of re-signing:
    /// that is what makes a replay or a relay (wormhole) verify at the receiver even
    /// though the attacker holds none of the original signer's keys. An emission with
    /// `replayed` set carries the *captured* signer and the *captured* generation time.
    pub replayed: bool,
    /// Infrastructure messages this node falsifies — a compromised road-side unit only.
    pub infra: Vec<InfraClaim>,
}

impl Emission {
    /// A conforming emission carrying `honest`, signed by `signer`, valid for
    /// `[cert_valid_from, cert_valid_to]`.
    #[must_use]
    pub fn honest(
        signer: [u8; 8],
        honest: HonestClaim,
        generation_time: SimTime,
        cert_valid_from: SimTime,
        cert_valid_to: SimTime,
    ) -> Self {
        Self {
            signer,
            x_m: honest.x_m,
            y_m: honest.y_m,
            speed_mps: honest.speed_mps,
            heading_rad: honest.heading_rad,
            generation_time,
            station_type: StationType::Vehicle,
            repetitions: 1,
            signature_valid: true,
            cert_valid_from,
            cert_valid_to,
            suppressed: false,
            ghosts: Vec::new(),
            events: Vec::new(),
            payload_bytes: None,
            cert_region: None,
            perceived: Vec::new(),
            raw_energy: None,
            replayed: false,
            infra: Vec::new(),
        }
    }

    /// How many messages this emission puts on the air, ghosts and event messages
    /// included, before the MAC and DCC get a say.
    ///
    /// `repetitions` counts the copies of the beacon; a ghost carries its own.
    #[must_use]
    pub fn messages_on_air(&self) -> u64 {
        let ghosts: u64 = self.ghosts.iter().map(|g| u64::from(g.repetitions)).sum();
        if self.suppressed {
            return 0;
        }
        u64::from(self.repetitions) + ghosts + self.events.len() as u64
    }
}

/// Everything an attacker can see (03-interfaces.md §9, invariant I-T1).
///
/// No world, no actor index, no actor id, no true position and no simulator clock. What a
/// field here says is either something this node received, something this node holds, or
/// something this node believes about itself.
#[derive(Debug, Clone, Copy)]
pub struct AttackerView<'a> {
    /// The messages this node received and verified.
    pub own_rx: &'a [ObservedMessage],
    /// The digests of the credentials this node holds.
    pub own_credentials: &'a [[u8; 8]],
    /// How many revocations this node has seen on the **public** CRL.
    ///
    /// `None` when the attacker did not declare [`crate::capability::Knowledge::crl`]:
    /// the view carries only declared knowledge. The number is public information — a CRL
    /// is broadcast — not the authority's internal state, which is what makes CRL-aware
    /// evasion a realistic adversary rather than an omniscient one.
    pub crl_revocations_seen: Option<u32>,
    /// What this node believes about itself.
    pub own_belief: SelfBelief,
    /// What its own stack would have claimed: its measured state.
    pub honest: HonestClaim,
    /// The instant this node believes it is. Every schedule decision reads this, not the
    /// host clock.
    pub believed_time: SimTime,
}

/// One action an attacker took (03-interfaces.md §9).
#[derive(Debug, Clone, PartialEq)]
pub enum AttackAction {
    /// Edited fields of the outgoing message before signing.
    FalsifyOutgoing {
        /// The fields changed, sorted: `position`, `speed`, `heading`,
        /// `generation_time`, `station_type`, `signature`, `certificate`,
        /// `repetitions`.
        fields: Vec<String>,
        /// The magnitude of the edit in the units of its principal field.
        magnitude: f64,
    },
    /// Signed with a different credential from the attacker's own set.
    UseCredential {
        /// The digest used.
        signer: [u8; 8],
    },
    /// Dropped a message this node was asked to forward.
    Suppress,
    /// Delayed a message.
    Delay {
        /// How much staleness the message carries, seconds.
        seconds: f64,
    },
    /// Retransmitted stored earlier state.
    Replay {
        /// How old the replayed state is, seconds.
        age_s: f64,
    },
    /// Transmitted from a fabricated concurrent identity.
    Ghost {
        /// The ghost's signer digest.
        signer: [u8; 8],
    },
    /// Emitted an event message announcing a hazard that is not happening.
    ForgeEvent {
        /// The cause code announced.
        event_type: String,
    },
    /// Filed a misbehaviour report against a subject the attacker has no evidence about.
    ForgeReport {
        /// The subject framed.
        subject: String,
    },
    /// Put raw energy on the channel.
    TransmitRaw {
        /// The jammer profile: `constant`, `reactive`, `random-duty`.
        profile: String,
        /// Transmit power, dBm.
        power_dbm: f64,
    },
    /// Claimed to perceive objects that are not there (a phantom CPM).
    ForgeObject {
        /// How many phantom objects the message carries.
        count: u32,
    },
    /// Falsified an infrastructure message — a compromised road-side unit's SPaT, MAP,
    /// CRL or CTL.
    FalsifyInfrastructure {
        /// Which message: `spat`, `map`, `crl`, `ctl`.
        message: String,
        /// The field falsified.
        field: String,
        /// The size of the falsification in that field's units.
        magnitude: f64,
    },
    /// Dropped a misbehaviour report it was asked to forward, rather than a beacon.
    ///
    /// Distinct from [`AttackAction::Suppress`] because the thing suppressed is evidence
    /// on its way to the authority, not a frame on the air: it removes no bytes from the
    /// channel and it is the infrastructure half of report poisoning (07-threats §2.2).
    SuppressReport {
        /// The subject of the report that was dropped.
        subject: String,
    },
}

impl AttackAction {
    /// The action's name, as the `gt.attack.action` channel carries it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            AttackAction::FalsifyOutgoing { .. } => "FalsifyOutgoing",
            AttackAction::UseCredential { .. } => "UseCredential",
            AttackAction::Suppress => "Suppress",
            AttackAction::Delay { .. } => "Delay",
            AttackAction::Replay { .. } => "Replay",
            AttackAction::Ghost { .. } => "Ghost",
            AttackAction::ForgeEvent { .. } => "ForgeEvent",
            AttackAction::ForgeReport { .. } => "ForgeReport",
            AttackAction::TransmitRaw { .. } => "TransmitRaw",
            AttackAction::ForgeObject { .. } => "ForgeObject",
            AttackAction::FalsifyInfrastructure { .. } => "FalsifyInfrastructure",
            AttackAction::SuppressReport { .. } => "SuppressReport",
        }
    }

    /// The fields this action changed, for the ground-truth record.
    #[must_use]
    pub fn fields(&self) -> Vec<String> {
        match self {
            AttackAction::FalsifyOutgoing { fields, .. } => fields.clone(),
            AttackAction::UseCredential { .. } | AttackAction::Ghost { .. } => {
                vec!["signer".to_string()]
            }
            AttackAction::Delay { .. } | AttackAction::Replay { .. } => {
                vec!["generation_time".to_string()]
            }
            AttackAction::ForgeEvent { .. } => vec!["event".to_string()],
            AttackAction::ForgeObject { .. } => vec!["perceived".to_string()],
            AttackAction::FalsifyInfrastructure { message, field, .. } => {
                vec![format!("{message}.{field}")]
            }
            AttackAction::Suppress
            | AttackAction::ForgeReport { .. }
            | AttackAction::SuppressReport { .. }
            | AttackAction::TransmitRaw { .. } => Vec::new(),
        }
    }

    /// The magnitude of the action in the units of its principal field, when it has one.
    #[must_use]
    pub fn magnitude(&self) -> Option<f64> {
        match self {
            AttackAction::FalsifyOutgoing { magnitude, .. } => Some(*magnitude),
            AttackAction::Delay { seconds } => Some(*seconds),
            AttackAction::Replay { age_s } => Some(*age_s),
            AttackAction::TransmitRaw { power_dbm, .. } => Some(*power_dbm),
            AttackAction::ForgeObject { count } => Some(f64::from(*count)),
            AttackAction::FalsifyInfrastructure { magnitude, .. } => Some(*magnitude),
            AttackAction::UseCredential { .. }
            | AttackAction::Ghost { .. }
            | AttackAction::ForgeEvent { .. }
            | AttackAction::ForgeReport { .. }
            | AttackAction::SuppressReport { .. }
            | AttackAction::Suppress => None,
        }
    }

    /// Whether the action changed bytes on the air. Invariant I-T3 is about exactly
    /// these.
    ///
    /// The two suppression actions are the ones that did not: they removed bytes rather
    /// than changing them, and the record says so instead of claiming an air change that a
    /// byte-accounting invariant would then fail to find.
    ///
    /// [`AttackAction::ForgeReport`] *does* change bytes: a report is a real transmission
    /// over the reporting transport and costs the attacker its air time and its budget,
    /// which is what makes report flooding bounded rather than free.
    #[must_use]
    pub const fn changed_bytes_on_air(&self) -> bool {
        !matches!(
            self,
            AttackAction::Suppress | AttackAction::SuppressReport { .. }
        )
    }
}

/// A swappable attacker model (03-interfaces.md §9).
pub trait Attacker: Model {
    /// What this attacker is allowed to see and do. The engine enforces it.
    fn capabilities(&self) -> &Capabilities;

    /// When it acts.
    fn schedule(&self) -> &AttackSchedule;

    /// Lets the attacker update its state from what it can see.
    ///
    /// The default does nothing; an attacker with feedback — CRL-aware dormancy, a replay
    /// buffer, a coalition follower — overrides it.
    fn observe(&mut self, _ctx: &mut dyn ThreatCtx, _view: &AttackerView<'_>) {}

    /// Edits `out` in place and returns what it did.
    ///
    /// Called immediately before signing, so an edit here is an edit to the bytes that go
    /// on the air, and everything after it — signing cost, MAC, DCC — is the ordinary
    /// node path.
    fn act(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        view: &AttackerView<'_>,
        out: &mut Emission,
    ) -> Vec<AttackAction>;
}

/// Writes the ground-truth record for each action (invariant I-T3). **Host side.**
///
/// Called by the engine, which knows which actor carries this node. An attacker cannot
/// call it, because it has no [`ActorId`] to pass — see the module documentation.
pub fn log_actions(
    ctx: &mut dyn ThreatCtx,
    t: SimTime,
    actor: ActorId,
    attacker_model_id: &str,
    actions: &[AttackAction],
    msg: Option<u64>,
) {
    for a in actions {
        let mut fields = a.fields();
        fields.sort_unstable();
        ctx.emit(GtAttackAction {
            t,
            actor,
            attacker: attacker_model_id.to_string(),
            action: a.name().to_string(),
            fields,
            changed_bytes_on_air: a.changed_bytes_on_air(),
            msg,
            magnitude: a.magnitude().map(crate::records::q),
        });
    }
}

/// The per-message `falsified` label of 07-threats §4, from the honest claim and the
/// emission that went out. **Host side**, for the same reason as [`log_actions`]: the
/// label is ground truth.
///
/// The legacy rule verbatim (`run.py`, the broadcast pre-pass): position error > 1 m,
/// speed error > 1 m/s, heading error > 5°, more than one message, a generation time
/// earlier than now, a bad signature, a bad certificate, or a station type the sender is
/// not.
#[must_use]
pub fn is_falsified(
    honest: &HonestClaim,
    out: &Emission,
    now: SimTime,
    true_station_type: StationType,
) -> bool {
    let dpos = v2xw_core::math::hypot(out.x_m - honest.x_m, out.y_m - honest.y_m);
    let dspeed = (out.speed_mps - honest.speed_mps).abs();
    let dhead = crate::capability::angle_diff_rad(out.heading_rad, honest.heading_rad);
    let slack = v2xw_core::time::NS_PER_S;
    let cert_bad = now > out.cert_valid_to.saturating_add(slack)
        || now.saturating_add(slack) < out.cert_valid_from;
    dpos > 1.0
        || dspeed > 1.0
        || dhead > 5.0_f64.to_radians()
        || out.repetitions > 1
        || out.generation_time < now
        || !out.signature_valid
        || cert_bad
        || out.station_type != true_station_type
}

/// How many of the messages one [`Emission`] puts on the air the label rule calls
/// falsified. **Host side**, as [`is_falsified`] is.
///
/// [`is_falsified`] judges the beacon. Two kinds of message it does not cover are
/// falsified by construction, exactly as the legacy engine labelled them:
///
/// * a **ghost** transmission — an identity with no body behind it, which the legacy
///   engine emitted with `falsified=True` unconditionally;
/// * a **phantom event message** — a hazard announcement with no hazard, whose own
///   ground-truth stream carried the real/fake flag.
///
/// A Sybil attacker's own beacon is honest, so counting only the beacon would label a
/// Sybil run as carrying no falsification at all.
#[must_use]
pub fn falsified_count(
    honest: &HonestClaim,
    out: &Emission,
    now: SimTime,
    true_station_type: StationType,
) -> usize {
    let beacon = usize::from(is_falsified(honest, out, now, true_station_type));
    beacon + out.ghosts.len() + out.events.len() + usize::from(!out.perceived.is_empty())
}

/// The largest conforming application payload, bytes: the 802.11 maximum MSDU.
///
/// 04-models.md §4.6: 2 304 B, and `Phy::begin_tx` rejects a frame above it, so a
/// flooding attacker's largest legal SPDU is this. The value is the size cap the flood
/// aims at, not a threshold anybody chose.
pub const MAX_MSDU_BYTES: u32 = 2_304;

/// The label rule extended to the two new families whose falsification the frozen rule of
/// [`is_falsified`] has no term for. **Host side**, as [`is_falsified`] is.
///
/// 07-threats-and-detection.md §4 fixes the per-message `falsified` rule, and it was
/// written before §2.2's certificate-misuse-across-regions and oversized-message-flooding
/// families existed: neither edits a field the rule reads, so both would be labelled
/// honest and both would then be scored as detector false positives when a detector
/// caught them. [`is_falsified`] is left exactly as the legacy corpus needs it and the two
/// extra terms live here:
///
/// * the certificate states a region other than the one the sender is in
///   (IEEE 1609.2 `IdentifiedRegion`, ETSI authorization-ticket region); and
/// * the payload exceeds `max_payload_bytes` (default [`MAX_MSDU_BYTES`]).
///
/// `own_region` is the region the sender is actually in, which is ground truth — which is
/// why this function is host side and why no attacker and no detector may call it.
#[must_use]
pub fn is_falsified_extended(
    honest: &HonestClaim,
    out: &Emission,
    now: SimTime,
    true_station_type: StationType,
    own_region: Option<crate::obs::RegionId>,
    max_payload_bytes: u32,
) -> bool {
    let region_misuse = match (out.cert_region, own_region) {
        (Some(stated), Some(actual)) => stated != actual,
        // Nothing stated, or nowhere declared to compare against: no evidence either way,
        // and an absent input is not a falsification.
        _ => false,
    };
    let oversized = out.payload_bytes.is_some_and(|b| b > max_payload_bytes);
    is_falsified(honest, out, now, true_station_type) || region_misuse || oversized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legacy_catalog_is_the_frozen_order() {
        // run.py :: ATTACK_CATALOG, read out of the legacy source.
        let expected = [
            "ConstPos",
            "ConstPosOffset",
            "RandomPos",
            "Teleport",
            "SineWavePos",
            "ConstSpeedOffset",
            "RandomSpeed",
            "StopAndGo",
            "ReversedHeading",
            "HeadingOffset",
            "DataReplay",
            "SlowDrift",
            "AlongRoadOffset",
            "Sybil",
            "DoS",
            "DelayedMessages",
            "InvalidSignature",
            "ExpiredCert",
            "NotYetValid",
            "OutOfOrder",
            "DoSRandom",
        ];
        let got: Vec<&str> = AttackKind::LEGACY_CATALOG
            .iter()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn the_legacy_twenty_eight_are_all_present_and_parse_by_name() {
        let legacy_count = AttackKind::LEGACY_CATALOG.len()
            + AttackKind::COMBINED.len()
            + AttackKind::IDENTITY_SPOOF.len()
            + AttackKind::DENM.len();
        assert_eq!(legacy_count, 28);
        assert_eq!(AttackKind::ALL.len(), 29);
        for k in AttackKind::ALL {
            assert_eq!(AttackKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(AttackKind::parse("NotAnAttack"), None);
    }

    /// featurize.py :: _ATTACK_FAMILY, for the types this crate renders.
    #[test]
    fn families_match_the_legacy_feature_pipeline() {
        for (name, fam) in [
            ("ConstPos", "position"),
            ("SineWavePos", "position"),
            ("StopAndGo", "speed"),
            ("HeadingOffset", "heading"),
            ("EventualStop", "combined"),
            ("DoSRandom", "timing"),
            ("DataReplay", "timing"),
            ("SlowDrift", "stealth"),
            ("AlongRoadOffset", "stealth"),
            ("Sybil", "identity"),
            ("VruPositionSpoof", "identity"),
            ("ExpiredCert", "credential"),
            ("FakeHazard", "event"),
            ("SelectiveDrop", "suppression"),
        ] {
            assert_eq!(
                AttackKind::parse(name).unwrap().family().as_str(),
                fam,
                "{name}"
            );
        }
    }

    /// run.py :: _UNSCALABLE_MAGNITUDE_TYPES
    #[test]
    fn the_unscalable_types_are_the_legacy_five() {
        let unscalable: Vec<&str> = AttackKind::ALL
            .iter()
            .filter(|k| !k.is_magnitude_scalable())
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            unscalable,
            [
                "ConstPos",
                "ReversedHeading",
                "DataReplay",
                "VruImpersonation",
                "FakeHazard"
            ]
        );
    }

    #[test]
    fn suppression_is_the_one_action_that_does_not_change_bytes_on_the_air() {
        assert!(!AttackAction::Suppress.changed_bytes_on_air());
        for a in [
            AttackAction::Delay { seconds: 6.0 },
            AttackAction::Ghost { signer: [0; 8] },
            AttackAction::FalsifyOutgoing {
                fields: vec!["position".into()],
                magnitude: 25.0,
            },
        ] {
            assert!(a.changed_bytes_on_air());
        }
    }

    #[test]
    fn the_legacy_falsification_label_thresholds_hold() {
        let honest = HonestClaim {
            x_m: 0.0,
            y_m: 0.0,
            speed_mps: 10.0,
            heading_rad: 0.0,
        };
        let base = Emission::honest([0; 8], honest, 1_000_000_000, 0, 10_000_000_000);
        assert!(!is_falsified(
            &honest,
            &base,
            1_000_000_000,
            StationType::Vehicle
        ));

        let mut just_under = base.clone();
        just_under.x_m = 0.9;
        assert!(!is_falsified(
            &honest,
            &just_under,
            1_000_000_000,
            StationType::Vehicle
        ));
        let mut just_over = base.clone();
        just_over.x_m = 1.1;
        assert!(is_falsified(
            &honest,
            &just_over,
            1_000_000_000,
            StationType::Vehicle
        ));

        let mut speed = base.clone();
        speed.speed_mps = 11.5;
        assert!(is_falsified(
            &honest,
            &speed,
            1_000_000_000,
            StationType::Vehicle
        ));

        let mut heading = base.clone();
        heading.heading_rad = 6.0_f64.to_radians();
        assert!(is_falsified(
            &honest,
            &heading,
            1_000_000_000,
            StationType::Vehicle
        ));

        let mut vru = base.clone();
        vru.station_type = StationType::Vru;
        assert!(is_falsified(
            &honest,
            &vru,
            1_000_000_000,
            StationType::Vehicle
        ));

        let mut flood = base;
        flood.repetitions = 12;
        assert!(is_falsified(
            &honest,
            &flood,
            1_000_000_000,
            StationType::Vehicle
        ));
    }
}

#[cfg(test)]
mod extended_tests {
    use super::*;
    use crate::obs::{PerceivedObject, RegionId};

    fn honest() -> HonestClaim {
        HonestClaim {
            x_m: 0.0,
            y_m: 0.0,
            speed_mps: 10.0,
            heading_rad: 0.0,
        }
    }

    #[test]
    fn a_conforming_emission_declares_none_of_the_new_surfaces() {
        let e = Emission::honest([0; 8], honest(), 1_000_000_000, 0, 10_000_000_000);
        assert_eq!(e.payload_bytes, None);
        assert_eq!(e.cert_region, None);
        assert!(e.perceived.is_empty());
        assert!(e.raw_energy.is_none());
        assert!(!e.replayed);
        assert!(e.infra.is_empty());
        assert_eq!(e.messages_on_air(), 1);
    }

    #[test]
    fn the_frozen_label_rule_has_no_term_for_the_two_new_families() {
        // The point of `is_falsified_extended`: these two would otherwise be labelled
        // honest, and a detector that caught them would be scored a false positive.
        let h = honest();
        let mut region = Emission::honest([0; 8], h, 1_000_000_000, 0, 10_000_000_000);
        region.cert_region = Some(RegionId(276));
        assert!(!is_falsified(&h, &region, 1_000_000_000, StationType::Vehicle));
        assert!(is_falsified_extended(
            &h,
            &region,
            1_000_000_000,
            StationType::Vehicle,
            Some(RegionId(840)),
            MAX_MSDU_BYTES
        ));
        // Same region: not a misuse.
        assert!(!is_falsified_extended(
            &h,
            &region,
            1_000_000_000,
            StationType::Vehicle,
            Some(RegionId(276)),
            MAX_MSDU_BYTES
        ));
        // No declared own region: an absent input is not evidence.
        assert!(!is_falsified_extended(
            &h,
            &region,
            1_000_000_000,
            StationType::Vehicle,
            None,
            MAX_MSDU_BYTES
        ));

        let mut big = Emission::honest([0; 8], h, 1_000_000_000, 0, 10_000_000_000);
        big.payload_bytes = Some(MAX_MSDU_BYTES);
        assert!(
            !is_falsified_extended(
                &h,
                &big,
                1_000_000_000,
                StationType::Vehicle,
                None,
                MAX_MSDU_BYTES
            ),
            "the MSDU cap itself is legal"
        );
        big.payload_bytes = Some(MAX_MSDU_BYTES + 1);
        assert!(is_falsified_extended(
            &h,
            &big,
            1_000_000_000,
            StationType::Vehicle,
            None,
            MAX_MSDU_BYTES
        ));
    }

    #[test]
    fn a_phantom_perception_payload_is_counted_as_a_falsified_message() {
        let h = honest();
        let mut e = Emission::honest([0; 8], h, 1_000_000_000, 0, 10_000_000_000);
        assert_eq!(falsified_count(&h, &e, 1_000_000_000, StationType::Vehicle), 0);
        e.perceived
            .push(PerceivedObject::from_metres(1, 30.0, 0.0, 12.0, 10));
        e.perceived
            .push(PerceivedObject::from_metres(2, 60.0, 0.0, 12.0, 10));
        // One CPM, however many phantom objects it carries.
        assert_eq!(falsified_count(&h, &e, 1_000_000_000, StationType::Vehicle), 1);
    }

    #[test]
    fn suppressing_a_report_removes_no_bytes_from_the_air() {
        let a = AttackAction::SuppressReport {
            subject: "aabb".to_string(),
        };
        assert!(!a.changed_bytes_on_air());
        assert_eq!(a.name(), "SuppressReport");
        assert!(a.fields().is_empty());
        assert_eq!(a.magnitude(), None);
        // A forged report is a real transmission and is charged as one.
        assert!(
            AttackAction::ForgeReport {
                subject: "aabb".to_string()
            }
            .changed_bytes_on_air()
        );
    }

    #[test]
    fn the_new_actions_name_the_field_they_changed() {
        let o = AttackAction::ForgeObject { count: 4 };
        assert_eq!(o.fields(), vec!["perceived".to_string()]);
        assert_eq!(o.magnitude(), Some(4.0));
        let i = AttackAction::FalsifyInfrastructure {
            message: "spat".to_string(),
            field: "phase".to_string(),
            magnitude: 12.0,
        };
        assert_eq!(i.fields(), vec!["spat.phase".to_string()]);
        assert_eq!(i.magnitude(), Some(12.0));
        assert!(i.changed_bytes_on_air());
    }

    #[test]
    fn the_legacy_families_come_first_and_the_new_ones_are_appended() {
        assert_eq!(AttackFamily::ALL.len(), 14);
        for (i, f) in AttackFamily::LEGACY.into_iter().enumerate() {
            assert_eq!(AttackFamily::ALL[i], f);
        }
        assert_eq!(AttackFamily::Jamming.to_string(), "jamming");
        assert_eq!(AttackFamily::Privacy.as_str(), "privacy");
        assert_eq!(JamProfile::RandomDuty.to_string(), "random-duty");
    }
}
