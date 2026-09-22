//! Replay mode (§7), and the property the whole design turns on: the bytes a client sees
//! from a recording are the bytes it would have seen live.
//!
//! §7.2's guarantee is that for every canonical frame, the 24-byte header with
//! `flags &= CANONICAL_FLAG_MASK` and the entire body are byte-identical whether the frame
//! came from the live engine or from the replay reader. `v2xw-record` proves that for the
//! file; these tests prove this crate does not break it on the way to the socket.

use std::sync::Arc;

use v2xw_record::fixture::{self, RunShape};
use v2xw_record::profile::Profile;
use v2xw_record::wire::hello::{HELLO_LIVE, HELLO_REPLAY, HelloBody};
use v2xw_record::wire::{CANONICAL_FLAG_MASK, MsgType};
use v2xw_server::engine::{Engine, RunState};
use v2xw_server::session::{ConnectParams, Session};
use v2xw_server::{ReplayEngine, ServerOptions};

fn world() -> Arc<v2xw_world::WorldPayload> {
    let params = v2xw_world::procedural::GridParams {
        cols: 3,
        rows: 3,
        ..v2xw_world::procedural::GridParams::legacy()
    };
    let world =
        v2xw_world::procedural::grid(&params, &v2xw_world::ImportOptions::default()).expect("grid");
    Arc::new(v2xw_world::serde_vwp::write(&world).expect("payload"))
}

fn recording(tag: &str, shape: &RunShape) -> (std::path::PathBuf, Vec<v2xw_record::Frame>) {
    let dir = fixture::scratch_dir(tag).expect("scratch dir");
    let path = dir.join(format!("{tag}.mcap"));
    let (frames, _) = fixture::write_recording(&path, shape).expect("write");
    (path, frames)
}

#[test]
fn a_replayed_stream_is_byte_identical_to_the_live_one() {
    let shape = RunShape::new(6, 30);
    let (path, live) = recording("replay-identity", &shape);
    let mut engine = ReplayEngine::open(&path, world()).expect("open");
    let d = engine.descriptor().clone();
    assert!(!d.live, "a recording produces HELLO_REPLAY, not HELLO_LIVE");

    let mut session = Session::new(ConnectParams::default(), &d);
    session
        .hello_frame(&d, RunState::Paused, 0, "")
        .expect("hello");

    let mut replayed = Vec::new();
    while let Some(out) = engine.step().expect("step") {
        for frame in session.encode_step(&out).expect("encode").frames {
            replayed.push(frame);
        }
    }

    let canonical = |frames: &[v2xw_record::Frame]| -> Vec<Vec<u8>> {
        frames
            .iter()
            .filter(|f| {
                f.header()
                    .ok()
                    .and_then(|h| MsgType::from_id(h.msg_type))
                    .is_some_and(MsgType::is_canonical)
            })
            .map(|f| f.canonical().into_bytes())
            .collect()
    };
    let live = canonical(&live);
    let out = canonical(&replayed);
    assert!(!live.is_empty(), "the fixture produced canonical frames");
    assert_eq!(out.len(), live.len(), "the same number of canonical frames");
    for (i, (a, b)) in live.iter().zip(out.iter()).enumerate() {
        assert_eq!(
            a, b,
            "frame {i} differs between the live stream and the replayed one"
        );
    }
}

