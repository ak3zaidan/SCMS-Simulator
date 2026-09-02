"""Units, frames and quantisation -- **the substance of the standards profile, not a detail**.

`STANDARDS-AUDIT.md` finding 5: *"Units and frames are engine-private and mutually incompatible.
Python uses degrees CCW from East, local metres, float m/s, float seconds. ... ETSI requires 0.1
degree CW from North, WGS84 in 1/10 microdegree, 0.01 m/s, and `generationDeltaTime` as ms mod
65536 since the 2004 epoch. Any standards profile is therefore ALSO a unit-conversion layer."*
This module is that layer, and it is deliberately separate from `etsi.py` so that **every
conversion is testable without `asn1tools` installed**.

Three rules the whole module obeys:

1. **Never fabricate a value the engine does not have.** ETSI gives every data element an explicit
   `unavailable` code; a field the engine cannot supply is encoded as that code, never as a
   plausible-looking zero. `curvature`, `yawRate`, `vehicleLength`, `accelerationConfidence`,
   `altitude` and both `*Confidence` members are `unavailable` in this engine's CAMs, and that is a
   truthful statement about the simulator rather than a gap in the encoder.
2. **Never read the oracle to fill a wire field.** The tempting one is `StationType`: ETSI has 13
   named classes and the engine has a real fleet of car / motorcycle / truck / bus -- on
   `GtVehicle`, which is ORACLE. The MA-visible declaration is two-valued (`"vehicle"` / `"vru"`),
   so that is what the mapping reads. Encoding the true fleet class would put ground truth into
   every PDU. See :data:`ENGINE_STATION_TYPE` and its note.
3. **Rounding is one function, and it is not `round()`.** :func:`iround` is round-half-away-from-
   zero. CPython's builtin `round()` is banker's rounding, whose tie direction depends on the
   parity of the neighbouring integer -- correct, but it makes "maximum error is half an LSB" a
   claim about the *inputs* rather than about the *encoder*. One rule, one bound: exactly 0.5 LSB.

Everything below cites the ASN.1 it implements by module, tag and type name. The vendored modules
are in `codecs/asn1/` with their BSD-3-Clause `LICENSE` files and `PROVENANCE.json`.
"""
from __future__ import annotations

import math
from typing import Optional

from ..api.codec import GeoFrame

# --------------------------------------------------------------------------- #
# Rounding
# --------------------------------------------------------------------------- #


def iround(v: float) -> int:
    """Round half away from zero. Deterministic, symmetric, max error exactly 0.5 LSB.

    NOT `round()`: the builtin is round-half-to-even, so `round(0.5) == 0` and `round(1.5) == 2`.
    Both are legitimate; only one of them lets the error bound be stated as a property of the
    encoder rather than of the sample.
    """
    return int(math.floor(v + 0.5)) if v >= 0.0 else -int(math.floor(-v + 0.5))


def _clamp(v: int, lo: int, hi: int) -> int:
    return lo if v < lo else (hi if v > hi else v)


# --------------------------------------------------------------------------- #
# Time -- TimestampIts and generationDeltaTime
# --------------------------------------------------------------------------- #
#: UNIX seconds at 2004-01-01T00:00:00Z. `TimestampIts ::= INTEGER {utcStartOf2004(0),
#: oneMillisecAfterUTCStartOf2004(1)} (0..4398046511103)` -- ITS-Container, TS 102 894-2 v1.3.1.
UNIX_EPOCH_2004 = 1072915200.0

#: UNIX timestamps at which a positive leap second was inserted AFTER the 2004 ITS epoch. The ITS
#: timestamp does not pause for a leap second, so it gains one second on UTC at each of these -- the
#: reason the task specification calls the epoch a **TAI** epoch even though the ASN.1 names it
#: `utcStartOf2004`. TAI-UTC was 32 s at 2004-01-01 and is 37 s from 2017-01-01, i.e. exactly these
#: five insertions. (IERS Bulletin C; no leap second has been inserted since 2016-12-31.)
LEAP_SECONDS_AFTER_2004 = (
    1136073600.0,   # 2006-01-01T00:00:00Z  (inserted at the end of 2005-12-31)
    1230768000.0,   # 2009-01-01
    1341100800.0,   # 2012-07-01
    1435708800.0,   # 2015-07-01
    1483228800.0,   # 2017-01-01
)

