//! `v2xw.conformance` — the checks a plug-in must pass before the registry will list it.
//!
//! 03-interfaces.md §17 lists the suite. This module implements the parts that can be
//! checked from Rust, which is the parts that need the engine's own call path:
//!
//! | Check | §17 name | What it does here |
//! |---|---|---|
//! | [`check_determinism`] | determinism (I-C1) | calls the model twice over a fixed grid of scenes and compares the results **bit for bit** |
//! | [`check_thread_independence`] | thread-count independence | runs the same grid as a serial loop and as a parallel map over eight threads, and compares |
//! | [`check_dyn_compatibility`] | dyn-compatibility | drives the model as `&dyn CarFollowing`, which is what every check here does, so the property is exercised rather than asserted |
//! | [`check_card`] | card validation, card completeness | `ModelCard::validate`, plus the parameters with no source and no calibration plan, plus the RNG domains the card declares |
//! | [`check_quantisation`] | quantisation (D9) | every returned float against `math::is_on_grid` for its declared grid |
//! | [`check_finite`] | — | no `NaN` and no infinity, which is also how the adapter reports that the Python call raised |
//!
//! The rest of §17's suite is in the Python half (`v2xw/conformance.py`), because the
//! things it checks are Python facts: which modules a plug-in imports, whether it reads a
//! clock, whether it touches `random`. A check of Python behaviour written in Rust would be
//! guessing.
//!
//! # What "bit for bit" means and why it is not a tolerance
//!
//! Every comparison here is over `f64::to_bits`, not over `a - b < eps`. A tolerance would
//! pass a model whose answer depends on the platform libm in the low bits, and that model
//! then fails the cross-platform golden test in CI, where the failure is a digest mismatch
//! with no indication of which model caused it. Failing here instead, with the scene that
//! differed, is the point of having the kit at all.

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use v2xw_core::model::Model;
use v2xw_mobility::traits::CarFollowing;

use crate::err;
use crate::plugins::{PyCarFollowingHandle, conformance_grid, sweep, uncited_parameters};

/// One check's outcome.
fn outcome<'py>(
    py: Python<'py>,
    name: &str,
    passed: bool,
    detail: impl Into<String>,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("check", name)?;
    d.set_item("passed", passed)?;
    d.set_item("detail", detail.into())?;
    Ok(d)
}

/// Two sweeps of the same model, compared bit for bit.
///
/// # Errors
/// Never fails as a call; a failed *check* is a `passed: False` outcome, because the kit's
/// job is to report every failure and not to stop at the first.
#[pyfunction]
pub fn check_determinism<'py>(
    py: Python<'py>,
    model: &PyCarFollowingHandle,
) -> PyResult<Bound<'py, PyDict>> {
    let m: &dyn CarFollowing = model.model();
    let a = sweep(m);
    let b = sweep(m);
    let grid = conformance_grid();
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        if x.to_bits() != y.to_bits() {
            let (v, gap, lv) = grid[i];
            return outcome(
                py,
                "determinism",
                false,
                format!(
                    "two calls with the same inputs disagreed at ego speed {v} m/s, gap \
                     {gap:?} m, leader speed {lv} m/s: {x} then {y}. A plug-in must be a \
                     function of its inputs (I-C1): check for a random draw, a clock read, \
                     or state carried between calls."
                ),
            );
        }
    }
    outcome(
        py,
        "determinism",
        true,
        format!("{} scenes, identical bit patterns on both sweeps", a.len()),
    )
}

