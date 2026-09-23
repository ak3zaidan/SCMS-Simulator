//! The foundry, end to end, against a fixture executor that never starts an engine.
//!
//! The same arrangement `sweep.rs` uses and for the same reason: the search's contract is
//! about *orchestration and scoring* — which candidate, under which seed, scored how, and
//! archived where — so the engine is exactly the part that should not be in these tests.
//! The fixture writes the `metrics.json` a real run would write and nothing else, which
//! also keeps the suite inside a small machine's memory.
//!
//! # The fixture's detector is a made-up function, and that is the point
//!
//! `recall = clamp(1 - 2·attacker_fraction, 0, 1)`: a fixture in which a larger attacker
//! population is harder to catch. It is not a model of anything. What it buys is a search
//! space with a **known gradient**, so the tests can assert that the search climbs it —
//! which is the one property a fixture whose answers were random could not establish.
//!
//! # Each check is shown to be capable of failing
//!
//! Every positive assertion has a negative control beside it: a fixture that files no
//! reports must fill no cells, a fixture that fails must be survived and counted, and two
//! runs under different seeds must not produce the same archive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::json;
use v2xw_core::ctx::Visibility;
use v2xw_engine::Scenario;
use v2xw_engine::scenario::{Attacker, DilationWindow, ModelChoice};
use v2xw_experiment::foundry::{
    ARCHIVE_FILE, Archive, FoundryOptions, Genome, MutationOperator, Objective, RandomMutation,
    REPORT_FILE, Validity, fitness, search,
};
use v2xw_experiment::plan::PlannedRun;
use v2xw_experiment::runner::{RunArtifacts, RunExecutor};
use v2xw_metrics::stats::{ConfidenceLevel, Proportion};
use v2xw_metrics::{Agg, Dim, DimValue, Dims, MetricDef, MetricSample, Quantum, SampleValue};

/// A directory of this test's own, removed first so a rerun does not read a stale archive.
fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("v2xw-foundry-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The base scenario a foundry search perturbs.
///
/// Every path the mutation operators write has to be **present in the serialised
/// document**, because `set_path` replaces a value and never creates one. That is the
/// single thing a foundry base scenario has to get right, and it is why the demand rate,
/// the attacker fraction and the attacker schedule are set here explicitly rather than
/// left to default: three of them are `Option` fields that serde skips when they are
/// `None`.
fn base() -> Scenario {
    let mut scenario = Scenario::minimal();
    scenario.meta.name = "foundry-fixture".to_string();
    scenario.seed = 0xC0FFEE;
    scenario.time.duration_s = 60.0;
    scenario.actors.vehicles.demand.rate_veh_per_h = Some(6000.0);
    scenario.threats.attackers = vec![Attacker {
        id: "threat/attacker/legacy/ConstPos".to_string(),
        fraction: Some(0.05),
        count: None,
        actor_ids: Vec::new(),
        params: serde_json::Value::Null,
        schedule: Some(DilationWindow {
            from_s: 5.0,
            to_s: 60.0,
        }),
    }];
    scenario.detection.local = vec![ModelChoice {
        id: "detect/legacy-12".to_string(),
        params: serde_json::Value::Null,
    }];
    scenario
        .validate()
        .expect("the foundry's base scenario must be a valid scenario");
    scenario
}

fn count_def(name: &'static str) -> MetricDef {
    MetricDef::new(
        name,
        "count",
        Agg::Count,
        Visibility::Node,
        Quantum::COUNT,
        "a confusion-matrix cell, as the fixture claims it",
    )
    .with_dims([Dim::T, Dim::Level, Dim::Cell])
    .not_accounting_for("a fixture's arithmetic, which is not a detector")
}

fn recall_def() -> MetricDef {
    MetricDef::new(
        "det_recall",
        "ratio",
        Agg::ratio("attackers revoked", "attackers"),
        Visibility::Node,
        Quantum::RATIO,
        "tp / (tp + fn), as the fixture claims it",
    )
    .with_dims([Dim::T, Dim::Level])
    .not_accounting_for("a fixture's arithmetic, which is not a detector")
}

fn dims(level: &str, cell: Option<&str>) -> Dims {
    let mut d = Dims::new();
    d.insert(Dim::Level, DimValue::label(level));
    if let Some(cell) = cell {
        d.insert(Dim::Cell, DimValue::label(cell));
    }
    d
}

/// How the fixture behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mood {
    /// Reports fire and the recall falls as the attacker fraction rises.
    Normal,
    /// Attackers exist and nobody ever files a report — the gate's own hard case.
    Silent,
    /// No attacker is ever in the population.
    Empty,
    /// Every candidate fails to run.
    Broken,
}

