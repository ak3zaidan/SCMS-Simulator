//! The followed node's message feed and queues: what it sent, what it heard, and what is
//! waiting inside it right now.
//!
//! The owner's ask: "when you go down into a specific chase view for a vehicle, the user
//! should be able to see the broadcast messages being sent out and their content, along
//! with the queue of that specific node."
//!
//! # Where the facts come from
//!
//! * **Sent.** Every `node.tx` record, joined by message id to the octets of the SPDU the
//!   node signed (`v2xw_engine::RunRecorder::tap_frame`). The content is decoded from those
//!   octets ([`decode`]), never from the record's own summary.
//! * **Received.** Every `node.rx` record at the node: the sender (ground truth), the fate
//!   and its single loss cause, the received power, the SINR, the distance, the signature
//!   verdict, and the message's journey stamp by stamp. A delivered message's content is
//!   decoded from the sender's SPDU, which is the octet string the receiver verified. The
//!   PHY's two "never detected" causes (`out-of-range`, `below-sensitivity`) are counted and
//!   not listed: the receiver cannot know about a frame it never detected, and with a 1 km
//!   candidate range they are most attempts.
//! * **Queues.** Reconstructed at the stream's instant from the same stamps: a received
//!   message is in the receive stage from its arrival to the end of its parse, in the
//!   verification queue from then until its signature check starts, being verified until it
//!   ends, and in the application queue until it is delivered; a generated message is in the
//!   transmit pipeline — the signing queue, the signer, then channel access — from its
//!   generation to the instant it goes on the air. These are the node runtime's own
//!   instants (`v2xw-node`, moved onto the simulation's timeline by the engine), so the
//!   depth and the waits are the model's, not a second model of it. The CRL task queue has
//!   no per-task stamps on any channel; its depth is the node's own telemetry window.
//!
//! # Bounds
//!
//! The kernel runs up to a bounded distance ahead of the stream, so the feed is filled at
//! the kernel's frontier and read at the stream's instant. It keeps [`HISTORY_NS`] behind
//! the stream for every node — so a vehicle clicked a moment ago already has a past — plus
//! whatever lies ahead, and a hard per-node cap ([`MAX_PER_NODE`]) that counts what it
//! sheds. What reaches a client is bounded again per push ([`FeedLimits`]), newest first,
//! with the number left out.

pub mod decode;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use serde_json::{Value, json};
use v2xw_core::ids::NodeId;
use v2xw_core::time::SimTime;
use v2xw_metrics::channels::{NodeRxView, NodeTxView, RxFate, SignerId, rx_cause};

/// The feed's schema version, carried in every `node.feed` notification as `v`.
///
/// Additive changes (a new optional member) keep it; a change a v1 reader would misread
/// raises it. `@vwp/protocol`'s `NODE_FEED_VERSION` is the same number, and both sides'
/// conformance tests read the one shared vector (`docs/protocol/vectors/node-feed-v1.json`).
pub const FEED_VERSION: u32 = 1;

/// How far behind the stream the feed keeps a node's traffic, ns.
pub const HISTORY_NS: u64 = 20_000_000_000;

/// The most entries per direction a node keeps, whatever the time window holds.
pub const MAX_PER_NODE: usize = 4_096;

/// The window the queues' wait percentiles and drop counts cover, ns.
pub const QUEUE_WINDOW_NS: u64 = 1_000_000_000;

/// The window the drop counts cover, ns.
pub const DROP_WINDOW_NS: u64 = 10_000_000_000;

/// PHY causes the receiver cannot observe: the frame was never detected.
const UNDETECTED: [&str; 2] = ["out-of-range", "below-sensitivity"];

/// A message type as a static string, so an entry holds no heap text.
fn type_code(name: Option<&str>) -> &'static str {
    match name {
        Some("bsm") => "bsm",
        Some("cam") => "cam",
        Some("denm") => "denm",
        Some("spat") => "spat",
        Some("map") => "map",
        Some("psm") => "psm",
        Some("vam") => "vam",
        Some("cpm") => "cpm",
        Some("srm") => "srm",
        Some("ssm") => "ssm",
        Some("wsa") => "wsa",
        Some("crl") => "crl",
        Some("mbr") => "mbr",
        _ => "other",
    }
}

/// A loss cause as a static string from the published vocabulary.
fn cause_code(cause: Option<&str>) -> Option<&'static str> {
    let c = cause?;
    rx_cause::PHY
        .iter()
        .chain(rx_cause::ABOVE_PHY.iter())
        .find(|k| **k == c)
        .copied()
        .or(Some(if c == "unknown" { "unknown" } else { "other" }))
}

