//! The copilot boundary, checked against the crate as it actually is.
//!
//! `src/boundary.rs` states the rule and holds the three audits; each of them is
//! demonstrated going red on an injected violation in that module's own unit tests. This
//! file is where they are pointed at the real manifest, the real source and the real tool
//! surface, so a future change that crosses the line fails here rather than being noticed
//! by nobody.

use std::path::{Path, PathBuf};

use v2xw_copilot::boundary::{audit_manifest, audit_source, audit_tools};
use v2xw_copilot::tools::ToolSurface;

/// This crate's directory.
fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_manifest_declares_no_forbidden_dependency() {
    let manifest =
        std::fs::read_to_string(crate_dir().join("Cargo.toml")).expect("the crate has a manifest");
    let found = audit_manifest(&manifest);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn no_source_file_reads_a_clock_draws_a_random_number_or_writes_a_record() {
    let src = crate_dir().join("src");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&src)
        .expect("the crate has sources")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    // Sorted so the first failure reported is the same one on every machine.
    files.sort();
    assert!(
        files.len() >= 10,
        "the source scan found almost nothing: {files:?}"
    );

    let mut found = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).expect("a source file reads");
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unnamed>");
        // `boundary.rs`'s own tests must spell the constructs they inject, so only the part
        // of that one file before its test module is scanned. Everywhere else is scanned
        // whole, tests included.
        let scanned = if name == "boundary.rs" {
            text.split("#[cfg(test)]")
                .next()
                .unwrap_or(&text)
                .to_string()
        } else {
            text
        };
        found.extend(audit_source(name, &scanned));
    }
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn every_tool_is_a_method_the_server_publishes_or_a_declared_local_read() {
    let surface = ToolSurface::new().expect("the server's document translates");
    let found = audit_tools(&surface);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn the_crate_directory_contains_no_environment_file() {
    // Belt and braces for "never committed": a key lives in the environment, and an
    // `.env` inside a crate is the way one ends up in a commit.
    for name in [".env", ".env.local", "openai.key"] {
        let path: &Path = &crate_dir().join(name);
        assert!(!path.exists(), "{} should not exist", path.display());
    }
}
