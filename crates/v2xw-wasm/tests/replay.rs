//! Host-side tests for the replay session: the seek contract, the delta application and
//! the range-fetch loop.
//!
//! Everything under test here is target-independent, which is the point: the `wasm32`
//! build adds only `wasm-bindgen` glue, so what is proved on the host is what ships in
//! the browser. `tests/node/smoke.mjs` then proves the two builds agree on one file.
//!
//! # What "independent" means in this file
//!
//! [`reference`] re-applies the *live* frame stream — the frames
//! [`v2xw_record::fixture::write_recording`] handed to the container, not the frames the
//! reader read back — with a different data structure (a `BTreeMap` of rows rather than
//! columns) and a separately written application of §3.4. A seek that lands on the same
//! numbers has therefore agreed with something that does not share its code.

use std::collections::BTreeMap;

use v2xw_core::time::SimTime;
use v2xw_record::Reader;
use v2xw_record::encoder::Cadence;
use v2xw_record::fixture::{self, RunShape};
use v2xw_record::wire::snapshot::{DeltaBody, KeyframeBody, MFLAG_ABSOLUTE, MFLAG_LANE_CHANGED};
use v2xw_record::wire::{Frame, MsgType, U32_NONE};
use v2xw_wasm::{ByteCache, ReplaySession, Scene, Step, seconds_to_ns};

/// A run small enough to be quick and long enough to span several GOPs and chunks.
fn shape() -> RunShape {
    RunShape {
        actors: 64,
        steps: 300,
        signals: 6,
        cadence: Cadence::DEFAULT,
        teleport_at: Some(97),
        ..Default::default()
    }
}

struct Recording {
    bytes: Vec<u8>,
    path: std::path::PathBuf,
    frames: Vec<Frame>,
}

fn recording(tag: &str) -> Recording {
    let dir = fixture::scratch_dir(tag).expect("a scratch directory");
    let path = dir.join(format!("{tag}.mcap"));
    let (frames, _) = fixture::write_recording(&path, &shape()).expect("a recording");
    let bytes = std::fs::read(&path).expect("the recording reads back");
    Recording {
        bytes,
        path,
        frames,
    }
}

/// One actor slot, as an independent applier tracks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Row {
    actor_id: u32,
    x_mm: i32,
    y_mm: i32,
    z_mm: i32,
    lane_id: u32,
    heading: u16,
    speed: i16,
    accel: i16,
    class_idx: u8,
    state: u8,
    neighbors: u8,
}

/// The state the live stream is in at `t`, applied row-wise from the frames the fixture
/// produced.
mod reference {
    use super::*;

    pub struct State {
        pub rows: BTreeMap<u32, Row>,
        pub signals: BTreeMap<u32, (u16, u8)>,
        pub origin: [f64; 3],
        pub slots: usize,
    }

