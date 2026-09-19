//! Crate error types.
//!
//! Two of them, split by who is at fault:
//!
//! * [`CodecError`] — a message could not be turned into bytes or back. Either the codec
//!   was handed a message type it does not implement, or a field was outside the range its
//!   ASN.1 type allows, or the bytes did not decode.
//! * [`MsgError`] — everything else this crate can refuse: a malformed model card, or a
//!   size-model entry outside its declared anchor spread.
//!
//! Both are `#[non_exhaustive]`, and both carry only variants something actually produces.
//! An error variant nothing returns is worse than no variant: a caller writes a match arm
//! for it, the arm is never taken, and the dead branch looks like tested behaviour. The
//! `#[non_exhaustive]` attribute is what makes adding one later a non-breaking change, so
//! there is no reason to declare them in advance.
//!
//! Neither wraps `rasn`'s own error types. `rasn::error::EncodeError` and `DecodeError` are
//! `#[non_exhaustive]`, large, and versioned with `rasn`; putting them in a public enum
//! would make a `rasn` bump a breaking change for `v2xw-sec` and every other consumer. The
//! text is kept, which is what a diagnostic needs, and the failing field is named
//! separately, which is what a *fix* needs.

use crate::codec::MsgType;

/// A message could not be encoded or decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CodecError {
    /// This codec does not implement this message type.
    ///
    /// Not a bug on its own: the three-tier strategy of build decision D2 means the ETSI
    /// codec genuinely does not encode a BSM, and the caller is expected to route by
    /// [`crate::codec::MessageCodec::message_types`].
    #[error("codec `{codec}` does not implement {ty}")]
    Unsupported {
        /// The codec's model id.
        codec: String,
        /// The message type asked for.
        ty: MsgType,
    },

    /// A value did not fit the range its ASN.1 type declares.
    ///
    /// Raised by the builders before `rasn` is called, so the message names the simulator
    /// quantity and the ASN.1 field rather than a byte offset.
    #[error(
        "{field}: {value} is outside the range {min}..={max} that ASN.1 type `{asn1_type}` allows"
    )]
    OutOfRange {
        /// Dotted path of the field being filled, e.g. `cam.basicContainer.referencePosition.latitude`.
        field: &'static str,
        /// The ASN.1 type whose constraint was violated.
        asn1_type: &'static str,
        /// The offending value, already converted to the ASN.1 unit.
        value: i64,
        /// Lowest admissible value.
        min: i64,
        /// Highest admissible value.
        max: i64,
    },

    /// `rasn` refused to encode the message.
    #[error("UPER/COER encoding of {ty} failed: {detail}")]
    Encode {
        /// The message type being encoded.
        ty: MsgType,
        /// `rasn`'s own message.
        detail: String,
    },

    /// `rasn` refused to decode the bytes.
    #[error("UPER/COER decoding of {ty} failed after {len} bytes: {detail}")]
    Decode {
        /// The message type being decoded.
        ty: MsgType,
        /// How many bytes were offered.
        len: usize,
        /// `rasn`'s own message.
        detail: String,
    },

    /// A construct the hand-written J2735 codec does not implement (build decision D2).
    ///
    /// Distinct from [`CodecError::Unsupported`], which is about a *message type* no codec
    /// claims. This one is about a construct *inside* a message the codec does claim: a
    /// `FullPositionVector` in a path history, a regional extension, an extension addition
    /// from a later edition of the standard. It has to be an error rather than a skipped
    /// field, because PER carries no tags and no lengths on most fields, so a decoder that
    /// stepped over an unknown element would misread every element after it and return a
    /// plausible message assembled from the wrong bits.
    #[error("the {ty} codec does not implement {construct}: {detail}")]
    UnsupportedConstruct {
        /// The message being encoded or decoded.
        ty: MsgType,
        /// The ASN.1 element, e.g. `PathHistory.initialPosition`.
        construct: &'static str,
        /// Why it is not implemented, and what happens instead.
        detail: &'static str,
    },

    /// The bytes came from the size-model tier and carry no fields to decode.
    ///
    /// Invariant I-S2 allows a `SizeModel` encoding to be exact in *size* only. Decoding
    /// one is a caller error, not a data error: nothing was ever encoded.
    #[error(
        "{ty} was produced by the size model (version {version}), so its {len} bytes are \
         placeholders and carry no fields; see build decision D2"
    )]
    PlaceholderBytes {
        /// The message type.
        ty: MsgType,
        /// The size model version that produced them.
        version: crate::codec::SizeModelVersion,
        /// How many placeholder bytes.
        len: usize,
    },
}

/// Anything this crate refuses that is not an encoding failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MsgError {
    /// A model card this crate builds did not validate.
    #[error(transparent)]
    Card(#[from] v2xw_core::card::CardError),

    /// Encoding or decoding failed.
    #[error(transparent)]
    Codec(#[from] CodecError),

    /// A size-model entry's value sits outside the anchor spread its own card declares
    /// (invariant I-S2).
    #[error(
        "size-model entry {entry}: modelled {modelled} B is outside its anchor spread \
         {low}..={high} B"
    )]
    OutsideAnchorSpread {
        /// `<message>/<profile>`.
        entry: String,
        /// What the model says.
        modelled: u32,
        /// Lowest cited anchor.
        low: u32,
        /// Highest cited anchor.
        high: u32,
    },
}

/// `Result<T>` is `Result<T, MsgError>`.
pub type Result<T, E = MsgError> = core::result::Result<T, E>;
