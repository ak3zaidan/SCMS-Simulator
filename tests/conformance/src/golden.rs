//! The golden determinism harness: one scenario, one digest, compared against the record.
//!
//! ADR 0004's contract is "same scenario file + seed + engine build + plug-in set ⇒
//! byte-identical outputs on macOS/Linux/Windows". Two things have to be true for a test to
//! be evidence of that, and only one of them is a test:
//!
//! 1. **A later run matches an earlier one.** That is a comparison against a value
//!    committed to the repository ([`golden_path`]), not against a second run in the same
//!    process — two runs in one process share a compiler, a platform and a libm, so
//!    agreeing proves far less than it looks like it proves.
//! 2. **Three platforms agree.** No `#[test]` can establish this, because each CI job only
//!    checks its own assertions; a job has to *print* a value and a fourth job has to
//!    compare the three. That is what `src/bin/golden_digest.rs` is for, and it calls
//!    [`digest_of`] — the same function the test calls — so the number CI compares and the
//!    number the test asserts on cannot drift apart.
//!
//! `.github/workflows/ci.yml` already runs a three-platform comparison over the world
//! importer's digests. This harness is the shape that comparison grows into as the engine
//! joins it: the job body becomes `cargo run -p v2xw-conformance --bin v2xw-golden-digest`
//! and the existing `determinism-compare` job is unchanged.
//!
//! # When there is no golden record yet
//!
//! [`compare_against_golden`] returns [`Comparison::Missing`] and the test **fails**,
//! naming the environment variable that blesses it. A harness that quietly wrote the file
//! and passed would be a check that cannot fail: the first run would create its own oracle
//! and every run afterwards would agree with it. The first failure is the point at which a
//! human looks at the number and decides it is right.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use v2xw_engine::{DigestRecorder, Engine, Scenario};

/// The manifest timestamp every golden run is built with.
///
/// [`v2xw_core::Manifest::build_utc`] is the only wall-clock field in the engine and is
/// excluded from every digest, so its value cannot affect the result — which is exactly why
/// it is pinned here: a harness that passed `chrono::Utc::now()` would be reading a clock
/// to produce a value it then asserts is clock-independent.
pub const BUILD_UTC: &str = "2026-09-22T00:00:00Z";

/// The environment variable that writes a missing or changed golden record.
pub const BLESS_ENV: &str = "V2XW_CONFORMANCE_BLESS";

/// A scenario the harness runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Case {
    /// The name of the golden file, without its extension.
    pub name: &'static str,
    /// The scenario file, relative to the repository root.
    pub scenario: &'static str,
    /// Why this case is in the set.
    pub because: &'static str,
}

/// The golden cases.
///
/// Deliberately short. A golden set that takes minutes is a golden set that gets disabled;
/// the value is in one scenario that exercises mobility, radio, the message layer, the
/// security envelope and the recorder, run on every platform, not in twenty that are run
/// nowhere.
pub const CASES: &[Case] = &[Case {
    name: "grid-traffic",
    scenario: "crates/v2xw-engine/scenarios/grid-traffic.yaml",
    because: "ten simulated seconds over a procedural grid with Poisson demand and a \
              weather front: the shortest run that still puts frames on the air, equips \
              vehicles, evaluates receptions and writes every channel",
}];

/// What one run produced, in the form the record stores and CI compares.
///
/// Every field is either a digest or a count. There is no float, no path and no timestamp,
/// because a golden record whose fields can differ between two correct runs is a golden
/// record that gets `--force`d until it means nothing — which is the failure
/// `docs/design/findings/slice-verification.md` records against the run manifest, whose two
/// varying fields made it the one artefact that could not be compared byte for byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDigest {
    /// The case name.
    pub case: String,
    /// SHA-256 (hex) of the scenario's canonical JSON, from the run manifest.
    pub scenario_hash: String,
    /// Content hash (hex) of the world geometry, from the run manifest.
    pub world_hash: String,
    /// The master seed the run used.
    pub master_seed: u64,
    /// The instant the run ended, nanoseconds.
    pub end_ns: u64,
    /// How many records the recorder was handed.
    pub records: u64,
    /// `SHA-256(t ‖ channel ‖ visibility ‖ json)*` over every record, in order.
    pub record_digest: String,
    /// Records and JSON bytes per channel, by channel name.
    pub per_channel: BTreeMap<String, (u64, u64)>,
}

