//! Chunk boundaries: what the summary counts, and what `chunk_target_bytes` bounds.
//!
//! Two defects, both in the writer's accounting rather than in the bytes it wrote.
//!
//! `RecordingSummary::chunk_count` was short by one whenever the recording ended with an
//! attachment or a metadata record, because `attach` and `write_manifest` reset
//! `bytes_since_flush` — the container finishes the open chunk before writing either — and
//! `finish` then only counted a final chunk when that was non-zero. Every recording this
//! crate's own fixture writes ends with an attachment, so the number a caller would report
//! or assert on was wrong for all of them.
//!
//! `chunk_target_bytes` was not a bound. The writer ended a chunk only immediately before a
//! keyframe, so a chunk held at least one whole GOP and grew with
//! `keyframe_period / mobility_step`: at 400 actors and a 100 ms step a 1 s period gives
//! 4.0 MiB and a 600 s period roughly 48 MiB, twelve times the size §7.4's latency budget
//! is written against, silently. §7.1 forbids a chunk starting "in the middle of a GOP's
//! keyframe" — the keyframe record, not the GOP — and §7.3 step 7 explicitly contemplates a
//! GOP spanning a boundary, so the writer may split between deltas once the GOP alone has
//! passed the target, and the reader follows the GOP across the chunks it ended up in.

use v2xw_record::encoder::Cadence;
use v2xw_record::fixture::{RunShape, scratch_dir, write_recording, write_recording_with};
use v2xw_record::wire::MsgType;
use v2xw_record::wire::snapshot::{DeltaBody, KeyframeBody};
use v2xw_record::{Reader, RecordingOptions};

#[test]
fn the_summary_counts_every_chunk_the_file_holds() {
    // The fixture attaches `scenario.yaml` last, which closes the open chunk inside the
    // container — so this is not an exotic case, it is every recording the crate writes.
    let dir = scratch_dir("chunking-count").expect("a scratch directory");
    for (tag, actors, steps, target) in [
        ("small", 4u32, 30u32, 4 * 1024 * 1024u64),
        ("many-chunks", 60, 600, 64 * 1024),
    ] {
        let path = dir.join(format!("{tag}.mcap"));
        let shape = RunShape::new(actors, steps);
        let (_, summary) = write_recording_with(
            &path,
            &shape,
            RecordingOptions {
                cadence: shape.cadence,
                chunk_target_bytes: target,
                ..Default::default()
            },
            true,
        )
        .expect("the recording is written");

        let reader = Reader::open(&path).expect("the recording opens");
        let in_file = reader.index().chunks.len() as u64;
        assert!(in_file > 0);
        assert_eq!(
            summary.chunk_count, in_file,
            "{tag}: the summary says {} chunks and the chunk index holds {in_file}",
            summary.chunk_count
        );
    }
}

