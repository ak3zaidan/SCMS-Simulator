//! Imports an OpenStreetMap extract and writes the world in every form the rest of the
//! system reads, plus the import report.
//!
//! ```text
//! cargo run -p v2xw-world --release --example import_osm -- \
//!     worlds/cache/manhattan.osm.xml /tmp/manhattan
//! ```
//!
//! It writes `world.vwb` and `world.json` (the `vwp-world/1` payload the UI consumes, in
//! both its binary and its JSON form), `world.v2xw` (the engine's own format), and
//! `report.txt`. The import date is a command-line argument rather than the clock, because
//! no part of the engine may read one (02-architecture.md §6.1).

use std::path::PathBuf;

use v2xw_world::osm::{HighwayPreset, OsmOptions, import_osm};
use v2xw_world::{serde_native, serde_vwp};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args
        .next()
        .ok_or("usage: import_osm <extract.osm.xml> [out-dir] [imported-at] [speed-preset]")?;
    let out = PathBuf::from(args.next().unwrap_or_else(|| ".".to_string()));
    let imported_at = args
        .next()
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    // V4/W1: the class-default speed preset is required — there is no default, because
    // a default speed limit is a statement about a jurisdiction. This is the Phase 1
    // Manhattan path, so it selects the New York City one; pass a name to override it.
    let preset_name = args.next().unwrap_or_else(|| "urban-us-nyc".to_string());
    let preset = HighwayPreset::parse(&preset_name)
        .ok_or_else(|| format!("unknown speed preset {preset_name}"))?;

    let options = OsmOptions::default()
        .imported_at(imported_at)
        .highway_preset(preset);
    println!("speed preset      {} ({})", preset.label(), preset.source());
    let started = std::time::Instant::now();
    let (world, report) = import_osm(&source, &options)?;
    let elapsed = started.elapsed();

    let payload = serde_vwp::write(&world)?;
    std::fs::create_dir_all(&out)?;
    std::fs::write(out.join("world.vwb"), &payload.bytes)?;
    std::fs::write(out.join("world.json"), serde_vwp::to_json_string(&world)?)?;
    std::fs::write(out.join("world.v2xw"), serde_native::to_bytes(&world)?)?;
    std::fs::write(out.join("report.txt"), report.to_text())?;

    print!("{}", report.to_text());
    println!("import took {} ms", elapsed.as_millis());
    println!("world hash {}", v2xw_world::hash::content_hash_hex(&world));
    println!(
        "payload {} bytes at {}, {} precision warning(s)",
        payload.bytes.len(),
        payload.url_path(),
        payload.precision_warnings.len()
    );
    for warning in &payload.precision_warnings {
        println!("  {warning}");
    }
    Ok(())
}