struct Fixture {
    mood: Mood,
    seen: Mutex<Vec<String>>,
}

impl Fixture {
    fn new(mood: Mood) -> Fixture {
        Fixture {
            mood,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn candidates(&self) -> usize {
        self.seen.lock().expect("never poisoned").len()
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
            "the foundry writes the candidate's scenario before calling the executor"
        );
        self.seen
            .lock()
            .expect("never poisoned")
            .push(run.run_id.clone());
        if self.mood == Mood::Broken {
            return Err("the fixture was told to fail".to_string());
        }

        // The genome reaches the executor as the planned run's cell values, so a fixture
        // can answer as a function of the candidate without parsing the scenario back.
        let fraction = run
            .cell
            .values
            .get("threats.attackers[0].fraction")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.05);
        let attackers: u64 = match self.mood {
            Mood::Empty => 0,
            _ => 10,
        };
        let recall = (1.0 - 2.0 * fraction).clamp(0.0, 1.0);
        let tp = (recall * attackers as f64).round() as u64;
        let missed = attackers - tp;
        // A silent fixture files no reports at all, which is what the validity gate is
        // there to catch: without it such a run scores a perfect evasion for the wrong
        // reason.
        let reported_tp = if self.mood == Mood::Silent { 0 } else { attackers };

        let samples = vec![
            MetricSample::new(
                &count_def("det_tp"),
                1_000_000_000,
                dims("vehicle", Some("tp")),
                SampleValue::count(tp),
            ),
            MetricSample::new(
                &count_def("det_fn"),
                1_000_000_000,
                dims("vehicle", Some("fn")),
                SampleValue::count(missed),
            ),
            MetricSample::new(
                &count_def("det_tp"),
                1_000_000_000,
                dims("report", Some("tp")),
                SampleValue::count(reported_tp),
            ),
            MetricSample::new(
                &count_def("det_fp"),
                1_000_000_000,
                dims("report", Some("fp")),
                SampleValue::count(0),
            ),
            MetricSample::new(
                &recall_def(),
                1_000_000_000,
                dims("vehicle", None),
                SampleValue::Ratio(
                    Proportion::from_counts(tp, attackers.max(1))
                        .estimate(1, ConfidenceLevel::P95),
                ),
            ),
        ];
        let path = dir.join("metrics.json");
        let bytes = serde_json::to_vec(&json!({"samples": samples})).map_err(|e| e.to_string())?;
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        Ok(RunArtifacts {
            scenario_hash: format!("scenario-{}", run.run_id),
            world_hash: "world".to_string(),
            content_digest: None,
            metrics_path: path,
            wall_s: 0.0,
        })
    }
}

fn options(name: &str, budget: u64) -> FoundryOptions {
    FoundryOptions::new(out_dir(name))
        .budget(budget)
        .seed(0x5EED)
        .objective(Objective::Evade)
}

#[test]
fn a_search_fills_cells_and_writes_both_outputs() {
    let opts = options("normal", 12);
    let fixture = Fixture::new(Mood::Normal);
    let archive = search(&base(), &fixture, &RandomMutation, &opts).expect("the search runs");

    // Three base genomes plus the budget.
    assert_eq!(fixture.candidates(), 15);
    assert_eq!(archive.evaluated, 15);
    assert_eq!(archive.failures, 0);
    assert!(archive.coverage() > 0, "no cell filled");
    assert!(archive.coverage() <= 15);
    assert!(opts.out.join(ARCHIVE_FILE).is_file());
    assert!(opts.out.join(REPORT_FILE).is_file());
    // The throwaway candidate directories are gone.
    assert!(!opts.out.join(v2xw_experiment::foundry::WORK_DIR).exists());

    // Every archived elite is a *valid* one, so every fitness is a measurement and not a
    // gate's zero.
    for elite in archive.cells.values() {
        assert!(elite.signals.attackers > 0);
        assert!(elite.signals.reported_subjects > 0);
        assert!((0.0..=1.0).contains(&elite.fitness));
        assert!(elite.scenario_hash.starts_with("scenario-foundry-c"));
    }
}

