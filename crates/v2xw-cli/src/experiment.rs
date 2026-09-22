//! `v2xw experiment` — run a sweep, see where it is, resume it.
//!
//! The three commands are one function with two flags between them: `resume` is `run` with
//! [`v2xw_experiment::RunnerOptions::require_journal`] set, and `status` is the same plan
//! expansion with nothing executed.
//!
//! # Where the clock is
//!
//! [`v2xw_experiment`] reads no clock at all. This module supplies the manifest timestamp
//! each run needs, once, from [`crate::wall::now_iso8601_utc`], and hands the *same* string
//! to every run in the sweep — so two runs of one cell differ in their seed and in nothing
//! else, and a manifest diff across a sweep shows the sweep rather than the minute each run
//! happened to start in. `--build-utc` pins it explicitly.
//!
//! # Concurrency
//!
//! The default is one simulation at a time, and on a small machine it should stay there.
//! See [`v2xw_experiment::runner`] for the arithmetic; the short version is that `k`
//! concurrent runs cost `k` times the peak memory of one and share the cores the engine's
//! own parallel phase is already using.

use std::path::{Path, PathBuf};

use v2xw_experiment::plan::PlannedRun;
use v2xw_experiment::runner::{
    RunArtifacts, RunExecutor, RunnerOptions, run_experiment, status as plan_status,
};
use v2xw_experiment::{ExperimentOutcome, ExperimentStatus, ResultsTable};
use v2xw_metrics::ConfidenceLevel;
use v2xw_record::export::ExportFormat;

use crate::error::{CliError, Result};
use crate::wall::now_iso8601_utc;

/// How `v2xw experiment run` and `v2xw experiment resume` were asked to run.
#[derive(Debug, Clone)]
pub struct ExperimentOptions {
    /// The scenario carrying the `experiment` block.
    pub scenario: PathBuf,
    /// Where the sweep's outputs go. `None` means `runs/<meta.name>-sweep`.
    pub out: Option<PathBuf>,
    /// The manifest timestamp every run in the sweep is built with. `None` reads the clock
    /// once, here.
    pub build_utc: Option<String>,
    /// How many simulations to start at once. One unless you have measured otherwise.
    pub concurrency: usize,
    /// The confidence level for the aggregate.
    pub level: ConfidenceLevel,
    /// The recording keyframe period each run uses.
    pub keyframe_ms: u64,
    /// Write a recording per run. Off makes a large sweep far cheaper on disk.
    pub record: bool,
    /// Record the NODE-only profile.
    pub node_only: bool,
    /// Read each run's recording back and verify it.
    pub verify: bool,
    /// Also write the results table in this tabular format, beside `results.json`.
    pub format: Option<ExportFormat>,
    /// Refuse to start unless the sweep was already started here.
    pub resume: bool,
}

impl ExperimentOptions {
    /// The output directory this asks for, given the scenario's name.
    #[must_use]
    pub fn out_dir(&self, scenario_name: &str) -> PathBuf {
        self.out
            .clone()
            .unwrap_or_else(|| PathBuf::from("runs").join(format!("{scenario_name}-sweep")))
    }
}

/// What `v2xw experiment run` produced.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExperimentRunOutcome {
    /// What the runner reported.
    pub outcome: ExperimentOutcome,
    /// The tabular file names the export wrote, if one was asked for.
    pub exported: Vec<String>,
    /// A digest of the results table, so two sweeps can be compared without diffing rows.
    pub results_digest: String,
    /// How many rows carry a usable estimate.
    pub rows_with_estimate: usize,
}

/// Runs — or resumes — a sweep.
///
/// # Errors
/// [`CliError::Experiment`] for anything the runner refuses, including a plan that changed
/// under an existing journal and a run that failed.
pub fn run(options: &ExperimentOptions) -> Result<ExperimentRunOutcome> {
    let scenario = v2xw_engine::Scenario::load(&options.scenario)?;
    let out = options.out_dir(&scenario.meta.name);
    let build_utc = options
        .build_utc
        .clone()
        .unwrap_or_else(|| now_iso8601_utc().to_string());

    let executor = CliExecutor {
        build_utc,
        keyframe_ms: options.keyframe_ms,
        record: options.record,
        node_only: options.node_only,
        verify: options.verify && options.record,
    };
    let mut runner = RunnerOptions::new(&out);
    runner.concurrency = options.concurrency;
    runner.level = options.level;
    runner.require_journal = options.resume;

    let outcome = run_experiment(&scenario, &executor, &runner)?;
    let table = v2xw_experiment::runner::rebuild_table(&scenario, &runner)?;

    let mut exported = Vec::new();
    if let Some(format) = options.format {
        for file in table.write_table(&out, format)? {
            exported.push(file.path.display().to_string());
        }
    }

    Ok(ExperimentRunOutcome {
        rows_with_estimate: table.rows.iter().filter(|r| r.mean.is_some()).count(),
        results_digest: results_digest(&table)?,
        outcome,
        exported,
    })
}

/// Reports where a sweep stands without running anything.
///
/// # Errors
/// [`CliError::Experiment`] if the scenario has no sweep or the journal will not parse.
pub fn status(scenario_path: &Path, out: Option<&Path>) -> Result<ExperimentStatus> {
    let scenario = v2xw_engine::Scenario::load(scenario_path)?;
    let dir = match out {
        Some(dir) => dir.to_path_buf(),
        None => PathBuf::from("runs").join(format!("{}-sweep", scenario.meta.name)),
    };
    Ok(plan_status(&scenario, &dir)?)
}

/// SHA-256 over the canonical JSON of the results table.
fn results_digest(table: &ResultsTable) -> Result<String> {
    let bytes = v2xw_core::hash::canonical_json(table)?;
    Ok(v2xw_core::hash::sha256_hex(&bytes))
}

/// The executor: one run of `v2xw run`, into the directory the runner prepared.
///
/// This is the only place the experiment runner touches the engine, and it is in the one
/// crate this project allows a clock read in.
#[derive(Debug, Clone)]
struct CliExecutor {
    build_utc: String,
    keyframe_ms: u64,
    record: bool,
    node_only: bool,
    verify: bool,
}

impl RunExecutor for CliExecutor {
    fn execute(
        &self,
        _run: &PlannedRun,
        scenario_path: &Path,
        out_dir: &Path,
    ) -> core::result::Result<RunArtifacts, String> {
        let options = crate::run::RunOptions {
            scenario: scenario_path.to_path_buf(),
            out: Some(out_dir.to_path_buf()),
            build_utc: Some(self.build_utc.clone()),
            keyframe_ms: self.keyframe_ms,
            node_only: self.node_only,
            record: self.record,
            // A city world attached to every run of a 600-run sweep is tens of gigabytes.
            attach_world: false,
            verify: self.verify,
            // The sweep sets its parameters in the scenario document, not on the command
            // line, so that every run's hash is the hash of the document it executed.
            duration_s: None,
            rate_veh_per_h: None,
            json: false,
        };
        let outcome = crate::run::run(&options).map_err(describe)?;
        Ok(RunArtifacts {
            scenario_hash: outcome.scenario_hash,
            world_hash: outcome.world_hash,
            content_digest: outcome.content_digest,
            // `v2xw run` writes its metric samples here; see `crate::run`'s table.
            metrics_path: out_dir.join("metrics.json"),
            wall_s: outcome.timing.total_s,
        })
    }
}

/// An error and its whole source chain as one line, because the runner's message is a
/// string and a bare `to_string` would drop the cause that names the field.
fn describe(error: CliError) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        text.push_str(" — caused by: ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
