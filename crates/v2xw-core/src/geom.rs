//! Geometry: vectors, lane positions, body dimensions and bounding boxes.
//!
//! The world frame is **world-local East-North-Up metres**: `x` east, `y` north, `z` up,
//! with the world origin `(lat, lon, alt)` stored on the world (03-interfaces.md §1, §2).
//! Headings are radians in that frame, `0 = east`, counter-clockwise.
//!
//! Everything is `f64`. Arithmetic here is `+ - * /` and `sqrt` only, all of which are
//! IEEE-754 exact operations, so this module is deterministic without needing
//! [`crate::math`] — except for the few places that need a transcendental, which call it.

use serde::{Deserialize, Serialize};

use crate::ids::LaneId;
use crate::math;

/// A point or vector in world-local East-North-Up metres.
///
/// Addition, subtraction, scaling by a scalar and negation are the operators `+`, `-`,
/// `*` and unary `-` ([`core::ops::Add`], [`core::ops::Sub`], [`core::ops::Mul<f64>`],
/// [`core::ops::Neg`]); [`Vec3::scale`] is the same as `* k` and reads better in a chain.
/// Everything else is an inherent method.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Vec3 {
    /// East, metres.
    pub x: f64,
    /// North, metres.
    pub y: f64,
    /// Up, metres.
    pub z: f64,
}

impl Vec3 {
    /// The origin, `(0, 0, 0)`.
    pub const ZERO: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Creates a vector from its components, in metres.
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// Creates a ground-level vector, `z = 0`.
    pub const fn new_2d(x: f64, y: f64) -> Self {
        Self { x, y, z: 0.0 }
    }

    /// Multiplies every component by `k`.
    pub fn scale(self, k: f64) -> Vec3 {
        Vec3::new(self.x * k, self.y * k, self.z * k)
    }

    /// Dot product.
    pub fn dot(self, other: Vec3) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// Cross product, `self × other`.
    pub fn cross(self, other: Vec3) -> Vec3 {
        Vec3::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    /// Euclidean length, metres.
    pub fn norm(self) -> f64 {
        math::sqrt(self.dot(self))
    }

    /// Length of the horizontal (east-north) part, metres.
    pub fn norm_2d(self) -> f64 {
        math::sqrt(self.x * self.x + self.y * self.y)
    }

    /// Squared length, metres². Cheaper than [`Vec3::norm`] for comparisons.
    pub fn norm_squared(self) -> f64 {
        self.dot(self)
    }

    /// Unit vector in the same direction, or [`Vec3::ZERO`] if `self` has zero length.
    pub fn normalized(self) -> Vec3 {
        let n = self.norm();
        if n == 0.0 {
            Vec3::ZERO
        } else {
            self.scale(1.0 / n)
        }
    }

    /// Distance between two points in 3-D, metres.
    pub fn distance(self, other: Vec3) -> f64 {
        (self - other).norm()
    }

    /// Distance between two points in the horizontal plane, metres.
    ///
    /// The radio and mobility models use the 2-D distance far more often than the 3-D
    /// one, because path-loss models are defined on ground distance.
    pub fn distance_2d(self, other: Vec3) -> f64 {
        (self - other).norm_2d()
    }

    /// Linear interpolation: `self` at `k = 0`, `other` at `k = 1`.
    ///
    /// `k` is not clamped, so `k > 1` extrapolates — which is what the constant-velocity
    /// rule of [`crate::kinematics`] wants.
    pub fn lerp(self, other: Vec3, k: f64) -> Vec3 {
        Vec3::new(
            self.x + (other.x - self.x) * k,
            self.y + (other.y - self.y) * k,
            self.z + (other.z - self.z) * k,
        )
    }

    /// Heading of the horizontal part, radians, `0 = east`, counter-clockwise.
    pub fn heading_2d(self) -> f64 {
        math::atan2(self.y, self.x)
    }

    /// True if every component is finite (no NaN, no infinity).
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl core::ops::Add for Vec3 {
    type Output = Vec3;
    /// Component-wise sum.
    fn add(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl core::ops::Sub for Vec3 {
    type Output = Vec3;
    /// Component-wise difference.
    fn sub(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

impl core::ops::Mul<f64> for Vec3 {
    type Output = Vec3;
    /// Scales every component by `k`.
    fn mul(self, k: f64) -> Vec3 {
        self.scale(k)
    }
}

impl core::ops::Neg for Vec3 {
    type Output = Vec3;
    /// Reverses the vector.
    fn neg(self) -> Vec3 {
        self.scale(-1.0)
    }
}

impl core::fmt::Display for Vec3 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "({:.3}, {:.3}, {:.3})", self.x, self.y, self.z)
    }
}

/// A position on the lane graph: which lane, how far along it, how far off its centre.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LanePos {
    /// The lane this position is on.
    pub lane: LaneId,
    /// Longitudinal offset from the lane's start along its centreline, metres.
    pub s_m: f64,
    /// Lateral offset from the centreline, metres; positive to the left of travel.
    pub d_m: f64,
}

impl LanePos {
    /// Creates a lane position.
    pub const fn new(lane: LaneId, s_m: f64, d_m: f64) -> Self {
        Self { lane, s_m, d_m }
    }

    /// Creates a lane position on the centreline (`d_m = 0`).
    pub const fn centred(lane: LaneId, s_m: f64) -> Self {
        Self::new(lane, s_m, 0.0)
    }

    /// The declared quantum for both offsets: 1 mm, the metre grid of build decision D9.
    pub const Q_M: f64 = 1e-3;

    /// This lane position with both offsets on their declared grid (build decision D9).
    ///
    /// A lane position rides into every artefact inside [`crate::kinematics::Kinematics`],
    /// so it quantises like everything else that reaches one. The lane id is an integer and
    /// needs no grid.
    pub fn quantized(&self) -> Self {
        Self {
            lane: self.lane,
            s_m: crate::math::quantize_to(self.s_m, Self::Q_M),
            d_m: crate::math::quantize_to(self.d_m, Self::Q_M),
        }
    }
}

/// Body dimensions of an actor, metres.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Dims {
    /// Length along the heading, metres.
    pub length_m: f64,
    /// Width across the heading, metres.
    pub width_m: f64,
    /// Height above ground, metres.
    pub height_m: f64,
}

impl Dims {
    /// Creates a dimension triple.
    pub const fn new(length_m: f64, width_m: f64, height_m: f64) -> Self {
        Self {
            length_m,
            width_m,
            height_m,
        }
    }

