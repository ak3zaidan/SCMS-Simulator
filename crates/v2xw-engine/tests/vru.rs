//! `actors.vru` reaches the road: pedestrians walk the sidewalks and cyclists ride, on an
//! OpenStreetMap world, through the kernel's own wiring.
//!
//! Until this build the loader refused any VRU count, because nothing spawned one. The
//! test drives the Midtown fixture extract (tests/fixtures/midtown-6block.osm.xml, which
//! carries 185 footways) for a simulated minute and checks the population is there, stays
//! at its size, keeps to walkable lanes, and moves.

use std::collections::BTreeMap;
use std::path::PathBuf;

use v2xw_core::rng::RngRegistry;
use v2xw_engine::Scenario;
use v2xw_mobility::vru::social_force::SocialForce;
use v2xw_mobility::{Mobility, MobilityCtx, VehicleClass};
use v2xw_world::WorldSourceSpec;

fn fixture_scenario(pedestrians: u32, cyclists: u32) -> Scenario {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut s = Scenario::minimal();
    s.world.source = WorldSourceSpec::OsmXml {
        path: root
            .join("tests/fixtures/midtown-6block.osm.xml")
            .to_string_lossy()
            .into_owned(),
        bbox: None,
    };
    s.world.highway_preset = Some(v2xw_world::osm::HighwayPreset::UrbanUsNyc);
    s.world.imported_at = "2026-09-18T00:00:00Z".to_string();
    s.actors.vru.pedestrians = pedestrians;
    s.actors.vru.cyclists = cyclists;
    s
}

#[test]
fn pedestrians_and_cyclists_walk_and_ride_on_an_osm_world() {
    let scenario = fixture_scenario(25, 4);
    scenario.validate().expect("a VRU population validates");
    let world = v2xw_engine::wiring::build_world(&scenario).expect("the fixture imports");
    let rng = RngRegistry::new(scenario.seed);
    let mut mobility = v2xw_engine::wiring::native_mobility(&scenario);
    {
        let demand = v2xw_engine::wiring::build_demand(&scenario, &world).expect("demand");
        let mut ctx = MobilityCtx::new(0, &world, &rng);
        mobility.init(&mut ctx, demand).expect("init");
    }
    let step = scenario.time.mobility_step();
    let mut first_seen: BTreeMap<u32, v2xw_core::geom::Vec3> = BTreeMap::new();
    let mut last: BTreeMap<u32, v2xw_core::geom::Vec3> = BTreeMap::new();
    let mut classes: BTreeMap<u32, VehicleClass> = BTreeMap::new();
    let mut t = 0u64;
    let mut pedestrians_now = 0usize;
    let mut cyclists_now = 0usize;
    while t < 60_000_000_000 {
        let mut ctx = MobilityCtx::new(t, &world, &rng);
        let update = mobility.step(&mut ctx, step);
        for s in &update.spawned {
            classes.insert(s.actor.index(), s.class);
        }
        pedestrians_now = 0;
        cyclists_now = 0;
        for (actor, k) in &update.states {
            let id = actor.index();
            match classes.get(&id) {
                Some(VehicleClass::Pedestrian) => {
                    pedestrians_now += 1;
                    // On a walkable lane, always.
                    let lane = k.lane.expect("a pedestrian is on a lane").lane;
                    assert!(
                        SocialForce::is_walkable(&world, lane),
                        "pedestrian {id} on lane {lane:?}, which is not walkable"
                    );
                }
                Some(VehicleClass::Bicycle) => {
                    cyclists_now += 1;
                    assert!(
                        k.ground_speed_mps() <= 6.0,
                        "cyclist {id} at {} m/s",
                        k.ground_speed_mps()
                    );
                }
                _ => {}
            }
            first_seen.entry(id).or_insert(k.pos);
            last.insert(id, k.pos);
        }
        t = update.t;
    }
    // The population holds at its size.
    assert_eq!(pedestrians_now, 25, "pedestrians on the world at the end");
    assert_eq!(cyclists_now, 4, "cyclists on the world at the end");
    // And they move: most walkers and every rider cover ground in a minute.
    let moved = |class: VehicleClass| {
        classes
            .iter()
            .filter(|(_, c)| **c == class)
            .filter(|(id, _)| {
                first_seen
                    .get(id)
                    .zip(last.get(id))
                    .is_some_and(|(a, b)| a.distance_2d(*b) > 5.0)
            })
            .count()
    };
    assert!(
        moved(VehicleClass::Pedestrian) >= 15,
        "{} walkers moved",
        moved(VehicleClass::Pedestrian)
    );
    assert!(
        moved(VehicleClass::Bicycle) >= 3,
        "{} riders moved",
        moved(VehicleClass::Bicycle)
    );
}

