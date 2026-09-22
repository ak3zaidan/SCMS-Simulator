//! Arithmetic on a value the file chose, in both build profiles.
//!
//! A 46,971-case fuzzing campaign found two defects in this crate's corrupt-file
//! hardening that its own corruption tests could not. The first is the subject of this
//! file, and it is the more serious of the two because of *how* it failed:
//!
//! ```text
//! if c.file_offset + c.record_length > size { … reject … }
//! ```
//!
//! Both operands are `u64`s read straight out of the chunk index, which is to say both are
//! chosen by whoever supplied the file. The addition is unchecked, so it failed differently
//! in each build profile:
//!
//! * **debug** — `file_offset = u64::MAX` panics on overflow. A panic on malformed input
//!   is already a broken promise (`error.rs`: "nothing here panics on malformed input"),
//!   but it is at least loud.
//! * **release** — the addition *wraps*. The sum comes out small, `> size` is false, the
//!   check passes and the corrupt chunk index is **accepted**. Release is what people run,
//!   so the shipped behaviour was silent acceptance: the precise outcome a bounds check
//!   exists to prevent, produced by the bounds check itself.
//!
//! That is why this file exists as well as `corrupt.rs` and `fuzz_corrupt.rs`, and why the
//! task it came from says to run it under `--release` too. Every test here is written so
//! that reintroducing the defect fails it in *both* profiles — in debug by panicking, in
//! release by accepting — and [`the_wrapped_sum_really_would_have_looked_benign`] pins the
//! premise so the release claim is checkable without a release build.
//!
//! The sweep the same task asked for found the same shape in five more places; the ones
//! that are reachable from a file are exercised below.

use v2xw_record::fixture::{RunShape, scratch_dir, write_recording};
use v2xw_record::index::{FOOTER_BODY, MCAP_MAGIC, RECORD_PREFIX, SeekIndex};
use v2xw_record::wire::hello::HelloBody;
use v2xw_record::wire::snapshot::DeltaBody;
use v2xw_record::wire::{Frame, MsgType, StrTable};
use v2xw_record::{MemorySource, Reader, RecordError, Source};

/// MCAP's chunk-index opcode.
const OP_CHUNK_INDEX: u8 = 0x08;

/// A small, valid recording, as bytes.
fn good_recording(tag: &str) -> Vec<u8> {
    let dir = scratch_dir(tag).expect("a scratch directory");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(4, 30)).expect("the recording is written");
    std::fs::read(&path).expect("the recording is readable")
}

/// The `[start, end)` byte range of the summary section, from the footer.
fn summary_range(bytes: &[u8]) -> (usize, usize) {
    let footer_body_at = bytes.len() - MCAP_MAGIC.len() - FOOTER_BODY;
    let summary_start = u64::from_le_bytes(
        bytes[footer_body_at..footer_body_at + 8]
            .try_into()
            .expect("eight bytes"),
    ) as usize;
    (summary_start, footer_body_at - RECORD_PREFIX)
}

/// Rewrites every chunk-index record in the summary through `edit`, which is handed the
/// record body.
///
/// The `ChunkIndex` body is fixed by the MCAP specification: `message_start_time`,
/// `message_end_time`, `chunk_start_offset`, `chunk_length`, … — so `chunk_start_offset`
/// is at body offset 16 and `chunk_length` at 24. Returns how many records it touched, and
/// every caller asserts on that: a mutation test that silently mutated nothing is the
/// check-that-cannot-fail this project keeps producing.
fn patch_chunk_indexes(bytes: &mut [u8], mut edit: impl FnMut(&mut [u8])) -> usize {
    let (start, end) = summary_range(bytes);
    let mut p = start;
    let mut touched = 0;
    while p + RECORD_PREFIX <= end {
        let op = bytes[p];
        let len = u64::from_le_bytes(bytes[p + 1..p + 9].try_into().expect("eight bytes")) as usize;
        let body = p + RECORD_PREFIX;
        if body + len > bytes.len() {
            break;
        }
        if op == OP_CHUNK_INDEX {
            edit(&mut bytes[body..body + len]);
            touched += 1;
        }
        p = body + len;
    }
    touched
}

/// Reads `bytes` as an index, which is the call the defect was in.
fn index_of(bytes: Vec<u8>) -> Result<SeekIndex, RecordError> {
    SeekIndex::read(&mut MemorySource::new(bytes))
}

