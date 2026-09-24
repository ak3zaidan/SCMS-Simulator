//! Pedestrian infrastructure: crosswalks, where they cut the vehicle lanes, and the
//! pedestrian signal intervals of a signalised junction.
//!
//! # Crosswalks
//!
//! A crosswalk is carried as one or more [`LaneKind::Crossing`] lanes — one per walking
//! direction — so a pedestrian routes over it like any other walkable lane. [`crosswalks`]
//! groups the lanes that are the same painted crosswalk (the two directions of one
//! `footway=crossing` way, or of one generated crossing), and [`crosswalk_conflicts`] finds
//! where each crosswalk cuts a lane a vehicle or a cyclist drives: the stretch of that
//! lane the crosswalk band covers. The mobility model yields at those stretches and the
//! traffic auditor checks them, and both read this one geometry.
//!
//! # Pedestrian signal intervals (MUTCD 2009 §4E.06)
//!
//! A signalised crosswalk shows WALKING PERSON (walk), flashing UPRAISED HAND (the
//! pedestrian change interval, "flashing don't walk") and steady UPRAISED HAND (don't
//! walk). [`signalise_crossings`] adds these to a junction's plan **alongside** the vehicle
//! phases, without changing any vehicle state:
//!
//! * The crosswalk may show walk only while no vehicle movement that cuts it has a
//!   protected indication: a straight-through movement on anything but red, or a left
//!   turn on a protected (non-yielding) green. Turning movements on a permissive green
//!   keep their green and **yield** to pedestrians in the crosswalk — UVC §11-202(a)1
//!   ("shall yield the right of way ... to pedestrians lawfully within ... an adjacent
//!   crosswalk"), which the mobility model enforces — so they do not stop the walk. This is
//!   the concurrent pedestrian phasing of MUTCD §4E.06 and of `netconvert`'s generated
//!   crossings.
//! * Walk starts with the first green inside that window (not during an all-red that ends
//!   the conflicting phase).
//! * The pedestrian clearance time is the crossing length at **3.5 ft/s (1.07 m/s)**,
//!   MUTCD 2009 §4E.06 ¶07. The crossing length is the crosswalk lane's own length, kerb
//!   to kerb (a little longer than the travelled way, so conservative).
//! * The pedestrian change interval is followed by a **buffer interval of at least 3 s**
//!   of steady don't-walk before any conflicting vehicle is released (MUTCD 2009 §4E.06
//!   ¶¶13-15); it may overlap the vehicle change and clearance intervals, which is where it
//!   falls here.
//! * The walk interval should be at least **7 s**, and may be as short as **4 s** where
//!   pedestrian volumes and characteristics do not need 7 s (MUTCD 2009 §4E.06 ¶¶04-05).
//!   The vehicle split is not this module's to change, so a window too short for both is
//!   *reported* rather than hidden: a walk between 4 s and 7 s is counted as
//!   [`PedestrianSignalReport::short_walk`], and a window that cannot hold 4 s of walk and
//!   the full clearance is given 4 s of walk with a clearance shorter than the MUTCD value
//!   and counted as [`PedestrianSignalReport::short_clearance`].
//!
//! The pedestrian indications are encoded in the plan's own state vector: walk is
//! [`SignalState::Green`], the pedestrian change interval is [`SignalState::Amber`] and
//! don't-walk is [`SignalState::Red`]. That is also how SAE J2735 carries a pedestrian
//! signal group in SPaT (walk = protected-Movement-Allowed, flashing don't walk =
//! protected-clearance, don't walk = stop-And-Remain), so a SPaT encoder that maps a
//! vehicle movement's states maps a crosswalk's correctly with no special case. Each
//! crosswalk gets its own signal group, numbered from [`PEDESTRIAN_GROUP_BASE`], and a
//! [`SignalHeadKind::Pedestrian`] head at each end.
//!
//! **Paragraph numbers.** The ¶ numbers above are from the 2009 edition as revised in
//! 2012; the values (3.5 ft/s, 7 s, 4 s, 3 s) are the ones the text states, and the card
//! records the edition. The 2023 11th edition keeps the same values.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{JunctionId, LaneId};
use v2xw_core::math;

use crate::model::{
    ClassMask, Connection, Lane, LaneKind, SignalHead, SignalHeadKind, SignalPhase, SignalPlan,
    SignalState, TurnDirection,
};
use crate::quant::{Q_TIME_S, quantise};

/// The walking speed the pedestrian clearance time is computed at, m/s: 3.5 ft/s
/// (MUTCD 2009 §4E.06 ¶07).
pub const MUTCD_WALKING_SPEED_MPS: f64 = 1.0668;

/// The minimum walk interval, seconds (MUTCD 2009 §4E.06 ¶04).
pub const MUTCD_MIN_WALK_S: f64 = 7.0;

/// The reduced minimum walk interval, seconds (MUTCD 2009 §4E.06 ¶05).
pub const MUTCD_REDUCED_WALK_S: f64 = 4.0;

/// The minimum buffer interval of steady don't-walk after the pedestrian change interval,
/// seconds (MUTCD 2009 §4E.06).
pub const MUTCD_BUFFER_S: f64 = 3.0;

