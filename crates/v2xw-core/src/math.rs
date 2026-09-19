//! Deterministic transcendental functions.
//!
//! **Never call `f64::sin`, `f64::exp`, `f64::powf` or any other standard-library
//! transcendental anywhere in the engine. Call these instead.**
//!
//! Rust's standard library documents that the precision of `sin`, `cos`, `exp`, `pow`
//! and the rest of the transcendentals is "non-deterministic": it "varies by platform,
//! Rust version, and can even differ within the same execution", because those methods
//! delegate to the platform's libm (ADR 0003, evidence R8 §B.1). A simulator whose
//! contract is byte-identical output on macOS, Linux, Windows, x86-64, arm64 and WASM
//! therefore cannot use them. This module re-exports the pure-Rust [`libm`] crate (a
//! port of musl's libm), which is the *same code* on every target and so produces the
//! same bits everywhere; the golden determinism tests treat its output as the reference
//! (ADR 0003 Consequences, ADR 0004 §4).
//!
//! `sqrt` is the one exception and is implemented with the standard-library intrinsic:
//! IEEE-754 requires `sqrt` to be correctly rounded, so it is bit-identical on every
//! conforming platform. The same applies to `+ - * /`, `abs`, `floor`, `ceil`, `round`
//! and `trunc`, which need no wrapper. `mul_add` (FMA) is *not* wrapped and must not be
//! used in models: Rust never contracts `a * b + c` into an FMA by itself, so calling
//! `mul_add` explicitly would make a model's result depend on whether the target has an
//! FMA unit (02-architecture.md §6.3).
//!
//! The conformance kit greps plug-in crates for `f64::sin`-style calls and fails them;
//! this module is the sanctioned alternative.

/// Sine of `x` (radians).
pub fn sin(x: f64) -> f64 {
    libm::sin(x)
}

/// Cosine of `x` (radians).
pub fn cos(x: f64) -> f64 {
    libm::cos(x)
}

/// Sine and cosine of `x` (radians), computed together.
pub fn sin_cos(x: f64) -> (f64, f64) {
    libm::sincos(x)
}

/// Tangent of `x` (radians).
pub fn tan(x: f64) -> f64 {
    libm::tan(x)
}

/// Arc sine of `x`, in radians.
pub fn asin(x: f64) -> f64 {
    libm::asin(x)
}

/// Arc cosine of `x`, in radians.
pub fn acos(x: f64) -> f64 {
    libm::acos(x)
}

/// Arc tangent of `x`, in radians.
pub fn atan(x: f64) -> f64 {
    libm::atan(x)
}

/// Four-quadrant arc tangent of `y / x`, in radians.
pub fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}

/// `e` raised to the power `x`.
pub fn exp(x: f64) -> f64 {
    libm::exp(x)
}

/// `2` raised to the power `x`.
pub fn exp2(x: f64) -> f64 {
    libm::exp2(x)
}

/// Natural logarithm of `x`.
pub fn ln(x: f64) -> f64 {
    libm::log(x)
}

/// Base-10 logarithm of `x`. The workhorse of every dB conversion in the radio stack.
pub fn log10(x: f64) -> f64 {
    libm::log10(x)
}

/// Base-2 logarithm of `x`.
pub fn log2(x: f64) -> f64 {
    libm::log2(x)
}

/// `base` raised to the power `exponent`.
pub fn pow(base: f64, exponent: f64) -> f64 {
    libm::pow(base, exponent)
}

/// Cube root of `x`.
pub fn cbrt(x: f64) -> f64 {
    libm::cbrt(x)
}

/// `sqrt(x² + y²)` without intermediate overflow.
pub fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

/// Hyperbolic sine of `x`.
pub fn sinh(x: f64) -> f64 {
    libm::sinh(x)
}

/// Hyperbolic cosine of `x`.
pub fn cosh(x: f64) -> f64 {
    libm::cosh(x)
}

/// Hyperbolic tangent of `x`.
pub fn tanh(x: f64) -> f64 {
    libm::tanh(x)
}

/// Square root of `x`.
///
/// Uses the standard-library intrinsic on purpose: IEEE-754 requires `sqrt` to be
/// correctly rounded, so it is bit-identical on every conforming platform, and the
/// intrinsic is a single hardware instruction where `libm`'s would be a software loop.
pub fn sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// The writer-side quantiser's grid for the legacy three-decimal convention: 1 mm, 1 ms.
pub const LEGACY_QUANTUM: f64 = 1e-3;

/// The legacy engine's float convention, in decimal places: `round(x, 3)`.
pub const LEGACY_DECIMALS: u8 = 3;

/// The largest `decimals` [`quantize`] accepts; `10^17` is the last power of ten an `f64`
/// holds exactly at the precision this function needs.
pub const MAX_DECIMALS: u8 = 17;

/// Exact powers of ten, so the scale factor never comes from `powf`/`powi`.
const POW10: [f64; 18] = [
    1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17,
];

