//! The dataset's declared float grids, and the scan that checks the written files against
//! them.
//!
//! D9: "every schema field that carries a float declares its quantum … the quantum is part
//! of the field's contract and appears in the model card **or the dataset schema**", and
//! ADR 0004 §7 asks for "a test [that] scans every output file for any value that is off
//! its grid".
//!
//! [`crate::export::scan`] already does that for the channel tables, against the
//! `schema.json` each one writes beside itself. The `ma-dataset` profile needs its own
//! because its columns are the *legacy* names, which carry no unit suffix: `ingest_time`
//! is seconds and `st_bbox` is metres, and neither says so. Falling back on the suffix
//! heuristic would give both the 1e-6 default, which is finer than the grid they are
//! actually written on — so the scan would pass, and it would be passing for the wrong
//! reason: it would never notice a writer that stopped quantising to 1e-3, because a value
//! on no grid at all is usually still off 1e-6 but a value on 1e-4 is not.
//!
//! So this module *declares* the grid of every dataset column, writes the declaration into
//! the dataset as `schema.json`, and scans against the declaration. The scan is then a
//! check of the files against a contract, not a tautology about what the writer did.
//!
//! # Coarser is on-grid
//!
//! A value on a coarser grid is exactly on a finer one: `quantize_to` composes, and
//! `crate::grid`'s `metric_grid_composes_with_the_coarser_ones` proves it bit for bit. So
//! declaring `detector_score` at 1e-3 and writing a value that happens to be 1.5 passes,
//! while writing 1.5001 does not.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{RecordError, Result};
use crate::grid::{Q_DB, Q_METRES, Q_RATIO, Q_SECONDS};

/// The declared quantum of every float column in the `ma-dataset` tables.
///
/// The grid each one is on and *why*:
///
/// * **1e-3 (seconds)** — every time column. The legacy engine wrote `round(t, 3)` and the
///   contract is millisecond resolution; D9 keeps the same convention.
/// * **1e-3 (metres)** — positions and the `st_bbox` corners. D9 names `st_bbox`
///   specifically: "the MA dataset exporter keeps `st_bbox` for v1 schema compatibility but
///   quantises it like every other float".
/// * **1e-3** — detector scores. They are normalised ratios but the legacy engine wrote
///   them at three decimals, and a coarser declaration than the data would reject valid
///   files while a finer one would accept unquantised ones.
/// * **1e-2 (dB)** — the v2 `rx_rssi_dbm`.
/// * **1e-4 (ratios)** — the confidence radius and anything else in `[0, 1]`.
pub const DATASET_GRIDS: &[(&str, f64)] = &[
    ("believed_heading", Q_RATIO),
    ("believed_speed", Q_METRES),
    ("claimed_speed", Q_METRES),
    ("claimed_x", Q_METRES),
    ("claimed_y", Q_METRES),
    ("decision_time", Q_SECONDS),
    ("delay_s", Q_SECONDS),
    ("detection_time", Q_SECONDS),
    ("detector_score", Q_SECONDS),
    ("detector_score_norm", Q_SECONDS),
    ("download_s", Q_SECONDS),
    ("first_seen", Q_SECONDS),
    ("generation_time", Q_SECONDS),
    ("ingest_time", Q_SECONDS),
    ("issue_time", Q_SECONDS),
    ("last_seen", Q_SECONDS),
    ("opened_time", Q_SECONDS),
    ("pos_conf", Q_RATIO),
    ("resolution_time", Q_SECONDS),
    ("revocation_time", Q_SECONDS),
    ("rx_rssi_dbm", Q_DB),
    ("score", Q_SECONDS),
    ("spawn_time", Q_SECONDS),
    ("st_bbox", Q_METRES),
    ("st_tend", Q_SECONDS),
    ("st_tstart", Q_SECONDS),
    ("subject_pos_confidence", Q_RATIO),
    ("t", Q_SECONDS),
    ("true_heading", Q_RATIO),
    ("true_revocation_time", Q_SECONDS),
    ("true_speed", Q_METRES),
    ("true_x", Q_METRES),
    ("true_y", Q_METRES),
    ("valid_from", Q_SECONDS),
    ("valid_to", Q_SECONDS),
    ("verify_latency_ms", Q_SECONDS),
];

/// The declared quantum of one dataset column.
///
/// A `detnorm_*` column takes the detector-score grid. Anything not declared falls back to
/// [`crate::export::schema::declared_quantum`], which reads the unit suffix — so a column
/// added later with a conventional name is still checked, and one added with an
/// unconventional name gets the finest grid D9 lists rather than no check at all.
#[must_use]
pub fn column_quantum(name: &str) -> f64 {
    if let Some((_, q)) = DATASET_GRIDS.iter().find(|(n, _)| *n == name) {
        return *q;
    }
    if name.starts_with("detnorm_") || name.starts_with("detmax_") {
        return Q_SECONDS;
    }
    crate::export::schema::declared_quantum(name)
}

/// The `schema.json` a dataset ships, declaring each table's float grids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetSchema {
    /// The schema id.
    pub schema: String,
    /// Column name → declared quantum, for every float column across the tables.
    pub grids: BTreeMap<String, f64>,
    /// What the grids mean, for a human reading the file.
    pub legend: String,
}