#[test]
fn the_transport_flag_bits_are_the_only_ones_that_may_differ() {
    // §7.2 item 3. The check is on the mask, not on a particular frame: a future flag
    // added to the wrong mask would be caught here rather than in a client.
    //
    // It runs over a `node`-profile recording as well as a `full` one, because
    // `FLAG_NODE_ONLY` is a *canonical* flag and a `node` stream carries it on every
    // frame. A masked comparison over a stream whose canonical bits are all zero would
    // pass whatever the mask said, and the `full` fixture's frames are exactly that:
    // `v2xw-record`'s fixture never sets `FLAG_END_OF_RUN`, so nothing in a `full`
    // recording exercises the canonical half of the mask.
    assert_eq!(CANONICAL_FLAG_MASK, 0x000C);
    let mut canonical_bits_seen = 0usize;
    let mut transport_bits_seen = 0usize;

    for (tag, shape, profile) in [
        ("replay-flags-full", RunShape::new(4, 22), Profile::Full),
        (
            "replay-flags-node",
            RunShape::new(4, 22).node_only(),
            Profile::NodeOnly,
        ),
    ] {
        let (path, live) = recording(tag, &shape);
        let mut engine = ReplayEngine::open(&path, world()).expect("open");
        let d = engine.descriptor().clone();
        let mut session = Session::new(
            ConnectParams {
                profile,
                ..ConnectParams::default()
            },
            &d,
        );
        session
            .hello_frame(&d, RunState::Paused, 0, "")
            .expect("hello");

        let mut sent = Vec::new();
        while let Some(out) = engine.step().expect("step") {
            sent.extend(session.encode_step(&out).expect("encode").frames);
        }
        let canonical = |frames: &[v2xw_record::Frame]| -> Vec<v2xw_record::Frame> {
            frames
                .iter()
                .filter(|f| {
                    f.header()
                        .ok()
                        .and_then(|h| MsgType::from_id(h.msg_type))
                        .is_some_and(MsgType::is_canonical)
                })
                .cloned()
                .collect()
        };
        let live_canonical = canonical(&live);
        let sent_canonical = canonical(&sent);
        assert_eq!(
            sent_canonical.len(),
            live_canonical.len(),
            "{tag}: frame counts"
        );
        assert!(!sent_canonical.is_empty(), "{tag}: some frames");
        for (a, b) in live_canonical.iter().zip(sent_canonical.iter()) {
            let ha = a.header().expect("header");
            let hb = b.header().expect("header");
            assert_eq!(
                ha.flags & CANONICAL_FLAG_MASK,
                hb.flags & CANONICAL_FLAG_MASK,
                "{tag}: a canonical flag bit changed in the transport"
            );
            if hb.flags & CANONICAL_FLAG_MASK != 0 {
                canonical_bits_seen += 1;
            }
            if hb.flags & v2xw_record::wire::TRANSPORT_FLAG_MASK != 0 {
                transport_bits_seen += 1;
            }
        }
    }
    assert!(
        canonical_bits_seen > 0,
        "no frame in either stream carried a canonical flag, so the masked comparison \
         proved nothing"
    );
    assert!(
        transport_bits_seen > 0,
        "no frame carried a transport flag either (the opening keyframe should carry \
         RESYNC)"
    );
}

#[test]
fn a_full_recording_replayed_as_node_withholds_the_ground_truth_channels() {
    // Conformance V5's direction: a `full` recording can be replayed under `profile=node`,
    // and the reader blanks on the way out. The stripper is `v2xw-record`'s, so this test
    // is about the transport handing it every frame — a path that would otherwise forward
    // ground truth to a blind evaluation.
    let shape = RunShape::new(6, 25);
    let (path, _) = recording("replay-node", &shape);
    let mut engine = ReplayEngine::open(&path, world()).expect("open");
    let d = engine.descriptor().clone();
    let params = ConnectParams {
        profile: Profile::NodeOnly,
        ..ConnectParams::default()
    };
    let mut session = Session::new(params, &d);
    let hello = session
        .hello_frame(&d, RunState::Paused, 0, "")
        .expect("hello");
    let body = HelloBody::decode(hello.body()).expect("decode");
    assert_eq!(body.hello_flags & HELLO_REPLAY, HELLO_REPLAY);
    assert_eq!(body.hello_flags & HELLO_LIVE, 0);

    let mut keyframes = 0;
    let mut seqs = Vec::new();
    while let Some(out) = engine.step().expect("step") {
        for frame in session.encode_step(&out).expect("encode").frames {
            let header = frame.header().expect("header");
            if !MsgType::from_id(header.msg_type).is_some_and(MsgType::is_canonical) {
                continue;
            }
            assert_eq!(
                header.flags & v2xw_record::wire::FLAG_NODE_ONLY,
                v2xw_record::wire::FLAG_NODE_ONLY,
                "§5.3: every canonical frame of a node stream is marked"
            );
            seqs.push(header.seq);
            if header.msg_type == MsgType::Keyframe.id() {
                let kf = v2xw_record::wire::snapshot::KeyframeBody::decode(frame.body())
                    .expect("decode keyframe");
                assert_eq!(kf.profile, 1);
                for row in &kf.actors {
                    if row.actor_id == v2xw_record::wire::U32_NONE {
                        continue;
                    }
                    assert_eq!(row.lane_id, v2xw_record::wire::U32_NONE, "lane_id blanked");
                    assert_eq!(row.accel_cq, 0, "accel_cq blanked");
                    assert_eq!(
                        row.state & v2xw_record::wire::snapshot::ST_ATTACKER,
                        0,
                        "ST_ATTACKER cleared"
                    );
                }
                keyframes += 1;
            }
        }
    }
    assert!(keyframes > 1, "several keyframes were replayed");
    for (i, seq) in seqs.iter().enumerate() {
        assert_eq!(*seq, i as u64, "the stripped stream is renumbered densely");
    }
}

