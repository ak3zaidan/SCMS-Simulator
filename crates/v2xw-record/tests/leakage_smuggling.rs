//! Trying to smuggle a ground-truth column into a node-visible profile, seven ways.
//!
//! A check that cannot go red is not a check. The leakage linter's whole value is that it
//! refuses; a linter nobody has ever seen refuse is an assertion that the code is correct,
//! dressed up as a test. So each test here **injects the fault** and asserts the refusal,
//! and every one of them was watched failing before the guard that catches it was written.
//!
//! The seven routes, which are the seven ways this has actually gone wrong in a V2X
//! dataset:
//!
//! | # | Route | Caught by |
//! |---|---|---|
//! | 1 | a top-level `true_vehicle_id` column | the name registry |
//! | 2 | a nested `senderRealId`, F2MD's exact leak | the recursive walk |
//! | 3 | an `_is_attacker` column hiding behind the underscore convention | the underscore strip |
//! | 4 | a ground-truth score smuggled through the open `detnorm_*` map | the name registry, at the map's key |
//! | 5 | a true identity as a *value* under an innocent column name | the identity-value check |
//! | 6 | an `ORACLE`-tagged row dropped into an MA file | the visibility tag |
//! | 7 | `phy.rx`'s `tx` and `dist_m` in a receiver log — ground truth with innocent names | the channel's declared column list |
//!
//! Route 7 is the one a name registry alone cannot catch, and it is the one that was
//! actually live in this crate: `ground_truth_fields("phy.rx")` listed the prose spellings
//! `tx_node` and `distance_m` while the record the engine emits spells them `tx` and
//! `dist_m`, so the `NODE-only` projection matched nothing and a receiver log carried the
//! transmitter's identity and the true distance with the grid scan and the profile both
//! reporting success.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_record::dataset::fixture::{DatasetShape, dataset, provenance};
use v2xw_record::dataset::leakage::{
    ORACLE, ViolationKind, lint_dataset, lint_rows, lint_rows_of_channel,
};
use v2xw_record::dataset::{DatasetProfile, DatasetWriter, LogProfile, profiles};
use v2xw_record::export::ExportFormat;
use v2xw_record::export::schema::{TableSchema, ground_truth_fields};
use v2xw_record::fixture::scratch_dir;
use v2xw_record::reader::RecordedRecord;

fn clean_report() -> serde_json::Value {
    serde_json::json!({
        "report_id": "rpt_00001",
        "ingest_time": 10.0,
        "detection_time": 9.5,
        "reporter_cert_digest": "aabbccddeeff0011",
        "subject_cert_digest": "1100ffeeddccbbaa",
        "reason_codes": ["positionJump"],
        "detector_outputs": [{"check_id": "positionJump", "score": 1.5, "verdict": "fail"}],
        "cert_validity": {"sig_valid": true, "not_expired": true, "not_revoked": true, "chain_ok": true},
        "evidence_msg_refs": ["rpt_00001-m"],
        "st_bbox": [0.0, 0.0, 1.0, 1.0],
        "st_tstart": 9.5, "st_tend": 9.5, "duplicate_flag": false,
        "detector_score": 1.5, "detector_score_norm": 1.5,
        "subject_pos_confidence": 1.0, "cert_crl_status": "active", "sig_valid": true,
        "detnorm_positionJump": 1.5,
        "_visibility": "MA",
    })
}

/// The control: the clean row passes. Without this, a linter that refused everything would
/// look like a linter that works.
#[test]
fn the_control_row_passes_so_a_refusal_means_something() {
    let report = lint_rows("ma/ma_reports.jsonl", &[clean_report()]);
    assert!(report.is_clean(), "{}", report.summary());
    assert!(
        report.keys > 15,
        "the linter looked at {} keys",
        report.keys
    );
    assert!(report.summary().starts_with("PASS"));
}

/// Route 1: the classic.
#[test]
fn route_1_a_top_level_true_vehicle_id_is_refused() {
    let mut row = clean_report();
    row["true_vehicle_id"] = serde_json::json!("veh_042");
    let report = lint_rows("ma/ma_reports.jsonl", &[row]);
    assert!(!report.is_clean(), "the classic leak was not caught");
    assert_eq!(report.violations[0].kind, ViolationKind::ForbiddenKey);
    assert_eq!(report.violations[0].path, "true_vehicle_id");
}