#: One `generationDeltaTime` cycle. `GenerationDeltaTime ::= INTEGER {oneMilliSec(1)} (0..65535)`
#: -- CAM-PDU-Descriptions, EN 302 637-2 v1.4.1. 65 536 ms = 65.536 s.
GEN_DELTA_MODULUS = 65536


def leap_seconds_since_2004(unix_s: float) -> int:
    """How many seconds the ITS clock has gained on UTC by `unix_s`."""
    return sum(1 for t in LEAP_SECONDS_AFTER_2004 if unix_s >= t)


def timestamp_its_ms(t_engine: float, epoch_unix: float) -> int:
    """Engine seconds -> `TimestampIts` (milliseconds since the 2004 ITS epoch).

    `epoch_unix` is the UNIX time of engine `t = 0.0` and is a declared codec PARAMETER: a codec
    that read a wall clock here would make every dataset unreproducible.

    **The large-offset split, and why it is not cosmetic.** The obvious spelling,
    ``iround(1000 * (epoch_unix + t - UNIX_EPOCH_2004 + leap))``, forms a float of magnitude
    ~6.3e8 s before rounding, whose IEEE-754 ulp is ~2.4e-7 s. That is *added to* the 0.5 ms
    rounding error, and a sweep measures the result at **0.50029 ms** -- over the half-LSB bound,
    not by much and not by accident. Splitting the constant twenty-year offset into exact INTEGER
    milliseconds and rounding only the small ``frac + t_engine`` term restores the bound exactly.
    The lesson generalises to every fixed-point conversion with a distant epoch.
    """
    whole = math.floor(epoch_unix)
    frac = epoch_unix - whole
    leap = leap_seconds_since_2004(epoch_unix + t_engine)
    return ((int(whole) - int(UNIX_EPOCH_2004)) * 1000 + leap * 1000
            + iround(1000.0 * (frac + t_engine)))


def generation_delta_time(t_engine: float, epoch_unix: float) -> int:
    """`TimestampIts mod 65536`, per EN 302 637-2 -- the only time field a CAM carries.

    The wrap is not a defect of this implementation: **a CAM is genuinely ambiguous beyond 65.536
    s** and a receiver resolves it against its own clock. :func:`resolve_generation_delta_time` is
    that resolution, and TS 103 759's normative beacon-interval check states the same rule ("add
    65 536 ms" when the difference is negative).
    """
    return timestamp_its_ms(t_engine, epoch_unix) % GEN_DELTA_MODULUS


def resolve_generation_delta_time(gdt: int, reference_ms: int) -> int:
    """The `TimestampIts` congruent to `gdt` (mod 65536) that is nearest `reference_ms`.

    This is what makes `decode -> encode` a round trip rather than a lossy projection, and it is
    exactly what a real receiver does with its own clock as the reference.
    """
    base = reference_ms - (reference_ms % GEN_DELTA_MODULUS) + (gdt % GEN_DELTA_MODULUS)
    best = base
    for cand in (base - GEN_DELTA_MODULUS, base, base + GEN_DELTA_MODULUS):
        if abs(cand - reference_ms) < abs(best - reference_ms):
            best = cand
    return best


def engine_time_from_timestamp_its(ms: int, epoch_unix: float) -> float:
    """Inverse of :func:`timestamp_its_ms`, in the same exact-integer arithmetic.

    `leap` is a step function of UTC, so it is solved by one fixed-point iteration: the provisional
    UTC over-states by at most 5 s (the whole 2004-to-now leap total), and the five insertions are
    years apart, so a single re-evaluation converges everywhere except inside a 5 s window around an
    insertion instant -- which no scenario epoch here sits in.
    """
    provisional = UNIX_EPOCH_2004 + ms / 1000.0
    leap = leap_seconds_since_2004(provisional - leap_seconds_since_2004(provisional))
    whole = math.floor(epoch_unix)
    frac = epoch_unix - whole
    rem_ms = int(ms) - (int(whole) - int(UNIX_EPOCH_2004)) * 1000 - leap * 1000
    return rem_ms / 1000.0 - frac


# --------------------------------------------------------------------------- #
# Heading -- the frame flip
# --------------------------------------------------------------------------- #
#: `HeadingValue ::= INTEGER {wgs84North(0), wgs84East(900), wgs84South(1800), wgs84West(2700),
#: unavailable(3601)} (0..3601)` -- ITS-Container v1.3.1. Unit 0.1 degree, CLOCKWISE FROM NORTH.
#: `Wgs84AngleValue` in ETSI-ITS-CDD v2.1.1 is the same with `doNotUse(3600)` added.
HEADING_UNAVAILABLE = 3601
HEADING_LSB_DEG = 0.1


