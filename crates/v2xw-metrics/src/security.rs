//! Security-processing metrics: verification rate and cost, certificate attachment,
//! envelope overhead, revocation latency, CRL size over time
//! (08-measurement-and-data.md §2.2 and §2.3).
//!
//! | Metric | Formula | Unit | What it does **not** account for |
//! |---|---|---|---|
//! | `verify_rate` | verifications completed / window | 1/s | tasks still queued at the window's end, and tasks dropped before they ran — those are `verify_drops` |
//! | `verify_cost` | distribution of the per-task cost | ms | the cost of *not* verifying: a policy that skips verification records no task, so a run with a low mean cost may simply be verifying less |
//! | `verify_wait` | start − enqueue | ms | a task that never started, which has no wait to report and is therefore missing from the distribution rather than counted as an infinite wait |
//! | `verify_queue_depth` | distribution of the depth at enqueue | count | the depth between enqueues: this is sampled at arrivals, so it is an arrival-biased estimate of the queue, not a time average |
//! | `unverified_ratio` | delivered without verification / delivered | ratio | messages that were dropped rather than delivered |
//! | `full_cert_share` | messages carrying a full certificate / all messages | ratio | peer-to-peer certificate distribution responses, which carry certificates outside the ordinary message flow |
//! | `envelope_overhead` | Σ envelope bytes / Σ payload bytes | ratio | network and MAC headers, which are neither envelope nor payload; and it is a **ratio of sums**, so it carries no confidence interval |
//! | `revocation_latency_stage` | t(stage) − t(previous stage) for one revocation | s | a revocation whose stages a protocol does not emit: a missing stage produces no sample, and invariant I-P4 is what reports the omission |
//! | `crl_entries`, `crl_bytes` | the list's size as last published | count, B | per-node views of the list: this is the published list, not what any node holds |
//!
//! # The stage order
//!
//! 05-protocols.md §8 fixes the stage ids and their order, and [`STAGE_ORDER`] is that
//! table. A latency is measured between **consecutive stages that were both emitted**, so a
//! protocol that skips `resolved` reports `decision → issued` rather than nothing; the stage
//! dimension names the pair, so a reader can see which transition a number describes.

use std::collections::BTreeMap;

