//! `v2xw-py` — the native half of the `v2xw` Python package.
//!
//! This crate is how a researcher actually drives the simulator: load a scenario, run it,
//! open the recording, pull a metric out as a table, and — when the model they want does
//! not exist — write it in Python and have the engine call it.
//!
//! It is a *binding*, not a layer. Every decision about what a run means lives in
//! `v2xw-engine`, `v2xw-record` and `v2xw-metrics`; nothing here reimplements one. Where
//! this crate adds anything, it is one of four things, and each is a module:
//!
//! | Module | What it adds |
//! |---|---|
//! | [`scenario`] | loading and validating a scenario, with the "every problem" form an editor needs beside the "raise on the first" form a run needs |
//! | [`run`] | the tee that feeds the recording and the metric providers from one record stream, and the windowing that makes a metric a time series |
//! | [`recording`] | the three ways out of a recording — Arrow, IPC bytes, dictionaries — and honest verification |
//! | [`metrics`] | the Arrow hand-off, zero copy where `pyarrow` is present |
//! | [`plugins`] | the Python plug-in seam: a Python object wearing an engine trait |
//! | [`conformance`] | the checks of 03-interfaces.md §17 that need the engine's own call path |
//! | [`mathmod`] | the deterministic transcendentals a plug-in must use instead of `math` |
//!
//! # The determinism boundary
//!
//! Everything this crate exposes sits on the engine side of a contract that a Python
//! caller can break by accident. Three rules are worth stating here, because they are the
//! reason several signatures look the way they do:
//!
//! * **No wall clock is read on this side.** `run` requires the caller's `build_utc` and
//!   will not invent one. The Python wrapper reads the clock, visibly, in the caller's
//!   process.
//! * **No randomness originates here.** Nothing in this crate creates an RNG, and no
//!   plug-in family exposed here is handed one.
//! * **No hash iteration reaches an output.** Every map that decides an ordering is a
//!   `BTreeMap` or a sorted `Vec`; `PyMetrics::names` sorts, `PyMetrics::series` sorts by
//!   time, and `Recording::records` preserves the file's order.
//!
//! [`plugins`] has the full list of what a Python plug-in may not do, and [`conformance`]
//! is what checks it.
//!
//! # Build
//!
//! `maturin develop` inside `python/`, or `maturin build`. The `extension-module` feature
//! is what maturin turns on; it is deliberately **not** a default, because it tells pyo3
//! not to link libpython and `cargo test` needs those symbols.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod conformance;
pub mod err;
pub mod mathmod;
pub mod metrics;
pub mod plugins;
pub mod recording;
pub mod run;
pub mod scenario;

use pyo3::prelude::*;

/// The version of the `v2xw` package, which is the workspace version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The native module the `v2xw` package imports.
///
/// # Errors
/// Whatever registering a class, a function or a submodule returns.
#[pymodule]
fn _v2xw(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    populate(py, m)
}

/// Builds the module standalone, without the import machinery.
///
/// The tests use this. Going through `append_to_inittab` and `import` instead would make
/// every test depend on the interpreter's import system being set up the way a wheel sets
/// it up, which is a different thing from the bindings working; this builds the same module
/// object the wheel exposes and hands it over directly.
///
/// # Errors
/// As [`populate`].
pub fn build_module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    let m = PyModule::new(py, "_v2xw")?;
    populate(py, &m)?;
    Ok(m)
}

/// Registers everything the module exposes.
///
/// # Errors
/// Whatever registering a class, a function or a submodule returns.
pub fn populate(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", VERSION)?;
    m.add(
        "__doc__",
        "The native half of the v2xw package. Import `v2xw`, not this module.",
    )?;

    err::register(m)?;

    m.add_class::<scenario::PyScenario>()?;
    m.add_class::<metrics::PyMetrics>()?;
    m.add_class::<recording::PyRecording>()?;
    m.add_class::<run::PyRun>()?;
    m.add_function(wrap_pyfunction!(run::run, m)?)?;
    m.add_function(wrap_pyfunction!(run::run_twice, m)?)?;

    // Submodules are registered in `sys.modules` as well as attached to the parent, so
    // `from v2xw._v2xw.math import exp` works and not only `_v2xw.math.exp`. Python does
    // not do this for a submodule created from Rust, and without it the import machinery
    // and the attribute lookup disagree about whether the module exists.
    let sys_modules = py.import("sys")?.getattr("modules")?;
    for (name, module) in [
        ("math", mathmod::module(py)?),
        ("plugins", plugins::module(py)?),
        ("_conformance", conformance::module(py)?),
    ] {
        sys_modules.set_item(format!("v2xw._v2xw.{name}"), &module)?;
        m.add(name, module)?;
    }

    Ok(())
}
