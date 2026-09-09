#!/usr/bin/env python3
"""Is the counting data servable by ANY route assignment on this network?

P2 established that SUMO does not drive the route set routeSampler solved for, and that
forcing it to (``--execute-assigned-routes``) makes every delivered-flow number worse.  Two
explanations survive that:

  (a) the ASSIGNMENT METHOD is at fault -- routeSampler solves a one-shot problem that
      ignores congestion, so an equilibrium method would produce a feasible route set; or
  (b) the COUNTING DATA is at fault -- no route-flow pattern whatever can carry the measured
      counts through this network, so every assignment method is doomed and the rerouting
      device is only the messenger.

A simulation cannot separate those, because a simulation only ever shows you one assignment.
This tool answers (b) directly and without simulating anything, by solving the linear
program routeSampler *would* have solved if it had been told about capacity:

    minimise    sum_e ( over_e + under_e )                      [veh/h of count mismatch]
    subject to  sum_r  A[e,r] x_r  -  over_e + under_e  =  c_e   for each counting edge e
                sum_r  B[f,r] x_r                       <= cap_f for every other edge f
                x >= 0,  over >= 0,  under >= 0

``x_r`` is the hourly flow on candidate route ``r``.  ``A`` and ``B`` count how many times a
route traverses an edge.  The route universe is routeSampler's OWN candidate pool, so the
answer is directly comparable: routeSampler chose ``x`` from exactly this set, and the only
thing this LP adds is the capacity constraint routeSampler never had.

Read the result like this:

  * mismatch ~ 0  -- the counts ARE servable within capacity.  routeSampler's route set is
    then infeasible for a reason the LP does not model (dynamics: queue spillback, signal
    coordination, insertion), and an equilibrium method is a reasonable thing to try.
  * mismatch large -- the counts are NOT servable by any assignment over this pool.  That is
    a structural property of the network-and-data pair, not of routeSampler, and no
    assignment method can fix it.

The LP is a STEADY-STATE RELAXATION and is therefore generous in every direction that
matters: it lets flow appear and vanish anywhere along a route, it ignores queue spillback,
it ignores junction conflicts between crossing streams, it ignores signal offsets and
insertion capacity, and it lets every vehicle be in the right place at the right time.  Its
optimum is an UPPER BOUND on the count match any dynamic assignment can reach.  A dynamic
method beating it is impossible; a dynamic method falling short of it is expected.

The capacity model is deliberately generous too, for the same reason -- see ``capacity``.

Answering (b) turns out to raise a sharper question, so the tool answers that too.  If the
counts ARE servable, why did routeSampler not serve them?  ``detour`` prices the assignment
against the congested shortest path in SUMO's own cost model, and ``lp --max-detour`` re-runs
the LP over only those routes a travel-time-minimising driver could plausibly be on.  Between
them they separate "the counts are impossible" from "the counts are possible but routeSampler
chose an expensive way to reproduce them" -- which is a fixable defect in an objective
function rather than a property of the city.

``lp --emit-routes`` then writes the LP's solution out as a SUMO route file, so the existence
result can be simulated instead of argued about.  See ``emit_routes`` for what that file is
and, more importantly, what it is not.

LICENCE.  The per-edge targets are Stadt Ingolstadt / SAVeNoW loop counts with no formally
stated licence.  Everything this tool writes is derived from them, so output is refused
outside the git-ignored cache roots unless ``--i-know`` is passed.  Publish the aggregate
verdict, never the per-edge numbers.

Usage
-----
    # is the counting data servable at all, and how much of the answer is the
    # generosity of the capacity model?
    python tools/assignment_feasibility.py lp \
        --net   scms-sim/scenarios/gen_intas_urban_low/sumo/ingolstadt.net.xml \
        --pool  .cache/calib/cand_am.rou.xml \
        --targets-meta .cache/calib/targets_am.edg.meta.json \
        --begin 25200 --end 28800 --minor-cap 515 \
        --json  .cache/calib/probe/feasibility_am_graded.json

    # what does reproducing the counts cost the driver?  (--shortest is the same
    # vehicles routed shortest-path on the same weights, by duarouter)
    python tools/assignment_feasibility.py detour \
        --net $NET --routes $ASSIGNED --shortest $SHORTEST \
        --edgedata .cache/calib/probe/y0AM_edgedata900_all.xml \
        --begin 25200 --end 28800 --json .cache/calib/probe/detour_am_graded.json

    # is there a count-matching solution that is ALSO near-shortest-path, and what
    # does SUMO do with it?
    python tools/assignment_feasibility.py lp ... --max-detour 0.15 \
        --edgedata $EDGEDATA --shortest $POOL_OD_SHORTEST \
        --emit-routes .cache/calib/probe/lp_graded.rou.xml
"""
from __future__ import annotations

