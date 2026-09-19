//! The value types the mobility traits of 03-interfaces.md §3 exchange.
//!
//! 03-interfaces.md names these types in the trait signatures but does not define them, so
//! they are defined here, once, and every model in the crate speaks them. Three rules shape
//! them:
//!
//! 1. **A view is a copy of a snapshot, not a borrow of live state.** Every view here is
//!    built from the start-of-step [`crate::snapshot::ActorSnapshot`], so a model cannot
//!    observe another actor's mid-step state and the update is the Jacobi update ADR 0004
//!    requires.
//! 2. **A view carries the per-vehicle parameters a neighbour's behaviour depends on**
//!    ([`DriverProfile`]), because MOBIL has to score the acceleration *another* vehicle
//!    would experience, and scoring it with the ego's parameters would be wrong.
//! 3. **Distances are net gaps in metres along the route**, leader rear to follower front,
//!    with the leader's length already subtracted. Every model in the crate reads `gap_m`
//!    with that one meaning.

use serde::{Deserialize, Serialize};
use v2xw_core::geom::{Dims, Vec3};
use v2xw_core::ids::{ActorId, JunctionId, LaneId, SignalId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::time::{Duration, SimTime};
use v2xw_core::weather::WeatherState;
use v2xw_world::{ClassMask, JunctionControl, Lane, LaneKind, SignalState, TurnDirection};

use crate::classes::VehicleClass;

// ---------------------------------------------------------------------------
// Drivers and vehicles
// ---------------------------------------------------------------------------

/// The per-vehicle numbers every car-following model in the literature needs.
///
/// IDM, Krauss and Gipps all take a desired speed, a comfortable acceleration, a
/// comfortable deceleration, a desired time headway and a standstill gap; they differ in
/// what they do with them. Those five live here, per vehicle, because driver heterogeneity
/// is a per-vehicle draw (04-models.md §2.1: "uniform 8-18 × class multiplier" in the
/// legacy set, `speedDev` in the SUMO set). Model-wide constants — the IDM exponent δ, the
/// acceleration floor, the enhanced-model coolness factor — belong to the model and are on
/// its card, not here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DriverProfile {
    /// Desired (free-road) speed `v0`, m/s, after any class multiplier and heterogeneity
    /// draw. Never the lane's speed limit: the model takes the minimum of the two.
    pub desired_speed_mps: f64,
    /// Comfortable maximum acceleration `a`, m/s².
    pub max_accel_mps2: f64,
    /// Comfortable deceleration `b`, m/s² (a positive magnitude).
    pub comfort_decel_mps2: f64,
    /// Desired time headway `T`, seconds.
    pub time_headway_s: f64,
    /// Standstill (jam) distance `s0`, metres.
    pub min_gap_m: f64,
}

impl DriverProfile {
    /// The profile with every field scaled the way bad weather scales it
    /// (04-models.md §2.6): the desired speed down by `desired_speed_factor`, the headway
    /// up by `headway_factor`, the comfortable deceleration capped by `max_decel_mps2`.
    #[must_use]
    pub fn with_weather(mut self, effects: &v2xw_core::weather::DrivingEffects) -> Self {
        self.desired_speed_mps = effects.apply_to_desired_speed(self.desired_speed_mps);
        self.time_headway_s = effects.apply_to_headway(self.time_headway_s);
        self.comfort_decel_mps2 = effects.cap_decel(self.comfort_decel_mps2);
        self
    }
}

