"""The secured message: real signing, real verification, and the attack surface preserved.

This is the module that turns `sig_ok` from *"a boolean the attack switch sets"*
(`run.py:5084`, `elif tx.attack_type == "InvalidSignature": sig_ok = False`) into the return value
of an ECDSA verification.

## Shape

Modelled on IEEE 1609.2 `Ieee1609Dot2Data / SignedData`, profiled as ETSI TS 103 097 does it:

```
SignedData ::= SEQUENCE { hashId, tbsData ToBeSignedData, signer SignerIdentifier, signature }
ToBeSignedData ::= SEQUENCE { payload SignedDataPayload, headerInfo HeaderInfo }
HeaderInfo ::= SEQUENCE { psid Psid, generationTime Time64 OPTIONAL, expiryTime OPTIONAL, ... }
SignerIdentifier ::= CHOICE { digest HashedId8, certificate SequenceOfCertificate, self NULL }
```

Same honesty split as `certificate.py`: the **signature is real** ECDSA-P256-SHA256 over a
**canonical but non-COER** serialisation. `wire_octets()` is a byte string this repo defines.

## The one construction worth spelling out

1609.2 §5.3.1 signs `Hash(ToBeSignedData) || Hash(signer identifier input)`, where the second hash
is over the signer's certificate. That double hash is not ceremony: it **binds the signature to the
certificate**. Without it, a signature lifted off a valid message and re-presented alongside a
different certificate would still verify, and "certificate grafting" would be a free attack. This
module implements the construction, and `graft_certificate()` exists precisely so a test can show
the attack now fails.

## sig_ok is one bit of an eight-valued answer

The engine's two crypto detectors are separate (`signatureVerification` reads `obs.sig_ok`;
`certValidity` reads `cvf`/`cvt`), so `VerificationResult` keeps them separate too and adds the
outcomes the boolean could never express: an unknown issuer, a certificate whose own signature is
bad, a revoked certificate, a certificate not permitted to sign this PSID, and a message whose
certificate the receiver has never seen. `signature_valid` is additionally **tri-state**, because a
receiver that drops a frame on its certificate has not checked the signature and must not claim to
have — see `VerificationResult`. Mapping all of this onto today's fields is in the report's wiring
diff; nothing here decides it, because the reception loop is not this module's to write.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, replace
from typing import Mapping, Optional, Sequence

from .certificate import (PSID_CA_BASIC_SERVICE, ExplicitCertificate, hashed_id8)
from .ecdsa_p256 import (SIGNATURE_BYTES, SigningKey, VerificationCache, VerifyingKey)
from .linkage import CrlLinkageEntry, linkage_seed_at, linkage_value, pre_linkage_value

#: 1609.2 `SignerIdentifier` arms this module uses.
SIGNER_DIGEST = "digest"
SIGNER_CERTIFICATE = "certificate"

#: Verification outcomes. `OK` and `SIGNATURE_INVALID` are the two the current boolean can express.
OK = "ok"
SIGNATURE_INVALID = "signature_invalid"
CERT_SIGNATURE_INVALID = "cert_signature_invalid"
UNKNOWN_ISSUER = "unknown_issuer"
CERT_NOT_YET_VALID = "cert_not_yet_valid"
CERT_EXPIRED = "cert_expired"
CERT_REVOKED = "cert_revoked"
PSID_NOT_PERMITTED = "psid_not_permitted"
CERT_UNAVAILABLE = "cert_unavailable"

#: Microseconds per second — `Time64` is microseconds since the 2004 epoch (`certificate.py`).
_USEC = 1_000_000


def time64(t: float, time_base: int = 0) -> int:
    """Engine seconds -> 1609.2 `Time64`. `time_base` is the run's Time32 for engine t = 0.0."""
    return int(round((time_base + t) * _USEC))


@dataclass(frozen=True, slots=True)
class HeaderInfo:
    """1609.2 6.3.9. `psid` is what makes `appPermissions` checkable."""

    psid: int
    generation_time: int                      # Time64
    expiry_time: Optional[int] = None         # Time64

    def to_octets(self) -> bytes:
        exp = b"\x00" if self.expiry_time is None else b"\x01" + self.expiry_time.to_bytes(8, "big")
        return (b"\x20" + self.psid.to_bytes(4, "big")
                + self.generation_time.to_bytes(8, "big") + exp)


