//! A connection that lives across runs.
//!
//! §6.6: `run.start` "sends a fresh `Hello` on this connection before the first `Keyframe`
//! of the new run". Before generations existed the server did not, and the new run's
//! first step was encoded by a snapshot encoder still holding the previous run's last
//! step. The encoder refuses a step that does not advance — correctly — and the transport
//! answered that by ending the connection with no frame at all, so the page's socket died
//! on every "Run again" and came back only by reconnecting. These tests pin both halves:
//! the failure is real (so the check can fail), and the regreet removes it.

use std::sync::Arc;

use v2xw_record::wire::MsgType;
use v2xw_record::wire::hello::{HELLO_RESUMED, HelloBody};
use v2xw_server::engine::Control;
use v2xw_server::{Run, Session, StubEngine, StubOptions};

fn fixture() -> Arc<Run> {
    let engine = StubEngine::new(StubOptions {
        actors: 12,
        grid: 4,
        duration_s: 30,
        paused: true,
        ..StubOptions::default()
    })
    .expect("fixture");
    let world_json = v2xw_world::serde_vwp::to_json_string(engine.geometry()).expect("world json");
    Run::new(Box::new(engine), world_json).expect("run")
}

fn start(run: &Run) {
    run.control(Control::Start {
        paused: true,
        speed: 0.0,
        seed: None,
        scenario: None,
    })
    .expect("start");
}

/// A session that has greeted and encoded ten steps of the first run.
fn watched(run: &Run) -> (Session, Vec<Arc<v2xw_server::StepOutput>>) {
    let descriptor = run.descriptor();
    let mut session = Session::new(Default::default(), &descriptor);
    session.bind(run.generation(), "");
    session.greet(run, &descriptor).expect("greet");
    let mut rx = run.subscribe();
    run.control(Control::Resume).expect("resume");
    let mut seen = Vec::new();
    for _ in 0..10 {
        assert!(run.tick().expect("tick"));
        let step = rx.try_recv().expect("a step");
        session.encode_step(&step).expect("encode");
        seen.push(step);
    }
    run.control(Control::Pause).expect("pause");
    (session, seen)
}

#[test]
fn a_new_runs_first_step_cannot_be_encoded_against_the_old_runs_encoder() {
    let run = fixture();
    let (mut session, _) = watched(&run);
    start(&run);
    let mut rx = run.subscribe();
    run.control(Control::Resume).expect("resume");
    assert!(run.tick().expect("tick"));
    let first = rx.try_recv().expect("the new run's first step");
    assert_eq!(first.sim_time, 0, "run.start rewinds");
    assert_eq!(
        first.generation, 1,
        "and the step says which run it belongs to"
    );
    assert!(
        session.encode_step(&first).is_err(),
        "the failure the regreet exists for: a step that does not advance is refused"
    );
}

#[test]
fn a_regreeted_connection_gets_a_fresh_hello_and_streams_the_new_run() {
    let run = fixture();
    let (mut session, _) = watched(&run);
    assert!(session.next_seq() > 0);
    start(&run);
    assert_ne!(session.generation(), run.generation());

    let frames = session.regreet(&run).expect("regreet");
    let hello = &frames[0];
    let header = hello.header().expect("header");
    assert_eq!(header.msg_type, MsgType::Hello.id());
    let body = HelloBody::decode(hello.body()).expect("decode Hello");
    assert_eq!(
        body.hello_flags & HELLO_RESUMED,
        0,
        "a new run is never a resume"
    );
    assert_eq!(
        body.resume_seq, 0,
        "§1.4: seq starts again at 0 for a new run"
    );
    assert_eq!(session.generation(), run.generation());

    let mut rx = run.subscribe();
    run.control(Control::Resume).expect("resume");
    assert!(run.tick().expect("tick"));
    let first = rx.try_recv().expect("the new run's first step");
    let effects = session.encode_step(&first).expect("the new run encodes");
    let opening = effects.frames[0].header().expect("header");
    assert_eq!(
        opening.msg_type,
        MsgType::Keyframe.id(),
        "the new run opens with a keyframe"
    );
    assert_eq!(opening.seq, 0);
}
