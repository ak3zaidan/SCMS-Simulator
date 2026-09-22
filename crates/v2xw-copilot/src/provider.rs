//! The language-model seam.
//!
//! The owner specified OpenAI, and [`crate::openai`] is that. The seam exists anyway,
//! because a research tool that can only be run against one vendor's endpoint is a
//! research tool with a dependency its results are hostage to: a model deprecated upstream
//! takes the copilot with it, a local model cannot be compared against, and an
//! air-gapped installation cannot run it at all. So the vendor lives behind
//! [`LlmProvider`], which is four small types and one method.
//!
//! [`ScriptedProvider`] is the same seam used the other way: the whole tool loop of
//! [`crate::session`] is exercised with a scripted model, no key and no network.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// Who said something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The instructions the copilot runs under.
    System,
    /// The person.
    User,
    /// The model.
    Assistant,
    /// The result of a tool the model called.
    Tool,
}

impl Role {
    /// The spelling the chat API uses.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// One tool call the model asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The call's id, which the matching tool message quotes back.
    pub id: String,
    /// The tool's name, as [`crate::tools::ToolSpec::name`] spells it.
    pub name: String,
    /// The arguments, as the raw JSON text the model produced. Kept as text because a
    /// model can emit text that is not valid JSON, and the failure has to be reportable
    /// rather than silently turned into an empty object.
    pub arguments: String,
}

/// One message in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Who said it.
    pub role: Role,
    /// The text, when there is any. An assistant message that only calls tools has none.
    pub content: Option<String>,
    /// The tool calls, on an assistant message.
    pub tool_calls: Vec<ToolCall>,
    /// The call this message answers, on a tool message.
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// A message with a role and text and nothing else.
    #[must_use]
    fn plain(role: Role, text: impl Into<String>) -> Self {
        ChatMessage {
            role,
            content: Some(text.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// The instructions.
    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self::plain(Role::System, text)
    }

    /// Something the person said.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self::plain(Role::User, text)
    }

    /// Something the model said.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, text)
    }

    /// An assistant message that calls tools, with or without text beside them.
    #[must_use]
    pub fn calls(text: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        ChatMessage {
            role: Role::Assistant,
            content: text,
            tool_calls,
            tool_call_id: None,
        }
    }

    /// The result of one tool call.
    #[must_use]
    pub fn tool(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        ChatMessage {
            role: Role::Tool,
            content: Some(text.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }

    /// The text, or the empty string.
    #[must_use]
    pub fn text(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

/// One request to a model.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatRequest {
    /// The conversation so far, instructions first.
    pub messages: Vec<ChatMessage>,
    /// The tool definitions, as [`crate::tools::ToolSurface::to_chat_tools`] builds them.
    pub tools: Vec<Value>,
}

/// What a model answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The assistant message.
    pub message: ChatMessage,
    /// Why it stopped: `stop`, `tool_calls`, `length`, …
    pub finish_reason: String,
}

/// A language model the copilot can talk to.
pub trait LlmProvider {
    /// A name for transcripts and errors, e.g. `openai:gpt-4o-mini`.
    fn name(&self) -> &str;

    /// One completion.
    ///
    /// # Errors
    /// [`crate::CopilotError::Provider`] for a refusal or an error status,
    /// [`crate::CopilotError::Transport`] if the request could not be sent, and
    /// [`crate::CopilotError::Decode`] for a reply that could not be read.
    fn complete(&mut self, request: &ChatRequest) -> Result<Completion>;
}

/// A provider that hands back prepared completions, in order.
#[derive(Debug, Default)]
pub struct ScriptedProvider {
    replies: Vec<Completion>,
    used: usize,
    /// Every request it was asked to complete.
    pub seen: Vec<ChatRequest>,
}

impl ScriptedProvider {
    /// A provider that will answer with these, in order.
    #[must_use]
    pub fn new(replies: Vec<Completion>) -> Self {
        ScriptedProvider {
            replies,
            used: 0,
            seen: Vec::new(),
        }
    }

    /// How many completions it has handed out.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.used
    }
}

impl LlmProvider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    fn complete(&mut self, request: &ChatRequest) -> Result<Completion> {
        self.seen.push(request.clone());
        let reply = self.replies.get(self.used).cloned().ok_or_else(|| {
            crate::CopilotError::Provider(format!(
                "the scripted provider has no completion {} (it was given {})",
                self.used + 1,
                self.replies.len()
            ))
        })?;
        self.used += 1;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_message_carries_the_call_it_answers() {
        let m = ChatMessage::tool("call_1", "{\"known\":false}");
        assert_eq!(m.role, Role::Tool);
        assert_eq!(m.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(m.text(), "{\"known\":false}");
    }

    #[test]
    fn the_scripted_provider_refuses_to_invent_a_reply() {
        let mut p = ScriptedProvider::new(Vec::new());
        assert!(p.complete(&ChatRequest::default()).is_err());
    }
}