@dataclass(frozen=True, slots=True)
class ToBeSignedData:
    """1609.2 6.3.6. `payload` is the encoded facilities-layer PDU — a CAM under a `MessageCodec`
    (`api/codec.py`, PLUGIN-ARCHITECTURE.md 2.5), today whatever the caller hands over."""

    payload: bytes
    header: HeaderInfo

    def to_octets(self) -> bytes:
        return (b"\x21" + len(self.payload).to_bytes(4, "big") + self.payload
                + self.header.to_octets())


@dataclass(frozen=True, slots=True)
class SignedMessage:
    """A signed PDU as it goes on the wire.

    `signer_digest` is always present (it is what a receiver keys its certificate cache on, and it
    is the engine's `cert_digest`); `certificate` is present only when the signer alternation says
    to attach it. TS 103 097 requires attaching at least once per second and on every response to
    an unrecognised-certificate indication; the ~150-200 byte difference is why
    `MessageCodec.wire_size_bytes` takes a `signer` argument at all.
    """

    tbs: ToBeSignedData
    signer: str
    signer_digest: bytes
    signature: bytes
    certificate: Optional[ExplicitCertificate] = None

    def __post_init__(self):
        if len(self.signature) != SIGNATURE_BYTES:
            raise ValueError("signature must be a raw 64-octet EcdsaP256Signature")
        if len(self.signer_digest) != 8:
            raise ValueError("signer digest must be a HashedId8")

    @property
    def digest_hex(self) -> str:
        """The engine's `cert_digest` — 16 hex characters, HashedId8 of the signer certificate."""
        return self.signer_digest.hex()

    @property
    def psid(self) -> int:
        return self.tbs.header.psid

    @property
    def generation_time(self) -> int:
        return self.tbs.header.generation_time

    def wire_octets(self) -> bytes:
        """Canonical serialisation of the whole SignedData. NOT TS 103 097 COER — see the module
        docstring. Its `len()` is nonetheless the right *shape* of input for airtime -> CBR -> DCC,
        and it moves by the certificate's size exactly as the real alternation does."""
        cert = b"\x00" if self.certificate is None else b"\x01" + self.certificate.to_octets()
        arm = b"\x00" if self.signer == SIGNER_DIGEST else b"\x01"
        return (b"\x22" + arm + self.signer_digest + cert
                + self.tbs.to_octets() + self.signature)


def signature_input(tbs: ToBeSignedData, cert: ExplicitCertificate,
                    cert_hash: Optional[bytes] = None) -> bytes:
    """1609.2 5.3.1: `Hash(tbsData) || Hash(signer identifier input)`. See the module docstring.

    `cert_hash` is `SHA-256(cert.to_octets())` when the caller already has it. The certificate does
    not change for the life of a pseudonym while the payload changes every message, so re-hashing
    ~250 octets of certificate per frame is pure waste. Caching it and the chain verdict together
    took `MessageVerifier.verify` from **44.0 us to 39.4 us**, and the cached fan-out path from
    10.2 us to 5.5 us per delivery — on the 1188-vehicle run, 10 s.
    """
    if cert_hash is None:
        cert_hash = hashlib.sha256(cert.to_octets()).digest()
    return hashlib.sha256(tbs.to_octets()).digest() + cert_hash


# ----------------------------------------------------------------------------------- signing ---- #

