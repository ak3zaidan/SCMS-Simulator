//! The OpenRPC 1.3.2 document `rpc.discover` and `GET /rpc/schema` return (§6.3).
//!
//! It is built here rather than checked in as a file so that it cannot fall out of step
//! with [`crate::rpc::METHODS`]: the method list is one array, used by the dispatcher and
//! by this document, and `tests/rpc.rs` asserts the two agree and that there are exactly
//! the 32 methods of §6.15 (conformance R1 and R2).

use serde_json::{Value, json};

use crate::rpc::METHODS;

/// The shared `$defs` of §6.5, as the document's `components.schemas`.
fn components() -> Value {
    json!({
        "SimTimeNs": {"type": "integer", "minimum": 0,
                      "description": "nanoseconds since t0"},
        "RunId": {"type": "string", "format": "uuid"},
        "NodeId": {"type": "integer", "minimum": 0, "maximum": 4_294_967_294u64},
        "ActorId": {"type": "integer", "minimum": 0, "maximum": 4_294_967_294u64},
        "LaneId": {"type": "integer", "minimum": 0, "maximum": 4_294_967_294u64},
        "Sha256Hex": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
        "Visibility": {"enum": ["GT", "NODE", "PUBLIC", "MIXED", "DERIVED", "META"]},
        "RunState": {"enum": ["idle", "loading", "running", "paused", "seeking",
                              "finished", "error"]},
        "ChannelName": {"type": "string",
                        "pattern": "^[a-z][a-z0-9]*(\\.[a-z][a-z0-9_]*)+$"},
        "CameraMode": {"enum": ["map", "chase", "dashboard", "free", "rsu", "jump"]},
        "Vec3": {"type": "object", "additionalProperties": false,
                 "required": ["x", "y", "z"],
                 "properties": {"x": {"type": "number"}, "y": {"type": "number"},
                                "z": {"type": "number"}}},
        "ValueRef": {"type": "object", "additionalProperties": false,
                     "required": ["kind"],
                     "properties": {
                        "kind": {"enum": ["metric", "node_field", "actor_field", "event",
                                          "link", "entity", "channel", "world", "overlay"]},
                        "id": {"type": "string"},
                        "node": {"$ref": "#/components/schemas/NodeId"},
                        "actor": {"$ref": "#/components/schemas/ActorId"},
                        "t_ns": {"$ref": "#/components/schemas/SimTimeNs"},
                        "prov_id": {"type": "integer", "minimum": 1}}},
        "Provenance": {"type": "object",
                       "required": ["prov_id", "model_id", "model_version", "param_set_id"],
                       "properties": {
                         "prov_id": {"type": "integer"},
                         "model_id": {"type": "string"},
                         "model_version": {"type": "string"},
                         "param_set_id": {"type": "string"},
                         "family": {"type": "string"},
                         "card_url": {"type": "string", "format": "uri-reference"}}},
        "ValidationError": {"type": "object",
                            "required": ["path", "message"],
                            "properties": {"path": {"type": "string"},
                                           "message": {"type": "string"},
                                           "hint": {"type": "string"},
                                           "severity": {"enum": ["error", "warning"]}}},
        "WorldImportResult": {"type": "object",
                              "required": ["world_hash", "bbox_m", "lanes", "buildings",
                                           "junctions", "bytes", "cached"],
                              "properties": {
                                "world_hash": {"$ref": "#/components/schemas/Sha256Hex"},
                                "url": {"type": "string"},
                                "bbox_m": {"type": "object"},
                                "origin": {"type": "object"},
                                "lanes": {"type": "integer"},
                                "buildings": {"type": "integer"},
                                "junctions": {"type": "integer"},
                                "signals": {"type": "integer"},
                                "bytes": {"type": "integer"},
                                "cached": {"type": "boolean"}}},
        "Job": {"type": "object", "required": ["job_id", "state"],
                "properties": {"job_id": {"type": "string"},
                               "state": {"enum": ["queued", "running", "done", "failed",
                                                  "cancelled"]},
                               "progress": {"type": "number", "minimum": 0, "maximum": 1},
                               "message": {"type": "string"},
                               "outputs": {"type": "array", "items": {"type": "string"}}}}
    })
}

