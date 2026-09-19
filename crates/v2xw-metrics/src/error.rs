//! The crate's error type.
//!
//! Every fallible entry point here returns [`MetricError`]. It wraps
//! [`v2xw_core::CoreError`] so `?` works against the contract crate, and adds the four
//! failure modes this crate owns: a record that does not decode against the reader-side
//! view of its channel, a definition that contradicts itself, a float that reached a
//! writer off its declared grid (build decision D9), and a runtime diagnostic offered to
//! a digest (08-measurement-and-data.md: machine-dependent numbers are not digested).

/// The error type of `v2xw-metrics`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MetricError {
    /// An error from the contract crate.
    #[error(transparent)]
    Core(#[from] v2xw_core::CoreError),

    /// JSON decoding of a recorded event failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// A recorded event on `channel` did not decode against this crate's reader-side view
    /// of that channel's schema (03-interfaces.md §14).
    #[error("channel {channel}: record does not decode against the reader-side view: {source}")]
    Decode {
        /// The channel the record claimed.
        channel: String,
        /// The underlying serde failure.
        #[source]
        source: serde_json::Error,
    },

    /// A record was handed to a decoder for a different channel.
    #[error("expected a record on channel {expected}, got one on {got}")]
    ChannelMismatch {
        /// The channel the decoder reads.
        expected: &'static str,
        /// The channel the record claimed.
        got: String,
    },

    /// A [`crate::MetricDef`] is internally inconsistent — an empty name, a non-positive
    /// quantum, a ratio aggregation without both of its terms.
    #[error("metric definition {name}: {problem}")]
    BadDefinition {
        /// The metric's name.
        name: String,
        /// What is wrong with it.
        problem: String,
    },

    /// A float reached a writer without sitting on its declared grid (build decision D9).
    ///
    /// The writers in this crate quantise rather than reject, so this is raised only by the
    /// scanning check [`crate::invariants::check_d9_quantisation`] and by the Arrow writer's
    /// post-condition — the places whose job is to prove that nothing escaped.
    #[error("metric {name}: value {value} is off its declared grid of {quantum}")]
    OffGrid {
        /// The metric whose value is off grid.
        name: String,
        /// The offending value.
        value: f64,
        /// The grid it should have sat on.
        quantum: f64,
    },

    /// A machine-dependent runtime diagnostic was offered to a digested artefact.
    ///
    /// Wall-clock rates and memory high-water marks differ between machines, so including
    /// one in a digest would make the determinism gate fail for reasons that have nothing
    /// to do with the simulation. [`crate::DigestSet`] cannot be constructed with one; this
    /// variant is what the explicit check returns.
    #[error("metric {name} is a runtime diagnostic and may not enter a digested artefact")]
    DiagnosticInDigest {
        /// The diagnostic metric's name.
        name: String,
    },

    /// One or more invariant checks failed.
    ///
    /// The message names every failing invariant with its numbers, because "an invariant
    /// failed" is not actionable and "I-N1: frame 42's bytes are attributed to both `air`
    /// and `backhaul` (400 B each)" is. The structured form is
    /// [`crate::invariants::InvariantReport`], which the caller still holds.
    #[error("{count} invariant violation(s): {detail}")]
    InvariantViolated {
        /// How many violations there were.
        count: usize,
        /// The violations, one per line.
        detail: String,
    },

    /// Arrow schema or array construction failed.
    #[error("arrow: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

/// `Result<T>` is `Result<T, MetricError>`.
pub type Result<T, E = MetricError> = core::result::Result<T, E>;
