"""The run-facing security layer: the object `run.py` holds when `security_model="ecdsa"`.

`provisioning.py` builds real butterfly credentials, `secured.py` signs and verifies real messages,
and `unlinkability.py` scores the result -- and until now nothing on the dataset path constructed
any of them. This module is the seam between those three and the engine loop, and it exists so that
`run.py` gains **one object and two call sites** rather than four hundred lines of PKI.

WHAT CHANGES ON THE WIRE
-----------------------
=============================  ==============================  ==============================
                               `security_model="none"` (default) `security_model="ecdsa"`
=============================  ==============================  ==============================
pseudonym key                  `sha256(f"key:{vid}:{k}")`      butterfly `d = a + f1(ck,i,j) + c`
`cert_digest`                  HashedId8 of that public blob   HashedId8 of a real 1609.2 cert
PCA's view of a device         one `request_hash` for ALL its  one opaque per-(device,i,j) token
                               pseudonyms -> perfectly linkable
`sig_ok` on the wire           a boolean the attack switch set the RESULT of an ECDSA verification
a revoked certificate          engine-side vid bookkeeping     recomputed linkage values, receiver-side
=============================  ==============================  ==============================

DETERMINISM AND RNG
-------------------
Not one random number is drawn here. Every key derives from `cfg.derive`, the PCA's per-request
randomiser derives from a PCA-private label, the RA's mixing queue is a keyed sort, and ECDSA runs
under RFC 6979 deterministic nonces (`ecdsa_p256.SIGNING_MODE`, probed at import). A run with this
layer on replays byte-for-byte, and a run with it off never imports the module at all -- `run.py`
imports it lazily, inside the `security_model != "none"` branch, so the ECDSA backend probe is not
paid by a default run.

THE COST, MEASURED, AND WHY THE CACHE IS NOT CHEATING
-----------------------------------------------------
One CAM is signed ONCE and verified by every receiver that hears it. On the 1188-vehicle InTAS run
that fan-out is 14.14, so the receive path's *logical* verification count is 14x its *computed*
one. `VerificationCache` memoises a pure function of (key, message, signature) and therefore cannot
change any verdict; `stats()` reports both numbers, and the LOGICAL one is what a CPU-exhaustion or
signature-flooding study must use. :meth:`SecurityLayer.stats` carries both into the manifest so no
consumer has to take the cheap number for the real one.
"""
from __future__ import annotations

import hashlib
from typing import Optional, Sequence

from .certificate import (PSID_CA_BASIC_SERVICE, PSID_DEN_BASIC_SERVICE, TIME_EPOCH_UNIX,
                          ExplicitCertificate)
from .ecdsa_p256 import SIGNING_MODE, SigningKey, VerificationCache
from .linkage import CrlLinkageEntry, DeviceLinkageContext
from .provisioning import ProvisionedDevice, PseudonymCredential, ScmsProvisioning
from .secured import (CERT_EXPIRED, CERT_NOT_YET_VALID, CERT_REVOKED, MessageSigner,
                      MessageVerifier, SIGNER_CERTIFICATE, SIGNER_DIGEST, SignedMessage,
                      graft_certificate, present_future_credential, present_stale_credential,
                      replay, self_issued_certificate, sign_with_foreign_key, time64)

#: PSID per message type -- `appPermissions` is only checkable because these differ.
PSID_BY_MSG_TYPE = {"cam": PSID_CA_BASIC_SERVICE, "denm": PSID_DEN_BASIC_SERVICE,
                    "vam": PSID_CA_BASIC_SERVICE}

#: TS 103 097 clause 7.1: a station attaches its certificate at least once per second, and always
#: in response to an unrecognised-certificate indication. Between those it signs with the 8-octet
#: digest. This is the alternation that makes `wire_size_bytes(signer=...)` a real parameter: the
#: certificate arm is 126 more octets on this engine's measured envelope (219 vs 93).
CERT_ATTACH_PERIOD_S = 1.0

#: UNIX seconds for engine `t = 0.0`. THE SAME INSTANT `api.codec.DEFAULT_EPOCH_UNIX` pins for the
#: codec's `generationDeltaTime` (2024-01-01T00:00:00Z), restated here rather than imported because
#: `scms_core` must not depend on `api`. Both layers stamping the same instant is what makes a
#: message's `generationDeltaTime` and its `HeaderInfo.generationTime` agree.
ENGINE_EPOCH_UNIX = 1_704_067_200

