//! The `NodeView` conformance sentinel, run over this crate's real source
//! (03-interfaces.md §17, invariants I-C2 and I-T1; build decision D11 §3).
//!
//! `src/firewall.rs` holds the rules and the scanner, and unit-tests the scanner against
//! synthetic input. This file points it at the crate itself, which is the part that can
//! actually go red during development.
//!
//! # Faults injected to prove these checks can fail
//!
//! This project has produced four separate checks that could not fail and therefore read
//! as evidence while proving nothing. Each assertion below was run against a deliberately
//! broken tree before being committed:
//!
//! 1. `let _leak: v2xw_core::kinematics::Kinematics = todo!();` added to
//!    `ObuRuntime::generate` — `no_ground_truth_reaches_the_node_runtime` fails, naming
//!    `src/runtime.rs` and the line.
//! 2. `fn world(&self) -> &World;` added to the `NodeCtx` trait in `src/ctx.rs` —
//!    `the_narrowed_context_cannot_reach_the_world` fails.
//! 3. `if self.gt_pos_error_m > 1.0 { return; }` added to `ObuRuntime::generate` —
//!    `ground_truth_reaches_only_the_telemetry_record` fails.
//! 4. `self.belief.pos = truth.pos;` — the behavioural half, covered by
//!    `ground_truth_changes_nothing_a_node_does` in `tests/runtime_behaviour.rs`.
//!
//! The results are recorded in the build notes for this crate.

use std::path::{Path, PathBuf};

use v2xw_node::firewall::{Violation, scan_context_trait, scan_ground_truth_fields, scan_source};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let dir = src_dir();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the crate has a src directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect();
    // Sorted so the failure message is the same on every machine.
    entries.sort();
    for path in entries {
        let name = format!("src/{}", path.file_name().unwrap().to_string_lossy());
        // `src/firewall.rs` is the rule table: it contains every forbidden string as a
        // literal, by construction. It is the one exclusion, it is checked separately by
        // `the_rule_table_is_only_a_rule_table`, and nothing else is exempt.
        if name == "src/firewall.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable");
        out.push((name, text));
    }
    out
}

fn report(v: &[Violation]) -> String {
    v.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The crate is non-empty and the scan actually opened files.
///
/// Without this, every assertion below would pass on an empty list — which is the exact
/// shape of a check that cannot fail.
#[test]
fn the_sentinel_is_actually_reading_the_crate() {
    let sources = rust_sources();
    assert!(
        sources.len() >= 10,
        "expected the whole crate, found {} files",
        sources.len()
    );
    let names: Vec<&str> = sources.iter().map(|(n, _)| n.as_str()).collect();
    for expected in [
        "src/ctx.rs",
        "src/generate.rs",
        "src/runtime.rs",
        "src/stores.rs",
    ] {
        assert!(names.contains(&expected), "{expected} was not scanned");
    }
    assert!(
        sources.iter().all(|(_, t)| !t.is_empty()),
        "a source file read as empty"
    );
}

/// **I-C2.** No file in the node runtime names the ground-truth kinematics type, calls
/// `world()` or `actors()`, or handles an `ActorId`.
#[test]
fn no_ground_truth_reaches_the_node_runtime() {
    let mut violations = Vec::new();
    for (name, text) in rust_sources() {
        violations.extend(scan_source(&name, &text));
    }
    assert!(
        violations.is_empty(),
        "the node runtime can reach ground truth:\n{}",
        report(&violations)
    );
}

/// **D12.2 + I-C2.** The narrowed context the runtime is driven through declares no
/// ground-truth accessor, so the hole the view closes is not re-opened above it.
#[test]
fn the_narrowed_context_cannot_reach_the_world() {
    let ctx = std::fs::read_to_string(src_dir().join("ctx.rs")).expect("src/ctx.rs");
    assert!(
        ctx.contains("pub trait NodeCtx"),
        "the sentinel is looking for a trait that has been renamed"
    );
    let violations = scan_context_trait(&ctx);
    assert!(
        violations.is_empty(),
        "NodeCtx has grown a ground-truth accessor:\n{}",
        report(&violations)
    );
}

/// The two §3.5.2 fields marked **GT** are handed in from outside the firewall. They may
/// be written, and they may reach the telemetry record. Nothing else.
#[test]
fn ground_truth_reaches_only_the_telemetry_record() {
    let mut violations = Vec::new();
    for (name, text) in rust_sources() {
        violations.extend(scan_ground_truth_fields(&name, &text));
    }
    assert!(
        violations.is_empty(),
        "a ground-truth value is being used to decide something:\n{}",
        report(&violations)
    );
}

/// The one file the scan skips holds rules and nothing else: it pulls in no engine type,
/// so its copies of the forbidden strings are text rather than reach-through.
#[test]
fn the_rule_table_is_only_a_rule_table() {
    let src = std::fs::read_to_string(src_dir().join("firewall.rs")).expect("src/firewall.rs");
    let code: Vec<&str> = src
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//") && !l.is_empty())
        .collect();
    for line in &code {
        assert!(
            !line.starts_with("use v2xw_core") && !line.starts_with("use v2xw_"),
            "the rule table has acquired an engine dependency: {line}"
        );
    }
    assert!(
        code.iter().any(|l| l.starts_with("pub const RULES")),
        "the rule table has moved"
    );
}

/// The runtime really does hold such a field, so the rule above has something to police.
///
/// A rule with no subject passes for the wrong reason, which is how a check stops being
/// evidence.
#[test]
fn the_ground_truth_field_the_rule_polices_exists() {
    let runtime = std::fs::read_to_string(src_dir().join("runtime.rs")).expect("src/runtime.rs");
    assert!(
        runtime.contains("gt_pos_error_m"),
        "the GT field has been renamed; the scan rule needs the same rename"
    );
    assert!(
        runtime.contains("pub fn observe_truth"),
        "the single documented door for ground truth has moved"
    );
}
