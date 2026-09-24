//! `run.speed {sync: "client"}` paces the producer to the slowest connection (§1.5).
//!
//! The mode was accepted, echoed by `run.speed` and reported by `run.status`, and the
//! producer read the speed and ignored it: a "lossless" demo at speed 0 shed deltas exactly
//! like a free-running one. A slow reader here reads an unthrottled fixture; in the free mode
//! the server must report drops (the control, which shows the check can fail), and in the
//! client mode it must report none and deliver a dense `seq`.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use v2xw_record::wire::{Frame, MsgType};
use v2xw_server::{ServerOptions, StubOptions, serve_stub};

/// Reads `frames` canonical frames slowly; returns `(drops notified, seq gaps seen)`.
async fn slow_read(sync: &str, frames: usize) -> (usize, usize) {
    let server = serve_stub(
        ServerOptions {
            bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            ..ServerOptions::default()
        },
        StubOptions {
            actors: 400,
            grid: 8,
            duration_s: 3600,
            paused: true,
            speed: 0.0,
            ..StubOptions::default()
        },
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
    for (id, method, params) in [
        (1, "run.speed", json!({"speed": 0, "sync": sync})),
        (2, "run.resume", json!({})),
    ] {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        ws.send(Message::Text(msg.to_string().into()))
            .await
            .expect("send");
    }
    let (mut drops, mut gaps, mut seen) = (0usize, 0usize, 0usize);
    let mut next: Option<u64> = None;
    while seen < frames {
        let msg = tokio::time::timeout(Duration::from_secs(30), ws.next())
            .await
            .expect("the stream kept coming")
            .expect("open")
            .expect("frame");
        match msg {
            Message::Text(text) => {
                let v: Value = serde_json::from_str(&text).expect("json");
                if v.get("method").and_then(Value::as_str) == Some("stream.drop") {
                    drops += 1;
                }
            }
            Message::Binary(bytes) => {
                let frame = Frame::from_bytes(bytes.to_vec()).expect("frame");
                let h = frame.header().expect("header");
                if !MsgType::from_id(h.msg_type).is_some_and(MsgType::is_canonical) {
                    continue;
                }
                if let Some(expected) = next
                    && h.seq != expected
                {
                    gaps += 1;
                }
                next = Some(h.seq + 1);
                seen += 1;
                // A reader slower than an unthrottled producer.
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    server.stop().await;
    (drops, gaps)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_synchronised_run_sheds_nothing_for_a_slow_reader() {
    let (free_drops, _) = slow_read("free", 600).await;
    assert!(
        free_drops > 0,
        "the control: an unthrottled free run outruns a slow reader and says so"
    );
    let (drops, gaps) = slow_read("client", 600).await;
    assert_eq!(drops, 0, "a client-synchronised run dropped frames");
    assert_eq!(gaps, 0, "and the seq is dense");
}
