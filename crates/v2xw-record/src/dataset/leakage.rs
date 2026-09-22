//! The leakage linter — the port of `scms_sim_ref/datagen/leakage_linter.py` and the
//! registry it reads (`scms_sim_ref/schemas/records.py`).
//!
//! 08-measurement-and-data.md §6: "the linter runs on every exporter output in CI; the
//! forbidden-key rules are unchanged and extended with `gt_`-prefixed names from the new
//! tables." This module is the unchanged rules plus that extension, and it is the one
//! scientific-validity guarantee of the whole dataset family:
//!
//! > No ground-truth column ever reaches a node-visible profile.
//!
//! # Why a name registry and not a type discipline
//!
//! A type discipline is the better tool and this crate already has one — a `GT` channel is
//! withheld whole by [`crate::profile`], and a mixed channel's ground-truth *columns* are
//! projected out by [`crate::export::schema::TableSchema::without_ground_truth`]. The
//! registry catches what a type discipline cannot: a column that arrives from a plug-in,
//! from a Python provider, from a hand-built table, or from a well-meaning addition to a
//! record struct that nobody remembered to tag. It is a second, independent gate over the
//! bytes that were actually written, and it is deliberately conservative — it inspects
//! keys recursively through objects and arrays and reports a hit at any depth, because the
//! leak F2MD shipped (`senderRealId` inside a nested detector output) was exactly that.
//!
//! # The two halves of a leak
//!
//! A forbidden **key** is what the legacy linter checks. It is not sufficient: a table
//! could carry the real vehicle id under an innocent name. So [`lint_dataset`] also runs
//! the legacy audit's `L3` check — no true-identity *value* appears anywhere in an
//! MA-visible file — and both halves have to pass.

use std::collections::{BTreeMap, BTreeSet};

/// The visibility tag a legacy record carries in its `_visibility` field.
pub const MA: &str = "MA";
/// The tag of a record everyone may see, such as a CRL event.
pub const PUBLIC: &str = "PUBLIC";
/// The tag of a ground-truth record: labels and evaluation only, never a feature.
pub const ORACLE: &str = "ORACLE";

/// The exact-match half of the forbidden-key registry.
///
/// This is `FORBIDDEN_FEATURE_KEYS` from `schemas/records.py`, unchanged, in sorted order.
/// The two F2MD spellings are in it verbatim because they are the fields that corpus
/// actually leaked, and a case-insensitive rule alone would not have caught
/// `senderRealId` in 2020 either.
pub const FORBIDDEN_FEATURE_KEYS: &[&str] = &[
    "attack_family",
    "attack_id",
    "attack_type",
    "attacker_role",
    "colluding_group_id",
    "falsified",
    "is_attacker",
    "is_fake",
    "is_faulty",
    "is_vru",
    "real_id",
    "realid",
    "report_correctness",
    "reported_real_id",
    "reportedRealId",
    "reporter_true_id",
    "sender_real_id",
    "senderRealId",
    "should_have_been_revoked",
    "subject_true_id",
    "true_accel",
    "true_heading",
    "true_linkage_seed",
    "true_revocation_time",
    "true_speed",
    "true_vehicle_id",
    "true_x",
    "true_y",
];

