//! The fundamental-diagram validation of 04-models.md §2.9.
//!
//! # What is measured, and how
//!
//! A **closed single-lane ring** is loaded to a series of densities. On a ring the density
//! is exactly `N / L` and stays there, so the flow-density relation can be measured without
//! an inflow boundary condition to argue about — which is why it is the classical setting
//! for this check.
//!
//! At each density the run warms up, and the measurement window is then cut into
//! one-minute bins (§2.9: "flow-density and speed-density scatter over 1-minute bins per
//! lane"). Each bin is reduced with **Edie's generalised definitions**, which are the
//! correct ones for a region of road over an interval:
//!
//! ```text
//! flow    q = Σ distance travelled / (L · T)
//! density k = Σ time spent        / (L · T)   ( = N/L on a closed ring)
//! speed   v = q / k                            ( = the space-mean speed)
//! ```
//!
//! # What is fitted
//!
//! * **capacity** — the largest binned flow over every density;
//! * **critical density** — the density at which that maximum occurred;
//! * **congested branch** — a least-squares line through the bins above the critical
//!   density, whose slope is the **wave speed** and whose zero-flow intercept is the
//!   **jam density**;
//! * **capacity drop** — a *same-density* hysteresis. At each density measured on both
//!   branches at or above the critical density, `(free max − jammed max) / free max`;
//!   the reported figure is the largest of those and the density it was taken at is
//!   reported with it. Both branches are filtered identically, which is the whole point:
//!   comparing the free branch's peak over *all* densities against the jammed branch's
//!   peak over the densities *above* the critical one measures the two at different
//!   densities, so it mixes a real hysteresis with the slope of the congested branch
//!   between them;
//! * **dynamic capacity** — the jammed branch's largest flow at that same density: the
//!   throughput traffic carries once it has broken down, measured where the drop is;
//! * **theoretical capacity** — `Q_max = (1/T)·(1 − l_eff/(v0·T + l_eff))`
//!   [Kesting 2010 Eq. 4.1], for comparison.
//!
//! # Why the Treiber 2000 parameter set
//!
//! Every target in §2.9 — jam density 140 veh/km, wave speed about −15 km/h, convective
//! stability threshold 1,050 veh/h — is a Treiber, Hennecke and Helbing 2000 number
//! [R10 §B1, §B15], measured with that paper's own parameter set. Checking those targets
//! against a different parameter set would be checking the wrong thing, so
//! [`FdParams::default`] uses [`IdmPreset::Treiber2000`]. With `T = 1.6 s`, `s0 = 2 m` and a
//! 5 m car the triangular diagram it predicts is `ρ_jam = 1000/7 = 142.9 veh/km` and
//! `w = −l_eff/T = −4.375 m/s = −15.75 km/h`, which is where the paper's figures come from.
//!
//! # The perturbation
//!
//! A ring of identical vehicles at identical spacing is an exact equilibrium: nothing ever
//! happens. Treiber 2000 breaks it with a *localised perturbation*, and so does this: one
//! vehicle starts slower than the others by [`FdParams::perturbation`]. Without it the
//! congested branch would never form and the capacity drop would be measured as zero — not
//! because the model is stable, but because nothing asked it a question.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use v2xw_core::ids::{ActorId, LaneId};
use v2xw_core::math;
use v2xw_core::rng::RngRegistry;
use v2xw_core::time::{Duration, SimTime};
use v2xw_world::World;

use crate::carfollowing::idm::{Idm, IdmPreset};
use crate::classes::VehicleClass;
use crate::ctx::MobilityCtx;
use crate::demand::NoDemand;
use crate::engine::{EngineParams, IntersectionMode, NativeMobility};
use crate::error::Result;
use crate::lanechange::mobil::MobilPreset;
use crate::traits::{CarFollowing, Mobility};
use crate::views::DriverProfile;
use crate::worlds::{RingParams, cycle_length_m, ring, ring_cycle};

/// The §2.9 target bands.
pub mod targets {
    /// Capacity drop at a freeway bottleneck: the literature range.
    pub const CAPACITY_DROP: (f64, f64) = (0.05, 0.20);
    /// Jam density, veh/km (Treiber 2000's IDM calibration).
    pub const JAM_DENSITY_VEH_KM: f64 = 140.0;
    /// Jam propagation speed, km/h (Treiber 2000).
    pub const WAVE_SPEED_KMH: f64 = -15.0;
    /// The uncongested constant-speed regime, passenger cars per hour per lane
    /// (Hall, FHWA Traffic Flow Theory Ch. 2).
    pub const UNCONGESTED_FLOW_VEH_H: (f64, f64) = (300.0, 2200.0);
    /// The convective-stability flow threshold, veh/h (Treiber 2000).
    ///
    /// **Carried for reference; this measurement does not test it.** Deciding whether a
    /// localised perturbation grows as it propagates upstream needs a perturbation-tracking
    /// experiment, not a flow-density scatter, so the number is reported by
    /// [`uncovered_rows`] as a gap rather than silently asserted.
    pub const CONVECTIVE_STABILITY_VEH_H: f64 = 1050.0;

