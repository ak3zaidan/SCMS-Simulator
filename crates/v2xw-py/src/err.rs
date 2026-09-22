//! The exception hierarchy the `v2xw` package raises, and the conversions into it.
//!
//! Every error the bindings can produce is one of five Python types, all descending from
//! [`V2xwError`], so a notebook can write `except v2xw.V2xwError` and catch everything the
//! simulator can say. The split is by *which contract was broken*, not by which Rust crate
//! raised it:
//!
//! | Exception | Raised when |
//! |---|---|
//! | `ScenarioError` | a scenario will not load, parse, migrate or validate |
//! | `RecordingError` | a recording will not open, or is malformed, truncated or unverifiable |
//! | `MetricError` | a metric definition, sample or Arrow batch is rejected |
//! | `DeterminismError` | a plug-in broke a determinism rule the conformance kit enforces |
//! | `V2xwError` | everything else, including I/O |
//!
//! The Rust error types are not reconstructible from a Python exception, so the message is
//! the whole payload. Each conversion therefore keeps the source chain: an engine error
//! that wraps a scenario error prints both halves.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(
    _v2xw,
    V2xwError,
    PyException,
    "Base of every exception the simulator raises."
);
create_exception!(
    _v2xw,
    ScenarioError,
    V2xwError,
    "A scenario would not load, parse, migrate or validate."
);
create_exception!(
    _v2xw,
    RecordingError,
    V2xwError,
    "A recording would not open, or is malformed or unverifiable."
);
create_exception!(
    _v2xw,
    MetricError,
    V2xwError,
    "A metric definition, sample or table was rejected."
);
create_exception!(
    _v2xw,
    DeterminismError,
    V2xwError,
    "A plug-in broke one of the determinism rules (ADR 0004)."
);

/// Renders an error and everything it wraps as one message.
///
/// `thiserror`'s `Display` prints only the outermost layer, and the layer underneath is
/// usually the one that says what actually went wrong — "scenario invalid" over
/// "actors.vehicles.demand.target_count must be positive". Python has no error chaining
/// that survives a `PyErr::new`, so the chain is flattened into the message.
pub fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

/// An engine error as the right Python exception.
///
/// A scenario failure becomes `ScenarioError` wherever it came from, including when the
/// engine wrapped it, because the thing the caller has to fix is the scenario file.
pub fn engine(e: v2xw_engine::EngineError) -> PyErr {
    let message = chain(&e);
    match e {
        v2xw_engine::EngineError::Scenario(_) => ScenarioError::new_err(message),
        _ => V2xwError::new_err(message),
    }
}

/// A recording error as `RecordingError`.
pub fn record(e: v2xw_record::RecordError) -> PyErr {
    RecordingError::new_err(chain(&e))
}

/// A metrics error as `MetricError`.
pub fn metric(e: v2xw_metrics::MetricError) -> PyErr {
    MetricError::new_err(chain(&e))
}

/// A core error (card validation, registry refusal) as `V2xwError`.
pub fn core(e: v2xw_core::error::CoreError) -> PyErr {
    V2xwError::new_err(chain(&e))
}

/// A JSON error as `V2xwError`, with the context that says which document failed.
pub fn json(what: &str, e: serde_json::Error) -> PyErr {
    V2xwError::new_err(format!("{what}: {e}"))
}

/// An I/O error as `V2xwError`, naming the path.
pub fn io(path: &str, e: std::io::Error) -> PyErr {
    V2xwError::new_err(format!("{path}: {e}"))
}

/// Registers the exception types on the module.
///
/// # Errors
/// Whatever `PyModule::add` returns.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("V2xwError", m.py().get_type::<V2xwError>())?;
    m.add("ScenarioError", m.py().get_type::<ScenarioError>())?;
    m.add("RecordingError", m.py().get_type::<RecordingError>())?;
    m.add("MetricError", m.py().get_type::<MetricError>())?;
    m.add("DeterminismError", m.py().get_type::<DeterminismError>())?;
    Ok(())
}
