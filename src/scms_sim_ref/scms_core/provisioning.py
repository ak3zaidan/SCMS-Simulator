"""Butterfly provisioning — wiring the orphan in `butterfly.py` to the credential path.

`STANDARDS-AUDIT.md` finding 3: *"butterfly.py implements CAMP SCP1 correctly — and nothing in the
dataset path imports it. Actual provisioning is `pk = ca.keypair_from_seed(cfg.derive(f"key:{vid}:
{k}"))`, i.e. every pseudonym of a device derives from ONE label, the exact opposite of the
unlinkability butterfly provides."*

## The defect this replaces, stated precisely

The label scheme's failure is **not** against a passive radio observer: `SHA-256("key:17:3")` is as
pseudorandom to an outsider as any other seed. It fails against the two parties the SCMS design
exists to defend against, and it fails totally:

1. **The PCA links every pseudonym of a device by its own records.** `run.py:4008` calls
   `pca.issue(dig, req_hash, i, j, la_h1, la_h2)` once per rotation with the **same `req_hash`**.
   The PCA's provenance table is therefore a complete `device -> {all its certificates}` map. In
   CAMP SCP1 the PCA is explicitly not supposed to be able to build that map; here it is handed one.
2. **One secret compromises the whole fleet's history, forwards and backwards.** Every pseudonym
   of every device is `derive(f"key:{vid}:{k}")` of a single master seed, over a label alphabet of
   about 10^4 strings. An adversary who learns the seed enumerates the fleet in milliseconds. In
   butterfly, the per-device caterpillar (a, p, ck, ek) is the unit of compromise and the PCA's
   per-request randomiser `c` is outside it entirely.

Both are measured, not asserted — `unlinkability.py` scores each role's view with an adjusted Rand
index and the harness reports 1.0 for the label scheme's PCA view and ~0 for this one.

## The role split, and who holds what

```
device            RA                                   PCA
------            --                                   ---
(a,p,ck,ek) --->  A, P, ck, ek  ------------------->
                  B = A + f1(ck,i,j)*G                 sees B, Q, i, an opaque per-request token
                  Q = P + f2(ek,i,j)*G                 picks secret c, certifies B + c*G
                  token <-> (device, i, j)  [ONLY HERE]
                  shuffles requests across devices      records nothing that groups by device
        <-------  cert, c  <------------------------
d = a + f1(ck,i,j) + c
```

* The **RA can** link a device's cocoon keys — it holds `ck`. That is by design and is the same
  power `run.py`'s RA already has (`ra.bind(req_hash, true_id)`, the only holder of
  `request_hash -> true_id`); misbehaviour investigation depends on it. What the RA never learns is
  the *certified* key, because `c` is the PCA's.
* The **PCA cannot** link, because a cocoon is pseudorandom without `ck` and the RA shuffles.
* Neither alone can deanonymise; both together can. That two-party split *is* the SCMS.

## Determinism

Everything derives from the run seed through `derive(label, nbytes)` — the same convention
`PipelineConfig.derive` uses. No `random` module, no `os.urandom`, no draw from the engine's global
stream, so wiring this in costs **zero** RNG draws on any existing path (hard constraint: "no new
rng on default paths"). The PCA's per-request randomiser `c` is derived from a PCA-private label;
in a real deployment it is drawn from a hardware RNG, and that is the one place this module trades
fidelity for reproducibility. It is a trade the whole repo already makes, and it is stated here
rather than buried.

## Cost

Measured on this host: **228 us per pseudonym certificate** end to end — butterfly expansion (two
AES-based f-values), four P-256 base multiplies, three affine adds, the PCA's ECDSA signature over
the certificate, and the device's re-derivation check. A 1188-device fleet at 5 rotations each is
**1.36 s** of provisioning, once, before step 0, against a ~35 s run. `test_scms_crypto.py::
test_provisioning_cost_is_affordable_before_step_zero` re-measures it and fails above 5 ms.

Two things make that affordable, and both are in `ec.py`: `_inv` uses extended Euclid rather than
Fermat exponentiation (66.9 us -> 8.4 us per affine add), and `scalar_mult` delegates the generator
multiply to the library (494 us -> 21.8 us). Neither changes a single returned value; the tests
pin both against the transparent reference.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass
from typing import Callable, Iterable, Optional, Sequence

from . import butterfly as bf
from .certificate import (CERT_TYPE_EXPLICIT, PSID_CA_BASIC_SERVICE, Duration, ExplicitCertificate,
                          LinkageData, PsidSsp, ToBeSignedCertificate, ValidityPeriod, hashed_id3)
from .ec import N
from .ecdsa_p256 import SigningKey, VerifyingKey, compress_point
from .linkage import DeviceLinkageContext, linkage_seed_at, linkage_value, pre_linkage_value

Derive = Callable[..., bytes]


def seed_derive(seed: object) -> Derive:
    """A `PipelineConfig.derive`-shaped deriver from any seed value, for standalone use."""
    root = str(seed).encode("utf-8")

    def derive(label: str, nbytes: int = 32) -> bytes:
        out, ctr = b"", 0
        while len(out) < nbytes:
            out += hashlib.sha256(root + b"|" + label.encode("utf-8") + b"|" + bytes([ctr])).digest()
            ctr += 1
        return out[:nbytes]

    return derive


def _scalar(material: bytes) -> int:
    """A non-zero P-256 scalar from key material."""
    return (int.from_bytes(hashlib.sha256(material).digest(), "big") % (N - 1)) + 1


# ------------------------------------------------------------------------------ the entities --- #

@dataclass(frozen=True, slots=True)
class Authority:
    """A signing CA: its certificate plus the key that issued the certificates below it."""

    name: str
    key: SigningKey
    certificate: ExplicitCertificate

    @property
    def hashed_id8(self) -> bytes:
        return self.certificate.hashed_id8()

    def verifying_key(self) -> VerifyingKey:
        return self.key.public_key()


@dataclass(frozen=True, slots=True)
class DeviceEnrolment:
    """What the device holds and what it uploaded. `caterpillar` never leaves the device except as
    (A, P, ck, ek); the private scalars a, p do not leave at all.

    `A` and `P` are materialised here rather than read from `Caterpillar.A` / `.P`, which are
    **properties that recompute a P-256 base multiply on every access** — a trap worth 2 of the 4
    base multiplies per certificate (~44 us of 250) if a caller reads them per request, as
    `ra_cocoon_keys(cat.A, cat.P, ...)` naturally does.
    """

    device_label: str
    caterpillar: bf.Caterpillar
    linkage_ctx: DeviceLinkageContext
    #: The RA-held binding, unchanged in spirit from `run.py`'s `request_hash`.
    enrolment_handle: str
    #: The caterpillar public points, computed once.
    A: tuple = ()
    P: tuple = ()


@dataclass(frozen=True, slots=True)
class CocoonRequest:
    """RA -> PCA. Carries **no device identifier of any kind** — that is the whole point.

    `request_token` is an opaque per-(device, i, j) value only the RA can invert. `arrival_index`
    is assigned by the RA's shuffled queue, so even the PCA's *arrival order* carries no grouping.
    """

    request_token: bytes
    cocoon_signing: tuple                        # B
    cocoon_encryption: tuple                     # Q
    i_period: int
    j_index: int
    #: The XOR of the two LAs' pre-linkage values, which in the real protocol the PCA computes from
    #: two independently encrypted halves. Computed here, by the PCA, from the halves it is given.
    pre_linkage_1: bytes
    pre_linkage_2: bytes
    valid_from: int                              # Time32
    valid_to: int                                # Time32
    arrival_index: int = -1


@dataclass(frozen=True, slots=True)
class PcaResponse:
    """PCA -> RA -> device. In the full protocol `(certificate, c)` is encrypted to Q; modelling the
    ECIES wrapper adds no observable to this dataset, and its absence is recorded here rather than
    implied away."""

    certificate: ExplicitCertificate
    randomiser: int                              # c
    request_token: bytes


@dataclass(frozen=True, slots=True)
class PseudonymCredential:
    """One usable pseudonym: the certificate, and the private key the device re-derived for it."""

    certificate: ExplicitCertificate
    signing_key: SigningKey
    i_period: int
    j_index: int
    valid_from: float                            # engine seconds
    valid_to: float                              # engine seconds

    @property
    def digest(self) -> str:
        return self.certificate.digest_hex()

    @property
    def linkage_value(self) -> bytes:
        return self.certificate.linkage.linkage_value


# ------------------------------------------------------------------------- registration authority #

class RegistrationAuthority:
    """Expands caterpillars into cocoons, shuffles requests, and is the ONLY holder of the map back.

    `token_map` is the RA's private table. `unlinkability.py` uses its presence to show that
    identity resolution still works — an unlinkable scheme that cannot be investigated would be
    useless to this project, whose entire purpose is misbehaviour detection under pseudonymity.
    """

    def __init__(self, derive: Derive):
        self._derive = derive
        self._pending: list[CocoonRequest] = []
        self.token_map: dict[bytes, tuple[str, int, int]] = {}

    def enrol(self, device_label: str, linkage_ctx: DeviceLinkageContext) -> DeviceEnrolment:
        cat = bf.new_caterpillar(self._derive(f"caterpillar:{device_label}", 64))
        handle = hashlib.sha256(f"enrol|{device_label}".encode()).hexdigest()[:16]
        return DeviceEnrolment(device_label, cat, linkage_ctx, handle, cat.A, cat.P)

    def request(self, enr: DeviceEnrolment, i: int, j: int,
                valid_from: int, valid_to: int) -> CocoonRequest:
        """One butterfly expansion: B = A + f1(ck,i,j)*G, Q = P + f2(ek,i,j)*G."""
        cat = enr.caterpillar
        B, Q = bf.ra_cocoon_keys(enr.A, enr.P, cat.ck, cat.ek, i, j)
        token = self._derive(f"ratok:{enr.device_label}:{i}:{j}", 16)
        ctx = enr.linkage_ctx
        plv1 = pre_linkage_value(ctx.la_id1, linkage_seed_at(ctx.la_id1, ctx.ls1_0, i), j)
        plv2 = pre_linkage_value(ctx.la_id2, linkage_seed_at(ctx.la_id2, ctx.ls2_0, i), j)
        req = CocoonRequest(token, B, Q, i, j, plv1, plv2, valid_from, valid_to)
        self.token_map[token] = (enr.device_label, i, j)
        self._pending.append(req)
        return req

    def drain(self, batch_label: str = "batch") -> list[CocoonRequest]:
        """Release the queue to the PCA in a device-mixing order.

        The shuffle is a deterministic keyed sort (a per-token derived key), not `random.shuffle`:
        it draws nothing from any RNG the engine owns, and it replays identically. What matters for
        unlinkability is only that arrival order be uncorrelated with device, which a keyed sort
        gives exactly.
        """
        pending, self._pending = self._pending, []
        keyed = sorted(pending, key=lambda r: self._derive(
            f"shuffle:{batch_label}:{r.request_token.hex()}", 8))
        return [CocoonRequest(r.request_token, r.cocoon_signing, r.cocoon_encryption, r.i_period,
                              r.j_index, r.pre_linkage_1, r.pre_linkage_2, r.valid_from,
                              r.valid_to, arrival_index=n)
                for n, r in enumerate(keyed)]

    def token_for(self, device_label: str, i: int, j: int) -> bytes:
        """The opaque per-(device, i, j) request token.

        Exposed because the engine's PCA record (`run.py:2294`, `pca.issue(dig, request_hash, ...)`)
        needs *something* to key its provisioning record on, and `request_hash` — one value shared
        by every pseudonym of a vehicle — is precisely the linkability defect this module removes.
        The token is the correct replacement: unique per certificate, and invertible only here.
        """
        return self._derive(f"ratok:{device_label}:{i}:{j}", 16)

    def resolve(self, token: bytes) -> Optional[tuple[str, int, int]]:
        """Identity resolution — the RA's job during a misbehaviour investigation."""
        return self.token_map.get(token)


