//! The security lifecycle and the backend: credentials, pseudonyms, misbehaviour
//! reporting, the Misbehaviour Authority's decision, revocation and CRL distribution.
//!
//! 10-roadmap.md's Phase 2 is a *path*, not a feature list: a vehicle detects another one
//! lying about its position, files a misbehaviour report, the report crosses the
//! vehicle's backend link to the Misbehaviour Authority, the authority decides on
//! sustained evidence, the PCA and both Linkage Authorities identify the device, the CRL
//! Generator issues on its cadence, the list is distributed by cellular download and by
//! roadside broadcast, and every vehicle that installs it stops trusting the liar. This
//! module wires that path across the crates that model each piece and **reimplements
//! none of them**:
//!
//! | Piece | Whose | What this module does with it |
//! |---|---|---|
//! | the attacker | [`v2xw_threat::LegacyAttacker`] | calls `act` on the outgoing claim before it is signed |
//! | the detector suite | [`v2xw_threat::Legacy12`] | feeds it the messages the node's own runtime delivered |
//! | the report | [`v2xw_threat::MisbehaviourReport`] | builds one `from_verdict`, at most once a second per subject per reporter |
//! | the access leg | [`crate::backend::BackendAccess`] | cellular Uu, roadside relay, or none |
//! | the backend | [`v2xw_proto::ScmsRun`] | run **in lockstep** with the engine's clock: RA shuffles, PCA and LA lookups, CRL cadence |
//! | the decision | [`v2xw_threat::LegacyWindow`] | `detection.ma`: k trusted reporters, distinct seconds, a span, a window |
//! | the CRL | [`v2xw_sec::linkage::CrlLinkageEntry`] | installs it in the receiving node's own `CrlGate` |
//! | enforcement | [`v2xw_node::stores::CrlGate`] | the node's own revocation check does the rest |
//!
//! # Why honest traffic used to be revoked
//!
//! The Phase 2 wiring took the authority's decision itself: whenever it held two reports
//! about two pseudonyms it asked the Linkage Authorities whether they belonged to one
//! device, and revoked if they did. The Linkage Authorities answer *linkage*, not
//! *guilt*: two honest receivers reporting one honest vehicle across a pseudonym change —
//! a Gauss–Markov GNSS burst of 3 s at six times the nominal noise is exactly that, and
//! the legacy engine says its burst length was chosen to be shorter than "the revocation
//! persistence gate" — is correctly linked, and was revoked. The persistence gate is the
//! legacy authority's `threat/ma/legacy-window` (k = 3 trusted reporters, reports in 4
//! distinct seconds spanning at least 3.0 s inside a 15 s window; `PipelineConfig`
//! `report_threshold_k`, `revoke_min_seconds`, `revoke_persist_s`, `revoke_window_s`), and
//! it ships in `v2xw-threat` — it was simply not on this path. It is now: the decision is
//! the pipeline's, and the backend only carries it out.
//!
//! # The joints, stated
//!
//! **1. The air credential is the SCMS credential's shape, with a stand-in key.** Each
//! credential carries the linkage value and the i-period the SCMS provisioning issued, and
//! the validity window of that period, so a CRL entry revokes exactly the certificates the
//! protocol says. The signing key and the certificate bytes are the node's own
//! (`ObuRuntime` issues a real certificate for each pseudonym on first use), because the
//! credential protocol's key material is not handed to the node (build decision D11).
//!
//! **2. The subject lookup.** A report names a certificate by its digest; the engine maps
//! digest → `(node, i, j)` from what it provisioned, standing in for the PCA's own table.
//! The PCA's *service time* for that lookup is charged by the backend.
//!
//! **3. Pre-run provisioning.** A vehicle on the road already holds its pool, so each
//! vehicle's first batches are provisioned *before* the run on a scratch kernel
//! ([`v2xw_proto::ScmsRun::preload`]) — real butterfly expansion, real LA and PCA state —
//! and the run's own backend clock starts clean. Top-ups during the run are real in-run
//! flows over the vehicle's access link.
//!
//! **4. The access leg of multi-message flows.** A report's upload and a CRL download are
//! carried packet by packet by the vehicle's own Uu model or relay. A top-up's exchange
//! (request, acknowledgement, batch polls) runs inside the backend kernel over a link set
//! to the access's nominal latency and capacity at the instant it starts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use v2xw_core::geom::Vec3;
use v2xw_core::ids::{ActorId, NodeId};
use v2xw_core::time::{Duration, SimTime};
use v2xw_proto::net::Transport;
use v2xw_proto::scms::run::{ScmsRun, crl_bytes};
use v2xw_proto::stage::{FlowRun, StageId};
use v2xw_proto::{RevocationLatency, ScmsParams};
use v2xw_sec::linkage::{CrlLinkageEntry, LinkageValue};
use v2xw_threat::{
    AttackKind, Attacker, AttackerView, DetectorParams, Emission, Evidence, HonestClaim,
    Legacy12, LegacyAttacker, LegacyAttackerParams, LegacyWindow, MaAction, MaParams,
    MisbehaviourReport, NoMap, ObservedKind, ObservedMessage, SelfBelief, StationType,
    ThreatCtx, VerificationState,
};
use v2xw_world::World;

use crate::backend::{AccessKind, Backhaul, BackendAccess};
use crate::error::{EngineError, Result};
use crate::scenario::Scenario;

/// The protocol id a scenario names in `actors.backend.protocol` or `security.protocol`.
pub const CAMP_SCMS: &str = v2xw_proto::CAMP_SCMS_ID;

/// The ETSI ITS PKI's protocol id. Recognised so it can be refused by name.
pub const ETSI_PKI: &str = "protocol/etsi/ts102941";

/// The detector suite's id, as `detection.local` names it.
pub const LEGACY_12: &str = "detect/legacy-12";

/// The authority pipeline's id, as `detection.ma` names it.
pub const MA_LEGACY_WINDOW: &str = v2xw_threat::ma::MODEL_ID;

/// How the SCMS's device numbering is kept clear of the engine's.
pub const SCMS_DEVICE_BASE: u32 = 1_000_000;

/// The mast height a roadside unit's antenna stands at above its ground position, metres.
pub const RSU_MAST_HEIGHT_M: f64 = 6.0;

/// The hardware profile a roadside unit runs on when the scenario names none.
pub const DEFAULT_RSU_PROFILE: &str = "rsu/commsignia-its-rs4";

/// How often a reporter may report one subject again, by default: the legacy engine
/// reported every step its detectors fired, at `PipelineConfig.dt` = 1.0 s.
pub const REPORT_INTERVAL_S: f64 = 1.0;

/// How often a cellular vehicle asks the CRL Store for the current list, by default.
///
/// **Uncited, `todo-calibrate`.** The SCMS design has the device fetch "on every RA
/// connection" [PRIMER p.7] and prints no interval; an OEM configures it. An hour is a
/// conservative polling interval chosen so a scenario states its own when revocation
/// latency is what it measures.
pub const CRL_FETCH_INTERVAL_S: f64 = 3_600.0;

/// How often a roadside unit with the `crl` role repeats the list it holds, by default.
///
/// **Uncited, `todo-calibrate`.** TS 102 941 Annex D.3 specifies the RSU broadcast of the
/// delta CTL but no repetition rate, and the SCMS design names the CRL broadcast path
/// without one. Five seconds keeps the broadcast a small share of the channel (one frame
/// per unit per five seconds) while reaching a vehicle that drives past.
pub const CRL_BROADCAST_INTERVAL_S: f64 = 5.0;

/// The SCMS device id of an engine node.
fn device_of(node: NodeId) -> NodeId {
    NodeId::new(SCMS_DEVICE_BASE + node.index())
}

/// The link-layer source address a pseudonym is sent under: six octets of its digest,
/// with the locally-administered bit set and the group bit clear (IEEE 802 §8.2), so the
/// address changes exactly when the certificate does.
#[must_use]
pub fn l2_address(digest: &[u8]) -> [u8; 6] {
    let mut a = [0u8; 6];
    for (k, b) in a.iter_mut().enumerate() {
        *b = digest.get(k + 2).copied().unwrap_or(0);
    }
    a[0] = (a[0] & 0xFC) | 0x02;
    a
}

/// One credential the SCMS provisioning issued, as the engine installs it on the air.
#[derive(Debug, Clone, Copy)]
pub struct ProvisionedCred {
    /// The i-period.
    pub i: u32,
    /// The index within the period.
    pub j: u32,
    /// The linkage value the certificate's `linkageData` carries.
    pub lv: LinkageValue,
    /// The start of the certificate's validity window.
    pub valid_from: SimTime,
    /// Its end.
    pub valid_until: SimTime,
}

/// One roadside unit the scenario declared.
#[derive(Debug, Clone)]
pub struct RsuSpec {
    /// Where it stands, world-local metres — the antenna phase centre.
    pub position: Vec3,
    /// What it does: `crl`, `report-forward`, `provisioning-proxy`, …
    pub roles: Vec<String>,
    /// Its hardware profile id.
    pub profile: String,
    /// Its backhaul.
    pub backhaul: Backhaul,
}

impl RsuSpec {
    /// Whether this unit carries a role.
    #[must_use]
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

/// The credential lifecycle's parameters (`security.protocol`).
#[derive(Debug, Clone, Copy)]
pub struct LifecycleParams {
    /// The backend deployment's parameters.
    pub scms: ScmsParams,
    /// Certificates per i-period (`certs_per_period`, 20 [CAMP-EE Table 2.1.2.6.2]).
    pub jmax: u32,
    /// i-periods a vehicle holds at the start (`pool_periods`).
    pub pool_periods: u32,
    /// A vehicle asks for the next period when fewer than this many periods, counting the
    /// current one, remain in its pool (`topup_below_periods`).
    pub topup_below_periods: u32,
    /// How often a cellular vehicle polls the CRL Store.
    pub crl_fetch_interval: Duration,
    /// How often a `crl` unit repeats the list.
    pub crl_broadcast_interval: Duration,
}

fn secs(v: f64) -> Duration {
    Duration::from_nanos((v * 1e9).round().max(0.0) as u64)
}

impl LifecycleParams {
    /// The parameters a scenario states, over the cited defaults.
    ///
    /// # Errors
    /// [`EngineError::Scenario`] for a key this build does not read or a value out of range.
    pub fn from_scenario(scenario: &Scenario) -> Result<LifecycleParams> {
        let mut scms = ScmsParams::default();
        scms.master_seed = scenario.seed;
        // A run publishes the CRL on the deployment's cadence: that wait is part of what
        // revocation costs (05-protocols §2.5), and `crl_cadence_s: 0` turns it off.
        scms.publish_on_cadence = true;
        let mut p = LifecycleParams {
            scms,
            jmax: scms.certs_per_period,
            pool_periods: 2,
            topup_below_periods: 1,
            crl_fetch_interval: secs(CRL_FETCH_INTERVAL_S),
            crl_broadcast_interval: secs(CRL_BROADCAST_INTERVAL_S),
        };
        let Some(choice) = scenario.security.protocol.as_ref() else {
            return Ok(p);
        };
        let params = &choice.params;
        let Some(map) = params.as_object() else {
            return Ok(p);
        };
        for (key, value) in map {
            let v = value.as_f64().ok_or_else(|| {
                conflict(
                    &format!("security.protocol.params.{key}"),
                    format!("must be a number, got {value}"),
                )
            })?;
            if !(v.is_finite() && v >= 0.0) {
                return Err(conflict(
                    &format!("security.protocol.params.{key}"),
                    format!("must be a finite number ≥ 0, got {v}"),
                ));
            }
            match key.as_str() {
                "i_period_s" => p.scms.i_period = secs(v.max(1.0)),
                "cert_lifetime_s" => p.scms.cert_lifetime = secs(v.max(1.0)),
                "certs_per_period" => {
                    p.jmax = (v.round() as u32).clamp(1, 60);
                    p.scms.certs_per_period = p.jmax;
                }
                "pool_periods" => p.pool_periods = (v.round() as u32).clamp(1, 64),
                "topup_below_periods" => p.topup_below_periods = (v.round() as u32).min(64),
                "cert_shuffle_window_s" => p.scms.shuffle_window = secs(v),
                "report_shuffle_window_s" => p.scms.report_shuffle_window = secs(v),
                "crl_cadence_s" => {
                    p.scms.crl_cadence = secs(v);
                    p.scms.publish_on_cadence = v > 0.0;
                }
                "crl_fetch_interval_s" => p.crl_fetch_interval = secs(v.max(0.1)),
                "crl_broadcast_interval_s" => p.crl_broadcast_interval = secs(v.max(0.1)),
                "first_batch_delay_s" => p.scms.first_batch_delay = secs(v),
                "download_poll_interval_s" => p.scms.download_poll_interval = secs(v.max(0.1)),
                other => {
                    return Err(conflict(
                        "security.protocol.params",
                        format!(
                            "'{other}' is not a lifecycle parameter; allowed: {}",
                            LIFECYCLE_KEYS.join(", ")
                        ),
                    ));
                }
            }
        }
        if p.scms.cert_lifetime < p.scms.i_period {
            return Err(conflict(
                "security.protocol.params.cert_lifetime_s",
                "is shorter than i_period_s, which would leave a gap between two periods' \
                 certificates in which a vehicle can sign with nothing (CAMP-EE §2.1.5.3.2 \
                 makes the lifetime an hour longer than the period)"
                    .to_string(),
            ));
        }
        Ok(p)
    }