/// The height of a pedestrian signal head above the walkway, metres: MUTCD 2009 §4E.08
/// puts the bottom of the housing 7 to 10 ft (2.1 to 3.0 m) above the sidewalk; the middle
/// of that range.
pub const PEDESTRIAN_HEAD_HEIGHT_M: f64 = 2.5;

/// The first signal-group number a crosswalk gets. Vehicle groups are small integers
/// (the phase group of the approach), so a crosswalk's group never collides with one.
pub const PEDESTRIAN_GROUP_BASE: u16 = 100;

/// How the pedestrian intervals are timed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PedestrianTiming {
    /// The walking speed of the clearance time, m/s.
    pub walking_speed_mps: f64,
    /// The walk interval below which a walk is reported as short, seconds.
    pub min_walk_s: f64,
    /// The shortest walk interval ever given, seconds.
    pub reduced_walk_s: f64,
    /// The buffer interval, seconds.
    pub buffer_s: f64,
    /// How far along an approach or a departure lane a crosswalk still belongs to the
    /// junction, metres. Not a MUTCD value: the reach within which the importer looks.
    pub reach_m: f64,
    /// The height of a pedestrian head above the walkway, metres.
    pub head_height_m: f64,
}

impl Default for PedestrianTiming {
    fn default() -> Self {
        PedestrianTiming::mutcd()
    }
}

impl PedestrianTiming {
    /// The MUTCD 2009 values.
    pub const fn mutcd() -> Self {
        Self {
            walking_speed_mps: MUTCD_WALKING_SPEED_MPS,
            min_walk_s: MUTCD_MIN_WALK_S,
            reduced_walk_s: MUTCD_REDUCED_WALK_S,
            buffer_s: MUTCD_BUFFER_S,
            reach_m: 30.0,
            head_height_m: PEDESTRIAN_HEAD_HEIGHT_M,
        }
    }

    /// The pedestrian clearance time for a crossing `length_m` long, seconds.
    pub fn clearance_s(&self, length_m: f64) -> f64 {
        length_m / self.walking_speed_mps.max(0.1)
    }
}

/// One painted crosswalk: the crossing lanes that are its walking directions.
#[derive(Debug, Clone, PartialEq)]
pub struct Crosswalk {
    /// The crossing lanes, ascending. The first is the representative whose centreline
    /// the geometry is measured on.
    pub lanes: Vec<LaneId>,
    /// The band's width across the direction of walking, metres (the widest lane's).
    pub width_m: f64,
    /// Kerb-to-kerb length, metres (the longest lane's).
    pub length_m: f64,
    /// The band's midpoint.
    pub midpoint: Vec3,
    /// How far the representative lane's centreline is from the band's centre line,
    /// metres (zero when every direction shares one line).
    pub rep_offset_m: f64,
}

/// Groups every [`LaneKind::Crossing`] lane into crosswalks.
///
/// Two crossing lanes are one crosswalk when their midpoints are closer than half their
/// summed widths plus half a metre and they run along the same line (parallel or
/// anti-parallel to within about 25°). Lanes are taken in id order and each joins the
/// first crosswalk it matches, so the grouping is deterministic.
///
/// The band is as wide as the lanes it holds, side by side: an importer that lays the two
/// walking directions of a crossing way half a band apart gets the whole band, and a
/// generator that puts both on one line gets one lane's width.
pub fn crosswalks(lanes: &[Lane]) -> Vec<Crosswalk> {
    // (representative midpoint, its direction, [(lateral offset, width)]) per group.
    struct Group {
        lanes: Vec<LaneId>,
        rep_mid: Vec3,
        dir: (f64, f64),
        spread: Vec<(f64, f64)>,
        length_m: f64,
    }
    let mut groups: Vec<Group> = Vec::new();
    for lane in lanes.iter().filter(|l| l.kind == LaneKind::Crossing) {
        let mid = lane.point_at(0.5 * lane.length_m);
        let d = direction_of(lane);
        let found = groups.iter().position(|g| {
            let cos = (g.dir.0 * d.0 + g.dir.1 * d.1).abs();
            let reach = g.spread.iter().map(|(_, w)| *w).fold(0.0, f64::max);
            cos >= 0.9 && g.rep_mid.distance_2d(mid) <= 0.5 * (reach + lane.width_m) + 0.5
        });
        match found {
            Some(k) => {
                let g = &mut groups[k];
                // Signed offset across the representative's direction.
                let off = (mid.x - g.rep_mid.x) * -g.dir.1 + (mid.y - g.rep_mid.y) * g.dir.0;
                g.lanes.push(lane.id);
                g.spread.push((off, lane.width_m));
                g.length_m = g.length_m.max(lane.length_m);
            }
            None => groups.push(Group {
                lanes: vec![lane.id],
                rep_mid: mid,
                dir: d,
                spread: vec![(0.0, lane.width_m)],
                length_m: lane.length_m,
            }),
        }
    }
    groups
        .into_iter()
        .map(|g| {
            let lo = g
                .spread
                .iter()
                .map(|(o, w)| o - 0.5 * w)
                .fold(f64::INFINITY, f64::min);
            let hi = g
                .spread
                .iter()
                .map(|(o, w)| o + 0.5 * w)
                .fold(f64::NEG_INFINITY, f64::max);
            let centre = 0.5 * (lo + hi);
            Crosswalk {
                lanes: g.lanes,
                width_m: hi - lo,
                length_m: g.length_m,
                rep_offset_m: centre.abs(),
                midpoint: Vec3::new_2d(
                    g.rep_mid.x - g.dir.1 * centre,
                    g.rep_mid.y + g.dir.0 * centre,
                ),
            }
        })
        .collect()
}

