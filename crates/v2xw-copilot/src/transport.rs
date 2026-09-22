//! How a tool call reaches the engine.
//!
//! This is the whole of the copilot's authority over the simulator, and it is deliberately
//! one method wide: a JSON-RPC method name and a parameter object in, a result out. There
//! is no other path. The copilot does not hold an `Engine`, cannot construct one, and
//! cannot reach any part of a run that the server does not already expose to a browser.
//! Whatever it can do, a person clicking in the Studio can do; whatever it cannot, neither
//! can they.
//!
//! [`NoEngine`] is the default, and it refuses every call with a sentence explaining that
//! no run is attached. That is the right behaviour for a copilot answering registry
//! questions offline, and it means "forgot to attach a server" never looks like "the run
//! said nothing".

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::error::{CopilotError, Result};
use crate::http::{HttpPost, HttpRequest};
use crate::secret::Secret;

/// A way of calling the server's JSON-RPC methods.
pub trait RpcTransport {
    /// Where this transport points, for a transcript.
    fn endpoint(&self) -> &str;

    /// Calls one method.
    ///
    /// # Errors
    /// [`CopilotError::Rpc`] for a JSON-RPC error object or for a server that could not be
    /// reached, and [`CopilotError::Transport`] for a failure below the protocol.
    fn call(&mut self, method: &str, params: &Value) -> Result<Value>;
}

/// The transport for a copilot with no run attached.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoEngine;

impl RpcTransport for NoEngine {
    fn endpoint(&self) -> &str {
        "<no engine attached>"
    }

    fn call(&mut self, method: &str, _params: &Value) -> Result<Value> {
        Err(CopilotError::Rpc {
            method: method.to_string(),
            message: "no engine is attached to this copilot, so nothing can be asked of a \
                      run. Registry, model-card, metric-definition and scenario-checking \
                      questions can still be answered; anything about a run cannot."
                .to_string(),
        })
    }
}

/// A transport that answers from a table, for tests and for a recorded session.
#[derive(Debug, Clone, Default)]
pub struct ScriptedRpc {
    replies: BTreeMap<String, Value>,
    /// Every call that was made, in order, as `(method, params)`.
    pub seen: Vec<(String, Value)>,
}

impl ScriptedRpc {
    /// A transport with no answers; every call fails.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the answer for one method.
    #[must_use]
    pub fn with(mut self, method: impl Into<String>, reply: Value) -> Self {
        self.replies.insert(method.into(), reply);
        self
    }
}

impl RpcTransport for ScriptedRpc {
    fn endpoint(&self) -> &str {
        "<scripted>"
    }

    fn call(&mut self, method: &str, params: &Value) -> Result<Value> {
        self.seen.push((method.to_string(), params.clone()));
        self.replies
            .get(method)
            .cloned()
            .ok_or_else(|| CopilotError::Rpc {
                method: method.to_string(),
                message: "the scripted transport has no answer for this method".to_string(),
            })
    }
}

/// JSON-RPC 2.0 over `POST /rpc`, through an [`HttpPost`].
///
/// The three connection-scoped methods (`view.follow`, `view.camera`, `overlay.set`)
/// answer `-32009` on this path, because the HTTP path has no connection. That is the
/// server's rule, not this crate's, and the error it returns says so.
#[derive(Debug)]
pub struct HttpRpc<H: HttpPost> {
    http: H,
    url: String,
    token: Option<Secret>,
    next_id: u64,
}

impl<H: HttpPost> HttpRpc<H> {
    /// A transport pointing at a server's origin, e.g. `http://127.0.0.1:8787`.
    ///
    /// The `/rpc` path is appended, so a caller passes what
    /// [`v2xw_server::VwpServer::http_url`] returns.
    #[must_use]
    pub fn new(http: H, origin: &str) -> Self {
        HttpRpc {
            http,
            url: format!("{}/rpc", origin.trim_end_matches('/')),
            token: None,
            next_id: 1,
        }
    }

    /// The same, with the bearer token a non-loopback server requires.
    #[must_use]
    pub fn with_token(mut self, token: Secret) -> Self {
        self.token = Some(token);
        self
    }
}

impl<H: HttpPost> RpcTransport for HttpRpc<H> {
    fn endpoint(&self) -> &str {
        &self.url
    }

