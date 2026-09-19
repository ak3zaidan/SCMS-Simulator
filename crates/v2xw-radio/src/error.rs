//! The crate's error type.

use crate::types::Mcs;

/// What can go wrong in a radio model.
///
/// Small on purpose: almost everything in this crate is a closed-form computation over
/// cited constants, and the only failures are a caller asking for something the standards
/// forbid (a frame above the MSDU cap) or a parameter set that a card's own range would
/// have rejected.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RadioError {
    /// The frame is larger than the maximum MSDU, and 04-models.md §4.6 requires the
    /// fragmenter to have acted before the PHY sees it.
    #[error("frame of {bytes} B exceeds the {cap} B maximum MSDU (04-models.md §4.6)")]
    FrameTooLarge {
        /// The offending frame length.
        bytes: u32,
        /// The cap it exceeded.
        cap: u32,
    },
    /// A parameter is outside the range the model can evaluate.
    #[error("parameter {name:?} = {value} is outside {expected}")]
    Parameter {
        /// The parameter's name as the card spells it.
        name: &'static str,
        /// The value that was supplied.
        value: f64,
        /// What would have been acceptable.
        expected: &'static str,
    },
    /// A preset id no model in this crate ships.
    #[error("unknown preset {preset:?} for {model:?}")]
    UnknownPreset {
        /// The model that was asked.
        model: &'static str,
        /// The preset that was asked for.
        preset: String,
    },
    /// An arrival was evaluated that the PHY has no record of.
    #[error("no arrival {tx} is registered at node {rx}")]
    UnknownArrival {
        /// The transmission id.
        tx: u64,
        /// The receiver.
        rx: u32,
    },
    /// The abstract tier was asked for a point outside the envelope its table was
    /// calibrated for (04-models.md §4.9 step 6).
    #[error("{what} is outside the calibrated envelope of this table: {detail}")]
    OutsideEnvelope {
        /// What was asked for.
        what: &'static str,
        /// Why it does not fit.
        detail: String,
    },
    /// The error model was asked about an MCS the table does not cover.
    #[error("no error model for {mcs}")]
    UnsupportedMcs {
        /// The MCS.
        mcs: Mcs,
    },
}

/// The crate's result alias.
pub type Result<T> = core::result::Result<T, RadioError>;
