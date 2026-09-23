//! The two crypto backends, and the equivalence between them (invariant I-S1).
//!
//! [`Real`] executes genuine cryptography: ECDSA over NIST P-256 with RFC 6979
//! deterministic nonces, real SHA-256, and SEC 4 ECQV implicit-certificate
//! reconstruction. [`Modeled`] executes none: a signature is a token derived from the key
//! and the message, verification compares tokens, and sizes come from the descriptor
//! tables of [`crate::primitive`].
//!
//! # Invariant I-S1, and what makes it true rather than hoped for
//!
//! > For any run, `Real` and `Modeled` produce identical verification outcomes, identical
//! > sizes, and identical event logs except the `crypto_mode` manifest field.
//!
//! Both halves are structural, not statistical:
//!
//! * **Outcomes.** A `Modeled` token is `expand(commitment ‖ digest)`, where the
//!   commitment is `SHA-256(label ‖ seed)` — a function of the key's *secret*, recorded
//!   when the backend generated it. So a token verifies exactly when it was produced by
//!   *that* key over *that* digest, which is also exactly when a real ECDSA signature
//!   verifies. Neither backend can say yes to a message that was not signed, and neither
//!   can say no to one that was. A tampered payload changes `digest` and both refuse; a
//!   substituted key changes the commitment and both refuse; truncated bytes fail the
//!   length check in one and `Signature::from_slice` in the other.
//!
//!   The commitment is recorded against the key's public material in a directory every
//!   backend instance shares (`modeled_directory`), and that is not an implementation
//!   detail: every node in a run owns its own backend, so every verification in a run is
//!   one instance checking a key another instance generated. Held per instance — which it
//!   was until 2026-09-22 — a receiver found no commitment for any peer and answered
//!   `false` for every message from every peer, which is exactly the outcome divergence
//!   I-S1 forbids. The Phase 2 scenario runs `crypto_mode: modeled`, and it measured
//!   98.23 % of receptions `Invalid`.
//!
//!   The commitment is also what makes the *forgery* half true by construction rather
//!   than by API accident. Keying the token on public material instead — which is what this did
//!   until v2.0.0 of the modelled card — let anyone holding a peer's certificate and the
//!   digest compute a token `verify_prehashed` accepts; nothing reached it only because
//!   signing needs a [`KeyHandle`] carrying a secret. A forgery model in `v2xw-threat`
//!   would have succeeded modelled and failed real. See [`modeled_token`].
//!
//!   Two more places where the equality is not free, and is therefore arranged:
//!
//!   * **Signature malleability.** `(r, n − s)` is a second valid ECDSA signature over the
//!     same message, and a stock verifier accepts it. A modelled token compared byte for
//!     byte does not. So [`Real`] emits only the low-`s` representative and refuses the
//!     high-`s` one — see [`CryptoBackend::verify_prehashed`] on `Real`.
//!   * **Invalid key material.** Curve membership is a question `Modeled` cannot answer,
//!     because its public material is deliberately not a point. So neither backend asks
//!     it: [`check_public_material`] is the single shape check both use, and material that
//!     is not a point fails every *verification* instead of the import, identically in
//!     both modes.
//! * **Sizes.** A `Modeled` token is exactly `sig_bytes` long, from the same descriptor
//!   the real primitive's size is published in, and a `Modeled` public key is 33 bytes
//!   beginning `0x02` or `0x03` — the same SEC 1 shape a real compressed point has. So
//!   every ASN.1 alternative chosen downstream, and therefore every encoded length, is
//!   the same in both modes. Nothing in the envelope branches on the mode.
//!
//! The crate's `tests/equivalence.rs` runs a scenario-shaped sequence of operations —
//! keygen, sign, verify, tampered payload, wrong key, truncated signature, a signature
//! malleated to `(r, n − s)`, off-curve and misshapen key material, replay through a
//! second backend instance — through both backends and asserts the outcome sequence and
//! the size sequence are equal element for element.
//!
//! # Why deterministic signatures
//!
//! RFC 6979 derives the ECDSA nonce from the key and the message instead of from
//! entropy, so signing is a pure function and a run reproduces byte for byte (ADR 0004).
//! A randomised ECDSA would make `Real` mode non-reproducible and would break the
//! manifest digest, which is not a trade this project makes. `p256`'s `SigningKey` is
//! RFC 6979 by default, so this is a property of the dependency rather than something
//! this module arranges.
//!
//! # Where key material comes from
//!
//! From the scenario RNG, [`RngDomain::Crypto`] keyed on the owning node — in **both**
//! modes. Determinism leaves no alternative: a reproducible run needs reproducible keys,
//! and the counter-based streams are the only reproducible source the engine has. The
//! domain's own documentation in `v2xw-core` says it is "never used for real keys in
//! `crypto_mode: real`"; that sentence is about *modelled* nonce and digest material and
//! cannot be read as forbidding a deterministic key seed, because the alternative it would
//! leave — real entropy — contradicts the determinism contract the same crate enforces.
//! The discrepancy is recorded rather than silently resolved.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use p256::Scalar;
use p256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use v2xw_core::card::{
    Determinism, Family, ModelCard, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::hash::sha256;
use v2xw_core::ids::NodeId;
use v2xw_core::manifest::CryptoMode;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::Duration;

use crate::ec::{self, Point};
use crate::error::{Result, SecError};
use crate::primitive::{PrimitiveCatalogue, PrimitiveFamily, PrimitiveId, PrimitiveOpKind};

/// The model id of the real backend.
pub const REAL_ID: &str = "crypto-backend/real";

/// The model id of the modelled backend.
pub const MODELED_ID: &str = "crypto-backend/modeled";

/// Bytes of a compressed P-256 public key, and of the modelled stand-in for one.
pub const PUBLIC_MATERIAL_BYTES: usize = 33;

/// The check **both** backends apply to imported public key material, and the only one.
///
/// Shape, not curve membership: 33 bytes whose first is `0x02` or `0x03`, the SEC 1
/// compressed form. That keeps every encoded certificate the same size in both modes,
/// which is half of invariant I-S1.
///
/// # Why the real backend does not also check the point is on the curve
///
/// It used to, and that was the bug. Curve membership is a question the modelled backend
/// cannot answer — [`modeled_public_material`] deliberately produces 33 bytes that are
/// *not* a point, so a curve check in `Modeled::import_public` would reject the modelled
/// backend's own keys. With the check on one side only, crafted material from a received
/// certificate's `verifyKeyIndicator` was an `Err` in real mode and an `Ok` in modelled
/// mode: the same run took different branches depending on `crypto_mode`, which is exactly
/// what I-S1 forbids.
///
/// So the decision lives here, in one place, and nothing is lost. Material that is not a
/// point still fails every verification: the real backend's
/// `VerifyingKey::from_sec1_bytes` refuses it and answers `false`, and the modelled
/// backend answers `false` because no token over that material exists. Both modes reject
/// the message, at the same step, with the same outcome — later than before, and
/// identically, which is the trade I-S1 asks for.
fn check_public_material(
    primitive: PrimitiveId,
    material: &[u8],
) -> Result<[u8; PUBLIC_MATERIAL_BYTES]> {
    if material.len() != PUBLIC_MATERIAL_BYTES {
        return Err(SecError::BadLength {
            what: "public key material",
            expected: PUBLIC_MATERIAL_BYTES,
            got: material.len(),
        });
    }
    if material[0] != 0x02 && material[0] != 0x03 {
        return Err(SecError::Crypto {
            op: "public key import",
            primitive,
            detail: format!(
                "compressed key material begins 0x02 or 0x03, not {:#04x}",
                material[0]
            ),
        });
    }
    let mut out = [0u8; PUBLIC_MATERIAL_BYTES];
    out.copy_from_slice(material);
    Ok(out)
}

/// A key's identity within one backend.
///
/// Both backends number from 1 and increment by one per [`CryptoBackend::keygen`], so a
/// scenario that generates the same keys in the same order gets the same ids in both
/// modes — which is what lets a recorded event log be compared across modes (I-S1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct KeyId(pub u64);

impl core::fmt::Display for KeyId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "k{}", self.0)
    }
}

/// A handle on a private key held by a backend.
///
/// Carries the issuing backend's model id so a handle cannot be used with the other
/// backend by accident. Both backends number their keys from the same counter, so without
/// that field a handle from one would name a *different, existing* key in the other and
/// verification would return a wrong answer instead of an error
/// ([`SecError::WrongBackend`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyHandle {
    /// The key's id within its backend.
    pub id: KeyId,
    /// The primitive it was generated for.
    pub primitive: PrimitiveId,
    /// The node that owns it.
    pub owner: NodeId,
    /// The model id of the backend that issued it.
    pub backend: &'static str,
}

/// A handle on a public key held by a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PubHandle {
    /// The key's id within its backend.
    pub id: KeyId,
    /// The primitive it belongs to.
    pub primitive: PrimitiveId,
    /// The node that owns it.
    pub owner: NodeId,
    /// The model id of the backend that issued it.
    pub backend: &'static str,
}

/// A signature, real or modelled.
///
/// One type for both because everything downstream — the envelope, the recorder, a size
/// accounting — cares only about the bytes and their length, and a two-variant enum would
/// put a `match` on the mode into every one of those places. That `match` is precisely
/// what invariant I-S1 says must not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigToken {
    /// The primitive that produced it.
    pub primitive: PrimitiveId,
    /// The raw signature bytes: `r ‖ s` for real ECDSA, the modelled token otherwise.
    /// Always exactly the descriptor's `sig_bytes` long.
    pub bytes: Vec<u8>,
}