/// The ego vehicle, as a car-following, lane-change or intersection model sees it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VehicleView {
    /// Which actor.
    pub actor: ActorId,
    /// Its class, for the dimensions and the lane-access mask.
    pub class: VehicleClass,
    /// The lane it is on.
    pub lane: LaneId,
    /// Its index within that lane's edge, `0` = rightmost (needed by MOBIL to know which
    /// side an adjacent lane is on).
    pub lane_index: u8,
    /// Arc length along that lane, metres, of the **front bumper**.
    ///
    /// The reference point of a [`Kinematics`] is the rear-axle centre (03-interfaces.md
    /// §1); a gap is a bumper-to-bumper quantity, so the longitudinal models work in front
    /// positions and the engine converts once, here.
    pub s_m: f64,
    /// Lateral offset from the lane centreline, metres, positive to the left of travel —
    /// non-zero only during a lane-change transition.
    pub lateral_m: f64,
    /// Speed along the lane, m/s.
    pub speed_mps: f64,
    /// The acceleration the previous step gave it, m/s².
    pub accel_mps2: f64,
    /// Heading in the ENU frame, radians.
    pub heading_rad: f64,
    /// Body dimensions.
    pub dims: Dims,
    /// The driver behind the wheel.
    pub driver: DriverProfile,
}

impl VehicleView {
    /// The rear of the vehicle, in arc length along its lane.
    pub fn rear_s_m(&self) -> f64 {
        self.s_m - self.dims.length_m
    }
}

/// What the vehicle ahead looks like to the one behind — or a virtual obstacle.
///
/// A *virtual* leader ([`LeaderView::virtual_obstacle`]) is how every stopping rule in the
/// crate is expressed: a red signal, a yield at an unsignalised junction and a curve-speed
/// cap all become a leader standing still (or crawling) a known gap ahead. That is the
/// legacy engine's mechanism [`run.py` L2466-2483] and it keeps one equation — the
/// car-following equation — responsible for every deceleration the vehicle ever applies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LeaderView {
    /// The leading vehicle, when the leader is one. `None` for a virtual obstacle.
    pub vehicle: Option<VehicleView>,
    /// Net gap, metres: the leader's rear to the ego's front, leader length already
    /// subtracted.
    pub gap_m: f64,
    /// The leader's speed, m/s (`0` for a stopped virtual obstacle).
    pub speed_mps: f64,
    /// The leader's acceleration, m/s².
    pub accel_mps2: f64,
    /// The leader's length, metres (`0` for a virtual obstacle, which has no body).
    pub length_m: f64,
}

impl LeaderView {
    /// A real leading vehicle `gap_m` metres ahead.
    pub fn of(vehicle: VehicleView, gap_m: f64) -> Self {
        Self {
            gap_m,
            speed_mps: vehicle.speed_mps,
            accel_mps2: vehicle.accel_mps2,
            length_m: vehicle.dims.length_m,
            vehicle: Some(vehicle),
        }
    }

    /// A virtual obstacle `gap_m` metres ahead moving at `speed_mps` — a stop line, a
    /// signal, a junction to yield at, a bend to slow into.
    pub fn virtual_obstacle(gap_m: f64, speed_mps: f64) -> Self {
        Self {
            vehicle: None,
            gap_m,
            speed_mps,
            accel_mps2: 0.0,
            length_m: 0.0,
        }
    }

    /// True if this is a real vehicle rather than a virtual obstacle.
    pub fn is_vehicle(&self) -> bool {
        self.vehicle.is_some()
    }

    /// The same leader seen from a gap of `gap_m` instead — what MOBIL needs when it asks
    /// "what gap would the *other* follower have had?".
    #[must_use]
    pub fn at_gap(mut self, gap_m: f64) -> Self {
        self.gap_m = gap_m;
        self
    }
}

/// The lane a vehicle is on, as the models see it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LaneView {
    /// Which lane.
    pub id: LaneId,
    /// What it is for.
    pub kind: LaneKind,
    /// Its speed limit, m/s.
    pub speed_limit_mps: f64,
    /// Its width, metres.
    pub width_m: f64,
    /// Its centreline length, metres.
    pub length_m: f64,
    /// Which classes it admits.
    pub allowed: ClassMask,
}

