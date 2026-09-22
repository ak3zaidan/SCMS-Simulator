//! Real payloads, real signatures, real bytes — the tests that would have caught the
//! constant-size stub on day one.
//!
//! # The defect these exist for
//!
//! Until 2026-09-22 `ObuRuntime::generate` called neither a codec nor a crypto backend. It
//! produced a `u32` — `93 + signer identifier`, over a zero-length payload — and handed
//! that to the engine as a message. An independent audit found it *before reading a line
//! of the implementation*, from one number in a report: a J2735 basic safety message and
//! an ETSI cooperative awareness message both came out at exactly 101 bytes. Two
//! standards, two formats, two field sets, one size. That cannot happen to bytes anything
//! encoded, and `a_safety_message_and_an_awareness_message_do_not_have_the_same_size` is
//! that observation turned into an assertion.
//!
//! It is the cheapest test in the crate and it was worth more than the rest of the suite,
//! because security overhead as a fraction of airtime, verification cost under load and
//! what a node does when it cannot keep up with signing are the three headline results
//! this simulator exists to produce — and all three are ratios whose numerator was a
//! constant.
//!
//! # Faults injected to prove these checks can fail
//!
//! * `encode_payload` made to return `Some(vec![0u8; 61])` for both message types —
//!   `a_safety_message_and_an_awareness_message_do_not_have_the_same_size` fails, which is
//!   the original defect reproduced exactly.
//! * `SignedFrame::payload` populated from the SPDU rather than the payload —
//!   `the_bytes_a_node_produced_decode_back_through_the_real_codec` fails at the first
//!   `decode_message_frame`.
//! * The tamper in `a_signature_verifies_against_its_certificate` removed — the negative
//!   control passes trivially, so the two halves are asserted together and the positive
//!   half is asserted to be `Valid` rather than merely "not Invalid".
//! * `OpDescriptor::sign` charged against a fresh `ServerBank` per call —
//!   `a_slow_signer_queues_behind_its_own_throughput` fails, because every frame is then
//!   ready one service time after it was generated however slow the signer is.

use v2xw_core::belief::{FixQuality, PositionEstimate};
use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::rng::RngRegistry;
use v2xw_core::time::{NS_PER_MS, NS_PER_S, SimTime};
use v2xw_msg::MsgType;
use v2xw_msg::codec::{EtsiUperCodec, MessageCodec};
use v2xw_msg::j2735::bsm;
use v2xw_node::ctx::NodeRuntimeCtx;
use v2xw_node::generate::ServiceSet;
use v2xw_node::policy::VerifyAll;
use v2xw_node::profile::HardwareProfile;
use v2xw_node::runtime::{NodeConfig, ObuRuntime, StepOutcome, Transmission};
use v2xw_node::secure::{CryptoMode, NodeSecurity, SpduVerdict};
use v2xw_node::stores::{CredState, CredentialHandle, pseudo_signer};

/// The node under test. One is enough: every assertion here is about what one station
/// puts on the air.
const NODE: u32 = 1;

/// Where the vehicle believes it is, in world ENU metres — 1,200 m east and 800 m north
/// of the origin, the same point `v2xw-msg`'s own fixtures use, so a size measured here
/// and a size measured there are measuring the same message.
const POS: Vec3 = Vec3 {
    x: 1_200.0,
    y: 800.0,
    z: 12.5,
};

/// Believed ground speed, m/s — 50 km/h.
const SPEED_MPS: f64 = 13.89;

/// The world's geodetic anchor: the south-west corner of the Manhattan preset (D7).
///
/// Not the null-island default, because a test that encoded at `(0, 0)` would not notice
/// a latitude sign error, and both formats carry a signed latitude.
fn origin() -> GeoOrigin {
    GeoOrigin::new(40.7440, -73.9900, 0.0)
}

fn belief() -> PositionEstimate {
    let mut p = PositionEstimate::no_fix(0);
    p.pos = POS;
    p.vel = Vec3::new(SPEED_MPS, 0.0, 0.0);
    p.heading_rad = 0.0;
    p.semi_major_m = 5.0;
    p.semi_minor_m = 3.0;
    p.fix = FixQuality::ThreeD;
    p
}

