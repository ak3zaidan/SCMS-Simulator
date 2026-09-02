"""`MessageCodec` -- the standards-profile seam (PLUGIN-ARCHITECTURE.md section 2.5).

The engine keeps its private representation; the WIRE FORMAT becomes swappable and testable
against the real ASN.1 modules. That is the whole point: `STANDARDS-AUDIT.md` finding 2 is that
**nothing in this repository is byte-encodable** -- "a CAM is a bare dict built at one site" -- and
without an encoder no interoperability claim is testable even in principle.

**A standards profile is also a unit-conversion layer, and that is the substance, not a detail.**
The engine is degrees CCW from East / local metres / m/s / float seconds (`manifest.conventions`,
`run.py`). ETSI is 0.1 degree CW from North / WGS84 in 1/10 microdegree / 0.01 m/s /
`generationDeltaTime` as milliseconds mod 65536 since the 2004 epoch. :meth:`MessageCodec.conventions`
is what makes that explicit rather than implicit, and every conversion is quantised -- so a codec
that claims to round-trip must be able to say, per field, how much error it introduces. See
:mod:`scms_sim_ref.codecs.units`, which measures it rather than asserting it.

This module is **stdlib only**, like the rest of :mod:`scms_sim_ref.api`. It defines the contract
and the frame arithmetic; it never imports an ASN.1 library. `asn1tools` is an OPTIONAL extra
(``pip install scms-sim-ref[asn1]``) and the engine imports and runs without it.

THE FIREWALL APPLIES HERE TOO
-----------------------------
:class:`Claim` is the codec's input and it is built to the same rule as
:class:`~scms_sim_ref.api.detect.Observation`: **MA-visible fields only**. It carries the CLAIMED
kinematics -- what the sender put on the wire -- never `veh`, `x`, `y` (true position), `falsified`,
`ghost`, `tspd` or `thdg`. A codec that could see ground truth would be a laundering vector into
every encoded PDU, and encoded PDUs are exactly what a TS 103 759 `v2xPduEvidence` entry carries.
`tests/test_message_codec.py::test_claim_carries_no_oracle_field` asserts it by name against
`FORBIDDEN_FEATURE_KEYS`.
"""
from __future__ import annotations

import math
from dataclasses import dataclass, replace
from typing import Mapping, Optional, Protocol, runtime_checkable

INTERFACE_NAME = "MessageCodec"
INTERFACE_VERSION = "MessageCodec/1.0"

#: Highest interface MINOR this engine understands. A codec declaring `MessageCodec/1.1` against a
#: 1.0 engine is refused at load, not at step k > 0.
MAX_MINOR = 0

# --------------------------------------------------------------------------- #
# Capabilities
# --------------------------------------------------------------------------- #
CAP_UPER = "uper"                 #: encodes real ASN.1 unaligned PER octets
CAP_JSON = "json"                 #: encodes canonical JSON (the native profile)
CAP_CAM = "cam"                   #: implements encode_cam / decode_cam
CAP_DENM = "denm"                 #: implements encode_denm / decode_denm
CAP_VAM = "vam"                   #: implements encode_vam / decode_vam
CAP_LOSSLESS = "lossless"         #: decode(encode(claim)) == claim EXACTLY, no quantisation
CAP_WIRE_SIZE = "wire_size"       #: wire_size_bytes() is derived from a real encode, not a constant
CAP_EVIDENCE = "evidence"         #: evidence_pdu() returns bytes fit for TS 103 759 v2xPduEvidence

KNOWN_CAPABILITIES = frozenset({
    CAP_UPER, CAP_JSON, CAP_CAM, CAP_DENM, CAP_VAM, CAP_LOSSLESS, CAP_WIRE_SIZE, CAP_EVIDENCE,
})

#: Nothing is reserved on this slot. The channel and detector slots reserve capabilities because a
#: built-in there is grandfathered against a pinned digest (`legacy_global_rng`); a codec touches no
#: RNG stream and no digest-bearing column, so there is nothing to grandfather.
RESERVED_CAPABILITIES: frozenset = frozenset()

