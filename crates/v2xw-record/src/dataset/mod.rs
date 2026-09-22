//! Dataset exporters — 08-measurement-and-data.md §5 and §6.
//!
//! This module turns a finished run into a dataset somebody else can use: the
//! `ma-dataset` profile in both schema versions, a VeReMi-compatible per-receiver export,
//! the `receiver-logs` / `telemetry` / `net-trace` profiles, a datasheet, and the leakage
//! linter that gates all of them.
//!
//! | Concern | Module |
//! |---|---|
//! | The legacy canonical-JSON bytes, including Python's float spelling | [`pyjson`] |
//! | The forbidden-key registry and the linter | [`leakage`] |
//! | The v1 and v2 table shapes | [`tables`] |
//! | Channels → tables | [`assemble`] |
//! | A synthetic run covering every channel they read | [`fixture`] |
//! | Views for channels `v2xw-metrics` has none for | [`views`] |
//! | The declared float grids and the dataset scan | [`grids`] |
//! | `manifest.json` and the aggregate digest | [`manifest`] |
//! | `DATASHEET.md` | [`datasheet`] |
//! | The VeReMi-compatible export | [`veremi`] |
//! | `receiver-logs`, `telemetry`, `net-trace` | [`profiles`] |
//!
//! # The two gates every write goes through
//!
//! **Quantisation.** Every float is put on its field's declared grid before it is written
//! (D9), and [`crate::export::scan`] reads the written files back and fails on any value
//! off its grid. The scan is the check; the writer is not trusted to have been careful.
//!
//! **Leakage.** [`DatasetWriter::write`] runs the linter over the node-visible files it
//! just wrote and **refuses to finish** if anything is found: the dataset is written, then
//! inspected, then either declared clean or torn down. Refusing after writing rather than
//! before is deliberate — the linter inspects the bytes on disk, which is the only
//! artefact a consumer will ever see, and a linter that inspected the in-memory structs
//! would miss a defect in the writer itself. The datasheet then carries the verdict, so a
//! dataset cannot be published without its lint result attached.
//!
//! # No wall clock, no RNG
//!
//! An exporter is engine-facing code. It reads no clock — `build_utc` comes from the
//! engine's manifest ([`manifest::RunProvenance`]) — and it draws no random numbers, so
//! the sampling of the emissions tables is a deterministic stride rather than a
//! probability ([`assemble::DatasetAssembler::with_sample_stride`]). A writer that
//! consumed random numbers would change the run it was writing about.

pub mod assemble;
pub mod datasheet;
pub mod fixture;
pub mod grids;
pub mod leakage;
pub mod manifest;
pub mod profiles;
pub mod pyjson;
pub mod tables;
pub mod veremi;
pub mod views;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub use assemble::{DatasetAssembler, MaDataset};
pub use grids::{DatasetSchema, scan_dataset};
pub use leakage::{LeakageReport, LeakageViolation, ViolationKind, is_forbidden_feature_key};
pub use manifest::{DatasetManifest, OutputFile, RunProvenance};
pub use profiles::{LogProfile, ProfileSet};
pub use tables::DatasetProfile;

use crate::error::{RecordError, Result};

/// One written dataset.
#[derive(Debug, Clone, PartialEq)]
pub struct WrittenDataset {
    /// Where it is.
    pub root: PathBuf,
    /// Its manifest.
    pub manifest: DatasetManifest,
    /// The leakage linter's verdict over the node-visible files.
    pub lint: LeakageReport,
    /// Every file written, relative paths in path order, `manifest.json` and
    /// `DATASHEET.md` included.
    pub files: Vec<String>,
}

impl WrittenDataset {
    /// The aggregate data digest, which is what a consumer pins.
    #[must_use]
    pub fn data_digest(&self) -> &str {
        &self.manifest.data_digest_sha256
    }
}

