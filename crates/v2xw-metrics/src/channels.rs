//! Reader-side views of the event channels this crate consumes (03-interfaces.md §14).
//!
//! # Why this crate declares its own view types
//!
//! A `MetricProvider` is handed an `EventRecord` — `v2xw_core::ctx::OwnedRecord`, whose
//! payload is the record's JSON encoding — and never the emitting crate's concrete type.
//! That is deliberate: 08-measurement-and-data.md §1 says metrics "read the typed event
//! channels, never engine internals, so a metric provider written in Python sees exactly
//! what a Rust one sees". If this crate deserialised `v2xw_radio`'s own struct it would be
//! reading an internal, and a Python provider could not.
//!
//! So each channel gets a **view**: the subset of §14's key fields a metric actually needs,
//! with everything optional that a producer at a lower tier may not fill. `serde` ignores
//! unknown fields by default, so a producer is free to carry more than a view reads, and
//! adding a field to a channel does not break a provider that does not want it.
//!
//! # The two consequences, stated
//!
//! 1. A view is a *projection*, not the schema. Where §14 names a field in prose, the view
//!    names it in `snake_case` with its unit in the name (`rssi_dbm`, `cost_us`,
//!    `bytes_on_wire`). Where §14 leaves a field's spelling open, the view's spelling is
//!    this crate's proposal and the producing crate is the normative source once it lands.
//! 2. A field a view makes `Option` is one a metric must handle the absence of, and every
//!    metric in this crate does: a missing distance puts a reception in the unbinned
//!    bucket rather than in bin zero, a missing airtime is not counted as zero airtime.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use v2xw_core::ctx::{ChannelName, OwnedRecord, Visibility};
use v2xw_core::ids::{ActorId, NodeId};
use v2xw_core::time::SimTime;

use crate::error::{MetricError, Result};

/// The visibility tags 03-interfaces.md §14 declares for each channel.
///
/// A record's own tag is what the recorder acts on ([`Visibility::allowed_on_node_channel`]),
/// but §14 also fixes what a channel *may* carry, and a record that disagrees with its
/// channel is the leak invariant I-T2 exists to catch. Most channels allow exactly one tag;
/// `phy.rx` allows two, because §14 says its transmitter id is ground truth and
/// `Record::visibility`'s documentation says a `phy.rx` written without it is
/// [`Visibility::Node`].
///
/// `net.bytes` is this crate's reader-side projection of the `net.*` accounting records and
/// is listed as a NODE channel, which is what §14's `net.frag` row implies for the family.
const CHANNEL_VISIBILITY: &[(&str, &[Visibility])] = &[
    ("app.warning", &[Visibility::Node]),
    ("det.observation", &[Visibility::Node]),
    ("gt.attack.action", &[Visibility::Gt]),
    ("gt.despawn", &[Visibility::Gt]),
    ("gt.kinematics", &[Visibility::Gt]),
    ("gt.spawn", &[Visibility::Gt]),
    ("ma.case", &[Visibility::Node]),
    ("ma.decision", &[Visibility::Node]),
    ("ma.report", &[Visibility::Node]),
    ("mac.cbr", &[Visibility::Node]),
    ("manifest", &[Visibility::Meta]),
    (
        "metric.sample",
        &[
            // A metric inherits the visibility of the channel it was derived from
            // (08-measurement-and-data.md §1), so `metric.sample` carries whichever tag the
            // metric's own definition declares. The channel's default is `derived`.
            Visibility::Derived,
            Visibility::Gt,
            Visibility::Node,
            Visibility::NodeAndGt,
            Visibility::Public,
            Visibility::Meta,
        ],
    ),
    // `msg.latency` carries the stages of one message's journey; a trace that names its
    // endpoints' true node ids is ground-truth tainted, one that does not is the node's own.
    ("msg.latency", &[Visibility::Node, Visibility::NodeAndGt]),
    ("net.bytes", &[Visibility::Node]),
    ("net.frag", &[Visibility::Node]),
    // One fragmented SDU's fate at one receiver, with the loss its fragments' PHY success
    // probabilities predicted: the sender's identity and those probabilities are ground truth.
    ("net.reassembly", &[Visibility::Gt]),
    ("node.neighbor", &[Visibility::Node]),
    // Like `phy.rx`: the sender's identity and the distance are ground truth.
    ("node.rx", &[Visibility::Node, Visibility::NodeAndGt]),
    ("node.telemetry", &[Visibility::Node]),
    ("node.tx", &[Visibility::Node]),
    ("node.verify", &[Visibility::Node]),
    // The reception census: who was within range of a frame, which no node can know.
    ("phy.prr", &[Visibility::Gt]),
    ("phy.rx", &[Visibility::Node, Visibility::NodeAndGt]),
    ("proto.msg", &[Visibility::Node]),
    ("proto.revocation", &[Visibility::Public]),
    ("sec.cert", &[Visibility::Node]),
    ("snapshot.delta", &[Visibility::Mixed]),
    ("snapshot.keyframe", &[Visibility::Mixed]),
];

/// The visibility tags `channel` may carry, or `None` for a channel 03-interfaces.md §14
/// does not list.
///
/// An unlisted channel is not a violation of anything: a plug-in may invent a channel. It
/// is simply outside the table, and the invariant checks say so rather than guessing.
#[must_use]
pub fn allowed_visibilities(channel: &str) -> Option<&'static [Visibility]> {
    CHANNEL_VISIBILITY
        .iter()
        .find(|(name, _)| *name == channel)
        .map(|(_, v)| *v)
}

/// A reader-side view of one recording channel.
pub trait ChannelView: DeserializeOwned {
    /// The channel's stable id, as 03-interfaces.md §14 spells it.
    const CHANNEL: &'static str;

    /// The channel as a [`ChannelName`], for a `subscribe()` list.
    #[must_use]
    fn channel_name() -> ChannelName {
        ChannelName(Self::CHANNEL)
    }
}

/// Decodes a recorded event into the view of its channel.
///
/// # Errors
/// [`MetricError::ChannelMismatch`] if the record is on another channel, or
/// [`MetricError::Decode`] if its JSON does not fit the view.
pub fn decode<V: ChannelView>(rec: &OwnedRecord) -> Result<V> {
    if rec.channel != V::CHANNEL {
        return Err(MetricError::ChannelMismatch {
            expected: V::CHANNEL,
            got: rec.channel.to_string(),
        });
    }
    serde_json::from_slice(&rec.json).map_err(|source| MetricError::Decode {
        channel: rec.channel.to_string(),
        source,
    })
}

