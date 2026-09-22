//! `v2xw.run` — building an engine from a scenario, running it, and collecting what it
//! produced.
//!
//! # The recorder is a tee, and that is the whole design
//!
//! `v2xw_engine::Engine::run` writes every record into one [`RunRecorder`]. A researcher
//! wants two things out of a run at once: a recording on disk to go back to, and metric
//! samples to look at now. [`TeeRecorder`] is that fork. It writes each record to the MCAP
//! writer and hands the same record to the metric providers, so the numbers and the file
//! are computed from *one* stream and cannot disagree.
//!
//! It also windows. Metric providers accumulate and emit on `flush(at)`, so a run that
//! flushed only at the end would produce one sample per metric and no time series at all.
//! The tee flushes on the window boundaries the record times cross, which is what makes
//! `metrics.series("pdr")` a curve rather than a point.
//!
//! # No wall clock, on this side of the boundary either
//!
//! `Engine::build` takes the manifest timestamp as an argument because nothing in the
//! engine may read a clock (02-architecture.md §6.1). These bindings are engine-facing
//! code, so the rule reaches them: [`run`] takes `build_utc` as a **required** keyword
//! argument and never fills it in. The Python wrapper defaults it from
//! `datetime.now(timezone.utc)` — in Python, in the caller's process, visibly — which is
//! exactly the "the caller's timestamp" the engine asks for.
//!
//! # Where the metric providers are registered, and why it is not the engine's registry
//!
//! `ProviderSet::register` needs `&mut Registry` and `Engine` owns its registry privately,
//! so the providers here are registered in a registry of their own. The consequence is
//! real and worth stating rather than hiding: **the metric providers this function runs do
//! not appear in the run manifest's plug-in list.** The manifest pins the models the engine
//! registered. Closing that gap needs a seam on `Engine` (a `metrics: &mut ProviderSet`
//! argument to `build`, or a `registry_mut`), which belongs to `v2xw-engine` and is noted
//! in this crate's README as an owed change.

use std::fs::File;
use std::io::BufWriter;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use v2xw_core::ctx::OwnedRecord;
use v2xw_core::manifest::Manifest;
use v2xw_core::time::SimTime;
use v2xw_engine::ctx::RunRecorder;
use v2xw_engine::{Engine, RunReport};
use v2xw_metrics::{MetricSample, ProviderSet};
use v2xw_record::{RecordingOptions, RecordingWriter};

use crate::err;
use crate::metrics::PyMetrics;
use crate::scenario::{PyScenario, to_py_dict};

/// Writes each record to a recording, to the metric providers, or to both.
///
/// A record that the recording refuses — an undeclared channel, a ground-truth record on a
/// NODE channel, a float off its declared grid — is counted, not raised: the engine is
/// mid-phase and has no useful answer, which is the reason `RunRecorder::write` is
/// infallible in the first place. [`PyRun::records_refused`] is where the count surfaces,
/// and a non-zero value there means the recording is not the whole run.
pub struct TeeRecorder<'a> {
    writer: Option<&'a mut RecordingWriter<BufWriter<File>>>,
    providers: Option<&'a mut ProviderSet>,
    samples: Vec<MetricSample>,
    window_ns: u64,
    next_window: SimTime,
    refused: u64,
    written: u64,
}

impl<'a> TeeRecorder<'a> {
    /// A tee over an optional recording and an optional provider set.
    ///
    /// `window_ns` of zero disables windowing: the providers then flush once, at the end.
    pub fn new(
        writer: Option<&'a mut RecordingWriter<BufWriter<File>>>,
        providers: Option<&'a mut ProviderSet>,
        window_ns: u64,
    ) -> Self {
        Self {
            writer,
            providers,
            samples: Vec::new(),
            window_ns,
            next_window: window_ns,
            refused: 0,
            written: 0,
        }
    }

