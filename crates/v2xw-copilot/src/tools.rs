//! The tool surface: the server's own methods, turned into tool definitions.
//!
//! 09-ui.md §8 is explicit that "the copilot's tool registry is generated from them",
//! them being the JSON-RPC methods the UI already uses. So this module does not contain a
//! list of tools. It contains a *translation* of [`v2xw_server::openrpc::document`], which
//! is itself built from [`v2xw_server::rpc::METHODS`]. A method added to the server
//! appears here on the next build; a method spelled wrongly here cannot exist, because
//! nothing here spells one.
//!
//! Three things the translation has to do:
//!
//! 1. **Rename.** A tool name may hold letters, digits, `_` and `-`, and a JSON-RPC method
//!    name holds a dot. `run.start` becomes `run__start`, and [`method_name`] inverts it.
//!    No method contains `__`, so the mapping is one-to-one — [`tests`] pins that.
//! 2. **Close the schema.** The OpenRPC document refers to
//!    `#/components/schemas/…`, which means nothing once one method's schema is lifted out
//!    of the document. Every referenced schema is collected transitively and inlined under
//!    `$defs`, with the references rewritten. Only the schemas a method actually reaches
//!    are copied, because every byte here is a byte in a prompt.
//! 3. **Classify.** Every method is read-only, connection-scoped, or a change to the run
//!    ([`Effect`]). A read-only copilot is offered only the first two, so "do not let it
//!    touch the run" is a property of the tool list the model is given, not a rule it is
//!    asked to obey.
//!
//! The local tools of [`local_tools`] are the other half: the registry, the scenario
//! checker and the why-tab explainer, which are answered inside this process out of the
//! model cards. They are reads. None of them invents simulator capability — a local tool
//! either finds a card or says it did not.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{CopilotError, Result};

/// What a `$ref` in the OpenRPC document points at.
const COMPONENTS_PREFIX: &str = "#/components/schemas/";

/// The methods that change the run, the scenario, the world or a stored artefact.
///
/// Everything not listed here and not in [`VIEW_SCOPED`] is a read. The partition is
/// asserted complete against [`v2xw_server::rpc::METHODS`] in this module's tests, so a
/// method added to the server fails the test rather than being silently classified as a
/// read.
pub const MUTATING: [&str; 17] = [
    "run.start",
    "run.pause",
    "run.resume",
    "run.step",
    "run.seek",
    "run.speed",
    "run.stop",
    "scenario.set",
    "scenario.save",
    "scenario.load",
    "world.import_osm",
    "world.generate",
    "events.set",
    "export.dataset",
    "export.recording",
    "experiment.define",
    "experiment.run",
];

/// The methods that change only the calling connection's view, never the run.
///
/// This is [`v2xw_server::rpc::CONNECTION_SCOPED`], restated as an effect class.
pub const VIEW_SCOPED: [&str; 3] = ["view.follow", "view.camera", "overlay.set"];

/// The names of the tools this crate answers itself, out of the registry.
pub const LOCAL_TOOLS: [&str; 7] = [
    "registry__list_models",
    "registry__model_card",
    "registry__parameter",
    "registry__list_metrics",
    "registry__metric",
    "scenario__check_draft",
    "explain__value",
];

/// What calling a tool does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    /// Reads state. Safe to call at any time, including while a run is paused.
    Read,
    /// Changes what the calling connection is looking at, and nothing else.
    View,
    /// Changes the run, the scenario, the world, or writes a file.
    Mutate,
}

impl core::fmt::Display for Effect {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Effect::Read => "read",
            Effect::View => "view",
            Effect::Mutate => "mutate",
        })
    }
}

/// The effect class of a JSON-RPC method name.
#[must_use]
pub fn effect_of(method: &str) -> Effect {
    if MUTATING.contains(&method) {
        Effect::Mutate
    } else if VIEW_SCOPED.contains(&method) {
        Effect::View
    } else {
        Effect::Read
    }
}

/// The tool name for a JSON-RPC method name.
#[must_use]
pub fn tool_name(method: &str) -> String {
    method.replace('.', "__")
}

/// The JSON-RPC method name for a tool name; the inverse of [`tool_name`].
///
/// Meaningless for a local tool, which has no method: use [`ToolSpec::method`] instead of
/// inverting a name blind.
#[must_use]
pub fn method_name(tool: &str) -> String {
    tool.replace("__", ".")
}

