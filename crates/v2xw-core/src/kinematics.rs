//! Ground-truth kinematic state and the published extrapolation rule.
//!
//! [`Kinematics`] is what a mobility provider publishes for every active actor at every
//! `MobilityStep` (03-interfaces.md §1, §3). All tiers — abstract, native medium, SUMO
//! high — publish the same struct in the same frame and with the same reference point
//! (invariant I-M4): rear-axle centre for vehicles, centroid for VRUs, world-local
//! East-North-Up metres, heading `0 = east` counter-clockwise.

use serde::{Deserialize, Serialize};

use crate::geom::{Dims, LanePos, Vec3};
use crate::time::{NS_PER_S, SimTime};

/// Ground-truth kinematic state of one actor at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Kinematics {
    /// The instant this state is valid for.
    pub t: SimTime,
    /// Reference point: rear-axle centre for vehicles, centroid for VRUs, metres.
    pub pos: Vec3,
    /// Velocity, m/s.
    pub vel: Vec3,
    /// Acceleration, m/s².
    pub acc: Vec3,
    /// Heading in the ENU frame, radians, `0 = east`, counter-clockwise.
    pub heading_rad: f64,
    /// Yaw rate, rad/s, positive counter-clockwise.
    pub yaw_rate_rad_s: f64,
    /// Position on the lane graph, when the actor is on one.
    pub lane: Option<LanePos>,
    /// Body dimensions, metres.
    pub dims: Dims,
}

impl Kinematics {
    /// The declared quantum for every metre, m/s and m/s² field here: 1 mm, the default
    /// grid for metres in build decision D9.
    ///
    /// The same value [`crate::belief::PositionEstimate::Q_M`] declares, and deliberately
    /// so: the truth channel and the belief channel carry the same quantities, and a
    /// detector that compared a recorded truth against a recorded belief on two different
    /// grids would see a disagreement the engine invented.
    pub const Q_M: f64 = 1e-3;

    /// The declared quantum for every angle and angular rate here: 1 µrad, 1 µrad/s.
    ///
    /// One microradian is 0.2 mm of lateral displacement at 200 m, far below
    /// [`Kinematics::Q_M`], so the grid costs nothing real. It matches
    /// [`crate::belief::PositionEstimate::Q_RAD`] for the same reason `Q_M` matches its
    /// twin.
    pub const Q_RAD: f64 = 1e-6;

    /// This state with every float on its declared grid (build decision D9).
    ///
    /// `gt.kinematics` is a recorded channel (03-interfaces.md §14), every UI keyframe and
    /// delta carries this struct, and the ground-truth labels of a dataset are built from
    /// it — so it goes through the writer-side quantiser exactly like its belief twin
    /// [`crate::belief::PositionEstimate::quantized`]. The truth channel escaping the
    /// quantiser while the belief channel did not is the asymmetry ADR 0004's own evidence
    /// (one field that escaped `round(x, 3)`) was written about.
    ///
    /// Apply it once, at the writer, and before any threshold comparison whose outcome is
    /// compared across engines (build decision D10). `t` is an integer and needs no grid.
    pub fn quantized(&self) -> Self {
        let q = |x: f64| crate::math::quantize_to(x, Self::Q_M);
        let qv = |v: Vec3| Vec3::new(q(v.x), q(v.y), q(v.z));
        let qa = |x: f64| crate::math::quantize_to(x, Self::Q_RAD);
        Self {
            t: self.t,
            pos: qv(self.pos),
            vel: qv(self.vel),
            acc: qv(self.acc),
            heading_rad: qa(self.heading_rad),
            yaw_rate_rad_s: qa(self.yaw_rate_rad_s),
            lane: self.lane.map(|l| l.quantized()),
            dims: self.dims.quantized(),
        }
    }

    /// A state at rest at `pos` at time `t`, facing east, with car dimensions. Intended
    /// for tests and for spawning; real states come from a mobility provider.
    pub fn at_rest(t: SimTime, pos: Vec3) -> Self {
        Self {
            t,
            pos,
            vel: Vec3::ZERO,
            acc: Vec3::ZERO,
            heading_rad: 0.0,
            yaw_rate_rad_s: 0.0,
            lane: None,
            dims: Dims::CAR,
        }
    }

    /// Speed, m/s (the 3-D magnitude of [`Kinematics::vel`]).
    pub fn speed_mps(&self) -> f64 {
        self.vel.norm()
    }

    /// Ground speed, m/s (the horizontal magnitude of [`Kinematics::vel`]).
    pub fn ground_speed_mps(&self) -> f64 {
        self.vel.norm_2d()
    }

