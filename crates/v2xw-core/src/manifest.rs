//! The run manifest: everything needed to reproduce or refuse to reproduce a run.
//!
//! Fields follow 02-architecture.md §6.5: scenario hash, master seed, engine version,
//! git commit, build hash, platform triple, crate versions, plug-in pins with content
//! hashes, model-card versions, world content hash, crypto mode, time-dilation windows,
//! per-file SHA-256 digests and an aggregate data digest.
//!
//! # Wall-clock time
//!
//! [`Manifest::build_utc`] is the one field in the whole engine that names a real-world
//! instant, and the engine never fills it in: the caller supplies it. It is deliberately
//! **excluded from the aggregate digest**, exactly as the legacy engine excluded its own
//! `build_utc` (`legacy/scms_sim_ref/mock_pipeline/run.py`), so that two runs of the same
//! scenario at different times still produce identical digests.
//!
//! # Aggregate digest
//!
//! [`Manifest::finalize`] reproduces the legacy `_data_digest` byte for byte, so datasets
//! produced by the new engine stay comparable with the frozen reference corpus:
//!
//! ```text
//! h = SHA-256()
//! for rel in sorted(paths):
//!     h.update(rel.as_bytes())
//!     h.update(hex(sha256(file_bytes)).as_bytes())   # 64 lower-case hex characters
//! digest = hex(h)
//! ```
//!
//! Note what it hashes: the *hex text* of each file's digest, not its 32 raw bytes, and
//! the path with no separator between the two. Rust's `str` ordering is UTF-8 byte order,
//! which for valid UTF-8 is the same as Python's code-point ordering, so `sorted()`
//! agrees on every path.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hash::{hex_encode, sha256_hex};
use crate::time::SimTime;

/// How cryptography was handled in the run (scenario field `security.crypto_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CryptoMode {
    /// Costs and sizes are modelled; no real keys, signatures or certificates.
    Modeled,
    /// Real primitives are executed (slower, and the results are bit-exact crypto).
    Real,
}

impl core::fmt::Display for CryptoMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            CryptoMode::Modeled => "modeled",
            CryptoMode::Real => "real",
        })
    }
}

/// A window in which only the backend and abstract-mobility tiers ran, so radio events
/// were skipped (02-architecture.md §5.4).
///
/// Metrics that depend on radio events are marked `not-observed` inside these windows,
/// which is why they have to be in the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeDilationWindow {
    /// Start of the window, simulated nanoseconds.
    pub start_ns: SimTime,
    /// End of the window, simulated nanoseconds.
    pub end_ns: SimTime,
}

impl TimeDilationWindow {
    /// Creates a window.
    pub const fn new(start_ns: SimTime, end_ns: SimTime) -> Self {
        Self { start_ns, end_ns }
    }

    /// True if `t` falls inside the window (start inclusive, end exclusive).
    pub const fn contains(&self, t: SimTime) -> bool {
        t >= self.start_ns && t < self.end_ns
    }
}

/// A pinned plug-in: what ran, at which version, with which content hash.
///
/// A replay refuses a different content hash unless `--allow-plugin-drift` is given, and
/// that flag is itself recorded (ADR 0007 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPin {
    /// Model or plug-in id.
    pub id: String,
    /// Its semver version.
    pub version: String,
    /// Lower-case hex SHA-256 of its content (card canonical bytes, or the artefact).
    pub content_hash: String,
}

impl PluginPin {
    /// Creates a pin.
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            content_hash: content_hash.into(),
        }
    }
}

/// One output file and its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDigest {
    /// Path relative to the run's output directory, with `/` separators.
    pub path: String,
    /// Lower-case hex SHA-256 of the file's bytes.
    pub sha256: String,
}