/// One callable tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// The name the model calls, e.g. `run__status`.
    pub name: String,
    /// The JSON-RPC method this forwards to, or `None` for a tool answered in process.
    pub method: Option<String>,
    /// One line for the model.
    pub description: String,
    /// A self-contained JSON Schema for the arguments, with every `$ref` resolved into
    /// `$defs`.
    pub parameters: Value,
    /// What calling it does.
    pub effect: Effect,
}

/// Every tool this copilot has, indexed by name.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSurface {
    tools: Vec<ToolSpec>,
    by_name: BTreeMap<String, usize>,
}

impl ToolSurface {
    /// The full surface: every server method plus the local registry tools.
    ///
    /// # Errors
    /// [`CopilotError::BadOpenRpc`] if the document this build's server produces is not
    /// shaped as this module expects, which would mean the two crates have diverged.
    pub fn new() -> Result<Self> {
        let document = v2xw_server::openrpc::document(None);
        let mut specs = Self::specs_from_openrpc(&document)?;
        specs.extend(local_tools());
        Ok(Self::from_specs(specs))
    }

    /// The surface for a document a caller already has — a `rpc.discover` reply from a
    /// server that may be a different build from this one.
    ///
    /// # Errors
    /// As [`ToolSurface::new`].
    pub fn from_openrpc(document: &Value) -> Result<Self> {
        let mut specs = Self::specs_from_openrpc(document)?;
        specs.extend(local_tools());
        Ok(Self::from_specs(specs))
    }

    /// Indexes a list of specs. A duplicate name keeps the first.
    #[must_use]
    pub fn from_specs(tools: Vec<ToolSpec>) -> Self {
        let mut by_name = BTreeMap::new();
        for (i, t) in tools.iter().enumerate() {
            by_name.entry(t.name.clone()).or_insert(i);
        }
        ToolSurface { tools, by_name }
    }

    fn specs_from_openrpc(document: &Value) -> Result<Vec<ToolSpec>> {
        let methods = document
            .get("methods")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CopilotError::BadOpenRpc("no `methods` array in the document".to_string())
            })?;
        let empty = Map::new();
        let components = document
            .get("components")
            .and_then(|c| c.get("schemas"))
            .and_then(Value::as_object)
            .unwrap_or(&empty);

        let mut out = Vec::with_capacity(methods.len());
        for m in methods {
            let method = m
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| CopilotError::BadOpenRpc("a method has no `name`".to_string()))?;
            let summary = m
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let schema = m
                .get("params")
                .and_then(Value::as_array)
                .and_then(|p| p.first())
                .and_then(|p| p.get("schema"))
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            let effect = effect_of(method);
            out.push(ToolSpec {
                name: tool_name(method),
                method: Some(method.to_string()),
                description: describe(&summary, effect),
                parameters: close_schema(schema, components),
                effect,
            });
        }
        Ok(out)
    }

    /// The tool with this name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.by_name.get(name).and_then(|i| self.tools.get(*i))
    }

    /// Every tool, in the order the document listed them, local tools last.
    pub fn iter(&self) -> impl Iterator<Item = &ToolSpec> {
        self.tools.iter()
    }

    /// How many tools there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The tool list in the shape a chat completion wants.
    ///
    /// With `allow_mutation` false, a tool that changes the run is **not in the list at
    /// all**. The model is not asked to refrain; it is not told the capability exists.
    #[must_use]
    pub fn to_chat_tools(&self, allow_mutation: bool) -> Vec<Value> {
        self.tools
            .iter()
            .filter(|t| allow_mutation || t.effect != Effect::Mutate)
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect()
    }
}

/// The one-line description a tool carries into the prompt.
fn describe(summary: &str, effect: Effect) -> String {
    match effect {
        Effect::Read => summary.to_string(),
        Effect::View => format!("{summary} Changes only what this connection is looking at."),
        Effect::Mutate => {
            format!("{summary} CHANGES THE RUN: only call it when the request asks for that.")
        }
    }
}

