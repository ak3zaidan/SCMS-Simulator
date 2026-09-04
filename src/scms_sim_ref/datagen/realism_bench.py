"""Realism benchmark harness: score a generated dataset against pinned real-world references.

Where `calibration.py` calibrates ONE quantity (benign GNSS error vs a literature Rayleigh model),
this module measures the two things that decide whether a V2X misbehaviour dataset resembles a real
city: how the traffic MOVES and how the radio REACHES. Same shape as calibration.py -- numpy only,
read-only over a dataset directory, deterministic (no timestamps, no RNG), JSON out, embedded into
`datasheet.py` and usable as a CI gate from `corpus_report.py`.

    python -m scms_sim_ref.datagen.realism_bench <dataset_dir> [--refdata <dir>] [--json out.json]

TRAFFIC panel: per-vehicle finite-difference speeds and accelerations, time-headway distributions
per spatial cell (an edge proxy -- the dataset schema carries no edge id), an Edie-generalised
fundamental diagram over space-time cells, and two hard sim-health counters (teleports, vehicle
overlaps).

**THE TRAFFIC PANEL MUST NOT BE READ OFF A STREAM THAT ENFORCEMENT TRUNCATES.**
``ground_truth/gt_emissions_sample.jsonl`` is written INSIDE the engine's broadcast pre-pass, whose
first statement is ``if enforced(tx, t): continue`` -- so a revoked vehicle's kinematic record ENDS
at revocation while the vehicle keeps driving. That is exactly right for a detection dataset and
exactly wrong for a traffic measurement, and the error is not a constant: it GROWS with run length
and with the misbehaviour authority's false-positive rate. Measured on the InTAS AM peak hour --
9,143 of 14,896 vehicles revoked (61.38%), 5,949,526 of 13,589,568 vehicle-steps surviving (43.78%),
detection precision 0.308 so most of those revocations were BENIGN. At 60-300 s the same term costs
-9% to -19% of the vehicle-steps, which is why it was invisible at the durations and densities this
panel was previously measured at.

``resolve_mobility_source`` therefore picks the traffic panel's input in this order, and NAMES the
choice in the scorecard:

  1. ``ground_truth/gt_mobility_oracle.jsonl`` -- the engine's opt-in un-enforced record
     (``PipelineConfig.emit_mobility_oracle`` / ``--emit-mobility-oracle``): every active station,
     every step, whatever the CRL says. Unbiased by construction, survivorship 1.000.
  2. a frozen SUMO trace, when the caller ASKS for one (``--sumo-trace``, or ``--traffic-source
     trace`` to use the path ``manifest["config"]["sumo_trace"]`` pins) -- for a ``sumo_replay``
     dataset this IS the mobility, and it lets an already-generated dataset be re-measured without a
     re-run. Never picked up implicitly: an absolute path that may or may not exist on this host
     must not decide what a scorecard says.
  3. ``gt_emissions_sample.jsonl`` -- the truncated fallback, marked ``truncated=True``. Its
     survivorship is published beside every metric read from it, and the density-dependent metrics
     (headways, fundamental diagram, overlaps) degrade to ``na`` below ``SURVIVORSHIP_MIN_FRAC``
     rather than reporting a confidently wrong number.

The unbiased record is ORACLE and stays ORACLE: it lives under ``ground_truth/``, carries
``_visibility=ORACLE``, is withheld from an isolated third-party detector exactly like the answer
key, and this module -- which never writes into ``ma/`` or ``ml/`` and emits only aggregates -- is
the only consumer. The COMM panel deliberately keeps reading the BROADCAST stream: what the MA could
hear is the honest input to a reception measurement.

KINEMATIC SOURCE (ADR 0002). Where the emission record carries the simulator's own ``true_speed`` /
``true_heading``, those are used directly and the scorecard says so in ``kinematics_source`` (and in
each affected metric's ``details.kinematics_source``). Where it does not -- every dataset generated
before the ADR -- speed and acceleration are finite differences of the true POSITION, and a finite
difference of position measures whatever moved the position, including things that are not driving.
SUMO changes lane by re-assigning the vehicle from one lane centreline to the next in a single step
(``--lanechange.duration`` defaults to 0), which puts a whole lane width of sideways displacement
inside one sample; differenced twice at a 0.1 s CAM interval that is a 276 m/s^2 "acceleration".
``track_kinematics`` therefore splits every step into a LONGITUDINAL and a LATERAL component against
a median-smoothed direction of travel, scores acceleration on the longitudinal component only, and
publishes the lateral jumps as their own metric (``traffic.lateral_discontinuity_events``) instead of
letting them masquerade as dynamics.

Every finite difference -- acceleration AND the lateral discontinuity count, and the vehicle-km each
is normalised by -- is taken over sample pairs no wider than ``MAX_FD_DT_S``. Nothing about a wider
gap is a measurement of the vehicle's motion, so a sub-sampled dataset degrades to ``na`` rather than
reporting where a vehicle got to while nobody was looking.

COMM panel (from ``ma/ma_reports.jsonl`` + ``ground_truth/gt_report_labels.jsonl`` + emissions):
a PDR-vs-distance curve reconstructed from HONEST (false-positive) report links, the neighbour
awareness ratio at 100/200/300 m, the effective range, the gray-zone width, and the CAM
inter-packet gap.

AWARENESS IS ALSO MEASURED A SECOND, INDEPENDENT WAY (``datagen/awareness.py``, appended to the comm
panel when a ``dataset_dir`` is available). The reconstructed curve above is *proportional* to PDR --
normalised at the near band by an unknown constant -- so it cannot be compared to any absolute
threshold, and the 0.90 anchor it used to be graded against turns out to be a >=1-of-Z per-second
NAR over a 3-9 vehicle test fleet at a 6 dB richer link budget (conditions transcribed in
``refdata/v2x_awareness_conditions.json``, analysis in ``docs/realism/AWARENESS-GATE.md``). The
second path classifies every co-present pair LOS/NLOSv/NLOSb on the scenario's own geometry and
integrates the configured physics into an ABSOLUTE per-packet PDR, which can. The two agree on
effective range to 3-13% wherever real building geometry (or no blockage model at all) is present,
and diverge by 1.7x only under the synthetic urban-canyon fallback.

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
import sys

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
# --- traffic-panel mobility source + survivorship (see the module docstring) ---------------------
EMISSIONS_REL = "ground_truth/gt_emissions_sample.jsonl"      # MA-hearable; ENFORCEMENT-TRUNCATED
MOBILITY_ORACLE_REL = "ground_truth/gt_mobility_oracle.jsonl"  # un-enforced; the unbiased source
LINKAGE_REVOCATION_REL = "ground_truth/gt_linkage_revocation.jsonl"
GT_VEHICLE_REL = "ground_truth/gt_vehicle.jsonl"
#: Below this fraction of surviving vehicle-steps a DENSITY-DEPENDENT metric read from the truncated
#: stream is not a measurement of the traffic and is published as ``na``. 0.90 is deliberately
#: generous: the peak hour sits at 0.4378 and the 300 s grid reference run at 0.8126, so the gate
#: separates "a few vehicles went quiet" from "most of the traffic is missing" rather than trying to
#: certify a tolerance nobody has calibrated.
SURVIVORSHIP_MIN_FRAC = 0.90
#: The metrics a missing vehicle changes DIRECTLY -- it removes a leader, a follower, a cell
#: occupancy or an overlap partner -- so they are gated on survivorship. The per-sample
#: distributional metrics (speed quantiles, acceleration fractions, lateral discontinuities per
#: vehicle-km, moving fraction) are distorted only through WHICH steps survive; they carry the
#: survivorship number but are not withheld, because the selection effect is second order and
#: measurable (see docs/realism/TRAFFIC-PANEL-SURVIVORSHIP.md).
SURVIVORSHIP_GATED_METRICS = (
    "traffic.headway_p50_s", "traffic.headway_below_floor_frac",
    "traffic.headway_ks_shifted_exponential", "traffic.fd_capacity_veh_h_lane",
    "traffic.fd_backward_wave_speed_kmh", "traffic.overlap_events",
)
#: ... of which these are ONE-SIDED: truncation can only REMOVE vehicles, so it can only DECREASE a
#: co-presence count. The truncated value is therefore a valid LOWER BOUND, and a value that already
#: breaches a max-bound reference is a real breach that more traffic can only make worse. Withholding
#: it would disarm a HARD CI gate on exactly the datasets that most need it (measured: the peak hour
#: reads 28 overlaps truncated against 169 unbiased -- the truncated number is wrong, but it is not
#: wrong about the failure). A PASS from the same number is worthless and is still withheld.
SURVIVORSHIP_LOWER_BOUND_METRICS = ("traffic.overlap_events",)
#: SUMO trace artifact tag (``mock_pipeline.sumo_trace.TRACE_FORMAT``), duplicated rather than
#: imported so this module keeps its "numpy only, read-only" dependency contract.
TRACE_FORMAT = "scms-sumo-trace/1"
# --- ADR 0002: true speed / heading in the ground-truth record (consumed WHEN PRESENT) -----------
GT_SPEED_FIELD = "true_speed"      # ORACLE scalar speed at emission time, m/s
GT_HEADING_FIELD = "true_heading"  # ORACLE heading at emission time; the convention is DETECTED
GT_MIN_STEPS = 30          # usable moving steps needed before a ground-truth field is trusted
GT_HEADING_MAX_RESIDUAL_DEG = 20.0    # ... and the detected convention must fit the chords this well
GT_SPEED_MAX_RESIDUAL_MPS = 3.0       # ... and the speed must agree with the chord speed this well

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
        # The replayed-mobility pin, so `resolve_mobility_source` can find the frozen trace a
        # sumo_replay dataset was driven from without being told where it is. A path, not data --
        # the trace is only READ, and only when it is still on disk.
        "config": {"sumo_trace": cfg.get("sumo_trace"),
                   "sumo_trace_sha256": cfg.get("sumo_trace_sha256"),
                   "mobility_source": cfg.get("mobility_source"),
                   "emit_mobility_oracle": bool(cfg.get("emit_mobility_oracle", False)),
                   "duration_s": cfg.get("duration_s")},
        "has_emissions": os.path.exists(
            os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")),
        "has_mobility_oracle": os.path.exists(
            os.path.join(dataset_dir, "ground_truth", "gt_mobility_oracle.jsonl")),
        "has_reports": os.path.exists(os.path.join(dataset_dir, "ma", "ma_reports.jsonl")),
        "has_report_labels": os.path.exists(
            os.path.join(dataset_dir, "ground_truth", "gt_report_labels.jsonl")),
    }


# ================================================================================================
# the traffic panel's MOBILITY SOURCE, and how much of the traffic it kept
# ================================================================================================
def read_sumo_trace(path: str, *, t0: float = 0.0) -> list[dict]:
    """Read a frozen ``scms-sumo-trace/1`` artifact into emission-shaped rows.

    For a ``mobility_source="sumo_replay"`` dataset this file IS the mobility -- the adapter is
    measured bit-exact against it (max |dx|,|dy| = 0.000 m over 709,454 samples at 300 s, position
    and speed match fraction 1.000 over 5,949,526 at the peak hour) -- and unlike the emission
    stream it has never met the SCMS layer. It is therefore the unbiased source for an
    ALREADY-GENERATED dataset, which is what makes an hour-long run re-measurable in minutes
    instead of the ~2 h a re-run costs.

    Two conversions, both load-bearing:

      * **time.** The artifact's step *k* is SUMO time ``step0_sim_time + k*dt``; the engine labels
        the same state ``t = k*dt``. Rows are stamped with the ENGINE's label (plus ``t0``) so a
        trace-sourced panel and an emission-sourced one are directly comparable.
      * **heading.** The artifact carries SUMO's convention (degrees CLOCKWISE from North); the
        engine's is degrees counter-clockwise from East. ``(90 - angle) % 360`` converts it, which
        is the same mapping ``HEADING_CONVENTIONS["deg_cw_from_north"]`` applies -- so the panel's
        own convention detector independently re-confirms it on the resulting rows.

    Vehicle ids are the trace's own SUMO ids (one interned string per vehicle, so a multi-million
    row artifact costs one id object per trajectory rather than one per sample).
    """
    rows: list[dict] = []
    dt = 1.0
    names: dict[int, str] = {}
    with open(path, encoding="utf-8") as fh:
        head = fh.readline().strip()
        if head != f"#{TRACE_FORMAT}":
            raise ValueError(f"{path}: not a {TRACE_FORMAT} artifact (first line {head!r})")
        for line in fh:
            if not line:
                continue
            c = line[0]
            if c == "#":
                if line.startswith("#meta "):
                    dt = float(json.loads(line[6:]).get("dt", 1.0) or 1.0)
                continue
            if c == "V":
                p = line.split()
                names[int(p[1])] = sys.intern(p[2])
                continue
            p = line.split()
            if len(p) < 6:
                continue
            idx = int(p[1])
            rows.append({
                "t": round(t0 + int(p[0]) * dt, 6),
                "true_vehicle_id": names.get(idx, f"trace_{idx}"),
                "true_x": float(p[2]), "true_y": float(p[3]),
                "true_speed": float(p[4]),
                "true_heading": (90.0 - float(p[5])) % 360.0,
            })
    return rows


def _manifest_survivorship(dataset_dir: str) -> dict | None:
    """``manifest["counts"]["mobility_survivorship"]`` -- the engine's own step-loop tallies.

    Stamped by every run since the survivorship change (``run._survivorship_block``) and outside
    ``data_digest`` by construction, so it is present without having re-pinned anything. It is the
    ONLY exact denominator a truncated dataset can carry: a stream that stops at revocation cannot
    report what it did not record.
    """
    try:
        with open(os.path.join(dataset_dir, "manifest.json"), encoding="utf-8") as fh:
            blk = (json.load(fh).get("counts") or {}).get("mobility_survivorship")
    except (OSError, ValueError):
        return None
    return blk if isinstance(blk, dict) else None


def _count_lines(path: str) -> int:
    n = 0
    try:
        with open(path, "rb") as fh:
            for _ in fh:
                n += 1
    except OSError:
        return 0
    return n


def resolve_mobility_source(dataset_dir: str, probe: dict, *, prefer: str = "auto",
                            sumo_trace: str | None = None) -> dict:
    """Choose the traffic panel's input and SAY WHICH ONE IT IS.

    Returns ``{"source", "path", "rows", "truncated", "full_record", "note", "candidates"}``.
    ``truncated=True`` means the rows came from the broadcast stream and stop at revocation; the
    panel then publishes survivorship beside every metric and withholds the density-dependent ones
    below ``SURVIVORSHIP_MIN_FRAC``.

    ``prefer`` is ``auto`` (oracle, then trace, then emissions), or one of ``oracle`` / ``trace`` /
    ``emissions`` to force a source -- forcing one that is not available is an error the caller
    sees, not a silent fallback, because "which stream did this number come from" is exactly the
    question this whole mechanism exists to answer.

    **The trace is never picked up implicitly.** `manifest["config"]["sumo_trace"]` is an absolute
    path on whatever machine froze it, so honouring it under ``auto`` would make one dataset score
    differently on two hosts depending on whether that file happens to still be there -- a
    host-dependent scorecard, which breaks this module's determinism contract. It is therefore used
    only when the caller asks: ``sumo_trace=...`` names one directly, and ``prefer="trace"`` falls
    back to the manifest's pin.
    """
    orc = os.path.join(dataset_dir, MOBILITY_ORACLE_REL)
    emi = os.path.join(dataset_dir, EMISSIONS_REL)
    cfg_trace = str((probe.get("config") or {}).get("sumo_trace") or "")
    trace = sumo_trace or (cfg_trace if prefer == "trace" else "")
    cands = {"oracle": orc if os.path.isfile(orc) else None,
             "trace": trace if trace and os.path.isfile(trace) else None,
             "emissions": emi if os.path.isfile(emi) else None}
    order = ("oracle", "trace", "emissions") if prefer in ("auto", "") else (prefer,)
    for kind in order:
        p = cands.get(kind)
        if not p:
            continue
        if kind == "trace":
            rows = read_sumo_trace(p)
            note = ("frozen SUMO trace: the mobility the replay reproduces bit-exactly, and it "
                    "never met the SCMS layer")
        else:
            rows = _jsonl(p)
            note = ("un-enforced ORACLE mobility record: every active station, every step, "
                    "regardless of revocation" if kind == "oracle" else
                    "BROADCAST emission stream: TRUNCATED at revocation -- a traffic metric read "
                    "from it is low by the enforced fraction")
        return {"source": kind, "path": p, "rows": rows, "truncated": kind == "emissions",
                "full_record": kind != "emissions", "note": note,
                "candidates": {k: bool(v) for k, v in cands.items()}}
    if prefer not in ("auto", ""):
        raise FileNotFoundError(
            f"--traffic-source {prefer} requested but not available in {dataset_dir} "
            f"(available: {sorted(k for k, v in cands.items() if v) or 'none'})")
    return {"source": "none", "path": None, "rows": [], "truncated": True, "full_record": False,
            "note": "no mobility record found", "candidates": {k: bool(v) for k, v in cands.items()}}


def survivorship(dataset_dir: str, src: dict) -> dict:
    """How much of the SIMULATED mobility the panel's input actually contains.

    Three tiers, and the tier is always named in ``basis``:

      * ``full_record`` -- the source is the un-enforced oracle stream or the frozen trace, so the
        fraction is 1.0 by construction;
      * ``manifest_counts`` -- the engine's own step-loop tallies (exact);
      * ``unmeasurable`` -- an older dataset with neither. The revoked-vehicle fraction is still
        exact (it is two file lengths), but the vehicle-STEP fraction is not recoverable, and the
        obvious in-dataset estimator is measurably biased: never-revoked vehicles are systematically
        SHORT-trip vehicles, because exposure is what earns a false positive. Measured on the
        reference run, revoked vehicles average an 82.3 s simulated span against 68.2 s for the
        never-revoked; on the peak hour the naive estimate reads 0.71 against a true 0.4378. So this
        tier reports ``None`` and says why instead of publishing a number that flatters the dataset.
    """
    blk = _manifest_survivorship(dataset_dir) or {}
    n_veh = int(blk.get("vehicles") or 0) or _count_lines(os.path.join(dataset_dir, GT_VEHICLE_REL))
    n_rev = (int(blk["vehicles_revoked"]) if blk.get("vehicles_revoked") is not None
             else _count_lines(os.path.join(dataset_dir, LINKAGE_REVOCATION_REL)))
    out = {
        "mobility_source": src.get("source"),
        "vehicles": n_veh or None,
        "vehicles_revoked": n_rev or (0 if n_veh else None),
        "revoked_vehicle_frac": (round(n_rev / n_veh, 6) if n_veh else None),
        "vehicle_steps_simulated": blk.get("vehicle_steps_simulated"),
        "vehicle_steps_broadcast": blk.get("vehicle_steps_broadcast"),
        "vehicle_steps_surviving_enforcement": None,
        "vehicle_steps_survival_frac": None,
        "mean_record_span_s_revoked": blk.get("mean_record_span_s_revoked"),
        "mean_record_span_s_never_revoked": blk.get("mean_record_span_s_never_revoked"),
        "mean_simulated_span_s_revoked": blk.get("mean_simulated_span_s_revoked"),
        "mean_simulated_span_s_never_revoked": blk.get("mean_simulated_span_s_never_revoked"),
        "basis": None, "note": None,
    }
    rev, ok = out["mean_record_span_s_revoked"], out["mean_record_span_s_never_revoked"]
    out["record_span_truncation_ratio"] = (round(rev / ok, 6) if (rev and ok) else None)
    if src.get("full_record"):
        out["vehicle_steps_surviving_enforcement"] = len(src.get("rows") or []) or None
        out["vehicle_steps_survival_frac"] = 1.0
        out["basis"] = "full_record"
        out["note"] = ("the traffic panel reads the un-enforced record, so nothing enforcement did "
                       "is in these numbers; survivorship is 1.0 by construction")
    elif blk.get("vehicle_steps_survival_frac") is not None:
        out["vehicle_steps_surviving_enforcement"] = blk.get("vehicle_steps_broadcast")
        out["vehicle_steps_survival_frac"] = float(blk["vehicle_steps_survival_frac"])
        out["basis"] = "manifest_counts"
        out["note"] = ("broadcast vehicle-steps over SIMULATED vehicle-steps, tallied in the "
                       "engine's step loop (manifest.counts.mobility_survivorship)")
    else:
        out["basis"] = "unmeasurable"
        out["note"] = ("this dataset predates the survivorship tallies and carries no un-enforced "
                       "record, so the surviving vehicle-STEP fraction is not recoverable from it; "
                       "re-run with emit_mobility_oracle=true, or point --sumo-trace at the frozen "
                       "trace. The revoked-vehicle fraction beside it is still exact")
    return out


# ================================================================================================
# trajectory reconstruction
# ================================================================================================
def build_tracks(emissions: list[dict]) -> dict[str, dict]:
    """Per-vehicle TRUE trajectories, sorted by time, de-duplicated on (vehicle, t).

    Uses ``true_x``/``true_y`` -- the simulator's own state -- so falsified CAMs never distort a
    mobility metric (an attacker's car still drives like a car). Returns ``{vid: {t, x, y}}`` with
    numpy arrays.

    ADR 0002 adds ``true_speed`` / ``true_heading`` to the emission record. Where a vehicle carries
    EVERY one of its samples' values they are collected alongside as ``v`` / ``h_raw`` (raw, not yet
    unit- or convention-resolved -- that is ``ground_truth_kinematics``'s job). A track missing the
    field on any sample keeps neither, so a partially-populated field can never be silently
    interpolated. Older datasets simply carry no such key and take the differencing path.
    """
    per: dict[str, dict[float, tuple]] = {}
    for e in emissions:
        vid = e.get("true_vehicle_id")
        if vid is None:
            continue
        try:
            t = float(e["t"]); x = float(e["true_x"]); y = float(e["true_y"])
        except (KeyError, TypeError, ValueError):
            continue
        v = h = None
        try:
            if e.get(GT_SPEED_FIELD) is not None:
                v = float(e[GT_SPEED_FIELD])
        except (TypeError, ValueError):
            v = None
        try:
            if e.get(GT_HEADING_FIELD) is not None:
                h = float(e[GT_HEADING_FIELD])
        except (TypeError, ValueError):
            h = None
        per.setdefault(str(vid), {})[round(t, 6)] = (x, y, v, h)
    tracks: dict[str, dict] = {}
    for vid in sorted(per):
        ts = sorted(per[vid])
        if len(ts) < 2:
            continue
        rows = [per[vid][t] for t in ts]
        tr = {
            "t": np.array(ts, dtype=float),
            "x": np.array([r[0] for r in rows], dtype=float),
            "y": np.array([r[1] for r in rows], dtype=float),
        }
        if all(r[2] is not None and math.isfinite(r[2]) for r in rows):
            tr["v"] = np.array([r[2] for r in rows], dtype=float)
        if all(r[3] is not None and math.isfinite(r[3]) for r in rows):
            tr["h_raw"] = np.array([r[3] for r in rows], dtype=float)
        tracks[vid] = tr
    return tracks


# --- ADR 0002: resolving the ground-truth kinematic fields ---------------------------------------
# The harness does not own the emission schema (another change adds the fields), so it must not
# assume a unit or an angle convention. Each candidate below maps the RAW stored number onto the
# harness's internal convention -- radians, counter-clockwise, zero due east -- and the one that
# actually fits the observed chord bearings is selected and REPORTED. A field that fits none of
# them is rejected and the position-differencing path is used instead, which is the only safe
# default: a 90-degree convention error would silently rotate the whole lateral decomposition.
HEADING_CONVENTIONS = {
    "deg_ccw_from_east": lambda h: np.radians(h),                    # mock_pipeline (run.py:667)
    "deg_cw_from_north": lambda h: np.radians(90.0 - h),             # SUMO / ETSI CAM heading
    "rad_ccw_from_east": lambda h: np.asarray(h, dtype=float),
    "rad_cw_from_north": lambda h: (math.pi / 2.0) - np.asarray(h, dtype=float),
}


def ground_truth_kinematics(tracks: dict[str, dict], max_dt: float = MAX_FD_DT_S) -> dict:
    """Decide whether ``true_speed`` / ``true_heading`` can be trusted, and how to read the heading.

    Both fields are VALIDATED against the geometry the harness can already measure, over the steps
    where that geometry is meaningful (inside the finite-difference ceiling, at least 1 m of travel):

      * heading -- every candidate convention is scored by the median absolute angle between the
        step's mean stored heading and its chord bearing; the best is taken if it fits within
        ``GT_HEADING_MAX_RESIDUAL_DEG``;
      * speed -- accepted if the median |mean stored speed - chord speed| is within
        ``GT_SPEED_MAX_RESIDUAL_MPS`` (a km/h field, or a field in the wrong frame, fails this).

    A field is also required on EVERY track, not most of them: a half-migrated dataset would
    otherwise put first differences of a measured speed and second differences of position in the
    same panel, and one number would be an average of two estimators.

    Returns a machine-readable descriptor; ``used`` is what the panel branches on and every metric
    computed from the field copies ``source`` into its own ``details``.
    """
    n_tracks = len(tracks)
    n_with_v = sum(1 for tr in tracks.values() if "v" in tr)
    n_with_h = sum(1 for tr in tracks.values() if "h_raw" in tr)
    bear, chord_v, hh = [], [], []
    for vid in sorted(tracks):
        tr = tracks[vid]
        t, x, y = tr["t"], tr["x"], tr["y"]
        dt = np.diff(t)
        d = np.hypot(np.diff(x), np.diff(y))
        m = (dt > 0.0) & (dt <= float(max_dt)) & (d >= 1.0)
        if not m.any():
            continue
        b = np.arctan2(np.diff(y), np.diff(x))[m]
        if "v" in tr:
            chord_v.append(np.stack([(tr["v"][:-1] + tr["v"][1:])[m] * 0.5, (d / dt)[m]]))
        if "h_raw" in tr:
            hh.append(np.stack([tr["h_raw"][:-1][m], tr["h_raw"][1:][m], b]))
        bear.append(b)
    out = {
        "speed": {"field": GT_SPEED_FIELD, "present_tracks": n_with_v, "total_tracks": n_tracks,
                  "used": False, "source": "position finite difference"},
        "heading": {"field": GT_HEADING_FIELD, "present_tracks": n_with_h, "total_tracks": n_tracks,
                    "used": False, "convention": None, "source": "median-smoothed chord bearing"},
    }
    partial = ("only {n} of " + str(n_tracks) + " tracks carry {f} on every sample; a partial field "
               "would mix two estimators inside one panel")
    if not bear:
        out["speed"]["reason"] = out["heading"]["reason"] = "no usable moving steps to validate against"
        return out

    if not n_with_v:
        out["speed"]["reason"] = f"no track carries {GT_SPEED_FIELD} on every sample"
    elif n_with_v < n_tracks:
        out["speed"]["reason"] = partial.format(n=n_with_v, f=GT_SPEED_FIELD)
    elif chord_v:
        cv = np.concatenate(chord_v, axis=1)
        n = int(cv.shape[1])
        res = float(np.median(np.abs(cv[0] - cv[1]))) if n else float("inf")
        out["speed"].update({"validated_steps": n, "residual_vs_chord_speed_mps": _r(res, 4)})
        if n < GT_MIN_STEPS:
            out["speed"]["reason"] = f"only {n} validated steps (need {GT_MIN_STEPS})"
        elif not (res <= GT_SPEED_MAX_RESIDUAL_MPS):
            out["speed"]["reason"] = (f"median |true_speed - chord speed| = {res:.3f} m/s exceeds "
                                      f"{GT_SPEED_MAX_RESIDUAL_MPS:g} m/s (wrong unit or frame)")
        else:
            out["speed"].update({"used": True, "source": f"ground truth {GT_SPEED_FIELD}"})
    else:
        out["speed"]["reason"] = "no track carrying it has a usable moving step to validate against"

    if not n_with_h:
        out["heading"]["reason"] = f"no track carries {GT_HEADING_FIELD} on every sample"
    elif n_with_h < n_tracks:
        out["heading"]["reason"] = partial.format(n=n_with_h, f=GT_HEADING_FIELD)
    elif hh:
        ha = np.concatenate(hh, axis=1)
        n = int(ha.shape[1])
        scores = {}
        for name, conv in HEADING_CONVENTIONS.items():
            a, b = conv(ha[0]), conv(ha[1])
            mid = np.arctan2(np.sin(a) + np.sin(b), np.cos(a) + np.cos(b))
            scores[name] = float(np.degrees(np.median(np.abs(_ang_diff(mid, ha[2]))))) if n else 999.0
        best = min(sorted(scores), key=lambda k: scores[k])
        out["heading"].update({"validated_steps": n,
                               "convention_residuals_deg": {k: _r(v, 3) for k, v in
                                                            sorted(scores.items())}})
        if n < GT_MIN_STEPS:
            out["heading"]["reason"] = f"only {n} validated steps (need {GT_MIN_STEPS})"
        elif not (scores[best] <= GT_HEADING_MAX_RESIDUAL_DEG):
            out["heading"]["reason"] = (
                f"no heading convention fits the chord bearings (best {best} at "
                f"{scores[best]:.2f} deg > {GT_HEADING_MAX_RESIDUAL_DEG:g} deg)")
        else:
            out["heading"].update({"used": True, "convention": best,
                                   "residual_deg": _r(scores[best], 3),
                                   "source": f"ground truth {GT_HEADING_FIELD} ({best})"})
    else:
        out["heading"]["reason"] = "no track carrying it has a usable moving step to validate against"
    return out


def _track_gt(tr: dict, gt: dict | None) -> tuple[np.ndarray | None, np.ndarray | None]:
    """(true_speed, true_heading-in-radians-ccw-from-east) for one track, or (None, None) per field."""
    if not gt:
        return None, None
    v = tr.get("v") if (gt.get("speed", {}).get("used") and "v" in tr) else None
    h = None
    hc = gt.get("heading", {})
    if hc.get("used") and "h_raw" in tr:
        h = HEADING_CONVENTIONS[hc["convention"]](tr["h_raw"])
    return v, h


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
                     speed_bound_mps: float | None = None,
                     true_speed: np.ndarray | None = None,
                     true_heading: np.ndarray | None = None) -> dict:
    """Decompose ONE vehicle's samples into longitudinal / lateral motion and differentiate.

    ADR 0002 (ground-truth kinematics). When `true_speed` / `true_heading` are supplied -- already
    validated and unit-resolved by ``ground_truth_kinematics`` -- they REPLACE the corresponding
    reconstruction, and the estimator gets strictly simpler:

      * `true_heading` gives the direction of travel directly (the circular mean of the step's two
        endpoint headings), so the median smoothing, the "witness" search and the raw-heading
        stability test are all unnecessary: a lane change is a step whose heading did NOT turn, read
        off the heading itself instead of inferred from neighbouring chords;
      * `true_speed` turns acceleration into a FIRST difference of a measured quantity over one
        sampling interval, rather than a second difference of position. Nothing has to be screened
        out except a teleport (which the teleport gate already counts), because the artefacts the
        screen exists for -- lane-change teleports, backward remaps, the partial insertion/arrival
        steps at each end of a track -- are all artefacts of DIFFERENCING POSITION and cannot reach
        a speed the simulator reported itself. ``accel_pair_dt`` then carries the STEP gap `dt`
        rather than the midpoint separation, because the difference spans one step, not two.

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
        mistaken for a jump). ``lateral`` is its lane-width-scale subset (>= ``LATERAL_JUMP_M``)
        RESTRICTED TO ``lateral_scan``, which is what ``traffic.lateral_discontinuity_events``
        reports: a half-metre partial lane offset corrupts a 0.1 s difference just as badly, but it
        is not a lane change. ``lateral_scan`` marks the steps on which the count is even DEFINED --
        the step's own gap inside ``max_dt``, plus (when the heading is inferred) the
        ``HEADING_WINDOW_PAIRS`` steps either side that the inference reads. It is the same trust
        ceiling the acceleration series applies and it is not optional: across a 10 s sampling gap
        the "lateral" component is simply where the vehicle got to while unobserved. Measured on a
        0.02-sampled InTAS run, 473 of 535 flagged events sat on gaps wider than the ceiling (median
        gap 10.2 s, maximum "lateral offset" 891.7 m), and masking the step alone still left offsets
        of 31.4 m inside a <= 2 s gap because the median heading window reached across the 10 s gaps
        on either side. ``lateral_screen`` itself is left unmasked because its only consumers (the
        acceleration screen and the longitudinal projection) are already gated on the PAIR mask,
        which requires both of a pair's steps to be inside the ceiling;
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
                "lateral_scan": z,
                "accel": empty, "accel_pair_dt": empty, "accel_all": empty,
                "accel_all_pair_dt": empty, "sample_speed": np.asarray(true_speed, dtype=float)
                if true_speed is not None else empty,
                "n_excl": {"boundary": 0, "lateral": 0, "reversal": 0, "teleport": 0}}
    step_usable = (dt > 0.0) & (dt <= float(max_dt))
    tv = np.asarray(true_speed, dtype=float) if true_speed is not None else None
    th = np.asarray(true_heading, dtype=float) if true_heading is not None else None
    if th is not None:
        # the vehicle's own heading, averaged over the step: no smoothing, no witness search
        hs = np.arctan2(np.sin(th[:-1]) + np.sin(th[1:]), np.cos(th[:-1]) + np.cos(th[1:]))
    else:
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
    if th is not None:
        # the vehicle reports where it is pointing, so "did it turn?" needs no inference at all
        stable = np.abs(_ang_diff(th[1:], th[:-1])) <= tol
    else:
        ang_eff = np.where(dist >= HEADING_MIN_DISP_M, ang, hs)
        # The witnesses are the nearest steps either side that are not themselves suspect: a lane
        # change immediately followed by a second one would otherwise vouch for its own neighbour's
        # rogue heading and both would go uncounted (veh_115 in the InTAS run does exactly this,
        # 3.2 m one way then 6.4 m back inside 0.4 s). Witnesses are looked for at most
        # HEADING_WINDOW_PAIRS steps out.
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
    # Which steps the lateral count may be SCANNED on. The step itself must be inside the ceiling --
    # and, when the direction of travel is INFERRED, so must the +-window_pairs steps the inference
    # reads, because a "lateral offset" is only as meaningful as the heading it is measured across.
    # (Measured on a 0.02-sampled InTAS run: masking the step alone still left offsets up to 31.4 m
    # inside a <=2 s gap, because the median window reached across 10 s gaps either side of it.)
    # With a ground-truth heading there is no window and no inference, so the step's own gap is the
    # whole requirement.
    lateral_scan = step_usable.copy()
    if th is None:
        for s in range(1, max(1, int(HEADING_WINDOW_PAIRS)) + 1):
            lateral_scan[s:] &= step_usable[:-s]      # ... and at a track end, whatever exists
            lateral_scan[:-s] &= step_usable[s:]
    # the lane-width-scale REPORTED subset -- over the scannable steps ONLY (see above)
    lateral = jumped & (np.abs(d_lat) >= LATERAL_JUMP_M) & lateral_scan
    d_long = (proj if th is not None else
              np.where(lateral_screen, proj, np.where(proj >= 0.0, dist, -dist)))
    # same ceiling, same reason: 10 s later a vehicle can legitimately be 100 m "behind" itself
    reversal = (d_long < -REVERSAL_M) & step_usable
    teleport = ((mean_speed > float(speed_bound_mps))
                if (speed_bound_mps is not None and float(speed_bound_mps) > 0.0) else z.copy())
    boundary = z.copy()
    boundary[0] = True
    boundary[-1] = True
    with np.errstate(divide="ignore", invalid="ignore"):
        speed_long = (0.5 * (tv[:-1] + tv[1:]) if tv is not None else
                      np.where(pos_dt, d_long / np.where(pos_dt, dt, 1.0), np.nan))

    out = {"n": n, "dt": dt, "dist": dist, "heading": ang, "heading_smooth": hs,
           "d_long": d_long, "d_lat": d_lat, "speed_long": speed_long, "lateral_speed": lat_speed,
           "lateral": lateral, "lateral_screen": lateral_screen, "reversal": reversal,
           "teleport": teleport, "boundary": boundary, "lateral_scan": lateral_scan,
           "sample_speed": (tv if tv is not None else np.zeros(0, dtype=float))}
    if tv is not None:
        # ADR 0002: a FIRST difference of a measured speed over ONE step. Only the teleport screen
        # survives (its step is already a HARD failure of its own metric); the boundary / lateral /
        # reversal screens exist to protect a position double-difference and have nothing to do here.
        with np.errstate(divide="ignore", invalid="ignore"):
            acc = np.where(pos_dt, (tv[1:] - tv[:-1]) / np.where(pos_dt, dt, 1.0), np.nan)
        clean = step_usable & ~teleport
        out.update({"accel": acc[clean], "accel_pair_dt": dt[clean],
                    "accel_all": acc[step_usable], "accel_all_pair_dt": dt[step_usable],
                    "n_excl": {"boundary": 0, "lateral": 0, "reversal": 0,
                               "teleport": int((step_usable & teleport).sum())}})
        return out
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
                  "lateral_speed", "lateral", "lateral_screen", "reversal", "teleport", "boundary",
                  "lateral_scan")
