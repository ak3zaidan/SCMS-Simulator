"""Attestation, run in a process the candidate plugin has not been able to touch.

**The defect this exists for.** `build_channel` used to instantiate the run's plugin and only then
call the conformance suite on it (`run.py`: instantiate, *then* `_attest`); the detector slot repeated
the ordering. So a hostile ``__init__`` executed **before** it was ever gated. It could rebind
``random.Random.random``, replace ``api.channel.check_outcome``, or -- decisively --
monkeypatch ``conformance.runner.run_ref`` or ``ConformanceReport.ok`` itself, and then "pass"
attestation, because the thing performing the attestation was a process the candidate had already
edited. **An attestation performed by code the subject can rewrite attests to nothing.**

Two changes close it, and both are needed:

1. **Order.** `_attest` now runs BEFORE the run's instance is constructed. The verdict is reached
   before the plugin has a constructor call in this interpreter at all.
2. **Place.** The suite runs in a **child interpreter** (this module, as ``python -m
   scms_sim_ref.conformance.attest``). Conformance must itself construct the candidate -- there is no
   way to grade a model without building one -- so *somewhere* a hostile ``__init__`` runs. Here it
   runs in a process that is discarded a moment later and that shares no object with the run. A
   plugin that patches the runner patches the child's runner, and the child's verdict is JSON on a
   file descriptor the parent alone interprets.

The child ALSO brackets the suite with :class:`~scms_sim_ref.api.integrity.Sentinel` and reports
what it saw, so "the candidate tampered with the interpreter while being attested" is a recorded
property of the attestation rather than something the parent has to infer.

Residue, stated rather than left to be discovered: the child's *report* is JSON written by a process
the plugin ran inside, so a sufficiently determined plugin can write whatever JSON it likes to that
file. The parent's defence against that is not this module -- it is that the run's OWN instance is
constructed under a sentinel in the parent (`api/integrity.py`), and that a plugin doing this has to
write a line that means nothing else. The honest boundary for code you do not trust is still the
out-of-process DETECTOR mode, where the oracle is not in the address space at all.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile

from ..api.errors import ConfigError

#: Wall-clock ceiling for one attestation. A contract suite is seconds; a plugin that hangs the
#: child is refused rather than allowed to hang the run.
TIMEOUT_S = 300.0


# --------------------------------------------------------------------------- #
# Parent side
# --------------------------------------------------------------------------- #
def run_out_of_process(slot: str, ref: str, params=None, *, exclude=(),
                       timeout: float = TIMEOUT_S) -> dict:
    """Run the v1 suite for `ref` in a child interpreter and return its report dict.

    Raises :class:`~scms_sim_ref.api.errors.ConfigError` if the child could not be run, timed out,
    crashed, or produced no report -- "the attestation did not happen" is never allowed to read as
    "the attestation passed".
    """
    payload = {"slot": str(slot), "ref": str(ref), "params": dict(params or {}),
               "exclude": [str(e) for e in exclude]}
    tmpdir = tempfile.mkdtemp(prefix="scms-attest-")
    in_path = os.path.join(tmpdir, "request.json")
    out_path = os.path.join(tmpdir, "report.json")
    try:
        with open(in_path, "w", encoding="utf-8") as fh:
            json.dump(payload, fh)
        cmd = [sys.executable, "-m", "scms_sim_ref.conformance.attest",
               "--payload", in_path, "--out", out_path]
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout,
                                  cwd=os.getcwd(), env=_child_env())
        except subprocess.TimeoutExpired:
            raise ConfigError(
                f"plugins.{slot}.conformance=required: attesting {ref!r} timed out after "
                f"{timeout:.0f}s. Attestation runs in a CHILD interpreter so the candidate cannot "
                f"edit the process that judges it; a candidate that never returns is refused.") \
                from None
        except OSError as e:
            raise ConfigError(
                f"plugins.{slot}.conformance=required: could not start the attestation child "
                f"({' '.join(cmd[:3])}...): {e}") from None
        report = _read_report(out_path)
        if report is None:
            raise ConfigError(
                f"plugins.{slot}.conformance=required: the attestation child produced no report "
                f"for {ref!r} (exit {proc.returncode}).\n"
                f"--- child stderr ---\n{(proc.stderr or '').strip()[-2000:]}")
        return report
    finally:
        for p in (in_path, out_path):
            try:
                os.unlink(p)
            except OSError:
                pass
        try:
            os.rmdir(tmpdir)
        except OSError:                                            # pragma: no cover
            pass


def _read_report(path: str):
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return None


def _child_env() -> dict:
    """The child's environment: this interpreter's `sys.path` carried over as `PYTHONPATH`.

    Deliberately NOT `-I`/`-E`: isolated mode ignores `PYTHONPATH`, and without it an out-of-repo
    plugin distribution that the parent can import (a `sys.path` entry a test fixture inserted, an
    editable install, a directory a user added) is unresolvable in the child -- attestation would
    then fail for reasons that have nothing to do with the plugin, which is the failure mode most
    likely to get the whole mechanism turned off. What the child must import is exactly what this
    process could import; carrying `sys.path` over verbatim is the direct way to say that.
    """
    env = dict(os.environ)
    entries, seen = [], set()
    for p in sys.path:
        if not isinstance(p, str) or not p:
            continue
        if p in seen:
            continue
        seen.add(p)
        entries.append(p)
    env["PYTHONPATH"] = os.pathsep.join(entries)
    env.pop("PYTHONSTARTUP", None)
    return env


# --------------------------------------------------------------------------- #
# Child side
# --------------------------------------------------------------------------- #
def _attest_here(payload: dict) -> dict:
    """Run the suite in THIS process, bracketed by an integrity sentinel."""
    from ..api import integrity as _integrity
    from .runner import run_ref

    sentinel = _integrity.Sentinel(armed=True)
    slot, ref = payload["slot"], payload["ref"]
    params, exclude = payload.get("params") or {}, tuple(payload.get("exclude") or ())
    rep = run_ref(slot, ref, params, exclude=exclude)
    out = rep.to_dict()
    moved = sentinel.drift()
    sentinel.restore()
    out["integrity"] = {
        "ok": not moved,
        "tampered": [what for what, _why in moved],
    }
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="scms_sim_ref.conformance.attest",
                                 description="run one conformance contract and write JSON")
    ap.add_argument("--payload", required=True, help="JSON request file")
    ap.add_argument("--out", required=True, help="JSON report file to write")
    args = ap.parse_args(argv)
    try:
        with open(args.payload, encoding="utf-8") as fh:
            payload = json.load(fh)
    except (OSError, ValueError) as e:
        print(f"attest: unreadable payload {args.payload}: {e}", file=sys.stderr)
        return 3
    try:
        report = _attest_here(payload)
    except BaseException as e:                                     # noqa: BLE001 - report anything
        import traceback
        report = {"slot": payload.get("slot"), "ref": payload.get("ref"),
                  "error": f"{type(e).__name__}: {e}",
                  "traceback": traceback.format_exc()[-4000:],
                  "summary": {"ok": False, "passed": 0, "failed": 0, "errored": 1,
                              "skipped": 0, "waived": []},
                  "checks": []}
    try:
        with open(args.out, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(report, fh, indent=2, sort_keys=True)
            fh.write("\n")
    except OSError as e:                                           # pragma: no cover
        print(f"attest: cannot write {args.out}: {e}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":                                         # pragma: no cover
    raise SystemExit(main())