/// True if `key` names ground truth and must never appear in a node-visible table.
///
/// The rules, in the order the legacy predicate tries them:
///
/// 1. Leading underscores are stripped first. A leaf column literally named
///    `_is_attacker` must not slip past by hiding behind the `_visibility` convention,
///    and `_visibility` itself still passes because it is not in the registry.
/// 2. An exact match against [`FORBIDDEN_FEATURE_KEYS`].
/// 3. Lower-cased prefix rules: `true_`, `label_`, `attack_`.
/// 4. Lower-cased suffix rules: `_true_id`, `real_id`.
/// 5. Separator-insensitive suffix rules, which catch camelCase: a name whose letters end
///    in `realid` or `trueid` once `_` is removed (`driverRealId`, `vehicleTrueId`).
/// 6. Two literals the legacy registry lists separately: `realid`, `vehicleid_true`.
///
/// The extension 08-measurement-and-data.md §6 asks for is rule 7: a `gt_` prefix, which
/// is how the v2 profile's new ground-truth tables name their columns.
///
/// Near misses matter as much as hits: `reporter_cert_digest`, `serial_id`, `trailer_id`
/// and `is_vru_declared` are all legitimate MA-visible signals and all pass. The last is
/// the subtlest — `is_vru` is the oracle label and `is_vru_declared` is what the MA
/// actually observed on the air — which is why `is_vru` is an exact-match rule and not a
/// prefix one.
#[must_use]
pub fn is_forbidden_feature_key(key: &str) -> bool {
    let k = key.trim().trim_start_matches('_');
    if FORBIDDEN_FEATURE_KEYS.contains(&k) {
        return true;
    }
    let lk = k.to_ascii_lowercase();
    let norm: String = lk.chars().filter(|c| *c != '_').collect();
    lk.starts_with("true_")
        || lk.starts_with("label_")
        || lk.starts_with("attack_")
        || lk.starts_with("gt_")
        || lk.ends_with("_true_id")
        || lk.ends_with("real_id")
        || norm.ends_with("realid")
        || norm.ends_with("trueid")
        || lk == "realid"
        || lk == "vehicleid_true"
}

/// One thing the linter found.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LeakageViolation {
    /// The file the row was written to, relative to the dataset root.
    pub file: String,
    /// The row index within that file.
    pub row: usize,
    /// The dotted path of the offending key, or of the field whose value leaked.
    pub path: String,
    /// What is wrong.
    pub kind: ViolationKind,
    /// The detail the message carries.
    pub detail: String,
}

/// Which of the linter's gates a violation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ViolationKind {
    /// A key in [`FORBIDDEN_FEATURE_KEYS`] or matching one of the name rules.
    ForbiddenKey,
    /// The record's own `_visibility` is `ORACLE`, so it may not be in this file at all.
    OracleVisibility,
    /// A true-identity *value* appeared in an MA-visible file (the legacy audit's `L3`).
    IdentityValue,
    /// A column its channel declares ground truth, whatever its name suggests.
    TaggedGroundTruth,
}

impl core::fmt::Display for ViolationKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            ViolationKind::ForbiddenKey => "forbidden key",
            ViolationKind::OracleVisibility => "ORACLE-visibility record",
            ViolationKind::IdentityValue => "true-identity value",
            ViolationKind::TaggedGroundTruth => "channel-tagged ground-truth column",
        })
    }
}

impl core::fmt::Display for LeakageViolation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}[{}] {}: {} ({})",
            self.file, self.row, self.kind, self.path, self.detail
        )
    }
}

/// What a lint run looked at, and what it found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeakageReport {
    /// Files inspected.
    pub files: usize,
    /// Rows inspected.
    pub rows: usize,
    /// Keys inspected, at every depth.
    pub keys: usize,
    /// Every violation, sorted, so the message is reproducible.
    pub violations: Vec<LeakageViolation>,
}

impl LeakageReport {
    /// True if nothing was found. The gate an exporter refuses on.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }

    /// The one-line summary the datasheet's "Label leakage prevention" section carries.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_clean() {
            format!(
                "PASS — {} keys across {} rows in {} node-visible files; no forbidden key, \
                 no ORACLE record, no true-identity value",
                self.keys, self.rows, self.files
            )
        } else {
            format!(
                "FAIL — {} violation(s) across {} node-visible files: {}",
                self.violations.len(),
                self.files,
                self.violations
                    .iter()
                    .take(4)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        }
    }

    fn merge(&mut self, other: LeakageReport) {
        self.files += other.files;
        self.rows += other.rows;
        self.keys += other.keys;
        self.violations.extend(other.violations);
        self.violations.sort();
    }
}

