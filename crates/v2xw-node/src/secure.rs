//! The node's own security stack: real payloads, real signatures, real bytes.
//!
//! # What this module exists to stop
//!
//! Until 2026-09-22 a node's `generate` path called neither a codec nor a crypto backend.
//! It produced a `u32` — `93 + signer identifier`, with a zero payload — and handed that
//! to the engine as if it were a message. The tell an audit found before reading a line of
//! code: a basic safety message and a cooperative awareness message both came out at
//! exactly 101 bytes. Two different standards, two different formats, two different field
//! sets, one number. That cannot happen to real bytes, and it is why
//! [`crate::runtime::ObuRuntime`] now encodes and signs for real and why the first test in
//! `tests/wire_bytes.rs` is that the two sizes *differ*.
//!
//! It matters more here than it would in most projects: security overhead as a fraction of
//! airtime, verification cost under load and what a node does when it cannot keep up with
//! signing are the three headline results this simulator exists to produce, and all three
//! are ratios whose numerator was a constant.
//!
//! # The three pieces
//!
//! | Piece | What it is |
//! |---|---|
//! | [`SecCtx`] | the bridge from [`NodeCtx`] to [`v2xw_core::ctx::Ctx`], which `v2xw-sec` is generic over. It carries **no** world and **no** actors: both associated types are `()`, and [`crate::firewall::scan_security_bridge`] fails the build if either stops being. |
//! | [`NodeCrypto`] | the backend, `real` or `modeled`. Nothing outside this enum branches on which — invariant I-S1, and Phase 1 acceptance criterion 3. |
//! | [`NodeSecurity`] | the envelope, the signer handles, and the stand-in credential issuance that keeps a node signing while no `CredentialProtocol` plug-in ships. |
//!
//! # The stand-in issuer, and what it is not
//!
//! 03-interfaces.md §7 puts enrolment and provisioning in a `CredentialProtocol` plug-in
//! that does not exist yet. A node still has to sign with *some* key under *some*
//! certificate, and `v2xw-sec` will not sign without both. So [`NodeSecurity`] issues its
//! own: one local authority per node, and one explicit pseudonym certificate per
//! credential the store holds, issued by it.
//!
//! What that gives is real: a real P-256 key, a real IEEE 1609.2 certificate with a real
//! linkage value, a real `HashedId8`, and a signature a relying party can check against
//! the certificate the SPDU carries. What it does **not** give is a chain to a common
//! trust anchor — each node's authority is its own — so a receiver here verifies the
//! signature against the signer's certificate and cannot conclude that the certificate was
//! issued by anyone it trusts. That is a gap in the credential protocol, not a gap in the
//! cryptography, and it is stated rather than papered over: see
//! [`NodeSecurity::verify_parsed`], and [`SpduVerdict::Unverifiable`], which is what a
//! receiver answers rather than guessing when it holds no certificate for the signer.

use std::collections::BTreeMap;
use std::sync::Arc;

use v2xw_core::ctx::{Ctx, ErasedRecord};
use v2xw_core::event::{EventClass, EventHandle};
use v2xw_core::ids::NodeId;
use v2xw_core::provenance::ProvSubject;
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard};
use v2xw_core::time::{IEEE1609_EPOCH_UNIX_S, SimTime, WallClock};
use v2xw_msg::MsgType;
use v2xw_msg::sec_types::{Certificate, HashedId8};
use v2xw_sec::cert::{self, CertSpec, HolderId};
use v2xw_sec::crypto::{CryptoBackend, CryptoBackendInfo, KeyHandle, PubHandle, SigToken};
use v2xw_sec::envelope::{
    Envelope, EnvelopeProfile, GenerationLocation, HeaderInfoSpec, ParsedSecured, ParsedSigner,
    SecuredPdu, SecurityEnvelope, SecurityEnvelopeInfo, SignerHandle, SignerIdChoice,
    SignerIdPolicy,
};
use v2xw_sec::linkage::{DeviceLinkageContext, LaId, LinkageSeed};
use v2xw_sec::primitive::PrimitiveId;
use v2xw_sec::{SecError, hashedid};