    /// Flushes every window that ends at or before `at`.
    ///
    /// A loop rather than a single step because a run can be silent for several windows —
    /// a gap in the records must still produce the windows it spans, or the series has a
    /// hole where it should have a zero-sample window.
    fn advance_windows(&mut self, at: SimTime) {
        if self.window_ns == 0 {
            return;
        }
        while at >= self.next_window {
            if let Some(p) = self.providers.as_deref_mut() {
                self.samples.extend(p.flush(self.next_window));
            }
            self.next_window = self.next_window.saturating_add(self.window_ns);
        }
    }

    /// Flushes the final, partial window and returns every sample collected.
    pub fn finish(mut self, end: SimTime) -> Vec<MetricSample> {
        if let Some(p) = self.providers.as_deref_mut() {
            self.samples.extend(p.flush(end));
        }
        self.samples
    }

    /// How many records the recording refused.
    pub fn refused_count(&self) -> u64 {
        self.refused
    }

    /// How many records passed through.
    pub fn written_count(&self) -> u64 {
        self.written
    }
}

impl RunRecorder for TeeRecorder<'_> {
    fn write(&mut self, at: SimTime, record: &OwnedRecord) {
        self.advance_windows(at);
        self.written += 1;
        if let Some(w) = self.writer.as_deref_mut()
            && w.write_record(at, record).is_err()
        {
            self.refused += 1;
        }
        if let Some(p) = self.providers.as_deref_mut() {
            p.on_event(record);
        }
    }

    fn refused(&self) -> u64 {
        self.refused
    }
}

/// What one run produced: its manifest, its counters, its metrics and its recording.
#[pyclass(name = "Run", module = "v2xw", frozen)]
pub struct PyRun {
    report: RunReport,
    manifest: Manifest,
    recording: Option<String>,
    metrics: Py<PyMetrics>,
    records_refused: u64,
}

#[pymethods]
impl PyRun {
    /// The run manifest: scenario hash, seed, platform, crate versions, model cards.
    #[getter]
    fn manifest<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        to_py_dict(py, "manifest", &self.manifest)
    }

    /// The run's counters: events by class, actors, nodes, frames, records.
    #[getter]
    fn report<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        to_py_dict(py, "run report", &self.report)
    }

    /// The metric samples.
    #[getter]
    fn metrics(&self, py: Python<'_>) -> Py<PyMetrics> {
        self.metrics.clone_ref(py)
    }

    /// The recording written, if one was asked for.
    #[getter]
    fn recording(&self) -> Option<&str> {
        self.recording.as_deref()
    }

    /// The instant the loop stopped at, nanoseconds.
    #[getter]
    fn end_ns(&self) -> u64 {
        self.report.end_ns
    }

    /// How many records were emitted.
    #[getter]
    fn records(&self) -> u64 {
        self.report.records
    }

    /// How many records the recording refused — non-zero means it is not the whole run.
    #[getter]
    fn records_refused(&self) -> u64 {
        self.records_refused
    }

    /// The scenario hash this run is identified by.
    #[getter]
    fn scenario_hash(&self) -> &str {
        &self.manifest.scenario_hash
    }

    fn __repr__(&self) -> String {
        format!(
            "<v2xw.Run scenario={} end_s={:.3} records={} frames={}>",
            &self.manifest.scenario_hash[..12.min(self.manifest.scenario_hash.len())],
            self.report.end_ns as f64 / 1e9,
            self.report.records,
            self.report.frames_transmitted
        )
    }
}

