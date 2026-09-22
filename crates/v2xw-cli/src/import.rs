//! `v2xw import-osm` — import an OpenStreetMap extract into the three world formats.
//!
//! # The speed preset has no default, and this command does not invent one
//!
//! [`v2xw_world::osm::OsmOptions::highway_preset`] is required: the fallback speed of a
//! road class is a jurisdictional fact, and the importer refuses rather than shipping one
//! jurisdiction's table to every caller. `--speed-preset` is therefore a real choice the
//! operator makes, and the preset's own citation is printed next to it so that the choice
//! is on the record before the world is written.
//!
//! # The bounding box also fixes the world frame
//!
//! `--bbox` is not only a filter. [`v2xw_world::osm::OsmOptions::bbox`] makes the box's
//! south-west corner the world's origin, so a box stated here and the same box stated in a
//! scenario produce the same metre coordinates, and re-fetching the extract at a wider
//! radius cannot move a single one. Left out, the frame comes from the extract's own
//! bounds, which is what the importer's report calls `origin from the extract-bounds`.
//!
//! The four numbers are in the order the design documents write them —
//! `min_lon,min_lat,max_lon,max_lat` — because that is the order D7 states the Phase 1 box
//! in.
//!
//! **A range check cannot catch a transposed Manhattan box.** `40.744,-73.99,...` is a
//! perfectly legal longitude followed by a perfectly legal latitude: −73.99° is a real
//! place in the Southern Ocean. Measured, that box imports without complaint and produces
//! a world of `extent 0 x 0 m`, 0 drivable lanes and 488 `clipped-out` anomalies — the
//! importer returns `Ok`, because an empty world is not a malformed one. The order check
//! below therefore rejects only what is out of range, and the real guard is
//! [`refuse_an_empty_network`], which looks at what came out: an import with no drivable
//! lane is refused, naming the box, the extract's own bounds and the argument order.
//!
//! # The import date is an argument
//!
//! `--imported-at` fills [`v2xw_world::ImportOptions::imported_at`]. Left out, it comes
//! from [`crate::wall::now_iso8601_utc`] — the caller's clock, which is the only place in
//! this workspace that one may be read. It is excluded from the world's content hash, so
//! two imports of one extract on two days produce the same `world_hash`.

use std::path::{Path, PathBuf};

use v2xw_world::osm::{HighwayPreset, OsmOptions, import_osm};
use v2xw_world::{serde_native, serde_vwp};

use crate::error::{CliError, Result};
use crate::wall::{Stopwatch, now_iso8601_utc};

/// What `v2xw import-osm` was asked to do.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// The `.osm` or `.osm.xml` extract.
    pub extract: PathBuf,
    /// The directory the world is written to.
    pub out: PathBuf,
    /// The import date. `None` reads the clock.
    pub imported_at: Option<String>,
    /// The `highway=*` class-default preset, by name.
    pub speed_preset: String,
    /// The geodetic box to keep, and the frame to place the world in. `None` uses the
    /// extract's own bounds.
    pub bbox: Option<String>,
}

/// What an import produced.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportOutcome {
    /// Where the world went.
    pub out_dir: String,
    /// The preset that was applied.
    pub speed_preset: String,
    /// Where that preset's numbers come from.
    pub speed_preset_source: String,
    /// The box the world was placed in, as `min_lon,min_lat,max_lon,max_lat`, or `None`
    /// when the extract's own bounds were used.
    pub bbox: Option<String>,
    /// The world's content hash.
    pub world_hash: String,
    /// The `vwp-world/1` payload's size in bytes.
    pub payload_bytes: u64,
    /// The engine-native world's size in bytes.
    pub native_bytes: u64,
    /// How many anomalies the importer counted in total.
    pub anomalies: u64,
    /// Precision warnings the payload writer raised.
    pub precision_warnings: Vec<String>,
    /// Wall-clock seconds the import took. A measurement, never an input.
    pub elapsed_s: f64,
}

