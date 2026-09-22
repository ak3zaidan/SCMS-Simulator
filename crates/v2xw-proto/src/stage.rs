//! Flow identifiers, stage identifiers and the stage log.
//!
//! 05-protocols.md §8 fixes a stage vocabulary so that "revocation latency by stage" is
//! computed identically for every protocol: a run holds stamped stages, and the
//! decomposition is differences between them. This module is that vocabulary, plus the
//! stages the §3.2 provisioning and reporting sequences need, plus the log that records
//! them.
//!
//! **Where a stage is stamped.** An entity's stages are stamped at the instant it
//! *finishes* processing the step that produced them — the completion of its service
//! time, not the arrival of the message that triggered it. One rule, applied everywhere,
//! so a stage difference is always "work finished here, work finished there" and never a
//! mixture of arrivals and completions. Several stages from one step therefore share a
//! timestamp, which is why order is checked as non-decreasing rather than as strictly
//! increasing.
//!
//! **Channel.** Stage stamps ride `proto.revocation`, whose key fields 03-interfaces.md
//! §14 gives as `t, stage, id, size` — the shape of a stage stamp — and whose visibility
//! is `public`. The per-hop message records ride `proto.msg`.

use std::collections::BTreeMap;

use v2xw_core::ctx::{Record, Visibility};
use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, SimTime};

/// A named flow: one message-sequence state machine from 05-protocols §3.2 or §4.2.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FlowId {
    /// SCMS bootstrap: device → DCM → ECA → enrolment certificate.
    Enrolment,
    /// SCMS pseudonym provisioning by butterfly expansion.
    Provisioning,
    /// SCMS incremental batch top-up.
    Topup,
    /// Misbehaviour-report submission, device → LOP → RA → MA.
    Report,
    /// The Misbehaviour Authority's linkage resolution through PCA and both LAs.
    LinkageResolution,
    /// CRL assembly, signing and publication.
    CrlIssuance,
    /// CRL download, expansion and enforcement at a device.
    CrlDistribution,
    /// ETSI TS 102 941 enrolment.
    EtsiEnrolment,
    /// ETSI TS 102 941 authorization (standard variant).
    EtsiAuthorization,
    /// Distributed key generation for an interactive threshold protocol.
    ThresholdDkg,
    /// A t-of-n signing session.
    ThresholdSign,
    /// A proactive share refresh.
    ThresholdRefresh,
}

impl FlowId {
    /// The flow's stable name, as it appears on `proto.msg` and in a manifest.
    pub const fn as_str(self) -> &'static str {
        match self {
            FlowId::Enrolment => "enrolment",
            FlowId::Provisioning => "provisioning",
            FlowId::Topup => "topup",
            FlowId::Report => "report",
            FlowId::LinkageResolution => "linkage-resolution",
            FlowId::CrlIssuance => "crl-issuance",
            FlowId::CrlDistribution => "crl-distribution",
            FlowId::EtsiEnrolment => "etsi-enrolment",
            FlowId::EtsiAuthorization => "etsi-authorization",
            FlowId::ThresholdDkg => "threshold-dkg",
            FlowId::ThresholdSign => "threshold-sign",
            FlowId::ThresholdRefresh => "threshold-refresh",
        }
    }
}

