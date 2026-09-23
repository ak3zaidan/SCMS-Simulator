//! Network load: how full the channel is, who is filling it, and what it costs.
//!
//! `cbr` (the MAC's own busy-ratio measurement) and `airtime_per_node` live in
//! [`crate::comms`]. This module adds what a congestion study reads beside them:
//!
//! | Metric | Formula | Unit | Source |
//! |---|---|---|---|
//! | `channel_occupancy` | a node's own transmitted air time / window, across nodes | ratio (distribution) | 3GPP TS 36.214 §5.1.31 (channel occupancy ratio, CR, for LTE-V2X); ETSI TS 103 574 (the CR limit C-V2X congestion control enforces) — applied to 802.11p as the fraction of the window a node's own frames occupy |
//! | `channel_load` | (air time of every arrival at a node above the −85 dBm busy threshold + the node's own offered air time) / window | ratio (distribution; above 1 means more offered than the channel can carry) | Torrent-Moreno, Mittag, Santi, Hartenstein, "Vehicle-to-vehicle communication: fair transmit power control for safety-critical information", IEEE Trans. Veh. Technol. 58(7), 2009 (beaconing load at a location against the channel's capacity); EN 302 571 §4.2.10.1 (the −85 dBm threshold) |
//! | `offered_load` | PSDU bits handed to the MAC / window, network-wide | bit/s | — |
//! | `carried_load` | PSDU bits that went on the air / window, network-wide | bit/s | — |
//! | `loss_rate` | attempts lost for one cause / attempts resolved | ratio, per cause | 08-measurement-and-data.md §2.1 (`pdr_by_cause`), over attempts rather than over losses |
//! | `collision_rate` | attempts lost to a collision (collision, hidden terminal, resource collision) / attempts resolved | ratio | I-R3's cause list |
//! | `half_duplex_rate` | attempts lost because the receiver was transmitting / attempts resolved | ratio | IEEE 802.11p is half duplex |
//! | `mac_queue_depth` | frames waiting in a node's EDCA queues at each MAC report | count (distribution) | IEEE 802.11-2020 §10.23.2 |
//! | `mac_drops` | frames the MAC refused | count | — |
//! | `mac_access_delay` | preamble on the air − frame handed to the MAC | ms (distribution) | IEEE 802.11-2020 §10.23.2 (AIFS + backoff + deferral) |

use std::collections::BTreeMap;

