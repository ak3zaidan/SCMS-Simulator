//! The terrain profile a knife-edge model reads, and the edge extraction that turns it
//! into [`KnifeEdge`]s (04-models.md §3.5, `obstacle/terrain/knife-edge-p526`).
//!
//! The diffraction arithmetic itself — `ν`, `J(ν)`, the exact Fresnel-integral form, the
//! Deygout and ITU-R multiple-edge constructions — lives in [`crate::obstacle`], because
//! it is shared with the Boban vehicle knife edge. This module is only about the
//! *geometry*: given the ground along a link and the two antenna heights, which summits
//! diffract, how far along the path they are, and how high above the radio line.
//!
//! # The query this is written against
//!
//! `v2xw-world` is growing a terrain profile query. This module is written against the
//! **shape** of that query rather than against its type, so that it compiles today (on
//! [`v2xw_world::model::Terrain::height_at`], which exists and is exported) and needs no
//! change when the world-side query lands.
//!
//! As of this writing the world crate carries `src/los.rs` with
//!
//! ```text
//! pub fn terrain_profile(world: &World, a: Vec3, b: Vec3, params: &ProfileParams)
//!     -> Result<TerrainProfile>;
//! pub struct ProfileSample { pub s_m: f64, pub x_m: f64, pub y_m: f64,
//!                            pub ground_m: f64, pub line_m: f64 }
//! pub struct TerrainProfile { pub a: Vec3, pub b: Vec3, pub total_m: f64,
//!                             pub spacing_m: f64, pub has_terrain: bool,
//!                             pub obstruction_threshold_m: f64,
//!                             pub samples: Vec<ProfileSample> }
//! ```
//!
//! but `los` is **not yet declared in `v2xw-world/src/lib.rs`**, so nothing outside that
//! crate can name it and this module cannot depend on it. The adapter is therefore a
//! field-for-field map the caller writes in one line, and the only two fields it needs are
//! `s_m` and `ground_m`:
//!
//! ```text
//! let p = v2xw_world::los::terrain_profile(world, a, b, &params)?;
//! let profile = TerrainProfile::from_along_ground(
//!     p.samples.iter().map(|s| (s.s_m, s.ground_m)),
//! )?;
//! let edges = profile.knife_edges(tx.pos.z, rx.pos.z);
//! ```
//!
//! Three properties are assumed of whatever lands, and each is why the adapter is shaped
//! the way it is:
//!
//! 1. **`ground_m` is absolute**, metres above the world's `z = 0`, like
//!    [`v2xw_world::model::Terrain::height_at`] — and unlike a
//!    [`crate::types::RadioEndpoint`]'s `z`, which is an antenna height *above the local
//!    ground*. Mixing the two is the defect this module exists to remove, and it is why
//!    the adapter above takes `ground_m` and **not** `line_m`: the world-side `line_m` is
//!    `a.z + (b.z − a.z)·f`, i.e. the straight line between the two *endpoint `z` values*,
//!    which is the radio line only when those values are already elevations. Recomputing
//!    the line here from `ground(a) + z_a` and `ground(b) + z_b` costs two additions and
//!    is right either way.
//! 2. **Endpoints are included and the order is along-path**, so the first and last
//!    samples are the ground under the transmitter and the receiver.
//!    [`TerrainProfile::from_heights`] and [`TerrainProfile::from_along_ground`] both rely
//!    on it to place the line.
//! 3. **Spacing need not be even.** [`GroundProfile`] reads `(along_m, ground_m)` pairs,
//!    so a profile taken at the DEM's own post crossings — which is the better query —
//!    works unchanged.
//!
//! One thing the world-side query does that this module deliberately does not: it answers
//! `ground_m = 0.0` where the point is off the grid, with a separate `has_terrain` flag.
//! A profile assembled from those samples would see a cliff at the edge of an imported
//! DEM. [`TerrainProfile::sample`] reads [`v2xw_world::model::Terrain::height_at`]
//! directly for that reason and returns `None` rather than a cliff; a caller adapting the
//! world-side profile should check `has_terrain` and the grid extent itself.
//!
//! # The defect this fixes: an antenna height is not an elevation
//!
//! [`crate::types::RadioEndpoint`]'s `pos.z` is documented as "the antenna height above
//! the local ground". The ground height a DEM reports is an elevation above the world's
//! datum. The previous edge extraction compared the two directly — a ground height against
//! a line interpolated between the two `z` values — so on a world whose DEM sits at, say,
//! 200 m, *every* sample was 198.5 m "above" the radio line and every link was reported
//! obstructed by a 198 m knife edge. The clearance profile here is
//!
//! ```text
//! clearance(f) = ground(f) − [ (ground(a) + z_a) + f · ((ground(b) + z_b) − (ground(a) + z_a)) ]
//! ```
//!
//! which is zero at both endpoints by construction and is a real height above the line in
//! between. A world at `z = 0` — every procedural world, and every test world — gives
//! exactly the same answer as before, which is why the change is invisible to the existing
//! tests and decisive on an imported DEM.
//!
//! # What the extraction ignores
//!
//! * **Earth curvature.** The radio line is a straight chord, not a ray bent by the
//!   4/3-k-factor atmosphere. The bulge is `d1·d2/(2·a_e)`: at the worst case this crate
//!   models — a 1 km link with the summit at mid-path — that is 250,000 / (2 · 8.495e6) m,
//!   about 1.5 cm, two orders of magnitude below anything a 30 m-post DEM resolves. It is
//!   recorded as an `ignores` entry rather than implemented.
//! * **Rounded ridges and ground reflection**, which are P.526's other cases
//!   ([`crate::obstacle`] records them too).
//! * **Foliage on the summit**, which is `obstacle/foliage/boban-mel`'s business.

