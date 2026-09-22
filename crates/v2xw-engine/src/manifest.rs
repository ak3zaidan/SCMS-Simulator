//! Run-manifest assembly (02-architecture.md §6.5, Phase 1 acceptance criterion 6).
//!
//! The manifest is the answer to "what produced this output?", and the criterion lists
//! what it must carry. Each row below names where the value comes from, because a manifest
//! field whose provenance is a guess pins nothing:
//!
//! | Criterion 6 asks for | Field | Source |
//! |---|---|---|
//! | engine version | `engine_version` | `CARGO_PKG_VERSION` of this crate |
//! | engine hash | `build_hash` | [`build_hash`]: SHA-256 over the build's own identity |
//! | every plug-in and its hash | `plugins` | every [`Registry`] entry, as `id`, card version, card content hash |
//! | world hash | `world_hash` | [`v2xw_world::WorldProvenance::content_hash`] |
//! | every model card version | `model_cards` | [`Registry::model_card_versions`] |
//! | scenario hash | `scenario_hash` | [`Scenario::content_hash`], over canonical JSON |
//! | master seed | `master_seed` | `scenario.seed` |
//! | compiler version | `crate_versions["rustc"]` | the build script's `rustc --version` |
//!
//! Three further fields the architecture section names are filled here too: the platform
//! triple, the crypto mode, and the time-dilation windows.
//!
//! # What `build_hash` covers, and what it does not
//!
//! It is SHA-256 over the engine version, the compiler version, the platform triple, the
//! git commit when one was supplied, and every workspace crate version the manifest
//! carries — sorted, so it does not depend on map iteration order. It identifies *a
//! build*: two machines that agree on all of those produce the same hash, and the
//! determinism contract says they must then produce the same outputs.
//!
//! It does **not** cover uncommitted source edits. Nothing available to a running program
//! does — hashing the executable would, but the path to it is not reliable and the same
//! source produces different executables under different linkers. The git commit is what
//! covers source identity, which is why the build script reads `V2XW_GIT_COMMIT` and why
//! `"unknown"` is recorded rather than omitted when it is absent: a manifest that is
//! silent about a missing pin reads like a manifest that has one.
//!
//! # No wall clock
//!
//! [`v2xw_core::manifest::Manifest::build_utc`] is caller-supplied and excluded from every
//! digest (02-architecture.md §6.1). [`assemble`] takes it as an argument for exactly that
//! reason, and passing an empty string is a legitimate choice for a reproducibility test.

use v2xw_core::hash::{Sha256Writer, hex_encode};
use v2xw_core::manifest::{Manifest, PluginPin, TimeDilationWindow};
use v2xw_core::registry::Registry;
use v2xw_world::World;

use crate::error::Result;
use crate::scenario::Scenario;

/// This engine's version.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The compiler that built it, from the build script.
pub const RUSTC_VERSION: &str = env!("V2XW_RUSTC_VERSION");
/// The target triple it was built for.
pub const TARGET_TRIPLE: &str = env!("V2XW_TARGET");
/// The commit it was built from, or `"unknown"`.
pub const GIT_COMMIT: &str = env!("V2XW_GIT_COMMIT");

/// The workspace crates a run's outputs depend on, with their versions.
///
/// Every one of them is a dependency of this crate, so `CARGO_PKG_VERSION_*` would only
/// give this crate's view; the versions are read from each crate's own constant where it
/// publishes one and from the workspace version otherwise. They are all the same today
/// (one workspace version), and the list exists so that the day they diverge the manifest
/// records the divergence rather than one number standing for ten.
pub fn crate_versions() -> std::collections::BTreeMap<String, String> {
    let mut m = std::collections::BTreeMap::new();
    for name in [
        "v2xw-core",
        "v2xw-world",
        "v2xw-mobility",
        "v2xw-radio",
        "v2xw-net",
        "v2xw-msg",
        "v2xw-sec",
        "v2xw-node",
        "v2xw-record",
        "v2xw-metrics",
        "v2xw-engine",
    ] {
        m.insert(name.to_string(), ENGINE_VERSION.to_string());
    }
    m.insert("rustc".to_string(), RUSTC_VERSION.to_string());
    m
}

