//! The declared grid of every float this crate writes (build decision D9).
//!
//! D9: "No floating-point value reaches a recorded, exported or digested artefact in raw
//! IEEE-754 form. One central writer-side encoder quantises each float to its field's
//! declared grid, and a scanning test fails the build if any output value sits off its
//! grid." This module is that crate's half of the contract: the quanta D9 names, a
//! [`Quantum`] type that carries one alongside the value it governs, and the two writers
//! every output path in this crate goes through ([`quantise`] and [`grid`]).
//!
//! Every [`crate::MetricDef`] declares the quantum of its own values, and
//! [`crate::MetricSample`] quantises on construction, so a value cannot reach the Arrow
//! writer, the run summary or a digest unquantised. The scanning check is
//! [`crate::invariants::check_d9_quantisation`].
//!
//! # A digest hashes the integer, not the rounded float
//!
//! D9 again: "A digest hashes the integer multiple of the quantum
//! (`v2xw_core::math::grid_index`), not the rounded float: a quantised `f64` is still a
//! binary approximation of a decimal grid point, and the integer is the value two
//! platforms are guaranteed to agree on." [`grid`] is that integer, and
//! [`crate::DigestSet`] hashes nothing else.

use v2xw_core::math::{grid_index, is_on_grid, quantize_to};

/// Metres. 1 mm, the legacy convention D9 keeps for lengths.
pub const Q_LENGTH_M: f64 = 1e-3;

/// Seconds. 1 ms, the legacy convention D9 keeps for durations.
pub const Q_TIME_S: f64 = 1e-3;

/// Milliseconds, for the latency metrics whose unit 08-measurement-and-data.md §2.1 states
/// as `ms`. 1 µs expressed in milliseconds, so a millisecond figure keeps the microsecond
/// resolution the verification cost tables work in.
pub const Q_TIME_MS: f64 = 1e-3;

/// Decibels (and dB-referenced quantities: dBm, dB). 0.01 dB per D9.
pub const Q_DB: f64 = 1e-2;

/// Ratios: PDR, PER, CBR, shares, precision, recall. 1e-4 per D9.
pub const Q_RATIO: f64 = 1e-4;

/// Probabilities. 1e-6 per D9. Used where a ratio is genuinely a probability estimate
/// rather than an observed share — the bounds of a confidence interval, for instance,
/// which are statements about a probability.
pub const Q_PROBABILITY: f64 = 1e-6;

/// Bytes and byte rates. 1e-3, so a B/s figure divided by a window length keeps three
/// decimals rather than being silently truncated to an integer.
pub const Q_BYTES: f64 = 1e-3;

/// Speeds in m/s and accelerations in m/s². 1e-3, the length grid per second.
pub const Q_SPEED: f64 = 1e-3;

/// Vehicle flow in veh/h and density in veh/km. 1e-3.
pub const Q_TRAFFIC: f64 = 1e-3;

/// Counts expressed as a float (a rate of events per second, a mean queue depth). 1e-3.
pub const Q_COUNT: f64 = 1e-3;

/// The grid the **two sums** of a ratio-of-sums sit on: 1e-3.
///
/// A ratio of sums reports its numerator and its denominator beside the ratio
/// ([`crate::RatioEstimate::RatioOfSums`]), and those sums are bytes, metres or seconds
/// depending on the metric — all of which D9 puts on the same 1e-3 grid. One constant with
/// its own name, rather than reusing [`Q_BYTES`] for a sum of seconds, so a reader of the
/// writer does not have to wonder whether the choice was deliberate.
pub const Q_SUM: f64 = 1e-3;

/// A value's declared grid, carried with the value rather than remembered by convention.
///
/// A bare `f64` quantum is easy to pass to the wrong field; this newtype is not. It is
/// `Copy` and validated on construction, so a quantum of zero, a negative one or a `NaN`
/// cannot exist — those are the three inputs that would make [`quantize_to`] panic.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Quantum(f64);

impl Quantum {
    /// The quantum `q`.
    ///
    /// # Panics
    /// If `q` is not strictly positive and finite, exactly as [`quantize_to`] does. A
    /// quantum is a compile-time-ish constant of a schema field, so a bad one is a
    /// programming error rather than a runtime condition to be handled.
    #[must_use]
    pub fn new(q: f64) -> Self {
        assert!(
            q > 0.0 && q.is_finite(),
            "Quantum::new: the quantum must be strictly positive and finite, got {q}"
        );
        Self(q)
    }

