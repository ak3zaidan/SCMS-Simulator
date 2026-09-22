//! What the plan promises: the same block, the same runs, the same seeds.
//!
//! # Each check is shown to be capable of failing
//!
//! This project has shipped four checks that could not fail. Every positive assertion here
//! is therefore paired with a **negative control** — a perturbed input whose result must
//! *differ* — so that a digest wired to a constant, an expansion that ignored its sweep or
//! a derivation that ignored its seed would turn the test red rather than green.

use std::collections::BTreeMap;

use serde_json::json;
use v2xw_engine::Scenario;
use v2xw_engine::scenario::Experiment;
use v2xw_experiment::plan::{ExperimentPlan, derive_run_seed};

/// A scenario with the given sweep on it. `time.duration_s` is a real field of the schema,
/// so a sweep over it is one the validator accepts.
fn swept(
    sweep: Vec<(&str, Vec<serde_json::Value>)>,
    seeds: Vec<u64>,
    replications: u32,
) -> Scenario {
    let mut scenario = Scenario::minimal();
    let mut map: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for (path, values) in sweep {
        map.insert(path.to_string(), values);
    }
    scenario.experiment = Some(Experiment {
        sweep: map,
        seeds,
        replications,
    });
    scenario
}

#[test]
fn the_same_block_expands_to_the_same_runs() {
    let scenario = swept(
        vec![("time.duration_s", vec![json!(10.0), json!(20.0)])],
        vec![1, 2],
        3,
    );
    let first = ExperimentPlan::expand(&scenario).expect("the block expands");
    let second = ExperimentPlan::expand(&scenario).expect("the block expands");
    assert_eq!(first.runs, second.runs);
    assert_eq!(first.digest, second.digest);
    assert_eq!(
        first.runs.len(),
        2 * 2 * 3,
        "two points, two slots, three reps"
    );

    // Negative control: one more swept value has to move the digest. If the digest ignored
    // its input — the defect this project has shipped four times — this assertion fails.
    let wider = swept(
        vec![(
            "time.duration_s",
            vec![json!(10.0), json!(20.0), json!(30.0)],
        )],
        vec![1, 2],
        3,
    );
    let wider = ExperimentPlan::expand(&wider).expect("the block expands");
    assert_ne!(first.digest, wider.digest);
}

#[test]
fn the_last_axis_varies_fastest() {
    let scenario = swept(
        vec![
            (
                "actors.vehicles.equipped_fraction",
                vec![json!(0.5), json!(1.0)],
            ),
            (
                "time.duration_s",
                vec![json!(10.0), json!(20.0), json!(30.0)],
            ),
        ],
        vec![],
        1,
    );
    let plan = ExperimentPlan::expand(&scenario).expect("the block expands");
    assert_eq!(plan.cells.len(), 6);
    // Axes are in path order, so `actors…` is axis 0 and `time…` is axis 1; the last axis
    // takes the lowest-order digit.
    let duration_of = |i: usize| plan.cells[i].values["time.duration_s"].clone();
    let fraction_of = |i: usize| plan.cells[i].values["actors.vehicles.equipped_fraction"].clone();
    assert_eq!(duration_of(0), json!(10.0));
    assert_eq!(duration_of(1), json!(20.0));
    assert_eq!(duration_of(2), json!(30.0));
    assert_eq!(duration_of(3), json!(10.0), "the fast axis wraps");
    assert_eq!(fraction_of(0), json!(0.5));
    assert_eq!(fraction_of(3), json!(1.0), "the slow axis advances once");
}

