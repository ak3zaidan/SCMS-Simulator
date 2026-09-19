//! `mobility/routing/dynamic-reroute` — re-planning (04-models.md §2.4).
//!
//! The same search as [`crate::routing::dijkstra`], run again at the next junction when
//! something has changed: a closure event, a travel-time update, or a periodic timer. The
//! policy is [`ReroutePolicy`] and the engine asks [`DynamicReroute::due`] once per step per
//! vehicle.
//!
//! # Why re-plan at the *next junction* and not immediately
//!
//! A vehicle halfway down a lane cannot act on a new route until it reaches the end of that
//! lane, so replanning earlier would only make the route it is *displaying* disagree with
//! the road it is on. The engine therefore re-plans from the vehicle's current lane, keeps
//! the prefix it has already driven, and the new route's first lane is always the one the
//! vehicle is on.
//!
//! # The cost function
//!
//! [`DynamicCost`] is the mutable [`EdgeCost`] the scenario drives: free-flow by default,
//! `None` (impassable) for a closed lane, and an observed travel time where one has been
//! recorded. It is the one place a closure lives, so a closure is invisible to the search
//! except as a missing arc — which is exactly what §2.4's "closures raise cost to infinity"
//! means without putting an infinity into the arithmetic.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::card::ModelCard;
use v2xw_core::ids::LaneId;
use v2xw_core::time::{Duration, SimTime};
use v2xw_world::World;

use crate::ctx::MobCtx;
use crate::routing::dijkstra::{Dijkstra, DijkstraParams, card};
use crate::traits::Router;
use crate::views::{EdgeCost, ReroutePolicy, Route};

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// Free-flow costs, with closures and observed travel times on top.
#[derive(Debug, Clone)]
pub struct DynamicCost<'a> {
    world: &'a World,
    closed: BTreeSet<LaneId>,
    observed_s: BTreeMap<LaneId, f64>,
    /// Bumped whenever a closure or an observation changes, so a vehicle can tell whether
    /// anything has changed since it last planned.
    generation: u64,
}

impl<'a> DynamicCost<'a> {
    /// Free-flow costs over `world`, nothing closed and nothing observed.
    pub fn new(world: &'a World) -> Self {
        Self {
            world,
            closed: BTreeSet::new(),
            observed_s: BTreeMap::new(),
            generation: 0,
        }
    }

    /// Closes or reopens `lane`. Returns `true` if this changed anything.
    pub fn set_closed(&mut self, lane: LaneId, closed: bool) -> bool {
        let changed = if closed {
            self.closed.insert(lane)
        } else {
            self.closed.remove(&lane)
        };
        if changed {
            self.generation += 1;
        }
        changed
    }

    /// Records an observed travel time for `lane`, seconds. `None` forgets it.
    pub fn observe(&mut self, lane: LaneId, travel_time_s: Option<f64>) {
        let changed = match travel_time_s {
            Some(t) if t.is_finite() && t >= 0.0 => self.observed_s.insert(lane, t) != Some(t),
            _ => self.observed_s.remove(&lane).is_some(),
        };
        if changed {
            self.generation += 1;
        }
    }

    /// True if `lane` is closed.
    pub fn is_closed(&self, lane: LaneId) -> bool {
        self.closed.contains(&lane)
    }

    /// Every closed lane, in lane-id order.
    pub fn closures(&self) -> impl Iterator<Item = LaneId> + '_ {
        self.closed.iter().copied()
    }

    /// The generation counter: it changes exactly when a closure or an observation does.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl EdgeCost for DynamicCost<'_> {
    fn lane_cost_s(&self, lane: LaneId, _at: SimTime) -> Option<f64> {
        if self.closed.contains(&lane) {
            return None;
        }
        if let Some(observed) = self.observed_s.get(&lane) {
            return Some(*observed);
        }
        let l = self.world.try_lane(lane)?;
        if l.speed_limit_mps <= 0.0 {
            return None;
        }
        Some(l.length_m / l.speed_limit_mps)
    }
}

/// Dijkstra with a re-planning policy.
#[derive(Debug, Clone)]
pub struct DynamicReroute {
    inner: Dijkstra,
    policy: ReroutePolicy,
    card: ModelCard,
}

impl DynamicReroute {
    /// The router with the given parameters and policy.
    pub fn new(params: DijkstraParams, policy: ReroutePolicy) -> Self {
        Self {
            inner: Dijkstra::new(params),
            card: card(&params, policy),
            policy,
        }
    }

    /// The default: re-plan on a closure and on a travel-time change, never periodically.
    pub fn on_change(params: DijkstraParams) -> Self {
        Self::new(
            params,
            ReroutePolicy {
                on_closure: true,
                on_travel_time_change: true,
                periodic: None,
            },
        )
    }

    /// The underlying search.
    pub fn dijkstra(&self) -> &Dijkstra {
        &self.inner
    }

    /// Whether a vehicle that last planned at `planned_at` with cost generation
    /// `planned_generation` should re-plan now.
    ///
    /// Pure arithmetic on the policy, the two generations and the two instants: no state,
    /// so two engines agree on when a vehicle re-plans.
    pub fn due(
        &self,
        now: SimTime,
        planned_at: SimTime,
        planned_generation: u64,
        current_generation: u64,
    ) -> bool {
        let changed = current_generation != planned_generation;
        if changed && (self.policy.on_closure || self.policy.on_travel_time_change) {
            return true;
        }
        match self.policy.periodic {
            Some(every) if !every.is_zero() => {
                Duration::between(planned_at, now).as_nanos() >= every.as_nanos()
            }
            _ => false,
        }
    }

