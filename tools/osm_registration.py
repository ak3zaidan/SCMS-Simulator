"""Measure road/building registration on an OSM import, and the link-state mix it produces.

This is the instrument for the defect recorded as #7 in `docs/realism/CROSS-ENGINE-RADIO.md`:
**10.35% of the Python engine's own road length, and 9.62% of its vehicle positions, fall inside
its own building footprints**, against 2.26% / 0.20% for the InTAS scene under identical code. The
question that document left open was *why*, and this tool answers it by measurement rather than by
inspection. Three sub-commands:

`overlap`  -- samples the road graph every metre against the footprints TWICE, with two different
              instruments, because they answer different questions:
                * **raster** -- the engine's own `run._BuildingRaster` at `GEO_BUILDING_CELL_M` 3 m.
                  This is the number the cross-engine document quotes. It has a ~1-cell halo: the
                  wall stamp marks every cell a footprint edge passes through, so a centreline 1.5 m
                  from a facade reads as "inside". In an old town that is most of the network.
                * **exact** -- even-odd point-in-polygon, no cells, no halo. This is the number that
                  says whether the road is REALLY in the building.
              Reports both, plus the same pair over the vehicle positions of a dataset when one is
              given, plus how deep inside the footprint the offending samples sit.

`sweep`    -- rebuilds the graph at a range of RDP tolerances against the SAME footprints. This is
              the causal experiment: if simplification is what displaces the roads, the exact
              overlap must collapse as the tolerance goes to zero, and it does.

`raw`      -- no simplification anywhere, on either layer: raw OSM way polylines against raw closed
              `building=*` rings in the identical frame. Whatever overlap survives that is in the
              SOURCE DATA, and every offending way is printed with its tags so an arcade can be told
              from an artefact.

`flagship`  -- rebuilds a reference dataset's own run on both maps, every other knob taken verbatim
              from its manifest, and re-measures the VEHICLE-position overlap. That is the quantity
              the awareness metric actually sees, and the BEFORE arm must reproduce the reference
              dataset's `data_digest_sha256` byte for byte or the comparison is not the map alone.

`linkstate` -- a cheap RNG-free LOS/NLOSb composition over evenly spaced road points, for a map with
              no dataset behind it. For the real thing use `flagship` and then run the unmodified
              `python -m scms_sim_ref.datagen.awareness` on each arm.

Usage:
    python tools/osm_registration.py overlap --city ingolstadt [--fixed] [--cells] [--dataset <d>]
    python tools/osm_registration.py sweep   --city ingolstadt --tols "10,7.5,5,2,1,0" --fixed
    python tools/osm_registration.py raw     --city ingolstadt
    python tools/osm_registration.py flagship --city ingolstadt --reference <d> --out <d>
    python tools/osm_registration.py linkstate --city ingolstadt [--json out.json]
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys
import xml.etree.ElementTree as ET

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src"))

from scms_sim_ref.mock_pipeline import osm as O                            # noqa: E402
from scms_sim_ref.mock_pipeline.run import _BuildingRaster                 # noqa: E402
from scms_sim_ref.mock_pipeline.run import GEO_BUILDING_CELL_M             # noqa: E402

SAMPLE_STEP_M = 1.0


# --------------------------------------------------------------------------- #
# instruments
# --------------------------------------------------------------------------- #
def raster_hit(ras, x: float, y: float) -> bool:
    """Is (x, y) on an occupied cell of the engine's own 3 m building raster?"""
    ix = int((x - ras.x0) / ras.cell)
    iy = int((y - ras.y0) / ras.cell)
    return 0 <= ix < ras.nx and 0 <= iy < ras.ny and bool(ras.grid[iy * ras.nx + ix])


class Exact:
    """Grid-bucketed exact point-in-polygon over footprints (no cells, so no wall halo)."""

    def __init__(self, polys, cell: float = 40.0):
        self.cell = float(cell)
        self.polys = polys
        self.b: dict = {}
        fl = math.floor
        for k, ring in enumerate(polys):
            xs = [p[0] for p in ring]
            ys = [p[1] for p in ring]
            for ix in range(int(fl(min(xs) / cell)), int(fl(max(xs) / cell)) + 1):
                for iy in range(int(fl(min(ys) / cell)), int(fl(max(ys) / cell)) + 1):
                    self.b.setdefault((ix, iy), []).append(k)

    def hit(self, x: float, y: float):
        key = (int(math.floor(x / self.cell)), int(math.floor(y / self.cell)))
        for k in self.b.get(key, ()):
            if O._point_in_ring(x, y, self.polys[k]):
                return k
        return None

    def depth(self, x: float, y: float) -> float:
        """Distance from (x, y) to the nearest wall of the polygon containing it (-1 if outside)."""
        k = self.hit(x, y)
        if k is None:
            return -1.0
        ring = self.polys[k]
        best = float("inf")
        n = len(ring)
        for i in range(n):
            ax, ay = ring[i]
            bx, by = ring[(i + 1) % n]
            dx, dy = bx - ax, by - ay
            L2 = dx * dx + dy * dy
            t = 0.0 if L2 == 0 else max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / L2))
            best = min(best, math.hypot(x - (ax + t * dx), y - (ay + t * dy)))
        return best


