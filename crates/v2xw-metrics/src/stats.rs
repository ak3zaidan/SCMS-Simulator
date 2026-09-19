//! Statistics done honestly: sample counts, Wilson intervals, insufficiency, one stated
//! percentile rule.
//!
//! 08-measurement-and-data.md §1 asks that a metric be defined by its formula and its
//! source, and §4 asks for confidence intervals across seeds. The rules this module
//! enforces go one step further, because the failure mode they close is the one that makes
//! a measurement layer lie:
//!
//! 1. **Every aggregate carries its sample count.** A PDR of 0.0 over one candidate
//!    reception and a PDR of 0.0 over 40,000 are not the same finding, and a table that
//!    prints `0.0` for both is not a measurement.
//! 2. **A proportion over few trials carries a confidence interval**, computed with the
//!    **Wilson score interval** ([`wilson_interval`]) rather than the textbook normal
//!    approximation, which has essentially no coverage near 0 and 1 — exactly where a PDR
//!    at the edge of range or a false-positive rate lives.
//! 3. **A bin with too few samples reports [`RatioEstimate::Insufficient`]**, not a point
//!    estimate. `1/1 = 1.0` is not a delivery ratio of one.
//! 4. **Percentiles state their interpolation method.** The three common conventions differ
//!    by a whole sample on the windows a metric actually aggregates, so the rule is named in
//!    the output ([`Interpolation`]) and is the same one everywhere.
//! 5. **Nothing divides by zero silently, and no `NaN` is dressed as an estimate.** Every
//!    ratio in this crate is built by [`Proportion::estimate`] or [`ratio_of_sums`], and
//!    both answer [`RatioEstimate::Insufficient`] for a zero or non-finite denominator
//!    instead of `NaN` — and [`ratio_of_sums`] answers it for a non-finite **numerator**
//!    too, because a sum that came out `NaN` is a failed reduction and not a point estimate.
//!
//! # Determinism
//!
//! A mean is a reduction, and floating-point addition is not associative, so a mean over
//! samples in arrival order is not a function of the samples alone. Every reduction here
//! therefore **sorts before it sums**: [`Distribution::summary`] puts the sample into
//! `v2xw_core::math::sort_total_order` and reduces the sorted slice with
//! `v2xw_core::math::sum_ordered`, which makes every figure it reports a pure function of
//! the multiset of samples. Reductions whose contributors are entities rather than bare
//! numbers sort by id instead; see [`crate::comms`].

use serde::{Deserialize, Serialize};
use v2xw_core::math::{quantile_sorted, sort_total_order, sum_ordered};

use crate::quant::Quantum;

/// The default number of trials a proportion needs before it is reported as a point
/// estimate rather than as [`RatioEstimate::Insufficient`].
///
/// Thirty is the conventional threshold for treating a binomial sample as informative at
/// all, and it is a *declared* default rather than a discovered one: every metric
/// definition in this crate carries its own `min_samples`, and a caller with a reason to
/// choose differently sets it there. The number is not a claim about coverage — the Wilson
/// interval carries that — it is the line below which a point estimate misleads more than
/// it informs.
pub const DEFAULT_MIN_SAMPLES: u64 = 30;

/// The confidence level of an interval, as one of three tabulated standard-normal
/// quantiles.
///
/// # Why an enum and not a level in `(0, 1)`
///
/// Turning an arbitrary level into a `z` needs the inverse of the standard normal CDF,
/// which is a transcendental. Build decision D10 forbids a raw transcendental from driving
/// a comparison whose outcome is compared across engines, and an interval bound is compared
/// across engines. Three tabulated constants remove the question: they are exact `f64`
/// literals, identical on every platform and in every language that reads the output.
///
/// The values are the two-sided standard normal quantiles `z_{1 − α/2}`:
/// `z_{0.95} = 1.6448536269514722`, `z_{0.975} = 1.959963984540054`,
/// `z_{0.995} = 2.5758293035489004`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConfidenceLevel {
    /// 90 %, `z = 1.6448536269514722`.
    P90,
    /// 95 %, `z = 1.959963984540054`. The level 08-measurement-and-data.md §4 declares for
    /// the experiment system (`replications_policy: {ci: 0.95}`), and the default here.
    #[default]
    P95,
    /// 99 %, `z = 2.5758293035489004`.
    P99,
}

impl ConfidenceLevel {
    /// The standard-normal quantile `z` for this level.
    #[must_use]
    pub const fn z(self) -> f64 {
        match self {
            ConfidenceLevel::P90 => 1.644_853_626_951_472_2,
            ConfidenceLevel::P95 => 1.959_963_984_540_054,
            ConfidenceLevel::P99 => 2.575_829_303_548_900_4,
        }
    }

    /// The nominal coverage as a fraction, for the label on a figure or a card.
    #[must_use]
    pub const fn coverage(self) -> f64 {
        match self {
            ConfidenceLevel::P90 => 0.90,
            ConfidenceLevel::P95 => 0.95,
            ConfidenceLevel::P99 => 0.99,
        }
    }
}

