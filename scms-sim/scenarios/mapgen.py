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
and derive the MOSAIC projection from the net's ``<location netOffset>``. Vehicles are
generated with ``randomTrips.py`` and tagged with a single ``car`` vType.

``catalog()`` enumerates a few hundred ready-made keys for the GUI / docs; any well-formed
key outside the catalogue is generated on demand just the same.
"""

from __future__ import annotations

import os
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


def sim_step_ms() -> int:
    """SUMO/MOSAIC step in ms (SCMS_SIM_STEP seconds). 0.1 s enables up to 10 Hz ETSI CAMs."""
    return max(50, int(round(_envf("SCMS_SIM_STEP", 0.1) * 1000)))


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


def write_sns_config(dst: Path):
    """Per-scenario SNS radio config: SCMS_RADIO_RANGE (m) + SCMS_RADIO_LOSS (0..1)."""
    import json
    sns = {
        "maximumTtl": 10,
        "singlehopRadius": _envf("SCMS_RADIO_RANGE", 709.4),
        "adhocTransmissionModel": {"type": "SophisticatedAdhocTransmissionModel"},
        "singlehopDelay": {"type": "SimpleRandomDelay", "steps": 5,
                           "minDelay": "0.4 ms", "maxDelay": "2.4 ms"},
        "singleHopTransmission": {"lossProbability": _envf("SCMS_RADIO_LOSS", 0.0),
                                  "maxRetries": 0},
    }
    (dst / "sns").mkdir(exist_ok=True)
    (dst / "sns" / "sns_config.json").write_text(json.dumps(sns, indent=2), encoding="utf-8")


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


def _netgenerate(key: str, out_net: Path):
    m = re.match(r"^grid_(\d+)x(\d+)(?:_s(\d+))?$", key)
    if m:
        cols, rows, seed = int(m.group(1)), int(m.group(2)), int(m.group(3) or 1)
        _run([str(SUMO / "bin" / "netgenerate.exe"), "--grid",
              "--grid.x-number", str(cols), "--grid.y-number", str(rows),
              "--grid.length", "120", "--seed", str(seed),
              "--no-turnarounds", "--default.lanenumber", "2", "-o", str(out_net)])
        return
    m = re.match(r"^spider_(\d+)a(\d+)c(?:_s(\d+))?$", key)
    if m:
        arms, circ, seed = int(m.group(1)), int(m.group(2)), int(m.group(3) or 1)
        _run([str(SUMO / "bin" / "netgenerate.exe"), "--spider",
              "--spider.arm-number", str(arms), "--spider.circle-number", str(circ),
              "--spider.space-radius", "100", "--seed", str(seed),
              "--no-turnarounds", "--default.lanenumber", "2", "-o", str(out_net)])
        return
    m = re.match(r"^rand_(\d+)(?:_s(\d+))?$", key)
    if m:
        it, seed = int(m.group(1)), int(m.group(2) or 1)
        _run([str(SUMO / "bin" / "netgenerate.exe"), "--rand",
              "--rand.iterations", str(it), "--seed", str(seed),
              "--rand.min-distance", "80", "--rand.max-distance", "250",
              "--no-turnarounds", "--default.lanenumber", "2", "-o", str(out_net)])
        return
    raise SystemExit(f"unrecognized procedural map key '{key}'")


def _osm_net(key: str, out_net: Path):
    suffix = key[len("osm_"):]
    if suffix not in CITIES:
        raise SystemExit(f"unknown OSM city '{suffix}'. Known: {', '.join(CITIES)}")
    cached = CACHE / f"osm_{suffix}.net.xml"
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
    with urllib.request.urlopen(req, timeout=120) as resp:
        osm_raw.write_bytes(resp.read())
    if osm_raw.stat().st_size < 1000:
        raise SystemExit(f"OSM download for {suffix} was empty/too small")
    _run([str(SUMO / "bin" / "netconvert.exe"),
          "--osm-files", str(osm_raw), "-o", str(out_net),
          "--geometry.remove", "--roundabouts.guess", "--ramps.guess",
          "--junctions.join", "--tls.guess-signals", "--tls.discard-simple",
          "--remove-edges.isolated", "--keep-edges.by-vclass", "passenger",
          "--osm.all-attributes", "false"])
    shutil.copy(out_net, cached)
    shutil.rmtree(work, ignore_errors=True)


# ---------------------------------------------------------------------------
# demand + scenario assembly
# ---------------------------------------------------------------------------
def _random_trips(net: Path, routes: Path, duration_s: float, seed: int, period: float):
    _run([sys.executable, str(SUMO / "tools" / "randomTrips.py"),
          "-n", str(net), "-r", str(routes), "-o", str(routes.with_suffix(".trips.xml")),
          "-b", "0", "-e", str(int(duration_s)), "-p", str(period),
          "--seed", str(seed), "--prefix", "v", "--validate",
          "--vehicle-class", "passenger", "--fringe-factor", "5"],
         env={**os.environ, "SUMO_HOME": str(SUMO)})
    _tag_vtype(routes, seed)


def _tag_vtype(routes: Path, seed: int):
    """Assign each generated vehicle a fleet vType (weighted) and optionally reshape the demand
    over time (SCMS_DEMAND=rush front-loads departures), so mapping can match every type."""
    import random as _r
    tree = ET.parse(routes)
    root = tree.getroot()
    flt = fleet()
    existing = {vt.get("id") for vt in root.findall("vType")}
    for name, _w, attrs in flt:
        if name not in existing:
            root.insert(0, ET.Element("vType", {"id": name, **{k: str(v) for k, v in attrs.items()}}))
    rng = _r.Random((seed * 2654435761) & 0x7fffffff)
    names = [f[0] for f in flt]
    weights = [f[1] for f in flt]
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


def _net_offset(net: Path) -> tuple[float, float]:
    txt = net.read_text(encoding="utf-8", errors="replace")
    m = re.search(r'netOffset="([-\d.]+),([-\d.]+)"', txt)
    return (float(m.group(1)), float(m.group(2))) if m else (0.0, 0.0)


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

    # sumocfg (SUMO owns the demand) + fine step (ETSI CAM rate) + optional density scale
    step_s = sim_step_ms() / 1000.0
    scale_xml = f'\n\t<processing>\n\t\t<scale value="{scale}"/>\n\t</processing>' if scale else ""
    (dst / "sumo" / "map.sumocfg").write_text(
        "<configuration>\n\t<input>\n"
        '\t\t<net-file value="map.net.xml"/>\n'
        '\t\t<route-files value="map.rou.xml"/>\n'
        "\t</input>\n\t<time>\n"
        '\t\t<begin value="0"/>\n\t\t<end value="%d"/>\n\t\t<step-length value="%s"/>\n\t</time>%s\n</configuration>\n'
        % (int(dur_s) + 5, step_s, scale_xml), encoding="utf-8")
    import json
    (dst / "sumo" / "sumo_config.json").write_text(
        json.dumps({"sumoConfigurationFile": "map.sumocfg", "updateInterval": sim_step_ms()}, indent=2),
        encoding="utf-8")

    # projection: cartesianOffset = net's netOffset; geo center is a dummy (unused downstream).
    ox, oy = _net_offset(net)
    scenario = {
        "simulation": {
            "id": key, "duration": dur,
            "randomSeed": rng_seed,
            "projection": {
                "centerCoordinates": {"latitude": 48.0, "longitude": 11.0},
                "cartesianOffset": {"x": ox, "y": oy},
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

    # one MOSAIC prototype per fleet type (matched to the SUMO vType by name); the SUMO vType in
    # the route file carries the kinematics, so the prototype just needs name + our app.
    mapping = {"prototypes": [{"name": name, "applications": [OUR_APP], "weight": 1.0}
                              for name, _w, _a in fleet()]}
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
    return {"scenario_config": str(dst / "scenario_config.json"),
            "dataset_dir": str(REPO / "datasets" / key), "kind": "route"}
