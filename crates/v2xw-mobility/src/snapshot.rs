//! The start-of-step snapshot, and the one neighbour query every model shares.
//!
//! # Why a snapshot at all
//!
//! ADR 0004 decision 2 makes mobility a periodic phase, and decision 5 makes phases pure
//! maps over actors. A car-following model that read its leader's *already updated* speed
//! would break both: the result would depend on the order the actors happened to be
//! iterated in, so it would change with the container, with the thread count and with the
//! id assignment. That is a Gauss-Seidel update, and it is not reproducible.
//!
//! So every step begins by freezing what every actor looked like at its start. Every model
//! reads only the frozen copy and every result is written to a separate buffer, which is
//! published at the end of the step: the **Jacobi update**. Its observable property is the
//! one [`crate::engine`]'s test asserts directly — processing the actors in reverse order
//! produces bit-identical output. The legacy engine did the same thing for the same reason
//! (`snap` in [`run.py` L2455]), and this is the port of that idea onto the lane graph.
//!
//! # What the query walks
//!
//! Two structures, both rebuilt per step (ADR 0004 decision 6):
//!
//! * **Per-lane ordered lists.** Each lane's occupants are kept sorted by
//!   `(front arc length, actor id)`, so a leader search is a binary search and the
//!   answer never depends on insertion order. The search then continues *downstream along
//!   the ego's route*, over the world's own [`v2xw_world::Connection`] graph — which is
//!   directed, one edge per direction of travel, so everything the walk finds is travelling
//!   the same way as the ego. That is how "same-direction only" is guaranteed here: not by
//!   a heading window that can be fooled, but by never leaving the ego's own direction of
//!   the graph.
//! * **A uniform grid** over actor positions, cell size equal to the longest range any
//!   consumer queries (ADR 0004 decision 6), for the radius queries that are not
//!   lane-shaped: pedestrian repulsion, junction claimants, perception.
//!
//! Lanes themselves are found through the world's static index
//! ([`v2xw_world::World::nearest_lane_within`]), which is what places a spawning vehicle or
//! a wandering pedestrian on a lane in the first place.

use std::collections::BTreeMap;

use v2xw_core::geom::Vec3;
use v2xw_core::grid::{GridCell, GridIndex};
use v2xw_core::ids::{ActorId, LaneId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::time::SimTime;
use v2xw_world::{ClassMask, LaneKind, World};

use crate::views::{LaneNeighbors, LeaderView, Side, SideNeighbors, VehicleView};

/// One actor as the snapshot froze it.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotEntry {
    /// What the models read.
    pub view: VehicleView,
    /// The published ground truth, for consumers that want the full state.
    pub kinematics: Kinematics,
}

/// How far and how wide a neighbour query looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NeighborOptions {
    /// How far downstream to look for a leader, metres.
    ///
    /// The legacy engine's value is 70 m (`idm_lookahead_m`, [`run.py` L2455-2462]); a
    /// motorway parameter set wants more, because at 33 m/s a 70 m lookahead is 2.1 s.
    pub lookahead_m: f64,
    /// How many lane hops the downstream walk may make. A bound, not a model parameter:
    /// it stops the walk on a pathological world (a cycle of 1 m connectors).
    pub max_lane_hops: usize,
    /// The classes the ego may legally use, so the walk never crosses onto a lane it
    /// could not drive on.
    pub classes: ClassMask,
    /// Whether to classify the adjacent lanes as well. The lane-change model needs it;
    /// a single-lane road does not.
    pub sides: bool,
}

impl Default for NeighborOptions {
    /// The legacy lookahead, eight hops, motor traffic, sides included.
    fn default() -> Self {
        Self {
            lookahead_m: 70.0,
            max_lane_hops: 8,
            classes: ClassMask::MOTOR_TRAFFIC,
            sides: true,
        }
    }
}

/// Everything the models may read about the other actors during one step.
#[derive(Debug, Clone)]
pub struct ActorSnapshot {
    t: SimTime,
    entries: Vec<SnapshotEntry>,
    index_of: BTreeMap<ActorId, usize>,
    by_lane: BTreeMap<LaneId, Vec<(f64, ActorId)>>,
    grid: GridIndex,
    cells: BTreeMap<GridCell, Vec<ActorId>>,
}