#: 1609.2 `Time32` for engine `t = 0.0`: seconds from the 2004 epoch to :data:`ENGINE_EPOCH_UNIX`.
#:
#: **This is not cosmetic, and it is not a wall clock.** `ScmsProvisioning.time_base` defaults to 0,
#: i.e. "engine second 0 IS the 1609.2 epoch". Under that mapping a BACKDATED claim -- which is
#: precisely what the `DelayedMessages` (`cg = t - 6.0`) and `OutOfOrder` (`cg = t - U(6, 12)`)
#: attacks emit -- lands before the epoch, and `Time64` is unsigned: `(-5_600_000).to_bytes(8,
#: "big")` raises `OverflowError` and the run dies at step 6. Two of the twenty-one catalog attacks
#: were unrunnable under real signing for exactly that reason, and the attack sweep is what found it.
#:
#: 631 152 000 is a PINNED CONSTANT, not `time.time()`: a wall clock here would make every dataset
#: unreproducible. It buys ~20 years of headroom below t = 0, so no attack this engine can express
#: can push a timestamp under it.
ENGINE_TIME_BASE = ENGINE_EPOCH_UNIX - TIME_EPOCH_UNIX


class SignedBroadcast:
    """One outgoing PDU, its wire size, and everything a receiver needs to check it.

    Carried on the engine's broadcast dict. `payload` is the codec's octets (a real UPER CAM when a
    codec is active, the engine's canonical native bytes otherwise); `wire_bytes` is what the
    channel must charge airtime for.
    """

    __slots__ = ("message", "payload", "wire_bytes", "signer_form", "certificate")

    def __init__(self, message: SignedMessage, payload: bytes, wire_bytes: int,
                 signer_form: str, certificate: Optional[ExplicitCertificate]):
        self.message = message
        self.payload = payload
        self.wire_bytes = int(wire_bytes)
        self.signer_form = signer_form
        self.certificate = certificate


class DeviceCredentials:
    """One station's provisioned pseudonyms plus the per-pseudonym signer objects.

    `MessageSigner` construction hashes the certificate and caches it, so it is built once per
    pseudonym and reused for every frame that pseudonym sends -- not once per message, which would
    re-hash ~250 octets of certificate on every CAM.
    """

    __slots__ = ("device", "signers", "attacker_key", "last_cert_attach")

    def __init__(self, device: ProvisionedDevice, attacker_key: Optional[SigningKey] = None):
        self.device = device
        self.signers = {c.digest: MessageSigner(c) for c in device.credentials}
        #: A key the device is NOT certified for. Only an attacker needs one; it is what makes
        #: `InvalidSignature` a real failed verification instead of a flag.
        self.attacker_key = attacker_key
        self.last_cert_attach: dict = {}

    @property
    def credentials(self) -> tuple:
        return self.device.credentials

    def credential(self, digest: str) -> Optional[PseudonymCredential]:
        for c in self.device.credentials:
            if c.digest == digest:
                return c
        return None

    def signer(self, digest: str) -> Optional[MessageSigner]:
        return self.signers.get(digest)


def widened(derive):
    """`cfg.derive` extended to any requested length, deterministically.

    `PipelineConfig.derive` is `sha256(f"{seed}|{label}").digest()[:n]`, so it CANNOT return more
    than 32 octets -- it silently returns 32 when asked for more. A butterfly caterpillar needs 64
    (`butterfly.new_caterpillar` refuses anything shorter, and rightly: `a`, `p`, `ck` and `ek` are
    four independent secrets). Rather than change `PipelineConfig.derive` -- which is what every
    existing label in the engine derives through, and therefore what every pinned digest rests on --
    this wraps it: short answers are extended by chaining additional labelled derivations. The
    labels it invents (`<label>|x0`, `|x1`, ...) exist nowhere else in the engine, so no stream
    collides, and the result is a pure function of the seed.
    """
    def d(label: str, nbytes: int = 32) -> bytes:
        out = derive(label, nbytes)
        if len(out) >= nbytes:
            return out[:nbytes]
        parts, i = [out], 0
        n = len(out)
        while n < nbytes:
            chunk = derive(f"{label}|x{i}", 32)
            parts.append(chunk)
            n += len(chunk)
            i += 1
        return b"".join(parts)[:nbytes]
    return d