import argparse
import collections
import json
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from calibrate_demand import (LICENCE_NOTE, guard_out, lane_allows_car,  # noqa: E402
                              sha256_file)

GREEN = set("Gg")


# --------------------------------------------------------------------------- network
def parse_net(net_path):
    """Return (lanes, tls, conns).

    lanes  {edge: [(index, allows_car)]}   non-internal edges only
    tls    {tlLogic id: (cycle_seconds, [green_seconds per link index])}
    conns  {(edge, fromLane): [(tl id, link index)]}
    """
    lanes, tls, conns = {}, {}, collections.defaultdict(list)
    for _, el in ET.iterparse(net_path, events=("end",)):
        if el.tag == "edge":
            if el.get("function") != "internal":
                lanes[el.get("id")] = [
                    (int(ln.get("index")),
                     lane_allows_car(ln.get("allow"), ln.get("disallow")))
                    for ln in el.findall("lane")]
            el.clear()
        elif el.tag == "tlLogic":
            phases = [(float(p.get("duration")), p.get("state"))
                      for p in el.findall("phase")]
            cycle = sum(d for d, _ in phases)
            n = max((len(s) for _, s in phases), default=0)
            green = [0.0] * n
            for d, s in phases:
                for i, ch in enumerate(s):
                    if ch in GREEN:
                        green[i] += d
            # a net may hold several programs per tl; keep the one with the longest cycle
            prev = tls.get(el.get("id"))
            if prev is None or cycle > prev[0]:
                tls[el.get("id")] = (cycle, green)
            el.clear()
        elif el.tag == "connection":
            tl = el.get("tl")
            if tl is not None and el.get("linkIndex") is not None:
                conns[(el.get("from"), int(el.get("fromLane")))].append(
                    (tl, int(el.get("linkIndex"))))
            el.clear()
        elif el.tag in ("junction", "type", "roundabout"):
            el.clear()
    return lanes, tls, conns


def capacity(lanes, tls, conns, sat_flow: float, minor_cap: float):
    """Per-edge hourly capacity, built to be GENEROUS.

    For every car-carrying lane:
      * if the lane's outgoing movements are signal-controlled, its capacity is
        ``sat_flow * g/C``, where ``g/C`` is the BEST green share any of that lane's
        movements gets.  Taking the best rather than a movement-weighted share overstates
        capacity on shared lanes, on purpose.
      * if they are not signal-controlled the lane gets ``sat_flow`` outright when
        ``minor_cap`` is 0 -- i.e. give-way and priority junctions are treated as free.  A
        real two-way-stop minor approach passes far less than a saturation flow, so this is
        a large overstatement, again on purpose.  ``--minor-cap`` replaces it with a fixed
        per-lane ceiling to test how much of the answer that generosity is buying.

    Overstating capacity can only make the counts look MORE servable.  Any infeasibility
    that survives this model is real.
    """
    cap = {}
    for edge, rows in lanes.items():
        total = 0.0
        for idx, is_car in rows:
            if not is_car:
                continue
            links = conns.get((edge, idx), [])
            if links:
                best = 0.0
                for tl, li in links:
                    cyc, green = tls.get(tl, (0.0, []))
                    if cyc > 0 and li < len(green):
                        best = max(best, green[li] / cyc)
                total += sat_flow * best if best > 0 else sat_flow
            else:
                total += sat_flow if minor_cap <= 0 else minor_cap
        cap[edge] = total
    return cap


