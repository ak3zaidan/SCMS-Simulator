//! `delivery_curve` — delivery versus distance, run through the engine, and the same
//! curve split by the geometry of each link, for comparing the propagation laws with
//! published urban measurements.
//!
//! ```text
//! cargo run -p v2xw-engine --example delivery_curve -- <scenario.yaml> \
//!     [--tier medium|high] [--fleet N] [--secs S] [--margin DB] [--max-m M] \
//!     [--buildings on|off] [--geometry PAIRS]
//! ```
//!
//! **The run.** The scenario is run headless with the given propagation tier, a
//! bulk-spawned fleet of `N` and `S` seconds, and the delivery ratio is printed per 100 m
//! of link length over the reception attempts, with the wall time the run took. With
//! `--margin 40 --max-m 1000` every pair within 1 km is an attempt, which is the
//! definition the engine used before the candidate range came from the link budget.
//!
//! **The geometry.** `--geometry PAIRS` samples that many pairs of points on driving
//! lanes, 1.5 m antennas, and classifies each link the way the high tier does: line of
//! sight, round one street corner (where the two ends' streets cross, from each lane's
//! heading), or blocked with no single corner. For each class and 100 m bin it prints the share of links and the probability
//! the received power clears the 802.11p sensitivity (EN 302 663 static, 6 Mbit/s,
//! −88 dBm) at J2945/1's 20 dBm radiated power and a 3 dBi receive antenna, under each law's
//! own log-normal shadowing and no fast fading or interference — the medium tier's
//! (Abbas 2015 LOS, Sommer through buildings capped at TR 37.885 NLOS) and the high
//! tier's (TR 37.885 LOS, Mangel 2011 round the corner, TR 37.885 NLOS without one).
//! Published measurements are per geometry — a car driving past a roadside unit, two cars
//! approaching one intersection — so this split is what a measurement can be held
//! against; an all-pairs city curve mixes the classes in the proportions of one map.

use std::time::Instant;

use v2xw_engine::{Engine, MemoryRecorder, Scenario};
use v2xw_metrics::channels::{PhyRxView, RxOutcome};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let path = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("usage: delivery_curve <scenario.yaml> [--tier T] [--fleet N] [--secs S] ...")?;
    let mut scenario = Scenario::load(path)?;
    if let Some(t) = value("--tier") {
        scenario.radio.tiers.propagation = serde_json::from_value(serde_json::json!(t))?;
    }
    if let Some(n) = value("--fleet") {
        scenario.actors.vehicles.demand.rate_veh_per_h = Some(3_600_000.0);
        scenario.actors.vehicles.demand.params =
            serde_json::json!({ "max_total_vehicles": n.parse::<u32>()? });
    }
    if let Some(s) = value("--secs") {
        scenario.time.duration_s = s.parse()?;
    }
    if let Some(m) = value("--margin") {
        scenario.radio.range.margin_db = m.parse()?;
    }
    if let Some(m) = value("--max-m") {
        scenario.radio.range.max_m = Some(m.parse()?);
    }
    if let Some(b) = value("--buildings") {
        scenario.world.buildings.enabled = b == "on";
    }

    if let Some(pairs) = value("--geometry") {
        return geometry(&scenario, pairs.parse()?);
    }

    let t0 = Instant::now();
    let mut engine = Engine::build(scenario.clone(), "")?;
    let built = t0.elapsed().as_secs_f64();
    let mut recorder = MemoryRecorder::new();
    let t1 = Instant::now();
    let report = engine.run(&mut recorder)?;
    let ran = t1.elapsed().as_secs_f64();
    let rx: Vec<PhyRxView> = recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == <PhyRxView as v2xw_metrics::channels::ChannelView>::CHANNEL)
        .map(|(_, r)| v2xw_metrics::channels::decode(r))
        .collect::<Result<_, _>>()?;
    println!(
        "tier {} | buildings {} | margin {} dB | cap {:?} | {} actors, {} frames, {} attempts, \
         {} faint | pdr {:.3} | build {built:.1} s, run {ran:.1} s",
        scenario.radio.tiers.propagation,
        scenario.world.buildings.enabled,
        scenario.radio.range.margin_db,
        scenario.radio.range.max_m,
        report.actors_spawned,
        report.frames_transmitted,
        report.reception_attempts,
        report.faint_arrivals,
        report.pdr().unwrap_or(f64::NAN),
    );
    let mut bins = vec![(0u64, 0u64); 20];
    for r in &rx {
        let Some(d) = r.dist_m else { continue };
        let i = (d / 100.0) as usize;
        if i < bins.len() {
            bins[i].0 += 1;
            if r.outcome == RxOutcome::Ok {
                bins[i].1 += 1;
            }
        }
    }
    for (i, (n, ok)) in bins.iter().enumerate() {
        if *n > 0 {
            println!(
                "  {:>4}-{:<4} m  attempts {:>8}  pdr {:.3}",
                i * 100,
                (i + 1) * 100,
                n,
                *ok as f64 / *n as f64
            );
        }
    }
    Ok(())
}

