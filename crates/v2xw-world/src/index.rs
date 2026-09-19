//! Spatial indices over static geometry: an R-tree of building footprints, a uniform
//! grid of lane segments and the reverse lane adjacency.
//!
//! All three are **derived data**: pure functions of the [`World`] they are built from,
//! never serialised, and built lazily on the first query (`World::index`). That is what
//! lets [`World`] keep public data fields and still guarantee that no consumer can see an
//! index that disagrees with them.
//!
//! # Determinism
//!
//! An index may not leak its internal order into an answer:
//!
//! * every multi-result query **sorts by id before returning**, so an R-tree traversal
//!   order or a grid-cell order can never reach an output;
//! * the nearest-lane search breaks ties by `(distance, LaneId, s_m, later segment)` —
//!   the first two as 03-interfaces.md §2 requires, the last two to make the order total
//!   and to agree with [`crate::model::Lane::segment_at`] — and compares distances with
//!   `==`, so a "tie" is a bit-identical distance rather than a tolerance;
//! * the grid is built by two passes in lane-id order, so the entries of a cell are
//!   sorted by `(LaneId, segment)` without a sort step;
//! * there is no `HashMap` or `HashSet` anywhere in this module.

use std::collections::BTreeMap;

use rstar::{AABB, PointDistance, RTree, RTreeObject};
use serde::{Deserialize, Serialize};
use v2xw_core::geom::{Bbox, LanePos, Vec3};
use v2xw_core::ids::{BuildingId, LaneId};
use v2xw_core::math;

use crate::model::{ClassMask, Connection, Lane, World};

/// How the lazily built indices are sized.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IndexOptions {
    /// Side of one lane-grid cell, metres.
    ///
    /// The default is 25 m: a city block is 100–400 m, a lane segment after import is
    /// 5–50 m, and at 25 m a typical urban world puts one to three segments in a cell,
    /// which is where a ring search stops paying for more cells. It is a performance
    /// knob only — the answers do not depend on it, which the
    /// `grid_cell_size_does_not_change_the_answer` test asserts.
    pub lane_grid_cell_m: f64,
    /// Upper bound on grid cells, so a pathological world cannot allocate gigabytes.
    ///
    /// When `bbox / lane_grid_cell_m` would exceed this, the cell size is grown until it
    /// fits. The growth is a deterministic function of the bounding box, so two engines
    /// still build the same grid.
    pub max_lane_grid_cells: usize,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            lane_grid_cell_m: 25.0,
            max_lane_grid_cells: 4_000_000,
        }
    }
}

/// One lane segment in a grid cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SegmentRef {
    lane: u32,
    segment: u32,
}

/// A uniform grid over lane segments, in compressed row form.
///
/// `cell_start[c] .. cell_start[c + 1]` indexes `entries`, so the whole grid is two
/// allocations however many cells are empty.
#[derive(Debug)]
struct LaneGrid {
    min_x: f64,
    min_y: f64,
    cell_m: f64,
    nx: i64,
    ny: i64,
    cell_start: Vec<u32>,
    entries: Vec<SegmentRef>,
}

