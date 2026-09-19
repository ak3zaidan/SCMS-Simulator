//! What this crate can refuse.
//!
//! One error type, [`SecError`], because the four subsystems here — the envelope, the
//! backends, the primitive catalogue and the SCMS core — are used together in one call
//! chain (sign a payload, which signs with a backend, which looks up a descriptor) and
//! splitting them would mean a `From` impl per pair and nothing gained.
//!
//! `#[non_exhaustive]`, and every variant is produced by something in the crate. A
//! variant nothing returns is worse than no variant: a caller writes a match arm for it,
//! the arm is never taken, and the dead branch looks like tested behaviour. Two variants
//! drafted during the build were removed on exactly that ground: a "signer has no
//! certificate" error that [`crate::envelope::SignerHandle`] makes unreachable, because it
//! cannot be constructed without one, and an "unprofiled message type" error that no
//! profile rule raises.
//!
//! `rasn`'s own error types are not wrapped, for the reason `v2xw-msg`'s error module
//! gives: they are `#[non_exhaustive]` and versioned with `rasn`, so putting one in a
//! public enum would make a `rasn` bump a breaking change for every consumer. The text is
//! kept, which is what a diagnostic needs.

use v2xw_core::time::TimeError;
use v2xw_msg::error::CodecError;

use crate::primitive::PrimitiveId;

/// Anything this crate refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SecError {
    /// The COER encoder or decoder refused.
    #[error(transparent)]
    Codec(#[from] CodecError),

    /// A model card this crate builds did not validate.
    #[error(transparent)]
    Card(#[from] v2xw_core::card::CardError),

    /// A simulated instant could not be turned into an IEEE 1609.2 time.
    #[error(transparent)]
    Time(#[from] TimeError),

    /// This backend does not implement this primitive.
    ///
    /// The ordinary case, not a bug: the post-quantum primitives of 04-models.md §9.4
    /// have descriptors and cost rows but no implementation, so
    /// [`crate::crypto::Real`] refuses them while [`crate::crypto::Modeled`] serves
    /// them from the descriptor's sizes.
    #[error("crypto backend `{backend}` does not implement primitive `{primitive}`")]
    UnsupportedPrimitive {
        /// The backend's model id.
        backend: &'static str,
        /// The primitive asked for.
        primitive: PrimitiveId,
    },

    /// No primitive with this id is in the catalogue.
    #[error("no primitive descriptor is registered for `{primitive}`")]
    UnknownPrimitive {
        /// The id asked for.
        primitive: PrimitiveId,
    },

    /// A key handle named a key this backend never generated, or one it has since
    /// forgotten.
    #[error("crypto backend `{backend}` holds no key {key}")]
    UnknownKey {
        /// The backend's model id.
        backend: &'static str,
        /// The handle's key id.
        key: u64,
    },

    /// A signing operation was asked for on a key the backend holds only the public half
    /// of — one it learned from a certificate.
    ///
    /// Its own variant because the mistake it catches is a confusion of roles: a receiver
    /// that imported a peer's public key and then tried to sign with it has a bug in its
    /// logic, not bad input, and "unknown key" would have sent the reader looking in the
    /// wrong place.
    #[error("crypto backend `{backend}` holds only the public half of key {key}")]
    PublicKeyOnly {
        /// The backend's model id.
        backend: &'static str,
        /// The handle's key id.
        key: u64,
    },

    /// A handle from one backend was used with another.
    ///
    /// Worth its own variant because the failure it prevents is silent: the two backends
    /// number their keys from the same counter, so a handle from one would name a
    /// *different, existing* key in the other and verification would return a wrong
    /// answer instead of an error.
    #[error("key {key} belongs to backend `{owner}`, not to `{backend}`")]
    WrongBackend {
        /// The backend the handle was offered to.
        backend: &'static str,
        /// The backend that issued it.
        owner: &'static str,
        /// The handle's key id.
        key: u64,
    },

    /// The bytes decoded as an `Ieee1609Dot2Data` whose content is not `signedData`.
    #[error("the SPDU carries {found} content, not signedData")]
    NotSignedData {
        /// The content alternative that was found.
        found: &'static str,
    },

    /// A field the profile requires was absent.
    #[error("{profile}: required field `{field}` is absent")]
    MissingField {
        /// The profile that requires it.
        profile: &'static str,
        /// Dotted path of the field.
        field: &'static str,
    },

    /// A fixed-width value had the wrong width.
    #[error("{what}: expected {expected} bytes, got {got}")]
    BadLength {
        /// What was being built.
        what: &'static str,
        /// The width the standard fixes.
        expected: usize,
        /// The width offered.
        got: usize,
    },

    /// A real cryptographic operation failed on genuine key or signature material.
    ///
    /// Distinct from a verification returning `false`: this is malformed material (a
    /// point that is not on the curve, a scalar that is zero), which is a caller error,
    /// whereas a `false` verification outcome is data.
    #[error("{op} failed on {primitive}: {detail}")]
    Crypto {
        /// Which operation.
        op: &'static str,
        /// The primitive it ran under.
        primitive: PrimitiveId,
        /// What went wrong.
        detail: String,
    },
}

/// `Result<T>` is `Result<T, SecError>`.
pub type Result<T, E = SecError> = core::result::Result<T, E>;
