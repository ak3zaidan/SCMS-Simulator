//! Wire sizes, and where every one of them comes from.
//!
//! Invariant **I-P8** (this crate's addition to the list in 05-protocols §7): no byte
//! count reaches a link, a queue or a metric without a [`SizeProvenance`] saying how it
//! was obtained. The legitimate answers are
//!
//! * [`SizeProvenance::RealEncoder`] — the COER encoder in `v2xw-sec` encoded the object
//!   and this is the length it produced. Every certificate on every flow is sized this
//!   way, so a change to the certificate profile moves every batch, every CRL and every
//!   provisioning request with it;
//! * [`SizeProvenance::Cited`] and [`SizeProvenance::Derived`] — a constant, or a sum of
//!   constants, each carrying the clause it comes from;
//! * [`SizeProvenance::Parameter`] — a number no published source gives, carried as a
//!   model-card parameter with a calibration plan (registry rule R1). `tests/wire_sizes.rs`
//!   checks that every name used here is on the card *and* has a non-empty plan, so this
//!   variant cannot smuggle an invented number past the rule.
//!
//! The envelope overheads are the measured ones: 04-models.md §9.1 records 93 B with a
//! digest signer and 87 B plus the certificate with a certificate signer, **measured**
//! against the real encoder by `crates/v2xw-sec/tests/overhead.rs`, as equalities rather
//! than tolerances.

use v2xw_sec::cert::{self, CertSpec, HolderId};
use v2xw_sec::hashedid::hashed_id8;
use v2xw_sec::linkage::{LS_BYTES, LinkageValue, PLV_BYTES};

use crate::error::{ProtoError, Result};

/// How a byte count was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SizeProvenance {
    /// The real COER encoder produced this length.
    RealEncoder {
        /// What was encoded, e.g. `"pseudonym certificate (implicit, ECQV)"`.
        what: &'static str,
    },
    /// A constant from a standard, a paper or a measurement recorded in the design set.
    Cited {
        /// The clause, table or measurement, e.g. `"IEEE 1609.2 §6.3.26"`.
        citation: &'static str,
    },
    /// A sum or product of cited components.
    Derived {
        /// The arithmetic, in bytes.
        expression: &'static str,
        /// Where the components come from.
        citation: &'static str,
    },
    /// A number no source gives, carried as a model-card parameter with a plan.
    Parameter {
        /// The parameter's name, exactly as the card spells it.
        param: &'static str,
    },
}

impl SizeProvenance {
    /// The card parameter this size rests on, if it rests on one.
    pub const fn parameter(self) -> Option<&'static str> {
        match self {
            SizeProvenance::Parameter { param } => Some(param),
            _ => None,
        }
    }

    /// The citation text, for a provenance that carries one.
    pub const fn citation(self) -> Option<&'static str> {
        match self {
            SizeProvenance::Cited { citation } | SizeProvenance::Derived { citation, .. } => {
                Some(citation)
            }
            _ => None,
        }
    }
}

/// A number of bytes on the wire, with its provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct WireSize {
    bytes: u32,
    provenance: SizeProvenance,
}

impl WireSize {
    /// A size the real encoder produced.
    pub const fn real(bytes: u32, what: &'static str) -> WireSize {
        WireSize {
            bytes,
            provenance: SizeProvenance::RealEncoder { what },
        }
    }

    /// A cited constant.
    pub const fn cited(bytes: u32, citation: &'static str) -> WireSize {
        WireSize {
            bytes,
            provenance: SizeProvenance::Cited { citation },
        }
    }

    /// A sum of cited components.
    pub const fn derived(bytes: u32, expression: &'static str, citation: &'static str) -> WireSize {
        WireSize {
            bytes,
            provenance: SizeProvenance::Derived {
                expression,
                citation,
            },
        }
    }

    /// A size that rests on an uncalibrated card parameter.
    pub const fn parameter(bytes: u32, param: &'static str) -> WireSize {
        WireSize {
            bytes,
            provenance: SizeProvenance::Parameter { param },
        }
    }

