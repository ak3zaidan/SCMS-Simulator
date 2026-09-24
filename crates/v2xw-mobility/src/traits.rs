//! The plug-in traits of 03-interfaces.md §3.
//!
//! Every one extends [`Model`], so a registered mobility model always carries a card
//! (03-interfaces.md §12) and the card is reachable from the live object.
//!
//! # Deviations from the document, and why
//!
//! Three signatures differ from the ones 03-interfaces.md prints, and each difference is a
//! compile error in the document's version:
//!
//! 1. `&mut dyn Ctx` becomes `&mut dyn MobCtx`. [`v2xw_core::ctx::Ctx`] has an associated
//!    `Payload` type that only the kernel crate can name; [`crate::ctx`] explains the
//!    adapter and the one-line migration.
//! 2. `Mobility::init` takes `Box<dyn Demand>`, not `&dyn Demand`. Two things make the
//!    document's version impossible: [`Demand::spawns_in`] takes `&mut self`, because a
//!    demand model advances a stream, so a shared reference cannot call it at all; and
//!    `step` — which is where trips actually arrive — is given no demand argument, so the
//!    mobility model must *hold* the demand model rather than borrow it for the length of
//!    one call. Taking ownership at `init` is the only reading of the two signatures
//!    together that compiles. The separate `world: &World` parameter is dropped as well:
//!    the world is [`MobCtx::world`], and passing it twice would let a caller pass two
//!    different worlds.
//! 3. `Mobility::step` takes `dt: Duration`, not `dt: SimTime`. [`SimTime`] is an instant
//!    and [`Duration`] is a length of time; the step length is the second thing.
//!
//! One method is *added* rather than changed: [`CarFollowing::profile`]. 03-interfaces.md
//! §3 gives the trait `accel` alone, which leaves the engine with no way to ask a
//! car-following model for the per-class driver parameters that model was calibrated with;
//! the engine used a hard-coded set instead, so `NativeMobility::legacy()` ran the legacy
//! equations with Kesting 2010 drivers. It is defaulted, so no implementation outside this
//! crate has to change.

use v2xw_core::card::Tier;
use v2xw_core::ids::{ActorId, LaneId, NodeId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};
use v2xw_core::weather::WeatherState;
use v2xw_core::{FixQuality, PositionEstimate};

use crate::classes::VehicleClass;
use crate::ctx::MobCtx;
use crate::error::Result;
use crate::snapshot::ActorSnapshot;
use crate::views::{
    ConflictView, DriverProfile, EdgeCost, EntryDecision, GnssEnv, JunctionView,
    LaneChangeDecision, LaneNeighbors, LaneView, LeaderView, MobilityCommand, MobilityUpdate,
    ReroutePolicy, Route, TripRequest, VehicleView,
};

/// A source of ground-truth kinematics for every actor (03-interfaces.md §3).
pub trait Mobility: Model {
    /// Which fidelity tier this implementation serves.
    fn tier(&self) -> Tier;

    /// Prepares the model: reads the world from `ctx`, takes the demand model's first
    /// trips, and places nothing on the road yet.
    ///
    /// # Errors
    ///
    /// Whatever the world or the parameters make impossible — see [`crate::error::MobError`].
    fn init(&mut self, ctx: &mut dyn MobCtx, demand: Box<dyn Demand>) -> Result<()>;

    /// Advances every actor by `dt`.
    ///
    /// Must be a pure function of `(state, dt, RNG streams)`: no wall clock, no
    /// thread-local, no dependence on the order actors happen to be stored in. The
    /// returned [`MobilityUpdate`] is ordered by [`ActorId`] (invariant I-M1).
    fn step(&mut self, ctx: &mut dyn MobCtx, dt: Duration) -> MobilityUpdate;

    /// Applies an external control at the next step.
    fn command(&mut self, ctx: &mut dyn MobCtx, cmd: MobilityCommand);

    /// The last published state of one actor, for perception and safety metrics.
    fn kinematics(&self, a: ActorId) -> Option<&Kinematics>;

    /// The weather every vehicle drives in from the next step on.
    ///
    /// A provided method, so a tier that models no weather response keeps compiling; the
    /// native engine overrides it. Until the kernel called it, `weather.*` reached the
    /// radio and the GNSS models and no driver: a snowstorm changed nothing on the road.
    fn set_weather(&mut self, _weather: v2xw_core::weather::WeatherState) {}
}

/// Longitudinal acceleration from the gap and the speed difference to the leader
/// (03-interfaces.md §3, 04-models.md §2.1).
///
/// `accel` must be pure: same inputs, same output, on every platform. It draws no random
/// numbers — driver heterogeneity is a per-vehicle draw that reaches the model through
/// [`crate::views::DriverProfile`] — so it takes no context.
pub trait CarFollowing: Model {
    /// The acceleration, m/s², for `ego` behind `leader` on `lane` in weather `w`.
    ///
    /// `leader` is `None` on a free road; a stop line, a signal or a junction to yield at
    /// arrives as a *virtual* leader ([`LeaderView::virtual_obstacle`]), so one equation
    /// produces every deceleration the vehicle applies.
    fn accel(
        &self,
        ego: &VehicleView,
        leader: Option<&LeaderView>,
        lane: &LaneView,
        w: &WeatherState,
    ) -> f64;

