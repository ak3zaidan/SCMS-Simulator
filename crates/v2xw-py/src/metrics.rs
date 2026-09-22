//! `v2xw.Metrics` — a run's metric samples, and the two ways they cross into Python.
//!
//! # Why there are two ways, and which one is the default
//!
//! A metric table is a column store on both sides of this boundary: `v2xw-metrics` builds
//! `arrow::RecordBatch`es ([`v2xw_metrics::arrow_out`]) and a researcher reads
//! `pyarrow`/`polars`/`pandas`, which are the same buffers again. Turning that into a list
//! of Python dictionaries and back is the single most expensive thing these bindings could
//! do, so the default does not:
//!
//! * [`PyMetrics::arrow`] hands Python the **same allocation**, through the Arrow C data
//!   interface. Nothing is serialised, nothing is copied, and a hundred-megabyte table
//!   costs a pointer. This is the path ADR 0008 decision 2 means by "tabular metric batches
//!   … for Python plug-in exchange", and it needs `pyarrow` importable.
//! * [`PyMetrics::ipc`] returns the Arrow **IPC stream** as `bytes`. One buffer, one copy,
//!   no per-row objects, and no dependency on `pyarrow` at all — `polars.read_ipc_stream`
//!   or a file on disk will take it. This is the fallback, and it is also what an
//!   out-of-process consumer wants.
//!
//! [`PyMetrics::rows`] does build Python objects, one dictionary per sample. It exists
//! because a five-row assertion in a test should not require Arrow, and it says so in its
//! own docstring rather than being quietly the fast path.
//!
//! # What is in the table
//!
//! One row per [`MetricSample`]: the instant, the metric, its unit, its dimensions as a
//! JSON object, the point estimate with its confidence interval where it has one, the
//! sample count, the percentiles where the value is a distribution, and the grid the value
//! was quantised onto. `v2xw-metrics` writes every float in it on that grid (build
//! decision D9) and `samples_batch` re-checks the post-condition, so a table that comes
//! out of here has already failed loudly if a value was off its grid.

use std::collections::BTreeSet;

use arrow::array::RecordBatch;
use arrow::pyarrow::IntoPyArrow;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use v2xw_metrics::{MetricSample, RunSummary, arrow_out};

use crate::err;
use crate::scenario::to_py_dict;

/// The metric samples a run produced.
#[pyclass(name = "Metrics", module = "v2xw", frozen)]
pub struct PyMetrics {
    pub(crate) samples: Vec<MetricSample>,
    pub(crate) rejected_records: u64,
}

impl PyMetrics {
    /// Wraps the samples a run flushed.
    pub fn new(samples: Vec<MetricSample>, rejected_records: u64) -> Self {
        Self {
            samples,
            rejected_records,
        }
    }

    /// The Arrow batch for the samples.
    fn batch(&self) -> PyResult<RecordBatch> {
        arrow_out::samples_batch(&self.samples).map_err(err::metric)
    }
}

#[pymethods]
impl PyMetrics {
    /// How many samples there are.
    fn __len__(&self) -> usize {
        self.samples.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "<v2xw.Metrics samples={} metrics={}>",
            self.samples.len(),
            self.names().len()
        )
    }

    /// Every metric name present, in sorted order.
    fn names(&self) -> Vec<String> {
        self.samples
            .iter()
            .map(|s| s.metric.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The samples as a `pyarrow.RecordBatch`, sharing this table's buffers.
    ///
    /// Zero copy: the Arrow arrays are handed over through the C data interface and Python
    /// owns a reference to them. Requires `pyarrow`; use [`PyMetrics::ipc`] if it is not
    /// installed.
    fn arrow(&self, py: Python<'_>) -> PyResult<PyObject> {
        self.batch()?.into_pyarrow(py)
    }

    /// The samples as an Arrow IPC **stream**, as `bytes`.
    ///
    /// One buffer, no `pyarrow` dependency: `pyarrow.ipc.open_stream(buf).read_all()`,
    /// `polars.read_ipc_stream(buf)` or a `.arrows` file on disk all read it.
    fn ipc<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = arrow_out::to_ipc(&[self.batch()?]).map_err(err::metric)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Writes the samples as an Arrow IPC stream file, returning its size in bytes.
    fn write_arrow(&self, path: &str) -> PyResult<u64> {
        let bytes = arrow_out::to_ipc(&[self.batch()?]).map_err(err::metric)?;
        std::fs::write(path, &bytes).map_err(|e| err::io(path, e))?;
        Ok(bytes.len() as u64)
    }

    /// The samples as a list of dictionaries — **the slow path**.
    ///
    /// One Python object per field per row. Fine for a handful of assertions in a test,
    /// wrong for a table: use [`PyMetrics::arrow`] or [`PyMetrics::ipc`] for anything a
    /// human would call a dataset.
    #[pyo3(signature = (metric=None))]
    fn rows<'py>(&self, py: Python<'py>, metric: Option<&str>) -> PyResult<Bound<'py, PyList>> {
        let out = PyList::empty(py);
        for s in &self.samples {
            if metric.is_some_and(|m| m != s.metric) {
                continue;
            }
            out.append(to_py_dict(py, "metric sample", s)?)?;
        }
        Ok(out)
    }

    /// `(t_ns, value)` pairs for one metric, in time order, skipping insufficient samples.
    ///
    /// The convenience a plot is made of. A sample the statistics refused — too few
    /// observations for an honest estimate — has no point value and is *left out* rather
    /// than rendered as a zero or a `NaN`; [`PyMetrics::rows`] shows the refusals.
    fn series(&self, metric: &str) -> Vec<(u64, f64)> {
        let mut out: Vec<(u64, f64)> = self
            .samples
            .iter()
            .filter(|s| s.metric == metric)
            .filter_map(|s| s.value.point().map(|v| (s.t, v)))
            .collect();
        out.sort_by_key(|(t, _)| *t);
        out
    }

    /// The last point value of `metric`, or `None` if it never produced one.
    fn last(&self, metric: &str) -> Option<f64> {
        self.series(metric).last().map(|(_, v)| *v)
    }

    /// The run summary the manifest carries: the digested metrics and their digest.
    fn summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let mut summary = RunSummary::new(self.samples.clone()).map_err(err::metric)?;
        summary.rejected_records = self.rejected_records;
        to_py_dict(py, "run summary", &summary)
    }

    /// The SHA-256 digest over the digested metrics — what two runs are compared on.
    #[getter]
    fn digest(&self) -> PyResult<String> {
        Ok(RunSummary::new(self.samples.clone())
            .map_err(err::metric)?
            .digest)
    }

    /// The runtime diagnostics as their own dictionary.
    ///
    /// Separate on purpose: these are machine-dependent (events per second, wall clock per
    /// simulated second, peak memory), so `v2xw-metrics` keeps them out of the summary
    /// document and out of its digest. They are useful and they are not evidence.
    fn diagnostics<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let summary = RunSummary::new(self.samples.clone()).map_err(err::metric)?;
        let json = summary.diagnostics_json().map_err(err::metric)?;
        let text = String::from_utf8(json)
            .map_err(|e| err::MetricError::new_err(format!("diagnostics json: {e}")))?;
        py.import("json")?.call_method1("loads", (text,))?.extract()
    }
}
