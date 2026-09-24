//! Drivable path geometry: rounding a polyline's corners into circular arcs.
//!
//! # Why
//!
//! A road vehicle cannot turn on the spot. Its path has a smallest radius, set by its
//! wheelbase and steering lock: AASHTO's *A Policy on Geometric Design of Highways and
//! Streets* (the Green Book, 7th ed., 2018), Table 2-2, gives each design vehicle's minimum
//! centreline turning radius — 6.4 m (21 ft) for the passenger car P, 11.6 m (38 ft) for the
//! single-unit truck SU-9 (SU-30), 11.5 m for the CITY-BUS. A lane centreline with a vertex
//! in it — an OpenStreetMap way is a polyline, and a junction connector used to be a
//! seven-point Bézier whose curvature piled up at its shorter end — asks the vehicle on it
//! to turn through that vertex's whole angle in no distance at all. The traffic auditor
//! measured exactly that on Manhattan: 79 vehicle-steps turning faster than a 4 m radius
//! allows, every one on a junction connector or a lane vertex.
//!
//! # What this does
//!
//! [`fillet_polyline`] replaces each interior vertex by the **simple curve** of the Green
//! Book (§9.5, "Minimum edge-of-traveled-way designs", which designs turning roadways as
//! simple curves or three-centred compounds): a circular arc tangent to both segments,
//! of the largest radius up to a cap that fits in the tangent length the two segments can
//! give. A segment shared by two rounded vertices gives each half of itself; a segment
//! that ends at the polyline's first or last point gives its whole length, since nothing
//! else claims it. The endpoints and the directions of the first and last segments are
//! untouched, so a lane still starts and ends where its junction expects it, pointing the
//! way it did — which is what keeps the heading continuous across every lane-to-connector
//! join. The result is tangent-continuous (G1) everywhere; its curvature steps at each
//! tangent point, as a simple curve's does.
//!
//! Arcs are sampled every [`MAX_ARC_STEP_RAD`] of heading, so the chord of each sample
//! lies within `R · (1 − cos(step/2))` of the true arc — 3 mm at a 6.4 m radius.
//!
//! Arithmetic plus [`v2xw_core::math`], so the result is bit-identical on every platform.

use v2xw_core::geom::Vec3;
use v2xw_core::math;

use crate::model::normalise_angle;

/// The largest heading change between two samples of an arc, radians: 6°.
pub const MAX_ARC_STEP_RAD: f64 = core::f64::consts::PI / 30.0;

/// A vertex turning by less than this is left as it is, radians.
const STRAIGHT_RAD: f64 = 1e-4;

/// The shortest tangent length worth rounding, metres, and the shortest chord an arc is
/// sampled at.
///
/// On the millimetre position grid a segment `L` long has a direction good to about
/// `1 mm / L`: 0.6° at 10 cm, and tens of degrees at the centimetre an unguarded arc of a
/// nearly straight vertex was sampled at. A vertex that cannot be rounded over at least
/// this much turns so little, or has so little room, that rounding it would only add
/// noise.
const MIN_TANGENT_M: f64 = 0.1;

/// The smallest radius the arc at each interior vertex of `points` has, metres, when the
/// polyline is rounded by [`fillet_polyline`] with no cap: `(tangent length) / tan(δ/2)`
/// with the tangent length that vertex is given. `f64::INFINITY` for a straight polyline.
pub fn tightest_fillet_radius(points: &[Vec3]) -> f64 {
    let mut best = f64::INFINITY;
    for (_, d, t) in vertex_fillets(points, f64::INFINITY) {
        if t > 0.0 {
            best = best.min(d / t);
        }
    }
    best
}

