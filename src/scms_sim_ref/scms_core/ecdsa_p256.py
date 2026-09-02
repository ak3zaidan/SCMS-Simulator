"""Real ECDSA over secp256r1 (NIST P-256) — the signing primitive IEEE 1609.2 actually specifies.

This module exists because `STANDARDS-AUDIT.md` found that **no signing happens anywhere**: the
engine imports `crypto_abstract` but calls only `keypair_from_seed` / `public_bytes` / `hashed_id8` /
`canonical_bytes`, and `sig_ok` is a boolean the attack switch sets. Everything here is the real
operation, and `verify()` returning False is the *result* of a failed verification.

Three deliberate choices, each stated rather than assumed.

**1. `cryptography`, not `ec.py`, computes the signature.** `scms_core/ec.py` is a correct,
transparent affine implementation of the P-256 group — it is what makes butterfly's homomorphism
(`B = A + f1*G`) expressible at all, because no library exposes point addition. It is also ~400x
slower than OpenSSL, and a signature is not a homomorphism: it needs one base multiply, one modular
inverse and a hash. Using the library here costs nothing in transparency (the keys are still the
butterfly-derived scalars; see `provisioning.py`) and is the difference between signing being on the
dataset path and being a benchmark curiosity. `cryptography>=42` is already a hard requirement.

**2. Deterministic nonces (RFC 6979) when the backend supports it.** The whole project rests on
"same seed + config => byte-identical output". A random-`k` ECDSA signature is a fresh 64 random-ish
bytes on every run. Today no signature byte reaches an output record — only the boolean outcome does
— so random `k` would not *currently* move a digest; but the moment a `SignedMessage` is carried as
TS 103 759 `v2xPduEvidence` (the highest-value gap in the repo, per PLUGIN-ARCHITECTURE.md 6.3) it
would, silently. So determinism is established now, probed at import, and reported as
`SIGNING_MODE`. `cryptography` 50.0.1 on this host supports `ECDSA(..., deterministic_signing=True)`;
if a build does not, `SIGNING_MODE` becomes `"random_k"` and `DETERMINISTIC` is False — the caller
can then refuse to put signature bytes in a dataset instead of finding out later.

**3. Signatures are the raw 64-byte `r || s`.** IEEE 1609.2 defines
`EcdsaP256Signature ::= SEQUENCE {rSig EccP256CurvePoint, sSig OCTET STRING (SIZE(32))}`; the two
32-byte integers here are exactly (r, s). They are NOT COER octets — see `certificate.py` for the
same distinction, drawn the same way. `cryptography` speaks DER, so the conversion is explicit.

Measured on this host (Windows Server 2022, Python 3.12.10, cryptography 50.0.1, single thread) —
see `test_scms_crypto.py::test_measured_throughput`, which re-measures and asserts an order of
magnitude rather than a pinned number, because a pinned rate on a shared box is a flaky test:

| operation | rate | per call |
|---|---|---|
| `SigningKey.sign` (RFC 6979 deterministic, incl. DER->raw) | 58 130 /s | 17.2 us |
| `VerifyingKey.verify` (incl. raw->DER, key object cached) | 27 714 /s | 36.1 us |
| `secured.MessageSigner.sign` (header + 1609.2 double hash) | 51 979 /s | 19.2 us |
| `secured.MessageVerifier.verify` (chain + validity + CRL + signature) | 25 371 /s | 39.4 us |
| `MessageVerifier.verify` at the measured 14.14x fan-out, cached | 183 048 /s | 5.5 us |
| `VerifyingKey` construction | ~165 000 /s | 6.1 us |
"""

from __future__ import annotations

import hashlib

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec as _cec
from cryptography.hazmat.primitives.asymmetric.utils import (
    decode_dss_signature,
    encode_dss_signature,
)

from .ec import N, P, is_on_curve

#: IEEE 1609.2 `Signature` CHOICE arm this module implements.
SIGNATURE_ALGORITHM = "ecdsaNistP256Signature"
#: Raw signature width: two 32-byte big-endian integers.
SIGNATURE_BYTES = 64
#: Compressed EccP256CurvePoint width (SEC1 point compression: 0x02/0x03 || X).
COMPRESSED_POINT_BYTES = 33

_SHA256 = hashes.SHA256()
_CURVE = _cec.SECP256R1()


def _probe_deterministic() -> bool:
    try:
        alg = _cec.ECDSA(_SHA256, deterministic_signing=True)
    except TypeError:                                   # pragma: no cover - old cryptography
        return False
    try:
        k = _cec.derive_private_key(0x1234_5678, _CURVE)
        return k.sign(b"probe", alg) == k.sign(b"probe", alg)
    except Exception:                                   # pragma: no cover - backend refusal
        return False


#: True iff RFC 6979 deterministic ECDSA is available; see the module docstring.
DETERMINISTIC: bool = _probe_deterministic()
#: `"rfc6979"` or `"random_k"`. Belongs in `manifest["standards_profile"]["security_envelope"]`.
SIGNING_MODE: str = "rfc6979" if DETERMINISTIC else "random_k"

_SIGN_ALG = (_cec.ECDSA(_SHA256, deterministic_signing=True) if DETERMINISTIC
             else _cec.ECDSA(_SHA256))
_VERIFY_ALG = _cec.ECDSA(_SHA256)


# --------------------------------------------------------------------------- points and keys ---- #

def compress_point(pt) -> bytes:
    """SEC1 compressed encoding of an affine point — 1609.2 `EccP256CurvePoint` compressed-y-0/1."""
    if pt is None:
        raise ValueError("cannot encode the point at infinity as an EccP256CurvePoint")
    x, y = pt
    return bytes([0x02 | (y & 1)]) + int(x).to_bytes(32, "big")


