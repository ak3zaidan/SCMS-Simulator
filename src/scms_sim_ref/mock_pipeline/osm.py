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

TAG FIDELITY (opt-in, `attrs=True` / `signals=True`). The importer historically kept `highway` and
`maxspeed` and threw the rest away, so every imported one-way street became bidirectional and every
road had the engine's single global lane count. `attrs=True` now also keeps `oneway` (yes / -1 /
reversible, plus the implicit one-way of `junction=roundabout` and `highway=motorway`), `lanes`
with `lanes:forward` / `lanes:backward`, `junction=roundabout` and the presence of `turn:lanes*`;
`signals=True` keeps the `highway=traffic_signals` NODES (and pins them as graph nodes so they
survive simplification), which is what lets traffic lights be placed where the city actually has
them instead of at every intersection. Both default OFF and change neither the node/edge arrays nor
any RNG draw, so existing runs stay byte-identical. See `network_document` for the on-disk schema.

CLI:
    python -m scms_sim_ref.mock_pipeline.osm --city paris --out paris.json
    python -m scms_sim_ref.mock_pipeline.osm --city ingolstadt --buildings --out ingolstadt.json
    python -m scms_sim_ref.mock_pipeline.osm --city ingolstadt --attrs --signals --out ing.json
    python -m scms_sim_ref.mock_pipeline.osm --city ingolstadt --tag-stats
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

# --------------------------------------------------------------------------- #
# Tag fidelity: what an OSM way says beyond its class and speed limit.
# --------------------------------------------------------------------------- #
NETWORK_SCHEMA_VERSION = 2          # 1 = {nodes, edges}; 2 adds directed_edges/signal_nodes/meta

_ONEWAY_YES = {"yes", "true", "1"}
_ONEWAY_REV = {"-1", "reverse", "reversed"}
_ONEWAY_NO = {"no", "false", "0"}
_ONEWAY_REVERSIBLE = {"reversible", "alternating"}
# classes that OSM treats as one-way even with no `oneway` tag (the wiki's implicit default)
_IMPLICIT_ONEWAY = {"motorway", "motorway_link", "trunk_link"}
# way tags whose presence is counted by `extract_tag_stats` (coverage reporting, not behaviour)
TAG_KEYS = ("oneway", "lanes", "lanes:forward", "lanes:backward", "junction",
            "turn:lanes", "turn:lanes:forward", "turn:lanes:backward",
            "maxspeed", "width", "bridge", "tunnel", "layer", "access", "name")

# --------------------------------------------------------------------------- #
# PEDESTRIAN layer. Same cached extract, different question: where may a person
# WALK. Read by `extract_footways` and reported by `extract_tag_stats`.
# --------------------------------------------------------------------------- #
#: `highway=*` classes that ARE a walkable way in their own right. `footway`/`pedestrian`/`steps`
#: are unambiguous. `path` defaults to foot-legal on the OSM wiki, so it is kept unless the way says
#: otherwise. `cycleway`/`track` are NOT walkable by default and are kept only on an explicit
#: `foot=yes|designated|permissive` -- a cycleway is where the sidewalk is NOT.
PED_WAY_ALWAYS = ("footway", "pedestrian", "steps", "path")
PED_WAY_IF_FOOT = ("cycleway", "track")
#: `foot=*` values that grant / deny access. `destination` is a legal but conditional grant and is
#: counted as a grant, matching how a router treats it for a pedestrian.
_FOOT_YES = {"yes", "designated", "permissive", "destination", "official"}
_FOOT_NO = {"no", "private"}
#: `sidewalk=*` (and the `sidewalk:left|right|both=*` scheme) values -> which sides of the ROAD carry
#: a footway that is an ATTRIBUTE of the road rather than its own way. `separate` explicitly means
#: "mapped as its own way", i.e. the geometry is in the footway layer, not implied here.
SIDEWALK_KEYS = ("sidewalk", "sidewalk:left", "sidewalk:right", "sidewalk:both")
#: `crossing=*` values that mean the crossing is SIGNAL-controlled (a pedestrian phase exists).
CROSSING_SIGNALISED = {"traffic_signals", "signals"}


def _parse_oneway(tags: dict, cls: str | None = None) -> tuple[int, bool]:
    """OSM direction tags -> (direction, reversible).

    direction: 0 = drivable both ways, +1 = way order only, -1 = against way order (`oneway=-1`).
    reversible: `oneway=reversible|alternating` -- a tidal-flow lane whose direction changes with
    time of day. The simulator has no clock-dependent topology, so it is imported as bidirectional
    and FLAGGED rather than silently frozen into one direction.

    `junction=roundabout|circular` and (for `cls` in motorway/*_link) an absent tag imply one-way in
    way order, which is how the ~44% of Ingolstadt one-ways that carry no explicit `oneway=yes` are
    recovered."""
    v = (tags.get("oneway") or "").strip().lower()
    if v in _ONEWAY_REVERSIBLE:
        return 0, True
    if v in _ONEWAY_REV:
        return -1, False
    if v in _ONEWAY_YES:
        return 1, False
    if v in _ONEWAY_NO:
        return 0, False
    if (tags.get("junction") or "").strip().lower() in ("roundabout", "circular"):
        return 1, False                          # roundabouts are one-way by definition
    if cls in _IMPLICIT_ONEWAY:
        return 1, False
    return 0, False


