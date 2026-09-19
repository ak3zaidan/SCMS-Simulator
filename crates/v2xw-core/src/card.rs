//! Model cards: the mandatory description every plug-in ships with.
//!
//! A model without a card cannot be registered (ADR 0007 §2, 03-interfaces.md §12). The
//! card says what the model computes, with which equations, from which parameters (each
//! with a unit, a default and a *source*), what it assumes, what it leaves out relative
//! to the next tier up, how it was validated, and whether it draws random numbers. The
//! documentation site is generated from cards, never hand-written (ADR 0007 §3).
//!
//! These types are the Rust form of the `model-card-1.json` schema. They round-trip
//! through YAML (the authoring format) and JSON (the manifest and API format); the field
//! names match the schema exactly, including `ref` (spelled [`Source::reference`] in Rust
//! because `ref` is a keyword) and the kebab-case enum values.

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Fidelity tier. A card declares the tiers its model implements; the scenario picks one
/// tier per family (03-interfaces.md conventions, 02-architecture.md §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Cheapest: closed-form or probabilistic stand-ins, for 10,000-node runs.
    Abstract,
    /// The default: the models the literature agrees on, without frame-level detail.
    Medium,
    /// Frame-level / physically detailed, for a focus region or a small scenario.
    High,
}

impl core::fmt::Display for Tier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Tier::Abstract => "abstract",
            Tier::Medium => "medium",
            Tier::High => "high",
        })
    }
}

/// The plug-in family a model belongs to: one seam of the architecture.
///
/// Closed on purpose. Adding a family means adding a trait, a variant here, a
/// conformance suite and a Python base class (ADR 0007 Consequences).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    /// World import and construction.
    World,
    /// Vehicle mobility.
    Mobility,
    /// Vulnerable-road-user mobility.
    Vru,
    /// Weather state and its driving effects.
    Weather,
    /// GNSS position/time belief.
    Gnss,
    /// Node clock behaviour.
    Clock,
    /// Large-scale propagation loss.
    Propagation,
    /// Small-scale fading.
    Fading,
    /// Line-of-sight and obstruction geometry.
    Obstacle,
    /// Physical layer.
    Phy,
    /// Medium access control.
    Mac,
    /// Decentralised congestion control.
    Dcc,
    /// Network layer (WSMP, GeoNetworking/BTP).
    Net,
    /// Fragmentation and reassembly.
    Fragmenter,
    /// RSU backhaul links.
    Backhaul,
    /// Cellular Uu access.
    Cellular,
    /// Backend network between infrastructure entities.
    BackendNet,
    /// Message encoding (UPER/COER or a size model).
    Codec,
    /// Message generation rules (CAM/BSM triggering, DENM, CPM).
    Generator,
    /// Security envelope (IEEE 1609.2, ETSI TS 103 097).
    Envelope,
    /// Cryptographic primitive cost/behaviour model.
    Primitive,
    /// Real cryptographic backend.
    CryptoBackend,
    /// Verification policy (verify-all, on-demand, prioritised).
    VerificationPolicy,
    /// Safety application (FCW, IMA, …).
    SafetyApp,
    /// Credential-management protocol (SCMS, ETSI, experimental).
    Protocol,
    /// Backend service model (queueing, capacity, availability).
    ServiceModel,
    /// Node hardware profile (CPU, HSM, memory, storage).
    HardwareProfile,
    /// Perception / sensor model.
    Perception,
    /// Attacker behaviour.
    Attacker,
    /// Local misbehaviour detector.
    Detector,
    /// Misbehaviour-authority pipeline.
    MaPipeline,
    /// Response/enforcement model (revocation, CRL distribution).
    Responder,
    /// Metric definition.
    Metric,
    /// Exporter.
    Exporter,
}

impl core::fmt::Display for Family {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The serde representation is the canonical spelling; reuse it.
        let json = serde_json::to_string(self).unwrap_or_else(|_| "\"?\"".to_string());
        f.write_str(json.trim_matches('"'))
    }
}

/// Where a number or a claim comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// A standard and clause (ETSI, IEEE, SAE, ISO).
    Standard,
    /// A paper, by DOI or URL.
    Paper,
    /// A vendor datasheet.
    Datasheet,
    /// A published dataset.
    Dataset,
    /// Another implementation's source code.
    Code,
    /// **Not yet cited**: a value chosen by the implementer that still needs
    /// calibration. Requires a `calibration` plan on the parameter (rule R1) and lands
    /// on the generated "todo-calibrate" page.
    TodoCalibrate,
}

/// A citation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// What kind of source this is.
    pub kind: SourceKind,
    /// The reference itself: "ETSI EN 302 637-2 §6.1.3", a DOI, a URL, a datasheet name.
    ///
    /// Serialised as `ref`, which is the schema's field name and a Rust keyword.
    #[serde(rename = "ref")]
    pub reference: String,
    /// The date the reference was accessed, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessed: Option<String>,
    /// Anything a reader needs to know about how the reference was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Source {
    /// A citation of `kind` with the given reference and no date or note.
    pub fn new(kind: SourceKind, reference: impl Into<String>) -> Self {
        Self {
            kind,
            reference: reference.into(),
            accessed: None,
            note: None,
        }
    }

    /// An uncited value that still needs calibration (rule R1 applies).
    pub fn todo_calibrate(what: impl Into<String>) -> Self {
        Self::new(SourceKind::TodoCalibrate, what)
    }
}

