//! `v2xw.Scenario` — loading, overlaying, validating and hashing a run description.
//!
//! 03-interfaces.md §13 makes the scenario one YAML or JSON document, and
//! `v2xw_engine::Scenario` is the loader for it. This module adds nothing to that
//! pipeline; it exposes it, with two deliberate shapes:
//!
//! * **Validation is separable from loading.** `Scenario.load` raises on the first
//!   conflict, because a scenario that will not load cannot be run. `Scenario.problems()`
//!   returns *every* conflict as a list of strings without raising, which is what an editor
//!   or a sweep generator wants — it has many candidate documents and needs to know which
//!   are wrong and why, not to be interrupted by the first.
//! * **The content hash is a property, not a method.** It is the identity the run manifest
//!   pins (02-architecture.md §6.5), and a researcher comparing two runs reaches for it
//!   constantly.
//!
//! The typed document is available to Python as a plain `dict`
//! ([`PyScenario::as_dict`]) rather than as a mirrored class hierarchy. A mirror of a
//! schema with forty structs in it is forty places to forget to update; the `dict` is
//! generated from the same serde derives the loader uses, so it cannot drift.

use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3::types::PyDict;
use v2xw_engine::Scenario;

use crate::err;

/// A loaded, migrated, base-merged scenario.
#[pyclass(name = "Scenario", module = "v2xw", frozen)]
pub struct PyScenario {
    pub(crate) inner: Scenario,
    pub(crate) path: Option<PathBuf>,
}

impl PyScenario {
    /// The scenario behind a Python handle.
    pub fn scenario(&self) -> &Scenario {
        &self.inner
    }
}

#[pymethods]
impl PyScenario {
    /// Loads a scenario from a file, resolving `meta.base` relative to its directory.
    #[staticmethod]
    fn load(path: &str) -> PyResult<Self> {
        let inner = Scenario::load(path).map_err(err::engine)?;
        Ok(Self {
            inner,
            path: Some(Path::new(path).to_path_buf()),
        })
    }

    /// Parses a scenario from YAML or JSON text.
    ///
    /// `base_dir` is where a `meta.base` reference is resolved from; without it, a
    /// document that has one cannot be parsed and says so.
    #[staticmethod]
    #[pyo3(signature = (text, base_dir=None))]
    fn parse(text: &str, base_dir: Option<&str>) -> PyResult<Self> {
        let dir = base_dir.map(Path::new);
        let inner = Scenario::parse(text, dir).map_err(err::engine)?;
        Ok(Self {
            inner,
            path: dir.map(Path::to_path_buf),
        })
    }

    /// Builds a scenario from a Python `dict` of the same shape as the file.
    ///
    /// Validated, like [`PyScenario::load`]. `v2xw_engine::Scenario::from_document`
    /// deserialises without validating — it is the stage after the merge and before the
    /// check, and the engine's own loader validates in a later step — so the validation is
    /// done here explicitly. Without it, a document built in Python and one loaded from a
    /// file would disagree about what "loaded" means, and the disagreement would show up
    /// as a failure inside `run` rather than at the line that built the bad document.
    #[staticmethod]
    fn from_dict(doc: &Bound<'_, PyAny>) -> PyResult<Self> {
        let json: String = doc
            .py()
            .import("json")?
            .call_method1("dumps", (doc,))?
            .extract()?;
        let value = serde_json::from_str(&json).map_err(|e| err::json("scenario document", e))?;
        let inner = Scenario::from_document(value).map_err(err::engine)?;
        inner
            .validate()
            .map_err(|e| err::ScenarioError::new_err(err::chain(&e)))?;
        Ok(Self { inner, path: None })
    }

    /// The smallest scenario that runs: a procedural grid world and the schema defaults.
    ///
    /// This is what the worked example and the conformance kit start from, so neither
    /// depends on a file that may be edited underneath them.
    #[staticmethod]
    fn minimal() -> Self {
        Self {
            inner: Scenario::minimal(),
            path: None,
        }
    }