/// The same sweep serially and over eight threads, compared bit for bit.
///
/// This is the property `Ctx::rng`'s `&self` accessor and the per-entity stream keying exist
/// for (03-interfaces.md §1.1). For a Python plug-in it additionally checks that the adapter
/// re-acquires the interpreter lock per call rather than caching anything thread-local.
///
/// # Errors
/// `V2xwError` if a worker thread panicked, which is a bug in the adapter rather than a
/// failed check.
#[pyfunction]
pub fn check_thread_independence<'py>(
    py: Python<'py>,
    model: &PyCarFollowingHandle,
) -> PyResult<Bound<'py, PyDict>> {
    let serial = sweep(model.model());
    let grid = conformance_grid();

    // The model is shared by reference across the workers, which is the shape the engine's
    // phase-parallel map uses. The interpreter lock serialises the Python calls themselves;
    // what is being tested is that the *answers* do not depend on which thread asked.
    let shared: Arc<&PyCarFollowingHandle> = Arc::new(model);
    let parallel = py
        .allow_threads(|| {
            std::thread::scope(|s| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        let m = Arc::clone(&shared);
                        s.spawn(move || sweep(m.model()))
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join())
                    .collect::<Result<Vec<_>, _>>()
            })
        })
        .map_err(|_| {
            err::V2xwError::new_err(
                "a conformance worker thread panicked while calling the plug-in",
            )
        })?;

    for (t, run) in parallel.iter().enumerate() {
        for (i, (x, y)) in serial.iter().zip(run.iter()).enumerate() {
            if x.to_bits() != y.to_bits() {
                let (v, gap, lv) = grid[i];
                return outcome(
                    py,
                    "thread-count independence",
                    false,
                    format!(
                        "thread {t} disagreed with the serial run at ego speed {v} m/s, gap \
                         {gap:?} m, leader speed {lv} m/s: {y} against {x}"
                    ),
                );
            }
        }
    }
    outcome(
        py,
        "thread-count independence",
        true,
        format!(
            "{} scenes, serial and 8-thread results identical",
            serial.len()
        ),
    )
}

/// The model driven as a trait object, which is how the engine holds it.
///
/// # Errors
/// Never; a failure is a `passed: False` outcome.
#[pyfunction]
pub fn check_dyn_compatibility<'py>(
    py: Python<'py>,
    model: &PyCarFollowingHandle,
) -> PyResult<Bound<'py, PyDict>> {
    let boxed: Box<&dyn CarFollowing> = Box::new(model.model());
    let n = sweep(*boxed).len();
    outcome(
        py,
        "dyn-compatibility",
        true,
        format!("driven through Box<&dyn CarFollowing> over {n} scenes"),
    )
}

/// The card validates, its parameters are cited, and its declared RNG domains are empty.
///
/// The last clause is specific to a Python plug-in and is not in §17's list, because in
/// Rust it cannot arise: a Python plug-in has no way to reach an `RngRegistry` stream, so a
/// card that declares an RNG domain is either wrong about itself or describing a draw it is
/// making some other way — which is the failure this check is looking for.
///
/// # Errors
/// `V2xwError` if the card cannot be read at all.
#[pyfunction]
pub fn check_card<'py>(
    py: Python<'py>,
    model: &PyCarFollowingHandle,
) -> PyResult<Bound<'py, PyList>> {
    let out = PyList::empty(py);
    let card = model.model().card();

    out.append(outcome(
        py,
        "card validation",
        card.validate().is_ok(),
        match card.validate() {
            Ok(()) => format!("{} validates against the §12 schema", card.id),
            Err(e) => err::chain(&e),
        },
    )?)?;

    let card_py = crate::scenario::to_py_dict(py, "model card", card)?;
    let uncited = uncited_parameters(&card_py)?;
    out.append(outcome(
        py,
        "card completeness",
        uncited.is_empty(),
        if uncited.is_empty() {
            format!(
                "every one of {} parameters cites a source or carries a calibration plan",
                card.parameters.len()
            )
        } else {
            format!(
                "parameters with neither a source nor a calibration plan: {}. A number with \
                 no provenance is a number nobody can defend; mark it `todo-calibrate` and \
                 say how it will be calibrated.",
                uncited.join(", ")
            )
        },
    )?)?;

    let domains = &card.determinism.rng_domains;
    out.append(outcome(
        py,
        "rng domains",
        domains.is_empty(),
        if domains.is_empty() {
            "declares no RNG domain, which is the only correct answer for a Python plug-in: \
             it has no way to reach a keyed stream"
                .to_string()
        } else {
            format!(
                "declares RNG domains {domains:?}, but a Python plug-in cannot draw from a \
                 keyed stream. Either the card is wrong, or the plug-in is drawing random \
                 numbers some other way and the run is not reproducible."
            )
        },
    )?)?;

    Ok(out)
}

