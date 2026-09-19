//! Crate-wide error type.
//!
//! Every fallible entry point in `v2xw-core` returns either its module-local error
//! ([`crate::card::CardError`], [`crate::registry::RegistryError`],
//! [`crate::time::TimeError`]) or [`CoreError`], which wraps all of them plus the
//! serialization errors of `serde_json` and `serde_yml`. Downstream crates are expected
//! to convert into [`CoreError`] with `?` and to add their own domain errors on top.

use crate::card::CardError;
use crate::registry::RegistryError;
use crate::time::TimeError;

/// The error type of `v2xw-core`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// A model card failed schema or registry-rule validation (03-interfaces.md §12).
    #[error("invalid model card: {0}")]
    Card(#[from] CardError),

    /// A model registry operation failed (ADR 0007).
    #[error("registry: {0}")]
    Registry(#[from] RegistryError),

    /// A time conversion or parse failed.
    #[error("time: {0}")]
    Time(#[from] TimeError),

    /// JSON serialization or deserialization failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// YAML serialization or deserialization failed.
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yml::Error),

    /// A value handed to the kernel violated a documented contract (for example a
    /// negative rate parameter or an event scheduled in the past).
    #[error("contract violation: {0}")]
    Contract(String),
}

/// Result alias used throughout the crate: `Result<T>` is `Result<T, CoreError>`.
pub type Result<T, E = CoreError> = core::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_module_errors() {
        let e: CoreError = CardError::InvalidId {
            id: "Bad Id".to_string(),
        }
        .into();
        assert!(e.to_string().starts_with("invalid model card"));
    }

    #[test]
    fn wraps_json_errors() {
        let e: CoreError = serde_json::from_str::<u32>("nope").unwrap_err().into();
        assert!(e.to_string().starts_with("json:"));
    }
}