use serde::{Deserialize, Serialize};
use v2xw_core::geom::Vec3;
use v2xw_world::model::World;

use crate::types::{EdgeSource, KnifeEdge};

/// How many points a profile is sampled at by default.
///
/// Sixty-four, which is what `obstacle/terrain/knife-edge-p526`'s card declares (and
/// carries as `todo-calibrate`, because nothing cited prescribes a spacing): on a 1 km
/// link that is a point every 15.9 m, finer than the 30 m post spacing of the DEM the
/// world importer reads.
pub const DEFAULT_PROFILE_SAMPLES: usize = 64;

/// How many edges the extraction keeps, at most.
///
/// Eight. The Deygout construction recurses over the edge list, so an unbounded list makes
/// a rough profile quadratic in a quantity — the sample count — that has nothing to do with
/// the terrain. Eight is more edges than either multiple-edge construction is meaningful
/// over (ITU-R's own modified Epstein-Peterson is stated for the principal edges of a
/// path), and the ones kept are the highest, which are the ones that diffract. Recorded as
/// a design choice on the card.
pub const DEFAULT_MAX_EDGES: usize = 8;

/// One sample of the ground along a link.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProfilePoint {
    /// Distance from the transmitter along the horizontal path, metres.
    pub along_m: f64,
    /// Ground height, metres above the world's `z = 0` — an **elevation**, not a height
    /// above the radio line.
    pub ground_m: f64,
}

impl ProfilePoint {
    /// A sample.
    #[must_use]
    pub const fn new(along_m: f64, ground_m: f64) -> Self {
        Self { along_m, ground_m }
    }
}

/// What the knife-edge extraction needs of a terrain query.
///
/// A trait rather than a concrete type so that the world-side query can be adapted without
/// the diffraction model changing: anything that can answer "how long is the path" and
/// "where is the ground at sample `i`" is a profile. It deliberately does **not** assume
/// even spacing, because the better world-side query samples at the DEM's own post
/// crossings.
pub trait GroundProfile {
    /// The total horizontal path length, metres.
    fn total_m(&self) -> f64;

