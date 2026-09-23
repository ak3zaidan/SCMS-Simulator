//! Communication metrics: delivery, error, busy ratio, gaps, latency, goodput, bytes,
//! airtime (08-measurement-and-data.md §2.1).
//!
//! | Metric | Formula | Unit | What it does **not** account for |
//! |---|---|---|---|
//! | `pdr` | received frames / candidate receptions | ratio | receivers outside the candidate range; a frame nobody was in range of contributes nothing, so a PDR of 1.0 over an empty road is a PDR over no trials and is reported as insufficient |
//! | `per` | frames lost / candidate receptions | ratio | the same denominator, and it is `1 − pdr` by construction, not an independently measured quantity |
//! | `pdr_by_cause` | losses with this cause / all losses | ratio | a frame lost for two reasons at once: I-R3 requires exactly one cause, and [`crate::invariants`] checks it rather than this metric papering over it |
//! | `cbr` | busy time / window length, as the MAC measured it | ratio | what the MAC could not hear: a hidden terminal's transmission is not busy time at this receiver |
//! | `pir` | gap between successive receptions from one transmitter at one receiver | s | the first reception from a transmitter, which has no predecessor; and a gap that spans a pseudonym change, which looks like a new transmitter to the receiver but is recorded here against the true id |
//! | `goodput` | application payload bytes delivered to applications (`node.rx`) / window | B/s | retransmitted or duplicate deliveries are counted once per reception, so a message delivered to ten receivers counts ten times: this is receiver-side goodput, not network throughput |
//! | `bytes_*` | bytes on the wire per accounting bucket / window | B/s | bytes the producer did not attribute to a bucket, which are reported by I-N1 rather than folded into a bucket |
//! | `bytes_total` | Σ over the five buckets | B/s | the same bytes as the buckets: it is their sum by construction, and a test holds it to that |
//!
//! `e2e_latency` moved to [`crate::latency`], where it is decomposed stage by stage from
//! `node.rx`. It used to be a join of `node.tx` to `node.verify` by message id here; no
//! receiver can put the sender's message id on `node.verify` (it is not on the air), so
//! the join never matched a record a real run produced.
//! | `airtime_per_node` | transmitted air time / window | ms/s | receive-side occupancy and inter-frame spacing: this is the node's own transmissions only |
//!
//! # Determinism
//!
//! Byte counts, airtime and frame counts are accumulated as **integers**, so their
//! reductions are exact and order-independent with no floating-point question to answer.
//! The float-valued samples (`cbr`, `pir`) go through
//! [`crate::stats::Distribution`], which sorts into IEEE-754 total order before it reduces.
//! Per-node and per-bucket tables are `BTreeMap`s, so they are written in id order.
//!
//! Distance binning is on the integer millimetre grid ([`crate::bins`]), so a reception at
//! exactly 25 m cannot land in different bins on two platforms (build decision D10).

use std::collections::BTreeMap;

