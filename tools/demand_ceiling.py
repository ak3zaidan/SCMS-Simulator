#!/usr/bin/env python3
"""P2 -- where the calibrated demand goes between assignment and the loops.

routeSampler solves an assignment problem: it writes N vehicles whose routes, summed
over the 55 counting edges, reproduce the measured hourly counts (AM: 31,040 of 31,046).
SUMO then delivers far fewer passages at those same loops (AM: 24,071).  A flow count
cannot say why, because four completely different mechanisms subtract from it and they
have opposite fixes:

  1. the vehicle was never inserted            -- SUMO DISCARDED it (max-depart-delay)
  2. the vehicle was inserted late             -- SUMO DELAYED it (insertion capacity)
  3. the vehicle drove a DIFFERENT route       -- the rerouting device moved it off the
                                                  counting edge routeSampler put it on
  4. the vehicle had not got there yet         -- the passage happens after the horizon

This tool builds the exact per-vehicle ledger that separates them, from SUMO's own
``vehroute-output`` with ``--vehroute-output.exit-times``: for every counting-edge
passage routeSampler assigned to a graded-hour departure, it says which of the four
happened.  Every bucket is a count of real vehicles, not an inference from a total.

What it found (docs/realism/DEMAND-CALIBRATION.md sect. 12): mechanism 3.  Only 37.9 %
of the AM graded hour's assigned passages are driven as assigned; 44.0 % are lost
because SUMO's rerouting device (InTAS ships it at probability 0.82 with a 300 s period)
replaced the route of 75.0 % of the cohort.  Insertion is NOT the constraint -- 271
discards, median departure delay 0.05 s.  ``bottleneck`` then shows why the replacement
happens: the graded-hour assignment puts up to 1,778 veh/h on single give-way lanes that
pass a measured ~515, where the warm-up hour -- the one that meets its target -- puts
nothing above 1,200.

Usage
-----
    python tools/demand_ceiling.py ledger \
        --targets-meta .cache/calib/targets_am.edg.meta.json \
        --routes       .cache/calib/calibrated_am.rou.xml \
        --vehroute     .cache/calib/probe/y0AM_vehroute.xml \
        --begin 25200 --end 28800 \
        --json .cache/calib/probe/ledger_am.json
"""
from __future__ import annotations

import argparse
import collections
import json
import statistics
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


# ------------------------------------------------------------------ inputs
def load_targets(meta_path: Path, begin: float, end: float):
    """Counting edges and the per-edge target for one interval."""
    meta = json.load(open(meta_path, encoding="utf-8"))
    counting = set(meta["counting_edges"])
    per_edge, total = {}, None
    for iv in meta["intervals"]:
        if float(iv["begin"]) == begin and float(iv["end"]) == end:
            per_edge = {e: v["target_cars"] for e, v in iv["per_edge"].items()}
            total = iv["total_target_cars"]
    if not per_edge:
        raise SystemExit(f"no interval {begin}:{end} in {meta_path}")
    return counting, per_edge, total


def _iter_vehicles(path: Path):
    """Stream <vehicle> elements, freeing each one (files here are 30-170 MB)."""
    ctx = ET.iterparse(str(path), events=("start", "end"))
    _, root = next(ctx)
    for ev, el in ctx:
        if ev != "end" or el.tag != "vehicle":
            continue
        yield el
        el.clear()
        root.clear()


def load_assigned(rou_path: Path, counting: set[str]):
    """{vehicle id: (depart, Counter(counting edges on its ASSIGNED route))}."""
    out = {}
    for el in _iter_vehicles(rou_path):
        c = collections.Counter()
        r = el.find("route")
        if r is not None:
            for e in r.get("edges", "").split():
                if e in counting:
                    c[e] += 1
        out[el.get("id")] = (float(el.get("depart")), c)
    return out