impl LaneGrid {
    fn build(lanes: &[Lane], bbox: Bbox, options: IndexOptions) -> Self {
        let span_x = (bbox.max.x - bbox.min.x).max(1.0);
        let span_y = (bbox.max.y - bbox.min.y).max(1.0);
        let mut cell_m = if options.lane_grid_cell_m > 0.0 {
            options.lane_grid_cell_m
        } else {
            IndexOptions::default().lane_grid_cell_m
        };
        // Grow the cell until the grid fits the budget. Doubling rather than solving for
        // the exact size keeps this a short, obviously terminating loop whose result is a
        // pure function of the inputs.
        let budget = options.max_lane_grid_cells.max(1) as f64;
        while (span_x / cell_m + 1.0) * (span_y / cell_m + 1.0) > budget {
            cell_m *= 2.0;
        }
        let nx = (span_x / cell_m).floor() as i64 + 1;
        let ny = (span_y / cell_m).floor() as i64 + 1;
        let cells = (nx * ny) as usize;

        let skeleton = LaneGrid {
            min_x: bbox.min.x,
            min_y: bbox.min.y,
            cell_m,
            nx,
            ny,
            cell_start: Vec::new(),
            entries: Vec::new(),
        };
        // Pass one counts the entries per cell, pass two fills them: the classic
        // two-pass compressed-row build, with no per-cell `Vec` and no reallocation.
        let mut counts = vec![0u32; cells + 1];
        for lane in lanes {
            for i in 0..lane.point_count() - 1 {
                let (a, b) = lane.segment(i);
                skeleton.for_each_cell_of_segment(a, b, |c| counts[c + 1] += 1);
            }
        }
        for c in 0..cells {
            counts[c + 1] += counts[c];
        }
        let total = counts[cells] as usize;
        let cell_start = counts;
        let mut cursor = cell_start.clone();
        let mut entries = vec![
            SegmentRef {
                lane: u32::MAX,
                segment: u32::MAX,
            };
            total
        ];
        // Filling in lane-id order, then segment order, leaves every cell's slice sorted
        // by (lane, segment) with no sort step.
        for lane in lanes {
            for i in 0..lane.point_count() - 1 {
                let (a, b) = lane.segment(i);
                let entry = SegmentRef {
                    lane: lane.id.index(),
                    segment: i as u32,
                };
                skeleton.for_each_cell_of_segment(a, b, |c| {
                    entries[cursor[c] as usize] = entry;
                    cursor[c] += 1;
                });
            }
        }
        LaneGrid {
            cell_start,
            entries,
            ..skeleton
        }
    }

    /// The cell index of a point, clamped into the grid. Points outside the world's box
    /// land in the nearest border cell, which is what a query just off the edge wants.
    fn cell_of(&self, x: f64, y: f64) -> (i64, i64) {
        let ix = ((x - self.min_x) / self.cell_m).floor() as i64;
        let iy = ((y - self.min_y) / self.cell_m).floor() as i64;
        (ix, iy)
    }

    fn clamp(&self, ix: i64, iy: i64) -> Option<usize> {
        if ix < 0 || iy < 0 || ix >= self.nx || iy >= self.ny {
            return None;
        }
        Some((iy * self.nx + ix) as usize)
    }

    /// Calls `f` for every cell a segment's bounding box touches, clamped to the grid.
    ///
    /// Conservative on purpose: rasterising the box rather than the line puts a diagonal
    /// segment in a few cells it does not actually cross, which costs a distance
    /// computation and never a wrong answer. A callback rather than a `Vec`, because the
    /// two build passes call it once per segment on a world with hundreds of thousands of
    /// them.
    fn for_each_cell_of_segment(&self, a: Vec3, b: Vec3, mut f: impl FnMut(usize)) {
        let (ax, ay) = self.cell_of(a.x.min(b.x), a.y.min(b.y));
        let (bx, by) = self.cell_of(a.x.max(b.x), a.y.max(b.y));
        for iy in ay.max(0)..=by.min(self.ny - 1) {
            for ix in ax.max(0)..=bx.min(self.nx - 1) {
                if let Some(c) = self.clamp(ix, iy) {
                    f(c);
                }
            }
        }
    }

    fn cell_entries(&self, cell: usize) -> &[SegmentRef] {
        let from = self.cell_start[cell] as usize;
        let to = self.cell_start[cell + 1] as usize;
        &self.entries[from..to]
    }
}

/// A building's footprint envelope in the R-tree.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BuildingEntry {
    id: u32,
    min: [f64; 2],
    max: [f64; 2],
}

impl RTreeObject for BuildingEntry {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(self.min, self.max)
    }
}

impl PointDistance for BuildingEntry {
    /// Squared distance from a point to the footprint's envelope, zero inside it.
    fn distance_2(&self, point: &[f64; 2]) -> f64 {
        let dx = (self.min[0] - point[0])
            .max(0.0)
            .max(point[0] - self.max[0]);
        let dy = (self.min[1] - point[1])
            .max(0.0)
            .max(point[1] - self.max[1]);
        dx * dx + dy * dy
    }
}