class SecurityLayer:
    """Provisioning + signing + verification for one run.

    Constructed once, before step 0. `provision` is called per device from `make_vehicle`;
    `sign` per outgoing PDU; `verify` per delivered PDU.
    """

    def __init__(self, derive, *, time_base: int = ENGINE_TIME_BASE, jmax: int = 20,
                 crl: Optional[Sequence[CrlLinkageEntry]] = None,
                 cert_attach_period_s: float = CERT_ATTACH_PERIOD_S):
        derive = widened(derive)
        self.prov = ScmsProvisioning(derive=derive, time_base=time_base)
        self._derive = derive
        self.cert_attach_period_s = float(cert_attach_period_s)
        self.devices: dict = {}
        #: Held BY REFERENCE. `run.py` appends to its CRL list mid-run; a verifier handed a snapshot
        #: would keep accepting a revoked attacker for the rest of the run.
        self.crl = crl if crl is not None else []
        self.verifier = MessageVerifier(self.prov.trust_store(), cache=VerificationCache(),
                                        crl=self.crl, jmax=jmax)
        self.n_signed = 0
        self.n_sign_calls = 0
        #: Certificates learned by the receiver-side store, so the `digest` signer arm resolves.
        self._learned: set = set()

    # -- provisioning ------------------------------------------------------------------------- #
    def provision(self, device_label: str, linkage_ctx: DeviceLinkageContext,
                  windows: Sequence[tuple], *, attacker: bool = False) -> DeviceCredentials:
        """Provision one device. `windows` is `[(i, j, valid_from_s, valid_to_s)]` -- exactly the
        tuple `run.py` already computes for its pseudonym loop."""
        dev = self.prov.provision(device_label=device_label, linkage_ctx=linkage_ctx,
                                  windows=list(windows))
        key = None
        if attacker:
            # A key this device holds and NO PCA ever certified. Derived, never drawn.
            key = SigningKey(int.from_bytes(
                hashlib.sha256(self._derive(f"rogue:{device_label}", 32)).digest(), "big"))
        creds = DeviceCredentials(dev, key)
        self.devices[device_label] = creds
        # EVERY receiver is expected to know the CA chain but not every pseudonym certificate, so
        # certificates are learned here (the run's whole fleet is provisioned before step 0) rather
        # than only on a certificate-attached frame. That models a warm certificate cache and keeps
        # `CERT_UNAVAILABLE` for the case it really means: a certificate nobody ever issued.
        for c in dev.credentials:
            self.verifier.learn(c.certificate)
            self._learned.add(c.digest)
        return creds

    def digests_for(self, device_label: str) -> tuple:
        d = self.devices.get(device_label)
        return d.device.digests if d is not None else ()

    # -- signing ------------------------------------------------------------------------------ #
    def _attach_certificate(self, creds: DeviceCredentials, digest: str, t: float) -> bool:
        """TS 103 097 signer alternation: attach at most once per `cert_attach_period_s`."""
        last = creds.last_cert_attach.get(digest)
        if last is None or (t - last) + 1e-9 >= self.cert_attach_period_s:
            creds.last_cert_attach[digest] = t
            return True
        return False

    def sign(self, device_label: str, digest: str, payload: bytes, t: float, *,
             msg_type: str = "cam", attack: str = "", claimed_gen_time: Optional[float] = None
             ) -> Optional[SignedBroadcast]:
        """Sign one PDU. Returns None when the device holds no such pseudonym.

        `attack` selects an attacker BEHAVIOUR, never a flag: see `secured.SIGNATURE_ATTACKS`. Each
        one produces a well-formed `SignedMessage`; what differs is what `verify` concludes.
        `claimed_gen_time` is the timestamp the sender PUTS IN THE HEADER, which for an honest
        station is `t` and for a replay/delay attacker is the stale value it wants believed.
        """
        creds = self.devices.get(device_label)
        if creds is None:
            return None
        signer = creds.signer(digest)
        if signer is None:
            return None
        gen_t = t if claimed_gen_time is None else float(claimed_gen_time)
        gt64 = time64(gen_t, self.prov.time_base)
        if gt64 < 0:
            # Unreachable with ENGINE_TIME_BASE's 20 years of headroom, and a NAMED refusal rather
            # than the `OverflowError: can't convert negative int to unsigned` that a zero time_base
            # produced eight frames into a `DelayedMessages` run.
            raise ValueError(
                f"claimed generation time {gen_t} s maps to Time64 {gt64}, which is before the "
                f"1609.2 epoch. Raise SecurityLayer(time_base=...) above "
                f"{int(-gen_t) + 1} so backdating attacks stay representable.")
        psid = PSID_BY_MSG_TYPE.get(msg_type, PSID_CA_BASIC_SERVICE)
        attach = self._attach_certificate(creds, digest, t)
        self.n_sign_calls += 1
        if attack == "InvalidSignature" and creds.attacker_key is not None:
            msg = sign_with_foreign_key(signer, creds.attacker_key, payload, gt64,
                                        include_certificate=attach, psid=psid)
        elif attack == "ForgedCertificate":
            # A certificate the attacker minted for its own key. Internally perfect, issued by
            # nobody in any trust store -> UNKNOWN_ISSUER, an outcome the boolean cannot express.
            key = creds.attacker_key or signer._key                      # noqa: SLF001
            rogue = self_issued_certificate(signer.certificate, key)
            rogue_signer = MessageSigner.from_parts(rogue, key, psid=psid)
            msg = rogue_signer.sign(payload, gt64, include_certificate=True, psid=psid)
        elif attack == "CertificateGrafting":
            # A valid signature re-presented under someone else's certificate. Defeated by the
            # 1609.2 double hash in `signature_input`; included so the regression is live.
            other = self._other_certificate(device_label, digest)
            msg = signer.sign(payload, gt64, include_certificate=True, psid=psid)
            if other is not None:
                msg = graft_certificate(msg, other)
        else:
            msg = signer.sign(payload, gt64, include_certificate=attach, psid=psid)
        self.n_signed += 1
        return SignedBroadcast(msg, payload, len(msg.wire_octets()),
                               SIGNER_CERTIFICATE if msg.certificate is not None else SIGNER_DIGEST,
                               msg.certificate)

    def _other_certificate(self, device_label: str, digest: str) -> Optional[ExplicitCertificate]:
        """Any certificate that is not this one -- the graft victim."""
        for label, creds in self.devices.items():
            if label == device_label:
                continue
            for c in creds.device.credentials:
                return c.certificate
        creds = self.devices.get(device_label)
        if creds is not None:
            for c in creds.device.credentials:
                if c.digest != digest:
                    return c.certificate
        return None

    def replay_of(self, sb: SignedBroadcast, *, rewrite_gen_time: Optional[float] = None
                  ) -> SignedBroadcast:
        """A captured frame re-emitted verbatim.

        The signature is VALID -- a legitimate key made it over exactly these bytes -- and that is
        the correct model: a replay is a freshness failure caught by `staleOrReplay`, never a crypto
        failure. `rewrite_gen_time` is the weaker variant that a signature DOES defeat, kept as the
        contrast case.
        """
        at = None if rewrite_gen_time is None else time64(rewrite_gen_time, self.prov.time_base)
        msg = replay(sb.message, at_generation_time=at)
        return SignedBroadcast(msg, sb.payload, len(msg.wire_octets()), sb.signer_form,
                               sb.certificate)

    # -- credential-presentation attacks ------------------------------------------------------ #
    def stale_credential(self, device_label: str, now_s: float) -> Optional[PseudonymCredential]:
        """The honest `ExpiredCert`: a certificate the device REALLY HAS whose window has passed.

        Returns None when the device holds none -- an attacker in its first rotation period
        genuinely cannot mount this attack, which is a truer model than letting every attacker edit
        `cvt` on the wire. `run.py` falls back to leaving the attacker honest that step and counts
        the refusal, so the loss is measured rather than hidden.
        """
        creds = self.devices.get(device_label)
        return None if creds is None else present_stale_credential(creds.credentials, now_s)

    def future_credential(self, device_label: str, now_s: float) -> Optional[PseudonymCredential]:
        """The honest `NotYetValid`. Available whenever the device was provisioned more than one
        rotation ahead, which under `rotate_period_s > 0` it always is."""
        creds = self.devices.get(device_label)
        return None if creds is None else present_future_credential(creds.credentials, now_s)

    # -- verification ------------------------------------------------------------------------- #
    def verify(self, sb: SignedBroadcast, t: float):
        """Verify a delivered PDU at receiver time `t`. Returns `secured.VerificationResult`.

        `now` is the RECEIVER's clock, not the message's own `generationTime`: a receiver that
        validated a certificate window against a timestamp the sender chose would accept an expired
        certificate from any sender willing to lie about the time, which is precisely the
        `ExpiredCert` attack.
        """
        return self.verifier.verify(sb.message, now=self.prov.time32(t))

    def stats(self) -> dict:
        d = dict(self.verifier.stats())
        d.update(signatures_computed=self.n_signed, sign_calls=self.n_sign_calls,
                 signing_mode=SIGNING_MODE,
                 devices_provisioned=len(self.devices),
                 certificates_issued=sum(len(c.device.credentials) for c in self.devices.values()))
        return d


#: Status values that mean "the certificate itself was refused", i.e. the engine's `cert_bad`.
CERT_REJECT_STATUSES = frozenset({CERT_EXPIRED, CERT_NOT_YET_VALID, CERT_REVOKED})

__all__ = ["CERT_ATTACH_PERIOD_S", "CERT_REJECT_STATUSES", "DeviceCredentials", "ENGINE_EPOCH_UNIX",
           "ENGINE_TIME_BASE", "PSID_BY_MSG_TYPE", "SecurityLayer", "SignedBroadcast", "widened"]