impl SigToken {
    /// The signature's length in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// True when the signature carries no bytes, which no backend produces.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The `r` half of an ECDSA signature: the first 32 bytes.
    pub fn r(&self) -> Result<[u8; 32]> {
        self.half(0)
    }

    /// The `s` half of an ECDSA signature: the second 32 bytes.
    pub fn s(&self) -> Result<[u8; 32]> {
        self.half(32)
    }

    /// The IEEE 1609.2 `Signature` carrying this token.
    ///
    /// `rSig` uses the `x-only` alternative, which is what 1609.2 §6.3.29 specifies for a
    /// generated ECDSA signature: `r` is an integer modulo the group order and carries no
    /// y coordinate. All three 32-byte alternatives encode to the same 33 bytes, so the
    /// choice does not affect any size — it affects whether a conformant receiver accepts
    /// the bytes.
    ///
    /// Lives on the token rather than on the envelope because a certificate signature
    /// needs the same conversion, and two copies of "which alternative does `rSig` use"
    /// is one copy too many.
    pub fn to_ieee1609_signature(&self) -> Result<v2xw_msg::sec_types::Signature> {
        use v2xw_msg::sec_types::ieee1609_dot2_base_types::{
            EccP256CurvePoint, EcdsaP256Signature,
        };
        if self.primitive != PrimitiveId::ECDSA_P256_SHA256
            && self.primitive != PrimitiveId::ECQV_P256
        {
            // IEEE 1609.2's `Signature` CHOICE has no post-quantum alternative, and no
            // extension defining one is standardised. Refusing is the honest answer;
            // silently encoding a 2,420-byte ML-DSA signature in an ECDSA alternative
            // would produce a size that is right and bytes that are meaningless.
            return Err(SecError::UnsupportedPrimitive {
                backend: "envelope",
                primitive: self.primitive,
            });
        }
        Ok(v2xw_msg::sec_types::Signature::ecdsaNistP256Signature(
            EcdsaP256Signature::new(
                EccP256CurvePoint::x_only(rasn::types::OctetString::from_slice(&self.r()?)),
                rasn::types::OctetString::from_slice(&self.s()?),
            ),
        ))
    }

    fn half(&self, at: usize) -> Result<[u8; 32]> {
        if self.bytes.len() < at + 32 {
            return Err(SecError::BadLength {
                what: "ECDSA signature half",
                expected: at + 32,
                got: self.bytes.len(),
            });
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&self.bytes[at..at + 32]);
        Ok(out)
    }
}

/// What a cryptographic backend *is*: its mode, what it can do, and what its keys look
/// like.
///
/// Split out from [`CryptoBackend`] for a concrete reason rather than for tidiness. The
/// line is **whether the method needs the simulation context** — a clock and the
/// deterministic RNG streams — and not whether it mutates: importing a peer's public key
/// takes `&mut self` and belongs here, because it needs no clock. Methods that mention no
/// context cannot live on the generic trait, because the parameter would then be
/// unconstrained at every call site and `backend.public_material(&pk)` would not compile
/// without a turbofish naming a context type the caller has no interest in.
pub trait CryptoBackendInfo: Model {
    /// Which mode this backend implements.
    fn mode(&self) -> CryptoMode;

    /// The backend's model id, as it appears in a [`KeyHandle`].
    fn backend_id(&self) -> &'static str;

    /// True when this backend can actually execute `p`.
    fn supports(&self, p: PrimitiveId) -> bool;

    /// The public handle matching a private one.
    fn public_of(&self, k: &KeyHandle) -> Result<PubHandle>;

    /// The public key's material: 33 bytes in the SEC 1 compressed shape, beginning
    /// `0x02` or `0x03`.
    ///
    /// The same shape and length in both modes, which is what keeps every certificate and
    /// every SPDU the same size under I-S1.
    fn public_material(&self, pk: &PubHandle) -> Result<Vec<u8>>;

    /// Imports a peer's public key, learned from a certificate.
    ///
    /// The receive side needs this and cannot work without it: a verifier holds no private
    /// key for the sender, so a `PubHandle` it can use has to come from somewhere other
    /// than [`CryptoBackend::keygen`]. `material` is the 33 bytes a certificate's
    /// `verifyKeyIndicator` carries ([`crate::cert::public_key_material`]).
    ///
    /// Counts against the same key-id sequence as `keygen`, so a scenario that imports the
    /// same peers in the same order gets the same handles in both modes (I-S1).
    fn import_public(
        &mut self,
        primitive: PrimitiveId,
        owner: NodeId,
        material: &[u8],
    ) -> Result<PubHandle>;

    /// SHA-256, which both modes compute for real.
    ///
    /// Hashing is not modelled in either mode: it is cheap, it is deterministic, and the
    /// digests are what the envelope's identifiers are made of, so a modelled hash would
    /// change every `HashedId8` in a run for no gain.
    fn hash(&self, bytes: &[u8]) -> [u8; 32] {
        sha256(bytes)
    }

    /// The primitive catalogue this backend sizes and costs against.
    fn catalogue(&self) -> &'static PrimitiveCatalogue {
        PrimitiveCatalogue::standard()
    }

    /// The signature size for `p`, from the descriptor tables.
    fn sig_bytes(&self, p: PrimitiveId) -> Result<u32> {
        Ok(self.catalogue().require(p)?.sig_bytes.nominal())
    }

    /// The modelled duration of `op` on `p` for hardware profile `profile`.
    ///
    /// The same answer in both modes: costs always come from the tables, because even in
    /// `Real` mode the host CPU running the simulation is not the OBU being simulated.
    fn cost(&self, p: PrimitiveId, op: PrimitiveOpKind, profile: &str) -> Option<Duration> {
        let catalogue = self.catalogue();
        catalogue.get(p)?.cost_duration(op, profile, catalogue)
    }
}

/// A cryptographic backend's operations (03-interfaces.md §6).
///
/// # Two deviations from the published signature, and why
///
/// 03-interfaces.md §6 gives `fn keygen(&mut self, ctx: &mut dyn Ctx, …) -> KeyHandle`.
/// This trait differs in two ways, both of which the crate is normative for (build
/// decision D11):
///
/// * **Generic over the context, not `dyn Ctx`.** [`Ctx`] has three associated types with
///   no defaults, so `dyn Ctx` is not a type and the published signature does not compile.
///   `v2xw-msg`'s `MessageGenerator` already solved this by parameterising over
///   `C: Ctx + ?Sized`, which costs nothing and loses nothing: with the `?Sized` bound `C`
///   may itself be a trait object, so `dyn CryptoBackend<EngineCtx>` and
///   `dyn CryptoBackend<dyn Ctx<…>>` are both usable and in-process plug-ins stay trait
///   objects (ADR 0007 §8). This trait follows that precedent.
/// * **Fallible key generation and signing.** The published signature cannot fail, but
///   the post-quantum primitives of 04-models.md §9.4 deliberately have descriptors and
///   no implementation, so [`Real`] *must* be able to refuse one. Returning
///   `Result` says so at the call site; the alternative — panicking, or silently
///   substituting a different primitive — would either stop a run or corrupt its sizes.
pub trait CryptoBackend<C: Ctx + ?Sized>: CryptoBackendInfo {
    /// Generates a key pair for `p`, owned by `owner`.
    fn keygen(&mut self, ctx: &mut C, p: PrimitiveId, owner: NodeId) -> Result<KeyHandle>;

    /// Signs a 32-byte digest.
    ///
    /// The primitive operation, because IEEE 1609.2 §5.3.1 signs a digest it computes
    /// itself (`H(H(tbsData) ‖ H(signer identifier input))`) rather than a message.
    fn sign_prehashed(&mut self, ctx: &mut C, k: &KeyHandle, digest: &[u8; 32])
    -> Result<SigToken>;

    /// Verifies a signature over a 32-byte digest.
    ///
    /// Returns a `bool`, not a `Result`: an invalid signature is *data* — the outcome a
    /// misbehaviour scenario is measuring — and not an error. A malformed handle is an
    /// error, and is reported as `false` here only because the caller of a verification
    /// has nothing better to do with it; [`CryptoBackend::public_material`] is the call
    /// that surfaces a bad handle.
    fn verify_prehashed(
        &mut self,
        ctx: &mut C,
        pk: &PubHandle,
        digest: &[u8; 32],
        sig: &SigToken,
    ) -> bool;

    /// Signs a message, hashing it with SHA-256 first.
    ///
    /// For real ECDSA this is ECDSA-with-SHA-256 exactly: signing `SHA-256(msg)` as a
    /// prehash and signing `msg` with an internal SHA-256 produce the same signature,
    /// because RFC 6979 derives its nonce from the same scalar either way.
    fn sign(&mut self, ctx: &mut C, k: &KeyHandle, msg: &[u8]) -> Result<SigToken> {
        self.sign_prehashed(ctx, k, &sha256(msg))
    }

    /// Verifies a signature over a message.
    fn verify(&mut self, ctx: &mut C, pk: &PubHandle, msg: &[u8], sig: &SigToken) -> bool {
        self.verify_prehashed(ctx, pk, &sha256(msg), sig)
    }
}