    /// The schema version string the document is at, after migration.
    #[getter]
    fn schema(&self) -> &str {
        &self.inner.schema
    }

    /// `meta.name`.
    #[getter]
    fn name(&self) -> &str {
        &self.inner.meta.name
    }

    /// The master seed every RNG stream derives from (ADR 0004 §3).
    #[getter]
    fn seed(&self) -> u64 {
        self.inner.seed
    }

    /// How long the run lasts, simulated seconds.
    #[getter]
    fn duration_s(&self) -> f64 {
        self.inner.time.duration_s
    }

    /// The file it was loaded from, if it came from one.
    #[getter]
    fn path(&self) -> Option<String> {
        self.path.as_ref().map(|p| p.display().to_string())
    }

    /// SHA-256 of the canonical JSON — the identity the run manifest pins.
    #[getter]
    fn content_hash(&self) -> PyResult<String> {
        self.inner
            .content_hash()
            .map_err(|e| err::ScenarioError::new_err(err::chain(&e)))
    }

    /// Every validation conflict, as messages, without raising.
    ///
    /// An empty list means the scenario is valid. `Scenario.load` raises on the first of
    /// these; this returns all of them, which is what a tool fixing a document needs.
    fn problems(&self) -> Vec<String> {
        v2xw_engine::scenario::validate(&self.inner)
            .iter()
            .map(|e| err::chain(e))
            .collect()
    }

    /// Raises `ScenarioError` if the scenario is not valid; returns `self` if it is.
    fn validate(slf: PyRef<'_, Self>) -> PyResult<PyRef<'_, Self>> {
        slf.inner
            .validate()
            .map_err(|e| err::ScenarioError::new_err(err::chain(&e)))?;
        Ok(slf)
    }

    /// The document as YAML.
    fn to_yaml(&self) -> PyResult<String> {
        self.inner
            .to_yaml()
            .map_err(|e| err::ScenarioError::new_err(err::chain(&e)))
    }

    /// The document as JSON.
    fn to_json(&self) -> PyResult<String> {
        self.inner
            .to_json()
            .map_err(|e| err::ScenarioError::new_err(err::chain(&e)))
    }

    /// The document as a Python `dict`.
    fn as_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let json = self
            .inner
            .to_json()
            .map_err(|e| err::ScenarioError::new_err(err::chain(&e)))?;
        py.import("json")?.call_method1("loads", (json,))
    }

    /// Writes the scenario to `path` as YAML.
    fn write(&self, path: &str) -> PyResult<()> {
        let text = self.to_yaml()?;
        std::fs::write(path, text).map_err(|e| err::io(path, e))
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!(
            "<v2xw.Scenario name={:?} seed={:#x} duration_s={} hash={}>",
            self.inner.meta.name,
            self.inner.seed,
            self.inner.time.duration_s,
            &self.content_hash()?[..12]
        ))
    }
}

/// Renders an arbitrary serialisable value as a Python `dict`.
///
/// Used by the manifest, the report and the recording metadata. It goes through JSON
/// rather than building `PyDict`s field by field for the reason the module note gives:
/// the serde derives are the single description of these structures, and a second,
/// hand-written one would drift from them.
///
/// # Errors
/// [`crate::err::V2xwError`] if the value will not serialise, or whatever `json.loads`
/// raises.
pub fn to_py_dict<'py, T: serde::Serialize>(
    py: Python<'py>,
    what: &str,
    value: &T,
) -> PyResult<Bound<'py, PyAny>> {
    let json = serde_json::to_string(value).map_err(|e| err::json(what, e))?;
    py.import("json")?.call_method1("loads", (json,))
}

/// An empty `dict`, for the `None` cases.
pub fn empty_dict(py: Python<'_>) -> Bound<'_, PyDict> {
    PyDict::new(py)
}
