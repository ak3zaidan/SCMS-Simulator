//! Every exported float is on its declared grid — and the scan goes red when one is not.
//!
//! ADR 0004 §7 asks for "a test [that] scans every output file for any value that is off
//! its grid", and the reason it exists is in the same ADR: one legacy field escaped
//! rounding, and its `sin`/`cos`-derived coordinates then differed by one unit in the last
//! place between Windows x86-64 and macOS arm64, so the published golden digests stopped
//! reproducing on any other host.
//!
//! A scan that has only ever been run on correct output is not evidence. So every test
//! here has two halves: the export is clean, **and** an off-grid value injected into the
//! same export is caught, with the message naming the column.

use v2xw_record::dataset::fixture::{DatasetShape, dataset, provenance};
use v2xw_record::dataset::{DatasetProfile, DatasetWriter, LogProfile, profiles, scan_dataset};
use v2xw_record::export::scan::scan_export;
use v2xw_record::export::{ExportFormat, ExportProfile, Exporter};
use v2xw_record::fixture::scratch_dir;
use v2xw_record::reader::RecordedRecord;
use v2xw_record::{RecordError, dataset as ds};

fn write(profile: DatasetProfile, tag: &str) -> std::path::PathBuf {
    let root = scratch_dir(&format!("grid-{}-{tag}", profile.as_str())).expect("scratch");
    let d = dataset(&DatasetShape::default(), profile).expect("assemble");
    DatasetWriter::new(&root)
        .expect("writer")
        .write(&d, &provenance())
        .expect("write");
    root
}

/// The clean half: every float in every table of both profiles is on its declared grid.
#[test]
fn every_float_in_a_written_dataset_is_on_its_declared_grid() {
    for profile in [DatasetProfile::V1, DatasetProfile::V2] {
        let root = write(profile, "clean");
        let report = scan_dataset(&root).expect("the scan passes");
        assert!(report.files >= 10, "{} files scanned", report.files);
        assert!(report.rows > 50, "{} rows scanned", report.rows);
        assert!(
            report.values > 200,
            "{} floats checked in the {} profile — the scan looked at almost nothing",
            report.values,
            profile.as_str()
        );
    }
}

/// The red half: an off-grid value in a dataset table is caught, and the error names the
/// column, the file and the row.
#[test]
fn an_off_grid_value_injected_into_a_dataset_table_is_caught() {
    let root = write(DatasetProfile::V1, "injected");
    let path = root.join("ma/ma_reports.jsonl");
    let text = std::fs::read_to_string(&path).expect("read");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    assert!(!lines.is_empty(), "no report rows to inject into");

    // `ingest_time` is declared on the 1e-3 grid. 9.512_345_678 is on no decimal grid
    // coarser than 1e-9, so it is off 1e-3 — which is exactly what an unquantised value
    // out of a real computation looks like.
    let mut row: serde_json::Value = serde_json::from_str(&lines[0]).expect("json");
    row["ingest_time"] = serde_json::json!(9.512_345_678);
    lines[0] = v2xw_record::dataset::pyjson::canonical(&row);
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write");

    let err = scan_dataset(&root).expect_err("the scan must refuse an off-grid value");
    match err {
        RecordError::OffGrid {
            field,
            value,
            quantum,
            file,
            ..
        } => {
            assert_eq!(field, "ingest_time");
            assert_eq!(value, 9.512_345_678);
            assert_eq!(quantum, 1e-3);
            assert!(file.contains("ma_reports"), "{file}");
        }
        other => panic!("expected OffGrid, got {other}"),
    }
}

