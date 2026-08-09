"""Tests for the EC group ops and butterfly-key expansion engine."""

from cryptography.hazmat.primitives.asymmetric import ec as cec

from scms_sim_ref.scms_core import butterfly as bf
from scms_sim_ref.scms_core import ec


def _lib_pub(d: int):
    """Reference public point for scalar d, via the cryptography library."""
    k = cec.derive_private_key(d, cec.SECP256R1())
    nums = k.public_key().public_numbers()
    return (nums.x, nums.y)


def test_ec_generator_on_curve():
    assert ec.is_on_curve(ec.G)
    assert ec.is_on_curve(None)  # infinity


def test_scalar_mult_matches_library():
    for d in (1, 2, 3, 7, 12345, 2**128 + 1, ec.N - 1):
        assert ec.scalar_mult(d) == _lib_pub(d), f"mismatch at d={d}"


def test_ec_homomorphism():
    d1, d2 = 111111, 987654321
    lhs = ec.add(ec.scalar_mult(d1), ec.scalar_mult(d2))
    rhs = ec.scalar_mult((d1 + d2) % ec.N)
    assert lhs == rhs == _lib_pub((d1 + d2) % ec.N)


def test_caterpillar_deterministic():
    seed = bytes(range(64))
    c1, c2 = bf.new_caterpillar(seed), bf.new_caterpillar(seed)
    assert (c1.a, c1.p, c1.ck, c1.ek) == (c2.a, c2.p, c2.ck, c2.ek)


def test_expansion_values_vary_and_are_deterministic():
    ck = bytes(range(16))
    vals = {bf.f1(ck, i, j) for i in range(3) for j in range(5)}
    assert len(vals) == 15                      # distinct per (i, j)
    assert bf.f1(ck, 1, 2) == bf.f1(ck, 1, 2)   # deterministic


def test_butterfly_signing_key_identity():
    """The heart of butterfly: device-derived private key matches the PCA-certified key."""
    cat = bf.new_caterpillar(bytes(range(64)))
    A, P = cat.A, cat.P
    for i in range(2):
        for j in range(4):
            B, Q = bf.ra_cocoon_keys(A, P, cat.ck, cat.ek, i, j)
            # cocoon keys must differ from the caterpillar (RA actually expanded them)
            assert B != A and Q != P
            c = 0x1234567890ABCDEF + i * 7 + j            # PCA's secret randomizer
            certified_pub, _C = bf.pca_certify_explicit(B, c)
            # device re-derives the usable signing key and it must match the certified key
            d_priv = bf.device_signing_private(cat, i, j, c)
            assert ec.scalar_mult(d_priv) == certified_pub
            # and the cocoon encryption key is recoverable too
            q_priv = bf.device_encryption_private(cat, i, j)
            assert ec.scalar_mult(q_priv) == Q


def test_ra_cannot_predict_certified_key_without_c():
    """Without the PCA's secret c, the cocoon key B != the certified key B + cG."""
    cat = bf.new_caterpillar(bytes([9] * 64))
    B, _Q = bf.ra_cocoon_keys(cat.A, cat.P, cat.ck, cat.ek, 0, 0)
    certified_pub, _C = bf.pca_certify_explicit(B, 424242)
    assert B != certified_pub
