//! The run lifecycle as a state machine, and the one-kernel rule.
//!
//! The owner's report was that a few runs worked and then another would not start. The
//! mechanism, measured before this file existed: `run.stop` flipped a label and left the
//! kernel computing to its horizon, and each `run.start` spawned another kernel beside it —
//! four restarts of a dense grid left five kernels and 480 % CPU in the process. So this
//! file drives every transition the page can cause and holds the process to **at most one
//! kernel thread, ever**, and to **none** once a run is stopped.
//!
//! One test, on purpose: the kernel-thread count is process-wide, and a second test in this
//! binary running its own kernel in parallel would make the count meaningless.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use v2xw_server::live::{LiveEngine, LiveOptions, kernel_threads};
use v2xw_server::rpc::{self, Context};
use v2xw_server::{Run, RunState, Session};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

fn scenario(name: &str, seconds: u32, rate: u32) -> PathBuf {
    let text = std::fs::read_to_string(repo_root().join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("duration_s: 60.0", &format!("duration_s: {seconds}.0"))
        .replace("rate_veh_per_h: 30.0", &format!("rate_veh_per_h: {rate}.0"))
        .replace("name: phase1-grid", &format!("name: {name}"));
    let dir = std::env::temp_dir().join("v2xw-server-lifecycle");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(format!("{name}.yaml"));
    std::fs::write(&path, text).expect("write scenario");
    path
}

fn call(run: &Run, method: &str, params: Value) -> Result<Value, Value> {
    let text = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = rpc::parse(&text).expect("parse");
    let mut ctx = Context {
        run,
        session: None,
        pending: None,
        received_at: None,
    };
    match rpc::dispatch(&mut ctx, &request) {
        Ok(outcome) => Ok(outcome.result),
        Err(e) => Err(rpc::failure(&json!(1), &e)["error"].clone()),
    }
}

fn ok(run: &Run, method: &str, params: Value) -> Value {
    call(run, method, params).unwrap_or_else(|e| panic!("{method} failed: {e}"))
}

fn refused(run: &Run, method: &str, params: Value, code: i64) {
    let err = call(run, method, params).expect_err(method);
    assert_eq!(err["code"], code, "{method}: {err}");
}

fn state(run: &Run) -> String {
    ok(run, "run.status", json!({}))["state"]
        .as_str()
        .expect("state")
        .to_string()
}

/// Waits (bounded) for the kernel-thread count to reach `n`.
fn threads_reach(n: usize, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if kernel_threads() == n {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    kernel_threads() == n
}

#[test]
fn every_transition_is_clean_and_there_is_never_a_second_kernel() {
    // A dense, long run, so a kernel left behind would still be computing when checked.
    let path = scenario("lifecycle-dense", 600, 6000);
    let short = scenario("lifecycle-short", 2, 30);
    let engine = LiveEngine::open(
        &path,
        LiveOptions {
            build_utc: "2026-09-22T00:00:00Z".to_string(),
            paused: true,
            speed: 0.0,
            ..LiveOptions::default()
        },
    )
    .expect("build");
    let world_json = engine.world_json().to_string();
    let run: Arc<Run> = Run::new(Box::new(engine), world_json).expect("run");
    assert_eq!(kernel_threads(), 1, "one kernel for the run that was opened");

    // --- the state machine, transition by transition ---------------------------------
    assert_eq!(state(&run), "paused");
    refused(&run, "run.pause", json!({}), -32002);
    ok(&run, "run.resume", json!({}));
    assert_eq!(state(&run), "running");
    refused(&run, "run.resume", json!({}), -32002);
    refused(&run, "run.start", json!({}), -32001);
    for _ in 0..20 {
        assert!(run.tick().expect("tick"));
    }
    ok(&run, "run.pause", json!({}));
    assert_eq!(state(&run), "paused");
    let before = ok(&run, "run.status", json!({}))["t_ns"].as_u64().expect("t_ns");
    let stepped = ok(&run, "run.step", json!({"count": 5}));
    assert_eq!(stepped["stepped"], 5);
    assert_eq!(stepped["t_ns"].as_u64(), Some(before + 5 * 100_000_000));
    // A seek needs a connection; over HTTP it is refused and the run does not move.
    refused(&run, "run.seek", json!({"t_ns": 0}), -32009);
    assert_eq!(ok(&run, "run.status", json!({}))["t_ns"].as_u64(), Some(before + 500_000_000));
    // A page attaching to a paused run — a reload, a second tab — is sent the state at the
    // stream position, not an empty city until somebody presses play.
    let descriptor = run.descriptor();
    let mut session = Session::new(Default::default(), &descriptor);
    session.bind(run.generation(), "");
    let greeting = session.greet(&run, &descriptor).expect("greet");
    let kinds: Vec<u16> = greeting
        .iter()
        .map(|f| f.header().expect("header").msg_type)
        .collect();
    assert_eq!(kinds[0], v2xw_record::wire::MsgType::Hello.id());
    assert!(
        kinds.contains(&v2xw_record::wire::MsgType::Keyframe.id()),
        "a connection to a paused run gets a keyframe of where it stands: {kinds:?}"
    );
    // A seek on a connection: through a session, as the socket does it.
    let outputs = run.seek(1_000_000_000).expect("seek");
    session.encode_seek(&outputs).expect("encode the seek");
    assert_eq!(state(&run), "paused");
    assert_eq!(kernel_threads(), 1);

    // --- stop: the kernel is gone when the call returns --------------------------------
    ok(&run, "run.stop", json!({}));
    assert_eq!(state(&run), "finished");
    assert_eq!(
        kernel_threads(),
        0,
        "run.stop must stop and join the kernel, not leave it computing to the horizon"
    );

    // --- run again, many times, from every state -----------------------------------------
    let mut generation = run.generation();
    for i in 0..12 {
        ok(&run, "run.start", json!({"paused": i % 2 == 0, "speed": 0}));
        assert!(
            kernel_threads() <= 1,
            "cycle {i}: {} kernel threads after a start",
            kernel_threads()
        );
        assert_eq!(run.generation(), generation + 1, "every start is a new run");
        generation = run.generation();
        if i % 2 == 0 {
            ok(&run, "run.resume", json!({}));
        }
        for _ in 0..(5 + i) {
            let _ = run.tick().expect("tick");
        }
        if i % 3 == 0 {
            ok(&run, "run.pause", json!({}));
        }
        match i % 4 {
            // Stop from running or paused.
            0 | 1 => {
                ok(&run, "run.stop", json!({}));
                assert_eq!(kernel_threads(), 0, "cycle {i}: stop left a kernel");
            }
            // Start straight from paused: the previous kernel is joined first.
            2 => {
                if state(&run) == "running" {
                    ok(&run, "run.pause", json!({}));
                }
            }
            // Switch scenario, then start from whatever state.
            _ => {
                if state(&run) == "running" {
                    ok(&run, "run.pause", json!({}));
                }
                let target = if i % 8 == 3 { &short } else { &path };
                let loaded = ok(&run, "scenario.load", json!({"path": target.display().to_string()}));
                assert_eq!(loaded["valid"], true);
            }
        }
        assert!(kernel_threads() <= 1, "cycle {i}: {} kernels", kernel_threads());
    }

    // --- a run that ends by itself lets its kernel go ------------------------------------
    ok(&run, "scenario.load", json!({"path": short.display().to_string()}));
    if state(&run) == "running" {
        ok(&run, "run.pause", json!({}));
    }
    ok(&run, "run.start", json!({"paused": false, "speed": 0}));
    assert_eq!(run.descriptor().duration, 2_000_000_000, "the switched scenario is running");
    for _ in 0..10_000 {
        if !run.tick().expect("tick") && run.state() == RunState::Finished {
            break;
        }
    }
    assert_eq!(state(&run), "finished");
    assert!(
        threads_reach(0, Duration::from_secs(10)),
        "a finished run's kernel must end: {} alive",
        kernel_threads()
    );
    // Seek back through the finished run over a connection, and run again.
    let outputs = run.seek(500_000_000).expect("seek into a finished run");
    assert!(!outputs.is_empty());
    ok(&run, "run.start", json!({"paused": true}));
    assert_eq!(kernel_threads(), 1);
    drop(run);
    assert!(
        threads_reach(0, Duration::from_secs(10)),
        "dropping the run must not leave its kernel behind"
    );
}
