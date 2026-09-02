"""ETSI ITS profiles -- **real UPER octets** against the published ASN.1 modules.

Tier A of `PLUGIN-ARCHITECTURE.md` D5: *"CAM / DENM / VAM: real UPER, now. Blocked by nothing.
Claiming only 'documented structural conformance' when real UPER costs one `pip install` would
UNDERSTATE what this project can do."* This module is that tier.

WHAT IT ENCODES, AND AGAINST WHAT
---------------------------------
=========================  ==========================================  ================
profile                    standard                                    ASN.1 modules
=========================  ==========================================  ================
`etsi_cam_en302637_2`      ETSI EN 302 637-2 V1.4.1 (CAM)              CAM-PDU-Descriptions + ITS-Container (TS 102 894-2 V1.3.1)
`etsi_denm_en302637_3`     ETSI EN 302 637-3 V1.3.1 (DENM)             DENM-PDU-Descriptions + ITS-Container (TS 102 894-2 V1.3.1)
`etsi_vam_ts103300_3`      ETSI TS 103 300-3 V2.3.1 (VAM)              VAM-PDU-Descriptions + ETSI-ITS-CDD (TS 102 894-2 V2.1.1)
=========================  ==========================================  ================

The `.asn` files are **vendored** in `codecs/asn1/`, at those exact Forge tags, with each
repository's own `LICENSE` beside them. Vendoring is permitted: every ETSI Forge ASN.1 repository
is BSD-3-Clause ("Copyright 2019 ETSI" / "Copyright 2020 ETSI"), whose clause 1 allows source
redistribution with the notice retained. `PROVENANCE.json` records the project, tag, commit and
sha256 of every file, and `tools/fetch_etsi_asn1.py --check` re-verifies them against the Forge.

DEPENDENCY POSTURE
------------------
`asn1tools` is an **optional extra** (`pip install scms-sim-ref[asn1]`), never in
`requirements.txt`. Importing this module without it succeeds; only *constructing* a codec raises
:class:`CodecDependencyError`, with the install command in the message. The engine imports and runs
unchanged when it is absent -- asserted by `tests/test_message_codec.py`.

WHAT THIS DOES NOT CLAIM
------------------------
Encoding is not conformance. Per section 6.5 this profile MAY say that CAMs are UPER-encoded
against the published ETSI modules at a named tag and that the output is validated by an
independent decoder. It MAY NOT say "conformance-tested", "certified", "Plugtests-validated" or
"interoperable with production ITS stations". `standards_claim()` is worded to that rule.
"""
from __future__ import annotations

import json
import os
from typing import Optional

from ..api.codec import (CAP_CAM, CAP_DENM, CAP_EVIDENCE, CAP_UPER, CAP_VAM, CAP_WIRE_SIZE,
                         ENGINE_CONVENTIONS, INTERFACE_VERSION, SIGNER_FORMS, Claim,
                         DEFAULT_EPOCH_UNIX, DEFAULT_FRAME, GeoFrame, MessageCodecBase,
                         StationView)
from . import units as U

ASN1_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "asn1")


class CodecDependencyError(ImportError):
    """`asn1tools` is not installed. Raised at CONSTRUCTION, never mid-run."""


#: spec name -> (relative .asn paths, top-level PDU type, human standard label, Forge tag).
MODULE_SETS = {
    "cam_r1": (("cdd_v1.3.1/ITS-Container.asn",
                "cam_en302637_2_v1.4.1/CAM-PDU-Descriptions.asn"),
               "CAM", "ETSI EN 302 637-2 V1.4.1", "v1.4.1"),
    "denm_r1": (("cdd_v1.3.1/ITS-Container.asn",
                 "denm_en302637_3_v1.3.1/DENM-PDU-Descriptions.asn"),
                "DENM", "ETSI EN 302 637-3 V1.3.1", "v1.3.1"),
    "vam_r2": (("cdd_v2.1.1/ETSI-ITS-CDD.asn",
                "vam_ts103300_3_v2.3.1/VAM-PDU-Descriptions.asn",
                "vam_ts103300_3_v2.3.1/motorcyclist-special-container.asn"),
               "VAM", "ETSI TS 103 300-3 V2.3.1", "v2.3.1"),
}

#: `ItsPduHeader.messageID`, ITS-Container V1.3.1 / `MessageId`, ETSI-ITS-CDD V2.1.1.
MESSAGE_ID = {"cam": 2, "denm": 1, "vam": 16}

