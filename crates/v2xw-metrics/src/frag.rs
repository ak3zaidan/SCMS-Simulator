//! Fragmentation: what splitting an oversize message costs in loss, measured against what
//! the independent-loss formula predicts (04-models.md §7.4).
//!
//! Every fragmented SDU is followed to one `net.reassembly` record per receiver that had
//! any of its fragments in range, carrying the realised outcome and the prediction built
//! from that receiver's own per-fragment PHY success probabilities. Five metrics reduce it:
//!
//! | Metric | Formula | Unit |
//! |---|---|---|
//! | `frag_sdu_loss` | groups in which some fragment did not decode / groups | ratio (Wilson interval) |
//! | `frag_sdu_loss_predicted` | mean over groups of `1 − Π (1 − p_i)` | ratio |
//! | `frag_fragment_loss` | fragments that did not decode / fragments | ratio (Wilson interval) |
//! | `frag_content_loss` | Σ payload octets not received / Σ payload octets | ratio of sums |
//! | `frag_content_loss_predicted` | mean over groups of `Σ w_i p_i` | ratio |
//! | `frag_sdu_loss_independent` | mean over groups of `1 − (1 − p̂)^n`, `p̂` = `frag_fragment_loss` | ratio |
//!
//! `frag_sdu_loss` above `frag_fragment_loss` is loss amplification: an SDU in `n` pieces is
//! lost when any one is. Two predictions stand beside it, and they answer different
//! questions:
//!
//! * `frag_sdu_loss_predicted` takes each fragment's `p_i` from the PHY's success
//!   probability *under the interference that fragment actually met* at that receiver. It
//!   tests the formula given the channel each fragment saw; since the PHY's draws are
//!   independent per (link, frame), it should match the realised loss up to sampling noise,
//!   and a gap would be a defect in the reassembly path.
//! * `frag_sdu_loss_independent` is the textbook `1 − (1 − p)^n` with `p` the measured
//!   per-fragment loss rate. Realised loss below it is §7.4's first assumption failing in
//!   the usual direction: the fragments of one SDU share a receiver, a distance and often
//!   an interferer, so they tend to be lost together, which makes the true SDU loss *lower*
//!   than independence says.
//!
//! A fragment that never reached a receiver (out of range, dropped at the sender's queue)
//! is certainly lost on every side. For independently interpretable segments the content
//! loss is the figure that matters (§7.3), and it does not amplify.
//!
//! # Visibility
//!
//! Ground truth: the prediction uses PHY success probabilities no receiver knows, and the
//! record names the sender.

use std::collections::BTreeMap;

use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;