/// Imports an extract and writes `world.vwb`, `world.json`, `world.v2xw` and `report.txt`.
///
/// # Errors
/// [`CliError::BadArgument`] for an unknown preset name, [`CliError::World`] if the
/// importer refuses, [`CliError::Io`] if an output cannot be written.
pub fn import_osm_extract(opts: &ImportOptions) -> Result<(ImportOutcome, String)> {
    let preset = HighwayPreset::parse(&opts.speed_preset).ok_or_else(|| CliError::BadArgument {
        flag: "speed-preset",
        problem: format!(
            "'{}' is not a preset this build knows; the names are 'urban-us-nyc' (the legal \
             defaults of a US city with a citywide limit) and 'sumo-german' (netconvert's \
             table, for comparability with SUMO)",
            opts.speed_preset
        ),
    })?;
    let imported_at = opts
        .imported_at
        .clone()
        .unwrap_or_else(|| now_iso8601_utc().to_string());

    let bbox = opts.bbox.as_deref().map(parse_bbox).transpose()?;
    let mut options = OsmOptions::default()
        .imported_at(imported_at)
        .highway_preset(preset);
    if let Some(b) = bbox {
        options = options.bbox(b);
    }

    let sw = Stopwatch::start();
    let (world, report) = import_osm(&opts.extract, &options)?;
    let elapsed_s = sw.elapsed_s();

    refuse_an_empty_network(&report, opts)?;

    let payload = serde_vwp::write(&world)?;
    let native = serde_native::to_bytes(&world)?;
    let json = serde_vwp::to_json_string(&world)?;
    let text = report.to_text();

    std::fs::create_dir_all(&opts.out)
        .map_err(|e| CliError::io("cannot create output directory", &opts.out, e))?;
    write(&opts.out, "world.vwb", &payload.bytes)?;
    write(&opts.out, "world.json", json.as_bytes())?;
    write(&opts.out, "world.v2xw", &native)?;
    write(&opts.out, "report.txt", text.as_bytes())?;

    Ok((
        ImportOutcome {
            out_dir: opts.out.display().to_string(),
            speed_preset: preset.label().to_string(),
            speed_preset_source: preset.source().to_string(),
            bbox: bbox.map(|b| {
                format!(
                    "{},{},{},{}",
                    b.min_lon_deg, b.min_lat_deg, b.max_lon_deg, b.max_lat_deg
                )
            }),
            world_hash: v2xw_world::hash::content_hash_hex(&world),
            payload_bytes: payload.bytes.len() as u64,
            native_bytes: native.len() as u64,
            anomalies: report.total_anomalies(),
            precision_warnings: payload.precision_warnings.clone(),
            elapsed_s,
        },
        text,
    ))
}

/// Refuses an import that produced no drivable lane.
///
/// This is the check that catches a transposed box, a box over the wrong city and a sign
/// typo, none of which a range check can see. It is deliberately about the *output*: the
/// only reliable statement about a bounding box is what it kept.
///
/// A world with no road is never what an operator wanted — nothing can drive in it, and
/// every scenario over it would report zero vehicles and zero messages, which is exactly
/// the shape of a run that looks like it worked.
///
/// # Errors
/// [`CliError::BadArgument`] naming `bbox` when one was given, since the box is then what
/// the operator must change; otherwise it names the extract.
fn refuse_an_empty_network(
    report: &v2xw_world::osm::ImportReport,
    opts: &ImportOptions,
) -> Result<()> {
    if report.counts.drivable_lanes > 0 {
        return Ok(());
    }
    let bounds = report
        .bbox
        .map(|b| {
            format!(
                "{},{},{},{}",
                b.min_lon_deg, b.min_lat_deg, b.max_lon_deg, b.max_lat_deg
            )
        })
        .unwrap_or_else(|| "unknown".to_string());
    let extent = report
        .extent_m
        .map(|(e, n)| format!("{e:.0} x {n:.0} m"))
        .unwrap_or_else(|| "empty".to_string());
    match &opts.bbox {
        Some(asked) => Err(CliError::BadArgument {
            flag: "bbox",
            problem: format!(
                "'{asked}' kept no drivable lane from {}: the world came out {extent} with                  {} way(s) classified as drivable. The order is                  min_lon,min_lat,max_lon,max_lat, and a transposed box is legal arithmetic                  — the extract itself covers {bounds}",
                opts.extract.display(),
                report.counts.drivable_ways
            ),
        }),
        None => Err(CliError::BadArgument {
            flag: "bbox",
            problem: format!(
                "{} holds no drivable way: {} OSM ways were read and none classified as                  drivable, so the world came out {extent}. Nothing can drive in it",
                opts.extract.display(),
                report.counts.osm_ways
            ),
        }),
    }
}

/// `min_lon,min_lat,max_lon,max_lat`, in degrees.
///
/// The order is the one the design documents use; see the module note. Both degree ranges
/// are checked, because a transposed pair is the mistake this format invites and a
/// latitude of −73.99 is not a latitude.
fn parse_bbox(text: &str) -> Result<v2xw_world::GeoBbox> {
    let bad = |problem: String| CliError::BadArgument {
        flag: "bbox",
        problem,
    };
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(bad(format!(
            "'{text}' has {} comma-separated values; it needs four, as              min_lon,min_lat,max_lon,max_lat",
            parts.len()
        )));
    }
    let mut v = [0.0f64; 4];
    for (i, p) in parts.iter().enumerate() {
        v[i] = p
            .parse()
            .map_err(|e| bad(format!("'{p}' is not a number: {e}")))?;
    }
    let [min_lon, min_lat, max_lon, max_lat] = v;
    for (name, deg) in [("min_lon", min_lon), ("max_lon", max_lon)] {
        if !(-180.0..=180.0).contains(&deg) {
            return Err(bad(format!(
                "{name} {deg} is not a longitude; the order is                  min_lon,min_lat,max_lon,max_lat"
            )));
        }
    }
    for (name, deg) in [("min_lat", min_lat), ("max_lat", max_lat)] {
        if !(-90.0..=90.0).contains(&deg) {
            return Err(bad(format!(
                "{name} {deg} is not a latitude; the order is                  min_lon,min_lat,max_lon,max_lat"
            )));
        }
    }
    Ok(v2xw_world::GeoBbox::new(min_lat, min_lon, max_lat, max_lon))
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(|e| CliError::io("cannot write", &path, e))
}