# --------------------------------------------------------------------------- demand
def parse_pool(pool_path):
    """Distinct routes in the candidate pool -> list of edge tuples."""
    seen = {}
    order = []
    ctx = ET.iterparse(str(pool_path), events=("end",))
    for _, el in ctx:
        if el.tag != "route":
            if el.tag == "vehicle":
                el.clear()
            continue
        edges = tuple(el.get("edges", "").split())
        if edges and edges not in seen:
            seen[edges] = len(order)
            order.append(edges)
        el.clear()
    return order


def load_targets(meta_path, begin, end):
    meta = json.load(open(meta_path, encoding="utf-8"))
    for iv in meta["intervals"]:
        if float(iv["begin"]) == begin and float(iv["end"]) == end:
            return (set(meta["counting_edges"]),
                    {e: float(v["target_cars"]) for e, v in iv["per_edge"].items()},
                    float(iv["total_target_cars"]))
    raise SystemExit(f"no interval {begin}:{end} in {meta_path}")


# --------------------------------------------------------------------------- the LP
def emit_routes(a, routes, x, out_path):
    """Turn the LP's route flows into an actual SUMO route file.

    This exists to answer, constructively, the question the LP only answers in the
    abstract: a route-flow pattern that reproduces the counts within capacity AND stays
    near the shortest path is shown to EXIST -- so what does SUMO do when you hand it one?

    NOTE ON WHAT THIS IS AND IS NOT.  The demand LEVEL is untouched: the count targets are
    exactly the measured ones and are not adjusted.  What changes is only WHICH of the many
    count-matching route-flow patterns is chosen -- here, one restricted to routes a
    travel-time-minimising driver could plausibly be on.  This is a diagnostic, not a
    calibration, and it must never be reported as one.

    Departures are spread uniformly at random across the interval; departure attributes are
    copied from routeSampler (departLane="best" departSpeed="max") so insertion mechanics
    are held constant against every other arm.  Fractional flows are rounded
    probabilistically so the expected total is the LP total rather than a systematic
    floor.
    """
    import random
    rng = random.Random(a.seed)
    veh = []
    for r, f in zip(routes, x):
        if f <= 0:
            continue
        n = int(f) + (1 if rng.random() < (f - int(f)) else 0)
        for _ in range(n):
            veh.append((a.begin + rng.random() * (a.end - a.begin), r))
    veh.sort(key=lambda t: t[0])
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n')
        fh.write("<!-- LP route-flow solution: reproduces the measured counting-edge targets\n"
                 "     within link capacity, using only routes within "
                 f"{100*(a.max_detour or 0):.0f} % of the congested shortest path.\n"
                 "     Demand LEVEL is the measured counts, unchanged.  Diagnostic only. -->\n")
        fh.write('<routes xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" '
                 'xsi:noNamespaceSchemaLocation='
                 '"http://sumo.dlr.de/xsd/routes_file.xsd">\n')
        fh.write('    <vType id="calib_car" vClass="passenger"/>\n')
        for i, (dep, edges) in enumerate(veh):
            fh.write(f'    <vehicle id="lp{int(a.begin)}_{i}" depart="{dep:.2f}" '
                     f'type="calib_car" departLane="best" departSpeed="max">\n'
                     f'        <route edges="{" ".join(edges)}"/>\n    </vehicle>\n')
        fh.write("</routes>\n")
    print(f"wrote {out_path}: {len(veh)} vehicles")
    return len(veh)