def load_actual(vr_path: Path, counting: set[str], begin: float, end: float):
    """{vehicle id: dict} from vehroute-output written with --vehroute-output.exit-times.

    ``buckets``  Counter over (edge, bucket) of the counting-edge passages the vehicle
                 ACTUALLY made, bucket in 'before' | 'window' | 'after' | 'unreached'.
    ``reroutes`` number of route replacements SUMO's rerouting device performed.

    SUMO nests the superseded routes inside a ``<routeDistribution>`` when the rerouting
    device replaced the route, so the driven route is the LAST ``<route>`` anywhere under
    the vehicle -- the only one carrying ``exitTimes``.
    """
    out = {}
    for el in _iter_vehicles(vr_path):
        routes = el.findall(".//route")
        reroutes = sum(1 for r in routes if r.get("replacedOnEdge") is not None)
        final = routes[-1] if routes else None
        edges = final.get("edges", "").split() if final is not None else []
        ex = final.get("exitTimes", "").split() if final is not None else []
        b = collections.Counter()
        n_counting = 0
        for i, e in enumerate(edges):
            if e not in counting:
                continue
            n_counting += 1
            if i >= len(ex) or ex[i] in ("", "-1"):
                b[(e, "unreached")] += 1
                continue
            t = float(ex[i])
            b[(e, "before" if t < begin else "window" if t < end else "after")] += 1
        out[el.get("id")] = {
            "depart": float(el.get("depart", "nan")),
            "arrival": float(el.get("arrival")) if el.get("arrival") else None,
            "reroutes": reroutes,
            "n_edges": len(edges),
            "n_driven": len(ex),
            "n_counting": n_counting,
            "buckets": b,
        }
    return out