use serde_json::json;
use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::cards;
use crate::channels::{ChannelView, MacCbrView, NodeRxView, NodeTxView, RxFate, decode};
use crate::def::{Agg, DEFAULT_LEVEL, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::MetricProvider;
use crate::quant::Quantum;
use crate::stats::{ConfidenceLevel, Distribution, Estimate, Proportion};

/// The received power above which an arrival makes the medium busy, dBm
/// [EN 302 571 §4.2.10.1; 04-models.md §4.5].
pub const BUSY_THRESHOLD_DBM: f64 = -85.0;

/// The loss causes that are a collision of two transmissions.
const COLLISION_CAUSES: [&str; 3] = ["collision", "hidden-terminal", "resource-collision"];

/// The network-load provider.
pub struct LoadProvider {
    card: ModelCard,
    level: ConfidenceLevel,
    min_samples: u64,
    busy_dbm: f64,
    window_start: SimTime,

    /// Own transmitted air time per node, µs.
    own_us: BTreeMap<NodeId, u64>,
    /// Arrival air time at or above the busy threshold per receiving node, µs.
    sensed_us: BTreeMap<NodeId, u64>,
    /// Offered air time per node, µs, from the MAC's reports (sent or refused).
    offered_us: BTreeMap<NodeId, u64>,
    offered_bits: u64,
    offered_reported: bool,
    carried_bits: u64,
    /// Resolved reception attempts, and losses among them per cause.
    resolved: u64,
    lost_by_cause: BTreeMap<String, u64>,
    queue_depth: Distribution,
    mac_drops: u64,
    access_ms: Distribution,
    rejected: u64,
}

impl LoadProvider {
    /// A provider with the EN 302 571 busy threshold. `t0` starts the first window.
    #[must_use]
    pub fn new(t0: SimTime) -> Self {
        Self {
            card: Self::build_card(),
            level: DEFAULT_LEVEL,
            min_samples: crate::stats::DEFAULT_MIN_SAMPLES,
            busy_dbm: BUSY_THRESHOLD_DBM,
            window_start: t0,
            own_us: BTreeMap::new(),
            sensed_us: BTreeMap::new(),
            offered_us: BTreeMap::new(),
            offered_bits: 0,
            offered_reported: false,
            carried_bits: 0,
            resolved: 0,
            lost_by_cause: BTreeMap::new(),
            queue_depth: Distribution::new(),
            mac_drops: 0,
            access_ms: Distribution::new(),
            rejected: 0,
        }
    }

    /// Sets the insufficiency threshold.
    #[must_use]
    pub const fn with_min_samples(mut self, n: u64) -> Self {
        self.min_samples = n;
        self
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/load/channel",
            "1.0.0",
            "Channel occupancy per node, channel load against capacity, offered and carried \
             load, loss rates by cause (collision and half-duplex named), MAC queue depth, \
             MAC drops and channel-access delay.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new("channel_occupancy", "CR_i = Σ own air time_i / window"),
            v2xw_core::card::Equation::new(
                "channel_load",
                "L_i = (Σ_{arrivals at i with P_rx ≥ −85 dBm} air time + offered air time_i) \
                 / window; L_i > 1 means more air time is offered around i than exists",
            ),
            v2xw_core::card::Equation::new(
                "loss_rate",
                "loss_rate(c) = attempts lost with cause c / attempts resolved",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.parameters.push(cards::param(
            "busy_threshold_dbm",
            "dBm",
            json!(BUSY_THRESHOLD_DBM),
            json!(-100.0),
            json!(-60.0),
            cards::standard("ETSI EN 302 571 §4.2.10.1 (the CBR busy threshold)"),
        ));
        card.sources = vec![
            cards::standard(
                "3GPP TS 36.214 §5.1.31 (channel occupancy ratio) and ETSI TS 103 574 (its \
                 limit in C-V2X congestion control)",
            ),
            cards::paper(
                "Torrent-Moreno, Mittag, Santi, Hartenstein, 'Vehicle-to-vehicle \
                 communication: fair transmit power control for safety-critical \
                 information', IEEE Trans. Veh. Technol. 58(7), 2009 (beaconing load against \
                 the channel's capacity)",
            ),
            cards::standard("ETSI EN 302 571 §4.2.10.1 (−85 dBm busy threshold)"),
            cards::standard("IEEE 802.11-2020 §10.23.2 (EDCA channel access)"),
        ];
        card.limitations = vec![
            "channel_load counts a neighbour's frames only if they went on the air: a frame \
             a neighbour's MAC refused never arrived anywhere, so it is in that neighbour's \
             own load and not in this node's."
                .to_string(),
            "Overlapping arrivals are summed, not merged: two frames at once are twice the \
             air time. That is the definition of offered load (and why it can exceed one); \
             cbr is the metric that merges them."
                .to_string(),
            "Without MAC reports (the abstract tier) offered load equals carried load, \
             because nothing is refused."
                .to_string(),
        ];
        card.ignores = vec!["Adjacent-channel energy.".to_string()];
        card.validation.tests = vec![
            "load::tests::channel_load_sums_sensed_and_own_air_time".to_string(),
            "load::tests::loss_rates_are_over_resolved_attempts".to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let src = cards::design("08-measurement-and-data.md §2.1");
        vec![
            MetricDef::new(
                "channel_occupancy",
                "ratio",
                Agg::Distribution,
                Visibility::Node,
                Quantum::RATIO,
                "The fraction of the window each node's own transmissions occupied the \
                 channel — the channel occupancy ratio C-V2X congestion control limits, \
                 applied to 802.11p. Distribution across the nodes that transmitted.",
            )
            .with_dims([Dim::T])
            .with_source(cards::standard("3GPP TS 36.214 §5.1.31; ETSI TS 103 574"))
            .with_min_samples(1)
            .with_range(0.0, 1.0)
            .not_accounting_for("nodes that did not transmit in the window")
            .not_accounting_for("inter-frame spacing and backoff, which are not transmitted time"),
            MetricDef::new(
                "channel_load",
                "ratio",
                Agg::Distribution,
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "Offered load against capacity, as each node sees it: the air time of every \
                 frame arriving above the −85 dBm busy threshold plus the node's own offered \
                 air time, over the window. One is a channel exactly full; above one, more \
                 is being offered than the channel can carry.",
            )
            .with_dims([Dim::T])
            .with_source(cards::paper(
                "Torrent-Moreno, Mittag, Santi, Hartenstein, IEEE Trans. Veh. Technol. \
                 58(7), 2009",
            ))
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("frames neighbours' MACs refused, which never reached the air")
            .not_accounting_for("the merging of overlaps: overlapping frames are summed"),
            MetricDef::new(
                "offered_load",
                "bit/s",
                Agg::Rate,
                Visibility::Node,
                Quantum::BYTES,
                "PSDU bits every node handed to its MAC in the window, whatever became of \
                 them, per second — network-wide.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("the PHY preamble and tail, which are air time and not bits")
            .not_accounting_for("spatial reuse: this is a network total, not one channel's load"),
            MetricDef::new(
                "carried_load",
                "bit/s",
                Agg::Rate,
                Visibility::Node,
                Quantum::BYTES,
                "PSDU bits that went on the air in the window, per second — network-wide.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("whether anyone received them")
            .not_accounting_for("spatial reuse: this is a network total, not one channel's load"),
            MetricDef::new(
                "loss_rate",
                "ratio",
                Agg::ratio("attempts lost with this cause", "attempts resolved"),
                Visibility::Node,
                Quantum::RATIO,
                "The fraction of all resolved reception attempts that were lost to one \
                 cause — a PHY cause (collision, half-duplex, below-sensitivity, fading, …) \
                 or one above it (verification overflow, invalid signature, revoked, …). \
                 Summed over causes it is one minus the delivery ratio.",
            )
            .with_dims([Dim::T, Dim::Cause])
            .with_breakdown(
                Dim::Cause,
                crate::channels::rx_cause::PHY
                    .iter()
                    .chain(crate::channels::rx_cause::ABOVE_PHY.iter())
                    .copied(),
            )
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .with_range(0.0, 1.0)
            .not_accounting_for("attempts still in flight when the run ended")
            .not_accounting_for("receivers outside the engine's candidate range"),
            MetricDef::new(
                "collision_rate",
                "ratio",
                Agg::ratio("attempts lost to a collision", "attempts resolved"),
                Visibility::Node,
                Quantum::RATIO,
                "The fraction of resolved reception attempts lost because two transmissions \
                 overlapped at the receiver (causes collision, hidden-terminal and \
                 resource-collision).",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .with_range(0.0, 1.0)
            .not_accounting_for("a frame that survived an overlap by capture, which is a success")
            .not_accounting_for("attempts still in flight when the run ended"),
            MetricDef::new(
                "half_duplex_rate",
                "ratio",
                Agg::ratio("attempts lost to half duplex", "attempts resolved"),
                Visibility::Node,
                Quantum::RATIO,
                "The fraction of resolved reception attempts lost because the receiver was \
                 itself transmitting.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .with_range(0.0, 1.0)
            .not_accounting_for("attempts still in flight when the run ended")
            .not_accounting_for("receivers outside the engine's candidate range"),
            MetricDef::new(
                "mac_queue_depth",
                "count",
                Agg::Distribution,
                Visibility::Node,
                Quantum::COUNT,
                "Frames waiting in a node's EDCA access-category queues, at each MAC report, \
                 across nodes.",
            )
            .with_dims([Dim::T])
            .with_source(cards::standard("IEEE 802.11-2020 §10.23.2"))
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("the depth between reports: it is sampled, not time-averaged")
            .not_accounting_for(
                "frames still in the node's signer, which have not reached the MAC",
            ),
            MetricDef::new(
                "mac_drops",
                "count",
                Agg::Count,
                Visibility::Node,
                Quantum::COUNT,
                "Frames the MAC refused in the window: a full access-category queue or a \
                 frame over the MSDU cap.",
            )
            .with_dims([Dim::T])
            .with_source(cards::standard("IEEE 802.11-2020 §10.23.2"))
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("frames the node itself never generated because DCC held it back"),
            MetricDef::new(
                "mac_access_delay",
                "ms",
                Agg::Distribution,
                Visibility::Node,
                Quantum::TIME_MS,
                "Channel-access delay per transmitted frame: from the frame reaching the MAC \
                 (its signature complete) to its preamble going on the air — AIFS, backoff \
                 and deferral to a busy medium.",
            )
            .with_dims([Dim::T])
            .with_source(cards::standard("IEEE 802.11-2020 §10.23.2"))
            .with_min_samples(self.min_samples)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("frames the MAC refused, which never got access")
            .not_accounting_for("signing, which happens before the MAC has the frame"),
        ]
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    fn on_tx(&mut self, v: &NodeTxView) {
        if let Some(us) = v.airtime_us {
            *self.own_us.entry(v.node).or_insert(0) += us;
        }
        self.carried_bits += v.bytes_on_wire * 8;
        if let Some(signed) = v.t_signed
            && v.t >= signed
        {
            self.access_ms.observe(((v.t - signed) as f64) / 1e6);
        }
    }

    fn on_rx(&mut self, v: &NodeRxView) {
        if let (Some(p), Some(us)) = (v.rssi_dbm, v.airtime_us)
            && p >= self.busy_dbm
        {
            *self.sensed_us.entry(v.rx).or_insert(0) += us;
        }
        match v.outcome {
            RxFate::InFlight => {}
            RxFate::Delivered => self.resolved += 1,
            RxFate::Lost => {
                self.resolved += 1;
                let cause = v.cause.clone().unwrap_or_else(|| "unknown".to_string());
                *self.lost_by_cause.entry(cause).or_insert(0) += 1;
            }
        }
    }

    fn on_mac(&mut self, v: &MacCbrView) {
        if let Some(d) = v.queue_depth {
            self.queue_depth.observe(d as f64);
        }
        if let Some(d) = v.mac_drops {
            self.mac_drops += d;
        }
        if let Some(b) = v.offered_bytes {
            self.offered_bits += b * 8;
            self.offered_reported = true;
        }
        if let Some(us) = v.offered_airtime_us {
            *self.offered_us.entry(v.node).or_insert(0) += us;
        }
    }
}

impl Model for LoadProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for LoadProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![
            NodeTxView::channel_name(),
            NodeRxView::channel_name(),
            MacCbrView::channel_name(),
        ]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        match ev.channel {
            NodeTxView::CHANNEL => match decode::<NodeTxView>(ev) {
                Ok(v) => self.on_tx(&v),
                Err(_) => self.rejected += 1,
            },
            NodeRxView::CHANNEL => match decode::<NodeRxView>(ev) {
                Ok(v) => self.on_rx(&v),
                Err(_) => self.rejected += 1,
            },
            MacCbrView::CHANNEL => match decode::<MacCbrView>(ev) {
                Ok(v) => self.on_mac(&v),
                Err(_) => self.rejected += 1,
            },
            _ => {}
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();
        let window_us = Duration::between(self.window_start, at).as_nanos() / 1000;
        let secs = (window_us > 0).then(|| (window_us as f64) / 1e6);

        let own = core::mem::take(&mut self.own_us);
        let sensed = core::mem::take(&mut self.sensed_us);
        let offered = core::mem::take(&mut self.offered_us);

        // --- channel_occupancy -----------------------------------------------------------
        let mut occ = Distribution::new();
        if window_us > 0 {
            for us in own.values() {
                occ.observe((*us as f64) / (window_us as f64));
            }
        }
        out.push(MetricSample::new(
            &self.def("channel_occupancy"),
            at,
            Dims::new(),
            SampleValue::Distribution(occ.summary(1)),
        ));

        // --- channel_load ----------------------------------------------------------------
        let mut load = Distribution::new();
        if window_us > 0 {
            let nodes: std::collections::BTreeSet<NodeId> = own
                .keys()
                .chain(sensed.keys())
                .chain(offered.keys())
                .copied()
                .collect();
            for n in nodes {
                // A node's own demand is what it offered when the MAC reported it, and what
                // it sent otherwise; never less than what it sent.
                let own_demand = own
                    .get(&n)
                    .copied()
                    .unwrap_or(0)
                    .max(offered.get(&n).copied().unwrap_or(0));
                let total = sensed.get(&n).copied().unwrap_or(0) + own_demand;
                load.observe((total as f64) / (window_us as f64));
            }
        }
        out.push(MetricSample::new(
            &self.def("channel_load"),
            at,
            Dims::new(),
            SampleValue::Distribution(load.summary(1)),
        ));

        // --- offered and carried load ----------------------------------------------------
        let carried = core::mem::take(&mut self.carried_bits);
        let offered_bits = if core::mem::take(&mut self.offered_reported) {
            core::mem::take(&mut self.offered_bits).max(carried)
        } else {
            self.offered_bits = 0;
            carried
        };
        let rate = |bits: u64| match secs {
            Some(s) => Estimate::Value {
                point: (bits as f64) / s,
                n: 1,
            },
            None => Estimate::Insufficient { n: 0, required: 1 },
        };
        out.push(MetricSample::new(
            &self.def("offered_load"),
            at,
            Dims::new(),
            SampleValue::Scalar(rate(offered_bits)),
        ));
        out.push(MetricSample::new(
            &self.def("carried_load"),
            at,
            Dims::new(),
            SampleValue::Scalar(rate(carried)),
        ));

        // --- loss rates ------------------------------------------------------------------
        let resolved = core::mem::take(&mut self.resolved);
        let lost = core::mem::take(&mut self.lost_by_cause);
        let loss_def = self.def("loss_rate");
        for (cause, n) in &lost {
            let mut dims = Dims::new();
            dims.insert(Dim::Cause, DimValue::label(cause.clone()));
            out.push(MetricSample::new(
                &loss_def,
                at,
                dims,
                SampleValue::Ratio(
                    Proportion::from_counts(*n, resolved).estimate(self.min_samples, self.level),
                ),
            ));
        }
        let of = |causes: &[&str]| -> u64 {
            lost.iter()
                .filter(|(c, _)| causes.contains(&c.as_str()))
                .map(|(_, n)| *n)
                .sum()
        };
        out.push(MetricSample::new(
            &self.def("collision_rate"),
            at,
            Dims::new(),
            SampleValue::Ratio(
                Proportion::from_counts(of(&COLLISION_CAUSES), resolved)
                    .estimate(self.min_samples, self.level),
            ),
        ));
        out.push(MetricSample::new(
            &self.def("half_duplex_rate"),
            at,
            Dims::new(),
            SampleValue::Ratio(
                Proportion::from_counts(of(&["half-duplex"]), resolved)
                    .estimate(self.min_samples, self.level),
            ),
        ));

        // --- the MAC ---------------------------------------------------------------------
        let depth = core::mem::replace(&mut self.queue_depth, Distribution::new());
        out.push(MetricSample::new(
            &self.def("mac_queue_depth"),
            at,
            Dims::new(),
            SampleValue::Distribution(depth.summary(1)),
        ));
        out.push(MetricSample::new(
            &self.def("mac_drops"),
            at,
            Dims::new(),
            SampleValue::count(core::mem::take(&mut self.mac_drops)),
        ));
        let access = core::mem::replace(&mut self.access_ms, Distribution::new());
        out.push(MetricSample::new(
            &self.def("mac_access_delay"),
            at,
            Dims::new(),
            SampleValue::Distribution(access.summary(self.min_samples)),
        ));

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

    fn point(s: &[MetricSample], name: &str) -> Option<f64> {
        s.iter()
            .find(|x| x.metric == name && x.dims.is_empty())
            .and_then(|x| x.value.point())
    }

    #[test]
    fn channel_load_sums_sensed_and_own_air_time() {
        let mut p = LoadProvider::new(0).with_min_samples(1);
        // Node 1 sends 100 ms of air time in a 1 s window; node 2 hears 300 ms above the
        // threshold and 200 ms below it, and sends nothing.
        p.on_event(&rec(
            "node.tx",
            serde_json::json!({"t": 1, "node": 1, "bytes_on_wire": 1000, "airtime_us": 100_000}),
        ));
        for (p_dbm, us) in [(-60.0, 300_000u64), (-95.0, 200_000)] {
            p.on_event(&rec(
                "node.rx",
                serde_json::json!({"t": 1, "rx": 2, "outcome": "lost", "cause": "fading",
                    "rssi_dbm": p_dbm, "airtime_us": us}),
            ));
        }
        let s = p.flush(NS_PER_S);
        let load = s.iter().find(|x| x.metric == "channel_load").unwrap();
        let crate::def::SampleValue::Distribution(d) = &load.value else {
            panic!()
        };
        // Node 1: 0.1 (own). Node 2: 0.3 (sensed). Max is 0.3.
        assert_eq!(d.n(), 2);
        assert_eq!(
            d.quantile(crate::stats::Percentile::P99).map(|v| v > 0.29),
            Some(true)
        );
        assert_eq!(point(&s, "carried_load"), Some(8000.0));
        assert_eq!(
            point(&s, "offered_load"),
            Some(8000.0),
            "no MAC report: offered = carried"
        );
    }

    #[test]
    fn loss_rates_are_over_resolved_attempts() {
        let mut p = LoadProvider::new(0).with_min_samples(1);
        for outcome in [
            serde_json::json!({"t":1,"rx":2,"outcome":"delivered"}),
            serde_json::json!({"t":1,"rx":2,"outcome":"lost","cause":"collision"}),
            serde_json::json!({"t":1,"rx":2,"outcome":"lost","cause":"half-duplex"}),
            serde_json::json!({"t":1,"rx":2,"outcome":"lost","cause":"verify-overflow"}),
            serde_json::json!({"t":1,"rx":2,"outcome":"in-flight"}),
        ] {
            p.on_event(&rec("node.rx", outcome));
        }
        let s = p.flush(NS_PER_S);
        assert_eq!(point(&s, "collision_rate"), Some(0.25));
        assert_eq!(point(&s, "half_duplex_rate"), Some(0.25));
        let total: f64 = s
            .iter()
            .filter(|x| x.metric == "loss_rate")
            .map(|x| x.value.point().unwrap())
            .sum();
        assert!(
            (total - 0.75).abs() < 1e-9,
            "Σ loss rates = 1 − delivery ratio"
        );
    }

    #[test]
    fn the_mac_report_fills_queue_depth_drops_and_offered_load() {
        let mut p = LoadProvider::new(0).with_min_samples(1);
        p.on_event(&rec(
            "mac.cbr",
            serde_json::json!({"t":1,"node":1,"cbr":0.2,"queue_depth":3,"mac_drops":2,
                "offered_bytes":2000,"offered_airtime_us":1000}),
        ));
        p.on_event(&rec(
            "node.tx",
            serde_json::json!({"t": 2_000_000, "node": 1, "bytes_on_wire": 1000,
                "airtime_us": 500, "t_signed": 1_000_000}),
        ));
        let s = p.flush(NS_PER_S);
        assert_eq!(point(&s, "offered_load"), Some(16_000.0));
        assert_eq!(point(&s, "carried_load"), Some(8_000.0));
        assert_eq!(point(&s, "mac_drops"), Some(2.0));
        assert_eq!(point(&s, "mac_access_delay"), Some(1.0));
    }
}
