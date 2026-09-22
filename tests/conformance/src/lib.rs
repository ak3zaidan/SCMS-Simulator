//! The V2X World Simulator conformance kit.
//!
//! 03-interfaces.md §17 asks for "a `conformance/<family>` suite that any plug-in must pass
//! to be listed in the registry", and vwp-v1.md §10 carries a 65-item checklist whose ids
//! "match `tests/conformance/vwp/`". This crate is both, plus the two harnesses that make
//! the claims checkable rather than merely stated:
//!
//! | Part | Module | Driven by |
//! |---|---|---|
//! | the 65-item wire checklist, one test per item | [`checklist`] | `tests/vwp/` |
//! | the plug-in suite a contributor runs against their own model | [`plugin`] | `tests/kit/plugin_suite.rs` |
//! | the golden determinism harness, shared with CI | [`golden`] | `tests/kit/golden_suite.rs` and `src/bin/golden_digest.rs` |
//! | the interface firewall, generalised from `v2xw-node`'s sentinel | [`firewall`] | `tests/kit/firewall_suite.rs` |
//!
//! # Why this is a library and not only a directory of tests
//!
//! Two of the four parts have users outside this repository. A contributor writing a
//! propagation model cannot run a test that lives in our `tests/` directory against *their*
//! crate; they can depend on this library and call [`plugin::run_suite`]. CI cannot run a
//! `#[test]` on three platforms and compare the three results, because each job only
//! checks its own assertions — the comparison needs a value printed by a program, which is
//! what [`golden::RunDigest`] and the `v2xw-golden-digest` binary are for. Both of those
//! are the same code the tests here drive, so there is no parallel implementation to drift.
//!
//! # Everything here is honest about what it cannot check
//!
//! Several checklist items are properties of a live WebSocket connection, of a browser, or
//! of a benchmark, and no in-process test can establish them. Rather than write a check
//! that passes because it looks at nothing — this project has produced four of those, and
//! `docs/design/findings/slice-verification.md` records the cost — the kit records each such
//! item in [`checklist::COVERAGE`] as delegated to a named test elsewhere, and
//! `tests/vwp/coverage.rs` fails if that test has been renamed or deleted. An item with no
//! owner at all is a failure, not a silence.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod checklist;
pub mod firewall;
pub mod golden;
pub mod plugin;

use std::path::{Path, PathBuf};

/// The repository root, derived from this crate's manifest directory.
///
/// `CARGO_MANIFEST_DIR` is `<repo>/tests/conformance`, so the root is two levels up. It is
/// computed rather than searched for, because a search that walks upward looking for a
/// `.git` directory answers differently inside a worktree, inside a vendored copy and
/// inside CI's checkout, and a kit whose fixtures move with the caller's working directory
/// is a kit that cannot be trusted.
///
/// # Panics
/// Never: the two ancestors exist by construction of the workspace layout, and the
/// fallback is the manifest directory itself.
#[must_use]
pub fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

/// The directory a crate's sources live in: `<repo>/crates/<crate>/src`.
#[must_use]
pub fn crate_src(crate_name: &str) -> PathBuf {
    repo_root().join("crates").join(crate_name).join("src")
}

/// The `v2xw-*` crates in the workspace, in sorted order, read from `crates/`.
///
/// Read from the directory rather than listed here on purpose. A hard-coded list is one
/// more place to forget when a crate is added, and the thing it would protect against — a
/// crate silently escaping the scans — is better caught by [`MIN_CRATES`], which fails when
/// the read returns implausibly few. A list that had to be maintained by hand would be
/// wrong the first time two people added a crate in the same week.
///
/// # Panics
/// If `crates/` cannot be read, which means the kit is running outside the repository.
#[must_use]
pub fn crates() -> Vec<String> {
    let dir = repo_root().join("crates");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if !path.is_dir() || !path.join("Cargo.toml").is_file() {
                return None;
            }
            Some(path.file_name()?.to_string_lossy().into_owned())
        })
        .filter(|name| name.starts_with("v2xw-"))
        .collect();
    out.sort();
    out
}

/// The fewest crates a healthy workspace has.
///
/// ADR 0010's table names seventeen. The floor is deliberately below that so adding a crate
/// is not a test failure, and deliberately above zero so a scan that read nothing — the way
/// a firewall check stops being evidence — fails instead of passing.
pub const MIN_CRATES: usize = 15;
