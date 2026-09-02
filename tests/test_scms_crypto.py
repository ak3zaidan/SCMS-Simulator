"""Real ECDSA, real certificates, real signed messages — and their measured cost.

Covers requirements 2 and 3 of the security task: signing/verification over message bytes, and a
pseudonym certificate that carries the IEEE 1609.2 `ToBeSignedCertificate` fields the standards
audit found missing, with the linkage value INSIDE the certificate.
"""

from __future__ import annotations

import time

import pytest

from scms_sim_ref.scms_core import ec
from scms_sim_ref.scms_core.certificate import (PSID_CA_BASIC_SERVICE, PSID_DEN_BASIC_SERVICE,
                                                Duration, ExplicitCertificate, LinkageData, PsidSsp,
                                                ToBeSignedCertificate, ValidityPeriod, hashed_id3,
                                                hashed_id8)
from scms_sim_ref.scms_core.ecdsa_p256 import (DETERMINISTIC, SIGNATURE_BYTES, SIGNING_MODE,
                                               SigningKey, VerificationCache, VerifyingKey,
                                               compress_point, decompress_point)
from scms_sim_ref.scms_core.linkage import CrlLinkageEntry, DeviceLinkageContext
from scms_sim_ref.scms_core.provisioning import ScmsProvisioning, seed_derive
from scms_sim_ref.scms_core import secured as S


# ============================================================== the group, still the group ====== #

def test_fast_base_mult_matches_reference():
    """`ec.scalar_mult` was accelerated for the generator. The reference loop is the definition of
    correctness, so the two must agree on every scalar that matters — including the edges."""
    for k in (1, 2, 3, 7, 12345, 2 ** 128 + 1, ec.N - 1, ec.N - 2):
        assert ec.scalar_mult(k) == ec.scalar_mult_ref(k), f"mismatch at k={k}"
    assert ec.scalar_mult(0) is None and ec.scalar_mult(ec.N) is None
    # a non-generator base still takes the transparent path and still agrees
    p2 = ec.scalar_mult(9)
    assert ec.scalar_mult(5, p2) == ec.scalar_mult_ref(5, p2)


def test_point_compression_round_trips():
    for k in (1, 2, 3, 99991, ec.N - 1):
        pt = ec.scalar_mult(k)
        blob = compress_point(pt)
        assert len(blob) == 33 and blob[0] in (2, 3)
        assert decompress_point(blob) == pt
    with pytest.raises(ValueError):
        decompress_point(b"\x02" + b"\xff" * 32)          # x with no square root on the curve


# ============================================================== signing is real ================= #

def test_sign_verify_round_trip_and_tamper():
    key = SigningKey(0xC0FFEE)
    pub = key.public_key()
    sig = key.sign(b"a CAM, encoded")
    assert len(sig) == SIGNATURE_BYTES
    assert pub.verify(b"a CAM, encoded", sig) is True
    assert pub.verify(b"a CAM, altered", sig) is False
    assert pub.verify(b"a CAM, encoded", bytes(64)) is False
    assert SigningKey(0xBADF00D).public_key().verify(b"a CAM, encoded", sig) is False


def test_signing_is_deterministic_rfc6979():
    """The digest contract: same key + same bytes => same signature, across objects and calls."""
    if not DETERMINISTIC:                                   # pragma: no cover - backend dependent
        pytest.skip(f"backend signing mode is {SIGNING_MODE!r}, not rfc6979")
    assert SIGNING_MODE == "rfc6979"
    a, b = SigningKey(12345), SigningKey(12345)
    assert a.sign(b"payload") == b.sign(b"payload") == a.sign(b"payload")


def test_public_key_round_trips_through_the_curve_point():
    key = SigningKey(777)
    again = VerifyingKey.from_compressed(key.public_key().compressed())
    assert again.point == key.public_point
    assert again.verify(b"m", key.sign(b"m"))


# ============================================================== certificates are certificates === #

def _tbs(**kw):
    base = dict(
        id=LinkageData(i_cert=3, linkage_value=bytes(range(9))),
        craca_id=hashed_id3(b"CRACA-1"),
        crl_series=7,
        validity_period=ValidityPeriod(1000, Duration("seconds", 600)),
        verify_key_indicator=compress_point(ec.scalar_mult(4242)),
        app_permissions=(PsidSsp(PSID_CA_BASIC_SERVICE),),
        assurance_level=0xC0,
    )
    base.update(kw)
    return ToBeSignedCertificate(**base)