    /// The quantum as an `f64`, for the schema field that records it.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// `x` rounded onto this grid — the writer-side quantiser of D9.
    #[must_use]
    pub fn quantise(self, x: f64) -> f64 {
        quantize_to(x, self.0)
    }

    /// The integer multiple of this grid that `x` quantises to — the digest form.
    #[must_use]
    pub fn grid(self, x: f64) -> i64 {
        grid_index(x, self.0)
    }

    /// True if `x` already sits on this grid.
    #[must_use]
    pub fn holds(self, x: f64) -> bool {
        is_on_grid(x, self.0)
    }

    /// Metres and other lengths: [`Q_LENGTH_M`].
    pub const LENGTH_M: Quantum = Quantum(Q_LENGTH_M);
    /// Seconds: [`Q_TIME_S`].
    pub const TIME_S: Quantum = Quantum(Q_TIME_S);
    /// Milliseconds: [`Q_TIME_MS`].
    pub const TIME_MS: Quantum = Quantum(Q_TIME_MS);
    /// Decibels: [`Q_DB`].
    pub const DB: Quantum = Quantum(Q_DB);
    /// Ratios: [`Q_RATIO`].
    pub const RATIO: Quantum = Quantum(Q_RATIO);
    /// Probabilities: [`Q_PROBABILITY`].
    pub const PROBABILITY: Quantum = Quantum(Q_PROBABILITY);
    /// Bytes: [`Q_BYTES`].
    pub const BYTES: Quantum = Quantum(Q_BYTES);
    /// Speeds and accelerations: [`Q_SPEED`].
    pub const SPEED: Quantum = Quantum(Q_SPEED);
    /// Flow and density: [`Q_TRAFFIC`].
    pub const TRAFFIC: Quantum = Quantum(Q_TRAFFIC);
    /// Counts and per-second counts: [`Q_COUNT`].
    pub const COUNT: Quantum = Quantum(Q_COUNT);
    /// The two sums of a ratio-of-sums: [`Q_SUM`].
    pub const SUM: Quantum = Quantum(Q_SUM);
}

/// `x` rounded onto the grid `q` — the one writer-side quantiser this crate calls.
///
/// A free function as well as a method because most call sites have a [`Quantum`] constant
/// in hand and read better as `quantise(x, Quantum::RATIO)`.
#[must_use]
pub fn quantise(x: f64, q: Quantum) -> f64 {
    q.quantise(x)
}

/// The integer multiple of `q` that `x` quantises to — what a digest hashes.
#[must_use]
pub fn grid(x: f64, q: Quantum) -> i64 {
    q.grid(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantising_is_idempotent_on_every_declared_grid() {
        let qs = [
            Quantum::LENGTH_M,
            Quantum::TIME_S,
            Quantum::DB,
            Quantum::RATIO,
            Quantum::PROBABILITY,
            Quantum::BYTES,
            Quantum::SPEED,
            Quantum::TRAFFIC,
            Quantum::COUNT,
        ];
        for q in qs {
            for x in [0.0, 1.0 / 3.0, -1.0 / 7.0, 12.345_678_9, 1e6 / 7.0] {
                let once = q.quantise(x);
                assert_eq!(q.quantise(once).to_bits(), once.to_bits(), "{x} on {q:?}");
                assert!(q.holds(once), "{once} off the grid {q:?}");
            }
        }
    }

    #[test]
    fn a_ratio_keeps_four_decimals_and_a_probability_six() {
        assert_eq!(quantise(1.0 / 3.0, Quantum::RATIO), 0.3333);
        assert_eq!(quantise(1.0 / 3.0, Quantum::PROBABILITY), 0.333_333);
    }

    #[test]
    fn the_grid_index_is_the_integer_two_platforms_agree_on() {
        assert_eq!(grid(0.3333, Quantum::RATIO), 3333);
        // The two f64s either side of a grid point digest identically.
        assert_eq!(
            grid(0.333_299_999_999_999_9, Quantum::RATIO),
            grid(0.333_300_000_000_000_1, Quantum::RATIO)
        );
    }

    #[test]
    #[should_panic(expected = "strictly positive and finite")]
    fn a_zero_quantum_is_refused() {
        let _ = Quantum::new(0.0);
    }
}