impl LaneView {
    /// The view of a world lane.
    pub fn of(lane: &Lane) -> Self {
        Self {
            id: lane.id,
            kind: lane.kind,
            speed_limit_mps: lane.speed_limit_mps,
            width_m: lane.width_m,
            length_m: lane.length_m,
            allowed: lane.allowed,
        }
    }
}

/// Which way a lane change goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    /// Towards the centre of the road in right-hand traffic: a higher lane index.
    Left,
    /// Towards the kerb in right-hand traffic: a lower lane index.
    Right,
}

impl Side {
    /// The other side.
    pub const fn flipped(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

/// The leader and follower on one adjacent lane.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SideNeighbors {
    /// Which lane, and which side of the ego it is on.
    pub lane: LaneId,
    /// Which side.
    pub side: Side,
    /// The vehicle the ego would follow after changing, if any.
    pub leader: Option<LeaderView>,
    /// The vehicle that would follow the ego after changing, if any. Its `gap_m` is the
    /// net gap from the ego's rear to that vehicle's front.
    pub follower: Option<LeaderView>,
}

/// Everything around the ego, from **one** neighbour query.
///
/// 04-models.md §2.2 requires the lane-change model's neighbour classification to share the
/// car-following leader search rather than run a second one, which is what this type is
/// for: [`crate::snapshot::ActorSnapshot::neighbors`] fills it once per actor per step, the
/// car-following model reads [`LaneNeighbors::leader`] and the lane-change model reads the
/// rest.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LaneNeighbors {
    /// The ego's own lane.
    pub lane: LaneId,
    /// The vehicle ahead on the ego's route (across lane boundaries), if any.
    pub leader: Option<LeaderView>,
    /// The vehicle behind on the ego's lane, if any.
    pub follower: Option<LeaderView>,
    /// The lane to the left, when there is a usable one.
    pub left: Option<SideNeighbors>,
    /// The lane to the right, when there is a usable one.
    pub right: Option<SideNeighbors>,
}

impl LaneNeighbors {
    /// An empty neighbourhood on `lane`: a vehicle alone on an open road.
    pub fn empty(lane: LaneId) -> Self {
        Self {
            lane,
            leader: None,
            follower: None,
            left: None,
            right: None,
        }
    }

    /// The adjacent lane on `side`, if any.
    pub fn side(&self, side: Side) -> Option<&SideNeighbors> {
        match side {
            Side::Left => self.left.as_ref(),
            Side::Right => self.right.as_ref(),
        }
    }
}

// ---------------------------------------------------------------------------
// Intersections
// ---------------------------------------------------------------------------

/// The junction the ego is approaching.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JunctionView {
    /// Which junction.
    pub id: JunctionId,
    /// Its centre, world-local metres.
    pub position: Vec3,
    /// How it is controlled.
    pub control: JunctionControl,
    /// Gap from the ego's front bumper to the stop line, metres. Negative once the ego is
    /// inside the junction.
    pub stop_line_gap_m: f64,
    /// The movement the ego intends, from its route.
    pub movement: TurnDirection,
    /// The internal connector the ego's movement uses, when the world has one. This is the
    /// row of [`v2xw_world::ConflictMatrix`] and the index into
    /// [`v2xw_world::SignalPlan::controlled`] that names the ego's movement.
    pub movement_lane: Option<LaneId>,
    /// The signal state the ego's movement is showing, when the junction is signalised.
    pub signal: Option<SignalState>,
    /// How many major-stream lanes the conflicting flow has, which chooses the HCM
    /// critical-gap column (04-models.md §2.3: `< 4 lanes` or `>= 4 lanes`).
    pub major_lanes: u8,
}

/// One other claimant on the same junction.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConflictView {
    /// The claiming actor.
    pub actor: ActorId,
    /// Its distance to the same junction's stop line, metres.
    pub stop_line_gap_m: f64,
    /// Its speed, m/s.
    pub speed_mps: f64,
    /// Its heading, radians.
    pub heading_rad: f64,
    /// The movement it intends.
    pub movement: TurnDirection,
    /// Its movement's internal connector, when the world has one.
    pub movement_lane: Option<LaneId>,
    /// True if the world's conflict matrix (or, without one, the geometry) says this
    /// claimant's movement conflicts with the ego's.
    pub conflicts: bool,
    /// True if the ego must give way to it: the world's `response` row, or the priority
    /// rule of the model when the world has no matrix.
    pub ego_must_yield: bool,
}