def test_tobesigned_certificate_carries_every_field_the_audit_missed():
    tbs = _tbs()
    assert tbs.id.linkage_value == bytes(range(9))          # linkage INSIDE the certificate
    assert tbs.id.i_cert == 3
    assert len(tbs.craca_id) == 3 and tbs.crl_series == 7
    assert tbs.validity_period.start == 1000 and tbs.validity_period.end == 1600
    assert tbs.permits(PSID_CA_BASIC_SERVICE) and not tbs.permits(PSID_DEN_BASIC_SERVICE)
    assert tbs.verification_key().point == ec.scalar_mult(4242)


def test_certificate_signature_binds_every_field():
    """Change any signed field and the issuer's signature must fail — otherwise the fields are
    decoration rather than a certificate."""
    ca = SigningKey(0xCA)
    issuer = hashed_id8(b"issuer")
    tbs = _tbs()
    unsigned = ExplicitCertificate(tbs, issuer, bytes(64))
    cert = ExplicitCertificate(tbs, issuer, ca.sign(unsigned.signed_octets()))
    assert cert.verify_signature(ca.public_key())
    mutations = [
        _tbs(crl_series=8),
        _tbs(validity_period=ValidityPeriod(1000, Duration("seconds", 601))),
        _tbs(app_permissions=(PsidSsp(PSID_DEN_BASIC_SERVICE),)),
        _tbs(id=LinkageData(i_cert=3, linkage_value=bytes(range(1, 10)))),
        _tbs(id=LinkageData(i_cert=4, linkage_value=bytes(range(9)))),
        _tbs(craca_id=hashed_id3(b"CRACA-2")),
        _tbs(assurance_level=0x20),
        _tbs(verify_key_indicator=compress_point(ec.scalar_mult(4243))),
    ]
    for m in mutations:
        forged = ExplicitCertificate(m, issuer, cert.signature)
        assert not forged.verify_signature(ca.public_key()), f"unbound field in {m}"
    # and the issuer itself is signed over
    assert not ExplicitCertificate(tbs, hashed_id8(b"other"),
                                   cert.signature).verify_signature(ca.public_key())


def test_hashed_id8_is_eight_bytes_and_changes_with_content():
    ca = SigningKey(0xCA)
    issuer = hashed_id8(b"issuer")

    def mk(tbs):
        u = ExplicitCertificate(tbs, issuer, bytes(64))
        return ExplicitCertificate(tbs, issuer, ca.sign(u.signed_octets()))

    a, b = mk(_tbs()), mk(_tbs(crl_series=8))
    assert len(a.hashed_id8()) == 8 and len(a.digest_hex()) == 16
    assert a.digest_hex() != b.digest_hex()


def test_duration_is_a_1609dot2_choice_not_a_float():
    assert Duration.from_seconds(600) == Duration("seconds", 600)
    assert Duration.from_seconds(3600) == Duration("seconds", 3600)   # finest unit that fits
    assert Duration.from_seconds(300_000) == Duration("minutes", 5000)  # seconds overflows Uint16
    assert Duration.from_seconds(100_000).seconds >= 100_000  # rounds UP, never short of the window
    with pytest.raises(ValueError):
        Duration("fortnights", 1)


# ============================================================== provisioning end to end ========= #

@pytest.fixture(scope="module")
def prov():
    return ScmsProvisioning(seed_derive(42))


def _ctx(vid: int, derive):
    return DeviceLinkageContext(0x0001, 0x0002, derive(f"ls1:{vid}", 16), derive(f"ls2:{vid}", 16))


def test_chain_root_signs_pca_signs_pseudonym(prov):
    assert prov.root.certificate.verify_signature(prov.root.verifying_key())      # self-signed
    assert prov.pca_authority.certificate.issuer == prov.root.hashed_id8
    assert prov.pca_authority.certificate.verify_signature(prov.root.verifying_key())
    dev = prov.provision(device_label="veh_001", linkage_ctx=_ctx(1, seed_derive(42)),
                         windows=[(0, 1, 0.0, 60.0)])
    cert = dev.credentials[0].certificate
    assert cert.issuer == prov.pca_authority.hashed_id8
    assert cert.verify_signature(prov.pca_authority.verifying_key())


