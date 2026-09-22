//! Aggregating across replications: a mean, an interval and a sample count — never a
//! point estimate from one run.
//!
//! 08-measurement-and-data.md §4: "Aggregation computes means and confidence intervals
//! across seeds with the declared method." This module is where a cell's replications
//! become one row, and it refuses to report a row that stands on a single run without
//! saying so.
//!
//! # Two interval methods, and the row says which
//!
//! * A **proportion** pools its replications' successes and trials and reports the
//!   **Wilson score interval** over the pooled counts, computed by
//!   [`v2xw_metrics::Proportion::estimate`]. This crate does not implement an interval of
//!   its own; the one place a binomial interval is computed in this repository is
//!   `v2xw-metrics`, and that is deliberate.
//! * **Anything else** — a ratio of sums, a scalar, a distribution's mean, a count — has
//!   no binomial sampling distribution, so Wilson does not apply. Its interval is the
//!   **normal approximation over the replications**, `mean ± z·s/√k` with `s` the sample
//!   standard deviation over the `k` replications and `z` the same tabulated quantile
//!   [`ConfidenceLevel`] uses. [`CiMethod`] names it in the row.
//!
//! The normal approximation is the honest weakness of this table and is written down
//! rather than hidden: with `k` below roughly thirty it is narrower than a Student-`t`
//! interval would be, and a `t` quantile needs either a transcendental — which build
//! decision D10 forbids on a path whose output is compared across engines — or a table
//! this crate does not ship. The row therefore carries `replications` next to the interval,
//! so a reader can see how much the interval is worth, and [`CellAggregate::stddev`] is
//! reported so a reader can recompute it under any rule they prefer.
//!
//! # One replication is not a measurement
//!
//! With `k = 1` there is a mean and no interval, and [`CiMethod::None`] says so. The mean
//! is still reported, because "we ran this once and got 0.82" is a fact; what is refused is
//! the error bar that would make it look like more than one run.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_metrics::{ConfidenceLevel, Proportion, RatioEstimate};

use crate::plan::CellKey;
use crate::reduce::{RunMetric, RunValue, ordered_sum};

/// Which rule produced a row's interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CiMethod {
    /// The Wilson score interval over the pooled successes and trials.
    Wilson,
    /// `mean ± z·s/√k` over the replications' values.
    NormalOverReplications,
    /// No interval: fewer than two usable replications, or a pooled proportion the
    /// metric's own threshold calls insufficient.
    None,
}

impl core::fmt::Display for CiMethod {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            CiMethod::Wilson => "wilson-score",
            CiMethod::NormalOverReplications => "normal-over-replications",
            CiMethod::None => "none",
        })
    }
}

/// One cell's answer for one metric in one bin: the row of the results table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellAggregate {
    /// The cell.
    pub cell: CellKey,
    /// The metric's name.
    pub metric: String,
    /// Its unit.
    pub unit: String,
    /// The dimension values as canonical JSON.
    pub dims: String,
    /// The aggregation tag the metric declared.
    pub agg: String,
    /// How many replications of this cell were run.
    pub replications: u64,
    /// How many of them produced a usable value.
    pub replications_with_value: u64,
    /// The total number of underlying observations across those replications — trials for
    /// a proportion, observations otherwise.
    pub samples: u64,
    /// The estimate, or `None` when nothing usable was produced.
    pub mean: Option<f64>,
    /// The interval's lower bound.
    pub ci_lo: Option<f64>,
    /// The interval's upper bound.
    pub ci_hi: Option<f64>,
    /// The interval's nominal level.
    pub ci_level: ConfidenceLevel,
    /// Which rule produced the interval.
    pub ci_method: CiMethod,
    /// The sample standard deviation across replications, when there were at least two.
    pub stddev: Option<f64>,
    /// The smallest replication value.
    pub min: Option<f64>,
    /// The largest replication value.
    pub max: Option<f64>,
    /// Every replication's value, in run order, so a reader can redo the arithmetic.
    pub per_replication: Vec<f64>,
}

/// How an aggregation is parameterised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateOptions {
    /// The confidence level for both interval rules.
    pub level: ConfidenceLevel,
    /// The minimum pooled trial count a proportion needs before it is reported as a point
    /// estimate. Defaults to [`v2xw_metrics::stats::DEFAULT_MIN_SAMPLES`].
    pub min_trials: u64,
}

impl Default for AggregateOptions {
    fn default() -> Self {
        AggregateOptions {
            level: ConfidenceLevel::P95,
            min_trials: v2xw_metrics::stats::DEFAULT_MIN_SAMPLES,
        }
    }
}

