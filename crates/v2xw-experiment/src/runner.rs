//! Executing a plan: materialise, run, reduce, record, resume — and by default, one at a
//! time.
//!
//! # The concurrency default is one, and that is not a placeholder
//!
//! A single V2X run holds a world, a scheduler, every node's state and a recording
//! writer's chunk buffer. Running `k` of them at once multiplies **all** of that by `k`,
//! and the engine's own phase-parallel loop already uses the machine's cores inside one
//! run, so a second concurrent run mostly competes with the first for the same cores while
//! doubling the memory.
//!
//! This repository has already lost a whole wave of work to exactly that: several
//! processes compiling and running at once on an 8 GB machine, the OOM killer, and a job
//! that had to start over. [`DEFAULT_CONCURRENCY`] is therefore **1**, and
//! [`RunnerOptions::concurrency`] is clamped to at least 1. Raising it on a small machine
//! is how a sweep dies at run 340 of 600; the resume journal is what makes that survivable
//! rather than what makes it acceptable.
//!
//! If you do raise it: budget by *peak* memory of one run, not average, leave the
//! operating system a couple of gigabytes, and prefer `--no-recording` for cells whose
//! recording you will not open. Two concurrent runs of a dense city scenario is a lot on
//! any laptop.
//!
//! # What the runner itself does not do
//!
//! It does not run the engine. [`RunExecutor`] is the seam: the runner materialises a
//! scenario, writes it, hands the path to the executor and reads back what the executor
//! says it produced. `v2xw-cli` implements it with `v2xw run`, which is what puts the
//! *only* clock read in this whole path in the one crate that is allowed one. Nothing in
//! this crate reads a wall clock; the wall-clock seconds in a journal entry are a number
//! the executor handed over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use v2xw_engine::Scenario;
use v2xw_metrics::ConfidenceLevel;

use crate::aggregate::{AggregateOptions, CellAggregate, aggregate_cell};
use crate::error::{ExperimentError, Result};
use crate::journal::{JOURNAL_SCHEMA, Journal, JournalEntry, JournalHeader};
use crate::plan::{ExperimentPlan, PlannedRun};
use crate::reduce::{RUN_METRICS_SCHEMA, RunMetric, RunMetrics, read_samples, reduce_run};
use crate::table::ResultsTable;

/// The number of simulations the runner starts at once unless told otherwise.
///
/// One. See this module's header for why that is the correct default and not a stub.
pub const DEFAULT_CONCURRENCY: usize = 1;

/// The subdirectory each run's outputs go into, under the experiment directory.
pub const RUNS_DIR: &str = "runs";

/// The file a run's reduced metrics are written to, inside its own directory.
pub const RUN_METRICS_FILE: &str = "run-metrics.json";

/// The file the plan is written to.
pub const PLAN_FILE: &str = "experiment.json";

/// What one executed run produced.
///
/// The executor fills this in; the runner never inspects a recording itself.
#[derive(Debug, Clone, PartialEq)]
pub struct RunArtifacts {
    /// The scenario hash the run executed under.
    pub scenario_hash: String,
    /// The world's content hash.
    pub world_hash: String,
    /// SHA-256 over the recording's data section, when one was written.
    pub content_digest: Option<String>,
    /// Where the run's `metric.sample` records were written, as `{"samples": [...]}`.
    pub metrics_path: PathBuf,
    /// Wall-clock seconds the run took, for the journal. Never an input to anything.
    pub wall_s: f64,
}

/// What actually runs one scenario.
///
/// Implemented by `v2xw-cli` over `v2xw run`. The seam exists so that this crate does not
/// depend on the command-line tool — which depends on it — and so that a test can drive a
/// whole sweep with an executor that writes a fixed `metrics.json` and never starts an
/// engine.
pub trait RunExecutor {
    /// Runs `scenario_path` into `out_dir` and reports what it produced.
    ///
    /// # Errors
    /// Whatever went wrong, as a message the runner puts behind the run's id. A `String`
    /// rather than an error type, so an implementor is free to use its own.
    fn execute(
        &self,
        run: &PlannedRun,
        scenario_path: &Path,
        out_dir: &Path,
    ) -> core::result::Result<RunArtifacts, String>;
}

/// How a sweep is executed.
#[derive(Debug, Clone)]
pub struct RunnerOptions {
    /// Where the experiment's outputs go.
    pub out: PathBuf,
    /// How many simulations to start at once. Clamped to at least 1; see
    /// [`DEFAULT_CONCURRENCY`] before raising it.
    pub concurrency: usize,
    /// The confidence level for the aggregate.
    pub level: ConfidenceLevel,
    /// The minimum pooled trial count a proportion needs before it is reported.
    pub min_trials: u64,
    /// Refuse to start if the journal does not already exist — what `experiment resume`
    /// asks for, so that a typo in the output directory is a refusal rather than a sweep
    /// that silently starts from scratch.
    pub require_journal: bool,
}