/// The outcome of one reception attempt (`phy.rx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RxOutcome {
    /// The frame was received.
    Ok,
    /// The frame was not received; the cause says why.
    Lost,
}

/// Which accounting bucket a byte belongs to (invariant I-N1, 03-interfaces.md §5).
///
/// The list is closed and is exactly the invariant's: "air / cellular UL / cellular DL /
/// backhaul / backend". A byte that fits none of them is not attributable, which is itself
/// an I-N1 violation rather than a sixth bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ByteBucket {
    /// On the 5.9 GHz air interface.
    Air,
    /// Cellular uplink (Uu).
    CellularUl,
    /// Cellular downlink (Uu).
    CellularDl,
    /// An RSU's backhaul link.
    Backhaul,
    /// Between backend entities.
    Backend,
}

impl ByteBucket {
    /// Every bucket, in a fixed order — the order a per-bucket table is written in.
    pub const ALL: [ByteBucket; 5] = [
        ByteBucket::Air,
        ByteBucket::CellularUl,
        ByteBucket::CellularDl,
        ByteBucket::Backhaul,
        ByteBucket::Backend,
    ];

    /// The bucket's name, as it appears in a `bucket` dimension value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ByteBucket::Air => "air",
            ByteBucket::CellularUl => "cellular-ul",
            ByteBucket::CellularDl => "cellular-dl",
            ByteBucket::Backhaul => "backhaul",
            ByteBucket::Backend => "backend",
        }
    }

    /// The `bytes_*` metric name 08-measurement-and-data.md §2.1 gives this bucket.
    #[must_use]
    pub const fn metric_name(self) -> &'static str {
        match self {
            ByteBucket::Air => "bytes_air",
            ByteBucket::CellularUl => "bytes_uu_ul",
            ByteBucket::CellularDl => "bytes_uu_dl",
            ByteBucket::Backhaul => "bytes_backhaul",
            ByteBucket::Backend => "bytes_backend",
        }
    }
}

impl core::fmt::Display for ByteBucket {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which signer identifier a transmission carried (04-models.md §9.5,
/// `signer_id_policy` in the scenario schema).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignerId {
    /// The full certificate.
    Certificate,
    /// The certificate's digest only.
    Digest,
    /// Self-signed, or no signer identifier at all.
    SelfSigned,
}

/// The outcome of a verification task (`node.verify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyOutcome {
    /// The signature verified.
    Valid,
    /// The signature did not verify.
    Invalid,
    /// The task was dropped by policy or by queue overflow before it ran.
    Dropped,
    /// The message was delivered without verification (an on-demand or prioritised policy).
    Skipped,
}

/// The outcome of a reassembly (`net.frag`), as `v2xw_net::frag::ReassemblyOutcome::label`
/// spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FragOutcome {
    /// Every fragment arrived and the SDU was reassembled.
    Complete,
    /// Reassembly failed: a fragment was lost.
    Failed,
    /// The reassembly timer expired.
    Expired,
    /// Still waiting for fragments at the time of the record.
    Pending,
    /// A fragment of an SDU this receiver had already delivered.
    Duplicate,
}

/// `node.tx` — what a node transmitted (NODE).
///
/// 03-interfaces.md §14: "t, node, msg type, bytes, mcs, power, channel, ac, dcc state,
/// pseudonym digest". The view adds the fields 08-measurement-and-data.md §2 needs and §14
/// does not list separately: the airtime (`airtime_per_node`), the payload and envelope
/// split (`envelope overhead as a fraction of payload`), the generation instant
/// (`e2e_latency` starts at generation, not at transmission) and a message id to join on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTxView {
    /// The instant the frame went on the air.
    pub t: SimTime,
    /// The transmitting node.
    pub node: NodeId,
    /// The message id, for joining a transmission to its receptions and its verification.
    #[serde(default)]
    pub msg: Option<u64>,
    /// The message type (`bsm`, `cam`, `denm`, …).
    #[serde(default)]
    pub msg_type: Option<String>,
    /// Bytes on the wire, envelope and headers included.
    pub bytes_on_wire: u64,
    /// The application payload's bytes, before the security envelope.
    #[serde(default)]
    pub payload_bytes: Option<u64>,
    /// The security envelope's bytes (signature, signer identifier, headers).
    #[serde(default)]
    pub envelope_bytes: Option<u64>,
    /// The airtime this transmission occupied, in microseconds.
    #[serde(default)]
    pub airtime_us: Option<u64>,
    /// The modulation and coding scheme index.
    #[serde(default)]
    pub mcs: Option<u8>,
    /// The transmit power.
    #[serde(default)]
    pub power_dbm: Option<f64>,
    /// The 5.9 GHz channel number.
    #[serde(default)]
    pub channel: Option<u16>,
    /// The EDCA access category.
    #[serde(default)]
    pub ac: Option<u8>,
    /// The DCC state at transmission.
    #[serde(default)]
    pub dcc_state: Option<String>,
    /// Which signer identifier the envelope carried.
    #[serde(default)]
    pub signer: Option<SignerId>,
    /// When the message was generated, if earlier than `t` (queueing and DCC gating sit
    /// between the two).
    #[serde(default)]
    pub t_generated: Option<SimTime>,
    /// When the signer picked the message up — the end of the signing-queue wait.
    #[serde(default)]
    pub t_sign_start: Option<SimTime>,
    /// When the signature completed and the frame was handed to the MAC.
    #[serde(default)]
    pub t_signed: Option<SimTime>,
    /// How much of the channel-access delay (`t − t_signed`) was the AIFS, ns.
    #[serde(default)]
    pub mac_aifs_ns: Option<u64>,
    /// How much of it was the backoff countdown the MAC reported, ns.
    #[serde(default)]
    pub mac_backoff_ns: Option<u64>,
    /// The network and transport header (WSMP, or GeoNetworking plus BTP), octets.
    #[serde(default)]
    pub net_header_bytes: Option<u64>,
    /// The link layer's octets: LLC/SNAP, the 802.11 MAC header and the FCS.
    #[serde(default)]
    pub link_bytes: Option<u64>,
    /// A fragmentation strategy's own per-fragment header, octets.
    #[serde(default)]
    pub frag_header_bytes: Option<u64>,
    /// The SPDU this frame carries (payload plus envelope, or the whole SPDU when the split
    /// is unknown), octets.
    #[serde(default)]
    pub spdu_bytes: Option<u64>,
    /// How many octets of the envelope were the signer's certificate rather than a digest:
    /// the certificate's own encoding when one was attached, zero otherwise.
    #[serde(default)]
    pub cert_bytes: Option<u64>,
    /// The pseudonym certificate the frame was signed with: its IEEE 1609.2 `HashedId8`,
    /// sixteen hex digits (03-interfaces.md §14's "pseudonym digest"). It changes exactly
    /// when the node rotates its pseudonym.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pseudonym: Option<String>,
    /// What the message said, decoded from the payload octets that went on the air.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MsgContentView>,
}

