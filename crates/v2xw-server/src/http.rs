//! The HTTP surface of §1.1 and the WebSocket connection task.
//!
//! One axum router carries all of it: the `/vwp/v1` upgrade, the world endpoint of §4, the
//! one-shot JSON-RPC of §6.2, the OpenRPC document of §6.3 and `/healthz`. Every response
//! — including the upgrade — carries the three cross-origin isolation headers 09-ui §2
//! needs for `SharedArrayBuffer`, because a missing one there is what makes the Studio's
//! shared pose rings silently unavailable (conformance W2).
//!
//! # This is the one module that reads a clock
//!
//! §1.2 requires a Ping every 15 s of *wall* time and a close after 30 s without a Pong;
//! §1.5 requires a stall timeout in wall seconds; live pacing is by definition a
//! wall-clock schedule. None of that is engine-facing — no simulated quantity is derived
//! from it and no recorded byte depends on it — so the wall clock is read here and nowhere
//! else in the crate. Everything below the transport uses [`v2xw_core::time::SimTime`].

use std::sync::Arc;
use std::time::{Duration as WallDuration, Instant};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use v2xw_record::wire::hello::HELLO_LIVE;
use v2xw_record::wire::{Frame, MsgType};

use crate::engine::RunState;
use crate::error::{Result, ServerError};
use crate::rpc;
use crate::run::Run;
use crate::session::{ConnectParams, Session};

/// The WebSocket subprotocol token of §1.1. Carries the major version so a mismatched
/// client fails at the handshake rather than at the first frame.
pub const SUBPROTOCOL: &str = "vwp.v1";
/// The `Content-Type` of the binary world payload (§4).
pub const WORLD_CONTENT_TYPE: &str = "application/vnd.v2xw.world.v1";
/// §1.2's ping interval.
pub const PING_INTERVAL: WallDuration = WallDuration::from_secs(15);
/// §1.2's pong deadline; the connection closes with 1001 after it.
pub const PONG_DEADLINE: WallDuration = WallDuration::from_secs(30);

/// Shared server state.
#[derive(Clone)]
pub struct AppState {
    /// The run this process serves.
    pub run: Arc<Run>,
    /// The bearer token a non-loopback bind requires (02-architecture §12).
    pub token: Option<Arc<String>>,
    /// The sessions this server holds, attached or parked for a resume (§1.4).
    pub sessions: Arc<crate::resume::Sessions>,
}

/// The three cross-origin isolation headers of §1.1, plus the immutability headers for a
/// content-addressed response.
fn isolation_headers(immutable: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let put = |headers: &mut HeaderMap, name: &'static str, value: &str| {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    };
    put(&mut headers, "cross-origin-opener-policy", "same-origin");
    put(&mut headers, "cross-origin-embedder-policy", "require-corp");
    put(&mut headers, "cross-origin-resource-policy", "same-origin");
    if let Some(hash) = immutable {
        put(
            &mut headers,
            "cache-control",
            "public, max-age=31536000, immutable",
        );
        put(&mut headers, "etag", &format!("\"{hash}\""));
    }
    headers
}

/// Checks the bearer token when one is configured (§1.1).
///
/// A missing or wrong token is HTTP 401, never a WebSocket close, so the browser sees a
/// real status. The error variant is an already-built HTTP response rather than a small
/// code because there is exactly one thing to do with it — return it — and boxing it
/// would buy nothing on a path that runs once per request.
#[allow(clippy::result_large_err)]
fn authorize(state: &AppState, headers: &HeaderMap) -> core::result::Result<(), Response> {
    let Some(expected) = &state.token else {
        return Ok(());
    };
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if given == Some(expected.as_str()) {
        return Ok(());
    }
    Err((
        StatusCode::UNAUTHORIZED,
        isolation_headers(None),
        axum::Json(json!({"error": {"code": -32041, "message": "unauthorized"}})),
    )
        .into_response())
}

