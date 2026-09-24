//! The traffic-invariant auditor: every vehicle, every step, against the rules a road
//! user is held to.
//!
//! # Why it exists
//!
//! The owner watched the live page and reported cars that touch, cars that go through
//! buildings, a car that drives round a queue stopped at a red light and through the
//! junction, and motion that is not smooth. Each of those is an *invariant* a traffic
//! model must hold at every step, and none of them was checked anywhere: the engine's unit
//! tests put two or three vehicles on a lane and look at one property each. This module
//! checks all of them, for every vehicle, at every mobility step, on whatever scenario it
//! is handed, and counts every violation by class with examples (vehicle, time, place).
//!
//! # What it reads
//!
//! [`crate::NativeMobility::audit_actors`] — the engine's own internal state (lane, front
//! arc length, lateral offset, speed, acceleration, lane-change transition, next route
//! lane) and the published pose — and the world, including the signal plans. The signal
//! state a vehicle is judged against is the engine's own: the plan evaluated at the
//! instant the step's decisions were taken (`t0`), which is exactly what
//! [`crate::intersection::FixedTimeSignals::state_for`] reads.
//!
//! # The checks
//!
//! | Class | The rule |
//! |---|---|
//! | [`Check::Overlap`] | two vehicles' oriented footprints intersect |
//! | [`Check::GapBelowMinimum`] | a follower's net gap to its in-path leader is below its own standstill gap `s0` |
//! | [`Check::LateralOffset`] | the offset from the lane centreline exceeds what a lane change can produce |
//! | [`Check::InBuilding`] | a vehicle's body centre or a corner is inside a building footprint |
//! | [`Check::OutsideJunction`] | a vehicle on a junction's internal path is outside the junction area |
//! | [`Check::RedEntry`] | a vehicle entered a junction on a red (or red-amber) for its movement |
//! | [`Check::AmberEntry`] | it entered on an amber it could have stopped for at the amber's onset |
//! | [`Check::ConflictZone`] | two vehicles on conflicting movements occupy the same conflict zone at once |
//! | [`Check::LaneChangeNearJunction`] | a lane change started inside the no-change zone before a stop line |
//! | [`Check::QueueJump`] | a lane change round a vehicle standing at the stop line, started inside the no-change zone |
//! | [`Check::IllegalTransition`] | a vehicle arrived on a lane the lane graph does not connect to its previous one |
//! | [`Check::Teleport`] | the published position moved further in one step than the speed allows |
//! | [`Check::HeadingJump`] / [`Check::HeadingFlip`] | the heading turned faster than a car can, or reversed |
//! | [`Check::SpeedJump`] | the speed changed faster than any acceleration the vehicle can produce |
//! | [`Check::AccelBound`] | the acceleration is outside the vehicle's capability or the tyre-road limit |
//! | [`Check::Jerk`] | the acceleration changed faster than the jerk bound |
//! | [`Check::Standstill`] | a vehicle stood still for longer than the standstill limit (gridlock) |
//! | [`Check::MidRoadDespawn`] | a vehicle left the road anywhere but at the end of its trip |
//!
//! And three static checks of the world ([`audit_world`]): internal paths that leave their
//! junction, lanes that pass through a building, and signal phases that give two
//! conflicting movements a protected green at once.
//!
//! # The bounds, and where they come from
//!
//! See [`AuditParams`]: every bound is stated with its source or marked as this crate's
//! choice. None of them is tuned to make a run pass — the tests in this module inject each
//! fault and watch the check go red.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use v2xw_core::geom::{Bbox, Vec3};
use v2xw_core::ids::{ActorId, JunctionId, LaneId};
use v2xw_core::math;
use v2xw_core::time::{SimTime, ns_to_secs};
use v2xw_world::model::{normalise_angle, point_in_ring, ring_distance_sq_2d};
use v2xw_world::{JunctionControl, LaneKind, SignalState, World};

use crate::intersection::zones::{ConflictZones, Zone};
use crate::views::DespawnCause;

/// How far below ground a point must be to count as underground (in a tunnel, under the
/// buildings above it), metres: half a tunnel level.
const UNDERGROUND_Z_M: f64 = 3.0;

/// One vehicle as the auditor sees it: the engine's internal state plus the published
/// pose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuditActor {
    /// Which actor.
    pub actor: ActorId,
    /// Body length, metres.
    pub length_m: f64,
    /// Body width, metres.
    pub width_m: f64,
    /// The driver's standstill gap `s0`, metres.
    pub min_gap_m: f64,
    /// The driver's maximum acceleration `a`, m/s².
    pub max_accel_mps2: f64,
    /// The lane its front bumper is on.
    pub lane: LaneId,
    /// The lane it drove off to reach `lane`, while the body may still overhang it.
    pub prev_lane: Option<LaneId>,
    /// Front-bumper arc length along `lane`, metres.
    pub s_m: f64,
    /// Lateral offset from `lane`'s centreline, metres.
    pub lateral_m: f64,
    /// Speed, m/s.
    pub speed_mps: f64,
    /// The acceleration the last step applied, m/s².
    pub accel_mps2: f64,
    /// `(from, to)` while a lane-change transition is in progress.
    pub changing: Option<(LaneId, LaneId)>,
    /// The next lane of its route.
    pub route_next: Option<LaneId>,
    /// The published reference point (rear-axle centre).
    pub pos: Vec3,
    /// The published heading, radians.
    pub heading_rad: f64,
}

/// A violation class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Check {
    /// Two vehicles' footprints intersect.
    Overlap,
    /// An in-path gap below the follower's standstill gap.
    GapBelowMinimum,
    /// A lateral offset no lane change explains.
    LateralOffset,
    /// A vehicle inside a building footprint.
    InBuilding,
    /// A vehicle on an internal path outside its junction's area.
    OutsideJunction,
    /// A junction entered on red.
    RedEntry,
    /// A junction entered on an amber the vehicle could have stopped for.
    AmberEntry,
    /// Two conflicting movements in one conflict zone at once.
    ConflictZone,
    /// A lane change started inside the no-change zone before a stop line.
    LaneChangeNearJunction,
    /// A lane change round a queue standing at the stop line.
    QueueJump,
    /// A lane the lane graph does not reach from the previous one.
    IllegalTransition,
    /// A position jump the speed cannot explain.
    Teleport,
    /// A heading rate no car can turn at.
    HeadingJump,
    /// A heading reversal in one step.
    HeadingFlip,
    /// A speed change no acceleration explains.
    SpeedJump,
    /// An acceleration outside the physical envelope.
    AccelBound,
    /// A jerk above the bound.
    Jerk,
    /// A vehicle stood still past the standstill limit.
    Standstill,
    /// A despawn anywhere but at the end of the trip.
    MidRoadDespawn,
    /// World: an internal path that leaves its junction's area.
    WorldInternalOutsideJunction,
    /// World: a lane centreline inside a building footprint.
    WorldLaneInBuilding,
    /// World: two conflicting movements given a protected green in the same phase.
    WorldConflictingGreens,
}

impl Check {
    /// Every class, in report order.
    pub const ALL: [Check; 22] = [
        Check::Overlap,
        Check::GapBelowMinimum,
        Check::LateralOffset,
        Check::InBuilding,
        Check::OutsideJunction,
        Check::RedEntry,
        Check::AmberEntry,
        Check::ConflictZone,
        Check::LaneChangeNearJunction,
        Check::QueueJump,
        Check::IllegalTransition,
        Check::Teleport,
        Check::HeadingJump,
        Check::HeadingFlip,
        Check::SpeedJump,
        Check::AccelBound,
        Check::Jerk,
        Check::Standstill,
        Check::MidRoadDespawn,
        Check::WorldInternalOutsideJunction,
        Check::WorldLaneInBuilding,
        Check::WorldConflictingGreens,
    ];

    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Check::Overlap => "overlap",
            Check::GapBelowMinimum => "gap-below-minimum",
            Check::LateralOffset => "lateral-offset",
            Check::InBuilding => "in-building",
            Check::OutsideJunction => "outside-junction",
            Check::RedEntry => "red-entry",
            Check::AmberEntry => "amber-entry",
            Check::ConflictZone => "conflict-zone",
            Check::LaneChangeNearJunction => "lane-change-near-junction",
            Check::QueueJump => "queue-jump",
            Check::IllegalTransition => "illegal-transition",
            Check::Teleport => "teleport",
            Check::HeadingJump => "heading-jump",
            Check::HeadingFlip => "heading-flip",
            Check::SpeedJump => "speed-jump",
            Check::AccelBound => "accel-bound",
            Check::Jerk => "jerk",
            Check::Standstill => "standstill",
            Check::MidRoadDespawn => "mid-road-despawn",
            Check::WorldInternalOutsideJunction => "world-internal-outside-junction",
            Check::WorldLaneInBuilding => "world-lane-in-building",
            Check::WorldConflictingGreens => "world-conflicting-greens",
        }
    }
}

