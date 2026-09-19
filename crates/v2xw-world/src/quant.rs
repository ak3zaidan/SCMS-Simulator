//! Writer-side quantisation — the one place a float is put on its grid (D9).
//!
//! `docs/design/12-build-decisions.md` D9 is binding: *no floating-point value reaches a
//! recorded, exported or digested artefact in raw IEEE-754 form.* The evidence is in
//! ADR 0004 and `docs/design/research/legacy-digest-forensics.md`: the legacy engine's
//! frozen digests are unreproducible across architectures because exactly one field
//! escaped rounding, and its coordinates descended from `sin`/`cos`, which no two
//! platform libms round identically. Quantising at the writer kills the whole bug class
//! in about twenty lines, and keeps killing it if the maths library, the compiler or the
//! language ever changes.
//!
//! # How this crate applies it
//!
//! `v2xw-world` goes one step further than "quantise at the writer": **a `World` is
//! quantised when it is constructed**, by [`crate::model::Lane::new`] and
//! [`crate::model::WorldBuilder::build`]. Every float in the model therefore already
//! sits on its grid, so
//!
//! * every writer's quantisation step is idempotent — a writer that forgot to quantise
//!   would still emit on-grid values, so the guarantee does not depend on a future
//!   author remembering;
//! * the content hash ([`crate::hash`]) hashes the same integers the exporters write;
//! * the native round-trip ([`crate::serde_native`]) is exact, because writing a value
//!   that is already on its grid changes nothing.
//!
//! The scanning test D9 requires lives in [`crate::model::World::scan_exported_floats`]
//! plus the `d9_*` tests of [`crate::serde_vwp`]: they walk every float that reaches an
//! artefact and fail if one is off its grid — or is not finite at all, which
//! [`is_on_grid`] also reports, because a `NaN` serialises two different ways.
//!
//! # One quantiser, not two
//!
//! [`quantise`] and [`grid_index`] are `v2xw_core::math`'s, forwarded and re-exported:
//! D9 names **one** central writer-side encoder, and a second copy in this crate is a
//! second set of bytes waiting to happen. The module keeps only what is genuinely this
//! crate's: the grid table below, [`quantise_f32`], [`quantise_vec3`], the `f32` limit
//! and the export-side reading of [`is_on_grid`].
//!
//! # The grid table
//!
//! | Quantity | Constant | Quantum | Where the quantum comes from |
//! |---|---|---|---|
//! | position, length, width | [`Q_POSITION_M`] | 1e-3 m | D9 (metres default to 1e-3, the legacy convention) |
//! | height, altitude | [`Q_HEIGHT_M`] | 1e-3 m | D9 |
//! | speed | [`Q_SPEED_MPS`] | 1e-3 m/s | D9 (metres and seconds default to 1e-3) |
//! | duration | [`Q_TIME_S`] | 1e-3 s | D9 |
//! | angle | [`Q_ANGLE_RAD`] | 1e-6 rad | D9 / the task's field table (≈ 1 mm of lateral error at 1 km) |
//! | gain, loss | [`Q_DB`] | 1e-2 dB | D9 (dB to 1e-2) |
//! | geodetic latitude/longitude | [`Q_DEGREES`] | 1e-7 ° | this crate: 1e-7° is 11 mm of latitude, the same order as [`Q_POSITION_M`]; D9's table has no entry for degrees, so this one is declared here and recorded in the world provenance |
//!
//! A world stores no angles (lane headings are derived from the quantised centrelines on
//! demand), so [`Q_ANGLE_RAD`] is unused by this crate's own artefacts; it is declared
//! here because D9 wants one table, and the pose writers of `v2xw-record` and
//! `v2xw-server` quantise against it.

/// Quantum of every position, length, width and lane offset: 1 mm.
pub const Q_POSITION_M: f64 = 1e-3;

/// Quantum of every height and altitude: 1 mm.
pub const Q_HEIGHT_M: f64 = 1e-3;

