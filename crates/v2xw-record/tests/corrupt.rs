//! A truncated or corrupt recording is rejected with a clear error rather than a panic.
//!
//! This is not a nicety. A recording is the artefact someone reaches for *after* a run
//! went wrong, so it is routinely read half-written: the writer was killed, the disk
//! filled, the file was copied while it was still growing. A reader that panics on any of
//! those is useless exactly when it is needed, and a reader that indexes a slice directly
//! panics on all of them.
//!
//! Every case below asserts two things: that the call returns an error, and that the
//! error says something a human can act on.

use v2xw_record::fixture::{RunShape, scratch_dir, write_recording};
use v2xw_record::wire::snapshot::{DeltaBody, KeyframeBody, SignalRow};
use v2xw_record::wire::{Frame, MsgType};
use v2xw_record::{Reader, RecordError, RecordingOptions, RecordingWriter};

fn good_recording(tag: &str) -> (std::path::PathBuf, Vec<u8>) {
    let dir = scratch_dir(tag).expect("a scratch directory");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(4, 30)).expect("the recording is written");
    let bytes = std::fs::read(&path).expect("the recording is readable");
    (path, bytes)
}

/// Opens a byte string as a recording and returns the error, insisting it is one.
fn expect_rejected(label: &str, bytes: Vec<u8>) -> RecordError {
    match Reader::open_bytes(bytes) {
        Ok(mut reader) => {
            // Some corruptions survive the index and only bite when the data is walked.
            match reader.verify() {
                Ok(report) => panic!("{label}: accepted a corrupt recording ({report:?})"),
                Err(e) => e,
            }
        }
        Err(e) => e,
    }
}

#[test]
fn an_empty_or_tiny_file_is_rejected() {
    for (label, bytes) in [
        ("empty", Vec::new()),
        ("one byte", vec![0x89]),
        ("magic only", b"\x89MCAP0\r\n".to_vec()),
        ("magic twice", b"\x89MCAP0\r\n\x89MCAP0\r\n".to_vec()),
    ] {
        let err = expect_rejected(label, bytes);
        let text = err.to_string();
        assert!(
            text.contains("mcap") || text.contains("truncated") || text.contains("summary"),
            "{label}: the error does not name the problem: {text}"
        );
    }
}

#[test]
fn a_file_that_is_not_mcap_at_all_is_rejected() {
    let err = expect_rejected(
        "not mcap",
        b"this is a text file, not a recording\n".repeat(20),
    );
    assert!(
        err.to_string().contains("mcap"),
        "the error should name the container: {err}"
    );
}

#[test]
fn a_truncated_recording_is_rejected_at_every_length() {
    let (_, bytes) = good_recording("corrupt-truncate");
    assert!(
        bytes.len() > 4_000,
        "the fixture must be big enough to truncate"
    );
    // Every tenth of the file, plus one byte short of the end: the footer, the summary,
    // a chunk boundary and the middle of a chunk are all covered.
    for numerator in 1..10 {
        let cut = bytes.len() * numerator / 10;
        let err = expect_rejected(&format!("truncated at {cut}"), bytes[..cut].to_vec());
        assert!(!err.to_string().is_empty());
    }
    let err = expect_rejected("one byte short", bytes[..bytes.len() - 1].to_vec());
    assert!(
        err.to_string().contains("magic") || err.to_string().contains("truncated"),
        "a file missing its closing magic should say so: {err}"
    );
}

