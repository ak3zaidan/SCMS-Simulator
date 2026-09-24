//! §10.7 — the JSON-RPC control surface. Items R1, R7 and R10.
//!
//! R2 (the OpenRPC document is valid) and R3 (`-32602` carries `{path, message, hint}`) are
//! owned by `crates/v2xw-server/tests/rpc.rs`; R9 binds the client. R4, R5, R6 and R8 need a
//! dispatched call over a live session or a wall-clock budget, and are recorded as gaps.

use serde_json::json;
use v2xw_server::error::ServerError;
use v2xw_server::openrpc;
use v2xw_server::rpc::{CONNECTION_SCOPED, METHODS, parse};

/// **R1** — "All 33 methods of §6.15 are implemented and appear in `rpc.discover`."
///
/// 33 since `scenario.schema` (the generated settings surface) joined the inventory; §6.15
/// lists it. The server's own `tests/rpc.rs` counts the same 33.
///
/// The inventory and the self-description have to be the same set. A method in one and not
/// the other is the failure a generated client hits first: `rpc.discover` is how every
/// binding, stub and CLI completion is produced.
#[test]
fn r1_all_thirty_three_methods_are_implemented_and_discoverable() {
    assert_eq!(METHODS.len(), 33, "§6.15 lists 32 methods plus rpc.discover");
    assert!(METHODS.contains(&"scenario.schema"), "§6.15 names scenario.schema");
    let mut unique = METHODS.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 33, "the inventory has a duplicate");

    let doc = openrpc::document(None);
    let described: Vec<&str> = doc["methods"]
        .as_array()
        .expect("rpc.discover returns a methods array")
        .iter()
        .map(|m| m["name"].as_str().expect("every method has a name"))
        .collect();
    let mut described_sorted = described.clone();
    described_sorted.sort_unstable();
    described_sorted.dedup();
    assert_eq!(
        described_sorted, unique,
        "the dispatcher's inventory and rpc.discover disagree"
    );
    assert!(
        described.contains(&"rpc.discover"),
        "rpc.discover must describe itself, or a client cannot bootstrap"
    );

    // Narrowing must not lose the method: `rpc.discover {method}` is how a UI asks for one
    // schema instead of the whole document.
    for name in ["run.seek", "export.dataset", "experiment.run"] {
        let one = openrpc::document(Some(name));
        let methods = one["methods"].as_array().expect("a methods array");
        assert_eq!(methods.len(), 1, "narrowing to {name} returned {methods:?}");
        assert_eq!(methods[0]["name"], name);
    }
}

/// **R7** — "`POST /rpc` accepts the same requests; connection-scoped methods return
/// `-32009`."
///
/// The half that needs no HTTP server: the three methods §6.2 calls connection-scoped are
/// exactly the three the dispatcher knows about, they are all real methods, and the error
/// they raise on the connectionless path carries §6.4's code.
#[test]
fn r7_connection_scoped_methods_are_refused_on_the_http_path() {
    assert_eq!(CONNECTION_SCOPED.len(), 3);
    for method in CONNECTION_SCOPED {
        assert!(
            METHODS.contains(&method),
            "`{method}` is connection-scoped but is not a method"
        );
    }
    assert_eq!(
        CONNECTION_SCOPED,
        ["view.follow", "view.camera", "overlay.set"],
        "§6.2 names these three and no others"
    );

    let refused = ServerError::NotSupportedHere(
        "view.follow needs a connection; call it over the WebSocket".to_string(),
    );
    assert_eq!(refused.code(), -32009, "§6.4 assigns -32009");

    // The control: a method that is not connection-scoped must not be in the list, or
    // every call over HTTP would be refused and the check would look like it was working.
    for method in ["run.status", "metrics.query", "rpc.discover"] {
        assert!(
            !CONNECTION_SCOPED.contains(&method),
            "`{method}` is not connection-scoped"
        );
    }
}

/// **R10** — "JSON-RPC batch arrays are rejected with `-32600`."
#[test]
fn r10_a_json_rpc_batch_array_is_refused_with_32600() {
    let batch = json!([
        {"jsonrpc": "2.0", "id": 1, "method": "run.status"},
        {"jsonrpc": "2.0", "id": 2, "method": "run.status"}
    ])
    .to_string();
    assert_eq!(
        parse(&batch)
            .expect_err("a batch array must be refused")
            .code(),
        -32600,
        "§6.1's decision and R10"
    );

    // The control: the same request on its own is accepted, so the refusal is about the
    // array and not about the content.
    let single = parse(r#"{"jsonrpc":"2.0","id":1,"method":"run.status"}"#)
        .expect("a single request parses");
    assert_eq!(single.method, "run.status");
    assert_eq!(single.id, Some(json!(1)));
    assert!(single.params.is_empty(), "an absent params member means {{}}");

    // An empty array is still an array, which is the case a `len() > 1` check would miss.
    assert_eq!(
        parse("[]").expect_err("an empty batch is still a batch").code(),
        -32600
    );
}
