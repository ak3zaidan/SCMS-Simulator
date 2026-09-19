//! [`MetricDef`] and [`MetricSample`]: what a metric *is*, and one measurement of it.
//!
//! 03-interfaces.md §10 publishes the definition type as
//!
//! ```text
//! pub struct MetricDef { name, unit, dims, agg, visibility, definition_md, source }
//! ```
//!
//! and this is that type, with four fields the design's principles imply but the sketch
//! does not spell:
//!
//! | Added field | Why it has to be on the definition |
//! |---|---|
//! | [`MetricDef::quantum`] | Build decision D9: "Every schema field that carries a float declares its quantum … The quantum is part of the field's contract". A metric value *is* such a field, and the writer needs the grid at the point it writes. |
//! | [`MetricDef::min_samples`] | "A bin with too few samples reports as insufficient rather than as a point estimate." The threshold is part of the metric's contract, not of the caller's mood. |
//! | [`MetricDef::not_accounted`] | 08-measurement-and-data.md §1: "no black box". Every model card carries `ignores`; a metric that does not say what it leaves out invites the reader to assume it leaves out nothing. |
//! | [`MetricDef::diagnostic`] | 08 §2's runtime numbers are machine-dependent. The flag is what [`crate::DigestSet`] partitions on, so the exclusion is structural rather than a convention. |
//!
//! `MetricDef` is what the catalog page is generated from (§1: "The catalog page is
//! generated from the definitions, so the docs cannot drift from the code"), which is why
//! `definition_md` holds the formula in prose and `source` holds the citation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::Source;
use v2xw_core::ctx::{Record, Visibility};
use v2xw_core::time::SimTime;

use crate::error::{MetricError, Result};
use crate::quant::Quantum;
use crate::stats::{ConfidenceLevel, DistributionSummary, Estimate, RatioEstimate};

/// A dimension a metric is broken down by (08-measurement-and-data.md §2).
///
/// The first ten are the document's declared list; the rest are the per-metric dimensions
/// its own tables name (`cause` on `pdr_by_cause`, `channel` on `cbr`, `primitive` on
/// `verify_rate`, and so on). Closed on purpose: a dimension is a column in every exported
/// table and a facet in every figure, so adding one is a schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Dim {
    /// Time bin.
    T,
    /// Node.
    Node,
    /// Vehicle class (car/truck/bus/moto/vru/rsu).
    Class,
    /// Distance bin, 25 m by default.
    DistBin,
    /// Density bin (veh/km per lane, or veh/km²).
    DensityBin,
    /// Region of the world.
    Region,
    /// Credential-management protocol.
    Protocol,
    /// Radio access technology.
    Rat,
    /// Fidelity tier.
    Tier,
    /// Run (experiment cell).
    Run,
    /// Loss cause, for `pdr_by_cause`.
    Cause,
    /// 5.9 GHz channel number, for `cbr`.
    Channel,
    /// Cryptographic primitive, for `verify_rate`.
    Primitive,
    /// Message type.
    MsgType,
    /// Byte-accounting bucket (air / cellular UL / cellular DL / backhaul / backend).
    Bucket,
    /// Detector id.
    Detector,
    /// Revocation stage id (05-protocols.md §8).
    Stage,
    /// Neighborhood radius, for `nar`.
    Radius,
    /// What a detection metric is measured over: `report` or `vehicle`
    /// (08-measurement-and-data.md §2.4: "over reports … and over vehicles").
    ///
    /// Declared before [`Dim::Cell`] so that a sample key reads `level=vehicle|cell=tp`:
    /// `Dim`'s declaration order is the order a key and an exported row are written in.
    Level,
    /// A confusion matrix's cell: `tp`, `fp`, `fn`, `tn`. 08-measurement-and-data.md §2.4
    /// names the summaries and not the cells; this crate reports the matrix itself, so it
    /// needs a dimension to report it along.
    Cell,
}

impl core::fmt::Display for Dim {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let json = serde_json::to_string(self).unwrap_or_else(|_| "\"?\"".to_string());
        f.write_str(json.trim_matches('"'))
    }
}