/// One equation the model implements, so a reader can check the code against the maths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Equation {
    /// Short name, e.g. "path loss".
    pub name: String,
    /// The equation, in LaTeX or in plain text.
    pub latex_or_text: String,
    /// Anything that qualifies it: ranges of validity, units, conventions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Equation {
    /// An equation with no notes.
    pub fn new(name: impl Into<String>, latex_or_text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            latex_or_text: latex_or_text.into(),
            notes: None,
        }
    }
}

/// One tunable number the model reads.
///
/// Invariant I-C3: every numeric parameter a plug-in reads at runtime must appear here,
/// or the registry rejects the model and the conformance tracer fails it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    /// Parameter name as the scenario spells it.
    pub name: String,
    /// Unit, e.g. `dB`, `m`, `s`, `Hz`, `-` for dimensionless.
    pub unit: String,
    /// Default value, of whatever JSON type the parameter has.
    pub default: serde_json::Value,
    /// Optional `[min, max]` (or a longer list of allowed values).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Vec<serde_json::Value>>,
    /// Where the default comes from.
    pub source: Source,
    /// How this value is to be calibrated. **Required** when `source.kind` is
    /// [`SourceKind::TodoCalibrate`] (registry rule R1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<String>,
}

impl Parameter {
    /// A parameter with a cited default and no range.
    pub fn new(
        name: impl Into<String>,
        unit: impl Into<String>,
        default: serde_json::Value,
        source: Source,
    ) -> Self {
        Self {
            name: name.into(),
            unit: unit.into(),
            default,
            range: None,
            source,
            calibration: None,
        }
    }
}

/// How thoroughly the model has been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationStatus {
    /// Nothing beyond "it compiles".
    Unvalidated,
    /// Unit tests cover its behaviour.
    UnitTested,
    /// Its outputs were compared against published figures or tables.
    LiteratureChecked,
    /// Its outputs were compared against measurements.
    FieldChecked,
}

/// The card's validation section.
///
/// A scenario that uses an [`ValidationStatus::Unvalidated`] model at the `high` tier
/// gets a validator warning that is written to the manifest (registry rule R2, enforced
/// by the scenario validator rather than here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validation {
    /// How far validation got.
    pub status: ValidationStatus,
    /// What it was validated against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Source>,
    /// Test names or ids that perform the check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
}

impl Validation {
    /// A validation section with a status and nothing else.
    pub fn new(status: ValidationStatus) -> Self {
        Self {
            status,
            references: Vec::new(),
            tests: Vec::new(),
        }
    }
}

impl Default for Validation {
    fn default() -> Self {
        Self::new(ValidationStatus::Unvalidated)
    }
}

/// The card's determinism declaration: which RNG domains the model draws from.
///
/// The names are [`crate::rng::RngDomain`] spellings; the conformance kit checks that a
/// model draws only from the domains it declares.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Determinism {
    /// Whether the model draws random numbers at all.
    #[serde(default)]
    pub uses_rng: bool,
    /// The RNG domains it draws from, e.g. `["fading"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rng_domains: Vec<String>,
}

/// Optional computational cost class, used by the scheduler and by the tier planner to
/// predict a run's cost (03-interfaces.md §12).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CostClass {
    /// Estimated cost of one call, microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_call_us: Option<f64>,
    /// How that estimate was obtained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Errors from [`ModelCard::validate`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CardError {
    /// The id does not match `^[a-z0-9][a-z0-9-]*(/[a-z0-9-]+)*$`.
    #[error(
        "model id {id:?} must match ^[a-z0-9][a-z0-9-]*(/[a-z0-9-]+)*$ \
         (lower-case, digits and hyphens, slash-separated segments)"
    )]
    InvalidId {
        /// The offending id.
        id: String,
    },
    /// Registry rule R1: a `todo-calibrate` source needs a calibration plan.
    #[error(
        "parameter {parameter:?} of {id:?} has source kind 'todo-calibrate' but no \
         calibration plan (registry rule R1)"
    )]
    MissingCalibration {
        /// The card's id.
        id: String,
        /// The parameter without a plan.
        parameter: String,
    },
    /// Two parameters share a name, so a scenario could not address them separately.
    #[error("model {id:?} declares parameter {parameter:?} more than once")]
    DuplicateParameter {
        /// The card's id.
        id: String,
        /// The repeated name.
        parameter: String,
    },
    /// A parameter's declared default is outside the parameter's own declared range.
    ///
    /// The card contradicts itself, and the contradiction is invisible at run time: a
    /// scenario that does not mention the parameter — the common case — takes the default
    /// verbatim, so the model runs on a value its own card declares impossible while the
    /// range check on *overrides* gives the false assurance that this cannot happen.
    #[error(
        "model {id:?} declares parameter {parameter:?} with default {default}, \
         which is outside {range} the card itself declares for it"
    )]
    DefaultOutOfRange {
        /// The card's id.
        id: String,
        /// The self-contradicting parameter.
        parameter: String,
        /// The declared default, as JSON.
        default: String,
        /// The declared range, rendered the way the resolver's message renders it.
        range: String,
    },
    /// A required string field was empty.
    #[error("model {id:?} has an empty {field}")]
    EmptyField {
        /// The card's id.
        id: String,
        /// Which field.
        field: &'static str,
    },
    /// The card targets a plug-in API version this engine build does not implement.
    #[error(
        "model {id:?} targets plug-in API {card:?} but this engine implements {engine:?} \
         (majors must match; for major 0 the minors must match too)"
    )]
    ApiVersionMismatch {
        /// The card's id.
        id: String,
        /// The `api_version` the card declares.
        card: String,
        /// The engine's [`API_VERSION`].
        engine: &'static str,
    },
}

