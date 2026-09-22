//! Reducing one run's `metric.sample` stream to one value per metric per bin.
//!
//! A run emits a sample per metric, per dimension bin, *per window*. Aggregating across
//! replications needs one number per run, so the windows have to be pooled first — and how
//! they are pooled is the difference between a defensible number and an average of
//! averages.
//!
//! | Sample shape | Pooled how | What the run's value is |
//! |---|---|---|
//! | proportion (`pdr`, `det_recall`, …) | successes and trials **added** across windows | the pooled proportion, still as counts, so the Wilson interval is computed once at the end over the real trial count |
//! | ratio of sums (`cbr`, `envelope_overhead`, …) | numerators and denominators added | numerator sum over denominator sum |
//! | scalar | the windows' points averaged, unweighted | that mean |
//! | distribution | the windows' **means** averaged, unweighted | that mean |
//! | count | added | the total |
//!
//! Pooling the counts rather than averaging the windows' ratios is the whole point: a
//! window with four trials and a window with four thousand are not equally informative,
//! and averaging their ratios would say they were.
//!
//! # What is dropped, and said so
//!
//! Two kinds of sample never reach the table, and both are counted so their absence is
//! visible rather than silent:
//!
//! * **an insufficient window.** `RatioEstimate::Insufficient` carries its trial count but
//!   not its successes, so there is nothing to pool; the trials are reported as dropped.
//! * **a diagnostic sample.** `events_per_second`, `wall_clock_per_sim_second` and
//!   `memory_high_water_mark` are machine-dependent by construction — `v2xw-metrics` keeps
//!   them out of every digest for exactly this reason — and a results table that averaged
//!   them across a laptop and a cluster would be reporting the hardware.
//!
//! # Determinism
//!
//! Every float reduction goes through `v2xw_core::math::sort_total_order` followed by
//! `sum_ordered`, so a run's value is a function of the multiset of its windows and not of
//! the order they arrived in. The per-key map is a [`BTreeMap`], so the output order is the
//! key order.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_metrics::stats::{DistributionSummary, Estimate, RatioEstimate};
use v2xw_metrics::{MetricSample, SampleValue};

use crate::error::{ExperimentError, Result};

/// The schema id a run's reduced metrics document carries.
pub const RUN_METRICS_SCHEMA: &str = "v2xw/experiment-run-metrics/1";

/// One run's value for one metric in one bin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "kebab-case")]
pub enum RunValue {
    /// Pooled Bernoulli trials. Kept as counts so the interval is computed once, at the
    /// end, over the real number of trials.
    Proportion {
        /// Successes across the run's windows.
        successes: u64,
        /// Trials across the run's windows.
        trials: u64,
    },
    /// A plain number with however many observations stand behind it.
    Scalar {
        /// The value.
        value: f64,
        /// The underlying observation count, summed over the windows.
        n: u64,
    },
    /// An exact count, summed over the windows.
    Count {
        /// The total.
        count: u64,
    },
    /// The run produced nothing usable for this key.
    Insufficient {
        /// How many observations were seen anyway.
        n: u64,
    },
}

impl RunValue {
    /// The point value this run contributes to an across-replication mean, if any.
    ///
    /// A proportion contributes `successes / trials`; the aggregate itself pools the
    /// counts rather than averaging these, and uses them only to report the spread.
    #[must_use]
    pub fn point(&self) -> Option<f64> {
        match self {
            RunValue::Proportion { successes, trials } => {
                if *trials == 0 {
                    None
                } else {
                    Some((*successes as f64) / (*trials as f64))
                }
            }
            RunValue::Scalar { value, .. } => Some(*value),
            RunValue::Count { count } => Some(*count as f64),
            RunValue::Insufficient { .. } => None,
        }
    }

    /// The observation count behind the value.
    ///
    /// A [`RunValue::Count`] answers `1`, not its total: the total is the measurement, and
    /// counting it as its own sample size would claim a precision the run does not have.
    #[must_use]
    pub fn n(&self) -> u64 {
        match self {
            RunValue::Proportion { trials, .. } => *trials,
            RunValue::Scalar { n, .. } => *n,
            RunValue::Count { .. } => 1,
            RunValue::Insufficient { n } => *n,
        }
    }

    /// True if this value is a pooled proportion, which is what decides whether the
    /// aggregate's interval is a Wilson score interval.
    #[must_use]
    pub const fn is_proportion(&self) -> bool {
        matches!(self, RunValue::Proportion { .. })
    }
}

/// One run's value for one metric in one bin, with the bookkeeping that makes it readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunMetric {
    /// The metric's name.
    pub metric: String,
    /// Its unit, copied from the sample.
    pub unit: String,
    /// The dimension values as canonical JSON, `{}` for an undimensioned metric.
    pub dims: String,
    /// The aggregation tag the metric declared.
    pub agg: String,
    /// The pooled value.
    pub value: RunValue,
    /// How many windows were pooled into it.
    pub windows: u64,
    /// How many windows were seen but could not be pooled — an insufficient window, or a
    /// window whose shape disagreed with the first one's.
    pub dropped_windows: u64,
}

