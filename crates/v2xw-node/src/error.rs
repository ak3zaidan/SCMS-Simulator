//! Errors raised by the node runtime.

use v2xw_core::ids::NodeId;

/// What can go wrong in a node runtime.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NodeError {
    /// A hardware profile file did not parse.
    #[error("hardware profile `{id}` does not parse: {source}")]
    ProfileParse {
        /// The profile's file stem or declared id.
        id: String,
        /// The underlying YAML error.
        #[source]
        source: serde_yml::Error,
    },

    /// A hardware profile broke rule H1 or H2 of 06-node-models.md §1.
    #[error("hardware profile `{id}` violates rule {rule}: {what}")]
    ProfileInvalid {
        /// The profile id.
        id: String,
        /// `H1` (every numeric field has a source or a calibration plan) or `H2`
        /// (a profile without an HSM declares `hsm.kind: none`).
        rule: &'static str,
        /// The offending field and why.
        what: String,
    },

    /// A profile that a node referenced is not loaded.
    #[error("no hardware profile with id `{0}` is registered")]
    UnknownProfile(String),

    /// A model card built from a profile failed [`v2xw_core::card::ModelCard::validate`].
    #[error("hardware profile `{id}` produced an invalid model card: {source}")]
    Card {
        /// The profile id.
        id: String,
        /// The card error.
        #[source]
        source: v2xw_core::card::CardError,
    },

    /// A queue refused work because it was full.
    #[error("node {node} queue `{queue}` is full at depth {depth}")]
    Overload {
        /// Which node.
        node: NodeId,
        /// Which queue.
        queue: &'static str,
        /// The depth at the moment of refusal.
        depth: usize,
    },

    /// A security operation failed.
    #[error(transparent)]
    Sec(#[from] v2xw_sec::SecError),
}

/// The crate's result alias.
pub type Result<T> = core::result::Result<T, NodeError>;
