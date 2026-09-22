//! Registry grounding: the only place an answer's facts may come from.
//!
//! The premise of this whole crate is that a language model knows nothing about this
//! simulator and must not be asked to. What it knows is in the model cards
//! (03-interfaces.md §12) and the metric definitions (08-measurement-and-data.md §1),
//! both of which are machine-readable and both of which carry citations. This module
//! turns them into lookups.
//!
//! # The rule that matters
//!
//! Every lookup that can miss returns [`NotKnown`] rather than a default, an empty value
//! or a guess. [`NotKnown`] says what was asked, why the answer is absent, and what *is*
//! declared, so the copilot's reply can be "this model's card does not declare a
//! `tx_power_dbm`; it declares `eirp_dbm`, `noise_figure_db` and `antenna_gain_dbi`". A
//! confidently wrong parameter value is the worst output this tool can produce, and the
//! type system is where that is prevented, not the prompt.
//!
//! Every hit carries a [`Citation`]: model id, version, family and the SHA-256 of the
//! card's canonical bytes — the same hash the run manifest pins (ADR 0007 §4). An answer
//! is therefore checkable against the artefact a run recorded.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use v2xw_core::card::{ModelCard, Parameter, Source, SourceKind};
use v2xw_core::registry::Registry;
use v2xw_metrics::{MetricDef, ProviderSet};

use crate::error::{CopilotError, Result};

/// What a grounded fact is cited to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Citation {
    /// The model's stable id.
    pub model_id: String,
    /// Its version.
    pub model_version: String,
    /// Its family.
    pub family: String,
    /// SHA-256 of the card's canonical bytes, hex — what the manifest pins.
    pub card_content_hash: String,
}

/// An answer that is not available, and what was looked at.
///
/// Serialised into a tool result verbatim. The `known` list is what the copilot should say
/// instead of inventing: the names that *do* exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotKnown {
    /// Always `false`, so a reader that only looks at one field still gets it right.
    pub known: bool,
    /// What was asked for.
    pub question: String,
    /// Why there is no answer.
    pub why: String,
    /// The names that do exist in the place that was looked.
    pub declared: Vec<String>,
}

impl NotKnown {
    /// A miss.
    #[must_use]
    pub fn new(question: impl Into<String>, why: impl Into<String>, declared: Vec<String>) -> Self {
        NotKnown {
            known: false,
            question: question.into(),
            why: why.into(),
            declared,
        }
    }
}

/// One registered model, as the copilot sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelEntry {
    /// What to cite.
    pub citation: Citation,
    /// The tiers the card declares.
    pub tiers: Vec<String>,
    /// The card's validation status.
    pub validation: String,
    /// One line of purpose.
    pub purpose: String,
    /// The whole card.
    pub card: ModelCard,
}

/// One parameter, answered from a card.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParameterAnswer {
    /// Always `true`; see [`NotKnown::known`].
    pub known: bool,
    /// What to cite.
    pub citation: Citation,
    /// The parameter's name, as the scenario spells it.
    pub name: String,
    /// Its unit; `-` for dimensionless.
    pub unit: String,
    /// The card's default.
    pub default: Value,
    /// The declared range, when the card declares one: two numbers are the inclusive
    /// interval, anything else is the set of allowed values.
    pub range: Option<Vec<Value>>,
    /// What kind of source the default is cited to.
    pub source_kind: String,
    /// The citation itself: a standard and clause, a DOI, a datasheet.
    pub source_ref: String,
    /// The calibration plan, which a `todo-calibrate` default is required to carry.
    pub calibration: Option<String>,
    /// True when the default is *not* cited to anything external and still needs
    /// calibration. A copilot quoting such a value must say so.
    pub uncalibrated: bool,
}

