"""Measure what the ETSI protocol stack costs and what it buys, one arm per subprocess.

Every arm is a full pipeline run with a named set of protocol layers switched on, launched in its
OWN process so the wall-clock and the peak working set belong to that arm and nothing else. The
script writes one JSON record per arm; `protocol_stack_report.py` turns a directory of them into the
tables in `docs/realism/PROTOCOL-STACK-MEASURED.md`.

Nothing here touches `src/`. It drives the engine through `PipelineConfig` exactly as the CLI does,
and reads back only what the run itself wrote: the manifest's `counts["protocol"]` block and the
ground-truth emission stream.

    python tools/protocol_stack_measure.py --arm base --base C:/Temp/smob2/cfg_py300.json \
        --out C:/Temp/pstack/base --json C:/Temp/pstack/base.json

    python tools/protocol_stack_measure.py --list          # the arm table

The per-arm overrides live in ARMS below so the matrix is data, not a shell script.

RUN THE COST ARMS SERIALLY, on an otherwise idle box. Measured on this host: two independent `base`
runs agree to 0.05 %, but the same `full` arm came out at 131.578 s with three other arms in flight
against 112.062 s alone -- 17.4 % of pure co-tenancy on sixteen cores running single-threaded work.
The record carries `cpu_s` beside `wall_s` for exactly this reason; where they disagree by more than
a rounding error the box was not idle and the row is not a cost measurement.
"""
from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import gc
import json
import math
import os
import statistics
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
_SRC = os.path.join(os.path.dirname(_HERE), "src")
if _SRC not in sys.path:
    sys.path.insert(0, _SRC)


# --------------------------------------------------------------------------- peak RSS ----------- #
class _PMC(ctypes.Structure):
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t)]


def _mem_probe():
    """`K32GetProcessMemoryInfo` from kernel32 -- the redirected export that exists on Server 2022.

    `windll.psapi.GetProcessMemoryInfo` resolves on paper and returns 0 here, which is how the first
    pass of this script silently reported a 0.0 MiB peak for every arm.
    """
    k32 = ctypes.WinDLL("kernel32")
    fn = getattr(k32, "K32GetProcessMemoryInfo")
    fn.argtypes = [wt.HANDLE, ctypes.POINTER(_PMC), wt.DWORD]
    fn.restype = wt.BOOL
    return k32, fn


def peak_rss_mb() -> float:
    """Peak working set of THIS process, MiB. Windows only; 0.0 elsewhere."""
    try:
        k32, fn = _mem_probe()
        c = _PMC()
        c.cb = ctypes.sizeof(c)
        if not fn(k32.GetCurrentProcess(), ctypes.byref(c), c.cb):
            return 0.0
        return c.PeakWorkingSetSize / (1024.0 * 1024.0)
    except Exception:
        return 0.0


# --------------------------------------------------------------------------- the arm matrix ----- #
#: name -> config overrides. Every arm is the SAME base scenario with a different set of layers on,
#: so a difference between two arms is the layer and nothing else.
ARMS: dict[str, dict] = {
    # --- the stack, layer by layer, on the InTAS base ---
    "base":            {},
    "cam":             {"cam_generation_rules": True},
    "codec":           {"message_codec": "etsi_cam_en302637_2"},
    "cam_codec":       {"cam_generation_rules": True, "message_codec": "etsi_cam_en302637_2"},
    "cam_codec_dcc":   {"cam_generation_rules": True, "message_codec": "etsi_cam_en302637_2",
                        "dcc": True},
    "cam_dcc_nocodec": {"cam_generation_rules": True, "dcc": True},
    "cam_codec_dcc_lat": {"cam_generation_rules": True, "message_codec": "etsi_cam_en302637_2",
                          "dcc": True, "net_latency_model": True},
    "latency":         {"net_latency_model": True, "message_codec": "etsi_cam_en302637_2"},
    "cam_latency":     {"cam_generation_rules": True, "net_latency_model": True,
                        "message_codec": "etsi_cam_en302637_2"},
    "ecdsa":           {"security_model": "ecdsa"},
    "ecdsa_codec":     {"security_model": "ecdsa", "message_codec": "etsi_cam_en302637_2"},
    "full":            {"cam_generation_rules": True, "message_codec": "etsi_cam_en302637_2",
                        "dcc": True, "net_latency_model": True, "security_model": "ecdsa"},
    # --- the two declaration seams: what asking for them explicitly costs ---
    "profile_explicit": {"protocol_profile": "etsi_its_g5",
                         "message_codec": "etsi_cam_en302637_2"},
    # The profile with NO layer on: byte-identical to `base`, but it publishes the measured CBR --
    # which on a codec-less run is the engine's own `PHY_FRAME_AIRTIME_S` assumption, and is the
    # only way to read that constant's effect on the same scene as the codec's real airtime.
    "profile_only":     {"protocol_profile": "etsi_its_g5"},
    "report_v1":        {"report_format": "ma_report_v1"},
    "report_ts103759":  {"report_format": "ts103759_shape",
                         "message_codec": "etsi_cam_en302637_2"},
    # --- signer arms: what the TS 103 097 envelope costs on the air ---
    "codec_cert":      {"message_codec": "etsi_cam_en302637_2", "message_signer": "certificate"},
    "codec_nosig":     {"message_codec": "etsi_cam_en302637_2", "message_signer": "none"},
}


