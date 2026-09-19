//! Measuring the security envelope against 04-models.md §9.1's derivation.
//!
//! §9.1 derives the envelope overhead field by field from the ASN.1 and arrives at
//! "≈ 93-94 bytes with a digest signer" and "≈ 87 + certificate with a certificate
//! signer". That was arithmetic in a document. These tests are the encoder's answer.
//!
//! The derivation, so a failure says which line is wrong:
//!
//! | Component | §9.1 | Where it comes from |
//! |---|---|---|
//! | `Ieee1609Dot2Data` outer | 2 | `protocolVersion` 1 + content choice tag 1 |
//! | `hashId` | 1 | the `HashAlgorithm` enumerated |
//! | `SignedDataPayload` preamble + inner `Ieee1609Dot2Data` | 1 + (1 + 1 + 1-2 length) | |
//! | `HeaderInfo` preamble + `psid` + `generationTime` | 1 + 2 + 8 | PSID 0x20 is 2; an ITS-AID ≥ 256 is 3 |
//! | `generationLocation` (DENM) | 10 | Latitude 4 + Longitude 4 + Elevation 2 |
//! | signer = digest | 9 | choice tag 1 + `HashedId8` 8 |
//! | signer = certificate | 3 + cert | choice tag 1 + `SequenceOfCertificate` quantity 2 |
//! | ECDSA P-256 signature | 66 | choice 1 + `rSig` (1 + 32) + `sSig` 32 |
//!
//! `ToBeSignedData` itself contributes nothing: it is a non-extensible SEQUENCE with no
//! optional fields, so COER gives it no preamble.

mod common;

use common::{PSID_CAM, PSID_WIDE, T0_1609_SECONDS, T0_UNIX, build_pki};
use v2xw_core::ctx::Ctx;
use v2xw_core::time::{Duration, WallClock};
use v2xw_msg::codec::MsgType;
use v2xw_sec::cert;
use v2xw_sec::crypto::Real;
use v2xw_sec::envelope::{
    Envelope, EnvelopeProfile, GenerationLocation, HeaderInfoSpec, ParsedSigner, SecurityEnvelope,
    SecurityEnvelopeInfo, SignerIdChoice,
};
use v2xw_sec::testctx::TestCtx;

/// The overhead §9.1 derives for a digest signer under PSID 0x20.
const DERIVED_DIGEST_OVERHEAD: u32 = 93;

/// The overhead §9.1 derives for a certificate signer, before the certificate itself.
const DERIVED_CERT_OVERHEAD_WITHOUT_CERT: u32 = 87;

/// The `generationLocation` DENM carries, per TS 103 097 §7.1.2.
const DERIVED_GENERATION_LOCATION: u32 = 10;

fn location() -> GenerationLocation {
    GenerationLocation {
        // 40.7530 N, 73.9790 W, the centre of the Phase 1 Manhattan bounding box
        // (build decision D7), in tenths of a microdegree.
        lat_tenth_microdeg: 407_530_000,
        lon_tenth_microdeg: -739_790_000,
        elevation: 4_096 + 100,
    }
}

fn spec(psid: u64, msg_type: MsgType) -> HeaderInfoSpec {
    HeaderInfoSpec {
        psid,
        msg_type: Some(msg_type),
        ..HeaderInfoSpec::default()
    }
}

/// The headline measurement: the real encoder against the document's arithmetic.
#[test]
fn the_measured_overhead_matches_the_derivation() {
    let mut ctx = TestCtx::new(1);
    let mut backend = Real::new();
    let pki = build_pki(&mut ctx, &mut backend, 1);
    let envelope = Envelope::ieee1609(WallClock::new(T0_UNIX));
    let signer = &pki.entities[0].signer;
    let cert_bytes = cert::encoded_size(&pki.entities[0].certificate).expect("encodes");

    // A payload short enough for a one-byte COER length determinant, which is what §9.1's
    // "1-2 length" alternative assumes for the common case.
    let payload = b"0123456789";

    let digest_pdu = envelope
        .sign(
            &mut ctx,
            &mut backend,
            signer,
            payload,
            &spec(PSID_CAM, MsgType::Cam),
            SignerIdChoice::Digest,
        )
        .expect("signs");
    let cert_pdu = envelope
        .sign(
            &mut ctx,
            &mut backend,
            signer,
            payload,
            &spec(PSID_CAM, MsgType::Cam),
            SignerIdChoice::Certificate,
        )
        .expect("signs");

    println!(
        "MEASURED envelope overhead: digest signer = {} B, certificate signer = {} B \
         (= {} + certificate {} B), certificate = {} B, SPDU sizes {} / {} B for a \
         {}-byte payload",
        digest_pdu.overhead(),
        cert_pdu.overhead(),
        cert_pdu.overhead() - cert_bytes,
        cert_bytes,
        cert_bytes,
        digest_pdu.size(),
        cert_pdu.size(),
        payload.len(),
    );

    assert!(digest_pdu.is_real_coer(), "the bytes must be real COER");
    assert!(cert_pdu.is_real_coer());
    assert_eq!(
        digest_pdu.overhead(),
        DERIVED_DIGEST_OVERHEAD,
        "§9.1 derives 93 bytes with a digest signer under PSID 0x20"
    );
    assert_eq!(
        cert_pdu.overhead() - cert_bytes,
        DERIVED_CERT_OVERHEAD_WITHOUT_CERT,
        "§9.1 derives 87 bytes plus the certificate"
    );
    // The two overheads differ by exactly the signer identifier: 9 bytes of digest
    // against 3 + the certificate.
    assert_eq!(
        cert_pdu.overhead() - digest_pdu.overhead(),
        3 + cert_bytes - 9
    );
}