/// The lazily built indices of one world.
#[derive(Debug)]
pub(crate) struct WorldIndex {
    lane_grid: LaneGrid,
    buildings: RTree<BuildingEntry>,
    /// Connections sorted by `(to_lane, from_lane, via)`, so predecessors are a subslice.
    predecessors: Vec<Connection>,
    /// Row offsets into `predecessors`, one per lane plus a terminator.
    predecessor_offsets: Vec<u32>,
}

impl WorldIndex {
    /// Builds every index from a world. Pure, and called at most once per world.
    pub(crate) fn build(world: &World, options: IndexOptions) -> Self {
        let lanes = world.roads.lanes();
        let lane_grid = LaneGrid::build(lanes, world.bbox, options);

        let entries: Vec<BuildingEntry> = world
            .buildings
            .iter()
            .map(|b| {
                let bb = Bbox::from_points(b.footprint.iter().copied());
                BuildingEntry {
                    id: b.id.index(),
                    min: [bb.min.x, bb.min.y],
                    max: [bb.max.x, bb.max.y],
                }
            })
            .collect();
        // `bulk_load` is a deterministic sort-tile-recursive build: no randomness, and the
        // input is already in id order. Query results are sorted before they are returned
        // anyway, so the tree's shape can never reach an output.
        let buildings = RTree::bulk_load(entries);

        let mut predecessors = world.roads.connections().to_vec();
        predecessors.sort_by_key(|c| {
            (
                c.to_lane.index(),
                c.from_lane.index(),
                c.via.map_or(u32::MAX, |v| v.index()),
            )
        });
        let mut predecessor_offsets = vec![0u32; lanes.len() + 1];
        for c in &predecessors {
            if c.to_lane.as_usize() < lanes.len() {
                predecessor_offsets[c.to_lane.as_usize() + 1] += 1;
            }
        }
        for i in 0..lanes.len() {
            predecessor_offsets[i + 1] += predecessor_offsets[i];
        }

        Self {
            lane_grid,
            buildings,
            predecessors,
            predecessor_offsets,
        }
    }
}

/// A lane the spatial index matched, with everything the caller needs to place a vehicle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneMatch {
    /// Which lane, how far along it, how far off its centre.
    pub pos: LanePos,
    /// Horizontal distance from the query point to the lane centreline, metres.
    pub distance_m: f64,
    /// The point on the centreline itself.
    pub point: Vec3,
    /// The centreline segment the match lies on.
    ///
    /// Part of the tie-break, and not decoration: a point off the outside of a bend is
    /// equidistant from the two segments that meet at the vertex, and they give it
    /// *different* lateral offsets, because their normals differ. Without the segment in
    /// the key, which of the two answers came back would depend on the order the grid
    /// happened to visit its cells.
    pub segment: usize,
}

impl World {
    /// The lane position nearest `p`, over the whole world
    /// (03-interfaces.md §2, `RoadNetwork::project`).
    ///
    /// Ties are broken by `LaneId`, then by `s_m`, as the interface requires. `None` only
    /// when the world has no lanes at all.
    pub fn project(&self, p: Vec3) -> Option<LanePos> {
        self.nearest_lane_within(p, f64::INFINITY, None)
            .map(|m| m.pos)
    }

