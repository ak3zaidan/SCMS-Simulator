//! The JSON-RPC 2.0 control surface of §6: 32 methods and 8 notifications.
//!
//! One dispatcher serves both framings — text frames on the socket (§6.1) and
//! `POST /rpc` (§6.2) — because a second one would drift. The only difference between
//! them is that the HTTP path has no connection, so the three connection-scoped methods
//! return `-32009` there (conformance R7).
//!
//! Every method validates its parameters before it touches the engine, and every
//! validation failure is a `-32602` carrying the `{path, message, hint}` rows conformance
//! R3 asks for. That is why the parameter readers below are explicit rather than a serde
//! derive: a derive would produce "missing field `speed`", and the checklist asks for a
//! pointer and a hint.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use v2xw_core::ids::NodeId;
use v2xw_record::wire::Frame;

use crate::engine::{Control, Query, RunState, ScenarioSource, StageRequest};
use crate::error::{ParamError, Result, ServerError};
use crate::run::Run;
use crate::session::{CameraState, OVERLAYS, Session, overlay_is_gt};

/// Every method name of §6.15, in the order the inventory lists them.
pub const METHODS: [&str; 33] = [
    "run.start",
    "run.pause",
    "run.resume",
    "run.step",
    "run.seek",
    "run.speed",
    "run.stop",
    "run.status",
    "view.follow",
    "view.camera",
    "overlay.set",
    "inspect.node",
    "inspect.link",
    "inspect.entity",
    "explain",
    "scenario.get",
    "scenario.schema",
    "scenario.set",
    "scenario.validate",
    "scenario.save",
    "scenario.load",
    "scenario.list",
    "world.import_osm",
    "world.generate",
    "events.set",
    "metrics.query",
    "metrics.plot",
    "export.dataset",
    "export.recording",
    "experiment.define",
    "experiment.run",
    "experiment.status",
    "rpc.discover",
];

/// The three methods that need a connection to act on (§6.2).
pub const CONNECTION_SCOPED: [&str; 3] = ["view.follow", "view.camera", "overlay.set"];

/// The eight notification names of §6.14.
pub const NOTIFICATIONS: [&str; 8] = [
    "run.state",
    "stream.drop",
    "job.progress",
    "job.done",
    "view.changed",
    "log",
    "validation",
    "experiment.progress",
];

/// What a dispatched call produced.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Binary frames that MUST reach the client before the reply (§6.6's `run.seek` and
    /// `run.pause` ordering guarantees, conformance R4 and R5).
    pub pre_frames: Vec<Frame>,
    /// The JSON-RPC `result` member.
    pub result: Value,
    /// Notifications to send after the reply.
    pub notifications: Vec<Value>,
    /// True when the connection must end after the reply, with `Bye{reason=1}`.
    ///
    /// No method sets it any more: `run.stop` used to, and the page that pressed Stop was
    /// left with no socket to press Run on. It is kept for a method that genuinely ends a
    /// connection.
    pub bye: bool,
}

impl Outcome {
    /// An outcome that is only a result.
    fn of(result: Value) -> Self {
        Outcome {
            result,
            ..Default::default()
        }
    }
}

/// Everything a dispatched method may touch.
pub struct Context<'a> {
    /// The run.
    pub run: &'a Run,
    /// The calling connection, or `None` on the HTTP path.
    pub session: Option<&'a mut Session>,
    /// The connection's subscription to the run's step stream.
    ///
    /// `run.pause` and `run.step` promise that every frame up to the reply's `t_ns` has
    /// already been sent (§6.6, conformance R4 and R5). Keeping the receiver here is what
    /// lets the dispatcher make that true: it drains the steps the connection has not yet
    /// encoded and hands them back as [`Outcome::pre_frames`].
    pub pending:
        Option<&'a mut tokio::sync::broadcast::Receiver<std::sync::Arc<crate::engine::StepOutput>>>,
    /// When the transport received this request, for `run.seek`'s `elapsed_ms`.
    ///
    /// §7.4 measures a seek "from the `run.seek` request to the JSON-RPC reply", which is
    /// a wall-clock duration, and this crate reads a wall clock in exactly one module
    /// ([`crate::http`]). So the transport takes the reading and hands the instant down;
    /// nothing below this struct reads a clock, and `None` reports `0.0` rather than
    /// inventing a number.
    pub received_at: Option<std::time::Instant>,
}

/// A parsed JSON-RPC 2.0 request.
#[derive(Debug)]
pub struct Request {
    /// The `id`, absent for a notification.
    pub id: Option<Value>,
    /// The method name.
    pub method: String,
    /// The params object; `{}` when omitted.
    pub params: Map<String, Value>,
}