impl core::fmt::Display for ConfidenceLevel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            ConfidenceLevel::P90 => "90%",
            ConfidenceLevel::P95 => "95%",
            ConfidenceLevel::P99 => "99%",
        })
    }
}

/// The **Wilson score interval** for a binomial proportion, clamped to `[0, 1]`.
///
/// # The formula, exactly
///
/// With `p̂ = k / n` and `z` the standard-normal quantile of the level:
///
/// ```text
/// centre     = (p̂ + z²/(2n)) / (1 + z²/n)
/// half-width = z / (1 + z²/n) · sqrt( p̂(1 − p̂)/n + z²/(4n²) )
/// interval   = centre ∓ half-width
/// ```
///
/// Wilson (1927), "Probable inference, the law of succession, and statistical inference",
/// *JASA* 22(158):209–212; recommended over the normal approximation by Brown, Cai and
/// DasGupta (2001), "Interval estimation for a binomial proportion", *Statistical Science*
/// 16(2):101–133.
///
/// # Why this interval and not the normal approximation
///
/// The normal ("Wald") interval `p̂ ± z·sqrt(p̂(1−p̂)/n)` has zero width at `k = 0` and at
/// `k = n`, so it claims certainty exactly where a PDR at the edge of communication range,
/// a false-positive rate on a benign fleet or a reassembly-failure share actually sits.
/// Wilson's interval is the set of `p` the score test does not reject; it is never empty,
/// never leaves `[0, 1]` before clamping by more than rounding, and its coverage is close to
/// nominal for small `n` and extreme `p̂`.
///
/// # Every operation is exactly rounded
///
/// Only `+ - * /` and `sqrt` appear, all of which IEEE-754 requires to be correctly rounded,
/// so the bounds are bit-identical on every target. `sqrt` is the one transcendental-looking
/// operation the determinism contract exempts, for that reason (`v2xw_core::math::sqrt`).
///
/// # The clamp, stated
///
/// The formula cannot leave `[0, 1]` mathematically; floating-point rounding can put a bound
/// a few ULP outside it. The result is clamped, so a recorded bound is always a valid
/// probability. No other adjustment is made.
///
/// Returns `(lo, hi)`. `n = 0` has no proportion to bound and returns `(0.0, 1.0)` — the
/// whole range, which is the honest interval for no data; callers should not get here,
/// because [`Proportion::estimate`] reports [`RatioEstimate::Insufficient`] first.
///
/// ```
/// use v2xw_metrics::stats::{wilson_interval, ConfidenceLevel};
/// // The published example: 25 successes in 100 trials at 95 % is (0.1754, 0.3430).
/// let (lo, hi) = wilson_interval(25, 100, ConfidenceLevel::P95);
/// assert!((lo - 0.175_45).abs() < 1e-5, "{lo}");
/// assert!((hi - 0.343_04).abs() < 1e-5, "{hi}");
/// // Zero successes in ten trials is (0, 0.2775): an upper bound, not certainty.
/// let (lo, hi) = wilson_interval(0, 10, ConfidenceLevel::P95);
/// assert_eq!(lo, 0.0);
/// assert!((hi - 0.277_53).abs() < 1e-5, "{hi}");
/// ```
///
/// # Panics
/// If `successes > trials`, which is not a proportion.
#[must_use]
pub fn wilson_interval(successes: u64, trials: u64, level: ConfidenceLevel) -> (f64, f64) {
    assert!(
        successes <= trials,
        "wilson_interval: {successes} successes out of {trials} trials is not a proportion"
    );
    if trials == 0 {
        return (0.0, 1.0);
    }
    let n = trials as f64;
    let p = (successes as f64) / n;
    let z = level.z();
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let inner = p * (1.0 - p) / n + z2 / (4.0 * n * n);
    let half = (z / denom) * v2xw_core::math::sqrt(inner);
    (
        (centre - half).clamp(0.0, 1.0),
        (centre + half).clamp(0.0, 1.0),
    )
}

/// The percentile interpolation rule a [`DistributionSummary`] used, written into the
/// output so a reader never has to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Interpolation {
    /// Linear interpolation between the two nearest order statistics at position
    /// `h = (n − 1)·q` — Hyndman & Fan (1996) **type 7**, the default of NumPy's
    /// `percentile` and R's `quantile`.
    ///
    /// Implemented by `v2xw_core::math::quantile_sorted`, which is built only from `+ - *`
    /// and `floor` and is therefore bit-identical on every target.
    Type7Linear,
}

impl core::fmt::Display for Interpolation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Interpolation::Type7Linear => "hyndman-fan-type-7-linear",
        })
    }
}