/// §9.1 says "≈ 93-94": the 94 is a PSID that needs a second COER *value* byte. This pins
/// the boundary itself rather than one point past it.
///
/// The document, and this test, used to say the extra byte arrives at 128. It does not.
/// `Psid ::= INTEGER (0..MAX)` has a lower bound of zero, so X.696 §11 encodes it as a
/// length-prefixed **unsigned** integer with no sign bit to make room for: 0x80 is one
/// value byte, and the second one arrives at 0x100. The old assertion passed because it
/// only ever measured 0x8000, which is three bytes for a different reason — the test was
/// true and its stated reason was false, which is the shape of check this project treats
/// as worse than no check.
#[test]
fn the_ninety_four_in_the_range_is_the_wide_psid() {
    let mut ctx = TestCtx::new(2);
    let mut backend = Real::new();
    let pki = build_pki(&mut ctx, &mut backend, 1);
    let envelope = Envelope::ieee1609(WallClock::new(T0_UNIX));
    let signer = &pki.entities[0].signer;

    let overhead_of = |ctx: &mut TestCtx, backend: &mut Real, psid: u64| {
        envelope
            .sign(
                ctx,
                backend,
                signer,
                b"payload",
                &spec(psid, MsgType::Cam),
                SignerIdChoice::Digest,
            )
            .expect("signs")
            .overhead()
    };

    // The sweep straddles both candidate thresholds: 128 (wrong) and 256 (right).
    let sweep: Vec<(u64, u32)> = [
        0x0u64, 0x1, 0x20, 0x7f, 0x80, 0xff, 0x100, 0x7fff, 0x8000, 0x1_0000,
    ]
    .iter()
    .map(|&psid| (psid, overhead_of(&mut ctx, &mut backend, psid)))
    .collect();
    for (psid, overhead) in &sweep {
        println!("MEASURED overhead: PSID {psid:#x} = {overhead} B");
    }
    assert_eq!(
        sweep,
        vec![
            (0x0, 93),
            (0x1, 93),
            (0x20, 93),
            (0x7f, 93),
            // 0x80 is where the derivation used to claim the extra byte appears. It does
            // not: `Psid` is unsigned, so 0x80 still fits one value byte.
            (0x80, 93),
            (0xff, 93),
            // The real boundary: 0x100 is the first PSID needing two value bytes.
            (0x100, 94),
            (0x7fff, 94),
            (0x8000, 94),
            (0x1_0000, 95),
        ],
        "a PSID of 256 or more takes one more COER byte, which is §9.1\'s 94"
    );
    assert_eq!(
        overhead_of(&mut ctx, &mut backend, PSID_CAM),
        93,
        "the CAM PSID is the 93 end of the published range"
    );
    assert_eq!(overhead_of(&mut ctx, &mut backend, PSID_WIDE), 94);
}