    /// The lane position nearest `p` within `radius_m`, optionally restricted to lanes
    /// that admit at least one of `class_filter`'s classes.
    ///
    /// `radius_m` may be infinite, which searches the whole world. The search expands
    /// ring by ring over the lane grid and stops as soon as no unscanned cell could hold
    /// anything closer, so a hit next to the query point costs one cell.
    pub fn nearest_lane_within(
        &self,
        p: Vec3,
        radius_m: f64,
        class_filter: Option<ClassMask>,
    ) -> Option<LaneMatch> {
        let grid = &self.index().lane_grid;
        if grid.entries.is_empty() {
            return None;
        }
        let (qx, qy) = grid.cell_of(p.x, p.y);
        // The largest ring that can still contain a cell of the grid: the Chebyshev
        // distance from the query cell to the farthest corner. Rings beyond it are empty,
        // so the search is bounded even for a query far outside the world.
        let by_grid = qx
            .abs()
            .max((qx - (grid.nx - 1)).abs())
            .max(qy.abs())
            .max((qy - (grid.ny - 1)).abs());
        let max_ring = if radius_m.is_finite() {
            by_grid.min((radius_m / grid.cell_m).ceil() as i64 + 1)
        } else {
            by_grid
        };

        let mut best: Option<LaneMatch> = None;
        let mut ring = 0i64;
        while ring <= max_ring {
            // Everything not yet scanned is at least `ring - 1` whole cells away.
            if let Some(b) = &best {
                if b.distance_m <= (ring - 1).max(0) as f64 * grid.cell_m {
                    break;
                }
            }
            for (ix, iy) in ring_cells(qx, qy, ring) {
                let Some(cell) = grid.clamp(ix, iy) else {
                    continue;
                };
                for entry in grid.cell_entries(cell) {
                    let lane = self.roads.lane(LaneId::new(entry.lane));
                    if let Some(mask) = class_filter {
                        if !lane.admits(mask) {
                            continue;
                        }
                    }
                    let hit = lane.project_on_segment(entry.segment as usize, p);
                    if hit.distance_m > radius_m {
                        continue;
                    }
                    let candidate = LaneMatch {
                        pos: LanePos::new(lane.id, hit.s_m, hit.d_m),
                        distance_m: hit.distance_m,
                        point: hit.point,
                        segment: hit.segment,
                    };
                    best = Some(match best {
                        None => candidate,
                        Some(current) => better_match(current, candidate),
                    });
                }
            }
            ring += 1;
        }
        best
    }

    /// The connections that arrive at `lane` — the reverse of
    /// [`crate::model::RoadNetwork::successors`], for backward search in a router.
    ///
    /// Sorted by `(from_lane, via)`.
    pub fn predecessors(&self, lane: LaneId) -> &[Connection] {
        let index = self.index();
        let i = lane.as_usize();
        if i + 1 >= index.predecessor_offsets.len() {
            return &[];
        }
        let from = index.predecessor_offsets[i] as usize;
        let to = index.predecessor_offsets[i + 1] as usize;
        &index.predecessors[from..to]
    }