    pub fn at(frames: &[Frame], t: SimTime) -> State {
        let mut rows: BTreeMap<u32, Row> = BTreeMap::new();
        let mut signals: BTreeMap<u32, (u16, u8)> = BTreeMap::new();
        let mut origin = [0.0; 3];
        let mut slots = 0usize;
        for frame in frames {
            let header = frame.header().expect("a frame header");
            let kind = header.kind();
            if matches!(kind, Some(MsgType::Keyframe) | Some(MsgType::Delta))
                && frame.sim_time().expect("a snapshot time") > t
            {
                break;
            }
            match kind {
                Some(MsgType::Keyframe) => {
                    let kf = KeyframeBody::decode(frame.body()).expect("a keyframe");
                    origin = kf.origin;
                    slots = kf.actors.len();
                    rows.clear();
                    signals.clear();
                    for (slot, a) in kf.actors.iter().enumerate() {
                        rows.insert(
                            slot as u32,
                            Row {
                                actor_id: a.actor_id,
                                x_mm: a.x_mm,
                                y_mm: a.y_mm,
                                z_mm: i32::from(a.z_cm) * 10,
                                lane_id: a.lane_id,
                                heading: a.heading_brad,
                                speed: a.speed_cq,
                                accel: a.accel_cq,
                                class_idx: a.class_idx,
                                state: a.state,
                                neighbors: a.verified_neighbors,
                            },
                        );
                    }
                    for s in &kf.signals {
                        signals.insert(s.signal_id, (s.time_to_change_ds, s.phase));
                    }
                }
                Some(MsgType::Delta) => {
                    let d = DeltaBody::decode(frame.body()).expect("a delta");
                    for s in &d.spawns {
                        // §3.3.1: `actor_count` is the high-water mark at the keyframe,
                        // and the allocator hands out the lowest free slot afterwards, so
                        // a spawn may sit above it and the dense array grows.
                        slots = slots.max(s.slot as usize + 1);
                        rows.insert(
                            s.slot,
                            Row {
                                actor_id: s.actor_id,
                                x_mm: s.x_mm,
                                y_mm: s.y_mm,
                                z_mm: i32::from(s.z_cm) * 10,
                                lane_id: s.lane_id,
                                heading: s.heading_brad,
                                speed: s.speed_cq,
                                accel: 0,
                                class_idx: s.class_idx,
                                state: s.state,
                                neighbors: s.verified_neighbors,
                            },
                        );
                    }
                    let mut abs = d.abs.iter();
                    let mut lanes = d.lanes.iter();
                    for m in &d.moved {
                        let row = rows.get_mut(&m.slot).expect("a declared slot");
                        if m.mflags & MFLAG_ABSOLUTE != 0 {
                            let a = abs.next().expect("an absolute entry");
                            row.x_mm = a.x_mm;
                            row.y_mm = a.y_mm;
                            row.z_mm = i32::from(a.z_cm) * 10;
                        } else {
                            row.x_mm += i32::from(m.dx_mm);
                            row.y_mm += i32::from(m.dy_mm);
                            row.z_mm += i32::from(m.dz_mm);
                        }
                        if m.mflags & MFLAG_LANE_CHANGED != 0 {
                            row.lane_id = *lanes.next().expect("a lane entry");
                        }
                        row.heading = m.heading_brad;
                        row.speed = m.speed_cq;
                        row.accel = m.accel_cq;
                        row.state = m.state;
                        row.neighbors = m.verified_neighbors;
                    }
                    for r in &d.despawns {
                        rows.insert(
                            r.slot,
                            Row {
                                actor_id: U32_NONE,
                                ..Row::default()
                            },
                        );
                    }
                    for s in &d.signals {
                        signals.insert(s.signal_id, (s.time_to_change_ds, s.phase));
                    }
                }
                _ => {}
            }
        }
        State {
            rows,
            signals,
            origin,
            slots,
        }
    }
}

/// Compares a resolved scene with the independent reference, slot by slot.
fn assert_scene_matches(scene: &Scene, want: &reference::State, at: SimTime) {
    assert_eq!(scene.actor_count(), want.slots, "slot count at t = {at}");
    assert_eq!(scene.origin(), want.origin, "origin at t = {at}");
    for slot in 0..scene.actor_count() {
        let got = Row {
            actor_id: scene.actor_id()[slot],
            x_mm: scene.x_mm()[slot],
            y_mm: scene.y_mm()[slot],
            z_mm: scene.z_mm()[slot],
            lane_id: scene.lane_id()[slot],
            heading: scene.heading_brad()[slot],
            speed: scene.speed_cq()[slot],
            accel: scene.accel_cq()[slot],
            class_idx: scene.class_idx()[slot],
            state: scene.state()[slot],
            neighbors: scene.verified_neighbors()[slot],
        };
        let expected = want.rows.get(&(slot as u32)).copied().unwrap_or_default();
        assert_eq!(got, expected, "slot {slot} at t = {at}");
    }
    for (i, id) in scene.signal_id().iter().enumerate() {
        let expected = want.signals.get(id).copied().expect("a known signal");
        assert_eq!(
            (scene.signal_ttc_ds()[i], scene.signal_phase()[i]),
            expected,
            "signal {id} at t = {at}"
        );
    }
}

