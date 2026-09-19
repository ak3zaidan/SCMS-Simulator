//! The remaining conformance items of §10 that fall inside this crate: the ones about
//! slots, the state byte, telemetry content, `record_size` striding, unknown channels,
//! event ordering, forward compatibility and the version rules.
//!
//! The items that belong to the WebSocket transport (handshake, resume, backpressure,
//! liveness), to the JSON-RPC control surface, to the world payload or to the TypeScript
//! client are not here, because this crate implements none of those; the report says
//! which they are.

use v2xw_core::ids::ActorId;
use v2xw_core::time::Duration;
use v2xw_record::encoder::SlotAllocator;
use v2xw_record::fixture::{RunShape, scratch_dir};
use v2xw_record::wire::event::{EventBody, EventEntry};
use v2xw_record::wire::snapshot::{
    ST_ATTACKER, ST_EQUIPPED, ST_REPORTED, ST_REVOKED, ST_TRANSMITTING,
};
use v2xw_record::wire::telemetry::{NodeTelemetry, RECORD_SIZE_V1, TelemetryBody};
use v2xw_record::wire::{Frame, MsgType, U8_NONE, U16_NONE, U32_NONE, U64_NONE, put_u32};
use v2xw_record::{Reader, RecordError, RecordingOptions, RecordingWriter};

/// Q5: "A slot is not reused until one full keyframe period after its despawn."
#[test]
fn a_slot_is_not_reused_until_a_keyframe_period_after_its_despawn() {
    let period = Duration::from_secs(1);
    let mut slots = SlotAllocator::new(period);
    let a = ActorId::new(10);
    let b = ActorId::new(11);
    let c = ActorId::new(12);

    assert_eq!(slots.allocate(a, 0), 0, "the lowest free slot");
    assert_eq!(slots.allocate(b, 0), 1);
    assert_eq!(slots.allocate(a, 0), 0, "allocation is idempotent");

    assert_eq!(slots.release(a, 1_000_000_000), Some(0));
    // Inside the cooling-off period the freed slot is not handed out again: a delta that
    // arrives late would otherwise be applied to whoever inherited it.
    assert_eq!(slots.allocate(c, 1_500_000_000), 2);
    assert_eq!(slots.occupied().collect::<Vec<_>>(), vec![1, 2]);

    // A full keyframe period later it is free.
    let d = ActorId::new(13);
    assert_eq!(slots.allocate(d, 2_000_000_000), 0);
    assert_eq!(slots.slot_of(d), Some(0));
    assert_eq!(slots.slot_of(a), None);
}

/// Q6: the `state` byte's bits are §3.3.4's, and "benign" is the absence of bits 0–2.
#[test]
fn the_state_byte_means_what_the_specification_says() {
    assert_eq!(ST_ATTACKER, 0x01);
    assert_eq!(ST_REPORTED, 0x02);
    assert_eq!(ST_REVOKED, 0x04);
    assert_eq!(ST_EQUIPPED, 0x08);
    assert_eq!(ST_TRANSMITTING, 0x10);
    let benign = |state: u8| state & (ST_ATTACKER | ST_REPORTED | ST_REVOKED) == 0;
    assert!(benign(ST_EQUIPPED | ST_TRANSMITTING));
    assert!(!benign(ST_EQUIPPED | ST_ATTACKER));
    assert!(!benign(ST_EQUIPPED | ST_REPORTED));
    assert!(!benign(ST_EQUIPPED | ST_REVOKED));
}