/// Builds the router of §1.1.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/vwp/v1", get(upgrade))
        .route("/world/{file}", get(world))
        .route("/rpc", post(rpc_http))
        .route("/rpc/schema", get(schema))
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers) {
        return response;
    }
    (
        StatusCode::OK,
        isolation_headers(None),
        axum::Json(json!({
            "ok": true,
            "engine": concat!("v2xw-server ", env!("CARGO_PKG_VERSION")),
            "runs": [state.run.descriptor().run_id.clone()],
        })),
    )
        .into_response()
}

async fn schema(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers) {
        return response;
    }
    (
        StatusCode::OK,
        isolation_headers(None),
        axum::Json(crate::openrpc::document(None)),
    )
        .into_response()
}

async fn world(
    State(state): State<AppState>,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers) {
        return response;
    }
    let expected = state.run.world().content_hash_hex();
    let (hash, extension) = match file.rsplit_once('.') {
        Some(pair) => pair,
        None => {
            return (StatusCode::NOT_FOUND, isolation_headers(None)).into_response();
        }
    };
    if hash != expected {
        // §4: the URL is the content address. A different hash is a different world, and
        // this server has one.
        return (
            StatusCode::NOT_FOUND,
            isolation_headers(None),
            axum::Json(json!({"error": {"code": -32005,
                                        "message": format!("unknown world hash {hash}")}})),
        )
            .into_response();
    }
    let mut response_headers = isolation_headers(Some(hash));
    match extension {
        "vwb" => {
            response_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(WORLD_CONTENT_TYPE),
            );
            (
                StatusCode::OK,
                response_headers,
                state.run.world().bytes.clone(),
            )
                .into_response()
        }
        "json" => {
            response_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            (
                StatusCode::OK,
                response_headers,
                state.run.world_json().to_string(),
            )
                .into_response()
        }
        _ => (StatusCode::NOT_FOUND, isolation_headers(None)).into_response(),
    }
}

