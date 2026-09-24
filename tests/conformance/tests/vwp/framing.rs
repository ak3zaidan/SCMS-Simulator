//! §10.1 — framing. Items F1 to F8.
//!
//! F9 ("unknown flag bits are ignored") binds the client and is owned by
//! `ui/packages/protocol/test/framing.test.ts`.

use v2xw_record::RecordError;
use v2xw_record::fixture::{RunShape, live_frames};
use v2xw_record::wire::hello::NodeRow;
use v2xw_record::wire::snapshot::{
    ABS_STRIDE, ACTOR_STRIDE, DELTA_PREFIX_BYTES, DESPAWN_STRIDE, DeltaBody, KEYFRAME_PREFIX_BYTES,
    KeyframeBody, LANE_STRIDE, MOVED_STRIDE, SIGNAL_STRIDE, SPAWN_STRIDE, is_aligned,
};
use v2xw_record::wire::{
    FLAG_COMPRESSED, Frame, HEADER_BYTES, MAGIC, MsgType, TRANSPORT_FLAG_MASK, VERSION_MAJOR,
    get_u32, put_u16, put_u32, put_u64,
};

/// A short, busy run: a teleport for the absolute escape, a parked attacker for the frames
/// whose existence depends on a ground-truth field, and enough steps for several GOPs.
fn shape() -> RunShape {
    RunShape {
        actors: 6,
        steps: 30,
        teleport_at: Some(17),
        parked_attacker: true,
        ..RunShape::default()
    }
}

fn frames() -> Vec<Frame> {
    live_frames(&shape()).expect("the fixture encodes")
}

/// Bytes that are a syntactically valid frame, built by hand so a field can be corrupted.
fn hand_built(msg_type: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0u8; HEADER_BYTES + body.len()];
    put_u32(&mut bytes, 0, MAGIC);
    put_u16(&mut bytes, 4, VERSION_MAJOR);
    put_u16(&mut bytes, 6, msg_type);
    put_u32(
        &mut bytes,
        8,
        u32::try_from(body.len()).expect("small body"),
    );
    put_u16(&mut bytes, 12, 0);
    put_u16(&mut bytes, 14, 0);
    put_u64(&mut bytes, 16, 7);
    bytes[HEADER_BYTES..].copy_from_slice(body);
    bytes
}

/// **F1** — "Rejects a frame whose `magic ≠ 0x31505756` (close 1002)."
///
/// The negative control is the same bytes with the magic intact: without it, a decoder that
/// rejected everything would pass.
#[test]
fn f1_a_frame_whose_magic_is_wrong_is_refused() {
    let good = hand_built(MsgType::Keyframe.id(), &[0u8; 64]);
    Frame::from_bytes(good.clone()).expect("the control frame is accepted");

    let mut bad = good;
    put_u32(&mut bad, 0, 0xDEAD_BEEF);
    match Frame::from_bytes(bad) {
        Err(RecordError::BadMagic { found, expected }) => {
            assert_eq!(found, 0xDEAD_BEEF);
            assert_eq!(expected, MAGIC);
        }
        other => panic!("a wrong magic must be BadMagic, got {other:?}"),
    }
}

/// **F2** — "Ignores a frame with an unknown `msg_type` and keeps the connection."
///
/// On this side the property is that an unknown type decodes as a frame and reports itself
/// as unknown, rather than being an error: a reader that refused it would drop a v1.1
/// stream entirely instead of losing only what is new (§8.4, item N1).
#[test]
fn f2_an_unknown_message_type_is_ignored_rather_than_an_error() {
    assert_eq!(MsgType::from_id(0x0002), Some(MsgType::Keyframe));
    assert_eq!(MsgType::from_id(0x00A7), None, "0x00A7 is not a v1 type");

    let frame = Frame::from_bytes(hand_built(0x00A7, &[1u8; 32]))
        .expect("an unknown type is still a frame");
    let header = frame.header().expect("a header");
    assert_eq!(header.msg_type, 0x00A7);
    assert_eq!(header.kind(), None, "this build must not claim to know it");
    assert_eq!(header.body_len as usize, frame.body().len());
    assert!(
        !MsgType::from_id(0x00A7).is_some_and(MsgType::is_canonical),
        "an unknown type cannot be treated as canonical"
    );
}

