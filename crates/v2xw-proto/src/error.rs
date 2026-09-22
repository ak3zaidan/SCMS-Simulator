//! The crate's error type.
//!
//! Protocol *outcomes* are not errors: a request the Registration Authority refuses
//! because the enrolment certificate is blocklisted is data a run is measuring, and it
//! travels as a refusal message with a reason, not as a `Result::Err`. What lands here is
//! a defect — a size model that cannot be computed, a flow driven out of order, an entity
//! addressed on a node that holds no entity.

use v2xw_core::ids::NodeId;

/// The crate's result alias.
pub type Result<T> = core::result::Result<T, ProtoError>;

/// What can go wrong inside a protocol plug-in.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProtoError {
    /// A message was addressed to a node that hosts no entity of this deployment.
    #[error("no protocol entity is hosted on {node}")]
    NoEntity {
        /// The node that was addressed.
        node: NodeId,
    },

    /// The backend network has no link between two entities a flow needs to connect.
    #[error("no backend link from {from} to {to}")]
    NoLink {
        /// The sending node.
        from: NodeId,
        /// The receiving node.
        to: NodeId,
    },

    /// A size could not be obtained from the real encoder.
    #[error("size model: {what}: {source}")]
    Size {
        /// Which size was being computed.
        what: &'static str,
        /// The encoder's complaint.
        #[source]
        source: v2xw_sec::error::SecError,
    },

    /// The security layer refused an operation a flow depends on.
    #[error(transparent)]
    Sec(#[from] v2xw_sec::error::SecError),

    /// A flow was asked to do something its state machine does not allow here.
    #[error("flow {flow}: {detail}")]
    Flow {
        /// The flow's stable name.
        flow: &'static str,
        /// What was wrong.
        detail: String,
    },
}
