"""Writing a model in Python.

03-interfaces.md §15 says a researcher writes a class, gives it a model card, and the
engine calls it. This module is the Python half: the base classes to subclass, a helper for
building a card that validates, and the handles that put a Python object behind an engine
trait.

Two families are implemented, and they are the two ends of §15's spectrum:

``CarFollowing``
    Called once per vehicle per mobility step. Pure — views in, a number out, no context,
    no randomness. Allowed at the abstract tier only (§15 rule P2), and a run that uses one
    is a ``python-hot-path`` run: expect a large slowdown, because each call crosses the
    interpreter lock.

``Detector``
    Called once per window with the whole window as an Arrow record batch. This is the
    shape §15 prefers for everything it can batch, and the reason is arithmetic: one call
    per 10,000 messages instead of 10,000 calls.

What a plug-in may not do
-------------------------

A plug-in is a *function*, and the engine's reproducibility depends on it being one:

1. **No randomness of its own.** ``random``, ``numpy.random``, ``secrets`` and
   ``os.urandom`` are all streams the engine does not know about. Randomness in this
   simulator comes from streams keyed by ``(domain, entity)`` so that a draw does not
   depend on what was drawn before it (ADR 0004 §3), and neither family here is given a way
   to ask for one.
2. **No wall clock.** ``time.time``, ``time.perf_counter``, ``datetime.now``. Simulated
   time arrives as an argument.
3. **No platform libm.** ``math.exp``, ``x ** y`` on floats and ``numpy``'s ufuncs are the
   C library, which is not specified to the last bit and differs between machines. Use
   :mod:`v2xw.math`, which is the engine's own pure-Rust implementation.
4. **No iteration over a ``set``, or over a ``dict`` keyed by something hashed**, where the
   order reaches a result. Python randomises string hashing per process.
5. **No hidden state between calls.** The engine reorders and parallelises phases.

:mod:`v2xw.conformance` checks 1 to 4 and catches many instances of 5.
"""

from __future__ import annotations

import abc
from typing import Any, Dict, List, Optional, Sequence

from . import _v2xw as _native

_p = _native.plugins

__all__ = [
    "CarFollowing",
    "Detector",
    "Ego",
    "Lane",
    "Leader",
    "Weather",
    "DetectorCtx",
    "Observation",
    "CarFollowingModel",
    "DetectorModel",
    "ReferenceDetector",
    "card",
    "equation",
    "parameter",
    "source",
    "validate_card",
    "uncited_parameters",
    "PROBABILITY_QUANTUM",
]

Ego = _p.Ego
Lane = _p.Lane
Leader = _p.Leader
Weather = _p.Weather
DetectorCtx = _p.DetectorCtx
Observation = _p.Observation
CarFollowingModel = _p.CarFollowingModel
DetectorModel = _p.DetectorModel
ReferenceDetector = _p.ReferenceDetector
validate_card = _p.validate_card
uncited_parameters = _p.uncited_parameters
PROBABILITY_QUANTUM = _p.PROBABILITY_QUANTUM

#: The plug-in API version this package targets. A card declaring a different major version
#: is refused by the registry, which is how an interface change stops an out-of-date
#: plug-in rather than mis-calling it.
API_VERSION = "1.0.0"


def source(
    kind: str,
    ref: str,
    *,
    accessed: Optional[str] = None,
    note: Optional[str] = None,
) -> Dict[str, Any]:
    """Build the ``source`` entry of a card parameter.

    Args:
        kind: one of ``standard``, ``paper``, ``datasheet``, ``dataset``, ``code``,
            ``todo-calibrate``. There is deliberately no ``assumption``: a number you
            assumed is a number nobody has calibrated, which is what ``todo-calibrate``
            means, and it must carry a plan.
        ref: the reference itself — a standard and clause, a DOI, a datasheet name.
        accessed: ``YYYY-MM-DD``, where the reference is a URL.
        note: anything a reader needs in order to know how the reference was used.

    A default with no source is ``todo-calibrate``, and a ``todo-calibrate`` default must
    carry a ``calibration`` plan or the registry refuses the card. That rule is the reason
    this helper exists: it is easier to cite a source than to invent one, and a number
    nobody can defend should be visible as one.
    """
    out: Dict[str, Any] = {"kind": kind, "ref": ref}
    if accessed is not None:
        out["accessed"] = accessed
    if note is not None:
        out["note"] = note
    return out


def equation(
    name: str,
    latex_or_text: str,
    *,
    notes: Optional[str] = None,
) -> Dict[str, Any]:
    """Build one ``equations`` entry of a card.

    An equation carries no source of its own: the citation belongs on the card's
    ``sources`` list (for the model as a whole) or on the parameter whose value the
    reference fixes. ``notes`` is where the range of validity goes, and a model that has
    one and does not say so is the thing a card exists to prevent.
    """
    out: Dict[str, Any] = {"name": name, "latex_or_text": latex_or_text}
    if notes is not None:
        out["notes"] = notes
    return out