#: `ItsPduHeader.protocolVersion`. Release 1 CAM/DENM are version 2; Release 2 PDUs are version 3
#: (`ItsPduHeaderVam ::= ItsPduHeader(WITH COMPONENTS {..., protocolVersion(3), messageId(vam)})`).
PROTOCOL_VERSION = {"cam_r1": 2, "denm_r1": 2, "vam_r2": 3}

#: IEEE 1609.2 / TS 103 097 secured-envelope overhead in octets, **MEASURED, not estimated**.
#:
#: Method: `pycrate` 0.8.1's pre-compiled `pycrate_asn1dir.ITS_IEEE1609_2` (an INDEPENDENT ASN.1
#: runtime, and the only pure-Python one that compiles 1609.2 at all -- `asn1tools` fails on the
#: X.681/X.683 information object classes the security layer is built from). COER-encoded
#: `Ieee1609Dot2Data{protocolVersion 3, content signedData}` with `hashId=sha256`, an
#: `unsecuredData` payload of a real 41-byte CAM, `headerInfo{psid 36, generationTime}`, and an
#: `ecdsaNistP256Signature{rSig x-only, sSig}`:
#:
#:   * `signer = digest` (HashedId8)   -> 134 B total, **93 B overhead**
#:   * `signer = certificate`          -> 260 B total, **219 B overhead** (the AT certificate on
#:     its own is 132 B: explicit, `sha256AndDigest` issuer, `none` id, one `PsidSsp`, a compressed
#:     `ecdsaNistP256` verification key, a 7-day validity period)
#:
#: The certificate figure is a LOWER BOUND: a production AT certificate usually also carries a
#: geographic region and an assurance level. Both figures replace the flat **300 B** that
#: `SignedCam.java` and `Dcc.java` assume for every frame -- which over-states a digest-signed CAM's
#: airtime by 2.2x and therefore mis-estimates every CBR the DCC loop reacts to.
#:
#: This is a SIZE model, not an implementation: no envelope is built and no signature is computed
#: here. That is Tier B (roadmap phase 6) and `standards_claim()` says so.
SECURITY_ENVELOPE_BYTES = {"none": 0, "digest": 93, "certificate": 219}

#: The CAM fields a CAM actually carries, hence the only ones a round trip can restore.
#: `cert_digest`, `sig_ok`, `cert_valid_from/to` and `msg_count` are NOT among them: they belong to
#: the TS 103 097 security envelope and to the simulator's own bookkeeping, and a bare CAM has
#: neither. Saying so explicitly is the difference between a round-trip test and a tautology.
CAM_CARRIED_FIELDS = ("station_id", "gen_time", "x", "y", "speed", "heading", "pos_conf",
                      "station_type")
DENM_CARRIED_FIELDS = ("station_id", "gen_time", "x", "y", "station_type", "event_type",
                       "sequence_number")
VAM_CARRIED_FIELDS = CAM_CARRIED_FIELDS

_SPEC_CACHE: dict = {}


def _or_nan(v):
    """`None` (an ETSI `unavailable` code) -> NaN, never 0.0. See `_claim_from`."""
    return float("nan") if v is None else float(v)


def require_asn1tools():
    """Import `asn1tools` or raise with the exact install command."""
    try:
        import asn1tools                                   # noqa: F401  (optional extra)
    except ImportError as exc:
        raise CodecDependencyError(
            "the ETSI message codecs need the OPTIONAL dependency `asn1tools` "
            "(pip install 'scms-sim-ref[asn1]', or pip install asn1tools). It is deliberately "
            "absent from requirements.txt: the engine imports and runs without it, and the "
            "`native_v1` codec is the default.") from exc
    return asn1tools


def compiled(spec_name: str):
    """The compiled UPER specification for one module set, cached per process.

    Compilation is ~0.1-0.25 s per set on this host, and the in-process multi-run drivers would
    otherwise pay it thousands of times. The cache is keyed on the SPEC NAME, and every spec name
    pins a fixed tuple of vendored files at fixed tags, so it can never serve a stale compilation
    of a different module version.
    """
    if spec_name in _SPEC_CACHE:
        return _SPEC_CACHE[spec_name]
    if spec_name not in MODULE_SETS:
        raise ValueError(f"unknown ASN.1 module set {spec_name!r}; known: {sorted(MODULE_SETS)}")
    asn1tools = require_asn1tools()
    rels = MODULE_SETS[spec_name][0]
    paths = [os.path.join(ASN1_DIR, r.replace("/", os.sep)) for r in rels]
    missing = [p for p in paths if not os.path.exists(p)]
    if missing:
        raise FileNotFoundError(
            f"vendored ETSI ASN.1 module(s) missing: {missing}. Restore them with "
            f"`python tools/fetch_etsi_asn1.py --write`.")
    spec = asn1tools.compile_files(paths, "uper")
    _SPEC_CACHE[spec_name] = spec
    return spec