def edge_samples(nodes, edges, step: float = SAMPLE_STEP_M):
    for ei, e in enumerate(edges):
        a, b = nodes[e[0]], nodes[e[1]]
        L = math.dist(a, b)
        n = max(1, int(L / step))
        for i in range(n + 1):
            t = i / n
            yield ei, a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])


def measure_roads(nodes, edges, polys, *, depths: bool = False) -> dict:
    ras = _BuildingRaster(polys)
    ex = Exact(polys)
    tot = rh = eh = 0
    re_, ee = set(), set()
    dd = []
    for ei, x, y in edge_samples(nodes, edges):
        tot += 1
        if raster_hit(ras, x, y):
            rh += 1
            re_.add(ei)
        if ex.hit(x, y) is not None:
            eh += 1
            ee.add(ei)
            if depths:
                dd.append(ex.depth(x, y))
    out = {"samples": tot, "raster_inside": rh, "raster_frac": round(rh / tot, 6),
           "exact_inside": eh, "exact_frac": round(eh / tot, 6),
           "edges": len(edges), "edges_raster": len(re_), "edges_exact": len(ee),
           "road_length_m": round(sum(math.dist(nodes[e[0]], nodes[e[1]]) for e in edges), 1),
           "raster_cell_m": ras.cell, "polygons": len(polys)}
    if dd:
        dd.sort()
        out["interior_depth_p50_m"] = round(dd[len(dd) // 2], 2)
        out["interior_depth_p90_m"] = round(dd[int(0.9 * len(dd))], 2)
        out["interior_depth_max_m"] = round(dd[-1], 2)
        out["deeper_than_one_cell"] = sum(1 for d in dd if d > ras.cell)
    return out


def raster_by_cell(nodes, edges, polys, cells=(3.0, 2.0, 1.0, 0.5)) -> dict:
    """Road-inside-building fraction at several raster resolutions.

    Separates the ~1-cell wall halo from real overlap: `_BuildingRaster` stamps every cell a
    footprint EDGE passes through, so at 3 m a centreline running 1.5 m from a facade -- an ordinary
    old-town street -- already reads as inside. If the fraction collapses toward the exact
    point-in-polygon number as the cell shrinks, what is left at 3 m is halo, not map error."""
    out = {}
    for c in cells:
        ras = _BuildingRaster(polys, cell_m=c)
        tot = hit = 0
        for _ei, x, y in edge_samples(nodes, edges):
            tot += 1
            if raster_hit(ras, x, y):
                hit += 1
        out[f"cell_{c}m"] = round(hit / tot, 6)
    return out


def measure_positions(xs_ys, polys) -> dict:
    """Vehicle positions (iterable of (x, y)) against the same two instruments."""
    ras = _BuildingRaster(polys)
    ex = Exact(polys)
    tot = rh = eh = 0
    for x, y in xs_ys:
        tot += 1
        if raster_hit(ras, x, y):
            rh += 1
        if ex.hit(x, y) is not None:
            eh += 1
    return {"positions": tot, "raster_inside": rh, "raster_frac": round(rh / max(1, tot), 6),
            "exact_inside": eh, "exact_frac": round(eh / max(1, tot), 6)}


# --------------------------------------------------------------------------- #
# graph builders
# --------------------------------------------------------------------------- #
def load_extract(city_or_bbox, cache: str):
    bbox = O.CITY_BBOXES[city_or_bbox] if isinstance(city_or_bbox, str) else tuple(city_or_bbox)
    return O.fetch_osm(bbox, cache)


def build(xml_text: str, *, tol: float, avoid, max_nodes: int) -> tuple:
    nodes, edges, info = O.osm_to_network(xml_text, max_nodes=max_nodes, tol_m=tol,
                                          avoid_polygons=avoid)
    return nodes, edges, info


def footprints_for(xml_text: str, info: dict):
    return O.extract_buildings(xml_text, info["projection"], info["road_bbox"])


# --------------------------------------------------------------------------- #
# link-state composition (the number that drives awareness)
# --------------------------------------------------------------------------- #
BANDS = ((0, 50), (50, 100), (100, 150), (150, 200), (200, 250), (300, 350), (450, 500))


def link_state_mix(nodes, edges, polys, *, samples_per_edge: int = 6,
                   max_pairs: int = 400_000, seedless_stride: int = 1) -> dict:
    """LOS / NLOSb composition over co-present road points, band by band.

    Deterministic and RNG-free: the "population" is a fixed, evenly spaced set of points along the
    road graph (no vehicles are simulated, so nothing here can touch a pinned digest), and every
    ordered pair inside 500 m is classified with the engine's own `_BuildingRaster.blocked`. NLOSv
    needs a vehicle fleet and is deliberately not modelled here -- what this measures is the LOS vs
    NLOSb split the MAP itself imposes, which is the half the registration defect can move."""
    ras = _BuildingRaster(polys)
    pts = []
    for e in edges:
        a, b = nodes[e[0]], nodes[e[1]]
        for i in range(samples_per_edge):
            t = (i + 0.5) / samples_per_edge
            pts.append((a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])))
    pts = pts[::seedless_stride]
    tally = {b: [0, 0] for b in BANDS}            # band -> [n, n_blocked]
    n = len(pts)
    pairs = 0
    for i in range(n):
        xi, yi = pts[i]
        for j in range(i + 1, n):
            xj, yj = pts[j]
            d = math.hypot(xj - xi, yj - yi)
            if d > 500.0:
                continue
            for lo, hi in BANDS:
                if lo <= d < hi:
                    t = tally[(lo, hi)]
                    t[0] += 1
                    if ras.blocked(xi, yi, xj, yj):
                        t[1] += 1
                    pairs += 1
                    break
            if pairs >= max_pairs:
                break
        if pairs >= max_pairs:
            break
    out = {}
    for b, (nn, nb) in tally.items():
        out[f"{b[0]}-{b[1]}"] = {"pairs": nn,
                                 "nlosb": round(nb / nn, 4) if nn else None,
                                 "los": round(1.0 - nb / nn, 4) if nn else None}
    out["_points"] = n
    out["_pairs"] = pairs
    return out