class MessageSigner:
    """Signs with one pseudonym credential. Hold one per active pseudonym, not per message."""

    __slots__ = ("credential", "certificate", "_key", "_digest", "_cert_hash", "psid")

    def __init__(self, credential, *, psid: int = PSID_CA_BASIC_SERVICE):
        self.credential = credential
        self.certificate = credential.certificate
        self._key: SigningKey = credential.signing_key
        full = hashlib.sha256(self.certificate.to_octets()).digest()
        self._digest = full[-8:]                # HashedId8 is the tail of the same hash
        self._cert_hash = full
        self.psid = psid

    @classmethod
    def from_parts(cls, certificate: ExplicitCertificate, key: SigningKey, *,
                   psid: int = PSID_CA_BASIC_SERVICE) -> "MessageSigner":
        """Sign with an arbitrary (certificate, key) pair rather than a provisioned credential.

        Two real callers: an RSU or a CA signing outside the pseudonym flow, and an **attacker**
        signing under a certificate it minted itself (`self_issued_certificate`) — which has no
        `PseudonymCredential` because no PCA ever issued it.
        """
        obj = cls.__new__(cls)
        obj.credential = None
        obj.certificate = certificate
        obj._key = key
        full = hashlib.sha256(certificate.to_octets()).digest()
        obj._digest = full[-8:]
        obj._cert_hash = full
        obj.psid = psid
        return obj

    def sign(self, payload: bytes, generation_time: int, *,
             include_certificate: bool = True, expiry_time: Optional[int] = None,
             psid: Optional[int] = None) -> SignedMessage:
        tbs = ToBeSignedData(payload, HeaderInfo(psid if psid is not None else self.psid,
                                                 generation_time, expiry_time))
        sig = self._key.sign(signature_input(tbs, self.certificate, self._cert_hash))
        return SignedMessage(
            tbs=tbs,
            signer=SIGNER_CERTIFICATE if include_certificate else SIGNER_DIGEST,
            signer_digest=self._digest,
            signature=sig,
            certificate=self.certificate if include_certificate else None)


# ------------------------------------------------------------------------------ verification ---- #

@dataclass(frozen=True, slots=True)
class VerificationResult:
    """What a receiver actually learns.

    **`signature_valid` is tri-state, and that is deliberate.** `None` means *the signature was not
    evaluated* — the certificate was rejected first and a real receiver stops there, because
    spending 36 us of ECDSA on a frame it has already decided to drop is exactly the CPU-exhaustion
    lever a flooding attacker wants. Reporting `True` there would be a lie ("the signature checked
    out") and reporting `False` would be a different lie ("the signature failed"), and both would
    corrupt the `signatureVerification` detector's meaning. Use `sig_ok` for the engine's boolean:
    it fires the detector only on a signature that was checked and failed.

    Pass `full=True` to `MessageVerifier.verify` to evaluate the signature regardless, which is
    what a study of "do bad-certificate senders also have bad signatures?" needs.
    """

    status: str
    signature_valid: Optional[bool]
    certificate_valid: bool
    revoked: bool
    #: True only when every check passed; what a receiver should act on.
    accepted: bool

    @property
    def sig_ok(self) -> bool:
        """The engine's `sig_ok`: False only for a signature that was checked and failed."""
        return self.signature_valid is not False

    @property
    def ok(self) -> bool:
        return self.accepted


#: status -> (signature_valid, certificate_valid, revoked). `None` = not evaluated.
_REJECT = {
    UNKNOWN_ISSUER:         (None, False, False),
    CERT_SIGNATURE_INVALID: (None, False, False),
    CERT_UNAVAILABLE:       (None, False, False),
    PSID_NOT_PERMITTED:     (None, False, False),
    CERT_NOT_YET_VALID:     (None, False, False),
    CERT_EXPIRED:           (None, False, False),
    CERT_REVOKED:           (None, True, True),
    SIGNATURE_INVALID:      (False, True, False),
}


def _result(status: str, signature_valid: Optional[bool] = None) -> VerificationResult:
    if status == OK:
        return VerificationResult(OK, True, True, False, True)
    sig, cert, rev = _REJECT[status]
    return VerificationResult(status, sig if signature_valid is None else signature_valid,
                              cert, rev, False)