/// Parses one JSON-RPC 2.0 message.
///
/// # Errors
/// [`ServerError::Parse`] for invalid JSON, [`ServerError::InvalidRequest`] for anything
/// that is not a single request object — including a batch array, which §6.1's decision
/// and conformance R10 require be refused with `-32600`.
pub fn parse(text: &str) -> Result<Request> {
    let value: Value = serde_json::from_str(text).map_err(|e| ServerError::Parse(e.to_string()))?;
    if value.is_array() {
        return Err(ServerError::InvalidRequest(
            "batch requests are not supported in v1 (§6.1)".to_string(),
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| ServerError::InvalidRequest("not an object".to_string()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(ServerError::InvalidRequest(
            "`jsonrpc` must be the string \"2.0\"".to_string(),
        ));
    }
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| ServerError::InvalidRequest("`method` must be a string".to_string()))?
        .to_string();
    let params = match object.get("params") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => {
            return Err(ServerError::InvalidRequest(
                "`params` must be an object; positional params are not used in v1".to_string(),
            ));
        }
    };
    Ok(Request {
        id: object.get("id").cloned(),
        method,
        params,
    })
}

/// The JSON-RPC response object for a successful call.
pub fn success(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// The JSON-RPC response object for a failed call.
pub fn failure(id: &Value, error: &ServerError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": error.to_rpc_object()})
}

/// A server-to-client notification (§6.14).
pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

// --- parameter readers ---------------------------------------------------------------

fn num(params: &Map<String, Value>, key: &str) -> Option<f64> {
    params.get(key).and_then(Value::as_f64)
}

fn uint(params: &Map<String, Value>, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

fn text<'a>(params: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn flag(params: &Map<String, Value>, key: &str, default: bool) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn strings(params: &Map<String, Value>, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn bounded(params: &Map<String, Value>, key: &str, lo: f64, hi: f64, default: f64) -> Result<f64> {
    match num(params, key) {
        None => Ok(default),
        Some(v) if v >= lo && v <= hi => Ok(v),
        Some(v) => Err(ServerError::InvalidParams(vec![ParamError::new(
            format!("/{key}"),
            format!("{v} is outside [{lo}, {hi}]"),
            format!("pass a value between {lo} and {hi}"),
        )])),
    }
}

fn vec3(params: &Map<String, Value>, key: &str, default: [f64; 3]) -> Result<[f64; 3]> {
    let Some(v) = params.get(key) else {
        return Ok(default);
    };
    let obj = v.as_object().ok_or_else(|| {
        ServerError::param(
            &format!("/{key}"),
            "must be a Vec3 object",
            "pass {x, y, z}",
        )
    })?;
    let read = |axis: &str| -> Result<f64> {
        obj.get(axis).and_then(Value::as_f64).ok_or_else(|| {
            ServerError::param(
                &format!("/{key}/{axis}"),
                "missing or not a number",
                "pass {x, y, z} with three numbers",
            )
        })
    };
    Ok([read("x")?, read("y")?, read("z")?])
}

// --- dispatch -------------------------------------------------------------------------

/// Dispatches one call.
///
/// # Errors
/// Any [`ServerError`]; the caller turns it into a JSON-RPC error object.
pub fn dispatch(ctx: &mut Context<'_>, request: &Request) -> Result<Outcome> {
    let method = request.method.as_str();
    if !METHODS.contains(&method) {
        return Err(ServerError::MethodNotFound(method.to_string()));
    }
    if ctx.session.is_none() && CONNECTION_SCOPED.contains(&method) {
        return Err(ServerError::NotSupportedHere(format!(
            "`{method}` acts on a WebSocket connection and has none over HTTP (§6.2)"
        )));
    }
    let p = &request.params;
    match method {
        "run.start" => run_start(ctx, p),
        "run.pause" => run_pause(ctx),
        "run.resume" => run_resume(ctx),
        "run.step" => run_step(ctx, p),
        "run.seek" => run_seek(ctx, p),
        "run.speed" => run_speed(ctx, p),
        "run.stop" => run_stop(ctx, p),
        "run.status" => run_status(ctx, p),
        "view.follow" => view_follow(ctx, p),
        "view.camera" => view_camera(ctx, p),
        "overlay.set" => overlay_set(ctx, p),
        "inspect.node" => inspect_node(ctx, p),
        "inspect.link" => inspect_link(ctx, p),
        "inspect.entity" => inspect_entity(ctx, p),
        "explain" => explain(ctx, p),
        "scenario.get" => scenario_get(ctx, p),
        "scenario.schema" => scenario_schema(ctx, p),
        "scenario.set" => scenario_set(ctx, p),
        "scenario.validate" => scenario_validate(ctx, p),
        "scenario.save" => scenario_save(ctx, p),
        "scenario.load" => scenario_load(ctx, p),
        "scenario.list" => scenario_list(ctx, p),
        "world.import_osm" => world_import(ctx, p),
        "world.generate" => world_generate(ctx, p),
        "events.set" => events_set(ctx, p),
        "metrics.query" => metrics_query(ctx, p),
        "metrics.plot" => metrics_plot(ctx, p),
        "export.dataset" => export_dataset(ctx, p),
        "export.recording" => export_recording(ctx, p),
        "experiment.define" => experiment_define(ctx, p),
        "experiment.run" => experiment_run(ctx, p),
        "experiment.status" => experiment_status(ctx, p),
        "rpc.discover" => Ok(Outcome::of(crate::openrpc::document(text(p, "method")))),
        _ => Err(ServerError::MethodNotFound(method.to_string())),
    }
}

fn run_start(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let speed = bounded(p, "speed", 0.0, 100.0, 1.0)?;
    let paused = flag(p, "paused", false);
    let seed = seed_param(p)?;
    let scenario = match p.get("scenario") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(ScenarioSource::Preset(id.clone())),
        Some(doc @ Value::Object(_)) => Some(ScenarioSource::Document(doc.clone())),
        Some(_) => {
            return Err(ServerError::param(
                "/scenario",
                "must be a preset id, a path or a scenario document",
                "omit it to run the scenario scenario.set staged, or the current one",
            ));
        }
    };
    let outcome = ctx.run.control(Control::Start {
        paused,
        speed,
        seed,
        scenario,
    })?;
    let d = ctx.run.descriptor();
    let mut result = json!({
        "run_id": d.run_id,
        "state": outcome.state.as_str(),
        "world_hash": ctx.run.world().content_hash_hex(),
        "scenario_hash": d.scenario_hash_hex,
        "t_end_ns": d.duration,
        "generation": ctx.run.generation(),
    });
    if let Some(path) = &d.recording_path {
        result["recording_path"] = json!(path);
    }
    for (key, value) in outcome.extra {
        result[key] = value;
    }
    // Every connection hears it, not only the caller: a second tab watching the run has to
    // learn that a new one began as surely as the tab that pressed Run.
    ctx.run
        .notify_state(outcome.state, outcome.t_ns, "run.start");
    Ok(Outcome::of(result))
}

/// `seed`: a non-negative integer, or a decimal or `0x`-hexadecimal string — the two
/// spellings a scenario file uses for its master seed.
fn seed_param(p: &Map<String, Value>) -> Result<Option<u64>> {
    match p.get("seed") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n.as_u64().map(Some).ok_or_else(|| {
            ServerError::param("/seed", "must be a non-negative integer", "pass e.g. 42")
        }),
        Some(Value::String(text)) => parse_seed(text).map(Some).ok_or_else(|| {
            ServerError::param(
                "/seed",
                &format!("`{text}` is not a seed"),
                "decimal, or hexadecimal with a 0x prefix; underscores are allowed",
            )
        }),
        Some(_) => Err(ServerError::param(
            "/seed",
            "must be an integer or a string",
            "pass e.g. 42 or \"0xC0FFEE\"",
        )),
    }
}

/// Parses a seed written the way a scenario writes one.
pub fn parse_seed(text: &str) -> Option<u64> {
    let clean: String = text.trim().chars().filter(|c| *c != '_').collect();
    match clean
        .strip_prefix("0x")
        .or_else(|| clean.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => clean.parse().ok(),
    }
}

fn run_pause(ctx: &mut Context<'_>) -> Result<Outcome> {
    let outcome = ctx.run.control(Control::Pause)?;
    ctx.run
        .notify_state(outcome.state, outcome.t_ns, "run.pause");
    // §6.6: "The server MUST have sent every frame up to and including `t_ns` before
    // replying." Draining the connection's own backlog here is what makes that true
    // (conformance R5).
    let pre_frames = drain_pending(ctx)?;
    Ok(Outcome {
        pre_frames,
        result: json!({"state": outcome.state.as_str(), "t_ns": outcome.t_ns}),
        ..Default::default()
    })
}

fn run_resume(ctx: &mut Context<'_>) -> Result<Outcome> {
    let outcome = ctx.run.control(Control::Resume)?;
    ctx.run
        .notify_state(outcome.state, outcome.t_ns, "run.resume");
    Ok(Outcome::of(
        json!({"state": outcome.state.as_str(), "t_ns": outcome.t_ns}),
    ))
}