/// Quantum of every speed: 1 mm/s.
pub const Q_SPEED_MPS: f64 = 1e-3;

/// Quantum of every duration expressed in seconds: 1 ms.
pub const Q_TIME_S: f64 = 1e-3;

/// Quantum of every angle: 1 µrad.
pub const Q_ANGLE_RAD: f64 = 1e-6;

/// Quantum of every gain, loss or level in decibels: 0.01 dB.
pub const Q_DB: f64 = 1e-2;

/// Quantum of every geodetic latitude or longitude in degrees: 1e-7° (≈ 11 mm).
pub const Q_DEGREES: f64 = 1e-7;

/// Rounds `value` to the nearest multiple of `quantum`.
///
/// **This is [`v2xw_core::math::quantize_to`]**, and nothing else: D9 names one central
/// writer-side encoder, so this function forwards to it rather than reimplementing the
/// rounding. The only thing it adds is the quantum guard below, which keeps the
/// non-panicking contract this module has always published.
///
/// Ties round away from zero (`f64::round`), which is symmetric about zero and is what
/// every other rounding in the engine does. `quantum` must be positive and finite;
/// a non-positive or non-finite quantum returns `value` unchanged, because a "grid" of
/// zero or NaN has no nearest point. (`v2xw_core::math::quantize_to` asserts instead,
/// which is right for a core primitive and wrong here: this quantiser is called from a
/// writer, per field, with a quantum that comes from the grid table above.)
///
/// Non-finite values pass through untouched: the wire protocol uses `NaN` as the
/// "absent" sentinel for a float (docs/protocol/vwp-v1.md §0), and quantising a sentinel
/// would destroy it. A magnitude so large that `value / quantum` overflows to infinity
/// also passes through unchanged — consecutive `f64`s there are further apart than the
/// grid, so the value already *is* its own nearest grid point. That guard used to be
/// missing from this crate's private copy of the quantiser, which returned `±∞` for
/// `quantise(1e306, 1e-3)` where core returned `1e306`: two writers in two crates
/// disagreeing about the same field, which is exactly the bug class D9 exists to close.
///
/// The result is the nearest `f64` to `k · quantum` for the integer `k`, so it is not
/// *exactly* a multiple of `quantum` in binary arithmetic — no decimal grid is. What
/// matters, and what [`is_on_grid`] checks, is that the operation is **idempotent and
/// platform-independent**: `quantise(quantise(v, q), q) == quantise(v, q)` bit for bit
/// on every target, because it is built from `/`, `round` and `*`, all of which IEEE-754
/// requires to be correctly rounded. That is the whole point of D9: two engines that
/// quantise the same way agree bit for bit, whatever their maths libraries do.
///
/// ```
/// use v2xw_world::quant::{Q_POSITION_M, quantise};
/// assert_eq!(quantise(1.000_499_9, Q_POSITION_M), 1.0);
/// assert_eq!(quantise(1.000_5, Q_POSITION_M), 1.001);
/// assert_eq!(quantise(-1.000_5, Q_POSITION_M), -1.001);
/// assert!(quantise(f64::NAN, Q_POSITION_M).is_nan());
/// // The same numbers as the core quantiser, everywhere, including the overflow case.
/// assert_eq!(quantise(1e306, Q_POSITION_M), v2xw_core::math::quantize_to(1e306, 1e-3));
/// ```
#[inline]
pub fn quantise(value: f64, quantum: f64) -> f64 {
    if quantum <= 0.0 || !quantum.is_finite() {
        return value;
    }
    v2xw_core::math::quantize_to(value, quantum)
}

