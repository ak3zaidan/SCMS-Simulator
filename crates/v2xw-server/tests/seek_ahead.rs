//! A seek past the kernel's lead runs the kernel there, with progress, and lands.
//!
//! The kernel runs a bounded distance ahead of the stream (`tests/lead.rs`), which is right
//! for memory and for the chase view's live message log, and which made every seek beyond
//! that distance — 12.8 s at the defaults — fail with "the engine has only simulated up to
//! …". The scrub bar could not reach most of a run nobody had watched yet (reported by the
//! wave A integrator). This drives the real server over a real socket: a seek to 40 s of a
//! run standing at 0 s must report `job.progress`, land on a `SEEK_RESULT` keyframe whose
//! GOP covers 40 s with deltas up to exactly 40 s, reply with `t_ns = 40 s`, and leave the
//! lead bounded afterwards.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use v2xw_record::wire::{FLAG_SEEK_RESULT, Frame, MsgType};
use v2xw_server::{LiveOptions, ServerOptions, serve_scenario};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seek_beyond_the_lead_computes_ahead_with_progress_and_lands() {
    let text = std::fs::read_to_string(repo_root().join("scenarios/phase1-grid.yaml"))
        .expect("read phase1-grid.yaml")
        .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 600.0")
        .replace("name: phase1-grid", "name: seek-ahead");
    let dir = std::env::temp_dir().join("v2xw-server-seek-ahead");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("seek-ahead.yaml");
    std::fs::write(&path, text).expect("write scenario");
    let options = LiveOptions {
        build_utc: "2026-09-23T00:00:00Z".to_string(),
        paused: true,
        speed: 0.0,
        // A short lead, so 40 s is well past it whatever the channel holds.
        lookahead_steps: 16,
        ..LiveOptions::default()
    };
    let server = serve_scenario(
        ServerOptions {
            bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            ..ServerOptions::default()
        },
        &path,
        options,
    )
    .await
    .expect("server");

    let url = format!("ws://{}/vwp/v1?compress=none&v=1", server.address());
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
            .expect("request");
    request
        .headers_mut()
        .insert("sec-websocket-protocol", "vwp.v1".parse().expect("header"));
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connect");

    const TARGET: u64 = 40_000_000_000;
    let seek = json!({"jsonrpc": "2.0", "id": 7, "method": "run.seek",
                      "params": {"t_ns": TARGET, "pause_after": true}});
    ws.send(Message::Text(seek.to_string().into()))
        .await
        .expect("send");

    let mut progress: Vec<f64> = Vec::new();
    let mut job_done = false;
    let mut seek_keyframe: Option<u64> = None;
    let mut last_delta: Option<u64> = None;
    let reply: Value = loop {
        let msg = tokio::time::timeout(Duration::from_secs(120), ws.next())
            .await
            .expect("the server answered within two minutes")
            .expect("open")
            .expect("frame");
        match msg {
            Message::Text(text) => {
                let v: Value = serde_json::from_str(&text).expect("json");
                match v.get("method").and_then(Value::as_str) {
                    Some("job.progress") => {
                        progress.push(v["params"]["progress"].as_f64().expect("progress"));
                    }
                    Some("job.done") => job_done = true,
                    _ if v.get("id") == Some(&json!(7)) => break v,
                    _ => {}
                }
            }
            Message::Binary(bytes) => {
                let frame = Frame::from_bytes(bytes.to_vec()).expect("frame");
                let h = frame.header().expect("header");
                if h.flags & FLAG_SEEK_RESULT == 0 {
                    continue;
                }
                let t = || u64::from_le_bytes(frame.body()[0..8].try_into().expect("8 bytes"));
                if h.msg_type == MsgType::Keyframe.id() {
                    seek_keyframe = Some(t());
                } else if h.msg_type == MsgType::Delta.id() {
                    last_delta = Some(t());
                }
            }
            _ => {}
        }
    };

    assert!(
        reply.get("error").is_none(),
        "the seek was refused: {reply}"
    );
    assert_eq!(reply["result"]["t_ns"], json!(TARGET));
    assert!(
        !progress.is_empty(),
        "job.progress was reported while the kernel ran ahead"
    );
    assert!(
        progress.windows(2).all(|w| w[1] >= w[0]),
        "progress never goes backwards: {progress:?}"
    );
    assert_eq!(progress.last().copied(), Some(1.0), "and ends at 1");
    assert!(job_done, "job.done closed the job");
    let keyframe = seek_keyframe.expect("a SEEK_RESULT keyframe");
    assert!(
        keyframe <= TARGET && TARGET - keyframe < 1_000_000_000,
        "the keyframe opens the GOP that holds the target: {keyframe}"
    );
    assert_eq!(
        last_delta.unwrap_or(keyframe),
        TARGET,
        "the stream is positioned exactly at the target"
    );

    // The lead is bounded again: the kernel did not run on to the horizon behind the seek.
    let status = json!({"jsonrpc": "2.0", "id": 8, "method": "run.status", "params": {}});
    ws.send(Message::Text(status.to_string().into()))
        .await
        .expect("send");
    let status: Value = loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("status")
            .expect("open")
            .expect("frame");
        if let Message::Text(text) = msg {
            let v: Value = serde_json::from_str(&text).expect("json");
            if v.get("id") == Some(&json!(8)) {
                break v;
            }
        }
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let produced = status["result"]["engine"]["produced_ns"]
        .as_u64()
        .expect("produced_ns");
    assert!(
        produced < TARGET + 10_000_000_000,
        "the kernel stopped near the target, at {} s of 60 s",
        produced as f64 / 1e9
    );
    let _ = ws.close(None).await;
    server.stop().await;
}
