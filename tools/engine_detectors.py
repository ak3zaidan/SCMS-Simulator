#!/usr/bin/env python
"""Count induction-loop crossings from TRAJECTORIES, so the Python engine can be graded by the
same detector gate the MOSAIC/SUMO path is.

WHY THIS EXISTS. ``tools/sumo_realism.py --ref-counts`` grades a run against real measured loop
counts, but it reads SUMO's own ``<e1Detector>`` output, and only SUMO writes that. The Python
engine (``mock_pipeline``) emits vehicle POSITIONS, not detector events -- it has no lanes and no
loops -- so until now it could not be pointed at the same real-world reference at all. Every
validation in this repository was of the Java/MOSAIC path; the engine that writes the datasets a
researcher actually uses had never been compared to a measurement.

This tool closes that by counting crossings GEOMETRICALLY and writing an E1-output-shaped XML, so
``sumo_realism.py`` grades it with the same station aggregation, the same FHWA criteria and the same
reference file, unchanged and unaware of the producer.

THE GATE. Each ``<e1Detector>`` is reduced to a segment of the lane it sits on:

    P  the point at offset ``pos`` along the lane's own polyline (SUMO's convention; a negative
       ``pos`` counts back from the lane end)
    T  the unit tangent there = the direction of travel
    N  the left normal
    W  the half-width the loop is sensitive over, HALF A LANE by default -- never more, because a
       station's count is the SUM over its per-lane loops and a wider gate lets one vehicle be
       counted by its neighbour's loop as well, silently doubling the station

A sample pair (A, B) of ONE vehicle at consecutive times counts as one crossing when

    (A-P).T < 0 <= (B-P).T           it went from behind the loop to on/past it (half-open: a
                                     vehicle stopped exactly on the loop is counted once, not twice)
    |(C-P).N| <= W                   where C is the interpolated crossing point -- it passed over
                                     the loop, not over some other road that happens to cross here
    (B-A).T / |B-A| >= COS_MAX       it was travelling ALONG the lane, not across it: without this,
                                     a vehicle on the cross street of a signalised junction trips
                                     every loop it drives over

This is a FRONT-crossing count. SUMO's ``nVehContrib`` counts vehicles that completely passed the
loop within the interval, which is the same event one vehicle length later; over a full hour the
difference is the handful of vehicles straddling the window edge.

WHAT MAKES THE NUMBER TRUSTWORTHY. The counter is validated against the thing it is imitating: run
it on the FROZEN SUMO TRACE (``--trace``) -- the very positions SUMO reported, sampled at the
artifact's dt -- and compare with SUMO's own E1 output for the same run. That measures the counter
plus the sampling rate and nothing else. Any FURTHER difference when it is then run on the ENGINE's
emitted dataset (``--dataset``) is the mobility adapter, which is the quantity of interest.

    # 1. the counter's own error, against SUMO's loops on the same mobility
    python tools/engine_detectors.py --net ingolstadt.net.xml --det-add InTAS_E1.add.xml \
        --trace intas_hour.trace --out counts_trace.xml
    python tools/sumo_realism.py --det-out counts_trace.xml --det-add InTAS_E1.add.xml \
        --ref-det-out fz_InTAS_Detectors_Output.xml --begin 25200 --end 28800

    # 2. the engine, against reality
    python tools/engine_detectors.py --net ingolstadt.net.xml --det-add InTAS_E1.add.xml \
        --dataset datasets/py_intas_hour --out counts_engine.xml
    python tools/sumo_realism.py --det-out counts_engine.xml --det-add InTAS_E1.add.xml \
        --ref-counts refcounts.json

Positions are read in the SUMO network's own metric frame. The engine's ``--road sumo`` import
applies the IDENTITY transform unless ``--sumo-frame-city`` re-projects it, so an engine dataset
frozen from a net is already in that frame; ``--offset`` is there for the case where it is not.

Standalone: stdlib only, plus ``sumolib`` (from ``$SUMO_HOME/tools``) for the lane geometry --
which is not optional here, since the whole point is to use the net's own lane polylines rather than
a reconstruction. Reading is streamed, so a multi-GB trace or emissions file does not have to fit in
memory.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys
import xml.etree.ElementTree as ET



# --- gate geometry (module level so they are documented, importable and testable) ---------------
DEFAULT_HALF_WIDTH_M = 1.6     # half a SUMO lane (3.2 m). See the module docstring: wider than this
                               # and one vehicle is counted by the neighbouring lane's loop too.
COS_ALONG_MIN = 0.5            # travel direction within 60 deg of the lane's -- rejects a vehicle
                               # crossing the loop sideways on the conflicting arm of a junction
CELL_M = 100.0                 # spatial index cell for the detector lookup
RIVAL_PAD_M = 0.0              # slack added to (w_a + w_b) when deciding two gates are rivals; see
                               # build_rival_groups(). 0.0 = the gates' sensitive strips must
                               # actually overlap before either is allowed to steal the other's
                               # vehicle.
MAX_STEP_M = CELL_M            # a sample pair further apart than this is not a movement we can
                               # interpolate through (a teleport, or a resumed track); skipped and
                               # counted in `skipped_long_steps`. It is EQUAL TO the cell size on
                               # purpose: every point of a segment no longer than one cell is within
                               # one cell of its start, so a lookup on the start's cell -- into an
                               # index that already replicates each gate into its 3x3 neighbourhood
                               # -- is a provable superset of the gates the segment can cross.


def _maybe_sumolib():
    home = os.environ.get("SUMO_HOME")
    if home:
        tools = os.path.join(home, "tools")
        if os.path.isdir(tools) and tools not in sys.path:
            sys.path.append(tools)
    import sumolib                                    # noqa: PLC0415
    return sumolib


# ------------------------------------------------------------------------------------------------
# detector geometry
# ------------------------------------------------------------------------------------------------
def build_gates(net_path: str, det_add: str, half_width_m: float = DEFAULT_HALF_WIDTH_M) -> dict:
    """``{detector_id: {"p":(x,y), "t":(tx,ty), "w":half_width, "station":…, "lane":…}}``.

    The half-width is the SMALLER of the requested one and half the lane's own width, so a narrow
    lane cannot have its loop reach into the next one.
    """
    sumolib = _maybe_sumolib()
    net = sumolib.net.readNet(net_path)
    gates: dict[str, dict] = {}
    missing_lanes: list[str] = []
    for _ev, el in ET.iterparse(det_add, events=("end",)):
        if el.tag not in ("e1Detector", "inductionLoop"):
            el.clear()
            continue
        did, lane_id = el.get("id"), el.get("lane")
        pos = float(el.get("pos", "0") or 0.0)
        station = el.get("name") or did
        el.clear()
        if not did or not lane_id:
            continue
        try:
            lane = net.getLane(lane_id)
        except Exception:
            missing_lanes.append(lane_id)
            continue
        shape = [(float(x), float(y)) for x, y in lane.getShape()]
        length = lane.getLength()
        if len(shape) < 2 or length <= 0:
            missing_lanes.append(lane_id)
            continue
        off = pos if pos >= 0 else length + pos       # SUMO: a negative pos counts back from the end
        off = min(max(off, 0.0), length)
        p, t = _point_and_tangent(shape, off)
        w = min(float(half_width_m), max(0.1, lane.getWidth() / 2.0))
        gates[did] = {"p": p, "t": t, "w": w, "station": station, "lane": lane_id,
                      "edge": lane_id.rsplit("_", 1)[0] if "_" in lane_id else lane_id}
    if missing_lanes:
        raise ValueError(f"{len(missing_lanes)} detector lane(s) are not in {net_path}: "
                         f"{missing_lanes[:5]} -- the .add.xml and the .net.xml do not match")
    return gates


def build_rival_groups(gates: dict, pad_m: float = RIVAL_PAD_M) -> dict[str, int]:
    """``{detector_id: group_id}`` for gates that can each see the OTHER one's road.

    WHY. SUMO's ``<e1Detector>`` is a LANE-BOUND device: it sees a vehicle only if the vehicle's
    ``laneID`` is the detector's lane, so two loops on different roads never share a vehicle no
    matter how close the roads run. This counter has positions and no lane identity, so where two
    roads run closer together than the loops' own sensitive strips, one vehicle trips both loops and
    the station sums it twice.

    That is not hypothetical and it is not widespread. In the InTAS layout it happens at exactly TWO
    of the 19,110 gate pairs -- ``1010_6``/``1010_8`` (2.47 m apart) and ``1010_7``/``1010_9``
    (2.51 m apart), edge ``172515813`` against edge ``201278218#0``, tangents 13.6 deg apart -- and
    those four loops carry the whole of station 1010's measured +29.9% over-count against SUMO's own
    loops on the same run.

    Two gates are RIVALS when they sit on different EDGES (same-edge lanes are already handled by
    capping the half-width at half a lane), their gate points are no further apart than the sum of
    their half-widths, and their tangents agree to within the same 60 deg the direction gate uses.
    Rivalry is transitive-closed into groups, so a chain of three near-parallel roads resolves as
    one group rather than as two overlapping pairs.
    """
    ids = sorted(gates)
    parent = {d: d for d in ids}

    def find(d):
        while parent[d] != d:
            parent[d] = parent[parent[d]]
            d = parent[d]
        return d

    for i, a in enumerate(ids):
        ga = gates[a]
        for b in ids[i + 1:]:
            gb = gates[b]
            if ga["edge"] == gb["edge"]:
                continue
            if math.hypot(ga["p"][0] - gb["p"][0], ga["p"][1] - gb["p"][1]) > ga["w"] + gb["w"] + pad_m:
                continue
            if ga["t"][0] * gb["t"][0] + ga["t"][1] * gb["t"][1] < COS_ALONG_MIN:
                continue
            ra, rb = find(a), find(b)
            if ra != rb:
                parent[ra] = rb
    by_root: dict[str, list[str]] = {}
    for d in ids:
        by_root.setdefault(find(d), []).append(d)
    roots = {r: i for i, r in enumerate(sorted(r for r, m in by_root.items() if len(m) > 1))}
    return {d: roots[r] for r, m in by_root.items() if r in roots for d in m}


def _point_and_tangent(shape, offset):
    """Point at ``offset`` along a polyline, plus the unit tangent of the segment it lands on."""
    acc = 0.0
    for (x0, y0), (x1, y1) in zip(shape, shape[1:]):
        seg = math.hypot(x1 - x0, y1 - y0)
        if seg <= 0:
            continue
        if acc + seg >= offset or (x1, y1) == shape[-1]:
            u = min(1.0, max(0.0, (offset - acc) / seg))
            return (x0 + u * (x1 - x0), y0 + u * (y1 - y0)), ((x1 - x0) / seg, (y1 - y0) / seg)
        acc += seg
    x0, y0 = shape[-2]
    x1, y1 = shape[-1]
    seg = math.hypot(x1 - x0, y1 - y0) or 1.0
    return (x1, y1), ((x1 - x0) / seg, (y1 - y0) / seg)


# ------------------------------------------------------------------------------------------------
# position sources -- both stream, and both yield (vehicle_key, t, x, y) in time order per vehicle
# ------------------------------------------------------------------------------------------------
def iter_trace(path: str):
    """A frozen ``scms-sumo-trace/1`` artifact. Rows are already sorted by (step, idx)."""
    with open(path, "r", encoding="utf-8") as fh:
        head = fh.readline().rstrip("\n")
        if not head.startswith("#scms-sumo-trace/"):
            raise ValueError(f"{path}: not a frozen SUMO trace (first line {head!r})")
        meta = json.loads(fh.readline()[6:])
        dt = float(meta.get("dt", 1.0))
        t0 = float(meta.get("step0_sim_time", dt))
        n_veh = int(fh.readline().split()[1])
        for _ in range(n_veh):
            fh.readline()
        fh.readline()                                  # #rows
        for line in fh:
            if not line.strip():
                continue
            step_s, idx_s, x_s, y_s, _v, _a = line.split()
            yield int(idx_s), t0 + int(step_s) * dt, float(x_s), float(y_s)


def iter_dataset(path: str, field: str = "true"):
    """``ground_truth/gt_emissions_sample.jsonl`` from EITHER producer.

    ``true_x``/``true_y`` is the simulator's own position. ``claimed_x``/``claimed_y`` is what went
    on the air, which an attacker falsifies -- a count taken off it measures the attack, not the
    traffic, so ``true`` is the default and the choice is recorded in the output.
    """
    fx, fy = f"{field}_x", f"{field}_y"
    p = os.path.join(path, "ground_truth", "gt_emissions_sample.jsonl") \
        if os.path.isdir(path) else path
    with open(p, "r", encoding="utf-8") as fh:
        for line in fh:
            if not line.strip():
                continue
            r = json.loads(line)
            x, y = r.get(fx), r.get(fy)
            if x is None or y is None:
                continue
            yield r.get("true_vehicle_id", ""), float(r.get("t", 0.0)), float(x), float(y)


# ------------------------------------------------------------------------------------------------
# counting
# ------------------------------------------------------------------------------------------------
def count_crossings(samples, gates: dict, *, offset=(0.0, 0.0), cell_m: float = CELL_M,
                    max_step_m: float = MAX_STEP_M, progress_every: int = 0,
                    rival_groups: dict[str, int] | None = None) -> dict:
    """Stream ``(veh, t, x, y)`` and return per-detector crossing counts.

    Samples for one vehicle must arrive in time order; they may be interleaved with other vehicles'
    (the frozen trace is sorted by step, not by vehicle, and so is an emissions file).

    The spatial index is a dict of ``cell_m`` cells, each gate written into its own cell and the
    eight around it. A crossing point lies ON the segment, so for a segment no longer than one cell
    it is within one cell of the segment's START -- which makes a single lookup on the start cell a
    provable superset of the gates that pair can cross. ``max_step_m > cell_m`` would break that, so
    they are tied together.

    ``rival_groups`` (from :func:`build_rival_groups`; default ``None`` = OFF, so the plain gate is
    bit-for-bit unchanged) makes a group of mutually-overlapping cross-edge loops EXCLUSIVE: a
    vehicle is awarded to at most one loop of the group -- the one it passed CLOSEST TO laterally --
    instead of to every loop whose sensitive strip it happened to enter. That restores what SUMO
    gets for free from lane membership. Because the two loops of a group need not be crossed on the
    same sample pair, the winner is resolved at the END of the stream, not on the first hit.
    """
    if max_step_m > cell_m:
        raise ValueError(f"max_step_m ({max_step_m}) must not exceed cell_m ({cell_m}); the "
                         f"single-cell lookup stops being a superset above that")
    ox, oy = offset
    index: dict[tuple[int, int], list[str]] = {}
    for did, g in gates.items():
        cx, cy = int(g["p"][0] // cell_m), int(g["p"][1] // cell_m)
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                index.setdefault((cx + dx, cy + dy), []).append(did)

    counts = {did: 0 for did in gates}
    last: dict = {}
    n_samples = n_pairs = skipped_long = 0
    t_min = t_max = None
    rivals = rival_groups or {}
    # (vehicle, group) -> (best |lateral offset|, winning detector). Only gates that are in a rival
    # group ever land here, so on a layout with none this dict stays empty and costs nothing.
    contested: dict[tuple, tuple[float, str]] = {}
    for veh, t, x, y in samples:
        x += ox
        y += oy
        n_samples += 1
        t_min = t if t_min is None else min(t_min, t)
        t_max = t if t_max is None else max(t_max, t)
        prev = last.get(veh)
        last[veh] = (t, x, y)
        if prev is None:
            continue
        pt, px, py = prev
        if t <= pt:
            continue
        dx, dy = x - px, y - py
        seg_len = math.hypot(dx, dy)
        if seg_len <= 0:
            continue
        if seg_len > max_step_m:
            skipped_long += 1
            continue
        n_pairs += 1
        cand = index.get((int(px // cell_m), int(py // cell_m)))
        if not cand:
            continue
        ux, uy = dx / seg_len, dy / seg_len
        for did in cand:
            g = gates[did]
            tx, ty = g["t"]
            if ux * tx + uy * ty < COS_ALONG_MIN:
                continue
            gx, gy = g["p"]
            sa = (px - gx) * tx + (py - gy) * ty
            sb = (x - gx) * tx + (y - gy) * ty
            if not (sa < 0.0 <= sb):
                continue
            denom = sb - sa
            if denom <= 0:
                continue
            u = -sa / denom
            cxp = px + u * dx - gx
            cyp = py + u * dy - gy
            lateral = abs(-cxp * ty + cyp * tx)        # |(C-P).N|, N = left normal of T
            if lateral > g["w"]:
                continue
            grp = rivals.get(did)
            if grp is None:
                counts[did] += 1
            else:
                key = (veh, grp)
                best = contested.get(key)
                if best is None or lateral < best[0]:
                    contested[key] = (lateral, did)
        if progress_every and n_samples % progress_every == 0:
            print(f"  ... {n_samples:,} samples, {sum(counts.values()):,} crossings",
                  file=sys.stderr, flush=True)
    n_contested = len(contested)
    for _lateral, did in contested.values():
        counts[did] += 1
    return {"counts": counts, "n_samples": n_samples, "n_pairs": n_pairs,
            "n_vehicles": len(last), "skipped_long_steps": skipped_long,
            "t_min": t_min, "t_max": t_max,
            "n_rival_gates": len(rivals), "n_rival_groups": len(set(rivals.values())),
            "n_contested_awards": n_contested}


def write_e1_xml(path: str, counts: dict[str, int], begin: float, end: float, gates: dict,
                 note: str = "") -> None:
    """Write an E1-output-shaped XML that ``sumo_realism.parse_e1_output`` reads unchanged.

    One interval per detector spanning the whole window. Only ``id``/``begin``/``end``/
    ``nVehContrib`` are read by that parser; ``flow`` and ``speed`` are written for a human reading
    the file and are derived, not measured.
    """
    dur = max(1e-9, end - begin)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n')
        fh.write(f"<!-- geometric detector crossings from trajectories, "
                 f"tools/engine_detectors.py{(': ' + note) if note else ''} -->\n")
        fh.write('<detector xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" '
                 'xsi:noNamespaceSchemaLocation="http://sumo.dlr.de/xsd/det_e1_file.xsd">\n')
        for did in sorted(counts):
            n = counts[did]
            fh.write(f'    <interval begin="{begin:.2f}" end="{end:.2f}" id="{did}" '
                     f'nVehContrib="{n}" flow="{n * 3600.0 / dur:.2f}" occupancy="-1.00" '
                     f'speed="-1.00" harmonicMeanSpeed="-1.00" length="-1.00" nVehEntered="{n}"/>\n')
        fh.write("</detector>\n")


# ------------------------------------------------------------------------------------------------
def main(argv=None) -> int:
    p = argparse.ArgumentParser(
        prog="python tools/engine_detectors.py", description=__doc__.split("\n\n")[0],
        formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--net", required=True, help="the SUMO .net.xml the loops sit on")
    p.add_argument("--det-add", required=True, help="E1 additional file (InTAS_E1.add.xml)")
    src = p.add_mutually_exclusive_group(required=True)
    src.add_argument("--trace", help="a frozen scms-sumo-trace artifact (SUMO's own positions)")
    src.add_argument("--dataset", help="a dataset dir (or gt_emissions_sample.jsonl directly)")
    p.add_argument("--field", default="true", choices=("true", "claimed"),
                   help="dataset only: which position to count (default true = the simulator's)")
    p.add_argument("--out", required=True, help="E1-output-shaped XML to write")
    p.add_argument("--json", dest="json_out", default=None, help="also write the summary JSON here")
    p.add_argument("--half-width", type=float, default=DEFAULT_HALF_WIDTH_M,
                   help=f"loop half-width (m), capped at half the lane (default "
                        f"{DEFAULT_HALF_WIDTH_M})")
    p.add_argument("--offset", default="0,0", help="x,y added to every position before counting")
    p.add_argument("--begin", type=float, default=None,
                   help="window start written into the XML (default: the data's own first sample)")
    p.add_argument("--end", type=float, default=None, help="window end (default: last sample)")
    p.add_argument("--time-offset", type=float, default=0.0,
                   help="added to every sample time before the --begin/--end window is applied")
    p.add_argument("--progress", type=int, default=0, help="print progress every N samples")
    p.add_argument("--exclusive-gates", action="store_true",
                   help="award a vehicle to at most ONE of a group of overlapping loops on "
                        "DIFFERENT edges -- the one it passed closest to. SUMO's own loops get "
                        "this for free from lane membership; a position-only counter does not, and "
                        "where two roads run closer together than the loops' sensitive strips both "
                        "loops see the same vehicle. Default OFF so the plain gate is unchanged.")
    a = p.parse_args(argv)

    ox, oy = (float(v) for v in a.offset.split(","))
    gates = build_gates(a.net, a.det_add, half_width_m=a.half_width)
    rival_groups = build_rival_groups(gates) if a.exclusive_gates else None
    src_desc = a.trace or a.dataset
    stream = iter_trace(a.trace) if a.trace else iter_dataset(a.dataset, a.field)
    if a.time_offset:
        stream = ((v, t + a.time_offset, x, y) for v, t, x, y in stream)
    if a.begin is not None or a.end is not None:
        lo = -math.inf if a.begin is None else a.begin
        hi = math.inf if a.end is None else a.end
        stream = ((v, t, x, y) for v, t, x, y in stream if lo <= t <= hi)

    res = count_crossings(stream, gates, offset=(ox, oy), progress_every=a.progress,
                          rival_groups=rival_groups)
    begin = a.begin if a.begin is not None else (res["t_min"] or 0.0)
    end = a.end if a.end is not None else (res["t_max"] or 0.0)
    write_e1_xml(a.out, res["counts"], begin, end, gates, note=os.path.basename(str(src_desc)))

    by_station: dict[str, int] = {}
    for did, n in res["counts"].items():
        st = gates[did]["station"]
        by_station[st] = by_station.get(st, 0) + n
    summary = {"source": str(src_desc), "field": (a.field if a.dataset else "trace"),
               "net": a.net, "det_add": a.det_add, "out": a.out,
               "half_width_m": a.half_width, "cos_along_min": COS_ALONG_MIN,
               "exclusive_gates": bool(a.exclusive_gates),
               "n_rival_gates": res.get("n_rival_gates", 0),
               "n_rival_groups": res.get("n_rival_groups", 0),
               "n_contested_awards": res.get("n_contested_awards", 0),
               "n_detectors": len(gates), "n_samples": res["n_samples"],
               "n_pairs": res["n_pairs"], "n_vehicles": res["n_vehicles"],
               "skipped_long_steps": res["skipped_long_steps"],
               "t_min": res["t_min"], "t_max": res["t_max"],
               "window": [begin, end], "total_crossings": int(sum(res["counts"].values())),
               "by_station": {k: int(v) for k, v in sorted(by_station.items())}}
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(summary, fh, indent=1, sort_keys=True)
    print(json.dumps({k: v for k, v in summary.items() if k != "by_station"},
                     indent=1, sort_keys=True))
    print(f"total crossings {summary['total_crossings']:,} over {len(by_station)} stations "
          f"-> {a.out}")
    return 0


if __name__ == "__main__":                            # pragma: no cover
    sys.exit(main())