use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::cards;
use crate::channels::{
    ChannelView, NodeTelemetryView, NodeTxView, NodeVerifyView, ProtoRevocationView, SignerId,
    VerifyOutcome, decode,
};
use crate::def::{Agg, DEFAULT_LEVEL, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::{Decoded, MetricProvider};
use crate::quant::Quantum;
use crate::stats::{ConfidenceLevel, Distribution, Estimate, Proportion, ratio_of_sums};

/// The revocation stage ids in the order 05-protocols.md §8 fixes them.
///
/// A protocol need not emit all of them — the table's ETSI column has no `published` for
/// vehicles, and the threshold placeholder has no `processed` — so the latency metric pairs
/// consecutive stages **that were emitted**, and invariant I-P4 is what reports a stage a
/// protocol should have emitted and did not.
pub const STAGE_ORDER: [&str; 10] = [
    "detect",
    "report_sent",
    "report_received",
    "decision",
    "resolved",
    "issued",
    "published",
    "downloaded",
    "processed",
    "enforced",
];

/// The rank of a stage in [`STAGE_ORDER`], or `None` for a stage this build does not know.
#[must_use]
pub fn stage_rank(stage: &str) -> Option<usize> {
    STAGE_ORDER.iter().position(|s| *s == stage)
}

/// The security-processing metric provider.
///
/// Windowed: the verification, attachment and overhead metrics are per flush window.
/// Cumulative: the revocation stage timestamps are kept for the whole run, because a
/// revocation's stages span windows by construction, and the CRL's last observed size is
/// carried forward so a window with no CRL event still reports the list's size.
pub struct SecurityProvider {
    card: ModelCard,
    level: ConfidenceLevel,
    min_samples: u64,
    window_start: SimTime,

    /// Completed verifications per primitive, in the window. Integer, so exact.
    verifications: BTreeMap<String, u64>,
    /// Per-task costs in milliseconds.
    cost_ms: Distribution,
    /// Enqueue-to-start waits in milliseconds.
    wait_ms: Distribution,
    /// Queue depth at enqueue.
    queue_depth: Distribution,
    /// Deliveries, and whether they were delivered without verification.
    unverified: Proportion,
    /// Messages, and whether they carried a full certificate.
    full_cert: Proportion,
    /// Envelope and payload bytes, as integers.
    envelope_bytes: u64,
    payload_bytes: u64,
    /// Messages behind the overhead ratio.
    envelope_messages: u64,

    /// Stage timestamps per revocation id, cumulative over the run.
    stages: BTreeMap<String, BTreeMap<String, SimTime>>,
    /// Stage transitions already reported, so a cumulative store does not report the same
    /// transition in every window.
    reported: BTreeMap<(String, String), ()>,
    /// The last published list size.
    crl_entries: Option<u64>,
    crl_bytes: Option<u64>,

    rejected: u64,
}

impl SecurityProvider {
    /// A provider whose first window starts at `t0`.
    #[must_use]
    pub fn new(t0: SimTime) -> Self {
        Self {
            card: Self::build_card(),
            level: DEFAULT_LEVEL,
            min_samples: crate::stats::DEFAULT_MIN_SAMPLES,
            window_start: t0,
            verifications: BTreeMap::new(),
            cost_ms: Distribution::new(),
            wait_ms: Distribution::new(),
            queue_depth: Distribution::new(),
            unverified: Proportion::new(),
            full_cert: Proportion::new(),
            envelope_bytes: 0,
            payload_bytes: 0,
            envelope_messages: 0,
            stages: BTreeMap::new(),
            reported: BTreeMap::new(),
            crl_entries: None,
            crl_bytes: None,
            rejected: 0,
        }
    }

    /// Sets the insufficiency threshold.
    #[must_use]
    pub const fn with_min_samples(mut self, n: u64) -> Self {
        self.min_samples = n;
        self
    }

    /// Sets the confidence level.
    #[must_use]
    pub const fn with_level(mut self, level: ConfidenceLevel) -> Self {
        self.level = level;
        self
    }

    /// The stage timestamps collected so far, per revocation id — what invariant I-P4 is
    /// checked against.
    #[must_use]
    pub const fn stages(&self) -> &BTreeMap<String, BTreeMap<String, SimTime>> {
        &self.stages
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/security/envelope-and-verification",
            "1.0.0",
            "Signature verification rate, cost and wait; verification queue depth; the share \
             of messages delivered unverified; certificate attachment rate; security \
             envelope overhead as a fraction of payload; revocation latency per stage; CRL \
             size over time.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "verify_rate",
                "verify_rate = completed verifications / window length, in 1/s, per primitive",
            ),
            v2xw_core::card::Equation::new(
                "unverified_ratio",
                "unverified_ratio = messages delivered to applications without verification / \
                 messages delivered",
            ),
            v2xw_core::card::Equation::new(
                "full_cert_share",
                "full_cert_share = messages carrying a full certificate / all messages",
            ),
            v2xw_core::card::Equation::new(
                "envelope_overhead",
                "envelope_overhead = Σ envelope bytes / Σ application payload bytes; a ratio \
                 of sums, not a proportion, so no binomial interval applies",
            ),
            v2xw_core::card::Equation::new(
                "revocation_latency_stage",
                "for one revocation id, t(stage k) − t(stage k−1) over the consecutive \
                 emitted stages of 05-protocols.md §8",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.sources = vec![
            cards::design("08-measurement-and-data.md §2.2 (security processing)"),
            cards::design("08-measurement-and-data.md §2.3 (revocation and residual harm)"),
            cards::design("05-protocols.md §8 (revocation latency decomposition, stage ids)"),
            cards::standard(
                "IEEE 1609.2 and ETSI TS 103 097: the envelope whose bytes `envelope_overhead` \
                 divides by the payload",
            ),
            cards::paper(
                "B. Brecht et al., A Security Credential Management System for V2X \
                 Communications, IEEE T-ITS 2018, arXiv:1802.05323 (the SCMS revocation path \
                 the stage list follows)",
            ),
        ];
        card.limitations = vec![
            "verify_queue_depth is sampled at arrivals, so it estimates the queue as an \
             arriving task sees it, which is not the time average."
                .to_string(),
            "envelope_overhead is a ratio of two byte sums and carries no confidence \
             interval; the two sums are reported so a reader can form their own."
                .to_string(),
            "A revocation stage that a protocol never emits produces no latency sample. That \
             is reported by invariant I-P4, not smoothed over here."
                .to_string(),
            "crl_entries and crl_bytes describe the published list, not what any node holds; \
             a node that has not downloaded it holds an older list."
                .to_string(),
        ];
        card.ignores = vec![
            "Cryptographic correctness: outcomes come from the crypto backend, which \
             guarantees them identical between the modeled and real modes (I-S1)."
                .to_string(),
            "Privacy metrics (linkability, anonymity set), which are 07-threats §6.".to_string(),
        ];
        card.validation.tests = vec![
            "security::tests::verify_rate_and_cost_reproduce_a_hand_computed_fixture".to_string(),
            "security::tests::revocation_latency_pairs_consecutive_emitted_stages".to_string(),
            "security::tests::envelope_overhead_is_a_ratio_of_sums_without_an_interval".to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let sec22 = cards::design("08-measurement-and-data.md §2.2");
        let sec23 = cards::design("08-measurement-and-data.md §2.3");
        vec![
            MetricDef::new(
                "verify_rate",
                "1/s",
                Agg::Rate,
                Visibility::Node,
                Quantum::COUNT,
                "Verifications completed per second, per cryptographic primitive.",
            )
            .with_dims([Dim::T, Dim::Primitive])
            .with_source(sec22.clone())
            .with_min_samples(1)
            .not_accounting_for("tasks still queued when the window ended")
            .not_accounting_for("tasks dropped by policy or overflow before they ran"),
            MetricDef::new(
                "verify_cost",
                "ms",
                Agg::Distribution,
                Visibility::Node,
                Quantum::TIME_MS,
                "The per-task verification cost, from the primitive's cost table for the \
                 node's hardware profile (modeled mode) or measured (real mode).",
            )
            .with_dims([Dim::T, Dim::Primitive])
            .with_source(sec22.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for(
                "the cost of not verifying: a skipping policy records no task, so a low mean \
                 may mean less verification rather than cheaper verification",
            )
            .not_accounting_for("time the task spent waiting, which is verify_wait"),
            MetricDef::new(
                "verify_wait",
                "ms",
                Agg::Distribution,
                Visibility::Node,
                Quantum::TIME_MS,
                "Enqueue to start of verification.",
            )
            .with_dims([Dim::T])
            .with_source(sec22.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("a task that never started, which contributes no sample"),
            MetricDef::new(
                "verify_queue_depth",
                "count",
                Agg::Distribution,
                Visibility::Node,
                Quantum::COUNT,
                "The verification queue's depth, sampled when a task is enqueued.",
            )
            .with_dims([Dim::T])
            .with_source(sec22.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for(
                "the depth between arrivals: this is arrival-biased, not a time average",
            ),
            MetricDef::new(
                "unverified_ratio",
                "ratio",
                Agg::ratio("delivered without verification", "delivered"),
                Visibility::Node,
                Quantum::RATIO,
                "Messages delivered to applications without verification, divided by \
                 messages delivered.",
            )
            .with_dims([Dim::T])
            .with_source(sec22.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("messages that were dropped rather than delivered"),
            MetricDef::new(
                "full_cert_share",
                "ratio",
                Agg::ratio("messages carrying a full certificate", "all messages"),
                Visibility::Node,
                Quantum::RATIO,
                "The certificate attachment rate: messages whose signer identifier is a full \
                 certificate rather than a digest, divided by all messages.",
            )
            .with_dims([Dim::T])
            .with_source(sec22.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for(
                "peer-to-peer certificate distribution responses, which carry certificates \
                 outside the ordinary message flow",
            )
            .not_accounting_for("messages whose signer identifier the producer did not record"),
            MetricDef::new(
                "envelope_overhead",
                "ratio",
                Agg::ratio("envelope bytes", "payload bytes"),
                Visibility::Node,
                Quantum::RATIO,
                "Security envelope bytes divided by application payload bytes, over the \
                 window. A ratio of two sums, so it carries **no** confidence interval; both \
                 sums are reported.",
            )
            .with_dims([Dim::T])
            .with_source(sec22)
            .with_min_samples(1)
            .not_accounting_for("network and MAC headers, which are neither envelope nor payload")
            .not_accounting_for("a binomial interval, which does not apply to a ratio of sums"),
            MetricDef::new(
                "revocation_latency_stage",
                "s",
                Agg::Distribution,
                // A CRL is published to everyone by design (03-interfaces.md §14).
                Visibility::Public,
                Quantum::TIME_S,
                "The time between consecutive emitted stages of one revocation \
                 (05-protocols.md §8). The stage dimension names the transition, e.g. \
                 `decision->issued`.",
            )
            .with_dims([Dim::T, Dim::Stage])
            .with_source(cards::design("05-protocols.md §8"))
            .with_min_samples(1)
            .not_accounting_for("stages a protocol does not emit, which invariant I-P4 reports")
            .not_accounting_for(
                "per-node enforcement spread: this is the transition's distribution, not t95",
            ),
            MetricDef::new(
                "crl_entries",
                "count",
                Agg::Max,
                Visibility::Public,
                Quantum::COUNT,
                "The number of entries in the revocation list as last published.",
            )
            .with_dims([Dim::T])
            .with_source(sec23.clone())
            .with_min_samples(1)
            .not_accounting_for("what any individual node holds, which lags the published list"),
            MetricDef::new(
                "crl_bytes",
                "B",
                Agg::Max,
                Visibility::Public,
                Quantum::BYTES,
                "The revocation list's size in bytes as last published.",
            )
            .with_dims([Dim::T])
            .with_source(sec23)
            .with_min_samples(1)
            .not_accounting_for("compression on the distribution path")
            .not_accounting_for("what any individual node holds"),
        ]
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    fn on_tx(&mut self, v: &NodeTxView) {
        if let Some(signer) = v.signer {
            self.full_cert.observe(signer == SignerId::Certificate);
        }
        if let (Some(env), Some(payload)) = (v.envelope_bytes, v.payload_bytes) {
            self.envelope_bytes += env;
            self.payload_bytes += payload;
            self.envelope_messages += 1;
        }
    }

    fn on_verify(&mut self, v: &NodeVerifyView) {
        if let Some(d) = v.queue_depth {
            self.queue_depth.observe(d as f64);
        }
        match v.outcome {
            VerifyOutcome::Valid | VerifyOutcome::Invalid => {
                let primitive = v
                    .primitive
                    .clone()
                    .unwrap_or_else(|| "unspecified".to_string());
                *self.verifications.entry(primitive).or_insert(0) += 1;
                if let Some(us) = v.cost_us {
                    self.cost_ms.observe((us as f64) / 1000.0);
                }
                if let Some(start) = v.t_start
                    && start >= v.t_enqueue
                {
                    self.wait_ms.observe(((start - v.t_enqueue) as f64) / 1e6);
                }
                self.unverified.observe(false);
            }
            VerifyOutcome::Skipped => {
                // Delivered to the application without verification.
                self.unverified.observe(true);
            }
            VerifyOutcome::Dropped => {
                // Neither verified nor delivered: it is in neither term of
                // `unverified_ratio`, which is what "divided by delivered" means.
            }
        }
    }

    fn on_revocation(&mut self, v: &ProtoRevocationView) {
        self.stages
            .entry(v.id.clone())
            .or_default()
            // A stage reached twice for one revocation keeps the first instant: a stage is
            // a threshold crossing, and a protocol that re-publishes a list has not
            // re-reached `published` for a revocation already on it.
            .entry(v.stage.clone())
            .or_insert(v.t);
        if let Some(e) = v.entries {
            self.crl_entries = Some(e);
        }
        if let Some(b) = v.size_bytes {
            self.crl_bytes = Some(b);
        }
    }

    fn on_telemetry(&mut self, v: &NodeTelemetryView) {
        if let Some(d) = v.verify_queue_depth {
            self.queue_depth.observe(d as f64);
        }
    }

    /// The stage-to-stage latencies that have become measurable and are not yet reported.
    ///
    /// Keyed by `"from->to"`, which is the `stage` dimension's value. Iteration is over
    /// `BTreeMap`s, so the transitions are produced in a fixed order.
    fn new_transitions(&mut self) -> BTreeMap<String, Distribution> {
        let mut out: BTreeMap<String, Distribution> = BTreeMap::new();
        let mut newly_reported: Vec<(String, String)> = Vec::new();
        for (id, stages) in &self.stages {
            // The emitted stages of this revocation, in the canonical order. A stage the
            // build does not know is skipped rather than guessed at.
            let mut emitted: Vec<(usize, &String, SimTime)> = stages
                .iter()
                .filter_map(|(name, t)| stage_rank(name).map(|r| (r, name, *t)))
                .collect();
            emitted.sort_by_key(|(r, _, _)| *r);
            for pair in emitted.windows(2) {
                let (_, from, t_from) = &pair[0];
                let (_, to, t_to) = &pair[1];
                let transition = format!("{from}->{to}");
                if self
                    .reported
                    .contains_key(&(id.clone(), transition.clone()))
                {
                    continue;
                }
                if t_to >= t_from {
                    out.entry(transition.clone())
                        .or_default()
                        .observe(Duration::between(*t_from, *t_to).as_secs_f64());
                }
                newly_reported.push((id.clone(), transition));
            }
        }
        for key in newly_reported {
            self.reported.insert(key, ());
        }
        out
    }

    fn window_secs(&self, at: SimTime) -> Option<f64> {
        if at <= self.window_start {
            return None;
        }
        Some(Duration::between(self.window_start, at).as_secs_f64())
    }
}

impl Model for SecurityProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for SecurityProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![
            NodeTxView::channel_name(),
            NodeVerifyView::channel_name(),
            NodeTelemetryView::channel_name(),
            ProtoRevocationView::channel_name(),
        ]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        self.on_decoded(&Decoded::new(ev));
    }

    fn on_decoded(&mut self, d: &Decoded<'_>) {
        let ev = d.record();
        match ev.channel {
            NodeTxView::CHANNEL => d.with(|v: Option<&NodeTxView>| match v {
                Some(v) => self.on_tx(v),
                None => self.rejected += 1,
            }),
            NodeVerifyView::CHANNEL => match decode::<NodeVerifyView>(ev) {
                Ok(v) => self.on_verify(&v),
                Err(_) => self.rejected += 1,
            },
            NodeTelemetryView::CHANNEL => match decode::<NodeTelemetryView>(ev) {
                Ok(v) => self.on_telemetry(&v),
                Err(_) => self.rejected += 1,
            },
            ProtoRevocationView::CHANNEL => match decode::<ProtoRevocationView>(ev) {
                Ok(v) => self.on_revocation(&v),
                Err(_) => self.rejected += 1,
            },
            _ => {}
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();
        let secs = self.window_secs(at);

        let rate_def = self.def("verify_rate");
        for (primitive, n) in core::mem::take(&mut self.verifications) {
            let mut dims = Dims::new();
            dims.insert(Dim::Primitive, DimValue::label(primitive));
            let value = match secs {
                Some(s) => Estimate::Value {
                    point: (n as f64) / s,
                    n,
                },
                None => Estimate::Insufficient { n, required: 1 },
            };
            out.push(MetricSample::new(
                &rate_def,
                at,
                dims,
                SampleValue::Scalar(value),
            ));
        }

        let cost = core::mem::replace(&mut self.cost_ms, Distribution::new());
        out.push(MetricSample::new(
            &self.def("verify_cost"),
            at,
            Dims::new(),
            SampleValue::Distribution(cost.summary(self.min_samples)),
        ));
        let wait = core::mem::replace(&mut self.wait_ms, Distribution::new());
        out.push(MetricSample::new(
            &self.def("verify_wait"),
            at,
            Dims::new(),
            SampleValue::Distribution(wait.summary(self.min_samples)),
        ));
        let depth = core::mem::replace(&mut self.queue_depth, Distribution::new());
        out.push(MetricSample::new(
            &self.def("verify_queue_depth"),
            at,
            Dims::new(),
            SampleValue::Distribution(depth.summary(self.min_samples)),
        ));

        let unverified = core::mem::replace(&mut self.unverified, Proportion::new());
        out.push(MetricSample::new(
            &self.def("unverified_ratio"),
            at,
            Dims::new(),
            SampleValue::Ratio(unverified.estimate(self.min_samples, self.level)),
        ));
        let full_cert = core::mem::replace(&mut self.full_cert, Proportion::new());
        out.push(MetricSample::new(
            &self.def("full_cert_share"),
            at,
            Dims::new(),
            SampleValue::Ratio(full_cert.estimate(self.min_samples, self.level)),
        ));

        let env = core::mem::take(&mut self.envelope_bytes);
        let payload = core::mem::take(&mut self.payload_bytes);
        let msgs = core::mem::take(&mut self.envelope_messages);
        out.push(MetricSample::new(
            &self.def("envelope_overhead"),
            at,
            Dims::new(),
            SampleValue::Ratio(ratio_of_sums(env as f64, payload as f64, msgs, 1)),
        ));

        let stage_def = self.def("revocation_latency_stage");
        for (transition, d) in self.new_transitions() {
            let mut dims = Dims::new();
            dims.insert(Dim::Stage, DimValue::label(transition));
            out.push(MetricSample::new(
                &stage_def,
                at,
                dims,
                SampleValue::Distribution(d.summary(1)),
            ));
        }

        // The CRL's size is carried forward: a window with no CRL event still reports the
        // list as it stands, which is what "CRL size over time" means. Before the first
        // publication there is no list, and nothing is reported rather than a zero.
        if let Some(e) = self.crl_entries {
            out.push(MetricSample::new(
                &self.def("crl_entries"),
                at,
                Dims::new(),
                SampleValue::count(e),
            ));
        }
        if let Some(b) = self.crl_bytes {
            out.push(MetricSample::new(
                &self.def("crl_bytes"),
                at,
                Dims::new(),
                SampleValue::count(b),
            ));
        }

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
    use serde_json::json;
    use v2xw_core::ctx::OwnedRecord;

    fn rec(channel: &'static str, json: serde_json::Value) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility: Visibility::Node,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    fn sample<'a>(samples: &'a [MetricSample], key: &str) -> &'a MetricSample {
        samples.iter().find(|s| s.key() == key).unwrap_or_else(|| {
            panic!(
                "no sample with key {key}; have {:?}",
                samples.iter().map(MetricSample::key).collect::<Vec<_>>()
            )
        })
    }

    #[test]
    fn every_definition_validates_and_the_card_is_accepted() {
        let p = SecurityProvider::new(0);
        p.validate_defs().unwrap();
        p.card().validate().unwrap();
        p.card().check_api_version().unwrap();
    }

    /// Four verifications in a 2 s window at 1.2 ms each: a rate of 2/s and a mean cost of
    /// 1.2 ms, with waits of 1, 2, 3 and 4 ms.
    #[test]
    fn verify_rate_and_cost_reproduce_a_hand_computed_fixture() {
        let mut p = SecurityProvider::new(0).with_min_samples(1);
        for i in 1..=4u64 {
            p.on_event(&rec(
                "node.verify",
                json!({
                    "t_enqueue": 0, "t_start": i * 1_000_000, "t_done": i * 2_000_000,
                    "node": 1, "primitive": "ecdsa-p256", "cost_us": 1_200,
                    "outcome": "valid", "queue_depth": i
                }),
            ));
        }
        let s = p.flush(2_000_000_000);
        assert_eq!(
            sample(&s, "verify_rate|primitive=ecdsa-p256").value.point(),
            Some(2.0)
        );
        assert_eq!(sample(&s, "verify_cost").value.point(), Some(1.2));
        // Waits 1, 2, 3, 4 ms: mean 2.5 ms, p50 2.5 ms.
        let wait = &sample(&s, "verify_wait").value;
        assert_eq!(wait.point(), Some(2.5));
        assert_eq!(wait.n(), 4);
        // Queue depths 1..4: mean 2.5.
        assert_eq!(sample(&s, "verify_queue_depth").value.point(), Some(2.5));
        // All four were verified, so nothing was delivered unverified.
        assert_eq!(sample(&s, "unverified_ratio").value.point(), Some(0.0));
    }

    #[test]
    fn a_skipped_verification_is_an_unverified_delivery_and_a_dropped_one_is_neither() {
        let mut p = SecurityProvider::new(0).with_min_samples(1);
        p.on_event(&rec(
            "node.verify",
            json!({"t_enqueue":0,"node":1,"outcome":"valid","cost_us":1000,"primitive":"p"}),
        ));
        p.on_event(&rec(
            "node.verify",
            json!({"t_enqueue":0,"node":1,"outcome":"skipped"}),
        ));
        p.on_event(&rec(
            "node.verify",
            json!({"t_enqueue":0,"node":1,"outcome":"dropped"}),
        ));
        let s = p.flush(1_000_000_000);
        let v = &sample(&s, "unverified_ratio").value;
        assert_eq!(v.point(), Some(0.5), "one of two deliveries was unverified");
        assert_eq!(v.n(), 2, "the dropped task is in neither term");
    }

    #[test]
    fn the_certificate_attachment_rate_counts_full_certificates() {
        let mut p = SecurityProvider::new(0).with_min_samples(1);
        for signer in ["certificate", "digest", "digest", "digest"] {
            p.on_event(&rec(
                "node.tx",
                json!({"t":0,"node":1,"bytes_on_wire":300,"signer":signer}),
            ));
        }
        // A message whose signer the producer did not record is in neither term.
        p.on_event(&rec("node.tx", json!({"t":0,"node":1,"bytes_on_wire":300})));
        let s = p.flush(1_000_000_000);
        let v = &sample(&s, "full_cert_share").value;
        assert_eq!(v.point(), Some(0.25));
        assert_eq!(v.n(), 4);
    }

    #[test]
    fn envelope_overhead_is_a_ratio_of_sums_without_an_interval() {
        let mut p = SecurityProvider::new(0);
        // Two messages: 120 B of envelope over 300 B of payload each.
        for _ in 0..2 {
            p.on_event(&rec(
                "node.tx",
                json!({"t":0,"node":1,"bytes_on_wire":420,"envelope_bytes":120,
                       "payload_bytes":300}),
            ));
        }
        let s = p.flush(1_000_000_000);
        let v = &sample(&s, "envelope_overhead").value;
        assert_eq!(v.point(), Some(0.4));
        assert_eq!(v.n(), 2);
        match v {
            SampleValue::Ratio(r) => {
                assert_eq!(
                    r.interval(),
                    None,
                    "a ratio of sums carries no Wilson interval"
                );
                assert!(matches!(
                    r,
                    crate::stats::RatioEstimate::RatioOfSums {
                        numerator: 240.0,
                        denominator: 600.0,
                        ..
                    }
                ));
            }
            other => panic!("expected a ratio, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_payload_does_not_divide_by_zero() {
        let mut p = SecurityProvider::new(0);
        p.on_event(&rec(
            "node.tx",
            json!({"t":0,"node":1,"bytes_on_wire":120,"envelope_bytes":120,"payload_bytes":0}),
        ));
        let s = p.flush(1_000_000_000);
        assert!(sample(&s, "envelope_overhead").value.is_insufficient());
    }

    #[test]
    fn revocation_latency_pairs_consecutive_emitted_stages() {
        let mut p = SecurityProvider::new(0);
        // A protocol that skips `resolved`: decision at 1 s, issued at 3.5 s, published at 4 s.
        for (stage, t) in [
            ("detect", 0u64),
            ("decision", 1_000_000_000),
            ("issued", 3_500_000_000),
            ("published", 4_000_000_000),
        ] {
            p.on_event(&rec(
                "proto.revocation",
                json!({"t":t,"stage":stage,"id":"rev-1"}),
            ));
        }
        let s = p.flush(5_000_000_000);
        assert_eq!(
            sample(&s, "revocation_latency_stage|stage=detect->decision")
                .value
                .point(),
            Some(1.0)
        );
        assert_eq!(
            sample(&s, "revocation_latency_stage|stage=decision->issued")
                .value
                .point(),
            Some(2.5),
            "the skipped `resolved` stage does not break the chain"
        );
        assert_eq!(
            sample(&s, "revocation_latency_stage|stage=issued->published")
                .value
                .point(),
            Some(0.5)
        );
        // A transition is reported once, not in every window.
        let s = p.flush(6_000_000_000);
        assert!(
            !s.iter().any(|x| x.metric == "revocation_latency_stage"),
            "a cumulative store must not re-report a transition"
        );
    }

    #[test]
    fn the_crl_size_is_carried_forward_and_absent_before_the_first_publication() {
        let mut p = SecurityProvider::new(0);
        let s = p.flush(1_000_000_000);
        assert!(!s.iter().any(|x| x.metric == "crl_entries"), "no list yet");
        p.on_event(&rec(
            "proto.revocation",
            json!({"t":1_500_000_000u64,"stage":"published","id":"rev-1",
                   "entries":42,"size_bytes":1_337}),
        ));
        let s = p.flush(2_000_000_000);
        assert_eq!(sample(&s, "crl_entries").value, SampleValue::count(42));
        assert_eq!(sample(&s, "crl_bytes").value, SampleValue::count(1_337));
        // Carried into the next window with no new event.
        let s = p.flush(3_000_000_000);
        assert_eq!(sample(&s, "crl_entries").value, SampleValue::count(42));
    }

    #[test]
    fn an_empty_window_is_insufficient_and_never_nan() {
        let mut p = SecurityProvider::new(0);
        let s = p.flush(1_000_000_000);
        for key in [
            "verify_cost",
            "verify_wait",
            "verify_queue_depth",
            "unverified_ratio",
            "full_cert_share",
            "envelope_overhead",
        ] {
            assert!(sample(&s, key).value.is_insufficient(), "{key}");
        }
        for f in s.iter().flat_map(MetricSample::floats) {
            assert!(f.is_finite(), "{f}");
        }
    }

    #[test]
    fn the_stage_order_is_the_one_05_protocols_fixes() {
        assert_eq!(stage_rank("detect"), Some(0));
        assert_eq!(stage_rank("decision"), Some(3));
        assert_eq!(stage_rank("enforced"), Some(9));
        assert_eq!(stage_rank("something-a-protocol-invented"), None);
    }
}
