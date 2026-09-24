//! The records the security lifecycle and the backend put in a recording.
//!
//! Three of them are the design set's own channels with a reader already in
//! `v2xw-metrics` — `proto.revocation` (the revocation decomposition, PUBLIC), `sec.cert`
//! (credential lifecycle events) and `net.bytes` via [`crate::records::NetBytes`] — and are
//! written through the reader's view, so writer and reader cannot disagree about a field.
//! Two are new and have no reader outside this crate yet:
//!
//! * `sec.pseudonym` — one row per pseudonym change, naming every identifier that changed
//!   together (certificate digest, the BSM temporary ID or CAM station ID, and the
//!   link-layer source address) and what the pool looked like afterwards;
//! * `node.security` — one row per node per telemetry window: its certificate pool, its
//!   current pseudonym, its backend link and its revocation state. This is what a chase
//!   view's security panel reads.
//!
//! `privacy.link` is `v2xw-threat`'s own record, written by the passive observer.

use v2xw_core::ctx::{Record, Visibility};
use v2xw_core::ids::NodeId;
use v2xw_core::time::SimTime;
use v2xw_metrics::channels::{ProtoRevocationView, SecCertView};

/// `proto.revocation` — one stage of one revocation (05-protocols.md §8).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct ProtoRevocation(pub ProtoRevocationView);

impl Record for ProtoRevocation {
    const CHANNEL: &'static str = "proto.revocation";
    const VISIBILITY: Visibility = Visibility::Public;
}

impl ProtoRevocation {
    /// One stage.
    #[must_use]
    pub fn stage(
        t: SimTime,
        stage: &str,
        id: &str,
        size_bytes: Option<u64>,
        entries: Option<u64>,
        node: Option<NodeId>,
    ) -> Self {
        ProtoRevocation(ProtoRevocationView {
            t,
            stage: stage.to_string(),
            id: id.to_string(),
            size_bytes,
            entries,
            node,
        })
    }
}

/// `sec.cert` — a credential lifecycle event: `change`, `expire`, `top-up`, `revoked`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct SecCert(pub SecCertView);

impl Record for SecCert {
    const CHANNEL: &'static str = "sec.cert";
    const VISIBILITY: Visibility = Visibility::Node;
}

impl SecCert {
    /// One event.
    #[must_use]
    pub fn new(
        t: SimTime,
        node: NodeId,
        event: &str,
        digest: Option<String>,
        bytes: Option<u64>,
    ) -> Self {
        SecCert(SecCertView {
            t,
            node,
            event: event.to_string(),
            digest,
            bytes,
        })
    }
}

/// `sec.pseudonym` — one pseudonym change.
///
/// A change is only a change if **every** identifier a passive observer can read moves
/// together: the certificate (its `HashedId8`), the application-layer temporary identity
/// (the BSM `id`, SAE J2735 DE_TemporaryID, which SAE J2945/1 changes together with the
/// certificate; the CAM `stationID`, which ETSI TR 103 415 requires to change with the
/// authorization ticket), and the link-layer source address (the 802.11 MAC address, which
/// IEEE 1609.4 lets a device change for privacy, or the PC5 Layer-2 ID, which
/// 3GPP TS 23.285 changes with the application-layer ID). Leaving any one behind lets an
/// observer link the old pseudonym to the new one through it [TR 103 415 §5; Wiedersheim
/// et al. WONS 2010]. The record carries all three before and after, so a test can check
/// that none stayed behind. Clause numbers are not given here because the design set
/// does not pin them; the requirement itself is uncontroversial across the three.
///
/// The frame model carries no link-layer address field, so the address is modelled as a
/// function of the pseudonym ([`crate::phase2::l2_address`]): it changes exactly when the
/// certificate does, which is the property the record exists to show.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SecPseudonymView {
    /// The instant of the change.
    pub t: SimTime,
    /// The node.
    pub node: NodeId,
    /// Why: `age`, `distance`, `expired`, `revoked`, `startup`.
    pub reason: String,
    /// The previous certificate digest, hex.
    pub old_digest: Option<String>,
    /// The new certificate digest, hex.
    pub new_digest: Option<String>,
    /// The previous temporary ID (BSM `id` / CAM `stationID`), hex.
    pub old_temp_id: Option<String>,
    /// The new one.
    pub new_temp_id: Option<String>,
    /// The previous link-layer source address, hex.
    pub old_l2: Option<String>,
    /// The new one.
    pub new_l2: Option<String>,
    /// The new certificate's i-period and index.
    pub i: u32,
    /// Its `j`.
    pub j: u32,
    /// Certificates in the pool valid at `t`, after the change.
    pub pool_valid: u32,
    /// How many changes this node has made.
    pub changes: u32,
}

/// The `sec.pseudonym` record.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct SecPseudonym(pub SecPseudonymView);

impl Record for SecPseudonym {
    const CHANNEL: &'static str = "sec.pseudonym";
    const VISIBILITY: Visibility = Visibility::Node;
}

/// `node.security` — what a chase view's security panel shows for one node.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeSecurityView {
    /// The instant.
    pub t: SimTime,
    /// The node.
    pub node: NodeId,
    /// The credential protocol (`protocol/scms/camp`, `protocol/etsi/ts102941`).
    pub protocol: String,
    /// The current pseudonym's certificate digest, hex; `None` when the node holds no
    /// usable certificate and therefore cannot sign.
    pub pseudonym: Option<String>,
    /// Its temporary ID (BSM `id` / CAM `stationID`), hex.
    pub temp_id: Option<String>,
    /// Its i-period and index.
    pub cert_i: Option<u32>,
    /// Its `j`.
    pub cert_j: Option<u32>,
    /// When it stops being valid, ns.
    pub cert_valid_until: Option<SimTime>,
    /// Certificates valid now.
    pub pool_valid: u32,
    /// Certificates held for later periods.
    pub pool_preloaded: u32,
    /// Certificates stored at all.
    pub pool_stored: u32,
    /// The last i-period the pool reaches.
    pub pool_last_period: Option<u32>,
    /// Pseudonym changes so far.
    pub changes: u32,
    /// Whether a top-up is in flight.
    pub topup_in_flight: bool,
    /// The backend access: `cellular`, `rsu-relay`, `offline`.
    pub link: String,
    /// Whether that access is usable now (a serving cell, a relay in range).
    pub link_up: bool,
    /// Misbehaviour reports waiting in the outbox for connectivity.
    pub outbox_reports: u32,
    /// Reports this node has uploaded.
    pub reports_uploaded: u32,
    /// CRL entries installed.
    pub crl_entries: u32,
    /// The CRL version (entry count of the published list) this node last installed.
    pub crl_version: u32,
    /// Whether this node found itself on the CRL and stopped transmitting.
    pub self_revoked: bool,
}

/// The `node.security` record.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct NodeSecurity(pub NodeSecurityView);

impl Record for NodeSecurity {
    const CHANNEL: &'static str = "node.security";
    const VISIBILITY: Visibility = Visibility::Node;
}
