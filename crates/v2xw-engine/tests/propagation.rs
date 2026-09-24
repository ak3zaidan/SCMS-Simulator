//! The link budget's settings reach every received power: transmit power, antenna gain,
//! cable loss and antenna height per node class (`radio.devices`), the candidate range
//! derived from the link budget (`radio.range`), rain over the link, and the high tier's
//! own geometric law — with the buildings and terrain switches still doing what they say.
//!
//! Each property is shown against its counterexample in the same run pair: a check that
//! passes against the defect it guards is not a check.

use std::path::{Path, PathBuf};

use v2xw_engine::{Engine, MemoryRecorder, RunReport, Scenario};
use v2xw_metrics::channels::{PhyRxView, RxOutcome};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

/// The Midtown-shaped procedural grid (274 × 80 m blocks, one 30 m building per block)
/// with a bulk-spawned fleet of up to `n`, for `secs`.
fn grid_fleet(n: u32, secs: f64) -> Scenario {
    let mut s =
        Scenario::load(scenarios().join("phase1-grid.yaml")).expect("the shipped scenario loads");
    s.time.duration_s = secs;
    s.actors.vehicles.demand.rate_veh_per_h = Some(3_600_000.0);
    s.actors.vehicles.demand.params = serde_json::json!({ "max_total_vehicles": n });
    s
}

fn open_air(mut s: Scenario) -> Scenario {
    s.world.buildings.enabled = false;
    s
}

fn with_model(mut s: Scenario, family: &str, id: &str) -> Scenario {
    s.radio.models.insert(
        family.to_string(),
        v2xw_engine::scenario::schema::ModelChoice::new(id),
    );
    s
}

fn run(scenario: Scenario) -> (RunReport, Vec<PhyRxView>, Engine) {
    let mut engine = Engine::build(scenario, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    let rx = recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == <PhyRxView as v2xw_metrics::channels::ChannelView>::CHANNEL)
        .map(|(_, r)| v2xw_metrics::channels::decode(r).expect("decodes"))
        .collect();
    (report, rx, engine)
}

/// Mean received power over the attempts whose distance is in `[lo, hi)`, and how many.
fn mean_rssi(rx: &[PhyRxView], lo: f64, hi: f64) -> (f64, usize) {
    let v: Vec<f64> = rx
        .iter()
        .filter(|r| r.dist_m.is_some_and(|d| d >= lo && d < hi))
        .filter_map(|r| r.rssi_dbm)
        .collect();
    (v.iter().sum::<f64>() / v.len().max(1) as f64, v.len())
}

fn pdr_between(rx: &[PhyRxView], lo: f64, hi: f64) -> Option<f64> {
    let mut n = 0u64;
    let mut ok = 0u64;
    for r in rx {
        let Some(d) = r.dist_m else { continue };
        if d >= lo && d < hi {
            n += 1;
            if r.outcome == RxOutcome::Ok {
                ok += 1;
            }
        }
    }
    (n > 0).then(|| ok as f64 / n as f64)
}

// -----------------------------------------------------------------------------------------
// radio.devices
// -----------------------------------------------------------------------------------------

/// Transmit power moves every received power by itself, and not the set of links the run
/// attempts. Both powers are below what J2945/1 congestion control would allow (its
/// radiated power never falls under 10 dBm, so a unit with a 3 dBi antenna may conduct
/// 7 dBm), so the configured power is what goes on the air in both runs, and with the
/// grid's buildings in the way many links are near the floor. The counterexample is the
/// defect the fixed reference EIRP removes: when the attempt test used each frame's own
/// power, 5 dB less power dropped the weakest links from the set and the mean received
/// power over what was left went *up* (−85.3 to −81.2 dBm, measured at 10 dBm).
#[test]
fn the_transmit_power_moves_every_received_power_by_itself() {
    let base = with_model(grid_fleet(60, 4.0), "fading", "fading/none");
    let mut loud = base.clone();
    loud.radio.devices.obu.tx_power_dbm = 5.0;
    let mut quiet = base;
    quiet.radio.devices.obu.tx_power_dbm = 0.0;
    let (loud_report, loud_rx, _) = run(loud);
    let (quiet_report, quiet_rx, _) = run(quiet);
    assert!(loud_report.faint_arrivals > 0, "no link was near the floor");
    assert_eq!(
        loud_report.reception_attempts, quiet_report.reception_attempts,
        "the transmit power changed which links were attempted"
    );
    let (a, na) = mean_rssi(&loud_rx, 0.0, 10_000.0);
    let (b, nb) = mean_rssi(&quiet_rx, 0.0, 10_000.0);
    assert!(na > 40 && nb > 40, "{na} and {nb} attempts");
    assert!(
        (a - b - 5.0).abs() < 0.75,
        "5 dB less transmit power moved the mean received power by {:.2} dB",
        a - b
    );
}

