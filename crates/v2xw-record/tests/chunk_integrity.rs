//! The chunk checksum can be switched off from the wire, and this crate says so.
//!
//! MCAP protects a chunk with a CRC-32 over its uncompressed records, and §4 of the
//! container specification adds: "A zero value indicates that CRC validation should not be
//! performed." `mcap::sans_io::LinearReader` implements that literally — its
//! `prevalidate_chunk_crcs` path is entered only `if state.crc != 0` — so the four bytes at
//! chunk-header offset 24 are not merely a checksum, they are the switch that turns the
//! checksum off, and they are four bytes of the file like any other.
//!
//! [`a_zero_checksum_switches_off_the_check_a_wrong_one_trips`] is the test that pins the
//! mechanism rather than the consequence: the same chunk, with the same bytes, is rejected
//! when its stored CRC is wrong and accepted when its stored CRC is zero. Nothing else in
//! the file changes. That is the defect stated as a fact about behaviour.
//!
//! # The policy
//!
//! Read it, and report that nothing was checked. `v2xw_record::index::ChunkIntegrity`
//! carries the reasoning in full; in short, refusing outright would reject a conforming
//! third-party producer, and accepting silently is the one outcome that must not happen —
//! a recording reported as verified when nothing was checked. So
//! [`v2xw_record::VerifyReport::integrity_verified`] goes false, the count is carried, and
//! a caller for whom a missing checksum really is disqualifying turns on
//! `Reader::require_chunk_checksums` and gets a named error instead.

use v2xw_record::RecordingOptions;
use v2xw_record::fixture::{RunShape, scratch_dir, write_recording, write_recording_with};
use v2xw_record::index::{ChunkIntegrity, SeekIndex};
use v2xw_record::{MemorySource, Reader, RecordError, Source};

/// A small, valid recording, as bytes.
fn good_recording(tag: &str) -> Vec<u8> {
    let dir = scratch_dir(tag).expect("a scratch directory");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(4, 30)).expect("the recording is written");
    std::fs::read(&path).expect("the recording is readable")
}

/// Rewrites the `uncompressed_crc` of every chunk record, returning how many it touched.
///
/// The chunk record is `opcode || length || message_start_time || message_end_time ||
/// uncompressed_size || uncompressed_crc || …`, so the CRC is 33 bytes past the record's
/// file offset: 9 of prefix and 24 of times and size. The offsets come from the recording's
/// own chunk index rather than from a scan, so the test cannot drift onto the wrong bytes.
fn set_chunk_crcs(bytes: &mut [u8], crc: u32) -> usize {
    let index = SeekIndex::read(&mut MemorySource::new(bytes.to_vec())).expect("it indexes");
    let mut touched = 0;
    for chunk in &index.chunks {
        let at = chunk.file_offset as usize + 9 + 8 + 8 + 8;
        bytes[at..at + 4].copy_from_slice(&crc.to_le_bytes());
        touched += 1;
    }
    touched
}

/// The same fixture with a chunk target so small that every GOP lands in its own chunk.
fn many_chunk_recording(tag: &str) -> Vec<u8> {
    let dir = scratch_dir(tag).expect("a scratch directory");
    let path = dir.join("run.mcap");
    let shape = RunShape::new(4, 60);
    write_recording_with(
        &path,
        &shape,
        RecordingOptions {
            cadence: shape.cadence,
            profile: shape.profile,
            chunk_target_bytes: 1,
            ..Default::default()
        },
        true,
    )
    .expect("the recording is written");
    std::fs::read(&path).expect("the recording is readable")
}

#[test]
fn a_recording_this_crate_wrote_carries_a_checksum_on_every_chunk() {
    let bytes = good_recording("integrity-good");
    let chunks = SeekIndex::read(&mut MemorySource::new(bytes.clone()))
        .expect("it indexes")
        .chunks
        .len() as u64;
    assert!(chunks > 0, "the fixture must contain a chunk");

    let mut reader = Reader::open_bytes(bytes).expect("it opens");
    let report = reader.verify().expect("it verifies");
    assert_eq!(report.chunks_checksummed, chunks, "every chunk was checked");
    assert_eq!(report.chunks_without_checksum, 0);
    assert!(
        report.integrity_verified(),
        "a recording this crate wrote is checked end to end"
    );

    // …and the strict mode is a no-op on it, which is the point of the strict mode being
    // usable at all.
    let mut strict = Reader::open_bytes(good_recording("integrity-good-strict")).expect("it opens");
    strict.require_chunk_checksums(true);
    assert!(strict.requires_chunk_checksums());
    strict
        .verify()
        .expect("a checksummed recording passes the strict reader");
}

