//! Crosswalks as the traffic model sees them: when a vehicle yields, and when a pedestrian
//! may step off the kerb.
//!
//! Both sides read one function, [`CrosswalkIndex::ahead`], which walks a vehicle's own
//! route and returns every crosswalk band on it with the distance to its near and far
//! edges. So "the vehicle can still stop" (the pedestrian's test) and "the vehicle must
//! stop" (the driver's) are measured on the same geometry and cannot disagree.
//!
//! # The rules and their sources
//!
//! | Rule | Source |
//! |---|---|
//! | A driver yields to a pedestrian in a crosswalk at an uncontrolled crossing — here the whole crosswalk, not only the driver's half | UVC §11-502(a) (the half-roadway form; many states, New York among them for a marked crosswalk, extend it) |
//! | A driver turning on a green yields to pedestrians lawfully in an adjacent crosswalk | UVC §11-202(a)1; NY VTL §1111(a)1 |
//! | A pedestrian does not leave the kerb into the path of a vehicle so close that it cannot yield | UVC §11-502(b) |
//! | A pedestrian starts to cross only on the WALKING PERSON indication; on flashing or steady UPRAISED HAND they do not start | UVC §11-203; MUTCD 2009 §4E.02 |
//! | A driver does not stop within a crosswalk | UVC §11-1003(a)1(e); NY VTL §1202(a)1(e) |
//! | The stop point is 4 ft (1.2 m) before the crosswalk | MUTCD 2009 §3B.16: a stop line "should be placed a minimum of 4 feet in advance of the nearest crosswalk line" |
//!
//! "Cannot yield" is judged with the deceleration most drivers brake at for something
//! unexpected, 3.4 m/s² (AASHTO *Green Book* 2018 §3.2.2, the engine's planned-stop limit)
//! after a reaction of [`PEDESTRIAN_REACTION_MARGIN_S`]. That reaction is **this crate's
//! choice, not a cited value**: the vehicle model reacts within one step, and the margin
//! makes a pedestrian wait for a vehicle that could stop only by braking the instant the
//! pedestrian moved.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::ids::{LaneId, SignalId};
use v2xw_world::walk::{Crosswalk, CrosswalkConflict, crosswalk_conflicts, crosswalks};
use v2xw_world::{LaneKind, SignalState, World};

use crate::views::PhaseState;

/// How far before a crosswalk's near edge a vehicle stops, metres: 4 ft (MUTCD 2009
/// §3B.16).
pub const STOP_BEFORE_CROSSWALK_M: f64 = 1.2;

/// The deceleration a pedestrian judges an approaching vehicle able to stop at, m/s²
/// (AASHTO *Green Book* 2018 §3.2.2).
pub const YIELD_DECEL_MPS2: f64 = 3.4;

/// The reaction a pedestrian allows an approaching driver, seconds. **Not cited**: see the
/// module documentation.
pub const PEDESTRIAN_REACTION_MARGIN_S: f64 = 1.0;

/// Below this speed a leader counts as standing, for the don't-stop-in-a-crosswalk rule,
/// m/s.
const STANDING_MPS: f64 = 1.0;

/// How far along its route a vehicle is examined for crosswalks, metres. Beyond the
/// stopping distance from 17 m/s (≈ 60 km/h) at 3.4 m/s² with the reaction margin.
const HORIZON_M: f64 = 80.0;

/// One crosswalk band on a vehicle's path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandAhead {
    /// Which crosswalk ([`CrosswalkIndex::crosswalk`]).
    pub crosswalk: usize,
    /// Distance from the vehicle's front to the band's near edge, metres; negative once
    /// the front is past it.
    pub enter_m: f64,
    /// Distance from the vehicle's front to the band's far edge, metres.
    pub exit_m: f64,
}

/// Where a vehicle is, as the crosswalk rules need it.
#[derive(Debug, Clone, Copy)]
pub struct VehiclePath<'a> {
    /// Its route's lanes.
    pub route: &'a [LaneId],
    /// Where it is on the route.
    pub route_index: usize,
    /// The lane its front is on.
    pub lane: LaneId,
    /// Front-bumper arc length on `lane`.
    pub s_m: f64,
    /// The lane it drove off to reach `lane`, while its body may still overhang it.
    pub prev_lane: Option<LaneId>,
    /// Body length.
    pub length_m: f64,
    /// Speed.
    pub speed_mps: f64,
}

