"""Measure what the opt-in TR 37.885 channel-physics terms actually do to the delivered packets.

WHY THIS FILE EXISTS
--------------------
Six opt-in terms landed in ``GeometricChannel`` (``mock_pipeline/run.py``): a per-link NLOSv
blockage hold, a TR 37.885 vehicle antenna pattern, a measurement-based NLOSv level, a two-ray
ground-reflection breakpoint, per-vehicle-type blocker footprints, and a Clarke-correlated
small-scale fade. Every one defaults OFF, and the digests prove they are inert when off. Nothing
measured what they do when ON.

The instrument the project already owns cannot answer that. ``datagen/awareness.py`` computes
delivery by exact QUADRATURE over the physics -- ``propagation_pdr`` re-derives path loss,
shadowing, the ``TR37885_NLOSV["both_below"]`` blockage term and the Nakagami fade analytically --
so it is structurally blind to five of the six terms (§"the instrument gap" in
``docs/realism/CHANNEL-PHYSICS.md``, and :func:`analytic_blindness` here, which measures the
blindness rather than asserting it). The sixth, ``radio_blocker_width``, is invisible to it too
because ``link_state_composition`` rebuilds the blocker index without per-blocker widths.

So this module measures the channel by RUNNING IT. It replays a finished dataset's own emission
trace -- true positions, every vehicle, every step -- through the REAL ``GeometricChannel`` object,
once per arm, and records the per-link, per-step reception decision the channel takes. The scene is
held byte-identical across arms (same positions, same vehicle types, same buildings, same vids, and
therefore the same per-link RNG stream keys the engine itself uses, ``f"{seed}:geo:{tx}:{rx}"``), so
every difference between two arms is the physics term and nothing else.

That replay convention is ``xengine_radio.measure``'s: hold the scene fixed so the residual is
physics. The metric arithmetic is ``awareness``'s and is IMPORTED, never re-derived --
``curve_value_at`` (the reference's annulus), ``crossing_m`` (the least-non-increasing-majorant
crossing), ``gray_zone_ratio``, ``nar_from_pdr`` / ``z_for_engine`` (the paper's shot multiplicity),
``reference_nar90_distance_m`` (Boban & d'Orey's own curve at OUR link budget). A second copy of any
of those would be a second opinion, not a comparison.

WHAT IS MEASURED
----------------
``measure``
    Per arm, on one scene:

    * PDR vs distance on the reference's own 25 m PDR bin, and PDR at 200 m over its annulus;
    * effective range -- d90 / d50 / d20 and the 0.90-NAR-equivalent range at the engine's own CAM
      rate, against Boban & d'Orey's measured 200 m urban anchor;
    * THE GRAY ZONE -- d20 - d90 in metres (``comm.pdr_gray_zone_width_m``'s definition) and the
      dimensionless d20/d90;
    * CONSECUTIVE-LOSS RUN LENGTH -- per ordered link, over contiguous co-presence, the run-length
      distribution of losses plus P(loss | previous step lost) and the burstiness ratio
      P(loss|loss)/P(loss). This is the statistic misbehaviour detection consumes and the project
      had never measured it;
    * PACKET INTER-RECEPTION TIME -- the gap between successive successful receptions on one link,
      in seconds, with the tail (p95/p99/max) and the fraction over 1 s;
    * link-state composition and the channel's own ``stats`` counters (including the terms' honest
      not-modelled counters, e.g. ``ant_rsu_not_modelled``);
    * the replay's own cost in microseconds per link evaluation.

``cost``
    Wall-clock cost of the REAL engine, per arm, from timed ``mock_pipeline.run`` invocations --
    the ms/step figure ``FULL-CITY-SCENE.md`` quotes -- plus the emission-trace digest per arm,
    which is what PROVES the scene did not move between arms.

``blindness``
    Runs ``awareness.propagation_pdr`` across the arms' configs to demonstrate that the project's
    own graded instrument returns an identical number for every one of them.

TRUST FIREWALL: read-only over dataset directories; aggregate output only; no per-entity value in
any output; the only RNG is the pair-inclusion hash, which is deterministic and seed-free.

CLI:
    python tools/channel_physics.py measure <dataset_dir> --arms all --json out.json
    python tools/channel_physics.py cost --scene ref --out-root C:/Temp/cpm --json cost.json
    python tools/channel_physics.py blindness
    python tools/channel_physics.py render out.json --markdown
    python tools/channel_physics.py collusion --pairs 20000
"""
from __future__ import annotations

import argparse
import json
import math
import os
import random
import statistics
import subprocess
import sys
import time
import zlib
from collections import Counter

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if os.path.join(REPO, "src") not in sys.path:
    sys.path.insert(0, os.path.join(REPO, "src"))

from scms_sim_ref.datagen import awareness as aw            # noqa: E402  (path set above)
from scms_sim_ref.mock_pipeline import run as R             # noqa: E402

PDR_BIN_M = 25.0          # v2x_awareness_conditions.nar_definition: the reference's PDR bin
NAR_BIN_M = aw.DIST_BIN_M  # 50 m: the reference's NAR bin
MAX_DIST_M = aw.MAX_DIST_M  # 1000 m
#: Every ordered pair closer than this is evaluated, in EVERY arm. Deliberately NOT each arm's own
#: `reach_m`: with the antenna pattern on, `reach_m` widens by twice the element's max gain, and a
#: PDR curve read off a different pair population per arm would confound the term with its own
#: candidate window. Links past the engine's window are losses in the engine anyway.
EVAL_MAX_M = MAX_DIST_M
#: Budget on ordered link evaluations per arm. Above it, whole ordered PAIRS are dropped by a
#: deterministic hash of the pair -- never whole steps, because dropping steps would destroy the
#: contiguity that the burst and inter-reception statistics are computed over.
DEFAULT_EVAL_BUDGET = 1_500_000

#: The arms. Key -> the config overrides that define it. `off` is the shipped default.
ARMS: dict[str, dict] = {
    "off":            {},
    "nlosv_hold":     {"radio_nlosv_hold": True},
    "antenna":        {"radio_antenna_pattern": "tr37885_opt1"},
    # RETRACTED 2026-09-07: the arm `nlosv_boban` (`radio_nlosv_model="measured_boban"`) is gone,
    # knob and all. Its 100 m level anchor was GEMV^2 simulator output digitised from an
    # IEEE-copyright figure, and graded against the one independent 5.9 GHz truck measurement in
    # its own citation set it was +11 to +15 dB wrong where the SPECIFICATION it replaced was
    # -0.96 / +4.04 dB. Section 6 R2 of docs/realism/CHANNEL-PHYSICS.md is the negative result.
    "breakpoint":     {"radio_breakpoint": "two_ray"},
    "blocker_width":  {"radio_blocker_width": "tr37885"},
    "jakes":          {"radio_fading_correlation": "jakes"},
    # DIAGNOSTIC, AND IT IS THE NON-CONFORMANT ONE -- RENAMED AND RE-LABELLED 2026-09-07. It was
    # called `antenna_eirp_fix` on the argument that `radio_tx_power_dbm` is an EIRP (run.py's
    # comment said so), that an EIRP already contains the transmit antenna gain, and that dropping
    # the transmit power to a conducted 20 dBm therefore REPAIRED a 3 dB double count. Read against
    # the standard the model claims to implement, that has it backwards: TR 37.885 Table 6.1.1-1
    # lists 'UE Tx power ... 23dBm' beside 'Macro BS: 49dBm' -- conducted PA power -- and gives the
    # element gain separately in Table 6.1.4-8, so the conformant V2V budget is 23 + 3 - PL + 3 and
    # the `antenna` arm is the one that computes it. This arm now measures what it costs to run
    # 3 dB below the standard's budget, which is a legitimate question and a different one.
    "antenna_conducted_20dbm": {"radio_antenna_pattern": "tr37885_opt1",
                                "radio_tx_power_dbm": 20.0},
    # the three the standard we claim to implement actually specifies. `nlosv_hold` is NOT one of
    # them -- see run.py's GAP 2 comment; it is here on physics, and the label was corrected.
    "conformant":     {"radio_antenna_pattern": "tr37885_opt1",
                       "radio_blocker_width": "tr37885"},
    "all_on":         {"radio_nlosv_hold": True, "radio_antenna_pattern": "tr37885_opt1",
                       "radio_breakpoint": "two_ray",
                       "radio_blocker_width": "tr37885", "radio_fading_correlation": "jakes"},
}
#: Shippable arms, in report order. `antenna_conducted_20dbm` is a diagnostic, measured separately.
ARM_ORDER = [a for a in ARMS if a != "antenna_conducted_20dbm"] + ["antenna_conducted_20dbm"]


