"""Realism benchmark harness: score a generated dataset against pinned real-world references.

Where `calibration.py` calibrates ONE quantity (benign GNSS error vs a literature Rayleigh model),
this module measures the two things that decide whether a V2X misbehaviour dataset resembles a real
city: how the traffic MOVES and how the radio REACHES. Same shape as calibration.py -- numpy only,
read-only over a dataset directory, deterministic (no timestamps, no RNG), JSON out, embedded into
`datasheet.py` and usable as a CI gate from `corpus_report.py`.

    python -m scms_sim_ref.datagen.realism_bench <dataset_dir> [--refdata <dir>] [--json out.json]

TRAFFIC panel (from ``ground_truth/gt_emissions_sample.jsonl``, ideally at ``emit_sample_prob=1.0``):
per-vehicle finite-difference speeds and accelerations, time-headway distributions per spatial cell
(an edge proxy -- the dataset schema carries no edge id), an Edie-generalised fundamental diagram
over space-time cells, and two hard sim-health counters (teleports, vehicle overlaps).

The emission schema carries a true POSITION but no true speed, so speed and acceleration are finite
differences -- and a finite difference of position measures whatever moved the position, including
things that are not driving. SUMO changes lane by re-assigning the vehicle from one lane centreline
to the next in a single step (``--lanechange.duration`` defaults to 0), which puts a whole lane width
of sideways displacement inside one sample; differenced twice at a 0.1 s CAM interval that is a
276 m/s^2 "acceleration". ``track_kinematics`` therefore splits every step into a LONGITUDINAL and a
LATERAL component against a median-smoothed direction of travel, scores acceleration on the
longitudinal component only, and publishes the lateral jumps as their own metric
(``traffic.lateral_discontinuity_events``) instead of letting them masquerade as dynamics.

COMM panel (from ``ma/ma_reports.jsonl`` + ``ground_truth/gt_report_labels.jsonl`` + emissions):
a PDR-vs-distance curve reconstructed from HONEST (false-positive) report links, the neighbour
awareness ratio at 100/200/300 m, the effective range, the gray-zone width, and the CAM
inter-packet gap.

Both panels work on datasets from EITHER producer -- the pure-Python `mock_pipeline` or the
MOSAIC/SUMO layer -- by probing ``manifest.json``. Where a signal is absent (sampled emissions,
unknown regime, too few points) the metric degrades to ``status="na"`` with a machine-readable
``reason`` instead of guessing.

Every metric compares to a reference entry loaded from ``refdata/*.json``; each reference number
carries a ``source`` citation string, and the scorecard copies that citation next to the measured
value so a published datasheet is self-auditing.

TRUST FIREWALL: this module READS ground truth (like calibration.py) and emits only AGGREGATE
realism statistics. It never writes into ``ml/`` and never emits a per-entity value, so no
ORACLE-visibility field can reach a feature table through it.
"""

from __future__ import annotations

import argparse
import json
import math
import os

import numpy as np

REFDATA_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "refdata")

# --- analysis tunables (module level so they are documented, importable and testable) ------------
MAX_FD_DT_S = 2.0          # finite differences are only trusted across gaps up to this long
MIN_SAMPLES = 30           # a metric needs at least this many observations to be scored
T_BUCKET_S = 1.0           # co-presence snapshot width (vehicles inside one bucket are "at once")
DIST_BIN_M = 50.0          # PDR-vs-distance bin width
MIN_BIN_OPPORTUNITIES = 30    # a distance bin needs this many co-presence pairs to be trusted
MAX_LINK_DIST_M = 1000.0   # farthest reception distance the comm curve is reconstructed over
AWARENESS_ANCHORS_M = (100.0, 200.0, 300.0)   # v2x_awareness.awareness_anchor_distances_m
LANE_WIDTH_M = 3.2         # SUMO's default lane width: the lateral scale that separates two lanes
# --- longitudinal / lateral decomposition (traffic.accel_* and traffic.lateral_discontinuity_*) ---
HEADING_WINDOW_PAIRS = 3   # the direction of travel is a circular MEDIAN over +-3 sample pairs
HEADING_MIN_DISP_M = 0.05  # a shorter step carries no heading information (a stopped vehicle)
HEADING_STABLE_TOL_DEG = 45.0  # headings either side of a step must agree for it to be a "jump"
LATERAL_JUMP_M = LANE_WIDTH_M / 2.0     # half a lane, moved sideways inside ONE sample (REPORTED)
LATERAL_SCREEN_M = 0.25    # ... and the smaller partial lane offsets the accel series is screened of
LATERAL_SPEED_MAX_MPS = 2.0             # no vehicle crosses a lane faster than this (see refdata)
REVERSAL_M = 0.5           # a forward-moving vehicle whose next sample is this far BEHIND it
ACCEL_DT_BINS_S = (0.0, 0.15, 0.35, 0.75, 1.5, MAX_FD_DT_S)   # accel stratification by sample gap
HEADWAY_LATERAL_TOL_M = LANE_WIDTH_M / 2.0    # a leader shares the follower's LANE, not just its road
HEADWAY_HEADING_TOL_DEG = 22.5                # ... and drives the same way (half of the old octant)
HEADWAY_MAX_S = 60.0       # headways longer than this are "no leader", not a following headway
HEADWAY_MIN_SPEED_MPS = 1.0
HEADWAY_MAX_INSTANTS = 240    # cap on instants scanned for leaders (deterministic even spacing)
FD_CELL_M = 100.0          # fundamental-diagram space cell
FD_WINDOW_S = 60.0         # fundamental-diagram time window
FD_MIN_CELL_SAMPLES = 5    # per-cell segment count below which the cell is dropped
OVERLAP_DIST_M = 1.0       # two distinct vehicles closer than this at one instant physically overlap
LIVENESS_MIN_SPEED_MPS = 0.5  # a track whose whole span averages below this never actually moved
LIVENESS_MIN_VEHICLES = 5     # ... and the moving fraction needs this many tracks to mean anything
MAX_TIME_BUCKETS = 240     # cap on co-presence snapshots examined (deterministic even spacing)
MAX_VEH_PER_BUCKET = 400   # cap on vehicles per snapshot (deterministic: lowest vehicle ids first)
MOSAIC_ART_MAX_M = 1000.0  # ScmsBeaconApp ART_MAX_M default (env SCMS_ART_MAX_M), MOSAIC layer
FULL_TRACE_MIN_PROB = 0.999   # emit_sample_prob at/above this counts as a full trace

# Metric severity. "hard" failures gate CI (corpus_report --realism); "soft" ones are tracked.
HARD = "hard"
SOFT = "soft"


# ================================================================================================
# reference data
# ================================================================================================
def load_refdata(refdata_dir: str | None = None) -> dict:
    """Load every ``refdata/*.json`` reference set into one flat, citation-carrying lookup.

    Returns ``{"dir":…, "sets": {set_id: meta}, "entries": {"<set>.<key>": entry}}`` where each entry
    keeps its ``source``/``cite`` strings plus whichever of ``range``/``min``/``max``/``value``/
    ``points`` the reference pins. Missing directory -> empty tables (the harness then reports every
    metric as ``na`` with reason "no reference entry", rather than failing).
    """
    d = os.fspath(refdata_dir or REFDATA_DIR)
    out: dict = {"dir": d, "sets": {}, "entries": {}}
    if not os.path.isdir(d):
        return out
    for name in sorted(os.listdir(d)):
        if not name.endswith(".json"):
            continue
        with open(os.path.join(d, name), encoding="utf-8") as fh:
            doc = json.load(fh)
        set_id = str(doc.get("reference_set") or os.path.splitext(name)[0])
        out["sets"][set_id] = {k: v for k, v in doc.items() if k != "entries"}
        out["sets"][set_id]["file"] = name
        for key, entry in (doc.get("entries") or {}).items():
            ref = dict(entry)
            ref["ref_id"] = f"{set_id}.{key}"
            out["entries"][ref["ref_id"]] = ref
    return out


def _ref(refdata: dict, ref_id: str) -> dict | None:
    return (refdata.get("entries") or {}).get(ref_id)


# ================================================================================================
# small numeric helpers (shared with tools/sumo_realism.py)
# ================================================================================================
def ks_statistic(sample, cdf) -> float:
    """1-sample Kolmogorov-Smirnov statistic of `sample` against a reference CDF.

    `cdf` is either a callable evaluated on the sorted sample, or a pre-evaluated array aligned with
    it. This is the generic form of ``calibration._ks_vs_rayleigh`` and reproduces it exactly (see
    tests/test_realism_bench.py, which pins agreement to 1e-9).
    """
    x = np.sort(np.asarray(sample, dtype=float))
    n = int(x.size)
    if n == 0:
        return float("nan")
    f = np.asarray(cdf(x) if callable(cdf) else cdf, dtype=float)
    hi = np.arange(1, n + 1) / n
    lo = np.arange(0, n) / n
    return float(max(np.max(np.abs(hi - f)), np.max(np.abs(lo - f))))


def geh(modelled: float, counted: float) -> float:
    """GEH statistic: sqrt(2*(m-c)^2/(m+c)). Both flows in the same unit (veh/h)."""
    m, c = float(modelled), float(counted)
    s = m + c
    if s <= 0.0:
        return 0.0
    return math.sqrt(2.0 * (m - c) ** 2 / s)


def geh_summary(pairs, geh_max: float = 5.0) -> dict:
    """GEH over an iterable of ``(station_id, modelled, counted)`` -> per-station + aggregate stats.

    Deterministic: stations are sorted by id. Stations where both flows are zero are counted as
    passing (GEH 0) but flagged in ``n_both_zero`` so an empty comparison cannot look like a win.
    """
    rows = []
    for sid, m, c in sorted(((str(s), float(m), float(c)) for s, m, c in pairs), key=lambda r: r[0]):
        rows.append({"station": sid, "modelled": round(m, 3), "counted": round(c, 3),
                     "geh": round(geh(m, c), 4)})
    n = len(rows)
    tot_m = float(sum(r["modelled"] for r in rows))
    tot_c = float(sum(r["counted"] for r in rows))
    n_pass = sum(1 for r in rows if r["geh"] < geh_max)
    return {
        "n_stations": n,
        "n_both_zero": sum(1 for r in rows if r["modelled"] == 0.0 and r["counted"] == 0.0),
        "pass_fraction": round(n_pass / n, 4) if n else None,
        "geh_max_threshold": geh_max,
        "geh_median": round(float(np.median([r["geh"] for r in rows])), 4) if n else None,
        "geh_p85": round(float(np.percentile([r["geh"] for r in rows], 85)), 4) if n else None,
        "total_modelled": round(tot_m, 3),
        "total_counted": round(tot_c, 3),
        "total_geh": round(geh(tot_m, tot_c), 4),
        "total_rel_error": (round((tot_m - tot_c) / tot_c, 5) if tot_c else None),
        "stations": rows,
    }


def _r(x, nd: int = 4):
    """Round to a JSON-safe python float (numpy scalars included); pass None through."""
    if x is None:
        return None
    v = float(x)
    if not math.isfinite(v):
        return None
    return round(v, nd)


def _val(x, nd: int = 4):
    """JSON-safe scalar: counts stay ints, measurements become rounded floats."""
    if x is None or isinstance(x, (bool, str)):
        return x
    if isinstance(x, (int, np.integer)):
        return int(x)
    if isinstance(x, (float, np.floating)):
        return _r(x, nd)
    return x


def _pct(a: np.ndarray, q: float):
    return float(np.percentile(a, q)) if a.size else None


# ================================================================================================
# dataset probing / IO
# ================================================================================================
def _jsonl(path: str) -> list[dict]:
    if not os.path.exists(path):
        return []
    with open(path, encoding="utf-8") as fh:
        return [json.loads(x) for x in fh if x.strip()]


_URBAN_NETWORKS = {"grid", "ring", "spider", "custom", "osm"}
_HIGHWAY_NETWORKS = {"linear"}


