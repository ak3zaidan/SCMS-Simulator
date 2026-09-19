//! Cell arithmetic for the uniform grid index, with a deterministic neighbourhood order.
//!
//! ADR 0004 decision 6 and 02-architecture.md §5.2 fix the spatial index:
//!
//! > uniform grid hash rebuilt per mobility step with cell size = maximum modeled
//! > communication range (3 × 3 query) […] so a neighbor query touches at most nine cells.
//!
//! This module is the arithmetic that index is built on: which cell a position falls in,
//! and which cells a query has to visit, **in a fixed order**. It is not the index itself —
//! the buckets, the rebuild and the actor payloads live in `v2xw-world` — because at least
//! four crates need the cell arithmetic and only one needs the buckets: the radio stack
//! iterates neighbours, perception iterates a smaller radius, the recorder buckets events by
//! region and the UI culls by tile. Four private copies of `floor(x / size)` would agree
//! until one of them handled a negative coordinate differently, and then two crates would
//! disagree about which cell a vehicle west of the origin was in.
//!
//! # The determinism guarantee
//!
//! **Neighbourhood iteration is in ascending `x`, then ascending `y`, and depends on nothing
//! else.** Not on a hash, not on an insertion order, not on a thread count, not on the query
//! point's position within its cell. That ordering is identical to [`GridCell`]'s own
//! [`Ord`], so a `BTreeMap<GridCell, _>` walked over a range and a
//! [`GridIndex::neighborhood`] visit the same cells in the same sequence.
//!
//! This matters because the order cells are visited in is the order contributions are
//! accumulated in, and floating-point addition is not associative: an interference sum
//! gathered over a hash-ordered neighbourhood would produce different last bits on a
//! different run of the same binary, since `std`'s `HashMap` seeds its hasher per process.
//! Iterating in cell order — and then reducing in id order with
//! [`crate::math::sum_sorted_by_key`] — is what makes the result the same everywhere.

use crate::geom::{Bbox, Vec3};

/// One cell of the uniform grid: integer coordinates, not a dense id.
///
/// Signed, because world coordinates are: the world origin is its bounding box's south-west
/// corner (build decision D6), but a focus region, a VRU on a pavement outside the imported
/// area or a vehicle leaving the world all produce negative metres, and a grid that wrapped
/// or clamped them would collide two distant places into one cell.
///
/// Distinct from [`crate::ids::CellId`], which is a *cellular* cell — a base station's
/// coverage area. Nothing converts between them.
///
/// The derived [`Ord`] is `(x, y)` lexicographic, which is deliberately the same order
/// [`GridIndex::neighborhood`] yields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct GridCell {
    /// Cell index along east, `floor(x_m / cell_size_m)`.
    pub x: i32,
    /// Cell index along north, `floor(y_m / cell_size_m)`.
    pub y: i32,
}

impl GridCell {
    /// Creates a cell coordinate.
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// This cell offset by `(dx, dy)`, saturating at the `i32` bounds.
    pub const fn offset(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
        }
    }

    /// The Chebyshev distance to `other` in cells: the number of 3 × 3 rings between them,
    /// which is the useful measure for a grid whose query is a square neighbourhood.
    ///
    /// Saturating, so two cells at opposite ends of the `i32` range give `i32::MAX` rather
    /// than overflowing.
    pub const fn ring_distance(self, other: GridCell) -> i32 {
        let dx = self.x.saturating_sub(other.x).saturating_abs();
        let dy = self.y.saturating_sub(other.y).saturating_abs();
        if dx > dy { dx } else { dy }
    }
}

impl core::fmt::Display for GridCell {
    /// Formats as `g(x,y)`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "g({},{})", self.x, self.y)
    }
}

/// The geometry of a uniform grid: one cell size, and the arithmetic that follows from it.
///
/// Holds no cells and no contents — it is a `Copy` scalar wrapper, cheap to pass around and
/// safe to share across the phase-parallel maps. The container that maps a [`GridCell`] to
/// what is in it belongs to whichever crate owns the entities.
///
/// The grid is two-dimensional on purpose: `z` never enters the cell computation. Vehicles
/// and RSUs occupy a thin slab compared with the communication ranges the cell size comes
/// from, so a third axis would multiply the cell count for no pruning at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridIndex {
    cell_size_m: f64,
}