/// Collects every key in a JSON value, recursively, as dotted paths.
fn walk_keys(value: &serde_json::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                let here = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                out.push((k.clone(), here.clone()));
                walk_keys(v, &here, out);
            }
        }
        serde_json::Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                walk_keys(v, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// Every string value in a JSON value, for the identity-value check.
fn walk_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Object(m) => {
            for v in m.values() {
                walk_strings(v, out);
            }
        }
        serde_json::Value::Array(a) => {
            for v in a {
                walk_strings(v, out);
            }
        }
        _ => {}
    }
}

/// Lints one node-visible table's rows.
///
/// `file` is only used in the messages. A row whose `_visibility` is `ORACLE` is a
/// violation on its own, before any key is looked at: the legacy `assert_ma_visible`
/// refused such a record outright, because a correctly-tagged ground-truth record in an
/// MA file means the firewall was bypassed rather than that the tagging was wrong.
#[must_use]
pub fn lint_rows(file: &str, rows: &[serde_json::Value]) -> LeakageReport {
    let mut report = LeakageReport {
        files: 1,
        rows: rows.len(),
        ..Default::default()
    };
    for (i, row) in rows.iter().enumerate() {
        if row.get("_visibility").and_then(serde_json::Value::as_str) == Some(ORACLE) {
            report.violations.push(LeakageViolation {
                file: file.to_string(),
                row: i,
                path: "_visibility".to_string(),
                kind: ViolationKind::OracleVisibility,
                detail: "an ORACLE record must not appear in a node-visible file".to_string(),
            });
        }
        let mut keys = Vec::new();
        walk_keys(row, "", &mut keys);
        report.keys += keys.len();
        for (key, path) in keys {
            if is_forbidden_feature_key(&key) {
                report.violations.push(LeakageViolation {
                    file: file.to_string(),
                    row: i,
                    path,
                    kind: ViolationKind::ForbiddenKey,
                    detail: format!("{key:?} names ground truth"),
                });
            }
        }
    }
    report.violations.sort();
    report
}

/// Lints one node-visible table's rows **against its channel's declared ground-truth
/// columns** as well as against the name registry.
///
/// The two gates catch different things and both are needed:
///
/// * The **name registry** catches a column whose *name* says it is ground truth —
///   `true_speed`, `is_attacker`, `senderRealId`. It works on a table from anywhere,
///   including one a plug-in or a Python provider built, and it needs no schema.
/// * The **channel tag** catches a column that is ground truth without saying so.
///   `phy.rx` carries the transmitter's identity as `tx` and the true transmitter-to-
///   receiver distance as `dist_m`. Neither name matches any rule in the registry, and
///   neither should: `tx` is not a suspicious name. They are ground truth because
///   03-interfaces.md §14 says the channel's transmitter id and distance are, and
///   [`crate::export::schema::ground_truth_fields`] is where that list lives.
///
/// A linter with only the first gate reports a receiver log carrying `tx` and `dist_m` as
/// clean. That is not a hypothetical: the ground-truth column list for `phy.rx` once held
/// only the prose spellings `tx_node` and `distance_m`, so the visibility projection
/// matched nothing on a real record, and both the projection and the grid scan reported
/// success over a file holding the transmitter's identity.
#[must_use]
pub fn lint_rows_of_channel(
    file: &str,
    channel: &str,
    rows: &[serde_json::Value],
) -> LeakageReport {
    let mut report = lint_rows(file, rows);
    let declared: BTreeSet<&str> = crate::export::schema::ground_truth_fields(channel)
        .iter()
        .copied()
        .collect();
    for (i, row) in rows.iter().enumerate() {
        let Some(obj) = row.as_object() else { continue };
        for key in obj.keys() {
            if declared.contains(key.as_str()) {
                report.violations.push(LeakageViolation {
                    file: file.to_string(),
                    row: i,
                    path: key.clone(),
                    kind: ViolationKind::TaggedGroundTruth,
                    detail: format!(
                        "{channel}.{key} is declared a ground-truth column of its channel, so                          it may not appear in a node-visible file whatever its name suggests"
                    ),
                });
            }
        }
    }
    report.violations.sort();
    report.violations.dedup();
    report
}

