//! The OBU runtime: the thing that turns a vehicle into a communicating station.
//!
//! # What a node is
//!
//! A node is a hardware profile, a set of queues and servers sized by it, a clock, a
//! position belief, a set of stores, a verification policy and a pair of message timers.
//! [`ObuRuntime::step`] drives all of it once per engine tick: generate, sign, hand to the
//! network layer; receive, verify, update the stores; and once per telemetry window,
//! report.
//!
//! # What a node is not
//!
//! It is not a view onto the simulation. [`ObuRuntime`] implements
//! [`v2xw_core::nodeview::NodeView`], and everything a detector, a generator, a safety
//! application or an attacker can see about this node is that trait's surface: its own id,
//! its own believed time, its own position estimate, its own credentials, its neighbour
//! table and what it has received. There is no accessor here that returns the truth,
//! because there is no way for this crate to obtain the truth: [`crate::ctx::NodeCtx`] has
//! no `world()` and no `actors()`.
//!
//! Build decision D11 is explicit that this firewall is strong but **not compile-proof** —
//! an implementor can still smuggle ground truth in through an associated type — and that
//! enforcement therefore belongs in a conformance sentinel. [`crate::firewall`] is that
//! sentinel. The claim made here is the one that is true: the ordinary route is closed,
//! and the unusual route is checked by a test that has been shown to fail when the route
//! is taken.
//!
//! # The two ground-truth values that do cross
//!
//! Two quantities in the telemetry record are marked **GT** in vwp-v1 §3.5.2:
//! `clock_offset_ns` and `pos_error_m`. Both are differences between a belief and a truth,
//! so neither can be computed inside the firewall. They enter through
//! [`ObuRuntime::observe_truth`], which the engine calls from *outside* — it is the
//! engine, not the node, that knows both numbers — and they are stored in a field that
//! nothing but the telemetry path reads. The sentinel checks that too, because a
//! ground-truth value that arrived legitimately and was then used illegitimately is
//! exactly the leak the firewall exists to stop.

use std::collections::{BTreeMap, VecDeque};

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::Dims;
use v2xw_core::ids::NodeId;
use v2xw_core::nodeview::NodeView;
use v2xw_core::time::{Duration, SimTime, WallClock};
use v2xw_msg::MsgType;
use v2xw_msg::cam::{self, ParticipantType};
use v2xw_msg::codec::{EtsiUperCodec, Message, MessageCodec};
use v2xw_msg::generator::DccState;
use v2xw_msg::j2735::bsm;
use v2xw_msg::sec_types::HashedId8;
use v2xw_record::wire::telemetry::NodeTelemetry;

use crate::clock::ClockModel;
use crate::ctx::{NodeCtx, NodeCtxExt};
use crate::generate::{MessageSchedule, ServiceSet};
use crate::policy::{
    PolicyView, Prioritized, RxSummary, VerificationPolicy, VerifyDecision, VerifyDecisionRecord,
};
use crate::profile::{HardwareProfile, RunsOn};
use crate::queue::{Admission, DropCause, DropLedger, NodeQueue, QueueKind, Queued};
use crate::secure::{CryptoMode, NodeSecurity, PSID_SAFETY, SignedFrame, SpduVerdict};
use crate::server::{OpDescriptor, ProfileServiceModel, ServerBank, ServiceModel};
use crate::stores::{
    CredentialHandle, Neighbor, NeighborTable, PeerCertCache, Stores, VerificationState,
};
use crate::telemetry::{NodeState, TelemetryInputs, TelemetryWindow, gnss_fix_code};

/// A frame handed to a node by the PHY.
#[derive(Debug, Clone, PartialEq)]
pub struct RxFrame {
    /// The signer's certificate digest, as the SPDU names it.
    pub signer: Option<HashedId8>,
    /// What the SPDU claims to carry.
    pub msg_type: MsgType,
    /// Frame size on the air.
    pub bytes: u32,
    /// The position the payload claims, in world ENU metres.
    pub claimed_pos: Option<v2xw_core::geom::Vec3>,
    /// The speed it claims, m/s.
    pub claimed_speed_mps: f64,
    /// The heading it claims, ENU radians.
    pub claimed_heading_rad: f64,
    /// The generation time it claims, on the *sender's* clock.
    pub claimed_generation_time: SimTime,
    /// Whether the SPDU attached a full certificate rather than a digest.
    pub full_certificate: bool,
    /// Whether the signature is in fact good.
    ///
    /// The engine computes this from the sender's real key when the scenario runs in
    /// `modeled` crypto mode; in `real` mode the crypto backend does. Either way the node
    /// only learns it by *spending the verification time*, which is what
    /// [`ObuRuntime::step`] charges — a node that skips the check never reads this field.
    pub signature_valid: bool,
    /// The i-period the signer's certificate claims, for the linkage-CRL check.
    pub claimed_cert_period: u32,
    /// The linkage value the signer's certificate carries.
    pub claimed_linkage: Option<v2xw_sec::linkage::LinkageValue>,
    /// The signed SPDU as it arrived on the air, when the engine carries the bytes.
    ///
    /// `Some` is the honest path: the node parses these bytes, resolves the signer's
    /// certificate out of its own cache and checks the signature with its own crypto
    /// backend, so [`RxFrame::signature_valid`] is not consulted at all. `None` is the
    /// legacy path, where the engine decided validity on the node's behalf and the node
    /// only pays for the check. See [`ObuRuntime::classify`].
    pub spdu: Option<Vec<u8>>,
}

/// A message this node received and has an opinion about — the
/// [`NodeView::Message`] of invariant I-C2.
///
/// Every field is a claim or an observation of this node's own. There is no true position
/// and no actor id.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedMessage {
    /// Who signed it, as far as this node can tell.
    pub signer: Option<HashedId8>,
    /// What it is.
    pub msg_type: MsgType,
    /// How big it was.
    pub bytes: u32,
    /// When this node believes it arrived.
    pub received_at: SimTime,
    /// The generation time it claims.
    pub claimed_generation_time: SimTime,
    /// The position it claims.
    pub claimed_pos: Option<v2xw_core::geom::Vec3>,
    /// The speed it claims.
    pub claimed_speed_mps: f64,
    /// The heading it claims.
    pub claimed_heading_rad: f64,
    /// What this node concluded about the signature.
    pub verification: VerificationState,
}

/// When and how a frame reached this node, handed in by the engine beside the frame.
///
/// Neither field is on the air and neither names the sender. `token` is an opaque handle
/// the engine uses to join this node's [`RxReport`] back to the reception attempt it is
/// about; `arrived_at` is the instant the last symbol reached this radio, which a real
/// receiver observes with its own clock. Passing them beside [`RxFrame`] rather than inside
/// it keeps every existing constructor of a frame valid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RxStamp {
    /// The engine's reception-attempt id, echoed back in the report.
    pub token: u64,
    /// When the frame finished arriving, on the simulation's timeline. `None` means "at
    /// the instant of the step that delivers it", which is how a harness that has no
    /// radio feeds a node.
    pub arrived_at: Option<SimTime>,
}

/// What finally became of one received frame at this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxDisposition {
    /// Handed to the applications, with what the node concluded about the signature.
    /// `Invalid` and `Revoked` are delivered as such (a detector wants to see them) and
    /// are *losses* to anything measuring delivery.
    Delivered(VerificationState),
    /// Discarded, for this reason.
    Dropped(DropCause),
    /// The node was switched off when the frame reached it.
    NodeOff,
}