def test_butterfly_derived_key_is_the_certified_key_and_actually_signs(prov):
    """THE end-to-end property: the device re-derives `a + f1(ck,i,j) + c`, and that key verifies
    against the public key the PCA put in the certificate. Signing proves it, not arithmetic."""
    dev = prov.provision(device_label="veh_007", linkage_ctx=_ctx(7, seed_derive(42)),
                         windows=[(i, j, 60.0 * i, 60.0 * i + 60.0)
                                  for i, j in ((0, 7), (1, 8), (2, 9))])
    for cred in dev.credentials:
        msg = S.MessageSigner(cred).sign(b"cam-bytes", S.time64(10.0))
        assert cred.certificate.verification_key().verify(
            S.signature_input(msg.tbs, cred.certificate), msg.signature)


def test_linkage_value_in_the_certificate_matches_the_devices_own(prov):
    """The linkage value the PCA embedded must be the one the device's two LA seeds produce —
    otherwise revocation would silently never match."""
    d = seed_derive(42)
    ctx = _ctx(11, d)
    dev = prov.provision(device_label="veh_011", linkage_ctx=ctx,
                         windows=[(0, 3, 0.0, 60.0), (1, 4, 60.0, 120.0)])
    for cred in dev.credentials:
        assert cred.certificate.linkage.linkage_value == ctx.linkage_value_for(cred.i_period,
                                                                              cred.j_index)


def test_every_pseudonym_of_a_device_has_a_distinct_key_and_digest(prov):
    dev = prov.provision(device_label="veh_021", linkage_ctx=_ctx(21, seed_derive(42)),
                         windows=[(0, j, 60.0 * j, 60.0 * j + 60.0) for j in range(8)])
    keys = {c.certificate.to_be_signed.verify_key_indicator for c in dev.credentials}
    assert len(keys) == 8 == len({c.digest for c in dev.credentials})


def test_provisioning_is_deterministic(prov):
    a = ScmsProvisioning(seed_derive(99))
    b = ScmsProvisioning(seed_derive(99))
    w = [(0, 2, 0.0, 60.0), (1, 3, 60.0, 120.0)]
    da = a.provision(device_label="veh_002", linkage_ctx=_ctx(2, seed_derive(99)), windows=w)
    db = b.provision(device_label="veh_002", linkage_ctx=_ctx(2, seed_derive(99)), windows=w)
    assert da.digests == db.digests
    assert [c.signing_key.scalar for c in da.credentials] == [c.signing_key.scalar
                                                              for c in db.credentials]


def test_a_different_seed_gives_different_credentials():
    w = [(0, 2, 0.0, 60.0)]
    a = ScmsProvisioning(seed_derive(1)).provision(
        device_label="v", linkage_ctx=_ctx(2, seed_derive(1)), windows=w)
    b = ScmsProvisioning(seed_derive(2)).provision(
        device_label="v", linkage_ctx=_ctx(2, seed_derive(2)), windows=w)
    assert a.digests != b.digests


# ============================================================== the receive path ================ #

@pytest.fixture(scope="module")
def fleet(prov):
    d = seed_derive(42)
    out = {}
    for vid in range(4):
        out[f"veh_{vid:03d}"] = prov.provision(
            device_label=f"veh_{vid:03d}", linkage_ctx=_ctx(vid, d),
            windows=[(0, vid, 0.0, 60.0), (1, vid + 1, 60.0, 120.0)])
    return out


def _verifier(prov, **kw):
    return S.MessageVerifier(prov.trust_store(), **kw)


def test_an_honest_message_is_accepted(prov, fleet):
    cred = fleet["veh_000"].credentials[0]
    msg = S.MessageSigner(cred).sign(b"cam", S.time64(30.0))
    r = _verifier(prov).verify(msg, now=30)
    assert (r.status, r.signature_valid, r.accepted) == (S.OK, True, True)