#[test]
fn a_seek_into_a_recording_lands_on_a_keyframe_at_or_before_the_target() {
    // §7.3 step 6 and conformance P4.
    let shape = RunShape::new(5, 40);
    let (path, _) = recording("replay-seek", &shape);
    let mut engine = ReplayEngine::open(&path, world()).expect("open");
    let (min_ns, max_ns) = engine.seek_range();
    assert!(max_ns > min_ns, "the recording spans some time");
    let target = min_ns + (max_ns - min_ns) / 2;
    let outputs = engine.seek(target).expect("seek");
    assert!(!outputs.is_empty());
    assert!(
        outputs[0].sim_time <= target,
        "the first frame is at or before the target"
    );
    assert!(
        outputs.last().expect("last").sim_time <= target,
        "nothing past the target"
    );
    let opens_with_keyframe = outputs[0].recorded.iter().any(|f| {
        f.header()
            .is_ok_and(|h| h.msg_type == MsgType::Keyframe.id())
    });
    assert!(
        opens_with_keyframe,
        "§7.3 step 6: a keyframe opens the seek"
    );
    let max = engine.descriptor().cadence.max_deltas_per_gop() as usize + 1;
    assert!(
        outputs.len() <= max,
        "{} steps exceeds {max}",
        outputs.len()
    );

    assert_eq!(
        engine
            .seek(max_ns + 1_000_000_000)
            .expect_err("past the end")
            .code(),
        -32003
    );
}

#[test]
fn a_recording_with_no_hello_is_refused_rather_than_served_empty() {
    let engine = ReplayEngine::from_frames(Vec::new(), world(), "empty");
    assert_eq!(
        engine.expect_err("no Hello").code(),
        -32005,
        "a stream with no Hello has no run to serve"
    );
}

#[tokio::test]
async fn a_replay_server_serves_the_world_and_the_control_surface() {
    let shape = RunShape::new(4, 20);
    let (path, _) = recording("replay-serve", &shape);
    let payload = world();
    let json = {
        let params = v2xw_world::procedural::GridParams {
            cols: 3,
            rows: 3,
            ..v2xw_world::procedural::GridParams::legacy()
        };
        let w = v2xw_world::procedural::grid(&params, &v2xw_world::ImportOptions::default())
            .expect("grid");
        v2xw_world::serde_vwp::to_json_string(&w).expect("json")
    };
    let options = ServerOptions {
        bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        ..ServerOptions::default()
    };
    let server = v2xw_server::serve_replay(options, &path, Arc::clone(&payload), json)
        .await
        .expect("serve");
    let base = server.http_url();

    let health = reqwest_get(&format!("{base}/healthz")).await;
    assert!(health.contains("\"ok\":true"), "healthz said {health}");
    let schema = reqwest_get(&format!("{base}/rpc/schema")).await;
    assert!(schema.contains("\"openrpc\":\"1.3.2\""));
    let hash = payload.content_hash_hex();
    let world_json = reqwest_get(&format!("{base}/world/{hash}.json")).await;
    assert!(world_json.contains("vwp-world/1"));
    server.stop().await;
}

/// A minimal HTTP GET, so the test needs no HTTP client dependency.
async fn reqwest_get(url: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let parsed = url.strip_prefix("http://").expect("http url");
    let (authority, path) = parsed.split_once('/').expect("a path");
    let mut socket = tokio::net::TcpStream::connect(authority)
        .await
        .expect("connect");
    socket
        .write_all(
            format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("write");
    let mut body = Vec::new();
    socket.read_to_end(&mut body).await.expect("read");
    String::from_utf8_lossy(&body).to_string()
}
