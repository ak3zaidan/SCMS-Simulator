//! Arrow record batches: the tabular form of a run's metrics.
//!
//! ADR 0008 decision 2: "**Arrow IPC** (Apache-2.0) for tabular metric batches and for
//! Python plug-in exchange; **Parquet** for exported tables", and
//! 08-measurement-and-data.md §5's `metrics` exporter writes `metric.sample` as Parquet.
//! This module produces the record batch both of those start from; writing it to Parquet or
//! to an IPC stream is `v2xw-record`'s job, and [`to_ipc`] is here for the in-process
//! hand-off (a Python provider, a notebook) that does not go through a file.
//!
//! # One wide long-form table, not one table per shape
//!
//! A sample is a scalar, a ratio, a distribution or a count, and the four carry different
//! fields. Splitting them into four tables would make the common query — "every value of
//! `pdr` by distance bin over time" — a union, and would put the schema's shape at the mercy
//! of which metrics a run happened to enable. So there is one schema
//! ([`SAMPLE_SCHEMA_ID`]) with a `state` column saying which shape each row is and the
//! fields of the other shapes null. Every column's meaning is fixed; nothing is overloaded.
//!
//! # Every float is quantised at the writer (D9)
//!
//! [`crate::MetricSample::new`] already quantises, so the re-quantisation here is
//! idempotent — and it is done anyway, because D9 says the *writer* quantises and a writer
//! that trusted its input would be one refactor away from not doing it. The `*_grid` columns
//! carry the integer multiple of the quantum, which is what a cross-platform digest compares
//! (see [`crate::DigestSet`]); a reader that wants to compare two runs' tables compares those
//! and not the floats.
//!
//! The dimension values go into one `dims` column as **canonical JSON** — compact, keys
//! sorted at every level (`v2xw_core::hash::canonical_json`) — rather than into one column
//! per dimension, because the dimension set differs per metric and a column per dimension
//! would be mostly null and would change shape with the scenario. The canonical encoding
//! makes the string comparable and groupable, which is what a query needs it for.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, UInt64Array,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use v2xw_core::hash::canonical_json;

use crate::def::{MetricSample, SampleValue};
use crate::detection::{Cell, ConfusionMatrix, DetectionLevel};
use crate::error::{MetricError, Result};
use crate::quant::Quantum;
use crate::stats::{ConfidenceLevel, DistributionSummary, Estimate, RatioEstimate};

/// The schema id of the metric-sample table (08-measurement-and-data.md §5: "every file
/// carries `schema`").
pub const SAMPLE_SCHEMA_ID: &str = "v2xw/metric-sample/1";

/// The schema id of the confusion-matrix table.
pub const CONFUSION_SCHEMA_ID: &str = "v2xw/confusion-matrix/1";

/// Which shape a row's value is, in the `state` column.
fn state_of(v: &SampleValue) -> &'static str {
    match v {
        SampleValue::Scalar(Estimate::Insufficient { .. })
        | SampleValue::Ratio(RatioEstimate::Insufficient { .. })
        | SampleValue::Distribution(DistributionSummary::Insufficient { .. }) => "insufficient",
        SampleValue::Scalar(_) => "scalar",
        SampleValue::Ratio(RatioEstimate::Proportion { .. }) => "proportion",
        SampleValue::Ratio(RatioEstimate::RatioOfSums { .. }) => "ratio-of-sums",
        SampleValue::Distribution(_) => "distribution",
        SampleValue::Count { .. } => "count",
    }
}