def probe_dataset(dataset_dir: str) -> dict:
    """Identify the producing engine and the analysis constants a scorecard needs.

    Handles both dataset producers described in docs/realism/investigation/mosaic-sumo.json: the pure
    Python `mock_pipeline` (full PipelineConfig in the manifest) and the MOSAIC/SUMO Java backend
    (a much smaller config dict). Unknown producers degrade to ``engine="unknown"``, which only
    disables the engine-specific reconstructions, never the whole harness.
    """
    dataset_dir = os.fspath(dataset_dir)
    man: dict = {}
    mp = os.path.join(dataset_dir, "manifest.json")
    if os.path.exists(mp):
        try:
            with open(mp, encoding="utf-8") as fh:
                man = json.load(fh)
        except (json.JSONDecodeError, OSError):
            man = {}
    cfg = man.get("config") if isinstance(man.get("config"), dict) else {}
    gen = str(man.get("generator", ""))
    # order matters: the Python engine's own generator string says "pre-MOSAIC reference", so the
    # mock_pipeline marker has to win before the MOSAIC-layer marker is considered.
    if "mock_pipeline" in gen:
        engine = "python_mock"
    elif "MOSAIC" in gen:
        engine = "mosaic"
    elif "emit_sample_prob" in cfg:
        engine = "python_mock"
    else:
        engine = "unknown"

    # emission sampling: full traces are required for headway / fundamental-diagram / gap metrics
    if engine == "python_mock":
        emit_p = float(cfg.get("emit_sample_prob", 1.0) or 0.0)
    elif engine == "mosaic":
        emit_p = float(cfg.get("emit_sample_prob", 0.02) or 0.0)   # SCMS_EMIT_SAMPLE default
    else:
        emit_p = float(cfg.get("emit_sample_prob", 0.0) or 0.0)

    road = str(cfg.get("road_network") or "") or None
    regime = ("urban" if road in _URBAN_NETWORKS else
              "highway" if road in _HIGHWAY_NETWORKS else None)
    # A producer that classified its own network may say so directly (the MOSAIC generator derives
    # the regime from the SUMO net's speed limits, which is strictly better evidence than a topology
    # token). An explicit, VALID regime wins; anything else is ignored rather than trusted.
    explicit = str(cfg.get("regime") or "").strip().lower()
    if explicit in ("urban", "highway"):
        regime = explicit

    # acceptanceRangeThreshold normalisation differs per engine (see the two detector implementations):
    #   python_mock : detnorm = max(0, d - radio_range_m) / art_max_m   -> censored below the range
    #   mosaic      : detnorm = d / ART_MAX_M                           -> uncensored
    if engine == "python_mock":
        art_max_m = float(cfg.get("art_max_m", 150.0) or 150.0)
        radio_range_m = float(cfg.get("radio_range_m", 500.0) or 500.0)
        art_censored = True
    elif engine == "mosaic":
        # ScmsBackend writes the RESOLVED SCMS_ART_MAX_M into config (ScmsBackend.java:869), so an
        # overridden normaliser must be honoured -- hardcoding the default rescales every
        # reconstructed link distance and silently shifts the whole PDR-vs-distance curve.
        art_max_m = float(cfg.get("art_max_m", MOSAIC_ART_MAX_M) or MOSAIC_ART_MAX_M)
        radio_range_m = float(cfg.get("radio_range_m", 0.0) or 0.0)
        art_censored = False
    else:
        art_max_m, radio_range_m, art_censored = 0.0, 0.0, False

    return {
        "engine": engine,
        "generator": gen or None,
        "dataset_version": man.get("dataset_version"),
        "seed": man.get("seed"),
        "emit_sample_prob": emit_p,
        "full_trace": bool(emit_p >= FULL_TRACE_MIN_PROB),
        "dt_s": float(cfg.get("dt", 0.0) or 0.0) or None,
        "n_lanes": int(cfg.get("n_lanes", 1) or 1),
        "road_network": road,
        "regime": regime,
        "art_max_m": art_max_m,
        "radio_range_m": radio_range_m,
        "art_censored": art_censored,
        "has_emissions": os.path.exists(
            os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")),
        "has_reports": os.path.exists(os.path.join(dataset_dir, "ma", "ma_reports.jsonl")),
        "has_report_labels": os.path.exists(
            os.path.join(dataset_dir, "ground_truth", "gt_report_labels.jsonl")),
    }


# ================================================================================================
# trajectory reconstruction
# ================================================================================================
def build_tracks(emissions: list[dict]) -> dict[str, dict]:
    """Per-vehicle TRUE trajectories, sorted by time, de-duplicated on (vehicle, t).

    Uses ``true_x``/``true_y`` -- the simulator's own state -- so falsified CAMs never distort a
    mobility metric (an attacker's car still drives like a car). Returns ``{vid: {t, x, y}}`` with
    numpy arrays.
    """
    per: dict[str, dict[float, tuple[float, float]]] = {}
    for e in emissions:
        vid = e.get("true_vehicle_id")
        if vid is None:
            continue
        try:
            t = float(e["t"]); x = float(e["true_x"]); y = float(e["true_y"])
        except (KeyError, TypeError, ValueError):
            continue
        per.setdefault(str(vid), {})[round(t, 6)] = (x, y)
    tracks: dict[str, dict] = {}
    for vid in sorted(per):
        ts = sorted(per[vid])
        if len(ts) < 2:
            continue
        tracks[vid] = {
            "t": np.array(ts, dtype=float),
            "x": np.array([per[vid][t][0] for t in ts], dtype=float),
            "y": np.array([per[vid][t][1] for t in ts], dtype=float),
        }
    return tracks


def _ang_diff(a, b):
    """Signed shortest angular difference a - b, in (-pi, pi]."""
    return np.angle(np.exp(1j * (np.asarray(a, dtype=float) - np.asarray(b, dtype=float))))


def _circular_median_rows(win: np.ndarray) -> np.ndarray:
    """Circular median of each ROW of `win` (angles in rad), taken about that row's circular mean.

    Rotating to the circular mean before taking an ordinary median is what makes this wrap-safe; the
    mean is only a reference frame, so a single outlier that drags it by a few degrees cannot drag
    the median with it (the outlier is still the extreme order statistic after rotation).
    """
    r = np.arctan2(np.sin(win).sum(axis=1), np.cos(win).sum(axis=1))
    return r + np.median(_ang_diff(win, r[:, None]), axis=1)


def smoothed_heading(ang: np.ndarray, usable: np.ndarray,
                     window_pairs: int = HEADING_WINDOW_PAIRS) -> np.ndarray:
    """Per-step DIRECTION OF TRAVEL: a circular median over +-`window_pairs` neighbouring steps.

    A single consecutive-sample pair is not a heading: when SUMO changes lane it moves the vehicle a
    whole lane width sideways in one step, and that step's own ``atan2(dy, dx)`` points across the
    road rather than along it. The median is the estimator that fixes this without breaking turns:

      * ONE anomalous step among ``2*window_pairs+1`` cannot move a median at all, so a lane-change
        teleport is rejected outright, whereas a mean would still be dragged a few degrees (and a few
        degrees of heading error times a 5 m step is a metre of fabricated longitudinal displacement);
      * on a genuine turn the headings are MONOTONE, and the median of a monotone window is its
        middle element -- i.e. the step's own heading. Smoothing therefore costs nothing on a curve.

    Steps shorter than ``HEADING_MIN_DISP_M`` (a stopped vehicle) carry no direction and are left out
    of the window; they inherit the smoothed heading of the nearest step that does. The window is
    counted in STEPS, not seconds, so it adapts to whatever rate the CAM trigger happened to emit at.
    """
    ang = np.asarray(ang, dtype=float)
    n = int(ang.size)
    if n == 0:
        return ang.copy()
    idx = np.flatnonzero(np.asarray(usable, dtype=bool))
    if idx.size == 0:
        return ang.copy()
    a = ang[idx]
    m, k = int(a.size), max(1, int(window_pairs))
    if m == 1:
        h = a.copy()
    else:
        pad = min(k, m - 1)
        ap = np.pad(a, pad, mode="reflect")          # neighbours, not a repeat of the step itself
        if pad < k:                                   # a very short track: top the window up
            ap = np.pad(ap, k - pad, mode="edge")
        h = _circular_median_rows(np.lib.stride_tricks.sliding_window_view(ap, 2 * k + 1))
    if idx.size == n:
        out = h
    else:                                             # short steps take their nearest neighbour's
        j = np.searchsorted(idx, np.arange(n))
        hi = np.clip(j, 0, m - 1)
        lo = np.clip(j - 1, 0, m - 1)
        pos = np.arange(n)
        out = h[np.where(np.abs(idx[hi] - pos) <= np.abs(pos - idx[lo]), hi, lo)]
    return _ang_diff(out, 0.0)


def track_kinematics(t: np.ndarray, x: np.ndarray, y: np.ndarray, *,
                     max_dt: float = MAX_FD_DT_S,
                     speed_bound_mps: float | None = None) -> dict:
    """Decompose ONE vehicle's samples into longitudinal / lateral motion and differentiate.

    Per consecutive-sample step the displacement is split against the smoothed direction of travel
    into a LONGITUDINAL component (along the vehicle's own heading) and a LATERAL one (across it).
    Speed and acceleration are then read off the longitudinal component only, which is what stops a
    lane change from being read as a 33 m/s burst of forward motion and a 276 m/s^2 acceleration.

    The longitudinal displacement is the full along-path chord EXCEPT on a step flagged as a lateral
    jump, where it is the projection on the smoothed heading. Projecting unconditionally would be
    wrong: a vehicle that rounds a 90-degree corner inside a single 1 s sample really did travel the
    whole chord, and projecting it onto a heading the median cannot turn that fast FABRICATES a
    deceleration (tests/test_realism_bench.py::test_a_single_step_corner_is_not_a_lane_change_and_
    does_not_fabricate_acceleration). The projection is applied where the lateral component is real
    and nowhere else.

    Three step classes are marked so the acceleration series can exclude them (each is reported by
    its own metric, so nothing is dropped silently):

      * ``lateral_screen`` -- at least ``LATERAL_SCREEN_M`` sideways at more than
        ``LATERAL_SPEED_MAX_MPS``, with the headings either side agreeing (so a corner is not
        mistaken for a jump). ``lateral`` is its lane-width-scale subset (>= ``LATERAL_JUMP_M``),
        which is what ``traffic.lateral_discontinuity_events`` reports: a half-metre partial lane
        offset corrupts a 0.1 s difference just as badly, but it is not a lane change;
      * ``reversal`` -- the vehicle's next sample is more than ``REVERSAL_M`` BEHIND it, SUMO's
        longitudinal twin of the same lane/edge position remapping;
      * ``teleport`` -- a mean speed above `speed_bound_mps`, already counted by
        ``traffic.teleport_events``; leaving it in would report one event as two failures.

    ``boundary`` marks each track's first and last step: a vehicle is inserted part-way through a
    step and its final sample is clamped to the end of its route, so neither displacement is a
    second's worth of driving (in the Python engine that clamp alone produced accel_min = -15.6).
    """
    t = np.asarray(t, dtype=float); x = np.asarray(x, dtype=float); y = np.asarray(y, dtype=float)
    dt = np.diff(t); dx = np.diff(x); dy = np.diff(y)
    n = int(dt.size)
    z = np.zeros(n, dtype=bool)
    dist = np.hypot(dx, dy)
    ang = np.arctan2(dy, dx)
    if n == 0:
        empty = np.zeros(0, dtype=float)
        return {"n": 0, "dt": empty, "dist": empty, "heading": empty, "heading_smooth": empty,
                "d_long": empty, "d_lat": empty, "speed_long": empty, "lateral_speed": empty,
                "lateral": z, "lateral_screen": z, "reversal": z, "teleport": z, "boundary": z,
                "accel": empty, "accel_pair_dt": empty, "accel_all": empty,
                "accel_all_pair_dt": empty,
                "n_excl": {"boundary": 0, "lateral": 0, "reversal": 0, "teleport": 0}}
    hs = smoothed_heading(ang, dist >= HEADING_MIN_DISP_M)
    ux, uy = np.cos(hs), np.sin(hs)
    proj = dx * ux + dy * uy
    d_lat = dy * ux - dx * uy
    pos_dt = dt > 0.0
    with np.errstate(divide="ignore", invalid="ignore"):
        lat_speed = np.where(pos_dt, np.abs(d_lat) / np.where(pos_dt, dt, 1.0), np.inf)
        mean_speed = np.where(pos_dt, dist / np.where(pos_dt, dt, 1.0), np.inf)

    # A lane change leaves the direction of travel UNCHANGED -- the vehicle is on the same road one
    # lane over -- so a step is only a candidate jump when the steps either side of it point the same
    # way. Cornering fails that test, which is what stops a 90-degree turn taken inside a single 1 s
    # sample (the Python engine's grid junctions) from being counted as a lane-change teleport. The
    # RAW neighbouring headings are used, not the smoothed ones: near the end of a track the median
    # cannot turn fast enough and would call a corner "stable". The first and last step of a track
    # have no evidence on one side, so they are not scanned at all.
    tol = math.radians(HEADING_STABLE_TOL_DEG)
    ang_eff = np.where(dist >= HEADING_MIN_DISP_M, ang, hs)
    # The witnesses are the nearest steps either side that are not themselves suspect: a lane change
    # immediately followed by a second one would otherwise vouch for its own neighbour's rogue
    # heading and both would go uncounted (veh_115 in the InTAS run does exactly this, 3.2 m one way
    # then 6.4 m back inside 0.4 s). Witnesses are looked for at most HEADING_WINDOW_PAIRS steps out.
    suspect = (lat_speed > LATERAL_SPEED_MAX_MPS) & (np.abs(d_lat) >= LATERAL_SCREEN_M)
    ix = np.arange(n)
    prev_ok = np.concatenate(([-1], np.maximum.accumulate(np.where(~suspect, ix, -1))[:-1]))
    nxt = np.minimum.accumulate(np.where(~suspect, ix, n)[::-1])[::-1]
    next_ok = np.concatenate((nxt[1:], [n]))
    has_witness = ((prev_ok >= 0) & (next_ok < n)
                   & ((ix - prev_ok) <= HEADING_WINDOW_PAIRS)
                   & ((next_ok - ix) <= HEADING_WINDOW_PAIRS))
    stable = np.zeros(n, dtype=bool)
    if has_witness.any():
        w = np.flatnonzero(has_witness)
        stable[w] = np.abs(_ang_diff(ang_eff[next_ok[w]], ang_eff[prev_ok[w]])) <= tol

    jumped = (lat_speed > LATERAL_SPEED_MAX_MPS) & stable
    lateral_screen = jumped & (np.abs(d_lat) >= LATERAL_SCREEN_M)
    lateral = jumped & (np.abs(d_lat) >= LATERAL_JUMP_M)     # the lane-width-scale REPORTED subset
    d_long = np.where(lateral_screen, proj, np.where(proj >= 0.0, dist, -dist))
    reversal = d_long < -REVERSAL_M
    teleport = ((mean_speed > float(speed_bound_mps))
                if (speed_bound_mps is not None and float(speed_bound_mps) > 0.0) else z.copy())
    boundary = z.copy()
    boundary[0] = True
    boundary[-1] = True
    with np.errstate(divide="ignore", invalid="ignore"):
        speed_long = np.where(pos_dt, d_long / np.where(pos_dt, dt, 1.0), np.nan)

    out = {"n": n, "dt": dt, "dist": dist, "heading": ang, "heading_smooth": hs,
           "d_long": d_long, "d_lat": d_lat, "speed_long": speed_long, "lateral_speed": lat_speed,
           "lateral": lateral, "lateral_screen": lateral_screen, "reversal": reversal,
           "teleport": teleport, "boundary": boundary}
    if n < 2:
        empty = np.zeros(0, dtype=float)
        out.update({"accel": empty, "accel_pair_dt": empty, "accel_all": empty,
                    "accel_all_pair_dt": empty,
                    "n_excl": {"boundary": 0, "lateral": 0, "reversal": 0, "teleport": 0}})
        return out
    # the midpoint separation IS (dt_i + dt_{i+1})/2, which is the right denominator for an
    # irregularly sampled second difference: both speeds are MEAN speeds centred on their own step.
    pair_dt = (dt[:-1] + dt[1:]) * 0.5
    usable = (dt[:-1] > 0) & (dt[:-1] <= max_dt) & (dt[1:] > 0) & (dt[1:] <= max_dt)
    disc = lateral_screen | reversal | teleport
    bad = disc[:-1] | disc[1:]
    edge = boundary[:-1] | boundary[1:]
    clean = usable & ~bad & ~edge
    with np.errstate(divide="ignore", invalid="ignore"):
        acc = (speed_long[1:] - speed_long[:-1]) / pair_dt
    out.update({
        "accel": acc[clean], "accel_pair_dt": pair_dt[clean],
        "accel_all": acc[usable], "accel_all_pair_dt": pair_dt[usable],
        "n_excl": {"boundary": int((usable & edge).sum()),
                   "lateral": int((usable & ~edge & (lateral_screen[:-1] | lateral_screen[1:])).sum()),
                   "reversal": int((usable & ~edge & (reversal[:-1] | reversal[1:])).sum()),
                   "teleport": int((usable & ~edge & (teleport[:-1] | teleport[1:])).sum())},
    })
    return out


_KIN_PAIR_COLS = ("dt", "dist", "heading", "heading_smooth", "d_long", "d_lat", "speed_long",
                  "lateral_speed", "lateral", "lateral_screen", "reversal", "teleport", "boundary")
_KIN_BOOL_COLS = ("lateral", "lateral_screen", "reversal", "teleport", "boundary")


def kinematics(tracks: dict[str, dict], max_dt: float = MAX_FD_DT_S,
               speed_bound_mps: float | None = None) -> dict:
    """``track_kinematics`` over every vehicle, concatenated in sorted-vehicle order.

    Adds the per-vehicle bookkeeping ``segment_table`` and the traffic panel need: which pairs are
    inside the finite-difference gap ceiling (``usable``), the owning vehicle index, and the totals
    the lateral-discontinuity rate is normalised by (path length -> vehicle-km).
    """
    vids = sorted(tracks)
    cols: dict[str, list] = {c: [] for c in _KIN_PAIR_COLS}
    extra: dict[str, list] = {"vid_i": [], "t_end": [], "t_mid": [], "x_mid": [], "y_mid": [],
                              "x_end": [], "y_end": []}
    acc, acc_dt, acc_all, acc_all_dt = [], [], [], []
    n_excl = {"boundary": 0, "lateral": 0, "reversal": 0, "teleport": 0}
    for i, vid in enumerate(vids):
        tr = tracks[vid]
        t, x, y = tr["t"], tr["x"], tr["y"]
        k = track_kinematics(t, x, y, max_dt=max_dt, speed_bound_mps=speed_bound_mps)
        if not k["n"]:
            continue
        for c in _KIN_PAIR_COLS:
            cols[c].append(k[c])
        extra["vid_i"].append(np.full(k["n"], i, dtype=np.int64))
        extra["t_end"].append(t[1:]); extra["t_mid"].append((t[:-1] + t[1:]) * 0.5)
        extra["x_mid"].append((x[:-1] + x[1:]) * 0.5)
        extra["y_mid"].append((y[:-1] + y[1:]) * 0.5)
        extra["x_end"].append(x[1:]); extra["y_end"].append(y[1:])
        acc.append(k["accel"]); acc_dt.append(k["accel_pair_dt"])
        acc_all.append(k["accel_all"]); acc_all_dt.append(k["accel_all_pair_dt"])
        for key in n_excl:
            n_excl[key] += int(k["n_excl"][key])
    if not extra["vid_i"]:
        e = np.zeros(0, dtype=float)
        out = {c: (np.zeros(0, dtype=bool) if c in _KIN_BOOL_COLS else e) for c in _KIN_PAIR_COLS}
        out.update({k: e for k in extra})
        out.update({"n": 0, "vehicles": vids, "usable": np.zeros(0, dtype=bool),
                    "accel": e, "accel_pair_dt": e, "accel_all": e, "accel_all_pair_dt": e,
                    "n_excl": n_excl, "path_m": 0.0})
        return out
    cat = np.concatenate
    out = {c: cat(cols[c]) for c in _KIN_PAIR_COLS}
    out.update({k: cat(v) for k, v in extra.items()})
    out["n"] = int(out["dt"].size)
    out["vehicles"] = vids
    out["usable"] = (out["dt"] > 0.0) & (out["dt"] <= max_dt)
    # vehicle-km is distance driven ALONG the road, so a lane-change teleport's sideways component
    # must not pad the denominator of the rate it is being counted against.
    out["path_m"] = float(np.abs(out["d_long"]).sum())
    out["accel"] = cat(acc) if acc else np.zeros(0)
    out["accel_pair_dt"] = cat(acc_dt) if acc_dt else np.zeros(0)
    out["accel_all"] = cat(acc_all) if acc_all else np.zeros(0)
    out["accel_all_pair_dt"] = cat(acc_all_dt) if acc_all_dt else np.zeros(0)
    out["n_excl"] = n_excl
    return out


def segment_table(tracks: dict[str, dict], max_dt: float = MAX_FD_DT_S,
                  kin: dict | None = None) -> dict:
    """Finite-difference segments between consecutive samples of the same vehicle.

    Returns column arrays over every usable segment (0 < dt <= max_dt): the segment mid-point in
    space and time, the traversed distance, the elapsed time, the mean speed, the heading, and the
    owning vehicle. ``n_dropped_gap`` counts segments skipped because the sampling gap was too long
    to finite-difference honestly (the dominant effect when ``emit_sample_prob`` < 1).

    ``dist``/``speed``/``heading`` stay the RAW chord quantities (Edie's generalised definitions want
    distance actually travelled, and the leader search wants the geometric step direction);
    ``speed_long``/``heading_smooth``/``d_lat`` carry the decomposition from ``track_kinematics``.
    """
    k = kin if kin is not None else kinematics(tracks, max_dt)
    vids = k.get("vehicles", sorted(tracks))
    if not k.get("n"):
        return {"n": 0, "n_dropped_gap": 0, "vehicles": vids}
    ok = k["usable"]
    dropped = int((~ok).sum())
    if not ok.any():
        return {"n": 0, "n_dropped_gap": dropped, "vehicles": vids}
    d, dt = k["dist"][ok], k["dt"][ok]
    return {
        "n": int(d.size), "n_dropped_gap": dropped, "vehicles": vids,
        "vid_i": k["vid_i"][ok], "t_end": k["t_end"][ok], "t_mid": k["t_mid"][ok],
        "x_mid": k["x_mid"][ok], "y_mid": k["y_mid"][ok],
        "x_end": k["x_end"][ok], "y_end": k["y_end"][ok],
        "dist": d, "dt": dt, "speed": d / dt, "heading": k["heading"][ok],
        "heading_smooth": k["heading_smooth"][ok], "d_long": k["d_long"][ok],
        "d_lat": k["d_lat"][ok], "speed_long": np.abs(k["speed_long"][ok]),
        "lateral": k["lateral"][ok],
    }


def accelerations(tracks: dict[str, dict], max_dt: float = MAX_FD_DT_S) -> np.ndarray:
    """LONGITUDINAL accelerations over consecutive steps of one vehicle.

    Second finite difference of the longitudinal speed (see ``track_kinematics``), screened of the
    step classes that are position DISCONTINUITIES rather than dynamics -- lane-change teleports,
    backward position remaps, teleports and the two partial steps at each end of a track. Every
    screened class is counted and published by its own scorecard metric, and the unscreened series
    is available as ``kinematics(tracks)["accel_all"]``.
    """
    return kinematics(tracks, max_dt)["accel"]


def teleport_events(tracks: dict[str, dict], speed_bound_mps: float) -> tuple[int, int]:
    """Consecutive-sample displacements implying a mean speed above `speed_bound_mps`.

    Scans EVERY consecutive pair of a vehicle's samples, including the long sampling gaps that
    ``segment_table`` drops: a 500 km jump across a 50 s gap is a teleport whatever the gap length,
    and restricting the scan to gaps <= MAX_FD_DT_S is exactly how a dataset that teleports on every
    step reads as clean once emissions are sub-sampled. Mean speed over the gap is the honest test --
    no vehicle averages more than the bound, however long you wait between samples.

    Returns ``(events, pairs_examined)``; the pair count is the sample size the metric gates on.
    """
    n_tele = n_pair = 0
    for vid in sorted(tracks):
        tr = tracks[vid]
        dt = np.diff(tr["t"])
        d = np.hypot(np.diff(tr["x"]), np.diff(tr["y"]))
        ok = dt > 0.0
        if not ok.any():
            continue
        n_pair += int(ok.sum())
        n_tele += int((d[ok] / dt[ok] > float(speed_bound_mps)).sum())
    return n_tele, n_pair


def moving_vehicle_fraction(tracks: dict[str, dict],
                            min_speed_mps: float = LIVENESS_MIN_SPEED_MPS) -> tuple[float | None, int]:
    """LIVENESS: fraction of tracked vehicles whose whole track averages above `min_speed_mps`.

    Path length over track span, not per-segment speed, so a vehicle queued at one signal for part
    of its trip still counts as moving. Returns ``(fraction, n_tracks)``.
    """
    n_moving = n = 0
    for vid in sorted(tracks):
        tr = tracks[vid]
        span = float(tr["t"][-1] - tr["t"][0])
        if span <= 0.0:
            continue
        n += 1
        path = float(np.hypot(np.diff(tr["x"]), np.diff(tr["y"])).sum())
        if path / span >= float(min_speed_mps):
            n_moving += 1
    return (n_moving / n if n else None), n


def _instant_groups(segs: dict, max_instants: int = HEADWAY_MAX_INSTANTS,
                    max_veh: int = MAX_VEH_PER_BUCKET) -> list[np.ndarray]:
    """Segment indices grouped by end-of-segment timestamp: one group = one instant.

    Deterministic sub-sampling, matching ``_snapshots``: instants are taken evenly spaced across the
    run and, inside an instant, in vehicle-id order (``vid_i`` indexes the sorted vehicle list).
    """
    if not segs.get("n"):
        return []
    t = np.round(segs["t_end"], 3)
    order = np.lexsort((segs["vid_i"], t))
    bounds = np.flatnonzero(np.diff(t[order])) + 1
    groups = [g for g in np.split(order, bounds) if g.size >= 2]
    if len(groups) > max_instants:
        step = len(groups) / float(max_instants)
        groups = [groups[int(i * step)] for i in range(max_instants)]
    return [g[:max_veh] for g in groups]


def _snapshots(emissions: list[dict], bucket_s: float, max_buckets: int = MAX_TIME_BUCKETS,
               max_veh: int = MAX_VEH_PER_BUCKET) -> list[dict]:
    """Co-presence snapshots: one position per vehicle per time bucket (its latest sample there).

    Deterministic sub-sampling: buckets are taken evenly spaced across the run and, inside a bucket,
    vehicles are kept in sorted-id order. Nothing here draws random numbers.
    """
    buckets: dict[int, dict[str, tuple[float, float, float]]] = {}
    for e in emissions:
        vid = e.get("true_vehicle_id")
        if vid is None:
            continue
        try:
            t = float(e["t"]); x = float(e["true_x"]); y = float(e["true_y"])
        except (KeyError, TypeError, ValueError):
            continue
        b = int(math.floor(t / bucket_s))
        cur = buckets.setdefault(b, {}).get(str(vid))
        if cur is None or t >= cur[0]:
            buckets[b][str(vid)] = (t, x, y)
    keys = sorted(buckets)
    if len(keys) > max_buckets:
        step = len(keys) / float(max_buckets)
        keys = [keys[int(i * step)] for i in range(max_buckets)]
    out = []
    for b in keys:
        vids = sorted(buckets[b])[:max_veh]
        if len(vids) < 2:
            continue
        out.append({
            "bucket": b,
            "vids": vids,
            "x": np.array([buckets[b][v][1] for v in vids], dtype=float),
            "y": np.array([buckets[b][v][2] for v in vids], dtype=float),
            "t": np.array([buckets[b][v][0] for v in vids], dtype=float),
        })
    return out


def _pair_distance_hist(snaps: list[dict], edges: np.ndarray) -> np.ndarray:
    """Histogram of pairwise distances between co-present vehicles = reception OPPORTUNITIES."""
    hist = np.zeros(edges.size - 1, dtype=np.int64)
    for s in snaps:
        x, y = s["x"], s["y"]
        n = x.size
        if n < 2:
            continue
        dx = x[:, None] - x[None, :]
        dy = y[:, None] - y[None, :]
        d = np.hypot(dx, dy)
        iu = np.triu_indices(n, k=1)
        hist += np.histogram(d[iu], bins=edges)[0]
    return hist


# ================================================================================================
# metric plumbing
# ================================================================================================
def _status(value, ref: dict | None) -> tuple[str, str | None]:
    """pass / fail / na for a measured value against a normalised reference entry."""
    if ref is None:
        return "na", "no reference entry"
    if value is None:
        return "na", "no data"
    v = float(value)
    if "range" in ref and ref["range"] is not None:
        lo, hi = float(ref["range"][0]), float(ref["range"][1])
        return ("pass" if lo <= v <= hi else "fail"), None
    lo = ref.get("min")
    hi = ref.get("max")
    if lo is None and hi is None:
        return "na", "reference entry carries no numeric threshold (informational)"
    if lo is not None and v < float(lo):
        return "fail", None
    if hi is not None and v > float(hi):
        return "fail", None
    return "pass", None


def _ref_block(ref: dict | None) -> dict | None:
    if ref is None:
        return None
    out = {"ref_id": ref.get("ref_id"), "unit": ref.get("unit"),
           "confidence": ref.get("confidence"), "cite": ref.get("cite"),
           "source": ref.get("source")}
    for k in ("range", "min", "max", "value", "points", "derivation", "note"):
        if k in ref:
            out[k] = ref[k]
    return {k: v for k, v in out.items() if v is not None}


def _metric(mid: str, panel: str, title: str, value, unit: str, n: int | None,
            ref: dict | None = None, severity: str = SOFT, reason: str | None = None,
            extra: dict | None = None, nd: int = 4) -> dict:
    """Build one scorecard row. `reason` forces ``na`` (the signal is absent, not failing).

    `nd` widens the published precision for a metric whose gate sits at the edge of the default 4
    decimals -- a 0.999945 pass fraction against a >= 1.0 gate must not print as "1.0 fail".
    Status is always decided on the UNROUNDED value, so precision never moves a verdict.
    """
    if reason is not None:
        status, why = "na", reason
    else:
        status, why = _status(value, ref)
    row = {
        "id": mid, "panel": panel, "title": title,
        "value": _val(value, nd),
        "unit": unit, "n": (int(n) if n is not None else None),
        "status": status, "severity": severity,
    }
    if why:
        row["reason"] = why
    rb = _ref_block(ref)
    if rb:
        row["reference"] = rb
    if extra:
        row["details"] = extra
    return row


# ================================================================================================
# TRAFFIC panel
# ================================================================================================
def _time_headways(segs: dict) -> np.ndarray:
    """Time headways (spacing / follower speed) to the leader IN THE FOLLOWER'S OWN LANE.

    The dataset schema carries no edge or lane id (see docs/realism/investigation/benchmark-infra
    .json), so the leader is found geometrically, the way SUMO defines one. At each instant every
    other vehicle is projected into the follower's OWN heading frame and the leader is the nearest
    one that is

      * ahead              -- positive longitudinal offset,
      * driving the same way -- heading within ``HEADWAY_HEADING_TOL_DEG``,
      * in the same lane   -- ``|lateral offset| <= HEADWAY_LATERAL_TOL_M`` (half a lane width).

    The lateral test is what stops vehicles that are side by side in ADJACENT LANES from being read
    as a leader/follower pair: projecting on the along-road axis alone makes their spacing ~0 and
    fabricates near-zero headways (which is what drove ``headway_below_floor_frac`` on a dataset
    whose following model is fine). Nothing is grouped into a spatial cell either, so a headway is
    no longer censored by a cell boundary -- with a 50 m cell no headway longer than
    (cell diagonal / speed) could ever be observed and ``HEADWAY_MAX_S`` was unreachable, a
    one-sided truncation that biased the whole distribution short.
    """
    out: list[float] = []
    cos_tol = math.cos(math.radians(HEADWAY_HEADING_TOL_DEG))
    for g in _instant_groups(segs):
        x, y = segs["x_end"][g], segs["y_end"][g]
        v, hdg = segs["speed"][g], segs["heading"][g]
        ux, uy = np.cos(hdg), np.sin(hdg)
        dx = x[None, :] - x[:, None]
        dy = y[None, :] - y[:, None]
        along = dx * ux[:, None] + dy * uy[:, None]        # >0: column j is ahead of row i
        lat = dy * ux[:, None] - dx * uy[:, None]
        same_way = (ux[:, None] * ux[None, :] + uy[:, None] * uy[None, :]) >= cos_tol
        ok = same_way & (along > 0.0) & (np.abs(lat) <= HEADWAY_LATERAL_TOL_M)
        np.fill_diagonal(ok, False)
        gap = np.where(ok, along, np.inf).min(axis=1)
        sel = np.isfinite(gap) & (v >= HEADWAY_MIN_SPEED_MPS)
        if not sel.any():
            continue
        hw = gap[sel] / v[sel]
        out.extend(float(h) for h in hw[(hw > 0.0) & (hw <= HEADWAY_MAX_S)])
    return np.array(out, dtype=float)


def _lane_groups(lat: np.ndarray, tol: float = HEADWAY_LATERAL_TOL_M) -> int:
    """Number of distinct parallel lane lines in a set of lateral offsets.

    Single-linkage on the sorted offsets: a gap wider than `tol` (half a lane) starts a new line.
    Under-counts rather than over-counts when a lane-changing vehicle bridges two lanes, which keeps
    the derived per-lane flow conservative.
    """
    if lat.size == 0:
        return 0
    return 1 + int((np.diff(np.sort(lat)) > float(tol)).sum())


def _fundamental_diagram(segs: dict, n_lanes: int, cell_m: float, window_s: float) -> dict:
    """Edie's generalised flow/density per DIRECTIONAL space-time cell.

    For a cell of side L observed over T seconds, Edie gives q = sum(distance)/(L*T) and
    k = sum(time)/(L*T) with v = sum(distance)/sum(time). A square cell is treated as a road section
    of length L: vehicles cross it along one axis, so this is a per-lane-group approximation, not an
    exact per-edge measurement (documented as such in the metric details).

    Two corrections keep that approximation from inventing capacity out of geometry:

      * the cell key carries the HEADING OCTANT, so the two directions of a road -- and roads that
        cross inside one cell -- are separate sections instead of one summed flow;
      * the per-lane divisor is MEASURED, not assumed. Within a cell the members are projected onto
        the cell's mean heading and their lateral offsets are clustered at half a lane width
        (``_lane_groups``); parallel carriageways or lanes inside the same cell therefore each
        contribute their own lane. The manifest's ``n_lanes`` is used as a FLOOR, so a python-engine
        run that declares its lane count keeps it, while a MOSAIC manifest -- which carries no
        ``n_lanes`` at all and used to default to 1 -- no longer reports every parallel road in the
        cell as extra flow on a single lane.
    """
    if not segs.get("n"):
        return {"cells": 0}
    floor_lanes = max(1, int(n_lanes or 1))
    cx = np.floor(segs["x_mid"] / cell_m).astype(np.int64)
    cy = np.floor(segs["y_mid"] / cell_m).astype(np.int64)
    cw = np.floor(segs["t_mid"] / window_s).astype(np.int64)
    oct_ = np.floor((np.degrees(segs["heading"]) % 360.0) / 45.0).astype(np.int64)
    acc: dict[tuple, list] = {}
    for i in range(int(segs["n"])):
        key = (int(cx[i]), int(cy[i]), int(cw[i]), int(oct_[i]))
        a = acc.setdefault(key, [0.0, 0.0, 0, []])
        a[0] += float(segs["dist"][i])
        a[1] += float(segs["dt"][i])
        a[2] += 1
        a[3].append(i)
    q, k_, v, lanes_seen = [], [], [], []
    for key in sorted(acc):
        dsum, tsum, cnt, idx = acc[key]
        if cnt < FD_MIN_CELL_SAMPLES or tsum <= 0.0:
            continue
        ii = np.asarray(idx, dtype=np.int64)
        hs = segs["heading"][ii]
        ang = math.atan2(float(np.sin(hs).mean()), float(np.cos(hs).mean()))
        lat = segs["y_mid"][ii] * math.cos(ang) - segs["x_mid"][ii] * math.sin(ang)
        lanes = max(floor_lanes, _lane_groups(lat))
        q.append(dsum / (cell_m * window_s) * 3600.0 / lanes)     # veh/h/lane
        k_.append(tsum / (cell_m * window_s) * 1000.0 / lanes)    # veh/km/lane
        v.append(dsum / tsum)                                     # m/s
        lanes_seen.append(lanes)
    return {"cells": len(q), "q": np.array(q), "k": np.array(k_), "v": np.array(v),
            "cell_m": cell_m, "window_s": window_s, "lanes": floor_lanes,
            "lanes_per_cell": np.array(lanes_seen, dtype=float)}


def _overlap_events(emissions: list[dict],
                    max_instants: int = MAX_TIME_BUCKETS) -> tuple[int, int]:
    """Distinct vehicles occupying the same point at the SAME timestamp.

    Only exact timestamp matches are used (a bucketed snapshot would place vehicles up to a bucket
    apart in time and manufacture false overlaps). One position per (vehicle, instant): a vehicle
    emits at most one real CAM per step, and Sybil ghosts are never written to the emission stream.
    Instants are sub-sampled with deterministic even spacing when there are many.
    """
    inst: dict[float, dict[str, tuple[float, float]]] = {}
    for e in emissions:
        try:
            t = round(float(e["t"]), 3)
            inst.setdefault(t, {})[str(e.get("true_vehicle_id"))] = (
                float(e["true_x"]), float(e["true_y"]))
        except (KeyError, TypeError, ValueError):
            continue
    keys = [t for t in sorted(inst) if len(inst[t]) >= 2]
    if len(keys) > max_instants:
        step = len(keys) / float(max_instants)
        keys = [keys[int(i * step)] for i in range(max_instants)]
    n_over = 0
    for t in keys:
        pts = [inst[t][v] for v in sorted(inst[t])]
        x = np.array([p[0] for p in pts]); y = np.array([p[1] for p in pts])
        d = np.hypot(x[:, None] - x[None, :], y[:, None] - y[None, :])
        iu = np.triu_indices(x.size, k=1)
        n_over += int((d[iu] < OVERLAP_DIST_M).sum())
    return n_over, len(keys)


def _wave_speed_kmh(fd: dict) -> tuple[float | None, int]:
    """Backward wave speed = -slope of the congested branch of the flow-density scatter (km/h)."""
    q, k = fd.get("q"), fd.get("k")
    if q is None or q.size < 4:
        return None, 0
    k_crit = float(k[int(np.argmax(q))])
    m = k > k_crit
    n_cong = int(m.sum())
    if n_cong < 2:
        return None, n_cong
    kk, qq = k[m], q[m]
    if float(np.ptp(kk)) <= 0.0:
        return None, n_cong
    # q [veh/h/ln] vs k [veh/km/ln] -> slope is km/h directly
    slope = float(np.polyfit(kk, qq, 1)[0])
    return -slope, n_cong


def traffic_panel(emissions: list[dict], probe: dict, refdata: dict, *,
                  fd_cell_m: float = FD_CELL_M, fd_window_s: float = FD_WINDOW_S,
                  regime: str | None = None) -> list[dict]:
    P = "traffic"
    out: list[dict] = []
    # No early return on empty input: every metric below degrades to "na" with its own reason, so the
    # panel always has the SAME shape and a consumer can index it by metric id unconditionally.
    tracks = build_tracks(emissions)
    # the teleport bound is needed BEFORE the decomposition: a step that the teleport gate already
    # counts must not be counted a second time as an acceleration failure.
    sp_ref = _ref(refdata, "kinematics.speed_hard_bound_mps")
    tele_lim = float((sp_ref or {}).get("range", [0.0, 60.0])[1])
    kin = kinematics(tracks, MAX_FD_DT_S, speed_bound_mps=tele_lim)
    segs = segment_table(tracks, MAX_FD_DT_S, kin=kin)
    n_seg = int(segs.get("n", 0))
    out.append(_metric(
        "traffic.trace_segments", P, "Usable trajectory segments (finite-difference pairs)",
        n_seg, "count", n_seg, severity=SOFT,
        reason=("ground_truth/gt_emissions_sample.jsonl is missing or empty" if not emissions
                else "informational: sample size for every other traffic metric"),
        extra={"vehicles_with_track": len(tracks),
               "segments_dropped_sampling_gap": int(segs.get("n_dropped_gap", 0)),
               "max_finite_difference_dt_s": MAX_FD_DT_S,
               "emit_sample_prob": probe["emit_sample_prob"]}))

    thin = None if probe["full_trace"] else (
        f"emissions are a {probe['emit_sample_prob']:.3g} sample (emit_sample_prob < "
        f"{FULL_TRACE_MIN_PROB}); per-vehicle trajectories are lossy -- rerun with "
        f"emit_sample_prob=1.0 (python) / SCMS_EMIT_SAMPLE=1.0 (MOSAIC)")
    few = (None if n_seg >= MIN_SAMPLES else
           ("ground_truth/gt_emissions_sample.jsonl is missing or empty" if not emissions
            else f"only {n_seg} usable segments (need {MIN_SAMPLES})"))

    # ---- speeds -------------------------------------------------------------------------------
    # LONGITUDINAL speed: on a lane-change step the raw chord divides a whole lane width by one
    # sample interval, which at dt=0.1 s reads back as ~33 m/s of forward motion that never happened.
    spd = segs["speed_long"] if n_seg else np.zeros(0)
    reg = regime or probe.get("regime")
    reg_reason = None if reg else ("traffic regime unknown for this dataset (no road_network in the "
                                   "manifest); pass --regime urban|highway to score speed bands")
    for q, label in ((50, "p50"), (95, "p95")):
        ref = _ref(refdata, f"traffic_regimes.{reg}.speed_{label}_mps") if reg else None
        out.append(_metric(
            f"traffic.speed_{label}_mps", P, f"Benign true speed {label} ({reg or 'regime unknown'})",
            _pct(spd, q), "m/s", n_seg, ref, SOFT,
            reason=few or reg_reason,
            extra={"regime": reg,
                   "source_field": "longitudinal component of the true_x/true_y finite difference"}))
    out.append(_metric(
        "traffic.speed_max_mps", P, "Maximum finite-difference speed",
        float(spd.max()) if spd.size else None, "m/s", n_seg,
        _ref(refdata, "kinematics.speed_hard_bound_mps"), SOFT, reason=few,
        extra={"speed_max_raw_chord_mps": _r(float(segs["speed"].max()) if n_seg else None),
               "note": "value is longitudinal; the raw chord speed is shown for comparison"}))

    # ---- lateral position continuity -------------------------------------------------------------
    # This is the metric that EXPOSES the artefact the acceleration screen removes, so it comes
    # first: without a lane-change teleport counter, screening those steps out of the acceleration
    # series would delete the evidence instead of relocating it.
    n_pair = int(kin.get("n", 0))
    v_km = float(kin.get("path_m", 0.0)) / 1000.0
    lat_ev = kin["lateral"] if n_pair else np.zeros(0, dtype=bool)
    n_lat = int(lat_ev.sum())
    d_lat_abs = np.abs(kin["d_lat"]) if n_pair else np.zeros(0)
    lat_few = (None if (n_pair >= MIN_SAMPLES and v_km > 0.0) else
               ("ground_truth/gt_emissions_sample.jsonl is missing or empty" if not emissions
                else f"only {n_pair} consecutive-sample pairs (need {MIN_SAMPLES})"))
    out.append(_metric(
        "traffic.lateral_discontinuity_events", P,
        "Lane-change teleports (a lane-width sideways step inside one sample), per vehicle-km",
        (n_lat / v_km if lat_few is None else None), "events/vehicle-km", n_pair,
        _ref(refdata, "kinematics.lateral_discontinuity_per_vehicle_km_max"), SOFT, reason=lat_few,
        extra={
            "events": n_lat,
            "events_per_1000_sample_pairs": _r(1000.0 * n_lat / n_pair if n_pair else None),
            "vehicle_km": _r(v_km, 3), "sample_pairs": n_pair,
            "vehicle_km_note": "longitudinal path length, so a jump's sideways component does not "
                               "pad the denominator of the rate it is counted against",
            "lateral_jump_threshold_m": LATERAL_JUMP_M,
            "lateral_speed_threshold_mps": LATERAL_SPEED_MAX_MPS,
            "half_lane_steps": int((d_lat_abs >= LANE_WIDTH_M / 2.0).sum()),
            "full_lane_steps": int((d_lat_abs >= LANE_WIDTH_M).sum()),
            "lane_step_counts_note": "half_lane_steps/full_lane_steps are the RAW lateral-offset "
                                     "distribution with no physical guard applied -- they include "
                                     "cornering, so they exceed `events` on any engine that turns "
                                     "through 90 degrees inside one sample. Only `events` is the "
                                     "metric",
            "lateral_offset_p99_m": _r(_pct(d_lat_abs, 99)),
            "max_lateral_step_m": _r(float(d_lat_abs.max()) if n_pair else None),
            "sub_lane_screened_steps": (int(kin["lateral_screen"].sum()) - n_lat) if n_pair else 0,
            "sub_lane_screen_threshold_m": LATERAL_SCREEN_M,
            "longitudinal_reversal_events": int(kin["reversal"].sum()) if n_pair else 0,
            "longitudinal_reversal_per_vehicle_km": _r(
                (float(kin["reversal"].sum()) / v_km) if (n_pair and v_km > 0) else None),
            "method": "per consecutive-sample step, the displacement component ACROSS the smoothed "
                      f"direction of travel (circular median over +-{HEADING_WINDOW_PAIRS} steps). "
                      f"A step counts when it moves >= {LATERAL_JUMP_M:g} m sideways at more than "
                      f"{LATERAL_SPEED_MAX_MPS:g} m/s AND the RAW headings of the steps either side "
                      f"agree within {HEADING_STABLE_TOL_DEG:g} deg -- a lane change does not turn "
                      "the vehicle, a corner does, so cornering is not counted. A track's first and "
                      "last step are not scanned (no evidence on one side). Normalised per "
                      "vehicle-km of path, which is invariant to the CAM trigger rate; the "
                      "per-1000-step rate is given alongside and is not",
            "sub_lane_note": f"steps with a non-physical lateral speed but under {LATERAL_JUMP_M:g} m "
                             f"of offset (>= {LATERAL_SCREEN_M:g} m) are NOT counted in the headline "
                             "rate -- they are partial lane offsets, not lane changes -- but they "
                             "are screened out of the acceleration series for the same reason",
            "longitudinal_reversal_note": "the same discrete lane/edge position remapping seen "
                                          "along the road instead of across it: the next sample "
                                          f"lands more than {REVERSAL_M:g} m BEHIND this one"}))

    # ---- accelerations ------------------------------------------------------------------------
    acc = kin["accel"] if n_pair else np.zeros(0)
    acc_all = kin["accel_all"] if n_pair else np.zeros(0)
    acc_dt = kin["accel_pair_dt"] if n_pair else np.zeros(0)
    n_acc = int(acc.size)
    acc_few = (None if n_acc >= MIN_SAMPLES else
               f"only {n_acc} acceleration samples (need {MIN_SAMPLES})")
    hard_ref = _ref(refdata, "kinematics.accel_hard_bound_mps2")
    comf_ref = _ref(refdata, "kinematics.accel_comfort_band_mps2")
    lo, hi = (hard_ref or {}).get("range", (-8.0, 4.0))
    lo, hi = float(lo), float(hi)
    frac_hard = frac_comf = None
    if n_acc:
        frac_hard = float(np.mean((acc >= lo) & (acc <= hi)))
        clo, chi = (comf_ref or {}).get("range", (-3.0, 3.0))
        frac_comf = float(np.mean((acc >= float(clo)) & (acc <= float(chi))))
    # Irregular sampling (ETSI CAM triggering emits at 0.1-1.0 s here) is handled by (a) using the
    # exact midpoint separation as the second-difference denominator and (b) reporting the same
    # fraction stratified by that separation, so a small-dt artefact cannot hide inside the average.
    # Each acceleration sample counts once: they are independent second differences, and weighting
    # by dt would let one 1 s sample outvote ten 0.1 s ones taken over the same second of driving.
    by_dt = []
    for b_lo, b_hi in zip(ACCEL_DT_BINS_S[:-1], ACCEL_DT_BINS_S[1:]):
        m = (acc_dt > b_lo) & (acc_dt <= b_hi)
        if not m.any():
            continue
        a = acc[m]
        by_dt.append({"pair_dt_lo_s": b_lo, "pair_dt_hi_s": b_hi, "n": int(a.size),
                      "frac_within_band": _r(float(np.mean((a >= lo) & (a <= hi))), 6),
                      "accel_min": _r(float(a.min())), "accel_max": _r(float(a.max()))})
    out.append(_metric(
        "traffic.accel_within_hard_bound_frac", P,
        "Accelerations inside the human plausibility bound", frac_hard, "fraction", n_acc,
        {"ref_id": "kinematics.accel_hard_bound_mps2", "min": 1.0,
         "unit": "fraction", "confidence": (hard_ref or {}).get("confidence"),
         "cite": (hard_ref or {}).get("cite"), "source": (hard_ref or {}).get("source"),
         "note": "hard gate: the fraction inside the band must be exactly 1.0"} if hard_ref else None,
        HARD, reason=acc_few, nd=6,
        extra={"band_mps2": list((hard_ref or {}).get("range", [])) or None,
               "accel_p01": _r(_pct(acc, 1)), "accel_p99": _r(_pct(acc, 99)),
               "accel_min": _r(float(acc.min()) if n_acc else None),
               "accel_max": _r(float(acc.max()) if n_acc else None),
               "method": "second difference of the LONGITUDINAL speed (displacement projected on "
                         f"the direction of travel, smoothed over +-{HEADING_WINDOW_PAIRS} steps), "
                         "over the exact midpoint separation (dt_i + dt_i+1)/2",
               "screened_out": dict(kin.get("n_excl", {})),
               "screened_note": "position DISCONTINUITIES are not accelerations: lane-change "
                                "teleports go to traffic.lateral_discontinuity_events, teleports to "
                                "traffic.teleport_events, and a track's first/last step is a "
                                "partial insertion/arrival step, not a second of driving. "
                                f"screened_out.lateral uses the lower {LATERAL_SCREEN_M:g} m "
                                "sub-lane threshold, so it exceeds the reported event count",
               "samples_before_screening": int(acc_all.size),
               "unscreened_frac_within_band": _r(
                   float(np.mean((acc_all >= lo) & (acc_all <= hi))) if acc_all.size else None, 6),
               "unscreened_accel_min": _r(float(acc_all.min()) if acc_all.size else None),
               "unscreened_accel_max": _r(float(acc_all.max()) if acc_all.size else None),
               "unscreened_note": "the SAME longitudinal estimator with the discontinuity screen "
                                  "switched off (not the pre-fix raw-chord estimator)",
               "by_pair_dt_s": by_dt or None,
               "sampling_note": "each acceleration sample carries equal weight; the by_pair_dt_s "
                                "breakdown is what makes an interval-dependent artefact visible"}))
    out.append(_metric(
        "traffic.accel_within_comfort_frac", P,
        "Accelerations inside the comfort band (+/-3 m/s^2)", frac_comf, "fraction", n_acc,
        _ref(refdata, "kinematics.accel_comfort_min_fraction"), SOFT, reason=acc_few,
        extra={"band_mps2": list((comf_ref or {}).get("range", [])) or None}))

    # ---- sim health: teleports + overlaps + liveness ---------------------------------------------
    n_tele, n_tele_pairs = teleport_events(tracks, tele_lim)
    # Sample floor, like accel (MIN_SAMPLES) and overlap (10 instants): with none, a dataset whose
    # every real jump lands across a dropped sampling gap reported "0 teleports, pass" off ONE pair.
    tele_few = (None if n_tele_pairs >= MIN_SAMPLES else
                ("ground_truth/gt_emissions_sample.jsonl is missing or empty" if not emissions
                 else f"only {n_tele_pairs} consecutive-sample pairs (need {MIN_SAMPLES})"))
    out.append(_metric(
        "traffic.teleport_events", P, "Teleports (displacement implying a speed above the bound)",
        (n_tele if tele_few is None else None), "count", n_tele_pairs,
        _ref(refdata, "kinematics.teleport_events_max"), HARD, reason=tele_few,
        extra={"speed_bound_mps": tele_lim, "pairs_examined": n_tele_pairs,
               "method": "mean speed over EVERY consecutive sample pair, including gaps longer "
                         f"than max_finite_difference_dt_s ({MAX_FD_DT_S:g} s)"}))

    n_over, n_inst = _overlap_events(emissions)
    out.append(_metric(
        "traffic.overlap_events", P,
        f"Distinct vehicles overlapping (< {OVERLAP_DIST_M:g} m apart at one instant)",
        n_over if n_inst >= 10 else None, "count", n_inst,
        _ref(refdata, "kinematics.overlap_events_max"), HARD,
        reason=(None if n_inst >= 10 else
                f"only {n_inst} instants carry >=2 simultaneously-sampled vehicles (need 10)"),
        extra={"overlap_distance_m": OVERLAP_DIST_M, "instants_examined": n_inst}))

    # LIVENESS. Every other HARD gate is an impossibility check, and a fleet that never moves passes
    # all of them (no teleport, no overlap, no acceleration outside the band) while the headway
    # metrics degrade to "na" -- so without this the CI gate accepts a frozen dataset.
    mv_frac, n_tracks = moving_vehicle_fraction(tracks)
    out.append(_metric(
        "traffic.moving_vehicle_frac", P,
        f"Vehicles that actually move (track mean speed >= {LIVENESS_MIN_SPEED_MPS:g} m/s)",
        (mv_frac if n_tracks >= LIVENESS_MIN_VEHICLES else None), "fraction", n_tracks,
        _ref(refdata, "kinematics.moving_vehicle_min_fraction"), HARD,
        reason=(None if n_tracks >= LIVENESS_MIN_VEHICLES else
                ("ground_truth/gt_emissions_sample.jsonl is missing or empty" if not emissions
                 else f"only {n_tracks} vehicles carry a usable track "
                      f"(need {LIVENESS_MIN_VEHICLES})")),
        extra={"liveness_min_speed_mps": LIVENESS_MIN_SPEED_MPS,
               "vehicles_with_track": n_tracks,
               "method": "path length / track span per vehicle, so a vehicle queued for part of "
                         "its trip still counts as moving"}))

    # ---- time headways (edge proxy) ------------------------------------------------------------
    hw = _time_headways(segs) if (n_seg and probe["full_trace"]) else np.zeros(0)
    hw_reason = thin or (None if hw.size >= MIN_SAMPLES else
                         f"only {hw.size} in-lane leader/follower pairs (need {MIN_SAMPLES})")
    out.append(_metric(
        "traffic.headway_p50_s", P, "Time headway median (spacing / follower speed)",
        _pct(hw, 50), "s", int(hw.size), _ref(refdata, "traffic_regimes.saturation_headway_s"),
        SOFT, reason=hw_reason or "informational: run-wide median, not a capacity headway",
        extra={"lane_width_m": LANE_WIDTH_M, "lateral_tolerance_m": HEADWAY_LATERAL_TOL_M,
               "heading_tolerance_deg": HEADWAY_HEADING_TOL_DEG,
               "max_headway_s": HEADWAY_MAX_S,
               "leader_rule": "nearest vehicle ahead in the follower's own heading frame, within "
                              "half a lane width laterally (no spatial cell, so long headways are "
                              "not censored)",
               "headway_p15_s": _r(_pct(hw, 15)), "headway_p85_s": _r(_pct(hw, 85))}))
    floor_ref = _ref(refdata, "traffic_regimes.headway_implausible_below_s")
    floor_s = float((floor_ref or {}).get("value", 0.5))
    out.append(_metric(
        "traffic.headway_below_floor_frac", P,
        f"Time headways below the physical floor ({floor_s:g} s)",
        (float(np.mean(hw < floor_s)) if hw.size else None), "fraction", int(hw.size),
        _ref(refdata, "traffic_regimes.headway_implausible_max_fraction"), SOFT, reason=hw_reason,
        extra={"floor_s": floor_s, "floor_source_ref": "traffic_regimes.headway_implausible_below_s"}))
    ks_ref = _ref(refdata, f"traffic_regimes.{reg}.time_headway_ks_max") if reg else None
    ks_val = None
    if hw.size >= MIN_SAMPLES:
        h_min = float(hw.min())
        scale = float(hw.mean()) - h_min
        if scale > 0:
            ks_val = ks_statistic(hw, lambda z: 1.0 - np.exp(-np.maximum(z - h_min, 0.0) / scale))
    out.append(_metric(
        "traffic.headway_ks_shifted_exponential", P,
        "Headway shape: KS vs a fitted shifted-exponential (Cowan M3 family)",
        ks_val, "KS statistic", int(hw.size), ks_ref, SOFT,
        reason=hw_reason or reg_reason,
        extra={"model_ref": "traffic_regimes.time_headway_model",
               "fit": "h_min = sample minimum, scale = mean - h_min (shape test, like "
                      "calibration.ks_vs_fitted_rayleigh)"}))

    # ---- fundamental diagram --------------------------------------------------------------------
    fd = _fundamental_diagram(segs, probe["n_lanes"], fd_cell_m, fd_window_s) if n_seg else {"cells": 0}
    fd_reason = thin or (None if fd.get("cells", 0) >= 10 else
                         f"only {fd.get('cells', 0)} space-time cells met the sample floor (need 10)")
    cap = (float(np.percentile(fd["q"], 99)) if fd.get("cells") else None)
    out.append(_metric(
        "traffic.fd_capacity_veh_h_lane", P, "Fundamental diagram: capacity (p99 of cell flow)",
        cap, "veh/h/lane", fd.get("cells", 0),
        _ref(refdata, "fundamental_diagram.capacity_veh_per_h_per_lane"), SOFT, reason=fd_reason,
        extra={"method": "Edie generalised definitions over DIRECTIONAL space-time cells (space "
                         "cell x time window x heading octant); a square cell is treated as a road "
                         "section of side length (per-lane-group approximation, the dataset schema "
                         "carries no edge id). The per-lane divisor is measured from the lateral "
                         "spread inside each cell, floored at the manifest's n_lanes",
               "cell_m": fd_cell_m, "window_s": fd_window_s,
               "lanes_manifest_floor": probe["n_lanes"],
               "lanes_per_cell_p50": _r(_pct(fd["lanes_per_cell"], 50) if fd.get("cells") else None),
               "lanes_per_cell_max": _r(float(fd["lanes_per_cell"].max()) if fd.get("cells")
                                        else None),
               "flow_max_veh_h_lane": _r(float(fd["q"].max()) if fd.get("cells") else None),
               "density_p99_veh_km_lane": _r(_pct(fd["k"], 99) if fd.get("cells") else None)}))
    wave, n_cong = _wave_speed_kmh(fd) if fd.get("cells") else (None, 0)
    min_cong = int((_ref(refdata, "fundamental_diagram.min_congested_cells_for_wave_speed")
                    or {}).get("min", 10))
    out.append(_metric(
        "traffic.fd_backward_wave_speed_kmh", P, "Fundamental diagram: backward wave speed",
        wave if n_cong >= min_cong else None, "km/h", n_cong,
        _ref(refdata, "fundamental_diagram.backward_wave_speed_kmh"), SOFT,
        reason=fd_reason or (None if n_cong >= min_cong else
                             f"only {n_cong} congested-branch cells (need {min_cong})"),
        extra={"method": "negative slope of a least-squares line through cells denser than the "
                         "flow-maximising density"}))
    return out


# ================================================================================================
# COMM panel
# ================================================================================================
def _honest_links(reports: list[dict], labels: dict[str, dict]) -> list[dict]:
    """Reports whose subject is an HONEST vehicle and whose trigger is not distance-dependent.

    ``report_correctness == "false_positive"`` means an honest reporter flagged an honest subject, so
    the subject's CLAIMED position equals its measured true position -- the reconstruction the radio
    tests already rely on (tests/test_radio_propagation._honest_heard_dists). Reports triggered by
    ``acceptanceRangeThreshold`` are excluded because that reason code fires BECAUSE the link was
    long, which would bias the reconstructed distance distribution upward.
    """
    out = []
    for r in reports:
        lab = labels.get(r.get("report_id"))
        if lab is None or lab.get("report_correctness") != "false_positive":
            continue
        if "acceptanceRangeThreshold" in (r.get("reason_codes") or []):
            continue
        out.append(r)
    return out


def _gt_link_distances(links: list[dict], labels: dict[str, dict], tracks: dict[str, dict],
                       tol_s: float) -> tuple[np.ndarray, int]:
    """True reporter->subject distance at report time, from ground-truth trajectories.

    Engine-independent and uncensored: it never touches the detector's normalised score. Requires a
    trajectory sample for BOTH endpoints within `tol_s` of the detection time, so it degrades on
    sub-sampled emissions (``emit_sample_prob`` < 1).
    """
    d, miss = [], 0
    for r in links:
        lab = labels.get(r.get("report_id"))
        a = tracks.get(str((lab or {}).get("reporter_true_id")))
        b = tracks.get(str((lab or {}).get("subject_true_id")))
        t = r.get("detection_time", r.get("generation_time", r.get("ingest_time")))
        if a is None or b is None or t is None:
            miss += 1
            continue
        t = float(t)
        ia = int(np.argmin(np.abs(a["t"] - t)))
        ib = int(np.argmin(np.abs(b["t"] - t)))
        if abs(float(a["t"][ia]) - t) > tol_s or abs(float(b["t"][ib]) - t) > tol_s:
            miss += 1
            continue
        d.append(math.hypot(float(a["x"][ia]) - float(b["x"][ib]),
                            float(a["y"][ia]) - float(b["y"][ib])))
    return np.array(d, dtype=float), miss


def _art_link_distances(links: list[dict], probe: dict) -> tuple[np.ndarray, int]:
    """Fallback: reconstruct the heard distance from ``detnorm_acceptanceRangeThreshold``.

    python_mock: detnorm = max(0, d - radio_range_m)/art_max_m, so d = detnorm*art + range and a
    zero is CENSORED (the link was simply inside the range) -- censored links are returned as the
    censored count, not as a distance. mosaic: detnorm = d/ART_MAX_M, uncensored.
    """
    art, rr = probe["art_max_m"], probe["radio_range_m"]
    if art <= 0:
        return np.zeros(0, dtype=float), 0
    d, censored = [], 0
    for r in links:
        v = r.get("detnorm_acceptanceRangeThreshold")
        if v is None:
            continue
        v = float(v)
        if probe["art_censored"]:
            if v <= 0.0:
                censored += 1
                continue
            d.append(v * art + rr)
        else:
            if v <= 0.0:
                censored += 1
                continue
            d.append(v * art)
    return np.array(d, dtype=float), censored


def _pdr_curve(heard: np.ndarray, snaps: list[dict], bin_m: float, max_m: float,
               min_opportunities: int = MIN_BIN_OPPORTUNITIES) -> dict:
    """Normalised reception-vs-distance curve.

    Numerator: observed honest report links per distance bin. Denominator: co-present vehicle pairs
    per distance bin (reception OPPORTUNITIES) from the ground-truth snapshots. Their ratio is
    proportional to PDR(d) -- the unknown constant (report-trigger probability x sampling rate) is
    removed by normalising on the nearest populated bin, which is why the output is an AWARENESS
    RATIO shape, not an absolute PDR. Excluding distance-triggered reason codes upstream is what
    makes the trigger probability distance-independent.

    Both counts are Poisson, so a bin is only usable once it carries `min_opportunities` co-presence
    pairs; thinner bins are masked out (``usable``) instead of contributing a ratio estimated from a
    handful of pairs. Consumers must test ``usable``, not ``den > 0``.
    """
    edges = np.arange(0.0, max_m + bin_m, bin_m)
    num = np.histogram(heard, bins=edges)[0].astype(float)
    den = _pair_distance_hist(snaps, edges).astype(float)
    usable = den >= float(min_opportunities)
    with np.errstate(divide="ignore", invalid="ignore"):
        ratio = np.where(usable, num / np.where(usable, den, 1.0), np.nan)
    base = None
    for i in range(ratio.size):
        if usable[i] and num[i] > 0 and np.isfinite(ratio[i]):
            base = float(ratio[i])
            break
    norm = ratio / base if base else ratio * np.nan
    return {"edges": edges, "num": num, "den": den, "usable": usable, "ratio": ratio,
            "normalized": norm, "base_ratio": base, "bin_m": bin_m,
            "min_opportunities": int(min_opportunities)}


def _curve_at(curve: dict, dist_m: float) -> tuple[float | None, int]:
    """Normalised reception ratio over the band of half a bin either side of `dist_m`.

    Every bin overlapping ``[d - bin/2, d + bin/2]`` contributes, so an anchor that lands exactly on
    a bin edge (100/200/300 m with the default 50 m bins) averages its two neighbours instead of
    silently picking one of them.
    """
    edges, bin_m = curve["edges"], curve["bin_m"]
    lo, hi = dist_m - bin_m / 2.0, dist_m + bin_m / 2.0
    sel = (edges[:-1] < hi - 1e-9) & (edges[1:] > lo + 1e-9) & curve["usable"]
    if not sel.any():
        return None, 0
    num, den = float(curve["num"][sel].sum()), float(curve["den"][sel].sum())
    if den <= 0 or not curve["base_ratio"]:
        return None, int(num)
    return (num / den) / curve["base_ratio"], int(num)


def _crossing(curve: dict, level: float) -> float | None:
    """Distance at which the normalised curve drops below `level` FOR GOOD (linear interpolation).

    Both ends of the ratio are Poisson counts, so one unlucky bin can dip below a level the radio has
    not actually reached yet -- taking the first downward crossing of the raw curve therefore reports
    a wide stochastic gray zone for a hard unit-disc radio that has none (the exact case
    refdata/v2x_awareness.json's ``pdr_gray_zone_width_min_m`` note says must fail by construction).

    PDR is non-increasing in distance by construction, so the crossing is read off the least
    non-increasing majorant of the measured curve -- the running maximum taken from the far end. A
    lone dip is absorbed, a genuine decay is preserved unchanged, and because the majorant is >= the
    measurement everywhere, this can only ever move a crossing OUTWARD: the estimator cannot invent a
    gray zone that the data does not show.
    """
    edges, y = curve["edges"], curve["normalized"]
    centres = (edges[:-1] + edges[1:]) / 2.0
    ok = np.isfinite(y) & curve["usable"]
    if ok.sum() < 2:
        return None
    xs = centres[ok]
    ys = np.maximum.accumulate(y[ok][::-1])[::-1]
    for i in range(1, xs.size):
        if ys[i - 1] >= level > ys[i]:
            span = ys[i - 1] - ys[i]
            if span <= 0:
                return float(xs[i])
            return float(xs[i - 1] + (xs[i] - xs[i - 1]) * (ys[i - 1] - level) / span)
    return None


def comm_panel(emissions: list[dict], reports: list[dict], report_labels: list[dict],
               tracks: dict[str, dict], probe: dict, refdata: dict, *,
               t_bucket_s: float = T_BUCKET_S, dist_bin_m: float = DIST_BIN_M,
               max_dist_m: float = MAX_LINK_DIST_M, regime: str | None = None) -> list[dict]:
    P = "comm"
    out: list[dict] = []
    no_reports = (not reports or not report_labels)
    labels = {r.get("report_id"): r for r in report_labels}
    links = [] if no_reports else _honest_links(reports, labels)
    tol = max(t_bucket_s, probe.get("dt_s") or 0.0, 1.0)
    gt_d, gt_miss = _gt_link_distances(links, labels, tracks, tol)
    method, heard, censored = "gt_link_distance", gt_d, 0
    if gt_d.size < MIN_SAMPLES:
        art_d, censored = _art_link_distances(links, probe)
        if art_d.size > gt_d.size:
            method, heard = "art_reconstruction", art_d

    out.append(_metric(
        "comm.honest_links", P, "Honest (false-positive) report links usable for the PDR curve",
        int(heard.size), "count", int(heard.size), severity=SOFT,
        reason="informational: sample size for every other comm metric",
        extra={"reconstruction_method": method, "candidate_reports": len(links),
               "total_reports": len(reports),
               "gt_endpoint_lookup_failures": int(gt_miss),
               "art_censored_links": int(censored),
               "note": "distance-triggered reports (acceptanceRangeThreshold) are excluded so the "
                       "report-trigger probability is distance-independent"}))

    snaps = _snapshots(emissions, t_bucket_s)
    n_opp = sum(len(s["vids"]) * (len(s["vids"]) - 1) // 2 for s in snaps)
    few = None
    if no_reports:
        few = "ma/ma_reports.jsonl or ground_truth/gt_report_labels.jsonl is missing or empty"
    elif heard.size < MIN_SAMPLES:
        few = (f"only {heard.size} reconstructable honest links (need {MIN_SAMPLES}); "
               f"emissions sampled at {probe['emit_sample_prob']:.3g}")
    elif n_opp < MIN_SAMPLES:
        few = f"only {n_opp} co-presence pairs to normalise against (need {MIN_SAMPLES})"

    curve = _pdr_curve(heard, snaps, dist_bin_m, max_dist_m) if not few else None
    curve_pts = []
    if curve is not None:
        for i in range(curve["edges"].size - 1):
            if curve["den"][i] <= 0:
                continue
            curve_pts.append({"d_lo_m": _r(curve["edges"][i], 1),
                              "d_hi_m": _r(curve["edges"][i + 1], 1),
                              "links": int(curve["num"][i]), "opportunities": int(curve["den"][i]),
                              "usable": bool(curve["usable"][i]),
                              "normalized_ratio": _r(curve["normalized"][i])})

    reg = regime or probe.get("regime")
    aw_ref = _ref(refdata, "v2x_awareness.awareness_ratio_200m_urban_min")
    for anchor in AWARENESS_ANCHORS_M:
        val, n_at = (_curve_at(curve, anchor) if curve is not None else (None, 0))
        ref = aw_ref if (anchor == 200.0 and reg == "urban") else None
        out.append(_metric(
            f"comm.awareness_ratio_{int(anchor)}m", P,
            f"Neighbour awareness ratio at {int(anchor)} m (normalised)",
            val, "fraction", n_at, ref, SOFT,
            reason=few or (None if ref is not None else
                           "no reference gate at this distance/regime "
                           "(v2x_awareness pins 200 m urban and 500 m highway)"),
            extra={"regime": reg, "reconstruction_method": method,
                   "normalization": "ratio of observed honest links to co-presence opportunities, "
                                    "divided by the same ratio in the nearest populated bin"}))

    d90 = _crossing(curve, 0.90) if curve is not None else None
    d20 = _crossing(curve, 0.20) if curve is not None else None
    gray = (d20 - d90) if (d90 is not None and d20 is not None) else None
    out.append(_metric(
        "comm.pdr_gray_zone_width_m", P, "PDR gray-zone width (awareness 90% -> 20%)",
        gray, "m", int(heard.size), _ref(refdata, "v2x_awareness.pdr_gray_zone_width_min_m"),
        SOFT,
        reason=few or (None if gray is not None else
                       "the reconstructed curve never crosses both the 0.90 and 0.20 levels inside "
                       f"{max_dist_m:g} m (a hard-cutoff/unit-disc radio has no gray zone)"),
        extra={"d_at_0p90_m": _r(d90, 1), "d_at_0p20_m": _r(d20, 1),
               "crossing_method": "levels are crossed on the least non-increasing majorant of the "
                                  "measured curve (running maximum from the far end), so a single "
                                  "Poisson-noise dip cannot manufacture a gray zone",
               "min_bin_opportunities": curve["min_opportunities"] if curve is not None else None,
               "curve": curve_pts or None}))
    d50 = _crossing(curve, 0.50) if curve is not None else None
    out.append(_metric(
        "comm.effective_range_m", P, "Effective range (awareness ratio crosses 0.5)",
        d50, "m", int(heard.size), None, SOFT,
        reason=few or "informational: tracked, no pass/fail band (the curve is proportional-to-PDR, "
                      "not absolute -- see v2x_awareness.los_high_pdr_range_m for the pinned anchor)",
        extra={"nominal_radio_range_m": _r(probe["radio_range_m"], 1) or None}))

    # ---- CAM inter-packet gap -------------------------------------------------------------------
    gaps: list[float] = []
    for vid in sorted(tracks):
        dt = np.diff(tracks[vid]["t"])
        gaps.extend(float(v) for v in dt[dt > 0])
    ga = np.array(gaps, dtype=float)
    gap_reason = (None if probe["full_trace"] else
                  f"emissions are a {probe['emit_sample_prob']:.3g} sample, so observed CAM gaps are "
                  f"inflated by 1/emit_sample_prob; rerun with full emission tracing")
    if not gap_reason and ga.size < MIN_SAMPLES:
        gap_reason = f"only {ga.size} inter-emission gaps (need {MIN_SAMPLES})"
    out.append(_metric(
        "comm.cam_inter_packet_gap_p50_s", P, "CAM inter-packet gap (median, per vehicle)",
        _pct(ga, 50), "s", int(ga.size), _ref(refdata, "etsi_cam_dcc.cam_interval_s"), SOFT,
        reason=gap_reason,
        extra={"gap_p95_s": _r(_pct(ga, 95)), "gap_min_s": _r(float(ga.min()) if ga.size else None),
               "note": "one CAM per vehicle per step in the Python engine (1 Hz at dt=1.0 s); the "
                       "MOSAIC layer implements ETSI dynamic triggering (1-10 Hz)"}))
    return out


# ================================================================================================
# scorecard
# ================================================================================================
def scorecard(dataset_dir: str, refdata: dict | str | None = None, *, regime: str = "auto",
              art_max_m: float | None = None, radio_range_m: float | None = None,
              t_bucket_s: float = T_BUCKET_S, dist_bin_m: float = DIST_BIN_M,
              max_dist_m: float = MAX_LINK_DIST_M, fd_cell_m: float = FD_CELL_M,
              fd_window_s: float = FD_WINDOW_S) -> dict:
    """Score one dataset directory. Read-only, deterministic, never raises on a missing signal.

    Raises FileNotFoundError only when `dataset_dir` itself is not a directory.
    """
    dataset_dir = os.fspath(dataset_dir)
    if not os.path.isdir(dataset_dir):
        raise FileNotFoundError(f"dataset directory not found: {dataset_dir}")
    rd = load_refdata(refdata) if (refdata is None or isinstance(refdata, str)) else refdata

    probe = probe_dataset(dataset_dir)
    if art_max_m is not None:
        probe["art_max_m"] = float(art_max_m)
    if radio_range_m is not None:
        probe["radio_range_m"] = float(radio_range_m)
    reg = None if regime == "auto" else regime

    gt = os.path.join(dataset_dir, "ground_truth")
    ma = os.path.join(dataset_dir, "ma")
    emissions = _jsonl(os.path.join(gt, "gt_emissions_sample.jsonl"))
    reports = _jsonl(os.path.join(ma, "ma_reports.jsonl"))
    rlabels = _jsonl(os.path.join(gt, "gt_report_labels.jsonl"))
    tracks = build_tracks(emissions)

    traffic = traffic_panel(emissions, probe, rd, fd_cell_m=fd_cell_m, fd_window_s=fd_window_s,
                            regime=reg)
    comm = comm_panel(emissions, reports, rlabels, tracks, probe, rd, t_bucket_s=t_bucket_s,
                      dist_bin_m=dist_bin_m, max_dist_m=max_dist_m, regime=reg)
    metrics = traffic + comm
    counts = {"pass": 0, "fail": 0, "na": 0}
    for m in metrics:
        counts[m["status"]] = counts.get(m["status"], 0) + 1
    hard_fail = [m["id"] for m in metrics if m["status"] == "fail" and m["severity"] == HARD]
    soft_fail = [m["id"] for m in metrics if m["status"] == "fail" and m["severity"] == SOFT]
    return {
        "dataset_dir": dataset_dir,
        "probe": probe,
        "refdata": {"dir": rd.get("dir"), "sets": sorted(rd.get("sets", {})),
                    "n_entries": len(rd.get("entries", {}))},
        "settings": {"regime": reg or "auto", "t_bucket_s": t_bucket_s, "dist_bin_m": dist_bin_m,
                     "max_dist_m": max_dist_m, "fd_cell_m": fd_cell_m, "fd_window_s": fd_window_s,
                     "max_finite_difference_dt_s": MAX_FD_DT_S, "min_samples": MIN_SAMPLES},
        "panels": {"traffic": traffic, "comm": comm},
        "summary": {**counts, "total": len(metrics),
                    "hard_failures": hard_fail, "soft_failures": soft_fail},
    }


def hard_failures(card: dict) -> list[str]:
    """Human-readable lines for every HARD metric that failed (the CI gate)."""
    out = []
    for m in card.get("panels", {}).get("traffic", []) + card.get("panels", {}).get("comm", []):
        if m["status"] == "fail" and m["severity"] == HARD:
            ref = m.get("reference") or {}
            out.append(f"REALISM HARD FAIL [{m['id']}] {m['title']}: {m['value']} {m['unit']} "
                       f"(reference {_ref_text(ref)}; {ref.get('cite', 'no citation')})")
    return out


def warning_lines(card: dict) -> list[str]:
    """Warning lines for every failing metric (hard first), for corpus_report / CI output."""
    lines = list(hard_failures(card))
    for m in card.get("panels", {}).get("traffic", []) + card.get("panels", {}).get("comm", []):
        if m["status"] == "fail" and m["severity"] != HARD:
            ref = m.get("reference") or {}
            lines.append(f"Realism metric out of reference range [{m['id']}] {m['title']}: "
                         f"{m['value']} {m['unit']} (reference {_ref_text(ref)}; "
                         f"{ref.get('cite', 'no citation')})")
    return lines


def _ref_text(ref: dict) -> str:
    if not ref:
        return "none"
    if ref.get("range") is not None:
        return f"{ref['range'][0]}-{ref['range'][1]}"
    if ref.get("min") is not None and ref.get("max") is not None:
        return f"{ref['min']}-{ref['max']}"
    if ref.get("min") is not None:
        return f">= {ref['min']}"
    if ref.get("max") is not None:
        return f"<= {ref['max']}"
    return str(ref.get("value", "none"))


_ICON = {"pass": "✅", "fail": "⚠️", "na": "—"}


def render_lines(card: dict, include_na: bool = False) -> list[str]:
    """Compact one-line-per-metric rendering for the datasheet's realism scorecard."""
    lines: list[str] = []
    for panel in ("traffic", "comm"):
        rows = [m for m in card.get("panels", {}).get(panel, [])
                if include_na or m["status"] != "na"]
        if not rows:
            continue
        lines.append(f"- **{panel.capitalize()} panel** "
                     f"({sum(1 for m in rows if m['status'] == 'pass')} pass / "
                     f"{sum(1 for m in rows if m['status'] == 'fail')} fail):")
        for m in rows:
            ref = m.get("reference") or {}
            val = "n/a" if m["value"] is None else f"{m['value']}"
            tail = (f" (ref {_ref_text(ref)} {ref.get('unit', m['unit'])}; {ref['cite']})"
                    if ref.get("cite") else "")
            if m["status"] == "na" and m.get("reason"):
                tail += f" — {m['reason']}"
            lines.append(f"    - {_ICON[m['status']]} {m['title']}: **{val} {m['unit']}**{tail}")
    return lines


# ================================================================================================
# CLI
# ================================================================================================
def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        description="Score a dataset's traffic + communication realism against pinned references.")
    p.add_argument("dataset_dir", help="a dataset directory (python mock_pipeline or MOSAIC layer)")
    p.add_argument("--refdata", default=None, help=f"reference-summary directory (default {REFDATA_DIR})")
    p.add_argument("--json", dest="json_out", default=None, help="write the scorecard JSON here")
    p.add_argument("--regime", choices=("auto", "urban", "highway"), default="auto",
                   help="traffic regime for the speed/headway reference bands (default: from the manifest)")
    p.add_argument("--art-max-m", type=float, default=None,
                   help="override the acceptanceRangeThreshold normaliser (MOSAIC: SCMS_ART_MAX_M)")
    p.add_argument("--radio-range-m", type=float, default=None, help="override the nominal radio range")
    p.add_argument("--bin-m", type=float, default=DIST_BIN_M, help="PDR-vs-distance bin width (m)")
    p.add_argument("--max-dist-m", type=float, default=MAX_LINK_DIST_M,
                   help="farthest distance the comm curve is reconstructed over (m)")
    p.add_argument("--t-bucket-s", type=float, default=T_BUCKET_S, help="co-presence snapshot width (s)")
    p.add_argument("--fd-cell-m", type=float, default=FD_CELL_M, help="fundamental-diagram cell side (m)")
    p.add_argument("--fd-window-s", type=float, default=FD_WINDOW_S,
                   help="fundamental-diagram time window (s)")
    p.add_argument("--markdown", action="store_true", help="print the compact scorecard lines instead of JSON")
    p.add_argument("--fail-on-hard", action="store_true",
                   help="exit 1 when a HARD metric fails (CI gate; default is measure-only exit 0)")
    a = p.parse_args(argv)

    if not os.path.isdir(a.dataset_dir):
        p.error(f"dataset directory not found: {a.dataset_dir}")
    card = scorecard(a.dataset_dir, a.refdata, regime=a.regime, art_max_m=a.art_max_m,
                     radio_range_m=a.radio_range_m, t_bucket_s=a.t_bucket_s, dist_bin_m=a.bin_m,
                     max_dist_m=a.max_dist_m, fd_cell_m=a.fd_cell_m, fd_window_s=a.fd_window_s)
    if a.markdown:
        text = "\n".join(render_lines(card, include_na=True))
        try:
            print(text)
        except UnicodeEncodeError:      # legacy console codepage: drop the status glyphs
            print(text.encode("ascii", "replace").decode("ascii"))
    else:
        print(json.dumps(card, indent=2, default=str))
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(card, fh, indent=2, default=str)
            fh.write("\n")
        print(f"\n[wrote {a.json_out}]")
    for line in hard_failures(card):
        print(line)
    return 1 if (a.fail_on_hard and card["summary"]["hard_failures"]) else 0


if __name__ == "__main__":
    raise SystemExit(main())
