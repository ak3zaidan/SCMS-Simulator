//! The run-manifest summary, and the digest that machine-dependent numbers cannot enter.
//!
//! Two outputs live here:
//!
//! * [`RunSummary`] — the compact, self-describing form of a run's metrics, suitable for
//!   the run manifest (`v2xw_core::Manifest::warnings` and `files` carry the tables; this
//!   is the summary that goes beside them). It keeps every sample's value, unit, sample
//!   count and insufficiency, so a manifest is readable on its own.
//! * [`DigestSet`] — the digest form, over **integer grid indices** rather than floats,
//!   from which runtime diagnostics are excluded structurally.
//!
//! # The exclusion is structural, not a convention
//!
//! 08-measurement-and-data.md's runtime numbers are machine-dependent, so a digest that
//! included one would fail the cross-platform determinism gate for a reason that has nothing
//! to do with the simulation. The task is to enforce that in code, and this is how:
//!
//! * [`DigestSet`]'s field is **private**, and its only constructor is
//!   [`DigestSet::partition`], which returns the diagnostics it removed as a separate list.
//!   There is no way to put a diagnostic sample into a `DigestSet` — not by mistake, not on
//!   purpose.
//! * [`RunSummary`] keeps the two groups in two fields and computes its digest from the
//!   digested one only, so adding, removing or changing a diagnostic cannot move the digest.
//! * [`metric_digest`] exists for a caller that wants to be *told* rather than silently
//!   served: it refuses a list containing a diagnostic with
//!   [`crate::MetricError::DiagnosticInDigest`], naming the metric.
//! * [`RunSummary`]'s `diagnostics` field is **not serialised**, so the summary *document* —
//!   the bytes written to `metrics/summary.json`, and therefore the
//!   `v2xw_core::manifest::FileDigest` the run manifest records for that file — contains no
//!   machine-dependent number either. The diagnostics leave through one door only:
//!   [`RunSummary::diagnostics_json`], a sidecar document with its own schema and its own
//!   digest, which a manifest records separately or not at all.
//!
//! The four are checked against each other by the tests at the bottom of this module.
//!
//! The fourth used to be missing, and it mattered: `file_digest` hashes the summary's whole
//! canonical JSON, so while the diagnostics were serialised into it, two machines running
//! the same scenario disagreed on the manifest's `FileDigest` for that file — and on
//! `Manifest::finalize`'s `data_digest`, which hashes every recorded file digest. The
//! digest a manifest carries is only worth having if it is reproducible, so the exclusion
//! has to reach the document and not merely the `digest` field inside it.
//!
//! # A digest hashes integers
//!
//! Build decision D9: "A digest hashes the integer multiple of the quantum
//! (`v2xw_core::math::grid_index`), not the rounded float". Every float in a sample is
//! converted with [`crate::quant::grid`] on that sample's own declared quantum, and the
//! hash runs over the canonical JSON of those integers, the sample counts and the keys. Two
//! platforms that agree on the grid point agree on the bytes, whatever their float
//! formatting.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::hash::{canonical_json, sha256_hex};

use crate::def::{MetricSample, SampleValue};
use crate::error::{MetricError, Result};

/// The schema id of a [`RunSummary`].
pub const SUMMARY_SCHEMA: &str = "v2xw/metric-summary/1";

/// The schema id of a [`RunDiagnostics`] sidecar.
pub const DIAGNOSTICS_SCHEMA: &str = "v2xw/metric-diagnostics/1";

/// A run's machine-dependent runtime diagnostics, as the sidecar document that keeps them
/// out of the summary the manifest digests.
///
/// Written beside the summary (`metrics/diagnostics.json` beside `metrics/summary.json`) and
/// joined back to it by [`RunDiagnostics::summary_digest`], so a reader can tell which run's
/// numbers these are without them being part of that run's identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDiagnostics {
    /// The schema id, [`DIAGNOSTICS_SCHEMA`].
    pub schema: String,
    /// The [`RunSummary::digest`] of the run these diagnostics belong to. A digest rather
    /// than a path, because it is the run's identity and it is reproducible.
    pub summary_digest: String,
    /// The diagnostics, by key.
    pub diagnostics: BTreeMap<String, MetricSample>,
}