/// One run's reduced metrics, the document written beside its recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunMetrics {
    /// The schema id, [`RUN_METRICS_SCHEMA`].
    pub schema: String,
    /// The run's id.
    pub run_id: String,
    /// Which cell it belongs to.
    pub cell_index: usize,
    /// The master seed it ran under, hexadecimal.
    pub seed_hex: String,
    /// The scenario hash it ran under.
    pub scenario_hash: String,
    /// One entry per metric key, in key order.
    pub metrics: BTreeMap<String, RunMetric>,
}

/// Reads the `metrics.json` a run wrote and parses its samples.
///
/// The file is `{"samples": [...]}`, which is what `v2xw run` writes.
///
/// # Errors
/// [`ExperimentError::Io`] if the file cannot be read and [`ExperimentError::Json`] if it
/// is not the document `v2xw run` writes.
pub fn read_samples(path: &std::path::Path) -> Result<Vec<MetricSample>> {
    let bytes =
        std::fs::read(path).map_err(|e| ExperimentError::io("cannot read the run's", path, e))?;
    #[derive(Deserialize)]
    struct Document {
        samples: Vec<MetricSample>,
    }
    let document: Document = serde_json::from_slice(&bytes)
        .map_err(|e| ExperimentError::json("a run's metrics.json", e))?;
    Ok(document.samples)
}

/// Pools a run's samples into one value per metric key.
///
/// The key is [`MetricSample::key`], which is the metric name and its dimension values.
///
/// # Errors
/// [`ExperimentError::Core`] if a sample's dimensions will not canonicalise.
pub fn reduce_run(samples: &[MetricSample]) -> Result<BTreeMap<String, RunMetric>> {
    let mut accumulators: BTreeMap<String, Accumulator> = BTreeMap::new();
    for sample in samples {
        // Machine-dependent diagnostics never reach a results table; see the module header.
        if sample.diagnostic {
            continue;
        }
        let key = sample.key();
        // Two statements rather than an `entry().or_insert_with()`, because building the
        // fresh accumulator can fail and a closure cannot carry a `?` out.
        if !accumulators.contains_key(&key) {
            let fresh = Accumulator {
                metric: sample.metric.clone(),
                unit: sample.unit.clone(),
                dims: dims_json(sample)?,
                agg: sample.agg.clone(),
                windows: 0,
                dropped_windows: 0,
                kind: None,
            };
            accumulators.insert(key.clone(), fresh);
        }
        if let Some(entry) = accumulators.get_mut(&key) {
            entry.observe(&sample.value);
        }
    }
    Ok(accumulators
        .into_iter()
        .map(|(key, accumulator)| (key, accumulator.finish()))
        .collect())
}

/// A sample's dimensions as canonical JSON, `{"dist_bin":"25-50","rat":"dsrc-80211p"}`.
///
/// Built from the `Display` of the dimension and of its value rather than from `Dim`'s
/// own serialisation, so the string is the one a reader sees in the sample key.
fn dims_json(sample: &MetricSample) -> Result<String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (dimension, value) in &sample.dims {
        map.insert(dimension.to_string(), value.to_string());
    }
    let bytes = v2xw_core::hash::canonical_json(&map)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// What is being accumulated for one key. Fixed by the first window that contributes.
#[derive(Debug)]
enum Kind {
    /// Pooled Bernoulli counts.
    Proportion { successes: u64, trials: u64 },
    /// Pooled numerators and denominators.
    Sums {
        numerators: Vec<f64>,
        denominators: Vec<f64>,
        n: u64,
    },
    /// The windows' point values.
    Points { points: Vec<f64>, n: u64 },
    /// A running total.
    Count { count: u64 },
}

#[derive(Debug)]
struct Accumulator {
    metric: String,
    unit: String,
    dims: String,
    agg: String,
    windows: u64,
    dropped_windows: u64,
    kind: Option<Kind>,
}

/// One window, normalised to the shape it pools as.
///
/// Normalising first is what keeps [`fold`] a flat table of `(what we have, what arrived)`
/// pairs instead of a nest of matches, and it is why a scalar sample and a distribution's
/// mean take the same path: both are one number with an observation count behind it.
#[derive(Debug, Clone, Copy)]
enum Window {
    /// Bernoulli counts.
    Proportion { successes: u64, trials: u64 },
    /// One window's numerator and denominator.
    Sums {
        numerator: f64,
        denominator: f64,
        n: u64,
    },
    /// One number with its observation count.
    Point { point: f64, n: u64 },
    /// An exact count.
    Count { count: u64 },
    /// An insufficient window, or a shape this build does not know. `SampleValue` is
    /// `#[non_exhaustive]`, so the last case is reachable from a newer `v2xw-metrics`.
    Unusable,
}

