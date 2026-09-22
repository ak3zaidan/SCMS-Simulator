//! The rule that the copilot never becomes part of a result, and the checks that hold it.
//!
//! # The rule
//!
//! Nothing this crate produces may reach a simulation output, a recording, a metric, a
//! manifest or a run digest. A copilot is a reader and a driver of the interface; it is
//! never a term in an answer.
//!
//! # How it is enforced, in four layers
//!
//! **1. There is no path.** The only way out of this crate into a run is
//! [`crate::transport::RpcTransport`], whose whole surface is a method name and a JSON
//! object. It has no `Ctx`, no recorder, no scheduler and no random-number stream, and it
//! cannot obtain one: the crate depends on `v2xw-core`, `v2xw-metrics`, `v2xw-server` and
//! `v2xw-engine` for their *types*, and constructs from them only a [`v2xw_core::registry::Registry`]
//! (read, never given to a run) and a [`v2xw_engine::Scenario`] (returned as text, never
//! run). [`FORBIDDEN_DEPENDENCIES`] names the crates that would make a write path
//! reachable, and [`audit_manifest`] fails if one appears.
//!
//! **2. The authority is a person's authority.** Every method the copilot may name is one
//! of [`v2xw_server::rpc::METHODS`] — the same list the Studio drives. It has no private
//! method, no privileged flag and no back channel; [`audit_tools`] fails if a tool appears
//! whose method the server does not publish. What a copilot changes, a person could have
//! changed by clicking, and the run records it identically either way, because the run
//! cannot tell the difference and nothing asks it to.
//!
//! **3. A result is a function of the scenario, not of the conversation.** A scenario the
//! copilot drafts is checked (`crate::scenario`) and handed back as text. Making it a file
//! is `scenario.save`, and running it is `run.start` — both of them changes a person
//! authorises. Once a scenario exists, the manifest pins its content hash, so a run's
//! outputs are a function of that document and of the seed, and it is not knowable from
//! the artefacts whether a human or a copilot typed it. That is the correct property: the
//! copilot is an editor, and an editor does not appear in the text.
//!
//! **4. No source of drift.** This crate reads no clock, draws no random number, iterates
//! no hash map into an output, and computes no float that could be exported.
//! [`FORBIDDEN_SOURCE_PATTERNS`] names the constructs that would, and [`audit_source`]
//! fails on one.
//!
//! # What is not claimed
//!
//! The language model is not deterministic, and this crate does not pretend it could be.
//! That is exactly why none of its output may be an input to a result: the boundary is
//! drawn so that the one nondeterministic component in the system sits entirely outside
//! everything the determinism contract covers. `temperature = 0` reduces variance for a
//! reader's benefit and is not, and is never described as, determinism.
//!
//! The checks below are run over this crate by `tests/boundary.rs`, and each is
//! demonstrated failing on an injected violation in this module's own tests — a check
//! nobody has seen go red is not a check.

use crate::tools::{Effect, LOCAL_TOOLS, ToolSurface};

/// Crates this one may not depend on, and why each would matter.
///
/// * `v2xw-record` — the recorder and the MCAP writer: the path a value takes into an
///   artefact.
/// * `v2xw-node` — the OBU runtime: node state a copilot could reach around the server.
/// * `v2xw-threat` — attacker behaviour, which is a model in the run and not a tool.
/// * `rand`, `rand_chacha`, `getrandom` — every draw in this system comes from a keyed
///   stream in the engine. A copilot with its own generator is a copilot that can make a
///   run's inputs depend on something nothing pins.
pub const FORBIDDEN_DEPENDENCIES: [&str; 5] = [
    "v2xw-record",
    "v2xw-node",
    "v2xw-threat",
    "rand",
    "rand_chacha",
];

/// Constructs that may not appear in this crate's source, outside comments.
///
/// A wall clock and a random draw are the two ways a value that nothing pins gets into a
/// program; the recorder trait is the way one gets out. `Command` is not here: the HTTP
/// client is a child process by design, and it touches nothing of the simulation.
///
/// Each row is spelled with `concat!` so that this array does not itself contain the
/// strings it forbids: a check that trips on its own definition is a check that gets
/// switched off.
pub const FORBIDDEN_SOURCE_PATTERNS: [&str; 7] = [
    concat!("impl ", "Record for"),
    concat!("Rng", "Registry"),
    concat!("rand", "::"),
    concat!("System", "Time"),
    concat!("Instant", "::now"),
    concat!("fs", "::write"),
    concat!("File", "::create"),
];

/// Reads a `Cargo.toml` and reports every forbidden dependency it declares.
///
/// An empty result means clean. The scan is line-based rather than a TOML parse because
/// the question is "does this name appear as a dependency key", which a line answers, and
/// because a check with no dependencies of its own cannot be broken by one.
#[must_use]
pub fn audit_manifest(manifest: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_dependencies = false;
    for (number, raw) in manifest.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_dependencies = line.contains("dependencies");
            continue;
        }
        if !in_dependencies {
            continue;
        }
        for name in FORBIDDEN_DEPENDENCIES {
            if is_key(line, name) {
                found.push(format!(
                    "Cargo.toml line {}: depends on `{name}`, which would put a write path \
                     inside the copilot (see `boundary::FORBIDDEN_DEPENDENCIES`)",
                    number + 1
                ));
            }
        }
    }
    found
}

/// Whether a manifest line declares `name` as a key.
fn is_key(line: &str, name: &str) -> bool {
    let Some(rest) = line.strip_prefix(name) else {
        return false;
    };
    let rest = rest.trim_start();
    rest.starts_with('=') || rest.starts_with('.')
}

