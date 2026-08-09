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
    return pow(x % P, P - 2, P)


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


def scalar_mult(k: int, pt=G):
    """k * pt on secp256r1 (double-and-add). k is reduced mod the group order N."""
    k %= N
    result = None
    addend = pt
    while k:
        if k & 1:
            result = add(result, addend)
        addend = add(addend, addend)
        k >>= 1
    return result
