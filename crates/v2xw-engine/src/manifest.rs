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
//! git commit, the clean/dirty state of the tree that was compiled, and every workspace
//! crate version the manifest carries — sorted, so it does not depend on map iteration
//! order. It identifies *a build*: two machines that agree on all of those produce the
//! same hash, and the determinism contract says they must then produce the same outputs.
//!
//! The commit is now real. The Phase 1 build recorded `"unknown"` on every run because
//! the build script only read an environment variable that nothing set; it asks git as
//! well, and the vertical-slice audit's "the engine hash does not pin the engine's
//! source" is what that closes.
//!
//! It still does **not** cover the *content* of uncommitted source edits. Nothing
//! available to a running program does — hashing the executable would, but the path to it
//! is not reliable and the same source produces different executables under different
//! linkers. What it does cover is the *fact* of them: [`GIT_DIRTY`] is part of the hash
//! and a dirty tree is a manifest warning, so a run from edited source cannot be mistaken
//! for a run from the commit it sits on. Where the commit genuinely cannot be known — a
//! vendored crate with no repository — `"unknown"` is recorded and warned about rather
//! than omitted: a manifest that is silent about a missing pin reads like a manifest that
//! has one.
//!
//! # Comparing two manifests
//!
//! Two runs of one scenario produce manifests that are *not* byte-identical, and the
//! audit found this was nowhere stated. [`VARYING_FIELDS`] names the fields that differ,
//! [`FILES_EMBEDDING_THE_MANIFEST`] names the output that cannot have a stable digest,
//! [`comparison_digest`] is the number to compare instead, and [`assemble`] puts all of
//! that in the manifest's own `warnings` so a reader finds it without reading this file.
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
/// Whether the working tree the build compiled matched [`GIT_COMMIT`]: `"clean"`,
/// `"dirty"` or `"unknown"`.
///
/// A commit hash on its own is a claim the build cannot support — a dirty tree names
/// source that is not what ran — so the two travel together and
/// [`assemble`] turns anything but `"clean"` into a manifest warning.
pub const GIT_DIRTY: &str = env!("V2XW_GIT_DIRTY");

/// The manifest fields that differ between two otherwise identical runs, and which
/// [`comparison_digest`] therefore excludes.
///
/// * `build_utc` is the caller's timestamp and the only wall-clock value in the engine.
/// * `data_digest` covers [`v2xw_core::manifest::Manifest::files`], and one of those
///   files is the MCAP recording, which **embeds this manifest** — including `build_utc`.
///   So the recording's bytes differ, its file digest differs, and the aggregate over the
///   files differs, all from the one timestamp.
///
/// This is the same exclusion the run report already makes for its timing fields, and for
/// the same reason: a digested artefact that carries a fact about *this invocation* is an
/// artefact that never matches itself, which puts a spurious mismatch in front of the
/// reader and makes a real one unnoticeable.
pub const VARYING_FIELDS: [&str; 2] = ["build_utc", "data_digest"];