use crate::ctx::NodeCtx;

/// The PSID a CAM or a BSM is signed under.
///
/// 0x20, the value 04-models.md §9.1 uses in its envelope-overhead derivation and the one
/// the 93-byte figure is measured at. A PSID of 0x100 or more costs a third COER byte, so
/// this is not a free choice and a scenario that changes it changes the overhead.
pub const PSID_SAFETY: u64 = 0x20;

/// The signature primitive the node signs and verifies with.
///
/// The only entry in [`v2xw_sec::MODE_INDEPENDENT_PRIMITIVES`] that a safety message uses,
/// which is what makes the backend a swap rather than a scenario decision: a run that
/// selected a post-quantum primitive would refuse to start in `real` mode instead of
/// quietly diverging (see that constant's own documentation).
pub const SIGN_PRIMITIVE: PrimitiveId = PrimitiveId::ECDSA_P256_SHA256;

// =========================================================================================
// The context bridge
// =========================================================================================

/// Adapts [`NodeCtx`] to the [`Ctx`] that `v2xw-sec` is generic over.
///
/// # Why this is not a hole in the firewall
///
/// [`Ctx`] has `world()` and `actors()`; [`NodeCtx`] deliberately does not (build decision
/// D12.2). Bridging one to the other is exactly the shape of change that would re-open the
/// firewall, so the bridge is built so that it *cannot*: `World` and `Actors` are both the
/// unit type. There is no world here to return and no actor list to index — not "the node
/// promises not to look", but "there is nothing to look at".
///
/// That is a textual property, so [`crate::firewall::scan_security_bridge`] checks it
/// textually, and `tests/firewall_sentinel.rs` names the fault injected to prove the check
/// goes red.
///
/// # Scheduling
///
/// The security layer never schedules. `v2xw-sec`'s own test double records the same
/// observation — "a crypto backend needs `now()` and `rng()`, and an envelope needs
/// `now()`" — and this bridge turns it into an assertion: [`SecCtx::schedule`] panics.
/// A silently dropped event would be a queue that never fires and a cost never charged,
/// which is precisely the class of defect this crate keeps finding.
pub struct SecCtx<'a> {
    inner: &'a mut dyn NodeCtx,
    params: ParamSet,
    /// The unit value `world()` and `actors()` return. See the type documentation.
    nothing: (),
}

impl core::fmt::Debug for SecCtx<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SecCtx")
            .field("now", &self.inner.now())
            .finish_non_exhaustive()
    }
}

impl<'a> SecCtx<'a> {
    /// Wraps a node context for the length of one security operation.
    pub fn new(inner: &'a mut dyn NodeCtx) -> SecCtx<'a> {
        SecCtx {
            inner,
            params: ParamSet::new(),
            nothing: (),
        }
    }
}

impl Ctx for SecCtx<'_> {
    type World = ();
    type Actors = ();
    type Payload = ();

    fn now(&self) -> SimTime {
        self.inner.now()
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.inner.rng(domain, entity)
    }

    fn schedule(&mut self, _at: SimTime, _class: EventClass, _payload: ()) -> EventHandle {
        panic!(
            "the node's security bridge carries no scheduler: signing and verification are \
             synchronous, and the modelled time they take is charged against the node's own \
             service model (06-node-models.md §2.1). A security model that needs to schedule \
             needs the engine's context, not this one."
        )
    }

    fn cancel(&mut self, _handle: EventHandle) -> bool {
        false
    }

    fn world(&self) -> &Self::World {
        &self.nothing
    }

    fn actors(&self) -> &Self::Actors {
        &self.nothing
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        self.inner.emit_erased(record);
    }

    fn why(&mut self, _subject: ProvSubject, _model: ModelRef, _params: ParamSetId) {}

    fn params(&self) -> &ParamSet {
        &self.params
    }
}

// =========================================================================================
// The backend
// =========================================================================================