impl RunDigest {
    /// The record as `key=value` lines, sorted, for a CI job to publish and compare.
    ///
    /// Line-oriented rather than JSON because the comparison job is a `diff` and a human
    /// reading a failed one wants to see which line moved.
    #[must_use]
    pub fn to_key_values(&self) -> String {
        let mut lines = vec![
            format!("case={}", self.case),
            format!("scenario_hash={}", self.scenario_hash),
            format!("world_hash={}", self.world_hash),
            format!("master_seed={}", self.master_seed),
            format!("end_ns={}", self.end_ns),
            format!("records={}", self.records),
            format!("record_digest={}", self.record_digest),
        ];
        for (channel, (count, bytes)) in &self.per_channel {
            lines.push(format!("channel.{channel}={count},{bytes}"));
        }
        lines.sort();
        lines.join("\n")
    }

    /// The fields that differ between two records, as human-readable lines.
    #[must_use]
    pub fn differences(&self, other: &RunDigest) -> Vec<String> {
        let mut out = Vec::new();
        let mut note = |field: &str, a: String, b: String| {
            if a != b {
                out.push(format!("{field}: golden {a}, this run {b}"));
            }
        };
        note("case", self.case.clone(), other.case.clone());
        note(
            "scenario_hash",
            self.scenario_hash.clone(),
            other.scenario_hash.clone(),
        );
        note(
            "world_hash",
            self.world_hash.clone(),
            other.world_hash.clone(),
        );
        note(
            "master_seed",
            self.master_seed.to_string(),
            other.master_seed.to_string(),
        );
        note("end_ns", self.end_ns.to_string(), other.end_ns.to_string());
        note(
            "records",
            self.records.to_string(),
            other.records.to_string(),
        );
        note(
            "record_digest",
            self.record_digest.clone(),
            other.record_digest.clone(),
        );
        let channels: std::collections::BTreeSet<&String> = self
            .per_channel
            .keys()
            .chain(other.per_channel.keys())
            .collect();
        for channel in channels {
            let a = self.per_channel.get(channel);
            let b = other.per_channel.get(channel);
            if a != b {
                out.push(format!("channel {channel}: golden {a:?}, this run {b:?}"));
            }
        }
        out
    }
}

/// What went wrong.
#[derive(Debug)]
pub enum GoldenError {
    /// The engine refused to load or to run the scenario.
    Engine(String),
    /// A golden file could not be read or written.
    Io(String),
    /// A golden file exists but is not a [`RunDigest`].
    Corrupt(String),
}

impl core::fmt::Display for GoldenError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GoldenError::Engine(m) => write!(f, "engine: {m}"),
            GoldenError::Io(m) => write!(f, "io: {m}"),
            GoldenError::Corrupt(m) => write!(f, "corrupt golden record: {m}"),
        }
    }
}

impl std::error::Error for GoldenError {}

/// Runs `case` and returns its digest.
///
/// # Errors
/// [`GoldenError::Engine`] if the scenario does not load, does not validate, or the run
/// aborts.
pub fn digest_of(case: &Case) -> Result<RunDigest, GoldenError> {
    let path = crate::repo_root().join(case.scenario);
    digest_of_scenario(case.name, &path)
}

/// Runs the scenario at `path` under the name `case_name` and returns its digest.
///
/// Split out from [`digest_of`] so the CI binary can be pointed at a scenario that is not
/// in [`CASES`] without a second copy of the run code.
///
/// # Errors
/// As [`digest_of`].
pub fn digest_of_scenario(case_name: &str, path: &Path) -> Result<RunDigest, GoldenError> {
    let scenario = Scenario::load(path)
        .map_err(|e| GoldenError::Engine(format!("loading {}: {e}", path.display())))?;
    let mut engine = Engine::build(scenario, BUILD_UTC)
        .map_err(|e| GoldenError::Engine(format!("building {case_name}: {e}")))?;
    let manifest = engine.manifest().clone();
    let mut recorder = DigestRecorder::new();
    let report = engine
        .run(&mut recorder)
        .map_err(|e| GoldenError::Engine(format!("running {case_name}: {e}")))?;
    Ok(RunDigest {
        case: case_name.to_string(),
        scenario_hash: manifest.scenario_hash,
        world_hash: manifest.world_hash,
        master_seed: manifest.master_seed,
        end_ns: report.end_ns,
        records: recorder.written(),
        record_digest: recorder.digest_hex(),
        per_channel: recorder.per_channel().clone(),
    })
}

/// The directory the golden records live in.
#[must_use]
pub fn golden_dir() -> PathBuf {
    crate::repo_root().join("tests/conformance/golden")
}

/// The file one case's record lives in.
#[must_use]
pub fn golden_path(name: &str) -> PathBuf {
    golden_dir().join(format!("{name}.json"))
}

/// True when the caller asked for missing or changed records to be written.
#[must_use]
pub fn blessing() -> bool {
    std::env::var_os(BLESS_ENV).is_some_and(|v| v != "0" && !v.is_empty())
}

