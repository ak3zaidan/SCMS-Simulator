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
`signal_nodes`. `roads.edges_from_directed` turns those records into engine edge specs, so a
netconvert import loads into `CustomNetwork` directly. Two conventions are load-bearing there:
`shape` holds INTERMEDIATE vertices only (junction coordinates implied), and the two directions of
one physical road must carry mirror-image shapes -- netconvert does NOT (each carriageway has its
own offset polyline), so `_canonicalise_shapes` picks one. Pass `strong=True` for a directed
consumer: a bbox-clipped extract otherwise leaves junctions a vehicle can enter and never leave.

CLI:
    python -m scms_sim_ref.mock_pipeline.netimport --city ingolstadt --out ing_net.json
    python -m scms_sim_ref.mock_pipeline.netimport --city ingolstadt --strong --turns --out d.json
    python -m scms_sim_ref.mock_pipeline.netimport --net some.net.xml --out net.json --no-geo
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

from .osm import (CITY_BBOXES, _DROP_ORDER, NETWORK_SCHEMA_VERSION, fetch_osm, network_document,
                  osm_cache_path, road_projection)

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


def _canonicalise_shapes(directed: list, coords: list) -> int:
    """ONE geometry per physical road, and re-measure the lengths against it.

    netconvert models a two-way street as two edges, each carrying its OWN carriageway geometry --
    offset sideways from the centreline, and not a mirror image of its partner (MEASURED: 105 of
    154 two-way pairs in the Ingolstadt import). The engine's model is the opposite way round: one
    centreline per road, with per-direction lane offsets applied on top (`roads.py` stores exactly
    one `edge_shape` per undirected key and REJECTS a second, different one). So the importer has
    to choose, and it chooses deterministically: the low->high direction's polyline is the road's
    geometry, and the high->low record carries its exact reverse.

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


def read_net(net_path: str):
    """`sumolib.net.readNet` with a useful error when sumolib is missing."""
    try:
        import sumolib                          # noqa: PLC0415  (optional heavy dependency)
    except ImportError:                          # pragma: no cover - toolchain always has it
        raise RuntimeError("sumolib is required to import a .net.xml (pip install sumolib, or use "
                           "the SUMO_HOME/tools copy)") from None
    if not os.path.exists(net_path):
        raise FileNotFoundError(net_path)
    return sumolib.net.readNet(net_path)


def net_to_network(net, *, projection: dict | None = None, expect_bbox_xy: list | None = None,
                   max_nodes: int = 0, shapes: bool = True, turns: bool = False,
                   strong: bool = False, vclass: str | None = "passenger",
                   min_edge_m: float = 1.0, round_m: int = 1) -> tuple[list, list, dict]:
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
    the count is reported as `weak_only_nodes` either way."""
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
        if strong and len(strong_set) >= 2:
            keep_ids = sorted(i for i in remap if i in strong_set)
            remap = {old: new for new, old in enumerate(keep_ids)}
            out_nodes = [coords[old] for old in keep_ids]
            weak_only = 0                        # ... they are gone now
        out_edges = [[remap[a], remap[b], round(sp, 1)]
                     for (a, b), sp in sorted(undirected.items()) if a in remap and b in remap]
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
        info = dict(stats)
        info.update(kept_nodes=len(out_nodes), kept_edges=len(out_edges),
                    directed_edges=kept_dir, signal_nodes=sig,
                    dropped_classes=sorted(dropped), self_loops=self_loops,
                    short_edges_dropped=too_short, speeds_clamped=clamped,
                    strong_component_nodes=len(strong_set), weak_only_nodes=weak_only,
                    strongly_connected=bool(strong),
                    projection=projection,
                    road_bbox=[min(p[0] for p in out_nodes), min(p[1] for p in out_nodes),
                               max(p[0] for p in out_nodes), max(p[1] for p in out_nodes)])
        info["alignment"] = _assert_alignment(out_nodes, expect_bbox_xy)
        info.update(topology_stats(out_nodes, out_edges, kept_dir, sig))
        return out_nodes, out_edges, info
    raise ValueError("no connected component could be built from this .net.xml")


def topology_stats(nodes: list, edges: list, directed: list, signal_nodes=()) -> dict:
    """The fidelity numbers this import is judged on: one-way share, degree distribution,
    intersection count, per-edge lane distribution and lane-km. Same shape for either import path
    so the raw-OSM graph can be scored against the netconvert net as ground truth."""
    deg: dict[int, set] = {}
    for a, b, *_r in edges:
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
    """Convenience: read a `.net.xml` from disk and convert it (see `net_to_network`)."""
    return net_to_network(read_net(net_path), **kw)


def net_cache_path(bbox, cache_dir: str, args=NETCONVERT_OSM_ARGS) -> str:
    """`.net.xml` cache path beside the OSM XML cache, keyed by bbox AND by the netconvert flags
    (changing the flag set must not silently reuse a net built with the old one)."""
    key = hashlib.sha256(",".join(f"{b:.5f}" for b in bbox).encode()).hexdigest()[:16]
    tag = hashlib.sha256(" ".join(str(a) for a in args).encode()).hexdigest()[:6]
    return os.path.join(cache_dir, f"net_{key}_{tag}.net.xml")


def import_city(city_or_bbox, cache_dir: str = "datasets/_osmcache", *, max_nodes: int = 0,
                shapes: bool = True, turns: bool = False, strong: bool = False, geo: bool = True,
                args=NETCONVERT_OSM_ARGS, extra_args=(),
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
    nodes, edges, info = net_to_network(read_net(net_path), projection=projection,
                                        expect_bbox_xy=expect, max_nodes=max_nodes,
                                        shapes=shapes, turns=turns, strong=strong)
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
    p.add_argument("--stats", action="store_true", help="print topology fidelity stats")
    a = p.parse_args(argv)
    if a.net:
        frame = None
        if a.frame_city:
            frame = road_projection(fetch_osm(CITY_BBOXES[a.frame_city], a.cache))
        nodes, edges, info = import_net(a.net, max_nodes=a.max_nodes, shapes=not a.no_shapes,
                                        turns=a.turns, strong=a.strong, projection=frame)
    else:
        target = a.city if a.city else [float(v) for v in a.bbox.split(",")]
        nodes, edges, info = import_city(target, a.cache, max_nodes=a.max_nodes,
                                         shapes=not a.no_shapes, turns=a.turns, strong=a.strong,
                                         geo=not a.no_geo)
    doc = network_document(nodes, edges, info)
    with open(a.out, "w", encoding="utf-8") as fh:
        json.dump(doc, fh)
    keys = ("n_nodes", "n_edges", "n_directed_edges", "oneway_share", "intersections_deg_ge3",
            "degree_histogram", "lane_histogram", "lane_km", "n_signal_nodes", "dropped_classes")
    print(f"wrote {a.out}: " + json.dumps({k: info[k] for k in keys if k in info}))
    if a.stats:
        print(json.dumps({k: v for k, v in info.items()
                          if k not in ("directed_edges", "network_meta")},
                         indent=1, sort_keys=True, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