# ============================================================================================= #
# the replay config: a read-only stand-in for the run's own Config, carrying one arm's overrides
# ============================================================================================= #
class _ArmConfig:
    """The fields ``GeometricChannel.__init__`` reads, from a finished run's manifest config.

    Every radio field is taken from the DATASET, so the replay runs the link budget the run ran;
    only the six opt-in keys are set by the arm. `GeometricChannel` reads the extensions through
    ``getattr`` with defaults, so an unknown key here would be silently ignored -- hence the
    explicit whitelist check in :func:`arm_config`.
    """

    __slots__ = ("seed", "radio_env", "radio_tx_power_dbm", "radio_rx_sensitivity_dbm",
                 "radio_nlosb_density_per_km", "radio_range_m", "radio_cap_sigma",
                 "radio_cap_max_mult", "radio_nlosv_hold",
                 "radio_antenna_pattern", "radio_antenna_gain_dbi", "radio_blocker_width",
                 "radio_breakpoint", "radio_breakpoint_slope_db_per_decade",
                 "radio_fading_correlation")


def arm_config(cfg: dict, arm: str) -> _ArmConfig:
    """Build the channel config for one arm out of a dataset's manifest ``config`` block."""
    unknown = set(ARMS[arm]) - set(_ArmConfig.__slots__)
    if unknown:
        raise KeyError(f"arm {arm!r} sets config keys the channel never reads: {sorted(unknown)}")
    c = _ArmConfig()
    c.seed = int(cfg.get("seed", 42) or 42)
    c.radio_env = str(cfg.get("radio_env") or "urban")
    c.radio_tx_power_dbm = float(cfg.get("radio_tx_power_dbm", 23.0) or 23.0)
    c.radio_rx_sensitivity_dbm = float(cfg.get("radio_rx_sensitivity_dbm", -81.0) or -81.0)
    c.radio_nlosb_density_per_km = float(cfg.get("radio_nlosb_density_per_km", 4.0) or 0.0)
    c.radio_range_m = float(cfg.get("radio_range_m", 500.0) or 500.0)
    c.radio_cap_sigma = float(cfg.get("radio_cap_sigma", R.RADIO_CAP_SIGMA) or R.RADIO_CAP_SIGMA)
    c.radio_cap_max_mult = float(cfg.get("radio_cap_max_mult", R.RADIO_CAP_MAX_MULT)
                                 or R.RADIO_CAP_MAX_MULT)
    # the five, at their shipped defaults, then the arm's overrides
    c.radio_nlosv_hold = False
    c.radio_antenna_pattern = "none"
    c.radio_antenna_gain_dbi = float(cfg.get("radio_antenna_gain_dbi",
                                             R.TR37885_ANT_MAX_GAIN_DBI))
    c.radio_blocker_width = "uniform"
    c.radio_breakpoint = "none"
    c.radio_breakpoint_slope_db_per_decade = float(
        cfg.get("radio_breakpoint_slope_db_per_decade", R.TWO_RAY_SLOPE_DB_PER_DECADE))
    c.radio_fading_correlation = "none"
    for k, v in ARMS[arm].items():
        setattr(c, k, v)
    return c


# ============================================================================================= #
# the scene: one dataset's own trace, decoded once and shared by every arm
# ============================================================================================= #
def _vid_of(true_vehicle_id: str) -> int:
    """``"veh_017"`` -> 17. run.py:6394 writes ``true_id = f"veh_{vid:03d}"``, so this recovers the
    ENGINE'S OWN vid -- which is what makes the replay's per-link stream keys
    (``f"{seed}:geo:{tx}:{rx}"``) the same keys the run itself drew on."""
    return int(str(true_vehicle_id).rsplit("_", 1)[-1])


