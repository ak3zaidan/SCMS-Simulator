//! **Invariant I-S1**: the real and the modelled crypto backends produce identical
//! verification outcomes and identical sizes.
//!
//! > For any run, `Real` and `Modeled` crypto modes produce identical verification
//! > outcomes, identical sizes, and identical event logs except the `crypto_mode`
//! > manifest field; a golden test runs both on the Phase 2 scenario.
//! > — 03-interfaces.md §6
//!
//! This is the acceptance test. It runs one scenario-shaped script — build a three-level
//! PKI, issue certificates, sign a stream of CAMs and DENMs under a certificate-attachment
//! cadence, verify each one, and then attack them — through both backends and compares the
//! two traces element by element.
//!
//! # What makes this evidence rather than a re-run
//!
//! Three deliberate properties, each of which would let a broken implementation pass if it
//! were missing:
//!
//! 1. **One script, two backends.** `run` is generic over the backend and is compiled
//!    once. There is no per-mode branch anywhere in it, in the envelope, or in the
//!    certificate builder, so the two traces cannot diverge through a code path that
//!    "handles" one mode.
//! 2. **The trace records sizes *and* outcomes, including negative ones.** A test that
//!    only signed and verified would pass on a backend whose `verify` returned `true`
//!    unconditionally. So the script also tampers with the payload, substitutes a
//!    different signer's key, truncates the signature, malleates it to `(r, n − s)`,
//!    presents a revoked certificate, presents one outside its validity period, computes a
//!    token from the peer's public material and the digest alone, and imports key material
//!    that is not a point — and asserts the *sequence* of answers matches,
//!    not merely that both contain some falses. The last two of those are divergences an
//!    earlier version of this file did not cover and an adversarial review constructed by
//!    hand; they are steps now.
//!
//!    The same review pointed out that the invariant was only ever exercised for one
//!    primitive. `the_two_support_sets_are_named_rather_than_discovered` covers the rest of
//!    the catalogue: not with a full signing trace, which the real backend cannot produce
//!    for a post-quantum entry, but by pinning which primitives each backend executes and
//!    asserting that the mode-independent set is the published one. So I-S1 is demonstrated
//!    end to end for `ecdsa-p256-sha256` and `ecqv-p256`, and every other primitive is a
//!    named modelled-only entry that fails loudly in real mode rather than silently
//!    diverging.
//! 3. **The trace is a flat list of strings.** Comparing structured values would let a
//!    field that exists in one mode and not the other slip past `PartialEq` on an enum
//!    variant. A rendered trace makes any difference a diff, and the failure message shows
//!    the first divergent step rather than "assertion failed".
//!
//! The one thing that *must* differ is the key material itself: a real public key is a
//! point on P-256 and a modelled one is 33 bytes that are not. So the trace records
//! *lengths* of key material and certificates, never their bytes — which is exactly the
//! line the invariant draws.

mod common;

use common::{PSID_CAM, T0_UNIX, build_pki, device_linkage};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::NodeId;
use v2xw_core::manifest::CryptoMode;
use v2xw_core::time::{Duration, WallClock};
use v2xw_msg::codec::MsgType;
use v2xw_sec::cert;
use v2xw_sec::crypto::{CryptoBackend, CryptoBackendInfo, Modeled, Real};
use v2xw_sec::envelope::{
    CrlStore, Envelope, GenerationLocation, HeaderInfoSpec, PlanOutcome, SecurityEnvelope,
    SecurityEnvelopeInfo, SignerIdChoice, SignerIdPolicy,
};
use v2xw_sec::linkage::{CrlLinkageEntry, DEFAULT_JMAX};
use v2xw_sec::primitive::{PrimitiveCatalogue, PrimitiveId, PrimitiveOpKind, profiles};
use v2xw_sec::testctx::TestCtx;
use v2xw_sec::{
    MODE_INDEPENDENT_PRIMITIVES, PeerCertCache, SecError, SigToken, runs_in_both_modes,
};

/// How many end entities the script sets up.
const ENTITIES: u32 = 4;

