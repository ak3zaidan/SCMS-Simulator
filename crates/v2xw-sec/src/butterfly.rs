//! Butterfly key expansion — CAMP SCP1.
//!
//! A port of `legacy/scms_sim_ref/scms_core/butterfly.py`, algorithm for algorithm, with
//! the Python's own outputs as the acceptance vectors (see the crate's
//! `tests/legacy_vectors.rs`).
//!
//! # What the construction buys
//!
//! One small upload from the device — two "caterpillar" public keys `A = a·G` and
//! `P = p·G`, and two AES-128 expansion keys `ck` and `ek` — lets the Registration
//! Authority generate an arbitrary number of per-period "cocoon" keys, and lets the
//! Pseudonym Certificate Authority issue certificates against them, such that:
//!
//! * the **RA never learns the certified public key**, because the PCA adds a secret
//!   random `c` that the RA never sees;
//! * the **PCA cannot link two certificates to one device**, because cocoon keys are
//!   pseudorandom without `ck` and the RA shuffles the requests;
//! * the **device can still re-derive every usable private key** from material it
//!   already has, so nothing has to be sent back to it in the clear.
//!
//! # The expansion function
//!
//! With `ι = (i, j)` packed into a 128-bit AES input block
//! `x = prefix³² ‖ i³² ‖ j³² ‖ 0³²` — `prefix = 0³²` for `f₁` (signing) and `1³²` for
//! `f₂` (encryption) —
//!
//! ```text
//! f(k, ι) = ( AES(k, x+1) ⊕ (x+1) ‖ AES(k, x+2) ⊕ (x+2) ‖ AES(k, x+3) ⊕ (x+3) ) mod n
//! ```
//!
//! Three blocks because one 128-bit block is not enough entropy for a 256-bit scalar, and
//! `x+1 … x+3` rather than `x … x+2` so that the all-zero block never appears. The
//! additions are modulo `2¹²⁸`, i.e. they carry across the whole block; the reduction is
//! modulo the P-256 group order and is why [`crate::ec::scalar_from_be_mod_n`] exists.
//!
//! The prefix for `f₂` is `0xFFFFFFFF`, four bytes of ones. The legacy docstring writes
//! it "1³²", and the legacy code writes `b"\xff\xff\xff\xff"`; those agree, and this port
//! follows the code. Nothing distinguishes the two readings cryptographically — the
//! prefix exists only to keep the signing and encryption expansions in separate domains —
//! but a port that chose the other one would produce different keys from the reference, so
//! it is stated rather than assumed.
//!
//! # The explicit-certificate variant
//!
//! Implemented here, and the identity the tests assert:
//!
//! ```text
//! RA:     B = A + f₁(ck, ι)·G                      (the cocoon signing key)
//! PCA:    certified = B + c·G,  C = c·G            (c secret to the PCA)
//! device: d = a + f₁(ck, ι) + c   (mod n)          and  d·G == certified
//! ```
//!
//! In the full protocol the PCA also encrypts `(certificate, c)` to the cocoon encryption
//! key `Q` and signs the ciphertext; this module stops at the key arithmetic, which is the
//! part the simulator's credential-management protocol needs and the part that must be
//! exactly right. The implicit-certificate variant of SCP1 is a different final step
//! (the private key is `e·k + r`, which is [`crate::crypto::ecqv`]'s subject) and is not
//! this function.

use p256::Scalar;
use v2xw_core::hash::sha256;

use crate::aes128;
use crate::ec::{self, Point};
use crate::error::{Result, SecError};
use crate::primitive::PrimitiveId;

/// Bytes of device seed material [`Caterpillar::from_seed`] needs.
///
/// 64: two 32-byte scalars. The legacy reference accepts anything longer and hashes the
/// whole of it into `ck` and `ek`, which this port reproduces, because the caterpillar
/// vectors depend on it.
pub const SEED_BYTES: usize = 64;