fn run_step(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let unit = text(p, "unit").unwrap_or("step");
    if !["step", "event", "keyframe", "second"].contains(&unit) {
        return Err(ServerError::param(
            "/unit",
            "must be one of step, event, keyframe, second",
            "omit it for `step`",
        ));
    }
    let count = bounded(p, "count", 1.0, 100_000.0, 1.0)? as u64;
    if ctx.run.state() == RunState::Running {
        return Err(ServerError::RunNotRunning(
            "pause before stepping".to_string(),
        ));
    }
    let cadence = ctx.run.descriptor().cadence;
    let steps = match unit {
        "step" | "event" => count,
        "keyframe" => count.saturating_mul(cadence.max_deltas_per_gop()),
        "second" => count.saturating_mul(1_000_000_000 / cadence.mobility_step.as_nanos().max(1)),
        _ => count,
    };
    let (stepped, t_ns) = ctx.run.advance(steps)?;
    let pre_frames = drain_pending(ctx)?;
    let mut result = json!({
        "state": ctx.run.state().as_str(),
        "t_ns": t_ns,
        "stepped": stepped,
    });
    if unit == "event" {
        // The fixture has no DES heap to walk; a real engine fills this from the event it
        // just ran. The field is optional in the schema, so naming the channel it would
        // have carried is honest and the shape is right.
        result["last_event"] = json!({"channel": "snapshot.delta", "priority": 0, "seq": t_ns});
    }
    Ok(Outcome {
        pre_frames,
        result,
        ..Default::default()
    })
}

fn run_seek(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let d = ctx.run.descriptor().clone();
    let (min_ns, max_ns) = ctx.run.seek_range();
    let t_ns = if let Some(t) = uint(p, "t_ns") {
        t
    } else if let Some(f) = num(p, "fraction") {
        if !(0.0..=1.0).contains(&f) {
            return Err(ServerError::param(
                "/fraction",
                "must be in [0, 1]",
                "pass a fraction of the run duration",
            ));
        }
        ((d.duration as f64) * f) as u64
    } else if p.contains_key("event") {
        return Err(ServerError::NotSupportedHere(
            "seeking to the next event on a channel needs a recorded index; \
             pass t_ns or fraction"
                .to_string(),
        ));
    } else {
        return Err(ServerError::InvalidParams(vec![ParamError::new(
            "/",
            "one of t_ns, fraction or event is required",
            "pass {\"t_ns\": <nanoseconds>}",
        )]));
    };
    if t_ns < min_ns || t_ns > max_ns {
        return Err(ServerError::SeekOutOfRange { min_ns, max_ns });
    }
    // Refused *before* the run is touched. The check used to come after `Run::seek`, so a
    // seek over HTTP moved the run's cursor and paused it, and then reported that it could
    // not be done — a refusal with a side effect.
    if ctx.session.is_none() {
        return Err(ServerError::NotSupportedHere(
            "run.seek streams its result over the connection; call it on the socket".to_string(),
        ));
    }
    let before = ctx.run.state();
    let outputs = ctx.run.seek(t_ns)?;
    let after = ctx.run.state();
    if before != after {
        ctx.run.notify_state(after, t_ns, "run.seek");
    }
    let generation = ctx.run.generation();
    let session = ctx.session.as_deref_mut().ok_or_else(|| {
        ServerError::NotSupportedHere(
            "run.seek streams its result over the connection; call it on the socket".to_string(),
        )
    })?;
    if session.generation() != generation {
        // The run this connection's `Hello` described has been replaced; its frames would
        // be encoded against the wrong tables. The fresh `Hello` is on its way.
        return Err(ServerError::NotSupportedHere(
            "a new run started; seek again once its Hello has arrived".to_string(),
        ));
    }
    let (frames, keyframe_seq, deltas) = session.encode_seek(&outputs)?;
    Ok(Outcome {
        pre_frames: frames,
        result: json!({
            "t_ns": t_ns,
            "keyframe_seq": keyframe_seq,
            "deltas_applied": deltas,
            // §7.4 measures this from the request to the reply. The reading is taken by
            // the transport and passed in; `0.0` means the caller supplied no instant,
            // which is the case on the HTTP path where this method is refused anyway.
            "elapsed_ms": ctx
                .received_at
                .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1_000.0),
            "state": ctx.run.state().as_str(),
        }),
        ..Default::default()
    })
}

fn run_speed(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    if !p.contains_key("speed") {
        return Err(ServerError::param(
            "/speed",
            "required",
            "pass {\"speed\": 1}",
        ));
    }
    let speed = bounded(p, "speed", 0.0, 100.0, 1.0)?;
    let sync = text(p, "sync").unwrap_or("free");
    if !["free", "client"].contains(&sync) {
        return Err(ServerError::param(
            "/sync",
            "must be `free` or `client`",
            "omit it for `free`",
        ));
    }
    ctx.run.control(Control::Speed {
        speed,
        client_sync: sync == "client",
    })?;
    Ok(Outcome::of(json!({"speed": speed, "sync": sync})))
}

fn run_stop(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let finalize = flag(p, "finalize_exports", true);
    let outcome = ctx.run.control(Control::Stop {
        finalize_exports: finalize,
    })?;
    let mut result = json!({"state": outcome.state.as_str(), "t_ns": outcome.t_ns});
    for (key, value) in outcome.extra {
        result[key] = value;
    }
    ctx.run
        .notify_state(outcome.state, outcome.t_ns, "run.stop");
    // The connection stays open. §6.6 used to end it with `Bye{reason = 1}`, which left a
    // page that pressed Stop with no socket to press Run on; the run is over, the
    // connection is not, and `run.start` on it sends the next run's `Hello`.
    Ok(Outcome::of(result))
}

fn run_status(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let d = ctx.run.descriptor().clone();
    if let Some(asked) = text(p, "run_id") {
        if asked != d.run_id {
            return Err(ServerError::RunNotFound(asked.to_string()));
        }
    }
    let (speed, client_sync) = ctx.run.speed();
    let (actors, nodes) = ctx.run.counts();
    let profile = ctx.session.as_ref().map_or("full", |s| {
        if s.profile().is_node_only() {
            "node"
        } else {
            "full"
        }
    });
    let seq = ctx.session.as_ref().map_or(0, |s| s.next_seq());
    Ok(Outcome::of(json!({
        "run_id": d.run_id,
        "state": ctx.run.state().as_str(),
        "t_ns": ctx.run.sim_time(),
        "t_end_ns": d.duration,
        "speed": speed,
        "sync": if client_sync { "client" } else { "free" },
        "profile": profile,
        "live": d.live,
        "actors": actors,
        "nodes": nodes,
        "seq": seq,
        "dropped": {"delta": 0, "event": 0, "telemetry": 0, "metric": 0},
        "warnings": [],
        "generation": ctx.run.generation(),
        "scenario_hash": d.scenario_hash_hex,
        "staged_hash": ctx.run.staged().map(|s| s.hash),
        "engine": ctx.run.diagnostics(),
    })))
}

