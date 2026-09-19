//! What a node *believes* about where and when it is.
//!
//! [`crate::kinematics::Kinematics`] is ground truth: where an actor really is, published
//! by the mobility provider. [`PositionEstimate`] is the node's belief about the same
//! thing, produced by a GNSS model from that ground truth plus error, outages, multipath
//! and jamming (03-interfaces.md §3, `GnssModel::estimate`). **They are different types on
//! purpose.** A CAM carries the belief; a plausibility detector compares two beliefs; the
//! safety application reasons about the belief; only the simulator itself and a metric
//! computed for the analyst may look at the truth. A crate that passed `Kinematics` where a
//! `PositionEstimate` belongs would be leaking ground truth into a node — invariant I-C2 —
//! and the type system is the cheapest place to catch it.
//!
//! These types live in the contract crate because three crates sit on different sides of
//! them: `v2xw-node` (the GNSS model produces one, the node runtime caches it),
//! `v2xw-msg` (the generator reads one into every CAM/BSM) and `v2xw-threat` (a detector
//! and an attacker see one through [`crate::nodeview::NodeView`]). None of them can define
//! it locally without the others having to convert.

use serde::{Deserialize, Serialize};

use crate::geom::Vec3;
use crate::math;
use crate::time::SimTime;

/// How good a node's position fix is, worst first.
///
/// [`Ord`] follows the fidelity ladder — `NoFix < DeadReckoning < TwoD < ThreeD <
/// Differential < Rtk` — so `fix >= FixQuality::TwoD` is the idiomatic "good enough to
/// transmit" test and the enum can be compared, sorted and bucketed without a lookup table.
/// The ladder is about *how the fix was obtained*, not about a particular accuracy number:
/// the metres live in [`PositionEstimate`]'s error ellipse, where a model's card can
/// justify them, and a tunnel-degraded RTK receiver may well be worse than a clear-sky 3-D
/// one.
///
/// The variants are the ones V2X message sets and receivers actually distinguish: SAE
/// J2735's `PositionalAccuracy`/`GNSSstatus` and ETSI CDD's `PositionConfidenceEllipse`
/// both express "no fix", "2-D only", "3-D", "differentially corrected" and "carrier-phase
/// (RTK)", and every receiver datasheet quotes dead reckoning separately because its error
/// grows with time rather than staying bounded.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FixQuality {
    /// No fix at all: fewer than three satellites, a cold start, a tunnel, or jamming.
    /// The position fields are meaningless and must not be transmitted.
    #[default]
    NoFix,
    /// Propagated from the last fix by inertial sensors and odometry, with no satellite
    /// update. Bounded in the short term and unbounded in the long term, which is why it
    /// ranks below a real fix however recent it is.
    DeadReckoning,
    /// Horizontal fix only: latitude and longitude are observed, altitude is assumed.
    TwoD,
    /// Full three-dimensional fix.
    ThreeD,
    /// Three-dimensional with differential corrections (SBAS/DGNSS): the correlated
    /// atmospheric and orbital errors are removed.
    Differential,
    /// Carrier-phase (RTK) fix: centimetre class, and the best a vehicle receiver reports.
    Rtk,
}

impl FixQuality {
    /// Every quality, worst first. For reports and exhaustiveness tests.
    pub const ALL: [FixQuality; 6] = [
        FixQuality::NoFix,
        FixQuality::DeadReckoning,
        FixQuality::TwoD,
        FixQuality::ThreeD,
        FixQuality::Differential,
        FixQuality::Rtk,
    ];

    /// True if there is any position to speak of — anything but [`FixQuality::NoFix`].
    pub const fn has_position(self) -> bool {
        !matches!(self, FixQuality::NoFix)
    }

    /// True if the fix observed altitude rather than assuming it.
    pub const fn is_three_d(self) -> bool {
        matches!(
            self,
            FixQuality::ThreeD | FixQuality::Differential | FixQuality::Rtk
        )
    }

    /// True if the fix came from satellites now, rather than from propagating an old one.
    ///
    /// The distinction a clock model needs: only a satellite fix disciplines the node's
    /// time (03-interfaces.md §3, `ClockModel::read`), so a node in dead reckoning is also
    /// a node whose clock is drifting freely.
    pub const fn is_satellite_fix(self) -> bool {
        !matches!(self, FixQuality::NoFix | FixQuality::DeadReckoning)
    }