/// An AES-128 butterfly expansion key (`ck` for signing, `ek` for encryption).
pub type ExpansionKey = [u8; aes128::KEY];

/// The signing-key expansion value `f₁(ck, (i, j))`.
pub fn f1(ck: &ExpansionKey, i: u32, j: u32) -> Scalar {
    expand(ck, &x_block(false, i, j))
}

/// The encryption-key expansion value `f₂(ek, (i, j))`.
pub fn f2(ek: &ExpansionKey, i: u32, j: u32) -> Scalar {
    expand(ek, &x_block(true, i, j))
}

/// The 128-bit AES input block `prefix³² ‖ i³² ‖ j³² ‖ 0³²`.
fn x_block(prefix_one: bool, i: u32, j: u32) -> [u8; aes128::BLOCK] {
    let mut b = [0u8; aes128::BLOCK];
    if prefix_one {
        b[0..4].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    }
    b[4..8].copy_from_slice(&i.to_be_bytes());
    b[8..12].copy_from_slice(&j.to_be_bytes());
    // b[12..16] stays zero.
    b
}

/// The three-block expansion, reduced modulo the group order.
fn expand(key: &ExpansionKey, x: &[u8; aes128::BLOCK]) -> Scalar {
    let base = u128::from_be_bytes(*x);
    let mut wide = [0u8; 48];
    for m in 1u128..=3 {
        // Modulo 2^128, exactly as the reference's `& _MASK128`.
        let xm = base.wrapping_add(m).to_be_bytes();
        let block = aes128::davies_meyer(key, &xm);
        let off = ((m - 1) as usize) * aes128::BLOCK;
        wide[off..off + aes128::BLOCK].copy_from_slice(&block);
    }
    ec::scalar_from_be_mod_n(&wide)
}

/// The device's butterfly request material.
///
/// `a` and `p` never leave the device; `A`, `P`, `ck` and `ek` go to the RA. The struct
/// holds all six because it *is* the device's own state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caterpillar {
    a: Scalar,
    p: Scalar,
    ck: ExpansionKey,
    ek: ExpansionKey,
}

impl Caterpillar {
    /// Derives a caterpillar deterministically from at least [`SEED_BYTES`] of seed.
    ///
    /// The scalars are `(seed mod (n − 1)) + 1`, which lands them in `1..=n−1` — non-zero,
    /// as a private key must be. The expansion keys are `SHA-256("ck" ‖ seed)` and
    /// `SHA-256("ek" ‖ seed)` truncated to 128 bits, over the *whole* seed, so the entire
    /// request is reproducible from one value. All four derivations are the reference
    /// implementation's, byte for byte.
    pub fn from_seed(seed: &[u8]) -> Result<Caterpillar> {
        if seed.len() < SEED_BYTES {
            return Err(SecError::BadLength {
                what: "butterfly caterpillar seed",
                expected: SEED_BYTES,
                got: seed.len(),
            });
        }
        let a = scalar_from_seed_half(&seed[0..32])?;
        let p = scalar_from_seed_half(&seed[32..64])?;
        let mut ck = [0u8; aes128::KEY];
        let mut ek = [0u8; aes128::KEY];
        ck.copy_from_slice(&sha256(&[b"ck".as_slice(), seed].concat())[..aes128::KEY]);
        ek.copy_from_slice(&sha256(&[b"ek".as_slice(), seed].concat())[..aes128::KEY]);
        Ok(Caterpillar { a, p, ck, ek })
    }

    /// Builds a caterpillar from explicit material, for a test or a replayed vector.
    pub fn from_parts(a: Scalar, p: Scalar, ck: ExpansionKey, ek: ExpansionKey) -> Caterpillar {
        Caterpillar { a, p, ck, ek }
    }

    /// The caterpillar signing private scalar `a`.
    pub fn a(&self) -> &Scalar {
        &self.a
    }