    /// How far the measured capacity may sit below the parameter set's own `Q_max`.
    ///
    /// The §2.9 capacity band, 300-2,200 veh/h, is FHWA's *uncongested-regime* range
    /// rather than a capacity target, and is wide enough that nothing plausible fails it.
    /// The falsifiable comparison is against `Q_max = (1/T)·(1 − l_eff/(v0·T + l_eff))`
    /// [Kesting 2010 Eq. 4.1], which the same result already carries. A ring's measured
    /// capacity is expected to sit *below* it — `Q_max` is the deterministic equilibrium
    /// throughput and a ring with a perturbation never quite reaches it — so the check is
    /// one-sided, and 20 % is the width stated here rather than hidden in an assertion.
    pub const CAPACITY_BELOW_THEORY_TOLERANCE: f64 = 0.20;
    /// How wide a band the jam density and the wave speed are checked against.
    ///
    /// §2.9 gives both as "about": a 20 % band around each is what "about" is read as here,
    /// and the choice is stated rather than hidden in an assertion.
    pub const ABOUT_TOLERANCE: f64 = 0.20;

    /// The §2.9 rows this measurement does **not** cover, each saying why.
    ///
    /// §2.9 says "a run outside every band marks the parameter set unit-tested rather than
    /// literature-checked", and the cards that cite it claim `literature-checked`. That
    /// claim is only as wide as the rows actually measured, so the rows that are not are
    /// named here rather than left for a reader to discover by grepping for an unused
    /// constant.
    pub fn uncovered_rows() -> Vec<String> {
        vec![
            format!(
                "convective stability ({CONVECTIVE_STABILITY_VEH_H:.0} veh/h): NOT \
                 MEASURED — it is a statement about whether a localised perturbation grows \
                 as it propagates upstream, which needs a perturbation-tracking experiment \
                 rather than a flow-density scatter"
            ),
            "time-mean against space-mean speed: NOT MEASURED — every bin here is Edie's \
             space-mean speed, and no time-mean speed is formed to compare it with"
                .to_string(),
        ]
    }
}

/// The measurement's configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FdParams {
    /// The ring to measure on.
    pub ring: RingParams,
    /// The densities to load it to, veh/km.
    pub densities_veh_km: Vec<f64>,
    /// How long to let each density settle before measuring.
    pub warmup: Duration,
    /// How long to measure for.
    pub measure: Duration,
    /// The bin length (§2.9: one minute).
    pub bin: Duration,
    /// The mobility step.
    pub step: Duration,
    /// The master seed.
    pub seed: u64,
    /// Which car-following parameter set.
    pub preset: IdmPreset,
    /// The vehicle class every vehicle is.
    pub class: VehicleClass,
    /// The relative speed reduction of the one perturbed vehicle.
    pub perturbation: f64,
    /// Relative speed heterogeneity, ±this fraction, drawn per vehicle
    /// (Kesting 2007 uses ±20 %). Zero keeps the fleet homogeneous.
    pub speed_heterogeneity: f64,
    /// How long one vehicle is held at a standstill on the jammed branch, seconds. Long
    /// enough to form a wide moving jam, which is what the dynamic capacity is the outflow
    /// of.
    pub jam_hold_s: f64,
}

impl Default for FdParams {
    /// The full validation configuration: a 1 km ring, sixteen densities from 5 to
    /// 140 veh/km, five minutes of warm-up and five minutes of measurement.
    fn default() -> Self {
        Self {
            ring: RingParams {
                circumference_m: 1000.0,
                segments: 8,
                points_per_segment: 16,
                lanes: 1,
                lane_width_m: 3.5,
                speed_limit_mps: 33.3,
            },
            densities_veh_km: vec![
                5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 35.0, 40.0, 50.0, 60.0, 75.0, 90.0, 105.0,
                120.0, 130.0, 140.0,
            ],
            warmup: Duration::from_secs(300),
            measure: Duration::from_secs(300),
            bin: Duration::from_secs(60),
            step: Duration::from_millis(100),
            seed: 0x5C55_0000_0000_0001,
            preset: IdmPreset::Treiber2000,
            class: VehicleClass::Passenger,
            perturbation: 0.2,
            speed_heterogeneity: 0.0,
            jam_hold_s: 30.0,
        }
    }
}

impl FdParams {
    /// A smaller configuration for a unit test: a 600 m ring, twelve densities, two minutes
    /// of warm-up and three of measurement.
    ///
    /// The numbers it produces are the same numbers — the fit is scale-free — but a shorter
    /// run has fewer bins and a noisier congested branch, which is why the full
    /// configuration is what the example reports.
    pub fn quick() -> Self {
        Self {
            ring: RingParams {
                circumference_m: 600.0,
                segments: 6,
                points_per_segment: 12,
                ..RingParams::default()
            },
            densities_veh_km: vec![
                5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 40.0, 55.0, 70.0, 90.0, 115.0, 140.0,
            ],
            warmup: Duration::from_secs(120),
            measure: Duration::from_secs(180),
            ..Self::default()
        }
    }
}

/// Which initial condition a bin was measured from.
///
/// The capacity drop is a *hysteresis*: at one density, traffic that has never broken down
/// carries more flow than traffic that has. Measuring it therefore needs two runs per
/// density — one that starts homogeneous and is left alone, and one that is deliberately
/// jammed and then released — and this says which is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Branch {
    /// Started at the homogeneous equilibrium and never disturbed beyond the localised
    /// perturbation: the metastable free branch.
    Free,
    /// Started at the same equilibrium, then held one vehicle at a standstill long enough
    /// to form a wide moving jam, and released it: the congested branch.
    Jammed,
}

impl Branch {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Branch::Free => "free",
            Branch::Jammed => "jammed",
        }
    }
}

