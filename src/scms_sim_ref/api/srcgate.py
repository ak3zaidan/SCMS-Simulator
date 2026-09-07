"""A SOURCE gate for in-process plugins. **A guard rail, not a sandbox.**

Read this paragraph before anything else, because the rest of the module is only worth having if it
is understood correctly.

An in-process Python plugin runs inside the engine's own interpreter with the engine's own
privileges. It cannot be sandboxed. ``sys._getframe``, ``inspect.currentframe``,
``gc.get_referrers``, ``gc.get_objects``, module globals and `__subclasses__` walking are all
reachable from any callable the engine invokes, and no arrangement of frozen dataclasses, wrapper
objects or namespaced mappings changes that. **Measured on this engine**, from inside a `Check`'s
``evaluate()``::

    b = sys._getframe(1).f_locals["b"]        # the reception loop's broadcast dict
    b["veh"].is_attacker                      # -> True     (the label being predicted)
    b["veh"].attack_type                      # -> 'ConstPosOffset'
    b["x"], b["y"]                            # -> the sender's TRUE position
    b["falsified"], b["ghost"]                # -> the per-message oracle flags
    sys._getframe(1).f_locals["rng"]          # -> the engine's single global random.Random

A detector doing that scored the ground truth directly and fired on 1878 of 2297 reports, and every
other layer of this project -- the pinned goldens, the content-hash lock, the two-run equality gate,
`Observation`'s field list -- was blind to it, because reading the oracle is perfectly
deterministic and perfectly reproducible.

**So what is this module for?** Three things, all of them real and none of them "safety":

1. It turns the accident into an error. A detector ported from F2MD's `LegacyChecks.h`, which is
   *handed* ``realDynamicMap`` (the ground-truth position of every node), reaches for a frame walk
   as the natural way to get the same thing here. That author finds out at load time, by name, with
   a line number.
2. It makes the reach DELIBERATE. Defeating this gate takes ``getattr(sys, "_get" + "frame")`` or an
   ``eval``, which no honest detector contains and which is a sentence a reviewer, a competition
   organiser or a co-author can read and understand. That is the difference between "we cannot tell"
   and "they chose to hide it".
3. It gives a benchmark operator something to point at. "Submissions are screened for frame walking
   and rejected" is a statable policy. "Submissions cannot read the labels" would be a lie.

**It is defeatable, by design, and the error message says so.** Static analysis of a language with
`getattr`, `eval`, `exec`, `__import__` and attribute strings is a bar, not a wall. Anything that
must actually be prevented needs the out-of-process detector mode (a separate process, an explicit
message boundary, the oracle simply not present in the address space) -- phase 4, and the only
honest answer for genuinely untrusted code.

**The SPELLING half of that gap is closed at run time INSIDE A GUARDED CALL, and nowhere else.**
:mod:`~scms_sim_ref.api.guard` refuses `sys._getframe`, `sys._current_frames`, the `gc` object
walkers, trace/profile installation and `ctypes` from inside CPython -- a PEP 578 audit hook fires
in the C function itself, so `getattr(sys, "_get" + "frame")` reaches the same refusal as
`sys._getframe`, and so does a helper module this gate never parsed. **But the guard is armed around
the calls the engine makes INTO a plugin, not around the plugin's module-level code or its
`__init__`** -- and `frame.f_back` / `frame.f_locals` are audited by nothing, so a frame captured at
import or construction time is a live handle on the reception loop's locals for the rest of the run.
Measured, at this gate's default `"on"`: a module body that captured `sys._getframe(1)` read
`b["veh"].is_attacker` and the sender's true position from inside `evaluate`, and the run completed.

So: neither module is a sandbox and neither closes the reach. This one turns the accident into a
NAMED error at load time with a line number; the guard turns the deliberate act into a refusal for
the window it covers, and `guard.py`'s own docstring lists the four things it cannot do. The two
share one switch: `source_gate: "off"` turns off this gate AND the runtime guard for that plugin,
which is what makes "code I wrote or audited" a single, recorded decision instead of two.
**In-process plugins are TRUSTED CODE.** For a detector you have not reviewed, the only boundary
that PREVENTS rather than refuses is `plugins.check[].isolated: true`
(:mod:`~scms_sim_ref.api.isolate`).

Scope, stated so nobody over-reads a PASS: **the gate parses the single module the plugin class is
DEFINED IN.** A helper module that class imports is not parsed, and neither is anything reached at
run time. Widening it to the whole distribution is possible (`registry.package_root` already finds
the tree) and is listed as future work rather than claimed here.
"""
from __future__ import annotations