/// For each interior vertex: `(index, tangent length, tan(|δ|/2))`, with the tangent
/// length already capped by `r_max`. A vertex left sharp has tangent length zero.
fn vertex_fillets(points: &[Vec3], r_max: f64) -> Vec<(usize, f64, f64)> {
    let n = points.len();
    let mut out = Vec::new();
    if n < 3 {
        return out;
    }
    let len: Vec<f64> = points.windows(2).map(|w| w[0].distance_2d(w[1])).collect();
    for i in 1..n - 1 {
        let h_in = heading(points[i - 1], points[i]);
        let h_out = heading(points[i], points[i + 1]);
        let turn = normalise_angle(h_out - h_in).abs();
        if turn < STRAIGHT_RAD || len[i - 1] <= 0.0 || len[i] <= 0.0 {
            out.push((i, 0.0, 0.0));
            continue;
        }
        let avail_in = if i == 1 { len[0] } else { 0.5 * len[i - 1] };
        let avail_out = if i + 2 == n { len[i] } else { 0.5 * len[i] };
        let t = math::tan(0.5 * turn);
        let room = avail_in.min(avail_out);
        let mut d = (r_max * t).min(room);
        // A straight remainder shorter than the sampling floor is taken into the arc,
        // rather than left as a sliver whose direction is grid noise.
        if room - d < MIN_TANGENT_M {
            d = room;
        }
        out.push((i, if d < MIN_TANGENT_M { 0.0 } else { d }, t));
    }
    out
}

fn heading(a: Vec3, b: Vec3) -> f64 {
    math::atan2(b.y - a.y, b.x - a.x)
}

/// `points` with every interior vertex rounded into a circular arc of radius at most
/// `r_max` metres (see the module documentation).
///
/// The first and last points are kept exactly, and so are the directions of the first and
/// last segments. `z` is interpolated linearly along each arc.
pub fn fillet_polyline(points: &[Vec3], r_max: f64) -> Vec<Vec3> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    let fillets = vertex_fillets(points, r_max);
    let mut out: Vec<Vec3> = Vec::with_capacity(n * 4);
    out.push(points[0]);
    for (i, d, t) in fillets {
        let (a, b, c) = (points[i - 1], points[i], points[i + 1]);
        if d <= 0.0 {
            push_distinct(&mut out, b);
            continue;
        }
        let h_in = heading(a, b);
        let h_out = heading(b, c);
        let turn = normalise_angle(h_out - h_in);
        let r = d / t;
        let (s_in, c_in) = math::sin_cos(h_in);
        let len_in = a.distance_2d(b);
        let len_out = b.distance_2d(c);
        let p_in = b.lerp(a, d / len_in);
        let p_out = b.lerp(c, d / len_out);
        let side = if turn > 0.0 { 1.0 } else { -1.0 };
        // The centre is `r` to the left of the entry tangent for a left turn, to the right
        // for a right one.
        let centre = (p_in.x - side * r * s_in, p_in.y + side * r * c_in);
        let arc_m = r * turn.abs();
        let steps = ((turn.abs() / MAX_ARC_STEP_RAD).ceil() as usize)
            .min((arc_m / MIN_TANGENT_M).floor() as usize)
            .max(1);
        push_distinct(&mut out, p_in);
        for k in 1..steps {
            let f = k as f64 / steps as f64;
            let h = h_in + turn * f;
            let (s, c) = math::sin_cos(h);
            let z = p_in.z + (p_out.z - p_in.z) * f;
            push_distinct(
                &mut out,
                Vec3::new(centre.0 + side * r * s, centre.1 - side * r * c, z),
            );
        }
        push_distinct(&mut out, p_out);
    }
    push_distinct(&mut out, points[n - 1]);
    // The last point is the polyline's own, exactly.
    if let Some(last) = out.last_mut() {
        *last = points[n - 1];
    }
    out
}

/// Appends `p` unless it is within a millimetre of the last point.
fn push_distinct(out: &mut Vec<Vec3>, p: Vec3) {
    if out.last().is_some_and(|q| q.distance_2d(p) < 2e-3) {
        return;
    }
    out.push(p);
}