/// The run manifest.
///
/// Every field is public: the kernel fills them in as it learns them, and the recorder
/// writes the struct out as the `manifest` MCAP record (03-interfaces.md §14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema id of the manifest itself.
    pub schema: String,
    /// SHA-256 (hex) of the scenario's canonical JSON.
    pub scenario_hash: String,
    /// The run's master seed; every RNG stream derives from it (ADR 0004 §3).
    pub master_seed: u64,
    /// Engine version (the `v2xw-core` crate version).
    pub engine_version: String,
    /// Git commit the engine was built from.
    pub git_commit: String,
    /// Hash of the build itself (binary or build inputs), for builds outside git.
    pub build_hash: String,
    /// Target triple the run executed on, e.g. `aarch64-apple-darwin`.
    pub platform: String,
    /// Versions of the crates that took part, by crate name.
    pub crate_versions: std::collections::BTreeMap<String, String>,
    /// **The only wall-clock field in the engine**, supplied by the caller and excluded
    /// from [`Manifest::finalize`]'s digest. ISO 8601 UTC by convention.
    pub build_utc: String,
    /// The plug-ins that ran, pinned by version and content hash.
    pub plugins: Vec<PluginPin>,
    /// `(model id, card version)` for every registered model, in id order.
    pub model_cards: Vec<(String, String)>,
    /// Content hash (hex) of the world geometry (invariant I-W2).
    pub world_hash: String,
    /// SUMO's version string, when the high mobility tier was used (ADR 0005).
    pub sumo_version: Option<String>,
    /// How cryptography was handled.
    pub crypto_mode: CryptoMode,
    /// Windows in which radio events were skipped.
    pub time_dilation_windows: Vec<TimeDilationWindow>,
    /// Validator warnings worth carrying with the data, such as registry rule R2's
    /// "unvalidated model used at the high tier".
    pub warnings: Vec<String>,
    /// Every output file with its digest, kept sorted by path.
    pub files: Vec<FileDigest>,
    /// The aggregate digest over [`Manifest::files`], once [`Manifest::finalize`] has run.
    pub data_digest: Option<String>,
}

impl Manifest {
    /// Schema id written into [`Manifest::schema`].
    pub const SCHEMA: &'static str = "v2xw/manifest/1";

    /// A manifest with the two fields that have no sensible default, and empty or
    /// placeholder values elsewhere for the caller to fill in.
    pub fn new(engine_version: impl Into<String>, master_seed: u64) -> Self {
        Self {
            schema: Self::SCHEMA.to_string(),
            scenario_hash: String::new(),
            master_seed,
            engine_version: engine_version.into(),
            git_commit: String::new(),
            build_hash: String::new(),
            platform: String::new(),
            crate_versions: std::collections::BTreeMap::new(),
            build_utc: String::new(),
            plugins: Vec::new(),
            model_cards: Vec::new(),
            world_hash: String::new(),
            sumo_version: None,
            crypto_mode: CryptoMode::Modeled,
            time_dilation_windows: Vec::new(),
            warnings: Vec::new(),
            files: Vec::new(),
            data_digest: None,
        }
    }

    /// Records an output file and its SHA-256, computed from `bytes`.
    ///
    /// `path` is relative to the run's output directory and is what the digest hashes, so
    /// it must use `/` separators on every platform. Adding the same path twice replaces
    /// the earlier digest (a file rewritten during a run keeps one entry), and the list
    /// is kept sorted by path so the manifest reads the same however the run wrote it.
    ///
    /// Invalidates any previously computed [`Manifest::data_digest`].
    pub fn add_file(&mut self, path: impl Into<String>, bytes: &[u8]) -> &FileDigest {
        let path = path.into();
        let digest = FileDigest {
            sha256: sha256_hex(bytes),
            path,
        };
        self.data_digest = None;
        match self.files.binary_search_by(|f| f.path.cmp(&digest.path)) {
            Ok(i) => {
                self.files[i] = digest;
                &self.files[i]
            }
            Err(i) => {
                self.files.insert(i, digest);
                &self.files[i]
            }
        }
    }

    /// Records an output file whose digest was computed elsewhere (a streamed file, say).
    ///
    /// `sha256_hex` must be 64 lower-case hex characters, because that text is what the
    /// aggregate digest hashes.
    pub fn add_file_digest(&mut self, path: impl Into<String>, sha256_hex: impl Into<String>) {
        let digest = FileDigest {
            path: path.into(),
            sha256: sha256_hex.into(),
        };
        self.data_digest = None;
        match self.files.binary_search_by(|f| f.path.cmp(&digest.path)) {
            Ok(i) => self.files[i] = digest,
            Err(i) => self.files.insert(i, digest),
        }
    }