def parameter(
    name: str,
    unit: str,
    default: Any,
    src: Dict[str, Any],
    *,
    calibration: Optional[str] = None,
    value_range: Optional[Sequence[Any]] = None,
) -> Dict[str, Any]:
    """Build one ``parameters`` entry of a card.

    Every numeric parameter a plug-in reads at run time must be declared (invariant I-C3),
    including the ones it reads from its own defaults.
    """
    out: Dict[str, Any] = {
        "name": name,
        "unit": unit,
        "default": default,
        "source": src,
    }
    if calibration is not None:
        out["calibration"] = calibration
    if value_range is not None:
        out["range"] = list(value_range)
    return out


def card(
    *,
    id: str,
    family: str,
    version: str,
    purpose: str,
    tier: Sequence[str] = ("abstract",),
    equations: Sequence[Dict[str, Any]] = (),
    parameters: Sequence[Dict[str, Any]] = (),
    assumptions: Sequence[str] = (),
    limitations: Sequence[str] = (),
    ignores: Sequence[str] = (),
    sources: Sequence[Dict[str, Any]] = (),
    api_version: str = API_VERSION,
) -> Dict[str, Any]:
    """Build a model card and validate it immediately.

    Validated here rather than at attach time so that the traceback points at the card, not
    at the run. The returned value is the normalised card the registry will store.

    Raises:
        V2xwError: if the card does not validate, with the validator's own message naming
            the field.
    """
    doc: Dict[str, Any] = {
        "id": id,
        "family": family,
        "version": version,
        "api_version": api_version,
        "tier": list(tier),
        "purpose": purpose,
        "equations": [dict(e) for e in equations],
        "parameters": [dict(p) for p in parameters],
        # A Python plug-in has no way to reach a keyed RNG stream, so the only honest
        # answer here is "no". The conformance kit fails a card that says otherwise,
        # because such a card is either wrong about itself or describing a draw the plug-in
        # is making some other way.
        "determinism": {"uses_rng": False},
    }
    if assumptions:
        doc["assumptions"] = list(assumptions)
    if limitations:
        doc["limitations"] = list(limitations)
    if ignores:
        doc["ignores"] = list(ignores)
    if sources:
        doc["sources"] = [dict(s) for s in sources]
    return validate_card(doc)


class CarFollowing(abc.ABC):
    """A longitudinal (car-following) model.

    Implement :meth:`accel`. Attach the class attribute ``card``, built with :func:`card`
    and ``family="mobility"``.

    The parameters a longitudinal model needs — ``v0``, ``a``, ``b``, ``T``, ``s0`` — arrive
    on ``ego`` rather than being read from the card, because they are *per driver*: the
    engine draws them once per vehicle from the model's own calibration. Reading them off
    the ego is what makes the model itself stateless.
    """

    #: The model card. Required; a plug-in without one cannot be registered.
    card: Dict[str, Any]

    @abc.abstractmethod
    def accel(
        self,
        ego: Any,
        leader: Optional[Any],
        lane: Any,
        weather: Any,
    ) -> float:
        """The acceleration in m/s², for ``ego`` behind ``leader`` on ``lane``.

        ``leader`` is ``None`` on a free road. A stop line, a red signal, a junction to
        yield at and a curve-speed cap all arrive as a *virtual* leader with
        ``is_vehicle=False``, so one equation produces every deceleration the vehicle ever
        applies.

        Must be pure: the same arguments give the same result, on every machine and every
        thread. See the module note for what that rules out.
        """

    def profile(self, vehicle_class: str) -> Dict[str, float]:
        """The driver parameters this model's calibration gives a vehicle of that class.

        Override to supply your own calibration; return a mapping with the keys
        ``desired_speed_mps``, ``max_accel_mps2``, ``comfort_decel_mps2``,
        ``time_headway_s`` and ``min_gap_m``. The default is not implemented here at all:
        omitting it makes the engine fall back to the Kesting 2010 set that
        04-models.md §2.1 publishes as the medium-tier default, and the card should say
        that it borrows it.
        """
        raise NotImplementedError

    def attach(self) -> Any:
        """This model behind the engine's trait, as a :class:`CarFollowingModel` handle."""
        return CarFollowingModel(self)


class Detector(abc.ABC):
    """A local misbehaviour detector, called once per window with the whole window.

    Implement :meth:`on_messages`. Attach the class attribute ``card``, built with
    :func:`card` and ``family="detector"``.
    """

    #: The model card. Required.
    card: Dict[str, Any]

    @abc.abstractmethod
    def on_messages(self, ctx: Any, batch: Any) -> List[Any]:
        """What this detector concludes about the messages in ``batch``.

        Args:
            ctx: a :class:`DetectorCtx` — the simulated instant and the node this detector
                is running on. There is no RNG accessor on it, by design.
            batch: a ``pyarrow.RecordBatch`` of the window's received messages, one row per
                message, sharing the engine's buffers. Read it columnwise; a row-by-row loop
                over a ten-thousand-message window throws away the reason it is batched.

        Returns:
            a list of :class:`Observation`. An empty list means "this window looked fine",
            which is a result and not a failure — so if something went wrong, raise.
        """

    def attach(self) -> Any:
        """This detector behind the engine's trait, as a :class:`DetectorModel` handle."""
        return DetectorModel(self)
