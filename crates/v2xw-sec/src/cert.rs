//! Building the certificates the envelope carries.
//!
//! Two shapes, and the difference between them is the whole of 04-models.md §9.2:
//!
//! * an **implicit** (ECQV) pseudonym certificate carries a *reconstruction value* and no
//!   signature, so it is about 80 bytes;
//! * an **explicit** certificate carries a verification key and a signature, so it is
//!   about 147 bytes — the same fields plus 35 for the key and 66 for the signature,
//!   minus the 34 the reconstruction value took.
//!
//! Those are the document's *derived* figures. This module builds the certificates the
//! derivation describes, field for field, so `the_certificate_sizes_match_the_derivation`
//! can check the arithmetic against the encoder instead of against itself.
//!
//! # Why a key is 33 bytes here and not a [`Point`]
//!
//! Every function takes public key material as `&[u8]` in the SEC 1 compressed shape —
//! 33 bytes beginning `0x02` or `0x03` — rather than as an elliptic-curve point. That is
//! invariant I-S1 reaching into the type signatures: under
//! [`crate::crypto::Modeled`] a public key is 33 bytes of the right shape that are *not*
//! a point, and a certificate builder that insisted on a real point could not build a
//! certificate in modelled mode at all. Since the certificate carries 33 bytes either
//! way, and every encoded size is therefore identical, requiring a point would buy
//! nothing and cost the invariant. [`Point::compressed`] is how the real backend produces
//! those bytes.
//!
//! # What goes in a pseudonym certificate, and why each field is there
//!
//! | Field | Why | Bytes in §9.2's derivation |
//! |---|---|---|
//! | `id: linkageData` | the `i`-period and the 9-byte linkage value that a linked CRL revokes by (SCP2, [`crate::linkage`]) | 13 |
//! | `cracaId` | which Certificate Revocation Authorization CA may revoke it | 3 |
//! | `crlSeries` | which CRL to look in | 2 |
//! | `validityPeriod` | 1609.2 `Time32` start + a `Duration` choice | 7 |
//! | `appPermissions` | one `PsidSsp`: what the holder may say | 8 |
//! | `verifyKeyIndicator` | the reconstruction value, or the verification key | 34 / 35 |
//!
//! The optional fields a pseudonym certificate does *not* carry — `region`,
//! `assuranceLevel`, `certIssuePermissions`, `encryptionKey` — are left absent, which is
//! what makes the certificate small. Under COER their absence costs nothing beyond the
//! preamble bits already counted.

use std::sync::Arc;

use v2xw_msg::codec::MsgType;
use v2xw_msg::sec_types::ieee1609_dot2::VerificationKeyIndicator;
use v2xw_msg::sec_types::{
    Certificate, CertificateBase, CertificateId, CertificateType, HashAlgorithm, HashedId3,
    HashedId8, IssuerIdentifier, LinkageData, PsidGroupPermissions, Signature,
    ToBeSignedCertificate, ValidityPeriod, coer,
    ieee1609_dot2_base_types::{
        CrlSeries, Duration as Ieee1609Duration, EccP256CurvePoint, EcdsaP256Signature, IValue,
        Psid, PsidSsp, PublicVerificationKey, SequenceOfPsidSsp, Time32, Uint16, Uint32,
    },
};

use crate::ec::Point;
use crate::error::{Result, SecError};
use crate::linkage::LinkageValue;

/// The IEEE 1609.2 certificate version every certificate here carries.
pub const CERT_VERSION: u8 = 3;

/// Bytes of a SEC 1 compressed public key or ECQV reconstruction value.
pub const COMPRESSED_KEY_BYTES: usize = 33;

/// How a certificate identifies its holder.
#[derive(Debug, Clone)]
pub enum HolderId {
    /// A pseudonym certificate: the i-period and the linkage value a linked CRL revokes.
    Linkage {
        /// The `i`-period this certificate belongs to.
        i_cert: u16,
        /// The 72-bit linkage value (`plv_1 ⊕ plv_2`, [`crate::linkage`]).
        linkage_value: LinkageValue,
    },
    /// No identifier at all — the ETSI authorization-ticket shape [TS 103 097 §7.2.1].
    None,
}