/// **F3** — "`body_len` always equals the uncompressed body length, compressed or not."
#[test]
fn f3_body_len_is_the_uncompressed_body_length() {
    for frame in frames() {
        let header = frame.header().expect("a header");
        assert_eq!(
            header.body_len as usize,
            frame.body().len(),
            "frame {:#06x} at seq {} disagrees with its own body",
            header.msg_type,
            header.seq
        );
        assert_eq!(
            header.flags & FLAG_COMPRESSED,
            0,
            "a recorded frame is never compressed (§7.1)"
        );
    }

    // The injected fault: claim one byte more than is there.
    let mut lying = hand_built(MsgType::Delta.id(), &[0u8; 16]);
    put_u32(&mut lying, 8, 17);
    assert!(
        matches!(Frame::from_bytes(lying), Err(RecordError::Malformed { .. })),
        "a body_len that disagrees with the bytes present must be refused"
    );
}

/// **F4** — "All integers and floats are little-endian; a big-endian host produces
/// identical bytes."
///
/// Checked against literal bytes rather than against a round trip: a round trip through
/// this crate's own accessors would agree with itself on a big-endian host and prove
/// nothing.
#[test]
fn f4_every_scalar_on_the_wire_is_little_endian() {
    let frame = Frame::new(MsgType::Keyframe, 0x0102_0304_0506_0708, 0, &[]).expect("frame");
    let bytes = frame.as_bytes();
    assert_eq!(
        &bytes[0..4],
        b"VWP1",
        "§2.1: the little-endian bytes of the magic spell V W P 1"
    );
    assert_eq!(&bytes[4..6], &[0x01, 0x00], "version_major = 1, LE");
    assert_eq!(&bytes[6..8], &[0x02, 0x00], "msg_type = 2, LE");
    assert_eq!(
        &bytes[16..24],
        &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01],
        "seq is a little-endian u64"
    );

    let mut buf = [0u8; 8];
    put_u32(&mut buf, 0, 0x1234_5678);
    assert_eq!(&buf[0..4], &[0x78, 0x56, 0x34, 0x12]);
    assert_eq!(
        get_u32(&buf, 0, "probe").expect("reads back"),
        0x1234_5678,
        "the reader is the inverse of the writer"
    );
}

/// **F5** — "Every array offset satisfies §2.2 alignment; a client can build typed-array
/// views in place with no copy for an uncompressed frame."
///
/// Offsets are recomputed from the decoded row counts and the layout strides, so the check
/// is against the specification's arithmetic rather than against whatever the encoder
/// happened to write.
#[test]
fn f5_every_array_offset_satisfies_the_alignment_rule() {
    assert!(
        is_aligned(HEADER_BYTES, 8),
        "the body starts at frame offset 24, which must be 8-aligned for a u64 view"
    );
    for stride in [
        ACTOR_STRIDE,
        SIGNAL_STRIDE,
        MOVED_STRIDE,
        ABS_STRIDE,
        LANE_STRIDE,
        SPAWN_STRIDE,
        DESPAWN_STRIDE,
    ] {
        assert!(is_aligned(stride, 4), "stride {stride} breaks §2.2");
    }

    let mut keyframes = 0usize;
    let mut deltas = 0usize;
    for frame in frames() {
        let body = frame.body();
        assert!(
            is_aligned(body.len(), 4),
            "a body must be a whole number of 4-byte words"
        );
        match frame.header().expect("a header").kind() {
            Some(MsgType::Keyframe) => {
                let kf = KeyframeBody::decode(body).expect("a keyframe decodes");
                let signals_at = KEYFRAME_PREFIX_BYTES + ACTOR_STRIDE * kf.actors.len();
                assert!(is_aligned(KEYFRAME_PREFIX_BYTES, 4));
                assert!(is_aligned(signals_at, 4), "signal block at {signals_at}");
                keyframes += 1;
            }
            Some(MsgType::Delta) => {
                let d = DeltaBody::decode(body).expect("a delta decodes");
                let mut at = DELTA_PREFIX_BYTES;
                for (count, stride) in [
                    (d.moved.len(), MOVED_STRIDE),
                    (d.abs.len(), ABS_STRIDE),
                    (d.lanes.len(), LANE_STRIDE),
                    (d.spawns.len(), SPAWN_STRIDE),
                    (d.despawns.len(), DESPAWN_STRIDE),
                    (d.signals.len(), SIGNAL_STRIDE),
                ] {
                    assert!(
                        is_aligned(at, 4),
                        "a block starts at {at}, which is not 4-aligned"
                    );
                    at += stride * count;
                }
                assert!(at <= body.len(), "the blocks run past the body");
                deltas += 1;
            }
            _ => {}
        }
    }
    assert!(
        keyframes > 0 && deltas > 0,
        "the fixture produced {keyframes} keyframes and {deltas} deltas, so the scan \
         checked nothing"
    );
}