    /// The lanes a vehicle on `lane` can continue onto, in id order and without repeats.
    ///
    /// A convenience over [`crate::model::RoadNetwork::successors`] for graph search:
    /// a movement across a junction appears there twice (once with its internal connector
    /// in `via` and once from that connector), and a router walking lane to lane wants
    /// each reachable lane once.
    pub fn successor_lanes(&self, lane: LaneId) -> Vec<LaneId> {
        let mut out: Vec<LaneId> = self
            .successors(lane)
            .iter()
            .map(|c| c.via.unwrap_or(c.to_lane))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Every building whose footprint envelope meets `area`, in id order.
    pub fn buildings_in_bbox(&self, area: Bbox) -> Vec<BuildingId> {
        let envelope = AABB::from_corners([area.min.x, area.min.y], [area.max.x, area.max.y]);
        let mut out: Vec<BuildingId> = self
            .index()
            .buildings
            .locate_in_envelope_intersecting(&envelope)
            .map(|e| BuildingId::new(e.id))
            .collect();
        out.sort_unstable();
        out
    }

    /// Every building within `radius_m` of `p` (envelope test), in id order.
    pub fn buildings_near(&self, p: Vec3, radius_m: f64) -> Vec<BuildingId> {
        let area = Bbox::new(
            Vec3::new(p.x - radius_m, p.y - radius_m, 0.0),
            Vec3::new(p.x + radius_m, p.y + radius_m, 0.0),
        );
        self.buildings_in_bbox(area)
            .into_iter()
            .filter(|id| {
                let b = &self.buildings[id.as_usize()];
                ring_distance_2d(&b.footprint, p) <= radius_m || b.contains_2d(p)
            })
            .collect()
    }

    /// The building nearest `p`, or `None` if the world has none.
    ///
    /// "Nearest" is by footprint envelope; ties go to the lower [`BuildingId`], which is
    /// what makes the answer reproducible.
    pub fn nearest_building(&self, p: Vec3) -> Option<BuildingId> {
        let index = self.index();
        let mut best: Option<(f64, u32)> = None;
        // The iterator is ascending in distance, so the first strictly larger distance
        // ends the search; equal distances are resolved by the lower id.
        for (entry, d2) in index
            .buildings
            .nearest_neighbor_iter_with_distance_2(&[p.x, p.y])
        {
            match best {
                None => best = Some((d2, entry.id)),
                Some((bd, _)) if d2 > bd => break,
                Some((bd, bid)) if d2 == bd && entry.id < bid => best = Some((d2, entry.id)),
                Some(_) => {}
            }
        }
        best.map(|(_, id)| BuildingId::new(id))
    }

    /// Every building the segment `a → b` passes through or over, in id order.
    ///
    /// The envelope query narrows the candidates; each candidate is then tested against
    /// its actual outer ring, so a segment that clips the corner of a bounding box but
    /// misses the building is not reported. This is the primitive an `ObstacleModel`
    /// (03-interfaces.md §2) builds `walls_crossed` and `obstructed_len_m` on.
    pub fn buildings_on_segment(&self, a: Vec3, b: Vec3) -> Vec<BuildingId> {
        let area = Bbox::new(Vec3::new(a.x, a.y, 0.0), Vec3::new(b.x, b.y, 0.0));
        self.buildings_in_bbox(area)
            .into_iter()
            .filter(|id| {
                let building = &self.buildings[id.as_usize()];
                building.contains_2d(a)
                    || building.contains_2d(b)
                    || ring_segment_crossings(&building.footprint, a, b) > 0
            })
            .collect()
    }

    /// How many times the segment `a → b` crosses the outer ring of `building`.
    ///
    /// Two for a segment that passes clean through, one for a segment with an endpoint
    /// inside — the count the Sommer 2011 obstacle model takes as its input
    /// (03-interfaces.md §2, 04-models.md §3.5).
    pub fn wall_crossings(&self, building: BuildingId, a: Vec3, b: Vec3) -> u32 {
        self.buildings
            .get(building.as_usize())
            .map_or(0, |bl| ring_segment_crossings(&bl.footprint, a, b))
    }
}

/// Picks the better of two matches: smaller distance, then smaller `LaneId`, then
/// smaller `s_m`, then the **later** segment.
///
/// The first two keys are what 03-interfaces.md §2 asks for. The last two make the order
/// total, which it has to be: a point off the outside of a bend is exactly equidistant
/// from the two segments meeting at the vertex, and they disagree about its lateral
/// offset, because their normals differ. Without a tie-break, which answer came back
/// would depend on the order the grid happened to visit its cells.
///
/// The segment tie goes to the *later* segment on purpose. [`Lane::segment_at`] resolves
/// an arc length that falls exactly on a vertex to the segment that **starts** there, so
/// choosing the same one here is what makes `to_xyz(project(p))` reproduce `p` rather
/// than land a couple of centimetres away on the far side of the bend.
///
/// The distance comparison is exact equality, never a tolerance: a tie here means two
/// bit-identical `f64` distances, so the tie-break is over genuinely indistinguishable
/// candidates and the choice is the same on every platform.
fn better_match(current: LaneMatch, candidate: LaneMatch) -> LaneMatch {
    let better = match candidate.distance_m.partial_cmp(&current.distance_m) {
        Some(core::cmp::Ordering::Less) => true,
        Some(core::cmp::Ordering::Equal) => {
            let a = (current.pos.lane.index(), current.pos.s_m);
            let b = (candidate.pos.lane.index(), candidate.pos.s_m);
            b < a || (b == a && candidate.segment > current.segment)
        }
        _ => false,
    };
    if better { candidate } else { current }
}

/// The cells at Chebyshev distance exactly `ring` from `(cx, cy)`.
///
/// Emitted in a fixed order — bottom row west to east, then top row, then the two sides —
/// so a caller that does not sort still sees the same sequence everywhere. Nothing
/// depends on the order, because every result is ordered by the tie-break, but a fixed
/// order makes a failing test reproducible.
fn ring_cells(cx: i64, cy: i64, ring: i64) -> Vec<(i64, i64)> {
    if ring == 0 {
        return vec![(cx, cy)];
    }
    let mut out = Vec::with_capacity((8 * ring) as usize);
    for ix in cx - ring..=cx + ring {
        out.push((ix, cy - ring));
        out.push((ix, cy + ring));
    }
    for iy in cy - ring + 1..=cy + ring - 1 {
        out.push((cx - ring, iy));
        out.push((cx + ring, iy));
    }
    out
}

/// The horizontal distance from `p` to a closed ring's boundary.
///
/// The geometry lives in [`crate::model::ring_distance_sq_2d`] — one implementation, so
/// this answer and the junction-shape invariant cannot drift apart — and this is the
/// square root of it.
fn ring_distance_2d(ring: &[Vec3], p: Vec3) -> f64 {
    math::sqrt(crate::model::ring_distance_sq_2d(ring, p))
}

/// How many times the segment `a → b` crosses a closed ring, counted with the usual
/// orientation test.
///
/// Cross products only — no transcendental, no division — so the answer is exact and
/// identical on every platform. A segment that touches a vertex exactly is counted once,
/// by the half-open convention on the ring segment's parameter.
fn ring_segment_crossings(ring: &[Vec3], a: Vec3, b: Vec3) -> u32 {
    let mut count = 0;
    for w in ring.windows(2) {
        if segments_intersect(a, b, w[0], w[1]) {
            count += 1;
        }
    }
    count
}

/// True if two polylines have a point in common, including a shared endpoint.
///
/// The generator uses it to decide which movements through a junction conflict; the
/// obstacle queries use the same primitive against building rings, so "crossing" means
/// the same thing everywhere.
pub(crate) fn polylines_cross(a: &[Vec3], b: &[Vec3]) -> bool {
    a.windows(2).any(|p| {
        b.windows(2)
            .any(|q| segments_intersect(p[0], p[1], q[0], q[1]))
    })
}

/// True if the open segments `p→p2` and `q→q2` intersect, including collinear overlap.
fn segments_intersect(p: Vec3, p2: Vec3, q: Vec3, q2: Vec3) -> bool {
    let d1 = cross_2d(q, q2, p);
    let d2 = cross_2d(q, q2, p2);
    let d3 = cross_2d(p, p2, q);
    let d4 = cross_2d(p, p2, q2);
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    (d1 == 0.0 && on_segment(q, q2, p))
        || (d2 == 0.0 && on_segment(q, q2, p2))
        || (d3 == 0.0 && on_segment(p, p2, q))
        || (d4 == 0.0 && on_segment(p, p2, q2))
}

/// `(b - a) × (c - a)` in the horizontal plane.
fn cross_2d(a: Vec3, b: Vec3, c: Vec3) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// True if `c`, known to be collinear with `a→b`, lies within its bounding box.
fn on_segment(a: Vec3, b: Vec3, c: Vec3) -> bool {
    c.x >= a.x.min(b.x) && c.x <= a.x.max(b.x) && c.y >= a.y.min(b.y) && c.y <= a.y.max(b.y)
}

/// A summary of an index, for tests and for the import report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStats {
    /// Grid cells along `x`.
    pub grid_nx: i64,
    /// Grid cells along `y`.
    pub grid_ny: i64,
    /// Segment references stored in the grid (a segment counts once per cell it touches).
    pub grid_entries: usize,
    /// Building envelopes in the R-tree.
    pub building_entries: usize,
    /// Connection records in the reverse adjacency.
    pub predecessor_entries: usize,
    /// How many cells hold how many entries, for spotting a pathological grid.
    pub occupancy: BTreeMap<usize, usize>,
}

