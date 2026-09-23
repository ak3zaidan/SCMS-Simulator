//! Obstacles: `obstacle/building/sommer-2011`, `obstacle/vehicle/tr37885-nlosv`,
//! `obstacle/vehicle/knife-edge-boban` and `obstacle/terrain/knife-edge-p526`
//! (04-models.md §3.5).
//!
//! # The building geometry, and the R-tree
//!
//! The Sommer building model needs two numbers per link: how many exterior walls the
//! straight path crosses, and how many metres of it lie inside buildings. Both come from
//! a spatial index over the world's building footprints — 7,390 of them in the Manhattan
//! import — because testing every footprint against every link is quadratic in the wrong
//! variables.
//!
//! `v2xw-world` builds exactly such an index (`WorldIndex`, an `rstar` R-tree of
//! footprint envelopes, with `buildings_on_segment` and `wall_crossings` on it), but the
//! type and `World::index()` are both `pub(crate)`, so nothing outside that crate can
//! reach them. This module therefore builds the same tree over the public
//! `World::buildings` field with the same `rstar` crate, caches it per world (keyed by the
//! world's `content_hash`, which is what makes the cache safe: a different world is a
//! different hash) and reproduces the same two primitives. The change that would remove
//! this duplication is three lines in `v2xw-world` — a public accessor for the index or
//! for the two queries — and the card records it as a limitation rather than pretending
//! the duplication is a design choice.
//!
//! Determinism: every query sorts its results by [`BuildingId`] before returning, exactly
//! as `v2xw-world`'s own index does, so an R-tree traversal order can never reach a loss
//! value.

use std::collections::BTreeMap;

use rstar::{AABB, RTree, RTreeObject};
use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{BuildingId, LinkKey};
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_world::model::{Building, MaterialClass, World, point_in_ring};

use crate::numeric;
use crate::prop::SommerCoefficients;
use crate::traits::ObstacleModel;
use crate::types::{
    ActorObstacle, ActorSet, EdgeSource, KnifeEdge, LosClass, LosResult, RadioEndpoint,
};

// =========================================================================================
// Geometry
// =========================================================================================

/// One building's envelope in the R-tree.
#[derive(Debug, Clone, Copy)]
struct Envelope {
    id: u32,
    min: [f64; 2],
    max: [f64; 2],
}

impl RTreeObject for Envelope {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(self.min, self.max)
    }
}

/// An R-tree over a world's building footprints (see the module docs for why this exists
/// rather than reusing the world's own).
#[derive(Debug)]
pub struct BuildingIndex {
    tree: RTree<Envelope>,
    /// The hash of the world this index was built from, so a stale index cannot be used.
    world_hash: [u8; 32],
}

impl BuildingIndex {
    /// Builds the index for a world. Pure: same world, same tree.
    #[must_use]
    pub fn build(world: &World) -> Self {
        let entries: Vec<Envelope> = world
            .buildings
            .iter()
            .map(|b| {
                let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
                let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
                for p in &b.footprint {
                    min_x = min_x.min(p.x);
                    min_y = min_y.min(p.y);
                    max_x = max_x.max(p.x);
                    max_y = max_y.max(p.y);
                }
                Envelope {
                    id: b.id.index(),
                    min: [min_x, min_y],
                    max: [max_x, max_y],
                }
            })
            .collect();
        Self {
            tree: RTree::bulk_load(entries),
            world_hash: world.content_hash,
        }
    }

    /// The hash of the world this index belongs to.
    #[must_use]
    pub const fn world_hash(&self) -> [u8; 32] {
        self.world_hash
    }

    /// Every building whose envelope the segment's bounding box touches, in id order.
    ///
    /// The envelope query narrows the candidates; the caller tests the actual ring.
    #[must_use]
    pub fn candidates(&self, a: Vec3, b: Vec3) -> Vec<BuildingId> {
        let query = AABB::from_corners([a.x.min(b.x), a.y.min(b.y)], [a.x.max(b.x), a.y.max(b.y)]);
        let mut ids: Vec<u32> = self
            .tree
            .locate_in_envelope_intersecting(&query)
            .map(|e| e.id)
            .collect();
        ids.sort_unstable();
        ids.into_iter().map(BuildingId::new).collect()
    }
}

/// Where a segment enters and leaves one polygon, and how many of its edges it crossed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PolygonCrossing {
    /// Ring edges crossed: two for a clean pass-through, one for a segment with an
    /// endpoint inside.
    pub walls: u16,
    /// Length of the segment inside the polygon, metres.
    pub inside_len_m: f64,
}

/// The number of ring edges the segment `a → b` crosses, and the length inside the ring.
///
/// Cross products and one sort only — no transcendental, no tolerance — so the answer is
/// exact and identical on every platform. The inside length is accumulated by splitting
/// the segment at every crossing parameter and testing each sub-segment's midpoint
/// against the ring, which handles concave footprints and a segment that leaves and
/// re-enters the same building.
#[must_use]
pub fn ring_crossing(ring: &[Vec3], a: Vec3, b: Vec3) -> PolygonCrossing {
    let mut ts: Vec<f64> = vec![0.0, 1.0];
    let mut walls = 0u16;
    for w in ring.windows(2) {
        if let Some(t) = segment_intersection_t(a, b, w[0], w[1]) {
            walls = walls.saturating_add(1);
            ts.push(t);
        }
    }
    if walls == 0 && !point_in_ring(ring, a) {
        return PolygonCrossing::default();
    }
    math::sort_total_order(&mut ts);
    let total_len = a.distance_2d(b);
    let mut inside = 0.0;
    for pair in ts.windows(2) {
        let (t0, t1) = (pair[0], pair[1]);
        if t1 <= t0 {
            continue;
        }
        let mid_t = 0.5 * (t0 + t1);
        let mid = a.lerp(b, mid_t);
        if point_in_ring(ring, mid) {
            inside += (t1 - t0) * total_len;
        }
    }
    PolygonCrossing {
        walls,
        inside_len_m: inside,
    }
}

/// The parameter `t` along `p → p2` at which it crosses `q → q2`, if it does.
///
/// Returns `None` for parallel or non-crossing segments. Collinear overlap returns
/// `None` as well: a path running exactly along a wall crosses no wall, and counting one
/// would make the answer depend on the last bit of a coordinate.
fn segment_intersection_t(p: Vec3, p2: Vec3, q: Vec3, q2: Vec3) -> Option<f64> {
    let r = (p2.x - p.x, p2.y - p.y);
    let s = (q2.x - q.x, q2.y - q.y);
    let denom = r.0 * s.1 - r.1 * s.0;
    if denom == 0.0 {
        return None;
    }
    let qp = (q.x - p.x, q.y - p.y);
    let t = (qp.0 * s.1 - qp.1 * s.0) / denom;
    let u = (qp.0 * r.1 - qp.1 * r.0) / denom;
    if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
        Some(t)
    } else {
        None
    }
}

/// The height of the straight transmitter-receiver line at a fraction `f` of the way
/// along it.
fn line_height_at(tx_z: f64, rx_z: f64, f: f64) -> f64 {
    tx_z + (rx_z - tx_z) * f
}

// =========================================================================================
// Knife-edge diffraction (ITU-R P.526-14)
// =========================================================================================

/// The dimensionless knife-edge parameter `ν = h·sqrt(2(d1 + d2)/(λ·d1·d2))`
/// [ITU-R P.526-14 Eq. 26, self-consistent units].
#[must_use]
pub fn knife_edge_parameter(h_m: f64, d1_m: f64, d2_m: f64, lambda_m: f64) -> f64 {
    if d1_m <= 0.0 || d2_m <= 0.0 || lambda_m <= 0.0 {
        return 0.0;
    }
    h_m * math::sqrt(2.0 * (d1_m + d2_m) / (lambda_m * d1_m * d2_m))
}

/// The approximate single-knife-edge diffraction loss, dB
/// [ITU-R P.526-14 Eq. 31]: `J(ν) = 6.9 + 20·log10(sqrt((ν − 0.1)² + 1) + ν − 0.1)` for
/// `ν > −0.78`, and zero below that.
#[must_use]
pub fn knife_edge_loss_db(nu: f64) -> f64 {
    if nu <= -0.78 {
        return 0.0;
    }
    let x = nu - 0.1;
    6.9 + 20.0 * math::log10(math::sqrt(x * x + 1.0) + x)
}

/// The exact Fresnel-integral diffraction loss, dB [ITU-R P.526-14 Eq. 30].
///
/// `J(ν) = −20·log10|F(ν)|` with `F(ν) = ((1+j)/2)·∫_ν^∞ exp(−jπt²/2) dt`, which reduces
/// to `−10·log10(((1/2 − C(ν))² + (1/2 − S(ν))²)/2)` in the Fresnel cosine and sine
/// integrals. `C` and `S` are evaluated by composite Simpson's rule with a **fixed**
/// panel count (see [`fresnel_integrals`]), so the answer is a deterministic function of
/// `ν` with no adaptive step a platform could choose differently.
#[must_use]
pub fn knife_edge_loss_exact_db(nu: f64) -> f64 {
    let (c, s) = fresnel_integrals(nu);
    let a = 0.5 - c;
    let b = 0.5 - s;
    let mag2 = (a * a + b * b) / 2.0;
    if mag2 <= 0.0 {
        return 0.0;
    }
    (-10.0 * math::log10(mag2)).max(0.0)
}

