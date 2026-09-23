//! The dataset `manifest.json` — the legacy fields, and the provenance a datasheet needs.
//!
//! The legacy manifest is what makes a dataset reproducible without redistributing it, and
//! the frozen audit reads six of its keys: `config`, `counts`, `outputs`,
//! `data_digest_sha256`, `schema_versions` and `standards_profile`. Checks `I1` and `I2`
//! recompute every file's SHA-256 and the aggregate digest over them, so the digest rule
//! is part of the contract and is reproduced exactly:
//!
//! ```text
//! h = sha256()
//! for rel in sorted(data_files):
//!     h.update(rel.encode())
//!     h.update(sha256_hex(file(rel)).encode())
//! ```
//!
//! — the *path* and then the file's hex digest, as ASCII, in path order, with
//! `manifest.json` itself excluded so that writing the manifest cannot change the digest
//! the manifest carries.
//!
//! # No wall clock
//!
//! The legacy writer put `datetime.now(timezone.utc)` in `build_utc`. This crate may not
//! read a clock (the non-negotiable rule, and [`crate::lib`]'s third bullet), so
//! `build_utc` comes from the engine's own [`v2xw_core::Manifest`], which captured it once
//! at build time and carries it through the run. A dataset assembled without one says
//! `unknown` rather than inventing a time, and the datasheet says so too: a fabricated
//! timestamp in a provenance record is worse than an absent one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{Tier, ValidationStatus};

use super::bytes::{ByteProvenance, ByteProvenanceReport};
use super::tables::DatasetProfile;

/// One file the dataset wrote, with its digest — the manifest's `outputs` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    /// The path relative to the dataset root, with `/` separators on every platform.
    pub path: String,
    /// The file's SHA-256, lower-case hex.
    pub sha256: String,
}

/// The standards profile the legacy manifest recorded, and the audit's `standards_profile`
/// key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandardsProfile {
    /// How the misbehaviour reports are shaped.
    pub report: String,
    /// The certificate profile.
    pub cert: String,
    /// The linkage-value scheme.
    pub linkage: String,
}

impl Default for StandardsProfile {
    fn default() -> Self {
        StandardsProfile {
            report: "ETSI TS 103 759 (shape)".to_string(),
            cert: "IEEE 1609.2".to_string(),
            linkage: "CAMP SCP2".to_string(),
        }
    }
}

/// One model card that produced a number in a dataset, with the two things a consumer
/// needs beyond its name.
///
/// The legacy manifest carried `(id, version)`. That is enough to *identify* the model and
/// not enough to *use* it: a dataset is only as good as the validation status of the models
/// behind it, and 08-measurement-and-data.md §6 asks the datasheet to say what produced its
/// numbers. A reader who has to open the documentation site to find out whether the
/// propagation model behind a PDR column was ever compared to anything is a reader who will
/// not do it.
///
/// `content_hash` is the registry's own hash of the card's canonical bytes, so a replay
/// that used a different card with the same id and version is detectable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProvenance {
    /// The model's registry id.
    pub id: String,
    /// Its version.
    pub version: String,
    /// How far its validation got, in the card schema's own vocabulary.
    pub validation_status: ValidationStatus,
    /// The registry's content hash of the card, lower-case hex.
    pub content_hash: String,
    /// How many of its parameters are still `todo-calibrate`.
    ///
    /// A number, not a flag: a model with one uncited default is a different object from
    /// one with forty, and both are legitimately registered.
    pub todo_calibrate: u32,
    /// The tiers the card declares.
    pub tiers: Vec<String>,
}

impl ModelProvenance {
    /// Every registration in a registry, in id order — the list an engine hands the
    /// exporter.
    ///
    /// In id order rather than registration order, for the reason
    /// [`v2xw_core::registry::Registry::iter_by_id`] exists: the order two runs load their
    /// plug-ins in must not show up in anything the run produces.
    #[must_use]
    pub fn from_registry(registry: &v2xw_core::registry::Registry) -> Vec<Self> {
        registry
            .iter_by_id()
            .map(|(_, entry)| ModelProvenance {
                id: entry.card.id.clone(),
                version: entry.card.version.clone(),
                validation_status: entry.card.validation.status,
                content_hash: entry.content_hash_hex(),
                todo_calibrate: u32::try_from(entry.card.todo_calibrate().count())
                    .unwrap_or(u32::MAX),
                tiers: entry.card.tier.iter().map(Tier::to_string).collect(),
            })
            .collect()
    }

    /// True when the model was compared against something outside this repository.
    #[must_use]
    pub fn externally_checked(&self) -> bool {
        matches!(
            self.validation_status,
            ValidationStatus::LiteratureChecked | ValidationStatus::FieldChecked
        )
    }
}

