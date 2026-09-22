//! Scenario authoring: produce a draft, then run it through the engine's own loader.
//!
//! The copilot does not get to decide whether a scenario is valid. It writes a draft and
//! hands it to [`v2xw_engine::scenario::validate`] — the same function
//! [`v2xw_engine::Scenario::load`] calls — and reports what comes back. That is the whole
//! design: a second validator in this crate would be a second opinion, and the engine's
//! opinion is the only one that decides whether a run starts.
//!
//! Two deliberate differences from [`v2xw_engine::Scenario::load`]:
//!
//! * **Every error, not the first.** `Scenario::validate` is the pass-or-fail spelling and
//!   stops at the first conflict; `validate` returns them all, and an author fixing a
//!   draft wants them all. So the stages are driven here rather than through `load`.
//! * **Nothing is written and nothing is read from disk.** [`check_draft`] takes text. A
//!   draft becomes a file when a human asks the server to save it through the
//!   `scenario.save` method, which is a mutating tool. The copilot cannot put a file on
//!   disk by itself.
//!
//! `meta.base` is the one thing this cannot resolve, because a base is resolved relative
//! to the including *file* and a draft has no path. A draft that names one is checked
//! without it and the report says so, rather than being checked against a silently empty
//! base.

use serde::Serialize;
use serde_json::Value;
use v2xw_engine::scenario::{Chain, Scenario, validate};
use v2xw_engine::{EngineError, ScenarioError};

/// Which stage a draft got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// The text is not YAML or JSON.
    Parse,
    /// The `schema` key is missing, unknown, or the migrator chain stops short.
    Schema,
    /// The document does not fit the scenario struct: a wrong type, an unknown key.
    Deserialise,
    /// The document loads; the cross-field rules had their say.
    Validate,
}

/// One thing wrong with a draft, in the author's terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DraftError {
    /// The dotted field path, e.g. `radio.tiers.phy`, when the error is about one field.
    pub field: Option<String>,
    /// What is wrong, as the engine says it.
    pub message: String,
}

/// What came back from running a draft through the loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DraftReport {
    /// True only when the engine would load this scenario.
    pub ok: bool,
    /// How far it got.
    pub stage: Stage,
    /// Everything wrong with it, in schema order. Empty when `ok`.
    pub errors: Vec<DraftError>,
    /// Things the author should know that are not errors.
    pub warnings: Vec<String>,
    /// The normalised YAML, when the draft loads — what to save.
    pub yaml: Option<String>,
    /// The scenario hash the manifest would pin, when the draft loads.
    pub content_hash: Option<String>,
}

impl DraftReport {
    /// A report that stopped at one stage with one error.
    fn stopped(stage: Stage, warnings: Vec<String>, error: DraftError) -> Self {
        DraftReport {
            ok: false,
            stage,
            errors: vec![error],
            warnings,
            yaml: None,
            content_hash: None,
        }
    }
}

/// Runs a draft through parse, migration, deserialisation and validation.
///
/// Accepts YAML or JSON, through the loader's own parser, so this cannot disagree with the
/// engine about what a draft says.
#[must_use]
pub fn check_draft(text: &str) -> DraftReport {
    match serde_yml::from_str::<Value>(text) {
        Ok(doc) => check_document(doc),
        Err(e) => DraftReport::stopped(
            Stage::Parse,
            Vec::new(),
            DraftError {
                field: None,
                message: format!(
                    "the draft is not YAML or JSON: {e}. A scenario is one document with a \
                     top-level `schema: {}` key.",
                    v2xw_engine::scenario::CURRENT_SCHEMA
                ),
            },
        ),
    }
}

