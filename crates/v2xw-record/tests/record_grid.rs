//! Build decision D9 on the *record* path — the half that had no gate at all.
//!
//! > No floating-point value reaches a **recorded**, exported or digested artefact in raw
//! > IEEE-754 form. One central writer-side encoder quantises each float to its field's
//! > declared grid, and a scanning test fails the build if any output value sits off its
//! > grid.
//!
//! Three holes, all of which these tests would have caught and none of which the crate's
//! own scan could see:
//!
//! 1. `write_frame` scanned its frame; `write_record` stored the JSON verbatim. The
//!    fixture wrote seven raw doubles into every recording and `content_digest` hashed
//!    them. The exporters quantise on the way out, which is precisely why `export::scan`
//!    reported success — but the recording is itself a recorded artefact, and a replay, a
//!    re-export and a run digest are all taken from it.
//! 2. A nested object or array is typed as text and was written by stringifying it, so the
//!    floats inside one were never quantised; and the scan skipped every column that was
//!    not a float column, so it never looked at them either. Both halves are fixed here,
//!    and the scan is shown to catch a deliberately off-grid nested value.
//! 3. A column whose values all happened to be integral was typed `Int64` with no declared
//!    quantum and dropped out of the scan entirely, while the same channel in the next
//!    window exported as `Float64`.

use v2xw_core::{OwnedRecord, Visibility};
use v2xw_record::export::{ExportFormat, ExportProfile, Exporter, jsonl, parquet, scan};
use v2xw_record::fixture::{RunShape, scratch_dir, write_recording};
use v2xw_record::{Reader, RecordError, RecordingOptions, RecordingWriter};

/// Walks a JSON value and calls `f` on every float in it with the key it sits under.
fn for_each_float(key: &str, v: &serde_json::Value, f: &mut impl FnMut(&str, f64)) {
    match v {
        serde_json::Value::Number(n) if n.is_f64() => {
            if let Some(x) = n.as_f64() {
                f(key, x);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|e| for_each_float(key, e, f)),
        serde_json::Value::Object(m) => m.iter().for_each(|(k, e)| for_each_float(k, e, f)),
        _ => {}
    }
}

#[test]
fn every_float_stored_in_a_recording_is_on_its_declared_grid() {
    // The artefact, read back — not the writer's intention. This is the scan ADR 0004 §7
    // asks for, applied to the recording rather than to an export.
    let dir = scratch_dir("record-grid").expect("a scratch directory");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(6, 40)).expect("the recording is written");
    let mut reader = Reader::open(&path).expect("the recording opens");
    let records = reader.records(None).expect("the records come back");
    assert!(!records.is_empty());

    let mut checked = 0usize;
    for r in &records {
        let value: serde_json::Value = serde_json::from_slice(&r.json).expect("a record is JSON");
        for_each_float("", &value, &mut |key, x| {
            let q = v2xw_record::export::schema::declared_quantum(key);
            assert!(
                v2xw_core::math::is_on_grid(x, q),
                "{}.{key} = {x} is off its {q} grid inside the recording (D9)",
                r.channel
            );
            checked += 1;
        });
    }
    assert!(
        checked >= 100,
        "only {checked} floats were checked, so the sweep proves little"
    );
}

#[test]
fn the_recorder_refuses_an_off_grid_record() {
    // The gate, not the fixture: `write_frame` refuses an off-grid frame and `write_record`
    // now refuses an off-grid record, so a provider cannot put a raw double in a recording
    // by going through the serde path instead of the wire one.
    let dir = scratch_dir("record-grid-gate").expect("a scratch directory");
    let mut writer = RecordingWriter::create(dir.join("gate.mcap"), RecordingOptions::default())
        .expect("a recording");

    let off = OwnedRecord {
        channel: "phy.rx",
        visibility: Visibility::Node,
        // 1e-3 is the declared grid for a `_m` field; these are the fixture's own former
        // digits.
        json: br#"{"rx_node":1,"distance_m":123.4567891}"#.to_vec(),
    };
    match writer.write_record(0, &off) {
        Err(RecordError::OffGrid {
            field,
            value,
            quantum,
            ..
        }) => {
            assert_eq!(field, "distance_m");
            assert_eq!(quantum, 1e-3);
            assert_eq!(value, 123.456_789_1);
        }
        other => panic!("expected OffGrid, got {other:?}"),
    }

    // A nested one too: the record path walks objects and arrays, because a float does not
    // stop being recorded by being one level down.
    let nested = OwnedRecord {
        channel: "node.tx",
        visibility: Visibility::Node,
        json: br#"{"node_id":1,"detail":{"distance_m":123.4567891}}"#.to_vec(),
    };
    assert!(
        matches!(
            writer.write_record(0, &nested),
            Err(RecordError::OffGrid { .. })
        ),
        "a nested off-grid float must be refused"
    );

    let array = OwnedRecord {
        channel: "node.tx",
        visibility: Visibility::Node,
        json: br#"{"node_id":1,"samples_m":[1.234,2.345678912]}"#.to_vec(),
    };
    assert!(
        matches!(
            writer.write_record(0, &array),
            Err(RecordError::OffGrid { .. })
        ),
        "an off-grid array element must be refused"
    );

    // …and the on-grid forms of all three go in.
    for json in [
        &br#"{"rx_node":1,"distance_m":123.457}"#[..],
        &br#"{"node_id":1,"detail":{"distance_m":123.457}}"#[..],
        &br#"{"node_id":1,"samples_m":[1.234,2.346]}"#[..],
    ] {
        let channel = if json.starts_with(b"{\"rx_node") {
            "phy.rx"
        } else {
            "node.tx"
        };
        writer
            .write_record(
                0,
                &OwnedRecord {
                    channel,
                    visibility: Visibility::Node,
                    json: json.to_vec(),
                },
            )
            .expect("an on-grid record stores");
    }
    writer.finish().expect("the recording finishes");
}