#: Signer forms for :meth:`MessageCodec.wire_size_bytes`. TS 103 097 signer alternation is worth
#: 150-200 B and both engines currently hard-code 300 B (`SignedCam.java`, `Dcc.java`), which makes
#: every CBR estimate systematically wrong -- that is why this is a parameter and not a constant.
SIGNER_FORMS = ("none", "digest", "certificate")


# --------------------------------------------------------------------------- #
# The coordinate frame
# --------------------------------------------------------------------------- #
@dataclass(frozen=True, slots=True)
class GeoFrame:
    """The local equirectangular frame the engine's metres live in, and its WGS84 inverse.

    This is **not invented for the codec**: it is exactly the tuple `osm._frame` derives and
    `netimport._assert_frame` gates -- origin at the minimum latitude/longitude over the road ways,
    ``kx = 111320 * cos(mean_lat)``, ``ky = 110540``. Every layer that shares a map is projected
    with this exact tuple, so a codec that used any other frame would place the encoded WGS84
    position hundreds of metres from where the buildings are.

    For a synthetic map (`road_network` in {linear, grid, ring, spider}) there is no OSM extract and
    therefore no natural origin. :data:`DEFAULT_FRAME` supplies a pinned, documented one; it is a
    declared codec parameter, never a wall-clock or machine-derived value.

    The projection is exactly invertible in real arithmetic, so the ONLY error a local -> WGS84 ->
    local round trip introduces is the 1/10 microdegree integer quantisation plus IEEE-754 float
    error. Both are measured in `codecs/units.quantisation_report`.
    """

    lat0: float
    lon0: float
    kx: float           #: metres per degree of longitude at this frame's mean latitude
    ky: float           #: metres per degree of latitude

    def __post_init__(self) -> None:
        if not (-90.0 <= self.lat0 <= 90.0 and -180.0 <= self.lon0 <= 180.0):
            raise ValueError(f"GeoFrame origin ({self.lat0}, {self.lon0}) is not a WGS84 coordinate")
        if not (self.kx > 0.0 and self.ky > 0.0):
            raise ValueError(f"GeoFrame scale must be positive (kx={self.kx}, ky={self.ky})")

    def to_wgs84(self, x: float, y: float) -> tuple:
        """(local metres east, local metres north) -> (latitude, longitude) in degrees."""
        return (self.lat0 + y / self.ky, self.lon0 + x / self.kx)

    def to_local(self, lat: float, lon: float) -> tuple:
        """(latitude, longitude) in degrees -> (local metres east, local metres north)."""
        return ((lon - self.lon0) * self.kx, (lat - self.lat0) * self.ky)

    def to_dict(self) -> dict:
        return {"lat0": self.lat0, "lon0": self.lon0, "kx": self.kx, "ky": self.ky}

    @classmethod
    def from_dict(cls, d: Mapping) -> "GeoFrame":
        return cls(float(d["lat0"]), float(d["lon0"]), float(d["kx"]), float(d["ky"]))

    @classmethod
    def centred_on(cls, lat0: float, lon0: float) -> "GeoFrame":
        """The `osm._frame` construction for a synthetic map: the same kx/ky formulas, evaluated at
        the given origin (which is also the mean latitude when the map is small)."""
        return cls(lat0, lon0, 111320.0 * math.cos(math.radians(lat0)), 110540.0)


#: The pinned default origin for synthetic maps. Ingolstadt city centre, because the repository's
#: one real calibrated scenario (InTAS, via the VeReMi-NextGen submodule) is Ingolstadt, so a
#: synthetic run and a real-map run land in comparable coordinates. It is a CONSTANT: nothing here
#: reads a clock, a locale or an environment variable.
DEFAULT_FRAME = GeoFrame.centred_on(48.7665, 11.4258)

#: UNIX seconds for engine time `t = 0.0` when the config does not say. Pinned, not `time.time()`:
#: a codec that stamped the wall clock into `generationDeltaTime` would make every dataset
#: unreproducible. 2024-01-01T00:00:00Z.
DEFAULT_EPOCH_UNIX = 1704067200.0


