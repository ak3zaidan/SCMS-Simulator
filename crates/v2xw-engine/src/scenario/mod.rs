//! The scenario schema and its loader (03-interfaces.md §13, build decision D8).
//!
//! One file, YAML or JSON, describing a whole run. The pipeline is fixed and each stage is
//! a module of its own:
//!
//! ```text
//! bytes ──parse──► untyped document ──migrate──► current schema ──merge base──► document
//!                                                                       │
//!                                             typed Scenario ◄──deserialise──┘
//!                                                     │
//!                                                  validate ──► Vec<ScenarioError>
//! ```
//!
//! The order is not arbitrary:
//!
//! * **Migrate before merge.** A base written against an older schema is migrated on its
//!   own, so an overlay never has to know which version its base was written in.
//! * **Merge before deserialise.** On the typed struct, "the author wrote the default" and
//!   "the author wrote nothing" are the same thing, so a typed merge would silently
//!   overwrite everything a base set. See [`merge`].
//! * **Deserialise before validate.** Validation's job is cross-field conflicts in the
//!   author's vocabulary; type errors are serde's job and serde's messages are better.
//!
//! # Determinism
//!
//! The loader reads files and nothing else — no environment, no clock, no network. Two
//! loads of one file therefore produce the same [`Scenario`], and
//! [`Scenario::content_hash`] over its canonical JSON is what the run manifest pins
//! (02-architecture.md §6.5). `meta.base` is resolved relative to the **including file's**
//! directory, so a scenario tree relocates without editing.

pub mod merge;
pub mod migrate;
pub mod publish;
pub mod schema;
pub mod validate;

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{EngineError, Result, ScenarioError};

pub use merge::merge as merge_documents;
pub use migrate::{Chain, Migration};
pub use schema::{
    Actors, Attacker, Backend, BackendEntity, BackendLink, BuildingOptions, CURRENT_SCHEMA,
    CryptoModeSpec, DemandSpec, Detection, DilationWindow, Experiment, ExporterSpec, Focus,
    FocusRegion, Messages, Meta, ModelChoice, Net, Nodes, Radio, RadioTiers, Rat, Rsu, Scenario,
    PseudonymChangeSpec, Security, SignerIdPolicySpec, TerrainOptions, Threats, Time,
    TimelineItem, TimelineKind, VehicleClassSpec, Vehicles, Vru, Weather, WorldSpec,
};
pub use publish::{surface as scenario_surface, schema as scenario_schema};
pub use validate::{resolve_path, validate};

/// How many `meta.base` references may chain before the loader gives up.
///
/// A cycle is the reason: `a` based on `b` based on `a` would otherwise load forever. The
/// limit is generous because a preset hierarchy three or four deep is reasonable and
/// sixteen is not.
pub const MAX_BASE_DEPTH: usize = 16;

impl Scenario {
    /// Loads a scenario from a file, resolving `meta.base` relative to its directory.
    ///
    /// # Errors
    /// [`EngineError::Io`] if the file cannot be read, and [`EngineError::Scenario`] for
    /// everything else — a parse failure, an unknown schema version, an unresolvable base
    /// or the first validation conflict.
    pub fn load(path: impl AsRef<Path>) -> Result<Scenario> {
        let path = path.as_ref();
        let doc = load_document(path, 0)?;
        let scenario = Scenario::from_document(doc)?;
        scenario.validate()?;
        Ok(scenario)
    }

    /// Parses a scenario from a string, with `base_dir` for resolving `meta.base`.
    ///
    /// YAML and JSON are both accepted: JSON is a subset of YAML 1.2 and `serde_yml`
    /// parses both, so one entry point serves both and a file's extension does not change
    /// its meaning.
    ///
    /// # Errors
    /// As [`Scenario::load`].
    pub fn parse(text: &str, base_dir: Option<&Path>) -> Result<Scenario> {
        let mut doc = parse_document(text, "<memory>")?;
        Chain::shipped()
            .migrate(&mut doc)
            .map_err(EngineError::from)?;
        let doc = resolve_bases(doc, base_dir, 0)?;
        let scenario = Scenario::from_document(doc)?;
        scenario.validate()?;
        Ok(scenario)
    }

    /// Deserialises an already-merged, already-migrated document.
    ///
    /// Does **not** validate: [`Scenario::validate`] is separate so a tool can load a
    /// scenario it knows is broken in order to report on it.
    ///
    /// # Errors
    /// [`ScenarioError::Parse`] if the document does not match the schema.
    pub fn from_document(doc: Value) -> Result<Scenario> {
        serde_json::from_value(doc).map_err(|e| {
            EngineError::Scenario(ScenarioError::Parse {
                path: "<document>".to_string(),
                message: e.to_string(),
            })
        })
    }

