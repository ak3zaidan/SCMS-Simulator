//! The main loop and the phase-parallel structure of ADR 0004.
//!
//! # Shape of a run
//!
//! One single-threaded event loop over one heap, and a small number of *phases* that are
//! pure maps merged in id order. The loop never runs two events at once; the phases never
//! touch the heap while they are mapping. That split is ADR 0004 decision 5, and it is
//! what makes thread count change wall time and nothing else.
//!
//! ```text
//! pop (time, priority, seq)
//!   ├─ Control      p0  scenario timeline: weather, outage, demand, parameter change
//!   ├─ MobilityStep p1  ── phase ──► map over actors (mobility provider), merge by ActorId
//!   │                    publish Kinematics, rebuild the snapshot, spawn/retire nodes
//!   ├─ PhyEnd       p3  ── phase ──► map over that frame's receivers, merge by NodeId
//!   ├─ PhyStart     p5  one frame goes on the air; its receiver set is resolved at PhyEnd
//!   ├─ NodePhase    p6  ── phase ──► map over nodes, merge by NodeId; node-local queues
//!   │                    are drained *inside* the map and never reach the heap
//!   └─ Observe      p9  metric flush, and the end-of-run sentinel
//! ```
//!
//! # The mobility step publishes the instant it is dispatched at
//!
//! A mobility step dispatched at `t` advances the world to `t + dt`, so what it produces
//! describes the *end* of the step. The phase therefore publishes in this order:
//!
//! ```text
//! MobilityStep at t
//!   1. gt.kinematics for every actor, stamped at t  (what the previous step produced)
//!   2. one VWP Keyframe or Delta at t               (the same state, binary, §3.3/§3.4)
//!   3. mobility.step(dt)                            (the world advances to t + dt)
//!   4. absorb, reindex, beliefs
//! ```
//!
//! Publishing the step's own result instead would file every record one mobility step
//! before the state it describes, against the record's own `t` field — which is what the
//! vertical-slice audit found. Steps 1 and 2 are two encodings of one fact and are
//! deliberately adjacent: `tests/wire.rs` checks they agree, which neither can satisfy by
//! being self-consistent.
//!
//! The binary stream is [`crate::snapshot`]. It is what a browser replays, and the Phase 1
//! build produced none of it at all: `RecordingWriter::write_frame` was called from
//! nowhere, so a recording carried JSON records and no snapshot frames.
//!
//! # Between mobility steps
//!
//! Mobility is periodic (ADR 0004 decision 2). A radio event at `t′` strictly inside a
//! step reads a position from the **published extrapolation rule**,
//! `pos(t′) = pos(t) + vel(t)·(t′ − t)` — [`v2xw_core::kinematics::Kinematics::extrapolate`],
//! which is part of the interface contract (03-interfaces.md §3, invariant I-M4) and not
//! an engine convenience. [`Engine::position_at`] is the one place that rule is applied, so
//! a second, subtly different extrapolation cannot appear in a second phase.
//!
//! # Time dilation
//!
//! 02-architecture.md §5.4: a scenario may declare windows in which only the backend and
//! the abstract mobility tiers run, so a 24-hour credential experiment finishes in
//! minutes. The engine implements that by **not generating radio events** inside a window:
//! `PhyStart` is not scheduled, so no frame, no reception and no verification cost exist
//! for that period. The windows are in the manifest, and [`RunReport::suppressed_frames`]
//! counts what was skipped, because a metric that silently reads zero inside a window is
//! indistinguishable from a channel that was quiet.
//!
//! # What this loop does not do yet
//!
//! Stated rather than stubbed:
//!
//! * **Interference and capture.** A reception outcome here is a link-budget decision
//!   against the noise floor, so SINR is SNR: concurrent frames do not raise each other's
//!   denominator. That is `v2xw-radio`'s `OfdmPhy` high tier and a `MacTimer`-driven MAC,
//!   and both are scheduled through the event classes this enum already carries.
//! * **The MAC.** A frame reaches the air after the signing latency plus one AIFS; there
//!   is no backoff, no CBR measurement and no DCC gate. `Event::MacTimer` is the class
//!   those live at.
//! * **Backend, credential protocol and detection.** `Event::NetDeliver` and
//!   `Event::FlowTimer` are their classes; nothing schedules them here.
//!
//! Each gap is a missing *model*, not a missing seam: the event class, the phase and the
//! record channel for each already exist.

use std::collections::BTreeMap;

use rayon::prelude::*;
use v2xw_core::card::Tier;
use v2xw_core::event::{EventClass, Scheduler};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{ActorId, FrameSeq, LinkKey, NodeId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::manifest::Manifest;
use v2xw_core::provenance::ProvenanceLog;
use v2xw_core::registry::{ParamSet, Registry};
use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};
use v2xw_core::time::{Duration, SimTime, WallClock};
use v2xw_core::weather::WeatherState;
use v2xw_metrics::channels::{ByteBucket, RxOutcome, SignerId, rx_cause};
use v2xw_mobility::{
    ActorSnapshot, DriverProfile, GnssEnv, GnssModel, Mobility, MobilityUpdate, VehicleClass,
    VehicleView,
};
use v2xw_msg::generator::DccState;
use v2xw_node::stores::VerificationState;
use v2xw_node::{
    NodeConfig, ObuRuntime, RxDisposition, RxFrame, RxReport, RxStamp, StepOutcome, Transmission,
};
use v2xw_record::{Cadence, Profile};
use v2xw_radio::{
    AccessCategory, Arrival, ChannelId, Dcc, EdcaOcbMac, FrameDescriptor, FrameKind,
    InterferenceSource, LossCause, Mac, MacSdu, Mcs, OfdmPhy, Phy, RadioEndpoint,
    RxHandle, SaeJ2945Dcc, SduRef, TxHandle,
};
use v2xw_world::World;

use crate::adapters::{BoxedFading, BoxedPropagation};
use crate::ctx::{EngineCtx, RunRecorder};
use crate::error::{EngineError, Result};
use crate::event::{Event, Observe};
use crate::records::{GtKinematics, MacCbr, NetBytes, NodeRx, NodeTx, PhyRx};
use crate::scenario::Scenario;
use crate::snapshot::{ActorState, SnapshotStream};

pub mod jamming;
pub mod sidelink;

/// The 5.9 GHz safety channel, and the frequency the link budget is evaluated at.
///
/// Channel 172 is the SAE J2945/1 safety channel in the US band plan; 5.86–5.93 GHz maps
/// it to 5.860 GHz + 5 MHz × (n − 172) with 10 MHz channels, which puts 172 at 5.860 GHz.
const SAFETY_CHANNEL: ChannelId = ChannelId(172);
/// The centre frequency of [`SAFETY_CHANNEL`], hertz.
const SAFETY_FREQ_HZ: f64 = 5.860e9;
/// The MCS every safety frame in this build is sent at.
///
/// 6 Mbit/s QPSK 1/2 is the J2945/1 `vDataRate` default and the rate every published
/// 802.11p PDR-versus-distance curve this engine is validated against was measured at
/// (04-models.md §4.2, §13). Nothing selects a different one yet, so it is a constant here
/// rather than a scenario field that would have exactly one legal value.
const SAFETY_MCS: Mcs = Mcs::R6Qpsk12;
/// The EDCA access category a safety message is queued in.
///
/// AC_VO, which is what a BSM or a CAM uses (04-models.md §4.3; EN 302 663 Annex C.4.2
/// puts DENM and CAM at AC_VO and AC_VI respectively and J2945/1 puts the BSM at the
/// highest category).
const SAFETY_AC: AccessCategory = AccessCategory::Vo;
/// The AIFS a safety frame waits before the PHY may start it, at the abstract tier.
///
/// EDCA AC_VO on a 10 MHz OCB channel is the *floor* on access delay. At the medium and
/// high tiers the MAC computes the whole of it — AIFS plus a contention-window countdown
/// — and this constant is not used; the abstract tier models no medium access at all, so
/// the floor is all there is. The value is AC_VI's rather than AC_VO's because it is the
/// number the Phase 1 build shipped and changing it would move every abstract-tier digest
/// for no modelling gain: `AIFS = SIFS + 2·slot = 32 µs + 2·13 µs = 58 µs`
/// [IEEE 802.11-2020 Table 9-155, 10 MHz timing].
const AIFS: Duration = Duration::from_micros(58);
/// How far a candidate receiver may be. The grid cell size equals this (ADR 0004
/// decision 6), so a neighbour query touches at most nine cells.
///
/// It is **not** a radio horizon: a candidate beyond the receiver sensitivity is evaluated
/// and lost as [`LossCause::BelowSensitivity`], which is what makes it appear in the
/// denominator of the packet delivery ratio 08-measurement-and-data.md §2.1 defines. It is
/// also the single biggest term in the cost of a dense run — see
/// [`RunReport::reception_attempts`] — because the candidate set inside a 1 km disc grows
/// with the square of the density.
const MAX_RANGE_M: f64 = 1000.0;
/// How many frames one [`Event::MacTimer`] may grant before it reschedules itself.
///
/// A bound, not a model: `Mac::poll` drains one frame per call and re-arms the queue, so a
/// node with a backlog would spin here. Eight is more than one generation period's worth
/// of BSMs at the fastest cadence J2945/1 admits, so the bound is never the reason a frame
/// waits, and a run that hit it would be a run whose MAC is not draining.
const MAX_GRANTS_PER_TIMER: u32 = 8;
/// The radius J2945/1 counts neighbours inside, metres.
///
/// [Rostami et al. 2018 Eq. 1, via 04-models.md §6.4]: `N` is "vehicles within 100 m". The
/// count is taken from the node's **own neighbour table** against its **own** position
/// estimate, so it is a belief and not a ground-truth density (invariant I-C2).
const J2945_DENSITY_RADIUS_M: f64 = 100.0;

/// Where a backhaul transfer's byte-accounting id starts.
///
/// Invariant I-N1 checks that no id is attributed to two buckets, and a `node.tx` record's
/// id is its frame number. Backhaul SDUs are numbered by their own counter, so they are
/// placed in a range no frame number reaches (2^40 frames is decades at any fleet size).
const BACKHAUL_ID_BASE: u64 = 1 << 40;

/// One node's MAC counters since its last `mac.cbr` report.
#[derive(Debug, Clone, Copy, Default)]
struct MacWindow {
    /// Frames handed to the MAC.
    frames: u64,
    /// Their PSDU octets.
    bytes: u64,
    /// Their air time, µs.
    airtime_us: u64,
    /// Frames the MAC refused.
    drops: u64,
}

/// The journey of a misbehaviour report to the roadside unit that forwards it, kept beside
/// the backhaul transfer so the whole flow can be traced when it reaches the authority.
#[derive(Debug, Clone, Copy)]
struct ReportJourney {
    msg: u64,
    t_generated: SimTime,
    t_sign_start: SimTime,
    t_signed: SimTime,
    t_tx_start: SimTime,
    t_tx_end: SimTime,
    t_arrival: SimTime,
}

/// What one run produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct RunReport {
    /// How many events of each class were dispatched, in class order.
    pub events_by_class: BTreeMap<String, u64>,
    /// How many mobility steps ran.
    pub mobility_steps: u64,
    /// How many actors were ever spawned.
    pub actors_spawned: u64,
    /// How many nodes were ever created.
    pub nodes_created: u64,
    /// How many frames went on the air.
    pub frames_transmitted: u64,
    /// How many reception attempts were evaluated: one per (frame, candidate receiver).
    ///
    /// This is the denominator of the packet delivery ratio (08-measurement-and-data.md
    /// §2.1) and the term that dominates the cost of a dense run: it grows with the number
    /// of frames times the number of candidates inside [`CANDIDATE_RANGE_M`] of each, so
    /// with the square of the vehicle count at fixed area.
    pub reception_attempts: u64,
    /// How many of those attempts decoded — the numerator of the packet delivery ratio.
    pub receptions_ok: u64,
    /// The attempts that did not decode, by the single loss cause invariant I-R3 allows.
    pub rx_losses: BTreeMap<String, u64>,
    /// How many frames were successfully received by at least one node.
    pub frames_received: u64,
    /// How many frames the MAC granted channel access to.
    pub mac_grants: u64,
    /// How many frames the MAC refused — a full access-category queue, or a frame over the
    /// MSDU cap.
    pub mac_drops: u64,
    /// The total channel-access delay over every granted frame, nanoseconds: the interval
    /// between the signature completing and the preamble going on the air. Divided by
    /// [`RunReport::mac_grants`] it is the mean access delay, which is the one number that
    /// says whether the MAC is doing anything.
    pub mac_access_delay_ns: u64,
    /// How many frames the PHY refused as too large for the MSDU cap
    /// (04-models.md §4.6: the fragmenter must have acted first, and none is wired in).
    pub phy_refusals: u64,
    /// How many signed messages were refused before the MAC because they exceeded the
    /// network layer's MTU (`fragmenter/none`: 1,400 octets for WSMP, 1,398 for
    /// GeoNetworking).
    pub net_mtu_refusals: u64,
    /// How many frames were *not* generated because the instant fell in a time-dilation
    /// window (02-architecture.md §5.4).
    pub suppressed_frames: u64,
    /// How many records the engine **emitted**.
    ///
    /// Not what was stored: a recorder can refuse a record the engine handed it (an
    /// undeclared channel, a full disk), and this counts the handing over.
    /// [`RunReport::records_written`] is what the recorder says it kept, and the two
    /// disagreeing is exactly the condition a run report has to be able to state.
    pub records: u64,
    /// How many records the context refused, by the visibility rule.
    pub records_refused: u64,
    /// How many records the **recorder** confirms it stored, when it can say.
    ///
    /// `None` means the recorder does not report a count — a wrapper that forwards
    /// [`crate::RunRecorder::write`] and nothing else, for instance. It is `None` rather
    /// than zero on purpose: "I do not know" and "nothing was written" are different
    /// facts and a report that conflated them would be the same defect one layer up.
    pub records_written: Option<u64>,
    /// How many VWP `Keyframe` frames the engine produced (vwp-v1 §3.3).
    pub keyframes: u64,
    /// How many VWP `Delta` frames the engine produced (vwp-v1 §3.4).
    pub deltas: u64,
    /// How many VWP frames the **recorder** confirms it stored, when it can say.
    ///
    /// Compared against `keyframes + deltas` it says whether the normative binary stream
    /// reached the artefact. A recording whose frame count is zero while the engine
    /// produced thousands is a recording a browser cannot replay, and that is now a
    /// number in the report rather than something an auditor has to discover.
    pub frames_written: Option<u64>,
    /// The instant the loop stopped at.
    pub end_ns: SimTime,
    /// What the Phase 2 path did, when a scenario declared one
    /// ([`crate::phase2`]). All zeroes when it did not.
    pub phase2: crate::phase2::Phase2Report,
    /// What the sidelink access layer did, when `radio.rat` selected LTE-V2X or NR-V2X.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidelink: Option<sidelink::SidelinkReport>,
}

impl RunReport {
    fn count(&mut self, class: EventClass) {
        *self.events_by_class.entry(class.to_string()).or_insert(0) += 1;
    }

    fn lost(&mut self, cause: LossCause) {
        *self
            .rx_losses
            .entry(cause_name(cause).to_string())
            .or_insert(0) += 1;
    }

    /// The packet delivery ratio over reception attempts, or `None` when nothing was
    /// attempted.
    ///
    /// Stated here rather than left to a caller's division, because the two numbers have
    /// to be the ones 08-measurement-and-data.md §2.1 pairs: attempts inside the candidate
    /// range as the denominator, decoded arrivals as the numerator. `frames_received`
    /// counts frames that reached *at least one* node and is a different quantity.
    #[must_use]
    pub fn pdr(&self) -> Option<f64> {
        // `then_some`, not `then`: the body is a cast and a division of two integers
        // already in hand, so there is nothing to defer and `clippy::unnecessary_lazy_
        // evaluations` says so. A zero denominator never reaches the division, because
        // the predicate is what guards it.
        (self.reception_attempts > 0)
            .then_some(self.receptions_ok as f64 / self.reception_attempts as f64)
    }
}

/// The kebab-case name a `phy.rx` record carries for a loss cause.
///
/// `LossCause` serialises kebab-case already, but a record field is a `&str` and going
/// through `serde_json` for one word per lost frame is a measurable cost at the densities
/// this engine is built for.
const fn cause_name(cause: LossCause) -> &'static str {
    match cause {
        LossCause::OutOfRange => "out-of-range",
        LossCause::BelowSensitivity => "below-sensitivity",
        LossCause::Collision => "collision",
        LossCause::PreambleMissed => "preamble-missed",
        LossCause::HalfDuplex => "half-duplex",
        LossCause::HiddenTerminal => "hidden-terminal",
        LossCause::Jammed => "jammed",
        LossCause::Fading => "fading",
        LossCause::InBandEmission => "in-band-emission",
        LossCause::ResourceCollision => "resource-collision",
        // `LossCause` is `#[non_exhaustive]`: a cause added upstream lands here rather
        // than failing the build, and reports itself as unknown rather than as something
        // it is not.
        _ => "unknown",
    }
}

/// Everything the engine knows about one actor that the mobility update does not repeat.
#[derive(Debug, Clone)]
struct ActorRecord {
    class: VehicleClass,
    driver: DriverProfile,
    node: Option<NodeId>,
    last: Kinematics,
}