def build_config(base_path: str, overrides: dict, out_dir: str):
    from scms_sim_ref.mock_pipeline.run import PipelineConfig, config_from_dict
    if base_path:
        with open(base_path, encoding="utf-8-sig") as fh:
            d = json.load(fh)
    else:
        d = {}
    d.update(overrides)
    d["out_dir"] = out_dir
    d["verbose"] = False
    cfg = config_from_dict(d) if base_path else PipelineConfig(**d)
    return cfg


# --------------------------------------------------------------------------- gap distribution --- #
def gap_distribution(dataset_dir: str) -> dict:
    """Exact inter-packet gap distribution, rebuilt from the emission stream itself.

    The manifest publishes a mean and a max; a *distribution* needs the packets. `emit_sample_prob`
    is asserted to be 1.0 by the caller, so every CAM the engine put on the air is one row here and
    the gaps are the run's own, not a sample of them.
    """
    path = os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")
    per: dict[str, list[float]] = {}
    n_rows = 0
    with open(path, encoding="utf-8") as fh:
        for ln in fh:
            if not ln.strip():
                continue
            r = json.loads(ln)
            if r.get("msg_type") not in (None, "", "cam"):
                continue
            vid = str(r.get("true_vehicle_id"))
            per.setdefault(vid, []).append(float(r["t"]))
            n_rows += 1
    gaps: list[float] = []
    for vid, ts in per.items():
        ts.sort()
        gaps.extend(round(b - a, 6) for a, b in zip(ts, ts[1:]))
    if not gaps:
        return {"gaps": 0, "cams": n_rows, "stations": len(per)}
    gaps.sort()

    def q(p):
        i = min(len(gaps) - 1, max(0, int(math.ceil(p * len(gaps))) - 1))
        return gaps[i]

    hist: dict[str, int] = {}
    for g in gaps:
        hist[f"{g:.1f}"] = hist.get(f"{g:.1f}", 0) + 1
    return {
        "gaps": len(gaps), "cams": n_rows, "stations": len(per),
        "mean_gap_s": round(sum(gaps) / len(gaps), 6),
        "median_gap_s": round(statistics.median(gaps), 6),
        "min_gap_s": gaps[0], "max_gap_s": gaps[-1],
        "p10_s": round(q(0.10), 6), "p25_s": round(q(0.25), 6), "p75_s": round(q(0.75), 6),
        "p90_s": round(q(0.90), 6), "p99_s": round(q(0.99), 6),
        "harmonic_rate_hz": round(len(gaps) / sum(gaps), 6),
        "share_at_t_gen_cam_min": round(sum(1 for g in gaps if g <= 0.1000001) / len(gaps), 6),
        "share_at_t_gen_cam_max": round(sum(1 for g in gaps if g >= 0.9999) / len(gaps), 6),
        "hist_0p1s": dict(sorted(hist.items(), key=lambda kv: float(kv[0]))),
    }