/// Route 2: F2MD's exact leak, three levels down.
#[test]
fn route_2_a_nested_sender_real_id_is_refused_at_any_depth() {
    let mut row = clean_report();
    row["detector_outputs"][0]["senderRealId"] = serde_json::json!("veh_042");
    let report = lint_rows("ma/ma_reports.jsonl", &[row]);
    assert!(!report.is_clean(), "F2MD's leak was not caught");
    assert_eq!(
        report.violations[0].path,
        "detector_outputs[0].senderRealId"
    );

    // …and one more level down, inside an object inside an array.
    let mut row = clean_report();
    row["detector_outputs"][0]["detail"] = serde_json::json!({"inner": {"reportedRealId": 7}});
    let report = lint_rows("ma/ma_reports.jsonl", &[row]);
    assert!(!report.is_clean());
    assert!(
        report.violations[0].path.ends_with("reportedRealId"),
        "{:?}",
        report.violations[0].path
    );
}

/// Route 3: hiding behind the `_visibility` underscore convention.
#[test]
fn route_3_an_underscore_disguised_label_is_refused_while_the_real_metadata_field_passes() {
    let mut row = clean_report();
    row["_is_attacker"] = serde_json::json!(1);
    let report = lint_rows("ma/ma_reports.jsonl", &[row]);
    assert!(!report.is_clean(), "the underscore disguise worked");
    assert_eq!(report.violations[0].path, "_is_attacker");
    // The legitimate underscore field is still fine — otherwise the rule would be useless.
    assert!(lint_rows("x", &[clean_report()]).is_clean());
}

/// Route 4: the `detnorm_*` map is `pub` and open, which makes it a smuggling channel.
///
/// This one goes through the real typed writer rather than a hand-built JSON row, so it
/// proves the *exporter* refuses rather than that the linter would have.
#[test]
fn route_4_a_ground_truth_score_pushed_through_the_open_detnorm_map_stops_the_export() {
    let mut ds = dataset(&DatasetShape::default(), DatasetProfile::V1).expect("assemble");
    assert!(!ds.ma_reports.is_empty());
    // `set_detnorm` refuses a name outside the vocabulary, so a caller who wants to leak
    // has to reach past it into the map. This is that caller.
    ds.ma_reports[0]
        .detnorm
        .insert("true_speed".to_string(), 12.345);

    let root = scratch_dir("smuggle-detnorm").expect("scratch");
    let writer = DatasetWriter::new(&root).expect("writer");
    let err = writer
        .write(&ds, &provenance())
        .expect_err("the exporter must refuse a dataset with a leaked column");
    let msg = err.to_string();
    assert!(msg.contains("leakage linter refused"), "{msg}");
    assert!(
        msg.contains("true_speed"),
        "the message must name the column: {msg}"
    );
    // And the same dataset without the injection writes fine, so the refusal is about the
    // injected column and not about the fixture.
    let ok = dataset(&DatasetShape::default(), DatasetProfile::V1).expect("assemble");
    let root = scratch_dir("smuggle-detnorm-control").expect("scratch");
    DatasetWriter::new(&root)
        .expect("writer")
        .write(&ok, &provenance())
        .expect("the uninjected dataset writes");
}

/// Route 5: the identity as a *value*, under a column name nothing objects to.
#[test]
fn route_5_a_true_identity_smuggled_as_a_value_is_refused() {
    let mut ds = dataset(&DatasetShape::default(), DatasetProfile::V1).expect("assemble");
    assert!(!ds.ma_investigations.is_empty());
    let victim = ds.gt_vehicle[0].true_vehicle_id.clone();
    // `resolved_case_handle` is meant to be an opaque handle. Put the real id in it.
    ds.ma_investigations[0].resolved_case_handle = Some(victim.clone());

    // The key rules alone see nothing wrong: the column name is innocent.
    let row = serde_json::to_value(&ds.ma_investigations[0]).expect("serialise");
    assert!(
        lint_rows("ma/ma_investigations.jsonl", std::slice::from_ref(&row)).is_clean(),
        "the name registry cannot catch this, which is why the value check exists"
    );

    // The dataset-level lint does catch it.
    let mut files = BTreeMap::new();
    files.insert("ma/ma_investigations.jsonl".to_string(), vec![row]);
    let ids: BTreeSet<String> = [victim.clone()].into_iter().collect();
    let report = lint_dataset(&files, &ids);
    assert!(!report.is_clean(), "the identity value was not caught");
    assert_eq!(report.violations[0].kind, ViolationKind::IdentityValue);

    // …and so does the exporter, end to end.
    let root = scratch_dir("smuggle-identity-value").expect("scratch");
    let err = DatasetWriter::new(&root)
        .expect("writer")
        .write(&ds, &provenance())
        .expect_err("the exporter must refuse");
    assert!(err.to_string().contains(&victim), "{err}");
}

