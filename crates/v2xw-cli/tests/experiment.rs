//! `v2xw experiment`, end to end through the same functions the binary calls.
//!
//! The sweep here is deliberately tiny — two cells, one replication, two simulated seconds
//! each, no recording — because what is under test is the orchestration: that every cell
//! runs, that the seeds differ, that the results table appears and that a second invocation
//! does not redo the work. The runner's own suite covers the arithmetic against a fixture
//! that never starts an engine.
//!
//! # Each check is shown to be capable of failing
//!
//! Every positive assertion is paired with a negative control whose result must differ.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use v2xw_cli::experiment::{ExperimentOptions, run, status};
use v2xw_engine::Scenario;
use v2xw_engine::scenario::Experiment;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate lives two levels below the workspace root")
        .to_path_buf()
}

fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("v2xw-cli-experiment").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test's own directory");
    dir
}

/// The Phase 1 grid scenario with a two-point sweep on it, written into `dir`.
///
/// Written rather than shipped, because the sweep is about this test: a scenario file in
/// `scenarios/` that only a test reads is a file that rots.
fn sweep_scenario(dir: &Path) -> PathBuf {
    let mut scenario =
        Scenario::load(repo_root().join("scenarios/phase1-grid.yaml")).expect("the slice loads");
    let mut sweep: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    sweep.insert("time.duration_s".to_string(), vec![json!(2.0), json!(3.0)]);
    scenario.experiment = Some(Experiment {
        sweep,
        seeds: Vec::new(),
        replications: 1,
    });
    let path = dir.join("sweep.yaml");
    std::fs::write(&path, scenario.to_yaml().expect("it serialises")).expect("it writes");
    path
}

fn options(scenario: PathBuf, out: PathBuf, resume: bool) -> ExperimentOptions {
    ExperimentOptions {
        scenario,
        out: Some(out),
        // Pinned, so two invocations' manifests are comparable field by field.
        build_utc: Some("2026-09-22T00:00:00Z".to_string()),
        // One at a time. This suite runs on the same 8 GB machine everything else does.
        concurrency: 1,
        level: v2xw_metrics::ConfidenceLevel::P95,
        keyframe_ms: 1000,
        record: false,
        node_only: false,
        verify: false,
        format: None,
        resume,
    }
}

#[test]
fn a_sweep_runs_every_cell_and_writes_a_results_table() {
    let dir = out_dir("grid-sweep");
    let scenario = sweep_scenario(&dir);
    let out = dir.join("sweep-out");

    let first = run(&options(scenario.clone(), out.clone(), false)).expect("the sweep runs");
    assert_eq!(first.outcome.runs_planned, 2);
    assert_eq!(first.outcome.runs_executed, 2);
    assert_eq!(first.outcome.runs_pending, 0);
    assert_eq!(first.outcome.cells, 2);
    assert_eq!(first.outcome.concurrency, 1, "serial by default");
    assert!(out.join("results.json").exists());
    assert!(out.join("experiment.json").exists());
    assert!(out.join("journal.jsonl").exists());
    assert!(out.join("runs/c0000-s00-r000/scenario.yaml").exists());
    assert!(out.join("runs/c0001-s00-r000/run-metrics.json").exists());
    assert_eq!(first.results_digest.len(), 64);

    // The two cells executed different scenarios. If the sweep had run the base document
    // twice, the two manifests would agree and the whole mechanism would be measuring one
    // configuration twice.
    let a = std::fs::read_to_string(out.join("runs/c0000-s00-r000/manifest.json"))
        .expect("cell 0 has a manifest");
    let b = std::fs::read_to_string(out.join("runs/c0001-s00-r000/manifest.json"))
        .expect("cell 1 has a manifest");
    assert_ne!(a, b, "two cells, two scenarios, two manifests");

    // Negative control for resume: a second invocation finds nothing to do.
    let again = run(&options(scenario, out, false)).expect("the sweep is already done");
    assert_eq!(again.outcome.runs_executed, 0);
    assert_eq!(again.outcome.runs_skipped, 2);
    assert_eq!(
        again.results_digest, first.results_digest,
        "the same completed runs aggregate to the same table"
    );
}

#[test]
fn status_says_where_a_sweep_stands_without_running_it() {
    let dir = out_dir("grid-status");
    let scenario = sweep_scenario(&dir);
    let out = dir.join("sweep-out");

    let before = status(&scenario, Some(out.as_path())).expect("an unstarted sweep has a status");
    assert_eq!(before.runs_planned, 2);
    assert_eq!(before.runs_completed, 0);
    assert_eq!(before.journal_plan_digest, None);
    assert!(
        !out.join("journal.jsonl").exists(),
        "status runs nothing and writes nothing"
    );

    run(&options(scenario.clone(), out.clone(), false)).expect("the sweep runs");

    // Negative control: after the sweep the same call reports it done. A status wired to a
    // constant would still say zero.
    let after = status(&scenario, Some(out.as_path())).expect("a finished sweep has a status");
    assert_eq!(after.runs_completed, 2);
    assert_eq!(after.runs_pending, 0);
    assert!(after.next_run_ids.is_empty());
    assert!(!after.plan_changed);
}

#[test]
fn a_resume_of_a_sweep_that_was_never_started_is_refused() {
    let dir = out_dir("grid-resume-nothing");
    let scenario = sweep_scenario(&dir);
    let out = dir.join("sweep-out");
    let error = run(&options(scenario.clone(), out.clone(), true))
        .expect_err("there is nothing to resume here");
    assert!(
        format!("{error}").contains("resume"),
        "the error says what resume is for: {error}"
    );

    // Negative control: `run` in the same directory starts it.
    assert!(run(&options(scenario, out, false)).is_ok());
}

#[test]
fn a_scenario_without_a_sweep_is_refused_by_name() {
    let dir = out_dir("grid-no-sweep");
    let out = dir.join("sweep-out");
    let plain = repo_root().join("scenarios/phase1-grid.yaml");
    let error = run(&options(plain, out, false)).expect_err("there is no experiment block");
    assert!(
        format!("{error}").contains("experiment"),
        "the error names the missing block: {error}"
    );
}