impl GridIndex {
    /// Creates a grid of square cells `cell_size_m` across.
    ///
    /// ADR 0004 decision 6 sets the size: **the maximum modelled communication range**
    /// (default 1,000 m for the 802.11p high tier). At that size everything reachable from a
    /// point is in the 3 × 3 neighbourhood of its cell, so [`GridIndex::neighborhood`] is the
    /// complete candidate set for a radio query and no larger radius is ever needed.
    ///
    /// # Panics
    /// If `cell_size_m` is not strictly positive and finite. A zero or negative cell size has
    /// no cells; `NaN` would put every entity in cell `(0, 0)` and silently turn every query
    /// into a scan of the whole world.
    pub fn new(cell_size_m: f64) -> Self {
        assert!(
            cell_size_m > 0.0 && cell_size_m.is_finite(),
            "GridIndex: the cell size must be strictly positive and finite, got {cell_size_m}"
        );
        Self { cell_size_m }
    }

    /// The cell size, metres.
    pub const fn cell_size_m(&self) -> f64 {
        self.cell_size_m
    }

    /// The cell a position falls in: `floor(x / size)`, `floor(y / size)`, ignoring `z`.
    ///
    /// `floor`, not truncation, so the cells west and south of the origin are the same width
    /// as the others: truncating would make `[-1, 1)` one double-width cell at `0` and put a
    /// vehicle 0.5 m west of the origin in the same cell as one 0.5 m east.
    ///
    /// A coordinate beyond the `i32` range saturates rather than wrapping (Rust's float-to-int
    /// casts are saturating), so a runaway position lands in an extreme cell instead of
    /// appearing next to the origin. `NaN` maps to cell `(0, 0)`; in debug builds it trips an
    /// assertion first, because a `NaN` position is always a bug upstream.
    pub fn cell_of(&self, p: Vec3) -> GridCell {
        debug_assert!(
            p.x.is_finite() && p.y.is_finite(),
            "GridIndex::cell_of: non-finite position {p}"
        );
        GridCell {
            x: (p.x / self.cell_size_m).floor() as i32,
            y: (p.y / self.cell_size_m).floor() as i32,
        }
    }

    /// The south-west (minimum) corner of a cell, metres. `z` is zero.
    pub fn corner_of(&self, c: GridCell) -> Vec3 {
        Vec3::new_2d(
            f64::from(c.x) * self.cell_size_m,
            f64::from(c.y) * self.cell_size_m,
        )
    }

    /// The cell's extent as a box, unbounded in `z`.
    ///
    /// Half-open in the sense that matters: a point on the north or east edge belongs to the
    /// *next* cell, because [`GridIndex::cell_of`] floors. [`Bbox::contains`] is inclusive on
    /// both edges, so use it for culling, not for deciding membership.
    pub fn bbox_of(&self, c: GridCell) -> Bbox {
        let min = self.corner_of(c);
        Bbox::new(
            Vec3::new(min.x, min.y, f64::NEG_INFINITY),
            Vec3::new(
                min.x + self.cell_size_m,
                min.y + self.cell_size_m,
                f64::INFINITY,
            ),
        )
    }

    /// The 3 × 3 neighbourhood of `centre`, in **ascending `x`, then ascending `y`** — nine
    /// cells, the query of 02-architecture.md §5.2.
    ///
    /// The order is part of the contract, not an implementation detail: see the module
    /// documentation. It is the same order [`GridCell`]'s [`Ord`] gives, and it does not
    /// depend on any hash.
    pub fn neighborhood(&self, centre: GridCell) -> impl Iterator<Item = GridCell> + Clone {
        self.neighborhood_radius(centre, 1)
    }