/// Writes an [`MaDataset`] to disk in the legacy layout.
#[derive(Debug, Clone)]
pub struct DatasetWriter {
    root: PathBuf,
}

/// The dataset's file layout, as a table so the writer and the tests cannot disagree.
///
/// Each row is `(relative path, sort key description)`. The order of the *files* does not
/// matter — the digest sorts by path — but the order of the *rows inside* each file does,
/// because the legacy sort orders are part of the contract and the frozen audit compares
/// counts against them.
const LAYOUT: &[(&str, &str)] = &[
    ("ma/ma_reports.jsonl", "(ingest_time, report_id)"),
    ("ma/ma_cert_status.jsonl", "cert_digest"),
    ("ma/ma_investigations.jsonl", "(opened_time, case_id)"),
    ("ma/ma_crl_events.jsonl", "(issue_time, crl_id)"),
    ("ma/ma_crl_downloads.jsonl", "(t, crl_id, node_handle) [v2]"),
    ("ma/ma_report_transport.jsonl", "report_id [v2]"),
    ("ground_truth/gt_vehicle.jsonl", "true_vehicle_id"),
    (
        "ground_truth/gt_identity_map.jsonl",
        "pseudonym_cert_digest",
    ),
    ("ground_truth/gt_attacks.jsonl", "attack_id"),
    ("ground_truth/gt_report_labels.jsonl", "report_id"),
    (
        "ground_truth/gt_linkage_revocation.jsonl",
        "true_vehicle_id",
    ),
    ("ground_truth/gt_emissions_sample.jsonl", "emit_id"),
    ("ground_truth/gt_kinematics_sample.jsonl", "sample_id [v2]"),
    (
        "ground_truth/gt_revocation_stages.jsonl",
        "(revocation_id, t, stage) [v2]",
    ),
];

/// The node-visible files — the ones the linter must find clean.
///
/// `ma/` and nothing else. `ground_truth/` is where ground truth is *supposed* to be, so
/// linting it would be nonsense; the separation is the guarantee, and this list is the
/// machine-readable half of it.
pub const NODE_VISIBLE_FILES: &[&str] = &[
    "ma/ma_cert_status.jsonl",
    "ma/ma_crl_downloads.jsonl",
    "ma/ma_crl_events.jsonl",
    "ma/ma_report_transport.jsonl",
    "ma/ma_reports.jsonl",
    "ma/ma_investigations.jsonl",
];