# ------------------------------------------------------------------ ledger
def cmd_ledger(a):
    counting, per_edge, target_total = load_targets(Path(a.targets_meta), a.begin, a.end)
    assigned = load_assigned(Path(a.routes), counting)
    actual = load_actual(Path(a.vehroute), counting, a.begin, a.end)

    cohort = {v: c for v, (dep, c) in assigned.items() if a.begin <= dep < a.end}
    warm = {v: c for v, (dep, c) in assigned.items() if dep < a.begin}

    assigned_passages = sum(sum(c.values()) for c in cohort.values())
    warm_assigned = sum(sum(c.values()) for c in warm.values())

    L = collections.Counter()          # the four buckets, in passages
    per_edge_L = collections.defaultdict(collections.Counter)
    undeparted, undeparted_pass = [], 0
    depart_delay = []
    reroute_veh = 0

    for vid, cnt in cohort.items():
        rec = actual.get(vid)
        if rec is None:                                   # never entered the network
            undeparted.append(vid)
            undeparted_pass += sum(cnt.values())
            for e, k in cnt.items():
                L["discarded"] += k
                per_edge_L[e]["discarded"] += k
            continue
        depart_delay.append(rec["depart"] - assigned[vid][0])
        if rec["reroutes"]:
            reroute_veh += 1
        got = rec["buckets"]
        for e, k in cnt.items():
            w = got[(e, "window")]
            deliv = min(k, w)
            rest = k - deliv
            aft = min(rest, got[(e, "after")])
            rest -= aft
            unr = min(rest, got[(e, "unreached")])
            rest -= unr
            bef = min(rest, got[(e, "before")])
            rest -= bef
            L["delivered"] += deliv
            L["after_horizon"] += aft
            L["not_reached"] += unr
            L["before_window"] += bef
            L["route_deviation"] += rest
            per_edge_L[e]["delivered"] += deliv
            per_edge_L[e]["after_horizon"] += aft
            per_edge_L[e]["not_reached"] += unr
            per_edge_L[e]["route_deviation"] += rest
        # passages this vehicle made on counting edges it was NOT assigned to
        for (e, b), k in got.items():
            if b == "window":
                extra = max(0, k - cnt.get(e, 0))
                if extra:
                    L["deviation_onto"] += extra
                    per_edge_L[e]["deviation_onto"] += extra

    # credit: in-window passages by vehicles that departed BEFORE the window
    spill = collections.Counter()
    for vid in warm:
        rec = actual.get(vid)
        if rec is None:
            continue
        for (e, b), k in rec["buckets"].items():
            if b == "window":
                spill[e] += k
    spill_in = sum(spill.values())

    total_in_window = 0
    for vid, rec in actual.items():
        total_in_window += sum(k for (e, b), k in rec["buckets"].items() if b == "window")

    rep = {
        "window_sumo": [a.begin, a.end],
        "counting_edges": len(counting),
        "routeSampler_target": target_total,
        "assigned_passages_by_cohort": assigned_passages,
        "warmup_assigned_passages": warm_assigned,
        "cohort_vehicles": len(cohort),
        "ledger_passages": {
            "delivered_in_window": L["delivered"],
            "after_horizon": L["after_horizon"],
            "not_reached_at_horizon": L["not_reached"],
            "route_deviation_off": L["route_deviation"],
            "discarded_never_inserted": L["discarded"],
            "before_window": L["before_window"],
        },
        "credits_passages": {
            "spillover_from_warmup": spill_in,
            "route_deviation_onto": L["deviation_onto"],
        },
        "total_counting_passages_in_window": total_in_window,
        "check_sum": (L["delivered"] + L["after_horizon"] + L["not_reached"]
                      + L["route_deviation"] + L["discarded"] + L["before_window"]),
        "vehicles": {
            "cohort_undeparted": len(undeparted),
            "cohort_undeparted_passages": undeparted_pass,
            "cohort_with_reroute": reroute_veh,
            "cohort_reroute_share": round(reroute_veh / max(len(cohort), 1), 4),
            "depart_delay_mean_s": round(statistics.fmean(depart_delay), 3) if depart_delay else None,
            "depart_delay_median_s": round(statistics.median(depart_delay), 3) if depart_delay else None,
            "depart_delay_p95_s": round(sorted(depart_delay)[int(0.95 * len(depart_delay))], 3) if depart_delay else None,
            "depart_delay_max_s": round(max(depart_delay), 3) if depart_delay else None,
            "depart_delay_over_300s": sum(1 for d in depart_delay if d > 300),
        },
        "per_edge": {e: dict(v) | {"target": per_edge.get(e)} for e, v in sorted(per_edge_L.items())},
    }
    if a.json_out:
        Path(a.json_out).parent.mkdir(parents=True, exist_ok=True)
        json.dump(rep, open(a.json_out, "w", encoding="utf-8"), indent=1)

    lp = rep["ledger_passages"]
    print(f"window {a.begin:.0f}-{a.end:.0f}  routeSampler target {target_total}  "
          f"assigned to cohort {assigned_passages}")
    print(f"  delivered in window        {lp['delivered_in_window']:7d}  "
          f"({100*lp['delivered_in_window']/max(assigned_passages,1):5.1f}%)")
    print(f"  passed AFTER the horizon   {lp['after_horizon']:7d}  "
          f"({100*lp['after_horizon']/max(assigned_passages,1):5.1f}%)")
    print(f"  NOT REACHED at horizon     {lp['not_reached_at_horizon']:7d}  "
          f"({100*lp['not_reached_at_horizon']/max(assigned_passages,1):5.1f}%)")
    print(f"  route deviation (off)      {lp['route_deviation_off']:7d}  "
          f"({100*lp['route_deviation_off']/max(assigned_passages,1):5.1f}%)")
    print(f"  DISCARDED, never inserted  {lp['discarded_never_inserted']:7d}  "
          f"({100*lp['discarded_never_inserted']/max(assigned_passages,1):5.1f}%)")
    print(f"  + spillover from warm-up   {rep['credits_passages']['spillover_from_warmup']:7d}")
    print(f"  + deviation ONTO           {rep['credits_passages']['route_deviation_onto']:7d}")
    print(f"  = counting passages in window {total_in_window}")
    v = rep["vehicles"]
    print(f"  vehicles: {len(cohort)} in cohort, {v['cohort_undeparted']} never inserted, "
          f"{v['cohort_with_reroute']} rerouted ({100*v['cohort_reroute_share']:.1f}%), "
          f"depart delay mean {v['depart_delay_mean_s']}s median {v['depart_delay_median_s']}s "
          f"p95 {v['depart_delay_p95_s']}s max {v['depart_delay_max_s']}s, "
          f">300s: {v['depart_delay_over_300s']}")
    return 0


