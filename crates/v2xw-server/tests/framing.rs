//! The resume ring and the backpressure queue of §1.4 and §1.5.
//!
//! §1.5 is the one part of the transport whose failure mode is silent: a queue that grows
//! without bound looks fine until the process is killed, and a queue that drops the wrong
//! frame produces a client that renders subtly wrong poses. Both of the properties below
//! are therefore checked by *injecting the fault* — a queue that cannot overflow is not
//! evidence that the overflow path works.

use v2xw_record::MsgType;
use v2xw_record::wire::Frame;
use v2xw_server::ring::{MAX_RING_FRAMES, Priority, ResumeRing, RingEntry, SendQueue};

fn frame(kind: MsgType, seq: u64, body_bytes: usize) -> Frame {
    Frame::new(kind, seq, 0, &vec![0u8; body_bytes]).expect("frame")
}

#[test]
fn a_resume_point_is_only_usable_with_its_keyframe() {
    let mut ring = ResumeRing::new(4);
    // A delta with no keyframe in front of it is not resumable: §1.4 rule 1 needs both.
    ring.push(RingEntry {
        seq: 0,
        keyframe: false,
        frame: frame(MsgType::Delta, 0, 64),
    });
    assert!(!ring.can_resume(0), "a delta alone must not be resumable");

    ring.push(RingEntry {
        seq: 1,
        keyframe: true,
        frame: frame(MsgType::Keyframe, 1, 64),
    });
    ring.push(RingEntry {
        seq: 2,
        keyframe: false,
        frame: frame(MsgType::Delta, 2, 64),
    });
    assert!(ring.can_resume(1));
    assert!(
        ring.can_resume(2),
        "a delta behind a retained keyframe resumes"
    );
    assert!(
        !ring.can_resume(99),
        "a seq the ring never held does not resume"
    );

    let replayed: Vec<u64> = ring.replay_from(1).into_iter().map(|e| e.seq).collect();
    assert_eq!(replayed, vec![1, 2]);
}

#[test]
fn the_ring_is_bounded_by_frame_count() {
    let mut ring = ResumeRing::new(2);
    for seq in 0..(MAX_RING_FRAMES as u64 + 500) {
        ring.push(RingEntry {
            seq,
            keyframe: seq % 10 == 0,
            frame: frame(MsgType::Delta, seq, 32),
        });
    }
    assert!(
        ring.len() <= MAX_RING_FRAMES,
        "ring held {} frames, cap is {MAX_RING_FRAMES}",
        ring.len()
    );
}

#[test]
fn the_ring_is_bounded_by_bytes_but_never_below_two_gops() {
    let mut ring = ResumeRing::new(4);
    // 64 KiB bodies: 8 MiB is reached after ~128 of them.
    for seq in 0..400u64 {
        ring.push(RingEntry {
            seq,
            keyframe: seq % 4 == 0,
            frame: frame(MsgType::Delta, seq, 64 * 1024),
        });
    }
    assert!(
        ring.bytes() <= v2xw_server::ring::MAX_RING_BYTES + 64 * 1024,
        "ring holds {} bytes",
        ring.bytes()
    );
    assert!(ring.len() >= 8, "two GOPs of four frames must survive");
}

#[test]
fn priority_classes_match_the_table_of_section_1_5() {
    assert_eq!(Priority::of(MsgType::Hello), Priority::P0);
    assert_eq!(Priority::of(MsgType::Error), Priority::P0);
    assert_eq!(Priority::of(MsgType::Bye), Priority::P0);
    assert_eq!(Priority::of(MsgType::WorldChunk), Priority::P0);
    assert_eq!(Priority::of(MsgType::Keyframe), Priority::P1);
    assert_eq!(Priority::of(MsgType::Delta), Priority::P2);
    assert_eq!(Priority::of(MsgType::Telemetry), Priority::P3);
    assert_eq!(Priority::of(MsgType::Event), Priority::P3);
    assert_eq!(Priority::of(MsgType::MetricSample), Priority::P3);
}