/// The fields a pseudonym or authorization certificate needs.
#[derive(Debug, Clone)]
pub struct CertSpec {
    /// Who issued it: the issuer's whole-certificate hash, or `None` for `self`.
    pub issuer: Option<HashedId8>,
    /// How the holder is identified.
    pub holder: HolderId,
    /// The Certificate Revocation Authorization CA.
    pub craca_id: HashedId3,
    /// Which CRL series to look in.
    pub crl_series: u16,
    /// Validity start, in seconds since the 1609.2 epoch.
    pub valid_from: u32,
    /// How long it is valid for.
    pub valid_for: Ieee1609Duration,
    /// The PSIDs the holder may sign under.
    pub app_permissions: Vec<u64>,
}

impl CertSpec {
    /// A CAM/BSM pseudonym certificate under one PSID, valid for the CAMP end-entity
    /// certificate lifetime.
    ///
    /// 10,140 minutes: the i-period is 10,080 minutes (one week) and the certificate
    /// overlaps the next period by one hour, so a device crossing a period boundary is
    /// never without a usable certificate [CAMP EE Requirements 2016 §2.1.5.3.2,
    /// 04-models.md §9.6].
    pub fn pseudonym(issuer: HashedId8, holder: HolderId, valid_from: u32, psid: u64) -> CertSpec {
        CertSpec {
            issuer: Some(issuer),
            holder,
            craca_id: HashedId3(rasn::types::FixedOctetString::from([0u8; 3])),
            crl_series: 1,
            valid_from,
            valid_for: Ieee1609Duration::minutes(Uint16(10_140)),
            app_permissions: vec![psid],
        }
    }

    /// A root certificate authority's own certificate: `self`-issued, no holder
    /// identifier, valid for three years.
    pub fn authority(valid_from: u32, psid: u64) -> CertSpec {
        CertSpec {
            issuer: None,
            holder: HolderId::None,
            craca_id: HashedId3(rasn::types::FixedOctetString::from([0u8; 3])),
            crl_series: 1,
            valid_from,
            valid_for: Ieee1609Duration::years(Uint16(3)),
            app_permissions: vec![psid],
        }
    }
}

/// The `EccP256CurvePoint` for 33 bytes of SEC 1 compressed key material.
///
/// Refuses anything that is not the compressed shape. A 65-byte uncompressed point would
/// encode as `uncompressedP256` and change every certificate's size, and the leading byte
/// is what selects `compressed-y-0` from `compressed-y-1`, so neither check is a
/// formality: 1609.2 §6.1.2 canonicalises a certificate to the compressed form before
/// hashing it, and a non-canonical key would give a `HashedId8` no peer agrees with.
pub fn ecc_curve_point(compressed: &[u8]) -> Result<EccP256CurvePoint> {
    if compressed.len() != COMPRESSED_KEY_BYTES {
        return Err(SecError::BadLength {
            what: "SEC 1 compressed public key",
            expected: COMPRESSED_KEY_BYTES,
            got: compressed.len(),
        });
    }
    let x = rasn::types::OctetString::from_slice(&compressed[1..]);
    match compressed[0] {
        0x02 => Ok(EccP256CurvePoint::compressed_y_0(x)),
        0x03 => Ok(EccP256CurvePoint::compressed_y_1(x)),
        other => Err(SecError::Crypto {
            op: "certificate key encode",
            primitive: crate::primitive::PrimitiveId::ECDSA_P256_SHA256,
            detail: format!(
                "a compressed point begins 0x02 or 0x03, not {other:#04x}; an uncompressed \
                 point would change the certificate's size and its canonical hash"
            ),
        }),
    }
}

