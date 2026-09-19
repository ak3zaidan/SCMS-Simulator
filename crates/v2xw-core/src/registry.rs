//! The model registry and the content-addressed parameter-set store.
//!
//! Every model that runs in a scenario is registered here first (ADR 0007 §1). A
//! registration is a [`ModelCard`] plus a licence tag plus the content hash of the card's
//! canonical bytes; the manifest freezes `id@version+content-hash` and a replay refuses a
//! different hash unless drift is explicitly allowed (ADR 0007 §4).
//!
//! The registry also enforces the licensing gate (ADR 0007 §5, 02-architecture.md §11):
//! a copyleft plug-in cannot be loaded **in process**; it must run out of process, where
//! the engine hashes its replies into the run digest instead of linking its code.
//!
//! Parameter sets are content-addressed ([`ParamSetStore`]) so that provenance can point
//! at a resolved parameter set by id rather than copying it for every value
//! (02-architecture.md §6.5).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::card::{CardError, ModelCard, Parameter, describe_range, value_in_range};
use crate::error::CoreError;
use crate::hash::{hex_encode, sha256};
use crate::model::ModelHandle;

/// A cheap handle to a registered model.
///
/// Dense index into the registry, assigned in registration order. Copied freely into
/// provenance records, which is why it is a `u32` and not a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelRef(
    /// Dense registration index.
    pub u32,
);

impl ModelRef {
    /// Creates a handle from a registration index.
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The registration index.
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl core::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "m{}", self.0)
    }
}

/// The licence a plug-in ships under.
///
/// The engine is Apache-2.0 and only links Apache-compatible code (ADR 0003); anything
/// copyleft has to run out of process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Licence {
    /// Apache-2.0.
    Apache2,
    /// MIT.
    Mit,
    /// BSD 2- or 3-clause.
    Bsd,
    /// MPL-2.0 (file-level copyleft; linkable).
    Mpl2,
    /// GPL-2.0 or GPL-3.0: out of process only.
    Gpl,
    /// LGPL: out of process only, conservatively.
    Lgpl,
    /// A licence the engine does not know: out of process only.
    Other(String),
    /// Not declared. Treated as in-process-safe for first-party models built into this
    /// repository, which inherit the engine's own licence.
    Unspecified,
}

impl Licence {
    /// Whether a plug-in under this licence may be linked into the engine process.
    pub fn allows_in_process(&self) -> bool {
        matches!(
            self,
            Licence::Apache2 | Licence::Mit | Licence::Bsd | Licence::Mpl2 | Licence::Unspecified
        )
    }
}

impl core::fmt::Display for Licence {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Licence::Apache2 => f.write_str("Apache-2.0"),
            Licence::Mit => f.write_str("MIT"),
            Licence::Bsd => f.write_str("BSD"),
            Licence::Mpl2 => f.write_str("MPL-2.0"),
            Licence::Gpl => f.write_str("GPL"),
            Licence::Lgpl => f.write_str("LGPL"),
            Licence::Other(s) => f.write_str(s),
            Licence::Unspecified => f.write_str("unspecified"),
        }
    }
}

/// How a registered model is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Hosting {
    /// Linked into the engine process (Rust trait object, or batched Python via PyO3).
    InProcess,
    /// A separate process reached over gRPC; its replies are hashed into the run digest.
    OutOfProcess,
}

/// One registration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisteredModel {
    /// The card as registered.
    pub card: ModelCard,
    /// The licence tag the manifest records.
    pub licence: Licence,
    /// Where the model runs.
    pub hosting: Hosting,
    /// SHA-256 of the card's canonical bytes ([`ModelCard::canonical_bytes`]).
    pub content_hash: [u8; 32],
}

impl RegisteredModel {
    /// The content hash as lower-case hex, the form the manifest carries.
    pub fn content_hash_hex(&self) -> String {
        hex_encode(&self.content_hash)
    }

    /// `id@version`, the scenario's way of naming a model.
    pub fn id_at_version(&self) -> String {
        self.card.id_at_version()
    }
}

/// Errors from registry operations.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// The card failed validation, so the model cannot be registered (ADR 0007 §2).
    #[error(transparent)]
    InvalidCard(#[from] CardError),
    /// A model with this id is already registered.
    #[error("model {id:?} is already registered (version {existing})")]
    Duplicate {
        /// The repeated id.
        id: String,
        /// The version already holding that id.
        existing: String,
    },
    /// The licence forbids linking this model into the engine process (ADR 0007 §5).
    #[error("model {id:?} is licensed {licence} and may only be loaded out of process")]
    LicenceRequiresOutOfProcess {
        /// The model id.
        id: String,
        /// Its licence.
        licence: Licence,
    },
    /// The card's canonical bytes could not be produced (a serialisation failure).
    #[error("model {id:?} could not be hashed: {message}")]
    Unhashable {
        /// The model id.
        id: String,
        /// The serialiser's message.
        message: String,
    },
    /// A scenario overrode a parameter the model's card does not declare
    /// ([`ParamSet::resolve`]).
    #[error(
        "model {id:?} has no parameter {parameter:?} to override \
         (it declares: {declared}); a scenario typo, or a card that is missing the \
         parameter it reads (invariant I-C3)"
    )]
    UnknownParameter {
        /// The card's id.
        id: String,
        /// The name the scenario used.
        parameter: String,
        /// The names the card does declare, comma-separated and in card order.
        declared: String,
    },
    /// An override's JSON type does not match the type of the card's declared default
    /// ([`ParamSet::resolve`]).
    #[error(
        "parameter {parameter:?} of {id:?} is declared {expected} but was overridden with {got}"
    )]
    ParameterType {
        /// The card's id.
        id: String,
        /// The parameter.
        parameter: String,
        /// The type of the declared default: `number`, `string`, `boolean`, `array`,
        /// `object` or `null`.
        expected: &'static str,
        /// The type the override had.
        got: &'static str,
    },
    /// An override fell outside the range the card declares for the parameter
    /// ([`ParamSet::resolve`]).
    #[error("parameter {parameter:?} of {id:?} was overridden with {value}, outside {range}")]
    ParameterRange {
        /// The card's id.
        id: String,
        /// The parameter.
        parameter: String,
        /// The offending value, as JSON.
        value: String,
        /// The declared range, as JSON, described as an interval or a set.
        range: String,
    },
    /// The overrides document handed to [`ParamSet::resolve`] was not a JSON object.
    #[error("overrides for {id:?} must be a JSON object of parameter names, got {got}")]
    BadOverrides {
        /// The card's id.
        id: String,
        /// The type that was supplied instead.
        got: &'static str,
    },
}