# ------------------------------------------------------------------ undeparted
def cmd_undeparted(a):
    """Which loaded vehicles never entered the network, and why the count is what it is."""
    counting, _, _ = load_targets(Path(a.targets_meta), a.begin, a.end)
    assigned = load_assigned(Path(a.routes), counting)
    actual = load_actual(Path(a.vehroute), counting, a.begin, a.end)
    missing = [v for v in assigned if v not in actual]
    by_iv = collections.Counter()
    for v in missing:
        dep = assigned[v][0]
        by_iv[int(dep // 900) * 900] += 1
    print(f"loaded from route file: {len(assigned)}; present in vehroute: "
          f"{sum(1 for v in assigned if v in actual)}; MISSING: {len(missing)}")
    for k in sorted(by_iv):
        print(f"  depart bin {k}: {by_iv[k]}")
    print("  sample:", missing[:10])
    return 0


# ------------------------------------------------------------------ network production
def cmd_network(a):
    """Per-bin network production from ``edgeData``: is the WHOLE net saturating?

    A counting-edge shortfall says nothing on its own -- flow can be missing because the
    network is at capacity or because it went somewhere else.  Vehicle-kilometres per bin
    is the network's total production; if it is flat while demand rises, the network is
    the binding constraint.
    """
    counting = set()
    if a.targets_meta:
        counting, _, _ = load_targets(Path(a.targets_meta), a.window[0], a.window[1])
    bins = []
    ctx = ET.iterparse(str(a.edgedata), events=("start", "end"))
    _, root = next(ctx)
    cur = None
    for ev, el in ctx:
        if ev == "start" and el.tag == "interval":
            cur = {"begin": float(el.get("begin")), "end": float(el.get("end")),
                   "veh_m": 0.0, "veh_s": 0.0, "wait_s": 0.0, "loss_s": 0.0,
                   "entered": 0, "n_edges": 0, "n_jam": 0, "jam_veh_s": 0.0,
                   "worst": []}
            continue
        if ev != "end":
            continue
        if el.tag == "edge" and cur is not None:
            ss = float(el.get("sampledSeconds", 0.0))
            sp = float(el.get("speed", 0.0))
            sr = float(el.get("speedRelative", 1.0))
            cur["veh_m"] += ss * sp
            cur["veh_s"] += ss
            cur["wait_s"] += float(el.get("waitingTime", 0.0))
            cur["loss_s"] += float(el.get("timeLoss", 0.0))
            cur["entered"] += int(el.get("entered", 0))
            cur["n_edges"] += 1
            if sr < 0.2 and ss > 60:
                cur["n_jam"] += 1
                cur["jam_veh_s"] += ss
            cur["worst"].append((float(el.get("waitingTime", 0.0)), el.get("id"), sr,
                                 el.get("id") in counting))
            el.clear()
        elif el.tag == "interval":
            cur["worst"].sort(reverse=True)
            cur["worst"] = [{"edge": w[1], "waiting_s": round(w[0], 1),
                             "speed_rel": w[2], "is_counting_edge": w[3]}
                            for w in cur["worst"][:a.top]]
            bins.append(cur)
            cur = None
            root.clear()
    for b in bins:
        b["veh_km"] = round(b["veh_m"] / 1000.0, 1)
        b["mean_speed_mps"] = round(b["veh_m"] / b["veh_s"], 3) if b["veh_s"] else None
        b["veh_h"] = round(b["veh_s"] / 3600.0, 1)
        del b["veh_m"], b["veh_s"]
    rep = {"source": str(a.edgedata), "bins": bins}
    if a.json_out:
        Path(a.json_out).parent.mkdir(parents=True, exist_ok=True)
        json.dump(rep, open(a.json_out, "w", encoding="utf-8"), indent=1)
    print(f"{'bin':>8} {'veh-km':>10} {'veh-h':>9} {'mean m/s':>9} {'entered':>9} "
          f"{'jam edges':>10} {'wait veh-h':>11}")
    for b in bins:
        print(f"{b['begin']:8.0f} {b['veh_km']:10.1f} {b['veh_h']:9.1f} "
              f"{b['mean_speed_mps']:9.3f} {b['entered']:9d} {b['n_jam']:10d} "
              f"{b['wait_s']/3600:11.1f}")
    if a.top:
        print(f"\nworst {a.top} edges by waiting time in the last bin "
              f"(is_counting_edge in brackets):")
        for w in bins[-1]["worst"]:
            print(f"  {w['edge']:<28} waiting {w['waiting_s']:>9.1f}s  "
                  f"speedRel {w['speed_rel']:.3f}  [{w['is_counting_edge']}]")
    return 0


# ------------------------------------------------------------------ bottlenecks
def cmd_bottleneck(a):
    """Is routeSampler's ASSIGNMENT itself infeasible on some edges?

    Compares, per edge, the flow routeSampler's route set asks for in one hour against a
    generous saturation-flow bound (car lanes x 1800 veh/h/lane, which no signalised urban
    link reaches) and against what SUMO actually delivered.  An edge whose ASSIGNED load
    exceeds even the free-flow bound cannot be served by any network however configured.
    """
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from calibrate_demand import parse_net_lanes            # noqa: E402

    lanes = parse_net_lanes(a.net)
    n_car = {e: sum(1 for r in rows if r[2]) for e, rows in lanes.items()}

    demand = collections.Counter()
    for el in _iter_vehicles(Path(a.routes)):
        dep = float(el.get("depart"))
        if not (a.begin <= dep < a.end):
            continue
        r = el.find("route")
        if r is None:
            continue
        for e in r.get("edges", "").split():
            demand[e] += 1

    delivered = collections.Counter()
    speed, occ = {}, {}
    ctx = ET.iterparse(str(a.edgedata), events=("start", "end"))
    _, root = next(ctx)
    inside = False
    for ev, el in ctx:
        if ev == "start" and el.tag == "interval":
            inside = a.begin <= float(el.get("begin")) < a.end
            continue
        if ev != "end":
            continue
        if el.tag == "edge" and inside:
            eid = el.get("id")
            delivered[eid] += int(el.get("entered", 0))
            ss = float(el.get("sampledSeconds", 0.0))
            speed[eid] = speed.get(eid, 0.0) + ss * float(el.get("speed", 0.0))
            occ[eid] = occ.get(eid, 0.0) + ss
            el.clear()
        elif el.tag == "interval":
            root.clear()

    hours = (a.end - a.begin) / 3600.0
    # second pass: how much of the assignment has to cross a link it overloads
    over = {e for e, d in demand.items()
            if n_car.get(e) and d / hours / n_car[e] >= a.overload_vph}
    counting = set()
    if a.targets_meta:
        counting, _, _ = load_targets(Path(a.targets_meta), a.begin, a.end)
    veh_tot = veh_over = pass_tot = pass_over = 0
    for el in _iter_vehicles(Path(a.routes)):
        dep = float(el.get("depart"))
        if not (a.begin <= dep < a.end):
            continue
        r = el.find("route")
        if r is None:
            continue
        eds = r.get("edges", "").split()
        cp = sum(1 for e in eds if e in counting)
        veh_tot += 1
        pass_tot += cp
        if any(e in over for e in eds):
            veh_over += 1
            pass_over += cp

    rows = []
    for e, d in demand.items():
        nl = n_car.get(e, 0)
        cap_ff = nl * 1800 * hours
        rows.append({
            "edge": e, "car_lanes": nl,
            "assigned": d, "delivered_entered": delivered.get(e, 0),
            "assigned_per_lane_vph": round(d / hours / nl, 1) if nl else None,
            "load_vs_freeflow_cap": round(d / cap_ff, 3) if cap_ff else None,
            "mean_speed_mps": round(speed.get(e, 0.0) / occ[e], 2) if occ.get(e) else None,
        })
    over_ff = [r for r in rows if r["load_vs_freeflow_cap"] and r["load_vs_freeflow_cap"] > 1.0]
    over_sig = [r for r in rows if r["load_vs_freeflow_cap"] and r["load_vs_freeflow_cap"] > 0.5]
    rows.sort(key=lambda r: -(r["load_vs_freeflow_cap"] or 0))
    bands = collections.Counter()
    band_pass = collections.Counter()
    for r in rows:
        v = r["assigned_per_lane_vph"]
        if v is None:
            continue
        b = ("<450" if v < 450 else "450-700" if v < 700 else "700-900" if v < 900
             else "900-1200" if v < 1200 else ">=1200")
        bands[b] += 1
        band_pass[b] += r["assigned"]
    rep = {
        "window_sumo": [a.begin, a.end],
        "edges_with_assigned_flow": len(rows),
        "assigned_passages_total": sum(demand.values()),
        "edges_over_freeflow_capacity": len(over_ff),
        "edges_over_half_freeflow_capacity": len(over_sig),
        "assigned_on_overloaded_edges": sum(r["assigned"] for r in over_ff),
        "max_assigned_per_lane_vph": max((r["assigned_per_lane_vph"] or 0) for r in rows),
        "per_lane_load_bands_edges": dict(bands),
        "per_lane_load_bands_passages": dict(band_pass),
        "overload_threshold_vph_per_lane": a.overload_vph,
        "n_overloaded_edges": len(over),
        "cohort_vehicles": veh_tot,
        "cohort_vehicles_crossing_an_overloaded_edge": veh_over,
        "cohort_counting_passages": pass_tot,
        "counting_passages_on_routes_crossing_an_overloaded_edge": pass_over,
        "top": rows[:a.top],
    }
    if a.json_out:
        Path(a.json_out).parent.mkdir(parents=True, exist_ok=True)
        json.dump(rep, open(a.json_out, "w", encoding="utf-8"), indent=1)
    print(f"edges carrying assigned flow: {len(rows)}; assigned edge passages "
          f"{sum(demand.values())}")
    print(f"edges assigned MORE than car_lanes x 1800 veh/h (a bound no urban link "
          f"reaches): {len(over_ff)}")
    print(f"edges assigned more than HALF that: {len(over_sig)}")
    print("assigned load per car lane (veh/h): edges / passages by band")
    for b in ("<450", "450-700", "700-900", "900-1200", ">=1200"):
        print(f"   {b:>9}: {bands.get(b,0):5d} edges  {band_pass.get(b,0):8d} passages")
    print(f"   max {rep['max_assigned_per_lane_vph']} veh/h/lane")
    if veh_tot:
        print(f"{len(over)} edges assigned >= {a.overload_vph:.0f} veh/h/lane; "
              f"{veh_over}/{veh_tot} ({100*veh_over/veh_tot:.1f}%) of the cohort must cross "
              f"one, carrying {pass_over}/{pass_tot} "
              f"({100*pass_over/max(pass_tot,1):.1f}%) of the assigned counting passages")
    print(f"{'edge':<28}{'lanes':>6}{'assigned':>10}{'entered':>9}{'veh/h/ln':>10}"
          f"{'x cap':>8}{'m/s':>7}")
    for r in rows[:a.top]:
        print(f"{r['edge']:<28}{r['car_lanes']:>6}{r['assigned']:>10}"
              f"{r['delivered_entered']:>9}{r['assigned_per_lane_vph']:>10}"
              f"{r['load_vs_freeflow_cap']:>8}{str(r['mean_speed_mps']):>7}")
    return 0


def build_parser():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    for name, fn in (("ledger", cmd_ledger), ("undeparted", cmd_undeparted)):
        s = sub.add_parser(name)
        s.add_argument("--targets-meta", required=True)
        s.add_argument("--routes", required=True)
        s.add_argument("--vehroute", required=True)
        s.add_argument("--begin", type=float, required=True)
        s.add_argument("--end", type=float, required=True)
        s.add_argument("--json", dest="json_out")
        s.set_defaults(func=fn)
    s = sub.add_parser("bottleneck")
    s.add_argument("--net", required=True)
    s.add_argument("--routes", required=True)
    s.add_argument("--edgedata", required=True)
    s.add_argument("--targets-meta")
    s.add_argument("--begin", type=float, required=True)
    s.add_argument("--end", type=float, required=True)
    s.add_argument("--overload-vph", type=float, default=900.0,
                   help="per-lane assigned flow at or above which a signalised urban lane "
                        "is over capacity (HCM saturation ~1900 pc/h/lane of green x a "
                        "typical g/C of 0.45)")
    s.add_argument("--top", type=int, default=20)
    s.add_argument("--json", dest="json_out")
    s.set_defaults(func=cmd_bottleneck)
    s = sub.add_parser("network")
    s.add_argument("--edgedata", required=True)
    s.add_argument("--targets-meta")
    s.add_argument("--window", type=float, nargs=2, default=[25200.0, 28800.0])
    s.add_argument("--top", type=int, default=15)
    s.add_argument("--json", dest="json_out")
    s.set_defaults(func=cmd_network)
    return p


def main(argv=None):
    a = build_parser().parse_args(argv)
    return a.func(a)


if __name__ == "__main__":
    sys.exit(main())
