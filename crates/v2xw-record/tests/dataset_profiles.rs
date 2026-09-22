//! The non-`ma-dataset` profiles, end to end: VeReMi, `receiver-logs`, `telemetry`,
//! `net-trace` — 08-measurement-and-data.md §5.

use std::collections::BTreeSet;

use v2xw_record::dataset::fixture::{DatasetShape, records};
use v2xw_record::dataset::veremi::{self, VEREMI_COMPATIBILITY_NOTE, attacker_type_code};
use v2xw_record::dataset::{LogProfile, profiles};
use v2xw_record::export::{ExportFormat, jsonl};
use v2xw_record::fixture::scratch_dir;

// ---------------------------------------------------------------------------------------
// VeReMi
// ---------------------------------------------------------------------------------------

/// The builder turns a run into a VeReMi trace with the fields the literature's readers
/// expect, and the trace is joinable to the ground-truth log by `messageID`.
#[test]
fn a_run_becomes_a_joinable_veremi_trace() {
    let recs = records(&DatasetShape::default());
    let export = veremi::from_records(&recs).expect("build");
    assert!(
        !export.by_receiver.is_empty(),
        "no receiver produced a trace"
    );
    assert!(!export.truth.is_empty(), "no ground-truth log");

    let dir = scratch_dir("veremi-e2e").expect("scratch");
    let files = veremi::write(&dir, &export).expect("write");

    // One file per receiver, one ground-truth log, one note.
    let names: BTreeSet<String> = files
        .iter()
        .map(|f| {
            f.path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert!(names.contains("GroundTruthJSONlog.json"));
    assert!(names.contains("VEREMI-NOTE.txt"));
    assert!(
        names.iter().any(|n| n.starts_with("traceJSON-")),
        "no per-receiver trace: {names:?}"
    );

    // The join: every `messageID` in a receiver trace resolves in the ground-truth log.
    let truth = jsonl::read(dir.join("GroundTruthJSONlog.json")).expect("read");
    let truth_ids: BTreeSet<u64> = truth
        .iter()
        .map(|r| r["messageID"].as_u64().expect("messageID"))
        .collect();
    assert_eq!(
        truth_ids.len(),
        truth.len(),
        "duplicate messageID in the truth log"
    );

    let mut receptions = 0usize;
    for f in files.iter().filter(|f| {
        f.path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("traceJSON-"))
    }) {
        let rows = jsonl::read(&f.path).expect("read");
        assert!(!rows.is_empty(), "{:?} is empty", f.path);
        let mut last = f64::NEG_INFINITY;
        for row in &rows {
            let t = row["rcvTime"].as_f64().expect("rcvTime");
            assert!(t >= last, "a trace must be in time order");
            last = t;
            match row["type"].as_i64() {
                Some(3) => {
                    let id = row["messageID"].as_u64().expect("messageID");
                    assert!(
                        truth_ids.contains(&id),
                        "messageID {id} has no ground-truth row to join to"
                    );
                    assert!(row["sendTime"].as_f64().expect("sendTime") <= t + 1e-9);
                    receptions += 1;
                }
                // The receiver's own position report.
                Some(2) => {
                    assert!(row.get("pos").is_some());
                    assert!(row.get("messageID").is_none());
                }
                other => panic!("unexpected VeReMi type {other:?}"),
            }
        }
    }
    assert!(
        receptions > 10,
        "only {receptions} receptions in the whole trace"
    );
}

/// The deviation, verified over the written bytes: no receiver trace carries a device
/// identity, and the ground-truth log does.
#[test]
fn the_written_trace_holds_no_device_identity_and_the_truth_log_does() {
    let recs = records(&DatasetShape::default());
    let export = veremi::from_records(&recs).expect("build");
    let dir = scratch_dir("veremi-separation").expect("scratch");
    let files = veremi::write(&dir, &export).expect("write");

    // The pseudonym → `sender` mapping is injective on pseudonyms and independent of the
    // device, so the set of `sender` values in a trace is larger than the fleet whenever
    // devices rotate pseudonyms. That is the property a linkability attack loses.
    let mut trace_senders = BTreeSet::new();
    let mut trace_pseudonyms = BTreeSet::new();
    for f in files.iter().filter(|f| {
        f.path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("traceJSON-"))
    }) {
        for row in jsonl::read(&f.path).expect("read") {
            if row["type"] == 3 {
                trace_senders.insert(row["sender"].as_i64().expect("sender"));
                trace_pseudonyms.insert(
                    row["senderPseudo"]
                        .as_str()
                        .expect("senderPseudo")
                        .to_string(),
                );
            }
        }
    }
    assert_eq!(
        trace_senders.len(),
        trace_pseudonyms.len(),
        "`sender` must be one-to-one with the pseudonym, never with the device"
    );
    let devices: BTreeSet<i64> = jsonl::read(dir.join("GroundTruthJSONlog.json"))
        .expect("read")
        .iter()
        .map(|r| r["sender"].as_i64().expect("sender"))
        .collect();
    assert!(
        trace_pseudonyms.len() > devices.len(),
        "the fixture must rotate pseudonyms ({} pseudonyms, {} devices), or this property \
         is untested",
        trace_pseudonyms.len(),
        devices.len()
    );
    // And the note travels with the data.
    let note = std::fs::read_to_string(dir.join("VEREMI-NOTE.txt")).expect("note");
    assert_eq!(note, VEREMI_COMPATIBILITY_NOTE);
}