def cmd_lp(a):
    import numpy as np
    from scipy.optimize import linprog
    from scipy.sparse import csr_matrix

    counting, target, target_total = load_targets(Path(a.targets_meta), a.begin, a.end)
    lanes, tls, conns = parse_net(a.net)
    cap = capacity(lanes, tls, conns, a.sat_flow, a.minor_cap)
    routes = parse_pool(Path(a.pool))
    hours = (a.end - a.begin) / 3600.0
    if abs(hours - 1.0) > 1e-9:
        print(f"[warn] window is {hours:.3f} h; targets are treated as flow over that window",
              file=sys.stderr)

    # keep only routes that touch at least one counting edge -- the rest carry no
    # information about the count constraint and only make the LP bigger.  (routeSampler
    # discards them too: "Ignored 6313 routes which do not pass any counting location".)
    routes = [r for r in routes if any(e in counting for e in r)]
    n_pool = len(routes)

    # Optional constraint: keep only routes close to the shortest path under the SAME
    # weights SUMO's rerouting device uses.  Read this as AGREEMENT WITH THE DEVICE, not
    # as driver plausibility -- on InTAS the assignment's routes are already within ~7 %
    # of free-flow shortest, so plausibility is not what is at stake.  What is at stake is
    # how much of the count can be reproduced using routes the device would leave alone,
    # and that is a very different (and much smaller) number once the network congests.
    detour_stats = None
    if a.max_detour is not None:
        if not (a.edgedata and a.shortest):
            raise SystemExit("--max-detour needs --edgedata and --shortest")
        bins = edge_traveltimes(Path(a.edgedata))
        ff = freeflow_times(a.net)

        def bin_at(t):
            lo, hi = 0, len(bins) - 1
            if not bins or t < bins[0][0]:
                return bins[0][2] if bins else {}
            while lo < hi:
                mid = (lo + hi + 1) // 2
                if bins[mid][0] <= t:
                    lo = mid
                else:
                    hi = mid - 1
            return bins[lo][2]

        def cost(edges, depart):
            t = depart
            for e in edges:
                w = bin_at(t).get(e)
                if w is None:
                    w = ff.get(e, 0.0)
                t += w
            return t - depart

        best = {}
        ctx = ET.iterparse(str(a.shortest), events=("end",))
        for _, el in ctx:
            if el.tag == "vehicle":
                r = el.find("route")
                if r is not None:
                    eds = r.get("edges", "").split()
                    if eds:
                        best[(eds[0], eds[-1])] = cost(eds, a.begin)
                el.clear()
        kept, dropped, no_ref = [], 0, 0
        for r in routes:
            b = best.get((r[0], r[-1]))
            if b is None or b <= 0:
                no_ref += 1
                kept.append(r)
                continue
            if cost(r, a.begin) <= (1.0 + a.max_detour) * b:
                kept.append(r)
            else:
                dropped += 1
        detour_stats = {"max_detour": a.max_detour, "pool_routes": n_pool,
                        "kept": len(kept), "dropped": dropped,
                        "no_shortest_reference": no_ref}
        print(f"detour filter <= {1+a.max_detour:.2f}x shortest: kept {len(kept)} of "
              f"{n_pool} pool routes ({dropped} dropped)")
        routes = kept
    n_r = len(routes)
    if n_r == 0:
        raise SystemExit("no candidate route touches a counting edge")

    c_edges = sorted(e for e in counting if e in target)
    c_index = {e: i for i, e in enumerate(c_edges)}
    n_c = len(c_edges)

    # every edge any surviving route uses, that has a finite capacity
    used = collections.Counter()
    for r in routes:
        for e in set(r):
            used[e] += 1
    f_edges = sorted(e for e in used if e in cap and cap[e] > 0)
    f_index = {e: i for i, e in enumerate(f_edges)}
    n_f = len(f_edges)

    # A_eq: counting-edge rows.  columns = [routes | over | under]
    rows, cols, vals = [], [], []
    for j, r in enumerate(routes):
        k = collections.Counter(e for e in r if e in c_index)
        for e, m in k.items():
            rows.append(c_index[e]); cols.append(j); vals.append(float(m))
    for i in range(n_c):
        rows.append(i); cols.append(n_r + i); vals.append(-1.0)          # over
        rows.append(i); cols.append(n_r + n_c + i); vals.append(1.0)     # under
    A_eq = csr_matrix((vals, (rows, cols)), shape=(n_c, n_r + 2 * n_c))
    b_eq = np.array([target[e] for e in c_edges], dtype=float)

    # A_ub: capacity rows
    rows, cols, vals = [], [], []
    for j, r in enumerate(routes):
        k = collections.Counter(e for e in r if e in f_index)
        for e, m in k.items():
            rows.append(f_index[e]); cols.append(j); vals.append(float(m))
    A_ub = csr_matrix((vals, (rows, cols)), shape=(n_f, n_r + 2 * n_c))
    b_ub = np.array([cap[e] * hours for e in f_edges], dtype=float)

    obj = np.zeros(n_r + 2 * n_c)
    obj[n_r:] = 1.0                                   # minimise total |mismatch|

    print(f"LP: {n_r} candidate routes, {n_c} count equalities, {n_f} capacity inequalities, "
          f"{A_eq.nnz + A_ub.nnz} nonzeros")
    res = linprog(obj, A_ub=A_ub, b_ub=b_ub, A_eq=A_eq, b_eq=b_eq,
                  bounds=(0, None), method="highs")
    if not res.success:
        # infeasible even with free slack should be impossible; report it rather than guess
        raise SystemExit(f"LP failed: {res.message}")

    x = res.x[:n_r]
    over = res.x[n_r:n_r + n_c]
    under = res.x[n_r + n_c:]
    modelled = A_eq[:, :n_r] @ x
    mismatch = float(over.sum() + under.sum())

    # which capacity rows are binding, and by how much the count constraint needs them
    load = A_ub[:, :n_r] @ x
    binding = [(f_edges[i], float(load[i]), float(b_ub[i]))
               for i in range(n_f) if b_ub[i] > 0 and load[i] >= 0.999 * b_ub[i]]
    binding.sort(key=lambda t: -t[1])

    served = float(modelled.sum())
    rep = {
        "window_sumo": [a.begin, a.end],
        "route_universe": "routeSampler candidate pool, routes touching a counting edge",
        "n_candidate_routes": n_r,
        "detour_filter": detour_stats,
        "n_counting_edges": n_c,
        "n_capacity_constrained_edges": n_f,
        "capacity_model": {
            "saturation_flow_veh_h_lane": a.sat_flow,
            "signalised_lane_capacity": "sat_flow * best g/C of that lane's movements",
            "unsignalised_lane_capacity":
                "sat_flow (no give-way penalty)" if a.minor_cap <= 0
                else f"{a.minor_cap} veh/h/lane",
            "note": "generous by construction; overstating capacity can only make the "
                    "counts look more servable",
        },
        "target_total_cars": target_total,
        "lp_served_total": round(served, 1),
        "lp_total_abs_mismatch": round(mismatch, 1),
        "lp_mismatch_fraction_of_target": round(mismatch / max(target_total, 1), 4),
        "lp_overflow_total": round(float(over.sum()), 1),
        "lp_underflow_total": round(float(under.sum()), 1),
        "counting_edges_not_fully_served": int((under > 0.5).sum()),
        "n_binding_capacity_edges": len(binding),
        "verdict": None,
        "licence": LICENCE_NOTE,
        "inputs": {
            "net": {"path": str(a.net), "sha256": sha256_file(a.net)},
            "pool": {"path": str(a.pool), "sha256": sha256_file(a.pool)},
            "targets_meta": {"path": str(a.targets_meta),
                             "sha256": sha256_file(a.targets_meta)},
        },
    }
    frac = rep["lp_mismatch_fraction_of_target"]
    # Deliberately no pass/fail threshold: the number that matters is how this static
    # ceiling compares with the deficit the SIMULATION shows.  A ceiling of 97 % against a
    # simulated delivery of 73 % says the counts are servable in steady state and the loss
    # is dynamic; a ceiling of 60 % would say the opposite.  The caller does that
    # comparison -- inventing a threshold here would only hide it.
    rep["served_fraction_of_target"] = round(served / max(target_total, 1), 4)
    rep["verdict"] = (
        f"a steady-state route-flow pattern over this candidate pool can serve "
        f"{100*served/max(target_total,1):.1f} % of the measured volume within this "
        f"capacity model ({100*frac:.2f} % unservable). Compare this CEILING with what the "
        f"simulation actually delivers: a large gap between them is dynamic loss "
        f"(queueing, spillback, signal coordination, insertion), which no assignment "
        f"method addresses; a small gap would mean the counts are structurally unservable.")
    if a.detail:
        rep["binding_edges"] = [
            {"edge": e, "lp_load": round(l, 1), "capacity": round(c, 1)}
            for e, l, c in binding[:a.top]]
        rep["underserved_counting_edges"] = sorted(
            ({"edge": c_edges[i], "target": b_eq[i], "lp_served": round(float(modelled[i]), 1)}
             for i in range(n_c) if under[i] > 0.5),
            key=lambda d: d["lp_served"] - d["target"])[:a.top]

    if a.emit_routes:
        n_veh = emit_routes(a, routes, x, guard_out(a.emit_routes, a.i_know))
        rep["emitted_vehicles"] = n_veh

    if a.json_out:
        p = guard_out(a.json_out, a.i_know)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(json.dumps(rep, indent=1), encoding="utf-8")
        print(f"wrote {p}")

    print(f"target total          {target_total:10.0f}")
    print(f"LP can serve          {served:10.1f}")
    print(f"total abs mismatch    {mismatch:10.1f}   "
          f"({100*frac:.2f} % of target)")
    print(f"  of which underflow  {float(under.sum()):10.1f}")
    print(f"  of which overflow   {float(over.sum()):10.1f}")
    print(f"counting edges short  {int((under > 0.5).sum()):10d} of {n_c}")
    print(f"binding capacity rows {len(binding):10d} of {n_f}")
    print(f"VERDICT: {rep['verdict']}")
    return 0