/// The `toBeSigned` half of a certificate — what an issuer signs, and what an implicit
/// certificate's hash is taken over.
///
/// Public because issuance is a two-step dance: the signature covers `toBeSigned`, so the
/// issuer must be able to build it, hash it and sign it before the certificate exists.
pub fn to_be_signed(
    spec: &CertSpec,
    verify_key_indicator: VerificationKeyIndicator,
) -> Result<ToBeSignedCertificate> {
    let id = match &spec.holder {
        HolderId::Linkage {
            i_cert,
            linkage_value,
        } => CertificateId::linkageData(LinkageData::new(
            IValue(Uint16(*i_cert)),
            linkage_value.to_asn1(),
            None,
        )),
        HolderId::None => CertificateId::none(()),
    };
    if spec.app_permissions.is_empty() {
        return Err(SecError::MissingField {
            profile: "certificate",
            field: "toBeSigned.appPermissions (a certificate with no permissions can \
                    sign nothing)",
        });
    }
    let app_permissions = SequenceOfPsidSsp(
        spec.app_permissions
            .iter()
            .map(|p| PsidSsp::new(Psid(rasn::types::Integer::from(*p)), None))
            .collect(),
    );
    // `ToBeSignedCertificate` is `#[non_exhaustive]` (the ASN.1 SEQUENCE is extensible),
    // so it is built through the generated constructor rather than a struct literal. The
    // argument order is the ASN.1 field order; every `None` is a field a pseudonym
    // certificate deliberately omits, which is what keeps it at 80 bytes.
    Ok(ToBeSignedCertificate::new(
        id,
        spec.craca_id.clone(),
        CrlSeries(Uint16(spec.crl_series)),
        ValidityPeriod::new(Time32(Uint32(spec.valid_from)), spec.valid_for.clone()),
        None, // region
        None, // assuranceLevel
        Some(app_permissions),
        None, // certIssuePermissions: a pseudonym certificate issues nothing
        None, // certRequestPermissions
        None, // canRequestRollover
        None, // encryptionKey
        verify_key_indicator,
        None, // flags
        None, // appExtensions
        None, // certIssueExtensions
        None, // certRequestExtension
    ))
}

/// The `verifyKeyIndicator` of an explicit certificate.
pub fn verification_key_indicator(compressed_key: &[u8]) -> Result<VerificationKeyIndicator> {
    Ok(VerificationKeyIndicator::verificationKey(
        PublicVerificationKey::ecdsaNistP256(ecc_curve_point(compressed_key)?),
    ))
}

/// The `verifyKeyIndicator` of an implicit certificate.
pub fn reconstruction_key_indicator(
    compressed_reconstruction: &[u8],
) -> Result<VerificationKeyIndicator> {
    Ok(VerificationKeyIndicator::reconstructionValue(
        ecc_curve_point(compressed_reconstruction)?,
    ))
}

/// Assembles an **explicit** certificate from its parts.
///
/// `signature` is the issuer's signature over [`explicit_signature_input`]. This function
/// does not compute it — the issuer's key lives in a [`crate::crypto::CryptoBackend`],
/// and a certificate builder that reached into a backend would put the whole PKI into
/// this module. [`issue_explicit`] is the convenience that does both steps with a signing
/// closure the caller supplies.
pub fn explicit(
    spec: &CertSpec,
    compressed_key: &[u8],
    signature: Signature,
) -> Result<Certificate> {
    Ok(Certificate(CertificateBase::new(
        v2xw_msg::sec_types::Uint8(CERT_VERSION),
        CertificateType::explicit,
        issuer_identifier(spec),
        to_be_signed(spec, verification_key_indicator(compressed_key)?)?,
        Some(signature),
    )))
}