/// The unit vector from a lane's start to its end.
fn direction_of(lane: &Lane) -> (f64, f64) {
    let (a, b) = (lane.start(), lane.end());
    let n = math::hypot(b.x - a.x, b.y - a.y).max(1e-9);
    ((b.x - a.x) / n, (b.y - a.y) / n)
}

/// Where a crosswalk cuts a lane a vehicle or a cyclist drives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrosswalkConflict {
    /// Index into the [`crosswalks`] list.
    pub crosswalk: usize,
    /// The driven lane.
    pub lane: LaneId,
    /// Arc length along `lane` where the crosswalk's line crosses its centreline, metres.
    pub s_m: f64,
    /// How far either side of `s_m` the crosswalk band reaches along `lane` for a body as
    /// wide as the lane, metres.
    pub half_extent_m: f64,
    /// Arc length along the crosswalk's representative lane of the crossing point.
    pub s_on_crossing_m: f64,
}

impl CrosswalkConflict {
    /// The arc length on the driven lane where the band begins.
    pub fn enter_s(&self) -> f64 {
        self.s_m - self.half_extent_m
    }

    /// The arc length on the driven lane where the band ends.
    pub fn exit_s(&self) -> f64 {
        self.s_m + self.half_extent_m
    }
}

/// True for a lane a crosswalk can conflict with: one a motor vehicle or a cyclist drives.
pub fn is_driven(lane: &Lane) -> bool {
    !matches!(
        lane.kind,
        LaneKind::Sidewalk | LaneKind::Crossing | LaneKind::Parking
    ) && lane.admits(ClassMask::MOTOR_TRAFFIC.union(ClassMask::BICYCLE))
}