/// A scalar aggregate: either a point estimate with its sample count, or a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Estimate {
    /// Fewer than `required` samples, so no point estimate is reported.
    ///
    /// The variant exists so that an empty or thin bin is a *distinguishable* answer rather
    /// than a `NaN`, a zero or a missing row. A reader can tell "we did not measure this"
    /// from "we measured this and it is zero", which is the whole point.
    Insufficient {
        /// How many samples there were.
        n: u64,
        /// How many the definition asked for.
        required: u64,
    },
    /// A point estimate over `n` samples.
    Value {
        /// The estimate.
        point: f64,
        /// The number of samples it was computed from.
        n: u64,
    },
}

impl Estimate {
    /// The point estimate, or `None` when the sample was insufficient.
    #[must_use]
    pub const fn point(self) -> Option<f64> {
        match self {
            Estimate::Value { point, .. } => Some(point),
            Estimate::Insufficient { .. } => None,
        }
    }

    /// The number of samples behind this answer, sufficient or not.
    #[must_use]
    pub const fn n(self) -> u64 {
        match self {
            Estimate::Value { n, .. } | Estimate::Insufficient { n, .. } => n,
        }
    }

    /// True if no point estimate is reported.
    #[must_use]
    pub const fn is_insufficient(self) -> bool {
        matches!(self, Estimate::Insufficient { .. })
    }

    /// This estimate with its float rounded onto `q` — the writer-side quantisation of D9.
    #[must_use]
    pub fn quantised(self, q: Quantum) -> Self {
        match self {
            Estimate::Value { point, n } => Estimate::Value {
                point: q.quantise(point),
                n,
            },
            other => other,
        }
    }
}

/// A ratio aggregate, in the three honest shapes a ratio actually comes in.
///
/// The distinction between the second and the third variant is the one this crate refuses
/// to blur: a **proportion** counts successes among trials and has a binomial sampling
/// distribution, so Wilson's interval applies to it. A **ratio of sums** — envelope bytes
/// over payload bytes, busy time over window length — is not a count of Bernoulli trials,
/// and putting a Wilson interval on it would be a fabricated error bar. It therefore
/// reports its two sums and its sample count and no interval, and says so.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum RatioEstimate {
    /// Too few trials (or none at all, which is where a zero denominator lands).
    Insufficient {
        /// The trials, or the denominator's sample count.
        trials: u64,
        /// How many the definition asked for; `1` means "any at all".
        required: u64,
    },
    /// A proportion of `successes` among `trials`, with its Wilson score interval.
    Proportion {
        /// `successes / trials`.
        point: f64,
        /// The lower Wilson bound.
        ci_lo: f64,
        /// The upper Wilson bound.
        ci_hi: f64,
        /// The interval's nominal level.
        level: ConfidenceLevel,
        /// The number of trials.
        trials: u64,
        /// The number of successes.
        successes: u64,
    },
    /// A ratio of two sums, with **no** confidence interval, because it is not a proportion
    /// of Bernoulli trials. The two sums are reported so a reader can form their own.
    RatioOfSums {
        /// `numerator / denominator`.
        point: f64,
        /// The numerator's sum.
        numerator: f64,
        /// The denominator's sum.
        denominator: f64,
        /// How many observations contributed to the sums.
        n: u64,
    },
}

impl RatioEstimate {
    /// The point estimate, or `None` when the sample was insufficient.
    #[must_use]
    pub const fn point(self) -> Option<f64> {
        match self {
            RatioEstimate::Proportion { point, .. } | RatioEstimate::RatioOfSums { point, .. } => {
                Some(point)
            }
            RatioEstimate::Insufficient { .. } => None,
        }
    }

    /// The Wilson bounds, when this is a proportion that was estimated.
    #[must_use]
    pub const fn interval(self) -> Option<(f64, f64)> {
        match self {
            RatioEstimate::Proportion { ci_lo, ci_hi, .. } => Some((ci_lo, ci_hi)),
            _ => None,
        }
    }

    /// The sample count behind this answer: trials for a proportion, observations for a
    /// ratio of sums.
    #[must_use]
    pub const fn n(self) -> u64 {
        match self {
            RatioEstimate::Insufficient { trials, .. } => trials,
            RatioEstimate::Proportion { trials, .. } => trials,
            RatioEstimate::RatioOfSums { n, .. } => n,
        }
    }

    /// True if no point estimate is reported.
    #[must_use]
    pub const fn is_insufficient(self) -> bool {
        matches!(self, RatioEstimate::Insufficient { .. })
    }

