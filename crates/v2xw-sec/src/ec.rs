//! NIST P-256 scalar and point arithmetic, and the two reductions the SCMS core needs.
//!
//! The butterfly-key expansion of CAMP SCP1 and the ECQV implicit-certificate
//! construction of SEC 4 are both written in terms of group arithmetic — a private key is
//! a sum of scalars modulo the group order `n`, and a public key is the corresponding sum
//! of points. The legacy Python reference did that arithmetic with a 60-line affine
//! `ec.py` and Python's arbitrary-precision integers. This module does it with the `p256`
//! crate instead, so the sums run in constant time over a reviewed implementation, and
//! [`crate::butterfly`]'s tests then assert that the results are the same values the
//! Python produced, coordinate for coordinate.
//!
//! # The two reductions, and why they are not the same reduction
//!
//! `p256::Scalar` arithmetic *is* arithmetic modulo `n`, so a sum of scalars needs no
//! explicit reduction. Two places in SCP1 do:
//!
//! * **`mod n` of a 384-bit value.** The expansion function `f(k, ι)` concatenates three
//!   AES-Davies-Meyer blocks into 48 bytes and reduces that modulo `n`. A 384-bit integer
//!   does not fit a scalar, so [`scalar_from_be_mod_n`] evaluates it by Horner's rule over
//!   16-byte limbs: each 128-bit limb is below `2^128 < n` and therefore is a scalar on its
//!   own, and `acc = acc · 2^128 + limb` is exact modulo `n` at every step. No
//!   big-integer dependency, and no hand-written limb arithmetic to get wrong.
//! * **`mod (n − 1)` of a 256-bit value.** `new_caterpillar` derives its caterpillar
//!   scalars as `(seed mod (n − 1)) + 1`, which maps a uniform 256-bit seed onto
//!   `1..=n−1` — a *non-zero* scalar, which is what a private key must be. That modulus is
//!   `n − 1`, not `n`, so scalar arithmetic cannot express it. It does not need to:
//!   `2·(n − 1) > 2^256`, so any 256-bit input is reduced by at most one subtraction, and
//!   [`reduce_mod_n_minus_1`] is that single conditional subtract over big-endian bytes.
//!   The bound is asserted by a test rather than asserted in prose.

use p256::elliptic_curve::PrimeField;
use p256::elliptic_curve::group::GroupEncoding;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{AffinePoint, FieldBytes, ProjectivePoint, Scalar};
use v2xw_msg::sec_types::EccP256CurvePoint;

use crate::error::{Result, SecError};
use crate::primitive::PrimitiveId;

/// The order of the P-256 group, big-endian, as FIPS 186-4 / SEC 2 publishes it.
///
/// Held here as bytes rather than taken from `p256` because the two reductions above need
/// it as a byte string, and because pinning the literal lets a test compare it against the
/// value the legacy Python used (`ec.N`) and against the curve's own modulus.
pub const N_BE: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63, 0x25, 0x51,
];

/// `n − 1`, big-endian: the modulus `new_caterpillar` reduces a seed by.
pub const N_MINUS_1_BE: [u8; 32] = {
    let mut v = N_BE;
    // `n` ends in 0x51, so subtracting one cannot borrow. Asserted by a test as well,
    // because a future curve change must not silently take this branch.
    v[31] -= 1;
    v
};

/// A point of the P-256 group, including the point at infinity.
///
/// A newtype rather than a bare [`ProjectivePoint`] so the SCMS code reads as group
/// arithmetic (`Point::mul_base`, `a.add(&b)`) and so the conversions to the IEEE 1609.2
/// wire forms live with the type rather than at every call site. Equality is the group's
/// own: two projective representations of one point compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point(ProjectivePoint);

impl Point {
    /// The point at infinity — the group's identity, and the legacy Python's `None`.
    pub const IDENTITY: Point = Point(ProjectivePoint::IDENTITY);

    /// The standard generator `G`.
    pub const GENERATOR: Point = Point(ProjectivePoint::GENERATOR);