/// `attackerType` is `0` for a benign message and the family's code inside the attack
/// window — the label a misbehaviour-detection benchmark trains against.
#[test]
fn the_ground_truth_log_labels_the_attack_window_and_nothing_outside_it() {
    let recs = records(&DatasetShape::default());
    let export = veremi::from_records(&recs).expect("build");
    let labelled = export.truth.iter().filter(|t| t.attacker_type != 0).count();
    let benign = export.truth.iter().filter(|t| t.attacker_type == 0).count();
    assert!(labelled > 0, "no message was labelled as an attack");
    assert!(benign > 0, "every message was labelled as an attack");
    for t in &export.truth {
        assert!(
            t.attacker_type == 0 || t.attacker_type == attacker_type_code("ConstPosOffset"),
            "unexpected attacker code {}",
            t.attacker_type
        );
    }
}

/// An all-benign run produces a trace whose every label is `0`. The negative control: a
/// labeller that always fired would pass the test above.
#[test]
fn an_all_benign_run_produces_no_attack_label_at_all() {
    let recs = records(&DatasetShape::all_benign());
    let export = veremi::from_records(&recs).expect("build");
    assert!(!export.truth.is_empty(), "no messages at all");
    assert!(
        export.truth.iter().all(|t| t.attacker_type == 0),
        "an all-benign run produced an attack label"
    );
}