/// The set of models available to a run, in registration order.
///
/// A registration is always the metadata — card, licence, hosting, content hash — and
/// *optionally* the live implementation as well. Both paths exist because both are real:
///
/// * [`Registry::register`] and friends take a [`ModelCard`] alone. That is what a replay,
///   a manifest check or a documentation build needs: they compare and print cards, and
///   never call a model.
/// * [`Registry::register_model`] takes a [`ModelHandle`] — an `Arc<dyn Model + Send +
///   Sync>` — and registers its card *and* the object. That is what a run needs: the
///   engine resolves the scenario's model ids to handles and calls them.
///
/// The metadata of the two is identical, so a card registered either way produces the same
/// [`ModelRef`], the same content hash and the same manifest section;
/// [`Registry::get_model`] is simply `None` for one of them.
#[derive(Clone, Default)]
pub struct Registry {
    entries: Vec<RegisteredModel>,
    by_id: BTreeMap<String, ModelRef>,
    /// The live implementation of each entry, parallel to `entries`. `None` for a
    /// metadata-only registration. Kept beside `entries` rather than inside
    /// [`RegisteredModel`] because that type is `Serialize` and a trait object is not.
    impls: Vec<Option<ModelHandle>>,
}

impl core::fmt::Debug for Registry {
    /// Lists the registrations in id order and says which of them carry a live
    /// implementation. A trait object has no useful `Debug`, so the handle itself is
    /// rendered as `live` / `card-only`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut m = f.debug_map();
        for (r, e) in self.iter_by_id() {
            let kind = if self.get_model(r).is_some() {
                "live"
            } else {
                "card-only"
            };
            m.entry(
                &e.card.id_at_version(),
                &format_args!("{kind}, {:?}, {:?}", e.hosting, e.licence),
            );
        }
        m.finish()
    }
}

impl Registry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a first-party model, in process, with an undeclared licence.
    ///
    /// The card is validated first; an invalid card is rejected (ADR 0007 §2).
    pub fn register(&mut self, card: ModelCard) -> Result<ModelRef, RegistryError> {
        self.register_hosted(card, Licence::Unspecified, Hosting::InProcess)
    }

    /// Registers a model to run in process under a declared licence.
    ///
    /// Fails with [`RegistryError::LicenceRequiresOutOfProcess`] if the licence is
    /// copyleft or unknown.
    pub fn register_with_licence(
        &mut self,
        card: ModelCard,
        licence: Licence,
    ) -> Result<ModelRef, RegistryError> {
        self.register_hosted(card, licence, Hosting::InProcess)
    }

    /// Registers a model that runs in its own process, under any licence.
    pub fn register_out_of_process(
        &mut self,
        card: ModelCard,
        licence: Licence,
    ) -> Result<ModelRef, RegistryError> {
        self.register_hosted(card, licence, Hosting::OutOfProcess)
    }

    /// Registers a **live model**: its card *and* the object that implements it, in
    /// process, with an undeclared licence.
    ///
    /// The card comes from [`crate::model::Model::card`], so the registered metadata and
    /// the object can never disagree — which is the point of taking the implementation
    /// rather than a card the caller pairs with it by hand. Every check
    /// [`Registry::register`] makes (card validation, API version, duplicate id, licence
    /// gate) applies unchanged, and nothing is stored if any of them fails.
    ///
    /// The handle is retrievable with [`Registry::get_model`], so the engine can resolve a
    /// scenario's `id@version` to something it can call.
    pub fn register_model(&mut self, model: ModelHandle) -> Result<ModelRef, RegistryError> {
        self.register_model_hosted(model, Licence::Unspecified, Hosting::InProcess)
    }

    /// Registers a live model under a declared licence, to run in process.
    ///
    /// Fails with [`RegistryError::LicenceRequiresOutOfProcess`] if the licence is copyleft
    /// or unknown — a licence gate is about linking code into this process, and this is the
    /// call that links it.
    pub fn register_model_with_licence(
        &mut self,
        model: ModelHandle,
        licence: Licence,
    ) -> Result<ModelRef, RegistryError> {
        self.register_model_hosted(model, licence, Hosting::InProcess)
    }

    /// Registers a live model that stands in for one running out of process, under any
    /// licence.
    ///
    /// The object is the in-process *stub* — the gRPC client that forwards each call and
    /// hashes the replies into the run digest (ADR 0007 §5). The copyleft code itself is
    /// never linked, so the licence gate does not apply.
    pub fn register_model_out_of_process(
        &mut self,
        model: ModelHandle,
        licence: Licence,
    ) -> Result<ModelRef, RegistryError> {
        self.register_model_hosted(model, licence, Hosting::OutOfProcess)
    }

    /// The live-registration path all three model constructors funnel through.
    fn register_model_hosted(
        &mut self,
        model: ModelHandle,
        licence: Licence,
        hosting: Hosting,
    ) -> Result<ModelRef, RegistryError> {
        let card = crate::model::Model::card(&*model).clone();
        let r = self.register_hosted(card, licence, hosting)?;
        self.impls[r.index() as usize] = Some(model);
        Ok(r)
    }

    /// The live implementation behind a handle, or `None` if the registration is
    /// metadata-only (or the handle belongs to another registry).
    pub fn get_model(&self, r: ModelRef) -> Option<&ModelHandle> {
        self.impls.get(r.index() as usize)?.as_ref()
    }

    /// The live implementation registered under `id`, if there is one.
    pub fn get_model_by_id(&self, id: &str) -> Option<&ModelHandle> {
        self.get_model(self.resolve(id)?)
    }

    /// True if this registration carries a live implementation and not only a card.
    pub fn has_implementation(&self, r: ModelRef) -> bool {
        self.get_model(r).is_some()
    }

    /// Every live implementation, in id order, with the handle that names it.
    ///
    /// Id order rather than registration order, for the same reason
    /// [`Registry::iter_by_id`] exists: the order two runs load their plug-ins in must not
    /// show up in anything the run produces.
    pub fn iter_models(&self) -> impl Iterator<Item = (ModelRef, &ModelHandle)> {
        self.iter_by_id()
            .filter_map(|(r, _)| self.get_model(r).map(|m| (r, m)))
    }

    /// The registration path all three constructors funnel through.
    fn register_hosted(
        &mut self,
        card: ModelCard,
        licence: Licence,
        hosting: Hosting,
    ) -> Result<ModelRef, RegistryError> {
        card.validate()?;
        // The gate that stops a stale or future plug-in being loaded into an engine whose
        // interfaces it does not implement (ADR 0007 §4, `card::API_VERSION`).
        card.check_api_version()?;
        if let Some(existing) = self.by_id.get(&card.id) {
            return Err(RegistryError::Duplicate {
                id: card.id.clone(),
                existing: self.entries[existing.index() as usize].card.version.clone(),
            });
        }
        if hosting == Hosting::InProcess && !licence.allows_in_process() {
            return Err(RegistryError::LicenceRequiresOutOfProcess {
                id: card.id.clone(),
                licence,
            });
        }
        let bytes = card
            .canonical_bytes()
            .map_err(|e| RegistryError::Unhashable {
                id: card.id.clone(),
                message: e.to_string(),
            })?;
        let content_hash = sha256(&bytes);
        let r = ModelRef::new(self.entries.len() as u32);
        self.by_id.insert(card.id.clone(), r);
        self.entries.push(RegisteredModel {
            card,
            licence,
            hosting,
            content_hash,
        });
        self.impls.push(None);
        Ok(r)
    }

    /// The registration with this id, if any.
    pub fn get(&self, id: &str) -> Option<&RegisteredModel> {
        self.by_id
            .get(id)
            .map(|r| &self.entries[r.index() as usize])
    }

    /// The registration behind a handle, if the handle belongs to this registry.
    pub fn get_ref(&self, r: ModelRef) -> Option<&RegisteredModel> {
        self.entries.get(r.index() as usize)
    }

    /// The handle for an id, if registered.
    pub fn resolve(&self, id: &str) -> Option<ModelRef> {
        self.by_id.get(id).copied()
    }

    /// True if an id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Every registration, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = (ModelRef, &RegisteredModel)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| (ModelRef::new(i as u32), e))
    }

    /// Every registration, in id order — the order the manifest lists them in, so that
    /// two runs that load the same models in different orders still produce the same
    /// manifest section.
    pub fn iter_by_id(&self) -> impl Iterator<Item = (ModelRef, &RegisteredModel)> {
        self.by_id
            .values()
            .map(|r| (*r, &self.entries[r.index() as usize]))
    }

    /// Number of registered models.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every parameter in the registry that still needs calibration, in id order.
    ///
    /// This is the source of the generated "todo-calibrate" page (ADR 0007 §3): one
    /// place that lists every number in the simulator that nobody has justified yet.
    pub fn todo_calibrate_report(&self) -> Vec<(ModelRef, Parameter)> {
        let mut out = Vec::new();
        for (r, entry) in self.iter_by_id() {
            for p in entry.card.todo_calibrate() {
                out.push((r, p.clone()));
            }
        }
        out
    }

    /// `(id, version)` pairs for the manifest's `model_cards` field, in id order.
    pub fn model_card_versions(&self) -> Vec<(String, String)> {
        self.iter_by_id()
            .map(|(_, e)| (e.card.id.clone(), e.card.version.clone()))
            .collect()
    }
}

