"""SCMS linkage values — reference implementation.

Faithful to the CAMP SCMS PoC "SCP2: Linkage Values" specification and Brecht
et al., "A Security Credential Management System for V2X Communications"
(IEEE T-ITS 2018, arXiv:1802.05323). This module is the validated *reference*;
the Java back-end `crypto/LinkageValueEngine` will mirror it.

Key formulas (per LA x in {1, 2}; la_id_x is a 16-bit identity):

  seed chain  : ls_x(i) = trunc128( SHA-256( la_id_x || ls_x(i-1) || 0^112 ) )
  pre-linkage : plv_x(i, j) = trunc72( AES(ls_x(i), la_id_x || j || 0^80)
                                        XOR (la_id_x || j || 0^80) )     # Davies-Meyer
  linkage val : lv(i, j) = plv_1(i, j) XOR plv_2(i, j)     # computed ONLY by the PCA

Privacy properties this enables (asserted by the tests):
  * Two-LA split: neither LA ever sees the final lv (it is the XOR of the two plvs).
  * Forward-only revocation: publishing ls_x(i) reveals a device's certificates
    from period i onward only; earlier periods stay unlinkable (the hash chain
    cannot be run backwards) -> "a vehicle is trackable only after revocation".
  * O(1) CRL cost: two 128-bit seeds revoke all of a device's certificates for
    every future period, regardless of how many certificates were issued.
"""

from __future__ import annotations

import hashlib
import os
from dataclasses import dataclass

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

# Byte widths fixed by the CAMP PoC specification.
LA_ID_BYTES = 2          # 16-bit la_id
LS_BYTES = 16            # 128-bit linkage seed
J_BYTES = 4              # j packed into the 128-bit AES block
PLV_BYTES = 9            # 72-bit pre-linkage / linkage value
_AES_BLOCK = 16


def _aes_ecb_block(key16: bytes, block16: bytes) -> bytes:
    """Raw single-block AES-128 permutation (ECB), used inside Davies-Meyer."""
    if len(key16) != 16 or len(block16) != 16:
        raise ValueError("AES-128 Davies-Meyer requires 16-byte key and block")
    enc = Cipher(algorithms.AES(key16), modes.ECB()).encryptor()
    return enc.update(block16) + enc.finalize()


def _xor(a: bytes, b: bytes) -> bytes:
    return bytes(x ^ y for x, y in zip(a, b))


def _la_id_bytes(la_id: int) -> bytes:
    if not 0 <= la_id < (1 << (8 * LA_ID_BYTES)):
        raise ValueError(f"la_id must be a {LA_ID_BYTES*8}-bit value, got {la_id}")
    return la_id.to_bytes(LA_ID_BYTES, "big")


def random_linkage_seed() -> bytes:
    """A fresh 128-bit initial linkage seed ls_x(0), unique per device per LA."""
    return os.urandom(LS_BYTES)


def linkage_seed_next(la_id: int, ls_prev: bytes) -> bytes:
    """One step of the per-i-period seed hash-chain: ls_x(i) from ls_x(i-1)."""
    if len(ls_prev) != LS_BYTES:
        raise ValueError("linkage seed must be 16 bytes")
    pad = b"\x00" * (32 - LA_ID_BYTES - LS_BYTES)   # 0^112
    digest = hashlib.sha256(_la_id_bytes(la_id) + ls_prev + pad).digest()
    return digest[:LS_BYTES]                          # truncate to 128 bits


def linkage_seed_at(la_id: int, ls0: bytes, i: int) -> bytes:
    """ls_x(i): hash the initial seed forward i times (i = i-period index)."""
    if i < 0:
        raise ValueError("period index i must be >= 0")
    ls = ls0
    for _ in range(i):
        ls = linkage_seed_next(la_id, ls)
    return ls