/// One preloaded credential.
///
/// `digest` and `cert_coer` are the stand-ins a credential arrives with while no
/// `CredentialProtocol` ships; `ObuRuntime::generate` replaces both with the real
/// certificate's own identity before it signs anything, and
/// `the_station_identifier_is_the_credential_the_frame_was_signed_with` is what checks it
/// did.
fn credential(node: NodeId, j: u32) -> CredentialHandle {
    CredentialHandle {
        digest: pseudo_signer(node, j),
        cert_coer: vec![0u8; 120],
        key: v2xw_sec::KeyId(u64::from(j)),
        i_period: 100,
        j_index: j,
        valid_from: 0,
        valid_until: 10_000 * NS_PER_S,
        state: CredState::Active,
    }
}

fn reference_profile() -> HardwareProfile {
    v2xw_node::profiles::get(v2xw_node::profiles::REFERENCE_OBU)
        .expect("the reference profile ships with the crate")
        .clone()
}

/// A node on `profile` running `services` with `mode`'s crypto backend.
fn node_with(profile: HardwareProfile, services: ServiceSet, mode: CryptoMode) -> ObuRuntime {
    let node = NodeId::new(NODE);
    let config = NodeConfig {
        services,
        crypto_mode: mode,
        origin: origin(),
        ..NodeConfig::default()
    };
    let mut rt = ObuRuntime::new(node, profile, Box::new(VerifyAll::new()), config, 0);
    rt.stores_mut().crl.set_period(100);
    rt.stores_mut().certs.insert(credential(node, 0));
    rt.set_belief(belief());
    rt
}

/// The reference OBU on `services`, in the modelled backend.
///
/// The modelled backend is the default because every *size* in this file is identical in
/// both modes by construction (invariant I-S1), and the two tests that are about the
/// cryptography rather than the sizes ask for `CryptoMode::Real` explicitly.
fn node(services: ServiceSet) -> ObuRuntime {
    node_with(reference_profile(), services, CryptoMode::Modeled)
}

/// Runs `steps` ticks of `dt` with an empty inbox: this file is about what a node
/// transmits.
fn run(rt: &mut ObuRuntime, steps: u64, dt: SimTime) -> Vec<StepOutcome> {
    let reg = RngRegistry::new(19);
    let mut out = Vec::new();
    for k in 0..steps {
        let mut ctx = NodeRuntimeCtx::new(k * dt, &reg);
        out.push(rt.step(&mut ctx, Vec::new(), 0.0));
    }
    out
}

/// Every frame the run transmitted, in order.
fn transmissions(outcomes: &[StepOutcome]) -> Vec<Transmission> {
    outcomes
        .iter()
        .flat_map(|o| o.transmissions.iter().cloned())
        .collect()
}

fn first_of(txs: &[Transmission], t: MsgType) -> Transmission {
    txs.iter()
        .find(|x| x.msg_type == t)
        .unwrap_or_else(|| panic!("the node transmitted no {}", t.as_str()))
        .clone()
}

// =======================================================================================
// The day-one test
// =======================================================================================

/// **The test that would have caught the defect on day one.** A J2735 basic safety
/// message and an ETSI cooperative awareness message do not have the same wire size.
///
/// They are different standards carrying different field sets through different encoders.
/// The only way they can agree to the byte is if neither was encoded — which is exactly
/// what was happening when both came out at 101 bytes.
///
/// The assertion is on the *payload* as well as on the frame, because a shared envelope
/// could otherwise mask two identical payloads, and on `bytes` matching the SPDU's real
/// length, because a size carried beside the bytes is a size that can drift from them.
#[test]
fn a_safety_message_and_an_awareness_message_do_not_have_the_same_size() {
    let mut rt = node(ServiceSet::BOTH);
    let txs = transmissions(&run(&mut rt, 1, 100 * NS_PER_MS));
    assert_eq!(txs.len(), 2, "a dual-stack node sends one of each: {txs:?}");

    let bsm_tx = first_of(&txs, MsgType::Bsm);
    let cam_tx = first_of(&txs, MsgType::Cam);

    let bsm_frame = bsm_tx
        .signed
        .as_ref()
        .expect("a generated frame carries bytes");
    let cam_frame = cam_tx
        .signed
        .as_ref()
        .expect("a generated frame carries bytes");

    assert_ne!(
        bsm_frame.payload_bytes(),
        cam_frame.payload_bytes(),
        "a J2735 BSM and an ETSI CAM cannot encode to the same number of octets; \
         equal sizes mean neither was encoded (the 101-byte defect)"
    );
    assert_ne!(
        bsm_tx.bytes, cam_tx.bytes,
        "two different formats produced the same frame size"
    );

    // The count beside the bytes is the count *of* the bytes, in both directions.
    assert_eq!(bsm_tx.bytes, bsm_frame.bytes_on_wire());
    assert_eq!(cam_tx.bytes, cam_frame.bytes_on_wire());
    assert_eq!(
        bsm_frame.bytes_on_wire(),
        bsm_frame.payload_bytes() + bsm_frame.envelope_bytes(),
        "payload + envelope is the whole frame, or the split is not a split"
    );

    // Neither is empty, which is what the stub produced.
    assert!(bsm_frame.payload_bytes() > 0);
    assert!(cam_frame.payload_bytes() > 0);
}