def decompress_point(blob: bytes):
    """Inverse of `compress_point`. Raises on a point that is not on P-256."""
    if len(blob) != COMPRESSED_POINT_BYTES or blob[0] not in (0x02, 0x03):
        raise ValueError("not a compressed EccP256CurvePoint")
    x = int.from_bytes(blob[1:], "big")
    alpha = (pow(x, 3, P) + (P - 3) * x + 0x5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B) % P
    y = pow(alpha, (P + 1) // 4, P)                     # P = 3 mod 4
    if (y * y - alpha) % P != 0:
        raise ValueError("compressed point is not on the curve")
    if (y & 1) != (blob[0] & 1):
        y = P - y
    pt = (x, y)
    if not is_on_curve(pt):                             # pragma: no cover - defence in depth
        raise ValueError("decompressed point is not on the curve")
    return pt


class SigningKey:
    """A private signing key held as the P-256 scalar the butterfly expansion produced.

    The scalar is the honest unit here: `provisioning.device_private_scalar` returns
    `a + f1(ck, i, j) + c mod n`, and *that integer* is what must sign. Construction costs one
    library key derivation (~16 us), so build it once per pseudonym, not per message.
    """

    __slots__ = ("_d", "_key", "_public_point")

    def __init__(self, d: int):
        d %= N
        if d == 0:
            raise ValueError("private scalar must be non-zero mod n")
        self._d = d
        self._key = _cec.derive_private_key(d, _CURVE)
        nums = self._key.public_key().public_numbers()
        self._public_point = (nums.x, nums.y)

    @property
    def scalar(self) -> int:
        return self._d

    @property
    def public_point(self):
        return self._public_point

    def public_key(self) -> "VerifyingKey":
        return VerifyingKey(self._public_point, _key=self._key.public_key())

    def sign(self, message: bytes) -> bytes:
        """Sign `message` (the ToBeSigned octets). Returns raw 64-byte r||s."""
        r, s = decode_dss_signature(self._key.sign(message, _SIGN_ALG))
        return r.to_bytes(32, "big") + s.to_bytes(32, "big")


class VerifyingKey:
    """A public verification key. `1609.2 VerificationKeyIndicator.verificationKey`."""

    __slots__ = ("_pt", "_key")

    def __init__(self, point, *, _key=None):
        if not is_on_curve(point) or point is None:
            raise ValueError("verification key is not a point on P-256")
        self._pt = (int(point[0]), int(point[1]))
        self._key = _key or _cec.EllipticCurvePublicNumbers(
            self._pt[0], self._pt[1], _CURVE).public_key()

    @classmethod
    def from_compressed(cls, blob: bytes) -> "VerifyingKey":
        return cls(decompress_point(blob))

    @property
    def point(self):
        return self._pt

    def compressed(self) -> bytes:
        return compress_point(self._pt)

    def verify(self, message: bytes, signature: bytes) -> bool:
        """True iff `signature` is a valid ECDSA-P256-SHA256 signature by this key over `message`.

        Returns a bool rather than raising because the caller is a receiver deciding `sig_ok`, and
        a receiver does not get to treat "the sender lied" as an exception.
        """
        if len(signature) != SIGNATURE_BYTES:
            return False
        r = int.from_bytes(signature[:32], "big")
        s = int.from_bytes(signature[32:], "big")
        if not (0 < r < N and 0 < s < N):
            return False
        try:
            self._key.verify(encode_dss_signature(r, s), message, _VERIFY_ALG)
            return True
        except (InvalidSignature, ValueError):
            return False


# ----------------------------------------------------------------------- verification cache ---- #

class VerificationCache:
    """Memoise `verify` by (key, message, signature) — exact, not an approximation.

    ECDSA verification is a pure function of its three inputs, so caching its result cannot change
    any outcome. It matters because of the shape of a V2X run: one CAM is signed **once** and
    verified by every receiver that hears it. Measured on the 1188-vehicle InTAS run — 2 244 838
    delivered CAMs over 158 767 transmitted frames — the fan-out is **14.14**, so this collapses
    the bill from one verification per *(frame, receiver)* pair to one per *transmitted frame*:
    **88.5 s -> 12.3 s** for the whole receive path over 300 steps.

    `logical` counts what a real receiver would have computed. That number, not the cached one, is
    what a CPU-exhaustion / signature-flooding study needs, so it is kept even though the engine
    never pays it. `computed` is what this process actually spent.
    """

    __slots__ = ("_hits", "logical", "computed", "_cap")

    def __init__(self, capacity: int = 1 << 16):
        self._hits: dict[bytes, bool] = {}
        self.logical = 0
        self.computed = 0
        self._cap = capacity

    def verify(self, key: VerifyingKey, message: bytes, signature: bytes) -> bool:
        self.logical += 1
        tag = hashlib.sha256(key.compressed() + signature + message).digest()[:16]
        hit = self._hits.get(tag)
        if hit is not None:
            return hit
        self.computed += 1
        out = key.verify(message, signature)
        if len(self._hits) >= self._cap:
            self._hits.clear()                          # coarse, deterministic, order-free
        self._hits[tag] = out
        return out

    @property
    def hit_rate(self) -> float:
        return 0.0 if not self.logical else 1.0 - self.computed / self.logical

    def stats(self) -> dict:
        return {"logical_verifications": self.logical, "computed_verifications": self.computed,
                "hit_rate": round(self.hit_rate, 6)}
