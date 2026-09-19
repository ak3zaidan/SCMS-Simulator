//! INDEPENDENT VERIFIER INSTRUMENTATION — temporary, removed after the run.
//!
//! "Prove every repaired check can fail." Each gate is given the exact fault it exists to
//! catch, built by hand, and the error it returns is named.

use v2xw_core::{OwnedRecord, Visibility};
use v2xw_record::export::{ExportFormat, ExportProfile, Exporter, scan};
use v2xw_record::fixture::{RunShape, scratch_dir};
use v2xw_record::wire::snapshot::{DeltaBody, KeyframeBody, PROFILE_FULL, PROFILE_NODE};
use v2xw_record::wire::{FLAG_NODE_ONLY, Frame, MsgType};
use v2xw_record::{Reader, RecordError, RecordingOptions, RecordingWriter};

fn base_frames() -> Vec<Frame> {
    v2xw_record::fixture::live_frames(&RunShape::new(4, 25)).expect("frames")
}

/// Writes `frames` and returns what `verify` said about them.
fn verify_of(tag: &str, frames: &[Frame]) -> Result<v2xw_record::VerifyReport, RecordError> {
    let dir = scratch_dir(tag)?;
    let path = dir.join("run.mcap");
    let mut w = RecordingWriter::create(&path, RecordingOptions::default())?;
    w.write_manifest(r#"{"schema":"v2xw/manifest/1"}"#)?;
    for f in frames {
        w.write_frame(f)?;
    }
    w.finish()?;
    Reader::open(&path)?.verify()
}

fn expect_inconsistent(tag: &str, frames: &[Frame], needle: &str) {
    match verify_of(tag, frames) {
        Err(RecordError::Inconsistent { detail, .. }) => {
            assert!(
                detail.contains(needle),
                "{tag}: verify said {detail:?}, which does not mention {needle:?}"
            );
            println!("INDEP-VERIFY {tag} -> {detail}");
        }
        Err(e) => panic!("{tag}: expected Inconsistent, got {e}"),
        Ok(r) => panic!("{tag}: verify ACCEPTED the fault ({r:?})"),
    }
}

#[test]
fn the_clean_fixture_verifies_so_the_gate_tests_mean_something() {
    let r = verify_of("gate-clean", &base_frames()).expect("the clean fixture verifies");
    assert!(r.frames > 20, "{r:?}");
    println!(
        "INDEP-VERIFY clean: {} frames, {} keyframes, {} deltas, {} prov ids, {} prov refs",
        r.frames, r.keyframes, r.deltas, r.provenance_ids, r.provenance_references
    );
    assert!(
        r.provenance_references > 0,
        "the C5 check must have had something to check, or it is vacuous"
    );
}

#[test]
fn verify_catches_a_gap_in_the_canonical_sequence() {
    let frames = base_frames();
    // Drop the third delta: every following canonical seq is then one past what is due.
    let mut out = Vec::new();
    let mut dropped = false;
    for f in frames {
        if !dropped && f.header().unwrap().kind() == Some(MsgType::Delta) {
            dropped = true;
            continue;
        }
        out.push(f);
    }
    assert!(dropped);
    expect_inconsistent("gate-seq", &out, "not dense");
}

#[test]
fn verify_catches_a_gop_index_that_does_not_follow() {
    let mut frames = base_frames();
    let mut done = false;
    for f in &mut frames {
        let h = f.header().unwrap();
        if h.kind() == Some(MsgType::Keyframe) && h.seq > 0 && !done {
            let mut b = KeyframeBody::decode(f.body()).unwrap();
            b.gop_index += 7;
            *f = b.to_frame(h.seq, h.flags).unwrap();
            done = true;
        }
    }
    assert!(done, "the fixture needs a second keyframe");
    expect_inconsistent("gate-gop", &frames, "does not follow");
}

#[test]
fn verify_catches_a_step_index_that_does_not_follow() {
    let mut frames = base_frames();
    let mut done = false;
    for f in &mut frames {
        let h = f.header().unwrap();
        if h.kind() == Some(MsgType::Delta) && !done {
            let mut b = DeltaBody::decode(f.body()).unwrap();
            b.step_index += 3;
            *f = b.to_frame(h.seq, h.flags).unwrap();
            done = true;
        }
    }
    assert!(done);
    expect_inconsistent("gate-step", &frames, "does not follow");
}

#[test]
fn verify_catches_a_delta_rooted_in_nothing() {
    let frames = base_frames();
    // Everything but the first keyframe, renumbered so the seq stays dense — so the ONLY
    // thing wrong is that a delta has no keyframe before it.
    let mut out = Vec::new();
    let mut seq = 0u64;
    let mut dropped_kf = false;
    for f in frames {
        let h = f.header().unwrap();
        if !dropped_kf && h.kind() == Some(MsgType::Keyframe) {
            dropped_kf = true;
            continue;
        }
        if h.kind().is_some_and(MsgType::is_canonical) {
            out.push(f.renumbered(seq).unwrap());
            seq += 1;
        } else {
            out.push(f.renumbered(seq).unwrap());
        }
    }
    expect_inconsistent("gate-orphan", &out, "rooted in nothing");
}

#[test]
fn verify_catches_a_profile_word_that_disagrees_with_the_flag() {
    let mut frames = base_frames();
    let mut done = false;
    for f in &mut frames {
        let h = f.header().unwrap();
        if h.kind() == Some(MsgType::Keyframe) && !done {
            let mut b = KeyframeBody::decode(f.body()).unwrap();
            assert_eq!(b.profile, PROFILE_FULL);
            b.profile = PROFILE_NODE; // …while the frame carries no FLAG_NODE_ONLY.
            *f = b.to_frame(h.seq, h.flags).unwrap();
            done = true;
        }
    }
    assert!(done);
    expect_inconsistent("gate-profile", &frames, "FLAG_NODE_ONLY");
}

#[test]
fn verify_catches_a_profile_that_changes_mid_stream() {
    let mut frames = base_frames();
    let mut n = 0;
    for f in &mut frames {
        let h = f.header().unwrap();
        if h.kind().is_some_and(MsgType::is_canonical) {
            n += 1;
            if n > 6 && h.kind() == Some(MsgType::Delta) {
                // A blind delta in the middle of a full stream. The keyframe's profile
                // word is untouched, so the only fault is the flag changing.
                *f = f.with_flags(h.flags | FLAG_NODE_ONLY);
                break;
            }
        }
    }
    expect_inconsistent("gate-flagflip", &frames, "FLAG_NODE_ONLY changes");
}

#[test]
fn verify_catches_c5_a_prov_id_referenced_before_it_was_delivered() {
    // Conformance C5. The fixture now delivers a Provenance frame before the first
    // MetricSample that names one; removing it is the fault the check exists for.
    let frames = base_frames();
    let mut out = Vec::new();
    let mut seq = 0u64;
    let mut removed = false;
    for f in frames {
        let h = f.header().unwrap();
        if h.kind() == Some(MsgType::Provenance) {
            removed = true;
            continue;
        }
        out.push(f.renumbered(seq).unwrap());
        if h.kind().is_some_and(MsgType::is_canonical) {
            seq += 1;
        }
    }
    assert!(removed, "the fixture must deliver a Provenance frame at all");
    expect_inconsistent("gate-c5", &out, "before any Provenance frame");
}

// ------------------------------------------------------------- D9, record path

fn writer_for(tag: &str) -> RecordingWriter<std::io::BufWriter<std::fs::File>> {
    let dir = scratch_dir(tag).expect("scratch");
    RecordingWriter::create(dir.join("g.mcap"), RecordingOptions::default()).expect("writer")
}

#[test]
fn the_record_gate_reaches_a_float_however_deeply_it_is_buried() {
    let mut w = writer_for("gate-deep");
    // Shapes the register's own case did not cover: an array of objects, an object inside
    // an array inside an object, an array of arrays, and a negative value.
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "array of objects",
            serde_json::json!({"node_id":1,"hits":[{"distance_m":1.23456789}]}),
        ),
        (
            "object in array in object",
            serde_json::json!({"node_id":1,"a":{"b":[{"c":{"rssi_dbm":-78.123456}}]}}),
        ),
        (
            "array of arrays",
            serde_json::json!({"node_id":1,"samples_m":[[1.0,2.0],[3.0,4.00000001]]}),
        ),
        (
            "negative off-grid",
            serde_json::json!({"node_id":1,"detail":{"distance_m":-0.00049}}),
        ),
        (
            "scientific notation",
            serde_json::json!({"node_id":1,"detail":{"distance_m":1.5e-7}}),
        ),
    ];
    for (what, json) in &cases {
        let r = OwnedRecord {
            channel: "node.tx",
            visibility: Visibility::Node,
            json: serde_json::to_vec(json).unwrap(),
        };
        match w.write_record(0, &r) {
            Err(RecordError::OffGrid { field, quantum, .. }) => {
                println!("INDEP-D9REC {what}: refused, field {field}, grid {quantum}");
            }
            other => panic!("{what}: expected OffGrid, got {other:?}"),
        }
    }
    // …and the on-grid forms all go in, so the gate is not simply refusing everything.
    for json in [
        serde_json::json!({"node_id":1,"hits":[{"distance_m":1.235}]}),
        serde_json::json!({"node_id":1,"a":{"b":[{"c":{"rssi_dbm":-78.12}}]}}),
        serde_json::json!({"node_id":1,"samples_m":[[1.0,2.0],[3.0,4.0]]}),
    ] {
        w.write_record(
            0,
            &OwnedRecord {
                channel: "node.tx",
                visibility: Visibility::Node,
                json: serde_json::to_vec(&json).unwrap(),
            },
        )
        .expect("an on-grid record stores");
    }
    w.finish().expect("finish");
    println!("INDEP-D9REC {} deep shapes refused, 3 on-grid shapes stored", cases.len());
}