/// One frame a node put on the air.
#[derive(Debug, Clone)]
pub struct TxFrame {
    /// When it went on the air.
    pub t: SimTime,
    /// The sender.
    pub node: u32,
    /// The message id `node.tx` and `node.rx` join on.
    pub msg: u64,
    /// `bsm`, `cam`, ….
    pub msg_type: &'static str,
    /// The PSDU's octets, and the per-layer split when the producer gave one.
    pub bytes_on_wire: u32,
    /// Payload, envelope, certificate, network header and link octets.
    pub layers: [Option<u32>; 5],
    /// Air time, µs.
    pub airtime_us: Option<u32>,
    /// Transmit power, dBm.
    pub power_dbm: Option<f32>,
    /// Channel number.
    pub channel: Option<u16>,
    /// `certificate`, `digest` or `self`.
    pub signer: Option<&'static str>,
    /// Generation, signer pick-up, signed (handed to the MAC).
    pub t_generated: Option<SimTime>,
    /// When the signer picked it up.
    pub t_sign_start: Option<SimTime>,
    /// When it was signed.
    pub t_signed: Option<SimTime>,
    /// The pseudonym certificate's HashedId8, as the record names it.
    pub pseudonym: Option<[u8; 8]>,
    /// The SPDU octets, when the node encoded one.
    pub spdu: Option<Arc<[u8]>>,
}

/// One reception attempt at a node, compact.
#[derive(Debug, Clone)]
pub struct RxEntry {
    /// When the attempt was resolved.
    pub t: SimTime,
    /// The sender (ground truth).
    pub from: Option<u32>,
    /// The message id.
    pub msg: Option<u64>,
    /// The message type.
    pub msg_type: &'static str,
    /// `delivered`, `lost` or `in-flight`.
    pub outcome: RxFate,
    /// The loss cause, when lost.
    pub cause: Option<&'static str>,
    /// `verified`, `unverified`, or none.
    pub verified: Option<bool>,
    /// Received power, SINR and distance.
    pub rssi_dbm: Option<f32>,
    /// SINR, dB.
    pub sinr_db: Option<f32>,
    /// Distance, m (ground truth).
    pub dist_m: Option<f32>,
    /// PSDU octets.
    pub bytes_on_wire: Option<u32>,
    /// The journey: generated, sign start, signed, tx start, tx end, arrival, parsed,
    /// verify start, verify done, delivered.
    pub stamps: [Option<SimTime>; 10],
    /// AIFS and backoff shares of channel access, ns.
    pub mac: [Option<u64>; 2],
    /// Airtime, µs, and payload octets.
    pub airtime_us: Option<u64>,
    /// Payload octets.
    pub payload_bytes: Option<u64>,
}

const S_GEN: usize = 0;
const S_SIGN_START: usize = 1;
const S_SIGNED: usize = 2;
const S_TX_START: usize = 3;
const S_TX_END: usize = 4;
const S_ARRIVAL: usize = 5;
const S_RX_DONE: usize = 6;
const S_VERIFY_START: usize = 7;
const S_VERIFY_DONE: usize = 8;
const S_DELIVERED: usize = 9;

impl RxEntry {
    fn of(v: &NodeRxView) -> Self {
        RxEntry {
            t: v.t,
            from: v.tx.map(NodeId::index),
            msg: v.msg,
            msg_type: type_code(v.msg_type.as_deref()),
            outcome: v.outcome,
            cause: cause_code(v.cause.as_deref()),
            verified: v.verification.as_deref().map(|s| s == "verified"),
            rssi_dbm: v.rssi_dbm.map(|x| x as f32),
            sinr_db: v.sinr_db.map(|x| x as f32),
            dist_m: v.dist_m.map(|x| x as f32),
            bytes_on_wire: v
                .bytes_on_wire
                .map(|b| u32::try_from(b).unwrap_or(u32::MAX)),
            stamps: [
                v.t_generated,
                v.t_sign_start,
                v.t_signed,
                v.t_tx_start,
                v.t_tx_end,
                v.t_arrival,
                v.t_rx_done,
                v.t_verify_start,
                v.t_verify_done,
                v.t_delivered,
            ],
            mac: [v.mac_aifs_ns, v.mac_backoff_ns],
            airtime_us: v.airtime_us,
            payload_bytes: v.payload_bytes,
        }
    }

    /// The record back, for the metrics crate's own latency decomposition.
    fn view(&self, rx: u32) -> NodeRxView {
        let s = &self.stamps;
        NodeRxView {
            t: self.t,
            rx: NodeId::new(rx),
            tx: self.from.map(NodeId::new),
            msg: self.msg,
            msg_type: Some(self.msg_type.to_string()),
            outcome: self.outcome,
            cause: self.cause.map(str::to_string),
            verification: self
                .verified
                .map(|v| if v { "verified" } else { "unverified" }.to_string()),
            rssi_dbm: self.rssi_dbm.map(f64::from),
            sinr_db: self.sinr_db.map(f64::from),
            dist_m: self.dist_m.map(f64::from),
            bytes_on_wire: self.bytes_on_wire.map(u64::from),
            airtime_us: self.airtime_us,
            payload_bytes: self.payload_bytes,
            t_generated: s[S_GEN],
            t_sign_start: s[S_SIGN_START],
            t_signed: s[S_SIGNED],
            mac_aifs_ns: self.mac[0],
            mac_backoff_ns: self.mac[1],
            t_tx_start: s[S_TX_START],
            t_tx_end: s[S_TX_END],
            t_arrival: s[S_ARRIVAL],
            t_rx_done: s[S_RX_DONE],
            t_verify_start: s[S_VERIFY_START],
            t_verify_done: s[S_VERIFY_DONE],
            t_delivered: s[S_DELIVERED],
        }
    }
}