/// Route 6: a correctly-tagged ground-truth record dropped into an MA file.
#[test]
fn route_6_an_oracle_tagged_row_in_an_ma_file_is_refused_on_its_tag_alone() {
    let row = serde_json::json!({
        "cert_digest": "aabbccddeeff0011",
        "first_seen": 1.0, "last_seen": 2.0,
        "_visibility": ORACLE,
    });
    let report = lint_rows("ma/ma_cert_status.jsonl", &[row]);
    assert!(!report.is_clean());
    assert_eq!(report.violations[0].kind, ViolationKind::OracleVisibility);
}

/// Route 7: ground truth with an innocent name, which only the channel's own column list
/// can catch.
#[test]
fn route_7_the_transmitter_id_and_the_true_distance_never_reach_a_receiver_log() {
    // First: the ground-truth list must name the fields the record really carries. This
    // is the regression test for the defect — the list once held only `tx_node` and
    // `distance_m`, which no `phy.rx` record has ever contained.
    let declared: BTreeSet<&str> = ground_truth_fields("phy.rx").iter().copied().collect();
    for real in ["tx", "dist_m"] {
        assert!(
            declared.contains(real),
            "phy.rx.{real} is ground truth and the record really carries that name, so it \
             must be in the declared list or the projection silently matches nothing"
        );
    }

    let records: Vec<RecordedRecord> = (0..4u64)
        .map(|i| RecordedRecord {
            channel: "phy.rx".to_string(),
            sim_time: i * 1_000_000_000,
            json: format!(
                r#"{{"t_start":{t},"t_end":{t},"rx":1,"tx":{tx},"msg":{i},
                    "rssi_dbm":-70.25,"sinr_db":24.75,"dist_m":123.456,"outcome":"ok"}}"#,
                t = i * 1_000_000_000,
                tx = i + 2,
            )
            .into_bytes(),
        })
        .collect();

    // The injected fault: write the same rows through the *unblinded* schema, which is
    // what a careless exporter would do, and confirm the linter goes red. Without this
    // half, the clean result below would prove nothing.
    let rows: Vec<(u64, Vec<u8>)> = records
        .iter()
        .map(|r| (r.sim_time, r.json.clone()))
        .collect();
    let unblinded = TableSchema::infer("phy.rx", &rows).expect("schema");
    assert!(
        unblinded.column("tx").is_some_and(|c| c.ground_truth),
        "the inferred schema must tag `tx` as ground truth"
    );
    let dir = scratch_dir("smuggle-receiver-unblinded").expect("scratch");
    let leaky = dir.join("receiver_1_leaky.jsonl");
    v2xw_record::export::jsonl::write(&leaky, &unblinded, &rows).expect("write");
    let leaked = v2xw_record::export::jsonl::read(&leaky).expect("read");
    assert!(
        leaked[0].get("tx").is_some(),
        "the fault was not actually injected"
    );
    let red = lint_rows_of_channel("receiver_1_leaky.jsonl", "phy.rx", &leaked);
    assert!(
        !red.is_clean(),
        "the linter did not refuse a leaked receiver log"
    );
    assert!(
        red.violations
            .iter()
            .any(|v| v.kind == ViolationKind::TaggedGroundTruth && v.path == "tx"),
        "{:?}",
        red.violations
    );

    // Now the real profile, which must be clean — and the ground truth must be in the
    // sidecar, not simply thrown away.
    let dir = scratch_dir("smuggle-receiver-logs").expect("scratch");
    let set = profiles::write(
        &dir,
        LogProfile::ReceiverLogs,
        &records,
        ExportFormat::Jsonl,
    )
    .expect("write");
    let report = set.lint().expect("lint");
    assert!(report.is_clean(), "{}", report.summary());
    assert!(report.rows >= 4, "the lint looked at {} rows", report.rows);

    let data = set
        .node_files
        .iter()
        .find(|f| f.format == ExportFormat::Jsonl)
        .expect("a data file");
    for row in v2xw_record::export::jsonl::read(&data.path).expect("read") {
        assert!(
            row.get("tx").is_none(),
            "the transmitter id reached a receiver log"
        );
        assert!(
            row.get("dist_m").is_none(),
            "the true distance reached a receiver log"
        );
        assert!(
            row.get("rssi_dbm").is_some(),
            "the receiver's own measurement was dropped"
        );
    }
    let gt = v2xw_record::export::jsonl::read(&set.gt_files[0].path).expect("read");
    assert_eq!(
        gt.len(),
        4,
        "the ground truth must be kept, in its own file"
    );
    assert!(gt[0].get("tx").is_some());
    assert!(gt[0].get("dist_m").is_some());
    assert_eq!(gt[0]["_visibility"], ORACLE);
    assert!(
        gt[0].get("msg").is_some(),
        "without the join key the sidecar is useless"
    );
}