def test_sig_ok_is_now_a_result_not_a_flag(prov, fleet):
    """Requirement 2, restated as a test: `InvalidSignature` becomes a real signature by the wrong
    key, and the receiver's False comes out of ECDSA."""
    cred = fleet["veh_000"].credentials[0]
    other = fleet["veh_001"].credentials[0].signing_key
    signer = S.MessageSigner(cred)
    bad = S.sign_with_foreign_key(signer, other, b"cam", S.time64(30.0))
    r = _verifier(prov).verify(bad, now=30)
    assert r.status == S.SIGNATURE_INVALID
    assert r.signature_valid is False and r.certificate_valid is True and r.accepted is False
    assert r.sig_ok is False


def test_signature_validity_is_tri_state_and_sig_ok_is_the_honest_projection(prov, fleet):
    """A frame dropped on its certificate never had its signature checked. Saying so is the point:
    `sig_ok` must not report a crypto failure the receiver never observed, and must not report a
    crypto success either."""
    cred = fleet["veh_000"].credentials[0]                    # valid [0, 60]
    stale = S.MessageSigner(cred).sign(b"cam", S.time64(30.0))
    r = _verifier(prov).verify(stale, now=200)
    assert r.status == S.CERT_EXPIRED
    assert r.signature_valid is None                          # not evaluated
    assert r.sig_ok is True                                   # so signatureVerification must NOT fire
    assert r.certificate_valid is False and r.accepted is False
    # ... and `full=True` buys the answer back, at the cost of the ECDSA the receiver skipped
    rf = _verifier(prov).verify(stale, now=200, full=True)
    assert rf.status == S.CERT_EXPIRED and rf.signature_valid is True
    # a message that is BOTH expired and badly signed reports both under `full`
    forged = S.sign_with_foreign_key(S.MessageSigner(cred), SigningKey(0xABCD), b"cam",
                                     S.time64(30.0))
    rb = _verifier(prov).verify(forged, now=200, full=True)
    assert rb.status == S.CERT_EXPIRED and rb.signature_valid is False and rb.sig_ok is False


def test_tampering_after_signing_is_caught(prov, fleet):
    signer = S.MessageSigner(fleet["veh_000"].credentials[0])
    good = signer.sign(b"claimed x=10", S.time64(30.0))
    assert _verifier(prov).verify(good, now=30).accepted
    bad = S.tamper_after_signing(good, b"claimed x=99")
    assert _verifier(prov).verify(bad, now=30).status == S.SIGNATURE_INVALID


def test_verbatim_replay_still_verifies_and_that_is_correct(prov, fleet):
    """A replay is a freshness failure, not a crypto failure. If this ever started returning
    SIGNATURE_INVALID, `staleOrReplay` would be dead code and the attack would be mislabelled."""
    signer = S.MessageSigner(fleet["veh_000"].credentials[0])
    old = signer.sign(b"cam", S.time64(10.0))
    r = _verifier(prov).verify(S.replay(old), now=30)
    assert r.status == S.OK and r.signature_valid
    assert old.generation_time == S.time64(10.0)                 # the staleness is on the wire
    # ... but rewriting the timestamp without re-signing does fail
    forged = S.replay(old, at_generation_time=S.time64(30.0))
    assert _verifier(prov).verify(forged, now=30).status == S.SIGNATURE_INVALID


def test_certificate_grafting_fails_because_of_the_1609dot2_double_hash(prov, fleet):
    signer = S.MessageSigner(fleet["veh_000"].credentials[0])
    msg = signer.sign(b"cam", S.time64(30.0))
    victim = fleet["veh_001"].credentials[0].certificate
    assert _verifier(prov).verify(S.graft_certificate(msg, victim), now=30).status == S.SIGNATURE_INVALID


def test_a_self_issued_certificate_is_an_unknown_issuer_not_a_bad_signature(prov, fleet):
    """The outcome the boolean could never express. The rogue certificate is internally perfect."""
    cred = fleet["veh_000"].credentials[0]
    rogue_key = SigningKey(0xDEADBEEF)
    rogue = S.self_issued_certificate(cred.certificate, rogue_key)
    assert rogue.verify_signature(rogue_key.public_key())        # internally consistent
    msg = S.MessageSigner.from_parts(rogue, rogue_key).sign(b"cam", S.time64(30.0))
    assert rogue.verification_key().verify(                      # the message signature is fine
        S.signature_input(msg.tbs, rogue), msg.signature) is True # -- only the ISSUER is unknown
    assert _verifier(prov).verify(msg, now=30).status == S.UNKNOWN_ISSUER