/// One received frame's journey through this node, reported so the engine can record it on
/// `node.rx`. Every instant is on the node's **own clock**; the engine converts them to the
/// simulation's timeline with the arrival instant it handed in, so the durations between
/// them — which is what a latency decomposition needs — are exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxReport {
    /// The engine's token, from [`RxStamp::token`].
    pub token: u64,
    /// What became of it.
    pub disposition: RxDisposition,
    /// When it finished arriving.
    pub arrived: SimTime,
    /// When it was parsed and the verification policy decided.
    pub parsed: SimTime,
    /// When its signature check started, if it had one.
    pub verify_start: Option<SimTime>,
    /// When its signature check finished, if it had one.
    pub verify_done: Option<SimTime>,
}

/// What the verification queue holds beside each waiting frame.
#[derive(Debug, Clone, Copy)]
struct Waiting {
    token: u64,
    arrived: SimTime,
    parsed: SimTime,
    depth: u64,
}

/// A frame whose signature check has started and not yet finished: it reaches the
/// applications at `finish`, not before.
#[derive(Debug, Clone)]
struct Verifying {
    /// The message as the applications will receive it, with the node's conclusion.
    message: VerifiedMessage,
    /// The report the engine joins to the reception attempt.
    report: RxReport,
}

/// One message this node wants transmitted.
///
/// # The bytes, not a count
///
/// [`Transmission::bytes`] used to be the whole story, and the story was
/// `93 + signer identifier` with no payload at all. It is kept — every consumer reads it
/// and it is still exactly the frame's length — but it is now *derived* from
/// [`Transmission::signed`], which carries the encoded facilities-layer payload and the
/// IEEE 1609.2 SPDU that wraps it. A consumer that wants the split (and the `node.tx`
/// record wants it: `payload_bytes` and `envelope_bytes` are two of its columns) takes it
/// from there rather than recomputing an overhead nobody measured.
#[derive(Debug, Clone, PartialEq)]
pub struct Transmission {
    /// What it is.
    pub msg_type: MsgType,
    /// The signed size, bytes. Equal to `signed.bytes_on_wire()` whenever `signed` is
    /// present, which it is for every message a node's own generator produced.
    pub bytes: u32,
    /// The credential it was signed with.
    pub signer: HashedId8,
    /// Whether it attached the full certificate rather than a digest.
    pub full_certificate: bool,
    /// When the signature completed and the frame reached the transmit queue — the
    /// earliest the MAC could have it.
    pub ready_at: SimTime,
    /// The instant the payload claims, on this node's own clock.
    pub generation_time: SimTime,
    /// When the signer picked the message up, on this node's own clock: the end of its
    /// wait for the signing server, and the start of the signature.
    pub sign_start: SimTime,
    /// The real bytes: the encoded payload and the signed SPDU.
    ///
    /// `None` only for a frame the *engine* synthesised on a node's behalf — the
    /// misbehaviour-report and CRL-broadcast paths of `v2xw-engine`, which size a payload
    /// from a protocol table and never build one. Every frame a node's own generator
    /// produced carries `Some`, and an aggregation over `node.tx` that finds `None` is
    /// looking at a frame nobody encoded.
    pub signed: Option<SignedFrame>,
}

impl Transmission {
    /// The facilities-layer payload's length, for the `node.tx` record's `payload_bytes`.
    pub fn payload_bytes(&self) -> Option<u32> {
        self.signed.as_ref().map(SignedFrame::payload_bytes)
    }

    /// The security envelope's length, for the `node.tx` record's `envelope_bytes`.
    pub fn envelope_bytes(&self) -> Option<u32> {
        self.signed.as_ref().map(SignedFrame::envelope_bytes)
    }

    /// The attached certificate's octets inside the envelope (zero for a digest signer),
    /// for the `node.tx` record's `cert_bytes`.
    pub fn cert_bytes(&self) -> Option<u32> {
        self.signed.as_ref().map(|f| f.cert_bytes)
    }

    /// A transmission whose size is known but whose bytes are not — the shape the engine's
    /// own application-layer frames need.
    ///
    /// Named rather than a struct literal so that adding a field here is one edit in this
    /// crate instead of one in every caller.
    pub fn sized(
        msg_type: MsgType,
        bytes: u32,
        signer: HashedId8,
        full_certificate: bool,
        ready_at: SimTime,
        generation_time: SimTime,
    ) -> Transmission {
        Transmission {
            msg_type,
            bytes,
            signer,
            full_certificate,
            ready_at,
            generation_time,
            // A frame sized from a table is signed by the engine on the node's behalf with
            // no queue in front of the signer, so the signature starts at generation.
            sign_start: generation_time,
            signed: None,
        }
    }
}

/// What one step produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepOutcome {
    /// Frames for the network layer, in generation order.
    pub transmissions: Vec<Transmission>,
    /// Messages delivered to the applications, in arrival order.
    pub delivered: Vec<VerifiedMessage>,
    /// The telemetry record, when this step closed a window.
    pub telemetry: Option<NodeTelemetry>,
    /// Every received frame whose fate was settled in this step, with the instants of its
    /// journey through the node.
    pub rx_reports: Vec<RxReport>,
}

/// How a node is configured.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Which message services it runs.
    pub services: ServiceSet,
    /// Queue capacities, in [`QueueKind::ALL`] order.
    pub queue_capacity: [usize; 5],
    /// How many peer certificates it caches.
    pub peer_cache_capacity: usize,
    /// How many neighbours it tracks.
    pub neighbor_capacity: usize,
    /// How long between telemetry frames.
    pub telemetry_period: Duration,
    /// Transmit power, dBm.
    pub tx_power_dbm: f64,
    /// The primitive the signing and verification cost tables are keyed by.
    pub sign_op: &'static str,
    /// The verification primitive.
    pub verify_op: &'static str,
    /// The scenario's wall clock, for `generationTime`, `secMark` and certificate
    /// validity. Real messages carry real timestamps, so the node needs one.
    pub wall: WallClock,
    /// The geodetic anchor of the world's ENU frame.
    ///
    /// Both message formats carry latitude and longitude, so a node cannot encode one
    /// without knowing where `(0, 0, 0)` is. The default is the null island anchor, which
    /// encodes perfectly well and is obviously wrong in a map view — better than a
    /// plausible city that silently misplaces every run that forgot to set it.
    pub origin: GeoOrigin,
    /// The vehicle's dimensions, for `vehicleLength`/`vehicleWidth` and `size`.
    pub dims: Dims,
    /// What kind of road user this is, for the CAM's `stationType`.
    pub station_type: ParticipantType,
    /// Which crypto backend signs and verifies.
    ///
    /// Phase 1 acceptance criterion 3: a run in `real` and a run in `modeled` must produce
    /// identical event logs apart from the manifest. That holds here because this field is
    /// the *only* thing either mode changes — see [`crate::secure::NodeCrypto`].
    pub crypto_mode: CryptoMode,
    /// The PSID a safety message is signed under. Changing it changes the envelope
    /// overhead; see [`crate::secure::PSID_SAFETY`].
    pub psid: u64,
    /// The BSM generator's inter-transmission times (SAE J2945/1; `messages.generator`).
    pub bsm_params: v2xw_msg::generator::BsmGenParams,
    /// The CAM generator's triggering rules (EN 302 637-2; `messages.generator`).
    pub cam_params: v2xw_msg::generator::CamGenParams,
    /// Whether the node's facilities layer is ETSI's (the GeoNetworking/BTP stack): an
    /// SRM or SSM then goes out as a SREM or SSEM, with the ETSI `ItsPduHeader` in front.
    pub etsi_facilities: bool,
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig {
            services: ServiceSet::BOTH,
            // No profile publishes a queue depth — every one of the ten carries
            // `hsm.queue_depth` as uncalibrated — so these are engine defaults, not device
            // figures, and a scenario that cares must set them. 64 is one second of
            // arrivals from 6 neighbours at 10 Hz, which is enough that the queue is not
            // the first thing to saturate in a small scenario and small enough that it
            // does saturate in a large one.
            queue_capacity: [64, 64, 64, 32, 16],
            peer_cache_capacity: 128,
            neighbor_capacity: 256,
            telemetry_period: Duration::from_secs(1),
            tx_power_dbm: 20.0,
            sign_op: "ecdsa-p256-sign",
            verify_op: "ecdsa-p256-verify",
            wall: WallClock::default(),
            origin: GeoOrigin::new(0.0, 0.0, 0.0),
            dims: Dims::CAR,
            station_type: ParticipantType::PassengerCar,
            crypto_mode: CryptoMode::Modeled,
            psid: PSID_SAFETY,
            bsm_params: v2xw_msg::generator::BsmGenParams::j2945_1(),
            cam_params: v2xw_msg::generator::CamGenParams::en302637_2(),
            etsi_facilities: false,
        }
    }
}