impl ActorSnapshot {
    /// An empty snapshot for the instant `t`, with `cell_size_m` grid cells.
    pub fn new(t: SimTime, cell_size_m: f64) -> Self {
        Self {
            t,
            entries: Vec::new(),
            index_of: BTreeMap::new(),
            by_lane: BTreeMap::new(),
            grid: GridIndex::new(cell_size_m),
            cells: BTreeMap::new(),
        }
    }

    /// Freezes `actors` into a snapshot for `t`.
    ///
    /// The input order does not matter: everything inside is sorted by id or by
    /// `(arc length, id)`, which is what makes the query order-independent.
    pub fn build(
        t: SimTime,
        cell_size_m: f64,
        actors: impl IntoIterator<Item = (VehicleView, Kinematics)>,
    ) -> Self {
        let mut s = Self::new(t, cell_size_m);
        for (view, k) in actors {
            s.push(view, k);
        }
        s.sort();
        s
    }

    /// Adds one actor. [`ActorSnapshot::sort`] must be called before any query.
    pub fn push(&mut self, view: VehicleView, kinematics: Kinematics) {
        let actor = view.actor;
        let lane = view.lane;
        let s_m = view.s_m;
        let pos = kinematics.pos;
        self.entries.push(SnapshotEntry { view, kinematics });
        self.by_lane.entry(lane).or_default().push((s_m, actor));
        self.cells
            .entry(self.grid.cell_of(pos))
            .or_default()
            .push(actor);
    }

    /// Registers `actor` as an occupant of `lane` at front arc length `s_m` as well as of
    /// its own lane — a vehicle whose body still straddles the lane it is changing out of.
    /// The actor must already have been [`push`](ActorSnapshot::push)ed;
    /// [`ActorSnapshot::sort`] must be called before any query.
    pub fn push_ghost(&mut self, lane: LaneId, s_m: f64, actor: ActorId) {
        self.by_lane.entry(lane).or_default().push((s_m, actor));
    }