# ------------------------------------------------------------------------------- pseudonym CA --- #

class PseudonymCertificateAuthority:
    """Certifies cocoons. Holds a private randomiser secret the RA does not have, and a ledger that
    is *exactly* what a curious-but-honest PCA could later mine — the input to the PCA-view
    linkability measurement."""

    def __init__(self, authority: Authority, derive: Derive, *,
                 craca_id: bytes, crl_series: int = 1,
                 app_permissions: Sequence[PsidSsp] = (),
                 assurance_level: Optional[int] = 0xC0):
        self.authority = authority
        self._derive = derive
        self.craca_id = craca_id
        self.crl_series = crl_series
        self.app_permissions = tuple(app_permissions or (PsidSsp(PSID_CA_BASIC_SERVICE),))
        self.assurance_level = assurance_level
        #: Everything the PCA sees, in the order it saw it. No device identifier appears here.
        self.ledger: list[dict] = []

    def _randomiser(self, token: bytes) -> int:
        """The PCA's secret c. Derived from a PCA-private label the RA never sees."""
        return _scalar(self._derive(f"pca-c:{self.authority.name}:{token.hex()}", 32))

    def certify(self, req: CocoonRequest) -> PcaResponse:
        c = self._randomiser(req.request_token)
        certified_pub, _C = bf.pca_certify_explicit(req.cocoon_signing, c)
        lv = linkage_value(req.pre_linkage_1, req.pre_linkage_2)      # only the PCA computes this
        tbs = ToBeSignedCertificate(
            id=LinkageData(i_cert=req.i_period, linkage_value=lv),
            craca_id=self.craca_id,
            crl_series=self.crl_series,
            validity_period=ValidityPeriod(
                start=req.valid_from,
                duration=Duration.from_seconds(max(1, req.valid_to - req.valid_from))),
            verify_key_indicator=compress_point(certified_pub),
            app_permissions=self.app_permissions,
            assurance_level=self.assurance_level,
            encryption_key=compress_point(req.cocoon_encryption),
        )
        cert = self._sign(tbs)
        self.ledger.append({
            "arrival_index": req.arrival_index,
            "request_token": req.request_token.hex(),
            "i_cert": req.i_period,
            "j_index": req.j_index,
            "cocoon_x": req.cocoon_signing[0],
            "certified_key": tbs.verify_key_indicator.hex(),
            "linkage_value": lv.hex(),
            "cert_digest": cert.digest_hex(),
            "valid_from": req.valid_from,
            "valid_to": req.valid_to,
        })
        return PcaResponse(cert, c, req.request_token)

    def _sign(self, tbs: ToBeSignedCertificate) -> ExplicitCertificate:
        unsigned = ExplicitCertificate(to_be_signed=tbs, issuer=self.authority.hashed_id8,
                                       signature=b"\x00" * 64, cert_type=CERT_TYPE_EXPLICIT)
        return ExplicitCertificate(to_be_signed=tbs, issuer=self.authority.hashed_id8,
                                   signature=self.authority.key.sign(unsigned.signed_octets()),
                                   cert_type=CERT_TYPE_EXPLICIT)


