"""A RUNTIME reflection guard for in-process plugins -- the half `srcgate` could not do.

Read this before anything else.

:mod:`~scms_sim_ref.api.srcgate` parses a plugin's source and matches names, and its own docstring
says what that costs: ``getattr(sys, "_get" + "frame")`` is the same frame walk with the name
assembled at run time, and a static gate cannot see it. That gap was measured against this engine --
an obfuscated walk reaches ``b["veh"].is_attacker``, ``b["x"]``/``b["y"]``, ``b["falsified"]`` and
the engine's global RNG with the gate at its default ``"on"`` -- and reading the oracle moves
nothing, so the digests, the goldens, the lock and the integrity monitor are all structurally blind
to it.

This module closes the SPELLING half of that gap, and it closes it in C rather than in a parser.
CPython raises a PEP 578 audit event from inside ``sys._getframe`` itself, so the event fires
whatever name the call was reached by -- ``sys._getframe``, ``getattr(sys, "_get"+"frame")``,
``eval("sys._getframe")``, an alias captured at import, a helper module the gate never parsed. A hook
that RAISES makes the audited operation fail, so the frame object is never produced::

    while a gated plugin's code is on the stack:
        sys._getframe / sys._current_frames        -> refused (no frame object is created)
        gc.get_objects / get_referrers / referents -> refused (the reflective route to the same dict)
        sys.settrace / sys.setprofile             -> refused (a hook that sees every engine frame)
        ctypes.*                                  -> refused (reads interpreter memory directly)
        import of scms_sim_ref.<engine internals>  -> refused (the route that needs no reflection)

``f_back`` is NOT audited by CPython, so ANY frame handed to plugin code is a stepping stone to every
other frame in the thread. That is why the refusal is unconditional while the guard is armed rather
than filtered by which frame was asked for: a policy that lets one frame through lets all of them
through. It is also why the guard is armed TIGHTLY -- around the plugin call itself -- instead of for
the whole run: engine and stdlib code between plugin calls (``logging``, ``namedtuple``,
``dataclasses``) uses ``sys._getframe`` legitimately and must keep working.

THE CLAIM, IN ONE SENTENCE
--------------------------
    **While a guarded plugin call is on the stack, no Python-level route we have been able to
    enumerate produces a live engine FRAME** -- and the per-message ground truth (the broadcast dict
    ``b``, with ``veh.is_attacker``, the true position, ``falsified``) lives in a frame and nowhere
    else.

The last clause is what makes the sentence worth anything, and it is also its limit. The engine's
MODULES are a different matter: the ``import`` rule below is a **tripwire, not a barrier**, and three
things make it one. CPython raises the ``import`` audit event only on a cache MISS; it raises it from
the builtin ``__import__`` only, so ``importlib.import_module`` reaches ``_gcd_import`` without it;
and ``sys.modules`` is not audited at all. A plugin that reads
``sys.modules["scms_sim_ref.mock_pipeline.run"]`` gets the module, and no audit hook in CPython can
stop it. What it gets there is module globals, not the run's per-message oracle -- but the honest
statement is "the frames are closed", never "the engine is closed".

WHAT THIS IS NOT
----------------
**It is not a sandbox, and this module must never be described as one.** Three named limits:

1. **The guard's own state is an object in this interpreter.** A plugin that reaches
   ``sys.modules["scms_sim_ref.api.guard"]`` can set the depth cell to zero and walk freely. That
   reach is refused *statically* by the source gate (``sys.modules`` is on its list) and it is a
   deliberate, single-purpose act -- but it is not prevented. :class:`~scms_sim_ref.api.integrity.
   Sentinel` watches this module's functions by identity, so REPLACING them is fatal; mutating the
   cell is not visible to it.
2. **It covers the events CPython audits.** Those are the routes from plugin code to a live engine
   frame that we could enumerate; a route nobody has enumerated is not covered.
3. **An audit hook can never be removed once added** (CPython offers no API), so the hook is
   installed LAZILY -- the first time a run actually loads a gated in-process plugin -- and is inert
   (one list-index test) whenever nothing is armed. A run with no plugins never installs it and pays
   nothing, which is why the pinned no-plugin goldens are untouched by any of this.

WHAT IT COSTS A HONEST PLUGIN AUTHOR
------------------------------------
One thing, and it is worth naming rather than discovering: **`logging` above the enabled level does
not work inside a guarded call.** `Logger.findCaller` uses `sys._getframe`, and the refusal cannot
make an exception for it -- `logging.currentframe()` is a public function that RETURNS a frame three
levels up, which lands squarely in the engine, so a caller allow-list would be the laundering route
rather than a concession. The same goes for `collections.namedtuple`, `dataclass` and
`traceback.print_stack()` if a plugin builds or calls them per message. `logger.debug()` /
`logger.info()` under the default WARNING level never reach `findCaller` and are unaffected; log
from `__init__`, or set `source_gate: "off"` for a plugin you wrote yourself. ~100 ns per guarded
call is the other cost, measured, and it is paid only on the third-party path.

The only boundary that PREVENTS rather than REFUSES is out-of-process isolation
(:mod:`~scms_sim_ref.api.isolate`), where the oracle is not in the address space at all.

WHY THE REFUSAL IS A ``BaseException``
--------------------------------------
:class:`GuardError` derives from ``BaseException``, so a plugin's ``try: ... except Exception:``
cannot swallow it and carry on. And because a plugin *can* write ``except BaseException``, the
refusal is also RECORDED: :func:`guarded` compares the violation count across the call and raises
:class:`~scms_sim_ref.api.integrity.IntegrityError` -- fatal, before any manifest -- when it moved,
whether or not the plugin let the original exception out. Suppressing the exception therefore buys a
plugin nothing except a different error message.
"""
from __future__ import annotations