/// The provenance an exporter is handed, rather than one it invents.
///
/// Every field here is something only the engine knows, and the exporter refuses to guess
/// any of it. The seed above all: a dataset whose seed is wrong is not reproducible and is
/// worse than one with no seed at all, because the claim is false rather than missing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RunProvenance {
    /// The master seed the run was driven by.
    pub master_seed: u64,
    /// The scenario's content hash.
    pub scenario_hash: String,
    /// The world's content hash.
    pub world_hash: String,
    /// The engine's version.
    pub engine_version: String,
    /// The commit the engine was built from.
    pub git_commit: String,
    /// The platform triple.
    pub platform: String,
    /// When the engine was built, from [`v2xw_core::Manifest`] — never from a clock read
    /// here. Empty means unknown, and the datasheet says `unknown`.
    pub build_utc: String,
    /// The recording's content digest, which is what ties the dataset to the run it came
    /// from.
    pub content_digest: String,
    /// Every model card that produced a number in this dataset, as `(id, version)`.
    ///
    /// The legacy key set. [`RunProvenance::models`] carries the same models with their
    /// validation status and card hash; an engine that fills it need not fill this one, and
    /// [`DatasetManifest::new`] derives this list from that one when this one is empty.
    pub model_cards: Vec<(String, String)>,
    /// The same models with their validation status, card hash and calibration debt.
    ///
    /// Empty means the engine did not supply it, and the datasheet says so rather than
    /// implying that no model was involved.
    #[serde(default)]
    pub models: Vec<ModelProvenance>,
    /// What produced each message type's bytes: a real encoder, or a size model.
    ///
    /// Keyed by the `msg_type` spelling `node.tx` carries. Declared by the engine, because
    /// only the layer that chose the codec knows; this crate joins the declaration against
    /// the recording and counts an undeclared type's bytes as neither real nor modelled
    /// ([`super::bytes`]).
    #[serde(default)]
    pub message_encodings: BTreeMap<String, ByteProvenance>,
    /// The scenario configuration, verbatim, so the run can be replayed from the manifest.
    pub config: BTreeMap<String, serde_json::Value>,
    /// The world bundle's licence, where the world came from licensed data
    /// (08-measurement-and-data.md §9). `None` for a procedural world.
    pub world_licence: Option<String>,
    /// The attribution strings the world's provenance requires (§9: OSM contributors,
    /// Overture, DLR/Airbus for Copernicus).
    pub world_attribution: Vec<String>,
}

/// The dataset manifest.
///
/// Field names are the legacy ones because the audit reads them by name, and
/// `dataset_version`, `generator`, `seed`, `config`, `schema_versions`,
/// `standards_profile`, `data_digest_sha256`, `outputs` and `counts` are exactly the keys
/// the legacy writer emitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetManifest {
    /// This exporter's version.
    pub dataset_version: String,
    /// When the *engine* was built. Not part of the data digest, and not a clock read.
    pub build_utc: String,
    /// Which engine wrote it. §6 says the changed value semantics are "documented in the
    /// datasheet's Generator line", and this is where that line comes from.
    pub generator: String,
    /// The master seed.
    pub seed: u64,
    /// The scenario configuration.
    pub config: BTreeMap<String, serde_json::Value>,
    /// `{"ma_visible": n, "ground_truth": n}`.
    pub schema_versions: BTreeMap<String, u32>,
    /// The standards profile.
    pub standards_profile: StandardsProfile,
    /// The aggregate digest over every data file.
    pub data_digest_sha256: String,
    /// Every data file and its digest, in path order.
    pub outputs: Vec<OutputFile>,
    /// The row counts the audit reconciles.
    pub counts: BTreeMap<String, u64>,
    /// The scenario's content hash. Beyond the legacy key set; a v1 consumer ignores it.
    pub scenario_hash: String,
    /// The world's content hash.
    pub world_hash: String,
    /// The recording's content digest.
    pub recording_content_digest: String,
    /// The engine's version and commit.
    pub engine: BTreeMap<String, String>,
    /// The model cards that produced the numbers, in the legacy `(id, version)` shape.
    pub model_cards: Vec<BTreeMap<String, String>>,
    /// The same models with their validation status, card hash and calibration debt.
    ///
    /// Beyond the legacy key set; a v1 consumer ignores it. Empty when the engine supplied
    /// none, which the datasheet reports rather than glossing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelProvenance>,
    /// Which of this dataset's byte counts came from real bytes and which from a size
    /// model.
    ///
    /// `None` when the writer was not given a tally — a dataset assembled by hand rather
    /// than from a recording. The datasheet distinguishes "no transmission was recorded"
    /// from "nobody told the writer", because the first is a fact about the run and the
    /// second is a gap in the exporter's inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_provenance: Option<ByteProvenanceReport>,
    /// The profile these tables are in.
    pub profile: DatasetProfile,
    /// The leakage linter's verdict over this dataset's node-visible files.
    pub leakage_lint: String,
    /// The world bundle's licence, where there is one (§9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world_licence: Option<String>,
    /// The attribution strings §9 requires.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub world_attribution: Vec<String>,
}

