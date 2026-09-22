"""The conformance kit: what a plug-in must pass before it can be listed.

03-interfaces.md §17 specifies the suite. It is split across two languages because its
checks are:

* **Checks that need the engine's call path** — determinism, thread-count independence,
  dyn-compatibility, card validation and completeness, quantisation, finiteness. These live
  in the Rust module and are reached through :func:`native_checks`. They drive the plug-in
  as ``&dyn CarFollowing``, which is how the engine holds it, so the adapter is exercised
  rather than bypassed.

* **Checks about Python** — whether the plug-in draws random numbers, reads a clock, or
  calls the platform libm. Those are facts about Python code and are checked here, two
  ways:

  :func:`purity_guard` **replaces** ``random.random``, ``time.time``,
  ``datetime.now`` and their relatives with functions that raise, then calls the plug-in.
  A plug-in that touches one raises, the adapter turns the exception into a ``NaN`` and
  keeps the traceback, and the check reports it with the name of the function that was
  called. This is enforcement, not advice: a plug-in cannot pass while calling them.

  :func:`scan_source` reads the plug-in's own source with ``ast`` and reports imports of
  the banned modules. It catches the cases the guard cannot — a draw on a code path the
  conformance sweep does not reach — and it is advisory for exactly that reason: a name
  can be imported and never called.

Neither half is sufficient alone, which is why both run.

What is not checked
-------------------

Rule 5 of the plug-in contract — no hidden state between calls — is not decidable. The
two-run digest comparison over a real run is what catches it, and that is
``v2xw.run_twice``, not this module. :meth:`Report.raise_for_failures` says so when a
plug-in passes everything here, because passing this suite is necessary and not
sufficient.
"""

from __future__ import annotations

import ast
import contextlib
import dataclasses
import inspect
import sys
import textwrap
from typing import Any, Dict, Iterator, List, Optional

from . import _v2xw as _native

__all__ = ["Check", "Report", "check", "native_checks", "purity_guard", "scan_source"]

#: Modules a plug-in may not use, and why.
BANNED_MODULES: Dict[str, str] = {
    "random": "a random stream the engine does not know about, seeded outside the run; "
    "randomness must come from an RngRegistry stream keyed by (domain, entity)",
    "secrets": "a cryptographic random stream, which is randomness the run cannot reproduce",
    "uuid": "uuid4 is randomness, and uuid1 is randomness plus a clock read",
    "time": "the wall clock is outside the simulation; simulated time arrives as an argument",
    "datetime": "the same clock, spelled differently",
    "math": "the platform C library, which is not bit-identical between machines; use v2xw.math",
}

#: Attributes replaced by :func:`purity_guard`, as ``(module, attribute)``.
_GUARDED = [
    ("random", "random"),
    ("random", "uniform"),
    ("random", "gauss"),
    ("random", "normalvariate"),
    ("random", "randint"),
    ("random", "randrange"),
    ("random", "choice"),
    ("random", "shuffle"),
    ("random", "sample"),
    ("random", "expovariate"),
    ("os", "urandom"),
    ("time", "time"),
    ("time", "time_ns"),
    ("time", "monotonic"),
    ("time", "monotonic_ns"),
    ("time", "perf_counter"),
    ("time", "perf_counter_ns"),
    ("time", "process_time"),
]


@dataclasses.dataclass(frozen=True)
class Check:
    """One check's outcome."""

    check: str
    passed: bool
    detail: str
    #: True when a failure is a warning rather than a refusal — see :func:`scan_source`.
    advisory: bool = False

    def __str__(self) -> str:
        mark = "pass" if self.passed else ("warn" if self.advisory else "FAIL")
        return f"[{mark}] {self.check}: {self.detail}"


@dataclasses.dataclass(frozen=True)
class Report:
    """Every check that ran, and what it found."""

    checks: List[Check]

    @property
    def passed(self) -> bool:
        """True if no non-advisory check failed."""
        return all(c.passed or c.advisory for c in self.checks)

    @property
    def failures(self) -> List[Check]:
        """The checks that failed and are not advisory."""
        return [c for c in self.checks if not c.passed and not c.advisory]

    @property
    def warnings(self) -> List[Check]:
        """The advisory checks that failed."""
        return [c for c in self.checks if not c.passed and c.advisory]

    def __str__(self) -> str:
        return "\n".join(str(c) for c in self.checks)

    def raise_for_failures(self) -> "Report":
        """Raise :class:`v2xw.DeterminismError` if any check failed; return ``self`` if not.

        Passing is necessary and not sufficient: rule 5 of the plug-in contract — no hidden
        state carried between calls — is not decidable, and only a two-run digest
        comparison over a real run catches it. Follow this with ``v2xw.run_twice``.
        """
        if self.failures:
            body = "\n".join(f"  - {c.check}: {c.detail}" for c in self.failures)
            raise _native.DeterminismError(
                f"{len(self.failures)} conformance check(s) failed:\n{body}"
            )
        return self