/// C1: "Every field of §3.5.2 is populated or explicitly set to its unknown sentinel; no
/// field is silently zero."
#[test]
fn every_telemetry_field_round_trips_and_has_a_sentinel() {
    let unknown = NodeTelemetry::unknown(7);
    assert_eq!(unknown.node_id, 7);
    assert_eq!(unknown.storage_used_b, U64_NONE);
    assert_eq!(unknown.ram_used_kib, U32_NONE);
    assert_eq!(unknown.cpu_util_pm, U16_NONE);
    assert_eq!(unknown.gnss_fix, U8_NONE);
    assert!(unknown.msgs_in_per_s.is_nan());
    assert!(unknown.pos_error_m.is_nan());

    // A fully populated record survives the wire unchanged, field for field.
    let mut full = NodeTelemetry::unknown(3);
    full.storage_used_b = 1_234_567;
    full.storage_total_b = 8_000_000;
    full.next_topup_ns = 42_000_000_000;
    full.crl_bytes = 9_001;
    full.outbox_bytes = 77;
    full.clock_offset_ns = -12_345;
    full.ram_used_kib = 512;
    full.ram_total_kib = 2_048;
    full.drop_rx_overflow = 1;
    full.drop_verify_policy_skip = 2;
    full.drop_verify_overflow = 3;
    full.drop_tx_overflow = 4;
    full.drop_reassembly_timeout = 5;
    full.drop_crl_backlog = 6;
    full.cert_stored = 20;
    full.crl_entries = 30;
    full.outbox_msgs = 4;
    full.peer_cache_entries = 55;
    full.p2pcd_requests = 2;
    full.full_cert_msgs = 9;
    full.msgs_in_per_s = 37.5;
    full.msgs_out_per_s = 10.0;
    full.verifications_per_s = 25.25;
    full.verify_wait_p50_ms = 0.125;
    full.verify_wait_p95_ms = 1.5;
    full.gnss_hdop = 0.75;
    full.gnss_sigma_m = 1.25;
    full.clock_drift_ppm = 2.5;
    full.pos_error_m = 0.5;
    full.airtime_ms_per_s = 12.5;
    full.cpu_util_pm = 431;
    full.hsm_util_pm = 120;
    full.q_rx_p50 = 1;
    full.q_rx_p95 = 9;
    full.q_verify_p50 = 2;
    full.q_verify_p95 = 8;
    full.q_app_p50 = 3;
    full.q_app_p95 = 7;
    full.q_tx_p50 = 4;
    full.q_tx_p95 = 6;
    full.q_crl_p50 = 5;
    full.q_crl_p95 = 5;
    full.dcc_state = 2;
    full.cbr_pm = 345;
    full.tx_power_cdbm = 2_000;
    full.nbr_total = 40;
    full.nbr_verified = 30;
    full.nbr_unverified = 8;
    full.nbr_revoked = 2;
    full.cert_active = 20;
    full.crl_expansion_pm = 500;
    full.unverified_ratio_pm = 50;
    full.gnss_fix = 2;
    full.node_state = 2;
    full.verify_policy = 1;

    let body = TelemetryBody::new(1_000_000_000, 1_000_000_000, vec![unknown, full]);
    let frame = body.to_frame(0, 0).expect("a frame");
    let back = TelemetryBody::decode(frame.body()).expect("it decodes");
    assert_eq!(back.record_size, RECORD_SIZE_V1);
    assert_eq!(back.records[1], full.quantised());
    assert!(back.records[0].msgs_in_per_s.is_nan());
    assert_eq!(back.records[0].storage_used_b, U64_NONE);
    // The frame is exactly the size Appendix B predicts: 32 + 208·N.
    assert_eq!(frame.body().len(), 32 + 208 * 2);
}