/// The auditor's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AuditParams {
    /// How much each footprint is shrunk on every side before the overlap test, metres.
    ///
    /// **This crate's choice**: 5 cm, so two bumpers that touch on the millimetre
    /// quantisation grid are not a collision and anything more is.
    pub footprint_shrink_m: f64,
    /// Numerical tolerance on the standstill-gap test, metres.
    ///
    /// **This crate's choice**: 5 cm. IDM keeps `s ≥ s0` asymptotically; a 0.1 s
    /// integration can undershoot by millimetres.
    pub gap_tolerance_m: f64,
    /// The largest deceleration any vehicle may show, m/s².
    ///
    /// 9 m/s² ≈ μ·g with μ ≈ 0.9, the upper end of the dry-asphalt peak friction range
    /// (AASHTO *A Policy on Geometric Design of Highways and Streets*, 2018, §3.2.2 treats
    /// 3.4 m/s² as the comfortable and ≈ 9 m/s² as the emergency envelope). Anything
    /// harder is not a car.
    pub max_decel_mps2: f64,
    /// The jerk bound, m/s³.
    ///
    /// **This crate's choice, bracketed by the literature**: comfortable driving stays
    /// under about 2 m/s³ and emergency braking onset reaches 10-30 m/s³ (a brake system
    /// builds 8-9 m/s² in 0.2-0.3 s). 30 m/s³ is therefore the physical ceiling, not a
    /// comfort target.
    pub max_jerk_mps3: f64,
    /// The tightest path radius the published reference point may follow, metres.
    ///
    /// A passenger car's kerb-to-kerb turning radius is about 5-6 m (AASHTO 2018 Table 2-2,
    /// design vehicle P: minimum centreline turning radius 7.3 m, minimum inside radius
    /// 4.4 m). 4 m is the inside radius rounded down, so the check only fires on a heading
    /// change no road vehicle can make.
    pub min_turn_radius_m: f64,
    /// Slack on the teleport test, metres.
    pub teleport_slack_m: f64,
    /// The no-change zone before a stop line, metres.
    ///
    /// MUTCD 2009 §3B.04: a solid lane line where "crossing the lane line markings is
    /// discouraged", which is how the approach to a signalised junction is marked; the
    /// length of the solid section is an engineering choice the manual does not fix. The
    /// engine's own zone ([`crate::engine::EngineParams::no_change_zone_m`]) is what a
    /// vehicle obeys; this is what the auditor holds it to.
    pub no_change_zone_m: f64,
    /// A vehicle standing longer than this is counted as gridlocked, seconds.
    ///
    /// **This crate's choice**: 180 s is two full cycles of the longest generated signal
    /// plan (90 s), so a vehicle that waits through two reds is not gridlock and one that
    /// waits through three is.
    pub standstill_limit_s: f64,
    /// The deceleration the amber rule judges "could have stopped" by, m/s².
    ///
    /// 3 m/s², `netconvert --tls.yellow.min-decel` and the ITE yellow-interval
    /// deceleration (≈ 10 ft/s²), the same number the signal model sizes the yellow with.
    pub amber_decel_mps2: f64,
    /// How many examples to keep per class.
    pub examples_per_check: usize,
}

impl Default for AuditParams {
    fn default() -> Self {
        Self {
            footprint_shrink_m: 0.05,
            gap_tolerance_m: 0.05,
            max_decel_mps2: 9.0,
            max_jerk_mps3: 30.0,
            min_turn_radius_m: 4.0,
            teleport_slack_m: 0.25,
            no_change_zone_m: crate::engine::DEFAULT_NO_CHANGE_ZONE_M,
            standstill_limit_s: 180.0,
            amber_decel_mps2: 3.0,
            examples_per_check: 5,
        }
    }
}

/// One recorded violation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Example {
    /// The class.
    pub check: Check,
    /// When, seconds.
    pub t_s: f64,
    /// Which vehicle, if one.
    pub actor: Option<u32>,
    /// The other vehicle, for a pairwise class.
    pub other: Option<u32>,
    /// Where, world-local metres.
    pub x_m: f64,
    /// Where, world-local metres.
    pub y_m: f64,
    /// The lane involved, if one.
    pub lane: Option<u32>,
    /// What exactly.
    pub detail: String,
}

/// Distribution statistics the report carries alongside the counts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AuditStats {
    /// Mobility steps observed.
    pub steps: u64,
    /// Vehicle-steps observed.
    pub vehicle_steps: u64,
    /// Distinct vehicles observed.
    pub vehicles: u64,
    /// Largest vehicle count at one step.
    pub peak_vehicles: u64,
    /// Lane changes started.
    pub lane_changes: u64,
    /// Lane changes started beside a queue standing at the stop line, outside the
    /// no-change zone — moving to the shorter queue, which is lawful; a statistic, not a
    /// violation.
    pub lane_changes_past_standing_queue: u64,
    /// Junction entries observed.
    pub junction_entries: u64,
    /// Entries into a signalised junction.
    pub signalised_entries: u64,
    /// Steps at which two conflicting movements were both inside one junction (not a
    /// violation by itself: a permissive left waits inside the box).
    pub conflicting_occupancy_steps: u64,
    /// Vehicle-steps with the body in a building while on a lane whose own centreline
    /// runs through that building ([`Check::WorldLaneInBuilding`]): the source data puts
    /// the road there, as a passage or a covered ramp. Not counted as
    /// [`Check::InBuilding`], which is a vehicle off its road.
    pub in_building_on_lanes_through_buildings: u64,
    /// Largest |jerk| seen, m/s³.
    pub max_jerk_mps3: f64,
    /// Largest deceleration seen, m/s².
    pub max_decel_mps2: f64,
    /// Largest heading rate seen, rad/s.
    pub max_yaw_rate_rad_s: f64,
    /// Smallest in-path net gap seen, metres.
    pub min_gap_m: f64,
    /// Longest continuous standstill, seconds.
    pub max_standstill_s: f64,
    /// Vehicles that finished their trip.
    pub trips_completed: u64,
    /// Mean speed over every vehicle-step, m/s.
    pub mean_speed_mps: f64,
    /// Share of vehicle-steps at a standstill (speed ≤ 0.1 m/s).
    pub stopped_fraction: f64,
    /// Sum of speeds, for the mean.
    #[serde(skip)]
    speed_sum: f64,
    /// Vehicle-steps at a standstill, for the share.
    #[serde(skip)]
    stopped_steps: u64,
}

/// What an audit found.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AuditReport {
    /// Violations per class, every class present (zero included).
    pub counts: BTreeMap<String, u64>,
    /// Up to [`AuditParams::examples_per_check`] examples per class.
    pub examples: Vec<Example>,
    /// Distributions.
    pub stats: AuditStats,
}

impl AuditReport {
    /// The count of one class.
    pub fn count(&self, check: Check) -> u64 {
        self.counts.get(check.label()).copied().unwrap_or(0)
    }
}

/// The auditor.
#[derive(Debug, Clone)]
pub struct TrafficAuditor {
    params: AuditParams,
    counts: BTreeMap<Check, u64>,
    examples: BTreeMap<Check, Vec<Example>>,
    stats: AuditStats,
    prev: BTreeMap<ActorId, AuditActor>,
    seen: BTreeSet<ActorId>,
    /// `(gap to stop line, speed)` at the first step an actor's movement showed amber.
    amber_onset: BTreeMap<ActorId, (LaneId, f64, f64)>,
    standing_since: BTreeMap<ActorId, SimTime>,
    flagged_standing: BTreeSet<ActorId>,
    /// Every junction's conflict zones.
    zones: ConflictZones,
    /// Lanes whose own centreline passes through a building footprint in the source
    /// data — a tunnel under one, a passage through one.
    lanes_through_buildings: BTreeSet<LaneId>,
}

