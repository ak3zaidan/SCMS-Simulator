//! Binning that cannot flip between platforms.
//!
//! 08-measurement-and-data.md §2 dimensions a metric by `t`, `dist_bin` (25 m),
//! `density_bin` and the rest. A bin boundary is a threshold comparison, and build
//! decision D10 is explicit about those: "Quantise before any threshold comparison whose
//! outcome is compared across engines". Its measured justification is a legacy
//! configuration whose report count moved from 3,082 to 3,135 when every transcendental was
//! perturbed by one unit in the last place.
//!
//! So no comparison in this module is made between floats. [`Bins`] holds its edges as
//! **integer grid indices** (`v2xw_core::math::grid_index`), converts the value to its grid
//! index, and compares integers. A distance of 24.999 999 999 999 996 m and one of
//! 25.000 000 000 000 004 m are the same integer on the millimetre grid and therefore land
//! in the same bin on every platform, whatever produced them.
//!
//! [`TimeBins`] is integer arithmetic on `SimTime` nanoseconds from the start, so it has no
//! float in it at all.

use v2xw_core::time::{Duration, SimTime};

use crate::error::{MetricError, Result};
use crate::quant::Quantum;

/// A set of half-open bins over a quantity, compared on the integer grid.
///
/// The bins are `[lower[0], lower[1]) , [lower[1], lower[2]) , … , [lower[n−1], ∞)`: the
/// last one is open-ended, because a distance histogram that silently drops everything past
/// its last edge is a histogram that hides its tail. A value below `lower[0]` is outside the
/// domain and [`Bins::index_of`] answers `None` for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Bins {
    /// What the bins are over, for the dimension label: `"dist_bin"`, `"density_bin"`.
    dimension: String,
    /// The grid every edge and every value is compared on.
    quantum: Quantum,
    /// The declared lower edges, for labels and for the schema.
    lower: Vec<f64>,
    /// The same edges as integer multiples of `quantum` — what comparisons actually use.
    lower_grid: Vec<i64>,
    /// The unit, for the label: `"m"`, `"veh/km"`.
    unit: String,
}

impl Bins {
    /// Bins with the given lower edges.
    ///
    /// The edges are quantised onto `quantum` before anything else, so the bin set is
    /// itself on the grid. They must be strictly increasing **after quantisation**: two
    /// edges closer together than one quantum would be indistinguishable to every
    /// comparison this type makes, and accepting them would produce a bin nothing can land
    /// in.
    ///
    /// # Errors
    /// [`MetricError::BadDefinition`] if `lower` is empty, contains a non-finite value, or
    /// is not strictly increasing on the grid.
    pub fn new(
        dimension: impl Into<String>,
        unit: impl Into<String>,
        lower: &[f64],
        quantum: Quantum,
    ) -> Result<Self> {
        let dimension = dimension.into();
        if lower.is_empty() {
            return Err(MetricError::BadDefinition {
                name: dimension,
                problem: "a bin set needs at least one lower edge".to_string(),
            });
        }
        let mut lower_grid = Vec::with_capacity(lower.len());
        let mut edges = Vec::with_capacity(lower.len());
        for &e in lower {
            if !e.is_finite() {
                return Err(MetricError::BadDefinition {
                    name: dimension,
                    problem: format!("bin edge {e} is not finite"),
                });
            }
            let g = quantum.grid(e);
            if let Some(&prev) = lower_grid.last()
                && g <= prev
            {
                return Err(MetricError::BadDefinition {
                    name: dimension,
                    problem: format!(
                        "bin edges must strictly increase on the grid of {}: {e} follows an edge \
                         at the same or a higher grid index",
                        quantum.get()
                    ),
                });
            }
            lower_grid.push(g);
            edges.push(quantum.quantise(e));
        }
        Ok(Self {
            dimension,
            quantum,
            lower: edges,
            lower_grid,
            unit: unit.into(),
        })
    }

    /// `count` bins of width `width` starting at zero, plus the open-ended one above them.
    ///
    /// `Bins::uniform("dist_bin", "m", 25.0, 8, Quantum::LENGTH_M)` is
    /// 08-measurement-and-data.md §2's 25 m distance binning: `[0,25) … [175,200)` and
    /// `[200, ∞)`.
    ///
    /// # Errors
    /// [`MetricError::BadDefinition`] if `width` is not strictly positive and finite, or
    /// `count` is zero.
    pub fn uniform(
        dimension: impl Into<String>,
        unit: impl Into<String>,
        width: f64,
        count: usize,
        quantum: Quantum,
    ) -> Result<Self> {
        let dimension = dimension.into();
        if !(width > 0.0 && width.is_finite()) || count == 0 {
            return Err(MetricError::BadDefinition {
                name: dimension,
                problem: format!(
                    "uniform bins need a positive finite width and count, got {width} and {count}"
                ),
            });
        }
        // Built by multiplication from the index rather than by repeated addition, so the
        // edges do not accumulate rounding.
        let lower: Vec<f64> = (0..=count).map(|i| (i as f64) * width).collect();
        Self::new(dimension, unit, &lower, quantum)
    }

