//! The argument grammar.
//!
//! `clap` with derive is already a workspace dependency (the Phase 0 asset survey resolved
//! it), so no second argument parser is introduced here.
//!
//! Two conventions are followed throughout, both taken from the scenario schema:
//! units live in flag names (`--keyframe-ms`, `--duration-s`), and a flag that overrides a
//! scenario field is spelled `--<field>` and says in its help that it changes the scenario
//! hash — because it does, and a sweep whose runs all report one hash would be worthless.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// The `v2xw` command line.
#[derive(Debug, Parser)]
#[command(
    name = "v2xw",
    version,
    about = "V2X World Simulator — run scenarios, import worlds, inspect recordings",
    long_about = None,
)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The four commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a scenario and write its recording, metrics and manifest.
    Run(RunArgs),
    /// Check a scenario without running it.
    Validate(ValidateArgs),
    /// Import an OpenStreetMap extract into the three world formats.
    ImportOsm(ImportArgs),
    /// Print a recording's manifest, channels and verification report.
    Info(InfoArgs),
}

/// `v2xw run`.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// The scenario file, YAML or JSON.
    pub scenario: PathBuf,

    /// Where the outputs go. Defaults to `runs/<meta.name>`.
    #[arg(short, long)]
    pub out: Option<PathBuf>,

    /// The manifest's build timestamp. Defaults to this machine's clock; pass it
    /// explicitly to compare two runs' manifests field by field.
    #[arg(long, value_name = "ISO8601")]
    pub build_utc: Option<String>,

    /// The recording's keyframe period. Must be a whole number of mobility steps.
    #[arg(long, value_name = "MS", default_value_t = 1000)]
    pub keyframe_ms: u64,

    /// Record the NODE-only profile, which refuses every ground-truth record.
    #[arg(long)]
    pub node_only: bool,

    /// Run without writing a recording — the engine into a counting sink. This is what a
    /// scaling measurement wants: it times the engine and not the container.
    #[arg(long)]
    pub no_recording: bool,

    /// Also attach the world payload to the recording. Off by default: a city world is
    /// tens of megabytes.
    #[arg(long)]
    pub attach_world: bool,

    /// Skip reading the recording back. The read costs a second pass and reports the
    /// content digest and the chunk checksums, so it is on by default.
    #[arg(long)]
    pub no_verify: bool,

    /// Override `time.duration_s`. Changes the scenario hash.
    #[arg(long, value_name = "S")]
    pub duration_s: Option<f64>,

    /// Override `actors.vehicles.demand.rate_veh_per_h`. Changes the scenario hash.
    #[arg(long, value_name = "VEH_PER_H")]
    pub rate_veh_per_h: Option<f64>,

    /// Print the outcome as JSON.
    #[arg(long)]
    pub json: bool,
}

impl RunArgs {
    /// The library options this asks for.
    pub fn to_options(&self) -> crate::run::RunOptions {
        crate::run::RunOptions {
            scenario: self.scenario.clone(),
            out: self.out.clone(),
            build_utc: self.build_utc.clone(),
            keyframe_ms: self.keyframe_ms,
            node_only: self.node_only,
            record: !self.no_recording,
            attach_world: self.attach_world,
            verify: !self.no_verify && !self.no_recording,
            duration_s: self.duration_s,
            rate_veh_per_h: self.rate_veh_per_h,
            json: self.json,
        }
    }
}

/// `v2xw validate`.
#[derive(Debug, clap::Args)]
pub struct ValidateArgs {
    /// The scenario file, YAML or JSON.
    pub scenario: PathBuf,
    /// Print the outcome as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `v2xw import-osm`.
#[derive(Debug, clap::Args)]
pub struct ImportArgs {
    /// The `.osm` or `.osm.xml` extract.
    pub extract: PathBuf,
    /// The directory to write `world.vwb`, `world.json`, `world.v2xw` and `report.txt` to.
    pub out: PathBuf,

    /// The `highway=*` class-default speed preset. There is no default: a fallback speed
    /// limit is a statement about a jurisdiction, and the importer refuses to guess one.
    #[arg(long, value_name = "NAME")]
    pub speed_preset: String,

    /// The geodetic box to keep, as `min_lon,min_lat,max_lon,max_lat` in degrees — the
    /// order the design documents write it in. It also fixes the world frame: the origin
    /// is the box's south-west corner, so the same box always yields the same metre
    /// coordinates. Left out, the frame comes from the extract's own bounds.
    ///
    /// `allow_hyphen_values` is on because every western-hemisphere box starts with a
    /// minus sign, and without it `--bbox -73.99,40.74,...` is rejected as an unknown
    /// flag `-7` — an error about argument syntax in answer to a correct bounding box.
    #[arg(
        long,
        value_name = "MIN_LON,MIN_LAT,MAX_LON,MAX_LAT",
        allow_hyphen_values = true
    )]
    pub bbox: Option<String>,

    /// The import date written to the world's provenance. Defaults to this machine's
    /// clock; it is excluded from the world's content hash either way.
    #[arg(long, value_name = "ISO8601")]
    pub imported_at: Option<String>,

    /// Print the outcome as JSON instead of the importer's report.
    #[arg(long)]
    pub json: bool,
}

/// `v2xw info`.
#[derive(Debug, clap::Args)]
pub struct InfoArgs {
    /// The recording.
    pub recording: PathBuf,
    /// Print the outcome as JSON.
    #[arg(long)]
    pub json: bool,
}