# --------------------------------------------------------------------------- #
# sub-commands
# --------------------------------------------------------------------------- #
def cmd_overlap(a) -> int:
    xml_text = load_extract(a.city or [float(v) for v in a.bbox.split(",")], a.cache)
    n0, e0, i0 = build(xml_text, tol=a.tol, avoid=None, max_nodes=a.max_nodes)
    polys, binfo = footprints_for(xml_text, i0)
    res = {"footprints": len(polys)}
    res["before"] = measure_roads(n0, e0, polys, depths=True)
    res["before"]["nodes"] = len(n0)
    if a.cells:
        res["before"]["raster_by_cell"] = raster_by_cell(n0, e0, polys)
    if a.fixed:
        n1, e1, i1 = build(xml_text, tol=a.tol, avoid=polys, max_nodes=a.max_nodes)
        res["after"] = measure_roads(n1, e1, polys, depths=True)
        res["after"]["nodes"] = len(n1)
        if a.cells:
            res["after"]["raster_by_cell"] = raster_by_cell(n1, e1, polys)
        rep = dict(i1["road_building_overlap"])
        res["after"]["report"] = {k: v for k, v in rep.items() if k != "edges"}
        res["after"]["through_building_edges"] = rep["edges"]
    if a.dataset:
        pos = list(dataset_positions(a.dataset))
        res["positions_shipped_map"] = measure_positions(pos, polys)
    print(json.dumps(res, indent=1, sort_keys=True))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as fh:
            json.dump(res, fh, indent=1, sort_keys=True)
    return 0


def dataset_positions(path: str):
    f = os.path.join(path, "ground_truth", "gt_emissions_sample.jsonl")
    if not os.path.exists(f):
        f = os.path.join(path, "ground_truth", "gt_emissions.jsonl")
    with open(f, encoding="utf-8") as fh:
        for line in fh:
            r = json.loads(line)
            x = r.get("true_x", r.get("x", r.get("pos_x")))
            y = r.get("true_y", r.get("y", r.get("pos_y")))
            if x is not None and y is not None:
                yield float(x), float(y)


