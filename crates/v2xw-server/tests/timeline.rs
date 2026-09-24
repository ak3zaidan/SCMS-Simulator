//! The scenario timeline reaches the page: `run.status` reports each item as it fires, with
//! what the engine did, and only once the stream has reached it.
//!
//! The kernel runs ahead of the stream, so a report taken from the kernel's frontier would mark
//! an event as fired while the page is still seconds before it; the report is filtered to the
//! stream position, and this test holds it to that.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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
fn run_status_reports_each_timeline_item_once_the_stream_reaches_it() {
    let text = std::fs::read_to_string(repo_root().join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("duration_s: 60.0", "duration_s: 8.0")
        .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 1800.0")
        .replace("name: phase1-grid", "name: timeline-status");
    let text = format!(
        "{text}\nevents:\n  - {{t: 3.0, until: 6.0, type: closure, target: \"edge:0\"}}\n  \
         - {{t: 4.0, type: param.change, path: weather.intensity, value: 0.5}}\n"
    );
    let dir = std::env::temp_dir().join("v2xw-server-timeline");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("timeline-status.yaml");
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

    let fired = |run: &Run| -> Vec<Value> {
        call(run, "run.status", json!({}))["engine"]["timeline"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    // Two seconds in: the kernel is well past 3 s (its lead), the stream is not.
    call(&run, "run.step", json!({"count": 20}));
    assert!(
        fired(&run).is_empty(),
        "nothing is reported before the stream reaches it"
    );

    call(&run, "run.step", json!({"count": 22}));
    let at_4 = fired(&run);
    let kinds: Vec<(String, String)> = at_4
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap_or_default().to_string(),
                e["phase"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("closure".to_string(), "start".to_string()),
            ("param.change".to_string(), "start".to_string())
        ],
        "{at_4:?}"
    );
    assert!(
        at_4[0]["effect"]
            .as_str()
            .unwrap_or_default()
            .contains("lanes closed"),
        "{}",
        at_4[0]
    );
    assert!(!at_4[0]["lanes"].as_array().expect("lanes").is_empty());

    call(&run, "run.step", json!({"count": 40}));
    let all = fired(&run);
    assert_eq!(all.len(), 3, "the closure's end is reported too: {all:?}");
    assert_eq!(all[2]["phase"], "end");

    // A new run starts with an empty timeline.
    call(&run, "run.start", json!({"paused": true}));
    assert!(fired(&run).is_empty());
    call(&run, "run.stop", json!({}));
}