use serde_json::json;
use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::ids::{LinkKey, NodeId};
use v2xw_core::math::sum_sorted_by_key;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::bins::Bins;
use crate::cards;
use crate::channels::{
    ByteBucket, ChannelView, MacCbrView, NetBytesView, NodeRxView, NodeTxView, PhyRxView,
    ProtoMsgView, RxFate, RxOutcome, decode,
};
use crate::def::{Agg, DEFAULT_LEVEL, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::MetricProvider;
use crate::quant::Quantum;
use crate::stats::{ConfidenceLevel, Distribution, Estimate, Proportion, ratio_of_sums};

/// The dimension value for receptions whose ground-truth distance the producer did not
/// record.
///
/// They are reported under this label rather than folded into bin zero or dropped: a
/// distance-binned PDR that quietly counted unbinnable receptions in its nearest bin would
/// be wrong, and one that dropped them would disagree with the un-binned `pdr` for no
/// visible reason.
pub const UNBINNED: &str = "unbinned";

/// The communication metric provider.
///
/// Windowed: every metric here is per flush window, and [`MetricProvider::flush`] drains
/// the accumulators.
pub struct CommsProvider {
    card: ModelCard,
    level: ConfidenceLevel,
    min_samples: u64,
    bins: Bins,

    window_start: SimTime,

    /// Delivery outcomes per distance bin, and for receptions with no recorded distance.
    pdr_by_bin: BTreeMap<usize, Proportion>,
    pdr_unbinned: Proportion,
    /// Delivery outcomes over every candidate reception, regardless of distance.
    pdr_all: Proportion,
    /// Loss counts per cause.
    losses_by_cause: BTreeMap<String, u64>,
    /// Channel busy ratios, per 5.9 GHz channel (`None` = the producer did not say).
    cbr: BTreeMap<Option<u16>, Distribution>,
    /// Inter-packet gaps in seconds.
    pir: Distribution,
    /// The last successful reception per directed link, for the next gap.
    last_rx: BTreeMap<LinkKey, SimTime>,
    /// Airtime per transmitting node, in microseconds. Integer, so exact.
    airtime_us: BTreeMap<NodeId, u64>,
    /// Bytes on the wire per accounting bucket. Integer, so exact.
    bytes: BTreeMap<ByteBucket, u64>,
    /// Delivered application payload bytes. Integer, so exact.
    goodput_bytes: u64,
    /// Deliveries behind `goodput_bytes`, so the rate carries a sample count.
    deliveries: u64,

    /// Records this provider could not decode.
    rejected: u64,
}

impl CommsProvider {
    /// A provider with the documented defaults: 25 m distance bins out to 1 km — the
    /// engine's candidate range, so no reception it evaluates lands in the open-ended bin —
    /// a 95 % level and [`crate::stats::DEFAULT_MIN_SAMPLES`].
    ///
    /// `t0` is the start of the first window.
    ///
    /// # Panics
    /// Never: forty 25 m bins are a valid bin set, so the only fallible step cannot fail.
    /// The unwrap is kept rather than propagated so the common constructor is infallible.
    #[must_use]
    pub fn new(t0: SimTime) -> Self {
        Self::with_bins(t0, Bins::distance_25m(40).expect("25 m bins are valid"))
    }

    /// A provider with caller-chosen distance bins.
    #[must_use]
    pub fn with_bins(t0: SimTime, bins: Bins) -> Self {
        Self {
            card: Self::build_card(),
            level: DEFAULT_LEVEL,
            min_samples: crate::stats::DEFAULT_MIN_SAMPLES,
            bins,
            window_start: t0,
            pdr_by_bin: BTreeMap::new(),
            pdr_unbinned: Proportion::new(),
            pdr_all: Proportion::new(),
            losses_by_cause: BTreeMap::new(),
            cbr: BTreeMap::new(),
            pir: Distribution::new(),
            last_rx: BTreeMap::new(),
            airtime_us: BTreeMap::new(),
            bytes: BTreeMap::new(),
            goodput_bytes: 0,
            deliveries: 0,
            rejected: 0,
        }
    }

    /// Sets the insufficiency threshold (the card's `min_samples`).
    #[must_use]
    pub const fn with_min_samples(mut self, n: u64) -> Self {
        self.min_samples = n;
        self
    }

    /// Sets the confidence level (the card's `confidence_level`).
    #[must_use]
    pub const fn with_level(mut self, level: ConfidenceLevel) -> Self {
        self.level = level;
        self
    }

    /// The distance bins in use.
    #[must_use]
    pub const fn bins(&self) -> &Bins {
        &self.bins
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/comms/delivery-and-airtime",
            "1.0.0",
            "Packet delivery ratio by distance, packet error rate, channel busy ratio, \
             inter-packet gap, goodput, byte accounting by bucket and airtime per node.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "pdr",
                "pdr = received frames / candidate receptions, per 25 m distance bin",
            ),
            v2xw_core::card::Equation::new(
                "per",
                "per = frames lost / candidate receptions = 1 − pdr",
            ),
            v2xw_core::card::Equation::new(
                "cbr",
                "cbr = busy time / window length; recomputed from the producer's busy and \
                 window fields when both are present, so the producer's rounding does not \
                 become the metric's",
            ),
            v2xw_core::card::Equation::new(
                "pir",
                "pir = t(k) − t(k−1) for successive receptions from one transmitter at one \
                 receiver",
            ),
            v2xw_core::card::Equation::new(
                "goodput",
                "goodput = Σ delivered payload bytes / window length, in B/s",
            ),
            v2xw_core::card::Equation::new(
                "airtime_per_node",
                "airtime_per_node = Σ airtime of the node's transmissions / window length, in ms/s",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.parameters.push(cards::param(
            "dist_bin_width_m",
            "m",
            json!(25.0),
            json!(1.0),
            json!(1000.0),
            cards::design("08-measurement-and-data.md §2 (dist_bin, 25 m), to 1 km"),
        ));
        card.sources = vec![
            cards::design("08-measurement-and-data.md §2.1 (radio and network metric catalog)"),
            cards::standard(
                "3GPP TS 38.215 §5.1.27 (CBR for NR-V2X) and TS 36.214 (CBR for LTE-V2X), \
                 the definitions 08-measurement-and-data.md §2.1 cites for cbr",
            ),
            cards::standard(
                "ETSI TS 102 687 (DCC): the −85 dBm CCA threshold and the 100 ms window the \
                 802.11p CBR definition uses",
            ),
        ];
        card.limitations = vec![
            "pdr is measured over candidate receptions, so it says nothing about coverage: a \
             transmitter with no neighbours contributes no trials."
                .to_string(),
            "goodput is receiver-side: one broadcast delivered to ten receivers counts ten \
             times."
                .to_string(),
        ];
        card.ignores = vec![
            "Per-receiver interference decomposition (that is the radio crate's own output)."
                .to_string(),
            "Fragmentation: reassembly failure and loss amplification are net-layer metrics."
                .to_string(),
        ];
        card.validation.tests = vec![
            "comms::tests::pdr_by_distance_reproduces_a_hand_computed_fixture".to_string(),
            "comms::tests::cbr_is_recomputed_from_busy_over_window".to_string(),
            "comms::tests::airtime_and_bytes_are_order_independent".to_string(),
        ];
        card
    }

    /// The definitions, in a fixed order.
    fn definitions(&self) -> Vec<MetricDef> {
        let src = cards::design("08-measurement-and-data.md §2.1");
        vec![
            MetricDef::new(
                "pdr",
                "ratio",
                Agg::ratio("received frames", "candidate receptions"),
                // The distance bin comes from a ground-truth field of `phy.rx`, so the
                // binned metric inherits the GT taint (08 §1: visibility tags propagate).
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "Received frames divided by frames that were candidate receptions (the \
                 receiver was within the tier's candidate range), per 25 m distance bin.",
            )
            .with_dims([Dim::T, Dim::DistBin])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .with_range(0.0, 1.0)
            .not_accounting_for("receivers outside the tier's candidate range")
            .not_accounting_for(
                "coverage: a transmitter with no candidate receivers contributes no trials",
            ),
            MetricDef::new(
                "per",
                "ratio",
                Agg::ratio("frames lost", "candidate receptions"),
                // Reads only the outcome field, which is node-visible.
                Visibility::Node,
                Quantum::RATIO,
                "Frames lost divided by candidate receptions. Exactly `1 − pdr`; reported \
                 separately because a loss-oriented reader should not have to subtract.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .with_range(0.0, 1.0)
            .not_accounting_for("the same denominator as pdr")
            .not_accounting_for("bit errors within a received frame (a frame is received or not)"),
            MetricDef::new(
                "pdr_by_cause",
                "ratio",
                Agg::ratio("losses with this cause", "all losses"),
                Visibility::Node,
                Quantum::RATIO,
                "The share of losses attributable to each loss cause. Invariant I-R3 \
                 requires every loss to carry exactly one cause, so the shares sum to one \
                 by construction — and when they do not, that is an I-R3violation and not \
                 a rounding artefact.",
            )
            .with_dims([Dim::T, Dim::Cause])
            .with_breakdown(Dim::Cause, crate::channels::rx_cause::PHY)
            .breakdown_only()
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .with_range(0.0, 1.0)
            .not_accounting_for("a frame lost for two reasons at once")
            .not_accounting_for("losses the producer recorded without a cause"),
            MetricDef::new(
                "cbr",
                "ratio",
                Agg::Distribution,
                Visibility::Node,
                Quantum::RATIO,
                "Channel busy ratio as measured by the MAC: for 802.11p, the fraction of a \
                 100 ms window with CCA busy above −85 dBm; for C-V2X, the TS 38.215 \
                 §5.1.27 / TS 36.214 definition. Reported as a distribution over the \
                 window's per-node measurements; each is a node's own measurement over its \
                 own window, so the distribution is reported whatever the node count, with \
                 that count.",
            )
            .with_dims([Dim::T, Dim::Channel])
            // The one channel this build puts safety traffic on (SAE J2945/1: channel 172).
            .with_breakdown(Dim::Channel, ["172"])
            .with_source(cards::standard("3GPP TS 38.215 §5.1.27; TS 36.214"))
            .with_min_samples(1)
            .with_range(0.0, 1.0)
            .not_accounting_for("energy the receiver could not hear (a hidden terminal)")
            .not_accounting_for("which node measured it: this is the distribution across nodes"),
            MetricDef::new(
                "pir",
                "s",
                Agg::Distribution,
                // Pairing successive receptions needs the transmitter's identity, which is
                // ground truth on `phy.rx`.
                Visibility::NodeAndGt,
                Quantum::TIME_S,
                "Packet inter-reception time: the gap between successive receptions from \
                 the same transmitter at the same receiver.",
            )
            .with_dims([Dim::T])
            .with_source(cards::paper(
                "Martelli, Elena Renda, Resta, Santi, 'A measurement-based study of beaconing \
                 performance in IEEE 802.11p vehicular networks', IEEE INFOCOM 2012 (packet \
                 inter-reception time)",
            ))
            .with_min_samples(self.min_samples)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("the first reception from a transmitter, which has no predecessor")
            .not_accounting_for(
                "a pseudonym change, which a receiver sees as a new transmitter while this \
                 metric pairs on the true id",
            ),
            MetricDef::new(
                "goodput",
                "B/s",
                Agg::Rate,
                Visibility::Node,
                Quantum::BYTES,
                "Application payload bytes delivered to receivers' applications (node.rx), \
                 divided by the window's length.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("duplicate suppression: one broadcast to ten receivers counts ten times")
            .not_accounting_for("headers and the security envelope, which are in bytes_air"),
            MetricDef::new(
                "airtime_per_node",
                "ms/s",
                Agg::Rate,
                Visibility::Node,
                Quantum::TIME_MS,
                "Transmitted air time per node, divided by the window's length. With no \
                 dimension, the mean over the nodes that transmitted in the window.",
            )
            .with_dims([Dim::T, Dim::Node])
            .with_source(src.clone())
            .with_min_samples(1)
            .with_range(0.0, 1000.0)
            .not_accounting_for("receive-side occupancy")
            .not_accounting_for("inter-frame spacing and backoff, which are not transmitted time"),
        ]
        .into_iter()
        .chain(ByteBucket::ALL.into_iter().map(|b| {
            MetricDef::new(
                b.metric_name(),
                "B/s",
                Agg::Rate,
                Visibility::Node,
                Quantum::BYTES,
                format!(
                    "Bytes on the wire attributed to the `{b}` accounting bucket, divided by \
                     the window's length. Invariant I-N1 requires every byte to belong to \
                     exactly one bucket."
                ),
            )
            .with_dims([Dim::T, Dim::Bucket])
            .with_breakdown(Dim::Bucket, [b.as_str()])
            .with_source(cards::design("08-measurement-and-data.md §2.1 (bytes per bucket)"))
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("bytes the producer did not attribute to a bucket")
            .not_accounting_for("physical-layer preamble and padding, unless the producer counts them in bytes_on_wire")
        }))
        .chain(core::iter::once(
            MetricDef::new(
                "bytes_total",
                "B/s",
                Agg::Rate,
                Visibility::Node,
                Quantum::BYTES,
                "Every byte on the wire in every accounting bucket, divided by the window's \
                 length: the sum of bytes_air, bytes_uu_ul, bytes_uu_dl, bytes_backhaul and \
                 bytes_backend, which invariant I-N1 makes disjoint.",
            )
            .with_dims([Dim::T])
            .with_source(cards::design("03-interfaces.md §5 (I-N1)"))
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("bytes the producer did not attribute to a bucket")
            .not_accounting_for("physical-layer preamble and padding"),
        ))
        .collect()
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    /// Records one transmission.
    fn on_tx(&mut self, v: &NodeTxView) {
        if let Some(us) = v.airtime_us {
            *self.airtime_us.entry(v.node).or_insert(0) += us;
        }
        *self.bytes.entry(ByteBucket::Air).or_insert(0) += v.bytes_on_wire;
    }

    /// Records one reception attempt.
    fn on_rx(&mut self, v: &PhyRxView) {
        if !v.candidate {
            return;
        }
        let delivered = v.outcome == RxOutcome::Ok;
        self.pdr_all.observe(delivered);
        match v.dist_m.and_then(|d| self.bins.index_of(d)) {
            Some(bin) => self.pdr_by_bin.entry(bin).or_default().observe(delivered),
            None => self.pdr_unbinned.observe(delivered),
        }
        if !delivered {
            for cause in v.all_causes() {
                *self.losses_by_cause.entry(cause.to_string()).or_insert(0) += 1;
            }
        }
        if delivered {
            if let Some(tx) = v.tx {
                let key = LinkKey::new(tx, v.rx);
                if let Some(prev) = self.last_rx.insert(key, v.t_end)
                    && v.t_end >= prev
                {
                    self.pir
                        .observe(Duration::between(prev, v.t_end).as_secs_f64());
                }
            }
        }
    }

    /// Records one end-to-end outcome: a delivery to an application is goodput.
    fn on_node_rx(&mut self, v: &NodeRxView) {
        if v.outcome != RxFate::Delivered {
            return;
        }
        if let Some(bytes) = v.payload_bytes {
            self.goodput_bytes += bytes;
            self.deliveries += 1;
        }
    }

    fn on_cbr(&mut self, v: &MacCbrView) {
        let ratio = match (v.busy_us, v.window_us) {
            (Some(busy), Some(window)) if window > 0 => (busy as f64) / (window as f64),
            _ => v.cbr,
        };
        self.cbr.entry(v.channel).or_default().observe(ratio);
    }

    fn on_bytes(&mut self, v: &NetBytesView) {
        *self.bytes.entry(v.bucket).or_insert(0) += v.bytes_on_wire;
    }

    fn on_proto_msg(&mut self, v: &ProtoMsgView) {
        // A protocol message whose transport the producer named is counted in that bucket;
        // one without a named transport is a byte this metric cannot place, and I-N1
        // reports it rather than this metric guessing.
        if let Some(bucket) = v.transport {
            *self.bytes.entry(bucket).or_insert(0) += v.bytes_on_wire;
        }
    }

    /// The window's length in seconds, or `None` for a zero-length window.
    fn window_secs(&self, at: SimTime) -> Option<f64> {
        if at <= self.window_start {
            return None;
        }
        Some(Duration::between(self.window_start, at).as_secs_f64())
    }
}

impl Model for CommsProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for CommsProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![
            NodeTxView::channel_name(),
            PhyRxView::channel_name(),
            MacCbrView::channel_name(),
            NodeRxView::channel_name(),
            NetBytesView::channel_name(),
            ProtoMsgView::channel_name(),
        ]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        match ev.channel {
            NodeTxView::CHANNEL => match decode::<NodeTxView>(ev) {
                Ok(v) => self.on_tx(&v),
                Err(_) => self.rejected += 1,
            },
            PhyRxView::CHANNEL => match decode::<PhyRxView>(ev) {
                Ok(v) => self.on_rx(&v),
                Err(_) => self.rejected += 1,
            },
            MacCbrView::CHANNEL => match decode::<MacCbrView>(ev) {
                Ok(v) => self.on_cbr(&v),
                Err(_) => self.rejected += 1,
            },
            NodeRxView::CHANNEL => match decode::<NodeRxView>(ev) {
                Ok(v) => self.on_node_rx(&v),
                Err(_) => self.rejected += 1,
            },
            NetBytesView::CHANNEL => match decode::<NetBytesView>(ev) {
                Ok(v) => self.on_bytes(&v),
                Err(_) => self.rejected += 1,
            },
            ProtoMsgView::CHANNEL => match decode::<ProtoMsgView>(ev) {
                Ok(v) => self.on_proto_msg(&v),
                Err(_) => self.rejected += 1,
            },
            _ => {}
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();
        let secs = self.window_secs(at);

        // --- pdr, overall and by distance bin -------------------------------------------
        let pdr_def = self.def("pdr");
        out.push(MetricSample::new(
            &pdr_def,
            at,
            Dims::new(),
            SampleValue::Ratio(self.pdr_all.estimate(self.min_samples, self.level)),
        ));
        for (bin, p) in core::mem::take(&mut self.pdr_by_bin) {
            let mut dims = Dims::new();
            dims.insert(Dim::DistBin, DimValue::label(self.bins.label(bin)));
            out.push(MetricSample::new(
                &pdr_def,
                at,
                dims,
                SampleValue::Ratio(p.estimate(self.min_samples, self.level)),
            ));
        }
        let unbinned = core::mem::replace(&mut self.pdr_unbinned, Proportion::new());
        if unbinned.trials() > 0 {
            let mut dims = Dims::new();
            dims.insert(Dim::DistBin, DimValue::label(UNBINNED));
            out.push(MetricSample::new(
                &pdr_def,
                at,
                dims,
                SampleValue::Ratio(unbinned.estimate(self.min_samples, self.level)),
            ));
        }

        // --- per -------------------------------------------------------------------------
        let all = core::mem::replace(&mut self.pdr_all, Proportion::new());
        let per = Proportion::from_counts(all.failures(), all.trials());
        out.push(MetricSample::new(
            &self.def("per"),
            at,
            Dims::new(),
            SampleValue::Ratio(per.estimate(self.min_samples, self.level)),
        ));

        // --- pdr_by_cause ----------------------------------------------------------------
        let causes = core::mem::take(&mut self.losses_by_cause);
        // `sum_sorted_by_key` over the cause names: the reduction is over a BTreeMap, so
        // it is already in key order, and this states that fact where a reader can see it.
        let total_losses = sum_sorted_by_key(causes.iter().map(|(k, v)| (k.clone(), *v as f64)));
        let total_losses = total_losses as u64;
        let cause_def = self.def("pdr_by_cause");
        for (cause, n) in causes {
            let mut dims = Dims::new();
            dims.insert(Dim::Cause, DimValue::label(cause));
            out.push(MetricSample::new(
                &cause_def,
                at,
                dims,
                SampleValue::Ratio(
                    Proportion::from_counts(n, total_losses).estimate(self.min_samples, self.level),
                ),
            ));
        }

        // --- cbr -------------------------------------------------------------------------
        let cbr_def = self.def("cbr");
        for (channel, d) in core::mem::take(&mut self.cbr) {
            let mut dims = Dims::new();
            if let Some(c) = channel {
                dims.insert(Dim::Channel, DimValue::index(u64::from(c)));
            }
            out.push(MetricSample::new(
                &cbr_def,
                at,
                dims,
                SampleValue::Distribution(d.summary(1)),
            ));
        }

        // --- pir -------------------------------------------------------------------------
        let pir = core::mem::replace(&mut self.pir, Distribution::new());
        out.push(MetricSample::new(
            &self.def("pir"),
            at,
            Dims::new(),
            SampleValue::Distribution(pir.summary(self.min_samples)),
        ));

        // --- goodput ---------------------------------------------------------------------
        let goodput_bytes = core::mem::take(&mut self.goodput_bytes);
        let deliveries = core::mem::take(&mut self.deliveries);
        out.push(MetricSample::new(
            &self.def("goodput"),
            at,
            Dims::new(),
            SampleValue::Ratio(match secs {
                Some(s) => ratio_of_sums(goodput_bytes as f64, s, deliveries, 1),
                None => crate::stats::RatioEstimate::Insufficient {
                    trials: deliveries,
                    required: 1,
                },
            }),
        ));

        // --- airtime per node ------------------------------------------------------------
        let airtime_def = self.def("airtime_per_node");
        let airtime_us = core::mem::take(&mut self.airtime_us);
        for (node, us) in &airtime_us {
            let (node, us) = (*node, *us);
            let mut dims = Dims::new();
            dims.insert(Dim::Node, DimValue::index(u64::from(node.index())));
            // Airtime is in µs and the metric is in ms/s: µs/1000 is ms, divided by the
            // window in seconds.
            let value = match secs {
                Some(s) => Estimate::Value {
                    point: (us as f64) / 1000.0 / s,
                    n: 1,
                },
                None => Estimate::Insufficient { n: 0, required: 1 },
            };
            out.push(MetricSample::new(
                &airtime_def,
                at,
                dims,
                SampleValue::Scalar(value),
            ));
        }

        // The headline: the mean over the nodes that transmitted, so a live view has one
        // line to draw where the per-node samples would be one line per vehicle.
        if let (Some(s), false) = (secs, airtime_us.is_empty()) {
            let n = airtime_us.len() as u64;
            let total: u64 = airtime_us.values().sum();
            out.push(MetricSample::new(
                &airtime_def,
                at,
                Dims::new(),
                SampleValue::Scalar(Estimate::Value {
                    point: (total as f64) / 1000.0 / s / (n as f64),
                    n,
                }),
            ));
        }

        // --- bytes per bucket ------------------------------------------------------------
        let buckets = core::mem::take(&mut self.bytes);
        let mut all_bytes = 0u64;
        for bucket in ByteBucket::ALL {
            let bytes = buckets.get(&bucket).copied().unwrap_or(0);
            all_bytes += bytes;
            let mut dims = Dims::new();
            dims.insert(Dim::Bucket, DimValue::label(bucket.as_str()));
            let value = match secs {
                Some(s) => Estimate::Value {
                    point: (bytes as f64) / s,
                    n: 1,
                },
                None => Estimate::Insufficient { n: 0, required: 1 },
            };
            out.push(MetricSample::new(
                &self.def(bucket.metric_name()),
                at,
                dims,
                SampleValue::Scalar(value),
            ));
        }

        // The total is the sum of exactly the integers the five bucket samples were made
        // from, so `bytes_total` equals their sum before quantisation by construction.
        out.push(MetricSample::new(
            &self.def("bytes_total"),
            at,
            Dims::new(),
            SampleValue::Scalar(match secs {
                Some(s) => Estimate::Value {
                    point: (all_bytes as f64) / s,
                    n: 1,
                },
                None => Estimate::Insufficient { n: 0, required: 1 },
            }),
        ));

        // --- window bookkeeping ----------------------------------------------------------
        // `last_rx` is not pruned by time: it holds one instant per directed link, which is
        // bounded by the number of links the run actually used, and a gap that spans a
        // window is a real gap.
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
    use crate::provider::MetricProvider;
    use v2xw_core::ctx::OwnedRecord;

    fn rec(channel: &'static str, json: serde_json::Value) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility: Visibility::Node,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    fn rx(tx: u32, rx_node: u32, dist: f64, ok: bool, t: SimTime) -> OwnedRecord {
        rec(
            "phy.rx",
            json!({
                "t_start": t, "t_end": t + 1_000, "tx": tx, "rx": rx_node,
                "dist_m": dist,
                "outcome": if ok { "ok" } else { "lost" },
                "cause": if ok { serde_json::Value::Null } else { json!("collision") },
                "payload_bytes": if ok { json!(100) } else { serde_json::Value::Null },
            }),
        )
    }

    fn sample<'a>(samples: &'a [MetricSample], key: &str) -> &'a MetricSample {
        samples
            .iter()
            .find(|s| s.key() == key)
            .unwrap_or_else(|| panic!("no sample with key {key}; have {:?}", keys(samples)))
    }

    fn keys(samples: &[MetricSample]) -> Vec<String> {
        samples.iter().map(MetricSample::key).collect()
    }

    #[test]
    fn every_definition_validates() {
        CommsProvider::new(0).validate_defs().unwrap();
    }

    #[test]
    fn the_card_validates_and_declares_every_parameter_it_reads() {
        let p = CommsProvider::new(0);
        p.card().validate().unwrap();
        p.card().check_api_version().unwrap();
        let names: Vec<&str> = p
            .card()
            .parameters
            .iter()
            .map(|x| x.name.as_str())
            .collect();
        for expected in ["min_samples", "confidence_level", "dist_bin_width_m"] {
            assert!(names.contains(&expected), "{expected} not declared");
        }
    }

    /// The hand-computed fixture: in the 0–25 m bin, 3 of 4 frames arrive; in the 25–50 m
    /// bin, 1 of 4. With `min_samples` at 1 those are 0.75 and 0.25 exactly.
    #[test]
    fn pdr_by_distance_reproduces_a_hand_computed_fixture() {
        let mut p = CommsProvider::new(0).with_min_samples(1);
        for (i, ok) in [true, true, true, false].into_iter().enumerate() {
            p.on_event(&rx(1, 2, 10.0, ok, 1_000_000 * (i as u64 + 1)));
        }
        for (i, ok) in [true, false, false, false].into_iter().enumerate() {
            p.on_event(&rx(3, 4, 30.0, ok, 1_000_000 * (i as u64 + 1)));
        }
        let s = p.flush(1_000_000_000);
        assert_eq!(sample(&s, "pdr|dist_bin=0-25").value.point(), Some(0.75));
        assert_eq!(sample(&s, "pdr|dist_bin=25-50").value.point(), Some(0.25));
        // Overall: 4 of 8.
        assert_eq!(sample(&s, "pdr").value.point(), Some(0.5));
        assert_eq!(sample(&s, "pdr").value.n(), 8);
        // PER is the complement, over the same denominator.
        assert_eq!(sample(&s, "per").value.point(), Some(0.5));
        // All four losses had the one cause, so its share is 1.0.
        assert_eq!(
            sample(&s, "pdr_by_cause|cause=collision").value.point(),
            Some(1.0)
        );
    }

    #[test]
    fn a_thin_bin_reports_insufficient_rather_than_a_point_estimate() {
        let mut p = CommsProvider::new(0); // default min_samples = 30
        p.on_event(&rx(1, 2, 10.0, true, 1_000));
        let s = p.flush(1_000_000_000);
        let bin = sample(&s, "pdr|dist_bin=0-25");
        assert!(bin.value.is_insufficient(), "{:?}", bin.value);
        assert_eq!(bin.value.n(), 1);
        assert_eq!(bin.value.point(), None);
    }

    #[test]
    fn an_empty_window_yields_insufficient_and_never_nan() {
        let mut p = CommsProvider::new(0);
        let s = p.flush(1_000_000_000);
        for key in ["pdr", "per", "pir"] {
            let v = &sample(&s, key).value;
            assert!(v.is_insufficient(), "{key}: {v:?}");
            assert_eq!(v.point(), None, "{key}");
        }
        for f in s.iter().flat_map(MetricSample::floats) {
            assert!(f.is_finite(), "{f} is not finite");
        }
    }

    #[test]
    fn receptions_without_a_ground_truth_distance_are_labelled_not_dropped() {
        let mut p = CommsProvider::new(0).with_min_samples(1);
        p.on_event(&rec(
            "phy.rx",
            json!({"t_start":0,"t_end":10,"tx":1,"rx":2,"outcome":"ok"}),
        ));
        let s = p.flush(1_000_000_000);
        assert_eq!(sample(&s, "pdr|dist_bin=unbinned").value.point(), Some(1.0));
        assert_eq!(sample(&s, "pdr").value.n(), 1);
    }

    #[test]
    fn cbr_is_recomputed_from_busy_over_window() {
        let mut p = CommsProvider::new(0).with_min_samples(1);
        // The producer's own `cbr` field is deliberately wrong; busy/window wins.
        p.on_event(&rec(
            "mac.cbr",
            json!({"t":0,"node":1,"channel":180,"cbr":0.9,"busy_us":25_000,"window_us":100_000}),
        ));
        p.on_event(&rec(
            "mac.cbr",
            json!({"t":0,"node":2,"channel":180,"cbr":0.9,"busy_us":75_000,"window_us":100_000}),
        ));
        let s = p.flush(1_000_000_000);
        let v = &sample(&s, "cbr|channel=180").value;
        // Mean of 0.25 and 0.75.
        assert_eq!(v.point(), Some(0.5));
        assert_eq!(v.n(), 2);
        // And a producer that reports only the ratio is taken at its word.
        let mut p = CommsProvider::new(0).with_min_samples(1);
        p.on_event(&rec("mac.cbr", json!({"t":0,"node":1,"cbr":0.375})));
        let s = p.flush(1_000_000_000);
        assert_eq!(sample(&s, "cbr").value.point(), Some(0.375));
    }

    #[test]
    fn pir_pairs_successive_receptions_per_link() {
        let mut p = CommsProvider::new(0).with_min_samples(1);
        // Two receptions from tx 1 at rx 2, 100 ms apart (t_end at 1 ms and 101 ms).
        p.on_event(&rec(
            "phy.rx",
            json!({"t_start":0,"t_end":1_000_000,"tx":1,"rx":2,"outcome":"ok"}),
        ));
        p.on_event(&rec(
            "phy.rx",
            json!({"t_start":100_000_000,"t_end":101_000_000,"tx":1,"rx":2,"outcome":"ok"}),
        ));
        // A single reception on another link contributes no gap.
        p.on_event(&rec(
            "phy.rx",
            json!({"t_start":0,"t_end":1_000_000,"tx":9,"rx":2,"outcome":"ok"}),
        ));
        let s = p.flush(1_000_000_000);
        let v = &sample(&s, "pir").value;
        assert_eq!(v.n(), 1, "one gap from two receptions on one link");
        assert_eq!(v.point(), Some(0.1));
    }

    #[test]
    fn goodput_and_bytes_are_rates_over_the_window() {
        let mut p = CommsProvider::new(0);
        // Two 400 B transmissions and two 100 B deliveries in a 2 s window.
        p.on_event(&rec(
            "node.tx",
            json!({"t":0,"node":1,"msg":1,"bytes_on_wire":400,"airtime_us":600}),
        ));
        p.on_event(&rec(
            "node.tx",
            json!({"t":0,"node":1,"msg":2,"bytes_on_wire":400,"airtime_us":600}),
        ));
        for receiver in [2, 3] {
            p.on_event(&rec(
                "node.rx",
                json!({"t":1_000,"rx":receiver,"tx":1,"outcome":"delivered","payload_bytes":100}),
            ));
        }
        // A loss carries no goodput, whatever its payload.
        p.on_event(&rec(
            "node.rx",
            json!({"t":1_000,"rx":4,"tx":1,"outcome":"lost","cause":"fading","payload_bytes":100}),
        ));
        p.on_event(&rec(
            "net.bytes",
            json!({"t":0,"bucket":"backhaul","bytes_on_wire":1_000,"id":5}),
        ));
        let s = p.flush(2_000_000_000);
        // 800 B of air in 2 s.
        assert_eq!(
            sample(&s, "bytes_air|bucket=air").value.point(),
            Some(400.0)
        );
        assert_eq!(
            sample(&s, "bytes_backhaul|bucket=backhaul").value.point(),
            Some(500.0)
        );
        // Every bucket is reported, including the empty ones: a zero is a measurement.
        assert_eq!(
            sample(&s, "bytes_uu_ul|bucket=cellular-ul").value.point(),
            Some(0.0)
        );
        // 200 B delivered in 2 s.
        assert_eq!(sample(&s, "goodput").value.point(), Some(100.0));
        // The total is the buckets' sum: 800 B of air and 1000 B of backhaul in 2 s.
        assert_eq!(sample(&s, "bytes_total").value.point(), Some(900.0));
        // 1.2 ms of airtime in 2 s = 0.6 ms/s.
        assert_eq!(
            sample(&s, "airtime_per_node|node=1").value.point(),
            Some(0.6)
        );
    }

    #[test]
    fn a_zero_length_window_reports_insufficient_rather_than_dividing() {
        let mut p = CommsProvider::new(1_000);
        p.on_event(&rec(
            "node.tx",
            json!({"t":1_000,"node":1,"bytes_on_wire":400,"airtime_us":600}),
        ));
        let s = p.flush(1_000);
        assert!(sample(&s, "bytes_air|bucket=air").value.is_insufficient());
        assert!(
            sample(&s, "airtime_per_node|node=1")
                .value
                .is_insufficient()
        );
        assert!(sample(&s, "goodput").value.is_insufficient());
    }

    /// Permuting the event stream must not change a byte of the output. The events are
    /// independent of each other here (different nodes, different links), so the run is the
    /// same run in either order — which is exactly the case a thread-count change produces.
    #[test]
    fn airtime_and_bytes_are_order_independent() {
        let events: Vec<OwnedRecord> = (0..8)
            .map(|i| {
                rec(
                    "node.tx",
                    json!({"t": i * 1_000, "node": i % 3, "bytes_on_wire": 100 + i,
                           "airtime_us": 500 + i * 7}),
                )
            })
            .collect();
        let run = |order: Vec<usize>| {
            let mut p = CommsProvider::new(0);
            for i in order {
                p.on_event(&events[i]);
            }
            p.flush(1_000_000_000)
                .into_iter()
                .map(|s| {
                    (
                        s.key(),
                        s.floats().iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let forward = run((0..8).collect());
        let backward = run((0..8).rev().collect());
        let shuffled = run(vec![3, 7, 0, 5, 1, 6, 2, 4]);
        assert_eq!(forward, backward);
        assert_eq!(forward, shuffled);
    }

    #[test]
    fn an_undecodable_record_is_counted_not_swallowed() {
        let mut p = CommsProvider::new(0);
        p.on_event(&rec("phy.rx", json!({"nonsense": true})));
        assert_eq!(p.rejected(), 1);
    }

    #[test]
    fn a_non_candidate_reception_is_not_a_trial() {
        let mut p = CommsProvider::new(0).with_min_samples(1);
        p.on_event(&rec(
            "phy.rx",
            json!({"t_start":0,"t_end":1,"rx":2,"outcome":"lost","cause":"below-sensitivity",
                   "candidate":false}),
        ));
        let s = p.flush(1_000_000_000);
        assert_eq!(sample(&s, "pdr").value.n(), 0);
    }

    #[test]
    fn the_subscription_list_is_the_channels_it_reads() {
        let p = CommsProvider::new(0);
        let subs: Vec<&str> = p.subscribe().iter().map(|c| c.as_str()).collect();
        assert_eq!(
            subs,
            vec![
                "node.tx",
                "phy.rx",
                "mac.cbr",
                "node.rx",
                "net.bytes",
                "proto.msg"
            ]
        );
    }
}