/// [`check_draft`] for a draft that is already a JSON document.
#[must_use]
pub fn check_document(mut doc: Value) -> DraftReport {
    let mut warnings = Vec::new();
    if doc
        .get("meta")
        .and_then(|m| m.get("base"))
        .is_some_and(|b| !b.is_null())
    {
        warnings.push(
            "meta.base names a base scenario, which is resolved relative to the including \
             file and so cannot be resolved for a draft that is not on disk yet. Everything \
             below was checked without it: save the draft and load it to check the merge."
                .to_string(),
        );
    }

    if let Err(e) = Chain::shipped().migrate(&mut doc) {
        return DraftReport::stopped(Stage::Schema, warnings, scenario_error(&e));
    }

    let scenario = match Scenario::from_document(doc) {
        Ok(s) => s,
        Err(e) => {
            return DraftReport::stopped(
                Stage::Deserialise,
                warnings,
                DraftError {
                    field: engine_error_field(&e),
                    message: e.to_string(),
                },
            );
        }
    };

    let errors: Vec<DraftError> = validate(&scenario).iter().map(scenario_error).collect();
    if !errors.is_empty() {
        return DraftReport {
            ok: false,
            stage: Stage::Validate,
            errors,
            warnings,
            yaml: None,
            content_hash: None,
        };
    }

    let yaml = scenario.to_yaml().ok();
    let content_hash = scenario.content_hash().ok();
    if yaml.is_none() || content_hash.is_none() {
        warnings.push(
            "the scenario validates but would not serialise, which should not happen; save it \
             through the server rather than from this report"
                .to_string(),
        );
    }
    DraftReport {
        ok: true,
        stage: Stage::Validate,
        errors: Vec::new(),
        warnings,
        yaml,
        content_hash,
    }
}

/// The starting point a copilot edits: the minimal scenario the engine ships.
///
/// Offered rather than invented, so a draft begins from something that loads.
#[must_use]
pub fn starting_point() -> Scenario {
    Scenario::minimal()
}

/// A [`ScenarioError`] as a report row.
fn scenario_error(e: &ScenarioError) -> DraftError {
    DraftError {
        field: e.field().map(str::to_string),
        message: e.to_string(),
    }
}

/// The field an engine error is about, when it is about one.
fn engine_error_field(e: &EngineError) -> Option<String> {
    match e {
        EngineError::Scenario(s) => s.field().map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checker_returns_the_same_verdict_as_the_engine() {
        // The point of the module: no second opinion. Whatever `Scenario::validate` says
        // about the engine's own starting point, this says too, through the file form.
        let scenario = starting_point();
        let engine_accepts = scenario.validate().is_ok();
        let yaml = scenario.to_yaml().expect("the minimal scenario serialises");
        let report = check_draft(&yaml);
        assert_eq!(
            report.ok, engine_accepts,
            "the copilot and the engine disagree about a scenario: {report:?}"
        );
        if report.ok {
            assert!(report.errors.is_empty());
            assert_eq!(report.content_hash.as_ref().map(String::len), Some(64));
        }
    }

    #[test]
    fn a_broken_draft_is_reported_with_the_field_it_is_about() {
        let mut scenario = starting_point();
        scenario.time.duration_s = -5.0;
        scenario.time.mobility_step_ms = 5;
        let yaml = scenario.to_yaml().expect("serialises");
        let report = check_draft(&yaml);
        assert!(!report.ok, "a negative duration was accepted");
        let fields: Vec<&str> = report
            .errors
            .iter()
            .filter_map(|e| e.field.as_deref())
            .collect();
        assert!(fields.contains(&"time.duration_s"), "{report:?}");
        assert!(
            fields.contains(&"time.mobility_step_ms"),
            "only the first error was reported: {report:?}"
        );
        assert_eq!(report.stage, Stage::Validate);
    }

    #[test]
    fn a_draft_that_is_not_a_document_stops_at_parse() {
        let report = check_draft("schema: [unclosed");
        assert!(!report.ok);
        assert_eq!(report.stage, Stage::Parse);
        assert_eq!(report.errors.len(), 1);
    }

    #[test]
    fn an_unknown_schema_version_stops_at_the_migrator() {
        let report = check_draft("schema: v2xw/scenario/99\n");
        assert!(!report.ok);
        assert_eq!(report.stage, Stage::Schema);
        assert_eq!(report.errors[0].field.as_deref(), Some("schema"));
    }

    #[test]
    fn a_base_reference_is_warned_about_rather_than_silently_ignored() {
        let mut doc: Value = serde_json::to_value(starting_point()).expect("serialises");
        doc["meta"]["base"] = Value::String("presets/urban.yaml".to_string());
        let report = check_document(doc);
        assert!(
            report.warnings.iter().any(|w| w.contains("meta.base")),
            "a draft with a base was checked without saying so"
        );
    }
}