#[test]
fn a_run_seed_is_derived_and_never_the_declared_one() {
    let scenario = swept(vec![], vec![1, 2], 2);
    let plan = ExperimentPlan::expand(&scenario).expect("the block expands");
    assert_eq!(plan.runs.len(), 4);
    for run in &plan.runs {
        assert_ne!(
            run.seed, run.declared_seed,
            "a declared seed is an ingredient, not the seed the engine runs under"
        );
    }
    // Four runs, four distinct seeds.
    let mut seeds: Vec<u64> = plan.runs.iter().map(|r| r.seed).collect();
    seeds.sort_unstable();
    seeds.dedup();
    assert_eq!(seeds.len(), 4);

    // Negative control: the derivation has to depend on the master seed. If it did not,
    // two scenarios with different master seeds would produce identical replications and
    // every "independent replication" in this simulator would be the same run.
    let other = derive_run_seed(0x1234, 0, 1, 0);
    let same = derive_run_seed(0x1234, 0, 1, 0);
    let moved = derive_run_seed(0x1235, 0, 1, 0);
    assert_eq!(other, same, "the derivation is a function");
    assert_ne!(other, moved, "the derivation reads the master seed");
    assert_ne!(other, derive_run_seed(0x1234, 1, 1, 0), "it reads the slot");
    assert_ne!(
        other,
        derive_run_seed(0x1234, 0, 1, 1),
        "it reads the replication"
    );
}

#[test]
fn every_cell_uses_the_same_seed_for_the_same_slot() {
    // Common random numbers: comparing two sweep points at slot 0 compares two runs whose
    // streams came from the same seed, so the difference is not inflated by between-run
    // variance.
    let scenario = swept(
        vec![("time.duration_s", vec![json!(10.0), json!(20.0)])],
        vec![7],
        1,
    );
    let plan = ExperimentPlan::expand(&scenario).expect("the block expands");
    assert_eq!(plan.runs.len(), 2);
    assert_eq!(plan.runs[0].seed, plan.runs[1].seed);
    assert_ne!(plan.runs[0].cell.index, plan.runs[1].cell.index);
}

#[test]
fn materialising_a_run_writes_the_cell_value_and_the_seed() {
    let scenario = swept(
        vec![("time.duration_s", vec![json!(11.0), json!(22.0)])],
        vec![],
        1,
    );
    let plan = ExperimentPlan::expand(&scenario).expect("the block expands");
    let first = plan
        .materialise(&scenario, &plan.runs[0])
        .expect("cell 0 is a valid scenario");
    assert_eq!(first.time.duration_s, 11.0);
    assert_eq!(first.seed, plan.runs[0].seed);
    assert!(
        first.experiment.is_none(),
        "a run's scenario is an ordinary scenario, so its hash is an ordinary hash"
    );

    // Negative control: the second cell must differ. If `materialise` ignored the cell,
    // every run of the sweep would execute the base scenario and the sweep would measure
    // nothing.
    let second = plan
        .materialise(&scenario, &plan.runs[1])
        .expect("cell 1 is a valid scenario");
    assert_eq!(second.time.duration_s, 22.0);
    assert_ne!(
        first.content_hash().expect("hashes"),
        second.content_hash().expect("hashes")
    );
}

#[test]
fn an_axis_with_no_values_is_refused_by_name() {
    let scenario = swept(vec![("time.duration_s", vec![])], vec![], 1);
    let error = ExperimentPlan::expand(&scenario).expect_err("an empty axis is refused");
    assert!(
        format!("{error}").contains("time.duration_s"),
        "the error names the axis: {error}"
    );

    // Negative control: the same axis with a value is accepted, so the refusal is about
    // the emptiness and not about sweeping `time.duration_s`.
    let ok = swept(vec![("time.duration_s", vec![json!(5.0)])], vec![], 1);
    assert!(ExperimentPlan::expand(&ok).is_ok());
}

#[test]
fn a_scenario_without_an_experiment_block_says_so() {
    let scenario = Scenario::minimal();
    let error = ExperimentPlan::expand(&scenario).expect_err("there is no sweep");
    assert!(format!("{error}").contains("experiment"), "{error}");
}

#[test]
fn a_block_with_no_sweep_is_replications_of_one_configuration() {
    let scenario = swept(vec![], vec![], 5);
    let plan = ExperimentPlan::expand(&scenario).expect("the block expands");
    assert_eq!(plan.cells.len(), 1, "no sweep is one cell, not zero");
    assert_eq!(plan.runs.len(), 5);
    assert_eq!(plan.seed_slots, vec![scenario.seed]);
}