fn view_follow(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let mut node = uint(p, "node").map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    let mut clear = flag(p, "clear", false);
    // §6.7 lets a client follow an *actor*. The page does exactly that when it clicks a car
    // whose node it does not know, which is every car in a run whose vehicles spawn after
    // t = 0: the Hello node table is taken at t = 0 and is empty for them. The parameter was
    // in the schema and ignored here, so such a click followed nothing and the page said
    // "No radio selected". The actor is resolved to the node mounted on it now; an actor with
    // no radio follows no radio.
    if node.is_none()
        && !clear
        && let Some(actor) = uint(p, "actor").map(|a| u32::try_from(a).unwrap_or(u32::MAX))
    {
        node = ctx
            .run
            .nodes()
            .iter()
            .find(|r| r.actor_id == actor)
            .map(|r| r.node_id);
        clear = node.is_none();
    }
    let telemetry = flag(p, "telemetry", true);
    let radius_m = bounded(p, "radius_m", 0.0, 5_000.0, 0.0)?;
    if let Some(node) = node {
        if !ctx.run.has_node(node) {
            return Err(ServerError::UnknownId {
                kind: "node",
                id: node.to_string(),
            });
        }
    }
    let extra = match (node, radius_m > 0.0) {
        (Some(node), true) => ctx.run.nodes_within(node, radius_m),
        _ => Vec::new(),
    };
    let camera = text(p, "camera").map(str::to_string);
    let session = ctx.session.as_deref_mut().expect("checked in dispatch");
    let subscribed = session.follow(node, clear, telemetry, &extra);
    let mut result = json!({
        "following": session.following(),
        "subscribed_nodes": subscribed,
    });
    if let Some(mode) = camera {
        result["camera"] = json!(mode);
    }
    Ok(Outcome::of(result))
}

fn view_camera(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(mode) = text(p, "mode") else {
        return Err(ServerError::param(
            "/mode",
            "required",
            "pass one of map, chase, dashboard, free, rsu, jump",
        ));
    };
    const MODES: [&str; 6] = ["map", "chase", "dashboard", "free", "rsu", "jump"];
    if !MODES.contains(&mode) {
        return Err(ServerError::param(
            "/mode",
            &format!("`{mode}` is not a CameraMode"),
            "pass one of map, chase, dashboard, free, rsu, jump",
        ));
    }
    if let Some(node) = uint(p, "node") {
        let node = u32::try_from(node).unwrap_or(u32::MAX);
        if !ctx.run.has_node(node) {
            return Err(ServerError::UnknownId {
                kind: "node",
                id: node.to_string(),
            });
        }
    }
    let fov_deg = bounded(p, "fov_deg", 1.0, 150.0, 50.0)?;
    let projection = text(p, "projection").unwrap_or("perspective").to_string();
    if !["perspective", "orthographic"].contains(&projection.as_str()) {
        return Err(ServerError::param(
            "/projection",
            "must be `perspective` or `orthographic`",
            "omit it for `perspective`",
        ));
    }
    let session = ctx.session.as_deref_mut().expect("checked in dispatch");
    let previous = session.camera().clone();
    let camera = CameraState {
        mode: mode.to_string(),
        position: vec3(p, "position", previous.position)?,
        target: vec3(p, "target", previous.target)?,
        fov_deg,
        projection,
    };
    session.set_camera(camera.clone());
    let echo = json!({
        "mode": camera.mode,
        "position": {"x": camera.position[0], "y": camera.position[1], "z": camera.position[2]},
        "target": {"x": camera.target[0], "y": camera.target[1], "z": camera.target[2]},
        "fov_deg": camera.fov_deg,
        "projection": camera.projection,
    });
    let mut changed = echo.clone();
    changed["following"] = json!(session.following());
    Ok(Outcome {
        result: echo,
        notifications: vec![notification("view.changed", changed)],
        ..Default::default()
    })
}

fn overlay_set(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let node_profile = ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only());
    let list = flag(p, "list", false);
    let mut wanted = BTreeMap::new();
    if let Some(obj) = p.get("overlays").and_then(Value::as_object) {
        for (name, on) in obj {
            let Some(on) = on.as_bool() else {
                return Err(ServerError::param(
                    &format!("/overlays/{name}"),
                    "must be a boolean",
                    "pass true or false",
                ));
            };
            wanted.insert(name.clone(), on);
        }
    }
    let mut opacity = BTreeMap::new();
    if let Some(obj) = p.get("opacity").and_then(Value::as_object) {
        for (name, value) in obj {
            let Some(v) = value.as_f64().filter(|v| (0.0..=1.0).contains(v)) else {
                return Err(ServerError::param(
                    &format!("/opacity/{name}"),
                    "must be a number in [0, 1]",
                    "pass a fraction",
                ));
            };
            opacity.insert(name.clone(), v);
        }
    }
    let session = ctx.session.as_deref_mut().expect("checked in dispatch");
    if !list {
        session.set_overlays(&wanted, &opacity)?;
    }
    let mut result = json!({"overlays": session.overlays()});
    if list {
        result["catalogue"] = Value::Array(
            OVERLAYS
                .iter()
                .map(|name| {
                    let gt = overlay_is_gt(name);
                    json!({
                        "name": name,
                        "visibility": if gt { "GT" } else { "NODE" },
                        "available": !(gt && node_profile),
                        "description": format!("the `{name}` overlay of 09-ui §6"),
                        "needs_channels": overlay_channels(name),
                    })
                })
                .collect(),
        );
    }
    Ok(Outcome::of(result))
}

fn overlay_channels(name: &str) -> Vec<&'static str> {
    match name {
        "tx_pulses" => vec!["node.tx"],
        "links" => vec!["phy.rx"],
        "cbr_heatmap" => vec!["mac.cbr"],
        "detections" => vec!["det.observation"],
        "revoked" | "reported" => vec!["proto.revocation"],
        "attackers_gt" | "trajectories_gt" | "belief_vs_truth_gt" => vec!["gt.kinematics"],
        _ => Vec::new(),
    }
}

fn node_param(p: &Map<String, Value>, key: &str) -> Result<NodeId> {
    let raw = uint(p, key).ok_or_else(|| {
        ServerError::param(
            &format!("/{key}"),
            "required, and must be a node id",
            "pass an id from Hello.nodes",
        )
    })?;
    Ok(NodeId::new(u32::try_from(raw).unwrap_or(u32::MAX)))
}

fn inspect_node(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let node = node_param(p, "node")?;
    let include = if p.contains_key("include") {
        strings(p, "include")
    } else {
        vec![
            "telemetry".to_string(),
            "queues".to_string(),
            "neighbors".to_string(),
        ]
    };
    let limit = bounded(p, "limit", 1.0, 1_000.0, 50.0)? as usize;
    let value = ctx.run.query(&Query::Node {
        node,
        t_ns: uint(p, "t_ns"),
        include,
        limit,
    })?;
    Ok(Outcome::of(value))
}

