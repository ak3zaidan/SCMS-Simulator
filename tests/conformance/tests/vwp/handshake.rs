//! §10.2 — handshake, resume and backpressure. Items H4 and H5.
//!
//! The rest of §10.2 is owned elsewhere or cannot be owned in process, and
//! `tests/vwp/coverage.rs` says which is which: H2, H6, H7 and H9 are `v2xw-server`'s, H3
//! and H10 are the client's, and H1, H8 and H11 are deadlines measured against a real clock
//! on a live socket.

use v2xw_record::fixture::{RunShape, live_frames};
use v2xw_record::wire::{Frame, MsgType};
use v2xw_server::ring::{ResumeRing, RingEntry};

/// **H4** — "`seq` is dense and monotonic across canonical frames; `Hello`/`Error`/`Bye`
/// do not consume a `seq` and carry the next one."
///
/// Both halves, over a stream that actually contains a `Hello`, keyframes, deltas,
/// telemetry, metrics, provenance and events — a producer that emitted only keyframes could
/// satisfy density by accident.
#[test]
fn h4_seq_is_dense_and_monotonic_and_hello_carries_the_next_one() {
    let shape = RunShape {
        actors: 5,
        steps: 25,
        ..RunShape::default()
    };
    let frames = live_frames(&shape).expect("the fixture encodes");

    let mut expected_next = 0u64;
    let mut canonical = 0usize;
    let mut connection = 0usize;
    let mut kinds = std::collections::BTreeSet::new();

    for (i, frame) in frames.iter().enumerate() {
        let header = frame.header().expect("a header");
        let kind = header.kind().expect("a known message type");
        kinds.insert(header.msg_type);
        if kind.is_canonical() {
            assert_eq!(
                header.seq, expected_next,
                "frame {i} ({kind:?}) broke the dense sequence"
            );
            expected_next += 1;
            canonical += 1;
        } else {
            assert_eq!(
                header.seq, expected_next,
                "frame {i} ({kind:?}) is a connection frame, so it must carry the seq the \
                 next canonical frame will have"
            );
            connection += 1;
        }
    }

    assert!(canonical > 10, "only {canonical} canonical frames");
    assert_eq!(connection, 1, "exactly one Hello");
    assert!(
        kinds.len() >= 5,
        "the stream carries only {} message types, so density was checked over too \
         narrow a stream: {kinds:?}",
        kinds.len()
    );
}

/// **H5** — "`?resume=<seq>` inside the ring resumes with `HELLO_RESUMED` and no gap."
///
/// The ring half: everything from the resume point is replayed, in order, with no missing
/// sequence number. The `HELLO_RESUMED` flag half belongs to the session and is owned by
/// `crates/v2xw-server/tests/session.rs`.
#[test]
fn h5_a_resume_point_inside_the_ring_replays_without_a_gap() {
    let frames_per_gop = 4;
    let mut ring = ResumeRing::new(frames_per_gop);
    for seq in 0..20u64 {
        let keyframe = seq % frames_per_gop as u64 == 0;
        let kind = if keyframe {
            MsgType::Keyframe
        } else {
            MsgType::Delta
        };
        ring.push(RingEntry {
            seq,
            keyframe,
            frame: Frame::new(kind, seq, 0, &vec![0u8; 48]).expect("frame"),
        });
    }

    assert!(ring.can_resume(12), "12 sits behind the keyframe at 12");
    let replayed: Vec<u64> = ring.replay_from(12).into_iter().map(|e| e.seq).collect();
    assert_eq!(
        replayed,
        (12..20).collect::<Vec<u64>>(),
        "the backlog must be dense from the resume point to the head"
    );
    for (entry, seq) in ring.replay_from(12).into_iter().zip(12u64..) {
        assert_eq!(
            entry.frame.seq().expect("a header"),
            seq,
            "the replayed frame's own header must carry its sequence number"
        );
    }

    // The injected fault: a sequence number the ring never held must not resume, or a
    // client would silently continue from the wrong place.
    assert!(
        !ring.can_resume(10_000),
        "a seq past the head cannot resume"
    );
    assert!(
        ring.replay_from(10_000).is_empty(),
        "an unresumable point must hand back nothing rather than the whole ring"
    );
}