def provenance() -> dict:
    """`codecs/asn1/PROVENANCE.json`, or `{}` if the vendored tree was stripped."""
    p = os.path.join(ASN1_DIR, "PROVENANCE.json")
    if not os.path.exists(p):
        return {}
    with open(p, "r", encoding="utf-8") as fh:
        return json.load(fh)


# --------------------------------------------------------------------------- #
class EtsiItsCodec(MessageCodecBase):
    """CAM / DENM / VAM in real UPER, with the full unit and frame conversion.

    Construction parameters (all declared, all landing in `manifest["config"]` through
    `plugins.message_codec.params`, none read from the environment or a clock):

    ==========================  =========================================================
    `lat0` `lon0` `kx` `ky`     the :class:`GeoFrame`; defaults to `DEFAULT_FRAME`
    `epoch_unix`                UNIX seconds at engine `t = 0`; default 2024-01-01T00:00Z
    `vru_station_type`          ETSI name for a declared VRU; default `pedestrian`
    `vehicle_station_type`      ETSI name for a declared vehicle; default `passengerCar`
    `decode_reference_t`        engine time a decoder resolves `generationDeltaTime` against
    ==========================  =========================================================
    """

    interface_version = INTERFACE_VERSION
    plugin_id = "etsi_its"
    profile_id = "etsi_cam_en302637_2"

    _PARAMS = ("lat0", "lon0", "kx", "ky", "epoch_unix", "vru_station_type",
               "vehicle_station_type", "decode_reference_t")

    def __init__(self, *, params=None, rng=None, env=None):
        p = dict(params or {})
        unknown = sorted(set(p) - set(self._PARAMS))
        if unknown:
            raise ValueError(f"{self.profile_id} does not accept params {unknown}; "
                             f"known: {list(self._PARAMS)}")
        f = DEFAULT_FRAME
        self.frame = GeoFrame(float(p.get("lat0", f.lat0)), float(p.get("lon0", f.lon0)),
                              float(p.get("kx", f.kx)), float(p.get("ky", f.ky)))
        self.epoch_unix = float(p.get("epoch_unix", DEFAULT_EPOCH_UNIX))
        self.vru_station_type = str(p.get("vru_station_type", "pedestrian"))
        self.vehicle_station_type = str(p.get("vehicle_station_type", "passengerCar"))
        for name in (self.vru_station_type, self.vehicle_station_type):
            if name not in U.STATION_TYPE_BY_NAME:
                raise ValueError(f"{name!r} is not an ETSI StationType name; known: "
                                 f"{sorted(U.STATION_TYPE_BY_NAME)}")
        self.decode_reference_t = float(p.get("decode_reference_t", 0.0))
        self.params = p
        # Fail at CONSTRUCTION, never at step k > 0 (conformance check C10's rule, applied here).
        require_asn1tools()

    @classmethod
    def from_plugin(cls, *, params=None, rng=None, env=None):
        return cls(params=params, rng=rng, env=env)

    # -- declarations ------------------------------------------------------------------- #
    def capabilities(self) -> frozenset:
        return frozenset({CAP_UPER, CAP_CAM, CAP_DENM, CAP_VAM, CAP_WIRE_SIZE, CAP_EVIDENCE})

    def standards_claim(self) -> dict:
        prov = provenance().get("files", {})

        def tag_of(rel):
            return prov.get(rel, {}).get("tag")

        def sha_of(rel):
            return prov.get(rel, {}).get("sha256")

        return {
            "message": ("CAM encoded to ETSI EN 302 637-2 V1.4.1 in unaligned PER (UPER) against "
                        "the published ETSI ASN.1 modules (ETSI Forge, BSD-3-Clause, tag v1.4.1); "
                        "DENM to EN 302 637-3 V1.3.1 (tag v1.3.1); VAM to TS 103 300-3 V2.3.1 "
                        "(tag v2.3.1). Output is byte-stable across processes and is validated by "
                        "an INDEPENDENT decoder (pycrate) in the test suite. This is an encoding "
                        "claim only: NOT conformance-tested, NOT certified, NOT Plugtests-"
                        "validated, and NOT demonstrated against a production ITS station."),
            "asn1_source": "https://forge.etsi.org/rep/ITS/asn1",
            "asn1_licence": "BSD-3-Clause (Copyright 2019/2020 ETSI)",
            "asn1_modules": {
                "cam": {"standard": "ETSI EN 302 637-2 V1.4.1", "tag": tag_of(
                    "cam_en302637_2_v1.4.1/CAM-PDU-Descriptions.asn"),
                    "sha256": sha_of("cam_en302637_2_v1.4.1/CAM-PDU-Descriptions.asn")},
                "denm": {"standard": "ETSI EN 302 637-3 V1.3.1", "tag": tag_of(
                    "denm_en302637_3_v1.3.1/DENM-PDU-Descriptions.asn"),
                    "sha256": sha_of("denm_en302637_3_v1.3.1/DENM-PDU-Descriptions.asn")},
                "vam": {"standard": "ETSI TS 103 300-3 V2.3.1", "tag": tag_of(
                    "vam_ts103300_3_v2.3.1/VAM-PDU-Descriptions.asn"),
                    "sha256": sha_of("vam_ts103300_3_v2.3.1/VAM-PDU-Descriptions.asn")},
                "cdd_r1": {"standard": "ETSI TS 102 894-2 V1.3.1", "tag": tag_of(
                    "cdd_v1.3.1/ITS-Container.asn"),
                    "sha256": sha_of("cdd_v1.3.1/ITS-Container.asn")},
                "cdd_r2": {"standard": "ETSI TS 102 894-2 V2.1.1", "tag": tag_of(
                    "cdd_v2.1.1/ETSI-ITS-CDD.asn"),
                    "sha256": sha_of("cdd_v2.1.1/ETSI-ITS-CDD.asn")},
            },
            "security_envelope": ("none -- no IEEE 1609.2 / TS 103 097 envelope is built and no "
                                  "signature is computed or verified. wire_size_bytes() adds a "
                                  "MEASURED envelope SIZE only (see SECURITY_ENVELOPE_BYTES)."),
            "caveats": [
                "VAM-PDU-Descriptions.asn at Forge tag v2.3.1 carries its own header line "
                "'Draft V0.0.4_2.2.1 ... Modified to import from the CDD module V2.1.1'; the "
                "repository's submodule pin for that tag is CDD v2.1.1, and that is what is "
                "vendored. The VAM profile is therefore the Forge's published draft, not a "
                "final-and-independently-versioned module.",
                "Every field the engine does not measure is encoded with ETSI's explicit "
                "`unavailable` code, never with a fabricated value. In this engine that is: "
                "altitude, all *Confidence members, curvature, yawRate, vehicle dimensions and "
                "longitudinal acceleration.",
            ],
        }

    def conventions(self) -> dict:
        return {
            "engine": dict(ENGINE_CONVENTIONS),
            "wire": {
                "heading": "0.1 deg clockwise from North (HeadingValue / Wgs84AngleValue)",
                "speed": "0.01 m/s (SpeedValue)",
                "position": "WGS84 in 1/10 microdegree (Latitude / Longitude)",
                "time": ("generationDeltaTime = TimestampIts mod 65536, TimestampIts = ms since "
                         "2004-01-01T00:00:00 on a clock that does not pause for leap seconds"),
                "position_confidence": ("PosConfidenceEllipse{semiMajor, semiMinor, "
                                        "semiMajorOrientation}, semi-axes in cm at 95 %"),
                "station_type": "StationType INTEGER 0..255, named 0..15",
            },
            "assumptions": {
                "position_confidence": U.POS_CONFIDENCE_ASSUMPTION,
                "station_type": ("the engine's MA-visible declaration is two-valued; the real "
                                 "fleet class lives on GtVehicle, which is ORACLE, so it must not "
                                 "reach the wire. vehicle -> " + self.vehicle_station_type +
                                 ", vru -> " + self.vru_station_type),
                "drive_direction": ("forward(0) always: this engine's mobility integrates a "
                                    "non-negative speed along a route and never reverses, so "
                                    "backward(1) is unreachable and TS 103 759's "
                                    "backward-with-speed observation can never fire on this data"),
                "epoch": (f"engine t=0 is UNIX {self.epoch_unix:.0f}; a DECLARED parameter, never "
                          f"a wall clock, or the dataset would not replay"),
                "frame": self.frame.to_dict(),
            },
            "quantisation": ("measured per field by codecs.units.quantisation_report(); the "
                             "worst case is half an LSB in every convertible field"),
        }

    # -- CAM ---------------------------------------------------------------------------- #
    def _header(self, spec_name: str, claim: Claim) -> dict:
        sid = int(claim.station_id)
        if not (0 <= sid <= 4294967295):
            raise ValueError(f"station_id {sid} outside StationID INTEGER(0..4294967295)")
        if spec_name == "vam_r2":
            return {"protocolVersion": PROTOCOL_VERSION[spec_name],
                    "messageId": MESSAGE_ID["vam"], "stationId": sid}
        return {"protocolVersion": PROTOCOL_VERSION[spec_name],
                "messageID": MESSAGE_ID[claim.msg_type if claim.msg_type in MESSAGE_ID else "cam"],
                "stationID": sid}

    def _frame_for(self, station: Optional[StationView]) -> GeoFrame:
        """The frame to project with -- and a REFUSAL if the caller supplies a different one.

        `decode_*` has only `self.frame` to invert with (a CAM carries WGS84, not a frame), so a
        `StationView` carrying a different origin would encode correctly and decode into a position
        hundreds of metres away, silently and only for the stations that passed that view. Making
        it an error is the difference between a bug and a message.
        """
        if station is None or station.frame is None:
            return self.frame
        if station.frame != self.frame:
            raise ValueError(
                f"StationView.frame {station.frame.to_dict()} differs from this codec's frame "
                f"{self.frame.to_dict()}; decode_* can only invert the CODEC's frame, so the two "
                f"must agree. Construct the codec with the run's frame "
                f"(plugins.message_codec.params.lat0/lon0/kx/ky).")
        return station.frame

    def _reference_position(self, claim: Claim, station: StationView, release: int) -> dict:
        lat_i, lon_i = U.local_to_etsi_position(self._frame_for(station), claim.x, claim.y)
        alt = station.altitude_m if station is not None else None
        return {
            "latitude": lat_i, "longitude": lon_i,
            "positionConfidenceEllipse": U.pos_confidence_ellipse(claim.pos_conf, release),
            "altitude": {"altitudeValue": U.altitude_to_etsi(alt),
                         "altitudeConfidence": U.UNAVAILABLE["altitudeConfidence"]},
        }

    def _station_type(self, claim: Claim, station: StationView) -> int:
        return U.station_type_to_etsi(
            claim.station_type, is_rsu=bool(station is not None and station.is_rsu),
            vru_station_type=self.vru_station_type,
            vehicle_station_type=self.vehicle_station_type)

    def cam_dict(self, claim: Claim, station: Optional[StationView] = None) -> dict:
        """The full CAM as an `asn1tools` value tree. Public so a test -- or an INDEPENDENT
        decoder -- can compare structures, not just octets."""
        station = station or StationView(frame=self.frame, epoch_unix=self.epoch_unix)
        epoch = station.epoch_unix
        if station.is_rsu:
            hf = ("rsuContainerHighFrequency", {})
        else:
            hf = ("basicVehicleContainerHighFrequency", {
                "heading": {"headingValue": U.heading_to_etsi(claim.heading),
                            "headingConfidence": U.UNAVAILABLE["headingConfidence"]},
                "speed": {"speedValue": U.speed_to_etsi(claim.speed, release=1),
                          "speedConfidence": U.UNAVAILABLE["speedConfidence"]},
                # forward(0): see conventions()["assumptions"]["drive_direction"].
                "driveDirection": "forward",
                "vehicleLength": {
                    "vehicleLengthValue": U.vehicle_length_to_etsi(claim.length_m),
                    "vehicleLengthConfidenceIndication":
                        U.UNAVAILABLE["vehicleLengthConfidenceIndication"]},
                "vehicleWidth": U.vehicle_width_to_etsi(claim.width_m),
                "longitudinalAcceleration": {
                    "longitudinalAccelerationValue": U.accel_to_etsi(claim.accel),
                    "longitudinalAccelerationConfidence": U.UNAVAILABLE["accelerationConfidence"]},
                "curvature": {"curvatureValue": U.UNAVAILABLE["curvatureValue"],
                              "curvatureConfidence": U.UNAVAILABLE["curvatureConfidence"]},
                "curvatureCalculationMode": U.UNAVAILABLE["curvatureCalculationMode"],
                "yawRate": {"yawRateValue": U.UNAVAILABLE["yawRateValue"],
                            "yawRateConfidence": U.UNAVAILABLE["yawRateConfidence"]},
            })
        return {
            "header": self._header("cam_r1", claim.replace(msg_type="cam")),
            "cam": {
                "generationDeltaTime": U.generation_delta_time(claim.gen_time, epoch),
                "camParameters": {
                    "basicContainer": {
                        "stationType": self._station_type(claim, station),
                        "referencePosition": self._reference_position(claim, station, release=1),
                    },
                    "highFrequencyContainer": hf,
                },
            },
        }

    def encode_cam(self, claim: Claim, station: Optional[StationView] = None) -> bytes:
        return compiled("cam_r1").encode("CAM", self.cam_dict(claim, station))

    def decode_cam(self, blob: bytes, reference_t: Optional[float] = None) -> Claim:
        """UPER octets -> :class:`Claim`, in ENGINE units.

        Fields a CAM does not carry (`cert_digest`, `sig_ok`, `cert_valid_*`, `msg_count`) come
        back at their defaults; see :data:`CAM_CARRIED_FIELDS`. `reference_t` is the receiver-side
        engine time used to resolve `generationDeltaTime`'s 65.536 s wrap.
        """
        d = compiled("cam_r1").decode("CAM", blob)
        return self._claim_from(d["header"], d["cam"]["generationDeltaTime"],
                                d["cam"]["camParameters"]["basicContainer"],
                                d["cam"]["camParameters"]["highFrequencyContainer"],
                                "cam", reference_t)

    def _claim_from(self, header, gdt, basic, hf, msg_type, reference_t):
        ref_t = self.decode_reference_t if reference_t is None else float(reference_t)
        ref_ms = U.timestamp_its_ms(ref_t, self.epoch_unix)
        gen_t = U.engine_time_from_timestamp_its(
            U.resolve_generation_delta_time(gdt, ref_ms), self.epoch_unix)
        rp = basic["referencePosition"]
        x, y = U.etsi_position_to_local(self.frame, rp["latitude"], rp["longitude"])
        conf = U.pos_confidence_from_ellipse(rp["positionConfidenceEllipse"])
        st = U.station_type_from_etsi(basic["stationType"],
                                      vru_station_type=self.vru_station_type,
                                      vehicle_station_type=self.vehicle_station_type)
        speed = heading = None
        accel = length = width = None
        # NOTE the `_or_nan` convention below: an ETSI `unavailable` code decodes to NaN, never to
        # 0.0. "Unavailable" and "zero" are different statements -- a station that did not report
        # its speed is not a stationary station -- and `0.0 or 0.0` would silently make them the
        # same. NaN propagates and compares false, so a consumer that ignores the distinction gets
        # a visibly wrong answer instead of a plausibly wrong one.
        # `hf` is the CHOICE `(alternative_name, body)`. The vehicle alternative is
        # `basicVehicleContainerHighFrequency`; `rsuContainerHighFrequency` carries no kinematics at
        # all, so an RSU's CAM legitimately decodes with speed and heading absent.
        if isinstance(hf, tuple) and hf[0] == "basicVehicleContainerHighFrequency" and hf[1]:
            body = hf[1]
            if "heading" in body:
                hv = body["heading"]
                heading = U.heading_from_etsi(hv.get("headingValue", hv.get("value")))
            if "speed" in body:
                speed = U.speed_from_etsi(body["speed"]["speedValue"], release=1)
            if "longitudinalAcceleration" in body:
                accel = U.accel_from_etsi(
                    body["longitudinalAcceleration"]["longitudinalAccelerationValue"])
            if "vehicleLength" in body:
                length = U.vehicle_length_from_etsi(body["vehicleLength"]["vehicleLengthValue"])
            if "vehicleWidth" in body:
                width = U.vehicle_width_from_etsi(body["vehicleWidth"])
        return Claim(
            station_id=int(header.get("stationID", header.get("stationId"))),
            cert_digest="", msg_type=msg_type, gen_time=gen_t, x=x, y=y,
            speed=_or_nan(speed), heading=_or_nan(heading), pos_conf=_or_nan(conf),
            station_type=st, accel=accel, length_m=length, width_m=width)

    # -- DENM --------------------------------------------------------------------------- #
    def denm_dict(self, claim: Claim, station: Optional[StationView] = None) -> dict:
        station = station or StationView(frame=self.frame, epoch_unix=self.epoch_unix)
        epoch = station.epoch_unix
        cause, sub = U.denm_cause_code(claim.event_type)
        ts = U.timestamp_its_ms(claim.gen_time, epoch)
        seq = int(claim.sequence_number) % 65536
        return {
            "header": self._header("denm_r1", claim.replace(msg_type="denm")),
            "denm": {
                "management": {
                    "actionID": {"originatingStationID": int(claim.station_id),
                                 "sequenceNumber": seq},
                    # TimestampIts, NOT generationDeltaTime: a DENM carries the full 42-bit
                    # timestamp, so -- unlike a CAM -- its time is unambiguous on the wire.
                    "detectionTime": ts,
                    "referenceTime": ts,
                    "eventPosition": self._reference_position(claim, station, release=1),
                    # `validityDuration ValidityDuration DEFAULT defaultValidity` (= 600 s). Stated
                    # EXPLICITLY rather than left to the encoder, because a decoder materialises
                    # the DEFAULT and an "expected value tree" that omits it then differs from
                    # every decoder's output for a reason that is not a difference in the bytes.
                    "validityDuration": "defaultValidity",
                    "stationType": self._station_type(claim, station),
                },
                "situation": {
                    # informationQuality (0..7): 0 is `unavailable`. The engine's DENMs carry no
                    # quality estimate, so 0 is the truthful value, not a low score.
                    "informationQuality": 0,
                    "eventType": {"causeCode": cause, "subCauseCode": sub},
                },
            },
        }

    def encode_denm(self, claim: Claim, station: Optional[StationView] = None) -> bytes:
        return compiled("denm_r1").encode("DENM", self.denm_dict(claim, station))

    def decode_denm(self, blob: bytes) -> Claim:
        d = compiled("denm_r1").decode("DENM", blob)
        m = d["denm"]["management"]
        rp = m["eventPosition"]
        x, y = U.etsi_position_to_local(self.frame, rp["latitude"], rp["longitude"])
        sit = d["denm"].get("situation") or {}
        ev = sit.get("eventType") or {}
        return Claim(
            station_id=int(m["actionID"]["originatingStationID"]), cert_digest="", msg_type="denm",
            gen_time=U.engine_time_from_timestamp_its(m["detectionTime"], self.epoch_unix),
            x=x, y=y, speed=0.0, heading=0.0,
            pos_conf=_or_nan(U.pos_confidence_from_ellipse(rp["positionConfidenceEllipse"])),
            station_type=U.station_type_from_etsi(
                m["stationType"], vru_station_type=self.vru_station_type,
                vehicle_station_type=self.vehicle_station_type),
            event_type=U.denm_event_type(ev.get("causeCode", 0), ev.get("subCauseCode", 0)),
            sequence_number=int(m["actionID"]["sequenceNumber"]))

    # -- VAM ---------------------------------------------------------------------------- #
    def vam_dict(self, claim: Claim, station: Optional[StationView] = None) -> dict:
        """TS 103 300-3 V2.3.1 against ETSI-ITS-CDD V2.1.1 -- note the RELEASE 2 differences:
        `TrafficParticipantType` rather than `StationType`, `ReferencePositionWithConfidence`,
        `PositionConfidenceEllipse` with `*AxisLength` member names, a `Wgs84Angle` heading that
        carries its own confidence, and `SemiAxisLength` with `doNotUse(0)` so 0 cm is no longer a
        legal real value."""
        station = station or StationView(frame=self.frame, epoch_unix=self.epoch_unix)
        epoch = station.epoch_unix
        lat_i, lon_i = U.local_to_etsi_position(self._frame_for(station), claim.x, claim.y)
        return {
            "header": self._header("vam_r2", claim),
            "vam": {
                "generationDeltaTime": U.generation_delta_time(claim.gen_time, epoch),
                "vamParameters": {
                    "basicContainer": {
                        "stationType": self._station_type(claim, station),
                        "referencePosition": {
                            "latitude": lat_i, "longitude": lon_i,
                            "positionConfidenceEllipse":
                                U.pos_confidence_ellipse(claim.pos_conf, release=2),
                            "altitude": {
                                "altitudeValue": U.altitude_to_etsi(station.altitude_m),
                                "altitudeConfidence": U.UNAVAILABLE["altitudeConfidence"]},
                        },
                    },
                    "vruHighFrequencyContainer": {
                        "heading": {"value": U.heading_to_etsi(claim.heading),
                                    "confidence": U.UNAVAILABLE["headingConfidence"]},
                        "speed": {"speedValue": U.speed_to_etsi(claim.speed, release=2),
                                  "speedConfidence": U.UNAVAILABLE["speedConfidence"]},
                        "longitudinalAcceleration": {
                            "longitudinalAccelerationValue": U.accel_to_etsi(claim.accel),
                            "longitudinalAccelerationConfidence":
                                U.UNAVAILABLE["accelerationConfidence"]},
                    },
                },
            },
        }

    def encode_vam(self, claim: Claim, station: Optional[StationView] = None) -> bytes:
        return compiled("vam_r2").encode("VAM", self.vam_dict(claim, station))

    def decode_vam(self, blob: bytes, reference_t: Optional[float] = None) -> Claim:
        d = compiled("vam_r2").decode("VAM", blob)
        p = d["vam"]["vamParameters"]
        basic, hf = p["basicContainer"], p["vruHighFrequencyContainer"]
        ref_t = self.decode_reference_t if reference_t is None else float(reference_t)
        ref_ms = U.timestamp_its_ms(ref_t, self.epoch_unix)
        gen_t = U.engine_time_from_timestamp_its(
            U.resolve_generation_delta_time(d["vam"]["generationDeltaTime"], ref_ms),
            self.epoch_unix)
        rp = basic["referencePosition"]
        x, y = U.etsi_position_to_local(self.frame, rp["latitude"], rp["longitude"])
        return Claim(
            station_id=int(d["header"]["stationId"]), cert_digest="", msg_type="vam",
            gen_time=gen_t, x=x, y=y,
            speed=_or_nan(U.speed_from_etsi(hf["speed"]["speedValue"], release=2)),
            heading=_or_nan(U.heading_from_etsi(hf["heading"]["value"])),
            pos_conf=_or_nan(U.pos_confidence_from_ellipse(rp["positionConfidenceEllipse"])),
            station_type=U.station_type_from_etsi(
                basic["stationType"], vru_station_type=self.vru_station_type,
                vehicle_station_type=self.vehicle_station_type),
            accel=U.accel_from_etsi(
                hf["longitudinalAcceleration"]["longitudinalAccelerationValue"]))

    # -- sizing / evidence -------------------------------------------------------------- #
    def _encode_for_type(self, claim: Claim, station: Optional[StationView]) -> bytes:
        mt = (claim.msg_type or "cam").lower()
        if mt == "denm":
            return self.encode_denm(claim, station)
        if mt == "vam":
            return self.encode_vam(claim, station)
        return self.encode_cam(claim, station)

    def wire_size_bytes(self, claim: Claim, signer: str = "digest") -> int:
        """Real UPER payload length plus the MEASURED 1609.2/TS 103 097 envelope for `signer`.

        Default `digest`, because that is what an ITS-G5 station sends between certificate
        refreshes and what the CBR model should assume for most frames.
        """
        if signer not in SIGNER_FORMS:
            raise ValueError(f"signer must be one of {SIGNER_FORMS}, got {signer!r}")
        return len(self._encode_for_type(claim, None)) + SECURITY_ENVELOPE_BYTES[signer]

    def evidence_pdu(self, claim: Claim, station: Optional[StationView] = None) -> bytes:
        """The real UPER octets of the message, fit to be carried in a TS 103 759 `V2xPduStream`.

        This is the half of TS 103 759 that today's `evidence_msg_refs=[f"{rid}-m"]` cannot
        satisfy: `v2xPduEvidence` is `SEQUENCE (SIZE(1..MAX)) OF V2xPduStream`, so a report
        without actual PDU octets is structurally not a report. Wiring these octets into the
        report record is roadmap phase 5; producing them is this seam's job and it is done.
        """
        return self._encode_for_type(claim, station)


class EtsiCamCodec(EtsiItsCodec):
    """The CAM profile under its own registry name, so `plugins.message_codec.ref` can select the
    specific standard rather than a family. Behaviourally identical -- the family class encodes all
    three PDU types -- but `profile_id` is what lands in the manifest."""
    profile_id = "etsi_cam_en302637_2"


class EtsiDenmCodec(EtsiItsCodec):
    profile_id = "etsi_denm_en302637_3"

    def encode_cam(self, claim: Claim, station: Optional[StationView] = None) -> bytes:
        return self.encode_denm(claim, station)

    def decode_cam(self, blob: bytes, reference_t: Optional[float] = None) -> Claim:
        return self.decode_denm(blob)


class EtsiVamCodec(EtsiItsCodec):
    profile_id = "etsi_vam_ts103300_3"

    def encode_cam(self, claim: Claim, station: Optional[StationView] = None) -> bytes:
        return self.encode_vam(claim, station)

    def decode_cam(self, blob: bytes, reference_t: Optional[float] = None) -> Claim:
        return self.decode_vam(blob, reference_t)