/// Every seek target lands on exactly the state the live stream was in.
///
/// This is the property 09-ui §7 and vwp-v1 §3.2 rest on: because a delta is taken
/// against the previously *transmitted* quantised value, keyframe-plus-deltas is equal to
/// the server's pose rather than close to it, so this is an equality test and not a
/// tolerance test.
#[test]
fn a_seek_lands_on_the_live_state() {
    let rec = recording("seek-agreement");
    let mut session = ReplaySession::from_bytes(rec.bytes.clone()).expect("the session opens");
    let (start, end) = session.span().expect("a span").expect("a recorded span");

    // 37 targets spread over the run, deliberately including keyframe boundaries, the
    // step just before one, and the step the fixture teleports on.
    let mut targets: Vec<SimTime> = (0..32).map(|i| start + (end - start) * i / 31).collect();
    targets.push(start);
    targets.push(end);
    targets.push(start + 1_000_000_000);
    targets.push(start + 999_999_999);
    targets.push(start + 9_700_000_000);
    targets.sort_unstable();
    targets.dedup();

    for t in targets {
        let report = session
            .seek(t)
            .expect("the seek runs")
            .done()
            .expect("a resident recording never waits");
        assert!(report.position_ns <= t, "a seek never overshoots t = {t}");
        assert!(
            report.keyframe_time_ns <= t,
            "the keyframe is at or before t = {t}"
        );
        assert_scene_matches(session.scene(), &reference::at(&rec.frames, t), t);
    }
}

/// §7.3 and 09-ui §7: a seek reads the preceding keyframe plus at most one keyframe
/// period of deltas, out of one or two chunks.
#[test]
fn the_seek_contract_is_bounded() {
    let rec = recording("seek-bounds");
    let mut session = ReplaySession::from_bytes(rec.bytes).expect("the session opens");
    let cadence = session.cadence().expect("a cadence");
    let max_deltas =
        (cadence.keyframe_period.as_nanos() / cadence.mobility_step.as_nanos()) as usize;
    assert_eq!(
        max_deltas, 10,
        "the fixture records the default 1 s / 100 ms"
    );
    let (start, end) = session.span().expect("a span").expect("a recorded span");

    for i in 0..=200u64 {
        let t = start + (end - start) * i / 200;
        let report = session
            .seek(t)
            .expect("the seek runs")
            .done()
            .expect("resident");
        assert!(
            report.deltas <= max_deltas,
            "{} deltas at t = {t} exceeds one keyframe period",
            report.deltas
        );
        assert!(
            report.chunks_read <= 2,
            "{} chunks read at t = {t}",
            report.chunks_read
        );
        assert!(
            t - report.keyframe_time_ns < cadence.keyframe_period.as_nanos(),
            "the covering keyframe is more than a period before t = {t}"
        );
    }
}