/// Every error code of §6.4, as OpenRPC error objects.
fn errors() -> Vec<Value> {
    [
        (-32700, "parse_error"),
        (-32600, "invalid_request"),
        (-32601, "method_not_found"),
        (-32602, "invalid_params"),
        (-32603, "internal_error"),
        (-32000, "run_not_found"),
        (-32001, "run_already_running"),
        (-32002, "run_not_running"),
        (-32003, "seek_out_of_range"),
        (-32004, "scenario_invalid"),
        (-32005, "world_not_found"),
        (-32006, "unknown_id"),
        (-32007, "unknown_metric"),
        (-32008, "export_failed"),
        (-32009, "not_supported_here"),
        (-32010, "busy"),
        (-32011, "experiment_not_found"),
        (-32012, "plugin_drift"),
        (-32013, "io_error"),
        (-32040, "visibility_denied"),
        (-32041, "unauthorized"),
        (-32042, "rate_limited"),
        (-32050, "unsupported_version"),
    ]
    .iter()
    .map(|(code, name)| json!({"code": code, "message": name}))
    .collect()
}

fn object(required: &[&str], properties: Value) -> Value {
    json!({"type": "object", "additionalProperties": false,
           "required": required, "properties": properties})
}

fn method(name: &str, summary: &str, params: Value, result: Value, codes: &[i32]) -> Value {
    let all = errors();
    let listed: Vec<Value> = codes
        .iter()
        .filter_map(|c| {
            all.iter()
                .find(|e| e.get("code").and_then(Value::as_i64) == Some(i64::from(*c)))
                .cloned()
        })
        .collect();
    json!({
        "name": name,
        "summary": summary,
        "paramStructure": "by-name",
        "params": [{"name": "params", "required": false, "schema": params}],
        "result": {"name": "result", "schema": result},
        "errors": listed,
    })
}

/// Builds the document, optionally narrowed to one method (§6.3's `method` param).
///
/// # Panics
/// Never: the document is built from literals and the method list is a constant.
pub fn document(only: Option<&str>) -> Value {
    let mut methods = all_methods();
    if let Some(name) = only {
        methods.retain(|m| m.get("name").and_then(Value::as_str) == Some(name));
    }
    json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "VWP v1 control surface",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "docs/protocol/vwp-v1.md §6. 31 methods plus rpc.discover.",
            "license": {"name": "Apache-2.0"}
        },
        "servers": [{"name": "this engine", "url": "/rpc"}],
        "methods": methods,
        "components": {"schemas": components()},
        "x-notifications": crate::rpc::NOTIFICATIONS,
    })
}