impl ConflictView {
    /// Time for this claimant to reach the stop line at its current speed, seconds —
    /// the gap a gap-acceptance model compares against the critical gap.
    ///
    /// [`f64::INFINITY`] for a standing claimant, which therefore never blocks anybody: a
    /// stopped vehicle at a stop line is not an arriving gap-closing vehicle, and treating
    /// it as one deadlocks every all-way stop.
    pub fn time_to_stop_line_s(&self) -> f64 {
        if self.speed_mps <= 0.0 {
            f64::INFINITY
        } else {
            self.stop_line_gap_m.max(0.0) / self.speed_mps
        }
    }
}

/// What an [`crate::traits::IntersectionControl`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EntryDecision {
    /// Carry on: nothing at this junction constrains the ego.
    Proceed,
    /// Stop: treat the junction as a stationary obstacle `gap_m` ahead.
    Stop {
        /// Net gap to the virtual obstacle, metres.
        gap_m: f64,
    },
    /// Slow down: treat the junction as an obstacle `gap_m` ahead moving at `speed_mps` —
    /// a curve-speed cap or a permissive green that needs care.
    SlowTo {
        /// Net gap to the virtual obstacle, metres.
        gap_m: f64,
        /// The speed to match, m/s.
        speed_mps: f64,
    },
}

impl EntryDecision {
    /// The decision as a virtual leader for the car-following model, if it constrains
    /// anything.
    pub fn as_leader(&self) -> Option<LeaderView> {
        match *self {
            EntryDecision::Proceed => None,
            EntryDecision::Stop { gap_m } => Some(LeaderView::virtual_obstacle(gap_m, 0.0)),
            EntryDecision::SlowTo { gap_m, speed_mps } => {
                Some(LeaderView::virtual_obstacle(gap_m, speed_mps))
            }
        }
    }
}

/// What a [`crate::traits::LaneChange`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LaneChangeDecision {
    /// Stay in this lane.
    Stay,
    /// Change to `to`.
    Change {
        /// The target lane.
        to: LaneId,
        /// Which side it is on.
        side: Side,
        /// The MOBIL incentive that won, m/s² — recorded so a run can be explained.
        incentive_mps2: f64,
        /// How long the lateral transition takes.
        duration: Duration,
    },
}

// ---------------------------------------------------------------------------
// Routes, demand, commands
// ---------------------------------------------------------------------------

/// A route: the lanes to drive, in order, including the junctions' internal connectors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Route {
    /// The lanes, in travel order. Consecutive entries are connected by a permitted
    /// [`v2xw_world::Connection`].
    pub lanes: Vec<LaneId>,
    /// Total length, metres.
    pub length_m: f64,
    /// Free-flow travel time at the lanes' speed limits, seconds — the cost the router
    /// minimised.
    pub cost_s: f64,
}

impl Route {
    /// An empty route.
    pub fn empty() -> Self {
        Self {
            lanes: Vec::new(),
            length_m: 0.0,
            cost_s: 0.0,
        }
    }

    /// True if the route has no lanes.
    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    /// The position of `lane` in the route, searching from `hint` forward and then from
    /// the start — routes are short and a vehicle's position on one moves monotonically,
    /// so the hint makes the common case O(1).
    pub fn index_of(&self, lane: LaneId, hint: usize) -> Option<usize> {
        if self.lanes.get(hint) == Some(&lane) {
            return Some(hint);
        }
        self.lanes.iter().position(|l| *l == lane)
    }

