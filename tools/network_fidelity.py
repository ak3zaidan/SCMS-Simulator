"""Network fidelity: score an engine road network against a SUMO ``.net.xml`` ground truth, and
attribute the ``traffic.overlap_events`` defect to the network layer.

WHY. The pure-Python engine's road model is an UNDIRECTED, laneless, straight-segment graph. Two
questions follow from that, and this tool answers both with numbers rather than argument:

  1. ``compare`` -- HOW FAR from reality is a given engine network? netconvert's ``.net.xml`` is the
     reference topology (it is what SUMO itself drives, and it resolves OSM into directed edges with
     real lane counts and real signal placement), so an engine network is scored against it on the
     roadmap's Phase-3 network-fidelity gate metrics: one-way share (percentage-point difference),
     node-degree distribution (two-sample KS), intersection count (relative error), plus lane-count
     distribution, total lane-km and signalised-node count.

     The two graphs are NEVER compared node-for-node. They are independent simplifications of the
     same square kilometre and have no shared node identity, so every metric here is DISTRIBUTIONAL
     or aggregate. Coordinates are used only for edge length when a candidate declares none.

  2. ``overlaps`` -- HOW MUCH of the measured overlap failure is the network's fault? The realism
     harness reports ``traffic.overlap_events`` = distinct vehicle pairs under 1 m apart at one
     instant. This mode replays a finished dataset's ground-truth emission trace against the network
     that produced it, snaps every vehicle to its nearest edge, derives its direction of travel, and
     splits the overlapping pairs by (same edge? / opposing?). A pair that is OPPOSING ON THE SAME
     EDGE is the shared-centreline defect and nothing else: give each direction its own carriageway
     and the two vehicles are half a road apart. A pair travelling the SAME way on one edge is a
     car-following minimum-gap defect that no amount of network work fixes. A pair on DIFFERENT
     edges is a junction conflict, which is a signal/priority question.

     ``--project-carriageways`` additionally re-projects the SAME trace into directed lane frames
     (each vehicle offset to its side of the centreline by half its own carriageway width) and
     recounts. That is a geometry-only prediction of the fix -- routes and car-following are NOT
     re-simulated -- so it bounds what the directed-edge work removes before any engine wiring lands.

READ-ONLY. Nothing here writes into a dataset or mutates a network; the only output is JSON/stdout.

CLI:
    python tools/network_fidelity.py compare --city ingolstadt --path both
    python tools/network_fidelity.py compare --candidate map.json --gt-net some.net.xml
    python tools/network_fidelity.py overlaps datasets/realism_baseline/python_flow_grid6_fulltrace
    python tools/network_fidelity.py overlaps <dir> --project-carriageways --json out.json
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_SRC = os.path.join(os.path.dirname(_HERE), "src")
if _SRC not in sys.path:                              # runnable straight from a checkout
    sys.path.insert(0, _SRC)

# --------------------------------------------------------------------------------------------
# Gate thresholds. The first three are the roadmap's Phase-3 "Network fidelity" gate verbatim
# (docs/realism/ROADMAP.md section 4, Phase 3 benchmark gate): "reproduces reference one-way share
# +/-5 pp and node-degree distribution KS <= 0.1 vs the netconvert net (ground truth by
# construction); intersection count within 10%". The rest have no roadmap anchor and are reported
# as INFORMATIONAL bands, never as pass/fail on their own.
# --------------------------------------------------------------------------------------------
GATES = {
    "oneway_share_pp":      {"limit": 5.0,  "severity": "hard"},
    "degree_ks":            {"limit": 0.10, "severity": "hard"},
    "intersection_rel_err": {"limit": 0.10, "severity": "hard"},
    "lane_ks":              {"limit": 0.10, "severity": "informational"},
    "lane_km_rel_err":      {"limit": 0.10, "severity": "informational"},
    "signal_rel_err":       {"limit": 0.10, "severity": "informational"},
}

OVERLAP_DIST_M = 1.0        # realism_bench.OVERLAP_DIST_M -- kept in sync deliberately, see _load
MAX_TIME_BUCKETS = 240      # realism_bench.MAX_TIME_BUCKETS
OPPOSING_DEG = 135.0        # heading difference above which two vehicles are head-on
SAMEDIR_DEG = 45.0          # ... and below which they are travelling together (run.py's own filter)


# ============================================================================================ #
# statistics
# ============================================================================================ #
def ks_two_sample(a, b) -> tuple[float, float]:
    """Two-sample Kolmogorov-Smirnov statistic D and its asymptotic p-value.

    D = max |F_a(x) - F_b(x)| over the pooled support. The p-value uses the standard
    Kolmogorov limiting distribution with Stephens' small-sample correction. On DISCRETE samples
    (node degrees, lane counts) the asymptotic p-value is conservative -- the statistic itself is
    exact and is what the gate reads, so the p-value is reported as context only.
    """
    xa = sorted(float(v) for v in a)
    xb = sorted(float(v) for v in b)
    na, nb = len(xa), len(xb)
    if na == 0 or nb == 0:
        return float("nan"), float("nan")
    i = j = 0
    d = 0.0
    while i < na and j < nb:
        x = min(xa[i], xb[j])
        while i < na and xa[i] <= x:
            i += 1
        while j < nb and xb[j] <= x:
            j += 1
        d = max(d, abs(i / na - j / nb))
    ne = math.sqrt(na * nb / float(na + nb))
    lam = (ne + 0.12 + 0.11 / ne) * d
    if lam < 0.02:            # the alternating series does not converge at lam ~ 0 (D == 0 -> p = 1)
        return d, 1.0
    p, sign, term = 0.0, 1.0, 0.0
    for k in range(1, 101):
        term = math.exp(-2.0 * (k * lam) ** 2)
        p += sign * term
        sign = -sign
        if term < 1e-12:
            break
    return d, max(0.0, min(1.0, 2.0 * p))


def _rel_err(cand: float, gt: float) -> float:
    """(candidate - ground truth) / ground truth. inf when the reference is zero and the
    candidate is not; 0.0 when both are zero (a matched absence is not an error)."""
    if gt == 0:
        return 0.0 if cand == 0 else float("inf")
    return (cand - gt) / float(gt)


def _hist(vals) -> dict:
    h: dict = {}
    for v in vals:
        h[int(v)] = h.get(int(v), 0) + 1
    return dict(sorted(h.items()))


# ============================================================================================ #
# summaries: one shape, produced from either a .net.xml or an engine network document
# ============================================================================================ #
def _summarise(source: str, n_nodes: int, undirected: set, directed: list, signal_nodes: int,
               length_source: str, extra: dict | None = None) -> dict:
    """The comparable shape.

    `undirected` is a set of (lo, hi) node-index pairs -- one entry per PHYSICAL road.
    `directed` is a list of (a, b, lanes, length_m) -- one entry per LEGAL DIRECTION of travel.
    Both are needed: degree/intersection metrics are properties of the physical road graph, while
    one-way share and lane-km are properties of the directed one.
    """
    deg: dict[int, int] = {}
    for a, b in undirected:
        deg[a] = deg.get(a, 0) + 1
        deg[b] = deg.get(b, 0) + 1
    degree_samples = [deg.get(i, 0) for i in range(n_nodes)]
    pairs = {(a, b) for a, b, _l, _m in directed}
    oneway_dirs = sum(1 for a, b in pairs if (b, a) not in pairs)
    oneway_roads = sum(1 for a, b in undirected
                       if ((a, b) in pairs) != ((b, a) in pairs))
    lane_samples = [int(l) for _a, _b, l, _m in directed]
    lane_km = sum(l * m for _a, _b, l, m in directed) / 1000.0
    edge_km = sum(m for _a, _b, _l, m in directed) / 1000.0
    out = {
        "source": source,
        "n_nodes": n_nodes,
        "n_roads_undirected": len(undirected),
        "n_directed_edges": len(directed),
        "oneway_directed_edges": oneway_dirs,
        "oneway_share": round(oneway_dirs / max(1, len(directed)), 4),
        "oneway_road_share": round(oneway_roads / max(1, len(undirected)), 4),
        "degree_histogram": _hist(degree_samples),
        "mean_degree": round(sum(degree_samples) / max(1, n_nodes), 4),
        "intersections_deg_ge3": sum(1 for d in degree_samples if d >= 3),
        "dead_ends": sum(1 for d in degree_samples if d == 1),
        "isolated_nodes": sum(1 for d in degree_samples if d == 0),
        "lane_histogram": _hist(lane_samples),
        "mean_lanes": round(sum(lane_samples) / max(1, len(lane_samples)), 4),
        "lane_km": round(lane_km, 3),
        "directed_edge_km": round(edge_km, 3),
        "n_signal_nodes": int(signal_nodes),
        "length_source": length_source,
        "_degree_samples": degree_samples,
        "_lane_samples": lane_samples,
    }
    if extra:
        out.update(extra)
    return out


def net_ground_truth(net_path: str, *, vclass: str | None = "passenger",
                     scope: str = "lcc", length: str = "centre") -> dict:
    """A SUMO ``.net.xml`` -> the reference summary. This is the ground truth by construction.

    Filtering is minimal and stated in the output: internal edges/junctions are dropped (they are
    junction interiors, not roads), edges that do not admit `vclass` are dropped (a car simulator's
    network is the car-drivable subgraph), and `scope="lcc"` keeps only the largest weakly connected
    component -- which is what ANY engine importer keeps, so leaving the clipped stubs in would
    penalise the candidate for something it is right to discard. `scope="all"` keeps everything.

    LENGTH CONVENTION -- this one matters and is easy to get wrong. ``edge.getLength()`` is SUMO's
    DRIVING length: junction to junction along the lane, EXCLUDING the junction interior (the
    internal edges carry that). The engine's junctions are dimensionless points, so its edges
    inherently run centre to centre and are systematically longer. Comparing the two conventions
    charges the candidate for a modelling difference rather than a fidelity gap -- MEASURED on
    Ingolstadt it inflates the lane-km error of a byte-faithful import to +19%. So the default here
    is ``length="centre"``: the from-junction coordinate, the edge's interior shape vertices, and the
    to-junction coordinate -- exactly what ``netimport`` writes and what the engine drives.
    ``length="lane"`` selects SUMO's own number; both are reported either way.

    Lane counts come from ``edge.getLaneNumber()`` and signalised nodes from junctions whose type is
    ``traffic_light*``.
    """
    from scms_sim_ref.mock_pipeline.netimport import read_net
    net = read_net(net_path)
    junctions = [n for n in net.getNodes() if n.getType() != "internal"]
    idx = {n.getID(): k for k, n in enumerate(sorted(junctions, key=lambda n: n.getID()))}
    edges = [e for e in net.getEdges()
             if e.getFunction() != "internal" and (vclass is None or e.allows(vclass))]
    recs: list = []                      # (a, b, lanes, centre_to_centre_m, sumo_lane_m)
    undirected: set = set()
    self_loops = 0
    for e in sorted(edges, key=lambda e: e.getID()):
        fn, tn = e.getFromNode(), e.getToNode()
        a, b = idx.get(fn.getID()), idx.get(tn.getID())
        if a is None or b is None:
            continue
        if a == b:
            self_loops += 1
            continue
        shape = [tuple(p[:2]) for p in e.getShape()]
        centre = _poly_len([tuple(fn.getCoord()[:2]), *shape[1:-1], tuple(tn.getCoord()[:2])])
        recs.append((a, b, int(e.getLaneNumber()), centre, float(e.getLength())))
        undirected.add((min(a, b), max(a, b)))
    keep = set(range(len(idx)))
    if scope == "lcc":
        adj: dict[int, set] = {}
        for a, b in undirected:
            adj.setdefault(a, set()).add(b)
            adj.setdefault(b, set()).add(a)
        best: set = set()
        seen: set = set()
        for s in sorted(adj):
            if s in seen:
                continue
            comp, stack = {s}, [s]
            seen.add(s)
            while stack:
                for m in adj[stack.pop()]:
                    if m not in seen:
                        seen.add(m)
                        comp.add(m)
                        stack.append(m)
            if len(comp) > len(best):
                best = comp
        keep = best
        undirected = {(a, b) for a, b in undirected if a in keep and b in keep}
        recs = [r for r in recs if r[0] in keep and r[1] in keep]
    remap = {old: new for new, old in enumerate(sorted(keep))}
    undirected = {(remap[a], remap[b]) for a, b in undirected}
    pick = 3 if length == "centre" else 4
    directed = [(remap[r[0]], remap[r[1]], r[2], r[pick]) for r in recs]
    inv = {v: k for k, v in idx.items()}
    sig = sum(1 for old in sorted(keep)
              if net.getNode(inv[old]).getType().startswith("traffic_light"))
    return _summarise(f"net.xml:{os.path.basename(net_path)}", len(keep), undirected, directed,
                      sig, ("junction-centre to junction-centre polyline" if length == "centre"
                            else "sumolib edge.getLength() (excludes junction interiors)"),
                      {"gt_scope": scope, "gt_vclass": vclass, "gt_length_convention": length,
                       "net_junctions_total": len(junctions),
                       "net_edges_drivable": len(edges), "self_loops": self_loops,
                       "lane_km_centre_to_centre": round(
                           sum(r[2] * r[3] for r in recs) / 1000.0, 3),
                       "lane_km_sumo_lane_length": round(
                           sum(r[2] * r[4] for r in recs) / 1000.0, 3),
                       "net_path": os.path.abspath(net_path)})


def _poly_len(pts) -> float:
    return sum(math.dist(p, q) for p, q in zip(pts, pts[1:]))


def document_summary(doc: dict, source: str = "document", *,
                     assume_lanes: int = 1) -> dict:
    """An engine custom-network document -> the same summary shape.

    Accepts every form the engine reads:
      * legacy ``{"nodes", "edges"}`` -- UNDIRECTED and laneless. That is the pre-fix model, and it
        is summarised exactly as the engine behaves: every road yields BOTH directions of travel
        (one-way share 0) with `assume_lanes` lanes each. Scoring it is the point -- it is the
        baseline the new topology is measured against.
      * ``directed_edges`` (osm.py --attrs / netimport.py, schema v2) -- one record per legal
        direction with its own lane count; this is what makes one-way share and the lane
        distribution real.
      * ``edges`` in the extended positional/object form (roads.parse_edge_spec: lanes, oneway,
        shape) -- e.g. anything written by ``CustomNetwork.document()``.

    Edge length uses ``length_m`` when present, else the ``shape`` polyline, else the straight-line
    node distance; the choice is reported in ``length_source``.
    """
    nodes = [[float(p[0]), float(p[1])] for p in doc["nodes"]]
    n = len(nodes)
    raw_edges = doc.get("edges") or []
    directed_recs = doc.get("directed_edges")
    undirected: set = set()
    lanes_decl: dict[tuple[int, int], tuple[int, int]] = {}
    oneway_decl: dict[tuple[int, int], int] = {}
    shapes: dict[tuple[int, int], list] = {}
    from scms_sim_ref.mock_pipeline.roads import parse_edge_spec
    for e in raw_edges:
        spec = parse_edge_spec(e, n)
        key = (min(spec["a"], spec["b"]), max(spec["a"], spec["b"]))
        undirected.add(key)
        fwd_is_low = spec["a"] < spec["b"]
        if spec["lanes_f"] is not None or spec["lanes_b"] is not None:
            lf, lb = spec["lanes_f"] or 0, spec["lanes_b"] or 0
            lanes_decl[key] = (lf, lb) if fwd_is_low else (lb, lf)
        if spec["oneway"]:
            d = spec["oneway"] if fwd_is_low else -spec["oneway"]
            oneway_decl[key] = d
        if spec["shape"]:
            sh = [list(p) for p in spec["shape"]]
            shapes[key] = sh if fwd_is_low else sh[::-1]

    def _len(a: int, b: int, declared=None) -> float:
        if declared is not None:
            return float(declared)
        key = (min(a, b), max(a, b))
        sh = shapes.get(key)
        if sh:
            return _poly_len([nodes[key[0]], *sh, nodes[key[1]]])
        return math.dist(nodes[a], nodes[b])

    directed: list = []
    if directed_recs:
        length_source = "directed_edges length_m / shape polyline"
        used_declared = 0
        for r in directed_recs:
            a, b = int(r["a"]), int(r["b"])
            if not (0 <= a < n and 0 <= b < n) or a == b:
                continue
            lanes = int(r.get("lanes") or assume_lanes)
            dl = r.get("length_m")
            if dl is not None:
                used_declared += 1
                m = float(dl)
            elif r.get("shape"):
                m = _poly_len([nodes[a], *[list(p) for p in r["shape"]], nodes[b]])
            else:
                m = _len(a, b)
            directed.append((a, b, max(1, lanes), m))
            undirected.add((min(a, b), max(a, b)))
        if not used_declared:
            length_source = "directed_edges shape polyline / straight-line node distance"
    else:
        # No directed layer. Expand the undirected graph the way the engine actually drives it.
        length_source = "edge shape polyline / straight-line node distance"
        for key in sorted(undirected):
            a, b = key
            ow = oneway_decl.get(key, 0)
            lf, lb = lanes_decl.get(key, (0, 0))
            m = _len(a, b)
            if ow >= 0:
                directed.append((a, b, max(1, lf or assume_lanes), m))
            if ow <= 0:
                directed.append((b, a, max(1, lb or assume_lanes), m))
    sig = len(doc.get("signal_nodes") or ())
    extra = {"has_directed_layer": bool(directed_recs),
             "has_signal_layer": "signal_nodes" in doc,
             "declared_lane_edges": len(lanes_decl) + sum(
                 1 for r in (directed_recs or ()) if r.get("lanes")),
             "shaped_edges": len(shapes) + sum(1 for r in (directed_recs or ()) if r.get("shape")),
             "assumed_lanes_per_direction": assume_lanes}
    if not doc.get("signal_nodes"):
        # The engine's actual behaviour with traffic_lights=True and no signal layer: EVERY node is
        # signalised. Reported so the signal metric is not silently flattered by a missing layer.
        extra["engine_default_signalised_nodes"] = n
    return _summarise(source, n, undirected, directed, sig, length_source, extra)


# ============================================================================================ #
# comparison
# ============================================================================================ #
def compare(cand: dict, gt: dict) -> dict:
    """Candidate summary vs ground-truth summary -> the fidelity metric block."""
    d_deg, p_deg = ks_two_sample(cand["_degree_samples"], gt["_degree_samples"])
    d_lane, p_lane = ks_two_sample(cand["_lane_samples"], gt["_lane_samples"])
    ow_pp = (cand["oneway_share"] - gt["oneway_share"]) * 100.0
    ow_road_pp = (cand["oneway_road_share"] - gt["oneway_road_share"]) * 100.0
    ix_err = _rel_err(cand["intersections_deg_ge3"], gt["intersections_deg_ge3"])
    lkm_err = _rel_err(cand["lane_km"], gt["lane_km"])
    sig_err = _rel_err(cand["n_signal_nodes"], gt["n_signal_nodes"])
    metrics = [
        {"id": "oneway_share_pp", "title": "One-way share (directed edges)",
         "candidate": cand["oneway_share"], "reference": gt["oneway_share"],
         "value": round(ow_pp, 3), "unit": "pp difference",
         "extra": {"oneway_road_share_pp": round(ow_road_pp, 3),
                   "candidate_oneway_roads": cand["oneway_road_share"],
                   "reference_oneway_roads": gt["oneway_road_share"]}},
        {"id": "degree_ks", "title": "Node-degree distribution (two-sample KS)",
         "candidate": cand["degree_histogram"], "reference": gt["degree_histogram"],
         "value": round(d_deg, 4), "unit": "KS D",
         "extra": {"p_value": round(p_deg, 6),
                   "candidate_mean_degree": cand["mean_degree"],
                   "reference_mean_degree": gt["mean_degree"],
                   "n_candidate": len(cand["_degree_samples"]),
                   "n_reference": len(gt["_degree_samples"])}},
        {"id": "intersection_rel_err", "title": "Intersections (degree >= 3) relative error",
         "candidate": cand["intersections_deg_ge3"], "reference": gt["intersections_deg_ge3"],
         "value": round(ix_err, 4), "unit": "relative error",
         "extra": {"candidate_nodes": cand["n_nodes"], "reference_nodes": gt["n_nodes"],
                   "candidate_dead_ends": cand["dead_ends"],
                   "reference_dead_ends": gt["dead_ends"]}},
        {"id": "lane_ks", "title": "Lane-count distribution per direction (two-sample KS)",
         "candidate": cand["lane_histogram"], "reference": gt["lane_histogram"],
         "value": round(d_lane, 4), "unit": "KS D",
         "extra": {"p_value": round(p_lane, 6), "candidate_mean_lanes": cand["mean_lanes"],
                   "reference_mean_lanes": gt["mean_lanes"]}},
        {"id": "lane_km_rel_err", "title": "Total lane-km relative error",
         "candidate": cand["lane_km"], "reference": gt["lane_km"],
         "value": round(lkm_err, 4), "unit": "relative error",
         "extra": {"candidate_edge_km": cand["directed_edge_km"],
                   "reference_edge_km": gt["directed_edge_km"],
                   "edge_km_rel_err": round(_rel_err(cand["directed_edge_km"],
                                                     gt["directed_edge_km"]), 4)}},
        {"id": "signal_rel_err", "title": "Signalised nodes relative error",
         "candidate": cand["n_signal_nodes"], "reference": gt["n_signal_nodes"],
         "value": round(sig_err, 4), "unit": "relative error",
         "extra": {"engine_default_signalised_nodes":
                   cand.get("engine_default_signalised_nodes")}},
    ]
    for m in metrics:
        g = GATES[m["id"]]
        m["limit"] = g["limit"]
        m["severity"] = g["severity"]
        v = m["value"]
        m["status"] = ("na" if (isinstance(v, float) and math.isnan(v))
                       else "pass" if abs(v) <= g["limit"] else "fail")
    hard = [m["id"] for m in metrics if m["severity"] == "hard" and m["status"] == "fail"]
    return {"metrics": metrics,
            "summary": {"pass": sum(1 for m in metrics if m["status"] == "pass"),
                        "fail": sum(1 for m in metrics if m["status"] == "fail"),
                        "na": sum(1 for m in metrics if m["status"] == "na"),
                        "hard_failures": hard},
            "candidate": {k: v for k, v in cand.items() if not k.startswith("_")},
            "reference": {k: v for k, v in gt.items() if not k.startswith("_")}}


def _fmt_compare(res: dict) -> str:
    def _shape(s):
        return (f"{s['n_nodes']} nodes / {s['n_roads_undirected']} roads / "
                f"{s['n_directed_edges']} directed / {s['directed_edge_km']} centreline-km")
    rows = ["", f"  candidate : {res['candidate']['source']}",
            f"              {_shape(res['candidate'])}",
            f"  reference : {res['reference']['source']}",
            f"              {_shape(res['reference'])}", "",
            f"  {'metric':<34} {'cand':>12} {'ref':>12} {'value':>10} {'limit':>7}  status"]
    rows.append("  " + "-" * 88)
    for m in res["metrics"]:
        c = m["candidate"] if not isinstance(m["candidate"], dict) else "hist"
        r = m["reference"] if not isinstance(m["reference"], dict) else "hist"
        rows.append(f"  {m['id']:<34} {str(c):>12} {str(r):>12} {m['value']:>10} "
                    f"{m['limit']:>7}  {m['status'].upper()}"
                    f"{'' if m['severity'] == 'hard' else '  (informational)'}")
    s = res["summary"]
    rows.append(f"  -> {s['pass']} pass / {s['fail']} fail / {s['na']} n-a; "
                f"hard failures: {s['hard_failures'] or 'none'}")
    return "\n".join(rows)


# ============================================================================================ #
# overlap attribution
# ============================================================================================ #
def _network_from_config(cfg: dict):
    """The engine network a finished run used, rebuilt from its manifest config."""
    from scms_sim_ref.mock_pipeline import roads as R
    kind = cfg.get("road_network", "linear")
    if kind == "grid":
        return R.GridNetwork(int(cfg["grid_w"]), int(cfg["grid_h"]), float(cfg["grid_block_m"]),
                             float(cfg.get("grid_dropout") or 0.0), int(cfg.get("seed") or 0),
                             int(cfg.get("arterial_every") or 0),
                             float(cfg.get("arterial_speed_mps") or 0.0),
                             float(cfg.get("local_speed_mps") or 0.0))
    if kind == "ring":
        return R.RingNetwork(int(cfg.get("grid_w") or 12), float(cfg["grid_block_m"]))
    if kind == "spider":
        return R.CustomNetwork(*R.spider_graph(int(cfg.get("grid_w") or 8),
                                               int(cfg.get("grid_h") or 3),
                                               float(cfg["grid_block_m"])))
    if kind == "custom":
        raw = cfg.get("custom_network")
        doc = raw if isinstance(raw, dict) else json.loads(raw)
        return R.CustomNetwork(doc["nodes"], doc["edges"])
    raise ValueError(f"road_network={kind!r} has no map to attribute overlaps to "
                     f"('linear' is straight infinite lines, not a network)")


def _segments(net) -> list:
    """[(edge_key, ax, ay, bx, by), ...] -- every straight sub-segment of every road.

    `edge_key` is the UNDIRECTED road identity (a shaped edge contributes several sub-segments that
    all share one key), so "same edge" means "same physical road", not "same straight piece".
    """
    geo = net.geometry()
    pts = [(float(p[0]), float(p[1])) for p in geo["nodes"]]
    out = []
    attrs = geo.get("edge_attrs") or [None] * len(geo["edges"])
    for e, at in zip(geo["edges"], attrs):
        a, b = int(e[0]), int(e[1])
        key = (min(a, b), max(a, b))
        chain = [pts[key[0]]]
        if at and at.get("shape"):
            chain += [(float(p[0]), float(p[1])) for p in at["shape"]]
        chain.append(pts[key[1]])
        for p, q in zip(chain, chain[1:]):
            out.append((key, p[0], p[1], q[0], q[1]))
    return out, pts


def _snap(x: float, y: float, segs: list, u=None, eps: float = 0.05) -> tuple:
    """Nearest road sub-segment. Returns (edge_key, distance_m, along_unit_vector).

    AT A JUNCTION the nearest edge is ambiguous -- a vehicle standing on the node point is 0 m from
    all four arms -- and picking the wrong one turns a head-on pair into a phantom crossing. So when
    several segments are within `eps` of the minimum distance, the vehicle's own direction of travel
    `u` decides: it is on the road it is DRIVING along. Fully deterministic; with `u` unavailable or
    no tie, it falls back to the lowest edge key then the lowest segment index.
    """
    cands: list = []
    dmin = math.inf
    for i, (key, ax, ay, bx, by) in enumerate(segs):
        dx, dy = bx - ax, by - ay
        L2 = dx * dx + dy * dy
        t = 0.0 if L2 <= 0 else max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / L2))
        d = math.hypot(x - (ax + t * dx), y - (ay + t * dy))
        if d > dmin + eps:
            continue
        dmin = min(dmin, d)
        L = math.sqrt(L2) or 1.0
        cands.append((d, key, i, (dx / L, dy / L)))
    near = [c for c in cands if c[0] <= dmin + eps]
    if u is None:
        best = min(near, key=lambda c: (round(c[0], 9), c[1], c[2]))
    else:                                          # most aligned with the direction of travel
        best = min(near, key=lambda c: (-abs(u[0] * c[3][0] + u[1] * c[3][1]),
                                        round(c[0], 9), c[1], c[2]))
    return best[1], best[0], best[3]


def _collinear(u, v, tol_deg: float) -> bool:
    """True if two road directions are the same LINE (direction ignored) within `tol_deg`."""
    dot = abs(max(-1.0, min(1.0, u[0] * v[0] + u[1] * v[1])))
    return math.degrees(math.acos(dot)) <= tol_deg


def _headings(tracks: dict, max_dt: float = 10.0) -> dict:
    """(vid, t) -> unit direction of travel, from the trace's own displacement.

    A central difference over the neighbouring samples where they exist, else a one-sided one. A
    vehicle stopped at a red light has no instantaneous displacement, so the search walks outward in
    time (up to `max_dt`) to the nearest step in which it actually moved: that is still the
    direction it is FACING on its edge, which is what an opposing/same-direction split needs.
    Returns nothing for a vehicle that never moved.
    """
    out: dict = {}
    for vid, samples in tracks.items():
        ts = sorted(samples)
        pos = [samples[t] for t in ts]
        n = len(ts)
        for i, t in enumerate(ts):
            v = None
            for span in range(1, n):
                lo, hi = max(0, i - span), min(n - 1, i + span)
                if ts[hi] - ts[lo] > max_dt and span > 1:
                    break
                dx = pos[hi][0] - pos[lo][0]
                dy = pos[hi][1] - pos[lo][1]
                if math.hypot(dx, dy) >= 0.05:
                    L = math.hypot(dx, dy)
                    v = (dx / L, dy / L)
                    break
                if lo == 0 and hi == n - 1:
                    break
            if v is not None:
                out[(vid, t)] = v
    return out


def attribute_overlaps(dataset_dir: str, *, overlap_m: float = OVERLAP_DIST_M,
                       max_instants: int = MAX_TIME_BUCKETS, all_instants: bool = False,
                       junction_r_m: float = 5.0, project: bool = False,
                       lane_width_m: float | None = None, lanes_per_dir: int = 1,
                       drive_side: str = "right", collinear_deg: float = 20.0) -> dict:
    """Replay a finished dataset's ground-truth trace against its own network and split the
    overlapping vehicle pairs by network cause.

    The instant selection reproduces ``realism_bench._overlap_events`` exactly (timestamps rounded
    to 3 dp, instants with >= 2 vehicles, deterministic even-spaced sub-sampling to
    `max_instants`), so the total here is the harness's ``traffic.overlap_events`` number and not a
    near-miss of it. `all_instants=True` additionally reports the un-sub-sampled total.

    With `project=True` the same trace is re-projected into directed lane frames -- each vehicle
    moved to its side of its edge's centreline by ``0.5 * lanes_per_dir * lane_width_m`` in its own
    direction of travel -- and the overlaps recounted. Nothing is re-simulated: this isolates the
    GEOMETRIC effect of separating the carriageways, which is the part the network layer owns.
    """
    with open(os.path.join(dataset_dir, "manifest.json"), encoding="utf-8") as fh:
        manifest = json.load(fh)
    cfg = manifest.get("config") or {}
    net = _network_from_config(cfg)
    segs, node_pts = _segments(net)
    if lane_width_m is None:
        lane_width_m = float(cfg.get("lane_width_m") or 3.5)
    side = -1.0 if drive_side == "right" else 1.0

    path = os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")
    inst: dict = {}
    tracks: dict = {}
    heading_field = 0
    total_rows = 0
    with open(path, encoding="utf-8") as fh:
        for ln in fh:
            if not ln.strip():
                continue
            e = json.loads(ln)
            try:
                t = round(float(e["t"]), 3)
                vid = str(e["true_vehicle_id"])
                x, y = float(e["true_x"]), float(e["true_y"])
            except (KeyError, TypeError, ValueError):
                continue
            total_rows += 1
            inst.setdefault(t, {})[vid] = (x, y)
            tracks.setdefault(vid, {})[t] = (x, y)
            if e.get("true_heading") is not None:
                heading_field += 1

    keys_all = [t for t in sorted(inst) if len(inst[t]) >= 2]
    keys = list(keys_all)
    if len(keys) > max_instants:
        step = len(keys) / float(max_instants)
        keys = [keys[int(i * step)] for i in range(max_instants)]

    hdg = _headings(tracks)
    # heading source: displacement always (true_heading is absent in datasets written before ADR
    # 0002 and is NOT needed -- direction of travel along an edge is exactly what displacement gives)
    snap_cache: dict = {}

    def snapped(x, y, u):
        k = (round(x, 3), round(y, 3), None if u is None else (round(u[0], 3), round(u[1], 3)))
        v = snap_cache.get(k)
        if v is None:
            v = _snap(x, y, segs, u)
            snap_cache[k] = v
        return v

    def classify(keyset):
        buckets: dict = {}
        detail = {"pairs": 0, "unknown_heading": 0, "offroad_gt_5m": 0,
                  "at_junction_both": 0, "same_street_across_junction": 0}
        dist_to_edge = []
        for t in keyset:
            vids = sorted(inst[t])
            pts = [inst[t][v] for v in vids]
            for i in range(len(vids)):
                for j in range(i + 1, len(vids)):
                    if math.dist(pts[i], pts[j]) >= overlap_m:
                        continue
                    detail["pairs"] += 1
                    hi = hdg.get((vids[i], t))
                    hj = hdg.get((vids[j], t))
                    ki, di, _ui = snapped(pts[i][0], pts[i][1], hi)
                    kj, dj, _uj = snapped(pts[j][0], pts[j][1], hj)
                    dist_to_edge += [di, dj]
                    if max(di, dj) > 5.0:
                        detail["offroad_gt_5m"] += 1
                    nd_i = min(math.dist(pts[i], p) for p in node_pts)
                    nd_j = min(math.dist(pts[j], p) for p in node_pts)
                    if nd_i <= junction_r_m and nd_j <= junction_r_m:
                        detail["at_junction_both"] += 1
                    if ki == kj:
                        where = "same_edge"
                    elif set(ki) & set(kj) and _collinear(_ui, _uj, collinear_deg):
                        # two blocks of ONE street either side of a shared junction: the vehicles
                        # are on the same carriageway line, so this is the same shared-centreline
                        # defect as `same_edge` -- it only looks different because the pair straddles
                        # an intersection.
                        where = "same_street_across_junction"
                        detail["same_street_across_junction"] += 1
                    else:
                        where = "different_streets"
                    if hi is None or hj is None:
                        detail["unknown_heading"] += 1
                        rel = "unknown"
                    else:
                        dot = max(-1.0, min(1.0, hi[0] * hj[0] + hi[1] * hj[1]))
                        ang = math.degrees(math.acos(dot))
                        rel = ("opposing" if ang > OPPOSING_DEG else
                               "same_direction" if ang < SAMEDIR_DEG else "crossing")
                    b = where + "/" + rel
                    buckets[b] = buckets.get(b, 0) + 1
        detail["mean_dist_to_nearest_edge_m"] = (
            round(sum(dist_to_edge) / len(dist_to_edge), 3) if dist_to_edge else None)
        detail["max_dist_to_nearest_edge_m"] = round(max(dist_to_edge), 3) if dist_to_edge else None
        return dict(sorted(buckets.items())), detail

    buckets, detail = classify(keys)
    total = detail["pairs"]
    opposing_same = (buckets.get("same_edge/opposing", 0)
                     + buckets.get("same_street_across_junction/opposing", 0))
    out = {
        "dataset_dir": os.path.abspath(dataset_dir),
        "network": {"kind": cfg.get("road_network"), "grid_w": cfg.get("grid_w"),
                    "grid_h": cfg.get("grid_h"), "block_m": cfg.get("grid_block_m"),
                    "n_lanes_cfg": cfg.get("n_lanes"), "lane_width_m": lane_width_m,
                    "nodes": len(node_pts), "road_sub_segments": len(segs)},
        "trace": {"rows": total_rows, "vehicles": len(tracks),
                  "instants_with_2plus": len(keys_all), "instants_examined": len(keys),
                  "sub_sampled": len(keys_all) > max_instants,
                  "rows_with_true_heading": heading_field,
                  "heading_source": "trace displacement (central difference, nearest moving step)"},
        "overlap_events": total,
        "buckets": buckets,
        "detail": detail,
        "attribution": {
            "shared_centreline_opposing_same_carriageway": opposing_same,
            "shared_centreline_share": round(opposing_same / total, 4) if total else None,
            "of_which_same_edge": buckets.get("same_edge/opposing", 0),
            "of_which_across_a_junction":
                buckets.get("same_street_across_junction/opposing", 0),
            "car_following_same_direction":
                sum(v for k, v in buckets.items() if k.endswith("/same_direction")),
            "junction_conflict_different_streets":
                sum(v for k, v in buckets.items() if k.startswith("different_streets/")),
            "crossing_or_unknown_on_one_street":
                sum(v for k, v in buckets.items()
                    if not k.startswith("different_streets/")
                    and (k.endswith("/crossing") or k.endswith("/unknown"))),
        },
        "settings": {"overlap_dist_m": overlap_m, "max_instants": max_instants,
                     "opposing_deg": OPPOSING_DEG, "same_dir_deg": SAMEDIR_DEG,
                     "junction_radius_m": junction_r_m, "collinear_deg": collinear_deg},
    }
    if all_instants and len(keys_all) > len(keys):
        b_all, d_all = classify(keys_all)
        out["all_instants"] = {"instants_examined": len(keys_all), "overlap_events": d_all["pairs"],
                               "buckets": b_all, "detail": d_all,
                               "shared_centreline_opposing_same_carriageway":
                                   b_all.get("same_edge/opposing", 0)
                                   + b_all.get("same_street_across_junction/opposing", 0)}
    if project:
        off = 0.5 * max(1, int(lanes_per_dir)) * float(lane_width_m) * side
        moved = 0
        for t in keys_all:
            for vid, (x, y) in list(inst[t].items()):
                u = hdg.get((vid, t))
                if u is None:
                    continue
                # +off is to the vehicle's LEFT (roads.DRIVE_SIDES convention)
                inst[t][vid] = (x + off * -u[1], y + off * u[0])
                moved += 1
        b2, d2 = classify(keys)
        out["projected"] = {
            "note": "geometry-only re-projection of the SAME trace into directed lane frames; "
                    "routes, car-following and signals are NOT re-simulated",
            "carriageway_offset_m": round(off, 3), "lanes_per_direction": int(lanes_per_dir),
            "samples_moved": moved, "overlap_events": d2["pairs"], "buckets": b2,
            "removed": total - d2["pairs"],
            "removed_share": round((total - d2["pairs"]) / total, 4) if total else None}
    return out


# ============================================================================================ #
# CLI
# ============================================================================================ #
def _resolve_gt_net(city: str, cache_dir: str) -> str:
    """The netconvert ground-truth net for a preset city (built once, then disk-cached)."""
    from scms_sim_ref.mock_pipeline import netimport as NI
    from scms_sim_ref.mock_pipeline.osm import CITY_BBOXES, fetch_osm, osm_cache_path
    bbox = CITY_BBOXES[city]
    fetch_osm(bbox, cache_dir)
    net_path = NI.net_cache_path(bbox, cache_dir)
    NI.run_netconvert(osm_cache_path(bbox, cache_dir), net_path)
    return net_path


def _candidate_for(city: str, path: str, cache_dir: str, max_nodes: int) -> dict:
    """Build an engine network document for `city` by one of the two import paths."""
    from scms_sim_ref.mock_pipeline import netimport as NI
    from scms_sim_ref.mock_pipeline import osm as OSM
    if path in ("raw", "raw-legacy"):
        attrs = path == "raw"
        nodes, edges, info = OSM.import_city(city, cache_dir, max_nodes=max_nodes,
                                             attrs=attrs, signals=attrs)
        doc = OSM.network_document(nodes, edges, info)
        return document_summary(doc, f"osm.py --{'attrs --signals' if attrs else 'legacy'}"
                                     f" max_nodes={max_nodes} ({city})")
    if path == "netconvert":
        nodes, edges, info = NI.import_city(city, cache_dir, max_nodes=0, shapes=True, strong=False)
        doc = OSM.network_document(nodes, edges, info)
        return document_summary(doc, f"netimport.py ({city})")
    if path == "netconvert-strong":
        nodes, edges, info = NI.import_city(city, cache_dir, max_nodes=0, shapes=True, strong=True)
        doc = OSM.network_document(nodes, edges, info)
        return document_summary(doc, f"netimport.py --strong ({city})")
    raise ValueError(f"unknown import path {path!r}")


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("compare", help="score an engine network against a SUMO .net.xml")
    g = c.add_mutually_exclusive_group(required=True)
    g.add_argument("--city", help="preset OSM city: build the candidate(s) and the ground-truth net")
    g.add_argument("--candidate", help="an engine custom-network JSON document to score")
    c.add_argument("--gt-net", help="the .net.xml ground truth (required with --candidate)")
    c.add_argument("--path", default="both",
                   choices=["raw", "raw-legacy", "netconvert", "netconvert-strong", "both", "all"],
                   help="with --city: which import path(s) to score")
    c.add_argument("--cache", default="datasets/_osmcache")
    c.add_argument("--max-nodes", type=int, default=380, help="raw-OSM path node budget")
    c.add_argument("--gt-scope", default="lcc", choices=["lcc", "all"])
    c.add_argument("--gt-vclass", default="passenger")
    c.add_argument("--gt-length", default="centre", choices=["centre", "lane"],
                   help="reference edge length: junction-centre to junction-centre (default, the "
                        "engine's own convention) or SUMO's lane length (excludes junction "
                        "interiors, so it is systematically shorter)")
    c.add_argument("--assume-lanes", type=int, default=1,
                   help="lanes per direction assumed for a candidate that declares none "
                        "(the engine's own n_lanes default is 1)")
    c.add_argument("--json", help="write the full result document here")

    o = sub.add_parser("overlaps", help="attribute traffic.overlap_events to the network layer")
    o.add_argument("dataset_dir")
    o.add_argument("--overlap-m", type=float, default=OVERLAP_DIST_M)
    o.add_argument("--max-instants", type=int, default=MAX_TIME_BUCKETS)
    o.add_argument("--all-instants", action="store_true",
                   help="also report the un-sub-sampled total over every instant")
    o.add_argument("--junction-radius", type=float, default=5.0)
    o.add_argument("--collinear-deg", type=float, default=20.0,
                   help="two edges sharing a node within this angle are ONE street, so a pair "
                        "straddling the junction still counts as a shared-centreline overlap")
    o.add_argument("--project-carriageways", action="store_true",
                   help="re-project the trace into directed lane frames and recount (geometry only)")
    o.add_argument("--lanes-per-dir", type=int, default=1)
    o.add_argument("--drive-side", default="right", choices=["right", "left"])
    o.add_argument("--json")

    a = p.parse_args(argv)
    if a.cmd == "compare":
        if a.candidate:
            if not a.gt_net:
                p.error("--gt-net is required with --candidate")
            with open(a.candidate, encoding="utf-8") as fh:
                doc = json.load(fh)
            cands = [document_summary(doc, os.path.basename(a.candidate),
                                      assume_lanes=a.assume_lanes)]
            gt_net = a.gt_net
        else:
            from scms_sim_ref.mock_pipeline.osm import CITY_BBOXES
            if a.city not in CITY_BBOXES:
                p.error(f"unknown city {a.city!r}; have {sorted(CITY_BBOXES)}")
            gt_net = a.gt_net or _resolve_gt_net(a.city, a.cache)
            paths = ({"both": ["raw", "netconvert"],
                      "all": ["raw-legacy", "raw", "netconvert", "netconvert-strong"]}
                     .get(a.path, [a.path]))
            cands = []
            for q in paths:
                try:
                    cands.append(_candidate_for(a.city, q, a.cache, a.max_nodes))
                except Exception as exc:      # an importer that REFUSES a city is a result too
                    cands.append({"source": f"{q} ({a.city}) -- IMPORT FAILED",
                                  "import_error": f"{type(exc).__name__}: {exc}"})
        gt = net_ground_truth(gt_net, vclass=a.gt_vclass, scope=a.gt_scope, length=a.gt_length)
        results = [compare(cd, gt) if "import_error" not in cd
                   else {"metrics": [], "summary": {"pass": 0, "fail": 0, "na": 0,
                                                    "hard_failures": ["import_failed"]},
                         "candidate": cd, "reference": {k: v for k, v in gt.items()
                                                        if not k.startswith("_")}}
                   for cd in cands]
        for r in results:
            if "import_error" in r["candidate"]:
                print(f"\n  candidate : {r['candidate']['source']}\n"
                      f"              {r['candidate']['import_error']}")
                continue
            print(_fmt_compare(r))
        doc = {"tool": "network_fidelity.compare", "ground_truth": {k: v for k, v in gt.items()
                                                                    if not k.startswith("_")},
               "results": results}
        if a.json:
            with open(a.json, "w", encoding="utf-8") as fh:
                json.dump(doc, fh, indent=1, sort_keys=True, default=str)
            print(f"\nwrote {a.json}")
        return 1 if any(r["summary"]["hard_failures"] for r in results) else 0

    res = attribute_overlaps(a.dataset_dir, overlap_m=a.overlap_m, max_instants=a.max_instants,
                             all_instants=a.all_instants, junction_r_m=a.junction_radius,
                             project=a.project_carriageways, lanes_per_dir=a.lanes_per_dir,
                             drive_side=a.drive_side, collinear_deg=a.collinear_deg)
    print(json.dumps(res, indent=1, sort_keys=True, default=str))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as fh:
            json.dump(res, fh, indent=1, sort_keys=True, default=str)
        print(f"wrote {a.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