/// Reads a case's golden record, if it has one.
///
/// # Errors
/// [`GoldenError::Io`] if the file exists but cannot be read, [`GoldenError::Corrupt`] if
/// it is not a [`RunDigest`].
pub fn load_golden(name: &str) -> Result<Option<RunDigest>, GoldenError> {
    let path = golden_path(name);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| GoldenError::Io(format!("{}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| GoldenError::Corrupt(format!("{}: {e}", path.display())))
}

/// Writes a case's golden record, creating the directory if it is missing.
///
/// # Errors
/// [`GoldenError::Io`] for any file-system failure.
pub fn write_golden(digest: &RunDigest) -> Result<PathBuf, GoldenError> {
    let dir = golden_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| GoldenError::Io(format!("{}: {e}", dir.display())))?;
    let path = golden_path(&digest.case);
    let mut text = serde_json::to_string_pretty(digest)
        .map_err(|e| GoldenError::Corrupt(format!("serialising {}: {e}", digest.case)))?;
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| GoldenError::Io(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// The outcome of comparing a run against its golden record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comparison {
    /// The run matches the record.
    Match,
    /// No record has been committed yet.
    Missing {
        /// Where it would go.
        path: PathBuf,
    },
    /// The run differs from the record.
    Differs {
        /// One line per differing field.
        lines: Vec<String>,
    },
    /// The record was missing or differed, and `BLESS_ENV` asked for it to be written.
    Blessed {
        /// Where it was written.
        path: PathBuf,
    },
}

/// Compares `digest` against the committed record for its case, blessing it if asked.
///
/// # Errors
/// As [`load_golden`] and [`write_golden`].
pub fn compare_against_golden(digest: &RunDigest) -> Result<Comparison, GoldenError> {
    let existing = load_golden(&digest.case)?;
    match existing {
        Some(golden) if golden == *digest => Ok(Comparison::Match),
        Some(golden) => {
            if blessing() {
                let path = write_golden(digest)?;
                Ok(Comparison::Blessed { path })
            } else {
                Ok(Comparison::Differs {
                    lines: golden.differences(digest),
                })
            }
        }
        None => {
            if blessing() {
                let path = write_golden(digest)?;
                Ok(Comparison::Blessed { path })
            } else {
                Ok(Comparison::Missing {
                    path: golden_path(&digest.case),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(case: &str) -> RunDigest {
        RunDigest {
            case: case.to_string(),
            scenario_hash: "aa".repeat(32),
            world_hash: "bb".repeat(32),
            master_seed: 0xC0FF_EE5E,
            end_ns: 10_000_000_000,
            records: 2_189,
            record_digest: "cc".repeat(32),
            per_channel: BTreeMap::from([("node.tx".to_string(), (955u64, 120_000u64))]),
        }
    }

    /// The key/value form is stable, sorted and carries every field, so a CI `diff` of two
    /// platforms names the field that moved.
    #[test]
    fn the_key_value_form_carries_every_field_and_is_sorted() {
        let text = sample("x").to_key_values();
        for key in [
            "case=",
            "scenario_hash=",
            "world_hash=",
            "master_seed=",
            "end_ns=",
            "records=",
            "record_digest=",
            "channel.node.tx=955,120000",
        ] {
            assert!(text.contains(key), "{key} missing from:\n{text}");
        }
        let lines: Vec<&str> = text.lines().collect();
        let mut sorted = lines.clone();
        sorted.sort_unstable();
        assert_eq!(lines, sorted);
    }

    /// The injected fault: change one field and the comparison must name it.
    ///
    /// Without this the difference report could be an empty list for every input, which is
    /// the shape of a check that cannot fail.
    #[test]
    fn a_changed_field_is_named_and_an_unchanged_record_is_silent() {
        let a = sample("x");
        assert!(a.differences(&a).is_empty());

        let mut b = sample("x");
        b.record_digest = "dd".repeat(32);
        b.per_channel.insert("phy.rx".to_string(), (12, 34));
        let diff = a.differences(&b);
        assert!(
            diff.iter().any(|l| l.starts_with("record_digest:")),
            "{diff:?}"
        );
        assert!(
            diff.iter().any(|l| l.contains("channel phy.rx")),
            "{diff:?}"
        );
        assert_eq!(diff.len(), 2, "{diff:?}");
    }

    /// The case list points at files that exist. A golden case whose scenario was moved
    /// would otherwise fail with a file-not-found deep inside the engine.
    #[test]
    fn every_case_names_a_scenario_that_exists() {
        for case in CASES {
            let path = crate::repo_root().join(case.scenario);
            assert!(path.is_file(), "{} does not exist", path.display());
            assert!(!case.because.is_empty());
        }
    }
}