/// A recording scrubbed by range request lands on the same state as one held in memory,
/// and does not download the file to get there.
///
/// Two costs are measured separately because they behave differently. Opening the index
/// is vwp-v1 §7.3 steps 1-3 — footer, summary, then every chunk's message index — and an
/// MCAP message index sits immediately after the chunk it describes, so that walk is
/// spread over the whole file and costs one small request per chunk. A *seek* is then
/// steps 4-8 against an index already in hand, and costs one chunk.
#[test]
fn range_requests_scrub_without_downloading_the_file() {
    // A longer run with a small chunk target, so the file is many chunks: a recording
    // that fits in one chunk cannot demonstrate anything about range requests, because
    // one seek legitimately reads all of it.
    let dir = fixture::scratch_dir("range-scrub").expect("a scratch directory");
    let path = dir.join("range-scrub.mcap");
    let big = RunShape {
        actors: 400,
        steps: 3_000,
        signals: 8,
        cadence: Cadence::DEFAULT,
        teleport_at: Some(400),
        ..Default::default()
    };
    let (frames, _) = v2xw_record::fixture::write_recording_with(
        &path,
        &big,
        v2xw_record::RecordingOptions {
            cadence: big.cadence,
            chunk_target_bytes: 256 * 1024,
            ..Default::default()
        },
        false,
    )
    .expect("a recording");
    let rec = Recording {
        bytes: std::fs::read(&path).expect("the recording reads back"),
        path,
        frames,
    };
    let total = rec.bytes.len() as u64;
    assert!(
        total > 2 * 1024 * 1024,
        "the range fixture is only {total} bytes"
    );

    let mut resident = ReplaySession::from_bytes(rec.bytes.clone()).expect("the session opens");
    let (start, end) = resident.span().expect("a span").expect("a recorded span");

    let mut ranged = ReplaySession::over(ByteCache::new(total));
    // The fetch side of the loop: in the browser this is `fetch` with a `Range` header.
    let fetch = |off: u64, len: u64| -> Vec<u8> {
        let from = off as usize;
        let to = (off + len).min(total) as usize;
        rec.bytes[from..to].to_vec()
    };

    let mut open_rounds = 0u32;
    loop {
        match ranged.open().expect("the open runs") {
            Step::Done(()) => break,
            Step::Need(ranges) => {
                open_rounds += 1;
                assert!(open_rounds < 512, "the open loop did not converge");
                assert!(!ranges.is_empty(), "a wait always names a range");
                for (off, len) in ranges {
                    assert!(off + len <= total, "a wanted range is inside the file");
                    ranged.supply(off, fetch(off, len));
                }
            }
        }
    }
    let after_open = ranged.resident_bytes();
    let chunks = ranged.index().expect("an index").chunks.len();
    assert!(chunks > 8, "the range fixture is only {chunks} chunks");
    // The index walk reads kilobytes per chunk, not the chunks themselves.
    // The index walk reads kilobytes per chunk, not the chunks themselves, and costs
    // one small request per chunk rather than one per block of the file.
    assert!(
        after_open * 8 < total,
        "opening the index fetched {after_open} of {total} bytes"
    );
    assert!(
        u64::from(open_rounds) <= chunks as u64 + 32,
        "opening {chunks} chunks' indexes took {open_rounds} range requests"
    );

    let seek_rounds = std::cell::Cell::new(0u32);
    let drive = |session: &mut ReplaySession, t: SimTime| {
        for round in 0..256u32 {
            match session.seek(t).expect("the seek runs") {
                Step::Done(r) => {
                    seek_rounds.set(seek_rounds.get().max(round));
                    return r;
                }
                Step::Need(ranges) => {
                    assert!(!ranges.is_empty(), "a wait always names a range");
                    for (off, len) in ranges {
                        assert!(off + len <= total, "a wanted range is inside the file");
                        session.supply(off, fetch(off, len));
                    }
                }
            }
        }
        panic!("the seek loop did not converge in 256 rounds");
    };

    let mut after_first = 0u64;
    for i in 0..8u64 {
        let t = start + (end - start) * i / 7;
        let want = drive(&mut ranged, t);
        if i == 0 {
            after_first = ranged.resident_bytes();
        }
        let got = resident
            .seek(t)
            .expect("the seek runs")
            .done()
            .expect("resident");
        assert_eq!(want.keyframe_time_ns, got.keyframe_time_ns);
        assert_eq!(want.position_ns, got.position_ns);
        assert_eq!(want.deltas, got.deltas);
        assert_eq!(want.actor_count, got.actor_count);
        assert_scene_matches(ranged.scene(), &reference::at(&rec.frames, t), t);
    }

    // A seek is a lookup, not a download: it reads its own chunk and nothing else.
    assert!(
        seek_rounds.get() <= 4,
        "a seek took {} ask-and-retry rounds against an open index",
        seek_rounds.get()
    );
    let first_seek = after_first - after_open;
    assert!(
        first_seek * 4 < total,
        "the first seek fetched {first_seek} of {total} bytes"
    );
    let fetched = ranged.resident_bytes();
    assert!(
        fetched * 2 < total,
        "eight seeks fetched {fetched} of {total} bytes, which is most of the file"
    );
}