/// What one transmitted message said.
///
/// The wire fields are decoded from the payload octets the node encoded (a J2735
/// `MessageFrame` for a BSM), so they are what a receiver would read, units converted from
/// their J2735 least significant bits. The `claimed_*` fields are the kinematic claim the
/// engine hands the receivers' plausibility detectors in the world frame; for an honest
/// sender they are the node's own belief, for an attacker the falsified claim.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MsgContentView {
    /// `msgCnt`, 0-127, incremented per message and wrapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_count: Option<u8>,
    /// The temporary identifier (BSM `id`, CAM `stationID`), eight hex digits. It is the
    /// first four octets of the pseudonym's digest, so it rotates with the pseudonym.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_id: Option<String>,
    /// `secMark`, milliseconds within the minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sec_mark_ms: Option<u16>,
    /// Latitude, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat_deg: Option<f64>,
    /// Longitude, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lon_deg: Option<f64>,
    /// Elevation, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elev_m: Option<f64>,
    /// Speed, m/s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_mps: Option<f64>,
    /// Heading, degrees clockwise from true north.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_deg: Option<f64>,
    /// How many Part II containers the BSM carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_ii: Option<u8>,
    /// The claimed position in the world frame, metres east.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_x_m: Option<f64>,
    /// The claimed position in the world frame, metres north.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_y_m: Option<f64>,
    /// The claimed speed, m/s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_speed_mps: Option<f64>,
    /// The claimed heading, radians, ENU (0 = east, counter-clockwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_heading_rad: Option<f64>,
}

impl NodeTxView {
    /// The frame's bytes, checked against the per-layer split when the producer gave one:
    /// `Some(true)` when the layers add up to `bytes_on_wire`, `Some(false)` when they do
    /// not, `None` when the split is absent.
    #[must_use]
    pub fn layers_consistent(&self) -> Option<bool> {
        let (Some(spdu), Some(net), Some(link)) =
            (self.spdu_bytes, self.net_header_bytes, self.link_bytes)
        else {
            return None;
        };
        let frag = self.frag_header_bytes.unwrap_or(0);
        let split_ok = match (self.payload_bytes, self.envelope_bytes) {
            (Some(p), Some(e)) => p + e == spdu,
            _ => true,
        };
        Some(split_ok && spdu + net + link + frag == self.bytes_on_wire)
    }
}

impl ChannelView for NodeTxView {
    const CHANNEL: &'static str = "node.tx";
}

/// `phy.rx` — one reception attempt (NODE+GT: the transmitter's identity and the distance
/// are ground truth).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhyRxView {
    /// The instant the frame started arriving.
    pub t_start: SimTime,
    /// The instant it finished.
    pub t_end: SimTime,
    /// The transmitting node (ground truth; exporters project it out for NODE-only files).
    #[serde(default)]
    pub tx: Option<NodeId>,
    /// The receiving node.
    pub rx: NodeId,
    /// The message id, for joining to the transmission.
    #[serde(default)]
    pub msg: Option<u64>,
    /// Received signal strength.
    #[serde(default)]
    pub rssi_dbm: Option<f64>,
    /// Signal to interference-plus-noise ratio.
    #[serde(default)]
    pub sinr_db: Option<f64>,
    /// Whether the frame was received.
    pub outcome: RxOutcome,
    /// The single loss cause, when the outcome is `Lost`.
    #[serde(default)]
    pub cause: Option<String>,
    /// A producer that reports several causes puts them here; invariant I-R3 requires
    /// exactly one in total, and [`crate::invariants`] checks that rather than assuming it.
    #[serde(default)]
    pub causes: Vec<String>,
    /// The transmitter-to-receiver distance (ground truth), for the distance binning.
    #[serde(default)]
    pub dist_m: Option<f64>,
    /// Whether this receiver was a *candidate* reception — within the tier's candidate
    /// range, which is the denominator 08-measurement-and-data.md §2.1 defines PDR over.
    ///
    /// Defaults to `true`: a `phy.rx` record exists because the frame reached the
    /// receiver's arrival set, which is what makes it a candidate. A producer that records
    /// attempts outside the candidate range sets it to `false`.
    #[serde(default = "yes")]
    pub candidate: bool,
    /// The application payload delivered, for goodput.
    #[serde(default)]
    pub payload_bytes: Option<u64>,
    /// For a fragment of an SDU that is reassembled before it reaches the node (a
    /// `fragmenter/generic-sdu` piece): the message id the SDU is followed under on
    /// `node.rx`, so its fragments' attempts are one attempt at the message. Absent for
    /// every whole frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdu: Option<u64>,
}

/// serde default for a `bool` field that defaults to true.
const fn yes() -> bool {
    true
}

impl PhyRxView {
    /// Every loss cause the record carries, from both spellings.
    #[must_use]
    pub fn all_causes(&self) -> Vec<&str> {
        self.cause
            .iter()
            .map(String::as_str)
            .chain(self.causes.iter().map(String::as_str))
            .collect()
    }
}

impl ChannelView for PhyRxView {
    const CHANNEL: &'static str = "phy.rx";
}

/// The width of one packet-reception-ratio distance bin, metres: 3GPP TR 36.885 §A.2.1.4
/// ("CDF of PRR with a bin of 20 meters should be evaluated").
pub const PRR_BIN_M: f64 = 20.0;