use crate::cards;
use crate::channels::{ChannelView, NetReassemblyView};
use crate::def::{Agg, DEFAULT_LEVEL, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::{Decoded, MetricProvider};
use crate::quant::Quantum;
use crate::stats::{ConfidenceLevel, Estimate, Proportion, ratio_of_sums};

/// One window's sums for one message type.
#[derive(Debug, Default, Clone)]
struct Sums {
    groups: Proportion,
    fragments: Proportion,
    bytes: u64,
    bytes_lost: u64,
    predicted: Vec<f64>,
    predicted_content: Vec<f64>,
    /// Each group's fragment count, for the independence baseline.
    counts: Vec<u32>,
}

/// The fragmentation provider.
pub struct FragProvider {
    card: ModelCard,
    level: ConfidenceLevel,
    min_samples: u64,
    /// Keyed by message type, `None` the all-types aggregate.
    sums: BTreeMap<Option<String>, Sums>,
    rejected: u64,
}

impl Default for FragProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FragProvider {
    /// A provider at the crate's 95 % level and a one-group floor: a small sample is
    /// reported with the wide interval it deserves rather than refused.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: Self::build_card(),
            level: DEFAULT_LEVEL,
            min_samples: 1,
            sums: BTreeMap::new(),
            rejected: 0,
        }
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/net/fragmentation",
            "1.0.0",
            "Realised loss of fragmented SDUs, of their fragments and of their content, \
             beside the loss the per-fragment PHY success probabilities predict under \
             independent fragment loss (04-models.md §7.4).",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "frag_sdu_loss_predicted",
                "mean over (SDU, receiver) of 1 − Π_i (1 − p_i), p_i = 1 − PSR_i",
            ),
            v2xw_core::card::Equation::new(
                "frag_content_loss_predicted",
                "mean over (SDU, receiver) of Σ_i w_i p_i, w_i = payload_i / payload",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.sources = vec![
            cards::design("04-models.md §7.3 (the strategies) and §7.4 (loss amplification)"),
            cards::standard(
                "IEEE 802.11-2020 §10.2.7: group-addressed frames are neither acknowledged \
                 nor retransmitted, so a lost broadcast fragment stays lost",
            ),
        ];
        card.limitations = vec![
            "A group is recorded only at a receiver that had at least one of its fragments \
             in its arrival set; a receiver that heard none of them is not an attempt."
                .to_string(),
            "The per-fragment probability is the PHY's success probability under the \
             interference present during that fragment; under the hard-threshold capture \
             rule it is still the error model's, not the rule's 0/1."
                .to_string(),
        ];
        card.validation.tests = vec![
            "frag::tests::a_group_lost_to_one_fragment_amplifies_the_fragment_loss".to_string(),
            "frag::tests::segments_lose_content_not_messages".to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let src = cards::design("04-models.md §7.4");
        let ratio = |name: &str, num: &str, den: &str, def: &str| {
            MetricDef::new(
                name,
                "ratio",
                Agg::ratio(num.to_string(), den.to_string()),
                Visibility::Gt,
                Quantum::RATIO,
                def.to_string(),
            )
            .with_dims([Dim::T, Dim::MsgType])
            .with_source(src.clone())
            .with_min_samples(1)
            .with_range(0.0, 1.0)
        };
        vec![
            ratio(
                "frag_sdu_loss",
                "fragmented SDUs with a fragment that did not decode",
                "fragmented SDUs attempted",
                "The share of fragmented SDUs, per receiver that had any of their fragments \
                 in range, of which some fragment did not decode — the SDU's loss when the \
                 pieces mean nothing alone.",
            )
            .not_accounting_for("losses above reassembly (queue, verification), on node.rx"),
            ratio(
                "frag_sdu_loss_predicted",
                "Σ (1 − Π (1 − p_i))",
                "fragmented SDUs attempted",
                "The SDU loss 04-models.md §7.4 predicts from each fragment's PHY success \
                 probability at that receiver, if fragments were lost independently.",
            )
            .not_accounting_for("correlation between fragments, which is what the gap shows"),
            ratio(
                "frag_fragment_loss",
                "fragments that did not decode",
                "fragments of attempted SDUs",
                "The share of the fragments of attempted SDUs that did not decode at the \
                 receiver: the per-fragment loss the SDU loss amplifies.",
            )
            .not_accounting_for("fragments of SDUs no fragment of which reached the receiver"),
            ratio(
                "frag_content_loss",
                "Σ payload octets of fragments that did not decode",
                "Σ payload octets of attempted SDUs",
                "The share of the fragmented SDUs' payload that did not reach the receiver, \
                 which is what independently interpretable segments lose instead of whole \
                 messages.",
            )
            .not_accounting_for("content a decoded piece carried of an SDU never reassembled"),
            ratio(
                "frag_content_loss_predicted",
                "Σ Σ_i w_i p_i",
                "fragmented SDUs attempted",
                "The content loss 04-models.md §7.4 predicts from each fragment's PHY \
                 success probability, weighted by its share of the payload.",
            )
            .not_accounting_for("correlation between fragments"),
            ratio(
                "frag_sdu_loss_independent",
                "Σ (1 − (1 − p̂)^n_g)",
                "fragmented SDUs attempted",
                "The SDU loss if every fragment were lost independently at the window's own \
                 measured per-fragment loss rate p̂ (frag_fragment_loss): the textbook \
                 1 − (1 − p)^n of 04-models.md §7.4. Realised SDU loss below it means the \
                 fragments of one SDU tend to be lost together.",
            )
            .not_accounting_for("the per-fragment loss's dependence on distance and size"),
        ]
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    fn on_group(&mut self, v: &NetReassemblyView) {
        if v.fragments == 0 || v.received > v.fragments || v.bytes_received > v.bytes {
            self.rejected += 1;
            return;
        }
        let complete = v.received == v.fragments;
        for key in [None, v.msg_type.clone()] {
            let s = self.sums.entry(key).or_default();
            s.groups.observe(!complete);
            s.fragments
                .observe_many(u64::from(v.fragments - v.received), u64::from(v.fragments));
            s.bytes += v.bytes;
            s.bytes_lost += v.bytes - v.bytes_received;
            s.predicted.push(v.predicted_loss);
            s.predicted_content.push(v.predicted_content_loss);
            s.counts.push(v.fragments);
            if v.msg_type.is_none() {
                break;
            }
        }
    }
}

impl Model for FragProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for FragProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![NetReassemblyView::channel_name()]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        self.on_decoded(&Decoded::new(ev));
    }

    fn on_decoded(&mut self, ev: &Decoded<'_>) {
        if ev.channel() == NetReassemblyView::CHANNEL {
            ev.with(|v: Option<&NetReassemblyView>| match v {
                Some(v) => self.on_group(v),
                None => self.rejected += 1,
            });
        }
    }

    /// Nothing in a window with no fragmented SDU: a run that never fragments carries no
    /// column of refusals for a mechanism it did not use.
    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();
        let defs = [
            self.def("frag_sdu_loss"),
            self.def("frag_sdu_loss_predicted"),
            self.def("frag_fragment_loss"),
            self.def("frag_content_loss"),
            self.def("frag_content_loss_predicted"),
            self.def("frag_sdu_loss_independent"),
        ];
        for (msg_type, s) in core::mem::take(&mut self.sums) {
            let mut dims = Dims::new();
            if let Some(t) = msg_type {
                dims.insert(Dim::MsgType, DimValue::label(t));
            }
            let n = s.groups.trials();
            let mean = |xs: Vec<f64>| {
                SampleValue::Scalar(Estimate::Value {
                    point: v2xw_core::math::sum_ordered(xs) / (n as f64),
                    n,
                })
            };
            // 1 − (1 − p̂)^n by repeated multiplication: exact IEEE operations in a fixed
            // order, where a library `powi` is not guaranteed to be.
            let p_hat = s.fragments.estimate(1, self.level).point().unwrap_or(0.0);
            let independent: Vec<f64> = s
                .counts
                .iter()
                .map(|&n| {
                    let mut survive = 1.0;
                    for _ in 0..n {
                        survive *= 1.0 - p_hat;
                    }
                    1.0 - survive
                })
                .collect();
            let values = [
                SampleValue::Ratio(s.groups.estimate(self.min_samples, self.level)),
                mean(s.predicted),
                SampleValue::Ratio(s.fragments.estimate(self.min_samples, self.level)),
                SampleValue::Ratio(ratio_of_sums(s.bytes_lost as f64, s.bytes as f64, n, 1)),
                mean(s.predicted_content),
                mean(independent),
            ];
            for (def, value) in defs.iter().zip(values) {
                out.push(MetricSample::new(def, at, dims.clone(), value));
            }
        }
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

    #[allow(clippy::too_many_arguments)]
    fn group(
        fragments: u32,
        received: u32,
        bytes: u64,
        bytes_received: u64,
        predicted: f64,
        predicted_content: f64,
    ) -> OwnedRecord {
        let json = serde_json::json!({
            "t": 1, "rx": 2, "tx": 1, "sdu": 7, "strategy": "fragmenter/generic-sdu",
            "kind": "message", "msg_type": "bsm", "fragments": fragments,
            "received": received, "bytes": bytes, "bytes_received": bytes_received,
            "predicted_loss": predicted, "predicted_content_loss": predicted_content,
            "outcome": if received == fragments { "complete" } else { "lost" },
        });
        OwnedRecord {
            channel: "net.reassembly",
            visibility: Visibility::Gt,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    fn point(s: &[MetricSample], name: &str) -> Option<f64> {
        s.iter()
            .find(|x| x.metric == name && x.dims.is_empty())
            .and_then(|x| x.value.point())
    }

    /// Two SDUs in two pieces: one whole, one missing a piece. Half the SDUs are lost but
    /// only a quarter of the fragments — the amplification.
    #[test]
    fn a_group_lost_to_one_fragment_amplifies_the_fragment_loss() {
        let mut p = FragProvider::new();
        p.on_event(&group(2, 2, 2_000, 2_000, 0.19, 0.1));
        p.on_event(&group(2, 1, 2_000, 1_000, 0.51, 0.3));
        let s = p.flush(1_000_000_000);
        assert_eq!(point(&s, "frag_sdu_loss"), Some(0.5));
        assert_eq!(point(&s, "frag_fragment_loss"), Some(0.25));
        assert_eq!(point(&s, "frag_content_loss"), Some(0.25));
        assert!((point(&s, "frag_sdu_loss_predicted").unwrap() - 0.35).abs() < 1e-12);
        assert!((point(&s, "frag_content_loss_predicted").unwrap() - 0.2).abs() < 1e-12);
        // Independent pieces at the measured quarter: 1 − 0.75² for each two-piece SDU.
        assert!((point(&s, "frag_sdu_loss_independent").unwrap() - 0.4375).abs() < 1e-12);
        assert_eq!(p.rejected(), 0);
        // A window with nothing fragmented says nothing.
        assert!(p.flush(2_000_000_000).is_empty());
    }

    /// Segments are delivered one by one: a missing segment loses its share of the content.
    #[test]
    fn segments_lose_content_not_messages() {
        let mut p = FragProvider::new();
        p.on_event(&group(4, 3, 4_000, 3_000, 0.4, 0.1));
        let s = p.flush(1_000_000_000);
        assert_eq!(point(&s, "frag_content_loss"), Some(0.25));
        assert_eq!(point(&s, "frag_fragment_loss"), Some(0.25));
        // A record claiming more than it had is refused, not counted.
        p.on_event(&group(2, 3, 2_000, 2_000, 0.0, 0.0));
        assert_eq!(p.rejected(), 1);
    }
}