/// Which crypto backend a node runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CryptoMode {
    /// [`v2xw_sec::Modeled`]: right sizes, right outcomes, no curve arithmetic.
    #[default]
    Modeled,
    /// [`v2xw_sec::Real`]: ECDSA P-256 with RFC 6979.
    Real,
}

/// The node's crypto backend.
///
/// An enum and not a `Box<dyn CryptoBackend<_>>` because [`CryptoBackend`] is generic over
/// the context type and its object form would need a higher-ranked trait object Rust
/// cannot express. What matters for invariant I-S1 is not the dispatch mechanism but that
/// **nothing outside these two `match` arms knows which backend is running**: the sizes are
/// equal by construction (a modelled token is exactly the descriptor's `sig_bytes`, a
/// modelled public key is 33 bytes in the compressed shape), so every encoded length, every
/// record and every queueing decision is identical in both modes.
#[derive(Debug)]
pub enum NodeCrypto {
    /// The modelled backend.
    Modeled(Box<v2xw_sec::Modeled>),
    /// The real backend.
    Real(Box<v2xw_sec::Real>),
}

impl NodeCrypto {
    /// A backend for `mode`, holding no keys.
    pub fn new(mode: CryptoMode) -> NodeCrypto {
        match mode {
            CryptoMode::Modeled => NodeCrypto::Modeled(Box::new(v2xw_sec::Modeled::new())),
            CryptoMode::Real => NodeCrypto::Real(Box::new(v2xw_sec::Real::new())),
        }
    }

    /// Which mode this backend implements.
    pub fn mode(&self) -> CryptoMode {
        match self {
            NodeCrypto::Modeled(_) => CryptoMode::Modeled,
            NodeCrypto::Real(_) => CryptoMode::Real,
        }
    }

    /// The backend's model id, as a manifest records it.
    pub fn backend_id(&self) -> &'static str {
        match self {
            NodeCrypto::Modeled(b) => b.backend_id(),
            NodeCrypto::Real(b) => b.backend_id(),
        }
    }

    /// Generates a signing key for `owner` on the node's own deterministic stream.
    pub fn keygen(
        &mut self,
        ctx: &mut dyn NodeCtx,
        owner: NodeId,
    ) -> Result<KeyHandle, SecError> {
        let mut c = SecCtx::new(ctx);
        match self {
            NodeCrypto::Modeled(b) => b.keygen(&mut c, SIGN_PRIMITIVE, owner),
            NodeCrypto::Real(b) => b.keygen(&mut c, SIGN_PRIMITIVE, owner),
        }
    }

    /// The 33-byte compressed public material of a key this backend holds.
    pub fn public_material_of(&self, key: &KeyHandle) -> Result<Vec<u8>, SecError> {
        match self {
            NodeCrypto::Modeled(b) => b.public_material(&b.public_of(key)?),
            NodeCrypto::Real(b) => b.public_material(&b.public_of(key)?),
        }
    }

    /// Imports a peer's public key from the 33 bytes a certificate carries.
    pub fn import_public(
        &mut self,
        owner: NodeId,
        material: &[u8],
    ) -> Result<PubHandle, SecError> {
        match self {
            NodeCrypto::Modeled(b) => b.import_public(SIGN_PRIMITIVE, owner, material),
            NodeCrypto::Real(b) => b.import_public(SIGN_PRIMITIVE, owner, material),
        }
    }

    /// Signs a 32-byte digest.
    pub fn sign_prehashed(
        &mut self,
        ctx: &mut dyn NodeCtx,
        key: &KeyHandle,
        digest: &[u8; 32],
    ) -> Result<SigToken, SecError> {
        let mut c = SecCtx::new(ctx);
        match self {
            NodeCrypto::Modeled(b) => b.sign_prehashed(&mut c, key, digest),
            NodeCrypto::Real(b) => b.sign_prehashed(&mut c, key, digest),
        }
    }

    /// Checks a signature over a 32-byte digest.
    pub fn verify_prehashed(
        &mut self,
        ctx: &mut dyn NodeCtx,
        pk: &PubHandle,
        digest: &[u8; 32],
        sig: &SigToken,
    ) -> bool {
        let mut c = SecCtx::new(ctx);
        match self {
            NodeCrypto::Modeled(b) => b.verify_prehashed(&mut c, pk, digest, sig),
            NodeCrypto::Real(b) => b.verify_prehashed(&mut c, pk, digest, sig),
        }
    }

    /// Signs a payload into a `SignedData` SPDU through `envelope`.
    pub fn sign_envelope(
        &mut self,
        envelope: &Envelope,
        ctx: &mut dyn NodeCtx,
        signer: &SignerHandle,
        payload: &[u8],
        hdr: &HeaderInfoSpec,
        sid: SignerIdChoice,
    ) -> Result<SecuredPdu, SecError> {
        let mut c = SecCtx::new(ctx);
        match self {
            NodeCrypto::Modeled(b) => {
                envelope.sign(&mut c, b.as_mut(), signer, payload, hdr, sid)
            }
            NodeCrypto::Real(b) => envelope.sign(&mut c, b.as_mut(), signer, payload, hdr, sid),
        }
    }
}