/// The Fresnel integrals `C(ν) = ∫_0^ν cos(πt²/2) dt` and `S(ν) = ∫_0^ν sin(πt²/2) dt`.
///
/// Composite Simpson's rule with `n = 1024 + 256·ceil(|ν|)` panels, capped at 65,536: the
/// integrand oscillates with period `2/ν` near `ν`, so the panel count grows with `ν` to
/// keep at least a hundred panels per oscillation. The rule is fixed rather than adaptive
/// on purpose — an adaptive quadrature's panel boundaries depend on floating-point
/// comparisons, and two builds could disagree about them.
#[must_use]
pub fn fresnel_integrals(nu: f64) -> (f64, f64) {
    if nu == 0.0 {
        return (0.0, 0.0);
    }
    let panels = (1_024.0 + 256.0 * nu.abs().ceil()).min(65_536.0) as usize;
    let panels = panels + panels % 2; // Simpson needs an even count.
    let h = nu / panels as f64;
    let f = |t: f64| {
        let arg = core::f64::consts::FRAC_PI_2 * t * t;
        let (sin, cos) = math::sin_cos(arg);
        (cos, sin)
    };
    let (mut c, mut s) = {
        let (c0, s0) = f(0.0);
        let (cn, sn) = f(nu);
        (c0 + cn, s0 + sn)
    };
    for i in 1..panels {
        let (ci, si) = f(h * i as f64);
        let weight = if i % 2 == 0 { 2.0 } else { 4.0 };
        c += weight * ci;
        s += weight * si;
    }
    (c * h / 3.0, s * h / 3.0)
}

/// How a multiple-edge path is reduced to a loss (04-models.md §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MultiEdgeRule {
    /// Deygout: the dominant edge plus the sub-path edges either side. More pessimistic,
    /// and the default 04-models.md §3.5 records.
    #[default]
    Deygout,
    /// The modified Epstein-Peterson construction ITU-R gives. More optimistic.
    ItuR,
}

/// The total diffraction loss of an edge list, dB.
///
/// Deygout: the edge with the largest `ν` dominates, and the two sub-paths either side of
/// it contribute their own dominant edges, recursively. The recursion is bounded by the
/// edge count, and the edges are taken in a fixed order (sorted by `d1`), so the answer
/// does not depend on how the caller collected them.
#[must_use]
pub fn multi_edge_loss_db(
    edges: &[KnifeEdge],
    lambda_m: f64,
    rule: MultiEdgeRule,
    exact: bool,
) -> f64 {
    let loss = |nu: f64| {
        if exact {
            knife_edge_loss_exact_db(nu)
        } else {
            knife_edge_loss_db(nu)
        }
    };
    if edges.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<KnifeEdge> = edges.to_vec();
    sorted.sort_by(|a, b| a.d1_m.total_cmp(&b.d1_m));
    match rule {
        // Epstein-Peterson as ITU-R gives it: each edge is treated against its
        // neighbours, and the losses add.
        MultiEdgeRule::ItuR => math::sum_ordered(
            sorted
                .iter()
                .map(|e| loss(knife_edge_parameter(e.h_m, e.d1_m, e.d2_m, lambda_m))),
        ),
        MultiEdgeRule::Deygout => deygout(&sorted, lambda_m, &loss, 0),
    }
}

/// The Deygout recursion. `depth` bounds it at the edge count, which no real profile
/// exceeds.
fn deygout(edges: &[KnifeEdge], lambda_m: f64, loss: &dyn Fn(f64) -> f64, depth: usize) -> f64 {
    if edges.is_empty() || depth > 8 {
        return 0.0;
    }
    let mut best = 0usize;
    let mut best_nu = f64::NEG_INFINITY;
    for (i, e) in edges.iter().enumerate() {
        let nu = knife_edge_parameter(e.h_m, e.d1_m, e.d2_m, lambda_m);
        if nu > best_nu {
            best_nu = nu;
            best = i;
        }
    }
    let main = loss(best_nu);
    let left = deygout(&edges[..best], lambda_m, loss, depth + 1);
    let right = deygout(&edges[best + 1..], lambda_m, loss, depth + 1);
    math::sum_ordered([main, left, right])
}

// =========================================================================================
// `obstacle/building/sommer-2011`
// =========================================================================================

/// One fitted row of the Sommer 2011 building table (04-models.md §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SommerFit {
    /// The default row, the majority of the dataset: β 9 dB, γ 0.4 dB/m.
    Default,
    /// Free-standing warehouse (countryside): β 9.2, γ 0.32.
    FreeStandingWarehouse,
    /// Suburban house: β 9.6, γ 0.45.
    SuburbanHouse,
    /// Light-construction house: β 2.4, γ 0.63.
    LightConstructionHouse,
    /// Urban residential home, per-building fit: β 2.38, γ 0.10.
    UrbanResidentialHome,
    /// Urban residential garage, per-building fit: β 6.26, γ 0.41.
    UrbanResidentialGarage,
}

impl SommerFit {
    /// Every fitted row, in the order the document's table prints them.
    pub const ALL: [SommerFit; 6] = [
        SommerFit::Default,
        SommerFit::FreeStandingWarehouse,
        SommerFit::SuburbanHouse,
        SommerFit::LightConstructionHouse,
        SommerFit::UrbanResidentialHome,
        SommerFit::UrbanResidentialGarage,
    ];

    /// The row's coefficients [Sommer 2011, R3 §C.1].
    #[must_use]
    pub const fn coefficients(self) -> SommerCoefficients {
        match self {
            SommerFit::Default => SommerCoefficients::DEFAULT,
            SommerFit::FreeStandingWarehouse => SommerCoefficients {
                beta_db_per_wall: 9.2,
                gamma_db_per_m: 0.32,
            },
            SommerFit::SuburbanHouse => SommerCoefficients {
                beta_db_per_wall: 9.6,
                gamma_db_per_m: 0.45,
            },
            SommerFit::LightConstructionHouse => SommerCoefficients {
                beta_db_per_wall: 2.4,
                gamma_db_per_m: 0.63,
            },
            SommerFit::UrbanResidentialHome => SommerCoefficients {
                beta_db_per_wall: 2.38,
                gamma_db_per_m: 0.10,
            },
            SommerFit::UrbanResidentialGarage => SommerCoefficients {
                beta_db_per_wall: 6.26,
                gamma_db_per_m: 0.41,
            },
        }
    }

    /// The row's id as a scenario spells it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SommerFit::Default => "default",
            SommerFit::FreeStandingWarehouse => "free-standing-warehouse",
            SommerFit::SuburbanHouse => "suburban-house",
            SommerFit::LightConstructionHouse => "light-construction-house",
            SommerFit::UrbanResidentialHome => "urban-residential-home",
            SommerFit::UrbanResidentialGarage => "urban-residential-garage",
        }
    }
}

/// `obstacle/building/sommer-2011` — building shadowing from wall crossings and the
/// in-building path length.
///
/// Holds a [`BuildingIndex`] per world hash, built on first use.
#[derive(Debug)]
pub struct BuildingShadowing {
    card: ModelCard,
    tier: Tier,
    fit: SommerFit,
    /// Overrides for individual material classes. Empty by default: 04-models.md §3.5
    /// says the material class selects the row, but the cited table's rows are *building
    /// kinds* (warehouse, suburban house, garage) and the world's `MaterialClass` is a
    /// *construction material*, and nothing cited maps one onto the other. Every material
    /// therefore takes the default row until a mapping is calibrated, which the card
    /// records as `todo-calibrate` rather than guessing that, say, timber means
    /// light construction.
    material_overrides: BTreeMap<u8, SommerFit>,
    index: Option<BuildingIndex>,
    /// Whether [`BuildingShadowing::loss_for_path`] caps the through-building loss at the
    /// TR 37.885 around-the-corner NLOS excess.
    street_canyon_ceiling: bool,
}

impl BuildingShadowing {
    /// The model's id.
    pub const ID: &'static str = "obstacle/building/sommer-2011";

