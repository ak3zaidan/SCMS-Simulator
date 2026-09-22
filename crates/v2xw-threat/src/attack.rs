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
        }
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
        }
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
            AttackAction::Suppress
            | AttackAction::ForgeReport { .. }
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
            AttackAction::UseCredential { .. }
            | AttackAction::Ghost { .. }
            | AttackAction::ForgeEvent { .. }
            | AttackAction::ForgeReport { .. }
            | AttackAction::Suppress => None,
        }
    }

    /// Whether the action changed bytes on the air. Invariant I-T3 is about exactly
    /// these.
    ///
    /// [`AttackAction::Suppress`] is the one that did not: it removed bytes rather than
    /// changing them, and the record says so instead of claiming an air change that a
    /// byte-accounting invariant would then fail to find.
    #[must_use]
    pub const fn changed_bytes_on_air(&self) -> bool {
        !matches!(self, AttackAction::Suppress)
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
    beacon + out.ghosts.len() + out.events.len()
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