/// How many message events it runs.
const EVENTS: u32 = 12;

/// The tick between message events: 100 ms, a CAM's minimum generation interval.
const TICK: Duration = Duration::from_millis(100);

/// The hardware profile the plan costs are charged against.
const PROFILE: &str = profiles::CORTEX_M4_NRF52840;

fn location(i: u32) -> GenerationLocation {
    GenerationLocation {
        lat_tenth_microdeg: 407_530_000 + i as i32 * 1_000,
        lon_tenth_microdeg: -739_790_000 + i as i32 * 1_000,
        elevation: 4_096 + 100,
    }
}

/// The whole scenario, as a list of rendered steps.
fn run<B>(backend: &mut B, seed: u64) -> Vec<String>
where
    B: CryptoBackend<TestCtx> + CryptoBackendInfo,
{
    let mut ctx = TestCtx::new(seed);
    let mut trace: Vec<String> = Vec::new();
    let step = |t: &mut Vec<String>, what: String| t.push(what);

    // ---- setup -----------------------------------------------------------------------
    let pki = build_pki(&mut ctx, backend, ENTITIES);
    let envelope = Envelope::etsi(WallClock::new(T0_UNIX));
    step(
        &mut trace,
        format!(
            "pki root={}B pca={}B ee={}B entities={}",
            cert::encoded_size(&pki.root).expect("encodes"),
            cert::encoded_size(&pki.pca).expect("encodes"),
            cert::encoded_size(&pki.entities[0].certificate).expect("encodes"),
            pki.entities.len()
        ),
    );

    let anchors = pki.trust_store();
    let crl = CrlStore::new();

    // A receiver's view: it learns each peer's certificate and imports the public key
    // from it, exactly as a node runtime would after P2PCD.
    let mut cache = PeerCertCache::new();
    cache.insert(pki.pca.clone(), true).expect("encodes");
    let mut peer_keys = Vec::new();
    for e in &pki.entities {
        cache.insert(e.certificate.clone(), true).expect("encodes");
        let material = cert::public_key_material(&e.certificate).expect("extracts");
        let handle = backend
            .import_public(PrimitiveId::ECDSA_P256_SHA256, e.node, &material)
            .expect("imports");
        step(
            &mut trace,
            format!("import peer={} material={}B", e.node, material.len()),
        );
        peer_keys.push(handle);
    }

    // ---- the message stream ----------------------------------------------------------
    let mut last_attached: Vec<Option<u64>> = vec![None; pki.entities.len()];
    for event in 0..EVENTS {
        ctx.set_now(TICK.saturating_mul(u64::from(event)).as_nanos());
        for (index, e) in pki.entities.iter().enumerate() {
            // Every fourth event from every entity is a DENM, which the ETSI profile
            // forces to carry a certificate and a generation location.
            let is_denm = (event + index as u32) % 4 == 3;
            let msg_type = if is_denm { MsgType::Denm } else { MsgType::Cam };
            let choice = envelope.choose_signer_id(
                SignerIdPolicy::ETSI_1S,
                ctx.now(),
                last_attached[index],
                Some(msg_type),
            );
            if choice == SignerIdChoice::Certificate {
                last_attached[index] = Some(ctx.now());
            }

            let mut hdr = HeaderInfoSpec {
                psid: PSID_CAM,
                msg_type: Some(msg_type),
                ..HeaderInfoSpec::default()
            };
            if is_denm {
                hdr.generation_location = Some(location(event));
            }
            // A payload whose length varies with the event, so the trace exercises both
            // sides of the COER length-determinant boundary.
            let payload = vec![(event as u8).wrapping_mul(7); 40 + (event as usize * 13) % 120];

            let pdu = envelope
                .sign(&mut ctx, backend, &e.signer, &payload, &hdr, choice)
                .expect("signs");
            step(
                &mut trace,
                format!(
                    "sign e={event} n={} {msg_type} signer={choice:?} size={} overhead={} \
                     payload={} real_coer={}",
                    e.node,
                    pdu.size(),
                    pdu.overhead(),
                    payload.len(),
                    pdu.is_real_coer()
                ),
            );

            // ---- the receiver ---------------------------------------------------------
            let parsed = envelope.parse(pdu.bytes()).expect("parses");
            let plan = envelope.verify_plan(&parsed, &cache, &anchors, &crl);
            step(
                &mut trace,
                format!(
                    "plan e={event} n={} outcome={} ops={} cost={:?} psid={} time={:?} \
                     loc={}",
                    e.node,
                    outcome_label(&plan.outcome),
                    plan.ops.len(),
                    plan.cost(v2xw_sec::PrimitiveCatalogue::standard(), PROFILE)
                        .map(|d| d.as_nanos()),
                    parsed.psid,
                    parsed.generation_time,
                    parsed.generation_location.is_some(),
                ),
            );

            let digest = parsed.signing_digest(e.signer.cert_coer());
            let token = SigToken {
                primitive: PrimitiveId::ECDSA_P256_SHA256,
                bytes: parsed.signature.clone(),
            };

            // 1. The honest case.
            let ok = backend.verify_prehashed(&mut ctx, &peer_keys[index], &digest, &token);
            step(&mut trace, format!("verify e={event} n={} -> {ok}", e.node));

            // 2. A tampered payload. Byte 10 of the COER `tbsData` is inside the
            //    protected payload for every payload this script generates (the
            //    `SignedDataPayload` preamble, the inner `Ieee1609Dot2Data` header and
            //    the length determinant take at most five bytes before it), so flipping
            //    it changes the message and nothing else. Flipping a byte of the *SPDU*
            //    would land in the signature, which proves nothing: the digest would be
            //    unchanged and the original signature would still verify.
            let mut tampered_tbs = parsed.tbs_coer.clone();
            tampered_tbs[10] ^= 0x01;
            let tampered_digest =
                v2xw_sec::envelope::signing_digest(&tampered_tbs, e.signer.cert_coer());
            let tampered_ok =
                backend.verify_prehashed(&mut ctx, &peer_keys[index], &tampered_digest, &token);
            step(
                &mut trace,
                format!("verify-tampered e={event} n={} -> {tampered_ok}", e.node),
            );

            // 3. The wrong signer's key: the same bytes against a different peer.
            let other = (index + 1) % peer_keys.len();
            let wrong_key = backend.verify_prehashed(&mut ctx, &peer_keys[other], &digest, &token);
            step(
                &mut trace,
                format!("verify-wrong-key e={event} n={} -> {wrong_key}", e.node),
            );

            // 3b. Certificate substitution: the same `tbsData` and the same signature,
            //     presented as if a different peer's certificate were the signer. This is
            //     the attack that IEEE 1609.2 §5.3.1's second hash exists to stop, and the
            //     reason the certificate is hashed even when the wire carries only a
            //     digest. An implementation that signed `H(tbsData)` alone would answer
            //     `true` here.
            let substituted = v2xw_sec::envelope::signing_digest(
                &parsed.tbs_coer,
                pki.entities[other].signer.cert_coer(),
            );
            let substituted_ok =
                backend.verify_prehashed(&mut ctx, &peer_keys[index], &substituted, &token);
            step(
                &mut trace,
                format!(
                    "verify-substituted-cert e={event} n={} -> {substituted_ok}",
                    e.node
                ),
            );

            // 4. A truncated signature.
            let mut short = token.clone();
            short.bytes.truncate(token.bytes.len() - 4);
            let truncated = backend.verify_prehashed(&mut ctx, &peer_keys[index], &digest, &short);
            step(
                &mut trace,
                format!("verify-truncated e={event} n={} -> {truncated}", e.node),
            );

            // 4b. The same signature with `s` replaced by `n − s`. ECDSA is malleable:
            //     this is a *second valid signature* over the same message under the same
            //     key, so a stock verifier answers `true` while a modelled backend
            //     comparing token bytes answers `false`. The two modes disagreed here
            //     until `Real` was made to emit and accept only the low-`s`
            //     representative. An adversary model that mutates bytes in transit reaches
            //     this on purpose, so it belongs in the trace rather than in a unit test
            //     alone.
            let mut malleated = token.clone();
            let s_scalar = v2xw_sec::ec::scalar_from_be_mod_n(&token.bytes[32..64]);
            malleated.bytes[32..64].copy_from_slice(&v2xw_sec::ec::scalar_to_be32(&(-s_scalar)));
            let malleated_ok =
                backend.verify_prehashed(&mut ctx, &peer_keys[index], &digest, &malleated);
            step(
                &mut trace,
                format!(
                    "verify-malleated-s e={event} n={} -> {malleated_ok}",
                    e.node
                ),
            );

            // 5. A signature whose bytes are all zero — a forgery attempt that is neither
            //    truncated nor derived from any key.
            let zeros = SigToken {
                primitive: PrimitiveId::ECDSA_P256_SHA256,
                bytes: vec![0u8; token.bytes.len()],
            };
            let forged = backend.verify_prehashed(&mut ctx, &peer_keys[index], &digest, &zeros);
            step(
                &mut trace,
                format!("verify-forged e={event} n={} -> {forged}", e.node),
            );

            // 5b. A forgery computed from exactly what an eavesdropper has: the peer's
            //     public key material, which travels in every certificate, and the digest
            //     of the message being forged. Against the real backend this is 64 bytes
            //     that are not a signature. Against the modelled backend it *was* the
            //     token itself, because the token was keyed on the public material — the
            //     third I-S1 divergence an adversarial review predicted, latent only
            //     because no API exposed it. The step is written from the public inputs
            //     here rather than borrowed from the backend, which is the whole point.
            let material = backend
                .public_material(&peer_keys[index])
                .expect("public material");
            let mut from_public: Vec<u8> = Vec::with_capacity(token.bytes.len());
            let mut counter: u32 = 0;
            while from_public.len() < token.bytes.len() {
                let block = v2xw_core::hash::sha256(
                    &[
                        b"v2xw-sec/modeled/signature/v1".as_slice(),
                        material.as_slice(),
                        digest.as_slice(),
                        &counter.to_be_bytes(),
                    ]
                    .concat(),
                );
                let take = (token.bytes.len() - from_public.len()).min(block.len());
                from_public.extend_from_slice(&block[..take]);
                counter += 1;
            }
            let public_forgery = backend.verify_prehashed(
                &mut ctx,
                &peer_keys[index],
                &digest,
                &SigToken {
                    primitive: PrimitiveId::ECDSA_P256_SHA256,
                    bytes: from_public,
                },
            );
            step(
                &mut trace,
                format!(
                    "verify-forged-from-public-material e={event} n={} -> {public_forgery}",
                    e.node
                ),
            );
        }
    }

    // ---- the rejection paths ---------------------------------------------------------
    let e = &pki.entities[0];
    let hdr = HeaderInfoSpec {
        psid: PSID_CAM,
        msg_type: Some(MsgType::Cam),
        ..HeaderInfoSpec::default()
    };
    let pdu = envelope
        .sign(
            &mut ctx,
            backend,
            &e.signer,
            b"payload",
            &hdr,
            SignerIdChoice::Certificate,
        )
        .expect("signs");
    let parsed = envelope.parse(pdu.bytes()).expect("parses");

    // A cold cache and a digest signer: P2PCD, not a rejection.
    let digest_pdu = envelope
        .sign(
            &mut ctx,
            backend,
            &e.signer,
            b"payload",
            &hdr,
            SignerIdChoice::Digest,
        )
        .expect("signs");
    let digest_parsed = envelope.parse(digest_pdu.bytes()).expect("parses");
    let cold = PeerCertCache::new();
    let cold_plan = envelope.verify_plan(&digest_parsed, &cold, &anchors, &crl);
    step(
        &mut trace,
        format!(
            "cold-cache outcome={} ops={}",
            outcome_label(&cold_plan.outcome),
            cold_plan.ops.len()
        ),
    );

    // A hash CRL entry naming this certificate.
    let mut revoked_crl = CrlStore::new();
    revoked_crl
        .revoke_hash(v2xw_sec::hashedid::certificate_hashed_id10(&e.certificate).expect("hash"));
    let revoked_plan = envelope.verify_plan(&parsed, &cache, &anchors, &revoked_crl);
    step(
        &mut trace,
        format!(
            "hash-crl outcome={} ops={}",
            outcome_label(&revoked_plan.outcome),
            revoked_plan.ops.len()
        ),
    );

    // A linkage CRL entry revoking the device from its i-period forward.
    let mut linked_crl = CrlStore::new();
    linked_crl.add_linkage_entry(CrlLinkageEntry::from_device(
        &device_linkage(1),
        7,
        DEFAULT_JMAX,
    ));
    let linked_plan = envelope.verify_plan(&parsed, &cache, &anchors, &linked_crl);
    step(
        &mut trace,
        format!(
            "linkage-crl outcome={} ops={}",
            outcome_label(&linked_plan.outcome),
            linked_plan.ops.len()
        ),
    );

    // An SPDU generated long after the certificate expired.
    ctx.set_now(Duration::from_secs(11_000 * 60).as_nanos());
    let stale = envelope
        .sign(
            &mut ctx,
            backend,
            &e.signer,
            b"payload",
            &hdr,
            SignerIdChoice::Certificate,
        )
        .expect("signs");
    let stale_parsed = envelope.parse(stale.bytes()).expect("parses");
    let stale_plan = envelope.verify_plan(&stale_parsed, &cache, &anchors, &crl);
    step(
        &mut trace,
        format!(
            "expired outcome={} ops={} size={}",
            outcome_label(&stale_plan.outcome),
            stale_plan.ops.len(),
            stale.size()
        ),
    );

    // Signing with a handle the backend holds only the public half of must fail in both
    // modes, with the same error kind.
    let public_only = v2xw_sec::KeyHandle {
        id: peer_keys[0].id,
        primitive: peer_keys[0].primitive,
        owner: peer_keys[0].owner,
        backend: peer_keys[0].backend,
    };
    let err = backend
        .sign_prehashed(&mut ctx, &public_only, &[0u8; 32])
        .expect_err("a public key cannot sign");
    step(
        &mut trace,
        format!("sign-with-public-key -> {}", error_label(&err)),
    );

    // Key material that is not a point. It arrives from a received certificate's
    // `verifyKeyIndicator`, i.e. from a peer, so both modes have to answer it the same
    // way — and they did not: the real backend validated curve membership, which the
    // modelled backend cannot do, so a crafted certificate took different branches
    // depending on `crypto_mode`. Both now check shape only, and reject at verification.
    let mut off_curve = [0u8; 33];
    off_curve[0] = 0x02;
    off_curve[32] = 1; // x = 1 has no y on P-256.
    let bad_material: Vec<(&str, Vec<u8>)> = vec![
        ("off-curve", off_curve.to_vec()),
        ("empty", Vec::new()),
        ("identity", vec![0x00]),
        ("short", vec![0x02; 32]),
        ("long", vec![0x02; 34]),
        ("uncompressed", vec![0x04; 65]),
        ("bad-prefix", vec![0x05; 33]),
    ];
    for (name, material) in &bad_material {
        let outcome =
            match backend.import_public(PrimitiveId::ECDSA_P256_SHA256, NodeId::new(900), material)
            {
                Ok(_) => "ok".to_string(),
                Err(err) => error_label(&err).to_string(),
            };
        step(&mut trace, format!("import-invalid {name} -> {outcome}"));
    }

    // The one shape that imports is still useless: nothing verifies under it. Rendered
    // rather than unwrapped, so that a backend which refused the import shows up as a
    // *diff* against the other mode instead of a panic halfway through building the
    // trace — the divergence this step exists to catch is exactly "one mode refused".
    let off_curve_ok =
        match backend.import_public(PrimitiveId::ECDSA_P256_SHA256, NodeId::new(901), &off_curve) {
            Ok(key) => backend
                .verify_prehashed(
                    &mut ctx,
                    &key,
                    &[0x5au8; 32],
                    &SigToken {
                        primitive: PrimitiveId::ECDSA_P256_SHA256,
                        bytes: vec![0u8; 64],
                    },
                )
                .to_string(),
            Err(err) => error_label(&err),
        };
    step(&mut trace, format!("verify-offcurve-key -> {off_curve_ok}"));

    // Costs are charged from the tables, so they are the same number in both modes.
    step(
        &mut trace,
        format!(
            "cost verify={:?} sign={:?}",
            backend
                .cost(
                    PrimitiveId::ECDSA_P256_SHA256,
                    PrimitiveOpKind::Verify,
                    PROFILE
                )
                .map(|d| d.as_nanos()),
            backend
                .cost(
                    PrimitiveId::ECDSA_P256_SHA256,
                    PrimitiveOpKind::Sign,
                    PROFILE
                )
                .map(|d| d.as_nanos()),
        ),
    );

    trace
}