impl RunnerOptions {
    /// The defaults for an output directory: serial, 95 %, the metrics crate's threshold.
    #[must_use]
    pub fn new(out: impl Into<PathBuf>) -> Self {
        RunnerOptions {
            out: out.into(),
            concurrency: DEFAULT_CONCURRENCY,
            level: ConfidenceLevel::P95,
            min_trials: v2xw_metrics::stats::DEFAULT_MIN_SAMPLES,
            require_journal: false,
        }
    }

    /// The concurrency actually used: never below one.
    #[must_use]
    pub fn effective_concurrency(&self) -> usize {
        self.concurrency.max(1)
    }

    /// The aggregation parameters these options imply.
    #[must_use]
    pub fn aggregate_options(&self) -> AggregateOptions {
        AggregateOptions {
            level: self.level,
            min_trials: self.min_trials,
        }
    }
}

/// What a sweep produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentOutcome {
    /// The experiment's name.
    pub name: String,
    /// The plan digest, which identifies the sweep.
    pub plan_digest: String,
    /// Where the outputs went.
    pub out_dir: String,
    /// How many runs the plan calls for.
    pub runs_planned: usize,
    /// How many were already done when this invocation started.
    pub runs_skipped: usize,
    /// How many this invocation executed.
    pub runs_executed: usize,
    /// How many runs are still outstanding, which is non-zero only if the sweep stopped.
    pub runs_pending: usize,
    /// How many cells the sweep has.
    pub cells: usize,
    /// How many rows the results table has.
    pub rows: usize,
    /// The concurrency actually used.
    pub concurrency: usize,
    /// The files written, relative to the output directory.
    pub files: Vec<String>,
}

/// Where a sweep stands, without running anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentStatus {
    /// The experiment's name.
    pub name: String,
    /// The plan digest the scenario expands to now.
    pub plan_digest: String,
    /// The plan digest the journal was started under, if there is a journal.
    pub journal_plan_digest: Option<String>,
    /// Where the outputs are.
    pub out_dir: String,
    /// How many runs the plan calls for.
    pub runs_planned: usize,
    /// How many are recorded as done.
    pub runs_completed: usize,
    /// How many are outstanding.
    pub runs_pending: usize,
    /// The sweep's cells.
    pub cells: usize,
    /// The declared seed slots.
    pub seed_slots: usize,
    /// Replications per (cell, slot).
    pub replications: u32,
    /// The first few outstanding run ids, so the operator can see where it stopped.
    pub next_run_ids: Vec<String>,
    /// True if the journal's plan digest disagrees with the scenario's — the sweep changed
    /// underneath the directory and a resume would mix two experiments.
    pub plan_changed: bool,
}

/// How many outstanding run ids a status report lists.
pub const STATUS_PREVIEW: usize = 10;

/// Reports where a sweep stands without executing anything.
///
/// # Errors
/// [`ExperimentError::NoExperiment`] if the scenario declares no sweep, and
/// [`ExperimentError::BadJournal`] if the journal will not parse. A *missing* journal is
/// not an error here: a sweep that has not been started yet has a status.
pub fn status(base: &Scenario, out: &Path) -> Result<ExperimentStatus> {
    let plan = ExperimentPlan::expand(base)?;
    let (journal_plan_digest, completed) = match Journal::read(out) {
        Ok((header, completed)) => (Some(header.plan_digest), completed),
        Err(ExperimentError::NoJournal { .. }) => (None, BTreeMap::new()),
        Err(e) => return Err(e),
    };
    let pending: Vec<&PlannedRun> = plan
        .runs
        .iter()
        .filter(|r| !completed.contains_key(&r.run_id))
        .collect();
    let plan_changed = journal_plan_digest
        .as_ref()
        .is_some_and(|d| *d != plan.digest);
    Ok(ExperimentStatus {
        name: plan.name.clone(),
        plan_digest: plan.digest.clone(),
        journal_plan_digest,
        out_dir: out.display().to_string(),
        runs_planned: plan.runs.len(),
        runs_completed: plan.runs.len().saturating_sub(pending.len()),
        runs_pending: pending.len(),
        cells: plan.cells.len(),
        seed_slots: plan.seed_slots.len(),
        replications: plan.replications,
        next_run_ids: pending
            .iter()
            .take(STATUS_PREVIEW)
            .map(|r| r.run_id.clone())
            .collect(),
        plan_changed,
    })
}