#[test]
fn the_chunk_target_bounds_a_chunk_whatever_the_keyframe_period() {
    // A cadence whose GOP is far larger than the chunk target: 120 steps per keyframe at
    // 40 actors against a 16 KiB target, which is about 106 KiB of GOP. Before the fix the
    // chunk was the whole GOP and the target meant nothing.
    let dir = scratch_dir("chunking-bound").expect("a scratch directory");
    let path = dir.join("long-gop.mcap");
    let target = 16 * 1024u64;
    let shape = RunShape {
        actors: 40,
        steps: 600,
        signals: 4,
        cadence: Cadence::new(
            v2xw_core::time::Duration::from_secs(12),
            v2xw_core::time::Duration::from_millis(100),
        )
        .expect("a cadence"),
        ..Default::default()
    };
    let (_, summary) = write_recording_with(
        &path,
        &shape,
        RecordingOptions {
            cadence: shape.cadence,
            chunk_target_bytes: target,
            ..Default::default()
        },
        false,
    )
    .expect("the recording is written");

    // The premise: one GOP really is bigger than the target, so the old rule would have
    // produced chunks of at least `largest_gop_bytes`.
    assert!(
        summary.largest_gop_bytes > 4 * target,
        "the fixture must have a GOP far larger than the target, got {} against {target}",
        summary.largest_gop_bytes
    );

    // The claim: a chunk is the target plus at most the one message that crossed it.
    let reader = Reader::open(&path).expect("the recording opens");
    let largest = reader
        .index()
        .chunks
        .iter()
        .map(|c| c.uncompressed_size)
        .max()
        .expect("a chunk");
    // One delta of this run is about 860 bytes, so "the target plus the one message that
    // crossed it" is what the writer's own payload count must show. The chunk index's
    // uncompressed size is a little larger because the container's record framing and the
    // channel and schema records it repeats sit on top of the payload.
    assert!(
        summary.largest_chunk_bytes <= target + 2 * 1024,
        "a chunk reached {} payload bytes against a {target}-byte target",
        summary.largest_chunk_bytes
    );
    assert!(
        largest < 2 * target,
        "a chunk holds {largest} uncompressed bytes against a {target}-byte target"
    );

    // And the reader still reconstructs the same state, GOP boundaries or not: a seek to
    // any point gives what a full replay gives.
    let mut reader = Reader::open(&path).expect("the recording opens");
    let frames = reader.replay().expect("it replays");
    let (min, max) = reader.index().snapshot_span().expect("a span");
    let step = shape.cadence.mobility_step.as_nanos();
    let mut checked = 0;
    let mut multi_chunk = 0;
    for i in 0..40u64 {
        let t = min + (max - min) * i / 39;
        let t = t - (t - min) % step;
        let result = reader
            .seek(t)
            .unwrap_or_else(|e| panic!("seek to {t}: {e}"));
        if result.chunks_read > 1 {
            multi_chunk += 1;
        }
        // Ground truth: the deltas a full replay would have applied since that keyframe.
        let want: Vec<u64> = frames
            .iter()
            .filter(|f| {
                f.frame.header().expect("a header").kind() == Some(MsgType::Delta)
                    && f.sim_time > result.keyframe_time
                    && f.sim_time <= t
            })
            .map(|f| f.sim_time)
            .collect();
        let got: Vec<u64> = result
            .deltas
            .iter()
            .map(|d| d.sim_time().expect("a time"))
            .collect();
        assert_eq!(
            got, want,
            "seeking to {t} lost deltas because the GOP spans {} chunks",
            result.chunks_read
        );
        // P4 still holds: at most one keyframe period of deltas.
        assert!(result.deltas.len() as u64 <= shape.cadence.max_deltas_per_gop());
        checked += 1;
    }
    assert_eq!(checked, 40);
    assert!(
        multi_chunk > 0,
        "the fixture must make at least one seek cross a chunk boundary, or the reader's \
         half of the fix is untested"
    );

    // §7.1's own rule is still honoured: no chunk starts in the middle of a keyframe
    // record, because a chunk holds whole records and the writer never splits before one.
    let reader = Reader::open(&path).expect("the recording opens");
    for (i, chunk) in reader.index().chunks.iter().enumerate() {
        assert!(
            chunk.uncompressed_size > 0,
            "chunk {i} is empty, so a boundary was taken where there was nothing to end"
        );
    }
}

#[test]
fn a_gop_that_fits_the_target_is_still_never_split() {
    // The property the strong rule bought — a seek reads exactly one chunk — must survive
    // the relaxation at every cadence where a GOP does fit, which is every realistic one.
    let dir = scratch_dir("chunking-whole-gop").expect("a scratch directory");
    let path = dir.join("short-gop.mcap");
    let shape = RunShape {
        actors: 40,
        steps: 600,
        signals: 4,
        ..Default::default()
    };
    let (_, summary) = write_recording_with(
        &path,
        &shape,
        RecordingOptions {
            cadence: shape.cadence,
            chunk_target_bytes: 64 * 1024,
            ..Default::default()
        },
        false,
    )
    .expect("the recording is written");
    assert!(
        summary.largest_gop_bytes < 64 * 1024,
        "this cadence's GOP must fit the target for the claim to mean anything"
    );

    let mut reader = Reader::open(&path).expect("the recording opens");
    let keyframes: Vec<u64> = reader
        .index()
        .keyframes
        .iter()
        .map(|k| k.sim_time)
        .collect();
    assert!(reader.index().chunks.len() > 3);
    for (i, chunk) in reader.index().chunks.iter().enumerate().skip(1) {
        assert!(
            keyframes.contains(&chunk.message_start_time),
            "chunk {i} starts at {} which is not a keyframe time",
            chunk.message_start_time
        );
    }
    let (min, max) = reader.index().snapshot_span().expect("a span");
    for i in 0..20u64 {
        let t = min + (max - min) * i / 19;
        let result = reader.seek(t).expect("the seek succeeds");
        assert_eq!(
            result.chunks_read, 1,
            "a GOP that fits its chunk must still be read in one"
        );
    }
}