/// Aggregates one cell's replications.
///
/// `runs` is one entry per replication, each being that run's reduced metrics keyed by
/// metric key. Rows come back in metric-key order, so the table is reproducible.
#[must_use]
pub fn aggregate_cell(
    cell: &CellKey,
    runs: &[BTreeMap<String, RunMetric>],
    options: AggregateOptions,
) -> Vec<CellAggregate> {
    // Every key any replication produced, in key order. A key missing from one run is not
    // an error — a bin can be empty in one seed and busy in another — but it does mean the
    // row's `replications` and `replications_with_value` differ, which the row reports.
    let mut keys: Vec<&String> = Vec::new();
    for run in runs {
        for key in run.keys() {
            keys.push(key);
        }
    }
    keys.sort_unstable();
    keys.dedup();

    let mut rows = Vec::with_capacity(keys.len());
    for key in keys {
        let contributions: Vec<&RunMetric> = runs.iter().filter_map(|r| r.get(key)).collect();
        if let Some(row) = aggregate_key(cell, &contributions, runs.len() as u64, options) {
            rows.push(row);
        }
    }
    rows
}

/// One row, or `None` if no replication produced the key at all.
fn aggregate_key(
    cell: &CellKey,
    contributions: &[&RunMetric],
    replications: u64,
    options: AggregateOptions,
) -> Option<CellAggregate> {
    let first = contributions.first()?;
    let mut values: Vec<f64> = Vec::with_capacity(contributions.len());
    let mut samples: u64 = 0;
    let mut successes: u64 = 0;
    let mut trials: u64 = 0;
    let mut all_proportions = true;
    for contribution in contributions {
        samples = samples.saturating_add(contribution.value.n());
        match &contribution.value {
            RunValue::Proportion {
                successes: s,
                trials: t,
            } => {
                successes = successes.saturating_add(*s);
                trials = trials.saturating_add(*t);
            }
            _ => all_proportions = false,
        }
        if let Some(point) = contribution.value.point() {
            values.push(point);
        }
    }

    let replications_with_value = values.len() as u64;
    let spread = Spread::of(&values);
    let mut row = CellAggregate {
        cell: cell.clone(),
        metric: first.metric.clone(),
        unit: first.unit.clone(),
        dims: first.dims.clone(),
        agg: first.agg.clone(),
        replications,
        replications_with_value,
        samples,
        mean: spread.mean,
        ci_lo: None,
        ci_hi: None,
        ci_level: options.level,
        ci_method: CiMethod::None,
        stddev: spread.stddev,
        min: spread.min,
        max: spread.max,
        per_replication: values,
    };

    if all_proportions && !contributions.is_empty() {
        // Pooled counts, one interval, computed by `v2xw-metrics` and not here.
        row.samples = trials;
        match Proportion::from_counts(successes, trials).estimate(options.min_trials, options.level)
        {
            RatioEstimate::Proportion {
                point,
                ci_lo,
                ci_hi,
                ..
            } => {
                row.mean = Some(point);
                row.ci_lo = Some(ci_lo);
                row.ci_hi = Some(ci_hi);
                row.ci_method = CiMethod::Wilson;
            }
            // Below the metric's threshold: no point estimate, which is the whole reason
            // `RatioEstimate` has the variant. The replications' own values stay in the
            // row so a reader can see what was thrown away.
            _ => {
                row.mean = None;
                row.ci_method = CiMethod::None;
            }
        }
        return Some(row);
    }

    // Everything else: the normal approximation over the replications, and no interval at
    // all below two of them.
    if let (Some(mean), Some(stddev)) = (spread.mean, spread.stddev)
        && replications_with_value >= 2
    {
        let half_width =
            options.level.z() * stddev / v2xw_core::math::sqrt(replications_with_value as f64);
        if half_width.is_finite() {
            row.ci_lo = Some(mean - half_width);
            row.ci_hi = Some(mean + half_width);
            row.ci_method = CiMethod::NormalOverReplications;
        }
    }
    Some(row)
}

/// The mean, sample standard deviation and extremes of a set of replication values.
#[derive(Debug, Clone, Copy, Default)]
struct Spread {
    mean: Option<f64>,
    stddev: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
}

impl Spread {
    /// Reduces in IEEE-754 total order, so the answer is a function of the multiset.
    fn of(values: &[f64]) -> Spread {
        if values.is_empty() {
            return Spread::default();
        }
        let count = values.len() as f64;
        let mut sorted = values.to_vec();
        v2xw_core::math::sort_total_order(&mut sorted);
        let mean = v2xw_core::math::sum_ordered(sorted.iter().copied()) / count;
        let min = sorted.first().copied();
        let max = sorted.last().copied();
        let stddev = if values.len() >= 2 {
            let mut squares: Vec<f64> = sorted.iter().map(|x| (x - mean) * (x - mean)).collect();
            let variance = ordered_sum(&mut squares) / (count - 1.0);
            let s = v2xw_core::math::sqrt(variance);
            s.is_finite().then_some(s)
        } else {
            None
        };
        Spread {
            mean: mean.is_finite().then_some(mean),
            stddev,
            min,
            max,
        }
    }
}