import ast
import os
import sys

from .errors import ConfigError

#: `plugins.<slot>[].source_gate` values. "on" is the default and the only one that inspects
#: anything; "off" is the explicit, RECORDED opt-out for code the user themselves wrote or audited.
GATE_MODES = ("on", "off")
DEFAULT_MODE = "on"


class SourceGateError(ConfigError):
    """A plugin's source contains a construct that reaches around the detector interface.

    A `ConfigError` subclass on purpose: the engine already refuses a bad `ConfigError` BEFORE step
    0 and before any output directory exists, which is exactly the handling this needs.
    """


# --------------------------------------------------------------------------- #
# What is refused
# --------------------------------------------------------------------------- #
#: Attribute (or imported) names that mean one thing only. Flagged wherever they appear, on any
#: object, because there is no benign `obs.f_locals`.
_ANY_NAMES = {
    "_getframe": "sys._getframe() -- walks to the engine's own stack frame, whose locals carry the "
                 "broadcast dict (veh/.is_attacker, true x/y, falsified, ghost) and the global rng",
    "_current_frames": "sys._current_frames() -- every thread's frame stack",
    "currentframe": "inspect.currentframe() -- the same frame walk, through `inspect`",
    "getouterframes": "inspect.getouterframes() -- enumerates the calling frames",
    "getinnerframes": "inspect.getinnerframes() -- enumerates frames from a traceback",
    "f_locals": "frame.f_locals -- reads another function's local variables",
    "f_globals": "frame.f_globals -- reads another module's globals",
    "f_back": "frame.f_back -- climbs the call stack",
    "tb_frame": "traceback.tb_frame -- recovers a frame from an exception",
    "gi_frame": "generator.gi_frame -- recovers a frame from a generator",
    "cr_frame": "coroutine.cr_frame -- recovers a frame from a coroutine",
    "get_referrers": "gc.get_referrers() -- reaches every object holding a reference to a given one",
    "get_referents": "gc.get_referents() -- walks out of any object the engine hands over",
    "get_objects": "gc.get_objects() -- enumerates every live object in the interpreter",
    "__import__": "__import__ -- imports a module named at run time, past the import rules below",
}

#: Qualified ``<module>.<attr>`` forms whose bare attribute name is too common to flag on its own.
_QUALIFIED = {
    ("inspect", "stack"): "inspect.stack() -- the full calling-frame stack",
    ("inspect", "trace"): "inspect.trace() -- the frame stack of the exception being handled",
    ("sys", "modules"): "sys.modules -- reaches any already-imported engine module by string, with "
                        "no import statement for the import rules below to see",
    ("sys", "settrace"): "sys.settrace() -- installs a hook that sees every engine frame",
    ("sys", "setprofile"): "sys.setprofile() -- installs a profiling hook over the engine",
    ("threading", "settrace"): "threading.settrace() -- the same hook, per thread",
}

#: Modules a detector plugin has no business importing. `scms_sim_ref.*` is handled separately.
_BANNED_MODULES = {
    "ctypes": "ctypes -- reads and writes interpreter memory directly, past every Python-level rule",
}

#: Callables that make static analysis of the rest of the file meaningless. A detector that needs
#: them is doing something a reviewer must see.
_DYNAMIC = {
    "eval": "eval() -- executes source this gate cannot see",
    "exec": "exec() -- executes source this gate cannot see",
    "compile": "compile() -- builds code objects this gate cannot see",
    "__import__": "__import__() -- imports a module named at run time, past the import rules above",
}

#: The ONLY `scms_sim_ref` subpackage a detector plugin is meant to import. Everything else --
#: `mock_pipeline` (the engine loop, `Vehicle`, the attack generator), `datagen` (the label
#: pipeline), `schemas` (the oracle record shapes) -- is engine internals, and importing it is a
#: route to the oracle that needs no frame walk at all.
ENGINE_PACKAGE = "scms_sim_ref"
PUBLIC_SUBPACKAGES = frozenset({"api", "conformance"})


class Finding:
    """One refused construct: what it is, where it is, and why it is refused."""

    __slots__ = ("construct", "line", "col", "why")

    def __init__(self, construct, line, col, why):
        self.construct, self.line, self.col, self.why = construct, int(line), int(col), why

    def __repr__(self):                                    # pragma: no cover - debugging aid
        return f"Finding({self.construct!r}, line={self.line})"

    def to_dict(self) -> dict:
        return {"construct": self.construct, "line": self.line, "why": self.why}