/// A recording holding one `node.tx` record with a flat, a nested and an array float —
/// the exact shape the register used.
fn nested_export(tag: &str) -> (Vec<v2xw_record::RecordedRecord>, std::path::PathBuf) {
    let dir = scratch_dir(tag).expect("a scratch directory");
    let path = dir.join("nested.mcap");
    let mut writer = RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
    writer
        .write_manifest(r#"{"schema":"v2xw/manifest/1"}"#)
        .expect("a manifest");
    for k in 0..3u64 {
        writer
            .write_record(
                k,
                &OwnedRecord {
                    channel: "node.tx",
                    visibility: Visibility::Node,
                    json: serde_json::to_vec(&serde_json::json!({
                        "node_id": 1,
                        "airtime_ms": 0.526,
                        "detail": {"distance_m": 123.457, "rssi_dbm": -78.12},
                        "samples_m": [1.234, 2.346],
                    }))
                    .expect("json"),
                },
            )
            .expect("the record stores");
    }
    writer.finish().expect("the recording finishes");
    let mut reader = Reader::open(&path).expect("the recording opens");
    let records = reader.records(None).expect("the records");
    (records, dir)
}

#[test]
fn nested_floats_are_quantised_by_every_exporter() {
    let (records, dir) = nested_export("nested-quantise");

    // JSONL keeps the nested shape, so the values can be read straight back.
    let jl = Exporter::new(dir.join("jsonl"), ExportProfile::Full).expect("an exporter");
    let files = jl
        .export_channel("node.tx", &records, ExportFormat::Jsonl)
        .expect("the export succeeds");
    let rows = jsonl::read(&files[0].path).expect("the jsonl reads back");
    assert_eq!(rows.len(), 3);
    for row in &rows {
        let obj = row.as_object().expect("an object");
        let detail = obj.get("detail").expect("the nested object");
        assert_eq!(
            detail.get("distance_m").and_then(serde_json::Value::as_f64),
            Some(123.457),
            "a nested metre value must be on the 1 mm grid"
        );
        assert_eq!(
            detail.get("rssi_dbm").and_then(serde_json::Value::as_f64),
            Some(-78.12)
        );
        let samples = obj
            .get("samples_m")
            .and_then(serde_json::Value::as_array)
            .expect("the array");
        assert_eq!(samples[1].as_f64(), Some(2.346));
    }

    // Parquet and Arrow IPC store the nested value as its compact JSON; the numbers in it
    // are quantised the same way.
    for (format, tag) in [
        (ExportFormat::Parquet, "parquet"),
        (ExportFormat::ArrowIpc, "arrow"),
    ] {
        let exporter = Exporter::new(dir.join(tag), ExportProfile::Full).expect("an exporter");
        let files = exporter
            .export_channel("node.tx", &records, format)
            .expect("the export succeeds");
        let report = scan::scan_export(&files).expect("every value is on its grid");
        assert!(
            report.values >= 3 * 4,
            "{tag}: the scan looked at {} values, so it did not see the nested ones",
            report.values
        );
        if format == ExportFormat::Parquet {
            let batches = parquet::read(&files[0].path).expect("the parquet reads back");
            let text = batches[0]
                .column_by_name("detail")
                .expect("a detail column")
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("a text column");
            assert_eq!(text.value(0), r#"{"distance_m":123.457,"rssi_dbm":-78.12}"#);
        }
    }

    // And the whole-export scan counts the nested values, so "every exported float is on
    // its grid" is a claim about all of them.
    let all = Exporter::new(dir.join("all"), ExportProfile::Full).expect("an exporter");
    let files = all
        .export_all(&records, ExportFormat::Jsonl)
        .expect("the export succeeds");
    let report = scan::scan_export(&files).expect("every value is on its grid");
    assert!(report.values >= 3 * 4, "{report:?}");
}

#[test]
fn the_scan_catches_an_off_grid_value_nested_inside_a_text_column() {
    // The teeth. A scan that cannot fail is worse than no scan, and this one reported
    // `ScanReport { files: 1, rows: 3, values: 3 }` with no error over an artefact whose
    // nested values were raw. The file below is one the exporter would never write, and
    // the scan has to find it in each of the three formats.
    let (records, dir) = nested_export("nested-teeth");
    let schema = v2xw_record::export::TableSchema::infer(
        "node.tx",
        &records
            .iter()
            .map(|r| (r.sim_time, r.json.clone()))
            .collect::<Vec<_>>(),
    )
    .expect("a schema");
    assert_eq!(
        schema.column("detail").map(|c| c.kind),
        Some(v2xw_record::export::ColumnKind::Text),
        "a nested object is a text column, which is how the hole opened"
    );

    // JSONL: write the row by hand with an unrounded nested double.
    let path = dir.join("off_grid.jsonl");
    std::fs::write(
        &path,
        b"{\"detail\":{\"distance_m\":123.4567891},\"node_id\":1,\"sim_time_ns\":0}\n",
    )
    .expect("the file is written");
    match scan::scan_jsonl(&path, &schema) {
        Err(RecordError::OffGrid {
            field,
            value,
            quantum,
            ..
        }) => {
            assert_eq!(field, "distance_m");
            assert_eq!(quantum, 1e-3);
            assert_eq!(value, 123.456_789_1);
        }
        other => panic!("the scan must catch a nested off-grid value, got {other:?}"),
    }

    // The same value inside an array, and inside a Parquet text column.
    let path = dir.join("off_grid_array.jsonl");
    std::fs::write(
        &path,
        b"{\"node_id\":1,\"samples_m\":[1.234,2.345678912],\"sim_time_ns\":0}\n",
    )
    .expect("the file is written");
    assert!(
        matches!(
            scan::scan_jsonl(&path, &schema),
            Err(RecordError::OffGrid { .. })
        ),
        "an off-grid array element must be caught"
    );

    use std::sync::Arc;
    let arrow_schema = Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("detail", arrow::datatypes::DataType::Utf8, true),
    ]));
    let batch = arrow::array::RecordBatch::try_new(
        arrow_schema,
        vec![Arc::new(arrow::array::StringArray::from(vec![
            r#"{"distance_m":123.457}"#,
            r#"{"distance_m":123.4567891}"#,
        ]))],
    )
    .expect("a batch");
    let path = dir.join("off_grid.parquet");
    parquet::write(&path, &batch).expect("the file is written");
    match scan::scan_parquet(&path, &schema) {
        Err(RecordError::OffGrid { field, row, .. }) => {
            assert_eq!(field, "distance_m");
            assert_eq!(row, 1, "the scan must name the row it found it in");
        }
        other => panic!("the scan must catch a nested off-grid value, got {other:?}"),
    }
}