/// A metric, answered from its definition.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricAnswer {
    /// Always `true`.
    pub known: bool,
    /// The definition, verbatim.
    pub definition: MetricDef,
    /// The aggregation's short spelling, e.g. `ratio`, `p95`.
    pub agg: String,
    /// The visibility token: `GT`, `NODE`, `PUBLIC`, `MIXED`, `DERIVED` or `META`.
    pub visibility: String,
}

/// The catalogue the copilot answers from.
#[derive(Debug, Clone, PartialEq)]
pub struct Grounding {
    models: BTreeMap<String, ModelEntry>,
    metrics: BTreeMap<String, MetricDef>,
}

impl Grounding {
    /// Indexes a registry and a metric catalogue.
    ///
    /// Both are copied, so the grounding outlives the registry it was read from and can be
    /// shared by a UI that rebuilds its run.
    #[must_use]
    pub fn new(registry: &Registry, metrics: Vec<MetricDef>) -> Self {
        let mut models = BTreeMap::new();
        for (_, registered) in registry.iter_by_id() {
            let card = &registered.card;
            let entry = ModelEntry {
                citation: Citation {
                    model_id: card.id.clone(),
                    model_version: card.version.clone(),
                    family: card.family.to_string(),
                    card_content_hash: registered.content_hash_hex(),
                },
                tiers: card.tier.iter().map(ToString::to_string).collect(),
                validation: validation_name(card),
                purpose: card.purpose.clone(),
                card: card.clone(),
            };
            models.insert(card.id.clone(), entry);
        }
        let mut by_name = BTreeMap::new();
        for def in metrics {
            by_name.insert(def.name.clone(), def);
        }
        Grounding {
            models,
            metrics: by_name,
        }
    }

    /// The catalogue of everything this build has: every model the engine registers and
    /// every metric the five providers publish.
    ///
    /// # Errors
    /// [`CopilotError::Grounding`] if the engine's own registration fails, which means this
    /// build could not run a scenario either.
    pub fn builtin() -> Result<Self> {
        let mut registry = Registry::new();
        v2xw_engine::wiring::register_all(&mut registry)
            .map_err(|e| CopilotError::Grounding(e.to_string()))?;
        let mut providers = ProviderSet::new();
        // `t0` only sets where the first window starts; the catalogue does not depend on
        // it, and nothing here observes time.
        v2xw_metrics::register_all(&mut registry, &mut providers, 0)
            .map_err(|e| CopilotError::Grounding(e.to_string()))?;
        let metrics = providers.catalog();
        Ok(Grounding::new(&registry, metrics))
    }

    /// How many models are indexed.
    #[must_use]
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// How many metrics are indexed.
    #[must_use]
    pub fn metric_count(&self) -> usize {
        self.metrics.len()
    }

    /// Every model id, sorted.
    #[must_use]
    pub fn model_ids(&self) -> Vec<&str> {
        self.models.keys().map(String::as_str).collect()
    }

    /// Every metric name, sorted.
    #[must_use]
    pub fn metric_names(&self) -> Vec<&str> {
        self.metrics.keys().map(String::as_str).collect()
    }

    /// One model's entry.
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&ModelEntry> {
        self.models.get(id)
    }

    /// The models of one family, in id order.
    #[must_use]
    pub fn models_in_family(&self, family: &str) -> Vec<&ModelEntry> {
        self.models
            .values()
            .filter(|m| m.citation.family == family)
            .collect()
    }

