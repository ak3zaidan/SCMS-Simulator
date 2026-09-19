//! Loss amplification (04-models.md §7.4) and the hook that measures it.
//!
//! An SDU carried in `n` fragments that are lost independently with probabilities `p_i` is
//! itself lost with probability
//!
//! ```text
//! P_sdu = 1 - prod_i (1 - p_i)
//! ```
//!
//! This module implements **that** formula, not the equal-`p` simplification
//! `1 - (1 - p)^n`. §7.4 is explicit about why: fragments differ in size, so they differ in
//! PER, and the model is required to take the per-fragment `p_i` the PHY computed.
//! [`equal_p_loss`] exists only to reproduce the section's worked example and is itself
//! implemented by calling the general function with `n` equal entries, so the simplification
//! can never drift away from the formula it simplifies.
//!
//! # The four assumptions, which are limitations
//!
//! §7.4 lists them, and every fragmenter card repeats them (invariant I-N2):
//!
//! 1. **Independence between fragments.** Violated when fragments share a fading state or a
//!    burst of interference, which makes the true loss *lower* than the formula predicts.
//! 2. **Equal `p` for every fragment.** Violated when fragments differ in size — which is
//!    why this module takes per-fragment `p_i` and the equal-`p` form is only an example.
//! 3. **No retransmission.** True for group-addressed frames: 802.11 does not retransmit
//!    them and never fragments them at the MAC (04-models.md §4.6).
//! 4. **A reassembly timeout longer than the spread of fragment arrivals.** Otherwise
//!    timeouts add to the loss, and the formula understates it.
//!
//! Assumptions 1 and 4 are the reason [`AmplificationMeter`] exists: it accumulates the
//! predicted `P_sdu` next to the realised outcome, so a metric provider can report the two
//! together and the *gap* between them is the visible signature of correlated fragment loss
//! (04-models.md §7.4: "Metric providers report `P_sdu` measured against the formula so that
//! correlation effects are visible").
//!
//! # Determinism
//!
//! The product runs over fragments **sorted by index**, and the index is the fragment's id
//! within its SDU, so the reduction is id-ordered exactly as 02-architecture.md §6.4
//! requires; floating-point multiplication is correctly rounded per operation, so a fixed
//! order gives bit-identical results on every target. The content-weighted mean uses
//! [`v2xw_core::math::sum_sorted_by_key`], which is that rule in one call. The meter's
//! running sum uses the same compensated recurrence as
//! [`v2xw_core::math::sum_ordered`] — [`AmplificationMeter::total_predicted`] is asserted
//! equal to it in this module's tests — and merging per-node meters goes through
//! [`AmplificationMeter::merged`], which sorts by [`NodeId`] first.

use serde::{Deserialize, Serialize};
use v2xw_core::card::Equation;
use v2xw_core::ids::NodeId;
use v2xw_core::math;

/// One fragment's contribution to the amplification formula.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FragmentLoss {
    /// The fragment's index within its SDU. Also the key the reduction is ordered by.
    pub index: u16,
    /// The probability this fragment is lost, from the PHY. Clamped into `0.0..=1.0`.
    pub p: f64,
    /// The SDU bytes this fragment carries, which weight the content-loss figure.
    pub payload_bytes: u32,
}

impl FragmentLoss {
    /// A fragment loss with an index, a probability and a payload size.
    pub const fn new(index: u16, p: f64, payload_bytes: u32) -> Self {
        Self {
            index,
            p,
            payload_bytes,
        }
    }
}

/// What a set of per-fragment loss probabilities implies for the SDU.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SduLossModel {
    /// How many fragments the SDU was split into.
    pub fragments: u16,
    /// `1 - prod(1 - p_i)`: the probability that **at least one** fragment is lost.
    ///
    /// For a strategy that amplifies loss this is the SDU's loss probability; for
    /// independently interpretable segments it is only the probability that *some* content
    /// is missing.
    pub p_any_fragment_lost: f64,
    /// `prod(1 - p_i)`: the probability that every fragment arrives.
    pub p_all_arrive: f64,
    /// The expected fraction of the SDU's content that is lost, weighted by each
    /// fragment's payload: `sum(w_i * p_i)` with `w_i = payload_i / total_payload`.
    pub expected_content_lost: f64,
    /// Whether losing one fragment loses the whole SDU.
    ///
    /// `true` for [`crate::frag::generic`] and [`crate::frag::cert_cycle`], where the
    /// receiver needs every fragment; `false` for [`crate::frag::facilities`], where each
    /// segment is a complete, separately signed message.
    pub amplifies: bool,
}