_KIN_BOOL_COLS = ("lateral", "lateral_screen", "reversal", "teleport", "boundary", "lateral_scan")


def kinematics(tracks: dict[str, dict], max_dt: float = MAX_FD_DT_S,
               speed_bound_mps: float | None = None, gt: dict | None = None) -> dict:
    """``track_kinematics`` over every vehicle, concatenated in sorted-vehicle order.

    Adds the per-vehicle bookkeeping ``segment_table`` and the traffic panel need: which pairs are
    inside the finite-difference gap ceiling (``usable``), the owning vehicle index, and the totals
    the lateral-discontinuity rate is normalised by (path length -> vehicle-km).

    ``gt`` is the descriptor from ``ground_truth_kinematics``; when it accepts a field, every track
    that carries it is differentiated the ADR-0002 way and the rest fall back per track, so a
    dataset in which only some vehicles carry the field still scores (the descriptor records how
    many did). ``path_m`` is the vehicle-km denominator and covers ONLY steps inside the ceiling --
    the rate and its numerator must be measured over the same subset of the trace.
    """
    vids = sorted(tracks)
    cols: dict[str, list] = {c: [] for c in _KIN_PAIR_COLS}
    extra: dict[str, list] = {"vid_i": [], "t_end": [], "t_mid": [], "x_mid": [], "y_mid": [],
                              "x_end": [], "y_end": []}
    acc, acc_dt, acc_all, acc_all_dt, samp_v = [], [], [], [], []
    n_excl = {"boundary": 0, "lateral": 0, "reversal": 0, "teleport": 0}
    for i, vid in enumerate(vids):
        tr = tracks[vid]
        t, x, y = tr["t"], tr["x"], tr["y"]
        tv, th = _track_gt(tr, gt)
        k = track_kinematics(t, x, y, max_dt=max_dt, speed_bound_mps=speed_bound_mps,
                             true_speed=tv, true_heading=th)
        if not k["n"]:
            continue
        if k["sample_speed"].size:
            samp_v.append(k["sample_speed"])
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
                    "n_excl": n_excl, "path_m": 0.0, "path_m_all": 0.0, "sample_speed": e})
        return out
    cat = np.concatenate
    out = {c: cat(cols[c]) for c in _KIN_PAIR_COLS}
    out.update({k: cat(v) for k, v in extra.items()})
    out["n"] = int(out["dt"].size)
    out["vehicles"] = vids
    out["usable"] = (out["dt"] > 0.0) & (out["dt"] <= max_dt)
    out["sample_speed"] = cat(samp_v) if samp_v else np.zeros(0, dtype=float)
    # vehicle-km is distance driven ALONG the road, so a lane-change teleport's sideways component
    # must not pad the denominator of the rate it is being counted against -- and only the steps the
    # numerator can be counted on (``lateral_scan``) belong in the denominator at all.
    out["path_m"] = float(np.abs(out["d_long"][out["lateral_scan"]]).sum())
    out["path_m_all"] = float(np.abs(out["d_long"]).sum())
    out["accel"] = cat(acc) if acc else np.zeros(0)
    out["accel_pair_dt"] = cat(acc_dt) if acc_dt else np.zeros(0)
    out["accel_all"] = cat(acc_all) if acc_all else np.zeros(0)
    out["accel_all_pair_dt"] = cat(acc_all_dt) if acc_all_dt else np.zeros(0)
    out["n_excl"] = n_excl
    return out


