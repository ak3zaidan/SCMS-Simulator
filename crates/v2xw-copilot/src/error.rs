//! The crate's error type.
//!
//! Every variant names what failed and, where a caller can act on it, what to do. A
//! copilot that cannot answer says so; it never invents.

use thiserror::Error;

/// Anything that can go wrong grounding, planning or executing a copilot turn.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CopilotError {
    /// The built-in registry could not be assembled, so there is nothing to ground on.
    #[error("the model registry could not be built, so no answer can be grounded: {0}")]
    Grounding(String),

    /// The OpenRPC document did not have the shape the tool builder expects.
    #[error("the OpenRPC document is not usable as a tool surface: {0}")]
    BadOpenRpc(String),

    /// A tool name that is not in this copilot's surface.
    #[error(
        "{tool:?} is not a tool this copilot has; the surface is generated from the server's method list and nothing may be added to it by hand"
    )]
    UnknownTool {
        /// The name the model used.
        tool: String,
    },

    /// A tool that would change the run, refused because the policy is read-only.
    #[error(
        "{tool:?} changes the run, and this copilot is read-only; set `Policy::allow_mutation` to permit it"
    )]
    MutationRefused {
        /// The tool that was refused.
        tool: String,
    },

    /// Tool arguments that are not a JSON object.
    #[error("{tool:?} was called with arguments that are not a JSON object: {message}")]
    BadArguments {
        /// The tool.
        tool: String,
        /// What the decoder said.
        message: String,
    },

    /// The remote procedure call failed, or there was no engine to send it to.
    #[error("{method} failed: {message}")]
    Rpc {
        /// The JSON-RPC method.
        method: String,
        /// The server's message, or why there was no server.
        message: String,
    },

    /// The language-model provider refused or failed.
    #[error("the language-model provider failed: {0}")]
    Provider(String),

    /// No API key in the environment.
    ///
    /// The message names the variable and nothing else: a key never appears in an error.
    #[error(
        "no API key: set {variable} in the environment (it is read once, never logged, never written to a file and never sent anywhere but the API)"
    )]
    MissingApiKey {
        /// The environment variable that was checked.
        variable: String,
    },

    /// The HTTP transport failed below the provider.
    #[error("the HTTP transport failed: {0}")]
    Transport(String),

    /// A reply that could not be read.
    #[error("the reply could not be read: {0}")]
    Decode(String),

    /// A budget the policy sets was exhausted.
    #[error(
        "the copilot exhausted its {what} budget of {budget}; it stopped rather than continuing unsupervised"
    )]
    Budget {
        /// Which budget: `round` or `tool-call`.
        what: &'static str,
        /// The limit that was reached.
        budget: usize,
    },

    /// JSON that would not encode or decode.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// The crate's result alias.
pub type Result<T> = core::result::Result<T, CopilotError>;