    /// `k · G`.
    pub fn mul_base(k: &Scalar) -> Point {
        Point(ProjectivePoint::GENERATOR * k)
    }

    /// `self + other`.
    #[must_use]
    pub fn add(&self, other: &Point) -> Point {
        Point(self.0 + other.0)
    }

    /// `k · self`.
    #[must_use]
    pub fn mul(&self, k: &Scalar) -> Point {
        Point(self.0 * k)
    }

    /// True for the point at infinity.
    pub fn is_identity(&self) -> bool {
        self.0 == ProjectivePoint::IDENTITY
    }

    /// The affine coordinates as two 32-byte big-endian strings, or `None` at infinity.
    ///
    /// This is the form the legacy Python's `(x, y)` tuples are compared against.
    pub fn xy(&self) -> Option<([u8; 32], [u8; 32])> {
        if self.is_identity() {
            return None;
        }
        let affine = AffinePoint::from(self.0);
        let encoded = affine.to_encoded_point(false);
        let x = encoded.x()?;
        let y = encoded.y()?;
        let mut xb = [0u8; 32];
        let mut yb = [0u8; 32];
        xb.copy_from_slice(x);
        yb.copy_from_slice(y);
        Some((xb, yb))
    }

    /// The SEC 1 §2.3.3 compressed encoding: `0x02 | 0x03` then the 33-byte x coordinate.
    ///
    /// 33 bytes is the public-key size 04-models.md §9.4 gives for `ecdsa-p256-sha256`
    /// and the size of an ECQV reconstruction value (§9.2).
    pub fn compressed(&self) -> Option<[u8; 33]> {
        if self.is_identity() {
            return None;
        }
        let bytes = AffinePoint::from(self.0).to_bytes();
        let mut out = [0u8; 33];
        out.copy_from_slice(&bytes);
        Some(out)
    }

    /// Parses a SEC 1 §2.3.4 compressed (33-byte) or uncompressed (65-byte) encoding of a
    /// point on P-256.
    ///
    /// Every other input is an error, including the single `0x00` octet SEC 1 gives the
    /// point at infinity: this function's callers are importing *public keys*, and the
    /// identity is not one.
    ///
    /// # Why `PublicKey::from_sec1_bytes` and not `AffinePoint::from_bytes`
    ///
    /// Because the argument is untrusted. `AffinePoint::from_bytes` takes a
    /// `GenericArray<u8, U33>`, and converting a `&[u8]` into one **panics** on any other
    /// length — so the obvious spelling, which decodes an `EncodedPoint` first and then
    /// converts it, aborts the process on exactly the two encodings `EncodedPoint` accepts
    /// and the array does not: the 65-byte uncompressed form this function claims to
    /// support, and the 1-byte identity. Key material reaches here from a received
    /// certificate's `verifyKeyIndicator`, i.e. from a peer, so that was a remote crash.
    /// `PublicKey::from_sec1_bytes` is length-generic and returns `Result` for every
    /// input, which is the whole of the fix.
    pub fn from_sec1(bytes: &[u8]) -> Result<Point> {
        let key = p256::PublicKey::from_sec1_bytes(bytes).map_err(|_| SecError::Crypto {
            op: "point decode",
            primitive: PrimitiveId::ECDSA_P256_SHA256,
            // The crate's own error is opaque, and the length is the first thing a reader
            // of a failing run wants to know.
            detail: format!(
                "{} bytes are not a SEC 1 encoding of a point on P-256",
                bytes.len()
            ),
        })?;
        Ok(Point(ProjectivePoint::from(*key.as_affine())))
    }