/// Copies every schema a document-level `$ref` reaches into the schema's own `$defs` and
/// rewrites the references, so the result stands alone.
fn close_schema(mut schema: Value, components: &Map<String, Value>) -> Value {
    let mut needed: BTreeSet<String> = BTreeSet::new();
    collect_refs(&schema, &mut needed);
    let mut frontier: Vec<String> = needed.iter().cloned().collect();
    while let Some(name) = frontier.pop() {
        let Some(def) = components.get(&name) else {
            continue;
        };
        let mut more = BTreeSet::new();
        collect_refs(def, &mut more);
        for m in more {
            if needed.insert(m.clone()) {
                frontier.push(m);
            }
        }
    }

    let mut defs = Map::new();
    for name in &needed {
        if let Some(def) = components.get(name) {
            let mut d = def.clone();
            rewrite_refs(&mut d);
            defs.insert(name.clone(), d);
        }
    }

    rewrite_refs(&mut schema);
    if !defs.is_empty() {
        if let Some(obj) = schema.as_object_mut() {
            obj.insert("$defs".to_string(), Value::Object(defs));
        }
    }
    schema
}

/// Every component schema name this value refers to, directly.
fn collect_refs(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get("$ref") {
                if let Some(rest) = r.strip_prefix(COMPONENTS_PREFIX) {
                    out.insert(rest.to_string());
                }
            }
            for (_, child) in map {
                collect_refs(child, out);
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

/// Rewrites `#/components/schemas/X` into `#/$defs/X`, everywhere.
fn rewrite_refs(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get_mut("$ref") {
                if let Some(rest) = r.strip_prefix(COMPONENTS_PREFIX) {
                    let rewritten = format!("#/$defs/{rest}");
                    *r = rewritten;
                }
            }
            for (_, child) in map.iter_mut() {
                rewrite_refs(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_refs(item);
            }
        }
        _ => {}
    }
}

/// The tools answered in this process out of the model cards and the metric catalogue.
///
/// Every one of them is a read, and every one of them can answer "not known": that is the
/// point. A parameter this crate cannot find in a card is reported as absent, with the
/// names that *are* declared, rather than filled in from anywhere else.
#[must_use]
pub fn local_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "registry__list_models".to_string(),
            method: None,
            description: "List the registered models: id, version, family, tiers and card hash. \
                 Optionally narrowed to one family."
                .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "family": {"type": "string",
                               "description": "a model-card family, e.g. propagation, mac, detector, metric"}
                }
            }),
            effect: Effect::Read,
        },
        ToolSpec {
            name: "registry__model_card".to_string(),
            method: None,
            description:
                "The full model card for one model id: purpose, equations, every parameter \
                 with its unit, default and source, assumptions, limitations, what it \
                 ignores, and its validation status."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["id"],
                "properties": {"id": {"type": "string"}}
            }),
            effect: Effect::Read,
        },
        ToolSpec {
            name: "registry__parameter".to_string(),
            method: None,
            description: "One parameter of one model: its unit, default, allowed range, and the \
                 source the default is cited to. Returns `known: false` when the card does \
                 not declare it. Never state a parameter value that did not come from here."
                .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["model", "name"],
                "properties": {
                    "model": {"type": "string"},
                    "name": {"type": "string"}
                }
            }),
            effect: Effect::Read,
        },
        ToolSpec {
            name: "registry__list_metrics".to_string(),
            method: None,
            description: "List every metric: name, unit, aggregation, dimensions and visibility."
                .to_string(),
            parameters: json!({"type": "object", "additionalProperties": false, "properties": {}}),
            effect: Effect::Read,
        },
        ToolSpec {
            name: "registry__metric".to_string(),
            method: None,
            description: "One metric's definition: the formula, its unit, the grid its values are \
                 quantised onto, the sample count below which it reports insufficient, what \
                 it does not account for, and its citation."
                .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["name"],
                "properties": {"name": {"type": "string"}}
            }),
            effect: Effect::Read,
        },
        ToolSpec {
            name: "scenario__check_draft".to_string(),
            method: None,
            description: "Run a scenario draft through the engine's own loader and validator and \
                 return every error with the field it is about. Writes nothing. Use this \
                 before claiming a scenario is valid; never claim it without running it."
                .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "text": {"type": "string", "description": "the draft as YAML or JSON"},
                    "document": {"type": "object", "description": "the draft as a JSON object"}
                }
            }),
            effect: Effect::Read,
        },
        ToolSpec {
            name: "explain__value".to_string(),
            method: None,
            description: "Where a value in the interface came from: the metric definition or the \
                 provenance chain, resolved into model cards with their equations, \
                 parameters and sources. Pass the `explain` method's result as \
                 `server_result` when there is a run, so the chain is the run's own."
                .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["subject"],
                "properties": {
                    "subject": {"type": "object",
                                "description": "a ValueRef, e.g. {\"kind\":\"metric\",\"id\":\"pdr\"}"},
                    "server_result": {"type": "object",
                                      "description": "the result of the `explain` JSON-RPC method, if one was fetched"}
                }
            }),
            effect: Effect::Read,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_server::rpc::{CONNECTION_SCOPED, METHODS};

    #[test]
    fn the_effect_partition_covers_every_method() {
        let mut read = 0;
        for m in METHODS {
            match effect_of(m) {
                Effect::Read => read += 1,
                Effect::View | Effect::Mutate => {}
            }
        }
        assert_eq!(read + MUTATING.len() + VIEW_SCOPED.len(), METHODS.len());
        for m in MUTATING {
            assert!(
                METHODS.contains(&m),
                "{m} is classified but is not a method"
            );
        }
        for m in VIEW_SCOPED {
            assert!(
                METHODS.contains(&m),
                "{m} is classified but is not a method"
            );
        }
    }

    #[test]
    fn view_scoped_matches_the_server() {
        let mut mine = VIEW_SCOPED.to_vec();
        let mut theirs = CONNECTION_SCOPED.to_vec();
        mine.sort_unstable();
        theirs.sort_unstable();
        assert_eq!(mine, theirs);
    }

    #[test]
    fn the_name_mapping_round_trips() {
        for m in METHODS {
            assert_eq!(method_name(&tool_name(m)), m);
            assert!(
                tool_name(m)
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
                "{m} does not map to a legal tool name"
            );
        }
    }

    #[test]
    fn every_method_becomes_a_tool_with_a_closed_schema() {
        let surface = ToolSurface::new().expect("the server's own document must translate");
        assert_eq!(surface.len(), METHODS.len() + LOCAL_TOOLS.len());
        for m in METHODS {
            let spec = surface
                .get(&tool_name(m))
                .unwrap_or_else(|| panic!("{m} is missing from the surface"));
            assert_eq!(spec.method.as_deref(), Some(m));
            let text = serde_json::to_string(&spec.parameters).expect("schema serialises");
            assert!(
                !text.contains(COMPONENTS_PREFIX),
                "{m} still points outside its own schema"
            );
        }
        for l in LOCAL_TOOLS {
            let spec = surface.get(l).unwrap_or_else(|| panic!("{l} is missing"));
            assert_eq!(spec.method, None);
            assert_eq!(spec.effect, Effect::Read);
        }
    }

    #[test]
    fn a_read_only_surface_omits_every_mutating_tool() {
        let surface = ToolSurface::new().expect("document");
        let listed = surface.to_chat_tools(false);
        let names: BTreeSet<String> = listed
            .iter()
            .filter_map(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        for m in MUTATING {
            assert!(
                !names.contains(&tool_name(m)),
                "{m} is offered to a read-only copilot"
            );
        }
        assert!(names.contains("run__status"));
        assert_eq!(listed.len(), surface.len() - MUTATING.len());
    }

    #[test]
    fn a_ref_is_inlined_transitively() {
        let components = json!({
            "A": {"type": "object", "properties": {"b": {"$ref": "#/components/schemas/B"}}},
            "B": {"type": "integer"},
            "C": {"type": "string"}
        });
        let schema = json!({"type": "object",
                            "properties": {"a": {"$ref": "#/components/schemas/A"}}});
        let closed = close_schema(schema, components.as_object().expect("object"));
        let defs = closed
            .get("$defs")
            .and_then(Value::as_object)
            .expect("defs");
        assert!(defs.contains_key("A"));
        assert!(defs.contains_key("B"), "a transitive reference was dropped");
        assert!(!defs.contains_key("C"), "an unreferenced schema was copied");
        assert_eq!(
            closed["properties"]["a"]["$ref"].as_str(),
            Some("#/$defs/A")
        );
        assert_eq!(
            defs["A"]["properties"]["b"]["$ref"].as_str(),
            Some("#/$defs/B")
        );
    }
}