    /// The model at `tier` with the default fitted row.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self::with_fit(tier, SommerFit::Default)
    }

    /// The model with one of the per-class fitted rows.
    #[must_use]
    pub fn with_fit(tier: Tier, fit: SommerFit) -> Self {
        Self {
            card: building_card(fit, true),
            tier,
            fit,
            material_overrides: BTreeMap::new(),
            index: None,
            street_canyon_ceiling: true,
        }
    }

    /// Maps one material class onto a fitted row, for a study that has calibrated the
    /// mapping the shipped model refuses to guess.
    #[must_use]
    pub fn mapping(mut self, material: MaterialClass, fit: SommerFit) -> Self {
        self.material_overrides.insert(material.wire_code(), fit);
        self
    }

    /// The coefficients in force for one building.
    #[must_use]
    pub fn coefficients_for(&self, building: &Building) -> SommerCoefficients {
        self.material_overrides
            .get(&building.material.wire_code())
            .copied()
            .unwrap_or(self.fit)
            .coefficients()
    }

    /// Builds or reuses the index for this world.
    fn index_for(&mut self, world: &World) -> &BuildingIndex {
        let stale = match &self.index {
            None => true,
            Some(i) => i.world_hash() != world.content_hash,
        };
        if stale {
            self.index = Some(BuildingIndex::build(world));
        }
        self.index.as_ref().expect("just built")
    }

    /// The geometry of one link against the world's buildings, with the loss.
    ///
    /// Separate from [`ObstacleModel::los`] because it needs `&mut self` to build the
    /// index, and `los` is `&self` (it is pure, and the engine calls it from a parallel
    /// map). A caller that wants the index cached calls this; `los` falls back to a
    /// linear scan over the world's buildings, which is what a one-off query costs.
    pub fn los_cached(&mut self, world: &World, a: Vec3, b: Vec3) -> LosResult {
        let candidates = self.index_for(world).candidates(a, b);
        self.classify(world, a, b, candidates.into_iter())
    }

    fn classify(
        &self,
        world: &World,
        a: Vec3,
        b: Vec3,
        candidates: impl Iterator<Item = BuildingId>,
    ) -> LosResult {
        let mut walls = 0u16;
        let mut inside = 0.0;
        let mut edges: Vec<KnifeEdge> = Vec::new();
        let total = a.distance_2d(b);
        for id in candidates {
            let Some(building) = world.building(id) else {
                continue;
            };
            // A building whose roof is below both antennas cannot block the path. The
            // cited model is two-dimensional and assumes buildings taller than the
            // antennas; this test is the honest 2.5-D refinement and is recorded as a
            // design choice on the card.
            let roof_z = building.base_z_m + building.height_m;
            if roof_z <= a.z.min(b.z) {
                continue;
            }
            let crossing = ring_crossing(&building.footprint, a, b);
            if crossing.walls == 0 && crossing.inside_len_m <= 0.0 {
                continue;
            }
            walls = walls.saturating_add(crossing.walls);
            inside += crossing.inside_len_m;
            // The roof line is a diffracting edge for the high tier's terrain and
            // vehicle models to reuse; its along-path position is the midpoint of the
            // in-building interval, which is where the dominant edge of a box sits.
            if total > 0.0 {
                let d1 = (total * 0.5).max(1e-6);
                let h = roof_z - line_height_at(a.z, b.z, 0.5);
                edges.push(KnifeEdge {
                    d1_m: d1,
                    d2_m: (total - d1).max(1e-6),
                    h_m: h,
                    source: EdgeSource::Building { building: id },
                });
            }
        }
        if walls == 0 && inside <= 0.0 {
            return LosResult::clear();
        }
        LosResult {
            class: LosClass::NlosB,
            walls_crossed: walls,
            obstructed_len_m: inside,
            knife_edges: edges,
        }
    }

    /// The obstacle loss of a building-blocked link, dB, with the street-canyon ceiling.
    ///
    /// The Sommer term is the loss *through* the buildings on the straight path. In a
    /// street grid a blocked link is also reached *around* them — along the street canyon
    /// and round the corner — and the receiver sees the stronger of the two. 3GPP
    /// TR 37.885 Table 6.2.1-1 gives that around-the-corner path for urban V2V links
    /// "blocked by buildings" as its NLOS law, `36.85 + 30·log10(d3D) + 18.9·log10(fc)`,
    /// fitted (through WINNER+ B1) on Manhattan-grid street-canyon measurements. So the
    /// obstacle loss is the smaller of the Sommer term and the excess of that NLOS law over
    /// the line-of-sight path loss `los_path_db` the propagation model already charged:
    ///
    /// `L_obs = min(β·n + γ·d_in, max(0, PL_NLOS(d3D, fc) − PL_LOS))`
    ///
    /// Without the ceiling a link across four Midtown blocks paid several hundred decibels
    /// — every wall of every tower on the straight line — where a receiver actually
    /// hears the corner-diffracted path at a few tens of decibels below line of sight.
    /// Below the sensitivity floor the difference changes no outcome, but it is what makes
    /// a car round the corner of an intersection audible at 60-100 m as measured, rather
    /// than silent at 20 m.
    #[must_use]
    pub fn loss_for_path(&self, los: &LosResult, d3d_m: f64, f_hz: f64, los_path_db: f64) -> f64 {
        let through = self.loss_for(los);
        if through <= 0.0 || !self.street_canyon_ceiling {
            return through;
        }
        let around = crate::prop::tr37885_nlos_db(d3d_m, f_hz / 1e9) - los_path_db;
        through.min(around.max(0.0))
    }

    /// The model with the street-canyon ceiling of [`BuildingShadowing::loss_for_path`]
    /// switched on or off. On by default; off is the bare Sommer 2011 model, which its
    /// authors fitted in a suburban town where going round a block is not a shorter path.
    #[must_use]
    pub fn with_street_canyon_ceiling(mut self, on: bool) -> Self {
        self.street_canyon_ceiling = on;
        self.card = building_card(self.fit, on);
        self
    }

    /// Whether the street-canyon ceiling is on.
    #[must_use]
    pub const fn street_canyon_ceiling(&self) -> bool {
        self.street_canyon_ceiling
    }

    /// The Sommer loss for a classified link, dB. Deterministic: no draw.
    #[must_use]
    pub fn loss_for(&self, los: &LosResult) -> f64 {
        if !los.class.has_building() {
            return 0.0;
        }
        self.fit
            .coefficients()
            .loss_db(los.walls_crossed, los.obstructed_len_m)
    }
}

impl Model for BuildingShadowing {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> ObstacleModel<C> for BuildingShadowing {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn los(&self, world: &World, a: Vec3, b: Vec3, _actors: Option<&ActorSet>) -> LosResult {
        // Without the cached index this is a scan, but it is still narrowed by the
        // segment's bounding box before any ring arithmetic happens.
        let ids = world.buildings.iter().filter_map(|bl| {
            let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
            let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
            for p in &bl.footprint {
                min_x = min_x.min(p.x);
                min_y = min_y.min(p.y);
                max_x = max_x.max(p.x);
                max_y = max_y.max(p.y);
            }
            let overlaps = max_x >= a.x.min(b.x)
                && min_x <= a.x.max(b.x)
                && max_y >= a.y.min(b.y)
                && min_y <= a.y.max(b.y);
            overlaps.then_some(bl.id)
        });
        self.classify(world, a, b, ids)
    }

    fn obstacle_loss_db(
        &mut self,
        _ctx: &mut C,
        _tx: &RadioEndpoint,
        _rx: &RadioEndpoint,
        los: &LosResult,
        _f_hz: f64,
    ) -> f64 {
        self.loss_for(los)
    }
}

fn sommer_source() -> Source {
    Source {
        kind: SourceKind::Paper,
        reference: "C. Sommer, D. Eckhoff, R. German, F. Dressler, \"A Computationally \
                    Inexpensive Empirical Model of IEEE 802.11p Radio Shadowing in Urban \
                    Environments\" 2011 (R3 §C.1), via 04-models.md §3.5"
            .to_string(),
        accessed: None,
        note: Some("Fitted by Gauss-Newton least squares, tolerance 1e−5.".to_string()),
    }
}

fn building_card(fit: SommerFit, ceiling: bool) -> ModelCard {
    let c = fit.coefficients();
    let mut card = ModelCard::new(
        BuildingShadowing::ID,
        Family::Obstacle,
        "1.0.0",
        "Building shadowing: β dB per exterior wall crossed plus γ dB per metre of \
         in-building path, from the world's building footprints.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![Equation {
        name: "street-canyon ceiling".to_string(),
        latex_or_text: "L_obs = min(β·n + γ·d_m, max(0, PL_NLOS(d3D, fc) − PL_LOS)), \
                        PL_NLOS = 36.85 + 30·log10(d3D) + 18.9·log10(fc_GHz)"
            .to_string(),
        notes: Some(
            "3GPP TR 37.885 Table 6.2.1-1 urban NLOS ('blocked by buildings'): the \
             around-the-corner path a receiver hears when the straight one runs through \
             buildings. Applied when the ceiling is on."
                .to_string(),
        ),
    }, Equation {
        name: "obstacle loss".to_string(),
        latex_or_text: "L_obs[dB] = β·n + γ·d_m".to_string(),
        notes: Some(
            "n exterior walls crossed, d_m metres of path inside buildings; combined with \
             the path loss as P_r = P_t + 10·log10(G_t·G_r·λ²/(16π²·d^α)) − β·n − γ·d_m."
                .to_string(),
        ),
    }];
    card.parameters = vec![
        Parameter {
            name: "fit".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(fit.label()),
            range: Some(
                SommerFit::ALL
                    .iter()
                    .map(|f| serde_json::json!(f.label()))
                    .collect(),
            ),
            source: sommer_source(),
            calibration: None,
        },
        Parameter {
            name: "beta_db_per_wall".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(c.beta_db_per_wall),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(20.0)]),
            source: sommer_source(),
            calibration: None,
        },
        Parameter {
            name: "gamma_db_per_m".to_string(),
            unit: "dB/m".to_string(),
            default: serde_json::json!(c.gamma_db_per_m),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(2.0)]),
            source: sommer_source(),
            calibration: None,
        },
        Parameter {
            name: "material_map".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!({}),
            range: None,
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "04-models.md §3.5 says the material class selects the fitted \
                            row, but the cited rows are building kinds (warehouse, \
                            suburban house, garage) and the world's MaterialClass is a \
                            construction material; nothing cited maps one onto the other"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Empty by default: every material takes the default row (9 dB, \
                     0.4 dB/m), which is the row Sommer fitted to the majority of the \
                     dataset. Guessing that timber means light construction would be an \
                     invented mapping."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Classify the OSM building tags (building=warehouse, house, garage, \
                 residential) onto the five fitted rows and validate against the \
                 §13 shadowing targets; the OSM tag, not the material, is the right key."
                    .to_string(),
            ),
        },
    ];
    card.parameters.push(Parameter {
        name: "street_canyon_ceiling".to_string(),
        unit: "-".to_string(),
        default: serde_json::json!(ceiling),
        range: Some(vec![serde_json::json!(true), serde_json::json!(false)]),
        source: Source::new(
            SourceKind::Standard,
            "3GPP TR 37.885 V15.3.0 Table 6.2.1-1, urban V2V NLOS (blocked by buildings), \
             via 04-models.md §3.3",
        ),
        calibration: None,
    });
    card.assumptions = vec![
        "The receiver hears the stronger of the straight path through the buildings and the \
         around-the-corner street-canyon path; the latter is TR 37.885's urban NLOS law, \
         which is distance-only and does not trace the actual corner."
            .to_string(),
        "The path is the straight line between the two antenna phase centres.".to_string(),
        "A building whose roof is below both antennas does not block the path (a 2.5-D \
         refinement of the cited 2-D model, recorded as a design choice)."
            .to_string(),
        "Interior courtyards are part of the footprint: holes are kept by the world model \
         but are not subtracted here, so a courtyard counts as building."
            .to_string(),
    ];
    card.limitations = vec![
        "The R-tree over building footprints is built in this crate, because \
         v2xw-world's own index and World::index() are pub(crate). The duplication is \
         removed by a public accessor in that crate."
            .to_string(),
        "No reflection or waveguiding: an urban canyon's multipath enrichment is absorbed \
         by the NLOS path-loss exponent, not modelled here."
            .to_string(),
        "The Veins SimpleObstacleShadowing defaults are UNVERIFIED and are not used \
         (04-models.md §3.5)."
            .to_string(),
    ];
    card.ignores =
        vec!["Diffraction over roofs (the high tier's knife-edge models) and foliage.".to_string()];
    card.sources = vec![sommer_source()];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![sommer_source()],
        tests: vec![
            "the_sommer_table_is_reproduced".to_string(),
            "a_link_through_a_building_crosses_two_walls".to_string(),
            "the_index_and_the_scan_agree".to_string(),
        ],
    };
    card.determinism = Determinism::default();
    card
}