/// The export scan, given an off-grid float inside a Text column — the shape it used to
/// `continue` past.
#[test]
fn the_export_scan_sees_inside_a_text_column() {
    let dir = scratch_dir("gate-scan").expect("scratch");
    let records: Vec<v2xw_record::RecordedRecord> = (0..3u64)
        .map(|k| v2xw_record::RecordedRecord {
            channel: "node.tx".to_string(),
            sim_time: k,
            json: serde_json::to_vec(&serde_json::json!({
                "node_id": 1,
                "airtime_ms": 0.526,
                "detail": {"distance_m": 123.457},
            }))
            .unwrap(),
        })
        .collect();
    let ex = Exporter::new(dir.join("out"), ExportProfile::Full).expect("exporter");
    let mut clean_files = Vec::new();
    for fmt in [ExportFormat::Jsonl, ExportFormat::Parquet, ExportFormat::ArrowIpc] {
        clean_files.extend(ex.export_channel("node.tx", &records, fmt).expect("export"));
    }
    let report = scan::scan_export(&clean_files).expect("a clean export scans");
    println!(
        "INDEP-SCAN clean export: {} files, {} rows, {} values",
        report.files, report.rows, report.values
    );
    assert!(report.values >= 6, "the scan looked at almost nothing");

    // Now hand-write a JSONL file with the SAME schema whose nested float is off grid,
    // and check the scan reports it rather than walking past the Text column.
    let jsonl = clean_files
        .iter()
        .find(|f| f.path.extension().is_some_and(|e| e == "jsonl"))
        .expect("a jsonl file")
        .clone();
    let text = std::fs::read_to_string(&jsonl.path).expect("read");
    let poisoned = text.replace("123.457", "123.4567891");
    assert_ne!(poisoned, text, "the substitution must have bitten");
    std::fs::write(&jsonl.path, &poisoned).expect("write");
    match scan::scan_export(std::slice::from_ref(&jsonl)) {
        Err(RecordError::OffGrid {
            field,
            value,
            quantum,
            ..
        }) => {
            println!("INDEP-SCAN poisoned nested float caught: {field} = {value} off {quantum}");
            assert_eq!(field, "distance_m");
            assert_eq!(quantum, 1e-3);
        }
        other => panic!("the scan walked past a nested off-grid float: {other:?}"),
    }
}