/// [`lint_columns`] with the channel tag as well — see [`lint_rows_of_channel`].
#[must_use]
pub fn lint_channel_columns(file: &str, channel: &str, columns: &[String]) -> LeakageReport {
    let mut report = lint_columns(file, columns);
    let declared: BTreeSet<&str> = crate::export::schema::ground_truth_fields(channel)
        .iter()
        .copied()
        .collect();
    for name in columns {
        if declared.contains(name.as_str()) {
            report.violations.push(LeakageViolation {
                file: file.to_string(),
                row: 0,
                path: name.clone(),
                kind: ViolationKind::TaggedGroundTruth,
                detail: format!("{channel}.{name} is a declared ground-truth column"),
            });
        }
    }
    report.violations.sort();
    report.violations.dedup();
    report
}

/// Lints a column *name list* — a Parquet or CSV header, where there are no values to
/// walk.
///
/// This is the legacy audit's `L1`, and it is the gate the v2 profile's new feature
/// columns go through.
#[must_use]
pub fn lint_columns(file: &str, columns: &[String]) -> LeakageReport {
    let mut report = LeakageReport {
        files: 1,
        keys: columns.len(),
        ..Default::default()
    };
    for name in columns {
        if is_forbidden_feature_key(name) {
            report.violations.push(LeakageViolation {
                file: file.to_string(),
                row: 0,
                path: name.clone(),
                kind: ViolationKind::ForbiddenKey,
                detail: format!("{name:?} names ground truth"),
            });
        }
    }
    report.violations.sort();
    report
}