/// One one-minute bin.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bin {
    /// Which initial condition it came from.
    pub branch: Branch,
    /// The density the ring was loaded to, veh/km.
    pub density_veh_km: f64,
    /// Edie's flow, veh/h.
    pub flow_veh_h: f64,
    /// Edie's space-mean speed, m/s.
    pub speed_mps: f64,
    /// How many vehicles were on the ring.
    pub vehicles: usize,
}

/// What the measurement found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FdResult {
    /// Every bin, in the order they were measured.
    pub bins: Vec<Bin>,
    /// The largest binned flow, veh/h.
    pub capacity_veh_h: f64,
    /// The density at which it occurred, veh/km.
    pub critical_density_veh_km: f64,
    /// The jammed branch's largest flow at [`FdResult::capacity_drop_density_veh_km`],
    /// veh/h: the throughput traffic carries once it has broken down, measured at the
    /// density where the drop is largest.
    pub dynamic_capacity_veh_h: f64,
    /// The largest same-density hysteresis, `(free max − jammed max) / free max`, over
    /// the densities measured on both branches at or above the critical density.
    pub capacity_drop: f64,
    /// The density the capacity drop was measured at, veh/km.
    ///
    /// Reported because a drop is only a drop *somewhere*: without it, `capacity_drop`,
    /// [`FdResult::capacity_veh_h`] and [`FdResult::dynamic_capacity_veh_h`] cannot be
    /// checked against one another, which is exactly how a drop measured between two
    /// different densities went unnoticed.
    pub capacity_drop_density_veh_km: f64,
    /// The free branch's largest flow at that same density, veh/h.
    ///
    /// `capacity_drop == (free_flow_at_drop_veh_h − dynamic_capacity_veh_h) /
    /// free_flow_at_drop_veh_h` exactly, which is the identity a consumer can check.
    pub free_flow_at_drop_veh_h: f64,
    /// The congested branch's zero-flow intercept, veh/km.
    pub jam_density_veh_km: f64,
    /// The congested branch's slope, km/h.
    pub wave_speed_kmh: f64,
    /// The mean speed at the lowest density measured, m/s.
    pub free_speed_mps: f64,
    /// `Q_max = (1/T)·(1 − l_eff/(v0·T + l_eff))` [Kesting 2010 Eq. 4.1], veh/h.
    pub theoretical_capacity_veh_h: f64,
    /// The jam density the parameters predict, `1000/(l + s0)`, veh/km.
    pub theoretical_jam_density_veh_km: f64,
    /// The wave speed the parameters predict, `−(l + s0)/T`, km/h.
    pub theoretical_wave_speed_kmh: f64,
    /// How many bins fell on the congested branch.
    pub congested_bins: usize,
}

impl FdResult {
    /// Whether the capacity drop is inside its §2.9 band.
    pub fn capacity_drop_in_band(&self) -> bool {
        (targets::CAPACITY_DROP.0..=targets::CAPACITY_DROP.1).contains(&self.capacity_drop)
    }

    /// Whether the jam density is within [`targets::ABOUT_TOLERANCE`] of its target.
    pub fn jam_density_in_band(&self) -> bool {
        relative_error(self.jam_density_veh_km, targets::JAM_DENSITY_VEH_KM)
            <= targets::ABOUT_TOLERANCE
    }

    /// Whether the wave speed is within [`targets::ABOUT_TOLERANCE`] of its target.
    pub fn wave_speed_in_band(&self) -> bool {
        relative_error(self.wave_speed_kmh, targets::WAVE_SPEED_KMH) <= targets::ABOUT_TOLERANCE
    }

    /// Whether the measured capacity is inside the uncongested regime's range.
    pub fn capacity_in_band(&self) -> bool {
        (targets::UNCONGESTED_FLOW_VEH_H.0..=targets::UNCONGESTED_FLOW_VEH_H.1)
            .contains(&self.capacity_veh_h)
    }

    /// How far the measured capacity sits below the parameter set's own `Q_max`, as a
    /// fraction. Negative if it sits above.
    pub fn capacity_below_theory(&self) -> f64 {
        if self.theoretical_capacity_veh_h <= 0.0 {
            return f64::NAN;
        }
        (self.theoretical_capacity_veh_h - self.capacity_veh_h) / self.theoretical_capacity_veh_h
    }

    /// Whether the measured capacity is within
    /// [`targets::CAPACITY_BELOW_THEORY_TOLERANCE`] below `Q_max`, and not above it.
    ///
    /// This is the falsifiable capacity check; [`FdResult::capacity_in_band`] is a
    /// regime-membership check against a range nothing plausible fails.
    pub fn capacity_matches_theory(&self) -> bool {
        let gap = self.capacity_below_theory();
        gap.is_finite() && (0.0..=targets::CAPACITY_BELOW_THEORY_TOLERANCE).contains(&gap)
    }

    /// The identity the same-density drop satisfies by construction, for a consumer that
    /// wants to check it: `(free at drop − dynamic) / free at drop`.
    ///
    /// Returns `NaN` when no density was measured on both branches.
    pub fn capacity_drop_recomputed(&self) -> f64 {
        if self.free_flow_at_drop_veh_h <= 0.0 {
            return f64::NAN;
        }
        (self.free_flow_at_drop_veh_h - self.dynamic_capacity_veh_h) / self.free_flow_at_drop_veh_h
    }

