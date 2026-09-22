//! The frozen legacy conformance suite, run against the v1 export.
//!
//! `legacy/tools/verify_data.py` is the audit that gates every dataset the reference
//! engine ever published, and 08-measurement-and-data.md §6 makes passing it the
//! acceptance criterion for the v1 profile: "`verify_data.py` must pass unchanged on
//! v1-profile output". That audit is Python and this is Rust, so this file re-implements
//! **its checks**, check id for check id, over the files the Rust exporter actually wrote:
//!
//! | Id | What it asserts |
//! |---|---|
//! | `L1` / `L2` | no forbidden column, no forbidden key, no ORACLE record in an MA file |
//! | `L3` | no true-identity *value* in an MA file |
//! | `R1` | `report_id` bijection between `ma_reports` and `gt_report_labels` |
//! | `R3` | every subject digest resolves; unresolved reporters ≤ the RSU count |
//! | `C3` | `report_correctness` is in the closed vocabulary |
//! | `C4` | `correct` ⇒ attacker or faulty; `false_positive` ⇒ benign and not faulty |
//! | `C6` | every `malicious_false_report` was filed by a true attacker |
//! | `CNT1`–`CNT4` | the manifest's counts reconcile with the line counts |
//! | `V1` | `ingest_time >= detection_time` |
//! | `V2` | every `detnorm_*` is non-negative |
//! | `V4` | a falsified emission belongs to an attacker and falls inside its window |
//! | `I1` / `I2` | every file's digest, and the aggregate digest, match the manifest |
//!
//! Re-implementing rather than shelling out to `python3` is deliberate: the Rust suite has
//! to go red in CI on a machine with no Python and no `pandas`, and a check that is skipped
//! is not a check. The Python audit is *also* run against this exporter's output, by hand,
//! and what it says is reported with the work — but the gate that blocks a merge is here.
//!
//! Every check is written so it can fail. Where a check needs a non-empty input to mean
//! anything, the test asserts the input is non-empty first, because the frozen audit's own
//! failure mode was `SKIP` on an empty table and a silent pass is the worst outcome of all.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_record::dataset::fixture::{DatasetShape, dataset, provenance};
use v2xw_record::dataset::leakage::{ORACLE, is_forbidden_feature_key};
use v2xw_record::dataset::tables::{DETNORM_VOCAB, REPORT_CORRECTNESS_VOCAB};
use v2xw_record::dataset::{DatasetProfile, DatasetWriter, MaDataset, WrittenDataset};
use v2xw_record::export::jsonl;
use v2xw_record::fixture::scratch_dir;

/// A written v1 dataset, and the tables read back off disk.
struct Audit {
    root: std::path::PathBuf,
    written: WrittenDataset,
    tables: BTreeMap<String, Vec<serde_json::Value>>,
}

impl Audit {
    fn rows(&self, rel: &str) -> &[serde_json::Value] {
        self.tables.get(rel).map_or(&[], Vec::as_slice)
    }

    fn strs(&self, rel: &str, field: &str) -> Vec<String> {
        self.rows(rel)
            .iter()
            .filter_map(|r| r.get(field).and_then(|v| v.as_str()).map(str::to_string))
            .collect()
    }
}

/// Writes the default fixture in `profile` into a scratch directory of its own.
///
/// The `tag` is the caller's name and it matters: `scratch_dir` keys on the tag alone, so
/// two tests sharing one would write into the same directory concurrently and read each
/// other's half-written files. That happened, and it made two checks pass that should have
/// failed and two fail that should have passed — which is why every call site names itself.
fn audit(profile: DatasetProfile, tag: &str) -> Audit {
    let shape = DatasetShape::default();
    let ds = dataset(&shape, profile).expect("the fixture assembles");
    write_audit(&ds, profile, tag)
}