    /// How many samples there are.
    fn len(&self) -> usize;

    /// Sample `i`, or `None` past the end.
    fn point(&self, i: usize) -> Option<ProfilePoint>;

    /// True when there is nothing to read.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The ground along one link, sampled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainProfile {
    /// The total horizontal path length, metres.
    pub total_m: f64,
    /// The samples, in along-path order, first at the transmitter and last at the
    /// receiver.
    pub points: Vec<ProfilePoint>,
}

impl TerrainProfile {
    /// The profile between two endpoints, sampled from the world's DEM.
    ///
    /// `None` when the world has no terrain — "the ground is the zero plane and a link
    /// cannot be blocked by it" — when the endpoints coincide, or when fewer than three
    /// samples were asked for, since a profile needs an interior point to have a summit in.
    ///
    /// Also `None` when any sample falls outside the DEM's grid, so a link that runs off
    /// the imported terrain is not reported as blocked by the edge of it.
    ///
    /// This is the adapter over the query that exists today
    /// ([`v2xw_world::model::Terrain::height_at`], which answers `None` outside the grid —
    /// unlike [`World::ground_height_at`], which answers `0.0` and would put a cliff
    /// there). When the world-side `terrain_profile` lands, the body becomes one call and
    /// [`TerrainProfile::from_heights`]; the signature here does not change.
    #[must_use]
    pub fn sample(world: &World, a: Vec3, b: Vec3, samples: usize) -> Option<Self> {
        let terrain = world.terrain.as_ref()?;
        let total_m = a.distance_2d(b);
        if !(total_m.is_finite() && total_m > 0.0) || samples < 3 {
            return None;
        }
        let last = samples - 1;
        let mut points = Vec::with_capacity(samples);
        for i in 0..samples {
            let f = i as f64 / last as f64;
            let p = a.lerp(b, f);
            // `Terrain::height_at` and not `World::ground_height_at`: the latter answers
            // `0.0` outside the grid, and a link that runs off the DEM would then see a
            // cliff at the grid's edge and be reported blocked by it. A profile that is
            // not wholly inside the grid is `None` — assumption 4 of the module docs, and
            // the same answer the world-side query is expected to give.
            points.push(ProfilePoint::new(
                total_m * f,
                terrain.height_at(p.x, p.y)?,
            ));
        }
        Some(Self { total_m, points })
    }

    /// A profile from evenly spaced heights: the adapter for a world-side query that
    /// returns the heights alone.
    ///
    /// `heights` are absolute ground elevations, first at the transmitter and last at the
    /// receiver, so `n` heights span `n − 1` intervals of `total_m / (n − 1)`. `None` for
    /// fewer than three heights or a non-positive length.
    #[must_use]
    pub fn from_heights(total_m: f64, heights: &[f64]) -> Option<Self> {
        if heights.len() < 3 || !(total_m.is_finite() && total_m > 0.0) {
            return None;
        }
        let last = heights.len() - 1;
        let points = heights
            .iter()
            .enumerate()
            .map(|(i, h)| ProfilePoint::new(total_m * i as f64 / last as f64, *h))
            .collect();
        Some(Self { total_m, points })
    }

    /// A profile from `(along_m, ground_m)` pairs — the one-line adapter for the
    /// world-side query (see the module docs).
    ///
    /// `None` on the same conditions as [`TerrainProfile::from_points`].
    #[must_use]
    pub fn from_along_ground(samples: impl IntoIterator<Item = (f64, f64)>) -> Option<Self> {
        Self::from_points(
            samples
                .into_iter()
                .map(|(along_m, ground_m)| ProfilePoint::new(along_m, ground_m))
                .collect(),
        )
    }