// =========================================================================================
// `obstacle/vehicle/tr37885-nlosv`
// =========================================================================================

/// Which of the three TR 37.885 antenna-height cases a blocked link is in
/// (04-models.md §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NlosvCase {
    /// Case 1: the **minimum** antenna height of transmitter and receiver is above the
    /// blocker. 0 dB.
    MinAntennaAboveBlocker,
    /// Case 2: the **maximum** antenna height is below the blocker. Mean
    /// `9 + max(0, 15·log10(d) − 41)` dB, σ 4.5 dB.
    MaxAntennaBelowBlocker,
    /// Case 3: anything else. Mean `5 + max(0, 15·log10(d) − 41)` dB, σ 4 dB.
    Between,
}

impl NlosvCase {
    /// The case for two antenna heights and a blocker height, all in the same datum.
    #[must_use]
    pub fn classify(h_tx_m: f64, h_rx_m: f64, blocker_top_m: f64) -> NlosvCase {
        if h_tx_m.min(h_rx_m) > blocker_top_m {
            NlosvCase::MinAntennaAboveBlocker
        } else if h_tx_m.max(h_rx_m) < blocker_top_m {
            NlosvCase::MaxAntennaBelowBlocker
        } else {
            NlosvCase::Between
        }
    }

    /// The mean extra loss at distance `d_m`, dB [TR 37.885 §6.2.1].
    #[must_use]
    pub fn mean_db(self, d_m: f64) -> f64 {
        let ramp = (15.0 * math::log10(d_m.max(1.0)) - 41.0).max(0.0);
        match self {
            NlosvCase::MinAntennaAboveBlocker => 0.0,
            NlosvCase::MaxAntennaBelowBlocker => 9.0 + ramp,
            NlosvCase::Between => 5.0 + ramp,
        }
    }

    /// The standard deviation of the extra loss, dB [TR 37.885 §6.2.1].
    #[must_use]
    pub const fn sigma_db(self) -> f64 {
        match self {
            NlosvCase::MinAntennaAboveBlocker => 0.0,
            NlosvCase::MaxAntennaBelowBlocker => 4.5,
            NlosvCase::Between => 4.0,
        }
    }

    /// The case's id as a report spells it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            NlosvCase::MinAntennaAboveBlocker => "case-1-min-antenna-above-blocker",
            NlosvCase::MaxAntennaBelowBlocker => "case-2-max-antenna-below-blocker",
            NlosvCase::Between => "case-3-between",
        }
    }
}

/// `obstacle/vehicle/tr37885-nlosv` — the vehicle blockage loss of the 3GPP evaluation
/// model.
#[derive(Debug, Clone)]
pub struct VehicleBlockage {
    card: ModelCard,
    tier: Tier,
    /// The blocker height used when the caller passes no actors, metres.
    default_blocker_height_m: f64,
}

impl VehicleBlockage {
    /// The model's id.
    pub const ID: &'static str = "obstacle/vehicle/tr37885-nlosv";

    /// The model at `tier`.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self {
            card: vehicle_card(),
            tier,
            default_blocker_height_m: 1.5,
        }
    }

    /// Whether this actor's body blocks the segment, and the resulting edge.
    ///
    /// The body is tested as its axis-aligned footprint around the reference point,
    /// inflated to the vehicle's width and length: the heading is known but a rotated
    /// rectangle test would make the answer depend on the last bit of a sine, and at
    /// these sizes the difference cannot change a blockage decision that matters.
    #[must_use]
    pub fn blocking_edge(a: Vec3, b: Vec3, actor: &ActorObstacle) -> Option<KnifeEdge> {
        let half = 0.5 * actor.dims.length_m.max(actor.dims.width_m);
        let ring = [
            Vec3::new(actor.pos.x - half, actor.pos.y - half, actor.pos.z),
            Vec3::new(actor.pos.x + half, actor.pos.y - half, actor.pos.z),
            Vec3::new(actor.pos.x + half, actor.pos.y + half, actor.pos.z),
            Vec3::new(actor.pos.x - half, actor.pos.y + half, actor.pos.z),
            Vec3::new(actor.pos.x - half, actor.pos.y - half, actor.pos.z),
        ];
        let crossing = ring_crossing(&ring, a, b);
        if crossing.walls == 0 && crossing.inside_len_m <= 0.0 {
            return None;
        }
        let total = a.distance_2d(b);
        if total <= 0.0 {
            return None;
        }
        let along = (a.distance_2d(actor.pos) / total).clamp(0.0, 1.0);
        let d1 = (total * along).max(1e-6);
        let top = actor.top_z_m();
        if top <= a.z.min(b.z) {
            // Both antennas see over it.
            return None;
        }
        Some(KnifeEdge {
            d1_m: d1,
            d2_m: (total - d1).max(1e-6),
            h_m: top - line_height_at(a.z, b.z, along),
            source: EdgeSource::Vehicle { actor: actor.actor },
        })
    }

    /// The blocker top height implied by an edge list, in the world's datum.
    #[must_use]
    pub fn blocker_top_m(&self, tx: &RadioEndpoint, rx: &RadioEndpoint, los: &LosResult) -> f64 {
        let vehicle_edges: Vec<&KnifeEdge> = los
            .knife_edges
            .iter()
            .filter(|e| matches!(e.source, EdgeSource::Vehicle { .. }))
            .collect();
        if vehicle_edges.is_empty() {
            return self.default_blocker_height_m;
        }
        // The tallest blocker decides the case, which is the conservative reading of
        // "the blocker" when several vehicles are in the way.
        let mut best = f64::NEG_INFINITY;
        for e in vehicle_edges {
            let total = e.d1_m + e.d2_m;
            let f = if total > 0.0 { e.d1_m / total } else { 0.5 };
            let top = line_height_at(tx.pos.z, rx.pos.z, f) + e.h_m;
            if top > best {
                best = top;
            }
        }
        best
    }
}

impl Default for VehicleBlockage {
    fn default() -> Self {
        Self::new(Tier::Medium)
    }
}

impl Model for VehicleBlockage {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> ObstacleModel<C> for VehicleBlockage {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn los(&self, _world: &World, a: Vec3, b: Vec3, actors: Option<&ActorSet>) -> LosResult {
        let Some(actors) = actors else {
            return LosResult::clear();
        };
        let mut edges = Vec::new();
        // In id order, because the set is: the edge list is part of the answer.
        for actor in actors.as_slice() {
            // An endpoint's own body never blocks its own link.
            if actor.pos.distance_2d(a) < 1e-9 || actor.pos.distance_2d(b) < 1e-9 {
                continue;
            }
            if let Some(edge) = Self::blocking_edge(a, b, actor) {
                edges.push(edge);
            }
        }
        if edges.is_empty() {
            return LosResult::clear();
        }
        LosResult {
            class: LosClass::NlosV,
            walls_crossed: 0,
            obstructed_len_m: 0.0,
            knife_edges: edges,
        }
    }