    /// The caterpillar encryption private scalar `p`.
    pub fn p(&self) -> &Scalar {
        &self.p
    }

    /// The signing expansion key `ck`.
    pub fn ck(&self) -> &ExpansionKey {
        &self.ck
    }

    /// The encryption expansion key `ek`.
    pub fn ek(&self) -> &ExpansionKey {
        &self.ek
    }

    /// The caterpillar signing public key `A = a·G`, which the device uploads.
    pub fn signing_public(&self) -> Point {
        Point::mul_base(&self.a)
    }

    /// The caterpillar encryption public key `P = p·G`, which the device uploads.
    pub fn encryption_public(&self) -> Point {
        Point::mul_base(&self.p)
    }

    /// The usable signing private key for period `i`, index `j`: `a + f₁(ck, ι) + c`.
    ///
    /// `c` is the PCA's secret randomiser, which reaches the device encrypted to its
    /// cocoon encryption key. This is the explicit-certificate variant.
    pub fn signing_private(&self, i: u32, j: u32, c: &Scalar) -> Scalar {
        self.a + f1(&self.ck, i, j) + c
    }

    /// The cocoon encryption private key `q = p + f₂(ek, ι)`.
    pub fn encryption_private(&self, i: u32, j: u32) -> Scalar {
        self.p + f2(&self.ek, i, j)
    }
}

/// `(seed mod (n − 1)) + 1`.
fn scalar_from_seed_half(half: &[u8]) -> Result<Scalar> {
    let mut be = [0u8; 32];
    be.copy_from_slice(half);
    let reduced = ec::reduce_mod_n_minus_1(&be);
    // `reduced` is in `0..=n−2`, so it is a valid scalar and `+1` cannot reach `n`.
    Ok(ec::scalar_from_be32(&reduced)? + Scalar::ONE)
}

/// The RA's expansion step: the per-`(i, j)` cocoon keys.
///
/// `B = A + f₁(ck, ι)·G` for signing and `Q = P + f₂(ek, ι)·G` for encryption. The RA can
/// do this because it holds the two public caterpillar keys and the two expansion keys —
/// and it learns nothing about the certified key, because the PCA has yet to add `c`.
pub fn ra_cocoon_keys(
    signing_public: &Point,
    encryption_public: &Point,
    ck: &ExpansionKey,
    ek: &ExpansionKey,
    i: u32,
    j: u32,
) -> (Point, Point) {
    let b = signing_public.add(&Point::mul_base(&f1(ck, i, j)));
    let q = encryption_public.add(&Point::mul_base(&f2(ek, i, j)));
    (b, q)
}

/// The PCA's explicit-certificate step: certify `B + c·G` and return `C = c·G`.
///
/// The returned certified key is what goes into the certificate's
/// `verifyKeyIndicator.verificationKey`; `C` is what the device needs — in the full
/// protocol it receives `c` itself, encrypted to `Q`, and `C` is how a test checks the
/// arithmetic without modelling that encryption.
pub fn pca_certify_explicit(cocoon_signing: &Point, c: &Scalar) -> (Point, Point) {
    let big_c = Point::mul_base(c);
    (cocoon_signing.add(&big_c), big_c)
}

/// Checks that a device-derived private key matches a PCA-certified public key.
///
/// The identity the whole construction rests on. Returned as a `bool` rather than
/// asserted so the credential-management protocol can *detect* a mismatch — a PCA that
/// certified the wrong cocoon key, or a device that expanded the wrong `(i, j)` — instead
/// of panicking inside a simulation run.
pub fn derived_key_matches(private: &Scalar, certified_public: &Point) -> bool {
    Point::mul_base(private) == *certified_public
}

