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

THE VERDICT IS AUTHENTICATED, AND IT HAS TO BE
----------------------------------------------
The child's report used to be JSON written to a FILE whose path was in the child's own ``argv``.
The candidate's ``__init__`` runs in that child, so an ``atexit`` handler that rewrote the file
after ``main()`` had written it was enough: a plugin that genuinely failed conformance
(``ok=False``, three checks failed) was recorded in the dataset manifest as ``passed: 99``,
``ok: true``, ``integrity.ok: true``. Measured, exactly as the previous version of this docstring
predicted it would be.

Three changes close it, and they only work together:

1. **There is no report file.** The parent creates the child's stdout PIPE, the child writes the
   report to it, and the parent writes the file. Nothing on disk can be overwritten because nothing
   is on disk, and there is no path in ``argv`` for a candidate to read.
2. **The report is framed by a SECRET the parent generated.** The parent sends a 128-bit token on
   the child's stdin; the child prints it on its own line ahead of the report; the parent accepts
   the report that follows the token and NOTHING else. A candidate that writes its own JSON to the
   inherited descriptor -- which it can -- produces bytes the parent does not read, because it does
   not know the token.
3. **The candidate cannot reach the token.** Two barriers: stdin is consumed and pointed at the null
   device before the candidate's module is imported, and the whole of the suite runs inside
   :class:`scms_sim_ref.api.guard.arm`, which refuses ``sys._getframe``, ``sys._current_frames`` and
   the ``gc`` object walkers from inside CPython -- so the token, which lives in a local of
   :func:`_run_child`, is not reachable by walking to it. The child then ``os._exit``\\ s the
   instant the report is written, so ``atexit`` handlers, ``__del__`` and daemon threads never run
   at all.

