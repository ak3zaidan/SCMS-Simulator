//! The connection state machine of §1.3, §1.4 and §5.3, driven without a socket.

use v2xw_record::profile::Profile;
use v2xw_record::wire::hello::{
    HELLO_LIVE, HELLO_NODE_ONLY, HELLO_PAUSED, HELLO_RESUMED, HelloBody,
};
use v2xw_record::wire::{FLAG_NODE_ONLY, FLAG_RESYNC, MsgType};
use v2xw_server::engine::{Engine, RunState};
use v2xw_server::session::{Compression, ConnectParams, OVERLAYS, Session, overlay_is_gt};
use v2xw_server::{StubEngine, StubOptions};

fn engine() -> StubEngine {
    StubEngine::new(StubOptions {
        actors: 24,
        grid: 4,
        duration_s: 20,
        ..StubOptions::default()
    })
    .expect("the fixture builds")
}

#[test]
fn the_query_string_of_section_1_1_parses_and_defaults() {
    let default = ConnectParams::parse("").expect("empty");
    assert_eq!(
        default.profile,
        Profile::Full,
        "§1.1: profile defaults to full"
    );
    assert_eq!(
        default.compress,
        Compression::Zstd,
        "§1.1: compress defaults to zstd"
    );
    assert_eq!(default.version, 1);
    assert!(default.run.is_none());
    assert!(default.resume.is_none());

    let p = ConnectParams::parse("run=latest&resume=42&profile=node&compress=none&v=1")
        .expect("full query");
    assert!(
        p.run.is_none(),
        "`latest` means the current run, not a named one"
    );
    assert_eq!(p.resume, Some(42));
    assert_eq!(p.profile, Profile::NodeOnly);
    assert_eq!(p.compress, Compression::None);

    // An unknown parameter is ignored (§8.4's additive rule), a malformed known one is not.
    assert!(ConnectParams::parse("future=1").is_ok());
    assert_eq!(
        ConnectParams::parse("profile=gods-eye")
            .expect_err("bad profile")
            .code(),
        -32602,
        "a profile the server does not know must never silently become `full`"
    );
    assert_eq!(
        ConnectParams::parse("compress=brotli")
            .expect_err("bad compress")
            .code(),
        -32602
    );
    assert_eq!(
        ConnectParams::parse("resume=soon")
            .expect_err("bad resume")
            .code(),
        -32602
    );
}

#[test]
fn the_handshake_sets_the_connection_scoped_flags() {
    let engine = engine();
    let d = engine.descriptor().clone();
    let mut session = Session::new(ConnectParams::default(), &d);
    let frame = session
        .hello_frame(&d, RunState::Running, 0, "")
        .expect("hello");
    let header = frame.header().expect("header");
    assert_eq!(header.msg_type, MsgType::Hello.id());
    assert_eq!(header.flags, 0, "§2.6: Hello is never compressed");
    assert_eq!(header.seq, 0, "§2.4: Hello carries the next canonical seq");

    let body = HelloBody::decode(frame.body()).expect("decode");
    assert_eq!(body.hello_flags & HELLO_LIVE, HELLO_LIVE);
    assert_eq!(body.hello_flags & HELLO_NODE_ONLY, 0);
    assert_eq!(body.hello_flags & HELLO_PAUSED, 0);
    assert_eq!(body.hello_flags & HELLO_RESUMED, 0);
    assert_eq!(body.resume_seq, 0);
    assert!(session.greeted());
    assert!(!session.resumed());
}

#[test]
fn a_paused_run_sets_hello_paused() {
    let engine = engine();
    let d = engine.descriptor().clone();
    let mut session = Session::new(ConnectParams::default(), &d);
    let frame = session
        .hello_frame(&d, RunState::Paused, 0, "")
        .expect("hello");
    let body = HelloBody::decode(frame.body()).expect("decode");
    assert_eq!(body.hello_flags & HELLO_PAUSED, HELLO_PAUSED);
}