/// A frame between the signature finishing and the last symbol arriving.
///
/// It lives in [`Engine::frames`] from the moment the node hands it down until its
/// `PhyEnd`, so one entry covers three states: waiting for the MAC, granted and on the
/// air, and being evaluated at its receivers.
#[derive(Debug, Clone)]
struct FrameState {
    tx: NodeId,
    /// The transmitter's ground-truth position at [`FrameState::start`], filled in when
    /// the MAC's grant fixes that instant.
    tx_pos: Vec3,
    bytes: u32,
    msg_type: v2xw_msg::MsgType,
    signer: v2xw_msg::sec_types::HashedId8,
    full_certificate: bool,
    generation_time: SimTime,
    claimed_pos: Vec3,
    claimed_speed_mps: f64,
    claimed_heading_rad: f64,
    /// When the signature completed: the earliest the MAC may have the frame.
    ready_at: SimTime,
    /// When the preamble goes on the air — the MAC's grant, not the ready instant.
    start: SimTime,
    /// `start + air`.
    end: SimTime,
    air: Duration,
    /// The frame as the PHY and the MAC see it, carrying the DCC-controlled power.
    descriptor: FrameDescriptor,
    /// The PHY's transmission handle, once [`Phy::begin_tx`] has issued one.
    tx_handle: Option<TxHandle>,
    /// The arrivals the engine registered with the PHY: receiver → (received power dBm,
    /// transmitter-to-receiver distance m). A `BTreeMap`, so every walk over the receiver
    /// set is in [`NodeId`] order without a sort.
    arrivals: BTreeMap<NodeId, (f64, f64)>,
    /// The i-period the signer's certificate belongs to, as the envelope states it.
    claimed_cert_period: u32,
    /// The linkage value the signer's certificate carries, when the credential the node
    /// signed with has one. This is what a CRL entry revokes, so it is the field the
    /// receiver's revocation check turns on (see [`crate::phase2`], joint 1).
    claimed_linkage: Option<v2xw_sec::linkage::LinkageValue>,
    /// The application payload a Phase 2 message carries, for the two messages the
    /// revocation path needs and no node runtime generates.
    app: Option<AppPayload>,
    /// The signed SPDU as it goes on the air, when the node's own generator built one.
    ///
    /// `None` for the two frames the engine synthesises on a node's behalf (the
    /// misbehaviour report and the CRL broadcast), which size a payload from a protocol
    /// table and never build one; the receiver then falls back to the engine's own
    /// validity decision, which is what [`v2xw_node::RxFrame::spdu`] documents.
    spdu: Option<Vec<u8>>,
    /// The facilities-layer payload's length in octets, when the node encoded one.
    ///
    /// This and [`FrameState::envelope_bytes`] are what make `node.tx`'s `payload_bytes`
    /// and `envelope_bytes` real. They were null for every frame of the Phase 1 build,
    /// which is why "security overhead as a fraction of airtime" — one of the three
    /// results this simulator exists to produce — could not be computed from a recording
    /// at all. `None` for a frame the engine sized from a protocol table rather than
    /// encoding, which is the same set of frames `spdu` is `None` for.
    payload_bytes: Option<u32>,
    /// The 1609.2 envelope's cost in octets: the SPDU less the payload.
    envelope_bytes: Option<u32>,
    /// The sidelink resource the transport block was sent on, when the access layer is a
    /// sidelink.
    sl_resource: Option<v2xw_radio::SlResource>,
    /// Co-slot sidelink transmissions at each receiver, declared as each one starts.
    sl_interferers: BTreeMap<NodeId, Vec<v2xw_radio::SlInterferer>>,
    /// Receivers inside a `high` focus region, whose reception the high-tier PHY rule
    /// decides (preamble capture) whatever the surrounding tier is.
    focus_high: std::collections::BTreeSet<NodeId>,
    /// Every octet of the PSDU by layer (`v2xw_net::frame`). `bytes` is the SPDU a
    /// receiver verifies; `layers.psdu_bytes()` is what went on the air, and what the air
    /// time is computed from.
    layers: v2xw_net::FrameLayers,
    /// Octets of an attached certificate inside the envelope, when the node built one.
    cert_bytes: Option<u32>,
    /// The generation instant on the simulation's timeline (`generation_time` is the
    /// sender's own clock, which is what the payload claims).
    t_generated: SimTime,
    /// When the sender's signer picked the message up, on the simulation's timeline.
    t_sign_start: SimTime,
    /// Of the channel-access delay, the AIFS, ns.
    mac_aifs_ns: u64,
    /// Of the channel-access delay, the backoff slots the MAC counted, ns.
    mac_backoff_ns: u64,
    /// What the message said, for the `node.tx` record ([`message_content`]).
    content: Option<v2xw_metrics::channels::MsgContentView>,
}

/// What a Phase 2 application message carries, beyond its length.
///
/// 06-node-models.md §2.1's application layer is what would hold these, and `v2xw-node`
/// ships none, so the engine carries the payload beside the frame and acts on it at the
/// receiver. Both are real messages with real lengths on the air; what is missing is a
/// runtime that would decide to send them.
#[derive(Debug, Clone)]
enum AppPayload {
    /// A misbehaviour report on its way to a roadside unit that forwards it.
    Report(Box<v2xw_threat::MisbehaviourReport>),
    /// A certificate revocation list broadcast by the roadside.
    Crl(Box<v2xw_sec::linkage::CrlLinkageEntry>),
}

impl FrameState {
    /// The PHY's id for this transmission, or zero before `begin_tx`.
    fn tx_id(&self) -> u64 {
        self.tx_handle.map_or(0, |h| h.id)
    }
}

/// A configured run, ready to be driven.
pub struct Engine {
    scenario: Scenario,
    world: World,
    registry: Registry,
    manifest: Manifest,
    scheduler: Scheduler<Event>,
    rng: RngRegistry,
    provenance: ProvenanceLog,
    params: ParamSet,
    wall: WallClock,
    snapshot: ActorSnapshot,
    mobility: Box<dyn Mobility>,
    gnss: Box<dyn GnssModel>,
    propagation: Box<dyn BoxedPropagation>,
    fading: Box<dyn BoxedFading>,
    /// The physical layer. One instance for the run: it holds the live arrival set, every
    /// node's transmit intervals for the half-duplex test, and the air-time ledger.
    phy: OfdmPhy,
    /// Medium access, at the medium and high tiers. `None` at the abstract tier, which
    /// models no medium access: there the frame reaches the air one AIFS after signing.
    mac: Option<EdcaOcbMac>,
    /// Congestion control, at the medium and high tiers.
    dcc: Option<SaeJ2945Dcc>,
    weather: WeatherState,
    actors: BTreeMap<ActorId, ActorRecord>,
    nodes: BTreeMap<NodeId, ObuRuntime>,
    /// Frames each node has received and not yet processed, with the instant each
    /// finished arriving and the token its `node.rx` record is joined by.
    inboxes: BTreeMap<NodeId, Vec<(RxFrame, RxStamp)>>,
    /// The network and transport stack every frame is framed with (`net.layer`).
    net: v2xw_net::NetStack,
    /// Reception attempts a node has been handed and has not yet resolved, by
    /// (receiver, token): the `node.rx` record so far, waiting for its fate.
    rx_pending: BTreeMap<(NodeId, u64), NodeRx>,
    /// The next reception-attempt token.
    next_rx_token: u64,
    /// Per-node MAC counters since the last `mac.cbr` report.
    mac_window: BTreeMap<NodeId, MacWindow>,
    /// `node.rx` fates settled where no recorder is in hand (a receiver retired), written
    /// at the next point that has one.
    orphaned_rx: Vec<NodeRx>,
    frames: BTreeMap<FrameSeq, FrameState>,
    /// Which frames currently have a registered arrival at each receiver, so a new frame
    /// can find the ones it overlaps without scanning every live frame.
    live_at_rx: BTreeMap<NodeId, Vec<FrameSeq>>,
    /// Frames whose signature has finished but which the MAC has not yet been handed,
    /// per transmitter, keyed by (ready instant, frame) so the walk is in time order.
    pending_tx: BTreeMap<NodeId, Vec<(SimTime, FrameSeq)>>,
    /// The Phase 2 path, when the scenario declared one.
    phase2: Option<crate::phase2::Phase2>,
    /// The roadside units' positions. They are nodes but not actors, so they are not in
    /// the mobility snapshot and the reception phase has to find them here.
    rsus: BTreeMap<NodeId, Vec3>,
    /// Reports in flight over a backhaul, by the SDU id their [`Event::NetDeliver`]
    /// carries.
    backhaul: BTreeMap<
        v2xw_core::ids::SduId,
        (NodeId, Box<v2xw_threat::MisbehaviourReport>, NodeId, ReportJourney),
    >,
    /// The next backhaul SDU id.
    next_sdu: u32,
    next_node: u32,
    next_frame: u32,
    /// The producer of the normative `Keyframe`/`Delta` stream (vwp-v1 §3.3, §3.4).
    snapshots: SnapshotStream,
    /// Whether that stream is produced at all.
    ///
    /// On by default, because a recording without it is a recording a browser cannot
    /// replay — the defect this field's default is set against. A scale measurement that
    /// records nothing turns it off, since encoding ten thousand rows per step into
    /// frames nobody stores is cost with no artefact.
    snapshots_enabled: bool,
    /// Which nodes put a frame on the air since the last mobility step, for §3.3.4's
    /// `ST_TRANSMITTING` bit. A `BTreeSet`, so nothing about the frame depends on hash
    /// iteration order (02-architecture.md §6.1).
    transmitted_since_step: std::collections::BTreeSet<NodeId>,
    providers: v2xw_metrics::ProviderSet,
    metric_period: Duration,
    reverse_node_walk: bool,
    /// Where each node's generator sits on the time axis (`v2xw_msg::GenerationTiming`).
    gen_timing: v2xw_msg::GenerationTiming,
    /// Each node's generation phase, drawn once when the node is created.
    node_phase: BTreeMap<NodeId, Duration>,
    /// Each vehicle node's radio class, which sets its antenna height and gain.
    node_class: BTreeMap<NodeId, v2xw_radio::ActorClass>,
    /// What obstructs a link: buildings and terrain, as the scenario selected them.
    obstacles: crate::wiring::ObstacleStack,
    /// The LTE-V2X or NR-V2X sidelink access layer, when `radio.rat` selects one. `None`
    /// runs 802.11p through `phy`, `mac` and `dcc`.
    sidelink: Option<sidelink::SidelinkAccess>,
    /// The focus region and its radio stack, when `radio.tiers.focus` declares one.
    focus: Option<crate::wiring::FocusStack>,
    /// The jammers `threats.jammers` declared.
    jamming: jamming::Jamming,
    report: RunReport,
}

