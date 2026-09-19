//! The security envelope: IEEE 1609.2 `SignedData` and the ETSI TS 103 097 profile.
//!
//! Real COER through the generated bindings, not a size model. A [`SecuredPdu`]'s bytes
//! are bytes a conformant receiver would accept, and its size is therefore exact by
//! construction rather than by a table (invariant I-S3).
//!
//! # What signing actually computes
//!
//! IEEE 1609.2 §5.3.1 does not sign the message. It signs
//!
//! ```text
//! H( H(tbsData) ‖ H(signer identifier input) )
//! ```
//!
//! where the *signer identifier input* is the COER encoding of the signer's own
//! certificate — **including when the signer identifier on the wire is only a digest**.
//! That is the detail that makes a hand-rolled implementation interoperate with nothing:
//! attaching a digest saves 70-odd bytes on the wire but changes nothing about what was
//! signed, because the receiver looks the certificate up and hashes it itself. So
//! [`SignerHandle`] carries the certificate's encoding whichever identifier policy is in
//! force, and [`Envelope::sign`] hashes it either way.
//!
//! # The two profiles
//!
//! `EtsiTs103097Data` is a *profile* of `Ieee1609Dot2Data`, not a different type — the
//! generated binding is a delegate newtype, so the bytes are identical and there is no
//! separate trailer [TS 103 097 V2.1.1 §5.1-5.2]. What differs is what is allowed:
//! `generationTime` is mandatory, `p2pcdLearningRequest` and `missingCrlIdentifier` are
//! forbidden, and DENM must carry `generationLocation` and must sign with a certificate
//! [§7.1.2]. [`EnvelopeProfile::EtsiTs103097`] enforces those at signing time rather than
//! trusting the caller, because a profile violation produces bytes that decode fine and
//! are rejected in the field.
//!
//! # Deviations from 03-interfaces.md §6, and why
//!
//! The crate is normative for signatures and the document tracks it (build decision D11).
//! Three differences:
//!
//! * **Generic over the context.** `&mut dyn Ctx` is not a type — [`Ctx`] has three
//!   associated types with no defaults — so the published signature does not compile.
//!   Parameterising over `C: Ctx + ?Sized` is what `v2xw-msg` already does for
//!   `MessageGenerator`, and with `?Sized` the parameter may itself be a trait object.
//! * **`sign` takes the crypto backend, and takes `&self`.** The published signature has
//!   the envelope owning its backend and mutating itself. Passing the backend makes the
//!   envelope stateless, which has two concrete consequences: it is `Sync`, so it can be
//!   called from inside the phase-parallel maps of 02-architecture.md §6.4; and the *same*
//!   envelope instance can be driven by the real and the modelled backend in one test,
//!   which is what makes the I-S1 equivalence test evidence rather than a re-run.
//! * **`sign` takes a [`HeaderInfoSpec`] rather than a `HeaderInfo`.** The envelope fills
//!   `generationTime` from the simulation clock and `generationLocation` from the
//!   profile's rules; handing it a finished `HeaderInfo` would mean every caller
//!   duplicating the 1609.2 epoch conversion, and one of them getting it wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use v2xw_core::card::{Family, ModelCard, Source, SourceKind, Tier, Validation, ValidationStatus};
use v2xw_core::ctx::Ctx;
use v2xw_core::hash::sha256;
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime, WallClock};
use v2xw_msg::codec::{Encoded, MsgType, SizeSource};
use v2xw_msg::sec_types::{
    Certificate, CertificateId, CertificateType, EccP256CurvePoint, HashAlgorithm, HashedId3,
    HashedId8, HeaderInfo, Ieee1609Dot2Content, Ieee1609Dot2Data, Ieee1609Duration,
    IssuerIdentifier, Opaque, Psid, SequenceOfCertificate, Signature, SignedData,
    SignedDataPayload, SignerIdentifier, Time64, ToBeSignedData, Uint8, ValidityPeriod, coer,
    ieee1609_dot2_base_types::{HashedId10, SequenceOfHashedId3, Uint64},
};

use crate::crypto::CryptoBackend;
use crate::error::{Result, SecError};
use crate::hashedid::{certificate_digest, certificate_hashed_id10, hashed_id3_of_id8, id8_bytes};
use crate::linkage::{CrlLinkageEntry, LinkageValue};
use crate::primitive::{PrimitiveCatalogue, PrimitiveId, PrimitiveOpKind};

/// The model id of the IEEE 1609.2 envelope.
pub const IEEE_1609_2_ID: &str = "envelope/ieee-1609-2";

/// The model id of the ETSI TS 103 097 envelope.
pub const ETSI_TS_103_097_ID: &str = "envelope/etsi-ts103097";

/// The IEEE 1609.2 protocol version every SPDU in this simulator carries.
pub const PROTOCOL_VERSION: u8 = 3;

/// Which profile an envelope implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvelopeProfile {
    /// IEEE 1609.2 `SignedData`, unconstrained.
    Ieee1609Dot2,
    /// The ETSI TS 103 097 profile of the same structure.
    EtsiTs103097,
}

impl EnvelopeProfile {
    /// The model id of the envelope implementing this profile.
    pub const fn model_id(self) -> &'static str {
        match self {
            EnvelopeProfile::Ieee1609Dot2 => IEEE_1609_2_ID,
            EnvelopeProfile::EtsiTs103097 => ETSI_TS_103_097_ID,
        }
    }
}

impl core::fmt::Display for EnvelopeProfile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.model_id())
    }
}

/// Which signer identifier went on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignerIdChoice {
    /// The signer's certificate digest, 9 encoded bytes.
    Digest,
    /// The signer's whole certificate.
    Certificate,
}

/// When to attach the whole certificate instead of a digest (04-models.md §9.5).
///
/// The scenario's `SignerIdPolicy`. SAE's schedule is a full certificate every 450 ms
/// (Rostami 2018 Table 1) or, as the industry cadence NDSS 2024 restates from J2945/1,
/// every fifth SPDU; ETSI's is once a second after the last inclusion [TS 103 097
/// §7.1.1]. Both are expressed here as an interval, which is what the two sources agree
/// on the shape of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignerIdPolicy {
    /// Attach the certificate when this long has passed since the last attachment.
    /// `None` means never attach unless something else forces it.
    pub full_cert_every: Option<Duration>,
    /// Attach the certificate on every message, whatever the interval says.
    pub always_certificate: bool,
}

impl SignerIdPolicy {
    /// Digest only, until the profile forces a certificate.
    pub const DIGEST_ONLY: SignerIdPolicy = SignerIdPolicy {
        full_cert_every: None,
        always_certificate: false,
    };

    /// A certificate on every message — the DENM rule of TS 103 097 §7.1.2.
    pub const ALWAYS_CERTIFICATE: SignerIdPolicy = SignerIdPolicy {
        full_cert_every: None,
        always_certificate: true,
    };