impl core::fmt::Display for FlowId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One stage timestamp a flow emits.
///
/// The first block is 05-protocols §8's revocation decomposition, spelled exactly as the
/// table spells it. The second block is the provisioning, reporting and enrolment stages
/// the §3.2 sequences need; they are in the same enum because a latency decomposition is
/// the same computation whichever flow it is run over.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
// Snake case, not kebab: 05-protocols.md §8 spells these `report_sent` and
// `first_rsu_broadcast`, and that is the vocabulary "revocation latency by stage" is
// defined over. A recording that wrote `report-sent` would carry a second spelling of the
// same stage, which is how one query answers a question and another silently does not.
// `stage_names_serialise_as_the_design_set_spells_them` pins the two together.
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StageId {
    // --- 05-protocols §8, the revocation decomposition ---
    /// A local detector fired.
    Detect,
    /// The report left the device's outbox.
    ReportSent,
    /// The Misbehaviour Authority received the report, after the RA's shuffle.
    ReportReceived,
    /// The Misbehaviour Authority decided.
    Decision,
    /// The device behind the reports was identified (PCA and LA round trips).
    Resolved,
    /// The revocation artefact was signed (CRL Generator, or an EA blocklist entry).
    Issued,
    /// It reached the distribution point.
    Published,
    /// It was first broadcast by a roadside unit.
    FirstRsuBroadcast,
    /// A node downloaded it.
    Downloaded,
    /// A node finished expanding it (the per-entry, per-period hash and AES cost).
    Processed,
    /// A node is enforcing it.
    Enforced,
    /// A message from the revoked device was still accepted somewhere.
    ResidualHarm,
    /// The passive component: the enrolment certificate was blocklisted at the RA or EA.
    Blocklisted,
    /// The passive component: the device's last valid credential expired.
    LastValidCredentialExpiry,

    // --- 05-protocols §3.2 and §4.2, the issuance sequences ---
    /// A request left the end entity.
    Requested,
    /// A privacy proxy forwarded it with the network identifiers stripped.
    ProxyForwarded,
    /// The front end acknowledged it.
    Acknowledged,
    /// The Registration Authority expanded the caterpillar keys into cocoon keys.
    Expanded,
    /// The Linkage Authorities returned their pre-linkage values.
    PreLinkageReady,
    /// The batching window closed and the shuffled requests were released.
    Shuffled,
    /// The Pseudonym Certificate Authority certified a batch.
    Certified,
    /// The batch was stored in the device's repository and is downloadable.
    BatchReady,
    /// The end entity installed the credentials and can sign with them.
    Installed,
}

impl StageId {
    /// The stage's stable name, as 05-protocols §8 spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            StageId::Detect => "detect",
            StageId::ReportSent => "report_sent",
            StageId::ReportReceived => "report_received",
            StageId::Decision => "decision",
            StageId::Resolved => "resolved",
            StageId::Issued => "issued",
            StageId::Published => "published",
            StageId::FirstRsuBroadcast => "first_rsu_broadcast",
            StageId::Downloaded => "downloaded",
            StageId::Processed => "processed",
            StageId::Enforced => "enforced",
            StageId::ResidualHarm => "residual_harm",
            StageId::Blocklisted => "blocklisted",
            StageId::LastValidCredentialExpiry => "last_valid_credential_expiry",
            StageId::Requested => "requested",
            StageId::ProxyForwarded => "proxy_forwarded",
            StageId::Acknowledged => "acknowledged",
            StageId::Expanded => "expanded",
            StageId::PreLinkageReady => "pre_linkage_ready",
            StageId::Shuffled => "shuffled",
            StageId::Certified => "certified",
            StageId::BatchReady => "batch_ready",
            StageId::Installed => "installed",
        }
    }

    /// True for the stages 05-protocols §8's table names, which are the ones the
    /// revocation-latency metric is defined over.
    pub const fn is_revocation_decomposition(self) -> bool {
        matches!(
            self,
            StageId::Detect
                | StageId::ReportSent
                | StageId::ReportReceived
                | StageId::Decision
                | StageId::Resolved
                | StageId::Issued
                | StageId::Published
                | StageId::Downloaded
                | StageId::Processed
                | StageId::Enforced
                | StageId::ResidualHarm
        )
    }
}

impl core::fmt::Display for StageId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One run of one flow: the key a stage decomposition is grouped by.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct FlowRun(pub u32);

impl core::fmt::Display for FlowRun {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "run{}", self.0)
    }
}

/// One stamped stage — the record, and the row the decomposition is computed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StageStamp {
    /// When the stage was reached.
    pub t: SimTime,
    /// Which run of which flow.
    pub run: FlowRun,
    /// Which flow.
    pub flow: FlowId,
    /// Which stage.
    pub stage: StageId,
    /// The node the stage happened at, where the stage is per-node.
    pub node: Option<NodeId>,
    /// The artefact's size in bytes, where the stage has one (a CRL, a batch).
    pub size: Option<u32>,
}