# --------------------------------------------------------------- congested edge costs
def edge_traveltimes(edgedata):
    """Per-bin measured travel time: [(begin, end, {edge: traveltime}), ...] in time order.

    Kept per bin, not averaged, because duarouter routed these trips with TIME-DEPENDENT
    weights: it advances a clock along the path and prices each edge with the bin the
    vehicle is in when it gets there.  Pricing with an hour average instead would compare
    the two route sets under a cost function neither of them was chosen under, and would
    produce ratios below 1 -- an assigned route apparently cheaper than the shortest path.
    """
    bins = []
    ctx = ET.iterparse(str(edgedata), events=("start", "end"))
    _, root = next(ctx)
    cur = None
    for ev, el in ctx:
        if ev == "start" and el.tag == "interval":
            cur = [float(el.get("begin")), float(el.get("end")), {}]
            continue
        if ev != "end":
            continue
        if el.tag == "edge" and cur is not None:
            tt = el.get("traveltime")
            if tt is not None:
                cur[2][el.get("id")] = float(tt)
            el.clear()
        elif el.tag == "interval":
            bins.append(tuple(cur))
            cur = None
            root.clear()
    bins.sort(key=lambda b: b[0])
    return bins


def freeflow_times(net_path):
    """length / speed per non-internal edge, as the fallback where nothing was measured."""
    ff = {}
    for _, el in ET.iterparse(net_path, events=("end",)):
        if el.tag == "edge":
            if el.get("function") != "internal":
                best = None
                for ln in el.findall("lane"):
                    v = float(ln.get("speed", "13.9"))
                    t = float(ln.get("length", "0")) / max(v, 0.1)
                    best = t if best is None else min(best, t)
                if best is not None:
                    ff[el.get("id")] = best
            el.clear()
        elif el.tag in ("junction", "connection", "tlLogic", "type", "roundabout"):
            el.clear()
    return ff