impl TrafficAuditor {
    /// An auditor for runs over `world`.
    pub fn new(world: &World, params: AuditParams) -> Self {
        Self {
            params,
            counts: BTreeMap::new(),
            examples: BTreeMap::new(),
            stats: AuditStats {
                min_gap_m: f64::INFINITY,
                ..AuditStats::default()
            },
            prev: BTreeMap::new(),
            seen: BTreeSet::new(),
            amber_onset: BTreeMap::new(),
            standing_since: BTreeMap::new(),
            flagged_standing: BTreeSet::new(),
            zones: ConflictZones::build(world),
            lanes_through_buildings: audit_world(world)
                .into_iter()
                .filter(|(c, _)| *c == Check::WorldLaneInBuilding)
                .filter_map(|(_, e)| e.lane.map(LaneId::new))
                .collect(),
        }
    }

    /// Records one violation.
    fn flag(&mut self, check: Check, example: Example) {
        *self.counts.entry(check).or_insert(0) += 1;
        let list = self.examples.entry(check).or_default();
        if list.len() < self.params.examples_per_check {
            list.push(example);
        }
    }

    fn example(
        check: Check,
        t: SimTime,
        a: &AuditActor,
        other: Option<ActorId>,
        detail: String,
    ) -> Example {
        Example {
            check,
            t_s: ns_to_secs(t),
            actor: Some(a.actor.index()),
            other: other.map(|o| o.index()),
            x_m: a.pos.x,
            y_m: a.pos.y,
            lane: Some(a.lane.index()),
            detail,
        }
    }

    /// Adds the static world checks to this auditor's counts.
    pub fn audit_world(&mut self, world: &World) {
        for (check, example) in audit_world(world) {
            self.flag(check, example);
        }
    }

    /// Observes one mobility step: `actors` is every vehicle's state at `t1`, the end of
    /// the step that started at `t0`; `despawned` is what the step removed.
    pub fn observe(
        &mut self,
        world: &World,
        t0: SimTime,
        t1: SimTime,
        actors: &[AuditActor],
        despawned: &[(ActorId, DespawnCause)],
    ) {
        let dt = ns_to_secs(t1.saturating_sub(t0)).max(1e-9);
        self.stats.steps += 1;
        self.stats.vehicle_steps += actors.len() as u64;
        self.stats.peak_vehicles = self.stats.peak_vehicles.max(actors.len() as u64);
        for a in actors {
            if self.seen.insert(a.actor) {
                self.stats.vehicles += 1;
            }
            self.stats.speed_sum += a.speed_mps;
            if a.speed_mps <= 0.1 {
                self.stats.stopped_steps += 1;
            }
        }

        self.check_amber_onsets(world, t0);
        self.check_overlaps(t1, actors);
        self.check_gaps(world, t1, actors);
        self.check_placement(world, t1, actors);
        self.check_transitions(world, t0, t1, actors);
        self.check_conflict_zones(world, t1, actors);
        self.check_kinematics(t1, dt, actors);
        self.check_standstill(t1, actors);
        self.check_despawns(world, t1, despawned);

        self.prev = actors.iter().map(|a| (a.actor, *a)).collect();
    }

    /// The report so far.
    pub fn report(&self) -> AuditReport {
        let mut counts = BTreeMap::new();
        for c in Check::ALL {
            counts.insert(
                c.label().to_string(),
                self.counts.get(&c).copied().unwrap_or(0),
            );
        }
        let mut examples = Vec::new();
        for c in Check::ALL {
            if let Some(list) = self.examples.get(&c) {
                examples.extend(list.iter().cloned());
            }
        }
        let mut stats = self.stats.clone();
        if !stats.min_gap_m.is_finite() {
            stats.min_gap_m = 0.0;
        }
        if stats.vehicle_steps > 0 {
            stats.mean_speed_mps = stats.speed_sum / stats.vehicle_steps as f64;
            stats.stopped_fraction = stats.stopped_steps as f64 / stats.vehicle_steps as f64;
        }
        AuditReport {
            counts,
            examples,
            stats,
        }
    }

    // -----------------------------------------------------------------------
    // The checks
    // -----------------------------------------------------------------------

    /// Records the first instant each approaching vehicle saw amber, from the start-of-step
    /// states (the instant the engine's decision was taken).
    fn check_amber_onsets(&mut self, world: &World, t0: SimTime) {
        let mut still: BTreeSet<ActorId> = BTreeSet::new();
        let prev: Vec<AuditActor> = self.prev.values().copied().collect();
        for a in &prev {
            let lane = world.lane(a.lane);
            if lane.kind == LaneKind::Internal {
                continue;
            }
            let Some(next) = a.route_next else { continue };
            let Some(state) = movement_state(world, next, t0) else {
                continue;
            };
            if state == SignalState::Amber {
                still.insert(a.actor);
                self.amber_onset
                    .entry(a.actor)
                    .or_insert((next, lane.length_m - a.s_m, a.speed_mps));
            }
        }
        self.amber_onset.retain(|a, _| still.contains(a));
    }

