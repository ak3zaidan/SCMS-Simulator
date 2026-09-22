//! `v2xw-copilot` — a natural-language assistant over the simulator's own registry.
//!
//! The idea this crate is built on is that a language model knows nothing about this
//! simulator and must never be asked to. Everything a copilot says about a model, a
//! parameter, a metric or a run comes from somewhere machine-readable that already exists:
//!
//! | The question | Where the answer comes from |
//! |---|---|
//! | what does this model do, with what parameters, cited to what | the model cards, [`grounding`] |
//! | what is this metric, on what grid, missing what | the metric definitions, [`grounding`] |
//! | what can be done to a run | the server's 32 JSON-RPC methods, [`tools`] |
//! | is this scenario valid | the engine's own loader and validator, [`scenario`] |
//! | where did this number come from | the run's provenance chain joined to the cards, [`explain`] |
//!
//! Nothing here is a second implementation of any of those. The tool surface is
//! *generated* from [`v2xw_server::openrpc::document`]; the scenario check is
//! [`v2xw_engine::scenario::validate`] itself; the catalogue is a read of
//! [`v2xw_core::registry::Registry`]. A copilot that disagrees with the engine would be
//! worse than useless, so it is built such that it cannot hold a second opinion.
//!
//! # The one rule
//!
//! **Nothing the copilot does reaches a simulation output or a digest.** How that is
//! enforced — and it is enforced structurally, not by convention — is written out in
//! [`boundary`], with the checks that hold it and the demonstration that each check can go
//! red.
//!
//! # Shape
//!
//! ```text
//!   question ──► Copilot ──► LlmProvider  (OpenAI, or anything else behind the trait)
//!                  │  ▲
//!                  │  └── tool results
//!                  ├──► Grounding    model cards, metric definitions   (in process, read)
//!                  ├──► scenario     the engine's loader and validator (in process, read)
//!                  ├──► explain      provenance joined to cards        (in process, read)
//!                  └──► RpcTransport the server's own methods          (out of process)
//! ```
//!
//! # A copilot with no key and no network
//!
//! Every seam has a double, so the whole loop is exercised offline:
//!
//! ```
//! use serde_json::json;
//! use v2xw_copilot::grounding::Grounding;
//! use v2xw_copilot::provider::{ChatMessage, Completion, ScriptedProvider, ToolCall};
//! use v2xw_copilot::session::{Copilot, Policy, ToolOutcome};
//! use v2xw_copilot::tools::ToolSurface;
//! use v2xw_copilot::transport::NoEngine;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let grounding = Grounding::builtin()?;
//! let first_metric = grounding.metric_names().first().copied().unwrap_or("pdr").to_string();
//!
//! let provider = ScriptedProvider::new(vec![
//!     Completion {
//!         message: ChatMessage::calls(None, vec![ToolCall {
//!             id: "c1".to_string(),
//!             name: "registry__metric".to_string(),
//!             arguments: json!({"name": first_metric}).to_string(),
//!         }]),
//!         finish_reason: "tool_calls".to_string(),
//!     },
//!     Completion {
//!         message: ChatMessage::assistant("here is the definition, with its citation"),
//!         finish_reason: "stop".to_string(),
//!     },
//! ]);
//!
//! let mut copilot = Copilot::new(
//!     provider,
//!     NoEngine,
//!     grounding,
//!     ToolSurface::new()?,
//!     Policy::read_only(),
//! );
//! let turn = copilot.ask(&mut Vec::new(), "what does that metric mean?")?;
//! assert!(matches!(turn.steps[0].outcome, ToolOutcome::Ok { .. }));
//! # Ok(())
//! # }
//! ```
//!
//! # A copilot against a running engine
//!
//! ```no_run
//! use v2xw_copilot::http::CurlPost;
//! use v2xw_copilot::openai::OpenAi;
//! use v2xw_copilot::session::{Copilot, Policy};
//! use v2xw_copilot::transport::HttpRpc;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // The key is read from OPENAI_API_KEY and never leaves the Authorization header.
//! let provider = OpenAi::from_env(CurlPost::new())?;
//! let rpc = HttpRpc::new(CurlPost::new(), "http://127.0.0.1:8787");
//! let mut copilot = Copilot::build(provider, rpc, Policy::read_only())?;
//! let turn = copilot.ask(&mut Vec::new(), "packet delivery against distance, both radios")?;
//! println!("{}", turn.reply);
//! for step in &turn.steps {
//!     println!("  called {} ({:?})", step.tool, step.method);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # The rules this crate is written under
//!
//! * **No wall clock.** Not one reading, anywhere. The HTTP timeout is an argument handed
//!   to a child process, which this crate never observes.
//! * **No randomness.** There is no generator in this crate and none may be added
//!   ([`boundary::FORBIDDEN_DEPENDENCIES`]).
//! * **No `std` `HashMap` iteration reaching an output.** Every table here is a
//!   `BTreeMap`, so the catalogue, the prompt and every tool result are in a fixed order.
//! * **No transcendental, and no exported float.** This crate computes no number that any
//!   artefact could carry.
//! * No `unsafe`, and every public item documented.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod boundary;
pub mod error;
pub mod explain;
pub mod grounding;
pub mod http;
pub mod openai;
pub mod provider;
pub mod scenario;
pub mod secret;
pub mod session;
pub mod tools;
pub mod transport;

pub use error::{CopilotError, Result};
pub use explain::{Explanation, ModelExplanation};
pub use grounding::{Citation, Grounding, MetricAnswer, ModelEntry, NotKnown, ParameterAnswer};
pub use http::{CurlPost, HttpPost, HttpRequest, HttpResponse};
pub use openai::OpenAi;
pub use provider::{ChatMessage, ChatRequest, Completion, LlmProvider, Role, ToolCall};
pub use scenario::{DraftError, DraftReport, Stage, check_draft};
pub use secret::Secret;
pub use session::{Copilot, Policy, Step, ToolOutcome, Turn};
pub use tools::{Effect, ToolSpec, ToolSurface};
pub use transport::{HttpRpc, NoEngine, RpcTransport};