    /// The i-period `t` falls in.
    #[must_use]
    pub fn period_at(&self, t: SimTime) -> u32 {
        let since = t.saturating_sub(self.scms.epoch);
        u32::try_from(since / self.scms.i_period.as_nanos().max(1)).unwrap_or(u32::MAX)
    }
}

/// The lifecycle keys `security.protocol.params` accepts.
pub const LIFECYCLE_KEYS: [&str; 12] = [
    "i_period_s",
    "cert_lifetime_s",
    "certs_per_period",
    "pool_periods",
    "topup_below_periods",
    "cert_shuffle_window_s",
    "report_shuffle_window_s",
    "crl_cadence_s",
    "crl_fetch_interval_s",
    "crl_broadcast_interval_s",
    "first_batch_delay_s",
    "download_poll_interval_s",
];

/// An armed attacker: the model, and the window it is allowed to act in.
struct AttackerSlot {
    model: Box<dyn Attacker>,
    id: String,
    from: SimTime,
    to: SimTime,
}

/// What the scenario asked for, before any node exists to be it.
#[derive(Debug, Clone)]
struct AttackerSpec {
    id: String,
    params: LegacyAttackerParams,
    count: Option<u32>,
    fraction: Option<f64>,
    actor_ids: BTreeSet<u32>,
    from: SimTime,
    to: SimTime,
}

/// The revocation, once the backend has issued it.
#[derive(Debug, Clone)]
pub struct Revocation {
    /// The CRL entry the CRL Generator issued.
    pub entry: CrlLinkageEntry,
    /// The decomposition 05-protocols §8 asks for, to the first vehicle that enforced.
    pub stages: Vec<(StageId, SimTime)>,
    /// Detection to first enforcement.
    pub latency: Duration,
    /// The CRL's size when this entry was published, bytes.
    pub bytes: u32,
    /// Which engine node was revoked.
    pub subject: NodeId,
    /// The certificate the authority decided on, hex.
    pub subject_digest: String,
}

/// A decision waiting for, or being carried out by, the backend.
#[derive(Debug, Clone)]
struct Case {
    subject: NodeId,
    subject_digest: String,
    /// The triggering report's backend run.
    report_run: FlowRun,
    /// The resolution and CRL-issuance runs, once started.
    runs: Option<(FlowRun, FlowRun)>,
    /// The entry, once issued, and the CRL version (entry count) it first appears in.
    entry: Option<(CrlLinkageEntry, u32)>,
    stamps_emitted: BTreeSet<(u8, SimTime)>,
    enforced_first: Option<SimTime>,
}

/// An ETSI decision carried out as the EA's blocklist.
#[derive(Debug, Clone)]
struct Block {
    subject: NodeId,
    subject_digest: String,
    run: FlowRun,
    detected: SimTime,
    decided: SimTime,
    blocked: Option<SimTime>,
}

/// A report on its way to the authority, keyed by its backend run.
#[derive(Debug, Clone)]
struct InFlightReport {
    report: MisbehaviourReport,
    subject: NodeId,
}

/// One vehicle's security and backend state.
#[derive(Debug, Clone, Default)]
struct NodeSec {
    access: Option<AccessKind>,
    /// Reports waiting for connectivity, with the instant each was detected.
    outbox: Vec<(MisbehaviourReport, SimTime)>,
    reports_uploaded: u32,
    crl_version: u32,
    /// The CRL entries this vehicle holds, so a list arriving in several frames, or by two
    /// paths, is installed once.
    crl_installed: Vec<CrlLinkageEntry>,
    next_crl_fetch: SimTime,
    crl_fetch_in_flight: bool,
    topup: Option<FlowRun>,
    /// ETSI: the ticket count at which the requested top-up is complete.
    etsi_target: Option<u32>,
    /// The last i-period this vehicle holds certificates for.
    last_period: u32,
    changes_seen: u32,
    active_digest: Option<[u8; 8]>,
    self_revoked: bool,
    starved: bool,
}

/// Counters the run report carries for this path.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Phase2Report {
    /// How many roadside units were created.
    pub rsus: u64,
    /// Received envelopes that would not parse at all.
    pub spdu_parse_failures: u64,
    /// Received envelopes that parsed but whose signature did not verify.
    pub spdu_signature_failures: u64,
    /// How many received messages landed in each node verification state.
    pub verification_states: std::collections::BTreeMap<String, u64>,
    /// How many times each detector fired, by detector id.
    pub verdicts_by_detector: std::collections::BTreeMap<String, u64>,
    /// How many nodes were armed as attackers.
    pub attackers: u64,
    /// How many outgoing claims an attacker falsified.
    pub falsified_claims: u64,
    /// How many messages the local detectors checked.
    pub messages_checked: u64,
    /// How many detector verdicts fired.
    pub verdicts_fired: u64,
    /// How many misbehaviour reports were filed.
    pub reports_sent: u64,
    /// Of those, uploaded over a cellular modem.
    pub reports_uploaded_cellular: u64,
    /// Of those, put on the air to a relaying roadside unit.
    pub reports_uploaded_relay: u64,
    /// Reports still in an outbox at the end of the run, for want of connectivity.
    pub reports_unsent: u64,
    /// Reports the access link lost.
    pub reports_lost: u64,
    /// How many reached the Location Obscurer Proxy.
    pub reports_received: u64,
    /// How many the authority ingested, after the RA's shuffle.
    pub reports_at_ma: u64,
    /// How many revocation decisions the authority's pipeline took.
    pub ma_revoke_decisions: u64,
    /// How many investigations the backend opened.
    pub cases_opened: u64,
    /// How many ended without an entry.
    pub cases_unresolved: u64,
    /// How many CRL entries the backend issued.
    pub crls_issued: u64,
    /// How many CRL versions the CRL Store published.
    pub crl_versions_published: u64,
    /// How many CRL frames roadside units put on the air.
    pub crl_broadcasts: u64,
    /// How many CRL downloads cellular vehicles completed.
    pub crl_downloads: u64,
    /// Issued revocations whose publication the run horizon cut off.
    pub crl_past_horizon: u64,
    /// How many node installations of a CRL entry.
    pub crls_installed: u64,
    /// How many receptions the installed CRL caused to be classified revoked.
    pub revoked_receptions: u64,
    /// Detection to the first vehicle enforcing, ns, for the first revocation.
    pub revocation_latency_ns: u64,
    /// That revocation's stages, `(stage, ns since detection)`.
    pub revocation_stages: Vec<(String, u64)>,
    /// Certificate top-ups started.
    pub topups_started: u64,
    /// Top-ups whose batch was installed.
    pub topups_completed: u64,
    /// Certificates installed by top-ups.
    pub certs_topped_up: u64,
    /// Pseudonym changes, all vehicles.
    pub pseudonym_changes: u64,
    /// Links the passive observer claimed across a pseudonym change.
    pub privacy_links_claimed: u64,
    /// Of those, links between two pseudonyms of one vehicle (the ground-truth join).
    pub privacy_links_correct: u64,
    /// Vehicles that at some instant held no usable certificate and could not sign.
    pub vehicles_starved: u64,
    /// Node steps a vehicle spent unable to sign for want of a certificate.
    pub starved_node_steps: u64,
    /// The access legs.
    pub access: crate::backend::AccessReport,
    /// Revocations whose subject was an armed attacker (ground truth).
    pub revoked_attackers: u64,
    /// Revocations whose subject was honest (ground truth): false revocations.
    pub revoked_honest: u64,
    /// Decisions about a certificate the published list already revoked.
    pub decisions_already_covered: u64,
    /// Backend deliveries the protocol kernel refused (modelling defects).
    pub backend_errors: u64,
    /// The first of them, for the report.
    pub first_backend_error: String,
}

/// What the engine does after a backend step: records to write and changes to apply.
#[derive(Debug, Default)]
pub struct BackendTick {
    /// Revocation stages to write on `proto.revocation`.
    pub stages: Vec<crate::sec_records::ProtoRevocation>,
    /// Reports the authority ingested.
    pub ma_reports: Vec<v2xw_threat::records::MaReportRecord>,
    /// Its decisions.
    pub ma_decisions: Vec<v2xw_threat::records::MaDecisionRecord>,
    /// A new CRL version was published at the store (its entry count).
    pub published: Option<u32>,
    /// Certificates a completed top-up installs, per node.
    pub installs: Vec<(NodeId, Vec<ProvisionedCred>, u64)>,
    /// Backend-network and backhaul bytes moved since the last tick:
    /// `(t, bucket, bytes, node)`. The sidelink's own bytes are not here: a relayed
    /// report's air hop is a frame on `node.tx` like any other.
    pub bytes: Vec<(SimTime, v2xw_metrics::channels::ByteBucket, u64, Option<NodeId>)>,
}

/// The Phase 2 state of one run.
pub struct Phase2 {
    scms: ScmsRun,
    /// The ETSI ITS PKI, when `security.protocol` selects it; `scms` then carries nothing.
    etsi: Option<v2xw_proto::etsi::ts102941::EtsiRun>,
    /// ETSI decisions, carried out as blocklistings.
    blocks: Vec<Block>,
    params: LifecycleParams,
    access: BackendAccess,
    creds: BTreeMap<NodeId, Vec<ProvisionedCred>>,
    by_digest: BTreeMap<[u8; 8], (NodeId, u32, u32)>,
    rsus: Vec<RsuSpec>,
    rsu_nodes: Vec<NodeId>,
    specs: Vec<AttackerSpec>,
    attackers: BTreeMap<NodeId, AttackerSlot>,
    detectors: BTreeMap<NodeId, Legacy12>,
    detector_params: DetectorParams,
    detection_on: bool,
    report_interval: Duration,
    ma: LegacyWindow,
    vehicles: u64,
    /// When each (reporter, subject) pair was last filed.
    filed: BTreeMap<(NodeId, String), SimTime>,
    /// When each receiver last ran its suite on each sender.
    checked_at: BTreeMap<(NodeId, [u8; 8]), SimTime>,
    forwarded: BTreeSet<String>,
    in_flight: BTreeMap<FlowRun, InFlightReport>,
    stage_cursor: usize,
    step_cursor: usize,
    queue: VecDeque<Case>,
    cases: Vec<Case>,
    published_version: u32,
    broadcast_version: u32,
    nodes: BTreeMap<NodeId, NodeSec>,
    revocation: Option<Revocation>,
    report: Phase2Report,
    /// The passive privacy observer: every safety frame on the air, linked across
    /// pseudonym changes by kinematics (07-threats-and-detection.md §6).
    observer: v2xw_threat::PrivacyObserver,
    /// Its link claims since the last backend step, for the recorder.
    link_claims: Vec<v2xw_threat::records::PrivacyLinkClaim>,
}