/// An on-board unit.
pub struct ObuRuntime {
    /// Received envelopes that would not parse.
    spdu_parse_failures: u64,
    /// Received envelopes that parsed but whose signature did not verify.
    spdu_signature_failures: u64,
    node: NodeId,
    config: NodeConfig,
    service: ProfileServiceModel,
    cpu: ServerBank,
    hsm: ServerBank,
    /// A separate hardware engine, where the profile says one exists.
    ///
    /// On the reference OBU the FIPS security policy is explicit that the >2,500/s
    /// verification engine is an on-chip block *outside* the certified eHSM boundary
    /// (06-node-models.md §7.2), so signing on the Cortex-M0 and verifying on the engine
    /// do not contend. Lumping them into one bank would make a node that signs at 110/s
    /// appear to slow its own 2,500/s verification path, which is a modelling artefact
    /// and not a property of the part.
    accel: ServerBank,
    queues: [NodeQueue<Queued<RxFrame>>; 5],
    /// Beside every frame in the verification queue (`queues[1]`), in the same order: the
    /// token and the instants its report needs.
    verify_meta: VecDeque<Waiting>,
    /// Checks in progress, keyed by (finish instant, start order): what the applications
    /// receive once each finishes. A check that has started has been charged to its server
    /// and its verdict is fixed, but the message is not the applications' until the server
    /// is done with it.
    verifying: BTreeMap<(SimTime, u64), Verifying>,
    /// The start-order tie-break for [`ObuRuntime::verifying`].
    verify_seq: u64,
    /// When each frame still waiting to be parsed will start its parse, on the node's
    /// clock — the receive queue's occupancy, when parsing has a cost.
    parse_starts: VecDeque<SimTime>,
    drops: DropLedger,
    clock: ClockModel,
    belief: PositionEstimate,
    stores: Stores,
    policy: Box<dyn VerificationPolicy>,
    schedule: MessageSchedule,
    /// The event-driven and infrastructure services (DENM, SPaT, MAP, SRM, SSM).
    events: crate::events::EventServices,
    state: NodeState,
    received: Vec<VerifiedMessage>,
    evidence_capacity: usize,
    window: TelemetryWindow,
    dcc: DccState,
    dcc_state_code: u16,
    /// The node's own security stack: the envelope, the crypto backend, the signer.
    security: NodeSecurity,
    /// The ETSI codec. Stateless, but it carries the model card that says *which* ASN.1
    /// modules produced these bytes, which is what a manifest pins.
    etsi: EtsiUperCodec,
    /// Relevance scores a safety application published, by signer digest.
    ///
    /// 06-node-models.md §2.1 specifies the `on-demand` policy as "verify only messages
    /// that a safety application marks relevant", and until an application existed nothing
    /// produced a mark: every [`crate::policy::RxSummary::relevance`] was `None` and an
    /// `on-demand` node verified only strangers. [`crate::safety::SafetyAppSet`] is the
    /// producer and [`ObuRuntime::set_relevance`] is how the engine installs what it
    /// produced.
    ///
    /// A [`BTreeMap`] keyed by the digest bytes, for the reason
    /// [`crate::stores::NeighborTable`] is: an ordering that reaches a decision may not
    /// come from a hash container (02-architecture.md §6.4).
    relevance: BTreeMap<[u8; 8], f64>,
    /// The two §3.5.2 fields marked **GT**, handed in from outside the firewall by
    /// [`ObuRuntime::observe_truth`] and read by nothing but the telemetry path.
    gt_pos_error_m: f32,
}

impl core::fmt::Debug for ObuRuntime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ObuRuntime")
            .field("node", &self.node)
            .field("profile", &self.service.profile().id)
            .field("state", &self.state)
            .field("policy", &self.policy.card().id)
            .finish_non_exhaustive()
    }
}

impl ObuRuntime {
    /// A node on `profile`, with `policy` and `config`, starting at `at`.
    pub fn new(
        node: NodeId,
        profile: HardwareProfile,
        policy: Box<dyn VerificationPolicy>,
        config: NodeConfig,
        at: SimTime,
    ) -> Self {
        let cpu_servers = profile.cpu.cores.or(1).max(1);
        // Every profile carries `hsm.servers` as uncalibrated except obu/cohda-mk5, where
        // §7.1 states the single-server assumption; the fallback is therefore one, and it
        // is the pessimistic reading rather than a guess at parallelism nobody published.
        let hsm_servers = profile.hsm.servers.or(1).max(1);
        let service = ProfileServiceModel::new(profile.clone());
        let stores = Stores {
            peers: PeerCertCache::new(config.peer_cache_capacity),
            neighbors: NeighborTable::new(config.neighbor_capacity),
            ..Default::default()
        };
        ObuRuntime {
            spdu_parse_failures: 0,
            spdu_signature_failures: 0,
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
            verify_meta: VecDeque::new(),
            verifying: BTreeMap::new(),
            verify_seq: 0,
            parse_starts: VecDeque::new(),
            drops: DropLedger::new(),
            clock: ClockModel::new(0.0),
            belief: PositionEstimate::no_fix(at),
            stores,
            policy,
            schedule: MessageSchedule::new(config.services)
                .with_params(config.cam_params, config.bsm_params),
            events: crate::events::EventServices::default(),
            state: NodeState::Active,
            received: Vec::new(),
            evidence_capacity: 256,
            window: TelemetryWindow::new(at),
            dcc: DccState::UNRESTRICTED,
            dcc_state_code: v2xw_record::wire::U16_NONE,
            relevance: BTreeMap::new(),
            gt_pos_error_m: f32::NAN,
            security: NodeSecurity::new(config.wall, config.crypto_mode, config.psid),
            etsi: EtsiUperCodec::new(),
            service,
            config,
        }
    }

    /// Runs this node at the abstract compute tier: every cryptographic operation its
    /// profile costs takes [`crate::server::ProfileServiceModel::UNLIMITED_COST`], so it
    /// is never compute-bound (`nodes.compute_tier: abstract`).
    pub fn set_compute_unlimited(&mut self) {
        self.service = self.service.clone().unlimited();
    }

    /// Runs the node's CPU as `servers` FIFO servers.
    ///
    /// 06-node-models §2.1's tiers: `medium` is one CPU server and one HSM server, `high`
    /// is `c` CPU cores — the profile's `cpu.cores`, which is what [`ObuRuntime::new`]
    /// builds. The engine sets one at the medium compute tier.
    pub fn set_cpu_servers(&mut self, servers: u32) {
        self.cpu = self.cpu.clone().resized(servers);
    }

    /// How many CPU servers the node runs.
    pub fn cpu_servers(&self) -> usize {
        self.cpu.servers()
    }

    /// Sets the cost of a parse, a detector pass or a neighbour-table task on this node's
    /// CPU — the `app_task_us` every shipped profile carries as uncalibrated.
    ///
    /// With no cost (the default) a received frame is parsed the instant it arrives and the
    /// receive queue never holds anything; with one, frames wait for the CPU and the queue
    /// can overflow.
    pub fn set_app_task_cost(&mut self, cost: Duration) {
        self.service = self.service.clone().with_app_task_cost(cost);
    }