/// One node's recent traffic.
#[derive(Debug, Default, Clone)]
struct NodeLog {
    /// Message ids of the frames it sent, in on-air order.
    sent: VecDeque<u64>,
    /// Receptions, in resolution order.
    received: VecDeque<RxEntry>,
    /// Entries the per-node cap shed, per direction.
    shed: [u64; 2],
    /// Attempts the receiver never detected, by cause, since the run began.
    undetected: u64,
}

/// How much one push carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeedLimits {
    /// Newest sent frames per push.
    pub sent: usize,
    /// Newest receptions per push.
    pub received: usize,
    /// Waiting entries listed per queue.
    pub waiting: usize,
    /// Whether each entry carries its SPDU octets in hex.
    pub bytes: bool,
}

impl Default for FeedLimits {
    fn default() -> Self {
        FeedLimits {
            sent: 20,
            received: 40,
            waiting: 8,
            bytes: true,
        }
    }
}

/// Every node's recent traffic, filled at the kernel's frontier.
#[derive(Debug, Default)]
pub struct FeedStore {
    frames: BTreeMap<u64, TxFrame>,
    logs: BTreeMap<u32, NodeLog>,
    /// The SPDUs tapped in the step being projected, until their `node.tx` records arrive.
    pending_taps: BTreeMap<u64, Arc<[u8]>>,
}

impl FeedStore {
    /// An empty store.
    pub fn new() -> Self {
        FeedStore::default()
    }

    /// Holds one tapped SPDU until its `node.tx` record is read.
    pub fn tap(&mut self, msg: u64, spdu: Arc<[u8]>) {
        self.pending_taps.insert(msg, spdu);
    }

    /// Takes one `node.tx` record.
    pub fn on_tx(&mut self, v: &NodeTxView) {
        let msg = v.msg.unwrap_or(u64::MAX);
        let spdu = v.msg.and_then(|m| self.pending_taps.remove(&m));
        let mut pseudonym = None;
        if let Some(p) = v.pseudonym.as_deref()
            && p.len() == 16
        {
            let mut b = [0u8; 8];
            let ok = (0..8).all(|i| {
                u8::from_str_radix(&p[2 * i..2 * i + 2], 16)
                    .map(|x| b[i] = x)
                    .is_ok()
            });
            if ok {
                pseudonym = Some(b);
            }
        }
        let small = |x: Option<u64>| x.map(|b| u32::try_from(b).unwrap_or(u32::MAX));
        let frame = TxFrame {
            t: v.t,
            node: v.node.index(),
            msg,
            msg_type: type_code(v.msg_type.as_deref()),
            bytes_on_wire: u32::try_from(v.bytes_on_wire).unwrap_or(u32::MAX),
            layers: [
                small(v.payload_bytes),
                small(v.envelope_bytes),
                small(v.cert_bytes),
                small(v.net_header_bytes),
                small(v.link_bytes),
            ],
            airtime_us: small(v.airtime_us),
            power_dbm: v.power_dbm.map(|p| p as f32),
            channel: v.channel,
            signer: v.signer.map(|s| match s {
                SignerId::Certificate => "certificate",
                SignerId::Digest => "digest",
                SignerId::SelfSigned => "self",
            }),
            t_generated: v.t_generated,
            t_sign_start: v.t_sign_start,
            t_signed: v.t_signed,
            pseudonym,
            spdu,
        };
        let log = self.logs.entry(frame.node).or_default();
        if log.sent.len() >= MAX_PER_NODE {
            log.sent.pop_front();
            log.shed[0] += 1;
        }
        log.sent.push_back(msg);
        self.frames.insert(msg, frame);
    }

    /// Takes one `node.rx` record.
    pub fn on_rx(&mut self, v: &NodeRxView) {
        let log = self.logs.entry(v.rx.index()).or_default();
        if v.outcome == RxFate::Lost && v.cause.as_deref().is_some_and(|c| UNDETECTED.contains(&c))
        {
            log.undetected += 1;
            return;
        }
        if log.received.len() >= MAX_PER_NODE {
            log.received.pop_front();
            log.shed[1] += 1;
        }
        log.received.push_back(RxEntry::of(v));
    }

    /// Ends a projected step: taps whose record never came are dropped.
    pub fn end_step(&mut self) {
        self.pending_taps.clear();
    }

    /// Forgets everything that ended before `before`.
    pub fn prune(&mut self, before: SimTime) {
        for log in self.logs.values_mut() {
            while log
                .sent
                .front()
                .and_then(|m| self.frames.get(m))
                .is_some_and(|f| f.t < before)
            {
                log.sent.pop_front();
            }
            while log.received.front().is_some_and(|r| r.t < before) {
                log.received.pop_front();
            }
        }
        // A frame is kept while anything could still refer to it: its sender's log, or a
        // reception resolved after `before` (which is always later than its transmission).
        let horizon = before.saturating_sub(5_000_000_000);
        self.frames.retain(|_, f| f.t >= horizon);
    }

    /// How many frames and receptions the store holds, for a memory figure.
    pub fn size(&self) -> (usize, usize) {
        (
            self.frames.len(),
            self.logs.values().map(|l| l.received.len()).sum(),
        )
    }

