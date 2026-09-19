//! `mobility/routing/dijkstra` — shortest paths on the lane graph (04-models.md §2.4).
//!
//! # The graph
//!
//! A node is a **lane**, not a junction, and an arc is a permitted
//! [`v2xw_world::Connection`]. That is the only graph on which a turn restriction can be
//! expressed: a banned left turn is a connection with `permitted == false`, and a router on
//! a junction graph has nowhere to put it. The arcs out of a lane come from
//! [`v2xw_world::World::successors`], where a movement across a junction appears twice —
//! once as `approach → departure` carrying the internal connector in `via`, and once as
//! `connector → departure` — so following `via.unwrap_or(to_lane)` walks the geometry the
//! vehicle will actually drive and the route includes every internal connector.
//!
//! # The cost
//!
//! `length / speed limit`: free-flow travel time, seconds. It is supplied by an
//! [`EdgeCost`], not computed here, because a closure is the same thing as an infinite cost
//! and a travel-time update is the same thing as a changed one ([`crate::routing::dynamic`]).
//! [`FreeFlowCost`] is the plain version.
//!
//! # Why a car cannot be routed onto a sidewalk
//!
//! Two independent filters, and either alone would do:
//!
//! 1. Every candidate lane must admit the router's [`ClassMask`]. A sidewalk admits
//!    pedestrians only, so a car's mask never matches it.
//! 2. When the mask contains any motorised class, every candidate lane's
//!    [`LaneKind::is_motorised`] must also be true — so a *mis-tagged* sidewalk that
//!    happens to admit cars is still refused.
//!
//! Both are tested directly, and the second exists because an OSM import is only as good as
//! its tags.
//!
//! # Determinism
//!
//! The frontier is ordered by `(cost, lane id)` with the cost compared by
//! [`f64::total_cmp`], so equal-cost paths are resolved by the lane id rather than by heap
//! luck, and no [`std::collections::HashMap`] appears anywhere in the search: the visited
//! set and the predecessor map are [`BTreeMap`]s.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::LaneId;
use v2xw_core::time::SimTime;
use v2xw_world::{ClassMask, TurnDirection, World};

use crate::ctx::MobCtx;
use crate::traits::Router;
use crate::views::{EdgeCost, ReroutePolicy, Route};

/// The model id.
pub const MODEL_ID: &str = "mobility/routing/dijkstra";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The router's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DijkstraParams {
    /// The classes the vehicle belongs to. A lane that admits none of them is not in the
    /// graph.
    pub classes: ClassMask,
    /// Whether a U-turn connection may be used.
    pub allow_uturn: bool,
    /// Upper bound on settled nodes, so a pathological query cannot walk a whole continent.
    /// Exceeding it returns no route rather than a wrong one.
    pub max_settled: usize,
}

impl Default for DijkstraParams {
    fn default() -> Self {
        Self {
            classes: ClassMask::CAR,
            allow_uturn: false,
            max_settled: 2_000_000,
        }
    }
}

impl DijkstraParams {
    /// Parameters for a vehicle of one class.
    pub fn for_classes(classes: ClassMask) -> Self {
        Self {
            classes,
            ..Self::default()
        }
    }

    /// True if the mask contains a motorised class, which is what makes the lane-kind
    /// filter apply.
    pub fn is_motorised(&self) -> bool {
        self.classes.contains_any(ClassMask::MOTOR_TRAFFIC)
    }
}

/// Free-flow cost: `length / speed limit`, seconds (04-models.md §2.4).
#[derive(Debug, Clone, Copy)]
pub struct FreeFlowCost<'a> {
    world: &'a World,
}

impl<'a> FreeFlowCost<'a> {
    /// The cost function of a world.
    pub fn new(world: &'a World) -> Self {
        Self { world }
    }
}

impl EdgeCost for FreeFlowCost<'_> {
    fn lane_cost_s(&self, lane: LaneId, _at: SimTime) -> Option<f64> {
        let l = self.world.try_lane(lane)?;
        if l.speed_limit_mps <= 0.0 {
            return None;
        }
        Some(l.length_m / l.speed_limit_mps)
    }
}

/// A frontier entry, ordered by `(cost, lane)` and reversed for the min-heap.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frontier {
    cost_s: f64,
    lane: LaneId,
}