/// The metric-sample table's schema.
///
/// Columns, in order:
///
/// | Column | Type | Meaning |
/// |---|---|---|
/// | `t_ns` | `UInt64` | the instant, in `SimTime` nanoseconds |
/// | `metric` | `Utf8` | the metric's name |
/// | `unit` | `Utf8` | its unit |
/// | `agg` | `Utf8` | its aggregation's tag |
/// | `visibility` | `Utf8` | the canonical visibility tag |
/// | `diagnostic` | `Boolean` | true for a machine-dependent runtime diagnostic |
/// | `dims` | `Utf8` | the dimension values as canonical JSON |
/// | `state` | `Utf8` | `scalar`, `proportion`, `ratio-of-sums`, `distribution`, `count` or `insufficient` |
/// | `quantum` | `Float64` | the grid every float in the row sits on |
/// | `n` | `UInt64` | the sample count behind the value |
/// | `required_n` | `UInt64` | for an insufficient row, how many samples were needed |
/// | `value` | `Float64` | the point estimate, null when insufficient |
/// | `value_grid` | `Int64` | the point estimate as an integer multiple of `quantum` |
/// | `count` | `UInt64` | the exact count, for a `count` row |
/// | `successes` | `UInt64` | for a proportion, the numerator's count |
/// | `ci_lo`, `ci_hi` | `Float64` | the Wilson bounds, for a proportion |
/// | `ci_level` | `Float64` | the interval's nominal coverage |
/// | `numerator`, `denominator` | `Float64` | the two sums, for a ratio of sums |
/// | `min`, `max`, `mean`, `p50`, `p95`, `p99` | `Float64` | for a distribution |
/// | `interpolation` | `Utf8` | the percentile rule a distribution row used |
/// | `rejected` | `UInt64` | non-finite samples a distribution row refused |
#[must_use]
pub fn sample_schema() -> SchemaRef {
    Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("t_ns", DataType::UInt64, false),
            Field::new("metric", DataType::Utf8, false),
            Field::new("unit", DataType::Utf8, false),
            Field::new("agg", DataType::Utf8, false),
            Field::new("visibility", DataType::Utf8, false),
            Field::new("diagnostic", DataType::Boolean, false),
            Field::new("dims", DataType::Utf8, false),
            Field::new("state", DataType::Utf8, false),
            Field::new("quantum", DataType::Float64, false),
            Field::new("n", DataType::UInt64, false),
            Field::new("required_n", DataType::UInt64, true),
            Field::new("value", DataType::Float64, true),
            Field::new("value_grid", DataType::Int64, true),
            Field::new("count", DataType::UInt64, true),
            Field::new("successes", DataType::UInt64, true),
            Field::new("ci_lo", DataType::Float64, true),
            Field::new("ci_hi", DataType::Float64, true),
            Field::new("ci_level", DataType::Float64, true),
            Field::new("numerator", DataType::Float64, true),
            Field::new("denominator", DataType::Float64, true),
            Field::new("min", DataType::Float64, true),
            Field::new("max", DataType::Float64, true),
            Field::new("mean", DataType::Float64, true),
            Field::new("p50", DataType::Float64, true),
            Field::new("p95", DataType::Float64, true),
            Field::new("p99", DataType::Float64, true),
            Field::new("interpolation", DataType::Utf8, true),
            Field::new("rejected", DataType::UInt64, true),
        ],
        schema_metadata(SAMPLE_SCHEMA_ID),
    ))
}

/// The schema-level metadata of a table this crate writes: **exactly one** entry, the
/// schema id.
///
/// This is the one `std::collections::HashMap` in the crate, and it is here because Arrow's
/// `Schema::new_with_metadata` takes that type and no other. The determinism rule is that no
/// `HashMap` iteration may reach an output ordering or a hash, and a map with one entry has
/// no iteration order to get wrong — which is why this helper exists rather than a map built
/// at each call site: a second entry would make the IPC and Parquet bytes depend on a hash
/// seed. If one is ever needed, the fix is to keep the entries in a `BTreeMap` and let the
/// serialiser see them in sorted order, not to add a line here.
fn schema_metadata(id: &str) -> HashMap<String, String> {
    let mut m = HashMap::with_capacity(1);
    m.insert("schema".to_string(), id.to_string());
    m
}

/// The columns of the metric-sample table, built row by row.
#[derive(Default)]
struct SampleColumns {
    t: Vec<u64>,
    metric: Vec<String>,
    unit: Vec<String>,
    agg: Vec<String>,
    visibility: Vec<String>,
    diagnostic: Vec<bool>,
    dims: Vec<String>,
    state: Vec<&'static str>,
    quantum: Vec<f64>,
    n: Vec<u64>,
    required_n: Vec<Option<u64>>,
    value: Vec<Option<f64>>,
    value_grid: Vec<Option<i64>>,
    count: Vec<Option<u64>>,
    successes: Vec<Option<u64>>,
    ci_lo: Vec<Option<f64>>,
    ci_hi: Vec<Option<f64>>,
    ci_level: Vec<Option<f64>>,
    numerator: Vec<Option<f64>>,
    denominator: Vec<Option<f64>>,
    min: Vec<Option<f64>>,
    max: Vec<Option<f64>>,
    mean: Vec<Option<f64>>,
    p50: Vec<Option<f64>>,
    p95: Vec<Option<f64>>,
    p99: Vec<Option<f64>>,
    interpolation: Vec<Option<String>>,
    rejected: Vec<Option<u64>>,
}

impl SampleColumns {
    /// Pads every optional column that this row did not fill.
    fn pad(&mut self) {
        let rows = self.t.len();
        macro_rules! pad {
            ($($f:ident),*) => { $( while self.$f.len() < rows { self.$f.push(None); } )* };
        }
        pad!(
            required_n,
            value,
            value_grid,
            count,
            successes,
            ci_lo,
            ci_hi,
            ci_level,
            numerator,
            denominator,
            min,
            max,
            mean,
            p50,
            p95,
            p99,
            interpolation,
            rejected
        );
    }