/// What a backend stores per key.
///
/// `secret` is an `Option` because a receiver imports the *public* halves of its peers'
/// keys — it learns them from certificates — and those records have no secret at all.
/// Modelling that as a missing secret rather than as a second map means one lookup path,
/// and it means a signing attempt on a peer's key is caught by name
/// ([`SecError::PublicKeyOnly`]) instead of finding a different key that happens to share
/// an id.
#[derive(Debug, Clone)]
struct KeyRecord<S> {
    primitive: PrimitiveId,
    owner: NodeId,
    secret: Option<S>,
    public_material: [u8; PUBLIC_MATERIAL_BYTES],
}

impl<S> KeyRecord<S> {
    fn signing(&self, backend: &'static str, id: KeyId) -> Result<&S> {
        self.secret
            .as_ref()
            .ok_or(SecError::PublicKeyOnly { backend, key: id.0 })
    }
}

/// Draws 32 bytes of deterministic key seed for `owner`.
fn draw_seed<C: Ctx + ?Sized>(ctx: &mut C, owner: NodeId) -> [u8; 32] {
    let mut seed = [0u8; 32];
    let mut guard = ctx.rng(RngDomain::Crypto, EntityRef::Node(owner));
    guard.fill_bytes(&mut seed);
    seed
}

/// A non-zero P-256 scalar from 32 bytes of seed.
///
/// `(seed mod (n − 1)) + 1`, the same mapping [`crate::butterfly`] uses for a caterpillar
/// scalar: it lands in `1..=n−1` for every input, so no seed is rejected and no key is
/// zero. Rejection sampling would have been the textbook answer and is the wrong one
/// here, because "draw again" makes the number of RNG draws depend on the seed value and
/// therefore breaks the fixed-draw-count rule the determinism contract rests on
/// (ADR 0004 §3).
fn scalar_from_seed(seed: &[u8; 32]) -> Result<Scalar> {
    let reduced = ec::reduce_mod_n_minus_1(seed);
    Ok(ec::scalar_from_be32(&reduced)? + Scalar::ONE)
}

// --------------------------------------------------------------------------------------
// The real backend
// --------------------------------------------------------------------------------------

/// Genuine cryptography: ECDSA P-256 with RFC 6979, SHA-256, SEC 4 ECQV.
#[derive(Debug)]
pub struct Real {
    card: ModelCard,
    next: u64,
    keys: BTreeMap<u64, KeyRecord<SigningKey>>,
}

impl Default for Real {
    fn default() -> Self {
        Self::new()
    }
}

impl Real {
    /// A backend holding no keys.
    pub fn new() -> Real {
        Real {
            card: real_card(),
            next: 1,
            // `BTreeMap`, not `HashMap`: the map is small and only point-looked-up, but a
            // hashed map's iteration order is unspecified and a future diagnostic that
            // walked the key store would silently become non-reproducible
            // (02-architecture.md §6.4).
            keys: BTreeMap::new(),
        }
    }

    /// How many keys this backend holds.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True when it holds none.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Imports a key from an explicit scalar — for a pinned test vector, and for the
    /// butterfly path, where the private key is *derived* rather than generated.
    pub fn import(
        &mut self,
        primitive: PrimitiveId,
        owner: NodeId,
        d: &Scalar,
    ) -> Result<KeyHandle> {
        if !self.supports_primitive(primitive) {
            return Err(SecError::UnsupportedPrimitive {
                backend: REAL_ID,
                primitive,
            });
        }
        let bytes = ec::scalar_to_be32(d);
        let signing = SigningKey::from_slice(&bytes).map_err(|e| SecError::Crypto {
            op: "key import",
            primitive,
            detail: e.to_string(),
        })?;
        let public_material = Point::mul_base(d).compressed().ok_or(SecError::Crypto {
            op: "key import",
            primitive,
            detail: "the scalar is zero, so the public key is the point at infinity".to_string(),
        })?;
        let id = self.next;
        self.next += 1;
        self.keys.insert(
            id,
            KeyRecord {
                primitive,
                owner,
                secret: Some(signing),
                public_material,
            },
        );
        Ok(KeyHandle {
            id: KeyId(id),
            primitive,
            owner,
            backend: REAL_ID,
        })
    }

    /// The public key as a curve point, which only the real backend has.
    pub fn public_point(&self, pk: &PubHandle) -> Result<Point> {
        let rec = self.record(pk.id, pk.backend)?;
        Point::from_sec1(&rec.public_material)
    }

    fn insert_public(
        &mut self,
        primitive: PrimitiveId,
        owner: NodeId,
        public_material: [u8; PUBLIC_MATERIAL_BYTES],
        backend: &'static str,
    ) -> PubHandle {
        let id = self.next;
        self.next += 1;
        self.keys.insert(
            id,
            KeyRecord {
                primitive,
                owner,
                secret: None,
                public_material,
            },
        );
        PubHandle {
            id: KeyId(id),
            primitive,
            owner,
            backend,
        }
    }

    fn supports_primitive(&self, p: PrimitiveId) -> bool {
        // Only the two P-256 primitives are implemented. Brainpool needs a second curve,
        // and every post-quantum entry needs PQClean; both are separable later work, and
        // refusing here is what makes that honest.
        p == PrimitiveId::ECDSA_P256_SHA256 || p == PrimitiveId::ECQV_P256
    }

    fn record(&self, id: KeyId, backend: &'static str) -> Result<&KeyRecord<SigningKey>> {
        if backend != REAL_ID {
            return Err(SecError::WrongBackend {
                backend: REAL_ID,
                owner: backend,
                key: id.0,
            });
        }
        self.keys.get(&id.0).ok_or(SecError::UnknownKey {
            backend: REAL_ID,
            key: id.0,
        })
    }
}

impl Model for Real {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl CryptoBackendInfo for Real {
    fn mode(&self) -> CryptoMode {
        CryptoMode::Real
    }

    fn backend_id(&self) -> &'static str {
        REAL_ID
    }

    fn supports(&self, p: PrimitiveId) -> bool {
        self.supports_primitive(p)
    }

    fn public_of(&self, k: &KeyHandle) -> Result<PubHandle> {
        // The owner and the primitive come from the stored record rather than from the
        // handle: the backend is authoritative, and a handle whose fields were edited
        // must not be able to relabel a key it does not own.
        let rec = self.record(k.id, k.backend)?;
        Ok(PubHandle {
            id: k.id,
            primitive: rec.primitive,
            owner: rec.owner,
            backend: REAL_ID,
        })
    }

    fn public_material(&self, pk: &PubHandle) -> Result<Vec<u8>> {
        Ok(self.record(pk.id, pk.backend)?.public_material.to_vec())
    }

    fn import_public(
        &mut self,
        primitive: PrimitiveId,
        owner: NodeId,
        material: &[u8],
    ) -> Result<PubHandle> {
        if !self.supports_primitive(primitive) {
            return Err(SecError::UnsupportedPrimitive {
                backend: REAL_ID,
                primitive,
            });
        }
        // The same check the modelled backend makes, from the same function, for the
        // reason [`check_public_material`] gives at length: an import that succeeded in
        // one mode and failed in the other was an I-S1 violation.
        let public_material = check_public_material(primitive, material)?;
        Ok(self.insert_public(primitive, owner, public_material, REAL_ID))
    }
}

impl<C: Ctx + ?Sized> CryptoBackend<C> for Real {
    fn keygen(&mut self, ctx: &mut C, p: PrimitiveId, owner: NodeId) -> Result<KeyHandle> {
        if !self.supports_primitive(p) {
            return Err(SecError::UnsupportedPrimitive {
                backend: REAL_ID,
                primitive: p,
            });
        }
        let seed = draw_seed(ctx, owner);
        let d = scalar_from_seed(&seed)?;
        self.import(p, owner, &d)
    }

    fn sign_prehashed(
        &mut self,
        _ctx: &mut C,
        k: &KeyHandle,
        digest: &[u8; 32],
    ) -> Result<SigToken> {
        let rec = self.record(k.id, k.backend)?;
        let sig: Signature = rec
            .signing(REAL_ID, k.id)?
            .sign_prehash(digest)
            .map_err(|e| SecError::Crypto {
                op: "ECDSA sign",
                primitive: rec.primitive,
                detail: e.to_string(),
            })?;
        // Emit the low-`s` representative, and only it. See `verify_prehashed` for why
        // that matters here and not in a general-purpose ECDSA library.
        // `normalize_s` returns `Some` exactly when it changed something.
        let sig = sig.normalize_s().unwrap_or(sig);
        Ok(SigToken {
            primitive: rec.primitive,
            bytes: sig.to_bytes().to_vec(),
        })
    }

    fn verify_prehashed(
        &mut self,
        _ctx: &mut C,
        pk: &PubHandle,
        digest: &[u8; 32],
        sig: &SigToken,
    ) -> bool {
        let Ok(rec) = self.record(pk.id, pk.backend) else {
            return false;
        };
        let Ok(signature) = Signature::from_slice(&sig.bytes) else {
            return false;
        };
        // ECDSA is malleable: `(r, n − s)` verifies wherever `(r, s)` does, so a
        // general-purpose verifier answers `true` for a signature nobody produced.
        // The modelled backend cannot: its token is compared byte for byte, so it answers
        // `false` for any mutation at all. Accepting the malleated form here would make
        // the outcome depend on `crypto_mode` — an I-S1 violation, and one an adversary
        // model that flips signature bytes in transit would hit on purpose. Every
        // signature this backend emits is already low-`s` (see `sign_prehashed`), so
        // refusing the high-`s` form rejects nothing that was ever legitimately signed.
        if signature.normalize_s().is_some() {
            return false;
        }
        let Ok(vk) = VerifyingKey::from_sec1_bytes(&rec.public_material) else {
            return false;
        };
        vk.verify_prehash(digest, &signature).is_ok()
    }
}

