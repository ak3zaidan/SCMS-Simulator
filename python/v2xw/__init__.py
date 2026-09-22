"""The V2X World Simulator, from Python.

Five things, and the shortest form of each::

    import v2xw

    scenario = v2xw.Scenario.load("scenarios/downtown.yaml")   # load and validate
    run = v2xw.run(scenario, recording="run.mcap")             # run it
    pdr = run.metrics.series("pdr")                            # read a metric
    rec = v2xw.Recording.open("run.mcap")                      # open the recording
    table = rec.arrow("node.tx")                               # a channel as Arrow

and, when the model you want does not exist yet, write it::

    class MyModel(v2xw.plugins.CarFollowing):
        card = v2xw.plugins.card(id="mobility/car-following/mine", ...)
        def accel(self, ego, leader, lane, weather): ...

    v2xw.conformance.check(MyModel()).raise_for_failures()

Everything here delegates to the compiled module ``v2xw._v2xw``. This file is thin on
purpose; the three places it does real work are marked, and each is something that has to
happen on the Python side:

* :func:`run` fills in ``build_utc``. The engine may not read a wall clock
  (02-architecture.md §6.1), so the manifest timestamp is the caller's. Reading it here, in
  the caller's process, is the whole reason the argument exists.
* :func:`read_ipc` turns an Arrow IPC buffer into a ``pyarrow`` table, so a caller without
  ``pyarrow`` gets a clear message rather than an ``ImportError`` from deep inside.
* ``v2xw.plugins`` and ``v2xw.conformance`` add the Python-side halves of the plug-in seam:
  the base classes a researcher subclasses, and the purity checks that can only be made in
  Python.
"""

from __future__ import annotations

import datetime as _datetime
from typing import TYPE_CHECKING, Any, Optional

from . import _v2xw as _native

if TYPE_CHECKING:  # pragma: no cover - typing only
    from typing import Sequence

__all__ = [
    "Scenario",
    "Recording",
    "Run",
    "Metrics",
    "run",
    "run_twice",
    "read_ipc",
    "math",
    "plugins",
    "conformance",
    "V2xwError",
    "ScenarioError",
    "RecordingError",
    "MetricError",
    "DeterminismError",
    "__version__",
]

__version__: str = _native.__version__

Scenario = _native.Scenario
Recording = _native.Recording
Run = _native.Run
Metrics = _native.Metrics

V2xwError = _native.V2xwError
ScenarioError = _native.ScenarioError
RecordingError = _native.RecordingError
MetricError = _native.MetricError
DeterminismError = _native.DeterminismError

math = _native.math
"""The engine's own transcendentals. A plug-in uses these instead of Python's ``math``."""

run_twice = _native.run_twice
"""Run a scenario twice and return both record-stream digests — the determinism gate."""


def _now_utc() -> str:
    """The current instant, ISO 8601 UTC.

    **This is the only wall-clock read in the package**, and it is here rather than in the
    engine on purpose. The manifest carries one timestamp so a reader knows when a run was
    produced; nothing in the simulation may depend on it, and the engine excludes it from
    every digest. Keeping the read in the caller's process is what makes that separation
    visible instead of a promise.
    """
    return (
        _datetime.datetime.now(_datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def run(
    scenario: Any,
    *,
    build_utc: Optional[str] = None,
    recording: Optional[str] = None,
    metrics: bool = True,
    metric_window_s: float = 1.0,
) -> Any:
    """Run ``scenario`` and return a :class:`Run`.

    Args:
        scenario: a :class:`Scenario`, or a path to one.
        build_utc: the manifest timestamp. Defaults to now, read here (see
            :func:`_now_utc`). Pass a fixed string to make a run's manifest byte-identical
            across invocations.
        recording: where to write the MCAP recording, or ``None`` for no file.
        metrics: whether to register the metric providers.
        metric_window_s: the metric aggregation window in simulated seconds. ``0``
            collapses the run to one sample per metric; the default gives a per-second
            series.

    Raises:
        ScenarioError: the scenario will not build.
        RecordingError: the recording cannot be written.
        MetricError: a metric provider refused its own definition.
        V2xwError: a phase failed.
    """
    if isinstance(scenario, str):
        scenario = Scenario.load(scenario)
    return _native.run(
        scenario,
        build_utc=build_utc if build_utc is not None else _now_utc(),
        recording=recording,
        metrics=metrics,
        metric_window_s=metric_window_s,
    )


def read_ipc(buffer: bytes) -> Any:
    """Read an Arrow IPC stream buffer as a ``pyarrow.Table``.

    ``Metrics.ipc`` and ``Recording.table`` return this buffer. It is one allocation and no
    per-row Python objects, and ``polars.read_ipc_stream`` will take it directly if you
    would rather not have ``pyarrow``.

    Raises:
        ImportError: if ``pyarrow`` is not installed, with the name of the extra that
            installs it.
    """
    try:
        import pyarrow as pa
    except ImportError as exc:  # pragma: no cover - environment dependent
        raise ImportError(
            "reading an Arrow IPC buffer needs pyarrow: pip install 'v2xw[arrow]'. "
            "Without it, the buffer is still a valid Arrow IPC stream — polars.read_ipc_stream "
            "reads it, and so does writing it to a .arrows file."
        ) from exc
    with pa.ipc.open_stream(pa.py_buffer(buffer)) as reader:
        return reader.read_all()


from . import conformance, plugins  # noqa: E402  (they import this module's names)
