//! End-to-end delay, decomposed: the general trace, and the provider that reduces it.
//!
//! # The trace
//!
//! A message's latency is not one number but a journey: it waits for the signer, is
//! signed, waits for the channel, is on the air, propagates, is parsed, waits for the
//! verifier and is verified. A backend flow is the same shape with other stages — a
//! cellular uplink, a registration authority's processing, a backhaul hop, a CRL's
//! distribution. [`LatencyTrace`] is that shape, once, for every flow:
//!
//! * an origin instant (the message's generation, or a request's issue);
//! * an ordered list of [`Span`]s, each naming a **stage** and a **hop**, that tile the
//!   interval from the origin to the delivery with no gap and no overlap.
//!
//! Because the spans tile the interval, their durations sum to the end-to-end latency
//! *exactly*, in integer nanoseconds — the decomposition cannot drift from the total, and
//! [`LatencyTrace::validate`] refuses a trace for which that is not true (a gap, an overlap,
//! a negative stage). The consistency requirement "latency stages sum to the end-to-end
//! latency" is therefore a property of the type, and a producer that violates it produces
//! a rejected trace that [`LatencyProvider`] counts on `latency_trace_rejected` rather than
//! a plausible-looking wrong number.
//!
//! # How a flow plugs in
//!
//! * **V2V** messages are decomposed from `node.rx`, which carries every stamp of the
//!   journey; [`crate::channels::NodeRxView::latency_trace`] builds the trace with the
//!   stages of [`crate::channels::V2V_STAGES`] and the flow name `v2v`.
//! * **Any other flow** — multi-hop forwarding, credential top-up, misbehaviour reporting,
//!   CRL distribution — emits a [`LatencyTrace`] record on the `msg.latency` channel, built
//!   with [`TraceBuilder`], with its own flow name and its own stage names. Nothing in this
//!   module needs to know them in advance: a stage is a label, and the provider reduces
//!   whatever stages a flow reports. Multi-hop flows set [`TraceBuilder::hop`] between
//!   hops, so a relay's queueing is distinguishable from the originator's.
//!
//! # Metrics
//!
//! | Metric | Formula | Unit |
//! |---|---|---|
//! | `e2e_latency` | delivered − origin, per delivered message | ms (distribution: p50, p95, p99) |
//! | `latency_stage` | one stage's duration, per delivered message | ms (distribution) |
//! | `latency_stage_share` | Σ one stage's durations / Σ end-to-end latencies | ratio of sums |
//! | `latency_trace_rejected` | traces that failed [`LatencyTrace::validate`] | count |
//!
//! `latency_stage_share` is a ratio of *sums*, so over one window and one flow the shares
//! of all stages add up to exactly one (before quantisation); the mean of per-message
//! shares would not, and would weight a 1 ms message like a 100 ms one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Record, Visibility};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;

use crate::cards;
use crate::channels::{ChannelView, NodeRxView, decode};
use crate::def::{Agg, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::MetricProvider;
use crate::quant::Quantum;
use crate::stats::{Distribution, ratio_of_sums};

/// The flow name V2V broadcast messages are reduced under.
pub const FLOW_V2V: &str = "v2v";

/// The channel a [`LatencyTrace`] is recorded on.
pub const MSG_LATENCY: &str = "msg.latency";

/// One stage of one message's journey: `[start, end)` on the true timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// The stage's name, e.g. `sign_queue`, `airtime`, `uu_uplink`.
    pub stage: String,
    /// Which hop of the flow it belongs to, from 0 at the originator.
    #[serde(default)]
    pub hop: u16,
    /// When the stage began.
    pub start: SimTime,
    /// When it ended.
    pub end: SimTime,
}

impl Span {
    /// The stage's duration, ns, or `None` if it ends before it starts.
    #[must_use]
    pub fn duration_ns(&self) -> Option<u64> {
        self.end.checked_sub(self.start)
    }
}