    /// The SAE cadence: a full certificate every 450 ms.
    pub const SAE_450MS: SignerIdPolicy = SignerIdPolicy {
        full_cert_every: Some(Duration::from_millis(450)),
        always_certificate: false,
    };

    /// The ETSI cadence: a full certificate one second after the last inclusion.
    pub const ETSI_1S: SignerIdPolicy = SignerIdPolicy {
        full_cert_every: Some(Duration::from_millis(1000)),
        always_certificate: false,
    };

    /// Which identifier to use at `now`, given when the certificate was last attached.
    ///
    /// A pure function of its three arguments, so the envelope stays stateless and the
    /// node runtime owns the "when did I last attach" bookkeeping. The first message a
    /// signer sends always carries the certificate: a receiver that has never seen it has
    /// nothing to resolve a digest against, and a first message with a digest would cost
    /// a P2PCD round trip to save 70 bytes.
    pub fn choose(self, now: SimTime, last_attached: Option<SimTime>) -> SignerIdChoice {
        if self.always_certificate {
            return SignerIdChoice::Certificate;
        }
        match (self.full_cert_every, last_attached) {
            (_, None) => SignerIdChoice::Certificate,
            (None, Some(_)) => SignerIdChoice::Digest,
            (Some(every), Some(last)) => {
                if Duration::between(last, now) >= every {
                    SignerIdChoice::Certificate
                } else {
                    SignerIdChoice::Digest
                }
            }
        }
    }
}

/// Everything the envelope needs about the signer.
///
/// Holds the certificate's COER encoding, not just the certificate: it is hashed on every
/// signature (see the module documentation) and re-encoding it per message would be both
/// wasteful and a place for a non-canonical encoding to creep in.
#[derive(Debug, Clone)]
pub struct SignerHandle {
    /// The signing node.
    pub node: NodeId,
    /// The private key, in whichever backend will be asked to sign.
    pub key: crate::crypto::KeyHandle,
    /// The signer's own certificate.
    pub certificate: Arc<Certificate>,
    /// Its canonical COER encoding — the signer identifier input of §5.3.1.
    cert_coer: Arc<Vec<u8>>,
    /// Its whole-certificate hash.
    digest: HashedId8,
}

impl SignerHandle {
    /// Builds a handle, encoding and hashing the certificate once.
    pub fn new(
        node: NodeId,
        key: crate::crypto::KeyHandle,
        certificate: Arc<Certificate>,
    ) -> Result<SignerHandle> {
        let cert_coer = coer::encode(MsgType::Crl, certificate.as_ref())?;
        let digest = crate::hashedid::hashed_id8(&cert_coer);
        Ok(SignerHandle {
            node,
            key,
            certificate,
            cert_coer: Arc::new(cert_coer),
            digest,
        })
    }

    /// The signer's certificate digest, as it appears in a `digest` signer identifier.
    pub fn digest(&self) -> &HashedId8 {
        &self.digest
    }

    /// The canonical COER encoding of the certificate.
    pub fn cert_coer(&self) -> &[u8] {
        &self.cert_coer
    }
}

/// The header fields a caller supplies; the envelope fills the rest.
#[derive(Debug, Clone, Default)]
pub struct HeaderInfoSpec {
    /// The PSID / ITS-AID this SPDU is sent under.
    pub psid: u64,
    /// Which message is being protected. Drives the profile's per-message rules.
    pub msg_type: Option<MsgType>,
    /// The signer's position, as `generationLocation` carries it. Required for DENM
    /// under TS 103 097 §7.1.2.
    pub generation_location: Option<GenerationLocation>,
    /// How long after `generationTime` the SPDU expires.
    pub expiry: Option<Duration>,
    /// An out-of-band P2PCD learning request. Forbidden by the ETSI profile.
    pub p2pcd_learning_request: Option<HashedId3>,
    /// An inline P2PCD request naming unknown certificates.
    pub inline_p2pcd_request: Vec<HashedId3>,
}

/// A `generationLocation`, in the units IEEE 1609.2 carries.
///
/// Tenths of a microdegree for latitude and longitude — the same 1e-7 degree grid build
/// decision D9 fixes for every geodetic float in the simulator, so a position that has
/// been quantised for recording converts to this exactly. Elevation is in decimetres
/// above the WGS-84 ellipsoid with a −4,096 dm offset, as `Elevation` declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationLocation {
    /// Latitude in tenths of a microdegree.
    pub lat_tenth_microdeg: i32,
    /// Longitude in tenths of a microdegree.
    pub lon_tenth_microdeg: i32,
    /// Elevation, as the 16-bit `Elevation` encoding.
    pub elevation: u16,
}

impl GenerationLocation {
    /// The 1609.2 `ThreeDLocation`.
    pub fn to_asn1(self) -> v2xw_msg::sec_types::ieee1609_dot2_base_types::ThreeDLocation {
        use v2xw_msg::sec_types::ieee1609_dot2_base_types as bt;
        bt::ThreeDLocation::new(
            bt::Latitude(bt::NinetyDegreeInt(self.lat_tenth_microdeg)),
            bt::Longitude(bt::OneEightyDegreeInt(self.lon_tenth_microdeg)),
            bt::Elevation(bt::Uint16(self.elevation)),
        )
    }

    /// The location from the 1609.2 type.
    pub fn from_asn1(v: &v2xw_msg::sec_types::ieee1609_dot2_base_types::ThreeDLocation) -> Self {
        GenerationLocation {
            lat_tenth_microdeg: v.latitude.0.0,
            lon_tenth_microdeg: v.longitude.0.0,
            elevation: v.elevation.0.0,
        }
    }
}

/// A signed protocol data unit: the bytes, and what was decided while making them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecuredPdu {
    /// The COER bytes and their exact size.
    pub encoded: Encoded,
    /// Which signer identifier went on the wire.
    pub signer_id: SignerIdChoice,
    /// The protected payload's length, so the overhead is arithmetic rather than a table
    /// lookup.
    pub payload_bytes: u32,
    /// This SPDU's own `HashedId8`, for logging, deduplication and misbehaviour reports.
    pub digest: HashedId8,
}

impl SecuredPdu {
    /// Bytes the envelope added: total minus payload.
    ///
    /// Invariant I-S3 in one line: a `SecuredPdu`'s size equals the payload plus the
    /// overhead the profile tables predict, and the real-encoder tier asserts equality
    /// rather than tolerance.
    pub fn overhead(&self) -> u32 {
        self.encoded.size.saturating_sub(self.payload_bytes)
    }

