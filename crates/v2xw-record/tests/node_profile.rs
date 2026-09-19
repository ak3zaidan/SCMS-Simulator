//! The `NODE-only` replay profile — §5, ADR 0008's "ground-truth channels are tagged; a
//! `NODE-only` replay profile strips them for blind demonstrations", and conformance V1
//! (`node_profile_leakage`), V2 and V6.
//!
//! The assertion that matters for a demonstration is the negative one: the stripped
//! recording contains **no ground-truth channel at all** — not a disabled one, not an
//! empty one — and every ground-truth field of §5.2's exhaustive table is at its
//! sentinel.

use v2xw_record::fixture::{RunShape, scratch_dir, write_recording_with};
use v2xw_record::profile::{NodeProfileStripper, Profile};
use v2xw_record::wire::event::EventBody;
use v2xw_record::wire::metric::MetricBody;
use v2xw_record::wire::snapshot::{
    DeltaBody, KeyframeBody, MFLAG_LANE_CHANGED, PROFILE_NODE, ST_ATTACKER, ST_EQUIPPED,
};
use v2xw_record::wire::telemetry::TelemetryBody;
use v2xw_record::wire::{
    FLAG_NODE_ONLY, MsgType, U8_NONE, U16_NONE, U32_NONE, get_f32, get_u8, get_u32,
};
use v2xw_record::{Reader, RecordingOptions, RecordingWriter};

fn shape() -> RunShape {
    RunShape {
        actors: 6,
        steps: 45,
        signals: 2,
        teleport_at: None,
        // The run has to contain an actor whose *only* changes are ground truth, or the
        // V5 parity test agrees with the implementation instead of testing it: every other
        // fixture actor moves every step, so the stripper's extra rows never appeared.
        parked_attacker: true,
        ..Default::default()
    }
}

/// The slot the parked attacker of [`RunShape::parked_attacker`] occupies.
fn parked_slot() -> u32 {
    shape().actors
}