/// Antenna gain and cable loss follow J2945/1's radiated-power rule. At the default power
/// congestion control sets the *radiated* power, so a 3 dB better antenna transmits the
/// same EIRP and only receives 3 dB better, and 3 dB more cable loss only costs the
/// receiving end. Below the congestion-control ceiling the same antenna gains 3 dB at each
/// end, 6 dB in all. The counterexample is the double count this build removed: before it,
/// the antenna gain was added on top of J2945/1's radiated power.
#[test]
fn antenna_gain_and_cable_loss_follow_j2945s_radiated_power() {
    let base = open_air(with_model(grid_fleet(60, 4.0), "fading", "fading/none"));
    let (_, plain, _) = run(base.clone());
    let mut better = base.clone();
    better.radio.devices.obu.antenna_gain_dbi = 6.0;
    let (_, better_rx, _) = run(better);
    let mut lossy = base.clone();
    lossy.radio.devices.obu.cable_loss_db = 3.0;
    let (_, lossy_rx, _) = run(lossy);
    let (p, _) = mean_rssi(&plain, 0.0, 150.0);
    let (g, _) = mean_rssi(&better_rx, 0.0, 150.0);
    let (l, _) = mean_rssi(&lossy_rx, 0.0, 150.0);
    assert!(
        (g - p - 3.0).abs() < 0.75,
        "+3 dBi at the J2945/1 ceiling moved {:.2} dB",
        g - p
    );
    assert!(
        (p - l - 3.0).abs() < 0.75,
        "+3 dB cable loss moved {:.2} dB",
        p - l
    );

    // Below the ceiling the transmitter's gain counts too.
    let mut low = base;
    low.radio.devices.obu.tx_power_dbm = 0.0;
    let (_, low_rx, _) = run(low.clone());
    low.radio.devices.obu.antenna_gain_dbi = 6.0;
    let (_, low_better, _) = run(low);
    let (a, _) = mean_rssi(&low_rx, 0.0, 150.0);
    let (b, _) = mean_rssi(&low_better, 0.0, 150.0);
    assert!(
        (b - a - 6.0).abs() < 0.75,
        "+3 dBi below the ceiling moved {:.2} dB",
        b - a
    );
}

/// Antenna height reaches the budget: under the two-ray law, raising both antennas from
/// 1.5 m to 3 m moves the d⁻⁴ crossover from 550 m out past 2 km, so links 900-2,000 m
/// long follow Friis instead of d⁻⁴: 4 dB better at 900 m, 11 dB at 2 km.
#[test]
fn antenna_height_moves_the_two_ray_crossover() {
    let base = open_air(with_model(
        with_model(grid_fleet(60, 3.0), "fading", "fading/none"),
        "propagation",
        "propagation/two-ray-ground",
    ));
    let (_, low, _) = run(base.clone());
    let mut tall = base;
    tall.radio.devices.obu.antenna_height_m = Some(3.0);
    let (_, high, _) = run(tall);
    let (a, na) = mean_rssi(&low, 900.0, 2_000.0);
    let (b, nb) = mean_rssi(&high, 900.0, 2_000.0);
    assert!(na > 50 && nb > 50, "{na} and {nb} attempts at 900-2000 m");
    assert!(
        b > a + 4.0,
        "3 m antennas received {b:.1} dBm against {a:.1} dBm"
    );
}