    /// The kebab-case name, identical to the serde representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            FixQuality::NoFix => "no-fix",
            FixQuality::DeadReckoning => "dead-reckoning",
            FixQuality::TwoD => "two-d",
            FixQuality::ThreeD => "three-d",
            FixQuality::Differential => "differential",
            FixQuality::Rtk => "rtk",
        }
    }
}

impl core::fmt::Display for FixQuality {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A node's belief about its own position, velocity and time (03-interfaces.md §3).
///
/// Produced by a `GnssModel` from ground truth, cached by the node runtime, read by every
/// message generator and safety application, and carried — quantised — in every CAM, BSM
/// and PSM. The uncertainty is a **horizontal error ellipse**, the form both message sets
/// use: a semi-major axis, a semi-minor axis and the orientation of the major axis. It is
/// not a scalar "accuracy", because GNSS error is not isotropic — a receiver hemmed in by
/// buildings on one axis of a street canyon is far worse across the street than along it,
/// and that anisotropy is exactly what a position-plausibility detector keys on.
///
/// # Frames and conventions
///
/// * `pos` and `vel` are world-local East-North-Up metres and m/s, the one convention
///   everywhere (build decision D6, [`crate::geom`]).
/// * `heading_rad` is ENU radians, `0 = east`, counter-clockwise — the same convention as
///   [`crate::kinematics::Kinematics::heading_rad`], **not** a compass bearing.
/// * `orientation_rad` uses the same convention: it is the direction of the *semi-major
///   axis*, and is only defined modulo π (an ellipse is symmetric).
/// * `time_ns` is what the node believes [`SimTime`] to be, which is not the true instant:
///   a node with a drifting clock (03-interfaces.md §3, `ClockModel`) believes a different
///   number, and the difference is what makes a replay-detection window interesting.
///
/// # Quantisation
///
/// Every field here reaches a recorded artefact and a transmitted message, so every one of
/// them passes the writer-side quantiser first (ADR 0004 decision 7, build decision D9).
/// [`PositionEstimate::quantized`] applies the declared grids — [`PositionEstimate::Q_M`]
/// for metres and metres per second, [`PositionEstimate::Q_RAD`] for angles — in one call,
/// so no caller has to remember which field takes which.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PositionEstimate {
    /// Believed position, world-local ENU metres.
    pub pos: Vec3,
    /// Believed velocity, m/s.
    pub vel: Vec3,
    /// Believed heading, ENU radians, `0 = east`, counter-clockwise.
    pub heading_rad: f64,
    /// Semi-major axis of the horizontal error ellipse, metres.
    ///
    /// [`f64::INFINITY`] for [`PositionEstimate::no_fix`], where it means "this position
    /// means nothing". JSON cannot carry an infinity, so the field is encoded through
    /// [`crate::serde_sentinel::f64_inf`] — `null` on the wire, the sentinel back — and a
    /// no-fix belief survives its own round trip.
    #[serde(with = "crate::serde_sentinel::f64_inf")]
    pub semi_major_m: f64,
    /// Semi-minor axis of the horizontal error ellipse, metres.
    ///
    /// Infinite for [`PositionEstimate::no_fix`] and encoded as for
    /// [`PositionEstimate::semi_major_m`].
    #[serde(with = "crate::serde_sentinel::f64_inf")]
    pub semi_minor_m: f64,
    /// Orientation of the semi-major axis, ENU radians, `0 = east`, counter-clockwise.
    /// Defined modulo π.
    pub orientation_rad: f64,
    /// The instant the node believes this estimate to be valid at — its own clock, not the
    /// simulator's.
    pub time_ns: SimTime,
    /// How the fix was obtained.
    pub fix: FixQuality,
}

impl PositionEstimate {
    /// The declared quantum for every metre and metre-per-second field here: 1 mm, the
    /// default grid for metres in build decision D9.
    pub const Q_M: f64 = 1e-3;

    /// The declared quantum for every angle here: 1 µrad.
    ///
    /// One microradian is 0.2 mm of lateral displacement at 200 m, which is below the
    /// metre grid of every position these angles are used with, so the grid costs nothing
    /// real and keeps a heading from being the one field that escapes quantisation — which
    /// is the exact bug ADR 0004 was written about.
    pub const Q_RAD: f64 = 1e-6;