def cmd_sweep(a) -> int:
    xml_text = load_extract(a.city or [float(v) for v in a.bbox.split(",")], a.cache)
    _n, _e, i0 = build(xml_text, tol=a.tol, avoid=None, max_nodes=a.max_nodes)
    polys, _b = footprints_for(xml_text, i0)
    rows = []
    for tol in [float(v) for v in a.tols.split(",")]:
        for avoid in ((None, polys) if a.fixed else (None,)):
            n, e, info = build(xml_text, tol=tol, avoid=avoid, max_nodes=1_000_000)
            m = measure_roads(n, e, polys)
            m.update(tol_m=tol, constrained=avoid is not None, nodes=len(n))
            rows.append(m)
            print("tol=%-5s constrained=%-5s nodes=%-5d edges=%-5d  raster %.2f%%  exact %.2f%%"
                  % (tol, avoid is not None, len(n), len(e),
                     100 * m["raster_frac"], 100 * m["exact_frac"]))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as fh:
            json.dump(rows, fh, indent=1)
    return 0


def cmd_raw(a) -> int:
    """Raw OSM polylines vs raw closed building rings. No simplification on either layer."""
    xml_text = load_extract(a.city or [float(v) for v in a.bbox.split(",")], a.cache)
    root = ET.fromstring(xml_text)
    latlon = {n.get("id"): (float(n.get("lat")), float(n.get("lon"))) for n in root.iter("node")}
    fr = O.road_projection(xml_text)
    lat0, lon0, kx, ky = fr["lat0"], fr["lon0"], fr["kx"], fr["ky"]

    def xy(r):
        la, lo = latlon[r]
        return ((lo - lon0) * kx, (la - lat0) * ky)

    roads, bldgs = [], []
    for w in root.iter("way"):
        tags = {t.get("k"): t.get("v") for t in w.findall("tag")}
        if tags.get("highway") in O.HIGHWAY_SPEED:
            refs = [nd.get("ref") for nd in w.findall("nd") if nd.get("ref") in latlon]
            if len(refs) >= 2:
                roads.append((w.get("id"), tags, [xy(r) for r in refs]))
        elif tags.get("building") or tags.get("building:part"):
            refs = [nd.get("ref") for nd in w.findall("nd")]
            if len(refs) >= 4 and refs[0] == refs[-1] and all(r in latlon for r in refs):
                bldgs.append((w.get("id"), tags, [xy(r) for r in refs[:-1]]))
    ex = Exact([b[2] for b in bldgs])
    tot = inside = 0
    per_way: dict = {}
    per_b: dict = {}
    for wid, tags, pts in roads:
        for p, q in zip(pts, pts[1:]):
            L = math.dist(p, q)
            n = max(1, int(L / SAMPLE_STEP_M))
            for i in range(n + 1):
                t = i / n
                x, y = p[0] + t * (q[0] - p[0]), p[1] + t * (q[1] - p[1])
                tot += 1
                k = ex.hit(x, y)
                if k is not None:
                    inside += 1
                    per_way[wid] = per_way.get(wid, 0) + 1
                    per_b[bldgs[k][0]] = per_b.get(bldgs[k][0], 0) + 1
    rmap = {w: t for w, t, _p in roads}
    bmap = {w: t for w, t, _r in bldgs}
    ways = []
    for wid, n in sorted(per_way.items(), key=lambda z: (-z[1], z[0])):
        tags = rmap[wid]
        ways.append({"way": wid, "metres": n, "highway": tags.get("highway"),
                     "name": tags.get("name"),
                     "reason": O.through_building_reason(tags) or "untagged"})
    res = {"raw_drivable_ways": len(roads), "raw_building_rings": len(bldgs),
           "samples": tot, "inside": inside, "frac": round(inside / max(1, tot), 6),
           "offending_ways": len(per_way), "footprints_involved": len(per_b),
           "metres_tagged": sum(w["metres"] for w in ways if w["reason"] != "untagged"),
           "metres_untagged": sum(w["metres"] for w in ways if w["reason"] == "untagged"),
           "ways": ways,
           "footprints": [{"way": w, "metres": n,
                           "tags": {k: v for k, v in bmap[w].items()
                                    if k in ("building", "building:part", "layer", "tunnel",
                                             "covered", "name", "building:levels")}}
                          for w, n in sorted(per_b.items(), key=lambda z: (-z[1], z[0]))]}
    print(json.dumps(res, indent=1, sort_keys=True))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as fh:
            json.dump(res, fh, indent=1, sort_keys=True)
    return 0