/// Rounds `x` to the nearest multiple of `quantum` — **the writer-side quantiser of
/// ADR 0004 decision 7 and build decision D9**.
///
/// No floating-point value may reach a recorded, exported or digested artefact in raw
/// IEEE-754 form. Every float leaving the engine passes through this one function on its
/// field's declared quantum (metres and seconds 1e-3, dB 1e-2, ratios 1e-4, probabilities
/// 1e-6, D9), and every digest is computed over quantised values only. That is what makes
/// a digest survive a change of maths library, compiler or target: ADR 0004's evidence is
/// a legacy dataset whose golden digests stopped reproducing because exactly one field
/// escaped rounding, and its `sin`/`cos`-derived coordinates then differed by 1 ULP
/// between Windows x86-64 and macOS arm64.
///
/// # The rounding mode, exactly
///
/// `(x · (1/quantum)).round() / (1/quantum)`, so **ties round away from zero** — the
/// behaviour of [`f64::round`], symmetric about zero, and the same rule every other
/// rounding in the engine uses. It is built only from `*`, `/` and `round`, all of which
/// IEEE-754 requires to be correctly rounded, so the result is bit-identical on every
/// target; that, not decimal exactness, is what the determinism contract needs.
///
/// The result is the nearest `f64` to `k · quantum` for an integer `k`, which in binary
/// arithmetic is not *exactly* a multiple of a decimal quantum — no decimal grid is. What
/// matters and what [`is_on_grid`] checks is that the operation is idempotent:
/// `quantize_to(quantize_to(x, q), q) == quantize_to(x, q)` bit for bit.
///
/// Because it rounds the *scaled binary* value rather than the exact decimal expansion, it
/// can differ by one quantum from Python's `round(x, n)` on a value that is a near-tie:
/// `quantize_to(2.675, 1e-2)` is `2.68` where Python's `round(2.675, 2)` is `2.67`, the
/// exact value being 2.674999999999999822…. That is deliberate and harmless here: D9 §2
/// retires the legacy frozen digests (they are unreproducible on any non-Windows host
/// anyway) and D9 §3 compares the Rust engine against the Python reference within `1e-9`,
/// never by digest equality.
///
/// Non-finite values pass through unchanged: the wire protocol uses `NaN` as the "absent"
/// sentinel for a float, and quantising a sentinel would destroy it.
///
/// # Panics
/// If `quantum` is not strictly positive and finite. A grid of zero, negative or NaN has
/// no nearest point, and silently returning the raw value would put an unquantised float
/// into an artefact — precisely the bug class D9 exists to close.
#[inline]
pub fn quantize_to(x: f64, quantum: f64) -> f64 {
    assert!(
        quantum > 0.0 && quantum.is_finite(),
        "quantize_to: the quantum must be strictly positive and finite, got {quantum}"
    );
    if !x.is_finite() {
        return x;
    }
    let inv = 1.0 / quantum;
    let scaled = x * inv;
    if !scaled.is_finite() {
        // The scale factor overflowed. A magnitude that large is already an exact
        // multiple of far more than one quantum — consecutive `f64`s there are further
        // apart than the grid — so the nearest grid point is the value itself.
        return x;
    }
    scaled.round() / inv
}

/// [`quantize_to`] on the decimal grid `10^-decimals`.
///
/// Identical in every respect — same rounding, same guarantees — for the fields whose
/// contract is written in decimal places rather than in a quantum. `quantize(x, 3)` and
/// `quantize_to(x, 1e-3)` return the same bits.
///
/// # Panics
/// If `decimals` exceeds [`MAX_DECIMALS`].
#[inline]
pub fn quantize(x: f64, decimals: u8) -> f64 {
    assert!(
        decimals <= MAX_DECIMALS,
        "quantize: {decimals} decimals exceeds the {MAX_DECIMALS} this function supports"
    );
    if !x.is_finite() {
        return x;
    }
    let scale = POW10[decimals as usize];
    let scaled = x * scale;
    if !scaled.is_finite() {
        return x; // See `quantize_to`: already coarser than the grid.
    }
    scaled.round() / scale
}

/// [`quantize`] at the legacy three-decimal convention: the default grid for metres,
/// seconds and anything the reference engine wrote with `round(x, 3)`.
#[inline]
pub fn q3(x: f64) -> f64 {
    quantize(x, LEGACY_DECIMALS)
}

/// True if `x` is already exactly what [`quantize_to`] would return for it.
///
/// The predicate behind ADR 0004's and D9's scanning test — "a test scans every output
/// file for any value that is off its grid". Non-finite values are on every grid, because
/// they pass through quantisation untouched as sentinels.
#[inline]
pub fn is_on_grid(x: f64, quantum: f64) -> bool {
    !x.is_finite() || quantize_to(x, quantum).to_bits() == x.to_bits()
}