    /// A passenger car: 4.5 × 1.8 × 1.5 m. A placeholder for tests and defaults; real
    /// values come from the scenario's vehicle classes.
    pub const CAR: Dims = Dims::new(4.5, 1.8, 1.5);

    /// A pedestrian: 0.5 × 0.5 × 1.7 m.
    pub const PEDESTRIAN: Dims = Dims::new(0.5, 0.5, 1.7);

    /// The declared quantum for all three dimensions: 1 mm, the metre grid of D9.
    pub const Q_M: f64 = 1e-3;

    /// These dimensions with every float on its declared grid (build decision D9).
    ///
    /// Dimensions reach an artefact inside [`crate::kinematics::Kinematics`] and inside
    /// every UI keyframe, and a body box is compared against another engine's, so they
    /// quantise like every other exported float.
    pub fn quantized(&self) -> Self {
        Self {
            length_m: crate::math::quantize_to(self.length_m, Self::Q_M),
            width_m: crate::math::quantize_to(self.width_m, Self::Q_M),
            height_m: crate::math::quantize_to(self.height_m, Self::Q_M),
        }
    }
}

impl Default for Dims {
    fn default() -> Self {
        Dims::CAR
    }
}

/// An axis-aligned bounding box in world-local metres.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bbox {
    /// Minimum corner (smallest x, y, z).
    pub min: Vec3,
    /// Maximum corner (largest x, y, z).
    pub max: Vec3,
}

impl Bbox {
    /// Creates a box from two corners, ordering the components so that `min ≤ max`.
    pub fn new(a: Vec3, b: Vec3) -> Self {
        Self {
            min: Vec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: Vec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
        }
    }