/// The exporter's own version, which the manifest's `dataset_version` carries.
pub const DATASET_VERSION: &str = "v2xw-record/ma-dataset/2.0.0";

/// The `generator` line. §6 requires the changed value semantics to be attributable, so
/// the string names the engine rather than claiming to be the legacy one.
pub const GENERATOR: &str = "v2xw-record ma-dataset exporter (v2xw engine: lane-level world, modeled radio, \
     real verification queues)";

impl DatasetManifest {
    /// Builds a manifest from the provenance, the counts, the digested files and the lint
    /// verdict.
    ///
    /// `outputs` must already be sorted by path and must **not** include `manifest.json`;
    /// [`aggregate_digest`] is computed over it here so the two can never disagree.
    #[must_use]
    pub fn new(
        profile: DatasetProfile,
        prov: &RunProvenance,
        counts: BTreeMap<String, u64>,
        outputs: Vec<OutputFile>,
        leakage_lint: String,
    ) -> Self {
        let (ma_visible, ground_truth) = profile.schema_versions();
        let mut schema_versions = BTreeMap::new();
        schema_versions.insert("ma_visible".to_string(), ma_visible);
        schema_versions.insert("ground_truth".to_string(), ground_truth);
        let mut engine = BTreeMap::new();
        engine.insert("version".to_string(), prov.engine_version.clone());
        engine.insert("git_commit".to_string(), prov.git_commit.clone());
        engine.insert("platform".to_string(), prov.platform.clone());
        DatasetManifest {
            dataset_version: DATASET_VERSION.to_string(),
            build_utc: if prov.build_utc.is_empty() {
                "unknown".to_string()
            } else {
                prov.build_utc.clone()
            },
            generator: GENERATOR.to_string(),
            seed: prov.master_seed,
            config: prov.config.clone(),
            schema_versions,
            standards_profile: StandardsProfile::default(),
            data_digest_sha256: aggregate_digest(&outputs),
            outputs,
            counts,
            scenario_hash: prov.scenario_hash.clone(),
            world_hash: prov.world_hash.clone(),
            recording_content_digest: prov.content_digest.clone(),
            engine,
            model_cards: if prov.model_cards.is_empty() {
                // Derived from the richer list rather than left empty: the legacy key is
                // what the frozen audit reads by name, and an engine that filled only the
                // new field should not silently lose it.
                prov.models
                    .iter()
                    .map(|m| {
                        let mut entry = BTreeMap::new();
                        entry.insert("id".to_string(), m.id.clone());
                        entry.insert("version".to_string(), m.version.clone());
                        entry
                    })
                    .collect()
            } else {
                prov.model_cards
                    .iter()
                    .map(|(id, version)| {
                        let mut m = BTreeMap::new();
                        m.insert("id".to_string(), id.clone());
                        m.insert("version".to_string(), version.clone());
                        m
                    })
                    .collect()
            },
            models: prov.models.clone(),
            byte_provenance: None,
            profile,
            leakage_lint,
            world_licence: prov.world_licence.clone(),
            world_attribution: prov.world_attribution.clone(),
        }
    }

    /// Attaches the byte-provenance verdict.
    ///
    /// A builder rather than a `new` argument because the verdict is a join between the
    /// dataset's own tally and the engine's codec declaration, and the writer is the only
    /// place that holds both. Taking it here also keeps the legacy constructor's signature,
    /// which every existing caller and test uses.
    #[must_use]
    pub fn with_byte_provenance(mut self, report: ByteProvenanceReport) -> Self {
        self.byte_provenance = Some(report);
        self
    }

    /// The manifest's bytes: `json.dumps(indent=2, sort_keys=True)` plus a trailing
    /// newline, which is what the legacy writer wrote and what a reviewer diffs.
    ///
    /// # Errors
    /// [`crate::RecordError::Json`] if the manifest will not serialise.
    pub fn to_bytes(&self) -> crate::Result<Vec<u8>> {
        let value = serde_json::to_value(self)?;
        let sorted = sort_keys(value);
        let mut out = serde_json::to_vec_pretty(&sorted)?;
        out.push(b'\n');
        Ok(out)
    }
}

/// Rebuilds a JSON value with every object's keys sorted, so the pretty-printed manifest
/// is reproducible.
fn sort_keys(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> =
                map.into_iter().map(|(k, v)| (k, sort_keys(v))).collect();
            let mut out = serde_json::Map::with_capacity(sorted.len());
            for (k, v) in sorted {
                out.insert(k, v);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_keys).collect())
        }
        scalar => scalar,
    }
}