// =========================================================================================
// The node's security stack
// =========================================================================================

/// What a receiver concluded about an SPDU's signature, having spent the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpduVerdict {
    /// The signature checks out against the certificate the SPDU named.
    Valid,
    /// The bytes do not decode, or the signature does not check out.
    Invalid,
    /// The signer's certificate is not held, so nothing has been concluded. This is the
    /// P2PCD case, and it is *not* a rejection: a node that answered `Invalid` here would
    /// blame a peer for its own empty cache.
    Unverifiable,
}

/// One signed frame: the payload, the SPDU, and the split between them.
///
/// The split is the whole point. `bytes_on_wire = payload_bytes + envelope_bytes`, and the
/// security overhead as a fraction of airtime — one of the three results this simulator
/// exists to produce — is `envelope_bytes / bytes_on_wire` aggregated over a run. With a
/// constant-size stub that ratio was a constant; here it moves with the message type, with
/// the signer-identifier cadence and with the certificate's own encoded size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedFrame {
    /// The facilities-layer payload, as the codec encoded it.
    pub payload: Vec<u8>,
    /// The IEEE 1609.2 `SignedData` SPDU: what actually goes on the air.
    pub spdu: Vec<u8>,
    /// The octets of the signer's certificate inside the envelope when a full certificate
    /// was attached (its COER encoding), zero when the signer was named by an eight-octet
    /// digest. What the certificate-versus-digest share of the envelope is computed from.
    pub cert_bytes: u32,
}

impl SignedFrame {
    /// The payload's length in octets.
    pub fn payload_bytes(&self) -> u32 {
        u32::try_from(self.payload.len()).unwrap_or(u32::MAX)
    }

    /// The SPDU's length in octets — payload plus envelope.
    pub fn bytes_on_wire(&self) -> u32 {
        u32::try_from(self.spdu.len()).unwrap_or(u32::MAX)
    }

    /// The security envelope's cost in octets: everything the SPDU carries that is not the
    /// payload.
    pub fn envelope_bytes(&self) -> u32 {
        self.bytes_on_wire().saturating_sub(self.payload_bytes())
    }
}

/// The stand-in issuing authority a node signs its own pseudonym certificates with.
#[derive(Debug)]
struct Issuer {
    key: KeyHandle,
    coer: Vec<u8>,
    digest: HashedId8,
}

