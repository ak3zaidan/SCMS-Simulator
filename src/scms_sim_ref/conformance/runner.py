"""Run a contract without pytest, and render the `conformance_report.json` the manifest embeds.

One implementation, two drivers. Every check is a plain `check_<id>()` method that raises
`AssertionError` on failure; pytest's `test_<id>()` wrappers call it, and so does this runner. That
is deliberate: a suite whose CLI and whose pytest surface can disagree about what "passed" means is
worse than no suite, because the manifest would then carry a claim nobody can reproduce.

The summary this produces is what belongs in `manifest["plugins"]["loaded"][*]["conformance"]`, so
*"this dataset was produced by a conformant plugin"* becomes a machine-checkable property of the
artifact rather than a sentence in a README.
"""
from __future__ import annotations

import json
import time
import traceback

from .v1 import harness as _H
from .v1.channel import CHANNEL_CHECKS, SUITE_VERSION, ChannelModelContract, CheckSkipped
from .v1.detect import DETECT_CHECKS, CheckContract

#: slot -> (contract class, ordered check ids). `--slot` on the CLI reads this, so registering a
#: contract here is all it takes for `scms-poc conformance --slot check --ref ...` to work.
CONTRACTS = {"channel_model": (ChannelModelContract, CHANNEL_CHECKS),
             "check": (CheckContract, DETECT_CHECKS)}

PASS, FAIL, SKIP, WAIVED, ERROR = "PASS", "FAIL", "SKIP", "WAIVED", "ERROR"