def _parse_lanes(tags: dict, direction: int) -> tuple[int, int, bool]:
    """OSM lane tags -> (lanes_forward, lanes_backward, tagged).

    `lanes` counts BOTH directions; `lanes:forward` / `lanes:backward` split it. With only `lanes`
    on a two-way street the split is even with the odd lane going forward (the OSM wiki's own
    reading of an untagged odd count); on a one-way street every lane is forward. `tagged` is False
    when nothing was tagged and the 1-lane-per-direction default was used, so a consumer can tell a
    real single-lane street from an unmapped one."""
    def _int(key):
        raw = (tags.get(key) or "").split(";")[0].strip()
        try:
            v = int(float(raw))
        except ValueError:
            return None
        return v if 0 <= v <= 12 else None

    total, fwd, bwd = _int("lanes"), _int("lanes:forward"), _int("lanes:backward")
    tagged = any(v is not None for v in (total, fwd, bwd))
    if direction != 0:                           # one-way: everything faces the allowed direction
        n = fwd if fwd is not None else (total if total is not None else 1)
        return max(1, n), 0, tagged
    if fwd is None and bwd is None:
        if total is None:
            return 1, 1, tagged
        return max(1, total - total // 2), max(1, total // 2), tagged
    if fwd is None:
        fwd = max(1, (total - bwd)) if total is not None else 1
    if bwd is None:
        bwd = max(1, (total - fwd)) if total is not None else 1
    return max(1, fwd), max(1, bwd), tagged


def _way_attrs(tags: dict, cls: str) -> dict:
    """The per-way attribute record kept alongside a graph edge (see `network_document`)."""
    direction, reversible = _parse_oneway(tags, cls)
    fwd, bwd, tagged = _parse_lanes(tags, direction)
    a = {"class": cls, "dir": direction, "lanes_fwd": fwd, "lanes_bwd": bwd,
         "lanes_tagged": tagged}
    if reversible:
        a["reversible"] = True
    if (tags.get("junction") or "").strip().lower() in ("roundabout", "circular"):
        a["roundabout"] = True
    turns = [tags[k] for k in ("turn:lanes", "turn:lanes:forward", "turn:lanes:backward")
             if tags.get(k)]
    if turns:                                    # cheap: the raw pipe-separated spec, first match
        a["turn_lanes"] = turns[0]
    return a


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


def _frame(road_latlon: list) -> dict:
    """The ONE definition of the local equirectangular frame: origin = min(lat)/min(lon) over the
    ROAD way nodes, kx = 111320*cos(mean_lat), ky = 110540. Every layer that shares the map --
    buildings, an imported SUMO net -- must be projected with this exact tuple (see the
    `osm_to_network` docstring for what goes wrong otherwise)."""
    lats = [p[0] for p in road_latlon]
    lons = [p[1] for p in road_latlon]
    return {"lat0": min(lats), "lon0": min(lons),
            "kx": 111320.0 * math.cos(math.radians((min(lats) + max(lats)) / 2.0)),
            "ky": 110540.0}


def road_projection(xml_text: str) -> dict:
    """The projection tuple `osm_to_network` would derive from this extract, without building the
    graph. This is what an external importer (netimport.py) needs to land in the same frame."""
    root = ET.fromstring(xml_text)
    latlon = {nd.get("id"): (float(nd.get("lat")), float(nd.get("lon")))
              for nd in root.iter("node")}
    pts = []
    for way in root.iter("way"):
        tags = {t.get("k"): t.get("v") for t in way.findall("tag")}
        if tags.get("highway") not in HIGHWAY_SPEED:
            continue
        refs = [nd.get("ref") for nd in way.findall("nd") if nd.get("ref") in latlon]
        if len(refs) < 2:
            continue
        pts.extend(latlon[r] for r in refs)
    if not pts:
        raise ValueError("no drivable roads found in the OSM extract")
    return _frame(pts)


def osm_to_network(xml_text: str, max_nodes: int = 380, tol_m: float = 10.0, *,
                   attrs: bool = False, signals: bool = False) -> tuple[list, list, dict]:
    """OSM XML -> (nodes, edges, info) in custom-network form. Deterministic for a given input.

    Escalates simplification (higher tolerance, then dropping minor road classes) until the graph
    fits max_nodes; raises ValueError if even the arterial skeleton is too large for the budget.

    `attrs=True` additionally returns `info["edge_attrs"]` (one record per emitted edge: class,
    direction, per-direction lane counts, roundabout/turn-lane flags -- see `_way_attrs`) and
    `info["directed_edges"]`, the same graph expanded into DIRECTED edges honouring `oneway`.
    `signals=True` returns `info["signal_nodes"]`, the node indices carrying an OSM
    `highway=traffic_signals` tag, and PINS those OSM nodes as graph nodes so RDP simplification
    cannot dissolve a signalised junction into a curve vertex. Both flags leave `nodes`/`edges`
    unchanged except for that pinning (which only happens when `signals=True`), so the default call
    is byte-for-byte what it always was.

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
    ways = []                                    # (class, speed_mps, [node ids], attrs|None)
    for way in root.iter("way"):
        tags = {t.get("k"): t.get("v") for t in way.findall("tag")}
        cls = tags.get("highway")
        if cls not in HIGHWAY_SPEED:
            continue
        refs = [nd.get("ref") for nd in way.findall("nd") if nd.get("ref") in latlon]
        if len(refs) < 2:
            continue
        speed = _parse_maxspeed(tags.get("maxspeed")) or HIGHWAY_SPEED[cls]
        ways.append((cls, speed, refs, _way_attrs(tags, cls) if attrs else None))
    if not ways:
        raise ValueError("no drivable roads found in the OSM extract")
    # OSM nodes tagged as real traffic signals (opt-in: costs one extra tag scan over ~10^4 nodes)
    signal_refs: set[str] = set()
    if signals:
        way_refs = {r for _c, _s, refs, _a in ways for r in refs}
        for nd in root.iter("node"):
            if nd.get("id") not in way_refs:
                continue
            for t in nd.findall("tag"):
                if t.get("k") == "highway" and t.get("v") == "traffic_signals":
                    signal_refs.add(nd.get("id"))
                    break
    # local metric projection around the extract centre
    frame = _frame([latlon[r] for _, _, refs, _a in ways for r in refs])
    lat0, lon0, kx, ky = frame["lat0"], frame["lon0"], frame["kx"], frame["ky"]

    def xy(ref):
        la, lo = latlon[ref]
        return ((lo - lon0) * kx, (la - lat0) * ky)

    for attempt in range(len(_DROP_ORDER) + 1):
        dropped = set(_DROP_ORDER[:attempt])
        use = [(c, s, r, a) for c, s, r, a in ways if c not in dropped]
        tol = tol_m * (1.0 + 0.5 * attempt)      # simplify harder as we escalate
        if not use:
            break
        # graph nodes = way endpoints + nodes shared between ways
        counts: dict[str, int] = {}
        for _c, _s, refs, _a in use:
            for r in set(refs):
                counts[r] = counts.get(r, 0) + 1
        keep = {r for _c, _s, refs, _a in use for r in (refs[0], refs[-1])}
        keep |= {r for r, n in counts.items() if n >= 2}
        if signal_refs:                          # pin signalised junctions (opt-in, see docstring)
            keep |= {r for r in signal_refs if r in counts}
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

        edge_attr: dict[tuple[int, int], dict] = {}
        for _c, speed, refs, wattrs in use:
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
                            prev = edges.get(k2)
                            edges[k2] = max(prev or 0.0, speed)
                            if wattrs is not None and (prev is None or speed > prev):
                                # attrs follow the speed rule: parallel ways collapsing onto one
                                # edge keep the faster way's attributes, deterministically
                                rec = dict(wattrs)
                                # `dir` is stated in way order; re-express it on the sorted key
                                rec["dir"] = rec["dir"] if a < b else -rec["dir"]
                                if a > b:        # ... and the lane split flips with it
                                    rec["lanes_fwd"], rec["lanes_bwd"] = (rec["lanes_bwd"],
                                                                          rec["lanes_fwd"])
                                edge_attr[k2] = rec
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
            if attrs:
                kept_keys = [k for k in sorted(edges) if k[0] in remap and k[1] in remap]
                info["edge_attrs"] = [edge_attr.get(k, {}) for k in kept_keys]
                info["directed_edges"] = _directed_from_attrs(out_edges, info["edge_attrs"])
                info["oneway_edges"] = sum(1 for a in info["edge_attrs"] if a.get("dir"))
            if signals:
                sig = []
                for r in sorted(signal_refs, key=lambda s: (len(s), s)):
                    k = idx.get((round(xy(r)[0], 1), round(xy(r)[1], 1)))
                    if k is not None and k in remap:
                        sig.append(remap[k])
                info["signal_nodes"] = sorted(set(sig))
                info["signal_refs_found"] = len(signal_refs)
            return out_nodes, out_edges, info
    raise ValueError(f"extract still exceeds {max_nodes} nodes after dropping "
                     f"{_DROP_ORDER}; use a smaller bbox")


def _directed_from_attrs(edges: list, edge_attrs: list) -> list:
    """[[a,b,speed], ...] + per-edge attrs -> directed edge records honouring `oneway`.

    A two-way street yields both (a->b) and (b->a); a one-way yields only the permitted direction,
    which is what removes the head-on overlap the undirected graph creates (opposing streams share
    one centreline and pass through each other). Lane counts are per direction."""
    out = []
    for (a, b, *rest), rec in zip(edges, edge_attrs):
        sp = float(rest[0]) if rest else None
        d = int(rec.get("dir", 0) or 0)
        fwd = int(rec.get("lanes_fwd", 1) or 0)
        bwd = int(rec.get("lanes_bwd", 1) or 0)
        cls = rec.get("class")
        if d >= 0:
            e = {"a": a, "b": b, "speed_mps": sp, "lanes": max(1, fwd)}
            if cls:
                e["class"] = cls
            if rec.get("roundabout"):
                e["roundabout"] = True
            out.append(e)
        if d <= 0:
            e = {"a": b, "b": a, "speed_mps": sp, "lanes": max(1, bwd)}
            if cls:
                e["class"] = cls
            if rec.get("roundabout"):
                e["roundabout"] = True
            out.append(e)
    return out


def extract_tag_stats(xml_text: str) -> dict:
    """Tag coverage of the DRIVABLE ways in an extract -- what fraction carries each tag.

    Pure measurement (no graph is built): this is how you answer "is it worth importing `lanes` for
    this city?" before wiring anything, and it is the check that the tags the importer now reads are
    actually present in the cached extract rather than assumed."""
    root = ET.fromstring(xml_text)
    counts: dict[str, int] = {k: 0 for k in TAG_KEYS}
    oneway_vals: dict[str, int] = {}
    lane_vals: dict[str, int] = {}
    classes: dict[str, int] = {}
    drivable = 0
    implicit_oneway = 0
    explicit_oneway = 0
    roundabouts = 0
    turn_lanes = 0
    way_refs: set[str] = set()
    for way in root.iter("way"):
        tags = {t.get("k"): t.get("v") for t in way.findall("tag")}
        cls = tags.get("highway")
        if cls not in HIGHWAY_SPEED:
            continue
        drivable += 1
        classes[cls] = classes.get(cls, 0) + 1
        for nd in way.findall("nd"):
            way_refs.add(nd.get("ref"))
        for k in TAG_KEYS:
            if tags.get(k):
                counts[k] += 1
        if tags.get("oneway"):
            oneway_vals[tags["oneway"]] = oneway_vals.get(tags["oneway"], 0) + 1
        if tags.get("lanes"):
            lane_vals[tags["lanes"]] = lane_vals.get(tags["lanes"], 0) + 1
        d, _rev = _parse_oneway(tags, cls)
        if d:
            explicit_oneway += 1 if tags.get("oneway") else 0
            implicit_oneway += 0 if tags.get("oneway") else 1
        if (tags.get("junction") or "").lower() in ("roundabout", "circular"):
            roundabouts += 1
        if any(k.startswith("turn:lanes") and tags.get(k) for k in tags):
            turn_lanes += 1
    signal_nodes = 0
    signal_nodes_on_road = 0
    stop_nodes = 0
    give_way_nodes = 0
    for nd in root.iter("node"):
        for t in nd.findall("tag"):
            if t.get("k") != "highway":
                continue
            v = t.get("v")
            if v == "traffic_signals":
                signal_nodes += 1
                if nd.get("id") in way_refs:
                    signal_nodes_on_road += 1
            elif v == "stop":
                stop_nodes += 1
            elif v == "give_way":
                give_way_nodes += 1
    restrictions: dict[str, int] = {}
    for rel in root.iter("relation"):
        tags = {t.get("k"): t.get("v") for t in rel.findall("tag")}
        if tags.get("type") == "restriction":
            key = tags.get("restriction", "?")
            restrictions[key] = restrictions.get(key, 0) + 1
    den = max(1, drivable)
    return {"drivable_ways": drivable,
            "tag_counts": {k: v for k, v in counts.items() if v},
            "tag_fraction": {k: round(v / den, 4) for k, v in counts.items() if v},
            "oneway_values": dict(sorted(oneway_vals.items())),
            "oneway_ways_explicit": explicit_oneway, "oneway_ways_implicit": implicit_oneway,
            "oneway_share": round((explicit_oneway + implicit_oneway) / den, 4),
            "lanes_values": dict(sorted(lane_vals.items())),
            "roundabout_ways": roundabouts, "turn_lane_ways": turn_lanes,
            "highway_classes": dict(sorted(classes.items())),
            "traffic_signal_nodes": signal_nodes,
            "traffic_signal_nodes_on_drivable_way": signal_nodes_on_road,
            "stop_nodes": stop_nodes, "give_way_nodes": give_way_nodes,
            "restriction_relations": sum(restrictions.values()),
            "restriction_kinds": dict(sorted(restrictions.items())),
            # PEDESTRIAN coverage (additive: a reader that predates this key sees what it always
            # saw). Answers "can the sidewalk layer be READ from this map, or must it be DERIVED?"
            "pedestrian": pedestrian_tag_stats(xml_text)}


def network_document(nodes: list, edges: list, info: dict | None = None, *,
                     buildings: list | None = None) -> dict:
    """Assemble the on-disk custom-network document. SUPERSET of the legacy `{nodes, edges}`.

        {"nodes": [[x, y], ...],                      metres, local frame (see `osm_to_network`)
         "edges": [[a, b, speed_mps], ...],           UNDIRECTED, as before -- every consumer that
                                                      predates this schema keeps working unchanged
         "directed_edges": [{"a", "b", "speed_mps", "lanes", "class"?, "roundabout"?,
                             "shape"?, "turns"?, "id"?, "length_m"?}, ...],
         "signal_nodes": [i, ...],                    node indices that are REAL traffic signals
         "network_meta": {...}}                       provenance/measurements, never behaviour

    One record per LEGAL DIRECTION: a two-way street appears twice (once per direction, each with
    its own lane count), a one-way street once -- which is what `roads.edges_from_directed` expects.
    `lanes` is per direction. `shape` follows `roads.parse_edge_spec`: INTERMEDIATE curve vertices
    only, in a->b order, with the two junction coordinates implied; the two directions of one
    physical road must carry mirror-image shapes (see `netimport._canonicalise_shapes`).

    The undirected `edges` array stays authoritative for today's engine; `directed_edges` carries
    the one-way/lane fidelity for a directed consumer. A one-way street appears ONCE in
    `directed_edges` and still once in `edges`, so a reader that ignores the new keys sees exactly
    the old (bidirectional) graph rather than a subtly different one."""
    doc: dict = {"nodes": nodes, "edges": edges}
    info = info or {}
    for key in ("directed_edges", "signal_nodes"):
        if info.get(key):
            doc[key] = info[key]
    if buildings:
        doc["buildings"] = buildings
    meta = info.get("network_meta")
    if meta:
        doc["network_meta"] = dict(meta, schema=NETWORK_SCHEMA_VERSION)
    return doc


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


def _foot_allowed(tags: dict, cls: str) -> bool:
    """Is this `highway=<cls>` way walkable? (see PED_WAY_ALWAYS / PED_WAY_IF_FOOT)"""
    foot = (tags.get("foot") or "").strip().lower()
    if foot in _FOOT_NO:
        return False
    if (tags.get("access") or "").strip().lower() in _FOOT_NO and foot not in _FOOT_YES:
        return False
    if cls in PED_WAY_ALWAYS:
        return True
    return cls in PED_WAY_IF_FOOT and foot in _FOOT_YES


def _sidewalk_sides(tags: dict) -> tuple[frozenset, str]:
    """OSM `sidewalk*` tags -> (sides present as an ATTRIBUTE of the road, raw verdict).

    Sides are `{"left", "right"}` in WAY ORDER (OSM's own convention). The verdict distinguishes the
    three cases a consumer must treat differently and which a naive read conflates:

      * `"none"`   -- the road says it has no footway (`sidewalk=no`): build none.
      * `"separate"` -- the footway EXISTS but is mapped as its own way: its geometry belongs to the
        footway layer, so implying a sidewalk from the road as well would double-count it.
      * `"both"` / `"left"` / `"right"` -- an attribute footway on those sides.
      * `"untagged"` -- the road says nothing at all, which is the majority case on most extracts
        and the one a derived-geometry fallback exists for.
    """
    vals = {k: (tags.get(k) or "").strip().lower() for k in SIDEWALK_KEYS}
    if not any(vals.values()):
        return frozenset(), "untagged"
    sides = set()
    verdicts = set()
    main = vals["sidewalk"]
    if main:
        if main in ("both", "yes"):
            sides |= {"left", "right"}
        elif main in ("left", "right"):
            sides.add(main)
        elif main == "separate":
            verdicts.add("separate")
        elif main in ("no", "none"):
            verdicts.add("none")
    if vals["sidewalk:both"] in ("yes", "both"):
        sides |= {"left", "right"}
    elif vals["sidewalk:both"] == "separate":
        verdicts.add("separate")
    for s in ("left", "right"):
        v = vals[f"sidewalk:{s}"]
        if v in ("yes", s, "both"):
            sides.add(s)
        elif v == "separate":
            verdicts.add("separate")
        elif v in ("no", "none"):
            verdicts.add("none")
    if sides:
        return frozenset(sides), ("both" if sides == {"left", "right"} else sorted(sides)[0])
    if "separate" in verdicts:
        return frozenset(), "separate"
    if "none" in verdicts:
        return frozenset(), "none"
    return frozenset(), "untagged"


def pedestrian_tag_stats(xml_text: str) -> dict:
    """Coverage of the PEDESTRIAN tags in an extract. Pure measurement, no geometry, no graph.

    This is the number that decides whether a sidewalk layer can be READ from the map or has to be
    DERIVED from road geometry: `sidewalk_verdict` over the drivable ways says how many roads state
    anything at all about their footway, and `ped_way_classes` says how much separately-mapped
    footway geometry the same extract carries.
    """
    root = ET.fromstring(xml_text)
    drivable = 0
    verdicts: dict[str, int] = {}
    sidewalk_vals: dict[str, int] = {}
    ped_ways: dict[str, int] = {}
    ped_rejected: dict[str, int] = {}
    ped_vertices = 0
    footway_role: dict[str, int] = {}
    way_refs: set[str] = set()
    for way in root.iter("way"):
        tags = {t.get("k"): t.get("v") for t in way.findall("tag")}
        cls = tags.get("highway")
        if cls in HIGHWAY_SPEED:
            drivable += 1
            for nd in way.findall("nd"):
                way_refs.add(nd.get("ref"))
            _sides, verdict = _sidewalk_sides(tags)
            verdicts[verdict] = verdicts.get(verdict, 0) + 1
            for k in SIDEWALK_KEYS:
                if tags.get(k):
                    key = f"{k}={tags[k]}"
                    sidewalk_vals[key] = sidewalk_vals.get(key, 0) + 1
        elif cls in PED_WAY_ALWAYS or cls in PED_WAY_IF_FOOT:
            if _foot_allowed(tags, cls):
                ped_ways[cls] = ped_ways.get(cls, 0) + 1
                ped_vertices += len(way.findall("nd"))
                role = (tags.get("footway") or "").strip().lower()
                if role:
                    footway_role[role] = footway_role.get(role, 0) + 1
            else:
                ped_rejected[cls] = ped_rejected.get(cls, 0) + 1
    crossings = 0
    crossing_kinds: dict[str, int] = {}
    crossings_on_road = 0
    for nd in root.iter("node"):
        tags = {t.get("k"): t.get("v") for t in nd.findall("tag")}
        if tags.get("highway") != "crossing":
            continue
        crossings += 1
        if nd.get("id") in way_refs:
            crossings_on_road += 1
        kind = (tags.get("crossing") or tags.get("crossing:markings") or "?").strip().lower()
        crossing_kinds[kind] = crossing_kinds.get(kind, 0) + 1
    den = max(1, drivable)
    tagged = drivable - verdicts.get("untagged", 0)
    return {"drivable_ways": drivable,
            "sidewalk_tagged_ways": tagged,
            "sidewalk_tagged_share": round(tagged / den, 4),
            "sidewalk_verdict": dict(sorted(verdicts.items())),
            "sidewalk_verdict_share": {k: round(v / den, 4) for k, v in sorted(verdicts.items())},
            "sidewalk_values": dict(sorted(sidewalk_vals.items())),
            "ped_way_classes": dict(sorted(ped_ways.items())),
            "ped_ways": sum(ped_ways.values()),
            "ped_way_vertices": ped_vertices,
            "ped_ways_rejected": dict(sorted(ped_rejected.items())),
            "footway_role": dict(sorted(footway_role.items())),
            "crossing_nodes": crossings,
            "crossing_nodes_on_drivable_way": crossings_on_road,
            "crossing_kinds": dict(sorted(crossing_kinds.items())),
            "crossing_nodes_signalised": sum(v for k, v in crossing_kinds.items()
                                             if k in CROSSING_SIGNALISED)}


def extract_footways(xml_text: str, projection: dict, road_bbox: list | None = None, *,
                     margin_m: float = 250.0, simplify_tol_m: float = 1.0,
                     min_length_m: float = 3.0, max_ways: int = 20000) -> tuple[list, list, dict]:
    """Walkable ways + crossing nodes from the SAME cached Overpass XML -> projected metres.

    Returns `(footways, crossings, info)`:

      * `footways` -- open polylines `[[x, y], ...]`, one per walkable way (see `_foot_allowed`),
        RDP-simplified to `simplify_tol_m` and dropped below `min_length_m`. These are REAL mapped
        pedestrian surface, which is what makes them worth having: the derived sidewalk layer in
        `vru.py` can only invent a footway beside a road, and roughly two thirds of the tagged
        Ingolstadt roads say `sidewalk=separate` -- the footway exists, but only as its own way.
      * `crossings` -- `[x, y, kind]` per `highway=crossing` node, `kind` from `crossing=*`
        (`traffic_signals` => a real pedestrian phase, everything else => uncontrolled).
      * `info` -- provenance, including the `pedestrian_tag_stats` coverage block.

    `projection` MUST be `osm_to_network`'s `info["projection"]` for this exact XML, for the same
    reason `extract_buildings` insists on it: the road origin is min(lat)/min(lon) over ROAD ways
    only, so a re-derived origin slides the whole pedestrian layer off the streets while every
    individual footway still looks perfectly plausible. The same alignment GATE is applied here.
    """
    lat0 = float(projection["lat0"])
    lon0 = float(projection["lon0"])
    kx = float(projection["kx"])
    ky = float(projection["ky"])
    root = ET.fromstring(xml_text)
    latlon: dict[str, tuple[float, float]] = {}
    for nd in root.iter("node"):
        latlon[nd.get("id")] = (float(nd.get("lat")), float(nd.get("lon")))

    raw = incomplete = short = 0
    ways: list[list] = []
    for way in root.iter("way"):
        tags = {t.get("k"): t.get("v") for t in way.findall("tag")}
        cls = tags.get("highway")
        if cls not in PED_WAY_ALWAYS and cls not in PED_WAY_IF_FOOT:
            continue
        if not _foot_allowed(tags, cls):
            continue
        raw += 1
        refs = [nd.get("ref") for nd in way.findall("nd")]
        if len(refs) < 2 or any(r not in latlon for r in refs):
            incomplete += 1              # Overpass clips ways at the bbox edge: drop, never guess
            continue
        pts = []
        for r in refs:
            la, lo = latlon[r]
            pts.append([(lo - lon0) * kx, (la - lat0) * ky])
        length = sum(math.dist(p, q) for p, q in zip(pts, pts[1:]))
        if length < min_length_m:
            short += 1
            continue
        if simplify_tol_m > 0 and len(pts) > 2:
            pts = [list(p) for p in _rdp(pts, simplify_tol_m)]
        ways.append(pts)

    crossings: list[list] = []
    for nd in root.iter("node"):
        tags = {t.get("k"): t.get("v") for t in nd.findall("tag")}
        if tags.get("highway") != "crossing":
            continue
        la, lo = latlon[nd.get("id")]
        kind = (tags.get("crossing") or "").strip().lower()
        crossings.append([(lo - lon0) * kx, (la - lat0) * ky,
                          "traffic_signals" if kind in CROSSING_SIGNALISED else (kind or "unknown")])

    info = {"ped_ways_raw": raw, "incomplete": incomplete, "dropped_below_min_length": short,
            "projection": {"lat0": lat0, "lon0": lon0, "kx": kx, "ky": ky},
            "tag_coverage": pedestrian_tag_stats(xml_text)}
    if not ways:
        info.update(footways=0, crossings=len(crossings))
        return [], crossings, info

    if road_bbox is not None:
        rx0, ry0, rx1, ry1 = (float(v) for v in road_bbox)
        cents = [(sum(p[0] for p in w) / len(w), sum(p[1] for p in w) / len(w)) for w in ways]
        cxs = sorted(c[0] for c in cents)
        cys = sorted(c[1] for c in cents)
        med = (cxs[len(cxs) // 2], cys[len(cys) // 2])
        # ALIGNMENT GATE -- identical in spirit to `extract_buildings`. A footway layer projected
        # with a re-derived origin lands hundreds of metres off the roads it is supposed to parallel,
        # and every individual polyline still looks like a footway. This is the only place that
        # mistake is observable before it silently defines where pedestrians "legally" walk.
        diag = math.hypot(rx1 - rx0, ry1 - ry0)
        tol = max(margin_m, 0.5 * diag)
        info["median_footway_centroid"] = [round(med[0], 2), round(med[1], 2)]
        info["alignment_tolerance_m"] = round(tol, 1)
        if not (rx0 - tol <= med[0] <= rx1 + tol and ry0 - tol <= med[1] <= ry1 + tol):
            raise ValueError(
                f"projected footways do not sit on the road network: median centroid "
                f"{info['median_footway_centroid']} is outside the road bbox {[rx0, ry0, rx1, ry1]} "
                f"widened by {tol:.0f} m. This is the projection trap -- the pedestrian layer must "
                f"use the SAME (lat0, lon0, kx, ky) that osm_to_network derived from the ROAD ways "
                f"(note ky = 110540.0, not 111320), never a re-derived origin.")
        lo_x, lo_y, hi_x, hi_y = rx0 - margin_m, ry0 - margin_m, rx1 + margin_m, ry1 + margin_m
        keep, inside = [], 0
        for w, (cx, cy) in zip(ways, cents):
            if rx0 <= cx <= rx1 and ry0 <= cy <= ry1:
                inside += 1
            if lo_x <= cx <= hi_x and lo_y <= cy <= hi_y:
                keep.append(w)
        info["centroid_inside_road_bbox_frac"] = round(inside / len(ways), 4)
        ways = keep
        crossings = [c for c in crossings if lo_x <= c[0] <= hi_x and lo_y <= c[1] <= hi_y]

    ways.sort(key=lambda w: (round(w[0][0], 3), round(w[0][1], 3), len(w)))
    if len(ways) > max_ways:
        info["truncated_from"] = len(ways)
        ways = ways[:max_ways]
    out = [[[round(x, 2), round(y, 2)] for x, y in w] for w in ways]
    crossings.sort(key=lambda c: (round(c[0], 3), round(c[1], 3)))
    out_cross = [[round(c[0], 2), round(c[1], 2), c[2]] for c in crossings]
    info["footways"] = len(out)
    info["footway_vertices"] = sum(len(w) for w in out)
    info["footway_total_m"] = round(sum(math.dist(p, q) for w in out for p, q in zip(w, w[1:])), 1)
    info["crossings"] = len(out_cross)
    info["crossings_signalised"] = sum(1 for c in out_cross if c[2] == "traffic_signals")
    return out, out_cross, info


def osm_cache_path(bbox: tuple, cache_dir: str) -> str:
    """Where `fetch_osm` keeps the raw extract for this bbox (a stable sha256 of the rounded bbox).
    Exposed so another importer -- netconvert -- can consume the SAME file instead of refetching."""
    key = hashlib.sha256(",".join(f"{b:.5f}" for b in bbox).encode()).hexdigest()[:16]
    return os.path.join(cache_dir, f"osm_{key}.xml")


def fetch_osm(bbox: tuple, cache_dir: str) -> str:
    """Download (or reuse a cached) raw OSM XML extract for bbox = (minLon,minLat,maxLon,maxLat)."""
    os.makedirs(cache_dir, exist_ok=True)
    cached = osm_cache_path(bbox, cache_dir)
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
                buildings: bool = False, *, attrs: bool = False,
                signals: bool = False, footways: bool = False) -> tuple[list, list, dict]:
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
    nodes, edges, info = osm_to_network(xml_text, max_nodes=max_nodes, attrs=attrs, signals=signals)
    info["bbox"] = list(bbox)
    if attrs or signals:
        info["network_meta"] = {"source": "osm", "bbox": list(bbox),
                                "projection": info["projection"],
                                "road_bbox": info["road_bbox"],
                                "directed": bool(attrs),
                                "signal_nodes": len(info.get("signal_nodes", []))}
    if buildings:
        polys, binfo = extract_buildings(xml_text, info["projection"], info["road_bbox"])
        info["buildings"] = polys
        info["buildings_info"] = binfo
    if footways:
        fw, cross, finfo = extract_footways(xml_text, info["projection"], info["road_bbox"])
        info["footways"] = fw
        info["crossings"] = cross
        info["footways_info"] = finfo
    return nodes, edges, info


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="OSM extract -> custom-network JSON")
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--city", choices=sorted(CITY_BBOXES))
    g.add_argument("--bbox", help="minLon,minLat,maxLon,maxLat")
    p.add_argument("--out")
    p.add_argument("--cache", default="datasets/_osmcache")
    p.add_argument("--max-nodes", type=int, default=380)
    p.add_argument("--buildings", action="store_true",
                   help="also extract building=* footprints from the same cached extract and "
                        "persist them beside the graph (consumed by radio_model=geometric)")
    p.add_argument("--attrs", action="store_true",
                   help="keep oneway/lanes/roundabout/turn-lane tags -> directed_edges in the "
                        "output document (undirected edges stay for backward compatibility)")
    p.add_argument("--signals", action="store_true",
                   help="keep highway=traffic_signals nodes -> signal_nodes (real signal "
                        "placement instead of signalising every intersection)")
    p.add_argument("--footways", action="store_true",
                   help="also extract walkable ways (highway=footway/pedestrian/steps/path) and "
                        "highway=crossing nodes from the same cached extract -> footways/crossings "
                        "in the output document (consumed by vru.py's sidewalk layer)")
    p.add_argument("--tag-stats", action="store_true",
                   help="print tag coverage of the cached extract and exit (no graph is built)")
    p.add_argument("--ped-stats", action="store_true",
                   help="print PEDESTRIAN tag coverage (sidewalk=*/footway ways/crossing nodes) "
                        "of the cached extract and exit (no graph is built)")
    a = p.parse_args(argv)
    target = a.city if a.city else [float(v) for v in a.bbox.split(",")]
    if a.tag_stats or a.ped_stats:
        bbox = CITY_BBOXES[target] if isinstance(target, str) else tuple(target)
        fn = pedestrian_tag_stats if a.ped_stats else extract_tag_stats
        print(json.dumps(fn(fetch_osm(bbox, a.cache)), indent=1, sort_keys=True))
        return 0
    if not a.out:
        p.error("--out is required (or use --tag-stats / --ped-stats)")
    nodes, edges, info = import_city(target, a.cache, max_nodes=a.max_nodes, buildings=a.buildings,
                                     attrs=a.attrs, signals=a.signals, footways=a.footways)
    doc = network_document(nodes, edges, info,
                           buildings=info.get("buildings") if a.buildings else None)
    if a.footways:
        doc["footways"] = info["footways"]
        doc["crossings"] = info["crossings"]
    with open(a.out, "w", encoding="utf-8") as fh:
        json.dump(doc, fh)
    shown = {k: v for k, v in info.items()
             if k not in ("buildings", "directed_edges", "edge_attrs", "footways", "crossings")}
    print(f"wrote {a.out}: {shown}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