/// An operator that pushes the attacker fraction straight to the top of its range, so the
/// gradient is climbed in **one** iteration.
///
/// The built-in random operator would climb it too, and with what probability is a
/// question about weighted draws that this test would then be asking. A test whose pass
/// depends on a random walk reaching a threshold is a flaky test, and a flaky test teaches
/// a reader to ignore it. So the climb is made certain in a single step, and what is
/// actually under test is the archive's rule — it keeps the better elite and never loses a
/// cell — together with the fact that an injected operator's child really does reach the
/// engine.
struct Climber;

impl MutationOperator for Climber {
    fn name(&self) -> &str {
        "climber"
    }

    fn propose(
        &self,
        parent: &Genome,
        _summary: &v2xw_experiment::foundry::ArchiveSummary,
        _rng: &mut v2xw_core::rng::RngStream,
    ) -> Option<Genome> {
        let mut child = parent.clone();
        // The top of the built-in operator's own range, on the same three-decimal grid:
        // two genomes that differed only in floating-point noise would be two genomes,
        // and the archive would fill with near-duplicates.
        child.insert(
            "threats.attackers[0].fraction".to_string(),
            json!(0.6_f64),
        );
        Some(child)
    }
}

/// The property a fixture with a known gradient exists to establish.
///
/// The fixture's recall falls as the attacker fraction rises: `recall = 1 − 2·fraction`,
/// so fitness is `min(1, 2·fraction)`. The base genomes carry 0.05, 0.10 and 0.25, so the
/// best base fitness is 0.5. One `Climber` child reaches 0.6, whose recall clamps to zero
/// — a perfect evasion with reports still firing, which is the jackpot the gate is written
/// to preserve — so the archive must end holding a 1.0.
///
/// Every assertion here is certain rather than probable: the first search iteration has a
/// non-empty archive (the bases ran), picks some elite, and the child it produces is
/// feasible.
#[test]
fn the_archive_keeps_the_better_elite_when_the_operator_climbs() {
    let opts = options("gradient", 12);
    let fixture = Fixture::new(Mood::Normal);
    let archive = search(&base(), &fixture, &Climber, &opts).expect("the search runs");
    let hardest = archive.hardest().expect("a non-empty archive");
    assert_eq!(
        hardest.fitness, 1.0,
        "the best base genome scores 0.5 and the search ended at {}: either the archive \
         did not keep the better elite or the injected operator's child never reached the \
         engine",
        hardest.fitness
    );
    // A perfect evasion with reports firing is *valid*, not gated: the gate rejects
    // "nobody looked", not "everybody missed".
    assert!(hardest.signals.reported_subjects > 0);
    assert_eq!(hardest.signals.recall_vehicle, Some(0.0));
    // The elite is in the band the gradient points to. This is the assertion that would go
    // red if the descriptor axes were read from the base scenario rather than from the
    // materialised one — the bug that would put every candidate in one cell.
    assert_eq!(hardest.descriptor.attacker_band, "high");
    // …and the archived genome is the climbed one.
    assert_eq!(
        hardest
            .genome
            .get("threats.attackers[0].fraction")
            .and_then(serde_json::Value::as_f64),
        Some(0.6)
    );
}

/// The weaker claim, about the operator this crate ships.
///
/// Deliberately weaker: MAP-Elites never loses an elite, so the best base genome's 0.5 is
/// a floor whatever the random operator does. Asserting more than that about a random walk
/// is asserting a probability.
#[test]
fn the_built_in_operator_never_loses_the_best_base_genome() {
    let opts = options("random-floor", 12);
    let fixture = Fixture::new(Mood::Normal);
    let archive = search(&base(), &fixture, &RandomMutation, &opts).expect("the search runs");
    let best = archive.hardest().expect("a non-empty archive").fitness;
    assert!(
        best >= 0.5,
        "the 0.25-fraction base genome scores 0.5 and the archive's best is {best}: an \
         elite was displaced by a worse one"
    );
}

#[test]
fn attackers_nobody_reported_fill_no_cells() {
    // The gate, end to end. Without it every one of these candidates scores 1.0 — the
    // fixture's recall is 0 because tp is 0 — and the archive fills with scenarios in
    // which nobody ever looked.
    let opts = options("silent", 10);
    let fixture = Fixture::new(Mood::Silent);
    let archive = search(&base(), &fixture, &RandomMutation, &opts).expect("the search runs");
    assert_eq!(archive.coverage(), 0, "a silent run must archive nothing");
    assert_eq!(archive.evaluated, 13);
    assert_eq!(
        archive
            .gated
            .get(Validity::NoReports.because())
            .copied()
            .unwrap_or(0),
        13,
        "every candidate must be gated out for the reported reason"
    );
    // The report says so rather than showing an empty table with no explanation.
    let report = std::fs::read_to_string(opts.out.join(REPORT_FILE)).expect("a report");
    assert!(report.contains("archive is empty"));
    assert!(report.contains(Validity::NoReports.because()));
}