    /// Puts every list in its canonical order. Idempotent.
    pub fn sort(&mut self) {
        self.entries.sort_by_key(|e| e.view.actor);
        self.index_of = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.view.actor, i))
            .collect();
        for list in self.by_lane.values_mut() {
            // Total order on (arc length, id): `f64` needs a total-order sort, and the id
            // breaks the tie two vehicles at the same arc length would otherwise leave to
            // chance.
            list.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        }
        for list in self.cells.values_mut() {
            list.sort_unstable();
        }
    }

    /// The instant this snapshot froze.
    pub fn t(&self) -> SimTime {
        self.t
    }

    /// How many actors it holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if it holds none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The grid cell size, metres.
    pub fn cell_size_m(&self) -> f64 {
        self.grid.cell_size_m()
    }

    /// One actor's frozen entry.
    pub fn get(&self, a: ActorId) -> Option<&SnapshotEntry> {
        self.index_of.get(&a).map(|i| &self.entries[*i])
    }

    /// One actor's view.
    pub fn view(&self, a: ActorId) -> Option<&VehicleView> {
        self.get(a).map(|e| &e.view)
    }

    /// One actor's ground truth.
    pub fn kinematics(&self, a: ActorId) -> Option<&Kinematics> {
        self.get(a).map(|e| &e.kinematics)
    }

    /// Every entry, in actor-id order (invariant I-M1).
    pub fn iter(&self) -> impl Iterator<Item = &SnapshotEntry> + Clone {
        self.entries.iter()
    }

    /// The occupants of one lane, as `(front arc length, actor)`, sorted.
    pub fn on_lane(&self, lane: LaneId) -> &[(f64, ActorId)] {
        self.by_lane.get(&lane).map_or(&[][..], |v| v.as_slice())
    }

    /// Every lane that holds at least one actor, in lane-id order.
    pub fn occupied_lanes(&self) -> impl Iterator<Item = LaneId> + '_ {
        self.by_lane.keys().copied()
    }

    /// Every actor within `radius_m` of `p`, in actor-id order.
    ///
    /// The grid is scanned cell by cell in the documented neighbourhood order and the
    /// result is sorted, so the answer does not depend on the cell size — only the cost
    /// does.
    pub fn actors_within(&self, p: Vec3, radius_m: f64) -> Vec<ActorId> {
        let mut out = Vec::new();
        for cell in self.grid.cells_within(p, radius_m) {
            let Some(list) = self.cells.get(&cell) else {
                continue;
            };
            for a in list {
                let Some(e) = self.get(*a) else { continue };
                if e.kinematics.pos.distance_2d(p) <= radius_m {
                    out.push(*a);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    // -----------------------------------------------------------------------
    // The one neighbour query
    // -----------------------------------------------------------------------

    /// Classifies everything around `ego`: the leader on its route, its follower, and the
    /// leader and follower on each usable adjacent lane.
    ///
    /// This is the **single** query 04-models.md §2.2 requires the car-following and
    /// lane-change models to share. `route` is the ego's lane sequence and `route_index`
    /// the position of `ego.lane` in it; an empty route means "follow the only successor
    /// there is", which is what an abstract-tier actor on a chain of lanes does.
    pub fn neighbors(
        &self,
        world: &World,
        ego: &VehicleView,
        route: &[LaneId],
        route_index: usize,
        opts: NeighborOptions,
    ) -> LaneNeighbors {
        let mut out = LaneNeighbors::empty(ego.lane);
        out.leader = self.leader_on_route(world, ego, route, route_index, opts);
        out.follower = self.follower_behind(world, ego, opts);
        if opts.sides {
            out.left = self.side(world, ego, Side::Left, opts);
            out.right = self.side(world, ego, Side::Right, opts);
        }
        out
    }

    /// Just the leader — the car-following model's half of [`ActorSnapshot::neighbors`].
    ///
    /// Kept separate so a single-lane run does not pay for the side classification, and so
    /// the two halves are visibly the same walk.
    pub fn leader_on_route(
        &self,
        world: &World,
        ego: &VehicleView,
        route: &[LaneId],
        route_index: usize,
        opts: NeighborOptions,
    ) -> Option<LeaderView> {
        // Leg one: the ego's own lane, ahead of the ego.
        if let Some((s_front, actor)) = self.first_ahead(ego.lane, ego.s_m, Some(ego.actor)) {
            let cand = &self.get(actor)?.view;
            let gap = (s_front - cand.dims.length_m) - ego.s_m;
            if gap <= opts.lookahead_m {
                return Some(LeaderView::of(*cand, gap.max(0.0)));
            }
            return None;
        }

        // Legs two and on: downstream lanes, in route order.
        let mut base = world.try_lane(ego.lane)?.length_m - ego.s_m;
        let mut lane = ego.lane;
        let mut index = route_index;
        for _ in 0..opts.max_lane_hops {
            if base > opts.lookahead_m {
                return None;
            }
            let next = self.next_lane(world, lane, route, index, opts.classes)?;
            index = index.saturating_add(1);
            if let Some((s_front, actor)) = self.first_ahead(next, f64::NEG_INFINITY, None) {
                let cand = &self.get(actor)?.view;
                let gap = base + (s_front - cand.dims.length_m);
                if gap <= opts.lookahead_m {
                    return Some(LeaderView::of(*cand, gap.max(0.0)));
                }
                return None;
            }
            base += world.try_lane(next)?.length_m;
            lane = next;
        }
        None
    }

    /// The vehicle behind the ego, on its lane or one lane upstream.
    fn follower_behind(
        &self,
        world: &World,
        ego: &VehicleView,
        opts: NeighborOptions,
    ) -> Option<LeaderView> {
        if let Some((s_front, actor)) = self.last_behind(ego.lane, ego.s_m, Some(ego.actor)) {
            let cand = &self.get(actor)?.view;
            let gap = ego.rear_s_m() - s_front;
            return Some(LeaderView::of(*cand, gap.max(0.0)));
        }
        // One hop upstream: whoever is closest across every lane that feeds this one.
        let mut best: Option<(f64, ActorId)> = None;
        for c in world.predecessors(ego.lane) {
            if !c.permitted {
                continue;
            }
            let from = c.via.unwrap_or(c.from_lane);
            let Some(lane) = world.try_lane(from) else {
                continue;
            };
            if !lane.admits(opts.classes) {
                continue;
            }
            if let Some((s_front, actor)) = self.last_behind(from, f64::INFINITY, Some(ego.actor)) {
                let gap = ego.rear_s_m().max(0.0) + (lane.length_m - s_front);
                if gap > opts.lookahead_m {
                    continue;
                }
                let better = match best {
                    None => true,
                    Some((g, a)) => gap < g || (gap == g && actor < a),
                };
                if better {
                    best = Some((gap, actor));
                }
            }
        }
        let (gap, actor) = best?;
        Some(LeaderView::of(self.get(actor)?.view, gap.max(0.0)))
    }

    /// The adjacent lane on `side` and its two neighbours, when the ego may use it.
    fn side(
        &self,
        world: &World,
        ego: &VehicleView,
        side: Side,
        opts: NeighborOptions,
    ) -> Option<SideNeighbors> {
        let lane = self.adjacent_lane(world, ego, side, opts.classes)?;
        let leader =
            self.first_ahead(lane, ego.s_m, Some(ego.actor))
                .and_then(|(s_front, actor)| {
                    let cand = &self.get(actor)?.view;
                    let gap = (s_front - cand.dims.length_m) - ego.s_m;
                    (gap <= opts.lookahead_m).then(|| LeaderView::of(*cand, gap.max(0.0)))
                });
        let follower =
            self.last_behind(lane, ego.s_m, Some(ego.actor))
                .and_then(|(s_front, actor)| {
                    let cand = &self.get(actor)?.view;
                    let gap = ego.rear_s_m() - s_front;
                    (gap <= opts.lookahead_m).then(|| LeaderView::of(*cand, gap.max(0.0)))
                });
        Some(SideNeighbors {
            lane,
            side,
            leader,
            follower,
        })
    }

    /// The lane next to the ego's on `side`, if the world has one the ego may use.
    ///
    /// Adjacency is *within one edge*: an edge is one direction of travel
    /// (04-models.md §1.1), its lanes are ordered by index with `0` rightmost, so the lane
    /// on the left is `index + 1`. This is also where a car is stopped from changing onto a
    /// sidewalk or a cycle track: the candidate must admit the ego's classes **and** be a
    /// lane the ego's kind of traffic belongs on.
    pub fn adjacent_lane(
        &self,
        world: &World,
        ego: &VehicleView,
        side: Side,
        classes: ClassMask,
    ) -> Option<LaneId> {
        let lane = world.try_lane(ego.lane)?;
        if lane.kind == LaneKind::Internal {
            return None; // no lane changes inside a junction
        }
        let edge = world.edge(lane.edge);
        let want = match side {
            Side::Left => i32::from(lane.index) + 1,
            Side::Right => i32::from(lane.index) - 1,
        };
        if want < 0 {
            return None;
        }
        let want = u8::try_from(want).ok()?;
        for id in &edge.lanes {
            let cand = world.lane(*id);
            if cand.index != want {
                continue;
            }
            if !cand.admits(classes) || !cand.kind.is_motorised() {
                return None;
            }
            return Some(cand.id);
        }
        None
    }

    /// The next lane after `lane`: the route's if there is one, else the only permitted
    /// successor the ego may use, else nothing.
    fn next_lane(
        &self,
        world: &World,
        lane: LaneId,
        route: &[LaneId],
        index: usize,
        classes: ClassMask,
    ) -> Option<LaneId> {
        if let Some(next) = route.get(index + 1) {
            return Some(*next);
        }
        if !route.is_empty() {
            return None; // the route ends here
        }
        let mut only: Option<LaneId> = None;
        for c in world.successors(lane) {
            if !c.permitted {
                continue;
            }
            let next = c.via.unwrap_or(c.to_lane);
            if next == lane || !world.lane(next).admits(classes) {
                continue;
            }
            match only {
                None => only = Some(next),
                Some(prev) if prev == next => {}
                Some(_) => return None, // ambiguous without a route: do not guess
            }
        }
        only
    }

    /// The first occupant of `lane` whose front is strictly beyond `s_m`.
    fn first_ahead(&self, lane: LaneId, s_m: f64, skip: Option<ActorId>) -> Option<(f64, ActorId)> {
        let list = self.on_lane(lane);
        let start = list.partition_point(|(s, _)| *s <= s_m);
        list[start..]
            .iter()
            .find(|(_, a)| Some(*a) != skip)
            .copied()
    }

    /// The last occupant of `lane` whose front is at or before `s_m`.
    fn last_behind(&self, lane: LaneId, s_m: f64, skip: Option<ActorId>) -> Option<(f64, ActorId)> {
        let list = self.on_lane(lane);
        let end = list.partition_point(|(s, _)| *s <= s_m);
        list[..end]
            .iter()
            .rev()
            .find(|(_, a)| Some(*a) != skip)
            .copied()
    }
}