impl core::fmt::Debug for Engine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Engine")
            .field("scenario", &self.scenario.meta.name)
            .field("actors", &self.actors.len())
            .field("nodes", &self.nodes.len())
            .field("pending", &self.scheduler.len())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Builds a run from a validated scenario.
    ///
    /// `build_utc` is the caller's timestamp for the manifest; the engine may not read a
    /// clock for itself (02-architecture.md §6.1). Pass an empty string for a
    /// reproducibility comparison — the field is excluded from every digest either way.
    ///
    /// # Errors
    /// [`EngineError::World`] if the world cannot be built, [`EngineError::Mobility`] if
    /// the mobility provider refuses the world, [`EngineError::Registry`] if a model's
    /// card does not validate, and [`EngineError::Scenario`] if the scenario is invalid.
    pub fn build(scenario: Scenario, build_utc: &str) -> Result<Engine> {
        scenario.validate()?;
        let world = crate::wiring::build_world(&scenario)?;
        Self::build_with_world(scenario, world, build_utc)
    }

    /// As [`Engine::build`], on a world the caller already has.
    ///
    /// For a driver that runs one scenario many times — the server, where every Run is a
    /// fresh kernel — and keeps the world it imported last time. The caller vouches that
    /// `world` is what [`crate::wiring::build_world`] returns for this scenario; the server
    /// keys its copy by [`crate::wiring::world_cache_key`], which covers every input of the
    /// import, so a run on a kept world is the same run as one on a fresh import.
    ///
    /// # Errors
    /// As [`Engine::build`], less the world import.
    pub fn build_with_world(scenario: Scenario, world: World, build_utc: &str) -> Result<Engine> {
        scenario.validate()?;
        let mut registry = Registry::new();
        crate::wiring::register_all(&mut registry)?;

        let wall = WallClock::parse_rfc3339(&scenario.time.t0)
            .map_err(|e| EngineError::Core(v2xw_core::error::CoreError::Time(e)))?;
        let rng = RngRegistry::new(scenario.seed);
        let mut mobility = crate::wiring::build_mobility(&scenario);
        // The drivers see the weather the run starts in (FHWA speed and headway, sight
        // distance, surface grip — v2xw_mobility::weather).
        mobility.set_weather(crate::wiring::initial_weather(&scenario));
        let gnss = crate::wiring::build_gnss(&scenario);
        let (propagation, fading) = crate::wiring::build_radio(&scenario, &world);
        crate::wiring::register_radio(
            &mut registry,
            propagation.as_ref(),
            fading.as_ref(),
            &crate::wiring::build_phy(&scenario),
        )?;
        let obstacles = crate::wiring::build_obstacles(&scenario, &world);
        obstacles.register(&mut registry)?;
        let focus = crate::wiring::build_focus(&scenario, &world);
        if let Some(f) = focus.as_ref() {
            crate::wiring::register_radio(
                &mut registry,
                f.propagation.as_ref(),
                f.fading.as_ref(),
                &crate::wiring::build_phy(&scenario),
            )?;
        }
        let sidelink = sidelink::SidelinkAccess::for_scenario(&scenario);
        if let Some(sl) = sidelink.as_ref() {
            sl.register(&mut registry)?;
        }
        let jammers = jamming::Jamming::for_scenario(
            &scenario,
            sidelink.as_ref().map_or(SAFETY_CHANNEL, |sl| sl.channel),
        );
        for card in jammers.cards() {
            if !registry.contains(&card.id) {
                registry.register(card)?;
            }
        }

        // The mobility provider reads the world through a context, so it needs one before
        // the engine exists. Everything it can reach at this point is immutable state the
        // engine is about to own.
        {
            let mut scheduler: Scheduler<Event> = Scheduler::new();
            let mut provenance = ProvenanceLog::new();
            let params = ParamSet::new();
            let empty = ActorSnapshot::new(0, MAX_RANGE_M);
            let mut recorder = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                &mut scheduler,
                &rng,
                &world,
                &empty,
                &mut provenance,
                &params,
                &mut recorder,
            );
            let demand = crate::wiring::build_demand(&scenario, &world)?;
            mobility.init(&mut crate::adapters::mobility(&mut ctx), demand)?;
        }

        let gen_timing = crate::wiring::generation_timing(&scenario);
        crate::wiring::register_generation_timing(&mut registry, gen_timing)?;
        let providers = crate::wiring::build_metrics(&scenario, &mut registry)?;
        let manifest = crate::manifest::assemble(&scenario, &world, &registry, build_utc)?;
        // The snapshot stream's cadence is the scenario's mobility step and, by default,
        // a keyframe every simulated second (§3.1.1). A caller recording into a container
        // with a different `RecordingOptions::cadence` must say so with
        // [`Engine::configure_snapshots`], because `Reader::verify` checks the gap
        // between keyframes against the cadence the *container* declares.
        let snapshots = SnapshotStream::new(
            &world.bbox,
            crate::snapshot::snapshot_cadence(
                scenario.time.mobility_step(),
                crate::snapshot::DEFAULT_KEYFRAME_PERIOD,
            ),
            Profile::Full,
        );
        // The radio stack is selected from the scenario, which the struct literal below
        // moves; the clone is one `Scenario` per run, not per anything.
        let scenario_for_radio = scenario.clone();
        let mut engine = Engine {
            snapshot: ActorSnapshot::new(0, MAX_RANGE_M),
            weather: crate::wiring::initial_weather(&scenario),
            scenario,
            world,
            registry,
            manifest,
            scheduler: Scheduler::new(),
            rng,
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            wall,
            mobility,
            gnss,
            propagation,
            fading,
            phy: crate::wiring::build_phy(&scenario_for_radio),
            // 802.11p's EDCA and J2945/1 congestion control belong to the DSRC stack; a
            // sidelink scenario runs the SPS engine in `sidelink` instead.
            mac: if sidelink.is_some() { None } else { crate::wiring::build_mac(&scenario_for_radio) },
            dcc: if sidelink.is_some() { None } else { crate::wiring::build_dcc(&scenario_for_radio) },
            actors: BTreeMap::new(),
            nodes: BTreeMap::new(),
            inboxes: BTreeMap::new(),
            // `validate` refuses any other name, so the fallback is unreachable from a
            // loaded scenario; it is WSMP because that is the schema's default.
            net: v2xw_net::NetStack::from_name(&scenario_for_radio.net.layer)
                .unwrap_or(v2xw_net::NetStack::Wsmp(v2xw_net::WsmpNetLayer::default())),
            rx_pending: BTreeMap::new(),
            next_rx_token: 0,
            mac_window: BTreeMap::new(),
            orphaned_rx: Vec::new(),
            frames: BTreeMap::new(),
            live_at_rx: BTreeMap::new(),
            pending_tx: BTreeMap::new(),
            phase2: None,
            rsus: BTreeMap::new(),
            backhaul: BTreeMap::new(),
            next_sdu: 0,
            next_node: 0,
            next_frame: 0,
            snapshots,
            snapshots_enabled: true,
            transmitted_since_step: std::collections::BTreeSet::new(),
            providers,
            metric_period: Duration::from_secs(1),
            reverse_node_walk: false,
            gen_timing,
            node_phase: BTreeMap::new(),
            node_class: BTreeMap::new(),
            obstacles,
            sidelink,
            focus,
            jamming: jammers,
            report: RunReport::default(),
        };
        let phase2 = crate::phase2::Phase2::build(&engine.scenario, &engine.world)?;
        engine.phase2 = phase2;
        engine.create_rsus();
        engine.seed_timeline();
        Ok(engine)
    }

    /// The manifest this run will be recorded under.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Sets the cadence and profile the binary snapshot stream is produced at.
    ///
    /// **A caller recording into a container must call this with the same
    /// [`v2xw_record::Cadence`] it gave [`v2xw_record::RecordingOptions`].**
    /// `Reader::verify` checks the gap between two keyframes against the cadence the
    /// *container* declares, so a producer running at a slower cadence than the container
    /// advertises writes a recording that fails its own verification — and one running
    /// faster writes more keyframes than §7.3's seek bound was sized for. The default is
    /// the scenario's mobility step with a keyframe every simulated second, which is
    /// §3.1.1's default pair.
    ///
    /// It resets the stream: the next frame is a keyframe opening GOP 0, with sequence
    /// numbers from zero. Call it before [`Engine::run`].
    pub fn configure_snapshots(&mut self, cadence: Cadence, profile: Profile) {
        self.snapshots = SnapshotStream::new(&self.world.bbox, cadence, profile);
        self.report.keyframes = 0;
        self.report.deltas = 0;
    }

    /// The cadence the binary snapshot stream is being produced at.
    pub fn snapshot_cadence(&self) -> Cadence {
        self.snapshots.cadence()
    }

    /// Turns the binary snapshot stream on or off.
    ///
    /// On by default. Off is for a run that records nothing and is measuring the
    /// simulation's own cost; a run that writes a file and turns this off writes a file
    /// no browser can replay, which is the state this engine has just been brought out of.
    pub fn set_snapshots_enabled(&mut self, enabled: bool) {
        self.snapshots_enabled = enabled;
    }

    /// The model registry, for a caller assembling a report.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The world.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The scenario being run.
    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    /// The wall clock `SimTime` zero maps to — scenario data, not a clock read.
    ///
    /// It is what a 1609.2 `generationTime` is stamped from
    /// ([`v2xw_sec::envelope::Envelope`] takes one), and what a UI labels its axis with.
    pub fn wall_clock(&self) -> WallClock {
        self.wall
    }

    /// The run report as it stands, for a caller inspecting a partial run.
    pub fn report(&self) -> &RunReport {
        &self.report
    }

    /// The Phase 2 path's state, when the scenario declared one.
    ///
    /// Read-only. It exists so that a caller can ask a *built* engine which roadside unit
    /// carries which role — [`crate::phase2::Phase2::rsus_with_role`] — without running a
    /// scenario to its horizon to find out. The run loop reaches the field directly and
    /// does not go through this.
    #[must_use]
    pub fn phase2(&self) -> Option<&crate::phase2::Phase2> {
        self.phase2.as_ref()
    }

    /// **A test hook, not a model parameter.** Walks the node phase in reverse id order.
    ///
    /// The phase reads each node's own inbox and writes only its own state, so the
    /// published result must be identical either way; this is the ADR 0004 purity property
    /// and the only way to check it without a second thread, which
    /// [`v2xw_node::ObuRuntime`] not being `Send` denies us. `v2xw-mobility` carries the
    /// same hook (`EngineParams::reverse_order`) for the same reason.
    pub fn set_reverse_node_walk(&mut self, reverse: bool) {
        self.reverse_node_walk = reverse;
    }

    /// Runs `f` with an engine context over this engine's state.
    ///
    /// The seam a caller outside the loop reaches the kernel through: a tool that wants to
    /// emit a record, schedule an event or draw from a stream in the engine's own frame
    /// uses this rather than rebuilding a context and getting the borrows wrong. It is
    /// also how this crate's tests exercise [`EngineCtx`] against a real engine instead of
    /// a fixture, which is the difference between testing the context and testing a copy
    /// of it.
    pub fn with_ctx<R>(
        &mut self,
        recorder: &mut dyn RunRecorder,
        f: impl FnOnce(&mut EngineCtx<'_>) -> R,
    ) -> R {
        let Engine {
            scheduler,
            rng,
            world,
            snapshot,
            provenance,
            params,
            ..
        } = self;
        let mut ctx = EngineCtx::new(
            scheduler, rng, world, snapshot, provenance, params, recorder,
        );
        f(&mut ctx)
    }

    /// The `why` service's accumulated entries.
    pub fn provenance(&self) -> &ProvenanceLog {
        &self.provenance
    }

    /// The position of an actor at an instant inside the current mobility step.
    ///
    /// The published extrapolation rule (02-architecture.md §5.2, invariant I-M4) and the
    /// only place it is applied.
    pub fn position_at(&self, actor: ActorId, t: SimTime) -> Option<Vec3> {
        self.actors.get(&actor).map(|a| a.last.extrapolate(t).pos)
    }

    /// Moves a `follow` focus region onto its node's current position.
    fn recentre_focus(&mut self, now: SimTime) {
        let Some(node) = self.focus.as_ref().and_then(|f| f.plan.follows) else {
            return;
        };
        let centre = self.node_pos(node, now).unwrap_or(crate::wiring::FOCUS_NOWHERE);
        if let Some(f) = self.focus.as_mut() {
            f.plan.recentre(centre);
        }
    }

    /// Draws a new node's generation phase (`v2xw_msg::GenerationTiming::phase`).
    ///
    /// Keyed by the node id alone, so the phase does not depend on when the node was
    /// created or how many were created before it.
    fn note_node_created(&mut self, id: NodeId) {
        let phase = self.gen_timing.phase(&self.rng, id);
        self.node_phase.insert(id, phase);
    }

    /// One node's generation phase inside the mobility step, when it has one.
    pub fn node_phase(&self, node: NodeId) -> Option<Duration> {
        self.node_phase.get(&node).copied()
    }

    /// True if `t` falls inside a declared time-dilation window.
    pub fn is_dilated(&self, t: SimTime) -> bool {
        self.manifest.is_dilated(t)
    }

    /// Creates one node per roadside unit the scenario declared.
    ///
    /// A roadside unit is a node and **not** an actor: it does not move, it is not in the
    /// mobility snapshot, and nothing spawns or retires it. So it is created here, at
    /// build, and the reception phase finds it through [`Engine::rsus`] rather than through
    /// a grid query. Its runtime is an [`ObuRuntime`] on an RSU hardware profile with no
    /// message services; what it does here is receive, and put the CRL on the air when the
    /// backend hands it one. 06-node-models.md §3's roadside runtime — roles, failure
    /// states, store-and-forward — now ships as [`v2xw_node::RsuRuntime`] and this site
    /// has not been moved onto it; see `crate::phase2`'s "What is not here".
    ///
    /// The node ids are minted here, **before** the timeline is seeded and therefore
    /// before any vehicle's, from the same counter vehicle spawn uses. That ordering is
    /// published: `v2xw-server`'s live projector reconstructs the actor→node map from it
    /// and offsets its own counter by the roadside count.
    fn create_rsus(&mut self) {
        let Some(phase2) = self.phase2.as_mut() else {
            return;
        };
        let specs: Vec<crate::phase2::RsuSpec> = phase2.rsu_specs().to_vec();
        for spec in specs {
            let id = NodeId::new(self.next_node);
            self.next_node += 1;
            let env = crate::wiring::NodeEnv::new(&self.world, self.wall);
            let mut runtime = crate::wiring::build_rsu(&self.scenario, env, &spec, id, 0);
            // A surveyed position, not a fix: an RSU knows where its own mast is because
            // somebody measured it, which is why this is not a GNSS estimate and why the
            // node is not being handed ground truth it could not have (invariant I-C2).
            let mut belief = v2xw_core::PositionEstimate::no_fix(0);
            belief.pos = spec.position;
            belief.semi_major_m = 0.0;
            belief.semi_minor_m = 0.0;
            // `FixQuality` has no "surveyed" rank, because it enumerates what a *GNSS
            // receiver* reports. RTK is the closest honest label: centimetre class and
            // the best rank the enum carries, which is what a surveyed mast deserves and
            // what makes the unit's position usable by a detector's own plausibility test.
            belief.fix = v2xw_core::belief::FixQuality::Rtk;
            runtime.set_belief(belief.quantized());
            self.nodes.insert(id, runtime);
            self.inboxes.insert(id, Vec::new());
            self.note_node_created(id);
            self.rsus.insert(id, spec.position);
            self.report.nodes_created += 1;
            if let Some(phase2) = self.phase2.as_mut() {
                phase2.note_rsu(id);
            }
        }
    }

    /// What one signature costs this node, on its own hardware.
    ///
    /// The engine needs this for the two application messages it puts on the air itself —
    /// the misbehaviour report and the CRL broadcast — because a frame that reached the
    /// air the instant the application decided to send it would be a frame that was never
    /// signed, and because a `MacTimer` scheduled at the instant a priority-6 node phase
    /// is being dispatched is a zero-delay event at an earlier priority, which the kernel
    /// refuses (02-architecture.md §5.1).
    ///
    /// It is the profile's own cost for the signing primitive, read from the same table
    /// `ObuRuntime::generate` reads. **It is not queued**: the node's own signer submits
    /// to a `ServerBank` and waits behind whatever else that bank is doing, and this does
    /// not, so a report costs its service time and not its sojourn time. For one report
    /// per detection and one CRL per revocation that is the difference between a right
    /// answer and a slightly better one; for a per-frame cost it would not be.
    fn signing_cost(&self, node: NodeId) -> Duration {
        self.nodes
            .get(&node)
            .and_then(|n| {
                n.profile()
                    .op_cost(NodeConfig::default().sign_op)
                    .map(|(d, _)| d)
            })
            // A profile that publishes no signing rate: one microsecond, which is not a
            // claim about the hardware but the smallest interval that keeps the frame's
            // `MacTimer` strictly after the phase that produced it.
            .unwrap_or(Duration::from_micros(1))
    }

    /// One node's ground-truth position at an instant, whether it rides an actor or stands
    /// on a mast.
    fn node_pos(&self, node: NodeId, at: SimTime) -> Option<Vec3> {
        if let Some(pos) = self.rsus.get(&node) {
            return Some(*pos);
        }
        self.actors
            .values()
            .find(|a| a.node == Some(node))
            .map(|a| a.last.extrapolate(at).pos)
    }

    /// Puts the scheduled events that exist before the first dispatch on the heap.
    fn seed_timeline(&mut self) {
        let horizon = self.scenario.time.horizon_ns();
        self.scheduler
            .schedule(0, EventClass::MobilityStep, Event::MobilityStep);
        for (i, item) in self.scenario.events.iter().enumerate() {
            let at = (item.t * 1e9).round().max(0.0) as u64;
            if at <= horizon {
                self.scheduler.schedule(
                    at,
                    EventClass::Control,
                    Event::Control {
                        item: i as u32,
                        end: false,
                    },
                );
            }
            if let Some(until) = item.until {
                let end = (until * 1e9).round().max(0.0) as u64;
                if end <= horizon {
                    self.scheduler.schedule(
                        end,
                        EventClass::Control,
                        Event::Control {
                            item: i as u32,
                            end: true,
                        },
                    );
                }
            }
        }
        if !self.providers.is_empty() {
            let first = self.metric_period.after(0);
            if first <= horizon {
                self.scheduler.schedule(
                    first,
                    EventClass::Observe,
                    Event::Observe {
                        what: Observe::MetricFlush,
                    },
                );
            }
        }
        // The end-of-run sentinel is an `Observe`, so it is the last thing that happens at
        // the horizon: every metric flush and every keyframe at that instant runs first.
        self.scheduler.schedule(
            horizon,
            EventClass::Observe,
            Event::Observe {
                what: Observe::EndOfRun,
            },
        );
    }

    /// Runs to the horizon, writing into `recorder`.
    ///
    /// # Errors
    /// Whatever a phase returns: a mobility failure, a recorder failure.
    pub fn run(&mut self, recorder: &mut dyn RunRecorder) -> Result<RunReport> {
        let horizon = self.scenario.time.horizon_ns();
        let step = self.scenario.time.mobility_step();

        while let Some((key, event)) = self.scheduler.pop() {
            if key.time > horizon {
                break;
            }
            // The one cancellation point (see `RunRecorder::cancelled`): between two
            // events, never inside a phase. Asked before the event is counted, so a
            // cancelled run's report does not claim an event it did not run.
            if recorder.cancelled() {
                break;
            }
            self.report.count(event.class());
            match event {
                Event::Control { item, end } => self.on_control(item as usize, end),
                Event::MobilityStep => {
                    self.on_mobility_step(recorder, step, horizon)?;
                }
                Event::NodePhase => self.on_node_phase(recorder, horizon, None),
                Event::NodeTask {
                    node,
                    task: crate::event::NodeTask::Step,
                } => self.on_node_phase(recorder, horizon, Some(node)),
                Event::MacTimer { node, channel } => {
                    self.on_mac_timer(node, ChannelId(channel), horizon);
                }
                Event::PhyStart { frame, .. } => self.on_phy_start(frame, horizon),
                Event::PhyEnd { frame } => self.on_phy_end(recorder, frame),
                Event::Observe {
                    what: Observe::MetricFlush,
                } => self.on_metric_flush(recorder, horizon),
                Event::Observe {
                    what: Observe::EndOfRun,
                } => {
                    self.report.end_ns = key.time;
                    self.resolve_in_flight(recorder, key.time);
                    break;
                }
                // The remaining classes have no model scheduling them in this build; see
                // the module documentation's list of what is a missing model rather than a
                // missing seam. Counting them is what makes their absence visible in the
                // run report instead of silent.
                Event::NetDeliver { sdu, to } => self.on_net_deliver(recorder, sdu, to, horizon),
                Event::FlowTimer { .. } => self.on_flow_timer(horizon),
                Event::SignalPhase { .. }
                | Event::NodeTask { .. }
                | Event::Observe { .. } => {}
            }
            self.report.end_ns = key.time;
        }
        if let Some(phase2) = self.phase2.as_ref() {
            self.report.phase2 = phase2.report().clone();
        }
        self.report.sidelink = self.sidelink_report();
        // What the *recorder* says it kept, asked once at the end and never inferred from
        // what the engine handed over. The two numbers sitting side by side is the point:
        // a run report that quoted only the emitted count could contradict the artefact
        // beside it and nothing would say which was right.
        self.report.records_written = recorder.records_written();
        self.report.frames_written = recorder.frames_written();
        self.report.keyframes = self.snapshots.keyframes();
        self.report.deltas = self.snapshots.deltas();
        Ok(self.report.clone())
    }

    /// A scenario timeline item takes effect, or stops taking effect.
    fn on_control(&mut self, index: usize, end: bool) {
        let Some(item) = self.scenario.events.get(index).cloned() else {
            return;
        };
        use crate::scenario::TimelineKind;
        match item.kind {
            TimelineKind::WeatherFront => {
                if end {
                    // The front has passed: back to the scenario's own weather, rather than
                    // leaving the storm in force for the rest of the run.
                    self.weather = crate::wiring::initial_weather(&self.scenario);
                } else if let Some(v) = item.params.get("value")
                    && let Ok(kind) = serde_json::from_value(v.clone())
                {
                    let intensity = item
                        .params
                        .get("intensity")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0);
                    let visibility = item
                        .params
                        .get("visibility_m")
                        .and_then(serde_json::Value::as_f64);
                    let surface = item
                        .params
                        .get("surface")
                        .and_then(|s| serde_json::from_value(s.clone()).ok());
                    self.weather = crate::wiring::weather_of(kind, intensity, visibility, surface);
                }
                // And the drivers feel it, from the next mobility step.
                self.mobility.set_weather(self.weather);
            }
            TimelineKind::Outage => {
                // `target` names a node by index. An outage turns the node off, which is
                // the state `ObuRuntime::step` returns from immediately, so it stops both
                // transmitting and receiving without being removed.
                if let Some(node) = item
                    .params
                    .get("target")
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| NodeId::new(n as u32))
                    && let Some(runtime) = self.nodes.get_mut(&node)
                {
                    runtime.set_state(if end {
                        v2xw_node::NodeState::Active
                    } else {
                        v2xw_node::NodeState::Off
                    });
                }
            }
            // `demand.multiplier`, `attack.wave`, `param.change` and `closure` need a
            // demand model that takes a multiplier, an attacker set, a live parameter
            // store and a mobility command respectively. Each is a model this build does
            // not have; the events fire, are counted, and change nothing, which is what
            // the run report shows.
            TimelineKind::DemandMultiplier
            | TimelineKind::AttackWave
            | TimelineKind::ParamChange
            | TimelineKind::Closure => {}
        }
    }

    /// The mobility phase: map over actors, merge by [`ActorId`], publish, reindex.
    fn on_mobility_step(
        &mut self,
        recorder: &mut dyn RunRecorder,
        step: Duration,
        horizon: SimTime,
    ) -> Result<()> {
        let now = self.scheduler.now();

        // The published state of *this* instant goes out before the world advances past
        // it. A mobility step dispatched at `now` produces states for `now + step`, so
        // publishing the step's own result here would file every record one step early
        // against its own `t` field — which is the defect this ordering closes. What is
        // published instead is what the previous step left in `self.actors`, every entry
        // of which carries `k.t == now`.
        self.publish_state_at(recorder);
        self.emit_snapshot(recorder, now)?;
        // The transmit bit is "since the last mobility step", so the window closes with
        // the frame that reports it.
        self.transmitted_since_step.clear();

        let update = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                mobility,
                ..
            } = self;
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, recorder,
            );
            let mut mob = crate::adapters::mobility(&mut ctx);
            mobility.step(&mut mob, step).quantized()
        };

        self.absorb(&update, now);
        self.recentre_focus(now);
        for orphan in core::mem::take(&mut self.orphaned_rx) {
            self.emit(recorder, &orphan);
        }
        self.rebuild_snapshot(&update);
        self.declare_jamming(now, step);
        self.update_beliefs(recorder, now);

        self.report.mobility_steps += 1;

        // The node phase. With a synchronised generation timing every node steps at this
        // instant, at priority 6 — after any PhyEnd at this instant (priority 3), which is
        // the order 02-architecture.md §5.1 fixes. Otherwise each node steps at its own
        // phase inside the step (`v2xw_msg::GenerationTiming`): no standard puts two
        // stations' generators on a common grid, and stepping them all at once made every
        // vehicle contend for the channel in the same few hundred microseconds.
        if self.gen_timing.is_synchronised() {
            self.scheduler
                .schedule(now, EventClass::NodeTask, Event::NodePhase);
        } else {
            let step_ns = step.as_nanos().max(1);
            let due: Vec<(NodeId, SimTime)> = self
                .nodes
                .keys()
                .map(|n| {
                    let phase = self.node_phase.get(n).map_or(0, |d| d.as_nanos() % step_ns);
                    (*n, now + phase)
                })
                .collect();
            for (node, at) in due {
                if at <= horizon {
                    self.scheduler.schedule(
                        at,
                        EventClass::NodeTask,
                        Event::NodeTask {
                            node,
                            task: crate::event::NodeTask::Step,
                        },
                    );
                }
            }
        }

        let next = step.after(now);
        if next <= horizon {
            self.scheduler
                .schedule(next, EventClass::MobilityStep, Event::MobilityStep);
        }
        Ok(())
    }

    /// Takes the spawns and despawns out of an update, creating and retiring nodes.
    fn absorb(&mut self, update: &MobilityUpdate, now: SimTime) {
        for spawn in &update.spawned {
            self.report.actors_spawned += 1;
            // The equipped draw is keyed by the actor, so whether a vehicle carries an OBU
            // does not depend on how many vehicles spawned before it (ADR 0004 §3).
            let fraction = if crate::wiring::is_vru_class(spawn.class) {
                self.scenario.actors.vru.device_fraction
            } else {
                self.scenario.actors.vehicles.equipped_fraction
            };
            let equipped = self
                .rng
                .checkout(RngDomain::Spawn, EntityRef::Actor(spawn.actor))
                .bool(fraction);
            let node = if equipped {
                let id = NodeId::new(self.next_node);
                self.next_node += 1;
                let env = crate::wiring::NodeEnv::new(&self.world, self.wall);
                let mut runtime = crate::wiring::build_node(
                    &self.scenario,
                    env,
                    id,
                    now,
                    spawn.class,
                    spawn.kinematics.dims,
                );
                // Phase 2: the backend enrols and provisions the device, and the
                // credentials it installs carry the linkage values a CRL revokes. The
                // digest stays the `pseudo_signer` stand-in — see `crate::phase2`, joint 1
                // — so the *pool* is the protocol's and the *identity* is not.
                //
                // The digest the certificate is *announced under* is not knowable here:
                // `ObuRuntime` issues a real certificate for each pseudonym on its first
                // transmission and writes that certificate's own `HashedId8` back into the
                // store, replacing the stand-in this installs. So the digest → pseudonym
                // map is filled in `hand_down_app`, at the instant a frame carries one.
                if let Some(phase2) = self.phase2.as_mut() {
                    let creds = phase2.provision(id);
                    if !creds.is_empty() {
                        crate::wiring::install_provisioned(&mut runtime, &self.scenario, id, &creds);
                    }
                }
                self.nodes.insert(id, runtime);
                self.inboxes.insert(id, Vec::new());
                self.note_node_created(id);
                self.node_class.insert(id, radio_class(spawn.class));
                self.report.nodes_created += 1;
                if self.phase2.is_some() {
                    let Engine {
                        scheduler,
                        rng,
                        world,
                        snapshot,
                        provenance,
                        params,
                        phase2,
                        ..
                    } = self;
                    let mut null = crate::ctx::NullRecorder::new();
                    let mut ctx = EngineCtx::new(
                        scheduler, rng, world, snapshot, provenance, params, &mut null,
                    );
                    if let Some(p) = phase2.as_mut() {
                        p.arm_attacker(&mut ctx, id, spawn.actor);
                    }
                }
                Some(id)
            } else {
                None
            };
            self.actors.insert(
                spawn.actor,
                ActorRecord {
                    class: spawn.class,
                    driver: spawn.driver,
                    node,
                    last: spawn.kinematics,
                },
            );
        }
        for (actor, _) in &update.despawned {
            // The wire slot goes into its cooling-off period here, one step before the
            // frame whose row set no longer holds it. §3.3.1 forbids reusing a slot for
            // one keyframe period after a despawn, so a delta that arrives late cannot be
            // applied to whichever actor inherited the row.
            self.snapshots.retire(*actor, now);
            if let Some(rec) = self.actors.remove(actor)
                && let Some(node) = rec.node
            {
                self.nodes.remove(&node);
                self.inboxes.remove(&node);
                self.node_phase.remove(&node);
                self.node_class.remove(&node);
                self.mac_window.remove(&node);
                // Frames the retired node had been handed and not yet processed never
                // reach an application: each attempt is settled here rather than left
                // without a fate.
                let pending: Vec<(NodeId, u64)> = self
                    .rx_pending
                    .range((node, 0)..=(node, u64::MAX))
                    .map(|(k, _)| *k)
                    .collect();
                for key in pending {
                    if let Some(attempt) = self.rx_pending.remove(&key) {
                        self.orphaned_rx
                            .push(attempt.lost(now, rx_cause::RECEIVER_OFF));
                    }
                }
            }
        }
        for (actor, k) in &update.states {
            if let Some(rec) = self.actors.get_mut(actor) {
                rec.last = *k;
            }
        }
    }

    /// Rebuilds the spatial index from the published states (ADR 0004 decision 6).
    fn rebuild_snapshot(&mut self, update: &MobilityUpdate) {
        let mut entries: Vec<(VehicleView, Kinematics)> = Vec::with_capacity(update.states.len());
        for (actor, k) in &update.states {
            let Some(rec) = self.actors.get(actor) else {
                continue;
            };
            let lane = k.lane.unwrap_or(v2xw_core::geom::LanePos::new(
                v2xw_core::ids::LaneId::new(0),
                0.0,
                0.0,
            ));
            entries.push((
                VehicleView {
                    actor: *actor,
                    class: rec.class,
                    lane: lane.lane,
                    lane_index: self
                        .world
                        .roads
                        .lanes()
                        .get(lane.lane.0 as usize)
                        .map_or(0, |l| l.index),
                    // The view's `s_m` is the front bumper; `Kinematics` references the
                    // rear axle (03-interfaces.md §1), and the conversion happens once.
                    s_m: lane.s_m + k.dims.length_m,
                    lateral_m: lane.d_m,
                    speed_mps: k.ground_speed_mps(),
                    accel_mps2: k.acc.x,
                    heading_rad: k.heading_rad,
                    dims: k.dims,
                    driver: rec.driver,
                },
                *k,
            ));
        }
        self.snapshot = ActorSnapshot::build(update.t, MAX_RANGE_M, entries);
    }

    /// Emits `gt.kinematics` for every live actor's current state, in actor order.
    ///
    /// The record is stamped at **the instant the state describes** — `k.t`, not the
    /// scheduler's instant — which is the correction the vertical-slice audit asked for.
    /// The two coincide for every actor the previous mobility step published, and `k.t`
    /// is used rather than the scheduler's instant so that an actor whose state is older
    /// than the step — a spawn the provider did not include in its own state list — is
    /// filed at the time it is actually about, rather than at a time the engine asserted
    /// for it.
    fn publish_state_at(&mut self, recorder: &mut dyn RunRecorder) {
        let states: Vec<(ActorId, Kinematics, &'static str, Option<NodeId>)> = self
            .actors
            .iter()
            .map(|(actor, rec)| (*actor, rec.last, rec.class.as_str(), rec.node))
            .collect();
        for (actor, k, class, node) in states {
            let rec = GtKinematics::new(actor, &k, class).with_node(node);
            self.emit_at(recorder, k.t, &rec);
        }
    }

    /// Encodes and stores this instant's `Keyframe` or `Delta` (vwp-v1 §3.3, §3.4).
    ///
    /// One frame per mobility step; which of the two it is, is the encoder's decision
    /// from the cadence. This is the *only* producer of the binary snapshot stream in the
    /// engine, and the frame it hands the recorder is stored verbatim, which is what
    /// makes §7.2's byte-identity guarantee between a live stream and a replay hold by
    /// construction rather than by care.
    ///
    /// # Errors
    /// [`EngineError::Record`] if the encoder refuses the step. Each of its refusals is
    /// an engine bug — time that did not advance, a slot used twice — and a run that
    /// carried on after one would be a run whose recording silently stopped being
    /// replayable.
    fn emit_snapshot(&mut self, recorder: &mut dyn RunRecorder, at: SimTime) -> Result<()> {
        if !self.snapshots_enabled {
            return Ok(());
        }
        let mut states: Vec<ActorState> = Vec::with_capacity(self.actors.len());
        for (actor, rec) in &self.actors {
            let verified_neighbors = rec
                .node
                .and_then(|n| self.nodes.get(&n))
                .map_or(0, |runtime| runtime.stores().neighbors.counts().1 as u32);
            let attacker = rec.node.is_some_and(|n| {
                self.phase2.as_ref().is_some_and(|p| p.is_attacker(n))
            });
            let transmitting = rec
                .node
                .is_some_and(|n| self.transmitted_since_step.contains(&n));
            states.push(ActorState {
                actor: *actor,
                node: rec.node,
                kinematics: rec.last,
                class: rec.class,
                attacker,
                transmitting,
                verified_neighbors,
            });
        }
        let frame = self.snapshots.encode(at, &states)?;
        recorder.write_wire_frame(&frame);
        self.report.keyframes = self.snapshots.keyframes();
        self.report.deltas = self.snapshots.deltas();
        Ok(())
    }

    /// Advances every node's belief from its ground truth through the GNSS model.
    ///
    /// Sequential and in node order, because the GNSS model is stateful per node and the
    /// state is advanced here; the draws are keyed by node, so the *values* would be the
    /// same in any order, and the ordering is about the model's `&mut self` rather than
    /// about determinism.
    fn update_beliefs(&mut self, recorder: &mut dyn RunRecorder, now: SimTime) {
        let pairs: Vec<(NodeId, Kinematics)> = self
            .actors
            .values()
            .filter_map(|a| a.node.map(|n| (n, a.last)))
            .collect();
        let env = GnssEnv {
            weather: self.weather,
            ..GnssEnv::OPEN_SKY
        };
        for (node, truth) in pairs {
            let belief = {
                let Engine {
                    scheduler,
                    rng,
                    world,
                    snapshot,
                    provenance,
                    params,
                    gnss,
                    ..
                } = self;
                let mut ctx = EngineCtx::new(
                    scheduler, rng, world, snapshot, provenance, params, recorder,
                );
                let mut mob = crate::adapters::mobility(&mut ctx);
                gnss.estimate(&mut mob, node, &truth, &env)
            };
            // `pos_error_m` is one of the two ground-truth values vwp-v1 §3.5.2 marks GT
            // and that a node cannot compute for itself. It enters through the one door
            // the firewall leaves open, from the engine, which knows both numbers.
            let error =
                v2xw_core::math::hypot(belief.pos.x - truth.pos.x, belief.pos.y - truth.pos.y);
            if let Some(runtime) = self.nodes.get_mut(&node) {
                runtime.set_belief(belief.quantized());
                runtime.observe_truth(error as f32);
            }
            self.update_dcc(node, now);
        }
    }

    /// Closes the congestion-control loop for one node.
    ///
    /// Two measurements go in and one state comes out. The channel busy ratio is the
    /// MAC's, measured over its own window ending at `now`; the neighbour count is the
    /// node's **own** — taken from its neighbour table against its own position estimate,
    /// never from the actor snapshot, so a node's transmit rate is a function of what it
    /// has heard and not of a density only the engine knows (invariant I-C2).
    ///
    /// The state that comes out is handed to the node's message generator, which is where
    /// J2945/1 rate control belongs: `MessageSchedule::due` already refuses to generate
    /// inside `t_off`. The engine does **not** also call [`Dcc::gate`], because that would
    /// apply the same inter-transmission time twice; what it does read from the model is
    /// the transmit power, in [`Engine::dcc_power_dbm`].
    fn update_dcc(&mut self, node: NodeId, now: SimTime) {
        if self.dcc.is_none() {
            return;
        }
        let cbr = self
            .mac
            .as_ref()
            .map(|m| Mac::<EngineCtx<'_>>::cbr(m, node, SAFETY_CHANNEL, now));
        let neighbours = self.nodes.get(&node).map_or(0, |runtime| {
            let own = v2xw_core::NodeView::position(runtime).pos;
            runtime
                .stores()
                .neighbors
                .iter()
                .filter(|n| n.claimed_pos.distance(own) <= J2945_DENSITY_RADIUS_M)
                .count() as u32
        });
        let state = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                dcc,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            let dcc = dcc.as_mut().expect("checked above");
            if let Some(cbr) = cbr {
                Dcc::on_cbr(dcc, &mut ctx, node, cbr);
            }
            dcc.on_density(&mut ctx, node, neighbours);
            <SaeJ2945Dcc as Dcc<EngineCtx<'_>>>::state(dcc, node)
        };
        let (t_off, cbr) = state.generator_view();
        if let Some(runtime) = self.nodes.get_mut(&node) {
            // `state_code` is the reactive algorithm's numbered state, and J2945/1 has no
            // such ladder: it controls rate and power continuously. Zero is "no numbered
            // state", which is what the telemetry field means when the algorithm has none.
            runtime.set_dcc(DccState { t_off, cbr }, 0);
        }
    }

    /// The node phase: a map over nodes, merged by [`NodeId`].
    ///
    /// Each node is handed its own inbox and its own context, steps, and returns what it
    /// produced. Nothing in the map touches the heap, the recorder or another node, so it
    /// is a **pure map in the ADR 0004 sense** and its result does not depend on the order
    /// the nodes are walked in — which `the_node_phase_result_does_not_depend_on_walk_order`
    /// checks by walking them backwards.
    ///
    /// It is nevertheless **executed sequentially**, and the reason is a type and not a
    /// choice: [`v2xw_node::ObuRuntime`] is not `Send`, because it holds a
    /// `Box<dyn VerificationPolicy>` and [`v2xw_core::model::Model`] — the supertrait every
    /// family extends — has no `Send + Sync` bound. `rayon` therefore cannot take `&mut`
    /// to two runtimes at once, whatever the map's purity. Adding `Send + Sync` to `Model`
    /// (or to the policy box) makes this one call `par_iter_mut`, and nothing else in this
    /// function changes; that is reported rather than worked around, because working
    /// around it would mean `unsafe`, which this crate forbids. The reception phase in
    /// [`Engine::on_phy_end`] *is* executed in parallel, so the structure is exercised by
    /// the run rather than only described by it.
    ///
    /// `only` restricts the phase to one node: the per-node step of a desynchronised
    /// generation timing, where each node is woken at its own phase. `None` walks them all.
    fn on_node_phase(
        &mut self,
        recorder: &mut dyn RunRecorder,
        horizon: SimTime,
        only: Option<NodeId>,
    ) {
        let now = self.scheduler.now();
        let step_s = self.scenario.time.mobility_step().as_secs_f64();
        // A node stepping at its own phase generates between two GNSS epochs. Its belief
        // is the fix of the last mobility step, so the node dead-reckons it forward to the
        // instant it is about to stamp on the message, from its *own* velocity estimate —
        // never from ground truth (invariant I-C2). Without this every message would carry
        // a position `v·φ` older than its generation time, which plausibility detectors
        // read, correctly, as an inconsistent sender. An OBU does the same: the BSM's
        // position is meant to be the position at `secMark`, and 04-models.md §8.1 records
        // J2945/1's latency-compensation requirement as secondary-sourced.
        if let Some(node) = only {
            self.dead_reckon_belief(node, now);
        }
        let mut inboxes = core::mem::take(&mut self.inboxes);
        let rng = &self.rng;

        let reverse = self.reverse_node_walk;
        let selected = move |id: &NodeId| only.is_none_or(|o| o == *id);
        let walk: Box<dyn Iterator<Item = (&NodeId, &mut ObuRuntime)>> = if reverse {
            Box::new(self.nodes.iter_mut().rev().filter(move |(id, _)| selected(id)))
        } else {
            Box::new(self.nodes.iter_mut().filter(move |(id, _)| selected(id)))
        };
        let mut results: Vec<(NodeId, StepOutcome, Vec<v2xw_core::ctx::OwnedRecord>, f64)> = walk
            .map(|(id, runtime)| {
                // What has finished arriving by now is handed over; a frame whose last
                // symbol (plus its propagation delay) lands after this instant waits for
                // the next step, so a node never processes a frame before it arrived.
                let inbox: Vec<(RxFrame, RxStamp)> = match inboxes.get_mut(id) {
                    Some(q) => {
                        let (ready, later): (Vec<_>, Vec<_>) = core::mem::take(q)
                            .into_iter()
                            .partition(|(_, st)| st.arrived_at.is_none_or(|t| t <= now));
                        *q = later;
                        ready
                    }
                    None => Vec::new(),
                };
                let mut local = v2xw_node::NodeRuntimeCtx::new(now, rng);
                // Distance travelled since the last step drives distance-based pseudonym
                // rotation. It is the node's own odometry, not a ground-truth read: a
                // fielded receiver integrates its own speed the same way.
                let travelled = runtime
                    .state()
                    .transmits()
                    .then(|| v2xw_core::NodeView::position(runtime).ground_speed_mps() * step_s);
                let outcome = runtime.step_timed(&mut local, inbox, travelled.unwrap_or(0.0));
                (*id, outcome, local.take_emitted(), travelled.unwrap_or(0.0))
            })
            .collect();

        // The merge (02-architecture.md §6.4). `par_iter_mut` over a `BTreeMap` yields in
        // key order but `collect` into a `Vec` does not promise to preserve it for an
        // unindexed parallel iterator, so the order is *re-established* here rather than
        // assumed. That is the difference between a run that is deterministic and one that
        // happens to be.
        results.sort_by_key(|(id, ..)| *id);

        for (_, _, records, _) in &results {
            for rec in records {
                self.providers.on_event(rec);
                recorder.write(now, rec);
                self.report.records += 1;
            }
        }

        // Every received frame whose fate the node settled, joined back to its attempt and
        // recorded on `node.rx`, in (node, report) order.
        let mut fates: Vec<NodeRx> = Vec::new();
        for (id, outcome, _, _) in &results {
            for report in &outcome.rx_reports {
                if let Some(attempt) = self.rx_pending.remove(&(*id, report.token)) {
                    fates.push(resolve_rx(attempt, report, now));
                }
            }
        }
        for fate in fates {
            self.emit(recorder, &fate);
        }

        for (id, outcome, _, _) in &results {
            for tx in &outcome.transmissions {
                self.hand_down(*id, tx, now, horizon);
            }
        }

        // The local detector suite, over what each node's own runtime delivered to its
        // applications. It runs here and not inside the node phase's map because a report
        // is a *transmission*, and the map may not touch the heap (ADR 0004 decision 5).
        if self.phase2.is_some() {
            for (id, outcome, _, _) in &results {
                // What the installed CRL cost the liar: every delivered message whose
                // signer the node's own revocation check refused. It is counted here,
                // from the node's own conclusion, and not from the engine knowing who the
                // attacker is.
                let revoked = outcome
                    .delivered
                    .iter()
                    .filter(|m| {
                        m.verification == v2xw_node::stores::VerificationState::Revoked
                    })
                    .count() as u64;
                if revoked > 0 && let Some(phase2) = self.phase2.as_mut() {
                    for _ in 0..revoked {
                        phase2.note_revoked_reception();
                    }
                }
                self.run_detectors(*id, &outcome.delivered, now, horizon);
            }
        }

        self.inboxes = inboxes;
    }

    /// Advances one node's position belief to `now` along its own believed velocity.
    ///
    /// Only the node's own estimate is read: position, velocity and the believed instant
    /// the estimate is valid at. A belief with no fix, or one already at or past the
    /// node's believed `now`, is left alone.
    fn dead_reckon_belief(&mut self, node: NodeId, now: SimTime) {
        let Some(runtime) = self.nodes.get_mut(&node) else {
            return;
        };
        let believed_now = runtime.clock().believed_time(now);
        let mut belief = *v2xw_core::NodeView::position(runtime);
        if !belief.fix.has_position() || believed_now <= belief.time_ns {
            return;
        }
        let dt = (believed_now - belief.time_ns) as f64 * 1e-9;
        belief.pos = Vec3::new(
            belief.pos.x + belief.vel.x * dt,
            belief.pos.y + belief.vel.y * dt,
            belief.pos.z + belief.vel.z * dt,
        );
        belief.time_ns = believed_now;
        runtime.set_belief(belief.quantized());
    }

    /// Runs one node's detector suite and puts any report it filed on the air.
    fn run_detectors(
        &mut self,
        node: NodeId,
        delivered: &[v2xw_node::VerifiedMessage],
        now: SimTime,
        horizon: SimTime,
    ) {
        if delivered.is_empty() {
            return;
        }
        let believed = self
            .nodes
            .get(&node)
            .map_or(now, |n| n.clock().believed_time(now));
        let belief = self.nodes.get(&node).map(v2xw_core::NodeView::position);
        let me = v2xw_threat::SelfBelief {
            node,
            believed_time: believed,
            x_m: belief.map_or(0.0, |b| b.pos.x),
            y_m: belief.map_or(0.0, |b| b.pos.y),
            radio_range_m: MAX_RANGE_M,
        };
        let reports = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                phase2,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            phase2
                .as_mut()
                .map(|p| p.detect(&mut ctx, node, &me, delivered))
                .unwrap_or_default()
        };
        // The node's counters are cumulative, so take the running total rather than
        // adding it: accumulating a cumulative counter once per step multiplies it by the
        // step count, which is how this diagnostic first reported 49 million failures out
        // of 70,925 messages.
        if let (Some(rt), Some(p2)) = (self.nodes.get(&node), self.phase2.as_mut()) {
            let (parse, sig) = (rt.spdu_parse_failures(), rt.spdu_signature_failures());
            let r = p2.report_mut();
            r.spdu_parse_failures = r.spdu_parse_failures.max(parse);
            r.spdu_signature_failures = r.spdu_signature_failures.max(sig);
        }
        for report in reports {
            let Some(signer) = self
                .nodes
                .get(&node)
                .and_then(|n| n.stores().certs.active().map(|c| c.digest.clone()))
            else {
                continue;
            };
            // The report's size on the air is the SCMS deployment's own figure for a
            // report submission, which is one of the five wire sizes 05-protocols marks
            // as having no published value and which `v2xw-proto` carries with its
            // provenance rather than inventing here.
            let bytes = crate::phase2::report_bytes();
            // `Transmission::sized`, not a struct literal: the report's size comes from
            // `v2xw-proto`'s own wire table and nothing builds the octets, which is
            // exactly the case the constructor exists for — and it is one edit in
            // `v2xw-node` rather than one here when that struct grows a field.
            // Generation and signing are both on the node's own clock, as they are for
            // every frame the node's generator builds.
            let tx = Transmission::sized(
                v2xw_msg::MsgType::Mbr,
                bytes,
                signer,
                true,
                self.signing_cost(node).after(believed),
                believed,
            );
            self.hand_down_app(
                node,
                &tx,
                now,
                horizon,
                Some(AppPayload::Report(Box::new(report))),
            );
        }
    }

    /// Hands one transmission down to the MAC.
    ///
    /// The node does not put a frame on the air: it finishes a signature, and the frame
    /// then waits for channel access. This schedules the [`Event::MacTimer`] at the
    /// instant the signature completes, which is where [`Engine::on_mac_timer`] picks it
    /// up. At the abstract tier there is no MAC, and the frame is scheduled straight to
    /// the air one AIFS later.
    ///
    /// A message the node's own generator produced also waits its hand-off jitter
    /// (`v2xw_msg::GenerationTiming::jitter`) between the signature and the access layer:
    /// host and stack latency, and what keeps two stations whose phases coincide from
    /// colliding on every period. The engine's own application frames (a misbehaviour
    /// report, a CRL broadcast) go through [`Engine::hand_down_app`] directly and do not.
    fn hand_down(&mut self, node: NodeId, tx: &Transmission, now: SimTime, horizon: SimTime) {
        let jitter = self.gen_timing.jitter(&self.rng, node, tx.generation_time);
        if jitter.as_nanos() == 0 {
            self.hand_down_app(node, tx, now, horizon, None);
            return;
        }
        let mut delayed = tx.clone();
        delayed.ready_at = jitter.after(tx.ready_at);
        self.hand_down_app(node, &delayed, now, horizon, None);
    }

    /// [`Engine::hand_down`] with an application payload attached.
    fn hand_down_app(
        &mut self,
        node: NodeId,
        tx: &Transmission,
        now: SimTime,
        horizon: SimTime,
        app: Option<AppPayload>,
    ) {
        // The signing latency is a *duration* on the node's own clock, so it is
        // independent of the node's clock offset: `ready_at` and the believed instant are
        // both on that clock and the difference between them is a real interval.
        let believed = self
            .nodes
            .get(&node)
            .map_or(now, |n| n.clock().believed_time(now));
        let signing = Duration::between(believed, tx.ready_at);
        let ready = signing.after(now);
        // The node's clock stamps, moved onto the simulation's timeline by their distance
        // from the node's own "now": an offset clock shifts every stamp alike and leaves
        // the durations between them exact.
        let on_timeline = |x: SimTime| -> SimTime {
            if x <= believed {
                now.saturating_sub(believed - x)
            } else {
                now.saturating_add(x - believed)
            }
        };
        let t_generated = on_timeline(tx.generation_time);
        let t_sign_start = on_timeline(tx.sign_start).max(t_generated);
        if ready > horizon {
            return;
        }
        if self.is_dilated(ready) {
            self.report.suppressed_frames += 1;
            return;
        }
        // `fragmenter/none` (04-models.md §7.3): neither WSMP nor GeoNetworking can split an
        // SDU, so a signed message above the network layer's MTU is refused here, before
        // the MAC, and counted, rather than handed down to be refused by the PHY's MSDU cap
        // with its headers already counted as offered load.
        if tx.bytes > self.net.sdu_mtu() {
            self.report.net_mtu_refusals += 1;
            return;
        }
        let belief = self.nodes.get(&node).map(v2xw_core::NodeView::position);
        let frame = FrameSeq::new(self.next_frame);
        self.next_frame += 1;
        // The credential's i-period and linkage value, which is what a CRL revokes and
        // therefore what the receiver's revocation check reads (`crate::phase2`, joint 1).
        let credential = self
            .nodes
            .get(&node)
            .and_then(|n| n.stores().certs.active().cloned());
        let (claimed_cert_period, claimed_linkage) = match (&credential, self.phase2.as_ref()) {
            (Some(cred), Some(phase2)) => (
                cred.i_period,
                phase2
                    .creds(node)
                    .iter()
                    .find(|c| c.i == cred.i_period && c.j == cred.j_index)
                    .map(|c| c.lv),
            ),
            _ => (0, None),
        };
        // Joint 2's map, filled here and not at spawn: this is the first instant the
        // digest a receiver will see is the digest the sender's store holds, because the
        // node rewrote it when it issued the certificate it is about to sign with. A
        // subject a report names is a subject that was on the air, so registering here
        // registers exactly the digests that can be reported — and nothing else.
        if let (Some(cred), Some(phase2)) = (credential.as_ref(), self.phase2.as_mut()) {
            phase2.note_digest(node, &cred.digest, cred.i_period, cred.j_index);
        }
        // The attacker's edit, immediately before the frame is built: everything after it
        // — the signing cost already paid, the MAC, DCC, the PHY — is the ordinary path.
        let mut claim = (
            belief.map_or(Vec3::ZERO, |b| b.pos),
            belief.map_or(0.0, v2xw_core::PositionEstimate::ground_speed_mps),
            belief.map_or(0.0, |b| b.heading_rad),
        );
        let mut signature_valid = true;
        if self.phase2.as_ref().is_some_and(|p| p.is_attacker(node)) {
            let actor = self
                .actors
                .iter()
                .find(|(_, a)| a.node == Some(node))
                .map(|(id, _)| *id)
                .unwrap_or(ActorId::new(0));
            let believed = self
                .nodes
                .get(&node)
                .map_or(now, |n| n.clock().believed_time(now));
            let honest = v2xw_threat::HonestClaim {
                x_m: claim.0.x,
                y_m: claim.0.y,
                speed_mps: claim.1,
                heading_rad: claim.2,
            };
            let me = v2xw_threat::SelfBelief {
                node,
                believed_time: believed,
                x_m: claim.0.x,
                y_m: claim.0.y,
                radio_range_m: MAX_RANGE_M,
            };
            let signer = credential
                .as_ref()
                .map_or([0u8; 8], |c| crate::phase2::digest_bytes(&c.digest));
            let cert = credential
                .as_ref()
                .map_or((0, SimTime::MAX), |c| (c.valid_from, c.valid_until));
            let emission = {
                let Engine {
                    scheduler,
                    rng,
                    world,
                    snapshot,
                    provenance,
                    params,
                    phase2,
                    ..
                } = self;
                let mut null = crate::ctx::NullRecorder::new();
                let mut ctx = EngineCtx::new(
                    scheduler, rng, world, snapshot, provenance, params, &mut null,
                );
                phase2.as_mut().and_then(|p| {
                    p.falsify(
                        &mut ctx,
                        node,
                        actor,
                        believed,
                        signer,
                        honest,
                        me,
                        cert,
                        u64::from(frame.index()),
                    )
                })
            };
            if let Some(e) = emission {
                claim = (Vec3::new(e.x_m, e.y_m, claim.0.z), e.speed_mps, e.heading_rad);
                signature_valid = e.signature_valid;
            }
        }
        let _ = signature_valid;
        // The transmit power is congestion control's, not the scenario's: J2945/1 controls
        // power as well as rate, and the SUPRA filter's output is what the link budget has
        // to be evaluated at. With no DCC model (the abstract tier) it is the profile's.
        let (tx_power_dbm, channel) = match self.sidelink.as_ref() {
            Some(sl) => (sl.tx_power_dbm, sl.channel),
            None => (self.dcc_power_dbm(node), SAFETY_CHANNEL),
        };
        // The frame on the air is the SPDU inside a network and transport header, LLC/SNAP,
        // the 802.11 MAC header and the FCS (`v2xw_net::frame`); the PHY's air time and the
        // MSDU cap are over all of it, not over the SPDU alone.
        let mut layers = v2xw_net::FrameLayers::compose(
            &self.net,
            if tx.msg_type == v2xw_msg::MsgType::Denm {
                v2xw_net::FrameMsg::Denm
            } else {
                v2xw_net::FrameMsg::Safety
            },
            tx.payload_bytes().zip(tx.envelope_bytes()),
            tx.bytes,
        );
        if self.sidelink.is_some() {
            // A PC5 transport block carries the network PDU with no LLC/SNAP, no 802.11
            // MAC header and no FCS. Its own layer-2 headers (PDCP, RLC, MAC) and the CRC
            // are not modelled, so the link layer's share is zero rather than borrowed
            // from 802.11, and the overhead metrics say so by counting none.
            layers.llc_snap = 0;
            layers.mac_header = 0;
            layers.fcs = 0;
        }
        let psdu = layers.psdu_bytes();
        let descriptor = FrameDescriptor {
            bytes: psdu,
            mcs: SAFETY_MCS,
            tx_power_dbm,
            channel,
            ac: SAFETY_AC,
            kind: FrameKind::Broadcast,
            // One SDU per frame: no fragmentation model is wired in, so the SDU id and
            // the frame number are the same counter seen from two layers.
            sdu_ref: SduRef::new(v2xw_core::ids::SduId::new(frame.index()), frame),
        };
        // A sidelink transport block occupies one slot whatever it carries; its size
        // decides how many sub-channels it takes instead (04-models.md §5.1, §5.2).
        let air = match self.sidelink.as_ref() {
            Some(sl) => sl.slot(),
            None => v2xw_radio::air_time(psdu, SAFETY_MCS),
        };
        self.frames.insert(
            frame,
            FrameState {
                tx: node,
                // Filled in when the grant fixes the transmit instant; a frame that is
                // still queued has no position on the air yet.
                tx_pos: Vec3::ZERO,
                bytes: tx.bytes,
                msg_type: tx.msg_type,
                signer: tx.signer.clone(),
                full_certificate: tx.full_certificate,
                generation_time: tx.generation_time,
                claimed_pos: claim.0,
                claimed_speed_mps: claim.1,
                claimed_heading_rad: claim.2,
                ready_at: ready,
                start: ready,
                end: air.after(ready),
                air,
                descriptor,
                tx_handle: None,
                arrivals: BTreeMap::new(),
                claimed_cert_period,
                claimed_linkage,
                app,
                spdu: tx.signed.as_ref().map(|f| f.spdu.clone()),
                // Read from the node's own `SignedFrame`, not recomputed here: the split
                // between payload and envelope is the security stack's answer and the
                // engine has no business having a second one.
                payload_bytes: tx.payload_bytes(),
                envelope_bytes: tx.envelope_bytes(),
                sl_resource: None,
                sl_interferers: BTreeMap::new(),
                focus_high: std::collections::BTreeSet::new(),
                layers,
                cert_bytes: tx.cert_bytes(),
                t_generated,
                t_sign_start,
                // The abstract tier's frame waits exactly one AIFS and counts no backoff;
                // the MAC's grant overwrites both at the medium and high tiers.
                mac_aifs_ns: AIFS.as_nanos(),
                mac_backoff_ns: 0,
                content: Some(message_content(
                    tx.msg_type,
                    tx.signed.as_ref().map(|f| f.payload.as_slice()),
                    &tx.signer,
                    claim,
                )),
            },
        );
        if self.mac.is_some() || self.sidelink.is_some() {
            self.pending_tx.entry(node).or_default().push((ready, frame));
            self.scheduler.schedule(
                ready,
                EventClass::MacTimer,
                Event::MacTimer {
                    node,
                    channel: channel.0,
                },
            );
        } else {
            let at = AIFS.after(ready);
            if at > horizon {
                self.frames.remove(&frame);
                return;
            }
            if let Some(state) = self.frames.get_mut(&frame) {
                state.start = at;
                state.end = state.air.after(at);
            }
            self.scheduler.schedule(
                at,
                EventClass::PhyStart,
                Event::PhyStart { frame, tx: node },
            );
        }
    }

    /// The transmit power congestion control allows this node, dBm.
    fn dcc_power_dbm(&self, node: NodeId) -> f64 {
        self.dcc
            .as_ref()
            .and_then(|d| <SaeJ2945Dcc as Dcc<EngineCtx<'_>>>::state(d, node).power_dbm)
            .unwrap_or(crate::wiring::TX_POWER_DBM)
    }

    /// One node's medium-access state machine advances (invariant I-R1's access half).
    ///
    /// Three things happen, in this order: every frame whose signature has finished since
    /// the last timer is queued, the access state machine is polled for as many grants as
    /// it will give, and the next timer is scheduled from the MAC's own
    /// [`Mac::next_poll_at`] so a transmission happens at the slot boundary the backoff
    /// computed rather than at whatever cadence the engine polls on.
    ///
    /// # What the medium tier's MAC does and does not do
    ///
    /// It applies AIFS, a `CWmin` backoff countdown drawn from `(MacBackoff, Node)`,
    /// deferral to a busy medium, the per-access-category queue and its overflow drops,
    /// and it measures the channel busy ratio that congestion control reads.
    ///
    /// The clear-channel assessment is **sampled at poll instants**, not driven by a CCA
    /// transition event per node per overlapping frame. That is the one divergence from a
    /// fully event-driven CSMA/CA, and it is not a loss of fidelity in the deferral
    /// decision, because the engine closes the gap from the other side: when the sample
    /// says busy, [`Engine::medium_idle_at`] computes the instant the last overlapping
    /// arrival at this node ends and the timer is rescheduled *there*. So a node defers
    /// for exactly as long as the medium is occupied, and the events cost one per waiting
    /// node per in-flight frame rather than one per node per frame.
    ///
    /// What it does not model is the *capture* side of carrier sense: a node that starts
    /// transmitting in the same nanosecond as another cannot have sensed it, and the
    /// engine does not compute the transmitter-to-transmitter link budget that would tell
    /// a receiver whether a collider was hidden. Every collision is therefore reported as
    /// [`LossCause::Collision`] and never as [`LossCause::HiddenTerminal`]; the
    /// distinction is the high tier's, and the `audible_to_victim_tx` field the PHY takes
    /// for it is the seam.
    fn on_mac_timer(&mut self, node: NodeId, channel: ChannelId, horizon: SimTime) {
        if self.sidelink.is_some() {
            self.on_sidelink_timer(node, horizon);
            return;
        }
        if self.mac.is_none() {
            return;
        }
        let now = self.scheduler.now();

        // 1. The medium as this node's own energy detector sees it. Sampled here rather
        //    than delivered as a transition event; see the note above on what that costs.
        let cca = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                phy,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            Phy::cca(phy, &ctx, node, channel)
        };
        let busy = matches!(cca, v2xw_radio::CcaState::Busy { .. });
        {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                mac,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            let mac = mac.as_mut().expect("checked above");
            Mac::on_cca(mac, &mut ctx, node, channel, cca);
        }

        // 2. Everything whose signature has finished. The list is sorted by ready instant
        //    and then by frame number, so two frames that became ready in the same
        //    nanosecond are queued in generation order. It happens *after* the CCA report,
        //    because `Mac::enqueue` arms the backoff against the medium state and would
        //    otherwise arm it against the state at the previous timer.
        let mut ready: Vec<(SimTime, FrameSeq)> = Vec::new();
        if let Some(pending) = self.pending_tx.get_mut(&node) {
            pending.sort_unstable();
            let split = pending.partition_point(|(t, _)| *t <= now);
            ready.extend(pending.drain(..split));
            if pending.is_empty() {
                self.pending_tx.remove(&node);
            }
        }
        for (_, frame) in ready {
            let Some((descriptor, air)) = self.frames.get(&frame).map(|f| (f.descriptor, f.air))
            else {
                continue;
            };
            let window = self.mac_window.entry(node).or_default();
            window.frames += 1;
            window.bytes += u64::from(descriptor.bytes);
            window.airtime_us += air.as_nanos() / 1_000;
            let refused = {
                let Engine {
                    scheduler,
                    rng,
                    world,
                    snapshot,
                    provenance,
                    params,
                    mac,
                    ..
                } = self;
                let mut null = crate::ctx::NullRecorder::new();
                let mut ctx = EngineCtx::new(
                    scheduler, rng, world, snapshot, provenance, params, &mut null,
                );
                let mac = mac.as_mut().expect("checked above");
                Mac::enqueue(
                    mac,
                    &mut ctx,
                    node,
                    MacSdu {
                        frame: descriptor,
                        enqueued_at: now,
                    },
                    SAFETY_AC,
                )
                .err()
            };
            if refused.is_some() {
                // The frame never reaches the air, so its state is dropped here rather
                // than left in the map to be swept later: nothing else can resolve it.
                self.frames.remove(&frame);
                self.report.mac_drops += 1;
                self.mac_window.entry(node).or_default().drops += 1;
            }
        }

        // 3. As many grants as the state machine will give. A busy medium gives none, and
        //    `Mac::poll` says so itself; the loop is bounded so a node with a backlog
        //    cannot spin here.
        for _ in 0..MAX_GRANTS_PER_TIMER {
            let grant = {
                let Engine {
                    scheduler,
                    rng,
                    world,
                    snapshot,
                    provenance,
                    params,
                    mac,
                    ..
                } = self;
                let mut null = crate::ctx::NullRecorder::new();
                let mut ctx = EngineCtx::new(
                    scheduler, rng, world, snapshot, provenance, params, &mut null,
                );
                let mac = mac.as_mut().expect("checked above");
                Mac::poll(mac, &mut ctx, node, channel)
            };
            let Some(grant) = grant else { break };
            let frame = grant.sdu.frame.sdu_ref.seq;
            // `TxGrant::at` is the slot boundary the backoff computed, which may be in the
            // past when the poll is coarser than the slot; the frame cannot go on the air
            // before now, and the difference is the access delay the engine owes the MAC.
            let at = grant.at.max(now);
            if at > horizon {
                self.frames.remove(&frame);
                continue;
            }
            self.report.mac_grants += 1;
            if let Some(state) = self.frames.get_mut(&frame) {
                state.start = at;
                state.end = state.air.after(at);
                self.report.mac_access_delay_ns += at.saturating_sub(state.ready_at);
                // The split of the access delay the latency decomposition reports: one
                // AIFS of the frame's category and the slots the MAC counted down; the
                // rest is deferral to a busy medium.
                state.mac_aifs_ns = SAFETY_AC.aifs().as_nanos();
                state.mac_backoff_ns = u64::from(grant.backoff_slots)
                    * v2xw_radio::types::timing::SLOT_TIME.as_nanos();
            } else {
                continue;
            }
            if at == now {
                // Access won *at this instant*: the frame goes on the air here, inside the
                // MAC handler, rather than through a `PhyStart` event at the same instant.
                //
                // The difference is carrier sense. `MacTimer` is priority 4 and `PhyStart`
                // is priority 5 (02-architecture.md §5.1), so every node's MAC decision at
                // an instant is dispatched before any transmission at that instant. Going
                // through the event meant that a node polling in the same nanosecond as
                // another could not sense it: every node saw an idle medium, every node
                // was granted immediately with a zero backoff, and every frame collided
                // with every other. Measured on a three-node run, 95 % of all reception
                // attempts were lost to `half-duplex` — the receiver was transmitting its
                // own frame over the same window — and the packet delivery ratio was 0.11
                // at every distance, which is not a propagation result at all.
                //
                // Registering the transmission here closes that: the next node's poll at
                // this instant reads a busy medium from the PHY's own arrival set, defers
                // to the end of the frame, and then contends with a real contention-window
                // draw, because `on_cca(Idle)` has set `idle_since` and the AIFS test no
                // longer passes trivially. That is CSMA/CA, and it is what the medium tier
                // claims to model.
                self.start_frame(frame, horizon);
            } else {
                self.scheduler.schedule(
                    at,
                    EventClass::PhyStart,
                    Event::PhyStart { frame, tx: node },
                );
            }
        }

        // 4. A deferring node has to be woken when the medium clears, or its frame waits
        //    until the next one becomes ready — which at 10 Hz is a tenth of a second of
        //    access delay invented by the poll cadence.
        if busy {
            let clear = self.medium_idle_at(node, now);
            if clear > now && clear <= horizon {
                self.scheduler.schedule(
                    clear,
                    EventClass::MacTimer,
                    Event::MacTimer {
                        node,
                        channel: channel.0,
                    },
                );
            }
        }

        // 5. The next timer, from the MAC's own timing.
        let next = self
            .mac
            .as_ref()
            .and_then(|m| Mac::<EngineCtx<'_>>::next_poll_at(m, node, channel));
        if let Some(next) = next {
            // Strictly in the future: a timer at `now` would dispatch again at this
            // instant and the loop would not advance.
            let at = next.max(now + 1);
            if at <= horizon {
                self.scheduler.schedule(
                    at,
                    EventClass::MacTimer,
                    Event::MacTimer {
                        node,
                        channel: channel.0,
                    },
                );
            }
        }
    }

    /// The instant the medium stops being busy at one node, by its own energy detector.
    ///
    /// The maximum end over every arrival registered at this node whose received power
    /// reaches the CCA threshold, and over this node's own transmission — a transmitting
    /// radio is not listening, and 802.11p is half duplex. `now` when nothing is in
    /// flight, so a caller can compare it against `now` and find out that the medium is
    /// already clear.
    ///
    /// It is the engine's job rather than the PHY's because the PHY is asked "is it busy
    /// *now*" and answering "until when" needs the arrival set, the CCA configuration and
    /// the frame table together.
    fn medium_idle_at(&self, node: NodeId, now: SimTime) -> SimTime {
        let threshold = self.phy.cca_config().cca_threshold_dbm();
        let mut clear = now;
        for frame in self.live_at_rx.get(&node).into_iter().flatten() {
            let Some(state) = self.frames.get(frame) else {
                continue;
            };
            if state.arrivals.get(&node).is_some_and(|&(p, _)| p >= threshold) {
                clear = clear.max(state.end);
            }
        }
        for state in self.frames.values() {
            if state.tx == node && state.tx_handle.is_some() && state.end > now {
                clear = clear.max(state.end);
            }
        }
        // A jammer above the energy-detection threshold holds the medium busy for as long
        // as its window lasts: the denial of channel access a constant jammer causes.
        for a in self.phy.jamming().at(node) {
            if a.power_dbm >= threshold && a.window.from <= now && a.window.to > now {
                clear = clear.max(a.window.to);
            }
        }
        clear
    }

    /// A frame begins: the PHY starts the transmission, and the arrival set is registered.
    ///
    /// The receiver set is resolved **here**, not at `PhyEnd`, and that is a change from
    /// the Phase 1 build. The reason is interference: a frame that starts later must be
    /// able to declare itself an interferer of every frame already in flight at each
    /// shared receiver, and it can only do that if those arrivals exist. Resolving the set
    /// at the end of the frame instead made every SINR a plain SNR, because there was
    /// nothing for a concurrent frame to be added to.
    ///
    /// The outcome is still decided at `PhyEnd` (invariant I-R2): what happens here is the
    /// *geometry*, and nothing about it depends on the order frames are started in.
    fn on_phy_start(&mut self, frame: FrameSeq, horizon: SimTime) {
        self.start_frame(frame, horizon);
    }

    /// Puts one frame on the air at the current instant: the shared body of
    /// [`Engine::on_phy_start`] and of a grant won at the instant it is polled.
    fn start_frame(&mut self, frame: FrameSeq, horizon: SimTime) {
        let now = self.scheduler.now();
        // Taken out of the map for the duration, so the interference walk below can read
        // every *other* live frame without fighting the borrow checker over this one.
        let Some(mut state) = self.frames.remove(&frame) else {
            return;
        };
        let Some(pos) = self.node_pos(state.tx, now) else {
            // The transmitter despawned between the grant and the air. Nothing to do: the
            // frame is gone with it.
            return;
        };
        state.tx_pos = pos;
        state.start = now;
        state.end = state.air.after(now);

        // The PHY owns the air time, the transmit interval for the half-duplex test, and
        // the transmitted half of the air-time ledger. A sidelink transport block's
        // handle is the slot it was granted, and its half-duplex and interference
        // bookkeeping is `sidelink`'s.
        let handle = if self.sidelink.is_some() {
            Ok(v2xw_radio::TxHandle {
                id: u64::from(frame.index()) + 1,
                tx: state.tx,
                channel: state.descriptor.channel,
                start: now,
                end: state.air.after(now),
                air_time: state.air,
            })
        } else {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                phy,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            Phy::begin_tx(phy, &mut ctx, state.tx, &state.descriptor)
        };
        let handle = match handle {
            Ok(h) => h,
            Err(_) => {
                // Over the MSDU cap: 04-models.md §4.6 says the fragmenter must have
                // acted first, and none is wired in, so the frame is refused and counted.
                self.report.phy_refusals += 1;
                return;
            }
        };
        state.tx_handle = Some(handle);
        state.end = handle.end;
        self.report.frames_transmitted += 1;
        // §3.3.4 bit 4: "transmitted at least once in the last mobility step". Set where
        // the preamble actually goes on the air, not where the node decided to send, so
        // a frame the MAC dropped does not light the bit.
        self.transmitted_since_step.insert(state.tx);
        if state.end > horizon {
            // The frame would finish after the run does, so its outcome is never
            // evaluated. It still occupied the medium, which `begin_tx` has recorded.
            return;
        }

        // Stage 1: the candidate set — every equipped actor within the modelled range, by
        // the grid query ADR 0004 decision 6 sizes for exactly this.
        let mut candidates: Vec<(NodeId, Vec3)> = Vec::new();
        for actor in self.snapshot.actors_within(state.tx_pos, MAX_RANGE_M) {
            let Some(rec) = self.actors.get(&actor) else {
                continue;
            };
            let Some(node) = rec.node else { continue };
            if node == state.tx {
                continue;
            }
            candidates.push((node, rec.last.extrapolate(now).pos));
        }
        // The roadside units, which are nodes and not actors and so are not in the grid.
        // The walk is over a `BTreeMap`, and there are units rather than vehicles of them,
        // so a linear distance test is the whole cost.
        for (&rsu, &rsu_pos) in &self.rsus {
            if rsu == state.tx {
                continue;
            }
            if state.tx_pos.distance(rsu_pos) <= MAX_RANGE_M {
                candidates.push((rsu, rsu_pos));
            }
        }
        candidates.sort_by_key(|(n, _)| *n);

        // Stage 2: the link budgets, sequentially, because the models carry state — a
        // shadowing process is correlated along a trajectory, which is why it has state
        // at all.
        for &(rx, rx_pos) in &candidates {
            let (rssi, dist, high) = self.link_budget(&state, rx, rx_pos);
            state.arrivals.insert(rx, (rssi, dist));
            if high {
                state.focus_high.insert(rx);
            }
        }
        // A reactive jammer that hears this frame jams it at its receivers.
        self.react_to_frame(
            state.tx,
            state.tx_pos,
            state.descriptor.tx_power_dbm,
            state.start,
            state.end,
            &candidates,
        );

        if self.sidelink.is_some() {
            self.sidelink_register(frame, &mut state);
            for &rx in state.arrivals.keys() {
                self.live_at_rx.entry(rx).or_default().push(frame);
            }
            self.scheduler
                .schedule(state.end, EventClass::PhyEnd, Event::PhyEnd { frame });
            self.frames.insert(frame, state);
            return;
        }

        // Stage 3: register the arrivals, and cross-declare interference with everything
        // already in flight at each shared receiver. Gathered first and applied second,
        // because the gather reads `self.frames` and the apply writes `self.phy`.
        let mut overlaps: Vec<(NodeId, InterferenceSource, RxHandle)> = Vec::new();
        for (&rx, &(power, _)) in &state.arrivals {
            let _ = power;
            for other in self.live_at_rx.get(&rx).into_iter().flatten() {
                let Some(o) = self.frames.get(other) else {
                    continue;
                };
                // Half-open overlap on `[start, end)`, the same convention the PHY's own
                // window partition uses.
                if o.start >= state.end || state.start >= o.end {
                    continue;
                }
                let Some(&(o_power, _)) = o.arrivals.get(&rx) else {
                    continue;
                };
                overlaps.push((
                    rx,
                    InterferenceSource::new(o.tx, o_power, o.start, o.end),
                    RxHandle {
                        tx: o.tx_id(),
                        rx,
                    },
                ));
            }
        }
        let tx_id = state.tx_id();
        for (&rx, &(power, _)) in &state.arrivals {
            self.phy.register_arrival(Arrival {
                tx_id,
                tx: state.tx,
                rx,
                power_dbm: power,
                start: state.start,
                end: state.end,
                frame: state.descriptor,
                interferers: Vec::new(),
            });
            // The channel was busy at this receiver for the whole frame, as far as its
            // energy detector is concerned. This is what congestion control reads, and it
            // is fed from received power rather than from a CCA state machine — see
            // `on_mac_timer` for why.
            if power >= v2xw_radio::phy::CBR_BUSY_THRESHOLD_DBM
                && let Some(mac) = self.mac.as_mut()
            {
                mac.note_busy(rx, SAFETY_CHANNEL, state.start, state.end);
            }
        }
        for (rx, source, victim) in overlaps {
            // The frame already in flight gains this one as an interferer …
            let _ = self.phy.add_interferer(
                victim,
                InterferenceSource::new(state.tx, state.arrivals[&rx].0, state.start, state.end),
            );
            // … and this one gains it.
            let _ = self.phy.add_interferer(RxHandle { tx: tx_id, rx }, source);
        }
        for &rx in state.arrivals.keys() {
            self.live_at_rx.entry(rx).or_default().push(frame);
        }

        self.scheduler
            .schedule(state.end, EventClass::PhyEnd, Event::PhyEnd { frame });
        self.frames.insert(frame, state);
    }

    /// The reception phase (ADR 0004 decision 5, invariant I-R2).
    ///
    /// The arrival set and every received power were fixed when the frame started; what
    /// happens here is the **decision**, per receiver. For 802.11p it is
    /// [`v2xw_radio::OfdmPhy::decide`] — the PHY's own evaluation, half duplex, then
    /// sensitivity, then (at the high tier or inside a high focus region) preamble capture,
    /// then the error model against the per-window SINR with the jamming counterfactual —
    /// in parallel over receivers, each with its keyed `(link, frame)` draw. For a
    /// sidelink it is `v2xw_radio::SidelinkPhy::evaluate` ([`sidelink`]).
    ///
    /// The 802.11p decision used to be re-composed here from the PHY's public primitives,
    /// because the PHY's own was private. The re-composition had drifted: it never
    /// attributed a loss to a jammer, so a jammed frame was reported as a collision.
    fn on_phy_end(&mut self, recorder: &mut dyn RunRecorder, frame: FrameSeq) {
        let Some(state) = self.frames.remove(&frame) else {
            return;
        };
        let now = self.scheduler.now();
        let frame_index = u64::from(frame.index());

        let tx_id = state.tx_id();
        let mut outcomes: Vec<LinkOutcome> = if self.sidelink.is_some() {
            self.sidelink_outcomes(&state)
        } else {
            self.dsrc_outcomes(&state)
        };

        // The merge. `par_iter` over a `BTreeMap` is not an indexed parallel iterator, so
        // the order the results arrive in is `rayon`'s business; the guarantee the run
        // depends on is stated here rather than inherited from a library's iterator kind.
        outcomes.sort_by_key(|o| o.rx);
        self.finish_phy_end(recorder, frame, state, now, frame_index, tx_id, outcomes);
    }

    /// The 802.11p reception decisions for one frame, per receiver, in parallel.
    fn dsrc_outcomes(&self, state: &FrameState) -> Vec<LinkOutcome> {
        let phy = &self.phy;
        let rng = &self.rng;
        let domain = OfdmPhy::frame_error_domain();
        let high = Phy::<EngineCtx<'_>>::tier(phy) == Tier::High;
        let tx_id = state.tx_id();
        state
            .arrivals
            .par_iter()
            .map(|(&rx, &(power_dbm, distance_m))| {
                let mut out = LinkOutcome {
                    rx,
                    rssi_dbm: v2xw_radio::numeric::q_db(power_dbm),
                    sinr_db: f64::NEG_INFINITY,
                    distance_m,
                    received: false,
                    cause: Some(LossCause::OutOfRange),
                };
                let Some(arrival) = phy.arrival(RxHandle { tx: tx_id, rx }) else {
                    // Nothing was registered for this receiver, which the PHY reports as
                    // out of range rather than as a reception that failed.
                    return out;
                };
                let windows = phy.sinr_windows(arrival);
                // Reported, never used for the decision: the decision is per window.
                let mean_sinr = if windows.is_empty() {
                    f64::NEG_INFINITY
                } else {
                    v2xw_core::math::sum_ordered(windows.iter().map(|(_, _, s)| *s))
                        / windows.len() as f64
                };
                out.sinr_db = v2xw_radio::numeric::q_db(mean_sinr);
                // The draw is keyed by (link, frame), so a receiver's outcome depends on
                // neither the thread that computed it nor how many frames the link has
                // already carried.
                let draw = rng.checkout(domain, OfdmPhy::frame_key(arrival)).f64();
                let decided_high = high || state.focus_high.contains(&rx);
                match phy.decide(arrival, decided_high, draw) {
                    v2xw_radio::RxOutcome::Received { .. } => {
                        out.received = true;
                        out.cause = None;
                    }
                    v2xw_radio::RxOutcome::Lost(cause) => out.cause = Some(cause),
                }
                out
            })
            .collect()
    }

    /// Records, delivers and retires one frame whose outcomes have been decided.
    #[allow(clippy::too_many_arguments)]
    fn finish_phy_end(
        &mut self,
        recorder: &mut dyn RunRecorder,
        frame: FrameSeq,
        state: FrameState,
        now: SimTime,
        frame_index: u64,
        tx_id: u64,
        outcomes: Vec<LinkOutcome>,
    ) {
        let mut received_any = false;
        // Which receivers decoded a frame carrying an application payload, and when it
        // reached them, so the payload is acted on once per receiver after every outcome
        // has been recorded.
        let mut delivered_app: Vec<(NodeId, SimTime)> = Vec::new();
        let psdu = state.layers.psdu_bytes();
        let air_us = state.air.as_nanos() / 1_000;
        for outcome in outcomes {
            self.report.reception_attempts += 1;
            if let Some(cause) = outcome.cause {
                self.report.lost(cause);
            }
            let record = PhyRx::new(
                state.start,
                now,
                state.tx,
                outcome.rx,
                frame_index,
                outcome.rssi_dbm,
                outcome.sinr_db,
                if outcome.received {
                    RxOutcome::Ok
                } else {
                    RxOutcome::Lost
                },
                outcome.cause.map(cause_name),
                outcome.distance_m,
            );
            self.emit(recorder, &record);
            // The same attempt, on `node.rx`, with the sender's side of the journey. A PHY
            // loss is its fate already; a decoded frame's fate is the receiving node's to
            // decide, and it waits in `rx_pending` until the node reports it.
            let arrival =
                v2xw_radio::phy::propagation_delay(outcome.distance_m).after(state.end);
            let attempt = NodeRx::attempt(
                state.tx,
                outcome.rx,
                frame_index,
                msg_type_name(state.msg_type),
                outcome.rssi_dbm,
                outcome.sinr_db,
                outcome.distance_m,
                u64::from(psdu),
                air_us,
                state.payload_bytes.map(u64::from),
            )
            .journey(
                state.t_generated,
                state.t_sign_start,
                state.ready_at,
                state.mac_aifs_ns,
                state.mac_backoff_ns,
                state.start,
                state.end,
                arrival,
            );
            if !outcome.received {
                let cause = outcome.cause.map_or("unknown", cause_name);
                self.emit(recorder, &attempt.lost(now, cause));
            } else {
                self.report.receptions_ok += 1;
                received_any = true;
                let token = self.next_rx_token;
                self.next_rx_token += 1;
                let handed = self.inboxes.contains_key(&outcome.rx);
                if handed {
                    self.rx_pending.insert((outcome.rx, token), attempt);
                } else {
                    // Decoded by a radio whose node no longer exists.
                    self.emit(recorder, &attempt.lost(now, rx_cause::RECEIVER_OFF));
                }
                if let Some(inbox) = self.inboxes.get_mut(&outcome.rx) {
                    inbox.push((RxFrame {
                        signer: Some(state.signer.clone()),
                        msg_type: state.msg_type,
                        bytes: state.bytes,
                        claimed_pos: Some(state.claimed_pos),
                        claimed_speed_mps: state.claimed_speed_mps,
                        claimed_heading_rad: state.claimed_heading_rad,
                        claimed_generation_time: state.generation_time,
                        full_certificate: state.full_certificate,
                        // Modelled crypto: the engine knows the sender's key is genuine,
                        // so the signature is valid. The receiver only learns it by
                        // *spending* the verification time, which `ObuRuntime::step`
                        // charges against its servers.
                        signature_valid: true,
                        claimed_cert_period: state.claimed_cert_period,
                        claimed_linkage: state.claimed_linkage,
                        spdu: state.spdu.clone(),
                    }, RxStamp {
                        token,
                        arrived_at: Some(arrival),
                    }));
                }
                if state.app.is_some() {
                    delivered_app.push((outcome.rx, arrival));
                }
            }
        }
        if received_any {
            self.report.frames_received += 1;
        }
        if let Some(app) = state.app.clone() {
            for (rx, arrival) in delivered_app {
                let journey = ReportJourney {
                    msg: frame_index,
                    t_generated: state.t_generated,
                    t_sign_start: state.t_sign_start,
                    t_signed: state.ready_at,
                    t_tx_start: state.start,
                    t_tx_end: state.end,
                    t_arrival: arrival,
                };
                self.on_app_message(
                    recorder,
                    rx,
                    state.tx,
                    &app,
                    now,
                    self.scenario.time.horizon_ns(),
                    journey,
                );
            }
        }

        // The bookkeeping, after every decision has been taken: nothing an evaluation read
        // may depend on how many other arrivals have already been retired (invariant
        // I-R2), which is why the forgetting is a second pass and not part of the map.
        let dsrc = self.sidelink.is_none();
        for &rx in state.arrivals.keys() {
            if dsrc {
                self.phy.forget_arrival(RxHandle { tx: tx_id, rx });
            }
            if let Some(live) = self.live_at_rx.get_mut(&rx) {
                live.retain(|f| *f != frame);
                if live.is_empty() {
                    self.live_at_rx.remove(&rx);
                }
            }
        }
        if let Some(handle) = state.tx_handle
            && dsrc
        {
            self.phy.end_tx(handle);
        }

        let tx_record = NodeTx::new(
            state.start,
            state.tx,
            frame_index,
            msg_type_name(state.msg_type),
            u64::from(psdu),
            state.air.as_nanos() / 1000,
            state.descriptor.tx_power_dbm,
            state.descriptor.channel.0,
            if state.full_certificate {
                SignerId::Certificate
            } else {
                SignerId::Digest
            },
            state.generation_time,
        )
        .with_sizes(state.payload_bytes, state.envelope_bytes)
        .with_journey(
            state.t_sign_start,
            state.ready_at,
            state.mac_aifs_ns,
            state.mac_backoff_ns,
        )
        .with_layers(&state.layers, state.cert_bytes)
        .with_content(Some(hex_digest(&state.signer.0[..])), state.content.clone());
        self.emit(recorder, &tx_record);
    }

    /// A Phase 2 application message reached a node that decoded it.
    ///
    /// The two messages the revocation path needs, and what the receiver does with each.
    /// It is the engine doing an application layer's job; see [`crate::phase2`] for why
    /// and for what that skips.
    #[allow(clippy::too_many_arguments)]
    fn on_app_message(
        &mut self,
        recorder: &mut dyn RunRecorder,
        rx: NodeId,
        tx: NodeId,
        app: &AppPayload,
        now: SimTime,
        horizon: SimTime,
        journey: ReportJourney,
    ) {
        match app {
            AppPayload::Report(report) => {
                // Only a unit with the `report-forward` role carries a report onward; a
                // vehicle that happens to overhear one does nothing with it, which is what
                // makes the role a decision rather than a label. The question is asked of
                // **this** unit: asking whether any declared unit carries the role made
                // the role a property of the scenario instead of the mast.
                let forwards = self
                    .phase2
                    .as_ref()
                    .is_some_and(|p| p.rsu_has_role(rx, "report-forward"));
                if !forwards {
                    return;
                }
                // And it is this unit's backhaul that is paid for, not the first one's.
                let latency = self
                    .phase2
                    .as_ref()
                    .and_then(|p| p.rsu_spec_of(rx).map(|s| s.backhaul))
                    .unwrap_or(Duration::ZERO);
                let at = latency.after(now);
                if at > horizon {
                    return;
                }
                let sdu = v2xw_core::ids::SduId::new(self.next_sdu);
                self.next_sdu += 1;
                self.backhaul.insert(sdu, (tx, report.clone(), rx, journey));
                // The report's octets cross the unit's backhaul: one transfer in the
                // backhaul bucket (invariant I-N1), sized as the report was on the air.
                let bytes = u64::from(crate::phase2::report_bytes());
                let transfer = NetBytes::new(
                    now,
                    BACKHAUL_ID_BASE + u64::from(sdu.index()),
                    ByteBucket::Backhaul,
                    bytes,
                    Some(rx),
                );
                self.emit(recorder, &transfer);
                // The report crosses the backhaul as a `NetDeliver` to the Misbehaviour
                // Authority's host, which is the roadside unit's own node id: the backend
                // has its own id space (`crate::phase2`) and the engine never mixes them,
                // so the delivery is addressed to the unit that forwards it.
                self.scheduler.schedule(
                    at,
                    EventClass::NetDeliver,
                    Event::NetDeliver { sdu, to: rx },
                );
            }
            AppPayload::Crl(entry) => {
                let Some(runtime) = self.nodes.get_mut(&rx) else {
                    return;
                };
                runtime.stores_mut().crl.add_linkage_entry((**entry).clone());
                // A node that finds one of its *own* certificates on the CRL stops
                // transmitting [CAMP-EE §2.2.10.2]; `CertStore::sweep` does that on the
                // node's next step, from the gate this has just written.
                let revoked_here: Vec<v2xw_msg::sec_types::HashedId8> = {
                    let stores = runtime.stores();
                    let mine = self
                        .phase2
                        .as_ref()
                        .map(|p| p.creds(rx).to_vec())
                        .unwrap_or_default();
                    stores
                        .certs
                        .credentials()
                        .iter()
                        .filter(|c| {
                            mine.iter().any(|k| {
                                k.i == c.i_period
                                    && k.j == c.j_index
                                    && stores
                                        .crl
                                        .store()
                                        .revokes_linkage_at_period(k.i, k.lv)
                            })
                        })
                        .map(|c| c.digest.clone())
                        .collect()
                };
                for digest in revoked_here {
                    runtime.stores_mut().crl.revoke_own(&digest);
                }
                if let Some(phase2) = self.phase2.as_mut() {
                    phase2.note_crl_installed();
                }
            }
        }
    }

    /// A report arrives at the backend over the backhaul.
    ///
    /// The backend then runs to quiescence on its own clock and reports the latency of the
    /// whole revocation; the engine schedules the roadside broadcast for `now + latency`,
    /// which is how the two clocks are kept apart (see [`crate::phase2`], joint 3).
    fn on_net_deliver(
        &mut self,
        recorder: &mut dyn RunRecorder,
        sdu: v2xw_core::ids::SduId,
        to: NodeId,
        horizon: SimTime,
    ) {
        let now = self.scheduler.now();
        let Some((reporter, report, rsu, journey)) = self.backhaul.remove(&sdu) else {
            return;
        };
        // The report's whole journey, vehicle to authority, as one decomposed trace on
        // `msg.latency`: the first hop is the V2I frame, the second the backhaul. The
        // backhaul was scheduled from the end of the frame at the unit, so a backhaul
        // shorter than the propagation delay (a few microseconds) is clamped rather than
        // made negative.
        let mut trace = v2xw_metrics::latency::TraceBuilder::new(
            "mbr",
            Some("mbr".to_string()),
            Some(journey.msg),
            journey.t_generated,
        );
        trace
            .endpoints(Some(reporter), Some(rsu))
            .to("sign_queue", journey.t_sign_start)
            .to("sign", journey.t_signed)
            .to("mac_access", journey.t_tx_start)
            .to("airtime", journey.t_tx_end)
            .to("propagation", journey.t_arrival)
            .hop(1)
            .to("backhaul", now.max(journey.t_arrival));
        let trace = trace.finish();
        self.emit(recorder, &trace);
        let Some(phase2) = self.phase2.as_mut() else {
            return;
        };
        let Some(revocation) = phase2.on_report_received(*report, reporter) else {
            return;
        };
        let at = revocation.latency.after(now);
        if at > horizon {
            // The revocation was issued and the run ends before the backend's own latency
            // would have put it on the air. Counted, because `crls_issued` without
            // `crl_broadcasts` otherwise looks exactly like a roadside path that is not
            // wired up, and those want opposite responses: a longer `time.duration_s`
            // against a defect in this file.
            if let Some(phase2) = self.phase2.as_mut() {
                phase2.note_crl_past_horizon();
            }
            return;
        }
        let _ = to;
        // `FlowTimer` is the class credential and backend protocol timers live at
        // (02-architecture.md §5.1). Flow 0 step 0 is this run's one revocation.
        self.scheduler
            .schedule(at, EventClass::FlowTimer, Event::FlowTimer { flow: 0, step: 0 });
    }

    /// The roadside puts the CRL on the air.
    fn on_flow_timer(&mut self, horizon: SimTime) {
        let now = self.scheduler.now();
        let Some((entry, bytes)) = self
            .phase2
            .as_ref()
            .and_then(|p| p.revocation().map(|r| (r.entry.clone(), r.bytes)))
        else {
            return;
        };
        // The units that carry the `crl` role, each asked about itself. The predicate
        // used to ignore the unit it was filtering and ask whether *any* declared unit
        // carried the role, so a scenario with two masts had the one without the role
        // broadcast as well — and the counterexample test that says a unit with no `crl`
        // role puts nothing on the air held only because the shipped scenario declares
        // exactly one unit.
        let broadcasters: Vec<NodeId> = self
            .phase2
            .as_ref()
            .map(|p| p.rsus_with_role("crl"))
            .unwrap_or_default();
        for rsu in broadcasters {
            let Some(signer) = self
                .nodes
                .get(&rsu)
                .and_then(|n| n.stores().certs.active().map(|c| c.digest.clone()))
            else {
                continue;
            };
            let believed = self
                .nodes
                .get(&rsu)
                .map_or(now, |n| n.clock().believed_time(now));
            let ready_at = self.signing_cost(rsu).after(believed);
            // Sized from `v2xw-proto`'s CRL wire table; see `run_detectors` for why this
            // frame carries no encoded octets.
            let tx = Transmission::sized(
                v2xw_msg::MsgType::Crl,
                bytes,
                signer,
                true,
                ready_at,
                believed,
            );
            self.hand_down_app(
                rsu,
                &tx,
                now,
                horizon,
                Some(AppPayload::Crl(Box::new(entry.clone()))),
            );
            if let Some(phase2) = self.phase2.as_mut() {
                phase2.note_crl_broadcast();
            }
        }
    }

    /// One link's received power and distance.
    ///
    /// Sequential by necessity: the shadowing process and the fading model are stateful
    /// per link. `rx_power = P_tx − total_loss + fading_gain`, summed with
    /// [`v2xw_core::math::sum_ordered`] so two builds cannot disagree about its last bit.
    ///
    /// The third value is whether a focus region puts this receiver under the high-tier
    /// PHY rule. With a focus region the stack is chosen per link by
    /// `v2xw_radio::FocusPlan::evaluate` (02-architecture.md §7.3): inside, the focus
    /// stack; inbound, the surrounding propagation with no fading draw; otherwise the
    /// surrounding stack.
    fn link_budget(&mut self, state: &FrameState, rx: NodeId, rx_pos: Vec3) -> (f64, f64, bool) {
        let now = self.scheduler.now();
        let link = LinkKey::new(state.tx, rx);
        let distance_m = state.tx_pos.distance(rx_pos);
        let freq_hz = self.carrier_hz();

        // The antennas, not the ground points: a vehicle's phase centre stands at its
        // class's default height above the road (1.5 m for a car, TR 36.885), and a
        // roadside unit's position already carries its mast height. The building test
        // below is 2.5-D and compares roof heights against these.
        let tx_end = self.endpoint(state.tx, state.tx_pos, now);
        let rx_end = self.endpoint(rx, rx_pos, now);

        // What obstructs the path (04-models.md §3.5): building footprints crossed
        // (Sommer 2011) and terrain knife edges (ITU-R P.526), each only when the scenario
        // turned it on and the world has it. A clear link costs nothing but the index
        // query.
        let los = self.obstacles.classify(&self.world, tx_end.pos, rx_end.pos);
        let evaluation = self
            .focus
            .as_ref()
            .map(|f| f.plan.evaluate(tx_end.pos, rx_end.pos));
        let high = evaluation.is_some_and(|e| e.phy_tier == Tier::High);

        let (loss, fade, obstacle_db) = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                propagation,
                fading,
                weather,
                obstacles,
                focus,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            use v2xw_radio::LinkPlacement;
            let (prop, fad, draw_fading): (
                &mut Box<dyn BoxedPropagation>,
                &mut Box<dyn BoxedFading>,
                bool,
            ) = match (focus.as_mut(), evaluation.map(|e| e.placement)) {
                (Some(f), Some(LinkPlacement::Inside)) => {
                    (&mut f.propagation, &mut f.fading, true)
                }
                (_, Some(LinkPlacement::Inbound)) => (propagation, fading, false),
                _ => (propagation, fading, true),
            };
            let loss = prop.loss_db(&mut ctx, &tx_end, &rx_end, freq_hz, &los, weather);
            let obstacle_db =
                obstacles.loss_db(&mut ctx, &tx_end, &rx_end, &los, freq_hz, loss.path_db);
            let fade = if draw_fading {
                fad.sample_db(&mut ctx, link, distance_m, now)
            } else {
                0.0
            };
            (loss, fade, obstacle_db)
        };

        let rssi_dbm = v2xw_core::math::sum_ordered([
            state.descriptor.tx_power_dbm,
            -loss.total_db,
            -obstacle_db,
            fade,
        ]);
        (rssi_dbm, distance_m, high)
    }

    /// The carrier the link budget is evaluated at, hertz.
    fn carrier_hz(&self) -> f64 {
        self.sidelink.as_ref().map_or(SAFETY_FREQ_HZ, |sl| sl.freq_hz)
    }

    /// One node's radio endpoint at an instant: its antenna position and class.
    fn endpoint(&self, node: NodeId, ground: Vec3, now: SimTime) -> RadioEndpoint {
        if node.index() >= jamming::JAMMER_ID_BASE {
            // A jammer's declared position is its antenna's.
            return RadioEndpoint::isotropic(node, ground, v2xw_radio::ActorClass::Car, now);
        }
        if self.rsus.contains_key(&node) {
            // A mast's position is its antenna's (`crate::phase2`'s mast height).
            return RadioEndpoint::isotropic(node, ground, v2xw_radio::ActorClass::Rsu, now);
        }
        let class = self
            .node_class
            .get(&node)
            .copied()
            .unwrap_or(v2xw_radio::ActorClass::Car);
        let pos = Vec3::new(ground.x, ground.y, ground.z + class.default_antenna_height_m());
        RadioEndpoint::isotropic(node, pos, class, now)
    }

    /// The metric phase: every provider flushes its window, and the samples are recorded
    /// on `metric.sample`.
    ///
    /// `Observe` is priority 9 (02-architecture.md §5.1), so a flush sees an instant in
    /// which everything else has already happened — which is the whole reason the class
    /// exists and the reason a metric is not computed inside the phase that produced its
    /// inputs.
    fn on_metric_flush(&mut self, recorder: &mut dyn RunRecorder, horizon: SimTime) {
        let now = self.scheduler.now();
        self.emit_mac_reports(recorder, now);
        let samples = self.providers.flush(now);
        for sample in samples {
            self.emit(recorder, &sample);
        }
        let next = self.metric_period.after(now);
        if next <= horizon {
            self.scheduler.schedule(
                next,
                EventClass::Observe,
                Event::Observe {
                    what: Observe::MetricFlush,
                },
            );
        }
    }

    /// Every node's MAC report for the window that closes at `now`, on `mac.cbr`: the
    /// busy ratio its MAC measured, its EDCA queue depth, and what it offered and was
    /// refused since the previous report. Only at the tiers that model a MAC.
    fn emit_mac_reports(&mut self, recorder: &mut dyn RunRecorder, now: SimTime) {
        let Some(mac) = self.mac.as_ref() else {
            return;
        };
        let span = self.metric_period.as_nanos();
        let mut reports: Vec<MacCbr> = Vec::with_capacity(self.nodes.len());
        for &node in self.nodes.keys() {
            let cbr = Mac::<EngineCtx<'_>>::cbr(mac, node, SAFETY_CHANNEL, now);
            let depth = mac.queue_len(node, SAFETY_CHANNEL, SAFETY_AC) as u64;
            let w = self.mac_window.remove(&node).unwrap_or_default();
            reports.push(MacCbr::report(
                now,
                node,
                SAFETY_CHANNEL.0,
                cbr,
                depth,
                w.drops,
                w.frames,
                w.bytes,
                w.airtime_us,
                span,
            ));
        }
        for r in reports {
            self.emit(recorder, &r);
        }
    }

    /// At the end of the run, every attempt still between the PHY and an application is
    /// recorded as in flight, so each `phy.rx` attempt has exactly one `node.rx` fate.
    fn resolve_in_flight(&mut self, recorder: &mut dyn RunRecorder, at: SimTime) {
        for (_, attempt) in core::mem::take(&mut self.rx_pending) {
            self.emit(recorder, &attempt.in_flight(at));
        }
    }

    /// Emits a record through a context, so the visibility rule applies to it.
    ///
    /// Stamped at the scheduler's instant, which is right for every phase whose records
    /// describe the instant they are dispatched at. Mobility is the exception, and it
    /// uses [`Engine::emit_at`].
    fn emit(&mut self, recorder: &mut dyn RunRecorder, record: &dyn v2xw_core::ctx::ErasedRecord) {
        let at = self.scheduler.now();
        self.emit_at(recorder, at, record);
    }

    /// [`Engine::emit`], stamping the record at the instant it describes.
    ///
    /// The correction the vertical-slice audit asked for: a ground-truth kinematics
    /// record's time is the time of the *state*, not the time of the phase that happened
    /// to publish it. The two differ by one mobility step, because a step dispatched at
    /// `t` produces the world at `t + dt`.
    fn emit_at(
        &mut self,
        recorder: &mut dyn RunRecorder,
        at: SimTime,
        record: &dyn v2xw_core::ctx::ErasedRecord,
    ) {
        let Engine {
            scheduler,
            rng,
            world,
            snapshot,
            provenance,
            params,
            providers,
            report,
            ..
        } = self;
        let mut tee = Tee {
            inner: recorder,
            providers,
        };
        let mut ctx = EngineCtx::new(
            scheduler, rng, world, snapshot, provenance, params, &mut tee,
        );
        ctx.stamp_records_at(Some(at));
        v2xw_core::ctx::Ctx::emit_erased(&mut ctx, record);
        let refused = ctx.refused();
        report.records_refused += refused;
        if refused == 0 {
            report.records += 1;
        }
    }
}