// --------------------------------------------------------------------------------------
// The modelled backend
// --------------------------------------------------------------------------------------

/// The primitives **both** backends can execute, so a scenario selecting one produces the
/// same outcomes whichever `crypto_mode` a run is given.
///
/// [`Real`] implements the two P-256 entries and nothing else; [`Modeled`] admits every
/// `Signature` and `ImplicitCert` entry in the catalogue, which is the whole point of it —
/// a post-quantum SPDU's *size* can be modelled without a PQC implementation existing. The
/// two sets are therefore deliberately unequal, and this constant is where the inequality
/// is written down instead of being discovered when a run is switched to real mode and
/// stops working.
///
/// Invariant I-S1 is a statement about a run, and a run that selects a primitive outside
/// this list cannot execute in real mode at all — it fails at `keygen` with
/// [`SecError::UnsupportedPrimitive`], loudly, before any message is signed. So the
/// asymmetry is not a silent outcome divergence. What it *is* is a scenario that only
/// works in one mode, and [`runs_in_both_modes`] is the check a scenario loader makes to
/// say so up front.
pub const MODE_INDEPENDENT_PRIMITIVES: [PrimitiveId; 2] =
    [PrimitiveId::ECDSA_P256_SHA256, PrimitiveId::ECQV_P256];

/// Whether a scenario selecting `p` runs identically in both crypto modes.
///
/// `false` means modelled-only: the run is valid, its sizes are the published ones, and it
/// will refuse to start in `crypto_mode: real`. See [`MODE_INDEPENDENT_PRIMITIVES`].
pub fn runs_in_both_modes(p: PrimitiveId) -> bool {
    MODE_INDEPENDENT_PRIMITIVES.contains(&p)
}

/// Domain separator for a modelled public key, so it cannot collide with a token.
const MODELED_PK_LABEL: &[u8] = b"v2xw-sec/modeled/public-key/v1";

/// Domain separator for a modelled signature token.
const MODELED_SIG_LABEL: &[u8] = b"v2xw-sec/modeled/signature/v1";

/// Domain separator for a key's *secret commitment*, the value a modelled token is keyed
/// on.
///
/// A different label from [`MODELED_PK_LABEL`] over the same seed, so the commitment and
/// the public material are two independent SHA-256 images of one secret: holding either
/// one tells you nothing about the other short of inverting SHA-256.
const MODELED_COMMIT_LABEL: &[u8] = b"v2xw-sec/modeled/secret-commitment/v1";

/// Public material → the secret commitment of the seed it was derived from, for every
/// modelled key **any** backend instance in this process has generated.
///
/// This is what stands in for the mathematics of ECDSA, and it has to be shared, because
/// what it stands in for is shared. A real verifier decides "was this signed by the
/// holder of the secret behind this public key?" from the curve equation — a fact that is
/// global, public, and available to every party that holds the public key. A modelled
/// backend has no curve, so the same fact has to be materialised somewhere every verifier
/// can reach it.
///
/// # What holding it per instance cost
///
/// It was a field on [`Modeled`] until 2026-09-22, populated only by the instance that
/// generated the key. Every node owns its own backend, so a receiver's map held its own
/// keys and nothing else: `verify_prehashed` found no commitment for any peer's public
/// material and answered `false` for **every** message from **every** peer. Measured on
/// `scenarios/phase2-manhattan.yaml` (`crypto_mode: modeled`): 98.23 % of received
/// messages `Invalid`, 0.73 % `Verified`, and `signatureVerification` the only detector
/// firing — a 100 % false-positive rate on honest traffic dressed up as a cryptographic
/// result. It also broke invariant I-S1 in the one direction nothing was checking: the
/// same run in `real` mode verified, because the curve equation is not per-instance.
/// `tests/equivalence.rs` now drives a *second* backend instance for exactly this reason.
///
/// # What it does not give away
///
/// The map is module-private and only ever point-queried by `verify_prehashed`, so the
/// forgery property is unchanged: producing a token still needs the commitment, the
/// commitment is `SHA-256(label ‖ seed)`, and no caller outside this module can read
/// either. See [`modeled_token`].
///
/// A `BTreeMap`, not a `HashMap`: this crate never lets a hash order reach a result, and
/// although this map is only ever point-queried, the rule is kept structurally rather
/// than by argument. Writes are idempotent — the same seed yields the same public
/// material and the same commitment — so two runs in one process cannot disagree about an
/// entry, whichever order they insert in.
fn modeled_directory() -> &'static RwLock<BTreeMap<[u8; PUBLIC_MATERIAL_BYTES], [u8; 32]>> {
    static DIRECTORY: OnceLock<RwLock<BTreeMap<[u8; PUBLIC_MATERIAL_BYTES], [u8; 32]>>> =
        OnceLock::new();
    DIRECTORY.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Records a generated key's commitment against its public material.
fn record_modeled_commitment(
    public_material: [u8; PUBLIC_MATERIAL_BYTES],
    commitment: [u8; 32],
) {
    if let Ok(mut d) = modeled_directory().write() {
        d.insert(public_material, commitment);
    }
}

/// The commitment for `public_material`, or `None` when no modelled backend ever
/// generated a key with that public material — which is the modelled counterpart of
/// `VerifyingKey::from_sec1_bytes` refusing material that is not a point.
fn modeled_commitment_for(public_material: &[u8; PUBLIC_MATERIAL_BYTES]) -> Option<[u8; 32]> {
    modeled_directory()
        .read()
        .ok()
        .and_then(|d| d.get(public_material).copied())
}

/// Modelled cryptography: tokens instead of signatures, sizes from the descriptors.
#[derive(Debug)]
pub struct Modeled {
    card: ModelCard,
    next: u64,
    keys: BTreeMap<u64, KeyRecord<[u8; 32]>>,
}

impl Default for Modeled {
    fn default() -> Self {
        Self::new()
    }
}

impl Modeled {
    /// A backend holding no keys.
    pub fn new() -> Modeled {
        Modeled {
            card: modeled_card(),
            next: 1,
            keys: BTreeMap::new(),
        }
    }

    /// How many keys this backend holds.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True when it holds none.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Imports a key from an explicit 32-byte seed.
    ///
    /// The modelled counterpart of [`Real::import`]: the same call sequence must work in
    /// both modes, or a scenario that derives a butterfly key could not run modelled.
    pub fn import_seed(
        &mut self,
        primitive: PrimitiveId,
        owner: NodeId,
        seed: &[u8; 32],
    ) -> Result<KeyHandle> {
        self.catalogue_supports(primitive)?;
        let public_material = modeled_public_material(seed);
        // Generating a key is the one moment the backend holds the secret, so it is the
        // one moment the commitment can be recorded. Re-importing the same seed writes the
        // same value, so this is idempotent and order-independent. It goes into the
        // process-wide directory rather than into this instance, because a verifier is a
        // *different* instance — see `modeled_directory`.
        record_modeled_commitment(public_material, modeled_commitment(seed));
        let id = self.next;
        self.next += 1;
        self.keys.insert(
            id,
            KeyRecord {
                primitive,
                owner,
                secret: Some(*seed),
                public_material,
            },
        );
        Ok(KeyHandle {
            id: KeyId(id),
            primitive,
            owner,
            backend: MODELED_ID,
        })
    }

    /// Imports the key a real backend would have derived from the scalar `d`.
    ///
    /// The scalar's own 32 bytes are the modelled seed, so the modelled and the real
    /// backend agree on *which* key was imported even though only one of them can use it.
    pub fn import(
        &mut self,
        primitive: PrimitiveId,
        owner: NodeId,
        d: &Scalar,
    ) -> Result<KeyHandle> {
        self.import_seed(primitive, owner, &ec::scalar_to_be32(d))
    }

    fn catalogue_supports(&self, p: PrimitiveId) -> Result<()> {
        let d = PrimitiveCatalogue::standard().require(p)?;
        // A hash or a KEM has nothing to sign with. Modelling a signature for one would
        // produce a size that no real deployment has.
        match d.family {
            PrimitiveFamily::Signature | PrimitiveFamily::ImplicitCert => Ok(()),
            _ => Err(SecError::UnsupportedPrimitive {
                backend: MODELED_ID,
                primitive: p,
            }),
        }
    }

    fn record(&self, id: KeyId, backend: &'static str) -> Result<&KeyRecord<[u8; 32]>> {
        if backend != MODELED_ID {
            return Err(SecError::WrongBackend {
                backend: MODELED_ID,
                owner: backend,
                key: id.0,
            });
        }
        self.keys.get(&id.0).ok_or(SecError::UnknownKey {
            backend: MODELED_ID,
            key: id.0,
        })
    }
}

