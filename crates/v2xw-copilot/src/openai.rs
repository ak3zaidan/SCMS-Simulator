//! The OpenAI transport.
//!
//! Chat Completions with function calling, over whatever [`HttpPost`] the caller supplies.
//! The two interesting functions, [`encode_request`] and [`decode_completion`], are pure
//! and are tested against recorded bodies; the only impure part is the one call to
//! [`HttpPost::post_json`] between them.
//!
//! # The key
//!
//! Read once from `OPENAI_API_KEY` ([`KEY_VARIABLE`]) into a [`Secret`], which has no
//! `Display` and no `Serialize`. It goes into the `Authorization` header and nowhere else:
//! not into a log line, not into a file, not into an argument vector (see [`crate::http`]),
//! and not into a transcript — [`crate::session::Turn`] holds tool calls and results, never
//! a request. Anything the API says back is passed through [`Secret::redact`] before it is
//! surfaced, so an endpoint that echoed the key could not make this crate print it.
//!
//! # Model choice
//!
//! `OPENAI_MODEL` overrides [`DEFAULT_MODEL`], and `OPENAI_BASE_URL` overrides the
//! endpoint, which is what an Azure deployment or a local gateway needs. The reasoning
//! tiers reject a non-default `temperature`; a `400` that names the field is retried once
//! without it, so one build works from `gpt-4o-mini` upwards without per-model wiring.

use serde_json::{Map, Value, json};

use crate::error::{CopilotError, Result};
use crate::http::{HttpPost, HttpRequest};
use crate::provider::{ChatMessage, ChatRequest, Completion, LlmProvider, Role, ToolCall};
use crate::secret::Secret;

/// The environment variable the key is read from.
pub const KEY_VARIABLE: &str = "OPENAI_API_KEY";
/// The environment variable that overrides [`DEFAULT_MODEL`].
pub const MODEL_VARIABLE: &str = "OPENAI_MODEL";
/// The environment variable that overrides [`DEFAULT_URL`].
pub const URL_VARIABLE: &str = "OPENAI_BASE_URL";
/// The default endpoint.
pub const DEFAULT_URL: &str = "https://api.openai.com/v1/chat/completions";
/// The default model: the cheapest tier that calls tools reliably.
pub const DEFAULT_MODEL: &str = "gpt-4o-mini";

/// An OpenAI-compatible chat provider.
#[derive(Debug)]
pub struct OpenAi<H: HttpPost> {
    http: H,
    key: Secret,
    model: String,
    url: String,
    temperature: Option<f64>,
    name: String,
}

impl<H: HttpPost> OpenAi<H> {
    /// A provider with an explicit key.
    #[must_use]
    pub fn new(http: H, key: Secret) -> Self {
        let model = DEFAULT_MODEL.to_string();
        OpenAi {
            http,
            key,
            name: format!("openai:{model}"),
            model,
            url: DEFAULT_URL.to_string(),
            // Zero rather than the legacy 0.2: a research assistant that answers the same
            // question two ways is one the reader cannot check. It is not determinism —
            // nothing about a hosted model is deterministic, and nothing this returns
            // reaches a result — but it is the least variance available.
            temperature: Some(0.0),
        }
    }

    /// A provider configured entirely from the environment.
    ///
    /// # Errors
    /// [`CopilotError::MissingApiKey`] if [`KEY_VARIABLE`] is unset or empty.
    pub fn from_env(http: H) -> Result<Self> {
        let key = Secret::from_env(KEY_VARIABLE)?;
        let mut provider = OpenAi::new(http, key);
        if let Ok(model) = std::env::var(MODEL_VARIABLE) {
            let model = model.trim();
            if !model.is_empty() {
                provider = provider.with_model(model);
            }
        }
        if let Ok(url) = std::env::var(URL_VARIABLE) {
            let url = url.trim();
            if !url.is_empty() {
                provider = provider.with_url(url);
            }
        }
        Ok(provider)
    }

    /// The same, with another model.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self.name = format!("openai:{}", self.model);
        self
    }

    /// The same, with another endpoint.
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// The same, with another sampling temperature, or none at all.
    #[must_use]
    pub fn with_temperature(mut self, temperature: Option<f64>) -> Self {
        self.temperature = temperature;
        self
    }

    /// The model this provider talks to.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// One round trip, with the body already encoded.
    fn round_trip(&mut self, body: String) -> Result<Value> {
        let request = HttpRequest::json(self.url.clone(), body, self.key.clone());
        let response = self.http.post_json(&request)?;
        let text = self.key.redact(&response.body);
        if response.is_success() {
            return serde_json::from_str(&text)
                .map_err(|e| CopilotError::Decode(format!("the reply is not JSON: {e}")));
        }
        Err(CopilotError::Provider(format!(
            "HTTP {}: {}",
            response.status,
            api_error_message(&text)
        )))
    }
}