def heading_ccw_east_to_bearing(deg_ccw_east: float) -> float:
    """Engine convention -> compass bearing in degrees clockwise from North, [0, 360).

    The engine measures `atan2(vy, vx)` in degrees, i.e. counter-clockwise from the +x (East) axis.
    A compass bearing runs the other way from a different zero, so the map is the reflection
    `bearing = 90 - theta`, reduced modulo 360. The Java/SUMO side of this repository is already
    CW-from-North, which is why the two engines' heading columns are not comparable without this.
    """
    return (90.0 - deg_ccw_east) % 360.0


def bearing_to_heading_ccw_east(bearing_deg: float) -> float:
    """Inverse of :func:`heading_ccw_east_to_bearing`. The map is its own inverse."""
    return (90.0 - bearing_deg) % 360.0


def heading_to_etsi(deg_ccw_east: Optional[float]) -> int:
    """deg CCW from East -> `HeadingValue` (0..3599), or `unavailable(3601)`."""
    if deg_ccw_east is None or not math.isfinite(deg_ccw_east):
        return HEADING_UNAVAILABLE
    return iround(heading_ccw_east_to_bearing(deg_ccw_east) * 10.0) % 3600


def heading_from_etsi(value: int) -> Optional[float]:
    if value is None or value >= 3600:
        return None
    return bearing_to_heading_ccw_east(value * HEADING_LSB_DEG)


# --------------------------------------------------------------------------- #
# Speed
# --------------------------------------------------------------------------- #
#: `SpeedValue ::= INTEGER {standstill(0), oneCentimeterPerSec(1), unavailable(16383)} (0..16383)`
#: -- ITS-Container v1.3.1. ETSI-ITS-CDD v2.1.1 additionally names `outOfRange(16382)` and states
#: "16382 for speed values greater than 163,81 m/s", so the largest REAL value differs by release.
SPEED_UNAVAILABLE = 16383
SPEED_OUT_OF_RANGE_R2 = 16382
SPEED_LSB_MPS = 0.01


def speed_to_etsi(mps: Optional[float], release: int = 1) -> int:
    """m/s -> `SpeedValue` in 0.01 m/s."""
    if mps is None or not math.isfinite(mps) or mps < 0.0:
        return SPEED_UNAVAILABLE
    real_max = 16382 if release == 1 else 16381
    v = iround(mps * 100.0)
    if v > real_max:
        return real_max if release == 1 else SPEED_OUT_OF_RANGE_R2
    return _clamp(v, 0, real_max)


def speed_from_etsi(value: int, release: int = 1) -> Optional[float]:
    if value is None or value >= (SPEED_UNAVAILABLE if release == 1 else SPEED_OUT_OF_RANGE_R2):
        return None
    return value * SPEED_LSB_MPS


# --------------------------------------------------------------------------- #
# Position -- local metres <-> WGS84 in 1/10 microdegree
# --------------------------------------------------------------------------- #
#: `Latitude ::= INTEGER {..., unavailable(900000001)} (-900000000..900000001)`,
#: `Longitude ::= INTEGER {..., unavailable(1800000001)} (-1800000000..1800000001)` -- both
#: ITS-Container v1.3.1. Unit 1/10 microdegree = 1e-7 degree.
LATLON_LSB_DEG = 1e-7
LATITUDE_UNAVAILABLE = 900000001
LONGITUDE_UNAVAILABLE = 1800000001


def latitude_to_etsi(lat_deg: Optional[float]) -> int:
    if lat_deg is None or not math.isfinite(lat_deg):
        return LATITUDE_UNAVAILABLE
    return _clamp(iround(lat_deg * 1e7), -900000000, 900000000)


def longitude_to_etsi(lon_deg: Optional[float]) -> int:
    if lon_deg is None or not math.isfinite(lon_deg):
        return LONGITUDE_UNAVAILABLE
    return _clamp(iround(lon_deg * 1e7), -1800000000, 1800000000)


def latitude_from_etsi(v: int) -> Optional[float]:
    return None if v is None or v == LATITUDE_UNAVAILABLE else v * LATLON_LSB_DEG


def longitude_from_etsi(v: int) -> Optional[float]:
    return None if v is None or v == LONGITUDE_UNAVAILABLE else v * LATLON_LSB_DEG