/// Runs the fixture with `pedestrians` equipped pedestrians (and a little vehicle
/// traffic) for `seconds`, on the message sets given; returns the records and the report.
fn run_equipped(
    sets: &[&str],
    seconds: f64,
) -> (v2xw_engine::MemoryRecorder, v2xw_engine::RunReport) {
    // A small signalised grid with sidewalks, crosswalks and cyclists on the carriageway.
    let mut scenario = Scenario::minimal();
    scenario.world.source = WorldSourceSpec::procedural(
        "world/source/procedural-grid",
        serde_json::json!({
            "cols": 4, "rows": 4, "block_x_m": 120.0, "block_y_m": 90.0,
            "lanes_per_direction": 1, "sidewalk_m": 2.0, "crossings": true,
            "signalised": true, "corner_radius_m": 4.5, "bicycles_on_roads": true
        }),
    );
    scenario.actors.vru.pedestrians = 12;
    scenario.actors.vru.device_fraction = 1.0;
    scenario.actors.vehicles.equipped_fraction = 1.0;
    scenario.actors.vehicles.demand.kind = "mobility/demand/poisson".to_string();
    scenario.actors.vehicles.demand.rate_veh_per_h = Some(3600.0);
    scenario.messages.sets = sets.iter().map(|s| (*s).to_string()).collect();
    scenario.time.duration_s = seconds;
    scenario.validate().expect("an equipped VRU population validates");
    let mut engine = v2xw_engine::Engine::build(scenario, "").expect("builds");
    let mut recorder = v2xw_engine::MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (recorder, report)
}

/// The `msg_type` of every record on `channel`, with its node fields.
fn by_type(
    recorder: &v2xw_engine::MemoryRecorder,
    channel: &str,
) -> Vec<serde_json::Value> {
    recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == channel)
        .map(|(_, r)| serde_json::from_slice(&r.json).expect("json"))
        .collect()
}

/// `actors.vru.device_fraction` equips pedestrians with a VRU device, hosted in the node
/// phase: on the SAE stack each sends signed SAE J2735 PSMs, which appear on `node.tx`
/// beside the vehicles' BSMs, and other nodes' receptions of them appear on `node.rx`.
#[test]
fn equipped_pedestrians_send_psms_that_other_nodes_hear() {
    let (recorder, report) = run_equipped(&["bsm"], 30.0);
    assert!(report.vru_devices_created >= 12, "{report:?}");
    let tx = by_type(&recorder, "node.tx");
    let psm: Vec<&serde_json::Value> = tx.iter().filter(|r| r["msg_type"] == "psm").collect();
    assert!(!psm.is_empty(), "no PSM on node.tx");
    // A pedestrian sends PSMs and nothing else; a vehicle sends BSMs.
    let psm_nodes: std::collections::BTreeSet<u64> =
        psm.iter().filter_map(|r| r["node"].as_u64()).collect();
    for r in &tx {
        if psm_nodes.contains(&r["node"].as_u64().unwrap_or(u64::MAX)) {
            assert_eq!(r["msg_type"], "psm", "{r}");
        }
    }
    assert!(tx.iter().any(|r| r["msg_type"] == "bsm"), "no BSM either: {report:?}");
    // Signed like a vehicle's: an envelope around the payload.
    assert!(psm.iter().all(|r| r["envelope_bytes"].as_u64().unwrap_or(0) > 0));
    // Heard: a reception attempt of a PSM decoded somewhere.
    let rx = by_type(&recorder, "node.rx");
    let heard = rx
        .iter()
        .filter(|r| r["msg_type"] == "psm" && r["outcome"] == "delivered")
        .count();
    assert!(heard > 0, "no PSM delivered: {} psm attempts", rx.iter().filter(|r| r["msg_type"] == "psm").count());
    // And a pedestrian's own device hears the vehicles.
    assert!(
        rx.iter().any(|r| psm_nodes.contains(&r["rx"].as_u64().unwrap_or(u64::MAX))
            && r["msg_type"] == "bsm"),
        "no pedestrian received a BSM"
    );
    // Its queues and load are published like a vehicle's.
    let telemetry = by_type(&recorder, "node.telemetry");
    assert!(
        telemetry
            .iter()
            .any(|r| psm_nodes.contains(&r["node"].as_u64().unwrap_or(u64::MAX))),
        "no telemetry window from a pedestrian's device"
    );
}

/// On the ETSI stack the same pedestrians send VAMs.
#[test]
fn on_the_etsi_stack_equipped_pedestrians_send_vams() {
    let (recorder, _) = run_equipped(&["cam"], 12.0);
    let tx = by_type(&recorder, "node.tx");
    assert!(tx.iter().any(|r| r["msg_type"] == "vam"), "no VAM on node.tx");
    assert!(!tx.iter().any(|r| r["msg_type"] == "psm"));
}