fn outcome_label(o: &PlanOutcome) -> String {
    match o {
        PlanOutcome::Verifiable => "verifiable".to_string(),
        PlanOutcome::NeedCertificate(r) => format!("need-certificate({:?})", r.kind),
        PlanOutcome::Reject(r) => format!("reject({r:?})"),
    }
}

fn error_label(e: &v2xw_sec::SecError) -> String {
    let label = match e {
        v2xw_sec::SecError::PublicKeyOnly { .. } => "public-key-only",
        v2xw_sec::SecError::UnknownKey { .. } => "unknown-key",
        v2xw_sec::SecError::WrongBackend { .. } => "wrong-backend",
        // The two an invalid public-key import produces. Distinguished rather than
        // lumped into "other", because "both modes failed somehow" is a weaker claim than
        // "both modes failed the same way".
        v2xw_sec::SecError::BadLength { expected, got, .. } => {
            return format!("bad-length({expected} vs {got})");
        }
        v2xw_sec::SecError::Crypto { op, .. } => return format!("crypto({op})"),
        _ => "other",
    };
    label.to_string()
}

/// The headline assertion: one script, two backends, identical traces.
#[test]
fn the_two_backends_agree_on_every_outcome_and_every_size() {
    let mut real = Real::new();
    let mut modeled = Modeled::new();
    let real_trace = run(&mut real, 0xC0FFEE);
    let modeled_trace = run(&mut modeled, 0xC0FFEE);

    assert_eq!(
        real.mode(),
        CryptoMode::Real,
        "the two backends really are in different modes"
    );
    assert_eq!(modeled.mode(), CryptoMode::Modeled);

    // Report before asserting, so a failing run shows the shape of what it compared.
    println!(
        "I-S1: compared {} steps between crypto-backend/real and crypto-backend/modeled",
        real_trace.len()
    );
    assert!(
        real_trace.len() > 300,
        "the script must be long enough to be evidence; it produced {} steps",
        real_trace.len()
    );

    for (i, (r, m)) in real_trace.iter().zip(modeled_trace.iter()).enumerate() {
        assert_eq!(
            r, m,
            "\nI-S1 violated at step {i}:\n  real:    {r}\n  modeled: {m}\n"
        );
    }
    assert_eq!(
        real_trace.len(),
        modeled_trace.len(),
        "the two modes must run the same number of steps"
    );
}