impl SduLossModel {
    /// The SDU's loss probability under this strategy: [`SduLossModel::p_any_fragment_lost`]
    /// when the strategy amplifies loss, and the content-weighted figure when it does not.
    pub const fn p_sdu_lost(&self) -> f64 {
        if self.amplifies {
            self.p_any_fragment_lost
        } else {
            self.expected_content_lost
        }
    }

    /// The amplification factor relative to a single fragment of the same mean loss:
    /// `p_sdu_lost / mean(p_i)`, or `1.0` when nothing can be lost.
    ///
    /// The number that makes the cost of fragmenting legible: at `p` = 0.1 and five
    /// fragments it is 4.1.
    pub fn amplification_factor(&self, mean_p: f64) -> f64 {
        if mean_p <= 0.0 {
            1.0
        } else {
            self.p_sdu_lost() / mean_p
        }
    }
}

/// Clamps a probability into `0.0..=1.0`.
///
/// A `NaN` becomes 1.0 rather than 0.0: a probability that is not a number cannot be
/// assumed harmless, and the pessimistic choice makes the bug visible in the output instead
/// of hiding it. The debug assertion makes it visible in a test first.
fn clamp_p(p: f64) -> f64 {
    debug_assert!(
        p.is_finite(),
        "a per-fragment loss probability must be finite, got {p}"
    );
    if p.is_nan() { 1.0 } else { p.clamp(0.0, 1.0) }
}

/// `P_sdu = 1 - prod(1 - p_i)` and its companions, for a set of per-fragment losses.
///
/// `amplifies` says whether the receiver needs every fragment; see
/// [`SduLossModel::amplifies`]. An empty set means nothing was sent, so nothing can be
/// lost.
///
/// ```
/// use v2xw_net::amplification::{FragmentLoss, sdu_loss};
///
/// // Three fragments with unequal loss: 1 - 0.9*0.8*0.95 = 0.316.
/// let m = sdu_loss(
///     &[
///         FragmentLoss::new(0, 0.10, 400),
///         FragmentLoss::new(1, 0.20, 400),
///         FragmentLoss::new(2, 0.05, 200),
///     ],
///     true,
/// );
/// assert!((m.p_any_fragment_lost - 0.316).abs() < 1e-12);
/// assert_eq!(m.fragments, 3);
/// ```
pub fn sdu_loss(fragments: &[FragmentLoss], amplifies: bool) -> SduLossModel {
    let mut ordered: Vec<FragmentLoss> = fragments.to_vec();
    // The fragment index is the id of the contributor, and the reduction is ordered by it
    // (02-architecture.md §6.4). Stable sort: duplicate indices keep their input order,
    // which a caller that passes them must have made deterministic itself.
    ordered.sort_by_key(|f| f.index);

    // Ordered product of the survival probabilities. Each multiplication is correctly
    // rounded, so a fixed order is bit-reproducible.
    let mut p_all_arrive = 1.0_f64;
    for f in &ordered {
        p_all_arrive *= 1.0 - clamp_p(f.p);
    }

    let total_payload: u64 = ordered.iter().map(|f| u64::from(f.payload_bytes)).sum();
    let expected_content_lost = if ordered.is_empty() {
        0.0
    } else if total_payload == 0 {
        // No payload weights to go on: every fragment counts the same.
        let n = ordered.len() as f64;
        math::sum_sorted_by_key(ordered.iter().map(|f| (f.index, clamp_p(f.p) / n)))
    } else {
        let total = total_payload as f64;
        math::sum_sorted_by_key(
            ordered
                .iter()
                .map(|f| (f.index, clamp_p(f.p) * (f64::from(f.payload_bytes) / total))),
        )
    };

    SduLossModel {
        fragments: u16::try_from(ordered.len()).unwrap_or(u16::MAX),
        p_any_fragment_lost: 1.0 - p_all_arrive,
        p_all_arrive,
        expected_content_lost,
        amplifies,
    }
}