// =======================================================================================
// The bytes are the format's bytes
// =======================================================================================

/// What the node put on the air decodes back through the real codecs, with the fields it
/// encoded still in it.
///
/// Two decoders, because there are two formats: the hand-written J2735 encoder that was
/// cross-validated against `pycrate` over 235 vectors, and the generated ETSI UPER
/// bindings. The CAM is checked by re-encoding what came back and comparing the octets,
/// which is a stronger statement than any field-by-field check a test could keep in step
/// with a generated type: every bit that went out came back meaning the same thing.
#[test]
fn the_bytes_a_node_produced_decode_back_through_the_real_codec() {
    let mut rt = node(ServiceSet::BOTH);
    let txs = transmissions(&run(&mut rt, 1, 100 * NS_PER_MS));

    let bsm_tx = first_of(&txs, MsgType::Bsm);
    let bsm_frame = bsm_tx.signed.as_ref().expect("bytes");
    let decoded = bsm::decode_message_frame(&bsm_frame.payload)
        .expect("the payload is a J2735 MessageFrame this codec wrote");

    let (lat_deg, lon_deg, _alt) = origin().to_geodetic(POS);
    assert_eq!(
        decoded.core.lat,
        bsm::latitude(lat_deg).expect("the fixture is inside the Latitude range"),
        "the latitude the node believed is the latitude on the air"
    );
    assert_eq!(
        decoded.core.lon,
        bsm::longitude(lon_deg).expect("the fixture is inside the Longitude range")
    );
    // Quantised through the same function the encoder used, from the same belief: the
    // claim is that the node's belief reached the wire, not that this test can reproduce
    // a 0.02 m/s rounding rule.
    assert_eq!(
        decoded.core.speed,
        bsm::speed(belief().ground_speed_mps()),
        "the speed the node believed is the speed on the air"
    );
    assert_ne!(
        decoded.core.speed,
        v2xw_msg::j2735::bsm::SPEED_UNAVAILABLE,
        "a speed that encoded as `unavailable` would make the assertion above vacuous"
    );
    assert!(
        decoded.part_ii.is_empty(),
        "this generator emits Part I only (04-models.md §8.1)"
    );

    let cam_tx = first_of(&txs, MsgType::Cam);
    let cam_frame = cam_tx.signed.as_ref().expect("bytes");
    let codec = EtsiUperCodec::new();
    let message = codec
        .decode(&cam_frame.payload, MsgType::Cam)
        .expect("the payload is a UPER CAM this codec wrote");
    assert_eq!(message.msg_type(), MsgType::Cam);
    let again = codec.encode(&message).expect("re-encodes");
    assert_eq!(
        again.bytes, cam_frame.payload,
        "a CAM that decodes and re-encodes to different octets lost a field on the way"
    );
    assert!(again.is_real(), "these are wire bytes, not a size model");
}

/// The four-octet station identifier on the air is derived from the credential the frame
/// was signed with, not from the node id.
///
/// That is the property a linkability study measures: the identifier changes exactly when
/// the pseudonym changes. Derived from the node id it would have made every pseudonym
/// change trivially reversible, and the study meaningless.
#[test]
fn the_station_identifier_is_the_credential_the_frame_was_signed_with() {
    let mut rt = node(ServiceSet::SAE);
    let txs = transmissions(&run(&mut rt, 1, 100 * NS_PER_MS));
    let tx = first_of(&txs, MsgType::Bsm);
    let frame = tx.signed.as_ref().expect("bytes");
    let decoded = bsm::decode_message_frame(&frame.payload).expect("decodes");

    let mut expected = [0u8; 4];
    expected.copy_from_slice(&tx.signer.0[..4]);
    assert_eq!(decoded.core.id, expected);

    // And the credential the store holds is the certificate that was actually signed
    // with, not the stand-in it was preloaded with.
    assert_ne!(
        tx.signer,
        pseudo_signer(NodeId::new(NODE), 0),
        "the store still names the stand-in digest, so a receiver would resolve a \
         certificate nobody signed with"
    );
}

