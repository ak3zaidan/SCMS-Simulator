"""`native_v1` -- today's engine representation, expressed as a CODEC rather than as a special case.

This exists so that "the engine has no wire format" stops being a hole in the abstraction and
becomes a *named, declared profile with an honest standards claim*. It is the default, it is
byte-identical in content to the MA-visible half of the broadcast dict `run.py` builds today, and
its `standards_claim()` says in as many words that it encodes nothing standardised.

Serialisation is `json.dumps(sort_keys=True, separators=(",", ":"))` -- the same canonicalisation
`scms_core.crypto_abstract.canonical_bytes` already uses for hashing, so a native PDU is stable
across runs, platforms and interpreter versions, and two processes that build the same claim
produce the same octets. Float round-tripping is exact: CPython's `repr`/`float()` pair is
round-trip exact for IEEE-754 doubles, which is why this profile can declare `lossless`.
"""
from __future__ import annotations

import json

from ..api.codec import (CAP_CAM, CAP_DENM, CAP_JSON, CAP_LOSSLESS, CAP_VAM, ENGINE_CONVENTIONS,
                         INTERFACE_VERSION, Claim, MessageCodecBase, SIGNER_FORMS, StationView)

#: The MA-visible fields a native PDU carries, in the order they are DECLARED on :class:`Claim`.
#: Sorted-key JSON makes the on-wire order alphabetical regardless; this tuple is the definition of
#: WHICH fields travel, not of their order.
NATIVE_FIELDS = (
    "station_id", "cert_digest", "msg_type", "gen_time", "x", "y", "speed", "heading",
    "pos_conf", "station_type", "msg_count", "event_type", "sig_ok", "cert_valid_from",
    "cert_valid_to", "accel", "length_m", "width_m", "sequence_number",
)

#: What the engine and the MOSAIC layer currently ASSUME a signed CAM weighs. `SignedCam.java` and
#: `Dcc.java` both hard-code 300 B, which is why every CBR estimate in the Java engine is computed
#: from a constant rather than from a message. `native_v1` reproduces that assumption exactly --
#: including its wrongness -- so that switching to the ETSI profile shows the difference instead of
#: hiding it.
LEGACY_SIGNED_CAM_BYTES = 300


class NativeV1Codec(MessageCodecBase):
    """The engine-private representation. Default, lossless, and standardised in nothing."""

    interface_version = INTERFACE_VERSION
    plugin_id = "native_v1"
    profile_id = "native_v1"

    def __init__(self, *, params=None, rng=None, env=None):
        self.params = dict(params or {})
        unknown = sorted(set(self.params) - {"legacy_wire_size_bytes"})
        if unknown:
            raise ValueError(f"native_v1 does not accept params {unknown}")
        self._legacy_size = int(self.params.get("legacy_wire_size_bytes",
                                                LEGACY_SIGNED_CAM_BYTES))

    @classmethod
    def from_plugin(cls, *, params=None, rng=None, env=None):
        return cls(params=params, rng=rng, env=env)

    # -- declarations ------------------------------------------------------------------ #
    def capabilities(self) -> frozenset:
        return frozenset({CAP_JSON, CAP_CAM, CAP_DENM, CAP_VAM, CAP_LOSSLESS})

    def standards_claim(self) -> dict:
        return {
            "message": "native_v1 -- engine-private representation; no ASN.1 encoding",
            "encoding": "canonical JSON (sorted keys, compact separators)",
            "units": "engine-private: deg CCW from East, local metres, m/s, float seconds",
            "asn1_module": None,
            "asn1_tag": None,
        }

    def conventions(self) -> dict:
        c = dict(ENGINE_CONVENTIONS)
        c["wire"] = "identical to the engine side -- this profile performs NO conversion"
        return c

    # -- encode / decode ---------------------------------------------------------------- #
    def _to_dict(self, claim: Claim) -> dict:
        return {f: getattr(claim, f) for f in NATIVE_FIELDS}

    def _encode(self, claim: Claim) -> bytes:
        return json.dumps(self._to_dict(claim), sort_keys=True,
                          separators=(",", ":")).encode("utf-8")

    def _decode(self, blob: bytes) -> Claim:
        d = json.loads(blob.decode("utf-8"))
        missing = [f for f in NATIVE_FIELDS if f not in d]
        if missing:
            raise ValueError(f"native_v1 PDU is missing {missing}")
        return Claim(**{f: d[f] for f in NATIVE_FIELDS})

    def encode_cam(self, claim: Claim, station: StationView) -> bytes:
        return self._encode(claim)

    def decode_cam(self, blob: bytes) -> Claim:
        return self._decode(blob)

    def encode_denm(self, claim: Claim, station: StationView) -> bytes:
        return self._encode(claim)

    def decode_denm(self, blob: bytes) -> Claim:
        return self._decode(blob)

    def encode_vam(self, claim: Claim, station: StationView) -> bytes:
        return self._encode(claim)

    def decode_vam(self, blob: bytes) -> Claim:
        return self._decode(blob)

    # -- sizing ------------------------------------------------------------------------- #
    def wire_size_bytes(self, claim: Claim, signer: str = "none") -> int:
        """The LEGACY constant, not the JSON length.

        Deliberate. This profile's job is to be what the engine does today, and what the engine
        does today is assume 300 B for a signed CAM regardless of content (`SignedCam.java`,
        `Dcc.java`). Returning `len(json)` would silently improve the airtime model and make the
        `native_v1` -> `etsi_*` comparison meaningless. `signer="none"` returns the actual
        serialised length, which is the only honest answer for an unsigned native PDU.
        """
        if signer not in SIGNER_FORMS:
            raise ValueError(f"signer must be one of {SIGNER_FORMS}, got {signer!r}")
        return len(self._encode(claim)) if signer == "none" else self._legacy_size

    def evidence_pdu(self, claim: Claim, station: StationView) -> bytes:
        """Serialised claim octets.

        Usable as evidence only in the loose sense: a TS 103 759 `v2xPduEvidence` entry is a
        `V2xPduStream` of the ACTUAL received PDUs, and an actual ITS-G5 PDU is not JSON. This
        method exists so the seam is uniform; the profile that can satisfy the standard is
        `etsi_cam_en302637_2`.
        """
        return self._encode(claim)
