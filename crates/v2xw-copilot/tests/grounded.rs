//! The catalogue this build actually has, and the four jobs run against it.
//!
//! These are the tests that would catch the copilot answering from somewhere other than
//! the registry: they assert on the real cards and the real metric definitions, so a model
//! whose card loses its sources, or a metric that loses its formula, fails here.

use serde_json::json;
use v2xw_copilot::grounding::Grounding;
use v2xw_copilot::provider::ScriptedProvider;
use v2xw_copilot::scenario::{check_draft, starting_point};
use v2xw_copilot::session::{Copilot, Policy, ToolOutcome};
use v2xw_copilot::tools::ToolSurface;
use v2xw_copilot::transport::NoEngine;
use v2xw_copilot::{ChatMessage, Completion, ToolCall};

fn built_in() -> Grounding {
    Grounding::builtin().expect("this build registers its own models")
}

#[test]
fn the_catalogue_is_not_empty_and_every_metric_states_its_formula() {
    let g = built_in();
    assert!(g.model_count() > 0, "no model card was registered");
    assert!(g.metric_count() > 0, "no metric definition was published");
    for name in g.metric_names() {
        let answer = g.metric(name).expect("a listed metric resolves");
        assert!(
            !answer.definition.definition_md.trim().is_empty(),
            "metric `{name}` has no definition, so no answer about it could be checked"
        );
        assert!(
            !answer.definition.unit.trim().is_empty(),
            "metric `{name}` has no unit"
        );
        assert!(
            !answer.visibility.is_empty(),
            "metric `{name}` has no visibility tag"
        );
    }
}

#[test]
fn every_uncited_default_carries_a_calibration_plan() {
    // Registry rule R1, seen from the copilot's side: these are the numbers a reply is
    // required to flag rather than present as established.
    let g = built_in();
    for (model, parameter) in g.uncalibrated() {
        let answer = g
            .parameter(&model, &parameter)
            .expect("the parameter came from the same card");
        assert!(answer.uncalibrated);
        assert!(
            answer.calibration.is_some(),
            "{model}.{parameter} is uncited and has no calibration plan"
        );
    }
}

#[test]
fn a_parameter_that_does_not_exist_is_never_answered_with_a_number() {
    let g = built_in();
    let Some(model) = g.model_ids().first().copied().map(str::to_string) else {
        panic!("the registry is empty");
    };
    let miss = g
        .parameter(&model, "a_parameter_no_card_declares")
        .expect_err("it does not exist");
    assert!(!miss.known);
    assert!(miss.why.contains("does not declare"));
}

#[test]
fn the_tool_surface_covers_the_whole_control_plane() {
    let surface = ToolSurface::new().expect("the server's document translates");
    // Everything 09-ui.md §8 names as reachable by the copilot.
    for method in [
        "run.start",
        "run.seek",
        "scenario.validate",
        "world.import_osm",
        "events.set",
        "metrics.query",
        "metrics.plot",
        "explain",
        "inspect.node",
        "experiment.run",
        "export.dataset",
    ] {
        let name = method.replace('.', "__");
        assert!(
            surface.get(&name).is_some(),
            "{method} is not reachable through the copilot"
        );
    }
}

#[test]
fn a_drafted_scenario_is_reported_on_rather_than_declared_good() {
    // A draft that is wrong in a way the validator names.
    let mut document = serde_json::to_value(starting_point()).expect("serialises");
    document["time"]["duration_s"] = json!(-5.0);
    let report = v2xw_copilot::scenario::check_document(document);
    assert!(!report.ok, "a negative duration was accepted");
    assert!(!report.errors.is_empty());

    // And the checker agrees with the engine about the engine's own starting point.
    let scenario = starting_point();
    let engine_accepts = scenario.validate().is_ok();
    let good = scenario.to_yaml().expect("serialises");
    assert_eq!(check_draft(&good).ok, engine_accepts);
}

#[test]
fn a_whole_turn_answers_a_metric_question_from_the_definition() {
    let g = built_in();
    let metric = g
        .metric_names()
        .first()
        .copied()
        .expect("this build publishes metrics")
        .to_string();
    let provider = ScriptedProvider::new(vec![
        Completion {
            message: ChatMessage::calls(
                None,
                vec![ToolCall {
                    id: "c1".to_string(),
                    name: "registry__metric".to_string(),
                    arguments: json!({"name": metric}).to_string(),
                }],
            ),
            finish_reason: "tool_calls".to_string(),
        },
        Completion {
            message: ChatMessage::assistant("definition and citation"),
            finish_reason: "stop".to_string(),
        },
    ]);
    let mut copilot = Copilot::new(
        provider,
        NoEngine,
        g,
        ToolSurface::new().expect("translates"),
        Policy::read_only(),
    );
    let turn = copilot
        .ask(&mut Vec::new(), "what does that metric measure?")
        .expect("two scripted completions");
    assert_eq!(turn.steps.len(), 1);
    match &turn.steps[0].outcome {
        ToolOutcome::Ok { result } => {
            assert_eq!(result["known"], json!(true));
            assert!(result["definition"]["definition_md"].is_string());
        }
        other => panic!("the definition lookup should have succeeded: {other:?}"),
    }
}

#[test]
fn a_run_question_with_no_engine_says_there_is_no_engine() {
    let provider = ScriptedProvider::new(vec![
        Completion {
            message: ChatMessage::calls(
                None,
                vec![ToolCall {
                    id: "c1".to_string(),
                    name: "run__status".to_string(),
                    arguments: "{}".to_string(),
                }],
            ),
            finish_reason: "tool_calls".to_string(),
        },
        Completion {
            message: ChatMessage::assistant("there is no run attached"),
            finish_reason: "stop".to_string(),
        },
    ]);
    let mut copilot = Copilot::new(
        provider,
        NoEngine,
        built_in(),
        ToolSurface::new().expect("translates"),
        Policy::read_only(),
    );
    let turn = copilot
        .ask(&mut Vec::new(), "is it running?")
        .expect("scripted");
    match &turn.steps[0].outcome {
        ToolOutcome::Failed { message } => assert!(message.contains("no engine is attached")),
        other => panic!("it must say there is no engine: {other:?}"),
    }
}