import sys

#: Refused events, mapped to the sentence that explains WHY. Matched exactly, except for `ctypes.`,
#: which is matched by prefix (CPython spells several of them, and a new one is not a hole).
REFUSED_EVENTS = {
    "sys._getframe":
        "sys._getframe() -- returns a live frame object. The reception loop's frame holds the "
        "broadcast dict `b` (the Vehicle with .is_attacker/.attack_type, the sender's TRUE x/y, "
        "`falsified`, `ghost`) and the engine's global RNG. `f_back` is not audited by CPython, so "
        "one frame is every frame.",
    "sys._current_frames":
        "sys._current_frames() -- the frame stack of every thread in this interpreter, including "
        "the one running the reception loop.",
    "gc.get_objects":
        "gc.get_objects() -- enumerates every live object, the broadcast dict among them.",
    "gc.get_referrers":
        "gc.get_referrers() -- reaches the dict that HOLDS an object the plugin was handed, which "
        "for an Observation is the engine's own broadcast dict.",
    "gc.get_referents":
        "gc.get_referents() -- walks out of any object the engine hands over.",
    "sys.settrace":
        "sys.settrace() -- installs a hook that is called with every engine frame.",
    "sys.setprofile":
        "sys.setprofile() -- installs a hook that is called with every engine frame.",
}

#: Prefix-matched families. `ctypes` reads and writes interpreter memory directly, which is past
#: every Python-level rule this module could state.
REFUSED_PREFIXES = ("ctypes.",)

#: The engine subpackages a plugin may import. Everything else under `scms_sim_ref` is engine
#: internals -- `mock_pipeline` holds the loop and the `Vehicle`, `datagen` the label pipeline --
#: and importing one is a route to the oracle that needs no reflection at all.
#:
#: TAKEN FROM `srcgate`, not restated: the static gate refuses the same import at load time, and two
#: hand-maintained copies of one list is how a runtime rule and a load-time rule silently disagree.
from .srcgate import ENGINE_PACKAGE, PUBLIC_SUBPACKAGES  # noqa: E402 - documented above


class GuardError(BaseException):
    """A gated plugin reached for the engine's frames, objects or internals while it was running.

    ``BaseException`` on purpose: see the module docstring. It is never caught inside the engine --
    :func:`guarded` converts it into an
    :class:`~scms_sim_ref.api.integrity.IntegrityError` (a `ConfigError`), which takes the existing
    fatal-before-any-manifest path.
    """


# --------------------------------------------------------------------------- #
# State
# --------------------------------------------------------------------------- #
#: Nesting depth of armed plugin calls. A LIST because the hook reads it on every audited event in
#: the process and `_DEPTH[0]` is the cheapest read Python has for mutable module state.
_DEPTH = [0]