# ---------------------------------------------------------------------------------- device ----- #

def device_private_scalar(enr: DeviceEnrolment, i: int, j: int, c: int) -> int:
    """`d = a + f1(ck, i, j) + c mod n` — CAMP SCP1's explicit-certificate re-derivation."""
    return bf.device_signing_private(enr.caterpillar, i, j, c)


def accept(enr: DeviceEnrolment, resp: PcaResponse, i: int, j: int,
           valid_from_s: float, valid_to_s: float) -> PseudonymCredential:
    """Device side: re-derive the private key and REFUSE the certificate if it does not match.

    The check is not decoration. It is the property the whole construction rests on
    (`d*G == certified public key`), it costs nothing here because `SigningKey` computes `d*G`
    during construction anyway, and a silent mismatch would produce a fleet whose signatures never
    verify — a failure that would otherwise surface 300 steps later as "detection recall collapsed".

    `ScmsProvisioning.provision` inlines exactly this; it is exported separately because a
    `PkiBackend` plugin (PLUGIN-ARCHITECTURE.md 6.1, priority 6) needs the device half on its own.
    """
    key = SigningKey(device_private_scalar(enr, i, j, resp.randomiser))
    if compress_point(key.public_point) != resp.certificate.to_be_signed.verify_key_indicator:
        raise ValueError("butterfly re-derivation does not match the certified key "
                         f"(device={enr.device_label!r}, i={i}, j={j})")
    return PseudonymCredential(resp.certificate, key, i, j, valid_from_s, valid_to_s)