class MessageVerifier:
    """Receiver-side verification against a trust store, a validity window and a CRL.

    Three caches, all exact rather than approximate — each is a memo of a pure function, so none
    can change an outcome:

    * `sig_cache` memoises message-signature verification by (key, input, signature). One CAM is
      verified by every receiver that hears it; on the measured 1188-vehicle InTAS run the fan-out
      is **14.14 receivers per transmitted frame**, so this is a 14x reduction in ECDSA operations.
      `sig_cache.logical` still counts what a real receiver would have spent, which is the number a
      CPU-exhaustion study needs. Measured effect on that run: **88.5 s -> 12.3 s** for the whole
      receive path.
    * `_status` / `_keys` memoise certificate-chain verification by HashedId8. A pseudonym
      certificate is re-presented on every frame for its whole lifetime; verifying its PCA
      signature per message would cost a **second** ECDSA verification per message and double the
      bill outright.
    * `_lv_by_i` indexes the CRL's linkage values per i-period — see `revoked`.

    `crl` is a list of `CrlLinkageEntry`, the engine's existing CAMP SCP2 objects
    (`run.py:4636`). Revocation therefore becomes something a **receiver** can enforce from the
    certificate alone, because the linkage value now lives inside it.
    """

    __slots__ = ("trust_store", "sig_cache", "crl", "jmax", "_status", "_keys", "_certs",
                 "_cert_hash", "_lv_by_i", "_lv_consumed")

    def __init__(self, trust_store: Mapping[bytes, VerifyingKey], *,
                 cache: Optional[VerificationCache] = None,
                 crl: Optional[Sequence[CrlLinkageEntry]] = None, jmax: int = 20):
        self.trust_store = dict(trust_store)
        self.sig_cache = cache if cache is not None else VerificationCache()
        #: Held by reference on purpose: `run.py` appends to its `crl_entries` list mid-run, and a
        #: receiver that had been handed a snapshot would keep accepting a revoked attacker.
        self.crl = crl if crl is not None else []
        self.jmax = jmax
        self._status: dict[bytes, str] = {}          # HashedId8 -> chain verdict
        self._keys: dict[bytes, VerifyingKey] = {}
        self._certs: dict[bytes, ExplicitCertificate] = {}
        self._cert_hash: dict[bytes, bytes] = {}
        self._lv_by_i: dict[int, set] = {}           # i-period -> revoked linkage values
        self._lv_consumed: dict[int, int] = {}       # i-period -> CRL entries already folded in

    # -- certificate store: the `signer = digest` arm needs one ---------------------------------- #
    def learn(self, cert: ExplicitCertificate) -> tuple:
        """Index a certificate. Returns (HashedId8, SHA-256 of the encoded certificate)."""
        octets = cert.to_octets()
        full = hashlib.sha256(octets).digest()
        hid = full[-8:]
        self._certs[hid] = cert
        self._cert_hash[hid] = full
        return hid, full

    def known(self, digest: bytes) -> bool:
        return digest in self._certs

    def _chain_ok(self, hid: bytes, cert: ExplicitCertificate) -> str:
        cached = self._status.get(hid)
        if cached is not None:
            return cached
        issuer_key = self.trust_store.get(cert.issuer) if cert.issuer is not None else None
        if issuer_key is None:
            self._status[hid] = UNKNOWN_ISSUER
            return UNKNOWN_ISSUER
        good = cert.verify_signature(issuer_key)
        self._status[hid] = OK if good else CERT_SIGNATURE_INVALID
        if good:
            self._keys[hid] = cert.verification_key()
        return self._status[hid]

    def revoked(self, cert: ExplicitCertificate) -> bool:
        """Is this certificate on the CRL? Decided from the certificate alone.

        `j` is not a certificate field (see `LinkageData`), so a receiver recognises a revoked
        certificate by recomputing the linkage values a CRL entry covers — every `j` in
        `[0, jmax)` — and testing membership. That is exactly `CrlLinkageEntry.matches`, and
        `test_revocation_index_agrees_with_crl_contains` pins the two together.

        It is done as an **incremental index** rather than a scan because the scan is quadratic in
        a way that matters: the 1188-vehicle run issues 155 revocations, and re-deriving every
        entry's 20 linkage values per certificate would be ~10^7 AES/SHA operations. Folding each
        new entry in once costs 155 x 20 = 3 100 for the whole run. Revocation is monotone here
        (`crl_entries.append`), which is what makes an index sound; the consumed-count per period
        is the guard that a later append is never missed.
        """
        if not self.crl:
            return False
        lk = cert.linkage
        return lk.linkage_value in self._revoked_values(lk.i_cert)

    def _revoked_values(self, i_cert: int) -> set:
        seen = self._lv_consumed.get(i_cert, 0)
        values = self._lv_by_i.setdefault(i_cert, set())
        if seen < len(self.crl):
            for entry in self.crl[seen:]:
                if i_cert < entry.i:                      # forward-only: backward privacy holds
                    continue
                ls1 = linkage_seed_at(entry.la_id1, entry.ls1_i, i_cert - entry.i)
                ls2 = linkage_seed_at(entry.la_id2, entry.ls2_i, i_cert - entry.i)
                for j in range(min(self.jmax, entry.jmax)):
                    values.add(linkage_value(pre_linkage_value(entry.la_id1, ls1, j),
                                             pre_linkage_value(entry.la_id2, ls2, j)))
            self._lv_consumed[i_cert] = len(self.crl)
        return values

    def verify(self, msg: SignedMessage, now: Optional[int] = None,
               full: bool = False) -> VerificationResult:
        """Verify `msg`. `now` is a Time32; when None the message's own generationTime is used,
        which is the receiver behaviour 1609.2 describes for validity-window checking.

        The certificate is established FIRST, because a signature by a key nobody trusts is not
        "invalid", it is unverifiable — and because dropping a frame before paying 36 us of ECDSA
        is the receiver's only defence against signature flooding. The cost of that ordering is
        that `signature_valid` is `None` on those paths; `full=True` buys the answer back.
        """
        hid = msg.signer_digest
        cert = self._certs.get(hid)
        if cert is None:
            if msg.certificate is None:
                return _result(CERT_UNAVAILABLE)
            # An attached certificate is indexed by ITS OWN hash, never by the digest the sender
            # claims: trusting `signer_digest` here would let a sender poison the cache with a
            # certificate filed under someone else's identifier.
            real_hid, _full = self.learn(msg.certificate)
            if real_hid != hid:
                return _result(CERT_UNAVAILABLE)
            cert = msg.certificate

        def _sig() -> Optional[bool]:
            """Evaluate the message signature, or decline to (returning None)."""
            if hid not in self._keys:
                return None                            # no trusted key: nothing to verify against
            return self.sig_cache.verify(
                self._keys[hid], signature_input(msg.tbs, cert, self._cert_hash[hid]),
                msg.signature)

        chain = self._chain_ok(hid, cert)
        if chain != OK:
            return _result(chain, _sig() if full else None)
        if not cert.to_be_signed.permits(msg.psid):
            return _result(PSID_NOT_PERMITTED, _sig() if full else None)
        t32 = now if now is not None else msg.generation_time // _USEC
        vp = cert.validity
        if t32 < vp.start:
            return _result(CERT_NOT_YET_VALID, _sig() if full else None)
        if t32 > vp.end:
            return _result(CERT_EXPIRED, _sig() if full else None)
        if self.revoked(cert):
            return _result(CERT_REVOKED, _sig() if full else None)
        if not _sig():
            return _result(SIGNATURE_INVALID, False)
        return _result(OK)

    def stats(self) -> dict:
        d = self.sig_cache.stats()
        d["certificates_chain_verified"] = len(self._status)
        return d