#: Every refusal, in order: `(event, detail)`. Read by :func:`violations`. BOUNDED, so a plugin
#: cannot exhaust memory by looping on a refused call.
_VIOLATIONS: list = []
MAX_RECORDED = 256

#: The UNBOUNDED refusal counter, and it has to be separate from the list. The engine's wrappers
#: compare this across a call so a plugin that catches `GuardError` is still caught -- and if that
#: comparison were made on `len(_VIOLATIONS)`, a plugin that first looped `MAX_RECORDED` times to
#: fill the list would make every later refusal invisible to it. A monotone counter cannot be
#: saturated.
_COUNT = [0]

#: Nesting depth of the ENGINE-INTERNAL IMPORT rule, armed separately from the reflection rules.
#: A plugin call is armed for both. The attestation child arms only the reflection half
#: (`arm(imports=False)`), because the conformance suite it is running is entitled to import the
#: engine -- C12 runs a whole pipeline -- and refusing the harness's own imports would turn a
#: protection into a broken suite.
_IMPORTS = [0]

_INSTALLED = [False]


def _hook(event, args):
    """The audit hook. Inert unless a plugin call is in flight: one list index and a return."""
    if not _DEPTH[0]:
        return
    why = REFUSED_EVENTS.get(event)
    if why is None:
        if event == "import":
            if _IMPORTS[0]:
                _check_import(args)
            return
        for prefix in REFUSED_PREFIXES:
            if event.startswith(prefix):
                why = (f"{event} -- ctypes reads and writes interpreter memory directly, past every "
                       f"Python-level rule this guard could state.")
                break
        else:
            return
    _refuse(event, why)


def _check_import(args) -> None:
    name = args[0] if args else ""
    if not isinstance(name, str) or not name.startswith(ENGINE_PACKAGE):
        return
    parts = name.split(".")
    if parts[0] != ENGINE_PACKAGE:
        return
    sub = parts[1] if len(parts) > 1 else ""
    if sub and sub in PUBLIC_SUBPACKAGES:
        return
    _refuse("import", f"import {name} -- {ENGINE_PACKAGE}.{sub or ''} is ENGINE INTERNALS. The "
                      f"engine loop holds the ground truth this plugin is supposed to be "
                      f"predicting; import only "
                      f"{sorted(ENGINE_PACKAGE + '.' + p for p in PUBLIC_SUBPACKAGES)}.")


def _refuse(event: str, why: str):
    _COUNT[0] += 1
    if len(_VIOLATIONS) < MAX_RECORDED:
        _VIOLATIONS.append((event, why))
    raise GuardError(
        f"PLUGIN GUARD: {why}\n"
        f"  This was refused by a PEP 578 audit hook, INSIDE CPython, so the object was never "
        f"created -- spelling the call getattr(sys, '_get'+'frame'), eval()ing it, aliasing it at "
        f"import or hiding it in a helper module the source gate never parsed all reach this same "
        f"point.\n"
        f"  A detector that reads the engine's frames is scoring the labels it is supposed to be "
        f"predicting, and it stays perfectly deterministic while doing it, so no digest, golden or "
        f"content hash in this project can see it. That is why it is refused here instead.\n"
        f"  IF THIS IS YOUR OWN CODE and the construct is legitimate, say so in the config -- it is "
        f"recorded in the manifest and it replays:\n"
        f"      \"plugins\": {{\"check\": [..., {{\"ref\": \"...\", \"source_gate\": \"off\"}}]}}\n"
        f"  `source_gate: \"off\"` disables the static gate AND this runtime guard for that one "
        f"plugin, and a dataset built that way says so in its own manifest.")


# --------------------------------------------------------------------------- #
# The engine's side
# --------------------------------------------------------------------------- #
def install() -> bool:
    """Install the audit hook. Idempotent; returns True the first time.

    Lazy on purpose. `sys.addaudithook` cannot be undone, so a process that never loads a gated
    in-process plugin never acquires the hook -- which keeps the default dataset path, and every
    pinned golden, exactly as it was.
    """
    if _INSTALLED[0]:
        return False
    sys.addaudithook(_hook)
    _INSTALLED[0] = True
    return True


def installed() -> bool:
    return bool(_INSTALLED[0])


def depth() -> int:
    return _DEPTH[0]


