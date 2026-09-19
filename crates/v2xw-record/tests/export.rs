//! The exporters, and the scanning test ADR 0004 §7 asks for: "a test scans every output
//! file for any value that is off its grid".
//!
//! Conformance items: V6 (no file mixes GT and NODE unless it is declared mixed), plus
//! build decision D9 end to end — the writer quantises, the artefact declares the grid it
//! was quantised to, and the scan reads the artefact back and checks it.

use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema};

use v2xw_record::Reader;
use v2xw_record::export::{
    ExportFormat, ExportProfile, Exporter, TableSchema, jsonl, parquet, scan,
};
use v2xw_record::fixture::{RunShape, scratch_dir, write_recording};

/// A recorded fixture in its own scratch directory.
///
/// One directory per test, because the test harness runs them in parallel and a shared
/// path would have them overwrite each other's recording.
fn recorded(tag: &str) -> (Vec<v2xw_record::RecordedRecord>, std::path::PathBuf) {
    let dir = scratch_dir(&format!("export-{tag}")).expect("a scratch directory");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(6, 40)).expect("the recording is written");
    let mut reader = Reader::open(&path).expect("the recording opens");
    let records = reader.records(None).expect("the records come back");
    assert!(!records.is_empty());
    (records, dir)
}

#[test]
fn every_exported_float_is_on_its_declared_grid() {
    let (records, dir) = recorded("grids");
    for (format, tag) in [
        (ExportFormat::Parquet, "parquet"),
        (ExportFormat::ArrowIpc, "arrow"),
        (ExportFormat::Jsonl, "jsonl"),
    ] {
        let exporter = Exporter::new(dir.join(tag), ExportProfile::Full).expect("an exporter");
        let files = exporter
            .export_all(&records, format)
            .expect("the export succeeds");
        assert!(files.len() >= 8, "one data file and one schema per channel");
        let report = scan::scan_export(&files).expect("every value is on its grid");
        assert!(
            report.values > 0,
            "{tag}: the scan checked no values, so it proves nothing"
        );
        assert!(report.rows > 0);

        // Every float column declares a quantum, in the artefact itself.
        for file in &files {
            let Some(schema) = &file.schema else { continue };
            let sidecar = file.path.with_extension("").with_extension("schema.json");
            let _ = sidecar;
            for col in &schema.columns {
                if col.kind == v2xw_record::export::ColumnKind::Float {
                    assert!(
                        col.quantum.is_some(),
                        "{}: column {} declares no grid",
                        schema.channel,
                        col.name
                    );
                }
            }
        }
    }
}

#[test]
fn the_schema_sidecar_is_written_and_reads_back() {
    let (records, dir) = recorded("sidecar");
    let exporter = Exporter::new(dir.join("sidecar"), ExportProfile::Full).expect("an exporter");
    let files = exporter
        .export_channel("phy.rx", &records, ExportFormat::Parquet)
        .expect("the export succeeds");
    let sidecar = files
        .iter()
        .find(|f| f.format == ExportFormat::Json)
        .expect("a schema sidecar");
    let bytes = std::fs::read(&sidecar.path).expect("the sidecar is readable");
    let schema = TableSchema::from_json(&bytes).expect("the sidecar parses");
    assert_eq!(schema.schema, "v2xw/phy.rx/1");
    assert_eq!(schema.channel, "phy.rx");
    assert_eq!(schema.visibility, "node-and-gt");
    assert_eq!(
        schema.column("distance_m").map(|c| c.quantum),
        Some(Some(1e-3))
    );
    assert_eq!(
        schema.column("rssi_dbm").map(|c| c.quantum),
        Some(Some(1e-2))
    );
    assert!(schema.column("distance_m").expect("a column").ground_truth);
    // The sidecar is reproducible: canonical JSON with sorted keys.
    assert_eq!(schema.to_json().expect("it serialises"), bytes);
}

#[test]
fn the_node_only_export_contains_no_ground_truth() {
    let (records, dir) = recorded("blind");
    let exporter = Exporter::new(dir.join("blind"), ExportProfile::NodeOnly).expect("an exporter");
    let files = exporter
        .export_all(&records, ExportFormat::Parquet)
        .expect("the export succeeds");
    for file in &files {
        let Some(schema) = &file.schema else { continue };
        // V6: a ground-truth channel is not exported at all…
        let spec = v2xw_record::channels::by_name(&schema.channel).expect("a known channel");
        assert!(
            !spec.is_ground_truth_channel(),
            "{} is a ground-truth channel and must not be exported blind",
            schema.channel
        );
        // …and a mixed channel's ground-truth columns are projected out.
        for col in &schema.columns {
            assert!(
                !col.ground_truth,
                "{}.{} is ground truth and survived the blind export",
                schema.channel, col.name
            );
        }
        let batches = parquet::read(&file.path).expect("the file reads back");
        for batch in &batches {
            for name in ["tx_node", "distance_m", "los_class", "subject_actor_id"] {
                assert!(
                    batch.column_by_name(name).is_none(),
                    "{} still has a {name} column",
                    schema.channel
                );
            }
        }
    }
    scan::scan_export(&files).expect("every value is still on its grid");
}