def cmd_detour(a):
    """How far off the congested shortest path is the count-matching assignment?

    routeSampler and SUMO's rerouting device optimise different things: routeSampler
    reproduces counts, the device minimises travel time.  This measures the size of that
    disagreement in the only unit that matters to the device -- travel-time cost, priced on
    the SAME measured congested weights the device itself sees.

    For every vehicle in the assignment it reports cost(assigned route) / cost(shortest path
    for the same origin and destination).  A ratio of 1.0 means the device has nothing to
    gain and will leave the route alone.  The distribution of that ratio is the honest
    statement of how much detour the measured counts require drivers to accept.
    """
    # Omitting --edgedata prices on FREE-FLOW times instead.  That control matters: the
    # congested weights contain real gridlock (edges at tens of thousands of times their
    # free-flow time), so a route that merely touches a jam scores as a hundredfold detour.
    # Pricing the same two route sets on an empty network separates "these are bad paths"
    # from "these are ordinary paths through corridors that happen to jam" -- and on InTAS
    # it is the second.  Both numbers are real; they answer different questions.
    bins = edge_traveltimes(Path(a.edgedata)) if a.edgedata else []
    ff = freeflow_times(a.net)
    miss = [0]

    def bin_at(t):
        lo, hi = 0, len(bins) - 1
        if not bins or t < bins[0][0]:
            return bins[0][2] if bins else {}
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if bins[mid][0] <= t:
                lo = mid
            else:
                hi = mid - 1
        return bins[lo][2]

    def cost(edges, depart):
        """duarouter's own cost model: walk the path, advancing the clock as you go."""
        t = depart
        for e in edges:
            w = bin_at(t).get(e)
            if w is None:
                w = ff.get(e)
                if w is None:
                    miss[0] += 1
                    continue
            t += w
        return t - depart

    def load(path):
        out = {}
        ctx = ET.iterparse(str(path), events=("end",))
        for _, el in ctx:
            if el.tag == "vehicle":
                r = el.find("route")
                if r is not None:
                    out[el.get("id")] = (float(el.get("depart")),
                                         tuple(r.get("edges", "").split()))
                el.clear()
        return out

    assigned = load(Path(a.routes))
    shortest = load(Path(a.shortest))
    rows = []
    for vid, (dep, edges) in assigned.items():
        if not (a.begin <= dep < a.end) or vid not in shortest:
            continue
        ca, cs = cost(edges, dep), cost(shortest[vid][1], dep)
        if cs > 0:
            rows.append((ca / cs, ca, cs, edges == shortest[vid][1]))
    if not rows:
        raise SystemExit("no vehicle in the window is present in both files")
    ratios = sorted(r[0] for r in rows)
    n = len(ratios)

    def q(p):
        return round(ratios[min(n - 1, int(p * n))], 4)

    identical = sum(1 for r in rows if r[3])
    over = lambda t: sum(1 for r in ratios if r > t)          # noqa: E731
    rep = {
        "window_sumo": [a.begin, a.end],
        "n_vehicles": n,
        "weights": str(a.edgedata) if a.edgedata else "free-flow (length / speed limit)",
        "edges_with_no_cost": miss[0],
        "cost_ratio_assigned_over_shortest": {
            "min": q(0.0), "p25": q(0.25), "median": q(0.5),
            "p75": q(0.75), "p90": q(0.90), "p99": q(0.99), "max": round(ratios[-1], 4),
            "mean": round(sum(ratios) / n, 4),
        },
        "share_on_the_shortest_path_exactly": round(identical / n, 4),
        "share_within_1pct_of_shortest": round(sum(1 for r in ratios if r <= 1.01) / n, 4),
        "share_within_10pct_of_shortest": round(sum(1 for r in ratios if r <= 1.10) / n, 4),
        "share_over_10pct_longer": round(over(1.10) / n, 4),
        "share_over_25pct_longer": round(over(1.25) / n, 4),
        "share_over_50pct_longer": round(over(1.50) / n, 4),
        "total_cost_assigned_s": round(sum(r[1] for r in rows), 1),
        "total_cost_shortest_s": round(sum(r[2] for r in rows), 1),
        "excess_cost_fraction": round(
            sum(r[1] for r in rows) / max(sum(r[2] for r in rows), 1e-9) - 1, 4),
        "licence": LICENCE_NOTE,
    }
    # The aggregate is a ratio of sums and a gridlocked edge can carry a four-figure
    # measured travel time, so a handful of vehicles can dominate it.  Report the same
    # figure with the extreme 1 % of ratios at each end dropped, and prefer the MEDIAN in
    # any headline: it is the only one of the three that no outlier can move.
    lo, hi = int(0.01 * n), int(0.99 * n)
    trimmed = sorted(rows, key=lambda r: r[0])[lo:hi] or rows
    rep["excess_cost_fraction_trimmed_1pct"] = round(
        sum(r[1] for r in trimmed) / max(sum(r[2] for r in trimmed), 1e-9) - 1, 4)
    rep["median_cost_ratio"] = rep["cost_ratio_assigned_over_shortest"]["median"]
    rep["verdict"] = (
        f"only {100*rep['share_within_1pct_of_shortest']:.1f} % of the assignment is within "
        f"1 % of the congested shortest path; for the MEDIAN vehicle the assigned route "
        f"costs {100*(rep['median_cost_ratio']-1):+.1f} % more travel time than the "
        f"alternative the rerouting device can see, and the device will take it. "
        f"Aggregate excess {100*rep['excess_cost_fraction']:+.1f} % "
        f"({100*rep['excess_cost_fraction_trimmed_1pct']:+.1f} % trimmed).")
    if a.json_out:
        p = guard_out(a.json_out, a.i_know)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(json.dumps(rep, indent=1), encoding="utf-8")
        print(f"wrote {p}")
    print(json.dumps(rep, indent=1))
    return 0