/// True if `value` is a finite value that [`quantise`] would leave untouched — the
/// predicate the D9 scan asserts for every float that reaches an artefact.
///
/// **A non-finite value is not on the grid.** This is the one place where this module
/// deliberately says something different from [`v2xw_core::math::is_on_grid`], and the
/// difference is the question being asked:
///
/// * core's predicate answers *"would quantising change this?"*, so `NaN` and `±∞`
///   answer yes-it-is-on-grid, because the quantiser passes them through;
/// * this predicate answers *"may this value be exported?"*, and the answer for a
///   non-finite value is no.
///
/// The reason is the two writers. docs/protocol/vwp-v1.md §0 makes `NaN` the float
/// "absent" sentinel, so the binary payload can store it — but §4.6 requires the JSON
/// form to be "a direct transcription" of the binary, and JSON has no `NaN`:
/// [`crate::serde_vwp::to_json_with_hash`] turns one into `null`, and a `1e306` height
/// into `null` as well. A world carrying a non-finite geometry float therefore has two
/// non-interchangeable serialisations, so [`crate::model::World::validate`] rejects it
/// outright ([`crate::WorldError::NonFinite`]) instead of letting the D9 scan wave it
/// through. Until §4.6 gains an explicit encoding for the sentinel, "no non-finite float
/// in a world" is the only rule both writers can keep.
///
/// ```
/// use v2xw_world::quant::{Q_POSITION_M, is_on_grid};
/// assert!(is_on_grid(1.001, Q_POSITION_M));
/// assert!(!is_on_grid(1.000_1, Q_POSITION_M));
/// assert!(!is_on_grid(f64::NAN, Q_POSITION_M));
/// assert!(!is_on_grid(f64::INFINITY, Q_POSITION_M));
/// // Core answers the other question, and says yes:
/// assert!(v2xw_core::math::is_on_grid(f64::NAN, 1e-3));
/// ```
#[inline]
pub fn is_on_grid(value: f64, quantum: f64) -> bool {
    value.is_finite() && quantise(value, quantum).to_bits() == value.to_bits()
}

/// Quantises `value` and narrows it to `f32`, the form the wire protocol uses for
/// geometry columns (docs/protocol/vwp-v1.md §4.3, §4.4).
///
/// The narrowing is IEEE-754 round-to-nearest and therefore itself deterministic, but
/// `f32` has 24 bits of significand, so it can only *hold* a 1 mm grid out to
/// [`F32_MM_GRID_LIMIT_M`] = 16 384 m from the origin. Beyond that the payload's own
/// resolution is coarser than the grid and the guarantee weakens to "the nearest `f32`
/// to the quantised value" — which is still bit-identical on every platform, which is
/// what determinism needs. The world's origin is its bounding box's south-west corner
/// (D6), so a world stays inside the limit up to a 16 km extent; bigger worlds are
/// flagged by [`crate::serde_vwp::write`]'s precision warning rather than silently
/// losing millimetres.
#[inline]
pub fn quantise_f32(value: f64, quantum: f64) -> f32 {
    quantise(value, quantum) as f32
}

/// The coordinate magnitude beyond which an `f32` cannot hold a 1 mm grid: 2^14 m.
///
/// For `x` in `[2^13, 2^14)` the spacing of `f32` values is `2^13 · 2^-23 = 9.77e-4 m`,
/// so narrowing a millimetre-quantised value moves it by at most 4.88e-4 m — under half a
/// millimetre, which is why widening it again returns the same millimetre. At `2^14` the
/// spacing doubles to 1.95e-3 m and the grid no longer survives; the first value that
/// fails, found by scanning, is 16 384.009 m. The
/// `f32_narrowing_keeps_the_millimetre_grid_inside_the_documented_limit` test checks both
/// halves of that claim.
pub const F32_MM_GRID_LIMIT_M: f64 = 16_384.0;

/// Quantises a [`v2xw_core::geom::Vec3`]: `x` and `y` on [`Q_POSITION_M`], `z` on
/// [`Q_HEIGHT_M`].
#[inline]
pub fn quantise_vec3(v: v2xw_core::geom::Vec3) -> v2xw_core::geom::Vec3 {
    v2xw_core::geom::Vec3::new(
        quantise(v.x, Q_POSITION_M),
        quantise(v.y, Q_POSITION_M),
        quantise(v.z, Q_HEIGHT_M),
    )
}

