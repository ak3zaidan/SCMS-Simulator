//! The why tab: where a value came from.
//!
//! 09-ui.md §5 says the inspector's "why" tab "resolves each value's provenance id into
//! the model card, version, parameters and sources". The data for that already exists in
//! two places, and this module joins them:
//!
//! * the run's own provenance chain, which the server's `explain` method returns as
//!   `chain` — rows of `{model_id, model_version, param_set_id, family, card_url}`
//!   (vwp-v1 §6.5);
//! * the model cards and metric definitions, from [`crate::grounding`].
//!
//! The join is the answer: the chain says *which* model produced the value, and the card
//! says what that model computes, from which parameters, cited to what.
//!
//! # What it does when there is no chain
//!
//! It says so. Asked to explain a metric with no run attached, the result carries the
//! metric's definition, its grid, its citation and what it does not account for — all of
//! which are properties of the metric rather than of the run — and puts "which model
//! produced this value is not known without the run's provenance chain" in
//! [`Explanation::unknown`]. It does not pick a plausible model out of the registry. A
//! why tab that guesses the why is worse than no why tab.

use serde::Serialize;
use serde_json::Value;

use crate::grounding::{Citation, Grounding, ParameterAnswer};

/// One model in the chain behind a value, resolved into its card.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelExplanation {
    /// What to cite.
    pub citation: Citation,
    /// What the model is for.
    pub purpose: String,
    /// The equations it implements, as `(name, equation)`.
    pub equations: Vec<(String, String)>,
    /// Every parameter it reads, with its unit, default and source.
    pub parameters: Vec<ParameterAnswer>,
    /// What the model assumes.
    pub assumptions: Vec<String>,
    /// What it cannot represent.
    pub limitations: Vec<String>,
    /// What this tier leaves out relative to the next tier up.
    pub ignores: Vec<String>,
    /// Its sources, rendered.
    pub sources: Vec<String>,
    /// How far it has been validated.
    pub validation: String,
    /// The parameter set the run resolved, when the chain named one.
    pub param_set_id: Option<String>,
}

/// Where a value came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Explanation {
    /// The `ValueRef` that was asked about, verbatim.
    pub subject: Value,
    /// Its `kind`, e.g. `metric`, `node_field`.
    pub kind: String,
    /// Its `id`, when it has one.
    pub id: Option<String>,
    /// The metric's definition in Markdown, when the subject is a metric.
    pub definition_md: Option<String>,
    /// The unit, when known.
    pub unit: Option<String>,
    /// The grid the value is quantised onto at the writer, when known.
    pub quantum: Option<f64>,
    /// Below this many samples the metric reports insufficient rather than a value.
    pub min_samples: Option<u64>,
    /// What the metric does not account for, plus any caveats the run added.
    pub not_accounted: Vec<String>,
    /// The models behind the value, outermost first.
    pub models: Vec<ModelExplanation>,
    /// Everything about this value that is *not* known, stated rather than filled in.
    pub unknown: Vec<String>,
}

/// Resolves a value reference into its provenance.
///
/// `server_result` is the `explain` JSON-RPC method's result when a run is attached, and
/// `None` when there is not one. Nothing is invented in either case.
#[must_use]
pub fn explain(
    grounding: &Grounding,
    subject: &Value,
    server_result: Option<&Value>,
) -> Explanation {
    let kind = subject
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let id = subject
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut out = Explanation {
        subject: subject.clone(),
        kind: kind.clone(),
        id: id.clone(),
        definition_md: None,
        unit: None,
        quantum: None,
        min_samples: None,
        not_accounted: Vec::new(),
        models: Vec::new(),
        unknown: Vec::new(),
    };

    if kind.is_empty() {
        out.unknown.push(
            "the subject has no `kind`, so there is nothing to resolve; a value reference \
             looks like {\"kind\":\"metric\",\"id\":\"pdr\"}"
                .to_string(),
        );
    }

    if kind == "metric" {
        match id.as_deref() {
            Some(name) => match grounding.metric_def(name) {
                Some(def) => {
                    out.definition_md = Some(def.definition_md.clone());
                    out.unit = Some(def.unit.clone());
                    out.quantum = Some(def.quantum.get());
                    out.min_samples = Some(def.min_samples);
                    out.not_accounted = def.not_accounted.clone();
                    if def.source.is_none() {
                        out.unknown.push(format!(
                            "the definition of `{name}` carries no citation of its own"
                        ));
                    }
                }
                None => out.unknown.push(format!(
                    "no metric named `{name}` is published by this build, so its definition \
                     cannot be quoted"
                )),
            },
            None => out
                .unknown
                .push("the subject is a metric with no `id`".to_string()),
        }
    }

    match server_result {
        Some(result) => {
            if let Some(caveats) = result.get("caveats").and_then(Value::as_array) {
                for c in caveats {
                    if let Some(text) = c.as_str() {
                        if !out.not_accounted.iter().any(|n| n == text) {
                            out.not_accounted.push(text.to_string());
                        }
                    }
                }
            }
            let chain = result.get("chain").and_then(Value::as_array);
            match chain {
                Some(rows) if !rows.is_empty() => {
                    for row in rows {
                        match resolve_row(grounding, row) {
                            Ok(model) => out.models.push(model),
                            Err(message) => out.unknown.push(message),
                        }
                    }
                }
                _ => out.unknown.push(
                    "the run returned no provenance chain for this value, so which model \
                     produced it is not known"
                        .to_string(),
                ),
            }
        }
        None => out.unknown.push(
            "no run is attached, so the provenance chain — which model produced this value, \
             with which resolved parameters — is not known. What is above is the \
             definition, which is a property of the metric rather than of the run."
                .to_string(),
        ),
    }

    out
}