/// C2: "`Telemetry.record_size` is honoured for striding, not assumed to be 208."
/// N1: a v1 reader parses a synthetic v1.1 stream, losing only the new information.
#[test]
fn a_synthetic_v1_1_stream_parses_and_loses_only_what_is_new() {
    // A v1.1 telemetry frame: the same fields, a larger `record_size`, and a new field in
    // the tail that a v1 reader must stride over rather than misread (§8.4).
    let mut r = NodeTelemetry::unknown(1);
    r.cbr_pm = 250;
    r.msgs_in_per_s = 1.5;
    let v11_record_size = 224u32;
    let mut body = TelemetryBody::new(2_000_000_000, 1_000_000_000, vec![r, r]);
    body.record_size = v11_record_size;
    let frame = body.to_frame(0, 0).expect("a frame");
    assert_eq!(frame.body().len(), 32 + v11_record_size as usize * 2);

    let back = TelemetryBody::decode(frame.body()).expect("a v1 reader parses it");
    assert_eq!(back.record_size, v11_record_size);
    assert_eq!(back.records.len(), 2);
    for got in &back.records {
        assert_eq!(got.cbr_pm, 250, "striding by 208 would have misread this");
        assert_eq!(got.msgs_in_per_s, 1.5);
    }

    // A v1.1 event channel id: unknown to this build, skipped by `payload_len` (C3).
    let batch = EventBody::new(
        0,
        100,
        vec![
            EventEntry {
                sim_time_ns: 0,
                channel_id: 10,
                payload: vec![7u8; 40],
            },
            EventEntry {
                sim_time_ns: 50,
                channel_id: 1_234,
                payload: vec![9u8; 24],
            },
            EventEntry {
                sim_time_ns: 100,
                channel_id: 12,
                payload: vec![3u8; 16],
            },
        ],
    );
    let frame = batch.to_frame(1, 0).expect("a frame");
    let back = EventBody::decode(frame.body()).expect("a v1 reader parses it");
    assert_eq!(
        back.entries.len(),
        3,
        "the unknown channel did not desynchronise"
    );
    assert_eq!(back.entries[1].channel_id, 1_234);
    assert_eq!(back.entries[1].payload, vec![9u8; 24]);
    assert!(
        v2xw_record::channels::by_wire_id(1_234).is_none(),
        "1234 is a plug-in channel this build does not know (§3.6.2)"
    );
    assert_eq!(
        back.entries[2].payload,
        vec![3u8; 16],
        "and the next one still lands"
    );

    // A v1.1 message type: parsed as a frame, recognised as unknown, not an error (F2).
    let mut odd = frame.as_bytes().to_vec();
    odd[6] = 0x09;
    let odd = Frame::from_bytes(odd).expect("the frame is still well formed");
    assert!(odd.header().expect("a header").kind().is_none());
}

/// C4: "Event index arrays are sorted by `(sim_time_ns, channel_id)` and payloads are
/// 8-aligned."
#[test]
fn an_event_batch_is_sorted_and_eight_aligned_however_it_was_built() {
    let batch = EventBody::new(
        0,
        300,
        vec![
            EventEntry {
                sim_time_ns: 300,
                channel_id: 10,
                payload: vec![1u8; 40],
            },
            EventEntry {
                sim_time_ns: 100,
                channel_id: 12,
                payload: vec![2u8; 16],
            },
            EventEntry {
                sim_time_ns: 100,
                channel_id: 11,
                payload: vec![3u8; 48],
            },
        ],
    );
    let frame = batch.to_frame(0, 0).expect("a frame");
    let back = EventBody::decode(frame.body()).expect("it decodes, so it is sorted");
    let keys: Vec<(u64, u16)> = back
        .entries
        .iter()
        .map(|e| (e.sim_time_ns, e.channel_id))
        .collect();
    assert_eq!(keys, vec![(100, 11), (100, 12), (300, 10)]);

    // Every payload starts 8-aligned within the payload region.
    let body = frame.body();
    let off_payloads = u32::from_le_bytes(body[24..28].try_into().expect("four bytes")) as usize;
    assert_eq!(off_payloads % 8, 0);
    let e = back.entries.len();
    for i in 0..e {
        let at = 32 + 8 * e + 4 * i;
        let payload_off = u32::from_le_bytes(body[at..at + 4].try_into().expect("four bytes"));
        assert_eq!(payload_off % 8, 0, "payload {i} is not 8-aligned");
    }

    // An index that is not sorted is rejected rather than trusted. Swapping the first
    // and last index times puts 300 before 100.
    let mut unsorted = body.to_vec();
    let (a, b) = (32usize, 32 + 8 * (e - 1));
    for k in 0..8 {
        unsorted.swap(a + k, b + k);
    }
    let err = EventBody::decode(&unsorted).expect_err("the index is out of order");
    assert!(matches!(err, RecordError::Malformed { .. }), "got {err}");
}