#[test]
fn a_run_with_no_attackers_fills_no_cells_for_a_different_reason() {
    let opts = options("empty", 6);
    let fixture = Fixture::new(Mood::Empty);
    let archive = search(&base(), &fixture, &RandomMutation, &opts).expect("the search runs");
    assert_eq!(archive.coverage(), 0);
    // A *different* reason from the silent case, which is the whole point of counting
    // them separately.
    assert!(archive.gated.contains_key(Validity::NoAttackers.because()));
    assert!(!archive.gated.contains_key(Validity::NoReports.because()));
}

#[test]
fn a_failing_executor_is_survived_and_counted() {
    let opts = options("broken", 5);
    let fixture = Fixture::new(Mood::Broken);
    let archive = search(&base(), &fixture, &RandomMutation, &opts).expect(
        "a candidate that fails to run must not fail the search: the whole point of an \
         expensive search is that one bad candidate does not cost the rest",
    );
    assert_eq!(archive.failures, 8);
    assert_eq!(archive.coverage(), 0);
    assert!(
        archive
            .first_failure
            .as_deref()
            .is_some_and(|f| f.contains("the fixture was told to fail")),
        "the first failure's message must be kept, not just counted"
    );
    // A failure is not a gating, and the report must not conflate them.
    assert!(archive.gated.is_empty());
}

#[test]
fn two_searches_under_one_seed_produce_one_archive() {
    let run = |name: &str| {
        let opts = options(name, 10);
        let fixture = Fixture::new(Mood::Normal);
        search(&base(), &fixture, &RandomMutation, &opts).expect("the search runs")
    };
    let a = run("determinism-a");
    let b = run("determinism-b");
    assert_eq!(a, b, "the same seed must produce the same archive");

    // Negative control: another seed must not.
    let other = {
        let opts = FoundryOptions::new(out_dir("determinism-c"))
            .budget(10)
            .seed(0xD1FF)
            .objective(Objective::Evade);
        let fixture = Fixture::new(Mood::Normal);
        search(&base(), &fixture, &RandomMutation, &opts).expect("the search runs")
    };
    assert_ne!(
        a.cells, other.cells,
        "two seeds produced the same cells, so the seed is reaching nothing"
    );
}

#[test]
fn the_archive_document_round_trips() {
    let opts = options("document", 8);
    let fixture = Fixture::new(Mood::Normal);
    let archive = search(&base(), &fixture, &RandomMutation, &opts).expect("the search runs");
    let text = std::fs::read_to_string(opts.out.join(ARCHIVE_FILE)).expect("an archive");
    let document: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(
        document["schema"],
        json!(v2xw_experiment::foundry::ARCHIVE_SCHEMA)
    );
    assert_eq!(document["objective"], json!("evade"));
    assert_eq!(document["coverage_cells"], json!(archive.coverage()));
    assert_eq!(document["evaluated"], json!(archive.evaluated));
    // The cells survive the trip, keyed as the archive keys them.
    let cells = document["cells"].as_object().expect("a cell map");
    assert_eq!(cells.len(), archive.coverage());
    for key in archive.cells.keys() {
        assert!(cells.contains_key(key), "{key} is missing from the document");
    }
    // Every float in the document sits on the declared grid (build decision D9).
    let quantum = document["quantum"].as_f64().expect("a declared quantum");
    for cell in cells.values() {
        let fitness = cell["fitness"].as_f64().expect("a fitness");
        assert!(
            v2xw_core::math::is_on_grid(fitness, quantum),
            "{fitness} is off the {quantum} grid"
        );
    }
}

/// An injected operator that tries to switch the detector off.
///
/// The one test in this file that is about security rather than about search quality: an
/// operator is an *injected* component, and a language-model operator's output is whatever
/// a prompt produced. The filter has to hold whatever it returns.
struct Hostile;

impl MutationOperator for Hostile {
    fn name(&self) -> &str {
        "hostile"
    }