def test_validity_window_is_enforced_from_inside_the_certificate(prov, fleet):
    cred = fleet["veh_000"].credentials[0]                        # valid [0, 60]
    msg = S.MessageSigner(cred).sign(b"cam", S.time64(30.0))
    v = _verifier(prov)
    assert v.verify(msg, now=30).status == S.OK
    assert v.verify(msg, now=120).status == S.CERT_EXPIRED
    later = fleet["veh_000"].credentials[1]                       # valid [60, 120]
    m2 = S.MessageSigner(later).sign(b"cam", S.time64(10.0))
    assert v.verify(m2, now=10).status == S.CERT_NOT_YET_VALID


def test_app_permissions_are_enforced(prov, fleet):
    """A pseudonym certificate provisioned for CAM only may not sign a DENM. There is no field to
    check this against today, so the attack has no expression at all."""
    cred = fleet["veh_000"].credentials[0]
    denm = S.MessageSigner(cred).sign(b"denm", S.time64(30.0), psid=PSID_DEN_BASIC_SERVICE)
    assert _verifier(prov).verify(denm, now=30).status == S.PSID_NOT_PERMITTED


def test_a_revoked_certificate_is_rejected_by_the_receiver(prov):
    """Revocation becomes receiver-enforceable because the linkage value is IN the certificate.
    Today the CRL is only consumed centrally (`enforced()` in run.py), never by a verifier."""
    d = seed_derive(42)
    ctx = _ctx(31, d)
    dev = prov.provision(device_label="veh_031", linkage_ctx=ctx,
                         windows=[(0, 5, 0.0, 60.0), (1, 6, 60.0, 120.0)])
    entry = CrlLinkageEntry.from_device(ctx, i=0, jmax=20)
    v = _verifier(prov, crl=[entry])
    for cred in dev.credentials:
        msg = S.MessageSigner(cred).sign(b"cam", S.time64(cred.valid_from + 1.0))
        assert v.verify(msg, now=int(cred.valid_from) + 1).status == S.CERT_REVOKED
    # a different device on the same CRL is untouched
    other = prov.provision(device_label="veh_032", linkage_ctx=_ctx(32, d),
                           windows=[(0, 5, 0.0, 60.0)])
    m = S.MessageSigner(other.credentials[0]).sign(b"cam", S.time64(1.0))
    assert v.verify(m, now=1).status == S.OK


def test_revocation_index_agrees_with_crl_contains(prov):
    """The verifier's incremental linkage index must decide exactly what `crl_contains` decides —
    it is an optimisation of `CrlLinkageEntry.matches`, and an optimisation that disagrees with
    the reference is a security hole, not a speedup."""
    from scms_sim_ref.scms_core.linkage import crl_contains
    d = seed_derive(42)
    ctxs = {vid: _ctx(vid, d) for vid in (61, 62, 63)}
    devs = {vid: prov.provision(device_label=f"veh_{vid:03d}", linkage_ctx=c,
                                windows=[(i, (vid + i) % 20, 60.0 * i, 60.0 * (i + 1))
                                         for i in range(3)])
            for vid, c in ctxs.items()}
    crl = [CrlLinkageEntry.from_device(ctxs[61], i=0, jmax=20),
           CrlLinkageEntry.from_device(ctxs[62], i=2, jmax=20)]
    v = _verifier(prov, crl=crl)
    for vid, dev in devs.items():
        for cred in dev.credentials:
            cert = cred.certificate
            reference = crl_contains(crl, cert.linkage.i_cert, cred.j_index,
                                     cert.linkage.linkage_value)
            assert v.revoked(cert) is reference, (vid, cred.i_period, cred.j_index)