class _Scan(ast.NodeVisitor):
    """One pass over the module AST. Records findings; never raises."""

    def __init__(self):
        self.found: list = []

    def _add(self, construct, node, why):
        self.found.append(Finding(construct, getattr(node, "lineno", 0),
                                  getattr(node, "col_offset", 0), why))

    # -- attribute access: sys._getframe, frame.f_locals, gc.get_referrers, ... --------------- #
    def visit_Attribute(self, node):
        why = _ANY_NAMES.get(node.attr)
        if why is not None:
            self._add(f"{_dotted(node.value)}.{node.attr}" if _dotted(node.value) else node.attr,
                      node, why)
        else:
            base = _dotted(node.value)
            if base:
                why = _QUALIFIED.get((base.rsplit(".", 1)[-1], node.attr))
                if why is not None:
                    self._add(f"{base}.{node.attr}", node, why)
        self.generic_visit(node)

    # -- bare calls: eval / exec / compile ---------------------------------------------------- #
    def visit_Call(self, node):
        fn = node.func
        if isinstance(fn, ast.Name) and fn.id in _DYNAMIC:
            self._add(f"{fn.id}()", node, _DYNAMIC[fn.id])
        self.generic_visit(node)

    # -- imports ------------------------------------------------------------------------------ #
    def visit_Import(self, node):
        for alias in node.names:
            self._import(node, alias.name)
        self.generic_visit(node)

    def visit_ImportFrom(self, node):
        if node.level:                                     # a relative import stays inside the
            self.generic_visit(node)                       # plugin's own distribution
            return
        mod = node.module or ""
        if mod == ENGINE_PACKAGE:
            # `from scms_sim_ref import api` is the documented spelling and must pass; it is the
            # NAMES that decide, not the bare package.
            for alias in node.names:
                if alias.name not in PUBLIC_SUBPACKAGES:
                    self._add(f"from {ENGINE_PACKAGE} import {alias.name}", node,
                              _engine_import_why(alias.name))
        else:
            self._import(node, mod)
        for alias in node.names:
            why = _ANY_NAMES.get(alias.name)
            if why is not None:                            # `from sys import _getframe`
                self._add(f"from {mod} import {alias.name}", node, why)
        self.generic_visit(node)

    def _import(self, node, dotted: str):
        if not dotted:
            return
        head = dotted.split(".", 1)[0]
        why = _BANNED_MODULES.get(head)
        if why is not None:
            self._add(f"import {dotted}", node, why)
            return
        if head != ENGINE_PACKAGE:
            return
        parts = dotted.split(".")
        sub = parts[1] if len(parts) > 1 else ""
        if sub and sub in PUBLIC_SUBPACKAGES:
            return
        self._add(f"import {dotted}", node, _engine_import_why(sub or ENGINE_PACKAGE))


def _engine_import_why(sub: str) -> str:
    return (f"{ENGINE_PACKAGE}.{sub} is ENGINE INTERNALS, not the plugin interface. The engine loop "
            f"holds the ground truth this detector is supposed to be predicting; import only "
            f"{sorted(ENGINE_PACKAGE + '.' + p for p in PUBLIC_SUBPACKAGES)}")


def _dotted(node) -> str:
    """`a.b.c` for a Name/Attribute chain, else ''."""
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        base = _dotted(node.value)
        return f"{base}.{node.attr}" if base else ""
    return ""


# --------------------------------------------------------------------------- #
# Source retrieval
# --------------------------------------------------------------------------- #
def module_source(obj):
    """(source, origin) for the module `obj` is defined in, or (None, reason).

    Goes through the module's own loader first so a zipimported or otherwise non-file plugin is
    still readable, and falls back to the file on disk. `decode_source` applies the PEP 263 coding
    cookie, so a latin-1 plugin is not silently mis-parsed.
    """
    name = getattr(obj, "__module__", None)
    if not name:
        return None, f"{obj!r} declares no __module__"
    mod = sys.modules.get(name)
    if mod is None:
        return None, f"module {name!r} is not in sys.modules"
    loader = getattr(getattr(mod, "__spec__", None), "loader", None)
    origin = getattr(mod, "__file__", None) or getattr(getattr(mod, "__spec__", None),
                                                       "origin", None) or name
    if loader is not None and hasattr(loader, "get_source"):
        try:
            src = loader.get_source(name)
        except Exception:                                  # pragma: no cover - exotic loaders
            src = None
        if src is not None:
            return src, origin
    path = getattr(mod, "__file__", None)
    if path and os.path.isfile(path) and path.endswith(".py"):
        try:
            from importlib.util import decode_source
            with open(path, "rb") as fh:
                return decode_source(fh.read()), path
        except Exception as e:                             # pragma: no cover - unreadable file
            return None, f"cannot read {path}: {e}"
    return None, (f"module {name!r} has no readable Python source"
                  f"{' at ' + str(path) if path else ''}")