/// A recorder that also feeds the run's metric providers.
///
/// A metric provider consumes the *recorded* stream (03-interfaces.md §10), so the split
/// between "what is written" and "what is measured" would be a second source of truth if
/// the engine fed providers from anywhere but the record path. Everything a provider sees
/// is something a recording also contains, which is what makes a metric reproducible from
/// a replay.
struct Tee<'a> {
    inner: &'a mut dyn RunRecorder,
    providers: &'a mut v2xw_metrics::ProviderSet,
}

impl RunRecorder for Tee<'_> {
    fn write(&mut self, at: SimTime, record: &v2xw_core::ctx::OwnedRecord) {
        self.providers.on_event(record);
        self.inner.write(at, record);
    }

    /// Forwarded, not dropped. A wrapper that forwards `write` and silently swallows the
    /// binary stream is the defect [`RunRecorder::write_wire_frame`] documents; this is
    /// the in-crate wrapper, and it is the one an out-of-crate wrapper is modelled on.
    fn write_wire_frame(&mut self, frame: &v2xw_record::wire::Frame) {
        self.inner.write_wire_frame(frame);
    }

    fn refused(&self) -> u64 {
        self.inner.refused()
    }

    fn records_written(&self) -> Option<u64> {
        self.inner.records_written()
    }

    fn frames_written(&self) -> Option<u64> {
        self.inner.frames_written()
    }
}

