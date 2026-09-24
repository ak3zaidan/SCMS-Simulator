//! A dataset is publishable only if it carries its own provenance — and the part of that
//! provenance which decides whether a byte-count result can be cited is *which encoder
//! produced the bytes*.
//!
//! These tests drive the whole exporter — assemble, write, lint, manifest, datasheet — and
//! then read the two artefacts a consumer will actually see, `manifest.json` and
//! `DATASHEET.md`, rather than the in-memory structs. The distinction matters here for the
//! same reason it matters to the leakage linter: the bytes on disk are the only thing that
//! is published, and a writer is not trusted to have been careful.
//!
//! Each test injects the fault it claims to catch. A provenance section that has never
//! been seen printing "not citable" is an assertion dressed up as a test.

use std::collections::BTreeMap;

use v2xw_record::dataset::fixture::{DatasetShape, dataset, provenance};
use v2xw_record::dataset::{
    ByteProvenance, ByteProvenanceReport, DatasetProfile, DatasetWriter, MessageBytes,
    ModelProvenance, RunProvenance, TxTally,
};
use v2xw_record::fixture::scratch_dir;

/// Writes the fixture dataset under `tag` and returns the manifest JSON and the datasheet.
fn write(tag: &str, prov: &RunProvenance) -> (serde_json::Value, String) {
    let ds = dataset(&DatasetShape::default(), DatasetProfile::V2).expect("assemble");
    let dir = scratch_dir(tag).expect("scratch");
    let out = DatasetWriter::new(&dir)
        .expect("writer")
        .write(&ds, prov)
        .expect("write");
    assert!(out.lint.is_clean(), "the fixture must lint clean");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).expect("manifest"),
    )
    .expect("valid JSON");
    let sheet = std::fs::read_to_string(dir.join("DATASHEET.md")).expect("DATASHEET.md");
    (manifest, sheet)
}

