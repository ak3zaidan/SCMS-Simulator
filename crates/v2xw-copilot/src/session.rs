//! The turn loop: question in, tool calls out, grounded answer back.
//!
//! [`Copilot`] holds four things — a model ([`LlmProvider`]), a way to reach a run
//! ([`RpcTransport`]), the catalogue ([`Grounding`]) and the tool list ([`ToolSurface`])
//! — and one policy. It has no other state, and in particular it has no handle on an
//! engine, a recorder, a digest or a random-number stream.
//!
//! # What the policy is for
//!
//! [`Policy::allow_mutation`] is enforced twice, and the first enforcement is the real
//! one: with it false, the tools that change a run are **not in the list the model is
//! given**, so there is no call to refuse. [`Copilot::dispatch`] refuses them a second
//! time anyway, because a model can name a tool it was not offered and a defence that only
//! works when the prompt is obeyed is not a defence.
//!
//! [`Policy::max_rounds`] and [`Policy::max_tool_calls`] stop a loop rather than letting
//! it run up a bill, and [`Policy::max_result_bytes`] stops one enormous `metrics.query`
//! from filling the conversation. A truncated result says it was truncated; it is never
//! silently shortened.
//!
//! # The transcript
//!
//! Every turn returns a [`Turn`] whose [`Turn::steps`] hold each call, its arguments and
//! its outcome. 09-ui.md §6 asks the copilot panel to show "each call and result", and
//! this is that, in a form a UI can render and a test can assert on. It holds no key and
//! no request body.

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::error::{CopilotError, Result};
use crate::explain::explain;
use crate::grounding::Grounding;
use crate::provider::{ChatMessage, ChatRequest, LlmProvider};
use crate::scenario::{check_document, check_draft};
use crate::tools::{Effect, ToolSurface};
use crate::transport::RpcTransport;

/// What a copilot is allowed to do in a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Whether the tools that change a run are offered at all.
    pub allow_mutation: bool,
    /// How many times the model may be asked in one turn.
    pub max_rounds: usize,
    /// How many tools it may call in one turn.
    pub max_tool_calls: usize,
    /// The largest tool result, in bytes, that is passed back to the model.
    pub max_result_bytes: usize,
}

impl Default for Policy {
    /// Read-only. A copilot that can stop a run by misreading a sentence is not a default
    /// anyone should get without asking for it.
    fn default() -> Self {
        Policy {
            allow_mutation: false,
            max_rounds: 8,
            max_tool_calls: 24,
            max_result_bytes: 64 * 1024,
        }
    }
}

impl Policy {
    /// The read-only policy, named rather than implied.
    #[must_use]
    pub fn read_only() -> Self {
        Policy::default()
    }

    /// The same, with the tools that change a run offered.
    #[must_use]
    pub fn allowing_mutation() -> Self {
        Policy {
            allow_mutation: true,
            ..Policy::default()
        }
    }
}

/// What one tool call produced.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ToolOutcome {
    /// It ran and returned this. A registry lookup that found nothing is still this, with
    /// `known: false` inside: "not known" is an answer, not a failure.
    Ok {
        /// The result.
        result: Value,
    },
    /// The policy or the surface refused it before anything was called.
    Refused {
        /// Why.
        reason: String,
    },
    /// It was attempted and failed.
    Failed {
        /// What went wrong, in the caller's terms.
        message: String,
    },
}

/// One call in a turn.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    /// The tool's name.
    pub tool: String,
    /// The JSON-RPC method behind it, or `None` for a tool answered in process.
    pub method: Option<String>,
    /// The arguments, as they were parsed.
    pub arguments: Value,
    /// What happened.
    pub outcome: ToolOutcome,
}

/// One answered question.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Turn {
    /// The answer.
    pub reply: String,
    /// Every call made on the way, in order.
    pub steps: Vec<Step>,
    /// How many times the model was asked.
    pub rounds: usize,
}

/// The assistant.
#[derive(Debug)]
pub struct Copilot<P: LlmProvider, T: RpcTransport> {
    provider: P,
    rpc: T,
    grounding: Grounding,
    tools: ToolSurface,
    policy: Policy,
    system_prompt: String,
}

