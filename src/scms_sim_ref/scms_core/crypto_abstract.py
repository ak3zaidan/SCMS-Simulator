"""Abstract-but-real message security for the reference pipeline.

The dataset records *outcomes* of security operations (signature valid/invalid,
certificate expired/revoked/forged), which are curve-independent. To keep the
whole pipeline byte-for-byte reproducible we use **deterministic Ed25519**
(RFC 8032) keyed from the scenario RNG: same seed -> same keys -> same
signatures. The production MOSAIC layer will use ECDSA/ECQV over NIST P-256 per
IEEE 1609.2; only the signing primitive differs, not the recorded outcomes.

`HashedId8` follows IEEE 1609.2: the low-order 8 bytes of the SHA-256 hash of the
credential's public material -- this is the only identity that ever appears in
MA-visible data until an investigation resolves it.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import (
    Ed25519PrivateKey,
    Ed25519PublicKey,
)


def canonical_bytes(obj: Any) -> bytes:
    """Deterministic serialization for signing and determinism hashing.

    Sorted keys, no whitespace, bytes rendered as lowercase hex. Two runs that
    are semantically identical produce identical bytes here.
    """
    def _default(o: Any) -> Any:
        if isinstance(o, (bytes, bytearray)):
            return o.hex()
        raise TypeError(f"not JSON-serializable: {type(o)!r}")

    return json.dumps(obj, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=True, default=_default).encode("utf-8")


def keypair_from_seed(seed32: bytes) -> Ed25519PrivateKey:
    """Deterministic keypair from a 32-byte seed (drawn from the scenario RNG)."""
    if len(seed32) != 32:
        raise ValueError("Ed25519 seed must be 32 bytes")
    return Ed25519PrivateKey.from_private_bytes(seed32)


def public_bytes(priv: Ed25519PrivateKey) -> bytes:
    from cryptography.hazmat.primitives import serialization
    return priv.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )


def hashed_id8(public_material: bytes) -> bytes:
    """IEEE 1609.2 HashedId8: low-order 8 bytes of SHA-256(public material)."""
    return hashlib.sha256(public_material).digest()[-8:]


def sign(priv: Ed25519PrivateKey, payload: Any) -> bytes:
    return priv.sign(canonical_bytes(payload))


def verify(public_material: bytes, payload: Any, signature: bytes) -> bool:
    try:
        Ed25519PublicKey.from_public_bytes(public_material).verify(
            signature, canonical_bytes(payload)
        )
        return True
    except (InvalidSignature, ValueError):
        return False