/// How far the per-frame reception census reaches, metres. Fifty 20 m bins.
///
/// A fixed constant, deliberately **not** the engine's candidate range: the census counts
/// every equipped receiver within this distance whether or not the radio evaluated a link
/// to it, so a delivery ratio built on it means the same thing however the candidate range
/// is derived.
pub const PRR_MAX_M: f64 = 1_000.0;

/// `phy.prr` — the per-frame reception census behind the packet reception ratio (GT).
///
/// 3GPP TR 36.885 §A.2.1.4 defines the packet reception ratio of one transmitted packet as
/// `X / Y`, where `Y` is the number of receivers located in the distance range `(a, b)` from
/// the transmitter and `X` the number of those that received it successfully. This record is
/// that pair, for every 20 m range out to [`PRR_MAX_M`], for one frame.
///
/// `Y` is a **census**: every equipped node truly within range at the frame's start,
/// counted from ground truth whether or not the engine evaluated a link to it. A receiver
/// the radio never evaluated (beyond the candidate range) is in `Y` and not in `X`, so the
/// ratio does not depend on how many pairs the engine chose to evaluate, only on who was
/// where. That is also why the record is ground truth whole: no node knows who failed to
/// hear it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhyPrrView {
    /// The instant the frame finished (its outcome was decided).
    pub t: SimTime,
    /// The transmitter.
    pub tx: NodeId,
    /// The message id, the one `node.tx` and `phy.rx` carry.
    pub msg: u64,
    /// The message type (`bsm`, `cam`, `spat`, …).
    #[serde(default)]
    pub msg_type: Option<String>,
    /// One entry per non-empty 20 m range: `[bin index, receivers in range, receivers that
    /// decoded]`. Bin `i` covers `[20·i, 20·(i+1))` metres.
    pub bins: Vec<[u32; 3]>,
}

impl ChannelView for PhyPrrView {
    const CHANNEL: &'static str = "phy.prr";
}

/// What finally happened to one reception attempt (`node.rx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RxFate {
    /// The message reached the receiver's applications (verified, or delivered unverified
    /// by the verification policy — [`NodeRxView::verification`] says which).
    Delivered,
    /// The message never reached an application; [`NodeRxView::cause`] says why.
    Lost,
    /// The run ended with the message still between the PHY and the application: neither
    /// delivered nor lost. Recorded so that delivered + lost + in-flight equals the
    /// attempts on `phy.rx` exactly.
    InFlight,
}

/// The layer that decided a loss, for the loss-cause vocabulary of `node.rx`.
pub mod rx_cause {
    /// Causes the physical layer decides (invariant I-R3's list, `v2xw_radio::LossCause`).
    pub const PHY: [&str; 10] = [
        "out-of-range",
        "below-sensitivity",
        "collision",
        "preamble-missed",
        "half-duplex",
        "hidden-terminal",
        "jammed",
        "fading",
        "in-band-emission",
        "resource-collision",
    ];
    /// A fragmented SDU whose set never completed at this receiver.
    pub const REASSEMBLY_FAILED: &str = "reassembly-failed";
    /// The receive queue was full.
    pub const RX_OVERFLOW: &str = "rx-overflow";
    /// The verification policy discarded the message.
    pub const VERIFY_POLICY_DROP: &str = "verify-policy-drop";
    /// The verification queue was full, or the message was evicted from it.
    pub const VERIFY_OVERFLOW: &str = "verify-overflow";
    /// The signature did not verify (or the envelope did not parse).
    pub const SIGNATURE_INVALID: &str = "signature-invalid";
    /// The signer's certificate is on the receiver's revocation list.
    pub const REVOKED: &str = "revoked";
    /// The receiver was switched off (an outage) or retired before it processed the frame.
    pub const RECEIVER_OFF: &str = "receiver-off";

    /// The causes above the PHY, in a fixed order.
    pub const ABOVE_PHY: [&str; 7] = [
        REASSEMBLY_FAILED,
        RX_OVERFLOW,
        VERIFY_POLICY_DROP,
        VERIFY_OVERFLOW,
        SIGNATURE_INVALID,
        REVOKED,
        RECEIVER_OFF,
    ];

    /// True for a cause the PHY decided.
    #[must_use]
    pub fn is_phy(cause: &str) -> bool {
        PHY.contains(&cause) || cause == "unknown"
    }

    /// True for a cause this vocabulary defines.
    #[must_use]
    pub fn is_known(cause: &str) -> bool {
        is_phy(cause) || ABOVE_PHY.contains(&cause)
    }
}

/// The stages a V2V message's end-to-end latency is decomposed into, in the order they
/// happen. [`NodeRxView::latency_trace`] builds them; their durations sum to the
/// end-to-end latency exactly, in integer nanoseconds.
pub const V2V_STAGES: [&str; 10] = [
    "sign_queue",
    "sign",
    "mac_aifs",
    "mac_backoff",
    "mac_defer",
    "airtime",
    "propagation",
    "reception",
    "verify_queue",
    "verify",
];