def segment_table(tracks: dict[str, dict], max_dt: float = MAX_FD_DT_S,
                  kin: dict | None = None, gt: dict | None = None) -> dict:
    """Finite-difference segments between consecutive samples of the same vehicle.

    Returns column arrays over every usable segment (0 < dt <= max_dt): the segment mid-point in
    space and time, the traversed distance, the elapsed time, the mean speed, the heading, and the
    owning vehicle. ``n_dropped_gap`` counts segments skipped because the sampling gap was too long
    to finite-difference honestly (the dominant effect when ``emit_sample_prob`` < 1).

    ``dist``/``speed``/``heading`` stay the RAW chord quantities (Edie's generalised definitions want
    distance actually travelled, and the leader search wants the geometric step direction);
    ``speed_long``/``heading_smooth``/``d_lat`` carry the decomposition from ``track_kinematics``.
    """
    k = kin if kin is not None else kinematics(tracks, max_dt, gt=gt)
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


def accelerations(tracks: dict[str, dict], max_dt: float = MAX_FD_DT_S,
                  gt: dict | None = None) -> np.ndarray:
    """LONGITUDINAL accelerations over consecutive steps of one vehicle.

    Second finite difference of the longitudinal speed (see ``track_kinematics``), screened of the
    step classes that are position DISCONTINUITIES rather than dynamics -- lane-change teleports,
    backward position remaps, teleports and the two partial steps at each end of a track. Every
    screened class is counted and published by its own scorecard metric, and the unscreened series
    is available as ``kinematics(tracks)["accel_all"]``.

    With a ``gt`` descriptor that accepts ``true_speed`` this is instead a first difference of the
    simulator's own speed and nothing is screened but teleports (ADR 0002).
    """
    return kinematics(tracks, max_dt, gt=gt)["accel"]


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
                  regime: str | None = None, gt: dict | None = None,
                  src: dict | None = None, surv: dict | None = None) -> list[dict]:
    P = "traffic"
    out: list[dict] = []
    # WHICH STREAM THESE NUMBERS CAME FROM. `emissions` is whatever `resolve_mobility_source`
    # picked -- the un-enforced oracle record, a frozen SUMO trace, or (marked truncated) the
    # broadcast stream. A caller that passes neither gets the historical behaviour, which is the
    # truncated one, and it is LABELLED as such rather than assumed to be traffic.
    src = src or {"source": "emissions", "truncated": True, "full_record": False,
                  "note": "caller supplied rows directly; provenance unknown, assumed broadcast"}
    surv = surv or {"basis": "unmeasurable", "vehicle_steps_survival_frac": None,
                    "revoked_vehicle_frac": None}
    # A complete record is a complete record whatever `emit_sample_prob` was: the oracle stream and
    # the trace are written per STEP, not per sampled message, so the sub-sampling caveat that
    # gates headways and the fundamental diagram does not apply to them.
    probe = dict(probe)
    if src.get("full_record"):
        probe["full_trace"] = True
    surv_frac = surv.get("vehicle_steps_survival_frac")
    # The one place the degradation rule lives. A density-dependent metric read from a stream that
    # lost more than 1 - SURVIVORSHIP_MIN_FRAC of the traffic is not a measurement of the traffic;
    # publishing it as `na` with the measured fraction is strictly more informative than publishing
    # a confidently wrong number, which is what this panel did before.
    rev_frac = surv.get("revoked_vehicle_frac")
    if not src.get("truncated"):
        surv_na = None
    elif surv_frac is not None:
        surv_na = (
            f"the traffic panel's input is {src['source']}, which kept {surv_frac:.4f} of the "
            f"simulated vehicle-steps (below the {SURVIVORSHIP_MIN_FRAC:g} floor): a revoked "
            f"vehicle stops broadcasting, so this metric would be low by the enforced fraction. "
            f"Re-run with emit_mobility_oracle=true, or pass --sumo-trace"
            if surv_frac < SURVIVORSHIP_MIN_FRAC else None)
    elif rev_frac is not None and (1.0 - rev_frac) < SURVIVORSHIP_MIN_FRAC:
        # A LEGACY dataset: truncated input, and it carries neither the un-enforced record nor the
        # step-loop tallies, so the exact surviving fraction is gone. What it does carry is the
        # revoked-vehicle fraction, and `1 - revoked` is the standing-in PROXY -- the survivorship a
        # run would have if every revoked vehicle were silenced the instant it spawned. It is not a
        # bound in either direction (measured: it reads 0.386 against a true 0.438 on the InTAS peak
        # hour, and 0.919 against 0.908 on the 300 s replay), so it is used ONLY to decide whether
        # to withhold, never published as a number. Withholding on it is the conservative choice and
        # the whole point of the finding: a legacy peak-hour dataset must not go on quietly
        # publishing fd_capacity 763.6 when the traffic says 1130.0.
        surv_na = (
            f"the traffic panel's input is {src['source']} and its surviving vehicle-step fraction "
            f"is NOT RECOVERABLE from this dataset. What is known is that the misbehaviour "
            f"authority revoked {rev_frac:.4f} of the vehicles, each of which stops broadcasting "
            f"at that instant, putting the 1-revoked proxy at {1.0 - rev_frac:.4f} -- below the "
            f"{SURVIVORSHIP_MIN_FRAC:g} floor. The proxy is an ORDER OF MAGNITUDE CHECK, not a "
            f"bound (it reads 0.386 against a true 0.438 on the InTAS peak hour), and it is used "
            f"only to withhold, never published. Re-run with emit_mobility_oracle=true, or pass "
            f"--sumo-trace, to measure this instead of withholding it")
    else:
        surv_na = None
    src_detail = {"mobility_source": src.get("source"),
                  "mobility_source_truncated": bool(src.get("truncated")),
                  "vehicle_steps_survival_frac": surv_frac,
                  "survivorship_basis": surv.get("basis")}
    _src_name = src.get("path") or EMISSIONS_REL

    # ---- survivorship, as metrics rather than as a footnote --------------------------------------
    out.append(_metric(
        "traffic.survivorship_vehicle_steps_frac", P,
        # ENFORCEMENT only. `emit_sample_prob` sub-sampling is a separate, already-handled loss
        # (`probe["full_trace"]` / the `thin` reason), and folding the two together would
        # double-count one of them and blur which mechanism a low number is reporting.
        "Vehicle-steps surviving SCMS enforcement, over vehicle-steps simulated",
        surv_frac, "fraction", surv.get("vehicle_steps_simulated"),
        ({"ref_id": "internal.mobility_survivorship_min", "unit": "fraction",
          "min": SURVIVORSHIP_MIN_FRAC, "confidence": "engine invariant",
          "cite": "internal integrity gate: SCMS enforcement truncates gt_emissions_sample.jsonl "
                  "at revocation (docs/realism/TRAFFIC-PANEL-SURVIVORSHIP.md); a traffic metric "
                  "read below this fraction measures enforcement, not traffic",
          "source": "measured in the engine's step loop (manifest.counts.mobility_survivorship)"}
         if surv_frac is not None else None),
        SOFT,
        reason=(None if surv_frac is not None else surv.get("note")),
        extra={**src_detail, **{k: v for k, v in surv.items()
                                if k not in ("basis", "note", "mobility_source")},
               "survivorship_note": surv.get("note")}))
    out.append(_metric(
        "traffic.revoked_vehicle_frac", P,
        "Vehicles the misbehaviour authority revoked (each stops broadcasting at that instant)",
        surv.get("revoked_vehicle_frac"), "fraction", surv.get("vehicles"), None, SOFT,
        reason=("informational: the CAUSE of any truncation above. It is not a realism defect -- "
                "the SCMS layer is working as configured -- but with detection precision below 1 "
                "most revocations are BENIGN vehicles, so it deletes traffic rather than attackers"
                if surv.get("revoked_vehicle_frac") is not None else
                "gt_linkage_revocation.jsonl / gt_vehicle.jsonl missing"),
        extra={"vehicles_revoked": surv.get("vehicles_revoked"), "vehicles": surv.get("vehicles"),
               "mean_record_span_s_revoked": surv.get("mean_record_span_s_revoked"),
               "mean_record_span_s_never_revoked": surv.get("mean_record_span_s_never_revoked"),
               "record_span_truncation_ratio": surv.get("record_span_truncation_ratio"),
               "span_note": "record span is what the BROADCAST stream kept; the simulated spans "
                            "beside it show revoked vehicles are the LONGER-TRIP ones, which is "
                            "why the loss cannot be estimated from the surviving records",
               "mean_simulated_span_s_revoked": surv.get("mean_simulated_span_s_revoked"),
               "mean_simulated_span_s_never_revoked":
                   surv.get("mean_simulated_span_s_never_revoked")}))

    # No early return on empty input: every metric below degrades to "na" with its own reason, so the
    # panel always has the SAME shape and a consumer can index it by metric id unconditionally.
    tracks = build_tracks(emissions)
    # ADR 0002: use the simulator's own speed/heading where the record carries them, and record per
    # metric which path produced it, so a scorecard can never be compared across the two silently.
    gt = ground_truth_kinematics(tracks) if gt is None else gt
    src_v = gt["speed"]["source"]
    src_h = gt["heading"]["source"]
    # the teleport bound is needed BEFORE the decomposition: a step that the teleport gate already
    # counts must not be counted a second time as an acceleration failure.
    sp_ref = _ref(refdata, "kinematics.speed_hard_bound_mps")
    tele_lim = float((sp_ref or {}).get("range", [0.0, 60.0])[1])
    kin = kinematics(tracks, MAX_FD_DT_S, speed_bound_mps=tele_lim, gt=gt)
    segs = segment_table(tracks, MAX_FD_DT_S, kin=kin)
    n_seg = int(segs.get("n", 0))
    out.append(_metric(
        "traffic.trace_segments", P, "Usable trajectory segments (finite-difference pairs)",
        n_seg, "count", n_seg, severity=SOFT,
        reason=(f"{_src_name} is missing or empty" if not emissions
                else "informational: sample size for every other traffic metric"),
        extra={"vehicles_with_track": len(tracks),
               "segments_dropped_sampling_gap": int(segs.get("n_dropped_gap", 0)),
               "max_finite_difference_dt_s": MAX_FD_DT_S,
               "emit_sample_prob": probe["emit_sample_prob"],
               "kinematics_source": gt}))

    thin = None if probe["full_trace"] else (
        f"emissions are a {probe['emit_sample_prob']:.3g} sample (emit_sample_prob < "
        f"{FULL_TRACE_MIN_PROB}); per-vehicle trajectories are lossy -- rerun with "
        f"emit_sample_prob=1.0 (python) / SCMS_EMIT_SAMPLE=1.0 (MOSAIC)")
    few = (None if n_seg >= MIN_SAMPLES else
           (f"{_src_name} is missing or empty" if not emissions
            else f"only {n_seg} usable segments (need {MIN_SAMPLES})"))

    # ---- speeds -------------------------------------------------------------------------------
    # LONGITUDINAL speed: on a lane-change step the raw chord divides a whole lane width by one
    # sample interval, which at dt=0.1 s reads back as ~33 m/s of forward motion that never happened.
    # ... unless the record carries the simulator's own speed, in which case no reconstruction of any
    # kind is involved and the sample is every emitted sample rather than every usable step (ADR 0002).
    gt_speed = bool(gt["speed"]["used"]) and kin.get("sample_speed", np.zeros(0)).size > 0
    spd = kin["sample_speed"] if gt_speed else (segs["speed_long"] if n_seg else np.zeros(0))
    n_spd = int(spd.size) if gt_speed else n_seg
    spd_few = (None if n_spd >= MIN_SAMPLES else few or
               f"only {n_spd} true-speed samples (need {MIN_SAMPLES})")
    spd_src = ("ground truth true_speed, one value per emitted sample" if gt_speed else
               "longitudinal component of the true_x/true_y finite difference")
    reg = regime or probe.get("regime")
    reg_reason = None if reg else ("traffic regime unknown for this dataset (no road_network in the "
                                   "manifest); pass --regime urban|highway to score speed bands")
    for q, label in ((50, "p50"), (95, "p95")):
        ref = _ref(refdata, f"traffic_regimes.{reg}.speed_{label}_mps") if reg else None
        out.append(_metric(
            f"traffic.speed_{label}_mps", P, f"Benign true speed {label} ({reg or 'regime unknown'})",
            _pct(spd, q), "m/s", n_spd, ref, SOFT,
            reason=spd_few or reg_reason,
            extra={"regime": reg, "kinematics_source": src_v, "source_field": spd_src}))
    out.append(_metric(
        "traffic.speed_max_mps", P,
        "Maximum true speed" if gt_speed else "Maximum finite-difference speed",
        float(spd.max()) if spd.size else None, "m/s", n_spd,
        _ref(refdata, "kinematics.speed_hard_bound_mps"), SOFT, reason=spd_few,
        extra={"speed_max_raw_chord_mps": _r(float(segs["speed"].max()) if n_seg else None),
               "kinematics_source": src_v, "source_field": spd_src,
               "note": ("value is the simulator's own speed; the raw chord speed is shown for "
                        "comparison" if gt_speed else
                        "value is longitudinal; the raw chord speed is shown for comparison")}))

    # ---- lateral position continuity -------------------------------------------------------------
    # This is the metric that EXPOSES the artefact the acceleration screen removes, so it comes
    # first: without a lane-change teleport counter, screening those steps out of the acceleration
    # series would delete the evidence instead of relocating it.
    # A lane change is a claim about ONE sampling interval, so -- exactly like the acceleration
    # series -- both the count and the vehicle-km it is divided by are taken over the steps inside
    # the finite-difference ceiling and nowhere else. Without that mask this metric is invalid at
    # any emit_sample_prob < 1: see the docstring of track_kinematics for the measured overstatement.
    n_pair_all = int(kin.get("n", 0))
    use = kin["lateral_scan"] if n_pair_all else np.zeros(0, dtype=bool)
    n_pair = int(use.sum())
    v_km = float(kin.get("path_m", 0.0)) / 1000.0
    n_lat = int(kin["lateral"].sum()) if n_pair_all else 0     # already restricted to scannable steps
    d_lat_abs = np.abs(kin["d_lat"][use]) if n_pair_all else np.zeros(0)
    n_rev = int((kin["reversal"] & use).sum()) if n_pair_all else 0
    lat_few = (None if (n_pair >= MIN_SAMPLES and v_km > 0.0) else
               (f"{_src_name} is missing or empty" if not emissions
                else f"only {n_pair} of {n_pair_all} consecutive-sample steps can be scanned for a "
                     f"lane-change teleport (need {MIN_SAMPLES}): the step and the heading window "
                     f"it is measured against must all sit inside the {MAX_FD_DT_S:g} s "
                     f"finite-difference ceiling. At emit_sample_prob="
                     f"{probe['emit_sample_prob']:.3g} a wider gap measures where the vehicle GOT "
                     "TO unobserved, not a lane change"))
    out.append(_metric(
        "traffic.lateral_discontinuity_events", P,
        "Lane-change teleports (a lane-width sideways step inside one sample), per vehicle-km",
        (n_lat / v_km if lat_few is None else None), "events/vehicle-km", n_pair,
        _ref(refdata, "kinematics.lateral_discontinuity_per_vehicle_km_max"), SOFT, reason=lat_few,
        extra={
            "events": n_lat,
            "events_per_1000_sample_pairs": _r(1000.0 * n_lat / n_pair if n_pair else None),
            "vehicle_km": _r(v_km, 3), "sample_pairs": n_pair,
            "kinematics_source": src_h,
            "sample_pairs_total": n_pair_all,
            "sample_pairs_dropped_sampling_gap": n_pair_all - n_pair,
            "sample_pairs_inside_gap_ceiling": (int(kin["usable"].sum()) if n_pair_all else 0),
            "max_finite_difference_dt_s": MAX_FD_DT_S,
            "gap_ceiling_note": "events, vehicle-km and every diagnostic below are measured over "
                                f"the {n_pair} scannable steps only. A step is scannable when its "
                                f"own gap -- and, where the heading is inferred, the "
                                f"+-{HEADING_WINDOW_PAIRS}-step window it is measured against -- "
                                f"sit inside {MAX_FD_DT_S:g} s. A wider gap carries no lane-change "
                                "information at all: its lateral component is where the vehicle "
                                "drove while unobserved. Counting those steps put 473 of 535 "
                                "'events' on gaps with a 10.2 s median and a 891.7 m maximum "
                                "lateral offset on a 0.02-sampled InTAS run",
            "vehicle_km_note": "longitudinal path length over the same scannable steps, so a "
                               "jump's sideways component does not pad the denominator of the rate "
                               "it is counted against, and neither does distance covered across a "
                               "gap on which no event could have been counted",
            "vehicle_km_all_pairs": _r(float(kin.get("path_m_all", 0.0)) / 1000.0, 3),
            "sampling_validity_note": "the gate is MEASURED (how many steps are scannable), not "
                                      "declared from the manifest's emit_sample_prob: a manifest "
                                      "may be absent or wrong, a thinned-but-still-dense trace is "
                                      "legitimately scoreable, and a nominally full trace with a "
                                      "few holes in it is not scoreable across those holes. The "
                                      "rate stays comparable across sampling rates because "
                                      "numerator and denominator cover the same steps",
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
            "max_lateral_step_m": _r(float(d_lat_abs.max()) if d_lat_abs.size else None),
            "sub_lane_screened_steps": (int((kin["lateral_screen"] & use).sum()) - n_lat
                                        if n_pair_all else 0),
            "sub_lane_screen_threshold_m": LATERAL_SCREEN_M,
            "longitudinal_reversal_events": n_rev,
            "longitudinal_reversal_per_vehicle_km": _r(
                (n_rev / v_km) if (n_pair and v_km > 0) else None),
            "method": ("per consecutive-sample step, the displacement component ACROSS the "
                       + (f"vehicle's own {GT_HEADING_FIELD} (ground truth, no reconstruction). "
                          if gt["heading"]["used"] else
                          f"smoothed direction of travel (circular median over "
                          f"+-{HEADING_WINDOW_PAIRS} steps). ")
                       + f"A step counts when it is scannable (gap <= {MAX_FD_DT_S:g} s"
                       + ("" if gt["heading"]["used"] else
                          f", and so are the +-{HEADING_WINDOW_PAIRS} steps the heading is inferred "
                          "from") + ") AND it moves >= "
                       f"{LATERAL_JUMP_M:g} m sideways at more than {LATERAL_SPEED_MAX_MPS:g} m/s "
                       "AND the "
                       + ("vehicle's own heading did not turn"
                          if gt["heading"]["used"] else "RAW headings of the steps either side agree")
                       + f" within {HEADING_STABLE_TOL_DEG:g} deg -- a lane change does not turn "
                       "the vehicle, a corner does, so cornering is not counted. "
                       + ("" if gt["heading"]["used"] else
                          "A track's first and last step are not scanned (no evidence on one "
                          "side). ")
                       + "Normalised per vehicle-km of path, which is invariant to the CAM trigger "
                       "rate; the per-1000-step rate is given alongside and is not"),
            "sub_lane_note": f"steps with a non-physical lateral speed but under {LATERAL_JUMP_M:g} m "
                             f"of offset (>= {LATERAL_SCREEN_M:g} m) are NOT counted in the headline "
                             "rate -- they are partial lane offsets, not lane changes -- but they "
                             "are screened out of the acceleration series for the same reason",
            "longitudinal_reversal_note": "the same discrete lane/edge position remapping seen "
                                          "along the road instead of across it: the next sample "
                                          f"lands more than {REVERSAL_M:g} m BEHIND this one"}))

    # ---- accelerations ------------------------------------------------------------------------
    acc = kin["accel"] if n_pair_all else np.zeros(0)
    acc_all = kin["accel_all"] if n_pair_all else np.zeros(0)
    acc_dt = kin["accel_pair_dt"] if n_pair_all else np.zeros(0)
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
               "kinematics_source": src_v,
               "method": (f"FIRST difference of the ground-truth {GT_SPEED_FIELD} over one sampling "
                          f"interval (dt <= {MAX_FD_DT_S:g} s); no position differencing is involved"
                          if gt["speed"]["used"] else
                          "second difference of the LONGITUDINAL speed (displacement projected on "
                          f"the direction of travel, smoothed over +-{HEADING_WINDOW_PAIRS} steps), "
                          "over the exact midpoint separation (dt_i + dt_i+1)/2"),
               "screened_out": dict(kin.get("n_excl", {})),
               "screened_note": ("only teleports are screened: with a measured speed there is no "
                                 "position double-difference for a lane change, a backward remap or "
                                 "a track's partial first/last step to corrupt, and a teleport step "
                                 "is already a HARD failure of traffic.teleport_events"
                                 if gt["speed"]["used"] else
                                 "position DISCONTINUITIES are not accelerations: lane-change "
                                 "teleports go to traffic.lateral_discontinuity_events, teleports to "
                                 "traffic.teleport_events, and a track's first/last step is a "
                                 "partial insertion/arrival step, not a second of driving. "
                                 f"screened_out.lateral uses the lower {LATERAL_SCREEN_M:g} m "
                                 "sub-lane threshold, so it exceeds the reported event count"),
               "samples_before_screening": int(acc_all.size),
               "unscreened_frac_within_band": _r(
                   float(np.mean((acc_all >= lo) & (acc_all <= hi))) if acc_all.size else None, 6),
               "unscreened_accel_min": _r(float(acc_all.min()) if acc_all.size else None),
               "unscreened_accel_max": _r(float(acc_all.max()) if acc_all.size else None),
               "unscreened_note": ("the same estimator with the teleport screen switched off"
                                   if gt["speed"]["used"] else
                                   "the SAME longitudinal estimator with the discontinuity screen "
                                   "switched off (not the pre-fix raw-chord estimator)"),
               "pair_dt_meaning": ("the sampling interval the speed difference spans"
                                   if gt["speed"]["used"] else
                                   "the midpoint separation (dt_i + dt_i+1)/2 of the second "
                                   "difference"),
               "by_pair_dt_s": by_dt or None,
               "sampling_note": "each acceleration sample carries equal weight; the by_pair_dt_s "
                                "breakdown is what makes an interval-dependent artefact visible"}))
    out.append(_metric(
        "traffic.accel_within_comfort_frac", P,
        "Accelerations inside the comfort band (+/-3 m/s^2)", frac_comf, "fraction", n_acc,
        _ref(refdata, "kinematics.accel_comfort_min_fraction"), SOFT, reason=acc_few,
        extra={"band_mps2": list((comf_ref or {}).get("range", [])) or None,
               "kinematics_source": src_v}))

    # ---- sim health: teleports + overlaps + liveness ---------------------------------------------
    n_tele, n_tele_pairs = teleport_events(tracks, tele_lim)
    # Sample floor, like accel (MIN_SAMPLES) and overlap (10 instants): with none, a dataset whose
    # every real jump lands across a dropped sampling gap reported "0 teleports, pass" off ONE pair.
    tele_few = (None if n_tele_pairs >= MIN_SAMPLES else
                (f"{_src_name} is missing or empty" if not emissions
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
                (f"{_src_name} is missing or empty" if not emissions
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

    # ---- provenance on every row, and WITHHOLDING of what the truncation invalidates -------------
    # Two separate obligations, both from the same finding. (1) A traffic number is only
    # interpretable next to the stream it came from, so every row carries its source and the
    # surviving fraction -- no consumer of this scorecard can now read a flow, a headway or a
    # fundamental diagram without seeing whether the vehicles were there. (2) Below the floor the
    # DENSITY-DEPENDENT metrics are not published at all: a missing vehicle removes a leader, a
    # follower, a cell occupancy or an overlap partner, so those numbers would be low by a factor
    # that grows with the run length. `na` plus the measured fraction is the honest output.
    for m in out:
        if m["id"] in ("traffic.survivorship_vehicle_steps_frac", "traffic.revoked_vehicle_frac"):
            continue
        m.setdefault("details", {}).update(src_detail)
        if not (surv_na and m["id"] in SURVIVORSHIP_GATED_METRICS):
            continue
        if m["id"] in SURVIVORSHIP_LOWER_BOUND_METRICS and m["status"] == "fail":
            # A one-sided count that already breaches its bound: keep the FAIL, label the number.
            m["details"]["survivorship_note"] = (
                "LOWER BOUND: the missing vehicle-steps can only ADD co-presence events, so the "
                "true count is at least this. Reported as a failure rather than withheld, because "
                "more traffic cannot rescue it")
            continue
        # Withheld regardless of the row's PREVIOUS status: several of these are permanently `na`
        # because they carry no gate (`headway_p50_s` is "informational"), and a consumer reads
        # their VALUE anyway. A wrong value published under an `na` status is still a wrong value,
        # so the number goes too -- with the original reason kept behind it.
        was = m.get("reason")
        m["status"] = "na"
        m["value"] = None
        m["reason"] = surv_na + (f" (previously: {was})" if was else "")
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
               max_dist_m: float = MAX_LINK_DIST_M, regime: str | None = None,
               dataset_dir: str | None = None) -> list[dict]:
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
    # THE 0.90 ANCHOR IS NOT ATTACHED HERE ANY MORE, and that is a correctness fix, not a
    # relaxation. `v2x_awareness.awareness_ratio_200m_urban_min` = 0.90 comes from Boban & d'Orey's
    # Neighbourhood Awareness Ratio, which (refdata/v2x_awareness_conditions.json, transcribed from
    # the full text) is a ">= 1 message received in a 1 s window at 10 Hz CAM" metric over a 3-9
    # vehicle instrumented test fleet on a shared route, at a link budget of ~110 dB. This row is a
    # single-shot, all-pairs, city-wide ratio NORMALISED by an unknown constant. Grading it at 0.90
    # is the same class of error as grading a simulation against itself: the number is real, the
    # comparison is not like-for-like. `comm.nar90_equivalent_range_m` below is the restatement that
    # IS comparable; this row stays, unchanged and ungated, as the raw observable it always was.
    for anchor in AWARENESS_ANCHORS_M:
        val, n_at = (_curve_at(curve, anchor) if curve is not None else (None, 0))
        out.append(_metric(
            f"comm.awareness_ratio_{int(anchor)}m", P,
            f"Neighbour awareness ratio at {int(anchor)} m (normalised, all pairs)",
            val, "fraction", n_at, None, SOFT,
            reason=few or (
                "UNGATED SINCE 2026-09-01 and deliberately so. This is an ALL-PAIRS, SINGLE-SHOT "
                "ratio normalised at the near band; the 0.90 anchor it used to be graded against "
                "is a >=1-of-Z per-second NAR over a 3-9 vehicle test fleet at a ~110 dB link "
                "budget (v2x_awareness_conditions.nar_definition / nar_pair_population_measured / "
                "nar_shot_multiplicity_z). See comm.nar90_equivalent_range_m for the comparable "
                "restatement and comm.link_state_los_fraction_* for the quantity that explains "
                "this number."),
            extra={"regime": reg, "reconstruction_method": method,
                   "normalization": "ratio of observed honest links to co-presence opportunities, "
                                    "divided by the same ratio in the nearest populated bin",
                   "retired_reference": "v2x_awareness.awareness_ratio_200m_urban_min",
                   "retired_because": "conditions mismatch on pair population, shot multiplicity "
                                      "and link budget; see refdata/v2x_awareness_conditions.json"}))

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

    # ---- like-for-like awareness (datagen/awareness.py) ------------------------------------------
    # Needs the scenario GEOMETRY (building polygons, vehicle types), which lives in the dataset
    # directory rather than in the records this function is handed, so it is opt-in on that path
    # being supplied. Never allowed to break the scorecard: a geometry failure degrades to one `na`
    # row carrying the exception text, exactly like every other absent signal here.
    if dataset_dir:
        try:
            from . import awareness as _aw
            rep = _aw.awareness_report(dataset_dir, refdata_dir=refdata.get("dir"),
                                       bin_m=dist_bin_m, max_dist_m=max_dist_m)
            out.extend(_aw.panel_rows(rep, _metric, lambda k: _ref(refdata, k)))
        except Exception as exc:                       # noqa: BLE001 - reported, never raised
            out.append(_metric(
                "comm.link_state_los_fraction", P,
                "LOS fraction of the co-present pair population (all bands)",
                None, "fraction", None, None, SOFT,
                reason=f"link-state classification unavailable: {type(exc).__name__}: {exc}"))
    return out


# ================================================================================================
# scorecard
# ================================================================================================
def scorecard(dataset_dir: str, refdata: dict | str | None = None, *, regime: str = "auto",
              art_max_m: float | None = None, radio_range_m: float | None = None,
              t_bucket_s: float = T_BUCKET_S, dist_bin_m: float = DIST_BIN_M,
              max_dist_m: float = MAX_LINK_DIST_M, fd_cell_m: float = FD_CELL_M,
              fd_window_s: float = FD_WINDOW_S, traffic_source: str = "auto",
              sumo_trace: str | None = None) -> dict:
    """Score one dataset directory. Read-only, deterministic, never raises on a missing signal.

    Raises FileNotFoundError only when `dataset_dir` itself is not a directory, or when
    `traffic_source` names a source this dataset does not have.

    The TRAFFIC panel and the COMM panel read DIFFERENT streams on purpose. Traffic takes the best
    un-enforced mobility record available (see `resolve_mobility_source`); comm takes the broadcast
    emissions, because what the MA could actually hear is the honest input to a reception
    measurement. Reading traffic off the broadcast stream is the bias this split exists to remove.
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
    gt = ground_truth_kinematics(tracks)

    # THE TRAFFIC PANEL'S OWN INPUT, resolved and named. When it resolves to the emission stream
    # this is the same object the comm panel uses, so nothing is read twice.
    src = resolve_mobility_source(dataset_dir, probe, prefer=traffic_source, sumo_trace=sumo_trace)
    surv = survivorship(dataset_dir, src)
    if src["source"] == "emissions":
        src["rows"] = emissions
        traffic_rows, traffic_gt = emissions, gt
    else:
        traffic_rows = src["rows"]
        traffic_gt = ground_truth_kinematics(build_tracks(traffic_rows))

    traffic = traffic_panel(traffic_rows, probe, rd, fd_cell_m=fd_cell_m, fd_window_s=fd_window_s,
                            regime=reg, gt=traffic_gt, src=src, surv=surv)
    comm = comm_panel(emissions, reports, rlabels, tracks, probe, rd, t_bucket_s=t_bucket_s,
                      dist_bin_m=dist_bin_m, max_dist_m=max_dist_m, regime=reg,
                      dataset_dir=dataset_dir)
    metrics = traffic + comm
    counts = {"pass": 0, "fail": 0, "na": 0}
    for m in metrics:
        counts[m["status"]] = counts.get(m["status"], 0) + 1
    hard_fail = [m["id"] for m in metrics if m["status"] == "fail" and m["severity"] == HARD]
    soft_fail = [m["id"] for m in metrics if m["status"] == "fail" and m["severity"] == SOFT]
    return {
        "dataset_dir": dataset_dir,
        "probe": probe,
        # ADR 0002: which estimator produced the kinematic metrics on THIS dataset. Every affected
        # metric repeats it in its own ``details.kinematics_source`` so a single row is self-auditing.
        "kinematics_source": traffic_gt,
        # WHICH STREAM THE TRAFFIC PANEL READ, and how much of the traffic it kept. Top-level
        # rather than buried per metric, because "is this a traffic sample at all" precedes every
        # question the panel answers. `rows` is dropped: the scorecard is aggregates only.
        "traffic_source": {k: v for k, v in src.items() if k != "rows"},
        "survivorship": surv,
        "refdata": {"dir": rd.get("dir"), "sets": sorted(rd.get("sets", {})),
                    "n_entries": len(rd.get("entries", {}))},
        "settings": {"regime": reg or "auto", "t_bucket_s": t_bucket_s, "dist_bin_m": dist_bin_m,
                     "max_dist_m": max_dist_m, "fd_cell_m": fd_cell_m, "fd_window_s": fd_window_s,
                     "max_finite_difference_dt_s": MAX_FD_DT_S, "min_samples": MIN_SAMPLES,
                     "traffic_source": traffic_source,
                     "survivorship_min_frac": SURVIVORSHIP_MIN_FRAC},
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


def source_line(card: dict) -> str:
    """One line naming WHICH STREAM the traffic panel read and how much of the mobility survived.

    The JSON carries `traffic_source` and `survivorship` as blocks and every traffic row repeats
    them in `details`, but the compact rendering is what a datasheet embeds -- and with
    `include_na=False` the two survivorship METRICS are themselves `na` on exactly the datasets
    whose numbers most need the caveat (a legacy dataset carries no tallies, so
    `survivorship_vehicle_steps_frac` has no value to print). Without this line such a scorecard
    shows speeds and accelerations off an enforcement-truncated stream and says nothing about it.
    """
    src = card.get("traffic_source") or {}
    surv = card.get("survivorship") or {}
    kind = src.get("source", "?")
    frac = surv.get("vehicle_steps_survival_frac")
    if frac is not None:
        state = f"survivorship {frac:.4f}"
    else:
        rv = surv.get("revoked_vehicle_frac")
        state = ("survivorship UNMEASURABLE (this dataset carries no un-enforced record and no "
                 "step-loop tallies" + (f"; {rv:.2%} of its vehicles were revoked)" if rv is not None
                                        else ")"))
    warn = ("  **ENFORCEMENT-TRUNCATED**: a revoked vehicle stops broadcasting, so this is a record "
            "of what the MA could hear and not a traffic sample. Re-run with "
            "`--emit-mobility-oracle`, or pass `--sumo-trace`." if src.get("truncated") else "")
    return f"- Traffic panel read **{kind}**, {state}.{warn}"


def render_lines(card: dict, include_na: bool = False) -> list[str]:
    """Compact one-line-per-metric rendering for the datasheet's realism scorecard."""
    lines: list[str] = [source_line(card)]
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
    p.add_argument("--traffic-source", choices=("auto", "oracle", "trace", "emissions"),
                   default="auto",
                   help="which mobility record the TRAFFIC panel reads: 'oracle' = the engine's "
                        "un-enforced gt_mobility_oracle.jsonl, 'trace' = a frozen SUMO trace, "
                        "'emissions' = the broadcast stream (TRUNCATED at revocation -- what this "
                        "flag exists to stop being the silent default). 'auto' prefers them in "
                        "that order. The COMM panel always reads the broadcast stream")
    p.add_argument("--sumo-trace", default=None,
                   help="path to the frozen SUMO trace to score the traffic panel from (default: "
                        "the one manifest.config.sumo_trace pins, if it is still on disk)")
    p.add_argument("--markdown", action="store_true", help="print the compact scorecard lines instead of JSON")
    p.add_argument("--fail-on-hard", action="store_true",
                   help="exit 1 when a HARD metric fails (CI gate; default is measure-only exit 0)")
    a = p.parse_args(argv)

    if not os.path.isdir(a.dataset_dir):
        p.error(f"dataset directory not found: {a.dataset_dir}")
    card = scorecard(a.dataset_dir, a.refdata, regime=a.regime, art_max_m=a.art_max_m,
                     radio_range_m=a.radio_range_m, t_bucket_s=a.t_bucket_s, dist_bin_m=a.bin_m,
                     max_dist_m=a.max_dist_m, fd_cell_m=a.fd_cell_m, fd_window_s=a.fd_window_s,
                     traffic_source=a.traffic_source, sumo_trace=a.sumo_trace)
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
