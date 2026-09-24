//! The roadside-unit runtime: roles, failure states, and store-and-forward.
//!
//! # Why this is not a configuration of the vehicle runtime
//!
//! It was, and the code said so. `v2xw-engine`'s `build_rsu` put an
//! [`crate::runtime::ObuRuntime`] on an RSU hardware profile with
//! `ServiceSet { cam: false, bsm: false }` and a comment recording what was missing:
//! "06-node-models.md §3's RSU runtime — roles, failure states, a store-and-forward queue
//! — does not ship in `v2xw-node`. So the RSU receives, its detectors run, and it puts a
//! revocation list on the air when the backend hands it one."
//!
//! Everything the design specifies beyond that is *between* the antenna and the backend,
//! and a vehicle has none of it:
//!
//! * **roles** ([`RsuRole`]) — a unit broadcasts SPaT and MAP, or proxies certificate
//!   provisioning, or forwards misbehaviour reports, or distributes a revocation list, or
//!   several, and a scenario says which. A vehicle has no roles;
//! * **failure and compromise states** — `down`, `degraded` and `compromised` are states
//!   of a *mast with a backhaul*, and a vehicle that lost its backhaul lost nothing;
//! * **store-and-forward** ([`ForwardQueue`]) — a unit with no backhaul holds what it was
//!   given until it has one, drops it when it ages out, and reports both.
//!
//! Adding three optional subsystems to the vehicle runtime would have put them in the path
//! of every vehicle in every run, and the roles would have been an enum nobody read.
//!
//! # What is shared, and where the sharing lives
//!
//! The queue-and-server structure, the stores, the verification policies, the clock model,
//! the security stack and the telemetry record are all the same code: 06-node-models.md §3
//! says an RSU "has the same queue/server structure as the OBU with a larger profile", and
//! it does, because [`crate::queue`], [`crate::server`], [`crate::stores`],
//! [`crate::policy`], [`crate::clock`], [`crate::secure`] and [`crate::telemetry`] are
//! shared modules and not part of either runtime. What differs is this file.
//!
//! # A surveyed mast, not a fix
//!
//! An RSU knows where its own antenna is because a surveyor wrote it down, so
//! [`RsuRuntime::set_belief`] is handed a belief with no GNSS error in it — and the runtime
//! still reads it as a belief, because a compromised or misconfigured unit may hold a wrong
//! one and nothing here may consult the truth to find out ([`crate::firewall`]).
//!
//! # What it broadcasts, and what it cannot
//!
//! | Message | Cadence | Payload length |
//! |---|---|---|
//! | SPaT | 10 Hz (04-models.md §8.1's default, CTI 4501) | real octets from [`RsuRuntime::set_payload`], else `codec/size-model/j2735` |
//! | MAP | 1 Hz (ibid.) | as SPaT |
//! | CRL / CTL | on installation, and on [`RsuConfig::crl_repeat`] if a scenario sets one | the length the backend handed it |
//! | WSA | [`RsuConfig::wsa_period`], 1 per 5 s and UNVERIFIED | **none exists** |
//!
//! There is no WSA size model and no WSA encoder, so [`RsuConfig::wsa_payload_bytes`] is
//! `None` by default and a unit with the WSA role transmits nothing and counts it
//! ([`RsuRuntime::unsized_broadcasts`]). That is the shape 06-node-models.md §1 rule H1
//! demands of a missing number, applied to a missing message.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geo::GeoOrigin;
use v2xw_core::ids::NodeId;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::nodeview::NodeView;
use v2xw_core::time::{Duration, SimTime, WallClock};
use v2xw_msg::MsgType;
use v2xw_msg::generator::DccState;
use v2xw_msg::size_model::ContentProfile;
use v2xw_record::wire::telemetry::NodeTelemetry;

use crate::clock::ClockModel;
use crate::ctx::{NodeCtx, NodeCtxExt};
use crate::policy::{
    PolicyView, RxSummary, VerificationPolicy, VerifyAll, VerifyDecision,
    VerifyDecisionRecord,
};
use crate::profile::{HardwareProfile, RunsOn};
use crate::queue::{Admission, DropCause, DropLedger, NodeQueue, QueueKind, Queued};
use crate::runtime::{RxFrame, Transmission, VerifiedMessage};
use crate::secure::{CryptoMode, NodeSecurity, PSID_SAFETY, SpduVerdict};
use crate::server::{OpDescriptor, ProfileServiceModel, ServerBank, ServiceModel};
use crate::stores::{
    CredentialHandle, Neighbor, NeighborTable, PeerCertCache, Stores, VerificationState,
};
use crate::telemetry::{NodeState, TelemetryInputs, TelemetryWindow, gnss_fix_code};

/// Model id of the roadside-unit runtime.
///
/// Under [`Family::Generator`] for the reason [`crate::vru::VRU_DEVICE_ID`] gives: the
/// `Family` enum is closed (ADR 0007 Consequences), and what a scenario selects this model
/// for is the broadcast cadences and the forwarding rules. The backhaul is a separate
/// model with its own card and its own family — [`BACKHAUL_ID`] — because `Family::Backhaul`
/// exists and is documented as "RSU backhaul links".
pub const RSU_RUNTIME_ID: &str = "node/rsu";

/// Model id of the fixed-latency backhaul link of 04-models.md §10.2.
pub const BACKHAUL_ID: &str = "backhaul/fixed";

// =========================================================================================
// Roles
// =========================================================================================

/// One role a scenario can assign to a roadside unit (06-node-models.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RsuRole {
    /// CRL and CTL distribution proxy: puts the revocation list or the trust list on the
    /// air on the protocol's own distribution path.
    CrlDistribution,
    /// Certificate-provisioning proxy: forwards end-entity ↔ Registration Authority (or
    /// Enrolment/Authorization Authority) traffic over the backhaul.
    ProvisioningProxy,
    /// Report forwarding: carries a misbehaviour report from an end entity towards the
    /// Misbehaviour Authority.
    ReportForwarding,
    /// SPaT and MAP broadcaster, at the rates of 04-models.md §8.1.
    SpatMapBroadcast,
    /// WSA broadcaster (IEEE 1609.3).
    WsaBroadcast,
    /// Detector host: the same local detector plug-ins a vehicle runs, run at the mast.
    DetectorHost,
}

impl RsuRole {
    /// Every role, in declaration order — which is also the order [`RsuRoles::iter`]
    /// yields them in, so a report's role list is the same on every run.
    pub const ALL: [RsuRole; 6] = [
        RsuRole::CrlDistribution,
        RsuRole::ProvisioningProxy,
        RsuRole::ReportForwarding,
        RsuRole::SpatMapBroadcast,
        RsuRole::WsaBroadcast,
        RsuRole::DetectorHost,
    ];

    /// The scenario's spelling. These are the strings `v2xw-engine`'s `RsuSpec::roles`
    /// already carries — `"crl"` and `"report-forward"` appear in the Phase 2 wiring — so
    /// [`RsuRoles::parse`] accepts them and a scenario written against the old wiring
    /// keeps working.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RsuRole::CrlDistribution => "crl",
            RsuRole::ProvisioningProxy => "provisioning-proxy",
            RsuRole::ReportForwarding => "report-forward",
            RsuRole::SpatMapBroadcast => "spat-map",
            RsuRole::WsaBroadcast => "wsa",
            RsuRole::DetectorHost => "detector-host",
        }
    }

    /// The bit this role occupies in an [`RsuRoles`] set.
    const fn bit(self) -> u8 {
        match self {
            RsuRole::CrlDistribution => 1 << 0,
            RsuRole::ProvisioningProxy => 1 << 1,
            RsuRole::ReportForwarding => 1 << 2,
            RsuRole::SpatMapBroadcast => 1 << 3,
            RsuRole::WsaBroadcast => 1 << 4,
            RsuRole::DetectorHost => 1 << 5,
        }
    }

    /// The role a scenario string names, if it names one.
    #[must_use]
    pub fn parse(name: &str) -> Option<RsuRole> {
        RsuRole::ALL.into_iter().find(|r| r.as_str() == name)
    }
}

/// The set of roles one unit carries.
///
/// A bit set rather than a `Vec<RsuRole>` or a `HashSet`: membership is a constant-time
/// test on a path that runs once per broadcast timer per unit per step, iteration is in
/// declaration order by construction, and there is no hash iteration order for a report to
/// inherit (02-architecture.md §6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, serde::Serialize)]
pub struct RsuRoles(u8);

impl RsuRoles {
    /// No roles: a unit that receives and does nothing else, which is what the Phase 2
    /// wiring's RSU was.
    pub const NONE: RsuRoles = RsuRoles(0);

    /// Every role.
    #[must_use]
    pub const fn all() -> RsuRoles {
        RsuRoles(0b0011_1111)
    }

    /// The set with `role` added.
    #[must_use]
    pub const fn with(self, role: RsuRole) -> RsuRoles {
        RsuRoles(self.0 | role.bit())
    }

    /// The set with `role` removed.
    #[must_use]
    pub const fn without(self, role: RsuRole) -> RsuRoles {
        RsuRoles(self.0 & !role.bit())
    }

    /// Whether this unit carries `role`.
    #[must_use]
    pub const fn contains(self, role: RsuRole) -> bool {
        self.0 & role.bit() != 0
    }

    /// Whether no role is assigned.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// How many roles are assigned.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// The roles, in [`RsuRole::ALL`] order.
    #[must_use]
    pub fn iter(self) -> impl Iterator<Item = RsuRole> {
        RsuRole::ALL.into_iter().filter(move |r| self.contains(*r))
    }

    /// The roles' scenario spellings, in declaration order.
    #[must_use]
    pub fn names(self) -> Vec<&'static str> {
        self.iter().map(RsuRole::as_str).collect()
    }

    /// The set a scenario's list of strings names, and the strings that named nothing.
    ///
    /// The unknown names are returned rather than ignored: a scenario that asked for
    /// `"spat"` and got a silently role-less unit is the failure mode a loader must be able
    /// to report, and a runtime that swallowed the typo would make it invisible.
    #[must_use]
    pub fn parse<S: AsRef<str>>(names: &[S]) -> (RsuRoles, Vec<String>) {
        let mut set = RsuRoles::NONE;
        let mut unknown = Vec::new();
        for n in names {
            match RsuRole::parse(n.as_ref()) {
                Some(r) => set = set.with(r),
                None => unknown.push(n.as_ref().to_string()),
            }
        }
        (set, unknown)
    }
}

// =========================================================================================
// Backhaul
// =========================================================================================

/// What a roadside unit's backhaul is made of (06-node-models.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackhaulKind {
    /// Fibre or Ethernet to the operator's network.
    Fibre,
    /// A cellular modem: "an RSU on cellular backhaul is a UE" (04-models.md §10.2).
    Cellular,
    /// None at all. A unit with no backhaul can only broadcast what it already holds, and
    /// everything it is handed for the backend goes into store-and-forward.
    None,
}

impl BackhaulKind {
    /// The stable name a card and a scenario spell it with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BackhaulKind::Fibre => "fibre",
            BackhaulKind::Cellular => "cellular",
            BackhaulKind::None => "none",
        }
    }
}

/// `backhaul/fixed` — one link's one-way latency and capacity (04-models.md §10.2,
/// abstract tier).
///
/// The two shipped presets are the design set's own figures, and both are one-way:
/// 04-models.md §10.1 states explicitly that the cellular round trip is "one way modeled
/// as half".
#[derive(Debug, Clone)]
pub struct Backhaul {
    kind: BackhaulKind,
    latency: Duration,
    bandwidth_bps: Option<u64>,
    degraded_multiplier: u32,
    card: ModelCard,
}