/// Drops interior vertices that leave a segment shorter than `min_m` metres, keeping both
/// endpoints.
///
/// A trimmed or offset lane can end in a segment a few millimetres long, and on the
/// millimetre position grid such a segment's direction is noise: the lane's end heading —
/// which its junction connectors are built tangent to — came out up to tens of degrees
/// off the road's. Merging it into its neighbour gives the end a direction the road
/// actually has.
pub fn drop_short_segments(points: &[Vec3], min_m: f64) -> Vec<Vec3> {
    let n = points.len();
    if n <= 2 {
        return points.to_vec();
    }
    let mut out: Vec<Vec3> = vec![points[0]];
    for p in &points[1..n - 1] {
        if out.last().is_some_and(|q| q.distance_2d(*p) >= min_m) {
            out.push(*p);
        }
    }
    let last = points[n - 1];
    while out.len() >= 2 && out.last().is_some_and(|q| q.distance_2d(last) < min_m) {
        out.pop();
    }
    out.push(last);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Vec3 {
        Vec3::new(x, y, 0.0)
    }

    /// The largest heading change per metre along a polyline's samples.
    fn max_curvature(points: &[Vec3]) -> f64 {
        let mut worst: f64 = 0.0;
        for w in points.windows(3) {
            let turn = normalise_angle(heading(w[1], w[2]) - heading(w[0], w[1])).abs();
            let l = 0.5 * (w[0].distance_2d(w[1]) + w[1].distance_2d(w[2]));
            worst = worst.max(turn / l);
        }
        worst
    }

    #[test]
    fn a_right_angle_becomes_a_quarter_circle_of_the_room_it_has() {
        // 10 m in, 6 m out: the tangent length is the shorter leg, so R = 6 m.
        let out = fillet_polyline(&[p(-10.0, 0.0), p(0.0, 0.0), p(0.0, -6.0)], f64::INFINITY);
        assert_eq!(out.first().copied(), Some(p(-10.0, 0.0)));
        assert_eq!(out.last().copied(), Some(p(0.0, -6.0)));
        // Every arc sample is 6 m from the centre (-6, -6).
        for q in &out[1..out.len() - 1] {
            let r = ((q.x + 6.0).powi(2) + (q.y + 6.0).powi(2)).sqrt();
            assert!(
                (r - 6.0).abs() < 1e-9,
                "sample {q:?} is {r} m from the centre"
            );
        }
        // The discrete curvature is that of a 6 m circle, not the vertex's infinite one.
        assert!(max_curvature(&out) < 1.0 / 5.9, "{}", max_curvature(&out));
        assert!(
            (tightest_fillet_radius(&[p(-10.0, 0.0), p(0.0, 0.0), p(0.0, -6.0)]) - 6.0).abs()
                < 1e-9
        );
    }

    #[test]
    fn the_end_directions_and_points_are_kept() {
        let pts = [p(0.0, 0.0), p(10.0, 0.0), p(15.0, 5.0), p(15.0, 20.0)];
        let out = fillet_polyline(&pts, 50.0);
        assert_eq!(out[0], pts[0]);
        assert_eq!(*out.last().unwrap(), pts[3]);
        assert!(normalise_angle(heading(out[0], out[1])).abs() < 1e-12);
        let n = out.len();
        let h = heading(out[n - 2], out[n - 1]);
        assert!((h - core::f64::consts::FRAC_PI_2).abs() < 1e-9, "{h}");
    }

    #[test]
    fn a_cap_limits_the_radius_and_a_straight_line_is_untouched() {
        let straight = [p(0.0, 0.0), p(5.0, 0.0), p(10.0, 0.0)];
        assert_eq!(fillet_polyline(&straight, 10.0).len(), 3);
        let out = fillet_polyline(&[p(-100.0, 0.0), p(0.0, 0.0), p(0.0, 100.0)], 8.0);
        assert!(max_curvature(&out) < 1.0 / 7.9 && max_curvature(&out) > 1.0 / 8.1);
    }

    #[test]
    fn a_noisy_last_millimetres_segment_is_merged() {
        let pts = [p(0.0, 0.0), p(10.0, 0.0), p(10.003, 0.002)];
        let out = drop_short_segments(&pts, 0.5);
        assert_eq!(out, vec![p(0.0, 0.0), p(10.003, 0.002)]);
        let pts = [p(0.0, 0.0), p(0.001, 0.003), p(10.0, 0.0)];
        assert_eq!(
            drop_short_segments(&pts, 0.5),
            vec![p(0.0, 0.0), p(10.0, 0.0)]
        );
    }
}