    /// A profile from irregular samples: the adapter for a world-side query that returns
    /// the DEM's own post crossings.
    ///
    /// The samples must be in along-path order; `total_m` is the last sample's `along_m`.
    /// `None` for fewer than three samples or a non-monotone ordering, because a summit
    /// test over a shuffled profile is meaningless and silently accepting one would make
    /// the loss depend on the query's traversal order.
    #[must_use]
    pub fn from_points(points: Vec<ProfilePoint>) -> Option<Self> {
        if points.len() < 3 {
            return None;
        }
        if !points
            .iter()
            .all(|p| p.along_m.is_finite() && p.ground_m.is_finite())
        {
            return None;
        }
        if points.windows(2).any(|w| w[1].along_m < w[0].along_m) {
            return None;
        }
        let total_m = points[points.len() - 1].along_m - points[0].along_m;
        if !(total_m.is_finite() && total_m > 0.0) {
            return None;
        }
        Some(Self { total_m, points })
    }

    /// The knife edges along this profile for two antenna heights above local ground.
    ///
    /// Shorthand for [`knife_edges`] with [`EdgeExtraction::default`].
    #[must_use]
    pub fn knife_edges(&self, tx_agl_m: f64, rx_agl_m: f64) -> Vec<KnifeEdge> {
        knife_edges(self, tx_agl_m, rx_agl_m, EdgeExtraction::default())
    }
}

impl GroundProfile for TerrainProfile {
    fn total_m(&self) -> f64 {
        self.total_m
    }

    fn len(&self) -> usize {
        self.points.len()
    }

    fn point(&self, i: usize) -> Option<ProfilePoint> {
        self.points.get(i).copied()
    }
}

impl GroundProfile for [ProfilePoint] {
    fn total_m(&self) -> f64 {
        match (self.first(), self.last()) {
            (Some(a), Some(b)) => b.along_m - a.along_m,
            _ => 0.0,
        }
    }

    fn len(&self) -> usize {
        <[ProfilePoint]>::len(self)
    }

    fn point(&self, i: usize) -> Option<ProfilePoint> {
        self.get(i).copied()
    }
}

/// How the edge list is extracted from a profile.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EdgeExtraction {
    /// At most this many edges, the highest kept ([`DEFAULT_MAX_EDGES`]).
    pub max_edges: usize,
    /// Whether to keep summits that sit **below** the radio line.
    ///
    /// True by default, and it matters: a summit a few centimetres below the line still
    /// intrudes into the first Fresnel zone and still diffracts, which is exactly what
    /// `ν > −0.78` in ITU-R P.526 Eq. 31 says. Dropping them at extraction would make the
    /// loss jump from `J(0⁻) ≈ 6 dB` to `0 dB` as a summit crossed the line by a
    /// millimetre.
    ///
    /// The filter that decides whether such an edge contributes is the diffraction model's
    /// own — [`crate::obstacle::knife_edge_loss_db`] returns zero below `ν = −0.78` — and
    /// that is where it belongs, because it is the only place that knows the wavelength:
    /// [`crate::traits::ObstacleModel::los`] has no frequency argument, so the extraction
    /// cannot compute `ν` and must not pretend to.
    pub include_below_line: bool,
}

impl Default for EdgeExtraction {
    fn default() -> Self {
        Self {
            max_edges: DEFAULT_MAX_EDGES,
            include_below_line: true,
        }
    }
}

impl EdgeExtraction {
    /// Only summits above the radio line, at most `max_edges` of them.
    #[must_use]
    pub const fn above_line_only(max_edges: usize) -> Self {
        Self {
            max_edges,
            include_below_line: false,
        }
    }
}

/// The height of the straight radio line above the datum at path fraction `f`.
///
/// `(ground_a + z_a) + f · ((ground_b + z_b) − (ground_a + z_a))`, with both `z` an antenna
/// height above the local ground. This is the one line the previous implementation got
/// wrong; see the module documentation.
#[must_use]
pub fn radio_line_height_m(
    tx_ground_m: f64,
    tx_agl_m: f64,
    rx_ground_m: f64,
    rx_agl_m: f64,
    f: f64,
) -> f64 {
    let a = tx_ground_m + tx_agl_m;
    let b = rx_ground_m + rx_agl_m;
    a + (b - a) * f
}