/// Builds an **implicit** (ECQV) certificate: a reconstruction value and no signature.
///
/// There is nothing to sign. The certificate is authenticated by the fact that only the
/// legitimate holder can derive a private key matching the public key a relying party
/// reconstructs from it ([`crate::crypto::ecqv`]), which is why it is 67 bytes smaller
/// than the explicit form.
pub fn implicit(spec: &CertSpec, compressed_reconstruction: &[u8]) -> Result<Certificate> {
    Ok(Certificate(CertificateBase::new(
        v2xw_msg::sec_types::Uint8(CERT_VERSION),
        CertificateType::implicit,
        issuer_identifier(spec),
        to_be_signed(
            spec,
            reconstruction_key_indicator(compressed_reconstruction)?,
        )?,
        None,
    )))
}

/// Issues an explicit certificate: build `toBeSigned`, hash it with the issuer's own
/// certificate, sign, assemble.
///
/// `sign` receives the 32-byte digest of IEEE 1609.2 §6.4.3 and returns the signature. A
/// closure rather than a backend handle, so this module stays free of the crypto layer
/// and the caller decides which key signs.
pub fn issue_explicit<F>(
    spec: &CertSpec,
    compressed_key: &[u8],
    issuer_cert_coer: &[u8],
    sign: F,
) -> Result<Certificate>
where
    F: FnOnce(&[u8; 32]) -> Result<Signature>,
{
    let tbs = to_be_signed(spec, verification_key_indicator(compressed_key)?)?;
    let digest = explicit_signature_input(&tbs, issuer_cert_coer)?;
    let signature = sign(&digest)?;
    Ok(Certificate(CertificateBase::new(
        v2xw_msg::sec_types::Uint8(CERT_VERSION),
        CertificateType::explicit,
        issuer_identifier(spec),
        tbs,
        Some(signature),
    )))
}

/// A self-signed trust anchor carrying `compressed_key`.
///
/// `issuer` is `self(sha256)` and there is no signature to check against anything else: a
/// trust anchor is trusted because it is in the trust store, not because of its
/// signature. Modelling it with a real self-signature would add a verification the
/// receiver never performs — [`crate::envelope::Envelope::verify_plan`] stops at the
/// trust store and does not plan one.
pub fn trust_anchor(spec: &CertSpec, compressed_key: &[u8]) -> Result<Certificate> {
    let mut spec = spec.clone();
    spec.issuer = None;
    Ok(Certificate(CertificateBase::new(
        v2xw_msg::sec_types::Uint8(CERT_VERSION),
        CertificateType::explicit,
        IssuerIdentifier::R_self(HashAlgorithm::sha256),
        to_be_signed(&spec, verification_key_indicator(compressed_key)?)?,
        Some(zero_signature()),
    )))
}

/// The bytes an issuer signs to produce an explicit certificate's signature.
///
/// IEEE 1609.2 §6.4.3: `H( H(toBeSigned) ‖ H(signer identifier input) )`, where the
/// signer identifier input is the issuer's own certificate encoding, or the empty string
/// when the issuer is `self`. The same two-stage construction as an SPDU signature, and
/// for the same reason: it binds the certificate to a particular issuer certificate
/// rather than to an issuer name.
pub fn explicit_signature_input(
    to_be_signed: &ToBeSignedCertificate,
    issuer_cert_coer: &[u8],
) -> Result<[u8; 32]> {
    let tbs = coer::encode(MsgType::Crl, to_be_signed)?;
    let inner = [
        v2xw_core::hash::sha256(&tbs),
        v2xw_core::hash::sha256(issuer_cert_coer),
    ]
    .concat();
    Ok(v2xw_core::hash::sha256(&inner))
}

/// A signature of the right shape and no cryptographic content.
///
/// For a trust anchor, whose signature a receiver never checks. Named `zero_signature`
/// rather than hidden inside [`trust_anchor`] so that nothing mistakes it for a real one.
pub fn zero_signature() -> Signature {
    Signature::ecdsaNistP256Signature(EcdsaP256Signature::new(
        EccP256CurvePoint::x_only(rasn::types::OctetString::from_slice(&[0u8; 32])),
        rasn::types::OctetString::from_slice(&[0u8; 32]),
    ))
}