    /// 08-measurement-and-data.md §2's declared distance binning: 25 m bins.
    ///
    /// `bins` is how many finite 25 m bins there are; the open-ended bin above them is
    /// added on top, so `distance_25m(8)` covers `[0, 200)` in eight bins and `[200, ∞)`.
    ///
    /// # Errors
    /// [`MetricError::BadDefinition`] if `bins` is zero.
    pub fn distance_25m(bins: usize) -> Result<Self> {
        Self::uniform("dist_bin", "m", 25.0, bins, Quantum::LENGTH_M)
    }

    /// The number of bins, the open-ended one included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lower.len()
    }

    /// Always false: [`Bins::new`] refuses an empty edge list. Present because clippy asks
    /// for it next to `len`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The dimension these bins are over.
    #[must_use]
    pub fn dimension(&self) -> &str {
        &self.dimension
    }

    /// The bin `value` falls in, or `None` if it is below the first edge or not finite.
    ///
    /// The comparison is between integers: `value` is converted to its grid index and the
    /// edges already are theirs. A value one ULP either side of an edge therefore cannot
    /// land in different bins on two platforms, which is what D10 requires.
    #[must_use]
    pub fn index_of(&self, value: f64) -> Option<usize> {
        if !value.is_finite() {
            return None;
        }
        let g = self.quantum.grid(value);
        if g < self.lower_grid[0] {
            return None;
        }
        // Every edge <= g; there is at least one, so the subtraction cannot underflow.
        Some(self.lower_grid.partition_point(|&e| e <= g) - 1)
    }

    /// The half-open range of bin `i`, with `None` for the open-ended top bin's upper edge.
    ///
    /// # Panics
    /// If `i` is not a bin index.
    #[must_use]
    pub fn range(&self, i: usize) -> (f64, Option<f64>) {
        assert!(i < self.lower.len(), "bin index {i} out of range");
        (self.lower[i], self.lower.get(i + 1).copied())
    }

    /// The bin's label as it appears in a dimension value: `"25-50"`, `"200+"`.
    ///
    /// The numbers are formatted from the quantised edges, so the label and the comparison
    /// agree by construction.
    ///
    /// # Panics
    /// If `i` is not a bin index.
    #[must_use]
    pub fn label(&self, i: usize) -> String {
        match self.range(i) {
            (lo, Some(hi)) => format!("{}-{}", trim(lo), trim(hi)),
            (lo, None) => format!("{}+", trim(lo)),
        }
    }

    /// The unit of the binned quantity.
    #[must_use]
    pub fn unit(&self) -> &str {
        &self.unit
    }

    /// The grid the edges and the values are compared on.
    #[must_use]
    pub const fn quantum(&self) -> Quantum {
        self.quantum
    }
}

/// Formats an edge without a trailing `.0`, so a 25 m bin is labelled `25-50` and not
/// `25-50` only by luck.
fn trim(x: f64) -> String {
    if x == x.trunc() && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// Fixed-period time bins over `SimTime`, in integer nanoseconds.
///
/// The `t` dimension of every metric. There is no float anywhere in it: a bin index is
/// `(t − t0) / period` in nanoseconds, so two engines that agree on the event's `SimTime`
/// agree on its bin, with no rounding to argue about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBins {
    t0: SimTime,
    period: Duration,
}

impl TimeBins {
    /// Bins of `period` starting at `t0`.
    ///
    /// # Errors
    /// [`MetricError::BadDefinition`] if `period` is zero: every instant would be in bin
    /// zero and the dimension would be a lie.
    pub fn new(t0: SimTime, period: Duration) -> Result<Self> {
        if period.is_zero() {
            return Err(MetricError::BadDefinition {
                name: "t".to_string(),
                problem: "a time bin period of zero puts every instant in one bin".to_string(),
            });
        }
        Ok(Self { t0, period })
    }

    /// One-second bins from `t0` — the cadence 08-measurement-and-data.md §2 assumes for
    /// per-second metrics (`airtime_per_node` in ms/s, `verify_rate` in 1/s).
    #[must_use]
    pub fn per_second(t0: SimTime) -> Self {
        Self {
            t0,
            period: Duration::from_secs(1),
        }
    }