    /// The node's sent frames with `after < t <= now`, oldest first.
    fn sent_between(&self, node: u32, after: Option<SimTime>, now: SimTime) -> Vec<&TxFrame> {
        let Some(log) = self.logs.get(&node) else {
            return Vec::new();
        };
        log.sent
            .iter()
            .filter_map(|m| self.frames.get(m))
            .filter(|f| f.t <= now && after.is_none_or(|a| f.t > a))
            .collect()
    }

    /// The node's receptions with `after < t <= now`, oldest first.
    fn received_between(&self, node: u32, after: Option<SimTime>, now: SimTime) -> Vec<&RxEntry> {
        let Some(log) = self.logs.get(&node) else {
            return Vec::new();
        };
        log.received
            .iter()
            .filter(|r| r.t <= now && after.is_none_or(|a| r.t > a))
            .collect()
    }

    /// One sent frame as the feed carries it.
    pub fn sent_json(&self, f: &TxFrame, bytes: bool) -> Value {
        let ms = |a: Option<SimTime>, b: Option<SimTime>| match (a, b) {
            (Some(a), Some(b)) if b >= a => Some(((b - a) as f64) / 1e6),
            _ => None,
        };
        let mut out = json!({
            "msg": f.msg,
            "t_ns": f.t,
            "type": f.msg_type,
            "bytes": {
                "on_wire": f.bytes_on_wire,
                "payload": f.layers[0],
                "envelope": f.layers[1],
                "certificate": f.layers[2],
                "network": f.layers[3],
                "link": f.layers[4],
            },
            "radio": {"power_dbm": f.power_dbm, "channel": f.channel, "airtime_us": f.airtime_us},
            "signer": f.signer,
            "pseudonym": f.pseudonym.map(|p| decode::hex(&p)),
            "timing": {
                "generated_ns": f.t_generated,
                "sign_start_ns": f.t_sign_start,
                "signed_ns": f.t_signed,
                "on_air_ns": f.t,
                "sign_queue_ms": ms(f.t_generated, f.t_sign_start),
                "sign_ms": ms(f.t_sign_start, f.t_signed),
                "channel_access_ms": ms(f.t_signed, Some(f.t)),
            },
        });
        match &f.spdu {
            Some(spdu) => out["decoded"] = decode::decode_frame(spdu, f.msg_type, bytes),
            None => {
                out["decoded"] = json!({
                    "note": "the engine sized this frame from a protocol table and built no octets, so there is nothing to decode",
                });
            }
        }
        out
    }

    /// One reception as the feed carries it. `gt` keeps the ground-truth fields (the true
    /// sender and the distance); a `node`-profile connection gets them removed (§5.2).
    pub fn received_json(&self, node: u32, r: &RxEntry, bytes: bool, gt: bool) -> Value {
        let view = r.view(node);
        let stages: serde_json::Map<String, Value> = view
            .latency_trace()
            .map(|t| {
                t.stage_ns()
                    .into_iter()
                    .map(|(k, ns)| (k, json!((ns as f64) / 1e6)))
                    .collect()
            })
            .unwrap_or_default();
        let mut out = json!({
            "msg": r.msg,
            "t_ns": r.t,
            "type": r.msg_type,
            "outcome": match r.outcome { RxFate::Delivered => "delivered", RxFate::Lost => "lost", RxFate::InFlight => "in-flight" },
            "cause": r.cause,
            "verification": r.verified.map(|v| if v { "verified" } else { "unverified" }),
            "rssi_dbm": r.rssi_dbm,
            "sinr_db": r.sinr_db,
            "bytes_on_wire": r.bytes_on_wire,
            "e2e_ms": view.e2e_ns().map(|ns| (ns as f64) / 1e6),
            "stages_ms": stages,
        });
        if gt {
            out["from"] = json!(r.from);
            out["dist_m"] = json!(r.dist_m);
        }
        // What the receiver read: only a delivered message's octets reached it.
        if r.outcome == RxFate::Delivered
            && let Some(frame) = r.msg.and_then(|m| self.frames.get(&m))
            && let Some(spdu) = &frame.spdu
        {
            out["decoded"] = decode::decode_frame(spdu, frame.msg_type, bytes);
        }
        out
    }