    /// This estimate with every float rounded onto its grid (D9).
    ///
    /// The point estimate goes onto `q`; the interval bounds go onto
    /// [`Quantum::PROBABILITY`], because a bound is a statement about a probability and D9
    /// gives probabilities the finer grid. A ratio of sums rounds its two sums onto `sums`.
    #[must_use]
    pub fn quantised(self, q: Quantum, sums: Quantum) -> Self {
        match self {
            RatioEstimate::Proportion {
                point,
                ci_lo,
                ci_hi,
                level,
                trials,
                successes,
            } => RatioEstimate::Proportion {
                point: q.quantise(point),
                ci_lo: Quantum::PROBABILITY.quantise(ci_lo),
                ci_hi: Quantum::PROBABILITY.quantise(ci_hi),
                level,
                trials,
                successes,
            },
            RatioEstimate::RatioOfSums {
                point,
                numerator,
                denominator,
                n,
            } => RatioEstimate::RatioOfSums {
                point: q.quantise(point),
                numerator: sums.quantise(numerator),
                denominator: sums.quantise(denominator),
                n,
            },
            other => other,
        }
    }
}

/// A count of successes among trials — the accumulator behind every proportion metric.
///
/// It holds two integers, so it is order-independent by construction: there is no
/// floating-point reduction to get wrong, and two runs that observed the same trials in
/// different orders hold the same pair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proportion {
    successes: u64,
    trials: u64,
}

impl Proportion {
    /// An empty proportion.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            successes: 0,
            trials: 0,
        }
    }

    /// A proportion with a known count, for a fixture or a merge.
    ///
    /// # Panics
    /// If `successes > trials`.
    #[must_use]
    pub const fn from_counts(successes: u64, trials: u64) -> Self {
        assert!(
            successes <= trials,
            "Proportion::from_counts: successes exceed trials"
        );
        Self { successes, trials }
    }

    /// Records one trial and whether it succeeded.
    pub const fn observe(&mut self, success: bool) {
        self.trials += 1;
        if success {
            self.successes += 1;
        }
    }

    /// Records `trials` trials of which `successes` succeeded.
    ///
    /// # Panics
    /// If `successes > trials`.
    pub const fn observe_many(&mut self, successes: u64, trials: u64) {
        assert!(
            successes <= trials,
            "Proportion::observe_many: successes exceed trials"
        );
        self.trials += trials;
        self.successes += successes;
    }

    /// Adds another proportion's counts to this one. Commutative and associative, because
    /// it is integer addition.
    pub const fn merge(&mut self, other: Proportion) {
        self.trials += other.trials;
        self.successes += other.successes;
    }

    /// The number of trials.
    #[must_use]
    pub const fn trials(self) -> u64 {
        self.trials
    }

    /// The number of successes.
    #[must_use]
    pub const fn successes(self) -> u64 {
        self.successes
    }

    /// The number of failures.
    #[must_use]
    pub const fn failures(self) -> u64 {
        self.trials - self.successes
    }

    /// The estimate: a point with its Wilson interval, or a refusal below `min_trials`.
    ///
    /// A zero denominator lands in [`RatioEstimate::Insufficient`] rather than in a
    /// division, which is rule 5 of this module.
    #[must_use]
    pub fn estimate(self, min_trials: u64, level: ConfidenceLevel) -> RatioEstimate {
        let required = min_trials.max(1);
        if self.trials < required {
            return RatioEstimate::Insufficient {
                trials: self.trials,
                required,
            };
        }
        let (ci_lo, ci_hi) = wilson_interval(self.successes, self.trials, level);
        RatioEstimate::Proportion {
            point: (self.successes as f64) / (self.trials as f64),
            ci_lo,
            ci_hi,
            level,
            trials: self.trials,
            successes: self.successes,
        }
    }
}

/// A ratio of two sums — bytes over bytes, busy time over window length.
///
/// Reports [`RatioEstimate::RatioOfSums`], which carries no confidence interval on purpose:
/// see [`RatioEstimate`].
///
/// # Both sums are guarded, not only the denominator
///
/// A zero or non-finite denominator reports [`RatioEstimate::Insufficient`] rather than
/// dividing — and so does a **non-finite numerator**, which is the same defect seen from the
/// other side. `NaN / 2.0` is `NaN`, and a `NaN` that reaches a sample is not a point
/// estimate: it says the reduction that produced the sum failed, which is an invalid or
/// insufficient result and is reported as one. Letting it through would put a `NaN` point
/// value into a sample, where the quantiser passes it untouched (`quantize_to` returns a
/// non-finite input unchanged, deliberately) and `is_on_grid` answers `true` for it, so it
/// would reach an exported table looking like a measurement. `Distribution::observe` already
/// refuses a non-finite sample for the same reason; this closes the matching hole here.
///
/// The two sums must already have been reduced deterministically by the caller — with
/// `sum_ordered` over id-sorted contributors — because this function only divides.
#[must_use]
pub fn ratio_of_sums(numerator: f64, denominator: f64, n: u64, min_n: u64) -> RatioEstimate {
    let required = min_n.max(1);
    if n < required || !numerator.is_finite() || !denominator.is_finite() || denominator == 0.0 {
        return RatioEstimate::Insufficient {
            trials: n,
            required,
        };
    }
    RatioEstimate::RatioOfSums {
        point: numerator / denominator,
        numerator,
        denominator,
        n,
    }
}