    /// The encoded bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.encoded.bytes
    }

    /// The total encoded size.
    pub fn size(&self) -> u32 {
        self.encoded.size
    }

    /// True when the bytes really are COER and the declared size is the byte count.
    ///
    /// The guard for invariant I-S3's premise: an SPDU whose `size_source` were
    /// `SizeModel` would carry a modelled length with placeholder bytes, and every
    /// overhead figure derived from it would be a restatement of the model rather than a
    /// measurement of the encoder.
    pub fn is_real_coer(&self) -> bool {
        self.encoded.size_source == SizeSource::Coer
            && self.encoded.size as usize == self.encoded.bytes.len()
    }
}

/// Who signed a parsed SPDU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSigner {
    /// A certificate digest; the receiver must resolve it.
    Digest(HashedId8),
    /// The whole certificate, with its computed digest.
    Certificate {
        /// The attached certificate.
        certificate: Box<Certificate>,
        /// Its whole-certificate hash.
        digest: HashedId8,
    },
    /// `self`: a self-signed SPDU, which no V2X profile admits.
    SelfSigned,
}

/// A decoded SPDU, with everything a verifier needs already extracted.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSecured {
    /// The profile it was parsed under.
    pub profile: EnvelopeProfile,
    /// Who signed it.
    pub signer: ParsedSigner,
    /// The PSID from the header.
    pub psid: u64,
    /// `generationTime`, in microseconds since the 1609.2 epoch.
    pub generation_time: Option<u64>,
    /// `generationLocation`, where present.
    pub generation_location: Option<GenerationLocation>,
    /// Inline P2PCD requests the sender attached.
    pub inline_p2pcd_request: Vec<HashedId3>,
    /// The protected payload.
    pub payload: Vec<u8>,
    /// The canonical COER re-encoding of `tbsData` — what the signature is computed over.
    pub tbs_coer: Vec<u8>,
    /// The signature, as `r ‖ s`.
    pub signature: Vec<u8>,
    /// The hash algorithm the signer declared.
    pub hash_id: HashAlgorithm,
    /// The whole SPDU's size in bytes.
    pub size: u32,
}

impl ParsedSecured {
    /// The digest this SPDU's signature is over: `H( H(tbsData) ‖ H(signer certificate) )`
    /// [IEEE 1609.2 §5.3.1].
    ///
    /// `signer_cert_coer` is the canonical COER encoding of the signer's certificate —
    /// attached to the SPDU, or looked up in the peer cache when the signer identifier was
    /// a digest. On the receive side this is the whole of what "verify the signature"
    /// means beyond the point arithmetic, and it lives here so that a receiver cannot
    /// accidentally hash only the `tbsData`: that variant passes its own round-trip tests
    /// and fails against every real stack.
    pub fn signing_digest(&self, signer_cert_coer: &[u8]) -> [u8; 32] {
        signing_digest(&self.tbs_coer, signer_cert_coer)
    }

    /// The signer's certificate digest, however it was carried.
    pub fn signer_digest(&self) -> Option<&HashedId8> {
        match &self.signer {
            ParsedSigner::Digest(d) => Some(d),
            ParsedSigner::Certificate { digest, .. } => Some(digest),
            ParsedSigner::SelfSigned => None,
        }
    }
}

/// One primitive operation a receiver must perform.
///
/// The node runtime charges each one against the receiving node's hardware profile and
/// queues it; a [`crate::primitive::PrimitiveId`] on every variant is what makes that a
/// table lookup rather than a special case per operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimitiveOp {
    /// Hash `bytes` bytes.
    Hash {
        /// Which hash.
        primitive: PrimitiveId,
        /// How many bytes go in.
        bytes: u32,
        /// What is being hashed.
        what: HashSubject,
    },
    /// Verify a signature.
    VerifySignature {
        /// Which signature scheme.
        primitive: PrimitiveId,
        /// What the signature covers.
        over: SigSubject,
    },
    /// Reconstruct an implicit certificate's public key (SEC 4 §3.5).
    ReconstructImplicit {
        /// Which implicit-certificate scheme.
        primitive: PrimitiveId,
    },
    /// Look a certificate up in a store. Not cryptography, but it is work, it can miss,
    /// and a miss is what triggers P2PCD — so the plan names it.
    StoreLookup {
        /// Which store.
        store: StoreKind,
        /// The key looked up.
        key: HashedId8,
    },
}

impl PrimitiveOp {
    /// The modelled duration of this operation on `profile`.
    ///
    /// Two operations are charged as zero, both deliberately and both stated in the
    /// primitive descriptors rather than invented here:
    ///
    /// * a **store lookup** is memory work whose cost belongs to the node model, not to a
    ///   cryptographic cost table;
    /// * a **hash with no published anchor** — which is every profile, since 04-models.md
    ///   §9.4 tabulates no SHA-256 figure — because hashing a V2X-sized message is
    ///   negligible beside the point multiplication in the same plan. The
    ///   `primitive/sha-256` descriptor's own note says so, and a plan that returned
    ///   `None` for its total because of it would make every verification cost
    ///   unknowable.
    ///
    /// A **signature verification** with no anchor is `None`, not zero: that one is the
    /// dominant term, and a plan missing it is not a plan whose cost is nearly right.
    pub fn cost(&self, catalogue: &PrimitiveCatalogue, profile: &str) -> Option<Duration> {
        match self {
            PrimitiveOp::StoreLookup { .. } => Some(Duration::ZERO),
            PrimitiveOp::Hash { primitive, .. } => Some(
                catalogue
                    .get(*primitive)
                    .and_then(|d| d.cost_duration(PrimitiveOpKind::Verify, profile, catalogue))
                    .unwrap_or(Duration::ZERO),
            ),
            PrimitiveOp::VerifySignature { primitive, .. }
            | PrimitiveOp::ReconstructImplicit { primitive } => catalogue
                .get(*primitive)?
                .cost_duration(PrimitiveOpKind::Verify, profile, catalogue),
        }
    }
}

/// What a [`PrimitiveOp::Hash`] covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HashSubject {
    /// The SPDU's `tbsData`.
    ToBeSignedData,
    /// The signer identifier input — the signer's certificate.
    SignerIdentifier,
    /// A certificate being validated in its own right.
    Certificate,
}

/// What a [`PrimitiveOp::VerifySignature`] covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SigSubject {
    /// The SPDU itself.
    Spdu,
    /// The attached certificate, against its issuer.
    Certificate,
}

/// Which store a lookup hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreKind {
    /// The peer certificate cache.
    PeerCertCache,
    /// The trust anchors.
    TrustStore,
    /// The CRL store.
    CrlStore,
}

/// Whether a plan can be executed, and what to do if not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOutcome {
    /// Every input is present; running the ops decides validity.
    Verifiable,
    /// A certificate is missing. The SPDU cannot be verified until P2PCD supplies it.
    NeedCertificate(P2pcdRequest),
    /// The SPDU is invalid for a reason no amount of computation will change.
    Reject(RejectReason),
}