    /// The node's queues at `now`, reconstructed from its own stamps (see the module
    /// header), with the node's own telemetry window beside them.
    pub fn queues_json(
        &self,
        node: u32,
        now: SimTime,
        reported: Option<&v2xw_record::wire::telemetry::NodeTelemetry>,
        waiting_limit: usize,
    ) -> Value {
        let mut qs: BTreeMap<&'static str, QueueTally> = BTreeMap::new();
        for id in QUEUES {
            qs.insert(id.0, QueueTally::default());
        }
        let clock = Clock {
            now,
            win_lo: now.saturating_sub(QUEUE_WINDOW_NS),
        };
        let drop_lo = now.saturating_sub(DROP_WINDOW_NS);
        if let Some(log) = self.logs.get(&node) {
            for r in &log.received {
                let s = &r.stamps;
                let who = Who {
                    msg: r.msg,
                    ty: r.msg_type,
                    from: r.from,
                };
                if r.outcome == RxFate::Lost
                    && r.t > drop_lo
                    && r.t <= now
                    && let Some(c) = r.cause
                {
                    let q = match c {
                        rx_cause::VERIFY_OVERFLOW
                        | rx_cause::VERIFY_POLICY_DROP
                        | rx_cause::SIGNATURE_INVALID
                        | rx_cause::REVOKED => Some("verify"),
                        rx_cause::RX_OVERFLOW
                        | rx_cause::REASSEMBLY_FAILED
                        | rx_cause::RECEIVER_OFF => Some("rx"),
                        _ => None,
                    };
                    if let Some(q) = q.and_then(|q| qs.get_mut(q)) {
                        *q.drops.entry(c).or_insert(0) += 1;
                    }
                }
                if s[S_ARRIVAL].is_none() {
                    continue;
                }
                // A stage the message never finished ends at its resolution instant.
                let parsed = s[S_RX_DONE].or((r.outcome == RxFate::Lost).then_some(r.t));
                clock.stage(
                    &mut qs,
                    "rx",
                    s[S_ARRIVAL],
                    parsed,
                    false,
                    &who,
                    "awaiting parse",
                );
                if s[S_VERIFY_START].is_some() {
                    clock.stage(
                        &mut qs,
                        "verify",
                        s[S_RX_DONE],
                        s[S_VERIFY_START],
                        false,
                        &who,
                        "awaiting verification",
                    );
                    clock.stage(
                        &mut qs,
                        "verify",
                        s[S_VERIFY_START],
                        Some(s[S_VERIFY_DONE].unwrap_or(r.t)),
                        true,
                        &who,
                        "verifying",
                    );
                } else if r.outcome == RxFate::Lost && s[S_RX_DONE].is_some() {
                    // Dropped from the verification queue: overflow, eviction or policy.
                    clock.stage(
                        &mut qs,
                        "verify",
                        s[S_RX_DONE],
                        Some(r.t),
                        false,
                        &who,
                        "awaiting verification",
                    );
                }
                if r.outcome == RxFate::Delivered {
                    let ready = s[S_VERIFY_DONE].or(s[S_RX_DONE]);
                    clock.stage(
                        &mut qs,
                        "app",
                        ready,
                        s[S_DELIVERED],
                        false,
                        &who,
                        "awaiting the application",
                    );
                }
            }
            for f in log.sent.iter().filter_map(|m| self.frames.get(m)) {
                let Some(g) = f.t_generated else { continue };
                let stage_name = match (f.t_sign_start, f.t_signed) {
                    (Some(s0), _) if now < s0 => "awaiting the signer",
                    (_, Some(s1)) if now < s1 => "signing",
                    _ => "awaiting channel access",
                };
                let who = Who {
                    msg: Some(f.msg),
                    ty: f.msg_type,
                    from: None,
                };
                clock.stage(&mut qs, "tx", Some(g), Some(f.t), false, &who, stage_name);
            }
        }

        let reported_depth = |p50: u16, p95: u16| {
            (p50 != u16::MAX || p95 != u16::MAX).then(|| {
                json!({"p50": (p50 != u16::MAX).then_some(p50),
                       "p95": (p95 != u16::MAX).then_some(p95)})
            })
        };
        let rep = |id: &str| -> Option<Value> {
            let t = reported?;
            match id {
                "rx" => reported_depth(t.q_rx_p50, t.q_rx_p95),
                "verify" => reported_depth(t.q_verify_p50, t.q_verify_p95),
                "app" => reported_depth(t.q_app_p50, t.q_app_p95),
                "tx" => reported_depth(t.q_tx_p50, t.q_tx_p95),
                "crl" => reported_depth(t.q_crl_p50, t.q_crl_p95),
                _ => None,
            }
        };
        let mut list: Vec<Value> = Vec::new();
        for (id, label, what) in QUEUES {
            let Some(q) = qs.get_mut(id) else { continue };
            q.waiting.sort_by(|a, b| a.0.cmp(&b.0));
            let depth = q.waiting.len();
            let shown: Vec<Value> = q
                .waiting
                .iter()
                .take(waiting_limit)
                .map(|(_, v)| v.clone())
                .collect();
            let mut row = json!({
                "id": id,
                "label": label,
                "what": what,
                "depth": if id == "crl" { Value::Null } else { json!(depth) },
                "in_service": q.in_service,
                "served": q.served,
                "wait_p50_ms": percentile_ms(&mut q.waits_ns, 0.50),
                "wait_p95_ms": percentile_ms(&mut q.waits_ns, 0.95),
                "drops": q.drops,
                "waiting": shown,
                "waiting_omitted": depth.saturating_sub(waiting_limit),
                "reported_depth": rep(id),
            });
            if id == "tx" {
                row["drops_note"] = json!(
                    "a frame the transmit queue refuses is on no channel, so transmit drops are \
                     not observable here"
                );
            }
            list.push(row);
        }
        json!({
            "t_ns": now,
            "window_ms": QUEUE_WINDOW_NS / 1_000_000,
            "drop_window_ms": DROP_WINDOW_NS / 1_000_000,
            "source": "reconstructed from the node's own node.tx and node.rx stamps; \
                       reported_depth is the node's own telemetry window",
            "list": list,
        })
    }