/// A DENM carries `generationLocation`, and §9.1 derives 10 bytes for it.
#[test]
fn a_denm_pays_ten_bytes_for_its_generation_location() {
    let mut ctx = TestCtx::new(3);
    let mut backend = Real::new();
    let pki = build_pki(&mut ctx, &mut backend, 1);
    let envelope = Envelope::etsi(WallClock::new(T0_UNIX));
    let signer = &pki.entities[0].signer;

    let mut denm = spec(PSID_CAM, MsgType::Denm);
    denm.generation_location = Some(location());
    // TS 103 097 §7.1.2: a DENM always signs with a certificate, whatever the cadence
    // policy says, so the envelope resolves the choice itself.
    let choice = envelope.choose_signer_id(
        v2xw_sec::envelope::SignerIdPolicy::DIGEST_ONLY,
        ctx.now(),
        Some(ctx.now()),
        Some(MsgType::Denm),
    );
    assert_eq!(
        choice,
        SignerIdChoice::Certificate,
        "the ETSI profile forces a certificate on a DENM"
    );

    let with_location = envelope
        .sign(&mut ctx, &mut backend, signer, b"payload", &denm, choice)
        .expect("signs");
    let cam = envelope
        .sign(
            &mut ctx,
            &mut backend,
            signer,
            b"payload",
            &spec(PSID_CAM, MsgType::Cam),
            choice,
        )
        .expect("signs");
    println!(
        "MEASURED generationLocation costs {} B",
        with_location.overhead() - cam.overhead()
    );
    assert_eq!(
        with_location.overhead() - cam.overhead(),
        DERIVED_GENERATION_LOCATION
    );

    // And the ETSI profile refuses a DENM without one, rather than emitting bytes the
    // field would reject.
    let mut no_location = denm.clone();
    no_location.generation_location = None;
    assert!(
        envelope
            .sign(
                &mut ctx,
                &mut backend,
                signer,
                b"payload",
                &no_location,
                choice
            )
            .is_err(),
        "TS 103 097 §7.1.2 requires generationLocation on a DENM"
    );
    // The unconstrained 1609.2 envelope does not require it.
    assert!(
        Envelope::ieee1609(WallClock::new(T0_UNIX))
            .sign(
                &mut ctx,
                &mut backend,
                signer,
                b"payload",
                &no_location,
                choice
            )
            .is_ok()
    );
}

/// The overhead is constant across payload sizes until the COER length determinant of the
/// inner `unsecuredData` needs a second byte — which is §9.1's "1-2 length" alternative,
/// and the only reason the overhead is not a single number.
#[test]
fn the_overhead_grows_only_where_the_length_determinant_does() {
    let mut ctx = TestCtx::new(4);
    let mut backend = Real::new();
    let pki = build_pki(&mut ctx, &mut backend, 1);
    let envelope = Envelope::ieee1609(WallClock::new(T0_UNIX));
    let signer = &pki.entities[0].signer;

    let mut seen = Vec::new();
    for len in [1usize, 10, 100, 127, 128, 200, 255, 256, 400] {
        let payload = vec![0x5a; len];
        let pdu = envelope
            .sign(
                &mut ctx,
                &mut backend,
                signer,
                &payload,
                &spec(PSID_CAM, MsgType::Cam),
                SignerIdChoice::Digest,
            )
            .expect("signs");
        assert_eq!(
            pdu.size() as usize - len,
            pdu.overhead() as usize,
            "size must be payload + overhead exactly (invariant I-S3)"
        );
        seen.push((len, pdu.overhead()));
    }
    println!("MEASURED overhead by payload length: {seen:?}");
    // Below 128 the length determinant is one byte, so the overhead is the derived 93.
    for (len, overhead) in &seen {
        let expected = if *len < 128 {
            93
        } else if *len < 256 {
            // A long-form determinant: 0x81 then the length.
            94
        } else {
            95
        };
        assert_eq!(*overhead, expected, "payload {len}");
    }
}

/// The ETSI profile is the same bytes as 1609.2 with the same inputs — it is a profile,
/// not a different structure [TS 103 097 V2.1.1 §5.1-5.2].
#[test]
fn the_etsi_profile_is_byte_identical_where_it_is_permitted() {
    let mut ctx = TestCtx::new(5);
    let mut backend = Real::new();
    let pki = build_pki(&mut ctx, &mut backend, 1);
    let signer = &pki.entities[0].signer;
    let wall = WallClock::new(T0_UNIX);

    let ieee = Envelope::ieee1609(wall)
        .sign(
            &mut ctx,
            &mut backend,
            signer,
            b"payload",
            &spec(PSID_CAM, MsgType::Cam),
            SignerIdChoice::Digest,
        )
        .expect("signs");
    let etsi = Envelope::etsi(wall)
        .sign(
            &mut ctx,
            &mut backend,
            signer,
            b"payload",
            &spec(PSID_CAM, MsgType::Cam),
            SignerIdChoice::Digest,
        )
        .expect("signs");
    assert_eq!(ieee.bytes(), etsi.bytes());
    assert_eq!(
        Envelope::etsi(wall).profile(),
        EnvelopeProfile::EtsiTs103097
    );

    // What differs is what the ETSI profile refuses: §5.2 forbids p2pcdLearningRequest in
    // a signed SPDU, and 1609.2 clause 8 permits it.
    let mut with_request = spec(PSID_CAM, MsgType::Cam);
    with_request.p2pcd_learning_request =
        Some(v2xw_sec::hashedid::hashed_id3(b"an unknown issuer"));
    assert!(
        Envelope::etsi(wall)
            .sign(
                &mut ctx,
                &mut backend,
                signer,
                b"payload",
                &with_request,
                SignerIdChoice::Digest
            )
            .is_err()
    );
    assert!(
        Envelope::ieee1609(wall)
            .sign(
                &mut ctx,
                &mut backend,
                signer,
                b"payload",
                &with_request,
                SignerIdChoice::Digest
            )
            .is_ok()
    );
}