def cmd_flagship(a) -> int:
    """Rebuild a dataset's own run on the BEFORE and AFTER maps and re-measure the two overlaps.

    Everything except `custom_network` is taken verbatim from the reference dataset's manifest, so
    the arm difference is the map and nothing else. This is what turns "the road graph is fixed"
    into "the vehicles no longer stand in walls", which is the quantity the awareness metric sees.
    """
    from scms_sim_ref.mock_pipeline import PipelineConfig, config_from_dict, run_pipeline

    xml_text = load_extract(a.city or [float(v) for v in a.bbox.split(",")], a.cache)
    n0, e0, i0 = build(xml_text, tol=a.tol, avoid=None, max_nodes=a.max_nodes)
    polys, _b = footprints_for(xml_text, i0)
    n1, e1, i1 = build(xml_text, tol=a.tol, avoid=polys, max_nodes=a.max_nodes)
    docs = {"before": O.network_document(n0, e0, i0, buildings=polys),
            "after": O.network_document(n1, e1, i1, buildings=polys)}
    base = json.load(open(os.path.join(a.reference, "manifest.json"), encoding="utf-8"))["config"]
    res: dict = {"maps": {}}
    for arm, doc in docs.items():
        cfg = dict(base)
        cfg["custom_network"] = json.dumps(doc, separators=(",", ":"))
        cfg["out_dir"] = os.path.join(a.out, arm)
        r = run_pipeline(config_from_dict(cfg) if not isinstance(cfg, PipelineConfig) else cfg)
        pos = list(dataset_positions(cfg["out_dir"]))
        res["maps"][arm] = {"nodes": len(doc["nodes"]), "edges": len(doc["edges"]),
                            "through_building_edges": len(doc.get("through_building_edges", [])),
                            "vehicles": r.n_vehicles, "reports": r.n_reports,
                            "digest": r.data_digest,
                            "roads": measure_roads(doc["nodes"], doc["edges"], polys),
                            "positions": measure_positions(pos, polys)}
        print(arm, json.dumps(res["maps"][arm]["positions"]))
    print(json.dumps(res, indent=1, sort_keys=True))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as fh:
            json.dump(res, fh, indent=1, sort_keys=True)
    return 0


def cmd_linkstate(a) -> int:
    xml_text = load_extract(a.city or [float(v) for v in a.bbox.split(",")], a.cache)
    n0, e0, i0 = build(xml_text, tol=a.tol, avoid=None, max_nodes=a.max_nodes)
    polys, _b = footprints_for(xml_text, i0)
    n1, e1, i1 = build(xml_text, tol=a.tol, avoid=polys, max_nodes=a.max_nodes)
    res = {"before": link_state_mix(n0, e0, polys, samples_per_edge=a.per_edge),
           "after": link_state_mix(n1, e1, polys, samples_per_edge=a.per_edge),
           "nodes_before": len(n0), "nodes_after": len(n1),
           "edges_before": len(e0), "edges_after": len(e1)}
    print(json.dumps(res, indent=1, sort_keys=True))
    if a.json:
        with open(a.json, "w", encoding="utf-8") as fh:
            json.dump(res, fh, indent=1, sort_keys=True)
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    for name, fn in (("overlap", cmd_overlap), ("sweep", cmd_sweep), ("raw", cmd_raw),
                     ("linkstate", cmd_linkstate), ("flagship", cmd_flagship)):
        s = sub.add_parser(name)
        g = s.add_mutually_exclusive_group(required=True)
        g.add_argument("--city", choices=sorted(O.CITY_BBOXES))
        g.add_argument("--bbox")
        s.add_argument("--cache", default="datasets/_osmcache")
        s.add_argument("--tol", type=float, default=10.0)
        s.add_argument("--max-nodes", type=int, default=380)
        s.add_argument("--json")
        s.set_defaults(fn=fn)
        if name == "overlap":
            s.add_argument("--fixed", action="store_true",
                           help="also build the constrained graph and report the after numbers")
            s.add_argument("--dataset", help="dataset dir whose gt emissions to test as positions")
            s.add_argument("--cells", action="store_true",
                           help="also report the raster fraction at 3/2/1/0.5 m cells (halo split)")
        if name == "sweep":
            s.add_argument("--tols", default="10,5,2,1,0")
            s.add_argument("--fixed", action="store_true",
                           help="run each tolerance constrained as well as unconstrained")
        if name == "linkstate":
            s.add_argument("--per-edge", type=int, default=6)
        if name == "flagship":
            s.add_argument("--reference", required=True,
                           help="dataset dir whose manifest config supplies every other knob")
            s.add_argument("--out", required=True,
                           help="parent dir for the before/ and after/ runs")
    a = p.parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
