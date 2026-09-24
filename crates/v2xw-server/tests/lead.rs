//! The kernel runs a bounded distance ahead of the stream.
//!
//! `LiveOptions::lookahead_steps` promised "small enough that a paused run stops the
//! kernel", and the pump absorbed steps until the whole retention window was full, so a
//! page watching at 1x was attached to a kernel that had already simulated the run to its
//! horizon: memory proportional to the run, `run.status` counters from the end of the run,
//! and a followed vehicle's message log (filled as steps are absorbed, read at the
//! stream's instant) that only ever held the run's last seconds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use v2xw_server::Run;
use v2xw_server::live::{LiveEngine, LiveOptions};
use v2xw_server::rpc::{self, Context};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

fn call(run: &Run, method: &str, params: Value) -> Value {
    let text = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = rpc::parse(&text).expect("parse");
    let mut ctx = Context {
        run,
        session: None,
        pending: None,
        received_at: None,
    };
    rpc::dispatch(&mut ctx, &request)
        .unwrap_or_else(|e| panic!("{method}: {e}"))
        .result
}

#[test]
fn a_run_nobody_is_watching_does_not_simulate_to_its_horizon() {
    let text = std::fs::read_to_string(repo_root().join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 600.0");
    let dir = std::env::temp_dir().join("v2xw-server-lead");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("lead.yaml");
    std::fs::write(&path, text).expect("write scenario");

    let options = LiveOptions {
        build_utc: "2026-09-22T00:00:00Z".to_string(),
        paused: true,
        speed: 0.0,
        ..LiveOptions::default()
    };
    let lookahead = options.lookahead_steps as u64;
    let engine = LiveEngine::open(&path, options).expect("build");
    let world_json = engine.world_json().to_string();
    let run = Run::new(Box::new(engine), world_json).expect("run");
    call(&run, "run.start", json!({"paused": true, "speed": 0}));

    // The stream stays at t = 0 while the kernel has time to run the whole 60 s grid
    // several times over, and the channel is drained the whole time, as the server's
    // loop does on every tick (a seek to 0 drains it without moving the stream).
    let until = std::time::Instant::now() + Duration::from_secs(4);
    while std::time::Instant::now() < until {
        let _ = run.seek(0);
        std::thread::sleep(Duration::from_millis(10));
    }

    let status = call(&run, "run.status", json!({}));
    let produced_ns = status["engine"]["produced_ns"]
        .as_u64()
        .expect("produced_ns");
    let horizon_ns = status["t_end_ns"].as_u64().expect("t_end_ns");
    // What the pump may take, plus what the host channel holds, plus the step in hand.
    let bound_ns = (2 * lookahead + 2) * 100_000_000;
    assert!(
        produced_ns <= bound_ns,
        "with the stream at t = 0 the run produced {} s of a {} s horizon; the lead is \
         bounded at {} s",
        produced_ns as f64 / 1e9,
        horizon_ns as f64 / 1e9,
        bound_ns as f64 / 1e9
    );
    assert!(produced_ns > 0, "the kernel produced nothing at all");
}
