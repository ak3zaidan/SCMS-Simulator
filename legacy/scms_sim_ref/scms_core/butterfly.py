"""Butterfly key expansion — reference implementation (CAMP SCP1 / Brecht et al.).

One tiny device upload (two "caterpillar" EC public keys A, P and two AES expansion
keys ck, ek) lets the RA generate an arbitrary number of unlinkable per-period
"cocoon" keys, and the PCA issue certificates, such that:
  * the RA never learns the certified public key (the PCA adds a secret random c),
  * the PCA cannot link two certificates to one device (cocoon keys are pseudorandom
    without ck, and requests are shuffled),
  * the device can still re-derive each usable private key.

Expansion function (SCP1): with ι = (i, j) packed into a 128-bit AES input block
`x = prefix32 || i32 || j32 || 0^32` (prefix 0^32 for f1, 1^32 for f2),

    f(k, ι) = ( AES(k, x+1) XOR (x+1) || AES(k, x+2) XOR (x+2) || AES(k, x+3) XOR (x+3) ) mod n

Explicit-certificate variant is implemented (final private key = b_ι + c). The tests
assert the fundamental identity: derived_private * G == certified_public_key.
"""

from __future__ import annotations

import os
from dataclasses import dataclass

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

from .ec import G, N, add, scalar_mult

_MASK128 = (1 << 128) - 1


def _aes_block(key16: bytes, block16: bytes) -> bytes:
    enc = Cipher(algorithms.AES(key16), modes.ECB()).encryptor()
    return enc.update(block16) + enc.finalize()


def _xor(a: bytes, b: bytes) -> bytes:
    return bytes(x ^ y for x, y in zip(a, b))


def _x_block_int(prefix_one: bool, i: int, j: int) -> int:
    prefix = b"\xff\xff\xff\xff" if prefix_one else b"\x00\x00\x00\x00"
    return int.from_bytes(prefix + i.to_bytes(4, "big") + j.to_bytes(4, "big") + b"\x00" * 4, "big")


def _f_int(key16: bytes, x_block_int: int) -> int:
    out = b""
    for m in (1, 2, 3):
        xm = ((x_block_int + m) & _MASK128).to_bytes(16, "big")
        out += _xor(_aes_block(key16, xm), xm)
    return int.from_bytes(out, "big") % N


def f1(ck: bytes, i: int, j: int) -> int:
    """Signing-key expansion value f1(ck, (i, j))."""
    return _f_int(ck, _x_block_int(False, i, j))


def f2(ek: bytes, i: int, j: int) -> int:
    """Encryption-key expansion value f2(ek, (i, j))."""
    return _f_int(ek, _x_block_int(True, i, j))


@dataclass(frozen=True)
class Caterpillar:
    """The device's butterfly request material (public parts A, P go to the RA)."""
    a: int          # caterpillar signing private scalar
    p: int          # caterpillar encryption private scalar
    ck: bytes       # AES-128 expansion key for signing
    ek: bytes       # AES-128 expansion key for encryption

    @property
    def A(self):
        return scalar_mult(self.a)

    @property
    def P(self):
        return scalar_mult(self.p)


def new_caterpillar(seed: bytes | None = None) -> Caterpillar:
    """Create a device caterpillar. Deterministic if a 64-byte seed is supplied."""
    if seed is None:
        seed = os.urandom(64)
    if len(seed) < 64:
        raise ValueError("seed must be >= 64 bytes")
    a = (int.from_bytes(seed[0:32], "big") % (N - 1)) + 1
    p = (int.from_bytes(seed[32:64], "big") % (N - 1)) + 1
    # ck/ek derived from the seed tail so the whole request is reproducible in tests
    import hashlib
    ck = hashlib.sha256(b"ck" + seed).digest()[:16]
    ek = hashlib.sha256(b"ek" + seed).digest()[:16]
    return Caterpillar(a=a, p=p, ck=ck, ek=ek)


def ra_cocoon_keys(A, P, ck: bytes, ek: bytes, i: int, j: int):
    """RA expands the caterpillar to per-(i,j) cocoon keys (B for signing, Q for encryption)."""
    B = add(A, scalar_mult(f1(ck, i, j)))
    Q = add(P, scalar_mult(f2(ek, i, j)))
    return B, Q


def pca_certify_explicit(B, c: int):
    """PCA explicit-cert step: pick secret c, certify the public key B + c*G.

    In the full protocol the PCA also encrypts (cert, c) to Q and signs the ciphertext;
    here we return the certified public key and C = c*G that the device needs.
    """
    C = scalar_mult(c)
    return add(B, C), C


def device_signing_private(cat: Caterpillar, i: int, j: int, c: int) -> int:
    """Device re-derives the usable signing private key: b_ι + c (mod n)."""
    return (cat.a + f1(cat.ck, i, j) + c) % N


def device_encryption_private(cat: Caterpillar, i: int, j: int) -> int:
    """Device re-derives the cocoon encryption private key q_ι = p + f2(ek, ι) (mod n)."""
    return (cat.p + f2(cat.ek, i, j)) % N