#[test]
fn deltas_are_dropped_all_or_nothing_and_ask_for_a_resync() {
    // The fault is injected: a queue of four frames, filled with deltas, then asked to
    // take one more. Without the injection this path never runs.
    let mut queue = SendQueue::new(4, 1 << 20);
    for seq in 0..4 {
        let report = queue.enqueue(frame(MsgType::Delta, seq, 128));
        assert!(report.dropped.is_empty(), "no drop below cap");
    }
    let report = queue.enqueue(frame(MsgType::Delta, 4, 128));
    assert_eq!(report.dropped.delta, 4, "every queued delta went, not some");
    assert!(report.resync_pending, "a delta drop owes a resync keyframe");
    assert_eq!(report.seq_first, Some(0));
    assert_eq!(report.seq_last, Some(3));
    assert_eq!(queue.len(), 1, "only the new delta is queued");
}

#[test]
fn a_keyframe_is_coalesced_rather_than_queued_twice() {
    let mut queue = SendQueue::new(16, 1 << 20);
    queue.enqueue(frame(MsgType::Keyframe, 0, 256));
    queue.enqueue(frame(MsgType::Delta, 1, 64));
    queue.enqueue(frame(MsgType::Keyframe, 2, 256));
    let seqs: Vec<u64> = std::iter::from_fn(|| queue.pop())
        .map(|f| f.header().expect("header").seq)
        .collect();
    assert_eq!(
        seqs,
        vec![1, 2],
        "the older keyframe is replaced by the newer one, the delta survives"
    );
}

#[test]
fn p3_frames_are_shed_oldest_first_and_counted_per_class() {
    let mut queue = SendQueue::new(3, 1 << 20);
    queue.enqueue(frame(MsgType::Telemetry, 0, 64));
    queue.enqueue(frame(MsgType::Event, 1, 64));
    queue.enqueue(frame(MsgType::MetricSample, 2, 64));
    let report = queue.enqueue(frame(MsgType::Telemetry, 3, 64));
    assert_eq!(report.dropped.telemetry, 1, "the oldest P3 went first");
    assert_eq!(report.dropped.event, 0);
    assert_eq!(report.dropped.metric, 0);
}

#[test]
fn a_p0_frame_displaces_droppable_frames_rather_than_being_refused() {
    let mut queue = SendQueue::new(2, 1 << 20);
    queue.enqueue(frame(MsgType::Delta, 0, 64));
    queue.enqueue(frame(MsgType::Delta, 1, 64));
    let report = queue.enqueue(frame(MsgType::Bye, 2, 32));
    assert!(!report.fatal, "a Bye must fit");
    assert_eq!(report.dropped.delta, 2);
    let kinds: Vec<u16> = std::iter::from_fn(|| queue.pop())
        .map(|f| f.header().expect("header").msg_type)
        .collect();
    assert_eq!(kinds, vec![MsgType::Bye.id()]);
}

#[test]
fn a_p0_frame_larger_than_the_whole_cap_is_reported_fatal() {
    // §1.5: "if P0 cannot be queued the connection is closed with 1011". The only way
    // that happens is a single frame past the byte cap, so that is what is injected.
    let mut queue = SendQueue::new(64, 512);
    let report = queue.enqueue(frame(MsgType::Error, 0, 4096));
    assert!(report.fatal, "an oversized P0 frame must be reported fatal");
}

#[test]
fn queue_bytes_never_exceed_the_cap_under_a_client_that_reads_nothing() {
    // Conformance H7's property, at the queue level: under a client that never drains,
    // queued bytes stay under `max_queued_bytes` however long the producer runs.
    let mut queue = SendQueue::new(64, 256 * 1024);
    for seq in 0..5_000u64 {
        let kind = match seq % 11 {
            0 => MsgType::Keyframe,
            1 | 2 => MsgType::Telemetry,
            3 => MsgType::Event,
            _ => MsgType::Delta,
        };
        queue.enqueue(frame(kind, seq, 8 * 1024));
        assert!(
            queue.bytes() <= 256 * 1024,
            "queued {} bytes at seq {seq}",
            queue.bytes()
        );
        assert!(
            queue.len() <= 64,
            "queued {} frames at seq {seq}",
            queue.len()
        );
    }
}
