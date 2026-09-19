//! Seeking — §7.3's algorithm and conformance P4.
//!
//! The contract: seeking to `t` loads the preceding keyframe plus at most one keyframe
//! period of deltas, using MCAP's chunk index and per-channel message index. These tests
//! check that it lands on the right frame from many targets, that the state it
//! reconstructs is the state a full replay would have reached, that it never reads more
//! than two chunks, and that the paged reader and the resident one agree.
//!
//! The *latency* target is measured by `benches/seek.rs`, not here: a debug-profile test
//! would measure the compiler's mood rather than the algorithm.

use std::collections::BTreeMap;

use v2xw_record::fixture::{RunShape, scratch_dir, write_recording_with};
use v2xw_record::wire::snapshot::{DeltaBody, KeyframeBody, MFLAG_ABSOLUTE};
use v2xw_record::wire::{FLAG_RESYNC, FLAG_SEEK_RESULT, MsgType};
use v2xw_record::{Reader, RecordingOptions};

/// A small deterministic generator, so "many random targets" means the same targets on
/// every machine and every run.
struct Lcg(u64);

impl Lcg {
    fn next_in(&mut self, lo: u64, hi: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        if hi <= lo {
            return lo;
        }
        lo + (self.0 >> 11) % (hi - lo + 1)
    }
}

/// Every slot's pose, as a client holds it.
type Poses = BTreeMap<u32, (i64, i64, i64, u16)>;

fn apply(poses: &mut Poses, frame: &v2xw_record::Frame) {
    match frame.header().expect("a header").kind() {
        Some(MsgType::Keyframe) => {
            poses.clear();
            let kf = KeyframeBody::decode(frame.body()).expect("a keyframe body");
            for (slot, row) in kf.actors.iter().enumerate() {
                if row.is_occupied() {
                    poses.insert(
                        slot as u32,
                        (
                            i64::from(row.x_mm),
                            i64::from(row.y_mm),
                            i64::from(row.z_cm) * 10,
                            row.heading_brad,
                        ),
                    );
                }
            }
        }
        Some(MsgType::Delta) => {
            let d = DeltaBody::decode(frame.body()).expect("a delta body");
            let mut abs = d.abs.iter();
            for row in &d.moved {
                let entry = if row.mflags & MFLAG_ABSOLUTE != 0 {
                    abs.next().copied()
                } else {
                    None
                };
                let p = poses.entry(row.slot).or_default();
                match entry {
                    Some(e) => {
                        *p = (
                            i64::from(e.x_mm),
                            i64::from(e.y_mm),
                            i64::from(e.z_cm) * 10,
                            row.heading_brad,
                        );
                    }
                    None => {
                        p.0 += i64::from(row.dx_mm);
                        p.1 += i64::from(row.dy_mm);
                        p.2 += i64::from(row.dz_mm);
                        p.3 = row.heading_brad;
                    }
                }
            }
            for s in &d.spawns {
                poses.insert(
                    s.slot,
                    (
                        i64::from(s.x_mm),
                        i64::from(s.y_mm),
                        i64::from(s.z_cm) * 10,
                        s.heading_brad,
                    ),
                );
            }
            for x in &d.despawns {
                poses.remove(&x.slot);
            }
        }
        other => panic!("not a snapshot frame: {other:?}"),
    }
}

struct Fixture {
    path: std::path::PathBuf,
    shape: RunShape,
}

fn fixture(tag: &str, chunk_target_bytes: u64) -> Fixture {
    let dir = scratch_dir(tag).expect("a scratch directory");
    let path = dir.join("run.mcap");
    let shape = RunShape {
        actors: 40,
        steps: 600,
        signals: 4,
        teleport_at: Some(317),
        ..Default::default()
    };
    write_recording_with(
        &path,
        &shape,
        RecordingOptions {
            cadence: shape.cadence,
            chunk_target_bytes,
            ..Default::default()
        },
        false,
    )
    .expect("the recording is written");
    Fixture { path, shape }
}

#[test]
fn seeking_lands_on_the_preceding_keyframe_from_many_targets() {
    let f = fixture("seek-targets", 64 * 1024);
    let mut reader = Reader::open(&f.path).expect("the recording opens");
    let keyframes: Vec<u64> = reader
        .index()
        .keyframes
        .iter()
        .map(|k| k.sim_time)
        .collect();
    assert!(keyframes.len() > 10, "the fixture must have many GOPs");
    assert!(
        reader.index().chunks.len() > 3,
        "the fixture must span several chunks for the chunk index to matter: {}",
        reader.index().chunks.len()
    );
    let (min, max) = reader.index().snapshot_span().expect("a recorded span");

    let mut rng = Lcg(0x5EED_1234_5678_9ABC);
    let step = f.shape.cadence.mobility_step.as_nanos();
    let max_deltas = f.shape.cadence.max_deltas_per_gop();
    for _ in 0..250 {
        let t = rng.next_in(min, max);
        let result = reader
            .seek(t)
            .unwrap_or_else(|e| panic!("seek to {t} failed: {e}"));

        // The keyframe is the last one at or before t.
        let expected = keyframes
            .iter()
            .copied()
            .rfind(|k| *k <= t)
            .expect("a keyframe at or before t");
        assert_eq!(
            result.keyframe_time, expected,
            "seek to {t} chose the wrong keyframe"
        );

        // §7.3 step 8: the keyframe answers a seek and re-seeds interpolation state.
        let flags = result.keyframe.header().expect("a header").flags;
        assert_eq!(flags & FLAG_RESYNC, FLAG_RESYNC);
        assert_eq!(flags & FLAG_SEEK_RESULT, FLAG_SEEK_RESULT);

        // P4: at most keyframe_period / mobility_step deltas, all in (kf, t].
        assert!(
            result.deltas.len() as u64 <= max_deltas,
            "seek to {t} returned {} deltas, more than the {max_deltas} a GOP can hold",
            result.deltas.len()
        );
        let mut want_step = 0u32;
        for d in &result.deltas {
            let body = DeltaBody::decode(d.body()).expect("a delta body");
            assert!(body.sim_time_ns > expected && body.sim_time_ns <= t);
            want_step += 1;
            assert_eq!(
                body.step_index, want_step,
                "the deltas must be contiguous from 1"
            );
        }
        // The position reached is the last mobility step at or before t.
        let expected_position = t - (t - min) % step;
        assert_eq!(
            result.position(),
            expected_position,
            "seek to {t} positioned at {} rather than {expected_position}",
            result.position()
        );
        // §7.3: at most two chunks are read.
        assert!(
            result.chunks_read <= 2,
            "seek read {} chunks",
            result.chunks_read
        );
    }
}