/// True if `x` is already on the `10^-decimals` grid. The decimal spelling of
/// [`is_on_grid`].
#[inline]
pub fn is_quantized(x: f64, decimals: u8) -> bool {
    !x.is_finite() || quantize(x, decimals).to_bits() == x.to_bits()
}

/// The **integer multiple** of `quantum` that `x` quantises to — the form a cross-platform
/// digest hashes.
///
/// [`quantize_to`] returns the nearest `f64` to `k · quantum`, which is what a recorded
/// artefact carries. A *digest* wants `k` itself: the quantised `f64` is still a binary
/// approximation of a decimal grid point, and hashing its bits reintroduces exactly the
/// question ("is this the same float everywhere?") that quantisation was there to remove.
/// Hashing the integer removes it: two engines that agree on the grid point agree on the
/// bytes, whatever their float formatting or their libm.
///
/// `grid_index(x, q)` and `quantize_to(x, q)` use the same rounding — `(x / q).round()`,
/// ties away from zero — so a value that is recorded through one and digested through the
/// other cannot disagree.
///
/// Non-finite values map to the extremes of the range rather than to a number, and each to a
/// *distinct* one, so a digest can tell them apart and no finite value can collide with a
/// sentinel:
///
/// | `x` | result |
/// |---|---|
/// | `NaN` | `i64::MIN` |
/// | `-INFINITY` | `i64::MIN + 1` |
/// | `+INFINITY` | `i64::MAX` |
/// | finite, beyond the `i64` range | saturates to `i64::MAX` / `i64::MIN + 1` |
///
/// This is the helper `v2xw-world` had to invent as `crate::quant::grid_index`; it belongs
/// here, with the quantiser whose rounding it must match, so that the two cannot drift.
///
/// ```
/// use v2xw_core::math::grid_index;
/// assert_eq!(grid_index(12.346, 1e-3), 12_346);
/// assert_eq!(grid_index(-12.346, 1e-3), -12_346);
/// // The two `f64`s either side of a grid point digest identically.
/// assert_eq!(
///     grid_index(12.346_000_000_000_001, 1e-3),
///     grid_index(12.345_999_999_999_999, 1e-3)
/// );
/// ```
///
/// # Panics
/// If `quantum` is not strictly positive and finite, exactly as [`quantize_to`] does.
#[inline]
pub fn grid_index(x: f64, quantum: f64) -> i64 {
    assert!(
        quantum > 0.0 && quantum.is_finite(),
        "grid_index: the quantum must be strictly positive and finite, got {quantum}"
    );
    if x.is_nan() {
        return i64::MIN;
    }
    if x == f64::NEG_INFINITY {
        return i64::MIN + 1;
    }
    if x == f64::INFINITY {
        return i64::MAX;
    }
    let scaled = (x * (1.0 / quantum)).round();
    if scaled >= i64::MAX as f64 {
        i64::MAX
    } else if scaled <= (i64::MIN + 1) as f64 {
        i64::MIN + 1
    } else {
        scaled as i64
    }
}

/// Sums `values` **in the order given**, with Neumaier compensation — the reduction every
/// value that reaches an output must go through.
///
/// # Why naive summation is not acceptable
///
/// Floating-point addition is not associative: `(a + b) + c` and `a + (b + c)` differ in
/// the last bits for almost any real data. A phase-parallel run (02-architecture.md §6.4)
/// splits a reduction over an unknown number of `rayon` tasks, so a naive `iter().sum()`
/// over per-task partial sums produces a value that depends on the thread count and on how
/// the work happened to be chunked. The crate's headline contract is that it does not:
/// "same scenario + seed ⇒ byte-identical outputs … single- or multi-threaded". An
/// interference sum over 400 transmitters, a CBR average over a window or a metric
/// aggregate is exactly such a reduction, and each of those lands in a recorded artefact
/// whose digest is compared across platforms.
///
/// The rule is therefore: **fix the order, then sum with a fixed algorithm**. This function
/// is the second half; the caller supplies the first half by iterating in id order
/// ([`sum_sorted_by_key`] does that too, given the ids).
///
/// # The algorithm, exactly
///
/// Neumaier's variant of Kahan compensated summation, which recovers the low-order bits
/// lost when a small addend meets a large accumulator:
///
/// ```text
/// t = sum + x
/// c += if |sum| >= |x| { (sum - t) + x } else { (x - t) + sum }
/// sum = t                      … and the result is sum + c
/// ```
///
/// Every operation is an IEEE-754 add or subtract, all of which are correctly rounded, so
/// the result is bit-identical on every target — and no `mul_add` appears, which would make
/// it depend on whether the target has an FMA unit (02-architecture.md §6.3).
///
/// Compensation reduces the error of the sum; it does **not** make the sum
/// order-independent, so it is not a licence to reduce in an arbitrary order. It is here so
/// that a run's aggregates do not drift as they accumulate: over a million 10 Hz samples a
/// naive sum loses several digits, and a value that has lost digits can cross a quantisation
/// boundary that its exact counterpart does not (build decision D10).
///
/// If the running sum becomes non-finite the compensation is discarded and the running sum
/// is returned, so an infinite contributor yields an infinity rather than the `NaN` that
/// `inf - inf` would put into the compensation term.
///
/// ```
/// use v2xw_core::math::sum_ordered;
/// // Naive left-to-right summation returns 0.0 here; the 1.0 is lost against 1e16.
/// assert_eq!(sum_ordered([1e16, 1.0, -1e16]), 1.0);
/// ```
pub fn sum_ordered(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = 0.0_f64;
    let mut c = 0.0_f64;
    for x in values {
        let t = sum + x;
        if sum.abs() >= x.abs() {
            c += (sum - t) + x;
        } else {
            c += (x - t) + sum;
        }
        sum = t;
    }
    if !sum.is_finite() {
        // `inf - inf` in the compensation term would turn a legitimate infinity into NaN.
        return sum;
    }
    sum + c
}