def local_to_etsi_position(frame: GeoFrame, x: float, y: float) -> tuple:
    """(local metres) -> (`Latitude`, `Longitude`) integers."""
    lat, lon = frame.to_wgs84(x, y)
    return latitude_to_etsi(lat), longitude_to_etsi(lon)


def etsi_position_to_local(frame: GeoFrame, lat_i: int, lon_i: int) -> tuple:
    """(`Latitude`, `Longitude`) integers -> (local metres)."""
    lat, lon = latitude_from_etsi(lat_i), longitude_from_etsi(lon_i)
    if lat is None or lon is None:
        return (float("nan"), float("nan"))
    return frame.to_local(lat, lon)


# --------------------------------------------------------------------------- #
# Position confidence -- a scalar becomes a real PosConfidenceEllipse
# --------------------------------------------------------------------------- #
#: `SemiAxisLength ::= INTEGER{oneCentimeter(1), outOfRange(4094), unavailable(4095)} (0..4095)`
#: -- ITS-Container v1.3.1. Unit 1 cm; largest real value 4093 cm = 40.93 m. ETSI-ITS-CDD v2.1.1
#: adds `doNotUse(0)`, so under Release 2 the smallest real value is 1 cm, not 0.
SEMI_AXIS_OUT_OF_RANGE = 4094
SEMI_AXIS_UNAVAILABLE = 4095
SEMI_AXIS_LSB_M = 0.01

#: THE ASSUMPTION, stated once and cited from here everywhere else.
#:
#: The engine's confidence is the SCALAR `conf = 2.448 * sqrt(sigma^2 + bias_x^2 + bias_y^2)` (the
#: GNSS error model in `run.py`). 2.448 is the 95th percentile of a Rayleigh distribution
#: (`P(R < k*sigma) = 1 - exp(-k^2/2)`, `k = 2.4477` at 0.95), so the scalar is already **the 95 %
#: radius in metres** -- the same quantity, and the same confidence level, that ETSI's
#: `PosConfidenceEllipse` semi-axes carry.
#:
#: The error model is ISOTROPIC: one sigma, and a bias whose two components enter only through
#: their Euclidean norm. There is no directional term anywhere in it. So the honest ellipse is a
#: CIRCLE: `semiMajor == semiMinor == conf`, and `semiMajorOrientation` is not "unavailable" -- it
#: is ARBITRARY, because every orientation of a circle's major axis is equally correct. It is
#: therefore encoded as `wgs84North(0)`, the canonical representative, and NOT as
#: `unavailable(3601)`, which would assert something false (that the orientation is unknown rather
#: than immaterial).
#:
#: What this does NOT claim: that the engine's position error is really circular in the world. It
#: claims that the engine's MODEL of it is, and encoding an eccentricity the model does not have
#: would be fabrication.
POS_CONFIDENCE_ASSUMPTION = (
    "engine pos_conf is an isotropic 95% radius in metres (2.448-sigma Rayleigh); the ellipse is "
    "therefore a circle: semiMajor == semiMinor == pos_conf, semiMajorOrientation = wgs84North(0) "
    "as the arbitrary-but-canonical representative"
)


def semi_axis_to_etsi(metres: Optional[float], release: int = 1) -> int:
    """metres -> `SemiAxisLength` in centimetres."""
    if metres is None or not math.isfinite(metres) or metres < 0.0:
        return SEMI_AXIS_UNAVAILABLE
    v = iround(metres * 100.0)
    if v > 4093:
        return SEMI_AXIS_OUT_OF_RANGE
    return _clamp(v, 0 if release == 1 else 1, 4093)


def semi_axis_from_etsi(v: int) -> Optional[float]:
    if v is None or v >= SEMI_AXIS_OUT_OF_RANGE:
        return None
    return v * SEMI_AXIS_LSB_M


def pos_confidence_ellipse(pos_conf_m: Optional[float], release: int = 1) -> dict:
    """The engine's scalar -> a genuine `PosConfidenceEllipse`. See :data:`POS_CONFIDENCE_ASSUMPTION`."""
    axis = semi_axis_to_etsi(pos_conf_m, release)
    if release == 1:
        return {"semiMajorConfidence": axis, "semiMinorConfidence": axis,
                "semiMajorOrientation": 0}
    return {"semiMajorAxisLength": axis, "semiMinorAxisLength": axis,
            "semiMajorAxisOrientation": 0}


def pos_confidence_from_ellipse(ell: dict) -> Optional[float]:
    """Inverse: the ellipse's SEMI-MAJOR axis is the scalar the engine started from."""
    v = ell.get("semiMajorConfidence", ell.get("semiMajorAxisLength"))
    return semi_axis_from_etsi(v)