impl core::fmt::Debug for Phase2 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Phase2")
            .field("rsus", &self.rsus.len())
            .field("attackers", &self.attackers.len())
            .field("detectors", &self.detectors.len())
            .field("cases", &self.cases.len())
            .field("revoked", &self.revocation.is_some())
            .finish_non_exhaustive()
    }
}

/// The detector-suite parameters `detection.local[].params` may override, by name.
fn apply_detector_param(p: &mut DetectorParams, key: &str, v: f64) -> bool {
    match key {
        "consistency_threshold_m" => p.consistency_threshold_m = v,
        "heading_threshold_deg" => p.heading_threshold_deg = v,
        "detector_lag_s" => p.detector_lag_s = v,
        "z_threshold" => p.z_threshold = v,
        "min_consecutive" => p.min_consecutive = v.round().max(1.0) as u32,
        "sybil_min_certs" => p.sybil_min_certs = v.round().max(1.0) as u32,
        "sybil_cell_m" => p.sybil_cell_m = v,
        "art_max_m" => p.art_max_m = v,
        "max_accel_mps2" => p.max_accel_mps2 = v,
        "stale_max_s" => p.stale_max_s = v,
        "heading_min_speed_mps" => p.heading_min_speed_mps = v,
        "heading_min_disp_m" => p.heading_min_disp_m = v,
        _ => return false,
    }
    true
}

impl Phase2 {
    /// Builds the Phase 2 state a scenario declared, or `None` when it declared none.
    ///
    /// # Errors
    /// [`EngineError::Scenario`] when the scenario names a protocol, a detector, an
    /// attacker model, a pipeline, a link model or a world site this build does not have.
    #[allow(clippy::too_many_lines)]
    pub fn build(scenario: &Scenario, world: &World) -> Result<Option<Phase2>> {
        let protocol = scenario
            .actors
            .backend
            .protocol
            .clone()
            .or_else(|| scenario.security.protocol.as_ref().map(|c| c.id.clone()));
        let wants_backend = protocol.is_some();
        let wants_rsus = !scenario.actors.rsus.is_empty();
        let wants_threats = !scenario.threats.attackers.is_empty();
        let wants_detection = !scenario.detection.local.is_empty();
        if !(wants_backend || wants_rsus || wants_threats || wants_detection) {
            return Ok(None);
        }

        if let (Some(a), Some(b)) = (
            scenario.actors.backend.protocol.as_ref(),
            scenario.security.protocol.as_ref(),
        ) && *a != b.id
        {
            return Err(conflict(
                "security.protocol",
                format!(
                    "names {} but actors.backend.protocol names {a}; one deployment runs one \
                     credential protocol",
                    b.id
                ),
            ));
        }
        if let Some(protocol) = &protocol
            && protocol != CAMP_SCMS
            && protocol != ETSI_PKI
        {
            return Err(conflict(
                "security.protocol",
                format!(
                    "{protocol} is not a credential protocol this build ships; \
                     {CAMP_SCMS} or {ETSI_PKI}"
                ),
            ));
        }

        let mut detector_params = DetectorParams::default();
        let mut report_interval = secs(REPORT_INTERVAL_S);
        for choice in &scenario.detection.local {
            if choice.id != LEGACY_12 {
                return Err(conflict(
                    "detection.local",
                    format!(
                        "this build ships one local detector suite, {LEGACY_12}; got {}",
                        choice.id
                    ),
                ));
            }
            if let Some(map) = choice.params.as_object() {
                for (key, value) in map {
                    let v = value.as_f64().filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(
                        || {
                            conflict(
                                &format!("detection.local[].params.{key}"),
                                format!("must be a finite number ≥ 0, got {value}"),
                            )
                        },
                    )?;
                    if key == "report_interval_s" {
                        report_interval = secs(v);
                    } else if !apply_detector_param(&mut detector_params, key, v) {
                        return Err(conflict(
                            &format!("detection.local[].params.{key}"),
                            "is not a legacy-12 parameter this build reads".to_string(),
                        ));
                    }
                }
            }
        }

        let mut ma_params = MaParams::default();
        if let Some(choice) = &scenario.detection.ma {
            if choice.id != MA_LEGACY_WINDOW {
                return Err(conflict(
                    "detection.ma",
                    format!(
                        "this build ships one authority pipeline, {MA_LEGACY_WINDOW}; got {}",
                        choice.id
                    ),
                ));
            }
            if let Some(map) = choice.params.as_object() {
                for (key, value) in map {
                    let bad = || {
                        conflict(
                            &format!("detection.ma.params.{key}"),
                            format!("has an unusable value {value}"),
                        )
                    };
                    match key.as_str() {
                        "report_threshold_k" => {
                            ma_params.report_threshold_k =
                                value.as_u64().filter(|v| *v >= 1).ok_or_else(bad)? as usize;
                        }
                        "revoke_min_seconds" => {
                            ma_params.revoke_min_seconds =
                                value.as_u64().filter(|v| *v >= 1).ok_or_else(bad)? as usize;
                        }
                        "revoke_persist_s" => {
                            ma_params.revoke_persist_s = value.as_f64().ok_or_else(bad)?;
                        }
                        "revoke_window_s" => {
                            ma_params.revoke_window_s = value.as_f64().ok_or_else(bad)?;
                        }
                        "defence" => ma_params.defence = value.as_bool().ok_or_else(bad)?,
                        "reputation_max" => {
                            ma_params.reputation_max = value.as_u64().ok_or_else(bad)? as u32;
                        }
                        "report_budget" => {
                            ma_params.report_budget = value.as_u64().ok_or_else(bad)? as u32;
                        }
                        other => {
                            return Err(conflict(
                                "detection.ma.params",
                                format!("'{other}' is not a {MA_LEGACY_WINDOW} parameter"),
                            ));
                        }
                    }
                }
            }
        }

        let access = BackendAccess::from_scenario(scenario)?;

        let mut rsus = Vec::new();
        for spec in &scenario.actors.rsus {
            let position = match (spec.site, spec.position_m) {
                (Some(site), None) => {
                    let site = world.sites.get(site as usize).ok_or_else(|| {
                        conflict(
                            "actors.rsus[].site",
                            format!(
                                "site {site} does not exist: this world has {} \
                                 infrastructure site(s). The OSM importer produces none, so a \
                                 scenario on an imported city states `position_m` instead; \
                                 the procedural generator produces one site per junction \
                                 with `rsu_at_junctions: true`.",
                                world.sites.len()
                            ),
                        )
                    })?;
                    site.antenna_position()
                }
                (None, Some(p)) => Vec3::new(p[0], p[1], p[2] + RSU_MAST_HEIGHT_M),
                _ => {
                    return Err(conflict(
                        "actors.rsus[]",
                        "a roadside unit stands either at a world `site` or at an explicit \
                         `position_m`; give exactly one",
                    ));
                }
            };
            rsus.push(RsuSpec {
                position,
                roles: spec.roles.clone(),
                // The Commsignia ITS-RS4 by default: of the two RSU profiles that ship it is
                // the one whose datasheet publishes a verification rate (>2,000/s, R7 §C2).
                // The Cohda MK5 RSU brief publishes no compute figure at all, so a unit on
                // it could price no verification and dropped every frame it heard as a
                // verification-queue overflow — it received, but it never saw a message.
                profile: spec
                    .profile
                    .clone()
                    .unwrap_or_else(|| DEFAULT_RSU_PROFILE.to_string()),
                backhaul: access.backhaul_of(spec.backhaul.as_deref())?,
            });
        }

        let mut specs = Vec::new();
        for a in &scenario.threats.attackers {
            let kind =
                a.id.strip_prefix("threat/attacker/legacy/")
                    .and_then(AttackKind::parse)
                    .ok_or_else(|| {
                        conflict(
                            "threats.attackers[].id",
                            format!(
                                "{} is not an attacker this build ships: the legacy family is \
                                 `threat/attacker/legacy/<Kind>`, e.g. \
                                 threat/attacker/legacy/ConstPos",
                                a.id
                            ),
                        )
                    })?;
            let params = attacker_params(kind, &a.params)?;
            let horizon = (scenario.time.duration_s * 1e9).round().max(0.0) as u64;
            let (from, to) = match &a.schedule {
                Some(w) => (
                    (w.from_s * 1e9).round().max(0.0) as u64,
                    (w.to_s * 1e9).round().max(0.0) as u64,
                ),
                None => (0, horizon),
            };
            specs.push(AttackerSpec {
                id: a.id.clone(),
                params,
                count: a.count,
                fraction: a.fraction,
                actor_ids: a.actor_ids.iter().copied().collect(),
                from,
                to,
            });
        }

        let params = LifecycleParams::from_scenario(scenario)?;
        let mut scms_params = params.scms;
        apply_backend_net(scenario, &mut scms_params)?;
        let mut scms = ScmsRun::new_at(scms_params, 0).map_err(|e| {
            conflict(
                "actors.backend",
                format!("the SCMS deployment refused to start: {e}"),
            )
        })?;
        apply_backend_topology(scenario, &mut scms)?;
        let params = LifecycleParams {
            scms: scms_params,
            ..params
        };
        let etsi = if protocol.as_deref() == Some(ETSI_PKI) {
            let mut ep = v2xw_proto::etsi::ts102941::EtsiParams::default();
            ep.decide_on_report = false;
            ep.at_validity = params.scms.cert_lifetime;
            Some(v2xw_proto::etsi::ts102941::EtsiRun::new(ep).map_err(|e| {
                conflict(
                    "security.protocol",
                    format!("the ETSI deployment refused to start: {e}"),
                )
            })?)
        } else {
            None
        };
        Ok(Some(Phase2 {
            scms,
            etsi,
            blocks: Vec::new(),
            params,
            access,
            creds: BTreeMap::new(),
            by_digest: BTreeMap::new(),
            rsus,
            rsu_nodes: Vec::new(),
            specs,
            attackers: BTreeMap::new(),
            detectors: BTreeMap::new(),
            detector_params,
            detection_on: wants_detection,
            report_interval,
            ma: LegacyWindow::new(ma_params),
            vehicles: 0,
            filed: BTreeMap::new(),
            checked_at: BTreeMap::new(),
            forwarded: BTreeSet::new(),
            in_flight: BTreeMap::new(),
            stage_cursor: 0,
            step_cursor: 0,
            queue: VecDeque::new(),
            cases: Vec::new(),
            published_version: 0,
            broadcast_version: 0,
            nodes: BTreeMap::new(),
            revocation: None,
            report: Phase2Report::default(),
            observer: v2xw_threat::PrivacyObserver::cited_defaults(NodeId::new(u32::MAX)),
            link_claims: Vec::new(),
        }))
    }

    /// The lifecycle parameters in force.
    #[must_use]
    pub fn params(&self) -> &LifecycleParams {
        &self.params
    }

    /// The roadside units to create, in declaration order.
    pub fn rsu_specs(&self) -> &[RsuSpec] {
        &self.rsus
    }

    /// Records the node id the engine gave one roadside unit.
    pub fn note_rsu(&mut self, node: NodeId) {
        self.rsu_nodes.push(node);
        self.report.rsus += 1;
    }

    /// The roadside units' node ids.
    pub fn rsu_nodes(&self) -> &[NodeId] {
        &self.rsu_nodes
    }

    /// The unit `node` is, if it is one.
    #[must_use]
    pub fn rsu_spec_of(&self, node: NodeId) -> Option<&RsuSpec> {
        let index = self.rsu_nodes.iter().position(|n| *n == node)?;
        self.rsus.get(index)
    }