/// `node.rx` — one reception attempt at one receiver, followed to its end (NODE+GT: the
/// sender's identity and the distance are ground truth, as on `phy.rx`).
///
/// `phy.rx` stops at the PHY's decision. This record follows the same attempt up the
/// stack — reassembly, the receive queue, the verification policy, the verification queue
/// and the signature check — to the one of three fates in [`RxFate`], and it carries every
/// timestamp of the message's journey from its generation at the sender, so the
/// end-to-end latency of a V2V message can be decomposed stage by stage
/// ([`NodeRxView::latency_trace`]).
///
/// There is one record per `phy.rx` record: the PHY-lost attempts are recorded at the
/// PHY's decision, the rest when the receiving node resolves them. Every timestamp is on
/// the simulation's true timeline; a node's own clock offset has already been taken out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRxView {
    /// When the attempt was resolved.
    pub t: SimTime,
    /// The receiving node.
    pub rx: NodeId,
    /// The sending node (ground truth).
    #[serde(default)]
    pub tx: Option<NodeId>,
    /// The message id, the same one `node.tx` carries.
    #[serde(default)]
    pub msg: Option<u64>,
    /// The message type (`bsm`, `cam`, `mbr`, `crl`, …).
    #[serde(default)]
    pub msg_type: Option<String>,
    /// What finally happened.
    pub outcome: RxFate,
    /// The single loss cause, when lost: a PHY cause ([`rx_cause::PHY`]) or one above it
    /// ([`rx_cause::ABOVE_PHY`]).
    #[serde(default)]
    pub cause: Option<String>,
    /// What the receiver concluded about the signature, when delivered: `verified` or
    /// `unverified`.
    #[serde(default)]
    pub verification: Option<String>,
    /// Received power.
    #[serde(default)]
    pub rssi_dbm: Option<f64>,
    /// Mean SINR over the frame.
    #[serde(default)]
    pub sinr_db: Option<f64>,
    /// Sender-to-receiver distance at the start of the frame (ground truth).
    #[serde(default)]
    pub dist_m: Option<f64>,
    /// The frame's PSDU octets.
    #[serde(default)]
    pub bytes_on_wire: Option<u64>,
    /// The frame's air time, µs.
    #[serde(default)]
    pub airtime_us: Option<u64>,
    /// The application payload octets the message carries.
    #[serde(default)]
    pub payload_bytes: Option<u64>,
    /// The message's generation at the sender.
    #[serde(default)]
    pub t_generated: Option<SimTime>,
    /// The sender's signer picked it up.
    #[serde(default)]
    pub t_sign_start: Option<SimTime>,
    /// The signature completed and the frame reached the MAC.
    #[serde(default)]
    pub t_signed: Option<SimTime>,
    /// Of the channel-access delay, the AIFS, ns.
    #[serde(default)]
    pub mac_aifs_ns: Option<u64>,
    /// Of the channel-access delay, the backoff countdown, ns.
    #[serde(default)]
    pub mac_backoff_ns: Option<u64>,
    /// The preamble went on the air.
    #[serde(default)]
    pub t_tx_start: Option<SimTime>,
    /// The last symbol left the transmitter.
    #[serde(default)]
    pub t_tx_end: Option<SimTime>,
    /// The last symbol reached this receiver: `t_tx_end` plus the propagation delay.
    #[serde(default)]
    pub t_arrival: Option<SimTime>,
    /// The receiver finished parsing the frame and applied its verification policy.
    #[serde(default)]
    pub t_rx_done: Option<SimTime>,
    /// The signature check started.
    #[serde(default)]
    pub t_verify_start: Option<SimTime>,
    /// The signature check finished.
    #[serde(default)]
    pub t_verify_done: Option<SimTime>,
    /// The message was handed to the receiver's applications.
    #[serde(default)]
    pub t_delivered: Option<SimTime>,
}

impl ChannelView for NodeRxView {
    const CHANNEL: &'static str = "node.rx";
}

impl NodeRxView {
    /// The end-to-end latency of a delivered message, ns, when its stamps are complete.
    #[must_use]
    pub fn e2e_ns(&self) -> Option<u64> {
        let (g, d) = (self.t_generated?, self.t_delivered?);
        d.checked_sub(g)
    }

    /// The message's journey as a [`crate::latency::LatencyTrace`], stage by stage
    /// ([`V2V_STAGES`]).
    ///
    /// `None` unless the message was delivered and every stamp the decomposition needs is
    /// present. The trace is built so its spans are contiguous by construction; whether the
    /// stamps were *monotonic* is the trace's own [`crate::latency::LatencyTrace::validate`]
    /// to answer, and a trace that fails it is a producer defect a test can see rather than
    /// a sample this function quietly repairs.
    #[must_use]
    pub fn latency_trace(&self) -> Option<crate::latency::LatencyTrace> {
        if self.outcome != RxFate::Delivered {
            return None;
        }
        let g0 = self.t_generated?;
        let sign_start = self.t_sign_start?;
        let signed = self.t_signed?;
        let tx_start = self.t_tx_start?;
        let tx_end = self.t_tx_end?;
        let arrival = self.t_arrival?;
        let rx_done = self.t_rx_done?;
        let delivered = self.t_delivered?;
        // An unverified delivery spends no time in the verification stages: they are
        // zero-length at the instant the policy decided.
        let verify_start = self.t_verify_start.unwrap_or(rx_done);
        let verify_done = self.t_verify_done.unwrap_or(verify_start);
        // The access delay is apportioned AIFS first, then backoff, then the remainder —
        // deferral to a busy medium, including the AIFS a node repeats after each busy
        // period. The apportionment is an accounting order, not a claim that the medium
        // was idle for the first AIFS: `saturating_sub` keeps each part within the whole.
        let access = tx_start.saturating_sub(signed);
        let aifs = self.mac_aifs_ns.unwrap_or(0).min(access);
        let backoff = self.mac_backoff_ns.unwrap_or(0).min(access - aifs);
        let mut b = crate::latency::TraceBuilder::new("v2v", self.msg_type.clone(), self.msg, g0);
        b.to("sign_queue", sign_start);
        b.to("sign", signed);
        b.to("mac_aifs", signed.saturating_add(aifs));
        b.to("mac_backoff", signed.saturating_add(aifs + backoff));
        b.to("mac_defer", tx_start);
        b.to("airtime", tx_end);
        b.to("propagation", arrival);
        b.to("reception", rx_done);
        b.to("verify_queue", verify_start);
        b.to("verify", verify_done);
        let trace = b.finish();
        // The delivery instant closes the trace: a stamp set in which the application
        // received the message at any other instant than the last stage ended is not a
        // decomposition of this message's latency.
        (trace.end() == Some(delivered)).then_some(trace)
    }
}