    /// The feed for `node` at `now`: what is new since `after` (everything kept when
    /// `after` is `None`), newest first up to the limits, with the counts left out.
    pub fn feed_json(
        &self,
        node: u32,
        after: Option<SimTime>,
        now: SimTime,
        limits: &FeedLimits,
        gt: bool,
        reported: Option<&v2xw_record::wire::telemetry::NodeTelemetry>,
    ) -> Value {
        let sent = self.sent_between(node, after, now);
        let received = self.received_between(node, after, now);
        let pick_sent: Vec<Value> = sent
            .iter()
            .rev()
            .take(limits.sent)
            .map(|f| self.sent_json(f, limits.bytes))
            .collect();
        let pick_received: Vec<Value> = received
            .iter()
            .rev()
            .take(limits.received)
            .map(|r| self.received_json(node, r, limits.bytes, gt))
            .collect();
        let log = self.logs.get(&node);
        json!({
            "v": FEED_VERSION,
            "node": node,
            "t_ns": now,
            "since_ns": after,
            "sent": pick_sent,
            "received": pick_received,
            "omitted": {
                "sent": sent.len().saturating_sub(limits.sent),
                "received": received.len().saturating_sub(limits.received),
            },
            "shed": {"sent": log.map_or(0, |l| l.shed[0]), "received": log.map_or(0, |l| l.shed[1])},
            "undetected": log.map_or(0, |l| l.undetected),
            "history_ns": HISTORY_NS,
            "queues": self.queues_json(node, now, reported, limits.waiting),
        })
    }

    /// `inspect.node`'s `messages` section: the node's recent traffic, oldest first, in
    /// the shape the section has always had plus the decoded content.
    pub fn messages_json(&self, node: u32, now: SimTime, limit: usize, gt: bool) -> Option<Value> {
        self.logs.get(&node)?;
        let sent = self.sent_between(node, None, now);
        let received = self.received_between(node, None, now);
        let skip_s = sent.len().saturating_sub(limit);
        let skip_r = received.len().saturating_sub(limit);
        let sent: Vec<Value> = sent[skip_s..]
            .iter()
            .map(|f| legacy_sent(self.sent_json(f, false)))
            .collect();
        let received: Vec<Value> = received[skip_r..]
            .iter()
            .map(|r| legacy_received(self.received_json(node, r, false, gt)))
            .collect();
        Some(json!({"sent": sent, "received": received}))
    }
}

/// The `messages` section's original member names for a sent frame, beside the new ones.
fn legacy_sent(mut v: Value) -> Value {
    let b = v["bytes"].clone();
    let t = v["timing"].clone();
    let r = v["radio"].clone();
    let obj = v.as_object_mut().expect("an object");
    obj.insert("msg_type".into(), obj["type"].clone());
    obj.insert("bytes_on_wire".into(), b["on_wire"].clone());
    obj.insert("payload_bytes".into(), b["payload"].clone());
    obj.insert("envelope_bytes".into(), b["envelope"].clone());
    obj.insert("cert_bytes".into(), b["certificate"].clone());
    obj.insert("net_header_bytes".into(), b["network"].clone());
    obj.insert("link_bytes".into(), b["link"].clone());
    obj.insert("airtime_us".into(), r["airtime_us"].clone());
    obj.insert("power_dbm".into(), r["power_dbm"].clone());
    obj.insert("channel".into(), r["channel"].clone());
    obj.insert("t_generated_ns".into(), t["generated_ns"].clone());
    obj.insert("t_signed_ns".into(), t["signed_ns"].clone());
    // The one-line summary the log printed, from the decoded fields.
    let mut content = serde_json::Map::new();
    if let Some(fields) = v["decoded"]["message"]["fields"].as_array() {
        for f in fields {
            let (Some(k), val) = (f["k"].as_str(), &f["v"]) else {
                continue;
            };
            let key = match k {
                "msg_cnt" => "msg_count",
                "temp_id" => "temp_id",
                "sec_mark" => "sec_mark_ms",
                "lat" => "lat_deg",
                "lon" => "lon_deg",
                "elev" => "elev_m",
                "speed" => "speed_mps",
                "heading" => "heading_deg",
                "part_ii" => "part_ii",
                _ => continue,
            };
            if !val.is_null() {
                content.insert(key.into(), val.clone());
            }
        }
    }
    v["content"] = Value::Object(content);
    v
}

/// The `messages` section's original member names for a reception.
fn legacy_received(mut v: Value) -> Value {
    let obj = v.as_object_mut().expect("an object");
    obj.insert("msg_type".into(), obj["type"].clone());
    v
}

/// The five queues of 03-interfaces.md §8 (`Task{queue: Rx|Verify|App|Tx|Crl}`), with what
/// each one's interval is.
const QUEUES: [(&str, &str, &str); 5] = [
    ("rx", "Receive", "from arrival to the end of the parse"),
    (
        "verify",
        "Verification",
        "from the parse to the start of the signature check; in service = being checked",
    ),
    (
        "app",
        "Application",
        "from the verdict to delivery to the applications",
    ),
    (
        "tx",
        "Transmit",
        "from generation to the air: the signing queue, the signer and channel access",
    ),
    (
        "crl",
        "CRL tasks",
        "no per-task stamps are published for CRL work; the depth is the node's own telemetry window",
    ),
];