    /// Whether `node` is a roadside unit that carries `role`.
    ///
    /// A unit that declares no role at all carries every one of them.
    #[must_use]
    pub fn rsu_has_role(&self, node: NodeId, role: &str) -> bool {
        self.rsu_spec_of(node)
            .is_some_and(|s| s.has_role(role) || s.roles.is_empty())
    }

    /// The roadside units that carry `role`, in declaration order.
    #[must_use]
    pub fn rsus_with_role(&self, role: &str) -> Vec<NodeId> {
        self.rsu_nodes
            .iter()
            .copied()
            .filter(|n| self.rsu_has_role(*n, role))
            .collect()
    }

    /// The counters, mutably, for the engine to fold node-local totals into.
    pub fn report_mut(&mut self) -> &mut Phase2Report {
        &mut self.report
    }

    /// The counters this path accumulated during the run, with the access legs' own.
    pub fn report(&self) -> Phase2Report {
        let mut r = self.report.clone();
        r.access = self.access.report.clone();
        r.reports_unsent = self
            .nodes
            .values()
            .map(|n| n.outbox.len() as u64)
            .sum();
        r
    }

    /// The first revocation, once a vehicle has enforced it.
    pub fn revocation(&self) -> Option<&Revocation> {
        self.revocation.as_ref()
    }

    /// The access legs.
    pub fn access_mut(&mut self) -> &mut BackendAccess {
        &mut self.access
    }

    /// Whether a cellular modem has a serving cell at `pos`.
    #[must_use]
    pub fn uu_coverage(&self, pos: Vec3, t: SimTime) -> bool {
        self.access.uu_coverage(pos, t)
    }

    /// A vehicle's backend access.
    #[must_use]
    pub fn access_kind(&self, node: NodeId) -> AccessKind {
        self.nodes
            .get(&node)
            .and_then(|n| n.access)
            .unwrap_or(AccessKind::Offline)
    }

    /// Whether a relaying unit (one with `role`, and a connected backhaul) is within the
    /// relay radius of `pos`, and which.
    #[must_use]
    pub fn relay_in_range(&self, pos: Vec3, role: &str) -> Option<NodeId> {
        let range = self.access.relay_range_m();
        self.rsu_nodes
            .iter()
            .copied()
            .zip(self.rsus.iter())
            .find(|(n, s)| {
                s.backhaul.connected
                    && self.rsu_has_role(*n, role)
                    && s.position.distance(Vec3::new(pos.x, pos.y, s.position.z)) <= range
            })
            .map(|(n, _)| n)
    }

    /// Enrols and provisions one vehicle before the run and gives it its access.
    ///
    /// The pool is `pool_periods` i-periods of `certs_per_period` certificates starting
    /// at the period `now` falls in — what a vehicle on the road holds — provisioned by
    /// the real flows on a scratch kernel ([`ScmsRun::preload`], joint 3).
    pub fn provision(
        &mut self,
        node: NodeId,
        now: SimTime,
        rng: &v2xw_core::rng::RngRegistry,
    ) -> Vec<ProvisionedCred> {
        let relay = self.rsus.iter().any(|s| {
            s.backhaul.connected && (s.roles.is_empty() || s.has_role("provisioning-proxy") || s.has_role("report-forward"))
        });
        let kind = self.access.assign(rng, node, relay);
        let device = device_of(node);
        let start = self.params.period_at(now);
        if let Some(etsi) = self.etsi.as_mut() {
            // Authorization tickets: `certs_per_period` a week — the C2C-CC profile's 20
            // parallel tickets [TR 103 415 Table A.2] under the EU policy's cap of 100
            // [EUCP §7.2.1] — with no linkage value, because nothing ever revokes one
            // (TS 102 941 §6.1.4 NOTE 4).
            etsi.preload(device, self.params.jmax * self.params.pool_periods);
            let out: Vec<ProvisionedCred> = (start..start + self.params.pool_periods)
                .flat_map(|i| {
                    let (from, until) = self.params.scms.validity(i);
                    (0..self.params.jmax).map(move |j| ProvisionedCred {
                        i,
                        j,
                        lv: LinkageValue::new([0u8; 9]),
                        valid_from: from,
                        valid_until: until,
                    })
                })
                .collect();
            self.nodes.insert(
                node,
                NodeSec {
                    access: Some(kind),
                    last_period: start + self.params.pool_periods.saturating_sub(1),
                    next_crl_fetch: SimTime::MAX,
                    ..NodeSec::default()
                },
            );
            self.creds.insert(node, out.clone());
            return out;
        }
        if self
            .scms
            .preload(device, start, self.params.pool_periods, self.params.jmax)
            .is_err()
        {
            self.nodes.insert(
                node,
                NodeSec {
                    access: Some(kind),
                    ..NodeSec::default()
                },
            );
            return Vec::new();
        }
        let out = self.creds_of_device(device, 0);
        let last = start + self.params.pool_periods.saturating_sub(1);
        self.nodes.insert(
            node,
            NodeSec {
                access: Some(kind),
                last_period: last,
                next_crl_fetch: now,
                ..NodeSec::default()
            },
        );
        self.creds.insert(node, out.clone());
        out
    }

    /// The device's credentials at period `from_i` and later.
    fn creds_of_device(&self, device: NodeId, from_i: u32) -> Vec<ProvisionedCred> {
        let Some(dev) = self.scms.state.devices.get(&device) else {
            return Vec::new();
        };
        dev.credentials
            .iter()
            .filter(|((i, _), _)| *i >= from_i)
            .map(|(&(i, j), c)| {
                let (from, until) = self.params.scms.validity(i);
                ProvisionedCred {
                    i,
                    j,
                    lv: c.lv,
                    valid_from: from,
                    valid_until: until,
                }
            })
            .collect()
    }

    /// Forgets a retired vehicle.
    pub fn retire(&mut self, node: NodeId) {
        self.access.retire(node);
        if let Some(n) = self.nodes.remove(&node) {
            // Its unsent reports leave with it; they are counted as unsent here because
            // the vehicle took them out of the run.
            self.report.reports_lost += n.outbox.len() as u64;
        }
    }

    /// Registers the air digest one provisioned credential is carried under (joint 2).
    pub fn note_digest(
        &mut self,
        node: NodeId,
        digest: &v2xw_msg::sec_types::HashedId8,
        i: u32,
        j: u32,
    ) {
        self.by_digest.insert(digest_key(digest), (node, i, j));
    }

    /// Arms this node as an attacker if the scenario's selection rule names it.
    pub fn arm_attacker(&mut self, ctx: &mut dyn ThreatCtx, node: NodeId, actor: ActorId) -> bool {
        let index = self.vehicles;
        self.vehicles += 1;
        for spec in &self.specs {
            let selected = if !spec.actor_ids.is_empty() {
                spec.actor_ids.contains(&actor.index())
            } else if let Some(count) = spec.count {
                index < u64::from(count)
            } else if let Some(fraction) = spec.fraction {
                ctx.rng(
                    v2xw_core::rng::RngDomain::Attack,
                    v2xw_core::rng::EntityRef::Node(node),
                )
                .bool(fraction)
            } else {
                false
            };
            if !selected {
                continue;
            }
            let schedule = v2xw_threat::AttackSchedule {
                from: spec.from,
                to: spec.to,
                ..v2xw_threat::AttackSchedule::default()
            };
            self.attackers.insert(
                node,
                AttackerSlot {
                    model: Box::new(LegacyAttacker::new(
                        node,
                        spec.params.clone(),
                        v2xw_threat::Capabilities::insider(self.params.jmax),
                        schedule.clone(),
                        Vec::new(),
                    )),
                    id: spec.id.clone(),
                    from: spec.from,
                    to: spec.to,
                },
            );
            self.report.attackers += 1;
            return true;
        }
        if self.detection_on {
            self.detectors
                .insert(node, Legacy12::new(self.detector_params.clone()));
        }
        false
    }

    /// Gives a roadside unit the local detector suite, when the scenario runs one.
    ///
    /// A unit verifies every frame it hears (`verify-all`, `wiring::build_rsu`) and the
    /// legacy authority counts it as trusted infrastructure — "never rate-limited or
    /// distrusted" (run.py `trusted()`, `LegacyWindow::trust_infrastructure`) — so a unit
    /// is a reporter like a vehicle, whose reports go straight onto its backhaul.
    pub fn arm_rsu_detector(&mut self, node: NodeId) {
        if self.detection_on {
            self.detectors
                .insert(node, Legacy12::new(self.detector_params.clone()));
        }
    }

    /// Whether this node is an armed attacker.
    #[must_use]
    pub fn is_attacker(&self, node: NodeId) -> bool {
        self.attackers.contains_key(&node)
    }

    /// Lets an attacker edit the claim that is about to be signed.
    #[allow(clippy::too_many_arguments)]
    pub fn falsify(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        node: NodeId,
        actor: ActorId,
        believed_time: SimTime,
        signer: [u8; 8],
        honest: HonestClaim,
        belief: SelfBelief,
        cert: (SimTime, SimTime),
        msg: u64,
    ) -> Option<Emission> {
        let slot = self.attackers.get_mut(&node)?;
        if believed_time < slot.from || believed_time >= slot.to {
            return None;
        }
        let view = AttackerView {
            own_rx: &[],
            own_credentials: &[],
            crl_revocations_seen: None,
            own_belief: belief,
            honest,
            believed_time,
        };
        let mut out = Emission::honest(signer, honest, believed_time, cert.0, cert.1);
        let actions = slot.model.act(ctx, &view, &mut out);
        if actions.is_empty() {
            return Some(out);
        }
        let id = slot.id.clone();
        v2xw_threat::log_actions(ctx, believed_time, actor, &id, &actions, Some(msg));
        if v2xw_threat::is_falsified(&honest, &out, believed_time, StationType::Vehicle) {
            self.report.falsified_claims += 1;
        }
        Some(out)
    }