/// The multiplier [`NodeState::Degraded`] applies to the backhaul's latency.
///
/// **Uncalibrated.** 06-node-models.md §3 specifies the state as "`degraded` (backhaul
/// latency multiplier)" and gives no multiplier. Ten is an order of magnitude, chosen so
/// that a degraded link is unmistakable in a latency plot rather than plausible; the card
/// carries the plan.
pub const DEFAULT_DEGRADED_MULTIPLIER: u32 = 10;

impl Default for Backhaul {
    fn default() -> Backhaul {
        Backhaul::fibre()
    }
}

impl Backhaul {
    /// A fibre or Ethernet link: 0.402 ms one way.
    ///
    /// 04-models.md §10.2 says fibre and microwave take "the transport-network figures
    /// above (0.4-2.4 ms mean)", and §10.1's table gives the fastest of them — MEC at the
    /// gNB, 0.402 ms mean, 0.422 ms at the 99.99th percentile [Coll-Perales 2022 Table VI].
    /// The faster end is the default because an RSU on fibre is the short-haul case; a
    /// scenario modelling a link to a central data centre sets 2.355 ms instead.
    ///
    /// Capacity is `None`: no published figure gives the capacity of an RSU's fibre drop,
    /// so the serialisation term is not modelled rather than guessed, and the card says so.
    #[must_use]
    pub fn fibre() -> Backhaul {
        Backhaul::new(BackhaulKind::Fibre, Duration::from_nanos(402_000), None)
    }

    /// A cellular link: 14.6 ms one way, 28 Mbit/s.
    ///
    /// Half of the 29.2 ms LTE first-hop round trip [Narayanan et al. WWW'20 Table 2,
    /// R11 §A1], which 04-models.md §10.1 tabulates with the note "one way modeled as
    /// half". The capacity is the MOSAIC example uplink cap of 28 Mbit/s [R11 §A7], which
    /// 04-models.md §10.1 lists "for comparison" — a worked example rather than a
    /// measurement, and flagged as such on the card.
    #[must_use]
    pub fn cellular() -> Backhaul {
        Backhaul::new(
            BackhaulKind::Cellular,
            Duration::from_nanos(14_600_000),
            Some(28_000_000),
        )
    }

    /// No backhaul.
    #[must_use]
    pub fn none() -> Backhaul {
        Backhaul::new(BackhaulKind::None, Duration::ZERO, None)
    }

    /// A link of `kind` with a one-way `latency` and an optional capacity.
    #[must_use]
    pub fn new(kind: BackhaulKind, latency: Duration, bandwidth_bps: Option<u64>) -> Backhaul {
        let card = backhaul_card(kind, latency, bandwidth_bps);
        Backhaul {
            kind,
            latency,
            bandwidth_bps,
            degraded_multiplier: DEFAULT_DEGRADED_MULTIPLIER,
            card,
        }
    }

    /// The same link with a different degraded multiplier.
    #[must_use]
    pub fn with_degraded_multiplier(mut self, k: u32) -> Backhaul {
        self.degraded_multiplier = k.max(1);
        self
    }

    /// What the link is made of.
    #[must_use]
    pub fn kind(&self) -> BackhaulKind {
        self.kind
    }

    /// Its one-way latency in the nominal state.
    #[must_use]
    pub fn latency(&self) -> Duration {
        self.latency
    }

    /// Its capacity, where one is modelled.
    #[must_use]
    pub fn bandwidth_bps(&self) -> Option<u64> {
        self.bandwidth_bps
    }

    /// The multiplier [`NodeState::Degraded`] applies.
    #[must_use]
    pub fn degraded_multiplier(&self) -> u32 {
        self.degraded_multiplier
    }

    /// Whether the link carries anything in `state`.
    ///
    /// `Down` loses the backhaul, which is 06-node-models.md §3's own definition of the
    /// state ("no transmissions, backhaul lost"); `Compromised` is up, because an attacker
    /// controlling a unit's forwarding needs a link to forward over.
    #[must_use]
    pub fn is_up(&self, state: NodeState) -> bool {
        !matches!(self.kind, BackhaulKind::None)
            && !matches!(state, NodeState::Down | NodeState::Off)
    }

    /// The one-way delay `bytes` take, in `state`.
    ///
    /// `latency × multiplier + bytes·8 / bandwidth`, with the serialisation term omitted
    /// when no capacity is modelled. The multiplier applies to the propagation term only:
    /// a degraded link is a slower path, not a narrower pipe, and 06-node-models.md §3
    /// names it a *latency* multiplier.
    #[must_use]
    pub fn delay(&self, bytes: u32, state: NodeState) -> Duration {
        let multiplier = if matches!(state, NodeState::Degraded) {
            u64::from(self.degraded_multiplier)
        } else {
            1
        };
        let propagation = self.latency.saturating_mul(multiplier);
        match self.bandwidth_bps {
            Some(bps) if bps > 0 => {
                let serial_ns = (u64::from(bytes) * 8).saturating_mul(1_000_000_000) / bps;
                Duration::from_nanos(propagation.as_nanos().saturating_add(serial_ns))
            }
            _ => propagation,
        }
    }
}

impl Model for Backhaul {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

// =========================================================================================
// Store-and-forward
// =========================================================================================

/// What a roadside unit is holding for the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForwardKind {
    /// A misbehaviour report on its way to the Misbehaviour Authority
    /// ([`RsuRole::ReportForwarding`]).
    MisbehaviourReport,
    /// An end entity's certificate request on its way to the Registration Authority
    /// ([`RsuRole::ProvisioningProxy`]).
    ProvisioningRequest,
    /// The backend's answer on its way back to the end entity.
    ProvisioningResponse,
    /// A trust-list or revocation-list update the unit is to broadcast.
    TrustListUpdate,
}

impl ForwardKind {
    /// Every kind, in declaration order.
    pub const ALL: [ForwardKind; 4] = [
        ForwardKind::MisbehaviourReport,
        ForwardKind::ProvisioningRequest,
        ForwardKind::ProvisioningResponse,
        ForwardKind::TrustListUpdate,
    ];

    /// The stable name a record and a report spell it with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ForwardKind::MisbehaviourReport => "misbehaviour-report",
            ForwardKind::ProvisioningRequest => "provisioning-request",
            ForwardKind::ProvisioningResponse => "provisioning-response",
            ForwardKind::TrustListUpdate => "trust-list-update",
        }
    }

    /// Which way it travels: `true` towards the backend, `false` towards the air.
    #[must_use]
    pub const fn is_uplink(self) -> bool {
        matches!(
            self,
            ForwardKind::MisbehaviourReport | ForwardKind::ProvisioningRequest
        )
    }
}

/// One item waiting in store-and-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardItem {
    /// What it is.
    pub kind: ForwardKind,
    /// Its size, bytes — which is what it costs on the backhaul.
    pub bytes: u32,
    /// When the unit took custody of it, on the unit's own clock.
    pub stored_at: SimTime,
    /// When it left, on the unit's own clock. `None` while it is still waiting.
    pub forwarded_at: Option<SimTime>,
    /// When it will arrive at the far end, once it has left.
    pub arrives_at: Option<SimTime>,
}

/// The age after which a held item is discarded.
///
/// One week, which is [CAMP-EE §2.2.8]'s rule for an end entity's own report outbox —
/// "EE may delete unsent reports older than 1 week", restated in 05-protocols.md §2.6 —
/// applied to a relay holding the same reports. No clause fixes a *relay's* retention, so
/// the end entity's is reused rather than a second number invented, and the card says so.
pub const FORWARD_MAX_AGE: Duration = crate::stores::REPORT_MAX_AGE;

/// A bounded queue of items waiting for a backhaul (06-node-models.md §3, §5).
///
/// 06-node-models.md §5 specifies the behaviour for the end entity — "vehicles out of
/// coverage keep reports and top-up requests in store-and-forward until coverage or an RSU
/// is available" — and §3 gives the roadside unit the same job one hop further on. Both
/// halves bound the queue and drop by age: an unbounded relay buffer turns a week-long
/// outage into a memory leak, and a relay that silently forgot would make a report's
/// disappearance unattributable.
#[derive(Debug, Clone)]
pub struct ForwardQueue {
    items: Vec<ForwardItem>,
    capacity: usize,
    max_age: Duration,
    dropped_overflow: u64,
    dropped_aged: u64,
    forwarded: u64,
    bytes_forwarded: u64,
}

impl Default for ForwardQueue {
    fn default() -> ForwardQueue {
        ForwardQueue::new(256, FORWARD_MAX_AGE)
    }
}

impl ForwardQueue {
    /// A queue of `capacity` items discarding anything older than `max_age`.
    #[must_use]
    pub fn new(capacity: usize, max_age: Duration) -> ForwardQueue {
        ForwardQueue {
            items: Vec::new(),
            capacity: capacity.max(1),
            max_age,
            dropped_overflow: 0,
            dropped_aged: 0,
            forwarded: 0,
            bytes_forwarded: 0,
        }
    }

    /// How many items are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many items it holds at most.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Bytes waiting.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.items.iter().map(|i| u64::from(i.bytes)).sum()
    }

    /// Items waiting, oldest first.
    #[must_use]
    pub fn items(&self) -> &[ForwardItem] {
        &self.items
    }

    /// How many items were discarded because the queue was full.
    #[must_use]
    pub const fn dropped_overflow(&self) -> u64 {
        self.dropped_overflow
    }

    /// How many items were discarded because they aged out.
    #[must_use]
    pub const fn dropped_aged(&self) -> u64 {
        self.dropped_aged
    }

    /// How many items eventually went out.
    #[must_use]
    pub const fn forwarded(&self) -> u64 {
        self.forwarded
    }

    /// How many bytes eventually went out.
    #[must_use]
    pub const fn bytes_forwarded(&self) -> u64 {
        self.bytes_forwarded
    }

    /// Takes custody of an item.
    ///
    /// The **oldest** is discarded on overflow, not the newcomer: a report held for six
    /// days is the one least likely still to matter, and dropping the arrival would make a
    /// full queue permanently deaf. It is the same rule [`crate::queue::NodeQueue`]'s
    /// `push_evicting` applies, and the same reason.
    pub fn store(&mut self, kind: ForwardKind, bytes: u32, at: SimTime) {
        self.items.push(ForwardItem {
            kind,
            bytes,
            stored_at: at,
            forwarded_at: None,
            arrives_at: None,
        });
        while self.items.len() > self.capacity {
            self.items.remove(0);
            self.dropped_overflow = self.dropped_overflow.saturating_add(1);
        }
    }

    /// Discards everything older than `max_age` at `now`, returning how many went.
    pub fn sweep(&mut self, now: SimTime) -> usize {
        let before = self.items.len();
        let max_age = self.max_age;
        self.items
            .retain(|i| Duration::between(i.stored_at, now) <= max_age);
        let gone = before - self.items.len();
        self.dropped_aged = self.dropped_aged.saturating_add(gone as u64);
        gone
    }

    /// Hands every **uplink** item over to a link that is up, stamping each item's
    /// departure and arrival.
    ///
    /// Uplink only, because the queue holds both directions and only one of them travels
    /// this way: a report or a certificate request goes to the backend over the backhaul,
    /// while a provisioning answer or a trust-list update goes to the air, and draining
    /// the two the same way would send the backend its own CRL back.
    /// [`ForwardQueue::take_downlink`] is the other half.
    ///
    /// Nothing is handed over when the link is down: the items stay, which is the whole
    /// point of the queue. The departures are all stamped at `now` — the abstract-tier
    /// reading of 04-models.md §10.2, where a link has a latency and a capacity and no
    /// scheduler — and each arrival is `now + backhaul.delay(bytes, state)`, so a
    /// capacity-limited link's serialisation term still shows.
    pub fn drain_to(
        &mut self,
        now: SimTime,
        backhaul: &Backhaul,
        state: NodeState,
    ) -> Vec<ForwardItem> {
        if !backhaul.is_up(state) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut kept = Vec::new();
        for mut item in self.items.drain(..) {
            if !item.kind.is_uplink() {
                kept.push(item);
                continue;
            }
            item.forwarded_at = Some(now);
            item.arrives_at = Some(backhaul.delay(item.bytes, state).after(now));
            self.forwarded = self.forwarded.saturating_add(1);
            self.bytes_forwarded = self.bytes_forwarded.saturating_add(u64::from(item.bytes));
            out.push(item);
        }
        self.items = kept;
        out
    }

    /// Takes every **downlink** item, for the caller to put on the air.
    ///
    /// The air path is the engine's: a provisioning answer or a trust-list update reaches
    /// an end entity as a frame on a channel, and a node does not schedule its own
    /// transmissions past the queue. Each item is stamped as having left at `now`, with no
    /// arrival — the arrival is the radio's answer, not this queue's.
    pub fn take_downlink(&mut self, now: SimTime) -> Vec<ForwardItem> {
        let mut out = Vec::new();
        let mut kept = Vec::new();
        for mut item in self.items.drain(..) {
            if item.kind.is_uplink() {
                kept.push(item);
                continue;
            }
            item.forwarded_at = Some(now);
            self.forwarded = self.forwarded.saturating_add(1);
            self.bytes_forwarded = self.bytes_forwarded.saturating_add(u64::from(item.bytes));
            out.push(item);
        }
        self.items = kept;
        out
    }
}