    /// A belief with no fix at all: the position fields are zero and mean nothing.
    ///
    /// What a node holds before its first fix, and what a GNSS model returns during a
    /// total outage. [`FixQuality::has_position`] is the guard every consumer uses. The
    /// error ellipse is infinite rather than zero, so a consumer that ignores the guard and
    /// divides by an uncertainty gets an obviously wrong answer instead of a plausible one.
    /// (JSON cannot carry an infinity, so the two axes are encoded through
    /// [`crate::serde_sentinel::f64_inf`]: `null` on the wire, the sentinel back. The
    /// binary wire protocol carries them as they are.)
    pub fn no_fix(time_ns: SimTime) -> Self {
        Self {
            pos: Vec3::ZERO,
            vel: Vec3::ZERO,
            heading_rad: 0.0,
            semi_major_m: f64::INFINITY,
            semi_minor_m: f64::INFINITY,
            orientation_rad: 0.0,
            time_ns,
            fix: FixQuality::NoFix,
        }
    }

    /// The belief an *ideal* receiver would hold: ground truth, with a zero error ellipse
    /// and a perfectly disciplined clock.
    ///
    /// The abstract tier's GNSS model, the baseline every error model is measured against,
    /// and the right default for a scenario that is not studying positioning. It is
    /// deliberately the only function in this crate that converts ground truth into a
    /// belief: a call to it is visible in a review, where an implicit conversion would not
    /// be.
    pub fn perfect(k: &crate::kinematics::Kinematics) -> Self {
        Self {
            pos: k.pos,
            vel: k.vel,
            heading_rad: k.heading_rad,
            semi_major_m: 0.0,
            semi_minor_m: 0.0,
            orientation_rad: 0.0,
            time_ns: k.t,
            fix: FixQuality::Rtk,
        }
    }

    /// Believed ground speed, m/s: the horizontal magnitude of [`PositionEstimate::vel`].
    pub fn ground_speed_mps(&self) -> f64 {
        self.vel.norm_2d()
    }

    /// The area of the horizontal error ellipse, m² — `π · a · b`.
    ///
    /// A single scalar for ranking or thresholding uncertainty when the shape does not
    /// matter. Infinite for [`PositionEstimate::no_fix`], which is the honest answer.
    pub fn error_ellipse_area_m2(&self) -> f64 {
        core::f64::consts::PI * self.semi_major_m * self.semi_minor_m
    }

    /// The 1-σ uncertainty in the direction `bearing_rad` (ENU radians), metres.
    ///
    /// `sqrt((a·cos θ)² + (b·sin θ)²)` with `θ` measured from the semi-major axis: the
    /// radius of the ellipse in that direction. This is what a detector needs to ask "is
    /// this reported position implausible *given the direction I would expect it to be
    /// wrong in*", and it is why the ellipse is carried rather than a scalar.
    pub fn uncertainty_towards_m(&self, bearing_rad: f64) -> f64 {
        let theta = bearing_rad - self.orientation_rad;
        let (s, c) = math::sin_cos(theta);
        let a = self.semi_major_m * c;
        let b = self.semi_minor_m * s;
        math::sqrt(a * a + b * b)
    }

    /// True if every field is finite and the ellipse is well formed
    /// (`semi_major >= semi_minor >= 0`).
    ///
    /// The check a conformance test runs over a GNSS model's output. A [`FixQuality::NoFix`]
    /// estimate is *not* well formed by this definition — its axes are infinite — which is
    /// deliberate: it is the one estimate whose numbers must never be used.
    pub fn is_well_formed(&self) -> bool {
        self.pos.is_finite()
            && self.vel.is_finite()
            && self.heading_rad.is_finite()
            && self.orientation_rad.is_finite()
            && self.semi_major_m.is_finite()
            && self.semi_minor_m.is_finite()
            && self.semi_minor_m >= 0.0
            && self.semi_major_m >= self.semi_minor_m
    }