    /// The node's security stack, for a test that wants to check a signature this node
    /// produced or ask which backend is running.
    pub fn security(&self) -> &NodeSecurity {
        &self.security
    }

    /// The security stack, mutably, for a credential protocol installing real credentials.
    pub fn security_mut(&mut self) -> &mut NodeSecurity {
        &mut self.security
    }

    /// The node's stores (03-interfaces.md §8).
    pub fn stores(&self) -> &Stores {
        &self.stores
    }

    /// The node's stores, mutably, for the engine's provisioning and CRL paths.
    pub fn stores_mut(&mut self) -> &mut Stores {
        &mut self.stores
    }

    /// The hardware profile (03-interfaces.md §8).
    pub fn profile(&self) -> &HardwareProfile {
        self.service.profile()
    }

    /// What state the node is in.
    pub fn state(&self) -> NodeState {
        self.state
    }

    /// Moves the node to another state.
    pub fn set_state(&mut self, state: NodeState) {
        self.state = state;
    }

    /// The node's clock model.
    pub fn clock(&self) -> &ClockModel {
        &self.clock
    }

    /// The node's clock model, mutably, for a scenario event or an attacker's step.
    pub fn clock_mut(&mut self) -> &mut ClockModel {
        &mut self.clock
    }

    /// The message schedule.
    pub fn schedule(&self) -> &MessageSchedule {
        &self.schedule
    }

    /// Hands the node this step's position belief, from the GNSS model.
    ///
    /// The GNSS model is the only thing entitled to hold both the truth and the belief;
    /// what arrives here is the belief alone, and the node has no way to ask what it was
    /// derived from.
    pub fn set_belief(&mut self, belief: PositionEstimate) {
        self.belief = belief;
    }

    /// Sets the DCC state the generators honour.
    pub fn set_dcc(&mut self, dcc: DccState, state_code: u16) {
        self.dcc = dcc;
        self.dcc_state_code = state_code;
    }

    /// Installs the SPaT or MAP payload a roadside unit's controller feed produced; the
    /// unit's schedule signs and sends it at the standard's rate ([`crate::events`]).
    pub fn set_infra_payload(&mut self, msg_type: MsgType, bytes: Vec<u8>) {
        self.events.set_infra_payload(msg_type, bytes);
    }

    /// The vehicle's own longitudinal acceleration, m/s², from its own accelerometer —
    /// the input the hard-braking DENM trigger reads ([`crate::events`]). Like the
    /// position belief it is the vehicle's own sensor, handed in from outside.
    pub fn set_own_acceleration(&mut self, a_mps2: f64) {
        self.events.set_own_acceleration(a_mps2);
    }

    /// The event and infrastructure services' state, for a test or a report.
    pub fn events(&self) -> &crate::events::EventServices {
        &self.events
    }

    /// Installs the relevance scores a safety application published
    /// ([`crate::safety::SafetyAppSet::relevance`]).
    ///
    /// This is what closes the `on-demand` policy's open input. The map is replaced
    /// wholesale rather than merged, because a subject that no application scored this
    /// step is a subject no application considers relevant now — merging would leave a
    /// warning's score in force after the warning cleared.
    pub fn set_relevance(&mut self, relevance: BTreeMap<[u8; 8], f64>) {
        self.relevance = relevance;
    }

    /// The relevance score this node holds for one signer, if any.
    #[must_use]
    pub fn relevance_of(&self, signer: &v2xw_msg::sec_types::HashedId8) -> Option<f64> {
        self.relevance
            .get(&crate::safety::digest_key(signer))
            .copied()
    }

    /// Hands in the two ground-truth differences §3.5.2 asks for, from outside the
    /// firewall.
    ///
    /// The engine computes both, because only the engine holds a belief and a truth at the
    /// same time. They are written straight into the telemetry path and read by nothing
    /// else — see the module documentation, and [`crate::firewall`] for the test.
    pub fn observe_truth(&mut self, pos_error_m: f32) {
        self.gt_pos_error_m = pos_error_m;
    }

    /// One engine tick.
    ///
    /// The order matters and is the order of 06-node-models.md §2.1: the clock advances
    /// first so that everything in the step shares one belief about the time; the inbox is
    /// admitted and policed; verification is charged against the servers; the stores are
    /// updated from what verification concluded; generation is checked against the node's
    /// own belief and signed; and the window is closed if it is due.
    pub fn step(
        &mut self,
        ctx: &mut dyn NodeCtx,
        inbox: Vec<RxFrame>,
        distance_travelled_m: f64,
    ) -> StepOutcome {
        let stamped = inbox.into_iter().map(|f| (f, RxStamp::default())).collect();
        self.step_timed(ctx, stamped, distance_travelled_m)
    }

    /// [`ObuRuntime::step`], with each frame's arrival instant and the engine's token for
    /// it.
    ///
    /// # Reception in continuous time
    ///
    /// The step runs at the engine's tick, but a radio receives whenever a frame ends, and
    /// the verification queue is a queue in continuous time. So the frames are taken in
    /// arrival order and each is placed at the instant it arrived: the verification server
    /// is advanced to that instant first — every waiting check that would have started by
    /// then starts, at the instant it would have — and only then is the frame parsed,
    /// offered to the policy (which sees the queue as it was at that instant) and queued.
    /// At the end of the step the server is advanced to the step's own instant.
    ///
    /// What is still waiting stays in the queue across steps, so a node that cannot keep up
    /// carries a real backlog, its queue-overflow drops happen at the instant the queue was
    /// full, and a verification's wait is the wait it would have had. A message reaches the
    /// applications when its check *finishes*: a check still running at the end of the step
    /// is held, [`ObuRuntime::next_completion`] says when it finishes, and the engine wakes
    /// the node then ([`ObuRuntime::wake_timed`]). Until 2026-09-24 it was delivered when
    /// it started, up to one verification service time early.
    pub fn step_timed(
        &mut self,
        ctx: &mut dyn NodeCtx,
        inbox: Vec<(RxFrame, RxStamp)>,
        distance_travelled_m: f64,
    ) -> StepOutcome {
        let now = ctx.now();
        self.clock.advance(now, self.belief.fix.has_position());
        let believed = self.clock.believed_time(now);

        let mut out = StepOutcome::default();
        if self.state == NodeState::Off {
            self.switched_off(believed, inbox, &mut out);
            return out;
        }

        self.receive(ctx, believed, inbox, &mut out);
        self.stores.neighbors.age(believed);
        self.stores.certs.travelled(distance_travelled_m);
        self.stores.certs.sweep(believed, &self.stores.crl);
        let _ = self.stores.certs.rotate(believed);

        if self.state.transmits() {
            self.generate(ctx, believed, &mut out);
        }

        if self.window.length(now) >= self.config.telemetry_period {
            out.telemetry = Some(self.close_window(ctx, now));
        }
        out
    }

    /// Wakes the node between its periodic steps, to hand over what has finished.
    ///
    /// A signature check that started during a step usually finishes after it, and its
    /// message is not the applications' until it does. The engine calls this at
    /// [`ObuRuntime::next_completion`]: the frames that have arrived by now are received
    /// in continuous time exactly as [`ObuRuntime::step_timed`] receives them, every check
    /// that has finished is delivered, and nothing else happens — no generation, no
    /// pseudonym rotation, no telemetry window, which are the periodic step's.
    pub fn wake_timed(
        &mut self,
        ctx: &mut dyn NodeCtx,
        inbox: Vec<(RxFrame, RxStamp)>,
    ) -> StepOutcome {
        let now = ctx.now();
        self.clock.advance(now, self.belief.fix.has_position());
        let believed = self.clock.believed_time(now);
        let mut out = StepOutcome::default();
        if self.state == NodeState::Off {
            self.switched_off(believed, inbox, &mut out);
            return out;
        }
        self.receive(ctx, believed, inbox, &mut out);
        out
    }

