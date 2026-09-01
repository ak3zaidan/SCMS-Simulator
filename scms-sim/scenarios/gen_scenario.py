"""Unified scenario generator: wire the SCMS app into any supported map.

Produces a runnable MOSAIC scenario under scms-sim/scenarios/gen_<key> by copying a
source scenario (a MOSAIC-bundled map or a NextGen InTAS variant), forcing the SNS
radio, assigning our app to vehicles, copying the routing DB + app jar, and applying
traffic controls. InTAS keeps its ~1 GB route set via a directory junction.

Realism defaults applied on top of the source scenario (all reversible via SCMS_* env,
all recorded in the generated ``scms_scenario_manifest.json``):

  * **100 ms MOSAIC<->SUMO sync** on curated + InTAS maps (they shipped 1000 ms, which caps
    CAM generation at 1 Hz and neuters the ETSI EN 302 637-2 dynamics rules). Opt out with
    ``SCMS_SYNC_MS=1000``. On InTAS this costs roughly 10x wall-clock.
  * **EIDM car-following** instead of Krauss: route maps get ``default.carfollowmodel`` in the
    copied sumocfg, flow maps get it through MOSAIC's ``additionalVehicleTypeParameters``.
    ``SCMS_CF_MODEL=krauss`` reverts.
  * **Sublane model** (``lateral-resolution`` = 0.8 m) on every generated sumocfg, so a lane change
    is a continuous ~3 s lateral traverse instead of SUMO's default one-step teleport across a full
    3.2 m lane width. ``SCMS_LATERAL_RES=off`` reverts; measured cost on InTAS urban low / 300 s:
    SUMO alone 6.17 s -> 10.91 s (+77%), whole pipeline 74.6 s -> 79.4 s (+6%).
  * **Driver heterogeneity**: a per-driver ``speedFactor`` distribution (MOSAIC otherwise
    hard-writes ``speedDev="0.0"``, i.e. every car drives exactly at the limit) plus MOSAIC
    per-vehicle parameter ``deviations``. InTAS's own 45 vType prototypes are no longer
    flattened by one uniform SCMS_VEH_* set (``SCMS_VEH_APPLY=1`` restores that).
  * **RSUs** are no longer stripped unconditionally — ``SCMS_RSUS`` (default ``auto``) places
    them on real junctions as soon as the Java layer ships an RSU application.
  * **Demand source** (``SCMS_DEMAND``, default ``intas``). ``calibrated`` swaps ``route-files``
    for a route set produced by ``tools/calibrate_demand.py`` with SUMO's ``routeSampler``, fitted
    to counts MEASURED at the city's own signal loops rather than to InTAS's 2019 calibration.
    Requires ``SCMS_DEMAND_ROUTES``; ``SCMS_DEMAND_DETECTORS`` additionally replaces the E1 layout
    with the repaired one (74 of InTAS's 194 named loops sit on sidewalk lanes and count nothing).
    The InTAS demand stays the default so published results keep reproducing.

    python gen_scenario.py <key> [--duration 300s] [--scale 1.0]
                                  [--max-vehicles N] [--target-flow F] [--lanes L] [--seed S]

Prints JSON: {"scenario_config": <path>, "dataset_dir": <path>, "kind": flow|route,
              "manifest": <path>}.
"""

from __future__ import annotations

import argparse
import json
import os
import re
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


# ---------------------------------------------------------------------------
# sumocfg surgery (text-level: the InTAS configs carry a GPL header comment we must keep)
# ---------------------------------------------------------------------------
def _cfg_get(txt: str, tag: str) -> str | None:
    m = re.search(r"<" + re.escape(tag) + r'\s+value\s*=\s*"([^"]*)"', txt)
    return m.group(1) if m else None