/// A node's report on one received frame, joined to the attempt it settles.
///
/// The node's instants are on its own clock; each is moved onto the simulation's timeline
/// by its distance from the frame's arrival, whose true instant the attempt carries. That
/// keeps every duration the node measured exact whatever its clock offset.
fn resolve_rx(attempt: NodeRx, report: &RxReport, now: SimTime) -> NodeRx {
    let arrival = attempt.0.t_arrival.unwrap_or(now);
    let on_timeline = |x: SimTime| arrival.saturating_add(x.saturating_sub(report.arrived));
    let rx_done = on_timeline(report.parsed);
    let verify_start = report.verify_start.map(on_timeline);
    let verify_done = report.verify_done.map(on_timeline);
    let mut a = attempt;
    a.0.t_rx_done = Some(rx_done);
    a.0.t_verify_start = verify_start;
    a.0.t_verify_done = verify_done;
    let delivered_at = verify_done.unwrap_or(rx_done);
    match report.disposition {
        RxDisposition::Delivered(VerificationState::Verified) => {
            a.delivered(now, "verified", delivered_at)
        }
        RxDisposition::Delivered(VerificationState::Unverified) => {
            a.delivered(now, "unverified", delivered_at)
        }
        RxDisposition::Delivered(VerificationState::Invalid) => {
            a.lost(now, rx_cause::SIGNATURE_INVALID)
        }
        RxDisposition::Delivered(VerificationState::Revoked) => a.lost(now, rx_cause::REVOKED),
        RxDisposition::Dropped(cause) => a.lost(now, drop_cause_name(cause)),
        RxDisposition::NodeOff => a.lost(now, rx_cause::RECEIVER_OFF),
    }
}