    /// Runs the local detector suite over what one node's runtime delivered, returning any
    /// report the node decided to file.
    ///
    /// A reporter files about one subject at most once per `report_interval` (the legacy
    /// engine filed on every 1 s step its detectors fired): the authority's persistence
    /// gate needs evidence in several distinct seconds, and one report per pair ever could
    /// not give it that, while one per message would flood the uplink.
    pub fn detect(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        node: NodeId,
        me: &SelfBelief,
        reporter_digest: Option<String>,
        delivered: &[v2xw_node::VerifiedMessage],
    ) -> Vec<MisbehaviourReport> {
        let Some(detector) = self.detectors.get_mut(&node) else {
            return Vec::new();
        };
        let interval = secs(self.detector_params.generation_interval_s);
        let mut out = Vec::new();
        for m in delivered {
            let Some(signer) = &m.signer else { continue };
            let key = digest_key(signer);
            // The suite checks one message per sender per `generation_interval_s` (1 s,
            // `PipelineConfig.dt`), the rate every one of its thresholds was calibrated at:
            // `min_consecutive` = 2 is two *seconds* of violation there, the one-step
            // heading baseline needs a 5 m displacement, and the 1.5 s motion lag is a
            // count of steps. Fed every 10 Hz BSM, "two consecutive" becomes 0.2 s and the
            // heading check runs only when noise moves a claim 5 m in 100 ms — which is how
            // honest vehicles were reported. A frozen or falsified claim is as visible at
            // one check a second as at ten.
            if let Some(last) = self.checked_at.get(&(node, key))
                && m.received_at < interval.after(*last).saturating_sub(interval.as_nanos() / 10)
            {
                continue;
            }
            self.checked_at.insert((node, key), m.received_at);
            let claimed = m.claimed_pos.unwrap_or(Vec3::ZERO);
            let observed = ObservedMessage {
                signer: key,
                kind: match m.msg_type {
                    v2xw_msg::MsgType::Denm => ObservedKind::Denm(String::new()),
                    _ => ObservedKind::Beacon,
                },
                received_at: m.received_at,
                claimed_generation_time: m.claimed_generation_time,
                claimed_x_m: claimed.x,
                claimed_y_m: claimed.y,
                claimed_speed_mps: m.claimed_speed_mps,
                claimed_heading_rad: m.claimed_heading_rad,
                claimed_pos_confidence_m: 5.0,
                repetitions: 1,
                cert_valid_from: 0,
                cert_valid_to: SimTime::MAX,
                station_type: StationType::Vehicle,
                verification: match m.verification {
                    v2xw_node::stores::VerificationState::Verified => VerificationState::Valid,
                    v2xw_node::stores::VerificationState::Invalid => {
                        VerificationState::BadSignature
                    }
                    v2xw_node::stores::VerificationState::Revoked => {
                        VerificationState::UnknownCertificate
                    }
                    _ => VerificationState::Unverified,
                },
            };
            let verdict = v2xw_threat::Detector::on_message(detector, ctx, me, &observed, &NoMap);
            self.report.messages_checked += 1;
            *self
                .report
                .verification_states
                .entry(format!("{:?}", m.verification))
                .or_insert(0) += 1;

            if !verdict.fired() {
                continue;
            }
            self.report.verdicts_fired += 1;
            for o in &verdict.fired {
                *self
                    .report
                    .verdicts_by_detector
                    .entry(o.detector.as_str().to_string())
                    .or_insert(0) += 1;
            }
            let pair = (node, verdict.subject.clone());
            if let Some(last) = self.filed.get(&pair)
                && m.received_at < self.report_interval.after(*last)
            {
                continue;
            }
            self.filed.insert(pair, m.received_at);
            let evidence = Evidence::at(m.received_at, m.received_at, 5.0);
            let id = format!("r-{}-{}-{}", node.index(), verdict.subject, m.received_at);
            let reporter = reporter_digest.clone().unwrap_or_else(|| {
                v2xw_core::hash::hex_encode(&me.node.index().to_le_bytes())
            });
            if self.rsu_nodes.contains(&node) {
                self.ma.trust_infrastructure(reporter.clone());
            }
            if let Some(report) =
                MisbehaviourReport::from_verdict(id, node, reporter, &verdict, &evidence)
            {
                self.report.reports_sent += 1;
                out.push(report);
            }
        }
        out
    }

    /// Resolves a report's subject to `(node, i, lv)` — the PCA's table (joint 2).
    fn resolve_subject(&self, digest_hex: &str) -> Option<(NodeId, u32, LinkageValue)> {
        let bytes = decode_hex8(digest_hex)?;
        let (node, i, j) = *self.by_digest.get(&bytes)?;
        let cred = self
            .creds
            .get(&node)?
            .iter()
            .find(|c| c.i == i && c.j == j)?;
        Some((node, cred.i, cred.lv))
    }

    /// Queues a report the vehicle could not upload yet.
    pub fn hold_report(&mut self, node: NodeId, report: MisbehaviourReport, detected_at: SimTime) {
        if let Some(n) = self.nodes.get_mut(&node) {
            n.outbox.push((report, detected_at));
        }
    }

    /// Takes a vehicle's held reports, to try again.
    pub fn take_outbox(&mut self, node: NodeId) -> Vec<(MisbehaviourReport, SimTime)> {
        self.nodes
            .get_mut(&node)
            .map(|n| core::mem::take(&mut n.outbox))
            .unwrap_or_default()
    }