/// The equal-`p` simplification `1 - (1 - p)^n`, for reproducing 04-models.md §7.4's worked
/// example.
///
/// Implemented by calling [`sdu_loss`] with `n` equal entries, so it cannot drift from the
/// general formula. Model code uses [`sdu_loss`] with the PHY's per-fragment values; this
/// exists for documentation and for tests.
///
/// ```
/// use v2xw_net::amplification::equal_p_loss;
/// // §7.4: "at p = 0.1 a five-fragment certificate cycle loses the certificate in 41 % of
/// // cycles".
/// assert!((equal_p_loss(5, 0.1) - 0.40951).abs() < 1e-12);
/// // One fragment cannot amplify anything: P_sdu is p, to within one rounding of 1 - (1 - p).
/// assert!((equal_p_loss(1, 0.1) - 0.1).abs() < 1e-16);
/// ```
pub fn equal_p_loss(n: u16, p: f64) -> f64 {
    let fragments: Vec<FragmentLoss> = (0..n).map(|i| FragmentLoss::new(i, p, 1)).collect();
    sdu_loss(&fragments, true).p_any_fragment_lost
}

/// The four assumptions 04-models.md §7.4 attaches to the formula, in its own order.
///
/// Every [`crate::frag::Fragmenter`] card carries these verbatim in its `limitations`,
/// which is the half of invariant I-N2 about loss amplification ("fragmentation strategies
/// must document reassembly timeout and loss amplification in their card"). They are
/// exported rather than copied into four cards so the four cannot drift apart, and so a
/// third-party fragmenter can state the same terms.
pub const ASSUMPTIONS: [&str; 4] = [
    "Loss amplification assumes independence between fragments. It is violated when \
     fragments share a fading state or a burst of interference, which makes the true loss \
     LOWER than the formula predicts; AmplificationMeter::correlation_gap is how that shows \
     up in the output (04-models.md §7.4).",
    "Loss amplification does NOT assume an equal p per fragment: fragments differ in size \
     and therefore in PER, so the model takes the per-fragment p_i from the PHY and \
     computes P_sdu = 1 - prod(1 - p_i). The equal-p form 1 - (1 - p)^n is used only to \
     reproduce §7.4's worked example.",
    "Loss amplification assumes no retransmission. That is true for the group-addressed \
     frames these strategies carry: 802.11 neither acknowledges nor fragments them at the \
     MAC (04-models.md §4.6).",
    "Loss amplification assumes a reassembly timeout longer than the spread of fragment \
     arrivals. When it is not, timeouts add loss the formula does not model and the \
     realised rate rises ABOVE the prediction.",
];

/// The amplification formula as a model-card equation.
///
/// Every fragmenter card carries it (invariant I-N2), from here rather than as four copies.
pub fn equation() -> Equation {
    Equation {
        name: "loss amplification".to_string(),
        latex_or_text: "P_sdu = 1 - prod_i (1 - p_i)".to_string(),
        notes: Some(
            "04-models.md §7.4. Per-fragment p_i from the PHY, never the equal-p \
             simplification. The product runs over fragments sorted by index, so the \
             reduction is id-ordered (02-architecture.md §6.4). Independently \
             interpretable segments report the content-weighted mean sum(w_i p_i) \
             instead, because losing one segment loses only its own content."
                .to_string(),
        ),
    }
}

/// The measurement hook: predicted `P_sdu` against realised loss.
///
/// A metric provider feeds it one observation per fragmented SDU —
/// [`AmplificationMeter::observe`] — and reads back the predicted mean, the realised rate
/// and the gap between them. A realised rate **below** the prediction is the signature of
/// correlated fragment loss (assumption 1 of §7.4); a realised rate **above** it is the
/// signature of reassembly timeouts (assumption 4).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AmplificationMeter {
    samples: u64,
    lost: u64,
    /// Compensated running sum of the predicted probabilities.
    predicted_sum: f64,
    /// The compensation term of that sum.
    predicted_c: f64,
}