/// A cheap handle to a resolved parameter set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamSetId(
    /// Dense index into the [`ParamSetStore`].
    pub u32,
);

impl ParamSetId {
    /// Creates a handle from an index.
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The index.
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl core::fmt::Display for ParamSetId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "p{}", self.0)
    }
}

/// A resolved parameter set: a model's defaults merged with the scenario's overrides.
///
/// Values are JSON so that a parameter can be a number, a string, a list or a nested
/// object without this crate knowing any model's shape. The map is a `BTreeMap`, so the
/// serialisation is canonical and the content hash is stable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamSet {
    values: BTreeMap<String, serde_json::Value>,
}

impl ParamSet {
    /// An empty parameter set.
    pub fn new() -> Self {
        Self::default()
    }

    /// A parameter set from name/value pairs.
    pub fn from_iter_values(it: impl IntoIterator<Item = (String, serde_json::Value)>) -> Self {
        Self {
            values: it.into_iter().collect(),
        }
    }

    /// **Resolves** a parameter set: the model card's declared defaults with the scenario's
    /// overrides applied on top, checked against the card.
    ///
    /// This is the merge the type's own definition promises ("a model's defaults merged
    /// with the scenario's overrides") and the value [`crate::ctx::Ctx::params`] hands a
    /// plug-in. Every number a model reads comes from here, and invariant I-C3 says every
    /// one of them is declared on the card — so this function can, and does, check that.
    ///
    /// # What it checks, and why each check is an error rather than a shrug
    ///
    /// 1. **`overrides` must be a JSON object** (or `null`, meaning "no overrides"). A
    ///    list or a string where an object belongs is a malformed scenario.
    /// 2. **Every overridden name must be declared by the card.** This is the check that
    ///    earns its keep. `shadowing_sigma_db: 6.0` misspelled as `shadowing_sigma: 6.0`
    ///    would otherwise be accepted silently, the model would read its 4.0 default, and
    ///    the run would be *reproducible, well-formed and not the experiment that was
    ///    asked for*. Every sweep over that parameter would then produce identical results
    ///    and the only clue would be a flat line in a chart. A scenario typo is a scenario
    ///    error.
    /// 3. **The override's JSON type must match the declared default's.** `"2.7"` is not
    ///    `2.7`; a model that calls `get_f64` on it gets `None` and falls back to whatever
    ///    its code does when a declared parameter is missing. Integers and floats are one
    ///    type (`number`) here, because JSON and every authoring format spell `2` and `2.0`
    ///    interchangeably; `null` matches only a declared `null`.
    /// 4. **The declared range, if any, must hold.** The card's `range` is `[min, max]`
    ///    *exactly when* it has two numeric entries and the value is a number; any other
    ///    shape — three entries, strings, booleans — is the **set of allowed values** and
    ///    the override must equal one of them. That disambiguation is the published rule:
    ///    a card that wants a two-element enumeration of numbers must express it another
    ///    way. Bounds are inclusive.
    ///
    /// Parameters the scenario does not mention keep their declared default, so the result
    /// always has exactly the card's parameters, in name order, whatever the scenario said.
    /// That is what makes [`ParamSet::content_hash`] a meaningful content address: two runs
    /// that resolved the same card with the same overrides intern to one id even if one of
    /// them spelled the defaults out.
    ///
    /// The card itself is not re-validated here; registration does that
    /// ([`Registry::register`]).
    ///
    /// ```
    /// use v2xw_core::card::{Family, ModelCard, Parameter, Source, SourceKind};
    /// use v2xw_core::registry::ParamSet;
    ///
    /// let mut card = ModelCard::new("radio/x", Family::Propagation, "1.0.0", "example");
    /// card.parameters.push(Parameter::new(
    ///     "sigma_db", "dB", serde_json::json!(4.0),
    ///     Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §5"),
    /// ));
    ///
    /// let p = ParamSet::resolve(&card, &serde_json::json!({"sigma_db": 6.0})).unwrap();
    /// assert_eq!(p.get_f64("sigma_db"), Some(6.0));
    ///
    /// // A typo is an error, not a silent fallback to the default.
    /// assert!(ParamSet::resolve(&card, &serde_json::json!({"sigma": 6.0})).is_err());
    /// ```
    pub fn resolve(
        defaults: &ModelCard,
        overrides: &serde_json::Value,
    ) -> crate::error::Result<ParamSet> {
        let id = &defaults.id;
        let mut values: BTreeMap<String, serde_json::Value> = defaults
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.default.clone()))
            .collect();

        let map = match overrides {
            serde_json::Value::Null => return Ok(Self { values }),
            serde_json::Value::Object(map) => map,
            other => {
                return Err(CoreError::from(RegistryError::BadOverrides {
                    id: id.clone(),
                    got: json_type_name(other),
                }));
            }
        };

        for (name, value) in map {
            let Some(declared) = defaults.parameters.iter().find(|p| &p.name == name) else {
                return Err(CoreError::from(RegistryError::UnknownParameter {
                    id: id.clone(),
                    parameter: name.clone(),
                    declared: defaults
                        .parameters
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                }));
            };
            let expected = json_type_name(&declared.default);
            let got = json_type_name(value);
            if expected != got {
                return Err(CoreError::from(RegistryError::ParameterType {
                    id: id.clone(),
                    parameter: name.clone(),
                    expected,
                    got,
                }));
            }
            match &declared.range {
                Some(range) if !value_in_range(value, range) => {
                    return Err(CoreError::from(RegistryError::ParameterRange {
                        id: id.clone(),
                        parameter: name.clone(),
                        value: value.to_string(),
                        range: describe_range(range),
                    }));
                }
                _ => {}
            }
            values.insert(name.clone(), value.clone());
        }
        Ok(Self { values })
    }

    /// Inserts or replaces a value, returning the previous one.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        value: serde_json::Value,
    ) -> Option<serde_json::Value> {
        self.values.insert(name.into(), value)
    }

    /// The raw value of a parameter.
    pub fn get(&self, name: &str) -> Option<&serde_json::Value> {
        self.values.get(name)
    }

    /// A parameter as `f64`, if it is a number.
    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.values.get(name)?.as_f64()
    }

    /// A parameter as `u64`, if it is a non-negative integer.
    pub fn get_u64(&self, name: &str) -> Option<u64> {
        self.values.get(name)?.as_u64()
    }

    /// A parameter as `bool`.
    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.values.get(name)?.as_bool()
    }

    /// A parameter as `&str`.
    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.values.get(name)?.as_str()
    }

    /// Every `(name, value)` pair, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> {
        self.values.iter()
    }

    /// Number of parameters.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// True if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Canonical bytes: compact JSON with keys in sorted order at every level of nesting.
    ///
    /// The set's own keys are sorted because it is a `BTreeMap`, but a parameter's *value*
    /// may be a nested object whose key order would otherwise depend on how the value was
    /// built, so the whole thing goes through [`crate::hash::canonical_json`].
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // A map of JSON values always serialises successfully.
        crate::hash::canonical_json(&self.values).unwrap_or_default()
    }

    /// SHA-256 of [`ParamSet::canonical_bytes`]: the set's content address.
    pub fn content_hash(&self) -> [u8; 32] {
        sha256(&self.canonical_bytes())
    }
}