fn write_audit(ds: &MaDataset, profile: DatasetProfile, tag: &str) -> Audit {
    let tag = format!("legacy-conformance-{}-{}", profile.as_str(), tag);
    let root = scratch_dir(&tag).expect("a scratch directory");
    let writer = DatasetWriter::new(&root).expect("a writer");
    let written = writer.write(ds, &provenance()).expect("the dataset writes");
    // The dataset is also copied where a human — or the frozen Python audit — can look at
    // it, when the caller asks. `V2XW_DATASET_OUT=/some/dir cargo test` exports it.
    if let Ok(out) = std::env::var("V2XW_DATASET_OUT") {
        let out = std::path::Path::new(&out).join(format!("v2xw-{}", profile.as_str()));
        copy_tree(&root, &out).expect("the export copy");
        eprintln!("dataset exported to {}", out.display());
    }
    let mut tables = BTreeMap::new();
    for (rel, _) in DatasetWriter::layout() {
        let path = root.join(rel);
        if path.exists() {
            tables.insert((*rel).to_string(), jsonl::read(&path).expect("readable"));
        }
    }
    Audit {
        root,
        written,
        tables,
    }
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Every key in a JSON value, at every depth — the audit's `_all_keys`.
fn all_keys(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                out.push(k.clone());
                all_keys(v, out);
            }
        }
        serde_json::Value::Array(a) => {
            for v in a {
                all_keys(v, out);
            }
        }
        _ => {}
    }
}

const MA_FILES: &[&str] = &[
    "ma/ma_reports.jsonl",
    "ma/ma_cert_status.jsonl",
    "ma/ma_investigations.jsonl",
    "ma/ma_crl_events.jsonl",
];

// ---------------------------------------------------------------------------------------
// A. Leakage and privacy
// ---------------------------------------------------------------------------------------

/// `L2_ma_no_forbidden_keys`: no MA file carries a forbidden key or an ORACLE record.
#[test]
fn l2_no_ma_file_carries_a_forbidden_key_or_an_oracle_record() {
    let a = audit(
        DatasetProfile::V1,
        "l2_no_ma_file_carries_a_forbidden_key_or_an_oracle_record",
    );
    let mut inspected = 0usize;
    for rel in MA_FILES {
        let rows = a.rows(rel);
        for (i, row) in rows.iter().enumerate() {
            assert_ne!(
                row.get("_visibility").and_then(|v| v.as_str()),
                Some(ORACLE),
                "{rel}[{i}] is an ORACLE record in an MA file"
            );
            let mut keys = Vec::new();
            all_keys(row, &mut keys);
            inspected += keys.len();
            let bad: Vec<&String> = keys
                .iter()
                .filter(|k| is_forbidden_feature_key(k))
                .collect();
            assert!(bad.is_empty(), "{rel}[{i}] leaks {bad:?}");
        }
    }
    assert!(
        inspected > 100,
        "only {inspected} keys inspected — the audit looked at nothing"
    );
}