/// The reported shape of a distribution: count, extremes, mean and three percentiles, with
/// the interpolation rule named.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DistributionSummary {
    /// Fewer than `required` samples, so nothing is reported. An empty sample lands here
    /// with `n = 0`, which is why no percentile in this crate can be a `NaN`.
    Insufficient {
        /// How many samples there were.
        n: u64,
        /// How many the definition asked for.
        required: u64,
    },
    /// The summary.
    Summary {
        /// The number of finite samples.
        n: u64,
        /// The smallest sample.
        min: f64,
        /// The largest sample.
        max: f64,
        /// The arithmetic mean, reduced over the **sorted** sample so that it is a function
        /// of the multiset and not of the arrival order.
        mean: f64,
        /// The median.
        p50: f64,
        /// The 95th percentile.
        p95: f64,
        /// The 99th percentile.
        p99: f64,
        /// The rule the three percentiles were interpolated with.
        interpolation: Interpolation,
        /// How many non-finite samples were refused by [`Distribution::observe`] and are
        /// therefore **not** in `n`. Reported rather than hidden: a stream that produced
        /// them has a defect upstream, and a silent filter would conceal it.
        rejected: u64,
    },
}

impl DistributionSummary {
    /// True if nothing is reported.
    #[must_use]
    pub const fn is_insufficient(self) -> bool {
        matches!(self, DistributionSummary::Insufficient { .. })
    }

    /// The sample count behind this answer.
    #[must_use]
    pub const fn n(self) -> u64 {
        match self {
            DistributionSummary::Insufficient { n, .. }
            | DistributionSummary::Summary { n, .. } => n,
        }
    }

    /// A named percentile, or `None` when the sample was insufficient.
    #[must_use]
    pub const fn quantile(self, which: Percentile) -> Option<f64> {
        match self {
            DistributionSummary::Summary { p50, p95, p99, .. } => Some(match which {
                Percentile::P50 => p50,
                Percentile::P95 => p95,
                Percentile::P99 => p99,
            }),
            DistributionSummary::Insufficient { .. } => None,
        }
    }

    /// The mean, or `None` when the sample was insufficient.
    #[must_use]
    pub const fn mean(self) -> Option<f64> {
        match self {
            DistributionSummary::Summary { mean, .. } => Some(mean),
            DistributionSummary::Insufficient { .. } => None,
        }
    }

    /// This summary with every float rounded onto `q` (D9).
    #[must_use]
    pub fn quantised(self, q: Quantum) -> Self {
        match self {
            DistributionSummary::Summary {
                n,
                min,
                max,
                mean,
                p50,
                p95,
                p99,
                interpolation,
                rejected,
            } => DistributionSummary::Summary {
                n,
                min: q.quantise(min),
                max: q.quantise(max),
                mean: q.quantise(mean),
                p50: q.quantise(p50),
                p95: q.quantise(p95),
                p99: q.quantise(p99),
                interpolation,
                rejected,
            },
            other => other,
        }
    }
}

/// Which of the three reported percentiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Percentile {
    /// The median.
    P50,
    /// The 95th percentile — what 08-measurement-and-data.md §2 asks for on `pir`,
    /// `verify_queue_depth` and the latency metrics.
    P95,
    /// The 99th percentile.
    P99,
}

impl Percentile {
    /// The fraction this percentile is at.
    #[must_use]
    pub const fn q(self) -> f64 {
        match self {
            Percentile::P50 => 0.5,
            Percentile::P95 => 0.95,
            Percentile::P99 => 0.99,
        }
    }
}

/// An accumulator for a sample of real numbers: keeps the values, refuses the non-finite
/// ones, and reports a [`DistributionSummary`].
///
/// It keeps the whole sample rather than streaming moments, because a p95 and a p99 cannot
/// be computed from moments and because an exact, reproducible percentile is worth more here
/// than the memory a sketch would save. A window's worth of one node's receptions is
/// thousands of values, not millions.
///
/// # Non-finite samples are refused, not filtered silently
///
/// `v2xw_core::math::quantile_sorted` documents that `NaN`s sort to the top of the total
/// order and contaminate a high percentile, so "a provider filters them out before sampling
/// rather than after". [`Distribution::observe`] does exactly that, and counts what it
/// refused into [`DistributionSummary::Summary::rejected`] so the defect upstream stays
/// visible.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Distribution {
    values: Vec<f64>,
    rejected: u64,
}

