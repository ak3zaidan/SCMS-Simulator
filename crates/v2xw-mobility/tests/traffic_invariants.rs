//! The traffic invariants, held at zero on a dense signalised grid.
//!
//! `v2xw_mobility::audit` checks every vehicle at every step for the faults the owner
//! reported on the live page — cars touching, cars in buildings, a car driving round a
//! queue at a red light and through the junction, motion that jumps. This test runs a
//! dense, signalised, two-lanes-per-direction grid with buildings for three simulated
//! minutes and requires every safety class to be zero.
//!
//! A gate that cannot fail is not a gate, so the same world and demand are also run with
//! the junction rules switched off (`IntersectionMode::None`, no clearance, no no-change
//! zone), and that run must show red-light entries and conflict-zone violations. If it
//! ever stops doing so, the auditor has gone blind and this test says so. Each check's
//! own fault-injection test is in `src/audit.rs`.
//!
//! The full-size runs (Manhattan, the Midtown-sized grid) are measurements, made with
//! `cargo run -p v2xw-engine --example traffic_audit`.

use v2xw_core::rng::RngRegistry;
use v2xw_core::time::{Duration, NS_PER_S};
use v2xw_mobility::audit::{AuditParams, AuditReport, Check, TrafficAuditor};
use v2xw_mobility::demand::{OdParams, PoissonDemand, PoissonParams};
use v2xw_mobility::{EngineParams, IntersectionMode, Mobility, MobilityCtx, NativeMobility};
use v2xw_world::procedural::GridParams;
use v2xw_world::{ImportOptions, World};

/// A dense Manhattan-like grid: 2 lanes each way, 25 mph, signals, buildings, drivable
/// corners.
fn world() -> World {
    let params = GridParams {
        cols: 5,
        rows: 6,
        block_x_m: 160.0,
        block_y_m: 80.0,
        lanes_per_direction: 2,
        lane_width_m: 3.5,
        sidewalk_m: 2.0,
        speed_limit_mps: 11.176,
        signalised: true,
        cycle_s: 60.0,
        amber_s: 3.0,
        block_buildings: true,
        corner_radius_m: 4.5,
        ..GridParams::legacy()
    };
    v2xw_world::procedural::grid(&params, &ImportOptions::default()).expect("grid")
}

fn run(params: EngineParams, seconds: u64) -> AuditReport {
    let world = world();
    let rng = RngRegistry::new(0x5EED_7AFF);
    let mut engine = NativeMobility::new(params);
    let demand = PoissonDemand::new(
        &world,
        PoissonParams {
            arrival_rate_per_s: 1.5,
            duration: Duration::from_secs(seconds),
            ..PoissonParams::default()
        },
        OdParams::default(),
    )
    .expect("demand");
    {
        let mut ctx = MobilityCtx::new(0, &world, &rng);
        engine.init(&mut ctx, Box::new(demand)).expect("init");
    }
    let mut auditor = TrafficAuditor::new(&world, AuditParams::default());
    auditor.audit_world(&world);
    let mut t = 0u64;
    while t < seconds * NS_PER_S {
        let mut ctx = MobilityCtx::new(t, &world, &rng);
        let update = engine.step(&mut ctx, params.step);
        let actors = engine.audit_actors(&world, update.t);
        auditor.observe(&world, t, update.t, &actors, &update.despawned);
        t = update.t;
    }
    auditor.report()
}

/// The classes that must be zero: every one a collision, a rule broken or a physically
/// impossible motion.
const SAFETY: [Check; 16] = [
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
    Check::HeadingFlip,
    Check::SpeedJump,
    Check::AccelBound,
    Check::MidRoadDespawn,
];

#[test]
fn a_dense_signalised_grid_holds_every_traffic_invariant() {
    let report = run(EngineParams::default(), 180);
    eprintln!("{:?}\n{:?}", report.stats, report.counts);
    let failing: Vec<String> = SAFETY
        .iter()
        .filter(|c| report.count(**c) > 0)
        .map(|c| format!("{} = {}", c.label(), report.count(*c)))
        .collect();
    let examples: Vec<&v2xw_mobility::audit::Example> = report
        .examples
        .iter()
        .filter(|e| SAFETY.contains(&e.check))
        .take(10)
        .collect();
    assert!(
        failing.is_empty(),
        "traffic invariants violated: {failing:?}\nexamples: {examples:#?}"
    );
    // The run was a real one: enough traffic, junctions entered on signals, lane
    // changes made, and no gridlock.
    assert!(report.stats.peak_vehicles >= 60, "{:?}", report.stats);
    assert!(report.stats.signalised_entries >= 100, "{:?}", report.stats);
    assert!(report.stats.lane_changes > 0, "{:?}", report.stats);
    assert_eq!(report.count(Check::Standstill), 0, "{:?}", report.stats);
    // The world itself is clean.
    for c in [
        Check::WorldInternalOutsideJunction,
        Check::WorldLaneInBuilding,
        Check::WorldConflictingGreens,
    ] {
        assert_eq!(report.count(c), 0, "{}", c.label());
    }
    // Motion is physically plausible: the jerk and heading-rate bounds are exceeded on
    // well under one vehicle-step in a thousand.
    let steps = report.stats.vehicle_steps.max(1) as f64;
    for c in [Check::Jerk, Check::HeadingJump] {
        let share = report.count(c) as f64 / steps;
        assert!(
            share < 1e-3,
            "{} on {:.4} % of vehicle-steps",
            c.label(),
            100.0 * share
        );
    }
}

#[test]
fn the_auditor_sees_the_violations_of_a_run_without_junction_rules() {
    // The control: nobody stops for anything. If this run came back clean the auditor
    // would be blind, and the zero above would mean nothing.
    let params = EngineParams {
        intersections: IntersectionMode::None,
        junction_clearance: false,
        no_change_zone_m: 0.0,
        ..EngineParams::default()
    };
    let report = run(params, 120);
    assert!(
        report.count(Check::RedEntry) > 0,
        "no red entries in a run that ignores signals: {:?}",
        report.counts
    );
    assert!(
        report.count(Check::ConflictZone) > 0 || report.count(Check::Overlap) > 0,
        "no conflicts in a run that ignores right of way: {:?}",
        report.counts
    );
}