/// Every place a crosswalk cuts a driven lane, sorted by (lane, arc length).
///
/// Each crosswalk's representative lane is intersected with every driven lane's
/// centreline, segment by segment, after a bounding-box test on a 64 m grid. The band's
/// extent along the driven lane is `w/(2·sin θ) + (lane width/2)·|cot θ|`, with `θ` the
/// angle between the two, floored at `sin θ = 0.25` so an almost-parallel crossing is not
/// given an unbounded band.
pub fn crosswalk_conflicts(lanes: &[Lane], walks: &[Crosswalk]) -> Vec<CrosswalkConflict> {
    const CELL: f64 = 64.0;
    let cell = |v: f64| (v / CELL).floor() as i64;
    // The crosswalks' representative segments, bucketed.
    let mut grid: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    for (k, walk) in walks.iter().enumerate() {
        let Some(rep) = lanes.get(walk.lanes[0].as_usize()) else {
            continue;
        };
        let (lo, hi) = bbox(&rep.centreline, 1.0);
        for x in cell(lo.x)..=cell(hi.x) {
            for y in cell(lo.y)..=cell(hi.y) {
                grid.entry((x, y)).or_default().push(k);
            }
        }
    }
    let mut out = Vec::new();
    for lane in lanes.iter().filter(|l| is_driven(l)) {
        let (lo, hi) = bbox(&lane.centreline, 0.0);
        let mut candidates: Vec<usize> = Vec::new();
        for x in cell(lo.x)..=cell(hi.x) {
            for y in cell(lo.y)..=cell(hi.y) {
                if let Some(v) = grid.get(&(x, y)) {
                    candidates.extend_from_slice(v);
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        for k in candidates {
            let walk = &walks[k];
            let rep = &lanes[walk.lanes[0].as_usize()];
            for (s_lane, s_cross, sin, cos) in polyline_crossings(lane, rep) {
                let sin = sin.abs().max(0.25);
                // The crossing point is measured on the representative lane, which may sit
                // off the band's centre line; the band is widened by that offset rather
                // than shifted, which errs towards stopping early.
                let half = (0.5 * walk.width_m + walk.rep_offset_m) / sin
                    + 0.5 * lane.width_m * cos.abs() / sin;
                out.push(CrosswalkConflict {
                    crosswalk: k,
                    lane: lane.id,
                    s_m: s_lane,
                    half_extent_m: half,
                    s_on_crossing_m: s_cross,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        (a.lane, a.crosswalk)
            .cmp(&(b.lane, b.crosswalk))
            .then(a.s_m.total_cmp(&b.s_m))
    });
    out
}

/// The axis-aligned box of a polyline, grown by `margin`.
fn bbox(points: &[Vec3], margin: f64) -> (Vec3, Vec3) {
    let mut lo = Vec3::new_2d(f64::INFINITY, f64::INFINITY);
    let mut hi = Vec3::new_2d(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in points {
        lo.x = lo.x.min(p.x);
        lo.y = lo.y.min(p.y);
        hi.x = hi.x.max(p.x);
        hi.y = hi.y.max(p.y);
    }
    (
        Vec3::new_2d(lo.x - margin, lo.y - margin),
        Vec3::new_2d(hi.x + margin, hi.y + margin),
    )
}

/// Every point where `a`'s centreline crosses `b`'s: `(s on a, s on b, sin θ, cos θ)`,
/// `θ` the angle from `a`'s segment to `b`'s.
fn polyline_crossings(a: &Lane, b: &Lane) -> Vec<(f64, f64, f64, f64)> {
    let mut out = Vec::new();
    for i in 0..a.centreline.len().saturating_sub(1) {
        let (p, p2) = (a.centreline[i], a.centreline[i + 1]);
        for j in 0..b.centreline.len().saturating_sub(1) {
            let (q, q2) = (b.centreline[j], b.centreline[j + 1]);
            let r = (p2.x - p.x, p2.y - p.y);
            let s = (q2.x - q.x, q2.y - q.y);
            let denom = r.0 * s.1 - r.1 * s.0;
            if denom.abs() < 1e-12 {
                continue;
            }
            let qp = (q.x - p.x, q.y - p.y);
            let t = (qp.0 * s.1 - qp.1 * s.0) / denom;
            let u = (qp.0 * r.1 - qp.1 * r.0) / denom;
            if !(0.0..=1.0).contains(&t) || !(0.0..=1.0).contains(&u) {
                continue;
            }
            let rl = math::hypot(r.0, r.1).max(1e-12);
            let sl = math::hypot(s.0, s.1).max(1e-12);
            let sin = denom / (rl * sl);
            let cos = (r.0 * s.0 + r.1 * s.1) / (rl * sl);
            out.push((
                a.cumulative[i] + t * rl,
                b.cumulative[j] + u * sl,
                sin,
                cos,
            ));
        }
    }
    out
}

/// What [`signalise_crossings`] did, for the import report and the card.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PedestrianSignalReport {
    /// Crosswalks given pedestrian intervals.
    pub signalised: u32,
    /// Of those, crosswalks whose walk is shorter than the 7 s minimum but at least 4 s.
    pub short_walk: u32,
    /// Crosswalks whose window could not hold 4 s of walk and the full clearance: they get
    /// 4 s of walk and a shorter clearance.
    pub short_clearance: u32,
    /// Crosswalks that cut a signalised junction's movements but have no window at all:
    /// they show don't-walk throughout.
    pub never_walk: u32,
}

/// One pedestrian interval inside a cycle: `[start, end)` seconds from the cycle start
/// (the end may exceed the cycle, which wraps), and the state shown.
type Interval = (f64, f64, SignalState);

/// Gives every signalised junction's crosswalks their pedestrian intervals.
///
/// `junction_position` places a junction, to settle a crosswalk that two neighbouring
/// plans both reach: it goes to the nearer junction. Vehicle states are never changed;
/// phases are split where a pedestrian interval starts or ends, and each crossing lane is
/// appended to [`SignalPlan::controlled`] with a [`SignalHeadKind::Pedestrian`] head at
/// its far end.
pub fn signalise_crossings(
    plans: &mut [SignalPlan],
    lanes: &[Lane],
    connections: &[Connection],
    junction_position: impl Fn(JunctionId) -> Vec3,
    timing: &PedestrianTiming,
) -> PedestrianSignalReport {
    let mut report = PedestrianSignalReport::default();
    let walks = crosswalks(lanes);
    if walks.is_empty() {
        return report;
    }
    let conflicts = crosswalk_conflicts(lanes, &walks);
    let mut by_lane: BTreeMap<LaneId, Vec<&CrosswalkConflict>> = BTreeMap::new();
    for c in &conflicts {
        by_lane.entry(c.lane).or_default().push(c);
    }
    // Each internal lane's approach, departure and turn.
    let mut movement: BTreeMap<LaneId, (LaneId, LaneId, TurnDirection)> = BTreeMap::new();
    for c in connections {
        if let Some(via) = c.via {
            movement
                .entry(via)
                .or_insert((c.from_lane, c.to_lane, c.direction));
        }
    }

    // Which crosswalks each plan's movements cut, and through which movements.
    let mut claims: Vec<BTreeMap<usize, Vec<usize>>> = Vec::with_capacity(plans.len());
    let mut owner: BTreeMap<usize, (usize, f64)> = BTreeMap::new();
    for (pi, plan) in plans.iter().enumerate() {
        let mut cut: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (mi, internal) in plan.controlled.iter().enumerate() {
            if lanes.get(internal.as_usize()).map(|l| l.kind) != Some(LaneKind::Internal) {
                continue;
            }
            let Some((from, to, _)) = movement.get(internal) else {
                continue;
            };
            let mut hit = |lane: LaneId, keep: &dyn Fn(f64, f64) -> bool| {
                let Some(list) = by_lane.get(&lane) else {
                    return;
                };
                let length = lanes[lane.as_usize()].length_m;
                for c in list {
                    if keep(c.s_m, length) {
                        let v = cut.entry(c.crosswalk).or_default();
                        if !v.contains(&mi) {
                            v.push(mi);
                        }
                    }
                }
            };
            let reach = timing.reach_m;
            hit(*from, &|s, len| s >= len - reach);
            hit(*internal, &|_, _| true);
            hit(*to, &|s, _| s <= reach);
        }
        let at = junction_position(plan.junction);
        for k in cut.keys() {
            let d = walks[*k].midpoint.distance_2d(at);
            match owner.get(k) {
                Some((_, best)) if *best <= d => {}
                _ => {
                    owner.insert(*k, (pi, d));
                }
            }
        }
        claims.push(cut);
    }

    for (pi, plan) in plans.iter_mut().enumerate() {
        let mine: Vec<(usize, Vec<usize>)> = claims[pi]
            .iter()
            .filter(|(k, _)| owner.get(*k).is_some_and(|(p, _)| *p == pi))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        if mine.is_empty() || plan.phases.is_empty() {
            continue;
        }
        let turn_of = |mi: usize| -> TurnDirection {
            movement
                .get(&plan.controlled[mi])
                .map_or(TurnDirection::Straight, |m| m.2)
        };
        let mut added: Vec<(Vec<LaneId>, Vec<Interval>)> = Vec::new();
        for (k, cutters) in &mine {
            let blocked: Vec<bool> = plan
                .phases
                .iter()
                .map(|p| {
                    cutters
                        .iter()
                        .any(|mi| blocks_walk(p.states[*mi], turn_of(*mi)))
                })
                .collect();
            if blocked.iter().all(|b| !*b) {
                // Nothing that cuts it ever has a protected indication: an unsignalised
                // crosswalk inside a signalised junction, where turning traffic yields.
                continue;
            }
            let intervals = if blocked.iter().all(|b| *b) {
                report.never_walk += 1;
                Vec::new()
            } else {
                let (iv, short_walk, short_clear) =
                    walk_intervals(plan, &blocked, walks[*k].length_m, timing);
                report.signalised += 1;
                report.short_walk += u32::from(short_walk);
                report.short_clearance += u32::from(short_clear);
                if iv.is_empty() {
                    report.signalised -= 1;
                    report.never_walk += 1;
                }
                iv
            };
            added.push((walks[*k].lanes.clone(), intervals));
        }
        if !added.is_empty() {
            overlay(plan, lanes, &added, timing);
        }
    }
    report
}

/// True if a vehicle movement showing `state` stops the crosswalks it cuts from showing
/// walk: a straight-through movement on anything but red, or a turn on a protected green.
fn blocks_walk(state: SignalState, turn: TurnDirection) -> bool {
    match state {
        SignalState::Red | SignalState::Off => false,
        SignalState::Green => matches!(
            turn,
            TurnDirection::Straight | TurnDirection::Left | TurnDirection::UTurn
        ),
        SignalState::GreenYield
        | SignalState::Amber
        | SignalState::RedAmber
        | SignalState::FlashingAmber => turn == TurnDirection::Straight,
    }
}

/// The walk and clearance intervals of one crosswalk, from which phases block it.
///
/// Returns the intervals and whether the walk came out short (below the 7 s minimum) or
/// the clearance did (the window could not hold 4 s of walk and the whole clearance).
fn walk_intervals(
    plan: &SignalPlan,
    blocked: &[bool],
    length_m: f64,
    timing: &PedestrianTiming,
) -> (Vec<Interval>, bool, bool) {
    let n = plan.phases.len();
    let starts: Vec<f64> = plan
        .phases
        .iter()
        .scan(0.0, |acc, p| {
            let s = *acc;
            *acc += p.duration_s;
            Some(s)
        })
        .collect();
    let cycle = plan.cycle_s;
    let clearance = timing.clearance_s(length_m);
    let mut out = Vec::new();
    let (mut short_walk, mut short_clear) = (false, false);
    // Walk the cycle from the phase after a blocked one, collecting each run of unblocked
    // phases (a run may wrap past the cycle's end, so times are unwrapped).
    let first_blocked = blocked.iter().position(|b| *b).unwrap_or(0);
    let mut k = 0;
    while k < n {
        let i = (first_blocked + 1 + k) % n;
        if blocked[i] {
            k += 1;
            continue;
        }
        // The walk starts at phase `b0`; a phase before it in the plan's order is reached
        // after the cycle wraps.
        let b0 = (first_blocked + 1) % n;
        let unwrap = |j: usize| if j < b0 { starts[j] + cycle } else { starts[j] };
        let mut run = vec![i];
        k += 1;
        while k < n {
            let j = (first_blocked + 1 + k) % n;
            if blocked[j] {
                break;
            }
            run.push(j);
            k += 1;
        }
        // Unwrapped start of each phase in the run: monotone from the run's first.
        let base = unwrap(run[0]);
        let mut t = base;
        let mut walk_start: Option<f64> = None;
        for j in &run {
            let permitted = plan.phases[*j]
                .states
                .iter()
                .any(|s| matches!(s, SignalState::Green | SignalState::GreenYield));
            if walk_start.is_none() && permitted {
                walk_start = Some(t);
            }
            t += plan.phases[*j].duration_s;
        }
        let window_end = t;
        let start = walk_start.unwrap_or(base);
        let end = quantise(window_end - timing.buffer_s, 0.1);
        if end - start < timing.reduced_walk_s {
            continue;
        }
        let mut walk = end - clearance - start;
        if walk < timing.reduced_walk_s {
            walk = timing.reduced_walk_s;
            short_clear = true;
        } else if walk < timing.min_walk_s {
            short_walk = true;
        }
        let change = quantise(start + walk, 0.1).min(end);
        out.push((start, change, SignalState::Green));
        if end > change {
            out.push((change, end, SignalState::Amber));
        }
    }
    (out, short_walk, short_clear)
}

/// Splits the plan's phases at every pedestrian interval boundary and appends the
/// crosswalks' lanes, states and heads.
fn overlay(
    plan: &mut SignalPlan,
    lanes: &[Lane],
    added: &[(Vec<LaneId>, Vec<Interval>)],
    timing: &PedestrianTiming,
) {
    let cycle = plan.cycle_s;
    let wrap = |t: f64| {
        let mut x = t % cycle;
        if x < 0.0 {
            x += cycle;
        }
        quantise(x, Q_TIME_S)
    };
    // Phase starts, then every interval boundary.
    let mut parents: Vec<(f64, usize)> = Vec::new();
    let mut acc = 0.0;
    for (i, p) in plan.phases.iter().enumerate() {
        parents.push((quantise(acc, Q_TIME_S), i));
        acc += p.duration_s;
    }
    let mut cuts: Vec<f64> = parents.iter().map(|(t, _)| *t).collect();
    for (_, intervals) in added {
        for (a, b, _) in intervals {
            cuts.push(wrap(*a));
            cuts.push(wrap(*b));
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup_by(|a, b| (*a - *b).abs() < 0.5 * Q_TIME_S);
    cuts.retain(|t| *t < cycle - 0.5 * Q_TIME_S);

    let state_at = |intervals: &[Interval], t: f64| -> SignalState {
        for (a, b, s) in intervals {
            // `t` is in [0, cycle); an interval may run past the cycle's end.
            let inside = (t >= *a && t < *b) || (t + cycle >= *a && t + cycle < *b);
            if inside {
                return *s;
            }
        }
        SignalState::Red
    };
    let mut phases: Vec<SignalPhase> = Vec::with_capacity(cuts.len());
    for (idx, start) in cuts.iter().enumerate() {
        let end = cuts.get(idx + 1).copied().unwrap_or(cycle);
        let duration = quantise(end - start, Q_TIME_S);
        if duration <= 0.0 {
            continue;
        }
        let mid = start + 0.5 * (end - start);
        let parent = parents
            .iter()
            .rev()
            .find(|(t, _)| *t <= mid)
            .map_or(0, |(_, i)| *i);
        let mut states = plan.phases[parent].states.clone();
        for (walk_lanes, intervals) in added {
            let s = state_at(intervals, mid);
            states.extend(core::iter::repeat_n(s, walk_lanes.len()));
        }
        phases.push(SignalPhase {
            duration_s: duration,
            states,
            name: plan.phases[parent].name,
        });
    }
    // The last duration absorbs the rounding, so the phases sum to the cycle.
    let sum_but_last: f64 = phases
        .iter()
        .take(phases.len().saturating_sub(1))
        .map(|p| p.duration_s)
        .sum();
    if let Some(last) = phases.last_mut() {
        last.duration_s = quantise(cycle - sum_but_last, Q_TIME_S);
    }
    plan.phases = phases;
    for (k, (walk_lanes, _)) in added.iter().enumerate() {
        let group = PEDESTRIAN_GROUP_BASE.saturating_add(u16::try_from(k).unwrap_or(u16::MAX));
        for lane in walk_lanes {
            plan.controlled.push(*lane);
            if let Some(l) = lanes.get(lane.as_usize()) {
                let end = l.end();
                plan.heads.push(SignalHead {
                    lane: *lane,
                    position: Vec3::new(end.x, end.y, end.z + timing.head_height_m),
                    kind: SignalHeadKind::Pedestrian,
                    group,
                });
            }
        }
    }
}

/// The pedestrian signal state of `crossing` in `plan` at `t_s`, if the plan controls it.
pub fn crossing_state(plan: &SignalPlan, crossing: LaneId, t_s: f64) -> Option<SignalState> {
    let index = plan.controlled.iter().position(|l| *l == crossing)?;
    plan.states_at(t_s)?.get(index).copied()
}

/// True if a signal plan controls any crossing lane.
pub fn has_pedestrian_intervals(plan: &SignalPlan) -> bool {
    plan.heads
        .iter()
        .any(|h| h.kind == SignalHeadKind::Pedestrian)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ids::{EdgeId, SignalId};

    fn lane(id: u32, kind: LaneKind, pts: &[(f64, f64)], width: f64, allowed: ClassMask) -> Lane {
        Lane::new(
            LaneId::new(id),
            EdgeId::new(id),
            if kind == LaneKind::Internal {
                Some(JunctionId::new(0))
            } else {
                None
            },
            0,
            kind,
            pts.iter().map(|(x, y)| Vec3::new_2d(*x, *y)),
            width,
            10.0,
            allowed,
        )
        .expect("valid lane")
    }

    /// A crossroads at (0, 0): an east-bound approach, its internal straight connector and
    /// its departure, a north-bound approach with its connector, and a crosswalk across
    /// the east arm at x = 12 in both walking directions.
    fn crossroads() -> (Vec<Lane>, Vec<Connection>, SignalPlan) {
        let m = ClassMask::MOTOR_TRAFFIC;
        let p = ClassMask::PEDESTRIAN;
        let lanes = vec![
            lane(0, LaneKind::Driving, &[(-60.0, -1.75), (-10.0, -1.75)], 3.5, m),
            lane(1, LaneKind::Internal, &[(-10.0, -1.75), (10.0, -1.75)], 3.5, m),
            lane(2, LaneKind::Driving, &[(10.0, -1.75), (60.0, -1.75)], 3.5, m),
            lane(3, LaneKind::Driving, &[(1.75, -60.0), (1.75, -10.0)], 3.5, m),
            lane(4, LaneKind::Internal, &[(1.75, -10.0), (1.75, 10.0)], 3.5, m),
            lane(5, LaneKind::Driving, &[(1.75, 10.0), (1.75, 60.0)], 3.5, m),
            lane(6, LaneKind::Crossing, &[(12.0, -8.0), (12.0, 8.0)], 4.0, p),
            lane(7, LaneKind::Crossing, &[(12.0, 8.0), (12.0, -8.0)], 4.0, p),
        ];
        let conn = |f: u32, t: u32, via: u32| Connection {
            from_lane: LaneId::new(f),
            to_lane: LaneId::new(t),
            via: Some(LaneId::new(via)),
            direction: TurnDirection::Straight,
            permitted: true,
        };
        let connections = vec![conn(0, 2, 1), conn(3, 5, 4)];
        let phase = |d: f64, s: [SignalState; 2]| SignalPhase {
            duration_s: d,
            states: s.to_vec(),
            name: None,
        };
        use SignalState::{Amber, Green, Red};
        let plan = SignalPlan {
            id: SignalId::new(0),
            junction: JunctionId::new(0),
            cycle_s: 60.0,
            offset_s: 0.0,
            controlled: vec![LaneId::new(1), LaneId::new(4)],
            phases: vec![
                phase(27.0, [Green, Red]),
                phase(3.0, [Amber, Red]),
                phase(27.0, [Red, Green]),
                phase(3.0, [Red, Amber]),
            ],
            heads: Vec::new(),
        };
        (lanes, connections, plan)
    }

    #[test]
    fn the_two_directions_of_one_crossing_are_one_crosswalk() {
        let (lanes, _, _) = crossroads();
        let walks = crosswalks(&lanes);
        assert_eq!(walks.len(), 1);
        assert_eq!(walks[0].lanes, vec![LaneId::new(6), LaneId::new(7)]);
    }

    #[test]
    fn a_crosswalk_cuts_the_lane_it_crosses_and_not_the_parallel_one() {
        let (lanes, _, _) = crossroads();
        let walks = crosswalks(&lanes);
        let c = crosswalk_conflicts(&lanes, &walks);
        // The east-bound departure (lane 2) runs from x = 10: the crosswalk at x = 12 is
        // 2 m along it. The north-bound lanes run parallel to the crosswalk: no conflict.
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].lane, LaneId::new(2));
        assert!((c[0].s_m - 2.0).abs() < 1e-9);
        // Perpendicular: the band is half the crosswalk's width either side.
        assert!((c[0].half_extent_m - 2.0).abs() < 1e-9);
    }

    #[test]
    fn walk_runs_with_the_parallel_green_and_clears_at_3_5_ft_per_s() {
        let (lanes, connections, plan) = crossroads();
        let mut plans = vec![plan];
        let report = signalise_crossings(
            &mut plans,
            &lanes,
            &connections,
            |_| Vec3::ZERO,
            &PedestrianTiming::mutcd(),
        );
        assert_eq!(report.signalised, 1);
        let plan = &plans[0];
        assert_eq!(plan.controlled.len(), 4);
        assert_eq!(plan.heads.len(), 2);
        // The east-west through movement cuts the crosswalk: it is don't-walk while that
        // movement is green or amber, and walk starts with the north-south green at 30 s.
        let walk = |t: f64| crossing_state(plan, LaneId::new(6), t).unwrap();
        assert_eq!(walk(0.0), SignalState::Red);
        assert_eq!(walk(28.0), SignalState::Red);
        assert_eq!(walk(30.0), SignalState::Green);
        // Clearance: 16 m at 1.0668 m/s is 15.0 s, ending 3 s before the window's end at
        // 60 s: flashing from 42.0 s to 57.0 s, so walk is 12 s.
        assert_eq!(walk(41.9), SignalState::Green);
        assert_eq!(walk(42.1), SignalState::Amber);
        assert_eq!(walk(56.9), SignalState::Amber);
        assert_eq!(walk(57.1), SignalState::Red);
        // Both directions show the same.
        for t in [5.0, 35.0, 50.0, 59.0] {
            assert_eq!(
                crossing_state(plan, LaneId::new(7), t),
                crossing_state(plan, LaneId::new(6), t)
            );
        }
        // Vehicle states are untouched at every instant.
        let (_, _, original) = crossroads();
        for k in 0..600 {
            let t = f64::from(k) * 0.1;
            assert_eq!(
                &plan.states_at(t).unwrap()[..2],
                original.states_at(t).unwrap(),
                "at {t}"
            );
        }
        // The phases still fill the cycle.
        assert!((plan.total_phase_duration_s() - plan.cycle_s).abs() < 1e-6);
    }

    /// The procedural grid's pedestrian network: every crosswalk lane is reached from a
    /// sidewalk and leads to one, every interior junction's four crosswalks carry
    /// pedestrian intervals, and a grid without sidewalks gets none of it.
    #[test]
    fn the_grid_gets_a_connected_walk_network_with_pedestrian_phases() {
        use crate::ImportOptions;
        use crate::procedural::{GridParams, grid};
        let params = GridParams {
            sidewalk_m: 2.0,
            crossings: true,
            signalised: true,
            corner_radius_m: 4.5,
            lanes_per_direction: 2,
            ..GridParams::legacy().with_size(3, 3)
        };
        let world = grid(&params, &ImportOptions::default()).expect("grid");
        let lanes = world.roads.lanes();
        let crossing: Vec<&Lane> = lanes
            .iter()
            .filter(|l| l.kind == LaneKind::Crossing)
            .collect();
        // Interior junction (1, 1): four arms, blocks on both sides of each: 4 crosswalks.
        // Each edge-middle junction: one arm with blocks on both sides. Corners: none.
        assert_eq!(crossing.len(), 16, "8 crosswalks, two directions each");
        assert_eq!(crosswalks(lanes).len(), 8);
        for c in &crossing {
            let into = world
                .roads
                .connections()
                .iter()
                .filter(|x| x.to_lane == c.id)
                .count();
            let out = world.successors(c.id).len();
            assert!(into > 0 && out > 0, "crossing {} is not joined", c.id);
            for x in world.successors(c.id) {
                assert_eq!(world.lane(x.to_lane).kind, LaneKind::Sidewalk);
            }
        }
        // Every walking lane is pedestrian-only.
        for l in lanes
            .iter()
            .filter(|l| matches!(l.kind, LaneKind::Sidewalk | LaneKind::Crossing))
        {
            assert_eq!(l.allowed, ClassMask::PEDESTRIAN);
        }
        // The centre junction's plan controls its eight crossing lanes, each with walk,
        // flashing don't-walk and don't-walk in the cycle.
        let centre = world
            .signals
            .iter()
            .find(|p| p.junction == JunctionId::new(4))
            .expect("the centre junction is signalised");
        let controlled_crossings: Vec<LaneId> = centre
            .controlled
            .iter()
            .copied()
            .filter(|l| world.lane(*l).kind == LaneKind::Crossing)
            .collect();
        assert_eq!(controlled_crossings.len(), 8);
        for l in &controlled_crossings {
            let seen: std::collections::BTreeSet<_> = (0..600)
                .map(|k| crossing_state(centre, *l, f64::from(k) * 0.1).unwrap())
                .collect();
            assert!(seen.contains(&SignalState::Green), "{l} never walks");
            assert!(seen.contains(&SignalState::Amber), "{l} never clears");
            assert!(seen.contains(&SignalState::Red), "{l} never stops");
        }
        // No sidewalks, no walk network: the legacy grid is untouched.
        let legacy = grid(&GridParams::legacy(), &ImportOptions::default()).expect("grid");
        assert!(
            legacy
                .roads
                .lanes()
                .iter()
                .all(|l| !matches!(l.kind, LaneKind::Sidewalk | LaneKind::Crossing))
        );
    }

    #[test]
    fn a_short_window_is_reported_not_hidden() {
        let (mut lanes, connections, plan) = crossroads();
        // A 40 m crossing: 37.5 s of clearance cannot fit a 30 s window.
        lanes[6] = lane(
            6,
            LaneKind::Crossing,
            &[(12.0, -20.0), (12.0, 20.0)],
            4.0,
            ClassMask::PEDESTRIAN,
        );
        lanes[7] = lane(
            7,
            LaneKind::Crossing,
            &[(12.0, 20.0), (12.0, -20.0)],
            4.0,
            ClassMask::PEDESTRIAN,
        );
        let mut plans = vec![plan];
        let report = signalise_crossings(
            &mut plans,
            &lanes,
            &connections,
            |_| Vec3::ZERO,
            &PedestrianTiming::mutcd(),
        );
        assert_eq!(report.short_clearance, 1, "{report:?}");
        let walk = |t: f64| crossing_state(&plans[0], LaneId::new(6), t).unwrap();
        assert_eq!(walk(31.0), SignalState::Green);
        assert_eq!(walk(34.5), SignalState::Amber);
    }
}