    /// This estimate with every float on its declared grid (build decision D9).
    ///
    /// Apply it once, at the writer: when the estimate is recorded, when it is encoded into
    /// a message, and before any threshold comparison whose outcome is compared across
    /// engines (build decision D10). `time_ns` is an integer and needs no grid.
    pub fn quantized(&self) -> Self {
        let q = |x: f64| math::quantize_to(x, Self::Q_M);
        let qa = |x: f64| math::quantize_to(x, Self::Q_RAD);
        Self {
            pos: Vec3::new(q(self.pos.x), q(self.pos.y), q(self.pos.z)),
            vel: Vec3::new(q(self.vel.x), q(self.vel.y), q(self.vel.z)),
            heading_rad: qa(self.heading_rad),
            semi_major_m: q(self.semi_major_m),
            semi_minor_m: q(self.semi_minor_m),
            orientation_rad: qa(self.orientation_rad),
            time_ns: self.time_ns,
            fix: self.fix,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinematics::Kinematics;
    use crate::time::NS_PER_MS;

    fn sample() -> PositionEstimate {
        PositionEstimate {
            pos: Vec3::new(10.0, 20.0, 0.5),
            vel: Vec3::new(3.0, 4.0, 0.0),
            heading_rad: 0.75,
            semi_major_m: 4.0,
            semi_minor_m: 1.0,
            orientation_rad: 0.0,
            time_ns: 1_000 * NS_PER_MS,
            fix: FixQuality::ThreeD,
        }
    }

    /// The ladder is the ordering, and the predicates follow it.
    #[test]
    fn fix_quality_orders_by_fidelity() {
        assert!(FixQuality::NoFix < FixQuality::DeadReckoning);
        assert!(FixQuality::DeadReckoning < FixQuality::TwoD);
        assert!(FixQuality::TwoD < FixQuality::ThreeD);
        assert!(FixQuality::ThreeD < FixQuality::Differential);
        assert!(FixQuality::Differential < FixQuality::Rtk);
        for (i, q) in FixQuality::ALL.iter().enumerate() {
            assert!(FixQuality::ALL[..i].iter().all(|p| p < q));
        }

        assert!(!FixQuality::NoFix.has_position());
        assert!(FixQuality::DeadReckoning.has_position());
        assert!(!FixQuality::TwoD.is_three_d());
        assert!(FixQuality::Rtk.is_three_d());
        assert!(!FixQuality::DeadReckoning.is_satellite_fix());
        assert!(FixQuality::TwoD.is_satellite_fix());
        assert_eq!(FixQuality::default(), FixQuality::NoFix);

        assert_eq!(FixQuality::DeadReckoning.to_string(), "dead-reckoning");
        assert_eq!(
            serde_json::to_string(&FixQuality::ThreeD).unwrap(),
            "\"three-d\""
        );
        for q in FixQuality::ALL {
            assert_eq!(
                serde_json::to_string(&q).unwrap(),
                format!("\"{}\"", q.as_str()),
                "the Display name and the wire name must agree"
            );
        }
    }

    /// The ellipse is not a scalar: the uncertainty in one direction differs from the
    /// uncertainty in another, and that is the whole reason three fields are carried.
    #[test]
    fn the_error_ellipse_is_directional() {
        let p = sample();
        // Along the major axis (orientation 0 = east) and across it.
        assert!((p.uncertainty_towards_m(0.0) - 4.0).abs() < 1e-12);
        assert!(
            (p.uncertainty_towards_m(core::f64::consts::FRAC_PI_2) - 1.0).abs() < 1e-12,
            "across the major axis the uncertainty is the minor axis"
        );
        assert!((p.uncertainty_towards_m(core::f64::consts::PI) - 4.0).abs() < 1e-12);
        // A rotated ellipse rotates with its orientation.
        let rotated = PositionEstimate {
            orientation_rad: core::f64::consts::FRAC_PI_2,
            ..p
        };
        assert!((rotated.uncertainty_towards_m(core::f64::consts::FRAC_PI_2) - 4.0).abs() < 1e-12);
        assert!((rotated.uncertainty_towards_m(0.0) - 1.0).abs() < 1e-12);

        assert!((p.error_ellipse_area_m2() - core::f64::consts::PI * 4.0).abs() < 1e-12);
        assert_eq!(p.ground_speed_mps(), 5.0);
        assert!(p.is_well_formed());

        // A circular estimate is the degenerate case and stays consistent.
        let circle = PositionEstimate {
            semi_major_m: 2.0,
            semi_minor_m: 2.0,
            ..p
        };
        for bearing in [0.0, 0.3, 1.0, 2.5, -1.0] {
            assert!((circle.uncertainty_towards_m(bearing) - 2.0).abs() < 1e-12);
        }
    }

    /// The no-fix estimate must be recognisable and must not look like a position at the
    /// world origin, which is what a zeroed struct would look like.
    #[test]
    fn a_no_fix_estimate_is_unusable_by_construction() {
        let p = PositionEstimate::no_fix(7 * NS_PER_MS);
        assert_eq!(p.fix, FixQuality::NoFix);
        assert!(!p.fix.has_position());
        assert!(
            !p.is_well_formed(),
            "an infinite ellipse is not well formed"
        );
        assert!(p.error_ellipse_area_m2().is_infinite());
        assert_eq!(p.time_ns, 7 * NS_PER_MS);
    }

    /// The one sanctioned conversion from ground truth, and the fact that it is a copy —
    /// a belief built from truth has no error, which is exactly what makes it a baseline
    /// and not a model.
    #[test]
    fn the_perfect_estimate_mirrors_ground_truth() {
        let k = Kinematics::at_rest(5 * NS_PER_MS, Vec3::new(1.0, 2.0, 3.0));
        let p = PositionEstimate::perfect(&k);
        assert_eq!(p.pos, k.pos);
        assert_eq!(p.vel, k.vel);
        assert_eq!(p.heading_rad, k.heading_rad);
        assert_eq!(p.time_ns, k.t);
        assert_eq!(p.semi_major_m, 0.0);
        assert_eq!(p.semi_minor_m, 0.0);
        assert_eq!(p.fix, FixQuality::Rtk);
        assert!(p.is_well_formed());
        assert_eq!(p.error_ellipse_area_m2(), 0.0);
    }

    /// Every float here reaches a message and a recording, so every one of them must land
    /// on its declared grid — including the ones a hand-written quantiser would forget.
    #[test]
    fn quantization_covers_every_float() {
        let p = PositionEstimate {
            pos: Vec3::new(10.000_499_9, 20.000_6, -0.123_456_7),
            vel: Vec3::new(3.141_492_6, -4.000_04, 0.000_49),
            heading_rad: 0.750_000_499_9,
            semi_major_m: 4.000_499_9,
            semi_minor_m: 1.000_5,
            orientation_rad: -1.234_567_891,
            time_ns: 1_000 * NS_PER_MS,
            fix: FixQuality::Differential,
        };
        let q = p.quantized();

        for x in [q.pos.x, q.pos.y, q.pos.z, q.vel.x, q.vel.y, q.vel.z] {
            assert!(
                math::is_on_grid(x, PositionEstimate::Q_M),
                "{x} is off grid"
            );
        }
        for a in [q.heading_rad, q.orientation_rad] {
            assert!(
                math::is_on_grid(a, PositionEstimate::Q_RAD),
                "{a} is off grid"
            );
        }
        assert!(math::is_on_grid(q.semi_major_m, PositionEstimate::Q_M));
        assert!(math::is_on_grid(q.semi_minor_m, PositionEstimate::Q_M));

        // The raw value is *not* on the grid, so the test above is not vacuous.
        assert!(!math::is_on_grid(p.pos.z, PositionEstimate::Q_M));
        assert!(!math::is_on_grid(
            p.orientation_rad,
            PositionEstimate::Q_RAD
        ));

        // Idempotent, and the integer and enum fields pass through untouched.
        assert_eq!(q.quantized(), q);
        assert_eq!(q.time_ns, p.time_ns);
        assert_eq!(q.fix, p.fix);
        assert_eq!(q.pos.x, 10.0);
        assert_eq!(q.pos.y, 20.001);

        let round_tripped: PositionEstimate =
            serde_json::from_str(&serde_json::to_string(&q).unwrap()).unwrap();
        assert_eq!(round_tripped, q);
    }

    /// **A no-fix belief must round-trip too.** It is the belief every node holds before
    /// its first fix, in a tunnel and under jamming — and it was the one this type could
    /// not read back: the infinite ellipse axes serialised to `null`, which the derived
    /// `Deserialize` for `f64` refuses. The test above missed it because it round-trips a
    /// *finite* estimate only.
    #[test]
    fn a_no_fix_belief_survives_a_json_round_trip() {
        let pe = PositionEstimate::no_fix(7);
        let json = serde_json::to_string(&pe).unwrap();
        assert!(
            json.contains(r#""semi_major_m":null"#) && json.contains(r#""semi_minor_m":null"#),
            "{json}"
        );
        let back: PositionEstimate = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pe);
        assert!(back.semi_major_m.is_infinite() && back.semi_major_m > 0.0);
        assert!(back.semi_minor_m.is_infinite() && back.semi_minor_m > 0.0);
        assert_eq!(back.fix, FixQuality::NoFix);
        assert!(
            !back.is_well_formed(),
            "a no-fix estimate is still not usable"
        );
        assert!(back.error_ellipse_area_m2().is_infinite());

        // Quantising first changes nothing: the quantiser passes sentinels through.
        let q = pe.quantized();
        assert_eq!(
            serde_json::from_str::<PositionEstimate>(&serde_json::to_string(&q).unwrap()).unwrap(),
            pe
        );

        // And the finite estimate's axes still travel as numbers.
        let json = serde_json::to_string(&sample()).unwrap();
        assert!(json.contains(r#""semi_major_m":4.0"#), "{json}");
        assert_eq!(
            serde_json::from_str::<PositionEstimate>(&json).unwrap(),
            sample()
        );
    }
}