/// The shape a sample pools as.
fn window_of(value: &SampleValue) -> Window {
    match value {
        SampleValue::Ratio(RatioEstimate::Proportion {
            successes, trials, ..
        }) => Window::Proportion {
            successes: *successes,
            trials: *trials,
        },
        SampleValue::Ratio(RatioEstimate::RatioOfSums {
            numerator,
            denominator,
            n,
            ..
        }) => Window::Sums {
            numerator: *numerator,
            denominator: *denominator,
            n: *n,
        },
        SampleValue::Scalar(Estimate::Value { point, n }) => Window::Point {
            point: *point,
            n: *n,
        },
        SampleValue::Distribution(DistributionSummary::Summary { mean, n, .. }) => Window::Point {
            point: *mean,
            n: *n,
        },
        SampleValue::Count { count } => Window::Count { count: *count },
        _ => Window::Unusable,
    }
}

/// Folds one window into what has been accumulated, answering whether it was pooled.
///
/// Owned in, owned out: nothing here holds a borrow across a reassignment, which is the
/// one thing that makes an accumulator like this awkward to write.
///
/// A window whose shape disagrees with what is already there is **not** merged. The last
/// arm restores the accumulator untouched and answers `false`, and the caller counts it as
/// dropped, so a metric that changed shape mid-run shows up as dropped windows rather than
/// as a silently mixed number.
fn fold(existing: Option<Kind>, window: Window) -> (Option<Kind>, bool) {
    match (existing, window) {
        (None, Window::Proportion { successes, trials }) => {
            (Some(Kind::Proportion { successes, trials }), true)
        }
        (
            Some(Kind::Proportion {
                successes: have,
                trials: seen,
            }),
            Window::Proportion { successes, trials },
        ) => (
            Some(Kind::Proportion {
                successes: have.saturating_add(successes),
                trials: seen.saturating_add(trials),
            }),
            true,
        ),
        (
            None,
            Window::Sums {
                numerator,
                denominator,
                n,
            },
        ) => (
            Some(Kind::Sums {
                numerators: vec![numerator],
                denominators: vec![denominator],
                n,
            }),
            true,
        ),
        (
            Some(Kind::Sums {
                mut numerators,
                mut denominators,
                n: total,
            }),
            Window::Sums {
                numerator,
                denominator,
                n,
            },
        ) => {
            numerators.push(numerator);
            denominators.push(denominator);
            (
                Some(Kind::Sums {
                    numerators,
                    denominators,
                    n: total.saturating_add(n),
                }),
                true,
            )
        }
        (None, Window::Point { point, n }) => (
            Some(Kind::Points {
                points: vec![point],
                n,
            }),
            true,
        ),
        (
            Some(Kind::Points {
                mut points,
                n: total,
            }),
            Window::Point { point, n },
        ) => {
            points.push(point);
            (
                Some(Kind::Points {
                    points,
                    n: total.saturating_add(n),
                }),
                true,
            )
        }
        (None, Window::Count { count }) => (Some(Kind::Count { count }), true),
        (Some(Kind::Count { count: total }), Window::Count { count }) => (
            Some(Kind::Count {
                count: total.saturating_add(count),
            }),
            true,
        ),
        (other, _) => (other, false),
    }
}

impl Accumulator {
    /// Pools one window, counting it as pooled or as dropped.
    fn observe(&mut self, value: &SampleValue) {
        let existing = self.kind.take();
        let (kind, pooled) = fold(existing, window_of(value));
        self.kind = kind;
        if pooled {
            self.windows += 1;
        } else {
            self.dropped_windows += 1;
        }
    }

    fn finish(self) -> RunMetric {
        let value = match self.kind {
            None => RunValue::Insufficient { n: 0 },
            Some(Kind::Proportion { successes, trials }) => {
                RunValue::Proportion { successes, trials }
            }
            Some(Kind::Sums {
                mut numerators,
                mut denominators,
                n,
            }) => {
                let numerator = ordered_sum(&mut numerators);
                let denominator = ordered_sum(&mut denominators);
                if denominator == 0.0 || !numerator.is_finite() || !denominator.is_finite() {
                    RunValue::Insufficient { n }
                } else {
                    RunValue::Scalar {
                        value: numerator / denominator,
                        n,
                    }
                }
            }
            Some(Kind::Points { mut points, n }) => {
                if points.is_empty() {
                    RunValue::Insufficient { n }
                } else {
                    let count = points.len() as f64;
                    let mean = ordered_sum(&mut points) / count;
                    if mean.is_finite() {
                        RunValue::Scalar { value: mean, n }
                    } else {
                        RunValue::Insufficient { n }
                    }
                }
            }
            Some(Kind::Count { count }) => RunValue::Count { count },
        };
        RunMetric {
            metric: self.metric,
            unit: self.unit,
            dims: self.dims,
            agg: self.agg,
            value,
            windows: self.windows,
            dropped_windows: self.dropped_windows,
        }
    }
}

/// Sums in IEEE-754 total order, which makes the result a function of the multiset.
pub(crate) fn ordered_sum(values: &mut Vec<f64>) -> f64 {
    v2xw_core::math::sort_total_order(values);
    v2xw_core::math::sum_ordered(values.iter().copied())
}