/// The `node.rx` loss cause a node's drop counter maps to.
const fn drop_cause_name(cause: v2xw_node::DropCause) -> &'static str {
    match cause {
        v2xw_node::DropCause::RxOverflow => rx_cause::RX_OVERFLOW,
        v2xw_node::DropCause::VerifyOverflow => rx_cause::VERIFY_OVERFLOW,
        v2xw_node::DropCause::ReassemblyTimeout => rx_cause::REASSEMBLY_FAILED,
        // A policy's own drop, whichever counter it charged.
        v2xw_node::DropCause::VerifyPolicySkip
        | v2xw_node::DropCause::TxOverflow
        | v2xw_node::DropCause::CrlBacklog => rx_cause::VERIFY_POLICY_DROP,
    }
}

/// One receiver's evaluated outcome for one frame.
#[derive(Debug, Clone, Copy)]
struct LinkOutcome {
    rx: NodeId,
    rssi_dbm: f64,
    sinr_db: f64,
    distance_m: f64,
    received: bool,
    /// Exactly one loss cause when the frame did not decode, and `None` when it did
    /// (invariant I-R3).
    cause: Option<LossCause>,
}

/// The radio actor class a vehicle class transmits as: what sets its antenna height and
/// gain (04-models.md §3.7).
fn radio_class(class: VehicleClass) -> v2xw_radio::ActorClass {
    match class {
        VehicleClass::Truck | VehicleClass::Trailer | VehicleClass::Bus | VehicleClass::Coach => {
            v2xw_radio::ActorClass::Truck
        }
        VehicleClass::Delivery => v2xw_radio::ActorClass::Van,
        VehicleClass::Motorcycle | VehicleClass::Moped => v2xw_radio::ActorClass::Motorcycle,
        VehicleClass::Bicycle | VehicleClass::Scooter => v2xw_radio::ActorClass::Bicycle,
        VehicleClass::Pedestrian => v2xw_radio::ActorClass::Pedestrian,
        VehicleClass::Passenger | VehicleClass::Emergency => v2xw_radio::ActorClass::Car,
    }
}