/// The modelled public key: 33 bytes in the SEC 1 compressed shape.
///
/// `0x02` or `0x03` then 32 bytes of digest. Not a point — nothing parses it as one — but
/// the same length and the same leading byte as a real compressed point, so it lands in
/// the same `EccP256CurvePoint` alternative and every certificate carrying it has the same
/// encoded size as its real counterpart. That is half of invariant I-S1.
fn modeled_public_material(seed: &[u8; 32]) -> [u8; PUBLIC_MATERIAL_BYTES] {
    let d = sha256(&[MODELED_PK_LABEL, seed.as_slice()].concat());
    let mut out = [0u8; PUBLIC_MATERIAL_BYTES];
    // Both parities occur, exactly as they do for real keys, so a test cannot pass by
    // only ever exercising `compressed-y-0`.
    out[0] = 0x02 | (d[31] & 1);
    out[1..].copy_from_slice(&d);
    out
}

/// A key's secret commitment: `SHA-256(label ‖ seed)`.
///
/// The one value a modelled token is keyed on. It is a function of the *seed*, which never
/// leaves the backend, and it is not derivable from [`modeled_public_material`] of the same
/// seed: both are SHA-256 images under different domain separators, so recovering one from
/// the other means inverting SHA-256.
fn modeled_commitment(seed: &[u8; 32]) -> [u8; 32] {
    sha256(&[MODELED_COMMIT_LABEL, seed.as_slice()].concat())
}

/// The modelled token: `len` bytes of SHA-256 counter-mode output over the key's **secret
/// commitment** and the message digest.
///
/// Counter mode rather than a bare concatenation so that the construction is uniform
/// across every signature size in the catalogue — 64 bytes for ECDSA, 2,420 for ML-DSA-44,
/// 7,856 for SLH-DSA — with one definition and no special case at 64.
///
/// # Why the commitment and not the public material
///
/// It used to be the public material, which made the token a pure function of values an
/// adversary has: a peer's certificate carries the public material and the digest is
/// computed from the message the adversary is forging, so it could compute a token
/// [`Modeled::verify_prehashed`] accepts. Nothing in the API reached that, because signing
/// needs a [`KeyHandle`] with a secret — but the property the card claims is that a
/// modelled verification answers yes exactly when a real one would, and that was resting
/// on an API accident rather than on the construction. The first forgery model in
/// `v2xw-threat` would have turned it into a third I-S1 divergence: forgery succeeding
/// modelled and failing real.
///
/// Keyed on the commitment, forging a token means finding a SHA-256 preimage, which is the
/// same reason forgery fails in real mode, and costs one extra SHA-256 per key at keygen.
fn modeled_token(commitment: &[u8; 32], digest: &[u8; 32], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut counter: u32 = 0;
    while out.len() < len {
        let block = sha256(
            &[
                MODELED_SIG_LABEL,
                commitment.as_slice(),
                digest.as_slice(),
                &counter.to_be_bytes(),
            ]
            .concat(),
        );
        let take = (len - out.len()).min(block.len());
        out.extend_from_slice(&block[..take]);
        counter += 1;
    }
    out
}

impl Model for Modeled {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl CryptoBackendInfo for Modeled {
    fn mode(&self) -> CryptoMode {
        CryptoMode::Modeled
    }

    fn backend_id(&self) -> &'static str {
        MODELED_ID
    }

    fn supports(&self, p: PrimitiveId) -> bool {
        self.catalogue_supports(p).is_ok()
    }

    fn public_of(&self, k: &KeyHandle) -> Result<PubHandle> {
        let rec = self.record(k.id, k.backend)?;
        Ok(PubHandle {
            id: k.id,
            primitive: rec.primitive,
            owner: rec.owner,
            backend: MODELED_ID,
        })
    }

    fn public_material(&self, pk: &PubHandle) -> Result<Vec<u8>> {
        Ok(self.record(pk.id, pk.backend)?.public_material.to_vec())
    }

    fn import_public(
        &mut self,
        primitive: PrimitiveId,
        owner: NodeId,
        material: &[u8],
    ) -> Result<PubHandle> {
        self.catalogue_supports(primitive)?;
        // The same check the real backend makes, from the same function.
        let public_material = check_public_material(primitive, material)?;
        let id = self.next;
        self.next += 1;
        self.keys.insert(
            id,
            KeyRecord {
                primitive,
                owner,
                secret: None,
                public_material,
            },
        );
        Ok(PubHandle {
            id: KeyId(id),
            primitive,
            owner,
            backend: MODELED_ID,
        })
    }
}

impl<C: Ctx + ?Sized> CryptoBackend<C> for Modeled {
    fn keygen(&mut self, ctx: &mut C, p: PrimitiveId, owner: NodeId) -> Result<KeyHandle> {
        self.catalogue_supports(p)?;
        // The same draw, from the same domain and entity, as the real backend makes: one
        // 32-byte fill. Identical draw counts are what keep the two modes' RNG streams in
        // step, so a mode switch does not shift every later random number in the run.
        let seed = draw_seed(ctx, owner);
        self.import_seed(p, owner, &seed)
    }

    fn sign_prehashed(
        &mut self,
        _ctx: &mut C,
        k: &KeyHandle,
        digest: &[u8; 32],
    ) -> Result<SigToken> {
        let rec = self.record(k.id, k.backend)?;
        let seed = rec.signing(MODELED_ID, k.id)?;
        let len = PrimitiveCatalogue::standard()
            .require(rec.primitive)?
            .sig_bytes
            .nominal() as usize;
        // From the seed, not from the stored commitment: signing is the path that proves
        // the secret is held, so it must read the secret.
        Ok(SigToken {
            primitive: rec.primitive,
            bytes: modeled_token(&modeled_commitment(seed), digest, len),
        })
    }

    fn verify_prehashed(
        &mut self,
        _ctx: &mut C,
        pk: &PubHandle,
        digest: &[u8; 32],
        sig: &SigToken,
    ) -> bool {
        let Ok(rec) = self.record(pk.id, pk.backend) else {
            return false;
        };
        let Some(d) = PrimitiveCatalogue::standard().get(rec.primitive) else {
            return false;
        };
        // Material no modelled backend ever generated has no commitment, so nothing
        // verifies under it — which is the modelled counterpart of
        // `VerifyingKey::from_sec1_bytes` refusing material that is not a point. Both
        // modes answer `false`.
        let Some(commitment) = modeled_commitment_for(&rec.public_material) else {
            return false;
        };
        let expected = modeled_token(&commitment, digest, d.sig_bytes.nominal() as usize);
        // A length mismatch is caught by the equality itself, which is what a real
        // backend's `Signature::from_slice` does for a truncated signature: both modes
        // answer `false` for the same malformed input.
        sig.bytes == expected
    }
}

// --------------------------------------------------------------------------------------
// ECQV implicit certificates (SEC 4 §3.4-3.5)
// --------------------------------------------------------------------------------------

/// ECQV implicit certificates: SEC 4 §3.4 (issuance) and §3.5 (reconstruction).
///
/// An implicit certificate carries no signature. Instead it carries a *reconstruction
/// value* `P_U`, a compressed point, and anyone can compute the subject's public key from
/// it and the issuer's:
///
/// ```text
/// requester: k_U random,  R_U = k_U·G                                    (§3.4 step 1)
/// CA:        k random,    P_U = R_U + k·G                                (the recon value)
///            e = H(IC),   r = e·k + c_CA  (mod n)                        (§3.4)
/// requester: d_U = e·k_U + r  (mod n)                                    (§3.5, private)
/// anyone:    Q_U = e·P_U + Q_CA                                          (§3.5, public)
/// ```
///
/// The pair is consistent — `d_U·G = Q_U` — which is what
/// [`verify_reconstruction`] checks. The security property is that a tampered
/// certificate changes `e` and therefore changes `Q_U` into a key nobody holds the private
/// half of: a forged implicit certificate does not fail to parse, it simply fails to
/// verify anything ever signed under it. That is why there is no signature to check and
/// why the certificate is 80 bytes rather than 147 (04-models.md §9.2).
///
/// # Which hash
///
/// SEC 4 hashes the encoded certificate. IEEE 1609.2 §5.3.2 hashes
/// `H(H(certificate) ‖ H(signer identifier input))`, the same two-stage construction it
/// uses for a signature, so that a certificate is bound to its issuer's own certificate.
/// Both are provided — [`cert_hash_sec4`] and [`cert_hash_1609`] — because a
/// simulator that mixed them up would produce keys that verify nothing, and neither is
/// derivable from the other.
pub mod ecqv {
    use super::*;

    /// `e = H(IC) mod n`, the SEC 4 §3.4 certificate hash.
    pub fn cert_hash_sec4(cert_coer: &[u8]) -> Scalar {
        hash_to_scalar(&sha256(cert_coer))
    }

    /// `e = H( H(certificate) ‖ H(signer identifier input) ) mod n`, the IEEE 1609.2
    /// §5.3.2 certificate hash.
    ///
    /// `signer_identifier_input` is the COER encoding of the issuer's certificate, or the
    /// empty string when the issuer is `self`.
    pub fn cert_hash_1609(cert_coer: &[u8], signer_identifier_input: &[u8]) -> Scalar {
        let inner = [sha256(cert_coer), sha256(signer_identifier_input)].concat();
        hash_to_scalar(&sha256(&inner))
    }

    /// A 32-byte digest reduced to a scalar.
    ///
    /// Reduction rather than rejection, for the reason [`super::scalar_from_seed`] gives:
    /// a variable number of hash evaluations would make the operation's cost depend on
    /// its input, and the engine charges a fixed cost per primitive operation.
    pub fn hash_to_scalar(digest: &[u8; 32]) -> Scalar {
        ec::scalar_from_be_mod_n(digest)
    }

