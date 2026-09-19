//! The deterministic numeric primitives this crate needs and [`v2xw_core::math`] does
//! not carry: the error function, the dB/linear conversions, and the id-ordered power
//! sum every interference reduction goes through.
//!
//! # Why this module names `libm` directly
//!
//! ADR 0003 and ADR 0004 §4 forbid the standard library's transcendentals, because
//! `f64::exp` and friends delegate to the platform libm whose precision the Rust
//! standard library documents as varying "by platform, Rust version, and can even differ
//! within the same execution". Every transcendental in the engine therefore goes through
//! [`v2xw_core::math`], which re-exports the pure-Rust `libm` crate.
//!
//! The NIST PER model of 04-models.md §4.7 is written in terms of the Gaussian tail
//! `Q(x)`, i.e. `erfc`, and `v2xw_core::math` re-exports everything from `sin` to
//! `hypot` but not the error function. This module is the one place in the crate that
//! reaches for `libm::erfc` directly. That keeps the property the ADR is about — no
//! platform libm is called anywhere in this crate — while the missing re-export is
//! recorded as the one-line change to make in the contract crate when it is next open
//! for edits (`v2xw_core::math::{erf, erfc}`). Every other transcendental here and in the
//! rest of the crate comes from [`v2xw_core::math`].
//!
//! # Ordered reductions
//!
//! [`sum_powers_mw`] exists because an interference sum is a floating-point reduction
//! over an unordered contributor set, and floating-point addition is not associative: the
//! same interferers summed in two different orders give two different last bits, and a
//! SINR one bit apart can land on either side of a PER threshold. It sorts by
//! [`NodeId`] and then reduces with [`v2xw_core::math::sum_ordered`], which is the rule
//! 02-architecture.md §6.3 states for exactly this sum.

use v2xw_core::ids::NodeId;
use v2xw_core::math;

/// Speed of light in vacuum, m/s (the defined SI value).
pub const SPEED_OF_LIGHT_M_S: f64 = 299_792_458.0;

/// The quantum every decibel value that reaches a recorded or exported artefact is
/// rounded to at the writer (build decision D9): a thousandth of a dB, which is three
/// orders of magnitude finer than any measurement this crate models.
pub const Q_DB: f64 = 1e-3;

/// The quantum for a dimensionless ratio (CBR, PER, a reception probability).
pub const Q_RATIO: f64 = 1e-6;

/// The Gauss error function, from the pure-Rust `libm` crate (see the module docs).
#[must_use]
pub fn erf(x: f64) -> f64 {
    libm::erf(x)
}

/// The complementary error function `1 - erf(x)`, from the pure-Rust `libm` crate.
///
/// Computed as its own function rather than as `1 - erf(x)` because the tail is where
/// the PER model lives: at `x = 3` the complement is 2.2e-5, and `1 - erf(3)` loses
/// eleven of its significant digits to cancellation.
#[must_use]
pub fn erfc(x: f64) -> f64 {
    libm::erfc(x)
}

/// The Gaussian tail probability `Q(x) = P(X > x)` for a standard normal `X`.
///
/// `Q(x) = 0.5 * erfc(x / sqrt(2))`. This is the function the Pei and Henderson BER
/// equations of 04-models.md §4.7 are written in.
#[must_use]
pub fn q_function(x: f64) -> f64 {
    0.5 * erfc(x / core::f64::consts::SQRT_2)
}

/// A power ratio in dB as a linear ratio: `10^(db/10)`.
#[must_use]
pub fn db_to_linear(db: f64) -> f64 {
    math::pow(10.0, db / 10.0)
}

/// A linear power ratio in dB: `10 * log10(x)`.
///
/// Zero maps to negative infinity, which is the honest answer and the one the noise and
/// interference arithmetic wants: a contributor of zero power adds nothing.
#[must_use]
pub fn linear_to_db(x: f64) -> f64 {
    10.0 * math::log10(x)
}

/// dBm as milliwatts.
#[must_use]
pub fn dbm_to_mw(dbm: f64) -> f64 {
    math::pow(10.0, dbm / 10.0)
}

/// Milliwatts as dBm.
#[must_use]
pub fn mw_to_dbm(mw: f64) -> f64 {
    10.0 * math::log10(mw)
}

/// The wavelength of `f_hz` in metres.
#[must_use]
pub fn wavelength_m(f_hz: f64) -> f64 {
    SPEED_OF_LIGHT_M_S / f_hz
}

/// The sum of a set of powers in milliwatts, reduced in [`NodeId`] order.
///
/// The contributor list is sorted by id and then summed with
/// [`v2xw_core::math::sum_ordered`], so the result does not depend on the order the
/// caller collected the interferers in (02-architecture.md §6.3, and the
/// `interference_sum_is_order_independent` test). Contributors are *not* deduplicated:
/// one node transmitting two overlapping frames at a receiver contributes twice, which
/// is physically what happens.
#[must_use]
pub fn sum_powers_mw(contributors: &[(NodeId, f64)]) -> f64 {
    let mut sorted: Vec<(NodeId, f64)> = contributors.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    math::sum_ordered(sorted.iter().map(|(_, p)| *p))
}

/// A decibel value rounded to the writer-side quantum [`Q_DB`].
#[must_use]
pub fn q_db(x: f64) -> f64 {
    math::quantize_to(x, Q_DB)
}

/// A dimensionless ratio rounded to the writer-side quantum [`Q_RATIO`].
#[must_use]
pub fn q_ratio(x: f64) -> f64 {
    math::quantize_to(x, Q_RATIO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_function_matches_its_known_values() {
        // Q(0) = 1/2, Q(1) = 0.158655…, Q(2) = 0.022750…, Q(3) = 0.001349…
        assert!((q_function(0.0) - 0.5).abs() < 1e-12);
        assert!((q_function(1.0) - 0.158_655_253_931_457).abs() < 1e-12);
        assert!((q_function(2.0) - 0.022_750_131_948_179).abs() < 1e-12);
        assert!((q_function(3.0) - 0.001_349_898_031_630).abs() < 1e-12);
    }

    #[test]
    fn db_round_trips_through_linear() {
        for db in [-120.0, -85.0, -3.0, 0.0, 6.6, 23.0] {
            let back = linear_to_db(db_to_linear(db));
            assert!((back - db).abs() < 1e-9, "{db} -> {back}");
        }
    }

    #[test]
    fn interference_sum_is_order_independent() {
        let a = NodeId::new(7);
        let b = NodeId::new(2);
        let c = NodeId::new(19);
        let forward = [(a, 1e-9), (b, 3.5e-7), (c, 2.25e-11)];
        let mut reversed = forward;
        reversed.reverse();
        let mut shuffled = [forward[1], forward[2], forward[0]];
        assert_eq!(
            sum_powers_mw(&forward).to_bits(),
            sum_powers_mw(&reversed).to_bits()
        );
        shuffled.swap(0, 2);
        assert_eq!(
            sum_powers_mw(&forward).to_bits(),
            sum_powers_mw(&shuffled).to_bits()
        );
    }

    #[test]
    fn wavelength_at_the_its_carrier_is_about_five_centimetres() {
        // 04-models.md §3: 5.9 GHz, lambda about 0.0508 m.
        let lambda = wavelength_m(5.9e9);
        assert!((lambda - 0.0508).abs() < 1e-4, "lambda = {lambda}");
    }
}