    /// The `(2r + 1)²` cells within `r` rings of `centre`, in ascending `x` then ascending
    /// `y`.
    ///
    /// `r = 0` yields the centre alone, `r = 1` is [`GridIndex::neighborhood`]. A model needs
    /// `r > 1` only when its own range exceeds the cell size, which the ADR's sizing rule is
    /// chosen to avoid; a *smaller* range still uses `r = 1` and filters by distance.
    ///
    /// The bounds are clamped and computed with saturating arithmetic on the *cell
    /// coordinates*, so a neighbourhood at the edge of the coordinate space yields fewer
    /// than `(2r + 1)²` cells — each of them once — rather than wrapping around or
    /// repeating the saturated edge. That matters for a caller that accumulates per cell:
    /// a duplicated cell would contribute twice. `r` above `i32::MAX` is clamped to it,
    /// which spans the whole coordinate space; the earlier `r as i32` cast made
    /// `u32::MAX` into `-1` and the iterator empty, so an unbounded query returned nothing
    /// at all.
    pub fn neighborhood_radius(
        &self,
        centre: GridCell,
        r: u32,
    ) -> impl Iterator<Item = GridCell> + Clone {
        let r = r.min(i32::MAX as u32) as i32;
        let (x0, x1) = (centre.x.saturating_sub(r), centre.x.saturating_add(r));
        let (y0, y1) = (centre.y.saturating_sub(r), centre.y.saturating_add(r));
        (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| GridCell::new(x, y)))
    }

    /// Every cell a circle of `radius_m` around `p` can touch, in ascending `x` then
    /// ascending `y`.
    ///
    /// The candidate set for a range query of an arbitrary radius: `ceil(radius / size)`
    /// rings around the cell of `p`, which is a superset of the circle (the caller still
    /// filters by true distance). Deriving the ring count from the radius rather than
    /// assuming one ring is what makes a 2 km query correct on a 1 km grid.
    ///
    /// An enormous or infinite radius covers the whole coordinate space, and yields it: the
    /// float-to-integer cast saturates the ring count and [`GridIndex::neighborhood_radius`]
    /// clamps it, so `radius_m = INFINITY` is "every cell", lazily. It is the caller's job
    /// to keep the radius bounded if it intends to enumerate the result — a model that
    /// spells "unlimited range" as an infinite radius is asking for the whole space and
    /// gets it, where it previously got an empty candidate set and silently matched
    /// nothing.
    ///
    /// # Panics
    /// If `radius_m` is negative or not a number. A zero radius is legal and yields the
    /// single cell containing `p`.
    pub fn cells_within(&self, p: Vec3, radius_m: f64) -> impl Iterator<Item = GridCell> + Clone {
        assert!(
            radius_m >= 0.0,
            "GridIndex::cells_within: the radius must be non-negative, got {radius_m}"
        );
        // Rust's float-to-integer casts saturate, so an infinite or enormous ring count
        // becomes `u32::MAX`, which `neighborhood_radius` clamps to the whole coordinate
        // space. NaN cannot reach the cast: the assertion above rejects it.
        let rings = (radius_m / self.cell_size_m).ceil() as u32;
        self.neighborhood_radius(self.cell_of(p), rings)
    }

    /// Every cell a box overlaps, in ascending `x` then ascending `y`.
    ///
    /// The query a viewport cull, a focus region or a region-scoped metric makes. An empty
    /// box yields nothing.
    pub fn cells_in_bbox(&self, b: Bbox) -> impl Iterator<Item = GridCell> + Clone {
        let (x0, x1, y0, y1) = if b.is_empty() {
            // An empty range: `x0 > x1`, so both loops yield nothing. Checked before any
            // arithmetic, because an empty box's corners are infinite.
            (0, -1, 0, -1)
        } else {
            let min = self.cell_of(b.min);
            let max = self.cell_of(b.max);
            (min.x, max.x, min.y, max.y)
        };
        (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| GridCell::new(x, y)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Cells are `floor`-based, so they are the same width either side of the origin.
    #[test]
    fn cells_are_uniform_across_the_origin() {
        let g = GridIndex::new(100.0);
        assert_eq!(g.cell_size_m(), 100.0);
        assert_eq!(g.cell_of(Vec3::new_2d(0.0, 0.0)), GridCell::new(0, 0));
        assert_eq!(g.cell_of(Vec3::new_2d(99.999, 0.0)), GridCell::new(0, 0));
        assert_eq!(g.cell_of(Vec3::new_2d(100.0, 0.0)), GridCell::new(1, 0));
        // The case truncation gets wrong: half a metre either side of the origin.
        assert_eq!(g.cell_of(Vec3::new_2d(-0.5, -0.5)), GridCell::new(-1, -1));
        assert_eq!(g.cell_of(Vec3::new_2d(0.5, 0.5)), GridCell::new(0, 0));
        assert_eq!(
            g.cell_of(Vec3::new_2d(-100.0, -100.0)),
            GridCell::new(-1, -1)
        );
        assert_eq!(g.cell_of(Vec3::new_2d(-100.001, 0.0)), GridCell::new(-2, 0));
        // `z` is ignored.
        assert_eq!(
            g.cell_of(Vec3::new(150.0, 250.0, 9_999.0)),
            g.cell_of(Vec3::new(150.0, 250.0, -9_999.0))
        );
        assert_eq!(g.cell_of(Vec3::new(150.0, 250.0, 0.0)), GridCell::new(1, 2));

        // Corners and boxes line up with the cells they came from.
        assert_eq!(g.corner_of(GridCell::new(1, 2)), Vec3::new_2d(100.0, 200.0));
        assert_eq!(
            g.corner_of(GridCell::new(-1, -1)),
            Vec3::new_2d(-100.0, -100.0)
        );
        let b = g.bbox_of(GridCell::new(1, 2));
        assert!(b.contains_2d(Vec3::new_2d(150.0, 250.0)));
        assert!(!b.contains_2d(Vec3::new_2d(99.0, 250.0)));
        assert_eq!(g.cell_of(b.min), GridCell::new(1, 2));
    }

    /// The published order: ascending `x`, then ascending `y`, always the same nine cells in
    /// the same sequence — and identical to `GridCell`'s own `Ord`, so a `BTreeSet` walk and
    /// a neighbourhood walk agree.
    #[test]
    fn the_neighbourhood_order_is_fixed_and_hash_free() {
        let g = GridIndex::new(1_000.0);
        let centre = GridCell::new(3, 7);
        let cells: Vec<GridCell> = g.neighborhood(centre).collect();

        assert_eq!(
            cells,
            vec![
                GridCell::new(2, 6),
                GridCell::new(2, 7),
                GridCell::new(2, 8),
                GridCell::new(3, 6),
                GridCell::new(3, 7),
                GridCell::new(3, 8),
                GridCell::new(4, 6),
                GridCell::new(4, 7),
                GridCell::new(4, 8),
            ]
        );

        // Sorted, and therefore the same order an ordered map would give.
        let mut sorted = cells.clone();
        sorted.sort();
        assert_eq!(cells, sorted, "iteration order must be the Ord order");
        let set: BTreeSet<GridCell> = cells.iter().copied().collect();
        assert_eq!(set.into_iter().collect::<Vec<_>>(), cells);

        // Repeating the query — on a fresh index, in a different process state — gives the
        // identical sequence. Nothing here is seeded.
        for _ in 0..3 {
            assert_eq!(
                GridIndex::new(1_000.0)
                    .neighborhood(centre)
                    .collect::<Vec<_>>(),
                cells
            );
        }
        // …and the order does not depend on where in the cell the query point was.
        for p in [
            Vec3::new_2d(3_000.0, 7_000.0),
            Vec3::new_2d(3_999.999, 7_999.999),
            Vec3::new_2d(3_500.0, 7_500.0),
        ] {
            assert_eq!(g.cell_of(p), centre);
            assert_eq!(g.neighborhood(g.cell_of(p)).collect::<Vec<_>>(), cells);
        }
    }

    /// Larger neighbourhoods keep the same order and the right count, and `r = 0` is the
    /// centre alone.
    #[test]
    fn radius_neighbourhoods_scale() {
        let g = GridIndex::new(50.0);
        let c = GridCell::new(0, 0);
        assert_eq!(g.neighborhood_radius(c, 0).collect::<Vec<_>>(), vec![c]);
        assert_eq!(g.neighborhood_radius(c, 1).count(), 9);
        assert_eq!(g.neighborhood_radius(c, 2).count(), 25);
        assert_eq!(g.neighborhood_radius(c, 3).count(), 49);

        let cells: Vec<GridCell> = g.neighborhood_radius(c, 2).collect();
        let mut sorted = cells.clone();
        sorted.sort();
        assert_eq!(cells, sorted);
        assert_eq!(cells[0], GridCell::new(-2, -2));
        assert_eq!(cells[24], GridCell::new(2, 2));
        assert!(cells.contains(&c));
        assert_eq!(c.ring_distance(GridCell::new(2, 1)), 2);
        assert_eq!(c.ring_distance(GridCell::new(-1, 3)), 3);
        assert_eq!(c.ring_distance(c), 0);
        assert_eq!(c.to_string(), "g(0,0)");
    }

    /// A query radius larger than the cell size must widen the ring count, or the index
    /// silently misses everything in the ring it did not look at.
    #[test]
    fn a_range_query_covers_its_whole_circle() {
        let g = GridIndex::new(100.0);
        let p = Vec3::new_2d(250.0, 250.0);

        assert_eq!(
            g.cells_within(p, 0.0).collect::<Vec<_>>(),
            vec![GridCell::new(2, 2)]
        );
        assert_eq!(g.cells_within(p, 50.0).count(), 9, "one ring");
        assert_eq!(g.cells_within(p, 100.0).count(), 9);
        assert_eq!(g.cells_within(p, 100.001).count(), 25, "two rings");
        assert_eq!(g.cells_within(p, 250.0).count(), 49, "three rings");

        // Every cell the circle actually touches is in the candidate set — the property the
        // index exists to provide. Sampled on the circle and inside it.
        let radius = 320.0;
        let candidates: BTreeSet<GridCell> = g.cells_within(p, radius).collect();
        for i in 0..720 {
            let a = (i as f64) * core::f64::consts::PI / 360.0;
            let (s, c) = crate::math::sin_cos(a);
            for k in [0.25, 0.5, 0.75, 1.0] {
                let q = Vec3::new_2d(p.x + radius * k * c, p.y + radius * k * s);
                assert!(
                    candidates.contains(&g.cell_of(q)),
                    "{q} at radius {} is outside the candidate set",
                    radius * k
                );
            }
        }
        // …and the candidate order is still the fixed one.
        let listed: Vec<GridCell> = g.cells_within(p, radius).collect();
        assert_eq!(listed, candidates.into_iter().collect::<Vec<_>>());
    }

    /// Box queries, including the empty box that an accumulating union can produce.
    #[test]
    fn box_queries_cover_the_box() {
        let g = GridIndex::new(100.0);
        let b = Bbox::new(Vec3::new_2d(50.0, 50.0), Vec3::new_2d(250.0, 150.0));
        let cells: Vec<GridCell> = g.cells_in_bbox(b).collect();
        assert_eq!(
            cells,
            vec![
                GridCell::new(0, 0),
                GridCell::new(0, 1),
                GridCell::new(1, 0),
                GridCell::new(1, 1),
                GridCell::new(2, 0),
                GridCell::new(2, 1),
            ]
        );
        let mut sorted = cells.clone();
        sorted.sort();
        assert_eq!(cells, sorted);

        // A box inside one cell yields that cell; an empty box yields nothing.
        assert_eq!(
            g.cells_in_bbox(Bbox::new(
                Vec3::new_2d(10.0, 10.0),
                Vec3::new_2d(20.0, 20.0)
            ))
            .collect::<Vec<_>>(),
            vec![GridCell::new(0, 0)]
        );
        assert_eq!(g.cells_in_bbox(Bbox::empty()).count(), 0);
        // Negative coordinates behave like any other.
        assert_eq!(
            g.cells_in_bbox(Bbox::new(
                Vec3::new_2d(-150.0, -50.0),
                Vec3::new_2d(-120.0, -10.0)
            ))
            .collect::<Vec<_>>(),
            vec![GridCell::new(-2, -1)]
        );
    }

    /// Extreme coordinates saturate instead of wrapping: a runaway actor must not land next
    /// to the origin, and an edge cell's neighbourhood must not appear on the far side of
    /// the world.
    #[test]
    fn extreme_coordinates_saturate() {
        let g = GridIndex::new(1.0);
        assert_eq!(g.cell_of(Vec3::new_2d(1e30, -1e30)).x, i32::MAX);
        assert_eq!(g.cell_of(Vec3::new_2d(1e30, -1e30)).y, i32::MIN);

        let edge = GridCell::new(i32::MAX, i32::MIN);
        let cells: Vec<GridCell> = g.neighborhood(edge).collect();
        assert!(
            cells
                .iter()
                .all(|c| c.x >= i32::MAX - 1 && c.y <= i32::MIN + 1)
        );
        assert_eq!(
            cells.len(),
            4,
            "the four cells that exist at a corner, each once"
        );
        assert_eq!(
            cells.iter().copied().collect::<BTreeSet<_>>().len(),
            cells.len(),
            "no duplicates: a caller that accumulates per cell must not count one twice"
        );
        let mut sorted = cells.clone();
        sorted.sort();
        assert_eq!(
            cells, sorted,
            "the order is the published one at the edge too"
        );
        assert_eq!(edge.offset(1, -1), edge, "the offset saturates");
        assert_eq!(GridCell::new(0, 0).ring_distance(edge), i32::MAX);
    }

    /// An unbounded radius must cover the coordinate space, not collapse to nothing.
    ///
    /// `cells_within` used to set `rings = u32::MAX` for an enormous radius and
    /// `neighborhood_radius` then cast it with `r as i32`, giving `-1`, so the range
    /// `(-r..=r)` was `(1..=-1)` — empty. A model expressing "unlimited range" as an
    /// infinite radius got an empty candidate set: a query that should match everything
    /// matched nothing, with no diagnostic. The set is enumerated lazily and by its bounds,
    /// never counted, because the whole space is 2^62 cells.
    #[test]
    fn an_unbounded_radius_covers_the_space_instead_of_nothing() {
        let g = GridIndex::new(100.0);
        let p = Vec3::new_2d(250.0, 250.0);
        let centre = g.cell_of(p);
        assert_eq!(centre, GridCell::new(2, 2));

        for radius in [f64::INFINITY, 1e40, f64::MAX] {
            let mut it = g.cells_within(p, radius);
            let first = it.next().expect("an unbounded query yields cells");
            assert_eq!(
                first,
                GridCell::new(
                    centre.x.saturating_sub(i32::MAX),
                    centre.y.saturating_sub(i32::MAX)
                ),
                "radius {radius} must start at the south-west end of the space"
            );
            // The first column runs the whole height of the space, in ascending y.
            assert_eq!(it.next().unwrap(), GridCell::new(first.x, first.y + 1));
            assert_eq!(
                g.cells_within(p, radius).take(10_000).count(),
                10_000,
                "radius {radius} must not run out after a handful of cells"
            );
        }

        // A bounded radius is unaffected: the ring count is still ceil(radius / size).
        assert_eq!(g.cells_within(p, 2_000.0).count(), 41 * 41);
        assert_eq!(g.cells_within(p, 100.001).count(), 25);

        // A huge ring count no longer overflows on `-r` either (this used to panic in
        // debug builds), and it is clamped rather than wrapped.
        let mut it = g.neighborhood_radius(GridCell::new(0, 0), 1u32 << 31);
        assert_eq!(it.next().unwrap(), GridCell::new(-i32::MAX, -i32::MAX));
        assert_eq!(
            g.neighborhood_radius(GridCell::new(0, 0), u32::MAX)
                .next()
                .unwrap(),
            GridCell::new(-i32::MAX, -i32::MAX)
        );
    }

    #[test]
    #[should_panic(expected = "strictly positive")]
    fn a_zero_cell_size_is_refused() {
        let _ = GridIndex::new(0.0);
    }

    #[test]
    #[should_panic(expected = "non-negative")]
    fn a_negative_query_radius_is_refused() {
        let _ = GridIndex::new(10.0).cells_within(Vec3::ZERO, -1.0);
    }
}