impl<P: LlmProvider, T: RpcTransport> Copilot<P, T> {
    /// A copilot over an explicit catalogue and tool surface.
    #[must_use]
    pub fn new(
        provider: P,
        rpc: T,
        grounding: Grounding,
        tools: ToolSurface,
        policy: Policy,
    ) -> Self {
        let system_prompt = system_prompt(&grounding, &policy, rpc.endpoint());
        Copilot {
            provider,
            rpc,
            grounding,
            tools,
            policy,
            system_prompt,
        }
    }

    /// A copilot over this build's own registry and this build's own method list.
    ///
    /// # Errors
    /// [`CopilotError::Grounding`] if the registry cannot be built, and
    /// [`CopilotError::BadOpenRpc`] if the server's document cannot be translated.
    pub fn build(provider: P, rpc: T, policy: Policy) -> Result<Self> {
        let grounding = Grounding::builtin()?;
        let tools = ToolSurface::new()?;
        Ok(Copilot::new(provider, rpc, grounding, tools, policy))
    }

    /// The catalogue it answers from.
    #[must_use]
    pub fn grounding(&self) -> &Grounding {
        &self.grounding
    }

    /// The tools it has.
    #[must_use]
    pub fn tools(&self) -> &ToolSurface {
        &self.tools
    }

    /// The policy it runs under.
    #[must_use]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// The instructions it runs under, for a UI that wants to show them.
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// The model it talks to.
    #[must_use]
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    /// Answers one question, appending everything said to `history`.
    ///
    /// `history` holds the conversation *without* the instructions, which are prepended on
    /// every request from [`Copilot::system_prompt`]. A caller keeps one `history` per
    /// chat panel.
    ///
    /// # Errors
    /// Whatever the provider returns, or [`CopilotError::Budget`] when the turn hits a
    /// limit. A tool that fails is not an error: it is a [`Step`] with a failed outcome,
    /// handed back to the model so it can say what went wrong.
    pub fn ask(&mut self, history: &mut Vec<ChatMessage>, question: &str) -> Result<Turn> {
        history.push(ChatMessage::user(question));
        let tools = self.tools.to_chat_tools(self.policy.allow_mutation);
        let mut steps: Vec<Step> = Vec::new();
        let mut calls = 0usize;

        for round in 1..=self.policy.max_rounds {
            let mut messages = Vec::with_capacity(history.len() + 1);
            messages.push(ChatMessage::system(self.system_prompt.clone()));
            messages.extend(history.iter().cloned());
            let completion = self.provider.complete(&ChatRequest {
                messages,
                tools: tools.clone(),
            })?;
            history.push(completion.message.clone());

            if completion.message.tool_calls.is_empty() {
                return Ok(Turn {
                    reply: completion.message.text().to_string(),
                    steps,
                    rounds: round,
                });
            }

            for call in &completion.message.tool_calls {
                if calls >= self.policy.max_tool_calls {
                    return Err(CopilotError::Budget {
                        what: "tool-call",
                        budget: self.policy.max_tool_calls,
                    });
                }
                calls += 1;
                let parsed = parse_arguments(&call.arguments);
                let outcome = match &parsed {
                    Ok(arguments) => self.dispatch(&call.name, arguments),
                    Err(message) => ToolOutcome::Failed {
                        message: message.clone(),
                    },
                };
                history.push(ChatMessage::tool(call.id.clone(), payload(&outcome)));
                steps.push(Step {
                    tool: call.name.clone(),
                    method: self.tools.get(&call.name).and_then(|s| s.method.clone()),
                    arguments: parsed.unwrap_or(Value::Null),
                    outcome,
                });
            }
        }

        Err(CopilotError::Budget {
            what: "round",
            budget: self.policy.max_rounds,
        })
    }

    /// Runs one tool. Public so a UI can replay a transcript step without a model.
    #[must_use]
    pub fn dispatch(&mut self, tool: &str, arguments: &Value) -> ToolOutcome {
        let Some(spec) = self.tools.get(tool) else {
            return ToolOutcome::Refused {
                reason: format!(
                    "{tool:?} is not a tool this copilot has. The surface is generated from \
                     the server's method list; nothing can be added to it from a message."
                ),
            };
        };
        // Copied out so the immutable borrow of `self.tools` ends before the transport is
        // used mutably.
        let method = spec.method.clone();
        let effect = spec.effect;

        if effect == Effect::Mutate && !self.policy.allow_mutation {
            return ToolOutcome::Refused {
                reason: format!(
                    "{tool:?} changes the run and this copilot is read-only. A person can \
                     call it from the Studio, or the session can be started with mutation \
                     allowed."
                ),
            };
        }

        let Some(method) = method else {
            return local_call(&self.grounding, tool, arguments);
        };
        // Bound before the match so that the mutable borrow of the transport ends before
        // `bound` takes `&self`: a borrow held in a match scrutinee lasts the whole match.
        let called = self.rpc.call(&method, arguments);
        match called {
            Ok(result) => ToolOutcome::Ok {
                result: self.bound(result),
            },
            Err(e) => ToolOutcome::Failed {
                message: e.to_string(),
            },
        }
    }

