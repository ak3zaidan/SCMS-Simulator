//! Conflict zones: where two movements' paths through a junction meet.
//!
//! A junction's [`v2xw_world::ConflictMatrix`] says *which* movements conflict; this says
//! *where*. For every pair of conflicting internal connectors that start on different
//! approach lanes, the points at which their centrelines cross (or, for a merge, the point
//! at which they end together) are found once, as an arc length along each path, together
//! with how long a stretch of each path the other path's corridor covers there.
//!
//! Two consumers read it:
//!
//! * the engine's entry rule, which holds a vehicle at the stop line while a vehicle
//!   already inside the junction on a conflicting movement has not yet cleared the zone
//!   the two paths share — the "yield to other vehicles lawfully within the intersection"
//!   of the Uniform Vehicle Code §11-202(a)1 and NY VTL §1111(a)1, which is the rule a
//!   green light is conditional on;
//! * the traffic-invariant auditor ([`crate::audit`]), which counts two vehicles inside
//!   one zone at the same instant.
//!
//! Movements from one approach lane are never a pair here: they diverge from a shared
//! start, which is a queue on one lane, not a crossing.

use std::collections::BTreeMap;

use v2xw_core::geom::Vec3;
use v2xw_core::ids::LaneId;
use v2xw_core::math;
use v2xw_world::World;

/// One place where the path of movement `a` meets that of movement `b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zone {
    /// The other movement's internal lane.
    pub other: LaneId,
    /// Arc length along this movement's path at the meeting point, metres.
    pub s_self: f64,
    /// Arc length along the other movement's path at the meeting point, metres.
    pub s_other: f64,
    /// Half the stretch of this path the other path's corridor covers, metres.
    pub half_self: f64,
    /// Half the stretch of the other path this path's corridor covers, metres.
    pub half_other: f64,
    /// True if the two paths meet where both end — a merge onto one exit lane, which is
    /// driven through in order rather than waited for.
    pub merge: bool,
}

impl Zone {
    /// True if a body occupying `[rear, front]` of arc length along this movement's path
    /// overlaps the zone.
    pub fn holds_self(&self, rear: f64, front: f64) -> bool {
        front >= self.s_self - self.half_self && rear <= self.s_self + self.half_self
    }

    /// True if a body occupying `[rear, front]` along the *other* path overlaps it.
    pub fn holds_other(&self, rear: f64, front: f64) -> bool {
        front >= self.s_other - self.half_other && rear <= self.s_other + self.half_other
    }

    /// True if a body on the other path, rear at `rear`, has not yet driven clear of the
    /// zone — it is in it, or still to reach it.
    pub fn other_not_clear(&self, rear: f64) -> bool {
        rear <= self.s_other + self.half_other
    }
}

/// Every junction's conflict zones, keyed by internal lane.
#[derive(Debug, Clone, Default)]
pub struct ConflictZones {
    zones: BTreeMap<LaneId, Vec<Zone>>,
    approach_of: BTreeMap<LaneId, LaneId>,
    exit_of: BTreeMap<LaneId, LaneId>,
}

/// The smallest `|sin θ|` a crossing angle is taken at when the corridor's footprint along
/// a path is computed: below 30° the footprint `w / sin θ` would grow without bound for
/// two nearly parallel paths, and a merge is capped at twice the lane width instead.
const MIN_SIN_CROSSING: f64 = 0.5;

impl ConflictZones {
    /// Finds every junction's zones.
    pub fn build(world: &World) -> Self {
        let mut approach_of = BTreeMap::new();
        let mut exit_of = BTreeMap::new();
        for c in world.roads.connections() {
            if let Some(via) = c.via {
                approach_of.entry(via).or_insert(c.from_lane);
                exit_of.entry(via).or_insert(c.to_lane);
            }
        }
        let mut zones: BTreeMap<LaneId, Vec<Zone>> = BTreeMap::new();
        for j in world.roads.junctions() {
            for (ra, la) in j.internal.iter().enumerate() {
                for (rb, lb) in j.internal.iter().enumerate() {
                    if ra == rb || !j.conflicts.is_foe(ra, rb) {
                        continue;
                    }
                    if approach_of.get(la).is_some() && approach_of.get(la) == approach_of.get(lb)
                    {
                        continue;
                    }
                    let (a, b) = (world.lane(*la), world.lane(*lb));
                    let mut hits = polyline_crossings(
                        &a.centreline,
                        &a.cumulative,
                        &b.centreline,
                        &b.cumulative,
                    );
                    // Two paths that end on one point merge there.
                    if hits.is_empty() && a.end().distance_2d(b.end()) < 0.5 {
                        hits.push((a.length_m, b.length_m));
                    }
                    for (s_a, s_b) in hits {
                        let merge = s_a >= a.length_m - 0.5 && s_b >= b.length_m - 0.5;
                        let angle = v2xw_world::model::normalise_angle(
                            a.heading_at(s_a) - b.heading_at(s_b),
                        );
                        let sin = math::sin(angle).abs().max(MIN_SIN_CROSSING);
                        zones.entry(*la).or_default().push(Zone {
                            other: *lb,
                            s_self: s_a,
                            s_other: s_b,
                            half_self: 0.5 * b.width_m / sin,
                            half_other: 0.5 * a.width_m / sin,
                            merge,
                        });
                    }
                }
            }
        }
        for list in zones.values_mut() {
            list.sort_by(|x, y| x.other.cmp(&y.other).then(x.s_self.total_cmp(&y.s_self)));
        }
        Self {
            zones,
            approach_of,
            exit_of,
        }
    }