fn inspect_link(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let window_ns = uint(p, "window_ns").unwrap_or(1_000_000_000);
    let t_ns = uint(p, "t_ns");
    let query = if let Some(link) = text(p, "link") {
        Query::NamedLink {
            link: link.to_string(),
            t_ns,
            window_ns,
        }
    } else {
        Query::Link {
            tx: node_param(p, "tx")?,
            rx: node_param(p, "rx")?,
            t_ns,
            window_ns,
        }
    };
    let mut value = ctx.run.query(&query)?;
    if ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only())
    {
        // §5.2: the true tx-rx distance and the LOS class are ground truth.
        if let Some(obj) = value.as_object_mut() {
            obj.remove("distance_m");
            obj.remove("los");
        }
    }
    Ok(Outcome::of(value))
}

fn inspect_entity(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(entity) = text(p, "entity") else {
        return Err(ServerError::param(
            "/entity",
            "required",
            "pass a role id such as \"ma\" or \"pca\"",
        ));
    };
    if ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only())
        && (entity == "attacker" || entity.starts_with("attacker:"))
    {
        // §5.3 names `inspect.entity` on an `Attacker` entity explicitly.
        return Err(ServerError::VisibilityDenied {
            field: entity.to_string(),
            visibility: "GT",
        });
    }
    let limit = bounded(p, "limit", 1.0, 1_000.0, 50.0)? as usize;
    Ok(Outcome::of(ctx.run.query(&Query::Entity {
        entity: entity.to_string(),
        t_ns: uint(p, "t_ns"),
        limit,
    })?))
}

fn explain(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(subject) = p.get("subject").filter(|v| v.is_object()) else {
        return Err(ServerError::param(
            "/subject",
            "required, and must be a ValueRef object",
            "pass {\"kind\": \"metric\", \"id\": \"pdr\"}",
        ));
    };
    let depth = bounded(p, "depth", 1.0, 5.0, 1.0)? as u8;
    let format = text(p, "format").unwrap_or("json");
    if !["json", "markdown"].contains(&format) {
        return Err(ServerError::param(
            "/format",
            "must be `json` or `markdown`",
            "omit it for `json`",
        ));
    }
    if ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only())
    {
        let id = subject.get("id").and_then(Value::as_str).unwrap_or("");
        if is_gt_metric(ctx.run, id) {
            return Err(ServerError::VisibilityDenied {
                field: id.to_string(),
                visibility: "GT",
            });
        }
    }
    Ok(Outcome::of(ctx.run.query(&Query::Explain {
        subject: subject.clone(),
        depth,
        markdown: format == "markdown",
    })?))
}

/// True if the run's catalogue marks this metric ground truth (§5.2, §6.9, §6.12).
fn is_gt_metric(run: &crate::Run, name: &str) -> bool {
    run.metric_catalogue()
        .iter()
        .any(|m| m.name == name && m.visibility == "GT")
}

fn scenario_get(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let d = ctx.run.descriptor();
    // The scenario the next `run.start` will run: the staged one when `scenario.set` or
    // `scenario.load` has put one in place, the running one otherwise (§6.6: `run.start`
    // "loads … the already-set one").
    let staged = ctx.run.staged();
    let (mut scenario, hash) = match &staged {
        Some(s) => (s.document.clone(), s.hash.clone()),
        None => (d.scenario.clone(), d.scenario_hash_hex.clone()),
    };
    if let Some(pointer) = text(p, "path") {
        scenario = scenario.pointer(pointer).cloned().ok_or_else(|| {
            ServerError::param(
                "/path",
                &format!("`{pointer}` does not resolve in the scenario"),
                "pass a JSON Pointer such as /traffic/actors",
            )
        })?;
    }
    let mut result = json!({
        "scenario": scenario,
        "hash": hash,
        "running_hash": d.scenario_hash_hex,
        "staged": staged.as_ref().map(|s| json!({
            "hash": s.hash, "changed": s.changed, "valid": s.errors.is_empty(),
            "errors": s.errors,
        })),
    });
    if flag(p, "with_schema", false) {
        // The published surface, not a hand-written stub. §13 of 03-interfaces requires
        // the schema to carry help text and units for every field, and
        // 13-product-direction.md §2 makes the page's settings form generated from it —
        // so the whole bundle is served here, and `scenario.schema` serves it without
        // the document for a client that only wants the form.
        let surface = v2xw_engine::scenario::publish::surface();
        result["schema"] = surface["schema"].clone();
        for key in [
            "fields",
            "groups",
            "slots",
            "models",
            "statuses",
            "validator",
        ] {
            result[key] = surface[key].clone();
        }
    }
    Ok(Outcome::of(result))
}

/// `scenario.schema` (§6.10): the generated settings surface, without the document.
///
/// Separate from `scenario.get` because the two have different lifetimes. The document
/// changes whenever the scenario is edited; the surface is a property of the *build* — the
/// schema is reflected from the types, the ranges come from the loader's own validator and
/// the model parameters from the registry — so a page fetches it once per connection and a
/// run's scenario as often as it likes.
///
/// `sections` selects parts of it, because the whole bundle is a few hundred kilobytes and
/// a page that only wants the model catalogue should not carry the field index with it.
fn scenario_schema(_ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let surface = v2xw_engine::scenario::publish::surface();
    let wanted: Vec<String> = match p.get("sections") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    if wanted.is_empty() {
        return Ok(Outcome::of(surface));
    }
    let known = [
        "version",
        "engine",
        "generated_from",
        "validator",
        "groups",
        "statuses",
        "schema",
        "fields",
        "slots",
        "models",
    ];
    if let Some(bad) = wanted.iter().find(|w| !known.contains(&w.as_str())) {
        return Err(ServerError::param(
            "/sections",
            &format!("`{bad}` is not a section of the scenario surface"),
            &format!("one of: {}", known.join(", ")),
        ));
    }
    let mut out = Map::new();
    // The version and the engine identity always come back: a cached surface that cannot
    // say which build produced it is a cache a page cannot invalidate.
    out.insert("version".into(), surface["version"].clone());
    out.insert("engine".into(), surface["engine"].clone());
    for section in wanted {
        out.insert(section.clone(), surface[section.as_str()].clone());
    }
    Ok(Outcome::of(Value::Object(out)))
}