/// A node's security stack: the envelope, the backend, and the credentials in use.
#[derive(Debug)]
pub struct NodeSecurity {
    envelope: Envelope,
    crypto: NodeCrypto,
    psid: u64,
    issuer: Option<Issuer>,
    /// One signer handle per pseudonym, keyed by `(i_period, j_index)`.
    ///
    /// Every credential the store holds gets one, not just the active one, and that is
    /// load-bearing rather than eager: a credential's `digest` is written back from the
    /// certificate here, the revocation path matches credentials *by digest*, and a
    /// certificate that only came into existence when the node rotated onto it would let
    /// a revoked node dodge its own revocation by rotating.
    signers: BTreeMap<(u32, u32), SignerHandle>,
    /// Which pseudonym is signing now.
    active: Option<(u32, u32)>,
    /// Public key handles for peers, keyed by certificate digest, so a repeat sender costs
    /// one import rather than one per message.
    peer_keys: BTreeMap<[u8; 8], PubHandle>,
    /// When a full certificate was last attached, per message type, for the
    /// [`SignerIdPolicy`] cadence of 05-protocols.md §2.4.
    last_certificate: BTreeMap<&'static str, SimTime>,
    cam_policy: SignerIdPolicy,
    bsm_policy: SignerIdPolicy,
}

impl NodeSecurity {
    /// A stack signing under `psid` on `wall`, with `mode`'s backend.
    pub fn new(wall: WallClock, mode: CryptoMode, psid: u64) -> NodeSecurity {
        NodeSecurity {
            envelope: Envelope::ieee1609(wall),
            crypto: NodeCrypto::new(mode),
            psid,
            issuer: None,
            signers: BTreeMap::new(),
            active: None,
            peer_keys: BTreeMap::new(),
            last_certificate: BTreeMap::new(),
            // 05-protocols.md §2.4: J2945/1 attaches a full certificate every 450 ms, the
            // ETSI stack once a second. Two cadences because they are two standards, and
            // the difference is real bytes on the air.
            cam_policy: SignerIdPolicy::ETSI_1S,
            bsm_policy: SignerIdPolicy::SAE_450MS,
        }
    }

    /// The same stack under a different envelope profile.
    #[must_use]
    pub fn with_profile(mut self, profile: EnvelopeProfile, wall: WallClock) -> NodeSecurity {
        self.envelope = match profile {
            EnvelopeProfile::Ieee1609Dot2 => Envelope::ieee1609(wall),
            EnvelopeProfile::EtsiTs103097 => Envelope::etsi(wall),
        };
        self
    }

    /// The same stack with different certificate-attachment cadences.
    #[must_use]
    pub fn with_signer_id_policies(
        mut self,
        cam: SignerIdPolicy,
        bsm: SignerIdPolicy,
    ) -> NodeSecurity {
        self.cam_policy = cam;
        self.bsm_policy = bsm;
        self
    }

    /// Which backend is running.
    pub fn mode(&self) -> CryptoMode {
        self.crypto.mode()
    }

    /// The backend, for a caller that needs to import a peer key directly.
    pub fn crypto_mut(&mut self) -> &mut NodeCrypto {
        &mut self.crypto
    }