/// One dimension's value on one sample.
///
/// Three shapes, all of which compare and sort deterministically: a string label (a bin
/// label, a cause, a protocol id), an integer (a node id, a channel number) and a time bin
/// index. No float: a dimension value is a key, and a float key is a key that two platforms
/// can disagree about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DimValue {
    /// A label: a bin label (`"25-50"`), an enumeration (`"collision"`), an id (`"rsu-3"`).
    Label(String),
    /// An integer: a node index, a channel number, a bin index.
    Index(u64),
}

impl DimValue {
    /// A label value.
    #[must_use]
    pub fn label(s: impl Into<String>) -> Self {
        DimValue::Label(s.into())
    }

    /// An integer value.
    #[must_use]
    pub const fn index(i: u64) -> Self {
        DimValue::Index(i)
    }
}

impl core::fmt::Display for DimValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DimValue::Label(s) => f.write_str(s),
            DimValue::Index(i) => write!(f, "{i}"),
        }
    }
}

/// The dimension values of one sample.
///
/// A [`BTreeMap`], so iteration — and therefore the canonical JSON, the Arrow column and
/// the digest — is in a fixed order whatever order the provider filled it in. A `HashMap`
/// here would put the crate's own output ordering at the mercy of a hash seed, which the
/// determinism contract forbids.
pub type Dims = BTreeMap<Dim, DimValue>;

/// How a metric's raw observations are reduced (03-interfaces.md §10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Agg {
    /// A sum over the window.
    Sum,
    /// An arithmetic mean over the window, reduced over the sorted sample.
    Mean,
    /// The maximum over the window.
    Max,
    /// A count.
    Count,
    /// A quantile, with the fraction stated. §10 spells these `p50`, `p95`; naming the
    /// fraction means the output says which one it is rather than relying on the metric's
    /// name.
    Quantile {
        /// The fraction, in `[0, 1]`.
        q: f64,
    },
    /// A ratio, with both terms named as §10 requires (`ratio(num, den)`).
    Ratio {
        /// What is counted in the numerator.
        numerator: String,
        /// What is counted in the denominator.
        denominator: String,
    },
    /// A per-second rate.
    Rate,
    /// A full distribution: count, extremes, mean and three percentiles.
    Distribution,
}

impl Agg {
    /// A ratio aggregation.
    #[must_use]
    pub fn ratio(numerator: impl Into<String>, denominator: impl Into<String>) -> Self {
        Agg::Ratio {
            numerator: numerator.into(),
            denominator: denominator.into(),
        }
    }

    /// The short spelling that goes in the `agg` column of an exported table.
    #[must_use]
    pub fn tag(&self) -> String {
        match self {
            Agg::Sum => "sum".to_string(),
            Agg::Mean => "mean".to_string(),
            Agg::Max => "max".to_string(),
            Agg::Count => "count".to_string(),
            Agg::Quantile { q } => format!("p{}", (q * 100.0).round() as i64),
            Agg::Ratio { .. } => "ratio".to_string(),
            Agg::Rate => "rate".to_string(),
            Agg::Distribution => "distribution".to_string(),
        }
    }
}

/// What a metric is: its name, unit, dimensions, reduction, visibility, definition and
/// citation — plus its grid, its insufficiency threshold, what it does not account for, and
/// whether it is a machine-dependent diagnostic.
///
/// Construct with [`MetricDef::new`] and the builder methods; [`MetricDef::validate`]
/// checks the constraints a definition can violate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDef {
    /// The metric's stable name, e.g. `pdr`, `verify_rate`, `ttc_min`.
    pub name: String,
    /// The unit, e.g. `ratio`, `ms`, `B/s`, `1/s`, `s`, `veh/h`.
    pub unit: String,
    /// The dimensions this metric is broken down by.
    pub dims: Vec<Dim>,
    /// How the observations are reduced.
    pub agg: Agg,
    /// Who may see it. A metric derived from a `GT` channel is `GT`
    /// (08-measurement-and-data.md §1: "Visibility tags propagate").
    #[serde(with = "crate::vis")]
    pub visibility: Visibility,
    /// The definition, in Markdown, including the formula. The catalog page is generated
    /// from this field.
    pub definition_md: String,
    /// The citation for the definition, where one exists.
    pub source: Option<Source>,
    /// The grid every value of this metric is quantised onto at the writer (D9).
    pub quantum: Quantum,
    /// Below this many samples the metric reports insufficient rather than a point estimate.
    pub min_samples: u64,
    /// **What this metric does not account for.** One entry per omission, in the same
    /// spirit as a model card's `ignores`: the reader is told what the number excludes
    /// rather than left to assume it excludes nothing.
    pub not_accounted: Vec<String>,
    /// True if the metric is a machine-dependent runtime diagnostic. Such a metric is
    /// excluded from every digested artefact, structurally: see [`crate::DigestSet`].
    pub diagnostic: bool,
}