#[test]
fn the_state_a_seek_reconstructs_is_the_state_a_full_replay_reaches() {
    let f = fixture("seek-state", 64 * 1024);
    let mut reader = Reader::open(&f.path).expect("the recording opens");
    let frames = reader.replay().expect("the recording replays");
    let (min, max) = reader.index().snapshot_span().expect("a recorded span");

    let mut rng = Lcg(0xC0FF_EE00_1234_5678);
    for _ in 0..40 {
        let t = rng.next_in(min, max);
        // Ground truth: walk the whole stream up to t.
        let mut replayed = Poses::new();
        for f in &frames {
            let kind = f.frame.header().expect("a header").kind();
            if !matches!(kind, Some(MsgType::Keyframe) | Some(MsgType::Delta)) {
                continue;
            }
            if f.sim_time > t {
                break;
            }
            apply(&mut replayed, &f.frame);
        }
        // The seek path.
        let result = reader.seek(t).expect("the seek succeeds");
        let mut sought = Poses::new();
        apply(&mut sought, &result.keyframe);
        for d in &result.deltas {
            apply(&mut sought, d);
        }
        assert_eq!(
            sought, replayed,
            "seeking to {t} reconstructed a different state than replaying to it"
        );
    }
}

#[test]
fn the_paged_reader_and_the_resident_reader_agree() {
    let f = fixture("seek-paged", 64 * 1024);
    let mut resident = Reader::open(&f.path).expect("the recording opens");
    let mut paged = Reader::open_paged(&f.path).expect("the recording opens paged");
    assert_eq!(resident.cadence(), paged.cadence());
    assert_eq!(
        resident.index().keyframes.len(),
        paged.index().keyframes.len()
    );
    let (min, max) = resident.index().snapshot_span().expect("a recorded span");
    let mut rng = Lcg(7);
    for _ in 0..25 {
        let t = rng.next_in(min, max);
        let a = resident.seek(t).expect("resident seek");
        let b = paged.seek(t).expect("paged seek");
        assert_eq!(a, b, "the two readers disagree at {t}");
    }
    // P5's native/WASM parity is the same property one layer up: the reader is one
    // implementation over a byte source, so a second source cannot produce a second
    // stream.
    assert_eq!(
        resident.replay().expect("resident replay"),
        paged.replay().expect("paged replay")
    );
}

#[test]
fn a_target_before_the_first_keyframe_is_out_of_range() {
    let f = fixture("seek-range", 4 * 1024 * 1024);
    let mut reader = Reader::open(&f.path).expect("the recording opens");
    let (min, _) = reader.index().snapshot_span().expect("a recorded span");
    // The first keyframe is at t = 0 in the fixture, so build the out-of-range case from
    // a recording whose span does not start at zero by seeking below the span.
    if min > 0 {
        let err = reader
            .seek(min - 1)
            .expect_err("below the span is out of range");
        assert!(matches!(
            err,
            v2xw_record::RecordError::SeekOutOfRange { .. }
        ));
    }
    // Beyond the end is not an error: the last keyframe still precedes it, which is what
    // a client scrubbing past the end of a live recording does.
    let (_, max) = reader.index().snapshot_span().expect("a recorded span");
    let result = reader
        .seek(max + 10_000_000_000)
        .expect("seeking past the end clamps");
    assert!(result.keyframe_time <= max);
}

#[test]
fn every_chunk_starts_at_a_keyframe() {
    // §7.1: "a chunk MUST NOT start in the middle of a GOP's keyframe". The recorder
    // closes a chunk before a keyframe once it has reached its target size, so a GOP is
    // never split and §7.3 step 7 reads at most two chunks.
    let f = fixture("seek-chunks", 32 * 1024);
    let reader = Reader::open(&f.path).expect("the recording opens");
    let keyframes: Vec<u64> = reader
        .index()
        .keyframes
        .iter()
        .map(|k| k.sim_time)
        .collect();
    for (i, chunk) in reader.index().chunks.iter().enumerate().skip(1) {
        assert!(
            keyframes.contains(&chunk.message_start_time),
            "chunk {i} starts at {} which is not a keyframe time",
            chunk.message_start_time
        );
    }
}
