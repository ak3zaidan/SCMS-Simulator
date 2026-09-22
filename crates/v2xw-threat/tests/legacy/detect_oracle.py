"""Execute the LEGACY detection pass, extracted from `run.py` at run time, over a claim
trace produced by the Rust port — and diff the two detector fingerprints message by
message.

Why the source is extracted rather than re-implemented: a comparison against formulas
typed into this file is a test of somebody's memory, and it passes when both copies drift
together. So the detection pass is located in `legacy/scms_sim_ref/mock_pipeline/run.py`
by anchor, sliced out verbatim, and `exec`uted. The same policy
`crates/v2xw-threat/tests/legacy_conformance.rs` uses for the constants, applied to the
code.

Two regions are extracted:

  * `def detectors(...)` — the six-check motion closure (`run.py`, inside `run_pipeline`);
  * the per-message detection block, from `tx, digest, cx, cy, ... = (b["veh"], ...` down to
    the `file_report(...)` call — the lagged reference, the heading baseline, the six
    radio/envelope checks, the alpha-beta tracker, the bad-signature and VRU gates, the
    streak gate and the reason ordering.

The block contains `continue`, so it is wrapped in a one-iteration `for` loop, where
`continue` means exactly what it means in the original: this message files no report.

Usage:  legacy_detect_oracle.py <trace.json> <out.json>
"""

import inspect
import json
import os
import math
import re
import sys
import textwrap

# The frozen reference lives at <repo>/legacy; this file is <repo>/crates/v2xw-threat/
# tests/legacy/, so the path is relative to it and the script moves with the repository.
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                                "..", "..", "..", "..", "legacy")))

from scms_sim_ref.mock_pipeline import run as legacy  # noqa: E402

SRC = inspect.getsource(legacy.run_pipeline)


def slice_block(src: str, start_anchor: str, end_anchor: str, include_end: bool) -> str:
    """The contiguous source from the line containing `start_anchor` to the line
    containing `end_anchor` (inclusive of the end's full statement when asked)."""
    lines = src.splitlines()
    start = next(i for i, l in enumerate(lines) if start_anchor in l)
    end = next(i for i, l in enumerate(lines) if end_anchor in l and i > start)
    if include_end:
        # Include the whole (possibly continued) statement.
        j = end
        depth = 0
        while j < len(lines):
            depth += lines[j].count("(") - lines[j].count(")")
            j += 1
            if depth <= 0:
                break
        end = j - 1
    else:
        end -= 1
    return "\n".join(lines[start:end + 1])


def extract_detectors_closure(src: str) -> str:
    lines = src.splitlines()
    start = next(i for i, l in enumerate(lines) if l.strip().startswith("def detectors("))
    indent = len(lines[start]) - len(lines[start].lstrip())
    j = start + 1
    while j < len(lines):
        l = lines[j]
        if l.strip() and (len(l) - len(l.lstrip())) <= indent:
            break
        j += 1
    return textwrap.dedent("\n".join(lines[start:j]))


DETECTORS_SRC = extract_detectors_closure(SRC)
BLOCK_SRC = slice_block(
    SRC,
    'tx, digest, cx, cy, cs, ch, conf = (b["veh"], b["digest"]',
    "file_report(t, reporter_digest, digest, tx, reasons, det, conf,",
    include_end=True,
)
# The block is indented to its position inside two nested `for` loops; dedent it and
# re-indent by four so it sits inside the `for _once in (0,):` wrapper.
BLOCK_SRC = textwrap.dedent(BLOCK_SRC)
BLOCK_WRAPPED = "for _once in (0,):\n" + textwrap.indent(BLOCK_SRC, "    ")

# The two operating-point lines, also read out of the source rather than typed here.
Z_LINE = textwrap.dedent(
    slice_block(SRC, "Z = cfg.detector_z_threshold",
                "MIN_CONSEC = cfg.detector_min_consec", include_end=True))


class _Cfg:
    """The subset of `PipelineConfig` the extracted code reads, at the values the trace
    declares. It IS a `PipelineConfig`, so a field the extraction needs and this comparison
    forgot raises rather than defaulting silently."""


