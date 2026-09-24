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

#[test]
fn an_equipped_pedestrian_is_refused_until_the_kernel_hosts_a_vru_device() {
    let mut scenario = fixture_scenario(10, 0);
    scenario.actors.vru.device_fraction = 0.5;
    let err = scenario.validate().expect_err("no VRU device yet");
    assert!(
        err.to_string().contains("actors.vru.device_fraction"),
        "{err}"
    );
}