/// Runs a sweep, skipping whatever the journal says is already done.
///
/// This is both `experiment run` and `experiment resume`: the two differ only in
/// [`RunnerOptions::require_journal`], because a resume is a run that insists the sweep was
/// already started.
///
/// # Errors
/// [`ExperimentError::NoExperiment`] if the scenario declares no sweep,
/// [`ExperimentError::PlanChanged`] if the journal belongs to another sweep,
/// [`ExperimentError::Run`] if a run fails, and [`ExperimentError::Io`] for any output
/// that cannot be written. A run that fails stops the sweep with everything before it
/// recorded, so a rerun continues from there.
pub fn run_experiment<E: RunExecutor + Sync>(
    base: &Scenario,
    executor: &E,
    options: &RunnerOptions,
) -> Result<ExperimentOutcome> {
    let plan = ExperimentPlan::expand(base)?;
    std::fs::create_dir_all(&options.out)
        .map_err(|e| ExperimentError::io("cannot create the output directory", &options.out, e))?;

    if options.require_journal {
        let journal_path = options.out.join(crate::journal::JOURNAL_FILE);
        if !journal_path.exists() {
            return Err(ExperimentError::NoJournal { path: journal_path });
        }
    }

    // The journal is opened first, because opening it is what refuses a directory that
    // belongs to a different sweep — and a refused sweep must not have overwritten the
    // plan document of the sweep that is already in there.
    let mut journal = Journal::open(
        &options.out,
        JournalHeader {
            schema: JOURNAL_SCHEMA.to_string(),
            experiment: plan.name.clone(),
            plan_digest: plan.digest.clone(),
            runs_planned: plan.runs.len(),
        },
    )?;

    // Written before the first run, so a sweep killed halfway still says what it was
    // going to do.
    write_json(&options.out, PLAN_FILE, &plan)?;

    let pending: Vec<&PlannedRun> = plan
        .runs
        .iter()
        .filter(|r| !journal.is_done(&r.run_id))
        .collect();
    let runs_skipped = plan.runs.len() - pending.len();

    let context = Context {
        base,
        plan: &plan,
        executor,
        out: options.out.as_path(),
    };
    let concurrency = options.effective_concurrency();
    let mut executed = 0usize;
    // Chunked rather than a work-queue: the journal is then appended in plan order
    // whatever order the workers finished in, so two interrupted sweeps that completed the
    // same runs have byte-identical journals.
    for chunk in pending.chunks(concurrency) {
        let results = execute_chunk(chunk, &context);
        for result in results {
            match result {
                Ok(entry) => {
                    journal.record(entry)?;
                    executed += 1;
                }
                // Everything before this is recorded, so a rerun resumes here, and the
                // error already names the run.
                Err(e) => return Err(e),
            }
        }
    }

    let table = build_table(&plan, &journal, options)?;
    let mut files = vec![
        PLAN_FILE.to_string(),
        crate::journal::JOURNAL_FILE.to_string(),
    ];
    files.extend(table.write_json(&options.out)?);

    let runs_pending = plan
        .runs
        .iter()
        .filter(|r| !journal.is_done(&r.run_id))
        .count();
    Ok(ExperimentOutcome {
        name: plan.name.clone(),
        plan_digest: plan.digest.clone(),
        out_dir: options.out.display().to_string(),
        runs_planned: plan.runs.len(),
        runs_skipped,
        runs_executed: executed,
        runs_pending,
        cells: plan.cells.len(),
        rows: table.rows.len(),
        concurrency,
        files,
    })
}

/// Rebuilds the results table from what the journal says is done, without running
/// anything.
///
/// This is what makes `experiment status --table` and a re-export possible after the fact:
/// each run's reduced metrics are on disk, so the table is a pure function of them.
///
/// # Errors
/// [`ExperimentError::NoJournal`] if the sweep was never started, and
/// [`ExperimentError::Io`] if a run's reduced metrics cannot be read.
pub fn rebuild_table(base: &Scenario, options: &RunnerOptions) -> Result<ResultsTable> {
    let plan = ExperimentPlan::expand(base)?;
    let (header, completed) = crate::journal::Journal::read(&options.out)?;
    if header.plan_digest != plan.digest {
        return Err(ExperimentError::PlanChanged {
            journal: options.out.join(crate::journal::JOURNAL_FILE),
            recorded: header.plan_digest,
            current: plan.digest,
        });
    }
    table_from(&plan, &completed, options)
}

/// The shared, read-only state a worker needs.
struct Context<'a, E: RunExecutor + Sync> {
    base: &'a Scenario,
    plan: &'a ExperimentPlan,
    executor: &'a E,
    out: &'a Path,
}

