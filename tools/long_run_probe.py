"""Measure what BREAKS as a simulated run gets long.

Everything in this repository was calibrated on 60--300 s windows. That is shorter than every
timescale the domain actually has: pseudonym rotation, CRL growth, certificate validity, attacker
duty cycles, a city's demand profile, and -- the one that already bit us -- the misbehaviour
authority's cumulative false-positive rate. `docs/realism/TRAFFIC-PANEL-SURVIVORSHIP.md` is the
record of a defect that was *invisible at 300 s* and deleted 56% of the traffic over an InTAS peak
hour. This tool exists on the assumption that there are more like it.

It runs ONE scenario at a LADDER of durations and reports every quantity as a FUNCTION of duration:

  cost         wall clock per simulated second, peak working set, output bytes -- split into the
               three phases a run actually has (setup / step loop / finalisation), because they do
               NOT scale alike and only the wall-clock total hides that;
  determinism  the same seed twice at each rung (`--repeat`), comparing `data_digest_sha256` AND
               every output file's SHA-256, not just the digest;
  metrics      the realism-harness traffic + comm panels at each rung (`--bench`);
  SCMS         CRL growth, pseudonym issuance, certificate validity, attacker lifetime and the MA's
               cumulative precision (`--curves`) -- each as a CURVE IN TIME read out of a single
               run, not one number per run, so a plateau can be told apart from a trend;
  survivorship the surviving vehicle-step fraction at every rung, from `manifest.counts`.

Four things it deliberately does that a naive timing harness does not:

1.  **It splits the phases** from the engine's own progress lines, timestamped as they arrive.
    `run_pipeline` materialises the WHOLE vehicle population before step 0 and runs an O(R*C) plus
    O(R^2) CRL sanity check after the last step; a single wall-clock number hides both.

2.  **It reads curves out of ONE run.** Comparing the 300 s rung's precision against the 3600 s
    rung's confounds "the metric drifts" with "these are different samples". Bucketing one run's
    revocations by time separates them -- and because a shorter run of this scenario is an exact
    PREFIX of a longer one (verified: 3,590/3,590 identical `gt_vehicle` rows, 960/960 revocations
    at the same instant), the curves from different rungs must coincide, which is the check.

3.  **It isolates the terms it blames.** `--memory-ab` re-runs the same duration with the
    population capped, `--gc-ab` re-runs it with the cyclic collector off, and
    `--crl-assert-cost` times the quadratic finalisation term alone on synthetic devices. Each is
    a controlled experiment rather than an inference from one curve.

4.  **It checks that the ladder IS a ladder.** `--scenario ref_grid_rush` exists to show that under
    `--demand rush` it is not: the profile is a function of `t / total_time`, so two durations are
    two different scenarios (11 of 278 vehicles survive the change).

Usage
-----
    . C:/Users/Administrator/tools/env.ps1
    $env:PYTHONPATH = "$PWD\\src"; $env:PYTHONHASHSEED = "0"
    python tools/long_run_probe.py --durations 300,1800,3600,7200 --repeat --bench --curves \\
        --out-root C:/Temp/longrun --json .realism_cache/longrun/ladder.json --markdown

Results and the argument they support: `docs/realism/LONG-RUNS.md`.

Owns nothing in `src/`. It shells out to `scms_sim_ref.mock_pipeline.run` and imports
`scms_sim_ref.datagen.realism_bench` read-only, so it can never move a digest.
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import time
from collections import Counter

# --------------------------------------------------------------------------- #
# The scenario. The repository's own reference arm, verbatim, so every rung is directly
# comparable with the false-positive sweep in TRAFFIC-PANEL-SURVIVORSHIP.md section 6 and with the
# pinned golden. `--emit-mobility-oracle` is ON because the whole point is to measure traffic at
# length, and the emission stream is the truncated one; its byte cost is reported separately.
# --------------------------------------------------------------------------- #
SCENARIOS: dict[str, list[str]] = {
    "ref_grid": ["--flow", "--road", "grid", "--grid", "6", "--arrival-rate", "2",
                 "--attacker-pct", "0.15", "--traffic-lights", "--seed", "42"],
    # Same, with pseudonym rotation armed. Rotation is a per-VEHICLE timescale, so this arm exists
    # to show that -- and to show what it costs the certificate files.
    "ref_grid_rot": ["--flow", "--road", "grid", "--grid", "6", "--arrival-rate", "2",
                     "--attacker-pct", "0.15", "--traffic-lights", "--seed", "42",
                     "--rotate-period", "60"],
    # Density held where the MA saturates, to check whether the duration story changes shape when
    # precision is already at its floor.
    "ref_grid_dense": ["--flow", "--road", "grid", "--grid", "6", "--arrival-rate", "8",
                       "--attacker-pct", "0.15", "--traffic-lights", "--seed", "42"],
    # `--demand rush` evaluates its profile at `tt / total_time`, so this arm exists to show that
    # the rung ladder STOPS being a controlled experiment the moment the demand profile is not
    # uniform: two durations are then two different scenarios, not one scenario seen for longer.
    "ref_grid_rush": ["--flow", "--road", "grid", "--grid", "6", "--arrival-rate", "2",
                      "--attacker-pct", "0.15", "--traffic-lights", "--seed", "42",
                      "--demand", "rush"],
}

PROGRESS_RE = re.compile(r"\[flow t=(\d+(?:\.\d+)?)/(\d+(?:\.\d+)?)s\]\s+active=(\d+)\s+"
                         r"spawned=(\d+)\s+reports=(\d+)\s+revoked=(\d+)")
DIGEST_RE = re.compile(r"^data_digest=([0-9a-f]{64})\s*$")
DETECT_RE = re.compile(r"^detection: precision=([\d.]+) recall=([\d.]+) attackers=(\d+) "
                       r"revoked=(\d+) latency_med=([\d.na]+)")


# --------------------------------------------------------------------------- #
# Peak working set of a CHILD process, exactly, on Windows.
#
# psutil is not installed in this toolchain and polling `Get-Process` both misses the peak and
# cannot see a process that has already exited. `GetProcessMemoryInfo` reports PeakWorkingSetSize
# and PeakPagefileUsage for a handle that is still open EVEN AFTER the process has terminated, so
# `subprocess.Popen`'s own handle gives the true peak with no sampling error and no polling cost.
# --------------------------------------------------------------------------- #
class _PROCESS_MEMORY_COUNTERS(ctypes.Structure):
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t)]


def peak_memory(handle) -> dict:
    """{peak_working_set, peak_pagefile} in bytes for a (possibly exited) process handle."""
    if os.name != "nt":
        return {}
    ctr = _PROCESS_MEMORY_COUNTERS()
    ctr.cb = ctypes.sizeof(ctr)
    ok = ctypes.windll.psapi.GetProcessMemoryInfo(
        wt.HANDLE(int(handle)), ctypes.byref(ctr), ctr.cb)
    if not ok:
        return {}
    return {"peak_working_set_bytes": int(ctr.PeakWorkingSetSize),
            "peak_pagefile_bytes": int(ctr.PeakPagefileUsage),
            "page_faults": int(ctr.PageFaultCount)}


# --------------------------------------------------------------------------- #
# One run, timestamped
# --------------------------------------------------------------------------- #
#: Launch the engine through a one-liner that disables the cyclic collector first. Nothing about
#: the run changes -- no output, no RNG draw, no digest -- but the generational collector stops
#: walking a heap that grows with the run. See `gc_ab`.
_GC_OFF_STUB = ("import gc, sys; gc.disable(); "
                "from scms_sim_ref.mock_pipeline.run import main; sys.exit(main(sys.argv[1:]))")


def run_once(scenario: str, duration: float, out_dir: str, *, extra: list[str] | None = None,
             oracle: bool = True, quiet: bool = False, gc_off: bool = False) -> dict:
    """Launch the engine, timestamp its progress lines, return timing + memory + digest.

    The progress lines are the only visibility into the phase split without touching `run.py`.
    They are emitted at `step % (n_steps//20) == 0` inside the step loop, so:

      setup_s   = t(first progress line) - t(launch)   -- import, network build, and the
                  materialisation of the ENTIRE vehicle population (see `--phases` in the doc)
      loop_s    = t(last progress line) - t(first)     -- 19/20 of the step loop
      final_s   = t(exit) - t(last progress line)      -- the last 1/20 of the loop PLUS the
                  end-of-run CRL sanity check, cert-status build, sort, write and digest
    """
    launch = ([sys.executable, "-c", _GC_OFF_STUB] if gc_off
              else [sys.executable, "-m", "scms_sim_ref.mock_pipeline.run"])
    argv = [*launch, *SCENARIOS[scenario], "--duration", str(duration), "--out", out_dir]
    if oracle:
        argv.append("--emit-mobility-oracle")
    argv += list(extra or [])

    env = dict(os.environ)
    env["PYTHONHASHSEED"] = "0"          # the repo's own discipline (run.ps1 / conftest.py)
    env.setdefault("PYTHONPATH", os.path.join(os.getcwd(), "src"))

    t0 = time.perf_counter()
    marks: list[tuple[float, dict]] = []
    digest = None
    detect = None
    tail: list[str] = []
    p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                         text=True, encoding="utf-8", errors="replace", bufsize=1, env=env)
    for line in p.stdout:
        now = time.perf_counter() - t0
        line = line.rstrip("\n")
        tail.append(line)
        if len(tail) > 40:
            tail.pop(0)
        m = PROGRESS_RE.search(line)
        if m:
            marks.append((now, {"t": float(m.group(1)), "active": int(m.group(3)),
                                "spawned": int(m.group(4)), "reports": int(m.group(5)),
                                "revoked": int(m.group(6))}))
            if not quiet and len(marks) % 5 == 0:
                print(f"    [{duration:>6.0f}s] sim t={m.group(1)}s  wall={now:6.1f}s  "
                      f"active={m.group(3)} revoked={m.group(6)}", flush=True)
            continue
        md = DIGEST_RE.match(line)
        if md:
            digest = md.group(1)
        mdet = DETECT_RE.match(line)
        if mdet:
            detect = {"precision": float(mdet.group(1)), "recall": float(mdet.group(2)),
                      "attackers": int(mdet.group(3)), "revoked": int(mdet.group(4))}
    p.stdout.close()
    rc = p.wait()
    wall = time.perf_counter() - t0
    mem = peak_memory(p._handle) if os.name == "nt" else {}
    try:
        p._handle.Close()
    except Exception:
        pass

    if rc != 0:
        raise RuntimeError(f"engine exited {rc} for duration={duration}\n" + "\n".join(tail))

    setup_s = marks[0][0] if marks else None
    loop_s = (marks[-1][0] - marks[0][0]) if len(marks) > 1 else None
    final_s = (wall - marks[-1][0]) if marks else None
    return {"argv": argv, "duration_s": duration, "wall_s": wall,
            "setup_s": setup_s, "loop_s": loop_s, "final_s": final_s,
            "progress": [{"wall_s": w, **d} for w, d in marks],
            "data_digest": digest, "detection_stdout": detect, **mem}


# --------------------------------------------------------------------------- #
# Artifact accounting
# --------------------------------------------------------------------------- #
def file_sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for blk in iter(lambda: fh.read(1 << 20), b""):
            h.update(blk)
    return h.hexdigest()


def artifact_bytes(out_dir: str) -> dict:
    per = {}
    for root, _dirs, files in os.walk(out_dir):
        for f in files:
            p = os.path.join(root, f)
            rel = os.path.relpath(p, out_dir).replace("\\", "/")
            per[rel] = os.path.getsize(p)
    return {"total_bytes": sum(per.values()), "per_file": dict(sorted(per.items()))}


def artifact_hashes(out_dir: str) -> dict:
    out = {}
    for root, _dirs, files in os.walk(out_dir):
        for f in files:
            if f == "manifest.json":
                continue                  # carries build_utc; excluded from data_digest too
            p = os.path.join(root, f)
            out[os.path.relpath(p, out_dir).replace("\\", "/")] = file_sha256(p)
    return out


def read_manifest(out_dir: str) -> dict:
    with open(os.path.join(out_dir, "manifest.json"), encoding="utf-8") as fh:
        return json.load(fh)


def jsonl(path: str, limit: int | None = None):
    if not os.path.exists(path):
        return
    with open(path, encoding="utf-8") as fh:
        for i, line in enumerate(fh):
            if limit is not None and i >= limit:
                return
            line = line.strip()
            if line:
                yield json.loads(line)


# --------------------------------------------------------------------------- #
# The SCMS curves, read out of ONE run
#
# This is the part that a ladder of separate runs cannot give you. Every quantity below is a
# function of SIMULATED time inside a single artifact, so "precision falls with duration" can be
# distinguished from "the 3600 s rung happens to be a denser sample".
# --------------------------------------------------------------------------- #
def scms_curves(out_dir: str, duration: float, n_buckets: int = 24) -> dict:
    gt = os.path.join(out_dir, "ground_truth")
    ma = os.path.join(out_dir, "ma")

    # --- revocation outcomes in time -------------------------------------------------------- #
    # gt_linkage_revocation carries `true_revocation_time` and `should_have_been_revoked`
    # (== the subject really was an attacker), which is exactly TP/FP per revocation event.
    revs = [(float(r["true_revocation_time"]), bool(r["should_have_been_revoked"]))
            for r in jsonl(os.path.join(gt, "gt_linkage_revocation.jsonl"))
            if r.get("true_revocation_time") is not None]
    revs.sort()
    n_att = sum(1 for r in jsonl(os.path.join(gt, "gt_vehicle.jsonl")) if r.get("is_attacker"))

    edges = [duration * (i + 1) / n_buckets for i in range(n_buckets)]
    cum_tp = cum_fp = 0
    idx = 0
    precision_curve = []
    for e in edges:
        while idx < len(revs) and revs[idx][0] <= e:
            if revs[idx][1]:
                cum_tp += 1
            else:
                cum_fp += 1
            idx += 1
        tot = cum_tp + cum_fp
        precision_curve.append({
            "t_s": round(e, 1), "revocations": tot, "tp": cum_tp, "fp": cum_fp,
            "precision": (cum_tp / tot) if tot else None,
            "recall_of_all_attackers": (cum_tp / n_att) if n_att else None,
            "fp_per_tp": (cum_fp / cum_tp) if cum_tp else None,
        })

    # --- CRL growth ------------------------------------------------------------------------- #
    # ma_crl_events.num_entries is the CRL length AFTER the event, so the last event in each
    # bucket is the CRL size at that time. Entries are never removed anywhere in run.py, which is
    # itself the finding: this CRL is monotone by construction.
    crl = sorted((float(r["issue_time"]), int(r["num_entries"]))
                 for r in jsonl(os.path.join(ma, "ma_crl_events.jsonl")))
    crl_curve, j, last = [], 0, 0
    for e in edges:
        while j < len(crl) and crl[j][0] <= e:
            last = crl[j][1]
            j += 1
        crl_curve.append({"t_s": round(e, 1), "crl_entries": last})

    # --- certificates: what was ISSUED vs what the MA is TOLD ------------------------------- #
    idmap = list(jsonl(os.path.join(gt, "gt_identity_map.jsonl")))
    per_veh = Counter(r["true_vehicle_id"] for r in idmap)
    issued_spans = [float(r["valid_to"]) - float(r["valid_from"]) for r in idmap]
    cert_rows = list(jsonl(os.path.join(ma, "ma_cert_status.jsonl")))
    ma_spans = [float(r["valid_to"]) - float(r["valid_from"]) for r in cert_rows]

    def stats(xs):
        if not xs:
            return None
        xs = sorted(xs)
        n = len(xs)
        return {"n": n, "min": round(xs[0], 3), "p50": round(xs[n // 2], 3),
                "max": round(xs[-1], 3), "mean": round(sum(xs) / n, 3)}

    certs = {
        "issued_total": len(idmap),
        "certs_per_vehicle_max": max(per_veh.values()) if per_veh else 0,
        "certs_per_vehicle_mean": round(sum(per_veh.values()) / len(per_veh), 4) if per_veh else 0,
        "issued_validity_span_s": stats(issued_spans),
        "ma_visible_validity_span_s": stats(ma_spans),
        # The MA-visible row is written with valid_from=0.0, valid_to=total_time for EVERY cert
        # (run.py `_cert_status_row`). So this ratio is 1.0 iff the MA is being told the truth.
        "ma_span_equals_duration_frac": (
            round(sum(1 for s in ma_spans if abs(s - duration) < 1e-6) / len(ma_spans), 6)
            if ma_spans else None),
        "issued_span_over_duration_p50": (
            round(stats(issued_spans)["p50"] / duration, 6) if issued_spans else None),
    }

    # --- lifetime of a revoked identity on the CRL ------------------------------------------ #
    # A revocation entry is added at t and never expires, so its residency is duration - t. On a
    # long run the mean residency is what a real CRL distribution service would have to carry.
    residency = [duration - t for t, _ in revs]

    return {"n_attackers": n_att, "n_revocations": len(revs),
            "precision_curve": precision_curve, "crl_curve": crl_curve,
            "certificates": certs, "crl_entry_residency_s": stats(residency),
            "revocation_times": [round(t, 2) for t, _ in revs[:0]]}   # times kept out of the JSON


def population_curves(out_dir: str, duration: float, n_buckets: int = 24) -> dict:
    """The two timescales that are set by the RUN rather than by the world.

    **Attack span.** In flow mode `run.py` sets `v.attack_to = spawn_time + life`, so an attacker's
    hostile window is one TRIP. A longer run therefore does not contain a longer-lived adversary;
    it contains more short ones. Anything that needs a persistent attacker -- a slow-drift
    falsification, a duty cycle whose period exceeds a trip, an adversary that waits out a
    detection window -- is unrepresentable at ANY duration, and this measures by how much.

    **Demand shape.** `demand_mult(frac)` is evaluated at `frac = tt / total_time`, so `--demand
    rush` places its peaks at 25% and 75% OF THE RUN with sigma = 9% OF THE RUN. The profile has no
    clock. Two runs of different length are different scenarios, not the same scenario sampled
    longer, and the spawn histogram below is what shows it.
    """
    gt = os.path.join(out_dir, "ground_truth")
    spans = []
    for r in jsonl(os.path.join(gt, "gt_attacks.jsonl")):
        try:
            spans.append(float(r["end_time"]) - float(r["start_time"]))
        except (KeyError, TypeError, ValueError):
            continue
    spawn = sorted(float(r["spawn_time"]) for r in jsonl(os.path.join(gt, "gt_vehicle.jsonl"))
                   if r.get("spawn_time") is not None)
    hist = [0] * n_buckets
    for s in spawn:
        b = min(n_buckets - 1, int(n_buckets * s / duration)) if duration > 0 else 0
        hist[b] += 1

    def stats(xs):
        if not xs:
            return None
        xs = sorted(xs)
        n = len(xs)
        return {"n": n, "min": round(xs[0], 3), "p50": round(xs[n // 2], 3),
                "p95": round(xs[min(n - 1, int(0.95 * n))], 3), "max": round(xs[-1], 3),
                "mean": round(sum(xs) / n, 3)}

    sp = stats(spans)
    return {
        "attack_span_s": sp,
        "attack_span_over_duration_p50": round(sp["p50"] / duration, 6) if sp else None,
        "attack_span_over_duration_max": round(sp["max"] / duration, 6) if sp else None,
        "spawn_histogram": hist,
        "spawn_bucket_s": round(duration / n_buckets, 2),
        # peak-to-trough of the arrival histogram: 1.0 for a flat profile, large for `rush`
        "spawn_peak_over_trough": (round(max(hist) / min(hist), 4)
                                   if hist and min(hist) > 0 else None),
    }


# --------------------------------------------------------------------------- #
# Harness metrics
# --------------------------------------------------------------------------- #
#: The rows the markdown table prints, in the order it prints them. `bench()` keeps EVERY row --
#: this is presentation only, so a metric appearing or disappearing between rungs is still in the
#: JSON. Ids are `realism_bench`'s own (`panel.metric`).
_PICK = ("traffic.trace_segments",
         "traffic.speed_p50_mps", "traffic.speed_p95_mps", "traffic.speed_max_mps",
         "traffic.moving_vehicle_frac", "traffic.headway_p50_s",
         "traffic.headway_ks_shifted_exponential", "traffic.headway_below_floor_frac",
         "traffic.fd_capacity_veh_h_lane", "traffic.fd_backward_wave_speed_kmh",
         "traffic.overlap_events", "traffic.teleport_events",
         "traffic.lateral_discontinuity_events", "traffic.accel_within_comfort_frac",
         "traffic.accel_within_hard_bound_frac",
         "traffic.survivorship_vehicle_steps_frac", "traffic.revoked_vehicle_frac",
         "comm.honest_links", "comm.awareness_ratio_100m", "comm.awareness_ratio_200m",
         "comm.awareness_ratio_300m", "comm.pdr_absolute_200m", "comm.effective_range_m",
         "comm.pdr_gray_zone_width_m", "comm.pdr_gray_zone_ratio",
         "comm.nar90_equivalent_range_m", "comm.cam_inter_packet_gap_p50_s",
         "comm.link_state_los_fraction")


def bench(out_dir: str, regime: str = "urban", traffic_source: str = "auto") -> dict:
    from scms_sim_ref.datagen import realism_bench as rb
    card = rb.scorecard(out_dir, regime=regime, traffic_source=traffic_source)
    flat = {}
    for m in card["panels"]["traffic"] + card["panels"]["comm"]:
        flat[m["id"]] = {"value": m.get("value"), "status": m.get("status"), "n": m.get("n"),
                         "reason": m.get("reason")}
    return {"metrics": flat,
            "traffic_source": card.get("traffic_source"),
            "survivorship": card.get("survivorship"),
            "summary": card.get("summary"),
            "hard_failures": rb.hard_failures(card)}


def prefix_bench(out_dir: str, cut_s: float, work_dir: str, regime: str = "urban") -> dict:
    """Score the FIRST `cut_s` seconds of an existing run's ORACLE record.

    The point of this is control. Comparing rung to rung changes the traffic AND the window at the
    same time; this changes only the window. It copies the oracle record truncated at `cut_s` into
    a shadow directory beside the real one, with the same manifest, and scores that.
    """
    src = os.path.join(out_dir, "ground_truth", "gt_mobility_oracle.jsonl")
    if not os.path.exists(src):
        return {"error": "no oracle record"}
    os.makedirs(os.path.join(work_dir, "ground_truth"), exist_ok=True)
    os.makedirs(os.path.join(work_dir, "ma"), exist_ok=True)
    kept = 0
    with open(src, encoding="utf-8") as fin, \
            open(os.path.join(work_dir, "ground_truth", "gt_mobility_oracle.jsonl"), "w",
                 encoding="utf-8", newline="\n") as fout:
        for line in fin:
            if not line.strip():
                continue
            # `t` is the second field of the canonical row; parse cheaply, fall back to json.
            try:
                t = float(line.split('"t":', 1)[1].split(",", 1)[0].rstrip("}"))
            except Exception:
                t = float(json.loads(line)["t"])
            if t > cut_s:
                break
            fout.write(line)
            kept += 1
    # the panel needs a manifest and the comm inputs; symlink-free copy of the small ones
    import shutil
    for rel in ("manifest.json",):
        shutil.copy2(os.path.join(out_dir, rel), os.path.join(work_dir, rel))
    for rel in ("ground_truth/gt_vehicle.jsonl", "ground_truth/gt_attacks.jsonl",
                "ground_truth/gt_identity_map.jsonl", "ground_truth/gt_linkage_revocation.jsonl"):
        s = os.path.join(out_dir, rel)
        if os.path.exists(s):
            shutil.copy2(s, os.path.join(work_dir, rel))
    out = bench(work_dir, regime=regime, traffic_source="oracle")
    out["rows_kept"] = kept
    out["cut_s"] = cut_s
    return out


# --------------------------------------------------------------------------- #
# Memory attribution: WHAT accumulates
# --------------------------------------------------------------------------- #
def attribute_memory(scenario: str, duration: float, out_dir: str, top: int = 20,
                     sample_s: float = 4.0) -> dict:
    """Run the engine IN-PROCESS under tracemalloc and name the top allocators AT PEAK.

    Deliberately separate from the ladder: tracemalloc costs 2-4x, so a timing rung must never
    carry it. Answers "if memory grows without bound, find what accumulates" by file and line of
    `run.py` rather than by inference from an RSS curve.

    The snapshot is taken from a sampling thread DURING the run and the largest one is kept. A
    snapshot taken after `main()` returns is useless here: `run_pipeline`'s locals -- which are the
    accumulators -- are exactly what has just been freed.
    """
    import threading
    import tracemalloc
    from scms_sim_ref.mock_pipeline import run as R

    argv = [*SCENARIOS[scenario], "--duration", str(duration), "--out", out_dir,
            "--emit-mobility-oracle"]
    best: dict = {"size": -1, "snap": None}
    trail: list[dict] = []
    stop = threading.Event()

    def sampler():
        # `get_traced_memory` is cheap; `take_snapshot` is NOT -- it walks the whole traced heap,
        # and on a run whose memory grows monotonically a "snapshot whenever it grew" rule fires
        # every sample and dominates the run. Snapshot only on a 1.3x growth step: that is
        # logarithmically many snapshots, and the last one is within 30% of the peak by
        # construction.
        while not stop.wait(sample_s):
            cur, _pk = tracemalloc.get_traced_memory()
            trail.append({"traced_bytes": cur})
            if cur > max(best["size"] * 1.3, 8 << 20):
                best["size"] = cur
                best["snap"] = tracemalloc.take_snapshot()

    tracemalloc.start(6)
    th = threading.Thread(target=sampler, daemon=True)
    th.start()
    try:
        R.main(argv)
    finally:
        stop.set()
        th.join(timeout=30)
        peak = tracemalloc.get_traced_memory()[1]
        tracemalloc.stop()

    snap = best["snap"]
    stats = snap.statistics("lineno")[:top] if snap is not None else []
    return {"traced_peak_bytes": peak, "snapshot_at_bytes": best["size"],
            "samples": len(trail), "trail": trail,
            "top": [{"where": str(s.traceback[0]), "size_bytes": s.size, "count": s.count}
                    for s in stats]}


def memory_ab(scenario: str, duration: float, out_root: str, cap: int) -> dict:
    """The controlled experiment behind the attribution.

    Two runs of the SAME duration -- so the same number of steps, the same amount of streamed
    output, the same detection state churn -- differing only in how many vehicles are ever created
    (`--max-total-vehicles`). If peak memory tracks the vehicle count rather than the step count,
    what grows is the POPULATION, which `run_pipeline` materialises in full before step 0 and never
    releases, not anything the loop does.
    """
    da = os.path.join(out_root, f"_ab_full_{int(duration)}")
    db = os.path.join(out_root, f"_ab_cap{cap}_{int(duration)}")
    a = run_once(scenario, duration, da, quiet=True)
    b = run_once(scenario, duration, db, extra=["--max-total-vehicles", str(cap)], quiet=True)
    a["counts"] = read_manifest(da).get("counts", {})
    b["counts"] = read_manifest(db).get("counts", {})
    return {
        "duration_s": duration, "cap": cap,
        "uncapped": {"vehicles": a["counts"].get("vehicles"),
                     "peak_mib": (a.get("peak_working_set_bytes") or 0) / (1 << 20),
                     "wall_s": a["wall_s"], "setup_s": a["setup_s"], "final_s": a["final_s"]},
        "capped": {"vehicles": b["counts"].get("vehicles"),
                   "peak_mib": (b.get("peak_working_set_bytes") or 0) / (1 << 20),
                   "wall_s": b["wall_s"], "setup_s": b["setup_s"], "final_s": b["final_s"]},
    }


def hash_cost(out_dir: str) -> dict:
    """What `_data_digest` + `manifest["outputs"]` cost on an existing dataset, and what they need to.

    `run.py`'s `_file_sha256` is `h.update(fh.read())` -- the whole file into one `bytes` object --
    and it is called ONCE PER FILE by `_data_digest` and AGAIN per file by `_write_manifest`, so
    every data file is read and hashed twice and the largest one is materialised in memory twice.
    This times the engine's shape against the streaming shape on the same bytes.
    """
    files = []
    for root, _d, fs in os.walk(out_dir):
        for f in fs:
            if f == "manifest.json":
                continue
            files.append(os.path.join(root, f))
    total = sum(os.path.getsize(p) for p in files)

    def whole(p):
        h = hashlib.sha256()
        with open(p, "rb") as fh:
            h.update(fh.read())
        return h.hexdigest()

    t0 = time.perf_counter()
    a = [whole(p) for p in files]        # _data_digest
    b = [whole(p) for p in files]        # manifest["outputs"]
    t_engine = time.perf_counter() - t0
    t0 = time.perf_counter()
    c = [file_sha256(p) for p in files]  # chunked, once, reused for both
    t_stream = time.perf_counter() - t0
    assert a == b == c
    return {"files": len(files), "bytes": total,
            "largest_file_bytes": max((os.path.getsize(p) for p in files), default=0),
            "engine_shape_s": round(t_engine, 3), "streamed_once_s": round(t_stream, 3),
            "speedup": round(t_engine / t_stream, 3) if t_stream else None,
            "engine_MB_per_s": round(2 * total / t_engine / 1e6, 1) if t_engine else None}


def gc_ab(scenario: str, duration: float, out_root: str) -> dict:
    """Is the STEP LOOP's growth with duration garbage collection?

    Python's generational collector runs a gen-2 pass every fixed number of gen-1 passes, and a
    gen-2 pass walks every live container. `run_pipeline` keeps every vehicle, every pseudonym and
    every per-certificate counter alive for the whole run, so the live set grows linearly with
    duration while the collection RATE stays constant -- which makes total GC time quadratic in
    duration even though the per-step work is constant.

    This runs the identical scenario twice, differing only in `gc.disable()` before the import. The
    output is byte-identical either way (the collector touches no value the engine reads), so the
    difference is the collector and nothing else. It also costs memory, which is measured here too.
    """
    da = os.path.join(out_root, f"_gc_on_{int(duration)}")
    db = os.path.join(out_root, f"_gc_off_{int(duration)}")
    a = run_once(scenario, duration, da, quiet=True)
    b = run_once(scenario, duration, db, quiet=True, gc_off=True)
    same = a["data_digest"] == b["data_digest"]
    out = {"duration_s": duration, "digest_identical": same,
           "gc_on": {k: a.get(k) for k in ("wall_s", "setup_s", "loop_s", "final_s",
                                           "peak_working_set_bytes", "data_digest")},
           "gc_off": {k: b.get(k) for k in ("wall_s", "setup_s", "loop_s", "final_s",
                                            "peak_working_set_bytes", "data_digest")}}
    if a.get("loop_s") and b.get("loop_s"):
        out["loop_speedup"] = round(a["loop_s"] / b["loop_s"], 4)
        out["gc_share_of_loop"] = round(1.0 - b["loop_s"] / a["loop_s"], 4)
    if a.get("peak_working_set_bytes") and b.get("peak_working_set_bytes"):
        out["memory_cost_mib"] = round(
            (b["peak_working_set_bytes"] - a["peak_working_set_bytes"]) / (1 << 20), 2)
    return out


def crl_assert_cost(revoked: int, certs: int, jmax: int = 20,
                    certs_per_vehicle: float = 1.05) -> dict:
    """Time `run.py`'s end-of-run CRL sanity check ALONE, at a stated (revoked, certs).

    The check is::

        for vid in revoked_vehicles:                       # O(R)
            for d in cert_first_seen:                      # O(C)   -- full scan per revoked vid
                if pseudonym_info[d]["veh_vid"] != vid: continue
                assert any(e.matches(...) for e in crl_entries)     # O(R) hash-chain calls

    so it is O(R*C) dict work plus O(R^2/2) `CrlLinkageEntry.matches` calls, and BOTH R and C are
    linear in duration. Reproduced here on synthetic devices with the same shape (i = 0, j =
    (vid + k) % jmax), which is all `matches` sees -- the cost does not depend on the seeds.

    Isolating it matters because it is invisible in a 300 s run (0.1 s) and dominant in an 8 h one.
    """
    import os as _os
    from scms_sim_ref.scms_core.linkage import CrlLinkageEntry, DeviceLinkageContext

    # A revoked vehicle triggers the assert once per certificate IT owns -- `certs / vehicles`,
    # not `certs / revoked`. With rotation off that is ~1.05 (Sybil ghosts are the only surplus);
    # with `--rotate-period 60` it is ~5, and the whole term scales with it.
    per_veh = max(1, int(round(certs_per_vehicle)))
    ctxs = [DeviceLinkageContext(1, 2, _os.urandom(16), _os.urandom(16)) for _ in range(revoked)]
    entries = [CrlLinkageEntry.from_device(c, 0, jmax) for c in ctxs]
    # one representative (i, j, lv) per revoked device, exactly what the assert passes
    subj = [(0, (v % jmax), ctxs[v].linkage_value_for(0, v % jmax)) for v in range(revoked)]

    t0 = time.perf_counter()
    calls = 0
    for v in range(revoked):
        for _k in range(per_veh):
            i, j, lv = subj[v]
            for e in entries:                       # first match wins, exactly like `any(...)`
                calls += 1
                if e.matches(i, j, lv):
                    break
    t_match = time.perf_counter() - t0

    # the O(R*C) scan that surrounds it: a dict lookup and an int compare per (revoked, cert) pair
    owner = {f"d{i}": i % max(1, revoked) for i in range(certs)}
    t0 = time.perf_counter()
    seen = 0
    for v in range(revoked):
        for d in owner:
            if owner[d] != v:
                continue
            seen += 1
    t_scan = time.perf_counter() - t0
    return {"revoked": revoked, "certs": certs, "certs_per_vehicle": per_veh,
            "matches_s": round(t_match, 3), "scan_s": round(t_scan, 3),
            "total_s": round(t_match + t_scan, 3), "matches_calls": calls,
            "us_per_match_call": round(1e6 * t_match / max(1, calls), 3), "scanned": seen}


# --------------------------------------------------------------------------- #
# Ladder driver
# --------------------------------------------------------------------------- #
def ladder(durations, scenario, out_root, *, repeat=False, do_bench=False, do_curves=False,
           regime="urban", oracle=True, quiet=False) -> dict:
    os.makedirs(out_root, exist_ok=True)
    rungs = []
    for d in durations:
        tag = f"{scenario}_{int(d)}"
        od = os.path.join(out_root, tag)
        print(f"[ladder] {scenario} duration={d:.0f}s -> {od}", flush=True)
        r = run_once(scenario, d, od, oracle=oracle, quiet=quiet)
        man = read_manifest(od)
        r["counts"] = man.get("counts", {})
        r["out_dir"] = od
        r["bytes"] = artifact_bytes(od)
        r["ms_per_sim_s"] = 1000.0 * r["wall_s"] / d
        r["bytes_per_sim_s"] = r["bytes"]["total_bytes"] / d
        if r.get("peak_working_set_bytes"):
            r["peak_mib"] = r["peak_working_set_bytes"] / (1 << 20)

        if repeat:
            od2 = od + "_rep"
            r2 = run_once(scenario, d, od2, oracle=oracle, quiet=True)
            h1, h2 = artifact_hashes(od), artifact_hashes(od2)
            diff = sorted(k for k in set(h1) | set(h2) if h1.get(k) != h2.get(k))
            r["repeat"] = {
                "wall_s": r2["wall_s"],
                "digest_match": r["data_digest"] == r2["data_digest"],
                "digest_a": r["data_digest"], "digest_b": r2["data_digest"],
                "files_compared": len(set(h1) | set(h2)),
                "files_differing": diff,
                "counts_match": read_manifest(od2).get("counts") == r["counts"],
                "wall_ratio": r2["wall_s"] / r["wall_s"] if r["wall_s"] else None,
                "peak_mib": (r2.get("peak_working_set_bytes") or 0) / (1 << 20),
            }
        if do_curves:
            r["scms"] = scms_curves(od, d)
            r["population"] = population_curves(od, d)
        if do_bench:
            t0 = time.perf_counter()
            try:
                r["bench"] = bench(od, regime=regime)
            except Exception as exc:                      # a panel that cannot score is a result
                r["bench"] = {"error": f"{type(exc).__name__}: {exc}"}
            r["bench_wall_s"] = time.perf_counter() - t0
        rungs.append(r)
        _dump_partial(out_root, scenario, rungs)
    return {"scenario": scenario, "argv_base": SCENARIOS[scenario], "oracle": oracle,
            "rungs": rungs}


def _dump_partial(out_root, scenario, rungs):
    p = os.path.join(out_root, f"_partial_{scenario}.json")
    with open(p, "w", encoding="utf-8") as fh:
        json.dump({"rungs": rungs}, fh, indent=1, default=str)


# --------------------------------------------------------------------------- #
# Reporting
# --------------------------------------------------------------------------- #
def _fit_exponent(xs, ys):
    """Least-squares slope of log(y) on log(x): the scaling exponent. 1.0 == linear."""
    pts = [(math.log(x), math.log(y)) for x, y in zip(xs, ys) if x > 0 and y > 0]
    if len(pts) < 2:
        return None
    n = len(pts)
    mx = sum(a for a, _ in pts) / n
    my = sum(b for _, b in pts) / n
    num = sum((a - mx) * (b - my) for a, b in pts)
    den = sum((a - mx) ** 2 for a, _ in pts)
    return round(num / den, 4) if den else None


def render(res: dict) -> list[str]:
    L = [f"# long-run ladder -- scenario `{res['scenario']}`", ""]
    rungs = res["rungs"]
    L.append("| duration s | wall s | ms/sim-s | setup s | loop s | final s | peak MiB | "
             "bytes | MB/sim-s | vehicles | revoked | survival |")
    L.append("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rungs:
        surv = (r["counts"].get("mobility_survivorship") or {})
        L.append("| {d:.0f} | {w:.1f} | {mps:.1f} | {s} | {lp} | {f} | {mem:.0f} | {b:,} | "
                 "{bs:.3f} | {v} | {rev} | {sv} |".format(
                     d=r["duration_s"], w=r["wall_s"], mps=r["ms_per_sim_s"],
                     s=f"{r['setup_s']:.1f}" if r["setup_s"] else "-",
                     lp=f"{r['loop_s']:.1f}" if r["loop_s"] else "-",
                     f=f"{r['final_s']:.1f}" if r["final_s"] else "-",
                     mem=r.get("peak_mib", 0.0), b=r["bytes"]["total_bytes"],
                     bs=r["bytes_per_sim_s"] / 1e6,
                     v=r["counts"].get("vehicles"), rev=r["counts"].get("revoked"),
                     sv=_fmt(surv.get("vehicle_steps_survival_frac"))))
    ds = [r["duration_s"] for r in rungs]
    L += ["", "scaling exponents (log-log slope vs duration; 1.0 == linear):",
          f"  wall clock      {_fit_exponent(ds, [r['wall_s'] for r in rungs])}",
          f"  peak memory     {_fit_exponent(ds, [r.get('peak_mib') or 0 for r in rungs])}",
          f"  output bytes    {_fit_exponent(ds, [r['bytes']['total_bytes'] for r in rungs])}"]
    fin = [r["final_s"] for r in rungs if r["final_s"]]
    if len(fin) == len(ds):
        L.append(f"  finalisation    {_fit_exponent(ds, fin)}")
    if any("repeat" in r for r in rungs):
        L += ["", "| duration s | digest reproduces | files differing |", "|---|---|---|"]
        for r in rungs:
            rp = r.get("repeat")
            if rp:
                L.append(f"| {r['duration_s']:.0f} | {rp['digest_match']} | "
                         f"{len(rp['files_differing'])} "
                         f"{'(' + ', '.join(rp['files_differing'][:4]) + ')' if rp['files_differing'] else ''} |")
    if any("bench" in r for r in rungs):
        keys = [k for k in _PICK
                if any((r.get("bench", {}).get("metrics") or {}).get(k) for r in rungs)]
        L += ["", "| metric | " + " | ".join(f"{r['duration_s']:.0f}s" for r in rungs) + " |",
              "|---" * (len(rungs) + 1) + "|"]
        for k in keys:
            cells = []
            for r in rungs:
                m = (r.get("bench", {}).get("metrics") or {}).get(k)
                if not m:
                    cells.append("-")
                else:
                    st = m.get("status")
                    cells.append(_fmt(m.get("value")) + ("" if st in (None, "na") else f" {st}"))
            L.append(f"| `{k}` | " + " | ".join(cells) + " |")

    if any("scms" in r for r in rungs):
        # the CUMULATIVE MA precision, read at the SAME simulated times out of every rung: under
        # uniform demand a shorter run is an exact prefix of a longer one, so the columns must
        # agree wherever they overlap, and a disagreement is itself the finding.
        L += ["", "cumulative MA precision at t (rows), per rung (columns):", "",
              "| t s | " + " | ".join(f"{r['duration_s']:.0f}s" for r in rungs) + " |",
              "|---" * (len(rungs) + 1) + "|"]
        marks = [300, 900, 1800, 3600, 7200, 14400, 28800, 43200, 86400]
        for t in marks:
            if t > max(r["duration_s"] for r in rungs):
                break
            cells = []
            for r in rungs:
                pt = None
                for p in (r.get("scms") or {}).get("precision_curve", []):
                    if abs(p["t_s"] - t) <= 0.51 * (r["duration_s"] / 24.0):
                        pt = p
                cells.append(f"{pt['precision']:.4f} ({pt['revocations']})" if pt else "-")
            L.append(f"| {t} | " + " | ".join(cells) + " |")
        L += ["", "| duration s | CRL entries | mean CRL residency s | certs issued | "
              "certs/veh | MA-visible validity == duration | attack span p50 s | "
              "attack span / duration |", "|---|---|---|---|---|---|---|---|"]
        for r in rungs:
            s = r.get("scms") or {}
            c = s.get("certificates") or {}
            pop = r.get("population") or {}
            asp = (pop.get("attack_span_s") or {})
            L.append(f"| {r['duration_s']:.0f} | {(s.get('crl_curve') or [{}])[-1].get('crl_entries')} | "
                     f"{(s.get('crl_entry_residency_s') or {}).get('mean')} | {c.get('issued_total')} | "
                     f"{c.get('certs_per_vehicle_mean')} | {c.get('ma_span_equals_duration_frac')} | "
                     f"{asp.get('p50')} | {pop.get('attack_span_over_duration_p50')} |")
    return L


def _fmt(v):
    if v is None:
        return "na"
    if isinstance(v, float):
        return f"{v:.4f}"
    return str(v)


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--durations", default="300,1800,3600",
                   help="comma-separated simulated seconds")
    p.add_argument("--scenario", default="ref_grid", choices=sorted(SCENARIOS))
    p.add_argument("--out-root", default=os.path.join("C:/Temp", "longrun"))
    p.add_argument("--repeat", action="store_true",
                   help="run each rung twice with the same seed and diff every file")
    p.add_argument("--bench", action="store_true", help="score each rung with realism_bench")
    p.add_argument("--curves", action="store_true",
                   help="extract the SCMS time-curves (CRL, precision, certs) from each rung")
    p.add_argument("--regime", default="urban", choices=("auto", "urban", "highway"))
    p.add_argument("--no-oracle", action="store_true",
                   help="omit --emit-mobility-oracle (measures the truncated-stream cost)")
    p.add_argument("--json", dest="json_out", default=None)
    p.add_argument("--markdown", action="store_true")
    p.add_argument("--quiet", action="store_true")
    p.add_argument("--attribute-memory", type=float, default=None, metavar="DURATION",
                   help="run once in-process under tracemalloc and name the top allocators")
    p.add_argument("--memory-ab", type=float, default=None, metavar="DURATION",
                   help="same duration, capped vs uncapped vehicle population (isolates WHAT grows)")
    p.add_argument("--memory-ab-cap", type=int, default=600,
                   help="--max-total-vehicles for the capped arm of --memory-ab")
    p.add_argument("--hash-cost", default=None, metavar="RUN_DIR",
                   help="time run.py's double whole-file SHA-256 against a single streamed pass")
    p.add_argument("--gc-ab", type=float, default=None, metavar="DURATION",
                   help="same run with and without the cyclic collector (isolates GC from the loop)")
    p.add_argument("--merge", default=None, metavar="A.json,B.json,...",
                   help="merge ladder JSONs written by separate invocations and render one report")
    p.add_argument("--crl-assert-cost", default=None, metavar="R:C[,R:C...]",
                   help="time run.py's end-of-run CRL sanity check alone at (revoked:certs) pairs")
    p.add_argument("--postprocess", default=None, metavar="OUT_ROOT",
                   help="re-read rung directories under OUT_ROOT and emit the curves only "
                        "(no simulation); pairs with --durations")
    p.add_argument("--prefix-bench", default=None, metavar="RUN_DIR",
                   help="score truncated prefixes of an existing run's oracle record")
    p.add_argument("--prefix-cuts", default="300,1800,3600",
                   help="prefix lengths (s) for --prefix-bench")
    a = p.parse_args(argv)

    if a.attribute_memory is not None:
        r = attribute_memory(a.scenario, a.attribute_memory,
                             os.path.join(a.out_root, f"_tracemalloc_{int(a.attribute_memory)}"))
        print(json.dumps(r, indent=2))
        if a.json_out:
            _write_json(a.json_out, r)
        return 0

    if a.merge:
        rungs = []
        for p_ in a.merge.split(","):
            p_ = p_.strip()
            if not p_ or not os.path.exists(p_):
                continue
            with open(p_, encoding="utf-8") as fh:
                rungs.extend(json.load(fh).get("rungs", []))
        seen, uniq = set(), []
        for r in sorted(rungs, key=lambda r: r["duration_s"]):
            if r["duration_s"] in seen:
                continue
            seen.add(r["duration_s"])
            uniq.append(r)
        res = {"scenario": a.scenario, "rungs": uniq}
        print("\n".join(render(res)))
        if a.json_out:
            _write_json(a.json_out, res)
        return 0

    if a.crl_assert_cost:
        rows = []
        for tok in a.crl_assert_cost.split(","):
            R, C = (int(x) for x in tok.split(":"))
            rows.append(crl_assert_cost(R, C))
            print(json.dumps(rows[-1]), flush=True)
        if a.json_out:
            _write_json(a.json_out, {"crl_assert_cost": rows})
        return 0

    if a.postprocess:
        out = []
        for d in [float(x) for x in a.durations.split(",") if x.strip()]:
            od = os.path.join(a.postprocess, f"{a.scenario}_{int(d)}")
            if not os.path.isdir(od):
                continue
            man = read_manifest(od)
            out.append({"duration_s": d, "out_dir": od, "counts": man.get("counts", {}),
                        "bytes": artifact_bytes(od),
                        "scms": scms_curves(od, d), "population": population_curves(od, d)})
        res = {"scenario": a.scenario, "rungs": out}
        if a.json_out:
            _write_json(a.json_out, res)
        else:
            print(json.dumps(res, indent=1, default=str))
        return 0

    if a.hash_cost:
        r = hash_cost(a.hash_cost)
        print(json.dumps(r, indent=2))
        if a.json_out:
            _write_json(a.json_out, r)
        return 0

    if a.gc_ab is not None:
        r = gc_ab(a.scenario, a.gc_ab, a.out_root)
        print(json.dumps(r, indent=2))
        if a.json_out:
            _write_json(a.json_out, r)
        return 0

    if a.memory_ab is not None:
        r = memory_ab(a.scenario, a.memory_ab, a.out_root, a.memory_ab_cap)
        print(json.dumps(r, indent=2))
        if a.json_out:
            _write_json(a.json_out, r)
        return 0

    if a.prefix_bench:
        cuts = [float(x) for x in a.prefix_cuts.split(",") if x.strip()]
        out = []
        for c in cuts:
            wd = os.path.join(a.out_root, f"_prefix_{int(c)}")
            print(f"[prefix] cut={c:.0f}s", flush=True)
            out.append(prefix_bench(a.prefix_bench, c, wd, regime=a.regime))
        res = {"prefix_bench_of": a.prefix_bench, "cuts": out}
        print(json.dumps(res, indent=2, default=str))
        if a.json_out:
            _write_json(a.json_out, res)
        return 0

    durations = [float(x) for x in a.durations.split(",") if x.strip()]
    res = ladder(durations, a.scenario, a.out_root, repeat=a.repeat, do_bench=a.bench,
                 do_curves=a.curves, regime=a.regime, oracle=not a.no_oracle, quiet=a.quiet)
    if a.json_out:
        _write_json(a.json_out, res)
    if a.markdown or not a.json_out:
        print("\n".join(render(res)))
    return 0


def _write_json(path, obj):
    os.makedirs(os.path.dirname(os.path.abspath(path)) or ".", exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, indent=1, default=str)
    print(f"[wrote] {path}", flush=True)


if __name__ == "__main__":
    raise SystemExit(main())