/// Why a trace is not a decomposition of its message's latency.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TraceDefect {
    /// The trace has no stages.
    #[error("the trace has no stages")]
    Empty,
    /// The first stage does not begin at the origin.
    #[error("the first stage '{stage}' begins at {start}, not at the origin {origin}")]
    NotFromOrigin {
        /// The stage.
        stage: String,
        /// Its start.
        start: SimTime,
        /// The origin.
        origin: SimTime,
    },
    /// A stage ends before it begins.
    #[error("stage '{stage}' ends at {end}, before it begins at {start}")]
    Negative {
        /// The stage.
        stage: String,
        /// Its start.
        start: SimTime,
        /// Its end.
        end: SimTime,
    },
    /// A stage does not begin where the previous one ended.
    #[error("stage '{stage}' begins at {start}, but the previous stage ended at {previous_end}")]
    NotContiguous {
        /// The stage.
        stage: String,
        /// Its start.
        start: SimTime,
        /// Where the previous stage ended.
        previous_end: SimTime,
    },
}

impl TraceDefect {
    /// The instants the defect is about, named, for a violation report.
    #[must_use]
    pub fn instants(&self) -> Vec<(&'static str, SimTime)> {
        match self {
            TraceDefect::Empty => Vec::new(),
            TraceDefect::NotFromOrigin { start, origin, .. } => {
                vec![("origin_ns", *origin), ("start_ns", *start)]
            }
            TraceDefect::Negative { start, end, .. } => {
                vec![("end_ns", *end), ("start_ns", *start)]
            }
            TraceDefect::NotContiguous {
                start,
                previous_end,
                ..
            } => vec![("previous_end_ns", *previous_end), ("start_ns", *start)],
        }
    }
}

/// One message's latency, decomposed into contiguous stages (the `msg.latency` record).
///
/// Built with [`TraceBuilder`]; checked with [`LatencyTrace::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencyTrace {
    /// The flow this message belongs to: `v2v`, or a backend flow's own name
    /// (`credential-topup`, `mbr`, `crl-download`, …).
    pub flow: String,
    /// The message type, where one applies.
    #[serde(default)]
    pub msg_type: Option<String>,
    /// The message id, for joining to `node.tx` and `node.rx`.
    #[serde(default)]
    pub msg: Option<u64>,
    /// The node that originated the message (ground truth when it is a vehicle).
    #[serde(default)]
    pub origin: Option<NodeId>,
    /// The node or entity it was delivered to.
    #[serde(default)]
    pub dest: Option<NodeId>,
    /// When the message came into existence: its generation, or a request's issue.
    pub t_origin: SimTime,
    /// The stages, in order, tiling `[t_origin, delivered)`.
    pub spans: Vec<Span>,
}

impl LatencyTrace {
    /// The instant the last stage ended — the delivery — or `None` for an empty trace.
    #[must_use]
    pub fn end(&self) -> Option<SimTime> {
        self.spans.last().map(|s| s.end)
    }

    /// The end-to-end latency, ns: delivery − origin.
    #[must_use]
    pub fn total_ns(&self) -> Option<u64> {
        self.end()?.checked_sub(self.t_origin)
    }

    /// Checks the trace tiles `[t_origin, end)`: non-empty, starting at the origin, every
    /// stage non-negative and beginning where the previous one ended.
    ///
    /// # Errors
    /// The first [`TraceDefect`] found.
    pub fn validate(&self) -> Result<(), TraceDefect> {
        let first = self.spans.first().ok_or(TraceDefect::Empty)?;
        if first.start != self.t_origin {
            return Err(TraceDefect::NotFromOrigin {
                stage: first.stage.clone(),
                start: first.start,
                origin: self.t_origin,
            });
        }
        let mut previous_end = self.t_origin;
        for s in &self.spans {
            if s.start != previous_end {
                return Err(TraceDefect::NotContiguous {
                    stage: s.stage.clone(),
                    start: s.start,
                    previous_end,
                });
            }
            if s.end < s.start {
                return Err(TraceDefect::Negative {
                    stage: s.stage.clone(),
                    start: s.start,
                    end: s.end,
                });
            }
            previous_end = s.end;
        }
        Ok(())
    }