def _step_snapshots(emissions: list[dict], dt_s: float) -> list[dict]:
    """One true position per vehicle per ENGINE STEP, in step order.

    ``awareness.snapshots`` is deliberately not used for the bucketing, for one reason that is not
    a matter of taste: it buckets at the reference's 1 s awareness window, and at ``dt = 0.1`` s
    that collapses ten CAMs on one link into a single row -- which is exactly the structure the
    burst and inter-reception statistics are made of. (Its ``floor(t / bucket_s)`` would also
    mis-bin at that width: ``0.3 / 0.1`` is 2.9999999999999996 in binary floating point.) The step
    index here is ``round(t / dt)``, which is exact for the emitted times. At ``dt = 1`` s the two
    agree, and :func:`load_scene` MEASURES that agreement rather than assuming it.
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
        b = int(round(t / dt_s))
        cur = buckets.setdefault(b, {}).get(str(vid))
        if cur is None or t >= cur[0]:
            buckets[b][str(vid)] = (t, x, y)
    out = []
    for b in sorted(buckets):
        vids = sorted(buckets[b])
        if len(vids) < 2:
            continue
        out.append({"bucket": b, "vids": vids,
                    "x": np.array([buckets[b][v][1] for v in vids], dtype=float),
                    "y": np.array([buckets[b][v][2] for v in vids], dtype=float)})
    return out


def load_scene(dataset_dir: str, max_steps: int | None = None) -> dict:
    """Positions, vehicle typing and buildings for one dataset, in step order.

    Every step is kept -- no sub-sampling at all: the burst-length and inter-reception statistics
    are defined over CONSECUTIVE steps, and a sub-sampled step list has no consecutive steps in it.
    """
    scen = aw.load_scenario(dataset_dir)
    dt = float(scen["dt_s"])
    snaps = _step_snapshots(scen["emissions"], dt)
    bucket_check = None
    if abs(dt - 1.0) < 1e-12:
        ref = aw.snapshots(scen["emissions"], max_snaps=None)
        bucket_check = ("identical to awareness.snapshots"
                        if [s["bucket"] for s in ref] == [s["bucket"] for s in snaps]
                        and all(a["vids"] == b["vids"] for a, b in zip(ref, snaps))
                        else "DIFFERS from awareness.snapshots -- investigate")
    if max_steps:
        snaps = snaps[:int(max_steps)]
    heights = scen["blocker_height_m"]
    vru = set()
    for v in aw._jsonl(os.path.join(dataset_dir, "ground_truth", "gt_vehicle.jsonl")):
        if v.get("is_vru"):
            vru.add(_vid_of(v["true_vehicle_id"]))
    steps = []
    for s in snaps:
        vids = np.array([_vid_of(v) for v in s["vids"]], dtype=np.int64)
        hs = np.array([0.0 if _vid_of(v) in vru else heights.get(v, R.TR37885_BLOCKER_HEIGHT_M["car"])
                       for v in s["vids"]], dtype=float)
        steps.append({"step": int(s["bucket"]), "vids": vids, "x": s["x"], "y": s["y"],
                      "blocker_h": hs,
                      "is_vru": np.array([int(v) in vru for v in vids], dtype=bool)})
    return {"dataset_dir": dataset_dir, "config": scen["config"], "buildings": scen["buildings"],
            "buildings_source": scen["buildings_source"], "steps": steps,
            "bucket_check": bucket_check,
            "dt_s": float(scen["dt_s"]), "n_vehicles": len(heights),
            "emit_sample_prob": float(scen["emit_sample_prob"]),
            "tx_power_dbm": scen["tx_power_dbm"],
            "rx_sensitivity_dbm": scen["rx_sensitivity_dbm"],
            "radio_env": scen["radio_env"], "radio_model": scen["radio_model"]}


def plan_pairs(scene: dict, budget: int = DEFAULT_EVAL_BUDGET) -> dict:
    """Decide which ordered pairs are evaluated, identically in every arm.

    Counts the in-range ordered pairs per step with one vectorised distance matrix, then, if the
    total exceeds ``budget``, keeps a fraction of the unordered pairs chosen by ``crc32`` of the
    pair. Whole PAIRS are dropped, never whole steps: inclusion is uniform over pairs, independent
    of arm, and every kept pair keeps its complete, contiguous history -- which is what the burst
    and inter-reception statistics are defined over.
    """
    total = 0
    for st in scene["steps"]:
        x, y = st["x"], st["y"]
        n = x.size
        if n < 2:
            continue
        d = np.hypot(x[:, None] - x[None, :], y[:, None] - y[None, :])[np.triu_indices(n, 1)]
        total += int((d < EVAL_MAX_M).sum()) * 2
    keep = 1.0 if total <= budget else float(budget) / float(total)
    return {"ordered_pair_steps_available": total, "keep_frac": keep,
            "ordered_pair_steps_planned": int(round(total * keep)), "budget": int(budget),
            "eval_max_m": EVAL_MAX_M}


def _pair_kept(a: int, b: int, keep: float) -> bool:
    if keep >= 1.0:
        return True
    lo, hi = (a, b) if a < b else (b, a)
    return (zlib.crc32(b"%d:%d" % (lo, hi)) / 4294967296.0) < keep


# ============================================================================================= #
# the replay
# ============================================================================================= #
class _Acc:
    """Per-arm accumulators. Histograms, never per-link records: a 300 s full-city arm has ~10^6
    link-steps and the report is aggregate by construction."""

    def __init__(self, n_pdr_bins: int, n_bands: int):
        self.n = np.zeros(n_pdr_bins, dtype=np.int64)         # link-steps per 25 m PDR bin
        self.heard = np.zeros(n_pdr_bins, dtype=np.int64)
        self.state_n = {s: np.zeros(n_bands, dtype=np.int64) for s in ("LOS", "NLOSv", "NLOSb")}
        # burst structure, per 50 m band and overall
        self.after_loss = np.zeros(n_bands, dtype=np.int64)
        self.loss_after_loss = np.zeros(n_bands, dtype=np.int64)
        self.after_heard = np.zeros(n_bands, dtype=np.int64)
        self.loss_after_heard = np.zeros(n_bands, dtype=np.int64)
        self.band_n = np.zeros(n_bands, dtype=np.int64)
        self.band_loss = np.zeros(n_bands, dtype=np.int64)
        self.runs: list[Counter] = [Counter() for _ in range(n_bands)]
        self.irt: list[Counter] = [Counter() for _ in range(n_bands)]
        self.rssi_sum = np.zeros(n_bands, dtype=float)
        # NAR the reference's own way: per link, per 1 s window, was AT LEAST ONE message received.
        # With eq. (4) NAR = 1 - (1 - PDR)^Z this yields an EMPIRICAL Z, which is the paper's own
        # burstiness statistic ("Z <= N ... discounted for the measured burstiness of CAM loss").
        self.win_n = np.zeros(n_bands, dtype=np.int64)
        self.win_hit = np.zeros(n_bands, dtype=np.int64)
        self.win_pkts = np.zeros(n_bands, dtype=np.int64)
        self.state_changes = 0
        self.state_obs = 0
        self.episodes = 0
        self.link_steps = 0


def replay(scene: dict, arm: str, *, keep_frac: float = 1.0,
           progress: bool = False) -> dict:
    """Run one arm's ``GeometricChannel`` over the whole scene and return its aggregate metrics."""
    cfg = arm_config(scene["config"], arm)
    t_build = time.perf_counter()
    chan = R.GeometricChannel(cfg, buildings=scene["buildings"] or None, dt=scene["dt_s"])
    build_s = time.perf_counter() - t_build

    n_pdr = int(MAX_DIST_M // PDR_BIN_M)
    n_band = int(MAX_DIST_M // NAR_BIN_M)
    acc = _Acc(n_pdr, n_band)
    # per ordered pair: [last_step, last_heard, run_len, run_dist_sum, last_rx_step, last_state,
    #                    nar_window, nar_any, nar_band, nar_pkts]
    live: dict[tuple[int, int], list] = {}
    dt = scene["dt_s"]

    def _commit_window(rec):
        if rec[6] is None:
            return
        acc.win_n[rec[8]] += 1
        acc.win_pkts[rec[8]] += rec[9]
        if rec[7]:
            acc.win_hit[rec[8]] += 1
        rec[6] = None
    t0 = time.perf_counter()
    n_eval = 0

    for si, st in enumerate(scene["steps"]):
        vids, xs, ys, hs = st["vids"], st["x"], st["y"], st["blocker_h"]
        n = vids.size
        stations = {}
        for k in range(n):
            v = int(vids[k])
            stations[v] = R.StationSnapshot(v, float(xs[k]), float(ys[k]),
                                            R.V2X_ANTENNA_HEIGHT_M, float(hs[k]),
                                            False, bool(st["is_vru"][k]), None, 0.0)
        frame = R.StepFrame(step=int(st["step"]), t=float(st["step"]) * dt, dt=dt,
                            stations=stations, transmissions=(), receivers=(),
                            weather_loss=0.0, env={})
        chan.begin_step(frame)
        step_idx = int(st["step"])
        if n < 2:
            continue
        dmat = np.hypot(xs[:, None] - xs[None, :], ys[:, None] - ys[None, :])
        ii, jj = np.nonzero(np.triu(dmat < EVAL_MAX_M, 1))
        for a, b in zip(ii.tolist(), jj.tolist()):
            va, vb = int(vids[a]), int(vids[b])
            if not _pair_kept(va, vb, keep_frac):
                continue
            d = float(dmat[a, b])
            pb = int(min(d // PDR_BIN_M, n_pdr - 1))
            bb = int(min(d // NAR_BIN_M, n_band - 1))
            for (tv, ti), (rv, ri) in (((va, a), (vb, b)), ((vb, b), (va, a))):
                heard, rssi, state, _ = chan.evaluate_raw(
                    tv, rv, float(xs[ti]), float(ys[ti]), float(xs[ri]), float(ys[ri]), d,
                    R.V2X_ANTENNA_HEIGHT_M, R.V2X_ANTENNA_HEIGHT_M)
                n_eval += 1
                acc.n[pb] += 1
                acc.band_n[bb] += 1
                acc.state_n[state][bb] += 1
                if heard:
                    acc.heard[pb] += 1
                    acc.rssi_sum[bb] += rssi
                else:
                    acc.band_loss[bb] += 1
                key = (tv, rv)
                prev = live.get(key)
                win = int(math.floor(step_idx * dt))
                if prev is not None and prev[6] is not None and prev[6] != win:
                    _commit_window(prev)
                if prev is not None and prev[0] == step_idx - 1:
                    if prev[6] is None:
                        prev[6], prev[7], prev[8], prev[9] = win, heard, bb, 1
                    else:
                        prev[7] = prev[7] or heard
                        prev[9] += 1
                    if prev[1]:                                  # previous step was heard
                        acc.after_heard[bb] += 1
                        if not heard:
                            acc.loss_after_heard[bb] += 1
                    else:
                        acc.after_loss[bb] += 1
                        if not heard:
                            acc.loss_after_loss[bb] += 1
                    acc.state_obs += 1
                    if prev[5] != state:
                        acc.state_changes += 1
                    if heard:
                        if prev[4] is not None:
                            gap = step_idx - prev[4]
                            acc.irt[bb][gap] += 1
                        if prev[2]:                              # a loss run just ended
                            rb = int(min((prev[3] / prev[2]) // NAR_BIN_M, n_band - 1))
                            acc.runs[rb][prev[2]] += 1
                        prev[2], prev[3] = 0, 0.0
                        prev[4] = step_idx
                    else:
                        prev[2] += 1
                        prev[3] += d
                    prev[0], prev[1], prev[5] = step_idx, heard, state
                else:
                    if prev is not None:                         # episode break: close run + window
                        if prev[2]:
                            rb = int(min((prev[3] / prev[2]) // NAR_BIN_M, n_band - 1))
                            acc.runs[rb][prev[2]] += 1
                        _commit_window(prev)
                    acc.episodes += 1
                    live[key] = [step_idx, heard, 0 if heard else 1, 0.0 if heard else d,
                                 step_idx if heard else None, state, win, heard, bb, 1]
        if progress and si % 25 == 0:
            print(f"  [{arm}] step {si}/{len(scene['steps'])} evals={n_eval}", file=sys.stderr)
    for prev in live.values():                                   # runs/windows open at the end
        if prev[2]:
            rb = int(min((prev[3] / prev[2]) // NAR_BIN_M, n_band - 1))
            acc.runs[rb][prev[2]] += 1
        _commit_window(prev)
    elapsed = time.perf_counter() - t0
    acc.link_steps = n_eval
    out = summarise(acc, scene, arm)
    out["cost"] = {"replay_s": round(elapsed, 3), "channel_build_s": round(build_s, 3),
                   "link_evaluations": n_eval,
                   "us_per_link_eval": round(1e6 * elapsed / max(n_eval, 1), 3)}
    out["channel_stats"] = {k: int(v) for k, v in sorted(chan.stats.items())}
    out["reach_m"] = round(float(chan.reach_m), 1)
    return out


# ============================================================================================= #
# metrics
# ============================================================================================= #
def _hist_stats(c: Counter) -> dict:
    if not c:
        return {"n": 0, "mean": None, "p50": None, "p95": None, "p99": None, "max": None}
    keys = np.array(sorted(c), dtype=float)
    w = np.array([c[int(k)] for k in keys], dtype=float)
    tot = w.sum()
    cum = np.cumsum(w) / tot

    def q(p):
        return float(keys[int(np.searchsorted(cum, p, side="left"))])
    return {"n": int(tot), "mean": round(float((keys * w).sum() / tot), 4),
            "p50": q(0.50), "p95": q(0.95), "p99": q(0.99), "max": float(keys[-1])}


def _curve(n: np.ndarray, heard: np.ndarray, bin_m: float) -> dict:
    edges = np.arange(0.0, bin_m * n.size + bin_m / 2.0, bin_m)
    with np.errstate(invalid="ignore", divide="ignore"):
        pdr = np.where(n > 0, heard / np.maximum(n, 1), np.nan)
    return {"edges": edges, "centres": (edges[:-1] + edges[1:]) / 2.0, "pdr": pdr,
            "n_pairs": n.astype(float)}


def summarise(acc: "_Acc", scene: dict, arm: str) -> dict:
    dt = scene["dt_s"]
    pdr_curve = _curve(acc.n, acc.heard, PDR_BIN_M)
    band_heard = acc.band_n - acc.band_loss
    nar_curve = _curve(acc.band_n, band_heard, NAR_BIN_M)

    z = aw.z_for_engine(dt)
    nar90_pdr = aw.pdr_for_nar(0.90, z)
    d90 = aw.crossing_m(nar_curve, 0.90)
    d50 = aw.crossing_m(nar_curve, 0.50)
    d20 = aw.crossing_m(nar_curve, 0.20)
    d_nar90 = aw.crossing_m(nar_curve, nar90_pdr)

    # THE MEASURED awareness curve: >= 1 message in a 1 s window, per link, per band. At dt = 1 s
    # there is one CAM per window and this is identically the per-packet PDR (a sanity check the
    # report quotes); at dt = 0.1 s it is the reference's NAR and the two together give an
    # EMPIRICAL Z, the paper's own burstiness statistic.
    with np.errstate(invalid="ignore", divide="ignore"):
        nar_emp = np.where(acc.win_n > 0, acc.win_hit / np.maximum(acc.win_n, 1), np.nan)
    nar_meas_curve = {"edges": nar_curve["edges"], "centres": nar_curve["centres"],
                      "pdr": nar_emp, "n_pairs": acc.win_n.astype(float)}
    d_nar90_meas = aw.crossing_m(nar_meas_curve, 0.90)
    pkts_per_window = (float(acc.win_pkts.sum()) / max(int(acc.win_n.sum()), 1))
    z_emp = []
    for b in range(acc.band_n.size):
        p = 1.0 - (float(acc.band_loss[b]) / acc.band_n[b]) if acc.band_n[b] else None
        na = float(nar_emp[b]) if acc.win_n[b] else None
        if p is None or na is None or not (0.0 < p < 1.0) or not (0.0 <= na < 1.0):
            z_emp.append(None)
        else:
            z_emp.append(round(math.log(1.0 - na) / math.log(1.0 - p), 4))

    n_band = acc.band_n.size
    bands = []
    for b in range(n_band):
        tot = int(acc.band_n[b])
        if tot == 0:
            continue
        p_loss = float(acc.band_loss[b]) / tot
        pll = (float(acc.loss_after_loss[b]) / acc.after_loss[b]
               if acc.after_loss[b] else None)
        bands.append({
            "band_m": [b * NAR_BIN_M, (b + 1) * NAR_BIN_M],
            "n_link_steps": tot,
            "pdr": round(1.0 - p_loss, 6),
            "p_loss": round(p_loss, 6),
            "p_loss_given_loss": None if pll is None else round(pll, 6),
            "burstiness": None if (pll is None or p_loss <= 0) else round(pll / p_loss, 4),
            "nar_measured": (None if not acc.win_n[b] else round(float(nar_emp[b]), 6)),
            "nar_windows": int(acc.win_n[b]),
            "z_empirical": z_emp[b],
            "loss_run": _hist_stats(acc.runs[b]),
            "irt_s": {k: (None if v is None else (v * dt if k != "n" else v))
                      for k, v in _hist_stats(acc.irt[b]).items()},
            "irt_over_1s_frac": (round(sum(v for k, v in acc.irt[b].items() if k * dt > 1.0)
                                       / max(sum(acc.irt[b].values()), 1), 6)
                                 if acc.irt[b] else None),
            "state_frac": {s: round(float(acc.state_n[s][b]) / tot, 6)
                           for s in acc.state_n},
            "mean_rssi_dbm_heard": (round(float(acc.rssi_sum[b] / max(band_heard[b], 1)), 3)
                                    if band_heard[b] else None),
        })

    all_runs: Counter = Counter()
    all_irt: Counter = Counter()
    for b in range(n_band):
        all_runs.update(acc.runs[b])
        all_irt.update(acc.irt[b])
    tot_n = int(acc.band_n.sum())
    tot_loss = int(acc.band_loss.sum())
    p_loss = tot_loss / max(tot_n, 1)
    pll = (float(acc.loss_after_loss.sum()) / acc.after_loss.sum()
           if acc.after_loss.sum() else None)
    irt_all = _hist_stats(all_irt)

    # the band that carries the anchor, reported on its own because it is the graded one
    def band_of(dm):
        for row in bands:
            if row["band_m"][0] <= dm < row["band_m"][1]:
                return row
        return None

    return {
        "arm": arm,
        "overrides": ARMS[arm],
        "headline": {
            "pdr_200m": _r(aw.curve_value_at(pdr_curve, 200.0, PDR_BIN_M)),
            "pdr_100m": _r(aw.curve_value_at(pdr_curve, 100.0, PDR_BIN_M)),
            "pdr_300m": _r(aw.curve_value_at(pdr_curve, 300.0, PDR_BIN_M)),
            "pdr_overall": _r(1.0 - p_loss),
            "d90_m": _r(d90, 1), "d50_m": _r(d50, 1), "d20_m": _r(d20, 1),
            "gray_zone_width_m": _r(None if (d90 is None or d20 is None) else d20 - d90, 1),
            "gray_zone_ratio": _r(aw.gray_zone_ratio(d90, d20), 4),
            "nar_z": _r(z, 4),
            "nar90_equivalent_pdr": _r(nar90_pdr, 4),
            "nar90_equivalent_range_m": _r(d_nar90, 1),
            "awareness_ratio_200m": _r(None if aw.curve_value_at(nar_curve, 200.0, NAR_BIN_M)
                                       is None else
                                       aw.nar_from_pdr(aw.curve_value_at(nar_curve, 200.0,
                                                                         NAR_BIN_M), z)),
            "awareness_ratio_200m_measured": _r(aw.curve_value_at(nar_meas_curve, 200.0,
                                                                  NAR_BIN_M)),
            "nar90_measured_range_m": _r(d_nar90_meas, 1),
            "packets_per_1s_window": _r(pkts_per_window, 3),
            "z_empirical_200_250m": z_emp[int(200.0 // NAR_BIN_M)]
            if int(200.0 // NAR_BIN_M) < len(z_emp) else None,
            "p_loss": _r(p_loss),
            "p_loss_given_loss": _r(pll),
            "burstiness": _r(None if (pll is None or p_loss <= 0) else pll / p_loss, 4),
            "loss_run_mean": _hist_stats(all_runs)["mean"],
            "loss_run_p95": _hist_stats(all_runs)["p95"],
            "loss_run_max": _hist_stats(all_runs)["max"],
            "irt_mean_s": _r(None if irt_all["mean"] is None else irt_all["mean"] * dt, 4),
            "irt_p95_s": _r(None if irt_all["p95"] is None else irt_all["p95"] * dt, 4),
            "irt_p99_s": _r(None if irt_all["p99"] is None else irt_all["p99"] * dt, 4),
            "irt_max_s": _r(None if irt_all["max"] is None else irt_all["max"] * dt, 4),
            "link_state_change_rate": _r(acc.state_changes / max(acc.state_obs, 1)),
        },
        "band_200m": band_of(200.0),
        "bands": bands,
        "loss_run_all": _hist_stats(all_runs),
        "totals": {"link_steps": tot_n, "episodes": acc.episodes,
                   "steps": len(scene["steps"])},
        "curve_pdr25": {"centres": pdr_curve["centres"].tolist(),
                        "pdr": [None if not np.isfinite(v) else round(float(v), 6)
                                for v in pdr_curve["pdr"]],
                        "n": acc.n.tolist()},
    }


def _r(x, nd: int = 6):
    if x is None:
        return None
    try:
        v = float(x)
    except (TypeError, ValueError):
        return x
    return None if not math.isfinite(v) else round(v, nd)


# ============================================================================================= #
# the analytic instrument's blindness, measured rather than asserted
# ============================================================================================= #
def analytic_blindness() -> dict:
    """``awareness.propagation_pdr`` evaluated for every arm's config, at three distances.

    It takes no argument that any of the six terms can reach: it re-derives path loss, shadowing,
    the ``TR37885_NLOSV["both_below"]`` blockage term and the Nakagami fade from the state name and
    the distance alone. This function exists so the report can quote a measured identity rather
    than an inspection of the source.
    """
    floor = aw.decode_floor_dbm(-81.0)
    rows = {}
    for state in ("LOS", "NLOSv", "NLOSb"):
        rows[state] = {str(d): _r(aw.propagation_pdr(state, d, tx_power_dbm=23.0,
                                                     decode_floor_dbm=floor, radio_env="urban"))
                       for d in (100.0, 200.0, 300.0)}
    return {"propagation_pdr": rows,
            "signature": "propagation_pdr(state, d, tx_power_dbm, decode_floor_dbm, radio_env)",
            "reads_any_extension_knob": False,
            "note": ("The signature carries no channel object and no config: the five knobs that "
                     "act inside GeometricChannel (nlosv hold, antenna pattern, nlosv level, "
                     "breakpoint, fade correlation) cannot reach it, and the sixth "
                     "(radio_blocker_width) cannot reach link_state_composition either, which "
                     "rebuilds _VehicleBlockerIndex from 4-tuples and so always takes the uniform "
                     "corridor width.")}


# ============================================================================================= #
# engine cost, from the real thing
# ============================================================================================= #
def _sha256(path: str) -> str:
    import hashlib
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run_engine(base_cfg_path: str, arm: str, out_dir: str, python_exe: str = sys.executable,
               timeout_s: float = 7200.0) -> dict:
    """Time one real ``mock_pipeline.run`` invocation with this arm's overrides.

    The config is replayed with ``--config`` (which "ignores other flags"), so the only difference
    between two arms' inputs is the six keys. The emission trace's sha256 is returned as a TEST of
    whether the transmit schedule moved -- not as a guarantee that it did not. On this engine it
    DOES move: the channel decides which reports are received, which decides which certificates the
    MA revokes, and `enforced(v, t)` then stops a revoked vehicle broadcasting. An end-to-end arm is
    therefore not a controlled A/B, and that is exactly why the physics is measured by
    :func:`replay` over ONE fixed trace instead. What these runs measure is wall-clock cost and the
    DOWNSTREAM consequence of the term, both of which are worth having and neither of which the
    replay can give.
    """
    with open(base_cfg_path, encoding="utf-8-sig") as fh:
        cfg = json.load(fh)
    cfg = dict(cfg.get("config", cfg))
    cfg.update(ARMS[arm])
    arm_cfg = os.path.join(os.path.dirname(out_dir), f"cfg_{arm}.json")
    with open(arm_cfg, "w", encoding="utf-8") as fh:
        json.dump(cfg, fh, indent=1)
    env = dict(os.environ)
    env["PYTHONPATH"] = os.path.join(REPO, "src") + os.pathsep + env.get("PYTHONPATH", "")
    t0 = time.perf_counter()
    proc = subprocess.run([python_exe, "-m", "scms_sim_ref.mock_pipeline.run",
                           "--config", arm_cfg, "--out", out_dir],
                          cwd=REPO, env=env, capture_output=True, text=True, timeout=timeout_s)
    wall = time.perf_counter() - t0
    # `duration_s` overrides `n_steps` when > 0 (run.py:3519); a sumo_replay arm carries neither and
    # gets its step count from the manifest below.
    _dt = max(float(cfg.get("dt", 1.0) or 1.0), 1e-9)
    steps = int(round(float(cfg.get("duration_s", 0.0) or 0.0) / _dt)) or \
        int(cfg.get("n_steps", 0) or 0) or None
    man = os.path.join(out_dir, "manifest.json")
    digest = None
    emis = None
    counts: dict = {}
    if os.path.exists(man):
        with open(man, encoding="utf-8") as fh:
            m = json.load(fh)
        digest = m.get("data_digest_sha256")
        steps = (m.get("mobility", {}).get("provider", {}).get("steps") or steps)
        cn = m.get("counts", {})
        counts = {k: cn.get(k) for k in ("vehicles", "reports", "investigations", "revoked")}
        ep = os.path.join(out_dir, "ground_truth", "gt_emissions_sample.jsonl")
        if os.path.exists(ep):
            emis = _sha256(ep)
    # The engine's own end-of-run summary line, kept because burst structure is what misbehaviour
    # detection consumes: a term that changes the loss process changes what the detectors see.
    det = {}
    for line in proc.stdout.splitlines():
        if line.startswith("detection:"):
            for tok in line.split()[1:]:
                if "=" in tok:
                    k, v = tok.split("=", 1)
                    det[k] = v
    return {"arm": arm, "returncode": proc.returncode, "wall_s": round(wall, 2),
            "steps": steps, "ms_per_step": (round(1000.0 * wall / steps, 1) if steps else None),
            "data_digest_sha256": digest, "emissions_sha256": emis,
            "counts": counts, "detection": det, "out_dir": out_dir,
            "stderr_tail": proc.stderr.strip().splitlines()[-3:] if proc.returncode else []}


# ============================================================================================= #
# rendering
# ============================================================================================= #
_COND: dict | None = None


def _conditions() -> dict:
    global _COND
    if _COND is None:
        _COND = aw.load_conditions()
    return _COND


def augment(rep: dict) -> dict:
    """Add the crossings under ``awareness_report``'s OWN conventions, so the rows in this report
    line up with the ones ``FULL-CITY-SCENE.md`` and the harness already publish.

    ``awareness_report`` does not read the 0.90 NAR level off our per-packet curve directly: it
    converts it through the paper's fitted urban shot multiplicity Z = 5.4579 into the per-packet
    level ``p* = pdr_for_nar(0.90, Z)`` = 0.3442 and reports the distance at which our curve crosses
    THAT. The 0.90 crossing of a per-packet curve is a different (and much shorter) quantity. Both
    are reported here, named for what they are, together with the paper's own Z bracket [2.14, 8.29]
    as an uncertainty band on the conversion.
    """
    cond = _conditions()
    zref = (aw._entry(cond, "nar_shot_multiplicity_z").get("value") or {})
    z_urban = float(zref.get("z_urban", 5.4579))
    z_lo, z_hi = [float(v) for v in (zref.get("z_range") or [2.1365, 8.2886])]
    env = str(rep["scene"].get("radio_env") or "urban").lower()
    budget = aw.link_budget_db(rep["scene"]["tx_power_dbm"], rep["scene"]["rx_sensitivity_dbm"])
    ref_d90, ref_note = aw.reference_nar90_distance_m(budget, env, cond)
    p_star = aw.pdr_for_nar(0.90, z_urban) if env == "urban" else None
    rep["reference"] = {
        "z_urban_fitted": z_urban, "z_range": [z_lo, z_hi],
        "per_packet_level_for_nar_0_90": _r(p_star, 4),
        "link_budget_db": _r(budget, 2),
        "boban_dorey_urban_nar90_range_at_our_budget_m": _r(ref_d90, 2),
        "reference_note": ref_note,
    }
    for a, r in rep["arms"].items():
        n = np.array([row["n_link_steps"] for row in r["bands"]], dtype=float)
        pdr = np.array([row["pdr"] for row in r["bands"]], dtype=float)
        ctr = np.array([(row["band_m"][0] + row["band_m"][1]) / 2.0 for row in r["bands"]])
        curve = {"centres": ctr, "pdr": pdr, "n_pairs": n,
                 "edges": np.append(np.array([row["band_m"][0] for row in r["bands"]]),
                                    r["bands"][-1]["band_m"][1])}
        h = r["headline"]
        h["nar90_equivalent_range_m"] = _r(aw.crossing_m(curve, p_star), 1) if p_star else None
        h["nar90_equivalent_range_lo_m"] = _r(aw.crossing_m(curve, aw.pdr_for_nar(0.90, z_lo)), 1)
        h["nar90_equivalent_range_hi_m"] = _r(aw.crossing_m(curve, aw.pdr_for_nar(0.90, z_hi)), 1)
        h["ratio_vs_boban_dorey_curve"] = (
            _r(h["nar90_equivalent_range_m"] / ref_d90, 3)
            if (ref_d90 and h["nar90_equivalent_range_m"]) else None)
        # The harness's OWN two graded curve rows -- `comm.pdr_gray_zone_width_m` and
        # `comm.effective_range_m` -- are read off a curve NORMALISED at the near band, not an
        # absolute one (`realism_bench._crossing` reads `curve["normalized"]`). Both forms are
        # reported so this document can be compared with a scorecard without a conversion step.
        near = next((p for p, k in zip(pdr, n) if k > 0 and np.isfinite(p)), None)
        if near:
            ncur = dict(curve, pdr=np.minimum(pdr / near, 1.0))
            n90, n50, n20 = (aw.crossing_m(ncur, lv) for lv in (0.90, 0.50, 0.20))
            h["normalised_near_band_pdr"] = _r(near)
            h["normalised_d90_m"] = _r(n90, 1)
            h["effective_range_m_normalised"] = _r(n50, 1)
            h["normalised_d20_m"] = _r(n20, 1)
            h["gray_zone_width_m_normalised"] = _r(
                None if (n90 is None or n20 is None) else n20 - n90, 1)
    return rep


def render(rep: dict) -> list[str]:
    augment(rep)
    L = []
    sc = rep["scene"]
    L.append(f"### {rep.get('label') or sc['dataset_dir']}")
    L.append("")
    L.append(f"- steps {sc['steps']}, vehicles {sc['n_vehicles']}, buildings {sc['n_buildings']} "
             f"({sc['buildings_source']}), dt {sc['dt_s']} s")
    L.append(f"- link budget {sc['tx_power_dbm']} dBm / {sc['rx_sensitivity_dbm']} dBm, "
             f"env {sc['radio_env']}")
    L.append(f"- pair plan: {rep['pairs']['ordered_pair_steps_available']} ordered link-steps "
             f"available, keep_frac {rep['pairs']['keep_frac']:.4f}")
    L.append("")
    hdr = ["arm", "PDR@200m", "PDR all", "NAR@200m", "d90", "d50", "d20", "gray m", "gray ratio",
           "NAR90 range", "Z(200-250)", "P(L|L)", "burst", "run mean", "run p95", "run max",
           "IRT p95 s", "IRT max s", "us/eval"]
    L.append("| " + " | ".join(hdr) + " |")
    L.append("|" + "---|" * len(hdr))
    base = rep["arms"].get("off", {}).get("headline", {})
    for a in ARM_ORDER:
        if a not in rep["arms"]:
            continue
        h = rep["arms"][a]["headline"]
        c = rep["arms"][a]["cost"]
        L.append("| " + " | ".join(str(v) for v in [
            a, h["pdr_200m"], h["pdr_overall"], h.get("awareness_ratio_200m_measured"),
            h["d90_m"], h["d50_m"], h["d20_m"],
            h["gray_zone_width_m"], h["gray_zone_ratio"], h.get("nar90_measured_range_m"),
            h.get("z_empirical_200_250m"),
            h["p_loss_given_loss"], h["burstiness"], h["loss_run_mean"], h["loss_run_p95"],
            h["loss_run_max"], h["irt_p95_s"], h["irt_max_s"], c["us_per_link_eval"]]) + " |")
    L.append("")
    if base:
        L.append("Deltas against `off`:")
        L.append("")
        L.append("| arm | dPDR@200m | dPDR all | d(gray m) | d(run mean) | d(P(L|L)) | cost x |")
        L.append("|---|---|---|---|---|---|---|")
        for a in ARM_ORDER:
            if a not in rep["arms"] or a == "off":
                continue
            h = rep["arms"][a]["headline"]

            def dd(k, nd=4):
                if h.get(k) is None or base.get(k) is None:
                    return "n/a"
                return f"{h[k] - base[k]:+.{nd}f}"
            cx = (rep["arms"][a]["cost"]["us_per_link_eval"]
                  / max(rep["arms"]["off"]["cost"]["us_per_link_eval"], 1e-9))
            L.append(f"| {a} | {dd('pdr_200m')} | {dd('pdr_overall')} | "
                     f"{dd('gray_zone_width_m', 1)} | {dd('loss_run_mean')} | "
                     f"{dd('p_loss_given_loss')} | {cx:.2f}x |")
        L.append("")
    return L


# ============================================================================================= #
# CLI
# ============================================================================================= #
def cmd_measure(args) -> int:
    scene = load_scene(args.dataset, max_steps=args.max_steps)
    pairs = plan_pairs(scene, budget=args.budget)
    arms = ARM_ORDER if args.arms == "all" else [a.strip() for a in args.arms.split(",")]
    for a in arms:
        if a not in ARMS:
            raise SystemExit(f"unknown arm {a!r}; have {ARM_ORDER}")
    rep = {"tool": "channel_physics", "mode": "measure", "label": args.label,
           "scene": {"dataset_dir": scene["dataset_dir"], "steps": len(scene["steps"]),
                     "n_vehicles": scene["n_vehicles"], "dt_s": scene["dt_s"],
                     "n_buildings": len(scene["buildings"]),
                     "buildings_source": scene["buildings_source"],
                     "tx_power_dbm": scene["tx_power_dbm"],
                     "rx_sensitivity_dbm": scene["rx_sensitivity_dbm"],
                     "radio_env": scene["radio_env"],
                     "radio_model_of_run": scene["radio_model"],
                     "bucket_check": scene["bucket_check"],
                     "emit_sample_prob": scene["emit_sample_prob"]},
           "pairs": pairs, "arms": {}}
    for a in arms:
        print(f"[channel_physics] arm {a} ...", file=sys.stderr)
        rep["arms"][a] = replay(scene, a, keep_frac=pairs["keep_frac"], progress=args.progress)
        print(f"[channel_physics] arm {a}: PDR@200m="
              f"{rep['arms'][a]['headline']['pdr_200m']} "
              f"{rep['arms'][a]['cost']['replay_s']} s", file=sys.stderr)
        if args.json:                       # checkpoint after every arm; these runs are long
            with open(args.json, "w", encoding="utf-8") as fh:
                json.dump(rep, fh, indent=1)
    augment(rep)                    # bake the reference-convention crossings into the saved JSON
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(rep, fh, indent=1)
    print("\n".join(render(rep)))
    return 0


def cmd_cost(args) -> int:
    arms = ARM_ORDER if args.arms == "all" else [a.strip() for a in args.arms.split(",")]
    os.makedirs(args.out_root, exist_ok=True)
    rows = []
    for a in arms:
        out = os.path.join(args.out_root, f"ds_{a}")
        print(f"[channel_physics] engine run, arm {a} ...", file=sys.stderr)
        row = run_engine(args.base_config, a, out, timeout_s=args.timeout)
        print(f"   -> {row['wall_s']} s, {row['ms_per_step']} ms/step, rc={row['returncode']}",
              file=sys.stderr)
        rows.append(row)
        if args.json:
            with open(args.json, "w", encoding="utf-8") as fh:
                json.dump({"tool": "channel_physics", "mode": "cost",
                           "base_config": args.base_config, "rows": rows}, fh, indent=1)
    base = next((r for r in rows if r["arm"] == "off"), None)
    print("| arm | wall s | ms/step | x off | emissions sha256 (16) | data digest (16) | reports "
          "| revoked | prec | recall |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        x = (r["ms_per_step"] / base["ms_per_step"]) if (base and base["ms_per_step"]
                                                         and r["ms_per_step"]) else float("nan")
        c, d = r.get("counts") or {}, r.get("detection") or {}
        print(f"| {r['arm']} | {r['wall_s']} | {r['ms_per_step']} | {x:.3f}x | "
              f"{(r['emissions_sha256'] or '')[:16]} | {(r['data_digest_sha256'] or '')[:16]} | "
              f"{c.get('reports')} | {c.get('revoked')} | {d.get('precision')} | "
              f"{d.get('recall')} |")
    if base and base.get("emissions_sha256"):
        same = all(r.get("emissions_sha256") == base["emissions_sha256"] for r in rows)
        print("")
        print("Emission traces identical across every arm: **" + ("YES" if same else "NO") + "**. "
              + ("The transmit schedule held, so an end-to-end difference is the channel term alone."
                 if same else
                 "The transmit schedule MOVED, so these runs are not a controlled A/B: the channel "
                 "changes which reports arrive, which changes what the MA revokes, and a revoked "
                 "vehicle stops broadcasting. Read these rows as cost and as downstream "
                 "consequence; read the physics off `measure`, which replays one fixed trace."))
    return 0


def term_budget() -> dict:
    """What each term does to ONE link's budget, in dB, from the shipped functions themselves.

    The scene replay says what the terms do to delivered packets; this says what they do to the
    arithmetic, so a reader can check the two against each other. Nothing here is a new model --
    every value is a call into ``mock_pipeline.run``.
    """
    ant = {}
    for label, directional in (("car (TR Type 2, rooftop)", False),
                               ("truck/bus (TR Type 3, front+rear)", True)):
        ant[label] = {f"{az:g} deg": _r(R.tr37885_antenna_gain_dbi(az, 90.0, directional), 3)
                      for az in (0, 30, 60, 90, 120, 150, 180)}
    db = R.two_ray_breakpoint_m(R.V2X_ANTENNA_HEIGHT_M, R.V2X_ANTENNA_HEIGHT_M)
    base_slope = R.TR37885_PATHLOSS["urban_los"][1]
    two_ray = {f"{d:g} m": _r(R.two_ray_excess_db(d, db, base_slope,
                                                  R.TWO_RAY_SLOPE_DB_PER_DECADE), 3)
               for d in (100, 150, 177, 200, 300, 400, 500, 700)}
    nlosv = {}
    for d in (10, 20, 26, 50, 100, 200, 300, 541, 700):
        nlosv[f"{d:g} m"] = {
            # keyed by the TR 37.885 case the geometry resolves to, which at the corrected 1.6 m
            # antenna height is Case 3 for a car/motorcycle and Case 2 for a truck/bus
            "tr37885_case3_car_mu_db": _r(
                R.tr37885_nlosv_mu_db(R.TR37885_NLOSV["one_below"][0], float(d)), 3),
            "tr37885_case2_truck_mu_db": _r(
                R.tr37885_nlosv_mu_db(R.TR37885_NLOSV["both_below"][0], float(d)), 3),
        }
    rho = {}
    for dt in (0.1, 1.0):
        rho[f"dt={dt} s"] = {f"{v:g} m/s": _r(R.jakes_rho(v, dt), 4)
                             for v in (0.0, 0.01, 0.1, 0.5, 1.0, 5.0, 30.0)}
    return {"antenna_element_gain_dbi_at_horizon": ant,
            "antenna_note": ("This is ONE end's element gain. evaluate_raw adds BOTH ends, so a "
                             "car-car link gains 2x the 0 deg value regardless of bearing."),
            "two_ray_breakpoint_m": _r(db, 2),
            "two_ray_excess_db": two_ray,
            "nlosv_mean_excess_db": nlosv,
            "tr37885_nlosv_sigma_db": {"case3_car": _r(R.TR37885_NLOSV["one_below"][1], 3),
                                       "case2_truck": _r(R.TR37885_NLOSV["both_below"][1], 3)},
            "blocker_half_width_m": {"car/motorcycle (1.6 m body)":
                                     _r(R.tr37885_blocker_half_width_m(1.6), 3),
                                     "truck/bus (3.0 m body)":
                                     _r(R.tr37885_blocker_half_width_m(3.0), 3),
                                     "shipped uniform": _r(R.GEO_BLOCKER_HALF_WIDTH_M, 3)},
            "jakes_rho": rho,
            "tx_power_semantics": (
                "CORRECTED 2026-09-07, and it inverts what this field used to say. TR 37.885 Table "
                "6.1.1-1 lists 'UE Tx power -- Vehicle/pedestrian UE or UE type RSU: 23dBm' in the "
                "same column as 'Macro BS: 49dBm', which is a macro's CONDUCTED PA power, and "
                "gives the element gain separately in Table 6.1.4-8 (3 dBi). The only 'e.i.r.p.' "
                "in the document is clause 5's 63-64 GHz regulatory survey. So 23 dBm is CONDUCTED, "
                "the conformant V2V budget is 23 + 3 - PL + 3 = 29 - PL, and the antenna term "
                "CLOSES a 6 dB gap rather than opening a 3 dB double count. The caveat this repo "
                "genuinely carries: run.py justifies the 23 in ETSI EIRP terms as well, so two "
                "conventions do meet on one number -- see the refdata entry "
                "link_budget_is_6db_below_tr37885_by_default."),
            "default_link_budget_shortfall_db": 2.0 * R.TR37885_ANT_MAX_GAIN_DBI,
            }


def cmd_terms(args) -> int:
    print(json.dumps(term_budget(), indent=1))
    return 0


def cmd_blindness(args) -> int:
    print(json.dumps(analytic_blindness(), indent=1))
    return 0


def cmd_render(args) -> int:
    with open(args.json_path, encoding="utf-8") as fh:
        rep = json.load(fh)
    print("\n".join(render(rep)))
    return 0


# ============================================================================================= #
# the collusion oracle: is a FABRICATED rssi_dbm distinguishable from a genuine one?
# ============================================================================================= #
#: (label, link length m, blocker body height m or None). Chosen so that no channel term is inert in
#: all three: the breakpoint does not switch on below 201.5 m, and the blockage draw needs a blocker.
COLLUSION_GEOMETRIES = (("A d=150 LOS", 150.0, None),
                        ("B d=300 LOS", 300.0, None),
                        ("C d=150 NLOSv", 150.0, 3.0))


def collusion_offset(n_pairs: int = 20000, seed: int = 3) -> dict:
    """Genuine vs fabricated received power on the SAME true links, per arm and geometry.

    A colluder's fabricated misbehaviour report carries an `rssi_dbm` it never measured. If the
    synthesis that produces it and the reception loop compute the link budget separately, the
    difference is a free classifier on the dataset. This measures that difference directly: `n`
    independent pairs 5 km apart (so each carries its own shadowing stream and no pair is in any
    other's way), the genuine column from `evaluate_raw` conditioned on decoding, the fabricated
    column from `synthesize_rx_dbm` drawn on a stream of its own.
    """
    from scms_sim_ref.api.channel import StationSnapshot, StepFrame
    from scms_sim_ref.mock_pipeline import PipelineConfig, validate_config

    ah = R.V2X_ANTENNA_HEIGHT_M
    car = R.TR37885_BLOCKER_HEIGHT_M["car"]
    out: dict = {"n_pairs": n_pairs, "antenna_height_m": ah, "geometries": {}}
    for label, d, blocker_h in COLLUSION_GEOMETRIES:
        rows = {}
        # the shipped arms, plus two PARAMETER sweeps of the same terms: the antenna pattern's
        # SHAPE without its absolute gain, and a second breakpoint slope. A gap can hide in a
        # parameter as easily as in a term, and neither is a shippable arm.
        arms = dict(ARMS)
        arms["antenna_gain0"] = {"radio_antenna_pattern": "tr37885_opt1",
                                 "radio_antenna_gain_dbi": 0.0}
        arms["breakpoint_slope28"] = {"radio_breakpoint": "two_ray",
                                      "radio_breakpoint_slope_db_per_decade": 28.0}
        for arm, over in arms.items():
            if "radio_tx_power_dbm" in over:            # a different budget, not a different term
                continue
            cfg = PipelineConfig(seed=seed, radio_model="geometric",
                                 radio_nlosb_density_per_km=0.0, **over)
            validate_config(cfg)
            ch = R.GeometricChannel(cfg, buildings=None, dt=1.0)

            def stations(step):
                sts = {}
                for k in range(n_pairs):
                    x0 = k * 5000.0 + step * 10.0
                    sts[3 * k] = StationSnapshot(3 * k, x0, 0.0, ah, car, False, False, None, 0.0)
                    sts[3 * k + 1] = StationSnapshot(3 * k + 1, x0 + d, 0.0, ah, car,
                                                     False, False, None, 0.0)
                    if blocker_h:
                        sts[3 * k + 2] = StationSnapshot(3 * k + 2, x0 + d / 2.0, 0.0, ah,
                                                         blocker_h, False, False, None, 0.0)
                return sts

            for step in (0, 1):
                ch.begin_step(StepFrame(step, float(step), 1.0, stations(step), (), (), 0.0, {}))
            place = {v: s.x for v, s in stations(1).items()}
            gen, fab, states = [], [], set()
            for k in range(n_pairs):
                a, b = 3 * k, 3 * k + 1
                heard, rssi, state, _ = ch.evaluate_raw(a, b, place[a], 0.0, place[b], 0.0,
                                                        d, ah, ah)
                states.add(state)
                if heard:
                    gen.append(rssi)
                fab.append(ch.synthesize_rx_dbm(a, b, place[a], 0.0, place[b], 0.0, d,
                                                random.Random(f"fab:{arm}:{k}")))
            mg, mf = statistics.fmean(gen), statistics.fmean(fab)
            rows[arm] = {"state": sorted(states), "genuine_mean_dbm": _r(mg, 3),
                         "genuine_sd_db": _r(statistics.pstdev(gen), 3),
                         "fabricated_mean_dbm": _r(mf, 3), "offset_db": _r(mf - mg, 3),
                         "n_genuine": len(gen)}
        out["geometries"][label] = rows
    return out


def cmd_collusion(args) -> int:
    rep = collusion_offset(n_pairs=args.pairs)
    print(json.dumps(rep, indent=1))
    worst = max(abs(r["offset_db"]) for g in rep["geometries"].values() for r in g.values())
    print(f"# worst |offset| across every arm and geometry: {worst:.3f} dB", file=sys.stderr)
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    m = sub.add_parser("measure", help="replay a dataset's scene through each arm's channel")
    m.add_argument("dataset")
    m.add_argument("--arms", default="all")
    m.add_argument("--budget", type=int, default=DEFAULT_EVAL_BUDGET)
    m.add_argument("--max-steps", type=int, default=0)
    m.add_argument("--label", default="")
    m.add_argument("--json", default=None)
    m.add_argument("--progress", action="store_true")
    m.set_defaults(fn=cmd_measure)

    c = sub.add_parser("cost", help="time the real engine, one run per arm")
    c.add_argument("--base-config", required=True)
    c.add_argument("--out-root", required=True)
    c.add_argument("--arms", default="all")
    c.add_argument("--timeout", type=float, default=7200.0)
    c.add_argument("--json", default=None)
    c.set_defaults(fn=cmd_cost)

    b = sub.add_parser("blindness", help="show that awareness.propagation_pdr cannot see the terms")
    b.set_defaults(fn=cmd_blindness)

    tb = sub.add_parser("terms", help="what each term does to one link's budget, in dB")
    tb.set_defaults(fn=cmd_terms)

    r = sub.add_parser("render", help="re-render a saved measure JSON")
    r.add_argument("json_path")
    r.add_argument("--markdown", action="store_true")
    r.set_defaults(fn=cmd_render)

    co = sub.add_parser("collusion",
                        help="genuine vs FABRICATED rssi on the same links, per arm and geometry")
    co.add_argument("--pairs", type=int, default=20000)
    co.set_defaults(fn=cmd_collusion)

    args = p.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())