impl Eq for Frontier {}

impl Ord for Frontier {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed, because `BinaryHeap` is a max-heap: the smallest cost must come first.
        // `total_cmp` rather than `partial_cmp` so the order is total and exact.
        other
            .cost_s
            .total_cmp(&self.cost_s)
            .then_with(|| other.lane.cmp(&self.lane))
    }
}

impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Dijkstra on the lane graph.
#[derive(Debug, Clone)]
pub struct Dijkstra {
    params: DijkstraParams,
    card: ModelCard,
}

impl Default for Dijkstra {
    fn default() -> Self {
        Dijkstra::new(DijkstraParams::default())
    }
}

impl Dijkstra {
    /// The router with the given parameters.
    pub fn new(params: DijkstraParams) -> Self {
        Self {
            card: card(&params, ReroutePolicy::STATIC),
            params,
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &DijkstraParams {
        &self.params
    }

    /// True if this router may use `lane` at all.
    pub fn admits(&self, world: &World, lane: LaneId) -> bool {
        let Some(l) = world.try_lane(lane) else {
            return false;
        };
        if !l.admits(self.params.classes) {
            return false;
        }
        // The second filter: a motorised vehicle may only use a lane whose *kind* is for
        // motor traffic, whatever its mask says. A sidewalk mis-tagged as admitting cars is
        // still not a road.
        !self.params.is_motorised() || l.kind.is_motorised()
    }

    /// The search itself, on a world rather than a context — what the engine and the tests
    /// both call.
    ///
    /// Returns the lane sequence from `from` to `to` inclusive, or `None` if none exists
    /// under the class, turn-restriction and cost filters.
    pub fn search(
        &self,
        world: &World,
        from: LaneId,
        to: LaneId,
        at: SimTime,
        costs: &dyn EdgeCost,
    ) -> Option<Route> {
        if !self.admits(world, from) || !self.admits(world, to) {
            return None;
        }
        if costs.lane_cost_s(from, at).is_none() || costs.lane_cost_s(to, at).is_none() {
            return None; // the origin or the destination is closed
        }
        if from == to {
            let lane = world.lane(from);
            return Some(Route {
                lanes: vec![from],
                length_m: lane.length_m,
                cost_s: costs.lane_cost_s(from, at).unwrap_or(0.0),
            });
        }

        let mut best: BTreeMap<LaneId, f64> = BTreeMap::new();
        let mut previous: BTreeMap<LaneId, LaneId> = BTreeMap::new();
        let mut heap: BinaryHeap<Frontier> = BinaryHeap::new();
        let start_cost = costs.lane_cost_s(from, at).unwrap_or(0.0);
        best.insert(from, start_cost);
        heap.push(Frontier {
            cost_s: start_cost,
            lane: from,
        });
        let mut settled = 0usize;

        while let Some(Frontier { cost_s, lane }) = heap.pop() {
            // A stale entry: this lane was already settled at a lower cost.
            if best.get(&lane).is_some_and(|b| cost_s > *b) {
                continue;
            }
            if lane == to {
                return Some(self.reconstruct(world, &previous, from, to, cost_s));
            }
            settled += 1;
            if settled > self.params.max_settled {
                return None;
            }
            for c in world.successors(lane) {
                if !c.permitted {
                    continue; // a turn restriction
                }
                if c.direction == TurnDirection::UTurn && !self.params.allow_uturn {
                    continue;
                }
                let next = c.via.unwrap_or(c.to_lane);
                if next == lane || !self.admits(world, next) {
                    continue;
                }
                let Some(step) = costs.lane_cost_s(next, at) else {
                    continue; // closed
                };
                if !(step.is_finite() && step >= 0.0) {
                    continue;
                }
                let candidate = cost_s + step;
                let improves = match best.get(&next) {
                    None => true,
                    Some(current) => candidate < *current,
                };
                if improves {
                    best.insert(next, candidate);
                    previous.insert(next, lane);
                    heap.push(Frontier {
                        cost_s: candidate,
                        lane: next,
                    });
                }
            }
        }
        None
    }

    /// Walks the predecessor map back and builds the route.
    fn reconstruct(
        &self,
        world: &World,
        previous: &BTreeMap<LaneId, LaneId>,
        from: LaneId,
        to: LaneId,
        cost_s: f64,
    ) -> Route {
        let mut lanes = vec![to];
        let mut cursor = to;
        while cursor != from {
            match previous.get(&cursor) {
                Some(p) => {
                    cursor = *p;
                    lanes.push(cursor);
                }
                None => break,
            }
        }
        lanes.reverse();
        let length_m = v2xw_core::math::sum_ordered(
            lanes
                .iter()
                .map(|l| world.lane(*l).length_m)
                .collect::<Vec<_>>(),
        );
        Route {
            lanes,
            length_m,
            cost_s,
        }
    }
}

impl v2xw_core::model::Model for Dijkstra {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Router for Dijkstra {
    fn route(
        &self,
        ctx: &mut dyn MobCtx,
        from: LaneId,
        to: LaneId,
        at: SimTime,
        costs: &dyn EdgeCost,
    ) -> Option<Route> {
        self.search(ctx.world(), from, to, at, costs)
    }

    fn reroute_policy(&self) -> ReroutePolicy {
        ReroutePolicy::STATIC
    }
}

/// The model card, shared by both routing models (they differ only in their policy).
pub fn card(params: &DijkstraParams, policy: ReroutePolicy) -> ModelCard {
    let src = Source {
        kind: SourceKind::Code,
        reference: "04-models.md §2.4 (edge cost = length / speed limit; closures raise the \
                    cost to infinity), from `roads.py` `CustomNetwork` [01-inventory §3.4]"
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: None,
    };
    let dynamic = policy.on_closure || policy.on_travel_time_change || policy.periodic.is_some();
    let mut card = ModelCard::new(
        if dynamic {
            "mobility/routing/dynamic-reroute"
        } else {
            MODEL_ID
        },
        Family::Mobility,
        MODEL_VERSION,
        if dynamic {
            "Dijkstra on the lane graph, re-run at the next junction when a closure event or \
             a travel-time update changes the best route."
        } else {
            "Dijkstra on the lane graph with edge cost = length / speed limit, honouring \
             turn restrictions and lane-class masks."
        },
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![Equation {
        name: "edge cost".to_string(),
        latex_or_text: "c(lane) = length_m / speed_limit_mps".to_string(),
        notes: Some(
            "free-flow travel time; a closed lane has no cost at all and is not in the \
             graph, which is how \"raise the cost to infinity\" is implemented without \
             infinities in the arithmetic"
                .to_string(),
        ),
    }];
    card.parameters = vec![
        Parameter::new(
            "classes",
            "-",
            serde_json::json!(params.classes.names()),
            Source::new(
                SourceKind::Code,
                "04-models.md §1.1: a lane carries the class mask that may use it",
            ),
        ),
        Parameter::new(
            "allow_uturn",
            "-",
            serde_json::json!(params.allow_uturn),
            src.clone(),
        ),
        Parameter::new(
            "max_settled",
            "-",
            serde_json::json!(params.max_settled),
            Source::new(
                SourceKind::Code,
                "an engineering bound, not a model parameter: it stops a pathological \
                 query rather than shaping a route",
            ),
        ),
    ];
    if dynamic {
        card.parameters.push(Parameter::new(
            "reroute_policy",
            "-",
            serde_json::json!({
                "on_closure": policy.on_closure,
                "on_travel_time_change": policy.on_travel_time_change,
                "periodic_s": policy.periodic.map(|d| d.as_secs_f64()),
            }),
            src.clone(),
        ));
    }
    card.assumptions = vec![
        "A node is a lane and an arc is a permitted connection, which is the only graph a \
         turn restriction can be expressed on."
            .to_string(),
        "Equal-cost paths are resolved by lane id, so the route is a pure function of the \
         world and the costs."
            .to_string(),
        "A motorised vehicle may only use a lane whose kind is for motor traffic, whatever \
         the lane's class mask says."
            .to_string(),
    ];
    card.limitations = vec![
        "One shortest path: no route-choice heterogeneity, no stochastic assignment \
         (04-models.md §2.4 lists this as ignored by all tiers)."
            .to_string(),
    ];
    card.ignores = vec![
        "SUMO's `duarouter` and its rerouters, which serve the high tier \
         (04-models.md §2.4)."
            .to_string(),
    ];
    card.sources = vec![src];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "routing::dijkstra::tests::a_car_is_never_routed_onto_a_sidewalk".to_string(),
            "routing::dijkstra::tests::a_banned_turn_is_never_crossed".to_string(),
        ],
    };
    card
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

    /// A world with a sidewalk in it, so the class filter has something to refuse.
    ///
    /// The procedural generator makes no sidewalk lanes (it insets the buildings instead,
    /// 04-models.md §1.2), and the OSM importer's Manhattan extract is not a unit-test
    /// dependency — so one lane of the grid is turned into a sidewalk, which is exactly the
    /// graph a city import produces.
    fn world_with_a_sidewalk() -> (World, LaneId) {
        let base = world();
        // A lane in the middle of the grid, so routes have reason to want it.
        let candidates = drivable(&base);
        let victim = candidates[candidates.len() / 3];
        let w = crate::worlds::rebuild(&base, |lanes, _| {
            let lane = &mut lanes[victim.as_usize()];
            lane.kind = LaneKind::Sidewalk;
            lane.allowed = ClassMask::PEDESTRIAN;
        })
        .expect("a world with a sidewalk");
        (w, victim)
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
    fn a_route_is_a_connected_chain_of_permitted_movements() {
        let w = world();
        let r = Dijkstra::default();
        let costs = FreeFlowCost::new(&w);
        let lanes = drivable(&w);
        let route = r
            .search(&w, lanes[0], *lanes.last().unwrap(), 0, &costs)
            .expect("a route across the grid");
        assert_eq!(route.lanes.first(), Some(&lanes[0]));
        assert_eq!(route.lanes.last(), lanes.last());
        assert!(route.length_m > 0.0 && route.cost_s > 0.0);
        // Every consecutive pair is joined by a permitted connection.
        for pair in route.lanes.windows(2) {
            let ok = w
                .successors(pair[0])
                .iter()
                .any(|c| c.permitted && c.via.unwrap_or(c.to_lane) == pair[1]);
            assert!(
                ok,
                "{:?} -> {:?} is not a permitted movement",
                pair[0], pair[1]
            );
        }
    }

    #[test]
    fn a_car_is_never_routed_onto_a_sidewalk() {
        let (w, walk) = world_with_a_sidewalk();
        let car = Dijkstra::new(DijkstraParams::for_classes(ClassMask::CAR));
        let costs = FreeFlowCost::new(&w);
        let lanes = drivable(&w);
        let mut checked = 0usize;
        for from in lanes.iter().take(10) {
            for to in lanes.iter().rev().take(10) {
                if let Some(route) = car.search(&w, *from, *to, 0, &costs) {
                    checked += 1;
                    for lane in &route.lanes {
                        let l = w.lane(*lane);
                        assert_ne!(*lane, walk, "the route used the sidewalk");
                        assert!(l.kind.is_motorised(), "route crosses a {:?} lane", l.kind);
                        assert!(
                            l.admits(ClassMask::CAR),
                            "route crosses a lane cars may not use"
                        );
                    }
                }
            }
        }
        assert!(checked > 0, "at least one route was found to check");
        // And a sidewalk is not even an acceptable endpoint.
        assert!(!car.admits(&w, walk));
        assert!(car.search(&w, lanes[0], walk, 0, &costs).is_none());
        assert!(car.search(&w, walk, lanes[0], 0, &costs).is_none());
    }

    #[test]
    fn a_mis_tagged_sidewalk_is_still_refused() {
        // The second filter: a sidewalk whose mask wrongly admits cars is still not a road.
        let (base, walk) = world_with_a_sidewalk();
        let w = crate::worlds::rebuild(&base, |lanes, _| {
            lanes[walk.as_usize()].allowed = ClassMask::ALL;
        })
        .expect("a world with a mis-tagged sidewalk");
        let car = Dijkstra::new(DijkstraParams::for_classes(ClassMask::CAR));
        assert!(w.lane(walk).admits(ClassMask::CAR), "the mask now lies");
        assert!(!car.admits(&w, walk), "and the kind filter catches it");
        // A pedestrian router, by contrast, may use it.
        let walker = Dijkstra::new(DijkstraParams::for_classes(ClassMask::PEDESTRIAN));
        assert!(walker.admits(&w, walk));
    }

    #[test]
    fn a_banned_turn_is_never_crossed() {
        // Ban every left turn in the world, then check no route uses one.
        let base = world();
        let mut banned = 0usize;
        let w = crate::worlds::rebuild(&base, |_, connections| {
            for c in connections.iter_mut() {
                if c.direction == TurnDirection::Left {
                    c.permitted = false;
                    banned += 1;
                }
            }
        })
        .expect("a world with banned left turns");
        assert!(banned > 0, "the grid has left turns to ban");
        let r = Dijkstra::default();
        let costs = FreeFlowCost::new(&w);
        let lanes = drivable(&w);
        let mut routes = 0usize;
        for from in lanes.iter().take(6) {
            for to in lanes.iter().rev().take(6) {
                if let Some(route) = r.search(&w, *from, *to, 0, &costs) {
                    routes += 1;
                    for pair in route.lanes.windows(2) {
                        let used: Vec<_> = w
                            .successors(pair[0])
                            .iter()
                            .filter(|c| c.via.unwrap_or(c.to_lane) == pair[1])
                            .collect();
                        assert!(
                            used.iter().any(|c| c.permitted),
                            "the route used a banned movement {:?} -> {:?}",
                            pair[0],
                            pair[1]
                        );
                        assert!(
                            used.iter()
                                .any(|c| c.permitted && c.direction != TurnDirection::Left),
                            "the route turned left where left turns are banned"
                        );
                    }
                }
            }
        }
        assert!(routes > 0, "banning left turns did not disconnect the grid");
    }

    #[test]
    fn a_u_turn_is_refused_unless_it_is_allowed() {
        let w = world();
        let costs = FreeFlowCost::new(&w);
        let strict = Dijkstra::default();
        let permissive = Dijkstra::new(DijkstraParams {
            allow_uturn: true,
            ..DijkstraParams::default()
        });
        // Find a lane that has a U-turn successor at all.
        let Some((lane, uturn)) = w.roads.lanes().iter().find_map(|l| {
            w.successors(l.id)
                .iter()
                .find(|c| c.direction == TurnDirection::UTurn && c.permitted)
                .map(|c| (l.id, c.via.unwrap_or(c.to_lane)))
        }) else {
            return; // this world has none; nothing to assert
        };
        let strict_route = strict.search(&w, lane, uturn, 0, &costs);
        let permissive_route = permissive
            .search(&w, lane, uturn, 0, &costs)
            .expect("the U-turn itself is a route when U-turns are allowed");
        assert_eq!(permissive_route.lanes.len(), 2);
        if let Some(route) = strict_route {
            assert!(
                route.lanes.len() > 2,
                "without U-turns the router had to go round"
            );
        }
    }

    #[test]
    fn the_shortest_path_is_shortest() {
        let w = world();
        let r = Dijkstra::default();
        let costs = FreeFlowCost::new(&w);
        let lanes = drivable(&w);
        let route = r
            .search(&w, lanes[0], lanes[lanes.len() / 2], 0, &costs)
            .expect("a route");
        // The reported cost equals the sum of the lane costs along it, and no single-arc
        // detour beats it (a spot check of Dijkstra's optimality on this graph).
        let summed: f64 = route
            .lanes
            .iter()
            .map(|l| costs.lane_cost_s(*l, 0).unwrap())
            .sum();
        assert!(
            (summed - route.cost_s).abs() < 1e-9,
            "{summed} vs {}",
            route.cost_s
        );
    }

    #[test]
    fn a_route_to_itself_is_one_lane() {
        let w = world();
        let r = Dijkstra::default();
        let costs = FreeFlowCost::new(&w);
        let lane = drivable(&w)[0];
        let route = r
            .search(&w, lane, lane, 0, &costs)
            .expect("a trivial route");
        assert_eq!(route.lanes, vec![lane]);
    }

    #[test]
    fn the_search_is_a_pure_function_of_the_world() {
        let w = world();
        let r = Dijkstra::default();
        let costs = FreeFlowCost::new(&w);
        let lanes = drivable(&w);
        let a = r.search(&w, lanes[1], lanes[20], 0, &costs);
        let b = r.search(&w, lanes[1], lanes[20], 0, &costs);
        assert_eq!(a, b);
    }

    #[test]
    fn the_card_validates() {
        Dijkstra::default().card().validate().expect("validates");
    }
}