#[test]
fn a_resume_the_ring_cannot_serve_falls_back_rather_than_failing() {
    // §1.4 rule 2, and conformance H6: "never an error".
    let engine = engine();
    let d = engine.descriptor().clone();
    let params = ConnectParams {
        resume: Some(9_999_999),
        ..ConnectParams::default()
    };
    let mut session = Session::new(params, &d);
    let frame = session
        .hello_frame(&d, RunState::Running, 0, "")
        .expect("hello");
    let body = HelloBody::decode(frame.body()).expect("decode");
    assert_eq!(body.hello_flags & HELLO_RESUMED, 0);
    assert_eq!(
        body.resume_seq, 0,
        "resume_seq is the next seq to be emitted"
    );
    assert!(session.resume_backlog().is_empty());
    assert!(!session.resumed());
}

#[test]
fn the_first_canonical_frame_after_a_fresh_hello_is_a_resync_keyframe() {
    // Conformance H2. The engine is stepped several times *before* the handshake so the
    // cadence would not have produced a keyframe on its own: without the forced resync the
    // connection would open mid-GOP with nothing to apply deltas against.
    let mut engine = engine();
    let d = engine.descriptor().clone();
    for _ in 0..4 {
        engine.step().expect("step");
    }
    let mut session = Session::new(ConnectParams::default(), &d);
    session
        .hello_frame(&d, RunState::Running, engine.sim_time(), "")
        .expect("hello");
    let out = engine.step().expect("step").expect("more steps");
    let effects = session.encode_step(&out).expect("encode");
    let first = effects.frames.first().expect("a frame");
    let header = first.header().expect("header");
    assert_eq!(header.msg_type, MsgType::Keyframe.id());
    assert_eq!(header.flags & FLAG_RESYNC, FLAG_RESYNC);
    assert_eq!(header.seq, 0, "the stream starts at seq 0");
}

/// A step older than one the connection has already encoded — the greeting's snapshot of a
/// kernel that had run ahead of the step channel — is skipped, not refused. Refusing it
/// closed the socket with 1011, and every reconnect raced the same way: Manhattan with 230
/// pedestrians and cyclists never streamed in the page.
#[test]
fn a_step_the_greeting_already_covered_is_skipped_not_fatal() {
    let mut engine = engine();
    let d = engine.descriptor().clone();
    let mut session = Session::new(ConnectParams::default(), &d);
    session
        .hello_frame(&d, RunState::Running, 0, "")
        .expect("hello");
    let first = engine.step().expect("step").expect("a step");
    let second = engine.step().expect("step").expect("a step");
    assert!(
        !session
            .encode_step(&second)
            .expect("encode")
            .frames
            .is_empty()
    );
    // The older step arrives after the newer one.
    let late = session
        .encode_step(&first)
        .expect("a late step is not an error");
    assert!(late.frames.is_empty(), "nothing is sent for a covered step");
    // And the stream carries on.
    let third = engine.step().expect("step").expect("a step");
    assert!(
        !session
            .encode_step(&third)
            .expect("encode")
            .frames
            .is_empty()
    );
}

#[test]
fn seq_is_dense_and_monotonic_across_every_canonical_frame() {
    // Conformance H4. Telemetry and events are subscribed so that the stream carries more
    // than snapshots; a counter that only the snapshot encoder advanced would leave gaps
    // here, and §1.4 tells the client to read a gap as a drop.
    let mut engine = engine();
    let d = engine.descriptor().clone();
    let mut session = Session::new(ConnectParams::default(), &d);
    session
        .hello_frame(&d, RunState::Running, 0, "")
        .expect("hello");
    let nodes: Vec<u32> = d.hello.nodes.iter().map(|n| n.node_id).take(3).collect();
    session.follow(Some(nodes[0]), false, true, &nodes);
    session
        .set_events(
            &["node.tx".to_string(), "phy.rx".to_string()],
            &[],
            None,
            None,
            None,
            None,
        )
        .expect("subscribe");

    let mut seqs = Vec::new();
    for _ in 0..25 {
        let out = engine.step().expect("step").expect("more steps");
        for frame in session.encode_step(&out).expect("encode").frames {
            let header = frame.header().expect("header");
            if MsgType::from_id(header.msg_type).is_some_and(MsgType::is_canonical) {
                seqs.push(header.seq);
            }
        }
    }
    assert!(seqs.len() > 25, "only {} canonical frames", seqs.len());
    for (i, seq) in seqs.iter().enumerate() {
        assert_eq!(*seq, i as u64, "seq must be dense: index {i} carried {seq}");
    }
    assert_eq!(session.next_seq(), seqs.len() as u64);
}

