//! What the tool can fail with, and how it says so.
//!
//! Every variant carries the path or the field it is about. A command-line tool whose
//! error is "invalid scenario" makes the operator open the loader; the engine already
//! produces errors that name the field (03-interfaces.md §13), and this type's job is to
//! pass them through without flattening them into a string.

use thiserror::Error;

/// Anything the tool can fail with.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CliError {
    /// A file could not be read or written.
    #[error("{what} {path}: {source}")]
    Io {
        /// What the tool was doing, e.g. `cannot write`.
        what: &'static str,
        /// The path it was doing it to.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The engine refused the scenario or the run.
    #[error(transparent)]
    Engine(#[from] v2xw_engine::EngineError),

    /// The recording could not be written or read.
    #[error(transparent)]
    Record(#[from] v2xw_record::RecordError),

    /// A core contract failed, such as a manifest that would not serialise.
    #[error(transparent)]
    Core(#[from] v2xw_core::error::CoreError),

    /// The importer refused.
    #[error(transparent)]
    World(#[from] v2xw_world::WorldError),

    /// A value on the command line is not one this build knows.
    #[error("--{flag}: {problem}")]
    BadArgument {
        /// The flag, without its dashes.
        flag: &'static str,
        /// What is wrong with the value, including the value and the accepted ones.
        problem: String,
    },

    /// JSON could not be produced.
    #[error("cannot serialise {what}: {source}")]
    Json {
        /// What failed to serialise.
        what: &'static str,
        /// The serde error.
        #[source]
        source: serde_json::Error,
    },
}

impl CliError {
    /// An I/O failure with the context of what the tool was attempting.
    pub fn io(
        what: &'static str,
        path: impl AsRef<std::path::Path>,
        source: std::io::Error,
    ) -> Self {
        CliError::Io {
            what,
            path: path.as_ref().display().to_string(),
            source,
        }
    }
}

/// The tool's result alias.
pub type Result<T> = core::result::Result<T, CliError>;