/// A certificate's encoded size in bytes.
pub fn encoded_size(cert: &Certificate) -> Result<u32> {
    Ok(u32::try_from(coer::encode(MsgType::Crl, cert)?.len()).unwrap_or(u32::MAX))
}

/// A certificate's canonical COER encoding.
pub fn encode(cert: &Certificate) -> Result<Vec<u8>> {
    Ok(coer::encode(MsgType::Crl, cert)?)
}

/// A certificate wrapped for sharing.
pub fn shared(cert: Certificate) -> Arc<Certificate> {
    Arc::new(cert)
}

/// The compressed key material of a real curve point.
///
/// The bridge from [`crate::crypto::Real`]'s world of points to this module's world of
/// 33-byte strings.
pub fn compressed_key(point: &Point) -> Result<[u8; COMPRESSED_KEY_BYTES]> {
    point.compressed().ok_or(SecError::Crypto {
        op: "certificate key encode",
        primitive: crate::primitive::PrimitiveId::ECDSA_P256_SHA256,
        detail: "the point at infinity is not a public key".to_string(),
    })
}

/// The 33 bytes of key material a certificate carries, whichever form it carries them in.
///
/// For an explicit certificate this is the verification key; for an implicit one it is the
/// ECQV reconstruction value, which is *not* a public key — a relying party must still run
/// [`crate::crypto::ecqv::reconstruct_public_key`] over it. The caller knows which,
/// because [`Certificate`]'s `type` field says so, and pretending otherwise here would
/// hide the one step that makes an implicit certificate work.
pub fn public_key_material(cert: &Certificate) -> Result<Vec<u8>> {
    let point = match &cert.0.to_be_signed.verify_key_indicator {
        VerificationKeyIndicator::verificationKey(k) => match k {
            PublicVerificationKey::ecdsaNistP256(p)
            | PublicVerificationKey::ecdsaBrainpoolP256r1(p)
            | PublicVerificationKey::ecsigSm2(p) => p,
            _ => {
                return Err(SecError::UnsupportedPrimitive {
                    backend: "certificate",
                    primitive: crate::primitive::PrimitiveId::ECDSA_P384,
                });
            }
        },
        VerificationKeyIndicator::reconstructionValue(p) => p,
        _ => {
            return Err(SecError::MissingField {
                profile: "certificate",
                field: "toBeSigned.verifyKeyIndicator (unrecognised alternative)",
            });
        }
    };
    let (prefix, x) = match point {
        EccP256CurvePoint::compressed_y_0(x) => (0x02u8, x),
        EccP256CurvePoint::compressed_y_1(x) => (0x03, x),
        EccP256CurvePoint::x_only(_)
        | EccP256CurvePoint::fill(())
        | EccP256CurvePoint::uncompressedP256(_) => {
            return Err(SecError::MissingField {
                profile: "certificate",
                field: "toBeSigned.verifyKeyIndicator: the key is not in the canonical \
                        compressed form 1609.2 §6.1.2 requires",
            });
        }
    };
    let mut out = Vec::with_capacity(COMPRESSED_KEY_BYTES);
    out.push(prefix);
    out.extend_from_slice(x);
    Ok(out)
}

fn issuer_identifier(spec: &CertSpec) -> IssuerIdentifier {
    match &spec.issuer {
        Some(d) => IssuerIdentifier::sha256AndDigest(d.clone()),
        None => IssuerIdentifier::R_self(HashAlgorithm::sha256),
    }
}

