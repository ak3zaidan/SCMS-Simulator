"""Unified scenario generator: wire the SCMS app into any supported map.

Produces a runnable MOSAIC scenario under scms-sim/scenarios/gen_<key> by copying a
source scenario (a MOSAIC-bundled map or a NextGen InTAS variant), forcing the SNS
radio, assigning ONLY vehicles our app (dropping RSU/traffic-light/server units so a
vehicle app is never loaded on a non-vehicle unit), copying the routing DB + app jar,
and applying traffic controls. InTAS keeps its ~1 GB route set via a directory junction.

    python gen_scenario.py <key> [--duration 300s] [--scale 1.0]
                                  [--max-vehicles N] [--target-flow F] [--lanes L] [--seed S]

Prints JSON: {"scenario_config": <path>, "dataset_dir": <path>, "kind": flow|route}.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import mapgen

REPO = Path(__file__).resolve().parents[2]
MOSAIC = Path(os.environ.get("MOSAIC_HOME", r"C:\Users\Administrator\tools\mosaic"))
NEXTGEN = REPO / "third_party" / "veremi-nextgen" / "Generator"
JAR = REPO / "scms-sim" / "mosaic-apps" / "scms-app" / "build" / "ScmsApp-0.1.0.jar"
OUR_APP = "org.scms.app.ScmsBeaconApp"

# key -> source + kind (+ default duration). kind drives which traffic control applies.
REG = {
    "smoke":              {"src": ("bundle", "HelloWorld"),               "kind": "flow",  "dur": "200s"},
    "highway":            {"src": ("bundle", "Highway"),                  "kind": "flow",  "dur": "400s"},
    "barnim":             {"src": ("bundle", "Barnim"),                   "kind": "flow",  "dur": "400s"},
    "tiergarten":         {"src": ("bundle", "Tiergarten"),              "kind": "flow",  "dur": "180s"},
    "intas_urban_low":    {"src": ("nextgen", "InTAS_urban_2_4_test"),    "kind": "route", "dur": "300s"},
    "intas_urban_rush":   {"src": ("nextgen", "InTAS_urban_7_9_test"),    "kind": "route", "dur": "300s"},
    "intas_highway_low":  {"src": ("nextgen", "InTAS_highway_2_4_test"),  "kind": "route", "dur": "300s"},
    "intas_highway_rush": {"src": ("nextgen", "InTAS_highway_7_9_test"),  "kind": "route", "dur": "300s"},
}


def _set_apps(obj):
    if isinstance(obj, dict):
        if "applications" in obj:
            obj["applications"] = [OUR_APP]
        for v in obj.values():
            _set_apps(v)
    elif isinstance(obj, list):
        for v in obj:
            _set_apps(v)


def _junction(link: Path, target: Path):
    subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(target)],
                   check=True, capture_output=True, text=True)


def generate(key: str, duration=None, scale=None, max_vehicles=None,
             target_flow=None, lanes=None, seed=None) -> dict:
    if key not in REG and mapgen.is_mapgen_key(key):
        dst = REPO / "scms-sim" / "scenarios" / f"gen_{key}"
        return mapgen.build(key, dst, duration=duration, scale=scale, seed=seed)
    if key not in REG:
        raise SystemExit(f"unknown scenario '{key}'. Known: {', '.join(REG)}, "
                         f"or a procedural/osm key (grid_/spider_/rand_/osm_).")
    spec = REG[key]
    kind = spec["kind"]
    dur = duration or spec["dur"]
    srctype, srcname = spec["src"]
    dst = REPO / "scms-sim" / "scenarios" / f"gen_{key}"
    if dst.exists():
        shutil.rmtree(dst)
    dst.mkdir(parents=True)

    if srctype == "bundle":
        src = MOSAIC / "scenarios" / srcname
        shutil.copytree(src, dst, dirs_exist_ok=True)   # small: whole dir incl. sumo + nav db
    else:
        src = NEXTGEN / "simulation" / "mosaic" / "scenarios" / srcname
        for sub in ("mapping", "application", "output"):
            if (src / sub).exists():
                shutil.copytree(src / sub, dst / sub)
        shutil.copy(src / "scenario_config.json", dst / "scenario_config.json")
        (dst / "sumo").mkdir()
        for item in (src / "sumo").iterdir():          # copy small sumo files; junction the huge routes/
            if item.is_dir() and item.name == "routes":
                _junction(dst / "sumo" / "routes", item)
            elif item.is_dir():
                shutil.copytree(item, dst / "sumo" / item.name)
            else:
                shutil.copy(item, dst / "sumo" / item.name)
        ddb = NEXTGEN / "docker" / "scenarios" / srcname / "application" / "InTAS.db"
        (dst / "application").mkdir(exist_ok=True)
        shutil.copy(ddb, dst / "application" / "InTAS.db")

    shutil.copy(JAR, dst / "application" / JAR.name)

    # scenario_config: force the SNS radio, disable federates we don't run, set duration/seed.
    cfgp = dst / "scenario_config.json"
    cfg = json.loads(cfgp.read_text(encoding="utf-8"))
    feds = cfg.setdefault("federates", {})
    feds.update({"application": True, "sumo": True, "output": True, "sns": True,
                 "omnetpp": False, "ns3": False, "cell": False, "environment": False})
    cfg["simulation"]["duration"] = dur
    if seed:
        cfg["simulation"]["randomSeed"] = int(seed)
    cfgp.write_text(json.dumps(cfg, indent=2), encoding="utf-8")

    # mapping: keep ONLY vehicles and give them our app. Route maps (InTAS) match SUMO
    # vehicle types to prototypes by name, so the app goes on prototypes; flow maps spawn
    # from vehicle flows, so the app goes on the flow types (and non-vehicle units are dropped).
    mp = dst / "mapping" / "mapping_config.json"
    m = json.loads(mp.read_text(encoding="utf-8"))
    for k in ("rsus", "trafficLights", "servers", "chargingStations", "tmcs"):
        m.pop(k, None)
    if kind == "route":
        for proto in m.get("prototypes", []):
            proto["applications"] = [OUR_APP]
    else:
        for proto in m.get("prototypes", []):
            proto.pop("applications", None)
        proto_name = m["prototypes"][0]["name"] if m.get("prototypes") else "car"
        for f in m.get("vehicles", []):
            types = f.get("types")
            if not types:
                f["types"] = [{"name": proto_name, "applications": [OUR_APP]}]
            else:
                for ty in types:
                    ty["applications"] = [OUR_APP]
            if max_vehicles:
                f["maxNumberVehicles"] = int(max_vehicles)
            if target_flow:
                f["targetFlow"] = int(target_flow)
            if lanes:
                f["lanes"] = list(range(int(lanes)))
    # apply the shared vehicle-dynamics knobs (SCMS_VEH_*) to every prototype
    vp = mapgen.veh_params()
    for proto in m.get("prototypes", []):
        proto.update({"accel": vp["accel"], "decel": vp["decel"], "length": vp["length"],
                      "maxSpeed": vp["maxSpeed"], "minGap": vp["minGap"],
                      "sigma": vp["sigma"], "tau": vp["tau"]})
    mp.write_text(json.dumps(m, indent=2), encoding="utf-8")

    # per-scenario SNS radio config (SCMS_RADIO_RANGE / SCMS_RADIO_LOSS)
    mapgen.write_sns_config(dst)

    # route-based traffic density: inject SUMO --scale into the sumocfg(s).
    if scale and float(scale) != 1.0:
        for scfg in (dst / "sumo").glob("*.sumocfg"):
            txt = scfg.read_text(encoding="utf-8")
            if "<scale " not in txt:
                if "</processing>" in txt:
                    txt = txt.replace("</processing>", f'\t<scale value="{scale}"/>\n\t</processing>', 1)
                else:
                    txt = txt.replace("</configuration>",
                                      f'\t<processing>\n\t\t<scale value="{scale}"/>\n\t</processing>\n</configuration>', 1)
                scfg.write_text(txt, encoding="utf-8")

    # optional: honour an explicit SCMS_SIM_STEP on curated maps (finer MOSAIC<->SUMO sync
    # so the ETSI CAM rules can trigger above 1 Hz); left untouched by default.
    if os.environ.get("SCMS_SIM_STEP"):
        scj = dst / "sumo" / "sumo_config.json"
        if scj.exists():
            sc = json.loads(scj.read_text(encoding="utf-8"))
            sc["updateInterval"] = mapgen.sim_step_ms()
            scj.write_text(json.dumps(sc, indent=2), encoding="utf-8")

    return {"scenario_config": str(cfgp), "dataset_dir": str(REPO / "datasets" / key), "kind": kind}


def main(argv=None):
    p = argparse.ArgumentParser(description="Generate a wired SCMS scenario.")
    p.add_argument("key", help="a REG key, or a procedural/osm key (grid_/spider_/rand_/osm_)")
    p.add_argument("--duration")
    p.add_argument("--scale")
    p.add_argument("--max-vehicles")
    p.add_argument("--target-flow")
    p.add_argument("--lanes")
    p.add_argument("--seed")
    a = p.parse_args(argv)
    print(json.dumps(generate(a.key, a.duration, a.scale, a.max_vehicles,
                              a.target_flow, a.lanes, a.seed)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
