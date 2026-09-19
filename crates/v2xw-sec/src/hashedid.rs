//! IEEE 1609.2 `HashedId3`, `HashedId8` and `HashedId10`.
//!
//! One definition, three widths: the **low-order** N bytes of SHA-256 over the canonical
//! COER encoding of whatever is being identified [IEEE 1609.2a-2017 §6.3.25-6.3.26].
//! Low-order, not high-order — that is the detail that makes a hand-written
//! implementation wrong half the time, and it is why this module exists rather than a
//! `sha256(..)[..8]` at each call site.
//!
//! The standard's own worked example is the test: SHA-256 of the empty string is
//! `e3b0c442…a495991b7852b855`, so its `HashedId8` is `a495991b7852b855` and its
//! `HashedId3` is `52b855`.
//!
//! # Why the three widths nest
//!
//! All three are suffixes of the same digest, so a shorter id is a suffix of a longer one:
//! `HashedId3` is the last 3 bytes of `HashedId8`, which is the last 8 of `HashedId10`.
//! That is not a coincidence to be exploited quietly — it is what makes P2PCD work. A
//! receiver that knows a certificate's `HashedId8` can answer a `p2pcdLearningRequest`
//! carrying only a `HashedId3` without recomputing anything, and
//! [`hashed_id3_of_id8`] is that operation, named.
//!
//! # Canonical encoding
//!
//! "Over the canonical COER encoding" is load-bearing. 1609.2 §6.1.2 canonicalises a
//! certificate before hashing it (compressed elliptic-curve points, a canonical
//! signature), and a receiver that hashed a non-canonical encoding would compute a
//! different `HashedId8` and fail to match a certificate it holds. This crate never
//! constructs a non-canonical point — [`crate::ec::Point::to_ecc_point`] only emits the
//! compressed alternatives — so hashing the encoder's own output is hashing the canonical
//! form. [`the_encoder_emits_only_canonical_points`] in `crate::envelope` is the test
//! that keeps that true.

use v2xw_core::hash::sha256;
use v2xw_msg::codec::MsgType;
use v2xw_msg::sec_types::ieee1609_dot2_base_types::HashedId10;
use v2xw_msg::sec_types::{Certificate, HashedId3, HashedId8, coer};

use crate::error::Result;

/// The low-order 3 bytes of SHA-256 over `canonical`.
pub fn hashed_id3(canonical: &[u8]) -> HashedId3 {
    let d = sha256(canonical);
    HashedId3(rasn::types::FixedOctetString::from([d[29], d[30], d[31]]))
}

/// The low-order 8 bytes of SHA-256 over `canonical`.
pub fn hashed_id8(canonical: &[u8]) -> HashedId8 {
    let d = sha256(canonical);
    let mut out = [0u8; 8];
    out.copy_from_slice(&d[24..32]);
    HashedId8(rasn::types::FixedOctetString::from(out))
}

/// The low-order 10 bytes of SHA-256 over `canonical`.
///
/// The width a CRL hash entry uses (`HashedId10` + `Time32` = 14 bytes per revoked
/// certificate, 04-models.md §9.6).
pub fn hashed_id10(canonical: &[u8]) -> HashedId10 {
    let d = sha256(canonical);
    let mut out = [0u8; 10];
    out.copy_from_slice(&d[22..32]);
    HashedId10(rasn::types::FixedOctetString::from(out))
}

/// The `HashedId3` that corresponds to a `HashedId8` of the same object.
///
/// Both are suffixes of one SHA-256 digest, so this is a truncation and not a second
/// hash. Used by P2PCD: an out-of-band `p2pcdLearningRequest` names an unknown issuer by
/// `HashedId3`, and a responder matches it against the `HashedId8`s in its own store.
pub fn hashed_id3_of_id8(id8: &HashedId8) -> HashedId3 {
    let b = &id8.0;
    HashedId3(rasn::types::FixedOctetString::from([b[5], b[6], b[7]]))
}

/// The whole-certificate `HashedId8`: SHA-256 over the certificate's COER encoding.
///
/// 1609.2 calls this the *whole-certificate hash* and uses it as the certificate's
/// identity everywhere — in a `digest` signer identifier, in an `IssuerIdentifier`, in a
/// peer certificate cache, in a CRL.
pub fn certificate_digest(cert: &Certificate) -> Result<HashedId8> {
    Ok(hashed_id8(&coer::encode(MsgType::Crl, cert)?))
}

/// The whole-certificate `HashedId10`, for a hash-based CRL entry.
pub fn certificate_hashed_id10(cert: &Certificate) -> Result<HashedId10> {
    Ok(hashed_id10(&coer::encode(MsgType::Crl, cert)?))
}

/// The `HashedId8` of any COER-encodable value.
pub fn hashed_id8_of<T: rasn::Encode>(value: &T) -> Result<HashedId8> {
    Ok(hashed_id8(&coer::encode(MsgType::Crl, value)?))
}

/// A `HashedId8`'s bytes, for logging and for map keys.
pub fn id8_bytes(id: &HashedId8) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&id.0[..]);
    out
}

/// A `HashedId8` from raw bytes — for a CRL or a request that carries one on the wire.
pub fn id8_from_bytes(bytes: [u8; 8]) -> HashedId8 {
    HashedId8(rasn::types::FixedOctetString::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::hash::{hex_encode, sha256_hex};

    /// The standard's own worked example (IEEE 1609.2a-2017 §6.3.25-6.3.26), and the one
    /// the task names: SHA-256 of the empty string gives HashedId8 `a495991b7852b855`.
    #[test]
    fn the_published_example_from_the_standard_reproduces() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "the SHA-256 of the empty string itself, as a sanity anchor"
        );
        assert_eq!(hex_encode(&hashed_id8(b"").0[..]), "a495991b7852b855");
        assert_eq!(hex_encode(&hashed_id3(b"").0[..]), "52b855");
        assert_eq!(hex_encode(&hashed_id10(b"").0[..]), "934ca495991b7852b855");
    }

    /// Low-order, not high-order. Stated as its own assertion because it is the one
    /// mistake this module exists to prevent, and because a high-order implementation
    /// would still pass a round-trip test.
    #[test]
    fn the_id_is_the_low_order_bytes_not_the_high_order_ones() {
        let msg = b"a certificate, or anything else";
        let digest = sha256(msg);
        assert_eq!(&hashed_id8(msg).0[..], &digest[24..32]);
        assert_eq!(&hashed_id3(msg).0[..], &digest[29..32]);
        assert_eq!(&hashed_id10(msg).0[..], &digest[22..32]);
        assert_ne!(
            &hashed_id8(msg).0[..],
            &digest[0..8],
            "a high-order implementation would differ here"
        );
    }

    /// The three widths are suffixes of one digest, which is what lets P2PCD match a
    /// `HashedId3` request against a stored `HashedId8`.
    #[test]
    fn the_three_widths_nest() {
        for msg in [b"" as &[u8], b"x", b"a longer certificate encoding"] {
            let id10 = hashed_id10(msg);
            let id8 = hashed_id8(msg);
            let id3 = hashed_id3(msg);
            assert_eq!(&id10.0[2..], &id8.0[..], "id8 is the last 8 bytes of id10");
            assert_eq!(&id8.0[5..], &id3.0[..], "id3 is the last 3 bytes of id8");
            assert_eq!(hashed_id3_of_id8(&id8), id3, "the truncation agrees");
        }
    }

    /// Round-tripping the raw-bytes helpers.
    #[test]
    fn the_raw_byte_helpers_round_trip() {
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(id8_bytes(&id8_from_bytes(bytes)), bytes);
    }
}