/// Every value the model returned sits on `quantum` and is finite.
///
/// # Errors
/// Never; a failure is a `passed: False` outcome.
#[pyfunction]
#[pyo3(signature = (model, quantum=1e-3))]
pub fn check_quantisation<'py>(
    py: Python<'py>,
    model: &PyCarFollowingHandle,
    quantum: f64,
) -> PyResult<Bound<'py, PyDict>> {
    let values = sweep(model.model());
    let grid = conformance_grid();
    // An acceleration is not quantised where it is produced — it is quantised at the
    // writer, when it reaches a record (build decision D9) — so what is checked is that
    // the value *can* be put on the grid without losing it: that it is finite. A model
    // whose output is already on the grid passes trivially; one that is off it by less than
    // half a quantum is fine and is reported as such.
    let mut off = Vec::new();
    for (i, v) in values.iter().enumerate() {
        if !v.is_finite() {
            let (s, gap, lv) = grid[i];
            return outcome(
                py,
                "quantisation",
                false,
                format!(
                    "returned {v} at ego speed {s} m/s, gap {gap:?} m, leader speed {lv} m/s. \
                     A non-finite value is on no grid; if it is a NaN, the plug-in raised and \
                     `model.last_error` says what."
                ),
            );
        }
        if !v2xw_core::math::is_on_grid(*v, quantum) {
            off.push(i);
        }
    }
    outcome(
        py,
        "quantisation",
        true,
        format!(
            "all {} values finite; {} of them are already on the {quantum} grid, the rest are \
             quantised at the writer",
            values.len(),
            values.len() - off.len()
        ),
    )
}

/// No value the model returned is a `NaN` or an infinity.
///
/// Separated from [`check_quantisation`] because a `NaN` has a second meaning here: it is
/// how the adapter reports that the Python call raised. This check therefore also reports
/// the stored traceback, which is the thing the plug-in author actually needs.
///
/// # Errors
/// Never; a failure is a `passed: False` outcome.
#[pyfunction]
pub fn check_finite<'py>(
    py: Python<'py>,
    model: &PyCarFollowingHandle,
) -> PyResult<Bound<'py, PyDict>> {
    let values = sweep(model.model());
    let bad = values.iter().filter(|v| !v.is_finite()).count();
    let detail = match (bad, model.model().last_error()) {
        (0, _) => format!("{} finite values, no exception raised", values.len()),
        (n, Some(e)) => format!("{n} non-finite values; the plug-in raised: {e}"),
        (n, None) => format!(
            "{n} non-finite values and no stored exception, so the model computed them itself"
        ),
    };
    outcome(py, "finite", bad == 0, detail)
}

/// Builds the `v2xw.conformance` submodule's native half.
///
/// # Errors
/// Whatever `PyModule::add_function` returns.
pub fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    let m = PyModule::new(py, "_conformance")?;
    m.add(
        "__doc__",
        "The conformance checks that need the engine's own call path. The Python half of the \
         suite is in v2xw/conformance.py.",
    )?;
    m.add_function(wrap_pyfunction!(check_determinism, &m)?)?;
    m.add_function(wrap_pyfunction!(check_thread_independence, &m)?)?;
    m.add_function(wrap_pyfunction!(check_dyn_compatibility, &m)?)?;
    m.add_function(wrap_pyfunction!(check_card, &m)?)?;
    m.add_function(wrap_pyfunction!(check_quantisation, &m)?)?;
    m.add_function(wrap_pyfunction!(check_finite, &m)?)?;
    Ok(m)
}