/// `mac.cbr` — the channel busy ratio the MAC measured (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MacCbrView {
    /// The end of the measurement window.
    pub t: SimTime,
    /// The measuring node.
    pub node: NodeId,
    /// The 5.9 GHz channel number.
    #[serde(default)]
    pub channel: Option<u16>,
    /// The measured ratio in `[0, 1]`.
    pub cbr: f64,
    /// The busy time in the window, where the producer reports it. When both this and
    /// `window_us` are present the ratio is recomputed from them, so a producer's rounding
    /// does not become the metric's.
    #[serde(default)]
    pub busy_us: Option<u64>,
    /// The measurement window's length (802.11p: 100 ms).
    #[serde(default)]
    pub window_us: Option<u64>,
    /// Frames waiting in the node's EDCA queues at the end of the window.
    #[serde(default)]
    pub queue_depth: Option<u64>,
    /// Frames the MAC refused since the previous report (a full access-category queue or
    /// a frame over the MSDU cap).
    #[serde(default)]
    pub mac_drops: Option<u64>,
    /// Frames handed to the MAC since the previous report, whatever became of them.
    #[serde(default)]
    pub offered_frames: Option<u64>,
    /// Their PSDU octets.
    #[serde(default)]
    pub offered_bytes: Option<u64>,
    /// Their air time, had every one of them gone on the air, µs.
    #[serde(default)]
    pub offered_airtime_us: Option<u64>,
    /// The span these per-report counters cover, ns — normally the metric period.
    #[serde(default)]
    pub span_ns: Option<u64>,
}

impl ChannelView for MacCbrView {
    const CHANNEL: &'static str = "mac.cbr";
}

/// `net.frag` — one reassembly outcome (NODE), as `v2xw_net::frag::FragRecord` writes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetFragView {
    /// The instant of the outcome, on the receiving node's own clock (`t_ns` on the wire).
    #[serde(alias = "t_ns")]
    pub t: SimTime,
    /// The reassembling node.
    pub node: NodeId,
    /// The SDU's id.
    pub sdu: u64,
    /// How many fragments the SDU was split into.
    pub fragments: u32,
    /// The outcome.
    pub outcome: FragOutcome,
    /// The message type, for the per-type breakdown.
    #[serde(default)]
    pub msg_type: Option<String>,
    /// How many distinct fragments had arrived when the outcome was reached.
    #[serde(default)]
    pub have: Option<u32>,
    /// SDU payload octets delivered (on `complete`) or discarded (on `expired`).
    #[serde(default)]
    pub bytes: Option<u64>,
}

impl ChannelView for NetFragView {
    const CHANNEL: &'static str = "net.frag";
}

/// `net.reassembly` — one fragmented SDU followed to its fate at one receiver (GT).
///
/// The loss-amplification record of 04-models.md §7.4: the realised outcome next to the
/// loss the per-fragment PHY success probabilities predicted if the fragments were lost
/// independently. One record per (SDU, receiver that had at least one of its fragments in
/// its arrival set), written when the group completes or its timeout runs out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetReassemblyView {
    /// When the group was resolved.
    pub t: SimTime,
    /// The receiving node.
    pub rx: NodeId,
    /// The sending node (ground truth).
    #[serde(default)]
    pub tx: Option<NodeId>,
    /// The message id the group is recorded under.
    pub sdu: u64,
    /// The fragmenter's model id.
    pub strategy: String,
    /// What was reassembled: `message` (pieces of one SDU), `segments` (independently
    /// interpretable segments) or `certificate` (a hybrid certificate over a cycle).
    pub kind: String,
    /// The message type.
    #[serde(default)]
    pub msg_type: Option<String>,
    /// How many fragments the SDU was split into.
    pub fragments: u32,
    /// How many of them decoded at this receiver.
    pub received: u32,
    /// The SDU's octets: what its fragments carry between them (the signed message, or
    /// the hybrid certificate), not counting any fragment's own header.
    pub bytes: u64,
    /// The SDU octets the decoded fragments carried.
    pub bytes_received: u64,
    /// The predicted probability that some fragment is lost, `1 − Π (1 − p_i)`, with `p_i`
    /// one minus the PHY's success probability for each fragment at this receiver (one for
    /// a fragment that never reached it). For pieces and certificates it is the SDU's loss.
    pub predicted_loss: f64,
    /// The predicted fraction of the SDU's content lost, `Σ w_i p_i` with `w_i` each
    /// fragment's share of the payload: what independent segments lose.
    pub predicted_content_loss: f64,
    /// `complete` when every fragment decoded, `lost` otherwise.
    pub outcome: String,
    /// For a lost group, the first PHY loss cause among its fragments, or
    /// `reassembly-failed` when every fragment that reached it decoded and one never did.
    #[serde(default)]
    pub cause: Option<String>,
}

impl ChannelView for NetReassemblyView {
    const CHANNEL: &'static str = "net.reassembly";
}

/// `node.verify` — one verification task (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeVerifyView {
    /// When the task was enqueued.
    pub t_enqueue: SimTime,
    /// When it started running.
    #[serde(default)]
    pub t_start: Option<SimTime>,
    /// When it finished (and the message was delivered to the application).
    #[serde(default)]
    pub t_done: Option<SimTime>,
    /// The verifying node.
    pub node: NodeId,
    /// The primitive (`ecdsa-p256`, `ml-dsa-65`, …).
    #[serde(default)]
    pub primitive: Option<String>,
    /// The modeled or measured cost, in microseconds.
    #[serde(default)]
    pub cost_us: Option<u64>,
    /// The outcome.
    pub outcome: VerifyOutcome,
    /// The verification policy's decision, where the policy recorded one.
    #[serde(default)]
    pub policy: Option<String>,
    /// The message id, for joining to the transmission.
    #[serde(default)]
    pub msg: Option<u64>,
    /// The queue depth when the task was enqueued.
    #[serde(default)]
    pub queue_depth: Option<u64>,
}

impl ChannelView for NodeVerifyView {
    const CHANNEL: &'static str = "node.verify";
}

/// `sec.cert` — a credential event at a node (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecCertView {
    /// The instant.
    pub t: SimTime,
    /// The node.
    pub node: NodeId,
    /// The event: `change`, `expire`, `top-up`, `learn`.
    pub event: String,
    /// The certificate digest.
    #[serde(default)]
    pub digest: Option<String>,
    /// Bytes downloaded, for a top-up.
    #[serde(default)]
    pub bytes: Option<u64>,
}

impl ChannelView for SecCertView {
    const CHANNEL: &'static str = "sec.cert";
}

/// `proto.revocation` — one revocation stage timestamp (PUBLIC, 05-protocols.md §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtoRevocationView {
    /// The instant the stage was reached.
    pub t: SimTime,
    /// The stage id: `detect`, `report_sent`, `report_received`, `decision`, `issued`,
    /// `published`, `downloaded`, `enforced`.
    pub stage: String,
    /// The revocation's id — the subject being revoked. Stages of one revocation share it.
    pub id: String,
    /// The list's size in bytes, on the stages that carry one.
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// The list's entry count, on the stages that carry one.
    #[serde(default)]
    pub entries: Option<u64>,
    /// The node the stage happened at, for the per-node stages (`downloaded`, `enforced`).
    #[serde(default)]
    pub node: Option<NodeId>,
}