    /// The **published extrapolation rule** between mobility steps.
    ///
    /// `pos(t′) = pos(t) + vel(t) · (t′ − t)` — constant velocity. Heading, yaw rate,
    /// velocity, acceleration, lane position and dimensions are carried over unchanged;
    /// only `pos` and `t` move. This is deliberately cruder than integrating `acc`: it
    /// is the rule published in 02-architecture.md §5.2, **it is part of the interface
    /// contract**, and every consumer that needs a position strictly between two
    /// mobility steps (frame-level radio events, perception, the UI) must produce the
    /// same value. Every model card that relies on it lists it as an assumption
    /// (ADR 0004 Consequences).
    ///
    /// Mobility runs at `Δt_mob` (default 100 ms, allowed 10–100 ms), so the rule is
    /// only ever applied over a window that short.
    ///
    /// Always extrapolate from the **published** state, never from an already
    /// extrapolated one: floating-point addition is not associative, so
    /// `k.extrapolate(t1).extrapolate(t2)` may differ in the last bits from
    /// `k.extrapolate(t2)`, and two consumers that chained differently would disagree
    /// about where an actor was.
    ///
    /// Extrapolating backwards is a contract violation — a consumer must never ask for a
    /// position before the last published step. Asking anyway returns the published state
    /// **unchanged, timestamp included**, rather than rewinding the actor along its
    /// velocity: `extrapolate(t) == extrapolate(max(t, self.t))`.
    ///
    /// Carrying the requested, earlier `t` on the result instead would produce a value
    /// that claims to be valid at `t` while holding a position from after `t`, and
    /// re-extrapolating it — which the warning above says consumers do — would then
    /// double-count the whole interval. With `vel = 13 m/s`,
    /// `k.extrapolate(k.t − 500 ms).extrapolate(k.t)` used to land 6.5 m ahead of the
    /// published position, silently.
    pub fn extrapolate(&self, t: SimTime) -> Kinematics {
        let t = t.max(self.t);
        let dt_s = ((t - self.t) as f64) / (NS_PER_S as f64);
        Kinematics {
            t,
            pos: self.pos + self.vel.scale(dt_s),
            ..*self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Dims;
    use crate::ids::LaneId;
    use crate::time::NS_PER_MS;

    fn sample() -> Kinematics {
        Kinematics {
            t: 1_000 * NS_PER_MS,
            pos: Vec3::new(10.0, 20.0, 0.5),
            vel: Vec3::new(13.0, -4.0, 0.0),
            acc: Vec3::new(1.5, 0.0, 0.0),
            heading_rad: 0.75,
            yaw_rate_rad_s: -0.02,
            lane: Some(LanePos::new(LaneId::new(3), 42.0, 0.1)),
            dims: Dims::CAR,
        }
    }

    #[test]
    fn extrapolation_is_constant_velocity() {
        let k = sample();
        let out = k.extrapolate(k.t + 100 * NS_PER_MS);
        assert_eq!(out.t, k.t + 100 * NS_PER_MS);
        assert_eq!(out.pos, Vec3::new(10.0 + 1.3, 20.0 - 0.4, 0.5));
        // Acceleration is deliberately NOT integrated.
        assert_eq!(out.vel, k.vel);
        assert_eq!(out.acc, k.acc);
        assert_eq!(out.heading_rad, k.heading_rad);
        assert_eq!(out.yaw_rate_rad_s, k.yaw_rate_rad_s);
        assert_eq!(out.lane, k.lane);
        assert_eq!(out.dims, k.dims);
    }

    #[test]
    fn extrapolation_is_exact_at_the_step_itself() {
        let k = sample();
        assert_eq!(k.extrapolate(k.t), k);
    }

    /// A backwards request returns the published state whole — position *and* timestamp —
    /// so the result is internally consistent and safe to extrapolate forward from again.
    #[test]
    fn extrapolation_never_rewinds() {
        let k = sample();
        let back = k.extrapolate(k.t - 500 * NS_PER_MS);
        assert_eq!(back.pos, k.pos);
        assert_eq!(
            back.t, k.t,
            "the result must carry the state's own validity time"
        );
        assert_eq!(back, k);

        // Re-extrapolating a clamped value forward must not double-count the interval.
        assert_eq!(back.extrapolate(k.t).pos, k.pos);
        assert_eq!(
            back.extrapolate(k.t + 100 * NS_PER_MS),
            k.extrapolate(k.t + 100 * NS_PER_MS)
        );
    }

    /// Consumers must extrapolate from the published state, never from an already
    /// extrapolated one: floating-point addition is not associative, so chaining can
    /// differ in the last bits, and two consumers that chain differently would disagree.
    #[test]
    fn chaining_is_not_the_same_as_extrapolating_once() {
        let k = sample();
        let once = k.extrapolate(k.t + 100 * NS_PER_MS);
        let chained = k
            .extrapolate(k.t + 40 * NS_PER_MS)
            .extrapolate(k.t + 100 * NS_PER_MS);
        assert!((once.pos.x - chained.pos.x).abs() < 1e-9);
        assert_ne!(
            once.pos.x, chained.pos.x,
            "if this ever becomes exactly equal the warning in the docs can be relaxed"
        );
        // Extrapolating twice from the published state is exactly reproducible.
        assert_eq!(k.extrapolate(k.t + 100 * NS_PER_MS), once);
    }

    /// `gt.kinematics` is a recorded channel and the payload of every UI keyframe, so the
    /// truth channel quantises exactly like its belief twin
    /// [`crate::belief::PositionEstimate`]. It used to be the one of the pair that did not
    /// — the asymmetry ADR 0004's own evidence was written about.
    #[test]
    fn the_truth_channel_quantises_like_the_belief_channel() {
        use crate::math;

        let k = Kinematics {
            t: 1_000 * NS_PER_MS,
            pos: Vec3::new(10.000_499_9, 20.000_6, -0.123_456_7),
            vel: Vec3::new(3.141_492_6, -4.000_04, 0.000_49),
            acc: Vec3::new(1.500_499_9, -0.000_6, 0.0),
            heading_rad: 0.750_000_499_9,
            yaw_rate_rad_s: -0.020_000_000_5,
            lane: Some(LanePos::new(LaneId::new(3), 42.000_499_9, 0.100_6)),
            dims: Dims::new(4.500_499_9, 1.800_06, 1.500_000_1),
        };
        let q = k.quantized();

        for x in [
            q.pos.x,
            q.pos.y,
            q.pos.z,
            q.vel.x,
            q.vel.y,
            q.vel.z,
            q.acc.x,
            q.acc.y,
            q.acc.z,
            q.lane.unwrap().s_m,
            q.lane.unwrap().d_m,
            q.dims.length_m,
            q.dims.width_m,
            q.dims.height_m,
        ] {
            assert!(math::is_on_grid(x, Kinematics::Q_M), "{x} is off grid");
        }
        for a in [q.heading_rad, q.yaw_rate_rad_s] {
            assert!(math::is_on_grid(a, Kinematics::Q_RAD), "{a} is off grid");
        }

        // The raw values really were off their grids, so the assertions are not vacuous.
        assert!(!math::is_on_grid(k.pos.z, Kinematics::Q_M));
        assert!(!math::is_on_grid(k.dims.height_m, Kinematics::Q_M));
        assert!(!math::is_on_grid(k.yaw_rate_rad_s, Kinematics::Q_RAD));

        assert_eq!(q.pos.x, 10.0);
        assert_eq!(q.pos.y, 20.001);
        assert_eq!(q.lane.unwrap().s_m, 42.0);
        assert_eq!(q.lane.unwrap().d_m, 0.101);
        assert_eq!(q.dims, Dims::new(4.5, 1.8, 1.5));
        assert_eq!(q.lane.unwrap().lane, LaneId::new(3), "the id is untouched");
        assert_eq!(q.t, k.t, "an integer needs no grid");
        assert_eq!(q.quantized(), q, "idempotent, as every quantiser must be");

        // The grids are the same ones the belief twin declares, so a recorded truth and a
        // recorded belief are comparable field by field.
        assert_eq!(Kinematics::Q_M, crate::belief::PositionEstimate::Q_M);
        assert_eq!(Kinematics::Q_RAD, crate::belief::PositionEstimate::Q_RAD);

        // An actor off the lane graph quantises too.
        let off_graph = Kinematics { lane: None, ..k };
        assert_eq!(off_graph.quantized().lane, None);

        // And the components carry their own grid, for a writer that has only one of them.
        assert_eq!(LanePos::Q_M, Kinematics::Q_M);
        assert_eq!(Dims::Q_M, Kinematics::Q_M);
        assert_eq!(
            LanePos::new(LaneId::new(1), 1.000_499_9, -0.000_6).quantized(),
            LanePos::new(LaneId::new(1), 1.0, -0.001)
        );
        assert_eq!(
            Dims::new(0.500_4, 0.499_6, 1.700_000_4).quantized(),
            Dims::new(0.5, 0.5, 1.7)
        );

        let back: Kinematics = serde_json::from_str(&serde_json::to_string(&q).unwrap()).unwrap();
        assert_eq!(back, q);
    }

    #[test]
    fn speeds_and_serde() {
        let k = Kinematics {
            vel: Vec3::new(3.0, 4.0, 12.0),
            ..sample()
        };
        assert_eq!(k.speed_mps(), 13.0);
        assert_eq!(k.ground_speed_mps(), 5.0);
        let s = serde_json::to_string(&k).unwrap();
        assert_eq!(serde_json::from_str::<Kinematics>(&s).unwrap(), k);
        assert_eq!(Kinematics::at_rest(0, Vec3::ZERO).speed_mps(), 0.0);
    }
}
