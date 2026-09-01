#!/usr/bin/env python3
"""Calibrate InTAS demand against measured Ingolstadt loop counts with routeSampler.

The InTAS scenario delivers 42-48 % of the flow the city actually measures at its signal
loops (``docs/realism/GEH-RESULT.md``).  This tool builds a demand set whose *simulated*
loop counts match *measured* loop counts, using SUMO's own ``routeSampler.py``, and it
validates that demand set on days routeSampler never saw.

It never hand-tunes a scale factor.  ``--scale`` is a single number fitted to the very
statistic the gate reports; routeSampler instead solves for a per-route sample count that
reproduces every counting location independently, which is a genuinely over-determined
problem (96 detector edges, one degree of freedom per candidate route) and can therefore
fail visibly.

Pipeline (each stage is a subcommand and writes its own provenance JSON)::

    layout      InTAS_E1.add.xml + net -> corrected + coverage detector layouts
    candidates  InTAS route files      -> a candidate route pool for one time band
    targets     fetched ref counts     -> routeSampler edgeData count targets
    sample      pool + targets         -> calibrated route set (routeSampler)
    grade       E1 output + ref counts -> per-station GEH + the four FHWA gates
    report      grade reports          -> calibration vs HELD-OUT, and the gap between them
    edges       E1 output + targets    -> per-edge lane share and target residuals
    scenario    calibrated routes      -> an opt-in sumocfg beside the InTAS one

CLOCKS.  The API stamps ``phenomenonTime`` in UTC; the InTAS SUMO clock is local Ingolstadt
time (CET = UTC+1 in November).  So the AM peak is measured ``06:00-07:00Z`` == SUMO
``25200-28800`` (warm-up ``05:00-06:00Z`` == ``21600-25200``) and the PM peak is measured
``15:00-16:00Z`` == SUMO ``57600-61200`` (warm-up ``14:00-15:00Z`` == ``54000-57600``).
Always grade a FULL CLOCK HOUR: GEH is not scale-free (``GEH(k*m, k*c) = sqrt(k)*GEH(m, c)``),
so a 300 s window extrapolated to veh/h inflates every value by sqrt(12) = 3.46.

VINTAGE.  InTAS demand is calibrated to November 2019; every count used here is from
November 2023.  Any number this tool produces is validation against present-day reality, not
against the scenario's own calibration epoch, and must be reported as such.

REPRODUCE (PowerShell; . C:/Users/Administrator/tools/env.ps1 first)::

    $S = "scms-sim/scenarios/gen_intas_urban_low/sumo"
    python tools/fetch_ingolstadt_counts.py --det-add $S/InTAS_E1.add.xml `
        --begin 2023-11-14T06:00:00Z --end 2023-11-14T07:00:00Z `
        --include exact+subset --out .cache/calib/ref_20231114_0600Z.json      # ... x N windows
    python tools/calibrate_demand.py layout --det-add $S/InTAS_E1.add.xml `
        --net $S/ingolstadt.net.xml --out-dir .cache/calib/layout
    python tools/calibrate_demand.py candidates --route-files $S/routes/InTAS_0*.rou.xml `
        --begin 18000 --end 32400 --out .cache/calib/cand_am.rou.xml
    python tools/calibrate_demand.py targets --layout-map .cache/calib/layout/layout_map.json `
        --interval "21600:25200=.cache/calib/ref_20231114_0500Z.json,.cache/calib/ref_20231116_0500Z.json" `
        --interval "25200:28800=.cache/calib/ref_20231114_0600Z.json,.cache/calib/ref_20231116_0600Z.json" `
        --bus-flows $S/routes/BusRoutes.flow.xml --out .cache/calib/targets_am.edg.xml
    python tools/calibrate_demand.py sample --candidates .cache/calib/cand_am.rou.xml `
        --targets .cache/calib/targets_am.edg.xml --out .cache/calib/calibrated_am.rou.xml `
        --begin 21600 --end 28800 --interval 3600 --seed 42 --prefix calAM
    python tools/calibrate_demand.py scenario --sumocfg $S/InTAS_buildings.sumocfg `
        --routes .cache/calib/calibrated_am.rou.xml `
        --additional $S/BusStations.add.xml .cache/calib/layout/calib_layout.add.xml `
                     $S/buildings.poly.xml --name InTAS_calibrated_am.sumocfg
    sumo -c $S/InTAS_calibrated_am.sumocfg --begin 21600 --end 28800 --output-prefix cAM_ --seed 42
    python tools/calibrate_demand.py grade --det-out .cache/calib/layout/cAM_calib_fixed_det.xml `
        --layout-map .cache/calib/layout/layout_map.json --id-prefix fx_ --begin 25200 --end 28800 `
        --ref ".cache/calib/ref_20231114_0600Z.json=calibration" `
        --ref ".cache/calib/ref_20231121_0600Z.json=held-out" --json .cache/calib/reports/cAM.json
    python tools/calibrate_demand.py report --grade .cache/calib/reports/*.json `
        --out .cache/calib/reports/summary.json

Every stage writes a ``.meta.json`` side-car carrying the sha256 of each input, so the chain
count window -> target -> route set -> simulated detector output is auditable end to end.

LICENCE / DATA HYGIENE.  The measured counts carry no formally stated licence (see
``docs/realism/GEH-VALIDATION.md``).  Every stage that touches them refuses to write inside
the repository unless the destination is under a git-ignored cache root (``.cache/`` or
``.realism_cache/``).  Pass ``--i-know`` only if you have a written licence.

THE LANE DEFECT (found while building this, and independent of demand).  74 of the 194
named ``e1Detector`` entries in ``InTAS_E1.add.xml`` sit on SUMO *sidewalk* lanes -- lane
index 0 of their edge, ``allow="pedestrian"``, 2.00 m wide.  A passenger car can never
drive there, so those 74 loops record exactly 0 vehicles in every simulated hour, while the
real loops they map to measured 16 673 of the 45 713 reference vehicles (36.5 %) in window
A.  ``layout`` detects this structurally (a lane that forbids ``passenger``), repairs it by
shifting every detector on such an edge up by the number of leading non-car lanes, and
proves the repair: on this network all 74 move onto a car lane with zero collisions and
zero overflows -- the signature of an off-by-one introduced when sidewalks were added to
the network after the loops were placed.  The original ids are never modified; the repaired
copies are written as ``fx_<id>`` into a separate file so one run measures both layouts.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import math
import os
import statistics
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
# Roots the repo's .gitignore excludes; measured counts and anything derived from them may
# only be written here.  Checked against the resolved path, so a symlink cannot escape.
IGNORED_ROOTS = (REPO / ".cache", REPO / ".realism_cache")

LICENCE_NOTE = (
    "Stadt Ingolstadt / SAVeNoW loop counts: licence NOT formally stated. Never commit "
    "these counts or anything derived from them. See docs/realism/GEH-VALIDATION.md.")

FIXED_PREFIX = "fx_"
COV_PREFIX = "cov_"
FIXED_OUT = "calib_fixed_det.xml"
COV_OUT = "calib_cov_det.xml"
ORIG_OUT = "InTAS_Detectors_Output.xml"


# --------------------------------------------------------------------------- helpers
def sha256_file(p) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def guard_out(path, i_know: bool = False) -> Path:
    """Refuse to write count-derived data into a tracked part of the repository."""
    p = Path(path).resolve()
    try:
        p.relative_to(REPO)
    except ValueError:
        return p                                    # outside the repo: caller's problem
    if any(_is_under(p, root) for root in IGNORED_ROOTS):
        return p
    if i_know:
        print(f"[warn] {p} is inside the repo and NOT under a git-ignored cache root. "
              f"{LICENCE_NOTE}", file=sys.stderr)
        return p
    raise SystemExit(
        f"[refused] {p} is inside the repository but not under {', '.join(str(r) for r in IGNORED_ROOTS)}.\n"
        f"          {LICENCE_NOTE}\n"
        f"          Write under .cache/ instead (or pass --i-know if a licence now exists).")


def _is_under(p: Path, root: Path) -> bool:
    try:
        p.relative_to(root)
        return True
    except ValueError:
        return False


def write_json(path, obj, i_know=False):
    p = guard_out(path, i_know)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(obj, indent=1, ensure_ascii=False), encoding="utf-8")
    return p


def geh(m: float, c: float) -> float:
    if m == 0 and c == 0:
        return 0.0
    return math.sqrt(2.0 * (m - c) ** 2 / (m + c))


def quantile(xs, q: float):
    if not xs:
        return None
    s = sorted(xs)
    if len(s) == 1:
        return float(s[0])
    i = q * (len(s) - 1)
    lo = int(math.floor(i))
    hi = min(lo + 1, len(s) - 1)
    return float(s[lo] + (s[hi] - s[lo]) * (i - lo))


# --------------------------------------------------------------------------- parsing
def parse_det_add(path):
    """id -> {station, lane, edge, idx, pos}. Unnamed detectors (gate counters) are kept
    with station None so a layout round-trip never silently loses one."""
    root = ET.parse(path).getroot()
    out = {}
    for e in root.iter("e1Detector"):
        lane = e.get("lane")
        edge, _, idx = lane.rpartition("_")
        out[e.get("id")] = {"station": e.get("name"), "lane": lane, "edge": edge,
                            "idx": int(idx), "pos": float(e.get("pos")),
                            "freq": e.get("freq") or "900.00"}
    return out


def lane_allows_car(allow, disallow) -> bool:
    if allow is not None:
        toks = allow.split()
        return "passenger" in toks or "all" in toks
    if disallow is not None:
        return "passenger" not in disallow.split()
    return True


def parse_net_lanes(net_path):
    """edge -> [(index, lane_id, allows_car, length)], normal (non-internal) edges only."""
    lanes = {}
    for _, el in ET.iterparse(net_path, events=("end",)):
        if el.tag == "edge":
            if el.get("function") != "internal":
                rows = []
                for ln in el.findall("lane"):
                    rows.append((int(ln.get("index")), ln.get("id"),
                                 lane_allows_car(ln.get("allow"), ln.get("disallow")),
                                 float(ln.get("length"))))
                lanes[el.get("id")] = sorted(rows)
            el.clear()
        elif el.tag in ("junction", "connection", "tlLogic", "type", "roundabout"):
            el.clear()
    return lanes


# --------------------------------------------------------------------------- layout
def build_layout(det_add, net):
    dets = parse_det_add(det_add)
    lanes = parse_net_lanes(net)
    by_edge = collections.defaultdict(list)
    for did, d in dets.items():
        if d["station"]:
            by_edge[d["edge"]].append(did)

    edges = {}
    fixed_lane = {}
    problems = []
    for edge, ids in sorted(by_edge.items()):
        L = lanes.get(edge)
        if L is None:
            problems.append({"edge": edge, "problem": "edge_not_in_net"})
            continue
        car_idx = [i for i, _, c, _ in L if c]
        lead_non_car = 0
        for i, _, c, _ in L:
            if c:
                break
            lead_non_car += 1
        idxs = sorted(dets[d]["idx"] for d in ids)
        on_non_car = [i for i in idxs if not L[i][2]]
        shift = lead_non_car if on_non_car else 0
        new = [i + shift for i in idxs]
        why = []
        if len(set(new)) != len(new):
            why.append("collision")
        if new and max(new) >= len(L):
            why.append("overflow")
        if any(i < len(L) and not L[i][2] for i in new):
            why.append("still_non_car")
        if why:
            problems.append({"edge": edge, "problem": "+".join(why),
                             "old": idxs, "new": new, "n_lanes": len(L)})
            shift = 0
            new = idxs
        for d in ids:
            fixed_lane[d] = f"{edge}_{dets[d]['idx'] + shift}"
        edges[edge] = {
            "n_lanes": len(L), "n_car_lanes": len(car_idx),
            "lead_non_car_lanes": lead_non_car, "shift": shift,
            "detectors": sorted(ids),
            "detector_lane_idx_original": idxs,
            "detector_lane_idx_fixed": new,
            "car_lane_ids": [L[i][1] for i in car_idx],
            "n_detectors": len(ids),
            # a counting location is only exact when every car lane of the edge carries
            # exactly one loop: otherwise the measured sum is a lower bound on edge flow
            "fully_instrumented": len(ids) == len(car_idx),
            "stations": sorted({dets[d]["station"] for d in ids}),
            "length_m": round(L[0][3], 2) if L else None,
        }

    n_moved = sum(1 for d, ln in fixed_lane.items() if ln != dets[d]["lane"])
    on_non_car_before = [d for d in fixed_lane
                         if not lanes[dets[d]["edge"]][dets[d]["idx"]][2]]
    on_non_car_after = [d for d, ln in fixed_lane.items()
                        if not lanes[ln.rpartition("_")[0]][int(ln.rpartition("_")[2])][2]]
    return {
        "det_add": str(det_add), "det_add_sha256": sha256_file(det_add),
        "net": str(net), "net_sha256": sha256_file(net),
        "n_detectors_total": len(dets),
        "n_detectors_named": sum(1 for d in dets.values() if d["station"]),
        "n_stations": len({d["station"] for d in dets.values() if d["station"]}),
        "n_detector_edges": len(edges),
        "n_detectors_on_non_car_lane_before": len(on_non_car_before),
        "n_detectors_on_non_car_lane_after": len(on_non_car_after),
        "n_detectors_moved": n_moved,
        "detectors_on_non_car_lane_before": sorted(on_non_car_before),
        "n_edges_fully_instrumented": sum(1 for e in edges.values() if e["fully_instrumented"]),
        "n_edges_partially_instrumented": sum(1 for e in edges.values()
                                              if not e["fully_instrumented"]),
        "problems": problems,
        "detectors": {d: {**dets[d], "fixed_lane": fixed_lane.get(d)} for d in dets},
        "edges": edges,
    }


def _e1(fh, did, lane, pos, freq, name, outfile):
    # SUMO rejects name="" outright, so an unnamed InTAS gate counter must stay unnamed.
    nm = f'name="{name}" ' if name else ""
    fh.write(f'    <e1Detector id="{did}" lane="{lane}" pos="{pos:.2f}" freq="{freq}" '
             f'{nm}file="{outfile}" friendlyPos="1"/>\n')


def write_layout_files(layout, outdir: Path):
    outdir.mkdir(parents=True, exist_ok=True)
    dets, edges = layout["detectors"], layout["edges"]
    head = ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<!-- generated by tools/calibrate_demand.py layout -->\n'
            '<additional xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" '
            'xsi:noNamespaceSchemaLocation="http://sumo.dlr.de/xsd/additional_file.xsd">\n')

    fixed_p = outdir / "InTAS_E1_fixed.add.xml"
    with open(fixed_p, "w", encoding="utf-8") as fh:
        fh.write(head)
        for did in sorted(dets):
            d = dets[did]
            if not d["station"]:
                continue
            _e1(fh, FIXED_PREFIX + did, d["fixed_lane"], d["pos"], d["freq"],
                d["station"], FIXED_OUT)
        fh.write("</additional>\n")

    cov_p = outdir / "InTAS_E1_cov.add.xml"
    with open(cov_p, "w", encoding="utf-8") as fh:
        fh.write(head)
        for edge in sorted(edges):
            e = edges[edge]
            pos_by_lane = {}
            for did in e["detectors"]:
                pos_by_lane[dets[did]["fixed_lane"]] = dets[did]["pos"]
            default_pos = (statistics.median(pos_by_lane.values()) if pos_by_lane else 5.0)
            for lane in e["car_lane_ids"]:
                _e1(fh, COV_PREFIX + lane, lane, pos_by_lane.get(lane, default_pos),
                    "900.00", edge, COV_OUT)
        fh.write("</additional>\n")

    merged_p = outdir / "calib_layout.add.xml"
    with open(merged_p, "w", encoding="utf-8") as fh:
        fh.write(head)
        fh.write("    <!-- 1: the InTAS layout verbatim (ids unchanged) -->\n")
        for did in sorted(dets):
            d = dets[did]
            _e1(fh, did, d["lane"], d["pos"], d["freq"], d["station"] or "", ORIG_OUT)
        fh.write("    <!-- 2: the same loops moved off sidewalk lanes -->\n")
        for did in sorted(dets):
            d = dets[did]
            if d["station"]:
                _e1(fh, FIXED_PREFIX + did, d["fixed_lane"], d["pos"], d["freq"],
                    d["station"], FIXED_OUT)
        fh.write("    <!-- 3: every car lane of every detector edge (lane-share diagnostic) -->\n")
        for edge in sorted(edges):
            e = edges[edge]
            pos_by_lane = {dets[d]["fixed_lane"]: dets[d]["pos"] for d in e["detectors"]}
            default_pos = (statistics.median(pos_by_lane.values()) if pos_by_lane else 5.0)
            for lane in e["car_lane_ids"]:
                _e1(fh, COV_PREFIX + lane, lane, pos_by_lane.get(lane, default_pos),
                    "900.00", edge, COV_OUT)
        fh.write("</additional>\n")
    return {"fixed": fixed_p, "cov": cov_p, "merged": merged_p}


def cmd_layout(a):
    layout = build_layout(a.det_add, a.net)
    outdir = Path(a.out_dir)
    paths = write_layout_files(layout, outdir)
    layout["outputs"] = {k: str(v) for k, v in paths.items()}
    layout["outputs_sha256"] = {k: sha256_file(v) for k, v in paths.items()}
    mp = outdir / "layout_map.json"
    mp.parent.mkdir(parents=True, exist_ok=True)
    mp.write_text(json.dumps(layout, indent=1, ensure_ascii=False), encoding="utf-8")
    print(f"detectors {layout['n_detectors_total']} "
          f"({layout['n_detectors_named']} named, {layout['n_stations']} stations) "
          f"over {layout['n_detector_edges']} edges")
    print(f"on a lane that forbids passenger cars: "
          f"{layout['n_detectors_on_non_car_lane_before']} before -> "
          f"{layout['n_detectors_on_non_car_lane_after']} after "
          f"({layout['n_detectors_moved']} moved)")
    print(f"edges fully instrumented {layout['n_edges_fully_instrumented']} / "
          f"{layout['n_detector_edges']} (the rest are lower bounds on edge flow)")
    if layout["problems"]:
        print(f"PROBLEMS: {len(layout['problems'])} edge(s) could not be repaired:")
        for p in layout["problems"][:20]:
            print("   ", p)
    for k, v in paths.items():
        print(f"wrote {k:7s} {v}")
    print(f"wrote map     {mp}")
    return 0 if not layout["problems"] else 4


# --------------------------------------------------------------------------- candidates
def cmd_candidates(a):
    """Extract one representative route per InTAS vehicle departing in [begin, end).

    InTAS vehicles carry a ``routeDistribution``; SUMO draws from it at insertion.  The
    highest-probability alternative is taken as the vehicle's representative route, which
    is the modal choice of the calibrated 2019 assignment.  Duplicates are KEPT: their
    multiplicity is InTAS's own OD prior, and routeSampler samples the pool uniformly, so
    keeping them makes the prior the sampling distribution.
    """
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    n_veh = n_kept = 0
    lens = []
    seen = set()
    with open(out, "w", encoding="utf-8") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n'
                 '<!-- candidate route pool, tools/calibrate_demand.py candidates -->\n'
                 '<routes>\n')
        for rf in a.route_files:
            ctx = ET.iterparse(rf, events=("start", "end"))
            _, root = next(ctx)          # keep the root so it can be emptied as we go:
            for ev, el in ctx:           # otherwise every parsed <vehicle> stays alive
                if ev != "end" or el.tag != "vehicle":
                    continue
                n_veh += 1
                dep = float(el.get("depart") or -1)
                if a.begin <= dep < a.end:
                    best, bestp = None, -1.0
                    for r in el.iter("route"):
                        p = float(r.get("probability") or 1.0)
                        if p > bestp:
                            best, bestp = r.get("edges"), p
                    if best:
                        n_kept += 1
                        lens.append(best.count(" ") + 1)
                        seen.add(best)
                        fh.write(f'    <route id="c{n_kept}" edges="{best}"/>\n')
                el.clear()
                root.clear()
            print(f"  {Path(rf).name}: cumulative kept {n_kept}", flush=True)
        fh.write("</routes>\n")
    meta = {"out": str(out), "sha256": sha256_file(out), "begin": a.begin, "end": a.end,
            "route_files": [{"path": str(r), "sha256": sha256_file(r)} for r in a.route_files],
            "n_vehicles_scanned": n_veh, "n_routes_kept": n_kept,
            "n_distinct_edge_sequences": len(seen),
            "route_edges_mean": round(statistics.fmean(lens), 2) if lens else None,
            "route_edges_median": statistics.median(lens) if lens else None,
            "selection": "highest-probability alternative of each vehicle's routeDistribution"}
    write_json(out.with_suffix(".meta.json"), meta, a.i_know)
    print(f"scanned {n_veh} vehicles, kept {n_kept} routes "
          f"({len(seen)} distinct edge sequences) -> {out}")
    return 0


# --------------------------------------------------------------------------- targets
def load_ref(path):
    d = json.load(open(path, encoding="utf-8"))
    per_det, stations = {}, {}
    for st, s in (d.get("station_details") or {}).items():
        if s.get("comparability") in (None, "unusable"):
            continue
        stations[st] = {"count": s.get("count"),
                        "comparability": s.get("comparability"),
                        "matched": set(s.get("matched_detector_ids") or [])}
        for did, r in (s.get("per_detector") or {}).items():
            if r.get("count") is not None:
                per_det[did] = float(r["count"])
    return {"path": str(path), "sha256": sha256_file(path), "per_detector": per_det,
            "stations": stations, "window": d.get("window") or d.get("provenance", {}).get("window"),
            "duration_s": d.get("duration_s") or 3600}


def bus_passages(bus_flow_xml, begin, end, edges_of_interest):
    """Exact number of scheduled bus passages per edge in [begin, end).

    Buses are part of the scenario on both sides of the comparison, so the routeSampler
    targets (which only produce cars) must be reduced by the buses that will cross the same
    loop.  Computed from the flow definitions rather than measured, so it is exact and does
    not need a simulation run.
    """
    root = ET.parse(bus_flow_xml).getroot()
    per_edge = collections.Counter()
    total = 0
    for fl in root.iter("flow"):
        fb = float(fl.get("begin") or 0)
        fe = float(fl.get("end") or 86400)
        per = fl.get("period")
        nveh = fl.get("number")
        vph = fl.get("vehsPerHour")
        deps = []
        if per is not None:
            step = float(per)
            t = fb
            while t < fe:
                deps.append(t)
                t += step
        elif vph is not None:
            step = 3600.0 / float(vph)
            t = fb
            while t < fe:
                deps.append(t)
                t += step
        elif nveh is not None:
            n = int(nveh)
            step = (fe - fb) / max(n, 1)
            deps = [fb + i * step for i in range(n)]
        else:
            deps = [fb]
        inwin = [t for t in deps if begin <= t < end]
        if not inwin:
            continue
        total += len(inwin)
        r = fl.find("route")
        if r is None:
            continue
        for e in r.get("edges").split():
            if e in edges_of_interest:
                per_edge[e] += len(inwin)
    return per_edge, total


def cmd_targets(a):
    layout = json.load(open(a.layout_map, encoding="utf-8"))
    edges, dets = layout["edges"], layout["detectors"]
    intervals = []
    for spec in a.interval:
        # BEGIN:END=ref1.json[,ref2.json...]
        head, _, files = spec.partition("=")
        b, _, e = head.partition(":")
        refs = [load_ref(f) for f in files.split(",") if f]
        if not refs:
            raise SystemExit(f"[usage] --interval {spec}: no reference file given")
        intervals.append({"begin": float(b), "end": float(e), "refs": refs})

    # a counting edge must be fully instrumented AND every one of its loops must be
    # measured in EVERY window, otherwise the target is a lower bound and routeSampler
    # would faithfully reproduce an under-count.
    cal_edges = []
    for edge, e in sorted(edges.items()):
        if not e["fully_instrumented"]:
            continue
        ok = all(all(d in iv_ref["per_detector"] for d in e["detectors"])
                 for iv in intervals for iv_ref in iv["refs"])
        if ok:
            cal_edges.append(edge)

    bus_edges = set(cal_edges)
    rows = []
    for iv in intervals:
        bus_per_edge = collections.Counter()
        n_bus = 0
        if a.bus_flows:
            bus_per_edge, n_bus = bus_passages(a.bus_flows, iv["begin"], iv["end"], bus_edges)
        per_edge = {}
        for edge in cal_edges:
            vals = [sum(r["per_detector"][d] for d in edges[edge]["detectors"])
                    for r in iv["refs"]]
            mean = statistics.fmean(vals)
            target = max(0.0, mean - bus_per_edge.get(edge, 0))
            per_edge[edge] = {"measured_mean": round(mean, 2),
                              "measured_per_day": [round(v, 2) for v in vals],
                              "measured_spread": (round(max(vals) - min(vals), 2)
                                                  if len(vals) > 1 else 0.0),
                              "bus_passages": bus_per_edge.get(edge, 0),
                              "target_cars": int(round(target))}
        iv["per_edge"] = per_edge
        iv["n_bus_departures_in_window"] = n_bus
        rows.append((iv["begin"], iv["end"], sum(v["target_cars"] for v in per_edge.values())))

    out = guard_out(a.out, a.i_know)
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "w", encoding="utf-8") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n'
                 '<!-- routeSampler count targets, tools/calibrate_demand.py targets.\n'
                 f'     {LICENCE_NOTE} -->\n<data>\n')
        for iv in intervals:
            fh.write(f'    <interval id="calib" begin="{iv["begin"]:.2f}" end="{iv["end"]:.2f}">\n')
            for edge in cal_edges:
                fh.write(f'        <edge id="{edge}" entered="{iv["per_edge"][edge]["target_cars"]}"/>\n')
            fh.write("    </interval>\n")
        fh.write("</data>\n")

    # coverage of the reference total that the counting edges represent
    cov = []
    for iv in intervals:
        m_all = sum(sum(r["per_detector"].values()) for r in iv["refs"]) / len(iv["refs"])
        m_cal = sum(v["measured_mean"] for v in iv["per_edge"].values())
        # per-station: how much of the measured flow the counting edges actually constrain.
        # A station with 0.0 here is entirely spatially held out -- routeSampler never sees it.
        n_days = len(iv["refs"])
        mean_det = collections.Counter()
        for r in iv["refs"]:
            for did, c in r["per_detector"].items():
                mean_det[did] += c / n_days
        st_all = collections.Counter()
        for did, c in mean_det.items():
            if dets.get(did, {}).get("station"):
                st_all[dets[did]["station"]] += c
        st_cal = collections.Counter()
        for edge in cal_edges:
            for d in edges[edge]["detectors"]:
                st_cal[dets[d]["station"]] += mean_det[d]
        cov.append({"begin": iv["begin"], "end": iv["end"],
                    "station_constrained_fraction": {
                        s: round(st_cal.get(s, 0.0) / st_all[s], 3) for s in sorted(st_all)},
                    "reference_total_mean": round(m_all, 1),
                    "counting_edge_total_mean": round(m_cal, 1),
                    "fraction_of_reference": round(m_cal / m_all, 4) if m_all else None,
                    "stations_with_zero_coverage": sorted(
                        s for s in st_all if st_cal.get(s, 0) < 1e-6)})

    meta = {
        "out": str(out), "sha256": sha256_file(out),
        "licence": LICENCE_NOTE,
        "layout_map": str(a.layout_map),
        "bus_flows": (str(a.bus_flows) if a.bus_flows else None),
        "bus_flows_sha256": (sha256_file(a.bus_flows) if a.bus_flows else None),
        "n_counting_edges": len(cal_edges), "counting_edges": cal_edges,
        "coverage": cov,
        "intervals": [{"begin": iv["begin"], "end": iv["end"],
                       "n_bus_departures_in_window": iv["n_bus_departures_in_window"],
                       "references": [{"path": r["path"], "sha256": r["sha256"]}
                                      for r in iv["refs"]],
                       "total_target_cars": sum(v["target_cars"] for v in iv["per_edge"].values()),
                       "per_edge": iv["per_edge"]} for iv in intervals],
    }
    write_json(out.with_suffix(".meta.json"), meta, a.i_know)
    print(f"counting edges: {len(cal_edges)} of {len(edges)} detector edges")
    for c in cov:
        print(f"  [{c['begin']:.0f},{c['end']:.0f}) reference {c['reference_total_mean']:.0f} veh, "
              f"counting edges cover {c['counting_edge_total_mean']:.0f} "
              f"({100*c['fraction_of_reference']:.1f}%)"
              + (f"; stations with NO counting edge: {','.join(c['stations_with_zero_coverage'])}"
                 if c["stations_with_zero_coverage"] else ""))
    for b, e, t in rows:
        print(f"  [{b:.0f},{e:.0f}) target cars {t}")
    print(f"wrote {out}")
    return 0


# --------------------------------------------------------------------------- sample
def cmd_sample(a):
    sumo_home = os.environ.get("SUMO_HOME")
    if not sumo_home:
        raise SystemExit("[usage] SUMO_HOME is not set; run . C:/Users/Administrator/tools/env.ps1")
    rs = Path(sumo_home) / "tools" / "routeSampler.py"
    if not rs.exists():
        raise SystemExit(f"[usage] {rs} not found")
    out = guard_out(a.out, a.i_know)
    out.parent.mkdir(parents=True, exist_ok=True)
    mism = out.with_name(out.stem + "_mismatch.xml")
    argv = [sys.executable, str(rs),
            "--route-files", str(a.candidates),
            "--edgedata-files", str(a.targets),
            "--edgedata-attribute", a.edgedata_attribute,
            "--output-file", str(out),
            "--mismatch-output", str(mism),
            "--begin", str(a.begin), "--end", str(a.end), "--interval", str(a.interval),
            "--seed", str(a.seed),
            "--prefix", a.prefix,
            "--attributes", a.attributes,
            "--min-count", str(a.min_count),
            "--verbose"]
    if a.total_count:
        argv += ["--total-count", a.total_count]
    if a.optimize:
        argv += ["--optimize", a.optimize]
    if a.minimize_vehicles:
        argv += ["--minimize-vehicles", str(a.minimize_vehicles)]
    print("+ " + " ".join(argv), flush=True)
    r = subprocess.run(argv, capture_output=True, text=True)
    sys.stdout.write(r.stdout[-8000:])
    sys.stderr.write(r.stderr[-8000:])
    if r.returncode != 0:
        raise SystemExit(f"routeSampler failed ({r.returncode})")

    n_veh = _inject_vtype(out, a.vtype_id, a.vtype_attrs)
    meta = {
        "out": str(out), "sha256": sha256_file(out), "n_vehicles": n_veh,
        "mismatch_output": str(mism), "mismatch_sha256": sha256_file(mism),
        "route_sampler": str(rs), "route_sampler_sha256": sha256_file(rs),
        "sumo_home": sumo_home,
        "argv": argv[1:],
        "command": " ".join(argv[1:]),
        "inputs": {"candidates": {"path": str(a.candidates), "sha256": sha256_file(a.candidates)},
                   "targets": {"path": str(a.targets), "sha256": sha256_file(a.targets)}},
        "vtype": {"id": a.vtype_id, "attrs": a.vtype_attrs},
        "stdout_tail": r.stdout[-4000:],
        "licence": LICENCE_NOTE,
    }
    write_json(out.with_suffix(".meta.json"), meta, a.i_know)
    print(f"wrote {out} ({n_veh} vehicles)")
    return 0


def _inject_vtype(route_file: Path, vtype_id: str, attrs: str) -> int:
    """routeSampler emits bare <vehicle> elements; give them a vType the sumocfg can bind.

    The vType is deliberately minimal: the InTAS sumocfg sets default.carfollowmodel=EIDM
    and default.speeddev=0.1, so an attribute-free vType inherits exactly the car-following
    model and driver heterogeneity the baseline run used.
    """
    txt = route_file.read_text(encoding="utf-8")
    n = txt.count("<vehicle ")
    decl = f'    <vType id="{vtype_id}" vClass="passenger"{(" " + attrs) if attrs else ""}/>\n'
    if "<routes" in txt and f'id="{vtype_id}"' not in txt:
        i = txt.index(">", txt.index("<routes")) + 1
        txt = txt[:i] + "\n" + decl + txt[i:]
        route_file.write_text(txt, encoding="utf-8")
    return n


# --------------------------------------------------------------------------- grade
def read_e1(det_out, begin, end):
    """detector id -> vehicles counted in intervals fully inside [begin, end)."""
    tot = collections.Counter()
    n_int = 0
    for _, el in ET.iterparse(det_out, events=("end",)):
        if el.tag == "interval":
            b = float(el.get("begin"))
            e = float(el.get("end"))
            if b >= begin and e <= end:
                tot[el.get("id")] += float(el.get("nVehContrib") or 0.0)
                n_int += 1
            el.clear()
    return tot, n_int


# FHWA Traffic Analysis Toolbox Vol III (FHWA-HRT-04-040, 2004) sect. 5. Read from the repo's
# curated refdata so this tool and tools/sumo_realism.py can never quote different thresholds;
# the literals are the documented values and only apply if the refdata file is missing.
_FHWA_FALLBACK = {"geh_link_max": 5.0, "geh_link_min_pass_fraction": 0.85, "geh_total_max": 4.0,
                  "total_flow_tolerance_fraction": 0.05, "link_flow_min_pass_fraction": 0.85,
                  "bands": {"below_700": {"max_abs_veh_h": 100.0},
                            "700_to_2700": {"max_rel": 0.15},
                            "above_2700": {"max_abs_veh_h": 400.0}}}
REFDATA = REPO / "src" / "scms_sim_ref" / "datagen" / "refdata" / "geh_criteria.json"


def load_fhwa():
    try:
        e = json.load(open(REFDATA, encoding="utf-8"))["entries"]
    except (OSError, KeyError, ValueError):
        return dict(_FHWA_FALLBACK, source="built-in fallback (refdata not readable)")
    return {"geh_link_max": float(e["geh_link_max"]["max"]),
            "geh_link_min_pass_fraction": float(e["geh_link_min_pass_fraction"]["min"]),
            "geh_total_max": float(e["geh_total_max"]["max"]),
            "total_flow_tolerance_fraction": float(e["total_flow_tolerance_fraction"]["max"]),
            "link_flow_min_pass_fraction": float(e["link_flow_min_pass_fraction"]["min"]),
            "bands": e["link_flow_tolerance_bands"]["value"],
            "source": str(REFDATA.relative_to(REPO)).replace("\\", "/")}


FHWA = load_fhwa()


def flow_tolerance_pass(m, c, bands=None):
    """FHWA per-link flow tolerance; the band is selected on the COUNTED (reference) flow."""
    b = bands or FHWA["bands"]
    if c < 700.0:
        return abs(m - c) <= float(b["below_700"]["max_abs_veh_h"])
    if c <= 2700.0:
        return abs(m - c) <= float(b["700_to_2700"]["max_rel"]) * c
    return abs(m - c) <= float(b["above_2700"]["max_abs_veh_h"])


def grade_stations(modelled, measured):
    stations = sorted(set(modelled) & set(measured))
    stations = [s for s in stations if not (modelled[s] == 0 and measured[s] == 0)]
    rows = []
    for s in stations:
        m, c = float(modelled[s]), float(measured[s])
        rows.append({"station": s, "modelled": round(m, 1), "measured": round(c, 1),
                     "ratio": round(m / c, 4) if c else None, "geh": round(geh(m, c), 3),
                     "geh_lt_5": geh(m, c) < FHWA["geh_link_max"],
                     "flow_tolerance_ok": flow_tolerance_pass(m, c)})
    tm = sum(r["modelled"] for r in rows)
    tc = sum(r["measured"] for r in rows)
    gehs = [r["geh"] for r in rows]
    ratios = [r["ratio"] for r in rows if r["ratio"] is not None]
    n = len(rows)
    pf = sum(1 for r in rows if r["geh_lt_5"]) / n if n else None
    tf = sum(1 for r in rows if r["flow_tolerance_ok"]) / n if n else None
    rel = (tm - tc) / tc if tc else None
    gates = [
        {"id": "geh.link_pass_fraction", "value": round(pf, 4) if pf is not None else None,
         "threshold": FHWA["geh_link_min_pass_fraction"], "cmp": ">=",
         "status": "pass" if (pf is not None and pf >= FHWA["geh_link_min_pass_fraction"]) else "fail"},
        {"id": "geh.total_flow_geh", "value": round(geh(tm, tc), 3),
         "threshold": FHWA["geh_total_max"], "cmp": "<=",
         "status": "pass" if geh(tm, tc) <= FHWA["geh_total_max"] else "fail"},
        {"id": "geh.total_flow_rel_error",
         "value": round(abs(rel), 4) if rel is not None else None,
         "threshold": FHWA["total_flow_tolerance_fraction"], "cmp": "<=",
         "status": "pass" if (rel is not None
                              and abs(rel) <= FHWA["total_flow_tolerance_fraction"]) else "fail"},
        {"id": "geh.link_flow_tolerance_pass_fraction",
         "value": round(tf, 4) if tf is not None else None,
         "threshold": FHWA["link_flow_min_pass_fraction"], "cmp": ">=",
         "status": "pass" if (tf is not None
                              and tf >= FHWA["link_flow_min_pass_fraction"]) else "fail"},
    ]
    return {
        "n_stations": n,
        "total_modelled": round(tm, 1), "total_measured": round(tc, 1),
        "total_rel_error": round(rel, 4) if rel is not None else None,
        "total_geh": round(geh(tm, tc), 3),
        "geh_median": round(statistics.median(gehs), 3) if gehs else None,
        "geh_p85": round(quantile(gehs, 0.85), 3) if gehs else None,
        "geh_max": round(max(gehs), 3) if gehs else None,
        "ratio_min": round(min(ratios), 4) if ratios else None,
        "ratio_p25": round(quantile(ratios, 0.25), 4) if ratios else None,
        "ratio_median": round(statistics.median(ratios), 4) if ratios else None,
        "ratio_p75": round(quantile(ratios, 0.75), 4) if ratios else None,
        "ratio_max": round(max(ratios), 4) if ratios else None,
        "n_under_075": sum(1 for r in ratios if r < 0.75),
        "n_within_075_125": sum(1 for r in ratios if 0.75 <= r <= 1.25),
        "n_over_125": sum(1 for r in ratios if r > 1.25),
        "n_geh_lt_5": sum(1 for r in rows if r["geh_lt_5"]),
        "criteria": {k: v for k, v in FHWA.items() if k != "bands"},
        "criteria_bands": FHWA["bands"],
        "criteria_source": "FHWA Traffic Analysis Toolbox Vol III (FHWA-HRT-04-040, 2004) "
                           f"sect. 5, via {FHWA['source']}",
        "gates": gates,
        "stations": rows,
    }


def modelled_by_station(counts, layout, ref, prefix=""):
    """Sum modelled loops per station, restricted to the loops the reference measured.

    Without the restriction a station where SAVeNoW instruments fewer loops than InTAS
    compares N modelled loops against an M-loop reference and flatters the model.
    """
    dets = layout["detectors"]
    out = collections.Counter()
    used = collections.Counter()
    for did, v in counts.items():
        # one E1 output can hold all three detector sets, so the prefix SELECTS a set --
        # matching loosely would sum the shipped and the repaired layout together.
        if prefix:
            if not did.startswith(prefix):
                continue
            base = did[len(prefix):]
        else:
            if did.startswith(FIXED_PREFIX) or did.startswith(COV_PREFIX):
                continue
            base = did
        d = dets.get(base)
        if not d or not d["station"]:
            continue
        st = d["station"]
        s = ref["stations"].get(st)
        if not s or base not in s["matched"]:
            continue
        out[st] += v
        used[st] += 1
    return out, used


def cmd_grade(a):
    layout = json.load(open(a.layout_map, encoding="utf-8"))
    counts, n_int = read_e1(a.det_out, a.begin, a.end)
    reports = []
    for spec in a.ref:
        # PATH[=SET] where SET labels the window as calibration or held-out
        path, _, setname = spec.partition("=")
        ref = load_ref(path)
        modelled, used = modelled_by_station(counts, layout, ref, a.id_prefix)
        measured = {s: v["count"] for s, v in ref["stations"].items() if v["count"] is not None}
        rep = grade_stations(modelled, measured)
        rep.update({
            "run": a.label, "set": setname or a.set_label or "unlabelled",
            "det_out": str(a.det_out), "det_out_sha256": sha256_file(a.det_out),
            "id_prefix": a.id_prefix, "layout": ("repaired" if a.id_prefix == FIXED_PREFIX
                                                 else "as-shipped"),
            "window_sumo": [a.begin, a.end], "window_s": a.end - a.begin,
            "n_intervals_used": n_int,
            "reference": {"path": ref["path"], "sha256": ref["sha256"]},
            "layout_map": str(a.layout_map),
            "loops_summed_per_station": dict(used),
            "n_stations_reference": len(measured),
            "licence": LICENCE_NOTE,
        })
        reports.append(rep)
        tag = f"{a.label or Path(a.det_out).name} | {Path(path).stem} | {rep['set']}"
        print(f"[{tag}] {rep['n_stations']} stations  "
              f"modelled {rep['total_modelled']:.0f} vs measured {rep['total_measured']:.0f}  "
              f"rel {100*rep['total_rel_error']:+.1f}%  medianGEH {rep['geh_median']}  "
              f"GEH<5 {rep['n_geh_lt_5']}/{rep['n_stations']}  "
              f"ratio {rep['ratio_min']}/{rep['ratio_median']}/{rep['ratio_max']}")
        for g in rep["gates"]:
            print(f"    {g['status'].upper():4s} {g['id']:42s} "
                  f"{g['value']} {g['cmp']} {g['threshold']}")
    if a.json_out:
        write_json(a.json_out, {"reports": reports}, a.i_know)
    return 0


def cmd_edges(a):
    """Edge-level diagnostic from the full-coverage detector set.

    Answers two questions the station GEH cannot: how much flow the model puts on the
    UNINSTRUMENTED car lanes of each detector edge (the reason a partly instrumented
    station's measured sum is only a lower bound), and how closely the simulated edge
    totals reproduce the counts routeSampler was asked to hit.
    """
    layout = json.load(open(a.layout_map, encoding="utf-8"))
    edges = layout["edges"]
    cov, _ = read_e1(a.cov_out, a.begin, a.end)
    fixed, _ = read_e1(a.fixed_out, a.begin, a.end)
    dets = layout["detectors"]
    inst_lanes = collections.defaultdict(set)
    for did, d in dets.items():
        if d["station"]:
            inst_lanes[d["edge"]].add(d["fixed_lane"])
    targets = {}
    if a.targets_meta:
        meta = json.load(open(a.targets_meta, encoding="utf-8"))
        for iv in meta["intervals"]:
            if float(iv["begin"]) == a.begin and float(iv["end"]) == a.end:
                targets = {e: v["target_cars"] for e, v in iv["per_edge"].items()}
    rows = []
    for edge, e in sorted(edges.items()):
        tot = sum(cov.get(COV_PREFIX + ln, 0.0) for ln in e["car_lane_ids"])
        inst = sum(cov.get(COV_PREFIX + ln, 0.0) for ln in inst_lanes[edge])
        modelled_loops = sum(fixed.get(FIXED_PREFIX + d, 0.0) for d in e["detectors"])
        rows.append({"edge": edge, "stations": e["stations"],
                     "n_car_lanes": e["n_car_lanes"], "n_loops": e["n_detectors"],
                     "fully_instrumented": e["fully_instrumented"],
                     "modelled_edge_total": tot, "modelled_on_instrumented_lanes": inst,
                     "lane_share_instrumented": round(inst / tot, 4) if tot else None,
                     "modelled_loop_sum": modelled_loops,
                     "target_cars": targets.get(edge),
                     "target_error": (round(modelled_loops - targets[edge], 1)
                                      if edge in targets else None)})
    part = [r for r in rows if not r["fully_instrumented"] and r["modelled_edge_total"] > 0]
    shares = [r["lane_share_instrumented"] for r in part if r["lane_share_instrumented"]]
    rep = {"window_sumo": [a.begin, a.end], "n_edges": len(rows),
           "n_partially_instrumented_with_flow": len(part),
           "lane_share_median": round(statistics.median(shares), 4) if shares else None,
           "lane_share_min": round(min(shares), 4) if shares else None,
           "modelled_total_all_detector_edges": round(sum(r["modelled_edge_total"] for r in rows), 1),
           "edges": rows}
    if targets:
        got = [r for r in rows if r["target_cars"]]
        rep["counting_edges"] = {
            "n": len(got),
            "target_total": sum(r["target_cars"] for r in got),
            "modelled_total": round(sum(r["modelled_loop_sum"] for r in got), 1),
            "rel_error": round((sum(r["modelled_loop_sum"] for r in got)
                                - sum(r["target_cars"] for r in got))
                               / max(sum(r["target_cars"] for r in got), 1), 4),
            "geh_median": round(statistics.median(
                [geh(r["modelled_loop_sum"], r["target_cars"]) for r in got]), 3),
            "n_geh_lt_5": sum(1 for r in got
                              if geh(r["modelled_loop_sum"], r["target_cars"]) < 5.0),
        }
    if a.json_out:
        write_json(a.json_out, rep, a.i_know)
    print(f"edges {rep['n_edges']}; partially instrumented with flow "
          f"{rep['n_partially_instrumented_with_flow']}; modelled share of edge flow on "
          f"instrumented lanes: median {rep['lane_share_median']} min {rep['lane_share_min']}")
    if targets:
        c = rep["counting_edges"]
        print(f"counting edges {c['n']}: modelled {c['modelled_total']:.0f} vs routeSampler "
              f"target {c['target_total']} ({100*c['rel_error']:+.1f}%), median GEH "
              f"{c['geh_median']}, GEH<5 at {c['n_geh_lt_5']}/{c['n']}")
    return 0


# --------------------------------------------------------------------------- report
def _mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.fmean(xs) if xs else None


def cmd_report(a):
    """Aggregate grade reports into the calibration-vs-held-out comparison.

    The GAP between the two sets is the whole point of the exercise: a demand set fitted to
    counting locations can always be made to reproduce the days it was fitted on, so only the
    held-out error says whether anything was learned.  A gap larger than the measured
    day-to-day spread of the reference itself is overfitting and is labelled as such here --
    it is never averaged away into a single headline.
    """
    reports = []
    for p in a.grade:
        d = json.load(open(p, encoding="utf-8"))
        reports += d["reports"] if "reports" in d else [d]
    if not reports:
        raise SystemExit("[usage] --grade matched no report")

    # noise floor: how much the reference itself moves between days in each set
    by_set = collections.defaultdict(list)
    for r in reports:
        by_set[(r["window_sumo"][0], r["set"])].append(r["total_measured"])
    noise = {}
    for (b, s), tots in by_set.items():
        u = sorted(set(tots))
        noise[(b, s)] = (statistics.pstdev(u) / statistics.fmean(u)) if len(u) > 1 else None

    groups = collections.defaultdict(lambda: collections.defaultdict(list))
    for r in reports:
        groups[(r.get("run"), r["layout"], r["window_sumo"][0])][r["set"]].append(r)

    rows = []
    print(f"{'run':28s} {'layout':10s} {'set':12s} {'n':>2s} {'rel%':>8s} {'medGEH':>7s} "
          f"{'GEH<5':>7s} {'inTol':>6s}")
    for key in sorted(groups, key=lambda k: (str(k[0]), k[1], k[2])):
        run, lay, beg = key
        row = {"run": run, "layout": lay, "sumo_begin": beg, "sets": {}}
        for s in sorted(groups[key]):
            ds = groups[key][s]
            g = {"n_windows": len(ds),
                 "windows": [Path(d["reference"]["path"]).stem for d in ds],
                 "rel_error_mean": round(_mean(d["total_rel_error"] for d in ds), 4),
                 "geh_median_mean": round(_mean(d["geh_median"] for d in ds), 3),
                 "geh_lt5_fraction_mean": round(
                     _mean(d["n_geh_lt_5"] / d["n_stations"] for d in ds), 4),
                 "in_tolerance_fraction_mean": round(_mean(
                     next(x["value"] for x in d["gates"]
                          if x["id"] == "geh.link_flow_tolerance_pass_fraction") for d in ds), 4),
                 "reference_day_to_day_cv": (round(noise[(beg, s)], 4)
                                             if noise.get((beg, s)) is not None else None)}
            row["sets"][s] = g
            print(f"{str(run):28s} {lay:10s} {s:12s} {g['n_windows']:2d} "
                  f"{100*g['rel_error_mean']:+8.1f} {g['geh_median_mean']:7.2f} "
                  f"{g['geh_lt5_fraction_mean']:7.3f} {g['in_tolerance_fraction_mean']:6.3f}")
        c, h = row["sets"].get("calibration"), row["sets"].get("held-out")
        if c and h:
            gap = abs(h["rel_error_mean"] - c["rel_error_mean"])
            floor = max(x for x in (c["reference_day_to_day_cv"] or 0.0,
                                    h["reference_day_to_day_cv"] or 0.0, 0.01))
            row["overfitting"] = {
                "rel_error_gap": round(gap, 4),
                "geh_median_gap": round(h["geh_median_mean"] - c["geh_median_mean"], 3),
                "reference_noise_floor": round(floor, 4),
                "verdict": ("gap within the reference's own day-to-day spread -- no evidence "
                            "of overfitting" if gap <= floor else
                            "GAP EXCEEDS the reference's day-to-day spread -- the fit does not "
                            "fully transfer to unseen days")}
            print(f"{'':28s} {lay:10s} {'GAP':12s}    "
                  f"{100*gap:+8.1f} {row['overfitting']['geh_median_gap']:+7.2f}"
                  f"   (noise floor {100*floor:.1f}%)  {row['overfitting']['verdict']}")
        rows.append(row)
    if a.out:
        write_json(a.out, {"groups": rows, "n_reports": len(reports),
                           "criteria_source": reports[0].get("criteria_source"),
                           "licence": LICENCE_NOTE}, a.i_know)
    return 0


# --------------------------------------------------------------------------- scenario
CALIB_CFG_NAME = "InTAS_calibrated.sumocfg"


def cmd_scenario(a):
    """Write an opt-in sumocfg that runs the calibrated demand instead of InTAS's own.

    The InTAS configs are left byte-identical, so the original demand stays the default and
    stays reproducible; only the new file selects the calibrated route set.
    """
    src = Path(a.sumocfg)
    txt = src.read_text(encoding="utf-8")
    new_routes = None
    if a.routes:
        routes = Path(a.routes)
        try:
            rel_routes = os.path.relpath(routes.resolve(), src.parent.resolve()).replace("\\", "/")
        except ValueError:
            rel_routes = str(routes.resolve()).replace("\\", "/")
        keep = [r.strip() for r in a.keep_routes.split(",") if r.strip()]
        new_routes = ",".join(keep + [rel_routes])
        txt = _cfg_set(txt, "route-files", new_routes, "input")
    if a.additional:
        adds = []
        for p in a.additional:
            pp = Path(p)
            try:
                adds.append(os.path.relpath(pp.resolve(), src.parent.resolve()).replace("\\", "/"))
            except ValueError:
                adds.append(str(pp.resolve()).replace("\\", "/"))
        txt = _cfg_set(txt, "additional-files", ",".join(adds), "input")
    out = src.parent / (a.name or CALIB_CFG_NAME)
    stem = out.stem
    for tag, val, sec in ((("summary-output", f"{stem}.summary.xml", "output"),
                           ("tripinfo-output", f"{stem}.tripinfo.xml", "output"),
                           ("statistic-output", f"{stem}.statistic.xml", "output"),
                           ("log", f"{stem}.log", "report"))):
        txt = _cfg_set(txt, tag, val, sec)
    out.write_text(txt, encoding="utf-8")
    print(f"wrote {out}\n  route-files = {new_routes or '(unchanged)'}")
    return 0


def _cfg_set(txt, tag, value, section):
    import re
    pat = re.compile(r"(<" + re.escape(tag) + r'\s*value\s*=\s*")([^"]*)(")')
    if pat.search(txt):
        return pat.sub(lambda m: m.group(1) + value + m.group(3), txt, count=1)
    close = f"</{section}>"
    if close in txt:
        return txt.replace(close, f'\t\t<{tag} value="{value}"/>\n\t{close}', 1)
    return txt.replace("</configuration>",
                       f'\t<{section}>\n\t\t<{tag} value="{value}"/>\n\t</{section}>\n'
                       "</configuration>", 1)


# --------------------------------------------------------------------------- main
def build_parser():
    p = argparse.ArgumentParser(
        prog="calibrate_demand.py",
        description="Calibrate InTAS demand against measured Ingolstadt loop counts "
                    "(SUMO routeSampler), with held-out validation.",
        epilog=LICENCE_NOTE)
    p.add_argument("--i-know", action="store_true",
                   help="permit writing count-derived output inside the tracked repo "
                        "(only with a written licence)")
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("layout", help="repair + extend the E1 detector layout")
    s.add_argument("--det-add", required=True)
    s.add_argument("--net", required=True)
    s.add_argument("--out-dir", required=True)
    s.set_defaults(fn=cmd_layout)

    s = sub.add_parser("candidates", help="extract a candidate route pool from InTAS routes")
    s.add_argument("--route-files", nargs="+", required=True)
    s.add_argument("--begin", type=float, required=True)
    s.add_argument("--end", type=float, required=True)
    s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_candidates)

    s = sub.add_parser("targets", help="build routeSampler edgeData count targets")
    s.add_argument("--layout-map", required=True)
    s.add_argument("--interval", action="append", required=True,
                   metavar="BEGIN:END=ref1.json[,ref2.json]",
                   help="one SUMO interval and the measured windows it is calibrated to "
                        "(several files are averaged)")
    s.add_argument("--bus-flows", default=None,
                   help="BusRoutes.flow.xml: scheduled bus passages are subtracted from "
                        "the car targets because buses cross the same loops")
    s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_targets)

    s = sub.add_parser("sample", help="run routeSampler")
    s.add_argument("--candidates", required=True)
    s.add_argument("--targets", required=True)
    s.add_argument("--out", required=True)
    s.add_argument("--begin", type=float, required=True)
    s.add_argument("--end", type=float, required=True)
    s.add_argument("--interval", type=float, default=3600.0)
    s.add_argument("--seed", type=int, default=42)
    s.add_argument("--prefix", default="cal")
    s.add_argument("--edgedata-attribute", default="entered")
    s.add_argument("--min-count", type=int, default=1)
    s.add_argument("--total-count", default=None)
    s.add_argument("--optimize", default=None, help="routeSampler --optimize (needs scipy)")
    s.add_argument("--minimize-vehicles", type=float, default=None)
    s.add_argument("--vtype-id", default="calib_car")
    s.add_argument("--vtype-attrs", default="")
    s.add_argument("--attributes", default='type="calib_car" departLane="best" departSpeed="max"')
    s.set_defaults(fn=cmd_sample)

    s = sub.add_parser("grade", help="GEH + FHWA gates for one E1 output against N windows")
    s.add_argument("--det-out", required=True)
    s.add_argument("--layout-map", required=True)
    s.add_argument("--ref", action="append", required=True, metavar="REF.json[=SET]",
                   help="reference window; SET labels it 'calibration' or 'held-out'")
    s.add_argument("--begin", type=float, required=True)
    s.add_argument("--end", type=float, required=True)
    s.add_argument("--id-prefix", default="", help="strip this from modelled detector ids "
                                                   "(e.g. fx_ for the repaired layout)")
    s.add_argument("--json", dest="json_out", default=None)
    s.add_argument("--label", default=None)
    s.add_argument("--set-label", default=None)
    s.set_defaults(fn=cmd_grade)

    s = sub.add_parser("edges", help="edge-level lane-share + routeSampler-target diagnostic")
    s.add_argument("--cov-out", required=True, help=f"E1 output of the {COV_PREFIX} detector set")
    s.add_argument("--fixed-out", required=True, help=f"E1 output of the {FIXED_PREFIX} set")
    s.add_argument("--layout-map", required=True)
    s.add_argument("--targets-meta", default=None, help="targets .meta.json for this window")
    s.add_argument("--begin", type=float, required=True)
    s.add_argument("--end", type=float, required=True)
    s.add_argument("--json", dest="json_out", default=None)
    s.set_defaults(fn=cmd_edges)

    s = sub.add_parser("report", help="calibration vs held-out summary over grade reports")
    s.add_argument("--grade", nargs="+", required=True)
    s.add_argument("--out", default=None)
    s.set_defaults(fn=cmd_report)

    s = sub.add_parser("scenario", help="write the opt-in calibrated sumocfg")
    s.add_argument("--sumocfg", required=True)
    s.add_argument("--routes", default=None,
                   help="calibrated route file; omit to keep the source cfg's own demand "
                        "(used to run the InTAS baseline with the repaired detector layout)")
    s.add_argument("--keep-routes", default="routes/ped.rou.xml,routes/BusRoutes.flow.xml")
    s.add_argument("--additional", nargs="*", default=None)
    s.add_argument("--name", default=CALIB_CFG_NAME)
    s.set_defaults(fn=cmd_scenario)
    return p


def main(argv=None):
    a = build_parser().parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