impl<E: RunExecutor + Sync> Context<'_, E> {
    /// Materialises, runs and reduces one run.
    fn execute_one(&self, run: &PlannedRun) -> Result<JournalEntry> {
        let relative = format!("{RUNS_DIR}/{}", run.run_id);
        let dir = self.out.join(RUNS_DIR).join(&run.run_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| ExperimentError::io("cannot create the run directory", &dir, e))?;

        let scenario = self.plan.materialise(self.base, run)?;
        let yaml = scenario.to_yaml().map_err(v2xw_engine::EngineError::from)?;
        let scenario_path = dir.join("scenario.yaml");
        std::fs::write(&scenario_path, yaml.as_bytes()).map_err(|e| {
            ExperimentError::io("cannot write the run's scenario", &scenario_path, e)
        })?;

        let artifacts = self
            .executor
            .execute(run, &scenario_path, &dir)
            .map_err(|message| ExperimentError::Run {
                run_id: run.run_id.clone(),
                message,
            })?;

        let samples = read_samples(&artifacts.metrics_path)?;
        let metrics = reduce_run(&samples)?;
        let reduced = RunMetrics {
            schema: RUN_METRICS_SCHEMA.to_string(),
            run_id: run.run_id.clone(),
            cell_index: run.cell.index,
            seed_hex: run.seed_hex(),
            scenario_hash: artifacts.scenario_hash.clone(),
            metrics,
        };
        write_json(&dir, RUN_METRICS_FILE, &reduced)?;

        Ok(JournalEntry {
            run_id: run.run_id.clone(),
            cell_index: run.cell.index,
            seed_hex: run.seed_hex(),
            scenario_hash: artifacts.scenario_hash,
            world_hash: artifacts.world_hash,
            out_dir: relative,
            content_digest: artifacts.content_digest,
            wall_s: artifacts.wall_s,
        })
    }
}

/// Runs a chunk, serially when the chunk holds one run and on scoped threads otherwise.
fn execute_chunk<E: RunExecutor + Sync>(
    chunk: &[&PlannedRun],
    context: &Context<'_, E>,
) -> Vec<Result<JournalEntry>> {
    if chunk.len() <= 1 {
        return chunk.iter().map(|run| context.execute_one(run)).collect();
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunk
            .iter()
            .map(|run| {
                let run: &PlannedRun = *run;
                scope.spawn(move || context.execute_one(run))
            })
            .collect();
        handles
            .into_iter()
            .zip(chunk.iter())
            .map(|(handle, run)| match handle.join() {
                Ok(result) => result,
                Err(_) => Err(ExperimentError::WorkerPanicked {
                    run_id: run.run_id.clone(),
                }),
            })
            .collect()
    })
}

/// Builds the results table from the journal's completed runs.
fn build_table(
    plan: &ExperimentPlan,
    journal: &Journal,
    options: &RunnerOptions,
) -> Result<ResultsTable> {
    table_from(plan, journal.completed(), options)
}

fn table_from(
    plan: &ExperimentPlan,
    completed: &BTreeMap<String, JournalEntry>,
    options: &RunnerOptions,
) -> Result<ResultsTable> {
    let mut rows: Vec<CellAggregate> = Vec::new();
    for cell in &plan.cells {
        // One entry per replication of this cell that actually finished, in plan order.
        let mut replications: Vec<BTreeMap<String, RunMetric>> = Vec::new();
        for run in plan.runs.iter().filter(|r| r.cell.index == cell.index) {
            if !completed.contains_key(&run.run_id) {
                continue;
            }
            let path = options
                .out
                .join(RUNS_DIR)
                .join(&run.run_id)
                .join(RUN_METRICS_FILE);
            let bytes = std::fs::read(&path).map_err(|e| {
                ExperimentError::io("cannot read a run's reduced metrics", &path, e)
            })?;
            let reduced: RunMetrics = serde_json::from_slice(&bytes)
                .map_err(|e| ExperimentError::json("a run's reduced metrics", e))?;
            replications.push(reduced.metrics);
        }
        if replications.is_empty() {
            continue;
        }
        rows.extend(aggregate_cell(
            cell,
            &replications,
            options.aggregate_options(),
        ));
    }
    Ok(ResultsTable::new(plan, rows))
}

/// Writes a JSON document, canonically, into `dir`.
fn write_json<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<()> {
    let path = dir.join(name);
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| ExperimentError::json("an experiment document", e))?;
    std::fs::write(&path, &bytes).map_err(|e| ExperimentError::io("cannot write", &path, e))?;
    Ok(())
}