    /// Every check, as `(name, measured, target, in band)` — what a report prints.
    pub fn report(&self) -> Vec<(&'static str, String, String, bool)> {
        vec![
            (
                "capacity (veh/h/lane)",
                format!("{:.1}", self.capacity_veh_h),
                format!(
                    "{:.0}-{:.0}",
                    targets::UNCONGESTED_FLOW_VEH_H.0,
                    targets::UNCONGESTED_FLOW_VEH_H.1
                ),
                self.capacity_in_band(),
            ),
            (
                "jam density (veh/km)",
                format!("{:.1}", self.jam_density_veh_km),
                format!("about {:.0}", targets::JAM_DENSITY_VEH_KM),
                self.jam_density_in_band(),
            ),
            (
                "wave speed (km/h)",
                format!("{:.2}", self.wave_speed_kmh),
                format!("about {:.0}", targets::WAVE_SPEED_KMH),
                self.wave_speed_in_band(),
            ),
            (
                "capacity drop",
                format!(
                    "{:.1} % at k = {:.0}",
                    100.0 * self.capacity_drop,
                    self.capacity_drop_density_veh_km
                ),
                format!(
                    "{:.0}-{:.0} %",
                    100.0 * targets::CAPACITY_DROP.0,
                    100.0 * targets::CAPACITY_DROP.1
                ),
                self.capacity_drop_in_band(),
            ),
            (
                "capacity vs Q_max",
                format!("{:.1} % below", 100.0 * self.capacity_below_theory()),
                format!(
                    "0-{:.0} % below",
                    100.0 * targets::CAPACITY_BELOW_THEORY_TOLERANCE
                ),
                self.capacity_matches_theory(),
            ),
        ]
    }
}

/// `|a − b| / |b|`, or `|a|` when `b` is zero.
fn relative_error(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        a.abs()
    } else {
        (a - b).abs() / b.abs()
    }
}

/// The equilibrium speed at a net gap, m/s: the speed at which the car-following model
/// produces no acceleration when the leader travels at the same speed.
///
/// Bisection, because the IDM's equilibrium has no closed form for `δ = 4` and a numerical
/// root is exact enough for an initial condition: 60 halvings of `[0, v0]` leave an
/// interval under `v0 · 2⁻⁶⁰`, which is far below the last bit of the speed it returns.
pub fn equilibrium_speed_mps(idm: &Idm, driver: &DriverProfile, v0_mps: f64, gap_m: f64) -> f64 {
    let mut low = 0.0;
    let mut high = v0_mps;
    for _ in 0..60 {
        let mid = 0.5 * (low + high);
        let accel = idm.accel_of(mid, v0_mps, gap_m, mid, driver);
        if accel > 0.0 {
            low = mid;
        } else {
            high = mid;
        }
    }
    0.5 * (low + high)
}

/// Runs the whole measurement.
///
/// # Errors
///
/// Whatever the ring geometry or the engine refuses.
pub fn measure(params: &FdParams) -> Result<FdResult> {
    let world = ring(&params.ring)?;
    let cycle = ring_cycle(&world, 0);
    let length_m = cycle_length_m(&world, &cycle);
    let idm = Idm::new(params.preset);
    let driver_base = params.preset.profile(params.class);
    let mut bins: Vec<Bin> = Vec::new();

    for density in &params.densities_veh_km {
        let count = ((density / 1000.0) * length_m).round().max(1.0) as usize;
        for branch in [Branch::Free, Branch::Jammed] {
            let mut run = run_one(
                params,
                &world,
                &cycle,
                length_m,
                count,
                &idm,
                &driver_base,
                branch,
            )?;
            bins.append(&mut run);
        }
    }
    Ok(fit(params, &bins, &driver_base, length_m))
}