    /// Vehicles with reports held.
    #[must_use]
    pub fn nodes_with_outbox(&self) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|(_, n)| !n.outbox.is_empty())
            .map(|(k, _)| *k)
            .collect()
    }

    /// Counts a report uploaded over `kind`.
    pub fn note_upload(&mut self, node: NodeId, kind: AccessKind) {
        match kind {
            AccessKind::Cellular => self.report.reports_uploaded_cellular += 1,
            AccessKind::RsuRelay => self.report.reports_uploaded_relay += 1,
            AccessKind::Offline => {}
        }
        if let Some(n) = self.nodes.get_mut(&node) {
            n.reports_uploaded += 1;
        }
    }

    /// Counts a report the access link lost.
    pub fn note_report_lost(&mut self) {
        self.report.reports_lost += 1;
    }

    /// A report reaches the Location Obscurer Proxy, its access leg paid.
    ///
    /// The backend takes it from here: the RA's report shuffle, the forward to the MA,
    /// the MA's own verification. The authority's *decision* happens when the MA has
    /// ingested it ([`Phase2::advance`]), not here.
    #[allow(clippy::too_many_arguments)]
    pub fn report_at_proxy(
        &mut self,
        report: MisbehaviourReport,
        reporter: NodeId,
        detected_at: SimTime,
        sent_at: SimTime,
        arrive_at: SimTime,
        transport: Transport,
    ) {
        self.report.reports_received += 1;
        let Some((subject, i, lv)) = self.resolve_subject(&report.subject_cert_digest) else {
            return;
        };
        if let Some(etsi) = self.etsi.as_mut() {
            // TS 103 759: the report goes to the MA, signed with the reporter's ticket and
            // encrypted to the authority — no RA shuffle in the ETSI system.
            let run = etsi.report_at_ma(
                device_of(reporter),
                device_of(subject),
                detected_at,
                sent_at,
                arrive_at,
                transport,
                report_bytes(),
            );
            self.in_flight
                .insert(run, InFlightReport { report, subject });
            return;
        }
        let run = self.scms.report_at_proxy(
            device_of(reporter),
            i,
            lv,
            detected_at,
            sent_at,
            arrive_at,
            transport,
            report_bytes(),
        );
        self.in_flight
            .insert(run, InFlightReport { report, subject });
    }

    /// Runs the backend to `now` and turns what happened into records and actions.
    #[allow(clippy::too_many_lines)]
    pub fn advance(&mut self, now: SimTime) -> BackendTick {
        if self.etsi.is_some() {
            return self.advance_etsi(now);
        }
        let mut tick = BackendTick::default();
        // Start any queued case the backend can take.
        self.start_cases(now);
        if let Err(e) = self.scms.run_until(now) {
            self.note_backend_error(&e);
        }
        // Stage stamps since the last tick.
        let stamps: Vec<v2xw_proto::stage::StageStamp> = {
            let (new, cursor) = self.scms.kernel.stages_since(self.stage_cursor);
            let v = new.to_vec();
            self.stage_cursor = cursor;
            v
        };
        for (subject, subject_digest, report_run, _t) in self.ingest_arrivals(&stamps, &mut tick) {
            self.queue.push_back(Case {
                subject,
                subject_digest,
                report_run,
                runs: None,
                entry: None,
                stamps_emitted: BTreeSet::new(),
                enforced_first: None,
            });
        }
        self.start_cases(now);
        if !self.queue.is_empty() || self.scms.case_open() {
            if let Err(e) = self.scms.run_until(now) {
                self.note_backend_error(&e);
            }
        }
        // Cases: note issuance, and emit the stages each has reached.
        let crlg_entries = self.scms.state.crlg.entries.clone();
        let log = self.scms.kernel.stages.clone();
        for case in &mut self.cases {
            let Some((resolution, issuance)) = case.runs else {
                continue;
            };
            if case.entry.is_none()
                && let Some(t) = log.at(issuance, StageId::Issued)
            {
                let _ = t;
                if let Some(entry) = crlg_entries.last() {
                    self.report.crls_issued += 1;
                    // The ground-truth join, for the run report only: whether the device
                    // the authority revoked was the one that lied.
                    if self.attackers.contains_key(&case.subject) {
                        self.report.revoked_attackers += 1;
                    } else {
                        self.report.revoked_honest += 1;
                    }
                    case.entry = Some((entry.clone(), crlg_entries.len() as u32));
                }
            }
            for (stage, run) in [
                (StageId::Detect, case.report_run),
                (StageId::ReportSent, case.report_run),
                (StageId::Shuffled, case.report_run),
                (StageId::ReportReceived, case.report_run),
                (StageId::Decision, resolution),
                (StageId::Resolved, resolution),
                (StageId::Blocklisted, resolution),
                (StageId::Issued, issuance),
            ] {
                if let Some(t) = log.at(run, stage)
                    && case.stamps_emitted.insert((stage as u8, t))
                {
                    tick.stages.push(crate::sec_records::ProtoRevocation::stage(
                        t,
                        stage.as_str(),
                        &case.subject_digest,
                        None,
                        None,
                        None,
                    ));
                }
            }
        }
        // Newly finished cases that issued nothing.
        let unresolved = self
            .cases
            .iter()
            .filter(|c| c.runs.is_some() && c.entry.is_none())
            .count() as u64;
        if !self.scms.case_open() {
            self.report.cases_unresolved = unresolved;
        }
        // Publication.
        let store = u32::try_from(self.scms.state.crl_store.entries.len()).unwrap_or(u32::MAX);
        if store > self.published_version {
            self.published_version = store;
            self.report.crl_versions_published += 1;
            tick.published = Some(store);
            let size = u64::from(crl_bytes(&self.scms.state.sizes, store).bytes());
            for s in &stamps {
                if s.stage == StageId::Published || s.stage == StageId::FirstRsuBroadcast {
                    for case in &mut self.cases {
                        if case
                            .entry
                            .as_ref()
                            .is_some_and(|(_, v)| *v <= store)
                            && case.stamps_emitted.insert((s.stage as u8, s.t))
                        {
                            tick.stages.push(crate::sec_records::ProtoRevocation::stage(
                                s.t,
                                s.stage.as_str(),
                                &case.subject_digest,
                                Some(size),
                                Some(u64::from(store)),
                                None,
                            ));
                        }
                    }
                }
            }
        }
        self.broadcast_version =
            u32::try_from(self.scms.state.crl_broadcast.entries.len()).unwrap_or(u32::MAX);
        // Top-ups whose batch has been installed.
        let mut finished = Vec::new();
        for (node, n) in &self.nodes {
            let Some(run) = n.topup else { continue };
            if log.at_node(run, StageId::Installed, device_of(*node)).is_some()
                || log.at(run, StageId::Installed).is_some()
            {
                finished.push((*node, run));
            }
        }
        for (node, run) in finished {
            let next_i = self.nodes.get(&node).map_or(0, |n| n.last_period + 1);
            let fresh = self.creds_of_device(device_of(node), next_i);
            let bytes = self.scms.kernel.steps.iter().filter(|s| s.run == run).map(|s| u64::from(s.bytes)).sum();
            if let Some(n) = self.nodes.get_mut(&node) {
                n.topup = None;
                if !fresh.is_empty() {
                    n.last_period = fresh.iter().map(|c| c.i).max().unwrap_or(n.last_period);
                }
            }
            if !fresh.is_empty() {
                self.report.topups_completed += 1;
                self.report.certs_topped_up += fresh.len() as u64;
                self.creds.entry(node).or_default().extend(fresh.iter().copied());
                tick.installs.push((node, fresh, bytes));
            }
        }
        // Bytes the backend moved.
        let (steps, cursor) = {
            let (new, cursor) = self.scms.kernel.steps_since(self.step_cursor);
            (new.to_vec(), cursor)
        };
        self.step_cursor = cursor;
        push_bytes(&mut tick, steps);
        tick
    }

    /// The reports the authority received in `stamps`, through the persistence gate, in the
    /// order their evidence was observed (a shuffle releases a batch at one instant, and
    /// the gate dates evidence by observation, not by arrival). Returns each decision:
    /// `(subject node, certificate digest, the triggering report's run, the instant)`.
    fn ingest_arrivals(
        &mut self,
        stamps: &[v2xw_proto::stage::StageStamp],
        tick: &mut BackendTick,
    ) -> Vec<(NodeId, String, FlowRun, SimTime)> {
        let mut arrived: Vec<(SimTime, FlowRun, InFlightReport)> = Vec::new();
        for s in stamps {
            if s.stage == StageId::ReportReceived
                && let Some(f) = self.in_flight.remove(&s.run)
            {
                arrived.push((s.t, s.run, f));
            }
        }
        arrived.sort_by(|a, b| {
            (a.2.report.detection_time, &a.2.report.report_id)
                .cmp(&(b.2.report.detection_time, &b.2.report.report_id))
        });
        let mut decisions = Vec::new();
        for (t, run, f) in arrived {
            self.report.reports_at_ma += 1;
            let mut r = f.report;
            r.ingest_time = t;
            tick.ma_reports.push(v2xw_threat::records::MaReportRecord {
                t,
                reporter: r.reporter,
                subject: r.subject_cert_digest.clone(),
                detector: r.leading_reason().map(str::to_string),
            });
            if let Some(MaAction::Revoke { subject }) = self.ma.ingest_evidence(&r, r.detection_time)
            {
                self.report.ma_revoke_decisions += 1;
                tick.ma_decisions.push(v2xw_threat::records::MaDecisionRecord {
                    t,
                    subject: subject.clone(),
                    decision: "revoke".to_string(),
                });
                decisions.push((f.subject, subject, run, t));
            }
        }
        decisions
    }

    /// The ETSI deployment's share of a step: reports through the gate, a decision
    /// carried out as the EA's blocklist (passive revocation, TS 102 941 §6.1.6), ticket
    /// top-ups, and the bytes it moved. There is no per-vehicle revocation list to publish:
    /// "revocation of authorization tickets is not possible as passive revocation is
    /// preferred" [TS 102 941 §6.1.4 NOTE 4], so a blocked vehicle keeps signing until its
    /// last ticket expires, and that instant is the revocation's last stage.
    fn advance_etsi(&mut self, now: SimTime) -> BackendTick {
        let mut tick = BackendTick::default();
        let Some(etsi) = self.etsi.as_mut() else {
            return tick;
        };
        if let Err(e) = etsi.run_until(now) {
            self.note_backend_error(&e);
        }
        let stamps: Vec<v2xw_proto::stage::StageStamp> = {
            let etsi = self.etsi.as_ref().expect("checked");
            let (new, cursor) = etsi.kernel.stages_since(self.stage_cursor);
            let v = new.to_vec();
            self.stage_cursor = cursor;
            v
        };
        for s in &stamps {
            if s.stage == StageId::Blocklisted
                && let Some(b) = self.blocks.iter_mut().find(|b| b.run == s.run && b.blocked.is_none())
            {
                b.blocked = Some(s.t);
                tick.stages.push(crate::sec_records::ProtoRevocation::stage(
                    s.t,
                    "blocklisted",
                    &b.subject_digest,
                    None,
                    None,
                    None,
                ));
                let last = self
                    .creds
                    .get(&b.subject)
                    .and_then(|c| c.iter().map(|c| c.valid_until).max())
                    .unwrap_or(s.t);
                tick.stages.push(crate::sec_records::ProtoRevocation::stage(
                    last,
                    "last_valid_credential_expiry",
                    &b.subject_digest,
                    None,
                    None,
                    Some(b.subject),
                ));
                if self.revocation.is_none() {
                    let stages = vec![
                        (StageId::Detect, b.detected),
                        (StageId::Decision, b.decided),
                        (StageId::Blocklisted, s.t),
                        (StageId::LastValidCredentialExpiry, last),
                    ];
                    let total = Duration::from_nanos(last.saturating_sub(b.detected));
                    self.report.revocation_latency_ns = total.as_nanos();
                    self.report.revocation_stages = stages
                        .iter()
                        .map(|(st, t)| (st.as_str().to_string(), t.saturating_sub(b.detected)))
                        .collect();
                }
            }
        }
        for (subject, subject_digest, run, t) in self.ingest_arrivals(&stamps, &mut tick) {
            if self.blocks.iter().any(|b| b.subject == subject) {
                self.report.decisions_already_covered += 1;
                continue;
            }
            let detected = self
                .etsi
                .as_ref()
                .and_then(|e| e.kernel.stages.at(run, StageId::Detect))
                .unwrap_or(t);
            let block_run = self
                .etsi
                .as_mut()
                .expect("checked")
                .decide_block(device_of(subject), t);
            self.report.cases_opened += 1;
            if self.attackers.contains_key(&subject) {
                self.report.revoked_attackers += 1;
            } else {
                self.report.revoked_honest += 1;
            }
            for (stage, at) in [("detect", detected), ("decision", t)] {
                tick.stages.push(crate::sec_records::ProtoRevocation::stage(
                    at,
                    stage,
                    &subject_digest,
                    None,
                    None,
                    None,
                ));
            }
            self.blocks.push(Block {
                subject,
                subject_digest,
                run: block_run,
                detected,
                decided: t,
                blocked: None,
            });
        }
        // Ticket top-ups: a request per ticket, and the batch installs when the AA has
        // granted them all.
        let mut finished = Vec::new();
        for (node, n) in &self.nodes {
            if let Some(target) = n.etsi_target
                && self
                    .etsi
                    .as_ref()
                    .is_some_and(|e| e.tickets_of(device_of(*node)) >= target)
            {
                finished.push(*node);
            }
        }
        for node in finished {
            let i = self.nodes.get(&node).map_or(0, |n| n.last_period + 1);
            let fresh: Vec<ProvisionedCred> = (0..self.params.jmax)
                .map(|j| {
                    let (from, until) = self.params.scms.validity(i);
                    ProvisionedCred {
                        i,
                        j,
                        lv: LinkageValue::new([0u8; 9]),
                        valid_from: from,
                        valid_until: until,
                    }
                })
                .collect();
            if let Some(n) = self.nodes.get_mut(&node) {
                n.etsi_target = None;
                n.topup = None;
                n.last_period = i;
            }
            self.report.topups_completed += 1;
            self.report.certs_topped_up += fresh.len() as u64;
            self.creds.entry(node).or_default().extend(fresh.iter().copied());
            tick.installs.push((node, fresh, 0));
        }
        let (steps, cursor) = {
            let etsi = self.etsi.as_ref().expect("checked");
            let (new, cursor) = etsi.kernel.steps_since(self.step_cursor);
            (new.to_vec(), cursor)
        };
        self.step_cursor = cursor;
        push_bytes(&mut tick, steps);
        tick
    }

    /// A backend delivery the kernel refused: a modelling defect (an unhosted entity, a
    /// missing link, a flow driven out of order), counted and kept so a run says so rather
    /// than going quiet.
    fn note_backend_error(&mut self, e: &v2xw_proto::ProtoError) {
        self.report.backend_errors += 1;
        if self.report.first_backend_error.is_empty() {
            self.report.first_backend_error = e.to_string();
        }
    }

    fn start_cases(&mut self, now: SimTime) {
        while !self.scms.case_open() {
            let Some(mut case) = self.queue.pop_front() else {
                return;
            };
            // The report the authority holds about the decided certificate: the latest
            // one, which is the one the decision came in on.
            let Some((_, i, lv)) = self.resolve_subject(&case.subject_digest) else {
                self.report.cases_unresolved += 1;
                continue;
            };
            let Some(index) = self
                .scms
                .state
                .ma
                .reports
                .iter()
                .rposition(|r| r.subject_lv == lv)
            else {
                continue;
            };
            let _ = now;
            // A certificate the Generator's list already revokes needs no second case: the
            // authority holds the list and checks it before spending lookups [BRECHT §VI-D:
            // one entry revokes every certificate of the device from `i` on].
            let mut covered = v2xw_sec::CrlStore::new();
            for e in &self.scms.state.crlg.entries {
                covered.add_linkage_entry(*e);
            }
            if covered.revokes_linkage_at_period(i, lv) {
                self.report.decisions_already_covered += 1;
                continue;
            }
            // Revoke from the reported certificate's own period forward: its entry must
            // match the certificate that was reported, and forward-only linkage keeps
            // every earlier certificate unlinkable [BRECHT §VI-D].
            if let Some(runs) = self.scms.revoke(index, i, self.params.jmax) {
                self.report.cases_opened += 1;
                case.runs = Some(runs);
                self.cases.push(case);
            } else {
                self.queue.push_front(case);
                return;
            }
        }
    }

    /// The list the CRL Store has published: its version (entry count) and entries.
    #[must_use]
    pub fn published_crl(&self) -> (u32, &[CrlLinkageEntry]) {
        (self.published_version, &self.scms.state.crl_store.entries)
    }

    /// The list the roadside broadcast path holds.
    #[must_use]
    pub fn broadcast_crl(&self) -> (u32, &[CrlLinkageEntry]) {
        (self.broadcast_version, &self.scms.state.crl_broadcast.entries)
    }

    /// The CRL's size on the wire for `entries` entries.
    #[must_use]
    pub fn crl_size(&self, entries: u32) -> u32 {
        crl_bytes(&self.scms.state.sizes, entries).bytes()
    }

    /// The size of a CRL request, bytes.
    #[must_use]
    pub fn crl_request_size(&self) -> u32 {
        self.scms.state.sizes.crl_request().bytes()
    }

    /// Vehicles whose CRL poll is due at `now`, with the version they hold. Advances each
    /// one's next poll.
    pub fn crl_polls_due(&mut self, now: SimTime) -> Vec<(NodeId, u32)> {
        let interval = self.params.crl_fetch_interval;
        let published = self.published_version;
        let mut out = Vec::new();
        for (node, n) in &mut self.nodes {
            if n.access != Some(AccessKind::Cellular) || n.crl_fetch_in_flight {
                continue;
            }
            if n.next_crl_fetch <= now {
                n.next_crl_fetch = interval.after(now);
                if n.crl_version < published {
                    n.crl_fetch_in_flight = true;
                    out.push((*node, n.crl_version));
                }
            }
        }
        out
    }

    /// Gives each vehicle's first poll a phase inside the interval, from a keyed draw, so
    /// a fleet does not poll in one instant.
    pub fn phase_crl_poll(&mut self, node: NodeId, rng: &v2xw_core::rng::RngRegistry, now: SimTime) {
        let interval = self.params.crl_fetch_interval.as_nanos();
        if let Some(n) = self.nodes.get_mut(&node) {
            let u = rng
                .checkout(
                    v2xw_core::rng::RngDomain::Backend,
                    v2xw_core::rng::EntityRef::Node(node),
                )
                .f64();
            n.next_crl_fetch = now.saturating_add((u * interval as f64) as u64);
        }
    }

    /// A CRL poll that ended without a download (lost, no coverage).
    pub fn crl_poll_failed(&mut self, node: NodeId) {
        if let Some(n) = self.nodes.get_mut(&node) {
            n.crl_fetch_in_flight = false;
        }
    }

    /// A vehicle received part or all of CRL `version`: `entries` are the entries the
    /// frame or download carried. Returns the entries new to this vehicle and the
    /// revocation stages its enforcement completes.
    ///
    /// A list may arrive split across several broadcast frames, or by both paths; an
    /// entry is installed once, and the vehicle holds `version` once it holds that many
    /// entries.
    pub fn install_crl(
        &mut self,
        node: NodeId,
        version: u32,
        entries: &[CrlLinkageEntry],
        now: SimTime,
        cellular: bool,
    ) -> (Vec<CrlLinkageEntry>, Vec<crate::sec_records::ProtoRevocation>) {
        let mut records = Vec::new();
        let Some(n) = self.nodes.get_mut(&node) else {
            return (Vec::new(), records);
        };
        if cellular {
            n.crl_fetch_in_flight = false;
            self.report.crl_downloads += 1;
        }
        let fresh: Vec<CrlLinkageEntry> = entries
            .iter()
            .filter(|e| !n.crl_installed.contains(e))
            .cloned()
            .collect();
        n.crl_installed.extend(fresh.iter().cloned());
        if n.crl_installed.len() >= version as usize {
            n.crl_version = n.crl_version.max(version);
        }
        let size = u64::from(crl_bytes(&self.scms.state.sizes, version).bytes());
        self.report.crls_installed += fresh.len() as u64;
        for case in &mut self.cases {
            let Some((entry, _)) = case.entry.as_ref() else {
                continue;
            };
            if !fresh.contains(entry) {
                continue;
            }
            for stage in ["downloaded", "processed", "enforced"] {
                records.push(crate::sec_records::ProtoRevocation::stage(
                    now,
                    stage,
                    &case.subject_digest,
                    (stage == "downloaded").then_some(size),
                    (stage == "downloaded").then_some(u64::from(version)),
                    Some(node),
                ));
            }
            if case.enforced_first.is_none() {
                case.enforced_first = Some(now);
            }
        }
        self.finish_first_revocation(now);
        (fresh, records)
    }

    /// Whether a report is being forwarded for the first time: two units that both hear
    /// one relayed report forward it once between them, as the RA's duplicate check
    /// would otherwise have to.
    pub fn claim_forward(&mut self, report_id: &str) -> bool {
        self.forwarded.insert(report_id.to_string())
    }

    /// How many CRL entries a vehicle holds.
    #[must_use]
    pub fn crl_entries_of(&self, node: NodeId) -> u32 {
        self.nodes
            .get(&node)
            .map_or(0, |n| n.crl_installed.len() as u32)
    }

    /// Assembles the first revocation's decomposition, once a vehicle enforces it.
    fn finish_first_revocation(&mut self, _now: SimTime) {
        if self.revocation.is_some() {
            return;
        }
        let Some(case) = self.cases.iter().find(|c| c.enforced_first.is_some()) else {
            return;
        };
        let (Some((resolution, issuance)), Some((entry, version)), Some(enforced)) =
            (case.runs, case.entry.clone(), case.enforced_first)
        else {
            return;
        };
        let log = &self.scms.kernel.stages;
        let latency = RevocationLatency::assemble(
            log,
            device_of(case.subject),
            Transport::CellularUu,
            case.report_run,
            resolution,
            issuance,
            FlowRun(u32::MAX),
        );
        let mut stages = latency.stages.clone();
        stages.push((StageId::Enforced, enforced));
        let first = stages.first().map_or(enforced, |s| s.1);
        let total = Duration::from_nanos(enforced.saturating_sub(first));
        self.report.revocation_latency_ns = total.as_nanos();
        self.report.revocation_stages = stages
            .iter()
            .map(|(s, t)| (s.as_str().to_string(), t.saturating_sub(first)))
            .collect();
        self.revocation = Some(Revocation {
            entry,
            stages,
            latency: total,
            bytes: crl_bytes(&self.scms.state.sizes, version).bytes(),
            subject: case.subject,
            subject_digest: case.subject_digest.clone(),
        });
    }

    /// Whether a CRL entry revokes any of `node`'s own certificates, and which `(i, j)`.
    #[must_use]
    pub fn own_revoked(&self, node: NodeId, gate: &v2xw_node::stores::CrlGate) -> Vec<(u32, u32)> {
        self.creds(node)
            .iter()
            .filter(|k| gate.store().revokes_linkage_at_period(k.i, k.lv))
            .map(|k| (k.i, k.j))
            .collect()
    }

    /// Marks a vehicle as having found itself on the CRL.
    pub fn note_self_revoked(&mut self, node: NodeId) {
        if let Some(n) = self.nodes.get_mut(&node) {
            n.self_revoked = true;
        }
    }

    /// Vehicles whose pool needs the next period, with the access to use. Marks each as
    /// having a top-up requested; the caller starts the flow with [`Phase2::start_topup`].
    pub fn topups_due(&mut self, now: SimTime) -> Vec<(NodeId, AccessKind, u32)> {
        let current = self.params.period_at(now);
        let below = self.params.topup_below_periods;
        let mut out = Vec::new();
        for (node, n) in &self.nodes {
            if n.topup.is_some() || n.self_revoked {
                continue;
            }
            let remaining = n.last_period.saturating_add(1).saturating_sub(current);
            if below > 0 && remaining <= below {
                out.push((*node, n.access.unwrap_or(AccessKind::Offline), n.last_period + 1));
            }
        }
        out
    }

    /// Starts one vehicle's top-up over `link`, for period `i`.
    pub fn start_topup(&mut self, node: NodeId, link: v2xw_proto::Link, i: u32, now: SimTime) {
        let device = device_of(node);
        if let Some(etsi) = self.etsi.as_mut() {
            // One standard authorization per ticket [TS 102 941 §6.2.3.3]; a blocklisted
            // station's requests are refused by the EA and its pool runs dry.
            etsi.set_access(device, link);
            let target = etsi.tickets_of(device) + self.params.jmax;
            let mut run = FlowRun(0);
            for _ in 0..self.params.jmax {
                run = etsi.authorize_at(device, now);
            }
            self.report.topups_started += 1;
            if let Some(n) = self.nodes.get_mut(&node) {
                n.topup = Some(run);
                n.etsi_target = Some(target);
            }
            let _ = i;
            return;
        }
        self.scms.set_access(device, link);
        let run = self.scms.topup_at(device, now, i, self.params.jmax);
        self.report.topups_started += 1;
        if let Some(n) = self.nodes.get_mut(&node) {
            n.topup = Some(run);
        }
    }

    /// Notes a pseudonym change the engine saw, returning the previous digest.
    pub fn note_change(&mut self, node: NodeId, changes: u32, digest: Option<[u8; 8]>) -> Option<Option<[u8; 8]>> {
        let n = self.nodes.get_mut(&node)?;
        if changes == n.changes_seen && digest == n.active_digest {
            return None;
        }
        let old = n.active_digest;
        let changed = changes > n.changes_seen;
        n.changes_seen = changes;
        n.active_digest = digest;
        if changed {
            self.report.pseudonym_changes += 1;
            Some(old)
        } else {
            None
        }
    }

    /// Counts a node step spent unable to sign.
    pub fn note_starved(&mut self, node: NodeId) {
        self.report.starved_node_steps += 1;
        if let Some(n) = self.nodes.get_mut(&node)
            && !n.starved
        {
            n.starved = true;
            self.report.vehicles_starved += 1;
        }
    }

    /// The security panel's row for one vehicle.
    #[must_use]
    pub fn security_view(
        &self,
        node: NodeId,
        t: SimTime,
        certs: &v2xw_node::CertStore,
        crl_entries: u32,
        link_up: bool,
    ) -> Option<crate::sec_records::NodeSecurityView> {
        let n = self.nodes.get(&node)?;
        let active = certs.active();
        let hex = |d: &[u8]| v2xw_core::hash::hex_encode(d);
        let valid = certs.credentials().iter().filter(|c| c.is_valid_at(t)).count();
        let preloaded = certs
            .credentials()
            .iter()
            .filter(|c| t < c.valid_from)
            .count();
        Some(crate::sec_records::NodeSecurityView {
            t,
            node,
            protocol: if self.etsi.is_some() { ETSI_PKI } else { CAMP_SCMS }.to_string(),
            pseudonym: active.map(|c| hex(&c.digest.0[..])),
            temp_id: active.map(|c| hex(&c.digest.0[..4])),
            cert_i: active.map(|c| c.i_period),
            cert_j: active.map(|c| c.j_index),
            cert_valid_until: active.map(|c| c.valid_until),
            pool_valid: valid as u32,
            pool_preloaded: preloaded as u32,
            pool_stored: certs.stored_count() as u32,
            pool_last_period: Some(n.last_period),
            changes: certs.changes(),
            topup_in_flight: n.topup.is_some(),
            link: n.access.unwrap_or(AccessKind::Offline).as_str().to_string(),
            link_up,
            outbox_reports: n.outbox.len() as u32,
            reports_uploaded: n.reports_uploaded,
            crl_entries,
            crl_version: n.crl_version,
            self_revoked: n.self_revoked,
        })
    }

    /// Notes that the roadside put a CRL frame on the air.
    pub fn note_crl_broadcast(&mut self) {
        self.report.crl_broadcasts += 1;
    }

    /// Notes a revocation whose publication fell past the run horizon.
    pub fn note_crl_past_horizon(&mut self) {
        self.report.crl_past_horizon += 1;
    }

    /// Notes that a node installed a CRL entry.
    pub fn note_crl_installed(&mut self) {
        self.report.crls_installed += 1;
    }

    /// Notes a reception the installed CRL caused to be classified revoked.
    pub fn note_revoked_reception(&mut self) {
        self.report.revoked_receptions += 1;
    }

    /// Settles the end of the run: an issued entry that no store published before the
    /// horizon is counted as cut off by it.
    pub fn finish(&mut self) {
        let issued_unpublished = self
            .cases
            .iter()
            .filter(|c| c.entry.as_ref().is_some_and(|(_, v)| *v > self.published_version))
            .count() as u64;
        self.report.crl_past_horizon = self.report.crl_past_horizon.max(issued_unpublished);
    }

    /// Feeds one safety frame, as it goes on the air, to the passive observer.
    ///
    /// The observer is a global eavesdropper: it hears every frame, which is the worst
    /// case for privacy and the upper bound a roadside receiver network approaches as its
    /// density grows. It reads only what is on the air — the signer digest, the claim and
    /// the claimed confidence — and whether a link it makes is right is judged downstream
    /// from the vehicles' own records, never here.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_frame(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        signer: [u8; 8],
        at: SimTime,
        generated: SimTime,
        pos: Vec3,
        speed_mps: f64,
        heading_rad: f64,
        confidence_m: f64,
    ) {
        let m = ObservedMessage {
            signer,
            kind: ObservedKind::Beacon,
            received_at: at,
            claimed_generation_time: generated,
            claimed_x_m: pos.x,
            claimed_y_m: pos.y,
            claimed_speed_mps: speed_mps,
            claimed_heading_rad: heading_rad,
            claimed_pos_confidence_m: confidence_m,
            repetitions: 1,
            cert_valid_from: 0,
            cert_valid_to: SimTime::MAX,
            station_type: StationType::Vehicle,
            verification: VerificationState::Valid,
        };
        let me = SelfBelief {
            node: NodeId::new(u32::MAX),
            believed_time: at,
            x_m: 0.0,
            y_m: 0.0,
            radio_range_m: f64::INFINITY,
        };
        if let Some(o) = self.observer.on_message(ctx, &me, &m) {
            self.link_claims.push(v2xw_threat::records::PrivacyLinkClaim {
                t: at,
                observer: NodeId::new(u32::MAX),
                predecessor: o.predecessor.clone().unwrap_or_default(),
                successor: o.successor.clone(),
                posterior: v2xw_core::math::quantize_to(o.posterior, 1e-6),
                candidates: o.anonymity_set_size.saturating_sub(1),
                anonymity_set_size: o.anonymity_set_size,
                effective_anonymity_set_bits: v2xw_core::math::quantize_to(
                    o.effective_anonymity_set_bits,
                    1e-6,
                ),
                degree_of_anonymity: v2xw_core::math::quantize_to(o.degree_of_anonymity, 1e-6),
                method: if o.predecessor.is_some() {
                    v2xw_threat::privacy::METHOD.to_string()
                } else {
                    v2xw_threat::privacy::METHOD_UNLINKED.to_string()
                },
            });
            if o.predecessor.is_some() {
                self.report.privacy_links_claimed += 1;
                let owner = |hex: &str| {
                    decode_hex8(hex).and_then(|d| self.by_digest.get(&d).map(|(n, ..)| *n))
                };
                if let (Some(a), Some(b)) =
                    (o.predecessor.as_deref().and_then(owner), owner(&o.successor))
                    && a == b
                {
                    self.report.privacy_links_correct += 1;
                }
            }
        }
    }

    /// The observer's link claims since the last call.
    pub fn take_link_claims(&mut self) -> Vec<v2xw_threat::records::PrivacyLinkClaim> {
        core::mem::take(&mut self.link_claims)
    }

    /// The credentials provisioned for a node.
    pub fn creds(&self, node: NodeId) -> &[ProvisionedCred] {
        self.creds.get(&node).map_or(&[], Vec::as_slice)
    }
}