    fn check_overlaps(&mut self, t: SimTime, actors: &[AuditActor]) {
        let shrink = self.params.footprint_shrink_m;
        let boxes: Vec<Obb> = actors.iter().map(|a| Obb::of(a, shrink)).collect();
        let cell = 12.0;
        let mut grid: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
        for (i, b) in boxes.iter().enumerate() {
            let key = (
                (b.centre.0 / cell).floor() as i64,
                (b.centre.1 / cell).floor() as i64,
            );
            grid.entry(key).or_default().push(i);
        }
        let mut pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
        for (&(cx, cy), members) in &grid {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let Some(others) = grid.get(&(cx + dx, cy + dy)) else {
                        continue;
                    };
                    for &i in members {
                        for &j in others {
                            // Two levels (a tunnel under a street) do not collide.
                            let apart = (actors[i].pos.z - actors[j].pos.z).abs() > UNDERGROUND_Z_M;
                            if i < j && !apart && boxes[i].intersects(&boxes[j]) {
                                pairs.insert((i, j));
                            }
                        }
                    }
                }
            }
        }
        for (i, j) in pairs {
            let (a, b) = (&actors[i], &actors[j]);
            let ex = Self::example(
                Check::Overlap,
                t,
                a,
                Some(b.actor),
                format!(
                    "lanes {} and {}, centres {:.2} m apart",
                    a.lane.index(),
                    b.lane.index(),
                    dist2(boxes[i].centre, boxes[j].centre)
                ),
            );
            self.flag(Check::Overlap, ex);
        }
    }

    fn check_gaps(&mut self, world: &World, t: SimTime, actors: &[AuditActor]) {
        let mut by_lane: BTreeMap<LaneId, Vec<usize>> = BTreeMap::new();
        for (i, a) in actors.iter().enumerate() {
            by_lane.entry(a.lane).or_default().push(i);
        }
        for list in by_lane.values_mut() {
            list.sort_by(|x, y| {
                actors[*x]
                    .s_m
                    .total_cmp(&actors[*y].s_m)
                    .then(actors[*x].actor.cmp(&actors[*y].actor))
            });
        }
        let mut found: Vec<(usize, usize, f64)> = Vec::new();
        for (lane, list) in &by_lane {
            for w in list.windows(2) {
                let (f, l) = (&actors[w[0]], &actors[w[1]]);
                found.push((w[0], w[1], l.s_m - l.length_m - f.s_m));
            }
            // The frontmost vehicle on this lane against the rearmost on its next lane.
            let Some(&front) = list.last() else { continue };
            let f = &actors[front];
            let Some(next) = f.route_next else { continue };
            let Some(nlist) = by_lane.get(&next) else {
                continue;
            };
            let Some(&first) = nlist.first() else {
                continue;
            };
            let l = &actors[first];
            let len = world.lane(*lane).length_m;
            found.push((front, first, (len - f.s_m) + (l.s_m - l.length_m)));
        }
        for (fi, li, gap) in found {
            let f = &actors[fi];
            let l = &actors[li];
            self.stats.min_gap_m = self.stats.min_gap_m.min(gap);
            if gap < f.min_gap_m - self.params.gap_tolerance_m {
                let ex = Self::example(
                    Check::GapBelowMinimum,
                    t,
                    f,
                    Some(l.actor),
                    format!(
                        "net gap {gap:.2} m to leader on lane {} (s0 {:.2} m), speeds \
                         {:.2}/{:.2} m/s",
                        l.lane.index(),
                        f.min_gap_m,
                        f.speed_mps,
                        l.speed_mps
                    ),
                );
                self.flag(Check::GapBelowMinimum, ex);
            }
        }
    }

    fn check_placement(&mut self, world: &World, t: SimTime, actors: &[AuditActor]) {
        for a in actors {
            // Lateral offset: zero outside a transition, at most the two centrelines'
            // separation inside one.
            let bound = match a.changing {
                None => 1e-6,
                Some((from, to)) => {
                    0.5 * (world.lane(from).width_m + world.lane(to).width_m) + 1e-6
                }
            };
            if a.lateral_m.abs() > bound {
                let ex = Self::example(
                    Check::LateralOffset,
                    t,
                    a,
                    None,
                    format!("offset {:.2} m, bound {bound:.2} m", a.lateral_m),
                );
                self.flag(Check::LateralOffset, ex);
            }
            let obb = Obb::of(a, 0.1);
            let corners = obb.corners();
            let mut points = vec![Vec3::new(obb.centre.0, obb.centre.1, 0.0)];
            points.extend(corners.iter().map(|c| Vec3::new(c.0, c.1, 0.0)));
            let (mut lo, mut hi) = (points[0], points[0]);
            for p in &points {
                lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), 0.0);
                hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), 0.0);
            }
            // A vehicle below ground (in a tunnel) is under the buildings, not in them.
            let candidates = if a.pos.z < -UNDERGROUND_Z_M {
                Vec::new()
            } else {
                world.buildings_in_bbox(Bbox::new(lo, hi))
            };
            'buildings: for b in candidates {
                let Some(building) = world.building(b) else {
                    continue;
                };
                for p in &points {
                    if building.contains_2d(*p) {
                        let on_data_lane = self.lanes_through_buildings.contains(&a.lane)
                            || a.prev_lane
                                .is_some_and(|l| self.lanes_through_buildings.contains(&l));
                        if on_data_lane {
                            // The source data runs this road through (or under) the
                            // building — the Park Avenue portals of the Helmsley
                            // Building, a covered tunnel ramp. That is the world's fact,
                            // reported once per lane by `WorldLaneInBuilding`, not a
                            // vehicle leaving its lane; it is counted as a statistic.
                            self.stats.in_building_on_lanes_through_buildings += 1;
                            break 'buildings;
                        }
                        let ex = Self::example(
                            Check::InBuilding,
                            t,
                            a,
                            None,
                            format!("building {} at ({:.1}, {:.1})", b.index(), p.x, p.y),
                        );
                        self.flag(Check::InBuilding, ex);
                        break 'buildings;
                    }
                }
            }
            let lane = world.lane(a.lane);
            if lane.kind == LaneKind::Internal
                && let Some(j) = lane.junction.and_then(|j| world.roads.try_junction(j))
                && j.shape.len() >= 4
            {
                let c = Vec3::new(obb.centre.0, obb.centre.1, 0.0);
                let tol = lane.width_m;
                if !point_in_ring(&j.shape, c) && ring_distance_sq_2d(&j.shape, c) > tol * tol {
                    let ex = Self::example(
                        Check::OutsideJunction,
                        t,
                        a,
                        None,
                        format!(
                            "junction {}, body centre {:.1} m outside its area",
                            j.id.index(),
                            math::sqrt(ring_distance_sq_2d(&j.shape, c))
                        ),
                    );
                    self.flag(Check::OutsideJunction, ex);
                }
            }
        }
    }

    fn check_transitions(
        &mut self,
        world: &World,
        t0: SimTime,
        t1: SimTime,
        actors: &[AuditActor],
    ) {
        for a in actors {
            let Some(p) = self.prev.get(&a.actor).copied() else {
                continue;
            };
            if p.lane == a.lane {
                continue;
            }
            let started_change =
                a.changing.is_some_and(|(_, to)| to == a.lane) && p.changing.is_none();
            if started_change {
                self.stats.lane_changes += 1;
                let from = world.lane(p.lane);
                let to = world.lane(a.lane);
                let adjacent = from.edge == to.edge
                    && (i32::from(from.index) - i32::from(to.index)).abs() == 1
                    && from.kind != LaneKind::Internal;
                if !adjacent {
                    let ex = Self::example(
                        Check::IllegalTransition,
                        t1,
                        a,
                        None,
                        format!(
                            "lane change {} -> {} is not to an adjacent lane of one edge",
                            p.lane.index(),
                            a.lane.index()
                        ),
                    );
                    self.flag(Check::IllegalTransition, ex);
                }
                let to_line = from.length_m - p.s_m;
                if to_line < self.params.no_change_zone_m {
                    let ex = Self::example(
                        Check::LaneChangeNearJunction,
                        t1,
                        a,
                        None,
                        format!(
                            "started {to_line:.1} m before the end of lane {}",
                            p.lane.index()
                        ),
                    );
                    self.flag(Check::LaneChangeNearJunction, ex);
                }
                // A queue standing at the line on the lane being left. Moving to the
                // shorter queue well back from the line is what drivers do and is counted
                // only as a statistic; doing it inside the no-change zone — cutting round
                // the queue at the line — is the violation.
                let queued = self.prev.values().find(|o| {
                    o.actor != a.actor
                        && o.lane == p.lane
                        && o.s_m > p.s_m
                        && o.speed_mps < 0.5
                        && from.length_m - o.s_m < 15.0
                        && o.s_m - o.length_m - p.s_m < 60.0
                });
                if queued.is_some() {
                    self.stats.lane_changes_past_standing_queue += 1;
                }
                if let Some(q) = queued.filter(|_| to_line < self.params.no_change_zone_m) {
                    let ex = Self::example(
                        Check::QueueJump,
                        t1,
                        a,
                        Some(q.actor),
                        format!(
                            "left lane {} past vehicle {} standing {:.1} m before its end",
                            p.lane.index(),
                            q.actor.index(),
                            from.length_m - q.s_m
                        ),
                    );
                    self.flag(Check::QueueJump, ex);
                }
                continue;
            }
            // A longitudinal crossing: the new lane must be reachable along the graph.
            let Some(path) = reachable(world, p.lane, a.lane, 4) else {
                // A jump onto a junction's internal path is also judged against that
                // movement's signal: a vehicle that jumps a queue into a junction on red
                // has run the red, however it got there.
                if world.lane(a.lane).kind == LaneKind::Internal
                    && world.lane(p.lane).kind != LaneKind::Internal
                    && let Some(state @ (SignalState::Red | SignalState::RedAmber)) =
                        movement_state(world, a.lane, t0)
                {
                    let ex = Self::example(
                        Check::RedEntry,
                        t1,
                        a,
                        None,
                        format!(
                            "jumped from lane {} onto movement {} on {state:?}",
                            p.lane.index(),
                            a.lane.index()
                        ),
                    );
                    self.flag(Check::RedEntry, ex);
                }
                let ex = Self::example(
                    Check::IllegalTransition,
                    t1,
                    a,
                    None,
                    format!(
                        "moved from lane {} to lane {}, which the lane graph does not \
                         connect",
                        p.lane.index(),
                        a.lane.index()
                    ),
                );
                self.flag(Check::IllegalTransition, ex);
                continue;
            };
            // Junction entry: the first internal lane on the path, from a non-internal one.
            if world.lane(p.lane).kind == LaneKind::Internal {
                continue;
            }
            let Some(internal) = path
                .iter()
                .copied()
                .find(|l| world.lane(*l).kind == LaneKind::Internal)
            else {
                continue;
            };
            self.stats.junction_entries += 1;
            let Some(state) = movement_state(world, internal, t0) else {
                continue;
            };
            self.stats.signalised_entries += 1;
            match state {
                SignalState::Red | SignalState::RedAmber => {
                    let ex = Self::example(
                        Check::RedEntry,
                        t1,
                        a,
                        None,
                        format!(
                            "entered movement {} from lane {} on {state:?} at {:.2} m/s",
                            internal.index(),
                            p.lane.index(),
                            p.speed_mps
                        ),
                    );
                    self.flag(Check::RedEntry, ex);
                }
                SignalState::Amber => {
                    if let Some((_, gap, v)) = self.amber_onset.get(&a.actor).copied() {
                        let stopping = v * v / (2.0 * self.params.amber_decel_mps2);
                        if stopping + 0.5 < gap {
                            let ex = Self::example(
                                Check::AmberEntry,
                                t1,
                                a,
                                None,
                                format!(
                                    "amber began {gap:.1} m out at {v:.2} m/s (stopping \
                                     distance {stopping:.1} m) and it entered anyway"
                                ),
                            );
                            self.flag(Check::AmberEntry, ex);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn check_conflict_zones(&mut self, world: &World, t: SimTime, actors: &[AuditActor]) {
        // Occupancy intervals on internal lanes.
        let mut occ: BTreeMap<LaneId, Vec<(usize, f64, f64)>> = BTreeMap::new();
        for (i, a) in actors.iter().enumerate() {
            let lane = world.lane(a.lane);
            if lane.kind == LaneKind::Internal {
                occ.entry(a.lane).or_default().push((
                    i,
                    (a.s_m - a.length_m).max(0.0),
                    a.s_m.min(lane.length_m),
                ));
            }
            if a.s_m < a.length_m
                && let Some(prev) = a.prev_lane
                && world.lane(prev).kind == LaneKind::Internal
            {
                let len = world.lane(prev).length_m;
                occ.entry(prev)
                    .or_default()
                    .push((i, (len + a.s_m - a.length_m).max(0.0), len));
            }
        }
        let mut by_junction: BTreeMap<JunctionId, Vec<LaneId>> = BTreeMap::new();
        for lane in occ.keys() {
            if let Some(j) = world.lane(*lane).junction {
                by_junction.entry(j).or_default().push(*lane);
            }
        }
        let mut reported: BTreeSet<(ActorId, ActorId)> = BTreeSet::new();
        for (junction, lanes) in &by_junction {
            let Some(j) = world.roads.try_junction(*junction) else {
                continue;
            };
            let row = |l: LaneId| j.internal.iter().position(|x| *x == l);
            let mut conflicting_occupancy = false;
            for (ia, la) in lanes.iter().enumerate() {
                for lb in lanes.iter().skip(ia + 1) {
                    let (Some(ra), Some(rb)) = (row(*la), row(*lb)) else {
                        continue;
                    };
                    if !j.conflicts.is_foe(ra, rb) {
                        continue;
                    }
                    if self.zones.approach_of(*la) == self.zones.approach_of(*lb) {
                        continue; // one approach lane: a queue, not a conflict
                    }
                    conflicting_occupancy = true;
                    let zones: Vec<Zone> = self
                        .zones
                        .of(*la)
                        .iter()
                        .filter(|z| z.other == *lb)
                        .copied()
                        .collect();
                    for z in zones {
                        for &(i, a0, a1) in &occ[la] {
                            if !z.holds_self(a0, a1) {
                                continue;
                            }
                            for &(k, b0, b1) in &occ[lb] {
                                if i == k || !z.holds_other(b0, b1) {
                                    continue;
                                }
                                let (x, y) = (actors[i].actor, actors[k].actor);
                                let key = if x < y { (x, y) } else { (y, x) };
                                if !reported.insert(key) {
                                    continue;
                                }
                                let ex = Self::example(
                                    Check::ConflictZone,
                                    t,
                                    &actors[i],
                                    Some(actors[k].actor),
                                    format!(
                                        "junction {}: movements {} and {} both in the zone \
                                         at s = {:.1} / {:.1} m",
                                        junction.index(),
                                        la.index(),
                                        lb.index(),
                                        z.s_self,
                                        z.s_other
                                    ),
                                );
                                self.flag(Check::ConflictZone, ex);
                            }
                        }
                    }
                }
            }
            if conflicting_occupancy {
                self.stats.conflicting_occupancy_steps += 1;
            }
        }
    }

    fn check_kinematics(&mut self, t: SimTime, dt: f64, actors: &[AuditActor]) {
        for a in actors {
            if a.accel_mps2 > a.max_accel_mps2 + 1e-6 || a.accel_mps2 < -self.params.max_decel_mps2
            {
                let ex = Self::example(
                    Check::AccelBound,
                    t,
                    a,
                    None,
                    format!(
                        "acceleration {:.2} m/s² (vehicle max {:.2}, decel bound {:.1})",
                        a.accel_mps2, a.max_accel_mps2, self.params.max_decel_mps2
                    ),
                );
                self.flag(Check::AccelBound, ex);
            }
            self.stats.max_decel_mps2 = self.stats.max_decel_mps2.max(-a.accel_mps2);
            let Some(p) = self.prev.get(&a.actor).copied() else {
                continue;
            };
            let dv = a.speed_mps - p.speed_mps;
            if dv > a.max_accel_mps2 * dt + 1e-6 || -dv > self.params.max_decel_mps2 * dt + 1e-6 {
                let ex = Self::example(
                    Check::SpeedJump,
                    t,
                    a,
                    None,
                    format!("speed {:.2} -> {:.2} m/s in {dt:.2} s", p.speed_mps, a.speed_mps),
                );
                self.flag(Check::SpeedJump, ex);
            }
            let jerk = (a.accel_mps2 - p.accel_mps2).abs() / dt;
            self.stats.max_jerk_mps3 = self.stats.max_jerk_mps3.max(jerk);
            if jerk > self.params.max_jerk_mps3 {
                let ex = Self::example(
                    Check::Jerk,
                    t,
                    a,
                    None,
                    format!(
                        "acceleration {:.2} -> {:.2} m/s² in {dt:.2} s ({jerk:.1} m/s³)",
                        p.accel_mps2, a.accel_mps2
                    ),
                );
                self.flag(Check::Jerk, ex);
            }
            let moved = a.pos.distance_2d(p.pos);
            let v = a.speed_mps.max(p.speed_mps);
            let lateral = (a.lateral_m - p.lateral_m).abs();
            let allowed = v * dt + lateral + self.params.teleport_slack_m;
            if moved > allowed {
                let ex = Self::example(
                    Check::Teleport,
                    t,
                    a,
                    None,
                    format!(
                        "moved {moved:.2} m in {dt:.2} s at {v:.2} m/s (lane {} -> {})",
                        p.lane.index(),
                        a.lane.index()
                    ),
                );
                self.flag(Check::Teleport, ex);
            }
            let turn = normalise_angle(a.heading_rad - p.heading_rad).abs();
            let rate = turn / dt;
            self.stats.max_yaw_rate_rad_s = self.stats.max_yaw_rate_rad_s.max(rate);
            if turn > core::f64::consts::FRAC_PI_2 {
                let ex = Self::example(
                    Check::HeadingFlip,
                    t,
                    a,
                    None,
                    format!(
                        "heading {:.1}° -> {:.1}°",
                        p.heading_rad.to_degrees(),
                        a.heading_rad.to_degrees()
                    ),
                );
                self.flag(Check::HeadingFlip, ex);
            } else {
                // A path of radius R turns at most `distance / R`; the lateral slide of a
                // lane change adds its own heading swing, which the engine caps at 12°.
                let travelled = 0.5 * (a.speed_mps + p.speed_mps) * dt;
                let bound = travelled / self.params.min_turn_radius_m + 0.02;
                if turn > bound + if a.changing.is_some() || p.changing.is_some() {
                    0.21
                } else {
                    0.0
                } {
                    let ex = Self::example(
                        Check::HeadingJump,
                        t,
                        a,
                        None,
                        format!(
                            "turned {:.1}° over {travelled:.2} m (bound {:.1}°), lane {} -> {}",
                            turn.to_degrees(),
                            bound.to_degrees(),
                            p.lane.index(),
                            a.lane.index()
                        ),
                    );
                    self.flag(Check::HeadingJump, ex);
                }
            }
        }
    }

    fn check_standstill(&mut self, t: SimTime, actors: &[AuditActor]) {
        let present: BTreeSet<ActorId> = actors.iter().map(|a| a.actor).collect();
        self.standing_since.retain(|a, _| present.contains(a));
        for a in actors {
            if a.speed_mps > 0.1 {
                self.standing_since.remove(&a.actor);
                continue;
            }
            let since = *self.standing_since.entry(a.actor).or_insert(t);
            let standing = ns_to_secs(t.saturating_sub(since));
            self.stats.max_standstill_s = self.stats.max_standstill_s.max(standing);
            if standing > self.params.standstill_limit_s && self.flagged_standing.insert(a.actor)
            {
                let ex = Self::example(
                    Check::Standstill,
                    t,
                    a,
                    None,
                    format!("standing for {standing:.0} s"),
                );
                self.flag(Check::Standstill, ex);
            }
        }
    }

    fn check_despawns(
        &mut self,
        world: &World,
        t: SimTime,
        despawned: &[(ActorId, DespawnCause)],
    ) {
        for (actor, cause) in despawned {
            if *cause == DespawnCause::TripComplete {
                self.stats.trips_completed += 1;
                continue;
            }
            let Some(p) = self.prev.get(actor).copied() else {
                continue;
            };
            let lane = world.lane(p.lane);
            let ex = Self::example(
                Check::MidRoadDespawn,
                t,
                &p,
                None,
                format!(
                    "{cause:?} {:.1} m before the end of lane {}",
                    lane.length_m - p.s_m,
                    p.lane.index()
                ),
            );
            self.flag(Check::MidRoadDespawn, ex);
        }
    }
}

/// The signal state the engine shows the movement through internal lane `internal` at
/// `t`, or `None` if that movement is not signal-controlled.
pub fn movement_state(world: &World, internal: LaneId, t: SimTime) -> Option<SignalState> {
    let lane = world.try_lane(internal)?;
    if lane.kind != LaneKind::Internal {
        return None;
    }
    let junction = world.roads.try_junction(lane.junction?)?;
    let JunctionControl::Signalised { plan } = junction.control else {
        return None;
    };
    let plan = world.signal_plan(plan)?;
    let index = plan.controlled.iter().position(|l| *l == internal)?;
    plan.states_at(ns_to_secs(t))?.get(index).copied()
}

/// The lanes from `from` (exclusive) to `to` (inclusive) along the lane graph, within
/// `hops` lanes, or `None`.
fn reachable(world: &World, from: LaneId, to: LaneId, hops: usize) -> Option<Vec<LaneId>> {
    let mut frontier: Vec<Vec<LaneId>> = vec![Vec::new()];
    for _ in 0..hops {
        let mut next = Vec::new();
        for path in &frontier {
            let last = path.last().copied().unwrap_or(from);
            for l in world.successor_lanes(last) {
                let mut p = path.clone();
                p.push(l);
                if l == to {
                    return Some(p);
                }
                next.push(p);
            }
        }
        frontier = next;
    }
    None
}

fn dist2(a: (f64, f64), b: (f64, f64)) -> f64 {
    math::sqrt((a.0 - b.0) * (a.0 - b.0) + (a.1 - b.1) * (a.1 - b.1))
}

/// An oriented footprint.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Obb {
    centre: (f64, f64),
    axis: (f64, f64),
    half_len: f64,
    half_wid: f64,
}

impl Obb {
    /// The footprint of `a`: the published point is the rear reference, the body extends
    /// forward along the heading.
    fn of(a: &AuditActor, shrink: f64) -> Obb {
        let (s, c) = math::sin_cos(a.heading_rad);
        let half_len = (0.5 * a.length_m - shrink).max(0.01);
        let half_wid = (0.5 * a.width_m - shrink).max(0.01);
        Obb {
            centre: (
                a.pos.x + 0.5 * a.length_m * c,
                a.pos.y + 0.5 * a.length_m * s,
            ),
            axis: (c, s),
            half_len,
            half_wid,
        }
    }

    fn corners(&self) -> [(f64, f64); 4] {
        let (c, s) = self.axis;
        let (l, w) = (self.half_len, self.half_wid);
        let (x, y) = self.centre;
        [
            (x + l * c - w * s, y + l * s + w * c),
            (x + l * c + w * s, y + l * s - w * c),
            (x - l * c + w * s, y - l * s - w * c),
            (x - l * c - w * s, y - l * s + w * c),
        ]
    }

    /// The separating-axis test for two rectangles.
    fn intersects(&self, other: &Obb) -> bool {
        let axes = [
            self.axis,
            (-self.axis.1, self.axis.0),
            other.axis,
            (-other.axis.1, other.axis.0),
        ];
        let a = self.corners();
        let b = other.corners();
        for ax in axes {
            let proj = |pts: &[(f64, f64); 4]| {
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                for p in pts {
                    let d = p.0 * ax.0 + p.1 * ax.1;
                    lo = lo.min(d);
                    hi = hi.max(d);
                }
                (lo, hi)
            };
            let (a0, a1) = proj(&a);
            let (b0, b1) = proj(&b);
            if a1 < b0 || b1 < a0 {
                return false;
            }
        }
        true
    }
}

/// The static world checks: internal paths outside their junction, lanes through
/// buildings, conflicting protected greens.
pub fn audit_world(world: &World) -> Vec<(Check, Example)> {
    let mut out = Vec::new();
    let example = |check: Check, p: Vec3, lane: Option<LaneId>, detail: String| Example {
        check,
        t_s: 0.0,
        actor: None,
        other: None,
        x_m: p.x,
        y_m: p.y,
        lane: lane.map(|l| l.index()),
        detail,
    };
    for lane in world.roads.lanes() {
        if !lane.kind.is_motorised() && lane.kind != LaneKind::Internal {
            continue;
        }
        let samples = (lane.length_m / 1.0).ceil().max(1.0) as usize;
        let mut outside_reported = false;
        let mut building_reported = false;
        for k in 0..=samples {
            let s = lane.length_m * (k as f64) / (samples as f64);
            let p = lane.point_at(s);
            if lane.kind == LaneKind::Internal
                && !outside_reported
                && let Some(j) = lane.junction.and_then(|j| world.roads.try_junction(j))
                && j.shape.len() >= 4
            {
                let tol = lane.width_m;
                if !point_in_ring(&j.shape, p) && ring_distance_sq_2d(&j.shape, p) > tol * tol {
                    outside_reported = true;
                    out.push((
                        Check::WorldInternalOutsideJunction,
                        example(
                            Check::WorldInternalOutsideJunction,
                            p,
                            Some(lane.id),
                            format!(
                                "internal lane of junction {} passes {:.1} m outside it",
                                j.id.index(),
                                math::sqrt(ring_distance_sq_2d(&j.shape, p))
                            ),
                        ),
                    ));
                }
            }
            if !building_reported && p.z >= -UNDERGROUND_Z_M {
                for b in world.buildings_in_bbox(Bbox::new(p, p)) {
                    if world.building(b).is_some_and(|bl| bl.contains_2d(p)) {
                        building_reported = true;
                        out.push((
                            Check::WorldLaneInBuilding,
                            example(
                                Check::WorldLaneInBuilding,
                                p,
                                Some(lane.id),
                                format!("centreline inside building {}", b.index()),
                            ),
                        ));
                        break;
                    }
                }
            }
        }
    }
    // Conflicting protected greens.
    let mut approach_of: BTreeMap<LaneId, LaneId> = BTreeMap::new();
    let mut exit_of: BTreeMap<LaneId, LaneId> = BTreeMap::new();
    for c in world.roads.connections() {
        if let Some(via) = c.via {
            approach_of.entry(via).or_insert(c.from_lane);
            exit_of.entry(via).or_insert(c.to_lane);
        }
    }
    for plan in &world.signals {
        let Some(j) = world.roads.try_junction(plan.junction) else {
            continue;
        };
        let row = |l: LaneId| j.internal.iter().position(|x| *x == l);
        for (pi, phase) in plan.phases.iter().enumerate() {
            for (a, sa) in phase.states.iter().enumerate() {
                for (b, sb) in phase.states.iter().enumerate().skip(a + 1) {
                    if *sa != SignalState::Green || *sb != SignalState::Green {
                        continue;
                    }
                    let (la, lb) = (plan.controlled[a], plan.controlled[b]);
                    if approach_of.get(&la) == approach_of.get(&lb) {
                        continue;
                    }
                    // Two lanes of one road merging into one where the road narrows — a
                    // lane drop — share a green everywhere; the engine zips them. That
                    // is geometry, not a signal plan that lets two streams collide.
                    let same_road = approach_of
                        .get(&la)
                        .zip(approach_of.get(&lb))
                        .is_some_and(|(x, y)| world.lane(*x).edge == world.lane(*y).edge);
                    if same_road && exit_of.get(&la) == exit_of.get(&lb) {
                        continue;
                    }
                    let (Some(ra), Some(rb)) = (row(la), row(lb)) else {
                        continue;
                    };
                    if j.conflicts.is_foe(ra, rb) {
                        out.push((
                            Check::WorldConflictingGreens,
                            example(
                                Check::WorldConflictingGreens,
                                j.position,
                                Some(la),
                                format!(
                                    "signal {} phase {pi}: movements {} and {} conflict and \
                                     are both protected green",
                                    plan.id.index(),
                                    la.index(),
                                    lb.index()
                                ),
                            ),
                        ));
                    }
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

    fn grid() -> World {
        v2xw_world::procedural::grid(
            &GridParams::legacy().with_signals(true),
            &ImportOptions::default(),
        )
        .expect("grid")
    }

    fn straight_lane(world: &World) -> &v2xw_world::Lane {
        world
            .roads
            .lanes()
            .iter()
            .find(|l| l.kind == LaneKind::Driving && l.length_m > 40.0)
            .expect("a driving lane")
    }

    fn at(world: &World, id: u32, lane: LaneId, s: f64, v: f64) -> AuditActor {
        let l = world.lane(lane);
        let rear = (s - 4.5).max(0.0);
        AuditActor {
            actor: ActorId::new(id),
            length_m: 4.5,
            width_m: 1.8,
            min_gap_m: 2.0,
            max_accel_mps2: 1.4,
            lane,
            prev_lane: None,
            s_m: s,
            lateral_m: 0.0,
            speed_mps: v,
            accel_mps2: 0.0,
            changing: None,
            route_next: None,
            pos: l.point_at(rear),
            heading_rad: l.heading_at(rear),
        }
    }

    #[test]
    fn two_bodies_on_one_spot_are_an_overlap_and_a_short_gap() {
        let world = grid();
        let lane = straight_lane(&world).id;
        let mut audit = TrafficAuditor::new(&world, AuditParams::default());
        // Clean: 10 m apart.
        audit.observe(
            &world,
            0,
            100_000_000,
            &[at(&world, 0, lane, 10.0, 0.0), at(&world, 1, lane, 24.5, 0.0)],
            &[],
        );
        assert_eq!(audit.report().count(Check::Overlap), 0);
        assert_eq!(audit.report().count(Check::GapBelowMinimum), 0);
        // Fault: the follower's front 1 m inside the leader.
        audit.observe(
            &world,
            100_000_000,
            200_000_000,
            &[at(&world, 0, lane, 21.0, 0.0), at(&world, 1, lane, 24.5, 0.0)],
            &[],
        );
        let r = audit.report();
        assert_eq!(r.count(Check::Overlap), 1, "{r:?}");
        assert_eq!(r.count(Check::GapBelowMinimum), 1);
    }

    #[test]
    fn a_jump_is_a_teleport_and_a_speed_step_is_a_speed_jump() {
        let world = grid();
        let lane = straight_lane(&world).id;
        let mut audit = TrafficAuditor::new(&world, AuditParams::default());
        audit.observe(&world, 0, 100_000_000, &[at(&world, 0, lane, 10.0, 5.0)], &[]);
        audit.observe(&world, 100_000_000, 200_000_000, &[at(&world, 0, lane, 10.5, 5.0)], &[]);
        assert_eq!(audit.report().count(Check::Teleport), 0);
        audit.observe(&world, 200_000_000, 300_000_000, &[at(&world, 0, lane, 20.0, 5.0)], &[]);
        assert_eq!(audit.report().count(Check::Teleport), 1);
        audit.observe(&world, 300_000_000, 400_000_000, &[at(&world, 0, lane, 20.5, 9.0)], &[]);
        assert_eq!(audit.report().count(Check::SpeedJump), 1);
    }

    #[test]
    fn a_lane_the_graph_does_not_reach_is_an_illegal_transition() {
        let world = grid();
        let a = straight_lane(&world).id;
        // A lane that is neither a successor nor adjacent: pick one far away.
        let far = world
            .roads
            .lanes()
            .iter()
            .rev()
            .find(|l| l.kind == LaneKind::Driving && reachable(&world, a, l.id, 4).is_none())
            .expect("a far lane")
            .id;
        let mut audit = TrafficAuditor::new(&world, AuditParams::default());
        audit.observe(&world, 0, 100_000_000, &[at(&world, 0, a, 10.0, 5.0)], &[]);
        audit.observe(&world, 100_000_000, 200_000_000, &[at(&world, 0, far, 10.0, 5.0)], &[]);
        assert_eq!(audit.report().count(Check::IllegalTransition), 1);
    }

    #[test]
    fn entering_on_red_is_flagged_and_on_green_is_not() {
        let world = grid();
        let plan = world.signals.first().expect("signalised");
        // A movement and the phase instants at which it is red and green.
        let internal = plan.controlled[0];
        let approach = world
            .roads
            .connections()
            .iter()
            .find(|c| c.via == Some(internal))
            .expect("an approach")
            .from_lane;
        let find = |want: fn(SignalState) -> bool| -> SimTime {
            (0..1200u64)
                .map(|k| k * 100_000_000)
                .find(|t| movement_state(&world, internal, *t).is_some_and(want))
                .expect("the state occurs")
        };
        let red = find(|s| s == SignalState::Red);
        let green = find(|s| s == SignalState::Green);
        let len = world.lane(approach).length_m;
        for (t0, expect) in [(green, 0u64), (red, 1u64)] {
            let mut audit = TrafficAuditor::new(&world, AuditParams::default());
            audit.observe(&world, t0, t0 + 100_000_000, &[at(&world, 0, approach, len - 0.2, 5.0)], &[]);
            let mut inside = at(&world, 0, internal, 0.3, 5.0);
            inside.prev_lane = Some(approach);
            audit.observe(&world, t0 + 100_000_000, t0 + 200_000_000, &[inside], &[]);
            // Judged at the start of the second step.
            let t_judge = t0 + 100_000_000;
            let state = movement_state(&world, internal, t_judge);
            let r = audit.report();
            if state == Some(SignalState::Red) {
                assert_eq!(r.count(Check::RedEntry), expect, "{state:?}");
            } else {
                assert_eq!(r.count(Check::RedEntry), 0, "{state:?}");
            }
        }
    }

    #[test]
    fn two_vehicles_in_one_conflict_zone_are_flagged() {
        let world = grid();
        let audit = TrafficAuditor::new(&world, AuditParams::default());
        let (lane, zone) = world
            .roads
            .junctions()
            .iter()
            .flat_map(|j| j.internal.iter())
            .find_map(|l| audit.zones.of(*l).first().map(|z| (*l, *z)))
            .expect("the grid has crossing movements");
        let mut audit = audit;
        let a = at(&world, 0, lane, zone.s_self + 1.0, 3.0);
        let b = at(&world, 1, zone.other, zone.s_other + 1.0, 3.0);
        audit.observe(&world, 0, 100_000_000, &[a], &[]);
        assert_eq!(audit.report().count(Check::ConflictZone), 0);
        audit.observe(&world, 100_000_000, 200_000_000, &[a, b], &[]);
        assert_eq!(audit.report().count(Check::ConflictZone), 1);
    }

    #[test]
    fn a_heading_reversal_is_a_flip() {
        let world = grid();
        let lane = straight_lane(&world).id;
        let mut audit = TrafficAuditor::new(&world, AuditParams::default());
        let a = at(&world, 0, lane, 10.0, 5.0);
        audit.observe(&world, 0, 100_000_000, &[a], &[]);
        let mut b = at(&world, 0, lane, 10.5, 5.0);
        b.heading_rad += core::f64::consts::PI;
        audit.observe(&world, 100_000_000, 200_000_000, &[b], &[]);
        assert_eq!(audit.report().count(Check::HeadingFlip), 1);
        let mut c = at(&world, 0, lane, 11.0, 5.0);
        c.heading_rad = b.heading_rad + 0.6;
        audit.observe(&world, 200_000_000, 300_000_000, &[c], &[]);
        assert!(audit.report().count(Check::HeadingJump) >= 1);
    }

    #[test]
    fn a_vehicle_that_never_moves_is_gridlocked_and_a_despawn_midroad_is_flagged() {
        let world = grid();
        let lane = straight_lane(&world).id;
        let mut audit = TrafficAuditor::new(&world, AuditParams::default());
        let a = at(&world, 0, lane, 10.0, 0.0);
        for k in 0..1900u64 {
            audit.observe(&world, k * 100_000_000, (k + 1) * 100_000_000, &[a], &[]);
        }
        assert_eq!(audit.report().count(Check::Standstill), 1);
        audit.observe(
            &world,
            1900 * 100_000_000,
            1901 * 100_000_000,
            &[],
            &[(ActorId::new(0), DespawnCause::LifetimeExpired)],
        );
        assert_eq!(audit.report().count(Check::MidRoadDespawn), 1);
    }

    #[test]
    fn a_body_inside_a_building_is_flagged() {
        let world = v2xw_world::procedural::grid(
            &GridParams::tr36885_urban(),
            &ImportOptions::default(),
        )
        .expect("grid");
        let building = world.buildings.first().expect("block buildings");
        let ring = building.open_ring();
        let cx = ring.iter().map(|p| p.x).sum::<f64>() / ring.len() as f64;
        let cy = ring.iter().map(|p| p.y).sum::<f64>() / ring.len() as f64;
        let lane = straight_lane(&world).id;
        let mut a = at(&world, 0, lane, 10.0, 0.0);
        let mut audit = TrafficAuditor::new(&world, AuditParams::default());
        audit.observe(&world, 0, 100_000_000, &[a], &[]);
        assert_eq!(audit.report().count(Check::InBuilding), 0);
        a.pos = Vec3::new(cx, cy, 0.0);
        audit.observe(&world, 100_000_000, 200_000_000, &[a], &[]);
        assert_eq!(audit.report().count(Check::InBuilding), 1);
    }

    #[test]
    fn an_offset_without_a_lane_change_and_a_body_outside_its_junction_are_flagged() {
        let world = grid();
        let lane = straight_lane(&world).id;
        let mut audit = TrafficAuditor::new(&world, AuditParams::default());
        let mut a = at(&world, 0, lane, 10.0, 5.0);
        audit.observe(&world, 0, 100_000_000, &[a], &[]);
        assert_eq!(audit.report().count(Check::LateralOffset), 0);
        a.lateral_m = 1.0; // no transition under way
        audit.observe(&world, 100_000_000, 200_000_000, &[a], &[]);
        assert_eq!(audit.report().count(Check::LateralOffset), 1);
        // A vehicle on a junction connector, placed 50 m away from the junction.
        let internal = world
            .roads
            .lanes()
            .iter()
            .find(|l| l.kind == LaneKind::Internal)
            .expect("a connector")
            .id;
        let mut b = at(&world, 1, internal, 4.6, 0.0);
        audit.observe(&world, 200_000_000, 300_000_000, &[b], &[]);
        assert_eq!(audit.report().count(Check::OutsideJunction), 0);
        b.pos = Vec3::new(b.pos.x + 50.0, b.pos.y, 0.0);
        audit.observe(&world, 300_000_000, 400_000_000, &[b], &[]);
        assert_eq!(audit.report().count(Check::OutsideJunction), 1);
    }

    #[test]
    fn acceleration_and_jerk_beyond_the_bounds_are_flagged() {
        let world = grid();
        let lane = straight_lane(&world).id;
        let mut audit = TrafficAuditor::new(&world, AuditParams::default());
        let mut a = at(&world, 0, lane, 10.0, 5.0);
        a.accel_mps2 = -2.0;
        audit.observe(&world, 0, 100_000_000, &[a], &[]);
        let mut b = at(&world, 0, lane, 10.5, 4.8);
        b.accel_mps2 = -2.5;
        audit.observe(&world, 100_000_000, 200_000_000, &[b], &[]);
        assert_eq!(audit.report().count(Check::Jerk), 0, "5 m/s³ is within the bound");
        assert_eq!(audit.report().count(Check::AccelBound), 0);
        let mut c = at(&world, 0, lane, 11.0, 4.0);
        c.accel_mps2 = -9.5; // beyond the tyre-road limit, and 70 m/s³ of jerk
        audit.observe(&world, 200_000_000, 300_000_000, &[c], &[]);
        let r = audit.report();
        assert_eq!(r.count(Check::AccelBound), 1);
        assert_eq!(r.count(Check::Jerk), 1);
    }

    #[test]
    fn an_amber_the_vehicle_could_have_stopped_for_is_flagged() {
        let world = grid();
        let plan = world.signals.first().expect("signalised");
        let internal = plan.controlled[0];
        let approach = world
            .roads
            .connections()
            .iter()
            .find(|c| c.via == Some(internal))
            .expect("an approach")
            .from_lane;
        let len = world.lane(approach).length_m;
        // The first 0.1 s-aligned instant the movement shows amber.
        let amber = (0..1200u64)
            .map(|k| k * 100_000_000)
            .find(|t| movement_state(&world, internal, *t) == Some(SignalState::Amber))
            .expect("an amber");
        let entry = |gap_at_onset: f64, speed: f64| {
            let mut audit = TrafficAuditor::new(&world, AuditParams::default());
            let mut a = at(&world, 0, approach, len - gap_at_onset, speed);
            a.route_next = Some(internal);
            // Seen approaching at the onset of amber...
            audit.observe(&world, amber - 100_000_000, amber, &[a], &[]);
            let mut b = a;
            b.s_m = len - 0.1;
            audit.observe(&world, amber, amber + 100_000_000, &[b], &[]);
            // ...and inside the junction a step later, still on amber.
            let mut c = at(&world, 0, internal, 0.3, speed);
            c.prev_lane = Some(approach);
            audit.observe(&world, amber + 100_000_000, amber + 200_000_000, &[c], &[]);
            audit.report().count(Check::AmberEntry)
        };
        // 10 m/s needs 16.7 m at 3 m/s²: from 40 m out it could have stopped.
        assert_eq!(entry(40.0, 10.0), 1);
        // From 10 m out it could not: the dilemma zone, and going is right.
        assert_eq!(entry(10.0, 10.0), 0);
    }

    #[test]
    fn a_lane_change_at_the_stop_line_round_a_standing_queue_is_flagged() {
        let world = v2xw_world::procedural::grid(
            &GridParams {
                lanes_per_direction: 2,
                ..GridParams::legacy().with_signals(true)
            },
            &ImportOptions::default(),
        )
        .expect("grid");
        // A lane with a neighbour on the same edge.
        let (from, to) = world
            .roads
            .lanes()
            .iter()
            .filter(|l| l.kind == LaneKind::Driving && l.length_m > 60.0)
            .find_map(|l| {
                world.edge(l.edge).lanes.iter().copied().find(|o| {
                    let o = world.lane(*o);
                    o.id != l.id && (i32::from(o.index) - i32::from(l.index)).abs() == 1
                })
                .map(|o| (l.id, o))
            })
            .expect("a two-lane edge");
        let len = world.lane(from).length_m;
        let change = |start_s: f64| {
            let mut audit = TrafficAuditor::new(&world, AuditParams::default());
            let queued = at(&world, 1, from, len - 4.0, 0.0);
            let ego = at(&world, 0, from, start_s, 5.0);
            audit.observe(&world, 0, 100_000_000, &[ego, queued], &[]);
            let mut moved = at(&world, 0, to, start_s + 0.5, 5.0);
            moved.changing = Some((from, to));
            moved.lateral_m = -3.4;
            audit.observe(&world, 100_000_000, 200_000_000, &[moved, queued], &[]);
            let r = audit.report();
            (r.count(Check::LaneChangeNearJunction), r.count(Check::QueueJump))
        };
        // 20 m before the end, beside a queue at the line: both.
        assert_eq!(change(len - 20.0), (1, 1));
        // 50 m back: moving to the shorter queue, which is lawful.
        assert_eq!(change(len - 50.0), (0, 0));
    }

    #[test]
    fn the_world_checks_see_a_conflicting_green_and_a_lane_through_a_building() {
        let world = grid();
        // Give every movement of a four-way junction's plan a protected green in every
        // phase.
        let four_way = world
            .signals
            .iter()
            .position(|p| world.junction(p.junction).incoming.len() >= 4)
            .expect("a four-way signalised junction");
        let rebuilt = crate::worlds::rebuild_with(&world, |parts| {
            for phase in &mut parts.signals[four_way].phases {
                for s in &mut phase.states {
                    *s = SignalState::Green;
                }
            }
        })
        .expect("a world");
        assert!(
            audit_world(&rebuilt)
                .iter()
                .any(|(c, _)| *c == Check::WorldConflictingGreens)
        );
        // And put a building on a lane.
        let lane = straight_lane(&world).clone();
        let mid = lane.point_at(0.5 * lane.length_m);
        let ring = vec![
            Vec3::new(mid.x - 3.0, mid.y - 3.0, 0.0),
            Vec3::new(mid.x + 3.0, mid.y - 3.0, 0.0),
            Vec3::new(mid.x + 3.0, mid.y + 3.0, 0.0),
            Vec3::new(mid.x - 3.0, mid.y + 3.0, 0.0),
        ];
        let building = v2xw_world::model::Building::new(
            v2xw_core::ids::BuildingId::new(0),
            ring,
            Vec::<Vec<Vec3>>::new(),
            10.0,
            0.0,
            v2xw_world::model::MaterialClass::default(),
            v2xw_world::model::HeightSource::default(),
        )
        .expect("a building");
        let with_building = World::builder(world.origin)
            .roads(world.roads.clone())
            .signals(world.signals.clone())
            .buildings(vec![building])
            .provenance(world.provenance.clone())
            .build()
            .expect("a world");
        assert!(
            audit_world(&with_building)
                .iter()
                .any(|(c, _)| *c == Check::WorldLaneInBuilding)
        );
        assert!(
            !audit_world(&world)
                .iter()
                .any(|(c, _)| *c == Check::WorldLaneInBuilding)
        );
    }

    #[test]
    fn the_grid_world_passes_its_static_checks() {
        let world = grid();
        let found = audit_world(&world);
        assert!(found.is_empty(), "{:?}", &found[..found.len().min(5)]);
    }
}