/// One density: load the ring, warm it up, and measure it in bins.
#[allow(clippy::too_many_arguments)]
fn run_one(
    params: &FdParams,
    world: &World,
    cycle: &[LaneId],
    length_m: f64,
    count: usize,
    idm: &Idm,
    driver: &DriverProfile,
    branch: Branch,
) -> Result<Vec<Bin>> {
    let rng = RngRegistry::new(params.seed);
    let cf: Arc<dyn CarFollowing + Send + Sync> = Arc::new(idm.clone());
    let mut engine = NativeMobility::with_models(
        EngineParams {
            step: params.step,
            intersections: IntersectionMode::None,
            lane_changes: false,
            lookahead_m: (length_m / count as f64).max(70.0) * 1.5,
            dynamic_rerouting: false,
            ..EngineParams::default()
        },
        Arc::clone(&cf),
        MobilPreset::Kesting2007,
    );
    {
        let mut ctx = MobilityCtx::new(0, world, &rng);
        engine.init(&mut ctx, Box::new(NoDemand::new()))?;
    }

    // The route: the ring, driven enough times to outlast the run.
    let total_s = params.warmup.as_secs_f64() + params.measure.as_secs_f64();
    let laps = ((driver.desired_speed_mps * total_s / length_m).ceil() as usize + 2).max(3);
    let route: Vec<LaneId> = cycle
        .iter()
        .cycle()
        .take(cycle.len() * laps)
        .copied()
        .collect();

    // Load the ring: equally spaced, at the equilibrium speed for that spacing.
    let spacing = length_m / count as f64;
    let body = params.class.spec().length_m;
    let gap = (spacing - body).max(0.5);
    let v_eq = equilibrium_speed_mps(idm, driver, driver.desired_speed_mps, gap);
    let mut ids: Vec<ActorId> = Vec::with_capacity(count);
    for i in 0..count {
        let s = spacing * i as f64;
        // Which lane of the cycle that arc length falls on.
        let mut remaining = s;
        let mut lane_index = 0usize;
        for (k, lane) in cycle.iter().enumerate() {
            let l = world.lane(*lane).length_m;
            if remaining <= l || k == cycle.len() - 1 {
                lane_index = k;
                break;
            }
            remaining -= l;
        }
        let rotated: Vec<LaneId> = route[lane_index..].to_vec();
        let mut heterogeneous = *driver;
        if params.speed_heterogeneity > 0.0 {
            // A deterministic, symmetric spread: vehicle `i` of `count` takes its place on
            // an evenly spaced grid across the band, which reproduces the ±20 % of
            // Kesting 2007 without a random draw that the ring's symmetry would then hide.
            let position = if count > 1 {
                2.0 * (i as f64) / ((count - 1) as f64) - 1.0
            } else {
                0.0
            };
            heterogeneous.desired_speed_mps *= 1.0 + params.speed_heterogeneity * position;
        }
        let id = engine.spawn_with_route(
            world,
            0,
            params.class,
            heterogeneous,
            rotated,
            remaining.min(world.lane(cycle[lane_index]).length_m),
        )?;
        let speed = if i == 0 && branch == Branch::Free {
            v_eq * (1.0 - params.perturbation)
        } else {
            v_eq
        };
        engine.set_speed(id, speed)?;
        ids.push(id);
    }
    // The jammed branch: hold one vehicle still long enough for a wide moving jam to form
    // behind it, then let it go. The measurement window starts well after the release, so
    // what it sees is the jam's own dynamics and not the block itself.
    if branch == Branch::Jammed && !ids.is_empty() {
        let mut ctx = MobilityCtx::new(0, world, &rng);
        let until = Duration::from_secs_f64(params.jam_hold_s).after(0);
        engine.command(
            &mut ctx,
            crate::views::MobilityCommand::Stop {
                actor: ids[0],
                until: Some(until),
            },
        );
    }

    // Run, measuring Edie's quantities bin by bin.
    let step_ns = params.step.as_nanos();
    let bin_ns = params.bin.as_nanos();
    let warmup_ns = params.warmup.as_nanos();
    let total_ns = warmup_ns + params.measure.as_nanos();
    let mut t: SimTime = 0;
    let mut bin_distance_m = 0.0f64;
    let mut bin_elapsed_ns: u64 = 0;
    let mut out: Vec<Bin> = Vec::new();
    while t < total_ns {
        let mut ctx = MobilityCtx::new(t, world, &rng);
        engine.step(&mut ctx, params.step);
        t += step_ns;
        // The distance every vehicle covered this step, from its own speed: on a ring the
        // arc length wraps, so the speed is the honest measure and it is exactly what Edie's
        // definition integrates.
        let states = engine.longitudinal_states();
        let travelled: f64 = math::sum_ordered(
            states
                .iter()
                .map(|(_, _, _, v)| v * params.step.as_secs_f64())
                .collect::<Vec<_>>(),
        );
        if t > warmup_ns {
            bin_distance_m += travelled;
            bin_elapsed_ns += step_ns;
            if bin_elapsed_ns >= bin_ns {
                let bin_s = (bin_elapsed_ns as f64) / 1e9;
                let vehicles = states.len();
                let flow_veh_s = bin_distance_m / (length_m * bin_s);
                let density_veh_m = vehicles as f64 / length_m;
                out.push(Bin {
                    branch,
                    density_veh_km: density_veh_m * 1000.0,
                    flow_veh_h: flow_veh_s * 3600.0,
                    speed_mps: if density_veh_m > 0.0 {
                        flow_veh_s / density_veh_m
                    } else {
                        0.0
                    },
                    vehicles,
                });
                bin_distance_m = 0.0;
                bin_elapsed_ns = 0;
            }
        }
    }
    Ok(out)
}

