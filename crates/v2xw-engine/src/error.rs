//! The engine's error type.
//!
//! Scenario validation is the loud one: 03-interfaces.md §13 requires that an invalid
//! scenario produce an error naming *the field* and *the conflict*, not a line number and
//! a serde message. [`ScenarioError::Conflict`] is that shape, and
//! [`crate::scenario::validate`] is the only thing that builds it.

use thiserror::Error;

/// Anything that can go wrong loading, validating or running a scenario.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The scenario file could not be read.
    #[error("cannot read scenario {path}: {source}")]
    Io {
        /// The path that failed.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The scenario is not valid.
    #[error(transparent)]
    Scenario(#[from] ScenarioError),

    /// A model could not be registered, so the run has no manifest entry for it.
    #[error("registry: {0}")]
    Registry(#[from] v2xw_core::registry::RegistryError),

    /// The world could not be built or imported.
    #[error("world: {0}")]
    World(#[from] v2xw_world::WorldError),

    /// The mobility provider refused to start or to step.
    #[error("mobility: {0}")]
    Mobility(#[from] v2xw_mobility::MobError),

    /// A core contract was violated.
    #[error("core: {0}")]
    Core(#[from] v2xw_core::error::CoreError),

    /// A metric provider could not be registered or could not read its own channel.
    #[error("metrics: {0}")]
    Metrics(#[from] v2xw_metrics::MetricError),

    /// The recorder failed.
    #[error("recording: {0}")]
    Record(#[from] v2xw_record::RecordError),
}

/// Why a scenario is not loadable.
///
/// Every variant names the field it is about, because the whole point of §13's
/// "actionable errors" is that the author can find the line without reading the loader.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScenarioError {
    /// The document is not YAML or JSON the schema deserialises.
    #[error("{path}: cannot parse scenario: {message}")]
    Parse {
        /// The file, or `<memory>`.
        path: String,
        /// The parser's message.
        message: String,
    },

    /// The `schema` key is missing or is not a version this build knows.
    #[error(
        "schema: '{found}' is not a schema this build can load; known versions are {known}, \
         and migration runs one step at a time (schema: v2xw/scenario/1)"
    )]
    UnknownSchema {
        /// What the document said.
        found: String,
        /// The versions the migrator chain covers, oldest first.
        known: String,
    },

    /// A field's value conflicts with another field's value.
    ///
    /// This is the §13 shape: `radio.tiers.phy: 'high' requires mac 'high' (mac is
    /// 'medium')`. [`ScenarioError::field`] is the dotted path and nothing else, so a UI
    /// can highlight it.
    #[error("{field}: {conflict}")]
    Conflict {
        /// The dotted path of the offending field, e.g. `radio.tiers.phy`.
        field: String,
        /// What is wrong with it, in the author's terms, including the value that
        /// conflicts and where that value came from.
        conflict: String,
    },

    /// A base scenario referenced by `meta.base` could not be resolved.
    #[error("meta.base: cannot resolve base scenario '{base}': {why}")]
    Base {
        /// The unresolved reference.
        base: String,
        /// Why it could not be resolved.
        why: String,
    },

    /// Two migrators claim the same version step, or a step is missing from the chain.
    #[error("schema: no migrator from '{from}' to the next version; the chain stops there")]
    MigrationGap {
        /// The version with no successor.
        from: String,
    },
}

impl ScenarioError {
    /// A conflict about `field`.
    pub fn conflict(field: impl Into<String>, conflict: impl Into<String>) -> Self {
        ScenarioError::Conflict {
            field: field.into(),
            conflict: conflict.into(),
        }
    }

    /// The dotted field path this error is about, when it has one.
    ///
    /// A UI highlights this; a test asserts on it, which is why it is not folded into the
    /// `Display` string alone.
    pub fn field(&self) -> Option<&str> {
        match self {
            ScenarioError::Conflict { field, .. } => Some(field),
            ScenarioError::Base { .. } => Some("meta.base"),
            ScenarioError::UnknownSchema { .. } | ScenarioError::MigrationGap { .. } => {
                Some("schema")
            }
            ScenarioError::Parse { .. } => None,
        }
    }
}

/// The engine's result alias.
pub type Result<T> = core::result::Result<T, EngineError>;