/// `L3_no_true_id_in_ma`: no true device id appears as a *value* in an MA file.
#[test]
fn l3_no_true_device_id_appears_anywhere_in_an_ma_file() {
    let a = audit(
        DatasetProfile::V1,
        "l3_no_true_device_id_appears_anywhere_in_an_ma_file",
    );
    let true_ids: BTreeSet<String> = a
        .strs("ground_truth/gt_vehicle.jsonl", "true_vehicle_id")
        .into_iter()
        .chain(a.strs("ground_truth/gt_identity_map.jsonl", "true_vehicle_id"))
        .collect();
    assert!(!true_ids.is_empty(), "no true ids to look for");
    for rel in MA_FILES {
        let blob = serde_json::to_string(a.rows(rel)).expect("serialise");
        for id in &true_ids {
            assert!(
                !blob.contains(&format!("\"{id}\"")),
                "{rel} contains the true identity {id:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// B. Referential integrity
// ---------------------------------------------------------------------------------------

/// `R1_reportid_bijection_ma_vs_gtlabels`.
#[test]
fn r1_the_report_ids_are_a_bijection_between_the_reports_and_their_labels() {
    let a = audit(
        DatasetProfile::V1,
        "r1_the_report_ids_are_a_bijection_between_the_reports_and_their_labels",
    );
    let ma: BTreeSet<String> = a
        .strs("ma/ma_reports.jsonl", "report_id")
        .into_iter()
        .collect();
    let gt: BTreeSet<String> = a
        .strs("ground_truth/gt_report_labels.jsonl", "report_id")
        .into_iter()
        .collect();
    assert!(!ma.is_empty(), "no reports, so the check would be vacuous");
    assert_eq!(ma, gt, "the report-id sets differ");
    assert_eq!(
        ma.len(),
        a.rows("ma/ma_reports.jsonl").len(),
        "duplicate report ids"
    );
}

/// `R3_cert_digests_resolve`: every subject digest resolves through the identity map, and
/// no more reporters are unresolved than there are RSUs.
#[test]
fn r3_every_subject_digest_resolves_and_only_rsus_may_not() {
    let a = audit(
        DatasetProfile::V1,
        "r3_every_subject_digest_resolves_and_only_rsus_may_not",
    );
    let known: BTreeSet<String> = a
        .strs(
            "ground_truth/gt_identity_map.jsonl",
            "pseudonym_cert_digest",
        )
        .into_iter()
        .collect();
    assert!(!known.is_empty());
    let subjects: BTreeSet<String> = a
        .strs("ma/ma_reports.jsonl", "subject_cert_digest")
        .into_iter()
        .collect();
    assert!(
        !subjects.is_empty(),
        "no subjects, so the check would be vacuous"
    );
    let unresolved: Vec<&String> = subjects.difference(&known).collect();
    assert!(unresolved.is_empty(), "unresolved subjects: {unresolved:?}");

    let reporters: BTreeSet<String> = a
        .strs("ma/ma_reports.jsonl", "reporter_cert_digest")
        .into_iter()
        .collect();
    let n_rsus = a
        .written
        .manifest
        .config
        .get("n_rsus")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let unresolved_reporters = reporters.difference(&known).count();
    assert!(
        unresolved_reporters <= n_rsus,
        "{unresolved_reporters} unresolved reporters with an RSU allowance of {n_rsus}"
    );
}

// ---------------------------------------------------------------------------------------
// C. Label correctness
// ---------------------------------------------------------------------------------------

/// `C3_report_correctness_vocab`.
#[test]
fn c3_every_correctness_label_is_in_the_frozen_vocabulary() {
    let a = audit(
        DatasetProfile::V1,
        "c3_every_correctness_label_is_in_the_frozen_vocabulary",
    );
    let vocab: BTreeSet<&str> = REPORT_CORRECTNESS_VOCAB.iter().copied().collect();
    let seen: BTreeSet<String> = a
        .strs("ground_truth/gt_report_labels.jsonl", "report_correctness")
        .into_iter()
        .collect();
    assert!(!seen.is_empty(), "no labels, so the check would be vacuous");
    for s in &seen {
        assert!(
            vocab.contains(s.as_str()),
            "{s:?} is outside the vocabulary"
        );
    }
}

/// `C4_correctness_semantics`: `correct` implies the subject really was an attacker or
/// faulty; `false_positive` implies it was neither.
#[test]
fn c4_a_correct_label_means_the_subject_really_was_bad_and_a_false_positive_means_it_was_not() {
    let a = audit(
        DatasetProfile::V1,
        "c4_a_correct_label_means_the_subject_really_was_bad_and_a_false_positive_means_it_was_not",
    );
    let mut attacker: BTreeMap<String, bool> = BTreeMap::new();
    let mut faulty: BTreeMap<String, bool> = BTreeMap::new();
    for v in a.rows("ground_truth/gt_vehicle.jsonl") {
        let id = v["true_vehicle_id"].as_str().expect("id").to_string();
        attacker.insert(id.clone(), v["is_attacker"].as_bool().unwrap_or(false));
        faulty.insert(id, v["is_faulty"].as_bool().unwrap_or(false));
    }
    let mut checked = 0usize;
    for l in a.rows("ground_truth/gt_report_labels.jsonl") {
        let subject = l["subject_true_id"].as_str().expect("subject");
        let correctness = l["report_correctness"].as_str().expect("correctness");
        let bad = attacker.get(subject).copied().unwrap_or(false)
            || faulty.get(subject).copied().unwrap_or(false);
        match correctness {
            "correct" => {
                assert!(
                    bad,
                    "report about {subject} labelled correct, but it is benign"
                );
                checked += 1;
            }
            "false_positive" => {
                assert!(
                    !bad,
                    "report about {subject} labelled a false positive, but it is bad"
                );
                checked += 1;
            }
            _ => {}
        }
    }
    assert!(
        checked > 0,
        "no correct/false_positive labels, so the check was vacuous"
    );
}

/// `C6_collusion_consistency`: every `malicious_false_report` was filed by a true attacker.
#[test]
fn c6_a_malicious_false_report_was_filed_by_a_true_attacker() {
    let a = audit(
        DatasetProfile::V1,
        "c6_a_malicious_false_report_was_filed_by_a_true_attacker",
    );
    let attackers: BTreeSet<String> = a
        .rows("ground_truth/gt_vehicle.jsonl")
        .iter()
        .filter(|v| v["is_attacker"].as_bool().unwrap_or(false))
        .map(|v| v["true_vehicle_id"].as_str().expect("id").to_string())
        .collect();
    let mfr: Vec<&serde_json::Value> = a
        .rows("ground_truth/gt_report_labels.jsonl")
        .iter()
        .filter(|l| l["report_correctness"] == "malicious_false_report")
        .collect();
    assert!(
        !mfr.is_empty(),
        "the fixture produced no collusion, so the check was vacuous"
    );
    for l in mfr {
        let reporter = l["reporter_true_id"].as_str().expect("reporter");
        assert!(
            attackers.contains(reporter),
            "a malicious false report was filed by {reporter}, which is not an attacker"
        );
    }
}

// ---------------------------------------------------------------------------------------
// D. Count reconciliation
// ---------------------------------------------------------------------------------------

/// `CNT1`–`CNT4`.
#[test]
fn the_manifests_counts_reconcile_with_the_line_counts() {
    let a = audit(
        DatasetProfile::V1,
        "the_manifests_counts_reconcile_with_the_line_counts",
    );
    let counts = &a.written.manifest.counts;
    assert_eq!(
        counts["vehicles"] as usize,
        a.rows("ground_truth/gt_vehicle.jsonl").len(),
        "CNT1"
    );
    assert_eq!(
        counts["reports"] as usize,
        a.rows("ma/ma_reports.jsonl").len(),
        "CNT2 (ma_reports)"
    );
    assert_eq!(
        counts["reports"] as usize,
        a.rows("ground_truth/gt_report_labels.jsonl").len(),
        "CNT2 (gt_report_labels)"
    );
    assert_eq!(
        counts["investigations"] as usize,
        a.rows("ma/ma_investigations.jsonl").len(),
        "CNT4"
    );

    // CNT3: the count is distinct true *devices* with a revoked certificate, not the
    // number of revoked certificates. The fixture rotates pseudonyms, so the two differ
    // and a wrong implementation is visible here rather than hidden by a one-to-one map.
    let map: BTreeMap<String, String> = a
        .rows("ground_truth/gt_identity_map.jsonl")
        .iter()
        .map(|r| {
            (
                r["pseudonym_cert_digest"]
                    .as_str()
                    .expect("digest")
                    .to_string(),
                r["true_vehicle_id"].as_str().expect("id").to_string(),
            )
        })
        .collect();
    let revoked_certs: Vec<&serde_json::Value> = a
        .rows("ma/ma_cert_status.jsonl")
        .iter()
        .filter(|c| c["crl_status"] == "revoked")
        .collect();
    let revoked_devices: BTreeSet<&String> = revoked_certs
        .iter()
        .filter_map(|c| map.get(c["cert_digest"].as_str().expect("digest")))
        .collect();
    assert!(
        !revoked_devices.is_empty(),
        "nothing revoked, so CNT3 was vacuous"
    );
    assert!(
        revoked_certs.len() > revoked_devices.len(),
        "the fixture must rotate pseudonyms, or CNT3 cannot distinguish devices from certificates"
    );
    assert_eq!(counts["revoked"] as usize, revoked_devices.len(), "CNT3");
}

// ---------------------------------------------------------------------------------------
// F. Value sanity
// ---------------------------------------------------------------------------------------

/// `V1_ingest_after_detection`.
#[test]
fn v1_no_report_is_ingested_before_it_was_detected() {
    let a = audit(
        DatasetProfile::V1,
        "v1_no_report_is_ingested_before_it_was_detected",
    );
    let rows = a.rows("ma/ma_reports.jsonl");
    assert!(!rows.is_empty());
    for (i, r) in rows.iter().enumerate() {
        let ingest = r["ingest_time"].as_f64().expect("ingest_time");
        let detect = r["detection_time"].as_f64().expect("detection_time");
        assert!(
            ingest >= detect - 1e-9,
            "report {i}: ingest {ingest} precedes detection {detect}"
        );
    }
}

/// `V2_detnorm_nonneg`, and the vocabulary itself: every report carries every
/// `detnorm_*` column, so the table has one schema rather than one per row.
#[test]
fn v2_every_detnorm_column_is_present_on_every_report_and_none_is_negative() {
    let a = audit(
        DatasetProfile::V1,
        "v2_every_detnorm_column_is_present_on_every_report_and_none_is_negative",
    );
    let rows = a.rows("ma/ma_reports.jsonl");
    assert!(!rows.is_empty());
    for (i, r) in rows.iter().enumerate() {
        let obj = r.as_object().expect("object");
        for d in DETNORM_VOCAB {
            let key = format!("detnorm_{d}");
            let v = obj
                .get(&key)
                .unwrap_or_else(|| panic!("report {i} is missing {key}"));
            let x = v
                .as_f64()
                .unwrap_or_else(|| panic!("{key} is not a number"));
            assert!(x >= 0.0, "report {i}: {key} = {x} is negative");
        }
        // …and nothing calls itself a detnorm that is not in the vocabulary.
        for key in obj.keys().filter(|k| k.starts_with("detnorm_")) {
            let name = key.trim_start_matches("detnorm_");
            assert!(
                DETNORM_VOCAB.contains(&name),
                "{key} is outside the frozen detector vocabulary"
            );
        }
    }
}

/// `V4_emissions_truth`: a falsified emission belongs to an attacker, and falls inside
/// that attacker's window.
#[test]
fn v4_a_falsified_emission_belongs_to_an_attacker_inside_its_attack_window() {
    let a = audit(
        DatasetProfile::V1,
        "v4_a_falsified_emission_belongs_to_an_attacker_inside_its_attack_window",
    );
    let attackers: BTreeSet<String> = a
        .rows("ground_truth/gt_vehicle.jsonl")
        .iter()
        .filter(|v| v["is_attacker"].as_bool().unwrap_or(false))
        .map(|v| v["true_vehicle_id"].as_str().expect("id").to_string())
        .collect();
    let windows: BTreeMap<String, (f64, f64)> = a
        .rows("ground_truth/gt_attacks.jsonl")
        .iter()
        .map(|r| {
            (
                r["true_vehicle_id"].as_str().expect("id").to_string(),
                (
                    r["start_time"].as_f64().unwrap_or(0.0),
                    r["end_time"].as_f64().unwrap_or(f64::MAX),
                ),
            )
        })
        .collect();
    let falsified: Vec<&serde_json::Value> = a
        .rows("ground_truth/gt_emissions_sample.jsonl")
        .iter()
        .filter(|e| e["falsified"].as_bool().unwrap_or(false))
        .collect();
    assert!(
        !falsified.is_empty(),
        "no falsified emission, so the check would be vacuous"
    );
    const EPS: f64 = 0.05;
    for e in falsified {
        let v = e["true_vehicle_id"].as_str().expect("id");
        assert!(
            attackers.contains(v),
            "a benign device {v} emitted a falsified message"
        );
        if let Some((from, to)) = windows.get(v) {
            let t = e["t"].as_f64().unwrap_or(0.0);
            assert!(
                t >= from - EPS && t <= to + EPS,
                "a falsified emission from {v} at {t} is outside its window [{from}, {to}]"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// G. File integrity
// ---------------------------------------------------------------------------------------

/// `I1_file_digests_match_manifest` and `I2_aggregate_data_digest`, recomputed the way the
/// frozen audit recomputes them.
#[test]
fn i1_and_i2_the_file_digests_and_the_aggregate_digest_match_the_manifest() {
    let a = audit(
        DatasetProfile::V1,
        "i1_and_i2_the_file_digests_and_the_aggregate_digest_match_the_manifest",
    );
    assert!(!a.written.manifest.outputs.is_empty());
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    for f in &a.written.manifest.outputs {
        let path = a.root.join(&f.path);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", f.path));
        let actual = v2xw_core::hash::sha256_hex(&bytes);
        assert_eq!(actual, f.sha256, "I1: {} digest differs", f.path);
        digests.insert(f.path.clone(), actual);
    }
    // The aggregate: sha256 over each path's bytes then its digest's *hex text*, in path
    // order. Recomputed here from the files on disk rather than from the manifest's own
    // list, so a manifest that agreed with itself but not with the files would fail.
    let mut buf = Vec::new();
    for (rel, hex) in &digests {
        buf.extend_from_slice(rel.as_bytes());
        buf.extend_from_slice(hex.as_bytes());
    }
    assert_eq!(
        v2xw_core::hash::sha256_hex(&buf),
        a.written.manifest.data_digest_sha256,
        "I2"
    );
}

// ---------------------------------------------------------------------------------------
// The contract §6 lists beyond the audit's checks
// ---------------------------------------------------------------------------------------

/// §6: "file set … sort orders … id formats".
#[test]
fn every_table_is_written_in_its_legacy_sort_order() {
    let a = audit(
        DatasetProfile::V1,
        "every_table_is_written_in_its_legacy_sort_order",
    );
    let sorted_by = |rel: &str, key: &dyn Fn(&serde_json::Value) -> String| {
        let rows = a.rows(rel);
        let keys: Vec<String> = rows.iter().map(key).collect();
        let mut want = keys.clone();
        want.sort();
        assert_eq!(keys, want, "{rel} is not in its declared sort order");
        assert!(
            !rows.is_empty(),
            "{rel} is empty, so its order check was vacuous"
        );
    };
    let s = |r: &serde_json::Value, f: &str| r[f].as_str().unwrap_or_default().to_string();
    let t = |r: &serde_json::Value, f: &str| format!("{:020.6}", r[f].as_f64().unwrap_or(0.0));

    sorted_by("ma/ma_reports.jsonl", &|r| {
        format!("{}|{}", t(r, "ingest_time"), s(r, "report_id"))
    });
    sorted_by("ma/ma_cert_status.jsonl", &|r| s(r, "cert_digest"));
    sorted_by("ma/ma_investigations.jsonl", &|r| {
        format!("{}|{}", t(r, "opened_time"), s(r, "case_id"))
    });
    sorted_by("ma/ma_crl_events.jsonl", &|r| {
        format!("{}|{}", t(r, "issue_time"), s(r, "crl_id"))
    });
    sorted_by("ground_truth/gt_vehicle.jsonl", &|r| {
        s(r, "true_vehicle_id")
    });
    sorted_by("ground_truth/gt_identity_map.jsonl", &|r| {
        s(r, "pseudonym_cert_digest")
    });
    sorted_by("ground_truth/gt_attacks.jsonl", &|r| s(r, "attack_id"));
    sorted_by("ground_truth/gt_report_labels.jsonl", &|r| {
        s(r, "report_id")
    });
    sorted_by("ground_truth/gt_linkage_revocation.jsonl", &|r| {
        s(r, "true_vehicle_id")
    });
    sorted_by("ground_truth/gt_emissions_sample.jsonl", &|r| {
        s(r, "emit_id")
    });
}

/// §6: "id formats".
#[test]
fn the_id_formats_are_the_legacy_ones() {
    let a = audit(DatasetProfile::V1, "the_id_formats_are_the_legacy_ones");
    for id in a.strs("ma/ma_reports.jsonl", "report_id") {
        assert!(
            id.starts_with("rpt_") && id.len() == 9 && id[4..].chars().all(|c| c.is_ascii_digit()),
            "report id {id:?} is not rpt_NNNNN"
        );
    }
    for id in a.strs("ground_truth/gt_vehicle.jsonl", "true_vehicle_id") {
        assert!(id.starts_with("veh_"), "device id {id:?} is not veh_NNN");
        assert!(id[4..].chars().all(|c| c.is_ascii_digit()));
        assert!(
            id.len() >= 7,
            "device id {id:?} is narrower than three digits"
        );
    }
    for id in a.strs("ma/ma_investigations.jsonl", "case_id") {
        assert!(id.starts_with("case_") && id.len() == 9, "case id {id:?}");
    }
    for id in a.strs("ma/ma_crl_events.jsonl", "crl_id") {
        assert!(id.starts_with("crl_") && id.len() == 8, "crl id {id:?}");
    }
    for id in a.strs("ground_truth/gt_emissions_sample.jsonl", "emit_id") {
        assert!(id.starts_with("emt_") && id.len() == 12, "emit id {id:?}");
    }
    for id in a.strs("ground_truth/gt_attacks.jsonl", "attack_id") {
        assert!(id.starts_with("atk_"), "attack id {id:?}");
    }
}

/// §6: "canonical JSON bytes". Every line is sorted-key, whitespace-free JSON, and
/// re-encoding a parsed line reproduces it exactly.
#[test]
fn every_line_is_canonical_json_and_re_encodes_to_itself() {
    let a = audit(
        DatasetProfile::V1,
        "every_line_is_canonical_json_and_re_encodes_to_itself",
    );
    let mut lines = 0usize;
    for (rel, _) in DatasetWriter::layout() {
        let path = a.root.join(rel);
        if !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable");
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            assert_eq!(
                v2xw_record::dataset::pyjson::canonical(&value),
                line,
                "{rel}: the line is not its own canonical encoding"
            );
            assert!(!line.contains(", "), "{rel}: whitespace after a separator");
            assert!(!line.contains(": "), "{rel}: whitespace after a colon");
            assert!(line.is_ascii(), "{rel}: ensure_ascii was not applied");
            lines += 1;
        }
        // Unix line endings, and a trailing newline on a non-empty file.
        assert!(!text.contains('\r'), "{rel}: CRLF line endings");
        if !text.is_empty() {
            assert!(text.ends_with('\n'), "{rel}: no trailing newline");
        }
    }
    assert!(lines > 50, "only {lines} lines checked");
}

/// §6: the v1 defects are present, because a consumer that pinned the schema depends on
/// them. A test that the exporter *has* a defect looks odd until you remember that the
/// alternative is silently changing a published schema.
#[test]
fn the_v1_profile_reproduces_the_legacy_defects_rather_than_quietly_improving_them() {
    let a = audit(
        DatasetProfile::V1,
        "the_v1_profile_reproduces_the_legacy_defects_rather_than_quietly_improving_them",
    );
    let certs = a.rows("ma/ma_cert_status.jsonl");
    assert!(!certs.is_empty());
    for c in certs {
        assert_eq!(c["valid_from"], 0.0, "the constant valid_from defect");
        assert!(
            ["active", "revoked"].contains(&c["crl_status"].as_str().expect("status")),
            "v1 never writes `unknown`"
        );
    }
    let reports = a.rows("ma/ma_reports.jsonl");
    assert!(!reports.is_empty());
    for r in reports {
        assert_eq!(
            r["cert_crl_status"], "active",
            "the constant cert_crl_status defect"
        );
        let cv = &r["cert_validity"];
        for flag in ["sig_valid", "not_expired", "not_revoked", "chain_ok"] {
            assert_eq!(cv[flag], true, "the all-true cert_validity defect: {flag}");
        }
        // …and no v2 column has leaked into v1.
        for v2_only in [
            "verification_status",
            "verify_latency_ms",
            "rx_rssi_dbm",
            "rat",
        ] {
            assert!(
                r.get(v2_only).is_none(),
                "{v2_only} leaked into the v1 profile"
            );
        }
    }
    let cases = a.rows("ma/ma_investigations.jsonl");
    assert!(!cases.is_empty());
    for c in cases {
        assert_eq!(
            c["revocation_decision"], "revoke",
            "v1 writes a case only where the authority revoked"
        );
    }
}

/// §6: "additive columns only". Every v1 column survives into v2, with v2 adding and never
/// removing — which is the whole promise of the profile.
#[test]
fn the_v2_profile_is_additive_over_v1() {
    let v1 = audit(DatasetProfile::V1, "the_v2_profile_is_additive_over_v1");
    let v2 = audit(DatasetProfile::V2, "the_v2_profile_is_additive_over_v1");
    let columns = |a: &Audit, rel: &str| -> BTreeSet<String> {
        a.rows(rel)
            .iter()
            .filter_map(|r| r.as_object())
            .flat_map(|o| o.keys().cloned())
            .collect()
    };
    for rel in [
        "ma/ma_reports.jsonl",
        "ma/ma_cert_status.jsonl",
        "ma/ma_investigations.jsonl",
        "ma/ma_crl_events.jsonl",
        "ground_truth/gt_vehicle.jsonl",
        "ground_truth/gt_emissions_sample.jsonl",
    ] {
        let a = columns(&v1, rel);
        let b = columns(&v2, rel);
        assert!(!a.is_empty(), "{rel} has no v1 columns to compare");
        let missing: Vec<&String> = a.difference(&b).collect();
        assert!(missing.is_empty(), "{rel}: v2 dropped {missing:?}");
    }
    // …and the four §6 additions really are there.
    let reports = columns(&v2, "ma/ma_reports.jsonl");
    for added in [
        "verification_status",
        "verify_latency_ms",
        "rx_rssi_dbm",
        "rat",
    ] {
        assert!(reports.contains(added), "v2 is missing {added}");
    }
    assert!(
        columns(&v2, "ma/ma_crl_events.jsonl").contains("num_entries_delta"),
        "v2 is missing num_entries_delta"
    );
    // The v1 defects are fixed in v2.
    assert!(
        v2.rows("ma/ma_cert_status.jsonl")
            .iter()
            .any(|c| c["valid_from"].as_f64() != Some(0.0)),
        "v2 still writes the constant validity window"
    );
}

/// The `standards_profile` and `schema_versions` keys the audit reads.
#[test]
fn the_manifest_carries_the_keys_the_frozen_audit_reads_by_name() {
    let a = audit(
        DatasetProfile::V1,
        "the_manifest_carries_the_keys_the_frozen_audit_reads_by_name",
    );
    let text = std::fs::read_to_string(a.root.join("manifest.json")).expect("manifest");
    let m: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    for key in [
        "config",
        "counts",
        "outputs",
        "data_digest_sha256",
        "schema_versions",
        "standards_profile",
        "dataset_version",
        "generator",
        "seed",
        "build_utc",
    ] {
        assert!(m.get(key).is_some(), "the manifest has no {key}");
    }
    assert_eq!(m["schema_versions"]["ma_visible"], 1);
    assert_eq!(m["schema_versions"]["ground_truth"], 1);
    assert_eq!(m["standards_profile"]["cert"], "IEEE 1609.2");
    assert_eq!(m["standards_profile"]["linkage"], "CAMP SCP2");
    assert!(
        m["standards_profile"]["report"]
            .as_str()
            .expect("report")
            .contains("103 759")
    );
    // Each output is {path, sha256}, which is the shape the audit's `pairs` handling wants.
    for o in m["outputs"].as_array().expect("outputs is a list") {
        assert!(o["path"].is_string());
        assert_eq!(o["sha256"].as_str().expect("sha256").len(), 64);
    }
}

/// The datasheet ships with the dataset and carries the lint verdict — "a dataset without
/// a datasheet is not publishable".
#[test]
fn the_datasheet_ships_with_the_dataset_and_carries_its_provenance() {
    let a = audit(
        DatasetProfile::V1,
        "the_datasheet_ships_with_the_dataset_and_carries_its_provenance",
    );
    let sheet = std::fs::read_to_string(a.root.join("DATASHEET.md")).expect("DATASHEET.md");
    assert!(
        sheet.contains(&a.written.manifest.data_digest_sha256),
        "no data digest"
    );
    assert!(sheet.contains("0xc0ffee5eed"), "no seed");
    assert!(sheet.contains(&a.written.lint.summary()), "no lint verdict");
    assert!(
        sheet.contains("radio/propagation/log-distance"),
        "no model cards"
    );
    assert!(sheet.contains("PASS"), "the lint verdict is not stated");
    assert!(sheet.contains("## Known defects of this profile"));
    // Every digested file is listed, so a reviewer can check one by hand.
    for f in &a.written.manifest.outputs {
        assert!(
            sheet.contains(&f.path),
            "{} is not in the datasheet",
            f.path
        );
    }
}

/// An all-benign run: the edge case where every check still has to hold and several of
/// them have nothing to find. A suite that only ever sees a rich fixture does not know
/// whether it is asserting anything.
#[test]
fn an_all_benign_run_still_satisfies_every_invariant() {
    let ds = dataset(&DatasetShape::all_benign(), DatasetProfile::V1).expect("assemble");
    let a = write_audit(
        &ds,
        DatasetProfile::V1,
        "an_all_benign_run_still_satisfies_every_invariant",
    );
    assert!(a.written.lint.is_clean());
    assert_eq!(a.written.manifest.counts["revoked"], 0);
    for l in a.rows("ground_truth/gt_report_labels.jsonl") {
        assert_eq!(
            l["report_correctness"], "false_positive",
            "with no attacker and no fault, every report is a false positive"
        );
    }
    assert!(
        a.rows("ground_truth/gt_emissions_sample.jsonl")
            .iter()
            .all(|e| e["falsified"] == false)
    );
    assert!(
        a.rows("ma/ma_investigations.jsonl").is_empty(),
        "nothing to investigate"
    );
}