/// What P2PCD must fetch (IEEE 1609.2 clause 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P2pcdRequest {
    /// The unknown certificate's whole-certificate hash.
    pub unknown: HashedId8,
    /// The `HashedId3` an out-of-band `p2pcdLearningRequest` carries. Derived from
    /// `unknown` by truncation, not by a second hash.
    pub id3: HashedId3,
    /// Whether the missing certificate is an end entity's or a CA's.
    pub kind: P2pcdKind,
}

/// Which kind of certificate P2PCD is being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum P2pcdKind {
    /// The signer's own certificate, named by a `digest` signer identifier.
    EndEntity,
    /// The issuer of an attached certificate.
    CertificateAuthority,
}

/// Why an SPDU is rejected outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RejectReason {
    /// The signer's certificate is on the CRL.
    Revoked,
    /// `generationTime` is outside the signer certificate's validity period.
    OutsideValidityPeriod,
    /// The SPDU is self-signed, which no V2X profile admits.
    SelfSigned,
    /// The signer declared a hash algorithm this envelope does not implement.
    UnsupportedHashAlgorithm,
    /// The attached certificate's issuer is `self` but it is not a trust anchor.
    UntrustedSelfSignedIssuer,
}

/// A verification plan: the operations, and whether they can be run at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyPlan {
    /// The operations, in the order a receiver performs them.
    pub ops: Vec<PrimitiveOp>,
    /// Whether the plan is executable.
    pub outcome: PlanOutcome,
}

impl VerifyPlan {
    /// The total modelled cost of the plan on `profile`.
    ///
    /// `None` if any operation has no cost anchor for that profile, because a partial sum
    /// would understate the load in exactly the situation — an unbenchmarked platform —
    /// where a reader most needs to know the number is incomplete.
    pub fn cost(&self, catalogue: &PrimitiveCatalogue, profile: &str) -> Option<Duration> {
        let mut total = Duration::ZERO;
        for op in &self.ops {
            total += op.cost(catalogue, profile)?;
        }
        Some(total)
    }

    /// True when the plan can be executed.
    pub fn is_verifiable(&self) -> bool {
        self.outcome == PlanOutcome::Verifiable
    }
}

// --------------------------------------------------------------------------------------
// Stores
// --------------------------------------------------------------------------------------

/// A certificate held in the peer cache, with whether its own signature has been checked.
#[derive(Debug, Clone)]
pub struct CachedCert {
    /// The certificate.
    pub certificate: Arc<Certificate>,
    /// True once its issuer's signature over it has been verified.
    pub verified: bool,
}

/// What a node knows about its neighbours' certificates.
///
/// Keyed by the raw `HashedId8` bytes in a `BTreeMap`, so iteration is ordered and a
/// report or a digest over the cache is reproducible (02-architecture.md §6.4).
#[derive(Debug, Clone, Default)]
pub struct PeerCertCache {
    certs: BTreeMap<[u8; 8], CachedCert>,
}

impl PeerCertCache {
    /// An empty cache.
    pub fn new() -> PeerCertCache {
        PeerCertCache::default()
    }

    /// Inserts a certificate, returning its digest. `verified` records whether its own
    /// signature has already been checked.
    pub fn insert(&mut self, certificate: Arc<Certificate>, verified: bool) -> Result<HashedId8> {
        let digest = certificate_digest(certificate.as_ref())?;
        self.certs.insert(
            id8_bytes(&digest),
            CachedCert {
                certificate,
                verified,
            },
        );
        Ok(digest)
    }

    /// The entry for a digest.
    pub fn get(&self, digest: &HashedId8) -> Option<&CachedCert> {
        self.certs.get(&id8_bytes(digest))
    }

    /// Marks a cached certificate as verified.
    pub fn mark_verified(&mut self, digest: &HashedId8) -> bool {
        match self.certs.get_mut(&id8_bytes(digest)) {
            Some(c) => {
                c.verified = true;
                true
            }
            None => false,
        }
    }

    /// The first cached certificate whose digest ends with `id3` — a P2PCD
    /// `p2pcdLearningRequest` match.
    ///
    /// Ordered iteration, so a collision between two certificates on three bytes resolves
    /// the same way on every platform. Three bytes collide often enough to matter:
    /// C2C-CC RS 2037 requires an authorization-ticket change on a *32-bit* collision, so
    /// a 24-bit one is a routine event, not a curiosity.
    pub fn find_by_id3(&self, id3: &HashedId3) -> Option<&CachedCert> {
        self.certs
            .iter()
            .find(|(k, _)| k[5..8] == id3.0[..])
            .map(|(_, v)| v)
    }

    /// How many certificates are cached.
    pub fn len(&self) -> usize {
        self.certs.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.certs.is_empty()
    }
}

/// The trust anchors a node will chain to.
#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    anchors: BTreeMap<[u8; 8], Arc<Certificate>>,
}

impl TrustStore {
    /// An empty store.
    pub fn new() -> TrustStore {
        TrustStore::default()
    }

    /// Adds an anchor, returning its digest.
    pub fn insert(&mut self, certificate: Arc<Certificate>) -> Result<HashedId8> {
        let digest = certificate_digest(certificate.as_ref())?;
        self.anchors.insert(id8_bytes(&digest), certificate);
        Ok(digest)
    }

    /// The anchor with this digest.
    pub fn get(&self, digest: &HashedId8) -> Option<&Arc<Certificate>> {
        self.anchors.get(&id8_bytes(digest))
    }

    /// True when this digest names a trust anchor.
    pub fn contains(&self, digest: &HashedId8) -> bool {
        self.anchors.contains_key(&id8_bytes(digest))
    }

    /// How many anchors.
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// What a node knows about revocation: both CRL forms of IEEE 1609.2 clause 7.
///
/// Hash entries revoke one certificate each (`HashedId10` + `Time32`, 14 bytes); linkage
/// entries revoke every certificate a device will ever hold, from one i-period forward,
/// for two 16-byte seeds. Both exist because they trade differently: a hash CRL is O(1) to
/// check and O(certificates) to publish; a linkage CRL is O(1) to publish per device and
/// O(entries) to check (04-models.md §9.6).
#[derive(Debug, Clone, Default)]
pub struct CrlStore {
    hashes: BTreeSet<[u8; 10]>,
    linkage: Vec<CrlLinkageEntry>,
}

impl CrlStore {
    /// An empty store.
    pub fn new() -> CrlStore {
        CrlStore::default()
    }

    /// Revokes one certificate by its `HashedId10`.
    pub fn revoke_hash(&mut self, id: HashedId10) {
        let mut b = [0u8; 10];
        b.copy_from_slice(&id.0[..]);
        self.hashes.insert(b);
    }

    /// Adds a linkage entry, revoking a device from its period forward.
    pub fn add_linkage_entry(&mut self, entry: CrlLinkageEntry) {
        self.linkage.push(entry);
    }