# --------------------------------------------------------------------------- #
# StationType
# --------------------------------------------------------------------------- #
#: `StationType ::= INTEGER {unknown(0), pedestrian(1), ...,  roadSideUnit(15)} (0..255)`
#: -- ITS-Container v1.3.1; `TrafficParticipantType` in ETSI-ITS-CDD v2.1.1 is the same value set.
STATION_TYPE_BY_NAME = {
    "unknown": 0, "pedestrian": 1, "cyclist": 2, "moped": 3, "motorcycle": 4,
    "passengerCar": 5, "bus": 6, "lightTruck": 7, "heavyTruck": 8, "trailer": 9,
    "specialVehicles": 10, "tram": 11, "roadSideUnit": 15,
}
STATION_TYPE_BY_VALUE = {v: k for k, v in STATION_TYPE_BY_NAME.items()}

#: The engine's MA-VISIBLE declaration -> the ETSI enum. TWO entries, because the engine's declared
#: `station_type` has two values (`run.py`'s broadcast pre-pass: `"vru" if tx.is_vru else "vehicle"`,
#: plus the identity-spoof attacker that fraudulently declares `"vru"`).
#:
#: **Why this is not richer.** The engine HAS a real heterogeneous fleet -- `VEHICLE_TYPES` =
#: car / motorcycle / truck / bus -- and ETSI has `passengerCar(5)` / `motorcycle(4)` /
#: `heavyTruck(8)` / `bus(6)` waiting for exactly those. But the fleet class lives on `GtVehicle`,
#: which is ORACLE: no receiver observes it, because this engine's CAM never declared it. An
#: encoder that reached for `veh.veh_type` would put ground truth into every encoded PDU -- and
#: encoded PDUs are precisely what a TS 103 759 `v2xPduEvidence` entry carries to the MA. The
#: two-valued mapping is the *correct* consequence of the firewall, not a shortcut.
#:
#: **Why `vru` -> `pedestrian(1)`.** The engine's VRU is a constant-speed point mass at
#: `vru_speed_mps` (default 1.8 m/s; the field help reads "~1.4 walking .. ~5 cycling"), i.e.
#: TS 103 300-3 VRU Profile 1. At the top of that configured range `cyclist(2)` would be the better
#: match, so the choice is exposed as the codec parameter `vru_station_type` rather than frozen
#: here -- a declared input that lands in `manifest["config"]`, not a hidden constant.
ENGINE_STATION_TYPE = {"vehicle": "passengerCar", "vru": "pedestrian"}


def station_type_to_etsi(declared: str, *, is_rsu: bool = False,
                         vru_station_type: str = "pedestrian",
                         vehicle_station_type: str = "passengerCar") -> int:
    """The MA-visible declaration -> a real `StationType` value in 0..15."""
    if is_rsu:
        return STATION_TYPE_BY_NAME["roadSideUnit"]
    name = str(declared)
    if name == "vru":
        name = vru_station_type
    elif name == "vehicle":
        name = vehicle_station_type
    if name not in STATION_TYPE_BY_NAME:
        raise ValueError(f"unknown station type {declared!r}; engine values are "
                         f"{sorted(ENGINE_STATION_TYPE)} and ETSI names are "
                         f"{sorted(STATION_TYPE_BY_NAME)}")
    return STATION_TYPE_BY_NAME[name]


def station_type_from_etsi(value: int, *, vru_station_type: str = "pedestrian",
                           vehicle_station_type: str = "passengerCar") -> str:
    """`StationType` -> the engine's two-valued declaration, which is all it can represent."""
    name = STATION_TYPE_BY_VALUE.get(int(value), "unknown")
    if name == vru_station_type:
        return "vru"
    if name == vehicle_station_type:
        return "vehicle"
    return "vru" if name in ("pedestrian", "cyclist", "moped") else "vehicle"