def pre_linkage_value(la_id: int, ls_i: bytes, j: int) -> bytes:
    """plv_x(i, j): AES Davies-Meyer over the (la_id || j || 0^80) block, trunc 72 bits."""
    if len(ls_i) != LS_BYTES:
        raise ValueError("linkage seed must be 16 bytes")
    pad = b"\x00" * (_AES_BLOCK - LA_ID_BYTES - J_BYTES)   # 0^80
    block = _la_id_bytes(la_id) + j.to_bytes(J_BYTES, "big") + pad
    dm = _xor(_aes_ecb_block(ls_i, block), block)          # Davies-Meyer compression
    return dm[:PLV_BYTES]                                    # truncate to 72 bits


def linkage_value(plv1: bytes, plv2: bytes) -> bytes:
    """lv(i, j) = plv_1 XOR plv_2. In the real system, only the PCA computes this."""
    if len(plv1) != PLV_BYTES or len(plv2) != PLV_BYTES:
        raise ValueError("pre-linkage values must be 9 bytes")
    return _xor(plv1, plv2)


@dataclass(frozen=True)
class DeviceLinkageContext:
    """Per-device linkage material.

    ls1_0 / ls2_0 are held by LA1 / LA2 respectively and NEVER shared across the
    two authorities in the clear. This class colocates them only for the reference
    engine and the oracle; in the simulator each seed lives inside its own LA
    module and only encrypted-for-PCA pre-linkage values ever cross the RA.
    """

    la_id1: int
    la_id2: int
    ls1_0: bytes
    ls2_0: bytes

    def linkage_value_for(self, i: int, j: int) -> bytes:
        """The lv embedded in this device's certificate for period i, index j."""
        plv1 = pre_linkage_value(self.la_id1, linkage_seed_at(self.la_id1, self.ls1_0, i), j)
        plv2 = pre_linkage_value(self.la_id2, linkage_seed_at(self.la_id2, self.ls2_0, i), j)
        return linkage_value(plv1, plv2)


@dataclass(frozen=True)
class CrlLinkageEntry:
    """A CRL entry that revokes one device from period `i` forward.

    Published fields are exactly the two current-period linkage seeds and their
    la_ids -- this is what the Misbehavior Authority receives from the two LAs
    after identity resolution, and all a verifier needs to recognise the device.
    """

    i: int
    la_id1: int
    la_id2: int
    ls1_i: bytes
    ls2_i: bytes
    jmax: int = 20

    @classmethod
    def from_device(cls, ctx: DeviceLinkageContext, i: int, jmax: int = 20) -> "CrlLinkageEntry":
        return cls(
            i=i,
            la_id1=ctx.la_id1,
            la_id2=ctx.la_id2,
            ls1_i=linkage_seed_at(ctx.la_id1, ctx.ls1_0, i),
            ls2_i=linkage_seed_at(ctx.la_id2, ctx.ls2_0, i),
            jmax=jmax,
        )

    def matches(self, cert_i: int, cert_j: int, cert_lv: bytes) -> bool:
        """True iff a certificate (cert_i, cert_j, cert_lv) is revoked by this entry.

        Forward-only: certificates from periods *before* the revocation period are
        never matched (backward privacy is preserved).
        """
        if cert_i < self.i:
            return False
        if not 0 <= cert_j < self.jmax:
            return False
        ls1 = linkage_seed_at(self.la_id1, self.ls1_i, cert_i - self.i)
        ls2 = linkage_seed_at(self.la_id2, self.ls2_i, cert_i - self.i)
        recomputed = linkage_value(
            pre_linkage_value(self.la_id1, ls1, cert_j),
            pre_linkage_value(self.la_id2, ls2, cert_j),
        )
        return recomputed == cert_lv


def crl_contains(entries: "list[CrlLinkageEntry]", cert_i: int, cert_j: int, cert_lv: bytes) -> bool:
    """Enforcement helper: is this certificate revoked by any entry on the CRL?"""
    return any(e.matches(cert_i, cert_j, cert_lv) for e in entries)