    /// Appends one sample.
    ///
    /// # Errors
    /// [`MetricError::Json`] if the dimension map cannot be encoded, and
    /// [`MetricError::OffGrid`] if a float is off its declared grid after quantisation —
    /// which cannot happen, and is checked rather than assumed, because that check is the
    /// post-condition D9 asks the writer for.
    fn push(&mut self, s: &MetricSample) -> Result<()> {
        let q = s.quantum;
        // The writer's own quantiser, and D9's scanning check in one place.
        //
        // `MetricSample::new` already put every float on the metric's grid, so this is
        // idempotent on any sample built the normal way. It does two things anyway:
        //
        // * it **quantises**, so the table is on-grid whatever it was handed — D9's "one
        //   central writer-side encoder quantises each float"; and
        // * it **refuses an input that was off grid**, naming the metric and the value —
        //   D9's "a scanning test fails the build if any output value sits off its grid".
        //   A float reaching here off grid means a sample was built past the constructor,
        //   which is the defect the scan exists to find, and quietly rounding it would hide
        //   exactly the case worth knowing about.
        // Not every float is on the metric's own grid: a proportion's bounds are on the
        // probability grid and a ratio of sums' two sums on the sum grid, exactly as
        // `SampleValue::quantised` put them there. Each float is therefore checked against
        // **its own** grid, not against the metric's, and not against the finest one —
        // holding a bound to the metric's coarser grid would reject correct output, and
        // accepting any float that happens to sit on the finest grid would check nothing.
        let quantise_on = |x: f64, grid: Quantum| -> Result<f64> {
            if !grid.holds(x) {
                return Err(MetricError::OffGrid {
                    name: s.metric.clone(),
                    value: x,
                    quantum: grid.get(),
                });
            }
            Ok(grid.quantise(x))
        };
        let quantise = |x: f64| -> Result<f64> { quantise_on(x, q) };

        self.t.push(s.t);
        self.metric.push(s.metric.clone());
        self.unit.push(s.unit.clone());
        self.agg.push(s.agg.clone());
        self.visibility.push(s.visibility.to_string());
        self.diagnostic.push(s.diagnostic);
        self.dims.push(
            String::from_utf8(canonical_json(&s.dims).map_err(MetricError::Core)?).map_err(
                |e| MetricError::BadDefinition {
                    name: s.metric.clone(),
                    problem: format!("the dimension map did not encode as UTF-8: {e}"),
                },
            )?,
        );
        self.state.push(state_of(&s.value));
        self.quantum.push(q.get());
        self.n.push(s.value.n());

        match &s.value {
            SampleValue::Scalar(Estimate::Value { point, .. }) => {
                let v = quantise(*point)?;
                self.value.push(Some(v));
                self.value_grid.push(Some(q.grid(v)));
            }
            SampleValue::Scalar(Estimate::Insufficient { required, .. }) => {
                self.required_n.push(Some(*required));
            }
            SampleValue::Ratio(RatioEstimate::Proportion {
                point,
                ci_lo,
                ci_hi,
                level,
                successes,
                ..
            }) => {
                let v = quantise(*point)?;
                self.value.push(Some(v));
                self.value_grid.push(Some(q.grid(v)));
                self.successes.push(Some(*successes));
                // A bound is a statement about a probability, so it goes on D9's
                // probability grid rather than on the ratio's coarser one — and is refused
                // if it arrives off that grid, like every other float here.
                self.ci_lo
                    .push(Some(quantise_on(*ci_lo, Quantum::PROBABILITY)?));
                self.ci_hi
                    .push(Some(quantise_on(*ci_hi, Quantum::PROBABILITY)?));
                self.ci_level.push(Some(level.coverage()));
            }
            SampleValue::Ratio(RatioEstimate::RatioOfSums {
                point,
                numerator,
                denominator,
                ..
            }) => {
                let v = quantise(*point)?;
                self.value.push(Some(v));
                self.value_grid.push(Some(q.grid(v)));
                self.numerator
                    .push(Some(quantise_on(*numerator, Quantum::SUM)?));
                self.denominator
                    .push(Some(quantise_on(*denominator, Quantum::SUM)?));
            }
            SampleValue::Ratio(RatioEstimate::Insufficient { required, .. }) => {
                self.required_n.push(Some(*required));
            }
            SampleValue::Distribution(DistributionSummary::Summary {
                min,
                max,
                mean,
                p50,
                p95,
                p99,
                interpolation,
                rejected,
                ..
            }) => {
                let m = quantise(*mean)?;
                self.value.push(Some(m));
                self.value_grid.push(Some(q.grid(m)));
                self.min.push(Some(quantise(*min)?));
                self.max.push(Some(quantise(*max)?));
                self.mean.push(Some(m));
                self.p50.push(Some(quantise(*p50)?));
                self.p95.push(Some(quantise(*p95)?));
                self.p99.push(Some(quantise(*p99)?));
                self.interpolation.push(Some(interpolation.to_string()));
                self.rejected.push(Some(*rejected));
            }
            SampleValue::Distribution(DistributionSummary::Insufficient { required, .. }) => {
                self.required_n.push(Some(*required));
            }
            SampleValue::Count { count } => {
                self.count.push(Some(*count));
                self.value.push(Some(*count as f64));
                // A count is exact, so its "grid index" is the count itself rather than a
                // multiple of a quantum: a digest over it must not depend on the metric's
                // float grid.
                self.value_grid.push(Some(*count as i64));
            }
        }
        self.pad();
        Ok(())
    }