/// A signed PDU parses back to what went in, and the signature it carries verifies
/// against the signer's key through the backend.
#[test]
fn a_signed_pdu_round_trips_and_verifies() {
    let mut ctx = TestCtx::new(6);
    let mut backend = Real::new();
    let pki = build_pki(&mut ctx, &mut backend, 1);
    let envelope = Envelope::ieee1609(WallClock::new(T0_UNIX));
    let e = &pki.entities[0];
    let payload = b"a CAM's UPER bytes would go here";

    for choice in [SignerIdChoice::Digest, SignerIdChoice::Certificate] {
        let pdu = envelope
            .sign(
                &mut ctx,
                &mut backend,
                &e.signer,
                payload,
                &spec(PSID_CAM, MsgType::Cam),
                choice,
            )
            .expect("signs");
        let parsed = envelope.parse(pdu.bytes()).expect("parses");
        assert_eq!(parsed.payload, payload);
        assert_eq!(parsed.psid, PSID_CAM);
        assert_eq!(parsed.size, pdu.size());
        assert_eq!(
            parsed.generation_time,
            Some(u64::from(T0_1609_SECONDS) * 1_000_000),
            "generationTime is microseconds since 2004-01-01, and t is still t0"
        );
        match (&parsed.signer, choice) {
            (v2xw_sec::ParsedSigner::Digest(d), SignerIdChoice::Digest) => {
                assert_eq!(d, e.signer.digest());
            }
            (
                ParsedSigner::Certificate {
                    certificate,
                    digest,
                },
                SignerIdChoice::Certificate,
            ) => {
                assert_eq!(certificate.as_ref(), e.certificate.as_ref());
                assert_eq!(digest, e.signer.digest());
            }
            (got, want) => panic!("signer {got:?} does not match the choice {want:?}"),
        }

        // The signature really is over H(H(tbsData) ‖ H(signer certificate)) — the
        // detail that makes a digest signer interoperable. A verifier that hashed only
        // the tbsData would pass its own tests and fail against every real stack.
        let pk = {
            use v2xw_sec::crypto::CryptoBackendInfo;
            backend.public_of(&e.key).expect("pub")
        };
        let token = v2xw_sec::SigToken {
            primitive: v2xw_sec::PrimitiveId::ECDSA_P256_SHA256,
            bytes: parsed.signature.clone(),
        };
        let digest = {
            use v2xw_core::hash::sha256;
            sha256(&[sha256(&parsed.tbs_coer), sha256(e.signer.cert_coer())].concat())
        };
        use v2xw_sec::crypto::CryptoBackend;
        assert!(
            backend.verify_prehashed(&mut ctx, &pk, &digest, &token),
            "the SPDU signature must verify over the 1609.2 §5.3.1 digest"
        );
        // And not over the naive one.
        let naive = v2xw_core::hash::sha256(&parsed.tbs_coer);
        assert!(!backend.verify_prehashed(&mut ctx, &pk, &naive, &token));
    }
}

/// `generationTime` is microseconds since 2004-01-01, not since the Unix epoch. A
/// simulator that got this wrong would produce SPDUs every receiver considered 36 years
/// stale, and nothing else would complain.
#[test]
fn the_generation_time_is_on_the_ieee_epoch() {
    let mut ctx = TestCtx::new(7);
    let mut backend = Real::new();
    let pki = build_pki(&mut ctx, &mut backend, 1);
    let envelope = Envelope::ieee1609(WallClock::new(T0_UNIX));
    ctx.set_now(Duration::from_millis(1_500).as_nanos());

    let pdu = envelope
        .sign(
            &mut ctx,
            &mut backend,
            &pki.entities[0].signer,
            b"payload",
            &spec(PSID_CAM, MsgType::Cam),
            SignerIdChoice::Digest,
        )
        .expect("signs");
    let parsed = envelope.parse(pdu.bytes()).expect("parses");
    let expected_us = u64::from(T0_1609_SECONDS) * 1_000_000 + 1_500_000;
    assert_eq!(parsed.generation_time, Some(expected_us));
    // 2026-01-01T00:00:00Z is 694,310,400 s after 2004-01-01T00:00:00Z, so 1.5 s into
    // the run the whole-second part is one more than that.
    assert_eq!(T0_1609_SECONDS, 694_310_400);
    assert_eq!(expected_us / 1_000_000, 694_310_401);
}
