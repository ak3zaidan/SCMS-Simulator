"""Real-city street graphs (and building footprints) for the pure-Python generator.

Converts an OpenStreetMap extract (raw OSM XML) into the {nodes, edges} custom-network format:
drivable ways become edges (with per-edge speed limits from maxspeed or the highway class),
shared way-nodes become intersections, curve geometry is simplified (RDP) under a deviation
tolerance, and only the largest connected component is kept. The result is plain data -- the
simulation itself stays deterministic and offline; the network fetch happens once and is cached.

The SAME cached Overpass extract also carries every `building=*` way (Overpass `/api/map` returns
all raw OSM data in the bbox; the importer historically discarded everything that was not a
`highway`). `extract_buildings` recovers those footprints as closed rings in the SAME local metric
frame as the road graph -- see the projection note on `osm_to_network` -- so the Phase-2 geometric
radio model can run a LOS/NLOSb blockage test against real city geometry with no new download, no
new cache and no geometry dependency.

CLI:
    python -m scms_sim_ref.mock_pipeline.osm --city paris --out paris.json
    python -m scms_sim_ref.mock_pipeline.osm --city ingolstadt --buildings --out ingolstadt.json
    python -m scms_sim_ref.mock_pipeline.run --flow --road custom --custom-network paris.json ...
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import sys
import urllib.request
import xml.etree.ElementTree as ET

# central extracts kept small so the graph fits the simulator's custom-map budget
CITY_BBOXES = {
    "manhattan": (-73.9900, 40.7440, -73.9680, 40.7620),
    "sanfrancisco": (-122.4180, 37.7840, -122.3980, 37.8000),
    "london": (-0.1050, 51.5100, -0.0800, 51.5250),
    "paris": (2.3300, 48.8560, 2.3600, 48.8680),
    "berlin": (13.3800, 52.5100, 13.4100, 52.5250),
    "rome": (12.4700, 41.8900, 12.4950, 41.9050),
    "amsterdam": (4.8850, 52.3650, 4.9050, 52.3780),
    "vienna": (16.3600, 48.2000, 16.3800, 48.2150),
    "chicago": (-87.6400, 41.8750, -87.6200, 41.8900),
    "munich": (11.5650, 48.1330, 11.5850, 48.1450),
    "ingolstadt": (11.4180, 48.7590, 11.4380, 48.7700),
}

# drivable highway classes -> default speed limit (m/s) when no maxspeed tag is present
HIGHWAY_SPEED = {
    "motorway": 33.0, "motorway_link": 16.0, "trunk": 28.0, "trunk_link": 14.0,
    "primary": 17.0, "primary_link": 14.0, "secondary": 14.0, "secondary_link": 12.0,
    "tertiary": 12.5, "tertiary_link": 11.0, "unclassified": 11.0,
    "residential": 8.3, "living_street": 5.5,
}
# progressively droppable classes when an extract exceeds the node budget (minor roads first)
_DROP_ORDER = ["living_street", "residential", "unclassified", "tertiary_link", "tertiary"]

_OVERPASS = "https://overpass-api.de/api/map?bbox="   # the OSM main API 406s; Overpass serves raw XML


def _parse_maxspeed(v: str | None) -> float | None:
    """'50' (km/h), '30 mph', '50; 30' -> m/s. None/unparseable -> None."""
    if not v:
        return None
    v = v.split(";")[0].strip().lower()
    try:
        if v.endswith("mph"):
            return max(1.0, min(70.0, float(v[:-3].strip()) * 0.44704))
        return max(1.0, min(70.0, float(v.split()[0]) / 3.6))
    except ValueError:
        return None


def _rdp(pts: list, tol: float) -> list:
    """Ramer-Douglas-Peucker polyline simplification (keeps curve shape within tol metres)."""
    if len(pts) < 3:
        return pts
    ax, ay = pts[0]
    bx, by = pts[-1]
    dx, dy = bx - ax, by - ay
    dd = math.hypot(dx, dy)
    imax, dmax = 0, -1.0
    for i in range(1, len(pts) - 1):
        px, py = pts[i]
        d = (abs(dx * (ay - py) - dy * (ax - px)) / dd) if dd > 0 else math.hypot(px - ax, py - ay)
        if d > dmax:
            imax, dmax = i, d
    if dmax > tol:
        left = _rdp(pts[:imax + 1], tol)
        return left[:-1] + _rdp(pts[imax:], tol)
    return [pts[0], pts[-1]]


def osm_to_network(xml_text: str, max_nodes: int = 380, tol_m: float = 10.0) -> tuple[list, list, dict]:
    """OSM XML -> (nodes, edges, info) in custom-network form. Deterministic for a given input.

    Escalates simplification (higher tolerance, then dropping minor road classes) until the graph
    fits max_nodes; raises ValueError if even the arterial skeleton is too large for the budget.

    `info["projection"]` carries the EXACT local equirectangular frame the road nodes were projected
    with, `{"lat0","lon0","kx","ky"}` -- and `info["road_bbox"]` the projected extent of the kept
    component. Any other layer derived from the same extract (buildings, POIs, ...) MUST reuse that
    tuple verbatim: the origin is `min(lat), min(lon)` OVER THE ROAD WAYS ONLY, so an independently
    derived origin misaligns the layers against the road graph by whole city blocks while still
    producing plausible-looking output. (Note ky = 110540.0 here, not the 111320 that a generic
    equirectangular snippet uses -- copying the wrong constant is a 0.7% north-south scale error.)"""
    root = ET.fromstring(xml_text)
    latlon: dict[str, tuple[float, float]] = {}
    for nd in root.iter("node"):
        latlon[nd.get("id")] = (float(nd.get("lat")), float(nd.get("lon")))
    ways = []                                    # (class, speed_mps, [node ids])
    for way in root.iter("way"):
        tags = {t.get("k"): t.get("v") for t in way.findall("tag")}
        cls = tags.get("highway")
        if cls not in HIGHWAY_SPEED:
            continue
        refs = [nd.get("ref") for nd in way.findall("nd") if nd.get("ref") in latlon]
        if len(refs) < 2:
            continue
        speed = _parse_maxspeed(tags.get("maxspeed")) or HIGHWAY_SPEED[cls]
        ways.append((cls, speed, refs))
    if not ways:
        raise ValueError("no drivable roads found in the OSM extract")
    # local metric projection around the extract centre
    lats = [latlon[r][0] for _, _, refs in ways for r in refs]
    lons = [latlon[r][1] for _, _, refs in ways for r in refs]
    lat0, lon0 = min(lats), min(lons)
    kx = 111320.0 * math.cos(math.radians((min(lats) + max(lats)) / 2.0))
    ky = 110540.0

    def xy(ref):
        la, lo = latlon[ref]
        return ((lo - lon0) * kx, (la - lat0) * ky)

    for attempt in range(len(_DROP_ORDER) + 1):
        dropped = set(_DROP_ORDER[:attempt])
        use = [(c, s, r) for c, s, r in ways if c not in dropped]
        tol = tol_m * (1.0 + 0.5 * attempt)      # simplify harder as we escalate
        if not use:
            break
        # graph nodes = way endpoints + nodes shared between ways
        counts: dict[str, int] = {}
        for _c, _s, refs in use:
            for r in set(refs):
                counts[r] = counts.get(r, 0) + 1
        keep = {r for _c, _s, refs in use for r in (refs[0], refs[-1])}
        keep |= {r for r, n in counts.items() if n >= 2}
        idx: dict[tuple, int] = {}
        nodes: list[list[float]] = []
        edges: dict[tuple[int, int], float] = {}

        def nid(pt):
            key = (round(pt[0], 1), round(pt[1], 1))
            k = idx.get(key)
            if k is None:
                k = len(nodes)
                idx[key] = k
                nodes.append([round(pt[0], 1), round(pt[1], 1)])
            return k

        for _c, speed, refs in use:
            # split the way at graph nodes, RDP-simplify each chain, emit straight edges
            chain = [refs[0]]
            for r in refs[1:]:
                chain.append(r)
                if r in keep:
                    pts = _rdp([xy(q) for q in chain], tol)
                    for p, q in zip(pts, pts[1:]):
                        a, b = nid(p), nid(q)
                        if a != b and math.dist(nodes[a], nodes[b]) >= 1.0:
                            k2 = (min(a, b), max(a, b))
                            edges[k2] = max(edges.get(k2, 0.0), speed)
                    chain = [r]
        if not edges:
            continue
        # largest connected component only (extracts contain fragments)
        adj: dict[int, list[int]] = {}
        for a, b in edges:
            adj.setdefault(a, []).append(b)
            adj.setdefault(b, []).append(a)
        seen: set[int] = set()
        best_comp: set[int] = set()
        for start in sorted(adj):
            if start in seen:
                continue
            comp = {start}
            stack = [start]
            seen.add(start)
            while stack:
                for m in adj[stack.pop()]:
                    if m not in seen:
                        seen.add(m)
                        comp.add(m)
                        stack.append(m)
            if len(comp) > len(best_comp):
                best_comp = comp
        remap = {old: new for new, old in enumerate(sorted(best_comp))}
        out_nodes = [nodes[old] for old in sorted(best_comp)]
        out_edges = [[remap[a], remap[b], round(sp, 1)]
                     for (a, b), sp in sorted(edges.items()) if a in remap and b in remap]
        if len(out_nodes) <= max_nodes:
            xs = [p[0] for p in out_nodes]
            ys = [p[1] for p in out_nodes]
            info = {"kept_nodes": len(out_nodes), "kept_edges": len(out_edges),
                    "dropped_classes": sorted(dropped), "rdp_tol_m": tol,
                    "ways_parsed": len(ways),
                    # the projection the building layer MUST reuse verbatim (see the docstring)
                    "projection": {"lat0": lat0, "lon0": lon0, "kx": kx, "ky": ky},
                    "road_bbox": [min(xs), min(ys), max(xs), max(ys)]}
            return out_nodes, out_edges, info
    raise ValueError(f"extract still exceeds {max_nodes} nodes after dropping "
                     f"{_DROP_ORDER}; use a smaller bbox")


def _ring_area(ring: list) -> float:
    """Unsigned shoelace area (m^2) of a closed ring given as an open vertex list."""
    a = 0.0
    n = len(ring)
    for i in range(n):
        x0, y0 = ring[i]
        x1, y1 = ring[(i + 1) % n]
        a += x0 * y1 - x1 * y0
    return abs(a) * 0.5


def extract_buildings(xml_text: str, projection: dict, road_bbox: list | None = None, *,
                      margin_m: float = 250.0, min_area_m2: float = 12.0,
                      simplify_tol_m: float = 1.0, max_polygons: int = 6000) -> tuple[list, dict]:
    """`building=*` ways from the SAME cached Overpass XML -> closed rings in projected metres.

    `projection` MUST be the `info["projection"]` dict returned by `osm_to_network` for this exact
    XML. Re-deriving an origin from the building nodes would shift the whole footprint layer against
    the road graph (the road origin is min(lat)/min(lon) over ROAD ways only) and silently corrupt
    every LOS/NLOS classification downstream, so the tuple is threaded through rather than
    recomputed -- and the caller-visible alignment assertion below is what makes a mistake loud.

    Only ways with a complete, closed geometry are kept (an Overpass extract clips ways at the bbox
    edge; a way with an unresolvable node ref is dropped rather than guessed). Rings smaller than
    `min_area_m2` are dropped, the rest are RDP-simplified to `simplify_tol_m`, and -- when
    `road_bbox` is given -- only footprints within `margin_m` of the road extent are returned, since
    nothing further away can ever block a link between two vehicles.

    Returns `(polygons, info)`; each polygon is an OPEN vertex list `[[x, y], ...]` (the closing
    edge back to `polygons[i][0]` is implied). Deterministic for a given input.

    Raises ValueError if the projected footprints do not overlap the road network -- the signature
    of a wrong projection origin/scale, which is otherwise invisible in the output.
    """
    lat0 = float(projection["lat0"])
    lon0 = float(projection["lon0"])
    kx = float(projection["kx"])
    ky = float(projection["ky"])
    root = ET.fromstring(xml_text)
    latlon: dict[str, tuple[float, float]] = {}
    for nd in root.iter("node"):
        latlon[nd.get("id")] = (float(nd.get("lat")), float(nd.get("lon")))

    raw = 0
    incomplete = 0
    unclosed = 0
    rings: list[list] = []
    for way in root.iter("way"):
        tags = {t.get("k"): t.get("v") for t in way.findall("tag")}
        if not (tags.get("building") or tags.get("building:part")):
            continue
        raw += 1
        refs = [nd.get("ref") for nd in way.findall("nd")]
        if any(r not in latlon for r in refs):
            incomplete += 1
            continue
        if len(refs) < 4 or refs[0] != refs[-1]:
            unclosed += 1
            continue
        pts = []
        for r in refs[:-1]:                      # drop the repeated closing vertex
            la, lo = latlon[r]
            pts.append([(lo - lon0) * kx, (la - lat0) * ky])
        if len(pts) < 3:
            unclosed += 1
            continue
        rings.append(pts)

    kept: list[list] = []
    dropped_small = 0
    for pts in rings:
        if _ring_area(pts) < min_area_m2:
            dropped_small += 1
            continue
        if simplify_tol_m > 0 and len(pts) > 4:
            simp = _rdp(pts + [pts[0]], simplify_tol_m)   # simplify as a closed polyline
            if len(simp) >= 4:
                pts = [list(p) for p in simp[:-1]]
        kept.append(pts)

    info = {"building_ways": raw, "incomplete": incomplete, "unclosed": unclosed,
            "kept_before_bbox": len(kept), "dropped_below_min_area": dropped_small,
            "projection": {"lat0": lat0, "lon0": lon0, "kx": kx, "ky": ky}}
    if not kept:
        info.update(polygons=0, centroid_inside_road_bbox_frac=None)
        return [], info

    bxs = [p[0] for ring in kept for p in ring]
    bys = [p[1] for ring in kept for p in ring]
    info["building_bbox"] = [min(bxs), min(bys), max(bxs), max(bys)]

    if road_bbox is not None:
        rx0, ry0, rx1, ry1 = (float(v) for v in road_bbox)
        cents = []
        for ring in kept:
            cents.append((sum(p[0] for p in ring) / len(ring),
                          sum(p[1] for p in ring) / len(ring)))
        cxs = sorted(c[0] for c in cents)
        cys = sorted(c[1] for c in cents)
        med = (cxs[len(cxs) // 2], cys[len(cys) // 2])
        # ALIGNMENT ASSERTION. Buildings and roads come out of the SAME extract, so the footprint
        # cloud must sit on the road network. A wrong origin translates the whole cloud by hundreds
        # of metres to kilometres while every individual polygon still looks perfectly fine -- this
        # is the only place that mistake is observable. The tolerance is scale-relative (half the
        # road diagonal, floor `margin_m`) so it holds for a sparse rural extract with one footprint
        # as well as for a dense core, and it is a GATE, not a filter.
        diag = math.hypot(rx1 - rx0, ry1 - ry0)
        tol = max(margin_m, 0.5 * diag)
        info["median_building_centroid"] = [round(med[0], 2), round(med[1], 2)]
        info["alignment_tolerance_m"] = round(tol, 1)
        if not (rx0 - tol <= med[0] <= rx1 + tol and ry0 - tol <= med[1] <= ry1 + tol):
            raise ValueError(
                f"projected building footprints do not sit on the road network: median centroid "
                f"{info['median_building_centroid']} is outside the road bbox "
                f"{[rx0, ry0, rx1, ry1]} widened by {tol:.0f} m. This is the projection trap -- "
                f"buildings must be projected with the SAME (lat0, lon0, kx, ky) that "
                f"osm_to_network derived from the ROAD ways (note ky = 110540.0, not 111320), "
                f"never with a re-derived origin.")
        # diagnostics (reported, not gated): a dense core blankets the road extent, a rural one
        # does not, and neither says anything about whether the projection is right
        ox = max(0.0, min(rx1, info["building_bbox"][2]) - max(rx0, info["building_bbox"][0]))
        oy = max(0.0, min(ry1, info["building_bbox"][3]) - max(ry0, info["building_bbox"][1]))
        road_area = max(1e-9, (rx1 - rx0) * (ry1 - ry0))
        info["road_bbox_covered_by_buildings"] = round((ox * oy) / road_area, 4)
        lo_x, lo_y, hi_x, hi_y = rx0 - margin_m, ry0 - margin_m, rx1 + margin_m, ry1 + margin_m
        near, inside = [], 0
        for ring, (cx, cy) in zip(kept, cents):
            if rx0 <= cx <= rx1 and ry0 <= cy <= ry1:
                inside += 1
            if lo_x <= cx <= hi_x and lo_y <= cy <= hi_y:
                near.append(ring)
        info["centroid_inside_road_bbox_frac"] = round(inside / len(kept), 4)
        kept = near

    kept.sort(key=lambda r: (round(min(p[0] for p in r), 3), round(min(p[1] for p in r), 3)))
    if len(kept) > max_polygons:
        info["truncated_from"] = len(kept)
        kept = kept[:max_polygons]
    out = [[[round(x, 2), round(y, 2)] for x, y in ring] for ring in kept]
    info["polygons"] = len(out)
    info["vertices"] = sum(len(r) for r in out)
    return out, info


def fetch_osm(bbox: tuple, cache_dir: str) -> str:
    """Download (or reuse a cached) raw OSM XML extract for bbox = (minLon,minLat,maxLon,maxLat)."""
    os.makedirs(cache_dir, exist_ok=True)
    key = hashlib.sha256(",".join(f"{b:.5f}" for b in bbox).encode()).hexdigest()[:16]
    cached = os.path.join(cache_dir, f"osm_{key}.xml")
    if os.path.exists(cached) and os.path.getsize(cached) > 1000:
        with open(cached, encoding="utf-8") as fh:
            return fh.read()
    url = _OVERPASS + ",".join(str(b) for b in bbox)
    req = urllib.request.Request(url, headers={"User-Agent": "SCMS-Simulator/1.0"})
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = resp.read()
    if len(data) < 1000:
        raise ValueError(f"OSM download for bbox {bbox} came back empty")
    with open(cached, "wb") as fh:
        fh.write(data)
    return data.decode("utf-8", "replace")


def import_city(city_or_bbox, cache_dir: str, max_nodes: int = 380,
                buildings: bool = False) -> tuple[list, list, dict]:
    """City name (see CITY_BBOXES) or explicit bbox -> (nodes, edges, info).

    With `buildings=True` the SAME cached extract is re-read for `building=*` footprints, projected
    with the road graph's own projection tuple, and returned as `info["buildings"]` (a list of open
    rings in metres) plus `info["buildings_info"]`. No extra download: `fetch_osm` is cached and
    Overpass `/api/map` already returned the footprints."""
    if isinstance(city_or_bbox, str):
        if city_or_bbox not in CITY_BBOXES:
            raise ValueError(f"unknown city {city_or_bbox!r}; have {sorted(CITY_BBOXES)} "
                             f"(or pass an explicit bbox [minLon,minLat,maxLon,maxLat])")
        bbox = CITY_BBOXES[city_or_bbox]
    else:
        bbox = tuple(float(v) for v in city_or_bbox)
        if len(bbox) != 4 or bbox[0] >= bbox[2] or bbox[1] >= bbox[3]:
            raise ValueError("bbox must be [minLon, minLat, maxLon, maxLat]")
        if (bbox[2] - bbox[0]) > 0.05 or (bbox[3] - bbox[1]) > 0.04:
            raise ValueError("bbox too large (keep it under ~0.05 x 0.04 degrees, a city core)")
    xml_text = fetch_osm(bbox, cache_dir)
    nodes, edges, info = osm_to_network(xml_text, max_nodes=max_nodes)
    info["bbox"] = list(bbox)
    if buildings:
        polys, binfo = extract_buildings(xml_text, info["projection"], info["road_bbox"])
        info["buildings"] = polys
        info["buildings_info"] = binfo
    return nodes, edges, info


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="OSM extract -> custom-network JSON")
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--city", choices=sorted(CITY_BBOXES))
    g.add_argument("--bbox", help="minLon,minLat,maxLon,maxLat")
    p.add_argument("--out", required=True)
    p.add_argument("--cache", default="datasets/_osmcache")
    p.add_argument("--max-nodes", type=int, default=380)
    p.add_argument("--buildings", action="store_true",
                   help="also extract building=* footprints from the same cached extract and "
                        "persist them beside the graph (consumed by radio_model=geometric)")
    a = p.parse_args(argv)
    target = a.city if a.city else [float(v) for v in a.bbox.split(",")]
    nodes, edges, info = import_city(target, a.cache, max_nodes=a.max_nodes, buildings=a.buildings)
    doc = {"nodes": nodes, "edges": edges}
    if a.buildings:
        doc["buildings"] = info["buildings"]
    with open(a.out, "w", encoding="utf-8") as fh:
        json.dump(doc, fh)
    shown = {k: v for k, v in info.items() if k != "buildings"}
    print(f"wrote {a.out}: {shown}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