/// The lower-case name a `node.tx` record carries for a message type.
/// Lower-case hex of an identifier's octets.
fn hex_digest(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// What a message said, for the followed-vehicle view and the recording.
///
/// A BSM's Part I is decoded from the payload octets the node actually encoded (a J2735
/// `MessageFrame`), so the fields are the ones a receiver reads, converted from their
/// least significant bits and left out when they carry J2735's "unavailable" value. Every
/// other type carries its temporary identifier only, which the node takes from the first
/// four octets of the pseudonym's digest for a CAM and a BSM alike
/// (`v2xw_node::ObuRuntime::encode_payload`). The claim is the kinematic claim the
/// receivers' detectors are handed (an attacker's is falsified).
fn message_content(
    msg_type: v2xw_msg::MsgType,
    payload: Option<&[u8]>,
    signer: &v2xw_msg::sec_types::HashedId8,
    claim: (Vec3, f64, f64),
) -> v2xw_metrics::channels::MsgContentView {
    use v2xw_msg::j2735::bsm;
    let mut c = v2xw_metrics::channels::MsgContentView {
        temp_id: Some(hex_digest(&signer.0[..4])),
        claimed_x_m: Some(claim.0.x),
        claimed_y_m: Some(claim.0.y),
        claimed_speed_mps: Some(claim.1),
        claimed_heading_rad: Some(claim.2),
        ..Default::default()
    };
    if msg_type == v2xw_msg::MsgType::Bsm
        && let Some(bytes) = payload
        && let Ok(m) = bsm::decode_message_frame(bytes)
    {
        let k = &m.core;
        c.msg_count = Some(k.msg_cnt);
        c.temp_id = Some(hex_digest(&k.id));
        c.sec_mark_ms = (k.sec_mark != bsm::D_SECOND_UNAVAILABLE).then_some(k.sec_mark);
        c.lat_deg = (k.lat != bsm::LATITUDE_UNAVAILABLE).then(|| f64::from(k.lat) * 1e-7);
        c.lon_deg = (k.lon != bsm::LONGITUDE_UNAVAILABLE).then(|| f64::from(k.lon) * 1e-7);
        c.elev_m = (k.elev != bsm::ELEVATION_UNKNOWN).then(|| f64::from(k.elev) * 0.1);
        c.speed_mps = (k.speed != bsm::SPEED_UNAVAILABLE).then(|| f64::from(k.speed) * 0.02);
        c.heading_deg =
            (k.heading != bsm::HEADING_UNAVAILABLE).then(|| f64::from(k.heading) * 0.0125);
        c.part_ii = Some(u8::try_from(m.part_ii.len()).unwrap_or(u8::MAX));
    }
    c
}

fn msg_type_name(t: v2xw_msg::MsgType) -> &'static str {
    match t {
        v2xw_msg::MsgType::Bsm => "bsm",
        v2xw_msg::MsgType::Cam => "cam",
        v2xw_msg::MsgType::Denm => "denm",
        v2xw_msg::MsgType::Spat => "spat",
        v2xw_msg::MsgType::Map => "map",
        v2xw_msg::MsgType::Psm => "psm",
        v2xw_msg::MsgType::Vam => "vam",
        v2xw_msg::MsgType::Cpm => "cpm",
        v2xw_msg::MsgType::Srm => "srm",
        v2xw_msg::MsgType::Ssm => "ssm",
        v2xw_msg::MsgType::Wsa => "wsa",
        v2xw_msg::MsgType::Crl => "crl",
        v2xw_msg::MsgType::Mbr => "mbr",
    }
}

/// Re-exported so a caller can size a grid or a range the same way the engine does.
pub const CANDIDATE_RANGE_M: f64 = MAX_RANGE_M;

/// The tier the radio stack runs at, for a caller's report.
pub fn radio_tier(scenario: &Scenario) -> Tier {
    scenario.radio.tiers.phy
}

/// A configured [`Engine`] with `NodeConfig` defaults exposed, for a caller building one
/// node outside a run.
pub fn default_node_config() -> NodeConfig {
    NodeConfig::default()
}