/// Whether a pedestrian may step onto one crossing lane now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CrossingPermit {
    /// The pedestrian signal for this crossing lane, if a plan controls it.
    pub signal: Option<SignalState>,
    /// True if a vehicle is on the crosswalk or too close to yield to someone stepping
    /// onto it.
    pub hazard: bool,
}

/// Every crosswalk of a world, indexed for the two rules.
#[derive(Debug, Clone, Default)]
pub struct CrosswalkIndex {
    walks: Vec<Crosswalk>,
    by_lane: BTreeMap<LaneId, Vec<CrosswalkConflict>>,
    walk_of: BTreeMap<LaneId, usize>,
    signal_of: BTreeMap<LaneId, (SignalId, usize)>,
}

impl CrosswalkIndex {
    /// Builds the index of `world`'s crosswalks and the pedestrian signals that control
    /// them.
    pub fn build(world: &World) -> Self {
        let lanes = world.roads.lanes();
        let walks = crosswalks(lanes);
        let mut by_lane: BTreeMap<LaneId, Vec<CrosswalkConflict>> = BTreeMap::new();
        for c in crosswalk_conflicts(lanes, &walks) {
            by_lane.entry(c.lane).or_default().push(c);
        }
        let mut walk_of = BTreeMap::new();
        for (k, w) in walks.iter().enumerate() {
            for l in &w.lanes {
                walk_of.insert(*l, k);
            }
        }
        let mut signal_of = BTreeMap::new();
        for plan in &world.signals {
            for (i, l) in plan.controlled.iter().enumerate() {
                if world.try_lane(*l).map(|x| x.kind) == Some(LaneKind::Crossing) {
                    signal_of.insert(*l, (plan.id, i));
                }
            }
        }
        Self {
            walks,
            by_lane,
            walk_of,
            signal_of,
        }
    }

    /// How many crosswalks there are.
    pub fn len(&self) -> usize {
        self.walks.len()
    }

    /// True if the world has none.
    pub fn is_empty(&self) -> bool {
        self.walks.is_empty()
    }

    /// One crosswalk.
    pub fn crosswalk(&self, k: usize) -> Option<&Crosswalk> {
        self.walks.get(k)
    }

    /// The crosswalk a crossing lane belongs to.
    pub fn crosswalk_of(&self, crossing_lane: LaneId) -> Option<usize> {
        self.walk_of.get(&crossing_lane).copied()
    }

    /// The crosswalk bands a driven lane has, by arc length.
    pub fn conflicts_on(&self, lane: LaneId) -> &[CrosswalkConflict] {
        self.by_lane.get(&lane).map_or(&[], Vec::as_slice)
    }

    /// Every crosswalk band on the vehicle's path from its rear to [`HORIZON_M`] ahead of
    /// its front, nearest first.
    pub fn ahead(&self, world: &World, v: &VehiclePath<'_>) -> Vec<BandAhead> {
        let mut out = Vec::new();
        if self.walks.is_empty() {
            return out;
        }
        // The lane it has just left, while the body overhangs it.
        if let Some(prev) = v.prev_lane
            && v.s_m < v.length_m
            && let Some(l) = world.try_lane(prev)
        {
            for c in self.conflicts_on(prev) {
                let base = -(v.s_m + (l.length_m - c.s_m));
                push(&mut out, c, base, v.length_m);
            }
        }
        // Its own lane, then the route ahead.
        let mut offset = -v.s_m;
        let mut lane = Some(v.lane);
        let mut k = v.route_index;
        while let Some(id) = lane {
            let Some(l) = world.try_lane(id) else { break };
            for c in self.conflicts_on(id) {
                push(&mut out, c, offset + c.s_m, v.length_m);
            }
            offset += l.length_m;
            if offset > HORIZON_M {
                break;
            }
            k += 1;
            lane = v.route.get(k).copied();
            // A vehicle not at its route position (the route index lags a lane change)
            // still looks along the route from the next lane.
            if k == v.route_index + 1 && v.route.get(v.route_index) != Some(&v.lane) {
                lane = v
                    .route
                    .iter()
                    .position(|r| *r == v.lane)
                    .and_then(|i| v.route.get(i + 1))
                    .copied();
            }
        }
        out.sort_by(|a, b| a.enter_m.total_cmp(&b.enter_m));
        out
    }