# ------------------------------------------------------------------------- the attack surface --- #
# Requirement 4: "with real signatures, a forged or invalid signature must still be expressible".
# Each function below is a THING AN ATTACKER DOES, not a flag. Every one produces a well-formed
# SignedMessage; what differs is what `MessageVerifier.verify` concludes about it.

def sign_with_foreign_key(signer: MessageSigner, foreign: SigningKey, payload: bytes,
                          generation_time: int, **kw) -> SignedMessage:
    """The honest replacement for `sig_ok = False`.

    An attacker presents its own (genuine, unrevoked, in-date) certificate and signs with a key
    that is not the one the certificate names. This is what "InvalidSignature" *is*: not a flag,
    but a signature that does not verify under the advertised verification key. `verify` returns
    `SIGNATURE_INVALID`, and it returns it because the ECDSA check failed.
    """
    msg = signer.sign(payload, generation_time, **kw)
    cert = msg.certificate or signer.certificate
    return replace(msg, signature=foreign.sign(signature_input(msg.tbs, cert)))


def tamper_after_signing(msg: SignedMessage, new_payload: bytes) -> SignedMessage:
    """A relay or a compromised stack alters the payload after signing. The signature still covers
    the old bytes, so it fails — which is the property a signature is *for*."""
    return replace(msg, tbs=ToBeSignedData(new_payload, msg.tbs.header))


