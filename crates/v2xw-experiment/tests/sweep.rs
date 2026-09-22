//! A whole sweep, end to end, against a fixture executor that never starts an engine.
//!
//! The runner's contract is about *orchestration* — which runs, in what order, under what
//! seeds, skipping what is done — so the engine is exactly the part that should not be in
//! these tests. The fixture writes the `metrics.json` a real run would write and nothing
//! else, which also means the suite stays within a small machine's memory.
//!
//! # Each check is shown to be capable of failing
//!
//! Every positive assertion is paired with a negative control: a perturbed input whose
//! result must differ. A resume that redid everything, an aggregate that ignored its
//! replications or a journal that accepted any plan would turn these red.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::json;
use v2xw_core::ctx::Visibility;
use v2xw_engine::Scenario;
use v2xw_engine::scenario::Experiment;
use v2xw_experiment::aggregate::CiMethod;
use v2xw_experiment::error::ExperimentError;
use v2xw_experiment::plan::PlannedRun;
use v2xw_experiment::runner::{
    RunArtifacts, RunExecutor, RunnerOptions, rebuild_table, run_experiment, status,
};
use v2xw_metrics::stats::{ConfidenceLevel, Proportion};
use v2xw_metrics::{Agg, Dims, MetricDef, MetricSample, Quantum, SampleValue};

/// A directory of this test's own, removed first so a rerun does not read a stale journal.
fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("v2xw-experiment-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A scenario sweeping `time.duration_s` over `values`, with `replications` replications.
fn swept(values: Vec<serde_json::Value>, replications: u32) -> Scenario {
    let mut scenario = Scenario::minimal();
    let mut sweep: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    if !values.is_empty() {
        sweep.insert("time.duration_s".to_string(), values);
    }
    scenario.experiment = Some(Experiment {
        sweep,
        seeds: Vec::new(),
        replications,
    });
    scenario
}

fn pdr_def() -> MetricDef {
    MetricDef::new(
        "pdr",
        "ratio",
        Agg::ratio("received", "candidates"),
        Visibility::Node,
        Quantum::RATIO,
        "received frames over candidate receptions",
    )
    .not_accounting_for("a fixture's arithmetic, which is not a radio")
}

fn frames_def() -> MetricDef {
    MetricDef::new(
        "frames",
        "count",
        Agg::Count,
        Visibility::Node,
        Quantum::COUNT,
        "frames the fixture claims to have transmitted",
    )
    .not_accounting_for("a fixture's arithmetic, which is not a radio")
}

/// An executor that writes the metrics a run would write, and never runs one.
struct Fixture {
    fail_on: Vec<String>,
    executed: Mutex<Vec<String>>,
}

impl Fixture {
    fn new(fail_on: &[&str]) -> Fixture {
        Fixture {
            fail_on: fail_on.iter().map(|s| (*s).to_string()).collect(),
            executed: Mutex::new(Vec::new()),
        }
    }

    fn executed(&self) -> Vec<String> {
        self.executed
            .lock()
            .expect("the fixture's list is never poisoned")
            .clone()
    }
}

impl RunExecutor for Fixture {
    fn execute(
        &self,
        run: &PlannedRun,
        scenario_path: &Path,
        dir: &Path,
    ) -> Result<RunArtifacts, String> {
        assert!(
            scenario_path.exists(),
            "the runner writes the run's scenario before calling the executor"
        );
        if self.fail_on.contains(&run.run_id) {
            return Err("the fixture was told to fail on this run".to_string());
        }
        self.executed
            .lock()
            .expect("the fixture's list is never poisoned")
            .push(run.run_id.clone());

        // A different, seed-dependent answer per replication, so an aggregate that ignored
        // its replications would produce an interval of zero width.
        let trials: u64 = 100;
        let successes: u64 = 50 + (run.seed % 21);
        let pdr = MetricSample::new(
            &pdr_def(),
            1_000_000_000,
            Dims::new(),
            SampleValue::Ratio(
                Proportion::from_counts(successes, trials).estimate(1, ConfidenceLevel::P95),
            ),
        );
        let frames = MetricSample::new(
            &frames_def(),
            1_000_000_000,
            Dims::new(),
            SampleValue::count(trials),
        );
        let document = json!({"samples": [pdr, frames]});
        let path = dir.join("metrics.json");
        let bytes = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;

        Ok(RunArtifacts {
            scenario_hash: format!("scenario-{}", run.run_id),
            world_hash: "world".to_string(),
            content_digest: Some(format!("digest-{}", run.run_id)),
            metrics_path: path,
            wall_s: 0.0,
        })
    }
}

#[test]
fn the_default_concurrency_is_one() {
    // Not a style preference: several simulations at once on a small machine exhaust its
    // memory, and this project has lost a wave of work to exactly that.
    let options = RunnerOptions::new("unused");
    assert_eq!(options.concurrency, 1);
    assert_eq!(options.effective_concurrency(), 1);
    assert_eq!(v2xw_experiment::DEFAULT_CONCURRENCY, 1);

    // Negative control: a zero is clamped up rather than dividing by nothing.
    let mut zero = RunnerOptions::new("unused");
    zero.concurrency = 0;
    assert_eq!(zero.effective_concurrency(), 1);
}

#[test]
fn a_sweep_runs_every_cell_and_aggregates_across_replications() {
    let dir = out_dir("full-sweep");
    let scenario = swept(vec![json!(10.0), json!(20.0)], 3);
    let fixture = Fixture::new(&[]);
    let options = RunnerOptions::new(&dir);

    let outcome = run_experiment(&scenario, &fixture, &options).expect("the sweep runs");
    assert_eq!(outcome.runs_planned, 6, "two cells, three replications");
    assert_eq!(outcome.runs_executed, 6);
    assert_eq!(outcome.runs_skipped, 0);
    assert_eq!(outcome.runs_pending, 0);
    assert_eq!(outcome.cells, 2);

    let table = rebuild_table(&scenario, &options).expect("the table rebuilds from disk");
    let pdr: Vec<_> = table.rows.iter().filter(|r| r.metric == "pdr").collect();
    assert_eq!(pdr.len(), 2, "one row per cell");
    for row in &pdr {
        assert_eq!(row.replications, 3);
        assert_eq!(row.replications_with_value, 3);
        assert_eq!(row.samples, 300, "three replications of a hundred trials");
        assert_eq!(row.ci_method, CiMethod::Wilson);
        let mean = row.mean.expect("a pooled proportion has a point estimate");
        let lo = row.ci_lo.expect("and a Wilson interval");
        let hi = row.ci_hi.expect("and a Wilson interval");
        assert!(lo < hi, "an interval has width: [{lo}, {hi}]");
        assert!(lo <= mean && mean <= hi, "{lo} <= {mean} <= {hi}");
        assert_eq!(row.per_replication.len(), 3);
    }

    // The count metric is not a proportion, so it gets the normal approximation and says
    // so rather than borrowing Wilson's name.
    let frames: Vec<_> = table.rows.iter().filter(|r| r.metric == "frames").collect();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].ci_method, CiMethod::NormalOverReplications);

    // Negative control: the results table is written, and it is not empty. A runner that
    // aggregated nothing would still have returned `Ok`.
    assert!(dir.join("results.json").exists());
    assert!(dir.join("experiment.json").exists());
    assert!(dir.join("journal.jsonl").exists());
    assert!(!table.rows.is_empty());
}