impl ChannelView for ProtoRevocationView {
    const CHANNEL: &'static str = "proto.revocation";
}

/// `det.observation` — one local detector firing (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetObservationView {
    /// The instant.
    pub t: SimTime,
    /// The observing node.
    pub node: NodeId,
    /// The detector's id.
    pub detector: String,
    /// The subject's pseudonym digest — what the node can see, not who it really is.
    pub subject: String,
    /// The detector's score.
    #[serde(default)]
    pub score: Option<f64>,
}

impl ChannelView for DetObservationView {
    const CHANNEL: &'static str = "det.observation";
}

/// `ma.report` — a misbehaviour report as it reached the authority (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaReportView {
    /// The instant the report reached the authority.
    pub t: SimTime,
    /// The reporting node.
    #[serde(default)]
    pub reporter: Option<NodeId>,
    /// The subject of the report, as the reporter could name it.
    pub subject: String,
    /// The detector that produced the report.
    #[serde(default)]
    pub detector: Option<String>,
}

impl ChannelView for MaReportView {
    const CHANNEL: &'static str = "ma.report";
}

/// `ma.decision` — a misbehaviour authority's decision about a subject (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaDecisionView {
    /// The instant of the decision.
    pub t: SimTime,
    /// The subject.
    pub subject: String,
    /// The decision: `revoke`, `dismiss`, `investigate`.
    pub decision: String,
}

impl ChannelView for MaDecisionView {
    const CHANNEL: &'static str = "ma.decision";
}

/// `gt.kinematics` — an actor's true state (GT).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtKinematicsView {
    /// The instant.
    pub t: SimTime,
    /// The actor.
    pub actor: ActorId,
    /// East, in the world's local tangent plane (build decision D6).
    pub x_m: f64,
    /// North.
    pub y_m: f64,
    /// Up.
    #[serde(default)]
    pub z_m: Option<f64>,
    /// Speed along the heading.
    pub speed_mps: f64,
    /// Acceleration along the heading.
    #[serde(default)]
    pub acc_mps2: Option<f64>,
    /// Heading in radians, ENU, 0 = east, counter-clockwise (D6).
    #[serde(default)]
    pub heading_rad: Option<f64>,
    /// The lane the actor is on.
    #[serde(default)]
    pub lane: Option<u32>,
    /// The distance travelled along the lane, for a headway computation on one lane.
    #[serde(default)]
    pub lane_pos_m: Option<f64>,
    /// The actor's class, for the per-class breakdown.
    #[serde(default)]
    pub class: Option<String>,
    /// The node mounted on the actor, when it is equipped. Ground truth, like the rest of
    /// the record: it is what joins a vehicle's true position to the node that names it on
    /// `node.rx`, which the neighbour-awareness ratio needs.
    #[serde(default)]
    pub node: Option<NodeId>,
}

impl ChannelView for GtKinematicsView {
    const CHANNEL: &'static str = "gt.kinematics";
}

/// `gt.attack.action` — an attacker's action, with the true actor id (GT, invariant I-T3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtAttackActionView {
    /// The instant.
    pub t: SimTime,
    /// The true actor behind the action — the field I-T3 requires.
    pub actor: ActorId,
    /// The attacker model's id.
    pub attacker: String,
    /// The action.
    pub action: String,
    /// The fields the action changed.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Whether the action changed bytes on the air. I-T3 is about exactly those actions.
    #[serde(default = "yes")]
    pub changed_bytes_on_air: bool,
    /// The message id the action produced, for joining to `node.tx`.
    #[serde(default)]
    pub msg: Option<u64>,
}

impl ChannelView for GtAttackActionView {
    const CHANNEL: &'static str = "gt.attack.action";
}

/// `net.bytes` — the reader-side projection of the `net.*` byte-accounting records
/// (08-measurement-and-data.md §2.1 names the source channels `node.tx`, `net.*`,
/// `proto.msg`).
///
/// Invariant I-N1 is about "every byte counted in `bytes_on_wire`", so the view carries the
/// identity of what was sent as well as its size: without an id, a double attribution is
/// undetectable and the invariant becomes unfalsifiable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetBytesView {
    /// The instant.
    pub t: SimTime,
    /// The identity of the frame, PDU or message these bytes belong to. `None` means the
    /// producer did not say, which I-N1 reports as unattributable rather than ignoring.
    #[serde(default)]
    pub id: Option<u64>,
    /// The accounting bucket.
    pub bucket: ByteBucket,
    /// The bytes on the wire.
    pub bytes_on_wire: u64,
    /// The node, where one link end is a node.
    #[serde(default)]
    pub node: Option<NodeId>,
}

impl ChannelView for NetBytesView {
    const CHANNEL: &'static str = "net.bytes";
}

/// `proto.msg` — one protocol message between entities (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtoMsgView {
    /// The instant.
    pub t: SimTime,
    /// The sending entity.
    #[serde(default)]
    pub from: Option<NodeId>,
    /// The receiving entity.
    #[serde(default)]
    pub to: Option<NodeId>,
    /// The flow's id (05-protocols.md).
    #[serde(default)]
    pub flow: Option<String>,
    /// The step within the flow.
    #[serde(default)]
    pub step: Option<String>,
    /// The bytes on the wire.
    pub bytes_on_wire: u64,
    /// The transport this message crossed, which names its accounting bucket.
    #[serde(default)]
    pub transport: Option<ByteBucket>,
    /// The message id.
    #[serde(default)]
    pub msg: Option<u64>,
}

impl ChannelView for ProtoMsgView {
    const CHANNEL: &'static str = "proto.msg";
}