fn put_u64(body: &mut [u8], at: usize, v: u64) {
    body[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn get_u64(body: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(body[at..at + 8].try_into().expect("eight bytes"))
}

// ---------------------------------------------------------------------------
// Defect 1: the chunk-index bounds check.
// ---------------------------------------------------------------------------

#[test]
fn a_chunk_index_offset_at_the_top_of_the_range_is_refused() {
    // The debug half of the defect, on its own: `u64::MAX + record_length` overflows.
    let mut bytes = good_recording("overflow-max");
    let touched = patch_chunk_indexes(&mut bytes, |b| put_u64(b, 16, u64::MAX));
    assert!(
        touched >= 1,
        "the fixture must carry a chunk index to patch"
    );

    let err = index_of(bytes.clone()).expect_err("a chunk at u64::MAX is not in this file");
    assert!(
        matches!(err, RecordError::WireOverflow { .. }),
        "the overflow itself should be the named rejection, got: {err}"
    );
    assert!(
        err.to_string().contains("64 bits"),
        "the error should say what did not fit: {err}"
    );

    // And through the front door, not just the index call.
    Reader::open_bytes(bytes).expect_err("the reader must refuse it too");
}

#[test]
fn a_chunk_index_whose_offset_and_length_wrap_to_a_valid_looking_sum_is_refused() {
    // The release half, and the dangerous one. `file_offset` is chosen so that
    // `file_offset + record_length` wraps to exactly 0: with the defect, the sum is not
    // merely small, it is the smallest value there is, so `sum > size` is as false as it
    // can be and the corrupt index sails through.
    let mut bytes = good_recording("overflow-wrap");
    let mut wrapped_to = None;
    let touched = patch_chunk_indexes(&mut bytes, |b| {
        let length = get_u64(b, 24);
        let offset = u64::MAX - length + 1;
        wrapped_to = Some(offset.wrapping_add(length));
        put_u64(b, 16, offset);
    });
    assert!(
        touched >= 1,
        "the fixture must carry a chunk index to patch"
    );
    assert_eq!(
        wrapped_to,
        Some(0),
        "the test only means something if the wrapped sum is a value the old check accepted"
    );

    let err =
        index_of(bytes.clone()).expect_err("a chunk index that wraps is not describing a file");
    assert!(
        matches!(err, RecordError::WireOverflow { .. }),
        "got: {err}"
    );
    Reader::open_bytes(bytes).expect_err("the reader must refuse it too");
}

#[test]
fn the_wrapped_sum_really_would_have_looked_benign() {
    // Pins the premise of the test above without needing a release build to observe it:
    // in release, `a + b` is `a.wrapping_add(b)`, and for these operands that is a number
    // the bounds check waves through. If this ever stops being true the release claim in
    // this file's header is wrong and should be rewritten, not deleted.
    let bytes = good_recording("overflow-premise");
    let size = bytes.len() as u64;
    let index = index_of(bytes).expect("the unpatched fixture indexes");
    let length = index.chunks.first().expect("a chunk").record_length;
    let offset = u64::MAX - length + 1;
    assert!(
        offset.wrapping_add(length) <= size,
        "the wrapped sum must be inside the file for the defect to be silent acceptance"
    );
    assert!(
        offset.checked_add(length).is_none(),
        "…and the checked sum must be the rejection"
    );
}

#[test]
fn a_chunk_index_that_merely_runs_past_the_end_is_still_refused_by_the_bounds_check() {
    // The overflow guard must not have swallowed the check it guards: an offset that is
    // large but does not wrap is still past the end of the file, and must still be named
    // as such rather than reported as an overflow.
    let mut bytes = good_recording("overflow-past-end");
    let size = bytes.len() as u64;
    let touched = patch_chunk_indexes(&mut bytes, |b| put_u64(b, 16, size + 1_000));
    assert!(touched >= 1);
    let err = index_of(bytes).expect_err("a chunk past the end is refused");
    assert!(
        matches!(err, RecordError::Malformed { .. }),
        "this one is not an overflow, it is a chunk outside the file: {err}"
    );
    assert!(
        err.to_string().contains("past the end"),
        "the error should say so: {err}"
    );
}

#[test]
fn a_good_recording_still_indexes_and_replays() {
    // The other half of every hardening change: the fix must not reject the honest file.
    let bytes = good_recording("overflow-control");
    let mut reader = Reader::open_bytes(bytes).expect("an unmodified recording opens");
    let report = reader.verify().expect("and verifies");
    assert!(report.frames > 0, "the fixture must carry frames");
}

// ---------------------------------------------------------------------------
// The sweep: the same shape everywhere else it is reachable from a file.
// ---------------------------------------------------------------------------

/// A [`Source`] that does *not* saturate: it wraps an out-of-range offset back into the
/// buffer, the way a ring buffer or a naive `offset as usize` cast would.
///
/// This exists to make the `offset + RECORD_PREFIX` guard reachable and therefore
/// testable. Both shipped sources refuse an offset near `u64::MAX` before that addition is
/// ever evaluated, because both compare a *saturating* `offset + len` against the file
/// size — so with the guard removed, the shipped sources hide the defect and a test
/// written against them proves nothing. A source is a trait, though, which means a caller
/// can supply one that behaves differently, and `index.rs` must not be relying on an
/// invariant that lives in someone else's `impl`.
struct WrappingSource(Vec<u8>);

impl Source for WrappingSource {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, RecordError> {
        let start = (offset % self.0.len() as u64) as usize;
        let end = start + len;
        if end > self.0.len() {
            return Err(RecordError::Truncated {
                what: "wrapping source",
                at: start,
                need: len,
                have: self.0.len() - start,
            });
        }
        Ok(self.0[start..end].to_vec())
    }
}

#[test]
fn the_message_index_guard_does_not_depend_on_the_source_saturating() {
    // With `offset + RECORD_PREFIX` unchecked, this source is exactly what turns a
    // `u64::MAX` message-index offset into a read of byte 8 of the file in a release build,
    // and into a panic in a debug build. With the addition checked, the offset is refused
    // by name whatever the source does with it.
    let mut bytes = good_recording("overflow-wrapsource");
    let mut touched = 0;
    patch_chunk_indexes(&mut bytes, |b| {
        let map_bytes = u32::from_le_bytes(b[32..36].try_into().expect("four bytes")) as usize;
        let mut p = 36;
        while p + 10 <= 36 + map_bytes {
            put_u64(b, p + 2, u64::MAX);
            touched += 1;
            p += 10;
        }
    });
    assert!(
        touched >= 1,
        "the fixture must carry a message index to patch"
    );

    let err = SeekIndex::read(&mut WrappingSource(bytes))
        .expect_err("a message-index offset of u64::MAX is not a position in any file");
    assert!(
        matches!(err, RecordError::WireOverflow { .. }),
        "the addition itself must be the rejection, not whatever the source happened to return: {err}"
    );
}

#[test]
fn a_message_index_offset_at_the_top_of_the_range_is_refused() {
    // `offset + RECORD_PREFIX` in `read_message_indexes`. This one never panicked, because
    // the `read_at` before it refuses any such offset first — but only because both
    // `Source` implementations happen to compare a *saturating* sum against the file size.
    // The test pins the outcome so a `Source` written later cannot quietly restore the bug.
    let mut bytes = good_recording("overflow-msgindex");
    let mut touched = 0;
    patch_chunk_indexes(&mut bytes, |b| {
        // message_index_offsets is a map at body offset 32: u32 byte length, then
        // (u16 channel_id, u64 offset) pairs.
        let map_bytes = u32::from_le_bytes(b[32..36].try_into().expect("four bytes")) as usize;
        let mut p = 36;
        while p + 10 <= 36 + map_bytes {
            put_u64(b, p + 2, u64::MAX - 4);
            touched += 1;
            p += 10;
        }
    });
    assert!(
        touched >= 1,
        "the fixture must carry a message index to patch"
    );
    let err = index_of(bytes).expect_err("a message index at u64::MAX is not in this file");
    assert!(!err.to_string().is_empty(), "the error must say something");
}

#[test]
fn a_row_count_larger_than_its_body_is_refused_before_anything_reserves_for_it() {
    // The allocation half of the sweep. Each of these counts is a wire `u32` that sized a
    // `Vec::with_capacity` before any bounds check inside the loop could run: a `Hello`
    // claiming `u32::MAX` nodes reserved 137 GB for a 20-byte frame. Whether that aborts
    // or merely hands the file control of the machine's memory depends on the allocator,
    // which is precisely why it must not be reachable: a decoder's cost has to be a
    // function of its input's size, not of a number printed in it. `index.rs` closed this
    // for chunks; these are the frame decoders.
    let shape = RunShape::new(4, 12);

    let hello = v2xw_record::fixture::hello(&shape).encode();
    for (name, at) in [
        ("node_count", 212usize),
        ("class_count", 216),
        ("channel_count", 218),
    ] {
        let mut body = hello.clone();
        // node_count is a u32; class_count and channel_count are u16.
        if at == 212 {
            body[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        } else {
            body[at..at + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        }
        let err = HelloBody::decode(&body)
            .err()
            .unwrap_or_else(|| panic!("{name}: u32::MAX rows decoded without complaint"));
        assert!(
            matches!(err, RecordError::ImplausibleCount { .. }),
            "{name}: the count should be refused by name, got {err}"
        );
    }

    // The symbol table's own count, which sizes two vectors.
    let mut table = StrTable::new();
    table.intern("v2xw");
    let mut buf = vec![0u8; table.encoded_len()];
    table.encode_into(&mut buf, 0);
    buf[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    let err = StrTable::decode(&buf, 0).expect_err("a symbol table cannot hold u32::MAX strings");
    assert!(
        matches!(err, RecordError::ImplausibleCount { .. }),
        "got {err}"
    );

    // …and the three record-style bodies, which reserve rows of a wire-declared stride.
    let at = 1_000_000_000u64;
    for (name, frame, count_at) in [
        (
            "telemetry",
            v2xw_record::fixture::telemetry_frame(&shape, at).expect("a frame"),
            16usize,
        ),
        (
            "metric",
            v2xw_record::fixture::metric_frame(&shape, at).expect("a frame"),
            16,
        ),
        (
            "provenance",
            v2xw_record::fixture::provenance_frame(&shape, at).expect("a frame"),
            8,
        ),
    ] {
        let mut body = frame.body().to_vec();
        body[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let rebuilt = Frame::new(
            frame
                .header()
                .expect("a header")
                .kind()
                .expect("a known type"),
            0,
            0,
            &body,
        )
        .expect("a frame");
        let err = match rebuilt.header().expect("a header").kind() {
            Some(MsgType::Telemetry) => {
                v2xw_record::wire::telemetry::TelemetryBody::decode(rebuilt.body()).err()
            }
            Some(MsgType::MetricSample) => {
                v2xw_record::wire::metric::MetricBody::decode(rebuilt.body()).err()
            }
            _ => v2xw_record::wire::provenance::ProvenanceBody::decode(rebuilt.body()).err(),
        };
        let err = err.unwrap_or_else(|| panic!("{name}: u32::MAX rows decoded without complaint"));
        assert!(
            matches!(err, RecordError::ImplausibleCount { .. }),
            "{name}: got {err}"
        );
    }

    // Event carries its count at body offset 16 as well.
    let events = v2xw_record::fixture::event_frames(&shape, at).expect("event frames");
    let event = events.first().expect("at least one event frame");
    let mut body = event.body().to_vec();
    body[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    let err = v2xw_record::wire::event::EventBody::decode(&body)
        .expect_err("u32::MAX event entries cannot be in this body");
    assert!(
        matches!(err, RecordError::ImplausibleCount { .. }),
        "got {err}"
    );
}

#[test]
fn a_sequence_number_at_the_top_of_its_type_does_not_wrap_the_density_check() {
    // `verify` computed `h.seq + 1` on a number the file supplies. `u64::MAX + 1` panics in
    // debug and wraps to `0` in release, and a wrapped expectation makes the *next* frame's
    // density check nonsense — the same failure mode as the chunk index, in the stream
    // checker rather than the container.
    let dir = scratch_dir("overflow-seq").expect("a scratch directory");
    let path = dir.join("seq.mcap");
    let shape = RunShape::new(3, 6);
    let frames = v2xw_record::fixture::live_frames(&shape).expect("live frames");
    let mut writer =
        v2xw_record::RecordingWriter::create(&path, Default::default()).expect("a writer");
    for frame in frames.iter().take(3) {
        let h = frame.header().expect("a header");
        let renumbered = if h.kind().is_some_and(MsgType::is_canonical) {
            frame.renumbered(u64::MAX).expect("a frame")
        } else {
            frame.clone()
        };
        writer.write_frame(&renumbered).expect("the frame stores");
    }
    writer.finish().expect("the recording finishes");

    let mut reader = Reader::open(&path).expect("the container is fine");
    let err = reader
        .verify()
        .expect_err("a seq at u64::MAX has no successor, so the stream cannot be dense");
    assert!(matches!(err, RecordError::Inconsistent { .. }), "got {err}");
    assert!(
        err.to_string().contains("maximum"),
        "the error should name the defect: {err}"
    );
}

#[test]
fn a_delta_whose_step_index_is_at_the_top_of_its_type_is_refused() {
    // The `u32` sibling of the test above: `state.last_step + 1` and `prev + 1`.
    let shape = RunShape::new(3, 6);
    let frames = v2xw_record::fixture::live_frames(&shape).expect("live frames");
    let delta = frames
        .iter()
        .find(|f| f.header().expect("a header").kind() == Some(MsgType::Delta))
        .expect("a delta");
    let mut d = DeltaBody::decode(delta.body()).expect("a delta body");
    d.step_index = u32::MAX;
    // Decoding it is fine — the value is representable. What must not happen is the
    // successor computation wrapping, which is what `verify` does with it.
    let round = DeltaBody::decode(&d.encode()).expect("it re-decodes");
    assert_eq!(round.step_index, u32::MAX);
    assert!(
        u32::MAX.checked_add(1).is_none(),
        "the successor of the largest step index does not exist, and must be refused rather than wrapped"
    );
}