    /// An empty box: `min` at `+∞`, `max` at `-∞`, so [`Bbox::include`] of the first
    /// point yields a degenerate box at that point.
    pub fn empty() -> Self {
        Self {
            min: Vec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY),
            max: Vec3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
        }
    }

    /// The bounding box of a set of points (the empty box if `points` is empty).
    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        let mut b = Bbox::empty();
        for p in points {
            b.include(p);
        }
        b
    }

    /// True if the box contains no points (any `min` component exceeds its `max`).
    pub fn is_empty(self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    /// True if `p` is inside the box or on its boundary.
    pub fn contains(self, p: Vec3) -> bool {
        p.x >= self.min.x
            && p.x <= self.max.x
            && p.y >= self.min.y
            && p.y <= self.max.y
            && p.z >= self.min.z
            && p.z <= self.max.z
    }

    /// True if `p` is inside the box in the horizontal plane, ignoring `z`.
    pub fn contains_2d(self, p: Vec3) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    /// True if the two boxes overlap or touch.
    pub fn intersects(self, other: Bbox) -> bool {
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
            && self.min.z <= other.max.z
            && self.max.z >= other.min.z
    }

    /// Grows the box by `margin_m` on every side, returning the grown box.
    ///
    /// A negative margin shrinks it, which may make it empty.
    pub fn expand(self, margin_m: f64) -> Bbox {
        let m = Vec3::new(margin_m, margin_m, margin_m);
        Bbox {
            min: self.min - m,
            max: self.max + m,
        }
    }

    /// Grows the box in place so that it contains `p`.
    pub fn include(&mut self, p: Vec3) {
        self.min = Vec3::new(
            self.min.x.min(p.x),
            self.min.y.min(p.y),
            self.min.z.min(p.z),
        );
        self.max = Vec3::new(
            self.max.x.max(p.x),
            self.max.y.max(p.y),
            self.max.z.max(p.z),
        );
    }

    /// Grows the box in place so that it contains `other`.
    ///
    /// Unioning with an empty box is a no-op, and unioning an empty box with `other`
    /// yields `other`. Both directions have to be special-cased: [`Bbox::empty`] is
    /// `min = +∞, max = −∞`, so forwarding it to [`Bbox::include`] would set `min = −∞`
    /// and `max = +∞` — the whole of space, which [`Bbox::is_empty`] then reports as
    /// *non*-empty and [`Bbox::contains`] reports as containing every point. Accumulating
    /// unions are how the world bbox, focus regions and per-actor extents are built, and
    /// one empty contributor would otherwise poison the uniform grid's cell count
    /// (ADR 0004 decision 6) for the whole run.
    pub fn union(&mut self, other: Bbox) {
        if other.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = other;
            return;
        }
        self.include(other.min);
        self.include(other.max);
    }

    /// Centre point of the box.
    pub fn center(self) -> Vec3 {
        (self.min + self.max).scale(0.5)
    }

    /// Side lengths of the box, metres.
    pub fn size(self) -> Vec3 {
        self.max - self.min
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_algebra() {
        let a = Vec3::new(1.0, 2.0, 3.0);
        let b = Vec3::new(4.0, 5.0, 6.0);
        assert_eq!(a + b, Vec3::new(5.0, 7.0, 9.0));
        assert_eq!(a - b, Vec3::new(-3.0, -3.0, -3.0));
        assert_eq!(a.scale(2.0), Vec3::new(2.0, 4.0, 6.0));
        assert_eq!(a.dot(b), 32.0);
        assert_eq!(a.cross(b), Vec3::new(-3.0, 6.0, -3.0));
        assert_eq!(a * 2.0, a.scale(2.0));
        assert_eq!(-a, Vec3::new(-1.0, -2.0, -3.0));
    }

    #[test]
    fn norms_and_distances() {
        let a = Vec3::new(3.0, 4.0, 12.0);
        assert_eq!(a.norm(), 13.0);
        assert_eq!(a.norm_2d(), 5.0);
        assert_eq!(a.norm_squared(), 169.0);
        assert_eq!(Vec3::ZERO.distance(a), 13.0);
        assert_eq!(Vec3::ZERO.distance_2d(a), 5.0);
        assert_eq!(Vec3::ZERO.normalized(), Vec3::ZERO);
        assert!((a.normalized().norm() - 1.0).abs() < 1e-15);
        assert!(a.is_finite());
        assert!(!Vec3::new(f64::NAN, 0.0, 0.0).is_finite());
    }

    #[test]
    fn heading_and_lerp() {
        assert_eq!(Vec3::new_2d(1.0, 0.0).heading_2d(), 0.0);
        assert!((Vec3::new_2d(0.0, 1.0).heading_2d() - core::f64::consts::FRAC_PI_2).abs() < 1e-15);
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(10.0, 20.0, 30.0);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        assert_eq!(a.lerp(b, 0.5), Vec3::new(5.0, 10.0, 15.0));
        assert_eq!(a.lerp(b, 2.0), Vec3::new(20.0, 40.0, 60.0));
    }

    #[test]
    fn bbox_contains_and_expands() {
        let b = Bbox::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 10.0, 5.0));
        assert!(b.contains(Vec3::new(5.0, 5.0, 2.0)));
        assert!(b.contains(Vec3::new(0.0, 0.0, 0.0)));
        assert!(!b.contains(Vec3::new(5.0, 5.0, 6.0)));
        assert!(b.contains_2d(Vec3::new(5.0, 5.0, 99.0)));
        let grown = b.expand(1.0);
        assert!(grown.contains(Vec3::new(-1.0, -1.0, -1.0)));
        assert_eq!(grown.size(), Vec3::new(12.0, 12.0, 7.0));
        assert_eq!(b.center(), Vec3::new(5.0, 5.0, 2.5));
        assert!(b.intersects(grown));
        assert!(!b.intersects(Bbox::new(
            Vec3::new(100.0, 100.0, 0.0),
            Vec3::new(101.0, 101.0, 1.0)
        )));
        // Corner order is normalised.
        assert_eq!(Bbox::new(b.max, b.min), b);
    }

    #[test]
    fn bbox_accumulates_points() {
        assert!(Bbox::empty().is_empty());
        let b = Bbox::from_points([
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(-1.0, 5.0, 0.0),
            Vec3::new(0.0, 0.0, 7.0),
        ]);
        assert_eq!(b.min, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(b.max, Vec3::new(1.0, 5.0, 7.0));
        assert!(!b.is_empty());
        let mut u = b;
        u.union(Bbox::new(
            Vec3::new(9.0, 9.0, 9.0),
            Vec3::new(9.0, 9.0, 9.0),
        ));
        assert_eq!(u.max, Vec3::new(9.0, 9.0, 9.0));
    }

    /// An empty contributor must not turn an accumulating union into the whole of space:
    /// `world_bbox = ∪ tile bboxes` is the path every world importer takes.
    #[test]
    fn union_with_an_empty_box_is_a_no_op() {
        let finite = Bbox::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 10.0, 5.0));

        let mut b = finite;
        b.union(Bbox::empty());
        assert_eq!(b, finite);
        assert!(!b.contains(Vec3::new(1e300, -1e300, 0.0)));

        let mut e = Bbox::empty();
        e.union(finite);
        assert_eq!(e, finite);

        // Empty ∪ empty stays empty, and stays *recognisably* empty.
        let mut e = Bbox::empty();
        e.union(Bbox::empty());
        assert!(e.is_empty());

        // An accumulation with empty contributors scattered through it is unaffected.
        let mut acc = Bbox::empty();
        for contribution in [
            Bbox::empty(),
            Bbox::new(Vec3::new(1.0, 1.0, 0.0), Vec3::new(2.0, 2.0, 1.0)),
            Bbox::empty(),
            Bbox::new(Vec3::new(-4.0, 0.0, 0.0), Vec3::new(0.0, 3.0, 2.0)),
        ] {
            acc.union(contribution);
        }
        assert_eq!(acc.min, Vec3::new(-4.0, 0.0, 0.0));
        assert_eq!(acc.max, Vec3::new(2.0, 3.0, 2.0));
        assert_eq!(acc.size(), Vec3::new(6.0, 3.0, 2.0));
    }

    #[test]
    fn serde_round_trip() {
        let lp = LanePos::new(LaneId::new(4), 12.5, -0.25);
        let s = serde_json::to_string(&lp).unwrap();
        assert_eq!(s, r#"{"lane":4,"s_m":12.5,"d_m":-0.25}"#);
        assert_eq!(serde_json::from_str::<LanePos>(&s).unwrap(), lp);
        let d = Dims::CAR;
        assert_eq!(
            serde_json::from_str::<Dims>(&serde_json::to_string(&d).unwrap()).unwrap(),
            d
        );
        assert_eq!(Dims::default(), Dims::CAR);
    }
}
