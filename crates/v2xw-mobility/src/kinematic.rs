//! `mobility/kinematic/lane-follow` — the abstract tier (04-models.md §2.1).
//!
//! > Each actor follows its route at `min(desired_speed, lane speed limit)`, stops for
//! > nothing, despawns at trip end.
//!
//! That is the whole model, and the point of it is what it leaves out: no leader
//! interaction, no signals, no gap acceptance, no lane changes, no weather. A 10,000-node
//! run that only needs plausible positions for a radio study pays for none of them
//! (02-architecture.md §7.1), and the trip length and time it produces are exactly
//! `route length` and `route length / speed`, which is what makes it useful as a baseline:
//! any difference the medium tier shows is interaction, and nothing else.
//!
//! It publishes [`v2xw_core::kinematics::Kinematics`] in the same frame and at the same
//! reference point as every other tier (invariant I-M4), so a consumer cannot tell which
//! tier produced a state without being told.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geom::{LanePos, Vec3};
use v2xw_core::ids::{ActorId, LaneId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::math;
use v2xw_core::time::{Duration, SimTime};
use v2xw_world::{ClassMask, World};

use crate::classes::VehicleClass;
use crate::ctx::MobCtx;
use crate::error::{MobError, Result};
use crate::routing::dijkstra::{Dijkstra, DijkstraParams, FreeFlowCost};
use crate::traits::{Demand, Mobility};
use crate::views::{
    ActorSpawn, DespawnCause, DriverProfile, MobilityCommand, MobilityUpdate, PhaseState, Route,
    TripRequest,
};

/// The model id.
pub const MODEL_ID: &str = "mobility/kinematic/lane-follow";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The model's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaneFollowParams {
    /// The classes its actors may use.
    pub classes: ClassMask,
    /// The maximum lifetime of an actor. `None` is unlimited.
    pub max_lifetime: Option<Duration>,
}

impl Default for LaneFollowParams {
    fn default() -> Self {
        Self {
            classes: ClassMask::MOTOR_TRAFFIC,
            max_lifetime: None,
        }
    }
}

/// One actor.
#[derive(Debug, Clone, PartialEq)]
struct Actor {
    id: ActorId,
    seq: u64,
    class: VehicleClass,
    desired_speed_mps: f64,
    route: Route,
    route_index: usize,
    lane: LaneId,
    s_m: f64,
    speed_mps: f64,
    spawned: SimTime,
}

/// The abstract-tier kinematic model.
pub struct KinematicLaneFollow {
    params: LaneFollowParams,
    router: Dijkstra,
    demand: Option<Box<dyn Demand>>,
    actors: BTreeMap<ActorId, Actor>,
    published: BTreeMap<ActorId, Kinematics>,
    pending: Vec<MobilityCommand>,
    next_actor: u32,
    card: ModelCard,
}

impl core::fmt::Debug for KinematicLaneFollow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KinematicLaneFollow")
            .field("params", &self.params)
            .field("actors", &self.actors.len())
            .field("has_demand", &self.demand.is_some())
            .finish()
    }
}

impl Default for KinematicLaneFollow {
    fn default() -> Self {
        Self::new(LaneFollowParams::default())
    }
}