    fn finish(self) -> Result<RecordBatch> {
        let columns: Vec<ArrayRef> = vec![
            Arc::new(UInt64Array::from(self.t)),
            Arc::new(StringArray::from(self.metric)),
            Arc::new(StringArray::from(self.unit)),
            Arc::new(StringArray::from(self.agg)),
            Arc::new(StringArray::from(self.visibility)),
            Arc::new(BooleanArray::from(self.diagnostic)),
            Arc::new(StringArray::from(self.dims)),
            Arc::new(StringArray::from(self.state)),
            Arc::new(Float64Array::from(self.quantum)),
            Arc::new(UInt64Array::from(self.n)),
            Arc::new(UInt64Array::from(self.required_n)),
            Arc::new(Float64Array::from(self.value)),
            Arc::new(Int64Array::from(self.value_grid)),
            Arc::new(UInt64Array::from(self.count)),
            Arc::new(UInt64Array::from(self.successes)),
            Arc::new(Float64Array::from(self.ci_lo)),
            Arc::new(Float64Array::from(self.ci_hi)),
            Arc::new(Float64Array::from(self.ci_level)),
            Arc::new(Float64Array::from(self.numerator)),
            Arc::new(Float64Array::from(self.denominator)),
            Arc::new(Float64Array::from(self.min)),
            Arc::new(Float64Array::from(self.max)),
            Arc::new(Float64Array::from(self.mean)),
            Arc::new(Float64Array::from(self.p50)),
            Arc::new(Float64Array::from(self.p95)),
            Arc::new(Float64Array::from(self.p99)),
            Arc::new(StringArray::from(self.interpolation)),
            Arc::new(UInt64Array::from(self.rejected)),
        ];
        Ok(RecordBatch::try_new(sample_schema(), columns)?)
    }
}

/// The metric-sample record batch for `samples`, in the order given.
///
/// A caller that wants a stable table sorts first — `ProviderSet::flush` already returns
/// samples sorted by `(key, t)`, and that is the order the digest uses.
///
/// # Errors
/// [`MetricError::Arrow`] if the columns do not fit the schema, [`MetricError::Json`] if a
/// dimension map cannot be encoded, or [`MetricError::OffGrid`] if a float fails the
/// writer's quantisation post-condition.
pub fn samples_batch(samples: &[MetricSample]) -> Result<RecordBatch> {
    let mut columns = SampleColumns::default();
    for s in samples {
        columns.push(s)?;
    }
    columns.finish()
}

/// The confusion-matrix table's schema: one row per detection level, with the four cells and
/// the summaries derived from them.
///
/// The cells are the result and the summaries are conveniences
/// ([`crate::detection`]), so they sit in one row: a reader cannot pick up a precision
/// without seeing the counts it came from.
#[must_use]
pub fn confusion_schema() -> SchemaRef {
    Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("level", DataType::Utf8, false),
            Field::new("tp", DataType::UInt64, false),
            Field::new("fp", DataType::UInt64, false),
            Field::new("fn", DataType::UInt64, false),
            Field::new("tn", DataType::UInt64, false),
            Field::new("total", DataType::UInt64, false),
            Field::new("recall", DataType::Float64, true),
            Field::new("recall_ci_lo", DataType::Float64, true),
            Field::new("recall_ci_hi", DataType::Float64, true),
            Field::new("fpr", DataType::Float64, true),
            Field::new("precision", DataType::Float64, true),
            Field::new("precision_ci_lo", DataType::Float64, true),
            Field::new("precision_ci_hi", DataType::Float64, true),
            Field::new("f1", DataType::Float64, true),
            Field::new("accuracy", DataType::Float64, true),
            Field::new("ci_level", DataType::Float64, false),
        ],
        schema_metadata(CONFUSION_SCHEMA_ID),
    ))
}