def scan_source(src: str, origin: str = "<plugin>") -> list:
    """Findings for one module's source, in line order. Never raises on a syntax error --
    an unparseable module is reported as one finding, because a module the gate cannot read is
    exactly the case a gate must not wave through."""
    try:
        tree = ast.parse(src, filename=str(origin))
    except SyntaxError as e:
        return [Finding("<unparseable>", e.lineno or 0, e.offset or 0,
                        f"the gate could not parse this module ({e.msg}); it cannot certify source "
                        f"it cannot read")]
    scan = _Scan()
    scan.visit(tree)
    scan.found.sort(key=lambda f: (f.line, f.col, f.construct))
    return scan.found


# --------------------------------------------------------------------------- #
# The gate
# --------------------------------------------------------------------------- #
def check_mode(mode, where: str) -> str:
    m = str(mode or DEFAULT_MODE)
    if m not in GATE_MODES:
        raise ConfigError(f"{where} must be one of {list(GATE_MODES)} (got {mode!r}); "
                          f"'on' scans the plugin's source for engine-internal reach, 'off' is the "
                          f"explicit opt-out for code you wrote or audited yourself")
    return m


def gate(slot: str, ref: str, obj, *, mode: str = DEFAULT_MODE) -> dict:
    """Run the gate for one resolved plugin. Returns a record; raises :class:`SourceGateError`.

    The record is small and JSON-shaped on purpose: it is what a caller records next to the
    plugin's content hash, so "this run screened this plugin, and here is what the screen saw" is a
    property of the artifact rather than a claim in a changelog.
    """
    mode = check_mode(mode, f"plugins.{slot}[].source_gate")
    if mode == "off":
        # RECORDED, not silent. `source_gate: "off"` lives in `cfg.plugins`, which is serialised
        # verbatim into `manifest["config"]`, so a dataset produced with the gate disabled says so
        # in its own manifest and a replay reproduces the same decision.
        return {"mode": "off", "scanned": False, "findings": []}
    src, origin = module_source(obj)
    if src is None:
        raise SourceGateError(_message(slot, ref, origin, [
            Finding("<no source>", 0, 0,
                    f"{origin}. The gate refuses what it cannot read rather than passing it")]))
    findings = scan_source(src, origin)
    if findings:
        raise SourceGateError(_message(slot, ref, origin, findings))
    return {"mode": "on", "scanned": True, "origin": str(origin), "findings": []}


def _message(slot: str, ref: str, origin, findings) -> str:
    lines = [f"plugins.{slot} {ref!r}: REFUSED by the source gate.",
             f"  source: {origin}"]
    for f in findings:
        lines.append(f"  line {f.line}: {f.construct}")
        lines.append(f"      {f.why}")
    lines += [
        "",
        "WHY THIS IS REFUSED. An in-process detector runs inside the engine's interpreter, so the",
        "constructs above reach the engine's own stack frame -- whose locals hold the broadcast dict",
        "(the Vehicle with .is_attacker/.attack_type, the sender's TRUE x/y, `falsified`, `ghost`)",
        "and the global RNG. A detector that reads those is scoring the labels it is supposed to be",
        "predicting, and it stays perfectly deterministic while doing it, so no digest, golden or",
        "content hash in this project can see it.",
        "",
        "WHAT THIS GATE IS. A guard rail, not a sandbox. It parses the ONE module the plugin class is",
        "defined in and matches names; it is defeated by getattr(sys, '_get' + 'frame'), by an eval,",
        "by a helper module it does not parse, or by anything resolved at run time. It exists to make",
        "the reach an explicit act rather than an accident, and to make it reviewable. If you need a",
        "detector you genuinely do not trust to be UNABLE to read the labels, run it out of process;",
        "in-process plugins are TRUSTED code, on the same footing as any installed dependency.",
        "",
        "IF THIS IS YOUR OWN CODE and the construct is legitimate, say so in the config -- it is",
        "recorded in the manifest and it replays:",
        f'    "plugins": {{"{slot}": [..., {{"ref": "{ref}", "source_gate": "off"}}]}}',
        "See docs/realism/DETECTOR-PLUGIN.md section 2 (the trust model).",
    ]
    return "\n".join(lines)