impl KinematicLaneFollow {
    /// The model with the given parameters.
    pub fn new(params: LaneFollowParams) -> Self {
        Self {
            card: card(&params),
            router: Dijkstra::new(DijkstraParams::for_classes(params.classes)),
            params,
            demand: None,
            actors: BTreeMap::new(),
            published: BTreeMap::new(),
            pending: Vec::new(),
            next_actor: 0,
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &LaneFollowParams {
        &self.params
    }

    /// How many actors are on the road.
    pub fn len(&self) -> usize {
        self.actors.len()
    }

    /// True if none are.
    pub fn is_empty(&self) -> bool {
        self.actors.is_empty()
    }

    /// The speed an actor drives at on its current lane: `min(desired, limit)`.
    fn speed_of(&self, world: &World, actor: &Actor) -> f64 {
        let limit = world.lane(actor.lane).speed_limit_mps;
        actor.desired_speed_mps.min(limit).max(0.0)
    }

    /// The published state of one actor.
    fn state_of(&self, world: &World, actor: &Actor, t: SimTime) -> Kinematics {
        let lane = world.lane(actor.lane);
        let length = actor.class.spec().length_m;
        let s_rear = (actor.s_m - length).clamp(0.0, lane.length_m);
        let (pos, heading) = lane.pose_at(s_rear);
        let (sin_h, cos_h) = math::sin_cos(heading);
        Kinematics {
            t,
            pos,
            vel: Vec3::new(actor.speed_mps * cos_h, actor.speed_mps * sin_h, 0.0),
            acc: Vec3::ZERO,
            heading_rad: heading,
            yaw_rate_rad_s: 0.0,
            lane: Some(LanePos::centred(actor.lane, s_rear)),
            dims: actor.class.dims(),
        }
    }

    /// Routes a trip and puts it on the road.
    fn insert(&mut self, world: &World, trip: &TripRequest) -> Option<ActorId> {
        let costs = FreeFlowCost::new(world);
        let route = self
            .router
            .search(world, trip.origin, trip.destination, trip.t, &costs)?;
        let id = ActorId::new(self.next_actor);
        self.next_actor += 1;
        self.actors.insert(
            id,
            Actor {
                id,
                seq: trip.seq,
                class: trip.class,
                desired_speed_mps: trip.desired_speed_mps,
                route,
                route_index: 0,
                lane: trip.origin,
                s_m: trip.origin_s_m.max(trip.class.spec().length_m),
                speed_mps: 0.0,
                spawned: trip.t,
            },
        );
        Some(id)
    }
}

impl v2xw_core::model::Model for KinematicLaneFollow {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Mobility for KinematicLaneFollow {
    fn tier(&self) -> Tier {
        Tier::Abstract
    }

    fn init(&mut self, ctx: &mut dyn MobCtx, demand: Box<dyn Demand>) -> Result<()> {
        let world = ctx.world();
        if world
            .roads
            .lanes()
            .iter()
            .all(|l| !l.kind.is_motorised() || !l.admits(self.params.classes))
        {
            return Err(MobError::EmptyWorld {
                what: "drivable lane the configured classes may use",
            });
        }
        self.demand = Some(demand);
        Ok(())
    }

    fn step(&mut self, ctx: &mut dyn MobCtx, dt: Duration) -> MobilityUpdate {
        let t0 = ctx.now();
        let t1 = dt.after(t0);
        let dt_s = dt.as_secs_f64();

        // Commands. This tier honours the two that mean anything without interaction.
        for command in core::mem::take(&mut self.pending) {
            match command {
                MobilityCommand::Despawn { actor } => {
                    self.actors.remove(&actor);
                }
                MobilityCommand::SpeedCap { actor, v_mps } => {
                    if let Some(a) = self.actors.get_mut(&actor)
                        && let Some(cap) = v_mps
                    {
                        a.desired_speed_mps = a.desired_speed_mps.min(cap);
                    }
                }
                MobilityCommand::Spawn(trip) => {
                    let world = ctx.world();
                    self.insert(world, &trip);
                }
                // A reroute, a stop and a closure all describe interaction this tier does
                // not model; ignoring them is what "stops for nothing" means, and the card
                // says so.
                _ => {}
            }
        }

        let mut spawned: Vec<ActorSpawn> = Vec::new();
        if let Some(mut demand) = self.demand.take() {
            let trips = demand.spawns_in(ctx, t0, t1);
            self.demand = Some(demand);
            let world = ctx.world();
            for trip in trips {
                if let Some(id) = self.insert(world, &trip) {
                    let actor = &self.actors[&id];
                    spawned.push(ActorSpawn {
                        actor: id,
                        t: t0,
                        class: actor.class,
                        kinematics: self.state_of(world, actor, t0),
                        route: actor.route.clone(),
                        driver: DriverProfile {
                            desired_speed_mps: actor.desired_speed_mps,
                            max_accel_mps2: actor.class.spec().accel_mps2,
                            comfort_decel_mps2: actor.class.spec().decel_mps2,
                            time_headway_s: 0.0,
                            min_gap_m: actor.class.spec().min_gap_m,
                        },
                        seq: actor.seq,
                    });
                }
            }
        }

        let world = ctx.world();
        let mut despawned: Vec<(ActorId, DespawnCause)> = Vec::new();
        let ids: Vec<ActorId> = self.actors.keys().copied().collect();
        for id in ids {
            let speed = {
                let actor = &self.actors[&id];
                self.speed_of(world, actor)
            };
            let actor = self.actors.get_mut(&id).expect("present");
            actor.speed_mps = speed;
            actor.s_m += speed * dt_s;
            let mut cause = None;
            loop {
                let length = world.lane(actor.lane).length_m;
                if actor.s_m <= length {
                    break;
                }
                match actor.route.lanes.get(actor.route_index + 1).copied() {
                    Some(next) => {
                        actor.s_m -= length;
                        actor.lane = next;
                        actor.route_index += 1;
                    }
                    None => {
                        actor.s_m = length;
                        cause = Some(DespawnCause::TripComplete);
                        break;
                    }
                }
            }
            if cause.is_none()
                && let Some(limit) = self.params.max_lifetime
                && t1.saturating_sub(actor.spawned) >= limit.as_nanos()
            {
                cause = Some(DespawnCause::LifetimeExpired);
            }
            if let Some(cause) = cause {
                despawned.push((id, cause));
            }
        }
        for (id, _) in &despawned {
            self.actors.remove(id);
        }
        let states: Vec<(ActorId, Kinematics)> = self
            .actors
            .values()
            .map(|a| (a.id, self.state_of(world, a, t1)))
            .collect();
        self.published = states.iter().copied().collect();
        spawned.sort_by_key(|s| s.actor);
        despawned.sort_by_key(|(a, _)| *a);
        MobilityUpdate {
            t: t1,
            states,
            spawned,
            despawned,
            signal_states: Vec::<(v2xw_core::ids::SignalId, PhaseState)>::new(),
        }
    }

    fn command(&mut self, _ctx: &mut dyn MobCtx, cmd: MobilityCommand) {
        self.pending.push(cmd);
    }

    fn kinematics(&self, a: ActorId) -> Option<&Kinematics> {
        self.published.get(&a)
    }
}

/// The model card.
pub fn card(params: &LaneFollowParams) -> ModelCard {
    let src = Source {
        kind: SourceKind::Code,
        reference: "04-models.md §2.1: \"Each actor follows its route at min(desired_speed, \
                    lane speed limit), stops for nothing, despawns at trip end\""
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: None,
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "The abstract tier: every actor follows its route at the smaller of its desired \
         speed and the lane's limit, stops for nothing, and despawns when the route ends. \
         Its trip length and time are exactly the route's, which is what makes it the \
         baseline the medium tier's interaction is measured against.",
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![Equation {
        name: "speed".to_string(),
        latex_or_text: "v = min(desired_speed, lane.speed_limit);  s ← s + v·Δt".to_string(),
        notes: None,
    }];
    card.parameters = vec![
        Parameter::new(
            "classes",
            "-",
            serde_json::json!(params.classes.names()),
            src.clone(),
        ),
        Parameter::new(
            "max_lifetime",
            "s",
            serde_json::json!(params.max_lifetime.map(|d| d.as_secs_f64())),
            Source::new(
                SourceKind::Code,
                "an optional bound on a run's memory; unlimited by default",
            ),
        ),
    ];
    card.assumptions = vec![
        "Speed changes instantly at a lane boundary: there is no acceleration at all.".to_string(),
        "Kinematics are published in the same frame and at the same reference point as \
         every other tier (invariant I-M4)."
            .to_string(),
    ];
    card.limitations = vec![
        "Vehicles pass through one another and through red lights, because neither is \
         modelled."
            .to_string(),
    ];
    card.ignores = vec![
        "Leader interaction, signals, gap acceptance, lane changes and weather — every one \
         of them relative to the medium tier (04-models.md §2.1)."
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
        tests: vec!["kinematic::tests::a_trip_takes_its_route_length_over_its_speed".to_string()],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::MobilityCtx;
    use crate::demand::NoDemand;
    use crate::worlds::{RingParams, cycle_length_m, ring, ring_cycle};
    use v2xw_core::model::Model;
    use v2xw_core::rng::RngRegistry;
    use v2xw_core::time::NS_PER_MS;

    fn world() -> World {
        ring(&RingParams {
            circumference_m: 1000.0,
            segments: 5,
            speed_limit_mps: 20.0,
            ..RingParams::default()
        })
        .expect("a ring")
    }

    #[test]
    fn a_trip_takes_its_route_length_over_its_speed() {
        let w = world();
        let cycle = ring_cycle(&w, 0);
        let length = cycle_length_m(&w, &cycle);
        let rng = RngRegistry::new(1);
        let mut m = KinematicLaneFollow::default();
        {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            m.init(&mut ctx, Box::new(NoDemand::new())).expect("init");
            m.command(
                &mut ctx,
                MobilityCommand::Spawn(TripRequest {
                    seq: 0,
                    t: 0,
                    origin: cycle[0],
                    origin_s_m: 0.0,
                    destination: *cycle.last().unwrap(),
                    class: VehicleClass::Passenger,
                    desired_speed_mps: 10.0,
                }),
            );
        }
        // The trip is the whole ring bar the wrap: length over 10 m/s.
        let dt = Duration::from_millis(100);
        let mut t = 0u64;
        let mut finished_at = None;
        while t < 300 * v2xw_core::time::NS_PER_S {
            let mut ctx = MobilityCtx::new(t, &w, &rng);
            let update = m.step(&mut ctx, dt);
            if update
                .despawned
                .iter()
                .any(|(_, cause)| *cause == DespawnCause::TripComplete)
            {
                finished_at = Some(update.t);
                break;
            }
            t += 100 * NS_PER_MS;
        }
        let finished = finished_at.expect("the trip finished");
        let want_s = length / 10.0;
        let got_s = v2xw_core::time::ns_to_secs(finished);
        assert!(
            (got_s - want_s).abs() < 1.0,
            "took {got_s} s against the route's {want_s} s"
        );
    }

    #[test]
    fn the_lane_limit_caps_the_speed() {
        let w = world(); // a 20 m/s ring
        let cycle = ring_cycle(&w, 0);
        let rng = RngRegistry::new(2);
        let mut m = KinematicLaneFollow::default();
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        m.init(&mut ctx, Box::new(NoDemand::new())).expect("init");
        m.command(
            &mut ctx,
            MobilityCommand::Spawn(TripRequest {
                seq: 0,
                t: 0,
                origin: cycle[0],
                origin_s_m: 0.0,
                destination: cycle[2],
                class: VehicleClass::Passenger,
                desired_speed_mps: 50.0, // faster than the limit
            }),
        );
        let update = m.step(&mut ctx, Duration::from_millis(100));
        let (_, k) = update.states.first().expect("one actor");
        assert!(
            (k.speed_mps() - 20.0).abs() < 1e-9,
            "capped at the limit, got {}",
            k.speed_mps()
        );
    }

    #[test]
    fn it_stops_for_nothing() {
        // Two vehicles in the same place: the abstract tier drives them straight through
        // each other, which is exactly what its card says it ignores.
        let w = world();
        let cycle = ring_cycle(&w, 0);
        let rng = RngRegistry::new(3);
        let mut m = KinematicLaneFollow::default();
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        m.init(&mut ctx, Box::new(NoDemand::new())).expect("init");
        for seq in 0..2u64 {
            m.command(
                &mut ctx,
                MobilityCommand::Spawn(TripRequest {
                    seq,
                    t: 0,
                    origin: cycle[0],
                    origin_s_m: 10.0,
                    destination: cycle[3],
                    class: VehicleClass::Passenger,
                    desired_speed_mps: 15.0,
                }),
            );
        }
        let update = m.step(&mut ctx, Duration::from_millis(100));
        assert_eq!(update.states.len(), 2);
        let speeds: Vec<f64> = update.states.iter().map(|(_, k)| k.speed_mps()).collect();
        assert!(
            speeds.iter().all(|v| (v - 15.0).abs() < 1e-9),
            "neither slowed for the other: {speeds:?}"
        );
    }

    #[test]
    fn the_output_is_ordered_by_actor_id() {
        let w = world();
        let cycle = ring_cycle(&w, 0);
        let rng = RngRegistry::new(4);
        let mut m = KinematicLaneFollow::default();
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        m.init(&mut ctx, Box::new(NoDemand::new())).expect("init");
        for seq in 0..5u64 {
            m.command(
                &mut ctx,
                MobilityCommand::Spawn(TripRequest {
                    seq,
                    t: 0,
                    origin: cycle[seq as usize % cycle.len()],
                    origin_s_m: 5.0,
                    destination: cycle[(seq as usize + 2) % cycle.len()],
                    class: VehicleClass::Passenger,
                    desired_speed_mps: 12.0,
                }),
            );
        }
        let update = m.step(&mut ctx, Duration::from_millis(100));
        let ids: Vec<ActorId> = update.states.iter().map(|(a, _)| *a).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "invariant I-M1");
        // And `kinematics` answers for each of them.
        for id in ids {
            assert!(Mobility::kinematics(&m, id).is_some());
        }
    }

    #[test]
    fn the_card_validates_and_is_abstract_tier() {
        let m = KinematicLaneFollow::default();
        m.card().validate().expect("validates");
        assert_eq!(m.tier(), Tier::Abstract);
        assert!(!m.card().determinism.uses_rng);
        assert!(m.card().ignores.iter().any(|i| i.contains("signals")));
    }
}