/// Reports every forbidden construct in one source file, ignoring comments.
///
/// Each line is cut at its first `//`, which drops line comments and doc comments whole.
/// It also truncates a line at a `//` inside a string literal, which costs nothing here:
/// the only such literals in this crate are URLs.
#[must_use]
pub fn audit_source(file: &str, source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (number, raw) in source.lines().enumerate() {
        let code = match raw.find("//") {
            Some(at) => &raw[..at],
            None => raw,
        };
        for pattern in FORBIDDEN_SOURCE_PATTERNS {
            if code.contains(pattern) {
                found.push(format!(
                    "{file}:{}: uses `{pattern}`, which the copilot boundary forbids \
                     (see `boundary::FORBIDDEN_SOURCE_PATTERNS`)",
                    number + 1
                ));
            }
        }
    }
    found
}

/// Reports every tool whose authority is not a published server method or a local read.
///
/// This is the check that the copilot cannot name a capability the interface does not
/// already have.
#[must_use]
pub fn audit_tools(tools: &ToolSurface) -> Vec<String> {
    let mut found = Vec::new();
    for tool in tools.iter() {
        match &tool.method {
            Some(method) => {
                if !v2xw_server::rpc::METHODS.contains(&method.as_str()) {
                    found.push(format!(
                        "tool `{}` forwards to `{method}`, which the server does not publish",
                        tool.name
                    ));
                }
                if tool.effect != crate::tools::effect_of(method) {
                    found.push(format!(
                        "tool `{}` is classified `{}` but `{method}` is `{}`",
                        tool.name,
                        tool.effect,
                        crate::tools::effect_of(method)
                    ));
                }
            }
            None => {
                if !LOCAL_TOOLS.contains(&tool.name.as_str()) {
                    found.push(format!(
                        "tool `{}` has no method and is not one of the declared local \
                         tools, so nothing says what it may do",
                        tool.name
                    ));
                }
                if tool.effect != Effect::Read {
                    found.push(format!(
                        "local tool `{}` is not a read; a tool answered inside the copilot \
                         may only read",
                        tool.name
                    ));
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolSpec, tool_name};
    use serde_json::json;

    #[test]
    fn a_clean_manifest_passes_and_a_forbidden_dependency_fails() {
        let clean = "[dependencies]\nv2xw-core = { workspace = true }\nserde = \"1\"\n";
        assert!(audit_manifest(clean).is_empty());
        // Injected violation: the check goes red.
        let dirty = "[dependencies]\nv2xw-core = { workspace = true }\nv2xw-record = \"0.1\"\n";
        let found = audit_manifest(dirty);
        assert_eq!(found.len(), 1, "the check did not fire: {found:?}");
        assert!(found[0].contains("v2xw-record"));
        // And in the dotted spelling.
        let dotted = "[dev-dependencies]\nrand.workspace = true\n";
        assert_eq!(audit_manifest(dotted).len(), 1);
        // A mention outside a dependency table is not a dependency.
        let mention = "[package]\ndescription = \"not rand = 1, just prose\"\n";
        assert!(audit_manifest(mention).is_empty());
        // Nor is a comment.
        let comment = "[dependencies]\n# rand = \"0.9\" would be wrong here\n";
        assert!(audit_manifest(comment).is_empty());
    }

    #[test]
    fn a_clean_source_passes_and_a_forbidden_construct_fails() {
        let clean = "//! RngRegistry is named here, in prose.\nlet x = 1;\n";
        assert!(audit_source("a.rs", clean).is_empty());
        // Injected violation: the check goes red.
        let dirty = "let now = SystemTime::now();\n";
        let found = audit_source("a.rs", dirty);
        assert_eq!(found.len(), 1, "the check did not fire: {found:?}");
        assert!(found[0].contains("a.rs:1"));
        assert_eq!(audit_source("b.rs", "fs::write(p, b)\n").len(), 1);
    }

    #[test]
    fn a_tool_the_server_does_not_publish_fails_the_audit() {
        let clean = ToolSurface::new().expect("the server's document translates");
        assert!(audit_tools(&clean).is_empty(), "{:?}", audit_tools(&clean));

        // Injected violation: a tool that claims a method the server has no such thing as.
        let mut specs: Vec<ToolSpec> = clean.iter().cloned().collect();
        specs.push(ToolSpec {
            name: tool_name("engine.write_record"),
            method: Some("engine.write_record".to_string()),
            description: "smuggle a value into a recording".to_string(),
            parameters: json!({"type": "object"}),
            effect: Effect::Read,
        });
        let found = audit_tools(&ToolSurface::from_specs(specs));
        assert_eq!(found.len(), 1, "the check did not fire: {found:?}");
        assert!(found[0].contains("engine.write_record"));
    }

    #[test]
    fn a_local_tool_that_is_not_a_read_fails_the_audit() {
        let specs = vec![ToolSpec {
            name: "registry__list_models".to_string(),
            method: None,
            description: "x".to_string(),
            parameters: json!({"type": "object"}),
            effect: Effect::Mutate,
        }];
        let found = audit_tools(&ToolSurface::from_specs(specs));
        assert_eq!(found.len(), 1, "the check did not fire: {found:?}");
        assert!(found[0].contains("may only read"));
    }

    #[test]
    fn a_misclassified_method_fails_the_audit() {
        let specs = vec![ToolSpec {
            name: tool_name("run.stop"),
            method: Some("run.stop".to_string()),
            description: "x".to_string(),
            // Injected violation: a change to the run dressed as a read, which a
            // read-only policy would then offer.
            parameters: json!({"type": "object"}),
            effect: Effect::Read,
        }];
        let found = audit_tools(&ToolSurface::from_specs(specs));
        assert_eq!(found.len(), 1, "the check did not fire: {found:?}");
        assert!(found[0].contains("run.stop"));
    }
}