/// The geometry-class split described in the module documentation.
fn geometry(scenario: &Scenario, pairs: usize) -> Result<(), Box<dyn std::error::Error>> {
    use v2xw_core::geom::Vec3;
    let world = v2xw_engine::wiring::build_world(scenario)?;
    let lanes: Vec<&v2xw_world::model::Lane> = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == v2xw_world::model::LaneKind::Driving && l.length_m > 5.0)
        .collect();
    let total: f64 = lanes.iter().map(|l| l.length_m).sum();
    // A fixed-seed generator: this is a measurement, and the same seed gives the same
    // sample on every machine.
    let mut state = 0x2545_F491_4F6C_DD1Du64 ^ scenario.seed;
    let mut uniform = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let point = |u: f64, v: f64| {
        // Length-weighted lane, uniform along it.
        let mut target = u * total;
        let mut lane = lanes[lanes.len() - 1];
        for l in &lanes {
            if target < l.length_m {
                lane = l;
                break;
            }
            target -= l.length_m;
        }
        let s_m = v * lane.length_m;
        let p = lane.offset_point(s_m, 0.0);
        let (sn, cs) = v2xw_core::math::sin_cos(lane.heading_at(s_m));
        (Vec3::new(p.x, p.y, p.z + 1.5), (cs, sn))
    };
    let mut buildings = v2xw_radio::BuildingShadowing::new(v2xw_core::card::Tier::Medium);
    let tracer = v2xw_radio::CornerTracer::build(&world);
    let env = v2xw_engine::wiring::world_env(&world);
    let high = v2xw_radio::GeometricUrbanV2v::new(v2xw_core::card::Tier::High, env);
    let medium_law = v2xw_radio::LogDistancePreset::for_environment(env, false).params();
    let f = 5.86e9;
    let sensitivity = v2xw_radio::Mcs::R6Qpsk12.sensitivity_static_dbm();
    let budget = 20.0 + 3.0;
    let p_clear = |mean_dbm: f64, sigma: f64| {
        let z = (mean_dbm - sensitivity) / sigma;
        0.5 * (1.0 + v2xw_radio::numeric::erf(z / core::f64::consts::SQRT_2))
    };
    // [bin][class] = (count, Σ p_medium, Σ p_high)
    let mut table = vec![[(0u64, 0.0f64, 0.0f64); 3]; 10];
    let mut done = 0usize;
    let mut tries = 0usize;
    while done < pairs && tries < pairs * 50 {
        tries += 1;
        let (a, a_dir) = point(uniform(), uniform());
        let (b, b_dir) = point(uniform(), uniform());
        let d = a.distance(b);
        if !(1.0..1_000.0).contains(&d) {
            continue;
        }
        done += 1;
        let mut los = buildings.los_cached(&world, a, b);
        let class = if !los.class.has_building() {
            0
        } else {
            // The street each end is on, as the engine traces it: the vehicle's heading.
            los.corner = tracer.trace_directed(&world, a, b, Some(a_dir), Some(b_dir));
            if los.corner.is_some() { 1 } else { 2 }
        };
        let tx = v2xw_radio::RadioEndpoint::isotropic(
            v2xw_core::ids::NodeId::new(0),
            a,
            v2xw_radio::ActorClass::Car,
            0,
        );
        let rx = v2xw_radio::RadioEndpoint::isotropic(
            v2xw_core::ids::NodeId::new(1),
            b,
            v2xw_radio::ActorClass::Car,
            0,
        );
        // Medium: Abbas LOS + min(Sommer, TR NLOS excess), σ of the LOS preset.
        let pl_los = medium_law.path_loss_db(d);
        let obstacle = buildings.loss_for_path(&los, d, f, pl_los);
        let p_med = p_clear(budget - pl_los - obstacle, medium_law.sigma_db);
        // High: the geometric law with its state's σ.
        let (pl_high, state) = high.mean_loss_db(&tx, &rx, f, &los);
        let p_high = p_clear(budget - pl_high, state.sigma_db());
        let bin = &mut table[(d / 100.0) as usize][class];
        bin.0 += 1;
        bin.1 += p_med;
        bin.2 += p_high;
    }
    println!(
        "{done} links sampled on {} driving lanes; P(clear −88 dBm) at 20 dBm EIRP + 3 dBi, \
         each law's own shadowing, no fading, no interference",
        lanes.len()
    );
    println!(
        "  bin (m)    | line of sight          | round one corner       | no single corner       | all links"
    );
    println!(
        "             | share  medium  high     | share  medium  high     | share  medium  high     | medium  high"
    );
    for (i, row) in table.iter().enumerate() {
        let n: u64 = row.iter().map(|c| c.0).sum();
        if n == 0 {
            continue;
        }
        let mut line = format!("  {:>4}-{:<4}  ", i * 100, (i + 1) * 100);
        let (mut sm, mut sh) = (0.0, 0.0);
        for c in row {
            sm += c.1;
            sh += c.2;
            if c.0 == 0 {
                line.push_str("|   -      -      -     ");
            } else {
                line.push_str(&format!(
                    "| {:.2}   {:.3}  {:.3}   ",
                    c.0 as f64 / n as f64,
                    c.1 / c.0 as f64,
                    c.2 / c.0 as f64
                ));
            }
        }
        line.push_str(&format!("| {:.3}  {:.3}", sm / n as f64, sh / n as f64));
        println!("{line}");
    }
    Ok(())
}