    /// The envelope.
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }

    /// The signer handle currently in use, once one has been provisioned and selected.
    pub fn signer(&self) -> Option<&SignerHandle> {
        self.active.and_then(|k| self.signers.get(&k))
    }

    /// The signer handle for one pseudonym, if it has been provisioned.
    pub fn signer_for_pseudonym(&self, i_period: u32, j_index: u32) -> Option<&SignerHandle> {
        self.signers.get(&(i_period, j_index))
    }

    /// Selects the pseudonym that signs from now on. `false` if it is not provisioned.
    pub fn set_active(&mut self, i_period: u32, j_index: u32) -> bool {
        if self.signers.contains_key(&(i_period, j_index)) {
            self.active = Some((i_period, j_index));
            true
        } else {
            false
        }
    }

    /// The PSID messages are signed under.
    pub fn psid(&self) -> u64 {
        self.psid
    }

    /// Seconds from the IEEE 1609.2 epoch to `at` on this stack's wall clock.
    fn epoch_seconds(&self, at: SimTime) -> u32 {
        let unix = self
            .envelope
            .wall_clock()
            .t0_unix_s()
            .saturating_add((at / 1_000_000_000) as i64);
        u32::try_from(unix.saturating_sub(IEEE1609_EPOCH_UNIX_S)).unwrap_or(0)
    }

    /// Provisions — or reuses — the signer handle for one credential.
    ///
    /// The stand-in for the `CredentialProtocol` of 03-interfaces.md §7; see the module
    /// documentation for exactly what it does and does not model. One authority per node,
    /// one pseudonym certificate per `(i_period, j_index)`, both on the node's own
    /// deterministic crypto stream, so two runs of the same scenario produce the same
    /// certificates and therefore the same `HashedId8`s.
    pub fn provision(
        &mut self,
        ctx: &mut dyn NodeCtx,
        node: NodeId,
        i_period: u32,
        j_index: u32,
        at: SimTime,
    ) -> Result<&SignerHandle, SecError> {
        if self.signers.contains_key(&(i_period, j_index)) {
            return Ok(&self.signers[&(i_period, j_index)]);
        }
        let valid_from = self.epoch_seconds(at);

        if self.issuer.is_none() {
            let key = self.crypto.keygen(ctx, node)?;
            let material = self.crypto.public_material_of(&key)?;
            let certificate =
                cert::trust_anchor(&CertSpec::authority(valid_from, self.psid), &material)?;
            let coer = cert::encode(&certificate)?;
            let digest = hashedid::hashed_id8(&coer);
            self.issuer = Some(Issuer { key, coer, digest });
        }
        let issuer = self.issuer.as_ref().expect("just set");

        let key = self.crypto.keygen(ctx, node)?;
        let material = self.crypto.public_material_of(&key)?;
        // The linkage value is the real CAMP SCP2 construction over seeds derived from the
        // node id, so a linked CRL that revokes `(i, j)` revokes exactly this certificate
        // and the revocation path has something true to match on.
        let device = device_linkage(node);
        let spec = CertSpec::pseudonym(
            issuer.digest.clone(),
            HolderId::Linkage {
                i_cert: u16::try_from(i_period % u32::from(u16::MAX)).unwrap_or(0),
                linkage_value: device.linkage_value_for(i_period, j_index),
            },
            valid_from,
            self.psid,
        );
        let issuer_key = issuer.key;
        let issuer_coer = issuer.coer.clone();
        // Two steps because the signature covers `toBeSigned`: the certificate cannot
        // exist until its own issuer has signed the bytes that will be inside it.
        let tbs = cert::to_be_signed(&spec, cert::verification_key_indicator(&material)?)?;
        let digest = cert::explicit_signature_input(&tbs, &issuer_coer)?;
        let token = self.crypto.sign_prehashed(ctx, &issuer_key, &digest)?;
        let certificate = cert::explicit(&spec, &material, token.to_ieee1609_signature()?)?;

        let handle = SignerHandle::new(node, key, Arc::new(certificate))?;
        self.signers.insert((i_period, j_index), handle);
        Ok(&self.signers[&(i_period, j_index)])
    }

    /// Whether a full certificate is due for `msg_type` at `believed`.
    ///
    /// The cadence of 05-protocols.md §2.4, resolved through the envelope so that the
    /// profile's own rules (TS 103 097 §7.1.2 makes a DENM's signer always a certificate)
    /// apply first and no caller can forget them.
    pub fn signer_id_for(&self, msg_type: MsgType, believed: SimTime) -> SignerIdChoice {
        let policy = match msg_type {
            MsgType::Bsm => self.bsm_policy,
            _ => self.cam_policy,
        };
        self.envelope.choose_signer_id(
            policy,
            believed,
            self.last_certificate.get(msg_type.as_str()).copied(),
            Some(msg_type),
        )
    }

    /// Signs `payload` and returns the SPDU with the payload beside it.
    ///
    /// The caller must have called [`NodeSecurity::provision`] for the credential in use;
    /// signing without a credential is [`SecError`], never a zero-length signature.
    pub fn sign(
        &mut self,
        ctx: &mut dyn NodeCtx,
        msg_type: MsgType,
        payload: &[u8],
        sid: SignerIdChoice,
        location: Option<GenerationLocation>,
    ) -> Result<(SignedFrame, SecuredPdu), SecError> {
        let signer = self.signer().cloned().ok_or(SecError::MissingField {
            profile: "node/security",
            field: "no credential has been provisioned; a node without one stops \
                    transmitting rather than sending unsigned [CAMP-EE §2.2.10.2]",
        })?;
        let hdr = HeaderInfoSpec {
            psid: self.psid,
            msg_type: Some(msg_type),
            generation_location: location,
            ..HeaderInfoSpec::default()
        };
        let pdu = self
            .crypto
            .sign_envelope(&self.envelope, ctx, &signer, payload, &hdr, sid)?;
        let cert_bytes = if pdu.signer_id == v2xw_sec::SignerIdChoice::Certificate {
            u32::try_from(signer.cert_coer().len()).unwrap_or(u32::MAX)
        } else {
            0
        };
        let frame = SignedFrame {
            payload: payload.to_vec(),
            spdu: pdu.bytes().to_vec(),
            cert_bytes,
        };
        Ok((frame, pdu))
    }

    /// Records that a full certificate went out for `msg_type` at `believed`.
    pub fn note_certificate_attached(&mut self, msg_type: MsgType, believed: SimTime) {
        self.last_certificate.insert(msg_type.as_str(), believed);
    }

    /// Decodes an SPDU, or `None` when the bytes are not a `SignedData` this profile
    /// admits.
    ///
    /// Separate from [`NodeSecurity::verify_parsed`] because the caller has to resolve the
    /// signer's certificate in between, out of its own bounded cache — which is what makes
    /// P2PCD a cost rather than a formality.
    pub fn parse(&self, bytes: &[u8]) -> Option<ParsedSecured> {
        self.envelope.parse(bytes).ok()
    }

    /// Checks a parsed SPDU's signature against `certificate`.
    ///
    /// What this does **not** do is validate the certificate's chain to a trust anchor.
    /// See the module documentation: with no `CredentialProtocol` there is no common
    /// anchor to chain to, and answering `Valid` on a chain nobody checked would be the
    /// same class of defect as the constant-size stub this module replaced.
    pub fn verify_parsed(
        &mut self,
        ctx: &mut dyn NodeCtx,
        parsed: &ParsedSecured,
        certificate: &Certificate,
        owner: NodeId,
    ) -> SpduVerdict {
        let Ok(cert_coer) = cert::encode(certificate) else {
            return SpduVerdict::Invalid;
        };
        let digest = parsed.signing_digest(&cert_coer);
        // Material that is not a compressed point is a malformed certificate, which is the
        // sender's fault and a rejection rather than an inconclusive answer.
        let Ok(key) = self.peer_key(owner, certificate, &cert_coer) else {
            return SpduVerdict::Invalid;
        };
        let token = SigToken {
            primitive: SIGN_PRIMITIVE,
            bytes: parsed.signature.clone(),
        };
        if self.crypto.verify_prehashed(ctx, &key, &digest, &token) {
            SpduVerdict::Valid
        } else {
            SpduVerdict::Invalid
        }
    }

    /// The certificate a parsed SPDU attached, if it attached one.
    pub fn attached_certificate(parsed: &ParsedSecured) -> Option<Arc<Certificate>> {
        match &parsed.signer {
            ParsedSigner::Certificate { certificate, .. } => {
                Some(Arc::new((**certificate).clone()))
            }
            ParsedSigner::Digest(_) | ParsedSigner::SelfSigned => None,
        }
    }

    /// The digest a parsed SPDU names its signer by, whichever identifier it used.
    pub fn parsed_signer_digest(parsed: &ParsedSecured) -> Option<HashedId8> {
        match &parsed.signer {
            ParsedSigner::Certificate { digest, .. } => Some(digest.clone()),
            ParsedSigner::Digest(d) => Some(d.clone()),
            ParsedSigner::SelfSigned => None,
        }
    }

    fn peer_key(
        &mut self,
        owner: NodeId,
        certificate: &Certificate,
        cert_coer: &[u8],
    ) -> Result<PubHandle, SecError> {
        let mut id = [0u8; 8];
        id.copy_from_slice(&hashedid::hashed_id8(cert_coer).0[..]);
        if let Some(handle) = self.peer_keys.get(&id) {
            return Ok(*handle);
        }
        let material = cert::public_key_material(certificate)?;
        let handle = self.crypto.import_public(owner, &material)?;
        self.peer_keys.insert(id, handle);
        Ok(handle)
    }
}