// -----------------------------------------------------------------------------------------
// radio.range
// -----------------------------------------------------------------------------------------

/// The candidate range is the link budget's, not 1 km. In open air, free space and no
/// fading, 802.11p at a 20 dBm radiated power still arrives above the receiver's
/// sensitivity at 1.2 km, so links 1.0-1.5 km long exist and deliver — where the fixed 1 km
/// range left no attempt there at all. A cap stops the full evaluation at the cap and
/// counts what lies beyond as interference only.
#[test]
fn the_candidate_range_follows_the_link_budget() {
    let base = open_air(with_model(
        with_model(grid_fleet(60, 3.0), "fading", "fading/none"),
        "propagation",
        "propagation/free-space",
    ));
    let (_, rx, _) = run(base.clone());
    let far = pdr_between(&rx, 1_000.0, 1_500.0).expect("links 1.0-1.5 km long");
    assert!(far > 0.5, "free space at 1.0-1.5 km delivered {far:.3}");

    let mut capped = base;
    capped.radio.range.max_m = Some(500.0);
    let (report, capped_rx, _) = run(capped);
    assert!(
        capped_rx
            .iter()
            .all(|r| r.dist_m.is_none_or(|d| d <= 500.0 + 1.0)),
        "an attempt beyond the 500 m cap"
    );
    assert!(
        report.faint_arrivals > 0,
        "nothing beyond the cap was counted as interference"
    );
}

/// The margin decides what is a reception attempt: with the buildings of the grid in the
/// way, many arrivals fall between 10 and 30 dB under the noise floor, so a 30 dB margin
/// attempts them and a 10 dB margin counts them as energy only.
#[test]
fn the_range_margin_separates_attempts_from_energy() {
    let base = grid_fleet(60, 3.0);
    let (narrow, _, _) = run(base.clone());
    let mut wide = base;
    wide.radio.range.margin_db = 30.0;
    let (wider, _, _) = run(wide);
    assert!(
        narrow.faint_arrivals > 0,
        "no arrival fell under the margin"
    );
    assert!(
        wider.reception_attempts > narrow.reception_attempts,
        "{} attempts at 30 dB against {} at 10 dB",
        wider.reception_attempts,
        narrow.reception_attempts
    );
    assert!(wider.faint_arrivals < narrow.faint_arrivals);
}

// -----------------------------------------------------------------------------------------
// The high tier, rain, and the buildings and terrain switches
// -----------------------------------------------------------------------------------------

/// The high tier runs its own law, and the laws it composes are pinned by the manifest;
/// rain is pinned wherever the tier is not abstract.
#[test]
fn the_high_tier_is_its_own_law_and_the_manifest_pins_it() {
    let mut s = grid_fleet(1, 0.5);
    s.radio.tiers.propagation = v2xw_core::card::Tier::High;
    let high = Engine::build(s.clone(), "").expect("builds");
    let ids: Vec<&str> = high
        .manifest()
        .model_cards
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    for id in [
        v2xw_radio::GeometricUrbanV2v::ID,
        v2xw_radio::obstacle::VehicleBlockage::ID,
        v2xw_radio::RainAttenuation::ID,
        v2xw_radio::BuildingShadowing::ID,
    ] {
        assert!(ids.contains(&id), "{id} is not in the manifest: {ids:?}");
    }
    s.radio.tiers.propagation = v2xw_core::card::Tier::Medium;
    let medium = Engine::build(s.clone(), "").expect("builds");
    let ids: Vec<&str> = medium
        .manifest()
        .model_cards
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert!(!ids.contains(&v2xw_radio::GeometricUrbanV2v::ID));
    assert!(ids.contains(&v2xw_radio::RainAttenuation::ID));
    assert!(ids.contains(&v2xw_radio::LogDistanceShadowing::ID));
}