/// One sample's contribution to a digest: its key, its floats as grid integers, and its
/// counts.
///
/// Serialised in field order into the canonical JSON the digest is taken over. Nothing here
/// is a float.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DigestEntry {
    /// The sample's key: metric name and dimension values.
    key: String,
    /// The instant.
    t: u64,
    /// Every float in the value, as its integer multiple of the metric's quantum.
    grid: Vec<i64>,
    /// The sample count, and the successes where there are some.
    n: u64,
    /// True if the sample reported insufficient rather than an estimate. Part of the digest
    /// because "we refused to estimate this" is a result, and two runs that disagree about
    /// it disagree.
    insufficient: bool,
    /// An exact integer count, where the value is one.
    count: Option<u64>,
}

/// The digest form of a run's metrics, with runtime diagnostics removed.
///
/// Constructible only through [`DigestSet::partition`]. See the module documentation for why
/// that is the whole point of the type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestSet {
    entries: Vec<DigestEntry>,
}

impl DigestSet {
    /// Splits `samples` into the digested set and the diagnostics that were removed.
    ///
    /// The digested entries are sorted by `(key, t)`, so the digest does not depend on the
    /// order the providers were flushed in. The returned diagnostics keep their input order,
    /// because they are for a human to read and not for a hash.
    #[must_use]
    pub fn partition(samples: impl IntoIterator<Item = MetricSample>) -> (Self, Vec<MetricSample>) {
        let mut entries = Vec::new();
        let mut diagnostics = Vec::new();
        for s in samples {
            if s.diagnostic {
                diagnostics.push(s);
                continue;
            }
            entries.push(entry_of(&s));
        }
        entries.sort_by(|a, b| a.key.cmp(&b.key).then(a.t.cmp(&b.t)));
        (Self { entries }, diagnostics)
    }

    /// How many samples are in the digest.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if nothing is in the digest.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The sample keys in the digest, in digest order.
    #[must_use]
    pub fn keys(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.key.as_str()).collect()
    }

    /// The canonical bytes the digest is taken over — integers, counts and keys, no floats.
    ///
    /// # Errors
    /// [`MetricError::Json`] if the entries cannot be encoded, which they always can.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(&self.entries).map_err(MetricError::Core)
    }

    /// The digest: lower-case hex SHA-256 over [`DigestSet::canonical_bytes`].
    ///
    /// # Errors
    /// As [`DigestSet::canonical_bytes`].
    pub fn digest_hex(&self) -> Result<String> {
        Ok(sha256_hex(&self.canonical_bytes()?))
    }
}

/// The digest of `samples`, **refusing** a list that contains a runtime diagnostic.
///
/// [`DigestSet::partition`] quietly removes diagnostics, which is right for a caller that is
/// handing over everything a run produced. This is for a caller that believes it has already
/// filtered them and wants to be told if it has not: the error names the metric.
///
/// # Errors
/// [`MetricError::DiagnosticInDigest`] naming the first diagnostic metric found, or
/// [`MetricError::Json`] from the encoding.
pub fn metric_digest(samples: &[MetricSample]) -> Result<String> {
    if let Some(s) = samples.iter().find(|s| s.diagnostic) {
        return Err(MetricError::DiagnosticInDigest {
            name: s.metric.clone(),
        });
    }
    let (set, diagnostics) = DigestSet::partition(samples.iter().cloned());
    debug_assert!(
        diagnostics.is_empty(),
        "the check above already ruled these out"
    );
    set.digest_hex()
}

/// Builds one sample's digest entry: floats become grid integers on the sample's own
/// quantum, counts stay integers.
fn entry_of(s: &MetricSample) -> DigestEntry {
    let grid = s
        .value
        .floats()
        .into_iter()
        .map(|f| crate::quant::grid(f, s.quantum))
        .collect();
    DigestEntry {
        key: s.key(),
        t: s.t,
        grid,
        n: s.value.n(),
        insufficient: s.value.is_insufficient(),
        count: match &s.value {
            SampleValue::Count { count } => Some(*count),
            _ => None,
        },
    }
}