/// The negative cases really are in the trace. Without this, the equivalence test above
/// would pass on a pair of backends that both said `true` to everything.
#[test]
fn the_script_actually_exercises_the_failure_paths() {
    let mut real = Real::new();
    let trace = run(&mut real, 0xC0FFEE);

    let count = |needle: &str| trace.iter().filter(|s| s.contains(needle)).count();

    assert_eq!(
        count("-> true"),
        (EVENTS * ENTITIES) as usize,
        "exactly the honest verifications succeed"
    );
    assert_eq!(
        count("-> false"),
        (EVENTS * ENTITIES * 7) as usize + 1,
        "seven attacks per message, all rejected, plus the off-curve key"
    );
    assert!(count("verify-tampered") > 0);
    assert!(count("verify-wrong-key") > 0);
    assert!(count("verify-truncated") > 0);
    assert!(count("verify-forged") > 0);
    assert!(count("verify-substituted-cert") > 0);
    assert!(count("verify-malleated-s") > 0);
    assert_eq!(
        trace
            .iter()
            .filter(
                |s| s.starts_with("verify-forged-from-public-material") && s.ends_with("-> false")
            )
            .count(),
        (EVENTS * ENTITIES) as usize,
        "a token computed from the peer's public material and the digest must be refused \
         in both modes, on every message"
    );
    assert_eq!(count("import-invalid"), 7);
    assert!(count("verify-offcurve-key -> false") == 1);
    assert!(
        trace
            .iter()
            .any(|s| s.contains("cold-cache") && s.contains("need-certificate")),
        "a digest signer with a cold cache must plan a P2PCD request"
    );
    assert!(
        trace
            .iter()
            .any(|s| s.contains("hash-crl") && s.contains("reject(Revoked)")),
        "a hash CRL entry must reject"
    );
    assert!(
        trace
            .iter()
            .any(|s| s.contains("linkage-crl") && s.contains("reject(Revoked)")),
        "a linkage CRL entry must reject the device it revokes"
    );
    assert!(
        trace
            .iter()
            .any(|s| s.contains("expired") && s.contains("reject(OutsideValidityPeriod)")),
        "an SPDU generated after the certificate expired must be rejected"
    );
    assert!(
        trace
            .iter()
            .any(|s| s.contains("sign-with-public-key -> public-key-only"))
    );
    // Both signer identifiers occur, so the cadence policy is really being exercised.
    assert!(trace.iter().any(|s| s.contains("signer=Digest")));
    assert!(trace.iter().any(|s| s.contains("signer=Certificate")));
}