def _cfg_set(txt: str, tag: str, value: str, section: str) -> str:
    """Set ``<tag value="..."/>`` inside ``<section>``, creating the entry/section if needed."""
    pat = re.compile(r"(<" + re.escape(tag) + r'\s+value\s*=\s*")([^"]*)(")')
    if pat.search(txt):
        return pat.sub(lambda m: m.group(1) + value + m.group(3), txt, count=1)
    close = f"</{section}>"
    if close in txt:
        return txt.replace(close, f'\t\t<{tag} value="{value}"/>\n\t{close}', 1)
    return txt.replace("</configuration>",
                       f'\t<{section}>\n\t\t<{tag} value="{value}"/>\n\t</{section}>\n'
                       "</configuration>", 1)


def _sumocfgs(dst: Path) -> list[Path]:
    return sorted((dst / "sumo").glob("*.sumocfg"))


def _primary_sumocfg(dst: Path, scj: dict) -> Path | None:
    name = scj.get("sumoConfigurationFile")
    if name:
        p = (dst / "sumo" / name)
        if p.exists():
            return p
    cfgs = _sumocfgs(dst)
    return cfgs[0] if cfgs else None


def _net_of(cfg: Path) -> Path | None:
    try:
        nf = _cfg_get(cfg.read_text(encoding="utf-8", errors="replace"), "net-file")
    except OSError:
        return None
    if not nf:
        return None
    p = (cfg.parent / nf.split(",")[0].strip())
    return p if p.exists() else None


def _tune_sumo(dst: Path, kind: str, vtype_params: dict = None) -> dict:
    """Apply the SUMO-side realism settings and return what was resolved (for the manifest).

    * the MOSAIC ``updateInterval`` is written into sumo_config.json (100 ms by default);
    * every sumocfg's ``step-length`` is rewritten to that SAME value, because MOSAIC's
      ``SumoAmbassador.getProgramArguments`` unconditionally appends
      ``--step-length <updateInterval/1000>`` to the SUMO command line and a command-line option
      beats the sumocfg. Under MOSAIC the step SUMO actually integrates at IS the update interval;
      writing anything else into the file (and recording it in the manifest) describes a run that
      never happened. The file is kept in sync so a standalone ``sumo -c`` reproduces the same step;
    * route maps get ``default.carfollowmodel`` / ``default.speeddev`` (their vTypes come from
      the scenario's own route files, which we must not rewrite — InTAS junctions ~1 GB of them);
    * every sumocfg gets ``lateral-resolution`` (SUMO's sublane model). Without it SUMO keeps each
      vehicle on the lane CENTRELINE and a lane change is a single-step teleport of one full lane
      width; with it the lane-change model becomes SL2015 and the vehicle traverses laterally over
      ~3 s. It is written into the sumocfg rather than passed on MOSAIC's command line because
      ``SumoAmbassador.getProgramArguments`` only ever appends
      ``-c/-v/--remote-port/--step-length`` (+ ``--xml-validation never`` and
      ``additionalSumoParameters``), so a ``<processing>`` option in the file is honoured verbatim
      AND a standalone ``sumo -c`` reproduces the same lateral dynamics.
    """
    scj_path = dst / "sumo" / "sumo_config.json"
    scj = json.loads(scj_path.read_text(encoding="utf-8")) if scj_path.exists() else {}
    sync = mapgen.sync_ms()
    cf = mapgen.cf_model()
    lat_res = mapgen.lateral_res()
    cfgs = _sumocfgs(dst)
    shipped = []
    for cfg in cfgs:
        cur = _cfg_get(cfg.read_text(encoding="utf-8", errors="replace"), "step-length")
        shipped.append(int(round(float(cur) * 1000)) if cur else 1000)   # SUMO's default is 1 s
    # the finest step the scenario asks for, but never coarser than the sync period
    base_ms = min([min(shipped), sync]) if shipped else sync
    update_ms = mapgen.align_sync_ms(base_ms, sync)
    for cfg in cfgs:
        txt = cfg.read_text(encoding="utf-8", errors="replace")
        txt = _cfg_set(txt, "step-length", f"{update_ms / 1000.0:g}", "time")
        # -1 is SUMO's documented "off" value (every vehicle drives at the lane centre); write it
        # explicitly so SCMS_LATERAL_RES=off also overrides a source scenario that shipped a value.
        txt = _cfg_set(txt, "lateral-resolution",
                       f"{lat_res:g}" if lat_res > 0 else "-1", "processing")
        if kind == "route":
            # Route maps own their vTypes (InTAS ships 45 of them across ~1 GB of junctioned
            # route files we must not rewrite), so the SUMO-wide defaults are the only lever.
            if cf:
                txt = _cfg_set(txt, "default.carfollowmodel", cf, "processing")
            txt = _cfg_set(txt, "default.speeddev", f"{mapgen.speed_dev():g}", "processing")
        cfg.write_text(txt, encoding="utf-8")
    scj["updateInterval"] = update_ms
    if vtype_params:
        merged = dict(scj.get("additionalVehicleTypeParameters") or {})
        for name, params in vtype_params.items():
            merged[name] = {**(merged.get(name) or {}), **params}
        scj["additionalVehicleTypeParameters"] = merged
    scj_path.write_text(json.dumps(scj, indent=2), encoding="utf-8")
    return {"sumo_step_ms": update_ms, "mosaic_sync_ms": update_ms,
            "sumo_step_source": "MOSAIC SumoAmbassador --step-length (= updateInterval); it "
                                "overrides the sumocfg, which is rewritten to match",
            "sumocfg_step_ms_shipped": sorted(set(shipped)),
            "car_follow_model": cf or "sumo-default",
            # lateral dynamics (see the docstring): sublane model on == continuous lane changes
            "sublane_model": lat_res > 0,
            "lateral_resolution_m": (lat_res if lat_res > 0 else None),
            "lane_change_model": ("SL2015 (auto-selected by --lateral-resolution)" if lat_res > 0
                                  else "LC2013 (SUMO default; lane changes are instantaneous)"),
            "lateral_vtype_attrs": (mapgen.lateral_vtype_attrs() if kind == "flow" else {}),
            "lateral_vtype_source": (
                None if lat_res <= 0 else
                "MOSAIC additionalVehicleTypeParameters (flow map: MOSAIC owns the vTypes)"
                if kind == "flow" else
                "SUMO SL2015 defaults (route map: the scenario's own route files own the vTypes "
                "and must not be rewritten)"),
            "vtype_overrides": sorted(vtype_params or {})}