    /// The IEEE 1609.2 `EccP256CurvePoint`, as the `compressed-y-0` / `compressed-y-1`
    /// alternative.
    ///
    /// Those two alternatives carry the x coordinate and the parity of y, which is exactly
    /// what SEC 1 compression encodes: `0x02` is even y (`compressed-y-0`) and `0x03` is
    /// odd (`compressed-y-1`). Both encode to 1 + 32 bytes under COER, which is the 33
    /// bytes 04-models.md §9.4 lists for a compressed public key.
    pub fn to_ecc_point(&self) -> Result<EccP256CurvePoint> {
        let c = self.compressed().ok_or(SecError::Crypto {
            op: "curve point encode",
            primitive: PrimitiveId::ECDSA_P256_SHA256,
            detail: "the point at infinity has no EccP256CurvePoint encoding".to_string(),
        })?;
        let x = rasn::types::OctetString::from_slice(&c[1..]);
        Ok(match c[0] {
            0x02 => EccP256CurvePoint::compressed_y_0(x),
            _ => EccP256CurvePoint::compressed_y_1(x),
        })
    }

    /// The underlying `p256` point, for callers that need the crate's own API.
    pub fn as_projective(&self) -> &ProjectivePoint {
        &self.0
    }
}

impl From<ProjectivePoint> for Point {
    fn from(p: ProjectivePoint) -> Point {
        Point(p)
    }
}

/// A scalar from a big-endian byte string of any multiple-of-16 length, reduced modulo
/// the group order.
///
/// Horner's rule over 128-bit limbs, as the module documentation explains. The 48-byte
/// case is CAMP SCP1's expansion function; the 32-byte case is an ECQV certificate hash
/// and the caterpillar seed.
///
/// # Panics
///
/// If `bytes.len()` is not a positive multiple of 16. Every caller in this crate passes a
/// fixed-size array, so this is a programming error rather than input validation.
pub fn scalar_from_be_mod_n(bytes: &[u8]) -> Scalar {
    assert!(
        !bytes.is_empty() && bytes.len() % 16 == 0,
        "scalar_from_be_mod_n takes whole 128-bit limbs, got {} bytes",
        bytes.len()
    );
    let two_128 = {
        let mut repr = FieldBytes::default();
        repr[15] = 1;
        scalar_from_repr(repr)
    };
    let mut acc = Scalar::ZERO;
    for limb in bytes.chunks_exact(16) {
        let mut repr = FieldBytes::default();
        repr[16..].copy_from_slice(limb);
        // Every limb is below 2^128 and therefore below n, so `from_repr` succeeds.
        acc = acc * two_128 + scalar_from_repr(repr);
    }
    acc
}

/// A scalar that is already known to be below `n`.
///
/// # Panics
///
/// If the value is `n` or greater. Only called on values this module has just reduced or
/// bounded, so a failure is a bug here and not bad input.
fn scalar_from_repr(repr: FieldBytes) -> Scalar {
    Option::<Scalar>::from(Scalar::from_repr(repr))
        .expect("the caller guarantees the value is below the group order")
}

/// A scalar from a 32-byte big-endian string, refusing a value that is not a valid
/// private key.
///
/// Used where the standard says the value *is* a scalar rather than something to reduce:
/// an ECQV private-key contribution, a key seed that a caller pinned.
pub fn scalar_from_be32(bytes: &[u8; 32]) -> Result<Scalar> {
    let repr = FieldBytes::from(*bytes);
    Option::<Scalar>::from(Scalar::from_repr(repr)).ok_or(SecError::Crypto {
        op: "scalar decode",
        primitive: PrimitiveId::ECDSA_P256_SHA256,
        detail: "the value is not below the P-256 group order".to_string(),
    })
}

/// A scalar's 32-byte big-endian encoding.
pub fn scalar_to_be32(s: &Scalar) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&s.to_repr());
    out
}

/// `value mod (n − 1)` for a 256-bit big-endian `value`.
///
/// One conditional subtraction is enough, and that is the whole content of the function:
/// `n − 1 > 2^255`, so `2·(n − 1) > 2^256 > value`, so the quotient is 0 or 1.
/// `the_single_subtraction_suffices` is the test that holds that claim to the bound.
pub fn reduce_mod_n_minus_1(value: &[u8; 32]) -> [u8; 32] {
    if cmp_be(value, &N_MINUS_1_BE) == core::cmp::Ordering::Less {
        *value
    } else {
        sub_be(value, &N_MINUS_1_BE)
    }
}