# --------------------------------------------------------------------------- #
# The remaining CAM high-frequency fields
# --------------------------------------------------------------------------- #
#: Every one of these is a field the ENGINE DOES NOT HAVE. ETSI names an explicit code for exactly
#: this case and encoding it is the truthful act; encoding a zero would be a fabricated measurement.
#: Sources: ITS-Container v1.3.1.
UNAVAILABLE = {
    "altitudeValue": 800001,          # AltitudeValue (-100000..800001)
    "altitudeConfidence": "unavailable",
    "headingConfidence": 127,         # HeadingConfidence (1..127), outOfRange(126)
    "speedConfidence": 127,           # SpeedConfidence (1..127)
    "vehicleLengthValue": 1023,       # VehicleLengthValue (1..1023)
    "vehicleLengthConfidenceIndication": "unavailable",
    "vehicleWidth": 62,               # VehicleWidth (1..62)
    "longitudinalAccelerationValue": 161,   # LongitudinalAccelerationValue (-160..161)
    "accelerationConfidence": 102,    # AccelerationConfidence (0..102)
    "curvatureValue": 1023,           # CurvatureValue (-1023..1023)
    "curvatureConfidence": "unavailable",
    "curvatureCalculationMode": "unavailable",
    "yawRateValue": 32767,            # YawRateValue (-32766..32767)
    "yawRateConfidence": "unavailable",
}

ACCEL_LSB_MPS2 = 0.1
LENGTH_LSB_M = 0.1
WIDTH_LSB_M = 0.1
ALTITUDE_LSB_M = 0.01


def accel_to_etsi(mps2: Optional[float]) -> int:
    """m/s^2 -> `LongitudinalAccelerationValue` in 0.1 m/s^2 (-160..160), or `unavailable(161)`."""
    if mps2 is None or not math.isfinite(mps2):
        return UNAVAILABLE["longitudinalAccelerationValue"]
    return _clamp(iround(mps2 * 10.0), -160, 160)


def accel_from_etsi(v: int) -> Optional[float]:
    return None if v is None or v == 161 else v * ACCEL_LSB_MPS2


def vehicle_length_to_etsi(metres: Optional[float]) -> int:
    """metres -> `VehicleLengthValue` in 0.1 m (1..1022), or `unavailable(1023)`."""
    if metres is None or not math.isfinite(metres) or metres <= 0.0:
        return UNAVAILABLE["vehicleLengthValue"]
    return _clamp(iround(metres * 10.0), 1, 1022)


def vehicle_length_from_etsi(v: int) -> Optional[float]:
    return None if v is None or v >= 1023 else v * LENGTH_LSB_M


def vehicle_width_to_etsi(metres: Optional[float]) -> int:
    """metres -> `VehicleWidth` in 0.1 m (1..61), or `unavailable(62)`."""
    if metres is None or not math.isfinite(metres) or metres <= 0.0:
        return UNAVAILABLE["vehicleWidth"]
    return _clamp(iround(metres * 10.0), 1, 61)


def vehicle_width_from_etsi(v: int) -> Optional[float]:
    return None if v is None or v >= 62 else v * WIDTH_LSB_M


def altitude_to_etsi(metres: Optional[float]) -> int:
    """metres -> `AltitudeValue` in 0.01 m (-100000..800000), or `unavailable(800001)`."""
    if metres is None or not math.isfinite(metres):
        return UNAVAILABLE["altitudeValue"]
    return _clamp(iround(metres * 100.0), -100000, 800000)


def altitude_from_etsi(v: int) -> Optional[float]:
    return None if v is None or v >= 800001 else v * ALTITUDE_LSB_M


# --------------------------------------------------------------------------- #
# DENM cause codes
# --------------------------------------------------------------------------- #
#: The engine's DENM `event_type` strings -> the normative `(causeCode, subCauseCode)` pair.
#: `CauseCodeType` and `DangerousSituationSubCauseCode` / `StationaryVehicleSubCauseCode` are in
#: ITS-Container v1.3.1.
#:
#: **A finding, not a translation.** `"emergencyElectronicBrakeLight"` is NOT an ETSI DENM cause
#: code. It is the name of an `ExteriorLights` bit and of a CAM concept; the DENM that announces
#: the same event is `dangerousSituation(99)` with `emergencyElectronicBrakeEngaged(1)`. The engine
#: has been emitting a CAM-vocabulary name in a DENM-shaped record. The mapping below is the
#: correction; the engine-side name is left alone because renaming it moves pinned digests.
DENM_CAUSE_CODES = {
    "stationaryVehicle": (94, 0),                    # stationaryVehicle / unavailable
    "emergencyElectronicBrakeLight": (99, 1),        # dangerousSituation / emergencyElectronicBrakeEngaged
    "emergencyElectronicBrakeEngaged": (99, 1),
    "collisionRisk": (97, 0),
    "hazardousLocation-ObstacleOnTheRoad": (10, 0),
    "humanPresenceOnTheRoad": (12, 0),
}
DENM_EVENT_BY_CAUSE = {(94, 0): "stationaryVehicle", (99, 1): "emergencyElectronicBrakeLight",
                       (97, 0): "collisionRisk", (10, 0): "hazardousLocation-ObstacleOnTheRoad",
                       (12, 0): "humanPresenceOnTheRoad"}