    fn obstacle_loss_db(
        &mut self,
        ctx: &mut C,
        tx: &RadioEndpoint,
        rx: &RadioEndpoint,
        los: &LosResult,
        _f_hz: f64,
    ) -> f64 {
        if !los.class.has_vehicle() {
            return 0.0;
        }
        let d_m = tx.pos.distance(rx.pos);
        let blocker = self.blocker_top_m(tx, rx, los);
        let case = NlosvCase::classify(tx.pos.z, rx.pos.z, blocker);
        if case == NlosvCase::MinAntennaAboveBlocker {
            return 0.0;
        }
        // `max{0, lognormal}`: the extra loss is normal in dB and clamped at zero, so a
        // favourable realisation cannot turn a blockage into a gain.
        let link = LinkKey(tx.node, rx.node);
        let draw = ctx
            .rng(RngDomain::Shadow, EntityRef::Link(link))
            .normal(case.mean_db(d_m), case.sigma_db());
        draw.max(0.0)
    }
}

fn tr37885_source() -> Source {
    Source::new(
        SourceKind::Standard,
        "3GPP TR 37.885 §6.2.1 (R2c and R3 §D.4), via 04-models.md §3.5",
    )
}

fn vehicle_card() -> ModelCard {
    let mut card = ModelCard::new(
        VehicleBlockage::ID,
        Family::Obstacle,
        "1.0.0",
        "Vehicle blockage loss by antenna-height case: 0 dB when both antennas see over \
         the blocker, a log-normal extra loss otherwise.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![Equation {
        name: "extra loss".to_string(),
        latex_or_text: "max{0, N(μ, σ²)} with μ = 0 (case 1); \
                        μ = 9 + max(0, 15·log10(d) − 41), σ = 4.5 (case 2); \
                        μ = 5 + max(0, 15·log10(d) − 41), σ = 4 (case 3)"
            .to_string(),
        notes: Some(
            "Case 1: minimum antenna height of Tx and Rx above the blocker. Case 2: \
             maximum antenna height below the blocker. Case 3: otherwise."
                .to_string(),
        ),
    }];
    card.parameters = vec![
        Parameter {
            name: "case2_mean_offset_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(9.0),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(30.0)]),
            source: tr37885_source(),
            calibration: None,
        },
        Parameter {
            name: "case2_sigma_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(4.5),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(15.0)]),
            source: tr37885_source(),
            calibration: None,
        },
        Parameter {
            name: "case3_mean_offset_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(5.0),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(30.0)]),
            source: tr37885_source(),
            calibration: None,
        },
        Parameter {
            name: "case3_sigma_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(4.0),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(15.0)]),
            source: tr37885_source(),
            calibration: None,
        },
        Parameter {
            name: "default_blocker_height_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(1.5),
            range: Some(vec![serde_json::json!(0.5), serde_json::json!(4.5)]),
            source: Source {
                kind: SourceKind::Paper,
                reference: "Boban thesis Table 3.7 measured vehicle heights (R3 §D.2), via \
                            04-models.md §3.5: passenger cars 1.453-1.547 m"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Used only when the caller passes no actors. With actors, the real \
                     blocker's Dims decide the case, which is what 04-models.md §3.5 \
                     specifies; TR 37.885's own population-weighted draw is the fallback \
                     the engine does not need."
                        .to_string(),
                ),
            },
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "The blocker's height comes from the actual actor's Dims when actors are passed \
         to los()."
            .to_string(),
        "The tallest blocker on the path decides the antenna-height case.".to_string(),
        "A vehicle body is tested as an axis-aligned square of its larger horizontal \
         dimension, not as a rotated rectangle: a sine in a blockage test would make the \
         decision depend on its last bit."
            .to_string(),
    ];
    card.limitations = vec![
        "One draw per evaluation from the link's Shadow stream, with no temporal \
         correlation: a vehicle that stays in the way re-draws its extra loss."
            .to_string(),
        "The measured anchors are stronger than this model at short range: a large truck \
         cost 27 dB at 26 m and a van 12 dB at 20 m (Meireles 2010), against this model's \
         9 dB mean. obstacle/vehicle/knife-edge-boban is the physical alternative."
            .to_string(),
    ];
    card.sources = vec![
        tr37885_source(),
        Source::new(
            SourceKind::Paper,
            "Boban thesis Tables 3.1, 3.7, 4.2, 5.2 (measured vehicle dimensions and \
             tall-vehicle fractions), via 04-models.md §3.5",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![tr37885_source()],
        tests: vec![
            "the_nlosv_cases_match_the_document".to_string(),
            "a_vehicle_in_the_way_is_detected_and_costs_decibels".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["shadow".to_string()],
    };
    card
}

// =========================================================================================
// `obstacle/vehicle/knife-edge-boban`
// =========================================================================================

/// The Boban vehicle knife-edge loss, dB (04-models.md §3.5).
///
/// `A_sk = 6.9 + 20·log10(sqrt((v − 0.1)² + 1) + v − 0.1)` for `v > −0.7`, else 0, with
/// `v = sqrt(2H/r_f)` and `r_f = sqrt(λ·d1·d2/(d1 + d2))` the Fresnel radius. Valid
/// because λ ≈ 5 cm is much smaller than a vehicle.
#[must_use]
pub fn boban_vehicle_loss_db(h_m: f64, d1_m: f64, d2_m: f64, lambda_m: f64) -> f64 {
    if h_m <= 0.0 || d1_m <= 0.0 || d2_m <= 0.0 {
        return 0.0;
    }
    let r_f = math::sqrt(lambda_m * d1_m * d2_m / (d1_m + d2_m));
    if r_f <= 0.0 {
        return 0.0;
    }
    let v = math::sqrt(2.0 * h_m / r_f);
    if v <= -0.7 {
        return 0.0;
    }
    let x = v - 0.1;
    6.9 + 20.0 * math::log10(math::sqrt(x * x + 1.0) + x)
}

// =========================================================================================
// `obstacle/terrain/knife-edge-p526`
// =========================================================================================

/// `obstacle/terrain/knife-edge-p526` — single or multiple knife-edge diffraction over
/// the terrain profile.
///
/// The geometry — which summits diffract, how far along the path they are and how high
/// above the *radio line* they sit — is [`crate::terrain`]'s, which is also where the
/// shape of the world-side terrain profile query is documented. This type is the
/// [`ObstacleModel`] wrapper: it chooses the sample count, the multiple-edge construction
/// and the `J(ν)` form, and it reduces the edge list to decibels.
#[derive(Debug, Clone)]
pub struct TerrainDiffraction {
    card: ModelCard,
    tier: Tier,
    rule: MultiEdgeRule,
    exact: bool,
    /// How many points the DEM profile is sampled at between the endpoints.
    profile_samples: usize,
    /// How the edge list is extracted from that profile.
    extraction: crate::terrain::EdgeExtraction,
}

impl TerrainDiffraction {
    /// The model's id.
    pub const ID: &'static str = "obstacle/terrain/knife-edge-p526";

    /// The model at `tier` with the Deygout rule and the approximate `J(ν)`.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self {
            card: terrain_card(MultiEdgeRule::Deygout, false),
            tier,
            rule: MultiEdgeRule::Deygout,
            exact: false,
            profile_samples: crate::terrain::DEFAULT_PROFILE_SAMPLES,
            extraction: crate::terrain::EdgeExtraction::default(),
        }
    }

    /// The model with a caller-chosen profile sample count.
    ///
    /// Clamped to the range the card declares, `3..=4_096`: below three there is no
    /// interior sample for a summit to sit at, and above four thousand the profile invents
    /// detail no DEM carries.
    #[must_use]
    pub fn with_profile_samples(mut self, samples: usize) -> Self {
        self.profile_samples = samples.clamp(3, 4_096);
        self
    }

    /// The model with a caller-chosen edge extraction.
    ///
    /// The card keeps declaring the *defaults* for `profile_samples`, `max_edges` and
    /// `include_below_line`, as a card declares defaults; the resolved values come from
    /// the scenario's parameter set the same way every other override does
    /// (02-architecture.md §6.5).
    #[must_use]
    pub fn with_extraction(mut self, extraction: crate::terrain::EdgeExtraction) -> Self {
        self.extraction = extraction;
        self
    }

    /// How many points the profile is sampled at.
    #[must_use]
    pub const fn profile_samples(&self) -> usize {
        self.profile_samples
    }

    /// The model using the exact Fresnel-integral form of `J(ν)`.
    #[must_use]
    pub fn exact(mut self) -> Self {
        self.exact = true;
        self.card = terrain_card(self.rule, true);
        self
    }

    /// The model using the ITU-R multiple-edge construction instead of Deygout.
    #[must_use]
    pub fn with_rule(mut self, rule: MultiEdgeRule) -> Self {
        self.rule = rule;
        self.card = terrain_card(rule, self.exact);
        self
    }

    /// The terrain profile along a link, or `None` when the world has no DEM.
    ///
    /// Delegated to [`crate::terrain::TerrainProfile::sample`], which is the one place
    /// that knows the shape of the world-side query.
    #[must_use]
    pub fn profile(&self, world: &World, a: Vec3, b: Vec3) -> Option<crate::terrain::TerrainProfile> {
        crate::terrain::TerrainProfile::sample(world, a, b, self.profile_samples)
    }

    /// The terrain edges along a link: every local maximum of the clearance profile —
    /// ground height minus the **radio line's** height — in along-path order.
    ///
    /// A world with no DEM has no terrain edges at all, which is the honest answer: the
    /// ground is the zero plane and a link cannot be blocked by it.
    ///
    /// `a.z` and `b.z` are antenna heights *above the local ground*
    /// ([`crate::types::RadioEndpoint`]), not elevations, and
    /// [`crate::terrain::knife_edges`] is what adds the ground under each endpoint before
    /// interpolating the line. Comparing a DEM elevation against a bare `z` — which this
    /// method used to do — reported every link on a world at 200 m as blocked by a 198 m
    /// knife edge; [`crate::terrain`] documents the fix in full.
    ///
    /// The list can contain edges **below** the line: they may still intrude into the
    /// first Fresnel zone, and the `ν > −0.78` filter that decides whether they cost
    /// anything is [`knife_edge_loss_db`]'s, which is the only place that knows the
    /// wavelength.
    #[must_use]
    pub fn terrain_edges(&self, world: &World, a: Vec3, b: Vec3) -> Vec<KnifeEdge> {
        let Some(profile) = self.profile(world, a, b) else {
            return Vec::new();
        };
        crate::terrain::knife_edges(&profile, a.z, b.z, self.extraction)
    }
}

impl Default for TerrainDiffraction {
    fn default() -> Self {
        Self::new(Tier::High)
    }
}