impl World {
    /// A summary of the spatial indices, building them if they are not built yet.
    pub fn index_stats(&self) -> IndexStats {
        let index = self.index();
        let grid = &index.lane_grid;
        let mut occupancy = BTreeMap::new();
        for c in 0..(grid.nx * grid.ny) as usize {
            let n = grid.cell_entries(c).len();
            *occupancy.entry(n).or_insert(0) += 1;
        }
        IndexStats {
            grid_nx: grid.nx,
            grid_ny: grid.ny,
            grid_entries: grid.entries.len(),
            building_entries: index.buildings.size(),
            predecessor_entries: index.predecessors.len(),
            occupancy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Vec3 {
        Vec3::new_2d(x, y)
    }

    #[test]
    fn segments_intersect_in_the_cases_that_matter() {
        // Proper crossing.
        assert!(segments_intersect(
            p(0.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(10.0, 0.0)
        ));
        // Parallel, never meeting.
        assert!(!segments_intersect(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(0.0, 1.0),
            p(10.0, 1.0)
        ));
        // Touching at an endpoint: a merge, which counts.
        assert!(segments_intersect(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0)
        ));
        // Collinear overlap.
        assert!(segments_intersect(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(5.0, 0.0),
            p(15.0, 0.0)
        ));
        // Collinear but disjoint.
        assert!(!segments_intersect(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(11.0, 0.0),
            p(15.0, 0.0)
        ));
        // A T-junction: one segment's endpoint lies on the other.
        assert!(segments_intersect(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(5.0, 0.0),
            p(5.0, 10.0)
        ));
        // Near miss.
        assert!(!segments_intersect(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(5.0, 0.001),
            p(5.0, 10.0)
        ));
    }

    #[test]
    fn polylines_cross_uses_every_segment_pair() {
        let a = [p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)];
        let b = [p(20.0, 5.0), p(9.0, 5.0)];
        assert!(polylines_cross(&a, &b));
        let c = [p(20.0, 20.0), p(30.0, 30.0)];
        assert!(!polylines_cross(&a, &c));
    }

    #[test]
    fn ring_cells_walk_the_border_exactly_once() {
        assert_eq!(ring_cells(3, 4, 0), vec![(3, 4)]);
        let ring1 = ring_cells(0, 0, 1);
        assert_eq!(ring1.len(), 8);
        let mut sorted = ring1.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 8, "no cell is visited twice");
        assert!(
            ring1.iter().all(|(x, y)| x.abs().max(y.abs()) == 1),
            "every cell is exactly one step away"
        );
        assert_eq!(ring_cells(5, 5, 3).len(), 24);
    }