/// The model card itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCard {
    /// Stable model id, e.g. `radio/propagation/log-distance-shadowing`. Matches
    /// `^[a-z0-9][a-z0-9-]*(/[a-z0-9-]+)*$`.
    pub id: String,
    /// The seam this model plugs into.
    pub family: Family,
    /// The model's own version (semver).
    pub version: String,
    /// The plug-in API version the card targets (semver).
    pub api_version: String,
    /// The tiers this model implements.
    pub tier: Vec<Tier>,
    /// What the model is for, in prose.
    pub purpose: String,
    /// The equations it implements.
    pub equations: Vec<Equation>,
    /// Every parameter it reads (invariant I-C3).
    pub parameters: Vec<Parameter>,
    /// What it assumes about the world or its inputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    /// Where it is known to be wrong or inapplicable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    /// What this tier leaves out relative to the next tier up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignores: Vec<String>,
    /// Citations for the model as a whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    /// How far it has been validated.
    #[serde(default)]
    pub validation: Validation,
    /// Its RNG usage.
    #[serde(default)]
    pub determinism: Determinism,
    /// Its computational cost class, if estimated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostClass>,
}

impl ModelCard {
    /// A minimal, valid card: id, family, version and purpose, everything else empty.
    ///
    /// Intended for tests and for `v2xw plugin new` scaffolding; a real model fills in
    /// equations, parameters and sources, and the registry's `todo-calibrate` report
    /// makes the gaps visible.
    ///
    /// `tier` defaults to `[Tier::Medium]` — the tier [`Tier::Medium`] itself documents as
    /// "the default" — because [`ModelCard::validate`] requires a non-empty `tier` (the
    /// schema lists it as required) and this constructor's contract is that what it returns
    /// validates. A model that implements another tier, or several, overwrites the field.
    pub fn new(
        id: impl Into<String>,
        family: Family,
        version: impl Into<String>,
        purpose: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            family,
            version: version.into(),
            api_version: API_VERSION.to_string(),
            tier: vec![Tier::Medium],
            purpose: purpose.into(),
            equations: Vec::new(),
            parameters: Vec::new(),
            assumptions: Vec::new(),
            limitations: Vec::new(),
            ignores: Vec::new(),
            sources: Vec::new(),
            validation: Validation::default(),
            determinism: Determinism::default(),
            cost: None,
        }
    }

    /// Checks the card against the schema constraints this crate can enforce and against
    /// the registry rules of 03-interfaces.md §12.
    ///
    /// Enforced here:
    ///
    /// * the id matches `^[a-z0-9][a-z0-9-]*(/[a-z0-9-]+)*$`;
    /// * **R1** — every parameter whose source kind is
    ///   [`SourceKind::TodoCalibrate`] carries a non-empty `calibration` plan;
    /// * parameter names are unique;
    /// * every parameter's declared `default` satisfies its own declared `range`
    ///   ([`CardError::DefaultOutOfRange`]);
    /// * `version`, `api_version` and `purpose` are non-empty;
    /// * `tier` is non-empty.
    ///
    /// `tier` is required by the published schema (§12 lists it in `required`), and it is
    /// not decoration: the scenario selects one tier per family, so a card that declares
    /// none describes a model that no scenario can select and that
    /// [`ModelCard::implements_tier`] answers `false` for at every tier. Accepting such a
    /// card would let a plug-in register and then never be chosen, with no diagnostic
    /// anywhere — and rule R2 (a `high`-tier scenario using an unvalidated model warns)
    /// has nothing to test against. The empty list is rejected here rather than at scenario
    /// load so that the failure names the plug-in.
    ///
    /// The default-against-range check is here rather than in
    /// [`crate::registry::ParamSet::resolve`] because a scenario that never mentions the
    /// parameter never reaches the resolver's range check: it copies the default in
    /// verbatim. A card whose default contradicts its own range would therefore run — on
    /// the value the card declares impossible — in exactly the common case, and the check
    /// on overrides would be giving false assurance. Refusing it at registration means the
    /// failure names the plug-in.
    ///
    /// Rule R2 (a `high`-tier scenario using an unvalidated model warns) belongs to the
    /// scenario validator, because it depends on the scenario's tier choice, not on the
    /// card; rule R3 (docs are generated) is a build-system rule.
    pub fn validate(&self) -> core::result::Result<(), CardError> {
        if !is_valid_model_id(&self.id) {
            return Err(CardError::InvalidId {
                id: self.id.clone(),
            });
        }
        for (field, value) in [
            ("version", &self.version),
            ("api_version", &self.api_version),
            ("purpose", &self.purpose),
        ] {
            if value.trim().is_empty() {
                return Err(CardError::EmptyField {
                    id: self.id.clone(),
                    field,
                });
            }
        }
        if self.tier.is_empty() {
            return Err(CardError::EmptyField {
                id: self.id.clone(),
                field: "tier",
            });
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.parameters.len());
        for p in &self.parameters {
            if seen.contains(&p.name.as_str()) {
                return Err(CardError::DuplicateParameter {
                    id: self.id.clone(),
                    parameter: p.name.clone(),
                });
            }
            seen.push(p.name.as_str());
            if let Some(range) = &p.range
                && !value_in_range(&p.default, range)
            {
                return Err(CardError::DefaultOutOfRange {
                    id: self.id.clone(),
                    parameter: p.name.clone(),
                    default: p.default.to_string(),
                    range: describe_range(range),
                });
            }
            if p.source.kind == SourceKind::TodoCalibrate
                && p.calibration
                    .as_ref()
                    .map(|c| c.trim().is_empty())
                    .unwrap_or(true)
            {
                return Err(CardError::MissingCalibration {
                    id: self.id.clone(),
                    parameter: p.name.clone(),
                });
            }
        }
        Ok(())
    }

    /// Every parameter that still needs calibration (rule R1's report, ADR 0007 §3).
    pub fn todo_calibrate(&self) -> impl Iterator<Item = &Parameter> {
        self.parameters
            .iter()
            .filter(|p| p.source.kind == SourceKind::TodoCalibrate)
    }

    /// True if the card declares `tier`.
    pub fn implements_tier(&self, tier: Tier) -> bool {
        self.tier.contains(&tier)
    }

    /// `id@version`, the form the manifest and scenario references use.
    pub fn id_at_version(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    /// Parses a card from YAML (the authoring format).
    pub fn from_yaml(s: &str) -> Result<Self> {
        Ok(serde_yml::from_str(s)?)
    }

    /// Serialises the card to YAML.
    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yml::to_string(self)?)
    }

    /// Parses a card from JSON.
    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// Serialises the card to JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Serialises the card to indented JSON, for files a human will read.
    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// The canonical bytes of the card: compact JSON with map keys in sorted order at
    /// every level of nesting ([`crate::hash::canonical_json`]).
    ///
    /// This is what [`crate::registry::Registry`] hashes for a card's content hash and
    /// what the manifest pins, so it must not depend on formatting, on field declaration
    /// order, or on which features happen to be enabled in the dependency graph.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        crate::hash::canonical_json(self)
    }

    /// Checks that this card targets an API version this engine build implements.
    ///
    /// Semver: the majors must match. Major `0` is unstable by convention, so there the
    /// minors must match too — a `0.3` card is refused by a `0.4` engine. A card whose
    /// `api_version` is not `major.minor[.patch]` is refused outright.
    ///
    /// [`Registry::register_hosted`] calls this, so a stale or future plug-in cannot be
    /// loaded into an engine whose interfaces it does not implement.
    ///
    /// [`Registry::register_hosted`]: crate::registry::Registry
    pub fn check_api_version(&self) -> core::result::Result<(), CardError> {
        let mismatch = || CardError::ApiVersionMismatch {
            id: self.id.clone(),
            card: self.api_version.clone(),
            engine: API_VERSION,
        };
        let card = parse_major_minor(&self.api_version).ok_or_else(mismatch)?;
        let engine = parse_major_minor(API_VERSION).expect("API_VERSION is valid semver");
        let compatible = if engine.0 == 0 {
            card == engine
        } else {
            card.0 == engine.0
        };
        if compatible { Ok(()) } else { Err(mismatch()) }
    }
}