    /// The lane after `index`, if the route continues.
    pub fn next(&self, index: usize) -> Option<LaneId> {
        self.lanes.get(index + 1).copied()
    }

    /// The last lane of the route.
    pub fn destination(&self) -> Option<LaneId> {
        self.lanes.last().copied()
    }
}

/// One trip the demand model asks for.
///
/// Invariant I-M2: actor ids are assigned from the demand stream order, so `seq` is the
/// order the demand model produced the request in and the engine assigns ids in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TripRequest {
    /// Position in the demand stream, from `0`.
    pub seq: u64,
    /// When the trip starts.
    pub t: SimTime,
    /// The lane it starts on.
    pub origin: LaneId,
    /// How far along that lane, metres.
    pub origin_s_m: f64,
    /// The lane it ends on.
    pub destination: LaneId,
    /// The vehicle class.
    pub class: VehicleClass,
    /// The drawn desired speed, m/s, before any lane speed limit applies.
    pub desired_speed_mps: f64,
}

/// A new actor the mobility model has put on the road.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActorSpawn {
    /// Its id.
    pub actor: ActorId,
    /// When it appeared.
    pub t: SimTime,
    /// Its class.
    pub class: VehicleClass,
    /// Its state at that instant.
    pub kinematics: Kinematics,
    /// Its route.
    pub route: Route,
    /// The driver behind the wheel.
    pub driver: DriverProfile,
    /// The demand-stream position it came from.
    pub seq: u64,
}

/// Why an actor left the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DespawnCause {
    /// It reached the end of its route.
    TripComplete,
    /// Its route was cut (a closure with no alternative) and it could not continue.
    RouteBlocked,
    /// An external [`MobilityCommand::Despawn`] removed it.
    Commanded,
    /// It exceeded its maximum lifetime, which bounds a run's memory.
    LifetimeExpired,
}

/// The live state of one signal controller, as the mobility step publishes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseState {
    /// Index of the running phase in [`v2xw_world::SignalPlan::phases`].
    pub phase: usize,
    /// How long the phase has been running, seconds.
    pub elapsed_s: f64,
    /// How long it has left, seconds.
    pub remaining_s: f64,
    /// The state of each controlled movement, parallel to
    /// [`v2xw_world::SignalPlan::controlled`].
    pub states: Vec<SignalState>,
}

/// What one mobility step produced (03-interfaces.md §3).
///
/// Invariant I-M1: `states` is ordered by [`ActorId`]. So are `spawned` and `despawned`,
/// and `signal_states` is ordered by [`SignalId`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MobilityUpdate {
    /// The instant this update is valid for — the end of the step.
    pub t: SimTime,
    /// Every active actor's kinematics, ordered by actor id.
    pub states: Vec<(ActorId, Kinematics)>,
    /// Actors that appeared during the step, ordered by actor id.
    pub spawned: Vec<ActorSpawn>,
    /// Actors that left during the step, ordered by actor id.
    pub despawned: Vec<(ActorId, DespawnCause)>,
    /// Every signal controller's state, ordered by signal id.
    pub signal_states: Vec<(SignalId, PhaseState)>,
}

impl MobilityUpdate {
    /// An update with nothing in it.
    pub fn empty(t: SimTime) -> Self {
        Self {
            t,
            states: Vec::new(),
            spawned: Vec::new(),
            despawned: Vec::new(),
            signal_states: Vec::new(),
        }
    }

    /// Every float in every published state, on its declared grid (build decision D9).
    ///
    /// `gt.kinematics` is a recorded channel, so the phase driver calls this before it
    /// emits. Quantising here rather than at each writer keeps the rule in one place.
    #[must_use]
    pub fn quantized(mut self) -> Self {
        for (_, k) in &mut self.states {
            *k = k.quantized();
        }
        for s in &mut self.spawned {
            s.kinematics = s.kinematics.quantized();
        }
        for (_, p) in &mut self.signal_states {
            p.elapsed_s = v2xw_core::math::q3(p.elapsed_s);
            p.remaining_s = v2xw_core::math::q3(p.remaining_s);
        }
        self
    }
}