#[test]
fn a_window_of_integral_ratios_is_still_exported_as_a_float_column() {
    // F12 end to end: a `cbr` window that happened to hold only 0 and 1 used to export as
    // Int64 with no declared quantum, which both gave one channel two Parquet schemas and
    // dropped the column out of the grid scan — the scan skips everything that is not a
    // float column, so a raw value there would never be seen again.
    let dir = scratch_dir("integral-ratio").expect("a scratch directory");
    let path = dir.join("cbr.mcap");
    let mut writer = RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
    for (k, cbr) in [(0u64, 0i64), (1, 1), (2, 0)] {
        writer
            .write_record(
                k,
                &OwnedRecord {
                    channel: "mac.cbr",
                    visibility: Visibility::Node,
                    json: serde_json::to_vec(&serde_json::json!({
                        "node_id": 1, "cbr": cbr, "channel": 180,
                    }))
                    .expect("json"),
                },
            )
            .expect("the record stores");
    }
    writer.finish().expect("the recording finishes");
    let mut reader = Reader::open(&path).expect("the recording opens");
    let records = reader.records(None).expect("the records");

    let exporter = Exporter::new(dir.join("out"), ExportProfile::Full).expect("an exporter");
    let files = exporter
        .export_channel("mac.cbr", &records, ExportFormat::Parquet)
        .expect("the export succeeds");
    let schema = files[0].schema.as_ref().expect("a schema");
    let cbr = schema.column("cbr").expect("a cbr column");
    assert_eq!(cbr.kind, v2xw_record::export::ColumnKind::Float);
    assert_eq!(cbr.quantum, Some(1e-4), "and it declares its grid");
    assert_eq!(
        schema.column("node_id").expect("a node_id column").kind,
        v2xw_record::export::ColumnKind::Int,
        "an identifier is still an integer"
    );
    assert_eq!(
        schema.column("sim_time_ns").expect("a time column").kind,
        v2xw_record::export::ColumnKind::Int,
        "and so is the time column, whose name ends in a unit suffix"
    );

    let batches = parquet::read(&files[0].path).expect("the parquet reads back");
    assert_eq!(
        batches[0]
            .schema()
            .field_with_name("cbr")
            .unwrap()
            .data_type(),
        &arrow::datatypes::DataType::Float64,
        "two windows of one channel must not have two schemas"
    );
    // The column is now in the scan's sight, which is the point of typing it.
    let report = scan::scan_export(&files).expect("every value is on its grid");
    assert!(
        report.values >= 3,
        "the cbr column is still invisible to the scan: {report:?}"
    );
}