    /// The byte count.
    pub const fn bytes(self) -> u32 {
        self.bytes
    }

    /// Where the byte count came from.
    pub const fn provenance(self) -> SizeProvenance {
        self.provenance
    }
}

/// A SEC 1 compressed P-256 point: `04-models.md §9.1`, `SEC 1 v2.0 §2.3.3`.
pub const EC_POINT_COMPRESSED_BYTES: u32 = 33;
/// An AES-128 key: FIPS 197 §5.
pub const AES128_KEY_BYTES: u32 = 16;
/// A SHA-256 digest: FIPS 180-4 §1.
pub const SHA256_BYTES: u32 = 32;
/// `HashedId8`: IEEE 1609.2 §6.3.26.
pub const HASHED_ID8_BYTES: u32 = 8;
/// `HashedId10`: IEEE 1609.2a-2017 §6.3.25.
pub const HASHED_ID10_BYTES: u32 = 10;
/// `Time32 ::= Uint32`: `Ieee1609Dot2BaseTypes.asn`.
pub const TIME32_BYTES: u32 = 4;
/// An ECDSA P-256 signature inside a 1609.2 COER structure: 05-protocols.md §5.3.
pub const ECDSA_P256_SIG_COER_BYTES: u32 = 66;
/// `EciesP256EncryptedKey ::= SEQUENCE { v EccP256CurvePoint, c OCTET STRING (SIZE(16)),
/// t OCTET STRING (SIZE(16)) }` — 33 + 16 + 16: `Ieee1609Dot2BaseTypes.asn`.
pub const ECIES_P256_ENCRYPTED_KEY_BYTES: u32 = 65;
/// `SignedData` overhead with a **digest** signer, PSID below 256 and a payload below
/// 128 B — measured, not estimated: 04-models.md §9.1.
pub const ENVELOPE_OVERHEAD_DIGEST_BYTES: u32 = 93;
/// `SignedData` overhead with a **certificate** signer, *excluding* the certificate —
/// measured: 04-models.md §9.1.
pub const ENVELOPE_OVERHEAD_CERT_BYTES: u32 = 87;
/// A linkage value: 9 B, `Ieee1609Dot2BaseTypes.asn`; Brecht 2018 §V-B.
pub const LINKAGE_VALUE_BYTES: u32 = PLV_BYTES as u32;
/// A linkage seed: 16 B, `Ieee1609Dot2BaseTypes.asn`; Brecht 2018 §V-B.
pub const LINKAGE_SEED_BYTES: u32 = LS_BYTES as u32;
/// One linked-CRL entry: 32 B of seeds plus group overhead, ≈ 40 B — Brecht 2018 §VI-F,
/// restated in 04-models.md §9.6 (10,000 entries ≈ 400 kB).
pub const CRL_LINKAGE_ENTRY_BYTES: u32 = 40;
/// One hash-identified CRL entry: `HashedId10` + `Time32` — 04-models.md §9.6.
pub const CRL_HASH_ENTRY_BYTES: u32 = HASHED_ID10_BYTES + TIME32_BYTES;

/// The sizes that no standard publishes, declared once so the cards and the size model
/// cannot drift apart.
///
/// Every field here is a `todo-calibrate` parameter on the protocol's model card, and
/// every one is used through [`WireSize::parameter`], so `tests/wire_sizes.rs` can walk
/// from a byte count on the wire to the plan for pinning it down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeParams {
    /// Bytes of certificate-repository URL in the Registration Authority's acknowledgement.
    pub repo_url_bytes: u32,
    /// Framing added by the batch container (`X_Y.zip`) around one i-period of
    /// certificates.
    pub batch_container_bytes: u32,
    /// The TS 103 759 misbehaviour-report payload, before the envelope.
    pub report_payload_bytes: u32,
    /// A linkage-chain identifier as it crosses RA → LA and MA → LA.
    pub linkage_chain_identifier_bytes: u32,
    /// The ETSI `InnerEcRequest` subject-attribute block.
    pub etsi_subject_attributes_bytes: u32,
}