async fn rpc_http(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    if let Err(response) = authorize(&state, &headers) {
        return response;
    }
    let response = match rpc::parse(&body) {
        Err(e) => rpc::failure(&Value::Null, &e),
        Ok(request) => {
            let id = request.id.clone().unwrap_or(Value::Null);
            let mut ctx = rpc::Context {
                run: &state.run,
                session: None,
                pending: None,
                received_at: None,
            };
            match rpc::dispatch(&mut ctx, &request) {
                Ok(outcome) => rpc::success(&id, outcome.result),
                Err(e) => rpc::failure(&id, &e),
            }
        }
    };
    (
        StatusCode::OK,
        isolation_headers(None),
        axum::Json(response),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct UpgradeQuery {
    #[serde(default)]
    v: Option<String>,
}

async fn upgrade(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
    RawQuery(query): RawQuery,
    Query(parsed): Query<UpgradeQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers) {
        return response;
    }
    // §1.1: a server that does not implement `vwp.v1` fails the upgrade with 426. The
    // mirror of that is that a client offering a *different* token gets 426 too, which is
    // conformance N2.
    if let Some(offered) = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
    {
        if !offered.split(',').map(str::trim).any(|t| t == SUBPROTOCOL) {
            return (
                StatusCode::UPGRADE_REQUIRED,
                isolation_headers(None),
                format!("this endpoint speaks `{SUBPROTOCOL}` only"),
            )
                .into_response();
        }
    }
    if let Some(v) = parsed.v.as_deref() {
        if v != "1" {
            // §1.3 rule 5 wants Error + Bye + close 4406 for a version this server cannot
            // serve, but a *malformed* version on the query string never gets that far:
            // there is no socket to send a frame on yet.
            return (
                StatusCode::UPGRADE_REQUIRED,
                isolation_headers(None),
                format!("unsupported protocol major version `{v}`"),
            )
                .into_response();
        }
    }
    let query = query.unwrap_or_default();
    let mut response = ws
        .protocols([SUBPROTOCOL])
        .on_upgrade(move |socket| connection(socket, state, query));
    for (name, value) in isolation_headers(None).iter() {
        response.headers_mut().insert(name.clone(), value.clone());
    }
    response
}

/// How a connection's loop ended, which decides what becomes of its session.
enum Exit {
    /// The socket went away without saying goodbye: park the session for a resume (§1.4).
    Park,
    /// The client closed cleanly (1000) or the server ended the connection on purpose:
    /// nobody will resume this session.
    Forget,
    /// Another connection presented this session's token: hand it over (§1.4 supersede).
    Handover(crate::resume::Claim),
}

/// The session a new connection starts with: a retained one it named, or a new one.
struct Attached {
    session: Session,
    steps: tokio::sync::broadcast::Receiver<Arc<crate::engine::StepOutput>>,
    notices: tokio::sync::broadcast::Receiver<Value>,
    claims: tokio::sync::mpsc::Receiver<crate::resume::Claim>,
    /// The frames to send before the loop starts: `Hello`, then a replay or a keyframe.
    greeting: Vec<Frame>,
}

/// Finds or makes the session for a new connection and builds its greeting.
///
/// A token that names a retained session with the same profile takes it over, and the
/// greeting is §1.4 rule 1 when the ring still covers `?resume=` (a resumed `Hello` and the
/// missed frames, nothing re-encoded) or rule 2 when it does not (a fresh `Hello` and a
/// resync keyframe). If the run moved to a new generation while the client was away, the
/// client has not seen that run's `Hello`, so it gets one — never a resume across runs.
async fn attach(state: &AppState, params: ConnectParams) -> Result<Attached> {
    let claimed = match params.session.as_deref() {
        Some(token) => state.sessions.claim(token).await,
        None => None,
    };
    if let Some(d) = claimed {
        if d.session.accepts(&params) {
            let crate::resume::Detached {
                mut session,
                steps,
                notices,
                claims,
                client_generation,
            } = *d;
            let generation = state.run.generation();
            let greeting = if session.generation() != generation {
                // The run moved on while nobody was attached; the parked task follows
                // steps, not generations, so it may still be on the previous one.
                session.reattach(None);
                session.regreet(&state.run)?
            } else {
                let resumable = client_generation == session.generation();
                session.reattach(if resumable { params.resume } else { None });
                let descriptor = state.run.descriptor();
                let mut frames = session.greet(&state.run, &descriptor)?;
                if session.resumed() {
                    // Hello first, then the ring from `?resume=`: the frames this client
                    // missed, with the `seq` it would have seen them under.
                    let backlog = session.resume_backlog();
                    frames.splice(1..1, backlog);
                }
                frames
            };
            return Ok(Attached {
                session,
                steps,
                notices,
                claims,
                greeting,
            });
        }
        // A different profile is a different stream (§5.3): the retained session stays
        // retained for a client that asks for it as it was.
        state.sessions.park(Arc::clone(&state.run), d);
    }

    // Subscribed before the generation is read, so a run started between the two is seen
    // as a change rather than missed.
    let notices = state.run.subscribe_notices();
    let steps = state.run.subscribe();
    let generation = state.run.generation();
    let descriptor = state.run.descriptor();
    let mut params = params;
    // `?resume=` without a session this server holds names a `seq` of nothing (§1.4 rule 2).
    params.resume = None;
    let mut session = Session::new(params, &descriptor);
    let token = state.sessions.mint();
    let claims = state.sessions.register(&token);
    session.bind(generation, &token);
    // §1.3: the server sends exactly one Hello immediately, before anything else. A live
    // run's node table is the set it has *now*, not the set it had when the server bound
    // (§3.1.3), so it is read here per connection. A run that is not moving also gets the
    // state at its position, so a page that attaches to a paused or finished run — a
    // reload, a second tab, a reconnect after the engine restarted — shows the city as it
    // stands instead of nothing.
    let greeting = session.greet(&state.run, &descriptor)?;
    Ok(Attached {
        session,
        steps,
        notices,
        claims,
        greeting,
    })
}

/// One connection's whole life (§1.3 onwards).
async fn connection(mut socket: WebSocket, state: AppState, query: String) {
    let params = match ConnectParams::parse(&query) {
        Ok(p) => p,
        Err(e) => {
            let _ = send_error_and_bye(&mut socket, &e, 3, 1002).await;
            return;
        }
    };
    if params.version != 1 {
        let e = ServerError::UnsupportedVersion;
        let _ = send_error_and_bye(&mut socket, &e, 3, 4406).await;
        return;
    }
    if let Some(run) = &params.run {
        if !state.run.matches(run) {
            // §1.4 rule 3: unknown run is Error(-32000) then Bye{reason=3}, close 4404.
            let e = ServerError::RunNotFound(run.clone());
            let _ = send_error_and_bye(&mut socket, &e, 3, 4404).await;
            return;
        }
    }

    let mut generations = state.run.watch_generation();
    generations.borrow_and_update();
    let Attached {
        mut session,
        mut steps,
        mut notices,
        mut claims,
        greeting,
    } = match attach(&state, params).await {
        Ok(a) => a,
        Err(e) => {
            let _ = send_error_and_bye(&mut socket, &e, 3, 1011).await;
            return;
        }
    };
    let greeted_generation = session.generation();
    let exit = run_connection(
        &mut socket,
        &state,
        &mut session,
        &mut steps,
        &mut notices,
        &mut claims,
        &mut generations,
        greeting,
    )
    .await;
    // Every regreet on this connection was written before the loop went on, so the last
    // `Hello` the client got is the one for the session's generation — unless the socket
    // died inside a regreet, in which case it is the one the connection started with.
    let client_generation = if session.greeted() {
        session.generation()
    } else {
        greeted_generation
    };
    let detached = Box::new(crate::resume::Detached {
        session,
        steps,
        notices,
        claims,
        client_generation,
    });
    match exit {
        Exit::Park => state.sessions.park(Arc::clone(&state.run), detached),
        Exit::Forget => state.sessions.forget(detached.session.session_token()),
        Exit::Handover(reply) => {
            // §1.4: "the server sends `Bye{reason = 4 (superseded)}` on the old socket and
            // closes it with 1012". Said before the hand-over, so the old page's last word
            // from the server is why it lost its stream.
            if let Ok(frame) = bye_frame(state.run.sim_time(), 0, 4, "superseded") {
                let _ = socket
                    .send(Message::Binary(frame.into_bytes().into()))
                    .await;
            }
            let _ = close(&mut socket, 1012, "superseded by a resumed connection").await;
            if let Err(back) = reply.send(detached) {
                // The claimant gave up waiting; the session is still resumable.
                state.sessions.park(Arc::clone(&state.run), back);
            }
        }
    }
}

/// The connection loop, from the greeting to whatever ends it.
#[allow(clippy::too_many_arguments)]
async fn run_connection(
    socket: &mut WebSocket,
    state: &AppState,
    session: &mut Session,
    steps: &mut tokio::sync::broadcast::Receiver<Arc<crate::engine::StepOutput>>,
    notices: &mut tokio::sync::broadcast::Receiver<Value>,
    claims: &mut tokio::sync::mpsc::Receiver<crate::resume::Claim>,
    generations: &mut tokio::sync::watch::Receiver<u64>,
    greeting: Vec<Frame>,
) -> Exit {
    for frame in greeting {
        if socket
            .send(Message::Binary(frame.into_bytes().into()))
            .await
            .is_err()
        {
            return Exit::Park;
        }
    }

    // A finished run is not a reason to hang up. This server can seek back through it
    // and start another run on the same connection (§6.6: `run.start` sends "a fresh
    // `Hello` on this connection"), so the connection stays open until the client or the
    // process ends it. See `docs/protocol/vwp-v1.md` §1.4 on `Bye{reason = 0}`.

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = Instant::now();

    loop {
        tokio::select! {
            biased;
            claim = claims.recv() => {
                match claim {
                    Some(reply) => return Exit::Handover(reply),
                    // The registry forgot this session (the server is going away).
                    None => return Exit::Forget,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) => return Exit::Park,
                    Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                    Some(Ok(Message::Ping(_))) => { /* axum answers automatically */ }
                    Some(Ok(Message::Close(frame))) => {
                        // 1000 is "I am done"; anything else — 1001 on a navigation, a
                        // proxy's 1006 — may be followed by a reconnect.
                        return if frame.as_ref().is_some_and(|f| f.code == 1000) {
                            Exit::Forget
                        } else {
                            Exit::Park
                        };
                    }
                    Some(Ok(Message::Binary(_))) => {
                        // §1.2: a conforming client never sends binary in v1; the server
                        // replies with an Error frame and may close with 1003.
                        let e = ServerError::InvalidRequest(
                            "client-to-server binary frames do not exist in v1".to_string(),
                        );
                        let _ = send_error_and_bye(socket, &e, 3, 1003).await;
                        return Exit::Forget;
                    }
                    Some(Ok(Message::Text(text))) => {
                        match handle_text(socket, state, session, steps, &text).await {
                            TextOutcome::Continue => {}
                            TextOutcome::Gone => return Exit::Park,
                            TextOutcome::Ended => return Exit::Forget,
                        }
                    }
                }
            }
            changed = generations.changed() => {
                if changed.is_err() {
                    return Exit::Forget;
                }
                let current = *generations.borrow_and_update();
                if current != session.generation()
                    && !regreet(socket, state, session).await
                {
                    return Exit::Park;
                }
            }
            notice = notices.recv() => {
                match notice {
                    Ok(notice) => {
                        if socket
                            .send(Message::Text(notice.to_string().into()))
                            .await
                            .is_err()
                        {
                            return Exit::Park;
                        }
                    }
                    // A notification is advisory state, and `run.status` is the
                    // authoritative answer; a connection that missed some carries on.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Exit::Forget,
                }
            }
            step = steps.recv() => {
                match step {
                    Ok(output) => {
                        if output.generation < session.generation() {
                            // A step the previous run produced before it was replaced.
                            continue;
                        }
                        if output.generation > session.generation()
                            && !regreet(socket, state, session).await
                        {
                            return Exit::Park;
                        }
                        let effects = match session.encode_step(&output) {
                            Ok(e) => e,
                            Err(e) => {
                                // Said, not swallowed: a connection that ends here with no
                                // frame looked, from the page, like the engine vanishing.
                                tracing::error!(error = %e, "a step could not be encoded");
                                let _ = send_error_and_bye(socket, &e, 3, 1011).await;
                                return Exit::Forget;
                            }
                        };
                        if effects.fatal {
                            let _ = close(socket, 1011, "send queue overflow").await;
                            return Exit::Forget;
                        }
                        for frame in effects.frames {
                            if socket
                                .send(Message::Binary(frame.into_bytes().into()))
                                .await
                                .is_err()
                            {
                                return Exit::Park;
                            }
                        }
                        if let Some((first, last, counts, resync)) = effects.drop_notice {
                            let notice = rpc::notification(
                                "stream.drop",
                                json!({"seq_first": first, "seq_last": last,
                                       "dropped": {"delta": counts.delta,
                                                   "event": counts.event,
                                                   "telemetry": counts.telemetry,
                                                   "metric": counts.metric},
                                       "resync_seq": resync}),
                            );
                            if socket
                                .send(Message::Text(notice.to_string().into()))
                                .await
                                .is_err()
                            {
                                return Exit::Park;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // §1.5: the connection fell behind. A resync keyframe is owed, and
                        // the gap is reported as a notification so the binary stream stays
                        // byte-identical between live and replay (§7.2).
                        session.request_resync();
                        // `n` is the number of *steps* the broadcast dropped, not the
                        // number of frames in them: the frames were never encoded, so
                        // their count is not knowable here. It is reported under `delta`
                        // because a step always carries one snapshot frame and that is the
                        // one whose loss matters — the client must not apply the next delta
                        // against a stale base. §1.5's own rule covers the rest: the client
                        // "MUST treat a `seq` gap as a drop even if the notification is
                        // lost", so the counts are advisory and the gap is authoritative.
                        let notice = rpc::notification(
                            "stream.drop",
                            json!({"seq_first": session.next_seq(),
                                   "seq_last": session.next_seq() + n,
                                   "dropped": {"delta": n, "event": 0,
                                               "telemetry": 0, "metric": 0},
                                   "resync_seq": session.next_seq()}),
                        );
                        if socket
                            .send(Message::Text(notice.to_string().into()))
                            .await
                            .is_err()
                        {
                            return Exit::Park;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Exit::Forget,
                }
            }
            _ = ping.tick() => {
                if last_pong.elapsed() > PONG_DEADLINE {
                    let _ = close(socket, 1001, "no pong within 30 s").await;
                    return Exit::Park;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return Exit::Park;
                }
            }
        }
    }
}

/// Moves a connection onto the run's new generation: a fresh `Hello`, then the state at
/// the new run's position when it is not moving. Returns `false` when the socket is gone.
async fn regreet(socket: &mut WebSocket, state: &AppState, session: &mut Session) -> bool {
    let frames = match session.regreet(&state.run) {
        Ok(frames) => frames,
        Err(e) => {
            let _ = send_error_and_bye(socket, &e, 3, 1011).await;
            return false;
        }
    };
    for frame in frames {
        if socket
            .send(Message::Binary(frame.into_bytes().into()))
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

/// What handling one text frame left the connection in.
enum TextOutcome {
    /// Carry on.
    Continue,
    /// The socket failed under a write: the client may come back.
    Gone,
    /// The connection was ended on purpose (`run.stop` with its `Bye`).
    Ended,
}

/// Handles one text frame.
async fn handle_text(
    socket: &mut WebSocket,
    state: &AppState,
    session: &mut Session,
    steps: &mut tokio::sync::broadcast::Receiver<Arc<crate::engine::StepOutput>>,
    text: &str,
) -> TextOutcome {
    let sent = |ok: bool| {
        if ok {
            TextOutcome::Continue
        } else {
            TextOutcome::Gone
        }
    };
    let request = match rpc::parse(text) {
        Ok(r) => r,
        Err(e) => {
            let response = rpc::failure(&Value::Null, &e);
            return sent(
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .is_ok(),
            );
        }
    };
    let id = request.id.clone();
    let received_at = Instant::now();
    if request.method == "run.seek"
        && let Some(target) = seek_target(&state.run, &request.params)
        && !compute_ahead(socket, state, target).await
    {
        return TextOutcome::Gone;
    }
    let mut ctx = rpc::Context {
        run: &state.run,
        session: Some(session),
        pending: Some(steps),
        received_at: Some(received_at),
    };
    let outcome = rpc::dispatch(&mut ctx, &request);
    match outcome {
        Ok(outcome) => {
            // The ordering guarantees of §6.6: frames first, then the reply.
            for frame in outcome.pre_frames {
                if socket
                    .send(Message::Binary(frame.into_bytes().into()))
                    .await
                    .is_err()
                {
                    return TextOutcome::Gone;
                }
            }
            if let Some(id) = id {
                let response = rpc::success(&id, outcome.result);
                if socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .is_err()
                {
                    return TextOutcome::Gone;
                }
            }
            for notice in outcome.notifications {
                if socket
                    .send(Message::Text(notice.to_string().into()))
                    .await
                    .is_err()
                {
                    return TextOutcome::Gone;
                }
            }
            if outcome.bye {
                let _ = send_bye(socket, state, 1, "run.stop").await;
                let _ = close(socket, 1000, "client requested stop").await;
                return TextOutcome::Ended;
            }
            // Steps that arrived while this call ran are the calling connection's to
            // encode against the generation it is on now, which the call may have moved.
            if session.generation() != state.run.generation() {
                match session.regreet(&state.run) {
                    Ok(frames) => {
                        for frame in frames {
                            if socket
                                .send(Message::Binary(frame.into_bytes().into()))
                                .await
                                .is_err()
                            {
                                return TextOutcome::Gone;
                            }
                        }
                    }
                    Err(_) => return TextOutcome::Gone,
                }
            }
            TextOutcome::Continue
        }
        Err(e) => {
            // §6.1: a notification never gets a reply, not even an error one.
            match id {
                None => TextOutcome::Continue,
                Some(id) => {
                    let response = rpc::failure(&id, &e);
                    sent(
                        socket
                            .send(Message::Text(response.to_string().into()))
                            .await
                            .is_ok(),
                    )
                }
            }
        }
    }
}

/// Where a `run.seek` asks to go, when that is past what the run has produced but inside
/// the run: the case [`compute_ahead`] exists for. `None` for anything the seek itself will
/// answer — a target already seekable, one outside the run, a malformed request.
fn seek_target(run: &Run, params: &serde_json::Map<String, Value>) -> Option<u64> {
    let horizon = run.horizon_ns();
    let target = match (params.get("t_ns"), params.get("fraction")) {
        (Some(t), _) => t.as_u64()?,
        (None, Some(f)) => {
            let f = f.as_f64()?;
            if !(0.0..=1.0).contains(&f) {
                return None;
            }
            (horizon as f64 * f) as u64
        }
        _ => return None,
    };
    let (_, max_ns) = run.seek_range();
    (target > max_ns && target <= horizon).then_some(target)
}

/// Runs the kernel forward until `target` is seekable, telling the client how far it is.
///
/// A live kernel keeps only a bounded lead on the stream (`LiveOptions::lookahead_steps`,
/// 12.8 s at the defaults once the channel is counted), so a seek past it used to be refused
/// with "the engine has only simulated up to …": the scrub bar could not reach most of a run
/// the user had not watched. The kernel is let run ahead in 100 ms slices, with the run lock
/// released between them so `run.status` and the producer keep going, and every slice sends
/// §6.14's `job.progress` so the page can show a progress bar rather than a frozen control.
/// The seek then lands exactly as one inside the range does. Returns `false` if the socket
/// went away meanwhile.
async fn compute_ahead(socket: &mut WebSocket, state: &AppState, target: u64) -> bool {
    const SLICE: WallDuration = WallDuration::from_millis(100);
    let job_id = format!("run.seek:{target}");
    let (_, start) = state.run.seek_range();
    let mut last = start;
    let mut stalled = 0u32;
    loop {
        let run = Arc::clone(&state.run);
        let reached = tokio::task::spawn_blocking(move || run.extend_to(target, SLICE))
            .await
            .unwrap_or(last);
        let progress = if target > start {
            ((reached.saturating_sub(start)) as f64 / (target - start) as f64).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let done = reached >= target;
        let notice = rpc::notification(
            "job.progress",
            json!({"job_id": job_id, "progress": progress,
                   "message": format!("simulating ahead to {:.1} s: at {:.1} s",
                                      target as f64 / 1e9, reached as f64 / 1e9),
                   "t_ns": reached, "target_ns": target}),
        );
        if socket
            .send(Message::Text(notice.to_string().into()))
            .await
            .is_err()
        {
            return false;
        }
        if done {
            break;
        }
        // A kernel that has ended, or failed, will not get further; the seek then answers
        // with the range it has, which is -32003 and says so.
        stalled = if reached == last { stalled + 1 } else { 0 };
        if stalled >= 50 {
            break;
        }
        last = reached;
    }
    let done = rpc::notification(
        "job.done",
        json!({"job_id": job_id, "state": "done", "outputs": []}),
    );
    socket
        .send(Message::Text(done.to_string().into()))
        .await
        .is_ok()
}

/// Sends an `Error` frame, a `Bye` and closes (§3.10, §3.11).
async fn send_error_and_bye(
    socket: &mut WebSocket,
    error: &ServerError,
    reason: u8,
    close_code: u16,
) -> Result<()> {
    let frame = error_frame(error, true)?;
    let _ = socket
        .send(Message::Binary(frame.into_bytes().into()))
        .await;
    let frame = bye_frame(0, 0, reason, &error.to_string())?;
    let _ = socket
        .send(Message::Binary(frame.into_bytes().into()))
        .await;
    let _ = close(socket, close_code, "").await;
    Ok(())
}

async fn send_bye(
    socket: &mut WebSocket,
    state: &AppState,
    reason: u8,
    detail: &str,
) -> Result<()> {
    let sim_time = state.run.sim_time();
    let frame = bye_frame(sim_time, 0, reason, detail)?;
    let _ = socket
        .send(Message::Binary(frame.into_bytes().into()))
        .await;
    Ok(())
}

async fn close(socket: &mut WebSocket, code: u16, reason: &str) -> core::result::Result<(), ()> {
    socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.to_string().into(),
        })))
        .await
        .map_err(|_| ())
}

/// Encodes an `Error` frame (§3.10).
pub fn error_frame(error: &ServerError, fatal: bool) -> Result<Frame> {
    let mut strings = v2xw_record::wire::StrTable::new();
    let message = strings.intern(&error.to_string());
    let detail = strings.intern(&error.data().map(|d| d.to_string()).unwrap_or_default());
    let table = strings.encode();
    let mut body = vec![0u8; 32 + table.len()];
    body[8..12].copy_from_slice(&error.code().to_le_bytes());
    body[12..16].copy_from_slice(&32u32.to_le_bytes());
    body[16..20].copy_from_slice(&message.to_le_bytes());
    body[20..24].copy_from_slice(&detail.to_le_bytes());
    body[24] = u8::from(fatal);
    body[32..].copy_from_slice(&table);
    Ok(Frame::new(MsgType::Error, 0, 0, &body)?)
}

/// Encodes a `Bye` frame (§3.11).
pub fn bye_frame(
    sim_time_ns: u64,
    canonical_frames: u64,
    reason: u8,
    detail: &str,
) -> Result<Frame> {
    let mut strings = v2xw_record::wire::StrTable::new();
    let detail_id = strings.intern(detail);
    let table = strings.encode();
    let mut body = vec![0u8; 32 + table.len()];
    body[0..8].copy_from_slice(&sim_time_ns.to_le_bytes());
    body[8..16].copy_from_slice(&canonical_frames.to_le_bytes());
    body[16] = reason;
    body[20..24].copy_from_slice(&32u32.to_le_bytes());
    body[24..28].copy_from_slice(&detail_id.to_le_bytes());
    body[32..].copy_from_slice(&table);
    Ok(Frame::new(MsgType::Bye, 0, 0, &body)?)
}

/// How many steps a connection may fall behind before a client-synchronised producer waits.
///
/// Well under the broadcast capacity (`run::STEP_CHANNEL_CAPACITY`, 64), so a paced run never
/// reaches the lag that forces a resync.
pub const SYNC_HIGH_WATER_STEPS: usize = 8;

/// The producer task: advances the run and paces it against wall time (§1.5).
///
/// `speed` is a multiple of real time and `0` means unthrottled. The loop never blocks on
/// a socket, which is §1.5's first requirement; a connection that cannot keep up sheds
/// load in its own queue and is told it lagged.
pub async fn producer(run: Arc<Run>) {
    let step = run.descriptor().cadence.mobility_step;
    loop {
        let (speed, client_sync) = run.speed();
        if run.state() != RunState::Running {
            tokio::time::sleep(WallDuration::from_millis(5)).await;
            continue;
        }
        // §1.5 "Live pacing": with `sync: "client"` the producer waits while any connection
        // is more than a few steps behind, so no connection ever lags and nothing is shed.
        // The option was accepted and reported by `run.status` and did nothing: the producer
        // read the speed and ignored the mode, so a "lossless" demo at speed 0 dropped
        // deltas like any other.
        if client_sync && run.step_backlog() >= SYNC_HIGH_WATER_STEPS {
            tokio::time::sleep(WallDuration::from_millis(2)).await;
            continue;
        }
        match run.tick() {
            Ok(true) => {}
            Ok(false) => {
                tokio::time::sleep(WallDuration::from_millis(50)).await;
                continue;
            }
            Err(e) => {
                tracing::error!(error = %e, "the engine aborted");
                return;
            }
        }
        if speed > 0.0 {
            let wall_ns = (step.as_nanos() as f64) / speed;
            tokio::time::sleep(WallDuration::from_nanos(wall_ns as u64)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
}

/// True when the run's `Hello` says the stream is live, for a caller deciding whether to
/// start a producer task at all.
pub fn is_live(run: &Run) -> bool {
    run.descriptor().hello.hello_flags & HELLO_LIVE != 0 || run.descriptor().live
}