#[test]
fn a_recording_whose_footer_points_nowhere_is_rejected() {
    let (_, bytes) = good_recording("corrupt-footer");
    // The footer body is the 20 bytes before the closing magic: summary_start,
    // summary_offset_start, summary_crc.
    let at = bytes.len() - 8 - 20;

    let mut zeroed = bytes.clone();
    zeroed[at..at + 8].fill(0);
    let err = expect_rejected("summary_start = 0", zeroed);
    assert!(
        matches!(err, RecordError::NoSummary),
        "a recording that was never finished has no summary: {err}"
    );

    let mut absurd = bytes.clone();
    absurd[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    let err = expect_rejected("summary_start beyond the file", absurd);
    assert!(!err.to_string().is_empty());

    let mut inside_data = bytes;
    inside_data[at..at + 8].copy_from_slice(&64u64.to_le_bytes());
    let err = expect_rejected("summary_start inside the data section", inside_data);
    assert!(!err.to_string().is_empty());
}

#[test]
fn a_corrupt_chunk_is_rejected_rather_than_decoded() {
    let (path, bytes) = good_recording("corrupt-chunk");
    // Target bytes that are genuinely inside a chunk's compressed payload, taken from the
    // recording's own chunk index rather than guessed at. MCAP protects a chunk with a
    // CRC over its uncompressed records, and `ChunkReader` validates it, so a single
    // flipped bit there is detected rather than decoded into plausible nonsense.
    let reader = Reader::open(&path).expect("the recording opens");
    let chunks: Vec<(u64, u64)> = reader
        .index()
        .chunks
        .iter()
        .map(|c| (c.file_offset, c.record_length))
        .collect();
    assert!(!chunks.is_empty(), "the fixture must contain a chunk");
    drop(reader);

    let mut checked = 0;
    for (offset, length) in chunks {
        for fraction in [4u64, 2, 4 * 3] {
            let at = (offset + 32 + length * fraction / 16) as usize;
            if at >= bytes.len() {
                continue;
            }
            let mut broken = bytes.clone();
            broken[at] ^= 0xFF;
            let err = expect_rejected(&format!("byte {at} of a chunk flipped"), broken);
            let text = err.to_string();
            assert!(
                text.contains("mcap") || text.contains("malformed") || text.contains("truncated"),
                "the error should name the container or the frame: {text}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 2, "the test must have flipped some chunk bytes");
}

#[test]
fn a_recording_with_a_gap_in_its_sequence_numbers_is_reported() {
    let dir = scratch_dir("corrupt-seq").expect("a scratch directory");
    let path = dir.join("gap.mcap");
    let shape = RunShape::new(3, 12);
    let frames = v2xw_record::fixture::live_frames(&shape).expect("live frames");
    let mut writer = RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
    for (i, frame) in frames.iter().enumerate() {
        // Drop one canonical frame from the middle: the recording is well formed as a
        // container and inconsistent as a stream, which is exactly what `verify` is for.
        if i == 5 {
            continue;
        }
        writer.write_frame(frame).expect("the frame stores");
    }
    writer.finish().expect("the recording finishes");

    let mut reader = Reader::open(&path).expect("the container itself is fine");
    let err = reader.verify().expect_err("the stream is not dense");
    match err {
        RecordError::Inconsistent { detail, .. } => {
            assert!(
                detail.contains("dense"),
                "the error should name the defect: {detail}"
            );
        }
        other => panic!("expected Inconsistent, got {other}"),
    }
}

#[test]
fn a_delta_that_is_rooted_in_no_keyframe_is_reported() {
    let dir = scratch_dir("corrupt-orphan").expect("a scratch directory");
    let path = dir.join("orphan.mcap");
    let mut writer = RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
    let delta = DeltaBody {
        sim_time_ns: 100_000_000,
        gop_index: 7,
        step_index: 1,
        moved: Vec::new(),
        abs: Vec::new(),
        lanes: Vec::new(),
        spawns: Vec::new(),
        despawns: Vec::new(),
        signals: vec![SignalRow {
            signal_id: 1,
            time_to_change_ds: 10,
            phase: 3,
        }],
    };
    writer
        .write_frame(&delta.to_frame(0, 0).expect("a frame"))
        .expect("the frame stores");
    writer.finish().expect("the recording finishes");

    let mut reader = Reader::open(&path).expect("the container is fine");
    let err = reader
        .verify()
        .expect_err("a delta must be rooted in a keyframe");
    match err {
        RecordError::Inconsistent { detail, .. } => {
            assert!(detail.contains("rooted in nothing"), "got {detail}");
        }
        other => panic!("expected Inconsistent, got {other}"),
    }
}

#[test]
fn a_snapshot_gap_larger_than_the_cadence_is_reported() {
    let dir = scratch_dir("corrupt-gap").expect("a scratch directory");
    let path = dir.join("gap.mcap");
    let shape = RunShape::new(3, 12);
    let frames = v2xw_record::fixture::live_frames(&shape).expect("live frames");
    let mut writer = RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
    let mut seq = 0u64;
    let mut dropped = 0;
    for frame in &frames {
        let h = frame.header().expect("a header");
        // Skip two consecutive deltas of the first GOP, renumbering so the seq stays
        // dense: the only remaining defect is the time gap, so that is what `verify` must
        // find.
        if matches!(h.kind(), Some(MsgType::Delta)) {
            let d = DeltaBody::decode(frame.body()).expect("a delta body");
            if d.gop_index == 0 && (3..=4).contains(&d.step_index) {
                dropped += 1;
                continue;
            }
        }
        let renumbered = frame.renumbered(seq).expect("a frame");
        writer.write_frame(&renumbered).expect("the frame stores");
        if h.kind().is_some_and(MsgType::is_canonical) {
            seq += 1;
        }
    }
    writer.finish().expect("the recording finishes");
    assert_eq!(dropped, 2, "the test must actually remove two deltas");

    let mut reader = Reader::open(&path).expect("the container is fine");
    let err = reader
        .verify()
        .expect_err("the snapshot stream has a hole in it");
    match err {
        RecordError::Inconsistent { detail, .. } => {
            assert!(
                detail.contains("mobility step") || detail.contains("step_index"),
                "got {detail}"
            );
        }
        other => panic!("expected Inconsistent, got {other}"),
    }
}

#[test]
fn a_frame_body_that_contradicts_its_own_prefix_is_rejected() {
    // A `Delta` whose `abs_count` disagrees with its rows' `MFLAG_ABSOLUTE` bits, and a
    // `Keyframe` whose actor block is shorter than its `actor_count`: both are how a
    // partially written or bit-rotted body looks, and both must be errors rather than
    // out-of-bounds reads.
    let shape = RunShape::new(3, 12);
    let frames = v2xw_record::fixture::live_frames(&shape).expect("live frames");
    let keyframe = frames
        .iter()
        .find(|f| f.header().expect("a header").kind() == Some(MsgType::Keyframe))
        .expect("a keyframe");

    let mut body = keyframe.body().to_vec();
    // actor_count is at body offset 32.
    body[32] = 0xFF;
    let err = KeyframeBody::decode(&body).expect_err("the actor block cannot hold 255 rows");
    assert!(matches!(err, RecordError::Truncated { .. }), "got {err}");

    let mut body = keyframe.body().to_vec();
    // off_actors is at body offset 40: point it past the end.
    body[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let err = KeyframeBody::decode(&body).expect_err("the actor block is outside the body");
    assert!(matches!(err, RecordError::Truncated { .. }), "got {err}");

    let delta = frames
        .iter()
        .find(|f| f.header().expect("a header").kind() == Some(MsgType::Delta))
        .expect("a delta");
    let mut body = delta.body().to_vec();
    // abs_count is at body offset 20.
    body[20] = 1;
    let err = DeltaBody::decode(&body).expect_err("no row sets MFLAG_ABSOLUTE");
    assert!(matches!(err, RecordError::Malformed { .. }), "got {err}");

    // Truncating a frame's body at every length either decodes or errors, never panics.
    for cut in 0..keyframe.body().len() {
        let _ = KeyframeBody::decode(&keyframe.body()[..cut]);
    }
    for cut in 0..delta.body().len() {
        let _ = DeltaBody::decode(&delta.body()[..cut]);
    }
    // …and so does truncating the whole frame.
    for cut in 0..keyframe.as_bytes().len() {
        let _ = Frame::from_bytes(keyframe.as_bytes()[..cut].to_vec());
    }
}