# --------------------------------------------------------------------------- #
# The message vocabulary
# --------------------------------------------------------------------------- #
@dataclass(frozen=True, slots=True)
class Claim:
    """ONE outgoing message, in engine units, MA-VISIBLE ONLY.

    "Claim" and not "message" deliberately: every kinematic field here is what the sender ASSERTS.
    For an honest station that is its measured state; for an attacker it is the falsified state. The
    codec cannot tell the difference and must not be able to -- see the module docstring.

    Units are the engine's, and only the engine's:

    ==================  ==========================================================
    ``x`` / ``y``       local metres in a :class:`GeoFrame`, x East, y North
    ``speed``           m/s
    ``heading``         degrees **counter-clockwise from East**, [0, 360)
    ``gen_time``        engine seconds since the run start (float)
    ``pos_conf``        metres, the 95 % *radius* (`run.py`: ``2.448 * sqrt(sigma^2 + bias^2)``)
    ``station_type``    ``"vehicle"`` / ``"vru"``, or any ETSI StationType name
    ==================  ==========================================================
    """

    station_id: int                 #: ETSI StationID (0..2^32-1); derived from the pseudonym
    cert_digest: str                #: HashedId8 hex -- NOT rotation-stable, never a state key
    msg_type: str                   #: "cam" | "denm" | "vam"
    gen_time: float                 #: engine seconds
    x: float                        #: CLAIMED position, local metres east
    y: float                        #: CLAIMED position, local metres north
    speed: float                    #: CLAIMED, m/s
    heading: float                  #: CLAIMED, degrees CCW from East
    pos_conf: float                 #: 95 % radius, metres
    station_type: str = "vehicle"
    msg_count: int = 1
    event_type: Optional[str] = None       #: DENM cause-code NAME as the engine spells it
    sig_ok: bool = True
    cert_valid_from: float = 0.0
    cert_valid_to: float = 0.0
    #: Longitudinal acceleration, m/s^2, if the profile carries it. `None` -> ETSI `unavailable`.
    accel: Optional[float] = None
    #: Vehicle dimensions, metres. `None` -> ETSI `unavailable`.
    length_m: Optional[float] = None
    width_m: Optional[float] = None
    #: DENM `ActionID.sequenceNumber` (0..65535). MA-visible by construction -- a DENM's action id
    #: is on the wire, and it is what lets a receiver correlate an update with the original event.
    sequence_number: int = 0

    def replace(self, **kw) -> "Claim":
        return replace(self, **kw)


@dataclass(frozen=True, slots=True)
class StationView:
    """What the codec needs about the sending station that is not per-message.

    Held separately from :class:`Claim` because it changes on a different cadence (never, mostly)
    and because `encode_cam(claim, station)` is the signature section 2.5 publishes.
    """

    frame: GeoFrame = DEFAULT_FRAME
    #: UNIX seconds corresponding to engine `t = 0.0`.
    epoch_unix: float = DEFAULT_EPOCH_UNIX
    #: `True` for a road-side unit: it maps to ETSI `roadSideUnit(15)` and, in CAM, to the
    #: `rsuContainerHighFrequency` alternative rather than the vehicle one.
    is_rsu: bool = False
    #: Altitude above the WGS84 ellipsoid, metres. The engine is 2-D, so the honest default is
    #: `None`, which encodes as ETSI `unavailable(800001)` rather than as a fabricated 0.
    altitude_m: Optional[float] = None