/// `st_bbox` is the field D9 names by hand — "the MA dataset exporter keeps `st_bbox` for
/// v1 schema compatibility but quantises it like every other float" — and it is an
/// *array*, which is how a float escapes a column-by-column scan.
#[test]
fn an_off_grid_corner_of_st_bbox_is_caught_even_though_it_is_inside_an_array() {
    let root = write(DatasetProfile::V1, "bbox");
    let path = root.join("ma/ma_reports.jsonl");
    let text = std::fs::read_to_string(&path).expect("read");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut row: serde_json::Value = serde_json::from_str(&lines[0]).expect("json");
    // The third corner only. A scan that checked the first element and stopped, or that
    // treated the array as an opaque value, would miss this.
    row["st_bbox"][2] = serde_json::json!(123.456_789);
    lines[0] = v2xw_record::dataset::pyjson::canonical(&row);
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write");

    let err = scan_dataset(&root).expect_err("an off-grid array element must be caught");
    match err {
        RecordError::OffGrid { field, quantum, .. } => {
            assert_eq!(field, "st_bbox");
            assert_eq!(quantum, 1e-3, "st_bbox is on the metre grid");
        }
        other => panic!("expected OffGrid, got {other}"),
    }
}

/// A dataset with no `schema.json` cannot be scanned, and reporting success for it would
/// be the worst possible answer.
#[test]
fn a_dataset_with_no_declared_grids_is_an_error_rather_than_a_pass() {
    let root = write(DatasetProfile::V1, "no-schema");
    std::fs::remove_file(root.join("schema.json")).expect("remove");
    let err = scan_dataset(&root).expect_err("an undeclared dataset must not scan clean");
    assert!(matches!(err, RecordError::Io { .. }), "{err}");
}

/// The scan reads the declaration back from the artefact, not from the constant the writer
/// used — so tightening the declaration makes previously-passing data fail.
///
/// This is what makes the scan a check rather than a tautology: if it consulted
/// `DATASET_GRIDS` directly it would agree with the writer by construction.
#[test]
fn the_scan_reads_the_declaration_from_the_file_so_tightening_it_bites() {
    let root = write(DatasetProfile::V1, "declaration");
    assert!(scan_dataset(&root).is_ok());

    // Re-declare `detector_score` on a grid ten times coarser than the data is written
    // on. Some score in the dataset is now off its declared grid.
    let path = root.join("schema.json");
    let bytes = std::fs::read(&path).expect("read");
    let mut schema: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    schema["grids"]["detector_score"] = serde_json::json!(1e-2);
    std::fs::write(&path, serde_json::to_vec_pretty(&schema).expect("json")).expect("write");

    let err = scan_dataset(&root).expect_err("the tightened declaration must bite");
    match err {
        RecordError::OffGrid { field, quantum, .. } => {
            assert_eq!(field, "detector_score");
            assert_eq!(quantum, 1e-2, "the scan used the file's declaration");
        }
        other => panic!("expected OffGrid, got {other}"),
    }
}

/// The declaration is digested with the data, so a consumer can tell it was not tampered
/// with after the fact.
#[test]
fn the_grid_declaration_is_covered_by_the_datasets_digest() {
    let root = scratch_dir("grid-digested").expect("scratch");
    let d = dataset(&DatasetShape::default(), DatasetProfile::V2).expect("assemble");
    let out = DatasetWriter::new(&root)
        .expect("writer")
        .write(&d, &provenance())
        .expect("write");
    let entry = out
        .manifest
        .outputs
        .iter()
        .find(|f| f.path == "schema.json")
        .expect("schema.json must be in the digested outputs");
    let bytes = std::fs::read(root.join("schema.json")).expect("read");
    assert_eq!(v2xw_core::hash::sha256_hex(&bytes), entry.sha256);
}

