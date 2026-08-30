"""Synthesize runnable MOSAIC scenarios from generated or real-world road networks.

Two sources, both yielding unlimited maps:

  * **procedural** — SUMO ``netgenerate`` grid / spider / random networks, parameterized by
    size and seed (offline, deterministic). Keys like ``grid_6x6``, ``spider_8a4c``, ``rand_150``
    (optionally ``…_s2`` for a generation seed).
  * **osm** — real cities imported from OpenStreetMap via ``osmGet.py`` + ``netconvert``.
    Keys like ``osm_manhattan`` from the CITIES catalogue (needs internet the first time;
    the imported ``.net.xml`` is cached under ``scms-sim/scenarios/_mapcache``).

For any key we build a self-contained route-mode scenario (SUMO owns the demand; MOSAIC
attaches our app to vehicles by matching their vType to a prototype), force the SNS radio,
and derive the MOSAIC projection from the net's ``<location>`` header.

Realism defaults (all reversible through ``SCMS_*`` env knobs, all recorded in the generated
``scms_scenario_manifest.json``):

  * **networks** get the signal/geometry flags every published SUMO city scenario uses --
    ``--tls.guess``/``--tls.guess-signals``, ``--tls.join``, ``--junctions.join``,
    ``--ramps.guess`` (``SCMS_TLS=off`` to revert). Procedural maps used to have no signals.
  * **demand** is drawn with gravity-style source/sink weights (production/attraction ~
    capacity-weighted node degree) instead of uniform randomTrips (``SCMS_OD=uniform``), and can
    follow a per-interval departure-rate profile (``SCMS_DEPART_PROFILE``).
  * **drivers** are heterogeneous: each fleet class becomes a ``<vTypeDistribution>`` of
    ``SCMS_VTYPE_SAMPLES`` jittered prototypes with ``carFollowModel="EIDM"`` and a
    ``speedFactor="normc(...)"`` per-driver desired speed (``SCMS_CF_MODEL=krauss`` to revert).
  * **lateral dynamics**: SUMO's sublane model is on (``--lateral-resolution`` 0.8 m), so lane
    changes are a continuous ~3 s traverse instead of a single-step teleport across a full lane
    width (``SCMS_LATERAL_RES=off`` to revert, ``SCMS_LATERAL_SPEED`` to retune ``maxSpeedLat``).
  * **MOSAIC<->SUMO sync** is 100 ms so the ETSI CAM rules can fire above 1 Hz
    (``SCMS_SYNC_MS=1000`` to revert).
  * **RSUs** can be placed on real junctions once the Java layer ships an RSU app
    (``SCMS_RSUS``/``SCMS_RSU_PLACEMENT``/``SCMS_RSU_APP``).

``catalog()`` enumerates a few hundred ready-made keys for the GUI / docs; any well-formed
key outside the catalogue is generated on demand just the same.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import random
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SUMO = Path(os.environ.get("SUMO_HOME", r"C:\Users\Administrator\tools\sumo\sumo-1.25.0"))
JAR = REPO / "scms-sim" / "mosaic-apps" / "scms-app" / "build" / "ScmsApp-0.1.0.jar"
MOSAIC = Path(os.environ.get("MOSAIC_HOME", r"C:\Users\Administrator\tools\mosaic"))
OUR_APP = "org.scms.app.ScmsBeaconApp"
CACHE = REPO / "scms-sim" / "scenarios" / "_mapcache"

# Real cities: key suffix -> (label, bbox as minLon,minLat,maxLon,maxLat). Small central
# extracts keep import + routing fast; enlarge the bbox for a bigger network.
CITIES = {
    "manhattan":   ("Manhattan, New York",  (-73.9900, 40.7440, -73.9680, 40.7620)),
    "sanfrancisco":("San Francisco",         (-122.4180, 37.7840, -122.3980, 37.8000)),
    "london":      ("London, City",          (-0.1050, 51.5100, -0.0800, 51.5250)),
    "paris":       ("Paris, Centre",         (2.3300, 48.8560, 2.3600, 48.8680)),
    "berlin":      ("Berlin, Mitte",         (13.3800, 52.5100, 13.4100, 52.5250)),
    "tokyo":       ("Tokyo, Chiyoda",        (139.7500, 35.6800, 139.7700, 35.6950)),
    "rome":        ("Rome, Centro",          (12.4700, 41.8900, 12.4950, 41.9050)),
    "madrid":      ("Madrid, Centro",        (-3.7100, 40.4100, -3.6900, 40.4250)),
    "amsterdam":   ("Amsterdam, Centrum",    (4.8850, 52.3650, 4.9050, 52.3780)),
    "vienna":      ("Vienna, Innere Stadt",  (16.3600, 48.2000, 16.3800, 48.2150)),
    "barcelona":   ("Barcelona, Eixample",   (2.1550, 41.3850, 2.1750, 41.4000)),
    "singapore":   ("Singapore, Downtown",   (103.8450, 1.2800, 103.8650, 1.2950)),
    "chicago":     ("Chicago, Loop",         (-87.6400, 41.8750, -87.6200, 41.8900)),
    "toronto":     ("Toronto, Downtown",     (-79.3900, 43.6450, -79.3700, 43.6600)),
    "sydney":      ("Sydney, CBD",           (151.2000, -33.8750, 151.2150, -33.8600)),
    "boston":      ("Boston, Downtown",      (-71.0650, 42.3520, -71.0500, 42.3640)),
    "losangeles":  ("Los Angeles, Downtown", (-118.2600, 34.0400, -118.2400, 34.0550)),
    "seattle":     ("Seattle, Downtown",     (-122.3400, 47.6020, -122.3250, 47.6140)),
    "washington":  ("Washington, DC",        (-77.0400, 38.8950, -77.0200, 38.9080)),
    "munich":      ("Munich, Altstadt",      (11.5650, 48.1330, 11.5850, 48.1450)),
    "mexicocity":  ("Mexico City, Centro",   (-99.1400, 19.4260, -99.1250, 19.4380)),
    "saopaulo":    ("São Paulo, Centro",     (-46.6450, -23.5550, -46.6300, -23.5430)),
    "mumbai":      ("Mumbai, Fort",          (72.8300, 18.9250, 72.8450, 18.9400)),
    "delhi":       ("New Delhi, Connaught",  (77.2150, 28.6250, 77.2300, 28.6380)),
    "shanghai":    ("Shanghai, Huangpu",     (121.4750, 31.2250, 121.4900, 31.2380)),
    "beijing":     ("Beijing, Dongcheng",    (116.3950, 39.9080, 116.4100, 39.9200)),
    "seoul":       ("Seoul, Jung-gu",        (126.9800, 37.5600, 126.9950, 37.5720)),
    "bangkok":     ("Bangkok, Phra Nakhon",  (100.4950, 13.7500, 100.5100, 13.7620)),
    "istanbul":    ("Istanbul, Fatih",       (28.9600, 41.0050, 28.9750, 41.0170)),
    "moscow":      ("Moscow, Tverskoy",      (37.6050, 55.7550, 37.6200, 55.7670)),
    "dublin":      ("Dublin, Centre",        (-6.2700, 53.3400, -6.2550, 53.3520)),
    "lisbon":      ("Lisbon, Baixa",         (-9.1450, 38.7080, -9.1300, 38.7200)),
    "stockholm":   ("Stockholm, Norrmalm",   (18.0550, 59.3300, 18.0700, 59.3420)),
    "copenhagen":  ("Copenhagen, Indre By",  (12.5650, 55.6750, 12.5800, 55.6870)),
    "zurich":      ("Zürich, Altstadt",      (8.5350, 47.3680, 8.5500, 47.3800)),
    "brussels":    ("Brussels, Centre",      (4.3450, 50.8420, 4.3600, 50.8540)),
    "prague":      ("Prague, Staré Město",   (14.4150, 50.0800, 14.4300, 50.0920)),
    "warsaw":      ("Warsaw, Śródmieście",   (21.0050, 52.2280, 21.0200, 52.2400)),
    "athens":      ("Athens, Centre",        (23.7250, 37.9750, 23.7400, 37.9870)),
    "milan":       ("Milan, Centro",         (9.1850, 45.4600, 9.2000, 45.4720)),
    "frankfurt":   ("Frankfurt, Altstadt",   (8.6750, 50.1080, 8.6900, 50.1200)),
    "montreal":    ("Montréal, Centre-ville",(-73.5700, 45.5000, -73.5550, 45.5120)),
    "vancouver":   ("Vancouver, Downtown",   (-123.1250, 49.2800, -123.1100, 49.2900)),
    "austin":      ("Austin, Downtown",      (-97.7450, 30.2650, -97.7300, 30.2770)),
    "denver":      ("Denver, Downtown",      (-104.9950, 39.7400, -104.9800, 39.7520)),
    "miami":       ("Miami, Downtown",       (-80.1950, 25.7700, -80.1850, 25.7800)),
    "philadelphia":("Philadelphia, Center",  (-75.1650, 39.9480, -75.1500, 39.9600)),
    "dallas":      ("Dallas, Downtown",      (-96.8050, 32.7780, -96.7900, 32.7880)),
    "sandiego":    ("San Diego, Downtown",   (-117.1650, 32.7100, -117.1500, 32.7200)),
    "hongkong":    ("Hong Kong, Central",     (114.1500, 22.2780, 114.1650, 22.2880)),
    "kualalumpur": ("Kuala Lumpur, KLCC",     (101.7100, 3.1500, 101.7250, 3.1600)),
    "jakarta":     ("Jakarta, Menteng",       (106.8300, -6.1950, 106.8450, -6.1850)),
    "manila":      ("Manila, Ermita",         (120.9800, 14.5750, 120.9900, 14.5850)),
    "bogota":      ("Bogota, La Candelaria",  (-74.0800, 4.5950, -74.0700, 4.6050)),
    "santiago":    ("Santiago, Centro",       (-70.6550, -33.4450, -70.6450, -33.4350)),
    "buenosaires": ("Buenos Aires, Centro",   (-58.3850, -34.6100, -58.3700, -34.6000)),
    "lima":        ("Lima, Centro",           (-77.0350, -12.0500, -77.0250, -12.0420)),
    "capetown":    ("Cape Town, CBD",         (18.4150, -33.9250, 18.4280, -33.9150)),
    "nairobi":     ("Nairobi, CBD",           (36.8150, -1.2870, 36.8280, -1.2780)),
    "lagos":       ("Lagos, Island",          (3.3900, 6.4500, 3.4020, 6.4580)),
    "tehran":      ("Tehran, Centre",         (51.4100, 35.6950, 51.4220, 35.7050)),
    "riyadh":      ("Riyadh, Olaya",          (46.6800, 24.6900, 46.6920, 24.7000)),
    "telaviv":     ("Tel Aviv, Centre",       (34.7700, 32.0700, 34.7820, 32.0800)),
    "helsinki":    ("Helsinki, Kluuvi",       (24.9350, 60.1680, 24.9480, 60.1750)),
    "oslo":        ("Oslo, Sentrum",          (10.7350, 59.9100, 10.7480, 59.9160)),
    "kyiv":        ("Kyiv, Centre",           (30.5150, 50.4450, 30.5280, 50.4520)),
}


# ---------------------------------------------------------------------------
# shared config knobs (read by both mapgen and gen_scenario, applied to SUMO + SNS)
# ---------------------------------------------------------------------------
def _envf(name: str, default: float) -> float:
    try:
        v = os.environ.get(name)
        return float(v) if v not in (None, "") else float(default)
    except ValueError:
        return float(default)


def _envi(name: str, default: int) -> int:
    try:
        v = os.environ.get(name)
        return int(round(float(v))) if v not in (None, "") else int(default)
    except ValueError:
        return int(default)


def _envs(name: str, default: str) -> str:
    v = os.environ.get(name)
    return v.strip() if v not in (None, "") else default


def _envb(name: str, default: bool) -> bool:
    v = os.environ.get(name)
    if v in (None, ""):
        return bool(default)
    return v.strip().lower() in ("1", "true", "yes", "on", "y")


def sim_step_ms() -> int:
    """SUMO step-length in ms (SCMS_SIM_STEP seconds). 0.1 s enables up to 10 Hz ETSI CAMs."""
    return max(50, int(round(_envf("SCMS_SIM_STEP", 0.1) * 1000)))


def sync_ms() -> int:
    """MOSAIC<->SUMO sync period in ms (the SUMO federate's ``updateInterval``).

    100 ms by default on EVERY map — that is what lets the ETSI EN 302 637-2 CAM rules fire
    above 1 Hz and gives the app a 100 ms channel-busy window. ``SCMS_SYNC_MS=1000`` restores
    the old 1 Hz behaviour of the bundled/InTAS scenarios (and is ~10x faster on InTAS).
    An explicit ``SCMS_SIM_STEP`` still drives the sync period when SCMS_SYNC_MS is unset,
    so the historical single-knob usage keeps working."""
    v = os.environ.get("SCMS_SYNC_MS")
    if v not in (None, ""):
        return max(50, _envi("SCMS_SYNC_MS", 100))
    return sim_step_ms()


def align_sync_ms(step_ms: int, sync: int = None) -> int:
    """MOSAIC's updateInterval must be a whole multiple of the SUMO step-length it drives."""
    s = int(sync if sync is not None else sync_ms())
    step = max(1, int(step_ms))
    return max(step, int(round(s / step)) * step)


# ---- driver-model / fleet realism knobs -----------------------------------------------------
# SUMO car-following model applied to every generated vType (SCMS_CF_MODEL). EIDM (Salles et al.,
# SUMO Conf 2020) is the human-like extended-IDM model and the default here; 'krauss' reverts to
# SUMO's stock model. NB: EIDM uses log() internally, a documented cross-platform reproducibility
# caveat -- MOSAIC-layer determinism is same-host only anyway (see docs/realism/ROADMAP.md §4).
_CF_MODELS = {
    "eidm": "EIDM", "krauss": "Krauss", "kraussorig1": "KraussOrig1", "idm": "IDM",
    "idmm": "IDMM", "acc": "ACC", "cacc": "CACC", "w99": "W99", "wiedemann": "Wiedemann",
    "": "", "off": "", "none": "", "default": "",
}


def cf_model() -> str:
    """SUMO ``carFollowModel`` for generated vTypes ('' = leave SUMO's default alone)."""
    raw = _envs("SCMS_CF_MODEL", "eidm").lower()
    return _CF_MODELS.get(raw, "EIDM" if raw == "eidm" else raw)


def speed_dev() -> float:
    """Std-dev of the per-driver desired-speed factor (SUMO's own passenger default is 0.1)."""
    return max(0.0, _envf("SCMS_SPEED_DEV", 0.1))


def speed_factor_spec() -> str:
    """Per-driver desired-speed distribution, SUMO ``normc(mean,dev,min,max)`` syntax.

    This is the single biggest cheap realism win: without it every vehicle drives exactly at the
    edge speed limit (MOSAIC hard-writes ``speedDev="0.0"`` into its generated vType file, so a
    scalar speedFactor collapses the distribution)."""
    custom = _envs("SCMS_SPEEDFACTOR_DIST", "")
    if custom:
        return custom
    d = speed_dev()
    mean = _envf("SCMS_VEH_SPEEDFACTOR", 1.0)
    if d <= 0:                                   # SCMS_SPEED_DEV=0 -> the old fixed-speed fleet
        return f"{mean:g}"
    return f"normc({mean:g},{d:g},{max(0.05, mean - 3 * d):g},{mean + 3 * d:g})"


def vtype_samples() -> int:
    """Driver prototypes sampled per fleet class (SCMS_VTYPE_SAMPLES; <=1 = one uniform type)."""
    return max(1, min(64, _envi("SCMS_VTYPE_SAMPLES", 8)))


# ---- lateral dynamics: SUMO's sublane model --------------------------------------------------
# Without it SUMO pins every vehicle to the lane CENTRELINE and a lane change is an instantaneous
# one-step teleport of a full lane width (3.2 m on a default lane) -- which is exactly the 3.2 m
# single-sample jump the raw MOSAIC traces show, and exactly the signature a V2X position-
# plausibility detector keys on. `--lateral-resolution` divides each lane into sublanes, switches
# the lane-change model to SL2015 and makes the vehicle walk across at `maxSpeedLat`.
_LATERAL_OFF = ("off", "none", "no", "0", "false", "-1", "instant", "centreline", "centerline")


def lateral_res() -> float:
    """SUMO ``--lateral-resolution`` in m (``SCMS_LATERAL_RES``). <=0 / 'off' = no sublane model.

    0.8 m is the default because:

      * SUMO's default lane width is 3.2 m, so 0.8 splits it into exactly 4 equal sublanes. That is
        the SUMO manual's own worked example (Simulation/SublaneModel.html, "Model Details") and it
        avoids the reduced-width leftmost stripe the manual warns about for values that do not
        divide the lane evenly. It matters here: 26 243 of the 33 204 lanes in InTAS's
        ingolstadt.net.xml carry no ``width`` attribute, i.e. they are exactly 3.2 m.
      * it is at or below the width of the narrowest motorised vehicle SUMO simulates (motorcycle
        0.9 m, moped 0.8 m), the manual's other sizing rule -- so two-wheelers still get a stripe.
      * runtime grows as lane_width / resolution (the manual: "The smaller the value of
        --lateral-resolution, the higher the running time"), so 0.8 is the COARSEST value that
        satisfies both rules.
    """
    raw = _envs("SCMS_LATERAL_RES", "0.8").lower()
    if raw in _LATERAL_OFF:
        return 0.0
    v = _envf("SCMS_LATERAL_RES", 0.8)
    return v if v > 0 else 0.0


def sublane_on() -> bool:
    """True when the sublane model (continuous lateral movement) is active."""
    return lateral_res() > 0


def lateral_speed() -> float:
    """vType ``maxSpeedLat`` in m/s (``SCMS_LATERAL_SPEED``).

    SUMO's own default is 1.0 m/s, which walks a vehicle across a 3.2 m lane in ~3.2 s plus the
    ``lcAccelLat=1 m/s^2`` ramp -- inside the 2-4 s a real lane change takes. Kept as the default
    here so the sublane runs stay comparable to stock SUMO."""
    return max(0.05, _envf("SCMS_LATERAL_SPEED", 1.0))


def lateral_vtype_attrs() -> dict:
    """Sublane vType attributes for the vTypes WE own (generated route files + MOSAIC flow types).

    Empty when the sublane model is off, so opting out restores the previous XML byte for byte.

    ``laneChangeModel`` is deliberately NOT set: SUMO selects **SL2015** automatically as soon as
    ``--lateral-resolution`` is given ("The lane-changing model SL2015 is automatically used when
    enabling the sublane model", Simulation/SublaneModel.html), and pinning it here would silently
    override a scenario that asked for a different model.

    ``lcMaxSpeedLatStanding="0"`` is the one non-default value. SUMO's default for it is
    ``maxSpeedLat``, i.e. effectively disabled, which lets a STOPPED vehicle slide sideways at
    1 m/s -- the manual calls this "orthogonal sliding" and says to set 0 to avoid it. With 0 the
    lateral speed bound becomes ``0 + lcMaxSpeedLatFactor * speed``, so lateral motion requires
    forward motion, which is what steering physically does. SUMO suspends the bound at the end of a
    lane, so it cannot deadlock.
    """
    if not sublane_on():
        return {}
    return {"maxSpeedLat": f"{lateral_speed():g}", "lcMaxSpeedLatStanding": "0"}


def vtype_jitter() -> float:
    """Relative std-dev of the per-prototype tau/accel/decel/minGap/length jitter."""
    return max(0.0, min(1.0, _envf("SCMS_VTYPE_JITTER", 0.15)))


def veh_params() -> dict:
    """SUMO driver-model / vehicle-type parameters from env (SCMS_VEH_*)."""
    return {
        "maxSpeed": _envf("SCMS_VEH_MAXSPEED", 42.0),   # ~151 km/h — realistic passenger-car cap
        "accel": _envf("SCMS_VEH_ACCEL", 2.6),
        "decel": _envf("SCMS_VEH_DECEL", 4.5),
        "sigma": _envf("SCMS_VEH_SIGMA", 0.5),        # driver imperfection 0..1
        "minGap": _envf("SCMS_VEH_MINGAP", 2.5),
        "tau": _envf("SCMS_VEH_TAU", 1.0),            # reaction time / headway
        "length": _envf("SCMS_VEH_LENGTH", 5.0),
        "speedFactor": _envf("SCMS_VEH_SPEEDFACTOR", 1.0),
    }


def fleet() -> list[tuple]:
    """Vehicle-type mix (name, spawn weight, SUMO vType attrs). SCMS_FLEET=car for homogeneous."""
    vp = veh_params()
    car = ("car", 0.78, {"vClass": "passenger", "accel": vp["accel"], "decel": vp["decel"],
                         "length": vp["length"], "minGap": vp["minGap"], "tau": vp["tau"],
                         "maxSpeed": vp["maxSpeed"], "speedFactor": vp["speedFactor"]})
    if os.environ.get("SCMS_FLEET", "mixed").lower() == "car":
        return [(car[0], 1.0, car[2])]
    return [
        car,
        ("truck", 0.08, {"vClass": "truck", "accel": 1.3, "decel": 4.0, "length": 12.0,
                         "minGap": 3.0, "tau": 1.4, "maxSpeed": 36.0}),
        ("bus", 0.04, {"vClass": "bus", "accel": 1.2, "decel": 4.0, "length": 12.0,
                       "minGap": 3.0, "tau": 1.4, "maxSpeed": 25.0}),
        ("moto", 0.10, {"vClass": "motorcycle", "accel": 3.5, "decel": 6.0, "length": 2.2,
                        "minGap": 1.5, "tau": 0.8, "maxSpeed": 45.0}),
    ]


# vType attributes that get per-driver multiplicative jitter (bounds keep them physical).
_JITTER_BOUNDS = {
    "accel":  (0.5, 6.0),
    "decel":  (1.5, 9.0),
    "tau":    (0.4, 3.0),
    "minGap": (0.5, 8.0),
    "length": (1.8, 20.0),
}


def _sattrs(attrs: dict) -> dict:
    """XML attribute dict: numbers rendered without trailing float noise."""
    out = {}
    for k, v in attrs.items():
        out[k] = f"{v:g}" if isinstance(v, (int, float)) and not isinstance(v, bool) else str(v)
    return out


def _jitter(rng: random.Random, value: float, rel: float, lo: float, hi: float) -> float:
    """Truncated-normal multiplicative jitter, rounded so the XML stays byte-stable."""
    f = rng.gauss(1.0, rel)
    f = min(1.0 + 2.0 * rel, max(1.0 - 2.0 * rel, f))
    return round(min(hi, max(lo, value * f)), 3)


def driver_population(seed: int = 0) -> list[tuple[str, float, list[tuple[str, dict]]]]:
    """The heterogeneous driver population: one vTypeDistribution per fleet class.

    Returns ``[(class_name, spawn_weight, [(vtype_id, attrs), ...]), ...]``. Each class expands to
    ``SCMS_VTYPE_SAMPLES`` jittered driver prototypes (tau/accel/decel/minGap/length drawn from a
    truncated normal around the class base, keyed on the scenario seed so the population is
    reproducible) and every member carries ``carFollowModel`` + a ``speedFactor`` distribution.
    ``SCMS_VTYPE_SAMPLES=1`` reproduces the old single-vType-per-class fleet."""
    n = vtype_samples()
    rel = vtype_jitter()
    cf = cf_model()
    sf = speed_factor_spec()
    dev = speed_dev()
    lat = lateral_vtype_attrs()          # {} unless the sublane model is on
    out: list[tuple[str, float, list[tuple[str, dict]]]] = []
    for name, weight, base in fleet():
        members: list[tuple[str, dict]] = []
        for i in range(n):
            attrs = dict(base)
            if n > 1 and rel > 0:
                rng = random.Random(f"{seed}:vtype:{name}:{i}")
                for key, (lo, hi) in _JITTER_BOUNDS.items():
                    if key in attrs:
                        attrs[key] = _jitter(rng, float(attrs[key]), rel, lo, hi)
                attrs["sigma"] = round(min(0.9, max(0.0, rng.gauss(0.5, 0.15))), 3)
            if cf:
                attrs["carFollowModel"] = cf
            attrs["speedFactor"] = sf
            attrs["speedDev"] = dev          # defeats MOSAIC's hard-coded speedDev="0.0"
            attrs.update(lat)                # sublane lateral dynamics (maxSpeedLat, ...)
            members.append((f"{name}_{i:02d}" if n > 1 else name, attrs))
        out.append((name, weight, members))
    return out


def prototype_names(seed: int = 0) -> list[str]:
    """Every vType id a SUMO vehicle can carry, plus the distribution ids MOSAIC may report.

    MOSAIC matches a route-file vehicle to a mapping prototype by its SUMO vType name, so the
    mapping must list every concrete member of every vTypeDistribution."""
    names: list[str] = []
    for cls, _w, members in driver_population(seed):
        if len(members) > 1:
            names.append(cls)                      # the distribution id itself
        names.extend(mid for mid, _a in members)
    return names


def radio_range_m() -> float:
    """SNS single-hop radius in metres (SCMS_RADIO_RANGE). Recorded in the manifest so the realism
    benchmark can reconstruct link distances without being told the radio range by hand."""
    return _envf("SCMS_RADIO_RANGE", 709.4)


def write_sns_config(dst: Path):
    """Per-scenario SNS radio config: SCMS_RADIO_RANGE (m) + SCMS_RADIO_LOSS (0..1)."""
    sns = {
        "maximumTtl": 10,
        "singlehopRadius": radio_range_m(),
        "adhocTransmissionModel": {"type": "SophisticatedAdhocTransmissionModel"},
        "singlehopDelay": {"type": "SimpleRandomDelay", "steps": 5,
                           "minDelay": "0.4 ms", "maxDelay": "2.4 ms"},
        "singleHopTransmission": {"lossProbability": _envf("SCMS_RADIO_LOSS", 0.0),
                                  "maxRetries": 0},
    }
    (dst / "sns").mkdir(exist_ok=True)
    (dst / "sns" / "sns_config.json").write_text(json.dumps(sns, indent=2), encoding="utf-8")


# ---------------------------------------------------------------------------
# infrastructure (RSU) placement -- needs SUMO(x,y) -> WGS84, and MOSAIC wants GeoPoints
# ---------------------------------------------------------------------------
RSU_APP = "org.scms.app.ScmsRsuApp"


def rsu_app() -> str:
    return _envs("SCMS_RSU_APP", RSU_APP)


def rsu_placement() -> str:
    """junction | grid | keep — 'keep' only preserves whatever the source scenario shipped."""
    p = _envs("SCMS_RSU_PLACEMENT", "junction").lower()
    return p if p in ("junction", "grid", "keep") else "junction"


def app_in_jar(cls: str, jar: Path = None) -> bool:
    """True when the built app jar actually contains `cls` (assigning a missing/vehicle-only app
    to an RSU unit crashes MOSAIC at unit start-up, which is why RSUs were stripped wholesale)."""
    jar = jar or JAR
    try:
        import zipfile
        with zipfile.ZipFile(jar) as z:
            return (cls.replace(".", "/") + ".class") in z.namelist()
    except Exception:
        return False


def rsu_count() -> int:
    """SCMS_RSUS: explicit count, 0 = none, 'auto' = 8 once the RSU app exists in the jar.

    Default is 'auto' so that RSU-equipped runs light up the moment the Java layer ships
    ``org.scms.app.ScmsRsuApp``, and stay off (byte-identical to the historical behaviour)
    until then."""
    raw = _envs("SCMS_RSUS", "auto").lower()
    if raw in ("auto", "on", "yes", "true"):
        return 8 if app_in_jar(rsu_app()) else 0
    return max(0, _envi("SCMS_RSUS", 0))


def _net_location(net: Path) -> dict:
    """The <location .../> header of a SUMO net (netOffset / convBoundary / proj zone)."""
    txt = ""
    with open(net, "r", encoding="utf-8", errors="replace") as fh:
        for _ in range(400):                      # the header sits in the first few lines
            line = fh.readline()
            if not line:
                break
            txt += line
            if "<location" in line:
                break
    m = re.search(r"<location[^>]*>", txt)
    if not m:
        return {}
    tag = m.group(0)

    def attr(name):
        a = re.search(name + r'="([^"]*)"', tag)
        return a.group(1) if a else ""
    off = [float(v) for v in attr("netOffset").split(",")] if attr("netOffset") else [0.0, 0.0]
    conv = [float(v) for v in attr("convBoundary").split(",")] if attr("convBoundary") else []
    proj = attr("projParameter")
    zm = re.search(r"\+zone=(\d+)", proj)
    return {"netOffset": off, "convBoundary": conv, "proj": proj,
            "zone": int(zm.group(1)) if zm else 0,
            "north": "+south" not in proj}


# ---------------------------------------------------------------------------
# network classification -- so a MOSAIC dataset can be scored by datagen.realism_bench
# ---------------------------------------------------------------------------
# realism_bench resolves its reference SPEED / HEADWAY bands from the manifest's `road_network`,
# using the pure-Python engine's vocabulary (grid|ring|spider|custom|osm are urban, linear is
# highway). A MOSAIC dataset has no such field, so the generator has to supply an honest one.
_HIGHWAY_MEAN_LIMIT_MS = 22.22        # 80 km/h length-weighted mean -> score against highway refs
_LANE_RE = re.compile(r'<lane\b[^>]*\bspeed="([0-9.eE+-]+)"[^>]*\blength="([0-9.eE+-]+)"'
                      r'|<lane\b[^>]*\blength="([0-9.eE+-]+)"[^>]*\bspeed="([0-9.eE+-]+)"')


def net_speed_profile(net: Path) -> dict:
    """Length-weighted speed-limit profile of a SUMO net: {mean_limit_ms, max_limit_ms, lanes}.

    Streamed with a regex over ``<lane>`` elements rather than sumolib, so it works with no
    SUMO_HOME and costs one pass over the file (InTAS's ingolstadt.net.xml is 17 MB)."""
    tot_len = 0.0
    tot_ls = 0.0
    mx = 0.0
    n = 0
    try:
        with open(net, "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if "<lane" not in line:
                    continue
                for m in _LANE_RE.finditer(line):
                    spd = float(m.group(1) or m.group(4))
                    ln = float(m.group(2) or m.group(3))
                    if ln <= 0:
                        continue
                    tot_len += ln
                    tot_ls += ln * spd
                    mx = max(mx, spd)
                    n += 1
    except OSError:
        return {}
    if not tot_len:
        return {}
    return {"mean_limit_ms": round(tot_ls / tot_len, 3), "max_limit_ms": round(mx, 3), "lanes": n}


_ROAD_NETWORK_TOKENS = ("grid", "ring", "spider", "custom", "osm", "linear")


def road_network_token(key: str, net: Path = None) -> tuple[str, dict]:
    """``(road_network, evidence)`` in the python engine's vocabulary, for the dataset manifest.

    Resolution order, most authoritative first:

    1. ``SCMS_ROAD_NETWORK`` — an explicit operator override (the same knob ScmsBackend reads).
    2. Procedural keys, which carry their topology in the key itself.
    3. **InTAS variants**, which are classified from the DEMAND, not the net: ``InTAS_highway_*``
       and ``InTAS_urban_*`` ship the byte-identical ``ingolstadt.net.xml`` (md5
       89f9dcde4394fcc0c4a468929174536f, 16.9 MB) and differ only in their route sets, so the net's
       length-weighted mean limit (13.78 m/s, dominated by the city streets both variants sit on)
       reads 'urban' for both and would score the motorway runs against urban speed/headway bands.
    4. Everything else (curated bundle maps, OSM imports) from the NET: a length-weighted mean speed
       limit at or above 80 km/h reads as a motorway corridor ('linear'), below as a street network.

    The evidence dict is recorded next to the token so the call is auditable instead of a guess."""
    k = (key or "").lower()
    prof = net_speed_profile(net) if net else {}
    forced = _envs("SCMS_ROAD_NETWORK", "").lower()
    if forced in _ROAD_NETWORK_TOKENS:
        return forced, {"from": "SCMS_ROAD_NETWORK override", **prof}
    if k.startswith("grid_"):
        return "grid", {"from": "scenario key"}
    if k.startswith("spider_"):
        return "spider", {"from": "scenario key"}
    if k.startswith("rand_"):
        return "custom", {"from": "scenario key"}
    if k.startswith("intas_highway"):
        return "linear", {"from": "InTAS route set (motorway demand)", **prof,
                          "note": "the InTAS highway and urban variants ship the SAME "
                                  "ingolstadt.net.xml; only the route set distinguishes them, so "
                                  "the net's speed limits cannot classify this scenario"}
    if k.startswith("intas_urban"):
        return "osm", {"from": "InTAS route set (urban demand)", **prof,
                       "note": "same shared ingolstadt.net.xml as the highway variants"}
    if not prof:
        return "osm", {"from": "default (net unreadable)"}
    token = "linear" if prof["mean_limit_ms"] >= _HIGHWAY_MEAN_LIMIT_MS else "osm"
    return token, {"from": "net speed limits", **prof,
                   "threshold_ms": _HIGHWAY_MEAN_LIMIT_MS}


def regime_of(token: str) -> str:
    """urban | highway — the reference-band regime realism_bench derives from `road_network`."""
    return "highway" if token == "linear" else "urban"


def _utm_to_lonlat(easting: float, northing: float, zone: int, north: bool = True):
    """Inverse UTM (Snyder series, WGS-84) — pure stdlib, no pyproj dependency."""
    a, f = 6378137.0, 1.0 / 298.257223563
    e2 = f * (2 - f)
    k0 = 0.9996
    x = easting - 500000.0
    y = northing if north else northing - 10000000.0
    mu = (y / k0) / (a * (1 - e2 / 4 - 3 * e2 ** 2 / 64 - 5 * e2 ** 3 / 256))
    e1 = (1 - math.sqrt(1 - e2)) / (1 + math.sqrt(1 - e2))
    fp = (mu
          + (3 * e1 / 2 - 27 * e1 ** 3 / 32) * math.sin(2 * mu)
          + (21 * e1 ** 2 / 16 - 55 * e1 ** 4 / 32) * math.sin(4 * mu)
          + (151 * e1 ** 3 / 96) * math.sin(6 * mu)
          + (1097 * e1 ** 4 / 512) * math.sin(8 * mu))
    ep2 = e2 / (1 - e2)
    c1 = ep2 * math.cos(fp) ** 2
    t1 = math.tan(fp) ** 2
    s = math.sin(fp)
    r1 = a * (1 - e2) / (1 - e2 * s * s) ** 1.5
    n1 = a / math.sqrt(1 - e2 * s * s)
    d = x / (n1 * k0)
    lat = fp - (n1 * math.tan(fp) / r1) * (
        d ** 2 / 2
        - (5 + 3 * t1 + 10 * c1 - 4 * c1 ** 2 - 9 * ep2) * d ** 4 / 24
        + (61 + 90 * t1 + 298 * c1 + 45 * t1 ** 2 - 3 * c1 ** 2 - 252 * ep2) * d ** 6 / 720)
    lon0 = math.radians((zone - 1) * 6 - 180 + 3)
    lon = lon0 + (d
                  - (1 + 2 * t1 + c1) * d ** 3 / 6
                  + (5 - 2 * c1 + 28 * t1 - 3 * c1 ** 2 + 8 * ep2 + 24 * t1 ** 2) * d ** 5 / 120
                  ) / math.cos(fp)
    return math.degrees(lat), math.degrees(lon)


def _junction_sites(net: Path, limit: int = 20000) -> list[tuple[float, float, int, bool]]:
    """(x, y, incoming-lane count, is_traffic_light) for every real junction of a SUMO net."""
    sites: list[tuple[float, float, int, bool]] = []
    for _ev, el in ET.iterparse(str(net), events=("end",)):
        if el.tag != "junction":
            el.clear()
            continue
        jtype = el.get("type", "")
        if jtype not in ("internal",) and el.get("x") is not None:
            inc = el.get("incLanes", "")
            sites.append((float(el.get("x")), float(el.get("y")),
                          len([c for c in inc.split() if c]), jtype == "traffic_light"))
        el.clear()
        if len(sites) >= limit:
            break
    return sites


def _spread(cands: list[tuple], n: int) -> list[tuple]:
    """Greedy max-min spacing so N RSUs cover the map instead of clumping on one arterial."""
    if n >= len(cands):
        return list(cands)
    picked = [cands[0]]
    rest = list(cands[1:])
    while len(picked) < n and rest:
        best, bestd = None, -1.0
        for c in rest:
            d = min((c[0] - p[0]) ** 2 + (c[1] - p[1]) ** 2 for p in picked)
            if d > bestd:
                best, bestd = c, d
        picked.append(best)
        rest.remove(best)
    return picked


def utm_zone_of(longitude: float) -> int:
    """The UTM zone MOSAIC derives from a scenario's centerCoordinates."""
    return int((float(longitude) + 180.0) / 6.0) % 60 + 1


def rsu_units(net: Path, count: int, app: str, placement: str = "junction",
              proj: tuple = None) -> list[dict]:
    """MOSAIC mapping ``rsus`` entries placed on the real network (GeoPoint positions).

    ``junction`` prefers signalised, then high-degree junctions (where real C-ITS road-side
    units go); ``grid`` lays a regular lattice over the network bounding box. ``proj`` is
    ``(offset_x, offset_y, utm_zone, northern)`` and should come from the scenario config MOSAIC
    will actually use (``cartesian = UTM + cartesianOffset``); it defaults to the net's own
    ``<location>`` header."""
    if count <= 0:
        return []
    loc = _net_location(net)
    conv = loc.get("convBoundary") or []
    if len(conv) != 4:
        print(f"[mapgen] cannot place RSUs: {net.name} has no <location convBoundary>",
              file=sys.stderr)
        return []
    if proj:
        ox, oy, zone, north = proj
    else:
        _center, offset, zone, north = net_projection(net)
        ox, oy = offset["x"], offset["y"]
    x0, y0, x1, y1 = conv
    if placement == "grid":
        cols = max(1, int(round(math.sqrt(count))))
        rows = max(1, int(math.ceil(count / cols)))
        pts = [(x0 + (x1 - x0) * (i + 0.5) / cols, y0 + (y1 - y0) * (j + 0.5) / rows)
               for j in range(rows) for i in range(cols)][:count]
    else:
        sites = _junction_sites(net)
        if not sites:
            return []
        sites.sort(key=lambda s: (-int(s[3]), -s[2], s[0], s[1]))
        pool = sites[:max(count * 12, count)]
        pts = [(s[0], s[1]) for s in _spread(pool, count)]
    units = []
    for i, (x, y) in enumerate(pts):
        lat, lon = _utm_to_lonlat(x - ox, y - oy, zone, north)
        units.append({"name": f"rsu_{i:03d}", "group": "scms-rsu",
                      "position": {"latitude": round(lat, 7), "longitude": round(lon, 7)},
                      "applications": [app]})
    return units


# ---------------------------------------------------------------------------
# scenario provenance manifest (replayability of a MOSAIC run)
# ---------------------------------------------------------------------------
MANIFEST_NAME = "scms_scenario_manifest.json"
INPUTS_NAME = "scms_inputs.json"             # side-car the Java back-end inlines into manifest.json
_MANIFEST_HASH_MAX = 96 * 1024 * 1024        # skip hashing the ~1 GB InTAS route set

# Resolved knobs the Java back-end lifts into manifest.json["config"] (schema "scms.inputs/1"
# params block). These are exactly the fields datagen.realism_bench needs to resolve a MOSAIC
# dataset's traffic regime and its acceptanceRangeThreshold normaliser without a --regime flag.
_INPUT_PARAM_KEYS = ("duration", "seed", "scale", "max_vehicles", "target_flow", "lanes",
                     "road_network", "regime", "radio_range_m", "sumo_step_ms", "mosaic_sync_ms",
                     "car_follow_model", "speed_factor", "speed_dev", "od_mode", "rsus",
                     "lateral_resolution_m", "lane_change_model")


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def file_fingerprint(path: Path, rel_to: Path = None) -> dict:
    """{path, bytes, sha256} — sha256 omitted (with a reason) for oversized inputs."""
    try:
        size = path.stat().st_size
    except OSError:
        return {"path": str(path), "missing": True}
    name = str(path)
    if rel_to:
        try:
            name = str(path.relative_to(rel_to))
        except ValueError:
            pass
    out = {"path": name.replace("\\", "/"), "bytes": size}
    cap = max(0, _envi("SCMS_MANIFEST_HASH_MAX_MB", _MANIFEST_HASH_MAX // (1024 * 1024)))
    cap *= 1024 * 1024
    if size <= cap:
        out["sha256"] = _sha256(path)
    else:
        out["sha256"] = None
        out["sha256_skipped"] = "larger than SCMS_MANIFEST_HASH_MAX_MB"
    return out


def effective_env() -> dict:
    """Every SCMS_* variable actually visible to this generation (sorted, values as strings)."""
    return {k: os.environ[k] for k in sorted(os.environ) if k.startswith("SCMS_")}


def _tool_versions() -> dict:
    v = {"python": sys.version.split()[0]}
    try:
        r = subprocess.run([str(SUMO / "bin" / "sumo.exe"), "--version"],
                           capture_output=True, text=True, timeout=30)
        m = re.search(r"SUMO\s+sumo\s+(\S+)", r.stdout)
        v["sumo"] = m.group(1) if m else (r.stdout.splitlines() or [""])[0].strip()
    except Exception:
        v["sumo"] = None
    try:
        jars = sorted((MOSAIC / "lib" / "mosaic").glob("mosaic-starter-*.jar"))
        v["mosaic"] = jars[-1].stem.replace("mosaic-starter-", "") if jars else None
    except Exception:
        v["mosaic"] = None
    return v


def write_scenario_manifest(dst: Path, key: str, kind: str, resolved: dict,
                            inputs: list[Path]) -> Path:
    """Write the replay manifest for a generated MOSAIC scenario.

    The Java back-end writes the dataset manifest at JVM exit and has no view of how the scenario
    was generated; this file closes that gap (effective SCMS_* env + sha256 of every scenario
    input + tool versions). run.ps1 copies it into the dataset dir as scenario_provenance.json."""
    fps = [file_fingerprint(p, dst) for p in inputs if p and Path(p).exists()]
    tools = _tool_versions()
    man = {
        "schema": "scms.scenario_manifest/1",
        "generator": "scms-sim/scenarios (gen_scenario+mapgen)",
        "scenario_key": key,
        "kind": kind,
        "resolved": resolved,
        "env": effective_env(),
        "tools": tools,
        "inputs": fps,
    }
    out = dst / MANIFEST_NAME
    out.write_text(json.dumps(man, indent=2, sort_keys=False) + "\n", encoding="utf-8")
    write_inputs_sidecar(dst, key, resolved, fps, tools)
    return out


def write_inputs_sidecar(dst: Path, key: str, resolved: dict, fingerprints: list[dict],
                         tools: dict) -> Path:
    """Write ``scms_inputs.json`` next to ``scenario_config.json`` (schema ``scms.inputs/1``).

    The Java back-end (ScmsBackend.inputManifest) discovers this file from the MOSAIC configuration
    path -- or from ``SCMS_INPUTS_JSON`` -- and inlines it into the dataset ``manifest.json`` under
    ``inputs``, and lifts ``params.road_network`` / ``params.radio_range_m`` into ``config`` so
    datagen.realism_bench can score the run without being told the scenario by hand.

    Deliberately carries NO generation timestamp: the whole file is then a pure function of
    (scenario, environment, input bytes), so regenerating the same scenario twice produces a
    byte-identical side-car and therefore a byte-identical ``manifest.inputs.inputs_file_sha256``.
    Null-valued params are OMITTED rather than emitted as JSON null, because Gson drops nulls on the
    Java side and an omitted key is then indistinguishable from an explicit null.
    """
    params = {k: resolved[k] for k in _INPUT_PARAM_KEYS
              if resolved.get(k) is not None and resolved.get(k) != ""}
    doc = {
        "schema": "scms.inputs/1",
        "scenario_key": key,
        "generator": "gen_scenario.py",
        "tool_versions": {k: v for k, v in (tools or {}).items() if v},
        "params": params,
        # map form {path: sha256}; entries whose hash was skipped (the ~1 GB InTAS route set) are
        # recorded by name with an explicit marker rather than silently dropped.
        "inputs": {f["path"]: (f.get("sha256") or f"skipped:{f.get('bytes')}bytes")
                   for f in fingerprints if f.get("path")},
    }
    out = dst / INPUTS_NAME
    out.write_text(json.dumps(doc, indent=2, sort_keys=False) + "\n", encoding="utf-8")
    return out


# ---------------------------------------------------------------------------
# key parsing + catalogue
# ---------------------------------------------------------------------------
_MAPGEN_RE = re.compile(r"^(grid_\d{1,3}x\d{1,3}|spider_\d{1,3}a\d{1,3}c|rand_\d{1,4})(_s\d{1,4})?$")


def is_mapgen_key(key: str) -> bool:
    """Strict, fully-anchored key grammar. Rejects anything with path separators / '..' and any
    unknown OSM city, so a key can never escape scms-sim/scenarios/ (no path traversal)."""
    if key.startswith("osm_"):
        return key[len("osm_"):] in CITIES
    return bool(_MAPGEN_RE.match(key))


def catalog() -> list[dict]:
    """A few hundred ready-made map keys, grouped by family (for the GUI / docs)."""
    out: list[dict] = []
    for n in (3, 4, 5, 6, 7, 8, 9, 10, 12, 14):
        for s in (1, 2, 3):
            out.append({"key": f"grid_{n}x{n}_s{s}", "label": f"Grid {n}×{n} (seed {s})",
                        "family": "procedural-grid", "kind": "route"})
    for cols, rows in ((4, 6), (6, 4), (5, 8), (8, 5), (6, 10), (10, 6), (8, 12), (12, 8),
                       (2, 16), (16, 2), (3, 20), (20, 3), (16, 16), (20, 20)):
        for s in (1, 2):
            out.append({"key": f"grid_{cols}x{rows}_s{s}", "label": f"Grid {cols}×{rows} (seed {s})",
                        "family": "procedural-grid", "kind": "route"})
    for arms in (4, 5, 6, 8, 10, 12):
        for circ in (3, 4, 5, 6):
            for s in (1, 2, 3):
                out.append({"key": f"spider_{arms}a{circ}c_s{s}",
                            "label": f"Spider {arms} arms × {circ} rings (seed {s})",
                            "family": "procedural-spider", "kind": "route"})
    for it in (50, 80, 120, 160, 200, 260, 320, 400):
        for s in (1, 2, 3, 4, 5):
            out.append({"key": f"rand_{it}_s{s}", "label": f"Random network {it} iters (seed {s})",
                        "family": "procedural-random", "kind": "route"})
    for suffix, (label, _bbox) in CITIES.items():
        out.append({"key": f"osm_{suffix}", "label": f"OSM · {label}",
                    "family": "osm-city", "kind": "route"})
    return out


# ---------------------------------------------------------------------------
# net generation
# ---------------------------------------------------------------------------
def _run(cmd: list[str], **kw):
    kw.setdefault("timeout", 600)   # netgenerate/netconvert/randomTrips must not hang forever
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def tls_flags(osm: bool) -> list[str]:
    """Traffic-light / geometry cleanup flags shared by netgenerate + netconvert.

    Procedural maps used to be built with no TLS options at all, so netgenerate emitted
    priority-only junctions and the "city" maps had zero signalised intersections. These are the
    flags every published SUMO city scenario (LuST/InTAS/BeST/HaTS) uses on import.
    ``SCMS_TLS=off`` restores the signal-free networks."""
    mode = _envs("SCMS_TLS", "guess").lower()
    if mode in ("off", "none", "0", "false"):
        return []
    flags = ["--tls.join", "--junctions.join"]
    if osm:
        # OSM carries real `highway=traffic_signals` nodes: promote those, and drop the
        # single-approach ones netconvert would otherwise invent. (--ramps.guess is a
        # netconvert-only option; netgenerate has no ramp guessing.)
        flags += ["--ramps.guess", "--tls.guess-signals", "--tls.discard-simple",
                  "--tls.guess.joining"]
    else:
        flags += ["--tls.guess", "--tls.guess.joining"]
    ttype = _envs("SCMS_TLS_TYPE", "")
    if ttype:
        flags += ["--tls.default-type", ttype]
    return flags


def _netgenerate(key: str, out_net: Path):
    base = ["--no-turnarounds", "--default.lanenumber", "2"] + tls_flags(osm=False)
    m = re.match(r"^grid_(\d+)x(\d+)(?:_s(\d+))?$", key)
    if m:
        cols, rows, seed = int(m.group(1)), int(m.group(2)), int(m.group(3) or 1)
        _run([str(SUMO / "bin" / "netgenerate.exe"), "--grid",
              "--grid.x-number", str(cols), "--grid.y-number", str(rows),
              "--grid.length", "120", "--seed", str(seed),
              *base, "-o", str(out_net)])
        return
    m = re.match(r"^spider_(\d+)a(\d+)c(?:_s(\d+))?$", key)
    if m:
        arms, circ, seed = int(m.group(1)), int(m.group(2)), int(m.group(3) or 1)
        _run([str(SUMO / "bin" / "netgenerate.exe"), "--spider",
              "--spider.arm-number", str(arms), "--spider.circle-number", str(circ),
              "--spider.space-radius", "100", "--seed", str(seed),
              *base, "-o", str(out_net)])
        return
    m = re.match(r"^rand_(\d+)(?:_s(\d+))?$", key)
    if m:
        it, seed = int(m.group(1)), int(m.group(2) or 1)
        _run([str(SUMO / "bin" / "netgenerate.exe"), "--rand",
              "--rand.iterations", str(it), "--seed", str(seed),
              "--rand.min-distance", "80", "--rand.max-distance", "250",
              *base, "-o", str(out_net)])
        return
    raise SystemExit(f"unrecognized procedural map key '{key}'")


def _osm_net(key: str, out_net: Path):
    suffix = key[len("osm_"):]
    if suffix not in CITIES:
        raise SystemExit(f"unknown OSM city '{suffix}'. Known: {', '.join(CITIES)}")
    conv = ["--geometry.remove", "--roundabouts.guess",
            "--remove-edges.isolated", "--keep-edges.by-vclass", "passenger",
            "--osm.all-attributes", "false"] + tls_flags(osm=True)
    # The cache key includes the netconvert flags, so changing the import options (e.g. turning
    # signal guessing off) rebuilds the net instead of silently reusing an old one.
    sig = hashlib.sha256(" ".join(sorted(conv)).encode()).hexdigest()[:8]
    cached = CACHE / f"osm_{suffix}_{sig}.net.xml"
    legacy = CACHE / f"osm_{suffix}.net.xml"
    if cached.exists():
        shutil.copy(cached, out_net)
        return
    CACHE.mkdir(parents=True, exist_ok=True)
    _label, bbox = CITIES[suffix]
    work = CACHE / f"_work_{suffix}"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    osm_raw = work / "raw.osm.xml"
    # The OSM main API returns 406 here; Overpass's /api/map?bbox= serves the same raw XML.
    bbox_str = ",".join(str(b) for b in bbox)   # minLon,minLat,maxLon,maxLat
    url = "https://overpass-api.de/api/map?bbox=" + bbox_str
    import urllib.request
    req = urllib.request.Request(url, headers={"User-Agent": "SCMS-Simulator/1.0"})
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            osm_raw.write_bytes(resp.read())
    except Exception:
        if legacy.exists():                     # offline: fall back to a previously imported net
            print(f"[mapgen] Overpass unreachable; reusing cached {legacy.name}", file=sys.stderr)
            shutil.copy(legacy, out_net)
            shutil.rmtree(work, ignore_errors=True)
            return
        raise
    if osm_raw.stat().st_size < 1000:
        raise SystemExit(f"OSM download for {suffix} was empty/too small")
    _run([str(SUMO / "bin" / "netconvert.exe"),
          "--osm-files", str(osm_raw), "-o", str(out_net), *conv])
    shutil.copy(out_net, cached)
    shutil.rmtree(work, ignore_errors=True)


# ---------------------------------------------------------------------------
# demand + scenario assembly
# ---------------------------------------------------------------------------
def od_mode() -> str:
    """gravity | uniform — how source/destination edges are drawn (SCMS_OD)."""
    m = _envs("SCMS_OD", "gravity").lower()
    return m if m in ("gravity", "uniform") else "gravity"


def depart_profile() -> list[float]:
    """Relative departure rate per equal time slice (SCMS_DEPART_PROFILE, '' = flat).

    A named profile or an explicit comma list; the mean rate is normalised so the total vehicle
    count matches a flat run — the profile only reshapes *when* they depart, the way a real
    diurnal loop-count curve does."""
    named = {
        "flat": [], "uniform": [],
        # 6 slices ~ a morning peak / midday plateau / evening peak day (InTAS & BeST shape)
        "morning": [0.5, 1.4, 2.0, 1.5, 0.9, 0.7],
        "evening": [0.7, 0.9, 1.2, 1.8, 2.0, 1.0],
        "diurnal": [0.4, 1.6, 1.0, 0.8, 1.8, 0.6],
        "peak": [0.6, 1.8, 2.2, 1.4, 0.7, 0.4],
    }
    raw = _envs("SCMS_DEPART_PROFILE", "").lower()
    if not raw:
        return []
    if raw in named:
        return list(named[raw])
    try:
        vals = [float(x) for x in re.split(r"[,;\s]+", raw) if x]
    except ValueError:
        return []
    return [v for v in vals if v >= 0] if len(vals) >= 2 else []


def _period_series(period: float) -> list[str]:
    """randomTrips -p accepts one period per equal sub-interval; turn the rate profile into that."""
    prof = depart_profile()
    if not prof:
        return [str(period)]
    mean = sum(prof) / len(prof)
    if mean <= 0:
        return [str(period)]
    floor = 0.05 * mean                       # never fully starve an interval (period -> inf)
    return [f"{period * mean / max(floor, w):.4f}" for w in prof]


def write_od_weights(net: Path, prefix: Path, fringe_factor: float = 5.0) -> bool:
    """Gravity-style source/sink edge weights for randomTrips (`--weights-prefix`).

    Trip production/attraction at a node is proportional to its *capacity-weighted degree*
    (sum of lanes x speed over incident edges) -- a standard zonal-mass proxy -- instead of
    randomTrips' default uniform/length weighting, which spreads a city's demand evenly over
    residential back streets. Uses sumolib (ships with SUMO, no pip dependency) so the built-in
    vClass / fringe / roundabout exclusions are reproduced exactly."""
    try:
        sys.path.insert(0, str(SUMO / "tools"))
        import sumolib                                   # noqa: E402  (SUMO_HOME/tools)
        net_obj = sumolib.net.readNet(str(net))
    except Exception as exc:                             # pragma: no cover - toolchain issue
        print(f"[mapgen] gravity OD unavailable ({exc}); falling back to uniform randomTrips",
              file=sys.stderr)
        return False
    roundabouts = set()
    for rb in net_obj.getRoundabouts():
        roundabouts.update(rb.getEdges())
    mass: dict[str, float] = {}
    for e in net_obj.getEdges():
        m = len(e.getLanes()) * max(1.0, e.getSpeed())
        for node in (e.getFromNode(), e.getToNode()):
            mass[node.getID()] = mass.get(node.getID(), 0.0) + m
    src: list[tuple[str, float]] = []
    dst: list[tuple[str, float]] = []
    for e in net_obj.getEdges():
        eid = e.getID()
        if not e.allows("passenger") or eid in roundabouts:
            continue
        lanes = len(e.getLanes())
        ws = mass.get(e.getFromNode().getID(), 0.0) * lanes
        wd = mass.get(e.getToNode().getID(), 0.0) * lanes
        # a sink-fringe edge cannot start a trip, a source-fringe edge cannot end one
        if e.is_fringe(e._outgoing):
            ws = 0.0
        elif e.is_fringe(e._incoming):
            ws *= fringe_factor
        if e.is_fringe(e._incoming):
            wd = 0.0
        elif e.is_fringe(e._outgoing):
            wd *= fringe_factor
        if ws > 0:
            src.append((eid, ws))
        if wd > 0:
            dst.append((eid, wd))
    if not src or not dst:
        return False
    for suffix, rows in ((".src.xml", src), (".dst.xml", dst)):
        norm = 100.0 / max(w for _e, w in rows)
        lines = ['<edgedata>', '    <interval id="gravity" begin="0" end="86400">']
        lines += [f'        <edge id="{eid}" value="{w * norm:.4f}"/>' for eid, w in sorted(rows)]
        lines += ["    </interval>", "</edgedata>", ""]
        Path(str(prefix) + suffix).write_text("\n".join(lines), encoding="utf-8")
    return True


def _random_trips(net: Path, routes: Path, duration_s: float, seed: int, period: float):
    cmd = [sys.executable, str(SUMO / "tools" / "randomTrips.py"),
           "-n", str(net), "-r", str(routes), "-o", str(routes.with_suffix(".trips.xml")),
           "-b", "0", "-e", str(int(duration_s)),
           "--seed", str(seed), "--prefix", "v", "--validate",
           "--vehicle-class", "passenger", "--fringe-factor", "5"]
    cmd += ["-p"] + _period_series(period)
    if od_mode() == "gravity":
        prefix = routes.parent / "_odw"
        if write_od_weights(net, prefix, fringe_factor=5.0):
            cmd += ["--weights-prefix", str(prefix)]
    _run(cmd, env={**os.environ, "SUMO_HOME": str(SUMO)})
    _tag_vtype(routes, seed)


def _tag_vtype(routes: Path, seed: int):
    """Attach the heterogeneous driver population to the generated demand.

    Every fleet class becomes a ``<vTypeDistribution>`` of jittered driver prototypes (EIDM
    car-following + a per-driver ``speedFactor`` distribution by default), and each vehicle is
    assigned a class so SUMO samples a driver for it. Also reshapes departures over time
    (SCMS_DEMAND=rush/night, SCMS_DEPART_PROFILE)."""
    tree = ET.parse(routes)
    root = tree.getroot()
    pop = driver_population(seed)
    existing = {vt.get("id") for vt in root.findall("vType")}
    existing |= {vd.get("id") for vd in root.findall("vTypeDistribution")}
    for cls, _w, members in reversed(pop):
        if cls in existing:
            continue
        if len(members) == 1:
            mid, attrs = members[0]
            root.insert(0, ET.Element("vType", {"id": mid, **_sattrs(attrs)}))
            continue
        dist = ET.Element("vTypeDistribution", {"id": cls})
        for mid, attrs in members:
            ET.SubElement(dist, "vType", {"id": mid, "probability": "1", **_sattrs(attrs)})
        root.insert(0, dist)
    rng = random.Random((seed * 2654435761) & 0x7fffffff)
    names = [c for c, _w, _m in pop]
    weights = [w for _c, w, _m in pop]
    vehs = [v for v in list(root) if v.tag in ("vehicle", "trip")]
    profile = os.environ.get("SCMS_DEMAND", "uniform").lower()
    dmax = max((float(v.get("depart", "0")) for v in vehs), default=1.0) or 1.0
    for v in vehs:
        v.set("type", rng.choices(names, weights=weights, k=1)[0])
        if profile == "rush":                       # concentrate departures early (peak then quiet)
            u = float(v.get("depart", "0")) / dmax
            v.set("depart", f"{dmax * u * u:.2f}")
    if profile == "night":                          # sparse traffic: keep ~35% of vehicles
        keep = [v for v in vehs if rng.random() < 0.35]
        for v in vehs:
            if v not in keep:
                root.remove(v)
    elif profile == "rush":                          # SUMO needs departures sorted
        for v in vehs:
            root.remove(v)
        for v in sorted(vehs, key=lambda e: float(e.get("depart", "0"))):
            root.append(v)
    tree.write(routes, encoding="UTF-8", xml_declaration=True)


# Synthetic (netgenerate) nets carry no projection at all (projParameter="!"), so their local
# x/y frame is anchored on a real UTM tile: MOSAIC's geo coordinates -- and therefore RSU
# GeoPoints -- become well-formed instead of degenerate easting~0 points.
_SYNTH_ANCHOR = (600000.0, 5300000.0, 32, True)      # zone 32N, southern Germany


def net_projection(net: Path) -> tuple[dict, dict, int, bool]:
    """(centerCoordinates, cartesianOffset, utm zone, northern) for the MOSAIC scenario config.

    MOSAIC's rule is ``cartesian = UTM + cartesianOffset`` with the UTM zone taken from
    centerCoordinates, so the centre must come from the net itself — a hard-coded dummy centre
    silently mis-zones every non-European OSM city."""
    loc = _net_location(net)
    off = loc.get("netOffset") or [0.0, 0.0]
    conv = loc.get("convBoundary") or []
    if loc.get("zone") and len(conv) == 4:
        lat, lon = _utm_to_lonlat((conv[0] + conv[2]) / 2 - off[0],
                                  (conv[1] + conv[3]) / 2 - off[1],
                                  loc["zone"], loc["north"])
        return ({"latitude": round(lat, 7), "longitude": round(lon, 7)},
                {"x": off[0], "y": off[1]}, loc["zone"], loc["north"])
    e0, n0, zone, north = _SYNTH_ANCHOR
    lat, lon = _utm_to_lonlat(e0, n0, zone, north)
    return ({"latitude": round(lat, 7), "longitude": round(lon, 7)},
            {"x": -e0, "y": -n0}, zone, north)


def build(key: str, dst: Path, duration=None, scale=None, seed=None, period=None) -> dict:
    if not is_mapgen_key(key):
        raise SystemExit(f"invalid map key {key!r}")   # guard before any filesystem op
    dur = duration or "300s"
    dur_s = float(re.sub(r"[^\d.]", "", str(dur)) or 300)
    rng_seed = int(seed) if seed else 42
    per = float(period) if period else 1.5   # smaller -> denser traffic

    # defence in depth: the output dir must stay under scms-sim/scenarios/
    base = (REPO / "scms-sim" / "scenarios").resolve()
    if not str(dst.resolve()).startswith(str(base)):
        raise SystemExit(f"refusing to write outside {base}: {dst}")
    if dst.exists():
        shutil.rmtree(dst)
    (dst / "sumo").mkdir(parents=True)
    (dst / "mapping").mkdir()
    (dst / "application").mkdir()
    (dst / "output").mkdir()

    net = dst / "sumo" / "map.net.xml"
    routes = dst / "sumo" / "map.rou.xml"
    if key.startswith("osm_"):
        _osm_net(key, net)
    else:
        _netgenerate(key, net)
    _random_trips(net, routes, dur_s, rng_seed, per)

    # sumocfg (SUMO owns the demand) + fine step (ETSI CAM rate) + optional density scale.
    # MOSAIC's SumoAmbassador always passes --step-length <updateInterval/1000> on the SUMO command
    # line, which overrides the sumocfg, so the step written here IS the update interval -- anything
    # else would put a step in the file (and in the manifest) that SUMO never runs.
    update_ms = align_sync_ms(sim_step_ms())
    step_s = update_ms / 1000.0
    # <processing>: traffic density + the sublane model. --lateral-resolution goes in the SUMOCFG
    # rather than on MOSAIC's command line so that a standalone `sumo -c map.sumocfg` reproduces the
    # same lateral dynamics (MOSAIC only ever appends -c/-v/--remote-port/--step-length/
    # --xml-validation, so a processing option in the file is honoured verbatim).
    proc = []
    if scale:
        proc.append(f'\t\t<scale value="{scale}"/>')
    res = lateral_res()
    proc.append(f'\t\t<lateral-resolution value="{res:g}"/>' if res > 0
                else '\t\t<lateral-resolution value="-1"/>')
    proc_xml = "\n\t<processing>\n" + "\n".join(proc) + "\n\t</processing>"
    (dst / "sumo" / "map.sumocfg").write_text(
        "<configuration>\n\t<input>\n"
        '\t\t<net-file value="map.net.xml"/>\n'
        '\t\t<route-files value="map.rou.xml"/>\n'
        "\t</input>\n\t<time>\n"
        '\t\t<begin value="0"/>\n\t\t<end value="%d"/>\n\t\t<step-length value="%s"/>\n\t</time>%s\n</configuration>\n'
        % (int(dur_s) + 5, step_s, proc_xml), encoding="utf-8")
    (dst / "sumo" / "sumo_config.json").write_text(
        json.dumps({"sumoConfigurationFile": "map.sumocfg",
                    "updateInterval": update_ms}, indent=2),
        encoding="utf-8")

    # projection derived from the net itself (correct UTM zone for every OSM city; a synthetic
    # anchor for projection-less netgenerate maps) so MOSAIC geo coordinates are well-formed.
    center, offset, _zone, _north = net_projection(net)
    scenario = {
        "simulation": {
            "id": key, "duration": dur,
            "randomSeed": rng_seed,
            "projection": {
                "centerCoordinates": center,
                "cartesianOffset": offset,
            },
            "network": {
                "netMask": "255.255.0.0", "vehicleNet": "10.1.0.0", "rsuNet": "10.2.0.0",
                "tlNet": "10.3.0.0", "csNet": "10.4.0.0", "serverNet": "10.5.0.0",
                "tmcNet": "10.6.0.0",
            },
        },
        "federates": {"application": True, "sumo": True, "output": True, "sns": True,
                      "omnetpp": False, "ns3": False, "cell": False, "environment": False},
    }
    (dst / "scenario_config.json").write_text(json.dumps(scenario, indent=2), encoding="utf-8")

    # one MOSAIC prototype per SUMO vType the demand can carry (every jittered driver prototype
    # plus the distribution ids); the SUMO vType in the route file carries the kinematics, so the
    # prototype just needs name + our app.
    mapping = {"prototypes": [{"name": name, "applications": [OUR_APP], "weight": 1.0}
                              for name in prototype_names(rng_seed)]}
    rsus = rsu_units(net, rsu_count(), rsu_app(), rsu_placement())
    if rsus:
        mapping["rsus"] = rsus
    (dst / "mapping" / "mapping_config.json").write_text(json.dumps(mapping, indent=2), encoding="utf-8")
    write_sns_config(dst)

    # generic output config (copied from a bundle so the output federate has something to do)
    src_out = MOSAIC / "scenarios" / "Highway" / "output" / "output_config.xml"
    if src_out.exists():
        shutil.copy(src_out, dst / "output" / "output_config.xml")

    # our app never requests navigation, and synthetic/OSM nets have no MOSAIC routing DB,
    # so use the 'no-routing' navigation component instead of the default database routing.
    (dst / "application" / "application_config.json").write_text(
        json.dumps({"navigationConfiguration": {"type": "no-routing"}}, indent=2),
        encoding="utf-8")

    shutil.copy(JAR, dst / "application" / JAR.name)

    _rn_token, _rn_ev = road_network_token(key, net)
    resolved = {
        "source": "mapgen", "duration": dur, "seed": rng_seed, "period": per, "scale": scale,
        "sumo_step_ms": update_ms, "mosaic_sync_ms": update_ms,
        "sumo_step_source": "MOSAIC SumoAmbassador --step-length (= updateInterval); it overrides "
                            "the sumocfg, which is written to match",
        "sumocfg_step_ms_requested": sim_step_ms(),
        "car_follow_model": cf_model() or "sumo-default",
        # lateral dynamics: with the sublane model on, a lane change is a continuous ~3 s lateral
        # traverse instead of a one-step teleport across the full lane width.
        "sublane_model": sublane_on(),
        "lateral_resolution_m": (res if res > 0 else None),
        "lane_change_model": ("SL2015 (auto-selected by --lateral-resolution)" if res > 0
                              else "LC2013 (SUMO default; lane changes are instantaneous)"),
        "lateral_vtype_attrs": lateral_vtype_attrs(),
        "speed_factor": speed_factor_spec(), "speed_dev": speed_dev(),
        "vtype_samples": vtype_samples(), "vtype_jitter": vtype_jitter(),
        "fleet_classes": [c for c, _w, _m in driver_population(rng_seed)],
        "vtypes": prototype_names(rng_seed),
        "od_mode": od_mode(), "depart_profile": depart_profile(),
        "demand_profile": os.environ.get("SCMS_DEMAND", "uniform"),
        # network classification + radio range: what datagen.realism_bench needs to pick its
        # reference bands and reconstruct link distances on a MOSAIC dataset.
        "road_network": _rn_token, "regime": regime_of(_rn_token),
        "road_network_evidence": _rn_ev, "radio_range_m": radio_range_m(),
        "tls_flags": tls_flags(osm=key.startswith("osm_")),
        "rsus": len(rsus), "rsu_app": rsu_app() if rsus else None,
        "rsu_placement": (rsu_placement() if rsu_count() > 0 else None),
        "projection": {"center": center, "cartesianOffset": offset},
    }
    manifest = write_scenario_manifest(
        dst, key, "route", resolved,
        [net, routes, dst / "sumo" / "map.sumocfg", dst / "sumo" / "sumo_config.json",
         dst / "scenario_config.json", dst / "mapping" / "mapping_config.json",
         dst / "sns" / "sns_config.json", dst / "application" / JAR.name,
         dst / "sumo" / "_odw.src.xml", dst / "sumo" / "_odw.dst.xml"])
    return {"scenario_config": str(dst / "scenario_config.json"),
            "dataset_dir": str(REPO / "datasets" / key), "kind": "route",
            "manifest": str(manifest), "inputs_json": str(dst / INPUTS_NAME)}