@runtime_checkable
class MessageCodec(Protocol):
    """Section 2.5, verbatim in shape, with the container methods made explicit.

    ``encode_*`` takes engine units and returns wire octets; ``decode_*`` is its inverse **up to
    quantisation**. A codec that is exactly invertible declares :data:`CAP_LOSSLESS`; one that is
    not must be able to bound its own error, which is what `codecs/units.quantisation_report`
    produces and what `tests/test_message_codec.py` grades.
    """

    interface_version: str
    plugin_id: str
    profile_id: str            #: "native_v1" | "etsi_cam_en302637_2" | ...

    def capabilities(self) -> frozenset: ...

    def standards_claim(self) -> Mapping:
        """What this profile may honestly assert, verbatim into `manifest["standards_profile"]`.

        MUST name the ASN.1 module AND the tag when it encodes anything (section 6.5): "CAMs are
        encoded to <standard> <version> using UPER against the published ETSI ASN.1 modules (ETSI
        Forge, BSD-3-Clause, tag <T>)". MUST NOT say "conformance-tested", "certified" or
        "Plugtests-validated" -- none of those follows from encoding.
        """

    def conventions(self) -> Mapping:
        """Units and frames, on both sides of the conversion. See the module docstring."""

    def encode_cam(self, claim: "Claim", station: "StationView") -> bytes: ...

    def decode_cam(self, blob: bytes) -> "Claim": ...

    def wire_size_bytes(self, claim: "Claim", signer: str) -> int:
        """Payload octets plus the security envelope for `signer` in :data:`SIGNER_FORMS`.

        Feeds airtime -> CBR -> DCC. `signer` is a parameter because TS 103 097 signer alternation
        changes the frame length by 150-200 B, and a CBR computed from a hard-coded 300 B is wrong
        for every frame.
        """

    def evidence_pdu(self, claim: "Claim", station: "StationView") -> bytes:
        """The octets that go into a TS 103 759 `v2xPduEvidence` entry.

        `v2xPduEvidence` is `SEQUENCE (SIZE(1..MAX)) OF V2xPduStream` -- **mandatory, minimum one**
        -- so a report is structurally not a report without this. Today the engine files
        `evidence_msg_refs=[f"{rid}-m"]`, a synthetic self-reference. This method is what makes real
        evidence possible; wiring it into the report is roadmap phase 5, not this seam.
        """


#: The load-time signature contract (`registry._check_signature`). Neither `Protocol` nor `ABC`
#: checks signatures at runtime; this is the third component that does.
CODEC_SPEC = {
    "capabilities": (),
    "standards_claim": (),
    "conventions": (),
    "encode_cam": ("claim", "station"),
    "decode_cam": ("blob",),
    "wire_size_bytes": ("claim", "signer"),
    "evidence_pdu": ("claim", "station"),
}


class MessageCodecBase:
    """Optional convenience base with the defaults every profile shares.

    Inheriting is never required -- the published contract is the `Protocol`, so an implementer
    needs no import of ours. This exists for authors who prefer inheritance, exactly as
    `LinkChannelModelBase` and `CheckBase` do on the other slots.
    """

    interface_version = INTERFACE_VERSION
    plugin_id = "message_codec"
    profile_id = "abstract"

    def capabilities(self) -> frozenset:
        return frozenset()

    def standards_claim(self) -> Mapping:
        return {}

    def conventions(self) -> Mapping:
        return dict(ENGINE_CONVENTIONS)

    def encode_cam(self, claim, station):
        raise NotImplementedError

    def decode_cam(self, blob):
        raise NotImplementedError

    def wire_size_bytes(self, claim, signer="none"):
        raise NotImplementedError

    def evidence_pdu(self, claim, station):
        return self.encode_cam(claim, station)


#: The engine side of every conversion, in one place, so it can never be spelled two ways.
#: Transcribed from `manifest.conventions` (`run.py`) and `schemas/records.py`'s ground_truth note.
ENGINE_CONVENTIONS = {
    "heading": "deg_ccw_from_east",
    "speed": "m_s",
    "position": "m_local_xy",
    "time": "s_float_since_run_start",
    "position_confidence": "m_radius_95pct",
    "station_type": "vehicle|vru",
}

__all__ = [
    "CAP_CAM", "CAP_DENM", "CAP_EVIDENCE", "CAP_JSON", "CAP_LOSSLESS", "CAP_UPER", "CAP_VAM",
    "CAP_WIRE_SIZE", "CODEC_SPEC", "Claim", "DEFAULT_EPOCH_UNIX", "DEFAULT_FRAME",
    "ENGINE_CONVENTIONS", "GeoFrame", "INTERFACE_NAME", "INTERFACE_VERSION",
    "KNOWN_CAPABILITIES", "MAX_MINOR", "MessageCodec", "MessageCodecBase",
    "RESERVED_CAPABILITIES", "SIGNER_FORMS", "StationView",
]