/// A run's metrics in the compact form a manifest carries.
///
/// The two maps are keyed by `MetricSample::key()` — metric name plus dimension values — so
/// a summary is a lookup table rather than a list to scan, and the keys are ordered.
///
/// A summary is **not** the metric table. The table is Parquet or Arrow
/// ([`crate::arrow_out`]) and holds every sample of every window; this holds the last sample
/// of each key, which is what a manifest should carry: enough to tell two runs apart and to
/// read the headline numbers without opening the tables.
///
/// # The struct carries the diagnostics; the document does not
///
/// A `RunSummary` in memory holds both groups, so a caller has everything a run produced in
/// one place. Serialising one writes the digested half only
/// ([`RunSummary::to_canonical_json`]); the diagnostics go out through
/// [`RunSummary::diagnostics_json`] as their own document. That is what makes
/// [`RunSummary::file_digest`] reproducible across machines. Deserialising a summary
/// therefore yields an empty `diagnostics` map — from a document written by this crate
/// because it never held them, and from an older one because they are no longer read back
/// into a value whose digest must not depend on them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    /// The schema id, [`SUMMARY_SCHEMA`].
    pub schema: String,
    /// The digested metrics, by key.
    pub metrics: BTreeMap<String, MetricSample>,
    /// The runtime diagnostics, by key. **Not** digested and **not serialised**: changing
    /// one of these can move neither [`RunSummary::digest`] nor
    /// [`RunSummary::file_digest`], because it is in neither the digest nor the document.
    /// They are written through [`RunSummary::diagnostics_json`] instead.
    ///
    /// `skip` rather than a convention, for the reason the module documentation gives: a
    /// machine-dependent number that any code path can serialise into the summary is a
    /// machine-dependent file digest waiting to happen.
    #[serde(skip)]
    pub diagnostics: BTreeMap<String, MetricSample>,
    /// The digest over [`RunSummary::metrics`] alone.
    pub digest: String,
    /// How many of the digested metrics reported insufficient rather than an estimate.
    ///
    /// A headline number worth having in a manifest: a run whose metrics are mostly
    /// insufficient did not measure what it was asked to, and no reader should have to
    /// discover that by opening every table.
    pub insufficient: u64,
    /// How many records the providers could not decode, if the caller supplied the figure.
    pub rejected_records: u64,
}

impl RunSummary {
    /// Builds a summary from every sample a run produced.
    ///
    /// Where a key appears more than once — one sample per window, which is the normal case
    /// — the **latest** sample wins, ties by input order. That is what makes the summary a
    /// snapshot of the run's end rather than an arbitrary window.
    ///
    /// # Errors
    /// [`MetricError::Json`] from the digest's encoding.
    pub fn new(samples: impl IntoIterator<Item = MetricSample>) -> Result<Self> {
        let mut metrics: BTreeMap<String, MetricSample> = BTreeMap::new();
        let mut diagnostics: BTreeMap<String, MetricSample> = BTreeMap::new();
        for s in samples {
            let key = s.key();
            let target = if s.diagnostic {
                &mut diagnostics
            } else {
                &mut metrics
            };
            match target.get(&key) {
                Some(existing) if existing.t > s.t => {}
                _ => {
                    target.insert(key, s);
                }
            }
        }
        // The digest is taken over the digested map only. `partition` would remove the
        // diagnostics anyway; passing only `metrics` makes the independence visible at the
        // call site as well as guaranteed by the type.
        let (set, removed) = DigestSet::partition(metrics.values().cloned());
        debug_assert!(removed.is_empty());
        let insufficient = metrics
            .values()
            .filter(|s| s.value.is_insufficient())
            .count() as u64;
        Ok(Self {
            schema: SUMMARY_SCHEMA.to_string(),
            digest: set.digest_hex()?,
            metrics,
            diagnostics,
            insufficient,
            rejected_records: 0,
        })
    }