def denm_cause_code(event_type: Optional[str]) -> tuple:
    if event_type is None:
        return (0, 0)                                # reserved(0) -- no event declared
    if event_type not in DENM_CAUSE_CODES:
        raise ValueError(f"no ETSI cause code mapped for DENM event {event_type!r}; known: "
                         f"{sorted(DENM_CAUSE_CODES)}")
    return DENM_CAUSE_CODES[event_type]


def denm_event_type(cause: int, sub_cause: int) -> Optional[str]:
    return DENM_EVENT_BY_CAUSE.get((int(cause), int(sub_cause)))


# --------------------------------------------------------------------------- #
# The quantisation report -- MEASURED, not asserted
# --------------------------------------------------------------------------- #
#: A deterministic low-discrepancy sequence. NOT `random`: this module must be callable from a test
#: (and, in principle, from a run) without touching any RNG stream at all, and the additive
#: golden-ratio recurrence is the standard order-free way to get an equidistributed sweep. It is
#: also reproducible across interpreters, which `random.random()` is documented to be and
#: `random.uniform()` is not.
_PHI_INV = 0.6180339887498949


def _sweep(lo: float, hi: float, n: int):
    """`n` equidistributed samples in [lo, hi], deterministically, endpoints included."""
    yield lo
    yield hi
    u = 0.5
    span = hi - lo
    for _ in range(max(0, n - 2)):
        u = (u + _PHI_INV) % 1.0
        yield lo + u * span