    /// True when a certificate is revoked by a hash entry.
    pub fn revokes_certificate(&self, cert: &Certificate) -> Result<bool> {
        let id10 = certificate_hashed_id10(cert)?;
        let mut b = [0u8; 10];
        b.copy_from_slice(&id10.0[..]);
        Ok(self.hashes.contains(&b))
    }

    /// True when a linkage entry revokes the certificate with this period *and* index.
    pub fn revokes_linkage(&self, i: u32, j: u32, lv: LinkageValue) -> bool {
        crate::linkage::crl_contains(&self.linkage, i, j, lv)
    }

    /// True when any linkage entry revokes the certificate carrying `lv` in period `i`,
    /// searching each entry's own declared index range.
    ///
    /// The form a verifier actually needs, and the reason it lives on the store: a
    /// certificate's `linkageData` carries `iCert` and the value but not `j`, so the index
    /// has to be searched, and only the store knows how wide each entry's range is.
    /// Searching a fixed range here instead would silently miss a device whose CRL entry
    /// covers more than the default 20 certificates per period.
    ///
    /// The cost — one hash-chain walk and two AES blocks per candidate index per entry —
    /// is a real property of linkage-based revocation and one the simulator exists to
    /// measure, so it is not cached away.
    ///
    /// What it is *not* is one chain walk per index. The walk depends on the period alone,
    /// so [`CrlLinkageEntry::matches_any_index`] does it once per entry and then costs two
    /// AES blocks per index — which is also what a real verifier does. Calling `matches`
    /// in a loop over `j`, as this used to, repeated the walk `jmax` times for nothing,
    /// and `jmax × delta` is the product an attacker who controls `iCert` was multiplying.
    pub fn revokes_linkage_at_period(&self, i: u32, lv: LinkageValue) -> bool {
        self.linkage.iter().any(|e| e.matches_any_index(i, lv))
    }