Residue, stated rather than left to be discovered: barrier 3 is the runtime guard, and
`api/guard.py` is explicit that it is not a sandbox -- a candidate that reaches
``sys.modules["scms_sim_ref.api.guard"]`` and disarms it can then walk to the token. What that costs
an attacker is a deliberate, single-purpose act against a named object, rather than four lines of
``atexit``. The honest boundary for code you do not trust at all is still the out-of-process
DETECTOR mode, where the oracle is not in the address space.
"""
from __future__ import annotations

import argparse
import json
import os
import secrets
import subprocess
import sys

from ..api.errors import ConfigError

#: The framing token's length in bytes. 16 is 128 bits: a candidate that has to guess it has to
#: guess it, and the parent refuses everything that is not it.
TOKEN_BYTES = 16

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
    token = secrets.token_hex(TOKEN_BYTES)
    payload = {"slot": str(slot), "ref": str(ref), "params": dict(params or {}),
               "exclude": [str(e) for e in exclude], "token": token}
    cmd = [sys.executable, "-m", "scms_sim_ref.conformance.attest", "--serve"]
    try:
        proc = subprocess.run(cmd, input=json.dumps(payload), capture_output=True, text=True,
                              timeout=timeout, cwd=os.getcwd(), env=_child_env())
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
    report = read_framed_report(proc.stdout or "", token)
    if report is None:
        raise ConfigError(
            f"plugins.{slot}.conformance=required: the attestation child produced no report the "
            f"engine could authenticate for {ref!r} (exit {proc.returncode}).\n"
            f"The report has to arrive on the child's stdout behind a one-time token this process "
            f"generated, exactly once; anything else -- no report, a malformed one, or a second "
            f"one -- is refused rather than read, because the candidate's own code runs in that "
            f"child and could otherwise write the verdict on itself.\n"
            f"--- child stderr ---\n{(proc.stderr or '').strip()[-2000:]}")
    return report


def read_framed_report(stream_text: str, token: str):
    """The report that follows `token` on its own line, or None.

    Exactly one framed report is accepted. Two means something else in the child wrote one, which
    is precisely the case that must not be resolved by picking a winner.
    """
    if not token:
        return None
    lines = (stream_text or "").splitlines()
    hits = [i for i, ln in enumerate(lines) if ln.strip() == token]
    if len(hits) != 1:
        return None
    try:
        return json.loads("\n".join(lines[hits[0] + 1:]))
    except ValueError:
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
    """Run the suite in THIS process, bracketed by an integrity sentinel AND the runtime guard.

    `guard.arm(imports=False, raising=False)` is the barrier that keeps the framing token out of the
    candidate's reach: it refuses `sys._getframe`, `sys._current_frames` and the `gc` object walkers
    for the whole time the candidate's code can run. `imports=False` because the suite it is running
    is this project's own and is entitled to import this project; `raising=False` because a reach is
    REPORTED here (in `integrity.reflection`) rather than converted into an exception -- the parent
    then refuses the plugin with the rest of the verdict, which is a better message than a
    traceback out of a check.
    """
    from ..api import guard as _guard
    from ..api import integrity as _integrity
    from .runner import run_ref

    sentinel = _integrity.Sentinel(armed=True)
    slot, ref = payload["slot"], payload["ref"]
    params, exclude = payload.get("params") or {}, tuple(payload.get("exclude") or ())
    before = _guard.violation_count()
    with _guard.arm(imports=False, raising=False):
        rep = run_ref(slot, ref, params, exclude=exclude)
    reached = _guard.violation_count() - before
    out = rep.to_dict()
    moved = sentinel.drift()
    sentinel.restore()
    out["integrity"] = {
        "ok": not moved and not reached,
        "tampered": [what for what, _why in moved],
        # Reflective reaches (a frame walk, a gc walk) made while being attested. Non-zero means the
        # candidate went looking for the attesting process's own state, which is a refusal in itself.
        "reflection": [event for event, _why in _guard.violations()[-reached:]] if reached else [],
    }
    return out


def _error_report(payload: dict, exc: BaseException) -> dict:
    import traceback
    return {"slot": payload.get("slot"), "ref": payload.get("ref"),
            "error": f"{type(exc).__name__}: {exc}",
            "traceback": traceback.format_exc()[-4000:],
            "summary": {"ok": False, "passed": 0, "failed": 0, "errored": 1,
                        "skipped": 0, "waived": []},
            "checks": []}


def _run_child(payload_text: str, out_fd: int) -> int:
    """Attest, frame the report with the parent's token, write it to `out_fd`.

    Split out of :func:`main` so it is testable in process. `token` is a LOCAL here on purpose: it
    exists only in this frame, and the frame is exactly what the guard armed in `_attest_here`
    refuses to hand out.
    """
    try:
        payload = json.loads(payload_text)
    except ValueError as e:
        print(f"attest: unreadable request on stdin: {e}", file=sys.stderr)
        return 3
    token = str(payload.pop("token", "") or "")
    if not token:
        print("attest: the request carried no framing token", file=sys.stderr)
        return 3
    try:
        report = _attest_here(payload)
    except BaseException as e:                                     # noqa: BLE001 - report anything
        report = _error_report(payload, e)
    body = (token + "\n" + json.dumps(report, indent=2, sort_keys=True) + "\n").encode("utf-8")
    try:
        os.write(out_fd, body)
    except OSError as e:                                           # pragma: no cover
        print(f"attest: cannot write the report: {e}", file=sys.stderr)
        return 3
    return 0


def main(argv=None) -> int:
    """``python -m scms_sim_ref.conformance.attest --serve``, reading the request on stdin.

    **The first thing it does is take the report's descriptor away from the candidate**, the same
    move `api/isolate.py` makes for the same reason: fd 1 is duplicated to a private descriptor and
    then pointed at stderr, and fd 0 -- which carried the token -- at the null device. A candidate
    that `print()`s cannot corrupt the report stream, and one that reads `input()` gets EOF.

    It ends with ``os._exit``: the report is on the pipe, and `atexit` handlers, `__del__` methods
    and daemon threads belonging to the candidate must not get a turn after it. The old file-based
    protocol lost exactly there.
    """
    argv = list(sys.argv[1:] if argv is None else argv)
    ap = argparse.ArgumentParser(prog="scms_sim_ref.conformance.attest",
                                 description="run one conformance contract, report on stdout")
    ap.add_argument("--serve", action="store_true",
                    help="read the JSON request on stdin, write the framed report on stdout")
    args = ap.parse_args(argv)
    if not args.serve:
        print("usage: python -m scms_sim_ref.conformance.attest --serve", file=sys.stderr)
        return 2
    payload_text = sys.stdin.read()
    out_fd = os.dup(1)
    os.dup2(2, 1)
    devnull = os.open(os.devnull, os.O_RDONLY)
    os.dup2(devnull, 0)
    os.close(devnull)
    sys.stdout = sys.stderr
    sys.stdin = open(os.devnull, encoding="utf-8")                 # noqa: SIM115 - process lifetime
    rc = _run_child(payload_text, out_fd)
    try:
        os.close(out_fd)
    except OSError:                                                # pragma: no cover
        pass
    # NOT `return rc`. `os._exit` skips atexit, __del__ and every daemon thread, which is the whole
    # point: the candidate's code has run in this interpreter and must not get a turn after the
    # verdict has been written.
    sys.stderr.flush()
    os._exit(rc)


if __name__ == "__main__":                                         # pragma: no cover
    raise SystemExit(main())
