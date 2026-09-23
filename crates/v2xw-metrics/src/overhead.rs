//! Communication overhead: what fraction of what goes on the air is not the application's.
//!
//! A frame on the air is the application payload plus, in order outward, the security
//! envelope, a network and transport header, LLC/SNAP, the MAC header and the FCS
//! (`v2xw_net::frame`). `node.tx` carries that split for every frame, and the byte-accounting
//! channels carry every byte that left the air — cellular uplink and downlink, backhaul,
//! backend — in exactly one bucket. This module reduces both:
//!
//! | Metric | Formula | Unit |
//! |---|---|---|
//! | `security_overhead` | Σ envelope octets / Σ octets on the air | ratio of sums, per message type |
//! | `net_header_overhead` | Σ network + transport header octets / Σ octets on the air | ratio of sums, per message type |
//! | `link_overhead` | Σ LLC/SNAP + MAC header + FCS octets / Σ octets on the air | ratio of sums, per message type |
//! | `cert_bytes_share` | Σ octets of attached certificates / Σ envelope octets — certificate versus digest | ratio of sums, per message type |
//! | `air_bytes_per_payload_byte` | Σ octets on the air / Σ application payload octets | ratio of sums, per message type |
//! | `bytes_per_vehicle_hour` | octets in one accounting bucket / equipped-vehicle hours in the window | B/(veh·h), per bucket |
//!
//! `security_overhead + net_header_overhead + link_overhead + payload share = 1` for the
//! frames whose split is known; `overhead::tests::the_shares_of_a_frame_add_up_to_one` holds
//! it. The per-second totals per bucket (`bytes_air`, `bytes_uu_ul`, …) and their sum
//! `bytes_total` are [`crate::comms`]'s.

use std::collections::BTreeMap;

use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::ids::ActorId;
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;