impl SizeParams {
    /// The names, in card order. Used by the conformance test, and by the cards
    /// themselves, so a new parameter cannot be added in one place only.
    pub const NAMES: [&'static str; 5] = [
        "repo_url_bytes",
        "batch_container_bytes",
        "report_payload_bytes",
        "linkage_chain_identifier_bytes",
        "etsi_subject_attributes_bytes",
    ];
}

impl Default for SizeParams {
    /// The shipped defaults. **None of these five numbers is cited**; each is a starting
    /// value with a plan on the card. They are grouped here so that is impossible to miss.
    fn default() -> SizeParams {
        SizeParams {
            repo_url_bytes: 64,
            batch_container_bytes: 76,
            report_payload_bytes: 1_200,
            linkage_chain_identifier_bytes: 16,
            etsi_subject_attributes_bytes: 64,
        }
    }
}

/// The certificate sizes this deployment's flows carry, straight from the real encoder.
///
/// Built once per deployment. A certificate is the largest single component of almost
/// every message in 05-protocols §3.2, so sizing it by encoding a real one — rather than
/// by carrying the design document's "≈ 80–120 B" into the code — is what makes the
/// provisioning and CRL byte counts move when the certificate profile moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificateSizes {
    /// An implicit (ECQV) pseudonym certificate with `linkageData`.
    pub pseudonym: WireSize,
    /// An explicit enrolment certificate.
    pub enrolment: WireSize,
    /// An explicit authority (CA) certificate.
    pub authority: WireSize,
}

impl CertificateSizes {
    /// Encodes one certificate of each shape and records the lengths.
    ///
    /// # Errors
    /// [`ProtoError::Size`] if the COER encoder refuses a certificate — which would be a
    /// defect in the certificate profile, not a runtime condition.
    pub fn measured() -> Result<CertificateSizes> {
        let issuer = hashed_id8(b"v2xw-proto issuer");
        let key = [0x02u8; cert::COMPRESSED_KEY_BYTES];

        let pseudonym_spec = CertSpec::pseudonym(
            issuer.clone(),
            HolderId::Linkage {
                i_cert: 0,
                linkage_value: LinkageValue::new([0u8; PLV_BYTES]),
            },
            0,
            0x20,
        );
        let pseudonym =
            cert::implicit(&pseudonym_spec, &key).map_err(|source| ProtoError::Size {
                what: "pseudonym certificate",
                source,
            })?;

        let enrolment_spec = CertSpec::pseudonym(issuer.clone(), HolderId::None, 0, 0x23);
        let enrolment =
            cert::explicit(&enrolment_spec, &key, cert::zero_signature()).map_err(|source| {
                ProtoError::Size {
                    what: "enrolment certificate",
                    source,
                }
            })?;

        let authority_spec = CertSpec::authority(0, 0x23);
        let authority =
            cert::explicit(&authority_spec, &key, cert::zero_signature()).map_err(|source| {
                ProtoError::Size {
                    what: "authority certificate",
                    source,
                }
            })?;

        Ok(CertificateSizes {
            pseudonym: WireSize::real(
                encoded(&pseudonym, "pseudonym certificate")?,
                "pseudonym certificate (implicit, ECQV, linkageData)",
            ),
            enrolment: WireSize::real(
                encoded(&enrolment, "enrolment certificate")?,
                "enrolment certificate (explicit, ECDSA P-256)",
            ),
            authority: WireSize::real(
                encoded(&authority, "authority certificate")?,
                "authority certificate (explicit, self-signed)",
            ),
        })
    }
}

fn encoded(c: &v2xw_msg::sec_types::ieee1609_dot2::Certificate, what: &'static str) -> Result<u32> {
    cert::encoded_size(c).map_err(|source| ProtoError::Size { what, source })
}
