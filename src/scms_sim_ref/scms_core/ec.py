"""Minimal NIST P-256 (secp256r1) group arithmetic for the butterfly-key engine.

Self-contained affine EC point ops (add / double / scalar-multiply). Kept tiny and
transparent on purpose; correctness is cross-validated against the `cryptography`
library in the tests (scalar_mult(d) must equal the library's public point for d).
"""

from __future__ import annotations

# secp256r1 domain parameters (FIPS 186-4 / SEC 2).
P = 0xFFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF
A = (P - 3) % P
B = 0x5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B
GX = 0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296
GY = 0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5
N = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551

Point = "tuple[int, int] | None"  # None == point at infinity
G: "tuple[int, int]" = (GX, GY)


def _inv(x: int) -> int:
    """Modular inverse mod the field prime P.

    `pow(x, -1, P)` (extended Euclid, CPython 3.8+) rather than `pow(x, P-2, P)` (Fermat): P is
    prime so the two are equal for every invertible x, and measured on this host the exponentiation
    costs **66.9 us** against **8.4 us**. That single line is ~8x of the affine point arithmetic,
    which is why butterfly provisioning is affordable enough to wire in at all. Zero keeps the
    Fermat form's behaviour (0, not a raise) so no caller's error path changes.
    """
    x %= P
    return 0 if x == 0 else pow(x, -1, P)


def is_on_curve(pt) -> bool:
    if pt is None:
        return True
    x, y = pt
    return (y * y - (x * x * x + A * x + B)) % P == 0


def add(p1, p2):
    if p1 is None:
        return p2
    if p2 is None:
        return p1
    x1, y1 = p1
    x2, y2 = p2
    if x1 == x2 and (y1 + y2) % P == 0:
        return None
    if p1 == p2:
        m = (3 * x1 * x1 + A) * _inv(2 * y1) % P
    else:
        m = (y2 - y1) * _inv(x2 - x1) % P
    x3 = (m * m - x1 - x2) % P
    y3 = (m * (x1 - x3) - y1) % P
    return (x3, y3)


def scalar_mult_ref(k: int, pt=G):
    """k * pt on secp256r1 (textbook affine double-and-add). k is reduced mod N.

    The transparent reference. Kept as the definition of correctness: `scalar_mult` has a fast
    path for the generator and the tests assert the two agree, so the acceleration can never
    silently change a value.
    """
    k %= N
    result = None
    addend = pt
    while k:
        if k & 1:
            result = add(result, addend)
        addend = add(addend, addend)
        k >>= 1
    return result


def _base_mult_via_library(k: int):
    """k * G using the `cryptography` (OpenSSL) scalar multiply.

    This is not a different curve or a different answer -- it is the SAME group operation computed
    by a constant-time C implementation. Measured on this host: **21.8 us against 494 us** for
    `scalar_mult_ref`, i.e. **23x**. (The gap was ~400x before `_inv` moved to extended Euclid;
    fixing the inverse fixed most of the pure-Python loop too, and the honest remaining factor is
    23x, not 400x.)

    Butterfly provisioning does four base multiplies per pseudonym certificate, so a 1188-device
    x 5-rotation fleet costs ~0.5 s here against ~12 s of pure-Python base multiplies alone. That
    is the difference between provisioning being a rounding error in a run's startup and being
    visible in it.

    `cryptography` is already a hard requirement (requirements.txt), so this adds no dependency.
    """
    from cryptography.hazmat.primitives.asymmetric import ec as _cec

    nums = _cec.derive_private_key(k, _cec.SECP256R1()).public_key().public_numbers()
    return (nums.x, nums.y)


def scalar_mult(k: int, pt=G):
    """k * pt on secp256r1. k is reduced mod the group order N.

    For the generator (the overwhelmingly common case: every butterfly expansion value, every
    PCA randomiser, every keypair) this delegates to the library scalar multiply; otherwise it
    runs the transparent double-and-add. `scalar_mult_ref` is the unaccelerated definition and
    `test_scms_crypto.py::test_fast_base_mult_matches_reference` pins the two together.
    """
    k %= N
    if k == 0:
        return None
    if pt is G or pt == G:
        return _base_mult_via_library(k)
    return scalar_mult_ref(k, pt)