    /// The driver parameters this model's own calibration gives a vehicle of `class`.
    ///
    /// `a_max`, `b`, `T` and `s0` are *this model's* parameters — they are what the
    /// calibration the model was published with consists of — but they travel per vehicle,
    /// in a [`DriverProfile`], because they are drawn per driver. Without this method the
    /// engine has nowhere to get them from and has to guess a set, which is how a
    /// legacy-parity run came to be driven by Kesting 2010 drivers.
    ///
    /// Defaulted to the Kesting 2010 set, §2.1's medium-tier default, so a model with no
    /// per-class calibration of its own is explicit about which one it borrows.
    fn profile(&self, class: VehicleClass) -> DriverProfile {
        crate::carfollowing::idm::IdmPreset::Kesting2010.profile(class)
    }
}

/// Discretionary and mandatory lane changes (03-interfaces.md §3, 04-models.md §2.2).
pub trait LaneChange: Model {
    /// Whether `ego` changes lane, given the **one** neighbour query `nbrs` that the
    /// car-following leader search also used.
    fn decide(
        &self,
        ctx: &mut dyn MobCtx,
        ego: &VehicleView,
        nbrs: &LaneNeighbors,
        w: &WeatherState,
    ) -> LaneChangeDecision;
}

/// Right of way at a junction (03-interfaces.md §3, 04-models.md §2.3).
pub trait IntersectionControl: Model {
    /// Whether `ego` may enter `j` given the other claimants, and if not, where to stop.
    ///
    /// `conflicts` is ordered by [`ActorId`], so a model that breaks a tie by id sees the
    /// same order on every run and every thread count.
    fn may_enter(
        &self,
        ego: &VehicleView,
        j: &JunctionView,
        conflicts: &[ConflictView],
        w: &WeatherState,
    ) -> EntryDecision;
}

/// Shortest paths on the lane graph (03-interfaces.md §3, 04-models.md §2.4).
pub trait Router: Model {
    /// A route from `from` to `to` for a vehicle that may use the lanes `costs` prices,
    /// or `None` when none exists.
    fn route(
        &self,
        ctx: &mut dyn MobCtx,
        from: LaneId,
        to: LaneId,
        at: SimTime,
        costs: &dyn EdgeCost,
    ) -> Option<Route>;

    /// When this router re-plans.
    fn reroute_policy(&self) -> ReroutePolicy;
}

/// A source of trips (03-interfaces.md §3, 04-models.md §2.4).
pub trait Demand: Model {
    /// Every trip that starts in `[from, to)`, in stream order (invariant I-M2).
    ///
    /// Called once per mobility step with consecutive, non-overlapping windows. A model
    /// that draws candidate arrivals faster than it keeps them (the thinned Poisson
    /// process of §2.4) must keep its own cursor so the draw sequence does not depend on
    /// how the caller chopped time up.
    fn spawns_in(&mut self, ctx: &mut dyn MobCtx, from: SimTime, to: SimTime) -> Vec<TripRequest>;
}

/// Vulnerable road users (03-interfaces.md §3, 04-models.md §2.5).
pub trait VruMobility: Model {
    /// Advances every VRU by `dt` and returns their states, ordered by [`ActorId`].
    ///
    /// `vehicles` is the start-of-step snapshot: VRUs see vehicles where they were at the
    /// start of the step, exactly as vehicles see each other, so the whole mobility phase
    /// is one Jacobi update.
    fn step(
        &mut self,
        ctx: &mut dyn MobCtx,
        dt: Duration,
        vehicles: &ActorSnapshot,
    ) -> Vec<(ActorId, Kinematics)>;
}

/// A node's belief about its own position and time (03-interfaces.md §3, 04-models.md §3.8).
pub trait GnssModel: Model {
    /// The belief of `node`, given its ground truth and its environment.
    ///
    /// Stateful per node: the error process is correlated in time, so the model keeps one
    /// state per node and advances it here. The state must be advanced from the RNG stream
    /// of that node only, so one node's outage cannot shift another node's noise.
    fn estimate(
        &mut self,
        ctx: &mut dyn MobCtx,
        node: NodeId,
        gt: &Kinematics,
        env: &GnssEnv,
    ) -> PositionEstimate;
}

/// A node's clock (03-interfaces.md §3, 04-models.md §3.8).
pub trait ClockModel: Model {
    /// The time `node` believes it is, including drift during holdover.
    fn read(&mut self, ctx: &mut dyn MobCtx, node: NodeId, gnss: &FixQuality) -> SimTime;
}