def _apply_demand(dst: Path, kind: str) -> dict:
    """Opt-in alternative demand source. Returns what was resolved, for the manifest.

    ``SCMS_DEMAND=intas`` (the default) touches nothing: the scenario keeps the route set InTAS
    calibrated against November 2019, so every result already published against it reproduces.

    ``SCMS_DEMAND=calibrated`` points ``route-files`` at a route set built by
    ``tools/calibrate_demand.py``, whose simulated induction-loop counts were fitted with SUMO's
    ``routeSampler`` to counts MEASURED at the city's own signal loops. Pedestrians and the
    scheduled bus network are kept from the source scenario (they are not demand under
    calibration, and the count targets already have the scheduled bus passages subtracted).
    ``SCMS_DEMAND_DETECTORS`` additionally swaps the E1 layout for the repaired one.

    Nothing here is a scale factor: the swapped route file is a per-route sample count solved
    against 55 independent counting locations. The route file's own ``.meta.json`` (written by
    ``calibrate_demand.py sample``) carries the exact routeSampler invocation, the sha256 of every
    input and the count windows that were fitted; the held-out error must be quoted from a
    ``calibrate_demand.py grade`` report against a window that is NOT in that list.
    """
    src = mapgen.demand_source()
    info = {"demand_source": src, "demand_routes": None, "demand_detectors": None,
            "demand_kept_routes": None}
    if src == "intas":
        return info
    if kind != "route":
        raise SystemExit("SCMS_DEMAND=calibrated applies to route maps (InTAS) only; "
                         f"'{kind}' maps get their demand from MOSAIC vehicle flows.")
    routes = mapgen.demand_routes()
    if not routes:
        raise SystemExit("SCMS_DEMAND=calibrated requires SCMS_DEMAND_ROUTES=<calibrated .rou.xml>."
                         " Build one with tools/calibrate_demand.py (layout -> candidates -> "
                         "targets -> sample).")
    rp = Path(routes).expanduser().resolve()
    if not rp.exists():
        raise SystemExit(f"SCMS_DEMAND_ROUTES: {rp} does not exist")
    dets = mapgen.demand_detectors()
    dp = Path(dets).expanduser().resolve() if dets else None
    if dp and not dp.exists():
        raise SystemExit(f"SCMS_DEMAND_DETECTORS: {dp} does not exist")
    keep = mapgen.demand_keep_routes()
    kept_all = []
    for cfg in _sumocfgs(dst):
        txt = cfg.read_text(encoding="utf-8", errors="replace")
        old = [r.strip() for r in (_cfg_get(txt, "route-files") or "").split(",") if r.strip()]
        kept = [r for r in keep if r in old]
        kept_all = kept
        rel = os.path.relpath(rp, cfg.parent).replace("\\", "/")
        txt = _cfg_set(txt, "route-files", ",".join(kept + [rel]), "input")
        if dp:
            adds = [x.strip() for x in (_cfg_get(txt, "additional-files") or "").split(",")
                    if x.strip()]
            rel_d = os.path.relpath(dp, cfg.parent).replace("\\", "/")
            # the repaired layout REPLACES the shipped one; loading both would define every
            # detector id twice and SUMO refuses to start.
            adds = [a for a in adds if Path(a).name != "InTAS_E1.add.xml"]
            txt = _cfg_set(txt, "additional-files", ",".join(adds + [rel_d]), "input")
        cfg.write_text(txt, encoding="utf-8")
    info.update({"demand_routes": str(rp), "demand_detectors": (str(dp) if dp else None),
                 "demand_kept_routes": kept_all,
                 "demand_note": "routeSampler-calibrated against measured Ingolstadt loop counts "
                                "(tools/calibrate_demand.py); the fitted count windows and the "
                                "exact routeSampler invocation are in the route file's "
                                ".meta.json side-car"})
    print(f"[gen_scenario] demand: calibrated <- {rp}", file=sys.stderr)
    return info