/// The fixture's only transmitted message type is `bsm`, which its provenance declares as
/// real UPER, so its byte counts are citable and the datasheet says so in those words.
#[test]
fn a_run_whose_every_byte_is_real_says_its_byte_counts_are_citable() {
    let (manifest, sheet) = write("provenance-real", &provenance());

    let bp = &manifest["byte_provenance"];
    assert_eq!(bp["modelled_bytes"], 0);
    assert_eq!(bp["undeclared_bytes"], 0);
    assert_eq!(bp["real_share_permille"], 1000);
    assert!(
        bp["real_bytes"].as_u64().expect("real_bytes") > 0,
        "the fixture transmits something"
    );
    assert_eq!(bp["real_bytes"], bp["bytes_on_wire"]);

    let rows = bp["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "the fixture transmits one message type");
    assert_eq!(rows[0]["msg_type"], "bsm");
    assert_eq!(rows[0]["provenance"]["kind"], "real");
    assert_eq!(rows[0]["provenance"]["detail"], "uper");

    // The declaration carries `psm` and the run never sent one. Reported, not hidden: a
    // declaration that describes a different run is how this section comes to be wrong
    // while looking complete.
    assert_eq!(
        bp["declared_types_unused"]
            .as_array()
            .expect("declared_types_unused"),
        &vec![serde_json::Value::from("psm")]
    );

    assert!(sheet.contains("## Byte provenance"));
    assert!(sheet.contains("citable as one"), "{sheet}");
    assert!(sheet.contains("real bytes (uper)"));
}

/// The fault injected: the same run, with `bsm` declared as a size model. Nothing else
/// changes, and the datasheet must flip to refusing the citation.
#[test]
fn declaring_the_same_traffic_as_modelled_makes_the_datasheet_refuse_the_citation() {
    let modelled = RunProvenance {
        message_encodings: BTreeMap::from([(
            "bsm".to_string(),
            ByteProvenance::SizeModel("1.0.0".to_string()),
        )]),
        ..provenance()
    };
    let (manifest, sheet) = write("provenance-modelled", &modelled);

    let bp = &manifest["byte_provenance"];
    assert_eq!(bp["real_bytes"], 0);
    assert_eq!(bp["real_share_permille"], 0);
    assert!(bp["modelled_bytes"].as_u64().expect("modelled") > 0);

    assert!(!sheet.contains("citable as one"), "{sheet}");
    assert!(
        sheet.contains("**not** a measurement of an encoding"),
        "{sheet}"
    );
    assert!(sheet.contains("size model 1.0.0"), "{sheet}");
}

/// The fault injected: the engine forgets to declare the codec at all. An omission must
/// not buy citable byte counts, which is the one way this section could be gamed without
/// anybody noticing.
#[test]
fn an_undeclared_codec_does_not_read_as_a_real_one() {
    let undeclared = RunProvenance {
        message_encodings: BTreeMap::new(),
        ..provenance()
    };
    let (manifest, sheet) = write("provenance-undeclared", &undeclared);

    let bp = &manifest["byte_provenance"];
    assert_eq!(bp["real_bytes"], 0);
    assert_eq!(bp["modelled_bytes"], 0);
    assert!(bp["undeclared_bytes"].as_u64().expect("undeclared") > 0);
    assert_eq!(bp["rows"][0]["provenance"]["kind"], "undeclared");

    assert!(!sheet.contains("citable as one"));
    assert!(sheet.contains("**undeclared**"), "{sheet}");
    assert!(
        sheet.contains("should be reported"),
        "an undeclared codec is a provenance defect and the datasheet says so: {sheet}"
    );
}

/// Every model behind the dataset appears with its validation status, and a model that was
/// never checked says `unvalidated` rather than being left out.
#[test]
fn the_datasheet_states_the_validation_status_of_every_model_behind_it() {
    let (manifest, sheet) = write("provenance-models", &provenance());

    let models = manifest["models"].as_array().expect("models");
    assert_eq!(models.len(), 3);
    let statuses: Vec<&str> = models
        .iter()
        .map(|m| m["validation_status"].as_str().expect("status"))
        .collect();
    assert!(statuses.contains(&"literature-checked"));
    assert!(statuses.contains(&"unit-tested"));
    assert!(
        statuses.contains(&"unvalidated"),
        "a model nothing has checked must say so"
    );

    // The legacy key is still there and still agrees.
    let legacy = manifest["model_cards"].as_array().expect("model_cards");
    assert_eq!(legacy.len(), 3);

    assert!(sheet.contains("`unvalidated` — nothing checked"), "{sheet}");
    assert!(sheet.contains("**1 of 3**"), "{sheet}");
    assert!(
        sheet.contains("**7 parameter default(s) still marked `todo-calibrate`**"),
        "{sheet}"
    );
}

/// An engine that fills only the richer model list still gets the legacy `(id, version)`
/// key the frozen audit reads by name.
#[test]
fn the_legacy_model_card_key_is_derived_when_only_the_richer_list_is_supplied() {
    let prov = RunProvenance {
        model_cards: Vec::new(),
        models: vec![ModelProvenance {
            id: "radio/propagation/log-distance".to_string(),
            version: "2.0.0".to_string(),
            validation_status: v2xw_core::card::ValidationStatus::FieldChecked,
            content_hash: "ab".repeat(32),
            todo_calibrate: 0,
            tiers: vec!["high".to_string()],
        }],
        ..provenance()
    };
    let (manifest, sheet) = write("provenance-derived-legacy", &prov);
    assert_eq!(
        manifest["model_cards"][0]["id"],
        "radio/propagation/log-distance"
    );
    assert_eq!(manifest["model_cards"][0]["version"], "2.0.0");
    assert!(sheet.contains("`field-checked` — against measurements"));
}

/// The tally is a function of what was transmitted and not of the order the records
/// arrived in, and a partial sum is never printed as a total.
#[test]
fn the_report_is_order_independent_and_marks_a_partial_sum() {
    let bytes = MessageBytes {
        messages: 4,
        bytes_on_wire: 1_600,
        payload_bytes: 120,
        payload_stated: 1,
        envelope_bytes: 444,
        envelope_stated: 4,
    };
    // Inserted in reverse, so the sorted output order is the map's and not the caller's.
    let mut tally = TxTally::default();
    tally.by_msg_type.insert("denm".to_string(), bytes.clone());
    tally.by_msg_type.insert("cam".to_string(), bytes);
    let declared = BTreeMap::from([
        ("cam".to_string(), ByteProvenance::Real("uper".to_string())),
        ("denm".to_string(), ByteProvenance::Real("uper".to_string())),
    ]);
    let report = ByteProvenanceReport::new(&tally, &declared);
    assert!(report.all_real());
    assert_eq!(report.messages, 8);
    assert_eq!(report.rows[0].msg_type, "cam");

    let ds = dataset(&DatasetShape::default(), DatasetProfile::V1).expect("assemble");
    let dir = scratch_dir("provenance-partial").expect("scratch");
    let manifest = DatasetWriter::new(&dir)
        .expect("writer")
        .write(&ds, &provenance())
        .expect("write")
        .manifest
        .with_byte_provenance(report);
    let sheet = v2xw_record::dataset::datasheet::render(
        &ds,
        &manifest,
        &v2xw_record::dataset::LeakageReport::default(),
    );
    assert!(sheet.contains("120 (over 1 of 4)"), "{sheet}");
    assert!(sheet.contains("| 444 |"), "{sheet}");
}