/// One provenance row, joined to its card.
fn resolve_row(
    grounding: &Grounding,
    row: &Value,
) -> core::result::Result<ModelExplanation, String> {
    let model_id = row
        .get("model_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "a provenance row has no `model_id`".to_string())?;
    let entry = grounding.model(model_id).ok_or_else(|| {
        format!(
            "the run names model `{model_id}`, which this build's registry does not have. \
             The run and this copilot are different builds; trust the run."
        )
    })?;
    let card = &entry.card;
    let parameters = card
        .parameters
        .iter()
        .filter_map(|p| grounding.parameter(model_id, &p.name).ok())
        .collect();
    Ok(ModelExplanation {
        citation: entry.citation.clone(),
        purpose: card.purpose.clone(),
        equations: card
            .equations
            .iter()
            .map(|e| (e.name.clone(), e.latex_or_text.clone()))
            .collect(),
        parameters,
        assumptions: card.assumptions.clone(),
        limitations: card.limitations.clone(),
        ignores: card.ignores.clone(),
        sources: card.sources.iter().map(crate::grounding::cite).collect(),
        validation: entry.validation.clone(),
        param_set_id: row
            .get("param_set_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use v2xw_core::card::{Equation, Family, ModelCard, Parameter, Source, SourceKind};
    use v2xw_core::registry::Registry;

    fn grounding_with_one_model() -> Grounding {
        let mut card = ModelCard::new(
            "radio/propagation/log-distance",
            Family::Propagation,
            "1.0.0",
            "log-distance path loss with log-normal shadowing",
        );
        card.equations.push(Equation::new(
            "path loss",
            "PL(d) = PL(d0) + 10 n log10(d / d0) + X",
        ));
        card.parameters.push(Parameter::new(
            "path_loss_exponent",
            "-",
            json!(2.75),
            Source::new(SourceKind::Paper, "doi:10.1109/TVT.2011.2158461"),
        ));
        card.sources
            .push(Source::new(SourceKind::Standard, "IEEE 802.11p"));
        let mut registry = Registry::new();
        registry.register(card).expect("valid");
        Grounding::new(&registry, Vec::new())
    }

    #[test]
    fn a_chain_row_resolves_to_its_card() {
        let g = grounding_with_one_model();
        let server = json!({
            "chain": [{"model_id": "radio/propagation/log-distance",
                       "model_version": "1.0.0",
                       "param_set_id": "ps3"}],
            "caveats": ["the focus region is smaller than the world"]
        });
        let e = explain(
            &g,
            &json!({"kind": "node_field", "id": "rssi"}),
            Some(&server),
        );
        assert_eq!(e.models.len(), 1);
        let m = &e.models[0];
        assert_eq!(m.citation.model_id, "radio/propagation/log-distance");
        assert_eq!(m.equations.len(), 1);
        assert_eq!(m.parameters.len(), 1);
        assert_eq!(m.parameters[0].source_ref, "doi:10.1109/TVT.2011.2158461");
        assert_eq!(m.param_set_id.as_deref(), Some("ps3"));
        assert!(
            e.not_accounted
                .contains(&"the focus region is smaller than the world".to_string())
        );
        assert!(
            e.unknown.is_empty(),
            "nothing should be missing: {:?}",
            e.unknown
        );
    }

    #[test]
    fn with_no_run_the_chain_is_declared_unknown_rather_than_guessed() {
        let g = grounding_with_one_model();
        let e = explain(&g, &json!({"kind": "metric", "id": "pdr"}), None);
        assert!(e.models.is_empty(), "a model was invented without a run");
        assert!(e.unknown.iter().any(|u| u.contains("no run is attached")));
    }

    #[test]
    fn a_model_the_registry_does_not_have_is_reported_rather_than_dropped() {
        let g = grounding_with_one_model();
        let server = json!({"chain": [{"model_id": "radio/fading/nakagami"}]});
        let e = explain(&g, &json!({"kind": "node_field"}), Some(&server));
        assert!(e.models.is_empty());
        assert!(
            e.unknown
                .iter()
                .any(|u| u.contains("radio/fading/nakagami"))
        );
    }
}