    /// The first thing wrong with this scenario, if anything is.
    ///
    /// [`validate`] returns all of them; this is the pass-or-fail spelling.
    ///
    /// # Errors
    /// The first [`ScenarioError`] the rules produce, in schema order.
    pub fn validate(&self) -> core::result::Result<(), ScenarioError> {
        match validate(self).into_iter().next() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Writes the scenario as YAML.
    ///
    /// The round trip `parse(save(s)) == s` is a test in this module: a scenario a tool
    /// writes has to be one the loader reads, or the UI's "save as" silently corrupts.
    ///
    /// # Errors
    /// [`ScenarioError::Parse`] if the scenario does not serialise, which a plain-data
    /// struct does not.
    pub fn to_yaml(&self) -> core::result::Result<String, ScenarioError> {
        serde_yml::to_string(self).map_err(|e| ScenarioError::Parse {
            path: "<memory>".to_string(),
            message: e.to_string(),
        })
    }

    /// Writes the scenario as JSON.
    ///
    /// # Errors
    /// As [`Scenario::to_yaml`].
    pub fn to_json(&self) -> core::result::Result<String, ScenarioError> {
        serde_json::to_string_pretty(self).map_err(|e| ScenarioError::Parse {
            path: "<memory>".to_string(),
            message: e.to_string(),
        })
    }

    /// The canonical JSON bytes this scenario hashes as.
    ///
    /// [`v2xw_core::hash::canonical_json`] sorts object keys and normalises numbers, so
    /// two files that differ only in key order or in whitespace hash the same — which is
    /// what makes "same scenario ⇒ same manifest" (02-architecture.md §6.1) a statement
    /// about the scenario rather than about its formatting.
    ///
    /// # Errors
    /// [`ScenarioError::Parse`] if the scenario does not serialise.
    pub fn canonical_bytes(&self) -> core::result::Result<Vec<u8>, ScenarioError> {
        let value = serde_json::to_value(self).map_err(|e| ScenarioError::Parse {
            path: "<memory>".to_string(),
            message: e.to_string(),
        })?;
        let canonical =
            v2xw_core::hash::canonical_json(&value).map_err(|e| ScenarioError::Parse {
                path: "<memory>".to_string(),
                message: e.to_string(),
            })?;
        Ok(canonical)
    }

    /// The scenario hash the manifest records (02-architecture.md §6.5), hex-encoded.
    ///
    /// # Errors
    /// As [`Scenario::canonical_bytes`].
    pub fn content_hash(&self) -> core::result::Result<String, ScenarioError> {
        Ok(v2xw_core::hash::sha256_hex(&self.canonical_bytes()?))
    }

    /// A minimal valid scenario over a procedural world, for tests and as the starting
    /// point a UI offers.
    pub fn minimal() -> Scenario {
        Scenario {
            schema: CURRENT_SCHEMA.to_string(),
            meta: Meta {
                name: "minimal".to_string(),
                ..Meta::default()
            },
            seed: 0x5EED,
            time: Time::default(),
            world: schema::WorldSpec {
                source: v2xw_world::WorldSourceSpec::procedural(
                    "world/source/procedural-grid",
                    serde_json::Value::Null,
                ),
                imported_at: String::new(),
                buildings: BuildingOptions::default(),
                terrain: TerrainOptions::default(),
                cache: None,
                // A procedural world needs no jurisdiction; only an OSM import does.
                highway_preset: None,
            },
            actors: Actors::default(),
            weather: Weather::default(),
            radio: Radio::default(),
            net: Net::default(),
            messages: Messages::default(),
            security: Security::default(),
            nodes: Nodes::default(),
            threats: Threats::default(),
            detection: Detection::default(),
            metrics: Vec::new(),
            exporters: Vec::new(),
            events: Vec::new(),
            experiment: None,
        }
    }
}

/// Parses YAML or JSON into an untyped document.
fn parse_document(text: &str, path: &str) -> Result<Value> {
    serde_yml::from_str(text).map_err(|e| {
        EngineError::Scenario(ScenarioError::Parse {
            path: path.to_string(),
            message: e.to_string(),
        })
    })
}

/// Reads, parses, migrates and base-merges one file.
fn load_document(path: &Path, depth: usize) -> Result<Value> {
    let text = std::fs::read_to_string(path).map_err(|source| EngineError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut doc = parse_document(&text, &path.display().to_string())?;
    Chain::shipped()
        .migrate(&mut doc)
        .map_err(EngineError::from)?;
    resolve_bases(doc, path.parent(), depth)
}

/// Follows `meta.base` until there is none, merging each base underneath.
fn resolve_bases(mut doc: Value, base_dir: Option<&Path>, depth: usize) -> Result<Value> {
    let Some(base_ref) = merge::take_base(&mut doc) else {
        return Ok(doc);
    };
    if depth >= MAX_BASE_DEPTH {
        return Err(EngineError::Scenario(ScenarioError::Base {
            base: base_ref,
            why: format!(
                "the chain of meta.base references is more than {MAX_BASE_DEPTH} deep, which \
                 is what a cycle looks like from here"
            ),
        }));
    }
    let resolved = resolve_base_path(&base_ref, base_dir)?;
    let base_doc = load_document(&resolved, depth + 1)?;
    Ok(merge::merge(&base_doc, &doc))
}

/// A base reference is a path, relative to the including file's directory.
fn resolve_base_path(base: &str, base_dir: Option<&Path>) -> Result<PathBuf> {
    let candidate = Path::new(base);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        match base_dir {
            Some(dir) => dir.join(candidate),
            None => candidate.to_path_buf(),
        }
    };
    if resolved.is_file() {
        Ok(resolved)
    } else {
        Err(EngineError::Scenario(ScenarioError::Base {
            base: base.to_string(),
            why: format!(
                "no file at {}; a base is a path relative to the including scenario's \
                 directory",
                resolved.display()
            ),
        }))
    }
}