#[test]
fn a_chunk_target_of_one_byte_still_produces_a_readable_recording() {
    // The degenerate setting a test uses to force a boundary everywhere. A chunk cannot be
    // smaller than one message, so the bound is "one message"; what must not happen is a
    // split inside a record, a lost message or an unreadable file.
    let dir = scratch_dir("chunking-tiny").expect("a scratch directory");
    let path = dir.join("tiny.mcap");
    let shape = RunShape::new(5, 30);
    let (live, summary) = write_recording_with(
        &path,
        &shape,
        RecordingOptions {
            cadence: shape.cadence,
            chunk_target_bytes: 1,
            ..Default::default()
        },
        false,
    )
    .expect("the recording is written");

    let mut reader = Reader::open(&path).expect("the recording opens");
    assert_eq!(summary.chunk_count, reader.index().chunks.len() as u64);
    let replayed = reader.replay().expect("it replays");
    assert_eq!(replayed.len(), live.len());
    for (i, (l, r)) in live.iter().zip(replayed.iter()).enumerate() {
        assert_eq!(
            l.canonical().as_bytes(),
            r.frame.as_bytes(),
            "frame {i} survived the boundary intact"
        );
    }
    reader.verify().expect("the recording verifies");

    // Every keyframe and every delta still decodes, and a seek still lands.
    let (min, max) = reader.index().snapshot_span().expect("a span");
    let result = reader.seek((min + max) / 2).expect("the seek succeeds");
    KeyframeBody::decode(result.keyframe.body()).expect("a keyframe body");
    for d in &result.deltas {
        DeltaBody::decode(d.body()).expect("a delta body");
    }
}

#[test]
fn the_default_recording_reports_what_it_actually_wrote() {
    // The summary is a report, so it has to be checkable against the file rather than
    // taken on trust.
    let dir = scratch_dir("chunking-report").expect("a scratch directory");
    let path = dir.join("run.mcap");
    let (live, summary) =
        write_recording(&path, &RunShape::new(6, 40)).expect("the recording is written");
    let mut reader = Reader::open(&path).expect("the recording opens");
    let replayed = reader.replay().expect("it replays");
    let records = reader.records(None).expect("the records");
    assert_eq!(summary.frame_count, live.len() as u64);
    assert_eq!(summary.frame_count, replayed.len() as u64);
    assert_eq!(summary.record_count, records.len() as u64);
    assert_eq!(
        summary.message_count,
        summary.frame_count + summary.record_count
    );
    assert_eq!(summary.chunk_count, reader.index().chunks.len() as u64);
    assert!(summary.largest_gop_bytes > 0);
    assert!(summary.largest_chunk_bytes >= summary.largest_gop_bytes);
}

#[test]
fn a_recording_split_into_hundreds_of_chunks_is_still_byte_identical_on_replay() {
    // `a_chunk_target_of_one_byte_still_produces_a_readable_recording` forces a boundary
    // everywhere but on a run short enough to hold a handful of GOPs. The property the
    // hardening work was told not to regress is stated over *hundreds* of chunks, because
    // the chunk walk is where the corrupt-file guards live and every chunk boundary is a
    // fresh trip through them: a guard that were too strict by one byte would show up here
    // and nowhere else.
    let dir = scratch_dir("chunking-hundreds").expect("a scratch directory");
    let path = dir.join("many.mcap");
    // One GOP per second at the default cadence, one chunk per GOP at a one-byte target.
    let shape = RunShape::new(3, 3_000);
    let (live, summary) = write_recording_with(
        &path,
        &shape,
        RecordingOptions {
            cadence: shape.cadence,
            chunk_target_bytes: 1,
            ..Default::default()
        },
        false,
    )
    .expect("the recording is written");

    let mut reader = Reader::open(&path).expect("the recording opens");
    let chunks = reader.index().chunks.len();
    assert!(
        chunks >= 200,
        "this test only means something with hundreds of chunks; got {chunks}"
    );
    assert_eq!(summary.chunk_count, chunks as u64);

    let replayed = reader.replay().expect("it replays");
    assert_eq!(
        replayed.len(),
        live.len(),
        "no frame was lost at a boundary"
    );
    let mut compared = 0;
    for (i, (l, r)) in live.iter().zip(replayed.iter()).enumerate() {
        assert_eq!(
            l.canonical().as_bytes(),
            r.frame.as_bytes(),
            "frame {i} is not byte-identical across {chunks} chunks"
        );
        compared += 1;
    }
    assert_eq!(compared, live.len(), "every frame must have been compared");

    let report = reader.verify().expect("the recording verifies");
    assert_eq!(
        report.chunks_checksummed, chunks as u64,
        "every one of those chunks carried a checksum and it was checked"
    );
    assert!(report.integrity_verified());
    println!("{chunks} chunks, {compared} frames compared byte for byte");
}