/// Sorts `(key, value)` pairs by key and then sums them with [`sum_ordered`] — the
/// **id-ordered reduction** of 02-architecture.md §6.4 in one call.
///
/// This is the form to use when the contributors arrive in whatever order a parallel phase
/// finished them in: pair each value with the id it belongs to and let this function impose
/// the order. Two runs that computed the same contributions then produce the same bits
/// whatever the thread count, which is the property the golden digests rest on.
///
/// Sorting is [`slice::sort_by`], which is stable, so equal keys keep their input order.
/// Ids are unique, so that case should not arise; if a caller deliberately passes duplicate
/// keys it must supply them in a deterministic order itself.
///
/// ```
/// use v2xw_core::ids::NodeId;
/// use v2xw_core::math::sum_sorted_by_key;
/// let a = sum_sorted_by_key([(NodeId::new(2), 0.1), (NodeId::new(1), 0.2)]);
/// let b = sum_sorted_by_key([(NodeId::new(1), 0.2), (NodeId::new(2), 0.1)]);
/// assert_eq!(a.to_bits(), b.to_bits());
/// ```
pub fn sum_sorted_by_key<K: Ord>(pairs: impl IntoIterator<Item = (K, f64)>) -> f64 {
    let mut items: Vec<(K, f64)> = pairs.into_iter().collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    sum_ordered(items.into_iter().map(|(_, v)| v))
}

/// Sorts a slice of `f64` into IEEE-754 **total order** — the deterministic ordering a
/// quantile, a median or a sorted dump has to be built on.
///
/// `f64` has no [`Ord`], because `NaN` compares false against everything, so
/// `sort_by(|a, b| a.partial_cmp(b).unwrap())` panics on real data and
/// `sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))` silently produces an order
/// that depends on the input permutation. Either one makes two engines disagree about a p95
/// that is recorded and compared. [`f64::total_cmp`] is the standard's own total order and
/// is exact: it orders `-NaN < -INFINITY < … < -0.0 < +0.0 < … < +INFINITY < +NaN`,
/// distinguishing `-0.0` from `+0.0` and placing every `NaN` at one end.
///
/// The sort is [`slice::sort_by`], which is stable; under a total order two elements that
/// compare equal are bit-identical anyway, so the result is a pure function of the input
/// multiset. **Every metric that needs an order over floats uses this one**, so that a
/// crate cannot pick its own tie-break.
///
/// ```
/// use v2xw_core::math::sort_total_order;
/// let mut v = [3.0, f64::NAN, -0.0, 0.0, -1.5];
/// sort_total_order(&mut v);
/// assert_eq!(v[0], -1.5);
/// assert!(v[1].is_sign_negative() && v[1] == 0.0, "-0.0 sorts below +0.0");
/// assert_eq!(v[3], 3.0);
/// assert!(v[4].is_nan(), "NaN sorts to the top, it does not disappear");
/// ```
pub fn sort_total_order(values: &mut [f64]) {
    values.sort_by(f64::total_cmp);
}