    #[test]
    fn ring_distance_is_to_the_boundary() {
        let square = [
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.0, 0.0),
        ];
        assert_eq!(ring_distance_2d(&square, p(5.0, -2.0)), 2.0);
        assert_eq!(
            ring_distance_2d(&square, p(5.0, 5.0)),
            5.0,
            "inside, to the wall"
        );
        assert_eq!(
            ring_distance_2d(&square, p(-3.0, -4.0)),
            5.0,
            "to the corner"
        );
    }

    #[test]
    fn ring_segment_crossings_counts_walls() {
        let square = [
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.0, 0.0),
        ];
        assert_eq!(
            ring_segment_crossings(&square, p(-5.0, 5.0), p(15.0, 5.0)),
            2
        );
        assert_eq!(
            ring_segment_crossings(&square, p(-5.0, 5.0), p(5.0, 5.0)),
            1
        );
        assert_eq!(ring_segment_crossings(&square, p(2.0, 5.0), p(5.0, 5.0)), 0);
        assert_eq!(
            ring_segment_crossings(&square, p(-5.0, 20.0), p(15.0, 20.0)),
            0
        );
    }

    #[test]
    fn building_entry_distance_is_zero_inside_its_envelope() {
        let e = BuildingEntry {
            id: 0,
            min: [0.0, 0.0],
            max: [10.0, 10.0],
        };
        assert_eq!(e.distance_2(&[5.0, 5.0]), 0.0);
        assert_eq!(e.distance_2(&[13.0, 5.0]), 9.0);
        assert_eq!(e.distance_2(&[-3.0, -4.0]), 25.0);
        assert_eq!(e.envelope(), AABB::from_corners([0.0, 0.0], [10.0, 10.0]));
    }

    #[test]
    fn default_index_options_are_the_documented_ones() {
        let o = IndexOptions::default();
        assert_eq!(o.lane_grid_cell_m, 25.0);
        assert_eq!(o.max_lane_grid_cells, 4_000_000);
    }
}
