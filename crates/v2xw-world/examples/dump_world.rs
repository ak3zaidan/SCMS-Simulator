//! Writes a generated world in both `vwp-world/1` forms, for cross-checking against the
//! TypeScript client in `ui/packages/protocol`.
//!
//! ```text
//! cargo run -p v2xw-world --example dump_world -- /tmp/out
//! node -e 'import("/path/to/ui/packages/protocol/dist/index.js").then(async (m) => {
//!   const fs = await import("node:fs");
//!   const f = fs.readFileSync("/tmp/out/world.vwb");
//!   const w = m.decodeWorld(f.buffer.slice(f.byteOffset, f.byteOffset + f.byteLength));
//!   console.log(w.contentHash, w.lanes.count, w.buildings.count);
//! })'
//! ```
//!
//! The payload this writes has been decoded by that client: the content hash it
//! recomputes matches, every section decodes, and `worldToJson` of the binary equals the
//! JSON form field for field (conformance item W4).

use std::path::PathBuf;

use v2xw_world::procedural::{GridParams, grid};
use v2xw_world::{ImportOptions, serde_native, serde_vwp};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".to_string()));
    let mut params = GridParams::tr36885_urban();
    params.rsu_at_junctions = true;
    // The date is the caller's to supply: no part of the engine reads a clock.
    let world = grid(
        &params,
        &ImportOptions::default().imported_at("2026-09-18T00:00:00Z"),
    )?;
    let payload = serde_vwp::write(&world)?;

    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("world.vwb"), &payload.bytes)?;
    std::fs::write(dir.join("world.json"), serde_vwp::to_json_string(&world)?)?;
    std::fs::write(dir.join("world.v2xw"), serde_native::to_bytes(&world)?)?;
    std::fs::write(
        dir.join("world.native.json"),
        serde_native::to_json_pretty(&world)?,
    )?;

    println!(
        "world content hash   {}",
        v2xw_world::hash::content_hash_hex(&world)
    );
    println!("payload content hash {}", payload.content_hash_hex());
    println!("payload path         {}", payload.url_path());
    println!("payload bytes        {}", payload.bytes.len());
    println!("counts               {:?}", world.counts());
    println!("buildings            {}", world.buildings.len());
    println!(
        "signal heads         {}",
        world.signals.iter().map(|p| p.heads.len()).sum::<usize>()
    );
    for warning in &payload.precision_warnings {
        println!("precision warning    {warning}");
    }
    Ok(())
}