/// The channel-table exporters go through their own scan, over their own `schema.json`
/// sidecars — both halves, again.
#[test]
fn the_channel_table_exports_are_on_grid_and_their_scan_goes_red_when_one_is_not() {
    let records = v2xw_record::dataset::fixture::records(&DatasetShape::default());
    let dir = scratch_dir("grid-channels").expect("scratch");
    let exporter = Exporter::new(&dir, ExportProfile::NodeOnly).expect("exporter");
    let files = exporter
        .export_all(&records, ExportFormat::Jsonl)
        .expect("export");
    let report = scan_export(&files).expect("the export is on grid");
    assert!(report.values > 100, "{} floats checked", report.values);

    // Inject: put an unquantised dB value into the written `mac.cbr` table.
    let data = files
        .iter()
        .find(|f| f.channel.as_deref() == Some("mac.cbr") && f.format == ExportFormat::Jsonl)
        .expect("a mac.cbr table");
    let text = std::fs::read_to_string(&data.path).expect("read");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut row: serde_json::Value = serde_json::from_str(&lines[0]).expect("json");
    row["cbr"] = serde_json::json!(0.123_456_789);
    lines[0] = serde_json::to_string(&row).expect("json");
    std::fs::write(&data.path, format!("{}\n", lines.join("\n"))).expect("write");

    let err = scan_export(&files).expect_err("the export scan must refuse");
    match err {
        RecordError::OffGrid { field, quantum, .. } => {
            assert_eq!(field, "cbr");
            assert_eq!(quantum, 1e-4, "a ratio is on the 1e-4 grid (D9)");
        }
        other => panic!("expected OffGrid, got {other}"),
    }
}

/// The `receiver-logs`, `telemetry` and `net-trace` profiles are on grid too, and their
/// own scan catches an injection.
#[test]
fn the_log_profiles_are_on_grid_and_catch_an_injection() {
    let records = v2xw_record::dataset::fixture::records(&DatasetShape::default());
    for profile in [
        LogProfile::ReceiverLogs,
        LogProfile::Telemetry,
        LogProfile::NetTrace,
    ] {
        let dir = scratch_dir(&format!("grid-{}", profile.dir_name())).expect("scratch");
        let set = profiles::write(&dir, profile, &records, ExportFormat::Jsonl).expect("write");
        let files: Vec<_> = set.node_files.clone();
        let report = scan_export(&files).expect("on grid");
        assert!(report.rows > 0, "{} wrote no rows", profile.dir_name());
    }

    // The injection, on the receiver logs: an unquantised RSSI.
    let dir = scratch_dir("grid-receiver-injected").expect("scratch");
    let set = profiles::write(
        &dir,
        LogProfile::ReceiverLogs,
        &records,
        ExportFormat::Jsonl,
    )
    .expect("write");
    let data = set
        .node_files
        .iter()
        .find(|f| f.format == ExportFormat::Jsonl)
        .expect("a data file");
    let text = std::fs::read_to_string(&data.path).expect("read");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut row: serde_json::Value = serde_json::from_str(&lines[0]).expect("json");
    row["rssi_dbm"] = serde_json::json!(-70.256_789);
    lines[0] = serde_json::to_string(&row).expect("json");
    std::fs::write(&data.path, format!("{}\n", lines.join("\n"))).expect("write");

    let err = scan_export(&set.node_files).expect_err("the scan must refuse");
    match err {
        RecordError::OffGrid { field, quantum, .. } => {
            assert_eq!(field, "rssi_dbm");
            assert_eq!(quantum, 1e-2);
        }
        other => panic!("expected OffGrid, got {other}"),
    }
}