/// Output files that embed the manifest and therefore cannot have a stable digest.
///
/// The MCAP recording carries the manifest as its `v2xw.manifest` metadata record
/// (vwp-v1 §7.1), so its bytes contain `build_utc` and its SHA-256 moves with it. A
/// caller comparing two runs' manifests must drop these entries from
/// [`v2xw_core::manifest::Manifest::files`] as well as the two fields above, which is
/// what [`comparison_copy`] does.
pub const FILES_EMBEDDING_THE_MANIFEST: [&str; 1] = ["recording.mcap"];

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
    // A dirty tree is a different build from the commit it sits on, and the hash says so
    // rather than claiming two builds are the same because their commits are.
    w.update(GIT_DIRTY.as_bytes());
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

    // Byte-for-byte comparability, said in the artefact itself rather than left for a
    // reader to discover by diffing two manifests and finding they never match.
    m.warnings.push(format!(
        "two manifests of identical runs differ in {} and in the {} entry of `files`; \
         `build_utc` is the caller's timestamp and the recording embeds this manifest, so \
         its bytes and its digest move with it. Compare manifests with \
         v2xw_engine::manifest::comparison_digest, which excludes exactly those.",
        VARYING_FIELDS.join(" and "),
        FILES_EMBEDDING_THE_MANIFEST.join(", ")
    ));

    if GIT_COMMIT == "unknown" {
        m.warnings.push(
            "git commit not recorded: neither V2XW_GIT_COMMIT nor a git repository was \
             available at build time, so build_hash pins the compiler, the target and the \
             crate versions but not the source revision"
                .to_string(),
        );
    } else if GIT_DIRTY == "dirty" {
        m.warnings.push(format!(
            "the working tree had uncommitted tracked changes when this engine was built, \
             so commit {GIT_COMMIT} names source that is not what ran; build_hash covers \
             the fact of the divergence but not its content, and nothing available to a \
             running program can"
        ));
    } else if GIT_DIRTY != "clean" {
        m.warnings.push(format!(
            "commit {GIT_COMMIT} is recorded but the build could not check whether the \
             working tree matched it, so the commit is a claim about the checkout and not \
             about the source that compiled"
        ));
    }
    if RUSTC_VERSION == "unknown" {
        m.warnings.push(
            "compiler version not recorded: the build script could not run rustc --version"
                .to_string(),
        );
    }
    Ok(m)
}

/// A copy of `m` with everything that varies between identical runs removed.
///
/// [`VARYING_FIELDS`] are blanked and [`FILES_EMBEDDING_THE_MANIFEST`] are dropped from
/// `files`. Everything else is carried through untouched, including the warnings — a
/// manifest that stopped warning when it was compared would be a manifest whose
/// comparison hid the thing it was warning about.
///
/// The result is not a manifest of anything; it exists to be hashed or diffed.
pub fn comparison_copy(m: &Manifest) -> Manifest {
    let mut out = m.clone();
    out.build_utc = String::new();
    out.data_digest = None;
    out.files
        .retain(|f| !FILES_EMBEDDING_THE_MANIFEST.contains(&f.path.as_str()));
    out
}