    /// Keeps one result from filling the conversation, saying so when it does.
    fn bound(&self, value: Value) -> Value {
        let Ok(text) = serde_json::to_string(&value) else {
            return value;
        };
        if text.len() <= self.policy.max_result_bytes {
            return value;
        }
        json!({
            "truncated": true,
            "bytes": text.len(),
            "limit": self.policy.max_result_bytes,
            "note": "the result was larger than this copilot's per-result limit and was \
                     not passed on. Ask again for less of it: fewer bins, a shorter \
                     window, one dimension at a time."
        })
    }
}

/// The tools this crate answers itself, out of the catalogue.
fn local_call(grounding: &Grounding, tool: &str, arguments: &Value) -> ToolOutcome {
    match tool {
        "registry__list_models" => {
            let family = arguments.get("family").and_then(Value::as_str);
            let entries = match family {
                Some(f) => grounding.models_in_family(f),
                None => grounding
                    .model_ids()
                    .into_iter()
                    .filter_map(|id| grounding.model(id))
                    .collect(),
            };
            let rows: Vec<Value> = entries
                .iter()
                .map(|e| {
                    json!({
                        "id": e.citation.model_id,
                        "version": e.citation.model_version,
                        "family": e.citation.family,
                        "tiers": e.tiers,
                        "validation": e.validation,
                        "purpose": e.purpose,
                        "card_content_hash": e.citation.card_content_hash,
                    })
                })
                .collect();
            let known_families = grounding.families();
            if rows.is_empty() && family.is_some() {
                return ok(json!({
                    "known": false,
                    "why": "no model in this build declares that family",
                    "declared": known_families
                }));
            }
            ok(json!({"known": true, "models": rows, "families": known_families}))
        }
        "registry__model_card" => match string_argument(arguments, "id") {
            Ok(id) => match grounding.card(id) {
                Ok(entry) => encode(entry),
                Err(miss) => encode(&miss),
            },
            Err(message) => ToolOutcome::Failed { message },
        },
        "registry__parameter" => {
            let model = match string_argument(arguments, "model") {
                Ok(v) => v,
                Err(message) => return ToolOutcome::Failed { message },
            };
            let name = match string_argument(arguments, "name") {
                Ok(v) => v,
                Err(message) => return ToolOutcome::Failed { message },
            };
            match grounding.parameter(model, name) {
                Ok(answer) => encode(&answer),
                Err(miss) => encode(&miss),
            }
        }
        "registry__list_metrics" => {
            let rows: Vec<Value> = grounding
                .metric_names()
                .into_iter()
                .filter_map(|n| grounding.metric(n).ok())
                .map(|m| {
                    json!({
                        "name": m.definition.name,
                        "unit": m.definition.unit,
                        "agg": m.agg,
                        "visibility": m.visibility,
                        "dims": m.definition.dims,
                        "diagnostic": m.definition.diagnostic,
                    })
                })
                .collect();
            ok(json!({"known": true, "metrics": rows}))
        }
        "registry__metric" => match string_argument(arguments, "name") {
            Ok(name) => match grounding.metric(name) {
                Ok(answer) => encode(&answer),
                Err(miss) => encode(&miss),
            },
            Err(message) => ToolOutcome::Failed { message },
        },
        "scenario__check_draft" => {
            if let Some(text) = arguments.get("text").and_then(Value::as_str) {
                encode(&check_draft(text))
            } else if let Some(document) = arguments.get("document") {
                encode(&check_document(document.clone()))
            } else {
                ToolOutcome::Failed {
                    message: "pass the draft as `text` (YAML or JSON) or as `document` (a \
                              JSON object)"
                        .to_string(),
                }
            }
        }
        "explain__value" => match arguments.get("subject") {
            Some(subject) if subject.is_object() => {
                let server = arguments.get("server_result").filter(|v| v.is_object());
                encode(&explain(grounding, subject, server))
            }
            _ => ToolOutcome::Failed {
                message: "`subject` must be a value reference object, e.g. \
                          {\"kind\":\"metric\",\"id\":\"pdr\"}"
                    .to_string(),
            },
        },
        other => ToolOutcome::Refused {
            reason: format!("{other:?} has no implementation in this copilot"),
        },
    }
}

