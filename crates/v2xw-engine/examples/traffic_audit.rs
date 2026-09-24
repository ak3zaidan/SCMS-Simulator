//! `traffic_audit` — runs a scenario's traffic headless and checks every vehicle at every
//! mobility step against the traffic invariants of `v2xw_mobility::audit`.
//!
//! ```text
//! cargo run -p v2xw-engine --example traffic_audit -- <scenario.yaml> \
//!     [--rate VEH_PER_H] [--duration S] [--json OUT] [--pedestrians N] [--cyclists N]
//! ```
//!
//! The world, the mobility engine and the demand model are built by the same
//! `v2xw_engine::wiring` functions the kernel uses, and the mobility engine is stepped
//! with the scenario's own seed and step, so the traffic audited is the traffic the page
//! shows. Radio, security and nodes are not run: they do not feed back into motion.
//!
//! It is an example rather than a test because it is a measurement: it prints the count of
//! every violation class with examples (vehicle, time, place). The regression gate that
//! holds the counts at zero is `v2xw-mobility`'s `tests/traffic_invariants.rs`.

use std::time::Instant;

use v2xw_core::rng::RngRegistry;
use v2xw_engine::Scenario;
use v2xw_mobility::audit::{AuditParams, TrafficAuditor};
use v2xw_mobility::{Mobility, MobilityCtx};

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
        .ok_or("usage: traffic_audit <scenario.yaml> [--rate R] [--duration S] [--json OUT]")?;
    let mut scenario = Scenario::load(path)?;
    // `--pedestrians N` and `--cyclists N` override `actors.vru`, so a scenario without one can
    // be audited with vulnerable road users on it.
    if let Some(n) = value("--pedestrians") {
        scenario.actors.vru.pedestrians = n.parse()?;
    }
    if let Some(n) = value("--cyclists") {
        scenario.actors.vru.cyclists = n.parse()?;
    }
    if let Some(rate) = value("--rate") {
        scenario.actors.vehicles.demand.rate_veh_per_h = Some(rate.parse()?);
        if scenario.actors.vehicles.demand.kind == "mobility/demand/none" {
            scenario.actors.vehicles.demand.kind = "mobility/demand/poisson".to_string();
        }
    }
    if let Some(d) = value("--duration") {
        scenario.time.duration_s = d.parse()?;
    }
    let built = Instant::now();
    let world = v2xw_engine::wiring::build_world(&scenario)?;
    let rng = RngRegistry::new(scenario.seed);
    let mut mobility = v2xw_engine::wiring::native_mobility(&scenario);
    {
        let demand = v2xw_engine::wiring::build_demand(&scenario, &world)?;
        let mut ctx = MobilityCtx::new(0, &world, &rng);
        mobility.init(&mut ctx, demand)?;
    }
    eprintln!(
        "world: {} lanes, {} junctions, {} signal plans, {} buildings ({:.1} s to build)",
        world.roads.lanes().len(),
        world.roads.junctions().len(),
        world.signals.len(),
        world.buildings.len(),
        built.elapsed().as_secs_f64()
    );
    // `--describe-building N[,M...]`: print a building's footprint summary, then stop.
    if let Some(list) = value("--describe-building") {
        for id in list.split(',') {
            let id: u32 = id.trim().parse()?;
            let Some(b) = world.building(v2xw_core::ids::BuildingId::new(id)) else {
                continue;
            };
            let bbox = b.bbox();
            println!(
                "building {id}: name {:?} height {:.1} min_height {:.1} base_z {:.1} \
                 holes {} points {} bbox ({:.1},{:.1})-({:.1},{:.1})",
                b.name.map(|n| world.symbols.resolve(n).to_string()),
                b.height_m,
                b.min_height_m,
                b.base_z_m,
                b.holes.len(),
                b.footprint.len(),
                bbox.min.x,
                bbox.min.y,
                bbox.max.x,
                bbox.max.y
            );
        }
        return Ok(());
    }
    // `--describe-lane N[,M...]`: print a lane and the movements through it, then stop.
    if let Some(list) = value("--describe-lane") {
        for id in list.split(',') {
            let id: u32 = id.trim().parse()?;
            let lane = world.lane(v2xw_core::ids::LaneId::new(id));
            println!(
                "lane {id}: kind {:?} edge {} junction {:?} index {} len {:.2} width {:.2} \
                 limit {:.2}",
                lane.kind,
                lane.edge.index(),
                lane.junction.map(|j| j.index()),
                lane.index,
                lane.length_m,
                lane.width_m,
                lane.speed_limit_mps
            );
            for p in &lane.centreline {
                println!("    ({:.2}, {:.2})", p.x, p.y);
            }
            for c in world.roads.connections() {
                if c.from_lane.index() == id
                    || c.to_lane.index() == id
                    || c.via.map(|v| v.index()) == Some(id)
                {
                    println!(
                        "  conn {} -> {} via {:?} {:?} permitted {}",
                        c.from_lane.index(),
                        c.to_lane.index(),
                        c.via.map(|v| v.index()),
                        c.direction,
                        c.permitted
                    );
                }
            }
        }
        return Ok(());
    }
    {
        let mut kinds: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        let mut bike = 0usize;
        for l in world.roads.lanes() {
            *kinds.entry(l.kind.wire_name()).or_default() += 1;
            if l.admits(v2xw_world::ClassMask::BICYCLE) {
                bike += 1;
            }
        }
        eprintln!("lane kinds: {kinds:?}; lanes admitting bicycles: {bike}");
    }
    let mut auditor = TrafficAuditor::new(&world, AuditParams::default());
    auditor.audit_world(&world);
    let trace: Option<Vec<u32>> =
        value("--trace").map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect());
    let window: (f64, f64) = value("--window")
        .and_then(|v| {
            let mut it = v.split(',').filter_map(|x| x.trim().parse::<f64>().ok());
            Some((it.next()?, it.next()?))
        })
        .unwrap_or((0.0, f64::INFINITY));
    // `--near X,Y,R`: every actor within R metres of (X, Y) in the window.
    let near: Option<(f64, f64, f64)> = value("--near").and_then(|v| {
        let mut it = v.split(',').filter_map(|x| x.trim().parse::<f64>().ok());
        Some((it.next()?, it.next()?, it.next()?))
    });
    let step = scenario.time.mobility_step();
    let horizon = (scenario.time.duration_s * 1e9) as u64;
    let started = Instant::now();
    let mut t = 0u64;
    while t < horizon {
        let mut ctx = MobilityCtx::new(t, &world, &rng);
        let update = mobility.step(&mut ctx, step);
        let t1 = update.t;
        let actors = mobility.audit_actors(&world, t1);
        // `--trace A,B --window T0,T1`: every state of the named actors in the window.
        if trace.is_some() || near.is_some() {
            let ts = t1 as f64 * 1e-9;
            if ts >= window.0 && ts <= window.1 {
                let wanted = |a: &v2xw_mobility::audit::AuditActor| {
                    trace.as_ref().is_some_and(|l| l.contains(&a.actor.index()))
                        || near.is_some_and(|(x, y, r)| (a.pos.x - x).hypot(a.pos.y - y) <= r)
                };
                for a in actors.iter().filter(|a| wanted(a)) {
                    println!(
                        "t={ts:.1} actor={} lane={} prev={:?} s={:.2} lat={:.2} v={:.2} \
                         a={:.2} len={:.1} next={:?} chg={:?} pos=({:.2},{:.2}) h={:.1}°",
                        a.actor.index(),
                        a.lane.index(),
                        a.prev_lane.map(|l| l.index()),
                        a.s_m,
                        a.lateral_m,
                        a.speed_mps,
                        a.accel_mps2,
                        a.length_m,
                        a.route_next.map(|l| l.index()),
                        a.changing.map(|(f, t)| (f.index(), t.index())),
                        a.pos.x,
                        a.pos.y,
                        a.heading_rad.to_degrees()
                    );
                }
            }
        }
        let people = mobility.audit_pedestrians(&world);
        auditor.observe_with_pedestrians(&world, t, t1, &actors, &update.despawned, &people);
        t = t1;
    }
    let report = auditor.report();
    eprintln!(
        "simulated {:.0} s in {:.1} s wall",
        scenario.time.duration_s,
        started.elapsed().as_secs_f64()
    );
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(out) = value("--json") {
        std::fs::write(&out, &json)?;
    }
    println!("{}", serde_json::to_string_pretty(&report.stats)?);
    for (k, v) in &report.counts {
        println!("{k:<34} {v}");
    }
    for e in &report.examples {
        println!(
            "  [{}] t={:.1}s actor={:?} other={:?} ({:.1}, {:.1}) lane={:?}: {}",
            e.check.label(),
            e.t_s,
            e.actor,
            e.other,
            e.x_m,
            e.y_m,
            e.lane,
            e.detail
        );
    }
    Ok(())
}