/// An external control applied at the next step (03-interfaces.md §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MobilityCommand {
    /// Send an actor somewhere else; it re-plans at the next junction.
    Reroute {
        /// The actor.
        actor: ActorId,
        /// Its new destination lane.
        to: LaneId,
    },
    /// Cap an actor's speed, or lift the cap with `None`.
    SpeedCap {
        /// The actor.
        actor: ActorId,
        /// The cap, m/s.
        v_mps: Option<f64>,
    },
    /// Hold an actor at a standstill until `until` (or indefinitely).
    Stop {
        /// The actor.
        actor: ActorId,
        /// When it may move again.
        until: Option<SimTime>,
    },
    /// Remove an actor.
    Despawn {
        /// The actor.
        actor: ActorId,
    },
    /// Close or reopen a lane. Closed lanes cost infinity to the router, and vehicles
    /// already on one continue to its end.
    Closure {
        /// The lane.
        lane: LaneId,
        /// `true` closes it.
        closed: bool,
    },
    /// Put a specific trip on the road, bypassing the demand model.
    Spawn(TripRequest),
}

/// When a router re-plans (04-models.md §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReroutePolicy {
    /// Re-plan at the next junction after a closure event.
    pub on_closure: bool,
    /// Re-plan at the next junction when a travel-time update changes the best route.
    pub on_travel_time_change: bool,
    /// Re-plan periodically, regardless of events.
    pub periodic: Option<Duration>,
}

impl ReroutePolicy {
    /// Never re-plan: the route chosen at spawn is driven to the end.
    pub const STATIC: ReroutePolicy = ReroutePolicy {
        on_closure: false,
        on_travel_time_change: false,
        periodic: None,
    };

    /// Re-plan on a closure, which is the minimum a scenario with closures needs.
    pub const ON_CLOSURE: ReroutePolicy = ReroutePolicy {
        on_closure: true,
        on_travel_time_change: false,
        periodic: None,
    };
}

impl Default for ReroutePolicy {
    fn default() -> Self {
        ReroutePolicy::STATIC
    }
}

/// Per-lane traversal costs, as the router reads them (03-interfaces.md §3).
///
/// `None` means impassable: a closure, a lane the class may not use, a lane removed from
/// the network. A cost must be finite, positive and a pure function of `(lane, at)`, or
/// Dijkstra's optimality argument does not hold.
pub trait EdgeCost {
    /// The cost of traversing `lane` starting at `at`, seconds.
    fn lane_cost_s(&self, lane: LaneId, at: SimTime) -> Option<f64>;
}

/// The environment a GNSS receiver is in (03-interfaces.md §3, 04-models.md §2.10, §3.8).
///
/// The legacy model read the weather from a global and the outage state from the vehicle
/// struct; 01-inventory §3.3 records that as a defect, because it made the error model
/// depend on state it did not own. Everything the model needs is passed in here instead.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GnssEnv {
    /// The weather at the receiver.
    pub weather: WeatherState,
    /// How much sky the receiver can see.
    pub sky: SkyView,
    /// True while a jammer is denying the receiver its signal.
    pub jammed: bool,
    /// True for a vehicle whose sensor is faulty: a sustained bias several times nominal
    /// (04-models.md §2.10, `faulty_bias_mult` 5.0).
    pub faulty_sensor: bool,
}

impl GnssEnv {
    /// Open sky, clear weather, no jamming, healthy sensor.
    pub const OPEN_SKY: GnssEnv = GnssEnv {
        weather: WeatherState::CLEAR,
        sky: SkyView::OpenSky,
        jammed: false,
        faulty_sensor: false,
    };
}

impl Default for GnssEnv {
    fn default() -> Self {
        GnssEnv::OPEN_SKY
    }
}