impl DatasetSchema {
    /// The declaration for a dataset in `profile`.
    #[must_use]
    pub fn new(profile: super::DatasetProfile) -> Self {
        let mut grids: BTreeMap<String, f64> = DATASET_GRIDS
            .iter()
            .map(|(n, q)| ((*n).to_string(), *q))
            .collect();
        for d in super::tables::DETNORM_VOCAB {
            grids.insert(format!("detnorm_{d}"), Q_SECONDS);
        }
        DatasetSchema {
            schema: format!("v2xw/ma-dataset/{}", profile.as_str()),
            grids,
            legend: "Each entry is a float column's declared quantum (build decision D9). \
                     Every value in the dataset is an exact multiple of its column's \
                     quantum, or of a coarser one; `dataset scan` checks it."
                .to_string(),
        }
    }

    /// The file's bytes: sorted, pretty JSON with a trailing newline.
    ///
    /// # Errors
    /// [`RecordError::Json`] if it will not serialise.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = serde_json::to_vec_pretty(self)?;
        out.push(b'\n');
        Ok(out)
    }
}

/// What a dataset scan looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DatasetScanReport {
    /// Files scanned.
    pub files: usize,
    /// Rows scanned.
    pub rows: usize,
    /// Float values checked.
    pub values: usize,
}

/// Scans every JSONL table under `root` against the grids `root/schema.json` declares.
///
/// The declaration is read back **from the file**, not taken from
/// [`DATASET_GRIDS`]: a scan that consulted the same constant the writer consulted would
/// agree with the writer by construction and could never disagree with the artefact.
///
/// # Errors
/// [`RecordError::OffGrid`] naming the first value that is off its declared grid, or an
/// I/O or JSON error. A missing `schema.json` is an error too: a dataset with no declared
/// grids cannot be scanned, and reporting success for it would be the worst answer.
pub fn scan_dataset(root: impl AsRef<Path>) -> Result<DatasetScanReport> {
    let root = root.as_ref();
    let schema_path = root.join("schema.json");
    let bytes = std::fs::read(&schema_path).map_err(|e| RecordError::io(&schema_path, e))?;
    let declared: DatasetSchema = serde_json::from_slice(&bytes)?;

    let mut report = DatasetScanReport::default();
    for (rel, _) in super::DatasetWriter::layout() {
        let path = root.join(rel);
        if !path.exists() {
            continue;
        }
        report.files += 1;
        let rows = crate::export::jsonl::read(&path)?;
        report.rows += rows.len();
        for (i, row) in rows.iter().enumerate() {
            let Some(obj) = row.as_object() else { continue };
            for (key, value) in obj {
                check(rel, &declared, key, value, i, &mut report)?;
            }
        }
    }
    Ok(report)
}

fn check(
    file: &str,
    declared: &DatasetSchema,
    key: &str,
    value: &serde_json::Value,
    row: usize,
    report: &mut DatasetScanReport,
) -> Result<()> {
    match value {
        serde_json::Value::Number(n) if n.is_f64() => {
            let Some(x) = n.as_f64() else { return Ok(()) };
            let quantum = declared
                .grids
                .get(key)
                .copied()
                .unwrap_or_else(|| column_quantum(key));
            report.values += 1;
            if !v2xw_core::math::is_on_grid(x, quantum) {
                return Err(RecordError::OffGrid {
                    file: file.to_string(),
                    channel: "ma-dataset".to_string(),
                    field: key.to_string(),
                    value: x,
                    quantum,
                    row,
                });
            }
            Ok(())
        }
        // An array takes its grid from the array's own key, which is how `st_bbox`'s four
        // corners are all checked on the metre grid.
        serde_json::Value::Array(a) => {
            for e in a {
                check(file, declared, key, e, row, report)?;
            }
            Ok(())
        }
        serde_json::Value::Object(m) => {
            for (k, e) in m {
                check(file, declared, k, e, row, report)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::DatasetProfile;

    #[test]
    fn the_legacy_time_and_metre_columns_are_declared_rather_than_guessed_from_a_suffix() {
        // Without the declaration these would all fall back to 1e-6, and the scan would
        // stop being able to notice a writer that quantised at the wrong grid.
        assert_eq!(column_quantum("ingest_time"), 1e-3);
        assert_eq!(column_quantum("st_bbox"), 1e-3);
        assert_eq!(column_quantum("true_x"), 1e-3);
        assert_eq!(column_quantum("pos_conf"), 1e-4);
        assert_eq!(column_quantum("rx_rssi_dbm"), 1e-2);
        assert_eq!(column_quantum("detnorm_positionJump"), 1e-3);
        // …and a column nobody declared still gets a grid from its suffix.
        assert_eq!(column_quantum("something_new_dbm"), 1e-2);
        assert_eq!(column_quantum("something_new"), 1e-6);
    }

    #[test]
    fn every_detector_in_the_vocabulary_is_declared() {
        let s = DatasetSchema::new(DatasetProfile::V1);
        for d in crate::dataset::tables::DETNORM_VOCAB {
            assert!(
                s.grids.contains_key(&format!("detnorm_{d}")),
                "detnorm_{d} has no declared grid"
            );
        }
    }

    #[test]
    fn a_value_on_a_coarser_grid_is_on_its_declared_one() {
        // The composition property, as it applies here: 1.5 is on 1e-3 and also on 1e-6.
        assert!(v2xw_core::math::is_on_grid(1.5, 1e-3));
        assert!(v2xw_core::math::is_on_grid(1.5, 1e-6));
        assert!(!v2xw_core::math::is_on_grid(1.500_1, 1e-3));
    }
}
