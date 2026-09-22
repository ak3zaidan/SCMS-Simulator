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

/// The five commands.
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
    /// Expand a scenario's `experiment` block into runs, execute them and aggregate them.
    Experiment(ExperimentArgs),
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

/// `v2xw experiment`.
#[derive(Debug, clap::Args)]
pub struct ExperimentArgs {
    /// Which part of the experiment system to use.
    #[command(subcommand)]
    pub command: ExperimentCommand,
}

/// The three experiment subcommands.
#[derive(Debug, Subcommand)]
pub enum ExperimentCommand {
    /// Expand the sweep, run every outstanding cell, and write the results table.
    ///
    /// Already-finished runs are skipped whatever this is called with: the journal is the
    /// authority, not the subcommand.
    Run(ExperimentRunArgs),
    /// Say how far along a sweep is, and where it stopped. Runs nothing.
    Status(ExperimentStatusArgs),
    /// Continue a sweep that was already started here, and refuse if it was not.
    Resume(ExperimentRunArgs),
}

/// `v2xw experiment run` and `v2xw experiment resume`.
#[derive(Debug, clap::Args)]
pub struct ExperimentRunArgs {
    /// The scenario file carrying the `experiment` block.
    pub scenario: PathBuf,

    /// Where the sweep's outputs go. Defaults to `runs/<meta.name>-sweep`.
    #[arg(short, long)]
    pub out: Option<PathBuf>,

    /// The manifest build timestamp every run in the sweep uses. Defaults to this
    /// machine's clock, read once, so the runs differ in their seed and in nothing else.
    #[arg(long, value_name = "ISO8601")]
    pub build_utc: Option<String>,

    /// How many simulations to start at once.
    ///
    /// **The default is 1, and on a small machine it should stay there.** Each concurrent
    /// run holds its own world, scheduler, node state and recording buffer, so `k` runs
    /// cost `k` times the peak memory of one — and the engine already uses the machine's
    /// cores inside a single run, so a second concurrent run mostly competes with the
    /// first. Raising this on an 8 GB machine is how a sweep is killed at run 340 of 600.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub concurrency: usize,

    /// The confidence level for the aggregate, as a percentage.
    ///
    /// Spelled as a string default rather than a typed one so the flag's default reads as
    /// the percentage a paper reports.
    #[arg(long, value_enum, default_value = "95")]
    pub ci: CiLevel,

    /// The recording keyframe period each run uses.
    #[arg(long, value_name = "MS", default_value_t = 1000)]
    pub keyframe_ms: u64,

    /// Run without writing a recording per cell. A long sweep writes a lot of MCAP; the
    /// metrics, the run report and the manifest are written either way, and those are what
    /// the results table is built from.
    #[arg(long)]
    pub no_recording: bool,

    /// Record the NODE-only profile in every run.
    #[arg(long)]
    pub node_only: bool,

    /// Skip reading each recording back. On by default for a single run; a sweep of
    /// hundreds pays for it hundreds of times.
    #[arg(long)]
    pub no_verify: bool,

    /// Also write the results table in this format, beside `results.json`.
    #[arg(long, value_enum)]
    pub format: Option<TableFormat>,

    /// Print the outcome as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `v2xw experiment status`.
#[derive(Debug, clap::Args)]
pub struct ExperimentStatusArgs {
    /// The scenario file carrying the `experiment` block.
    pub scenario: PathBuf,
    /// Where the sweep's outputs are. Defaults to `runs/<meta.name>-sweep`.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Print the outcome as JSON.
    #[arg(long)]
    pub json: bool,
}

/// The confidence level, spelled as the percentage a paper reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CiLevel {
    /// 90 %.
    #[value(name = "90")]
    P90,
    /// 95 %, the level 08-measurement-and-data.md §4 declares.
    #[value(name = "95")]
    P95,
    /// 99 %.
    #[value(name = "99")]
    P99,
}

impl CiLevel {
    /// The metrics crate's level this names.
    #[must_use]
    pub fn level(self) -> v2xw_metrics::ConfidenceLevel {
        match self {
            CiLevel::P90 => v2xw_metrics::ConfidenceLevel::P90,
            CiLevel::P95 => v2xw_metrics::ConfidenceLevel::P95,
            CiLevel::P99 => v2xw_metrics::ConfidenceLevel::P99,
        }
    }
}

/// Which tabular format the results table is also written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TableFormat {
    /// Apache Parquet — "the primary format" (08-measurement-and-data.md §5).
    Parquet,
    /// Arrow IPC.
    Arrow,
    /// JSON Lines.
    Jsonl,
}

impl TableFormat {
    /// The exporter format this names.
    #[must_use]
    pub fn format(self) -> v2xw_record::export::ExportFormat {
        match self {
            TableFormat::Parquet => v2xw_record::export::ExportFormat::Parquet,
            TableFormat::Arrow => v2xw_record::export::ExportFormat::ArrowIpc,
            TableFormat::Jsonl => v2xw_record::export::ExportFormat::Jsonl,
        }
    }
}

impl ExperimentRunArgs {
    /// The library options this asks for. `resume` is the one thing the two subcommands
    /// differ in.
    #[must_use]
    pub fn to_options(&self, resume: bool) -> crate::experiment::ExperimentOptions {
        crate::experiment::ExperimentOptions {
            scenario: self.scenario.clone(),
            out: self.out.clone(),
            build_utc: self.build_utc.clone(),
            concurrency: self.concurrency,
            level: self.ci.level(),
            keyframe_ms: self.keyframe_ms,
            record: !self.no_recording,
            node_only: self.node_only,
            verify: !self.no_verify && !self.no_recording,
            format: self.format.map(TableFormat::format),
            resume,
        }
    }
}