    /// Re-plans from `from` to `to`, keeping the vehicle on the lane it is on: the returned
    /// route's first lane is `from`.
    pub fn replan(
        &self,
        world: &World,
        from: LaneId,
        to: LaneId,
        at: SimTime,
        costs: &dyn EdgeCost,
    ) -> Option<Route> {
        self.inner.search(world, from, to, at, costs)
    }
}

impl v2xw_core::model::Model for DynamicReroute {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Router for DynamicReroute {
    fn route(
        &self,
        ctx: &mut dyn MobCtx,
        from: LaneId,
        to: LaneId,
        at: SimTime,
        costs: &dyn EdgeCost,
    ) -> Option<Route> {
        self.inner.search(ctx.world(), from, to, at, costs)
    }

    fn reroute_policy(&self) -> ReroutePolicy {
        self.policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::model::Model;
    use v2xw_world::{ImportOptions, LaneKind, procedural::GridParams};

    fn world() -> World {
        v2xw_world::procedural::grid(&GridParams::legacy(), &ImportOptions::default())
            .expect("grid")
    }

    fn drivable(world: &World) -> Vec<LaneId> {
        world
            .roads
            .lanes()
            .iter()
            .filter(|l| l.kind == LaneKind::Driving)
            .map(|l| l.id)
            .collect()
    }

    #[test]
    fn a_closure_diverts_the_route() {
        let w = world();
        let r = DynamicReroute::on_change(DijkstraParams::default());
        let lanes = drivable(&w);
        let mut costs = DynamicCost::new(&w);
        let before = r
            .replan(&w, lanes[0], *lanes.last().unwrap(), 0, &costs)
            .expect("a route");
        assert!(before.lanes.len() > 2);
        // Close a lane in the middle of that route and re-plan: the new route avoids it.
        let victim = before.lanes[before.lanes.len() / 2];
        assert!(costs.set_closed(victim, true));
        let after = r
            .replan(&w, lanes[0], *lanes.last().unwrap(), 0, &costs)
            .expect("a diverted route");
        assert!(!after.lanes.contains(&victim), "the closed lane is avoided");
        assert!(
            after.cost_s >= before.cost_s - 1e-9,
            "a detour is not cheaper"
        );
        // Reopening restores the original.
        assert!(costs.set_closed(victim, false));
        let restored = r
            .replan(&w, lanes[0], *lanes.last().unwrap(), 0, &costs)
            .expect("a route");
        assert_eq!(restored.lanes, before.lanes);
    }

    #[test]
    fn a_travel_time_update_changes_the_best_route() {
        let w = world();
        let r = DynamicReroute::on_change(DijkstraParams::default());
        let lanes = drivable(&w);
        let mut costs = DynamicCost::new(&w);
        let before = r
            .replan(&w, lanes[0], *lanes.last().unwrap(), 0, &costs)
            .expect("a route");
        // Make one lane of the route very slow: the router prefers another way round.
        let victim = before.lanes[1];
        costs.observe(victim, Some(10_000.0));
        let after = r
            .replan(&w, lanes[0], *lanes.last().unwrap(), 0, &costs)
            .expect("a route");
        assert_ne!(after.lanes, before.lanes);
    }

    #[test]
    fn the_policy_decides_when_to_replan() {
        let params = DijkstraParams::default();
        let on_change = DynamicReroute::on_change(params);
        assert!(!on_change.due(1_000, 0, 3, 3), "nothing changed");
        assert!(on_change.due(1_000, 0, 3, 4), "the cost generation moved");

        let periodic = DynamicReroute::new(
            params,
            ReroutePolicy {
                on_closure: false,
                on_travel_time_change: false,
                periodic: Some(Duration::from_secs(60)),
            },
        );
        assert!(
            !periodic.due(30 * v2xw_core::time::NS_PER_S, 0, 1, 2),
            "too soon"
        );
        assert!(
            periodic.due(60 * v2xw_core::time::NS_PER_S, 0, 1, 1),
            "the timer fired"
        );

        let never = DynamicReroute::new(params, ReroutePolicy::STATIC);
        assert!(!never.due(u64::MAX / 2, 0, 1, 99));
    }

    #[test]
    fn a_closed_lane_has_no_cost_at_all() {
        let w = world();
        let lane = drivable(&w)[0];
        let mut costs = DynamicCost::new(&w);
        assert!(costs.lane_cost_s(lane, 0).is_some());
        costs.set_closed(lane, true);
        assert!(costs.lane_cost_s(lane, 0).is_none());
        assert!(costs.is_closed(lane));
        assert_eq!(costs.closures().collect::<Vec<_>>(), vec![lane]);
    }

    #[test]
    fn the_generation_moves_only_on_a_real_change() {
        let w = world();
        let lane = drivable(&w)[0];
        let mut costs = DynamicCost::new(&w);
        let g0 = costs.generation();
        assert!(
            !costs.set_closed(lane, false),
            "reopening an open lane changes nothing"
        );
        assert_eq!(costs.generation(), g0);
        costs.set_closed(lane, true);
        let g1 = costs.generation();
        assert_ne!(g1, g0);
        costs.observe(lane, Some(12.0));
        assert_ne!(costs.generation(), g1);
        let g2 = costs.generation();
        costs.observe(lane, Some(12.0));
        assert_eq!(
            costs.generation(),
            g2,
            "the same observation twice is one change"
        );
    }

    #[test]
    fn the_card_is_the_dynamic_one_and_validates() {
        let r = DynamicReroute::on_change(DijkstraParams::default());
        r.card().validate().expect("validates");
        assert_eq!(r.card().id, "mobility/routing/dynamic-reroute");
        assert!(r.reroute_policy().on_closure);
    }
}
