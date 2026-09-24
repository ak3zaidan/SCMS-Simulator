//! Every run-control parameter survives a rewind.
//!
//! Reported by the radio track: after `run.start` rewound a server started with
//! `--speed 0`, `run.status` said `speed: 1.0`. The cause was `run_start` reading an absent
//! `speed` as its schema default, 1, and writing that over the speed the run had. The page
//! passes the speed it last read, so it hid the defect; a CLI or notebook client that
//! rewinds with `{}` — the radio engineer's — got a run twenty times slower than asked.
//!
//! The same question is asked of every other parameter a rewind could reset: the pacing
//! mode (`sync`), the seed a previous `run.start` chose, and — for the fixture — the
//! `--speed` the binary was started with, which the fixture ignored altogether.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use v2xw_server::live::{LiveEngine, LiveOptions};
use v2xw_server::rpc::{self, Context};
use v2xw_server::{Run, StubEngine, StubOptions};

fn call(run: &Run, method: &str, params: Value) -> Value {
    let text = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = rpc::parse(&text).expect("parse");
    let mut ctx = Context {
        run,
        session: None,
        pending: None,
        received_at: None,
    };
    match rpc::dispatch(&mut ctx, &request) {
        Ok(outcome) => outcome.result,
        Err(e) => panic!("{method} failed: {e}"),
    }
}

fn status(run: &Run) -> Value {
    call(run, "run.status", json!({}))
}

fn fixture(speed: f64) -> Arc<Run> {
    let engine = StubEngine::new(StubOptions {
        actors: 8,
        grid: 3,
        duration_s: 20,
        paused: true,
        speed,
        ..StubOptions::default()
    })
    .expect("fixture");
    let world_json = v2xw_world::serde_vwp::to_json_string(engine.geometry()).expect("world json");
    Run::new(Box::new(engine), world_json).expect("run")
}

/// The pacing parameters a rewind must leave alone, checked on one run.
fn rewinds_keep_pacing(run: &Run, started_at: f64) {
    assert_eq!(
        status(run)["speed"],
        json!(started_at),
        "the speed it was started with"
    );

    // A rewind that says nothing about speed keeps it — the reported defect.
    call(run, "run.start", json!({"paused": true}));
    assert_eq!(
        status(run)["speed"],
        json!(started_at),
        "run.start without `speed` reset the speed"
    );

    // One that names a speed sets it, and the next silent rewind keeps *that*.
    call(run, "run.start", json!({"paused": true, "speed": 4.0}));
    assert_eq!(status(run)["speed"], json!(4.0));
    call(run, "run.start", json!({"paused": true}));
    assert_eq!(status(run)["speed"], json!(4.0), "a chosen speed survives");

    // The pacing mode: `run.speed {sync: client}` is a property of the session with the
    // engine, not of one run, and a rewind does not quietly turn it off.
    call(run, "run.speed", json!({"speed": 2.0, "sync": "client"}));
    call(run, "run.start", json!({"paused": true}));
    let s = status(run);
    assert_eq!(s["speed"], json!(2.0));
    assert_eq!(
        s["sync"],
        json!("client"),
        "the pacing mode survives a rewind"
    );
}

#[test]
fn a_fixture_rewind_keeps_the_speed_and_the_pacing_mode() {
    let run = fixture(0.0);
    rewinds_keep_pacing(&run, 0.0);
}

#[test]
fn the_fixture_honours_the_speed_it_was_started_with() {
    // `--speed` is documented for both modes; the fixture used to run at 1 whatever it said.
    assert_eq!(status(&fixture(0.0))["speed"], json!(0.0));
    assert_eq!(status(&fixture(3.0))["speed"], json!(3.0));
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

#[test]
fn a_live_rewind_keeps_the_speed_the_pacing_mode_and_the_seed() {
    let text = std::fs::read_to_string(repo_root().join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("duration_s: 60.0", "duration_s: 3.0")
        .replace("name: phase1-grid", "name: rewind-grid");
    let dir = std::env::temp_dir().join("v2xw-server-rewind");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("rewind-grid.yaml");
    std::fs::write(&path, text).expect("write scenario");
    let engine = LiveEngine::open(
        &path,
        LiveOptions {
            build_utc: "2026-09-23T00:00:00Z".to_string(),
            paused: true,
            speed: 0.0,
            ..LiveOptions::default()
        },
    )
    .expect("build");
    let world_json = engine.world_json().to_string();
    let run: Arc<Run> = Run::new(Box::new(engine), world_json).expect("run");
    rewinds_keep_pacing(&run, 0.0);

    // A seed chosen by one `run.start` is the run's seed from then on: the next rewind
    // re-runs that run, not the scenario file's seed.
    call(
        run.as_ref(),
        "run.start",
        json!({"paused": true, "seed": 77}),
    );
    let hash = status(&run)["scenario_hash"].clone();
    call(run.as_ref(), "run.start", json!({"paused": true}));
    assert_eq!(
        status(&run)["scenario_hash"],
        hash,
        "the rewind ran a different scenario from the one the seed chose"
    );
    let scenario = call(run.as_ref(), "scenario.get", json!({}));
    // The scenario document writes a seed as a zero-padded hex string.
    assert_eq!(
        scenario["scenario"]["seed"],
        json!("0x000000000000004d"),
        "{scenario}"
    );
    call(run.as_ref(), "run.stop", json!({}));
}