def _vtype_overrides(proto_names: list[str]) -> dict:
    """MOSAIC ``additionalVehicleTypeParameters``: raw SUMO vType attributes per prototype.

    MOSAIC generates ``mosaic_types.add.xml`` from the mapping prototypes for MOSAIC-spawned
    (flow) traffic and hard-codes ``carFollowModel="Krauss"`` and ``speedDev="0.0"`` there, so a
    scalar speedFactor collapses to "everybody drives exactly the limit". These overrides are
    merged over that file by SumoVehicleTypesWriter, which is the only supported way to reach
    those attributes from a scenario config."""
    if not proto_names:
        return {}
    cf = mapgen.cf_model()
    sf = mapgen.speed_factor_spec()
    dev = mapgen.speed_dev()
    lat = mapgen.lateral_vtype_attrs()   # {} unless the sublane model is on
    out = {}
    for name in proto_names:
        params = {"speedFactor": sf, "speedDev": f"{dev:g}", **lat}
        if cf:
            params["carFollowModel"] = cf
        out[name] = params
    return out


def _deviations(jitter: float, base: dict) -> dict:
    """MOSAIC per-vehicle parameter deviations (absolute std-devs) from a relative jitter."""
    return {k: round(float(base[k]) * jitter, 3)
            for k in ("accel", "decel", "tau", "minGap", "length") if k in base}


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

    # mapping: vehicles always get our app. Route maps (InTAS) match SUMO vehicle types to
    # prototypes by name, so the app goes on prototypes; flow maps spawn from vehicle flows,
    # so the app goes on the flow types. Non-vehicle units are dropped unless explicitly kept
    # (a vehicle-only app on an RSU/TL/server unit crashes MOSAIC at unit start-up).
    mp = dst / "mapping" / "mapping_config.json"
    m = json.loads(mp.read_text(encoding="utf-8"))
    keep_units = {u.strip() for u in os.environ.get("SCMS_KEEP_UNITS", "").split(",") if u.strip()}
    # Snapshot the source scenario's own RSUs BEFORE stripping: SCMS_RSU_PLACEMENT=keep is documented
    # as "preserve whatever the source scenario shipped", and reading m["rsus"] after the pop below
    # always found an empty list (the placement silently produced zero RSUs and the manifest recorded
    # rsu_placement: null).
    src_rsus = [dict(r) for r in (m.get("rsus") or [])]
    for k in ("rsus", "trafficLights", "servers", "chargingStations", "tmcs"):
        if k not in keep_units:
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
    # Vehicle dynamics. On FLOW maps the prototype IS the SUMO vType, so the SCMS_VEH_* knobs
    # apply there. On ROUTE maps (InTAS) SUMO owns the vTypes — MOSAIC never pushes prototype
    # kinematics back into SUMO for route-file traffic — so overwriting all 45 InTAS prototypes
    # with one uniform set only flattened MOSAIC's own view of the fleet. Keep their diversity
    # by default; SCMS_VEH_APPLY=1 restores the historical behaviour.
    vp = mapgen.veh_params()
    jitter = mapgen.vtype_jitter()
    apply_vp = mapgen._envb("SCMS_VEH_APPLY", kind == "flow")
    for proto in m.get("prototypes", []):
        if apply_vp:
            proto.update({"accel": vp["accel"], "decel": vp["decel"], "length": vp["length"],
                          "maxSpeed": vp["maxSpeed"], "minGap": vp["minGap"],
                          "sigma": vp["sigma"], "tau": vp["tau"]})
        if kind == "flow" and jitter > 0 and mapgen.vtype_samples() > 1:
            # MOSAIC draws one deviated VehicleType per spawned vehicle from these std-devs and
            # pushes it to SUMO per vehicle. Flow maps only: for route-file traffic SUMO owns the
            # vType, so a deviated MOSAIC-side type would just disagree with the actual physics.
            base = {k: proto.get(k, vp.get(k)) for k in ("accel", "decel", "tau", "minGap", "length")}
            proto["deviations"] = _deviations(jitter, {k: v for k, v in base.items() if v is not None})
    proto_names = [p.get("name") for p in m.get("prototypes", []) if p.get("name")]
    # Only the types a vehicle FLOW actually spawns reach MOSAIC's generated vType file, so those
    # are the only ones worth overriding (an unused RSU/TrafficLight prototype would just be noise).
    flow_types = [ty.get("name") for f in m.get("vehicles", []) for ty in (f.get("types") or [])
                  if ty.get("name")]

    # infrastructure: place RSUs on the real network once an RSU application exists (they used
    # to be stripped unconditionally, which is why the MOSAIC path had no always-trusted reporters).
    primary_cfg = _primary_sumocfg(dst, json.loads((dst / "sumo" / "sumo_config.json").read_text(
        encoding="utf-8")) if (dst / "sumo" / "sumo_config.json").exists() else {})
    net_file = _net_of(primary_cfg) if primary_cfg else None
    n_rsu = mapgen.rsu_count()
    rsu_app = mapgen.rsu_app()
    placement = mapgen.rsu_placement()
    rsus = []
    if n_rsu > 0:
        if not mapgen.app_in_jar(rsu_app):
            raise SystemExit(f"SCMS_RSUS={n_rsu} but {rsu_app} is not in {JAR.name}; "
                             "build an RSU application first or set SCMS_RSUS=0.")
        if placement == "keep":
            rsus = src_rsus                      # the source scenario's own units, pre-strip
            for r in rsus:
                r["applications"] = [rsu_app]
            if not rsus:
                print(f"[gen_scenario] SCMS_RSU_PLACEMENT=keep but {srctype}:{srcname} ships no "
                      "RSUs in its mapping_config.json -> 0 RSUs (use SCMS_RSU_PLACEMENT="
                      "junction|grid to place them on the net).", file=sys.stderr)
        elif net_file:
            # place against the projection MOSAIC will use (scenario_config), not the net's own
            pj = cfg.get("simulation", {}).get("projection", {})
            co = pj.get("cartesianOffset") or {}
            cc = pj.get("centerCoordinates") or {}
            proj = None
            if "x" in co and "longitude" in cc:
                proj = (float(co["x"]), float(co["y"]),
                        mapgen.utm_zone_of(cc["longitude"]), float(cc.get("latitude", 0)) >= 0)
            rsus = mapgen.rsu_units(net_file, n_rsu, rsu_app, placement, proj)
        if rsus:
            m["rsus"] = rsus
            print(f"[gen_scenario] {len(rsus)} RSU(s) -> {rsu_app}", file=sys.stderr)
    mp.write_text(json.dumps(m, indent=2), encoding="utf-8")

    # per-scenario SNS radio config (SCMS_RADIO_RANGE / SCMS_RADIO_LOSS)
    mapgen.write_sns_config(dst)

    # route-based traffic density: inject SUMO --scale into the sumocfg(s).
    if scale and float(scale) != 1.0:
        for scfg in _sumocfgs(dst):
            txt = scfg.read_text(encoding="utf-8")
            scfg.write_text(_cfg_set(txt, "scale", str(scale), "processing"), encoding="utf-8")

    # demand source (default: the scenario's own routes). Runs before _tune_sumo so the
    # step/lateral rewrites land on the final route-files/additional-files selection.
    demand_res = _apply_demand(dst, kind)

    # SUMO/MOSAIC coupling: 100 ms sync + EIDM + per-driver speed distribution.
    sumo_res = _tune_sumo(
        dst, kind,
        _vtype_overrides(sorted(set(flow_types) or set(proto_names)) if kind == "flow" else []))

    route_files = ""
    if primary_cfg:
        route_files = _cfg_get(primary_cfg.read_text(encoding="utf-8", errors="replace"),
                               "route-files") or ""
    # Network classification + radio range: the two things datagen.realism_bench cannot infer from a
    # MOSAIC dataset on its own (it resolves speed/headway reference bands from `road_network` and
    # reconstructs link distances from the radio range). Derived from the actual net, not guessed.
    rn_token, rn_evidence = mapgen.road_network_token(key, net_file)
    resolved = {
        "source": f"{srctype}:{srcname}", "duration": dur, "seed": seed, "scale": scale,
        "max_vehicles": max_vehicles, "target_flow": target_flow, "lanes": lanes,
        "road_network": rn_token, "regime": mapgen.regime_of(rn_token),
        "road_network_evidence": rn_evidence, "radio_range_m": mapgen.radio_range_m(),
        "speed_factor": mapgen.speed_factor_spec(), "speed_dev": mapgen.speed_dev(),
        "vtype_samples": mapgen.vtype_samples(), "vtype_jitter": jitter,
        "veh_params_applied_to_prototypes": apply_vp,
        "prototypes": proto_names,
        "rsus": len(rsus), "rsu_app": rsu_app if rsus else None,
        # the RESOLVED knob, recorded whether or not it produced units: a run that asked for 'keep'
        # and got nothing must still say so in the manifest.
        "rsu_placement": (placement if n_rsu > 0 else None),
        "rsus_in_source_mapping": len(src_rsus),
        "kept_units": sorted(keep_units),
        "sumocfg": primary_cfg.name if primary_cfg else None,
        "net_file": net_file.name if net_file else None,
        "route_files": [r.strip() for r in route_files.split(",") if r.strip()],
        **demand_res,
        **sumo_res,
    }
    inputs = [cfgp, mp, dst / "sumo" / "sumo_config.json", dst / "sns" / "sns_config.json",
              dst / "application" / JAR.name]
    inputs += _sumocfgs(dst)
    if net_file:
        inputs.append(net_file)
    inputs += sorted((dst / "sumo").glob("*.rou.xml"))
    # an out-of-tree calibrated demand must still be hashed into the manifest, or the run is
    # not reproducible from it
    for extra in (demand_res.get("demand_routes"), demand_res.get("demand_detectors")):
        if extra:
            inputs.append(Path(extra))
    manifest = mapgen.write_scenario_manifest(dst, key, kind, resolved, inputs)

    return {"scenario_config": str(cfgp), "dataset_dir": str(REPO / "datasets" / key),
            "kind": kind, "manifest": str(manifest),
            "inputs_json": str(dst / mapgen.INPUTS_NAME)}


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