/// The scalar a caller supplies as the PCA's secret `c`, from 32 big-endian bytes.
///
/// A thin wrapper over [`ec::scalar_from_be32`] that names the ECQV/butterfly context in
/// the error, because "the value is not below the group order" is otherwise a puzzling
/// thing for a certificate-issuance path to say.
pub fn secret_randomiser(be32: &[u8; 32]) -> Result<Scalar> {
    ec::scalar_from_be32(be32).map_err(|_| SecError::Crypto {
        op: "butterfly PCA randomiser",
        primitive: PrimitiveId::ECQV_P256,
        detail: "c must be a scalar below the P-256 group order".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::hash::hex_encode;

    fn seed_range64() -> Vec<u8> {
        (0u8..64).collect()
    }

    /// The legacy `test_caterpillar_deterministic`: one seed, one caterpillar.
    #[test]
    fn a_caterpillar_is_a_function_of_its_seed() {
        let s = seed_range64();
        let c1 = Caterpillar::from_seed(&s).expect("64 bytes");
        let c2 = Caterpillar::from_seed(&s).expect("64 bytes");
        assert_eq!(c1, c2);
        assert!(
            Caterpillar::from_seed(&s[..63]).is_err(),
            "short seed refused"
        );
    }

    /// The legacy `test_expansion_values_vary_and_are_deterministic`: 15 distinct values
    /// over a 3x5 grid, and each one stable.
    #[test]
    fn the_expansion_values_are_distinct_per_period_and_deterministic() {
        let ck: ExpansionKey = core::array::from_fn(|i| i as u8);
        let mut seen: Vec<[u8; 32]> = Vec::new();
        for i in 0..3 {
            for j in 0..5 {
                let v = ec::scalar_to_be32(&f1(&ck, i, j));
                assert!(!seen.contains(&v), "collision at ({i}, {j})");
                seen.push(v);
            }
        }
        assert_eq!(seen.len(), 15);
        assert_eq!(f1(&ck, 1, 2), f1(&ck, 1, 2));
        // And the two expansions are in different domains: same key, same (i, j),
        // different value, because the 32-bit prefix differs.
        assert_ne!(f1(&ck, 1, 2), f2(&ck, 1, 2));
    }

    /// The prefix really is the only difference between `f₁` and `f₂`, and it is four
    /// bytes of `0xff` — pinned because choosing the other reading of "1³²" would give a
    /// different, silently wrong key schedule.
    #[test]
    fn the_two_expansions_differ_only_in_a_four_byte_prefix() {
        let b1 = x_block(false, 0x0102_0304, 0x0506_0708);
        let b2 = x_block(true, 0x0102_0304, 0x0506_0708);
        assert_eq!(hex_encode(&b1), "00000000010203040506070800000000");
        assert_eq!(hex_encode(&b2), "ffffffff010203040506070800000000");
        assert_eq!(b1[4..], b2[4..]);
    }

    /// The legacy `test_butterfly_signing_key_identity` — the heart of the construction.
    #[test]
    fn a_device_rederives_exactly_the_key_the_pca_certified() {
        let cat = Caterpillar::from_seed(&seed_range64()).expect("64 bytes");
        let (big_a, big_p) = (cat.signing_public(), cat.encryption_public());
        for i in 0..2u32 {
            for j in 0..4u32 {
                let (b, q) = ra_cocoon_keys(&big_a, &big_p, cat.ck(), cat.ek(), i, j);
                assert_ne!(b, big_a, "the RA must actually have expanded the key");
                assert_ne!(q, big_p);
                let c = Scalar::from(0x1234_5678_90AB_CDEFu64 + u64::from(i) * 7 + u64::from(j));
                let (certified, _c_point) = pca_certify_explicit(&b, &c);
                let d = cat.signing_private(i, j, &c);
                assert!(
                    derived_key_matches(&d, &certified),
                    "d·G != certified at ({i}, {j})"
                );
                let qk = cat.encryption_private(i, j);
                assert!(derived_key_matches(&qk, &q), "q·G != Q at ({i}, {j})");
            }
        }
    }

    /// The legacy `test_ra_cannot_predict_certified_key_without_c`.
    #[test]
    fn the_ra_cannot_predict_the_certified_key_without_the_pcas_secret() {
        let cat = Caterpillar::from_seed(&[9u8; 64]).expect("64 bytes");
        let (b, _q) = ra_cocoon_keys(
            &cat.signing_public(),
            &cat.encryption_public(),
            cat.ck(),
            cat.ek(),
            0,
            0,
        );
        let (certified, big_c) = pca_certify_explicit(&b, &Scalar::from(424_242u64));
        assert_ne!(b, certified, "B alone is not the certified key");
        assert_eq!(certified, b.add(&big_c));
    }

    /// The expansion is over `x+1 … x+3`, not `x … x+2`: the all-zero input block never
    /// reaches the cipher. Checked structurally rather than by inspection, because the
    /// off-by-one would be invisible in every other test — it would simply produce a
    /// different, self-consistent key schedule.
    #[test]
    fn the_expansion_never_feeds_the_zero_block_to_the_cipher() {
        let ck: ExpansionKey = [0u8; 16];
        // The (0, 0) block is all zeros, so the three cipher inputs must be 1, 2, 3.
        let zero_block = x_block(false, 0, 0);
        assert_eq!(zero_block, [0u8; 16]);
        let expected = {
            let mut wide = [0u8; 48];
            for m in 1u128..=3 {
                let xm = m.to_be_bytes();
                let blk = aes128::davies_meyer(&ck, &xm);
                wide[((m - 1) as usize) * 16..][..16].copy_from_slice(&blk);
            }
            ec::scalar_from_be_mod_n(&wide)
        };
        assert_eq!(f1(&ck, 0, 0), expected);
    }

    /// The block addition carries across the whole 128 bits and wraps, which is what
    /// `& _MASK128` does in the reference. Reachable with `j = u32::MAX`, where `x`'s low
    /// 32 bits are zero but the `j` field is saturated — a naive per-field increment
    /// would differ.
    #[test]
    fn the_block_addition_is_modulo_two_to_the_128() {
        let x = x_block(true, u32::MAX, u32::MAX);
        let base = u128::from_be_bytes(x);
        assert_eq!(base.wrapping_add(1).to_be_bytes()[15], 1);
        // And the extreme: an all-ones block wraps to 0, 1, 2.
        let all_ones = u128::MAX;
        assert_eq!(all_ones.wrapping_add(1), 0);
        assert_eq!(all_ones.wrapping_add(3), 2);
    }

    /// A caterpillar scalar is never zero, whatever the seed — that is the reason for the
    /// `mod (n − 1)` then `+1` dance rather than a plain `mod n`.
    #[test]
    fn a_caterpillar_scalar_is_never_zero() {
        for seed in [[0u8; 64], [0xffu8; 64]] {
            let cat = Caterpillar::from_seed(&seed).expect("64 bytes");
            assert_ne!(*cat.a(), Scalar::ZERO);
            assert_ne!(*cat.p(), Scalar::ZERO);
            assert!(!cat.signing_public().is_identity());
            assert!(!cat.encryption_public().is_identity());
        }
        // The all-zero seed is the boundary: (0 mod (n-1)) + 1 = 1.
        let cat = Caterpillar::from_seed(&[0u8; 64]).expect("64 bytes");
        assert_eq!(*cat.a(), Scalar::ONE);
        assert_eq!(cat.signing_public(), Point::GENERATOR);
    }

    /// `secret_randomiser` refuses a value at or above the group order rather than
    /// silently reducing it, because a PCA that reduced `c` would certify a key the device
    /// cannot re-derive.
    #[test]
    fn the_pca_randomiser_refuses_an_out_of_range_value() {
        assert!(secret_randomiser(&ec::N_MINUS_1_BE).is_ok());
        assert!(secret_randomiser(&ec::N_BE).is_err());
        assert!(secret_randomiser(&[0xffu8; 32]).is_err());
    }
}