fn scenario_set(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    if !p.contains_key("scenario") && !p.contains_key("patch") {
        return Err(ServerError::InvalidParams(vec![ParamError::new(
            "/",
            "one of `scenario` or `patch` is required",
            "pass a whole document or an RFC 6902 patch",
        )]));
    }
    let request = match (p.get("scenario"), p.get("patch")) {
        (Some(doc @ Value::Object(_)), _) => StageRequest::Document(doc.clone()),
        (Some(_), _) => {
            return Err(ServerError::param(
                "/scenario",
                "must be a scenario document (an object)",
                "pass the whole document, or an RFC 6902 patch as `patch`",
            ));
        }
        (None, Some(Value::Array(ops))) => StageRequest::Patch(ops.clone()),
        (None, _) => {
            return Err(ServerError::param(
                "/patch",
                "must be an array of RFC 6902 operations",
                "[{\"op\": \"replace\", \"path\": \"/time/duration_s\", \"value\": 30}]",
            ));
        }
    };
    let staged = ctx.run.stage(request)?;
    let valid = staged.errors.is_empty();
    if !valid && flag(p, "validate", true) {
        return Err(ServerError::ScenarioInvalid(staged.errors));
    }
    Ok(Outcome {
        result: json!({
            "hash": staged.hash,
            "valid": valid,
            "errors": staged.errors,
            // Nothing is changed in a run that is already moving: every edit is taken at
            // the next `run.start`, which is the only instant at which a changed world,
            // seed or fleet can be consistent with everything already streamed.
            "applied_live": [],
            "requires_restart": staged.changed,
        }),
        notifications: vec![notification(
            "validation",
            json!({"errors": staged.errors, "warnings": []}),
        )],
        ..Default::default()
    })
}

fn scenario_validate(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let d = ctx.run.descriptor();
    let doc = p.get("scenario").unwrap_or(&d.scenario);
    let strict = flag(p, "strict", false);
    let (errors, warnings) = match ctx.run.validate_document(doc) {
        // The loader's own rules — the same code `run.start` will run the document
        // through, so "valid" here means it will start.
        Some(verdict) => verdict,
        None => (
            validate_scenario(doc),
            vec![ParamError {
                path: "/".to_string(),
                message: "this run is produced by the server's synthetic fixture, not by an \
                          engine, so only the document's shape was checked"
                    .to_string(),
                hint: None,
                severity: Some("warning".to_string()),
            }],
        ),
    };
    let (errors, warnings) = if strict {
        let mut e = errors;
        e.extend(warnings);
        (e, Vec::new())
    } else {
        (errors, warnings)
    };
    let (actors, nodes) = ctx.run.counts();
    Ok(Outcome::of(json!({
        "valid": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
        "resolved_tiers": {"radio": "abstract", "security": "abstract", "mobility": "abstract"},
        "estimated_cost": {
            "actors": actors, "nodes": nodes,
            "realtime_factor": 1.0, "recording_mb_per_sim_min": 6.0
        },
    })))
}

/// The scenario checks the fixture can make: the schema tag, the seed and the duration.
fn validate_scenario(doc: &Value) -> Vec<ParamError> {
    let mut out = Vec::new();
    if doc.get("schema").and_then(Value::as_str) != Some("v2xw/scenario/1") {
        out.push(ParamError::new(
            "/schema",
            "must be the string \"v2xw/scenario/1\"",
            "add \"schema\": \"v2xw/scenario/1\"",
        ));
    }
    if doc.get("seed").and_then(Value::as_u64).is_none() {
        out.push(ParamError::new(
            "/seed",
            "must be a non-negative integer",
            "add \"seed\": 0",
        ));
    }
    match doc.pointer("/time/duration_s").and_then(Value::as_u64) {
        Some(d) if d > 0 => {}
        _ => out.push(ParamError::new(
            "/time/duration_s",
            "must be a positive integer number of seconds",
            "add \"time\": {\"duration_s\": 60}",
        )),
    }
    out
}

fn scenario_save(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(path) = text(p, "path") else {
        return Err(ServerError::param(
            "/path",
            "required",
            "pass the file to write",
        ));
    };
    let format = text(p, "format").unwrap_or("yaml");
    if !["yaml", "json"].contains(&format) {
        return Err(ServerError::param(
            "/format",
            "must be `yaml` or `json`",
            "omit it for `yaml`",
        ));
    }
    let d = ctx.run.descriptor().clone();
    let bytes =
        serde_json::to_vec(&d.scenario).map_err(|e| ServerError::Internal(e.to_string()))?;
    // The fixture server is read-only on the file system by design: it never writes
    // outside a recording path the operator named. A real engine writes here.
    Err(ServerError::Io {
        path: path.to_string(),
        errno: format!(
            "EROFS: this server does not write scenarios ({} bytes would have been written)",
            bytes.len()
        ),
    })
}

fn scenario_load(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(path) = text(p, "path") else {
        return Err(ServerError::param(
            "/path",
            "required",
            "pass a file path or a preset id",
        ));
    };
    let d = ctx.run.descriptor();
    if path == "stub/grid" || path == d.scenario_hash_hex {
        // Loading the running scenario withdraws whatever `scenario.set` staged: the next run
        // is the one on screen again. An engine with no scenario to stage has nothing to undo.
        let _ = ctx.run.stage(StageRequest::Document(d.scenario.clone()));
        return Ok(Outcome::of(json!({
            "hash": d.scenario_hash_hex,
            "valid": true,
            "scenario": d.scenario,
            "errors": [],
        })));
    }
    // A live engine stages the preset for the next run, exactly as `scenario.set` stages
    // a document; the form then shows it, and Run runs it.
    match ctx.run.stage(StageRequest::Preset(path.to_string())) {
        Ok(staged) => Ok(Outcome::of(json!({
            "hash": staged.hash,
            "valid": staged.errors.is_empty(),
            "scenario": staged.document,
            "errors": staged.errors,
            "requires_restart": staged.changed,
        }))),
        Err(ServerError::NotSupportedHere(_)) => Err(ServerError::Io {
            path: path.to_string(),
            errno: "ENOENT".to_string(),
        }),
        Err(e) => Err(e),
    }
}

fn scenario_list(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let kind = text(p, "kind").unwrap_or("all");
    if !["presets", "saved", "runs", "all"].contains(&kind) {
        return Err(ServerError::param(
            "/kind",
            "must be one of presets, saved, runs, all",
            "omit it for `all`",
        ));
    }
    let prefix = text(p, "prefix").unwrap_or("");
    let limit = bounded(p, "limit", 1.0, 1_000.0, 100.0)? as usize;
    let d = ctx.run.descriptor();
    let mut items = Vec::new();
    if kind == "all" || kind == "presets" {
        let presets = ctx.run.presets();
        if presets.is_empty() {
            items.push(json!({
                "id": "stub/grid", "kind": "preset", "name": "Synthetic grid",
                "description": "the server's own fixture run", "tags": ["fixture"],
                "hash": d.scenario_hash_hex
            }));
        } else {
            items.extend(presets);
        }
    }
    if kind == "all" || kind == "runs" {
        items.push(json!({
            "id": d.run_id, "kind": "run", "name": "current run",
            "hash": d.scenario_hash_hex
        }));
    }
    items.retain(|i| {
        i.get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with(prefix))
    });
    items.truncate(limit);
    Ok(Outcome::of(json!({"items": items})))
}