def quantisation_report(frame: Optional[GeoFrame] = None, *, n: int = 20001,
                        epoch_unix: float = 1704067200.0,
                        map_span_m: float = 6000.0,
                        run_span_s: float = 3600.0) -> dict:
    """Round-trip every convertible field over its engine domain; return the MEASURED maximum
    error per field, beside the theoretical half-LSB bound.

    The point of measuring rather than asserting: a half-LSB claim is a claim about the *rounding*,
    and a position field also passes through a projection and two float multiplications. Only a
    sweep tells you whether the composition still lands inside the bound -- and for latitude and
    longitude it is the sweep, not the arithmetic, that shows the residual is dominated by the
    1/10 microdegree grid rather than by IEEE-754.

    Returns `{field: {unit, lsb, bound (theoretical), measured, at (worst-case input), n, note}}`.
    """
    frame = frame or GeoFrame.centred_on(48.7665, 11.4258)
    out: dict = {}

    def record(name, unit, lsb, bound, measured, at, n_s, note=""):
        out[name] = {"unit": unit, "lsb": lsb, "bound": bound, "measured": measured,
                     "at": at, "n": n_s, "note": note}

    # --- heading: deg CCW from East -> HeadingValue (0.1 deg CW from North) -----------------
    worst, at = 0.0, None
    for h in _sweep(0.0, 360.0, n):
        back = heading_from_etsi(heading_to_etsi(h))
        d = abs(((back - h + 180.0) % 360.0) - 180.0)
        if d > worst:
            worst, at = d, h
    record("heading", "deg", HEADING_LSB_DEG, HEADING_LSB_DEG / 2.0, worst, at, n,
           "circular difference; the CCW-East <-> CW-North flip is exact, only the 0.1 deg grid "
           "quantises")

    # --- speed ------------------------------------------------------------------------------
    worst, at = 0.0, None
    for s in _sweep(0.0, 70.0, n):
        d = abs(speed_from_etsi(speed_to_etsi(s)) - s)
        if d > worst:
            worst, at = d, s
    record("speed", "m/s", SPEED_LSB_MPS, SPEED_LSB_MPS / 2.0, worst, at, n,
           "domain 0..70 m/s (252 km/h) covers every engine speed; SpeedValue saturates at "
           "163.82 m/s")

    # --- position: local metres -> WGS84 1/10 microdeg -> local metres -----------------------
    worst_x, at_x, worst_y, at_y = 0.0, None, 0.0, None
    half = map_span_m / 2.0
    for u in _sweep(-half, half, n):
        lat_i, lon_i = local_to_etsi_position(frame, u, 0.0)
        bx, _ = etsi_position_to_local(frame, lat_i, lon_i)
        if abs(bx - u) > worst_x:
            worst_x, at_x = abs(bx - u), u
        lat_i, lon_i = local_to_etsi_position(frame, 0.0, u)
        _, by = etsi_position_to_local(frame, lat_i, lon_i)
        if abs(by - u) > worst_y:
            worst_y, at_y = abs(by - u), u
    record("position_x", "m", LATLON_LSB_DEG * frame.kx, LATLON_LSB_DEG * frame.kx / 2.0,
           worst_x, at_x, n,
           f"longitude grid at kx={frame.kx:.1f} m/deg; the equirectangular projection is exactly "
           f"invertible so the residual is the 1e-7 deg grid plus float error")
    record("position_y", "m", LATLON_LSB_DEG * frame.ky, LATLON_LSB_DEG * frame.ky / 2.0,
           worst_y, at_y, n, f"latitude grid at ky={frame.ky:.1f} m/deg")

    # --- position confidence ----------------------------------------------------------------
    worst, at = 0.0, None
    for c in _sweep(0.0, 40.0, n):
        back = semi_axis_from_etsi(semi_axis_to_etsi(c))
        d = abs(back - c)
        if d > worst:
            worst, at = d, c
    record("pos_confidence", "m", SEMI_AXIS_LSB_M, SEMI_AXIS_LSB_M / 2.0, worst, at, n,
           "SemiAxisLength saturates at 40.93 m -> outOfRange(4094); domain stops at 40 m")

    # --- generationDeltaTime ----------------------------------------------------------------
    worst, at = 0.0, None
    for t in _sweep(0.0, run_span_s, n):
        gdt = generation_delta_time(t, epoch_unix)
        ref = timestamp_its_ms(t, epoch_unix)
        back = engine_time_from_timestamp_its(resolve_generation_delta_time(gdt, ref), epoch_unix)
        d = abs(back - t)
        if d > worst:
            worst, at = d, t
    record("gen_time", "s", 0.001, 0.0005, worst, at, n,
           "resolved against a receiver-side reference; UNRESOLVED, generationDeltaTime is "
           "ambiguous modulo 65.536 s and that is a property of the STANDARD, not of this codec")

    # --- acceleration / dimensions / altitude ------------------------------------------------
    for name, unit, lsb, lo, hi, fwd, inv in (
            ("accel", "m/s^2", ACCEL_LSB_MPS2, -16.0, 16.0, accel_to_etsi, accel_from_etsi),
            ("length", "m", LENGTH_LSB_M, 0.1, 102.2, vehicle_length_to_etsi,
             vehicle_length_from_etsi),
            ("width", "m", WIDTH_LSB_M, 0.1, 6.1, vehicle_width_to_etsi, vehicle_width_from_etsi),
            ("altitude", "m", ALTITUDE_LSB_M, -1000.0, 8000.0, altitude_to_etsi,
             altitude_from_etsi)):
        worst, at = 0.0, None
        for v in _sweep(lo, hi, n):
            back = inv(fwd(v))
            if back is None:
                continue
            d = abs(back - v)
            if d > worst:
                worst, at = d, v
        record(name, unit, lsb, lsb / 2.0, worst, at, n, "")

    # --- exact fields ------------------------------------------------------------------------
    record("station_type", "enum", 0, 0.0, 0.0, None, len(ENGINE_STATION_TYPE),
           "EXACT for the engine's two declared values; the mapping is total and injective, so "
           "the round trip is the identity. It is LOSSY in the other direction: 13 ETSI classes "
           "collapse onto 2 engine values.")
    record("station_id", "integer", 0, 0.0, 0.0, None, 0,
           "EXACT: StationID is INTEGER(0..4294967295) and carries the engine value verbatim")
    record("cause_code", "enum", 0, 0.0, 0.0, None, len(DENM_CAUSE_CODES),
           "EXACT for every mapped DENM event type")
    return out


def format_quantisation_report(rep: dict) -> str:
    """A fixed-width table. Deterministic ordering; no locale, no float repr surprises."""
    rows = ["field            unit      lsb          bound        measured     worst-case input",
            "-" * 88]
    for k in sorted(rep):
        r = rep[k]
        at = "-" if r["at"] is None else f"{r['at']:.6g}"
        rows.append(f"{k:<16} {str(r['unit']):<9} {r['lsb']:<12.6g} {r['bound']:<12.6g} "
                    f"{r['measured']:<12.6g} {at}")
    return "\n".join(rows)