/// The linkage context for a node's own pseudonym certificates.
///
/// Derived from the node id so that it is a pure function of the scenario, and split
/// across two linkage authorities exactly as CAMP SCP2 requires: neither authority's seed
/// alone reveals the linkage value, which is the property the whole construction exists
/// for [Brecht 2018 §V-B].
fn device_linkage(node: NodeId) -> DeviceLinkageContext {
    let n = node.index();
    let mut s1 = [0u8; 16];
    let mut s2 = [0u8; 16];
    for (i, b) in s1.iter_mut().enumerate() {
        *b = (n.wrapping_mul(0x9E37).wrapping_add(i as u32) & 0xFF) as u8;
    }
    for (i, b) in s2.iter_mut().enumerate() {
        *b = (n.wrapping_mul(0x85EB).wrapping_add(i as u32 + 0x40) & 0xFF) as u8;
    }
    DeviceLinkageContext::new(
        LaId(0x0001),
        LaId(0x0002),
        LinkageSeed::new(s1),
        LinkageSeed::new(s2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::NodeRuntimeCtx;
    use v2xw_core::rng::RngRegistry;

    fn stack(mode: CryptoMode) -> NodeSecurity {
        NodeSecurity::new(WallClock::new(1_767_225_600), mode, PSID_SAFETY)
    }

    /// The bridge carries no truth: both associated types are the unit type, so there is
    /// nothing for a security model to read even if it asked.
    #[test]
    fn the_security_bridge_carries_no_world_and_no_actors() {
        let reg = RngRegistry::new(1);
        let mut inner = NodeRuntimeCtx::new(0, &reg);
        let c = SecCtx::new(&mut inner);
        assert_eq!(core::mem::size_of_val(c.world()), 0);
        assert_eq!(core::mem::size_of_val(c.actors()), 0);
    }

    /// A node provisions once per credential and reuses the handle.
    #[test]
    fn provisioning_is_idempotent_per_credential() {
        let reg = RngRegistry::new(2);
        let mut ctx = NodeRuntimeCtx::new(0, &reg);
        let mut s = stack(CryptoMode::Real);
        let first = s
            .provision(&mut ctx, NodeId::new(1), 3, 0, 0)
            .expect("provisions")
            .digest()
            .clone();
        let again = s
            .provision(&mut ctx, NodeId::new(1), 3, 0, 0)
            .expect("provisions")
            .digest()
            .clone();
        assert_eq!(first, again);
        let rotated = s
            .provision(&mut ctx, NodeId::new(1), 3, 1, 0)
            .expect("provisions")
            .digest()
            .clone();
        assert_ne!(
            first, rotated,
            "a new pseudonym must be a new certificate, or rotation buys nothing"
        );
    }

    /// Both backends produce certificates and SPDUs of identical size — half of I-S1, and
    /// the half a size model depends on.
    #[test]
    fn the_two_backends_agree_on_every_size() {
        let reg = RngRegistry::new(3);
        let payload = b"a payload of some length";
        let sizes = |mode| {
            let mut ctx = NodeRuntimeCtx::new(0, &reg);
            let mut s = stack(mode);
            s.provision(&mut ctx, NodeId::new(1), 0, 0, 0).expect("prov");
            assert!(s.set_active(0, 0));
            let cert_len = s.signer().expect("signer").cert_coer().len();
            let (frame, pdu) = s
                .sign(
                    &mut ctx,
                    MsgType::Cam,
                    payload,
                    SignerIdChoice::Digest,
                    None,
                )
                .expect("signs");
            (cert_len, frame.bytes_on_wire(), pdu.overhead())
        };
        assert_eq!(sizes(CryptoMode::Real), sizes(CryptoMode::Modeled));
    }
}
