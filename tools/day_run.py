#!/usr/bin/env python
"""Long runs: the InTAS day, measured rather than extrapolated.

WHY THIS EXISTS. Every cost number this project has published for the full-city engine was taken
on `--begin 0 --duration 300` -- the first five minutes of the InTAS day, which is 00:00-00:05,
the QUIETEST five minutes of the whole 86,400 s profile. `docs/realism/FULL-CITY-SCENE.md` reports
"104 ms/step and 261 MB at 333 replayed InTAS vehicles" and calls the whole city "the cheap
option". Both statements are true of midnight and of nothing else: the same scenario at 07:00
carries **3,552-3,845 concurrent vehicles**, an order of magnitude more, and cost follows
concurrency super-linearly. A cost envelope taken at one point of a demand profile is not a cost
envelope, it is a sample of one phase.

The same argument is the reason the project cares about long runs at all, and it has already been
paid for once. `gt_emissions_sample.jsonl` is written INSIDE the broadcast pre-pass, downstream of
`enforced()`, so a revoked vehicle's kinematic record ends at revocation while the vehicle keeps
driving. At 300 s that costs about 7% of the vehicle-steps. Over the InTAS peak hour it costs
**56.2%** -- 9,143 of 14,896 vehicles revoked, and at detection precision 0.308 about 6,329 of
those revocations are BENIGN vehicles. The defect is a function of RUN LENGTH and was invisible at
the length everything had been measured at (`docs/realism/TRAFFIC-PANEL-SURVIVORSHIP.md`).

So this tool does four things, and each is a measurement rather than a projection:

  `profile`  reads a SUMO summary-output over the whole day and reports the demand profile:
             concurrent vehicles, network mean speed, and where the AM peak / inter-peak trough /
             PM peak / night actually are. This is the x-axis everything else is indexed on.

  `ladder`   the COST ENVELOPE. Freeze a short window at each of several times of day, run the
             engine on each, and report ms/step, peak RSS and output bytes/s **as a function of
             concurrent vehicles**, not of simulated time. Cost follows density, so time-of-day is
             only a way of dialling concurrency.

  `day`      the long run itself, as a CHUNKED sequence of windows. One monolithic 86,400 s replay
             is not possible on this toolchain and the reason is measured, not assumed: see
             `WHY CHUNKED` below. Each window is frozen, run and finalised independently, the
             chunk index is written after every window, and an interrupted `day` resumes from the
             first window whose manifest is missing.

  `window`   one window, run in a child process so its peak working set is ITS OWN and not the
             high-water mark of every window before it. `ladder` and `day` both drive this.

WHY CHUNKED, and what that costs you. `sumo_trace.load()` materialises four Python lists of floats
per vehicle -- x, y, speed, angle -- so a frozen trace costs about **128 bytes of heap per
vehicle-step** (measured: the 13,589,568-row AM-peak hour trace loads to 1.74 GB). The InTAS day is
185,923 departures; at the peak hour's mean presence that is of order 1.0e8 vehicle-steps, i.e. a
12-14 GB resident trace before the engine allocates anything of its own, from a 4-6 GB text
artifact that must be parsed in one pass with no seek index. Chunking replaces that with one
window's worth. What it costs is REAL and must be stated: **SCMS state does not cross a chunk
boundary.** Pseudonym rotation restarts, the CRL is empty again, the misbehaviour authority's
reputation table and its accumulated false positives are gone. A chunked day is therefore a valid
measurement of TRAFFIC, RADIO and PER-WINDOW detector behaviour across the demand profile, and is
NOT a measurement of CRL growth or of MA false-positive accumulation across a day. For those, run
the longest single window the memory budget allows and say so. `day_index.json` records
`scms_state_continuous: false` so a consumer cannot mistake one for the other.

Every number this tool prints is measured on the host it ran on. Nothing here extrapolates.
"""
from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import gc
import json
import math
import os
import re
import subprocess
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
_ROOT = os.path.dirname(_HERE)
_SRC = os.path.join(_ROOT, "src")
if _SRC not in sys.path:
    sys.path.insert(0, _SRC)


# --------------------------------------------------------------------------- peak RSS ----------- #
# Same probe as `tools/protocol_stack_measure.py`: `K32GetProcessMemoryInfo` from kernel32, the
# redirected export that exists on Server 2022. `windll.psapi.GetProcessMemoryInfo` resolves on
# paper here and returns 0, which silently reports a 0.0 MiB peak for every arm.
class _PMC(ctypes.Structure):
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t)]


def _mem_fn():
    k32 = ctypes.WinDLL("kernel32")
    fn = getattr(k32, "K32GetProcessMemoryInfo")
    fn.argtypes = [wt.HANDLE, ctypes.POINTER(_PMC), wt.DWORD]
    fn.restype = wt.BOOL
    return k32, fn


def rss_mb() -> tuple[float, float]:
    """(current working set, peak working set) of THIS process, MiB. Windows only; (0,0) else."""
    try:
        k32, fn = _mem_fn()
        c = _PMC()
        c.cb = ctypes.sizeof(c)
        if not fn(k32.GetCurrentProcess(), ctypes.byref(c), c.cb):
            return 0.0, 0.0
        return c.WorkingSetSize / 1048576.0, c.PeakWorkingSetSize / 1048576.0
    except Exception:                                       # noqa: BLE001 - diagnostics only
        return 0.0, 0.0


def peak_rss_mb() -> float:
    return rss_mb()[1]


# --------------------------------------------------------------------------- the day ------------ #
#: The InTAS day, in seconds. `scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_buildings.sumocfg`
#: declares `<begin 0>`/`<end 86400>` with 185,923 vehicle departures over 22 route files.
DAY_S = 86400.0

#: Named phases of the day, as `(label, start_s, end_s)`. These are LABELS, not measurements: which
#: hour is actually the peak is read off a `profile` run, and `profile` prints both.
PHASES = (
    ("night",       0.0,     18000.0),   # 00:00-05:00
    ("am_ramp",     18000.0, 25200.0),   # 05:00-07:00
    ("am_peak",     25200.0, 32400.0),   # 07:00-09:00
    ("interpeak",   32400.0, 54000.0),   # 09:00-15:00
    ("pm_peak",     54000.0, 64800.0),   # 15:00-18:00
    ("evening",     64800.0, 79200.0),   # 18:00-22:00
    ("late",        79200.0, 86400.0),   # 22:00-24:00
)


def phase_of(t: float) -> str:
    for name, a, b in PHASES:
        if a <= t < b:
            return name
    return "late"


def hhmm(t: float) -> str:
    t = int(round(t))
    return f"{t // 3600:02d}:{(t % 3600) // 60:02d}"


# --------------------------------------------------------------------------- profile ------------ #
_STEP_RE = re.compile(
    rb'<step\s+time="([^"]+)"'
    rb'(?=[^>]*\brunning="(\d+)")'
    rb'(?=[^>]*\binserted="(\d+)")'
    rb'(?:(?=[^>]*\bmeanSpeed="([^"]*)")|)'
    rb'(?:(?=[^>]*\bhalting="(\d+)")|)'
    rb'(?:(?=[^>]*\bended="(\d+)")|)'
    rb'(?:(?=[^>]*\bteleports="(\d+)")|)')


def read_summary(path: str) -> list[dict]:
    """Parse a SUMO `--summary-output` file into rows. Streamed line by line: the day's summary at
    0.1 s resolution is 240 MB and there is no reason to hold the DOM for it."""
    rows = []
    with open(path, "rb") as fh:
        for line in fh:
            m = _STEP_RE.search(line)
            if not m:
                continue
            rows.append({
                "t": float(m.group(1)),
                "running": int(m.group(2)),
                "inserted": int(m.group(3)),
                "mean_speed": float(m.group(4)) if m.group(4) not in (None, b"") else None,
                "halting": int(m.group(5)) if m.group(5) else 0,
                "ended": int(m.group(6)) if m.group(6) else 0,
                "teleports": int(m.group(7)) if m.group(7) else 0,
            })
    return rows