    /// The bin `t` falls in, or `None` if `t` is before `t0`.
    #[must_use]
    pub fn index_of(&self, t: SimTime) -> Option<u64> {
        if t < self.t0 {
            return None;
        }
        Some((t - self.t0) / self.period.as_nanos())
    }

    /// The instant bin `i` starts at.
    #[must_use]
    pub fn start_of(&self, i: u64) -> SimTime {
        self.t0
            .saturating_add(self.period.as_nanos().saturating_mul(i))
    }

    /// The bin period.
    #[must_use]
    pub const fn period(&self) -> Duration {
        self.period
    }

    /// The first bin's start.
    #[must_use]
    pub const fn t0(&self) -> SimTime {
        self.t0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_25m_bins_are_the_documented_ones() {
        let b = Bins::distance_25m(8).unwrap();
        assert_eq!(b.len(), 9, "eight finite bins plus the open-ended one");
        assert_eq!(b.label(0), "0-25");
        assert_eq!(b.label(1), "25-50");
        assert_eq!(b.label(7), "175-200");
        assert_eq!(b.label(8), "200+");
        assert_eq!(b.index_of(0.0), Some(0));
        assert_eq!(b.index_of(24.999), Some(0));
        assert_eq!(
            b.index_of(25.0),
            Some(1),
            "half-open: the edge is in the upper bin"
        );
        assert_eq!(b.index_of(199.999), Some(7));
        assert_eq!(b.index_of(200.0), Some(8));
        assert_eq!(b.index_of(10_000.0), Some(8), "the top bin is open-ended");
        assert_eq!(b.index_of(-0.001), None);
        assert_eq!(b.index_of(f64::NAN), None);
    }

    /// The property D10 asks for: two values that differ by less than the grid cannot land
    /// in different bins, whatever produced them.
    #[test]
    fn a_boundary_case_cannot_flip_on_the_grid() {
        let b = Bins::distance_25m(8).unwrap();
        // The two f64s either side of 25.0, and 25 m reached by two different routes.
        let just_below = 25.0_f64 - f64::EPSILON * 16.0;
        let just_above = 25.0_f64 + f64::EPSILON * 16.0;
        assert_eq!(b.index_of(just_below), b.index_of(25.0));
        assert_eq!(b.index_of(just_above), b.index_of(25.0));
        // A value genuinely below the edge by more than half a millimetre still falls below.
        assert_eq!(b.index_of(24.999_4), Some(0));
        // And one genuinely on the edge to the millimetre falls above.
        assert_eq!(
            b.index_of(24.999_6),
            Some(1),
            "rounds onto the 25.000 grid point"
        );
    }

    #[test]
    fn edges_that_collapse_on_the_grid_are_refused() {
        let e = Bins::new("x", "m", &[0.0, 0.000_1, 1.0], Quantum::LENGTH_M);
        assert!(matches!(e, Err(MetricError::BadDefinition { .. })));
        assert!(Bins::new("x", "m", &[], Quantum::LENGTH_M).is_err());
        assert!(Bins::new("x", "m", &[0.0, f64::NAN], Quantum::LENGTH_M).is_err());
        assert!(Bins::uniform("x", "m", 0.0, 3, Quantum::LENGTH_M).is_err());
        assert!(Bins::uniform("x", "m", 25.0, 0, Quantum::LENGTH_M).is_err());
    }

    #[test]
    fn density_bins_are_declared_not_assumed() {
        // 08-measurement-and-data.md fixes the 25 m distance width but not the density
        // edges, so they come from the caller.
        let b = Bins::new(
            "density_bin",
            "veh/km",
            &[0.0, 10.0, 25.0, 50.0],
            Quantum::TRAFFIC,
        )
        .unwrap();
        assert_eq!(b.len(), 4);
        assert_eq!(b.label(3), "50+");
        assert_eq!(b.index_of(24.999), Some(1));
        assert_eq!(b.index_of(25.0), Some(2));
        // 24.9999 veh/km rounds onto the 25.000 grid point, so it lands in the upper bin.
        // That is the point of quantising first: the answer is a property of the grid the
        // field declares, not of how many digits the producer happened to compute.
        assert_eq!(b.index_of(24.999_9), Some(2));
    }

    #[test]
    fn time_bins_are_integer_arithmetic() {
        let t = TimeBins::per_second(1_000_000_000);
        assert_eq!(t.index_of(999_999_999), None);
        assert_eq!(t.index_of(1_000_000_000), Some(0));
        assert_eq!(t.index_of(1_999_999_999), Some(0));
        assert_eq!(t.index_of(2_000_000_000), Some(1));
        assert_eq!(t.start_of(3), 4_000_000_000);
        assert!(TimeBins::new(0, Duration::ZERO).is_err());
    }
}