impl<H: HttpPost> LlmProvider for OpenAi<H> {
    fn name(&self) -> &str {
        &self.name
    }

    fn complete(&mut self, request: &ChatRequest) -> Result<Completion> {
        let body = encode_request(&self.model, request, self.temperature)?;
        // Bound before the match: a `&mut self` borrow held in a match scrutinee lasts the
        // whole match, and the arms below read `self.temperature` and `self.model`.
        let first = self.round_trip(body);
        match first {
            Ok(value) => decode_completion(&value),
            Err(CopilotError::Provider(message))
                if self.temperature.is_some()
                    && message.starts_with("HTTP 400")
                    && message.contains("temperature") =>
            {
                // A reasoning tier that refuses a non-default temperature. Drop the field
                // and ask once more, so one build works across model generations.
                let retry = encode_request(&self.model, request, None)?;
                let value = self.round_trip(retry)?;
                decode_completion(&value)
            }
            Err(e) => Err(e),
        }
    }
}

/// The request body for one completion.
///
/// # Errors
/// [`CopilotError::Json`] if the body will not serialise, which plain data does not fail
/// at.
pub fn encode_request(
    model: &str,
    request: &ChatRequest,
    temperature: Option<f64>,
) -> Result<String> {
    let mut body = Map::new();
    body.insert("model".to_string(), json!(model));
    body.insert(
        "messages".to_string(),
        Value::Array(request.messages.iter().map(encode_message).collect()),
    );
    if !request.tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(request.tools.clone()));
        body.insert("tool_choice".to_string(), json!("auto"));
    }
    if let Some(t) = temperature {
        body.insert("temperature".to_string(), json!(t));
    }
    Ok(serde_json::to_string(&Value::Object(body))?)
}

/// One message in the shape the API takes.
fn encode_message(message: &ChatMessage) -> Value {
    let mut out = Map::new();
    out.insert("role".to_string(), json!(message.role.wire()));
    match &message.content {
        Some(text) => out.insert("content".to_string(), json!(text)),
        // An assistant message that only calls tools must still carry the field, as null.
        None => out.insert("content".to_string(), Value::Null),
    };
    if !message.tool_calls.is_empty() {
        let calls: Vec<Value> = message
            .tool_calls
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "type": "function",
                    "function": {"name": c.name, "arguments": c.arguments}
                })
            })
            .collect();
        out.insert("tool_calls".to_string(), Value::Array(calls));
    }
    if let Some(id) = &message.tool_call_id {
        out.insert("tool_call_id".to_string(), json!(id));
    }
    Value::Object(out)
}

/// The assistant message out of a reply body.
///
/// # Errors
/// [`CopilotError::Provider`] when the body is an API error object, and
/// [`CopilotError::Decode`] when it has no first choice with a message.
pub fn decode_completion(value: &Value) -> Result<Completion> {
    if let Some(error) = value.get("error") {
        return Err(CopilotError::Provider(
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the API returned an error with no message")
                .to_string(),
        ));
    }
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or_else(|| CopilotError::Decode("the reply has no choices".to_string()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| CopilotError::Decode("the first choice has no message".to_string()))?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (i, call) in calls.iter().enumerate() {
            let function = call
                .get("function")
                .ok_or_else(|| CopilotError::Decode(format!("tool call {i} has no function")))?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| CopilotError::Decode(format!("tool call {i} has no name")))?;
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .to_string();
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("call_{i}"), str::to_string);
            tool_calls.push(ToolCall {
                id,
                name: name.to_string(),
                arguments,
            });
        }
    }
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop")
        .to_string();
    Ok(Completion {
        message: ChatMessage {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
        },
        finish_reason,
    })
}