/// Every diffracting summit along a profile, in along-path order.
///
/// A summit is a **local maximum of the clearance profile** — ground height minus the
/// radio line's height — taken over the interior samples, with the plateau rule
/// `clearance[i] > clearance[i−1] && clearance[i] >= clearance[i+1]` so that a flat ridge
/// yields its first sample and never two adjacent edges.
///
/// The returned `h_m` is signed: positive above the line, negative below it, which is the
/// convention [`KnifeEdge::h_m`] documents and what
/// [`crate::obstacle::knife_edge_parameter`] expects.
///
/// Determinism: the sort that applies `max_edges` is by `(−h_m, along_m)` with
/// [`f64::total_cmp`] and the result is re-sorted into along-path order, so the list is a
/// pure function of the profile and never of a traversal order.
#[must_use]
pub fn knife_edges<P: GroundProfile + ?Sized>(
    profile: &P,
    tx_agl_m: f64,
    rx_agl_m: f64,
    opts: EdgeExtraction,
) -> Vec<KnifeEdge> {
    let n = profile.len();
    if n < 3 || opts.max_edges == 0 {
        return Vec::new();
    }
    let total = profile.total_m();
    if !(total.is_finite() && total > 0.0) {
        return Vec::new();
    }
    let (Some(first), Some(last)) = (profile.point(0), profile.point(n - 1)) else {
        return Vec::new();
    };
    let start_m = first.along_m;

    // The clearance profile, computed once.
    let mut clearance = Vec::with_capacity(n);
    for i in 0..n {
        let Some(p) = profile.point(i) else {
            return Vec::new();
        };
        let f = ((p.along_m - start_m) / total).clamp(0.0, 1.0);
        clearance.push(
            p.ground_m
                - radio_line_height_m(first.ground_m, tx_agl_m, last.ground_m, rx_agl_m, f),
        );
    }

    let mut edges: Vec<KnifeEdge> = Vec::new();
    for i in 1..n - 1 {
        let h = clearance[i];
        if !h.is_finite() {
            continue;
        }
        if !(h > clearance[i - 1] && h >= clearance[i + 1]) {
            continue;
        }
        if h <= 0.0 && !opts.include_below_line {
            continue;
        }
        let Some(p) = profile.point(i) else { continue };
        // Never zero: `ν` divides by `d1·d2`, and a summit at a sample coincident with an
        // endpoint would otherwise produce an infinity. A millimetre is the world's own
        // position quantum, so the clamp cannot move a real summit.
        let d1 = (p.along_m - start_m).max(1e-3);
        let d2 = (total - d1).max(1e-3);
        edges.push(KnifeEdge {
            d1_m: d1,
            d2_m: d2,
            h_m: h,
            source: EdgeSource::Terrain,
        });
    }

    if edges.len() > opts.max_edges {
        // Keep the highest; ties by position, so the choice is total.
        edges.sort_by(|a, b| {
            b.h_m
                .total_cmp(&a.h_m)
                .then(a.d1_m.total_cmp(&b.d1_m))
        });
        edges.truncate(opts.max_edges);
    }
    edges.sort_by(|a, b| a.d1_m.total_cmp(&b.d1_m));
    edges
}