/// Fits the triangular diagram to the bins.
///
/// The free branch gives the capacity and the critical density; the jammed branch gives the
/// congested line and the dynamic capacity. That split is what makes the capacity drop a
/// measurement of hysteresis rather than of the curvature of one equilibrium curve.
fn fit(params: &FdParams, bins: &[Bin], driver: &DriverProfile, _length_m: f64) -> FdResult {
    let free: Vec<&Bin> = bins.iter().filter(|b| b.branch == Branch::Free).collect();
    let jammed: Vec<&Bin> = bins.iter().filter(|b| b.branch == Branch::Jammed).collect();
    let capacity = free
        .iter()
        .fold(f64::NEG_INFINITY, |best, b| best.max(b.flow_veh_h));
    let critical = free
        .iter()
        .filter(|b| b.flow_veh_h >= capacity - 1e-9)
        .map(|b| b.density_veh_km)
        .fold(f64::INFINITY, f64::min);
    // The congested branch: jammed-branch bins above the critical density. This set exists
    // to fit the congested LINE (its slope is the wave speed and its zero-flow intercept
    // the jam density), which is a different question from the capacity drop below — at the
    // critical density the jammed branch has not yet joined that line.
    let congested: Vec<&Bin> = jammed
        .iter()
        .copied()
        .filter(|b| b.density_veh_km > critical + 1e-9)
        .collect();
    // The capacity drop: a hysteresis, so it is measured AT ONE DENSITY. At each density
    // present on both branches at or above the critical one, the free branch's peak against
    // the jammed branch's peak; the answer is the largest of those, reported with the
    // density it was taken at.
    //
    // The two branches are filtered by the SAME predicate, which is the point. Comparing
    // the free branch's peak over every density against the jammed branch's peak over the
    // densities strictly above the critical one measures the two at different densities —
    // 1,736.5 veh/h at k = 25 against 1,432.0 at k = 30 in the run that motivated this —
    // and so reports the slope of the congested branch between them as if it were a drop,
    // while silently excluding the jammed bins that exist AT the critical density.
    let drop = capacity_drop(&free, &jammed, critical);
    let (slope, intercept) = least_squares(
        &congested
            .iter()
            .map(|b| (b.density_veh_km, b.flow_veh_h))
            .collect::<Vec<_>>(),
    );
    let jam_density = if slope < 0.0 {
        -intercept / slope
    } else {
        f64::NAN
    };
    let lowest = free
        .iter()
        .map(|b| b.density_veh_km)
        .fold(f64::INFINITY, f64::min);
    let free_speed = free
        .iter()
        .filter(|b| (b.density_veh_km - lowest).abs() < 1e-9)
        .map(|b| b.speed_mps)
        .fold(0.0, f64::max);
    let l_eff = params.class.spec().length_m + driver.min_gap_m;
    let t_headway = driver.time_headway_s;
    let v0 = driver.desired_speed_mps;
    let theoretical_capacity =
        (1.0 / t_headway) * (1.0 - l_eff / (v0 * t_headway + l_eff)) * 3600.0;
    FdResult {
        bins: bins.to_vec(),
        capacity_veh_h: capacity,
        critical_density_veh_km: critical,
        dynamic_capacity_veh_h: drop.map_or(capacity, |d| d.jammed_veh_h),
        capacity_drop: drop.map_or(0.0, |d| d.drop),
        capacity_drop_density_veh_km: drop.map_or(f64::NAN, |d| d.density_veh_km),
        free_flow_at_drop_veh_h: drop.map_or(capacity, |d| d.free_veh_h),
        jam_density_veh_km: jam_density,
        wave_speed_kmh: slope,
        free_speed_mps: free_speed,
        theoretical_capacity_veh_h: theoretical_capacity,
        theoretical_jam_density_veh_km: 1000.0 / l_eff,
        theoretical_wave_speed_kmh: -l_eff / t_headway * 3.6,
        congested_bins: congested.len(),
    }
}

/// One density's hysteresis.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drop {
    /// Where it was measured, veh/km.
    density_veh_km: f64,
    /// The free branch's largest flow there, veh/h.
    free_veh_h: f64,
    /// The jammed branch's largest flow there, veh/h.
    jammed_veh_h: f64,
    /// `(free − jammed) / free`, floored at zero.
    drop: f64,
}

/// The largest same-density capacity drop, and where it is.
///
/// Both branches are reduced by the same rule at the same densities: the maximum binned
/// flow at that density. Densities below the critical one are skipped — traffic that has
/// not broken down has no breakdown to recover from, and the free and jammed branches
/// coincide there — but the skip applies to both branches alike, which is what makes the
/// comparison a hysteresis rather than a difference between two points on two curves.
///
/// Returns `None` when no density was measured on both branches at or above the critical
/// density, which is what a two-density smoke run produces.
fn capacity_drop(free: &[&Bin], jammed: &[&Bin], critical: f64) -> Option<Drop> {
    // Collected into a `BTreeMap` keyed by the density's bit pattern, so the walk is in
    // density order and does not depend on the order the bins were measured in.
    let peak = |bins: &[&Bin], k: f64| {
        bins.iter()
            .filter(|b| (b.density_veh_km - k).abs() < 1e-9)
            .fold(f64::NEG_INFINITY, |best, b| best.max(b.flow_veh_h))
    };
    let mut densities: Vec<f64> = free
        .iter()
        .map(|b| b.density_veh_km)
        .filter(|k| *k >= critical - 1e-9)
        .collect();
    densities.sort_by(f64::total_cmp);
    densities.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let mut best: Option<Drop> = None;
    for k in densities {
        let free_veh_h = peak(free, k);
        let jammed_veh_h = peak(jammed, k);
        if !free_veh_h.is_finite() || !jammed_veh_h.is_finite() || free_veh_h <= 0.0 {
            continue;
        }
        let drop = ((free_veh_h - jammed_veh_h) / free_veh_h).max(0.0);
        // `>` and not `>=`, so a tie is resolved by the lowest density: the drop is a
        // breakdown phenomenon and the breakdown happens at the lowest density that shows
        // it.
        if best.is_none_or(|b| drop > b.drop) {
            best = Some(Drop {
                density_veh_km: k,
                free_veh_h,
                jammed_veh_h,
                drop,
            });
        }
    }
    best
}