/// `node.telemetry` — a node's resource accounting (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTelemetryView {
    /// The instant.
    pub t: SimTime,
    /// The node.
    pub node: NodeId,
    /// CPU utilisation, as a fraction in `[0, 1]`.
    #[serde(default)]
    pub cpu: Option<f64>,
    /// HSM utilisation, as a fraction in `[0, 1]`.
    #[serde(default)]
    pub hsm: Option<f64>,
    /// RAM in use.
    #[serde(default)]
    pub ram_bytes: Option<u64>,
    /// Storage in use.
    #[serde(default)]
    pub storage_bytes: Option<u64>,
    /// The verification queue's depth.
    #[serde(default)]
    pub verify_queue_depth: Option<u64>,
    /// The receive queue's depth over the window, `[p50, p95]` (vwp-v1 §3.5.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_rx: Option<[u16; 2]>,
    /// The verification queue's depth over the window, `[p50, p95]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_verify: Option<[u16; 2]>,
    /// The application queue's depth over the window, `[p50, p95]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_app: Option<[u16; 2]>,
    /// The transmit queue's depth over the window, `[p50, p95]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_tx: Option<[u16; 2]>,
    /// The CRL task queue's depth over the window, `[p50, p95]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_crl: Option<[u16; 2]>,
    /// The 95th percentile of the wait from enqueue to verification start, ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_wait_p95_ms: Option<f64>,
    /// Own pseudonym certificates currently valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_active: Option<u16>,
    /// Neighbours in the table, and how many of them are verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbr_total: Option<u16>,
    /// Neighbours in state *verified*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbr_verified: Option<u16>,
}

impl ChannelView for NodeTelemetryView {
    const CHANNEL: &'static str = "node.telemetry";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(channel: &'static str, json: &str) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility: Visibility::Node,
            json: json.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_view_reads_only_the_fields_it_needs() {
        let r = rec(
            "node.tx",
            r#"{"t":1000,"node":3,"bytes_on_wire":400,"airtime_us":600,
                "something_a_future_producer_added":true}"#,
        );
        let v: NodeTxView = decode(&r).unwrap();
        assert_eq!(v.node, NodeId::new(3));
        assert_eq!(v.bytes_on_wire, 400);
        assert_eq!(v.airtime_us, Some(600));
        assert_eq!(v.payload_bytes, None, "absent means absent, not zero");
    }

    #[test]
    fn a_record_from_another_channel_is_refused() {
        let r = rec("mac.cbr", r#"{"t":1,"node":0,"cbr":0.3}"#);
        let e = decode::<NodeTxView>(&r).unwrap_err();
        assert!(matches!(e, MetricError::ChannelMismatch { .. }));
    }

    #[test]
    fn malformed_json_names_the_channel() {
        let r = rec("mac.cbr", r#"{"t":1,"node":0}"#);
        let e = decode::<MacCbrView>(&r).unwrap_err();
        assert!(matches!(e, MetricError::Decode { ref channel, .. } if channel == "mac.cbr"));
    }

    #[test]
    fn a_reception_defaults_to_being_a_candidate() {
        let r = rec(
            "phy.rx",
            r#"{"t_start":0,"t_end":10,"rx":2,"outcome":"ok"}"#,
        );
        let v: PhyRxView = decode(&r).unwrap();
        assert!(v.candidate);
        assert!(v.all_causes().is_empty());

        let r = rec(
            "phy.rx",
            r#"{"t_start":0,"t_end":10,"rx":2,"outcome":"lost","cause":"collision",
                "candidate":false}"#,
        );
        let v: PhyRxView = decode(&r).unwrap();
        assert!(!v.candidate);
        assert_eq!(v.all_causes(), vec!["collision"]);
    }

    #[test]
    fn the_bucket_names_match_the_metric_names_of_08_measurement() {
        assert_eq!(ByteBucket::Air.metric_name(), "bytes_air");
        assert_eq!(ByteBucket::CellularUl.metric_name(), "bytes_uu_ul");
        assert_eq!(ByteBucket::CellularDl.metric_name(), "bytes_uu_dl");
        assert_eq!(ByteBucket::Backhaul.metric_name(), "bytes_backhaul");
        assert_eq!(ByteBucket::Backend.metric_name(), "bytes_backend");
        assert_eq!(ByteBucket::ALL.len(), 5);
    }

    #[test]
    fn the_visibility_table_is_sorted_and_covers_the_channels_this_crate_reads() {
        // Sorted, so a reader can find a channel and a future addition has one place to go.
        let names: Vec<&str> = CHANNEL_VISIBILITY.iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for channel in [
            "node.tx",
            "phy.rx",
            "mac.cbr",
            "net.frag",
            "net.bytes",
            "node.verify",
            "node.telemetry",
            "sec.cert",
            "proto.msg",
            "proto.revocation",
            "det.observation",
            "ma.report",
            "ma.decision",
            "gt.kinematics",
            "gt.attack.action",
        ] {
            assert!(
                allowed_visibilities(channel).is_some(),
                "{channel} is missing from the visibility table"
            );
        }
        assert_eq!(
            allowed_visibilities("phy.rx"),
            Some(&[Visibility::Node, Visibility::NodeAndGt][..])
        );
        assert_eq!(allowed_visibilities("a.plugin.invented.this"), None);
    }

    #[test]
    fn channel_names_match_03_interfaces_section_14() {
        assert_eq!(NodeTxView::channel_name().as_str(), "node.tx");
        assert_eq!(PhyRxView::channel_name().as_str(), "phy.rx");
        assert_eq!(MacCbrView::channel_name().as_str(), "mac.cbr");
        assert_eq!(NetFragView::channel_name().as_str(), "net.frag");
        assert_eq!(NodeVerifyView::channel_name().as_str(), "node.verify");
        assert_eq!(SecCertView::channel_name().as_str(), "sec.cert");
        assert_eq!(
            ProtoRevocationView::channel_name().as_str(),
            "proto.revocation"
        );
        assert_eq!(
            DetObservationView::channel_name().as_str(),
            "det.observation"
        );
        assert_eq!(GtKinematicsView::channel_name().as_str(), "gt.kinematics");
        assert_eq!(
            GtAttackActionView::channel_name().as_str(),
            "gt.attack.action"
        );
        assert_eq!(ProtoMsgView::channel_name().as_str(), "proto.msg");
        assert_eq!(NodeTelemetryView::channel_name().as_str(), "node.telemetry");
        assert_eq!(MaDecisionView::channel_name().as_str(), "ma.decision");
        assert_eq!(MaReportView::channel_name().as_str(), "ma.report");
    }
}