impl AmplificationMeter {
    /// An empty meter.
    pub const fn new() -> Self {
        Self {
            samples: 0,
            lost: 0,
            predicted_sum: 0.0,
            predicted_c: 0.0,
        }
    }

    /// Records one fragmented SDU: what the formula predicted, and what happened.
    ///
    /// `predicted` is [`SduLossModel::p_sdu_lost`]; `lost` is whether the SDU actually
    /// failed to be delivered (a missing fragment, or a reassembly that timed out).
    pub fn observe(&mut self, predicted: f64, lost: bool) {
        // Neumaier's recurrence, the same one v2xw_core::math::sum_ordered runs, applied
        // incrementally so the meter needs constant memory.
        let x = clamp_p(predicted);
        let t = self.predicted_sum + x;
        if self.predicted_sum.abs() >= x.abs() {
            self.predicted_c += (self.predicted_sum - t) + x;
        } else {
            self.predicted_c += (x - t) + self.predicted_sum;
        }
        self.predicted_sum = t;
        self.samples += 1;
        if lost {
            self.lost += 1;
        }
    }

    /// How many SDUs were observed.
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// How many of them were lost.
    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// The compensated sum of the predicted probabilities.
    pub fn total_predicted(&self) -> f64 {
        if self.predicted_sum.is_finite() {
            self.predicted_sum + self.predicted_c
        } else {
            self.predicted_sum
        }
    }

    /// The mean predicted `P_sdu`, or `0.0` with no samples.
    pub fn predicted_mean(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.total_predicted() / self.samples as f64
        }
    }

    /// The realised loss rate, or `0.0` with no samples.
    pub fn realised(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.lost as f64 / self.samples as f64
        }
    }

    /// `realised - predicted_mean`: negative when fragment losses were correlated,
    /// positive when reassembly timeouts added loss the formula does not model.
    pub fn correlation_gap(&self) -> f64 {
        self.realised() - self.predicted_mean()
    }

    /// Folds per-node meters into one, in node-id order.
    ///
    /// The float sums are reduced with [`math::sum_sorted_by_key`], so the result does not
    /// depend on the order the phase-parallel map finished the nodes in
    /// (02-architecture.md §6.4).
    pub fn merged(meters: impl IntoIterator<Item = (NodeId, AmplificationMeter)>) -> Self {
        let mut items: Vec<(NodeId, AmplificationMeter)> = meters.into_iter().collect();
        items.sort_by_key(|(n, _)| *n);
        let samples = items.iter().map(|(_, m)| m.samples).sum();
        let lost = items.iter().map(|(_, m)| m.lost).sum();
        let predicted_sum =
            math::sum_sorted_by_key(items.iter().map(|(n, m)| (*n, m.total_predicted())));
        Self {
            samples,
            lost,
            predicted_sum,
            predicted_c: 0.0,
        }
    }

    /// A quantised, serialisable view for a record or an export.
    ///
    /// Every float is on the three-decimal grid, which is build decision D9: no float
    /// reaches a recorded or exported artefact in raw IEEE-754 form.
    pub fn snapshot(&self) -> AmplificationSnapshot {
        AmplificationSnapshot {
            samples: self.samples,
            lost: self.lost,
            p_predicted: math::q3(self.predicted_mean()),
            p_realised: math::q3(self.realised()),
            correlation_gap: math::q3(self.correlation_gap()),
        }
    }
}