def build_parser():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter,
                                epilog=LICENCE_NOTE)
    p.add_argument("--i-know", action="store_true",
                   help="permit writing count-derived output inside the tracked repo")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("lp", help="capacity-constrained count-matching LP")
    s.add_argument("--net", required=True)
    s.add_argument("--pool", required=True, help="routeSampler candidate route pool")
    s.add_argument("--targets-meta", required=True)
    s.add_argument("--begin", type=float, required=True)
    s.add_argument("--end", type=float, required=True)
    s.add_argument("--sat-flow", type=float, default=1800.0,
                   help="saturation flow per car lane, veh/h of green (default 1800, which "
                        "no signalised urban lane actually reaches)")
    s.add_argument("--minor-cap", type=float, default=0.0,
                   help="per-lane ceiling for lanes with no signal control; 0 (default) "
                        "gives them the full saturation flow, i.e. no give-way penalty")
    s.add_argument("--detail", action="store_true",
                   help="include per-edge binding/underserved lists (count-derived)")
    s.add_argument("--top", type=int, default=25)
    s.add_argument("--max-detour", type=float, default=None,
                   help="keep only pool routes costing at most (1+X) times the congested "
                        "shortest path for their own OD pair; needs --edgedata/--shortest")
    s.add_argument("--edgedata", help="measured edgeData with traveltime, for --max-detour")
    s.add_argument("--shortest", help="the OD pairs routed shortest-path on those weights")
    s.add_argument("--emit-routes", help="write the LP solution as a SUMO route file "
                                         "(diagnostic; see emit_routes)")
    s.add_argument("--seed", type=int, default=42)
    s.add_argument("--json", dest="json_out")
    s.set_defaults(func=cmd_lp)

    s = sub.add_parser("detour", help="cost of the assignment vs the congested shortest path")
    s.add_argument("--net", required=True)
    s.add_argument("--routes", required=True, help="the ASSIGNED route file")
    s.add_argument("--shortest", required=True,
                   help="same vehicles routed by shortest path on the same weights")
    s.add_argument("--edgedata",
                   help="measured edgeData carrying traveltime, used as the cost. OMIT to "
                        "price on free-flow times instead -- the control that separates "
                        "'bad paths' from 'ordinary paths through corridors that jam'")
    s.add_argument("--begin", type=float, required=True)
    s.add_argument("--end", type=float, required=True)
    s.add_argument("--json", dest="json_out")
    s.set_defaults(func=cmd_detour)
    return p


def main(argv=None):
    a = build_parser().parse_args(argv)
    return a.func(a)


if __name__ == "__main__":
    sys.exit(main())
