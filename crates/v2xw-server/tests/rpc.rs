//! The control surface of §6, at the level that needs no socket: parsing, the method
//! inventory, the OpenRPC document and the error-code mapping of §6.4.

use serde_json::{Value, json};
use v2xw_server::error::{ParamError, ServerError};
use v2xw_server::openrpc;
use v2xw_server::rpc::{CONNECTION_SCOPED, METHODS, NOTIFICATIONS, parse};

#[test]
fn the_inventory_is_the_thirty_two_methods_of_section_6_15() {
    // 33 since `scenario.schema` (the generated settings surface, 13-product-direction §2)
    // joined the inventory; the commit that added it did not update this count.
    assert_eq!(
        METHODS.len(),
        33,
        "§6.15: 31 methods plus rpc.discover, plus scenario.schema"
    );
    let mut sorted = METHODS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 33, "no duplicates");
    assert!(METHODS.contains(&"scenario.schema"));
    for group in [
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
    ] {
        assert!(METHODS.contains(&group), "§6.15 names `{group}`");
    }
}

/// Eight in v1.0; v1.1 added `node.feed`, the followed node's messages and queues (§6.14,
/// additive under §8.4).
#[test]
fn the_notification_list_is_the_nine_of_section_6_14() {
    assert_eq!(NOTIFICATIONS.len(), 9);
    for name in [
        "run.state",
        "stream.drop",
        "job.progress",
        "job.done",
        "view.changed",
        "log",
        "validation",
        "experiment.progress",
        "node.feed",
    ] {
        assert!(NOTIFICATIONS.contains(&name), "§6.14 names `{name}`");
    }
}

#[test]
fn the_openrpc_document_describes_every_method_once() {
    let doc = openrpc::document(None);
    assert_eq!(doc["openrpc"], "1.3.2", "conformance R2 wants 1.3.2");
    let methods = doc["methods"].as_array().expect("methods array");
    assert_eq!(methods.len(), METHODS.len());
    let mut names: Vec<&str> = methods
        .iter()
        .map(|m| m["name"].as_str().expect("name"))
        .collect();
    names.sort_unstable();
    let mut expected = METHODS.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected, "the document and the dispatcher agree");
    for m in methods {
        assert!(m["summary"].is_string(), "{} has a summary", m["name"]);
        assert!(
            m["result"]["schema"].is_object(),
            "{} has a result schema",
            m["name"]
        );
        assert!(m["params"].is_array(), "{} has params", m["name"]);
        assert!(m["errors"].is_array(), "{} lists its errors", m["name"]);
    }
    assert!(doc["components"]["schemas"]["SimTimeNs"].is_object());
    assert!(doc["components"]["schemas"]["ValueRef"].is_object());
}

#[test]
fn the_document_can_be_narrowed_to_one_method() {
    let doc = openrpc::document(Some("run.seek"));
    let methods = doc["methods"].as_array().expect("methods");
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0]["name"], "run.seek");
}

#[test]
fn every_schema_ref_in_the_document_resolves() {
    // R2 asks that the document be valid. The cheapest real check is that no `$ref` points
    // at a definition the document does not carry — a dangling one makes every generated
    // client and every generated CLI stub fail at the same place.
    let doc = openrpc::document(None);
    let schemas = doc["components"]["schemas"]
        .as_object()
        .expect("components.schemas");
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    assert!(!refs.is_empty(), "the document uses $ref at all");
    for reference in refs {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .unwrap_or_else(|| panic!("unexpected $ref form `{reference}`"));
        assert!(
            schemas.contains_key(name),
            "$ref `{reference}` has no definition"
        );
    }
}

fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref" {
                    if let Some(s) = child.as_str() {
                        out.push(s.to_string());
                    }
                } else {
                    collect_refs(child, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn the_three_connection_scoped_methods_are_the_ones_section_6_2_names() {
    assert_eq!(CONNECTION_SCOPED.len(), 3);
    for name in ["view.follow", "view.camera", "overlay.set"] {
        assert!(CONNECTION_SCOPED.contains(&name));
        assert!(METHODS.contains(&name));
    }
}

#[test]
fn a_batch_array_is_rejected_with_invalid_request() {
    // §6.1's decision and conformance R10.
    let error = parse(r#"[{"jsonrpc":"2.0","id":1,"method":"run.status"}]"#).expect_err("array");
    assert_eq!(error.code(), -32600);
}

#[test]
fn malformed_json_is_a_parse_error_and_a_bad_object_is_an_invalid_request() {
    assert_eq!(parse("{not json").expect_err("bad json").code(), -32700);
    assert_eq!(
        parse(r#"{"id":1,"method":"run.status"}"#)
            .expect_err("no jsonrpc member")
            .code(),
        -32600
    );
    assert_eq!(
        parse(r#"{"jsonrpc":"1.0","id":1,"method":"run.status"}"#)
            .expect_err("wrong version")
            .code(),
        -32600
    );
    assert_eq!(
        parse(r#"{"jsonrpc":"2.0","id":1}"#)
            .expect_err("no method")
            .code(),
        -32600
    );
    assert_eq!(
        parse(r#"{"jsonrpc":"2.0","id":1,"method":"x","params":[1,2]}"#)
            .expect_err("positional params")
            .code(),
        -32600
    );
}

#[test]
fn a_request_without_an_id_is_a_notification() {
    let request = parse(r#"{"jsonrpc":"2.0","method":"run.status","params":{}}"#).expect("parse");
    assert!(request.id.is_none(), "§6.1: no id means no reply");
    let request = parse(r#"{"jsonrpc":"2.0","id":0,"method":"run.status"}"#).expect("parse");
    assert_eq!(request.id, Some(json!(0)), "id 0 is an id, not an absence");
    assert!(request.params.is_empty(), "omitted params read as {{}}");
}

#[test]
fn every_error_code_of_section_6_4_is_reachable_and_carries_its_data() {
    let cases: Vec<(ServerError, i32, Option<&str>)> = vec![
        (ServerError::Parse("x".into()), -32700, None),
        (ServerError::InvalidRequest("x".into()), -32600, None),
        (ServerError::MethodNotFound("x".into()), -32601, None),
        (
            ServerError::InvalidParams(vec![ParamError::new("/a", "b", "c")]),
            -32602,
            Some("hint"),
        ),
        (ServerError::Internal("x".into()), -32603, None),
        (ServerError::RunNotFound("x".into()), -32000, None),
        (ServerError::RunAlreadyRunning, -32001, None),
        (ServerError::RunNotRunning("x".into()), -32002, None),
        (
            ServerError::SeekOutOfRange {
                min_ns: 1,
                max_ns: 2,
            },
            -32003,
            Some("min_ns"),
        ),
        (
            ServerError::ScenarioInvalid(vec![ParamError::bare("/a", "b")]),
            -32004,
            Some("errors"),
        ),
        (ServerError::WorldNotFound("x".into()), -32005, None),
        (
            ServerError::UnknownId {
                kind: "node",
                id: "7".into(),
            },
            -32006,
            Some("kind"),
        ),
        (
            ServerError::UnknownMetric {
                metric: "x".into(),
                did_you_mean: vec![],
            },
            -32007,
            Some("did_you_mean"),
        ),
        (
            ServerError::ExportFailed {
                stage: "open".into(),
                detail: "d".into(),
            },
            -32008,
            Some("stage"),
        ),
        (
            ServerError::NotSupportedHere("x".into()),
            -32009,
            Some("why"),
        ),
        (
            ServerError::Busy {
                operation: "o".into(),
                job_id: "j".into(),
            },
            -32010,
            Some("operation"),
        ),
        (ServerError::ExperimentNotFound("x".into()), -32011, None),
        (
            ServerError::Io {
                path: "p".into(),
                errno: "ENOENT".into(),
            },
            -32013,
            Some("errno"),
        ),
        (
            ServerError::VisibilityDenied {
                field: "f".into(),
                visibility: "GT",
            },
            -32040,
            Some("visibility"),
        ),
        (ServerError::Unauthorized, -32041, None),
        (
            ServerError::UnsupportedVersion,
            -32050,
            Some("supported_major"),
        ),
    ];
    for (error, code, data_key) in cases {
        assert_eq!(error.code(), code, "code for {error}");
        let object = error.to_rpc_object();
        assert_eq!(object["code"], json!(code));
        assert!(
            object["message"].as_str().is_some_and(|m| !m.is_empty()),
            "{code} has a message"
        );
        match data_key {
            None => assert!(object.get("data").is_none(), "{code} needs no data"),
            Some(key) => {
                let data = object.get("data").expect("data");
                assert!(
                    data.to_string().contains(key),
                    "{code} data must mention `{key}`, got {data}"
                );
            }
        }
    }
}

#[test]
fn invalid_params_data_is_an_array_of_path_message_hint() {
    // Conformance R3 asks for exactly this shape, and a caller that gets an object
    // instead cannot show the field it should highlight.
    let error = ServerError::param("/speed", "outside [0, 100]", "pass 1");
    let data = error.data().expect("data");
    let rows = data.as_array().expect("an array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], "/speed");
    assert!(rows[0]["message"].is_string());
    assert!(rows[0]["hint"].is_string());
}