def armed() -> bool:
    return bool(_DEPTH[0])


def violations() -> tuple:
    return tuple(_VIOLATIONS)


def violation_count() -> int:
    return _COUNT[0]


def reset() -> None:
    """Drop the recorded violations and zero the counter. **Test seam only** -- it disarms nothing,
    and no engine path calls it: the wrappers compare the counter across ONE call, so a stale
    history never makes a later refusal invisible."""
    del _VIOLATIONS[:]
    _COUNT[0] = 0


def enter(*, imports: bool = True) -> None:
    _DEPTH[0] += 1
    if imports:
        _IMPORTS[0] += 1


def leave(*, imports: bool = True) -> None:
    if _DEPTH[0] > 0:
        _DEPTH[0] -= 1
    if imports and _IMPORTS[0] > 0:
        _IMPORTS[0] -= 1


class arm:                                                 # noqa: N801 - context-manager naming
    """``with arm():`` -- the guard, for a block. Used where a wrapper is the wrong shape.

    `imports=False` arms the REFLECTION rules only. The attestation child uses it: what it is
    protecting is a secret in its own frame, and the code it runs (this project's conformance
    suite) is entitled to import this project.

    `raising=False` records refusals without turning them into an `IntegrityError` on the way out --
    for a caller that reports the violations itself rather than failing a run on them.
    """

    __slots__ = ("_n", "_imports", "_raising")

    def __init__(self, *, imports: bool = True, raising: bool = True):
        self._imports = bool(imports)
        self._raising = bool(raising)

    def __enter__(self):
        install()
        self._n = _COUNT[0]
        enter(imports=self._imports)
        return self

    def __exit__(self, exc_type, exc, tb):
        leave(imports=self._imports)
        if self._raising and _COUNT[0] != self._n:
            raise _integrity_error("", tuple(_VIOLATIONS[-(_COUNT[0] - self._n):]))
        return False


def guarded(fn, what: str = ""):
    """`fn`, with the guard armed for the duration of every call.

    Returns `fn` UNCHANGED when the caller does not want the guard (`what` is None), so the
    built-in path and the recorded `source_gate: "off"` opt-out pay nothing at all -- not even a
    wrapper frame.

    The violation COUNT is compared across the call, so a plugin that catches the `GuardError`
    (which needs `except BaseException`) and returns a plausible score still fails the run.
    """
    if what is None:
        return fn
    install()
    label = str(what)

    def call(*args, **kwargs):
        n = _COUNT[0]
        _DEPTH[0] += 1
        _IMPORTS[0] += 1
        try:
            return fn(*args, **kwargs)
        finally:
            if _DEPTH[0] > 0:
                _DEPTH[0] -= 1
            if _IMPORTS[0] > 0:
                _IMPORTS[0] -= 1
            if _COUNT[0] != n:
                raise _integrity_error(label, tuple(_VIOLATIONS[-(_COUNT[0] - n):]))

    call.__name__ = getattr(fn, "__name__", "call")
    call.__qualname__ = getattr(fn, "__qualname__", call.__name__)
    call.__doc__ = getattr(fn, "__doc__", None)
    #: So a test -- and a reader of a traceback -- can tell a guarded call plan entry from a raw one.
    call.__scms_guarded__ = label
    return call


def _integrity_error(label: str, moved):
    from .integrity import IntegrityError
    who = f" ({label})" if label else ""
    lines = [f"PLUGIN GUARD REFUSAL{who}: {len(moved)} reflective reach(es) into the engine while "
             f"the plugin was running:"]
    for event, why in moved:
        lines.append(f"  {event}")
        lines.append(f"      {why}")
    lines += [
        "",
        "The refusal is raised from a PEP 578 audit hook inside CPython, so the frame, the object",
        "list or the memory handle was never produced -- however the call was spelled. The run is",
        "refused and NO MANIFEST IS WRITTEN: a dataset whose detector reached for the labels it is",
        "predicting is worse than no dataset.",
        "",
        "This guard is not a sandbox. It refuses the reflective routes CPython audits while a gated",
        "plugin is on the stack; it does not put the oracle out of reach. For code you genuinely do",
        "not trust, run the detector out of process (`plugins.check[].isolated: true`), where the",
        "ground truth is not in the address space at all.",
    ]
    return IntegrityError("\n".join(lines))