/// The whole-dataset lint: every node-visible file's keys, plus the identity-value check.
///
/// `node_visible` maps each file's relative path to its rows. `true_identities` is every
/// true id the ground-truth tables carry; if any of them appears as a *value* anywhere in
/// a node-visible file, the separation has failed even though no key is forbidden. That is
/// the legacy audit's `L3`, and it is the check that catches a real id smuggled under an
/// innocent column name.
#[must_use]
pub fn lint_dataset(
    node_visible: &BTreeMap<String, Vec<serde_json::Value>>,
    true_identities: &BTreeSet<String>,
) -> LeakageReport {
    let mut report = LeakageReport::default();
    for (file, rows) in node_visible {
        report.merge(lint_rows(file, rows));
        for (i, row) in rows.iter().enumerate() {
            let mut strings = Vec::new();
            walk_strings(row, &mut strings);
            for s in strings {
                if true_identities.contains(&s) {
                    report.violations.push(LeakageViolation {
                        file: file.clone(),
                        row: i,
                        path: s.clone(),
                        kind: ViolationKind::IdentityValue,
                        detail: format!(
                            "the true identity {s:?} appears as a value in a node-visible file"
                        ),
                    });
                }
            }
        }
    }
    report.violations.sort();
    report.violations.dedup();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The legacy predicate's own test vectors, verbatim from
    /// `legacy/tests/test_leakage.py::test_forbidden_key_predicate`.
    #[test]
    fn the_legacy_predicate_is_reproduced_key_for_key() {
        for k in [
            "true_vehicle_id",
            "senderRealId",
            "reportedRealId",
            "true_x",
            "is_attacker",
            "attack_type",
            "reporter_true_id",
            "real_id",
        ] {
            assert!(is_forbidden_feature_key(k), "{k} must be forbidden");
        }
        for k in [
            "reporter_cert_digest",
            "subject_cert_digest",
            "reason_codes",
            "score",
        ] {
            assert!(!is_forbidden_feature_key(k), "{k} must be allowed");
        }
    }

    #[test]
    fn an_underscore_prefixed_leaf_does_not_bypass_the_rule() {
        assert!(is_forbidden_feature_key("_is_attacker"));
        assert!(is_forbidden_feature_key("__true_vehicle_id"));
        assert!(
            !is_forbidden_feature_key("_visibility"),
            "the legitimate visibility field still passes"
        );
    }

    #[test]
    fn camel_case_identity_fields_are_caught_and_near_misses_are_not() {
        for k in [
            "driverRealId",
            "ownerRealId",
            "vehicleTrueId",
            "subjectTrueId",
        ] {
            assert!(is_forbidden_feature_key(k), "{k}");
        }
        for k in ["serialId", "trailerId", "reporter_cert_digest"] {
            assert!(!is_forbidden_feature_key(k), "{k}");
        }
    }

    #[test]
    fn the_ma_visible_vru_declaration_survives_while_the_oracle_label_does_not() {
        // The subtlest pair in the registry: what the MA observed on the air is a
        // legitimate feature; the oracle's answer is not.
        assert!(is_forbidden_feature_key("is_vru"));
        assert!(!is_forbidden_feature_key("is_vru_declared"));
        assert!(!is_forbidden_feature_key("n_denms_implausible"));
    }

    #[test]
    fn the_v2_extension_catches_the_new_ground_truth_table_prefix() {
        // 08-measurement-and-data.md §6's extension.
        assert!(is_forbidden_feature_key("gt_revocation_stage"));
        assert!(is_forbidden_feature_key("gt_kinematics_sample"));
        assert!(!is_forbidden_feature_key("gateway_id"));
    }

    #[test]
    fn a_nested_leak_is_found_at_any_depth() {
        let rows = vec![json!({
            "report_id": "rpt_00001",
            "detector_outputs": [{"check_id": "positionJump", "senderRealId": "veh_042"}],
        })];
        let report = lint_rows("ma/ma_reports.jsonl", &rows);
        assert!(!report.is_clean());
        assert_eq!(report.violations.len(), 1);
        assert_eq!(
            report.violations[0].path,
            "detector_outputs[0].senderRealId"
        );
    }

    #[test]
    fn an_oracle_record_in_an_ma_file_is_refused_on_its_tag_alone() {
        let rows = vec![json!({"cert_digest": "aabb", "_visibility": ORACLE})];
        let report = lint_rows("ma/ma_cert_status.jsonl", &rows);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, ViolationKind::OracleVisibility);
    }

    #[test]
    fn an_identity_value_under_an_innocent_key_is_still_a_leak() {
        // No forbidden key anywhere — the column is called `case_note` — and the value is
        // a true vehicle id. The key rules alone would pass this.
        let mut files = BTreeMap::new();
        files.insert(
            "ma/ma_investigations.jsonl".to_string(),
            vec![json!({"case_id": "case_1", "case_note": "veh_042"})],
        );
        let ids: BTreeSet<String> = ["veh_042".to_string()].into_iter().collect();
        assert!(lint_rows("x", &[json!({"case_note": "veh_042"})]).is_clean());
        let report = lint_dataset(&files, &ids);
        assert!(!report.is_clean());
        assert_eq!(report.violations[0].kind, ViolationKind::IdentityValue);
    }

    #[test]
    fn a_clean_report_row_passes_and_the_summary_says_what_was_checked() {
        let rows = vec![json!({
            "report_id": "rpt_00001",
            "ingest_time": 10.0,
            "reporter_cert_digest": "aabbccddeeff0011",
            "subject_cert_digest": "1100ffeeddccbbaa",
            "reason_codes": ["positionPlausibility"],
            "detector_outputs": [{"check_id": "rangePlausibility", "score": 0.9}],
            "cert_validity": {"sig_valid": true, "chain_ok": true},
            "_visibility": MA,
        })];
        let report = lint_rows("ma/ma_reports.jsonl", &rows);
        assert!(report.is_clean(), "{}", report.summary());
        assert!(report.summary().starts_with("PASS"));
        assert!(report.keys > 0);
    }

    #[test]
    fn a_column_header_list_is_linted_too() {
        let bad = lint_columns(
            "ml/report_features.csv",
            &["detector_score".to_string(), "true_speed".to_string()],
        );
        assert_eq!(bad.violations.len(), 1);
        let good = lint_columns(
            "ml/report_features.csv",
            &["detector_score".to_string(), "n_reporters".to_string()],
        );
        assert!(good.is_clean());
    }
}