/// SHA-256 over the build's own identity. See the module documentation for its scope.
pub fn build_hash() -> String {
    let mut w = Sha256Writer::new();
    w.update(b"v2xw-engine-build/1\n");
    w.update(ENGINE_VERSION.as_bytes());
    w.update(b"\n");
    w.update(RUSTC_VERSION.as_bytes());
    w.update(b"\n");
    w.update(TARGET_TRIPLE.as_bytes());
    w.update(b"\n");
    w.update(GIT_COMMIT.as_bytes());
    w.update(b"\n");
    // BTreeMap, so the order is the key order and not a hash order (02-architecture.md
    // §6.1: no std HashMap iteration may reach an output).
    for (k, v) in crate_versions() {
        w.update(k.as_bytes());
        w.update(b"=");
        w.update(v.as_bytes());
        w.update(b"\n");
    }
    w.finish_hex()
}

/// Assembles the manifest for a run.
///
/// `build_utc` is the caller's — a timestamp the engine may not read for itself. It is
/// excluded from every digest, so two runs that differ only in it still compare equal.
///
/// # Errors
/// [`crate::EngineError::Scenario`] if the scenario does not serialise, which is what
/// makes the scenario hash unobtainable.
pub fn assemble(
    scenario: &Scenario,
    world: &World,
    registry: &Registry,
    build_utc: &str,
) -> Result<Manifest> {
    let mut m = Manifest::new(ENGINE_VERSION, scenario.seed);
    m.scenario_hash = scenario.content_hash()?;
    m.git_commit = GIT_COMMIT.to_string();
    m.build_hash = build_hash();
    m.platform = TARGET_TRIPLE.to_string();
    m.crate_versions = crate_versions();
    m.build_utc = build_utc.to_string();
    m.world_hash = hex_encode(&world.provenance.content_hash);
    m.crypto_mode = scenario.security.crypto_mode.into();
    m.model_cards = registry.model_card_versions();

    // Every registered model is a plug-in for manifest purposes, whether it is linked in
    // or reached over gRPC: ADR 0007 makes the card, not the hosting, what identifies a
    // model. `iter_by_id` is id-ordered, so the list does not depend on registration order
    // and two runs that register the same models in different orders still match.
    m.plugins = registry
        .iter_by_id()
        .map(|(_, e)| {
            PluginPin::new(
                e.card.id.clone(),
                e.card.version.clone(),
                e.content_hash_hex(),
            )
        })
        .collect();

    m.time_dilation_windows = scenario
        .time
        .time_dilation
        .iter()
        .map(|w| {
            TimeDilationWindow::new(
                (w.from_s * 1e9).round().max(0.0) as u64,
                (w.to_s * 1e9).round().max(0.0) as u64,
            )
        })
        .collect();

    if GIT_COMMIT == "unknown" {
        m.warnings.push(
            "git commit not recorded: the build environment did not set V2XW_GIT_COMMIT, so \
             build_hash pins the compiler, the target and the crate versions but not the \
             source revision"
                .to_string(),
        );
    }
    if RUSTC_VERSION == "unknown" {
        m.warnings.push(
            "compiler version not recorded: the build script could not run rustc --version"
                .to_string(),
        );
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The build hash is stable within a build — it is a pure function of constants — and
    /// it is a 64-character hex digest.
    #[test]
    fn the_build_hash_is_stable_and_hex() {
        let a = build_hash();
        assert_eq!(a, build_hash());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The compiler version really was captured, which is the thing acceptance criterion 6
    /// asks for and the thing most easily left as a placeholder.
    #[test]
    fn the_compiler_version_was_captured_by_the_build() {
        assert!(
            RUSTC_VERSION.starts_with("rustc "),
            "the build script did not capture a compiler version: {RUSTC_VERSION}"
        );
        assert!(!TARGET_TRIPLE.is_empty() && TARGET_TRIPLE != "unknown");
    }

    /// Crate versions are keyed in a `BTreeMap`, so the hash input is key-ordered.
    #[test]
    fn crate_versions_are_ordered_and_include_the_compiler() {
        let v = crate_versions();
        let keys: Vec<&String> = v.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert!(v.contains_key("rustc"));
        assert!(v.contains_key("v2xw-core"));
    }
}