/// The legacy aggregate data digest: SHA-256 over each path and each file's hex digest, in
/// path order.
///
/// Reproduced byte for byte from `_data_digest`, because the frozen audit's `I2` check
/// recomputes it and compares. The path goes in as its own bytes and the digest goes in as
/// its *hex text*, not as the 32 raw bytes — a detail that is easy to get wrong and that
/// changes the answer.
#[must_use]
pub fn aggregate_digest(outputs: &[OutputFile]) -> String {
    let mut sorted: Vec<&OutputFile> = outputs.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut buf = Vec::new();
    for f in sorted {
        buf.extend_from_slice(f.path.as_bytes());
        buf.extend_from_slice(f.sha256.as_bytes());
    }
    v2xw_core::hash::sha256_hex(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aggregate_digest_is_the_legacy_construction() {
        // Recomputed the way verify_data.py's I2 check recomputes it.
        let outputs = vec![
            OutputFile {
                path: "ma/ma_reports.jsonl".to_string(),
                sha256: "aa".repeat(32),
            },
            OutputFile {
                path: "ground_truth/gt_vehicle.jsonl".to_string(),
                sha256: "bb".repeat(32),
            },
        ];
        let mut buf = Vec::new();
        // path order, not insertion order
        buf.extend_from_slice(b"ground_truth/gt_vehicle.jsonl");
        buf.extend_from_slice("bb".repeat(32).as_bytes());
        buf.extend_from_slice(b"ma/ma_reports.jsonl");
        buf.extend_from_slice("aa".repeat(32).as_bytes());
        assert_eq!(
            aggregate_digest(&outputs),
            v2xw_core::hash::sha256_hex(&buf)
        );
    }

    #[test]
    fn the_digest_does_not_depend_on_the_order_the_files_were_written_in() {
        let a = OutputFile {
            path: "a".to_string(),
            sha256: "11".repeat(32),
        };
        let b = OutputFile {
            path: "b".to_string(),
            sha256: "22".repeat(32),
        };
        assert_eq!(
            aggregate_digest(&[a.clone(), b.clone()]),
            aggregate_digest(&[b, a])
        );
    }

    #[test]
    fn an_absent_build_time_is_unknown_rather_than_invented() {
        // The rule this enforces: no wall-clock read in this crate, and no fabricated
        // provenance either.
        let m = DatasetManifest::new(
            DatasetProfile::V1,
            &RunProvenance::default(),
            BTreeMap::new(),
            Vec::new(),
            "PASS".to_string(),
        );
        assert_eq!(m.build_utc, "unknown");
    }

    #[test]
    fn the_schema_versions_are_the_ones_section_six_declares() {
        let v1 = DatasetManifest::new(
            DatasetProfile::V1,
            &RunProvenance::default(),
            BTreeMap::new(),
            Vec::new(),
            String::new(),
        );
        assert_eq!(v1.schema_versions["ma_visible"], 1);
        assert_eq!(v1.schema_versions["ground_truth"], 1);
        let v2 = DatasetManifest::new(
            DatasetProfile::V2,
            &RunProvenance::default(),
            BTreeMap::new(),
            Vec::new(),
            String::new(),
        );
        assert_eq!(v2.schema_versions["ma_visible"], 2);
    }

    #[test]
    fn the_manifest_bytes_are_sorted_pretty_json_with_one_trailing_newline() {
        let m = DatasetManifest::new(
            DatasetProfile::V1,
            &RunProvenance::default(),
            BTreeMap::new(),
            Vec::new(),
            "PASS".to_string(),
        );
        let bytes = m.to_bytes().expect("bytes");
        let text = String::from_utf8(bytes).expect("utf-8");
        assert!(text.ends_with("}\n"));
        assert!(!text.ends_with("}\n\n"));
        let keys: Vec<&str> = text
            .lines()
            .filter_map(|l| l.trim().strip_prefix('"'))
            .filter_map(|l| l.split('"').next())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        // The top-level keys come out sorted; nested objects contribute their own sorted
        // runs, so this checks the first few rather than the whole flattened list.
        assert_eq!(keys.first(), sorted.first());
        assert!(text.contains("\"data_digest_sha256\""));
        assert!(text.contains("\"standards_profile\""));
    }

    #[test]
    fn the_generator_line_names_this_engine_rather_than_claiming_to_be_the_legacy_one() {
        // §6: the changed value semantics are "documented in the datasheet's Generator
        // line". A dataset that claimed to be legacy output would be a false provenance
        // claim, which is the one thing a datasheet exists to prevent.
        assert!(GENERATOR.contains("v2xw"));
        assert!(!GENERATOR.contains("scms_sim_ref"));
    }
}
