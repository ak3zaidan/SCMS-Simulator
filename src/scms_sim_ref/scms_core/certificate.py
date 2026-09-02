"""IEEE 1609.2 certificate structures — the fields, honestly labelled.

`STANDARDS-AUDIT.md` finding 4: *"Certificates are not certificates. A hex string plus
`{i, j, lv, valid_from, valid_to, issuing_pca}`. No ToBeSignedCertificate, issuer, cracaId,
crlSeries, region, assuranceLevel, appPermissions/PsidSsp, or verifyKeyIndicator. The linkage value
is real but lives in a side dict rather than inside a certificate structure."*

This module supplies those fields, with the linkage value **inside** `CertificateId.linkageData`
where 1609.2 puts it, and an issuer that is the HashedId8 of a real PCA certificate over a real
ECDSA signature.

## What is ENCODED and what is STRUCTURAL — read this before quoting the module

**ENCODED — real standard octets, byte-exact, independently checkable:**

| element | citation | note |
|---|---|---|
| `EccP256CurvePoint` compressed-y-0/1 | 1609.2 6.3.23 / SEC1 2.3.3 | `ecdsa_p256.compress_point`; 33 octets |
| `EcdsaP256Signature` (r, s) | 1609.2 6.3.29 | two 32-octet big-endian integers |
| `HashedId8` / `HashedId3` | 1609.2 6.4.3 | low-order 8 / 3 octets of SHA-256 |
| `LinkageValue` | 1609.2 6.4.28, CAMP SCP2 | 9 octets, from `scms_core.linkage` |
| `Time32` / `Duration` values | 1609.2 6.4.16 / 6.4.10 | seconds since the 2004-01-01T00:00:00Z epoch |
| `Psid` values | ISO/TS 17419 via ETSI TS 102 965 | CA=36, DEN=37, MRS(TS 103 759)=... see PSID_* |

**STRUCTURAL — the right fields in the right relationships, but NOT 1609.2 COER octets.**
`to_octets()` emits a canonical, deterministic, length-prefixed serialisation of this module's own
design. It is a stable byte string suitable for signing and for hashing to a HashedId8, and it is
*not* interoperable. PLUGIN-ARCHITECTURE.md 6.2 sets COER as Tier B, reachable only via pycrate
(`asn1tools` fails on 1609.2's X.681 information object classes), which is a dependency decision
outside this task. The consequence, stated plainly: **`ExplicitCertificate.hashed_id8()` is 8 bytes
of SHA-256 as 1609.2 requires, over a preimage that is ours, not COER.** A real 1609.2 stack would
compute a different HashedId8 for the same logical certificate. Nothing in this repo compares the
two, and nothing may claim it does.

The rule the audit lays down (6.5): *"1609.2-structured, signatures simulated"* was the permitted
claim while nothing signed. With `provisioning.py` and `secured.py` on the path the permitted claim
becomes **"IEEE 1609.2-structured explicit certificates carrying real ECDSA-P256 signatures over a
non-COER canonical serialisation"** — and never "IEEE 1609.2 compliant".
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from typing import Optional, Sequence

from .ecdsa_p256 import COMPRESSED_POINT_BYTES, SIGNATURE_BYTES, VerifyingKey, compress_point

#: IEEE 1609.2 / ETSI TS 103 097 time origin: 2004-01-01T00:00:00 UTC (TAI in the standard).
#: `Time32` counts seconds from here, `Time64` microseconds.
TIME_EPOCH_ISO = "2004-01-01T00:00:00Z"
#: Unix seconds at that instant; the simulator's float `t` is mapped through a run `time_base`.
TIME_EPOCH_UNIX = 1072915200

#: ITS-AIDs / PSIDs (ISO/TS 17419, registered values used by ETSI TS 102 965).
PSID_CA_BASIC_SERVICE = 36          # CAM
PSID_DEN_BASIC_SERVICE = 37         # DENM
PSID_MISBEHAVIOUR_REPORTING = 38    # TS 103 759 misbehaviour report service

#: 1609.2 `CertificateType`.
CERT_TYPE_EXPLICIT = "explicit"
CERT_TYPE_IMPLICIT = "implicit"

#: 1609.2 `Duration` CHOICE arms, in the standard's own order, with their multipliers in seconds.
_DURATION_UNITS = (("microseconds", 1e-6), ("milliseconds", 1e-3), ("seconds", 1),
                   ("minutes", 60), ("hours", 3600), ("sixtyHours", 216000), ("years", 31556952))
_DURATION_TAGS = {name: i for i, (name, _m) in enumerate(_DURATION_UNITS)}
_UINT16_MAX = 65535


# ------------------------------------------------------------------ canonical serialisation ---- #
# Deliberately tiny and total: a tag byte, then either a fixed-width integer or a length-prefixed
# octet string. Every field is emitted unconditionally in declaration order, with an explicit
# presence byte for the OPTIONALs, so two structurally equal certificates always serialise equal
# and no field can be silently dropped. This is what makes `to_octets()` safe to sign and to hash.

def _u8(v: int) -> bytes:
    return int(v).to_bytes(1, "big")


def _u16(v: int) -> bytes:
    return int(v).to_bytes(2, "big")


def _u32(v: int) -> bytes:
    return int(v).to_bytes(4, "big")


def _blob(b: Optional[bytes]) -> bytes:
    if b is None:
        return b"\x00"
    return b"\x01" + _u16(len(b)) + bytes(b)


def _opt_u32(v: Optional[int]) -> bytes:
    return b"\x00" if v is None else b"\x01" + _u32(v)


# ----------------------------------------------------------------------------- the structures -- #

@dataclass(frozen=True, slots=True)
class PsidSsp:
    """1609.2 6.4.25 `PsidSsp ::= SEQUENCE {psid Psid, ssp ServiceSpecificPermissions OPTIONAL}`.

    This is the field that answers "what is this certificate allowed to say?". Its absence is why
    today a pseudonym certificate cannot be denied permission to sign a DENM or a misbehaviour
    report: there is no permission to check. `secured.py` enforces it.
    """

    psid: int
    ssp: Optional[bytes] = None                 # `opaque` arm; bitmapSsp is not modelled

    def to_octets(self) -> bytes:
        return _u32(self.psid) + _blob(self.ssp)


@dataclass(frozen=True, slots=True)
class Duration:
    """1609.2 6.4.10 `Duration ::= CHOICE {...}` — a unit tag plus a Uint16."""

    unit: str
    value: int

    def __post_init__(self):
        if self.unit not in _DURATION_TAGS:
            raise ValueError(f"not a 1609.2 Duration unit: {self.unit!r}")
        if not 0 <= self.value <= _UINT16_MAX:
            raise ValueError("Duration value is a Uint16")

    @classmethod
    def from_seconds(cls, seconds: float) -> "Duration":
        """Finest 1609.2 unit that represents `seconds` exactly and fits a Uint16; failing that,
        the finest unit that fits, rounded **up** — a validity period must never be encoded short
        of the window the issuer intended.

        Deterministic and total: the unit is a pure function of the value, so two runs that compute
        the same lifetime always encode the same Duration.
        """
        s = int(round(seconds))
        if s < 0:
            raise ValueError("duration must be >= 0")
        for name, mult in _DURATION_UNITS[2:]:                 # seconds and coarser
            m = int(mult)
            if s % m == 0 and s // m <= _UINT16_MAX:
                return cls(name, s // m)
        for name, mult in _DURATION_UNITS[2:]:                 # else round up into a unit that fits
            m = int(mult)
            if -(-s // m) <= _UINT16_MAX:
                return cls(name, -(-s // m))
        raise ValueError(f"duration {seconds} exceeds the 1609.2 Duration range")

    @property
    def seconds(self) -> float:
        return self.value * dict(_DURATION_UNITS)[self.unit]

    def to_octets(self) -> bytes:
        return _u8(_DURATION_TAGS[self.unit]) + _u16(self.value)


@dataclass(frozen=True, slots=True)
class ValidityPeriod:
    """1609.2 6.4.15 `ValidityPeriod ::= SEQUENCE {start Time32, duration Duration}`.

    Note what changes by putting the window here: `start`/`duration` are **inside the signed
    certificate**. An attacker can no longer set `cvt = t - 5.0` on the wire the way
    `run.py`'s `ExpiredCert` does today — see `secured.py` and the attack mapping in the report.
    """

    start: int                                   # Time32: seconds since 2004-01-01T00:00:00Z
    duration: Duration

    @property
    def end(self) -> int:
        return self.start + int(self.duration.seconds)

    def contains(self, time32: int) -> bool:
        return self.start <= time32 <= self.end

    def to_octets(self) -> bytes:
        return _u32(self.start) + self.duration.to_octets()


@dataclass(frozen=True, slots=True)
class LinkageData:
    """1609.2 6.4.27 `LinkageData ::= SEQUENCE {iCert IValue, linkage-value LinkageValue,
    group-linkage-value GroupLinkageValue OPTIONAL}`.

    **This is the fix for the audit's "the linkage value is real but lives in a side dict".**
    `run.py` keeps `pseudonym_info[digest] = {"i":..., "j":..., "lv":...}` beside the certificate;
    1609.2 puts `iCert` and the 9-octet linkage value *in* `CertificateId`, which is what makes a
    CRL linkage entry checkable by any receiver holding only the certificate.

    `j` is deliberately NOT here, and that is not an omission: in CAMP SCP2 `j` is an index the
    issuer uses to compute the linkage value; a verifier recovers it by trying `j in [0, jmax)`
    against the published seeds (`CrlLinkageEntry.matches` already does exactly that).
    """

    i_cert: int                                  # IValue, Uint16 — the linkage i-period
    linkage_value: bytes                         # 9 octets
    group_linkage_value: Optional[bytes] = None

    def __post_init__(self):
        if not 0 <= self.i_cert <= _UINT16_MAX:
            raise ValueError("iCert is a Uint16")
        if len(self.linkage_value) != 9:
            raise ValueError("LinkageValue is 9 octets (1609.2 6.4.28)")

    def to_octets(self) -> bytes:
        return _u16(self.i_cert) + _blob(self.linkage_value) + _blob(self.group_linkage_value)


@dataclass(frozen=True, slots=True)
class ToBeSignedCertificate:
    """1609.2 6.4.8. Every field the audit named as missing, plus the ones needed to make them mean
    something. `certIssuePermissions` / `certRequestPermissions` / `region` / the R2 extension
    sequences are deliberately absent: a pseudonym certificate is an end-entity certificate that
    issues nothing, and a region constraint the engine cannot evaluate would be decoration.
    """

    #: `CertificateId ::= CHOICE {linkageData, ...}` — pseudonym certs use the linkageData arm.
    id: LinkageData
    #: HashedId3 of the Certificate Revocation Authorization CA. Names WHO may authorise revoking
    #: this certificate; a CRL from any other CRACA is not applicable to it.
    craca_id: bytes
    #: Which CRL series this certificate appears on. `(cracaId, crlSeries)` is the pair a receiver
    #: uses to decide whether a given CRL is even relevant.
    crl_series: int
    validity_period: ValidityPeriod
    #: `VerificationKeyIndicator ::= CHOICE {verificationKey PublicVerificationKey ...}`, and
    #: `PublicVerificationKey ::= CHOICE {ecdsaNistP256 EccP256CurvePoint, ...}`. 33 compressed
    #: octets of the key that the butterfly expansion produced and that actually verifies messages.
    verify_key_indicator: bytes
    #: `SequenceOfPsidSsp`. What this certificate may sign.
    app_permissions: tuple = ()
    #: `SubjectAssurance` — 1609.2 6.4.22: 3 bits of assurance level, 2 bits of confidence.
    assurance_level: Optional[int] = None
    #: `PublicEncryptionKey` — the butterfly cocoon encryption key Q, compressed. Present because
    #: SCP1 produces it (`device_encryption_private`) and it is how a PCA response is delivered.
    encryption_key: Optional[bytes] = None

    def __post_init__(self):
        if len(self.craca_id) != 3:
            raise ValueError("cracaId is a HashedId3 (3 octets)")
        if not 0 <= self.crl_series <= 0xFFFFFFFF:
            raise ValueError("crlSeries is a Uint32")
        if len(self.verify_key_indicator) != COMPRESSED_POINT_BYTES:
            raise ValueError("verifyKeyIndicator must be a compressed EccP256CurvePoint")
        if self.encryption_key is not None and len(self.encryption_key) != COMPRESSED_POINT_BYTES:
            raise ValueError("encryptionKey must be a compressed EccP256CurvePoint")

    def verification_key(self) -> VerifyingKey:
        return VerifyingKey.from_compressed(self.verify_key_indicator)

    def permits(self, psid: int) -> bool:
        """True iff `appPermissions` grants this PSID. No appPermissions => permits nothing."""
        return any(p.psid == psid for p in self.app_permissions)

    def to_octets(self) -> bytes:
        out = [b"\x10",                                       # structure tag: ToBeSignedCertificate
               b"\x01", self.id.to_octets(),                  # CertificateId CHOICE arm 0 (linkage)
               _blob(self.craca_id),
               _u32(self.crl_series),
               self.validity_period.to_octets(),
               _u16(len(self.app_permissions))]
        out += [p.to_octets() for p in self.app_permissions]
        out += [_opt_u32(self.assurance_level),
                _blob(self.encryption_key),
                b"\x00",                                      # verifyKeyIndicator CHOICE arm 0
                b"\x00",                                      # PublicVerificationKey arm ecdsaNistP256
                _blob(self.verify_key_indicator)]
        return b"".join(out)


@dataclass(frozen=True, slots=True)
class ExplicitCertificate:
    """1609.2 6.4.2 `CertificateBase` with `type = explicit` and a mandatory signature.

    `issuer` is `IssuerIdentifier ::= CHOICE {sha256AndDigest HashedId8, self HashAlgorithm}` —
    a self-signed root carries `issuer_self = True` and its own hash algorithm; everything else
    carries the HashedId8 of the certificate that signed it. That single field is what turns
    `"issuing_pca": "PCA-1"` (a string in a side dict) into a verifiable chain link.
    """

    to_be_signed: ToBeSignedCertificate
    #: HashedId8 of the signer's certificate, or None for a self-signed certificate.
    issuer: Optional[bytes]
    #: Raw r||s over `signed_octets()` by the issuer's key.
    signature: bytes
    version: int = 3
    cert_type: str = CERT_TYPE_EXPLICIT

    def __post_init__(self):
        if self.issuer is not None and len(self.issuer) != 8:
            raise ValueError("issuer sha256AndDigest is a HashedId8 (8 octets)")
        if len(self.signature) != SIGNATURE_BYTES:
            raise ValueError("signature must be a raw 64-octet EcdsaP256Signature (r||s)")

    # -- the two byte strings, and they are different on purpose -------------------------------- #
    def signed_octets(self) -> bytes:
        """Exactly the octets the issuer signed: version, type, issuer, toBeSigned. The signature
        itself is excluded, which is the only way a signature over a structure can be well founded.
        """
        iss = b"\x00\x00" if self.issuer is None else b"\x01\x00" + self.issuer
        return (b"\x11" + _u8(self.version)
                + _u8(0 if self.cert_type == CERT_TYPE_EXPLICIT else 1)
                + iss + self.to_be_signed.to_octets())

    def to_octets(self) -> bytes:
        """The whole certificate. This is what HashedId8 is taken over (see the module docstring
        for the COER caveat) and what a `signer = certificate` secured message carries."""
        return self.signed_octets() + _blob(self.signature)

    def hashed_id8(self) -> bytes:
        """1609.2 6.4.3: low-order 8 octets of SHA-256 over the encoded certificate.

        Contrast with what the engine computes today, `hashed_id8(public_bytes(pk))` — 8 bytes of
        SHA-256 over a bare 32-byte Ed25519 public key. Same construction, different preimage: this
        one commits to the validity period, the permissions, the linkage value and the issuer, so
        two certificates that differ in any of those get different identifiers. The Java side is
        worse still (`hex(sha("cert|"+MASTER_SEED+"|"+unitId), 8)`, a hash of a *string*).
        """
        return hashlib.sha256(self.to_octets()).digest()[-8:]

    def digest_hex(self) -> str:
        """The 16-hex-character form the engine uses as `cert_digest` everywhere."""
        return self.hashed_id8().hex()

    def verify_signature(self, issuer_key: VerifyingKey) -> bool:
        return issuer_key.verify(self.signed_octets(), self.signature)

    # -- convenience the receive path wants -------------------------------------------------- #
    @property
    def linkage(self) -> LinkageData:
        return self.to_be_signed.id

    @property
    def validity(self) -> ValidityPeriod:
        return self.to_be_signed.validity_period

    def verification_key(self) -> VerifyingKey:
        return self.to_be_signed.verification_key()


def hashed_id3(material: bytes) -> bytes:
    """1609.2 6.4.3 `HashedId3`: low-order 3 octets of SHA-256. Used for `cracaId`."""
    return hashlib.sha256(material).digest()[-3:]


def hashed_id8(material: bytes) -> bytes:
    """1609.2 6.4.3 `HashedId8`. Identical construction to `crypto_abstract.hashed_id8`, restated
    here so `certificate.py` does not depend on the Ed25519 module it is meant to replace."""
    return hashlib.sha256(material).digest()[-8:]