/// A seek past the end of the recording resolves to the last state rather than failing,
/// and a session that has not read its index yet says so rather than pretending.
///
/// The symmetric case — a target *before* the first keyframe — cannot be built from this
/// fixture, whose span starts at t = 0, so it is not asserted here; it is
/// `v2xw-record`'s `tests/seek.rs`, which constructs it directly against the index.
#[test]
fn the_edges_of_the_span_behave() {
    let rec = recording("seek-range");
    let mut session = ReplaySession::from_bytes(rec.bytes).expect("the session opens");
    let (start, end) = session.span().expect("a span").expect("a recorded span");
    assert_eq!(start, 0, "the fixture starts at t = 0");

    // Past the end clamps to the last keyframe, because a recording that stopped is a
    // recording whose last state is the answer (§7.3: the keyframe at or before t).
    let report = session
        .seek(end + 60_000_000_000)
        .expect("past the end resolves")
        .done()
        .expect("resident");
    assert!(report.position_ns <= end);
    assert_scene_matches(session.scene(), &reference::at(&rec.frames, end), end);

    // An unopened ranged session has no index to answer from and says so, rather than
    // reporting a zero span that a scrub bar would draw.
    let unopened = ReplaySession::ranged(4_096);
    assert!(!unopened.is_open());
    assert!(unopened.span().is_err(), "an unopened session has no span");
    assert!(
        unopened.cadence().is_err(),
        "an unopened session has no cadence"
    );
}

/// A scrub bar hands over a floating-point number of seconds; the nanosecond it becomes
/// must not depend on which side of a boundary the pointer was.
#[test]
fn seconds_round_half_away_from_zero() {
    assert_eq!(seconds_to_ns(0.0), 0);
    assert_eq!(seconds_to_ns(1.0), 1_000_000_000);
    assert_eq!(seconds_to_ns(12.5), 12_500_000_000);
    assert_eq!(seconds_to_ns(1.000_000_000_5), 1_000_000_001);
    assert_eq!(seconds_to_ns(-4.0), 0, "a negative target clamps to zero");
    assert_eq!(seconds_to_ns(f64::NAN), 0, "NaN clamps rather than wraps");
    assert_eq!(
        seconds_to_ns(f64::INFINITY),
        u64::MAX,
        "an infinite target saturates rather than wrapping to zero"
    );
}

/// The native reader on the same file reports the same keyframe and delta bytes the
/// session resolved from, which is §7.2's byte identity surviving the wasm-facing
/// wrapper.
#[test]
fn the_session_hands_back_the_recorded_frames() {
    let rec = recording("frames");
    let mut native = Reader::open(&rec.path).expect("the native reader opens");
    let mut session = ReplaySession::from_bytes(rec.bytes).expect("the session opens");
    let (start, end) = session.span().expect("a span").expect("a recorded span");

    for i in 0..5u64 {
        let t = start + (end - start) * i / 4;
        session
            .seek(t)
            .expect("the seek runs")
            .done()
            .expect("resident");
        let want = native.seek(t).expect("the native seek runs");
        assert_eq!(
            session.keyframe_frame().expect("a keyframe"),
            want.keyframe.as_bytes(),
            "keyframe bytes at t = {t}"
        );
        assert_eq!(session.delta_frames(), want.deltas.len());
        for (j, delta) in want.deltas.iter().enumerate() {
            assert_eq!(
                session.delta_frame(j).expect("a delta"),
                delta.as_bytes(),
                "delta {j} at t = {t}"
            );
        }
    }
}