    /// When the next signature check in progress finishes, on this node's own clock — the
    /// instant the engine must wake it for ([`ObuRuntime::wake_timed`]). `None` when no
    /// check is running.
    #[must_use]
    pub fn next_completion(&self) -> Option<SimTime> {
        self.verifying.keys().next().map(|(finish, _)| *finish)
    }

    /// [`ObuRuntime::next_completion`] on the simulation's timeline, seen from `now`: the
    /// node's clock stands a fixed offset from the truth between two of its steps, so the
    /// interval to the finish is the same on both. Never before `now`.
    #[must_use]
    pub fn next_completion_after(&self, now: SimTime) -> Option<SimTime> {
        let finish = self.next_completion()?;
        let believed = self.clock.believed_time(now);
        Some(now.saturating_add(finish.saturating_sub(believed)))
    }

    /// Everything a switched-off node was handed, and every check it had in progress, is
    /// reported as reaching a node that was off, rather than vanishing.
    fn switched_off(
        &mut self,
        believed: SimTime,
        inbox: Vec<(RxFrame, RxStamp)>,
        out: &mut StepOutcome,
    ) {
        for (_, v) in core::mem::take(&mut self.verifying) {
            out.rx_reports.push(RxReport {
                disposition: RxDisposition::NodeOff,
                ..v.report
            });
        }
        {
            for (_, stamp) in inbox {
                let at = stamp
                    .arrived_at
                    .map_or(believed, |t| self.clock.believed_time(t));
                out.rx_reports.push(RxReport {
                    token: stamp.token,
                    disposition: RxDisposition::NodeOff,
                    arrived: at,
                    parsed: at,
                    verify_start: None,
                    verify_done: None,
                });
            }
        }
    }