/// The legacy attacker's parameters from a scenario's `threats.attackers[].params`.
///
/// Every key is read or refused — none is silently dropped: `intensity`, `dt_s`,
/// `magnitude_scale` (this attacker kind's multiplier), `dos_burst`, `delay_s`,
/// `expired_cert_lag_s`, `not_yet_valid_lead_s`, and each field of
/// [`v2xw_threat::Magnitudes`] by its own name.
pub fn attacker_params(
    kind: AttackKind,
    params: &serde_json::Value,
) -> Result<LegacyAttackerParams> {
    let mut p = LegacyAttackerParams::new(kind);
    if params.is_null() {
        return Ok(p);
    }
    let Some(map) = params.as_object() else {
        return Err(conflict(
            "threats.attackers[].params",
            "must be an object of named numbers".to_string(),
        ));
    };
    for (key, value) in map {
        let v = value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| {
            conflict(
                &format!("threats.attackers[].params.{key}"),
                format!("must be a finite number, got {value}"),
            )
        })?;
        let m = &mut p.magnitudes;
        let whole = |v: f64| v.round().max(0.0) as u64;
        match key.as_str() {
            "intensity" => p.intensity = v,
            "dt_s" => p.dt_s = v,
            "magnitude_scale" => {
                p.magnitude_scale.insert(kind, v);
            }
            "dos_burst" => p.dos_burst = whole(v) as u32,
            "delay_s" => p.delay_s = v,
            "expired_cert_lag_s" => p.expired_cert_lag_s = v,
            "not_yet_valid_lead_s" => p.not_yet_valid_lead_s = v,
            "const_pos_offset_m" => m.const_pos_offset_m = v,
            "random_pos_half_range_m" => m.random_pos_half_range_m = v,
            "teleport_dx_m" => m.teleport_dx_m = v,
            "teleport_dy_m" => m.teleport_dy_m = v,
            "teleport_period_s" => m.teleport_period_s = whole(v).max(1),
            "sine_pos_amplitude_m" => m.sine_pos_amplitude_m = v,
            "sine_pos_omega_rad_s" => m.sine_pos_omega_rad_s = v,
            "const_speed_offset_mps" => m.const_speed_offset_mps = v,
            "random_speed_max_mps" => m.random_speed_max_mps = v,
            "stop_and_go_speed_mps" => m.stop_and_go_speed_mps = v,
            "heading_offset_deg" => m.heading_offset_deg = v,
            "replay_lag_samples" => m.replay_lag_samples = whole(v) as usize,
            "slow_drift_ramp_mps" => m.slow_drift_ramp_mps = v,
            "slow_drift_max_rate_mps" => m.slow_drift_max_rate_mps = v,
            "along_road_offset_m" => m.along_road_offset_m = v,
            "disruptive_pos_half_range_m" => m.disruptive_pos_half_range_m = v,
            "disruptive_speed_up_mps" => m.disruptive_speed_up_mps = v,
            "disruptive_speed_down_mps" => m.disruptive_speed_down_mps = v,
            "disruptive_heading_deg" => m.disruptive_heading_deg = v,
            "pos_speed_inconsistent_drop_mps" => m.pos_speed_inconsistent_drop_mps = v,
            "pos_heading_swing_m" => m.pos_heading_swing_m = v,
            "pos_heading_omega_rad_s" => m.pos_heading_omega_rad_s = v,
            "eventual_stop_delay_s" => m.eventual_stop_delay_s = v,
            "eventual_stop_base_speed_mps" => m.eventual_stop_base_speed_mps = v,
            other => {
                return Err(conflict(
                    &format!("threats.attackers[].params.{other}"),
                    "is not a legacy attacker parameter this build reads".to_string(),
                ));
            }
        }
    }
    Ok(p)
}