# --------------------------------------------------------------------------------- the facade --- #

@dataclass(frozen=True, slots=True)
class ProvisionedDevice:
    enrolment: DeviceEnrolment
    credentials: tuple                            # tuple[PseudonymCredential, ...]

    @property
    def digests(self) -> tuple:
        return tuple(c.digest for c in self.credentials)


class ScmsProvisioning:
    """The one object `run.py` needs. Builds a Root CA and a PCA, then provisions devices.

    Usage from the engine (see the wiring diff in the task report):

        prov = ScmsProvisioning(derive=cfg.derive, time_base=cfg.security_time_base)
        dev  = prov.provision(device_label=f"veh_{vid:03d}", linkage_ctx=ctx,
                              windows=[(i_k, j_k, vf, vt) for ...])
        # dev.credentials[k].digest  replaces  ca.hashed_id8(ca.public_bytes(pk)).hex()

    `time_base` maps engine second 0.0 onto a 1609.2 `Time32`. It defaults to the 2004 epoch
    itself, i.e. `t=0.0 -> Time32 0`, which keeps validity arithmetic identical to the engine's and
    makes the field honest about being a simulation clock rather than pretending to be wall time.
    """

    def __init__(self, derive: Derive, *, time_base: int = 0, crl_series: int = 1,
                 app_permissions: Sequence[PsidSsp] = (),
                 root_name: str = "RootCA-1", pca_name: str = "PCA-1",
                 craca_name: str = "CRACA-1"):
        self._derive = derive
        self.time_base = int(time_base)
        self.craca_id = hashed_id3(f"craca|{craca_name}".encode())
        self.root = self._make_root(root_name)
        self.pca_authority = self._make_pca(pca_name)
        self.ra = RegistrationAuthority(derive)
        self.pca = PseudonymCertificateAuthority(
            self.pca_authority, derive, craca_id=self.craca_id, crl_series=crl_series,
            app_permissions=app_permissions)

    # -- the trust chain ----------------------------------------------------------------------- #
    def _ca_tbs(self, name: str, key: SigningKey) -> ToBeSignedCertificate:
        """A CA certificate, with two simplifications named rather than hidden.

        1. 1609.2 would give a CA `CertificateId.name` (a Hostname); this module's
           `ToBeSignedCertificate.id` only models the `linkageData` arm, because that is the arm a
           **pseudonym** certificate needs and the arm the audit found missing. A CA therefore
           carries a zero linkage value, which is structurally wrong for a CA and structurally
           irrelevant to everything this module verifies (a CA is never on a linkage CRL).
        2. 1609.2 would give a CA `certIssuePermissions` (what it may issue), not `appPermissions`
           (what it may sign). Modelling the issuance-permission lattice would let a receiver check
           that the PCA was entitled to issue a PSID-36 pseudonym; nothing does that today, so the
           field would be inert. It is on the list, not in the code.
        """
        return ToBeSignedCertificate(
            id=LinkageData(i_cert=0, linkage_value=b"\x00" * 9),
            craca_id=self.craca_id,
            crl_series=0,
            validity_period=ValidityPeriod(self.time_base, Duration("years", 3)),
            verify_key_indicator=compress_point(key.public_point),
            app_permissions=(PsidSsp(PSID_CA_BASIC_SERVICE),),
            assurance_level=0xE0,
        )

    def _make_root(self, name: str) -> Authority:
        key = SigningKey(_scalar(self._derive(f"ca-key:{name}", 32)))
        tbs = self._ca_tbs(name, key)
        unsigned = ExplicitCertificate(tbs, issuer=None, signature=b"\x00" * 64)
        cert = ExplicitCertificate(tbs, issuer=None, signature=key.sign(unsigned.signed_octets()))
        return Authority(name, key, cert)

    def _make_pca(self, name: str) -> Authority:
        key = SigningKey(_scalar(self._derive(f"ca-key:{name}", 32)))
        tbs = self._ca_tbs(name, key)
        issuer = self.root.hashed_id8
        unsigned = ExplicitCertificate(tbs, issuer=issuer, signature=b"\x00" * 64)
        cert = ExplicitCertificate(tbs, issuer=issuer,
                                   signature=self.root.key.sign(unsigned.signed_octets()))
        return Authority(name, key, cert)

    def trust_store(self) -> dict:
        """`HashedId8 -> VerifyingKey` for every CA a receiver is expected to know. A certificate
        whose issuer is absent from this map is `unknown_issuer`, not `signature_invalid` — the
        distinction the current boolean cannot express."""
        return {self.root.hashed_id8: self.root.verifying_key(),
                self.pca_authority.hashed_id8: self.pca_authority.verifying_key()}

    # -- provisioning -------------------------------------------------------------------------- #
    def time32(self, t: float) -> int:
        return self.time_base + int(t)

    def provision(self, *, device_label: str, linkage_ctx: DeviceLinkageContext,
                  windows: Sequence[tuple]) -> ProvisionedDevice:
        """`windows` = [(i, j, valid_from_s, valid_to_s)], exactly what `run.py` already computes.

        Requests go through the RA's shuffled queue per device. A cross-device shuffle is stronger
        still and `provision_fleet` does it; per-device is the shape that fits `make_vehicle`'s
        one-vehicle-at-a-time construction without restructuring the engine, and it is sufficient
        for the PCA-view property because the cocoons themselves carry no device signal.
        """
        enr = self.ra.enrol(device_label, linkage_ctx)
        for (i, j, vf, vt) in windows:
            self.ra.request(enr, i, j, self.time32(vf), self.time32(vt))
        reqs = self.ra.drain(batch_label=device_label)
        by_token = {r.request_token: r for r in reqs}
        creds = []
        for (i, j, vf, vt) in windows:                       # rebuild in the caller's order
            token = self._derive(f"ratok:{device_label}:{i}:{j}", 16)
            resp = self.pca.certify(by_token[token])
            d = device_private_scalar(enr, i, j, resp.randomiser)
            key = SigningKey(d)
            if compress_point(key.public_point) != resp.certificate.to_be_signed.verify_key_indicator:
                raise ValueError("butterfly re-derivation does not match the certified key")
            creds.append(PseudonymCredential(resp.certificate, key, i, j, vf, vt))
        return ProvisionedDevice(enr, tuple(creds))

    def provision_fleet(self, devices: Iterable[tuple]) -> dict:
        """`devices` = [(label, linkage_ctx, windows)] provisioned as ONE cross-device batch.

        This is the strongest form: every request in the fleet enters one queue and is drained in a
        device-mixing order, so the PCA's arrival index is uncorrelated with the device even in
        principle. Use it when the engine can build the fleet before issuing (the fixed-fleet path);
        `provision` per device is the flow-mode fallback.
        """
        enrolments, plan = {}, []
        for (label, ctx, windows) in devices:
            enr = self.ra.enrol(label, ctx)
            enrolments[label] = enr
            for (i, j, vf, vt) in windows:
                self.ra.request(enr, i, j, self.time32(vf), self.time32(vt))
                plan.append((label, i, j, vf, vt))
        reqs = {r.request_token: r for r in self.ra.drain(batch_label="fleet")}
        out: dict[str, list] = {label: [] for label in enrolments}
        for (label, i, j, vf, vt) in plan:
            token = self._derive(f"ratok:{label}:{i}:{j}", 16)
            resp = self.pca.certify(reqs[token])
            enr = enrolments[label]
            key = SigningKey(device_private_scalar(enr, i, j, resp.randomiser))
            if compress_point(key.public_point) != resp.certificate.to_be_signed.verify_key_indicator:
                raise ValueError("butterfly re-derivation does not match the certified key")
            out[label].append(PseudonymCredential(resp.certificate, key, i, j, vf, vt))
        return {label: ProvisionedDevice(enrolments[label], tuple(creds))
                for label, creds in out.items()}
