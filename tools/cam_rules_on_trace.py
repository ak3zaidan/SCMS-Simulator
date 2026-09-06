"""Run EN 302 637-2 clause 6.1.3 straight over a frozen SUMO trace, with no engine in the way.

The engine's own CAM rate is a property of two things: the standard's state machine, and the
mobility it is fed. This isolates the second. It replays :class:`CamGenerationState` over the
trajectories in a `sumo_trace` artifact -- the same class `run.py` calls, imported not
re-implemented -- and reports the gap distribution, the trigger mix, AND the decomposition of *which
trigger produced which gap length*, which is what tells a 0.1 s spike apart from a 0.3 s bulk.

It is cheap (no channel, no PKI, no detectors), so a disagreement between two engines can be
attributed to the SCENARIO by running the identical rules over each scenario's own trajectories --
and, with `--rule`, to a specific line of one engine's implementation by running the identical
TRAJECTORIES under each engine's own rule:

    etsi     clause 6.1.3 as `codecs/etsi_rules.py` implements it, N_GenCam ratchet included
    java     `ScmsBeaconApp.onVehicleUpdated`'s flat 1.0 s heart-beat, everything else identical
    java_fp  the same, plus its floor test's own floating-point arithmetic

    python tools/cam_rules_on_trace.py C:/Temp/pstack/intas_amwarm_dt01.trace --json out.json
    python tools/cam_rules_on_trace.py trace --rule java_fp
    python tools/cam_rules_on_trace.py trace --decimate 2       # the same trace at dt = 0.2
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_SRC = os.path.join(os.path.dirname(_HERE), "src")
if _SRC not in sys.path:
    sys.path.insert(0, _SRC)

from scms_sim_ref.codecs.etsi_rules import (CamGenerationState, DYNAMICS_TRIGGERS,   # noqa: E402
                                            T_GEN_CAM_MAX_S, T_GEN_CAM_MIN_S)
from scms_sim_ref.mock_pipeline import sumo_trace as ST                              # noqa: E402

BUCKETS = ((0.0, 0.15), (0.15, 0.25), (0.25, 0.35), (0.35, 0.55), (0.55, 0.95), (0.95, 1.05),
           (1.05, 1e9))


def _bucket(g: float) -> str:
    for lo, hi in BUCKETS:
        if lo <= g < hi:
            return f"<{hi}" if lo == 0.0 else (f">={lo}" if hi > 1e8 else f"{lo}-{hi}")
    return "?"


class JavaRuleState:
    """`ScmsBeaconApp.onVehicleUpdated`'s trigger, transcribed, so the two rules can be run over the
    SAME trajectories.

    It differs from :class:`CamGenerationState` in exactly one place, and that place is the whole
    disagreement: the heart-beat test is `dtLast >= CAM_INTERVAL_S` with `CAM_INTERVAL_S` a FLAT
    1.0 s. Clause 6.1.3's `N_GenCam` ratchet -- after a dynamics trigger, `T_GenCam` becomes the
    elapsed interval and is held there for the next three CAMs -- is not implemented on the Java
    side, so it emits no shortened heart-beats at all. Everything else (the 4 m / 4 deg / 0.5 m/s
    thresholds, the reference being the LAST SENT state, the `T_GenCamMin` floor) is identical.
    """

    __slots__ = ("last_t", "last_x", "last_y", "last_speed", "last_heading")

    def __init__(self) -> None:
        self.last_t = None
        self.last_x = self.last_y = self.last_speed = self.last_heading = 0.0

    def evaluate(self, t, x, y, speed, heading, min_gap: float = T_GEN_CAM_MIN_S) -> str:
        if self.last_t is None:
            self._commit(t, x, y, speed, heading)
            return "first"
        dt = t - self.last_t
        pos = math.hypot(x - self.last_x, y - self.last_y) > 4.0
        hdg = abs((heading - self.last_heading + 180.0) % 360.0 - 180.0) > 4.0
        spd = abs(speed - self.last_speed) > 0.5
        beat = dt + 1e-9 >= T_GEN_CAM_MAX_S
        if not (pos or hdg or spd or beat):
            return ""
        if dt + 1e-9 < max(T_GEN_CAM_MIN_S, min_gap):
            return ""
        self._commit(t, x, y, speed, heading)
        return "position" if pos else ("heading" if hdg else ("speed" if spd else "heartbeat"))

    def _commit(self, t, x, y, speed, heading) -> None:
        self.last_t, self.last_x, self.last_y = t, x, y
        self.last_speed, self.last_heading = speed, heading


class JavaFpRuleState(JavaRuleState):
    """`JavaRuleState` with the Java floor test's ARITHMETIC as well as its logic.

    `ScmsBeaconApp` derives its clock as `tS = getSimulationTime() / 1e9` and tests
    `dtLast >= CAM_MIN_S` with no tolerance. Neither `tS` nor `0.1` is exactly representable, so the
    difference of two consecutive 100 ms instants lands below 0.1 for 60.5 % of the step pairs in a
    3000-step run and the floor spuriously rejects a CAM the trigger had already fired. Reproducing
    it here -- with `run()` feeding the same nanosecond-derived clock -- is what makes the residual
    disagreement between the two engines attributable rather than merely visible.
    """

    __slots__ = ()

    def evaluate(self, t, x, y, speed, heading, min_gap: float = T_GEN_CAM_MIN_S) -> str:
        if self.last_t is None:
            self._commit(t, x, y, speed, heading)
            return "first"
        dt = t - self.last_t
        pos = math.hypot(x - self.last_x, y - self.last_y) > 4.0
        hdg = abs((heading - self.last_heading + 180.0) % 360.0 - 180.0) > 4.0
        spd = abs(speed - self.last_speed) > 0.5
        beat = dt >= T_GEN_CAM_MAX_S
        if not (pos or hdg or spd or beat):
            return ""
        if not (dt >= max(T_GEN_CAM_MIN_S, min_gap)):        # NO epsilon: the Java test verbatim
            return ""
        self._commit(t, x, y, speed, heading)
        return "position" if pos else ("heading" if hdg else ("speed" if spd else "heartbeat"))


RULES = {"etsi": CamGenerationState, "java": JavaRuleState, "java_fp": JavaFpRuleState}


def run(path: str, decimate: int = 1, max_vehicles: int = 0, dcc_floor: float = 0.0,
        rule: str = "etsi") -> dict:
    tr = ST.load(path)
    dt = tr.dt * decimate
    if dt > T_GEN_CAM_MAX_S + 1e-9:
        raise SystemExit(f"decimated dt {dt} exceeds T_GenCamMax {T_GEN_CAM_MAX_S}")
    gaps: list[float] = []
    triggers: dict[str, int] = {}
    by_bucket: dict[str, dict[str, int]] = {}
    # Per-step dynamics of the FLEET, so the gap distribution can be read against the motion that
    # produced it rather than against an assumption about it.
    dv: list[float] = []
    dh: list[float] = []
    dd: list[float] = []
    n_cams = 0
    n_steps_eval = 0
    vehs = tr.vehicles[:max_vehicles] if max_vehicles else tr.vehicles
    for v in vehs:
        xs, ys, vs, angs = tr.series(v.idx)
        st = RULES[rule]()
        last_t = None
        px = py = pv = ph = None
        dt_ns = int(round(tr.dt * 1e9))
        for k in range(0, len(xs), decimate):
            # MOSAIC's own clock path when the Java arithmetic is being reproduced: the simulation
            # time is an integer nanosecond count divided by 1e9, and that division is where the
            # representation error the floor test trips over is introduced.
            t = (((v.first_step + k) * dt_ns) / 1e9 if rule.endswith("_fp")
                 else (v.first_step + k) * tr.dt)
            x, y, sp, hd = xs[k], ys[k], vs[k], angs[k]
            if px is not None:
                dv.append(abs(sp - pv))
                dh.append(abs((hd - ph + 180.0) % 360.0 - 180.0))
                dd.append(math.hypot(x - px, y - py))
            px, py, pv, ph = x, y, sp, hd
            n_steps_eval += 1
            r = st.evaluate(t, x, y, sp, hd, dcc_floor)
            if not r:
                continue
            n_cams += 1
            triggers[r] = triggers.get(r, 0) + 1
            if last_t is not None:
                g = round(t - last_t, 6)
                gaps.append(g)
                b = _bucket(g)
                by_bucket.setdefault(b, {})
                by_bucket[b][r] = by_bucket[b].get(r, 0) + 1
            last_t = t
    gaps.sort()
    n = len(gaps)

    def q(p):
        return gaps[min(n - 1, max(0, int(math.ceil(p * n)) - 1))] if n else None

    dyn = sum(c for k, c in triggers.items() if k in DYNAMICS_TRIGGERS)
    total = sum(gaps)

    def _pct(arr, p):
        if not arr:
            return None
        a = sorted(arr)
        return round(a[min(len(a) - 1, max(0, int(math.ceil(p * len(a))) - 1))], 4)

    return {
        "trace": os.path.basename(path), "sha256": tr.sha256, "rule": rule,
        "trace_dt_s": tr.dt, "evaluated_dt_s": round(dt, 6), "decimate": decimate,
        "vehicles": len(vehs), "vehicle_steps_evaluated": n_steps_eval,
        "cams": n_cams, "gaps": n,
        "duty_cycle_cams_per_vehicle_step": round(n_cams / n_steps_eval, 6) if n_steps_eval else 0,
        "mean_gap_s": round(total / n, 6) if n else None,
        "rate_hz": round(n / total, 6) if total else None,
        "median_s": q(0.50), "p05_s": q(0.05), "p25_s": q(0.25), "p75_s": q(0.75),
        "p95_s": q(0.95), "p99_s": q(0.99), "max_s": (gaps[-1] if n else None),
        "share_at_t_gen_cam_min": round(sum(1 for g in gaps if g <= dt + 1e-9) / n, 6) if n else 0,
        "share_at_t_gen_cam_max": round(sum(1 for g in gaps if g >= T_GEN_CAM_MAX_S - 1e-4) / n, 6)
        if n else 0,
        "triggers": dict(sorted(triggers.items())),
        "dynamics_share": round(dyn / n_cams, 6) if n_cams else 0,
        "gap_histogram": {k: sum(v.values()) for k, v in sorted(by_bucket.items())},
        "trigger_by_gap_bucket": {k: dict(sorted(v.items())) for k, v in sorted(by_bucket.items())},
        "fleet_dynamics_per_step": {
            "abs_dspeed_mps": {"p50": _pct(dv, 0.5), "p90": _pct(dv, 0.9), "p99": _pct(dv, 0.99),
                               "share_over_0p5": round(sum(1 for a in dv if a > 0.5) / len(dv), 6)
                               if dv else 0},
            "abs_dheading_deg": {"p50": _pct(dh, 0.5), "p90": _pct(dh, 0.9), "p99": _pct(dh, 0.99),
                                 "share_over_4deg": round(sum(1 for a in dh if a > 4.0) / len(dh),
                                                          6) if dh else 0},
            "step_distance_m": {"p50": _pct(dd, 0.5), "p90": _pct(dd, 0.9), "p99": _pct(dd, 0.99),
                                "share_over_4m": round(sum(1 for a in dd if a > 4.0) / len(dd), 6)
                                if dd else 0},
            "mean_speed_mps": round(sum(a for a in dd) / len(dd) / dt, 4) if dd else None,
        },
        "constants": {"t_gen_cam_min_s": T_GEN_CAM_MIN_S, "t_gen_cam_max_s": T_GEN_CAM_MAX_S},
    }


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("traces", nargs="+")
    p.add_argument("--decimate", type=int, default=1, help="evaluate every Nth step (dt = N*trace dt)")
    p.add_argument("--max-vehicles", type=int, default=0)
    p.add_argument("--rule", choices=sorted(RULES), default="etsi",
                   help="etsi = clause 6.1.3 WITH the N_GenCam ratchet (the engine's own state "
                        "machine); java = ScmsBeaconApp's flat-heartbeat variant")
    p.add_argument("--json", dest="json_out", default="")
    a = p.parse_args(argv)
    out = {}
    for t in a.traces:
        key = f"{os.path.basename(t)}@dt{a.decimate}:{a.rule}"
        out[key] = run(t, a.decimate, a.max_vehicles, rule=a.rule)
    txt = json.dumps(out, indent=1, sort_keys=True)
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(txt + "\n")
    print(txt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