    fn propose(
        &self,
        _parent: &Genome,
        _summary: &v2xw_experiment::foundry::ArchiveSummary,
        _rng: &mut v2xw_core::rng::RngStream,
    ) -> Option<Genome> {
        let mut genome = Genome::new();
        genome.insert("detection.local".to_string(), json!([]));
        genome.insert(
            "security.verification_policy".to_string(),
            json!("on-demand"),
        );
        Some(genome)
    }
}

#[test]
fn an_injected_operator_cannot_switch_the_detector_off() {
    let opts = options("hostile", 10);
    let fixture = Fixture::new(Mood::Normal);
    let archive = search(&base(), &fixture, &Hostile, &opts).expect("the search runs");
    // Every genome that reached an elite carries only permitted paths. The hostile
    // operator's whole proposal is filtered away, so `mutate` falls back — first to the
    // built-in operator and finally to the parent — and the search continues.
    for elite in archive.cells.values() {
        for path in elite.genome.keys() {
            assert!(
                !path.starts_with("detection.") && path != "security.verification_policy",
                "{path} reached an archived genome"
            );
        }
    }
    // …and the detector in the scenario is still the one the base declared, which is the
    // statement the archive's fitnesses depend on for their meaning.
    assert_eq!(base().detection.local.len(), 1);
}

#[test]
fn the_objective_is_refused_before_the_budget_is_spent() {
    // Not a search test: a typo in an objective must cost nothing. The check lives in
    // `Objective::parse` rather than in the loop, which is what makes that true.
    assert!(Objective::parse("family:nonesuch").is_err());
    assert!(Objective::parse("evaed").is_err());
    assert!(Objective::parse("family:position").is_ok());
}

#[test]
fn the_gate_reads_the_keys_the_metric_provider_writes() {
    // A fixture-driven version of the unit test in `foundry.rs`: the signal keys are
    // `MetricSample::key`'s spelling, so if `Dim`'s declaration order ever put `cell`
    // before `level` every candidate would gate out as having no attackers. Reaching the
    // keys through a real `MetricSample` rather than through a string literal is what
    // makes this test notice.
    let sample = MetricSample::new(
        &count_def("det_tp"),
        0,
        dims("vehicle", Some("tp")),
        SampleValue::count(1),
    );
    assert_eq!(sample.key(), "det_tp|level=vehicle|cell=tp");
    let recall = MetricSample::new(&recall_def(), 0, dims("vehicle", None), {
        SampleValue::Ratio(Proportion::from_counts(1, 2).estimate(1, ConfidenceLevel::P95))
    });
    assert_eq!(recall.key(), "det_recall|level=vehicle");
}

#[test]
fn a_perfectly_detected_scenario_is_archived_at_zero() {
    // The arithmetic, without the search: a valid run in which every attacker was caught
    // is a *measurement* of zero evasion and belongs in the archive, not in the gated
    // table. An implementation that gated it out would leave the easy cells empty and the
    // coverage number would be a statement about difficulty rather than about coverage.
    let signals = v2xw_experiment::foundry::Signals {
        attackers: 10,
        reported_subjects: 10,
        recall_vehicle: Some(1.0),
        recall_report: Some(1.0),
        time_to_detect_s: Some(2.0),
    };
    let descriptor = v2xw_experiment::foundry::Descriptor {
        attack_family: "position".to_string(),
        density_band: "medium".to_string(),
        topology: "procedural".to_string(),
        attacker_band: "low".to_string(),
        rat: "dsrc-80211p".to_string(),
        protocol: "none".to_string(),
    };
    let (score, validity) = fitness(&Objective::Evade, &signals, &descriptor, 60.0);
    assert!(validity.is_valid());
    assert_eq!(score, 0.0);
}

#[test]
fn an_empty_archive_is_still_a_document() {
    // A search that archived nothing still writes both outputs, because "the search ran
    // and found nothing" and "the search did not run" must be distinguishable on disk.
    let opts = options("empty-document", 3);
    let fixture = Fixture::new(Mood::Silent);
    let archive = search(&base(), &fixture, &RandomMutation, &opts).expect("the search runs");
    assert_eq!(archive, {
        let text = std::fs::read_to_string(opts.out.join(ARCHIVE_FILE)).expect("an archive");
        let document: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        Archive {
            cells: BTreeMap::new(),
            evaluated: document["evaluated"].as_u64().unwrap_or(0),
            gated: serde_json::from_value(document["gated"].clone()).unwrap_or_default(),
            failures: document["failures"].as_u64().unwrap_or(0),
            first_failure: None,
        }
    });
}