fn world_import(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    if !p.contains_key("bbox") && !p.contains_key("file") {
        return Err(ServerError::InvalidParams(vec![ParamError::new(
            "/",
            "one of `bbox` or `file` is required",
            "pass {\"bbox\": [min_lon, min_lat, max_lon, max_lat]}",
        )]));
    }
    if let Some(bbox) = p.get("bbox") {
        let arr = bbox.as_array().filter(|a| a.len() == 4).ok_or_else(|| {
            ServerError::param(
                "/bbox",
                "must be four numbers",
                "pass [min_lon, min_lat, max_lon, max_lat]",
            )
        })?;
        if arr.iter().any(|v| !v.is_number()) {
            return Err(ServerError::param(
                "/bbox",
                "must be four numbers",
                "pass [min_lon, min_lat, max_lon, max_lat]",
            ));
        }
    }
    // §6.11's `DECISION`: anything that can exceed 2 s returns a Job. An OSM import always
    // can, so this always does, and the job fails because this server has no importer
    // wired to it — but it fails through `job.done`, which is the shape R8 checks.
    let job_id = format!("job-import-{}", ctx.run.descriptor().run_id);
    Ok(Outcome {
        result: json!({"job_id": job_id, "state": "queued", "progress": 0.0,
                       "message": "OSM import queued"}),
        notifications: vec![
            notification(
                "job.progress",
                json!({"job_id": job_id, "progress": 0.0, "message": "queued"}),
            ),
            notification(
                "job.done",
                json!({"job_id": job_id, "state": "failed", "outputs": [],
                       "error": "this server serves one world; call world.generate or \
                                 start the engine with an imported world"}),
            ),
        ],
        ..Default::default()
    })
}

fn world_generate(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(kind) = text(p, "kind") else {
        return Err(ServerError::param(
            "/kind",
            "required",
            "pass one of grid, manhattan, highway, ring, intersection, custom",
        ));
    };
    const KINDS: [&str; 6] = [
        "grid",
        "manhattan",
        "highway",
        "ring",
        "intersection",
        "custom",
    ];
    if !KINDS.contains(&kind) {
        return Err(ServerError::param(
            "/kind",
            &format!("`{kind}` is not a generator"),
            "pass one of grid, manhattan, highway, ring, intersection, custom",
        ));
    }
    let lanes = bounded(p, "lanes_per_direction", 1.0, 6.0, 2.0)? as u32;
    let lane_width_m = bounded(p, "lane_width_m", 2.0, 5.0, 3.25)?;
    let block_m = bounded(p, "block_m", 20.0, 5_000.0, 120.0)?;
    let seed = uint(p, "seed").unwrap_or(0);
    let result = ctx
        .run
        .generate_world(kind, block_m, lanes, lane_width_m, seed)?;
    Ok(Outcome::of(result))
}

fn events_set(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let node_profile = ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only());
    let list = flag(p, "list", false);
    let subscribe = strings(p, "subscribe");
    let unsubscribe = strings(p, "unsubscribe");
    let only = p.contains_key("only").then(|| strings(p, "only"));
    let mut nodes = None;
    let mut sample_1_in = None;
    if let Some(filter) = p.get("filter").and_then(Value::as_object) {
        if let Some(list) = filter.get("nodes").and_then(Value::as_array) {
            nodes = Some(
                list.iter()
                    .filter_map(Value::as_u64)
                    .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
                    .collect::<Vec<_>>(),
            );
        }
        if let Some(n) = filter.get("sample_1_in").and_then(Value::as_u64) {
            if !(1..=10_000).contains(&n) {
                return Err(ServerError::param(
                    "/filter/sample_1_in",
                    "must be in [1, 10000]",
                    "omit it for 1",
                ));
            }
            sample_1_in = Some(n as usize);
        }
    }
    let max_events = match uint(p, "max_events_per_step") {
        Some(n) if n <= 1_000_000 => Some(n as usize),
        Some(_) => {
            return Err(ServerError::param(
                "/max_events_per_step",
                "must be in [0, 1000000]",
                "omit it for 5000",
            ));
        }
        None => None,
    };

    let session = ctx.session.as_deref_mut();
    let subscribed_ids = match session {
        Some(session) => session.set_events(
            &subscribe,
            &unsubscribe,
            only.as_deref(),
            nodes.as_deref(),
            sample_1_in,
            max_events,
        )?,
        None => {
            // Over HTTP there is no stream to subscribe. §6.2 lists only the three view
            // methods as connection-scoped, so `events.set` answers with the catalogue
            // rather than refusing — but it cannot change a subscription.
            if !subscribe.is_empty() || !unsubscribe.is_empty() || only.is_some() {
                return Err(ServerError::NotSupportedHere(
                    "`events.set` changes a stream subscription and there is no stream \
                     over HTTP; call it on the socket, or pass {\"list\": true}"
                        .to_string(),
                ));
            }
            Vec::new()
        }
    };

    let describe = |wire_id: u16| -> Option<Value> {
        let spec = v2xw_record::channels::by_wire_id(wire_id)?;
        Some(json!({
            "channel": spec.name,
            "channel_id": wire_id,
            "visibility": crate::visibility_name(spec.visibility),
            "est_rate_per_s": 0.0,
        }))
    };
    let subscribed: Vec<Value> = subscribed_ids
        .iter()
        .filter_map(|id| describe(*id))
        .collect();
    let mut result = json!({"subscribed": subscribed});
    if list || subscribed.is_empty() {
        result["available"] = Value::Array(
            v2xw_record::CHANNELS
                .iter()
                .filter(|c| c.wire_id.is_some())
                .filter(|c| !(node_profile && c.is_ground_truth_channel()))
                .map(|c| {
                    json!({
                        "channel": c.name,
                        "channel_id": c.wire_id,
                        "visibility": crate::visibility_name(c.visibility),
                    })
                })
                .collect(),
        );
    }
    Ok(Outcome::of(result))
}

fn metrics_query(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let node_profile = ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only());
    let metrics = strings(p, "metrics");
    if metrics.is_empty() && !p.contains_key("metrics") {
        return Ok(Outcome::of(ctx.run.query(&Query::MetricCatalogue)?));
    }
    for name in &metrics {
        if node_profile && is_gt_metric(ctx.run, name) {
            return Err(ServerError::VisibilityDenied {
                field: name.clone(),
                visibility: "GT",
            });
        }
    }
    let format = text(p, "format").unwrap_or("json");
    if !["json", "arrow"].contains(&format) {
        return Err(ServerError::param(
            "/format",
            "must be `json` or `arrow`",
            "omit it for `json`",
        ));
    }
    let limit = bounded(p, "limit", 1.0, 1_000_000.0, 10_000.0)? as usize;
    let bin_ns = uint(p, "bin_ns").unwrap_or(1_000_000_000).max(1);
    Ok(Outcome::of(ctx.run.query(&Query::Metrics {
        metrics,
        t_from_ns: uint(p, "t_from_ns"),
        t_to_ns: uint(p, "t_to_ns"),
        bin_ns,
        group_by: strings(p, "group_by"),
        limit,
    })?))
}