/// SHA-256 over [`comparison_copy`]'s canonical JSON: the number two runs of one scenario
/// must agree on.
///
/// # Errors
/// [`crate::EngineError::Core`] if the manifest will not serialise, which for a
/// [`Manifest`] means a `serde_json` failure rather than a manifest problem.
pub fn comparison_digest(m: &Manifest) -> Result<String> {
    // The same canonicalisation the scenario hash uses: keys sorted at every depth, so
    // two manifests that carry the same facts hash the same whatever order `serde_json`
    // happened to emit them in.
    let json = v2xw_core::hash::canonical_json(&comparison_copy(m))?;
    Ok(v2xw_core::hash::sha256_hex(&json))
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

    /// Two manifests that differ only in the fields an invocation owns produce the same
    /// comparison digest, and one that differs in a field the *run* owns does not.
    ///
    /// Injected rather than asserted: the second half changes `scenario_hash` and checks
    /// the digest moves, because a digest that ignored everything would pass the first
    /// half and prove nothing. This is the check the audit found missing — the manifest
    /// was the one artefact that could not be compared byte for byte, and nothing said so.
    #[test]
    fn the_comparison_digest_ignores_only_what_an_invocation_owns() {
        let mut a = Manifest::new("0.1.0", 42);
        a.scenario_hash = "aa".repeat(32);
        a.build_utc = "2026-09-22T10:00:00Z".to_string();
        a.add_file("metrics.json", b"{}\n");
        a.add_file("recording.mcap", b"one run's container bytes");
        a.finalize();

        let mut b = a.clone();
        b.build_utc = "2026-09-22T11:30:05Z".to_string();
        // The recording embeds the manifest, so its bytes — and therefore its digest and
        // the aggregate over the files — move with the timestamp.
        b.add_file("recording.mcap", b"the other run's container bytes");
        b.finalize();

        assert_ne!(
            a.to_json_pretty().expect("json"),
            b.to_json_pretty().expect("json"),
            "the two manifests are supposed to differ; the digest is what must not"
        );
        assert_eq!(
            comparison_digest(&a).expect("digest"),
            comparison_digest(&b).expect("digest"),
        );

        // And it is not ignoring everything: a fact about the run moves it.
        let mut c = b.clone();
        c.scenario_hash = "bb".repeat(32);
        assert_ne!(
            comparison_digest(&b).expect("digest"),
            comparison_digest(&c).expect("digest"),
            "a changed scenario hash must change the comparison digest"
        );

        // A file whose digest is genuinely reproducible is still covered.
        let mut d = b.clone();
        d.add_file("metrics.json", b"{\"samples\":[]}\n");
        assert_ne!(
            comparison_digest(&b).expect("digest"),
            comparison_digest(&d).expect("digest"),
            "metrics.json does not embed the manifest, so it must stay in the digest"
        );
    }

    /// The copy blanks exactly the two fields it names and drops exactly the files it
    /// names, and nothing else.
    #[test]
    fn the_comparison_copy_removes_exactly_what_it_documents() {
        let mut m = Manifest::new("0.1.0", 7);
        m.build_utc = "2026-09-22T10:00:00Z".to_string();
        m.warnings.push("a warning worth keeping".to_string());
        m.add_file("metrics.json", b"{}\n");
        m.add_file("recording.mcap", b"container");
        m.finalize();

        let c = comparison_copy(&m);
        assert!(c.build_utc.is_empty());
        assert_eq!(c.data_digest, None);
        assert_eq!(
            c.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["metrics.json"]
        );
        // A warning is a fact about the run and survives the comparison, because a
        // comparison that hid the warning would hide what it warns about.
        assert_eq!(c.warnings, m.warnings);
        assert_eq!(c.master_seed, m.master_seed);
        assert_eq!(c.engine_version, m.engine_version);
    }

    /// The manifest says, in itself, that two of its fields cannot be compared. The
    /// warning naming them is what makes the exclusion discoverable without reading this
    /// module.
    #[test]
    fn the_manifest_warns_about_its_own_varying_fields() {
        let scenario = crate::scenario::Scenario::minimal();
        let world = crate::wiring::build_world(&scenario).expect("procedural world builds");
        let mut registry = Registry::new();
        crate::wiring::register_all(&mut registry).expect("models register");
        let m = assemble(&scenario, &world, &registry, "2026-09-22T00:00:00Z").expect("assembles");
        for field in VARYING_FIELDS {
            assert!(
                m.warnings.iter().any(|w| w.contains(field)),
                "no warning names the varying field {field}: {:?}",
                m.warnings
            );
        }
        assert!(
            m.warnings.iter().any(|w| w.contains("comparison_digest")),
            "the warning does not say how to compare two manifests: {:?}",
            m.warnings
        );
    }

    /// The build script answered about the source revision — either with a commit or with
    /// an explicit "unknown", and with a dirty flag either way.
    ///
    /// The audit's finding was that `git_commit` read `"unknown"` on every run while the
    /// repository was right there. This does not assert a commit was found (a vendored
    /// build has none); it asserts the build *asked*, and that the two constants are the
    /// three values the manifest's warnings are written against.
    #[test]
    fn the_build_recorded_a_source_revision_or_said_it_could_not() {
        assert!(
            matches!(GIT_DIRTY, "clean" | "dirty" | "unknown"),
            "V2XW_GIT_DIRTY is {GIT_DIRTY}, which is none of clean, dirty or unknown"
        );
        if GIT_COMMIT != "unknown" {
            assert_eq!(
                GIT_COMMIT.len(),
                40,
                "a recorded commit is a full 40-character SHA-1: {GIT_COMMIT}"
            );
            assert!(GIT_COMMIT.chars().all(|c| c.is_ascii_hexdigit()));
        }
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
