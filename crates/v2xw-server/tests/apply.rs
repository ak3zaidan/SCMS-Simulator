//! The apply path, end to end through the JSON-RPC surface: an edit made with
//! `scenario.set` is what the next `run.start` runs.
//!
//! This is the owner's report — "applying edited parameters does nothing" — as a test.
//! Before this file existed `scenario.set` validated a document, answered with the hash of
//! the scenario already running and threw the document away, and `run.start` refused a
//! seed and rewound the same scenario. Every test below edits one setting, starts a run
//! through `run.start` exactly as the page does, drives it to the end, and checks an
//! observable the setting must move: the horizon, the output digest, the fleet, the radio
//! outcome, the message family.
//!
//! The calls go through [`v2xw_server::rpc::dispatch`] on the HTTP path (no connection),
//! which is the path the page's Run button uses, so what is tested is the server's own
//! parameter handling and not a shortcut around it.

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

/// `phase1-grid.yaml`, shortened to `seconds` and written to a scratch directory of its own,
/// so this file's presets are the scenarios it wrote and nothing else.
fn scenario(name: &str, seconds: u32, rate: u32) -> PathBuf {
    let source = repo_root().join("scenarios/phase1-grid.yaml");
    let text = std::fs::read_to_string(&source)
        .expect("read phase1-grid.yaml")
        .replace("duration_s: 60.0", &format!("duration_s: {seconds}.0"))
        .replace("rate_veh_per_h: 30.0", &format!("rate_veh_per_h: {rate}.0"));
    let dir = std::env::temp_dir().join(format!("v2xw-server-apply-{name}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(format!("{name}.yaml"));
    std::fs::write(&path, text).expect("write scenario");
    path
}

fn serve(path: &Path) -> Arc<Run> {
    let engine = LiveEngine::open(
        path,
        LiveOptions {
            build_utc: "2026-09-22T00:00:00Z".to_string(),
            paused: true,
            speed: 0.0,
            ..LiveOptions::default()
        },
    )
    .expect("build");
    let world_json = engine.world_json().to_string();
    Run::new(Box::new(engine), world_json).expect("run")
}

/// One JSON-RPC call on the HTTP path. Returns the `result`, or the error object.
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

/// Starts a run the way the page does — `run.start {paused}` then `run.resume` — and
/// steps it to its end, returning the final `run.status`.
fn run_to_end(run: &Run) -> Value {
    start_and_finish(run, json!({"paused": true, "speed": 0}))
}

fn start_and_finish(run: &Run, start: Value) -> Value {
    ok(run, "run.start", start);
    ok(run, "run.resume", json!({}));
    for _ in 0..100_000 {
        match run.tick() {
            Ok(true) => {}
            Ok(false) => {
                if run.state() == v2xw_server::RunState::Finished {
                    break;
                }
            }
            Err(e) => panic!("the engine aborted: {e}"),
        }
    }
    let status = ok(run, "run.status", json!({}));
    assert_eq!(status["state"], "finished", "the run did not finish: {status}");
    status
}

fn digest(status: &Value) -> String {
    status["engine"]["output_digest"]
        .as_str()
        .unwrap_or_else(|| panic!("a finished run publishes its output digest: {status}"))
        .to_string()
}

fn stats(status: &Value) -> &Value {
    &status["engine"]["stats"]
}

fn patch(run: &Run, ops: Value) -> Value {
    let result = ok(run, "scenario.set", json!({"patch": ops}));
    assert_eq!(result["valid"], true, "the edit was refused: {result}");
    result
}

#[test]
fn an_edited_duration_is_the_next_runs_horizon() {
    let run = serve(&scenario("duration", 4, 30));
    let first = run_to_end(&run);
    assert_eq!(first["t_end_ns"], 4_000_000_000u64);

    let set = patch(&run, json!([{"op": "replace", "path": "/time/duration_s", "value": 2}]));
    assert_eq!(set["requires_restart"], json!(["/time/duration_s"]));
    let staged = set["hash"].as_str().expect("hash").to_string();
    assert_ne!(
        staged,
        first["scenario_hash"].as_str().unwrap(),
        "an edit is a different scenario, so it has a different digest"
    );
    // Held for the next run, visibly: the next scenario and the running one differ.
    let get = ok(&run, "scenario.get", json!({}));
    assert_eq!(get["hash"], staged);
    assert_eq!(get["running_hash"], first["scenario_hash"]);

    let second = run_to_end(&run);
    assert_eq!(second["t_end_ns"], 2_000_000_000u64, "the edit did not reach the run");
    assert_eq!(second["t_ns"], 2_000_000_000u64, "the clock stops at the horizon, not past it");
    assert_eq!(second["scenario_hash"], staged);
    assert_eq!(run.descriptor().duration, 2_000_000_000);
    // Consumed: the next run of the page is the edited scenario, with nothing pending.
    assert!(ok(&run, "scenario.get", json!({}))["staged"].is_null());
}

#[test]
fn the_master_seed_moves_the_digest_and_the_same_seed_reproduces_it() {
    let run = serve(&scenario("seed", 3, 3000));
    let a = digest(&run_to_end(&run));
    let a_again = digest(&run_to_end(&run));
    assert_eq!(a, a_again, "the same scenario and seed must reproduce the run");

    patch(&run, json!([{"op": "replace", "path": "/seed", "value": "0x1234"}]));
    let b = digest(&run_to_end(&run));
    assert_ne!(a, b, "a different master seed must give a different run");
    let b_again = digest(&run_to_end(&run));
    assert_eq!(b, b_again, "the edited seed reproduces too");

    // `run.start`'s own `seed` override reaches the run the same way.
    let c = digest(&start_and_finish(&run, json!({"paused": true, "speed": 0, "seed": 99})));
    assert_ne!(c, b, "run.start's seed override must reach the run");
    assert_eq!(
        ok(&run, "scenario.get", json!({}))["scenario"]["seed"],
        "0x0000000000000063",
        "the scenario on screen is the one that ran"
    );
}

#[test]
fn the_arrival_rate_moves_the_fleet() {
    let run = serve(&scenario("rate", 20, 30));
    let few = stats(&run_to_end(&run))["actors_seen"].as_u64().expect("actors_seen");
    patch(
        &run,
        json!([{"op": "replace", "path": "/actors/vehicles/demand/rate_veh_per_h", "value": 6000}]),
    );
    let many = stats(&run_to_end(&run))["actors_seen"].as_u64().expect("actors_seen");
    assert!(
        many > few * 3,
        "raising the arrival rate 200x must spawn more vehicles: {few} -> {many}"
    );
}

/// An edited radio model reaches the next run's receptions.
///
/// This used to switch the PHY and MAC tiers from medium to high. The radio track made
/// every node generate at its own phase with a hand-off jitter, so on this grid no two
/// frames overlap at a receiver any more; the high PHY's only addition, preamble capture,
/// decides overlapping frames alone, and the high MAC is the medium one (KEY_STATUS says
/// both). The tiers are therefore correctly inert here, and the test edits the key the
/// radio track wired instead: `radio.models`, free-space propagation with no fading in
/// place of the default dual-slope law with Nakagami fading, which changes every received
/// power. The assertion is the same.
#[test]
fn a_radio_setting_changes_what_is_received() {
    let run = serve(&scenario("radio", 20, 6000));
    let medium = run_to_end(&run);
    patch(
        &run,
        json!([
            {"op": "add", "path": "/radio/models/propagation", "value": {"id": "propagation/free-space"}},
            {"op": "add", "path": "/radio/models/fading", "value": {"id": "fading/none"}},
        ]),
    );
    let high = run_to_end(&run);
    assert!(
        stats(&medium)["rx_attempts"].as_u64().unwrap_or(0) > 0,
        "the run must have receptions to compare: {medium}"
    );
    assert_ne!(
        (stats(&medium)["rx_ok"].clone(), digest(&medium)),
        (stats(&high)["rx_ok"].clone(), digest(&high)),
        "the radio models must change the reception outcome"
    );
    // A conflicting edit is refused and changes nothing: the next run is the last good one.
    let refused = call(
        &run,
        "scenario.set",
        json!({"patch": [{"op": "replace", "path": "/radio/tiers/propagation", "value": "abstract"}]}),
    );
    assert!(refused.is_err(), "abstract propagation with a high PHY must be refused");
    assert_eq!(digest(&run_to_end(&run)), digest(&high));
}

#[test]
fn the_message_family_is_what_goes_on_the_air() {
    let run = serve(&scenario("family", 3, 3000));
    let bsm = run_to_end(&run);
    assert!(stats(&bsm)["tx_by_type"]["bsm"].as_u64().unwrap_or(0) > 0, "{bsm}");
    patch(&run, json!([{"op": "replace", "path": "/messages/sets", "value": ["cam"]}]));
    let cam = run_to_end(&run);
    let by_type = &stats(&cam)["tx_by_type"];
    assert!(by_type["cam"].as_u64().unwrap_or(0) > 0, "no CAM was sent: {cam}");
    assert!(by_type.get("bsm").is_none(), "a BSM was sent after the set became [cam]: {cam}");
}

#[test]
fn the_exporters_write_their_files_after_the_run() {
    let path = scenario("exporters", 3, 3000);
    let out = std::env::temp_dir().join(format!("v2xw-apply-exports-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let engine = LiveEngine::open(
        &path,
        LiveOptions {
            build_utc: "2026-09-22T00:00:00Z".to_string(),
            paused: true,
            speed: 0.0,
            recording: Some(out.join("recording.mcap")),
            ..LiveOptions::default()
        },
    )
    .expect("build");
    let world_json = engine.world_json().to_string();
    let run = Run::new(Box::new(engine), world_json).expect("run");

    // An id this build does not have is refused, not skipped.
    let refused = call(
        &run,
        "scenario.set",
        json!({"patch": [{"op": "add", "path": "/exporters", "value": [{"id": "ma-dataset-v9"}]}]}),
    )
    .expect_err("an unknown exporter is refused");
    assert!(refused.to_string().contains("not an exporter this build has"), "{refused}");

    patch(
        &run,
        json!([{"op": "add", "path": "/exporters",
                "value": [{"id": "recording"}, {"id": "jsonl", "opts": {"profile": "node"}}]}]),
    );
    let done = run_to_end(&run);
    // The exporters run on the kernel's thread after the last step; give them a moment.
    let mut exports = done["engine"]["exports"].clone();
    for _ in 0..200 {
        if !exports.is_null() {
            break;
        }
        let _ = run.tick();
        std::thread::sleep(std::time::Duration::from_millis(10));
        exports = ok(&run, "run.status", json!({}))["engine"]["exports"].clone();
    }
    let list = exports.as_array().unwrap_or_else(|| panic!("exports reported: {exports}"));
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["exporter"], "recording");
    let tables = list[1]["files"].as_array().expect("files");
    assert!(
        tables.iter().any(|f| f["path"].as_str().is_some_and(|p| p.ends_with("node_tx.jsonl"))
            && f["rows"].as_u64().unwrap_or(0) > 0),
        "a node.tx table with rows: {exports}"
    );
    assert!(
        !tables.iter().any(|f| f["path"].as_str().is_some_and(|p| p.contains("gt_"))),
        "the node profile drops ground-truth channels: {exports}"
    );
    assert!(out.join("jsonl").is_dir());
}

#[test]
fn the_time_keys_reach_the_run() {
    let run = serve(&scenario("time-keys", 20, 6000));
    let plain = run_to_end(&run);
    assert_eq!(plain["engine"]["suppressed_frames"], 0);
    // time.time_dilation: no radio frame inside the window.
    patch(
        &run,
        json!([{"op": "add", "path": "/time/time_dilation",
                "value": [{"from_s": 5.0, "to_s": 15.0}]}]),
    );
    let dilated = run_to_end(&run);
    let suppressed = dilated["engine"]["suppressed_frames"].as_u64().expect("suppressed");
    assert!(suppressed > 0, "frames inside the window are suppressed: {dilated}");
    assert!(
        stats(&dilated)["tx_frames"].as_u64() < stats(&plain)["tx_frames"].as_u64(),
        "fewer frames go on the air with a dilation window"
    );
    // time.des_resolution: a promise the run must be able to keep.
    let refused = call(
        &run,
        "scenario.set",
        json!({"patch": [{"op": "replace", "path": "/time/des_resolution", "value": "1ms"}]}),
    )
    .expect_err("1ms with a microsecond PHY is refused");
    assert!(refused.to_string().contains("des_resolution"), "{refused}");
    // world.cache: the world is written once and read back identically.
    let dir = std::env::temp_dir().join(format!("v2xw-apply-world-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    patch(
        &run,
        json!([
            {"op": "replace", "path": "/time/time_dilation", "value": []},
            {"op": "add", "path": "/world/cache", "value": dir.display().to_string()},
        ]),
    );
    let first = run_to_end(&run);
    let entries = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
    assert_eq!(entries, 1, "the imported world is kept in the cache directory");
    let second = run_to_end(&run);
    assert_eq!(digest(&first), digest(&second), "a cached world is the same world");
    assert_eq!(digest(&first), digest(&plain), "and the same run as without the cache");
}

#[test]
fn a_document_round_tripped_through_a_browser_is_not_an_edit() {
    // JavaScript has one number type, so a document that went through the page comes back
    // with every whole float written as an integer. Inside the world generator's opaque
    // parameter block nothing re-types them, and a plain `==` reported six untouched
    // fields as edits ("Applied 7 changes" for one).
    let run = serve(&scenario("roundtrip", 2, 30));
    let mut doc = ok(&run, "scenario.get", json!({}))["scenario"].clone();
    for value in doc["world"]["source"]["params"]
        .as_object_mut()
        .expect("params")
        .values_mut()
    {
        if let Some(f) = value.as_f64()
            && f.fract() == 0.0
            && value.is_f64()
        {
            *value = json!(f as i64);
        }
    }
    let set = ok(&run, "scenario.set", json!({"scenario": doc}));
    assert_eq!(set["requires_restart"], json!([]), "{set}");
    doc["time"]["duration_s"] = json!(1);
    let set = ok(&run, "scenario.set", json!({"scenario": doc}));
    assert_eq!(set["requires_restart"], json!(["/time/duration_s"]), "{set}");
}

#[test]
fn an_invalid_edit_is_reported_and_not_held() {
    let run = serve(&scenario("invalid", 2, 30));
    let before = ok(&run, "scenario.get", json!({}))["hash"].clone();
    let err = call(
        &run,
        "scenario.set",
        json!({"patch": [{"op": "replace", "path": "/time/duration_s", "value": -5}]}),
    )
    .expect_err("a negative duration is refused");
    assert_eq!(err["code"], -32004, "{err}");
    assert!(
        err.to_string().contains("duration"),
        "the refusal names the field: {err}"
    );
    // With validate:false it answers valid:false and still holds nothing.
    let soft = ok(
        &run,
        "scenario.set",
        json!({"validate": false,
               "patch": [{"op": "replace", "path": "/time/duration_s", "value": -5}]}),
    );
    assert_eq!(soft["valid"], false);
    assert_eq!(ok(&run, "scenario.get", json!({}))["hash"], before);
    // `scenario.validate` runs the loader's rules, not a shape check.
    let doc = ok(&run, "scenario.get", json!({}))["scenario"].clone();
    let mut bad = doc.clone();
    bad["radio"]["tiers"]["phy"] = json!("high");
    let verdict = ok(&run, "scenario.validate", json!({"scenario": bad}));
    assert_eq!(verdict["valid"], false, "phy high with mac medium is a loader conflict: {verdict}");
    assert_eq!(ok(&run, "scenario.validate", json!({"scenario": doc}))["valid"], true);
}

#[test]
fn a_preset_can_be_loaded_and_run() {
    let path = scenario("presets", 2, 30);
    let other = path.with_file_name("presets-other.yaml");
    let text = std::fs::read_to_string(&path)
        .expect("read")
        .replace("name: phase1-grid", "name: presets-other")
        .replace("duration_s: 2.0", "duration_s: 3.0");
    std::fs::write(&other, text).expect("write");
    let run = serve(&path);
    let list = ok(&run, "scenario.list", json!({"kind": "presets"}));
    let id = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["name"] == "presets-other")
        .unwrap_or_else(|| panic!("the other scenario in the folder is offered: {list}"))["id"]
        .as_str()
        .expect("id")
        .to_string();
    let loaded = ok(&run, "scenario.load", json!({"path": id}));
    assert_eq!(loaded["valid"], true);
    let status = run_to_end(&run);
    assert_eq!(status["t_end_ns"], 3_000_000_000u64, "the loaded preset is what ran");
    assert_eq!(run.descriptor().hello.strings.get(run.descriptor().hello.str_scenario_name), Some("presets-other"));
}

#[test]
fn run_seek_over_http_is_refused_without_moving_the_run() {
    let run = serve(&scenario("seek-http", 3, 30));
    ok(&run, "run.start", json!({"paused": true}));
    ok(&run, "run.step", json!({"count": 10}));
    let before = ok(&run, "run.status", json!({}));
    let err = call(&run, "run.seek", json!({"t_ns": 100_000_000u64})).expect_err("refused over HTTP");
    assert_eq!(err["code"], -32009);
    let after = ok(&run, "run.status", json!({}));
    assert_eq!(before["t_ns"], after["t_ns"], "a refused seek moved the run");
    assert_eq!(before["state"], after["state"]);
}