/// How much sky a receiver sees, which is what scales its error (04-models.md §3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkyView {
    /// Open sky: the Reid 2019 production-automotive baseline.
    OpenSky,
    /// A canyon with NLOS exclusion working: the Wen and Hsu mitigated mean.
    CanyonMitigated,
    /// A deep urban canyon, standalone: the Wen and Hsu unmitigated mean.
    DeepCanyon,
    /// No sky at all: a tunnel or a car park. No fix.
    Obstructed,
}

impl SkyView {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            SkyView::OpenSky => "open-sky",
            SkyView::CanyonMitigated => "canyon-mitigated",
            SkyView::DeepCanyon => "deep-canyon",
            SkyView::Obstructed => "obstructed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::weather::DrivingEffects;

    fn profile() -> DriverProfile {
        DriverProfile {
            desired_speed_mps: 33.3,
            max_accel_mps2: 1.4,
            comfort_decel_mps2: 2.0,
            time_headway_s: 1.5,
            min_gap_m: 2.0,
        }
    }

    #[test]
    fn weather_scales_a_driver_profile() {
        let effects = DrivingEffects {
            desired_speed_factor: 0.9,
            headway_factor: 1.2,
            max_decel_mps2: 1.5,
            visibility_m: 200.0,
        };
        let p = profile().with_weather(&effects);
        assert!((p.desired_speed_mps - 33.3 * 0.9).abs() < 1e-12);
        assert!((p.time_headway_s - 1.5 * 1.2).abs() < 1e-12);
        assert_eq!(p.comfort_decel_mps2, 1.5, "the cap binds");
        // And the neutral element leaves it alone.
        let q = profile().with_weather(&DrivingEffects::UNAFFECTED);
        assert_eq!(q, profile());
    }

    #[test]
    fn a_virtual_obstacle_has_no_body() {
        let l = LeaderView::virtual_obstacle(12.0, 0.0);
        assert!(!l.is_vehicle());
        assert_eq!(l.length_m, 0.0);
        assert_eq!(l.gap_m, 12.0);
        assert_eq!(l.at_gap(3.0).gap_m, 3.0);
    }

    #[test]
    fn an_entry_decision_becomes_a_virtual_leader() {
        assert!(EntryDecision::Proceed.as_leader().is_none());
        let stop = EntryDecision::Stop { gap_m: 8.0 }.as_leader().unwrap();
        assert_eq!((stop.gap_m, stop.speed_mps), (8.0, 0.0));
        let slow = EntryDecision::SlowTo {
            gap_m: 20.0,
            speed_mps: 6.0,
        }
        .as_leader()
        .unwrap();
        assert_eq!((slow.gap_m, slow.speed_mps), (20.0, 6.0));
    }

    #[test]
    fn a_standing_claimant_never_closes_a_gap() {
        let mut c = ConflictView {
            actor: ActorId::new(1),
            stop_line_gap_m: 10.0,
            speed_mps: 0.0,
            heading_rad: 0.0,
            movement: TurnDirection::Straight,
            movement_lane: None,
            conflicts: true,
            ego_must_yield: true,
        };
        assert!(c.time_to_stop_line_s().is_infinite());
        c.speed_mps = 5.0;
        assert!((c.time_to_stop_line_s() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn a_route_finds_its_lanes() {
        let r = Route {
            lanes: vec![LaneId::new(3), LaneId::new(7), LaneId::new(11)],
            length_m: 300.0,
            cost_s: 30.0,
        };
        assert_eq!(r.index_of(LaneId::new(7), 1), Some(1));
        assert_eq!(
            r.index_of(LaneId::new(7), 0),
            Some(1),
            "hint miss still finds it"
        );
        assert_eq!(r.index_of(LaneId::new(99), 0), None);
        assert_eq!(r.next(1), Some(LaneId::new(11)));
        assert_eq!(r.next(2), None);
        assert_eq!(r.destination(), Some(LaneId::new(11)));
    }
}