/// Which primitives each backend executes must be *named*, not discovered when a run is
/// switched to real mode and stops working.
///
/// The two sets are deliberately unequal: [`Real`] implements the two P-256 entries, while
/// [`Modeled`] admits every `Signature` and `ImplicitCert` entry in the catalogue, which is
/// the point of a modelled backend — a post-quantum SPDU's size can be modelled without a
/// PQC implementation existing. An adversarial review flagged the inequality as a way for a
/// run to be silently mode-dependent, so this pins three things: real never exceeds
/// modelled, `supports` and `keygen` agree within each backend, and the mode-independent
/// set is exactly `MODE_INDEPENDENT_PRIMITIVES` — the list a scenario loader checks. A
/// run that selects anything else fails *loudly* at keygen in real mode, which is asserted
/// here too, rather than producing different outcomes from a modelled run.
#[test]
fn the_two_support_sets_are_named_rather_than_discovered() {
    let mut ctx = TestCtx::new(3);
    let mut real = Real::new();
    let mut modeled = Modeled::new();
    let mut mode_independent = Vec::new();

    for d in PrimitiveCatalogue::standard().iter() {
        let r = real.supports(d.id);
        let m = modeled.supports(d.id);
        assert!(
            m || !r,
            "{}: the real backend executes a primitive the modelled one refuses, which \
             would make a real run impossible to model",
            d.id
        );
        assert_eq!(
            r,
            runs_in_both_modes(d.id),
            "{}: the published mode-independence rule disagrees with the real backend",
            d.id
        );
        // `supports` must not be a claim `keygen` then contradicts, in either backend.
        assert_eq!(
            real.keygen(&mut ctx, d.id, NodeId::new(1)).is_ok(),
            r,
            "{}: real supports/keygen disagree",
            d.id
        );
        assert_eq!(
            modeled.keygen(&mut ctx, d.id, NodeId::new(1)).is_ok(),
            m,
            "{}: modelled supports/keygen disagree",
            d.id
        );
        if r {
            mode_independent.push(d.id);
        }
    }
    assert_eq!(mode_independent, MODE_INDEPENDENT_PRIMITIVES.to_vec());

    // A modelled-only primitive is refused by name, before anything is signed.
    for p in [PrimitiveId::ML_DSA_44, PrimitiveId::FALCON_512] {
        assert!(modeled.supports(p) && !runs_in_both_modes(p));
        let err = real
            .keygen(&mut ctx, p, NodeId::new(2))
            .expect_err("the real backend implements no post-quantum primitive");
        assert!(
            matches!(err, SecError::UnsupportedPrimitive { .. }),
            "{err}"
        );
    }
}