/// The `q`-quantile of an **already sorted** slice, by one fixed interpolation rule.
///
/// The slice must be in [`sort_total_order`] order; this function does not sort, because a
/// caller that computes p50 and p95 of the same sample should sort once, and because taking
/// `&mut` here would stop two quantiles sharing one buffer.
///
/// # The rule, exactly
///
/// Linear interpolation between the two nearest order statistics, with the `q`-quantile at
/// position `h = (n − 1) · q` — the rule Hyndman & Fan call **type 7**, which is NumPy's
/// and R's default:
///
/// ```text
/// h   = (n − 1) · q
/// lo  = floor(h)
/// out = v[lo] + (h − lo) · (v[lo + 1] − v[lo])          (out = v[n − 1] when lo = n − 1)
/// ```
///
/// It is built only from `+ - *` and [`f64::floor`], all correctly rounded, so it is
/// bit-identical on every target. `q` is clamped to `[0, 1]`; `quantile_sorted(v, 0.0)` is
/// the minimum and `quantile_sorted(v, 1.0)` the maximum. An empty slice, or a `NaN` `q`,
/// yields `NaN` — the honest answer for "the median of nothing", and one that a recorded
/// artefact carries as `null` rather than as a plausible zero.
///
/// **One rule, written down, in one place**: a p95 is compared across engines and across
/// runs, and the three common conventions (nearest rank, lower order statistic, linear
/// interpolation) differ by a whole sample on the small windows a metric actually
/// aggregates. `NaN`s in the data sort to the top under the total order and will contaminate
/// a high quantile, so a provider filters them out before sampling rather than after.
///
/// ```
/// use v2xw_core::math::{quantile_sorted, sort_total_order};
/// let mut v = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
/// sort_total_order(&mut v);
/// assert_eq!(quantile_sorted(&v, 0.0), 1.0);
/// assert_eq!(quantile_sorted(&v, 0.5), 3.5);
/// assert_eq!(quantile_sorted(&v, 1.0), 9.0);
/// ```
pub fn quantile_sorted(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() || q.is_nan() {
        return f64::NAN;
    }
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let h = ((n - 1) as f64) * q.clamp(0.0, 1.0);
    let lo = h.floor();
    let idx = lo as usize;
    if idx + 1 >= n {
        return sorted[n - 1];
    }
    sorted[idx] + (h - lo) * (sorted[idx + 1] - sorted[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values that are exact in binary floating point, so any platform difference shows
    /// up as an exact-equality failure rather than as a tolerance question.
    #[test]
    fn exact_known_values() {
        assert_eq!(sin(0.0), 0.0);
        assert_eq!(cos(0.0), 1.0);
        assert_eq!(tan(0.0), 0.0);
        assert_eq!(asin(0.0), 0.0);
        assert_eq!(atan(0.0), 0.0);
        assert_eq!(atan2(0.0, 1.0), 0.0);
        assert_eq!(exp(0.0), 1.0);
        assert_eq!(exp2(10.0), 1024.0);
        assert_eq!(ln(1.0), 0.0);
        assert_eq!(log10(1000.0), 3.0);
        assert_eq!(log2(1024.0), 10.0);
        assert_eq!(pow(2.0, 10.0), 1024.0);
        assert_eq!(cbrt(27.0), 3.0);
        assert_eq!(hypot(3.0, 4.0), 5.0);
        assert_eq!(sinh(0.0), 0.0);
        assert_eq!(cosh(0.0), 1.0);
        assert_eq!(tanh(0.0), 0.0);
        assert_eq!(sqrt(4.0), 2.0);
        assert_eq!(sqrt(0.0), 0.0);
    }

    #[test]
    fn approximate_known_values() {
        let pi = core::f64::consts::PI;
        assert!((sin(pi / 6.0) - 0.5).abs() < 1e-15);
        assert!((cos(pi / 3.0) - 0.5).abs() < 1e-15);
        assert!((acos(0.0) - pi / 2.0).abs() < 1e-15);
        assert!((atan2(1.0, 1.0) - pi / 4.0).abs() < 1e-15);
        assert!((exp(1.0) - core::f64::consts::E).abs() < 1e-15);
        assert!((ln(core::f64::consts::E) - 1.0).abs() < 1e-15);
        assert!((tanh(1.0) - 0.761_594_155_955_764_9).abs() < 1e-15);
        let (s, c) = sin_cos(0.7);
        assert_eq!(s, sin(0.7));
        assert_eq!(c, cos(0.7));
    }

    /// The tie rule, the idempotence and the one place the convention is visible.
    ///
    /// Ties round away from zero, because that is symmetric about zero and is what
    /// `f64::round` — the rounding every other part of the engine uses — does.
    #[test]
    fn quantize_rounds_halves_away_from_zero() {
        assert_eq!(quantize_to(1.000_5, 1e-3), 1.001);
        assert_eq!(quantize_to(-1.000_5, 1e-3), -1.001);
        assert_eq!(quantize_to(1.000_499_9, 1e-3), 1.0);
        assert_eq!(quantize(2.5, 0), 3.0);
        assert_eq!(quantize(-2.5, 0), -3.0);
        assert_eq!(q3(1.234_56), 1.235);
        assert_eq!(q3(-1.234_56), -1.235);
        assert_eq!(LEGACY_DECIMALS, 3);
        assert_eq!(LEGACY_QUANTUM, 1e-3);
    }

    /// The decimal and the quantum spellings are one function, so a field declared "three
    /// decimals" and a field declared "quantum 1e-3" cannot drift apart.
    #[test]
    fn the_decimal_and_quantum_spellings_agree() {
        let mut s = crate::rng::RngStream::derive(
            3,
            crate::rng::RngDomain::Shadow,
            crate::rng::EntityRef::Global,
        );
        for (decimals, quantum) in [(0u8, 1.0), (2, 1e-2), (3, 1e-3), (4, 1e-4), (6, 1e-6)] {
            // The D9 grids, each the reciprocal of an exact power of ten.
            assert_eq!(1.0 / quantum, POW10[decimals as usize]);
            for _ in 0..2_000 {
                let x = s.uniform(-1e5, 1e5);
                assert_eq!(
                    quantize(x, decimals).to_bits(),
                    quantize_to(x, quantum).to_bits(),
                    "{x} at {decimals} decimals / quantum {quantum}"
                );
            }
        }
    }

    /// The property the determinism contract actually rests on: quantising twice changes
    /// nothing, and the result is detectably on its grid.
    #[test]
    fn quantization_is_idempotent_and_detectable() {
        let mut s = crate::rng::RngStream::derive(
            7,
            crate::rng::RngDomain::Shadow,
            crate::rng::EntityRef::Global,
        );
        for _ in 0..10_000 {
            let x = s.uniform(-1e6, 1e6);
            let q = q3(x);
            assert_eq!(q3(q).to_bits(), q.to_bits(), "not idempotent at {x}");
            assert!(is_quantized(q, 3), "{q} reported as off-grid");
            assert!(is_on_grid(q, 1e-3), "{q} reported as off-grid");
            assert!(
                (q - x).abs() <= 0.000_5,
                "{q} is not within half a quantum of {x}"
            );
        }
        // Raw doubles out of a transcendental are essentially never on a grid.
        assert!(!is_quantized(ln(2.0), 3));
        assert!(!is_on_grid(ln(2.0), 1e-3));
        assert!(is_quantized(1.5, 3));
        // A non-decimal grid: 0.5 dB steps, and the dB grid of D9.
        assert_eq!(quantize_to(13.26, 0.5), 13.5);
        assert_eq!(quantize_to(-13.26, 0.5), -13.5);
        assert_eq!(quantize_to(13.24, 0.5), 13.0);
        assert_eq!(
            quantize_to(0.25, 0.5),
            0.5,
            "ties go away from zero here too"
        );
        assert_eq!(quantize_to(-0.25, 0.5), -0.5);
        assert_eq!(quantize_to(-72.4551, 1e-2), -72.46, "the dB grid of D9");
    }

    /// Sentinels survive. The wire protocol spells "absent float" `NaN`, so quantising one
    /// must not turn it into a number, and the scanner must not flag it.
    #[test]
    fn non_finite_values_pass_through() {
        assert!(quantize(f64::NAN, 3).is_nan());
        assert!(quantize_to(f64::NAN, 0.5).is_nan());
        assert_eq!(quantize(f64::INFINITY, 3), f64::INFINITY);
        assert_eq!(quantize_to(f64::NEG_INFINITY, 1e-3), f64::NEG_INFINITY);
        assert!(is_quantized(f64::NAN, 3));
        assert!(is_on_grid(f64::INFINITY, 1e-3));
        // A magnitude that overflows the scale factor is already coarser than any grid,
        // so it comes back unchanged rather than as an infinity.
        assert_eq!(quantize(f64::MAX, 3), f64::MAX);
        assert_eq!(quantize_to(f64::MAX, 1e-6), f64::MAX);
        assert!(is_on_grid(f64::MAX, 1e-3));
        assert!(is_on_grid(1e300, 1e-3));
        assert_eq!(q3(q3(1e300)), q3(1e300));
    }

    /// The documented disagreement with Python's `round`, pinned so that it is a decision
    /// and not a surprise: D9 §2 retires digest equality with the legacy corpus and D9 §3
    /// compares floats within 1e-9, so rounding the scaled binary value is sound.
    #[test]
    fn near_ties_differ_from_pythons_decimal_round_by_at_most_one_quantum() {
        // Python: round(2.675, 2) == 2.67, because 2.675 is really 2.674999999999999822.
        assert_eq!(quantize_to(2.675, 1e-2), 2.68);
        assert_eq!(quantize(2.675, 2), 2.68);
        assert!(
            (quantize(2.675, 2) - 2.67).abs() <= 1e-2 + 1e-12,
            "the disagreement is one quantum, never more"
        );
        // Away from a tie the two agree exactly.
        // π on the three-decimal grid, the way a heading is written out.
        // (`3.142` *is* an approximation of π — that is the assertion.)
        #[allow(clippy::approx_constant)]
        {
            assert_eq!(quantize(core::f64::consts::PI, 3), 3.142);
        }
        assert_eq!(quantize(123_456.789_123_5, 3), 123_456.789);
        assert_eq!(quantize(0.1 + 0.2, 3), 0.3);
    }

    #[test]
    #[should_panic(expected = "exceeds the 17")]
    fn quantize_rejects_impossible_precision() {
        let _ = quantize(1.0, 18);
    }

    #[test]
    #[should_panic(expected = "strictly positive")]
    fn quantize_to_rejects_a_zero_quantum() {
        let _ = quantize_to(1.0, 0.0);
    }

    /// The compensation is the point: a naive left-to-right sum returns the wrong answer
    /// for every one of these, so the test fails if `sum_ordered` is `iter().sum()`.
    #[test]
    fn ordered_summation_recovers_what_a_naive_sum_loses() {
        assert_eq!([1e16, 1.0, -1e16].iter().sum::<f64>(), 0.0);
        assert_eq!(sum_ordered([1e16, 1.0, -1e16]), 1.0);

        // One large accumulator and many small addends: the classic case, and the shape of
        // an interference sum or a long metric window.
        let mut v = vec![1e17];
        v.extend(std::iter::repeat_n(1.0, 10_000));
        assert_eq!(sum_ordered(v.iter().copied()), 1e17 + 10_000.0);
        assert_ne!(v.iter().sum::<f64>(), 1e17 + 10_000.0);

        // 0.1 ten times: naive summation drifts, compensated summation lands on 1.0.
        assert_ne!(std::iter::repeat_n(0.1, 10).sum::<f64>(), 1.0);
        assert_eq!(sum_ordered(std::iter::repeat_n(0.1, 10)), 1.0);
    }

    /// The degenerate inputs, including the non-finite ones the wire protocol uses as
    /// sentinels: an infinity must stay an infinity rather than becoming `NaN` in the
    /// compensation term.
    #[test]
    fn ordered_summation_handles_the_edges() {
        assert_eq!(sum_ordered(std::iter::empty()), 0.0);
        assert_eq!(sum_ordered([42.0]), 42.0);
        assert_eq!(sum_ordered([f64::INFINITY, 1.0]), f64::INFINITY);
        assert_eq!(sum_ordered([1.0, f64::NEG_INFINITY]), f64::NEG_INFINITY);
        assert!(sum_ordered([f64::INFINITY, f64::NEG_INFINITY]).is_nan());
        assert!(sum_ordered([1.0, f64::NAN]).is_nan());
        // -0.0 + 0.0 is +0.0, as IEEE-754 requires.
        assert_eq!(sum_ordered([-0.0, 0.0]).to_bits(), 0.0_f64.to_bits());
    }

    /// The property the parallel phases need: the same contributions in any arrival order
    /// reduce to the same **bits** once the ids impose the order.
    #[test]
    fn id_sorted_summation_is_independent_of_arrival_order() {
        use crate::ids::NodeId;

        let mut s = crate::rng::RngStream::derive(
            11,
            crate::rng::RngDomain::Shadow,
            crate::rng::EntityRef::Global,
        );
        let contributions: Vec<(NodeId, f64)> = (0..500)
            .map(|i| (NodeId::new(i), s.normal(0.0, 1e6)))
            .collect();

        let reference = sum_sorted_by_key(contributions.iter().copied());

        // Every rotation of the arrival order stands in for a different chunking of the
        // same phase-parallel map.
        for split in [1usize, 7, 100, 499] {
            let mut shuffled = contributions[split..].to_vec();
            shuffled.extend_from_slice(&contributions[..split]);
            assert_ne!(
                shuffled[0].0, contributions[0].0,
                "the rotation must actually reorder the input"
            );
            assert_eq!(
                sum_sorted_by_key(shuffled.iter().copied()).to_bits(),
                reference.to_bits(),
                "arrival order changed the reduction at split {split}"
            );
        }

        // …and a reduction that does not impose the order genuinely depends on it, which
        // is the whole reason the helper exists.
        assert_ne!(
            [1e16, 1.0, -1e16].iter().sum::<f64>(),
            [-1e16, 1e16, 1.0].iter().sum::<f64>(),
            "naive summation is order-dependent"
        );
        assert_eq!(
            sum_sorted_by_key([
                (NodeId::new(0), 1e16),
                (NodeId::new(1), 1.0),
                (NodeId::new(2), -1e16)
            ]),
            sum_sorted_by_key([
                (NodeId::new(2), -1e16),
                (NodeId::new(0), 1e16),
                (NodeId::new(1), 1.0)
            ]),
        );

        assert_eq!(sum_sorted_by_key(Vec::<(NodeId, f64)>::new()), 0.0);
    }

    /// The digest form of the quantiser: the integer multiple, with the same rounding as
    /// [`quantize_to`] and a distinct value for each sentinel.
    #[test]
    fn grid_index_is_the_hashed_integer() {
        assert_eq!(grid_index(12.346, 1e-3), 12_346);
        assert_eq!(grid_index(-12.346, 1e-3), -12_346);
        assert_eq!(grid_index(0.0, 1e-3), 0);
        assert_eq!(grid_index(-0.0, 1e-3), 0);
        assert_eq!(grid_index(40.744, 1e-7), 407_440_000);

        // Same rounding as the quantiser it accompanies — ties away from zero — so a value
        // recorded through one and digested through the other cannot disagree.
        for x in [0.0015, -0.0015, 2.675, 1.0005, -1.0005, 1e-4, 123.456_789] {
            let q = 1e-3;
            assert_eq!(
                grid_index(x, q),
                grid_index(quantize_to(x, q), q),
                "{x} disagrees between the quantiser and the digest"
            );
            assert_eq!(grid_index(x, q), (x * 1000.0).round() as i64);
        }

        // The two neighbours of a grid point digest identically: that is the whole point.
        assert_eq!(
            grid_index(12.346_000_000_000_001, 1e-3),
            grid_index(12.345_999_999_999_999, 1e-3)
        );

        // Sentinels get distinct extremes, so nothing finite can collide with one.
        assert_eq!(grid_index(f64::NAN, 1e-3), i64::MIN);
        assert_eq!(grid_index(f64::NEG_INFINITY, 1e-3), i64::MIN + 1);
        assert_eq!(grid_index(f64::INFINITY, 1e-3), i64::MAX);
        assert_eq!(grid_index(1e300, 1e-3), i64::MAX);
        assert_eq!(grid_index(-1e300, 1e-3), i64::MIN + 1);
    }

    #[test]
    #[should_panic(expected = "strictly positive")]
    fn grid_index_refuses_a_zero_quantum() {
        let _ = grid_index(1.0, 0.0);
    }

    /// `f64` has no `Ord`, so every metric that ranks floats needs one ordering, defined
    /// once. Two engines that picked their own tie-breaks would disagree on a p95.
    #[test]
    fn the_total_order_is_total_and_the_quantile_rule_is_fixed() {
        let mut v = [
            3.0,
            f64::NAN,
            -0.0,
            0.0,
            -1.5,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        sort_total_order(&mut v);
        assert_eq!(v[0], f64::NEG_INFINITY);
        assert_eq!(v[1], -1.5);
        assert!(v[2].is_sign_negative() && v[2] == 0.0, "-0.0 before +0.0");
        assert!(v[3].is_sign_positive() && v[3] == 0.0);
        assert_eq!(v[4], 3.0);
        assert_eq!(v[5], f64::INFINITY);
        assert!(v[6].is_nan(), "NaN sorts to the top, it does not vanish");

        // The order is a pure function of the multiset: any input permutation, same bits.
        let base = [5.0, 1.0, -2.5, 1.0, 0.0, 9.75];
        let mut reference = base;
        sort_total_order(&mut reference);
        for rotation in 1..base.len() {
            let mut rotated = base;
            rotated.rotate_left(rotation);
            sort_total_order(&mut rotated);
            assert_eq!(rotated, reference, "rotation {rotation} changed the order");
        }

        // The documented interpolation rule (Hyndman & Fan type 7), worked through.
        let mut s = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
        sort_total_order(&mut s); // 1, 1, 2, 3, 4, 5, 6, 9
        assert_eq!(quantile_sorted(&s, 0.0), 1.0, "p0 is the minimum");
        assert_eq!(quantile_sorted(&s, 1.0), 9.0, "p100 is the maximum");
        assert_eq!(quantile_sorted(&s, 0.5), 3.5, "h = 3.5 → 3 + 0.5·(4 − 3)");
        let p95 = quantile_sorted(&s, 0.95); // h = 6.65 → 6 + 0.65·(9 − 6)
        assert!((p95 - 7.95).abs() < 1e-12, "p95 = {p95}");
        let h = ((s.len() - 1) as f64) * 0.95;
        assert_eq!(p95, s[6] + (h - 6.0) * (s[7] - s[6]), "the rule, verbatim");
        assert_eq!(quantile_sorted(&s, 0.25), 1.75);

        // Degenerate inputs answer rather than panic, and say "I do not know" as NaN.
        assert!(quantile_sorted(&[], 0.5).is_nan());
        assert!(quantile_sorted(&s, f64::NAN).is_nan());
        assert_eq!(quantile_sorted(&[7.5], 0.95), 7.5);
        assert_eq!(quantile_sorted(&s, -1.0), 1.0, "q is clamped to [0, 1]");
        assert_eq!(quantile_sorted(&s, 2.0), 9.0);

        // Bit-identical across runs, which is what a recorded p95 needs.
        assert_eq!(
            quantile_sorted(&s, 0.95).to_bits(),
            quantile_sorted(&s, 0.95).to_bits()
        );
    }

    /// Guards the decision itself: these must be the `libm` crate's results, so that a
    /// future refactor to `f64::sin` changes the test, not just the behaviour.
    #[test]
    fn delegates_to_the_libm_crate() {
        for x in [0.1_f64, 1.0, 2.5, -3.75, 1e6] {
            assert_eq!(sin(x), libm::sin(x));
            assert_eq!(cos(x), libm::cos(x));
            assert_eq!(exp(x), libm::exp(x));
            assert_eq!(pow(x.abs() + 0.5, 1.5), libm::pow(x.abs() + 0.5, 1.5));
        }
    }
}