// =========================================================================================
// Broadcast timers
// =========================================================================================

/// A periodic broadcast timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Periodic {
    period: Duration,
    last: Option<SimTime>,
}

impl Periodic {
    const fn new(period: Duration) -> Periodic {
        Periodic { period, last: None }
    }

    /// Whether the timer is due at `now`, honouring a DCC floor.
    fn due(&mut self, now: SimTime, dcc: &DccState) -> bool {
        let interval = self.period.max(dcc.t_off);
        match self.last {
            Some(last) if Duration::between(last, now) < interval => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

// =========================================================================================
// Configuration
// =========================================================================================

/// How a roadside unit is configured.
#[derive(Debug, Clone)]
pub struct RsuConfig {
    /// Which roles it carries.
    pub roles: RsuRoles,
    /// Queue capacities, in [`QueueKind::ALL`] order.
    pub queue_capacity: [usize; 5],
    /// How many peer certificates it caches. Larger than a vehicle's by default: a mast
    /// hears every vehicle that passes it, and the design gives it "a larger profile".
    pub peer_cache_capacity: usize,
    /// How many neighbours it tracks.
    pub neighbor_capacity: usize,
    /// How many items store-and-forward holds.
    pub forward_capacity: usize,
    /// How long between telemetry frames.
    pub telemetry_period: Duration,
    /// Transmit power, dBm.
    pub tx_power_dbm: f64,
    /// The primitive the signing cost table is keyed by.
    pub sign_op: &'static str,
    /// The verification primitive.
    pub verify_op: &'static str,
    /// The scenario's wall clock.
    pub wall: WallClock,
    /// The geodetic anchor of the world's ENU frame.
    pub origin: GeoOrigin,
    /// Which crypto backend signs and verifies.
    pub crypto_mode: CryptoMode,
    /// The PSID an infrastructure message is signed under.
    pub psid: u64,
    /// The SPaT interval. 100 ms — 04-models.md §8.1's "Defaults: SPaT 10 Hz".
    pub spat_period: Duration,
    /// How many movement states a SPaT carries, for the size model's per-element term.
    /// `None` takes the size model's own nominal count for the chosen profile.
    pub spat_movement_states: Option<u32>,
    /// The MAP interval. 1,000 ms — ibid., "MAP 1 Hz".
    pub map_period: Duration,
    /// How many lanes a MAP carries, for the size model's per-element term. `None` as
    /// [`RsuConfig::spat_movement_states`].
    pub map_lanes: Option<u32>,
    /// Which content profile the size model is asked for.
    pub content_profile: ContentProfile,
    /// The WSA interval. 5 s, and UNVERIFIED (04-models.md §8.1).
    pub wsa_period: Duration,
    /// The WSA payload length. `None` — there is no WSA encoder and no WSA size-model row.
    pub wsa_payload_bytes: Option<u32>,
    /// How often an installed revocation or trust list is re-broadcast. `None` broadcasts
    /// it once when it is installed and then only on update.
    pub crl_repeat: Option<Duration>,
}

impl Default for RsuConfig {
    fn default() -> RsuConfig {
        RsuConfig {
            roles: RsuRoles::NONE,
            // Four times the vehicle's rx and verify depth, because a mast at a junction
            // hears every approach at once. Engine defaults, not device figures: every one
            // of the ten profiles carries `hsm.queue_depth` as uncalibrated.
            queue_capacity: [256, 256, 256, 64, 32],
            peer_cache_capacity: 512,
            neighbor_capacity: 1_024,
            forward_capacity: 1_024,
            telemetry_period: Duration::from_secs(1),
            tx_power_dbm: 20.0,
            sign_op: "ecdsa-p256-sign",
            verify_op: "ecdsa-p256-verify",
            wall: WallClock::default(),
            origin: GeoOrigin::new(0.0, 0.0, 0.0),
            crypto_mode: CryptoMode::Modeled,
            psid: PSID_SAFETY,
            spat_period: Duration::from_millis(100),
            // `None`: the size model's own nominal element count for the chosen profile,
            // so a scenario that says nothing broadcasts the message the size model was
            // validated at rather than a different one.
            spat_movement_states: None,
            map_period: Duration::from_secs(1),
            map_lanes: None,
            content_profile: ContentProfile::Typical,
            wsa_period: Duration::from_secs(5),
            wsa_payload_bytes: None,
            crl_repeat: None,
        }
    }
}

impl RsuConfig {
    /// The configuration for a unit carrying `roles`.
    #[must_use]
    pub fn with_roles(roles: RsuRoles) -> RsuConfig {
        RsuConfig {
            roles,
            ..RsuConfig::default()
        }
    }
}

/// What one roadside-unit step produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RsuStepOutcome {
    /// Frames for the network layer, in broadcast order.
    pub transmissions: Vec<Transmission>,
    /// Messages delivered to the unit's applications and detectors, in arrival order.
    pub delivered: Vec<VerifiedMessage>,
    /// Items that went over the backhaul this step, with their departure and arrival
    /// instants.
    ///
    /// The unit does not record them: a forwarded item belongs to a protocol flow, and the
    /// `proto.msg` record needs the `FlowId` and `FlowRun` that only the protocol knows
    /// (`v2xw-proto`'s `WireStep`). The engine dispatches these and records them there,
    /// which is also invariant I-P1 — every `Send` crosses a modelled link — honoured on
    /// the one hop a node owns.
    pub forwarded: Vec<ForwardItem>,
    /// The telemetry record, when this step closed a window.
    pub telemetry: Option<NodeTelemetry>,
}

// =========================================================================================
// The runtime
// =========================================================================================

/// A roadside unit: a fixed antenna, a backhaul, a set of roles and a failure state.
pub struct RsuRuntime {
    node: NodeId,
    config: RsuConfig,
    service: ProfileServiceModel,
    cpu: ServerBank,
    hsm: ServerBank,
    accel: ServerBank,
    queues: [NodeQueue<Queued<RxFrame>>; 5],
    drops: DropLedger,
    clock: ClockModel,
    belief: PositionEstimate,
    stores: Stores,
    policy: Box<dyn VerificationPolicy>,
    state: NodeState,
    backhaul: Backhaul,
    forward: ForwardQueue,
    spat: Periodic,
    map: Periodic,
    wsa: Periodic,
    crl: Periodic,
    /// The length of the revocation or trust list the backend installed, if any.
    crl_payload_bytes: Option<u32>,
    /// Whether an installed list still owes its first broadcast.
    crl_pending: bool,
    received: Vec<VerifiedMessage>,
    evidence_capacity: usize,
    window: TelemetryWindow,
    dcc: DccState,
    dcc_state_code: u16,
    security: NodeSecurity,
    card: ModelCard,
    /// Payload octets a scenario or the engine supplied, by message type, in
    /// [`MsgType`] order. A `Vec` of pairs rather than a map: at most four entries, and it
    /// keeps the iteration order a report sees fixed without a hash container.
    payloads: Vec<(MsgType, Vec<u8>)>,
    /// Broadcasts a role asked for and no size model could size.
    unsized_broadcasts: u64,
    /// Broadcasts whose payload was a size-model placeholder rather than encoder output.
    modelled_broadcasts: u64,
    /// Broadcasts and forwards withheld because an attacker controls this unit.
    compromised_suppressed: u64,
    /// Reports and requests taken off the air for the backend.
    uplink_accepted: u64,
    /// The §3.5.2 field marked **GT**, handed in from outside the firewall by
    /// [`RsuRuntime::observe_truth`] and read by nothing but the telemetry path.
    gt_pos_error_m: f32,
}

impl core::fmt::Debug for RsuRuntime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RsuRuntime")
            .field("node", &self.node)
            .field("profile", &self.service.profile().id)
            .field("roles", &self.config.roles.names())
            .field("state", &self.state)
            .field("backhaul", &self.backhaul.kind())
            .field("held", &self.forward.len())
            .finish_non_exhaustive()
    }
}

impl Model for RsuRuntime {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl RsuRuntime {
    /// A unit on `profile` with `config` and `backhaul`, starting at `at`.
    #[must_use]
    pub fn new(
        node: NodeId,
        profile: HardwareProfile,
        config: RsuConfig,
        backhaul: Backhaul,
        at: SimTime,
    ) -> RsuRuntime {
        let cpu_servers = profile.cpu.cores.or(1).max(1);
        let hsm_servers = profile.hsm.servers.or(1).max(1);
        let service = ProfileServiceModel::new(profile.clone());
        let stores = Stores {
            peers: PeerCertCache::new(config.peer_cache_capacity),
            neighbors: NeighborTable::new(config.neighbor_capacity),
            ..Default::default()
        };
        let card = card(&profile, &config, &backhaul);
        RsuRuntime {
            node,
            cpu: ServerBank::new("cpu", cpu_servers, at),
            hsm: ServerBank::new("hsm", hsm_servers, at),
            accel: ServerBank::new("accel", 1, at),
            queues: [
                NodeQueue::new(QueueKind::Rx, config.queue_capacity[0]),
                NodeQueue::new(QueueKind::Verify, config.queue_capacity[1]),
                NodeQueue::new(QueueKind::App, config.queue_capacity[2]),
                NodeQueue::new(QueueKind::Tx, config.queue_capacity[3]),
                NodeQueue::new(QueueKind::Crl, config.queue_capacity[4]),
            ],
            drops: DropLedger::new(),
            // A surveyed mast has a perfect clock as often as it has a bad one; the drift
            // is the scenario's to set through `clock_mut`, and zero is the honest default
            // for a unit with a wired time source rather than an invented ppm figure.
            clock: ClockModel::new(0.0),
            belief: PositionEstimate::no_fix(at),
            stores,
            // `verify-all`, whatever the vehicles run: a unit that forwards a misbehaviour
            // report has to have verified the report it forwards, and `prioritized` would
            // skip a distant sender — which is every sender, at a mast.
            policy: Box::new(VerifyAll::new()),
            state: NodeState::Active,
            forward: ForwardQueue::new(config.forward_capacity, FORWARD_MAX_AGE),
            spat: Periodic::new(config.spat_period),
            map: Periodic::new(config.map_period),
            wsa: Periodic::new(config.wsa_period),
            crl: Periodic::new(config.crl_repeat.unwrap_or(Duration::MAX)),
            crl_payload_bytes: None,
            crl_pending: false,
            received: Vec::new(),
            evidence_capacity: 512,
            window: TelemetryWindow::new(at),
            dcc: DccState::UNRESTRICTED,
            dcc_state_code: v2xw_record::wire::U16_NONE,
            security: NodeSecurity::new(config.wall, config.crypto_mode, config.psid),
            card,
            payloads: Vec::new(),
            unsized_broadcasts: 0,
            modelled_broadcasts: 0,
            compromised_suppressed: 0,
            uplink_accepted: 0,
            gt_pos_error_m: f32::NAN,
            backhaul,
            service,
            config,
        }
    }