/// A lost frame is not a trace line: VeReMi logs what a receiver received, and a loss
/// belongs in the net trace.
#[test]
fn a_lost_frame_does_not_appear_in_a_veremi_trace_but_does_appear_in_the_net_trace() {
    let recs = records(&DatasetShape::default());
    let lost: usize = recs
        .iter()
        .filter(|r| r.channel == "phy.rx")
        .filter(|r| {
            serde_json::from_slice::<serde_json::Value>(&r.json)
                .map(|v| v["outcome"] == "lost")
                .unwrap_or(false)
        })
        .count();
    assert!(lost > 0, "the fixture lost no frames, so this is untested");

    let export = veremi::from_records(&recs).expect("build");
    let traced: usize = export.by_receiver.values().map(Vec::len).sum();
    let received: usize = recs
        .iter()
        .filter(|r| r.channel == "phy.rx")
        .filter(|r| {
            serde_json::from_slice::<serde_json::Value>(&r.json)
                .map(|v| v["outcome"] == "ok")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        traced, received,
        "the trace must hold exactly the receptions"
    );

    // …and the net trace holds every attempt, with its cause.
    let dir = scratch_dir("veremi-vs-net-trace").expect("scratch");
    let set =
        profiles::write(&dir, LogProfile::NetTrace, &recs, ExportFormat::Jsonl).expect("write");
    let rx_table = set
        .node_files
        .iter()
        .find(|f| f.channel.as_deref() == Some("phy.rx") && f.format == ExportFormat::Jsonl)
        .expect("a phy.rx table");
    let rows = jsonl::read(&rx_table.path).expect("read");
    assert_eq!(
        rows.len(),
        received + lost,
        "the net trace must hold every attempt"
    );
    assert!(
        rows.iter()
            .any(|r| r["outcome"] == "lost" && r["cause"] == "per"),
        "the loss cause did not survive into the net trace"
    );
}

// ---------------------------------------------------------------------------------------
// The three §5 log profiles
// ---------------------------------------------------------------------------------------

/// Each profile writes the channels §5 assigns it, in Parquet, with a schema sidecar, and
/// lints clean.
#[test]
fn each_log_profile_writes_its_channels_as_parquet_with_a_schema_sidecar() {
    let recs = records(&DatasetShape::default());
    for profile in [
        LogProfile::ReceiverLogs,
        LogProfile::Telemetry,
        LogProfile::NetTrace,
    ] {
        let dir = scratch_dir(&format!("profile-parquet-{}", profile.dir_name())).expect("scratch");
        let set = profiles::write(&dir, profile, &recs, ExportFormat::Parquet).expect("write");

        let data: Vec<_> = set
            .node_files
            .iter()
            .filter(|f| f.format == ExportFormat::Parquet)
            .collect();
        assert!(!data.is_empty(), "{} wrote no Parquet", profile.dir_name());
        for f in &data {
            assert!(f.rows > 0, "{:?} has no rows", f.path);
            let schema = f.schema.as_ref().expect("a schema");
            assert_eq!(schema.schema, profile.schema_id());
            // Every float column declares its grid — the contract D9 puts on a schema.
            for col in &schema.columns {
                if col.kind == v2xw_record::export::ColumnKind::Float {
                    assert!(col.quantum.is_some(), "{} declares no grid", col.name);
                }
            }
            // The sidecar is on disk beside the data and round-trips.
            let sidecar = f.path.with_extension("").with_extension("schema.json");
            let bytes =
                std::fs::read(&sidecar).unwrap_or_else(|e| panic!("{}: {e}", sidecar.display()));
            let read_back = v2xw_record::export::TableSchema::from_json(&bytes).expect("parse");
            assert_eq!(&read_back, schema);
            // Parquet reads back with the row count it claims.
            let batches = v2xw_record::export::parquet::read(&f.path).expect("read");
            let rows: usize = batches
                .iter()
                .map(arrow::array::RecordBatch::num_rows)
                .sum();
            assert_eq!(rows, f.rows);
        }
        assert!(set.lint().expect("lint").is_clean());
    }
}

/// `net-trace` covers all four channels §5 names for it, whenever the run has them.
#[test]
fn a_net_trace_covers_every_channel_section_five_assigns_it() {
    let recs = records(&DatasetShape::default());
    let dir = scratch_dir("profile-net-trace-channels").expect("scratch");
    let set =
        profiles::write(&dir, LogProfile::NetTrace, &recs, ExportFormat::Jsonl).expect("write");
    let covered: BTreeSet<&str> = set
        .node_files
        .iter()
        .filter_map(|f| f.channel.as_deref())
        .collect();
    // `net.frag` is absent from the fixture (nothing fragments a 421-byte BSM), so it is
    // legitimately missing; the other three must be there.
    for channel in ["node.tx", "phy.rx", "mac.cbr"] {
        assert!(covered.contains(channel), "the net trace has no {channel}");
    }
    assert!(
        LogProfile::NetTrace.channels().contains(&"net.frag"),
        "net.frag must still be in the profile's channel list, for a run that fragments"
    );
}

/// `telemetry` is the HUD-equivalent data: one series per node, with the resource fields.
#[test]
fn the_telemetry_profile_is_a_per_node_series_with_the_resource_fields() {
    let recs = records(&DatasetShape::default());
    let dir = scratch_dir("profile-telemetry-fields").expect("scratch");
    let set =
        profiles::write(&dir, LogProfile::Telemetry, &recs, ExportFormat::Jsonl).expect("write");
    let data: Vec<_> = set
        .node_files
        .iter()
        .filter(|f| f.format == ExportFormat::Jsonl)
        .collect();
    assert_eq!(
        data.len(),
        DatasetShape::default().devices as usize,
        "one series per node"
    );
    for f in &data {
        let rows = jsonl::read(&f.path).expect("read");
        assert!(!rows.is_empty());
        for field in [
            "cpu",
            "hsm",
            "ram_bytes",
            "storage_bytes",
            "verify_queue_depth",
        ] {
            assert!(
                rows[0].get(field).is_some(),
                "{field} is missing from the telemetry series"
            );
        }
        // One node per file, which is what makes the split useful.
        let nodes: BTreeSet<i64> = rows
            .iter()
            .map(|r| r["node"].as_i64().expect("node"))
            .collect();
        assert_eq!(nodes.len(), 1, "{:?} mixes nodes", f.path);
    }
}

/// Writing a profile twice produces the same bytes — the determinism property an
/// experiment's resume depends on.
#[test]
fn writing_a_profile_twice_produces_the_same_bytes() {
    let recs = records(&DatasetShape::default());
    let digest = |tag: &str| {
        let dir = scratch_dir(tag).expect("scratch");
        let set = profiles::write(&dir, LogProfile::ReceiverLogs, &recs, ExportFormat::Jsonl)
            .expect("write");
        let mut all = Vec::new();
        for f in set.all() {
            all.extend_from_slice(
                f.path
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .as_bytes(),
            );
            all.extend_from_slice(&std::fs::read(&f.path).expect("read"));
        }
        v2xw_core::hash::sha256_hex(&all)
    };
    assert_eq!(digest("profile-repro-a"), digest("profile-repro-b"));

    let veremi_digest = |tag: &str| {
        let dir = scratch_dir(tag).expect("scratch");
        let export = veremi::from_records(&recs).expect("build");
        let files = veremi::write(&dir, &export).expect("write");
        let mut all = Vec::new();
        for f in &files {
            all.extend_from_slice(&std::fs::read(&f.path).expect("read"));
        }
        v2xw_core::hash::sha256_hex(&all)
    };
    assert_eq!(
        veremi_digest("veremi-repro-a"),
        veremi_digest("veremi-repro-b")
    );
}