// =======================================================================================
// The signature is a signature
// =======================================================================================

/// A signature this node produced verifies against the certificate the SPDU carries, and
/// a tampered payload does not.
///
/// Run in `CryptoMode::Real`, which is the only mode in which the negative half means
/// anything: the question is whether the curve arithmetic is being done over the bytes
/// that went out.
#[test]
fn a_signature_verifies_against_its_certificate_and_a_tampered_payload_does_not() {
    let mut rt = node_with(reference_profile(), ServiceSet::SAE, CryptoMode::Real);
    let txs = transmissions(&run(&mut rt, 1, 100 * NS_PER_MS));
    let frame = first_of(&txs, MsgType::Bsm)
        .signed
        .expect("a generated frame carries bytes");

    let reg = RngRegistry::new(23);
    let mut ctx = NodeRuntimeCtx::new(0, &reg);
    let owner = NodeId::new(NODE);

    let parsed = rt
        .security()
        .parse(&frame.spdu)
        .expect("the node's own SPDU parses under its own envelope profile");
    assert_eq!(
        parsed.payload, frame.payload,
        "the SPDU protects the payload the codec produced"
    );
    let certificate = NodeSecurity::attached_certificate(&parsed)
        .expect("the first SPDU from a signer always attaches its certificate");

    assert_eq!(
        rt.security_mut()
            .verify_parsed(&mut ctx, &parsed, &certificate, owner),
        SpduVerdict::Valid,
        "a node's own signature must check out against its own certificate"
    );

    // The negative control: one bit of the protected payload, flipped where it sits
    // inside the SPDU. The signature covers the payload, so this must not verify.
    let at = frame
        .spdu
        .windows(frame.payload.len())
        .position(|w| w == frame.payload.as_slice())
        .expect("the SPDU carries the payload verbatim");
    let mut tampered = frame.spdu.clone();
    tampered[at] ^= 0x01;
    assert_ne!(tampered, frame.spdu);

    let reparsed = rt.security().parse(&tampered);
    let verdict = match reparsed {
        Some(p) => rt
            .security_mut()
            .verify_parsed(&mut ctx, &p, &certificate, owner),
        // Bytes that no longer decode are a rejection too, and a stricter one.
        None => SpduVerdict::Invalid,
    };
    assert_ne!(
        verdict,
        SpduVerdict::Valid,
        "a tampered payload verified; the signature is not covering the payload"
    );
}

// =======================================================================================
// The envelope costs what the derivation says it costs
// =======================================================================================

/// The security envelope costs exactly the 93 bytes 04-models.md §9.1 derives, for a
/// digest signer under PSID 0x20 with a payload below 128 octets.
///
/// Not a tolerance: `crates/v2xw-sec/tests/overhead.rs` measured the encoder and found the
/// derivation confirmed line for line, so invariant I-S3 holds as equality. The three
/// conditions are all load-bearing and all met here — a Part I-only BSM `MessageFrame` is
/// 40 octets, so the inner `unsecuredData` length determinant is one byte; a PSID of 0x100
/// or more would cost a third COER byte and make it 94.
///
/// The *second* frame, because the first message a signer sends always attaches its whole
/// certificate: a receiver that has never seen it has nothing to resolve a digest against.
/// That first frame costs `87 + certificate` instead, and the difference between the two
/// is the certificate-attachment cadence made of real bytes.
#[test]
fn the_envelope_costs_the_ninety_three_bytes_the_derivation_predicts() {
    let mut rt = node(ServiceSet::SAE);
    let txs = transmissions(&run(&mut rt, 2, 100 * NS_PER_MS));
    assert_eq!(txs.len(), 2, "10 Hz over two ticks: {txs:?}");

    assert!(
        txs[0].full_certificate,
        "the first SPDU from a signer attaches its certificate"
    );
    assert!(
        !txs[1].full_certificate,
        "the SAE cadence is one certificate every 450 ms, so 100 ms later is a digest"
    );

    let digest_frame = txs[1].signed.as_ref().expect("bytes");
    assert!(
        digest_frame.payload_bytes() < 128,
        "the 93-byte figure assumes a one-octet length determinant; this payload is {} B",
        digest_frame.payload_bytes()
    );
    assert_eq!(
        txs[1].envelope_bytes(),
        Some(93),
        "04-models.md §9.1, measured in crates/v2xw-sec/tests/overhead.rs"
    );

    // The certificate frame is the bigger one, by the encoded certificate itself.
    assert!(
        txs[0].envelope_bytes() > txs[1].envelope_bytes(),
        "attaching a certificate must cost bytes, or the cadence is free"
    );
}