    /// A unit on the default roadside profile with `roles` and a fibre backhaul.
    ///
    /// # Panics
    /// Never in a built crate: the profile is compiled in by
    /// [`crate::profiles::PROFILE_SOURCES`].
    #[must_use]
    pub fn with_roles(node: NodeId, roles: RsuRoles, at: SimTime) -> RsuRuntime {
        let profile = crate::profiles::get(crate::profiles::DEFAULT_RSU)
            .expect("the default roadside profile ships with the crate")
            .clone();
        RsuRuntime::new(
            node,
            profile,
            RsuConfig::with_roles(roles),
            Backhaul::fibre(),
            at,
        )
    }

    /// The hardware profile.
    #[must_use]
    pub fn profile(&self) -> &HardwareProfile {
        self.service.profile()
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &RsuConfig {
        &self.config
    }

    /// The roles this unit carries.
    #[must_use]
    pub fn roles(&self) -> RsuRoles {
        self.config.roles
    }

    /// The backhaul.
    #[must_use]
    pub fn backhaul(&self) -> &Backhaul {
        &self.backhaul
    }

    /// Replaces the backhaul — how a scenario event moves a unit from fibre to cellular.
    pub fn set_backhaul(&mut self, backhaul: Backhaul) {
        self.backhaul = backhaul;
    }

    /// The store-and-forward queue.
    #[must_use]
    pub fn forward_queue(&self) -> &ForwardQueue {
        &self.forward
    }

    /// The unit's stores.
    #[must_use]
    pub fn stores(&self) -> &Stores {
        &self.stores
    }

    /// The stores, mutably, for the engine's provisioning and CRL paths.
    pub fn stores_mut(&mut self) -> &mut Stores {
        &mut self.stores
    }

    /// The security stack.
    #[must_use]
    pub fn security(&self) -> &NodeSecurity {
        &self.security
    }

    /// The security stack, mutably.
    pub fn security_mut(&mut self) -> &mut NodeSecurity {
        &mut self.security
    }

    /// The unit's clock model.
    #[must_use]
    pub fn clock(&self) -> &ClockModel {
        &self.clock
    }

    /// The clock model, mutably.
    pub fn clock_mut(&mut self) -> &mut ClockModel {
        &mut self.clock
    }

    /// What state the unit is in.
    #[must_use]
    pub fn state(&self) -> NodeState {
        self.state
    }

    /// Moves the unit to another state — the failure and compromise transitions of
    /// 06-node-models.md §3.
    pub fn set_state(&mut self, state: NodeState) {
        self.state = state;
    }

    /// Replaces the verification policy.
    pub fn set_policy(&mut self, policy: Box<dyn VerificationPolicy>) {
        self.policy = policy;
    }

    /// Hands the unit its surveyed position.
    pub fn set_belief(&mut self, belief: PositionEstimate) {
        self.belief = belief;
    }

    /// Sets the DCC state the broadcast timers honour.
    pub fn set_dcc(&mut self, dcc: DccState, state_code: u16) {
        self.dcc = dcc;
        self.dcc_state_code = state_code;
    }

    /// Hands in the ground-truth position error §3.5.2 asks for, from outside the
    /// firewall. Written into the telemetry path and read by nothing else.
    pub fn observe_truth(&mut self, pos_error_m: f32) {
        self.gt_pos_error_m = pos_error_m;
    }

    /// Installs a revocation or trust list of `bytes` for broadcast.
    ///
    /// The list's length is the backend's, not this unit's: a CRL's size is the protocol's
    /// (`v2xw-proto`'s CRL wire table) and inventing one here would put a made-up number
    /// on the air.
    ///
    /// Custody is [`RsuRuntime::crl_payload_bytes`] and the pending flag, not the
    /// store-and-forward queue: a `down` unit keeps the list and broadcasts it when it
    /// comes back, and the list is not something the unit owes the *backend*. An item a
    /// scenario wants modelled as a downlink relay hop goes through
    /// [`RsuRuntime::accept_uplink`] with [`ForwardKind::TrustListUpdate`] instead, and
    /// comes out of [`ForwardQueue::take_downlink`].
    pub fn install_crl(&mut self, bytes: u32, at: SimTime) {
        let _ = at;
        self.crl_payload_bytes = Some(bytes);
        self.crl_pending = true;
    }

    /// The installed list's length, if one is installed.
    #[must_use]
    pub fn crl_payload_bytes(&self) -> Option<u32> {
        self.crl_payload_bytes
    }

    /// Takes an item off the air, or from a local application, for the backend.
    ///
    /// `false` when no role of this unit accepts it, which is not a failure: a unit that
    /// was not given the report-forwarding role is not a report-forwarding unit, and
    /// silently accepting would make the role meaningless.
    pub fn accept_uplink(&mut self, kind: ForwardKind, bytes: u32, at: SimTime) -> bool {
        let accepted = match kind {
            ForwardKind::MisbehaviourReport => {
                self.config.roles.contains(RsuRole::ReportForwarding)
            }
            ForwardKind::ProvisioningRequest | ForwardKind::ProvisioningResponse => {
                self.config.roles.contains(RsuRole::ProvisioningProxy)
            }
            ForwardKind::TrustListUpdate => self.config.roles.contains(RsuRole::CrlDistribution),
        };
        if !accepted {
            return false;
        }
        self.forward.store(kind, bytes, at);
        self.uplink_accepted = self.uplink_accepted.saturating_add(1);
        true
    }

    /// Hands the unit real encoder output for one broadcast message type.
    ///
    /// This is the path to real wire bytes for a SPaT or a MAP: the engine holds the world
    /// and can build them, a node cannot ([`RsuRuntime::payload_length`] explains why).
    /// Setting an empty payload removes the override and returns the unit to the size
    /// model.
    pub fn set_payload(&mut self, msg_type: MsgType, bytes: Vec<u8>) {
        self.payloads.retain(|(t, _)| *t != msg_type);
        if !bytes.is_empty() {
            self.payloads.push((msg_type, bytes));
            self.payloads.sort_by_key(|(t, _)| *t);
        }
    }

    /// Broadcasts a role asked for and no size model could size.
    #[must_use]
    pub const fn unsized_broadcasts(&self) -> u64 {
        self.unsized_broadcasts
    }

    /// Broadcasts whose payload was a size-model placeholder rather than encoder output.
    ///
    /// The number a reader needs before believing a byte count: a run where this is equal
    /// to the SPaT and MAP count is a run where no real infrastructure message was encoded.
    #[must_use]
    pub const fn modelled_broadcasts(&self) -> u64 {
        self.modelled_broadcasts
    }

    /// Broadcasts and forwards withheld because an attacker controls this unit.
    #[must_use]
    pub const fn compromised_suppressed(&self) -> u64 {
        self.compromised_suppressed
    }

    /// Items taken off the air or from an application for the backend.
    #[must_use]
    pub const fn uplink_accepted(&self) -> u64 {
        self.uplink_accepted
    }

    /// One engine tick.
    ///
    /// The order is 06-node-models.md §2.1's, with the roadside additions in the places
    /// the design puts them: receive and police the inbox; age the stores; broadcast
    /// whatever a role and a timer agree is due; sweep and drain store-and-forward; close
    /// the telemetry window if it is due.
    pub fn step(&mut self, ctx: &mut dyn NodeCtx, inbox: Vec<RxFrame>) -> RsuStepOutcome {
        let now = ctx.now();
        self.clock.advance(now, self.belief.fix.has_position());
        let believed = self.clock.believed_time(now);

        let mut out = RsuStepOutcome::default();
        if matches!(self.state, NodeState::Off) {
            return out;
        }

        self.receive(ctx, believed, inbox, &mut out);
        self.stores.neighbors.age(believed);
        self.stores.certs.sweep(believed, &self.stores.crl);

        if self.state.transmits() {
            self.broadcast(ctx, believed, &mut out);
        }

        self.forward.sweep(believed);
        if matches!(self.state, NodeState::Compromised) {
            // An attacker with the `CompromisedRsu` capability controls this unit's
            // broadcasts and forwarding (06-node-models.md §3), so the *runtime* does
            // neither on its own: the attacker plug-in decides, and invariant I-T3 has it
            // log the decision on a ground-truth channel. Withholding is counted so a run
            // where a report vanished can say why.
            if !self.forward.is_empty() {
                self.compromised_suppressed = self
                    .compromised_suppressed
                    .saturating_add(self.forward.len() as u64);
            }
        } else {
            out.forwarded = self.forward.drain_to(believed, &self.backhaul, self.state);
            out.forwarded.extend(self.forward.take_downlink(believed));
        }

        if self.window.length(now) >= self.config.telemetry_period {
            out.telemetry = Some(self.close_window(now));
        }
        out
    }

    fn receive(
        &mut self,
        ctx: &mut dyn NodeCtx,
        believed: SimTime,
        inbox: Vec<RxFrame>,
        out: &mut RsuStepOutcome,
    ) {
        for frame in inbox {
            self.window.message_in();
            if let Admission::Refused(_) = self.queues[0].push(Queued {
                item: frame,
                enqueued_at: believed,
            }) {
                self.drops.record(DropCause::RxOverflow);
            }
        }

        for q in self.queues[0].drain() {
            let frame = q.item;
            self.learn_or_request(&frame);
            let summary = RxSummary {
                signer: frame.signer.clone(),
                msg_type: frame.msg_type,
                bytes: frame.bytes,
                received_at: believed,
                claimed_pos: frame.claimed_pos,
                // A mast runs no safety application, so no application marks anything
                // relevant. With `verify-all` the field is not read; it is `None` rather
                // than a fabricated score so that a scenario putting `on-demand` on a unit
                // sees the correct degenerate behaviour.
                relevance: None,
            };
            let decision = {
                let view = PolicyView {
                    position: &self.belief,
                    neighbors: &self.stores.neighbors,
                    queue_depth: self.queues[1].len(),
                    queue_capacity: self.queues[1].capacity(),
                };
                self.policy.decide(&summary, &view)
            };
            self.log_decision(ctx, believed, frame.msg_type, &decision);
            match decision {
                VerifyDecision::Drop { cause } => self.drops.record(cause),
                VerifyDecision::DeliverUnverified { reason } => {
                    let _ = reason;
                    self.drops.record(DropCause::VerifyPolicySkip);
                    let m = self.to_message(&frame, believed, VerificationState::Unverified);
                    self.deliver(m, out);
                }
                VerifyDecision::Verify { .. } => {
                    let admitted = if self.policy.oldest_drop() {
                        self.queues[1].push_evicting(Queued {
                            item: frame,
                            enqueued_at: believed,
                        })
                    } else {
                        self.queues[1].push(Queued {
                            item: frame,
                            enqueued_at: believed,
                        })
                    };
                    if !matches!(admitted, Admission::Queued) {
                        self.drops.record(DropCause::VerifyOverflow);
                    }
                }
            }
        }

        self.run_verifications(ctx, believed, out);
    }

    fn learn_or_request(&mut self, frame: &RxFrame) {
        let Some(signer) = frame.signer.clone() else {
            return;
        };
        if frame.full_certificate {
            self.stores.peers.learn(&signer);
        } else if !self.stores.peers.touch(&signer) {
            self.stores.peers.record_p2pcd_request();
        }
    }

    fn run_verifications(
        &mut self,
        ctx: &mut dyn NodeCtx,
        believed: SimTime,
        out: &mut RsuStepOutcome,
    ) {
        let probe = OpDescriptor::verify(self.config.verify_op, 0);
        if self.service.service_time(ctx, &probe).is_none() {
            return;
        }
        let where_ = self.service.runs_on(&probe);
        for q in self.queues[1].drain() {
            let frame = q.item;
            let op = OpDescriptor::verify(self.config.verify_op, frame.bytes);
            let Some(cost) = self.service.service_time(ctx, &op) else {
                continue;
            };
            let sched = match where_ {
                RunsOn::Hsm => self.hsm.submit(q.enqueued_at, cost),
                RunsOn::Accelerator => self.accel.submit(q.enqueued_at, cost),
                RunsOn::Cpu => self.cpu.submit(q.enqueued_at, cost),
            };
            self.window.verification(sched.wait);
            let verdict = self.classify(ctx, &frame);
            ctx.emit(VerifyDecisionRecord::verified(
                self.node,
                q.enqueued_at,
                sched.start,
                sched.finish,
                policy_id(self.policy.code()),
                frame.msg_type.as_str(),
                self.config.verify_op,
                match verdict {
                    VerificationState::Verified | VerificationState::Revoked => "valid",
                    VerificationState::Invalid => "invalid",
                    _ => "skipped",
                },
                0,
            ));
            // The report-forwarding role, on the one message type that reaches it over the
            // air: a verified misbehaviour report goes into store-and-forward, and an
            // unverified one does not. A relay that forwarded what it had not checked
            // would let an attacker use the infrastructure as an amplifier.
            if frame.msg_type == MsgType::Mbr
                && verdict == VerificationState::Verified
                && self.config.roles.contains(RsuRole::ReportForwarding)
            {
                self.forward
                    .store(ForwardKind::MisbehaviourReport, frame.bytes, believed);
                self.uplink_accepted = self.uplink_accepted.saturating_add(1);
            }
            let m = self.to_message(&frame, believed, verdict);
            self.deliver(m, out);
        }
    }

    fn classify(&mut self, ctx: &mut dyn NodeCtx, frame: &RxFrame) -> VerificationState {
        let authentic = match &frame.spdu {
            Some(bytes) => match self.verify_on_the_wire(ctx, bytes) {
                SpduVerdict::Valid => true,
                SpduVerdict::Invalid => false,
                SpduVerdict::Unverifiable => return VerificationState::Unverified,
            },
            None => frame.signature_valid,
        };
        if !authentic {
            return VerificationState::Invalid;
        }
        let Some(lv) = frame.claimed_linkage else {
            return VerificationState::Verified;
        };
        if let Some(own) = self.stores.certs.active() {
            let mine = own.i_period;
            if self.stores.crl.current_period() != mine {
                self.stores.crl.set_period(mine);
            }
        }
        match self
            .stores
            .crl
            .check(frame.claimed_cert_period, lv, authentic)
        {
            crate::stores::CrlVerdict::Revoked => VerificationState::Revoked,
            crate::stores::CrlVerdict::NotRevoked => VerificationState::Verified,
            crate::stores::CrlVerdict::RefusedImplausiblePeriod { .. } => {
                VerificationState::Invalid
            }
        }
    }

    fn verify_on_the_wire(&mut self, ctx: &mut dyn NodeCtx, bytes: &[u8]) -> SpduVerdict {
        let Some(parsed) = self.security.parse(bytes) else {
            return SpduVerdict::Invalid;
        };
        let certificate = match NodeSecurity::attached_certificate(&parsed) {
            Some(c) => {
                self.stores.peers.learn_certificate(c.clone());
                Some(c)
            }
            None => NodeSecurity::parsed_signer_digest(&parsed)
                .and_then(|d| self.stores.peers.certificate(&d)),
        };
        let Some(certificate) = certificate else {
            return SpduVerdict::Unverifiable;
        };
        self.security
            .verify_parsed(ctx, &parsed, &certificate, self.node)
    }

    fn to_message(
        &self,
        frame: &RxFrame,
        believed: SimTime,
        verification: VerificationState,
    ) -> VerifiedMessage {
        VerifiedMessage {
            signer: frame.signer.clone(),
            msg_type: frame.msg_type,
            bytes: frame.bytes,
            received_at: believed,
            claimed_generation_time: frame.claimed_generation_time,
            claimed_pos: frame.claimed_pos,
            claimed_speed_mps: frame.claimed_speed_mps,
            claimed_heading_rad: frame.claimed_heading_rad,
            verification,
        }
    }

    fn deliver(&mut self, m: VerifiedMessage, out: &mut RsuStepOutcome) {
        self.window
            .delivered(m.verification == VerificationState::Verified);
        if m.verification != VerificationState::Invalid
            && let Some(signer) = m.signer.clone()
        {
            self.stores.neighbors.observe(Neighbor {
                signer,
                claimed_pos: m.claimed_pos.unwrap_or(v2xw_core::geom::Vec3::ZERO),
                claimed_speed_mps: m.claimed_speed_mps,
                claimed_heading_rad: m.claimed_heading_rad,
                claimed_generation_time: m.claimed_generation_time,
                last_heard: m.received_at,
                messages: 1,
                state: m.verification,
            });
        }
        if self.received.len() >= self.evidence_capacity {
            self.received.remove(0);
        }
        self.received.push(m.clone());
        out.delivered.push(m);
    }

    /// Everything a role and a timer agree is due, built, signed and queued.
    fn broadcast(&mut self, ctx: &mut dyn NodeCtx, believed: SimTime, out: &mut RsuStepOutcome) {
        if matches!(self.state, NodeState::Compromised) {
            // As in `step`: the attacker owns this unit's broadcasts.
            self.compromised_suppressed = self.compromised_suppressed.saturating_add(1);
            return;
        }
        let dcc = self.dcc;
        let mut due: Vec<MsgType> = Vec::new();
        if self.config.roles.contains(RsuRole::SpatMapBroadcast) {
            if self.spat.due(believed, &dcc) {
                due.push(MsgType::Spat);
            }
            if self.map.due(believed, &dcc) {
                due.push(MsgType::Map);
            }
        }
        if self.config.roles.contains(RsuRole::WsaBroadcast) && self.wsa.due(believed, &dcc) {
            due.push(MsgType::Wsa);
        }
        if self.config.roles.contains(RsuRole::CrlDistribution) && self.crl_payload_bytes.is_some()
        {
            // A freshly installed list goes out at once; after that only a scenario that
            // set a repeat period re-broadcasts it, because no clause fixes a cadence —
            // TS 102 941 Annex D.3 gives the delta-CTL broadcast its shape (single hop, no
            // segmentation, re-broadcast unmodified) and not its period.
            let repeat = self.config.crl_repeat.is_some() && self.crl.due(believed, &dcc);
            if self.crl_pending || repeat {
                self.crl_pending = false;
                due.push(MsgType::Crl);
            }
        }
        if due.is_empty() {
            return;
        }

        let Some(cred) = self.stores.certs.active().cloned() else {
            self.drops.record_n(DropCause::TxOverflow, due.len() as u32);
            return;
        };
        if !self.provision_all(ctx, believed) || !self.security.set_active(cred.i_period, cred.j_index)
        {
            self.drops.record_n(DropCause::TxOverflow, due.len() as u32);
            return;
        }
        let Some(cred) = self.stores.certs.active().cloned() else {
            self.drops.record_n(DropCause::TxOverflow, due.len() as u32);
            return;
        };

        for msg_type in due {
            let Some((payload_length, modelled)) = self.payload_length(msg_type) else {
                // A role asked for a message nothing can size. Counted, never invented:
                // there is no WSA size-model row and no WSA encoder, so a unit with the
                // WSA role and no scenario-supplied length transmits nothing.
                self.unsized_broadcasts = self.unsized_broadcasts.saturating_add(1);
                continue;
            };
            if modelled {
                self.modelled_broadcasts = self.modelled_broadcasts.saturating_add(1);
            }
            let payload = self.payload_bytes(msg_type, payload_length);
            let sid = self.security.signer_id_for(msg_type, believed);
            let Ok((frame, pdu)) = self.security.sign(ctx, msg_type, &payload, sid, None) else {
                self.drops.record(DropCause::TxOverflow);
                continue;
            };
            let op = OpDescriptor::sign(self.config.sign_op, frame.payload_bytes());
            let Some(cost) = self.service.service_time(ctx, &op) else {
                // Nothing is signed for free, at a mast as in a vehicle.
                self.drops.record(DropCause::TxOverflow);
                continue;
            };
            let sched = match self.service.runs_on(&op) {
                RunsOn::Hsm => self.hsm.submit(believed, cost),
                RunsOn::Accelerator => self.accel.submit(believed, cost),
                RunsOn::Cpu => self.cpu.submit(believed, cost),
            };
            let full_certificate = pdu.signer_id == v2xw_sec::SignerIdChoice::Certificate;
            if full_certificate {
                self.security.note_certificate_attached(msg_type, believed);
            }
            let bytes = frame.bytes_on_wire();
            // Air time is the PHY's to compute for a unit as for a vehicle; the window
            // records the byte count and contributes nothing to `airtime_ms_per_s` rather
            // than inventing a data rate.
            self.window.message_out(Duration::ZERO, full_certificate);
            out.transmissions.push(Transmission {
                msg_type,
                bytes,
                signer: cred.digest.clone(),
                full_certificate,
                ready_at: sched.finish,
                generation_time: believed,
                sign_start: sched.start,
                signed: Some(frame),
            });
        }
    }

    /// The payload length this unit will broadcast for `msg_type`.
    ///
    /// A scenario-supplied length wins; otherwise the J2735 size model's row for the
    /// message, at [`RsuConfig::content_profile`], with the row's own nominal element count
    /// unless the configuration overrides it. `None` when neither exists, which is the
    /// WSA's case and is counted rather than filled in.
    ///
    /// # Real encoders exist for SPaT and MAP, and this is not using them
    ///
    /// `v2xw-msg` carries hand-written UPER encoders for both
    /// (`codec/uper/j2735-spat-map`), and the size model's SPaT and MAP rows are marked
    /// `superseded_by` that codec — kept, its own documentation says, because "a retired
    /// row is kept rather than deleted … it is the only record of what the modelled size
    /// *was*" and remains "sizable through `lookup` on purpose".
    ///
    /// Reaching the real encoder needs a `Spat` and a `MapData`, and those need the
    /// intersection the unit stands at: signal groups and their movement events for a
    /// SPaT, lane geometry for a MAP. A node holds no map — 06-node-models.md §3 gives an
    /// RSU its site "from the world", and [`crate::firewall`] is the reason this crate
    /// cannot go and read one. So the path for real bytes is
    /// [`RsuRuntime::set_payload`]: the engine, which does hold the world, encodes the
    /// SPaT and the MAP and hands the octets in. Until a scenario does that, the modelled
    /// length is used and [`RsuRuntime::modelled_broadcasts`] counts how often.
    fn payload_length(&self, msg_type: MsgType) -> Option<(u32, bool)> {
        if let Some(bytes) = self.payload_override(msg_type) {
            return Some((bytes, false));
        }
        if msg_type == MsgType::Wsa {
            return self.config.wsa_payload_bytes.map(|b| (b, true));
        }
        let elements = match msg_type {
            MsgType::Spat => self.config.spat_movement_states,
            MsgType::Map => self.config.map_lanes,
            _ => None,
        };
        let entry = v2xw_msg::size_model::lookup(msg_type, self.config.content_profile)?;
        Some((entry.bytes(elements.unwrap_or(entry.nominal_elements)), true))
    }

    /// The scenario-supplied length for `msg_type`, if one was set.
    fn payload_override(&self, msg_type: MsgType) -> Option<u32> {
        match msg_type {
            MsgType::Crl => self.crl_payload_bytes,
            _ => self
                .payloads
                .iter()
                .find(|(t, _)| *t == msg_type)
                .map(|(_, p)| u32::try_from(p.len()).unwrap_or(u32::MAX)),
        }
    }

    /// The bytes to put in the SPDU for `msg_type`: the scenario's own octets where it
    /// supplied them, a placeholder of the modelled length otherwise.
    fn payload_bytes(&self, msg_type: MsgType, length: u32) -> Vec<u8> {
        if let Some((_, real)) = self.payloads.iter().find(|(t, _)| *t == msg_type) {
            return real.clone();
        }
        vec![v2xw_msg::codec::PLACEHOLDER_FILL; length as usize]
    }

    fn provision_all(&mut self, ctx: &mut dyn NodeCtx, believed: SimTime) -> bool {
        let pseudonyms: Vec<(u32, u32)> = self
            .stores
            .certs
            .credentials()
            .iter()
            .map(|c| (c.i_period, c.j_index))
            .collect();
        for (i, j) in pseudonyms {
            if self
                .security
                .provision(ctx, self.node, i, j, believed)
                .is_err()
            {
                return false;
            }
        }
        for cred in self.stores.certs.credentials_mut() {
            let Some(signer) = self
                .security
                .signer_for_pseudonym(cred.i_period, cred.j_index)
            else {
                return false;
            };
            if cred.digest != *signer.digest() {
                cred.digest = signer.digest().clone();
                cred.cert_coer = signer.cert_coer().to_vec();
            }
        }
        true
    }

    fn log_decision(
        &self,
        ctx: &mut dyn NodeCtx,
        believed: SimTime,
        msg_type: MsgType,
        d: &VerifyDecision,
    ) {
        // A skip or a drop settles the message's fate here; a decision to verify is
        // recorded when the check runs (`run_verifications`), with its instants.
        if let Some(rec) = VerifyDecisionRecord::decided(
            self.node,
            believed,
            policy_id(self.policy.code()),
            msg_type.as_str(),
            d,
        ) {
            ctx.emit(rec);
        }
    }

    fn close_window(&mut self, now: SimTime) -> NodeTelemetry {
        let storage = self.service.profile().storage_model;
        let profile = self.service.profile();
        let (total, verified, unverified, revoked) = self.stores.neighbors.counts();
        let queue_depths = [
            self.queues[0].depth_percentiles(),
            self.queues[1].depth_percentiles(),
            self.queues[2].depth_percentiles(),
            self.queues[3].depth_percentiles(),
            self.queues[4].depth_percentiles(),
        ];
        let stores_bytes = self.stores.bytes(&storage);
        let inputs = TelemetryInputs {
            node: self.node,
            storage_used_b: stores_bytes.saturating_add(self.forward.bytes()),
            storage_total_b: profile
                .flash_bytes
                .get()
                .copied()
                .unwrap_or(v2xw_record::wire::U64_NONE),
            next_topup_ns: v2xw_record::wire::U64_NONE,
            crl_bytes: u64::from(self.crl_payload_bytes.unwrap_or(0)),
            // §3.5.2's outbox is "pending misbehaviour reports with store-and-forward
            // state" at a vehicle; at a mast the same two fields carry the same thing one
            // hop on, which is what makes a held report visible in a HUD without a second
            // pair of fields nobody reads.
            outbox_bytes: self.forward.bytes(),
            clock_offset_ns: self.clock.offset_ns(),
            ram_used_kib: u32::try_from(
                stores_bytes
                    .saturating_add(self.forward.bytes())
                    .saturating_add(storage.baseline_ram_bytes)
                    / 1024,
            )
            .unwrap_or(u32::MAX),
            ram_total_kib: profile
                .ram_bytes
                .get()
                .map(|b| u32::try_from(b / 1024).unwrap_or(u32::MAX))
                .unwrap_or(v2xw_record::wire::U32_NONE),
            drops: self.drops.counts(),
            cert_stored: self.stores.certs.stored_count() as u32,
            crl_entries: self.stores.crl.entries() as u32,
            outbox_msgs: u32::try_from(self.forward.len()).unwrap_or(u32::MAX),
            peer_cache_entries: self.stores.peers.len() as u32,
            p2pcd_requests: self.stores.peers.p2pcd_requests(),
            gnss_sigma_m: self.belief.semi_major_m as f32,
            gnss_hdop: f32::NAN,
            clock_drift_ppm: self.clock.drift_ppm() as f32,
            pos_error_m: self.gt_pos_error_m,
            cpu_util_pm: self.cpu.utilisation_pm(now),
            hsm_util_pm: self
                .hsm
                .utilisation_pm(now)
                .max(self.accel.utilisation_pm(now)),
            queue_depths,
            dcc_state: self.dcc_state_code,
            cbr_pm: self.dcc.cbr.map_or(v2xw_record::wire::U16_NONE, |c| {
                (math::quantize_to(c * 1000.0, 1.0) as u16).min(1000)
            }),
            tx_power_cdbm: i16::try_from(
                math::quantize_to(self.config.tx_power_dbm * 100.0, 1.0) as i64
            )
            .unwrap_or(i16::MAX),
            neighbors: (total, verified, unverified, revoked),
            cert_active: u16::try_from(self.stores.certs.active_count(now)).unwrap_or(u16::MAX),
            crl_expansion_pm: self.stores.crl.expansion_pm(),
            gnss_fix: gnss_fix_code(self.belief.fix),
            state: self.state,
            verify_policy: self.policy.code(),
        };
        let record = self.window.record(now, &inputs);

        self.window.reset(now);
        self.drops.reset();
        self.cpu.reset_window(now);
        self.hsm.reset_window(now);
        self.accel.reset_window(now);
        for q in &mut self.queues {
            q.reset_window();
        }
        self.stores.peers.reset_window();
        self.stores.crl.reset_window();
        record
    }
}

impl NodeView for RsuRuntime {
    type Neighbors = NeighborTable;
    type Credential = CredentialHandle;
    type Message = VerifiedMessage;