impl DatasetWriter {
    /// A writer rooted at `root`, which is created if it does not exist.
    ///
    /// # Errors
    /// [`RecordError::Io`] if the directory tree cannot be created.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        for sub in ["ma", "ground_truth"] {
            let dir = root.join(sub);
            std::fs::create_dir_all(&dir).map_err(|e| RecordError::io(&dir, e))?;
        }
        Ok(DatasetWriter { root })
    }

    /// The root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes every table, the manifest and the datasheet, then lints what it wrote.
    ///
    /// The order matters and is the contract: tables → lint → refuse or continue →
    /// manifest (whose digests cover the tables) → datasheet (which carries the lint
    /// verdict and the digests). The manifest can therefore never disagree with the files
    /// it digests, and the datasheet can never claim a lint result that was not obtained.
    ///
    /// # Errors
    /// [`RecordError::Io`] for a file that cannot be written, and
    /// [`RecordError::Malformed`] naming the violations if the leakage linter finds
    /// anything. A leak is a refusal, not a warning: the point of the linter is that the
    /// build stops.
    pub fn write(&self, dataset: &MaDataset, prov: &RunProvenance) -> Result<WrittenDataset> {
        let profile = dataset.profile;
        let mut written: BTreeMap<String, Vec<u8>> = BTreeMap::new();

        // ---- the tables -------------------------------------------------------------
        write_table(&mut written, "ma/ma_reports.jsonl", &dataset.ma_reports)?;
        write_table(
            &mut written,
            "ma/ma_cert_status.jsonl",
            &dataset.ma_cert_status,
        )?;
        write_table(
            &mut written,
            "ma/ma_investigations.jsonl",
            &dataset.ma_investigations,
        )?;
        write_table(
            &mut written,
            "ma/ma_crl_events.jsonl",
            &dataset.ma_crl_events,
        )?;
        write_table(
            &mut written,
            "ground_truth/gt_vehicle.jsonl",
            &dataset.gt_vehicle,
        )?;
        write_table(
            &mut written,
            "ground_truth/gt_identity_map.jsonl",
            &dataset.gt_identity_map,
        )?;
        write_table(
            &mut written,
            "ground_truth/gt_attacks.jsonl",
            &dataset.gt_attacks,
        )?;
        write_table(
            &mut written,
            "ground_truth/gt_report_labels.jsonl",
            &dataset.gt_report_labels,
        )?;
        write_table(
            &mut written,
            "ground_truth/gt_linkage_revocation.jsonl",
            &dataset.gt_linkage_revocation,
        )?;
        write_table(
            &mut written,
            "ground_truth/gt_emissions_sample.jsonl",
            &dataset.gt_emissions_sample,
        )?;
        if profile.is_v2() {
            // Additive only: a v1 consumer never sees these paths, which is what makes v2
            // a superset rather than a different dataset.
            write_table(
                &mut written,
                "ma/ma_crl_downloads.jsonl",
                &dataset.ma_crl_downloads,
            )?;
            write_table(
                &mut written,
                "ma/ma_report_transport.jsonl",
                &dataset.ma_report_transport,
            )?;
            write_table(
                &mut written,
                "ground_truth/gt_kinematics_sample.jsonl",
                &dataset.gt_kinematics_sample,
            )?;
            write_table(
                &mut written,
                "ground_truth/gt_revocation_stages.jsonl",
                &dataset.gt_revocation_stages,
            )?;
        }

        for (rel, bytes) in &written {
            let path = self.root.join(rel);
            std::fs::write(&path, bytes).map_err(|e| RecordError::io(&path, e))?;
        }

        // ---- the lint, over the bytes on disk ---------------------------------------
        let lint = self.lint()?;
        if !lint.is_clean() {
            return Err(RecordError::malformed(
                "ma-dataset",
                format!(
                    "the leakage linter refused this dataset, so it was not finished: {}",
                    lint.summary()
                ),
            ));
        }

        // ---- the declared grids -----------------------------------------------------
        // Written before the manifest so it is digested with the data: the grids are part
        // of the dataset's contract (D9), and a declaration a consumer could not verify
        // had not been tampered with would be worth less than none.
        let schema = grids::DatasetSchema::new(profile);
        let schema_bytes = schema.to_bytes()?;
        let schema_path = self.root.join("schema.json");
        std::fs::write(&schema_path, &schema_bytes)
            .map_err(|e| RecordError::io(&schema_path, e))?;
        written.insert("schema.json".to_string(), schema_bytes);

        // ---- the manifest -----------------------------------------------------------
        let outputs: Vec<OutputFile> = written
            .iter()
            .map(|(rel, bytes)| OutputFile {
                path: rel.clone(),
                sha256: v2xw_core::hash::sha256_hex(bytes),
            })
            .collect();
        let manifest =
            DatasetManifest::new(profile, prov, dataset.counts(), outputs, lint.summary());
        let manifest_bytes = manifest.to_bytes()?;
        let manifest_path = self.root.join("manifest.json");
        std::fs::write(&manifest_path, &manifest_bytes)
            .map_err(|e| RecordError::io(&manifest_path, e))?;

        // ---- the datasheet ----------------------------------------------------------
        let sheet = datasheet::render(dataset, &manifest, &lint);
        let sheet_path = self.root.join("DATASHEET.md");
        std::fs::write(&sheet_path, sheet.as_bytes())
            .map_err(|e| RecordError::io(&sheet_path, e))?;

        let mut files: Vec<String> = written.keys().cloned().collect();
        files.push("DATASHEET.md".to_string());
        files.push("manifest.json".to_string());
        files.sort();

        Ok(WrittenDataset {
            root: self.root.clone(),
            manifest,
            lint,
            files,
        })
    }

    /// Lints the node-visible files on disk.
    ///
    /// # Errors
    /// [`RecordError::Io`] or [`RecordError::Json`] if a file exists but cannot be read
    /// back. A file that is absent is skipped — the v2-only tables are legitimately absent
    /// from a v1 dataset — but a file that is present and unreadable is a failure, because
    /// an unreadable artefact proves nothing.
    pub fn lint(&self) -> Result<LeakageReport> {
        let mut node_visible: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        for rel in NODE_VISIBLE_FILES {
            let path = self.root.join(rel);
            if !path.exists() {
                continue;
            }
            node_visible.insert((*rel).to_string(), crate::export::jsonl::read(&path)?);
        }
        // The identity-value half of the check needs the ground truth's own ids, which is
        // why it is read here even though it is never linted itself.
        let mut identities: BTreeSet<String> = BTreeSet::new();
        for rel in [
            "ground_truth/gt_vehicle.jsonl",
            "ground_truth/gt_identity_map.jsonl",
        ] {
            let path = self.root.join(rel);
            if !path.exists() {
                continue;
            }
            for row in crate::export::jsonl::read(&path)? {
                if let Some(id) = row.get("true_vehicle_id").and_then(|v| v.as_str()) {
                    identities.insert(id.to_string());
                }
            }
        }
        Ok(leakage::lint_dataset(&node_visible, &identities))
    }

    /// The sort key each table is written in, for the documentation and the tests.
    #[must_use]
    pub fn layout() -> &'static [(&'static str, &'static str)] {
        LAYOUT
    }
}

