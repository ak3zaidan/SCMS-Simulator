//! Card-building helpers shared by the providers in this crate.
//!
//! 03-interfaces.md §12 makes a model card mandatory for every plug-in, and
//! 08-measurement-and-data.md §1 asks that a metric's definition and source live with the
//! code so "the docs cannot drift from the code". A metric provider's card therefore cites
//! the definition of what it computes, and every numeric parameter it reads at runtime is
//! declared on it (invariant I-C3).
//!
//! These helpers exist so that the five providers cannot disagree about how a citation to
//! the measurement design, or to a standard clause, is spelled.

use serde_json::json;
use v2xw_core::card::{
    Determinism, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};

/// A citation to a section of the measurement design.
///
/// `SourceKind::Code` rather than `Paper` or `Standard`: the reference is a document in this
/// repository at a known path, which is what that kind means. A metric whose definition
/// comes from a standard or a paper cites that instead, with [`standard`] or [`paper`].
#[must_use]
pub fn design(section: &str) -> Source {
    Source {
        kind: SourceKind::Code,
        reference: format!("docs/design/{section}"),
        accessed: Some("2026-09-18".to_string()),
        note: None,
    }
}

/// A citation to a standard and clause.
#[must_use]
pub fn standard(reference: &str) -> Source {
    Source {
        kind: SourceKind::Standard,
        reference: reference.to_string(),
        accessed: None,
        note: None,
    }
}

/// A citation to a paper or report.
#[must_use]
pub fn paper(reference: &str) -> Source {
    Source {
        kind: SourceKind::Paper,
        reference: reference.to_string(),
        accessed: None,
        note: None,
    }
}

/// A declared numeric parameter with an inclusive range.
#[must_use]
pub fn param(
    name: &str,
    unit: &str,
    default: serde_json::Value,
    min: serde_json::Value,
    max: serde_json::Value,
    source: Source,
) -> Parameter {
    Parameter {
        name: name.to_string(),
        unit: unit.to_string(),
        default,
        range: Some(vec![min, max]),
        source,
        calibration: None,
    }
}

/// A declared numeric parameter whose default is **not** sourced, with the calibration plan
/// registry rule R1 requires.
///
/// 04-models.md marks several surrogate-safety thresholds UNVERIFIED. A card that claimed a
/// source for one of them would be the "no black box" principle defeated by a citation that
/// does not say what it is cited for, so the honest form is `todo-calibrate` plus the plan
/// for getting the real number. The registry enforces the plan's presence (rule R1), and the
/// generated docs list every such parameter on one page.
#[must_use]
pub fn param_todo(
    name: &str,
    unit: &str,
    default: serde_json::Value,
    min: serde_json::Value,
    max: serde_json::Value,
    what: &str,
    plan: &str,
) -> Parameter {
    Parameter {
        name: name.to_string(),
        unit: unit.to_string(),
        default,
        range: Some(vec![min, max]),
        source: Source::todo_calibrate(what),
        calibration: Some(plan.to_string()),
    }
}

/// The two parameters every provider in this crate reads: the insufficiency threshold and
/// the confidence level.
///
/// Both are read at runtime, so I-C3 requires them on the card. The level is spelled as its
/// coverage fraction because that is how 08-measurement-and-data.md §4 writes it
/// (`replications_policy: {ci: 0.95}`).
#[must_use]
pub fn statistics_params() -> Vec<Parameter> {
    vec![
        param(
            "min_samples",
            "count",
            json!(crate::stats::DEFAULT_MIN_SAMPLES),
            json!(1),
            json!(1_000_000),
            design("08-measurement-and-data.md §1 (a thin bin is reported as insufficient)"),
        ),
        param(
            "confidence_level",
            "-",
            json!(0.95),
            json!(0.90),
            json!(0.99),
            design("08-measurement-and-data.md §4 (replications_policy: {ci: 0.95})"),
        ),
    ]
}

/// A metric provider's card: family `metric`, every tier, with the standard determinism
/// declaration (a metric provider draws no random numbers).
///
/// The caller adds the equations, the parameters, the assumptions, the limitations and the
/// sources — all of which [`ModelCard::validate`] and the registry then check.
#[must_use]
pub fn provider_card(id: &str, version: &str, purpose: &str) -> ModelCard {
    let mut card = ModelCard::new(id, Family::Metric, version, purpose);
    // A metric provider reads events and reduces them; it runs at every tier because it is
    // the event schema it depends on, not the fidelity of what produced the events.
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: Vec::new(),
    };
    card.assumptions
        .push("Events arrive in the order the run produced them.".to_string());
    card.assumptions.push(
        "Every field this provider reads is filled by the producing model at the selected tier; \
         an absent field is counted as absent, never as zero."
            .to_string(),
    );
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_card_validates_and_targets_this_engine() {
        let mut card = provider_card("metric/test/helper", "1.0.0", "A card.");
        card.parameters = statistics_params();
        card.sources.push(design("08-measurement-and-data.md §2"));
        card.validate().unwrap();
        card.check_api_version().unwrap();
    }

    #[test]
    fn a_declared_default_inside_its_range_is_accepted() {
        let p = param(
            "x",
            "m",
            serde_json::json!(25.0),
            serde_json::json!(0.0),
            serde_json::json!(1000.0),
            design("08-measurement-and-data.md §2"),
        );
        let mut card = provider_card("metric/test/range", "1.0.0", "A card.");
        card.parameters = vec![p];
        card.validate().unwrap();
    }
}