def replay(msg: SignedMessage, *, at_generation_time: Optional[int] = None) -> SignedMessage:
    """Verbatim re-emission of a message captured earlier.

    The signature is **valid** — it was made by a legitimate key over these exact bytes — and that
    is the correct model. A replay is not a crypto failure; it is a freshness failure, and it is
    caught by `staleOrReplay` against `generationTime`, never by `signatureVerification`. Passing
    `at_generation_time` rewrites the header *without* re-signing, which is the weaker variant that
    a signature does defeat (`SIGNATURE_INVALID`), and is worth having as the contrast case.
    """
    if at_generation_time is None:
        return msg
    hdr = replace(msg.tbs.header, generation_time=at_generation_time)
    return replace(msg, tbs=ToBeSignedData(msg.tbs.payload, hdr))


def graft_certificate(msg: SignedMessage, other: ExplicitCertificate) -> SignedMessage:
    """Cut-and-paste: a valid signature re-presented under someone else's certificate.

    Fails because of the 1609.2 double hash (`signature_input`), which binds the signature to the
    certificate. Included as a regression: drop that binding and this attack silently succeeds.
    """
    return replace(msg, certificate=other, signer_digest=other.hashed_id8())


def present_stale_credential(credentials: Sequence, now_s: float):
    """Reuse a pseudonym whose validity window has passed — the honest `ExpiredCert`.

    **This is the attack whose current implementation stops working, and it matters.** Today
    `ExpiredCert` sets `cvt = t - 5.0` on the wire (`run.py:5086`). Once the validity period is a
    signed certificate field, an attacker cannot edit it — it can only *use a certificate it really
    has*. A rotating device holds exactly such certificates: its earlier pseudonyms. This returns
    the newest already-expired credential, or None when the device has none yet (an attacker in its
    first rotation period genuinely cannot mount this attack, which is a truer model than letting
    every attacker mount it from spawn).
    """
    expired = [c for c in credentials if c.valid_to < now_s]
    return max(expired, key=lambda c: c.valid_to) if expired else None


def present_future_credential(credentials: Sequence, now_s: float):
    """Reuse a not-yet-valid pseudonym — the honest `NotYetValid`. Same argument as above."""
    future = [c for c in credentials if c.valid_from > now_s]
    return min(future, key=lambda c: c.valid_from) if future else None


def self_issued_certificate(template: ExplicitCertificate, key: SigningKey,
                            issuer_label: bytes = b"rogue-ca") -> ExplicitCertificate:
    """An attacker mints its own certificate, for its own key, and signs it itself.

    `template` supplies the shape (validity, permissions, linkage, cracaId) so the forgery is
    plausible; the verification key is replaced with the attacker's, because a credential whose
    private key you do not hold is useless. The result is **internally perfect** — every field is
    well formed and the self-signature verifies — but its issuer is in no receiver's trust store,
    so `verify` returns `UNKNOWN_ISSUER`.

    This is the attack the boolean cannot express at all: today a fabricated certificate is
    indistinguishable from a real one, because there is nothing about a certificate to check.
    """
    tbs = replace(template.to_be_signed, verify_key_indicator=key.public_key().compressed())
    issuer = hashed_id8(issuer_label)
    unsigned = ExplicitCertificate(tbs, issuer=issuer, signature=b"\x00" * 64)
    return ExplicitCertificate(tbs, issuer=issuer, signature=key.sign(unsigned.signed_octets()))


#: Name -> what the attacker does, for the engine's attack switch. Every entry is a real operation
#: on real bytes; none of them sets a flag.
SIGNATURE_ATTACKS = {
    "InvalidSignature": "sign_with_foreign_key",
    "MessageTampering": "tamper_after_signing",
    "DataReplay": "replay",
    "CertificateGrafting": "graft_certificate",
    "ExpiredCert": "present_stale_credential",
    "NotYetValid": "present_future_credential",
    "ForgedCertificate": "self_issued_certificate",
}