def per_window_cam_counts(dataset_dir: str, window_s: float = 1.0) -> dict:
    """CAMs per station per `window_s` -- the N that bounds the awareness shot multiplicity Z.

    `awareness.z_for_engine` derives N from `dt`, which is right only when a station emits every
    step. Under the generation rules it does not, so N has to be COUNTED. Windows are whole and the
    trailing partial window is dropped, and a station is counted only in windows where it is
    actually present (it has at least one CAM), because a vehicle that has not spawned yet has no
    awareness to measure.
    """
    path = os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")
    counts: dict[tuple, int] = {}
    tmax = 0.0
    with open(path, encoding="utf-8") as fh:
        for ln in fh:
            if not ln.strip():
                continue
            r = json.loads(ln)
            if r.get("msg_type") not in (None, "", "cam"):
                continue
            t = float(r["t"])
            tmax = max(tmax, t)
            k = (str(r.get("true_vehicle_id")), int(math.floor(t / window_s)))
            counts[k] = counts.get(k, 0) + 1
    last = int(math.floor(tmax / window_s))
    vals = [v for (_, w), v in counts.items() if w < last]
    if not vals:
        return {"windows": 0}
    vals.sort()
    n = len(vals)
    return {"windows": n, "window_s": window_s,
            "mean_cams_per_window": round(sum(vals) / n, 6),
            "median": vals[n // 2], "p10": vals[max(0, int(0.10 * n) - 1)],
            "p90": vals[min(n - 1, int(0.90 * n))], "max": vals[-1],
            "hist": {str(k): vals.count(k) for k in sorted(set(vals))[:16]}}


# --------------------------------------------------------------------------- one arm ------------ #
def run_arm(name: str, base_path: str, out_dir: str, extra: dict | None = None) -> dict:
    from scms_sim_ref.mock_pipeline.run import run_pipeline
    if name not in ARMS and not extra:
        # An unknown name used to fall through to `{}` and quietly re-run `base` under the wrong
        # label -- which is exactly what happened when a comma-separated list reached here as one
        # string. A typo has to be an error, not four minutes of a mislabelled baseline.
        raise SystemExit(f"unknown arm {name!r}; known: {sorted(ARMS)}")
    over = dict(ARMS.get(name, {}))
    over.update(extra or {})
    cfg = build_config(base_path, over, out_dir)
    gc.collect()
    rss0 = peak_rss_mb()
    t0 = time.perf_counter()
    cpu0 = time.process_time()
    res = run_pipeline(cfg)
    wall = time.perf_counter() - t0
    cpu = time.process_time() - cpu0
    rss1 = peak_rss_mb()

    man = {}
    mp = os.path.join(res.out_dir, "manifest.json")
    if os.path.exists(mp):
        with open(mp, encoding="utf-8") as fh:
            man = json.load(fh)
    counts = man.get("counts", {})
    rec = {
        "arm": name, "overrides": over,
        "wall_s": round(wall, 3), "cpu_s": round(cpu, 3),
        "peak_rss_mb": round(rss1, 1), "peak_rss_mb_before": round(rss0, 1),
        "data_digest": res.data_digest,
        "vehicles": res.n_vehicles, "reports": res.n_reports,
        "investigations": res.n_investigations, "revoked": res.n_revoked,
        "protocol": counts.get("protocol", {}),
        "out_dir": res.out_dir,
        "dt": cfg.dt, "duration_s": cfg.duration_s,
    }
    try:
        rec["gap"] = gap_distribution(res.out_dir)
        rec["window"] = per_window_cam_counts(res.out_dir)
    except (OSError, ValueError, KeyError) as e:
        rec["gap_error"] = f"{type(e).__name__}: {e}"
    return rec


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--arm", default="")
    p.add_argument("--base", default="", help="base config JSON (the InTAS replay config)")
    p.add_argument("--out", default="")
    p.add_argument("--json", dest="json_out", default="")
    p.add_argument("--set", action="append", default=[],
                   help="extra config override, key=json_value; repeatable")
    p.add_argument("--list", action="store_true")
    a = p.parse_args(argv)
    if a.list:
        for k, v in ARMS.items():
            print(f"{k:18s} {json.dumps(v, sort_keys=True)}")
        return 0
    if not a.arm:
        p.error("--arm is required (or --list)")
    extra = {}
    for s in a.set:
        k, _, v = s.partition("=")
        try:
            extra[k] = json.loads(v)
        except json.JSONDecodeError:
            extra[k] = v
    rec = run_arm(a.arm, a.base, a.out or f"C:/Temp/pstack/{a.arm}", extra)
    txt = json.dumps(rec, indent=1, sort_keys=True)
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(txt + "\n")
    print(txt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