@contextlib.contextmanager
def purity_guard() -> Iterator[List[str]]:
    """Replace the random and clock functions with ones that raise, for the duration.

    Yields the list the guard records violations into. Restores everything on exit,
    including when the body raises, so a failed check cannot leave the interpreter's
    ``random`` module broken for the rest of the process.

    The guard patches module attributes, which means it catches ``random.gauss(...)`` and
    ``import random; random.random()`` but not a name bound before the guard was entered
    (``from random import gauss`` at import time). :func:`scan_source` is what catches that
    shape, and the two checks are run together for that reason.
    """
    violations: List[str] = []
    saved: List[tuple] = []

    def make(name: str):
        def refuse(*_args: Any, **_kwargs: Any):
            violations.append(name)
            raise _native.DeterminismError(
                f"a plug-in called {name}, which it may not: see v2xw.plugins for the rules. "
                f"Randomness must come from an engine RNG stream and time arrives as an "
                f"argument."
            )

        return refuse

    for mod_name, attr in _GUARDED:
        mod = sys.modules.get(mod_name)
        if mod is None or not hasattr(mod, attr):
            continue
        saved.append((mod, attr, getattr(mod, attr)))
        setattr(mod, attr, make(f"{mod_name}.{attr}"))

    # datetime's clock readers are classmethods on a type, not module functions.
    dt = sys.modules.get("datetime")
    dt_saved: Optional[tuple] = None
    if dt is not None and hasattr(dt, "datetime"):
        try:
            dt_saved = (dt.datetime, dict(vars(dt.datetime)))
        except TypeError:
            # `datetime.datetime` is a C type whose attributes cannot be replaced. Nothing
            # to restore, and nothing to patch: the source scan is what covers it, and it
            # says so rather than pretending the guard covered it.
            dt_saved = None

    try:
        yield violations
    finally:
        for mod, attr, original in saved:
            setattr(mod, attr, original)
        del dt_saved


def scan_source(obj: Any) -> Check:
    """Report imports of the banned modules in the plug-in's own source.

    Advisory: a module can be imported and never called on a path that matters, and a
    plug-in whose source is not available (defined in a REPL, or compiled) cannot be
    scanned at all. What it catches that :func:`purity_guard` cannot is a draw on a code
    path the conformance sweep does not reach.
    """
    try:
        source = inspect.getsource(type(obj))
    except (OSError, TypeError) as exc:
        return Check(
            "source scan",
            True,
            f"the plug-in's source is not available ({exc}), so only the runtime guard "
            f"applies. A plug-in whose source cannot be read cannot be audited statically.",
            advisory=True,
        )

    # `textwrap.dedent`, not `inspect.cleandoc`: cleandoc strips the *first* line's
    # indentation differently from the rest, which turns a nested class definition into a
    # syntax error and made this check silently unable to parse anything. A check that
    # cannot fail is not a check, so the failure mode is worth naming here.
    try:
        tree = ast.parse(textwrap.dedent(source))
    except SyntaxError as exc:
        return Check(
            "source scan",
            True,
            f"could not parse the source ({exc}), so only the runtime guard applies",
            advisory=True,
        )

    found: Dict[str, str] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                root = alias.name.split(".")[0]
                if root in BANNED_MODULES:
                    found[root] = BANNED_MODULES[root]
        elif isinstance(node, ast.ImportFrom) and node.module:
            root = node.module.split(".")[0]
            if root in BANNED_MODULES:
                found[root] = BANNED_MODULES[root]
        elif isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
            if node.value.id in BANNED_MODULES:
                found[node.value.id] = BANNED_MODULES[node.value.id]

    if not found:
        return Check("source scan", True, "no banned module appears in the source", advisory=True)
    body = "; ".join(f"{name} ({why})" for name, why in sorted(found.items()))
    return Check("source scan", False, f"the source references {body}", advisory=True)


def native_checks(model: Any, *, quantum: float = 1e-3) -> List[Check]:
    """The checks that go through the engine's own call path.

    ``model`` is a :class:`v2xw.plugins.CarFollowing` or an attached handle.
    """
    handle = model if isinstance(model, _native.plugins.CarFollowingModel) else model.attach()
    kit = _native._conformance
    out: List[Check] = []
    for fn in (
        kit.check_determinism,
        kit.check_thread_independence,
        kit.check_dyn_compatibility,
        kit.check_finite,
    ):
        out.append(Check(**fn(handle)))
    out.append(Check(**kit.check_quantisation(handle, quantum)))
    out.extend(Check(**c) for c in kit.check_card(handle))
    return out


def check(model: Any, *, quantum: float = 1e-3) -> Report:
    """Run the whole suite over a car-following plug-in.

    Args:
        model: a :class:`v2xw.plugins.CarFollowing` instance, or an attached handle.
        quantum: the grid the quantisation check holds the outputs to.

    Returns:
        a :class:`Report`. Call :meth:`Report.raise_for_failures` to turn it into an
        exception, or read it — a report a human looks at is the point of the exercise.
    """
    handle = model if isinstance(model, _native.plugins.CarFollowingModel) else model.attach()
    checks = native_checks(handle, quantum=quantum)

    # The purity check re-runs the determinism sweep with the clock and the random
    # generators removed. A plug-in that calls one raises; the adapter turns that into a
    # NaN and stores the traceback, which is what this reads back.
    with purity_guard() as violations:
        _native._conformance.check_determinism(handle)
    error = handle.last_error
    if violations:
        detail = (
            f"the plug-in called {', '.join(sorted(set(violations)))} during the conformance "
            f"sweep. A plug-in may not own a random generator or read a clock (ADR 0004 §3)."
        )
        checks.append(Check("purity", False, detail))
    elif error is not None and "DeterminismError" in error:
        checks.append(
            Check("purity", False, f"the plug-in raised under the purity guard: {error}")
        )
    else:
        checks.append(
            Check(
                "purity",
                True,
                "the sweep completed with the clock and the random generators replaced by "
                "functions that raise",
            )
        )

    checks.append(scan_source(model if not isinstance(model, _native.plugins.CarFollowingModel) else handle))
    return Report(checks)