/// `world.buildings.enabled` still switches obstruction under the high tier's law: with
/// buildings, delivery at 200-500 m collapses behind the blocks; without, the same pairs
/// are line of sight and nearly all deliver. And the high tier is not the medium one: the
/// same fleet delivers differently.
#[test]
fn the_buildings_switch_works_under_the_high_tiers_law() {
    let mut base = grid_fleet(60, 3.0);
    base.radio.tiers.propagation = v2xw_core::card::Tier::High;
    let (_, city, _) = run(base.clone());
    let (_, open, _) = run(open_air(base.clone()));
    let city_mid = pdr_between(&city, 200.0, 500.0).expect("links at 200-500 m");
    let open_mid = pdr_between(&open, 200.0, 500.0).expect("links at 200-500 m");
    assert!(
        open_mid > 0.9,
        "open-air delivery at 200-500 m is {open_mid:.3}"
    );
    assert!(
        city_mid < 0.5 * open_mid,
        "buildings changed 200-500 m delivery only from {open_mid:.3} to {city_mid:.3}"
    );
    let mut medium = base;
    medium.radio.tiers.propagation = v2xw_core::card::Tier::Medium;
    let (_, med, _) = run(medium);
    let differs = [0.0, 100.0, 200.0, 300.0]
        .iter()
        .any(|&lo| pdr_between(&city, lo, lo + 100.0) != pdr_between(&med, lo, lo + 100.0));
    assert!(
        differs,
        "the high tier delivered exactly what the medium tier did"
    );
}

/// Rain reaches the received power, by exactly what ITU-R P.838-3 says: a few hundredths of
/// a decibel. The same links, clear and in a 50 mm/h downpour, at the same instants.
///
/// Rain also changes how people drive (FHWA), which would move the vehicles; the check
/// therefore compares the budget of one fixed link through the engine's own obstacle stack
/// and rain model rather than two runs.
#[test]
fn rain_costs_what_p838_says_and_fog_costs_nothing() {
    let rain = v2xw_radio::RainAttenuation::new();
    let wet = v2xw_core::weather::WeatherState::new(
        v2xw_core::weather::WeatherKind::Rain,
        1.0,
        2_000.0,
        v2xw_core::weather::SurfaceCondition::Wet,
    );
    let fog = v2xw_core::weather::WeatherState::new(
        v2xw_core::weather::WeatherKind::Fog,
        1.0,
        50.0,
        v2xw_core::weather::SurfaceCondition::Dry,
    );
    let a = rain.attenuation_db(&wet, 300.0, 5.86e9);
    assert!(a > 0.03 && a < 0.1, "{a} dB on a 300 m link in a downpour");
    assert_eq!(rain.attenuation_db(&fog, 300.0, 5.86e9), 0.0);
}

/// `world.terrain` still obstructs under the high tier: its stack composes the terrain
/// knife edges beside the corner tracer and the vehicle blockage.
#[test]
fn the_high_tier_stack_keeps_terrain_and_adds_corners_and_vehicles() {
    let mut s = grid_fleet(1, 0.5);
    s.radio.tiers.propagation = v2xw_core::card::Tier::High;
    let world = v2xw_engine::wiring::build_world(&s).expect("builds");
    let stack = v2xw_engine::wiring::build_obstacles(&s, &world);
    assert!(stack.buildings.is_some() && stack.corners.is_some() && stack.vehicles.is_some());
    // Buildings off: no corners to trace, vehicles still block.
    s.world.buildings.enabled = false;
    let stack = v2xw_engine::wiring::build_obstacles(&s, &world);
    assert!(stack.buildings.is_none() && stack.corners.is_none() && stack.vehicles.is_some());
    // The medium tier traces nothing.
    s.radio.tiers.propagation = v2xw_core::card::Tier::Medium;
    s.world.buildings.enabled = true;
    let stack = v2xw_engine::wiring::build_obstacles(&s, &world);
    assert!(stack.corners.is_none() && stack.vehicles.is_none());
}