/// Both crypto backends agree on every size a node puts on the air — invariant I-S1 and
/// Phase 1 acceptance criterion 3, asserted at the level that matters: the frame.
#[test]
fn the_two_backends_produce_identically_sized_frames() {
    let sizes = |mode| {
        let mut rt = node_with(reference_profile(), ServiceSet::BOTH, mode);
        transmissions(&run(&mut rt, 6, 100 * NS_PER_MS))
            .iter()
            .map(|t| (t.msg_type, t.bytes, t.payload_bytes(), t.envelope_bytes()))
            .collect::<Vec<_>>()
    };
    assert_eq!(sizes(CryptoMode::Real), sizes(CryptoMode::Modeled));
}

// =======================================================================================
// A slow signer falls behind
// =======================================================================================

/// **The phenomenon, on the transmit side.** A node whose signer is slower than its own
/// message rate queues behind itself, and the lag between when a frame was generated and
/// when it could reach the MAC grows without bound.
///
/// The rates are the model's, not the test's. The reference profile publishes a `<9 ms`
/// signing latency on a single eHSM engine, which at the J2945/1 10 Hz cadence is 9 % of
/// the interval and never queues. The slow profile is the same profile with that one
/// published number replaced by 150 ms — longer than the 100 ms it has to produce a frame
/// in — so each signature starts 50 ms later than the last and the backlog is linear.
///
/// This is what makes the signing path a cost rather than a formality: with a service time
/// that was never charged, or charged against a fresh server each time, every frame would
/// be ready one service time after it was generated however slow the signer was.
#[test]
fn a_slow_signer_queues_behind_its_own_throughput() {
    // Ten ticks at the J2945/1 cadence.
    const TICKS: u64 = 10;
    const DT: SimTime = 100 * NS_PER_MS;

    let lags = |profile: HardwareProfile| -> Vec<u64> {
        let mut rt = node_with(profile, ServiceSet::SAE, CryptoMode::Modeled);
        transmissions(&run(&mut rt, TICKS, DT))
            .iter()
            .map(|t| t.ready_at.saturating_sub(t.generation_time))
            .collect()
    };
    // The modelled service time, read out of the profile rather than restated here: the
    // published figure is "<9 ms", and a test that hard-coded 9,000,000 ns would be
    // asserting its own arithmetic about a nanosecond conversion rather than the
    // queueing behaviour it is named after.
    let sign_cost = |profile: &HardwareProfile| -> u64 {
        profile
            .op_cost("ecdsa-p256-sign")
            .expect("the profile costs a signature")
            .0
            .as_nanos()
    };

    // The published part: one signature well inside the 100 ms budget never queues, so
    // every frame is ready exactly one service time after it was generated.
    let fast_profile = reference_profile();
    let fast_cost = sign_cost(&fast_profile);
    assert!(
        fast_cost < DT,
        "this half of the test assumes the reference signer keeps up: {fast_cost} ns"
    );
    let fast = lags(fast_profile);
    assert_eq!(fast.len(), TICKS as usize);
    assert!(
        fast.iter().all(|l| *l == fast_cost),
        "a signer inside its budget is ready one service time later, every time: {fast:?}"
    );

    // The same profile with the one published number replaced by a signer slower than the
    // message rate. Nothing else changes.
    let mut slow_profile = reference_profile();
    slow_profile
        .hsm
        .ops
        .get_mut("ecdsa-p256-sign")
        .expect("the reference profile costs a signature")
        .latency_us
        .value = Some(150_000.0);
    let slow_cost = sign_cost(&slow_profile);
    assert!(
        slow_cost > DT,
        "this half of the test assumes the signer cannot keep up: {slow_cost} ns"
    );
    let slow = lags(slow_profile);
    assert_eq!(slow.len(), TICKS as usize);

    // One server, `slow_cost` of work offered every `DT`: the k-th signature cannot start
    // until the (k-1)-th finished, so it finishes at `(k + 1) * slow_cost` while it was
    // generated at `k * DT`. The backlog is linear in k and unbounded in the run.
    for (k, lag) in slow.iter().enumerate() {
        let k = k as u64;
        let expected = (k + 1) * slow_cost - k * DT;
        assert_eq!(
            *lag, expected,
            "frame {k} should be ready {expected} ns after it was generated: {slow:?}"
        );
    }
    assert!(
        slow.last() > slow.first(),
        "a slow signer that does not fall behind is a signer nobody charged"
    );
}
