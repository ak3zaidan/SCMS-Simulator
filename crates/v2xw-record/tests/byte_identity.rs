//! The property the crate exists to have: a replayed stream is byte-identical to the
//! live one (§7.2, conformance P1 `golden_live_vs_replay`, P3).
//!
//! Also here: every channel round-trips (the container does not lose or reorder a
//! message), `verify` accepts a well-formed recording, and the mixed-`Event`-batch
//! splitter behaves as §7.1 requires.

use v2xw_record::fixture::{RunShape, scratch_dir, write_recording};
use v2xw_record::wire::{CANONICAL_FLAG_MASK, FLAG_RESYNC, MsgType, TRANSPORT_FLAG_MASK};
use v2xw_record::{Reader, RecordingOptions, RecordingWriter};

#[test]
fn a_replayed_stream_is_byte_identical_to_the_live_one() {
    let dir = scratch_dir("byte-identity").expect("a scratch directory");
    let path = dir.join("run.mcap");
    let shape = RunShape {
        actors: 6,
        steps: 45,
        teleport_at: Some(23),
        // An attacker in the run, so the frames whose *existence* depends on a §5.2
        // ground-truth field are part of what round-trips here as well as in
        // `tests/node_profile.rs`.
        parked_attacker: true,
        ..Default::default()
    };
    let (live, summary) = write_recording(&path, &shape).expect("the recording is written");
    assert!(summary.frame_count > 0 && summary.record_count > 0);

    let mut reader = Reader::open(&path).expect("the recording opens");
    let replayed = reader.replay().expect("the recording replays");

    assert_eq!(
        live.len(),
        replayed.len(),
        "the replay produced a different number of frames"
    );
    let mut masked_at_least_one = false;
    for (i, (l, r)) in live.iter().zip(replayed.iter()).enumerate() {
        let canonical = l.canonical();
        assert_eq!(
            canonical.as_bytes(),
            r.frame.as_bytes(),
            "frame {i} ({:#06x}) differs between live and replay",
            l.header().expect("a header").msg_type
        );
        if l.as_bytes() != canonical.as_bytes() {
            masked_at_least_one = true;
            let flags = l.header().expect("a header").flags;
            assert_ne!(
                flags & TRANSPORT_FLAG_MASK,
                0,
                "a frame differed in something other than a transport flag"
            );
            assert_eq!(
                flags & CANONICAL_FLAG_MASK,
                r.frame.header().expect("a header").flags,
                "the canonical flag bits must survive"
            );
        }
    }
    assert!(
        masked_at_least_one,
        "the fixture must contain a keyframe with FLAG_RESYNC, or the masking is untested"
    );

    // P3: no stored frame carries a transport flag bit.
    for r in &replayed {
        let flags = r.frame.header().expect("a header").flags;
        assert_eq!(
            flags & TRANSPORT_FLAG_MASK,
            0,
            "stored frame on {} carries transport flags {flags:#06x}",
            r.topic
        );
    }

    // …and the live stream really did set one, so the assertion above has teeth.
    assert!(
        live.iter().any(|f| {
            let h = f.header().expect("a header");
            h.kind() == Some(MsgType::Keyframe) && h.flags & FLAG_RESYNC != 0
        }),
        "a keyframe re-seeds interpolation state and carries FLAG_RESYNC (§2.3)"
    );
}

#[test]
fn every_channel_round_trips() {
    let dir = scratch_dir("round-trip").expect("a scratch directory");
    let path = dir.join("run.mcap");
    let shape = RunShape::new(5, 30);
    let (live, _) = write_recording(&path, &shape).expect("the recording is written");

    let mut reader = Reader::open(&path).expect("the recording opens");
    let frames = reader.replay().expect("the recording replays");
    let records = reader.records(None).expect("the records come back");

    // The frame topics: one per VWP message type plus one per event channel (§7.1).
    let mut topics: Vec<&str> = frames.iter().map(|f| f.topic.as_str()).collect();
    topics.sort_unstable();
    topics.dedup();
    assert_eq!(
        topics,
        vec![
            "vwp/delta",
            "vwp/event/gt.kinematics",
            "vwp/event/node.tx",
            "vwp/event/phy.rx",
            "vwp/hello",
            "vwp/keyframe",
            "vwp/metric",
            // The fixture delivers the `prov_id`s its metric samples reference before it
            // references them (conformance C5), so a `Provenance` channel is one of the
            // channels a round trip has to preserve.
            "vwp/provenance",
            "vwp/telemetry",
        ]
    );

    // The record topics: one per family of 03-interfaces §14 that the fixture emits.
    let mut channels: Vec<&str> = records.iter().map(|r| r.channel.as_str()).collect();
    channels.sort_unstable();
    channels.dedup();
    assert_eq!(
        channels,
        vec![
            "det.observation",
            "mac.cbr",
            "metric.sample",
            "node.tx",
            "phy.rx"
        ]
    );

    // Every record comes back with the bytes and the time it went in with.
    let written = v2xw_record::fixture::records(&shape);
    assert_eq!(records.len(), written.len());
    for (got, (at, want)) in records.iter().zip(written.iter()) {
        assert_eq!(got.sim_time, *at);
        assert_eq!(got.channel, want.channel);
        assert_eq!(got.json, want.json);
    }

    // And every frame body still decodes, which is what `verify` walks.
    let report = reader.verify().expect("the recording verifies");
    assert_eq!(report.frames as usize, live.len());
    assert_eq!(report.records as usize, written.len());
    assert_eq!(report.keyframes, 3, "30 steps of 100 ms at a 1 s cadence");
    assert_eq!(report.deltas, 27);
    assert_eq!(
        report.max_snapshot_gap_ns,
        shape.cadence.mobility_step.as_nanos()
    );
}