/// The VeReMi export quantises on the way out, whatever the caller handed in.
#[test]
fn the_veremi_export_quantises_everything_it_is_given() {
    use v2xw_record::dataset::veremi::{
        VeremiExport, VeremiReception, VeremiTruth, write as write_veremi,
    };

    let mut e = VeremiExport::default();
    e.by_receiver.insert(
        1,
        vec![VeremiReception {
            receiver: 1,
            rcv_time: 10.000_123_4,
            send_time: 9.999_876_5,
            sender: 12_345,
            sender_pseudo: "aabbccddeeff0011".to_string(),
            message_id: 7,
            pos: [100.123_456_7, 200.987_654_3, 0.5],
            spd: [12.345_678_9, 0.0, 0.0],
            rssi: -72.567_89,
        }],
    );
    e.truth.push(VeremiTruth {
        send_time: 9.999_876_5,
        sender: 42,
        sender_pseudo: "aabbccddeeff0011".to_string(),
        message_id: 7,
        pos: [100.0, 200.0, 0.0],
        spd: [12.345_678_9, 0.0, 0.0],
        attacker_type: 2,
    });

    let dir = scratch_dir("grid-veremi").expect("scratch");
    let files = write_veremi(&dir, &e).expect("write");
    let trace = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("traceJSON"))
        .expect("a trace file");
    let rows = v2xw_record::export::jsonl::read(&trace.path).expect("read");
    let row = rows
        .iter()
        .find(|r| r["type"] == 3)
        .expect("a reception line");
    assert_eq!(row["pos"][0], 100.123);
    assert_eq!(row["pos"][1], 200.988);
    assert_eq!(row["spd"][0], 12.346);
    assert_eq!(row["RSSI"], -72.57);
    assert_eq!(row["rcvTime"], 10.0);
    for (key, quantum) in [("pos", 1e-3), ("spd", 1e-3)] {
        for v in row[key].as_array().expect("array") {
            let x = v.as_f64().expect("number");
            assert!(
                v2xw_core::math::is_on_grid(x, quantum),
                "{key} element {x} is off its {quantum} grid"
            );
        }
    }
    let truth_file = files
        .iter()
        .find(|f| f.path.ends_with("GroundTruthJSONlog.json"))
        .expect("the ground-truth log");
    let truth = v2xw_record::export::jsonl::read(&truth_file.path).expect("read");
    assert_eq!(truth[0]["spd"][0], 12.346);
}

/// Every float is quantised **at the writer**, not by a later pass: a dataset assembled
/// from records whose values are already unquantised comes out on grid.
#[test]
fn the_writer_quantises_rather_than_trusting_its_input() {
    let mut a = ds::DatasetAssembler::new(DatasetProfile::V2);
    a.ingest(&RecordedRecord {
        channel: "gt.spawn".to_string(),
        sim_time: 0,
        json: br#"{"t":0,"actor":1,"is_attacker":false}"#.to_vec(),
    })
    .expect("spawn");
    // A kinematics record with raw IEEE-754 values, which is what a producer at a low
    // tier or a Python plug-in might legitimately hand over.
    a.ingest(&RecordedRecord {
        channel: "gt.kinematics".to_string(),
        sim_time: 1_000_000_000,
        json: br#"{"t":1000000000,"actor":1,"x_m":123.45678901234,
                  "y_m":0.1234567890123,"speed_mps":13.888888888888889,
                  "heading_rad":1.5707963267948966}"#
            .to_vec(),
    })
    .expect("kinematics");
    let d = a.finish().expect("finish");
    let root = scratch_dir("grid-writer-side").expect("scratch");
    DatasetWriter::new(&root)
        .expect("writer")
        .write(&d, &provenance())
        .expect("write");
    let report = scan_dataset(&root).expect("the writer quantised on the way out");
    assert!(report.values > 0);

    let rows =
        v2xw_record::export::jsonl::read(root.join("ground_truth/gt_emissions_sample.jsonl"))
            .expect("read");
    assert_eq!(rows[0]["true_x"], 123.457);
    assert_eq!(rows[0]["claimed_speed"], 13.889);
    let kin =
        v2xw_record::export::jsonl::read(root.join("ground_truth/gt_kinematics_sample.jsonl"))
            .expect("read");
    // π/2 rounded to the 1e-4 ratio grid. Written as the quantised constant rather
    // than the literal 1.5708 so clippy does not read it as a sloppy approximation of
    // `FRAC_PI_2`: it is not an approximation, it is the grid point.
    let want = v2xw_core::math::quantize_to(std::f64::consts::FRAC_PI_2, 1e-4);
    assert_eq!(kin[0]["true_heading"], want, "the ratio grid, 1e-4");
    assert!(
        v2xw_core::math::is_on_grid(want, 1e-4)
            && (want - std::f64::consts::FRAC_PI_2).abs() < 1e-4,
        "π/2 lands on the 1e-4 grid within half a quantum, and stays there"
    );
}
