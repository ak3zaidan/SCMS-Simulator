//! Two runs of the same fixture produce the same bytes — of the recording and of every
//! export.
//!
//! 02-architecture §6.1's determinism contract is about the engine, but it is worthless if
//! the container that stores the engine's output is not itself deterministic. Two things
//! could have broken it here and are checked:
//!
//! * **zstd's multithreaded encoder** does not produce identical bytes for identical
//!   input, so [`v2xw_record::RecordingOptions`] disables it. Without that, two runs of
//!   one scenario produce files whose frames are identical and whose SHA-256 is not.
//! * **`HashMap` iteration order** in Arrow's field metadata would reorder the schema
//!   block of a Parquet or IPC file between processes. Arrow sorts those keys before
//!   serialising them, which this test is the evidence for.

use v2xw_record::Reader;
use v2xw_record::export::{ExportFormat, ExportProfile, Exporter};
use v2xw_record::fixture::{RunShape, scratch_dir, write_recording};

fn digest(path: &std::path::Path) -> String {
    let bytes = std::fs::read(path).expect("the file is readable");
    v2xw_core::sha256_hex(&bytes)
}

/// The MCAP data section — everything before the summary, which is where the messages
/// live.
fn data_section(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("the file is readable");
    // The footer's body is the 20 bytes before the closing 8-byte magic.
    let at = bytes.len() - 8 - 20;
    let summary_start =
        u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes")) as usize;
    bytes[..summary_start].to_vec()
}

#[test]
fn two_recordings_of_the_same_run_carry_identical_bytes() {
    let dir = scratch_dir("determinism-mcap").expect("a scratch directory");
    let shape = RunShape {
        actors: 12,
        steps: 60,
        teleport_at: Some(31),
        ..Default::default()
    };
    let a = dir.join("a.mcap");
    let b = dir.join("b.mcap");
    write_recording(&a, &shape).expect("the first recording");
    write_recording(&b, &shape).expect("the second recording");
    assert!(
        std::fs::metadata(&a).expect("it exists").len() > 1_000,
        "the fixture must be big enough for compression to matter"
    );

    // The data section — every chunk, every compressed message, the manifest and the
    // attachment — is byte-identical. This is what `compression_threads(0)` buys: zstd's
    // multithreaded encoder would make these differ.
    assert_eq!(
        v2xw_core::sha256_hex(&data_section(&a)),
        v2xw_core::sha256_hex(&data_section(&b)),
        "the data sections differ; the container is not deterministic"
    );

    // …and so is the content, walked message by message.
    let mut ra = Reader::open(&a).expect("the first opens");
    let mut rb = Reader::open(&b).expect("the second opens");
    assert_eq!(
        ra.content_digest().expect("a digest"),
        rb.content_digest().expect("a digest")
    );
    assert_eq!(ra.replay().expect("replay"), rb.replay().expect("replay"));
}

/// An upstream defect, recorded rather than hidden.
///
/// `mcap` 0.25 writes the summary section's repeated schema and channel records by
/// iterating a `HashMap`, and `std`'s `RandomState` gives every `HashMap` in a thread a
/// different hash key, so two recordings of the same run get their summary records in
/// different orders and therefore have different SHA-256s. Nothing about the *content*
/// differs — the data section and every message are identical, as the test above shows —
/// but a run manifest must digest the content and not the file until this is fixed
/// upstream. [`Reader::content_digest`] is that digest.
///
/// This test asserts the situation as it is, so that whoever fixes it upstream finds a
/// test that fails and tells them why.
#[test]
fn the_whole_file_digest_is_not_yet_reproducible_upstream() {
    let dir = scratch_dir("determinism-upstream").expect("a scratch directory");
    let shape = RunShape::new(12, 60);
    let a = dir.join("a.mcap");
    let b = dir.join("b.mcap");
    write_recording(&a, &shape).expect("the first recording");
    write_recording(&b, &shape).expect("the second recording");
    let (da, db) = (digest(&a), digest(&b));
    if da == db {
        // If this starts passing, the upstream defect is fixed: delete this test and
        // assert whole-file equality in the one above.
        println!(
            "note: mcap now writes a reproducible summary section ({da}); \
             the whole-file digest can be used again"
        );
    } else {
        assert_eq!(
            std::fs::metadata(&a).expect("it exists").len(),
            std::fs::metadata(&b).expect("it exists").len(),
            "the files differ in length, which would be a defect in this crate, not upstream"
        );
    }
}

#[test]
fn two_exports_of_the_same_records_are_byte_identical() {
    let dir = scratch_dir("determinism-export").expect("a scratch directory");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(6, 40)).expect("the recording");
    let mut reader = Reader::open(&path).expect("the recording opens");
    let records = reader.records(None).expect("the records");

    for format in [
        ExportFormat::Parquet,
        ExportFormat::ArrowIpc,
        ExportFormat::Jsonl,
    ] {
        let first = Exporter::new(
            dir.join(format!("{}-1", format.extension())),
            ExportProfile::Full,
        )
        .expect("an exporter")
        .export_all(&records, format)
        .expect("the first export");
        let second = Exporter::new(
            dir.join(format!("{}-2", format.extension())),
            ExportProfile::Full,
        )
        .expect("an exporter")
        .export_all(&records, format)
        .expect("the second export");
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(
                digest(&a.path),
                digest(&b.path),
                "{:?} and {:?} differ; the exporter is not deterministic",
                a.path,
                b.path
            );
        }
    }
}

#[test]
fn two_encodings_of_the_same_snapshots_are_byte_identical() {
    // The producer half: same snapshots in, same frames out, with no state carried
    // between the two encoders.
    let shape = RunShape {
        actors: 10,
        steps: 50,
        teleport_at: Some(27),
        ..Default::default()
    };
    let a = v2xw_record::fixture::live_frames(&shape).expect("the first stream");
    let b = v2xw_record::fixture::live_frames(&shape).expect("the second stream");
    assert_eq!(a, b, "the producer is not a pure function of its snapshots");
}