    /// Each stage's total duration, ns, keyed by stage name — hops of one stage summed.
    ///
    /// Only meaningful for a trace that [`LatencyTrace::validate`]s; for one that does,
    /// the values sum to [`LatencyTrace::total_ns`] exactly.
    #[must_use]
    pub fn stage_ns(&self) -> BTreeMap<String, u64> {
        let mut out: BTreeMap<String, u64> = BTreeMap::new();
        for s in &self.spans {
            *out.entry(s.stage.clone()).or_insert(0) += s.duration_ns().unwrap_or(0);
        }
        out
    }
}

impl ChannelView for LatencyTrace {
    const CHANNEL: &'static str = MSG_LATENCY;
}

impl Record for LatencyTrace {
    const CHANNEL: &'static str = MSG_LATENCY;
    const VISIBILITY: Visibility = Visibility::Node;

    /// A trace that names its endpoints' node ids carries ground truth (which vehicle sent
    /// it), so it is tagged the way `phy.rx` is.
    fn visibility(&self) -> Visibility {
        if self.origin.is_some() || self.dest.is_some() {
            Visibility::NodeAndGt
        } else {
            Visibility::Node
        }
    }
}

/// Builds a [`LatencyTrace`] one stage at a time, so a producer names each stage's *end*
/// and contiguity holds by construction.
///
/// ```
/// use v2xw_metrics::latency::TraceBuilder;
/// let mut b = TraceBuilder::new("credential-topup", None, Some(7), 1_000);
/// b.to("uu_uplink", 21_000_000);
/// b.to("ra_processing", 31_000_000);
/// b.hop(1).to("uu_downlink", 52_000_000);
/// let t = b.finish();
/// t.validate().unwrap();
/// assert_eq!(t.total_ns(), Some(52_000_000 - 1_000));
/// assert_eq!(t.stage_ns().values().sum::<u64>(), t.total_ns().unwrap());
/// ```
#[derive(Debug, Clone)]
pub struct TraceBuilder {
    trace: LatencyTrace,
    cursor: SimTime,
    hop: u16,
}

impl TraceBuilder {
    /// A trace of `flow` that originated at `t_origin`.
    #[must_use]
    pub fn new(
        flow: impl Into<String>,
        msg_type: Option<String>,
        msg: Option<u64>,
        t_origin: SimTime,
    ) -> Self {
        Self {
            trace: LatencyTrace {
                flow: flow.into(),
                msg_type,
                msg,
                origin: None,
                dest: None,
                t_origin,
                spans: Vec::new(),
            },
            cursor: t_origin,
            hop: 0,
        }
    }

    /// Names the endpoints.
    pub fn endpoints(&mut self, origin: Option<NodeId>, dest: Option<NodeId>) -> &mut Self {
        self.trace.origin = origin;
        self.trace.dest = dest;
        self
    }

    /// The following stages belong to hop `hop`.
    pub fn hop(&mut self, hop: u16) -> &mut Self {
        self.hop = hop;
        self
    }

    /// Appends the stage `stage`, from where the previous one ended to `end`.
    ///
    /// `end` is taken as given, even if it lies before the cursor: a trace whose stamps
    /// run backwards is a producer defect, and [`LatencyTrace::validate`] is what reports
    /// it. Clamping here would hide the defect behind a zero-length stage.
    pub fn to(&mut self, stage: impl Into<String>, end: SimTime) -> &mut Self {
        self.trace.spans.push(Span {
            stage: stage.into(),
            hop: self.hop,
            start: self.cursor,
            end,
        });
        self.cursor = end;
        self
    }

    /// The trace.
    #[must_use]
    pub fn finish(self) -> LatencyTrace {
        self.trace
    }
}

/// Accumulated latency of one (flow, message type) key over one window.
#[derive(Debug, Default)]
struct FlowAcc {
    e2e_ms: Distribution,
    total_ns: u64,
    stages: BTreeMap<String, StageAcc>,
    n: u64,
}