/// `net.backend_net`: the links between backend entities.
fn apply_backend_net(scenario: &Scenario, p: &mut ScmsParams) -> Result<()> {
    let Some(choice) = &scenario.net.backend_net else {
        return Ok(());
    };
    if !crate::backend::BACKEND_NET_MODELS.contains(&choice.id.as_str()) {
        return Err(conflict(
            "net.backend_net",
            format!(
                "'{}' is not a backend network model; allowed: {}",
                choice.id,
                crate::backend::BACKEND_NET_MODELS.join(", ")
            ),
        ));
    }
    if let Some(ms) = choice.params.get("latency_ms").and_then(serde_json::Value::as_f64) {
        p.backend_link_latency = Duration::from_nanos((ms * 1e6).round().max(0.0) as u64);
    }
    if let Some(mbps) = choice
        .params
        .get("capacity_mbps")
        .and_then(serde_json::Value::as_f64)
    {
        p.backend_link_bandwidth_bps = (mbps * 1e6).round().max(1.0) as u64;
    }
    Ok(())
}

/// The SCMS roles a scenario may name in `actors.backend.entities` and `.links`.
pub const SCMS_ENTITIES: [&str; 11] = [
    "ra",
    "pca",
    "la1",
    "la2",
    "ma",
    "crlg",
    "lop",
    "crl_store",
    "crl_broadcast",
    "eca",
    "dcm",
];

/// The backend node an SCMS role name is hosted on.
fn scms_node(n: &v2xw_proto::ScmsNodes, name: &str) -> Option<NodeId> {
    Some(match name {
        "ra" => n.ra,
        "pca" => n.pca,
        "la1" => n.la1,
        "la2" => n.la2,
        "ma" => n.ma,
        "crlg" => n.crlg,
        "lop" => n.lop,
        "crl_store" => n.crl_store,
        "crl_broadcast" => n.crl_broadcast,
        "eca" => n.eca,
        "dcm" => n.dcm,
        _ => return None,
    })
}

/// `actors.backend.entities` and `actors.backend.links`: per-entity hardware profile and
/// service model, and per-link latency and capacity overrides.
fn apply_backend_topology(scenario: &Scenario, scms: &mut ScmsRun) -> Result<()> {
    let nodes = scms.state.nodes;
    let p = scms.state.params;
    // `nodes.backend_tier: abstract`: no backend entity ever queues. Sixty-four servers
    // and no fixed overhead leaves only the cryptography's own service time and the links,
    // which is the abstract tier's definition (06-node-models.md §4). The RA keeps its
    // shuffle: that is the protocol's batching, not a queue.
    if scenario.nodes.backend_tier == v2xw_core::card::Tier::Abstract {
        let free = v2xw_proto::ServiceModelSpec::new(64, Duration::ZERO);
        for node in nodes.all() {
            let spec = if node == nodes.ra {
                free.batched(v2xw_proto::BatchPolicy::CAMP_SHUFFLE)
            } else {
                free
            };
            scms.kernel.host(node, &spec, p.backend_profile);
        }
    }
    for (name, entity) in &scenario.actors.backend.entities {
        let node = scms_node(&nodes, name).ok_or_else(|| {
            conflict(
                "actors.backend.entities",
                format!(
                    "'{name}' is not a CAMP SCMS role; allowed: {}",
                    SCMS_ENTITIES.join(", ")
                ),
            )
        })?;
        let profile = match entity.profile.as_deref() {
            None => p.backend_profile,
            Some(id) => backend_profile(id).ok_or_else(|| {
                conflict(
                    &format!("actors.backend.entities.{name}.profile"),
                    format!("'{id}' is not a hardware profile the security crate prices"),
                )
            })?,
        };
        let servers = match entity.service_model.as_deref() {
            None | Some("service/mmc") => p.backend_servers,
            Some("service/mm1") => 1,
            Some(other) => {
                return Err(conflict(
                    &format!("actors.backend.entities.{name}.service_model"),
                    format!("'{other}' is not a service model; allowed: service/mmc, service/mm1"),
                ));
            }
        };
        let mut spec = v2xw_proto::ServiceModelSpec::new(servers, p.backend_overhead);
        if name == "ra" {
            spec = spec.batched(v2xw_proto::BatchPolicy::CAMP_SHUFFLE);
        }
        scms.kernel.host(node, &spec, profile);
    }
    for l in &scenario.actors.backend.links {
        let (Some(a), Some(b)) = (scms_node(&nodes, &l.from), scms_node(&nodes, &l.to)) else {
            return Err(conflict(
                "actors.backend.links",
                format!(
                    "'{}' → '{}': both ends must be CAMP SCMS roles ({})",
                    l.from,
                    l.to,
                    SCMS_ENTITIES.join(", ")
                ),
            ));
        };
        let base = scms.kernel.net().link(a, b).unwrap_or(v2xw_proto::Link {
            latency: p.backend_link_latency,
            bandwidth_bps: p.backend_link_bandwidth_bps,
            transport: Transport::BackendNet,
        });
        let link = v2xw_proto::Link {
            latency: l
                .latency_ms
                .map_or(base.latency, |ms| Duration::from_nanos((ms * 1e6).round() as u64)),
            bandwidth_bps: l
                .capacity_mbps
                .map_or(base.bandwidth_bps, |m| (m * 1e6).round().max(1.0) as u64),
            transport: Transport::BackendNet,
        };
        scms.kernel.net_mut().connect(a, b, link);
    }
    Ok(())
}

/// A backend hardware profile by id.
fn backend_profile(id: &str) -> Option<&'static str> {
    use v2xw_sec::primitive::profiles as p;
    [
        p::I9_11950H_WOLFSSL,
        p::COHDA_MK6_BOTAN,
        p::CORTEX_M4_NRF52840,
        p::CORTEX_M7_STM32F767,
    ]
    .into_iter()
    .find(|p| *p == id)
}

/// The backend's wire steps, in their byte buckets. The sidelink's own bytes are not here:
/// a relayed report's air hop is a frame on `node.tx` like any other.
fn push_bytes(tick: &mut BackendTick, steps: Vec<v2xw_proto::stage::WireStep>) {
    use v2xw_metrics::channels::ByteBucket;
    for s in steps {
        let from_device = s.from.index() >= SCMS_DEVICE_BASE;
        let node = [s.from, s.to]
            .into_iter()
            .find(|n| n.index() >= SCMS_DEVICE_BASE)
            .map(|n| NodeId::new(n.index() - SCMS_DEVICE_BASE));
        let bucket = match s.transport {
            Transport::BackendNet => ByteBucket::Backend,
            Transport::RsuBackhaul => ByteBucket::Backhaul,
            Transport::CellularUu if from_device => ByteBucket::CellularUl,
            Transport::CellularUu => ByteBucket::CellularDl,
            _ => continue,
        };
        tick.bytes.push((s.t, bucket, u64::from(s.bytes), node));
    }
}

/// The eight bytes of a certificate digest, as this module keys its maps by.
fn digest_key(digest: &v2xw_msg::sec_types::HashedId8) -> [u8; 8] {
    let mut key = [0u8; 8];
    key.copy_from_slice(&digest.0[..]);
    key
}

/// Eight bytes from a sixteen-character hex string.
fn decode_hex8(s: &str) -> Option<[u8; 8]> {
    if s.len() < 16 {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn conflict(field: &str, message: impl Into<String>) -> EngineError {
    EngineError::Scenario(crate::ScenarioError::conflict(field, message.into()))
}

/// The eight bytes of a certificate digest, for a caller outside this module.
#[must_use]
pub fn digest_bytes(digest: &v2xw_msg::sec_types::HashedId8) -> [u8; 8] {
    digest_key(digest)
}

/// How many bytes a misbehaviour report takes on the wire.
///
/// [`v2xw_proto`]'s own size model for a report submission, with its provenance:
/// 05-protocols marks the report's wire size as one of the five with no published value.
#[must_use]
pub fn report_bytes() -> u32 {
    ScmsParams::default().sizes.report_payload_bytes
}