/// The plug-in API version this build of the core implements.
///
/// A card declares the API version it targets and [`ModelCard::check_api_version`] compares
/// it against this one — majors must match, and for major `0` the minors must match too.
pub const API_VERSION: &str = "1.0.0";

/// Parses the `major.minor` prefix of a semver string, rejecting anything else.
fn parse_major_minor(v: &str) -> Option<(u64, u64)> {
    let mut parts = v.trim().split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    // A patch component is allowed and ignored; anything after it is not a version.
    match parts.next() {
        Some(patch) if patch.parse::<u64>().is_err() => return None,
        _ => {}
    }
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor))
}

/// True if `id` matches `^[a-z0-9][a-z0-9-]*(/[a-z0-9-]+)*$`.
///
/// Hand-written rather than pulled from a regex crate: the pattern is fixed, the check is
/// on a hot-ish registration path, and a dependency for one pattern is not worth it.
fn is_valid_model_id(id: &str) -> bool {
    let mut segments = id.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    let mut chars = first.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    if !chars.all(is_id_char) {
        return false;
    }
    for seg in segments {
        if seg.is_empty() || !seg.chars().all(is_id_char) {
            return false;
        }
    }
    true
}

/// True for the characters allowed inside a model-id segment.
fn is_id_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

/// True if `value` satisfies a card's declared `range`.
///
/// Two numeric entries and a numeric value mean the inclusive interval `[min, max]`;
/// anything else means the set of allowed values. See
/// [`crate::registry::ParamSet::resolve`] for the rule and why it is written down rather
/// than guessed at.
///
/// An empty range allows nothing, which is the honest reading of "the allowed values are
/// none of them" and shows up as a rejection with a diagnostic rather than as a range that
/// silently does nothing.
///
/// It lives here, beside [`ModelCard::validate`], because two stages need it: the card's
/// own default is checked against the range at registration
/// ([`CardError::DefaultOutOfRange`]) and a scenario's override is checked against it at
/// resolution ([`crate::registry::RegistryError::ParameterRange`]). One function, so the
/// two cannot disagree about what the range means.
///
/// # Numbers are compared numerically
///
/// `1` and `1.0` are the same number. [`crate::registry::ParamSet::resolve`]'s *type* check
/// is deliberately lenient about the two spellings ("JSON, YAML and every authoring format
/// spell `2` and `2.0` interchangeably"), and this check has to be lenient in the same way
/// or the pair contradicts itself: `serde_json::Value`'s `PartialEq` distinguishes
/// `Number::PosInt(1)` from `Number::Float(1.0)`, so a range declared `[0.1, 0.5, 1.0]` used
/// to accept `{"alpha": 1.0}` and reject `{"alpha": 1}` — with a message that listed `1.0`
/// among the allowed values it had just refused `1` for. YAML authoring produces the integer
/// spelling routinely.
///
/// Integers are compared as integers first, so two `u64`s beyond `2^53` that share an `f64`
/// do not compare equal. Non-numeric entries fall back to `Value` equality, which is what a
/// string or boolean enumeration wants.
pub(crate) fn value_in_range(value: &serde_json::Value, range: &[serde_json::Value]) -> bool {
    if let Some((min, max)) = numeric_interval(range) {
        return match value.as_f64() {
            Some(x) => x >= min && x <= max,
            // A non-numeric value against a numeric interval: the type check has already
            // passed, so the declared default was non-numeric too and the interval reading
            // does not apply; fall through to set membership.
            None => range.iter().any(|entry| json_values_equal(value, entry)),
        };
    }
    range.iter().any(|entry| json_values_equal(value, entry))
}