    /// The reconstruction value `P_U = R_U + k·G` the CA puts in the certificate.
    pub fn reconstruction_value(request_public: &Point, ca_ephemeral: &Scalar) -> Point {
        request_public.add(&Point::mul_base(ca_ephemeral))
    }

    /// The CA's private-key contribution `r = e·k + c_CA (mod n)`.
    pub fn private_key_contribution(
        e: &Scalar,
        ca_ephemeral: &Scalar,
        ca_private: &Scalar,
    ) -> Scalar {
        e * ca_ephemeral + ca_private
    }

    /// The subject's private key `d_U = e·k_U + r (mod n)`.
    pub fn subject_private_key(e: &Scalar, requester_ephemeral: &Scalar, r: &Scalar) -> Scalar {
        e * requester_ephemeral + r
    }

    /// The subject's public key `Q_U = e·P_U + Q_CA`, reconstructed by any relying party.
    ///
    /// This is the operation a receiver performs for every implicit certificate it
    /// validates, and the one [`crate::envelope::PrimitiveOp::ReconstructImplicit`]
    /// charges. Cost: one point multiplication and one addition, which is why
    /// `primitive/ecqv-p256` declares the ECDSA verify anchor as its cost proxy.
    pub fn reconstruct_public_key(e: &Scalar, reconstruction: &Point, ca_public: &Point) -> Point {
        reconstruction.mul(e).add(ca_public)
    }

    /// True when a reconstructed public key matches the private key the subject derived.
    pub fn verify_reconstruction(private: &Scalar, reconstructed: &Point) -> bool {
        Point::mul_base(private) == *reconstructed
    }
}

// --------------------------------------------------------------------------------------
// Model cards
// --------------------------------------------------------------------------------------

fn backend_determinism() -> Determinism {
    Determinism {
        uses_rng: true,
        rng_domains: vec!["crypto".to_string()],
    }
}

fn real_card() -> ModelCard {
    let mut card = ModelCard::new(
        REAL_ID,
        Family::CryptoBackend,
        "1.0.0",
        "Executes real cryptography: ECDSA over NIST P-256 with RFC 6979 deterministic \
         nonces, SHA-256, and SEC 4 ECQV implicit-certificate reconstruction. Refuses the \
         post-quantum primitives, which have descriptors and cost tables but no \
         implementation.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        v2xw_core::card::Equation::new(
            "ECDSA signature",
            "(r, s) over SHA-256(m) with k = HMAC-DRBG(d, m) per RFC 6979",
        ),
        v2xw_core::card::Equation::new(
            "ECQV reconstruction",
            "Q_U = e·P_U + Q_CA, d_U = e·k_U + r (mod n), e = H(IC)",
        ),
    ];
    card.assumptions = vec![
        "Key seeds come from the scenario RNG (domain `crypto`, entity `node`), so real \
         keys are reproducible across runs — a requirement of the determinism contract, \
         not a weakening of the cryptography being simulated."
            .to_string(),
        "Signatures are RFC 6979 deterministic, so signing is a pure function.".to_string(),
    ];
    card.limitations = vec![
        "Only `primitive/ecdsa-p256-sha256` and `primitive/ecqv-p256` are implemented; \
         every other catalogue entry is refused with `UnsupportedPrimitive`."
            .to_string(),
        "A key seed is mapped onto 1..=n-1 by `(seed mod (n-1)) + 1` rather than by \
         rejection sampling, which keeps the draw count fixed at one 32-byte fill and \
         leaves a negligible, documented bias."
            .to_string(),
    ];
    card.ignores = vec![
        "Side channels, key storage, HSM command latency (the cost tables model the \
         latency; this backend does not)."
            .to_string(),
    ];
    card.sources = vec![
        Source::new(SourceKind::Standard, "IEEE 1609.2 §5.3.1, §6.3.29"),
        Source::new(SourceKind::Standard, "RFC 6979"),
        Source::new(SourceKind::Standard, "SEC 4 §3.4-3.5"),
        Source::new(SourceKind::Standard, "FIPS 186-4"),
        Source::new(SourceKind::Code, "RustCrypto p256 0.13"),
    ];
    card.determinism = backend_determinism();
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![Source::new(SourceKind::Standard, "SEC 4 §3.4-3.5")],
        tests: vec![
            "the_two_backends_agree_on_every_outcome_and_every_size".to_string(),
            "an_ecqv_certificate_reconstructs_to_the_subjects_own_key".to_string(),
        ],
    };
    card
}