    fn receive(
        &mut self,
        ctx: &mut dyn NodeCtx,
        believed: SimTime,
        inbox: Vec<(RxFrame, RxStamp)>,
        out: &mut StepOutcome,
    ) {
        // Each frame at the instant it arrived, on this node's clock, in arrival order.
        // The engine hands frames over in arrival order already; the stable sort makes that
        // a property of this function rather than of its caller.
        let mut frames: Vec<(RxFrame, RxStamp, SimTime)> = inbox
            .into_iter()
            .map(|(f, stamp)| {
                let at = stamp
                    .arrived_at
                    .map_or(believed, |t| self.clock.believed_time(t))
                    .min(believed);
                (f, stamp, at)
            })
            .collect();
        frames.sort_by_key(|(_, _, at)| *at);

        let parse_op = OpDescriptor::task("spdu-parse", crate::server::OpClass::Parse);
        let parse_cost = self.service.service_time(ctx, &parse_op);

        for (frame, stamp, arrived) in frames {
            self.window.message_in();

            // 1. The receive queue and the parse. With no parse cost in the profile (every
            //    shipped profile carries it as uncalibrated) parsing is instantaneous and
            //    the receive queue is always empty; with one, frames wait for the CPU and
            //    the queue can overflow.
            let parsed = match parse_cost {
                None => arrived,
                Some(cost) => {
                    while self.parse_starts.front().is_some_and(|&t| t <= arrived) {
                        self.parse_starts.pop_front();
                    }
                    if self.parse_starts.len() >= self.queues[0].capacity() {
                        self.drops.record(DropCause::RxOverflow);
                        out.rx_reports.push(RxReport {
                            token: stamp.token,
                            disposition: RxDisposition::Dropped(DropCause::RxOverflow),
                            arrived,
                            parsed: arrived,
                            verify_start: None,
                            verify_done: None,
                        });
                        continue;
                    }
                    let sched = self.cpu.submit(arrived, cost);
                    if sched.start > arrived {
                        self.parse_starts.push_back(sched.start);
                    }
                    sched.finish
                }
            };

            // 2. Everything the verifier would have started by the time the policy looks, and
            //    every check that has finished by then handed to the applications — so the
            //    neighbour table the policy reads is the one it would really read.
            self.advance_verifications(ctx, parsed);
            self.complete_verifications(parsed, out);

            // 3. The policy, over the queue as it is at this instant.
            self.learn_or_request(&frame);
            let summary = RxSummary {
                signer: frame.signer.clone(),
                msg_type: frame.msg_type,
                bytes: frame.bytes,
                received_at: arrived,
                claimed_pos: frame.claimed_pos,
                // What a safety application said about this signer, if one ran and scored
                // it (06-node-models.md §2.1: the `on-demand` policy "verifies only
                // messages that a safety application marks relevant"). `None` when no
                // application is installed or none scored this signer, which makes an
                // `on-demand` node with no applications verify only strangers — the
                // correct degenerate behaviour rather than a silent "everything is
                // relevant".
                relevance: frame
                    .signer
                    .as_ref()
                    .and_then(|s| self.relevance.get(&crate::safety::digest_key(s)).copied()),
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
            if let Some(rec) = VerifyDecisionRecord::decided(
                self.node,
                parsed,
                policy_id(self.policy.code()),
                msg_type_name(frame.msg_type),
                &decision,
            ) {
                ctx.emit(rec);
            }

            match decision {
                VerifyDecision::Drop { cause } => {
                    self.drops.record(cause);
                    out.rx_reports.push(RxReport {
                        token: stamp.token,
                        disposition: RxDisposition::Dropped(cause),
                        arrived,
                        parsed,
                        verify_start: None,
                        verify_done: None,
                    });
                }
                VerifyDecision::DeliverUnverified { reason } => {
                    let _ = reason;
                    self.drops.record(DropCause::VerifyPolicySkip);
                    let m = self.to_message(&frame, arrived, VerificationState::Unverified);
                    self.deliver(m, out);
                    out.rx_reports.push(RxReport {
                        token: stamp.token,
                        disposition: RxDisposition::Delivered(VerificationState::Unverified),
                        arrived,
                        parsed,
                        verify_start: None,
                        verify_done: None,
                    });
                }
                VerifyDecision::Verify { .. } => {
                    let depth = self.queues[1].len() as u64;
                    let queued = Queued {
                        item: frame,
                        enqueued_at: parsed,
                    };
                    let admitted = if self.policy.oldest_drop() {
                        self.queues[1].push_evicting(queued)
                    } else {
                        self.queues[1].push(queued)
                    };
                    let waiting = Waiting {
                        token: stamp.token,
                        arrived,
                        parsed,
                        depth,
                    };
                    match admitted {
                        Admission::Queued => self.verify_meta.push_back(waiting),
                        Admission::Refused(refused) => {
                            self.overflow(ctx, &refused, waiting, out);
                        }
                        Admission::Evicted(evicted) => {
                            // The oldest waiting frame made room for this one: its report
                            // is the overflow, and this frame joins the back of the line.
                            if let Some(old) = self.verify_meta.pop_front() {
                                self.overflow(ctx, &evicted, old, out);
                            }
                            self.verify_meta.push_back(waiting);
                        }
                    }
                }
            }
        }

        self.advance_verifications(ctx, believed);
        self.complete_verifications(believed, out);
    }

    /// Hands every check that has finished by `until` to the applications, in the order
    /// they finished, with its report.
    ///
    /// This is the one place a verified message is delivered. It used to happen when the
    /// check *started*, up to one verification service time before the signature was
    /// actually known to be good; now it happens when it finishes, and a check still
    /// running at the end of a step waits for [`ObuRuntime::wake_timed`] or the next step.
    fn complete_verifications(&mut self, until: SimTime, out: &mut StepOutcome) {
        while let Some(entry) = self.verifying.first_entry() {
            if entry.key().0 > until {
                break;
            }
            let done = entry.remove();
            self.deliver(done.message, out);
            out.rx_reports.push(done.report);
        }
    }

    /// Reports one frame the verification queue refused or evicted.
    fn overflow(
        &mut self,
        ctx: &mut dyn NodeCtx,
        refused: &Queued<RxFrame>,
        waiting: Waiting,
        out: &mut StepOutcome,
    ) {
        self.drops.record(DropCause::VerifyOverflow);
        ctx.emit(VerifyDecisionRecord::overflowed(
            self.node,
            waiting.parsed,
            policy_id(self.policy.code()),
            msg_type_name(refused.item.msg_type),
        ));
        out.rx_reports.push(RxReport {
            token: waiting.token,
            disposition: RxDisposition::Dropped(DropCause::VerifyOverflow),
            arrived: waiting.arrived,
            parsed: waiting.parsed,
            verify_start: None,
            verify_done: None,
        });
    }

    /// The peer-to-peer certificate distribution path (IEEE 1609.2 clause 8).
    ///
    /// A message that attached its full certificate teaches this node the certificate; a
    /// message that named one by digest and missed the cache cannot be verified until
    /// P2PCD supplies it, and the request is counted. The counters are what make the
    /// certificate-attachment cadence of 05-protocols.md §2.4 — full certificate every
    /// 450 ms for J2945/1, once a second for ETSI — a measurable trade rather than a
    /// constant: attaching more often costs bytes on the air and saves requests.
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

    /// Starts every waiting signature check whose start instant is no later than `until`.
    ///
    /// The head of the line starts at `max(when it was queued, when a server frees up)`;
    /// if that is after `until` it is still waiting and so is everything behind it. A check
    /// that starts is charged to its server and classified, and waits in `verifying` until
    /// the instant it finishes ([`ObuRuntime::complete_verifications`] hands it over).
    fn advance_verifications(&mut self, ctx: &mut dyn NodeCtx, until: SimTime) {
        let probe = OpDescriptor::verify(self.config.verify_op, 0);
        if self.service.service_time(ctx, &probe).is_none() {
            // The profile costs no verification. Nothing is verified and nothing is
            // silently delivered as if it had been: the queue simply does not drain, which
            // shows up as a growing `q_verify` and a `verifications_per_s` of zero.
            return;
        }
        let where_ = self.service.runs_on(&probe);
        loop {
            let Some(head) = self.queues[1].iter().next() else {
                break;
            };
            let free = match where_ {
                RunsOn::Hsm => self.hsm.earliest_free(),
                RunsOn::Accelerator => self.accel.earliest_free(),
                RunsOn::Cpu => self.cpu.earliest_free(),
            };
            if head.enqueued_at.max(free) > until {
                break;
            }
            let Some(q) = self.queues[1].pop() else {
                break;
            };
            let meta = self.verify_meta.pop_front().unwrap_or(Waiting {
                token: 0,
                arrived: q.enqueued_at,
                parsed: q.enqueued_at,
                depth: 0,
            });
            let frame = q.item;
            // Charged over the bytes actually checked, as signing is over the bytes
            // actually signed.
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
                msg_type_name(frame.msg_type),
                self.config.verify_op,
                match verdict {
                    VerificationState::Verified | VerificationState::Revoked => "valid",
                    VerificationState::Invalid => "invalid",
                    // No certificate to check against: the node concluded nothing.
                    _ => "skipped",
                },
                meta.depth,
            ));
            // The verdict is fixed now, but the applications have it only when the server
            // is done: the message waits in `verifying` until `sched.finish`.
            let message = self.to_message(&frame, meta.arrived, verdict);
            let seq = self.verify_seq;
            self.verify_seq += 1;
            self.verifying.insert(
                (sched.finish, seq),
                Verifying {
                    message,
                    report: RxReport {
                        token: meta.token,
                        disposition: RxDisposition::Delivered(verdict),
                        arrived: meta.arrived,
                        parsed: meta.parsed,
                        verify_start: Some(sched.start),
                        verify_done: Some(sched.finish),
                    },
                },
            );
        }
    }

    /// What the node concludes about one frame, having spent the verification time.
    ///
    /// When the frame carries its SPDU the node does the real work: it parses the bytes,
    /// resolves the signer's certificate out of its own bounded cache, and checks the
    /// signature with its own crypto backend. [`RxFrame::signature_valid`] is not read at
    /// all on that path — it is the engine's opinion, and a receiver does not have access
    /// to one.
    ///
    /// When the frame carries no bytes the node falls back to that field, which is the
    /// state the engine is still in. The fallback is visible rather than silent: it is the
    /// only branch that reads `signature_valid`, and `tests/wire_bytes.rs` drives both.
    ///
    /// The revocation check is the bounded one: `authenticated` is the outcome of the
    /// signature check, so a frame whose signature failed never reaches the CRL with an
    /// attacker-chosen i-period in hand.
    fn classify(&mut self, ctx: &mut dyn NodeCtx, frame: &RxFrame) -> VerificationState {
        let authentic = match &frame.spdu {
            Some(bytes) => match self.verify_on_the_wire(ctx, bytes) {
                SpduVerdict::Valid => true,
                SpduVerdict::Invalid => false,
                // The node holds no certificate for this signer, so it has concluded
                // nothing. Answering `Invalid` would blame a peer for this node's own
                // empty cache; the P2PCD request is counted in `learn_or_request`.
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
        // Tell the gate which i-period this node believes it is in, before asking it to
        // judge a peer's.
        //
        // `CrlGate` refuses a claimed period further than its skew from `current_period`,
        // so that an attacker cannot make a receiver walk a hash chain for a period a
        // year away. That defence turns into a denial of service against honest traffic
        // if nothing ever tells the gate what period it is: it starts at 0 through
        // `Default`, every real certificate claims a provisioned period, the difference
        // exceeds the skew, and every frame is refused.
        //
        // Measured before this line existed: 98.23 % of received messages in a Phase 2
        // run landed in `Invalid`, only 0.73 % verified, and the detector suite reported
        // every one of them as a signature failure — 126 false misbehaviour reports and
        // one honest device revoked.
        //
        // A node's own active credential is the right source: it was provisioned for the
        // period the node is in, it is the node's own state rather than ground truth, and
        // it needs no new plumbing from the engine.
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
            // The node refused to spend work on an implausible claim, so it has learned
            // nothing about revocation and must not treat the certificate as clean.
            crate::stores::CrlVerdict::RefusedImplausiblePeriod { .. } => {
                VerificationState::Invalid
            }
        }
    }

    /// Parses an SPDU, resolves its signer's certificate and checks the signature.
    ///
    /// The certificate comes from exactly two places, and both are the node's own: the
    /// SPDU itself when the sender attached one, or this node's bounded peer cache when it
    /// named one by digest. A miss is [`SpduVerdict::Unverifiable`] — the P2PCD case —
    /// and never a rejection, because a receiver that has not been told a certificate has
    /// learned nothing about the message signed under it.
    fn verify_on_the_wire(&mut self, ctx: &mut dyn NodeCtx, bytes: &[u8]) -> SpduVerdict {
        let Some(parsed) = self.security.parse(bytes) else {
            self.spdu_parse_failures += 1;
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
        let v = self
            .security
            .verify_parsed(ctx, &parsed, &certificate, self.node);
        if v == SpduVerdict::Invalid {
            self.spdu_signature_failures += 1;
        }
        v
    }

    /// How many received SPDUs failed to parse at all.
    ///
    /// Split from the signature failures because the two have nothing in common: one is a
    /// malformed or truncated envelope, the other is a well-formed envelope whose
    /// signature does not check out. The aggregate `Invalid` count cannot tell them apart,
    /// and while it could not, two wrong hypotheses were pursued.
    #[must_use]
    pub const fn spdu_parse_failures(&self) -> u64 {
        self.spdu_parse_failures
    }

    /// How many received SPDUs parsed but whose signature did not verify.
    #[must_use]
    pub const fn spdu_signature_failures(&self) -> u64 {
        self.spdu_signature_failures
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

    fn deliver(&mut self, m: VerifiedMessage, out: &mut StepOutcome) {
        self.window
            .delivered(m.verification == VerificationState::Verified);
        self.events.on_delivered(&m);
        // The neighbour table is a table of *stations moving around this one*, and what
        // fills it is their awareness messages. A SPaT, a MAP or a signal request says
        // where a junction is, not where its sender is going, and a DENM describes an
        // event rather than a station; none of them makes its sender a neighbour.
        let describes_a_station = !matches!(
            m.msg_type,
            MsgType::Spat | MsgType::Map | MsgType::Srm | MsgType::Ssm | MsgType::Denm
        );
        if m.verification != VerificationState::Invalid
            && describes_a_station
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

    /// Builds, encodes, signs and queues whatever the schedule says is due.
    ///
    /// The order is the order a real stack does it in, and each step is real:
    ///
    /// 1. the timers decide, from the node's **belief** and the node's **clock**;
    /// 2. the active credential is turned into a real key and a real certificate, once per
    ///    pseudonym ([`crate::secure::NodeSecurity::provision`]);
    /// 3. the payload is built from the belief and encoded by the format's own encoder —
    ///    a CAM through the generated ETSI UPER bindings, a BSM through the hand-written
    ///    J2735 encoder that was cross-validated against `pycrate` over 235 vectors;
    /// 4. it is signed into an IEEE 1609.2 `SignedData` SPDU by the node's crypto backend;
    /// 5. the signature's *modelled* time is charged against the profile's own server
    ///    bank, so a slow signer queues and a node that cannot keep up falls behind.
    ///
    /// Step 3 is why a CAM and a BSM come out at different sizes. They are different
    /// formats carrying different fields; the only way they can agree to the byte is if
    /// neither was encoded.
    fn generate(&mut self, ctx: &mut dyn NodeCtx, believed: SimTime, out: &mut StepOutcome) {
        // The two arguments are the node's own clock and the node's own belief. Nothing
        // else is in scope, and `crate::firewall` checks that this stays true.
        let requests = self.schedule.due(believed, &self.belief, &self.dcc);
        let station_id = self.stores.certs.active().map(|c| {
            u32::from_be_bytes([c.digest.0[0], c.digest.0[1], c.digest.0[2], c.digest.0[3]])
        });
        let events = self
            .events
            .due(believed, &self.belief, self.schedule.services(), station_id);
        if requests.is_empty() && events.is_empty() {
            return;
        }
        let wanted = (requests.len() + events.len()) as u32;
        let Some(cred) = self.stores.certs.active().cloned() else {
            // No usable credential: a node on the CRL, or one whose pool has run out.
            // [CAMP-EE §2.2.10.2] — it stops transmitting rather than sending unsigned.
            self.drops.record_n(DropCause::TxOverflow, wanted);
            return;
        };
        // The credential protocol's stand-in: a real key and a real certificate for every
        // pseudonym the store holds, and each certificate's own digest written back into
        // the store so that what the node announces is what it can actually prove.
        //
        // Every pseudonym, not just the active one, and that is the difference between a
        // revocation that sticks and one that does not: a linked CRL revokes a
        // certificate, the node matches its own credentials against it *by digest*, and a
        // certificate that only came into existence at the moment the node rotated onto it
        // would let a revoked node walk away from its revocation by rotating.
        if !self.provision_all(ctx, believed) {
            self.drops.record_n(DropCause::TxOverflow, wanted);
            return;
        }
        if !self.security.set_active(cred.i_period, cred.j_index) {
            self.drops.record_n(DropCause::TxOverflow, wanted);
            return;
        }
        let Some(cred) = self.stores.certs.active().cloned() else {
            self.drops.record_n(DropCause::TxOverflow, wanted);
            return;
        };

        let mut built: Vec<(MsgType, SimTime, Option<Vec<u8>>)> =
            Vec::with_capacity(wanted as usize);
        for r in requests {
            built.push((
                r.msg_type,
                r.at,
                self.encode_payload(r.msg_type, believed, &cred),
            ));
        }
        for e in events {
            let ty = e.msg_type();
            let payload = self.events.encode(&e, believed, &cred, &self.config);
            built.push((ty, believed, payload));
        }
        for (msg_type, at, payload) in built {
            let Some(payload) = payload else {
                // The node could not build a conformant message — a belief with no fix, a
                // position outside the ASN.1's range, a clock before the 1609.2 epoch. It
                // transmits nothing rather than a payload that would not decode.
                self.drops.record(DropCause::TxOverflow);
                continue;
            };
            let r = (msg_type, at);
            let sid = self.security.signer_id_for(r.0, believed);
            // TS 103 097 §7.1.2: a DENM's envelope carries its generation location, and the
            // ETSI profile refuses to sign one without it. The node's own belief.
            let location = (r.0 == MsgType::Denm)
                .then(|| generation_location(&self.belief, self.config.origin));
            let Ok((frame, pdu)) = self.security.sign(ctx, r.0, &payload, sid, location) else {
                self.drops.record(DropCause::TxOverflow);
                continue;
            };
            // The cost is charged over the bytes actually signed, not over zero: a cost
            // table that ever grows a per-byte term will then be read correctly without
            // anyone having to remember to come back here.
            let op = OpDescriptor::sign(self.config.sign_op, frame.payload_bytes());
            let Some(cost) = self.service.service_time(ctx, &op) else {
                // The profile costs no signature. Nothing is signed for free.
                self.drops.record(DropCause::TxOverflow);
                continue;
            };
            let sched = match self.service.runs_on(&op) {
                RunsOn::Hsm => self.hsm.submit(believed, cost),
                RunsOn::Accelerator => self.accel.submit(believed, cost),
                RunsOn::Cpu => self.cpu.submit(believed, cost),
            };
            let full_certificate = pdu.signer_id == v2xw_sec::SignerIdChoice::Certificate;
            let bytes = frame.bytes_on_wire();
            let tx = Transmission {
                msg_type: r.0,
                bytes,
                signer: cred.digest.clone(),
                full_certificate,
                ready_at: sched.finish,
                generation_time: r.1,
                sign_start: sched.start,
                signed: Some(frame),
            };
            if let Admission::Refused(_) = self.queues[3].push(Queued {
                item: RxFrame {
                    signer: Some(cred.digest.clone()),
                    msg_type: r.0,
                    bytes,
                    claimed_pos: Some(self.belief.pos),
                    claimed_speed_mps: self.belief.ground_speed_mps(),
                    claimed_heading_rad: self.belief.heading_rad,
                    claimed_generation_time: r.1,
                    full_certificate,
                    signature_valid: true,
                    claimed_cert_period: cred.i_period,
                    claimed_linkage: None,
                    spdu: None,
                },
                enqueued_at: believed,
            }) {
                self.drops.record(DropCause::TxOverflow);
                continue;
            }
            let _ = self.queues[3].pop();
            if full_certificate {
                self.security.note_certificate_attached(r.0, believed);
            }
            // Air time is the PHY's to compute; until a scenario wires one in, the node
            // reports the byte count and leaves `airtime_ms_per_s` at zero contribution
            // rather than inventing a data rate.
            self.window.message_out(Duration::ZERO, tx.full_certificate);
            out.transmissions.push(tx);
        }
    }

    /// Issues a certificate for every credential the store holds and writes each one's own
    /// identity back into it. `false` if any of it failed.
    ///
    /// Until a `CredentialProtocol` ships, a credential arrives carrying
    /// [`crate::stores::pseudo_signer`]'s stand-in digest and a zero-filled `cert_coer` of
    /// the modelled length. Both are wrong in the only way that matters: the SPDU this node
    /// is about to sign names the *real* certificate, so the store has to name it too, or
    /// the digest a receiver sees will not be the digest the sender's store holds.
    ///
    /// Idempotent and therefore cheap after the first call: [`NodeSecurity::provision`]
    /// returns the handle it already made.
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

    /// Builds one message from the node's belief and encodes it with its own encoder.
    ///
    /// `None` when the belief cannot be expressed in the format — which is a real
    /// condition, not a defensive `unwrap`: `Latitude` and `Longitude` are constrained
    /// integers and a node whose world position falls outside the ellipsoid, or whose
    /// clock predates the 1609.2 epoch, has nothing conformant to send.
    ///
    /// The station identifier is the first four bytes of the credential's own
    /// `HashedId8`. That is not decoration: it means the identifier on the air changes
    /// exactly when the pseudonym changes, which is the property a linkability study
    /// measures. A station id derived from the node id would have made every pseudonym
    /// change trivially reversible.
    fn encode_payload(
        &self,
        msg_type: MsgType,
        believed: SimTime,
        cred: &CredentialHandle,
    ) -> Option<Vec<u8>> {
        let mut id = [0u8; 4];
        id.copy_from_slice(&cred.digest.0[..4]);
        match msg_type {
            MsgType::Cam => {
                let generation_time = cam::timestamp_its(self.config.wall, believed).ok()?;
                let input = cam::CamInput::new(
                    u32::from_be_bytes(id),
                    self.config.station_type,
                    self.belief,
                    self.config.origin,
                    self.config.dims,
                    generation_time,
                );
                let message = cam::build_cam(&input).ok()?;
                let encoded = self.etsi.encode(&Message::Cam(Box::new(message))).ok()?;
                debug_assert!(encoded.is_real(), "the ETSI codec produces real UPER bytes");
                Some(encoded.bytes)
            }
            MsgType::Bsm => {
                let input = bsm::BsmInput::new(
                    self.schedule.bsm_msg_count(),
                    id,
                    self.belief,
                    self.config.origin,
                    self.config.dims,
                    bsm::sec_mark(self.config.wall, believed),
                );
                let message = bsm::build_bsm(&input).ok()?;
                // A `MessageFrame`, because that is what goes in a WSM payload; the bare
                // PDU is three octets shorter and is not what a receiver decodes.
                let encoded = bsm::encode_message_frame(&message).ok()?;
                debug_assert!(encoded.is_real(), "the J2735 encoder produces real bytes");
                Some(encoded.bytes)
            }
            // A roadside unit's intersection messages: what its controller feed installed,
            // signed as it stands. A unit with no feed sends nothing rather than an empty
            // message.
            MsgType::Spat | MsgType::Map => self.events.infra_payload(msg_type).map(<[u8]>::to_vec),
            // Nothing else is generated here. A DENM, an SRM and an SSM are the event
            // services' (`crate::events`), and a CRL or a report is the engine's.
            _ => None,
        }
    }

    fn close_window(&mut self, _ctx: &mut dyn NodeCtx, now: SimTime) -> NodeTelemetry {
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
            storage_used_b: stores_bytes,
            storage_total_b: profile
                .flash_bytes
                .get()
                .copied()
                .unwrap_or(v2xw_record::wire::U64_NONE),
            // The top-up schedule belongs to the credential protocol, which is not this
            // crate's; until one is attached the field is the "none scheduled" sentinel
            // rather than a zero that would read as "overdue".
            next_topup_ns: v2xw_record::wire::U64_NONE,
            crl_bytes: self.stores.crl.bytes(&storage),
            outbox_bytes: self.stores.outbox.bytes(),
            clock_offset_ns: self.clock.offset_ns(),
            ram_used_kib: u32::try_from(
                stores_bytes.saturating_add(storage.baseline_ram_bytes) / 1024,
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
            outbox_msgs: self.stores.outbox.len() as u32,
            peer_cache_entries: self.stores.peers.len() as u32,
            p2pcd_requests: self.stores.peers.p2pcd_requests(),
            gnss_sigma_m: self.belief.semi_major_m as f32,
            gnss_hdop: f32::NAN,
            clock_drift_ppm: self.clock.drift_ppm() as f32,
            pos_error_m: self.gt_pos_error_m,
            cpu_util_pm: self.cpu.utilisation_pm(now),
            // §3.5.2 has one field for security hardware and this profile may have two
            // engines, so the binding one is reported: what a HUD needs to know is how
            // close the security path is to saturation, and that is the busier engine.
            hsm_util_pm: self
                .hsm
                .utilisation_pm(now)
                .max(self.accel.utilisation_pm(now)),
            queue_depths,
            dcc_state: self.dcc_state_code,
            cbr_pm: self.dcc.cbr.map_or(v2xw_record::wire::U16_NONE, |c| {
                (v2xw_core::math::quantize_to(c * 1000.0, 1.0) as u16).min(1000)
            }),
            tx_power_cdbm: i16::try_from(v2xw_core::math::quantize_to(
                self.config.tx_power_dbm * 100.0,
                1.0,
            ) as i64)
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
        self.schedule.reset_window();
        record
    }
}

/// The 1609.2 `ThreeDLocation` a DENM's envelope carries, from the node's own belief.
///
/// Latitude and longitude in tenths of a microdegree (1609.2 `NinetyDegreeInt`,
/// `OneEightyDegreeInt`). The 16-bit `Elevation` is decimetres with an offset of 4 096 so
/// that 0 is −409.6 m — **recalled, UNVERIFIED** against 1609.2 §6.4; it is clamped into
/// the range rather than wrapped.
fn generation_location(
    belief: &PositionEstimate,
    origin: v2xw_core::geo::GeoOrigin,
) -> v2xw_sec::envelope::GenerationLocation {
    let (lat, lon, alt) = origin.to_geodetic(belief.pos);
    let tenth_micro = |deg: f64, lim: f64| (deg.clamp(-lim, lim) * 1e7).round() as i32;
    let elevation = ((alt * 10.0).round() + 4_096.0).clamp(0.0, 61_439.0) as u16;
    v2xw_sec::envelope::GenerationLocation {
        lat_tenth_microdeg: tenth_micro(lat, 90.0),
        lon_tenth_microdeg: tenth_micro(lon, 180.0),
        elevation,
    }
}

fn policy_id(code: u8) -> &'static str {
    match code {
        0 => crate::policy::VERIFY_ALL_ID,
        1 => crate::policy::ON_DEMAND_ID,
        _ => crate::policy::PRIORITIZED_ID,
    }
}

fn msg_type_name(t: MsgType) -> &'static str {
    match t {
        MsgType::Cam => "cam",
        MsgType::Bsm => "bsm",
        MsgType::Denm => "denm",
        MsgType::Spat => "spat",
        MsgType::Map => "map",
        MsgType::Srm => "srm",
        MsgType::Ssm => "ssm",
        MsgType::Psm => "psm",
        MsgType::Vam => "vam",
        MsgType::Mbr => "mbr",
        MsgType::Crl => "crl",
        _ => "other",
    }
}

impl NodeView for ObuRuntime {
    type Neighbors = NeighborTable;
    type Credential = CredentialHandle;
    type Message = VerifiedMessage;

    fn node(&self) -> NodeId {
        self.node
    }

    fn believed_time(&self) -> SimTime {
        // The clock was advanced at the top of the step, so every plug-in that reads the
        // view within one step sees the same instant.
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

/// The default OBU: the reference profile of 06-node-models.md §7.2 with the prioritised
/// policy.
pub fn reference_obu(node: NodeId, at: SimTime) -> ObuRuntime {
    let profile = crate::profiles::get(crate::profiles::REFERENCE_OBU)
        .expect("the reference profile ships with the crate")
        .clone();
    ObuRuntime::new(
        node,
        profile,
        Box::new(Prioritized::new(300.0)),
        NodeConfig::default(),
        at,
    )
}