def build_cfg(trace: dict):
    return legacy.PipelineConfig(**trace["cfg"])


class _Net:
    """The node's own map, as the trace's receiver has it: `_offroad` is the only thing the
    extracted block asks a network for."""

    def __init__(self, distances):
        self._d = distances

    def dist_to_road(self, x, y):
        return self._d


def main() -> int:
    trace = json.load(open(sys.argv[1], encoding="utf-8"))
    out_path = sys.argv[2]
    cfg = build_cfg(trace)

    ns = {"math": math, "cfg": cfg, "_ang_diff": legacy._ang_diff}
    exec(Z_LINE, ns)
    exec(DETECTORS_SRC, ns)
    detectors = ns["detectors"]
    Z = ns["Z"]
    MIN_CONSEC = ns["MIN_CONSEC"]

    DET_KEYS = tuple(trace["det_keys"])
    SOFT_KEYS = ("kalmanConsistency",)
    MOTION_KEYS = ("positionSpeedInconsistency", "positionJump", "headingInconsistency",
                   "constantPositionFrozen", "implausibleAcceleration")

    filed = []

    def file_report(t, reporter_digest, digest, tx, reasons, det, conf, cx, cy, px, py,
                    malicious=False, sig_valid=True, station_type="vehicle"):
        filed.append(dict(t=t, subject=digest, reasons=list(reasons)))

    class _Rng:
        def random(self):
            return 0.0            # report_prob is 1.0 in the trace; never gates

    class _Rx:
        vid = 0

    last_claimed: dict = {}
    rows = []

    for msg in trace["messages"]:
        b = dict(veh=None, digest=msg["digest"], cx=msg["cx"], cy=msg["cy"], cs=msg["cs"],
                 ch=msg["ch"], conf=msg["conf"], msg_count=msg["msg_count"], cg=msg["cg"],
                 sig_ok=msg["sig_ok"], cvf=msg["cvf"], cvt=msg["cvt"],
                 station_type=msg["station_type"], ghost=False)
        local = dict(
            b=b, t=msg["t"], step=msg["step"], rx=_Rx(), rxx=msg["rx_x"], rxy=msg["rx_y"],
            rr=msg["rr"], cells=msg["cells"], last_claimed=last_claimed,
            reporter_digest="rx", detectors=detectors, file_report=file_report,
            rng=_Rng(), DET_KEYS=DET_KEYS, SOFT_KEYS=SOFT_KEYS, MOTION_KEYS=MOTION_KEYS,
            Z=Z, MIN_CONSEC=MIN_CONSEC, cfg=cfg, math=math,
            _ang_diff=legacy._ang_diff,
            _offroad=(lambda x, y, d=msg["offroad_m"]: d),
        )
        # `cells` arrives as a {"key": count} map keyed by the legacy tuple's repr; rebuild
        # the tuple keys the extracted code indexes with, and default a miss to 0 the way a
        # `Counter` does.
        class _Cells(dict):
            def __missing__(self, k):
                return 0
        local["cells"] = _Cells({tuple(k): v for k, v in
                                 (( [int(a) for a in key.split(",")], val)
                                  for key, val in msg["cells"].items())})
        before = len(filed)
        exec(BLOCK_WRAPPED, local)
        det = local.get("det", {})
        rows.append(dict(
            i=msg["i"],
            det={k: round(det.get(k, 0.0), 3) for k in (*DET_KEYS, *SOFT_KEYS)},
            fired=sorted(local.get("fired", {}) or {},
                         key=lambda k: -det.get(k, 0.0)),
            reported=len(filed) > before,
        ))

    json.dump(dict(rows=rows,
                   detectors_src_sha=legacy_src_sha(),
                   det_keys=list(DET_KEYS)),
              open(out_path, "w", encoding="utf-8"), indent=1)
    print(f"ran the legacy detection pass over {len(rows)} messages -> {out_path}")
    return 0


def legacy_src_sha() -> str:
    import hashlib
    h = hashlib.sha256()
    h.update(DETECTORS_SRC.encode())
    h.update(BLOCK_SRC.encode())
    return h.hexdigest()[:16]


if __name__ == "__main__":
    raise SystemExit(main())