/// The `error.message` of an API error body, or the body itself when it is not one.
fn api_error_message(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpResponse, ScriptedPost};

    fn one_tool_call_reply() -> &'static str {
        r#"{"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant",
        "content":null,"tool_calls":[{"id":"call_a","type":"function","function":
        {"name":"registry__parameter","arguments":"{\"model\":\"m\",\"name\":\"p\"}"}}]}}]}"#
    }

    #[test]
    fn a_tool_call_reply_decodes() {
        let value: Value = serde_json::from_str(one_tool_call_reply()).expect("fixture parses");
        let c = decode_completion(&value).expect("decodes");
        assert_eq!(c.finish_reason, "tool_calls");
        assert_eq!(c.message.tool_calls.len(), 1);
        assert_eq!(c.message.tool_calls[0].name, "registry__parameter");
        assert_eq!(c.message.tool_calls[0].id, "call_a");
        assert_eq!(c.message.content, None);
    }

    #[test]
    fn an_error_body_is_a_provider_error_not_a_completion() {
        let value: Value =
            serde_json::from_str(r#"{"error":{"message":"model not found","type":"x"}}"#)
                .expect("fixture parses");
        let err = decode_completion(&value).expect_err("an error body is not a completion");
        assert!(err.to_string().contains("model not found"));
    }

    #[test]
    fn the_request_carries_the_tools_and_the_conversation() {
        let request = ChatRequest {
            messages: vec![
                ChatMessage::system("rules"),
                ChatMessage::user("what is the path loss exponent?"),
                ChatMessage::calls(
                    None,
                    vec![ToolCall {
                        id: "call_a".to_string(),
                        name: "registry__parameter".to_string(),
                        arguments: "{}".to_string(),
                    }],
                ),
                ChatMessage::tool("call_a", "{\"known\":false}"),
            ],
            tools: vec![json!({"type": "function", "function": {"name": "registry__parameter"}})],
        };
        let body = encode_request("gpt-4o-mini", &request, Some(0.0)).expect("encodes");
        let value: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(value["model"], json!("gpt-4o-mini"));
        assert_eq!(value["messages"].as_array().map(Vec::len), Some(4));
        assert_eq!(value["messages"][0]["role"], json!("system"));
        assert_eq!(value["messages"][2]["content"], Value::Null);
        assert_eq!(
            value["messages"][2]["tool_calls"][0]["function"]["name"],
            json!("registry__parameter")
        );
        assert_eq!(value["messages"][3]["tool_call_id"], json!("call_a"));
        assert_eq!(value["tool_choice"], json!("auto"));
        assert_eq!(value["temperature"], json!(0.0));
    }

    #[test]
    fn no_temperature_field_when_it_is_dropped() {
        let body = encode_request("o9", &ChatRequest::default(), None).expect("encodes");
        assert!(!body.contains("temperature"));
    }

    #[test]
    fn the_key_never_reaches_the_body_and_only_the_header_holds_it() {
        let post = ScriptedPost::new(vec![HttpResponse::new(200, one_tool_call_reply())]);
        let mut provider = OpenAi::new(post, Secret::new("sk-secret"));
        let completion = provider
            .complete(&ChatRequest {
                messages: vec![ChatMessage::user("hello")],
                tools: Vec::new(),
            })
            .expect("the scripted reply decodes");
        assert_eq!(completion.message.tool_calls.len(), 1);
        let sent = &provider.http.seen[0];
        assert!(!sent.body.contains("sk-secret"), "the key reached the body");
        assert_eq!(
            sent.bearer.as_ref().map(crate::secret::Secret::expose),
            Some("sk-secret")
        );
        assert_eq!(sent.url, DEFAULT_URL);
    }

    #[test]
    fn an_error_status_is_reported_with_the_key_stripped_out() {
        let post = ScriptedPost::new(vec![HttpResponse::new(
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-secret"}}"#,
        )]);
        let mut provider = OpenAi::new(post, Secret::new("sk-secret"));
        let err = provider
            .complete(&ChatRequest::default())
            .expect_err("401 is not a completion");
        let text = err.to_string();
        assert!(text.contains("HTTP 401"));
        assert!(
            !text.contains("sk-secret"),
            "the key was echoed into an error"
        );
        assert!(text.contains("<redacted>"));
    }

    #[test]
    fn a_temperature_refusal_is_retried_once_without_it() {
        let post = ScriptedPost::new(vec![
            HttpResponse::new(
                400,
                r#"{"error":{"message":"Unsupported value: 'temperature' does not support 0.0"}}"#,
            ),
            HttpResponse::new(200, one_tool_call_reply()),
        ]);
        let mut provider = OpenAi::new(post, Secret::new("sk-secret"));
        let completion = provider
            .complete(&ChatRequest::default())
            .expect("the retry succeeds");
        assert_eq!(completion.finish_reason, "tool_calls");
        assert_eq!(provider.http.calls(), 2);
        assert!(provider.http.seen[0].body.contains("temperature"));
        assert!(!provider.http.seen[1].body.contains("temperature"));
    }
}