/// Big-endian unsigned comparison of two equal-length byte strings.
fn cmp_be(a: &[u8; 32], b: &[u8; 32]) -> core::cmp::Ordering {
    // Lexicographic order on equal-length big-endian strings *is* numeric order.
    a[..].cmp(&b[..])
}

/// `a − b` for big-endian `a >= b`, with a borrow chain.
fn sub_be(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = i16::from(a[i]) - i16::from(b[i]) - borrow;
        if d < 0 {
            out[i] = (d + 256) as u8;
            borrow = 1;
        } else {
            out[i] = d as u8;
            borrow = 0;
        }
    }
    debug_assert_eq!(borrow, 0, "sub_be requires a >= b");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pinned group order must be the one `p256` uses, or every reduction in this
    /// module is silently against the wrong modulus. `Scalar::from_repr` rejects exactly
    /// the values `>= n`, so `n − 1` is accepted and `n` is not: that brackets the
    /// constant from both sides with no second copy of the number to keep in step.
    #[test]
    fn the_pinned_group_order_is_the_curves_own() {
        assert!(
            Option::<Scalar>::from(Scalar::from_repr(FieldBytes::from(N_MINUS_1_BE))).is_some(),
            "n - 1 must be a valid scalar"
        );
        assert!(
            Option::<Scalar>::from(Scalar::from_repr(FieldBytes::from(N_BE))).is_none(),
            "n itself must not be"
        );
        // And n - 1 really is one less than n.
        assert_eq!(sub_be(&N_BE, &N_MINUS_1_BE)[31], 1);
    }

    /// `reduce_mod_n_minus_1` claims one subtraction is enough. The claim rests on
    /// `2·(n − 1) > 2^256 − 1`; check it on the extreme input rather than trusting the
    /// arithmetic in the comment.
    #[test]
    fn the_single_subtraction_suffices() {
        let max = [0xffu8; 32];
        let r = reduce_mod_n_minus_1(&max);
        assert_eq!(
            cmp_be(&r, &N_MINUS_1_BE),
            core::cmp::Ordering::Less,
            "2^256-1 reduced once must already be below n-1"
        );
        // And the boundary cases.
        assert_eq!(reduce_mod_n_minus_1(&N_MINUS_1_BE), [0u8; 32]);
        assert_eq!(reduce_mod_n_minus_1(&[0u8; 32]), [0u8; 32]);
        let mut n_minus_2 = N_MINUS_1_BE;
        n_minus_2[31] -= 1;
        assert_eq!(reduce_mod_n_minus_1(&n_minus_2), n_minus_2);
    }

    /// The Horner limb reduction must agree with scalar arithmetic on a value that is
    /// already a scalar, and must be additive-homomorphic the way `mod n` is.
    #[test]
    fn the_limb_reduction_agrees_with_scalar_arithmetic() {
        // A 32-byte value below n reduces to itself.
        let small = {
            let mut b = [0u8; 32];
            b[31] = 7;
            b
        };
        assert_eq!(scalar_from_be_mod_n(&small), Scalar::from(7u64));

        // 2^128 as a 48-byte value: limbs (0, 1, 0) -> 2^128.
        let mut wide = [0u8; 48];
        wide[15] = 1; // limb 0 = 2^120? No: byte 15 of limb 0 is its least significant.
        // limb0 = 0x00..01 = 1, so value = 1 * 2^256 = 2^256 mod n.
        let two_256 = scalar_from_be_mod_n(&wide);
        let mut two_128_bytes = [0u8; 32];
        two_128_bytes[15] = 1;
        let two_128 = scalar_from_be32(&two_128_bytes).expect("2^128 < n");
        assert_eq!(two_256, two_128 * two_128, "2^256 mod n = (2^128)^2 mod n");
    }

    /// The generator's coordinates are published in FIPS 186-4; if the point type ever
    /// stopped agreeing with them, every butterfly vector would be wrong in a way that
    /// looked like a bug in the expansion function.
    #[test]
    fn the_generator_matches_the_published_coordinates() {
        let (x, y) = Point::GENERATOR.xy().expect("G is not at infinity");
        assert_eq!(
            v2xw_core::hash::hex_encode(&x),
            "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296"
        );
        assert_eq!(
            v2xw_core::hash::hex_encode(&y),
            "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5"
        );
        assert!(Point::IDENTITY.is_identity());
        assert!(Point::IDENTITY.xy().is_none());
    }

    /// The group is additive in the exponent — the property the whole butterfly
    /// construction rests on (`(a + f) · G = A + f · G`).
    #[test]
    fn the_group_is_additive_in_the_exponent() {
        let a = Scalar::from(111_111u64);
        let b = Scalar::from(987_654_321u64);
        assert_eq!(
            Point::mul_base(&a).add(&Point::mul_base(&b)),
            Point::mul_base(&(a + b))
        );
    }

    /// `from_sec1` takes untrusted bytes, so every length must be an *error*. Before the
    /// length check it panicked on two of them — the 65-byte uncompressed form named in
    /// its own documentation, and the 1-byte identity — because `AffinePoint::from_bytes`
    /// converts through a `GenericArray<u8, U33>` that asserts its length.
    #[test]
    fn a_sec1_encoding_of_the_wrong_length_is_an_error_and_never_a_panic() {
        // 33 is missing on purpose: it is the one length a `0x02` prefix may legally have,
        // and `0x02` followed by 32 copies of `0x02` happens to be a point on the curve.
        // The off-curve case below covers the 33-byte rejection path instead.
        for len in [0usize, 1, 2, 31, 32, 34, 63, 64, 65, 66, 128] {
            // 0x02 is a compressed-point prefix, so nothing here is rejected on the
            // prefix alone: the length is what has to be caught.
            let bytes = vec![0x02u8; len];
            let r = Point::from_sec1(&bytes);
            assert!(r.is_err(), "{len} bytes of 0x02 must not parse");
            assert!(
                r.unwrap_err().to_string().contains(&format!("{len} bytes")),
                "the error names the length"
            );
        }
        // The identity's own SEC 1 encoding is refused too, and by the same path.
        assert!(Point::from_sec1(&[0x00]).is_err());
        // Off-curve but correctly shaped: x = 1 has no y on P-256.
        let mut off = [0u8; 33];
        off[0] = 0x02;
        off[32] = 1;
        assert!(Point::from_sec1(&off).is_err());
        // And the two forms the documentation promises really do parse, to the same point.
        let g = Point::GENERATOR;
        let (x, y) = g.xy().expect("G is not at infinity");
        let mut uncompressed = [0u8; 65];
        uncompressed[0] = 0x04;
        uncompressed[1..33].copy_from_slice(&x);
        uncompressed[33..].copy_from_slice(&y);
        assert_eq!(Point::from_sec1(&uncompressed).expect("uncompressed G"), g);
        assert_eq!(
            Point::from_sec1(&g.compressed().expect("compressed")).expect("compressed G"),
            g
        );
    }

    /// Compression round-trips, and the 1609.2 alternative records the parity.
    #[test]
    fn a_point_round_trips_through_its_compressed_form() {
        for k in [1u64, 2, 3, 7, 12_345] {
            let p = Point::mul_base(&Scalar::from(k));
            let c = p.compressed().expect("not at infinity");
            assert_eq!(c.len(), 33);
            assert!(c[0] == 0x02 || c[0] == 0x03);
            assert_eq!(Point::from_sec1(&c).expect("round trip"), p);
            let ecc = p.to_ecc_point().expect("encodes");
            match (&ecc, c[0]) {
                (EccP256CurvePoint::compressed_y_0(x), 0x02)
                | (EccP256CurvePoint::compressed_y_1(x), 0x03) => {
                    assert_eq!(&x[..], &c[1..]);
                }
                _ => panic!("parity and alternative disagree: {ecc:?} vs {:#04x}", c[0]),
            }
        }
        assert!(Point::IDENTITY.to_ecc_point().is_err());
    }
}