impl Record for StageStamp {
    const CHANNEL: &'static str = "proto.revocation";
    const VISIBILITY: Visibility = Visibility::Public;
}

/// One hop of one flow, as `proto.msg` records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct WireStep {
    /// When the message was put on the link.
    pub t: SimTime,
    /// The sender.
    pub from: NodeId,
    /// The receiver.
    pub to: NodeId,
    /// Which flow.
    pub flow: FlowId,
    /// Which run of that flow, so a per-run byte count is a filter rather than a guess.
    pub run: FlowRun,
    /// Which step, by name.
    pub step: &'static str,
    /// Bytes on the wire.
    pub bytes: u32,
    /// Which transport carried it.
    pub transport: crate::net::Transport,
}

impl Record for WireStep {
    const CHANNEL: &'static str = "proto.msg";
    const VISIBILITY: Visibility = Visibility::Node;
}

/// Every stage every flow stamped, in stamping order.
///
/// A `Vec` rather than a map, because order is part of what is being asserted and because
/// the decomposition wants the stages of a run in the order they happened. Lookups group
/// by [`FlowRun`] with a [`BTreeMap`], never a `HashMap`, so any iteration that reaches a
/// report is ordered.
#[derive(Debug, Clone, Default)]
pub struct StageLog {
    stamps: Vec<StageStamp>,
}

impl StageLog {
    /// An empty log.
    pub fn new() -> StageLog {
        StageLog { stamps: Vec::new() }
    }

    /// Appends a stamp.
    pub fn push(&mut self, stamp: StageStamp) {
        self.stamps.push(stamp);
    }

    /// Every stamp, in stamping order.
    pub fn stamps(&self) -> &[StageStamp] {
        &self.stamps
    }

    /// The stamps of one run, in stamping order.
    pub fn run(&self, run: FlowRun) -> Vec<StageStamp> {
        self.stamps
            .iter()
            .copied()
            .filter(|s| s.run == run)
            .collect()
    }

    /// The stage ids of one run, in stamping order.
    pub fn stages(&self, run: FlowRun) -> Vec<StageId> {
        self.run(run).into_iter().map(|s| s.stage).collect()
    }

    /// The runs of one flow, in ascending run order.
    pub fn runs_of(&self, flow: FlowId) -> Vec<FlowRun> {
        let mut seen: BTreeMap<FlowRun, ()> = BTreeMap::new();
        for s in self.stamps.iter().filter(|s| s.flow == flow) {
            seen.insert(s.run, ());
        }
        seen.into_keys().collect()
    }

    /// True if one run's stamps are non-decreasing in time.
    ///
    /// Non-decreasing, not increasing: several stages produced by one processing step
    /// share that step's completion instant, which is the truth about them.
    pub fn is_ordered(&self, run: FlowRun) -> bool {
        self.run(run).windows(2).all(|w| w[0].t <= w[1].t)
    }

    /// The first time `stage` was stamped in `run`.
    pub fn at(&self, run: FlowRun, stage: StageId) -> Option<SimTime> {
        self.run(run)
            .into_iter()
            .find(|s| s.stage == stage)
            .map(|s| s.t)
    }

    /// The first time `stage` was stamped in `run` at `node`.
    pub fn at_node(&self, run: FlowRun, stage: StageId, node: NodeId) -> Option<SimTime> {
        self.run(run)
            .into_iter()
            .find(|s| s.stage == stage && s.node == Some(node))
            .map(|s| s.t)
    }

    /// The interval between two stages of one run.
    pub fn between(&self, run: FlowRun, from: StageId, to: StageId) -> Option<Duration> {
        let a = self.at(run, from)?;
        let b = self.at(run, to)?;
        (b >= a).then(|| Duration::between(a, b))
    }

    /// The whole decomposition of one run: `(stage, t)` in stamping order.
    pub fn decomposition(&self, run: FlowRun) -> Vec<(StageId, SimTime)> {
        self.run(run).into_iter().map(|s| (s.stage, s.t)).collect()
    }
}