/// The JSON type of a value, as [`ParamSet::resolve`]'s type check names it.
///
/// Integers and floats are one type: JSON, YAML and every authoring format spell `2` and
/// `2.0` interchangeably, so refusing `2` for a parameter whose default is `2.0` would
/// reject correct scenarios.
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// A content-addressed store of parameter sets.
///
/// Interning the same values twice returns the same [`ParamSetId`], so provenance can
/// name a parameter set by a 4-byte id and the UI can resolve it back to the numbers that
/// produced a displayed value (02-architecture.md §6.5).
#[derive(Debug, Clone, Default)]
pub struct ParamSetStore {
    sets: Vec<ParamSet>,
    hashes: Vec<[u8; 32]>,
    /// Interning index. A `BTreeMap` rather than a `HashMap`: it is not iterated today,
    /// but a hash map's iteration order is unspecified and seeded per process, and this
    /// store feeds provenance records that are exported (02-architecture.md §6.5).
    by_hash: BTreeMap<[u8; 32], ParamSetId>,
}

impl ParamSetStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns a parameter set, returning its id. Identical content yields the id it got
    /// the first time.
    pub fn intern(&mut self, set: ParamSet) -> ParamSetId {
        let hash = set.content_hash();
        if let Some(id) = self.by_hash.get(&hash) {
            return *id;
        }
        let id = ParamSetId::new(self.sets.len() as u32);
        self.sets.push(set);
        self.hashes.push(hash);
        self.by_hash.insert(hash, id);
        id
    }

    /// The set behind an id.
    pub fn get(&self, id: ParamSetId) -> Option<&ParamSet> {
        self.sets.get(id.index() as usize)
    }

    /// The content hash behind an id.
    pub fn content_hash(&self, id: ParamSetId) -> Option<[u8; 32]> {
        self.hashes.get(id.index() as usize).copied()
    }

    /// The id of an already-interned set with this content hash.
    pub fn find(&self, content_hash: &[u8; 32]) -> Option<ParamSetId> {
        self.by_hash.get(content_hash).copied()
    }

    /// Every interned set, in interning order.
    pub fn iter(&self) -> impl Iterator<Item = (ParamSetId, &ParamSet)> {
        self.sets
            .iter()
            .enumerate()
            .map(|(i, s)| (ParamSetId::new(i as u32), s))
    }

    /// Number of distinct sets.
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    /// True if nothing has been interned.
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::{Family, Parameter, Source, SourceKind};

    fn card(id: &str) -> ModelCard {
        ModelCard::new(id, Family::Propagation, "1.0.0", "test model")
    }

    fn card_with_todo(id: &str, param: &str) -> ModelCard {
        let mut c = card(id);
        let mut p = Parameter::new(
            param,
            "dB",
            serde_json::json!(1.0),
            Source::todo_calibrate("nothing measured yet"),
        );
        p.calibration = Some("Measure in Phase 3.".to_string());
        c.parameters.push(p);
        c
    }

    #[test]
    fn register_and_look_up() {
        let mut r = Registry::new();
        assert!(r.is_empty());
        let a = r.register(card("radio/a")).unwrap();
        let b = r.register(card("radio/b")).unwrap();
        assert_eq!(a, ModelRef::new(0));
        assert_eq!(b, ModelRef::new(1));
        assert_eq!(r.len(), 2);
        assert!(r.contains("radio/a"));
        assert_eq!(r.resolve("radio/b"), Some(b));
        assert_eq!(r.get("radio/a").unwrap().card.id, "radio/a");
        assert_eq!(r.get_ref(b).unwrap().card.id, "radio/b");
        assert!(r.get("nope").is_none());
        assert!(r.get_ref(ModelRef::new(99)).is_none());
        assert_eq!(a.to_string(), "m0");
        assert_eq!(
            r.model_card_versions(),
            vec![
                ("radio/a".to_string(), "1.0.0".to_string()),
                ("radio/b".to_string(), "1.0.0".to_string())
            ]
        );
    }

    #[test]
    fn iteration_orders_are_as_documented() {
        let mut r = Registry::new();
        r.register(card("z/model")).unwrap();
        r.register(card("a/model")).unwrap();
        let registration: Vec<&str> = r.iter().map(|(_, e)| e.card.id.as_str()).collect();
        assert_eq!(registration, vec!["z/model", "a/model"]);
        let by_id: Vec<&str> = r.iter_by_id().map(|(_, e)| e.card.id.as_str()).collect();
        assert_eq!(by_id, vec!["a/model", "z/model"]);
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut r = Registry::new();
        r.register(card("radio/a")).unwrap();
        let mut second = card("radio/a");
        second.version = "2.0.0".to_string();
        assert_eq!(
            r.register(second),
            Err(RegistryError::Duplicate {
                id: "radio/a".to_string(),
                existing: "1.0.0".to_string()
            })
        );
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn invalid_cards_are_rejected() {
        let mut r = Registry::new();
        let bad = card("Radio/A");
        assert!(matches!(
            r.register(bad),
            Err(RegistryError::InvalidCard(CardError::InvalidId { .. }))
        ));
        assert!(r.is_empty());
    }

    /// A plug-in built against a different major version of the plug-in API cannot be
    /// loaded into this engine, whichever hosting it asks for (ADR 0007 §4).
    #[test]
    fn cards_for_another_api_version_are_refused() {
        let mut r = Registry::new();

        let mut future = card("radio/from-the-future");
        future.api_version = "9.9.9".to_string();
        assert_eq!(
            r.register(future),
            Err(RegistryError::InvalidCard(CardError::ApiVersionMismatch {
                id: "radio/from-the-future".to_string(),
                card: "9.9.9".to_string(),
                engine: crate::card::API_VERSION,
            }))
        );

        let mut stale = card("radio/from-the-past");
        stale.api_version = "0.4.0".to_string();
        assert!(matches!(
            r.register_out_of_process(stale, Licence::Gpl),
            Err(RegistryError::InvalidCard(
                CardError::ApiVersionMismatch { .. }
            ))
        ));

        let mut malformed = card("radio/nonsense");
        malformed.api_version = "latest".to_string();
        assert!(matches!(
            r.register_with_licence(malformed, Licence::Mit),
            Err(RegistryError::InvalidCard(
                CardError::ApiVersionMismatch { .. }
            ))
        ));

        assert!(r.is_empty(), "nothing incompatible was registered");

        // A later minor of the same major is additive, so it loads.
        let mut newer_minor = card("radio/newer-minor");
        newer_minor.api_version = "1.7.2".to_string();
        assert!(r.register(newer_minor).is_ok());
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn copyleft_cannot_be_loaded_in_process() {
        let mut r = Registry::new();
        let err = r
            .register_with_licence(card("mobility/sumo-bridge"), Licence::Gpl)
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::LicenceRequiresOutOfProcess { .. }
        ));
        // …but it may run out of process.
        let m = r
            .register_out_of_process(card("mobility/sumo-bridge"), Licence::Gpl)
            .unwrap();
        assert_eq!(r.get_ref(m).unwrap().hosting, Hosting::OutOfProcess);
        assert_eq!(r.get_ref(m).unwrap().licence, Licence::Gpl);
        // Permissive licences link fine.
        assert!(
            r.register_with_licence(card("radio/x"), Licence::Mit)
                .is_ok()
        );
        assert!(Licence::Apache2.allows_in_process());
        assert!(!Licence::Other("SSPL".to_string()).allows_in_process());
        assert_eq!(Licence::Apache2.to_string(), "Apache-2.0");
    }

    #[test]
    fn content_hash_follows_the_card() {
        let mut r = Registry::new();
        let a = r.register(card("radio/a")).unwrap();
        let mut other = card("radio/b");
        other.purpose = "a different purpose".to_string();
        let b = r.register(other).unwrap();
        let ha = r.get_ref(a).unwrap().content_hash;
        let hb = r.get_ref(b).unwrap().content_hash;
        assert_ne!(ha, hb);
        assert_eq!(r.get_ref(a).unwrap().content_hash_hex().len(), 64);
        assert_eq!(r.get_ref(a).unwrap().id_at_version(), "radio/a@1.0.0");

        // The same card registered into a second registry hashes identically.
        let mut r2 = Registry::new();
        let a2 = r2.register(card("radio/a")).unwrap();
        assert_eq!(r2.get_ref(a2).unwrap().content_hash, ha);
    }

    #[test]
    fn todo_calibrate_report_lists_every_uncited_default() {
        let mut r = Registry::new();
        r.register(card_with_todo("z/model", "sigma_db")).unwrap();
        r.register(card("clean/model")).unwrap();
        r.register(card_with_todo("a/model", "alpha")).unwrap();
        let report = r.todo_calibrate_report();
        assert_eq!(report.len(), 2);
        // Id order, not registration order.
        assert_eq!(report[0].1.name, "alpha");
        assert_eq!(report[1].1.name, "sigma_db");
        assert_eq!(r.get_ref(report[0].0).unwrap().card.id, "a/model");
        assert!(
            report
                .iter()
                .all(|(_, p)| p.source.kind == SourceKind::TodoCalibrate)
        );
    }

    /// A live registration carries the object *and* the metadata, and the metadata is
    /// exactly what the card-only path would have produced.
    #[test]
    fn a_live_model_registers_with_its_own_card() {
        use crate::model::{Model, ModelHandle};

        struct Impl(ModelCard);
        impl Model for Impl {
            fn card(&self) -> &ModelCard {
                &self.0
            }
        }

        let mut r = Registry::new();
        let handle: ModelHandle = std::sync::Arc::new(Impl(card("radio/live")));
        let live = r.register_model(handle).unwrap();
        let meta = r.register(card("radio/card-only")).unwrap();

        // The object came back, and it is the same model.
        let got = r
            .get_model(live)
            .expect("a live registration has an object");
        assert_eq!(got.id(), "radio/live");
        assert_eq!(got.card(), &r.get_ref(live).unwrap().card);
        assert!(r.has_implementation(live));
        assert_eq!(r.get_model_by_id("radio/live").unwrap().version(), "1.0.0");

        // The card-only path still works and is distinguishable.
        assert!(!r.has_implementation(meta));
        assert!(r.get_model(meta).is_none());
        assert!(r.get_model_by_id("radio/card-only").is_none());
        assert!(r.get_model_by_id("nope").is_none());
        assert!(r.get_model(ModelRef::new(99)).is_none());
        assert_eq!(r.len(), 2);

        // The metadata a live registration produces is identical to the card-only one's,
        // so a manifest cannot tell which route a model took.
        let mut card_only = Registry::new();
        card_only.register(card("radio/live")).unwrap();
        assert_eq!(
            r.get_ref(live).unwrap().content_hash,
            card_only.get_ref(ModelRef::new(0)).unwrap().content_hash
        );
        assert_eq!(r.get_ref(live).unwrap().hosting, Hosting::InProcess);

        // Only the live one shows up in the implementation iterator, in id order.
        let ids: Vec<&str> = r.iter_models().map(|(_, m)| m.id()).collect();
        assert_eq!(ids, vec!["radio/live"]);
        assert!(format!("{r:?}").contains("live"));
    }

    /// Every gate the card-only path applies still applies to a live one — and a rejected
    /// registration leaves nothing behind, object included.
    #[test]
    fn a_live_model_faces_the_same_gates() {
        use crate::model::{Model, ModelHandle};

        struct Impl(ModelCard);
        impl Model for Impl {
            fn card(&self) -> &ModelCard {
                &self.0
            }
        }
        let handle = |c: ModelCard| -> ModelHandle { std::sync::Arc::new(Impl(c)) };

        let mut r = Registry::new();

        // An invalid card: the missing tier of §12.
        let mut no_tier = card("radio/no-tier");
        no_tier.tier.clear();
        assert!(matches!(
            r.register_model(handle(no_tier)),
            Err(RegistryError::InvalidCard(CardError::EmptyField {
                field: "tier",
                ..
            }))
        ));
        assert!(r.is_empty(), "a rejected model must not be stored");

        // The licence gate, which is about linking code into this process.
        assert!(matches!(
            r.register_model_with_licence(handle(card("mobility/sumo")), Licence::Gpl),
            Err(RegistryError::LicenceRequiresOutOfProcess { .. })
        ));
        assert!(r.is_empty());
        // …but the out-of-process stub is fine.
        let stub = r
            .register_model_out_of_process(handle(card("mobility/sumo")), Licence::Gpl)
            .unwrap();
        assert_eq!(r.get_ref(stub).unwrap().hosting, Hosting::OutOfProcess);
        assert!(r.has_implementation(stub));

        // Duplicate ids, live or not.
        assert!(matches!(
            r.register_model(handle(card("mobility/sumo"))),
            Err(RegistryError::Duplicate { .. })
        ));
        assert_eq!(r.len(), 1);
        assert_eq!(r.iter_models().count(), 1);

        // A permissive licence links.
        assert!(
            r.register_model_with_licence(handle(card("radio/mit")), Licence::Mit)
                .is_ok()
        );
    }

    /// The four paths of the resolution rule, plus the one that makes it worth having.
    #[test]
    fn parameters_resolve_defaults_then_overrides() {
        let mut c = card("radio/resolve");
        c.parameters.push(Parameter::new(
            "sigma_db",
            "dB",
            serde_json::json!(4.0),
            Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §5"),
        ));
        c.parameters.push(Parameter::new(
            "n",
            "-",
            serde_json::json!(2.7),
            Source::new(SourceKind::Paper, "10.1109/TVT.2011.2158118"),
        ));
        c.parameters.push(Parameter::new(
            "environment",
            "-",
            serde_json::json!("urban"),
            Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §4"),
        ));

        // 1. No overrides: exactly the declared defaults, and `null` means the same as `{}`.
        let p = ParamSet::resolve(&c, &serde_json::Value::Null).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p.get_f64("sigma_db"), Some(4.0));
        assert_eq!(p.get_str("environment"), Some("urban"));
        assert_eq!(
            p,
            ParamSet::resolve(&c, &serde_json::json!({})).unwrap(),
            "null and an empty object are the same absence of overrides"
        );

        // 2. An override replaces one value and leaves the rest alone.
        let p = ParamSet::resolve(&c, &serde_json::json!({"sigma_db": 6.0})).unwrap();
        assert_eq!(p.get_f64("sigma_db"), Some(6.0));
        assert_eq!(p.get_f64("n"), Some(2.7));
        assert_eq!(p.len(), 3, "the result always has the card's parameters");
        // An integer for a float parameter is the same JSON type and is accepted.
        assert_eq!(
            ParamSet::resolve(&c, &serde_json::json!({"sigma_db": 6}))
                .unwrap()
                .get_f64("sigma_db"),
            Some(6.0)
        );

        // 3. A name the card does not declare is a scenario typo, and must not be silent.
        let err = ParamSet::resolve(&c, &serde_json::json!({"sigma": 6.0})).unwrap_err();
        assert!(matches!(
            err,
            CoreError::Registry(RegistryError::UnknownParameter { ref parameter, .. })
                if parameter == "sigma"
        ));
        assert!(
            err.to_string().contains("sigma_db"),
            "the message must list what the card does declare: {err}"
        );

        // 4. A type that does not match the declared default.
        let err = ParamSet::resolve(&c, &serde_json::json!({"sigma_db": "6.0"})).unwrap_err();
        assert!(matches!(
            err,
            CoreError::Registry(RegistryError::ParameterType {
                expected: "number",
                got: "string",
                ..
            })
        ));
        assert!(
            ParamSet::resolve(&c, &serde_json::json!({"environment": 3})).is_err(),
            "a number for a string parameter"
        );
        assert!(
            ParamSet::resolve(&c, &serde_json::json!({"sigma_db": null})).is_err(),
            "null is not a number"
        );

        // …and the document itself must be an object.
        assert!(matches!(
            ParamSet::resolve(&c, &serde_json::json!([1, 2])).unwrap_err(),
            CoreError::Registry(RegistryError::BadOverrides { got: "array", .. })
        ));

        // Resolution is content-addressable: spelling the defaults out changes nothing.
        let explicit = serde_json::json!({"sigma_db": 4.0, "n": 2.7, "environment": "urban"});
        assert_eq!(
            ParamSet::resolve(&c, &explicit).unwrap().content_hash(),
            ParamSet::resolve(&c, &serde_json::Value::Null)
                .unwrap()
                .content_hash()
        );
    }

    /// The range rule: two numbers are an inclusive interval, anything else is the set of
    /// allowed values.
    #[test]
    fn parameter_ranges_are_checked_both_ways() {
        let mut c = card("radio/ranges");
        let mut m = Parameter::new(
            "m",
            "-",
            serde_json::json!(3.0),
            Source::new(SourceKind::Paper, "10.1109/TVT.2007.905625"),
        );
        m.range = Some(vec![serde_json::json!(0.5), serde_json::json!(10.0)]);
        c.parameters.push(m);
        let mut env = Parameter::new(
            "environment",
            "-",
            serde_json::json!("urban"),
            Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §4"),
        );
        env.range = Some(vec![
            serde_json::json!("urban"),
            serde_json::json!("suburban"),
            serde_json::json!("highway"),
        ]);
        c.parameters.push(env);

        // Inside, and on both bounds, which are inclusive.
        for ok in [0.5, 1.0, 10.0] {
            assert!(
                ParamSet::resolve(&c, &serde_json::json!({ "m": ok })).is_ok(),
                "{ok} should be inside [0.5, 10.0]"
            );
        }
        for bad in [0.499, 10.001, -1.0] {
            let err = ParamSet::resolve(&c, &serde_json::json!({ "m": bad })).unwrap_err();
            assert!(
                matches!(
                    err,
                    CoreError::Registry(RegistryError::ParameterRange { ref parameter, .. })
                        if parameter == "m"
                ),
                "{bad} should be rejected, got {err}"
            );
            assert!(err.to_string().contains("range [0.5, 10]"), "{err}");
        }

        // A longer list is an enumeration, and membership is exact.
        assert!(ParamSet::resolve(&c, &serde_json::json!({"environment": "highway"})).is_ok());
        let err = ParamSet::resolve(&c, &serde_json::json!({"environment": "rural"})).unwrap_err();
        assert!(err.to_string().contains("allowed values"), "{err}");

        // A parameter with no declared range accepts anything of the right type.
        let mut free = card("radio/free");
        free.parameters.push(Parameter::new(
            "anything",
            "-",
            serde_json::json!(0.0),
            Source::new(SourceKind::Code, "reference implementation"),
        ));
        assert!(ParamSet::resolve(&free, &serde_json::json!({"anything": 1e9})).is_ok());
    }

    /// **`1` and `1.0` are the same number, and the range check has to agree with the type
    /// check about that.** `json_type_name` calls integers and floats one type on purpose —
    /// "JSON, YAML and every authoring format spell 2 and 2.0 interchangeably" — but the
    /// set-membership branch fell through to `Value` equality, and `serde_json`
    /// distinguishes `Number::PosInt(1)` from `Number::Float(1.0)`. A card declaring
    /// `range: [0.1, 0.5, 1.0]` therefore accepted `{"alpha": 1.0}` and rejected
    /// `{"alpha": 1}` — with a message that listed the value it had just refused among the
    /// allowed ones. YAML authoring produces the integer spelling routinely.
    #[test]
    fn a_set_range_accepts_either_spelling_of_the_same_number() {
        let mut c = card("radio/spelling");
        let mut alpha = Parameter::new(
            "alpha",
            "-",
            serde_json::json!(0.5),
            Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §5"),
        );
        alpha.range = Some(vec![
            serde_json::json!(0.1),
            serde_json::json!(0.5),
            serde_json::json!(1.0),
        ]);
        c.parameters.push(alpha);

        for spelling in [serde_json::json!(1.0), serde_json::json!(1)] {
            let set = ParamSet::resolve(&c, &serde_json::json!({ "alpha": spelling }))
                .unwrap_or_else(|e| panic!("{spelling} must be accepted: {e}"));
            assert_eq!(set.get_f64("alpha"), Some(1.0));
        }

        // A number that is genuinely not in the set is still refused, in either spelling.
        for bad in [serde_json::json!(2), serde_json::json!(0.2)] {
            let err = ParamSet::resolve(&c, &serde_json::json!({ "alpha": bad })).unwrap_err();
            assert!(err.to_string().contains("allowed values"), "{err}");
        }

        // An integer-valued enumeration accepts the float spelling for the same reason.
        let mut c = card("radio/slots");
        let mut slots = Parameter::new(
            "slots",
            "-",
            serde_json::json!(1),
            Source::new(SourceKind::Standard, "IEEE 802.11p §9"),
        );
        slots.range = Some(vec![
            serde_json::json!(1),
            serde_json::json!(2),
            serde_json::json!(4),
        ]);
        c.parameters.push(slots);
        assert!(ParamSet::resolve(&c, &serde_json::json!({"slots": 4})).is_ok());
        assert!(ParamSet::resolve(&c, &serde_json::json!({"slots": 4.0})).is_ok());
        assert!(ParamSet::resolve(&c, &serde_json::json!({"slots": 3})).is_err());

        // Non-numeric entries keep exact `Value` equality: no coercion of "1" to 1.
        let mut c = card("radio/words");
        let mut mode = Parameter::new(
            "mode",
            "-",
            serde_json::json!("urban"),
            Source::new(SourceKind::Standard, "ETSI TR 103 257-1 §4"),
        );
        mode.range = Some(vec![
            serde_json::json!("urban"),
            serde_json::json!("highway"),
        ]);
        c.parameters.push(mode);
        assert!(ParamSet::resolve(&c, &serde_json::json!({"mode": "highway"})).is_ok());
        assert!(ParamSet::resolve(&c, &serde_json::json!({"mode": "rural"})).is_err());
    }

    #[test]
    fn param_sets_are_content_addressed() {
        let mut store = ParamSetStore::new();
        assert!(store.is_empty());
        let mut a = ParamSet::new();
        a.insert("sigma_db", serde_json::json!(4.0));
        a.insert("n", serde_json::json!(2.7));
        // Same values, inserted in the other order.
        let mut b = ParamSet::new();
        b.insert("n", serde_json::json!(2.7));
        b.insert("sigma_db", serde_json::json!(4.0));
        assert_eq!(a, b);
        assert_eq!(a.content_hash(), b.content_hash());

        let ida = store.intern(a.clone());
        let idb = store.intern(b);
        assert_eq!(ida, idb, "identical content must intern to one id");
        assert_eq!(store.len(), 1);

        let mut c = ParamSet::new();
        c.insert("sigma_db", serde_json::json!(6.0));
        let idc = store.intern(c);
        assert_ne!(ida, idc);
        assert_eq!(store.len(), 2);
        assert_eq!(store.get(ida), Some(&a));
        assert_eq!(store.find(&a.content_hash()), Some(ida));
        assert_eq!(store.content_hash(ida).unwrap(), a.content_hash());
        assert_eq!(store.iter().count(), 2);
        assert_eq!(ida.to_string(), "p0");
    }

    #[test]
    fn param_set_accessors_and_serde() {
        let mut p = ParamSet::from_iter_values([
            ("n".to_string(), serde_json::json!(2.7)),
            ("count".to_string(), serde_json::json!(5)),
            ("on".to_string(), serde_json::json!(true)),
            ("name".to_string(), serde_json::json!("urban")),
        ]);
        assert_eq!(p.get_f64("n"), Some(2.7));
        assert_eq!(p.get_u64("count"), Some(5));
        assert_eq!(p.get_bool("on"), Some(true));
        assert_eq!(p.get_str("name"), Some("urban"));
        assert_eq!(p.get("missing"), None);
        assert_eq!(p.get_f64("name"), None);
        assert_eq!(p.len(), 4);
        assert!(!p.is_empty());
        assert_eq!(p.iter().next().unwrap().0, "count", "iteration is sorted");
        assert_eq!(
            p.insert("n", serde_json::json!(3.0)),
            Some(serde_json::json!(2.7))
        );

        let json = serde_json::to_string(&p).unwrap();
        assert!(json.starts_with(r#"{"count":5"#), "{json}");
        assert_eq!(serde_json::from_str::<ParamSet>(&json).unwrap(), p);
        assert_eq!(p.canonical_bytes(), json.as_bytes());
    }

    /// The set's own keys are sorted because it is a `BTreeMap`; a *nested* value's keys
    /// are sorted because [`ParamSet::canonical_bytes`] canonicalises the whole tree. Two
    /// sets built with the same nested object in different key orders must intern to one
    /// id, or provenance would show two parameter sets where the run used one.
    #[test]
    fn nested_parameter_values_are_canonicalised() {
        let nested_a: serde_json::Value =
            serde_json::from_str(r#"{"sigma_db":4.0,"env":{"rural":1,"urban":2}}"#).unwrap();
        let nested_b: serde_json::Value =
            serde_json::from_str(r#"{"env":{"urban":2,"rural":1},"sigma_db":4.0}"#).unwrap();

        let mut a = ParamSet::new();
        a.insert("preset", nested_a);
        let mut b = ParamSet::new();
        b.insert("preset", nested_b);

        assert_eq!(
            String::from_utf8(a.canonical_bytes()).unwrap(),
            r#"{"preset":{"env":{"rural":1,"urban":2},"sigma_db":4.0}}"#
        );
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        assert_eq!(a.content_hash(), b.content_hash());

        let mut store = ParamSetStore::new();
        assert_eq!(store.intern(a), store.intern(b));
        assert_eq!(store.len(), 1);
    }
}