    fn call(&mut self, method: &str, params: &Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let body = serde_json::to_string(&envelope)?;
        let request = match &self.token {
            Some(t) => HttpRequest::json(self.url.clone(), body, t.clone()),
            None => HttpRequest::json_unauthenticated(self.url.clone(), body),
        };
        let response = self.http.post_json(&request)?;
        let text = request.redact(&response.body);
        let value: Value = serde_json::from_str(&text).map_err(|e| CopilotError::Rpc {
            method: method.to_string(),
            message: format!("HTTP {}: the reply is not JSON: {e}", response.status),
        })?;
        if let Some(error) = value.get("error") {
            return Err(CopilotError::Rpc {
                method: method.to_string(),
                message: rpc_error_message(error),
            });
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| CopilotError::Rpc {
                method: method.to_string(),
                message: format!("HTTP {}: the reply has no result", response.status),
            })
    }
}

/// A JSON-RPC error object as one actionable line.
///
/// The server's `-32602` carries `{path, message, hint}` rows (vwp-v1 §6.4); those are
/// exactly what an author needs, so they are kept rather than flattened away.
fn rpc_error_message(error: &Value) -> String {
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no message");
    let mut out = format!("{code} {message}");
    if let Some(rows) = error
        .get("data")
        .and_then(|d| d.get("errors"))
        .and_then(Value::as_array)
    {
        for row in rows {
            let path = row.get("path").and_then(Value::as_str).unwrap_or("");
            let detail = row.get("message").and_then(Value::as_str).unwrap_or("");
            let hint = row.get("hint").and_then(Value::as_str).unwrap_or("");
            out.push_str(&format!("\n  {path}: {detail}"));
            if !hint.is_empty() {
                out.push_str(&format!(" ({hint})"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpResponse, ScriptedPost};

    #[test]
    fn with_no_engine_a_run_question_says_so_rather_than_failing_silently() {
        let mut t = NoEngine;
        let err = t
            .call("run.status", &json!({}))
            .expect_err("there is no engine");
        assert!(err.to_string().contains("no engine is attached"));
    }

    #[test]
    fn a_result_comes_back_unwrapped() {
        let post = ScriptedPost::new(vec![HttpResponse::new(
            200,
            r#"{"jsonrpc":"2.0","id":1,"result":{"state":"paused","t_ns":42}}"#,
        )]);
        let mut rpc = HttpRpc::new(post, "http://127.0.0.1:8787/");
        let out = rpc.call("run.status", &json!({})).expect("a result");
        assert_eq!(out["state"], json!("paused"));
        assert_eq!(rpc.endpoint(), "http://127.0.0.1:8787/rpc");
        let sent: Value =
            serde_json::from_str(&rpc.http.seen[0].body).expect("the envelope is JSON");
        assert_eq!(sent["jsonrpc"], json!("2.0"));
        assert_eq!(sent["method"], json!("run.status"));
        assert_eq!(sent["id"], json!(1));
    }

    #[test]
    fn a_validation_error_keeps_its_rows() {
        let post = ScriptedPost::new(vec![HttpResponse::new(
            200,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"invalid_params",
                "data":{"errors":[{"path":"/speed","message":"must be 0..100",
                "hint":"omit it for 1"}]}}}"#,
        )]);
        let mut rpc = HttpRpc::new(post, "http://127.0.0.1:8787");
        let err = rpc
            .call("run.speed", &json!({"speed": 900}))
            .expect_err("refused");
        let text = err.to_string();
        assert!(text.contains("-32602"));
        assert!(text.contains("/speed: must be 0..100"));
        assert!(text.contains("omit it for 1"));
    }

    #[test]
    fn the_scripted_transport_records_what_was_asked() {
        let mut rpc = ScriptedRpc::new().with("metrics.query", json!({"rows": []}));
        let out = rpc
            .call("metrics.query", &json!({"metric": "pdr"}))
            .expect("scripted");
        assert_eq!(out["rows"], json!([]));
        assert_eq!(rpc.seen.len(), 1);
        assert_eq!(rpc.seen[0].0, "metrics.query");
        assert!(rpc.call("run.start", &json!({})).is_err());
    }
}
