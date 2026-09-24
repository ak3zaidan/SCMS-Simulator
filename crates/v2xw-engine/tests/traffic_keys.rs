//! The traffic keys of the scenario reach the traffic: `weather.*`, `actors.vehicles.classes`
//! and `actors.vehicles.demand.kind` each change what the vehicles do.
//!
//! Every assertion compares a run with the key set against the same run without it, so a
//! key that silently stopped reaching the road would fail here rather than pass.

use v2xw_core::rng::RngRegistry;
use v2xw_core::weather::{SurfaceCondition, WeatherKind};
use v2xw_engine::Scenario;
use v2xw_mobility::{Mobility, MobilityCtx, VehicleClass};

/// A signalised procedural grid with steady Poisson demand.
fn grid_scenario() -> Scenario {
    let mut s = Scenario::minimal();
    s.world.source = v2xw_world::WorldSourceSpec::procedural(
        "world/source/procedural-grid",
        serde_json::json!({
            "cols": 5, "rows": 5, "block_x_m": 150.0, "block_y_m": 150.0,
            "lanes_per_direction": 2, "speed_limit_mps": 13.89, "signalised": true,
            "corner_radius_m": 4.5
        }),
    );
    s.time.duration_s = 120.0;
    s.actors.vehicles.demand.kind = "mobility/demand/poisson".to_string();
    s.actors.vehicles.demand.rate_veh_per_h = Some(2400.0);
    s
}

/// Mean speed over every vehicle-step, and the classes that spawned.
fn run(s: &Scenario) -> (f64, Vec<VehicleClass>) {
    s.validate().expect("the scenario validates");
    let world = v2xw_engine::wiring::build_world(s).expect("world");
    let rng = RngRegistry::new(s.seed);
    let mut mobility = v2xw_engine::wiring::native_mobility(s);
    mobility.set_weather(v2xw_engine::wiring::initial_weather(s));
    {
        let demand = v2xw_engine::wiring::build_demand(s, &world).expect("demand");
        let mut ctx = MobilityCtx::new(0, &world, &rng);
        mobility.init(&mut ctx, demand).expect("init");
    }
    let step = s.time.mobility_step();
    let (mut sum, mut n) = (0.0, 0u64);
    let mut classes = Vec::new();
    let mut t = 0u64;
    while t < (s.time.duration_s * 1e9) as u64 {
        let mut ctx = MobilityCtx::new(t, &world, &rng);
        let update = mobility.step(&mut ctx, step);
        classes.extend(update.spawned.iter().map(|sp| sp.class));
        for (_, k) in &update.states {
            sum += k.ground_speed_mps();
            n += 1;
        }
        t = update.t;
    }
    (sum / n.max(1) as f64, classes)
}

#[test]
fn snow_and_fog_slow_the_traffic_down() {
    let clear = grid_scenario();
    let mut snow = grid_scenario();
    snow.weather.initial = WeatherKind::Snow;
    snow.weather.intensity = 0.9;
    snow.weather.visibility_m = Some(60.0);
    snow.weather.surface = Some(SurfaceCondition::Snow);
    let (v_clear, _) = run(&clear);
    let (v_snow, _) = run(&snow);
    assert!(v_clear > 3.0, "the clear run moves: {v_clear} m/s");
    // The FHWA arterial snow band's midpoint is a 35 % speed cut, and 60 m of visibility
    // caps the desired speed near 13 m/s; together they must show.
    assert!(
        v_snow < 0.85 * v_clear,
        "snow {v_snow} m/s against clear {v_clear} m/s"
    );
}

#[test]
fn the_class_shares_are_the_fleet() {
    let mut s = grid_scenario();
    for (name, fraction) in [("passenger", 0.5), ("bus", 0.5)] {
        s.actors.vehicles.classes.insert(
            name.to_string(),
            v2xw_engine::scenario::schema::VehicleClassSpec {
                fraction,
                obu: None,
            },
        );
    }
    let (_, classes) = run(&s);
    let buses = classes.iter().filter(|c| **c == VehicleClass::Bus).count();
    let cars = classes
        .iter()
        .filter(|c| **c == VehicleClass::Passenger)
        .count();
    assert!(classes.len() >= 20, "{} vehicles spawned", classes.len());
    assert_eq!(buses + cars, classes.len(), "only the two classes named");
    let share = buses as f64 / classes.len() as f64;
    assert!((0.3..0.7).contains(&share), "bus share {share}");
    // And without the key the fleet is cars only.
    let (_, default) = run(&grid_scenario());
    assert!(default.iter().all(|c| *c == VehicleClass::Passenger));
}

#[test]
fn the_demand_kind_selects_the_model() {
    let mut drop = grid_scenario();
    drop.actors.vehicles.demand.kind = "mobility/demand/tr36885-drop".to_string();
    drop.actors.vehicles.demand.rate_veh_per_h = None;
    drop.time.duration_s = 5.0;
    let (_, placed) = run(&drop);
    // The drop model places its whole population at the start; Poisson at 2400 veh/h
    // would bring about three vehicles in five seconds.
    assert!(placed.len() > 50, "the drop placed {}", placed.len());
    let mut none = grid_scenario();
    none.actors.vehicles.demand.kind = "mobility/demand/none".to_string();
    none.time.duration_s = 5.0;
    assert!(run(&none).1.is_empty());
}