/// Runs a scenario.
///
/// `build_utc` is the caller's timestamp for the manifest; see the module note on why it
/// is required rather than read here. `recording` is where to write the MCAP file, if
/// anywhere. `metric_window_s` is the aggregation window; `0` collapses the run to one
/// sample per metric.
///
/// # Errors
/// `ScenarioError` if the scenario will not build, `RecordingError` if the recording
/// cannot be written, `MetricError` if a provider refuses a definition, `V2xwError` for a
/// phase failure.
#[pyfunction]
#[pyo3(signature = (scenario, *, build_utc, recording=None, metrics=true, metric_window_s=1.0))]
pub fn run(
    py: Python<'_>,
    scenario: &PyScenario,
    build_utc: &str,
    recording: Option<&str>,
    metrics: bool,
    metric_window_s: f64,
) -> PyResult<PyRun> {
    if !(metric_window_s.is_finite() && metric_window_s >= 0.0) {
        return Err(err::V2xwError::new_err(format!(
            "metric_window_s must be a finite, non-negative number of seconds, got {metric_window_s}"
        )));
    }
    let window_ns = (metric_window_s * 1e9).round() as u64;

    let mut engine = Engine::build(scenario.scenario().clone(), build_utc).map_err(err::engine)?;
    let manifest = engine.manifest().clone();
    let manifest_json = serde_json::to_string(&manifest).map_err(|e| err::json("manifest", e))?;

    let mut writer = match recording {
        Some(path) => {
            let opts = RecordingOptions::default();
            Some(RecordingWriter::create(path, opts).map_err(err::record)?)
        }
        None => None,
    };

    let mut registry = v2xw_core::registry::Registry::new();
    let mut providers = ProviderSet::new();
    if metrics {
        v2xw_metrics::register_all(&mut registry, &mut providers, 0).map_err(err::metric)?;
    }

    let mut tee = TeeRecorder::new(
        writer.as_mut(),
        if metrics { Some(&mut providers) } else { None },
        window_ns,
    );

    // The interpreter lock is **held** across the run, and that is a real limitation
    // rather than an oversight. Releasing it would let other Python threads work while the
    // simulation runs, but `Engine` is not `Send` — it owns `Box<dyn Mobility>` and a
    // dozen other family trait objects that the design does not require to be — so it
    // cannot cross the closure `Python::allow_threads` needs. Making the engine `Send` is a
    // `v2xw-engine` decision with consequences for every family trait, so it is noted in
    // this crate's README as an owed change and not forced from here. The practical effect:
    // a run blocks other Python threads in the same process. Use a process per run, which
    // is what the experiment system does anyway (08-measurement §4).
    let report = engine.run(&mut tee).map_err(err::engine)?;
    let records_refused = tee.refused_count();
    let samples = tee.finish(report.end_ns);

    if let Some(mut w) = writer {
        w.write_manifest(&manifest_json).map_err(err::record)?;
        w.finish().map_err(err::record)?;
    }

    let metrics_obj = Py::new(py, PyMetrics::new(samples, records_refused))?;
    Ok(PyRun {
        report,
        manifest,
        recording: recording.map(str::to_string),
        metrics: metrics_obj,
        records_refused,
    })
}

/// Runs a scenario twice and returns `(digest_a, digest_b)` over the record streams.
///
/// The determinism gate of ADR 0004 in one call, without files: both runs write into an
/// in-memory recorder and the digest covers the instant, the channel, the visibility and
/// the bytes of every record, **in order**. Two runs that emit the same records in a
/// different order are not the same run.
///
/// # Errors
/// As [`run`].
#[pyfunction]
#[pyo3(signature = (scenario, *, build_utc))]
pub fn run_twice(
    py: Python<'_>,
    scenario: &PyScenario,
    build_utc: &str,
) -> PyResult<(String, String)> {
    let s = scenario.scenario().clone();
    let _ = py;
    let mut digests = Vec::with_capacity(2);
    for _ in 0..2 {
        let mut engine = Engine::build(s.clone(), build_utc).map_err(err::engine)?;
        let mut rec = v2xw_engine::MemoryRecorder::new();
        engine.run(&mut rec).map_err(err::engine)?;
        digests.push(rec.digest_hex());
    }
    Ok((digests[0].clone(), digests[1].clone()))
}

/// An empty `dict`, used where a manifest is absent.
pub fn no_manifest(py: Python<'_>) -> Bound<'_, PyDict> {
    PyDict::new(py)
}