/// Ordinary least squares: returns `(slope, intercept)`.
///
/// The sums are formed with [`v2xw_core::math::sum_ordered`] after sorting by the abscissa,
/// so the fit is bit-identical however the bins were collected (ADR 0004 decision 4).
pub fn least_squares(points: &[(f64, f64)]) -> (f64, f64) {
    if points.len() < 2 {
        return (f64::NAN, f64::NAN);
    }
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
    let n = sorted.len() as f64;
    let sum_x = math::sum_ordered(sorted.iter().map(|(x, _)| *x).collect::<Vec<_>>());
    let sum_y = math::sum_ordered(sorted.iter().map(|(_, y)| *y).collect::<Vec<_>>());
    let sum_xx = math::sum_ordered(sorted.iter().map(|(x, _)| x * x).collect::<Vec<_>>());
    let sum_xy = math::sum_ordered(sorted.iter().map(|(x, y)| x * y).collect::<Vec<_>>());
    let denominator = n * sum_xx - sum_x * sum_x;
    if denominator.abs() < 1e-12 {
        return (f64::NAN, f64::NAN);
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denominator;
    let intercept = (sum_y - slope * sum_x) / n;
    (slope, intercept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn least_squares_fits_a_line_exactly() {
        let points = [(0.0, 1.0), (1.0, 3.0), (2.0, 5.0), (3.0, 7.0)];
        let (slope, intercept) = least_squares(&points);
        assert!((slope - 2.0).abs() < 1e-12);
        assert!((intercept - 1.0).abs() < 1e-12);
        // And the order of the points does not matter.
        let mut reversed = points.to_vec();
        reversed.reverse();
        assert_eq!(least_squares(&reversed), (slope, intercept));
    }

    #[test]
    fn the_equilibrium_speed_is_a_zero_of_the_acceleration() {
        let idm = Idm::new(IdmPreset::Treiber2000);
        let driver = IdmPreset::Treiber2000.profile(VehicleClass::Passenger);
        for gap in [5.0, 10.0, 25.0, 60.0, 200.0] {
            let v = equilibrium_speed_mps(&idm, &driver, driver.desired_speed_mps, gap);
            let accel = idm.accel_of(v, driver.desired_speed_mps, gap, v, &driver);
            assert!(accel.abs() < 1e-6, "gap {gap} m: v = {v} m/s, a = {accel}");
        }
        // A wide gap gives the free speed; a tight one gives a crawl.
        let wide = equilibrium_speed_mps(&idm, &driver, driver.desired_speed_mps, 1000.0);
        assert!(wide > 0.99 * driver.desired_speed_mps, "{wide}");
        let tight = equilibrium_speed_mps(&idm, &driver, driver.desired_speed_mps, 1.0);
        assert!(tight < 2.0, "{tight}");
    }

    #[test]
    fn the_theoretical_diagram_matches_the_treiber_figures() {
        // The Kesting 2010 Eq. 4.1 capacity and the triangular diagram's jam density and
        // wave speed, for the Treiber 2000 set: these are arithmetic, not a simulation, and
        // they are what §2.9's targets were computed from.
        let driver = IdmPreset::Treiber2000.profile(VehicleClass::Passenger);
        let l_eff = VehicleClass::Passenger.spec().length_m + driver.min_gap_m;
        assert_eq!(l_eff, 7.0);
        let q_max = (1.0 / driver.time_headway_s)
            * (1.0 - l_eff / (driver.desired_speed_mps * driver.time_headway_s + l_eff))
            * 3600.0;
        assert!((q_max - 1988.0).abs() < 5.0, "Q_max = {q_max} veh/h");
        let jam = 1000.0 / l_eff;
        assert!((jam - 142.86).abs() < 0.1, "ρ_jam = {jam} veh/km");
        let wave = -l_eff / driver.time_headway_s * 3.6;
        assert!((wave - (-15.75)).abs() < 0.1, "w = {wave} km/h");
        // And all three are inside the §2.9 bands.
        assert!(
            (targets::UNCONGESTED_FLOW_VEH_H.0..=targets::UNCONGESTED_FLOW_VEH_H.1)
                .contains(&q_max)
        );
        assert!(relative_error(jam, targets::JAM_DENSITY_VEH_KM) < targets::ABOUT_TOLERANCE);
        assert!(relative_error(wave, targets::WAVE_SPEED_KMH) < targets::ABOUT_TOLERANCE);
    }

    #[test]
    fn the_ring_reproduces_the_fundamental_diagram_targets() {
        let result = measure(&FdParams::quick()).expect("the measurement runs");
        // Every check, printed so a failure says what it measured rather than only that it
        // failed.
        for (name, measured, target, ok) in result.report() {
            println!("{name}: measured {measured}, target {target}, in band: {ok}");
        }
        println!(
            "critical density {:.1} veh/km, dynamic capacity {:.1} veh/h, \
             free speed {:.2} m/s, congested bins {}",
            result.critical_density_veh_km,
            result.dynamic_capacity_veh_h,
            result.free_speed_mps,
            result.congested_bins
        );
        println!(
            "theoretical: capacity {:.1} veh/h, jam density {:.1} veh/km, wave {:.2} km/h",
            result.theoretical_capacity_veh_h,
            result.theoretical_jam_density_veh_km,
            result.theoretical_wave_speed_kmh
        );
        assert!(!result.bins.is_empty(), "the run produced bins");
        // The free branch: at the lowest density the vehicles travel at their desired speed.
        assert!(
            result.free_speed_mps > 0.9 * 33.3,
            "free speed {} m/s",
            result.free_speed_mps
        );
        // The congested branch exists and slopes down.
        assert!(
            result.congested_bins >= 3,
            "{} congested bins",
            result.congested_bins
        );
        assert!(
            result.wave_speed_kmh < 0.0,
            "wave speed {}",
            result.wave_speed_kmh
        );
        // The four §2.9 checks.
        assert!(
            result.capacity_in_band(),
            "capacity {} veh/h",
            result.capacity_veh_h
        );
        assert!(
            result.jam_density_in_band(),
            "jam density {} veh/km",
            result.jam_density_veh_km
        );
        assert!(
            result.wave_speed_in_band(),
            "wave speed {} km/h",
            result.wave_speed_kmh
        );
        assert!(
            result.capacity_drop_in_band(),
            "capacity drop {}",
            result.capacity_drop
        );
    }

    #[test]
    fn the_capacity_drop_compares_the_two_branches_at_one_density() {
        // The defect this pins: the drop took the free branch's maximum over ALL densities
        // and the jammed branch's maximum over the densities STRICTLY above the critical
        // one, so the two flows were measured at different densities — the free peak at
        // k = 25 against the jammed peak at k = 30 in the run that motivated this — and
        // the five jammed bins that existed AT k = 25 were silently excluded.
        //
        // Synthetic bins, so the arithmetic is visible rather than simulated.
        let bin = |branch, k: f64, q: f64| Bin {
            branch,
            density_veh_km: k,
            flow_veh_h: q,
            speed_mps: q / 3.6 / k,
            vehicles: k as usize,
        };
        let bins = vec![
            bin(Branch::Free, 20.0, 1600.0),
            bin(Branch::Jammed, 20.0, 1600.0),
            // The critical density: the free branch peaks here, and the jammed branch has
            // bins here too — the ones the strict `>` used to throw away.
            bin(Branch::Free, 25.0, 2000.0),
            bin(Branch::Jammed, 25.0, 1900.0),
            // And one density further on, where both branches are lower but the gap
            // between them is wider.
            bin(Branch::Free, 30.0, 1800.0),
            bin(Branch::Jammed, 30.0, 1440.0),
            bin(Branch::Free, 40.0, 1400.0),
            bin(Branch::Jammed, 40.0, 1330.0),
        ];
        let params = FdParams::quick();
        let driver = params.preset.profile(params.class);
        let result = fit(&params, &bins, &driver, 1000.0);
        assert_eq!(result.capacity_veh_h, 2000.0);
        assert_eq!(result.critical_density_veh_km, 25.0);
        // 20 % at k = 30, not (2000 − 1440)/2000 = 28 % across two densities.
        assert_eq!(result.capacity_drop_density_veh_km, 30.0);
        assert_eq!(result.free_flow_at_drop_veh_h, 1800.0);
        assert_eq!(result.dynamic_capacity_veh_h, 1440.0);
        assert!(
            (result.capacity_drop - 0.20).abs() < 1e-12,
            "{}",
            result.capacity_drop
        );
        // The identity a consumer can check holds exactly.
        assert!((result.capacity_drop_recomputed() - result.capacity_drop).abs() < 1e-12);
        // The jammed bins at the critical density are no longer invisible: with the
        // k = 30 pair removed, the drop is the 5 % that exists AT k = 25, and it is
        // reported there rather than being compared against k = 40.
        let without_30: Vec<Bin> = bins
            .iter()
            .copied()
            .filter(|b| b.density_veh_km != 30.0)
            .collect();
        let result = fit(&params, &without_30, &driver, 1000.0);
        assert_eq!(result.capacity_drop_density_veh_km, 25.0);
        assert!(
            (result.capacity_drop - 0.05).abs() < 1e-12,
            "{}",
            result.capacity_drop
        );
        // Below the critical density there is no breakdown to recover from, and the two
        // branches coincide there anyway; a drop invented at k = 20 would be an artefact.
        let only_low: Vec<Bin> = vec![
            bin(Branch::Free, 20.0, 1600.0),
            bin(Branch::Jammed, 20.0, 800.0),
            bin(Branch::Free, 25.0, 2000.0),
            bin(Branch::Jammed, 25.0, 2000.0),
        ];
        let result = fit(&params, &only_low, &driver, 1000.0);
        assert_eq!(result.capacity_drop, 0.0);
        assert_eq!(result.capacity_drop_density_veh_km, 25.0);
    }

    #[test]
    fn the_uncovered_rows_are_declared() {
        // §2.9 has seven rows and this measurement covers five of them. The two it does
        // not are named, so `literature-checked` is not read as wider than it is.
        let rows = targets::uncovered_rows();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.contains("convective stability")));
        assert!(rows.iter().any(|r| r.contains("time-mean")));
        assert!(
            rows[0].contains(&format!("{:.0}", targets::CONVECTIVE_STABILITY_VEH_H)),
            "the threshold itself is carried: {}",
            rows[0]
        );
    }

    #[test]
    fn the_measurement_is_reproducible() {
        let mut params = FdParams::quick();
        params.densities_veh_km = vec![10.0, 60.0];
        params.warmup = Duration::from_secs(20);
        params.measure = Duration::from_secs(60);
        let a = measure(&params).expect("runs");
        let b = measure(&params).expect("runs");
        // The bins are the measurement; the fitted fields can be NaN when a two-density run
        // has no congested branch to fit, and NaN is not equal to itself.
        assert_eq!(a.bins, b.bins, "the same parameters give the same bins");
        assert_eq!(a.capacity_veh_h, b.capacity_veh_h);
        assert_eq!(a.critical_density_veh_km, b.critical_density_veh_km);
        assert_eq!(a.capacity_drop, b.capacity_drop);
    }
}