/// The integer multiple of `quantum` that a value quantises to, for hashing —
/// re-exported from [`v2xw_core::math`], which is where D9 puts it.
///
/// [`crate::hash`] hashes this integer rather than the `f64`, so the content hash cannot
/// depend on the representation error of `k · quantum` at all. Values outside the `i64`
/// range, and non-finite values, map to documented sentinels: `i64::MIN` for `NaN`,
/// `i64::MIN + 1` for `-∞`, `i64::MAX` for `+∞`, and saturation at the ends otherwise.
/// A coordinate anywhere near those bounds (9.2e15 mm = 9.2e12 m, 24 times the distance
/// to the Moon) is not a coordinate.
///
/// This crate invented the helper and core adopted it, with the note that it "belongs
/// here, with the quantiser whose rounding it must match, so that the two cannot drift".
/// A re-export is the only way to keep that promise: there is one implementation, and
/// `grid_index` and [`quantise`] round the same way because they are the same code.
pub use v2xw_core::math::grid_index;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_to_the_nearest_multiple() {
        assert_eq!(quantise(0.0, Q_POSITION_M), 0.0);
        assert_eq!(quantise(0.000_49, Q_POSITION_M), 0.0);
        assert_eq!(quantise(0.000_51, Q_POSITION_M), 0.001);
        assert_eq!(quantise(12.345_678, Q_POSITION_M), 12.346);
        assert_eq!(quantise(-12.345_678, Q_POSITION_M), -12.346);
        assert_eq!(quantise(0.123_456_789, Q_ANGLE_RAD), 0.123_457);
        assert_eq!(quantise(7.123_456, Q_DB), 7.12);
    }

    #[test]
    fn is_idempotent_over_a_wide_range() {
        // Idempotence is what makes two independent writers agree, so it is checked over
        // the whole coordinate range a world can hold rather than at a few points.
        let mut x = 1.0e-4_f64;
        while x < 1.0e7 {
            for q in [Q_POSITION_M, Q_ANGLE_RAD, Q_DB, Q_DEGREES] {
                let once = quantise(x, q);
                assert_eq!(quantise(once, q), once, "x = {x}, q = {q}");
                assert!(is_on_grid(once, q), "x = {x}, q = {q}");
                let neg = quantise(-x, q);
                assert_eq!(quantise(neg, q), neg, "x = {x}, q = {q}");
            }
            x *= 1.000_37;
        }
    }

    #[test]
    fn leaves_sentinels_alone() {
        assert!(quantise(f64::NAN, Q_POSITION_M).is_nan());
        assert_eq!(quantise(f64::INFINITY, Q_POSITION_M), f64::INFINITY);
        // A nonsense quantum is a no-op rather than a panic or a NaN.
        assert_eq!(quantise(1.234, 0.0), 1.234);
        assert_eq!(quantise(1.234, -1.0), 1.234);
        assert_eq!(quantise(1.234, f64::NAN), 1.234);
    }

    /// R5: `is_on_grid` used to answer *true* for every non-finite value, which made the
    /// D9 scan in `World::validate` blind to a `NaN` or an infinity — the one thing the
    /// scan exists to catch, because the binary writer stores a `NaN` and the JSON writer
    /// turns it into `null`.
    #[test]
    fn a_non_finite_value_is_never_on_the_grid() {
        for q in [Q_POSITION_M, Q_HEIGHT_M, Q_DEGREES, Q_DB, Q_TIME_S] {
            assert!(!is_on_grid(f64::NAN, q));
            assert!(!is_on_grid(f64::INFINITY, q));
            assert!(!is_on_grid(f64::NEG_INFINITY, q));
        }
        // And the core predicate still answers its own question, so the two are
        // deliberately different rather than accidentally out of step.
        assert!(v2xw_core::math::is_on_grid(f64::NAN, Q_POSITION_M));
    }

    /// R6: this module used to carry a private second copy of the core quantiser without
    /// core's overflow guard, so the two disagreed for large magnitudes —
    /// `quantise(1e306, 1e-3)` was `+∞` where `quantize_to` returned `1e306`. Two writers
    /// quantising one field to different bytes is precisely what D9 forbids.
    #[test]
    fn agrees_with_the_core_quantiser_bit_for_bit() {
        let quanta = [
            Q_POSITION_M,
            Q_HEIGHT_M,
            Q_SPEED_MPS,
            Q_TIME_S,
            Q_ANGLE_RAD,
            Q_DB,
            Q_DEGREES,
        ];
        let mut values = vec![
            0.0,
            -0.0,
            1e-9,
            0.000_5,
            12.345_678,
            -12.345_678,
            1.7e305,
            1e306,
            -1e306,
            f64::MAX,
            f64::MIN_POSITIVE,
        ];
        let mut x = 1.0e-4_f64;
        while x < 1.0e9 {
            values.push(x);
            values.push(-x);
            x *= 1.7;
        }
        for q in quanta {
            for v in &values {
                assert_eq!(
                    quantise(*v, q).to_bits(),
                    v2xw_core::math::quantize_to(*v, q).to_bits(),
                    "value = {v}, quantum = {q}"
                );
            }
            // The specific divergence that was measured.
            assert!(quantise(1e306, q).is_finite(), "quantum = {q}");
        }
        // And the grid integer is core's, so the digest and the writer cannot drift.
        assert_eq!(grid_index(12.346, Q_POSITION_M), 12_346);
    }

    #[test]
    fn detects_off_grid_values() {
        assert!(!is_on_grid(0.000_5, Q_POSITION_M));
        assert!(!is_on_grid(1.000_000_1, Q_POSITION_M));
        assert!(is_on_grid(1.001, Q_POSITION_M));
    }

    #[test]
    fn f32_narrowing_keeps_the_millimetre_grid_inside_the_documented_limit() {
        let mut x = 0.001_f64;
        while x < F32_MM_GRID_LIMIT_M {
            let q = quantise(x, Q_POSITION_M);
            let wide = f64::from(quantise_f32(x, Q_POSITION_M));
            assert_eq!(quantise(wide, Q_POSITION_M), q, "x = {x}");
            x *= 1.000_37;
        }
        // And the limit is real: just past it the grid no longer survives. 16 384.009 m
        // is the first millimetre that `f32` cannot hold, found by scanning.
        let beyond = 16_384.009;
        assert_ne!(
            quantise(f64::from(quantise_f32(beyond, Q_POSITION_M)), Q_POSITION_M),
            quantise(beyond, Q_POSITION_M)
        );
    }

    #[test]
    fn grid_index_is_the_hashed_integer() {
        assert_eq!(grid_index(12.346, Q_POSITION_M), 12_346);
        assert_eq!(grid_index(-12.346, Q_POSITION_M), -12_346);
        assert_eq!(grid_index(0.0, Q_POSITION_M), 0);
        assert_eq!(grid_index(f64::NAN, Q_POSITION_M), i64::MIN);
        assert_eq!(grid_index(f64::INFINITY, Q_POSITION_M), i64::MAX);
        assert_eq!(grid_index(f64::NEG_INFINITY, Q_POSITION_M), i64::MIN + 1);
        assert_eq!(grid_index(1e300, Q_POSITION_M), i64::MAX);
        assert_eq!(grid_index(-1e300, Q_POSITION_M), i64::MIN + 1);
        // The integer is insensitive to representation error either side of the grid.
        assert_eq!(
            grid_index(12.346_000_000_000_001, Q_POSITION_M),
            grid_index(12.345_999_999_999_999, Q_POSITION_M)
        );
    }

    #[test]
    fn quantises_vectors_component_wise() {
        let v = quantise_vec3(v2xw_core::geom::Vec3::new(1.000_4, -2.000_6, 3.000_5));
        assert_eq!(v, v2xw_core::geom::Vec3::new(1.0, -2.001, 3.001));
    }
}