    /// Computes the aggregate data digest, stores it in [`Manifest::data_digest`] and
    /// returns it.
    ///
    /// Reproduces the legacy `_data_digest` exactly (module documentation): SHA-256 over
    /// the path-sorted sequence of `path_bytes ‖ file_digest_hex_bytes`. The manifest
    /// itself is not among the files, so the digest does not depend on itself or on
    /// [`Manifest::build_utc`].
    pub fn finalize(&mut self) -> String {
        let mut h = Sha256::new();
        // `self.files` is maintained in path order by `add_file`; sort defensively in
        // case a caller pushed to the vector directly.
        let mut files: Vec<&FileDigest> = self.files.iter().collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        for f in files {
            h.update(f.path.as_bytes());
            h.update(f.sha256.as_bytes());
        }
        let digest = hex_encode(&h.finalize());
        self.data_digest = Some(digest.clone());
        digest
    }

    /// The digest of one file, if recorded.
    pub fn file_digest(&self, path: &str) -> Option<&FileDigest> {
        self.files.iter().find(|f| f.path == path)
    }

    /// True if `t` falls inside a time-dilation window, i.e. radio events were skipped.
    pub fn is_dilated(&self, t: SimTime) -> bool {
        self.time_dilation_windows.iter().any(|w| w.contains(t))
    }

    /// Serialises to indented JSON with the keys in a stable order, the form written next
    /// to a run's outputs.
    pub fn to_json_pretty(&self) -> crate::error::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parses a manifest from JSON.
    pub fn from_json(s: &str) -> crate::error::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_hashed_and_kept_sorted() {
        let mut m = Manifest::new("0.1.0", 42);
        m.add_file("ma/reports.jsonl", b"b\n");
        m.add_file("ground_truth/labels.jsonl", b"a\n");
        assert_eq!(
            m.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["ground_truth/labels.jsonl", "ma/reports.jsonl"]
        );
        assert_eq!(
            m.file_digest("ma/reports.jsonl").unwrap().sha256,
            crate::hash::sha256_hex(b"b\n")
        );
        // Re-adding replaces rather than duplicating.
        m.add_file("ma/reports.jsonl", b"b2\n");
        assert_eq!(m.files.len(), 2);
        assert_eq!(
            m.file_digest("ma/reports.jsonl").unwrap().sha256,
            crate::hash::sha256_hex(b"b2\n")
        );
    }

    /// The digest must match the legacy engine's `_data_digest`
    /// (`legacy/scms_sim_ref/mock_pipeline/run.py`) byte for byte, or the new engine's
    /// datasets stop being comparable with the frozen reference corpus.
    ///
    /// The expected value below was computed independently with the legacy algorithm:
    ///
    /// ```python
    /// h = hashlib.sha256()
    /// for rel in sorted({"ground_truth/labels.jsonl": b"a\n", "ma/reports.jsonl": b"b\n"}):
    ///     h.update(rel.encode())
    ///     h.update(hashlib.sha256(files[rel]).hexdigest().encode())
    /// h.hexdigest()
    /// ```
    #[test]
    fn data_digest_matches_the_legacy_algorithm() {
        let mut m = Manifest::new("0.1.0", 42);
        // Inserted in the "wrong" order on purpose: the digest is path-sorted.
        m.add_file("ma/reports.jsonl", b"b\n");
        m.add_file("ground_truth/labels.jsonl", b"a\n");
        let digest = m.finalize();
        assert_eq!(digest, LEGACY_DIGEST);
        assert_eq!(m.data_digest.as_deref(), Some(LEGACY_DIGEST));

        // Recomputing the same way by hand, to document the algorithm in code.
        let mut h = Sha256::new();
        for (path, bytes) in [
            ("ground_truth/labels.jsonl", &b"a\n"[..]),
            ("ma/reports.jsonl", &b"b\n"[..]),
        ] {
            h.update(path.as_bytes());
            h.update(crate::hash::sha256_hex(bytes).as_bytes());
        }
        assert_eq!(hex_encode(&h.finalize()), LEGACY_DIGEST);
    }

