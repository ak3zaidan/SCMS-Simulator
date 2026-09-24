//! `rsu_sites` — lists the signalised intersections of a scenario's world along named
//! streets, as `actors.rsus[].position_m` lines.
//!
//! ```text
//! cargo run -p v2xw-engine --example rsu_sites -- <scenario.yaml> "5th Avenue" "6th Avenue"
//! ```
//!
//! An OpenStreetMap extract carries no mast inventory, so a scenario on an imported city
//! states where each roadside unit stands. A deployment puts them at signalised
//! intersections along its equipped corridors — the NYC connected-vehicle pilot's
//! Manhattan units were on avenue corridors — and this prints exactly those positions
//! (each junction's own reference point, world-local ENU metres) so a scenario's list is
//! read off the imported network rather than typed from a map.

use v2xw_engine::Scenario;
use v2xw_world::model::JunctionControl;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .ok_or("usage: rsu_sites <scenario.yaml> <street name>...")?;
    let streets: Vec<String> = args.iter().skip(1).map(|s| s.to_lowercase()).collect();
    let scenario = Scenario::load(path)?;
    let world = v2xw_engine::wiring::build_world(&scenario)?;
    let roads = &world.roads;
    let mut n = 0;
    for j in roads.junctions() {
        if !matches!(j.control, JunctionControl::Signalised { .. }) {
            continue;
        }
        let mut names: Vec<String> = j
            .incoming
            .iter()
            .chain(j.outgoing.iter())
            .filter_map(|l| roads.edge(roads.lane(*l).edge).name)
            .map(|s| world.symbols.resolve(s).to_string())
            .collect();
        names.sort();
        names.dedup();
        let hit = streets.is_empty()
            || names
                .iter()
                .any(|name| streets.iter().any(|s| name.to_lowercase() == *s));
        if !hit {
            continue;
        }
        n += 1;
        println!(
            "    - position_m: [{:.1}, {:.1}, {:.1}]   # {}",
            j.position.x,
            j.position.y,
            j.position.z,
            names.join(" / ")
        );
    }
    eprintln!("{n} signalised junction(s)");
    Ok(())
}