/// **F6** — "Reserved header and prefix bytes are written as zero and ignored on read."
///
/// Both halves: this build writes zero, and a frame whose reserved word is set still
/// decodes, because §8.5 makes that the place a minor version adds a field.
#[test]
fn f6_reserved_bytes_are_written_zero_and_ignored_on_read() {
    for frame in frames() {
        assert_eq!(
            frame.header().expect("a header").reserved,
            0,
            "this build must write the reserved word as zero"
        );
    }

    let mut bytes = hand_built(MsgType::Delta.id(), &[0u8; 32]);
    put_u16(&mut bytes, 14, 0xBEEF);
    let frame = Frame::from_bytes(bytes).expect("a non-zero reserved word is not an error");
    assert_eq!(
        frame.header().expect("a header").reserved,
        0xBEEF,
        "the value is surfaced for a reader that knows what it means"
    );
}

/// **F7** — "`Hello` is never compressed, even when longer than 4 KiB."
///
/// The fixture's `Hello` caps its node table at four rows, so it is far below the §2.6
/// threshold and an assertion over it would be about a body the rule does not reach. The
/// table is grown here until the body is over 4 KiB, which is the case F7 is about.
#[test]
fn f7_hello_is_never_compressed() {
    let mut body = v2xw_record::fixture::hello(&RunShape::default());
    let before = body.encoded_len();
    for i in 0..400u32 {
        let label = body.strings.intern(&format!("veh_{i:06}"));
        let profile = body.strings.intern("obu/cohda-mk5");
        body.nodes.push(NodeRow {
            node_id: 1_000 + i,
            actor_id: 1_000 + i,
            pos_m: [0.0, 0.0, 0.0],
            str_label: label,
            str_profile_id: profile,
            flags: 0,
            kind: 0,
            class_idx: 0,
        });
    }
    assert!(
        body.encoded_len() > before,
        "the node table did not grow, so the fixture is not honest"
    );

    let frame = body.to_frame(0, 0).expect("the hello encodes");
    assert!(
        frame.body().len() > 4096,
        "the hello is {} bytes, which is below the §2.6 threshold this item is about",
        frame.body().len()
    );
    assert_eq!(
        frame.header().expect("a header").flags & FLAG_COMPRESSED,
        0,
        "§10.1 F7: Hello is never compressed"
    );
    assert_eq!(
        frame.header().expect("a header").kind(),
        Some(MsgType::Hello),
        "the frame under test is a Hello"
    );
    assert!(
        !MsgType::Hello.is_canonical(),
        "§2.4: Hello is a connection frame and consumes no seq"
    );
}

/// **F8** — "zstd round-trips every frame type; `compress=none` is honoured."
///
/// **This build does not satisfy F8 and this test pins that.** Compression is not
/// implemented: `v2xw_server::session::Compression` documents the decision, every body is
/// sent uncompressed and `FLAG_COMPRESSED` is never set, so `compress=none` is honoured
/// trivially and `compress=zstd` is accepted and then not applied.
///
/// The assertions below are a tripwire, not a pass. The day compression lands, the
/// `FLAG_COMPRESSED` assertion goes red and whoever turned it on has to replace this test
/// with the round trip F8 actually asks for — which is the point of writing it down rather
/// than leaving the item unticked and unexplained.
#[test]
fn f8_compression_is_not_applied_on_the_wire_in_this_build() {
    use v2xw_server::session::{Compression, ConnectParams};

    assert_eq!(
        ConnectParams::parse("compress=none")
            .expect("compress=none parses")
            .compress,
        Compression::None,
        "a client that cannot decompress must be able to say so"
    );
    assert_eq!(
        ConnectParams::parse("")
            .expect("the default parses")
            .compress,
        Compression::Zstd,
        "§1.1: compress defaults to zstd"
    );
    assert!(
        ConnectParams::parse("compress=brotli").is_err(),
        "an unknown compression must not silently become the default"
    );

    for frame in frames() {
        assert_eq!(
            frame.header().expect("a header").flags & FLAG_COMPRESSED,
            0,
            "no frame this build produces is compressed; when one is, F8 needs a real \
             round-trip test and this tripwire must be replaced"
        );
    }
    assert_eq!(
        FLAG_COMPRESSED & TRANSPORT_FLAG_MASK,
        FLAG_COMPRESSED,
        "FLAG_COMPRESSED is a transport bit, so compression can never change a recorded \
         frame's canonical bytes (§7.2)"
    );
}