/// Strips a `full` recording into a `node` one and returns both paths.
fn strip(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = scratch_dir(tag).expect("a scratch directory");
    let full = dir.join("full.mcap");
    let blind = dir.join("node.mcap");
    let shape = shape();
    write_recording_with(
        &full,
        &shape,
        RecordingOptions {
            cadence: shape.cadence,
            ..Default::default()
        },
        true,
    )
    .expect("the full recording is written");

    let mut reader = Reader::open(&full).expect("the full recording opens");
    let frames = reader.replay().expect("the full recording replays");
    let records = reader.records(None).expect("the records come back");

    let mut writer = RecordingWriter::create(
        &blind,
        RecordingOptions {
            cadence: shape.cadence,
            profile: Profile::NodeOnly,
            ..Default::default()
        },
    )
    .expect("the blind recording is created");
    writer
        .write_manifest(r#"{"schema":"v2xw/manifest/1","profile":"node"}"#)
        .expect("the manifest is written");
    let mut stripper = NodeProfileStripper::new();
    for f in &frames {
        if let Some(blanked) = stripper.strip(&f.frame).expect("the frame strips") {
            writer
                .write_frame(&blanked)
                .expect("the blanked frame stores");
        }
    }
    for r in &records {
        // A GT-tainted record has no place in a blind recording, and the recorder refuses
        // one anyway (see `the_recorder_refuses_a_ground_truth_record_in_a_blind_run`).
        let spec = v2xw_record::channels::by_name(&r.channel).expect("a known channel");
        if spec.is_gt_tainted() {
            continue;
        }
        let owned = v2xw_core::OwnedRecord {
            channel: spec.name,
            visibility: spec.visibility,
            json: r.json.clone(),
        };
        writer
            .write_record(r.sim_time, &owned)
            .expect("the record stores");
    }
    writer.finish().expect("the blind recording finishes");
    (full, blind)
}

#[test]
fn the_stripped_recording_contains_no_ground_truth_channel() {
    let (full, blind) = strip("node-channels");

    let full_reader = Reader::open(&full).expect("the full recording opens");
    let gt_before: Vec<String> = full_reader
        .ground_truth_channels()
        .map(|c| c.topic.clone())
        .collect();
    assert!(
        gt_before.iter().any(|t| t == "vwp/event/gt.kinematics"),
        "the full recording must have a GT channel to strip: {gt_before:?}"
    );

    let blind_reader = Reader::open(&blind).expect("the blind recording opens");
    let topics: Vec<&str> = blind_reader.channels().map(|c| c.topic.as_str()).collect();
    for spec in v2xw_record::channels::ground_truth_channels() {
        let event = spec.event_topic();
        let record = spec.record_topic();
        assert!(
            !topics.contains(&event.as_str()),
            "the blind recording still declares {event}"
        );
        assert!(
            !topics.contains(&record.as_str()),
            "the blind recording still declares {record}"
        );
    }
    assert_eq!(blind_reader.profile(), Profile::NodeOnly);

    // The strongest form of the claim: not one channel in the file is tagged as carrying
    // ground truth, so a consumer that filters on the tag alone — a leakage linter, a
    // demonstration UI — sees nothing to filter.
    let still_tagged: Vec<&str> = blind_reader
        .ground_truth_channels()
        .map(|c| c.topic.as_str())
        .collect();
    assert!(
        still_tagged.is_empty(),
        "the blind recording still tags channels as ground truth: {still_tagged:?}"
    );
}

#[test]
fn every_ground_truth_field_of_the_exhaustive_list_is_blanked() {
    let (_, blind) = strip("node-fields");
    let mut reader = Reader::open(&blind).expect("the blind recording opens");
    let frames = reader.replay().expect("the blind recording replays");
    let mut keyframes = 0;
    let mut deltas = 0;
    let mut telemetry = 0;
    let mut metrics = 0;
    let mut rx_payloads = 0;

    for f in &frames {
        let header = f.frame.header().expect("a header");
        let kind = header.kind().expect("a known type");
        if kind.is_canonical() {
            assert_ne!(
                header.flags & FLAG_NODE_ONLY,
                0,
                "§5.3: every canonical frame of a node stream carries FLAG_NODE_ONLY"
            );
        }
        match kind {
            MsgType::Keyframe => {
                keyframes += 1;
                let kf = KeyframeBody::decode(f.frame.body()).expect("a keyframe body");
                assert_eq!(kf.profile, PROFILE_NODE);
                for (slot, row) in kf.actors.iter().enumerate() {
                    if !row.is_occupied() {
                        continue;
                    }
                    assert_eq!(row.lane_id, U32_NONE, "slot {slot} still carries a lane");
                    assert_eq!(row.accel_cq, 0, "slot {slot} still carries an acceleration");
                    assert_eq!(
                        row.state & ST_ATTACKER,
                        0,
                        "slot {slot} is marked as an attacker"
                    );
                    assert_ne!(
                        row.state & ST_EQUIPPED,
                        0,
                        "slot {slot} is occupied by an unequipped actor (V2)"
                    );
                }
            }
            MsgType::Delta => {
                deltas += 1;
                let d = DeltaBody::decode(f.frame.body()).expect("a delta body");
                assert!(d.lanes.is_empty(), "the lane block must be absent (§5.2)");
                for row in &d.moved {
                    assert_eq!(row.accel_cq, 0);
                    assert_eq!(row.state & ST_ATTACKER, 0);
                    assert_eq!(row.mflags & MFLAG_LANE_CHANGED, 0);
                    assert_ne!(row.state & ST_EQUIPPED, 0);
                }
                for s in &d.spawns {
                    assert_eq!(s.lane_id, U32_NONE);
                    assert_eq!(s.cause, U16_NONE);
                    assert_ne!(s.state & ST_EQUIPPED, 0);
                }
                for x in &d.despawns {
                    assert_eq!(x.cause, U16_NONE);
                }
            }
            MsgType::Telemetry => {
                telemetry += 1;
                let t = TelemetryBody::decode(f.frame.body()).expect("a telemetry body");
                for r in &t.records {
                    assert_eq!(r.clock_offset_ns, 0);
                    assert!(r.pos_error_m.is_nan());
                    assert_ne!(r.node_state, 6, "compromised is reported as active (§5.2)");
                }
            }
            MsgType::MetricSample => {
                metrics += 1;
                let m = MetricBody::decode(f.frame.body()).expect("a metric body");
                for s in &m.samples {
                    assert_ne!(s.visibility, 0, "a GT sample must not be emitted (§5.2)");
                }
            }
            MsgType::Event => {
                let body = EventBody::decode(f.frame.body()).expect("an event body");
                for e in &body.entries {
                    assert!(
                        !matches!(e.channel_id, 1..=4),
                        "a GT channel's record survived: {}",
                        e.channel_id
                    );
                    if e.channel_id == 11 {
                        rx_payloads += 1;
                        assert_eq!(
                            get_u32(&e.payload, 20, "tx_node").expect("a payload"),
                            U32_NONE
                        );
                        assert!(
                            get_f32(&e.payload, 36, "distance_m")
                                .expect("a payload")
                                .is_nan()
                        );
                        assert_eq!(
                            get_u8(&e.payload, 42, "los_class").expect("a payload"),
                            U8_NONE
                        );
                    }
                }
            }
            _ => {}
        }
    }
    assert!(keyframes > 0 && deltas > 0 && telemetry > 0 && metrics > 0 && rx_payloads > 0);
    // The blind recording is still a consistent recording, not a mangled one.
    let report = reader.verify().expect("the blind recording verifies");
    assert_eq!(report.profile, Profile::NodeOnly);
    assert_eq!(report.keyframes, keyframes);
}

#[test]
fn stripping_agrees_with_a_live_node_profile_producer() {
    // Conformance V5: "a `full` recording replayed with `profile=node` yields
    // byte-identical frames to a live `node` run of the same scenario".
    //
    // It holds frame for frame, including the header, because the stripper renumbers
    // `seq` densely — a batch that was entirely ground truth is withheld whole, and a
    // stream with gaps could not match one without them (§1.4, H4). `Hello` is excluded,
    // as §7.2 excludes it everywhere: it carries connection state.
    //
    // The run includes a parked attacker (see `shape`), so the two producers are compared
    // on the one case in which they can disagree about *which rows exist* rather than only
    // about what is in them.
    let (_, blind) = strip("node-parity");
    let mut reader = Reader::open(&blind).expect("the blind recording opens");
    let stripped = reader.replay().expect("the blind recording replays");
    let live = v2xw_record::fixture::live_frames(&shape().node_only()).expect("live frames");

    let canonical = |frames: Vec<v2xw_record::Frame>| -> Vec<v2xw_record::Frame> {
        frames
            .into_iter()
            .filter(|f| {
                f.header()
                    .expect("a header")
                    .kind()
                    .is_some_and(MsgType::is_canonical)
            })
            .map(|f| f.canonical())
            .collect()
    };
    let got = canonical(stripped.iter().map(|f| f.frame.clone()).collect());
    let want = canonical(live);
    assert_eq!(
        got.len(),
        want.len(),
        "the two producers disagree on how many canonical frames a node stream has"
    );
    for (i, (a, b)) in got.iter().zip(want.iter()).enumerate() {
        let (ha, hb) = (a.header().expect("a header"), b.header().expect("a header"));
        assert_eq!(
            (ha.msg_type, ha.seq, ha.flags),
            (hb.msg_type, hb.seq, hb.flags),
            "canonical frame {i} has a different header"
        );
        assert_eq!(
            a.as_bytes(),
            b.as_bytes(),
            "canonical frame {i} ({:#06x}) differs between stripping and producing",
            ha.msg_type
        );
    }
}

#[test]
fn a_parked_actor_whose_hidden_fields_change_produces_no_blind_row() {
    // Conformance V5 and V1. The live blind producer blanks *before* quantisation, so the
    // lane and ST_ATTACKER are already gone when §3.4.2's change predicate decides whether
    // to emit a moved row; the stripper blanks a row the full producer already emitted. A
    // parked actor that changes lane at step 3 and whose attacker bit is set at step 6
    // therefore used to get two all-zero moved rows in the blind stream that a live blind
    // run does not emit — rows carrying no information except "a §5.2 ground-truth field
    // changed at this step", which is exactly what the profile withholds.
    let slot = parked_slot();
    let full = v2xw_record::fixture::live_frames(&shape()).expect("the full stream");

    let rows_for_the_parked_slot = |frames: &[v2xw_record::Frame]| -> usize {
        frames
            .iter()
            .filter(|f| f.header().expect("a header").kind() == Some(MsgType::Delta))
            .map(|f| {
                DeltaBody::decode(f.body())
                    .expect("a delta body")
                    .moved
                    .iter()
                    .filter(|r| r.slot == slot)
                    .count()
            })
            .sum()
    };

    // The full stream really does emit rows for it — otherwise the assertion below is
    // vacuous. One for the lane change, one for the attacker bit.
    let in_full = rows_for_the_parked_slot(&full);
    assert_eq!(
        in_full, 2,
        "the fixture must make the full producer emit a row for the parked actor, or this          test proves nothing"
    );

    let mut stripper = NodeProfileStripper::new();
    let stripped: Vec<v2xw_record::Frame> = full
        .iter()
        .filter_map(|f| stripper.strip(f).expect("the frame strips"))
        .collect();
    assert_eq!(
        rows_for_the_parked_slot(&stripped),
        0,
        "the blind stream emits a moved row for an actor that did not move, which announces          that a withheld field changed (§5.2, V1)"
    );

    // …and a live blind producer agrees, which is the other half of the claim.
    let live_blind =
        v2xw_record::fixture::live_frames(&shape().node_only()).expect("the live blind stream");
    assert_eq!(rows_for_the_parked_slot(&live_blind), 0);
}

#[test]
fn the_recorder_refuses_a_ground_truth_record_in_a_blind_run() {
    let dir = scratch_dir("node-refusal").expect("a scratch directory");
    let path = dir.join("refuse.mcap");
    let mut writer = RecordingWriter::create(
        &path,
        RecordingOptions {
            profile: Profile::NodeOnly,
            ..Default::default()
        },
    )
    .expect("a recording");
    let gt = v2xw_core::OwnedRecord {
        channel: "gt.kinematics",
        visibility: v2xw_core::Visibility::Gt,
        json: br#"{"actor_id":0}"#.to_vec(),
    };
    let err = writer
        .write_record(0, &gt)
        .expect_err("a blind recording must refuse ground truth");
    assert!(
        matches!(err, v2xw_record::RecordError::VisibilityDenied { .. }),
        "expected VisibilityDenied, got {err}"
    );

    // …and a ground-truth record on a node channel is refused in any profile, which is
    // the recorder's half of the firewall `v2xw_core::Visibility` declares.
    let mut writer = RecordingWriter::create(dir.join("refuse2.mcap"), RecordingOptions::default())
        .expect("a recording");
    let smuggled = v2xw_core::OwnedRecord {
        channel: "mac.cbr",
        visibility: v2xw_core::Visibility::Gt,
        json: br#"{"node_id":0,"cbr":0.5}"#.to_vec(),
    };
    let err = writer
        .write_record(0, &smuggled)
        .expect_err("a GT record on a NODE channel is refused");
    assert!(matches!(
        err,
        v2xw_record::RecordError::VisibilityDenied { .. }
    ));

    // A frame without FLAG_NODE_ONLY has no place in a blind recording either (§5.3).
    let mut writer = RecordingWriter::create(
        dir.join("refuse3.mcap"),
        RecordingOptions {
            profile: Profile::NodeOnly,
            ..Default::default()
        },
    )
    .expect("a recording");
    let full_frames = v2xw_record::fixture::live_frames(&shape()).expect("live frames");
    let keyframe = full_frames
        .iter()
        .find(|f| f.header().expect("a header").kind() == Some(MsgType::Keyframe))
        .expect("a keyframe");
    let err = writer
        .write_frame(keyframe)
        .expect_err("a full-profile frame must be refused");
    assert!(
        matches!(err, v2xw_record::RecordError::Malformed { .. }),
        "got {err}"
    );
}