def test_forward_only_revocation_survives_the_certificate_round_trip(prov):
    """CAMP SCP2's backward privacy, now checked through the certificate: revoking from period 1
    must not match a period-0 certificate."""
    d = seed_derive(42)
    ctx = _ctx(41, d)
    dev = prov.provision(device_label="veh_041", linkage_ctx=ctx,
                         windows=[(0, 2, 0.0, 60.0), (1, 3, 60.0, 120.0)])
    v = _verifier(prov, crl=[CrlLinkageEntry.from_device(ctx, i=1, jmax=20)])
    assert not v.revoked(dev.credentials[0].certificate)          # period 0: still private
    assert v.revoked(dev.credentials[1].certificate)              # period 1: revoked


def test_signer_alternation_changes_the_wire_size(prov, fleet):
    """TS 103 097 signer alternation is worth ~150-200 B and both engines hard-code 300 B, which
    makes every CBR estimate systematically wrong (PLUGIN-ARCHITECTURE.md 2.5)."""
    signer = S.MessageSigner(fleet["veh_000"].credentials[0])
    with_cert = signer.sign(b"cam" * 10, S.time64(30.0), include_certificate=True)
    digest_only = signer.sign(b"cam" * 10, S.time64(30.0), include_certificate=False)
    delta = len(with_cert.wire_octets()) - len(digest_only.wire_octets())
    assert 100 < delta < 300, delta
    # and a digest-signed message still verifies once the receiver has learned the certificate
    v = _verifier(prov)
    assert v.verify(digest_only, now=30).status == S.CERT_UNAVAILABLE
    v.learn(signer.certificate)
    assert v.verify(digest_only, now=30).status == S.OK


def test_the_whole_signature_attack_surface_maps_onto_real_operations(prov, fleet):
    """Requirement 4, as one table. Every signature-related attack in `run.py`'s catalog, what the
    attacker now does, and what the receiver concludes. Nothing here sets a flag.

    Read the three `OK` rows carefully — they are the point. A stale timestamp, a burst and a
    fabricated ghost identity all carry **valid** signatures, because the attacker really does hold
    the key. Real crypto does not make those attacks disappear; it moves them to the detectors that
    were always the right ones (`staleOrReplay`, `beaconFrequency`, `sybilCoLocation`).
    """
    v0, v1 = fleet["veh_000"].credentials[0], fleet["veh_001"].credentials[0]
    signer = S.MessageSigner(v0)
    honest = signer.sign(b"cam", S.time64(30.0))
    rogue_key = SigningKey(0xF0F0)

    cases = [
        # (attack, message, now, expected status)
        ("honest baseline", honest, 30, S.OK),
        ("InvalidSignature", S.sign_with_foreign_key(signer, v1.signing_key, b"cam",
                                                     S.time64(30.0)), 30, S.SIGNATURE_INVALID),
        ("MessageTampering", S.tamper_after_signing(honest, b"cam'"), 30, S.SIGNATURE_INVALID),
        ("CertificateGrafting", S.graft_certificate(honest, v1.certificate), 30,
         S.SIGNATURE_INVALID),
        ("ForgedCertificate",
         S.MessageSigner.from_parts(S.self_issued_certificate(v0.certificate, rogue_key),
                                    rogue_key).sign(b"cam", S.time64(30.0)), 30, S.UNKNOWN_ISSUER),
        ("ExpiredCert (real reuse)", honest, 200, S.CERT_EXPIRED),
        ("NotYetValid (real reuse)",
         S.MessageSigner(fleet["veh_000"].credentials[1]).sign(b"cam", S.time64(5.0)), 5,
         S.CERT_NOT_YET_VALID),
        ("DENM under a CAM-only cert",
         signer.sign(b"denm", S.time64(30.0), psid=PSID_DEN_BASIC_SERVICE), 30,
         S.PSID_NOT_PERMITTED),
        # ---- these keep a VALID signature, and that is the correct model -------------------- #
        ("DataReplay (verbatim)", S.replay(signer.sign(b"cam", S.time64(10.0))), 30, S.OK),
        ("DelayedMessages / OutOfOrder", signer.sign(b"cam", S.time64(20.0)), 30, S.OK),
        ("DoS burst frame", signer.sign(b"cam", S.time64(30.0)), 30, S.OK),
        ("Sybil ghost (own second cert)",
         S.MessageSigner(fleet["veh_002"].credentials[0]).sign(b"cam", S.time64(30.0)), 30, S.OK),
    ]
    seen = {}
    for name, msg, now, expected in cases:
        got = _verifier(prov).verify(msg, now=now)
        assert got.status == expected, f"{name}: {got.status} != {expected}"
        seen[name] = got.status
    # the replayed frame is stale on the wire even though it verifies -- the detector's input
    assert cases[8][1].generation_time < S.time64(30.0)
    assert set(seen.values()) == {S.OK, S.SIGNATURE_INVALID, S.UNKNOWN_ISSUER, S.CERT_EXPIRED,
                                  S.CERT_NOT_YET_VALID, S.PSID_NOT_PERMITTED}   # six, not two