    #[test]
    fn digest_ignores_insertion_order_and_wall_clock() {
        let mut a = Manifest::new("0.1.0", 1);
        a.add_file("z.jsonl", b"z");
        a.add_file("a.jsonl", b"a");
        a.build_utc = "2026-09-18T10:00:00Z".to_string();

        let mut b = Manifest::new("0.1.0", 1);
        b.add_file("a.jsonl", b"a");
        b.add_file("z.jsonl", b"z");
        b.build_utc = "2031-01-01T00:00:00Z".to_string();

        assert_eq!(a.finalize(), b.finalize());

        // Content changes the digest.
        let mut c = Manifest::new("0.1.0", 1);
        c.add_file("a.jsonl", b"a");
        c.add_file("z.jsonl", b"different");
        assert_ne!(a.finalize(), c.finalize());

        // So does a path change.
        let mut d = Manifest::new("0.1.0", 1);
        d.add_file("a.jsonl", b"a");
        d.add_file("zz.jsonl", b"z");
        assert_ne!(a.finalize(), d.finalize());

        // An empty file set still has a digest: SHA-256 of nothing.
        let mut e = Manifest::new("0.1.0", 1);
        assert_eq!(e.finalize(), crate::hash::sha256_hex(b""));
    }

    #[test]
    fn adding_a_file_invalidates_the_digest() {
        let mut m = Manifest::new("0.1.0", 1);
        m.add_file("a", b"a");
        let first = m.finalize();
        assert!(m.data_digest.is_some());
        m.add_file("b", b"b");
        assert_eq!(m.data_digest, None, "a new file invalidates the digest");
        assert_ne!(m.finalize(), first);
    }

    #[test]
    fn streamed_digests_can_be_supplied_directly() {
        let mut m = Manifest::new("0.1.0", 1);
        m.add_file_digest("big.mcap", crate::hash::sha256_hex(b"x"));
        m.add_file_digest("big.mcap", crate::hash::sha256_hex(b"y"));
        assert_eq!(m.files.len(), 1);
        let mut n = Manifest::new("0.1.0", 1);
        n.add_file("big.mcap", b"y");
        assert_eq!(m.finalize(), n.finalize());
    }

    #[test]
    fn time_dilation_windows() {
        let mut m = Manifest::new("0.1.0", 1);
        m.time_dilation_windows
            .push(TimeDilationWindow::new(1_000, 2_000));
        assert!(!m.is_dilated(999));
        assert!(m.is_dilated(1_000));
        assert!(m.is_dilated(1_999));
        assert!(!m.is_dilated(2_000));
    }

    #[test]
    fn json_round_trip() {
        let mut m = Manifest::new("0.1.0", 0xABCD);
        m.scenario_hash = crate::hash::sha256_hex(b"{}");
        m.git_commit = "b6183cd".to_string();
        m.build_hash = "deadbeef".to_string();
        m.platform = "aarch64-apple-darwin".to_string();
        m.build_utc = "2026-09-18T12:00:00Z".to_string();
        m.crate_versions
            .insert("v2xw-core".to_string(), "0.1.0".to_string());
        m.plugins.push(PluginPin::new("radio/a", "1.0.0", "00ff"));
        m.model_cards
            .push(("radio/a".to_string(), "1.0.0".to_string()));
        m.world_hash = crate::hash::sha256_hex(b"world");
        m.sumo_version = Some("1.20.0".to_string());
        m.crypto_mode = CryptoMode::Real;
        m.warnings.push("model radio/a is unvalidated".to_string());
        m.add_file("out.jsonl", b"{}\n");
        m.finalize();

        let json = m.to_json_pretty().unwrap();
        assert_eq!(Manifest::from_json(&json).unwrap(), m);
        assert!(json.contains("\"crypto_mode\": \"real\""));
        assert!(json.contains("\"schema\": \"v2xw/manifest/1\""));
        assert_eq!(CryptoMode::Modeled.to_string(), "modeled");
    }

    /// Pinned in one place so the test above reads as a comparison, not as a definition.
    const LEGACY_DIGEST: &str = "1bca7222c42cb2ad99229a98c57771f1ab01d72e4845a5f8411a564e59a6df62";
}