/// The confusion-matrix record batch for the given levels.
///
/// Every float is quantised at the writer: the ratios onto D9's ratio grid and the interval
/// bounds onto its probability grid. A summary the matrix is too thin to support is null,
/// never a fabricated number — the counts are still there, which is the point.
///
/// # Errors
/// [`MetricError::Arrow`] if the columns do not fit the schema.
pub fn confusion_batch(
    matrices: &[(DetectionLevel, ConfusionMatrix)],
    min_samples: u64,
    level: ConfidenceLevel,
) -> Result<RecordBatch> {
    let ratio = |x: Option<f64>| x.map(|v| Quantum::RATIO.quantise(v));
    let bound = |x: Option<(f64, f64)>, hi: bool| {
        x.map(|(lo, up)| Quantum::PROBABILITY.quantise(if hi { up } else { lo }))
    };

    let mut levels = Vec::new();
    let (mut tp, mut fp, mut fn_, mut tn, mut total) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut recall = Vec::new();
    let mut recall_lo = Vec::new();
    let mut recall_hi = Vec::new();
    let mut fpr = Vec::new();
    let mut precision = Vec::new();
    let mut precision_lo = Vec::new();
    let mut precision_hi = Vec::new();
    let mut f1 = Vec::new();
    let mut accuracy = Vec::new();
    let mut ci_level = Vec::new();

    for (lvl, m) in matrices {
        levels.push(lvl.as_str());
        tp.push(m.cell(Cell::Tp));
        fp.push(m.cell(Cell::Fp));
        fn_.push(m.cell(Cell::Fn));
        tn.push(m.cell(Cell::Tn));
        total.push(m.total());
        let r = m.recall(min_samples, level);
        recall.push(ratio(r.point()));
        recall_lo.push(bound(r.interval(), false));
        recall_hi.push(bound(r.interval(), true));
        fpr.push(ratio(m.fpr(min_samples, level).point()));
        let p = m.precision(min_samples, level);
        precision.push(ratio(p.point()));
        precision_lo.push(bound(p.interval(), false));
        precision_hi.push(bound(p.interval(), true));
        f1.push(ratio(m.f1(min_samples).point()));
        accuracy.push(ratio(m.accuracy(min_samples, level).point()));
        ci_level.push(level.coverage());
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(levels)),
        Arc::new(UInt64Array::from(tp)),
        Arc::new(UInt64Array::from(fp)),
        Arc::new(UInt64Array::from(fn_)),
        Arc::new(UInt64Array::from(tn)),
        Arc::new(UInt64Array::from(total)),
        Arc::new(Float64Array::from(recall)),
        Arc::new(Float64Array::from(recall_lo)),
        Arc::new(Float64Array::from(recall_hi)),
        Arc::new(Float64Array::from(fpr)),
        Arc::new(Float64Array::from(precision)),
        Arc::new(Float64Array::from(precision_lo)),
        Arc::new(Float64Array::from(precision_hi)),
        Arc::new(Float64Array::from(f1)),
        Arc::new(Float64Array::from(accuracy)),
        Arc::new(Float64Array::from(ci_level)),
    ];
    Ok(RecordBatch::try_new(confusion_schema(), columns)?)
}