    /// Which crosswalks have a pedestrian on them.
    pub fn occupied(&self, pedestrian_lanes: impl IntoIterator<Item = LaneId>) -> BTreeSet<usize> {
        pedestrian_lanes
            .into_iter()
            .filter_map(|l| self.walk_of.get(&l).copied())
            .collect()
    }

    /// The virtual obstacle a crosswalk puts in front of a vehicle, as a gap from its
    /// front, metres — or `None` if no crosswalk rule stops it.
    ///
    /// * **Yield**: a crosswalk with someone on it, whose near edge is still ahead.
    /// * **Don't stop in it**: a crosswalk the vehicle could not clear because what is
    ///   ahead of it (`leader`: gap and speed) stands inside the band or less than a body
    ///   length and `min_gap_m` beyond it — taken only while the vehicle can still stop
    ///   before it at [`YIELD_DECEL_MPS2`].
    pub fn stop_gap(
        &self,
        bands: &[BandAhead],
        occupied: &BTreeSet<usize>,
        v: &VehiclePath<'_>,
        leader: Option<(f64, f64)>,
        min_gap_m: f64,
    ) -> Option<f64> {
        for b in bands {
            if b.enter_m <= 0.0 {
                continue;
            }
            let gap = (b.enter_m - STOP_BEFORE_CROSSWALK_M).max(0.0);
            if occupied.contains(&b.crosswalk) {
                return Some(gap);
            }
            if let Some((lead_gap, lead_speed)) = leader
                && lead_speed <= STANDING_MPS
                && lead_gap < b.exit_m + v.length_m + min_gap_m
                && lead_gap >= b.enter_m - STOP_BEFORE_CROSSWALK_M
                && v.speed_mps * v.speed_mps <= 2.0 * YIELD_DECEL_MPS2 * gap.max(0.01)
            {
                return Some(gap);
            }
        }
        None
    }

    /// The crosswalks a pedestrian may not step onto because of this vehicle: the one its
    /// body is on, and every one it could not stop before.
    pub fn hazards_from(&self, bands: &[BandAhead], speed_mps: f64, out: &mut BTreeSet<usize>) {
        let stop = speed_mps * PEDESTRIAN_REACTION_MARGIN_S
            + speed_mps * speed_mps / (2.0 * YIELD_DECEL_MPS2)
            + STOP_BEFORE_CROSSWALK_M;
        for b in bands {
            let on_it = b.enter_m <= 0.0 && b.exit_m > 0.0;
            if on_it || (b.enter_m > 0.0 && b.enter_m <= stop) {
                out.insert(b.crosswalk);
            }
        }
    }

    /// Each crossing lane's permit: its pedestrian signal, from the plans' states at the
    /// step's start, and whether a vehicle makes stepping onto it a hazard.
    pub fn permits(
        &self,
        signal_states: &[(SignalId, PhaseState)],
        hazards: &BTreeSet<usize>,
    ) -> BTreeMap<LaneId, CrossingPermit> {
        let states: BTreeMap<SignalId, &PhaseState> =
            signal_states.iter().map(|(id, s)| (*id, s)).collect();
        self.walk_of
            .iter()
            .map(|(lane, k)| {
                let signal = self
                    .signal_of
                    .get(lane)
                    .and_then(|(plan, i)| states.get(plan).and_then(|s| s.states.get(*i)))
                    .copied();
                (
                    *lane,
                    CrossingPermit {
                        signal,
                        hazard: hazards.contains(k),
                    },
                )
            })
            .collect()
    }
}

/// Adds one band, `base` metres from the vehicle's front to the crossing point, if any of
/// it is still ahead of the vehicle's rear.
fn push(out: &mut Vec<BandAhead>, c: &CrosswalkConflict, base: f64, length_m: f64) {
    let enter = base - c.half_extent_m;
    let exit = base + c.half_extent_m;
    if exit + length_m <= 0.0 {
        return;
    }
    out.push(BandAhead {
        crosswalk: c.crosswalk,
        enter_m: enter,
        exit_m: exit + length_m.min(0.0),
    });
}
