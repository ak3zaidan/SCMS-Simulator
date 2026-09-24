//! Following a car the page clicked, by its actor id.
//!
//! In a run whose vehicles spawn after t = 0 the `Hello` node table is empty, so the page
//! does not know which node a clicked car carries and sends `view.follow {actor}`. That
//! parameter was published and ignored, the session followed nothing, and the page said
//! "No radio selected" for every car in the owner's Manhattan run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use v2xw_server::live::{LiveEngine, LiveOptions};
use v2xw_server::rpc::{self, Context};
use v2xw_server::{Run, Session};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

fn scenario() -> PathBuf {
    let text = std::fs::read_to_string(repo_root().join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("duration_s: 60.0", "duration_s: 5.0")
        .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 3000.0");
    let dir = std::env::temp_dir().join("v2xw-server-follow");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("follow.yaml");
    std::fs::write(&path, text).expect("write scenario");
    path
}

fn call(
    run: &Run,
    session: Option<&mut Session>,
    method: &str,
    params: Value,
) -> Result<Value, Value> {
    let text = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = rpc::parse(&text).expect("parse");
    let mut ctx = Context {
        run,
        session,
        pending: None,
        received_at: None,
    };
    match rpc::dispatch(&mut ctx, &request) {
        Ok(outcome) => Ok(outcome.result),
        Err(e) => Err(rpc::failure(&json!(1), &e)["error"].clone()),
    }
}

#[test]
fn view_follow_by_actor_follows_the_node_on_that_actor() {
    let engine = LiveEngine::open(
        &scenario(),
        LiveOptions {
            build_utc: "2026-09-22T00:00:00Z".to_string(),
            paused: true,
            speed: 0.0,
            ..LiveOptions::default()
        },
    )
    .expect("build");
    let world_json = engine.world_json().to_string();
    let run = Run::new(Box::new(engine), world_json).expect("run");
    let descriptor = run.descriptor();
    assert!(
        descriptor.hello.nodes.is_empty(),
        "the premise: no node exists at t = 0, so the Hello table cannot map a car to a node"
    );

    call(&run, None, "run.start", json!({"paused": true, "speed": 0})).expect("start");
    call(&run, None, "run.resume", json!({})).expect("resume");
    // No server loop runs in a test: the run is advanced by hand, as apply.rs does.
    let deadline = Instant::now() + Duration::from_secs(120);
    while run.nodes().iter().all(|r| r.actor_id == u32::MAX) {
        assert!(Instant::now() < deadline, "no equipped vehicle appeared");
        if !run.tick().expect("the engine runs") {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let row = run
        .nodes()
        .into_iter()
        .find(|r| r.actor_id != u32::MAX)
        .expect("an equipped vehicle");

    let mut session = Session::new(Default::default(), &descriptor);
    session.bind(run.generation(), "");
    let followed = call(
        &run,
        Some(&mut session),
        "view.follow",
        json!({"actor": row.actor_id, "camera": "chase"}),
    )
    .expect("view.follow");
    assert_eq!(
        followed["following"],
        json!(row.node_id),
        "following actor {} must follow node {}: {followed}",
        row.actor_id,
        row.node_id
    );
    assert!(session.telemetry_nodes().contains(&row.node_id));

    // A car with no radio follows no radio, rather than keeping the previous one.
    let none = call(
        &run,
        Some(&mut session),
        "view.follow",
        json!({"actor": 4_000_000}),
    )
    .expect("view.follow");
    assert_eq!(none["following"], Value::Null, "{none}");
}