#[test]
fn the_channels_carry_their_visibility_tag_and_their_schema() {
    let dir = scratch_dir("self-describing").expect("a scratch directory");
    let path = dir.join("run.mcap");
    write_recording(&path, &RunShape::new(3, 12)).expect("the recording is written");
    let reader = Reader::open(&path).expect("the recording opens");

    for channel in reader.channels() {
        assert!(
            channel.visibility().is_some(),
            "channel {} carries no visibility tag",
            channel.topic
        );
        let schema = reader
            .index()
            .schemas
            .get(&channel.schema_id)
            .unwrap_or_else(|| panic!("channel {} has no schema record", channel.topic));
        assert!(
            !schema.data.is_empty(),
            "channel {}'s schema record is empty, so the file is not self-describing (§7.1)",
            channel.topic
        );
        let text = String::from_utf8_lossy(&schema.data);
        if channel.message_encoding == "vwp1" {
            assert!(
                text.contains("magic      = 0x31505756"),
                "the frame layout must include the header (§2.1)"
            );
            assert!(schema.name.starts_with("vwp.v1."), "§7.1 names the schema");
        } else {
            assert!(
                text.contains("json-schema.org"),
                "a record schema is a JSON Schema"
            );
        }
    }

    // The ground-truth channels are tagged as such, which is what the NODE-only strip
    // and the leakage linter key on.
    let gt: Vec<&str> = reader
        .ground_truth_channels()
        .map(|c| c.topic.as_str())
        .collect();
    assert!(
        gt.contains(&"vwp/event/gt.kinematics"),
        "the GT event channel must be tagged: {gt:?}"
    );
    assert!(
        gt.contains(&"vwp/keyframe") && gt.contains(&"vwp/delta"),
        "the snapshot channels are mixed, which is GT-tainted: {gt:?}"
    );
}

#[test]
fn the_recording_carries_the_manifest_the_cadence_and_the_attachments() {
    let dir = scratch_dir("manifest").expect("a scratch directory");
    let path = dir.join("run.mcap");
    let shape = RunShape::new(3, 12);
    write_recording(&path, &shape).expect("the recording is written");

    let mut reader = Reader::open(&path).expect("the recording opens");
    assert_eq!(
        reader.cadence(),
        shape.cadence,
        "§8.6: the format version and the cadence are recorded"
    );
    assert_eq!(
        reader
            .manifest()
            .get("vwp_version_major")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        reader
            .manifest()
            .get("vwp_version_minor")
            .map(String::as_str),
        Some("0")
    );
    assert_eq!(
        reader.manifest().get("profile").map(String::as_str),
        Some("full")
    );
    assert!(
        reader
            .manifest_json()
            .is_some_and(|j| j.contains("v2xw/manifest/1")),
        "the run manifest is stored as JSON (§7.1)"
    );
    assert_eq!(
        reader
            .attachment("scenario.yaml")
            .expect("the attachment is there"),
        b"name: fixture\n"
    );
}

#[test]
fn a_mixed_event_batch_is_split_by_channel() {
    // §7.1: "the recorder splits mixed batches by channel so that per-channel message
    // indexes work". A single-channel batch is stored verbatim; a mixed one is not, and
    // this is the only place in the crate where a frame is re-encoded on the way in.
    use v2xw_record::wire::event::{EventBody, EventEntry};

    let dir = scratch_dir("event-split").expect("a scratch directory");
    let path = dir.join("run.mcap");
    let shape = RunShape::new(2, 10);
    let mut writer =
        RecordingWriter::create(&path, RecordingOptions::default()).expect("a recording");
    // A keyframe first, so the file has a snapshot channel and a valid GOP.
    let live = v2xw_record::fixture::live_frames(&shape).expect("frames");
    for frame in live.iter().take(2) {
        writer.write_frame(frame).expect("the frame is stored");
    }
    let mixed = EventBody::new(
        0,
        0,
        vec![
            EventEntry {
                sim_time_ns: 0,
                channel_id: 10,
                payload: vec![1u8; 40],
            },
            EventEntry {
                sim_time_ns: 0,
                channel_id: 12,
                payload: vec![2u8; 16],
            },
        ],
    );
    writer
        .write_frame(&mixed.to_frame(9, 0).expect("a frame"))
        .expect("the mixed batch is stored");
    writer.finish().expect("the recording finishes");

    let mut reader = Reader::open(&path).expect("the recording opens");
    let frames = reader.replay().expect("the recording replays");
    let events: Vec<&v2xw_record::RecordedFrame> = frames
        .iter()
        .filter(|f| f.topic.starts_with("vwp/event/"))
        .collect();
    assert_eq!(events.len(), 2, "one message per channel");
    let mut topics: Vec<&str> = events.iter().map(|e| e.topic.as_str()).collect();
    topics.sort_unstable();
    assert_eq!(topics, vec!["vwp/event/mac.cbr", "vwp/event/node.tx"]);
    for e in events {
        let body = EventBody::decode(e.frame.body()).expect("the split batch decodes");
        assert_eq!(body.entries.len(), 1);
        assert_eq!(body.entries[0].payload.len() % 8, 0, "§3.6.1 pads to 8");
    }
}