def test_a_revoked_attacker_cannot_keep_transmitting(prov):
    """The CRL becomes receiver-enforceable mid-run: adding an entry must flip the verdict, and the
    revocation memo must not hold the stale answer."""
    d = seed_derive(42)
    ctx = _ctx(51, d)
    dev = prov.provision(device_label="veh_051", linkage_ctx=ctx, windows=[(0, 9, 0.0, 60.0)])
    crl = []
    v = _verifier(prov, crl=crl)
    msg = S.MessageSigner(dev.credentials[0]).sign(b"cam", S.time64(10.0))
    assert v.verify(msg, now=10).status == S.OK
    crl.append(CrlLinkageEntry.from_device(ctx, i=0, jmax=20))
    assert v.verify(msg, now=10).status == S.CERT_REVOKED


# ============================================================== cost, measured ================== #

def test_verification_cache_is_exact_and_collapses_the_fan_out(prov, fleet):
    """One CAM, many receivers. The cache must return identical answers and compute once."""
    signer = S.MessageSigner(fleet["veh_000"].credentials[0])
    good = signer.sign(b"cam", S.time64(30.0))
    bad = S.tamper_after_signing(good, b"cam!")
    v = _verifier(prov)
    for _ in range(20):
        assert v.verify(good, now=30).accepted
        assert not v.verify(bad, now=30).accepted
    st = v.stats()
    assert st["logical_verifications"] == 40 and st["computed_verifications"] == 2
    assert st["certificates_chain_verified"] == 1


@pytest.mark.parametrize("n", [400])
def test_measured_throughput(n):
    """Requirement 2's measurement. Asserts an ORDER OF MAGNITUDE, never a pinned rate: a pinned
    number on a shared CI box is a flaky test, and the decision this informs ("can a 1188-vehicle
    1 Hz run afford real crypto?") only needs the order."""
    key = SigningKey(0x5EED)
    pub = key.public_key()
    msg = b"x" * 120

    t0 = time.perf_counter()
    sigs = [key.sign(msg + bytes([i % 251])) for i in range(n)]
    sign_rate = n / (time.perf_counter() - t0)

    t0 = time.perf_counter()
    for i, s in enumerate(sigs):
        assert pub.verify(msg + bytes([i % 251]), s)
    verify_rate = n / (time.perf_counter() - t0)

    print(f"\nsign={sign_rate:,.0f}/s verify={verify_rate:,.0f}/s mode={SIGNING_MODE}")
    assert sign_rate > 5_000, sign_rate
    assert verify_rate > 3_000, verify_rate


def test_provisioning_cost_is_affordable_before_step_zero():
    """A fleet's worth of butterfly provisioning must fit in the run's startup, not dominate it."""
    d = seed_derive(4242)
    p = ScmsProvisioning(d)
    t0 = time.perf_counter()
    total = 0
    for vid in range(20):
        dev = p.provision(device_label=f"veh_{vid:03d}", linkage_ctx=_ctx(vid, d),
                          windows=[(k, vid % 20, 60.0 * k, 60.0 * k + 60.0) for k in range(5)])
        total += len(dev.credentials)
    per = (time.perf_counter() - t0) / total
    print(f"\nprovisioning {per * 1e6:,.0f} us/certificate "
          f"=> {per * 1188 * 5:.2f} s for a 1188-device fleet at 5 rotations")
    assert per < 5e-3, per          # 5 ms/cert would be 30 s of fleet setup; we expect ~0.2 ms