/// Two runs of the same backend with the same seed produce the same trace. Determinism is
/// the premise of the equivalence test — without it, a difference between the two modes
/// could be a difference between two runs.
#[test]
fn a_backend_is_deterministic_across_runs() {
    for _ in 0..2 {
        let mut a = Real::new();
        let mut b = Real::new();
        assert_eq!(run(&mut a, 7), run(&mut b, 7));
        let mut c = Modeled::new();
        let mut d = Modeled::new();
        assert_eq!(run(&mut c, 7), run(&mut d, 7));
    }
}

/// The trace is seed-independent, and the material underneath it is not.
///
/// Both halves matter. The trace records sizes and outcomes only — never key bytes — so
/// two seeds must give the *same* trace: that is the property that makes the equivalence
/// comparison above meaningful, because a trace containing key material could never match
/// between a real backend and a modelled one. And the keys themselves must differ, or the
/// seed is being ignored and `a_backend_is_deterministic_across_runs` would be passing for
/// the wrong reason.
#[test]
fn the_trace_is_seed_independent_but_the_keys_are_not() {
    let mut a = Real::new();
    let mut b = Real::new();
    assert_eq!(
        run(&mut a, 7),
        run(&mut b, 8),
        "sizes and outcomes must not depend on the seed"
    );

    let mut ctx7 = TestCtx::new(7);
    let mut ctx8 = TestCtx::new(8);
    let mut c = Real::new();
    let mut d = Real::new();
    let pki7 = build_pki(&mut ctx7, &mut c, 1);
    let pki8 = build_pki(&mut ctx8, &mut d, 1);
    assert_ne!(
        v2xw_sec::certificate_digest(&pki7.entities[0].certificate).expect("digest"),
        v2xw_sec::certificate_digest(&pki8.entities[0].certificate).expect("digest"),
        "a different seed must give a different key and therefore a different certificate"
    );
    assert_eq!(
        cert::encoded_size(&pki7.entities[0].certificate).expect("encodes"),
        cert::encoded_size(&pki8.entities[0].certificate).expect("encodes"),
        "and the same size, which is why the trace can be seed-independent"
    );
}

/// Both backends draw the same number of random values from the same stream, so switching
/// modes does not shift any later draw in the run. That is the "identical event logs"
/// half of I-S1 for everything downstream of the security layer.
#[test]
fn both_modes_consume_the_rng_identically() {
    fn remaining(seed: u64, mode: CryptoMode) -> u64 {
        let mut ctx = TestCtx::new(seed);
        match mode {
            CryptoMode::Real => {
                let mut b = Real::new();
                build_pki(&mut ctx, &mut b, ENTITIES);
            }
            CryptoMode::Modeled => {
                let mut b = Modeled::new();
                build_pki(&mut ctx, &mut b, ENTITIES);
            }
        }
        // The next draw from an unrelated entity must be the same in both modes, and the
        // next draw from the *same* stream must be at the same position.
        let mut guard = ctx.rng(
            v2xw_core::rng::RngDomain::Crypto,
            v2xw_core::rng::EntityRef::Node(NodeId::new(0)),
        );
        guard.u64()
    }
    assert_eq!(
        remaining(42, CryptoMode::Real),
        remaining(42, CryptoMode::Modeled),
        "the two modes must leave the crypto stream at the same position"
    );
}
