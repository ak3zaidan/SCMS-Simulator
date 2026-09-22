//! `v2xw.math` — the transcendental functions a Python plug-in must use instead of `math`.
//!
//! # Why a plug-in may not call `math.exp`
//!
//! ADR 0004 §4 bans `std` transcendentals in the Rust code for a concrete reason: `sin`,
//! `exp`, `log` and `pow` in a platform libm are not specified to the last bit, and two
//! machines can disagree in the low bits of a result. The whole crate tree therefore routes
//! through `v2xw_core::math`, which delegates to the pure-Rust `libm` and is bit-identical
//! everywhere.
//!
//! CPython's `math` module is the platform libm again, with the same problem — and it
//! reaches further than `math`: `**` on floats, `numpy`'s ufuncs and `statistics` are all
//! the same C library. A Python car-following model that computes
//! `a * (1 - (v/v0)**4 - (s_star/s)**2)` can therefore produce a different acceleration on
//! macOS and on Linux, and the run digests will not match although nothing is wrong with
//! the model.
//!
//! This module is the fix: the *same* functions the Rust models use, exposed to Python, so
//! a plug-in that writes `v2xw.math.pow(v / v0, 4)` gets the engine's answer rather than
//! its interpreter's. [`crate::conformance`] checks that a plug-in imports it, and the
//! two-platform golden test is what would catch one that did not.
//!
//! # What is not here
//!
//! `+`, `-`, `*`, `/` and comparison. IEEE-754 specifies those exactly; Python and Rust
//! agree on them bit for bit and there is nothing to route.

use pyo3::prelude::*;
use v2xw_core::math;

/// Binds a one-argument function from `v2xw_core::math`.
macro_rules! unary {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        fn $name(x: f64) -> f64 {
            math::$name(x)
        }
    };
}

unary!(sin, "Sine, reproducibly.");
unary!(cos, "Cosine, reproducibly.");
unary!(tan, "Tangent, reproducibly.");
unary!(asin, "Arcsine, reproducibly.");
unary!(acos, "Arccosine, reproducibly.");
unary!(atan, "Arctangent, reproducibly.");
unary!(exp, "e to the x, reproducibly.");
unary!(exp2, "2 to the x, reproducibly.");
unary!(ln, "Natural logarithm, reproducibly.");
unary!(log10, "Base-10 logarithm, reproducibly.");
unary!(log2, "Base-2 logarithm, reproducibly.");
unary!(
    sqrt,
    "Square root. IEEE-754 specifies this one exactly; it is here for company."
);
unary!(cbrt, "Cube root, reproducibly.");
unary!(sinh, "Hyperbolic sine, reproducibly.");
unary!(cosh, "Hyperbolic cosine, reproducibly.");
unary!(tanh, "Hyperbolic tangent, reproducibly.");

/// `y / x` as an angle in radians, reproducibly.
#[pyfunction]
fn atan2(y: f64, x: f64) -> f64 {
    math::atan2(y, x)
}

/// `base ** exponent`, reproducibly. Use this instead of Python's `**` on floats.
#[pyfunction]
fn pow(base: f64, exponent: f64) -> f64 {
    math::pow(base, exponent)
}

/// `sqrt(x*x + y*y)` without the intermediate overflow, reproducibly.
#[pyfunction]
fn hypot(x: f64, y: f64) -> f64 {
    math::hypot(x, y)
}

/// Sine and cosine together, reproducibly.
#[pyfunction]
fn sin_cos(x: f64) -> (f64, f64) {
    math::sin_cos(x)
}

/// Rounds `x` onto a grid of `quantum` — the writer-side quantisation of build decision D9.
///
/// Every float a plug-in emits must sit on its declared grid. This is the function that
/// puts it there, and [`is_on_grid`] is the check.
#[pyfunction]
fn quantize_to(x: f64, quantum: f64) -> f64 {
    math::quantize_to(x, quantum)
}

/// Rounds `x` to `decimals` decimal places on the engine's rule.
#[pyfunction]
fn quantize(x: f64, decimals: u8) -> f64 {
    math::quantize(x, decimals)
}

/// True if `x` already sits on a grid of `quantum`.
///
/// The conformance kit's quantisation check is this function over every float a plug-in
/// emitted. A non-finite value is not on any grid and answers `False`.
#[pyfunction]
fn is_on_grid(x: f64, quantum: f64) -> bool {
    math::is_on_grid(x, quantum)
}

/// The integer multiple of `quantum` that `x` sits on — what a digest hashes, not the float.
#[pyfunction]
fn grid_index(x: f64, quantum: f64) -> i64 {
    math::grid_index(x, quantum)
}

/// Sums in IEEE-754 total order, so the result does not depend on the input's order.
///
/// Floating-point addition is not associative, so `sum(xs)` over a list a plug-in happened
/// to build in a different order is a different number. Every reduction that reaches an
/// output goes through this.
#[pyfunction]
fn sum_ordered(values: Vec<f64>) -> f64 {
    math::sum_ordered(values)
}

/// Sums `(key, value)` pairs in key order — the reduction over an entity map.
#[pyfunction]
fn sum_sorted_by_key(pairs: Vec<(String, f64)>) -> f64 {
    math::sum_sorted_by_key(pairs)
}

/// Builds the `v2xw.math` submodule.
///
/// # Errors
/// Whatever `PyModule::add_function` returns.
pub fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    let m = PyModule::new(py, "math")?;
    m.add(
        "__doc__",
        "Deterministic transcendentals: the same pure-Rust libm the engine uses. A plug-in \
         calls these instead of Python's `math`, whose results are the platform's C library \
         and can differ in the low bits between machines (ADR 0004 §4).",
    )?;
    macro_rules! add {
        ($($f:ident),* $(,)?) => { $( m.add_function(wrap_pyfunction!($f, &m)?)?; )* };
    }
    add!(
        sin,
        cos,
        tan,
        asin,
        acos,
        atan,
        atan2,
        exp,
        exp2,
        ln,
        log10,
        log2,
        pow,
        sqrt,
        cbrt,
        hypot,
        sinh,
        cosh,
        tanh,
        sin_cos,
        quantize_to,
        quantize,
        is_on_grid,
        grid_index,
        sum_ordered,
        sum_sorted_by_key,
    );
    Ok(m)
}