#[derive(Debug, Default)]
struct StageAcc {
    ms: Distribution,
    ns: u64,
}

/// The latency provider: `e2e_latency`, `latency_stage`, `latency_stage_share` and
/// `latency_trace_rejected`, per flow and per message type.
pub struct LatencyProvider {
    card: ModelCard,
    min_samples: u64,
    /// Keyed by (flow, message type or `None` for the all-types aggregate).
    acc: BTreeMap<(String, Option<String>), FlowAcc>,
    rejected_traces: u64,
    rejected: u64,
}

impl LatencyProvider {
    /// A provider with the crate's default insufficiency threshold.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: Self::build_card(),
            min_samples: crate::stats::DEFAULT_MIN_SAMPLES,
            acc: BTreeMap::new(),
            rejected_traces: 0,
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
            "metric/latency/decomposed",
            "1.0.0",
            "End-to-end delay of every delivered message, decomposed into contiguous stages \
             (signing queue, signing, channel access, air time, propagation, reception, \
             verification queue, verification for V2V; any stages a backend flow names), \
             with p50, p95 and p99 per flow and per message type and each stage's share.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "e2e_latency",
                "latency = t_delivered − t_origin = Σ over stages (t_end − t_start)",
            ),
            v2xw_core::card::Equation::new(
                "latency_stage_share",
                "share(stage) = Σ_messages d(stage) / Σ_messages latency",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.sources = vec![
            cards::design(
                "08-measurement-and-data.md §2.1 (e2e_latency: generation to application delivery, including queueing, air time and verification)",
            ),
            cards::standard(
                "SAE J2945/1 §6.3 and ETSI TS 102 637-1 (now TR 102 638): the end-to-end latency \
                 budgets (100 ms for cooperative awareness) a decomposition is read against",
            ),
            cards::standard(
                "IEEE 802.11-2020 §10.23.2 (EDCA: AIFS and backoff) for the three channel-access \
                 stages",
            ),
        ];
        card.limitations = vec![
            "The channel-access delay is split AIFS first, then the backoff slots the MAC \
             reported, then the remainder as deferral; a real EDCA interleaves them, so the \
             three stages are an accounting of how much of each, not of when."
                .to_string(),
            "Only delivered messages have a latency; the metric describes the survivors and \
             must be read beside the delivery ratio."
                .to_string(),
            "A stage whose start and end fall in different windows is attributed to the \
             window in which the message was delivered."
                .to_string(),
        ];
        card.ignores =
            vec!["The application's own processing after the message is handed to it.".to_string()];
        card.validation.tests = vec![
            "latency::tests::stages_sum_to_the_total_exactly".to_string(),
            "latency::tests::a_trace_with_a_gap_is_rejected_and_counted".to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let src = cards::design("08-measurement-and-data.md §2.1 (e2e_latency)");
        vec![
            MetricDef::new(
                "e2e_latency",
                "ms",
                Agg::Distribution,
                Visibility::Node,
                Quantum::TIME_MS,
                "End-to-end delay of each delivered message: from its generation at the \
                 sender to its delivery to the receiver's applications, including signing, \
                 channel access, air time, propagation, parsing and verification. With no \
                 dimension it is every V2V delivery; `msg_type` narrows it to one message \
                 type; `flow` names a non-V2V flow (a backend exchange, a multi-hop relay).",
            )
            .with_dims([Dim::T, Dim::Flow, Dim::MsgType])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("messages that were never delivered, which have no latency")
            .not_accounting_for("the application's own processing after delivery"),
            MetricDef::new(
                "latency_stage",
                "ms",
                Agg::Distribution,
                Visibility::Node,
                Quantum::TIME_MS,
                "The time each delivered message spent in one stage of its journey. For V2V: \
                 sign_queue, sign, mac_aifs, mac_backoff, mac_defer (deferral to a busy \
                 medium, including repeated AIFS), airtime, propagation, reception (parsing \
                 and the verification policy), verify_queue, verify. The stages tile the \
                 journey, so they sum to e2e_latency message by message.",
            )
            .with_dims([Dim::T, Dim::Flow, Dim::MsgType, Dim::Stage])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("undelivered messages")
            .not_accounting_for(
                "the interleaving of AIFS, backoff and deferral inside the access delay",
            ),
            MetricDef::new(
                "latency_stage_share",
                "ratio",
                Agg::ratio("Σ time in this stage", "Σ end-to-end latency"),
                Visibility::Node,
                Quantum::RATIO,
                "The share of the total end-to-end delay spent in one stage: the stage's \
                 summed duration over the summed latency of the same messages. The shares \
                 of all stages of one flow sum to one.",
            )
            .with_dims([Dim::T, Dim::Flow, Dim::MsgType, Dim::Stage])
            .with_source(src.clone())
            .with_min_samples(1)
            .with_range(0.0, 1.0)
            .not_accounting_for("undelivered messages")
            .not_accounting_for(
                "per-message variation: it is a ratio of sums, not a mean of ratios",
            ),
            MetricDef::new(
                "latency_trace_rejected",
                "count",
                Agg::Count,
                Visibility::Node,
                Quantum::COUNT,
                "Traces in the window that were not a valid decomposition of their message's \
                 latency (a gap, an overlap or a negative stage). Anything above zero is a \
                 producer defect, and those messages are missing from every latency metric.",
            )
            .with_dims([Dim::T])
            .with_source(src)
            .with_min_samples(1)
            .with_range(0.0, f64::INFINITY)
            .not_accounting_for("delivered messages whose stamps were absent altogether"),
        ]
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    /// Reduces one trace. `false` if it was rejected.
    fn observe(&mut self, trace: &LatencyTrace) -> bool {
        if trace.validate().is_err() {
            self.rejected_traces += 1;
            return false;
        }
        let Some(total) = trace.total_ns() else {
            self.rejected_traces += 1;
            return false;
        };
        let stages = trace.stage_ns();
        let mut keys = vec![(trace.flow.clone(), None)];
        if trace.msg_type.is_some() {
            keys.push((trace.flow.clone(), trace.msg_type.clone()));
        }
        for key in keys {
            let acc = self.acc.entry(key).or_default();
            acc.e2e_ms.observe(ns_to_ms(total));
            acc.total_ns += total;
            acc.n += 1;
            for (stage, ns) in &stages {
                let s = acc.stages.entry(stage.clone()).or_default();
                s.ms.observe(ns_to_ms(*ns));
                s.ns += ns;
            }
        }
        true
    }
}

impl Default for LatencyProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Integer nanoseconds to milliseconds: one correctly rounded division, identical on every
/// target.
fn ns_to_ms(ns: u64) -> f64 {
    (ns as f64) / 1e6
}

/// The dimensions of one (flow, message type) key: V2V's all-types aggregate has none, so
/// it is the headline series.
fn dims_of(flow: &str, msg_type: &Option<String>) -> Dims {
    let mut d = Dims::new();
    if flow != FLOW_V2V {
        d.insert(Dim::Flow, DimValue::label(flow));
    }
    if let Some(t) = msg_type {
        d.insert(Dim::MsgType, DimValue::label(t.clone()));
    }
    d
}

impl Model for LatencyProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for LatencyProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![
            NodeRxView::channel_name(),
            <LatencyTrace as ChannelView>::channel_name(),
        ]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        match ev.channel {
            NodeRxView::CHANNEL => match decode::<NodeRxView>(ev) {
                Ok(v) => {
                    if v.outcome == crate::channels::RxFate::Delivered {
                        match v.latency_trace() {
                            Some(t) => {
                                self.observe(&t);
                            }
                            // A delivery whose stamps do not close on its delivery instant
                            // is a decomposition that does not add up.
                            None if v.t_delivered.is_some() && v.t_generated.is_some() => {
                                self.rejected_traces += 1;
                            }
                            None => {}
                        }
                    }
                }
                Err(_) => self.rejected += 1,
            },
            MSG_LATENCY => match decode::<LatencyTrace>(ev) {
                Ok(t) => {
                    self.observe(&t);
                }
                Err(_) => self.rejected += 1,
            },
            _ => {}
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();
        let e2e = self.def("e2e_latency");
        let stage_def = self.def("latency_stage");
        let share_def = self.def("latency_stage_share");
        for ((flow, msg_type), acc) in core::mem::take(&mut self.acc) {
            let dims = dims_of(&flow, &msg_type);
            out.push(MetricSample::new(
                &e2e,
                at,
                dims.clone(),
                SampleValue::Distribution(acc.e2e_ms.summary(self.min_samples)),
            ));
            // Debug-checked here and tested in `stages_sum_to_the_total_exactly`: the
            // integer stage sums add up to the integer total, because every trace that got
            // this far tiled its interval.
            debug_assert_eq!(
                acc.stages.values().map(|s| s.ns).sum::<u64>(),
                acc.total_ns,
                "stage sums must equal the end-to-end total"
            );
            for (stage, s) in acc.stages {
                let mut d = dims.clone();
                d.insert(Dim::Stage, DimValue::label(stage));
                out.push(MetricSample::new(
                    &stage_def,
                    at,
                    d.clone(),
                    SampleValue::Distribution(s.ms.summary(self.min_samples)),
                ));
                out.push(MetricSample::new(
                    &share_def,
                    at,
                    d,
                    SampleValue::Ratio(ratio_of_sums(s.ns as f64, acc.total_ns as f64, acc.n, 1)),
                ));
            }
        }
        out.push(MetricSample::new(
            &self.def("latency_trace_rejected"),
            at,
            Dims::new(),
            SampleValue::count(core::mem::take(&mut self.rejected_traces)),
        ));
        out
    }

    fn rejected(&self) -> u64 {
        self.rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{NodeRxView, RxFate};
    use v2xw_core::ctx::OwnedRecord;

    fn rx(g0: u64, delivered: u64) -> NodeRxView {
        NodeRxView {
            t: delivered,
            rx: NodeId::new(2),
            tx: Some(NodeId::new(1)),
            msg: Some(9),
            msg_type: Some("bsm".into()),
            outcome: RxFate::Delivered,
            cause: None,
            verification: Some("verified".into()),
            rssi_dbm: Some(-70.0),
            sinr_db: Some(20.0),
            dist_m: Some(50.0),
            bytes_on_wire: Some(300),
            airtime_us: Some(424),
            payload_bytes: Some(40),
            t_generated: Some(g0),
            t_sign_start: Some(g0 + 1_000),
            t_signed: Some(g0 + 4_000),
            mac_aifs_ns: Some(58_000),
            mac_backoff_ns: Some(39_000),
            t_tx_start: Some(g0 + 4_000 + 150_000),
            t_tx_end: Some(g0 + 4_000 + 150_000 + 424_000),
            t_arrival: Some(g0 + 4_000 + 150_000 + 424_000 + 167),
            t_rx_done: Some(g0 + 578_167),
            t_verify_start: Some(g0 + 600_000),
            t_verify_done: Some(delivered),
            t_delivered: Some(delivered),
        }
    }

    fn record<R: Serialize>(channel: &'static str, r: &R) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility: Visibility::NodeAndGt,
            json: serde_json::to_vec(r).unwrap(),
        }
    }

    #[test]
    fn stages_sum_to_the_total_exactly() {
        let v = rx(1_000_000, 1_000_000 + 1_000_000);
        let t = v.latency_trace().expect("complete stamps build a trace");
        t.validate().unwrap();
        assert_eq!(t.spans.len(), crate::channels::V2V_STAGES.len());
        let names: Vec<&str> = t.spans.iter().map(|s| s.stage.as_str()).collect();
        assert_eq!(names, crate::channels::V2V_STAGES);
        assert_eq!(t.stage_ns().values().sum::<u64>(), t.total_ns().unwrap());
        assert_eq!(t.total_ns(), v.e2e_ns());
        // The access delay splits into AIFS, backoff and deferral that add back up.
        let s = t.stage_ns();
        assert_eq!(s["mac_aifs"] + s["mac_backoff"] + s["mac_defer"], 150_000);
        assert_eq!(s["mac_aifs"], 58_000);
        assert_eq!(s["propagation"], 167);
    }

    #[test]
    fn the_provider_reports_percentiles_and_shares_that_sum_to_one() {
        let mut p = LatencyProvider::new().with_min_samples(1);
        for i in 0..50u64 {
            let g0 = i * 100_000_000;
            let v = rx(g0, g0 + 1_000_000 + i * 10_000);
            p.on_event(&record("node.rx", &v));
        }
        let s = p.flush(10_000_000_000);
        let headline = s
            .iter()
            .find(|x| x.metric == "e2e_latency" && x.dims.is_empty())
            .expect("an all-types V2V sample");
        assert_eq!(headline.value.n(), 50);
        let shares: f64 = s
            .iter()
            .filter(|x| x.metric == "latency_stage_share" && !x.dims.contains_key(&Dim::MsgType))
            .map(|x| x.value.point().unwrap())
            .sum();
        assert!((shares - 1.0).abs() < 1e-3, "shares sum to {shares}");
        assert!(s.iter().any(|x| x.metric == "e2e_latency"
            && x.dims.get(&Dim::MsgType) == Some(&DimValue::label("bsm"))));
    }

    #[test]
    fn a_trace_with_a_gap_is_rejected_and_counted() {
        let mut p = LatencyProvider::new().with_min_samples(1);
        let mut v = rx(1_000_000, 3_000_000);
        // The signer "finished" before it started: the stamps run backwards.
        v.t_signed = Some(900_000);
        p.on_event(&record("node.rx", &v));
        let mut t = rx(0, 2_000_000).latency_trace().unwrap();
        t.spans[3].start += 1;
        p.on_event(&record("msg.latency", &t));
        let s = p.flush(1);
        let rejected = s
            .iter()
            .find(|x| x.metric == "latency_trace_rejected")
            .unwrap();
        assert_eq!(rejected.value.point(), Some(2.0));
        assert!(!s.iter().any(|x| x.metric == "e2e_latency"));
    }

    #[test]
    fn a_backend_flow_plugs_in_with_its_own_stages_and_hops() {
        let mut b = TraceBuilder::new("credential-topup", None, Some(4), 0);
        b.endpoints(Some(NodeId::new(3)), None);
        b.to("uu_uplink", 20_000_000)
            .to("ra_processing", 45_000_000)
            .hop(1)
            .to("backend", 47_000_000)
            .hop(2)
            .to("uu_downlink", 70_000_000);
        let t = b.finish();
        assert_eq!(Record::visibility(&t), Visibility::NodeAndGt);
        let mut p = LatencyProvider::new().with_min_samples(1);
        p.on_event(&record("msg.latency", &t));
        let s = p.flush(1_000_000_000);
        let e2e = s
            .iter()
            .find(|x| x.metric == "e2e_latency")
            .expect("one flow sample");
        assert_eq!(
            e2e.dims.get(&Dim::Flow),
            Some(&DimValue::label("credential-topup"))
        );
        assert_eq!(e2e.value.point(), Some(70.0));
    }

    #[test]
    fn an_undelivered_message_has_no_latency() {
        let mut v = rx(0, 1_000_000);
        v.outcome = RxFate::Lost;
        v.cause = Some("collision".into());
        assert!(v.latency_trace().is_none());
        let mut p = LatencyProvider::new().with_min_samples(1);
        p.on_event(&record("node.rx", &v));
        let s = p.flush(1);
        assert!(!s.iter().any(|x| x.metric == "e2e_latency"));
    }
}