def profile_report(rows: list[dict], bucket_s: float = 900.0) -> dict:
    """Bucketed demand profile + a per-phase table. `bucket_s` defaults to 15 minutes."""
    if not rows:
        raise SystemExit("summary file contained no <step> rows")
    buckets: dict[int, list[dict]] = {}
    for r in rows:
        buckets.setdefault(int(r["t"] // bucket_s), []).append(r)

    def agg(rs: list[dict]) -> dict:
        run = [r["running"] for r in rs]
        spd = [r["mean_speed"] for r in rs if r["mean_speed"] is not None]
        halt = [r["halting"] for r in rs]
        return {
            "n": len(rs),
            "running_mean": round(sum(run) / len(run), 1),
            "running_max": max(run),
            "running_min": min(run),
            "mean_speed_mps": round(sum(spd) / len(spd), 3) if spd else None,
            "halting_mean": round(sum(halt) / len(halt), 1) if halt else None,
            "halting_share": round(sum(halt) / max(1e-9, sum(run)), 4) if run else None,
        }

    prof = []
    for b in sorted(buckets):
        a = agg(buckets[b])
        a["t0"] = b * bucket_s
        a["clock"] = hhmm(b * bucket_s)
        a["phase"] = phase_of(b * bucket_s)
        prof.append(a)

    by_phase = {}
    for name, lo, hi in PHASES:
        rs = [r for r in rows if lo <= r["t"] < hi]
        if rs:
            by_phase[name] = agg(rs) | {"t0": lo, "t1": hi, "clock": f"{hhmm(lo)}-{hhmm(hi)}"}

    peak = max(rows, key=lambda r: r["running"])
    trough_candidates = [r for r in rows if 32400.0 <= r["t"] < 54000.0]
    trough = min(trough_candidates, key=lambda r: r["running"]) if trough_candidates else None
    return {
        "rows": len(rows),
        "t_first": rows[0]["t"], "t_last": rows[-1]["t"],
        "bucket_s": bucket_s,
        "inserted_total": max(r["inserted"] for r in rows),
        "peak": {"t": peak["t"], "clock": hhmm(peak["t"]), "running": peak["running"]},
        "interpeak_trough": ({"t": trough["t"], "clock": hhmm(trough["t"]),
                              "running": trough["running"]} if trough else None),
        "vehicle_steps_est": round(sum(r["running"] for r in rows)
                                   * (rows[1]["t"] - rows[0]["t"] if len(rows) > 1 else 1.0)),
        "by_phase": by_phase,
        "buckets": prof,
    }


# --------------------------------------------------------------------------- freeze ------------- #
def sumo_output_args(dirpath: str, tag: str) -> list[str]:
    """Redirect a scenario's own declared outputs into `dirpath`, by ABSOLUTE path.

    A `.sumocfg` names `summary-output` / `tripinfo-output` / `statistic-output` / `log` relative to
    its own directory, so freezing a window of a committed scenario OVERWRITES that scenario's
    committed measurement files in place. `--output-prefix` is SUMO's answer and it is not usable
    here: the prefix is prepended to the *relative* name, so an absolute prefix produces
    `<cfgdir>/C:\\...\\name` and SUMO refuses to build it (measured, not guessed). Overriding each
    output by absolute path is the version that works and that provably writes nothing into the
    scenario tree."""
    os.makedirs(dirpath, exist_ok=True)
    j = lambda s: os.path.join(dirpath, f"{tag}{s}")        # noqa: E731
    return ["--summary-output", j("summary.xml"),
            "--tripinfo-output", j("tripinfo.xml"),
            "--statistic-output", j("statistic.xml"),
            "--log", j("sumo.log")]


def freeze_window(*, net: str, sumocfg: str, out: str, at: float, steps: int, dt: float,
                  warmup: int, substeps: int, run_seed: int, time_to_teleport: float,
                  sumo_args: list[str], force: bool = False) -> dict:
    """Freeze one window of the day at `at` seconds, with `warmup` unrecorded fill steps before it.

    SUMO discards every vehicle whose departure is before `--begin`, so a window at 07:00 asked for
    cold starts on an EMPTY city and its first minutes are a fill transient, not the peak. The
    freeze therefore begins at `at - warmup*dt` and throws the fill away. `warmup` is a real
    parameter of the measurement, not a detail: compare the recorded window's concurrency against
    the same clock time in a continuous `profile` run, which is what says whether the warmup was
    long enough.

    THE CLAMP IS ON THE WARMUP, NOT ON `begin`. Clamping only `begin` at zero -- the obvious way to
    write it -- keeps the full `warmup_steps`, so the recorded window silently starts at
    `warmup*dt` instead of at `at`: a `day` run of 120 s windows from midnight produced THREE
    IDENTICAL windows (329.5 concurrent, 362 vehicles, all three) before this was measured, because
    every one of them had begin=0 and warmup=300 and therefore recorded [301, 421)."""
    from scms_sim_ref.mock_pipeline import sumo_trace as st
    warmup = min(int(warmup), int(round(at / dt)))
    begin = at - warmup * dt
    if os.path.exists(out) and not force:
        tr = st.load(out)
        return {"reused": True, "path": out, "sha256": tr.sha256, "summary": tr.summary(),
                "begin": begin, "at": at, "warmup": warmup, "freeze_wall_s": 0.0}
    t0 = time.perf_counter()
    tr = st.freeze(net=net, sumocfg=sumocfg, out=out, run_seed=run_seed, steps=steps, dt=dt,
                   begin=begin, warmup_steps=warmup, substeps=substeps,
                   time_to_teleport=time_to_teleport, split_on_gap=True,
                   extra_args=tuple(sumo_args))
    wall = time.perf_counter() - t0
    got = tr.meta.get("step0_sim_time")
    if got is not None and abs(float(got) - (at + dt)) > 1.5 * dt:
        raise RuntimeError(f"freeze at {at} s recorded its first step at SUMO time {got}, not "
                           f"{at + dt}: the warmup and begin do not agree")
    return {"reused": False, "path": out, "sha256": tr.sha256, "summary": tr.summary(),
            "begin": begin, "at": at, "warmup": warmup, "freeze_wall_s": round(wall, 2),
            "trace_bytes": os.path.getsize(out)}


def trace_concurrency(path: str) -> dict:
    """Concurrency of a frozen trace, read from its HEADER only -- no row parse, no 128 B/step heap.

    Every `V` record carries `first_step`/`last_step`, so the exact per-step present-count is a
    difference array over the vehicle table. That is the whole reason this is cheap: the AM-peak
    hour trace is 610 MB of rows and 14,896 header lines."""
    from scms_sim_ref.mock_pipeline.sumo_trace import TRACE_FORMAT
    with open(path, "r", encoding="utf-8") as fh:
        head = fh.readline().rstrip("\n")
        if head != "#" + TRACE_FORMAT:
            raise ValueError(f"{path}: not a {TRACE_FORMAT} artifact")
        meta = json.loads(fh.readline()[6:])
        n_veh = int(fh.readline().split()[1])
        n_steps = int(meta["steps"])
        delta = [0] * (n_steps + 2)
        rows = 0
        for _ in range(n_veh):
            p = fh.readline().split()
            a, b = int(p[3]), int(p[4])
            delta[a] += 1
            delta[b + 1] -= 1
            rows += b - a + 1
        line = fh.readline()
        declared_rows = int(line.split()[1]) if line.startswith("#rows ") else rows
    cur = 0
    series = []
    for s in range(n_steps):
        cur += delta[s]
        series.append(cur)
    series_sorted = sorted(series)
    return {
        "n_vehicles": n_veh, "n_steps": n_steps, "n_rows": declared_rows,
        "step0_sim_time": meta.get("step0_sim_time"),
        "concurrent_mean": round(sum(series) / len(series), 1),
        "concurrent_p50": series_sorted[len(series) // 2],
        "concurrent_min": series_sorted[0], "concurrent_max": series_sorted[-1],
        "concurrent_first": series[0], "concurrent_last": series[-1],
        "teleports": meta.get("teleports"), "gap_splits": meta.get("gap_splits"),
    }


# --------------------------------------------------------------------------- one window --------- #
def build_window_config(base: dict, overrides: dict, out_dir: str, trace: str, duration_s: float,
                        dt: float) -> object:
    from scms_sim_ref.mock_pipeline.run import config_from_dict
    d = dict(base)
    d.update(overrides)
    d["out_dir"] = out_dir
    d["mobility_source"] = "sumo_replay"
    d["sumo_trace"] = trace
    d["sumo_trace_sha256"] = ""          # validate_config fills it from the file
    d["traffic_flow"] = True
    d["duration_s"] = float(duration_s)
    d["dt"] = float(dt)
    return config_from_dict(d)


def dir_bytes(root: str) -> dict:
    out = {}
    for dirpath, _dirnames, files in os.walk(root):
        for f in files:
            p = os.path.join(dirpath, f)
            rel = os.path.relpath(p, root).replace("\\", "/")
            try:
                out[rel] = os.path.getsize(p)
            except OSError:
                pass
    return out


def run_one_window(cfg, *, sample_every: int, abort_at: int = 0) -> dict:
    """Run one window IN THIS PROCESS and instrument it.

    `run.PER_STEP_HOOK` is the engine's own telemetry seam (default None -> zero effect, documented
    as digest-neutral). It is used here for two things and nothing else: sampling wall clock and
    working set every `sample_every` steps, and -- when `abort_at` is set -- taking the engine's
    OWN graceful-interrupt path deterministically, which is what `run._ABORT` exists for. A real
    SIGINT is exercised separately by `--sigint-at`, which signals the child process for real."""
    from scms_sim_ref.mock_pipeline import run as R

    samples: list[dict] = []
    t0 = time.perf_counter()
    # `last_seen` is why the hook runs on EVERY step and not only on sampled ones: the manifest does
    # NOT record how many steps a flow run actually took. `cfg.n_steps` stays at its default (the
    # loop count is a local in `run_pipeline`), so a 120 s window reports `n_steps: 40` and an
    # INTERRUPTED run reports whatever the config said. Counting the hook calls is the only honest
    # source, and it is the one that stays right when the run is cut short.
    state = {"last": t0, "last_step": 0, "last_seen": -1, "t_step0": None, "t_last": t0}

    def hook(step: int) -> None:
        now = time.perf_counter()
        state["last_seen"] = step
        state["t_last"] = now
        if state["t_step0"] is None:
            state["t_step0"] = now
        if abort_at and step >= abort_at:
            R._ABORT["flag"] = True
            return
        if sample_every and step % sample_every == 0:
            cur, peak = rss_mb()
            d_steps = max(1, step - state["last_step"])
            samples.append({"step": step, "wall_s": round(now - t0, 3),
                            "ms_per_step_window": round(1000.0 * (now - state["last"]) / d_steps, 2),
                            "rss_mb": round(cur, 1), "peak_rss_mb": round(peak, 1)})
            state["last"], state["last_step"] = now, step

    prev_hook = R.PER_STEP_HOOK
    R.PER_STEP_HOOK = hook
    gc.collect()
    rss_before = peak_rss_mb()
    cpu0 = time.process_time()
    try:
        res = R.run_pipeline(cfg)
    finally:
        R.PER_STEP_HOOK = prev_hook
    wall = time.perf_counter() - t0
    cpu = time.process_time() - cpu0
    cur, peak = rss_mb()
    aborted = bool(R._ABORT["flag"])

    man = {}
    mp = os.path.join(res.out_dir, "manifest.json")
    if os.path.exists(mp):
        with open(mp, encoding="utf-8") as fh:
            man = json.load(fh)
    planned = int(round(cfg.duration_s / cfg.dt)) if cfg.duration_s > 0 else cfg.n_steps
    # The interrupt check breaks BEFORE the step whose hook call it saw, so an aborted run ran
    # `last_seen` steps and a complete one ran `last_seen + 1`.
    steps_run = (state["last_seen"] + (0 if aborted else 1)) if state["last_seen"] >= 0 else planned
    files = dir_bytes(res.out_dir)
    total_bytes = sum(files.values())
    sim_s = steps_run * cfg.dt
    setup_s = (state["t_step0"] - t0) if state["t_step0"] else 0.0
    loop_s = (state["t_last"] - state["t_step0"]) if state["t_step0"] else 0.0
    finalise_s = wall - (state["t_last"] - t0)
    return {
        "out_dir": res.out_dir,
        "wall_s": round(wall, 3), "cpu_s": round(cpu, 3),
        "steps": steps_run, "steps_planned": planned, "aborted": aborted,
        "sim_s": sim_s, "dt": cfg.dt,
        "setup_s": round(setup_s, 3), "loop_s": round(loop_s, 3),
        "finalise_s": round(finalise_s, 3),
        # `ms_per_step` is the whole run divided by the steps it ran -- what a user waits. The
        # `_loop` figure is the steady-state step cost with scene import, trace load and the final
        # digest pass taken out, which is the number that scales with duration.
        "ms_per_step": round(1000.0 * wall / max(1, steps_run), 2),
        "ms_per_step_loop": round(1000.0 * loop_s / max(1, steps_run - 1), 2),
        "realtime_factor": round(sim_s / wall, 3) if wall > 0 else None,
        "peak_rss_mb": round(peak, 1), "rss_end_mb": round(cur, 1),
        "peak_rss_before_mb": round(rss_before, 1),
        "vehicles": res.n_vehicles, "reports": res.n_reports,
        "investigations": res.n_investigations, "revoked": res.n_revoked,
        "revoked_frac": round(res.n_revoked / max(1, res.n_vehicles), 4),
        "data_digest": res.data_digest,
        "counts": man.get("counts", {}),
        "bytes_total": total_bytes,
        "bytes_per_sim_s": round(total_bytes / max(1e-9, sim_s), 1),
        "bytes": files,
        "samples": samples,
    }


# --------------------------------------------------------------------------- the child ---------- #
def _child_main(argv) -> int:
    """`day_run.py _child --spec <json>`: run ONE window and print one JSON result line.

    A child process per window is not fussiness. `PeakWorkingSetSize` is a high-water mark that
    never falls, so running eight windows in one interpreter reports the eighth window's peak as
    the maximum of all eight -- which is exactly the number a cost envelope must not be."""
    p = argparse.ArgumentParser(prog="day_run _child")
    p.add_argument("--spec", required=True)
    p.add_argument("--result", required=True)
    a = p.parse_args(argv)
    with open(a.spec, encoding="utf-8") as fh:
        spec = json.load(fh)
    if spec.get("enable_ctrl_c"):
        # A child created with CREATE_NEW_PROCESS_GROUP inherits Ctrl-C DISABLED. Re-enable it, or
        # the SIGINT test measures nothing: the signal is delivered and dropped.
        try:
            k32 = ctypes.WinDLL("kernel32")
            k32.SetConsoleCtrlHandler.argtypes = [ctypes.c_void_p, wt.BOOL]
            k32.SetConsoleCtrlHandler.restype = wt.BOOL
            k32.SetConsoleCtrlHandler(None, False)
            import signal as _s
            _s.signal(_s.SIGINT, _s.default_int_handler)     # make sure Python's handler is armed
        except Exception:                                   # noqa: BLE001
            pass
    cfg = build_window_config(spec.get("base", {}), spec.get("overrides", {}), spec["out_dir"],
                              spec["trace"], spec["duration_s"], spec["dt"])
    rec = run_one_window(cfg, sample_every=int(spec.get("sample_every", 0)),
                         abort_at=int(spec.get("abort_at", 0)))
    rec["window"] = spec.get("window", {})
    with open(a.result, "w", encoding="utf-8") as fh:
        json.dump(rec, fh, indent=1, sort_keys=True)
    print(f"[window done] {rec['ms_per_step']} ms/step  peak {rec['peak_rss_mb']} MB  "
          f"{rec['bytes_total'] / 1e6:.1f} MB out", flush=True)
    return 0


def spawn_window(spec: dict, workdir: str, tag: str, *, sigint_at_s: float = 0.0,
                 timeout_s: float = 0.0) -> dict:
    """Run one window in a child and return its result record (plus how the child ended).

    `sigint_at_s` > 0 sends a REAL `CTRL_C_EVENT` to the child that many wall-seconds in. That is
    the interruption a user actually performs, and it exercises the engine's own SIGINT handler --
    not a test seam that sets the same flag from inside."""
    os.makedirs(workdir, exist_ok=True)
    spec_path = os.path.join(workdir, f"{tag}.spec.json")
    res_path = os.path.join(workdir, f"{tag}.result.json")
    if sigint_at_s > 0:
        spec = dict(spec, enable_ctrl_c=True)
    with open(spec_path, "w", encoding="utf-8") as fh:
        json.dump(spec, fh, indent=1, sort_keys=True)
    env = dict(os.environ)
    env["PYTHONPATH"] = _SRC + os.pathsep + env.get("PYTHONPATH", "")
    flags = 0
    if sigint_at_s > 0 and os.name == "nt":
        flags = subprocess.CREATE_NEW_PROCESS_GROUP
    # The child's stdout goes to a FILE, not to a pipe. A pipe that nobody drains until
    # `communicate()` deadlocks the child the moment it fills the OS buffer, and a `day` run leaves
    # the child unread for the whole window -- an hour, at peak density. A file also survives the
    # parent being killed, which is the case a long run most needs a log for.
    log_path = os.path.join(workdir, f"{tag}.stdout.txt")
    t0 = time.perf_counter()
    with open(log_path, "w", encoding="utf-8", errors="replace") as logfh:
        proc = subprocess.Popen([sys.executable, os.path.abspath(__file__), "_child",
                                 "--spec", spec_path, "--result", res_path],
                                cwd=_ROOT, env=env, creationflags=flags,
                                stdout=logfh, stderr=subprocess.STDOUT, text=True)
        signalled = False
        if sigint_at_s > 0:
            import signal as _sig
            deadline = t0 + sigint_at_s
            while time.perf_counter() < deadline and proc.poll() is None:
                time.sleep(0.25)
            if proc.poll() is None:
                # CTRL_C_EVENT, not CTRL_BREAK_EVENT. Windows maps Ctrl-Break to SIGBREAK and the
                # engine's graceful handler is installed on SIGINT only, so a Ctrl-Break child dies
                # with 0xC000013A (STATUS_CONTROL_C_EXIT) and leaves the half-written streams with no
                # manifest -- measured, and it is the WRONG test. Ctrl-C is what a user presses.
                os.kill(proc.pid, _sig.CTRL_C_EVENT if os.name == "nt" else _sig.SIGINT)
                signalled = True
        timed_out = False
        try:
            proc.wait(timeout=timeout_s or None)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
            timed_out = True
    wall = time.perf_counter() - t0
    try:
        with open(log_path, encoding="utf-8", errors="replace") as fh:
            out = fh.read()
    except OSError:
        out = ""
    if timed_out:
        return {"error": "timeout", "stdout_tail": out[-4000:], "log": log_path,
                "child_wall_s": round(wall, 3)}
    rec: dict
    if os.path.exists(res_path):
        with open(res_path, encoding="utf-8") as fh:
            rec = json.load(fh)
    else:
        rec = {"error": "no result file", "stdout": out[-6000:]}
    rec["child_returncode"] = proc.returncode
    rec["child_wall_s"] = round(wall, 3)
    rec["signalled"] = signalled
    rec["log"] = log_path
    rec["stdout_tail"] = out[-2500:] if out else ""
    return rec


# --------------------------------------------------------------------------- commands ----------- #
def cmd_profile(a) -> int:
    rows = read_summary(a.summary)
    rep = profile_report(rows, bucket_s=a.bucket)
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8") as fh:
            json.dump(rep, fh, indent=1, sort_keys=True)
    print(f"summary rows {rep['rows']}  t=[{rep['t_first']:.0f},{rep['t_last']:.0f}]  "
          f"inserted={rep['inserted_total']}")
    print(f"peak concurrency {rep['peak']['running']} at {rep['peak']['clock']}")
    if rep["interpeak_trough"]:
        print(f"inter-peak trough {rep['interpeak_trough']['running']} at "
              f"{rep['interpeak_trough']['clock']}")
    print()
    print(f"{'phase':<12}{'clock':<14}{'mean':>8}{'max':>8}{'min':>7}{'speed m/s':>11}"
          f"{'halting':>9}")
    for name, _lo, _hi in PHASES:
        d = rep["by_phase"].get(name)
        if not d:
            continue
        print(f"{name:<12}{d['clock']:<14}{d['running_mean']:>8.0f}{d['running_max']:>8d}"
              f"{d['running_min']:>7d}{(d['mean_speed_mps'] or 0):>11.2f}"
              f"{(d['halting_share'] or 0):>9.3f}")
    if a.buckets:
        print()
        print(f"{'clock':<8}{'phase':<11}{'running':>9}{'max':>7}{'speed':>8}{'halt':>8}")
        for b in rep["buckets"]:
            print(f"{b['clock']:<8}{b['phase']:<11}{b['running_mean']:>9.0f}{b['running_max']:>7d}"
                  f"{(b['mean_speed_mps'] or 0):>8.2f}{(b['halting_share'] or 0):>8.3f}")
    return 0


def _base_cfg(a) -> dict:
    base = {}
    if a.base:
        with open(a.base, encoding="utf-8-sig") as fh:
            base = json.load(fh)
        base = base.get("config", base)
    over = {}
    for kv in a.set:
        k, _, v = kv.partition("=")
        try:
            over[k] = json.loads(v)
        except json.JSONDecodeError:
            over[k] = v
    return {**base, **over}


def _scene_overrides(a) -> dict:
    d = {"road_network": "sumo", "sumo_net": a.net, "custom_network_directed": True,
         "verbose": bool(a.verbose)}
    if a.buildings:
        d["sumo_buildings"] = a.buildings
    return d


def cmd_ladder(a) -> int:
    """The cost envelope: same engine config, different concurrency, measured."""
    os.makedirs(a.work, exist_ok=True)
    base = _base_cfg(a) | _scene_overrides(a)
    times = [float(x) for x in a.at.split(",") if x.strip()]
    results = []
    idx_path = os.path.join(a.work, "ladder.json")
    for at in times:
        tag = f"t{int(at):05d}"
        trace = os.path.join(a.work, f"{tag}.trace")
        fz = freeze_window(net=a.net, sumocfg=a.sumocfg, out=trace, at=at, steps=a.steps, dt=a.dt,
                           warmup=a.warmup, substeps=a.substeps, run_seed=a.seed,
                           time_to_teleport=a.time_to_teleport,
                           sumo_args=sumo_output_args(os.path.join(a.work, "sumo_out"), tag + "_"),
                           force=a.force)
        conc = trace_concurrency(trace)
        print(f"[freeze {tag} @ {hhmm(at)}] {conc['n_vehicles']} trajectories, {conc['n_rows']} "
              f"rows, concurrent mean {conc['concurrent_mean']} "
              f"(min {conc['concurrent_min']} max {conc['concurrent_max']}), "
              f"{fz['freeze_wall_s']} s", flush=True)
        out_dir = os.path.join(a.work, f"ds_{tag}")
        spec = {"base": base, "overrides": {}, "out_dir": out_dir, "trace": trace,
                "duration_s": a.steps * a.dt, "dt": a.dt, "sample_every": a.sample_every,
                "window": {"at": at, "clock": hhmm(at), "phase": phase_of(at)}}
        rec = spawn_window(spec, a.work, tag)
        rec["trace"] = {**conc, **{k: fz[k] for k in ("freeze_wall_s", "sha256", "begin")}}
        rec["at"] = at
        rec["clock"] = hhmm(at)
        rec["phase"] = phase_of(at)
        results.append(rec)
        with open(idx_path, "w", encoding="utf-8") as fh:
            json.dump({"host_note": "measured", "arms": results}, fh, indent=1, sort_keys=True)
        print(f"[run {tag}] {rec.get('ms_per_step')} ms/step, peak {rec.get('peak_rss_mb')} MB, "
              f"{rec.get('bytes_total', 0) / 1e6:.1f} MB out, rc={rec.get('child_returncode')}",
              flush=True)
    print()
    print(f"{'clock':<7}{'phase':<11}{'conc':>7}{'veh':>7}{'ms/step':>9}{'loop':>9}{'setup':>8}"
          f"{'peakMB':>8}{'MB/sim-s':>10}{'us/veh-step':>12}")
    for r in results:
        c = r.get("trace", {}).get("concurrent_mean") or 0
        mps = r.get("ms_per_step") or 0
        loop = r.get("ms_per_step_loop") or 0
        print(f"{r['clock']:<7}{r['phase']:<11}{c:>7.0f}{r.get('vehicles', 0):>7d}{mps:>9.1f}"
              f"{loop:>9.1f}{(r.get('setup_s') or 0):>8.1f}"
              f"{(r.get('peak_rss_mb') or 0):>8.0f}"
              f"{(r.get('bytes_per_sim_s') or 0) / 1e6:>10.3f}"
              f"{(1000.0 * loop / c if c else 0):>12.2f}")
    # The exponent, fitted on the two extreme arms only and labelled as a fit rather than a law.
    fit = [(r["trace"]["concurrent_mean"], r.get("ms_per_step_loop") or 0) for r in results
           if r.get("trace", {}).get("concurrent_mean") and r.get("ms_per_step_loop")]
    if len(fit) >= 2:
        fit.sort()
        (c0, m0), (c1, m1) = fit[0], fit[-1]
        if c1 > c0 and m0 > 0:
            alpha = math.log(m1 / m0) / math.log(c1 / c0)
            print(f"\nfitted per-step exponent over [{c0:.0f}, {c1:.0f}] concurrent: "
                  f"cost ~ N^{alpha:.2f}  (ENDPOINT FIT on two measured arms, not a law)")
    print(f"\nwrote {idx_path}")
    return 0


def cmd_day(a) -> int:
    """The chunked long run. Resumable; the chunk index is rewritten after every window."""
    os.makedirs(a.out, exist_ok=True)
    work = os.path.join(a.out, "_work")
    os.makedirs(work, exist_ok=True)
    base = _base_cfg(a) | _scene_overrides(a)
    idx_path = os.path.join(a.out, "day_index.json")
    index = {"begin_s": a.begin, "duration_s": a.duration, "window_s": a.window, "dt": a.dt,
             "warmup_steps": a.warmup, "seed": a.seed, "net": os.path.basename(a.net),
             "sumocfg": os.path.basename(a.sumocfg),
             "scms_state_continuous": False,
             "scms_state_note": ("each window is an independent run: pseudonym rotation, the CRL "
                                 "and the MA reputation table all restart at every boundary. Valid "
                                 "for traffic/radio/per-window detection across the profile; NOT a "
                                 "measurement of CRL growth or MA false-positive accumulation "
                                 "across a day."),
             "windows": []}
    if os.path.exists(idx_path) and a.resume:
        with open(idx_path, encoding="utf-8") as fh:
            index = json.load(fh)
        index["windows"] = list(index.get("windows", []))
    done = {w["tag"] for w in index["windows"] if w.get("ok")}
    n_windows = int(math.ceil(a.duration / a.window))
    t_all = time.perf_counter()
    for i in range(n_windows):
        at = a.begin + i * a.window
        dur = min(a.window, a.begin + a.duration - at)
        tag = f"w{i:04d}_t{int(at):05d}"
        out_dir = os.path.join(a.out, tag)
        if tag in done and os.path.exists(os.path.join(out_dir, "manifest.json")):
            print(f"[skip {tag}] already complete", flush=True)
            continue
        steps = int(round(dur / a.dt))
        trace = os.path.join(work, f"{tag}.trace")
        fz = freeze_window(net=a.net, sumocfg=a.sumocfg, out=trace, at=at, steps=steps, dt=a.dt,
                           # every window pays its own warmup: SUMO discards pre-`begin` departures,
                           # so a window cannot inherit the previous one's fill (measured: the
                           # freeze cost rises 5.4 -> 11.1 -> 18.5 s over three 120 s windows).
                           warmup=a.warmup,
                           substeps=a.substeps, run_seed=a.seed,
                           time_to_teleport=a.time_to_teleport,
                           sumo_args=sumo_output_args(os.path.join(work, "sumo_out"), tag + "_"),
                           force=False)
        conc = trace_concurrency(trace)
        print(f"[freeze {tag} @ {hhmm(at)}] {conc['n_vehicles']} veh, {conc['n_rows']} rows, "
              f"concurrent mean {conc['concurrent_mean']}, {fz['freeze_wall_s']} s", flush=True)
        spec = {"base": base, "overrides": {}, "out_dir": out_dir, "trace": trace,
                "duration_s": dur, "dt": a.dt, "sample_every": a.sample_every,
                "window": {"at": at, "clock": hhmm(at), "phase": phase_of(at), "index": i}}
        rec = spawn_window(spec, work, tag)
        entry = {"tag": tag, "at": at, "clock": hhmm(at), "phase": phase_of(at),
                 "ok": rec.get("child_returncode") == 0 and "error" not in rec,
                 "freeze_wall_s": fz["freeze_wall_s"], "trace_sha256": fz["sha256"],
                 "trace_bytes": os.path.getsize(trace), "concurrency": conc,
                 "result": {k: v for k, v in rec.items() if k not in ("bytes", "samples")},
                 "samples": rec.get("samples", [])}
        index["windows"] = [w for w in index["windows"] if w["tag"] != tag] + [entry]
        index["windows"].sort(key=lambda w: w["at"])
        index["elapsed_wall_s"] = round(time.perf_counter() - t_all, 1)
        with open(idx_path, "w", encoding="utf-8") as fh:
            json.dump(index, fh, indent=1, sort_keys=True)
        if a.drop_traces:
            try:
                os.remove(trace)
            except OSError:
                pass
        print(f"[run {tag}] {rec.get('ms_per_step')} ms/step, peak {rec.get('peak_rss_mb')} MB, "
              f"{rec.get('bytes_total', 0) / 1e6:.1f} MB, rc={rec.get('child_returncode')}",
              flush=True)
    ok = [w for w in index["windows"] if w.get("ok")]
    tot_bytes = sum(w["result"].get("bytes_total", 0) for w in ok)
    tot_wall = sum(w["result"].get("child_wall_s", 0) for w in ok)
    tot_freeze = sum(w.get("freeze_wall_s", 0) for w in ok)
    print(f"\n{len(ok)}/{n_windows} windows, {tot_bytes / 1e9:.2f} GB output, "
          f"{tot_freeze / 60:.1f} min freezing + {tot_wall / 60:.1f} min running")
    print(f"wrote {idx_path}")
    return 0


def cmd_freeze(a) -> int:
    """Freeze ONE window's trace and print its concurrency. Split out so a batch of windows can be
    frozen in parallel (SUMO is single-threaded; this host has 16 logical cores) before a `ladder`
    or `day` pass reuses them."""
    tag = a.tag or f"t{int(a.at):05d}"
    out = a.out or os.path.join(a.work, f"{tag}.trace")
    fz = freeze_window(net=a.net, sumocfg=a.sumocfg, out=out, at=a.at, steps=a.steps, dt=a.dt,
                       warmup=a.warmup, substeps=a.substeps, run_seed=a.seed,
                       time_to_teleport=a.time_to_teleport,
                       sumo_args=sumo_output_args(os.path.join(a.work, "sumo_out"), tag + "_"),
                       force=a.force)
    conc = trace_concurrency(out)
    rec = {"tag": tag, "at": a.at, "clock": hhmm(a.at), "phase": phase_of(a.at),
           "begin": fz["begin"], "warmup_steps": a.warmup, "reused": fz["reused"],
           "freeze_wall_s": fz["freeze_wall_s"], "sha256": fz["sha256"],
           "trace_bytes": os.path.getsize(out), "summary": fz["summary"], "concurrency": conc}
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8") as fh:
            json.dump(rec, fh, indent=1, sort_keys=True)
    print(json.dumps(rec, indent=1, sort_keys=True))
    return 0


def cmd_window(a) -> int:
    """One window, end to end -- the unit `day` and `ladder` are made of."""
    base = _base_cfg(a) | _scene_overrides(a)
    os.makedirs(a.work, exist_ok=True)
    tag = a.tag or f"t{int(a.at):05d}"
    trace = a.trace or os.path.join(a.work, f"{tag}.trace")
    steps = int(round(a.duration / a.dt))
    if not a.trace:
        fz = freeze_window(net=a.net, sumocfg=a.sumocfg, out=trace, at=a.at, steps=steps, dt=a.dt,
                           warmup=a.warmup, substeps=a.substeps, run_seed=a.seed,
                           time_to_teleport=a.time_to_teleport,
                           sumo_args=sumo_output_args(os.path.join(a.work, "sumo_out"), tag + "_"),
                           force=a.force)
        print(json.dumps(fz["summary"], indent=1, sort_keys=True)[:1200], flush=True)
    conc = trace_concurrency(trace)
    print(f"[trace] {conc}", flush=True)
    spec = {"base": base, "overrides": {}, "out_dir": a.out, "trace": trace,
            "duration_s": a.duration, "dt": a.dt, "sample_every": a.sample_every,
            "abort_at": a.abort_at,
            "window": {"at": a.at, "clock": hhmm(a.at), "phase": phase_of(a.at)}}
    rec = spawn_window(spec, a.work, tag, sigint_at_s=a.sigint_at)
    rec["trace"] = conc
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8") as fh:
            json.dump(rec, fh, indent=1, sort_keys=True)
    print(json.dumps({k: v for k, v in rec.items() if k not in ("bytes", "samples", "stdout_tail")},
                     indent=1, sort_keys=True))
    return 0 if rec.get("child_returncode") == 0 else 1


def cmd_volume(a) -> int:
    """Output-volume accounting for a finished window, and what a day of it would be.

    The projection is explicit and its basis is printed with it: bytes are scaled by the ratio of
    the day's vehicle-steps to the measured window's, because every large stream here is per
    vehicle-step (emissions) or per report (labels, ma_reports), not per second."""
    files = dir_bytes(a.dataset)
    man_path = os.path.join(a.dataset, "manifest.json")
    with open(man_path, encoding="utf-8") as fh:
        man = json.load(fh)
    cfg = man.get("config", {})
    sim_s, veh_steps = _extent(man, a.sim_s)
    print(f"dataset {a.dataset}: {sim_s:.0f} simulated s, {veh_steps} SIMULATED vehicle-steps "
          f"(the trace holds {man.get('mobility', {}).get('provider', {}).get('n_rows')}), "
          f"emit_sample_prob={cfg.get('emit_sample_prob')}")
    total = sum(files.values())
    n_veh = int(man.get("counts", {}).get("vehicles") or 0)
    print(f"  {'file':<44}{'size':>10}{'B/veh-step':>12}{'B/vehicle':>11}  scales with")
    for rel, n in sorted(files.items(), key=lambda kv: -kv[1]):
        if n < 1024:
            continue
        print(f"  {rel:<44}{n / 1e6:>8.2f} MB{n / max(1, veh_steps):>12.1f}"
              f"{n / max(1, n_veh):>11.1f}  {'veh-step' if rel in PER_STEP_STREAMS else 'vehicle'}")
    print(f"  {'TOTAL':<44}{total / 1e6:>8.2f} MB{total / max(1, veh_steps):>12.1f}"
          f"{total / max(1, n_veh):>11.1f}")
    if a.day_vehicle_steps:
        # The projection is per-STREAM, because the streams do not scale the same way. Anything in
        # PER_STEP_STREAMS grows with vehicle-steps; the rest is one row per vehicle (or per cert)
        # and grows with DEPARTURES. Scaling the whole directory by one factor -- the obvious thing
        # to do -- overstates a day by whichever term happens to dominate the measured window.
        ks = a.day_vehicle_steps / max(1, veh_steps)
        kv = (a.day_departures / max(1, n_veh)) if a.day_departures else ks
        print(f"\nPROJECTION -- basis: per-vehicle-step streams x {ks:.1f} "
              f"({a.day_vehicle_steps:.0f} day vehicle-steps / {veh_steps} measured); "
              f"per-vehicle streams x {kv:.1f}"
              + (f" ({a.day_departures:.0f} day departures / {n_veh} measured)"
                 if a.day_departures else " (same factor -- pass --day-departures to separate them)"))
        proj = 0.0
        for rel, n in sorted(files.items(), key=lambda kv2: -kv2[1]):
            if n < 1024:
                continue
            v = n * (ks if rel in PER_STEP_STREAMS else kv)
            proj += v
            print(f"    {rel:<44}{v / 1e9:>8.2f} GB")
        print(f"    {'WHOLE DAY':<44}{proj / 1e9:>8.2f} GB")
    return 0


#: Which output streams grow with VEHICLE-STEPS. Everything else in a dataset is one row per
#: vehicle, per attacker or per certificate and grows with DEPARTURES, which over a day is a very
#: different multiplier -- the InTAS day has 185,923 departures but of order 1e8 vehicle-steps.
PER_STEP_STREAMS = frozenset({
    "ground_truth/gt_emissions_sample.jsonl",
    "ground_truth/gt_mobility_oracle.jsonl",
    "ground_truth/gt_report_labels.jsonl",
    "ma/ma_reports.jsonl",
})


def _extent(man: dict, sim_s_override: float = 0.0) -> tuple[float, int]:
    """(simulated seconds, simulated vehicle-steps) for a finished dataset.

    Neither is in the manifest as such, and the obvious readings are both wrong on a long run:
    `config.n_steps` is the FIXED-FLEET step count and stays at its default (40) whenever
    `duration_s > 0`, and `mobility.provider.n_rows` is the TRACE's row count, which counts steps
    the run never reached when it was interrupted. `counts.mobility_survivorship` is written from
    the loop itself and is the one that stays true either way."""
    surv = man.get("counts", {}).get("mobility_survivorship", {})
    veh_steps = int(surv.get("vehicle_steps_simulated") or 0)
    if not veh_steps:
        veh_steps = int(man.get("mobility", {}).get("provider", {}).get("n_rows") or 0)
    cfg = man.get("config", {})
    sim_s = float(sim_s_override or cfg.get("duration_s")
                  or (cfg.get("n_steps", 0) * cfg.get("dt", 1.0)))
    return max(1e-9, sim_s), veh_steps


def cmd_verify(a) -> int:
    """Is this dataset a VALID dataset, or the wreckage of a killed process?

    The distinction matters most exactly where long runs live. A hard kill (Ctrl-Break, an OOM, a
    lost RDP session) leaves the streamed `.jsonl` files on disk with no `manifest.json` at all --
    no config, no digest, no side files, nothing that says how much of the run they cover. The
    engine's graceful SIGINT path leaves a complete dataset for the steps it did run. This tells the
    two apart by RE-DERIVING the digest from the bytes rather than by trusting the manifest."""
    import hashlib
    man_path = os.path.join(a.dataset, "manifest.json")
    if not os.path.exists(man_path):
        print(f"INVALID: {a.dataset} has no manifest.json -- this is a killed run's leftovers, "
              f"not a dataset ({len(dir_bytes(a.dataset))} files present)")
        return 1
    with open(man_path, encoding="utf-8") as fh:
        man = json.load(fh)
    outs = man.get("outputs", [])
    bad = []
    h = hashlib.sha256()
    for row in sorted(outs, key=lambda r: r["path"]):
        p = os.path.join(a.dataset, row["path"])
        if not os.path.exists(p):
            bad.append((row["path"], "MISSING"))
            continue
        fh2 = hashlib.sha256()
        with open(p, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                fh2.update(chunk)
        got = fh2.hexdigest()
        if got != row["sha256"]:
            bad.append((row["path"], f"{row['sha256'][:12]} != {got[:12]}"))
        h.update(row["path"].encode())
        h.update(got.encode())
    digest = h.hexdigest()
    declared = man.get("data_digest_sha256", "")
    ok = not bad and digest == declared
    print(f"{a.dataset}")
    print(f"  outputs           {len(outs)} declared, {len(outs) - len(bad)} verified")
    print(f"  data_digest       {'MATCH' if digest == declared else 'MISMATCH'} "
          f"({declared[:16]} vs {digest[:16]})")
    cfg = man.get("config", {})
    print(f"  config            duration_s={cfg.get('duration_s')} dt={cfg.get('dt')} "
          f"emit_sample_prob={cfg.get('emit_sample_prob')}")
    surv = man.get("counts", {}).get("mobility_survivorship", {})
    if surv:
        print(f"  survivorship      {surv.get('vehicle_steps_survival_frac')} of "
              f"{surv.get('vehicle_steps_simulated')} vehicle-steps kept in the BROADCAST record; "
              f"oracle rows {surv.get('oracle_rows')}")
    for p, why in bad:
        print(f"  BAD {p}: {why}")
    print("VALID" if ok else "INVALID")
    return 0 if ok else 1


_RPT_T = re.compile(rb'"generation_time":([0-9.eE+-]+)')
_RPT_R = re.compile(rb'"reporter_cert_digest":"([0-9a-f]+)"')


def cmd_dynamics(a) -> int:
    """What the SCMS layer does as the run LENGTHENS -- the half a 300 s window cannot show.

    Four curves, all read from a finished dataset, all bucketed on simulated time:

      * CRL growth (`ma/ma_crl_events.jsonl`) -- entries against issue time.
      * revocations per bucket AND their precision (`ground_truth/gt_linkage_revocation.jsonl`
        carries `should_have_been_revoked`, so precision is exact per bucket, not a run average).
      * report volume and the number of DISTINCT reporters per bucket (`ma/ma_reports.jsonl`).
      * **reporters past `report_budget`.** `run.trusted()` gates on `filed_by[cert] <=
        report_budget` and `received_by[cert] < reputation_max`, and both dicts are CUMULATIVE for
        the life of the certificate -- never decayed, never windowed. They are named rate limits and
        they are lifetime caps. In flow mode a cert lives one trip, so the cap binds by TRIP
        LENGTH rather than by run length; a station whose trip is long enough crosses it and is
        silently dropped from the trusted-reporter pool for the rest of that trip, after which
        `report_threshold_k` distinct trusted reporters is that much harder to reach. This counts
        how often that actually happens, so the claim is a measurement and not a reading of the
        source."""
    ds = a.dataset
    b = a.bucket_s
    with open(os.path.join(ds, "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    cfg = man.get("config", {})
    budget, repmax = cfg.get("report_budget", 30), cfg.get("reputation_max", 40)

    crl = []
    p = os.path.join(ds, "ma", "ma_crl_events.jsonl")
    if os.path.exists(p):
        with open(p, encoding="utf-8") as fh:
            for ln in fh:
                if ln.strip():
                    r = json.loads(ln)
                    crl.append((float(r["issue_time"]), int(r["num_entries"])))

    revs = []
    p = os.path.join(ds, "ground_truth", "gt_linkage_revocation.jsonl")
    if os.path.exists(p):
        with open(p, encoding="utf-8") as fh:
            for ln in fh:
                if ln.strip():
                    r = json.loads(ln)
                    revs.append((float(r["true_revocation_time"]),
                                 bool(r["should_have_been_revoked"])))

    # ma_reports is the big one (729 MB for the InTAS peak hour), so it is scanned with two
    # targeted regexes rather than parsed as JSON: 1.16 kB of row for two fields.
    filed: dict[bytes, int] = {}
    over_at: dict[bytes, float] = {}
    per_bucket: dict[int, dict] = {}
    p = os.path.join(ds, "ma", "ma_reports.jsonl")
    n_rows = n_over = 0
    if os.path.exists(p):
        with open(p, "rb") as fh:
            for raw in fh:
                mt, mr = _RPT_T.search(raw), _RPT_R.search(raw)
                if not mt or not mr:
                    continue
                t, who = float(mt.group(1)), mr.group(1)
                n_rows += 1
                k = int(t // b)
                d = per_bucket.setdefault(k, {"reports": 0, "reporters": set(),
                                              "by_over_budget": 0})
                d["reports"] += 1
                d["reporters"].add(who)
                c = filed.get(who, 0) + 1
                filed[who] = c
                if c > budget:
                    d["by_over_budget"] += 1
                    n_over += 1
                    over_at.setdefault(who, t)

    tmax = max([t for t, _ in revs] + [t for t, _ in crl] + [0.0])
    if per_bucket:
        tmax = max(tmax, (max(per_bucket) + 1) * b)
    print(f"{ds}: {n_rows} reports, {len(filed)} distinct reporter certs, "
          f"{len(revs)} revocations, bucket {b:.0f} s, horizon {tmax:.0f} s")
    print(f"report_budget={budget} reputation_max={repmax} "
          f"(both are LIFETIME counters in run.trusted(), not windowed rates)")
    print(f"reporter certs that crossed report_budget: {len(over_at)} of {len(filed)} "
          f"({len(over_at) / max(1, len(filed)):.4f}); reports they filed after crossing: "
          f"{n_over} of {n_rows} ({n_over / max(1, n_rows):.4f}) -- these count for the report "
          f"volume and NOT toward report_threshold_k")
    print()
    print(f"{'t0':>8}{'reports':>10}{'reporters':>11}{'over-budget':>13}{'revoked':>9}"
          f"{'true-pos':>10}{'precision':>11}{'CRL':>8}")
    nb = int(math.ceil(tmax / b)) if tmax > 0 else 0
    for k in range(nb):
        lo, hi = k * b, (k + 1) * b
        d = per_bucket.get(k, {"reports": 0, "reporters": set(), "by_over_budget": 0})
        rr = [ok for t, ok in revs if lo <= t < hi]
        crl_now = max([n for t, n in crl if t < hi], default=0)
        prec = (sum(rr) / len(rr)) if rr else None
        print(f"{lo:>8.0f}{d['reports']:>10d}{len(d['reporters']):>11d}"
              f"{d['by_over_budget']:>13d}{len(rr):>9d}{sum(rr):>10d}"
              f"{(f'{prec:.3f}' if prec is not None else '-'):>11}{crl_now:>8d}")
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8") as fh:
            json.dump({"dataset": ds, "bucket_s": b, "report_budget": budget,
                       "reputation_max": repmax, "reports": n_rows,
                       "reporter_certs": len(filed), "over_budget_certs": len(over_at),
                       "reports_from_over_budget": n_over,
                       "buckets": [{"t0": k * b, "reports": v["reports"],
                                    "reporters": len(v["reporters"]),
                                    "by_over_budget": v["by_over_budget"]}
                                   for k, v in sorted(per_bucket.items())],
                       "revocations": [{"t": t, "true_positive": ok} for t, ok in revs],
                       "crl": [{"t": t, "entries": n} for t, n in crl]}, fh, sort_keys=True)
    return 0


def cmd_withheld(a) -> int:
    """When does an ISOLATED detector's 384 MiB withheld-oracle budget run out?

    `run.WITHHELD_MEMORY_BYTES` is a budget shared across gt_report_labels + gt_emissions_sample
    (+ gt_mobility_oracle when on). Past it the overflow spills XOR-sealed to `.withheld/*.sealed`
    and is decrypted on commit. This solves the measured per-second rate of those exact streams for
    the run length at which the budget is exhausted."""
    files = dir_bytes(a.dataset)
    with open(os.path.join(a.dataset, "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    cfg = man.get("config", {})
    sim_s, _veh = _extent(man, a.sim_s)
    names = ["ground_truth/gt_report_labels.jsonl", "ground_truth/gt_emissions_sample.jsonl",
             "ground_truth/gt_mobility_oracle.jsonl"]
    from scms_sim_ref.mock_pipeline.run import WITHHELD_MEMORY_BYTES
    rate = sum(files.get(n, 0) for n in names) / max(1e-9, sim_s)
    print(f"withheld streams present: "
          f"{[n.rsplit('/', 1)[1] for n in names if files.get(n)]}")
    for n in names:
        if files.get(n):
            print(f"  {n.rsplit('/', 1)[1]:<34}{files[n] / 1e6:>10.2f} MB"
                  f"{files[n] / max(1e-9, sim_s) / 1e3:>10.2f} kB/sim-s")
    print(f"  {'TOTAL withheld':<34}{rate * sim_s / 1e6:>10.2f} MB{rate / 1e3:>10.2f} kB/sim-s")
    budget = WITHHELD_MEMORY_BYTES
    print(f"\nWITHHELD_MEMORY_BYTES = {budget} ({budget / 1048576:.0f} MiB)")
    if rate > 0:
        print(f"budget exhausted after {budget / rate:.0f} simulated seconds "
              f"({budget / rate / 60:.1f} min) at this concurrency and emit_sample_prob="
              f"{cfg.get('emit_sample_prob')}")
        print(f"a {int(a.horizon)} s run at this rate would withhold "
              f"{rate * a.horizon / 1e9:.2f} GB, i.e. "
              f"{max(0.0, rate * a.horizon - budget) / 1e9:.2f} GB spilled sealed to disk")
    return 0


# --------------------------------------------------------------------------- CLI ---------------- #
def _add_scene(p):
    p.add_argument("--net", required=True, help="the SUMO .net.xml (the whole city)")
    p.add_argument("--sumocfg", required=True, help="the scenario .sumocfg carrying the day demand")
    p.add_argument("--buildings", default="", help="the footprint polygon additional-file")
    p.add_argument("--base", default="", help="base config JSON or a run's manifest.json")
    p.add_argument("--set", action="append", default=[], help="config override key=json; repeatable")
    p.add_argument("--dt", type=float, default=1.0)
    p.add_argument("--seed", type=int, default=42)
    p.add_argument("--substeps", type=int, default=10,
                   help="SUMO integration substeps per engine step (InTAS is calibrated at 0.1 s)")
    p.add_argument("--warmup", type=int, default=900, metavar="STEPS",
                   help="unrecorded fill steps before the window (SUMO discards pre-begin departures)")
    p.add_argument("--time-to-teleport", type=float, default=300.0,
                   help="InTAS's own policy is 300 s; -1 changes the scenario")
    p.add_argument("--sample-every", type=int, default=25, metavar="STEPS",
                   help="wall/RSS telemetry sampling cadence (0 = off)")
    p.add_argument("--verbose", action="store_true")


def main(argv=None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv and argv[0] == "_child":
        return _child_main(argv[1:])
    p = argparse.ArgumentParser(prog="day_run", description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    q = sub.add_parser("profile", help="demand profile from a SUMO summary-output")
    q.add_argument("--summary", required=True)
    q.add_argument("--bucket", type=float, default=900.0)
    q.add_argument("--json", dest="json_out", default="")
    q.add_argument("--buckets", action="store_true", help="print every bucket, not just phases")
    q.set_defaults(fn=cmd_profile)

    q = sub.add_parser("ladder", help="cost envelope vs concurrent vehicles")
    _add_scene(q)
    q.add_argument("--at", required=True, help="comma-separated window start times (s into the day)")
    q.add_argument("--steps", type=int, default=120, help="recorded steps per arm")
    q.add_argument("--work", required=True)
    q.add_argument("--force", action="store_true", help="re-freeze even if the trace exists")
    q.set_defaults(fn=cmd_ladder)

    q = sub.add_parser("freeze", help="freeze one window's trace (parallelisable)")
    _add_scene(q)
    q.add_argument("--at", type=float, required=True)
    q.add_argument("--steps", type=int, required=True)
    q.add_argument("--work", required=True)
    q.add_argument("--out", default="")
    q.add_argument("--tag", default="")
    q.add_argument("--force", action="store_true")
    q.add_argument("--json", dest="json_out", default="")
    q.set_defaults(fn=cmd_freeze)

    q = sub.add_parser("window", help="one window, freeze + run + measure")
    _add_scene(q)
    q.add_argument("--at", type=float, required=True)
    q.add_argument("--duration", type=float, required=True)
    q.add_argument("--out", required=True)
    q.add_argument("--work", required=True)
    q.add_argument("--tag", default="")
    q.add_argument("--trace", default="", help="reuse an existing frozen trace instead of freezing")
    q.add_argument("--force", action="store_true")
    q.add_argument("--json", dest="json_out", default="")
    q.add_argument("--sigint-at", type=float, default=0.0, metavar="WALL_S",
                   help="send a REAL Ctrl-C to the child this many wall-seconds in")
    q.add_argument("--abort-at", type=int, default=0, metavar="STEP",
                   help="take the engine's graceful-interrupt path at this step (deterministic)")
    q.set_defaults(fn=cmd_window)

    q = sub.add_parser("day", help="the chunked long run")
    _add_scene(q)
    q.add_argument("--begin", type=float, default=0.0)
    q.add_argument("--duration", type=float, default=DAY_S)
    q.add_argument("--window", type=float, default=1800.0, help="chunk length (s)")
    q.add_argument("--out", required=True)
    q.add_argument("--resume", action="store_true", default=True)
    q.add_argument("--no-resume", dest="resume", action="store_false")
    q.add_argument("--drop-traces", action="store_true",
                   help="delete each window's trace once its dataset is finalised")
    q.set_defaults(fn=cmd_day)

    q = sub.add_parser("volume", help="output-volume accounting for a finished dataset")
    q.add_argument("dataset")
    q.add_argument("--day-vehicle-steps", type=float, default=0.0,
                   help="the day's total vehicle-steps, to project the day's volume")
    q.add_argument("--day-departures", type=float, default=0.0,
                   help="the day's vehicle departures, for the per-vehicle streams (InTAS: 185923)")
    q.add_argument("--sim-s", type=float, default=0.0,
                   help="simulated seconds actually run (needed only for an INTERRUPTED dataset)")
    q.set_defaults(fn=cmd_volume)

    q = sub.add_parser("dynamics", help="CRL growth, revocation precision and reporter saturation "
                                        "against simulated time")
    q.add_argument("dataset")
    q.add_argument("--bucket-s", type=float, default=300.0)
    q.add_argument("--json", dest="json_out", default="")
    q.set_defaults(fn=cmd_dynamics)

    q = sub.add_parser("verify", help="re-derive a dataset's digest from its bytes")
    q.add_argument("dataset")
    q.set_defaults(fn=cmd_verify)

    q = sub.add_parser("withheld", help="when the 384 MiB isolated-detector budget runs out")
    q.add_argument("dataset")
    q.add_argument("--horizon", type=float, default=86400.0)
    q.add_argument("--sim-s", type=float, default=0.0,
                   help="simulated seconds actually run (needed only for an INTERRUPTED dataset)")
    q.set_defaults(fn=cmd_withheld)

    a = p.parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    raise SystemExit(main())