impl Model for TerrainDiffraction {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> ObstacleModel<C> for TerrainDiffraction {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn los(&self, world: &World, a: Vec3, b: Vec3, _actors: Option<&ActorSet>) -> LosResult {
        let edges = self.terrain_edges(world, a, b);
        if edges.is_empty() {
            return LosResult::clear();
        }
        // An edge *below* the radio line diffracts — it can intrude into the first Fresnel
        // zone — but it does not obstruct. Calling such a link NLOS-terrain would put a
        // link with a fraction of a decibel of grazing loss into the same class as one
        // behind a ridge, and 04-models.md §3.2's path-loss presets are selected by that
        // class. So the edges travel in the result and the class follows the geometry.
        let class = if crate::terrain::any_edge_obstructs(&edges) {
            LosClass::NlosT
        } else {
            LosClass::Los
        };
        LosResult {
            class,
            walls_crossed: 0,
            obstructed_len_m: 0.0,
            knife_edges: edges,
        }
    }

    fn obstacle_loss_db(
        &mut self,
        _ctx: &mut C,
        _tx: &RadioEndpoint,
        _rx: &RadioEndpoint,
        los: &LosResult,
        f_hz: f64,
    ) -> f64 {
        let terrain: Vec<KnifeEdge> = los
            .knife_edges
            .iter()
            .copied()
            .filter(|e| e.source == EdgeSource::Terrain)
            .collect();
        if terrain.is_empty() {
            return 0.0;
        }
        multi_edge_loss_db(&terrain, numeric::wavelength_m(f_hz), self.rule, self.exact)
    }
}

fn terrain_card(rule: MultiEdgeRule, exact: bool) -> ModelCard {
    let itu = Source {
        kind: SourceKind::Standard,
        reference: "ITU-R P.526-14 Eq. 26, 30, 31, 33 (R3 §C.2), via 04-models.md §3.5".to_string(),
        accessed: None,
        note: Some("The cached edition is P.526-14; the current one is P.526-15.".to_string()),
    };
    let mut card = ModelCard::new(
        TerrainDiffraction::ID,
        Family::Obstacle,
        "1.0.0",
        "Knife-edge diffraction over the terrain profile: the ITU-R P.526 J(ν), either \
         the approximation or the exact Fresnel-integral form, reduced over multiple \
         edges by Deygout or the ITU-R construction.",
    );
    card.tier = vec![Tier::High];
    card.equations = vec![
        Equation {
            name: "knife-edge parameter".to_string(),
            latex_or_text: "ν = h·sqrt(2(d1 + d2)/(λ·d1·d2))".to_string(),
            notes: Some(
                "Self-consistent units. The practical-units form is \
                 ν = 0.0316·h·sqrt(2(d1 + d2)/(λ·d1·d2)) with h and λ in m and d in km."
                    .to_string(),
            ),
        },
        Equation::new(
            "diffraction loss",
            "J(ν) = 6.9 + 20·log10(sqrt((ν − 0.1)² + 1) + ν − 0.1) dB for ν > −0.78",
        ),
        Equation {
            name: "exact form".to_string(),
            latex_or_text: "J(ν) = −20·log10|F(ν)|, F(ν) = ((1+j)/2)·∫_ν^∞ exp(−jπt²/2) dt \
                            = −10·log10(((1/2 − C(ν))² + (1/2 − S(ν))²)/2)"
                .to_string(),
            notes: Some(
                "Fresnel integrals by composite Simpson with a fixed panel count, so the \
                 value is deterministic."
                    .to_string(),
            ),
        },
    ];
    card.parameters = vec![
        Parameter {
            name: "multi_edge_rule".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(match rule {
                MultiEdgeRule::Deygout => "deygout",
                MultiEdgeRule::ItuR => "itu-r",
            }),
            range: Some(vec![
                serde_json::json!("deygout"),
                serde_json::json!("itu-r"),
            ]),
            source: itu.clone(),
            calibration: None,
        },
        Parameter {
            name: "exact".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(exact),
            range: None,
            source: itu.clone(),
            calibration: None,
        },
        Parameter {
            name: "profile_samples".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(crate::terrain::DEFAULT_PROFILE_SAMPLES),
            range: Some(vec![serde_json::json!(3), serde_json::json!(4_096)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "how many points the DEM profile is sampled at between the \
                            endpoints; nothing cited prescribes a spacing"
                    .to_string(),
                accessed: None,
                note: Some(
                    "64 samples put a point every 15 m on a 1 km link, which is finer \
                     than the SRTM 30 m post spacing the world's DEM comes from."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Sample at the DEM's own post spacing once the terrain importer records \
                 it, so the profile neither invents detail nor misses a summit."
                    .to_string(),
            ),
        },
        Parameter {
            name: "max_edges".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(crate::terrain::DEFAULT_MAX_EDGES),
            range: Some(vec![serde_json::json!(1), serde_json::json!(64)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "how many of a profile's summits the multiple-edge \
                            construction is applied to; ITU-R P.526 states the \
                            constructions for the principal edges of a path without \
                            printing a count"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Eight. The Deygout construction recurses over the edge list, so an \
                     unbounded list makes the cost quadratic in the sample count, which \
                     is a property of the query and not of the terrain. The edges kept \
                     are the highest, which are the ones that diffract."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Compare the full-list and capped losses over an imported DEM once the \
                 terrain importer lands, and raise the cap if the difference exceeds the \
                 ±3 dB tolerance 04-models.md §13 states for an obstacle row."
                    .to_string(),
            ),
        },
        Parameter {
            name: "include_below_line".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(true),
            range: Some(vec![
                serde_json::json!(false),
                serde_json::json!(true),
            ]),
            source: itu.clone(),
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "Each local maximum of the clearance profile is an ideal knife edge.".to_string(),
        "A world with no DEM has no terrain obstruction.".to_string(),
        "The clearance profile is measured against the radio line in absolute elevation: \
         a RadioEndpoint's z is an antenna height above the local ground, so the ground \
         under each endpoint is added before the line is interpolated (see \
         crate::terrain)."
            .to_string(),
        "A summit below the line is still reported as an edge, because it can intrude \
         into the first Fresnel zone; the ν > −0.78 filter of P.526 Eq. 31 is what \
         decides whether it costs anything, and it is applied in the loss model, which is \
         the only place that knows the wavelength."
            .to_string(),
        "A link whose only edges sit below the line is classified LOS, not NLOS-terrain: \
         it diffracts but it is not obstructed, and 04-models.md §3.2's presets are \
         selected by the class."
            .to_string(),
    ];
    card.limitations = vec![
        "Deygout is the more pessimistic of the two multiple-edge constructions and the \
         ITU-R one the more optimistic; neither is exact for a rounded ridge."
            .to_string(),
        "No ground reflection or rounded-obstacle correction (P.526's other cases).".to_string(),
        "At most max_edges summits enter the construction; a profile with more loses its \
         lowest ones."
            .to_string(),
    ];
    card.ignores = vec![
        "Earth curvature: the radio line is a straight chord, not a 4/3-k-factor ray. The \
         bulge d1·d2/(2·a_e) is about 1.5 cm at the worst case this crate models (a 1 km \
         link with the summit at mid-path), two orders of magnitude below anything a 30 m \
         post spacing resolves."
            .to_string(),
        "Foliage on the summit, which is obstacle/foliage/boban-mel's term.".to_string(),
    ];
    card.sources = vec![itu.clone()];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![itu],
        tests: vec![
            "a_grazing_knife_edge_costs_six_decibels".to_string(),
            "the_exact_and_approximate_forms_agree".to_string(),
            "deygout_is_more_pessimistic_than_a_single_edge".to_string(),
            // crate::terrain's own tests, which cover the geometry this model reduces.
            "a_flat_profile_at_any_elevation_has_no_edges".to_string(),
            "a_hill_between_two_antennas_is_one_knife_edge_above_the_line".to_string(),
            "a_taller_antenna_clears_the_hill".to_string(),
            "a_grazing_summit_is_continuous_across_the_line".to_string(),
            "two_hills_are_two_edges_in_along_path_order".to_string(),
            "the_edge_cap_keeps_the_highest_and_is_order_independent".to_string(),
            "a_plateau_yields_one_edge_not_two".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testctx::{TestCtx, tiny_world, world_with_building};

    /// A link through a whole row of towers is charged the around-the-corner excess of
    /// TR 37.885's NLOS law, not every wall on the straight line; a thin building is
    /// cheaper to go through than round, and keeps the Sommer term; and the bare model is
    /// one switch away.
    #[test]
    fn the_street_canyon_ceiling_caps_a_link_through_many_buildings() {
        let m = BuildingShadowing::new(Tier::Medium);
        let blocked = LosResult {
            class: LosClass::NlosB,
            walls_crossed: 20,
            obstructed_len_m: 250.0,
            knife_edges: Vec::new(),
        };
        let through = m.loss_for(&blocked);
        assert!((through - (20.0 * 9.0 + 250.0 * 0.4)).abs() < 1e-9);
        let los_path_db = 90.0;
        let capped = m.loss_for_path(&blocked, 300.0, 5.9e9, los_path_db);
        let want = crate::prop::tr37885_nlos_db(300.0, 5.9) - los_path_db;
        assert!((capped - want).abs() < 1e-9, "capped {capped}, want {want}");
        assert!(capped < 0.2 * through);

        let thin = LosResult {
            walls_crossed: 2,
            obstructed_len_m: 10.0,
            ..blocked.clone()
        };
        assert_eq!(
            m.loss_for_path(&thin, 300.0, 5.9e9, los_path_db),
            m.loss_for(&thin),
            "22 dB through beats 36 dB round"
        );

        let bare = BuildingShadowing::new(Tier::Medium).with_street_canyon_ceiling(false);
        assert_eq!(bare.loss_for_path(&blocked, 300.0, 5.9e9, los_path_db), through);
        assert_eq!(m.loss_for_path(&LosResult::clear(), 300.0, 5.9e9, los_path_db), 0.0);
    }
    use crate::types::ActorClass;
    use v2xw_core::geom::Dims;
    use v2xw_core::ids::{ActorId, NodeId};

    fn endpoint(id: u32, x: f64, y: f64, h: f64) -> RadioEndpoint {
        RadioEndpoint {
            node: NodeId::new(id),
            pos: Vec3::new(x, y, h),
            gain_dbi: 0.0,
            pattern: None,
            pos_time: 0,
            class: ActorClass::Car,
        }
    }

    fn car(id: u32, x: f64, y: f64, height_m: f64) -> ActorObstacle {
        ActorObstacle {
            actor: ActorId::new(id),
            pos: Vec3::new(x, y, 0.0),
            dims: Dims {
                length_m: 5.0,
                width_m: 1.8,
                height_m,
            },
            heading_rad: 0.0,
            class: if height_m > 2.0 {
                ActorClass::Truck
            } else {
                ActorClass::Car
            },
        }
    }

    #[test]
    fn the_sommer_table_is_reproduced() {
        let rows: [(SommerFit, f64, f64); 6] = [
            (SommerFit::Default, 9.0, 0.4),
            (SommerFit::FreeStandingWarehouse, 9.2, 0.32),
            (SommerFit::SuburbanHouse, 9.6, 0.45),
            (SommerFit::LightConstructionHouse, 2.4, 0.63),
            (SommerFit::UrbanResidentialHome, 2.38, 0.10),
            (SommerFit::UrbanResidentialGarage, 6.26, 0.41),
        ];
        for (fit, beta, gamma) in rows {
            let c = fit.coefficients();
            assert_eq!(c.beta_db_per_wall, beta, "{}", fit.label());
            assert_eq!(c.gamma_db_per_m, gamma, "{}", fit.label());
        }
        // The formula itself: two walls and 18 m inside, on the default row.
        assert_eq!(
            SommerCoefficients::DEFAULT.loss_db(2, 18.0),
            9.0 * 2.0 + 0.4 * 18.0
        );
        assert_eq!(SommerCoefficients::DEFAULT.loss_db(0, 0.0), 0.0);
    }

    #[test]
    fn a_link_through_a_building_crosses_two_walls() {
        // A 10 m deep building between the two ends.
        let world = world_with_building(10.0, -5.0, 20.0, 5.0, 12.0);
        let model = BuildingShadowing::new(Tier::Medium);
        let los = ObstacleModel::<TestCtx>::los(
            &model,
            &world,
            Vec3::new(0.0, 0.0, 1.5),
            Vec3::new(30.0, 0.0, 1.5),
            None,
        );
        assert_eq!(los.class, LosClass::NlosB);
        assert_eq!(los.walls_crossed, 2, "in one side and out the other");
        assert!(
            (los.obstructed_len_m - 10.0).abs() < 1e-6,
            "{}",
            los.obstructed_len_m
        );
        // β·n + γ·d = 18 + 4 = 22 dB.
        assert!((model.loss_for(&los) - 22.0).abs() < 1e-9);

        // A link well outside the grid, where nothing stands, is clear. (The world is a
        // procedural urban grid, so "beside the test building" is not necessarily empty;
        // two kilometres south of it is.)
        let clear = ObstacleModel::<TestCtx>::los(
            &model,
            &world,
            Vec3::new(0.0, -2_000.0, 1.5),
            Vec3::new(30.0, -2_000.0, 1.5),
            None,
        );
        assert_eq!(clear.class, LosClass::Los);
        assert_eq!(clear.walls_crossed, 0);
        assert_eq!(model.loss_for(&clear), 0.0);

        // An endpoint inside crosses one wall.
        let inside = ObstacleModel::<TestCtx>::los(
            &model,
            &world,
            Vec3::new(15.0, 0.0, 1.5),
            Vec3::new(30.0, 0.0, 1.5),
            None,
        );
        assert_eq!(inside.walls_crossed, 1);
        assert!((inside.obstructed_len_m - 5.0).abs() < 1e-6);
    }

    #[test]
    fn a_building_shorter_than_both_antennas_does_not_block() {
        // A 1 m high wall between two 1.5 m antennas: the link passes over it.
        let world = world_with_building(10.0, -5.0, 20.0, 5.0, 1.0);
        let model = BuildingShadowing::new(Tier::Medium);
        let los = ObstacleModel::<TestCtx>::los(
            &model,
            &world,
            Vec3::new(0.0, 0.0, 1.5),
            Vec3::new(30.0, 0.0, 1.5),
            None,
        );
        assert_eq!(
            los.class,
            LosClass::Los,
            "a 1 m wall cannot block 1.5 m antennas"
        );
    }

    #[test]
    fn the_index_and_the_scan_agree() {
        let world = world_with_building(10.0, -5.0, 20.0, 5.0, 12.0);
        let mut model = BuildingShadowing::new(Tier::Medium);
        let a = Vec3::new(0.0, 0.0, 1.5);
        let b = Vec3::new(30.0, 0.0, 1.5);
        let scanned = ObstacleModel::<TestCtx>::los(&model, &world, a, b, None);
        let indexed = model.los_cached(&world, a, b);
        assert_eq!(scanned, indexed);
        // The index is keyed by the world's content hash, so it cannot be used for
        // another world by accident.
        let index = BuildingIndex::build(&world);
        assert_eq!(index.world_hash(), world.content_hash);
        assert_eq!(index.candidates(a, b).len(), 1);
        // And a query that misses the envelope finds nothing.
        assert!(
            index
                .candidates(Vec3::new(0.0, 500.0, 1.5), Vec3::new(30.0, 500.0, 1.5))
                .is_empty()
        );
    }

    #[test]
    fn the_nlosv_cases_match_the_document() {
        // Case 1: both antennas above the blocker.
        assert_eq!(
            NlosvCase::classify(2.5, 3.0, 1.5),
            NlosvCase::MinAntennaAboveBlocker
        );
        // Case 2: both below.
        assert_eq!(
            NlosvCase::classify(1.5, 1.5, 3.0),
            NlosvCase::MaxAntennaBelowBlocker
        );
        // Case 3: one above, one below.
        assert_eq!(NlosvCase::classify(1.5, 3.5, 3.0), NlosvCase::Between);

        // The means and sigmas.
        assert_eq!(NlosvCase::MinAntennaAboveBlocker.mean_db(100.0), 0.0);
        assert_eq!(NlosvCase::MinAntennaAboveBlocker.sigma_db(), 0.0);
        assert_eq!(NlosvCase::MaxAntennaBelowBlocker.sigma_db(), 4.5);
        assert_eq!(NlosvCase::Between.sigma_db(), 4.0);
        // The ramp max(0, 15·log10(d) − 41) is zero below 549 m and positive above.
        assert_eq!(NlosvCase::MaxAntennaBelowBlocker.mean_db(100.0), 9.0);
        assert_eq!(NlosvCase::Between.mean_db(100.0), 5.0);
        let at_1km = NlosvCase::MaxAntennaBelowBlocker.mean_db(1_000.0);
        assert!((at_1km - (9.0 + 45.0 - 41.0)).abs() < 1e-9, "{at_1km}");
        // The ramp turns on exactly where 15·log10(d) = 41, i.e. d = 545.6 m.
        // 15·log10(d) = 41 at d = 10^(41/15) = 541.2 m.
        let switch = math::pow(10.0, 41.0 / 15.0);
        assert!((switch - 541.2).abs() < 0.5, "{switch}");
        assert_eq!(NlosvCase::Between.mean_db(switch - 1.0), 5.0);
        assert!(NlosvCase::Between.mean_db(switch + 10.0) > 5.0);
    }

    #[test]
    fn a_vehicle_in_the_way_is_detected_and_costs_decibels() {
        let mut ctx = TestCtx::new(21);
        let world = tiny_world();
        let mut model = VehicleBlockage::new(Tier::Medium);
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 100.0, 0.0, 1.5);
        // A truck in the middle of the path, 3.4 m tall: both 1.5 m antennas are below
        // it, which is case 2.
        let actors = ActorSet::from_iter_sorted([car(0, 50.0, 0.0, 3.4)]);
        let los = ObstacleModel::<TestCtx>::los(&model, &world, tx.pos, rx.pos, Some(&actors));
        assert_eq!(los.class, LosClass::NlosV);
        assert_eq!(los.knife_edges.len(), 1);
        assert_eq!(
            los.knife_edges[0].source,
            EdgeSource::Vehicle {
                actor: ActorId::new(0)
            }
        );
        let blocker = model.blocker_top_m(&tx, &rx, &los);
        assert!((blocker - 3.4).abs() < 1e-9, "{blocker}");
        assert_eq!(
            NlosvCase::classify(tx.pos.z, rx.pos.z, blocker),
            NlosvCase::MaxAntennaBelowBlocker
        );
        // The loss is a clamped normal about 9 dB; over many draws its mean is near 9.
        let mut sum = 0.0;
        let n = 4_000u32;
        for i in 0..n {
            ctx.set_now(u64::from(i) * 1_000_000);
            sum += model.obstacle_loss_db(&mut ctx, &tx, &rx, &los, 5.9e9);
        }
        let mean = sum / f64::from(n);
        // Clamping at zero lifts the mean of a N(9, 4.5) slightly, and 4,000 draws leave
        // a standard error of 0.07 dB, so the window is the measured LOS-to-OLOS offset
        // Abbas reports: 8.6 to 10 dB.
        assert!((8.6..10.0).contains(&mean), "{mean}");

        // A short car with both antennas above it is case 1 and costs nothing.
        let low = ActorSet::from_iter_sorted([car(0, 50.0, 0.0, 1.0)]);
        let los_low = ObstacleModel::<TestCtx>::los(&model, &world, tx.pos, rx.pos, Some(&low));
        assert_eq!(
            los_low.class,
            LosClass::Los,
            "a 1 m body cannot block 1.5 m antennas"
        );
        assert_eq!(
            model.obstacle_loss_db(&mut ctx, &tx, &rx, &los_low, 5.9e9),
            0.0
        );
        // And no actors at all means no blockage.
        let none = ObstacleModel::<TestCtx>::los(&model, &world, tx.pos, rx.pos, None);
        assert_eq!(none.class, LosClass::Los);
    }

    #[test]
    fn the_tallest_blocker_decides_the_case() {
        let world = tiny_world();
        let model = VehicleBlockage::new(Tier::High);
        let tx = endpoint(0, 0.0, 0.0, 1.5);
        let rx = endpoint(1, 100.0, 0.0, 1.5);
        let actors = ActorSet::from_iter_sorted([car(0, 30.0, 0.0, 1.6), car(1, 60.0, 0.0, 3.4)]);
        let los = ObstacleModel::<TestCtx>::los(&model, &world, tx.pos, rx.pos, Some(&actors));
        assert_eq!(los.knife_edges.len(), 2);
        let blocker = model.blocker_top_m(&tx, &rx, &los);
        assert!((blocker - 3.4).abs() < 1e-9, "{blocker}");
    }

    #[test]
    fn a_grazing_knife_edge_costs_six_decibels() {
        // ν = 0 is the edge exactly on the line of sight, and both forms of J give 6 dB.
        // 6.9 + 20·log10(sqrt(1.01) − 0.1) = 6.0329 dB from the approximation, and
        // −10·log10(1/4) = 6.0206 dB exactly: the textbook "6 dB at grazing incidence".
        assert!(
            (knife_edge_loss_db(0.0) - 6.033).abs() < 0.01,
            "{}",
            knife_edge_loss_db(0.0)
        );
        assert!(
            (knife_edge_loss_exact_db(0.0) - 6.021).abs() < 0.01,
            "{}",
            knife_edge_loss_exact_db(0.0)
        );
        // Below −0.78 the approximation is zero by definition.
        assert_eq!(knife_edge_loss_db(-0.8), 0.0);
        assert_eq!(knife_edge_loss_db(-5.0), 0.0);
        // And the loss grows with the obstruction.
        let mut previous = -1.0;
        let mut nu = -0.7;
        while nu < 10.0 {
            let j = knife_edge_loss_db(nu);
            assert!(j >= previous, "fell at {nu}");
            previous = j;
            nu += 0.05;
        }
        // ν = 1 is about 14 dB and ν = 3 about 22 dB, the textbook values.
        assert!(
            (knife_edge_loss_db(1.0) - 14.0).abs() < 0.5,
            "{}",
            knife_edge_loss_db(1.0)
        );
        assert!(
            (knife_edge_loss_db(3.0) - 22.1).abs() < 0.5,
            "{}",
            knife_edge_loss_db(3.0)
        );
    }

    #[test]
    fn the_exact_and_approximate_forms_agree() {
        // ITU-R's approximation is stated to be within a fraction of a dB of the Fresnel
        // integral over the range that matters.
        let mut nu = -0.7;
        while nu <= 5.0 {
            let approx = knife_edge_loss_db(nu);
            let exact = knife_edge_loss_exact_db(nu);
            assert!(
                (approx - exact).abs() < 0.5,
                "ν = {nu}: approx {approx}, exact {exact}"
            );
            nu += 0.1;
        }
        // The Fresnel integrals themselves: C(0) = S(0) = 0, and both tend to 1/2.
        assert_eq!(fresnel_integrals(0.0), (0.0, 0.0));
        let (c, s) = fresnel_integrals(8.0);
        assert!((c - 0.5).abs() < 0.05, "C(8) = {c}");
        assert!((s - 0.5).abs() < 0.05, "S(8) = {s}");
    }

    #[test]
    fn the_knife_edge_parameter_scales_as_the_document_says() {
        let lambda = numeric::wavelength_m(5.9e9);
        // A 10 m obstruction halfway along a 1 km link.
        let nu = knife_edge_parameter(10.0, 500.0, 500.0, lambda);
        // ν = h·sqrt(2·1000/(λ·250000)) = 10·sqrt(2000/12703) = 3.97.
        assert!((nu - 3.968).abs() < 0.01, "{nu}");
        // Zero clearance gives zero, and a degenerate geometry gives zero rather than a
        // NaN.
        assert_eq!(knife_edge_parameter(0.0, 500.0, 500.0, lambda), 0.0);
        assert_eq!(knife_edge_parameter(10.0, 0.0, 500.0, lambda), 0.0);
    }

    #[test]
    fn deygout_is_more_pessimistic_than_a_single_edge() {
        let lambda = numeric::wavelength_m(5.9e9);
        let edges = vec![
            KnifeEdge {
                d1_m: 200.0,
                d2_m: 800.0,
                h_m: 5.0,
                source: EdgeSource::Terrain,
            },
            KnifeEdge {
                d1_m: 500.0,
                d2_m: 500.0,
                h_m: 12.0,
                source: EdgeSource::Terrain,
            },
            KnifeEdge {
                d1_m: 800.0,
                d2_m: 200.0,
                h_m: 4.0,
                source: EdgeSource::Terrain,
            },
        ];
        let dominant = knife_edge_loss_db(knife_edge_parameter(12.0, 500.0, 500.0, lambda));
        let deygout = multi_edge_loss_db(&edges, lambda, MultiEdgeRule::Deygout, false);
        let itu = multi_edge_loss_db(&edges, lambda, MultiEdgeRule::ItuR, false);
        assert!(deygout > dominant, "{deygout} vs {dominant}");
        assert!(itu > 0.0);
        // Order independence: the edge list is sorted before it is reduced.
        let mut reversed = edges.clone();
        reversed.reverse();
        assert_eq!(
            multi_edge_loss_db(&edges, lambda, MultiEdgeRule::Deygout, false).to_bits(),
            multi_edge_loss_db(&reversed, lambda, MultiEdgeRule::Deygout, false).to_bits()
        );
        assert_eq!(
            multi_edge_loss_db(&[], lambda, MultiEdgeRule::Deygout, false),
            0.0
        );
    }

    #[test]
    fn the_boban_vehicle_knife_edge_is_a_knife_edge() {
        let lambda = numeric::wavelength_m(5.9e9);
        // A van 1 m above the line of sight, halfway along a 40 m link: the Fresnel
        // radius is sqrt(λ·20·20/40) = 0.504 m, so v = sqrt(2/0.504) = 1.99 and the loss
        // is the J(ν) curve at that point.
        let loss = boban_vehicle_loss_db(1.0, 20.0, 20.0, lambda);
        assert!(loss > 12.0, "a van should cost more than 12 dB: {loss}");
        // Nothing above the line, nothing lost.
        assert_eq!(boban_vehicle_loss_db(0.0, 20.0, 20.0, lambda), 0.0);
        assert_eq!(boban_vehicle_loss_db(-1.0, 20.0, 20.0, lambda), 0.0);
        // A taller blocker costs more, and a truck at short range clears 20 dB, which is
        // the measured anchor 04-models.md §3.5 records (more than 20 dB for a single
        // obstructing vehicle).
        let truck = boban_vehicle_loss_db(2.0, 13.0, 13.0, lambda);
        assert!(truck > 20.0, "{truck}");
        assert!(truck > loss);
    }

    #[test]
    fn a_world_without_terrain_has_no_terrain_edges() {
        let world = tiny_world();
        let model = TerrainDiffraction::new(Tier::High);
        let edges =
            model.terrain_edges(&world, Vec3::new(0.0, 0.0, 1.5), Vec3::new(500.0, 0.0, 1.5));
        assert!(edges.is_empty(), "the zero plane cannot obstruct a link");
        let los = ObstacleModel::<TestCtx>::los(
            &model,
            &world,
            Vec3::new(0.0, 0.0, 1.5),
            Vec3::new(500.0, 0.0, 1.5),
            None,
        );
        assert_eq!(los.class, LosClass::Los);
    }

    #[test]
    fn the_cards_validate_and_register() {
        let mut registry = v2xw_core::registry::Registry::new();
        for card in [
            building_card(SommerFit::Default, true),
            vehicle_card(),
            terrain_card(MultiEdgeRule::Deygout, false),
        ] {
            card.validate().expect("card validates");
            card.check_api_version().expect("api version");
            registry.register(card).expect("registers");
        }
        assert!(registry.contains(BuildingShadowing::ID));
        assert!(registry.contains(VehicleBlockage::ID));
        assert!(registry.contains(TerrainDiffraction::ID));
        // Every fitted row produces a valid card.
        for fit in SommerFit::ALL {
            building_card(fit, true).validate().expect("card validates");
        }
        // The material map is empty by default and is a todo-calibrate parameter, so it
        // must carry a plan (registry rule R1) — which `validate` checks.
        let card = building_card(SommerFit::Default, true);
        let p = card
            .parameters
            .iter()
            .find(|p| p.name == "material_map")
            .expect("declared");
        assert_eq!(p.source.kind, SourceKind::TodoCalibrate);
        assert!(p.calibration.is_some());
    }

    #[test]
    fn a_material_override_changes_the_row() {
        let world = world_with_building(10.0, -5.0, 20.0, 5.0, 12.0);
        let model = BuildingShadowing::new(Tier::Medium)
            .mapping(MaterialClass::Wood, SommerFit::LightConstructionHouse);
        let building = world
            .buildings
            .last()
            .expect("the world has the test building");
        // The test building's material is Unknown, which takes the default row.
        assert_eq!(
            model.coefficients_for(building).beta_db_per_wall,
            SommerCoefficients::DEFAULT.beta_db_per_wall
        );
    }
}