/// The batches as one Arrow IPC stream — the in-process hand-off of ADR 0008 decision 2.
///
/// All the batches must share a schema; pass the sample batches and the confusion batches in
/// separate calls.
///
/// # Errors
/// [`MetricError::Arrow`] from the writer, or if `batches` is empty (there would be no
/// schema to write).
pub fn to_ipc(batches: &[RecordBatch]) -> Result<Vec<u8>> {
    let Some(first) = batches.first() else {
        return Err(MetricError::Arrow(
            arrow::error::ArrowError::InvalidArgumentError(
                "an IPC stream needs at least one batch, to carry the schema".to_string(),
            ),
        ));
    };
    let mut out = Vec::new();
    {
        let mut writer = arrow::ipc::writer::StreamWriter::try_new(&mut out, &first.schema())?;
        for b in batches {
            writer.write(b)?;
        }
        writer.finish()?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::def::{Agg, Dim, DimValue, Dims, MetricDef};
    use crate::stats::{Distribution, Proportion};
    use arrow::array::Array;
    use v2xw_core::ctx::Visibility;

    fn def(name: &str, unit: &str, q: Quantum) -> MetricDef {
        MetricDef::new(name, unit, Agg::Mean, Visibility::Node, q, "A test metric.")
            .not_accounting_for("being real")
    }

    fn dims() -> Dims {
        let mut d = Dims::new();
        d.insert(Dim::DistBin, DimValue::label("25-50"));
        d.insert(Dim::Node, DimValue::index(3));
        d
    }

    fn all_four_shapes() -> Vec<MetricSample> {
        let ratio_def = def("pdr", "ratio", Quantum::RATIO);
        let mut d = Distribution::new();
        d.observe_all([1.0, 2.0, 3.0, 4.0]);
        d.observe(f64::NAN);
        vec![
            MetricSample::new(
                &ratio_def,
                1_000,
                dims(),
                SampleValue::Ratio(Proportion::from_counts(3, 4).estimate(1, ConfidenceLevel::P95)),
            ),
            MetricSample::new(
                &def("goodput", "B/s", Quantum::BYTES),
                1_000,
                Dims::new(),
                SampleValue::Ratio(crate::stats::ratio_of_sums(240.0, 600.0, 2, 1)),
            ),
            MetricSample::new(
                &def("e2e_latency", "ms", Quantum::TIME_MS),
                1_000,
                Dims::new(),
                SampleValue::Distribution(d.summary(1)),
            ),
            MetricSample::new(
                &def("ttc_conflicts", "count", Quantum::COUNT),
                1_000,
                Dims::new(),
                SampleValue::count(7),
            ),
            MetricSample::new(
                &ratio_def,
                1_000,
                Dims::new(),
                SampleValue::Ratio(
                    Proportion::from_counts(1, 1).estimate(30, ConfidenceLevel::P95),
                ),
            ),
        ]
    }

    fn col_f64(batch: &RecordBatch, name: &str) -> Vec<Option<f64>> {
        let i = batch.schema().index_of(name).unwrap();
        let a = batch
            .column(i)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        (0..a.len())
            .map(|r| if a.is_null(r) { None } else { Some(a.value(r)) })
            .collect()
    }

    fn col_str(batch: &RecordBatch, name: &str) -> Vec<Option<String>> {
        let i = batch.schema().index_of(name).unwrap();
        let a = batch
            .column(i)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        (0..a.len())
            .map(|r| {
                if a.is_null(r) {
                    None
                } else {
                    Some(a.value(r).to_string())
                }
            })
            .collect()
    }

    fn col_u64(batch: &RecordBatch, name: &str) -> Vec<Option<u64>> {
        let i = batch.schema().index_of(name).unwrap();
        let a = batch
            .column(i)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();
        (0..a.len())
            .map(|r| if a.is_null(r) { None } else { Some(a.value(r)) })
            .collect()
    }

    #[test]
    fn the_batch_has_a_row_per_sample_and_the_declared_schema() {
        let batch = samples_batch(&all_four_shapes()).unwrap();
        assert_eq!(batch.num_rows(), 5);
        assert_eq!(batch.num_columns(), sample_schema().fields().len());
        assert_eq!(
            batch.schema().metadata().get("schema").map(String::as_str),
            Some(SAMPLE_SCHEMA_ID)
        );
    }

    #[test]
    fn each_shape_fills_its_own_columns_and_nulls_the_others() {
        let batch = samples_batch(&all_four_shapes()).unwrap();
        assert_eq!(
            col_str(&batch, "state"),
            vec![
                Some("proportion".to_string()),
                Some("ratio-of-sums".to_string()),
                Some("distribution".to_string()),
                Some("count".to_string()),
                Some("insufficient".to_string()),
            ]
        );
        // The proportion row: a value, successes and both bounds; no sums, no percentiles.
        assert_eq!(col_f64(&batch, "value")[0], Some(0.75));
        assert_eq!(col_u64(&batch, "successes")[0], Some(3));
        assert!(col_f64(&batch, "ci_lo")[0].is_some());
        assert_eq!(col_f64(&batch, "numerator")[0], None);
        assert_eq!(col_f64(&batch, "p95")[0], None);
        // The ratio-of-sums row: both sums, no interval.
        assert_eq!(col_f64(&batch, "value")[1], Some(0.4));
        assert_eq!(col_f64(&batch, "numerator")[1], Some(240.0));
        assert_eq!(col_f64(&batch, "denominator")[1], Some(600.0));
        assert_eq!(col_f64(&batch, "ci_lo")[1], None);
        // The distribution row: the percentiles, the interpolation rule and the refusals.
        assert_eq!(col_f64(&batch, "min")[2], Some(1.0));
        assert_eq!(col_f64(&batch, "max")[2], Some(4.0));
        assert_eq!(col_f64(&batch, "mean")[2], Some(2.5));
        assert_eq!(col_f64(&batch, "p50")[2], Some(2.5));
        assert_eq!(
            col_str(&batch, "interpolation")[2],
            Some("hyndman-fan-type-7-linear".to_string())
        );
        assert_eq!(col_u64(&batch, "rejected")[2], Some(1));
        // The count row.
        assert_eq!(col_u64(&batch, "count")[3], Some(7));
        // The insufficient row: no value, and the threshold it fell short of.
        assert_eq!(col_f64(&batch, "value")[4], None);
        assert_eq!(col_u64(&batch, "required_n")[4], Some(30));
        assert_eq!(col_u64(&batch, "n")[4], Some(1));
    }

    #[test]
    fn the_dims_column_is_canonical_json() {
        let batch = samples_batch(&all_four_shapes()).unwrap();
        assert_eq!(
            col_str(&batch, "dims")[0],
            Some(r#"{"dist_bin":"25-50","node":3}"#.to_string()),
            "compact, keys sorted"
        );
        assert_eq!(col_str(&batch, "dims")[1], Some("{}".to_string()));
    }

    /// Build decision D9: every float in the table sits on the row's declared grid, and the
    /// integer column is the grid index a digest compares.
    #[test]
    fn every_float_in_the_table_is_on_its_declared_grid() {
        let batch = samples_batch(&all_four_shapes()).unwrap();
        let quanta = col_f64(&batch, "quantum");
        for name in ["value", "min", "max", "mean", "p50", "p95", "p99"] {
            for (row, v) in col_f64(&batch, name).into_iter().enumerate() {
                if let Some(v) = v {
                    let q = Quantum::new(quanta[row].unwrap());
                    assert!(q.holds(v), "{name}[{row}] = {v} off the grid {q:?}");
                }
            }
        }
        for name in ["ci_lo", "ci_hi"] {
            for (row, v) in col_f64(&batch, name).into_iter().enumerate() {
                if let Some(v) = v {
                    assert!(
                        Quantum::PROBABILITY.holds(v),
                        "{name}[{row}] = {v} off the probability grid"
                    );
                }
            }
        }
        // The grid column is the integer multiple, so the proportion row's 0.75 on the 1e-4
        // grid is 7500.
        let i = batch.schema().index_of("value_grid").unwrap();
        let grid = batch
            .column(i)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(grid.value(0), 7_500);
        // …and a count's grid value is the count itself, not a multiple of a float grid.
        assert_eq!(grid.value(3), 7);
    }

    /// D9's scanning check, at the writer: a raw float that bypassed
    /// [`MetricSample::new`]'s quantiser is refused by name rather than quietly rounded.
    #[test]
    fn the_writer_refuses_a_float_that_bypassed_the_quantiser() {
        let mut s = MetricSample::new(
            &def("pdr", "ratio", Quantum::RATIO),
            0,
            Dims::new(),
            SampleValue::Scalar(Estimate::Value { point: 0.5, n: 1 }),
        );
        // The fields are public so a reader can destructure a sample; writing a raw,
        // unquantised float back into one is exactly what the scan exists to catch.
        s.value = SampleValue::Scalar(Estimate::Value {
            point: 1.0 / 3.0,
            n: 1,
        });
        let e = samples_batch(&[s]).unwrap_err();
        assert!(
            matches!(&e, MetricError::OffGrid { name, quantum, .. }
                     if name == "pdr" && *quantum == 1e-4),
            "{e}"
        );
        // …and a distribution's percentiles are scanned too, not only the headline value.
        let mut s = MetricSample::new(
            &def("e2e_latency", "ms", Quantum::TIME_MS),
            0,
            Dims::new(),
            SampleValue::Distribution(DistributionSummary::Summary {
                n: 3,
                min: 1.0,
                max: 3.0,
                mean: 2.0,
                p50: 2.0,
                p95: 1.0 / 7.0,
                p99: 3.0,
                interpolation: crate::stats::Interpolation::Type7Linear,
                rejected: 0,
            }),
        );
        // `MetricSample::new` quantised it; put the raw value back.
        if let SampleValue::Distribution(DistributionSummary::Summary { p95, .. }) = &mut s.value {
            *p95 = 1.0 / 7.0;
        }
        assert!(matches!(
            samples_batch(&[s]).unwrap_err(),
            MetricError::OffGrid { .. }
        ));
    }

    /// The same post-condition on the four floats that are **not** on the metric's own
    /// grid. They were quantised on the way out and never checked, so a raw interval bound
    /// or a raw sum was silently rounded rather than reported — the writer's promise to
    /// "refuse an input that was off grid" held for `value` and the percentiles only.
    ///
    /// Each is checked against the grid the writer puts it on: 1e-6 for a bound, 1e-3 for a
    /// sum. Note that both metrics below declare the coarser 1e-4 ratio grid, so a check
    /// against the metric's own quantum would reject the crate's own correct output, and a
    /// check that accepted anything on the finest grid would accept the injected value.
    #[test]
    fn the_writer_refuses_a_bound_or_a_sum_that_bypassed_the_quantiser() {
        let make = |value: SampleValue| {
            MetricSample::new(&def("pdr", "ratio", Quantum::RATIO), 0, Dims::new(), value)
        };
        let mut s = make(SampleValue::Ratio(
            Proportion::from_counts(30, 40).estimate(1, ConfidenceLevel::P95),
        ));
        if let SampleValue::Ratio(RatioEstimate::Proportion { ci_lo, .. }) = &mut s.value {
            *ci_lo = 1.0 / 3.0;
        }
        let e = samples_batch(&[s]).unwrap_err();
        assert!(
            matches!(&e, MetricError::OffGrid { name, quantum, .. }
                     if name == "pdr" && *quantum == 1e-6),
            "{e}"
        );

        let mut s = make(SampleValue::Ratio(crate::stats::ratio_of_sums(
            1.0, 4.0, 40, 1,
        )));
        if let SampleValue::Ratio(RatioEstimate::RatioOfSums { numerator, .. }) = &mut s.value {
            *numerator = 1.0 / 3.0;
        }
        let e = samples_batch(&[s]).unwrap_err();
        assert!(
            matches!(&e, MetricError::OffGrid { name, quantum, .. }
                     if name == "pdr" && *quantum == 1e-3),
            "{e}"
        );

        // …and a sample built the normal way still writes, so the check is not a blanket
        // refusal of the finer grids.
        assert!(
            samples_batch(&[
                make(SampleValue::Ratio(
                    Proportion::from_counts(30, 40).estimate(1, ConfidenceLevel::P95)
                )),
                make(SampleValue::Ratio(crate::stats::ratio_of_sums(
                    1.0, 3.0, 40, 1
                ))),
            ])
            .is_ok()
        );
    }

    /// The determinism rule about `HashMap`: the one map in this crate holds a single
    /// entry, so its iteration order cannot reach the serialised bytes.
    #[test]
    fn a_tables_schema_metadata_has_exactly_one_entry() {
        for schema in [sample_schema(), confusion_schema()] {
            assert_eq!(
                schema.metadata().len(),
                1,
                "a second metadata entry would put the IPC bytes at the mercy of a hash seed"
            );
        }
    }

    #[test]
    fn an_empty_batch_is_valid_and_has_no_rows() {
        let batch = samples_batch(&[]).unwrap();
        assert_eq!(batch.num_rows(), 0);
        assert_eq!(batch.num_columns(), sample_schema().fields().len());
    }

    #[test]
    fn the_confusion_batch_carries_the_cells_beside_the_summaries() {
        let m = ConfusionMatrix {
            tp: 3,
            fp: 1,
            fn_: 1,
            tn: 5,
        };
        let batch =
            confusion_batch(&[(DetectionLevel::Vehicle, m)], 1, ConfidenceLevel::P95).unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(
            batch.schema().metadata().get("schema").map(String::as_str),
            Some(CONFUSION_SCHEMA_ID)
        );
        assert_eq!(col_u64(&batch, "tp")[0], Some(3));
        assert_eq!(col_u64(&batch, "fn")[0], Some(1));
        assert_eq!(col_u64(&batch, "total")[0], Some(10));
        assert_eq!(col_f64(&batch, "recall")[0], Some(0.75));
        assert_eq!(col_f64(&batch, "precision")[0], Some(0.75));
        assert_eq!(col_f64(&batch, "f1")[0], Some(0.75));
        assert_eq!(col_f64(&batch, "accuracy")[0], Some(0.8));
        assert_eq!(col_f64(&batch, "ci_level")[0], Some(0.95));
        assert!(col_f64(&batch, "recall_ci_lo")[0].is_some());
        // F1 has no interval anywhere in the schema, which is the honesty this crate keeps.
        assert!(batch.schema().index_of("f1_ci_lo").is_err());
    }

    #[test]
    fn a_thin_matrix_nulls_its_summaries_and_keeps_its_counts() {
        let m = ConfusionMatrix {
            tp: 1,
            fp: 0,
            fn_: 0,
            tn: 1,
        };
        let batch =
            confusion_batch(&[(DetectionLevel::Report, m)], 30, ConfidenceLevel::P95).unwrap();
        assert_eq!(col_u64(&batch, "tp")[0], Some(1));
        assert_eq!(col_f64(&batch, "recall")[0], None, "too thin to estimate");
        assert_eq!(col_f64(&batch, "precision")[0], None);
    }

    #[test]
    fn the_batches_round_trip_through_an_ipc_stream() {
        let batch = samples_batch(&all_four_shapes()).unwrap();
        let bytes = to_ipc(std::slice::from_ref(&batch)).unwrap();
        let reader = arrow::ipc::reader::StreamReader::try_new(bytes.as_slice(), None).unwrap();
        let read: Vec<RecordBatch> = reader.map(|b| b.unwrap()).collect();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].num_rows(), batch.num_rows());
        assert_eq!(
            read[0]
                .schema()
                .metadata()
                .get("schema")
                .map(String::as_str),
            Some(SAMPLE_SCHEMA_ID),
            "the schema id survives the stream"
        );
        assert_eq!(col_f64(&read[0], "value"), col_f64(&batch, "value"));
    }

    #[test]
    fn an_ipc_stream_needs_a_batch_to_carry_its_schema() {
        assert!(to_ipc(&[]).is_err());
    }

    /// The table is a function of the samples, not of the machine: writing the same samples
    /// twice produces the same bytes.
    #[test]
    fn the_ipc_bytes_are_reproducible() {
        let a = to_ipc(&[samples_batch(&all_four_shapes()).unwrap()]).unwrap();
        let b = to_ipc(&[samples_batch(&all_four_shapes()).unwrap()]).unwrap();
        assert_eq!(a, b);
    }
}