#[test]
fn an_interrupted_sweep_resumes_where_it_stopped() {
    let dir = out_dir("resume");
    let scenario = swept(vec![json!(10.0), json!(20.0)], 3);

    let failing = Fixture::new(&["c0001-s00-r000"]);
    let error = run_experiment(&scenario, &failing, &RunnerOptions::new(&dir))
        .expect_err("the fixture fails on the fourth run");
    assert!(
        format!("{error}").contains("c0001-s00-r000"),
        "the error names the run: {error}"
    );
    assert_eq!(
        failing.executed().len(),
        3,
        "the three runs of cell 0 finished before the failure"
    );

    let resumed = Fixture::new(&[]);
    let mut options = RunnerOptions::new(&dir);
    options.require_journal = true;
    let outcome =
        run_experiment(&scenario, &resumed, &options).expect("the sweep resumes from the journal");
    assert_eq!(outcome.runs_skipped, 3, "the finished runs are not redone");
    assert_eq!(outcome.runs_executed, 3);
    assert_eq!(outcome.runs_pending, 0);
    assert_eq!(
        resumed.executed(),
        vec![
            "c0001-s00-r000".to_string(),
            "c0001-s00-r001".to_string(),
            "c0001-s00-r002".to_string()
        ],
        "resume runs exactly what was outstanding, in plan order"
    );

    // Negative control: a third invocation has nothing left to do. If resume were a no-op
    // that always reran everything, this would execute six runs again.
    let again = Fixture::new(&[]);
    let outcome = run_experiment(&scenario, &again, &options).expect("nothing is outstanding");
    assert_eq!(outcome.runs_executed, 0);
    assert_eq!(outcome.runs_skipped, 6);
    assert!(again.executed().is_empty());
}

#[test]
fn a_resume_of_a_sweep_that_was_never_started_is_refused() {
    let dir = out_dir("resume-nothing");
    let scenario = swept(vec![json!(10.0)], 1);
    let mut options = RunnerOptions::new(&dir);
    options.require_journal = true;
    let error = run_experiment(&scenario, &Fixture::new(&[]), &options)
        .expect_err("there is nothing to resume");
    assert!(
        matches!(error, ExperimentError::NoJournal { .. }),
        "{error}"
    );

    // Negative control: without `require_journal`, the same call starts the sweep.
    let started = RunnerOptions::new(&dir);
    assert!(run_experiment(&scenario, &Fixture::new(&[]), &started).is_ok());
}

