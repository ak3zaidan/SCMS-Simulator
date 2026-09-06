"""SUMO ``.net.xml`` -> engine custom-network importer (the highest-fidelity road-import path).

WHY THIS EXISTS. ``osm.py`` parses raw OpenStreetMap itself: it keeps a handful of tags, promotes
every surviving RDP curve vertex to a graph "intersection", and squeezes the result under a ~380
node budget by dropping minor road classes. netconvert has already solved that problem properly --
it resolves OSM into a validated topology with DIRECTED edges, real lane counts, junction geometry,
turn connections and traffic-light programs, and its ``--geometry.remove`` folds curve vertices into
edge SHAPE instead of spending graph nodes on them. This module inherits that work rather than
reimplementing it: read the ``.net.xml`` with ``sumolib``, emit the engine's custom-network
document. MEASURED on the cached Ingolstadt extract: the same 1.5 km^2 of city becomes 217
junctions of which 123 have degree >= 3, against the raw path's 341 nodes for 126 real
intersections (187 of its nodes are degree-2 curve vertices); 27.9% of directed edges are one-way,
none of which the pre-tag importer could represent at all; lane counts spread 1/2/3 instead of one
global n_lanes; and the two importers agree on total lane-km to 0.3%.

THE PROJECTION TRAP (the one thing that must not be got wrong). A netconvert net built from OSM is
geo-referenced in UTM with an arbitrary offset; ``osm.py`` projects roads and buildings into a LOCAL
equirectangular frame whose origin is ``min(lat) / min(lon)`` over the ROAD ways only, with
``kx = 111320*cos(mean_lat)``, ``ky = 110540``. Anything landing in that frame with a different
origin or scale is silently misregistered -- the geometric radio channel's LOS/NLOS tests against
building footprints would then be computed against a city translated by hundreds of metres while
every individual polyline still looks perfectly plausible. So this importer takes the road graph's
own ``projection`` tuple, inverts the net's UTM back to lon/lat, re-projects into that exact frame,
and GATES on the result landing inside the expected bbox (`_assert_alignment`).

``pyproj`` is not installed in this toolchain, so ``net.convertXY2LonLat`` raises; the inverse UTM
here is the standard Snyder series, verified against the OSM node coordinates the net was built
from (median error 5 mm, max 1 cm over the 206 junctions whose SUMO id is still their OSM node id).

SCHEMA. The output is `osm.network_document`: the legacy `{nodes, edges}` (undirected, unchanged
for every existing consumer) plus `directed_edges` -- one record per legal direction of travel,
carrying that direction's lane count, the posted speed and the edge's curve geometry -- and
`signal_nodes`. With ``--signals`` it also carries ``signal_programs``: the REAL ``<tlLogic>`` phase
programs -- state strings, phase durations, minDur/maxDur, offset -- plus the
``(from junction, to junction) -> phase-index`` mapping that makes a program usable by a vehicle.
See ``signals.py``. MEASURED end to end on InTAS with ``--signals --strong``: 98 programs on 98
graph junctions (of 109 typed ``traffic_light``; the other 11 are pedestrian clusters with no
``<tlLogic>`` at all), 1030 of 1042 controlled connections mapped to a movement, 778 movements over
330 approaches, 1015 of 1032 state columns addressable, 0 junction-coordinate collisions, cycle
median 90 s.
`roads.edges_from_directed` turns those records into engine edge specs, so a
netconvert import loads into `CustomNetwork` directly. Two conventions are load-bearing there:
`shape` holds INTERMEDIATE vertices only (junction coordinates implied), and the two directions of
one physical road must carry mirror-image shapes -- netconvert does NOT (each carriageway has its
own offset polyline), so `_canonicalise_shapes` picks one. Pass `strong=True` for a directed
consumer: a bbox-clipped extract otherwise leaves junctions a vehicle can enter and never leave.

THE WHOLE CITY, AND ITS BUILDINGS. The raw-OSM path cannot carry a city: it RDP-simplifies at 10 m
(measured: 10.35% of its road length ends up inside its own building footprints) and drops minor
road classes to fit a ~380 node budget. This path has neither problem -- InTAS imports 3332
junctions / 7941 edges against `CustomNetwork`'s 4000 / 12000 caps -- so the FULL 66 km^2 city is a
first-class route rather than a 2 km^2 extract. `buildings_from_poly` / `scene_from_net` bring the
footprints across in the SAME frame (see THE PROJECTION TRAP above): a SUMO `.poly.xml` is written
in the net's own metric coordinates, so it is transformed by the very `_transformer` closure the
junctions went through, and then GATED three ways -- median centroid against the median JUNCTION,
the fraction of centroids inside the road bbox, and the fraction of ROAD JUNCTIONS that land inside
a footprint. That last arm is the sharp one: a translated footprint layer pushes junctions into
walls. MEASURED on InTAS by displacing the real 21,717-footprint layer and bisecting: the gate fires
from 10.6-12.6 m in every direction, and nothing in 0-20 km passes (docs/realism/FULL-CITY-SCENE.md
carries the table). MEASURED end to end: the whole city with its buildings reads LOS 0.450 /
NLOSv 0.303 / NLOSb 0.247 at 200 m against the 2 km^2 extract's 0.060 / 0.031 / 0.909, closes the
whole 84.8% SCENE term of CROSS-ENGINE-RADIO.md, and runs 4.1x FASTER than the extract because cost
follows vehicle density rather than map area.

CLI:
    python -m scms_sim_ref.mock_pipeline.netimport --city ingolstadt --out ing_net.json
    python -m scms_sim_ref.mock_pipeline.netimport --city ingolstadt --strong --turns --out d.json
    python -m scms_sim_ref.mock_pipeline.netimport --net some.net.xml --out net.json --no-geo
    python -m scms_sim_ref.mock_pipeline.netimport --net ingolstadt.net.xml --signals --strong \
        --no-geo --out intas.json
    python -m scms_sim_ref.mock_pipeline.netimport --net ingolstadt.net.xml --strong --no-geo \
        --buildings buildings.poly.xml --out intas_scene.json      # the whole city + footprints
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

from .osm import (CITY_BBOXES, _DROP_ORDER, NETWORK_SCHEMA_VERSION, FootprintIndex, _ring_area,
                  fetch_osm, network_document, osm_cache_path, road_projection)

# netconvert flag set for OSM input. NOTE these are netconvert-ONLY options: netgenerate rejects
# --tls.guess-signals / --ramps.guess / --junctions.join (it takes --tls.guess/--tls.join instead),
# which is a real trap when the same flag list is shared between procedural and OSM map generation.
NETCONVERT_OSM_ARGS = (
    "--geometry.remove",            # fold degree-2 curve nodes into edge shape (not graph nodes)
    "--roundabouts.guess",
    "--ramps.guess",
    "--junctions.join",             # one junction per real intersection, not one per carriageway
    "--tls.guess-signals",          # OSM traffic_signals nodes -> a tls at the junction they guard
    "--tls.discard-simple",         # ... but not on 2-leg nodes (pedestrian crossings)
    "--tls.join",
    "--keep-edges.by-vclass", "passenger",
    "--remove-edges.isolated",
    "--no-turnarounds", "true",
)
_TYPEMAP = "data/typemap/osmNetconvert.typ.xml"

# WGS84 / UTM constants for the inverse projection
_A = 6378137.0
_F = 1.0 / 298.257223563
_E2 = _F * (2.0 - _F)
_K0 = 0.9996


# --------------------------------------------------------------------------- #
# netconvert driver
# --------------------------------------------------------------------------- #
def netconvert_binary() -> str | None:
    """Path to netconvert (PATH, then $SUMO_HOME/bin), or None if SUMO is not installed."""
    found = shutil.which("netconvert")
    if found:
        return found
    home = os.environ.get("SUMO_HOME")
    if home:
        for cand in (os.path.join(home, "bin", "netconvert.exe"),
                     os.path.join(home, "bin", "netconvert")):
            if os.path.exists(cand):
                return cand
    return None


def _check_sumo_path(path: str, what: str) -> str:
    """SUMO tools embed their own command line -- including every path -- in an XML comment of the
    file they write, and `--` is ILLEGAL inside an XML comment. A path containing `--` therefore
    produces a corrupt or empty output file with no error, so refuse it loudly instead."""
    if "--" in os.path.abspath(path):
        raise ValueError(f"{what} path contains '--': {path!r}. SUMO writes its command line into "
                         f"an XML comment and '--' cannot appear there, so netconvert would emit a "
                         f"broken file. Use a directory whose name has no double dash.")
    return path


def run_netconvert(osm_path: str, out_net: str, *, args=NETCONVERT_OSM_ARGS, extra_args=(),
                   typemap: bool = True, overwrite: bool = False, timeout: float = 900.0) -> str:
    """Run netconvert on a raw OSM XML extract -> `.net.xml`. Cached: an existing non-empty output
    is reused unless `overwrite`. Returns the net path.

    `args` REPLACES the default flag set and `extra_args` appends to it -- netconvert rejects a
    repeated option outright ("A value for the option 'x' was already set"), so a variant flips a
    default by passing a modified `args`, never by appending an override."""
    _check_sumo_path(out_net, "netconvert output")
    if os.path.exists(out_net) and os.path.getsize(out_net) > 1000 and not overwrite:
        return out_net
    exe = netconvert_binary()
    if exe is None:
        raise RuntimeError("netconvert not found: install SUMO and set SUMO_HOME (this importer "
                           "needs it once per extract; the resulting .net.xml is then cached)")
    cmd = [exe, "--osm-files", os.path.abspath(osm_path), "-o", os.path.abspath(out_net)]
    home = os.environ.get("SUMO_HOME")
    if typemap and home and os.path.exists(os.path.join(home, _TYPEMAP)):
        cmd += ["--type-files", os.path.join(home, _TYPEMAP)]
    cmd += [str(a) for a in args] + [str(a) for a in extra_args]
    os.makedirs(os.path.dirname(os.path.abspath(out_net)) or ".", exist_ok=True)
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if proc.returncode != 0 or not os.path.exists(out_net):
        tail = (proc.stderr or proc.stdout or "").strip().splitlines()[-8:]
        raise RuntimeError(f"netconvert failed ({proc.returncode}):\n" + "\n".join(tail))
    return out_net


# --------------------------------------------------------------------------- #
# geo-referencing
# --------------------------------------------------------------------------- #
def _utm_zone(proj_param: str) -> tuple[int, bool] | None:
    """('+proj=utm +zone=32 +ellps=WGS84 ...') -> (zone, northern). None if it is not plain UTM."""
    if "+proj=utm" not in proj_param:
        return None
    zone = None
    for tok in proj_param.split():
        if tok.startswith("+zone="):
            try:
                zone = int(tok.split("=", 1)[1])
            except ValueError:
                return None
    if zone is None:
        return None
    return zone, "+south" not in proj_param


def inverse_utm(x: float, y: float, zone: int, northern: bool = True) -> tuple[float, float]:
    """UTM easting/northing (WGS84) -> (lon, lat) in degrees. Snyder's series inverse -- exact to
    millimetres inside a zone, and self-contained so the import does not need pyproj."""
    x = x - 500000.0
    if not northern:
        y -= 10000000.0
    e1 = (1.0 - math.sqrt(1.0 - _E2)) / (1.0 + math.sqrt(1.0 - _E2))
    ep2 = _E2 / (1.0 - _E2)
    m = y / _K0
    mu = m / (_A * (1 - _E2 / 4 - 3 * _E2 ** 2 / 64 - 5 * _E2 ** 3 / 256))
    p1 = (mu + (3 * e1 / 2 - 27 * e1 ** 3 / 32) * math.sin(2 * mu)
          + (21 * e1 ** 2 / 16 - 55 * e1 ** 4 / 32) * math.sin(4 * mu)
          + (151 * e1 ** 3 / 96) * math.sin(6 * mu)
          + (1097 * e1 ** 4 / 512) * math.sin(8 * mu))
    c1 = ep2 * math.cos(p1) ** 2
    t1 = math.tan(p1) ** 2
    n1 = _A / math.sqrt(1 - _E2 * math.sin(p1) ** 2)
    r1 = _A * (1 - _E2) / (1 - _E2 * math.sin(p1) ** 2) ** 1.5
    d = x / (n1 * _K0)
    lat = p1 - (n1 * math.tan(p1) / r1) * (
        d ** 2 / 2
        - (5 + 3 * t1 + 10 * c1 - 4 * c1 ** 2 - 9 * ep2) * d ** 4 / 24
        + (61 + 90 * t1 + 298 * c1 + 45 * t1 ** 2 - 252 * ep2 - 3 * c1 ** 2) * d ** 6 / 720)
    lon = ((d - (1 + 2 * t1 + c1) * d ** 3 / 6
            + (5 - 2 * c1 + 28 * t1 - 3 * c1 ** 2 + 8 * ep2 + 24 * t1 ** 2) * d ** 5 / 120)
           / math.cos(p1))
    return math.degrees(lon) + ((zone - 1) * 6 - 180 + 3), math.degrees(lat)


#: `osm._frame`'s meridional constant. Restated here so the check below is an INDEPENDENT statement
#: about the frame it is handed rather than a comparison of a value with itself.
FRAME_KY = 110540.0
#: `osm._frame`'s equatorial constant: kx = FRAME_KX * cos(mean_lat), so 0 < kx <= FRAME_KX always.
FRAME_KX = 111320.0
#: Latitude span the kx band allows between `lat0` (the extract's MINIMUM) and its MEAN. One degree
#: is ~111 km -- two orders more than any supported extract (the cached cities span 0.013-0.018 deg).
LAT_SPAN_DEG = 1.0


def _assert_frame(projection: dict) -> dict:
    """GATE: the frame tuple itself, before anything is projected with it.

    `_assert_alignment` compares WHERE points land against a bbox derived from THIS SAME tuple, so
    it is structurally incapable of catching a wrong scale constant -- both sides move together. The
    error message there names exactly that trap ("note ky = 110540, not 111320") while being unable
    to fire on it: measured on the cached extracts, substituting ky = 111320 displaces nodes by only
    10.6-18.7 m, three orders below any translation tolerance, and every city still passes. That
    displacement is not harmless -- the geometric channel's building-blockage test would then be
    computed against footprints offset from the road graph, producing plausible-looking but wrong
    NLOS decisions with the gate green.

    This check is the one that can fire on it, because it tests the CONSTANTS rather than the
    geometry. It deliberately does NOT compare against `osm.road_projection(xml)`: the CLI's
    `--frame-city` deliberately projects one city's net into another city's frame, and a hard
    equality would forbid that legitimate use while adding nothing (the value being compared would
    be the value that produced it).
    """
    missing = [k for k in ("lat0", "lon0", "kx", "ky") if k not in (projection or {})]
    if missing:
        raise ValueError(f"projection frame is missing {missing}; it must be the tuple "
                         f"`osm.road_projection` returns: lat0/lon0 = min lat/lon over the ROAD "
                         f"ways, kx = {FRAME_KX} * cos(mean_lat), ky = {FRAME_KY}")
    lat0, lon0 = float(projection["lat0"]), float(projection["lon0"])
    kx, ky = float(projection["kx"]), float(projection["ky"])
    if not (-90.0 <= lat0 <= 90.0 and -180.0 <= lon0 <= 180.0):
        raise ValueError(f"projection origin (lat0={lat0}, lon0={lon0}) is not a WGS84 coordinate")
    if ky != FRAME_KY:
        raise ValueError(
            f"projection frame has ky = {ky}, not {FRAME_KY}. THIS IS THE TRAP: {FRAME_KX} is the "
            f"equatorial degree of LONGITUDE and {FRAME_KY} the mean degree of LATITUDE, and every "
            f"layer sharing this map (the road graph, the imported net, the building footprints) "
            f"must use the identical constant or they silently de-register by ~10-20 m over a "
            f"1.5 km extent -- far too little for the extent gate to see, far too much for the "
            f"geometric channel's building-blockage test, which would then compute NLOS against "
            f"footprints offset from the roads.")
    if not (0.0 < kx <= FRAME_KX + 1e-6):
        raise ValueError(f"projection frame has kx = {kx}, outside (0, {FRAME_KX}]: kx is "
                         f"{FRAME_KX} * cos(mean_lat) and cos is at most 1")
    # kx = FRAME_KX * cos(MEAN latitude) and `lat0` is the extract's MINIMUM, so the band is around
    # cos(|lat0|) widened by one degree of latitude span in BOTH directions. Symmetric in |lat|
    # deliberately: in the southern hemisphere the minimum latitude is the most negative, so the mean
    # is CLOSER to the equator and kx is LARGER than cos(lat0) gives -- a one-sided band derived from
    # the northern case would reject every southern-hemisphere import. One degree is ~111 km, two
    # orders more than any supported extract, because this is a units/typo check (kx and ky swapped,
    # or the equatorial constant used unscaled), not a re-derivation. A small latitude inconsistency
    # is the EXTENT gate's business, not this one's.
    lo = FRAME_KX * math.cos(math.radians(min(90.0, abs(lat0) + LAT_SPAN_DEG))) - 1e-6
    hi = FRAME_KX * math.cos(math.radians(max(0.0, abs(lat0) - LAT_SPAN_DEG))) + 1e-6
    if not (lo <= kx <= hi):
        raise ValueError(
            f"projection frame has kx = {kx:.1f}, inconsistent with lat0 = {lat0}: "
            f"{FRAME_KX} * cos(mean_lat) over an extract whose minimum latitude is {lat0} must lie "
            f"in [{lo:.1f}, {hi:.1f}] (kx and ky swapped? the equatorial constant used unscaled?)")
    return projection


def _transformer(net, projection: dict | None):
    """(x, y) in SUMO net coordinates -> (x, y) in the target frame.

    With `projection` (osm.py's `info["projection"]`) the net is un-projected to lon/lat and
    re-projected into that EXACT local equirectangular frame, so the imported network is registered
    with the road graph and the building footprints from the same extract. Without it the net's own
    metric coordinates are used unchanged (already metres, with the origin at the corner)."""
    if projection is None:
        return (lambda x, y: (x, y)), None
    if not net.hasGeoProj():
        raise ValueError("this .net.xml has no geo-projection (projParameter='!'), so it cannot be "
                         "registered with an osm.py projection frame -- import it with "
                         "projection=None (its own metric coordinates) instead")
    _assert_frame(projection)
    param = net._location["projParameter"]
    zone = _utm_zone(param)
    off_x, off_y = net.getLocationOffset()
    lat0 = float(projection["lat0"])
    lon0 = float(projection["lon0"])
    kx = float(projection["kx"])
    ky = float(projection["ky"])
    try:                                          # prefer pyproj when present: handles any +proj
        proj = net.getGeoProj()

        def to_lonlat(x, y):
            return proj(x - off_x, y - off_y, inverse=True)
    except Exception:                             # pyproj absent (this toolchain) -> UTM inverse
        if zone is None:
            raise ValueError(f"cannot invert projection {param!r} without pyproj (only plain UTM "
                             f"is supported by the built-in inverse)") from None

        def to_lonlat(x, y):
            return inverse_utm(x - off_x, y - off_y, zone[0], zone[1])

    def tf(x, y):
        lon, lat = to_lonlat(x, y)
        return (lon - lon0) * kx, (lat - lat0) * ky

    return tf, param


#: `_assert_alignment` tolerances, CALIBRATED on the cached extracts rather than guessed.
#:
#: The old rule was `median node inside the expected bbox WIDENED BY max(250, 0.5*diagonal)`, which
#: for Ingolstadt meant the median could sit anywhere in a box roughly 3.5x the modelled area wide:
#: measured by bisection on the real 217-junction cloud, it accepted an east-west translation of
#: 1821 m and a diagonal one of 2281 m -- both LARGER THAN THE CITY. A city could be projected
#: entirely off itself and pass.
#:
#: The rule now compares the median node with the bbox CENTRE, per axis, tolerating a fraction of
#: that axis's own extent. Measured |median - centre| / extent over the seven cached cities
#: (amsterdam, berlin, ingolstadt, manhattan, munich, paris, vienna): worst 0.104 (manhattan). 0.25
#: therefore carries ~2.4x headroom over the worst real city, while the largest translation it can
#: miss drops to 233-499 m across those seven -- 233 m for Ingolstadt itself, against the 1821 m the
#: old rule allowed there.
ALIGN_CENTRE_FRAC = 0.25
ALIGN_MARGIN_M = 250.0
#: Second, INDEPENDENT arm, over a statistic the function already computed and then ignored. It is
#: not a restatement of the median test: it moves under a wrong SCALE or a rotation, which shift the
#: cloud's spread without necessarily shifting its centre. netconvert keeps whole edges, so junctions
#: legitimately sit outside the requested bbox; measured containment for a CORRECT import is
#: 0.838-0.899 over the same seven cities, so 0.60 has ~0.24 absolute headroom.
ALIGN_MIN_INSIDE_FRAC = 0.60


def _assert_alignment(pts: list, expect_bbox_xy: list | None, *,
                      margin_m: float = ALIGN_MARGIN_M,
                      centre_frac: float = ALIGN_CENTRE_FRAC,
                      min_inside_frac: float = ALIGN_MIN_INSIDE_FRAC) -> dict:
    """GATE: the imported network must land where the extract says it is.

    A wrong origin translates the whole graph while every street still looks fine -- exactly the
    failure `extract_buildings` guards against for footprints.

    Two arms; see the constants above for how each tolerance was measured. NOTE what this gate can
    and cannot do: it is a TRANSLATION and gross-shape check. It cannot see a wrong scale CONSTANT,
    because `expect_bbox_xy` is derived from the very projection tuple under test, so both sides move
    together -- that is `_assert_frame`'s job, and it is called from `_transformer` before any point
    is projected.
    """
    xs = sorted(p[0] for p in pts)
    ys = sorted(p[1] for p in pts)
    med = [xs[len(xs) // 2], ys[len(ys) // 2]]
    info = {"median_node": [round(med[0], 2), round(med[1], 2)],
            "node_bbox": [round(xs[0], 2), round(ys[0], 2), round(xs[-1], 2), round(ys[-1], 2)]}
    if expect_bbox_xy is None:
        return info
    x0, y0, x1, y1 = (float(v) for v in expect_bbox_xy)
    cx, cy = 0.5 * (x0 + x1), 0.5 * (y0 + y1)
    tol_x = max(margin_m, centre_frac * abs(x1 - x0))
    tol_y = max(margin_m, centre_frac * abs(y1 - y0))
    info["expected_bbox"] = [round(x0, 2), round(y0, 2), round(x1, 2), round(y1, 2)]
    info["alignment_tolerance_m"] = [round(tol_x, 1), round(tol_y, 1)]
    info["median_offset_m"] = [round(med[0] - cx, 2), round(med[1] - cy, 2)]
    inside = sum(1 for p in pts if x0 <= p[0] <= x1 and y0 <= p[1] <= y1)
    frac = inside / max(1, len(pts))
    info["nodes_inside_expected_bbox_frac"] = round(frac, 4)
    if abs(med[0] - cx) > tol_x or abs(med[1] - cy) > tol_y:
        raise ValueError(
            f"imported network does not land on the expected extent: its median node "
            f"{info['median_node']} is offset {info['median_offset_m']} m from the centre of "
            f"{info['expected_bbox']}, beyond the tolerance {info['alignment_tolerance_m']} m "
            f"(= {centre_frac:g} of each axis's extent, floor {margin_m:g} m). A SUMO net must be "
            f"re-projected into the SAME local frame osm.py derived from the ROAD ways "
            f"(lat0/lon0 = min lat/lon over those ways), never with a re-derived origin.")
    if frac < min_inside_frac:
        raise ValueError(
            f"only {frac:.1%} of the imported junctions land inside the requested extent "
            f"{info['expected_bbox']} (floor {min_inside_frac:.0%}). netconvert keeps whole edges, "
            f"so a few junctions legitimately sit outside -- a correct import measures 84-90% -- "
            f"but this is the signature of a wrong SCALE or a rotated frame, which spreads the "
            f"cloud without necessarily moving its centre.")
    return info


# --------------------------------------------------------------------------- #
# .net.xml -> custom network
# --------------------------------------------------------------------------- #
def _edge_class(edge) -> str:
    """netconvert edge type 'highway.residential' -> 'residential' (own OSM class back again)."""
    t = edge.getType() or ""
    return t.split(".")[-1] if t else ""


def _largest_strong_component(n_nodes: int, directed: list) -> set:
    """Largest strongly-connected node set of the directed graph (iterative Kosaraju).

    An undirected import only has to be CONNECTED; a directed one has to be STRONGLY connected, and
    a bbox-clipped city is not: enter on a one-way street whose only exit leaves the extract and the
    junction becomes a trap that strands every trip routed into it. Deterministic (nodes visited in
    index order); draws no RNG."""
    fwd: dict[int, list[int]] = {}
    rev: dict[int, list[int]] = {}
    for d in directed:
        fwd.setdefault(d["a"], []).append(d["b"])
        rev.setdefault(d["b"], []).append(d["a"])

    order: list[int] = []
    seen: set[int] = set()
    for s in range(n_nodes):                     # first pass: finishing order on the forward graph
        if s in seen:
            continue
        stack = [(s, 0)]
        seen.add(s)
        while stack:
            v, i = stack.pop()
            nbrs = fwd.get(v, ())
            if i < len(nbrs):
                stack.append((v, i + 1))
                w = nbrs[i]
                if w not in seen:
                    seen.add(w)
                    stack.append((w, 0))
            else:
                order.append(v)
    best: set[int] = set()
    assigned: set[int] = set()
    for s in reversed(order):                    # second pass: components on the reverse graph
        if s in assigned:
            continue
        comp = {s}
        assigned.add(s)
        stack = [s]
        while stack:
            for w in rev.get(stack.pop(), ()):
                if w not in assigned:
                    assigned.add(w)
                    comp.add(w)
                    stack.append(w)
        if len(comp) > len(best):
            best = comp
    return best


def road_surface(net, tf, *, vclass: str | None = "passenger", round_m: int = 2) -> dict:
    """The map's DRIVABLE SURFACE, as `roads.CustomNetwork.set_road_surface` wants it.

    This is deliberately NOT the routing graph and it is not derived from it. The graph holds one
    centreline per physical road between two junction CENTRES; the surface holds what SUMO's own
    network says is tarmac:

      * every non-internal edge's own polyline, in its own place -- both carriageways of a two-way
        street, and every one of several distinct roads that happen to share a junction pair. None
        of that survives the graph, which keeps a single shape per undirected node pair (and must:
        `roads.CustomNetwork` rejects two different shapes for one edge);
      * every junction's polygon, reduced to a disc (centre + the furthest shape vertex). A vehicle
        crossing a signalised junction is on an INTERNAL lane for tens of metres and no edge covers
        it -- but the junction polygon does, and it is the map's own statement of where the tarmac
        is rather than a reconstruction from connections.

    Importing the internal lanes themselves was the alternative and it is not available: InTAS has
    15,706 of them against `CustomNetwork.MAX_EDGES` of 12,000, before any of them get nodes.

    Emitted BEFORE any class-dropping or connectivity trimming: `dist_to_road` asks whether a vehicle
    is on a road, which does not stop being true because the router declined to use that road.
    Deterministic (edges and junctions in id order); draws no RNG."""
    polylines = []
    for e in sorted(net.getEdges(), key=lambda e: e.getID()):
        if e.getFunction() == "internal" or (vclass is not None and not e.allows(vclass)):
            continue
        pts = [[round(v, round_m) for v in tf(px, py)] for px, py in e.getShape()]
        ded = [p for k, p in enumerate(pts) if k == 0 or p != pts[k - 1]]
        if len(ded) >= 2:
            polylines.append(ded)
    junctions = []
    for n in sorted(net.getNodes(), key=lambda n: n.getID()):
        if n.getType() == "internal":
            continue
        c = tf(*n.getCoord()[:2])
        # The radius is measured in the TARGET frame, from the transformed polygon -- taking it in
        # SUMO metres and carrying it across would silently rescale it under a re-projection.
        r = max((math.dist(c, tf(px, py)) for px, py in n.getShape()), default=0.0)
        junctions.append([round(c[0], round_m), round(c[1], round_m), round(r, round_m)])
    return {"polylines": polylines, "junctions": junctions}


# --------------------------------------------------------------------------- #
# the OTHER half of the scene: building footprints, in the road graph's frame
# --------------------------------------------------------------------------- #
#: SUMO `<poly type="...">` values treated as buildings. `type="building"` is what polyconvert
#: writes for an OSM `building=*` way and is exactly the selection
#: `org.scms.radio.BuildingIndex.parse` makes on the Java side, so both engines rasterise the SAME
#: footprint set rather than two subsets of one file.
POLY_BUILDING_TYPES = ("building",)

#: Footprints further than this from the road extent cannot block a link between two vehicles.
BUILDING_MARGIN_M = 250.0

#: `_assert_buildings_aligned` tolerances, every one MEASURED on InTAS by translating the real
#: footprint layer and bisecting (`docs/realism/FULL-CITY-SCENE.md` §3 carries the table). They are
#: chosen as a PAIR that covers the whole translation range with no hole: arm 1 catches a shift from
#: ~0.9 km up, arm 3 from ~11 m up, so between them nothing survives.
#:
#: ARM 1 -- median building centroid against the median JUNCTION, per axis, as a fraction of that
#: axis's road extent. Deliberately NOT `osm.extract_buildings`' "inside the road bbox widened by
#: half the diagonal": netconvert keeps motorway stubs far outside the built-up area, so InTAS's
#: road bbox is 13.58 x 11.09 km around an 8.31 x 8.01 km city and that rule tolerates an 8.8 km
#: shift -- larger than the city. Measured offset on InTAS: 237.6 m / 199.0 m against tolerances of
#: 1358.4 m / 1109.2 m, i.e. 5.7x / 5.6x headroom; this arm alone fires from 0.91-1.60 km depending
#: on the direction, which is why arm 3 exists.
ALIGN_BUILDING_CENTRE_FRAC = 0.10
#: ARM 2 -- the FRACTION of centroids inside the road bbox. InTAS measures 1.0000. Moves under a
#: wrong SCALE or a rotation, which spread the cloud without necessarily moving its centre.
ALIGN_MIN_CENTROID_INSIDE_FRAC = 0.60
#: ARM 3, the sharp one. A correctly registered city puts its junctions on tarmac: InTAS measures
#: 0.0027 of junctions inside a footprint, and displacing the layer drives that toward the
#: built-area fraction (0.0441 at 10 m, 0.1327 at 25 m, 0.16 at 100-400 m). 0.05 is 18x the real
#: value and fires at a shift of about 11 m -- roughly one lane width.
ALIGN_MAX_JUNCTIONS_IN_FOOTPRINT_FRAC = 0.05


def _poly_rings(poly_path: str, types=POLY_BUILDING_TYPES) -> tuple[list, dict]:
    """`<poly type="building" shape="x,y x,y ...">` -> OPEN vertex rings, in the file's own frame.

    A SUMO polygon additional-file is written in the NET's metric coordinates (the same frame the
    `.net.xml` junctions are in, `netOffset` already applied), so nothing is projected here -- the
    caller's transform does that, and it must be the road graph's own."""
    want = frozenset(types) if types else None
    rings: list = []
    raw = 0
    skipped_type = 0
    degenerate = 0
    for _ev, el in ET.iterparse(poly_path, events=("end",)):
        if el.tag != "poly":
            continue
        raw += 1
        if want is not None and (el.get("type") or "") not in want:
            skipped_type += 1
            el.clear()
            continue
        pts = []
        bad = False
        for part in (el.get("shape") or "").split():
            bits = part.split(",")
            if len(bits) < 2:
                continue
            try:
                pts.append((float(bits[0]), float(bits[1])))
            except ValueError:
                bad = True
                break
        el.clear()
        if bad:
            degenerate += 1
            continue
        if len(pts) >= 2 and pts[0] == pts[-1]:
            pts = pts[:-1]                       # the closing vertex is implied, as everywhere else
        if len(pts) < 3:
            degenerate += 1
            continue
        rings.append(pts)
    return rings, {"poly_elements": raw, "skipped_wrong_type": skipped_type,
                   "degenerate": degenerate, "rings": len(rings)}


def _assert_buildings_aligned(rings: list, road_bbox, junctions=None, *,
                              margin_m: float = ALIGN_MARGIN_M,
                              centre_frac: float = ALIGN_BUILDING_CENTRE_FRAC,
                              min_centroid_inside: float = ALIGN_MIN_CENTROID_INSIDE_FRAC,
                              max_junction_hit: float = ALIGN_MAX_JUNCTIONS_IN_FOOTPRINT_FRAC,
                              index_cell_m: float = 40.0) -> dict:
    """GATE: the footprints must sit ON the road network, in three independent ways.

    THE FAILURE THIS EXISTS FOR is silent. `osm.py` derives its frame origin from the ROAD ways
    only; a layer projected with a different origin (or with a SUMO net's own UTM offset left in)
    lands hundreds of metres away while every individual polygon still looks like a building and
    every street still looks like a street. The geometric channel then computes NLOSb against a city
    translated off itself, and the resulting dataset is plausible and wrong.

    ARM 1 -- the median building centroid against the median JUNCTION, per axis, tolerating
    `max(margin_m, centre_frac * that axis's road extent)`. Catches a large translation.

    ARM 2 -- the FRACTION of centroids inside the road bbox. Moves under a wrong SCALE or a rotation,
    which shift the cloud's spread without necessarily moving its centre.

    ARM 3 -- the fraction of ROAD JUNCTIONS that land inside a footprint. This is the sharp one and
    the only one that is a statement about REGISTRATION rather than about extent: a city's junctions
    are on tarmac, so a correct overlay puts almost none of them inside a wall, and even a one-lane
    displacement pushes that fraction toward the built-area fraction. Skipped (and reported as None)
    when the caller passes no junctions -- and then arms 1-2 alone tolerate a shift of hundreds of
    metres, so a caller that can supply junctions should.

    The two thresholds are chosen as a pair with no hole between them: see the constants above.
    """
    if not rings:
        raise ValueError("no building footprints were read: nothing to register against the roads")
    cents = [(sum(p[0] for p in r) / len(r), sum(p[1] for p in r) / len(r)) for r in rings]
    bxs = [p[0] for r in rings for p in r]
    bys = [p[1] for r in rings for p in r]
    cxs = sorted(c[0] for c in cents)
    cys = sorted(c[1] for c in cents)
    med = (cxs[len(cxs) // 2], cys[len(cys) // 2])
    info: dict = {"polygons": len(rings), "vertices": len(bxs),
                  "building_bbox": [round(min(bxs), 2), round(min(bys), 2),
                                    round(max(bxs), 2), round(max(bys), 2)],
                  "median_building_centroid": [round(med[0], 2), round(med[1], 2)]}
    if road_bbox is None:
        info.update(centroid_inside_road_bbox_frac=None, junctions_in_footprint_frac=None)
        return info
    rx0, ry0, rx1, ry1 = (float(v) for v in road_bbox)
    info["road_bbox"] = [round(rx0, 2), round(ry0, 2), round(rx1, 2), round(ry1, 2)]
    trap = ("THIS IS THE PROJECTION TRAP -- footprints must go through the SAME transform the "
            "junctions did (netimport._transformer on this net), never through a re-derived "
            "origin, and a SUMO .poly.xml is already in the net's own metric frame.")
    if junctions:
        # The median JUNCTION, not the bbox centre. netconvert keeps motorway stubs far outside the
        # built-up area, so InTAS's road bbox centre sits 1.9 km east of the city its buildings are
        # in and a bbox-centre test would fail a CORRECT import. The junction cloud's median is the
        # city; measured offset from it, 238 m / 199 m.
        jxs = sorted(p[0] for p in junctions)
        jys = sorted(p[1] for p in junctions)
        anchor = (jxs[len(jxs) // 2], jys[len(jys) // 2])
        tol_x = max(margin_m, centre_frac * abs(rx1 - rx0))
        tol_y = max(margin_m, centre_frac * abs(ry1 - ry0))
        info["alignment_anchor"] = [round(anchor[0], 2), round(anchor[1], 2)]
        info["alignment_anchor_is"] = "median junction"
        info["alignment_tolerance_m"] = [round(tol_x, 1), round(tol_y, 1)]
        info["median_offset_m"] = [round(med[0] - anchor[0], 2), round(med[1] - anchor[1], 2)]
        if abs(med[0] - anchor[0]) > tol_x or abs(med[1] - anchor[1]) > tol_y:
            raise ValueError(
                f"projected building footprints do not sit on the road network: their median "
                f"centroid {info['median_building_centroid']} is offset {info['median_offset_m']} m "
                f"from the median junction {info['alignment_anchor']}, beyond the tolerance "
                f"{info['alignment_tolerance_m']} m (= {centre_frac:g} of each axis's road extent, "
                f"floor {margin_m:g} m). " + trap)
    else:
        # No junction cloud: fall back to `osm.extract_buildings`' containment rule, which is all a
        # bbox alone can support. It is MUCH weaker -- on InTAS it tolerates an 8.8 km shift -- so
        # a caller that can supply junctions must.
        tol = max(margin_m, 0.5 * math.hypot(rx1 - rx0, ry1 - ry0))
        info["alignment_anchor_is"] = "road bbox containment (no junctions supplied -- weak)"
        info["alignment_tolerance_m"] = round(tol, 1)
        if not (rx0 - tol <= med[0] <= rx1 + tol and ry0 - tol <= med[1] <= ry1 + tol):
            raise ValueError(
                f"projected building footprints do not sit on the road network: median centroid "
                f"{info['median_building_centroid']} is outside the road bbox {info['road_bbox']} "
                f"widened by {tol:.0f} m. " + trap)
    inside = sum(1 for c in cents if rx0 <= c[0] <= rx1 and ry0 <= c[1] <= ry1)
    frac = inside / len(cents)
    info["centroid_inside_road_bbox_frac"] = round(frac, 4)
    if frac < min_centroid_inside:
        raise ValueError(
            f"only {frac:.2%} of building centroids land inside the road extent "
            f"{info['road_bbox']} (floor {min_centroid_inside:.0%}). A correct overlay measures "
            f"87-100%; this is the signature of a wrong SCALE or a rotated frame, which spreads the "
            f"footprint cloud without necessarily moving its centre.")
    info["junctions_in_footprint_frac"] = None
    if junctions:
        idx = FootprintIndex(rings, cell_m=index_cell_m)
        hit = sum(1 for jx, jy in junctions if idx.entered(jx, jy, jx, jy))
        jf = hit / len(junctions)
        info["junctions"] = len(junctions)
        info["junctions_in_footprint"] = hit
        info["junctions_in_footprint_frac"] = round(jf, 4)
        if jf > max_junction_hit:
            raise ValueError(
                f"{jf:.2%} of the road graph's junctions land INSIDE a building footprint (ceiling "
                f"{max_junction_hit:.0%}). Junctions are on tarmac in every real city -- InTAS "
                f"measures 0.27%, 9 of 3332 -- so this says the two layers are not registered with "
                f"each other. Check that the footprints went through this net's own transform: a "
                f"10 m displacement already reads 4.4% and a 25 m one 13.3%.")
    return info


def buildings_from_poly(poly_path: str, tf=None, *, types=POLY_BUILDING_TYPES,
                        road_bbox=None, junctions=None, margin_m: float = BUILDING_MARGIN_M,
                        min_area_m2: float = 0.0, simplify_tol_m: float = 0.0,
                        max_polygons: int = 0, round_m: int = 2,
                        gate: bool = True) -> tuple[list, dict]:
    """A SUMO `.poly.xml` -> building rings in the ROAD GRAPH's frame, gated on landing there.

    `tf` MUST be the transform this import applied to the junction coordinates
    (`netimport._transformer(net, projection)[0]`, which `scene_from_net` and
    `sumo_trace.engine_network` both hand out) -- see `_assert_buildings_aligned`.

    The defaults are deliberately LOSSLESS -- no minimum area, no RDP simplification, no polygon
    cap -- because the Java side (`org.scms.radio.BuildingIndex`) rasterises the file as it stands,
    and a cross-engine comparison of link-state composition is only a comparison if both engines
    hold the same footprints. `osm.extract_buildings` simplifies at 1 m and caps at 6000 because it
    is reconstructing rings from raw OSM node refs; here the polygons arrive already resolved.

    Returns `(polygons, info)`; each polygon is an OPEN vertex list `[[x, y], ...]`, sorted
    canonically so the layer is byte-stable for a given input.
    """
    rings, stats = _poly_rings(poly_path, types)
    if not rings:
        raise ValueError(f"{poly_path}: no <poly> of type {list(types)} carried a usable shape "
                         f"({stats['poly_elements']} poly elements seen)")
    if tf is not None:
        rings = [[tf(x, y) for x, y in r] for r in rings]
    if min_area_m2 > 0.0:
        n0 = len(rings)
        rings = [r for r in rings if _ring_area(r) >= min_area_m2]
        stats["dropped_below_min_area"] = n0 - len(rings)
    if simplify_tol_m > 0.0:
        from .osm import _rdp                     # noqa: PLC0415  (only on the opt-in path)
        simp = []
        for r in rings:
            if len(r) > 4:
                s = _rdp(list(r) + [r[0]], simplify_tol_m)
                if len(s) >= 4:
                    r = [tuple(p) for p in s[:-1]]
            simp.append(r)
        rings = simp
    stats["kept_before_bbox"] = len(rings)
    if road_bbox is not None:
        rx0, ry0, rx1, ry1 = (float(v) for v in road_bbox)
        lo_x, lo_y = rx0 - margin_m, ry0 - margin_m
        hi_x, hi_y = rx1 + margin_m, ry1 + margin_m
        near = []
        for r in rings:
            cx = sum(p[0] for p in r) / len(r)
            cy = sum(p[1] for p in r) / len(r)
            if lo_x <= cx <= hi_x and lo_y <= cy <= hi_y:
                near.append(r)
        stats["dropped_beyond_margin"] = len(rings) - len(near)
        rings = near
    if not rings:
        raise ValueError(f"{poly_path}: every footprint fell outside the road extent "
                         f"{road_bbox} widened by {margin_m:g} m")
    align = _assert_buildings_aligned(rings, road_bbox, junctions) if gate else {}
    rings.sort(key=lambda r: (round(min(p[0] for p in r), 3), round(min(p[1] for p in r), 3)))
    if max_polygons and len(rings) > max_polygons:
        stats["truncated_from"] = len(rings)
        rings = rings[:max_polygons]
    out = [[[round(x, round_m), round(y, round_m)] for x, y in r] for r in rings]
    info = {**stats, **align, "polygons": len(out),
            "vertices": sum(len(r) for r in out),
            "source": os.path.abspath(poly_path).replace("\\", "/"),
            "poly_types": list(types), "min_area_m2": float(min_area_m2),
            "simplify_tol_m": float(simplify_tol_m), "gated": bool(gate)}
    return out, info


def scene_from_net(net_path: str, poly_path: str, *, projection: dict | None = None,
                   **kw) -> tuple[list, dict]:
    """`(.net.xml, .poly.xml)` -> footprints in that net's frame, registered against its junctions.

    THE POINT OF THIS FUNCTION is that it does not accept a transform from the caller: it reads the
    net, builds the transform with `_transformer` -- the same call `net_to_network` makes -- and
    derives the road bbox and the junction cloud from that same net. There is therefore no way for
    the footprints to end up in a frame the roads are not in, which is the entire failure mode.

    Costs one extra `sumolib` read of the net (0.5 s on InTAS's 16.9 MB). That is deliberate: the
    alternative -- re-deriving the transform from the `<location>` element alone -- would be a
    second implementation of the one thing that must not diverge.
    """
    net = read_net(net_path)
    tf, _param = _transformer(net, projection)
    junctions = [tf(*n.getCoord()[:2]) for n in net.getNodes() if n.getType() != "internal"]
    if not junctions:
        raise ValueError(f"{net_path}: no non-internal junctions to register footprints against")
    xs = [p[0] for p in junctions]
    ys = [p[1] for p in junctions]
    polys, info = buildings_from_poly(poly_path, tf, road_bbox=[min(xs), min(ys), max(xs), max(ys)],
                                      junctions=junctions, **kw)
    info["net"] = os.path.abspath(net_path).replace("\\", "/")
    info["net_junctions"] = len(junctions)
    return polys, info


def _canonicalise_shapes(directed: list, coords: list) -> int:
    """ONE geometry per physical road, and re-measure the lengths against it.

    netconvert models a two-way street as two edges, each carrying its OWN carriageway geometry --
    offset sideways from the centreline, and not a mirror image of its partner (MEASURED: 105 of
    154 two-way pairs in the Ingolstadt import). The engine's model is the opposite way round: one
    centreline per road, with per-direction lane offsets applied on top (`roads.py` stores exactly
    one `edge_shape` per undirected key and REJECTS a second, different one). So the importer has
    to choose, and it chooses deterministically: the low->high direction's polyline is the road's
    geometry, and the high->low record carries its exact reverse.

    WHAT THIS COSTS, measured on InTAS rather than asserted: 2,165 of 7,941 records get a substituted
    shape, displacing their original vertices by p50 5.81 m / p95 9.61 m / max 183.69 m. The tail is
    not the carriageway offset -- it is 35 undirected node-pairs joined by MORE THAN ONE distinct
    physical road (a straight street and a loop road between the same two junctions), where one
    geometry is imposed on the other. Distance-to-road over the frozen InTAS trace: p95 4.776 m /
    max 17.173 m per carriageway, p95 6.399 m / max 57.481 m after this function.

    That loss is unavoidable in the GRAPH (one edge, one shape) and it is why `road_surface` exists:
    `dist_to_road` measures against the per-carriageway geometry, which this never touches, so the
    substitution now only affects where an INTERNAL (non-replay) vehicle drives.

    Returns the number of records whose shape was replaced."""
    canon: dict[tuple[int, int], list] = {}
    for rec in directed:
        if rec.get("shape") and rec["a"] < rec["b"]:
            canon.setdefault((rec["a"], rec["b"]), rec["shape"])
    replaced = 0
    for rec in directed:
        key = (min(rec["a"], rec["b"]), max(rec["a"], rec["b"]))
        if key in canon:
            want = canon[key] if rec["a"] < rec["b"] else list(reversed(canon[key]))
            if rec.get("shape") != want:
                rec["shape"] = want
                replaced += 1
        poly = [coords[rec["a"]], *rec.get("shape", ()), coords[rec["b"]]]
        rec["length_m"] = round(sum(math.dist(p, q) for p, q in zip(poly, poly[1:])), 1)
    return replaced


def read_net(net_path: str, *, programs: bool = False):
    """`sumolib.net.readNet` with a useful error when sumolib is missing.

    `programs=True` additionally parses the `<tlLogic>` elements (sumolib's `withPrograms`, which
    defaults OFF and silently yields NO programs at all when left off). Required for
    `net_to_network(signals=True)`; costs nothing on the default path."""
    try:
        import sumolib                          # noqa: PLC0415  (optional heavy dependency)
    except ImportError:                          # pragma: no cover - toolchain always has it
        raise RuntimeError("sumolib is required to import a .net.xml (pip install sumolib, or use "
                           "the SUMO_HOME/tools copy)") from None
    if not os.path.exists(net_path):
        raise FileNotFoundError(net_path)
    if programs:
        return sumolib.net.readNet(net_path, withPrograms=True)
    return sumolib.net.readNet(net_path)


def _extract_signals(net, by_id: dict, eidx: dict, remap: dict, old_to_new_dir: dict,
                     kept_dir: list, sig: list) -> tuple[list, dict]:
    """The REAL `<tlLogic>` programs of `net`, in THIS import's final node indices.

    Two closures carry every index remap the importer performs (the coordinate dedupe `by_id`, the
    connected-component `remap`, and the directed-record compaction `old_to_new_dir`) into
    `signals.extract`, so that module never has to know how the graph was trimmed. Getting this
    wrong is the failure mode the whole exercise is about: an approach mapped to the wrong graph
    node produces a junction whose signals are internally consistent and completely fictional.

    Raises if the net was read WITHOUT `withPrograms=True` -- an empty signal layer that looks like
    "this city has no signals" is exactly the silent failure to avoid."""
    from . import signals as _signals          # noqa: PLC0415  (only on the opt-in path)

    programs = _signals.programs_from_net(net)
    if not programs:
        raise ValueError(
            "signals=True but this net yielded no <tlLogic> programs. Read it with "
            "`read_net(path, programs=True)` (sumolib's withPrograms defaults OFF and drops every "
            "program silently), or import a net that carries traffic-light programs.")

    def node_index(sumo_node_id):
        ci = by_id.get(sumo_node_id)
        return remap.get(ci) if ci is not None else None

    def edge_endpoints(sumo_edge_id):
        k = eidx.get(sumo_edge_id)
        if k is None:
            return None
        nk = old_to_new_dir.get(k)
        if nk is None:
            return None
        rec = kept_dir[nk]
        return (rec["a"], rec["b"])

    records, st = _signals.extract(net, node_index, edge_endpoints, programs=programs)
    st.update(_signals.program_stats(programs))
    placed = {r["node"] for r in records}
    st["signal_nodes"] = len(sig)
    st["signal_nodes_with_program"] = len(placed & set(sig))
    st["program_nodes_not_typed_traffic_light"] = len(placed - set(sig))
    st["coverage_of_signal_nodes"] = (round(len(placed & set(sig)) / len(sig), 4) if sig else None)
    # MEASURED on InTAS (--signals --strong --undirected-shapes): junctions 98, links_total 1042,
    # links_mapped 1030, links_unmapped 10 (arms outside the strongly connected component),
    # links_self_loop 2, state_columns 1032, state_columns_mapped 1015, movements 778,
    # approaches_merged 1, joined_tls 0, coverage_of_signal_nodes 0.8991.
    return records, st


def net_to_network(net, *, projection: dict | None = None, expect_bbox_xy: list | None = None,
                   max_nodes: int = 0, shapes: bool = True, turns: bool = False,
                   strong: bool = False, vclass: str | None = "passenger",
                   min_edge_m: float = 1.0, round_m: int = 1,
                   undirected_shapes: bool = False, surface: bool = False,
                   signals: bool = False,
                   ) -> tuple[list, list, dict]:
    """A sumolib net -> (nodes, edges, info) in custom-network form.

    nodes are REAL junctions only (netconvert already folded curve vertices into edge shapes);
    `edges` is the undirected projection every existing consumer understands, and
    `info["directed_edges"]` carries the directed graph with per-edge lane counts, speeds and shape
    polylines. `info["signal_nodes"]` are the junctions netconvert marked `traffic_light` -- i.e.
    the ones OSM actually tags, consolidated across the cluster of nodes that guard one crossing.

    `max_nodes=0` means NO cap: unlike the raw-OSM path this graph is already free of fake nodes,
    so the ~380-node budget (which exists to stop RDP vertices eating the engine's node limit) is
    not applied. A positive `max_nodes` re-enables it, dropping minor road classes in the same order
    as `osm.py` until the graph fits.

    `strong=True` keeps only the largest STRONGLY connected component -- required by a consumer that
    honours one-ways (a bbox-clipped extract leaves junctions you can drive into and never out of);
    the count is reported as `weak_only_nodes` either way.

    `undirected_shapes=True` emits the undirected `edges` array in `parse_edge_spec`'s OBJECT form
    carrying the canonical curve geometry, instead of `[a, b, speed]` triples. THIS IS NOT COSMETIC.
    The triples describe every road as the straight chord between its junctions, and a consumer that
    builds from the undirected array -- which is what `road_network="sumo"` does by default -- then
    reasons about a city whose curved roads have been replaced by their chords. Measured on InTAS:
    distance from a replayed SUMO position to the engine's roads was p50 2.203 m / p95 17.434 m /
    max 95.227 m with the triples and p50 1.671 m / p95 7.219 m / max 57.481 m with the shapes. Left
    False the array is byte-identical to what every existing consumer reads.

    `surface=True` additionally emits `info["road_surface"]` -- see `road_surface()`. It is map
    geometry for `dist_to_road`, NOT topology: no node, no edge and no route changes because of it.

    `signals=True` additionally emits `info["signal_programs"]` -- the REAL `<tlLogic>` phase
    programs (state strings, durations, minDur/maxDur, offset) together with the
    movement -> phase-index mapping, keyed on THIS import's node indices. See `signals.py`; the net
    must have been read with `read_net(path, programs=True)` or there are no programs to take.
    Left False, `info` carries no such key and every existing consumer reads byte-identical output.
    MEASURED on InTAS: 98 programs, 98 junctions placed, cycle median 90 s, 20 of the 98
    effectively actuated."""
    tf, param = _transformer(net, projection)
    nodes_all = [n for n in net.getNodes() if n.getType() != "internal"]
    edges_all = [e for e in net.getEdges()
                 if e.getFunction() != "internal" and (vclass is None or e.allows(vclass))]
    if not edges_all:
        raise ValueError("no usable edges in this .net.xml (all internal or vclass-filtered)")
    drop_order = list(_DROP_ORDER)                # minor classes first, same order as osm.py
    stats: dict = {"net_junctions": len(nodes_all), "net_edges": len(edges_all),
                   "proj_parameter": param}

    for attempt in range(len(drop_order) + 1):
        dropped = set(drop_order[:attempt]) if max_nodes else set()
        use = [e for e in edges_all if _edge_class(e) not in dropped]
        idx: dict[tuple, int] = {}
        coords: list[list[float]] = []
        by_id: dict[str, int] = {}

        def nid(pt):
            key = (round(pt[0], round_m), round(pt[1], round_m))
            k = idx.get(key)
            if k is None:
                k = len(coords)
                idx[key] = k
                coords.append([key[0], key[1]])
            return k

        for n in sorted(nodes_all, key=lambda n: n.getID()):
            by_id[n.getID()] = nid(tf(*n.getCoord()[:2]))
        directed: list[dict] = []
        undirected: dict[tuple[int, int], float] = {}
        self_loops = 0
        too_short = 0
        clamped = 0
        eidx: dict[str, int] = {}
        for e in sorted(use, key=lambda e: e.getID()):
            a = by_id.get(e.getFromNode().getID())
            b = by_id.get(e.getToNode().getID())
            if a is None or b is None:
                continue
            if a == b:                            # a loop (usually a collapsed roundabout arm)
                self_loops += 1
                continue
            # SHAPE CONVENTION (roads.py `parse_edge_spec`): "shape" holds the INTERMEDIATE curve
            # vertices only, a->b, with the junction coordinates implied. netconvert's own shape
            # starts and ends at the junction's internal boundary rather than its centre, so the
            # first and last vertices are dropped and the graph nodes take their place.
            raw_shape = [list(tf(px, py)) for px, py in e.getShape()]
            interior = ([[round(p[0], 2), round(p[1], 2)] for p in raw_shape[1:-1]]
                        if (shapes and len(raw_shape) > 2) else [])
            poly = [coords[a], *interior, coords[b]]
            length = sum(math.dist(p, q) for p, q in zip(poly, poly[1:]))
            if length < min_edge_m:
                too_short += 1
                continue
            sp = float(e.getSpeed())
            if not (1.0 <= sp <= 70.0):           # CustomNetwork validates 1..70 m/s
                clamped += 1
                sp = min(70.0, max(1.0, sp))
            rec = {"a": a, "b": b, "speed_mps": round(sp, 2), "lanes": int(e.getLaneNumber()),
                   "id": e.getID(), "length_m": round(length, 1)}
            cls = _edge_class(e)
            if cls:
                rec["class"] = cls
            if interior:
                rec["shape"] = interior
            eidx[e.getID()] = len(directed)
            directed.append(rec)
            key = (min(a, b), max(a, b))
            undirected[key] = max(undirected.get(key, 0.0), round(sp, 1))
        if not directed:
            continue
        _canonicalise_shapes(directed, coords)
        if turns:
            for e in sorted(use, key=lambda e: e.getID()):
                if e.getID() not in eidx:
                    continue
                allowed = sorted(eidx[o.getID()] for o in e.getOutgoing() if o.getID() in eidx)
                directed[eidx[e.getID()]]["turns"] = allowed

        # largest connected component (an extract always has stubs netconvert could not attach)
        adj: dict[int, set] = {}
        for a, b in undirected:
            adj.setdefault(a, set()).add(b)
            adj.setdefault(b, set()).add(a)
        seen: set[int] = set()
        best: set[int] = set()
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
            if len(comp) > len(best):
                best = comp
        remap = {old: new for new, old in enumerate(sorted(best))}
        out_nodes = [coords[old] for old in sorted(best)]
        if max_nodes and len(out_nodes) > max_nodes and attempt < len(drop_order):
            continue
        if max_nodes and len(out_nodes) > max_nodes:
            raise ValueError(f"net still exceeds {max_nodes} nodes after dropping {drop_order}; "
                             f"raise max_nodes (0 = no cap, the point of the netconvert path)")
        # STRONG connectivity: a directed consumer needs it, an undirected one does not. It is
        # measured either way; `strong=True` also prunes to it. The pruned graph is still connected
        # for the undirected array, because a strong component is a subset of the weak one.
        strong_set = _largest_strong_component(
            len(coords), [d for d in directed if d["a"] in remap and d["b"] in remap])
        weak_only = len(remap) - len(strong_set & set(remap))
        strong_trimmed = 0
        if strong and len(strong_set) >= 2:
            keep_ids = sorted(i for i in remap if i in strong_set)
            strong_trimmed = len(remap) - len(keep_ids)
            remap = {old: new for new, old in enumerate(keep_ids)}
            out_nodes = [coords[old] for old in keep_ids]
            weak_only = 0                        # ... they are gone now
        inv_remap = {new: old for old, new in remap.items()}
        out_edges: list = [[remap[a], remap[b], round(sp, 1)]
                           for (a, b), sp in sorted(undirected.items()) if a in remap and b in remap]
        if undirected_shapes:
            # ONE canonical polyline per physical road (`_canonicalise_shapes` already made the two
            # directions agree), attached to the undirected array so the graph follows the road's
            # curve instead of chording it. Object form; `roads.parse_edge_spec` reads both.
            canon_shape = {(rec["a"], rec["b"]): rec["shape"]
                           for rec in directed if rec.get("shape") and rec["a"] < rec["b"]}
            shaped = []
            for a, b, sp in out_edges:
                e: dict = {"a": a, "b": b, "speed": sp}
                sh = canon_shape.get((inv_remap[a], inv_remap[b]))
                if sh:
                    e["shape"] = sh
                shaped.append(e)
            out_edges = shaped
        kept_dir = []
        old_to_new_dir: dict[int, int] = {}
        for k, rec in enumerate(directed):
            if rec["a"] not in remap or rec["b"] not in remap:
                continue
            old_to_new_dir[k] = len(kept_dir)
            r = dict(rec, a=remap[rec["a"]], b=remap[rec["b"]])
            kept_dir.append(r)
        if turns:
            for r in kept_dir:
                r["turns"] = [old_to_new_dir[t] for t in r.get("turns", ())
                              if t in old_to_new_dir]
        sig = sorted({remap[by_id[n.getID()]] for n in nodes_all
                      if n.getType().startswith("traffic_light")
                      and by_id.get(n.getID()) in remap})
        sig_records: list = []
        sig_stats: dict = {}
        if signals:
            sig_records, sig_stats = _extract_signals(net, by_id, eidx, remap, old_to_new_dir,
                                                      kept_dir, sig)
        info = dict(stats)
        info.update(kept_nodes=len(out_nodes), kept_edges=len(out_edges),
                    directed_edges=kept_dir, signal_nodes=sig,
                    dropped_classes=sorted(dropped), self_loops=self_loops,
                    short_edges_dropped=too_short, speeds_clamped=clamped,
                    strong_component_nodes=len(strong_set), weak_only_nodes=weak_only,
                    strongly_connected=bool(strong),
                    # nodes REMOVED because they could be entered and never left. Reported so the
                    # trim is a stated fact of the import rather than a silent difference between
                    # two consumers of the same document.
                    strong_trimmed_nodes=strong_trimmed,
                    projection=projection,
                    road_bbox=[min(p[0] for p in out_nodes), min(p[1] for p in out_nodes),
                               max(p[0] for p in out_nodes), max(p[1] for p in out_nodes)])
        # How much geometry the ONE-SHAPE-PER-EDGE graph model cannot hold, reported rather than
        # left to be rediscovered. A physical road contributes ONE directed record if it is one-way
        # and TWO if it is not, so a node pair carrying MORE than two is joined by more than one
        # distinct road -- and `_canonicalise_shapes` then imposes one of their geometries on the
        # others. 35 such pairs in InTAS; the `road_surface` layer is what keeps their real
        # polylines available to `dist_to_road`.
        _by_pair: dict[tuple[int, int], int] = {}
        for rec in kept_dir:
            k = (min(rec["a"], rec["b"]), max(rec["a"], rec["b"]))
            _by_pair[k] = _by_pair.get(k, 0) + 1
        info["parallel_road_keys"] = sum(1 for v in _by_pair.values() if v > 2)
        info["undirected_shapes"] = bool(undirected_shapes)
        info["alignment"] = _assert_alignment(out_nodes, expect_bbox_xy)
        info.update(topology_stats(out_nodes, out_edges, kept_dir, sig))
        if signals:
            info["signal_programs"] = sig_records
            info["signal_program_stats"] = sig_stats
        if surface:
            info["road_surface"] = road_surface(net, tf, vclass=vclass)
            info["road_surface_stats"] = {
                "polylines": len(info["road_surface"]["polylines"]),
                "segments": sum(len(p) - 1 for p in info["road_surface"]["polylines"]),
                "junction_discs": len(info["road_surface"]["junctions"]),
                "junction_radius_max_m": round(max((j[2] for j in info["road_surface"]["junctions"]),
                                                   default=0.0), 2)}
        return out_nodes, out_edges, info
    raise ValueError("no connected component could be built from this .net.xml")


def topology_stats(nodes: list, edges: list, directed: list, signal_nodes=()) -> dict:
    """The fidelity numbers this import is judged on: one-way share, degree distribution,
    intersection count, per-edge lane distribution and lane-km. Same shape for either import path
    so the raw-OSM graph can be scored against the netconvert net as ground truth.

    `edges` may be either form `roads.parse_edge_spec` accepts -- the `[a, b, speed]` triples or the
    object records `undirected_shapes=True` emits -- because this function is called on both."""
    deg: dict[int, set] = {}
    for e in edges:
        a, b = (e["a"], e["b"]) if isinstance(e, dict) else (e[0], e[1])
        deg.setdefault(a, set()).add(b)
        deg.setdefault(b, set()).add(a)
    hist: dict[int, int] = {}
    for i in range(len(nodes)):
        d = len(deg.get(i, ()))
        hist[d] = hist.get(d, 0) + 1
    pairs = {(d["a"], d["b"]) for d in directed}
    oneway = sum(1 for a, b in pairs if (b, a) not in pairs)
    lane_hist: dict[int, int] = {}
    lane_km = 0.0
    edge_km = 0.0
    for d in directed:
        n = int(d.get("lanes", 1))
        lane_hist[n] = lane_hist.get(n, 0) + 1
        ln = float(d.get("length_m") or 0.0)
        if not ln:
            ln = math.dist(nodes[d["a"]], nodes[d["b"]])
        lane_km += ln * n / 1000.0
        edge_km += ln / 1000.0
    return {"n_nodes": len(nodes), "n_edges": len(edges), "n_directed_edges": len(directed),
            "oneway_directed_edges": oneway,
            "oneway_share": round(oneway / max(1, len(directed)), 4),
            "degree_histogram": dict(sorted(hist.items())),
            "intersections_deg_ge3": sum(v for k, v in hist.items() if k >= 3),
            "dead_ends": hist.get(1, 0),
            "lane_histogram": dict(sorted(lane_hist.items())),
            "lane_km": round(lane_km, 3), "directed_edge_km": round(edge_km, 3),
            "mean_lanes": round(sum(k * v for k, v in lane_hist.items())
                                / max(1, sum(lane_hist.values())), 3),
            # NB "n_signal_nodes", not "signal_nodes" -- the latter is the node-index list
            "n_signal_nodes": len(signal_nodes)}


def import_net(net_path: str, **kw) -> tuple[list, list, dict]:
    """Convenience: read a `.net.xml` from disk and convert it (see `net_to_network`).

    `signals=True` also switches the READ to `withPrograms=True`, because sumolib drops every
    `<tlLogic>` otherwise and the caller would get a silently signal-free city."""
    return net_to_network(read_net(net_path, programs=bool(kw.get("signals"))), **kw)


def net_cache_path(bbox, cache_dir: str, args=NETCONVERT_OSM_ARGS) -> str:
    """`.net.xml` cache path beside the OSM XML cache, keyed by bbox AND by the netconvert flags
    (changing the flag set must not silently reuse a net built with the old one)."""
    key = hashlib.sha256(",".join(f"{b:.5f}" for b in bbox).encode()).hexdigest()[:16]
    tag = hashlib.sha256(" ".join(str(a) for a in args).encode()).hexdigest()[:6]
    return os.path.join(cache_dir, f"net_{key}_{tag}.net.xml")


def import_city(city_or_bbox, cache_dir: str = "datasets/_osmcache", *, max_nodes: int = 0,
                shapes: bool = True, turns: bool = False, strong: bool = False, geo: bool = True,
                args=NETCONVERT_OSM_ARGS, extra_args=(), signals: bool = False,
                overwrite: bool = False) -> tuple[list, list, dict]:
    """City name (or bbox) -> netconvert -> (nodes, edges, info), in osm.py's projection frame.

    Both heavy steps are cached on disk: the Overpass extract (`fetch_osm`) and the `.net.xml`
    (`run_netconvert`). `geo=True` re-projects into the frame `osm.py` derives from the SAME
    extract's road ways, so this network, the raw-OSM one and the building footprints are all
    registered; the bbox is used as the alignment gate."""
    if isinstance(city_or_bbox, str):
        if city_or_bbox not in CITY_BBOXES:
            raise ValueError(f"unknown city {city_or_bbox!r}; have {sorted(CITY_BBOXES)}")
        bbox = CITY_BBOXES[city_or_bbox]
    else:
        bbox = tuple(float(v) for v in city_or_bbox)
    xml_text = fetch_osm(bbox, cache_dir)         # cached; netconvert then reads that SAME file
    osm_path = osm_cache_path(bbox, cache_dir)
    net_path = net_cache_path(bbox, cache_dir, tuple(args) + tuple(extra_args))
    run_netconvert(osm_path, net_path, args=args, extra_args=extra_args, overwrite=overwrite)
    projection = road_projection(xml_text) if geo else None
    expect = None
    if projection:
        lon0, lat0 = projection["lon0"], projection["lat0"]
        expect = [(bbox[0] - lon0) * projection["kx"], (bbox[1] - lat0) * projection["ky"],
                  (bbox[2] - lon0) * projection["kx"], (bbox[3] - lat0) * projection["ky"]]
    nodes, edges, info = net_to_network(read_net(net_path, programs=signals),
                                        projection=projection,
                                        expect_bbox_xy=expect, max_nodes=max_nodes,
                                        shapes=shapes, turns=turns, strong=strong,
                                        signals=signals)
    info["bbox"] = list(bbox)
    info["net_path"] = net_path
    info["network_meta"] = {"source": "netconvert", "schema": NETWORK_SCHEMA_VERSION,
                            "bbox": list(bbox), "projection": projection,
                            "road_bbox": info["road_bbox"], "directed": True,
                            "netconvert_args": [str(a) for a in args] + [str(a) for a in
                                                                        extra_args],
                            "signal_nodes": len(info["signal_nodes"]),
                            "oneway_share": info["oneway_share"], "lane_km": info["lane_km"]}
    return nodes, edges, info


def signal_document(nodes: list, edges: list, info: dict, *, buildings: list | None = None) -> dict:
    """`osm.network_document` plus the optional `signal_programs` layer.

    Kept HERE rather than folded into `osm.network_document` for one deliberate reason: the base
    document's key set is what every existing consumer reads, and a document written without
    `signals=True` must be byte-identical to what it always was. `info` carrying no
    `signal_programs` therefore adds no key at all, and a reader that predates the layer sees the
    exact document it saw before."""
    doc = network_document(nodes, edges, info, buildings=buildings)
    if info.get("signal_programs"):
        doc["signal_programs"] = info["signal_programs"]
    return doc


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="SUMO .net.xml (netconvert) -> custom-network JSON")
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--city", choices=sorted(CITY_BBOXES))
    g.add_argument("--bbox", help="minLon,minLat,maxLon,maxLat")
    g.add_argument("--net", help="an existing .net.xml (skips fetch + netconvert)")
    p.add_argument("--out", required=True)
    p.add_argument("--cache", default="datasets/_osmcache")
    p.add_argument("--max-nodes", type=int, default=0,
                   help="0 (default) = no node cap; netconvert output has no fake curve nodes")
    p.add_argument("--turns", action="store_true", help="export permitted successors per edge")
    p.add_argument("--signals", action="store_true",
                   help="export the REAL <tlLogic> traffic-light programs (phase state strings, "
                        "durations, minDur/maxDur, offset) plus the movement -> phase-index "
                        "mapping, as a `signal_programs` layer")
    p.add_argument("--strong", action="store_true",
                   help="keep only the largest STRONGLY connected component (do this whenever the "
                        "consumer honours one-ways: a clipped extract leaves junctions with no "
                        "way out)")
    p.add_argument("--no-shapes", action="store_true", help="drop edge shape polylines")
    p.add_argument("--no-geo", action="store_true",
                   help="keep the net's own metric coordinates (do not re-project into the "
                        "osm.py frame; only correct when nothing else shares that frame)")
    p.add_argument("--frame-city", choices=sorted(CITY_BBOXES),
                   help="with --net: re-project into THIS city's osm.py frame (use it whenever the "
                        "net will share a map with osm.py roads or building footprints)")
    p.add_argument("--buildings", metavar="POLY_XML",
                   help="with --net: a SUMO polygon additional-file whose type=\"building\" "
                        "footprints are projected with THIS net's transform and emitted as the "
                        "document's `buildings` layer. Gated on landing on the junctions")
    p.add_argument("--stats", action="store_true", help="print topology fidelity stats")
    a = p.parse_args(argv)
    buildings = None
    if a.net:
        frame = None
        if a.frame_city:
            frame = road_projection(fetch_osm(CITY_BBOXES[a.frame_city], a.cache))
        nodes, edges, info = import_net(a.net, max_nodes=a.max_nodes, shapes=not a.no_shapes,
                                        turns=a.turns, strong=a.strong, projection=frame,
                                        signals=a.signals)
        if a.buildings:
            buildings, binfo = scene_from_net(a.net, a.buildings, projection=frame)
            info["buildings_stats"] = binfo
    else:
        if a.buildings:
            p.error("--buildings goes with --net (a SUMO .poly.xml belongs to a SUMO net); the "
                    "raw-OSM path takes its footprints from the same Overpass extract via "
                    "`python -m scms_sim_ref.mock_pipeline.osm --buildings`")
        target = a.city if a.city else [float(v) for v in a.bbox.split(",")]
        nodes, edges, info = import_city(target, a.cache, max_nodes=a.max_nodes,
                                         shapes=not a.no_shapes, turns=a.turns, strong=a.strong,
                                         geo=not a.no_geo, signals=a.signals)
    doc = signal_document(nodes, edges, info, buildings=buildings)
    with open(a.out, "w", encoding="utf-8") as fh:
        json.dump(doc, fh)
    keys = ("n_nodes", "n_edges", "n_directed_edges", "oneway_share", "intersections_deg_ge3",
            "degree_histogram", "lane_histogram", "lane_km", "n_signal_nodes", "dropped_classes")
    print(f"wrote {a.out}: " + json.dumps({k: info[k] for k in keys if k in info}))
    if a.signals:
        print("signal_programs: " + json.dumps(info.get("signal_program_stats", {}),
                                               sort_keys=True, default=str))
    if a.stats:
        print(json.dumps({k: v for k, v in info.items()
                          if k not in ("directed_edges", "network_meta", "signal_programs",
                                       "road_surface")},
                         indent=1, sort_keys=True, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