/// True when any edge in the list actually blocks the line — the test that decides whether
/// a link is classified [`crate::types::LosClass::NlosT`].
///
/// An edge below the line diffracts (it is kept, so the loss model can see it) but does not
/// *obstruct*: calling such a link NLOS-terrain would put a link with 6 dB of grazing loss
/// into the same class as one behind a ridge, and the path-loss presets of 04-models.md
/// §3.2 are selected by that class.
#[must_use]
pub fn any_edge_obstructs(edges: &[KnifeEdge]) -> bool {
    edges.iter().any(|e| e.h_m > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric;
    use crate::obstacle::{knife_edge_loss_db, knife_edge_parameter};

    /// A profile with a triangular hill of `peak_m` at mid-path over flat ground at
    /// `base_m`.
    fn hill(total_m: f64, base_m: f64, peak_m: f64, samples: usize) -> TerrainProfile {
        let last = samples - 1;
        let heights: Vec<f64> = (0..samples)
            .map(|i| {
                let f = i as f64 / last as f64;
                let t = 1.0 - (2.0 * f - 1.0).abs();
                base_m + peak_m * t
            })
            .collect();
        TerrainProfile::from_heights(total_m, &heights).expect("a hill is a profile")
    }

    #[test]
    fn a_flat_profile_at_any_elevation_has_no_edges() {
        // The defect this module fixes: a world whose DEM sits at 200 m must not report
        // every link as blocked by a 198 m knife edge.
        for base in [0.0, 200.0, 4_000.0] {
            let flat = hill(1_000.0, base, 0.0, 65);
            let edges = flat.knife_edges(1.5, 1.5);
            assert!(
                edges.is_empty() || !any_edge_obstructs(&edges),
                "flat ground at {base} m produced {edges:?}"
            );
        }
    }

    #[test]
    fn a_hill_between_two_antennas_is_one_knife_edge_above_the_line() {
        // 1 km link, ground at 100 m, a 30 m hill at mid-path, antennas 1.5 m up. The
        // radio line runs from 101.5 m to 101.5 m, so the summit is 130 − 101.5 = 28.5 m
        // above it.
        let profile = hill(1_000.0, 100.0, 30.0, 65);
        let edges = profile.knife_edges(1.5, 1.5);
        assert_eq!(edges.len(), 1, "{edges:?}");
        let e = edges[0];
        assert!((e.h_m - 28.5).abs() < 1e-9, "h = {}", e.h_m);
        assert!((e.d1_m - 500.0).abs() < 1e-6, "d1 = {}", e.d1_m);
        assert!((e.d1_m + e.d2_m - 1_000.0).abs() < 1e-6);
        assert_eq!(e.source, EdgeSource::Terrain);
        assert!(any_edge_obstructs(&edges));
        // And it costs real decibels: ν is large, so J(ν) is well past the 6 dB grazing
        // value.
        let lambda = numeric::wavelength_m(5.9e9);
        let nu = knife_edge_parameter(e.h_m, e.d1_m, e.d2_m, lambda);
        assert!(nu > 5.0, "nu = {nu}");
        assert!(knife_edge_loss_db(nu) > 25.0);
    }

    #[test]
    fn a_taller_antenna_clears_the_hill() {
        let profile = hill(1_000.0, 100.0, 30.0, 65);
        // Both antennas on 60 m masts: the line runs at 160 m and the 130 m summit is
        // 30 m below it. The edge is still reported, because it may intrude into the
        // Fresnel zone, but it does not obstruct — and at 5.9 GHz its `ν` is far below
        // −0.78, so the loss model charges nothing.
        let edges = knife_edges(&profile, 60.0, 60.0, EdgeExtraction::default());
        assert_eq!(edges.len(), 1);
        assert!((edges[0].h_m + 30.0).abs() < 1e-9, "{:?}", edges[0]);
        assert!(!any_edge_obstructs(&edges));
        let lambda = numeric::wavelength_m(5.9e9);
        let nu = knife_edge_parameter(edges[0].h_m, edges[0].d1_m, edges[0].d2_m, lambda);
        assert!(nu < -0.78, "nu = {nu}");
        assert_eq!(knife_edge_loss_db(nu), 0.0);
        // Asking for above-line edges only drops it entirely.
        assert!(
            knife_edges(&profile, 60.0, 60.0, EdgeExtraction::above_line_only(8)).is_empty()
        );
    }

    /// The grazing discontinuity `include_below_line` exists to remove: a summit a
    /// millimetre either side of the line must not change the loss by 6 dB.
    #[test]
    fn a_grazing_summit_is_continuous_across_the_line() {
        let lambda = numeric::wavelength_m(5.9e9);
        let loss_at = |peak: f64| {
            let profile = hill(1_000.0, 0.0, peak, 65);
            let edges = knife_edges(&profile, 1.5, 1.5, EdgeExtraction::default());
            match edges.first() {
                None => 0.0,
                Some(e) => {
                    knife_edge_loss_db(knife_edge_parameter(e.h_m, e.d1_m, e.d2_m, lambda))
                }
            }
        };
        // Antennas at 1.5 m, so a 1.5 m hill grazes exactly.
        let just_below = loss_at(1.499);
        let just_above = loss_at(1.501);
        assert!(
            (just_above - just_below).abs() < 0.1,
            "a millimetre of hill moved the loss from {just_below} dB to {just_above} dB"
        );
        // …and the grazing value is the 6.02 dB J(0) the curve gives.
        let grazing = loss_at(1.5);
        assert!((grazing - 6.02).abs() < 0.02, "J(0) = {grazing}");
    }

    #[test]
    fn two_hills_are_two_edges_in_along_path_order() {
        // Two summits: 20 m at a quarter of the way and 40 m at three quarters.
        let samples = 81;
        let last = samples - 1;
        let heights: Vec<f64> = (0..samples)
            .map(|i| {
                let f = i as f64 / last as f64;
                let a = (1.0 - (4.0 * (f - 0.25)).abs()).max(0.0) * 20.0;
                let b = (1.0 - (4.0 * (f - 0.75)).abs()).max(0.0) * 40.0;
                a.max(b)
            })
            .collect();
        let profile = TerrainProfile::from_heights(2_000.0, &heights).unwrap();
        let edges = knife_edges(&profile, 1.5, 1.5, EdgeExtraction::default());
        assert_eq!(edges.len(), 2, "{edges:?}");
        assert!(edges[0].d1_m < edges[1].d1_m, "along-path order");
        assert!((edges[0].d1_m - 500.0).abs() < 30.0, "{:?}", edges[0]);
        assert!((edges[1].d1_m - 1_500.0).abs() < 30.0, "{:?}", edges[1]);
        assert!(edges[1].h_m > edges[0].h_m);
    }

    #[test]
    fn the_edge_cap_keeps_the_highest_and_is_order_independent() {
        // A saw-tooth with many summits of strictly increasing height.
        let samples = 41;
        let heights: Vec<f64> = (0..samples)
            .map(|i| if i % 2 == 1 { f64::from(i) } else { 0.0 })
            .collect();
        let profile = TerrainProfile::from_heights(1_000.0, &heights).unwrap();
        let all = knife_edges(&profile, 0.0, 0.0, EdgeExtraction::default());
        assert_eq!(all.len(), DEFAULT_MAX_EDGES);
        // The highest twenty summits are at odd indices; the cap keeps the eight tallest,
        // which are the last eight odd samples.
        let mut sorted = all.clone();
        sorted.sort_by(|a, b| b.h_m.total_cmp(&a.h_m));
        assert!((sorted[0].h_m - 39.0).abs() < 1e-9, "{:?}", sorted[0]);
        assert!((sorted[7].h_m - 25.0).abs() < 1e-9, "{:?}", sorted[7]);
        // Still in along-path order after the cap.
        for w in all.windows(2) {
            assert!(w[0].d1_m < w[1].d1_m);
        }
        // Capping at zero yields nothing rather than panicking.
        assert!(
            knife_edges(
                &profile,
                0.0,
                0.0,
                EdgeExtraction {
                    max_edges: 0,
                    include_below_line: true
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn a_plateau_yields_one_edge_not_two() {
        // A flat-topped ridge: samples 3 and 4 are both at the peak.
        let heights = vec![0.0, 0.0, 0.0, 10.0, 10.0, 0.0, 0.0, 0.0, 0.0];
        let profile = TerrainProfile::from_heights(800.0, &heights).unwrap();
        let edges = knife_edges(&profile, 0.0, 0.0, EdgeExtraction::default());
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert!((edges[0].h_m - 10.0).abs() < 1e-9);
    }

    #[test]
    fn the_adapters_reject_what_they_cannot_read() {
        assert!(TerrainProfile::from_heights(1_000.0, &[0.0, 1.0]).is_none());
        assert!(TerrainProfile::from_heights(0.0, &[0.0, 1.0, 0.0]).is_none());
        assert!(TerrainProfile::from_heights(f64::NAN, &[0.0, 1.0, 0.0]).is_none());
        assert!(TerrainProfile::from_points(vec![ProfilePoint::new(0.0, 0.0)]).is_none());
        // Non-monotone samples are refused rather than silently sorted.
        assert!(
            TerrainProfile::from_points(vec![
                ProfilePoint::new(0.0, 0.0),
                ProfilePoint::new(100.0, 5.0),
                ProfilePoint::new(50.0, 0.0),
            ])
            .is_none()
        );
        // A non-finite height is refused.
        assert!(
            TerrainProfile::from_points(vec![
                ProfilePoint::new(0.0, 0.0),
                ProfilePoint::new(50.0, f64::INFINITY),
                ProfilePoint::new(100.0, 0.0),
            ])
            .is_none()
        );
        // The pair adapter is the irregular one with a map in front of it.
        assert!(TerrainProfile::from_along_ground([(0.0, 0.0), (1.0, 1.0)]).is_none());
        let paired =
            TerrainProfile::from_along_ground([(0.0, 0.0), (50.0, 7.0), (100.0, 0.0)]).unwrap();
        assert_eq!(paired.total_m, 100.0);
        assert_eq!(paired.points.len(), 3);
        assert_eq!(paired.knife_edges(0.0, 0.0).len(), 1);
        // The irregular adapter agrees with the even one on an even profile.
        let even = TerrainProfile::from_heights(100.0, &[0.0, 7.0, 0.0]).unwrap();
        let irregular = TerrainProfile::from_points(vec![
            ProfilePoint::new(0.0, 0.0),
            ProfilePoint::new(50.0, 7.0),
            ProfilePoint::new(100.0, 0.0),
        ])
        .unwrap();
        assert_eq!(even, irregular);
        assert_eq!(GroundProfile::total_m(&even), 100.0);
        assert_eq!(GroundProfile::len(&even), 3);
        assert!(!GroundProfile::is_empty(&even));
        assert_eq!(even.point(1), Some(ProfilePoint::new(50.0, 7.0)));
        assert_eq!(even.point(3), None);
        // The slice impl reads the same way.
        assert_eq!(GroundProfile::total_m(&irregular.points[..]), 100.0);
        assert_eq!(
            knife_edges(&irregular.points[..], 0.0, 0.0, EdgeExtraction::default()).len(),
            1
        );
    }

    #[test]
    fn the_radio_line_is_absolute_and_flat_when_both_ends_match() {
        // Both ends 1.5 m above ground at 200 m: the line is at 201.5 m everywhere.
        for f in [0.0, 0.25, 0.5, 1.0] {
            let h = radio_line_height_m(200.0, 1.5, 200.0, 1.5, f);
            assert!((h - 201.5).abs() < 1e-12, "f = {f}, h = {h}");
        }
        // A rising path interpolates.
        assert!((radio_line_height_m(0.0, 5.0, 100.0, 5.0, 0.5) - 55.0).abs() < 1e-12);
    }

    #[test]
    fn a_world_without_a_dem_has_no_profile() {
        let world = crate::testctx::tiny_world();
        assert!(
            TerrainProfile::sample(&world, Vec3::new(0.0, 0.0, 1.5), Vec3::new(500.0, 0.0, 1.5), 64)
                .is_none(),
            "the zero plane cannot obstruct a link"
        );
    }
}