/// Value equality for a set-membership range, with numbers compared numerically.
fn json_values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Number(x), serde_json::Value::Number(y)) => {
            if let (Some(x), Some(y)) = (x.as_i64(), y.as_i64()) {
                return x == y;
            }
            if let (Some(x), Some(y)) = (x.as_u64(), y.as_u64()) {
                return x == y;
            }
            match (x.as_f64(), y.as_f64()) {
                (Some(x), Some(y)) => x == y,
                // `serde_json` only produces non-representable numbers with
                // `arbitrary_precision`, which this crate does not enable.
                _ => false,
            }
        }
        _ => a == b,
    }
}

/// `Some((min, max))` if `range` is exactly two numbers, in either order.
fn numeric_interval(range: &[serde_json::Value]) -> Option<(f64, f64)> {
    match range {
        [a, b] => {
            let (a, b) = (a.as_f64()?, b.as_f64()?);
            Some((a.min(b), a.max(b)))
        }
        _ => None,
    }
}

/// Renders a declared range for an error message, saying which reading was applied.
///
/// The reading is the one [`value_in_range`] applies, so the message names the rule that
/// actually rejected the value: "the range \[min, max\]" for a two-number interval, "the
/// allowed values \[…\]" for a set. Entries are printed as they were authored, because the
/// point of the message is to let the author see their own card.
pub(crate) fn describe_range(range: &[serde_json::Value]) -> String {
    match numeric_interval(range) {
        Some((min, max)) => format!("the range [{min}, {max}]"),
        None => {
            let items: Vec<String> = range.iter().map(|v| v.to_string()).collect();
            format!("the allowed values [{}]", items.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> ModelCard {
        let mut c = ModelCard::new(
            "radio/propagation/log-distance-shadowing",
            Family::Propagation,
            "1.2.0",
            "Log-distance path loss with log-normal shadowing.",
        );
        c.tier = vec![Tier::Abstract, Tier::Medium];
        c.equations.push(Equation::new(
            "path loss",
            "L(d) = L_0 + 10 n log10(d / d_0) + X_sigma",
        ));
        c.parameters.push(Parameter::new(
            "path_loss_exponent",
            "-",
            serde_json::json!(2.7),
            Source::new(SourceKind::Paper, "10.1109/TVT.2011.2158118"),
        ));
        c.parameters.push(Parameter::new(
            "shadowing_sigma_db",
            "dB",
            serde_json::json!(4.0),
            Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §5"),
        ));
        c.sources
            .push(Source::new(SourceKind::Standard, "ETSI TR 103 257-1"));
        c.validation = Validation::new(ValidationStatus::LiteratureChecked);
        c.determinism = Determinism {
            uses_rng: true,
            rng_domains: vec!["shadow".to_string()],
        };
        c
    }

    #[test]
    fn a_complete_card_validates() {
        let c = card();
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(
            c.id_at_version(),
            "radio/propagation/log-distance-shadowing@1.2.0"
        );
        assert!(c.implements_tier(Tier::Medium));
        assert!(!c.implements_tier(Tier::High));
        assert_eq!(c.todo_calibrate().count(), 0);
    }

    /// **A declared default must satisfy the parameter's own declared range.**
    ///
    /// Nothing checked it: not `validate`, not `Registry::register`, and not
    /// `ParamSet::resolve`, which range-checks *overrides* but copies defaults in verbatim.
    /// A card with `default: 40.0, range: [0.0, 10.0]` validated, registered, and resolved
    /// to 40.0 — so a scenario that does not mention the parameter, which is the common
    /// case, ran the model on a value the card itself declares impossible, while the check
    /// on overrides gave false assurance that this could not happen.
    #[test]
    fn a_default_outside_its_own_range_is_refused() {
        let mut c = card();
        let mut p = Parameter::new(
            "beta",
            "-",
            serde_json::json!(40.0),
            Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §5"),
        );
        p.range = Some(vec![serde_json::json!(0.0), serde_json::json!(10.0)]);
        c.parameters.push(p);

        assert_eq!(
            c.validate(),
            Err(CardError::DefaultOutOfRange {
                id: c.id.clone(),
                parameter: "beta".to_string(),
                default: "40.0".to_string(),
                range: "the range [0, 10]".to_string(),
            })
        );
        let msg = c.validate().unwrap_err().to_string();
        assert!(
            msg.contains("beta") && msg.contains("40.0") && msg.contains("[0, 10]"),
            "{msg}"
        );

        // A default inside the range validates, on the bounds included.
        for ok in [0.0, 4.0, 10.0] {
            c.parameters.last_mut().unwrap().default = serde_json::json!(ok);
            assert_eq!(c.validate(), Ok(()), "{ok} is inside [0, 10]");
        }

        // The same rule for a set-membership range — and the integer spelling of a listed
        // number satisfies it, exactly as an override's does.
        let mut c = card();
        let mut p = Parameter::new(
            "alpha",
            "-",
            serde_json::json!(0.75),
            Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §5"),
        );
        p.range = Some(vec![
            serde_json::json!(0.1),
            serde_json::json!(0.5),
            serde_json::json!(1.0),
        ]);
        c.parameters.push(p);
        assert!(matches!(
            c.validate(),
            Err(CardError::DefaultOutOfRange { ref parameter, .. }) if parameter == "alpha"
        ));
        c.parameters.last_mut().unwrap().default = serde_json::json!(1);
        assert_eq!(c.validate(), Ok(()), "1 is the integer spelling of 1.0");

        // A parameter with no declared range is unconstrained, as before.
        let mut c = card();
        c.parameters.push(Parameter::new(
            "unbounded",
            "-",
            serde_json::json!(1e9),
            Source::new(SourceKind::Code, "reference implementation"),
        ));
        assert_eq!(c.validate(), Ok(()));
    }

    /// The range helpers are shared by the card check and the resolver, so their reading of
    /// a declared range is one reading.
    #[test]
    fn the_range_reading_is_one_reading() {
        let interval = [serde_json::json!(0.5), serde_json::json!(10.0)];
        assert!(value_in_range(&serde_json::json!(0.5), &interval));
        assert!(value_in_range(&serde_json::json!(10), &interval));
        assert!(!value_in_range(&serde_json::json!(10.001), &interval));
        assert_eq!(describe_range(&interval), "the range [0.5, 10]");

        // Three entries are a set, not an interval — the published disambiguation.
        let set = [
            serde_json::json!(0.1),
            serde_json::json!(0.5),
            serde_json::json!(1.0),
        ];
        assert!(value_in_range(&serde_json::json!(1), &set));
        assert!(value_in_range(&serde_json::json!(1.0), &set));
        assert!(!value_in_range(&serde_json::json!(0.3), &set));
        assert_eq!(describe_range(&set), "the allowed values [0.1, 0.5, 1.0]");

        // A string set keeps exact equality: "1" is not 1.
        let words = [serde_json::json!("urban"), serde_json::json!("1")];
        assert!(value_in_range(&serde_json::json!("urban"), &words));
        assert!(!value_in_range(&serde_json::json!(1), &words));
        assert!(value_in_range(&serde_json::json!("1"), &words));

        // Large integers that share an `f64` are still distinct.
        let big = [
            serde_json::json!(9_007_199_254_740_993_u64),
            serde_json::json!("x"),
        ];
        assert!(value_in_range(
            &serde_json::json!(9_007_199_254_740_993_u64),
            &big
        ));
        assert!(!value_in_range(
            &serde_json::json!(9_007_199_254_740_992_u64),
            &big
        ));

        // An empty range allows nothing, and says so.
        assert!(!value_in_range(&serde_json::json!(1.0), &[]));
        assert_eq!(describe_range(&[]), "the allowed values []");
    }

    #[test]
    fn rule_r1_requires_a_calibration_plan() {
        let mut c = card();
        c.parameters.push(Parameter::new(
            "antenna_loss_db",
            "dB",
            serde_json::json!(1.0),
            Source::todo_calibrate("guessed from a photo of a roof mount"),
        ));
        assert_eq!(
            c.validate(),
            Err(CardError::MissingCalibration {
                id: c.id.clone(),
                parameter: "antenna_loss_db".to_string(),
            })
        );
        assert_eq!(c.todo_calibrate().count(), 1);

        // An empty plan is not a plan.
        c.parameters.last_mut().unwrap().calibration = Some("   ".to_string());
        assert!(c.validate().is_err());

        // With a plan it passes, and it still shows up on the todo-calibrate report.
        c.parameters.last_mut().unwrap().calibration =
            Some("Measure against the RSU datasheet in Phase 3.".to_string());
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(c.todo_calibrate().count(), 1);
    }

    #[test]
    fn id_pattern_is_enforced() {
        for good in [
            "a",
            "0",
            "mobility",
            "radio/propagation/log-distance-shadowing",
            "x/y-z/2",
            "a-b-c",
        ] {
            let mut c = card();
            c.id = good.to_string();
            assert_eq!(c.validate(), Ok(()), "should accept {good:?}");
        }
        for bad in [
            "",
            "-leading-hyphen",
            "/leading-slash",
            "Upper/Case",
            "has space",
            "trailing/",
            "double//slash",
            "under_score",
            "unicode/é",
        ] {
            let mut c = card();
            c.id = bad.to_string();
            assert_eq!(
                c.validate(),
                Err(CardError::InvalidId {
                    id: bad.to_string()
                }),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn duplicate_parameters_and_empty_fields_are_rejected() {
        let mut c = card();
        let p = c.parameters[0].clone();
        c.parameters.push(p);
        assert!(matches!(
            c.validate(),
            Err(CardError::DuplicateParameter { .. })
        ));

        let mut c = card();
        c.version = "  ".to_string();
        assert_eq!(
            c.validate(),
            Err(CardError::EmptyField {
                id: c.id.clone(),
                field: "version"
            })
        );
    }

    /// `tier` is `required` in the published schema (§12), and a card that declares no
    /// tier describes a model no scenario can ever select — `implements_tier` is `false`
    /// for every tier. It must not register.
    #[test]
    fn a_card_without_a_tier_is_rejected() {
        let mut c = card();
        c.tier.clear();
        assert_eq!(
            c.validate(),
            Err(CardError::EmptyField {
                id: c.id.clone(),
                field: "tier"
            })
        );
        assert!(!c.implements_tier(Tier::Abstract));
        assert!(!c.implements_tier(Tier::Medium));
        assert!(!c.implements_tier(Tier::High));

        // One tier is enough.
        c.tier = vec![Tier::High];
        assert_eq!(c.validate(), Ok(()));

        // And the minimal constructor still returns a card that validates, which is its
        // documented contract: it declares the default tier.
        let minimal = ModelCard::new("radio/minimal", Family::Propagation, "1.0.0", "purpose");
        assert_eq!(minimal.tier, vec![Tier::Medium]);
        assert_eq!(minimal.validate(), Ok(()));

        // A YAML card with an explicitly empty tier list is refused too, which is the path
        // a real scenario's plug-in takes.
        let yaml = "id: radio/no-tier\nfamily: fading\nversion: 1.0.0\napi_version: 1.0.0\n\
                    tier: []\npurpose: nothing\nequations: []\nparameters: []\n";
        let from_yaml = ModelCard::from_yaml(yaml).unwrap();
        assert_eq!(
            from_yaml.validate(),
            Err(CardError::EmptyField {
                id: "radio/no-tier".to_string(),
                field: "tier"
            })
        );
    }

    #[test]
    fn json_round_trip() {
        let c = card();
        let json = c.to_json().unwrap();
        assert_eq!(ModelCard::from_json(&json).unwrap(), c);
        assert!(c.to_json_pretty().unwrap().contains('\n'));
        // Field names follow the schema, including `ref`.
        assert!(json.contains("\"ref\":"));
        assert!(json.contains("\"family\":\"propagation\""));
        assert!(json.contains("\"tier\":[\"abstract\",\"medium\"]"));
        assert!(json.contains("\"status\":\"literature-checked\""));
    }

    #[test]
    fn yaml_round_trip() {
        let c = card();
        let yaml = c.to_yaml().unwrap();
        assert_eq!(ModelCard::from_yaml(&yaml).unwrap(), c);
    }

    #[test]
    fn yaml_matches_the_published_schema_spelling() {
        let yaml = r#"
id: radio/fading/nakagami
family: fading
version: 0.3.1
api_version: 1.0.0
tier: [medium, high]
purpose: Nakagami-m small-scale fading.
equations:
  - name: amplitude
    latex_or_text: "f(x) = 2 m^m x^{2m-1} / (Gamma(m) Omega^m) exp(-m x^2 / Omega)"
parameters:
  - name: m
    unit: "-"
    default: 3.0
    range: [0.5, 10.0]
    source: {kind: paper, ref: "10.1109/TVT.2007.905625", accessed: "2026-09-17"}
  - name: omega
    unit: "-"
    default: 1.0
    source: {kind: todo-calibrate, ref: "no measurement for this environment"}
    calibration: "Fit to the Berlin measurement set in Phase 4."
assumptions: ["flat fading over the frame"]
limitations: ["no temporal correlation"]
ignores: ["Doppler spectrum shape"]
sources:
  - {kind: standard, ref: "ETSI TR 103 257-1"}
validation: {status: unit-tested, tests: ["fading::nakagami_moments"]}
determinism: {uses_rng: true, rng_domains: [fading]}
cost: {per_call_us: 0.4}
"#;
        let c = ModelCard::from_yaml(yaml).unwrap();
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(c.family, Family::Fading);
        assert_eq!(c.tier, vec![Tier::Medium, Tier::High]);
        assert_eq!(c.parameters[0].source.kind, SourceKind::Paper);
        assert_eq!(
            c.parameters[0].source.accessed.as_deref(),
            Some("2026-09-17")
        );
        assert_eq!(c.parameters[1].source.kind, SourceKind::TodoCalibrate);
        assert_eq!(c.validation.status, ValidationStatus::UnitTested);
        assert!(c.determinism.uses_rng);
        assert_eq!(c.cost.as_ref().unwrap().per_call_us, Some(0.4));
        assert_eq!(c.todo_calibrate().count(), 1);
    }

    #[test]
    fn canonical_bytes_are_stable() {
        let c = card();
        let a = c.canonical_bytes().unwrap();
        let b = ModelCard::from_json(&c.to_json().unwrap())
            .unwrap()
            .canonical_bytes()
            .unwrap();
        assert_eq!(a, b);
        // Same content through YAML gives the same canonical bytes.
        let d = ModelCard::from_yaml(&c.to_yaml().unwrap())
            .unwrap()
            .canonical_bytes()
            .unwrap();
        assert_eq!(a, d);
    }

    /// The literal canonical bytes of a fixed card.
    ///
    /// Comparing canonical bytes against *other* canonical bytes (the test above) passes
    /// whatever the key order is. This pins the order itself, so that enabling
    /// `serde_json/preserve_order` anywhere in the workspace — a global, feature-unified
    /// decision any future dependency can make — fails here instead of silently changing
    /// every registry content hash and every manifest that pins one.
    #[test]
    fn canonical_bytes_are_sorted_at_every_level() {
        let mut c = ModelCard::new(
            "radio/fading/nakagami",
            Family::Fading,
            "0.3.1",
            "Nakagami-m small-scale fading.",
        );
        c.tier = vec![Tier::Medium];
        c.parameters.push(Parameter::new(
            "m",
            "-",
            serde_json::json!({ "zulu": 1, "alpha": 2 }),
            Source::new(SourceKind::Paper, "10.1109/TVT.2007.905625"),
        ));
        assert_eq!(
            String::from_utf8(c.canonical_bytes().unwrap()).unwrap(),
            concat!(
                r#"{"api_version":"1.0.0","determinism":{"uses_rng":false},"equations":[],"#,
                r#""family":"fading","id":"radio/fading/nakagami","#,
                r#""parameters":[{"default":{"alpha":2,"zulu":1},"name":"m","#,
                r#""source":{"kind":"paper","ref":"10.1109/TVT.2007.905625"},"unit":"-"}],"#,
                r#""purpose":"Nakagami-m small-scale fading.","#,
                r#""tier":["medium"],"validation":{"status":"unvalidated"},"version":"0.3.1"}"#,
            ),
        );
        // A nested parameter value is sorted too, not just the card's own fields…
        assert!(
            String::from_utf8(c.canonical_bytes().unwrap())
                .unwrap()
                .contains(r#"{"alpha":2,"zulu":1}"#)
        );
        // …unlike plain serialisation, which is in declaration order.
        assert!(c.to_json().unwrap().starts_with(r#"{"id":"#));
    }

    /// The gate the `API_VERSION` documentation promises: a card built for a different
    /// major version of the plug-in API cannot be registered.
    #[test]
    fn api_version_is_checked_against_the_engine() {
        let mut c = card();
        assert_eq!(c.api_version, API_VERSION);
        assert_eq!(c.check_api_version(), Ok(()));

        // A newer minor or patch of the same major is fine: the API is additive within it.
        for ok in ["1.0.0", "1.4", "1.9.3"] {
            c.api_version = ok.to_string();
            assert_eq!(c.check_api_version(), Ok(()), "should accept {ok:?}");
        }
        for bad in [
            "9.9.9",
            "0.9.0",
            "2.0.0",
            "",
            "one.zero",
            "1",
            "1.0.0-beta",
            "1.0.0.0",
        ] {
            c.api_version = bad.to_string();
            assert_eq!(
                c.check_api_version(),
                Err(CardError::ApiVersionMismatch {
                    id: c.id.clone(),
                    card: bad.to_string(),
                    engine: API_VERSION,
                }),
                "should reject {bad:?}"
            );
        }
    }

    /// Under semver, major 0 is unstable, so a 0.x engine must match the minor too.
    #[test]
    fn zero_major_versions_compare_the_minor() {
        assert_eq!(parse_major_minor("0.3.1"), Some((0, 3)));
        assert_eq!(parse_major_minor("12.7"), Some((12, 7)));
        assert_eq!(parse_major_minor(" 1.0.0 "), Some((1, 0)));
        assert_eq!(parse_major_minor("1"), None);
        assert_eq!(parse_major_minor("1.x"), None);
        assert_eq!(parse_major_minor("-1.0"), None);
        // The comparison rule itself, expressed independently of the current API_VERSION.
        let compatible = |engine: (u64, u64), card: (u64, u64)| {
            if engine.0 == 0 {
                card == engine
            } else {
                card.0 == engine.0
            }
        };
        assert!(compatible((0, 3), (0, 3)));
        assert!(!compatible((0, 3), (0, 4)));
        assert!(compatible((1, 0), (1, 9)));
        assert!(!compatible((1, 0), (2, 0)));
    }

    #[test]
    fn enum_spellings() {
        assert_eq!(Tier::Abstract.to_string(), "abstract");
        assert_eq!(Family::BackendNet.to_string(), "backend-net");
        assert_eq!(Family::MaPipeline.to_string(), "ma-pipeline");
        assert_eq!(
            serde_json::to_string(&SourceKind::TodoCalibrate).unwrap(),
            "\"todo-calibrate\""
        );
        assert_eq!(
            serde_json::to_string(&ValidationStatus::FieldChecked).unwrap(),
            "\"field-checked\""
        );
    }
}