/// A quantised snapshot of an [`AmplificationMeter`], for a metric sample or an export.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AmplificationSnapshot {
    /// How many fragmented SDUs were observed.
    pub samples: u64,
    /// How many were lost.
    pub lost: u64,
    /// Mean predicted `P_sdu`, quantised to three decimals.
    pub p_predicted: f64,
    /// Realised loss rate, quantised to three decimals.
    pub p_realised: f64,
    /// `p_realised - p_predicted`, quantised to three decimals.
    pub correlation_gap: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formula against a hand computation with **unequal** per-fragment probabilities,
    /// which is the case 04-models.md §7.4 requires the model to use.
    #[test]
    fn the_formula_matches_a_hand_computation_for_unequal_probabilities() {
        // p = 0.10, 0.20, 0.05, 0.40.
        // survival = 0.90 * 0.80 * 0.95 * 0.60 = 0.4104
        // P_sdu    = 1 - 0.4104 = 0.5896
        let fragments = [
            FragmentLoss::new(0, 0.10, 500),
            FragmentLoss::new(1, 0.20, 500),
            FragmentLoss::new(2, 0.05, 500),
            FragmentLoss::new(3, 0.40, 500),
        ];
        let m = sdu_loss(&fragments, true);
        assert!(
            (m.p_all_arrive - 0.4104).abs() < 1e-12,
            "survival was {}",
            m.p_all_arrive
        );
        assert!(
            (m.p_sdu_lost() - 0.5896).abs() < 1e-12,
            "P_sdu was {}",
            m.p_sdu_lost()
        );
        assert_eq!(m.fragments, 4);
        assert!(m.amplifies);

        // The equal-p simplification would have given a different number for the same mean
        // p of 0.1875 — which is exactly why §7.4 forbids it.
        let mean_p = (0.10 + 0.20 + 0.05 + 0.40) / 4.0;
        assert!((equal_p_loss(4, mean_p) - m.p_sdu_lost()).abs() > 1e-3);
    }

    /// §7.4's worked example: "at `p` = 0.1 a five-fragment certificate cycle loses the
    /// certificate in 41 % of cycles".
    #[test]
    fn the_documented_worked_example_is_reproduced() {
        let p = equal_p_loss(5, 0.1);
        assert!((p - 0.40951).abs() < 1e-12, "P_sdu was {p}");
        assert_eq!(format!("{:.0}%", p * 100.0), "41%");

        // And the α = 1 case of the Partially-Hybrid design keeps n = 1, so there is no
        // amplification at all.
        assert!((equal_p_loss(1, 0.1) - 0.1).abs() < 1e-15);
    }

    #[test]
    fn independently_interpretable_segments_do_not_amplify() {
        let fragments = [
            FragmentLoss::new(0, 0.10, 800),
            FragmentLoss::new(1, 0.10, 200),
        ];
        let amplifying = sdu_loss(&fragments, true);
        let segmented = sdu_loss(&fragments, false);

        // Both see the same chance that *something* is missing…
        assert_eq!(
            amplifying.p_any_fragment_lost,
            segmented.p_any_fragment_lost
        );
        assert!((amplifying.p_any_fragment_lost - 0.19).abs() < 1e-12);
        // …but losing one segment of a segmented message loses only its own content.
        assert!((segmented.p_sdu_lost() - 0.10).abs() < 1e-12);
        assert!(segmented.p_sdu_lost() < amplifying.p_sdu_lost());
        assert!(!segmented.amplifies);

        // Content loss is weighted by payload: an unequal split moves it.
        let uneven = sdu_loss(
            &[
                FragmentLoss::new(0, 0.50, 900),
                FragmentLoss::new(1, 0.00, 100),
            ],
            false,
        );
        assert!((uneven.expected_content_lost - 0.45).abs() < 1e-12);
    }

    #[test]
    fn the_reduction_does_not_depend_on_the_input_order() {
        let a = [
            FragmentLoss::new(0, 0.10, 400),
            FragmentLoss::new(1, 0.20, 300),
            FragmentLoss::new(2, 0.05, 200),
        ];
        let mut b = a;
        b.reverse();
        let x = sdu_loss(&a, true);
        let y = sdu_loss(&b, true);
        assert_eq!(
            x.p_all_arrive.to_bits(),
            y.p_all_arrive.to_bits(),
            "the product is ordered by fragment index, bit for bit"
        );
        assert_eq!(
            x.expected_content_lost.to_bits(),
            y.expected_content_lost.to_bits()
        );
    }

    #[test]
    fn nothing_sent_cannot_be_lost() {
        let m = sdu_loss(&[], true);
        assert_eq!(m.fragments, 0);
        assert_eq!(m.p_all_arrive, 1.0);
        assert_eq!(m.p_any_fragment_lost, 0.0);
        assert_eq!(m.expected_content_lost, 0.0);
        assert_eq!(m.amplification_factor(0.0), 1.0);
    }

    #[test]
    fn a_certain_loss_saturates_rather_than_exceeding_one() {
        let m = sdu_loss(
            &[
                FragmentLoss::new(0, 1.0, 100),
                FragmentLoss::new(1, 0.5, 100),
            ],
            true,
        );
        assert_eq!(m.p_all_arrive, 0.0);
        assert_eq!(m.p_any_fragment_lost, 1.0);
    }

    #[test]
    fn the_amplification_factor_reads_as_the_documented_cost() {
        let m = sdu_loss(
            &(0..5)
                .map(|i| FragmentLoss::new(i, 0.1, 200))
                .collect::<Vec<_>>(),
            true,
        );
        let factor = m.amplification_factor(0.1);
        assert!((factor - 4.0951).abs() < 1e-12, "factor was {factor}");
    }

    /// The meter's incremental sum must be the same number `sum_ordered` gives for the same
    /// sequence — the property that lets it run in constant memory without drifting.
    #[test]
    fn the_meters_running_sum_equals_sum_ordered() {
        let predictions: Vec<f64> = (0..1_000).map(|i| 0.001 + (i % 97) as f64 * 1e-5).collect();
        let mut meter = AmplificationMeter::new();
        for (i, p) in predictions.iter().enumerate() {
            meter.observe(*p, i % 7 == 0);
        }
        assert_eq!(
            meter.total_predicted().to_bits(),
            math::sum_ordered(predictions.iter().copied()).to_bits()
        );
        assert_eq!(meter.samples(), 1_000);
        assert_eq!(meter.lost(), 143);
    }

    #[test]
    fn the_meter_exposes_the_gap_that_correlation_produces() {
        // The formula says 41 % for every SDU; the realised loss is 20 %, because the
        // fragments shared a fading state.
        let mut meter = AmplificationMeter::new();
        for i in 0..100 {
            meter.observe(0.40951, i % 5 == 0);
        }
        assert!((meter.predicted_mean() - 0.40951).abs() < 1e-12);
        assert!((meter.realised() - 0.20).abs() < 1e-12);
        assert!(
            meter.correlation_gap() < -0.2,
            "a realised loss below the prediction is correlated loss"
        );

        let snap = meter.snapshot();
        assert_eq!(snap.p_predicted, 0.41, "quantised to three decimals");
        assert_eq!(snap.p_realised, 0.2);
        assert_eq!(snap.correlation_gap, -0.21);
        assert!(math::is_quantized(snap.p_predicted, 3));
        assert!(math::is_quantized(snap.correlation_gap, 3));

        let empty = AmplificationMeter::new();
        assert_eq!(empty.predicted_mean(), 0.0);
        assert_eq!(empty.realised(), 0.0);
        assert_eq!(empty.correlation_gap(), 0.0);
    }

    #[test]
    fn merging_meters_is_id_ordered_and_order_independent() {
        let build = |n: u64, lost: u64, p: f64| {
            let mut m = AmplificationMeter::new();
            for i in 0..n {
                m.observe(p, i < lost);
            }
            m
        };
        let rows = [
            (NodeId::new(7), build(10, 2, 0.31)),
            (NodeId::new(2), build(20, 9, 0.17)),
            (NodeId::new(5), build(5, 0, 0.42)),
        ];
        let forward = AmplificationMeter::merged(rows);
        let mut reversed = rows;
        reversed.reverse();
        let backward = AmplificationMeter::merged(reversed);
        assert_eq!(
            forward.total_predicted().to_bits(),
            backward.total_predicted().to_bits()
        );
        assert_eq!(forward.samples(), 35);
        assert_eq!(forward.lost(), 11);
    }
}