/// Serialises one table's rows to the legacy canonical JSONL bytes.
///
/// A non-finite float is refused rather than written: Python would have spelled it `NaN`,
/// which is not JSON, and a silently-nulled value in a dataset is a wrong number rather
/// than a missing one.
fn write_table<T: serde::Serialize>(
    into: &mut BTreeMap<String, Vec<u8>>,
    rel: &str,
    rows: &[T],
) -> Result<()> {
    let mut text = String::new();
    for row in rows {
        let value = serde_json::to_value(row)?;
        if let Some(field) = first_non_finite(&value, "") {
            return Err(RecordError::malformed(
                "ma-dataset",
                format!(
                    "{rel}: {field} is not a finite number, and a dataset must not carry one \
                     — quantise or omit it at the producer"
                ),
            ));
        }
        text.push_str(&pyjson::canonical_line(&value));
    }
    into.insert(rel.to_string(), text.into_bytes());
    Ok(())
}

/// The dotted path of the first non-finite float in a value, if there is one.
///
/// `serde_json` cannot even represent a non-finite `f64` — `to_value` turns one into
/// `null` — so the check is on the nulls that appear where a float was expected. Rather
/// than guess which nulls are legitimate, this looks for the one thing that is certainly
/// wrong: a value that serialised as a number and is not finite. In practice
/// `serde_json::Number` cannot hold one, so this is a belt-and-braces check that costs a
/// walk and documents the intent.
fn first_non_finite(value: &serde_json::Value, path: &str) -> Option<String> {
    match value {
        serde_json::Value::Number(n) => {
            let x = n.as_f64()?;
            (!x.is_finite()).then(|| path.to_string())
        }
        serde_json::Value::Array(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, v)| first_non_finite(v, &format!("{path}[{i}]"))),
        serde_json::Value::Object(m) => m.iter().find_map(|(k, v)| {
            let here = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            first_non_finite(v, &here)
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::scratch_dir;

    #[test]
    fn the_layout_table_covers_every_node_visible_file() {
        let paths: BTreeSet<&str> = LAYOUT.iter().map(|(p, _)| *p).collect();
        for f in NODE_VISIBLE_FILES {
            assert!(paths.contains(f), "{f} is not in the layout table");
        }
    }

    #[test]
    fn a_v1_dataset_writes_no_v2_file() {
        let dir = scratch_dir("dataset-v1-layout").expect("scratch");
        let ds = MaDataset {
            profile: DatasetProfile::V1,
            ..Default::default()
        };
        let w = DatasetWriter::new(&dir).expect("writer");
        let out = w.write(&ds, &RunProvenance::default()).expect("write");
        for v2_only in [
            "ma/ma_crl_downloads.jsonl",
            "ma/ma_report_transport.jsonl",
            "ground_truth/gt_kinematics_sample.jsonl",
            "ground_truth/gt_revocation_stages.jsonl",
        ] {
            assert!(
                !out.files.contains(&v2_only.to_string()),
                "{v2_only} must not exist in a v1 dataset"
            );
            assert!(!dir.join(v2_only).exists());
        }
        assert!(out.files.contains(&"manifest.json".to_string()));
        assert!(out.files.contains(&"DATASHEET.md".to_string()));
        assert!(out.lint.is_clean());
    }

    #[test]
    fn a_v2_dataset_is_a_superset_of_the_v1_file_set() {
        let dir = scratch_dir("dataset-v2-layout").expect("scratch");
        let ds = MaDataset {
            profile: DatasetProfile::V2,
            ..Default::default()
        };
        let out = DatasetWriter::new(&dir)
            .expect("writer")
            .write(&ds, &RunProvenance::default())
            .expect("write");
        for v1 in [
            "ma/ma_reports.jsonl",
            "ma/ma_cert_status.jsonl",
            "ground_truth/gt_vehicle.jsonl",
            "ground_truth/gt_emissions_sample.jsonl",
        ] {
            assert!(out.files.contains(&v1.to_string()), "{v1} missing from v2");
        }
        for v2 in [
            "ma/ma_crl_downloads.jsonl",
            "ground_truth/gt_kinematics_sample.jsonl",
        ] {
            assert!(out.files.contains(&v2.to_string()), "{v2} missing from v2");
        }
    }

    #[test]
    fn the_manifests_digests_cover_the_files_that_were_written() {
        let dir = scratch_dir("dataset-digests").expect("scratch");
        let ds = MaDataset {
            profile: DatasetProfile::V1,
            ..Default::default()
        };
        let out = DatasetWriter::new(&dir)
            .expect("writer")
            .write(&ds, &RunProvenance::default())
            .expect("write");
        for f in &out.manifest.outputs {
            let bytes = std::fs::read(dir.join(&f.path)).expect("the digested file exists");
            assert_eq!(
                v2xw_core::hash::sha256_hex(&bytes),
                f.sha256,
                "{} digest disagrees with the file",
                f.path
            );
        }
        assert_eq!(
            out.manifest.data_digest_sha256,
            manifest::aggregate_digest(&out.manifest.outputs)
        );
        // The manifest is not in its own digest, so writing it cannot change it.
        assert!(
            !out.manifest
                .outputs
                .iter()
                .any(|f| f.path == "manifest.json"),
            "manifest.json must be excluded from its own outputs list"
        );
    }

    #[test]
    fn writing_the_same_dataset_twice_produces_the_same_bytes() {
        let ds = MaDataset {
            profile: DatasetProfile::V2,
            ..Default::default()
        };
        let digest = |tag: &str| {
            let dir = scratch_dir(tag).expect("scratch");
            DatasetWriter::new(&dir)
                .expect("writer")
                .write(&ds, &RunProvenance::default())
                .expect("write")
                .manifest
                .data_digest_sha256
        };
        assert_eq!(digest("dataset-repro-a"), digest("dataset-repro-b"));
    }
}