    fn node(&self) -> NodeId {
        self.node
    }

    fn believed_time(&self) -> SimTime {
        self.clock.believed_time(self.belief.time_ns)
    }

    fn position(&self) -> &PositionEstimate {
        &self.belief
    }

    fn neighbors(&self) -> &NeighborTable {
        &self.stores.neighbors
    }

    fn credentials(&self) -> &[CredentialHandle] {
        self.stores.certs.credentials()
    }

    fn received(&self) -> &[VerifiedMessage] {
        &self.received
    }
}

/// The policy id for a policy code, the same mapping [`crate::runtime`] uses.
fn policy_id(code: u8) -> &'static str {
    match code {
        0 => crate::policy::VERIFY_ALL_ID,
        1 => crate::policy::ON_DEMAND_ID,
        _ => crate::policy::PRIORITIZED_ID,
    }
}

fn plan(name: &str, unit: &str, default: serde_json::Value, why: &str, how: &str) -> Parameter {
    let mut p = Parameter::new(name, unit, default, Source::todo_calibrate(why));
    p.calibration = Some(how.to_string());
    p
}

fn backhaul_card(
    kind: BackhaulKind,
    latency: Duration,
    bandwidth_bps: Option<u64>,
) -> ModelCard {
    let mut card = ModelCard::new(
        BACKHAUL_ID,
        Family::Backhaul,
        "0.1.0",
        format!(
            "Fixed-latency roadside backhaul (04-models.md §10.2, abstract tier), \
             configured as `{}`: one-way latency plus an optional serialisation term.",
            kind.as_str()
        ),
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![Equation::new(
        "one-way delay",
        "d = latency*multiplier + bytes*8/bandwidth, the serialisation term omitted when \
         no capacity is modelled; the multiplier is 1 except in the `degraded` state",
    )];
    card.parameters = vec![
        Parameter::new(
            "latency_ns",
            "ns",
            serde_json::json!(latency.as_nanos()),
            Source::new(
                SourceKind::Paper,
                match kind {
                    BackhaulKind::Fibre => {
                        "Coll-Perales 2022 Table VI, transport network with the MEC at the \
                         gNB: 0.402 ms mean, 0.422 ms at the 99.99th percentile; \
                         04-models.md §10.2 takes fibre and microwave from these figures \
                         (0.4-2.4 ms mean) [R11 §A3]"
                    }
                    BackhaulKind::Cellular => {
                        "Narayanan et al. WWW'20 Table 2, LTE first-hop RTT 29.2 +/- 4.8 ms, \
                         'one way modeled as half' per 04-models.md §10.1 [R11 §A1]"
                    }
                    BackhaulKind::None => {
                        "no link: 04-models.md §10.2's third case, where every byte for the \
                         backend waits in store-and-forward"
                    }
                },
            ),
        ),
        match bandwidth_bps {
            Some(bps) => Parameter::new(
                "bandwidth_bps",
                "bit/s",
                serde_json::json!(bps),
                Source::new(
                    SourceKind::Code,
                    "Eclipse MOSAIC Cell example caps, 28 Mbit/s uplink and 42.2 Mbit/s \
                     downlink, which 04-models.md §10.1 lists 'for comparison' — a worked \
                     example, not a measurement [R11 §A7]",
                ),
            ),
            None => plan(
                "bandwidth_bps",
                "bit/s",
                serde_json::Value::Null,
                "no published figure gives the capacity of a roadside unit's fibre or \
                 Ethernet drop",
                "Not modelled: the delay is the latency alone, so a study of backhaul \
                 congestion must supply a capacity. Take it from the deployment — the \
                 Tampa THEA and Wyoming I-80 pilots publish the medium but not the rate \
                 [04-models.md §10.2, R11 §A6] — or use the cellular preset, whose capacity \
                 is a cited worked example.",
            ),
        },
        plan(
            "degraded_multiplier",
            "-",
            serde_json::json!(DEFAULT_DEGRADED_MULTIPLIER),
            "06-node-models.md §3 specifies the `degraded` state as a 'backhaul latency \
             multiplier' and gives no multiplier",
            "An order of magnitude, chosen to be unmistakable in a latency plot rather \
             than plausible. Calibrate against the degradation the study is about — a \
             congested microwave hop, a cellular link at the cell edge — and prefer \
             switching to the cellular preset over multiplying the fibre one when the \
             medium itself changes.",
        ),
    ];
    card.assumptions = vec![
        "One latency for the link, in both directions, with no queueing: the abstract tier \
         of 04-models.md §10.2. The measured transport-network chain (`backhaul/measured-tn`, \
         the Coll-Perales M/M/1 model) is the medium and high tier and is not this model."
            .into(),
    ];
    card.limitations = vec![
        "No queueing, no loss, no jitter and no availability model, so the 99.99th \
         percentile a study of an undersized link needs is not reachable here — that is \
         exactly what 04-models.md §10.2 says `backhaul/measured-tn` is for."
            .into(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Paper,
            "Coll-Perales et al., 'End-to-end V2X latency modeling and analysis in 5G \
             networks', IEEE TVT 2022, arXiv:2201.06082 [R11 §A3]",
        ),
        Source::new(
            SourceKind::Paper,
            "Narayanan et al., 'A first look at commercial 5G performance on smartphones', \
             WWW'20 [R11 §A1]",
        ),
    ];
    card.validation = Validation::new(ValidationStatus::LiteratureChecked);
    card.determinism = Determinism::default();
    card
}

fn card(profile: &HardwareProfile, config: &RsuConfig, backhaul: &Backhaul) -> ModelCard {
    let mut card = ModelCard::new(
        RSU_RUNTIME_ID,
        // See RSU_RUNTIME_ID: the `Family` enum is closed (ADR 0007 Consequences) and the
        // parameters below are broadcast cadences, so the honest fit is the family whose
        // definition is "message generation rules". The backhaul's own card is
        // Family::Backhaul.
        Family::Generator,
        "0.1.0",
        format!(
            "Roadside-unit runtime on hardware profile `{}` with roles [{}] and a {} \
             backhaul: SPaT/MAP/WSA/CRL broadcast on the rates of 04-models.md §8.1, \
             report and provisioning forwarding with store-and-forward, and the `down`, \
             `degraded` and `compromised` states of 06-node-models.md §3.",
            profile.id,
            config.roles.names().join(", "),
            backhaul.kind().as_str()
        ),
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![Equation::new(
        "store-and-forward",
        "an item is held until the backhaul is up, discarded at an age of \
         forward_max_age_s, and the oldest is discarded first on overflow",
    )];
    card.parameters = vec![
        Parameter::new(
            "spat_period_ms",
            "ms",
            serde_json::json!(config.spat_period.as_nanos() / 1_000_000),
            Source::new(
                SourceKind::Standard,
                "04-models.md §8.1 `generator/spat-map`, 'Defaults: SPaT 10 Hz': US CTI \
                 4501 v01.01 requires a SPaT average of 10 per second +/- 1 over 10 s, with \
                 the signal controller never more than 0.3 s from the unit and a latency \
                 <= 300 ms [R4 §A.4]",
            ),
        ),
        Parameter::new(
            "map_period_ms",
            "ms",
            serde_json::json!(config.map_period.as_nanos() / 1_000_000),
            Source::new(
                SourceKind::Standard,
                "04-models.md §8.1 `generator/spat-map`, 'Defaults: MAP 1 Hz': CTI 4501 \
                 requires a MAP average of 1 per second +/- 1 over 10 s [R4 §A.4]",
            ),
        ),
        Parameter::new(
            "spat_movement_states",
            "-",
            match config.spat_movement_states {
                Some(n) => serde_json::json!(n),
                None => serde_json::Value::Null,
            },
            Source::new(
                SourceKind::Code,
                "absent, the nominal element count of `codec/size-model/j2735`'s SPaT row \
                 for the chosen content profile, so a unit that says nothing broadcasts the \
                 message the size model was validated at",
            ),
        ),
        Parameter::new(
            "map_lanes",
            "-",
            match config.map_lanes {
                Some(n) => serde_json::json!(n),
                None => serde_json::Value::Null,
            },
            Source::new(
                SourceKind::Code,
                "absent, the nominal element count of `codec/size-model/j2735`'s MAP row \
                 for the chosen content profile; that row's anchor is CTI 4501 \
                 §4.3.3.1.3.1's 2,302-byte ceiling",
            ),
        ),
        Parameter::new(
            "content_profile",
            "-",
            serde_json::json!(config.content_profile.as_str()),
            Source::new(
                SourceKind::Code,
                "the content profile the size model is asked for; `typical` is v2xw-msg's \
                 own definition of 'what a deployment actually broadcasts: the mandatory \
                 fields plus the optional ones the relevant profile (CTI 4501, TS 103 301) \
                 requires'",
            ),
        ),
        plan(
            "wsa_period_ms",
            "ms",
            serde_json::json!(config.wsa_period.as_nanos() / 1_000_000),
            "04-models.md §8.1 records the IEEE 1609.3 `RepeatRate` semantics \
             ('transmissions per 5 s') as UNVERIFIED and the default of 1 per 5 s as \
             UNVERIFIED with it",
            "Read IEEE 1609.3's own definition of RepeatRate and replace both the semantics \
             and the default. Until then a unit with the WSA role transmits nothing anyway, \
             because there is no WSA size model: see wsa_payload_bytes.",
        ),
        plan(
            "wsa_payload_bytes",
            "B",
            match config.wsa_payload_bytes {
                Some(b) => serde_json::json!(b),
                None => serde_json::Value::Null,
            },
            "no WSA encoder and no WSA row in `codec/size-model/j2735` exists: the size \
             model covers BSM, SPaT, MAP, PSM, SRM and SSM",
            "The ASN.1 is available and VERIFIED (IEEE 1609.3 `wsa.asn`, `wee.asn`, \
             04-models.md §8.1), so the fix is to generate the encoder or add a size-model \
             row with an anchor. Until one exists a unit with the WSA role broadcasts \
             nothing and RsuRuntime::unsized_broadcasts counts every attempt, rather than \
             putting an invented length on the air.",
        ),
        plan(
            "crl_repeat_ms",
            "ms",
            match config.crl_repeat {
                Some(d) => serde_json::json!(d.as_nanos() / 1_000_000),
                None => serde_json::Value::Null,
            },
            "no clause fixes how often a roadside unit re-broadcasts a revocation or trust \
             list",
            "TS 102 941 Annex D.3 gives the delta-CTL broadcast its shape — single hop, no \
             segmentation, stations re-broadcast unmodified — and no period; 05-protocols.md \
             §2.5 makes the *issuance* cadence scenario-selectable (daily by default) and \
             says nothing about the air interface. Absent a period the unit broadcasts an \
             installed list once and then only on update, which is the minimal behaviour; \
             set a period from the deployment being modelled.",
        ),
        plan(
            "forward_max_age_s",
            "s",
            serde_json::json!(FORWARD_MAX_AGE.as_nanos() / 1_000_000_000),
            "[CAMP-EE §2.2.8] gives the one-week retention for an *end entity's* own \
             report outbox; no clause gives a relay's",
            "The end entity's rule is reused rather than a second number invented. Confirm \
             against CAMP-EE's RSE requirements or the deployment's own retention policy; \
             a shorter relay retention makes a long outage lose reports it currently keeps.",
        ),
        plan(
            "forward_capacity",
            "-",
            serde_json::json!(config.forward_capacity),
            "no source gives a roadside unit's store-and-forward depth",
            "It must be bounded — an unbounded relay buffer turns a week-long outage into a \
             memory leak — and the bound should come from the unit's published RAM \
             allowance. The two shipped roadside profiles publish 2 GB and NOT PUBLISHED \
             respectively, so size it from the deployment and watch \
             ForwardQueue::dropped_overflow.",
        ),
    ];
    card.assumptions = vec![
        format!(
            "Costs come from the hardware profile `{}` and nowhere else; an operation it \
             does not cost has no service time here either.",
            profile.id
        ),
        "SPaT and MAP payloads are `codec/size-model/j2735` placeholders of exact modelled \
         length unless the engine hands real octets in through RsuRuntime::set_payload; \
         the IEEE 1609.2 envelope around them is real and really signed either way, so \
         the signing cost and the envelope overhead are the real ones. \
         RsuRuntime::modelled_broadcasts counts how many went out on a modelled length."
            .into(),
        "`codec/uper/j2735-spat-map` — a real hand-written SPaT and MAP encoder — exists in \
         v2xw-msg and retires the size model's rows (`superseded_by`). It is not reached \
         from here because building a Spat or a MapData needs the intersection's signal \
         groups and lane geometry, which a node does not hold and the ground-truth firewall \
         stops it fetching; the engine holds the world and hands the octets in."
            .into(),
        "A revocation or trust list's length is the backend's, handed in by \
         RsuRuntime::install_crl: a CRL's size is the protocol's own (v2xw-proto's wire \
         table) and this runtime does not compute one."
            .into(),
        "The mast's position is surveyed, so it is handed in without GNSS error — and it is \
         still read as a belief, because a misconfigured unit may hold a wrong one and \
         nothing here may consult the truth to find out."
            .into(),
    ];
    card.limitations = vec![
        "`compromised` withholds this runtime's own broadcasts and forwarding rather than \
         emitting an attacker's: the attacker plug-in decides what a compromised unit puts \
         on the air (07-threats-and-detection.md §3, capability CompromisedRsu) and \
         invariant I-T3 has it logged on a ground-truth channel, which is not this crate's."
            .into(),
        "The detector-host role is the NodeView this runtime implements: the detectors \
         themselves are v2xw-threat's, and a unit without the role still exposes the view."
            .into(),
        "Air time is not computed here, so `airtime_ms_per_s` in the telemetry record is \
         zero for a unit exactly as it is for a vehicle."
            .into(),
        "The store-and-forward drain is abstract-tier: everything up goes at once when the \
         link is up, with the link's own latency and serialisation term, and no scheduler \
         orders it."
            .into(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 301 V2.1.1 §5.4.2, §6.4.2-6.4.3 and USDOT CTI 4501 v01.01 \
             §3.3.3.1.5.1-3 (SPaT and MAP rates) [R4 §A.4]",
        ),
        Source::new(
            SourceKind::Standard,
            "ETSI TS 102 941 V2.2.1 §6.3 and Annex D.3 (delta-CTL broadcast over ITS-G5)",
        ),
        Source::new(
            SourceKind::Paper,
            "CAMP SCMS EE requirements [CAMP-EE §2.2.8] (one-week report retention), via \
             05-protocols.md §2.6",
        ),
        Source::new(
            SourceKind::Datasheet,
            format!("hardware profile {}@{}", profile.id, profile.version),
        ),
    ];
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.validation.tests = vec![
        "rsu_runtime::a_unit_with_the_spat_map_role_broadcasts_at_ten_and_one_hertz".to_string(),
        "rsu_runtime::a_unit_with_no_roles_broadcasts_nothing".to_string(),
        "rsu_runtime::a_report_is_held_while_the_backhaul_is_down_and_goes_when_it_returns"
            .to_string(),
        "rsu_runtime::a_down_unit_transmits_nothing".to_string(),
        "rsu_runtime::a_compromised_unit_neither_broadcasts_nor_forwards".to_string(),
    ];
    card.determinism = Determinism::default();
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::time::{NS_PER_MS, NS_PER_S};

    /// The role set round-trips through its scenario spellings, and the two names the
    /// Phase 2 engine wiring already uses are among them.
    #[test]
    fn roles_round_trip_through_their_scenario_names() {
        let (roles, unknown) = RsuRoles::parse(&["crl", "report-forward"]);
        assert!(unknown.is_empty());
        assert!(roles.contains(RsuRole::CrlDistribution));
        assert!(roles.contains(RsuRole::ReportForwarding));
        assert!(!roles.contains(RsuRole::SpatMapBroadcast));
        assert_eq!(roles.len(), 2);
        assert_eq!(roles.names(), vec!["crl", "report-forward"]);

        for r in RsuRole::ALL {
            assert_eq!(RsuRole::parse(r.as_str()), Some(r));
            assert!(RsuRoles::NONE.with(r).contains(r));
            assert!(!RsuRoles::all().without(r).contains(r));
        }
        assert_eq!(RsuRoles::all().len(), RsuRole::ALL.len() as u32);
    }

    /// An unknown role name is reported, not swallowed: a scenario that asked for `spat`
    /// and got a role-less unit is the failure a loader must be able to name.
    #[test]
    fn an_unknown_role_name_is_reported() {
        let (roles, unknown) = RsuRoles::parse(&["spat", "crl"]);
        assert_eq!(unknown, vec!["spat".to_string()]);
        assert_eq!(roles.names(), vec!["crl"]);
    }

    /// The roles iterate in declaration order whatever order they were added in, so a
    /// report's role list cannot depend on how a scenario was written.
    #[test]
    fn roles_iterate_in_declaration_order() {
        let a = RsuRoles::NONE
            .with(RsuRole::DetectorHost)
            .with(RsuRole::CrlDistribution);
        let b = RsuRoles::NONE
            .with(RsuRole::CrlDistribution)
            .with(RsuRole::DetectorHost);
        assert_eq!(a, b);
        assert_eq!(a.names(), vec!["crl", "detector-host"]);
    }

    /// The fibre and cellular presets are the design set's own one-way figures.
    #[test]
    fn the_backhaul_presets_are_the_cited_ones() {
        assert_eq!(Backhaul::fibre().latency(), Duration::from_nanos(402_000));
        assert_eq!(
            Backhaul::cellular().latency(),
            Duration::from_nanos(14_600_000)
        );
        // Half of the 29.2 ms round trip, exactly as 04-models.md §10.1 models it.
        assert_eq!(
            Backhaul::cellular().latency().as_nanos() * 2,
            29_200_000,
            "the cellular one-way figure must be half the cited RTT"
        );
        assert_eq!(Backhaul::fibre().bandwidth_bps(), None);
        assert_eq!(Backhaul::cellular().bandwidth_bps(), Some(28_000_000));
    }

    /// A link with no capacity adds no serialisation term; one with a capacity does, and
    /// the degraded multiplier scales the latency and not the capacity.
    #[test]
    fn the_backhaul_delay_is_latency_plus_serialisation() {
        let fibre = Backhaul::fibre();
        assert_eq!(fibre.delay(10_000, NodeState::Active), fibre.latency());
        assert_eq!(
            fibre.delay(10_000, NodeState::Degraded),
            Duration::from_nanos(402_000 * u64::from(DEFAULT_DEGRADED_MULTIPLIER))
        );

        let cell = Backhaul::cellular();
        // 1,200 bytes is 9,600 bits, which at 28 Mbit/s is 342,857 ns.
        let expected = 14_600_000 + (1_200u64 * 8 * 1_000_000_000) / 28_000_000;
        assert_eq!(cell.delay(1_200, NodeState::Active).as_nanos(), expected);
        // Degraded multiplies the propagation term only.
        assert_eq!(
            cell.delay(1_200, NodeState::Degraded).as_nanos(),
            14_600_000 * u64::from(DEFAULT_DEGRADED_MULTIPLIER)
                + (1_200u64 * 8 * 1_000_000_000) / 28_000_000
        );
    }

    /// `down` loses the backhaul, which is the design's own definition of the state;
    /// `compromised` keeps it, because an attacker forwarding needs a link.
    #[test]
    fn down_loses_the_backhaul_and_compromised_does_not() {
        let f = Backhaul::fibre();
        assert!(f.is_up(NodeState::Active));
        assert!(f.is_up(NodeState::Degraded));
        assert!(f.is_up(NodeState::Compromised));
        assert!(!f.is_up(NodeState::Down));
        assert!(!f.is_up(NodeState::Off));
        assert!(!Backhaul::none().is_up(NodeState::Active));
    }

    /// Store-and-forward holds while the link is down, hands everything over when it is
    /// up, and stamps each item's arrival with the link's own delay.
    #[test]
    fn the_forward_queue_holds_then_drains() {
        let mut q = ForwardQueue::default();
        q.store(ForwardKind::MisbehaviourReport, 1_200, 0);
        q.store(ForwardKind::ProvisioningRequest, 400, 0);
        assert_eq!(q.len(), 2);
        assert_eq!(q.bytes(), 1_600);

        // No link: nothing moves.
        assert!(
            q.drain_to(NS_PER_S, &Backhaul::none(), NodeState::Active)
                .is_empty()
        );
        assert_eq!(q.len(), 2);

        // A link: every uplink item moves, oldest first, each with its own arrival.
        let out = q.drain_to(NS_PER_S, &Backhaul::cellular(), NodeState::Active);
        assert_eq!(out.len(), 2);
        assert!(q.is_empty());
        assert_eq!(q.forwarded(), 2);
        assert_eq!(q.bytes_forwarded(), 1_600);
        assert_eq!(out[0].kind, ForwardKind::MisbehaviourReport);
        assert_eq!(out[0].forwarded_at, Some(NS_PER_S));
        assert!(out[0].arrives_at.expect("stamped") > NS_PER_S);
        // The bigger item takes longer, because the link has a capacity.
        assert!(out[0].arrives_at > out[1].arrives_at);
    }

    /// An item that ages out is discarded and counted; the retention is the end entity's
    /// one week.
    #[test]
    fn a_held_item_ages_out_after_a_week() {
        let mut q = ForwardQueue::default();
        q.store(ForwardKind::MisbehaviourReport, 100, 0);
        assert_eq!(q.sweep(FORWARD_MAX_AGE.as_nanos()), 0, "exactly a week is in");
        assert_eq!(q.sweep(FORWARD_MAX_AGE.as_nanos() + 1), 1);
        assert_eq!(q.dropped_aged(), 1);
        assert!(q.is_empty());
    }

    /// Overflow discards the oldest, so a full queue is not permanently deaf.
    #[test]
    fn overflow_discards_the_oldest() {
        let mut q = ForwardQueue::new(2, FORWARD_MAX_AGE);
        q.store(ForwardKind::MisbehaviourReport, 1, 0);
        q.store(ForwardKind::MisbehaviourReport, 2, NS_PER_MS);
        q.store(ForwardKind::MisbehaviourReport, 3, 2 * NS_PER_MS);
        assert_eq!(q.len(), 2);
        assert_eq!(q.dropped_overflow(), 1);
        assert_eq!(q.items()[0].bytes, 2, "the oldest went");
        assert_eq!(q.items()[1].bytes, 3);
    }

    /// The periodic timer honours both its own period and a DCC floor above it.
    #[test]
    fn the_broadcast_timer_honours_dcc() {
        let mut t = Periodic::new(Duration::from_millis(100));
        let open = DccState::UNRESTRICTED;
        assert!(t.due(0, &open));
        assert!(!t.due(50 * NS_PER_MS, &open));
        assert!(t.due(100 * NS_PER_MS, &open));

        let mut t = Periodic::new(Duration::from_millis(100));
        let gated = DccState {
            t_off: Duration::from_millis(500),
            cbr: Some(0.7),
        };
        assert!(t.due(0, &gated));
        assert!(!t.due(100 * NS_PER_MS, &gated));
        assert!(t.due(500 * NS_PER_MS, &gated));
    }

    /// Both cards validate, which is also the assertion that every uncalibrated parameter
    /// carries a plan (registry rule R1).
    #[test]
    fn the_cards_validate() {
        for b in [Backhaul::fibre(), Backhaul::cellular(), Backhaul::none()] {
            b.card().validate().expect("the backhaul card validates");
            assert_eq!(b.card().family, Family::Backhaul);
        }
        let unit = RsuRuntime::with_roles(NodeId::new(1), RsuRoles::all(), 0);
        unit.card().validate().expect("the RSU card validates");
        assert_eq!(unit.card().family, Family::Generator);
        assert_eq!(unit.card().id, RSU_RUNTIME_ID);
    }

    /// Only a unit with the role accepts an uplink item, and the role it needs is the one
    /// the design names.
    #[test]
    fn only_the_right_role_accepts_an_uplink_item() {
        let mut unit = RsuRuntime::with_roles(
            NodeId::new(1),
            RsuRoles::NONE.with(RsuRole::ReportForwarding),
            0,
        );
        assert!(unit.accept_uplink(ForwardKind::MisbehaviourReport, 1_200, 0));
        assert!(!unit.accept_uplink(ForwardKind::ProvisioningRequest, 400, 0));
        assert_eq!(unit.forward_queue().len(), 1);
        assert_eq!(unit.uplink_accepted(), 1);
    }

    /// Installing a list takes custody of it without putting it in store-and-forward: a
    /// unit does not owe the backend its own CRL back.
    #[test]
    fn installing_a_list_takes_custody_without_queueing_it_for_the_backend() {
        let mut unit = RsuRuntime::with_roles(
            NodeId::new(1),
            RsuRoles::NONE.with(RsuRole::CrlDistribution),
            0,
        );
        unit.install_crl(400_000, 0);
        assert_eq!(unit.crl_payload_bytes(), Some(400_000));
        assert!(unit.forward_queue().is_empty());
    }

    /// The queue's two directions go different ways: a report to the backhaul, a
    /// trust-list update to the air.
    #[test]
    fn uplink_and_downlink_items_leave_by_different_routes() {
        let mut q = ForwardQueue::default();
        q.store(ForwardKind::MisbehaviourReport, 1_200, 0);
        q.store(ForwardKind::TrustListUpdate, 4_000, 0);

        let up = q.drain_to(NS_PER_S, &Backhaul::fibre(), NodeState::Active);
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].kind, ForwardKind::MisbehaviourReport);
        assert!(up[0].arrives_at.is_some(), "the backhaul stamps an arrival");
        assert_eq!(q.len(), 1, "the downlink item stays");

        let down = q.take_downlink(NS_PER_S);
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].kind, ForwardKind::TrustListUpdate);
        assert_eq!(
            down[0].arrives_at, None,
            "the arrival of an air-bound item is the radio's answer, not the queue's"
        );
        assert!(q.is_empty());
        assert_eq!(q.forwarded(), 2);
    }

    /// The SPaT and MAP lengths come from the size model, and they differ — because they
    /// are two different messages with two different rows. A WSA has neither a row nor an
    /// encoder, so it has no length at all.
    #[test]
    fn spat_and_map_are_sized_by_the_size_model_and_a_wsa_is_not() {
        let mut unit = RsuRuntime::with_roles(NodeId::new(1), RsuRoles::all(), 0);
        let (spat, spat_modelled) = unit.payload_length(MsgType::Spat).expect("a SPaT row");
        let (map, map_modelled) = unit.payload_length(MsgType::Map).expect("a MAP row");
        assert!(spat > 0 && map > 0);
        assert_ne!(spat, map);
        assert!(spat_modelled && map_modelled, "both are modelled lengths");

        assert!(unit.payload_length(MsgType::Wsa).is_none());
        assert!(unit.config().wsa_payload_bytes.is_none());

        // Real octets handed in win, and they are no longer reported as modelled.
        unit.set_payload(MsgType::Spat, vec![1, 2, 3, 4, 5]);
        assert_eq!(unit.payload_length(MsgType::Spat), Some((5, false)));
        assert_eq!(unit.payload_bytes(MsgType::Spat, 5), vec![1, 2, 3, 4, 5]);
        // And removing the override returns the unit to the model.
        unit.set_payload(MsgType::Spat, Vec::new());
        assert_eq!(unit.payload_length(MsgType::Spat), Some((spat, true)));
    }

    /// An installed list is sized by the backend, not by this unit, and the length is not
    /// reported as a modelled one.
    #[test]
    fn an_installed_list_carries_the_backends_own_length() {
        let mut unit = RsuRuntime::with_roles(
            NodeId::new(1),
            RsuRoles::NONE.with(RsuRole::CrlDistribution),
            0,
        );
        assert!(unit.payload_length(MsgType::Crl).is_none());
        unit.install_crl(4_000, 0);
        assert_eq!(unit.payload_length(MsgType::Crl), Some((4_000, false)));
    }
}