/// The `PsidGroupPermissions` default this crate never builds, referenced so the patched
/// generated type stays in use.
///
/// `v2xw-msg`'s build applies a patch to `PsidGroupPermissions::eeType`'s `DEFAULT`
/// (build decision D5). Nothing in a pseudonym certificate carries group permissions — a
/// pseudonym certificate issues nothing — so without this function the patched type would
/// have no user in the security crate and a future breakage would surface only in
/// `v2xw-msg`'s own tests. Naming it here makes the dependency explicit.
pub fn group_permissions_type_is_reachable() -> bool {
    core::mem::size_of::<PsidGroupPermissions>() > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linkage::{DeviceLinkageContext, LaId, LinkageSeed};
    use p256::Scalar;

    fn issuer_digest() -> HashedId8 {
        crate::hashedid::hashed_id8(b"a pseudonym certificate authority")
    }

    fn spec() -> CertSpec {
        let dev = DeviceLinkageContext::new(
            LaId(1),
            LaId(2),
            LinkageSeed::new([0x01; 16]),
            LinkageSeed::new([0x02; 16]),
        );
        CertSpec::pseudonym(
            issuer_digest(),
            HolderId::Linkage {
                i_cert: 7,
                linkage_value: dev.linkage_value_for(7, 3),
            },
            // 2026-01-01 is 694,224,000 seconds after the 1609.2 epoch.
            694_224_000,
            0x20,
        )
    }

    fn key() -> [u8; COMPRESSED_KEY_BYTES] {
        compressed_key(&Point::mul_base(&Scalar::from(12_345u64))).expect("a real point")
    }

    /// 04-models.md §9.2 derives ≈80 bytes for an implicit pseudonym certificate and
    /// ≈147 for an explicit one. Those were arithmetic on an ASN.1 module; this measures
    /// them.
    #[test]
    fn the_certificate_sizes_match_the_derivation() {
        let k = key();
        let imp = implicit(&spec(), &k).expect("builds");
        let exp = explicit(&spec(), &k, zero_signature()).expect("builds");
        let (si, se) = (
            encoded_size(&imp).expect("encodes"),
            encoded_size(&exp).expect("encodes"),
        );
        println!("MEASURED implicit certificate = {si} B, explicit = {se} B");
        // The derivation's own accounting: the explicit form is the implicit form minus
        // the 34-byte reconstruction value plus a 35-byte verification key and a 66-byte
        // signature, i.e. +67.
        assert_eq!(se - si, 67, "implicit {si} B, explicit {se} B");
        assert!(
            (76..=84).contains(&si),
            "implicit certificate is {si} B, and §9.2 derives about 80"
        );
        assert!(
            (143..=151).contains(&se),
            "explicit certificate is {se} B, and §9.2 derives about 147"
        );
    }

    /// An implicit certificate carries no signature and an explicit one does — the
    /// structural difference, asserted, because it is what the size difference is made of.
    #[test]
    fn only_the_explicit_form_carries_a_signature() {
        let k = key();
        assert!(implicit(&spec(), &k).expect("builds").0.signature.is_none());
        assert!(
            explicit(&spec(), &k, zero_signature())
                .expect("builds")
                .0
                .signature
                .is_some()
        );
        assert_eq!(
            implicit(&spec(), &k).expect("builds").0.r_type,
            CertificateType::implicit
        );
    }

    /// A certificate round-trips through COER, and its digest is stable across the trip.
    #[test]
    fn a_certificate_round_trips_and_keeps_its_digest() {
        let cert = explicit(&spec(), &key(), zero_signature()).expect("builds");
        let bytes = encode(&cert).expect("encodes");
        let back: Certificate = coer::decode(MsgType::Crl, &bytes).expect("decodes");
        assert_eq!(cert, back);
        assert_eq!(
            crate::hashedid::certificate_digest(&cert).expect("digest"),
            crate::hashedid::certificate_digest(&back).expect("digest")
        );
    }

    /// The ETSI authorization-ticket shape has no `CertificateId`, and is smaller for it.
    #[test]
    fn an_authorization_ticket_has_no_holder_identifier() {
        let k = key();
        let mut s = spec();
        s.holder = HolderId::None;
        let at = explicit(&s, &k, zero_signature()).expect("builds");
        assert!(matches!(at.0.to_be_signed.id, CertificateId::none(())));
        let pseudonym = explicit(&spec(), &k, zero_signature()).expect("builds");
        assert!(
            encoded_size(&at).expect("encodes") < encoded_size(&pseudonym).expect("encodes"),
            "dropping the 13-byte linkageData must shrink the certificate"
        );
    }

    /// A certificate that can sign nothing is refused rather than built.
    #[test]
    fn a_certificate_needs_at_least_one_permission() {
        let mut s = spec();
        s.app_permissions.clear();
        assert!(implicit(&s, &key()).is_err());
    }

    /// Key material must be the compressed 33-byte shape, and both parities must map to
    /// the right alternative. An uncompressed point would change every size in §9.2.
    #[test]
    fn key_material_must_be_the_compressed_shape() {
        let mut even = key();
        even[0] = 0x02;
        let mut odd = key();
        odd[0] = 0x03;
        assert!(matches!(
            ecc_curve_point(&even).expect("even"),
            EccP256CurvePoint::compressed_y_0(_)
        ));
        assert!(matches!(
            ecc_curve_point(&odd).expect("odd"),
            EccP256CurvePoint::compressed_y_1(_)
        ));
        assert!(ecc_curve_point(&[0u8; 32]).is_err(), "wrong length");
        assert!(ecc_curve_point(&[0u8; 65]).is_err(), "uncompressed length");
        let mut bad = key();
        bad[0] = 0x04;
        assert!(ecc_curve_point(&bad).is_err(), "uncompressed prefix");
        // Both parities encode to the same 33 bytes, which is why the choice is free.
        assert_eq!(
            encoded_size(&explicit(&spec(), &even, zero_signature()).expect("builds"))
                .expect("encodes"),
            encoded_size(&explicit(&spec(), &odd, zero_signature()).expect("builds"))
                .expect("encodes")
        );
    }

    /// A self-issued authority certificate has a `self` issuer, and an end-entity one
    /// names its issuer's digest.
    #[test]
    fn the_issuer_identifier_distinguishes_an_anchor_from_an_end_entity() {
        let k = key();
        let anchor = trust_anchor(&CertSpec::authority(694_224_000, 0x20), &k).expect("builds");
        assert!(matches!(anchor.0.issuer, IssuerIdentifier::R_self(_)));
        let ee = explicit(&spec(), &k, zero_signature()).expect("builds");
        match &ee.0.issuer {
            IssuerIdentifier::sha256AndDigest(d) => assert_eq!(*d, issuer_digest()),
            other => panic!("unexpected issuer: {other:?}"),
        }
    }

    /// The key a certificate carries comes back out, byte for byte, in the shape a
    /// backend can import.
    #[test]
    fn the_key_a_certificate_carries_comes_back_out() {
        let k = key();
        let exp = explicit(&spec(), &k, zero_signature()).expect("builds");
        assert_eq!(public_key_material(&exp).expect("extracts"), k.to_vec());
        let imp = implicit(&spec(), &k).expect("builds");
        assert_eq!(
            public_key_material(&imp).expect("extracts"),
            k.to_vec(),
            "an implicit certificate's reconstruction value is read the same way, though \
             it is not itself a public key"
        );
        // Both parities survive the round trip.
        let mut odd = k;
        odd[0] = 0x03;
        let c = explicit(&spec(), &odd, zero_signature()).expect("builds");
        assert_eq!(public_key_material(&c).expect("extracts")[0], 0x03);
    }

    /// Where the measured certificate size differs from 04-models.md §9.2's derivation,
    /// field by field.
    ///
    /// §9.2 derives the implicit pseudonym certificate as 80 bytes:
    /// `preamble 1, version 1, type 1, issuer 9, toBeSigned {preamble 1, id linkageData 13,
    /// cracaId 3, crlSeries 2, validityPeriod 7, appPermissions 8, verifyKeyIndicator 34}`.
    /// This test encodes each of those components on its own and reports the encoder's
    /// number for it, so a disagreement with the total names the line rather than leaving
    /// a reader to guess.
    #[test]
    fn the_derivation_reconciles_field_by_field() {
        let k = key();
        let sp = spec();
        let enc = |bytes: Vec<u8>| bytes.len();

        let issuer = enc(coer::encode(MsgType::Crl, &issuer_identifier(&sp)).expect("encodes"));
        let id = enc(coer::encode(
            MsgType::Crl,
            &CertificateId::linkageData(LinkageData::new(
                IValue(Uint16(7)),
                LinkageValue::new([0u8; 9]).to_asn1(),
                None,
            )),
        )
        .expect("encodes"));
        let craca = enc(coer::encode(MsgType::Crl, &sp.craca_id).expect("encodes"));
        let crl_series = enc(coer::encode(MsgType::Crl, &CrlSeries(Uint16(1))).expect("encodes"));
        let validity = enc(coer::encode(
            MsgType::Crl,
            &ValidityPeriod::new(Time32(Uint32(sp.valid_from)), sp.valid_for.clone()),
        )
        .expect("encodes"));
        let permissions = enc(coer::encode(
            MsgType::Crl,
            &SequenceOfPsidSsp(vec![PsidSsp::new(
                Psid(rasn::types::Integer::from(0x20u64)),
                None,
            )]),
        )
        .expect("encodes"));
        let recon = enc(coer::encode(
            MsgType::Crl,
            &reconstruction_key_indicator(&k).expect("builds"),
        )
        .expect("encodes"));
        let verify_key = enc(coer::encode(
            MsgType::Crl,
            &verification_key_indicator(&k).expect("builds"),
        )
        .expect("encodes"));
        let signature = enc(coer::encode(MsgType::Crl, &zero_signature()).expect("encodes"));

        println!(
            "MEASURED certificate components: issuer {issuer} B (§9.2: 9), \
             id/linkageData {id} B (13), cracaId {craca} B (3), crlSeries {crl_series} B (2), \
             validityPeriod {validity} B (7), appPermissions {permissions} B (8), \
             verifyKeyIndicator reconstruction {recon} B (34), verification key \
             {verify_key} B (35), signature {signature} B (66)"
        );

        // Every component the document derives, confirmed.
        assert_eq!(issuer, 9);
        assert_eq!(id, 13);
        assert_eq!(craca, 3);
        assert_eq!(crl_series, 2);
        assert_eq!(validity, 7);
        assert_eq!(recon, 34);
        assert_eq!(verify_key, 35);
        assert_eq!(signature, 66);

        // The one that does not. One `PsidSsp` with a PSID below 128 and no
        // service-specific permissions encodes in five bytes: the `SEQUENCE OF` quantity
        // (a one-byte length determinant plus the one-byte count), the `PsidSsp` preamble,
        // and the PSID as an unbounded INTEGER (a one-byte length plus one byte of value).
        // §9.2 derives eight, which is what the same field costs with an SSP present or a
        // larger PSID — the derivation is right about the shape and generous by three
        // bytes about this instance.
        assert_eq!(
            permissions, 5,
            "one PsidSsp, PSID 0x20, no SSP: quantity 2 + preamble 1 + psid 2"
        );

        // And the three bytes account for the whole difference between the document's
        // totals and the encoder's, on both certificate forms.
        let imp = encoded_size(&implicit(&sp, &k).expect("builds")).expect("encodes");
        let exp =
            encoded_size(&explicit(&sp, &k, zero_signature()).expect("builds")).expect("encodes");
        assert_eq!(
            (80 - imp, 147 - exp),
            (3, 3),
            "implicit {imp} B against §9.2's 80, explicit {exp} B against its 147"
        );
    }

    /// The patched generated type stays reachable (build decision D5).
    #[test]
    fn the_patched_group_permissions_type_is_reachable() {
        assert!(group_permissions_type_is_reachable());
    }
}