    /// The zones on `internal`'s path, against every conflicting movement.
    pub fn of(&self, internal: LaneId) -> &[Zone] {
        self.zones.get(&internal).map_or(&[][..], Vec::as_slice)
    }

    /// The approach lane an internal connector starts from.
    pub fn approach_of(&self, internal: LaneId) -> Option<LaneId> {
        self.approach_of.get(&internal).copied()
    }

    /// The departure lane an internal connector ends on.
    pub fn exit_of(&self, internal: LaneId) -> Option<LaneId> {
        self.exit_of.get(&internal).copied()
    }

    /// How many zones in total, for a report.
    pub fn len(&self) -> usize {
        self.zones.values().map(Vec::len).sum()
    }

    /// True if there are none.
    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }
}

/// Where two polylines intersect, as arc lengths along each. Segment endpoints count, so
/// two paths that touch are found; duplicate hits at a shared vertex are merged.
pub fn polyline_crossings(a: &[Vec3], ca: &[f64], b: &[Vec3], cb: &[f64]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for i in 0..a.len().saturating_sub(1) {
        let (p, p2) = (a[i], a[i + 1]);
        let r = (p2.x - p.x, p2.y - p.y);
        for k in 0..b.len().saturating_sub(1) {
            let (q, q2) = (b[k], b[k + 1]);
            let s = (q2.x - q.x, q2.y - q.y);
            let denom = r.0 * s.1 - r.1 * s.0;
            if denom.abs() < 1e-12 {
                continue;
            }
            let qp = (q.x - p.x, q.y - p.y);
            let t = (qp.0 * s.1 - qp.1 * s.0) / denom;
            let u = (qp.0 * r.1 - qp.1 * r.0) / denom;
            if (-1e-9..=1.0 + 1e-9).contains(&t) && (-1e-9..=1.0 + 1e-9).contains(&u) {
                let hit = (
                    ca[i] + t * (ca[i + 1] - ca[i]),
                    cb[k] + u * (cb[k + 1] - cb[k]),
                );
                if !out
                    .iter()
                    .any(|h| (h.0 - hit.0).abs() < 0.05 && (h.1 - hit.1).abs() < 0.05)
                {
                    out.push(hit);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_world::{ImportOptions, procedural::GridParams};

    #[test]
    fn a_crossroads_has_zones_and_they_are_symmetric() {
        let world = v2xw_world::procedural::grid(
            &GridParams::legacy().with_signals(true),
            &ImportOptions::default(),
        )
        .expect("grid");
        let zones = ConflictZones::build(&world);
        assert!(!zones.is_empty());
        // Every zone of a on b has its mirror on b's list.
        for j in world.roads.junctions() {
            for la in &j.internal {
                for z in zones.of(*la) {
                    let mirror = zones.of(z.other).iter().any(|m| {
                        m.other == *la
                            && (m.s_self - z.s_other).abs() < 1e-6
                            && (m.s_other - z.s_self).abs() < 1e-6
                    });
                    assert!(mirror, "zone {la:?}/{:?} has no mirror", z.other);
                }
            }
        }
    }

    #[test]
    fn two_crossing_segments_meet_where_they_cross() {
        let a = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0)];
        let b = [Vec3::new(4.0, -5.0, 0.0), Vec3::new(4.0, 5.0, 0.0)];
        let hits = polyline_crossings(&a, &[0.0, 10.0], &b, &[0.0, 10.0]);
        assert_eq!(hits.len(), 1);
        assert!((hits[0].0 - 4.0).abs() < 1e-9 && (hits[0].1 - 5.0).abs() < 1e-9);
        let far = [Vec3::new(20.0, -5.0, 0.0), Vec3::new(20.0, 5.0, 0.0)];
        assert!(polyline_crossings(&a, &[0.0, 10.0], &far, &[0.0, 10.0]).is_empty());
    }
}
