//! The model-card completeness gate, against the register this repository actually ships.
//!
//! `src/gate.rs`'s own tests cover the arithmetic and the refusals with synthetic cards.
//! These tests cover the thing a unit test cannot: that `docs/calibration/issues.json` —
//! the file a release check reads — parses, declares the schema this build understands, and
//! contains no coverage pattern that the gate would reject.
//!
//! That last one is the point. A typo in a coverage pattern makes the gate reject the
//! register *and* leave the parameters it was meant to cover uncovered, so the failure is
//! loud. It should nonetheless be caught here, at `cargo test`, rather than during a
//! release.
//!
//! The register is read through `CARGO_MANIFEST_DIR` rather than the working directory, so
//! the test does not depend on where `cargo test` was invoked from.

use std::path::PathBuf;

use v2xw_core::card::{Family, ModelCard, Parameter, Source, Tier};
use v2xw_core::registry::Registry;
use v2xw_metrics::gate::{self, ISSUE_REGISTER_SCHEMA, IssueRegister, IssueState};

/// `<repo>/docs/calibration/issues.json`.
fn register_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("calibration")
        .join("issues.json")
}

fn shipped_register() -> IssueRegister {
    let path = register_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the shipped register at {} is readable: {e}",
            path.display()
        )
    });
    IssueRegister::from_json(&text).unwrap_or_else(|e| panic!("the shipped register parses: {e}"))
}

#[test]
fn the_shipped_register_parses_and_declares_the_schema_this_build_reads() {
    let register = shipped_register();
    assert_eq!(
        register.schema, ISSUE_REGISTER_SCHEMA,
        "the shipped register must declare the schema this build reads"
    );
}

/// Every coverage pattern in the shipped register is well formed.
///
/// Run over an empty registry, so the only thing that can fail is the register itself.
#[test]
fn no_pattern_in_the_shipped_register_is_malformed() {
    let register = shipped_register();
    let report = gate::run(&Registry::new(), &register);
    assert!(
        report.malformed_patterns.is_empty(),
        "malformed coverage pattern(s) in docs/calibration/issues.json: {:#?}",
        report.malformed_patterns
    );
}

/// Every issue in the shipped register names an owner and a measurement.
///
/// The gate does not enforce this — it checks coverage, not the quality of an issue — so
/// the register's own review does, here. An issue with no owner is the card's calibration
/// plan wearing an id, and the whole reason the gate exists is that a plan with no owner
/// has never once been executed.
#[test]
fn every_shipped_issue_names_an_owner_and_the_measurement_that_would_close_it() {
    for issue in &shipped_register().issues {
        assert!(
            !issue.owner.trim().is_empty(),
            "{}: an issue with no owner is a plan with an id",
            issue.id
        );
        assert!(
            !issue.measurement.trim().is_empty(),
            "{}: an issue must say what measurement would close it",
            issue.id
        );
        assert!(
            !issue.id.trim().is_empty() && !issue.title.trim().is_empty(),
            "an issue needs an id and a title"
        );
        if issue.state == IssueState::Blocked {
            assert!(
                issue
                    .blocked_by
                    .as_ref()
                    .is_some_and(|b| !b.trim().is_empty()),
                "{}: a blocked issue must say what is blocking it",
                issue.id
            );
        }
    }
}

/// The gate over the shipped register and a registry with one uncalibrated `high`-tier
/// default: it must be red.
///
/// A test of the *register*, not of the gate's logic: the register ships empty, so nothing
/// in it can cover an arbitrary model, and the assertion is that emptiness has the
/// consequence it is supposed to have. If somebody ever adds a pattern broad enough to
/// cover a model it was never written for, this goes green and says so.
#[test]
fn the_shipped_register_covers_nothing_it_was_not_written_for() {
    let mut card = ModelCard::new(
        "metric/test/not-in-the-register",
        Family::Metric,
        "1.0.0",
        "A model nobody has opened a calibration issue for.",
    );
    card.tier = vec![Tier::High];
    let mut parameter = Parameter::new(
        "threshold",
        "-",
        serde_json::json!(0.5),
        Source::todo_calibrate("an implementer's guess"),
    );
    parameter.calibration = Some("measure it on a test track".to_string());
    card.parameters = vec![parameter];

    let mut registry = Registry::new();
    registry.register(card).expect("the card registers");
    let report = gate::run(&registry, &shipped_register());
    assert!(!report.passed(), "{}", report.summary());
    assert_eq!(report.high_tier_todo_parameters, 1);
    assert_eq!(report.covered, 0);
    assert!(report.summary().starts_with("FAIL"));
}

/// The gate's report is a function of its two inputs and of nothing else.
#[test]
fn two_runs_over_one_registry_produce_the_same_report() {
    let mut registry = Registry::new();
    for id in ["b/second", "a/first", "c/third"] {
        let mut card = ModelCard::new(id, Family::Metric, "1.0.0", "A card.");
        card.tier = vec![Tier::High];
        let mut parameter = Parameter::new(
            "x",
            "-",
            serde_json::json!(1.0),
            Source::todo_calibrate("a guess"),
        );
        parameter.calibration = Some("a plan".to_string());
        card.parameters = vec![parameter];
        registry.register(card).expect("registers");
    }
    let register = shipped_register();
    let a = gate::run(&registry, &register);
    let b = gate::run(&registry, &register);
    assert_eq!(a, b);
    // Registered out of id order; reported in id order, because the report goes into a
    // file and a file's contents must not depend on load order.
    let ids: Vec<&str> = a.failures.iter().map(|f| f.model.as_str()).collect();
    assert_eq!(ids, vec!["a/first", "b/second", "c/third"]);
}