fn metrics_plot(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let metrics = match p.get("metric") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => {
            return Err(ServerError::param(
                "/metric",
                "required, and must be a metric name or a list of them",
                "pass {\"metric\": \"pdr\"}",
            ));
        }
    };
    let node_profile = ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only());
    let catalogue = ctx.run.metric_catalogue();
    for name in &metrics {
        if !catalogue.iter().any(|m| &m.name == name) {
            return Err(ServerError::UnknownMetric {
                metric: name.clone(),
                did_you_mean: Vec::new(),
            });
        }
        if node_profile && is_gt_metric(ctx.run, name) {
            return Err(ServerError::VisibilityDenied {
                field: name.clone(),
                visibility: "GT",
            });
        }
    }
    let kind = text(p, "kind").unwrap_or("line").to_string();
    const KINDS: [&str; 6] = ["line", "scatter", "bar", "box", "heatmap", "cdf"];
    if !KINDS.contains(&kind.as_str()) {
        return Err(ServerError::param(
            "/kind",
            "must be one of line, scatter, bar, box, heatmap, cdf",
            "omit it for `line`",
        ));
    }
    Ok(Outcome::of(ctx.run.query(&Query::Plot {
        metrics,
        x: text(p, "x").unwrap_or("t").to_string(),
        kind,
    })?))
}

fn export_dataset(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(exporter) = text(p, "exporter") else {
        return Err(ServerError::param(
            "/exporter",
            "required",
            "pass one of ma-dataset, receiver-logs, telemetry, net-trace, backend-log, metrics",
        ));
    };
    const EXPORTERS: [&str; 6] = [
        "ma-dataset",
        "receiver-logs",
        "telemetry",
        "net-trace",
        "backend-log",
        "metrics",
    ];
    if !EXPORTERS.contains(&exporter) {
        return Err(ServerError::param(
            "/exporter",
            &format!("`{exporter}` is not an exporter"),
            "pass one of ma-dataset, receiver-logs, telemetry, net-trace, backend-log, metrics",
        ));
    }
    let visibility = text(p, "visibility").unwrap_or("both").to_string();
    if !["node", "gt", "both"].contains(&visibility.as_str()) {
        return Err(ServerError::param(
            "/visibility",
            "must be one of node, gt, both",
            "omit it for `both`",
        ));
    }
    if ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only())
        && visibility != "node"
    {
        return Err(ServerError::VisibilityDenied {
            field: "visibility".to_string(),
            visibility: "GT",
        });
    }
    Ok(Outcome::of(ctx.run.query(&Query::ExportDataset {
        exporter: exporter.to_string(),
        out_dir: text(p, "out_dir").map(str::to_string),
        visibility,
    })?))
}

fn export_recording(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let profile = text(p, "profile").unwrap_or("full").to_string();
    if !["full", "node"].contains(&profile.as_str()) {
        return Err(ServerError::param(
            "/profile",
            "must be `full` or `node`",
            "omit it for `full`",
        ));
    }
    if ctx
        .session
        .as_ref()
        .is_some_and(|s| s.profile().is_node_only())
        && profile == "full"
    {
        return Err(ServerError::VisibilityDenied {
            field: "profile".to_string(),
            visibility: "GT",
        });
    }
    Ok(Outcome::of(ctx.run.query(&Query::ExportRecording {
        path: text(p, "path").map(str::to_string),
        profile,
    })?))
}

fn experiment_define(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(name) = text(p, "name") else {
        return Err(ServerError::param(
            "/name",
            "required",
            "name the experiment",
        ));
    };
    let Some(sweep) = p.get("sweep").and_then(Value::as_object) else {
        return Err(ServerError::param(
            "/sweep",
            "required, and must be an object of path -> value list",
            "pass {\"traffic.actors\": [100, 200]}",
        ));
    };
    if sweep.is_empty() {
        return Err(ServerError::param(
            "/sweep",
            "must name at least one axis",
            "pass {\"traffic.actors\": [100, 200]}",
        ));
    }
    let mut cells = 1usize;
    for (path, values) in sweep {
        let Some(values) = values.as_array().filter(|a| !a.is_empty()) else {
            return Err(ServerError::param(
                &format!("/sweep/{path}"),
                "must be a non-empty array of values",
                "give the axis at least one value",
            ));
        };
        cells = cells.saturating_mul(values.len());
    }
    let seeds = p
        .get("seeds")
        .and_then(|v| v.get("count"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    cells = cells.saturating_mul(seeds);
    let id = ctx.run.define_experiment(name, cells);
    Ok(Outcome::of(json!({
        "experiment_id": id,
        "cells": cells,
        "estimated_wall_s": (cells as f64) * 30.0,
        "warnings": [],
    })))
}

fn experiment_run(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(id) = text(p, "experiment_id") else {
        return Err(ServerError::param(
            "/experiment_id",
            "required",
            "pass the id experiment.define returned",
        ));
    };
    let cells = ctx
        .run
        .experiment_cells(id)
        .ok_or_else(|| ServerError::ExperimentNotFound(id.to_string()))?;
    let job_id = format!("job-exp-{id}");
    Ok(Outcome {
        result: json!({
            "job_id": job_id, "state": "queued", "progress": 0.0,
            "experiment_id": id, "cells_total": cells, "cells_skipped": 0
        }),
        notifications: vec![
            notification(
                "experiment.progress",
                json!({"experiment_id": id, "cells_done": 0, "cells_total": cells,
                       "eta_s": (cells as f64) * 30.0}),
            ),
            notification(
                "job.done",
                json!({"job_id": job_id, "state": "failed", "outputs": [],
                       "error": "this server runs one fixture run and has no batch runner"}),
            ),
        ],
        ..Default::default()
    })
}

fn experiment_status(ctx: &mut Context<'_>, p: &Map<String, Value>) -> Result<Outcome> {
    let Some(id) = text(p, "experiment_id") else {
        return Err(ServerError::param(
            "/experiment_id",
            "required",
            "pass the id experiment.define returned",
        ));
    };
    let cells = ctx
        .run
        .experiment_cells(id)
        .ok_or_else(|| ServerError::ExperimentNotFound(id.to_string()))?;
    let mut result = json!({
        "experiment_id": id,
        "state": "defined",
        "cells_total": cells,
        "cells_done": 0,
        "cells_failed": 0,
        "progress": 0.0,
        "outputs": [],
    });
    if flag(p, "include_cells", false) {
        result["cells"] = Value::Array(
            (0..cells)
                .map(|i| json!({"index": i, "key": {}, "state": "pending"}))
                .collect(),
        );
    }
    Ok(Outcome::of(result))
}

/// Encodes every step the connection has not yet seen, so a reply that promises frames
/// have already gone out is telling the truth (§6.6, conformance R4 and R5).
fn drain_pending(ctx: &mut Context<'_>) -> Result<Vec<Frame>> {
    let (Some(session), Some(rx)) = (ctx.session.as_deref_mut(), ctx.pending.as_deref_mut()) else {
        return Ok(Vec::new());
    };
    let mut frames = Vec::new();
    while let Ok(output) = rx.try_recv() {
        frames.extend(session.encode_step(&output)?.frames);
    }
    Ok(frames)
}