    /// How many entries of both kinds.
    pub fn len(&self) -> usize {
        self.hashes.len() + self.linkage.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// --------------------------------------------------------------------------------------
// The trait and the implementation
// --------------------------------------------------------------------------------------

/// The receive half of the security envelope: decoding an SPDU and planning its
/// verification.
///
/// Split from [`SecurityEnvelope`] for the same reason [`crate::crypto::CryptoBackendInfo`]
/// is split from [`CryptoBackend`], and along a line that turns out to mean something:
/// **signing needs a clock and a key; parsing and planning need neither.** A receiver
/// decodes bytes, looks things up in three stores and produces a list of work — all of it
/// a pure function of its inputs. Only the signer reaches for `ctx.now()`.
///
/// The mechanical consequence is that these three methods do not mention the context type,
/// so putting them on the generic trait would leave it unconstrained at every call site
/// and `envelope.parse(bytes)` would not compile without a turbofish naming a context the
/// caller never touches.
pub trait SecurityEnvelopeInfo: Model {
    /// Which profile this envelope implements.
    fn profile(&self) -> EnvelopeProfile;

    /// Decodes an SPDU and extracts everything a verifier needs.
    fn parse(&self, bytes: &[u8]) -> Result<ParsedSecured>;

    /// The primitive operations a receiver must run, with what they cost and which stores
    /// they hit. A missing certificate yields a P2PCD request plan instead.
    fn verify_plan(
        &self,
        p: &ParsedSecured,
        cache: &PeerCertCache,
        anchors: &TrustStore,
        crl: &CrlStore,
    ) -> VerifyPlan;
}

/// The send half of the security envelope (03-interfaces.md §6).
pub trait SecurityEnvelope<C: Ctx + ?Sized>: SecurityEnvelopeInfo {
    /// Signs `payload` and returns the encoded SPDU with its exact size.
    fn sign(
        &self,
        ctx: &mut C,
        crypto: &mut dyn CryptoBackend<C>,
        signer: &SignerHandle,
        payload: &[u8],
        hdr: &HeaderInfoSpec,
        sid: SignerIdChoice,
    ) -> Result<SecuredPdu>;
}

/// The IEEE 1609.2 / TS 103 097 envelope.
///
/// Stateless: the only fields are the profile, the scenario wall clock and the card. That
/// is what lets one instance be shared across the phase-parallel receive map, and what
/// lets the I-S1 test drive the same envelope with both backends.
#[derive(Debug, Clone)]
pub struct Envelope {
    profile: EnvelopeProfile,
    wall: WallClock,
    card: ModelCard,
}

impl Envelope {
    /// The unconstrained IEEE 1609.2 envelope.
    pub fn ieee1609(wall: WallClock) -> Envelope {
        Envelope {
            profile: EnvelopeProfile::Ieee1609Dot2,
            wall,
            card: envelope_card(EnvelopeProfile::Ieee1609Dot2),
        }
    }

    /// The ETSI TS 103 097 profile.
    pub fn etsi(wall: WallClock) -> Envelope {
        Envelope {
            profile: EnvelopeProfile::EtsiTs103097,
            wall,
            card: envelope_card(EnvelopeProfile::EtsiTs103097),
        }
    }

    /// The scenario wall clock this envelope stamps `generationTime` from.
    pub fn wall_clock(&self) -> WallClock {
        self.wall
    }

    /// The signer identifier this policy selects, with the profile's own rules applied
    /// first.
    ///
    /// TS 103 097 §7.1.2 makes a DENM's signer always a certificate, whatever the
    /// scenario's cadence says. Resolving that here rather than in the caller means no
    /// caller can forget it.
    pub fn choose_signer_id(
        &self,
        policy: SignerIdPolicy,
        now: SimTime,
        last_attached: Option<SimTime>,
        msg_type: Option<MsgType>,
    ) -> SignerIdChoice {
        if self.profile == EnvelopeProfile::EtsiTs103097 && msg_type == Some(MsgType::Denm) {
            return SignerIdChoice::Certificate;
        }
        policy.choose(now, last_attached)
    }

    /// Builds the `HeaderInfo`, applying the profile's constraints.
    fn header_info(&self, spec: &HeaderInfoSpec, now: SimTime) -> Result<HeaderInfo> {
        let generation_time = self.wall.time64(now)?;
        if self.profile == EnvelopeProfile::EtsiTs103097 {
            // TS 103 097 §5.2: the profile forbids these two in a signed SPDU.
            if spec.p2pcd_learning_request.is_some() {
                return Err(SecError::MissingField {
                    profile: ETSI_TS_103_097_ID,
                    field: "headerInfo.p2pcdLearningRequest must be absent",
                });
            }
            // §7.1.2: a DENM carries the generation location.
            if spec.msg_type == Some(MsgType::Denm) && spec.generation_location.is_none() {
                return Err(SecError::MissingField {
                    profile: ETSI_TS_103_097_ID,
                    field: "headerInfo.generationLocation (required for DENM)",
                });
            }
        }
        // `HeaderInfo` is `#[non_exhaustive]` (an extensible ASN.1 SEQUENCE), so it is
        // built through the generated constructor. The argument order is the ASN.1 field
        // order.
        Ok(HeaderInfo::new(
            Psid(rasn::types::Integer::from(spec.psid)),
            Some(Time64(Uint64(generation_time))),
            spec.expiry
                .map(|d| Time64(Uint64(generation_time.saturating_add(d.as_nanos() / 1_000)))),
            spec.generation_location.map(|l| l.to_asn1()),
            spec.p2pcd_learning_request.clone(),
            None, // missingCrlIdentifier: out-of-band CRL requests are not modelled
            None, // encryptionKey
            (!spec.inline_p2pcd_request.is_empty())
                .then(|| SequenceOfHashedId3(spec.inline_p2pcd_request.clone())),
            None, // requestedCertificate: filled by the P2PCD responder, not the signer
            None, // pduFunctionalType
            None, // contributedExtensions
        ))
    }
}

/// `H( H(tbsData) ‖ H(signer identifier input) )` — the digest IEEE 1609.2 §5.3.1 signs.
///
/// A free function, and public, because three separate parties compute it and must agree
/// to the bit: the signer ([`Envelope::sign`]), the receiver
/// ([`ParsedSecured::signing_digest`]), and anything that wants to demonstrate *what* the
/// signature covers — which is the only way to show that a `digest` signer identifier is
/// interoperable, since the certificate is hashed in either case.
pub fn signing_digest(tbs_coer: &[u8], signer_identifier_input: &[u8]) -> [u8; 32] {
    let inner = [sha256(tbs_coer), sha256(signer_identifier_input)].concat();
    sha256(&inner)
}

impl Model for Envelope {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl SecurityEnvelopeInfo for Envelope {
    fn profile(&self) -> EnvelopeProfile {
        self.profile
    }

    fn parse(&self, bytes: &[u8]) -> Result<ParsedSecured> {
        let data: Ieee1609Dot2Data = coer::decode(MsgType::Crl, bytes)?;
        let signed = match &data.content {
            Ieee1609Dot2Content::signedData(s) => s,
            Ieee1609Dot2Content::unsecuredData(_) => {
                return Err(SecError::NotSignedData {
                    found: "unsecuredData",
                });
            }
            Ieee1609Dot2Content::encryptedData(_) => {
                return Err(SecError::NotSignedData {
                    found: "encryptedData",
                });
            }
            _ => {
                return Err(SecError::NotSignedData {
                    found: "an unrecognised content alternative",
                });
            }
        };

        let payload = match &signed.tbs_data.payload.data {
            Some(inner) => match &inner.content {
                Ieee1609Dot2Content::unsecuredData(o) => o.0.to_vec(),
                _ => {
                    return Err(SecError::MissingField {
                        profile: self.profile.model_id(),
                        field: "tbsData.payload.data.content.unsecuredData",
                    });
                }
            },
            None => {
                return Err(SecError::MissingField {
                    profile: self.profile.model_id(),
                    field: "tbsData.payload.data",
                });
            }
        };

        let signer = match &signed.signer {
            SignerIdentifier::digest(d) => ParsedSigner::Digest(d.clone()),
            SignerIdentifier::certificate(seq) => match seq.0.first() {
                Some(cert) => ParsedSigner::Certificate {
                    digest: certificate_digest(cert)?,
                    certificate: Box::new(cert.clone()),
                },
                None => {
                    return Err(SecError::MissingField {
                        profile: self.profile.model_id(),
                        field: "signer.certificate[0]",
                    });
                }
            },
            SignerIdentifier::R_self(()) => ParsedSigner::SelfSigned,
            _ => {
                return Err(SecError::MissingField {
                    profile: self.profile.model_id(),
                    field: "signer (unrecognised alternative)",
                });
            }
        };

        let signature = match &signed.signature {
            Signature::ecdsaNistP256Signature(s) | Signature::ecdsaBrainpoolP256r1Signature(s) => {
                let r: &[u8] = match &s.r_sig {
                    EccP256CurvePoint::x_only(x)
                    | EccP256CurvePoint::compressed_y_0(x)
                    | EccP256CurvePoint::compressed_y_1(x) => x,
                    EccP256CurvePoint::uncompressedP256(p) => &p.x,
                    EccP256CurvePoint::fill(()) => {
                        return Err(SecError::MissingField {
                            profile: self.profile.model_id(),
                            field: "signature.rSig (the `fill` alternative carries no value)",
                        });
                    }
                };
                [r, &s.s_sig].concat()
            }
            _ => {
                return Err(SecError::UnsupportedPrimitive {
                    backend: "envelope",
                    primitive: PrimitiveId::ECDSA_P384,
                });
            }
        };

        // Re-encode `tbsData` rather than slicing the input: the signature is over the
        // *canonical* encoding, and a receiver that hashed the bytes it happened to
        // receive would accept a non-canonical re-encoding of a valid SPDU and, worse,
        // reject a canonical one that had travelled through a re-encoding relay.
        let tbs_coer = coer::encode(MsgType::Crl, &signed.tbs_data)?;

        Ok(ParsedSecured {
            profile: self.profile,
            signer,
            psid: psid_to_u64(&signed.tbs_data.header_info.psid),
            generation_time: signed
                .tbs_data
                .header_info
                .generation_time
                .as_ref()
                .map(|t| t.0.0),
            generation_location: signed
                .tbs_data
                .header_info
                .generation_location
                .as_ref()
                .map(GenerationLocation::from_asn1),
            inline_p2pcd_request: signed
                .tbs_data
                .header_info
                .inline_p2pcd_request
                .as_ref()
                .map(|s| s.0.clone())
                .unwrap_or_default(),
            payload,
            tbs_coer,
            signature,
            hash_id: signed.hash_id,
            size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        })
    }

    fn verify_plan(
        &self,
        p: &ParsedSecured,
        cache: &PeerCertCache,
        anchors: &TrustStore,
        crl: &CrlStore,
    ) -> VerifyPlan {
        let mut ops: Vec<PrimitiveOp> = Vec::new();

        if p.hash_id != HashAlgorithm::sha256 {
            return VerifyPlan {
                ops,
                outcome: PlanOutcome::Reject(RejectReason::UnsupportedHashAlgorithm),
            };
        }

        // Which certificate signed this, and does the receiver have it? The signer's
        // digest comes out of this match rather than from `p.signer_digest()` later: the
        // `SelfSigned` arm has already returned by then, so a second lookup would need an
        // unreachable fallback, and an unreachable fallback is a branch no test can cover.
        let (certificate, already_verified, cert_bytes, signer_digest) = match &p.signer {
            ParsedSigner::SelfSigned => {
                return VerifyPlan {
                    ops,
                    outcome: PlanOutcome::Reject(RejectReason::SelfSigned),
                };
            }
            ParsedSigner::Digest(d) => {
                ops.push(PrimitiveOp::StoreLookup {
                    store: StoreKind::PeerCertCache,
                    key: d.clone(),
                });
                match cache.get(d) {
                    Some(c) => {
                        let bytes = coer::encode(MsgType::Crl, c.certificate.as_ref())
                            .map(|b| b.len() as u32)
                            .unwrap_or(0);
                        (c.certificate.clone(), c.verified, bytes, d.clone())
                    }
                    None => {
                        return VerifyPlan {
                            ops,
                            outcome: PlanOutcome::NeedCertificate(P2pcdRequest {
                                id3: hashed_id3_of_id8(d),
                                unknown: d.clone(),
                                kind: P2pcdKind::EndEntity,
                            }),
                        };
                    }
                }
            }
            ParsedSigner::Certificate {
                certificate,
                digest,
            } => {
                // The receiver must hash the attached certificate to learn its id, before
                // it can tell whether it already knows it.
                let bytes = coer::encode(MsgType::Crl, certificate.as_ref())
                    .map(|b| b.len() as u32)
                    .unwrap_or(0);
                ops.push(PrimitiveOp::Hash {
                    primitive: PrimitiveId::SHA_256,
                    bytes,
                    what: HashSubject::Certificate,
                });
                ops.push(PrimitiveOp::StoreLookup {
                    store: StoreKind::PeerCertCache,
                    key: digest.clone(),
                });
                let known = cache.get(digest).map(|c| c.verified).unwrap_or(false);
                (
                    Arc::new(certificate.as_ref().clone()),
                    known,
                    bytes,
                    digest.clone(),
                )
            }
        };

        // Hash revocation, before any signature work: a revoked certificate is rejected
        // whatever its signature says, and the check is one hash and a set lookup.
        ops.push(PrimitiveOp::StoreLookup {
            store: StoreKind::CrlStore,
            // The CRL is keyed by HashedId10, but the plan records the lookup against the
            // identity the rest of the plan uses; the extra two bytes change no cost.
            key: signer_digest.clone(),
        });
        if crl
            .revokes_certificate(certificate.as_ref())
            .unwrap_or(false)
        {
            return VerifyPlan {
                ops,
                outcome: PlanOutcome::Reject(RejectReason::Revoked),
            };
        }

        // Validity: `generationTime` must fall inside the signer certificate's period.
        if let Some(t64) = p.generation_time
            && !validity_covers(&certificate.0.to_be_signed.validity_period, t64 / 1_000_000)
        {
            return VerifyPlan {
                ops,
                outcome: PlanOutcome::Reject(RejectReason::OutsideValidityPeriod),
            };
        }

        // Linkage revocation *after* the validity check, and deliberately so. Unlike the
        // hash CRL this one is not cheap — it is a hash-chain walk per entry, driven by
        // the certificate's own `iCert` — so it goes behind the cheapest check that can
        // already refuse the message. A certificate that is both expired and revoked is
        // now reported as `OutsideValidityPeriod`; it is rejected either way, and the
        // ordering costs an attacker a valid period on top of a valid linkage claim.
        // The walk is bounded regardless: see `linkage::DEFAULT_MAX_FORWARD_PERIODS`.
        if let Some(reason) = linkage_revocation(certificate.as_ref(), crl) {
            return VerifyPlan {
                ops,
                outcome: PlanOutcome::Reject(reason),
            };
        }

        // Validate the certificate itself, unless the cache says it is already done.
        if !already_verified {
            match &certificate.0.issuer {
                IssuerIdentifier::sha256AndDigest(issuer)
                | IssuerIdentifier::sha384AndDigest(issuer)
                | IssuerIdentifier::sm3AndDigest(issuer) => {
                    ops.push(PrimitiveOp::StoreLookup {
                        store: StoreKind::TrustStore,
                        key: issuer.clone(),
                    });
                    if !anchors.contains(issuer) && cache.get(issuer).is_none() {
                        return VerifyPlan {
                            ops,
                            outcome: PlanOutcome::NeedCertificate(P2pcdRequest {
                                id3: hashed_id3_of_id8(issuer),
                                unknown: issuer.clone(),
                                kind: P2pcdKind::CertificateAuthority,
                            }),
                        };
                    }
                }
                IssuerIdentifier::R_self(_) => {
                    // A self-signed end-entity certificate is only usable if it is itself
                    // a trust anchor; otherwise there is nothing to chain to.
                    ops.push(PrimitiveOp::StoreLookup {
                        store: StoreKind::TrustStore,
                        key: signer_digest.clone(),
                    });
                    if !anchors.contains(&signer_digest) {
                        return VerifyPlan {
                            ops,
                            outcome: PlanOutcome::Reject(RejectReason::UntrustedSelfSignedIssuer),
                        };
                    }
                }
                _ => {}
            }
            match certificate.0.r_type {
                CertificateType::explicit => ops.push(PrimitiveOp::VerifySignature {
                    primitive: PrimitiveId::ECDSA_P256_SHA256,
                    over: SigSubject::Certificate,
                }),
                CertificateType::implicit => ops.push(PrimitiveOp::ReconstructImplicit {
                    primitive: PrimitiveId::ECQV_P256,
                }),
                _ => {}
            }
        }

        // Finally the SPDU's own signature: hash the tbsData, hash the signer identifier
        // input, verify.
        ops.push(PrimitiveOp::Hash {
            primitive: PrimitiveId::SHA_256,
            bytes: u32::try_from(p.tbs_coer.len()).unwrap_or(u32::MAX),
            what: HashSubject::ToBeSignedData,
        });
        ops.push(PrimitiveOp::Hash {
            primitive: PrimitiveId::SHA_256,
            bytes: cert_bytes,
            what: HashSubject::SignerIdentifier,
        });
        ops.push(PrimitiveOp::VerifySignature {
            primitive: PrimitiveId::ECDSA_P256_SHA256,
            over: SigSubject::Spdu,
        });

        VerifyPlan {
            ops,
            outcome: PlanOutcome::Verifiable,
        }
    }
}

impl<C: Ctx + ?Sized> SecurityEnvelope<C> for Envelope {
    fn sign(
        &self,
        ctx: &mut C,
        crypto: &mut dyn CryptoBackend<C>,
        signer: &SignerHandle,
        payload: &[u8],
        hdr: &HeaderInfoSpec,
        sid: SignerIdChoice,
    ) -> Result<SecuredPdu> {
        let now = ctx.now();
        let header_info = self.header_info(hdr, now)?;

        // The protected payload travels as an inner `Ieee1609Dot2Data` carrying
        // `unsecuredData`, which is what both profiles require of a data payload.
        let inner = Ieee1609Dot2Data::new(
            Uint8(PROTOCOL_VERSION),
            Ieee1609Dot2Content::unsecuredData(Opaque(rasn::types::OctetString::from_slice(
                payload,
            ))),
        );
        let tbs = ToBeSignedData::new(
            Box::new(SignedDataPayload::new(Some(inner), None, None)),
            header_info,
        );

        let tbs_coer = coer::encode(hdr.msg_type.unwrap_or(MsgType::Crl), &tbs)?;
        let digest = signing_digest(&tbs_coer, signer.cert_coer());
        let token = crypto.sign_prehashed(ctx, &signer.key, &digest)?;

        let signer_identifier = match sid {
            SignerIdChoice::Digest => SignerIdentifier::digest(signer.digest().clone()),
            SignerIdChoice::Certificate => {
                SignerIdentifier::certificate(SequenceOfCertificate(vec![
                    signer.certificate.as_ref().clone(),
                ]))
            }
        };

        let signed = SignedData::new(
            HashAlgorithm::sha256,
            tbs,
            signer_identifier,
            token.to_ieee1609_signature()?,
        );
        let data = Ieee1609Dot2Data::new(
            Uint8(PROTOCOL_VERSION),
            Ieee1609Dot2Content::signedData(signed),
        );
        let encoded = coer::encoded(hdr.msg_type.unwrap_or(MsgType::Crl), &data)?;
        let spdu_digest = crate::hashedid::hashed_id8(&encoded.bytes);
        Ok(SecuredPdu {
            payload_bytes: u32::try_from(payload.len()).unwrap_or(u32::MAX),
            encoded,
            signer_id: sid,
            digest: spdu_digest,
        })
    }
}

/// A `Psid` as a `u64`, saturating: a PSID is an unbounded INTEGER in the ASN.1 but every
/// assigned value is small.
fn psid_to_u64(p: &Psid) -> u64 {
    u64::try_from(&p.0).unwrap_or(u64::MAX)
}

/// The linkage-based revocation check, when the certificate carries `linkageData`.
fn linkage_revocation(cert: &Certificate, crl: &CrlStore) -> Option<RejectReason> {
    let CertificateId::linkageData(ld) = &cert.0.to_be_signed.id else {
        return None;
    };
    crl.revokes_linkage_at_period(
        u32::from(ld.i_cert.0.0),
        LinkageValue::from_asn1(&ld.linkage_value),
    )
    .then_some(RejectReason::Revoked)
}

/// True when `at_epoch_seconds` (seconds since the 1609.2 epoch) is inside the period.
fn validity_covers(vp: &ValidityPeriod, at_epoch_seconds: u64) -> bool {
    let start = u64::from(vp.start.0.0);
    let end = start.saturating_add(duration_seconds(&vp.duration));
    at_epoch_seconds >= start && at_epoch_seconds < end
}

/// An IEEE 1609.2 `Duration` in seconds.
///
/// The sub-second alternatives truncate, because a certificate validity period measured
/// in microseconds is not a thing any profile issues and rounding it up would extend a
/// certificate's life. `years` is 31,556,952 seconds — the Gregorian mean year 1609.2
/// defines, not 365 days.
fn duration_seconds(d: &Ieee1609Duration) -> u64 {
    match d {
        Ieee1609Duration::microseconds(v) => u64::from(v.0) / 1_000_000,
        Ieee1609Duration::milliseconds(v) => u64::from(v.0) / 1_000,
        Ieee1609Duration::seconds(v) => u64::from(v.0),
        Ieee1609Duration::minutes(v) => u64::from(v.0) * 60,
        Ieee1609Duration::hours(v) => u64::from(v.0) * 3_600,
        Ieee1609Duration::sixtyHours(v) => u64::from(v.0) * 216_000,
        Ieee1609Duration::years(v) => u64::from(v.0) * 31_556_952,
    }
}

/// The card for one profile.
fn envelope_card(profile: EnvelopeProfile) -> ModelCard {
    let (id, purpose) = match profile {
        EnvelopeProfile::Ieee1609Dot2 => (
            IEEE_1609_2_ID,
            "IEEE 1609.2 SignedData: real COER encoding through the generated ASN.1 \
             bindings, both signer identifiers, and a verification plan a node runtime can \
             cost and queue.",
        ),
        EnvelopeProfile::EtsiTs103097 => (
            ETSI_TS_103_097_ID,
            "The ETSI TS 103 097 profile of IEEE 1609.2 SignedData: the same structure \
             with the profile's constraints enforced at signing time — generationTime \
             mandatory, p2pcdLearningRequest forbidden, and a DENM carrying \
             generationLocation and signing with a certificate.",
        ),
    };
    let mut card = ModelCard::new(id, Family::Envelope, "1.0.0", purpose);
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        v2xw_core::card::Equation::new(
            "signed hash",
            "H( H(tbsData) ‖ H(signer identifier input) ), SHA-256 [IEEE 1609.2 §5.3.1]",
        ),
        v2xw_core::card::Equation::new(
            "HashedId8",
            "the low-order 8 bytes of SHA-256 over the canonical COER encoding \
             [IEEE 1609.2a-2017 §6.3.25]",
        ),
    ];
    card.assumptions = vec![
        "The signer identifier input is the signer's certificate even when the wire \
         carries only its digest, so attaching a digest changes the SPDU's size and \
         nothing else."
            .to_string(),
        "`rSig` uses the `x-only` alternative, as 1609.2 §6.3.29 specifies for a \
         generated ECDSA signature. All three 32-byte alternatives encode to 33 bytes, so \
         the choice affects conformance and not size."
            .to_string(),
        "`tbsData` is re-encoded before hashing rather than sliced out of the received \
         bytes, so a signature is always checked against the canonical encoding."
            .to_string(),
    ];
    card.limitations = vec![
        "Only ECDSA over P-256 can be carried: IEEE 1609.2's `Signature` CHOICE has no \
         post-quantum alternative and no extension defining one is standardised, so a \
         post-quantum primitive is refused rather than mis-encoded."
            .to_string(),
        "Encrypted SPDUs, countersignatures and certificate requests are out of scope; \
         only `signedData` is produced and parsed."
            .to_string(),
        "`verify_plan` checks revocation and the validity period, and plans the signature \
         work; it does not itself verify anything, which is the `NodeRuntime`'s job."
            .to_string(),
    ];
    card.ignores =
        vec!["Certificate chains deeper than one level above a trust anchor.".to_string()];
    card.sources = vec![
        Source::new(SourceKind::Standard, "IEEE 1609.2 §5.3.1, §6.3.4, §6.3.29"),
        Source::new(
            SourceKind::Standard,
            "IEEE 1609.2a-2017 §6.3.25-6.3.26, §8.1-8.2.4",
        ),
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 097 V2.1.1 §4.1, §5.1-5.2, §7.1",
        ),
        Source::new(
            SourceKind::Standard,
            "04-models.md §9.1 (envelope overhead)",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Standard,
            "04-models.md §9.1 envelope overhead derivation",
        )],
        tests: vec![
            "the_measured_overhead_matches_the_derivation".to_string(),
            "a_signed_pdu_round_trips_and_verifies".to_string(),
        ],
    };
    card
}