/// One queue's reconstruction at an instant.
#[derive(Default)]
struct QueueTally {
    waiting: Vec<(SimTime, Value)>,
    in_service: usize,
    waits_ns: Vec<u64>,
    served: usize,
    drops: BTreeMap<&'static str, u64>,
}

/// A message, as a waiting-entry row names it.
struct Who {
    msg: Option<u64>,
    ty: &'static str,
    from: Option<u32>,
}

/// The instant a reconstruction is for, and the start of its wait window.
struct Clock {
    now: SimTime,
    win_lo: SimTime,
}

impl Clock {
    /// One stage interval `[a, b)` of one message: waiting (or in service) at `now`, and a
    /// served wait when it ended inside the window.
    #[allow(clippy::too_many_arguments)]
    fn stage(
        &self,
        qs: &mut BTreeMap<&'static str, QueueTally>,
        q: &str,
        a: Option<SimTime>,
        b: Option<SimTime>,
        service: bool,
        who: &Who,
        stage: &str,
    ) {
        let (Some(a), Some(b)) = (a, b) else { return };
        let Some(q) = qs.get_mut(q) else { return };
        if a <= self.now && self.now < b {
            if service {
                q.in_service += 1;
            } else {
                q.waiting.push((
                    a,
                    json!({"msg": who.msg, "type": who.ty, "from": who.from, "enqueued_ns": a,
                           "waited_ms": ((self.now - a) as f64) / 1e6, "stage": stage}),
                ));
            }
        }
        if !service && b <= self.now && b > self.win_lo && b >= a {
            q.waits_ns.push(b - a);
            q.served += 1;
        }
    }
}