#[test]
fn the_node_profile_marks_every_canonical_frame_and_strips_the_gt_channels() {
    // §5.3 and conformance V1.
    let mut engine = engine();
    let d = engine.descriptor().clone();
    let params = ConnectParams {
        profile: Profile::NodeOnly,
        ..ConnectParams::default()
    };
    let mut session = Session::new(params, &d);
    let hello = session
        .hello_frame(&d, RunState::Running, 0, "")
        .expect("hello");
    let body = HelloBody::decode(hello.body()).expect("decode");
    assert_eq!(body.hello_flags & HELLO_NODE_ONLY, HELLO_NODE_ONLY);
    for row in &body.channels {
        let spec = v2xw_record::channels::by_wire_id(row.channel_id).expect("known channel");
        assert!(
            !spec.is_ground_truth_channel(),
            "§5.2: `{}` must be absent from the table, not merely disabled",
            spec.name
        );
    }
    for row in &body.nodes {
        assert_eq!(
            row.flags & v2xw_record::wire::hello::NODE_IS_ATTACKER,
            0,
            "§5.2: nodes.flags bit 1 must be 0"
        );
    }

    let nodes: Vec<u32> = d.hello.nodes.iter().map(|n| n.node_id).take(2).collect();
    session.follow(Some(nodes[0]), false, true, &nodes);
    for _ in 0..12 {
        let out = engine.step().expect("step").expect("more steps");
        for frame in session.encode_step(&out).expect("encode").frames {
            let header = frame.header().expect("header");
            if MsgType::from_id(header.msg_type).is_some_and(MsgType::is_canonical) {
                assert_eq!(
                    header.flags & FLAG_NODE_ONLY,
                    FLAG_NODE_ONLY,
                    "§5.3: FLAG_NODE_ONLY on every canonical frame"
                );
            }
        }
    }
}

#[test]
fn a_gt_channel_subscription_is_refused_in_the_node_profile() {
    // Conformance V3, at the session level.
    let engine = engine();
    let d = engine.descriptor().clone();
    let params = ConnectParams {
        profile: Profile::NodeOnly,
        ..ConnectParams::default()
    };
    let mut session = Session::new(params, &d);
    let error = session
        .set_events(&["gt.kinematics".to_string()], &[], None, None, None, None)
        .expect_err("a GT channel must be refused");
    assert_eq!(error.code(), -32040);
    assert!(
        session.event_channels().is_empty(),
        "a refused subscription must not partially apply"
    );

    // And the same subscription is fine on a `full` connection, which is what makes the
    // refusal above a property of the profile rather than of the channel table.
    let mut full = Session::new(ConnectParams::default(), &d);
    let ids = full
        .set_events(&["gt.kinematics".to_string()], &[], None, None, None, None)
        .expect("full may subscribe");
    assert_eq!(ids, vec![1]);
}

#[test]
fn a_gt_overlay_is_refused_in_the_node_profile_and_allowed_in_full() {
    let engine = engine();
    let d = engine.descriptor().clone();
    let gt: Vec<&str> = OVERLAYS
        .iter()
        .copied()
        .filter(|n| overlay_is_gt(n))
        .collect();
    assert_eq!(gt.len(), 3, "§6.7 lists three `*_gt` overlays");

    let mut node = Session::new(
        ConnectParams {
            profile: Profile::NodeOnly,
            ..ConnectParams::default()
        },
        &d,
    );
    for name in &gt {
        let wanted = [((*name).to_string(), true)].into_iter().collect();
        assert_eq!(
            node.set_overlays(&wanted, &Default::default())
                .expect_err("GT overlay")
                .code(),
            -32040,
            "enabling `{name}` must be refused"
        );
    }
    // Turning one *off* is not a disclosure, so it is allowed.
    let off = [("attackers_gt".to_string(), false)].into_iter().collect();
    node.set_overlays(&off, &Default::default())
        .expect("disabling a GT overlay is not a disclosure");

    let mut full = Session::new(ConnectParams::default(), &d);
    let wanted = [("attackers_gt".to_string(), true)].into_iter().collect();
    full.set_overlays(&wanted, &Default::default())
        .expect("full may enable it");
    assert_eq!(full.overlays().get("attackers_gt"), Some(&true));
}