/// P6: "A recording with a higher major is refused (`-32050`); a higher minor is
/// accepted."
#[test]
fn a_recording_from_a_future_major_is_refused_and_a_future_minor_is_accepted() {
    let dir = scratch_dir("conformance-version").expect("a scratch directory");
    let shape = RunShape::new(3, 12);
    let frames = v2xw_record::fixture::live_frames(&shape).expect("live frames");

    for (tag, major, minor, expect_ok) in [
        ("v1.0", 1u16, 0u16, true),
        ("v1.7", 1, 7, true),
        ("v2.0", 2, 0, false),
    ] {
        let path = dir.join(format!("{tag}.mcap"));
        let mut writer =
            RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
        writer
            .write_manifest(r#"{"schema":"v2xw/manifest/1"}"#)
            .expect("the manifest is written");
        for frame in &frames {
            writer.write_frame(frame).expect("the frame stores");
        }
        writer.finish().expect("the recording finishes");

        // Rewrite the recorded version in place: the manifest metadata is plain text in
        // the data section, so a targeted patch is enough to build the fixture.
        let mut bytes = std::fs::read(&path).expect("the file is readable");
        patch_version(&mut bytes, major, minor);
        let path = dir.join(format!("{tag}-patched.mcap"));
        std::fs::write(&path, &bytes).expect("the patched file is written");

        match Reader::open(&path) {
            Ok(mut reader) => {
                assert!(expect_ok, "{tag}: a future major should have been refused");
                assert_eq!(
                    reader
                        .manifest()
                        .get("vwp_version_minor")
                        .map(String::as_str),
                    Some(minor.to_string().as_str())
                );
                // A higher minor is read, losing only what this build does not know.
                assert!(reader.replay().expect("it replays").len() > 1);
            }
            Err(e) => {
                assert!(!expect_ok, "{tag}: should have been accepted, got {e}");
                assert!(
                    matches!(
                        e,
                        RecordError::UnsupportedVersion {
                            found: 2,
                            supported: 1
                        }
                    ),
                    "{tag}: expected UnsupportedVersion, got {e}"
                );
            }
        }
    }
}

/// Replaces the one-character version values in the `v2xw.manifest` metadata record.
///
/// The record stores them as decimal strings of known length, so the patch is a literal
/// byte replacement and does not disturb any offset.
fn patch_version(bytes: &mut [u8], major: u16, minor: u16) {
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
    for (key, value) in [
        (b"vwp_version_major".as_slice(), major),
        (b"vwp_version_minor".as_slice(), minor),
    ] {
        let at = find(bytes, key).expect("the key is in the metadata record");
        // key, then a u32 length and one byte of value.
        let value_at = at + key.len() + 4;
        assert_eq!(
            u32::from_le_bytes(
                bytes[at + key.len()..at + key.len() + 4]
                    .try_into()
                    .expect("four bytes")
            ),
            1,
            "the version value is a single digit in the fixture"
        );
        bytes[value_at] = b'0' + value as u8;
    }
}

/// F6 and N1: reserved header bytes are **ignored on read**, not refused, and a v1.1
/// frame that uses them survives a full write/read round trip.
///
/// §0: "Reserved bytes MUST be written as zero by the sender and MUST be ignored by the
/// reader." §8.5 makes that load-bearing — a minor version adds a field "in bytes that a
/// v1 reader is already required to ignore: a `reserved` field" — so refusing a non-zero
/// one makes every v1.1 frame unreadable rather than merely degraded. The existing N1 test
/// only checked that such a frame *decodes*; this one puts it through the recorder and the
/// reader, which is where N1 failed.
#[test]
fn a_frame_with_the_reserved_word_set_and_a_longer_body_round_trips() {
    let dir = scratch_dir("conformance-reserved").expect("a scratch directory");

    // A well-formed v1 Telemetry frame, then made to look like v1.1 two ways at once: the
    // header's reserved word carries a field this build does not know, and the body has a
    // new trailing section after the records (§8.4's "new trailing sections").
    let body = TelemetryBody::new(
        1_000_000_000,
        1_000_000_000,
        vec![NodeTelemetry::unknown(1)],
    );
    let frame = body.to_frame(0, 0).expect("a frame");
    let mut bytes = frame.as_bytes().to_vec();
    v2xw_record::wire::put_u16(&mut bytes, 14, 0x0001);
    let tail = b"a v1.1 section a v1 reader never looks at".to_vec();
    bytes.extend_from_slice(&tail);
    let new_body_len = (bytes.len() - 24) as u32;
    bytes[8..12].copy_from_slice(&new_body_len.to_le_bytes());

    let v11 = Frame::from_bytes(bytes).expect("a v1 reader accepts it (F6)");
    let header = v11.header().expect("a header");
    assert_eq!(
        header.reserved, 1,
        "the reserved word is read, not rejected"
    );
    assert_eq!(header.body_len, new_body_len);
    // …and the part this build does know still decodes, losing only what is new (N1).
    let back = TelemetryBody::decode(v11.body()).expect("the v1 part still decodes");
    assert_eq!(back.records.len(), 1);
    assert_eq!(back.records[0].node_id, 1);

    // End to end: the recorder stores it and the reader replays it byte for byte.
    let path = dir.join("v11.mcap");
    let mut writer = RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
    writer
        .write_manifest(r#"{"schema":"v2xw/manifest/1"}"#)
        .expect("a manifest");
    writer.write_frame(&v11).expect("the v1.1 frame stores");
    writer.finish().expect("the recording finishes");

    let mut reader = Reader::open(&path).expect("the recording opens");
    let replayed = reader.replay().expect("it replays");
    assert_eq!(replayed.len(), 1);
    assert_eq!(
        replayed[0].frame.as_bytes(),
        v11.as_bytes(),
        "a v1.1 frame must come back exactly as it was stored"
    );
    assert_eq!(replayed[0].frame.header().expect("a header").reserved, 1);
    reader.verify().expect("the recording verifies");
}

/// F2, F6 and N1: a `msg_type` this build does not know is recorded and replayed, not
/// refused.
///
/// §8.4: "A new message type id in the reserved range. Readers ignore unknown `msg_type`."
/// §8.6: the replay reader "accepts a higher minor, ignoring what it does not know". The
/// recorder used to refuse one outright and `verify` used to call it an inconsistency, so
/// a v1.1 stream was unrecordable by a v1 build — N1 failed end to end while the test that
/// claimed it passed, because it only decoded a frame and never recorded one.
#[test]
fn a_message_type_this_build_does_not_know_is_recorded_rather_than_refused() {
    let dir = scratch_dir("conformance-unknown-type").expect("a scratch directory");
    let path = dir.join("v11.mcap");
    let shape = RunShape::new(3, 12);
    let frames = v2xw_record::fixture::live_frames(&shape).expect("live frames");

    // A v1.1 canonical frame: message type 0x0009, in the range §2.4 reserves, with a body
    // this build has no layout for. It is spliced in after the first delta and takes that
    // delta's `seq`, with everything after it renumbered — which is what a v1.1 producer's
    // stream looks like to a v1 reader.
    let mut odd = frames[2].as_bytes().to_vec();
    v2xw_record::wire::put_u16(&mut odd, 6, 0x0009);
    let odd = Frame::from_bytes(odd).expect("the frame is well formed");
    assert!(
        odd.header().expect("a header").kind().is_none(),
        "0x0009 must be unknown to this build, or the test proves nothing"
    );

    let mut writer = RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
    writer
        .write_manifest(r#"{"schema":"v2xw/manifest/1"}"#)
        .expect("a manifest");
    let mut seq = 0u64;
    for (i, frame) in frames.iter().enumerate() {
        let renumbered = frame.renumbered(seq).expect("a frame");
        writer.write_frame(&renumbered).expect("the frame stores");
        if frame
            .header()
            .expect("a header")
            .kind()
            .is_some_and(MsgType::is_canonical)
        {
            seq += 1;
        }
        if i == 2 {
            // The v1.1 frame consumes a seq, as a canonical frame does.
            writer
                .write_frame(&odd.renumbered(seq).expect("a frame"))
                .expect("an unknown message type is recorded, not refused");
            seq += 1;
        }
    }
    writer.finish().expect("the recording finishes");

    let mut reader = Reader::open(&path).expect("the recording opens");
    let report = reader.verify().expect("the recording still verifies");
    assert_eq!(
        report.unknown_frames, 1,
        "verify must count what it could not read rather than fail on it"
    );
    let replayed = reader.replay().expect("it replays");
    let stored: Vec<&v2xw_record::RecordedFrame> = replayed
        .iter()
        .filter(|f| f.topic == "vwp/unknown.9")
        .collect();
    assert_eq!(
        stored.len(),
        1,
        "the frame is on its own `vwp/unknown.<id>` topic"
    );
    assert_eq!(
        stored[0].frame.body(),
        odd.body(),
        "its body is stored verbatim, because this build cannot re-encode it"
    );
    // The channel is self-describing about what it is, so a later build can find it.
    let channel = reader
        .channels()
        .find(|c| c.topic == "vwp/unknown.9")
        .expect("the channel is declared");
    assert_eq!(
        channel.metadata.get("vwp_msg_type").map(String::as_str),
        Some("0x0009")
    );
}

/// C5: "Every `prov_id` referenced by a `MetricSample` … has been delivered in a
/// `Provenance` frame before it is first referenced."
///
/// Neither implemented nor satisfied before: `verify` did not check it, and the crate's own
/// fixture — the one the conformance kit and `v2xw-server`'s replay tests are told to reuse
/// — emitted metric samples carrying `prov_id` 1 and 2 without ever writing a `Provenance`
/// frame, and `verify` returned `Ok`. It modelled a non-conformant producer.
#[test]
fn every_referenced_provenance_id_was_delivered_before_it_was_referenced() {
    let dir = scratch_dir("conformance-c5").expect("a scratch directory");
    let path = dir.join("c5.mcap");
    let shape = RunShape::new(4, 30);
    v2xw_record::fixture::write_recording(&path, &shape).expect("the recording is written");

    let mut reader = Reader::open(&path).expect("the recording opens");
    let report = reader.verify().expect("the fixture is C5-conformant");
    assert!(
        report.provenance_references > 0,
        "the fixture must reference a prov_id, or the check is vacuous"
    );
    assert_eq!(
        report.provenance_ids, 2,
        "both of the fixture's prov_ids are delivered"
    );

    // The check has teeth: the same stream with the `Provenance` frame withheld is refused,
    // naming C5.
    let path = dir.join("c5-broken.mcap");
    let frames = v2xw_record::fixture::live_frames(&shape).expect("live frames");
    let mut writer = RecordingWriter::create(&path, RecordingOptions::default()).expect("a writer");
    writer
        .write_manifest(r#"{"schema":"v2xw/manifest/1"}"#)
        .expect("a manifest");
    let mut seq = 0u64;
    let mut dropped = 0;
    for frame in &frames {
        let h = frame.header().expect("a header");
        if h.kind() == Some(MsgType::Provenance) {
            dropped += 1;
            continue;
        }
        writer
            .write_frame(&frame.renumbered(seq).expect("a frame"))
            .expect("the frame stores");
        if h.kind().is_some_and(MsgType::is_canonical) {
            seq += 1;
        }
    }
    writer.finish().expect("the recording finishes");
    assert_eq!(dropped, 1, "the test must actually remove the delivery");

    let mut reader = Reader::open(&path).expect("the container is fine");
    let err = reader
        .verify()
        .expect_err("a metric sample references a prov_id nothing delivered");
    match err {
        RecordError::Inconsistent { detail, .. } => {
            assert!(
                detail.contains("C5"),
                "the error should name the item: {detail}"
            );
            assert!(detail.contains("prov_id"), "{detail}");
        }
        other => panic!("expected Inconsistent, got {other}"),
    }
}

/// The recorder refuses the frames that are not part of a recording at all (§7.1, §0.1).
#[test]
fn connection_frames_and_world_chunks_are_not_recorded() {
    let dir = scratch_dir("conformance-refuse").expect("a scratch directory");
    let mut writer =
        RecordingWriter::create(dir.join("r.mcap"), RecordingOptions::default()).expect("a writer");
    for kind in [MsgType::Error, MsgType::Bye, MsgType::WorldChunk] {
        let mut body = vec![0u8; 64];
        put_u32(&mut body, 0, 0);
        let frame = Frame::new(kind, 0, 0, &body).expect("a frame");
        let err = writer
            .write_frame(&frame)
            .expect_err(&format!("{kind:?} has no place in a recording"));
        assert!(
            matches!(err, RecordError::Malformed { .. }),
            "{kind:?}: expected Malformed, got {err}"
        );
    }
}