#[test]
fn a_journal_from_a_different_sweep_is_refused() {
    let dir = out_dir("plan-changed");
    let first = swept(vec![json!(10.0), json!(20.0)], 1);
    run_experiment(&first, &Fixture::new(&[]), &RunnerOptions::new(&dir))
        .expect("the first sweep runs");

    let changed = swept(vec![json!(10.0), json!(30.0)], 1);
    let error = run_experiment(&changed, &Fixture::new(&[]), &RunnerOptions::new(&dir))
        .expect_err("the sweep changed underneath the directory");
    assert!(
        matches!(error, ExperimentError::PlanChanged { .. }),
        "{error}"
    );

    // Negative control: the unchanged sweep is still accepted in the same directory, so
    // the refusal is about the plan and not about the directory being used twice.
    assert!(run_experiment(&first, &Fixture::new(&[]), &RunnerOptions::new(&dir)).is_ok());
}

#[test]
fn status_reports_progress_without_running_anything() {
    let dir = out_dir("status");
    let scenario = swept(vec![json!(10.0), json!(20.0)], 2);

    let before = status(&scenario, &dir).expect("a sweep that has not started has a status");
    assert_eq!(before.runs_planned, 4);
    assert_eq!(before.runs_completed, 0);
    assert_eq!(before.runs_pending, 4);
    assert_eq!(before.journal_plan_digest, None);
    assert!(!before.plan_changed);
    assert_eq!(
        before.next_run_ids.first().map(String::as_str),
        Some("c0000-s00-r000")
    );

    let failing = Fixture::new(&["c0000-s00-r001"]);
    let _ = run_experiment(&scenario, &failing, &RunnerOptions::new(&dir));

    let after = status(&scenario, &dir).expect("a started sweep has a status");
    assert_eq!(after.runs_completed, 1);
    assert_eq!(after.runs_pending, 3);
    assert_eq!(
        after.journal_plan_digest.as_deref(),
        Some(after.plan_digest.as_str())
    );
    assert_eq!(
        after.next_run_ids.first().map(String::as_str),
        Some("c0000-s00-r001"),
        "status names where it stopped"
    );
}

#[test]
fn one_replication_gets_a_mean_and_no_error_bar() {
    let dir = out_dir("single-replication");
    let scenario = swept(vec![json!(10.0)], 1);
    run_experiment(&scenario, &Fixture::new(&[]), &RunnerOptions::new(&dir)).expect("it runs");
    let table = rebuild_table(&scenario, &RunnerOptions::new(&dir)).expect("the table rebuilds");

    let frames = table
        .rows
        .iter()
        .find(|r| r.metric == "frames")
        .expect("the count metric is in the table");
    assert_eq!(frames.replications, 1);
    assert_eq!(
        frames.mean,
        Some(100.0),
        "the measurement is still reported"
    );
    assert_eq!(
        frames.ci_method,
        CiMethod::None,
        "one run does not earn an error bar"
    );
    assert_eq!(frames.ci_lo, None);
    assert_eq!(frames.stddev, None);
}

#[test]
fn the_results_table_exports_to_the_formats_the_exporters_produce() {
    use v2xw_record::export::{ExportFormat, jsonl};

    let dir = out_dir("export");
    let scenario = swept(vec![json!(10.0), json!(20.0)], 2);
    run_experiment(&scenario, &Fixture::new(&[]), &RunnerOptions::new(&dir)).expect("it runs");
    let table = rebuild_table(&scenario, &RunnerOptions::new(&dir)).expect("the table rebuilds");

    let files = table
        .write_table(&dir, ExportFormat::Jsonl)
        .expect("JSONL is one of the formats the exporters produce");
    assert_eq!(files.len(), 2, "the data file and its schema sidecar");
    let lines = jsonl::read(&files[0].path).expect("the JSONL reads back");
    assert_eq!(lines.len(), table.rows.len());
    assert!(!lines.is_empty());

    // The swept parameter is a column, which is what makes the table queryable by cell.
    let column = v2xw_experiment::table::param_column("time.duration_s");
    assert_eq!(column, "param_time_duration_s");
    assert!(
        lines[0].get(&column).is_some(),
        "every row names its sweep point: {:?}",
        lines[0]
    );

    let parquet = table
        .write_table(&dir, ExportFormat::Parquet)
        .expect("Parquet is the primary format");
    assert!(parquet[0].path.exists());
    assert!(parquet[0].bytes > 0);
}
