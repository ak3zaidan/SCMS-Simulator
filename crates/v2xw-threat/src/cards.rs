//! Citation helpers, so the cards in this crate cannot disagree about how a source is
//! spelled.
//!
//! Every default in this crate is either a standard clause, a paper, a design section, or
//! a number that the legacy engine carried with no citation of its own. The last class is
//! the honest one to get right: 07-threats-and-detection.md §3.1 requires the legacy
//! twelve detectors to keep "the same formulas and normalisation", so the numbers must be
//! reproduced exactly, but reproducing a number is not the same as justifying it. Those
//! defaults therefore carry [`legacy`] — a [`SourceKind::Code`] citation naming the file
//! and the symbol the value was read out of — and, where the legacy engine itself gave no
//! rationale, a `calibration` plan through [`legacy_uncited`].

use serde_json::Value;
use v2xw_core::card::{Parameter, Source, SourceKind};

/// The legacy Python engine's path inside this repository.
pub const LEGACY_PY: &str = "legacy/scms_sim_ref/mock_pipeline/run.py";

/// The legacy JVM beacon application's path inside this repository.
pub const LEGACY_JVM: &str = "legacy/reference/jvm/ScmsBeaconApp.java";

/// The date the legacy sources cited by this crate were read.
pub const LEGACY_ACCESSED: &str = "2026-09-22";

/// A citation to a symbol in a legacy reference implementation.
///
/// `file` is one of [`LEGACY_PY`] or [`LEGACY_JVM`]; `symbol` names the function, class
/// field or module constant the value was read out of, so a reader can check the port
/// against the original rather than against this crate's prose.
#[must_use]
pub fn legacy(file: &str, symbol: &str) -> Source {
    Source {
        kind: SourceKind::Code,
        reference: format!("{file} :: {symbol} (legacy)"),
        accessed: Some(LEGACY_ACCESSED.to_string()),
        note: None,
    }
}

/// A citation to a section of the design documents.
#[must_use]
pub fn design(section: &str) -> Source {
    Source {
        kind: SourceKind::Code,
        reference: format!("docs/design/{section}"),
        accessed: Some(LEGACY_ACCESSED.to_string()),
        note: None,
    }
}

/// A citation to a standard and clause.
#[must_use]
pub fn standard(reference: &str) -> Source {
    Source::new(SourceKind::Standard, reference)
}

/// A citation to a paper or report.
#[must_use]
pub fn paper(reference: &str) -> Source {
    Source::new(SourceKind::Paper, reference)
}

/// A parameter whose default is a legacy value with a citation to the legacy symbol.
///
/// Use this when the legacy engine's own comment explains the number (the DENM brake
/// bound, say, which is derived from the benign trigger). When it does not, use
/// [`legacy_uncited`] instead, so the gap is visible on the generated todo-calibrate page
/// rather than hidden behind a file path.
#[must_use]
pub fn legacy_param(name: &str, unit: &str, default: Value, file: &str, symbol: &str) -> Parameter {
    Parameter::new(name, unit, default, legacy(file, symbol))
}

/// A parameter whose default reproduces a legacy constant that the legacy engine itself
/// never justified.
///
/// The source kind stays [`SourceKind::TodoCalibrate`], because that is what is true: the
/// value is reproduced for conformance, not because anyone has shown it is right. The
/// reference still names the legacy symbol, so the conformance test can assert against it,
/// and `calibration` says what would settle it.
#[must_use]
pub fn legacy_uncited(
    name: &str,
    unit: &str,
    default: Value,
    file: &str,
    symbol: &str,
    plan: &str,
) -> Parameter {
    let mut p = Parameter::new(
        name,
        unit,
        default,
        Source {
            kind: SourceKind::TodoCalibrate,
            reference: format!("{file} :: {symbol} (legacy, uncited there)"),
            accessed: Some(LEGACY_ACCESSED.to_string()),
            note: Some(
                "reproduced exactly for dataset conformance (07-threats-and-detection.md §3.1); \
                 the legacy engine gave no source for it"
                    .to_string(),
            ),
        },
    );
    p.calibration = Some(plan.to_string());
    p
}

/// A parameter with a cited default and an inclusive `[min, max]` range.
#[must_use]
pub fn ranged(
    name: &str,
    unit: &str,
    default: Value,
    min: Value,
    max: Value,
    source: Source,
) -> Parameter {
    let mut p = Parameter::new(name, unit, default, source);
    p.range = Some(vec![min, max]);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_uncited_legacy_default_stays_todo_calibrate_and_carries_a_plan() {
        let p = legacy_uncited(
            "freq_max",
            "msg/interval",
            json!(6.0),
            LEGACY_PY,
            "PipelineConfig.freq_max",
            "measure the CAM rate distribution of a calibrated benign fleet.",
        );
        assert_eq!(p.source.kind, SourceKind::TodoCalibrate);
        assert!(p.source.reference.contains("PipelineConfig.freq_max"));
        assert!(p.calibration.is_some_and(|c| !c.trim().is_empty()));
    }

    #[test]
    fn a_legacy_citation_names_the_file_and_the_symbol() {
        let s = legacy(LEGACY_JVM, "KF_ALPHA");
        assert_eq!(s.kind, SourceKind::Code);
        assert!(s.reference.contains("ScmsBeaconApp.java"));
        assert!(s.reference.contains("KF_ALPHA"));
    }
}