impl MetricDef {
    /// A definition with the required fields; the rest default and are set with the builder
    /// methods.
    ///
    /// Defaults: no dimensions, no source, [`crate::stats::DEFAULT_MIN_SAMPLES`], nothing
    /// declared as unaccounted-for, not a diagnostic.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        unit: impl Into<String>,
        agg: Agg,
        visibility: Visibility,
        quantum: Quantum,
        definition_md: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            unit: unit.into(),
            dims: Vec::new(),
            agg,
            visibility,
            definition_md: definition_md.into(),
            source: None,
            quantum,
            min_samples: crate::stats::DEFAULT_MIN_SAMPLES,
            not_accounted: Vec::new(),
            diagnostic: false,
        }
    }

    /// Sets the dimensions.
    #[must_use]
    pub fn with_dims(mut self, dims: impl IntoIterator<Item = Dim>) -> Self {
        self.dims = dims.into_iter().collect();
        self
    }

    /// Sets the citation.
    #[must_use]
    pub fn with_source(mut self, source: Source) -> Self {
        self.source = Some(source);
        self
    }

    /// Sets the insufficiency threshold.
    #[must_use]
    pub const fn with_min_samples(mut self, n: u64) -> Self {
        self.min_samples = n;
        self
    }

    /// Declares one thing the metric does not account for.
    #[must_use]
    pub fn not_accounting_for(mut self, what: impl Into<String>) -> Self {
        self.not_accounted.push(what.into());
        self
    }

    /// Marks the metric a machine-dependent runtime diagnostic.
    #[must_use]
    pub const fn as_diagnostic(mut self) -> Self {
        self.diagnostic = true;
        self
    }

    /// Checks the constraints a definition can violate.
    ///
    /// * the name and the unit are non-empty;
    /// * the definition text is non-empty — a metric with no stated formula is the black
    ///   box 08-measurement-and-data.md §1 forbids;
    /// * a `Quantile` aggregation's `q` is a finite fraction in `[0, 1]`;
    /// * a `Ratio` aggregation names both of its terms;
    /// * the dimension list has no duplicates;
    /// * **every metric declares at least one thing it does not account for.** There is no
    ///   metric that accounts for everything, so an empty list means the author did not
    ///   think about it rather than that the list is empty.
    ///
    /// # Errors
    /// [`MetricError::BadDefinition`], naming the metric and the problem.
    pub fn validate(&self) -> Result<()> {
        let bad = |problem: String| MetricError::BadDefinition {
            name: self.name.clone(),
            problem,
        };
        if self.name.trim().is_empty() {
            return Err(bad("the name is empty".to_string()));
        }
        if self.unit.trim().is_empty() {
            return Err(bad("the unit is empty".to_string()));
        }
        if self.definition_md.trim().is_empty() {
            return Err(bad("the definition text is empty".to_string()));
        }
        match &self.agg {
            Agg::Quantile { q } => {
                if !q.is_finite() || !(0.0..=1.0).contains(q) {
                    return Err(bad(format!("quantile fraction {q} is not in [0, 1]")));
                }
            }
            Agg::Ratio {
                numerator,
                denominator,
            } if numerator.trim().is_empty() || denominator.trim().is_empty() => {
                return Err(bad(
                    "a ratio aggregation must name both its numerator and its denominator"
                        .to_string(),
                ));
            }
            _ => {}
        }
        let mut seen = std::collections::BTreeSet::new();
        for d in &self.dims {
            if !seen.insert(*d) {
                return Err(bad(format!("dimension {d} is listed twice")));
            }
        }
        if self.not_accounted.is_empty() {
            return Err(bad(
                "no metric accounts for everything: declare at least one omission in \
                 `not_accounted`"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

/// The value of one sample: a scalar, a ratio, a distribution, a count — or a refusal.
///
/// Every variant that carries a float also carries the sample count behind it, because
/// item 3 of this crate's contract is that no aggregate is reported without one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SampleValue {
    /// A scalar with its sample count, or a refusal.
    Scalar(Estimate),
    /// A ratio: a proportion with its Wilson interval, a ratio of sums, or a refusal.
    Ratio(RatioEstimate),
    /// A distribution: count, extremes, mean, percentiles and the interpolation rule.
    Distribution(DistributionSummary),
    /// An exact count. Integer, so it is never quantised and never insufficient: a count
    /// of zero events is a measurement, not a missing one.
    ///
    /// A struct variant rather than a newtype one because [`SampleValue`] is **internally
    /// tagged**, and serde cannot write an internally tagged newtype variant that holds a
    /// bare integer — there is no map to put the tag in. A newtype variant here compiled and
    /// then failed at run time, in the run summary, for every count metric. Construct it
    /// with [`SampleValue::count`].
    Count {
        /// The count.
        count: u64,
    },
}

impl SampleValue {
    /// An exact count.
    #[must_use]
    pub const fn count(count: u64) -> Self {
        SampleValue::Count { count }
    }

    /// The point estimate, where there is one.
    #[must_use]
    pub fn point(&self) -> Option<f64> {
        match self {
            SampleValue::Scalar(e) => e.point(),
            SampleValue::Ratio(r) => r.point(),
            SampleValue::Distribution(d) => d.mean(),
            SampleValue::Count { count } => Some(*count as f64),
        }
    }

    /// The sample count behind the value.
    #[must_use]
    pub fn n(&self) -> u64 {
        match self {
            SampleValue::Scalar(e) => e.n(),
            SampleValue::Ratio(r) => r.n(),
            SampleValue::Distribution(d) => d.n(),
            SampleValue::Count { .. } => 1,
        }
    }

    /// True if the value is a refusal rather than an estimate.
    #[must_use]
    pub fn is_insufficient(&self) -> bool {
        match self {
            SampleValue::Scalar(e) => e.is_insufficient(),
            SampleValue::Ratio(r) => r.is_insufficient(),
            SampleValue::Distribution(d) => d.is_insufficient(),
            SampleValue::Count { .. } => false,
        }
    }

    /// Every float in the value, rounded onto `q` — the writer-side quantisation of D9.
    #[must_use]
    pub fn quantised(self, q: Quantum) -> Self {
        match self {
            SampleValue::Scalar(e) => SampleValue::Scalar(e.quantised(q)),
            // The two sums of a `RatioOfSums` are whatever the metric counts — bytes,
            // metres, seconds — and D9 puts all three on the same 1e-3 grid, which is what
            // `Quantum::SUM` names.
            SampleValue::Ratio(r) => SampleValue::Ratio(r.quantised(q, Quantum::SUM)),
            SampleValue::Distribution(d) => SampleValue::Distribution(d.quantised(q)),
            SampleValue::Count { count } => SampleValue::Count { count },
        }
    }

    /// Every float in the value paired with **the grid the writer actually put it on** —
    /// the scanning side of D9's contract.
    ///
    /// [`SampleValue::quantised`] does not put every float on the metric's own grid: a
    /// proportion's two interval bounds go onto [`Quantum::PROBABILITY`], because a bound is
    /// a statement about a probability, and a ratio of sums' two sums go onto
    /// [`Quantum::SUM`], because they are bytes or seconds rather than the ratio. A scan
    /// that compared every float with the metric's quantum would therefore report the
    /// writer's own correct output as a violation on any metric whose grid is coarser than
    /// 1e-6 or 1e-3.
    ///
    /// The previous way round that — accepting a float that is on *either* the metric's grid
    /// or the probability grid — cannot fail, because 1e-6 is the finest grid in the crate,
    /// so every value on a coarser declared grid is also on it. Pairing each float with the
    /// grid it was quantised onto is what lets the scan ask the one question that has an
    /// answer: is this float on the grid this writer put it on?
    ///
    /// The arms mirror [`SampleValue::quantised`] one for one and must be changed together;
    /// `graded_floats_are_the_floats_the_quantiser_wrote` holds them to it.
    #[must_use]
    pub fn graded_floats(&self, q: Quantum) -> Vec<(f64, Quantum)> {
        match self {
            SampleValue::Scalar(Estimate::Value { point, .. }) => vec![(*point, q)],
            SampleValue::Scalar(Estimate::Insufficient { .. }) => Vec::new(),
            SampleValue::Ratio(RatioEstimate::Proportion {
                point,
                ci_lo,
                ci_hi,
                ..
            }) => vec![
                (*point, q),
                (*ci_lo, Quantum::PROBABILITY),
                (*ci_hi, Quantum::PROBABILITY),
            ],
            SampleValue::Ratio(RatioEstimate::RatioOfSums {
                point,
                numerator,
                denominator,
                ..
            }) => vec![
                (*point, q),
                (*numerator, Quantum::SUM),
                (*denominator, Quantum::SUM),
            ],
            SampleValue::Ratio(RatioEstimate::Insufficient { .. }) => Vec::new(),
            SampleValue::Distribution(DistributionSummary::Summary {
                min,
                max,
                mean,
                p50,
                p95,
                p99,
                ..
            }) => vec![
                (*min, q),
                (*max, q),
                (*mean, q),
                (*p50, q),
                (*p95, q),
                (*p99, q),
            ],
            SampleValue::Distribution(DistributionSummary::Insufficient { .. }) => Vec::new(),
            SampleValue::Count { .. } => Vec::new(),
        }
    }

    /// Every float in the value, for the digest.
    ///
    /// The digest converts each of these with the sample's own quantum
    /// ([`crate::summary::DigestSet`]); the D9 scan needs the per-float grids instead and
    /// uses [`SampleValue::graded_floats`]. The two lists are the same values in the same
    /// order.
    #[must_use]
    pub fn floats(&self) -> Vec<f64> {
        match self {
            SampleValue::Scalar(Estimate::Value { point, .. }) => vec![*point],
            SampleValue::Scalar(Estimate::Insufficient { .. }) => Vec::new(),
            SampleValue::Ratio(RatioEstimate::Proportion {
                point,
                ci_lo,
                ci_hi,
                ..
            }) => vec![*point, *ci_lo, *ci_hi],
            SampleValue::Ratio(RatioEstimate::RatioOfSums {
                point,
                numerator,
                denominator,
                ..
            }) => vec![*point, *numerator, *denominator],
            SampleValue::Ratio(RatioEstimate::Insufficient { .. }) => Vec::new(),
            SampleValue::Distribution(DistributionSummary::Summary {
                min,
                max,
                mean,
                p50,
                p95,
                p99,
                ..
            }) => vec![*min, *max, *mean, *p50, *p95, *p99],
            SampleValue::Distribution(DistributionSummary::Insufficient { .. }) => Vec::new(),
            SampleValue::Count { .. } => Vec::new(),
        }
    }
}

/// One measurement of one metric at one instant, on the `metric.sample` channel
/// (03-interfaces.md §14: "t, name, dims, value", visibility `derived`).
///
/// A sample can only be built through [`MetricSample::new`], which **quantises its value
/// onto the metric's declared grid**. That is where build decision D9 is enforced for this
/// crate: there is no constructor that takes a raw float and no public field to set one
/// through, so a value cannot reach the Arrow writer, the run summary or a digest without
/// having passed the quantiser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSample {
    /// The instant this sample describes (the end of its window).
    pub t: SimTime,
    /// The metric's name, matching a [`MetricDef::name`].
    pub metric: String,
    /// The unit, copied from the definition so a reader of one sample needs nothing else.
    pub unit: String,
    /// The dimension values, in a fixed order.
    pub dims: Dims,
    /// The value, already on its grid.
    pub value: SampleValue,
    /// The grid the value sits on.
    pub quantum: Quantum,
    /// The metric's visibility, copied from the definition so the recorder's rule can be
    /// applied to the sample alone.
    #[serde(with = "crate::vis")]
    pub visibility: Visibility,
    /// True if this is a machine-dependent runtime diagnostic. Copied from the definition
    /// at construction, so the flag cannot be lost between the definition and the writer.
    pub diagnostic: bool,
    /// The aggregation's tag, for the exported table's `agg` column.
    pub agg: String,
}

impl MetricSample {
    /// Builds a sample of `def` at `t`, quantising the value onto the metric's grid.
    ///
    /// This is the only way to make a `MetricSample`. The fields are public so a reader can
    /// destructure one, but a value that reaches them has been through the quantiser.
    #[must_use]
    pub fn new(def: &MetricDef, t: SimTime, dims: Dims, value: SampleValue) -> Self {
        Self {
            t,
            metric: def.name.clone(),
            unit: def.unit.clone(),
            dims,
            value: value.quantised(def.quantum),
            quantum: def.quantum,
            visibility: def.visibility,
            diagnostic: def.diagnostic,
            agg: def.agg.tag(),
        }
    }

    /// The sample's stable key: its metric name and its dimension values, joined in the
    /// map's (fixed) order.
    ///
    /// Two samples of the same metric in the same bin have the same key, which is what the
    /// run summary and the digest index on.
    #[must_use]
    pub fn key(&self) -> String {
        let mut k = self.metric.clone();
        for (d, v) in &self.dims {
            k.push('|');
            k.push_str(&d.to_string());
            k.push('=');
            k.push_str(&v.to_string());
        }
        k
    }

    /// Every float the sample carries, for the digest and for a caller that only wants the
    /// numbers.
    #[must_use]
    pub fn floats(&self) -> Vec<f64> {
        self.value.floats()
    }

    /// Every float the sample carries, paired with the grid the writer put it on — what
    /// [`crate::invariants::check_d9_quantisation`] scans.
    ///
    /// See [`SampleValue::graded_floats`] for why the pairing is necessary and why a scan
    /// against the sample's own quantum alone is not the check D9 asks for.
    #[must_use]
    pub fn graded_floats(&self) -> Vec<(f64, Quantum)> {
        self.value.graded_floats(self.quantum)
    }
}

impl Record for MetricSample {
    const CHANNEL: &'static str = "metric.sample";
    const VISIBILITY: Visibility = Visibility::Derived;

    /// A metric derived from a ground-truth channel is itself ground truth
    /// (08-measurement-and-data.md §1: "Visibility tags propagate"). The channel's default
    /// tag is `derived`; a sample whose definition is `GT` reports `GT`, so the recorder's
    /// `allowed_on_node_channel` rule sees the truth about it.
    fn visibility(&self) -> Visibility {
        self.visibility
    }
}

/// The confidence level every provider in this crate uses unless told otherwise:
/// 95 %, matching `replications_policy: {ci: 0.95}` in 08-measurement-and-data.md §4.
pub const DEFAULT_LEVEL: ConfidenceLevel = ConfidenceLevel::P95;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{Distribution, Proportion};

    fn a_def() -> MetricDef {
        MetricDef::new(
            "pdr",
            "ratio",
            Agg::ratio("received frames", "candidate receptions"),
            Visibility::NodeAndGt,
            Quantum::RATIO,
            "received / candidate",
        )
        .with_dims([Dim::T, Dim::DistBin])
        .not_accounting_for("receivers outside the candidate range")
    }

    #[test]
    fn a_definition_validates() {
        a_def().validate().unwrap();
    }

    #[test]
    fn a_definition_without_a_stated_omission_is_refused() {
        let mut d = a_def();
        d.not_accounted.clear();
        assert!(d.validate().is_err());
    }

    #[test]
    fn a_duplicated_dimension_is_refused() {
        let d = a_def().with_dims([Dim::T, Dim::T]);
        assert!(d.validate().is_err());
    }

    #[test]
    fn a_ratio_without_both_terms_is_refused() {
        let mut d = a_def();
        d.agg = Agg::ratio("x", "  ");
        assert!(d.validate().is_err());
    }

    #[test]
    fn a_quantile_outside_zero_to_one_is_refused() {
        let mut d = a_def();
        d.agg = Agg::Quantile { q: 1.5 };
        assert!(d.validate().is_err());
        d.agg = Agg::Quantile { q: f64::NAN };
        assert!(d.validate().is_err());
        d.agg = Agg::Quantile { q: 0.95 };
        assert_eq!(d.agg.tag(), "p95");
        assert!(d.validate().is_ok());
    }

    #[test]
    fn a_sample_is_quantised_at_construction() {
        let def = a_def();
        let value = SampleValue::Ratio(Proportion::from_counts(1, 3).estimate(1, DEFAULT_LEVEL));
        let s = MetricSample::new(&def, 1_000, Dims::new(), value);
        assert_eq!(s.value.point(), Some(0.3333));
        // Each float against the grid it was put on, not against "either of two grids":
        // 1e-6 is the finest grid in the crate, so `RATIO.holds(f) || PROBABILITY.holds(f)`
        // is just `PROBABILITY.holds(f)` and says nothing about the declared grid.
        for (f, q) in s.graded_floats() {
            assert!(q.holds(f), "{f} off the grid {q:?}");
        }
        assert_eq!(s.graded_floats()[0].1, Quantum::RATIO, "the point estimate");
        assert!(!Quantum::RATIO.holds(s.graded_floats()[1].0), "the bound");
        assert_eq!(s.metric, "pdr");
        assert_eq!(s.agg, "ratio");
        assert!(!s.diagnostic);
    }

    /// `graded_floats` and `floats` must stay the same values in the same order — the digest
    /// reads one and the D9 scan reads the other, and a float that appeared in only one of
    /// them would be either undigested or unscanned.
    #[test]
    fn graded_floats_are_the_floats_the_quantiser_wrote() {
        let mut d = Distribution::new();
        d.observe_all([1.0, 2.0, 3.0]);
        let values = [
            SampleValue::Scalar(Estimate::Value { point: 0.25, n: 4 }),
            SampleValue::Scalar(Estimate::Insufficient { n: 1, required: 30 }),
            SampleValue::Ratio(Proportion::from_counts(1, 3).estimate(1, DEFAULT_LEVEL)),
            SampleValue::Ratio(crate::stats::ratio_of_sums(1.0, 3.0, 4, 1)),
            SampleValue::Ratio(crate::stats::ratio_of_sums(1.0, 0.0, 4, 1)),
            SampleValue::Distribution(d.summary(1)),
            SampleValue::Distribution(d.summary(30)),
            SampleValue::count(7),
        ];
        for v in values {
            let s = MetricSample::new(&a_def(), 0, Dims::new(), v);
            let graded: Vec<f64> = s.graded_floats().into_iter().map(|(f, _)| f).collect();
            assert_eq!(graded, s.floats(), "{:?}", s.value);
            // Every float really is on the grid the pairing claims, which is what makes the
            // scan a check rather than a restatement of the writer.
            for (f, q) in s.graded_floats() {
                assert!(q.holds(f), "{f} off {q:?} in {:?}", s.value);
            }
        }
    }

    #[test]
    fn a_sample_key_is_stable_and_ordered() {
        let def = a_def();
        let mut dims = Dims::new();
        dims.insert(Dim::DistBin, DimValue::label("25-50"));
        dims.insert(Dim::T, DimValue::index(3));
        let s = MetricSample::new(&def, 0, dims, SampleValue::count(7));
        // `Dim`'s declaration order puts `t` before `dist_bin`, whatever the insertion order.
        assert_eq!(s.key(), "pdr|t=3|dist_bin=25-50");
    }

    #[test]
    fn the_visibility_of_a_gt_derived_metric_propagates() {
        let def = a_def();
        let s = MetricSample::new(&def, 0, Dims::new(), SampleValue::count(1));
        assert_eq!(Record::visibility(&s), Visibility::NodeAndGt);
        assert!(!Record::visibility(&s).allowed_on_node_channel());
    }
}