#[test]
fn no_event_channel_is_subscribed_until_the_client_asks() {
    // §6.12's decision, and the reason for it: an unasked `phy.rx` swamps any client.
    let mut engine = engine();
    let d = engine.descriptor().clone();
    let mut session = Session::new(ConnectParams::default(), &d);
    session
        .hello_frame(&d, RunState::Running, 0, "")
        .expect("hello");
    assert!(session.event_channels().is_empty());
    let mut events = 0;
    for _ in 0..15 {
        let out = engine.step().expect("step").expect("more steps");
        assert!(!out.events.is_empty(), "the engine did produce events");
        for frame in session.encode_step(&out).expect("encode").frames {
            if frame.header().expect("header").msg_type == MsgType::Event.id() {
                events += 1;
            }
        }
    }
    assert_eq!(events, 0, "nothing subscribed means no Event frames");
}

#[test]
fn telemetry_carries_only_subscribed_nodes() {
    let mut engine = engine();
    let d = engine.descriptor().clone();
    let mut session = Session::new(ConnectParams::default(), &d);
    session
        .hello_frame(&d, RunState::Running, 0, "")
        .expect("hello");
    let node = d.hello.nodes[0].node_id;
    let subscribed = session.follow(Some(node), false, true, &[]);
    assert_eq!(subscribed, vec![node]);

    let mut seen = 0;
    for _ in 0..12 {
        let out = engine.step().expect("step").expect("more steps");
        assert!(out.telemetry.len() > 1, "the engine models several nodes");
        for frame in session.encode_step(&out).expect("encode").frames {
            if frame.header().expect("header").msg_type != MsgType::Telemetry.id() {
                continue;
            }
            let body =
                v2xw_record::wire::telemetry::TelemetryBody::decode(frame.body()).expect("decode");
            assert_eq!(body.records.len(), 1, "only the subscribed node");
            assert_eq!(body.records[0].node_id, node);
            seen += 1;
        }
    }
    assert!(seen > 0, "some telemetry arrived");

    // Clearing the subscription stops it, which is what closing an inspector must do.
    session.follow(None, true, true, &[]);
    for _ in 0..3 {
        let out = engine.step().expect("step").expect("more steps");
        for frame in session.encode_step(&out).expect("encode").frames {
            assert_ne!(
                frame.header().expect("header").msg_type,
                MsgType::Telemetry.id(),
                "an unsubscribed connection gets no telemetry"
            );
        }
    }
}

#[test]
fn a_seek_emits_a_seek_result_keyframe_and_at_most_one_gop_of_deltas() {
    // Conformance R4's frame shape and P4's bound.
    let mut engine = engine();
    let d = engine.descriptor().clone();
    let mut session = Session::new(ConnectParams::default(), &d);
    session
        .hello_frame(&d, RunState::Running, 0, "")
        .expect("hello");
    for _ in 0..12 {
        let out = engine.step().expect("step").expect("more steps");
        session.encode_step(&out).expect("encode");
    }
    // Backwards, which is the case the snapshot encoder refuses without a rebuild.
    let outputs = engine.seek(1_500_000_000).expect("seek");
    let (frames, keyframe_seq, deltas) = session.encode_seek(&outputs).expect("encode seek");
    let first = frames.first().expect("a frame").header().expect("header");
    assert_eq!(first.msg_type, MsgType::Keyframe.id());
    assert_eq!(
        first.flags & (FLAG_RESYNC | v2xw_record::wire::FLAG_SEEK_RESULT),
        FLAG_RESYNC | v2xw_record::wire::FLAG_SEEK_RESULT
    );
    assert_eq!(first.seq, keyframe_seq);
    let max = d.cadence.max_deltas_per_gop() as usize;
    assert!(deltas <= max, "{deltas} deltas exceeds the {max} of §7.3");
    for frame in frames.iter().skip(1) {
        let header = frame.header().expect("header");
        assert!(
            header.msg_type == MsgType::Delta.id() || header.msg_type == MsgType::Telemetry.id(),
            "a seek sends deltas and the §7.3 companion state, nothing else"
        );
    }
}