/// The `q` quantile of a set of waits, ms (nearest rank).
fn percentile_ms(v: &mut [u64], q: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let rank = ((v.len() as f64) * q).ceil() as usize;
    let i = rank.saturating_sub(1).min(v.len() - 1);
    Some((v[i] as f64) / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn rx(
        msg: u64,
        outcome: RxFate,
        cause: Option<&str>,
        stamps: [Option<u64>; 10],
        t: u64,
    ) -> NodeRxView {
        NodeRxView {
            t,
            rx: NodeId::new(7),
            tx: Some(NodeId::new(3)),
            msg: Some(msg),
            msg_type: Some("bsm".to_string()),
            outcome,
            cause: cause.map(str::to_string),
            verification: (outcome == RxFate::Delivered).then(|| "verified".to_string()),
            rssi_dbm: Some(-80.0),
            sinr_db: Some(12.0),
            dist_m: Some(40.0),
            bytes_on_wire: Some(176),
            airtime_us: Some(300),
            payload_bytes: Some(40),
            t_generated: stamps[0],
            t_sign_start: stamps[1],
            t_signed: stamps[2],
            mac_aifs_ns: Some(58_000),
            mac_backoff_ns: Some(0),
            t_tx_start: stamps[3],
            t_tx_end: stamps[4],
            t_arrival: stamps[5],
            t_rx_done: stamps[6],
            t_verify_start: stamps[7],
            t_verify_done: stamps[8],
            t_delivered: stamps[9],
        }
    }

    fn queue<'a>(v: &'a Value, id: &str) -> &'a Value {
        v["list"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|q| q["id"] == id)
            .expect("the queue")
    }

    /// A message delivered through every stage: arrival 100 ms, parsed 101, verification
    /// 105–106, delivered 107.
    fn one_delivery() -> FeedStore {
        let mut store = FeedStore::new();
        let s = |x: u64| Some(x * MS);
        store.on_rx(&rx(
            1,
            RxFate::Delivered,
            None,
            [
                s(90),
                s(91),
                s(93),
                s(99),
                Some(99 * MS + 300_000),
                s(100),
                s(101),
                s(105),
                s(106),
                s(107),
            ],
            107 * MS,
        ));
        store
    }

    #[test]
    fn a_message_is_in_exactly_one_queue_at_each_instant_of_its_journey() {
        let store = one_delivery();
        // (now, the queue it is waiting in, whether it is being verified)
        let cases: [(u64, Option<&str>, bool); 9] = [
            (99 * MS, None, false),
            (100 * MS, Some("rx"), false),
            (100 * MS + 500_000, Some("rx"), false),
            (101 * MS, Some("verify"), false),
            (104 * MS, Some("verify"), false),
            (105 * MS, None, true),
            (106 * MS, Some("app"), false),
            // At the delivery instant the message has left every queue: the intervals are
            // half-open, so no instant counts one message twice.
            (107 * MS, None, false),
            (200 * MS, None, false),
        ];
        for (now, waiting_in, verifying) in cases {
            let q = store.queues_json(7, now, None, 8);
            for id in ["rx", "verify", "app", "tx"] {
                let expect = u64::from(Some(id) == waiting_in);
                assert_eq!(
                    queue(&q, id)["depth"].as_u64(),
                    Some(expect),
                    "at {} µs the `{id}` queue: {q}",
                    now / 1000
                );
            }
            assert_eq!(
                queue(&q, "verify")["in_service"].as_u64(),
                Some(u64::from(verifying)),
                "at {} µs",
                now / 1000
            );
        }
        let q = store.queues_json(7, 104 * MS, None, 8);
        let w = &queue(&q, "verify")["waiting"][0];
        assert_eq!(w["enqueued_ns"].as_u64(), Some(101 * MS));
        assert_eq!(w["waited_ms"].as_f64(), Some(3.0));
        assert_eq!(w["from"].as_u64(), Some(3));
        assert_eq!(w["msg"].as_u64(), Some(1));
    }

    #[test]
    fn waits_are_the_served_intervals_inside_the_window() {
        let store = one_delivery();
        let q = store.queues_json(7, 500 * MS, None, 8);
        assert_eq!(queue(&q, "rx")["wait_p50_ms"].as_f64(), Some(1.0));
        assert_eq!(queue(&q, "verify")["wait_p95_ms"].as_f64(), Some(4.0));
        assert_eq!(queue(&q, "app")["wait_p50_ms"].as_f64(), Some(1.0));
        assert_eq!(queue(&q, "verify")["served"].as_u64(), Some(1));
        // A second later the window has moved past it.
        let q = store.queues_json(7, 1_200 * MS, None, 8);
        assert_eq!(queue(&q, "verify")["served"].as_u64(), Some(0));
        assert!(queue(&q, "verify")["wait_p50_ms"].is_null());
    }

    #[test]
    fn a_verification_overflow_is_a_drop_of_the_verification_queue() {
        let mut store = FeedStore::new();
        let s = |x: u64| Some(x * MS);
        store.on_rx(&rx(
            2,
            RxFate::Lost,
            Some(rx_cause::VERIFY_OVERFLOW),
            [s(1), s(1), s(2), s(3), s(3), s(4), s(5), None, None, None],
            9 * MS,
        ));
        // Waiting from its parse until it was dropped.
        let q = store.queues_json(7, 7 * MS, None, 8);
        assert_eq!(queue(&q, "verify")["depth"].as_u64(), Some(1));
        let q = store.queues_json(7, 50 * MS, None, 8);
        assert_eq!(
            queue(&q, "verify")["drops"][rx_cause::VERIFY_OVERFLOW].as_u64(),
            Some(1)
        );
        assert_eq!(
            queue(&q, "rx")["drops"]
                .as_object()
                .map(serde_json::Map::len),
            Some(0)
        );
    }

    #[test]
    fn an_undetected_frame_is_counted_and_not_listed() {
        let mut store = FeedStore::new();
        store.on_rx(&rx(
            3,
            RxFate::Lost,
            Some("below-sensitivity"),
            [None; 10],
            MS,
        ));
        let feed = store.feed_json(7, None, 10 * MS, &FeedLimits::default(), true, None);
        assert_eq!(feed["received"].as_array().map(Vec::len), Some(0));
        assert_eq!(feed["undetected"].as_u64(), Some(1));
    }

    #[test]
    fn a_transmitted_frame_waits_for_the_signer_then_for_the_channel() {
        let mut store = FeedStore::new();
        let tx: NodeTxView = serde_json::from_value(json!({
            "t": 5 * MS, "node": 7, "msg": 11, "msg_type": "bsm", "bytes_on_wire": 176,
            "t_generated": 0, "t_sign_start": MS, "t_signed": 3 * MS,
        }))
        .expect("a node.tx view");
        store.on_tx(&tx);
        store.end_step();
        let stage_at = |now: u64| -> Option<String> {
            let q = store.queues_json(7, now, None, 8);
            queue(&q, "tx")["waiting"][0]["stage"]
                .as_str()
                .map(str::to_string)
        };
        assert_eq!(stage_at(MS / 2).as_deref(), Some("awaiting the signer"));
        assert_eq!(stage_at(2 * MS).as_deref(), Some("signing"));
        assert_eq!(stage_at(4 * MS).as_deref(), Some("awaiting channel access"));
        assert_eq!(stage_at(5 * MS), None, "on the air at 5 ms");
        let q = store.queues_json(7, 100 * MS, None, 8);
        assert_eq!(queue(&q, "tx")["wait_p50_ms"].as_f64(), Some(5.0));
        // A frame with no octets says so rather than decoding nothing.
        let feed = store.feed_json(7, None, 10 * MS, &FeedLimits::default(), true, None);
        assert!(feed["sent"][0]["decoded"]["note"].is_string(), "{feed}");
    }

    #[test]
    fn the_ground_truth_members_are_left_out_for_a_node_profile() {
        let store = one_delivery();
        let gt = store.feed_json(7, None, 200 * MS, &FeedLimits::default(), true, None);
        let node = store.feed_json(7, None, 200 * MS, &FeedLimits::default(), false, None);
        assert_eq!(gt["received"][0]["from"].as_u64(), Some(3));
        assert_eq!(gt["received"][0]["dist_m"].as_f64(), Some(40.0));
        assert!(node["received"][0].get("from").is_none());
        assert!(node["received"][0].get("dist_m").is_none());
        assert_eq!(node["received"][0]["outcome"], "delivered");
    }
}