    /// Every family that has at least one model, sorted.
    #[must_use]
    pub fn families(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .models
            .values()
            .map(|m| m.citation.family.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// A model card, or a miss naming the ids that do exist.
    ///
    /// # Errors
    /// [`NotKnown`] when no model has that id.
    pub fn card(&self, id: &str) -> core::result::Result<&ModelEntry, NotKnown> {
        self.models.get(id).ok_or_else(|| {
            NotKnown::new(
                format!("model card for {id:?}"),
                "no model with that id is registered in this build",
                self.models.keys().cloned().collect(),
            )
        })
    }

    /// One parameter of one model.
    ///
    /// # Errors
    /// [`NotKnown`] when the model is unknown, or when the model's card does not declare
    /// that parameter. The second case lists the parameter names the card *does* declare,
    /// which is what the copilot should offer instead of a number.
    pub fn parameter(
        &self,
        model_id: &str,
        name: &str,
    ) -> core::result::Result<ParameterAnswer, NotKnown> {
        let entry = self.card(model_id)?;
        let p: &Parameter = entry
            .card
            .parameters
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| {
                NotKnown::new(
                    format!("parameter {name:?} of {model_id:?}"),
                    "the model card does not declare a parameter with that name, and a \
                     parameter that is not on the card is not a parameter of this model \
                     (invariant I-C3)",
                    entry
                        .card
                        .parameters
                        .iter()
                        .map(|p| p.name.clone())
                        .collect(),
                )
            })?;
        Ok(ParameterAnswer {
            known: true,
            citation: entry.citation.clone(),
            name: p.name.clone(),
            unit: p.unit.clone(),
            default: p.default.clone(),
            range: p.range.clone(),
            source_kind: source_kind_name(p.source.kind),
            source_ref: p.source.reference.clone(),
            calibration: p.calibration.clone(),
            uncalibrated: p.source.kind == SourceKind::TodoCalibrate,
        })
    }

    /// One metric's definition.
    ///
    /// # Errors
    /// [`NotKnown`] when no metric has that name, listing the names that exist.
    pub fn metric(&self, name: &str) -> core::result::Result<MetricAnswer, NotKnown> {
        let def = self.metrics.get(name).ok_or_else(|| {
            NotKnown::new(
                format!("metric {name:?}"),
                "no metric with that name is published by this build's providers",
                self.metrics.keys().cloned().collect(),
            )
        })?;
        Ok(MetricAnswer {
            known: true,
            definition: def.clone(),
            agg: def.agg.tag(),
            visibility: v2xw_server::visibility_name(def.visibility).to_string(),
        })
    }

    /// The metric definition without the answer wrapper, for callers inside this crate.
    #[must_use]
    pub fn metric_def(&self, name: &str) -> Option<&MetricDef> {
        self.metrics.get(name)
    }

    /// Every parameter in the catalogue whose default is not cited to anything external.
    ///
    /// Rule R1's report, from the copilot's side: these are the numbers a reply must not
    /// present as established.
    #[must_use]
    pub fn uncalibrated(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for entry in self.models.values() {
            for p in &entry.card.parameters {
                if p.source.kind == SourceKind::TodoCalibrate {
                    out.push((entry.citation.model_id.clone(), p.name.clone()));
                }
            }
        }
        out
    }

    /// A compact catalogue for the system prompt: what exists, by name, and nothing else.
    ///
    /// Deliberately not the cards themselves. The copilot is told what it may ask about
    /// and is made to fetch the facts, because a card summarised into a prompt is a card
    /// the reply can drift from.
    #[must_use]
    pub fn index_text(&self) -> String {
        let mut out = String::new();
        out.push_str("MODELS (id — family), fetch the card before quoting anything from one:\n");
        for entry in self.models.values() {
            out.push_str("  ");
            out.push_str(&entry.citation.model_id);
            out.push_str(" — ");
            out.push_str(&entry.citation.family);
            out.push('\n');
        }
        out.push_str("METRICS (name — unit), fetch the definition before quoting one:\n");
        for def in self.metrics.values() {
            out.push_str("  ");
            out.push_str(&def.name);
            out.push_str(" — ");
            out.push_str(&def.unit);
            out.push('\n');
        }
        out
    }
}

/// The kebab-case spelling of a source kind, as the card schema writes it.
fn source_kind_name(kind: SourceKind) -> String {
    match kind {
        SourceKind::Standard => "standard".to_string(),
        SourceKind::Paper => "paper".to_string(),
        SourceKind::Datasheet => "datasheet".to_string(),
        SourceKind::Dataset => "dataset".to_string(),
        SourceKind::Code => "code".to_string(),
        SourceKind::TodoCalibrate => "todo-calibrate".to_string(),
    }
}

/// The kebab-case spelling of a card's validation status.
fn validation_name(card: &ModelCard) -> String {
    use v2xw_core::card::ValidationStatus as V;
    match card.validation.status {
        V::Unvalidated => "unvalidated".to_string(),
        V::UnitTested => "unit-tested".to_string(),
        V::LiteratureChecked => "literature-checked".to_string(),
        V::FieldChecked => "field-checked".to_string(),
    }
}

/// A source rendered for a reply: `standard: IEEE 1609.2 §5.3`.
#[must_use]
pub fn cite(source: &Source) -> String {
    format!("{}: {}", source_kind_name(source.kind), source.reference)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::card::Family;

    fn registry_with_one_card() -> Registry {
        let mut card = ModelCard::new(
            "radio/propagation/log-distance",
            Family::Propagation,
            "1.0.0",
            "log-distance path loss with shadowing",
        );
        card.parameters.push(Parameter::new(
            "path_loss_exponent",
            "-",
            serde_json::json!(2.75),
            Source::new(SourceKind::Paper, "doi:10.1109/TVT.2011.2158461"),
        ));
        let mut guess = Parameter::new(
            "shadowing_sigma_db",
            "dB",
            serde_json::json!(4.0),
            Source::todo_calibrate("no urban measurement to hand"),
        );
        guess.calibration = Some("fit against the Cologne trace".to_string());
        card.parameters.push(guess);
        let mut registry = Registry::new();
        registry.register(card).expect("a valid card registers");
        registry
    }

    #[test]
    fn a_declared_parameter_comes_back_cited() {
        let g = Grounding::new(&registry_with_one_card(), Vec::new());
        let a = g
            .parameter("radio/propagation/log-distance", "path_loss_exponent")
            .expect("declared");
        assert_eq!(a.unit, "-");
        assert_eq!(a.default, serde_json::json!(2.75));
        assert_eq!(a.source_kind, "paper");
        assert!(!a.uncalibrated);
        assert_eq!(a.citation.family, "propagation");
        assert_eq!(a.citation.card_content_hash.len(), 64);
    }

    #[test]
    fn an_undeclared_parameter_is_a_miss_that_names_the_real_ones() {
        let g = Grounding::new(&registry_with_one_card(), Vec::new());
        let miss = g
            .parameter("radio/propagation/log-distance", "tx_power_dbm")
            .expect_err("the card does not declare it");
        assert!(!miss.known);
        assert!(miss.declared.contains(&"path_loss_exponent".to_string()));
        assert!(!miss.declared.contains(&"tx_power_dbm".to_string()));
    }

    #[test]
    fn an_uncited_default_is_flagged() {
        let g = Grounding::new(&registry_with_one_card(), Vec::new());
        let a = g
            .parameter("radio/propagation/log-distance", "shadowing_sigma_db")
            .expect("declared");
        assert!(a.uncalibrated);
        assert_eq!(
            a.calibration.as_deref(),
            Some("fit against the Cologne trace")
        );
        assert_eq!(g.uncalibrated().len(), 1);
    }

    #[test]
    fn an_unknown_model_is_a_miss() {
        let g = Grounding::new(&registry_with_one_card(), Vec::new());
        let miss = g.card("radio/propagation/nakagami").expect_err("absent");
        assert!(
            miss.declared
                .contains(&"radio/propagation/log-distance".to_string())
        );
    }

    #[test]
    fn an_unknown_metric_is_a_miss() {
        let g = Grounding::new(&registry_with_one_card(), Vec::new());
        let miss = g.metric("pdr").expect_err("no providers were registered");
        assert!(!miss.known);
    }
}