#[test]
fn parquet_and_jsonl_carry_the_same_quantised_values() {
    let (records, dir) = recorded("compare");
    let pq = Exporter::new(dir.join("cmp-pq"), ExportProfile::Full).expect("an exporter");
    let jl = Exporter::new(dir.join("cmp-jl"), ExportProfile::Full).expect("an exporter");
    let pq_files = pq
        .export_channel("phy.rx", &records, ExportFormat::Parquet)
        .expect("parquet");
    let jl_files = jl
        .export_channel("phy.rx", &records, ExportFormat::Jsonl)
        .expect("jsonl");

    let batches = parquet::read(&pq_files[0].path).expect("the parquet reads back");
    let rows = jsonl::read(&jl_files[0].path).expect("the jsonl reads back");
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, rows.len());
    assert_eq!(
        total,
        records.iter().filter(|r| r.channel == "phy.rx").count()
    );

    let mut i = 0usize;
    for batch in &batches {
        let distance = batch
            .column_by_name("distance_m")
            .expect("a distance column")
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("a float column");
        let time = batch
            .column_by_name("sim_time_ns")
            .expect("a time column")
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("an integer column");
        for r in 0..batch.num_rows() {
            let row = rows[i].as_object().expect("an object");
            assert_eq!(
                row.get("distance_m").and_then(serde_json::Value::as_f64),
                Some(distance.value(r)),
                "row {i}: parquet and jsonl disagree"
            );
            assert_eq!(
                row.get("sim_time_ns").and_then(serde_json::Value::as_i64),
                Some(time.value(r))
            );
            // The source value was 123.4567891 m; the declared grid is 1 mm.
            assert_eq!(distance.value(r), 123.457);
            i += 1;
        }
    }
}

#[test]
fn the_grid_scan_catches_a_value_that_was_not_quantised() {
    // The scan has to have teeth, or "every exported float is on its grid" is a
    // tautology about a writer that always quantises. This writes a file the exporter
    // would never write — an unrounded double under a schema that declares a 1 mm grid —
    // and insists the scan finds it. This is ADR 0004's `st_bbox` defect, reproduced.
    let dir = scratch_dir("export-teeth").expect("a scratch directory");
    let path = dir.join("off_grid.parquet");
    let schema = TableSchema {
        schema: "v2xw/phy.rx/1".to_string(),
        channel: "phy.rx".to_string(),
        visibility: "node-and-gt".to_string(),
        columns: vec![v2xw_record::export::ColumnSpec {
            name: "distance_m".to_string(),
            kind: v2xw_record::export::ColumnKind::Float,
            quantum: Some(1e-3),
            ground_truth: true,
        }],
    };
    let arrow_schema = Arc::new(Schema::new(vec![Field::new(
        "distance_m",
        DataType::Float64,
        true,
    )]));
    let batch = RecordBatch::try_new(
        arrow_schema,
        vec![Arc::new(Float64Array::from(vec![
            12.345,
            // The value a `sin`-derived coordinate has before rounding: on the grid to
            // thirteen decimal places and off it in the last bit.
            12.345_000_000_000_057,
        ]))],
    )
    .expect("a batch");
    parquet::write(&path, &batch).expect("the file is written");

    let err = scan::scan_parquet(&path, &schema).expect_err("the scan must catch it");
    match err {
        v2xw_record::RecordError::OffGrid {
            field,
            row,
            value,
            quantum,
            ..
        } => {
            assert_eq!(field, "distance_m");
            assert_eq!(row, 1);
            assert_eq!(quantum, 1e-3);
            assert_ne!(value, 12.345);
        }
        other => panic!("expected OffGrid, got {other}"),
    }
}

#[test]
fn arrow_ipc_round_trips_a_metric_batch() {
    // ADR 0008 decision 2: Arrow IPC for tabular metric batches and Python plug-in
    // exchange.
    let (records, dir) = recorded("metrics");
    let exporter = Exporter::new(dir.join("metrics"), ExportProfile::Full).expect("an exporter");
    let files = exporter
        .export_channel("metric.sample", &records, ExportFormat::ArrowIpc)
        .expect("the export succeeds");
    let batches =
        v2xw_record::export::arrow_ipc::read(&files[0].path).expect("the file reads back");
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(
        total,
        records
            .iter()
            .filter(|r| r.channel == "metric.sample")
            .count()
    );
    // The declared grid travels with the file, in the Arrow field metadata.
    let field = batches[0]
        .schema()
        .field_with_name("value")
        .expect("a value column")
        .clone();
    assert_eq!(
        field.metadata().get("v2xw.quantum").map(String::as_str),
        Some("1e-6"),
        "the Arrow schema must declare the grid"
    );
    scan::scan_export(&files).expect("every value is on its grid");
}