/// The same for `net-trace`, which §5 also gives the "NODE (+ GT tx id in a separate
/// column file)" visibility.
#[test]
fn a_net_trace_keeps_the_transmitter_id_out_of_the_node_visible_table_too() {
    let records: Vec<RecordedRecord> = (0..3u64)
        .map(|i| RecordedRecord {
            channel: "phy.rx".to_string(),
            sim_time: i,
            json: format!(
                r#"{{"t_start":{i},"t_end":{i},"rx":1,"tx":9,"msg":{i},"outcome":"lost",
                    "cause":"collision","dist_m":500.5,"sinr_db":-3.25}}"#
            )
            .into_bytes(),
        })
        .collect();
    let dir = scratch_dir("smuggle-net-trace").expect("scratch");
    let set =
        profiles::write(&dir, LogProfile::NetTrace, &records, ExportFormat::Jsonl).expect("write");
    assert!(set.lint().expect("lint").is_clean());
    let data = set
        .node_files
        .iter()
        .find(|f| f.format == ExportFormat::Jsonl)
        .expect("a data file");
    for row in v2xw_record::export::jsonl::read(&data.path).expect("read") {
        assert!(row.get("tx").is_none());
        assert!(row.get("dist_m").is_none());
        // What a net trace is *for* survives: the outcome and the cause.
        assert_eq!(row["outcome"], "lost");
        assert_eq!(row["cause"], "collision");
    }
}

/// The v2 profile's new ground-truth tables use a `gt_` prefix, which §6 asks the registry
/// to be extended with. Prove the extension bites.
#[test]
fn a_gt_prefixed_v2_column_is_refused_and_a_word_that_merely_starts_with_g_t_is_not() {
    let mut row = clean_report();
    row["gt_revocation_stage"] = serde_json::json!("decision");
    let report = lint_rows("ma/ma_reports.jsonl", &[row]);
    assert!(!report.is_clean(), "the v2 extension does not bite");
    assert_eq!(report.violations[0].path, "gt_revocation_stage");

    let mut row = clean_report();
    row["gateway_id"] = serde_json::json!("gw_1");
    row["gtt_estimate_s"] = serde_json::json!(1.5);
    assert!(
        lint_rows("ma/ma_reports.jsonl", &[row]).is_clean(),
        "the extension must not swallow innocent names beginning with the same letters"
    );
}

/// The VeReMi export's one deliberate deviation, as a test: `sender` is derived from the
/// pseudonym, so a per-receiver trace cannot be used to link a device's pseudonyms.
#[test]
fn the_veremi_receiver_trace_cannot_be_used_to_link_a_devices_pseudonyms() {
    use v2xw_record::dataset::veremi::pseudonym_int;
    // Two pseudonyms of one device, which a VeReMi trace's `sender` field would have
    // given the same value.
    let a = pseudonym_int("device-7-pseudonym-0");
    let b = pseudonym_int("device-7-pseudonym-1");
    assert_ne!(
        a, b,
        "if these matched, a detector could link every pseudonym of a device for free"
    );
    assert_eq!(a, pseudonym_int("device-7-pseudonym-0"), "but it is stable");
}