use crate::cards;
use crate::channels::{
    ByteBucket, ChannelView, GtKinematicsView, NetBytesView, NodeTxView, ProtoMsgView, decode,
};
use crate::def::{Agg, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::MetricProvider;
use crate::quant::Quantum;
use crate::stats::{Estimate, ratio_of_sums};

/// A gap in one vehicle's `gt.kinematics` longer than this is not integrated as time on
/// the road: the vehicle despawned and a later record is a new trip.
const KINEMATICS_GAP_NS: u64 = 2_000_000_000;

/// Byte sums over the frames of one message type in one window.
#[derive(Debug, Default, Clone, Copy)]
struct Sums {
    /// Every frame's octets on the air.
    air: u64,
    /// The octets on the air of the frames whose payload/envelope split is known.
    air_split: u64,
    payload: u64,
    envelope: u64,
    net: u64,
    link: u64,
    cert: u64,
    frames: u64,
    frames_split: u64,
}

/// The overhead provider.
pub struct OverheadProvider {
    card: ModelCard,
    window_start: SimTime,
    /// Keyed by message type, with `None` the all-types aggregate.
    sums: BTreeMap<Option<String>, Sums>,
    buckets: BTreeMap<ByteBucket, u64>,
    /// Equipped-vehicle time in the window, ns.
    vehicle_ns: u64,
    last_seen: BTreeMap<ActorId, SimTime>,
    rejected: u64,
}

impl OverheadProvider {
    /// A provider whose first window starts at `t0`.
    #[must_use]
    pub fn new(t0: SimTime) -> Self {
        Self {
            card: Self::build_card(),
            window_start: t0,
            sums: BTreeMap::new(),
            buckets: BTreeMap::new(),
            vehicle_ns: 0,
            last_seen: BTreeMap::new(),
            rejected: 0,
        }
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/overhead/communication",
            "1.0.0",
            "Security, network-header and link-layer overhead as shares of the octets on \
             the air, the certificate's share of the security envelope, octets on the air \
             per payload octet, and octets per equipped-vehicle hour in every accounting \
             bucket.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "security_overhead",
                "Σ envelope / Σ bytes_on_wire over frames whose split is known",
            ),
            v2xw_core::card::Equation::new(
                "bytes_per_vehicle_hour",
                "bytes(bucket) / (Σ equipped-vehicle time in the window / 3600 s)",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.sources = vec![
            cards::design("04-models.md §4.6, §7 and §9.3 (the frame's layers and their sizes)"),
            cards::design("03-interfaces.md §5, invariant I-N1 (every byte in exactly one bucket)"),
            cards::standard(
                "IEEE 1609.2-2022 §6.3.4 (SignerIdentifier: a certificate or its HashedId8 \
                 digest), the choice cert_bytes_share measures the cost of",
            ),
        ];
        card.limitations = vec![
            "Frames the engine sized from a protocol wire table (misbehaviour reports, CRL \
             broadcasts) have no payload/envelope split; they count in the octets on the air \
             and in the network and link overheads, and not in the security overhead or the \
             certificate share."
                .to_string(),
            "Vehicle hours integrate gt.kinematics between successive records of one equipped \
             vehicle; a vehicle's first record contributes no time, so a window with churn \
             slightly understates the hours."
                .to_string(),
        ];
        card.ignores =
            vec!["The PHY preamble and tail, which are air time, not octets.".to_string()];
        card.validation.tests = vec![
            "overhead::tests::the_shares_of_a_frame_add_up_to_one".to_string(),
            "overhead::tests::bytes_per_vehicle_hour_divides_by_equipped_time".to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let layers = cards::design("04-models.md §4.6, §7, §9.3");
        let share = |name: &str, what: &str, num: &str, den: &str, def: &str| {
            MetricDef::new(
                name,
                "ratio",
                Agg::ratio(num.to_string(), den.to_string()),
                Visibility::Node,
                Quantum::RATIO,
                def.to_string(),
            )
            .with_dims([Dim::T, Dim::MsgType])
            .with_breakdown(Dim::MsgType, ["bsm", "cam"])
            .with_source(layers.clone())
            .with_min_samples(1)
            .with_range(0.0, 1.0)
            .not_accounting_for(what.to_string())
            .not_accounting_for("the PHY preamble and tail, which are air time")
        };
        vec![
            share(
                "security_overhead",
                "frames whose envelope was sized from a table rather than built",
                "Σ envelope octets",
                "Σ octets on the air",
                "The share of the octets on the air that are the security envelope — \
                 headers, signer identifier (certificate or digest) and signature — over the \
                 frames whose payload/envelope split is known.",
            ),
            share(
                "net_header_overhead",
                "the LLC/SNAP octets, which are in link_overhead",
                "Σ network and transport header octets",
                "Σ octets on the air",
                "The share of the octets on the air that are the network and transport \
                 header: WSMP for the US stack, GeoNetworking plus BTP for the European one.",
            ),
            share(
                "link_overhead",
                "the 802.11 PLCP header, which is air time and not octets",
                "Σ LLC/SNAP, MAC header and FCS octets",
                "Σ octets on the air",
                "The share of the octets on the air that are the link layer: LLC/SNAP, the \
                 802.11 QoS Data MAC header and the frame check sequence.",
            ),
            share(
                "cert_bytes_share",
                "peer-to-peer certificate responses outside ordinary messages",
                "Σ attached-certificate octets",
                "Σ envelope octets",
                "The share of the security envelope's octets that are attached certificates \
                 rather than eight-octet digests — the price of the certificate-attachment \
                 policy.",
            ),
            MetricDef::new(
                "air_bytes_per_payload_byte",
                "B/B",
                Agg::ratio("Σ octets on the air", "Σ application payload octets"),
                Visibility::Node,
                Quantum::RATIO,
                "Octets on the air per octet of application payload: how many bytes the air \
                 carries for each byte the application wanted to send.",
            )
            .with_dims([Dim::T, Dim::MsgType])
            .with_source(layers.clone())
            .with_min_samples(1)
            .with_range(1.0, f64::INFINITY)
            .not_accounting_for("frames with no payload/envelope split")
            .not_accounting_for("the PHY preamble and tail, which are air time"),
            MetricDef::new(
                "bytes_per_vehicle_hour",
                "B/(veh·h)",
                Agg::ratio("octets in the bucket", "equipped-vehicle hours"),
                Visibility::NodeAndGt,
                Quantum::BYTES,
                "Octets in one accounting bucket — air, cellular uplink, cellular downlink, \
                 backhaul, backend — per hour of equipped-vehicle time on the road in the \
                 window. Every byte is in exactly one bucket (invariant I-N1).",
            )
            .with_dims([Dim::T, Dim::Bucket])
            .with_breakdown(Dim::Bucket, ByteBucket::ALL.iter().map(|b| b.as_str()))
            .with_source(cards::design("03-interfaces.md §5 (I-N1)"))
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("roadside units' own time, which is not vehicle time")
            .not_accounting_for("a vehicle's first record, which contributes no time"),
        ]
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    fn on_tx(&mut self, v: &NodeTxView) {
        *self.buckets.entry(ByteBucket::Air).or_insert(0) += v.bytes_on_wire;
        for key in [None, v.msg_type.clone()] {
            let s = self.sums.entry(key).or_default();
            s.air += v.bytes_on_wire;
            s.frames += 1;
            s.net += v.net_header_bytes.unwrap_or(0);
            s.link += v.link_bytes.unwrap_or(0);
            if let (Some(p), Some(e)) = (v.payload_bytes, v.envelope_bytes) {
                s.air_split += v.bytes_on_wire;
                s.payload += p;
                s.envelope += e;
                s.cert += v.cert_bytes.unwrap_or(0);
                s.frames_split += 1;
            }
            if v.msg_type.is_none() {
                break;
            }
        }
    }

    fn on_kinematics(&mut self, v: &GtKinematicsView) {
        if v.node.is_none() {
            return;
        }
        if let Some(prev) = self.last_seen.insert(v.actor, v.t)
            && v.t > prev
            && v.t - prev <= KINEMATICS_GAP_NS
        {
            self.vehicle_ns += v.t - prev;
        }
    }
}

impl Model for OverheadProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for OverheadProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![
            NodeTxView::channel_name(),
            NetBytesView::channel_name(),
            ProtoMsgView::channel_name(),
            GtKinematicsView::channel_name(),
        ]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        match ev.channel {
            NodeTxView::CHANNEL => match decode::<NodeTxView>(ev) {
                Ok(v) => self.on_tx(&v),
                Err(_) => self.rejected += 1,
            },
            NetBytesView::CHANNEL => match decode::<NetBytesView>(ev) {
                Ok(v) => *self.buckets.entry(v.bucket).or_insert(0) += v.bytes_on_wire,
                Err(_) => self.rejected += 1,
            },
            ProtoMsgView::CHANNEL => match decode::<ProtoMsgView>(ev) {
                Ok(v) => {
                    if let Some(b) = v.transport {
                        *self.buckets.entry(b).or_insert(0) += v.bytes_on_wire;
                    }
                }
                Err(_) => self.rejected += 1,
            },
            GtKinematicsView::CHANNEL => match decode::<GtKinematicsView>(ev) {
                Ok(v) => self.on_kinematics(&v),
                Err(_) => self.rejected += 1,
            },
            _ => {}
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();
        let (sec, net, link, cert, per_payload) = (
            self.def("security_overhead"),
            self.def("net_header_overhead"),
            self.def("link_overhead"),
            self.def("cert_bytes_share"),
            self.def("air_bytes_per_payload_byte"),
        );
        for (msg_type, s) in core::mem::take(&mut self.sums) {
            let mut dims = Dims::new();
            if let Some(t) = msg_type {
                dims.insert(Dim::MsgType, DimValue::label(t));
            }
            let r = |num: u64, den: u64, n: u64| {
                SampleValue::Ratio(ratio_of_sums(num as f64, den as f64, n, 1))
            };
            out.push(MetricSample::new(
                &sec,
                at,
                dims.clone(),
                r(s.envelope, s.air_split, s.frames_split),
            ));
            out.push(MetricSample::new(
                &net,
                at,
                dims.clone(),
                r(s.net, s.air, s.frames),
            ));
            out.push(MetricSample::new(
                &link,
                at,
                dims.clone(),
                r(s.link, s.air, s.frames),
            ));
            out.push(MetricSample::new(
                &cert,
                at,
                dims.clone(),
                r(s.cert, s.envelope, s.frames_split),
            ));
            out.push(MetricSample::new(
                &per_payload,
                at,
                dims,
                r(s.air_split, s.payload, s.frames_split),
            ));
        }

        let hours_def = self.def("bytes_per_vehicle_hour");
        let buckets = core::mem::take(&mut self.buckets);
        let vehicle_ns = core::mem::take(&mut self.vehicle_ns);
        let hours = (vehicle_ns as f64) / 3.6e12;
        for bucket in ByteBucket::ALL {
            let bytes = buckets.get(&bucket).copied().unwrap_or(0);
            let mut dims = Dims::new();
            dims.insert(Dim::Bucket, DimValue::label(bucket.as_str()));
            let value = if vehicle_ns > 0 {
                SampleValue::Ratio(ratio_of_sums(bytes as f64, hours, 1, 1))
            } else {
                SampleValue::Scalar(Estimate::Insufficient { n: 0, required: 1 })
            };
            out.push(MetricSample::new(&hours_def, at, dims, value));
        }
        // A vehicle unseen for longer than the gap is a finished trip; forgetting it keeps
        // the map bounded by the live fleet.
        self.last_seen
            .retain(|_, t| at.saturating_sub(*t) <= KINEMATICS_GAP_NS);
        self.window_start = at;
        out
    }

    fn rejected(&self) -> u64 {
        self.rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ctx::OwnedRecord;
    use v2xw_core::time::NS_PER_S;

    fn rec(channel: &'static str, json: serde_json::Value) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility: Visibility::Node,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    fn get(s: &[MetricSample], name: &str, msg_type: Option<&str>) -> f64 {
        s.iter()
            .find(|x| {
                x.metric == name
                    && x.dims.get(&Dim::MsgType) == msg_type.map(DimValue::label).as_ref()
            })
            .and_then(|x| x.value.point())
            .unwrap()
    }

    #[test]
    fn the_shares_of_a_frame_add_up_to_one() {
        let mut p = OverheadProvider::new(0);
        // A BSM: payload 40, envelope 93 (8-octet digest), WSMP 5, link 38.
        p.on_event(&rec(
            "node.tx",
            serde_json::json!({"t":1,"node":1,"msg_type":"bsm","bytes_on_wire":176,
                "payload_bytes":40,"envelope_bytes":93,"spdu_bytes":133,
                "net_header_bytes":5,"link_bytes":38,"cert_bytes":0}),
        ));
        let s = p.flush(NS_PER_S);
        let total = get(&s, "security_overhead", Some("bsm"))
            + get(&s, "net_header_overhead", Some("bsm"))
            + get(&s, "link_overhead", Some("bsm"))
            + 40.0 / 176.0;
        assert!((total - 1.0).abs() < 1e-3, "shares sum to {total}");
        assert!((get(&s, "air_bytes_per_payload_byte", None) - 4.4).abs() < 1e-3);
        assert_eq!(get(&s, "cert_bytes_share", None), 0.0);
    }

    #[test]
    fn bytes_per_vehicle_hour_divides_by_equipped_time() {
        let mut p = OverheadProvider::new(0);
        // One equipped vehicle for a second, seen every 100 ms; one unequipped one.
        for k in 0..=10u64 {
            let t = k * NS_PER_S / 10;
            p.on_event(&rec(
                "gt.kinematics",
                serde_json::json!({"t":t,"actor":1,"x_m":0.0,"y_m":0.0,"speed_mps":1.0,"node":7}),
            ));
            p.on_event(&rec(
                "gt.kinematics",
                serde_json::json!({"t":t,"actor":2,"x_m":0.0,"y_m":0.0,"speed_mps":1.0}),
            ));
        }
        p.on_event(&rec(
            "node.tx",
            serde_json::json!({"t":1,"node":7,"bytes_on_wire":1000}),
        ));
        p.on_event(&rec(
            "net.bytes",
            serde_json::json!({"t":1,"id":3,"bucket":"cellular-ul","bytes_on_wire":500}),
        ));
        let s = p.flush(NS_PER_S);
        let per_hour = |b: &str| {
            s.iter()
                .find(|x| {
                    x.metric == "bytes_per_vehicle_hour"
                        && x.dims.get(&Dim::Bucket) == Some(&DimValue::label(b))
                })
                .and_then(|x| x.value.point())
                .unwrap()
        };
        // 1000 B over one vehicle-second is 3.6 MB per vehicle-hour.
        assert_eq!(per_hour("air"), 3_600_000.0);
        assert_eq!(per_hour("cellular-ul"), 1_800_000.0);
        assert_eq!(per_hour("backend"), 0.0);
    }
}