/// A successful outcome.
fn ok(result: Value) -> ToolOutcome {
    ToolOutcome::Ok { result }
}

/// Serialises a grounded answer into an outcome.
fn encode<S: Serialize>(value: &S) -> ToolOutcome {
    match serde_json::to_value(value) {
        Ok(result) => ToolOutcome::Ok { result },
        Err(e) => ToolOutcome::Failed {
            message: format!("the answer would not serialise: {e}"),
        },
    }
}

/// One required string argument.
fn string_argument<'a>(arguments: &'a Value, name: &str) -> core::result::Result<&'a str, String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("`{name}` is required and must be a non-empty string"))
}

/// The arguments a model produced, as an object.
fn parse_arguments(text: &str) -> core::result::Result<Value, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(other) => Err(format!(
            "the arguments are {} rather than a JSON object",
            kind_of(&other)
        )),
        Err(e) => Err(format!("the arguments are not JSON: {e}")),
    }
}

/// A JSON value's kind, for an error message.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The text of a tool result, as it goes back into the conversation.
fn payload(outcome: &ToolOutcome) -> String {
    serde_json::to_string(outcome).unwrap_or_else(|_| {
        "{\"status\":\"failed\",\"message\":\"the result would not serialise\"}".to_string()
    })
}

/// The instructions.
///
/// Built from the catalogue rather than written around it: the model is told what exists
/// by name and is made to fetch every fact, because a card summarised into a prompt is a
/// card the reply can drift from.
#[must_use]
pub fn system_prompt(grounding: &Grounding, policy: &Policy, endpoint: &str) -> String {
    let mutation = if policy.allow_mutation {
        "You have tools marked CHANGES THE RUN. Call one only when the request asks for \
         that, one at a time, and say what it returned. Never start, seek or stop a run to \
         satisfy your own curiosity."
    } else {
        "You have no tool that changes the run: the tools that would are not in your list \
         at all. If a request needs one, name the method a person would have to call and \
         stop there."
    };
    format!(
        "You are the copilot of a V2X world simulator. You know nothing about this \
         simulator from training, and you must not pretend otherwise: every statement you \
         make about a model, a parameter, a metric or a run has to come from a tool result \
         in this conversation.\n\
         \n\
         Rules, most important first.\n\
         \n\
         1. NEVER state a parameter value, a default, a unit, a range or an equation that \
         did not come back from `registry__parameter`, `registry__model_card` or \
         `registry__metric` in this conversation. When a lookup answers `\"known\": false`, \
         say that the registry does not declare it and name what it does declare. A \
         confidently wrong parameter value is the worst thing you can produce here; \"I do \
         not know\" is a good answer and a plausible number is not.\n\
         2. CITE. Every value you quote carries the model id, the version and the source \
         it came from — for example `n = 2.75 (radio/propagation/log-distance 1.0.0, \
         paper doi:...)`. The card's content hash is in every lookup; offer it when the \
         reader may want to check an answer against a run manifest. A default whose source \
         kind is `todo-calibrate` is NOT an established value: say so whenever you quote \
         one.\n\
         3. NEVER call a scenario valid without running `scenario__check_draft` and seeing \
         `\"ok\": true`. Report every error it returns together with the field it names, \
         and fix the draft rather than explaining the error away.\n\
         4. To answer anything about a run, call the run's own methods. Do not compute, \
         estimate or extrapolate a result yourself. Values are quantised at the writer and \
         reductions are order-independent: do not re-round, re-average or re-scale what \
         you are given.\n\
         5. A node's own belief and the simulator's ground truth are different things, and \
         metrics tagged `GT` are ground truth. Never present a ground-truth value as \
         something a vehicle knew.\n\
         6. {mutation}\n\
         7. You never become part of a result. Nothing you say is recorded into a run, \
         hashed into a digest or exported. If a person needs a number in an artefact, they \
         run the simulator; you help them set that up and read what it produced.\n\
         \n\
         The run endpoint is {endpoint}.\n\
         \n\
         CATALOGUE — names only. Fetch the card or the definition before quoting anything \
         from one.\n\
         {}",
        grounding.index_text()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grounding::Grounding;
    use crate::provider::{Completion, Role, ScriptedProvider, ToolCall};
    use crate::tools::{ToolSpec, tool_name};
    use crate::transport::{NoEngine, ScriptedRpc};
    use v2xw_core::card::{Family, ModelCard, Parameter, Source, SourceKind};
    use v2xw_core::registry::Registry;

    fn grounding() -> Grounding {
        let mut card = ModelCard::new(
            "radio/propagation/log-distance",
            Family::Propagation,
            "1.0.0",
            "log-distance path loss",
        );
        card.parameters.push(Parameter::new(
            "path_loss_exponent",
            "-",
            json!(2.75),
            Source::new(SourceKind::Paper, "doi:10.1109/TVT.2011.2158461"),
        ));
        let mut registry = Registry::new();
        registry.register(card).expect("valid");
        Grounding::new(&registry, Vec::new())
    }

    fn surface() -> ToolSurface {
        let mut specs = crate::tools::local_tools();
        specs.push(ToolSpec {
            name: tool_name("run.status"),
            method: Some("run.status".to_string()),
            description: "status".to_string(),
            parameters: json!({"type": "object"}),
            effect: Effect::Read,
        });
        specs.push(ToolSpec {
            name: tool_name("run.stop"),
            method: Some("run.stop".to_string()),
            description: "stop".to_string(),
            parameters: json!({"type": "object"}),
            effect: Effect::Mutate,
        });
        ToolSurface::from_specs(specs)
    }

    fn call(name: &str, arguments: &str) -> Completion {
        Completion {
            message: ChatMessage::calls(
                None,
                vec![ToolCall {
                    id: "call_1".to_string(),
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                }],
            ),
            finish_reason: "tool_calls".to_string(),
        }
    }

    fn says(text: &str) -> Completion {
        Completion {
            message: ChatMessage::assistant(text),
            finish_reason: "stop".to_string(),
        }
    }

    #[test]
    fn a_declared_parameter_is_fetched_and_the_answer_is_returned() {
        let provider = ScriptedProvider::new(vec![
            call(
                "registry__parameter",
                r#"{"model":"radio/propagation/log-distance","name":"path_loss_exponent"}"#,
            ),
            says("n = 2.75 (radio/propagation/log-distance 1.0.0, paper doi:...)"),
        ]);
        let mut copilot = Copilot::new(
            provider,
            NoEngine,
            grounding(),
            surface(),
            Policy::read_only(),
        );
        let mut history = Vec::new();
        let turn = copilot
            .ask(&mut history, "what is the path loss exponent?")
            .expect("two scripted completions");
        assert_eq!(turn.rounds, 2);
        assert_eq!(turn.steps.len(), 1);
        match &turn.steps[0].outcome {
            ToolOutcome::Ok { result } => {
                assert_eq!(result["known"], json!(true));
                assert_eq!(result["default"], json!(2.75));
                assert_eq!(result["source_kind"], json!("paper"));
            }
            other => panic!("the lookup should have succeeded: {other:?}"),
        }
        // instructions are not in the history; the user turn, the tool call, the tool
        // result and the answer are.
        assert_eq!(history.len(), 4);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[2].role, Role::Tool);
    }

    #[test]
    fn an_undeclared_parameter_comes_back_as_not_known_rather_than_as_a_number() {
        let provider = ScriptedProvider::new(vec![
            call(
                "registry__parameter",
                r#"{"model":"radio/propagation/log-distance","name":"tx_power_dbm"}"#,
            ),
            says("the card does not declare that"),
        ]);
        let mut copilot = Copilot::new(
            provider,
            NoEngine,
            grounding(),
            surface(),
            Policy::read_only(),
        );
        let turn = copilot
            .ask(&mut Vec::new(), "what is the transmit power?")
            .expect("scripted");
        match &turn.steps[0].outcome {
            ToolOutcome::Ok { result } => {
                assert_eq!(result["known"], json!(false));
                assert!(
                    result["declared"]
                        .as_array()
                        .is_some_and(|a| a.contains(&json!("path_loss_exponent")))
                );
            }
            other => panic!("a miss is still a result: {other:?}"),
        }
    }

    #[test]
    fn a_mutating_tool_is_refused_by_the_dispatcher_even_when_it_was_never_offered() {
        let mut copilot = Copilot::new(
            ScriptedProvider::new(Vec::new()),
            NoEngine,
            grounding(),
            surface(),
            Policy::read_only(),
        );
        // It is not in the list the model is given...
        let offered = copilot.tools().to_chat_tools(false);
        let names: Vec<&str> = offered
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(!names.contains(&"run__stop"));
        // ...and naming it anyway is refused.
        match copilot.dispatch("run__stop", &json!({})) {
            ToolOutcome::Refused { reason } => assert!(reason.contains("read-only")),
            other => panic!("a read-only copilot must refuse it: {other:?}"),
        }
    }

    #[test]
    fn a_tool_that_is_not_in_the_surface_is_refused() {
        let mut copilot = Copilot::new(
            ScriptedProvider::new(Vec::new()),
            NoEngine,
            grounding(),
            surface(),
            Policy::allowing_mutation(),
        );
        match copilot.dispatch("engine__write_record", &json!({})) {
            ToolOutcome::Refused { reason } => assert!(reason.contains("is not a tool")),
            other => panic!("an invented tool must be refused: {other:?}"),
        }
    }

    #[test]
    fn a_failing_remote_call_is_a_step_not_a_turn_failure() {
        let provider = ScriptedProvider::new(vec![
            call("run__status", "{}"),
            says("there is no run attached"),
        ]);
        let mut copilot = Copilot::new(
            provider,
            ScriptedRpc::new(),
            grounding(),
            surface(),
            Policy::read_only(),
        );
        let turn = copilot
            .ask(&mut Vec::new(), "is it running?")
            .expect("scripted");
        assert!(matches!(turn.steps[0].outcome, ToolOutcome::Failed { .. }));
        assert_eq!(turn.reply, "there is no run attached");
    }

    #[test]
    fn arguments_that_are_not_json_fail_the_step_with_a_readable_message() {
        let provider = ScriptedProvider::new(vec![
            call("registry__model_card", "{not json"),
            says("I could not read my own arguments"),
        ]);
        let mut copilot = Copilot::new(
            provider,
            NoEngine,
            grounding(),
            surface(),
            Policy::read_only(),
        );
        let turn = copilot
            .ask(&mut Vec::new(), "show me a card")
            .expect("scripted");
        match &turn.steps[0].outcome {
            ToolOutcome::Failed { message } => assert!(message.contains("not JSON")),
            other => panic!("unparseable arguments must fail the step: {other:?}"),
        }
    }

    #[test]
    fn the_round_budget_stops_a_loop() {
        let provider = ScriptedProvider::new(vec![
            call("registry__list_metrics", "{}"),
            call("registry__list_metrics", "{}"),
            call("registry__list_metrics", "{}"),
        ]);
        let mut copilot = Copilot::new(
            provider,
            NoEngine,
            grounding(),
            surface(),
            Policy {
                max_rounds: 2,
                ..Policy::read_only()
            },
        );
        let err = copilot
            .ask(&mut Vec::new(), "loop forever")
            .expect_err("the budget stops it");
        assert!(err.to_string().contains("round"));
    }

    #[test]
    fn a_remote_result_over_the_limit_says_it_was_truncated() {
        let big = json!({"rows": vec![json!({"t": 1, "v": 0.5}); 2000]});
        let provider = ScriptedProvider::new(vec![call("run__status", "{}"), says("done")]);
        let mut copilot = Copilot::new(
            provider,
            ScriptedRpc::new().with("run.status", big),
            grounding(),
            surface(),
            Policy {
                max_result_bytes: 512,
                ..Policy::read_only()
            },
        );
        let turn = copilot
            .ask(&mut Vec::new(), "everything")
            .expect("scripted");
        match &turn.steps[0].outcome {
            ToolOutcome::Ok { result } => {
                assert_eq!(result["truncated"], json!(true));
                assert_eq!(result["limit"], json!(512));
            }
            other => panic!("a large result is bounded, not dropped: {other:?}"),
        }
    }

    #[test]
    fn the_instructions_name_the_catalogue_and_the_do_not_guess_rule() {
        let prompt = system_prompt(&grounding(), &Policy::read_only(), "<none>");
        assert!(prompt.contains("radio/propagation/log-distance"));
        assert!(prompt.contains("\"known\": false"));
        assert!(prompt.contains("no tool that changes the run"));
        let open = system_prompt(&grounding(), &Policy::allowing_mutation(), "<none>");
        assert!(open.contains("CHANGES THE RUN"));
    }
}