class ConformanceReport:
    """The result of one contract run. `ok` is the CI answer; `to_dict()` is the manifest field."""

    __slots__ = ("slot", "ref", "params", "suite", "interface_version", "rows", "seconds")

    def __init__(self, slot, ref, params, suite, interface_version, rows, seconds):
        self.slot, self.ref, self.params = slot, ref, dict(params or {})
        self.suite, self.interface_version = suite, interface_version
        self.rows, self.seconds = rows, seconds

    # -- queries ------------------------------------------------------------------------------ #
    def count(self, status) -> int:
        return sum(1 for r in self.rows if r["status"] == status)

    @property
    def ok(self) -> bool:
        """A waived failure is not a pass, but it is not a refusal either -- that is exactly what a
        waiver IS, and the justification travels with it into the report."""
        return self.count(FAIL) == 0 and self.count(ERROR) == 0

    @property
    def waived(self) -> list:
        return [r["check"] for r in self.rows if r["status"] == WAIVED]

    # -- rendering ---------------------------------------------------------------------------- #
    def summary(self) -> dict:
        """The compact form embedded in the manifest, shaped as the design's section 4.2 draws it."""
        out = {"suite": self.suite, "interface_version": self.interface_version,
               "passed": self.count(PASS), "failed": self.count(FAIL),
               "skipped": self.count(SKIP), "errored": self.count(ERROR),
               "waived": self.waived, "ok": self.ok}
        if self.waived:
            out["waivers"] = {r["check"]: r["detail"] for r in self.rows
                              if r["status"] == WAIVED}
        return out

    def to_dict(self) -> dict:
        return {"slot": self.slot, "ref": self.ref, "params": self.params,
                "suite": self.suite, "interface_version": self.interface_version,
                "seconds": round(self.seconds, 3), "summary": self.summary(),
                "checks": self.rows}

    def to_text(self) -> str:
        width = max((len(r["check"]) for r in self.rows), default=10)
        lines = [f"conformance {self.suite} :: {self.slot} :: {self.ref}"]
        for r in self.rows:
            detail = f"  {r['detail']}" if r["detail"] else ""
            lines.append(f"  {r['status']:<6} {r['check']:<{width}}{detail}")
        s = self.summary()
        lines.append(f"  -- {s['passed']} passed, {s['failed']} failed, {s['skipped']} skipped, "
                     f"{len(s['waived'])} waived, {s['errored']} errored in {self.seconds:.1f}s")
        return "\n".join(lines)

    def write(self, path: str) -> str:
        with open(path, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(self.to_dict(), fh, indent=2, sort_keys=True)
            fh.write("\n")
        return path


def _is_skip(exc) -> bool:
    """pytest's `Skipped` derives from `BaseException`, not `Exception`, so it has to be recognised
    structurally rather than caught by type -- importing pytest here would make the runner depend on
    a test framework it exists to be independent of."""
    return isinstance(exc, CheckSkipped) or type(exc).__name__ in ("Skipped", "OutcomeException")


def run_contract(contract, checks=None) -> ConformanceReport:
    """Run every check on an already-built contract instance and return the report.

    Checks are run in DECLARED order and each one is independent: a failure never aborts the run,
    because "which of the twelve did it fail" is the whole diagnostic value.
    """
    checks = tuple(checks or CHANNEL_CHECKS)
    _validate_waivers(contract)
    rows = []
    # The PRISTINE `random.Random` surface, captured before any plugin code has run in this suite.
    # C3's class-surface trap compares against this rather than against a snapshot it takes itself:
    # a model that rebinds `random.Random.random` does it at CONSTRUCTION, which happens in C1, so a
    # snapshot taken inside C3 would already contain the tamper -- and an evasive patch is written
    # idempotently precisely so the second construction changes nothing. Restored in `finally`, so a
    # suite run against a hostile plugin never leaves the interpreter patched for whatever runs next.
    surface = _H.random_class_surface()
    contract._pristine_random = surface
    t0 = time.perf_counter()
    try:
        for check_id in checks:
            fn = getattr(contract, "check_" + check_id, None)
            if fn is None:
                rows.append(_row(check_id, SKIP, "not implemented by this contract", 0.0))
                continue
            c0 = time.perf_counter()
            try:
                detail = fn()
                rows.append(_row(check_id, PASS, str(detail) if detail else "",
                                 time.perf_counter() - c0))
            except AssertionError as e:
                why = _waiver(contract, check_id)
                if why:
                    rows.append(_row(check_id, WAIVED, f"{why} [underlying: {_one_line(e)}]",
                                     time.perf_counter() - c0))
                else:
                    rows.append(_row(check_id, FAIL, _one_line(e), time.perf_counter() - c0))
            except BaseException as e:                      # noqa: BLE001 - Skipped is BaseException
                if _is_skip(e):
                    rows.append(_row(check_id, SKIP, _one_line(e), time.perf_counter() - c0))
                elif isinstance(e, Exception):
                    rows.append(_row(check_id, ERROR,
                                     f"{type(e).__name__}: {_one_line(e)}\n"
                                     + "".join(traceback.format_exc(limit=6)).strip()[-900:],
                                     time.perf_counter() - c0))
                else:
                    raise
    finally:
        _H.restore_random_class_surface(surface)
    return ConformanceReport(getattr(contract, "SLOT", "channel_model"),
                             getattr(contract, "REF", None), getattr(contract, "PARAMS", {}),
                             SUITE_VERSION, getattr(contract, "INTERFACE_VERSION", ""),
                             rows, time.perf_counter() - t0)


def run_ref(slot: str, ref: str, params=None, *, seed: int = None, waivers=None,
            radio_range_m: float = None, exclude=()) -> ConformanceReport:
    """Build a contract for `ref` and run it. This is what the CLI calls.

    `exclude` genuinely SKIPS the named checks rather than running and discarding them. The engine's
    in-run attestation needs that for C12, which runs two full pipelines: filtering its row out after
    the fact would still execute it from inside the pipeline it is attesting.
    """
    if slot not in CONTRACTS:
        raise ValueError(f"no conformance contract for slot {slot!r}; have {sorted(CONTRACTS)}")
    base, checks = CONTRACTS[slot]
    if exclude:
        checks = tuple(c for c in checks if c not in set(exclude))
    attrs = {"REF": ref, "PARAMS": dict(params or {}), "waivers": dict(waivers or {})}
    if seed is not None:
        attrs["SEED"] = int(seed)
    if radio_range_m is not None:
        attrs["RADIO_RANGE_M"] = float(radio_range_m)
    contract = type("AdHocContract", (base,), attrs)()
    return run_contract(contract, checks)


def _waiver(contract, check_id):
    """The effective waiver: the contract subclass's own first, then the one the IMPLEMENTATION
    itself declares (`SomeModel.conformance_waivers` -- Django's `django_test_skips`)."""
    fn = getattr(contract, "waiver_for", None)
    if callable(fn):
        return fn(check_id)
    return (getattr(contract, "waivers", None) or {}).get(check_id)


def _all_waivers(contract) -> dict:
    fn = getattr(contract, "declared_waivers", None)
    declared = dict(fn() or {}) if callable(fn) else {}
    declared.update(getattr(contract, "waivers", None) or {})
    return declared


def _validate_waivers(contract) -> None:
    """A waiver without a written justification is refused. The Django mechanism only works because
    the excuse is DATA that lands in the report next to the artifact."""
    for check_id, why in _all_waivers(contract).items():
        if not isinstance(why, str) or not why.strip():
            raise ValueError(f"waiver for {check_id!r} carries no written justification; a waiver "
                             f"is a declared, recorded limitation, not a mute button")


def _row(check_id, status, detail, seconds):
    return {"check": check_id, "status": status, "detail": detail, "seconds": round(seconds, 3)}


def _one_line(e) -> str:
    return " ".join(str(e).split())[:600]