fn modeled_card() -> ModelCard {
    let mut card = ModelCard::new(
        MODELED_ID,
        Family::CryptoBackend,
        // 3.0.0: the secret commitments moved from a field on the instance to a
        // process-wide directory, so a backend now verifies a key it did not generate.
        // No token byte changed; what changed is the *outcome* of every cross-instance
        // verification, from `false` to what the real backend answers. Since every node
        // in a run owns its own backend, that is every verification in every run.
        //
        // 2.0.0: the token is keyed on the key's secret commitment rather than on its
        // public material, so every modelled token byte changed. Sizes and outcomes did
        // not, which is what I-S1 constrains — but the bytes are pinned in tests, so the
        // change is a breaking one for anything that recorded them.
        "3.0.0",
        "Models cryptography without executing it: a signature is a token derived from \
         the signer's secret commitment and the message digest, verification compares \
         tokens, and every size comes from the primitive descriptor tables. Verification \
         outcomes and encoded sizes are identical to `crypto-backend/real` by \
         construction (invariant I-S1).",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        v2xw_core::card::Equation::new(
            "modelled secret commitment",
            "commitment = SHA-256( commit-label ‖ seed ), recorded at keygen against the \
             key's public material in a directory every backend instance shares",
        ),
        v2xw_core::card::Equation::new(
            "modelled signature token",
            "token = SHA-256-CTR( sig-label ‖ commitment ‖ digest ), truncated to the \
             descriptor's sig_bytes",
        ),
    ];
    card.assumptions = vec![
        "A token verifies exactly when it was produced by that key over that digest, \
         which is also exactly when a real signature verifies — so the outcome sequence \
         is identical (I-S1)."
            .to_string(),
        "A modelled public key is 33 bytes beginning 0x02 or 0x03, the shape of a real \
         compressed point, so every certificate and SPDU has the same encoded size as \
         under the real backend."
            .to_string(),
        "Key seeds come from the same RNG domain, entity and draw count as the real \
         backend, so switching modes does not shift any later random number in the run."
            .to_string(),
        "A token is keyed on the secret commitment, so producing one without the secret \
         means inverting SHA-256. Forgery therefore fails for the same reason it fails in \
         real mode, rather than because no API exposes it (v2.0.0)."
            .to_string(),
    ];
    card.limitations = vec![
        "No cryptographic strength is claimed beyond SHA-256 preimage resistance: a token \
         is a hash image, not a signature, and a backend that holds the commitment can \
         produce one for any digest. It models outcomes, not security."
            .to_string(),
        "Verification is answered from the commitment recorded when the key was \
         generated, because computing it from public material alone would hand an \
         attacker the same computation. The commitments therefore live in a directory \
         shared by every backend instance in the process — the stand-in for the curve \
         equation, which is likewise global and available to every relying party. What \
         this models is a world in which a genuine public key is recognisable as one; \
         what it does not model is a party that has to *decide* that from the key alone."
            .to_string(),
        "I-S1 is demonstrated end to end for `ecdsa-p256-sha256` and `ecqv-p256` only; \
         the other catalogue primitives are exercised for supports/keygen agreement, not \
         for a full signing trace, because the real backend implements no other family."
            .to_string(),
        "Falcon's variable signature length is modelled at its mean, because the \
         descriptor's `Variable { mean, max }` has no distribution attached."
            .to_string(),
    ];
    card.ignores = vec!["Everything cryptographic.".to_string()];
    card.sources = vec![
        Source::new(SourceKind::Standard, "04-models.md §9.6 (crypto modes)"),
        Source::new(SourceKind::Standard, "03-interfaces.md §6 (invariant I-S1)"),
    ];
    card.determinism = backend_determinism();
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![Source::new(
            SourceKind::Standard,
            "03-interfaces.md §6 invariant I-S1",
        )],
        tests: vec!["the_two_backends_agree_on_every_outcome_and_every_size".to_string()],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testctx::TestCtx;

    fn node(i: u32) -> NodeId {
        NodeId::new(i)
    }

    /// Both backends' cards must validate, and they must declare the RNG domain they
    /// actually draw from (invariant I-C3's sibling for determinism).
    #[test]
    fn both_backend_cards_validate_and_declare_their_rng_domain() {
        for card in [real_card(), modeled_card()] {
            card.validate()
                .unwrap_or_else(|e| panic!("{}: {e}", card.id));
            assert_eq!(card.family, Family::CryptoBackend);
            assert!(card.determinism.uses_rng);
            assert_eq!(card.determinism.rng_domains, vec!["crypto".to_string()]);
            assert!(!card.sources.is_empty());
        }
    }

    /// ECDSA is malleable: `(r, n − s)` verifies wherever `(r, s)` does. A stock verifier
    /// therefore says `true` to a signature nobody produced, while the modelled backend —
    /// which compares its token byte for byte — says `false`. Same key, same digest, same
    /// bytes, different answer per `crypto_mode`: an I-S1 violation, and one an adversary
    /// model that mutates signature bytes in transit reaches on purpose.
    #[test]
    fn a_malleated_signature_is_refused_by_both_backends() {
        let mut ctx = TestCtx::new(7);
        let mut real = Real::new();
        let mut modeled = Modeled::new();
        let digest = [0x42u8; 32];

        let kr = real
            .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(1))
            .expect("keygen");
        let pr = real.public_of(&kr).expect("public");
        let sr = real.sign_prehashed(&mut ctx, &kr, &digest).expect("signs");

        let km = modeled
            .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(1))
            .expect("keygen");
        let pm = modeled.public_of(&km).expect("public");
        let sm = modeled
            .sign_prehashed(&mut ctx, &km, &digest)
            .expect("signs");

        // The signature this backend emits is already the low-`s` representative, so
        // negating `s` really does produce the *other* one rather than the same bytes.
        assert!(
            Signature::from_slice(&sr.bytes)
                .expect("a signature")
                .normalize_s()
                .is_none(),
            "sign_prehashed must emit low-s"
        );

        let malleated = |t: &SigToken| {
            let mut out = t.clone();
            let s = ec::scalar_from_be_mod_n(&t.bytes[32..64]);
            out.bytes[32..64].copy_from_slice(&ec::scalar_to_be32(&(-s)));
            out
        };
        let mr = malleated(&sr);
        let mm = malleated(&sm);
        assert_ne!(mr.bytes, sr.bytes, "the malleation changed the bytes");

        assert!(real.verify_prehashed(&mut ctx, &pr, &digest, &sr));
        assert!(modeled.verify_prehashed(&mut ctx, &pm, &digest, &sm));
        assert!(
            !real.verify_prehashed(&mut ctx, &pr, &digest, &mr),
            "the real backend must refuse the high-s representative"
        );
        assert!(
            !modeled.verify_prehashed(&mut ctx, &pm, &digest, &mm),
            "the modelled backend refuses any mutation"
        );
    }

    /// A modelled token must not be computable from values an adversary has.
    ///
    /// It used to be: `token = SHA-256-CTR(label ‖ public material ‖ digest)`, and both
    /// inputs are public — the public material travels in every certificate and the digest
    /// is over the message being forged. Nothing in the API reached it, because signing
    /// needs a `KeyHandle` carrying a secret, so the claim "a modelled verification answers
    /// yes exactly when a real one would" rested on that guard rather than on the
    /// construction. The first forgery model in `v2xw-threat` would have succeeded modelled
    /// and failed real: a third I-S1 divergence.
    ///
    /// The forger's computation below is written out here rather than taken from
    /// [`modeled_token`] on purpose: it is the attacker's own implementation of the
    /// documented construction, from the public inputs and nothing else.
    #[test]
    fn a_modelled_token_cannot_be_computed_from_public_material_and_the_digest() {
        let mut ctx = TestCtx::new(11);
        let mut modeled = Modeled::new();
        let digest = [0x5au8; 32];

        let k = modeled
            .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(3))
            .expect("keygen");
        let pk = modeled.public_of(&k).expect("public");
        let material = modeled.public_material(&pk).expect("material");
        assert_eq!(material.len(), PUBLIC_MATERIAL_BYTES);

        // Everything the adversary has: the peer's certificate gave it `material`, and it
        // chose the message, so it has `digest`. This is the old construction.
        let forged = {
            let len = PrimitiveCatalogue::standard()
                .require(PrimitiveId::ECDSA_P256_SHA256)
                .expect("catalogued")
                .sig_bytes
                .nominal() as usize;
            let mut out: Vec<u8> = Vec::with_capacity(len);
            let mut counter: u32 = 0;
            while out.len() < len {
                let block = sha256(
                    &[
                        MODELED_SIG_LABEL,
                        material.as_slice(),
                        digest.as_slice(),
                        &counter.to_be_bytes(),
                    ]
                    .concat(),
                );
                let take = (len - out.len()).min(block.len());
                out.extend_from_slice(&block[..take]);
                counter += 1;
            }
            SigToken {
                primitive: PrimitiveId::ECDSA_P256_SHA256,
                bytes: out,
            }
        };

        let genuine = modeled
            .sign_prehashed(&mut ctx, &k, &digest)
            .expect("signs");
        assert_eq!(
            forged.bytes.len(),
            genuine.bytes.len(),
            "the forgery is the right size, so only the keying can refuse it"
        );
        assert_ne!(
            forged.bytes, genuine.bytes,
            "the token must not be the public-material image"
        );
        assert!(
            !modeled.verify_prehashed(&mut ctx, &pk, &digest, &forged),
            "a token computed from public inputs alone must not verify"
        );
        // And the honest path is untouched.
        assert!(modeled.verify_prehashed(&mut ctx, &pk, &digest, &genuine));

        // The commitment is a second, independent SHA-256 image of the seed: it is not the
        // public material and not the 32 bytes inside it, so holding the key's public form
        // gives an adversary nothing to key the token on.
        let seed = [0x11u8; 32];
        let commitment = modeled_commitment(&seed);
        let public = modeled_public_material(&seed);
        assert_ne!(commitment[..], public[1..]);
        assert_ne!(commitment.to_vec(), public.to_vec());
    }

    /// Importing key material that is not a point must give the *same* answer in both
    /// modes. It did not: the real backend validated curve membership and the modelled one
    /// could not, so a crafted `verifyKeyIndicator` in a received certificate took
    /// different branches depending on `crypto_mode`.
    #[test]
    fn both_backends_agree_on_every_shape_of_imported_key_material() {
        let mut real = Real::new();
        let mut modeled = Modeled::new();

        // x = 1 is not on P-256: correctly shaped, and not a point.
        let mut off_curve = [0u8; PUBLIC_MATERIAL_BYTES];
        off_curve[0] = 0x02;
        off_curve[32] = 1;

        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("off-curve", off_curve.to_vec()),
            ("empty", Vec::new()),
            ("one byte", vec![0x00]),
            ("32 bytes", vec![0x02; 32]),
            ("34 bytes", vec![0x02; 34]),
            ("uncompressed 65", vec![0x04; 65]),
            ("bad prefix", {
                let mut v = off_curve.to_vec();
                v[0] = 0x04;
                v
            }),
            ("all zero", vec![0u8; PUBLIC_MATERIAL_BYTES]),
        ];
        for (name, material) in cases {
            let r = real.import_public(PrimitiveId::ECDSA_P256_SHA256, node(1), &material);
            let m = modeled.import_public(PrimitiveId::ECDSA_P256_SHA256, node(1), &material);
            assert_eq!(
                r.is_ok(),
                m.is_ok(),
                "{name}: real={:?} modeled={:?}",
                r.as_ref().err().map(ToString::to_string),
                m.as_ref().err().map(ToString::to_string),
            );
            if let (Err(re), Err(me)) = (&r, &m) {
                assert_eq!(re.to_string(), me.to_string(), "{name}");
            }
        }

        // And the material that *is* correctly shaped but not a point imports in both
        // modes and then verifies nothing, which is where the rejection now happens.
        let mut ctx = TestCtx::new(9);
        let pr = real
            .import_public(PrimitiveId::ECDSA_P256_SHA256, node(2), &off_curve)
            .expect("shape is all that is checked");
        let pm = modeled
            .import_public(PrimitiveId::ECDSA_P256_SHA256, node(2), &off_curve)
            .expect("shape is all that is checked");
        let token = SigToken {
            primitive: PrimitiveId::ECDSA_P256_SHA256,
            bytes: vec![0u8; 64],
        };
        assert!(!real.verify_prehashed(&mut ctx, &pr, &[7u8; 32], &token));
        assert!(!modeled.verify_prehashed(&mut ctx, &pm, &[7u8; 32], &token));
    }

    /// Real signing is a pure function of the key and the message — RFC 6979, not
    /// entropy. Without this the manifest digest could not be reproduced.
    #[test]
    fn real_signatures_are_deterministic() {
        let mut ctx = TestCtx::new(7);
        let mut a = Real::new();
        let mut b = Real::new();
        let ka = a
            .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(1))
            .expect("keygen");
        let mut ctx2 = TestCtx::new(7);
        let kb = b
            .keygen(&mut ctx2, PrimitiveId::ECDSA_P256_SHA256, node(1))
            .expect("keygen");
        assert_eq!(
            a.public_material(&a.public_of(&ka).expect("pub"))
                .expect("material"),
            b.public_material(&b.public_of(&kb).expect("pub"))
                .expect("material"),
            "the same seed must give the same key"
        );
        let s1 = a.sign(&mut ctx, &ka, b"message").expect("sign");
        let s2 = a.sign(&mut ctx, &ka, b"message").expect("sign");
        let s3 = b.sign(&mut ctx2, &kb, b"message").expect("sign");
        assert_eq!(s1, s2, "signing twice must give the same bytes");
        assert_eq!(s1, s3, "a second backend with the same seed must agree");
        assert_eq!(s1.len(), 64, "raw ECDSA P-256 is 64 bytes");
    }

    /// A real signature verifies, and stops verifying the moment anything changes.
    #[test]
    fn a_real_signature_verifies_and_tampering_breaks_it() {
        let mut ctx = TestCtx::new(11);
        let mut be = Real::new();
        let k = be
            .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(3))
            .expect("keygen");
        let pk = be.public_of(&k).expect("pub");
        let sig = be.sign(&mut ctx, &k, b"payload").expect("sign");
        assert!(be.verify(&mut ctx, &pk, b"payload", &sig));
        assert!(
            !be.verify(&mut ctx, &pk, b"payloaD", &sig),
            "tampered message"
        );
        let mut bad = sig.clone();
        bad.bytes[0] ^= 1;
        assert!(
            !be.verify(&mut ctx, &pk, b"payload", &bad),
            "tampered signature"
        );
        bad.bytes.truncate(60);
        assert!(
            !be.verify(&mut ctx, &pk, b"payload", &bad),
            "truncated signature"
        );
    }

    /// The real backend refuses the primitives it does not implement, by name, rather
    /// than pretending.
    #[test]
    fn the_real_backend_refuses_the_primitives_it_does_not_implement() {
        let mut ctx = TestCtx::new(1);
        let mut be = Real::new();
        for p in [
            PrimitiveId::ML_DSA_44,
            PrimitiveId::FALCON_512,
            PrimitiveId::SLH_DSA_SHA2_128S,
            PrimitiveId::ED25519,
            PrimitiveId::ECDSA_BRAINPOOL_P256R1,
        ] {
            assert!(!be.supports(p), "{p}");
            let e = be.keygen(&mut ctx, p, node(1)).expect_err("must refuse");
            assert!(
                matches!(e, SecError::UnsupportedPrimitive { primitive, .. } if primitive == p),
                "{e}"
            );
        }
        assert!(be.supports(PrimitiveId::ECDSA_P256_SHA256));
        assert!(be.supports(PrimitiveId::ECQV_P256));
    }

    /// The modelled backend serves every signature primitive, at the descriptor's size,
    /// and refuses the ones that cannot sign.
    #[test]
    fn the_modelled_backend_sizes_every_signature_primitive_from_its_descriptor() {
        let mut ctx = TestCtx::new(5);
        let mut be = Modeled::new();
        for (p, expect) in [
            (PrimitiveId::ECDSA_P256_SHA256, 64usize),
            (PrimitiveId::ML_DSA_44, 2_420),
            (PrimitiveId::ML_DSA_87, 4_627),
            (PrimitiveId::FALCON_512, 666),
            (PrimitiveId::SLH_DSA_SHA2_128F, 17_088),
            (PrimitiveId::HYBRID_MLDSA44_ECDSA_P256, 2_484),
        ] {
            let k = be.keygen(&mut ctx, p, node(2)).expect("keygen");
            let pk = be.public_of(&k).expect("pub");
            let sig = be.sign(&mut ctx, &k, b"m").expect("sign");
            assert_eq!(sig.len(), expect, "{p}");
            assert!(be.verify(&mut ctx, &pk, b"m", &sig), "{p}");
            assert!(!be.verify(&mut ctx, &pk, b"n", &sig), "{p} tampered");
        }
        for p in [PrimitiveId::SHA_256, PrimitiveId::ML_KEM_768] {
            assert!(be.keygen(&mut ctx, p, node(2)).is_err(), "{p}");
        }
    }

    /// A modelled public key has the shape of a real compressed point — the property that
    /// makes every downstream encoded size identical.
    #[test]
    fn a_modelled_public_key_has_the_shape_of_a_real_one() {
        let mut ctx = TestCtx::new(13);
        let mut modeled = Modeled::new();
        let mut real = Real::new();
        let mut parities = std::collections::BTreeSet::new();
        for n in 0..16u32 {
            let km = modeled
                .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(n))
                .expect("keygen");
            let m = modeled
                .public_material(&modeled.public_of(&km).expect("pub"))
                .expect("material");
            assert_eq!(m.len(), PUBLIC_MATERIAL_BYTES);
            assert!(m[0] == 0x02 || m[0] == 0x03, "byte 0 = {:#04x}", m[0]);
            parities.insert(m[0]);

            let kr = real
                .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(n))
                .expect("keygen");
            let r = real
                .public_material(&real.public_of(&kr).expect("pub"))
                .expect("material");
            assert_eq!(r.len(), m.len(), "the two modes must agree on the length");
        }
        assert_eq!(
            parities.len(),
            2,
            "both compressed alternatives must occur, or a test could pass while only \
             exercising compressed-y-0"
        );
    }

    /// A handle from one backend must be refused by the other, not silently resolved to a
    /// different key with the same number.
    #[test]
    fn a_handle_from_one_backend_is_refused_by_the_other() {
        let mut ctx = TestCtx::new(3);
        let mut real = Real::new();
        let mut modeled = Modeled::new();
        let kr = real
            .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(1))
            .expect("keygen");
        let km = modeled
            .keygen(&mut ctx, PrimitiveId::ECDSA_P256_SHA256, node(1))
            .expect("keygen");
        assert_eq!(kr.id, km.id, "both number from 1, which is the hazard");
        let e = modeled.public_of(&kr).expect_err("must refuse");
        assert!(matches!(e, SecError::WrongBackend { .. }), "{e}");
        assert!(real.public_of(&km).is_err());
        // And an unknown id within the right backend is a different error.
        let ghost = KeyHandle {
            id: KeyId(999),
            primitive: PrimitiveId::ECDSA_P256_SHA256,
            owner: node(1),
            backend: REAL_ID,
        };
        assert!(matches!(
            real.public_of(&ghost).expect_err("unknown"),
            SecError::UnknownKey { .. }
        ));
    }

    /// Costs come from the tables in both modes, and they are the same number: even in
    /// real mode the host CPU is not the OBU being simulated.
    #[test]
    fn both_modes_charge_the_same_modelled_cost() {
        let real = Real::new();
        let modeled = Modeled::new();
        let profile = crate::primitive::profiles::CORTEX_M4_NRF52840;
        let a = real.cost(
            PrimitiveId::ECDSA_P256_SHA256,
            PrimitiveOpKind::Verify,
            profile,
        );
        let b = modeled.cost(
            PrimitiveId::ECDSA_P256_SHA256,
            PrimitiveOpKind::Verify,
            profile,
        );
        assert_eq!(a, Some(Duration::from_micros(15_300)));
        assert_eq!(a, b);
        // ECQV resolves through its declared proxy in both modes.
        assert_eq!(
            real.cost(PrimitiveId::ECQV_P256, PrimitiveOpKind::Verify, profile),
            a
        );
    }

    /// SEC 4 §3.4-3.5, end to end: the subject's derived private key and the public key a
    /// relying party reconstructs are the same key.
    #[test]
    fn an_ecqv_certificate_reconstructs_to_the_subjects_own_key() {
        use ecqv::*;
        let ca_private = Scalar::from(0x000B_ADC0_FFEE_u64);
        let ca_public = Point::mul_base(&ca_private);
        let k_u = Scalar::from(0x1234_5678u64); // requester ephemeral
        let r_u = Point::mul_base(&k_u);
        let k = Scalar::from(0x9ABC_DEF0u64); // CA ephemeral

        let p_u = reconstruction_value(&r_u, &k);
        let cert = b"a COER-encoded ToBeSignedCertificate carrying P_U";
        let e = cert_hash_sec4(cert);
        let r = private_key_contribution(&e, &k, &ca_private);

        let d_u = subject_private_key(&e, &k_u, &r);
        let q_u = reconstruct_public_key(&e, &p_u, &ca_public);
        assert!(verify_reconstruction(&d_u, &q_u), "d_U·G must equal Q_U");

        // The security property: a tampered certificate reconstructs to a key nobody
        // holds. There is no signature to fail — the failure surfaces as a key mismatch.
        let e_bad = cert_hash_sec4(b"a COER-encoded ToBeSignedCertificate carrying P_V");
        let q_bad = reconstruct_public_key(&e_bad, &p_u, &ca_public);
        assert_ne!(q_u, q_bad);
        assert!(!verify_reconstruction(&d_u, &q_bad));

        // And a real signature under the derived key verifies against the reconstructed
        // public key, which is the whole point of an implicit certificate.
        let mut ctx = TestCtx::new(1);
        let mut be = Real::new();
        let key = be
            .import(PrimitiveId::ECQV_P256, node(1), &d_u)
            .expect("import");
        let sig = be.sign(&mut ctx, &key, b"a signed BSM").expect("sign");
        let vk = VerifyingKey::from_sec1_bytes(&q_u.compressed().expect("point")).expect("vk");
        let signature = Signature::from_slice(&sig.bytes).expect("signature");
        assert!(
            vk.verify_prehash(&sha256(b"a signed BSM"), &signature)
                .is_ok(),
            "the reconstructed public key must verify the subject's signature"
        );
    }

    /// The two 1609.2 and SEC 4 hashes are genuinely different constructions; neither is
    /// a drop-in for the other.
    #[test]
    fn the_two_certificate_hashes_are_different_constructions() {
        let cert = b"certificate";
        assert_ne!(ecqv::cert_hash_sec4(cert), ecqv::cert_hash_1609(cert, b""));
        assert_ne!(
            ecqv::cert_hash_1609(cert, b""),
            ecqv::cert_hash_1609(cert, b"issuer")
        );
    }

    /// A key seed always maps to a usable, non-zero scalar — every input, including the
    /// two extremes.
    #[test]
    fn every_seed_maps_to_a_usable_key() {
        for seed in [[0u8; 32], [0xffu8; 32], ec::N_BE, ec::N_MINUS_1_BE] {
            let s = scalar_from_seed(&seed).expect("every seed maps");
            assert_ne!(s, Scalar::ZERO);
            assert!(!Point::mul_base(&s).is_identity());
        }
    }
}