    /// Records how many events the providers could not decode.
    #[must_use]
    pub const fn with_rejected(mut self, n: u64) -> Self {
        self.rejected_records = n;
        self
    }

    /// The summary's canonical JSON — the bytes to write at `metrics/summary.json`.
    ///
    /// The document holds the digested metrics and nothing machine-dependent: the runtime
    /// diagnostics are not serialised (see [`RunSummary::diagnostics_json`]), so two
    /// machines running the same scenario produce the same bytes.
    ///
    /// # Errors
    /// [`MetricError::Json`] from the encoding.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>> {
        canonical_json(self).map_err(MetricError::Core)
    }

    /// A `FileDigest` for the summary written at `path`, for
    /// `v2xw_core::Manifest::files`.
    ///
    /// SHA-256 over exactly the bytes [`RunSummary::to_canonical_json`] produces, which is
    /// what `FileDigest` promises ("hex SHA-256 of the file's bytes"), and **reproducible
    /// across machines**: nothing wall-clock or memory-shaped is in those bytes. A manifest
    /// records this digest, and `Manifest::finalize` hashes it into the run's `data_digest`,
    /// so a non-reproducible value here would fail the cross-platform determinism gate for a
    /// reason that has nothing to do with the simulation.
    ///
    /// # Errors
    /// [`MetricError::Json`] from the encoding.
    pub fn file_digest(&self, path: impl Into<String>) -> Result<v2xw_core::manifest::FileDigest> {
        Ok(v2xw_core::manifest::FileDigest {
            path: path.into(),
            sha256: sha256_hex(&self.to_canonical_json()?),
        })
    }

    /// The runtime diagnostics as their own document, keyed back to the run by its metric
    /// digest.
    ///
    /// These are the machine-dependent numbers — wall clock, rates derived from it, memory
    /// high-water mark — and they are worth keeping: a run that took an hour on one machine
    /// and a minute on another is telling a reader something. They are simply not part of
    /// the run's identity, so they live beside the summary rather than in it.
    #[must_use]
    pub fn diagnostics(&self) -> RunDiagnostics {
        RunDiagnostics {
            schema: DIAGNOSTICS_SCHEMA.to_string(),
            summary_digest: self.digest.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }

    /// The diagnostics sidecar's canonical JSON — the bytes to write at
    /// `metrics/diagnostics.json`.
    ///
    /// # Errors
    /// [`MetricError::Json`] from the encoding.
    pub fn diagnostics_json(&self) -> Result<Vec<u8>> {
        canonical_json(&self.diagnostics()).map_err(MetricError::Core)
    }

    /// A `FileDigest` for the diagnostics sidecar written at `path`.
    ///
    /// **This digest is machine-dependent by construction and must not be compared across
    /// machines, or added to a manifest whose `data_digest` is compared across machines.**
    /// It exists so that a caller writing the sidecar can record its integrity somewhere
    /// that is honest about what it is; that is the whole difference between it and
    /// [`RunSummary::file_digest`].
    ///
    /// # Errors
    /// [`MetricError::Json`] from the encoding.
    pub fn diagnostics_file_digest(
        &self,
        path: impl Into<String>,
    ) -> Result<v2xw_core::manifest::FileDigest> {
        Ok(v2xw_core::manifest::FileDigest {
            path: path.into(),
            sha256: sha256_hex(&self.diagnostics_json()?),
        })
    }

    /// The point estimate of one metric by key, when it has one.
    #[must_use]
    pub fn point(&self, key: &str) -> Option<f64> {
        self.metrics.get(key).and_then(|s| s.value.point())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::def::{Agg, Dims, MetricDef};
    use crate::quant::Quantum;
    use crate::stats::{ConfidenceLevel, Estimate, Proportion};
    use v2xw_core::ctx::Visibility;

    fn def(name: &str, diagnostic: bool) -> MetricDef {
        let d = MetricDef::new(
            name,
            "ratio",
            Agg::Mean,
            Visibility::Node,
            Quantum::RATIO,
            "A test metric.",
        )
        .not_accounting_for("being a real metric");
        if diagnostic { d.as_diagnostic() } else { d }
    }

    fn sample(name: &str, diagnostic: bool, point: f64, t: u64) -> MetricSample {
        MetricSample::new(
            &def(name, diagnostic),
            t,
            Dims::new(),
            SampleValue::Scalar(Estimate::Value { point, n: 10 }),
        )
    }

    #[test]
    fn a_runtime_diagnostic_cannot_enter_a_digest() {
        let samples = vec![
            sample("pdr", false, 0.9, 1_000),
            sample("events_per_second", true, 123_456.0, 1_000),
        ];
        let (set, removed) = DigestSet::partition(samples.clone());
        assert_eq!(set.len(), 1);
        assert_eq!(set.keys(), vec!["pdr"]);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].metric, "events_per_second");

        // And the explicit check refuses the list by name rather than filtering it.
        let e = metric_digest(&samples).unwrap_err();
        assert!(
            matches!(e, MetricError::DiagnosticInDigest { ref name } if name == "events_per_second"),
            "{e}"
        );
    }

    /// The property that matters: the diagnostics' *values* cannot move the digest.
    #[test]
    fn changing_a_diagnostic_does_not_move_the_digest() {
        let base = vec![
            sample("pdr", false, 0.9, 1_000),
            sample("events_per_second", true, 1.0, 1_000),
        ];
        let other = vec![
            sample("pdr", false, 0.9, 1_000),
            sample("events_per_second", true, 999_999.0, 1_000),
            sample("memory_high_water_mark", true, 8e9, 1_000),
        ];
        let a = RunSummary::new(base).unwrap();
        let b = RunSummary::new(other).unwrap();
        assert_eq!(a.digest, b.digest);
        assert_eq!(a.diagnostics.len(), 1);
        assert_eq!(b.diagnostics.len(), 2);
        // …while a change to a real metric does move it.
        let c = RunSummary::new(vec![sample("pdr", false, 0.8, 1_000)]).unwrap();
        assert_ne!(a.digest, c.digest);
    }

    #[test]
    fn the_digest_does_not_depend_on_the_order_of_the_samples() {
        let mut samples = vec![
            sample("a", false, 0.1, 1_000),
            sample("b", false, 0.2, 1_000),
            sample("c", false, 0.3, 2_000),
        ];
        let forward = metric_digest(&samples).unwrap();
        samples.reverse();
        assert_eq!(forward, metric_digest(&samples).unwrap());
        samples.rotate_left(1);
        assert_eq!(forward, metric_digest(&samples).unwrap());
    }

    /// The digest is over grid integers, so two values that quantise to the same grid point
    /// digest identically — which is the cross-platform property D9 asks for.
    #[test]
    fn two_values_on_one_grid_point_digest_identically() {
        let a = metric_digest(&[sample("pdr", false, 0.333_3, 0)]).unwrap();
        let b = metric_digest(&[sample("pdr", false, 0.333_300_000_000_000_1, 0)]).unwrap();
        assert_eq!(a, b);
        // And a genuinely different grid point does not.
        let c = metric_digest(&[sample("pdr", false, 0.333_4, 0)]).unwrap();
        assert_ne!(a, c);
    }

    /// An insufficient sample is part of the digest: "we refused to estimate this" is a
    /// result, and two runs that disagree about it ran differently.
    #[test]
    fn insufficiency_is_part_of_the_digest() {
        let d = def("pdr", false);
        let estimated = MetricSample::new(
            &d,
            0,
            Dims::new(),
            SampleValue::Ratio(Proportion::from_counts(1, 1).estimate(1, ConfidenceLevel::P95)),
        );
        let refused = MetricSample::new(
            &d,
            0,
            Dims::new(),
            SampleValue::Ratio(Proportion::from_counts(1, 1).estimate(30, ConfidenceLevel::P95)),
        );
        assert_ne!(
            metric_digest(&[estimated]).unwrap(),
            metric_digest(&[refused]).unwrap()
        );
    }

    #[test]
    fn the_summary_keeps_the_latest_sample_of_each_key() {
        let s = RunSummary::new(vec![
            sample("pdr", false, 0.5, 1_000),
            sample("pdr", false, 0.9, 5_000),
            sample("pdr", false, 0.1, 2_000),
        ])
        .unwrap();
        assert_eq!(s.metrics.len(), 1);
        assert_eq!(s.point("pdr"), Some(0.9));
    }

    #[test]
    fn the_summary_counts_its_insufficient_metrics() {
        let d = def("pdr", false);
        let refused = MetricSample::new(
            &d,
            0,
            Dims::new(),
            SampleValue::Scalar(Estimate::Insufficient { n: 2, required: 30 }),
        );
        let s = RunSummary::new(vec![refused, sample("per", false, 0.1, 0)]).unwrap();
        assert_eq!(s.insufficient, 1);
        assert_eq!(s.point("pdr"), None);
    }

    #[test]
    fn an_empty_summary_has_a_digest_and_no_metrics() {
        let s = RunSummary::new(Vec::new()).unwrap();
        assert!(s.metrics.is_empty());
        assert_eq!(s.insufficient, 0);
        assert_eq!(s.digest.len(), 64);
        // The digest of no metrics is the SHA-256 of the empty JSON array, a stable value.
        assert_eq!(s.digest, sha256_hex(b"[]"));
    }

    /// A sample with dimension values round-trips too: the dimension map's keys are an
    /// enum, and an enum used as a JSON object key is a place a schema quietly stops being
    /// readable.
    #[test]
    fn a_summary_with_dimensions_and_every_value_shape_round_trips() {
        use crate::def::{Dim, DimValue};
        use crate::stats::{Distribution, ratio_of_sums};
        let mut dims = Dims::new();
        dims.insert(Dim::DistBin, DimValue::label("25-50"));
        dims.insert(Dim::Node, DimValue::index(3));
        let mut d = Distribution::new();
        d.observe_all([1.0, 2.0, 3.0]);
        let samples = vec![
            MetricSample::new(
                &def("pdr", false),
                1,
                dims.clone(),
                SampleValue::Ratio(Proportion::from_counts(3, 4).estimate(1, ConfidenceLevel::P95)),
            ),
            MetricSample::new(
                &def("goodput", false),
                1,
                dims.clone(),
                SampleValue::Ratio(ratio_of_sums(2.0, 4.0, 2, 1)),
            ),
            MetricSample::new(
                &def("latency", false),
                1,
                dims.clone(),
                SampleValue::Distribution(d.summary(1)),
            ),
            MetricSample::new(&def("hits", false), 1, dims, SampleValue::count(7)),
        ];
        let s = RunSummary::new(samples).unwrap();
        assert_eq!(s.metrics.len(), 4);
        let bytes = s.to_canonical_json().unwrap();
        let back: RunSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, s);
        assert!(
            back.metrics.contains_key("pdr|node=3|dist_bin=25-50"),
            "{:?}",
            back.metrics.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_summary_round_trips_through_canonical_json() {
        let s = RunSummary::new(vec![
            sample("pdr", false, 0.9, 1_000),
            sample("events_per_second", true, 42.0, 1_000),
        ])
        .unwrap()
        .with_rejected(3);
        let bytes = s.to_canonical_json().unwrap();
        let back: RunSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.schema, SUMMARY_SCHEMA);
        assert_eq!(back.rejected_records, 3);
        assert_eq!(back.metrics, s.metrics);
        assert_eq!(back.digest, s.digest);
        // The document carries the digested half. The one diagnostic is in the struct and
        // not in the bytes, so the value that comes back is the document's own.
        assert_eq!(s.diagnostics.len(), 1);
        assert!(back.diagnostics.is_empty());
        assert_eq!(
            back,
            RunSummary {
                diagnostics: BTreeMap::new(),
                ..s.clone()
            }
        );
        // …and the file digest is over exactly those bytes.
        let fd = s.file_digest("metrics/summary.json").unwrap();
        assert_eq!(fd.path, "metrics/summary.json");
        assert_eq!(fd.sha256, sha256_hex(&bytes));
    }

    /// F5: a runtime diagnostic reached a digested artefact through `file_digest`.
    ///
    /// `RunSummary::digest` was already independent of the diagnostics, and stayed so — but
    /// `file_digest` hashes the whole canonical document, which serialised them, so the
    /// digest a run manifest records for `metrics/summary.json` moved when a wall-clock
    /// number moved. Two machines running the same scenario disagreed on it, and on the
    /// `data_digest` that `Manifest::finalize` derives from every recorded file digest.
    ///
    /// The verifier's case, reproduced: two summaries identical but for
    /// `events_per_second` = 1.0 against 987654.0.
    #[test]
    fn changing_a_diagnostic_moves_neither_the_digest_nor_the_file_digest() {
        let fast = RunSummary::new(vec![
            sample("pdr", false, 0.9, 1_000),
            sample("events_per_second", true, 1.0, 1_000),
        ])
        .unwrap();
        let slow = RunSummary::new(vec![
            sample("pdr", false, 0.9, 1_000),
            sample("events_per_second", true, 987_654.0, 1_000),
            sample("memory_high_water_mark", true, 8e9, 1_000),
        ])
        .unwrap();
        assert_eq!(fast.digest, slow.digest, "the protected digest");
        assert_eq!(
            fast.to_canonical_json().unwrap(),
            slow.to_canonical_json().unwrap(),
            "the document itself must not carry a machine-dependent number"
        );
        assert_eq!(
            fast.file_digest("metrics/summary.json").unwrap(),
            slow.file_digest("metrics/summary.json").unwrap(),
            "the digest a run manifest records for the summary file"
        );
        // The diagnostics are still there to be read and written, just not in that document.
        assert_eq!(fast.diagnostics.len(), 1);
        assert_eq!(slow.diagnostics.len(), 2);
        assert_ne!(
            fast.diagnostics_json().unwrap(),
            slow.diagnostics_json().unwrap()
        );
        // …and a change to a real metric still moves both.
        let other = RunSummary::new(vec![sample("pdr", false, 0.8, 1_000)]).unwrap();
        assert_ne!(fast.digest, other.digest);
        assert_ne!(
            fast.file_digest("metrics/summary.json").unwrap().sha256,
            other.file_digest("metrics/summary.json").unwrap().sha256
        );
    }

    /// The exclusion is structural: there is no serialisation of a `RunSummary` that carries
    /// a diagnostic, not only the canonical one.
    #[test]
    fn no_serialisation_of_a_summary_carries_a_diagnostic() {
        let s = RunSummary::new(vec![
            sample("pdr", false, 0.9, 1_000),
            sample("events_per_second", true, 987_654.0, 1_000),
        ])
        .unwrap();
        for bytes in [
            s.to_canonical_json().unwrap(),
            serde_json::to_vec(&s).unwrap(),
            serde_json::to_vec_pretty(&s).unwrap(),
        ] {
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("events_per_second"), "{text}");
            assert!(!text.contains("987654"), "{text}");
        }
    }

    /// The diagnostics still leave the process — through their own document, which names the
    /// run it belongs to by the one identifier that is reproducible.
    #[test]
    fn the_diagnostics_sidecar_round_trips_and_names_its_run() {
        let s = RunSummary::new(vec![
            sample("pdr", false, 0.9, 1_000),
            sample("events_per_second", true, 987_654.0, 1_000),
        ])
        .unwrap();
        let bytes = s.diagnostics_json().unwrap();
        let back: RunDiagnostics = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, s.diagnostics());
        assert_eq!(back.schema, DIAGNOSTICS_SCHEMA);
        assert_eq!(back.summary_digest, s.digest);
        assert_eq!(back.diagnostics.len(), 1);
        assert_eq!(
            back.diagnostics["events_per_second"].value.point(),
            Some(987_654.0)
        );
        let fd = s
            .diagnostics_file_digest("metrics/diagnostics.json")
            .unwrap();
        assert_eq!(fd.path, "metrics/diagnostics.json");
        assert_eq!(fd.sha256, sha256_hex(&bytes));
    }
}