#[test]
fn a_zero_checksum_switches_off_the_check_a_wrong_one_trips() {
    // The defect itself, as a pair of observations on one file.
    //
    // (a) A *wrong* non-zero CRC is caught: the reader verifies the chunk before parsing
    //     anything inside it, so the mismatch is the error.
    let mut wrong = good_recording("integrity-switch");
    let touched = set_chunk_crcs(&mut wrong, 0xDEAD_BEEF);
    assert!(touched >= 1, "the fixture must carry a chunk to patch");
    let err = Reader::open_bytes(wrong.clone())
        .expect("the container itself is still well formed")
        .verify()
        .expect_err("a chunk whose stored CRC does not match its contents is corrupt");
    assert!(
        err.to_string().to_lowercase().contains("crc"),
        "the error should name the check that failed: {err}"
    );

    // (b) The same chunk, same contents, CRC set to zero instead: accepted, because zero
    //     means "not present" and the check is skipped entirely. Nothing about the data
    //     changed between (a) and (b) — only the switch.
    let mut zeroed = good_recording("integrity-switch-zero");
    assert_eq!(set_chunk_crcs(&mut zeroed, 0), touched);
    let mut reader = Reader::open_bytes(zeroed).expect("it opens");
    let report = reader
        .verify()
        .expect("a zero CRC is legal: the recording is read, not refused");

    // …and this is the part that must never regress: it is read, but it is not called
    // verified.
    assert_eq!(report.chunks_checksummed, 0, "nothing was checked");
    assert_eq!(report.chunks_without_checksum, touched as u64);
    assert!(
        !report.integrity_verified(),
        "the crate must not report a file as verified when it checked nothing"
    );
}

#[test]
fn one_chunk_without_a_checksum_is_enough_to_withhold_the_verified_claim() {
    // The realistic shape of tampering: not every chunk, just the one that was edited.
    let mut bytes = many_chunk_recording("integrity-one");
    let index = SeekIndex::read(&mut MemorySource::new(bytes.clone())).expect("it indexes");
    let total = index.chunks.len() as u64;
    assert!(total >= 2, "this test needs a recording of several chunks");
    let at = index.chunks[1].file_offset as usize + 9 + 8 + 8 + 8;
    bytes[at..at + 4].copy_from_slice(&0u32.to_le_bytes());

    let mut reader = Reader::open_bytes(bytes).expect("it opens");
    let report = reader.verify().expect("the recording still reads");
    assert_eq!(report.chunks_without_checksum, 1);
    assert_eq!(report.chunks_checksummed, total - 1);
    assert!(
        !report.integrity_verified(),
        "one unchecked chunk is one unchecked chunk"
    );
}

#[test]
fn the_strict_reader_refuses_an_unchecked_chunk_by_name_on_every_path() {
    // A caller for whom a missing checksum is disqualifying gets a named error rather than
    // a report it has to remember to read — and gets it from `verify`, `replay` and `seek`
    // alike, because a strictness that only one entry point honoured would be no
    // strictness at all. That was the shape of the original chunk-reader defect too.
    let mut bytes = good_recording("integrity-strict");
    assert!(set_chunk_crcs(&mut bytes, 0) >= 1);

    for path in ["verify", "replay", "records", "digest", "seek"] {
        let mut reader = Reader::open_bytes(bytes.clone()).expect("it opens");
        reader.require_chunk_checksums(true);
        let span = reader.index().snapshot_span();
        let err = match path {
            "verify" => reader.verify().err(),
            "replay" => reader.replay().err(),
            "records" => reader.records(None).err(),
            "digest" => reader.content_digest().err(),
            _ => {
                let (min, _) = span.expect("the fixture has a span");
                reader.seek(min).err()
            }
        };
        let err =
            err.unwrap_or_else(|| panic!("{path}: the strict reader accepted an unchecked chunk"));
        assert!(
            matches!(err, RecordError::UncheckedChunk { .. }),
            "{path}: got {err}"
        );
        assert!(
            err.to_string().contains("nothing in it was checked"),
            "{path}: the error should say what was not done: {err}"
        );
    }
}

#[test]
fn the_chunk_walk_reports_which_of_the_two_happened() {
    // The unit under all of the above: `for_each_message_in_chunk` returns the fact rather
    // than discarding it. A caller that never asks still cannot be misled, because the
    // aggregate is on the report.
    let bytes = good_recording("integrity-unit");
    let mut src = MemorySource::new(bytes.clone());
    let index = SeekIndex::read(&mut src).expect("it indexes");
    let seen = index
        .for_each_message_in_chunk(&mut src, 0, |_, _, _| Ok(()))
        .expect("chunk 0 reads");
    assert_eq!(seen, ChunkIntegrity::Verified);
    assert!(seen.is_verified());

    let mut zeroed = bytes;
    assert!(set_chunk_crcs(&mut zeroed, 0) >= 1);
    let mut src = MemorySource::new(zeroed);
    assert!(src.size() > 0);
    let index = SeekIndex::read(&mut src).expect("it still indexes");
    let seen = index
        .for_each_message_in_chunk(&mut src, 0, |_, _, _| Ok(()))
        .expect("chunk 0 still reads");
    assert_eq!(seen, ChunkIntegrity::NotChecked);
    assert!(!seen.is_verified());
}