impl Distribution {
    /// An empty sample.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            values: Vec::new(),
            rejected: 0,
        }
    }

    /// An empty sample with room for `capacity` values.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            values: Vec::with_capacity(capacity),
            rejected: 0,
        }
    }

    /// Records one value. Returns `false` and counts it as rejected if it is not finite.
    pub fn observe(&mut self, x: f64) -> bool {
        if x.is_finite() {
            self.values.push(x);
            true
        } else {
            self.rejected += 1;
            false
        }
    }

    /// Records every value of an iterator.
    pub fn observe_all(&mut self, xs: impl IntoIterator<Item = f64>) {
        for x in xs {
            self.observe(x);
        }
    }

    /// Merges another sample into this one. The result's summary is unchanged by the order
    /// of merges, because the summary sorts before it reduces.
    pub fn merge(&mut self, other: &Distribution) {
        self.values.extend_from_slice(&other.values);
        self.rejected += other.rejected;
    }

    /// The number of finite samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// True if no finite sample was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// How many non-finite values were refused.
    #[must_use]
    pub const fn rejected(&self) -> u64 {
        self.rejected
    }

    /// The sample sorted into IEEE-754 total order — the deterministic form everything
    /// else is computed from.
    #[must_use]
    pub fn sorted(&self) -> Vec<f64> {
        let mut v = self.values.clone();
        sort_total_order(&mut v);
        v
    }

    /// The summary, or [`DistributionSummary::Insufficient`] below `min_samples`.
    ///
    /// The whole computation is a function of the multiset of samples: the sample is sorted
    /// into total order first, the percentiles come from `quantile_sorted` on that slice,
    /// and the mean is `sum_ordered` over the same slice. Two runs that observed the same
    /// values in different orders, on different thread counts, produce bit-identical
    /// figures.
    #[must_use]
    pub fn summary(&self, min_samples: u64) -> DistributionSummary {
        let required = min_samples.max(1);
        let n = self.values.len() as u64;
        if n < required {
            return DistributionSummary::Insufficient { n, required };
        }
        let sorted = self.sorted();
        let sum = sum_ordered(sorted.iter().copied());
        DistributionSummary::Summary {
            n,
            min: sorted[0],
            max: sorted[sorted.len() - 1],
            mean: sum / (n as f64),
            p50: quantile_sorted(&sorted, Percentile::P50.q()),
            p95: quantile_sorted(&sorted, Percentile::P95.q()),
            p99: quantile_sorted(&sorted, Percentile::P99.q()),
            interpolation: Interpolation::Type7Linear,
            rejected: self.rejected,
        }
    }

    /// The mean alone, as an [`Estimate`].
    #[must_use]
    pub fn mean(&self, min_samples: u64) -> Estimate {
        let required = min_samples.max(1);
        let n = self.values.len() as u64;
        if n < required {
            return Estimate::Insufficient { n, required };
        }
        let sorted = self.sorted();
        Estimate::Value {
            point: sum_ordered(sorted.iter().copied()) / (n as f64),
            n,
        }
    }

    /// The sum alone, as an [`Estimate`], reduced over the sorted sample.
    #[must_use]
    pub fn sum(&self, min_samples: u64) -> Estimate {
        let required = min_samples.max(1);
        let n = self.values.len() as u64;
        if n < required {
            return Estimate::Insufficient { n, required };
        }
        Estimate::Value {
            point: sum_ordered(self.sorted()),
            n,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Published Wilson values. Sources: the worked example in Brown, Cai and DasGupta
    /// (2001) and the standard textbook cases at the boundaries, all recomputed here to
    /// more digits than a table prints.
    #[test]
    fn the_wilson_interval_matches_published_values() {
        // 25/100 at 95 %: published as (0.1754, 0.3430); to full precision
        // (0.17545211362287674, 0.34304463548061603).
        let (lo, hi) = wilson_interval(25, 100, ConfidenceLevel::P95);
        assert!((lo - 0.175_452_113_622_876_74).abs() < 1e-15, "lo = {lo}");
        assert!((hi - 0.343_044_635_480_616_03).abs() < 1e-15, "hi = {hi}");

        // 0/10 at 95 %: published as (0, 0.2775) — the zero-success case the Wald interval
        // gets wrong by claiming (0, 0).
        let (lo, hi) = wilson_interval(0, 10, ConfidenceLevel::P95);
        assert_eq!(lo, 0.0);
        assert!((hi - 0.277_532_799_862_889_2).abs() < 1e-15, "hi = {hi}");

        // 10/10 at 95 %: published as (0.7225, 1), the mirror image by symmetry.
        //
        // The upper bound is exactly 1 in exact arithmetic (at p̂ = 1 the centre and the
        // half-width sum to `(1 + z²/n)/(1 + z²/n)`), and one ULP below it in floating
        // point. The writer's quantisation onto the 1e-6 probability grid resolves that to
        // 1.0; the raw value is left as the arithmetic produced it rather than nudged.
        let (lo, hi) = wilson_interval(10, 10, ConfidenceLevel::P95);
        assert!((lo - 0.722_467_200_137_110_7).abs() < 1e-15, "lo = {lo}");
        assert!((1.0 - hi) < 1e-15, "hi = {hi}");
        assert_eq!(Quantum::PROBABILITY.quantise(hi), 1.0);

        // 3/10 at 95 %: (0.10779126740630099, 0.6032218525388546).
        let (lo, hi) = wilson_interval(3, 10, ConfidenceLevel::P95);
        assert!((lo - 0.107_791_267_406_300_99).abs() < 1e-15, "lo = {lo}");
        assert!((hi - 0.603_221_852_538_854_6).abs() < 1e-15, "hi = {hi}");

        // 1/2 at 95 %: symmetric about 0.5.
        let (lo, hi) = wilson_interval(1, 2, ConfidenceLevel::P95);
        assert!(((lo + hi) / 2.0 - 0.5).abs() < 1e-12, "{lo}..{hi}");

        // 0/10 at 99 % is wider than at 95 %, and at 90 % narrower.
        let (_, hi90) = wilson_interval(0, 10, ConfidenceLevel::P90);
        let (_, hi95) = wilson_interval(0, 10, ConfidenceLevel::P95);
        let (_, hi99) = wilson_interval(0, 10, ConfidenceLevel::P99);
        assert!(hi90 < hi95 && hi95 < hi99, "{hi90} {hi95} {hi99}");
    }

    /// The symmetry property: the interval for `k/n` is the mirror of the one for
    /// `(n − k)/n`. It holds for Wilson and fails for several ad-hoc alternatives, so it is
    /// a real check on the implementation.
    #[test]
    fn the_wilson_interval_is_symmetric_under_complementation() {
        for (k, n) in [(0_u64, 7_u64), (1, 7), (3, 7), (2, 40), (17, 40)] {
            let (lo, hi) = wilson_interval(k, n, ConfidenceLevel::P95);
            let (clo, chi) = wilson_interval(n - k, n, ConfidenceLevel::P95);
            assert!((lo - (1.0 - chi)).abs() < 1e-12, "{k}/{n}");
            assert!((hi - (1.0 - clo)).abs() < 1e-12, "{k}/{n}");
        }
    }

    /// The interval always contains the point estimate and always lies inside `[0, 1]`.
    #[test]
    fn the_wilson_interval_brackets_the_estimate_and_stays_a_probability() {
        for n in [1_u64, 2, 5, 30, 1000] {
            for k in 0..=n {
                let (lo, hi) = wilson_interval(k, n, ConfidenceLevel::P95);
                let p = (k as f64) / (n as f64);
                assert!((0.0..=1.0).contains(&lo), "{k}/{n} lo = {lo}");
                assert!((0.0..=1.0).contains(&hi), "{k}/{n} hi = {hi}");
                assert!(lo <= p + 1e-12 && p <= hi + 1e-12, "{k}/{n}: {lo} {p} {hi}");
            }
        }
    }

    #[test]
    fn a_thin_proportion_reports_insufficient_rather_than_a_point() {
        let mut p = Proportion::new();
        p.observe(true);
        assert_eq!(
            p.estimate(30, ConfidenceLevel::P95),
            RatioEstimate::Insufficient {
                trials: 1,
                required: 30
            }
        );
        // …and with the threshold lowered, the same counts do give a point.
        assert!(p.estimate(1, ConfidenceLevel::P95).point().is_some());
    }

    #[test]
    fn an_empty_proportion_is_insufficient_and_never_nan() {
        let e = Proportion::new().estimate(1, ConfidenceLevel::P95);
        assert!(e.is_insufficient());
        assert_eq!(e.point(), None);
        assert_eq!(e.n(), 0);
    }

    #[test]
    fn a_zero_denominator_is_insufficient_rather_than_a_division() {
        let e = ratio_of_sums(5.0, 0.0, 10, 1);
        assert!(e.is_insufficient());
        assert_eq!(e.point(), None);
    }

    /// F9: the numerator was not guarded, so `ratio_of_sums(NaN, 2.0, 10, 1)` returned a
    /// `RatioOfSums { point: NaN, numerator: NaN, .. }`. A `NaN` point then survives every
    /// downstream test by construction — `quantize_to` passes a non-finite value through
    /// deliberately, and `is_on_grid` answers `true` for it — so it would reach an exported
    /// table looking like a measurement. Both sums are guarded now, and a failed reduction
    /// is reported as what it is.
    #[test]
    fn a_non_finite_numerator_is_insufficient_rather_than_a_nan_point() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let e = ratio_of_sums(bad, 2.0, 10, 1);
            assert_eq!(
                e,
                RatioEstimate::Insufficient {
                    trials: 10,
                    required: 1
                },
                "{bad} as a numerator"
            );
            assert_eq!(e.point(), None);
            assert!(e.is_insufficient());
            // The reason it has to be caught here: neither test below can see it.
            assert_eq!(Quantum::RATIO.quantise(bad).is_nan(), bad.is_nan());
            assert!(Quantum::RATIO.holds(bad));
        }
        // A finite numerator over a finite non-zero denominator is unaffected.
        let e = ratio_of_sums(1.0, 2.0, 10, 1);
        assert_eq!(e.point(), Some(0.5));
    }

    #[test]
    fn an_empty_distribution_is_insufficient_and_never_nan() {
        let d = Distribution::new();
        let s = d.summary(1);
        assert_eq!(s, DistributionSummary::Insufficient { n: 0, required: 1 });
        assert_eq!(s.mean(), None);
        assert_eq!(s.quantile(Percentile::P95), None);
        assert!(d.mean(1).is_insufficient());
    }

    /// The hand-computed case: the eight-value sample whose type-7 median is 3.5.
    #[test]
    fn the_summary_reproduces_a_hand_computed_sample() {
        let mut d = Distribution::new();
        d.observe_all([3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0]);
        let DistributionSummary::Summary {
            n,
            min,
            max,
            mean,
            p50,
            p95,
            interpolation,
            rejected,
            ..
        } = d.summary(1)
        else {
            panic!("expected a summary");
        };
        assert_eq!(n, 8);
        assert_eq!(min, 1.0);
        assert_eq!(max, 9.0);
        // 1+1+2+3+4+5+6+9 = 31, 31/8 = 3.875.
        assert_eq!(mean, 3.875);
        // Sorted: 1 1 2 3 4 5 6 9. h = 7·0.5 = 3.5 → 3 + 0.5·(4 − 3) = 3.5.
        assert_eq!(p50, 3.5);
        // h = 7·0.95 = 6.65 → 6 + 0.65·(9 − 6) = 7.95.
        assert!((p95 - 7.95).abs() < 1e-12, "{p95}");
        assert_eq!(interpolation, Interpolation::Type7Linear);
        assert_eq!(rejected, 0);
    }

    /// Permuting the input changes nothing, bit for bit — the determinism property of
    /// item 4 of the build rules.
    #[test]
    fn the_summary_is_order_independent_bit_for_bit() {
        let base = [1e16, 1.0, -1e16, 0.5, 3.25, -7.125, 1e-9, 42.0, 0.0, -0.5];
        let forward = Distribution {
            values: base.to_vec(),
            rejected: 0,
        }
        .summary(1);
        let mut reversed = base.to_vec();
        reversed.reverse();
        let backward = Distribution {
            values: reversed,
            rejected: 0,
        }
        .summary(1);
        let mut rotated = base.to_vec();
        rotated.rotate_left(3);
        let rotated = Distribution {
            values: rotated,
            rejected: 0,
        }
        .summary(1);
        let bits = |s: DistributionSummary| match s {
            DistributionSummary::Summary {
                min,
                max,
                mean,
                p50,
                p95,
                p99,
                ..
            } => [
                min.to_bits(),
                max.to_bits(),
                mean.to_bits(),
                p50.to_bits(),
                p95.to_bits(),
                p99.to_bits(),
            ],
            DistributionSummary::Insufficient { .. } => panic!("expected a summary"),
        };
        assert_eq!(bits(forward), bits(backward));
        assert_eq!(bits(forward), bits(rotated));
    }

    #[test]
    fn non_finite_samples_are_refused_and_counted() {
        let mut d = Distribution::new();
        assert!(d.observe(1.0));
        assert!(!d.observe(f64::NAN));
        assert!(!d.observe(f64::INFINITY));
        assert_eq!(d.len(), 1);
        assert_eq!(d.rejected(), 2);
        let DistributionSummary::Summary { p99, rejected, .. } = d.summary(1) else {
            panic!("expected a summary");
        };
        assert_eq!(p99, 1.0, "a NaN must not contaminate the tail");
        assert_eq!(rejected, 2);
    }

    #[test]
    fn merging_is_order_independent() {
        let mut a = Distribution::new();
        a.observe_all([1.0, 2.0]);
        let mut b = Distribution::new();
        b.observe_all([3.0, 4.0]);
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab.summary(1), ba.summary(1));
    }

    #[test]
    fn a_proportion_merge_is_commutative() {
        let mut a = Proportion::from_counts(3, 10);
        let b = Proportion::from_counts(7, 20);
        let mut ba = b;
        a.merge(b);
        ba.merge(Proportion::from_counts(3, 10));
        assert_eq!(a, ba);
        assert_eq!(a.successes(), 10);
        assert_eq!(a.trials(), 30);
        assert_eq!(a.failures(), 20);
    }

    #[test]
    fn quantisation_lands_every_float_on_its_grid() {
        let e = Proportion::from_counts(1, 3).estimate(1, ConfidenceLevel::P95);
        let q = e.quantised(Quantum::RATIO, Quantum::BYTES);
        let RatioEstimate::Proportion {
            point,
            ci_lo,
            ci_hi,
            ..
        } = q
        else {
            panic!("expected a proportion");
        };
        assert!(Quantum::RATIO.holds(point));
        assert!(Quantum::PROBABILITY.holds(ci_lo));
        assert!(Quantum::PROBABILITY.holds(ci_hi));
        assert_eq!(point, 0.3333);
    }
}