fn all_methods() -> Vec<Value> {
    let sim_time = json!({"$ref": "#/components/schemas/SimTimeNs"});
    let run_state = json!({"$ref": "#/components/schemas/RunState"});
    let node_id = json!({"$ref": "#/components/schemas/NodeId"});
    let sha = json!({"$ref": "#/components/schemas/Sha256Hex"});
    let validation = json!({"type": "array",
                            "items": {"$ref": "#/components/schemas/ValidationError"}});
    let provenance = json!({"type": "array",
                            "items": {"$ref": "#/components/schemas/Provenance"}});
    let state_and_t = object(
        &["state", "t_ns"],
        json!({"state": run_state, "t_ns": sim_time}),
    );

    let out = vec![
        method(
            "run.start",
            "Load a scenario and begin producing the stream.",
            object(
                &[],
                json!({
                    "scenario": {"oneOf": [{"type": "string"}, {"type": "object"}]},
                    "seed": {"type": "integer", "minimum": 0},
                    "speed": {"type": "number", "minimum": 0, "maximum": 100, "default": 1},
                    "paused": {"type": "boolean", "default": false},
                    "record": {"type": "boolean", "default": true},
                    "record_path": {"type": "string"},
                    "label": {"type": "string", "maxLength": 120}
                }),
            ),
            object(
                &["run_id", "state", "world_hash", "scenario_hash"],
                json!({"run_id": {"$ref": "#/components/schemas/RunId"},
                       "state": run_state, "world_hash": sha, "scenario_hash": sha,
                       "recording_path": {"type": "string"}, "t_end_ns": sim_time}),
            ),
            &[-32001, -32004, -32005, -32013],
        ),
        method(
            "run.pause",
            "Pause at the next mobility-step boundary, after flushing every frame up to it.",
            object(&[], json!({})),
            state_and_t.clone(),
            &[-32002],
        ),
        method(
            "run.resume",
            "Resume a paused run.",
            object(&[], json!({})),
            state_and_t.clone(),
            &[-32002],
        ),
        method(
            "run.step",
            "Advance a paused run by a number of steps, events, keyframes or seconds.",
            object(
                &[],
                json!({"unit": {"enum": ["step", "event", "keyframe", "second"],
                                "default": "step"},
                       "count": {"type": "integer", "minimum": 1, "maximum": 100_000,
                                 "default": 1}}),
            ),
            object(
                &["state", "t_ns", "stepped"],
                json!({"state": run_state, "t_ns": sim_time,
                       "stepped": {"type": "integer"},
                       "last_event": {"type": "object"}}),
            ),
            &[-32002],
        ),
        method(
            "run.seek",
            "Reposition the stream; the keyframe and deltas arrive before the reply.",
            object(
                &[],
                json!({"t_ns": sim_time,
                       "fraction": {"type": "number", "minimum": 0, "maximum": 1},
                       "event": {"type": "object"},
                       "pause_after": {"type": "boolean", "default": true}}),
            ),
            object(
                &["t_ns", "keyframe_seq", "deltas_applied", "elapsed_ms"],
                json!({"t_ns": sim_time, "keyframe_seq": {"type": "integer"},
                       "deltas_applied": {"type": "integer"},
                       "elapsed_ms": {"type": "number"}, "state": run_state}),
            ),
            &[-32003, -32009],
        ),
        method(
            "run.speed",
            "Set the wall-clock pacing of the producer.",
            object(
                &["speed"],
                json!({"speed": {"type": "number", "minimum": 0, "maximum": 100},
                       "sync": {"enum": ["free", "client"], "default": "free"}}),
            ),
            object(
                &["speed", "sync"],
                json!({"speed": {"type": "number"}, "sync": {"enum": ["free", "client"]}}),
            ),
            &[-32002],
        ),
        method(
            "run.stop",
            "Stop the run and finalise its outputs.",
            object(
                &[],
                json!({"finalize_exports": {"type": "boolean", "default": true}}),
            ),
            object(
                &["state", "t_ns"],
                json!({"state": run_state, "t_ns": sim_time,
                       "recording_path": {"type": "string"}, "digest": sha,
                       "files": {"type": "array", "items": {"type": "object"}}}),
            ),
            &[-32002, -32008],
        ),
        method(
            "run.status",
            "Everything about the run's current position and health.",
            object(
                &[],
                json!({"run_id": {"$ref": "#/components/schemas/RunId"}}),
            ),
            json!({"type": "object",
                   "required": ["run_id", "state", "t_ns", "t_end_ns", "speed", "profile",
                                "live"],
                   "properties": {"run_id": {"$ref": "#/components/schemas/RunId"},
                                  "state": run_state, "t_ns": sim_time,
                                  "t_end_ns": sim_time, "speed": {"type": "number"},
                                  "sync": {"enum": ["free", "client"]},
                                  "profile": {"enum": ["full", "node"]},
                                  "live": {"type": "boolean"},
                                  "actors": {"type": "integer"},
                                  "nodes": {"type": "integer"},
                                  "seq": {"type": "integer"},
                                  "dropped": {"type": "object"},
                                  "warnings": validation}}),
            &[-32000],
        ),
        method(
            "view.follow",
            "Follow a node and subscribe it to Telemetry frames.",
            object(
                &[],
                json!({"node": node_id, "actor": {"$ref": "#/components/schemas/ActorId"},
                       "camera": {"$ref": "#/components/schemas/CameraMode"},
                       "clear": {"type": "boolean", "default": false},
                       "telemetry": {"type": "boolean", "default": true},
                       "radius_m": {"type": "number", "minimum": 0, "maximum": 5000,
                                    "default": 0},
                       "feed": {"description": "v1.1: push the followed node's messages and \
                                                queues as `node.feed` notifications while it \
                                                is followed; false drops the subscription",
                                "oneOf": [
                                    {"type": "boolean"},
                                    {"type": "object", "additionalProperties": false,
                                     "properties": {
                                        "sent": {"type": "integer", "minimum": 0,
                                                 "maximum": 200, "default": 20},
                                        "received": {"type": "integer", "minimum": 0,
                                                     "maximum": 500, "default": 40},
                                        "waiting": {"type": "integer", "minimum": 0,
                                                    "maximum": 100, "default": 8},
                                        "bytes": {"type": "boolean", "default": true},
                                        "hz": {"type": "number", "minimum": 0.2,
                                               "maximum": 20, "default": 4}}}]}}),
            ),
            object(
                &["following", "subscribed_nodes"],
                json!({"following": {"type": ["integer", "null"]},
                       "camera": {"$ref": "#/components/schemas/CameraMode"},
                       "subscribed_nodes": {"type": "array", "items": node_id},
                       "feed": {"type": "object", "required": ["v"],
                                "properties": {"v": {"const": crate::feed::FEED_VERSION},
                                               "available": {"type": "boolean"},
                                               "reason": {"type": "string"},
                                               "hz": {"type": "number"},
                                               "sent": {"type": "integer"},
                                               "received": {"type": "integer"},
                                               "waiting": {"type": "integer"},
                                               "bytes": {"type": "boolean"}}}}),
            ),
            &[-32006, -32009],
        ),
        method(
            "view.camera",
            "Drive the client's camera from a script, a notebook or the copilot.",
            object(
                &["mode"],
                json!({"mode": {"$ref": "#/components/schemas/CameraMode"},
                       "target": {"$ref": "#/components/schemas/Vec3"},
                       "position": {"$ref": "#/components/schemas/Vec3"},
                       "fov_deg": {"type": "number", "minimum": 1, "maximum": 150},
                       "projection": {"enum": ["perspective", "orthographic"],
                                      "default": "perspective"},
                       "extent_m": {"type": "number", "minimum": 1},
                       "node": node_id,
                       "animate_ms": {"type": "integer", "minimum": 0, "maximum": 10_000,
                                      "default": 800}}),
            ),
            object(
                &["mode", "position", "target", "fov_deg", "projection"],
                json!({"mode": {"$ref": "#/components/schemas/CameraMode"},
                       "position": {"$ref": "#/components/schemas/Vec3"},
                       "target": {"$ref": "#/components/schemas/Vec3"},
                       "fov_deg": {"type": "number"},
                       "projection": {"enum": ["perspective", "orthographic"]}}),
            ),
            &[-32006, -32602, -32009],
        ),
        method(
            "overlay.set",
            "Turn rendering overlays on and off, or list the catalogue.",
            object(
                &[],
                json!({"overlays": {"type": "object",
                                    "additionalProperties": {"type": "boolean"}},
                       "opacity": {"type": "object",
                                   "additionalProperties": {"type": "number",
                                                            "minimum": 0, "maximum": 1}},
                       "list": {"type": "boolean", "default": false}}),
            ),
            object(
                &["overlays"],
                json!({"overlays": {"type": "object",
                                    "additionalProperties": {"type": "boolean"}},
                       "catalogue": {"type": "array", "items": {"type": "object"}}}),
            ),
            &[-32040, -32602],
        ),
        method(
            "inspect.node",
            "The HUD and inspector payload for one node, as JSON, on demand.",
            object(
                &["node"],
                json!({"node": node_id, "t_ns": sim_time,
                       "include": {"type": "array", "uniqueItems": true,
                                   "items": {"enum": ["telemetry", "stores", "queues",
                                                      "neighbors", "certs", "crl", "gnss",
                                                      "clock", "apps", "detectors",
                                                      "provenance", "messages"]}},
                       "limit": {"type": "integer", "minimum": 1, "maximum": 1000,
                                 "default": 50}}),
            ),
            json!({"type": "object",
                   "required": ["node", "t_ns", "kind", "profile_id"],
                   "properties": {"node": node_id, "t_ns": sim_time,
                                  "kind": {"enum": ["obu", "vru-device", "rsu",
                                                    "base-station", "router",
                                                    "backend-entity", "other"]},
                                  "label": {"type": "string"},
                                  "profile_id": {"type": "string"},
                                  "provenance": provenance}}),
            &[-32006, -32040],
        ),
        method(
            "inspect.link",
            "The radio or backhaul link between two nodes over a window.",
            object(
                &[],
                json!({"tx": node_id, "rx": node_id, "link": {"type": "string"},
                       "t_ns": sim_time, "window_ns": sim_time}),
            ),
            json!({"type": "object", "required": ["kind", "t_ns"],
                   "properties": {"kind": {"enum": ["radio", "backhaul", "uu",
                                                    "backend-net"]},
                                  "t_ns": sim_time,
                                  "distance_m": {"type": "number"},
                                  "provenance": provenance}}),
            &[-32006, -32040],
        ),
        method(
            "inspect.entity",
            "A backend entity, the MA, or any plug-in exposing a StateView.",
            object(
                &["entity"],
                json!({"entity": {"type": "string"}, "t_ns": sim_time,
                       "limit": {"type": "integer", "minimum": 1, "maximum": 1000,
                                 "default": 50}}),
            ),
            json!({"type": "object", "required": ["entity", "t_ns", "role", "state"],
                   "properties": {"entity": {"type": "string"}, "t_ns": sim_time,
                                  "role": {"type": "string"},
                                  "state": {"type": "object"},
                                  "provenance": provenance}}),
            &[-32006, -32040],
        ),
        method(
            "explain",
            "Resolve a displayed value back to the model that produced it.",
            object(
                &["subject"],
                json!({"subject": {"$ref": "#/components/schemas/ValueRef"},
                       "depth": {"type": "integer", "minimum": 1, "maximum": 5,
                                 "default": 1},
                       "format": {"enum": ["json", "markdown"], "default": "json"}}),
            ),
            object(
                &["subject", "chain"],
                json!({"subject": {"$ref": "#/components/schemas/ValueRef"},
                       "value": {}, "unit": {"type": "string"},
                       "chain": {"type": "array", "minItems": 1,
                                 "items": {"$ref": "#/components/schemas/Provenance"}},
                       "definition_md": {"type": "string"},
                       "markdown": {"type": "string"},
                       "caveats": {"type": "array", "items": {"type": "string"}}}),
            ),
            &[-32006, -32007, -32040],
        ),
        method(
            "scenario.get",
            "The scenario document, or a JSON Pointer into it.",
            object(
                &[],
                json!({"path": {"type": "string"},
                       "resolved": {"type": "boolean", "default": true},
                       "with_schema": {"type": "boolean", "default": false}}),
            ),
            object(
                &["scenario", "hash"],
                json!({"scenario": {}, "hash": sha, "schema": {"type": "object"},
                       "fields": {"type": "array", "items": {"type": "object"}},
                       "groups": {"type": "array", "items": {"type": "object"}},
                       "slots": {"type": "array", "items": {"type": "object"}},
                       "statuses": {"type": "array", "items": {"type": "object"}},
                       "models": {"type": "object"},
                       "validator": {"type": "object"}}),
            ),
            &[-32602],
        ),
        method(
            "scenario.schema",
            "The generated settings surface: every scenario field with its unit, default, \
             range, description and implementation status, every swappable model slot with \
             what is selectable in it, and every registered model's parameters with their \
             sources. A property of the build, not of the run.",
            object(
                &[],
                json!({"sections": {"type": "array",
                                    "items": {"enum": ["version", "engine",
                                                       "generated_from", "validator",
                                                       "groups", "statuses", "schema",
                                                       "fields", "slots", "models"]}}}),
            ),
            object(
                &["version", "engine"],
                json!({"version": {"type": "string"}, "engine": {"type": "object"},
                       "generated_from": {"type": "array", "items": {"type": "string"}},
                       "validator": {"type": "object"},
                       "groups": {"type": "array", "items": {"type": "object"}},
                       "statuses": {"type": "array", "items": {"type": "object"}},
                       "schema": {"type": "object"},
                       "fields": {"type": "array", "items": {"type": "object"}},
                       "slots": {"type": "array", "items": {"type": "object"}},
                       "models": {"type": "object"}}),
            ),
            &[-32602],
        ),
        method(
            "scenario.set",
            "Replace or patch the scenario, validating it first.",
            object(
                &[],
                json!({"scenario": {"type": "object"},
                       "patch": {"type": "array", "items": {"type": "object"}},
                       "validate": {"type": "boolean", "default": true},
                       "apply_live": {"type": "boolean", "default": false}}),
            ),
            object(
                &["hash", "valid"],
                json!({"hash": sha, "valid": {"type": "boolean"},
                       "errors": validation,
                       "applied_live": {"type": "array", "items": {"type": "string"}},
                       "requires_restart": {"type": "array", "items": {"type": "string"}}}),
            ),
            &[-32004, -32602],
        ),
        method(
            "scenario.validate",
            "Validate a scenario and estimate what running it would cost.",
            object(
                &[],
                json!({"scenario": {"type": "object"},
                       "strict": {"type": "boolean", "default": false}}),
            ),
            object(
                &["valid", "errors", "warnings"],
                json!({"valid": {"type": "boolean"}, "errors": validation,
                       "warnings": validation,
                       "resolved_tiers": {"type": "object"},
                       "estimated_cost": {"type": "object"}}),
            ),
            &[-32602],
        ),
        method(
            "scenario.save",
            "Write the scenario to a file.",
            object(
                &["path"],
                json!({"path": {"type": "string"}, "scenario": {"type": "object"},
                       "overwrite": {"type": "boolean", "default": false},
                       "format": {"enum": ["yaml", "json"], "default": "yaml"}}),
            ),
            object(
                &["path", "hash", "bytes"],
                json!({"path": {"type": "string"}, "hash": sha,
                       "bytes": {"type": "integer"}}),
            ),
            &[-32013, -32004],
        ),
        method(
            "scenario.load",
            "Read a scenario from a file or a preset id.",
            object(
                &["path"],
                json!({"path": {"type": "string"},
                       "validate": {"type": "boolean", "default": true}}),
            ),
            object(
                &["hash", "valid", "scenario"],
                json!({"hash": sha, "valid": {"type": "boolean"},
                       "scenario": {"type": "object"}, "errors": validation}),
            ),
            &[-32013, -32004],
        ),
        method(
            "scenario.list",
            "List presets, saved scenarios and runs.",
            object(
                &[],
                json!({"kind": {"enum": ["presets", "saved", "runs", "all"],
                                "default": "all"},
                       "prefix": {"type": "string"},
                       "limit": {"type": "integer", "minimum": 1, "maximum": 1000,
                                 "default": 100}}),
            ),
            object(
                &["items"],
                json!({"items": {"type": "array", "items": {"type": "object"}}}),
            ),
            &[-32013],
        ),
        method(
            "world.import_osm",
            "Import lane-level geometry from OpenStreetMap.",
            object(
                &[],
                json!({"bbox": {"type": "array", "minItems": 4, "maxItems": 4,
                                "items": {"type": "number"}},
                       "file": {"type": "string"},
                       "buildings": {"type": "boolean", "default": true},
                       "terrain": {"oneOf": [{"type": "boolean"}, {"type": "string"}]},
                       "simplify_tolerance_m": {"type": "number", "minimum": 0,
                                                "maximum": 5, "default": 0.25},
                       "default_levels_height_m": {"type": "number", "minimum": 1,
                                                   "maximum": 10, "default": 3.0},
                       "lane_inference": {"enum": ["osm2streets", "sumo-netconvert"],
                                          "default": "osm2streets"},
                       "cache": {"type": "boolean", "default": true}}),
            ),
            json!({"oneOf": [{"$ref": "#/components/schemas/Job"},
                             {"$ref": "#/components/schemas/WorldImportResult"}]}),
            &[-32005, -32013, -32010],
        ),
        method(
            "world.generate",
            "Generate a synthetic world; deterministic in its parameters and seed.",
            object(
                &["kind"],
                json!({"kind": {"enum": ["grid", "manhattan", "highway", "ring",
                                         "intersection", "custom"]},
                       "size_m": {"type": "array", "minItems": 2, "maxItems": 2,
                                  "items": {"type": "number", "minimum": 50}},
                       "block_m": {"type": "number", "minimum": 20, "default": 120},
                       "lanes_per_direction": {"type": "integer", "minimum": 1,
                                               "maximum": 6, "default": 2},
                       "lane_width_m": {"type": "number", "minimum": 2.0, "maximum": 5.0,
                                        "default": 3.25},
                       "speed_limit_mps": {"type": "number", "minimum": 1,
                                           "default": 13.89},
                       "buildings": {"type": "object"},
                       "signals": {"type": "boolean", "default": true},
                       "seed": {"type": "integer", "minimum": 0, "default": 0},
                       "params": {"type": "object"}}),
            ),
            json!({"oneOf": [{"$ref": "#/components/schemas/Job"},
                             {"$ref": "#/components/schemas/WorldImportResult"}]}),
            &[-32602, -32013],
        ),
        method(
            "events.set",
            "Choose which event channels this connection receives, and at what rate.",
            object(
                &[],
                json!({"subscribe": {"type": "array",
                                     "items": {"$ref": "#/components/schemas/ChannelName"}},
                       "unsubscribe": {"type": "array",
                                       "items": {"$ref": "#/components/schemas/ChannelName"}},
                       "only": {"type": "array",
                                "items": {"$ref": "#/components/schemas/ChannelName"}},
                       "filter": {"type": "object"},
                       "max_events_per_step": {"type": "integer", "minimum": 0,
                                               "maximum": 1_000_000, "default": 5000},
                       "list": {"type": "boolean", "default": false}}),
            ),
            object(
                &["subscribed"],
                json!({"subscribed": {"type": "array", "items": {"type": "object"}},
                       "available": {"type": "array", "items": {"type": "object"}}}),
            ),
            &[-32040, -32602],
        ),
        method(
            "metrics.query",
            "Query the metric store, or list the catalogue.",
            object(
                &[],
                json!({"metrics": {"type": "array", "items": {"type": "string"}},
                       "t_from_ns": sim_time, "t_to_ns": sim_time, "bin_ns": sim_time,
                       "group_by": {"type": "array", "items": {"type": "string"}},
                       "where": {"type": "object"},
                       "runs": {"type": "array",
                                "items": {"$ref": "#/components/schemas/RunId"}},
                       "agg": {"enum": ["sum", "mean", "p50", "p95", "p99", "ratio",
                                        "rate", "max", "min"]},
                       "format": {"enum": ["json", "arrow"], "default": "json"},
                       "limit": {"type": "integer", "minimum": 1, "maximum": 1_000_000,
                                 "default": 10_000}}),
            ),
            object(
                &["columns", "rows"],
                json!({"columns": {"type": "array", "items": {"type": "object"}},
                       "rows": {"type": "array", "items": {"type": "array"}},
                       "truncated": {"type": "boolean"},
                       "arrow_url": {"type": "string"},
                       "catalogue": {"type": "array", "items": {"type": "object"}},
                       "provenance": provenance,
                       "group_by": {"type": "array", "items": {"type": "string"}}}),
            ),
            &[-32007, -32040, -32602],
        ),
        method(
            "metrics.plot",
            "A Plotly figure for one or more metrics, identical in Studio and notebook.",
            object(
                &["metric"],
                json!({"metric": {"type": ["string", "array"]},
                       "x": {"enum": ["t", "dist_bin", "density_bin", "node", "class",
                                      "run", "config"], "default": "t"},
                       "by": {"type": "array", "items": {"type": "string"}},
                       "runs": {"type": "array",
                                "items": {"$ref": "#/components/schemas/RunId"}},
                       "kind": {"enum": ["line", "scatter", "bar", "box", "heatmap",
                                         "cdf"], "default": "line"},
                       "preset": {"type": "string"},
                       "ci": {"type": "number", "minimum": 0, "maximum": 1},
                       "render": {"enum": ["figure", "svg", "png", "csv"],
                                  "default": "figure"},
                       "width_px": {"type": "integer", "minimum": 100, "maximum": 4000,
                                    "default": 900},
                       "height_px": {"type": "integer", "minimum": 100, "maximum": 4000,
                                     "default": 500}}),
            ),
            object(
                &["figure"],
                json!({"figure": {"type": "object"}, "url": {"type": "string"},
                       "manifest_hash": sha, "provenance": provenance}),
            ),
            &[-32007, -32040],
        ),
        method(
            "export.dataset",
            "Run a dataset exporter; the leakage linter's verdict is part of the result.",
            object(
                &["exporter"],
                json!({"exporter": {"enum": ["ma-dataset", "receiver-logs", "telemetry",
                                             "net-trace", "backend-log", "metrics"]},
                       "out_dir": {"type": "string"}, "opts": {"type": "object"},
                       "visibility": {"enum": ["node", "gt", "both"], "default": "both"},
                       "t_from_ns": sim_time, "t_to_ns": sim_time,
                       "compress": {"enum": ["none", "zstd", "gzip"], "default": "zstd"}}),
            ),
            json!({"oneOf": [{"$ref": "#/components/schemas/Job"},
                             {"type": "object", "required": ["files", "digest"]}]}),
            &[-32008, -32013, -32040, -32010],
        ),
        method(
            "export.recording",
            "Copy a time range of the recording, optionally stripped to the node profile.",
            object(
                &[],
                json!({"path": {"type": "string"}, "t_from_ns": sim_time,
                       "t_to_ns": sim_time,
                       "channels": {"type": "array",
                                    "items": {"$ref": "#/components/schemas/ChannelName"}},
                       "profile": {"enum": ["full", "node"], "default": "full"},
                       "compression": {"enum": ["zstd", "lz4", "none"], "default": "zstd"},
                       "chunk_mb": {"type": "number", "minimum": 0.25, "maximum": 64,
                                    "default": 4}}),
            ),
            json!({"oneOf": [{"$ref": "#/components/schemas/Job"},
                             {"type": "object",
                              "required": ["path", "sha256", "bytes", "messages",
                                           "channels", "t_from_ns", "t_to_ns"]}]}),
            &[-32008, -32013, -32040],
        ),
        method(
            "experiment.define",
            "Define a parameter sweep and count its cells.",
            object(
                &["name", "sweep"],
                json!({"name": {"type": "string", "maxLength": 120},
                       "base": {"type": ["string", "object"]},
                       "sweep": {"type": "object", "minProperties": 1},
                       "seeds": {"type": "object"},
                       "replications_policy": {"type": "object"},
                       "outputs": {"type": "array", "items": {"type": "string"}},
                       "resources": {"type": "object"}}),
            ),
            object(
                &["experiment_id", "cells", "estimated_wall_s"],
                json!({"experiment_id": {"type": "string"},
                       "cells": {"type": "integer"},
                       "cell_keys": {"type": "array", "items": {"type": "object"}},
                       "estimated_wall_s": {"type": "number"},
                       "warnings": validation}),
            ),
            &[-32004, -32602],
        ),
        method(
            "experiment.run",
            "Run a defined experiment; returns a Job.",
            object(
                &["experiment_id"],
                json!({"experiment_id": {"type": "string"},
                       "resume": {"type": "boolean", "default": true},
                       "cells": {"type": "array", "items": {"type": "integer"}},
                       "runner": {"enum": ["local", "slurm", "k8s"], "default": "local"}}),
            ),
            json!({"allOf": [{"$ref": "#/components/schemas/Job"},
                             {"type": "object",
                              "properties": {"experiment_id": {"type": "string"},
                                             "cells_total": {"type": "integer"},
                                             "cells_skipped": {"type": "integer"}}}]}),
            &[-32011, -32010, -32013],
        ),
        method(
            "experiment.status",
            "Progress of a defined or running experiment.",
            object(
                &[],
                json!({"experiment_id": {"type": "string"}, "job_id": {"type": "string"},
                       "include_cells": {"type": "boolean", "default": false}}),
            ),
            object(
                &["experiment_id", "state", "cells_total", "cells_done"],
                json!({"experiment_id": {"type": "string"},
                       "state": {"enum": ["defined", "queued", "running", "done",
                                          "failed", "cancelled"]},
                       "cells_total": {"type": "integer"},
                       "cells_done": {"type": "integer"},
                       "cells_failed": {"type": "integer"},
                       "progress": {"type": "number", "minimum": 0, "maximum": 1},
                       "eta_s": {"type": "number"},
                       "outputs": {"type": "array", "items": {"type": "string"}},
                       "experiment_manifest_hash": sha,
                       "cells": {"type": "array", "items": {"type": "object"}}}),
            ),
            &[-32011],
        ),
        method(
            "rpc.discover",
            "This document.",
            object(&[], json!({"method": {"type": "string"}})),
            json!({"type": "object", "required": ["openrpc", "info", "methods"]}),
            &[-32601],
        ),
    ];
    debug_assert_eq!(out.len(), METHODS.len());
    out
}
