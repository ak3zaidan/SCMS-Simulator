"""The resolver (PLUGIN-ARCHITECTURE.md section 3) and the content-hash provenance lock (D4).

**Chosen: an explicit, ordered, config-declared reference resolved through three tiers, with entry
points used only as a CATALOGUE.**

| Mechanism | Reproducibility | Verdict |
|---|---|---|
| `importlib.metadata` entry points | **worst -- disqualifying for ACTIVATION** | catalogue only |
| decorator registry | good if snapshotted `tuple(sorted(...))` | built-ins only |
| `ABC` / `Protocol` | n/a (neither checks signatures) | the SHAPE, not the SELECTION |
| **dotted path in config `"pkg.mod:Class"`** | **best by construction** | **activation** |

Entry points are disqualified as an activation mechanism twice over: the active set becomes a
function of machine state (what is on `sys.path`), and iteration order follows distribution
discovery order, not sort order -- pluggy documents this failure mode explicitly, which is why it
ships `tryfirst`/`trylast`. In this repo that is fatal, because `DET_KEYS` (run.py:2579) is an
ORDERED tuple whose order fixes `detnorm_*` JSON key insertion order and therefore `data_digest`,
and the tie-break `sorted(fired, key=lambda k: -det[k])` relies on Python's stable sort over it. The
Java side already hit and fixed this exact bug class (`CamDetector.java:235-236`, a `LinkedHashMap`
with an explicit comment). Entry points stay useful for `--list-plugins` and the GUI dropdown, and
are always materialised as `sorted(eps, key=lambda e: e.name)`.

Dotted-path-in-config is best BY CONSTRUCTION: the string lives in `cfg`, so it flows automatically
into `manifest["config"]` (run.py:3793) and back through `config_from_dict` (run.py:1900) with NO
new plumbing. Its one weakness -- a dotted path is a MUTABLE NAME, and the same string resolves to
different bytes on different machines -- is closed by the content hash here. The pattern is DVC's
intent/lock split: `cfg.plugins` is `dvc.yaml`, `manifest["plugins"]` is `dvc.lock`. Nextflow's rule
generalises: never key provenance on a mutable name, always on content.
"""
from __future__ import annotations

import hashlib
import importlib
import inspect
import json
import os
import sys

from . import channel as _channel
from . import codec as _codec
from . import detect as _detect
from . import profile as _profile
from . import report as _report
from .errors import (ApiError, CapabilityError, ConfigError, InterfaceVersionError,
                     PluginDriftError, SignatureError)
from .rng import check_plugin_id

API_VERSION = "1.0"

#: slot -> {name: class}. Registration order is the DISPLAY order (it is what `_ENUM_OPTIONS` and
#: argparse `choices` publish); every ITERATION for determinism-bearing purposes goes through
#: :func:`builtin_names_sorted`.
_BUILTINS: dict = {"channel_model": {}, "check": {}, "fusion": {}, "message_codec": {},
                   "mobility": {}, "protocol_profile": {}, "report_format": {}}

SLOTS: tuple = tuple(sorted(_BUILTINS))

#: slot -> (interface name, interface version, max minor, {shape: {method: (required positional
#: parameter names,)}}). A slot with more than one accepted SHAPE (the channel's per-link vs
#: per-step forms) lists them all; the resolver reports which one an object satisfied.
INTERFACE: dict = {
    "channel_model": (
        _channel.INTERFACE_NAME, _channel.INTERFACE_VERSION, _channel.MAX_MINOR,
        {"link": {"capabilities": (),
                  "begin_step": ("frame",),
                  "evaluate": ("tx", "rx", "d_m", "txn")},
         "batch": {"capabilities": (),
                   "begin_step": ("frame",),
                   "deliver": ("frame", "candidates")}},
    ),
    "check": (
        _detect.INTERFACE_NAME, _detect.INTERFACE_VERSION, _detect.MAX_MINOR,
        {"check": _detect.CHECK_SPEC},
    ),
    "fusion": (
        _detect.INTERFACE_NAME, _detect.INTERFACE_VERSION, _detect.MAX_MINOR,
        {"fusion": _detect.FUSION_SPEC},
    ),
    "message_codec": (
        _codec.INTERFACE_NAME, _codec.INTERFACE_VERSION, _codec.MAX_MINOR,
        {"codec": _codec.CODEC_SPEC},
    ),
    # The seam that makes "which protocol" ONE declaration rather than five booleans. The built-in
    # ITS-G5 profile resolves through this entry exactly as a third-party stack does -- same
    # signature check, same capability screening, same lock entry -- which is the whole test of
    # whether the seam is real (api/profile.py).
    "protocol_profile": (
        _profile.INTERFACE_NAME, _profile.INTERFACE_VERSION, _profile.MAX_MINOR,
        {"profile": _profile.PROFILE_SPEC},
    ),
    # The last EMPTY slot. A misbehaviour report's format is part of the protocol a deployment
    # speaks, and a stack that is swappable on the air and hard-coded on the backhaul is only half
    # swappable (api/report.py).
    "report_format": (
        _report.INTERFACE_NAME, _report.INTERFACE_VERSION, _report.MAX_MINOR,
        {"report": _report.REPORT_SPEC},
    ),
}

#: slot -> the module whose IMPORT registers that slot's built-ins, imported ON DEMAND the first
#: time the slot is looked at.
#:
#: `channel_model`, `check`, `fusion` and `mobility` are registered as a side effect of importing
#: `run.py` / `detectors.py`, which every engine entry point does anyway. `message_codec` has no
#: such natural importer: nothing on the default dataset path constructs a codec, and adding an
#: unconditional import of the codecs package to `run.py` would make an OPTIONAL feature a
#: mandatory import. Resolving the slot is therefore what pulls the package in -- and the package
#: itself imports no third-party module, so this stays free. `ImportError` is swallowed: a
#: stripped-down install without the `codecs` package must still be able to resolve a third-party
#: codec by dotted path.
#: `protocol_profile` and `report_format` are registered by the same package for the same reason:
#: nothing on the default dataset path constructs one, so resolving the slot is what pulls the
#: package in and an engine that never asks pays nothing.
_LAZY_REGISTRARS = {"message_codec": "scms_sim_ref.codecs",
                    "protocol_profile": "scms_sim_ref.codecs",
                    "report_format": "scms_sim_ref.codecs"}
_LAZY_DONE: set = set()


def _ensure_builtins(slot: str) -> None:
    mod = _LAZY_REGISTRARS.get(slot)
    if mod is None or slot in _LAZY_DONE:
        return
    _LAZY_DONE.add(slot)
    try:
        importlib.import_module(mod)
    except ImportError:                                    # pragma: no cover - stripped install
        pass

#: slot -> (known capabilities, capabilities refused from anything that is not a built-in). Both
#: sets are the SLOT's, not the channel's: a detector declaring `rssi` means "reads obs.rssi_dbm",
#: which is a different statement from a channel declaring it.
CAPABILITIES: dict = {
    "channel_model": (_channel.KNOWN_CAPABILITIES, _channel.RESERVED_CAPABILITIES),
    "check": (_detect.KNOWN_CAPABILITIES, _detect.RESERVED_CAPABILITIES),
    "fusion": (_detect.KNOWN_CAPABILITIES, _detect.RESERVED_CAPABILITIES),
    "message_codec": (_codec.KNOWN_CAPABILITIES, _codec.RESERVED_CAPABILITIES),
    "protocol_profile": (_profile.KNOWN_CAPABILITIES, _profile.RESERVED_CAPABILITIES),
    "report_format": (_report.KNOWN_CAPABILITIES, _report.RESERVED_CAPABILITIES),
}


# --------------------------------------------------------------------------- #
# Built-in registration
# --------------------------------------------------------------------------- #
def register_builtin(slot: str, name: str, obj) -> None:
    """Register an in-tree implementation under a stable key. Idempotent per (slot, name)."""
    if slot not in _BUILTINS:
        raise ConfigError(f"unknown plugin slot {slot!r}; known: {SLOTS}")
    _BUILTINS[slot][name] = obj


def builtin_names(slot: str) -> tuple:
    """Registration (display) order -- what the CLI and the GUI dropdown publish."""
    _ensure_builtins(slot)
    return tuple(_BUILTINS[slot])


def builtin_names_sorted(slot: str) -> tuple:
    """Sorted snapshot -- the only order used anywhere a digest can see it."""
    _ensure_builtins(slot)
    return tuple(sorted(_BUILTINS[slot]))


def builtin(slot: str, name: str):
    _ensure_builtins(slot)
    return _BUILTINS[slot].get(name)


def is_builtin(slot: str, obj) -> bool:
    return any(o is obj for o in _BUILTINS[slot].values())


# --------------------------------------------------------------------------- #
# Provenance
# --------------------------------------------------------------------------- #
class ProvenanceRecord:
    """One entry of `manifest["plugins"]["loaded"]` -- the lock, not the intent."""

    __slots__ = ("slot", "order", "ref", "resolved_via", "distribution", "version",
                 "dist_sha256", "module_sha256", "package_sha256", "interface_version",
                 "capabilities", "declared_streams", "params", "params_sha256",
                 "provenance_incomplete", "conformance", "isolated", "import_closure_sha256",
                 "import_closure_modules")

    def __init__(self, slot, order, ref, resolved_via, distribution, version, dist_sha256,
                 module_sha256, interface_version, capabilities, declared_streams, params,
                 params_sha256, provenance_incomplete, conformance=None, package_sha256=None,
                 isolated=False, import_closure=None):
        self.slot, self.order, self.ref = slot, order, ref
        self.resolved_via, self.distribution, self.version = resolved_via, distribution, version
        self.dist_sha256, self.module_sha256 = dist_sha256, module_sha256
        #: sha256 over the plugin's whole top-level PACKAGE directory. `module_sha256` covers only
        #: the file the class is DEFINED in, so a behaviour-changing edit to any SIBLING module the
        #: class imports its physics from is invisible to it -- and `dist_sha256` comes from the
        #: wheel RECORD, which goes stale the moment a file is edited in place. This field is the
        #: one that closes that gap. `None` for a built-in and for a plugin that is a bare module
        #: rather than a package (there `module_sha256` already covers everything).
        self.package_sha256 = package_sha256
        #: sha256 over the plugin's whole static IMPORT CLOSURE outside the stdlib and this engine.
        #: `package_sha256` stops at the plugin's own top-level package, so logic imported from
        #: ANOTHER distribution is unhashed by every other field here -- measured: editing such a
        #: sibling moved the dataset digest while `verify-plugins` reported no drift and exited 0.
        #: `None` for a built-in and for a module this process cannot locate without importing it.
        self.import_closure_sha256 = (import_closure or {}).get("sha256")
        #: The module NAMES the closure covered, so a drift is diffable rather than a bare hash
        #: mismatch, plus whether the walk hit its deterministic ceiling.
        self.import_closure_modules = sorted((import_closure or {}).get("modules") or {})
        self.interface_version, self.capabilities = interface_version, capabilities
        self.declared_streams, self.params = declared_streams, params
        self.params_sha256 = params_sha256
        self.provenance_incomplete = provenance_incomplete
        #: The `conformance_report.json` summary, when the config declared
        #: `plugins.<slot>.conformance = "required"` and the suite passed. Absent otherwise --
        #: an absent field means "not attested", never "attested and failed", because a failing
        #: attestation raises before the run and no manifest is written at all.
        self.conformance = conformance
        #: True when this plugin ran OUT OF PROCESS (`api/isolate.py`). Recorded in the lock and not
        #: only in `manifest["config"]`, because it changes how the entry must be VERIFIED: an
        #: isolated entry must not be re-resolved by importing the plugin into the verifying process
        #: (`verify_lock` re-probes it in a child instead). Emitted only when true, so every manifest
        #: written before this field is byte-identical to what it was.
        self.isolated = bool(isolated)

    def to_dict(self) -> dict:
        d = {"slot": self.slot, "order": self.order, "ref": self.ref,
             "resolved_via": self.resolved_via, "distribution": self.distribution,
             "version": self.version, "dist_sha256": self.dist_sha256,
             "module_sha256": self.module_sha256, "interface_version": self.interface_version,
             "capabilities": sorted(self.capabilities), "declared_streams": list(self.declared_streams),
             "params": self.params, "params_sha256": self.params_sha256}
        # Emitted ONLY when it exists, so a built-in's lock entry -- and every manifest written
        # before this field -- is byte-identical to what it was.
        if self.package_sha256 is not None:
            d["package_sha256"] = self.package_sha256
        # Emitted only when established, so a built-in's entry -- and every manifest written before
        # this field -- is byte-identical to what it was.
        if self.import_closure_sha256 is not None:
            d["import_closure_sha256"] = self.import_closure_sha256
            d["import_closure_modules"] = list(self.import_closure_modules)
        if self.provenance_incomplete:
            d["provenance_incomplete"] = True
        if self.conformance:
            d["conformance"] = self.conformance
        if self.isolated:
            d["isolated"] = True
        return d


def canonical_bytes(obj) -> bytes:
    """Sorted-key, separator-stable JSON -- the same canonicalisation `crypto_abstract` uses."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"),
                      default=str).encode("utf-8")


def params_sha256(params) -> str:
    return hashlib.sha256(canonical_bytes(params or {})).hexdigest()


def _sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


#: (path, mtime_ns, size) -> sha256. Provenance is recomputed for every run, and the in-process
#: multi-run drivers do thousands of runs; hashing the same unchanged source file each time is pure
#: waste. Keyed on the stat tuple, not the path, so a plugin file EDITED IN THIS PROCESS is still
#: seen to drift -- which is exactly what the drift detector must be able to observe.
_MODULE_HASH_CACHE: dict = {}


def _file_sha256_cached(path: str):
    """`_sha256_file` behind the stat-tuple cache, or None if the path is not a readable file."""
    try:
        st = os.stat(path)
    except OSError:
        return None
    if not os.path.isfile(path):
        return None
    key = (path, st.st_mtime_ns, st.st_size)
    hit = _MODULE_HASH_CACHE.get(key)
    if hit is not None:
        return hit
    try:
        digest = _sha256_file(path)
    except OSError:
        return None
    _MODULE_HASH_CACHE[key] = digest
    return digest


def module_sha256(obj):
    """sha256 of the file the object is DEFINED in -- that file and nothing else.

    Always-available fallback for `dist_sha256`. Fails (returns None) for C extensions and
    `exec`-created classes.

    **It is deliberately narrow, and on its own it is not enough.** A class defined in `leaky.py`
    that inherits every line of its physics from a sibling `rayleigh.py` has a `module_sha256` that
    does not move when `rayleigh.py` is rewritten. That is what :func:`package_sha256` is for, and
    why :func:`make_provenance` records both.
    """
    try:
        src = inspect.getsourcefile(obj) or inspect.getfile(obj)
    except (TypeError, OSError):
        return None
    if not src:
        return None
    return _file_sha256_cached(src)


def package_sha256(root: str):
    """sha256 over a package directory: `sorted((relpath, filehash))`, `__pycache__` excluded.

    Structurally the same helper as `_data_digest` (run.py), deliberately.

    This is the lock's answer to the SIBLING-MODULE hole. `module_sha256` hashes one file and
    `dist_sha256` is copied out of the wheel's RECORD -- a manifest of what the installer WROTE,
    which says nothing about what the file contains now. Edit a sibling module of an installed
    distribution in place and both stay put, `verify-plugins` reports "no drift", and the replay
    produces a different dataset at exit 0: precisely D4's stated failure mode, with the lock in
    place. Hashing the package directory is what makes that edit visible.

    Per-file hashes go through the stat-tuple cache, so the in-process multi-run drivers pay the
    walk (a `stat` per file) and not the read. The residual blind spot is the cache's, and it is
    the one `module_sha256` has always had: an edit that preserves BOTH mtime_ns and size in the
    same process is not seen. A fresh process always re-reads.
    """
    if not os.path.isdir(root):
        return None
    h = hashlib.sha256()
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d != "__pycache__")
        for name in sorted(filenames):
            if name.endswith((".pyc", ".pyo")):
                continue
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, root).replace(os.sep, "/")
            digest = _file_sha256_cached(full)
            if digest is None:
                return None
            h.update(rel.encode("utf-8"))
            h.update(digest.encode())
    return h.hexdigest()


def package_root_of_path(path):
    """The top-level package directory a module FILE belongs to, or None for a bare module.

    Walks up while each parent still has an `__init__.py`, so it is the same tree
    :func:`package_root` returns -- derived from the FILESYSTEM instead of from an imported
    module object, which is what lets the parent of an isolated worker compute it without
    importing anything.
    """
    if not path:
        return None
    d = os.path.dirname(os.path.abspath(path))
    if not os.path.isfile(os.path.join(d, "__init__.py")):
        return None
    while True:
        parent = os.path.dirname(d)
        if parent == d or not os.path.isfile(os.path.join(parent, "__init__.py")):
            return d
        d = parent


def package_root(obj):
    """Directory of the top-level PACKAGE the object's module lives in, or None.

    None for a bare single-module plugin (where `module_sha256` already covers the whole thing) and
    for a namespace package spread over several directories (where there is no single tree to hash
    and saying so beats hashing an arbitrary one of them).
    """
    mod = getattr(obj, "__module__", None)
    if not mod:
        return None
    top = sys.modules.get(mod.split(".")[0])
    if top is None:
        return None
    paths = [p for p in (getattr(top, "__path__", None) or ())]
    if len(paths) != 1:
        return None
    return paths[0] if os.path.isdir(paths[0]) else None


# --------------------------------------------------------------------------- #
# The IMPORT CLOSURE -- the hash that covers a sibling in ANOTHER distribution
# --------------------------------------------------------------------------- #
#: Deterministic ceilings. A plugin that imports `numpy` reaches hundreds of modules and tens of
#: megabytes; hashing all of it would put seconds on every run for no extra assurance, and hashing
#: an ARBITRARY prefix of it would make the digest depend on dict order. The walk is breadth-first
#: over SORTED module names, so a truncation is a deterministic function of the import graph, and
#: it is RECORDED (`import_closure_truncated`) rather than hidden.
CLOSURE_MAX_MODULES = 400
CLOSURE_MAX_BYTES = 64 << 20

#: Modules that are never part of a plugin's identity: the standard library (it is pinned by
#: `runtime_block()["python"]`) and this engine (pinned by `dataset_version` and the goldens; keying
#: on it would make every manifest unreplayable after any edit to any engine file).
_CLOSURE_SKIP_ROOTS = frozenset({"scms_sim_ref"})


def static_locate(module_name: str):
    """The FILE a dotted module name resolves to on this `sys.path`, **without importing anything**.

    `importlib.util.find_spec` cannot be used for this: finding `a.b` imports `a`, and running a
    package's `__init__.py` is exactly the untrusted code the isolated mode exists to keep out of
    this process. This walks `sys.path` the way `FileFinder` does -- package directory with an
    `__init__.py` first, then a same-named module file, then an extension module -- and executes
    nothing.

    Returns the absolute path, or None when this process cannot resolve the name that way (a
    zipimport, a namespace package spread over several directories, an editable install behind a
    custom finder). None means "say so", never "assume it is fine".
    """
    parts = [p for p in str(module_name).split(".") if p]
    if not parts:
        return None
    search = list(sys.path)
    found = None
    for i, part in enumerate(parts):
        found = None
        for entry in search:
            if not isinstance(entry, str):
                continue
            base = entry or os.getcwd()
            init = os.path.join(base, part, "__init__.py")
            if os.path.isfile(init):
                found = (init, [os.path.join(base, part)])
                break
            for ext in (".py", ".pyd", ".so"):
                cand = os.path.join(base, part + ext)
                if os.path.isfile(cand):
                    found = (cand, None)
                    break
            if found is not None:
                break
        if found is None:
            return None
        search = found[1]
        if search is None and i < len(parts) - 1:
            return None                                # a module cannot contain a submodule
    return os.path.abspath(found[0]) if found else None


def _closure_targets(path: str, package: str):
    """Absolute module names this file imports, resolved through the AST. Never executes it."""
    import ast
    try:
        with open(path, "rb") as fh:
            src = fh.read()
        tree = ast.parse(src, filename=path)
    except (OSError, SyntaxError, ValueError):
        return ()
    out = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                out.add(alias.name)
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                # `from . import x` / `from ..y import z`: resolve against the containing package,
                # so a plugin's own tree is walked in full rather than stopping at its first
                # relative import.
                base = package.split(".")
                base = base[:len(base) - node.level + 1] if node.level <= len(base) else []
                mod = ".".join([p for p in base if p] + ([node.module] if node.module else []))
            else:
                mod = node.module or ""
            if not mod:
                continue
            out.add(mod)
            for alias in node.names:
                out.add(f"{mod}.{alias.name}")         # `from pkg import mod` names a MODULE too
    return tuple(sorted(out))


def _with_ancestors(name: str) -> tuple:
    """`("a", "a.b", "a.b.c")` for `"a.b.c"` -- every package whose `__init__.py` runs on the way."""
    parts = str(name).split(".")
    return tuple(".".join(parts[:i + 1]) for i in range(len(parts)))


def _closure_skip(name: str) -> bool:
    head = name.split(".", 1)[0]
    if head in _CLOSURE_SKIP_ROOTS:
        return True
    if head in getattr(sys, "stdlib_module_names", frozenset()):
        return True
    return head in sys.builtin_module_names


def import_closure(module_name: str) -> dict:
    """Every non-stdlib module reachable by following imports from `module_name`, with its hash.

    **THE HOLE THIS CLOSES.** `package_sha256` hashes the plugin's OWN top-level package and nothing
    else, so logic imported from a DIFFERENT distribution is unhashed. Measured on this engine: a
    channel model whose physics lives in a sibling top-level package changed behaviour -- the digest
    moved `9ea985e0` -> `ec9e476d` -- while `verify-plugins` printed "no drift" and exited 0, which
    is D4's stated failure mode with the lock in place. Following the imports is what makes that
    edit visible.

    Static, and deliberately so: it is computed from the SOURCE, so it does not depend on what this
    process happened to have imported already, and two machines with the same files agree.

    Returns ``{"modules": {name: sha256}, "sha256": <digest over it>, "unresolved": [names],
    "truncated": bool}``. A name this process cannot locate without importing it is REPORTED in
    `unresolved` rather than silently dropped.
    """
    start = str(module_name)
    seen: dict = {}
    unresolved: list = []
    frontier = list(_with_ancestors(start))
    total = 0
    truncated = False
    while frontier:
        name = frontier.pop(0)
        if name in seen or _closure_skip(name):
            continue
        path = static_locate(name)
        if path is None:
            if name != start and name.rpartition(".")[0] in seen:
                continue                               # `from pkg import Class`, not a submodule
            unresolved.append(name)
            continue
        if len(seen) >= CLOSURE_MAX_MODULES or total >= CLOSURE_MAX_BYTES:
            truncated = True
            continue
        digest = _file_sha256_cached(path)
        if digest is None:                             # pragma: no cover - vanished mid-walk
            unresolved.append(name)
            continue
        seen[name] = digest
        try:
            total += os.path.getsize(path)
        except OSError:                                # pragma: no cover
            pass
        if path.endswith(".py"):
            package = name if path.endswith("__init__.py") else name.rpartition(".")[0]
            for target in _closure_targets(path, package):
                # ANCESTORS TOO. `from prov_phys.core import reach` runs `prov_phys/__init__.py`
                # before `core.py`, so a closure that hashed only `prov_phys.core` would leave the
                # package's own module-level code -- the earliest hook there is -- unhashed.
                frontier.extend(a for a in _with_ancestors(target) if a not in seen)
        frontier.sort()
    h = hashlib.sha256()
    for name in sorted(seen):
        h.update(name.encode("utf-8"))
        h.update(seen[name].encode())
    return {"modules": seen, "sha256": h.hexdigest(), "unresolved": sorted(set(unresolved)),
            "truncated": truncated}


def import_closure_sha256(obj_or_name):
    """`import_closure(...)["sha256"]` for a class/instance or a module name, or None.

    None for a built-in and for anything whose defining module this process cannot locate on
    `sys.path` without importing it -- an absent field means "not established", never "clean".
    """
    name = obj_or_name if isinstance(obj_or_name, str) else getattr(obj_or_name, "__module__", None)
    if not name or _closure_skip(str(name)):
        return None
    if static_locate(str(name)) is None:
        return None
    return import_closure(str(name))["sha256"]


#: top-level module name -> (distribution, version, dist_sha256). `packages_distributions()` walks
#: every path entry in site-packages and cost ~0.2 s per call -- ~60 % of a short run's wall clock
#: if left uncached, which is far too high a price for a manifest field. Cached for the process:
#: distributions do not appear or change mid-run.
_DIST_CACHE: dict = {}


def _distribution_for(obj):
    """(distribution name, version, dist_sha256) from the wheel RECORD, or (None, None, None).

    `importlib.metadata.distribution(name).files` yields `PackagePath`s each carrying `.hash` (the
    RECORD's base64 sha256); the digest is over `sorted((str(p), p.hash.value))`. ABSENT for
    editable / `.pth` installs -- in which case we say so rather than fabricate.
    """
    try:
        from importlib import metadata
    except ImportError:                                        # pragma: no cover
        return None, None, None
    mod = getattr(obj, "__module__", None)
    if not mod:
        return None, None, None
    top = mod.split(".")[0]
    hit = _DIST_CACHE.get(top)
    if hit is not None:
        return hit
    result = _distribution_uncached(metadata, top)
    _DIST_CACHE[top] = result
    return result


def _distribution_uncached(metadata, top):
    try:
        mapping = metadata.packages_distributions()
    except Exception:                                          # pragma: no cover
        return None, None, None
    names = mapping.get(top) or []
    if not names:
        return None, None, None
    name = sorted(names)[0]
    try:
        dist = metadata.distribution(name)
    except Exception:                                          # pragma: no cover
        return None, None, None
    version = getattr(dist, "version", None)
    files = getattr(dist, "files", None)
    if not files:
        return name, version, None
    # A wheel's RECORD leaves three kinds of row UNHASHED, and none of them means "no provenance":
    # RECORD cannot hash itself, `__pycache__/*.pyc` are written at install time (and a .pyc embeds
    # mtimes and paths, so it must never be hashed anyway), and `.dist-info/direct_url.json` records
    # WHERE it was installed from, which differs between two identical installs. Bailing to None on
    # the first such row -- which is what this did until 2026-08-31 -- made `dist_sha256` null for
    # EVERY normally-installed wheel, not merely for the editable installs the design calls out, and
    # so silently discarded the strongest identity the lock has. Unhashed rows are now carried as
    # (path, "") so the FILE LIST is still covered; None is reserved for the case the design
    # actually meant, where nothing at all is hashed (an editable / .pth install).
    rows, hashed = [], 0
    for p in files:
        rel = str(p).replace("\\", "/")
        if "__pycache__/" in rel or rel.endswith((".pyc", ".pyo")):
            continue
        hv = getattr(getattr(p, "hash", None), "value", None)
        if hv is None:
            rows.append((rel, ""))
        else:
            rows.append((rel, str(hv)))
            hashed += 1
    if not hashed:
        return name, version, None                             # editable install -> no RECORD hashes
    h = hashlib.sha256()
    for rel, hv in sorted(rows):
        h.update(rel.encode("utf-8"))
        h.update(hv.encode("utf-8"))
    return name, version, h.hexdigest()


def provenance_digest(records) -> str:
    """One digest over the whole `loaded` list, in list order (which IS the declared order)."""
    h = hashlib.sha256()
    for r in records:
        h.update(canonical_bytes(r.to_dict() if isinstance(r, ProvenanceRecord) else r))
    return h.hexdigest()


def runtime_block() -> dict:
    """`sys.version` / platform / hash randomisation.

    NOT cosmetic. Python's documented reproducibility guarantee covers ONLY `Random.random()`:
    "the generator's random() method will continue to produce the same sequence when the compatible
    seeder is given the same seed". `gauss()`, `uniform()`, `choice()` and `shuffle()` carry NO
    cross-version guarantee -- and the pinned goldens depend on `gauss` (the shadowing draw at
    run.py:3361) and `uniform` (net_delay). **The digests are pinned to a CPython version as much as
    to a seed.** Recording it is a one-line, digest-free fix that should land regardless of plugins.
    """
    import platform as _platform
    return {
        "python": sys.version,
        "python_version": _platform.python_version(),
        "implementation": _platform.python_implementation(),
        "platform": _platform.platform(),
        "machine": _platform.machine(),
        "hash_randomization": bool(sys.flags.hash_randomization),
    }


# --------------------------------------------------------------------------- #
# Resolution
# --------------------------------------------------------------------------- #
def _entry_point(slot: str, ref: str):
    """Tier 2 -- the installed CATALOGUE, always materialised in `sorted(..., key=name)` order."""
    try:
        from importlib import metadata
    except ImportError:                                        # pragma: no cover
        return None
    group = f"scms_sim_ref.{slot}"
    try:
        eps = list(metadata.entry_points(group=group))
    except Exception:                                          # pragma: no cover
        return None
    for ep in sorted(eps, key=lambda e: e.name):
        if ep.name == ref:
            return ep
    return None


def list_entry_points(slot: str) -> tuple:
    """Catalogue for `--list-plugins` / the GUI dropdown. Never used for activation."""
    try:
        from importlib import metadata
    except ImportError:                                        # pragma: no cover
        return ()
    try:
        eps = list(metadata.entry_points(group=f"scms_sim_ref.{slot}"))
    except Exception:                                          # pragma: no cover
        return ()
    return tuple(sorted(e.name for e in eps))


def _import_ref(ref: str):
    """Tier 3 -- `"pkg.mod:Class"`."""
    mod_name, _, attr = ref.partition(":")
    if not mod_name or not attr:
        raise ConfigError(f"plugin reference {ref!r} must be 'package.module:Attribute'")
    try:
        mod = importlib.import_module(mod_name)
    except ImportError as e:
        raise ConfigError(f"cannot import module {mod_name!r} for plugin {ref!r}: {e}") from e
    obj = mod
    for part in attr.split("."):
        obj = getattr(obj, part, None)
        if obj is None:
            raise ConfigError(f"module {mod_name!r} has no attribute {attr!r} (plugin {ref!r})")
    return obj


def _parse_version(iv: str, slot: str):
    name, _, ver = str(iv).partition("/")
    major, _, minor = ver.partition(".")
    try:
        return name, int(major), int(minor)
    except ValueError:
        raise InterfaceVersionError(
            f"{slot}: interface_version {iv!r} must look like 'ChannelModel/1.0'") from None


def _check_interface_version(obj, slot: str) -> str:
    want_name, want_iv, max_minor, _shapes = INTERFACE[slot]
    _, want_major, _want_minor = _parse_version(want_iv, slot)
    got = getattr(obj, "interface_version", None)
    if got is None:
        raise InterfaceVersionError(
            f"{slot}: {obj!r} declares no interface_version (expected '{want_name}/"
            f"{want_major}.x')")
    name, major, minor = _parse_version(got, slot)
    if name != want_name:
        raise InterfaceVersionError(f"{slot}: interface {name!r} != {want_name!r}")
    if major != want_major:
        raise InterfaceVersionError(
            f"{slot}: interface major {major} != engine major {want_major} ({got!r})")
    if minor > max_minor:
        raise InterfaceVersionError(
            f"{slot}: interface minor {minor} exceeds this engine's maximum "
            f"{max_minor} ({got!r})")
    return got


def _members(obj):
    """The class body to inspect -- a class as given, or the type of an instance."""
    return obj if inspect.isclass(obj) else type(obj)


def _check_signature(obj, slot: str) -> str:
    """The third component: neither ABC nor Protocol does this at runtime.

    Fails BEFORE step 0 with an error naming the offending parameter. Returns the shape the object
    satisfied ("link" or "batch").
    """
    cls = _members(obj)
    shapes = INTERFACE[slot][3]
    errs = []
    for shape, spec in shapes.items():
        problem = _signature_problem(cls, spec)
        if problem is None:
            return shape
        errs.append(f"as a {shape} model: {problem}")
    if len(shapes) == 1:
        raise SignatureError(f"{slot}: {cls.__name__} does not match the declared shape -- "
                             + errs[0].split(": ", 1)[-1])
    raise SignatureError(f"{slot}: {cls.__name__} matches neither declared shape -- " +
                         "; ".join(errs))


def _signature_problem(cls, spec):
    for method, required in spec.items():
        fn = getattr(cls, method, None)
        if fn is None or not callable(fn):
            return f"missing method {method}()"
        try:
            sig = inspect.signature(fn)
        except (TypeError, ValueError):                        # pragma: no cover - builtins
            continue
        names = [p.name for p in sig.parameters.values()
                 if p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)]
        if names and names[0] in ("self", "cls"):
            names = names[1:]
        has_varargs = any(p.kind is p.VAR_POSITIONAL for p in sig.parameters.values())
        if has_varargs:
            continue
        if len(names) < len(required):
            return (f"{method}() takes {names}, but the {INTERFACE_LABEL} requires "
                    f"{list(required)} -- missing {list(required)[len(names):]!r}")
        for want, got in zip(required, names):
            if want != got:
                return (f"{method}() parameter {got!r} should be named {want!r} "
                        f"(declared signature: {method}({', '.join(required)}))")
    return None


INTERFACE_LABEL = "declared interface"


def resolve(slot: str, ref: str):
    """Three tiers, in this order, with every failure fatal here rather than at step k > 0.

    **THE IMPORT IS BRACKETED BY AN INTEGRITY SENTINEL, HERE, unconditionally.** Importing a plugin
    runs its MODULE-LEVEL code, which is an earlier hook than `__init__` and is where a one-line
    ``random.Random = Impostor`` lives. `run_pipeline` already snapshots before it resolves anything
    -- but `run_pipeline` is not the only caller. `verify_lock` (a manifest REPLAY),
    `--check-config`, `--verify-plugins`, the GUI's validation pass and the copilot all resolve, and
    all of them used to import untrusted module-level code with no sentinel at all. On a replay that
    is decisive: `config_from_dict` verifies the lock -- importing every declared plugin -- BEFORE
    `run_pipeline` takes its own baseline, so a module-level tamper was already installed when the
    baseline was captured, the baseline recorded the TAMPERED state as normal, and the end-of-run
    comparison saw no drift. Measured: the digest moved and the run reported clean.

    Snapshotting inside `resolve`, immediately around the import, closes that for every caller at
    once: the tamper is fatal at the import that performed it, so it can never survive to poison a
    later baseline. It costs one snapshot per THIRD-PARTY resolution (built-ins are already
    imported; there is no import to bracket and no third-party code to run), which is a handful per
    run against ~600 attribute reads each.
    """
    if slot not in _BUILTINS:
        raise ConfigError(f"unknown plugin slot {slot!r}; known: {SLOTS}")
    _ensure_builtins(slot)
    ref = str(ref)
    if ref in _BUILTINS[slot]:
        obj, how = _BUILTINS[slot][ref], "builtin"
    else:
        from .integrity import Sentinel
        sentinel = Sentinel(armed=True)
        try:
            ep = _entry_point(slot, ref)
            if ep is not None:
                obj, how = ep.load(), "entry_point"
            elif ":" in ref:
                obj, how = _import_ref(ref), "dotted_path"
            else:
                known = list(builtin_names_sorted(slot)) + list(list_entry_points(slot))
                raise ConfigError(
                    f"unknown {slot}: {ref!r}; known: {sorted(set(known))} "
                    f"(or give a dotted path 'package.module:Class')")
        finally:
            # In the `finally`, so a module whose import RAISES after tampering is still graded --
            # "the import failed" must never be a way to leave a rebind installed and unreported.
            sentinel.verify(f"while IMPORTING the {slot} plugin (module-level code)", subject=ref)
    iv = _check_interface_version(obj, slot)
    shape = _check_signature(obj, slot)
    return obj, how, iv, shape


def make_provenance(slot, order, ref, obj, how, iv, capabilities, declared_streams, params,
                    conformance=None):
    """Build the lock entry for one loaded plugin. Never fabricates: an unestablishable identity is
    recorded as `null` plus `provenance_incomplete: true`.

    The wheel-RECORD probe is skipped for BUILT-INS. It costs ~0.2 s of `site-packages` walking on
    the first call in a process, and it buys nothing there: a built-in's identity is
    `dataset_version` plus its `module_sha256`, and both are recorded. Third-party plugins -- where
    the distribution hash is the STRONGEST identity available and the whole point of the lock --
    still get the full probe.
    """
    if how == "builtin":
        dist, version, dsha, psha, closure = None, None, None, None, None
    else:
        dist, version, dsha = _distribution_for(obj)
        # The one hash that covers a SIBLING module. Third parties only: a built-in's package is
        # this engine, and hashing `src/scms_sim_ref/` would make every manifest unreplayable after
        # any edit to any engine file -- the same reason built-ins are exempt from `verify_lock`.
        root = package_root(obj)
        psha = package_sha256(root) if root else None
        # ...and the one that covers a sibling in ANOTHER top-level package, which `package_sha256`
        # by construction cannot. See `import_closure`.
        mod_name = getattr(obj, "__module__", None)
        closure = (import_closure(mod_name)
                   if mod_name and not _closure_skip(mod_name) and static_locate(mod_name)
                   else None)
    msha = module_sha256(obj)
    incomplete = dsha is None and msha is None and psha is None
    return ProvenanceRecord(
        slot=slot, order=order, ref=ref, resolved_via=how, distribution=dist, version=version,
        dist_sha256=dsha, module_sha256=msha, interface_version=iv,
        capabilities=frozenset(capabilities), declared_streams=tuple(declared_streams),
        params=dict(params or {}), params_sha256=params_sha256(params),
        provenance_incomplete=incomplete, conformance=conformance, package_sha256=psha,
        import_closure=closure)


def check_capabilities(slot, ref, obj, how, caps) -> frozenset:
    """Reserved capabilities are refused from anything that is not a built-in (section 4.1)."""
    caps = frozenset(caps)
    known, reserved = CAPABILITIES.get(slot, (_channel.KNOWN_CAPABILITIES,
                                              _channel.RESERVED_CAPABILITIES))
    if how != "builtin" or not is_builtin(slot, obj):
        bad = sorted(caps & reserved)
        if bad:
            raise CapabilityError(
                f"{slot} {ref!r} declares reserved capability {bad} -- reserved for built-ins "
                f"(these exist only so the built-ins keep their pinned digests)")
    unknown = sorted(c for c in caps if c not in known)
    if unknown:
        raise CapabilityError(f"{slot} {ref!r} declares unknown capability {unknown}; known: "
                              f"{sorted(known)}")
    return caps


def instantiate(cls, *, params, rng, env):
    """The construction contract.

    A third-party model implements `__init__(self, *, params, rng, env)`. A BUILT-IN carries a
    `from_plugin(cls, *, params, rng, env)` classmethod instead, so its historical constructor (the
    one `tests/test_geometric_channel.py` drives directly) is untouched.

    `rng` is an :class:`~scms_sim_ref.api.rng.RngNamespace` -- never the engine's global stream.
    """
    factory = getattr(cls, "from_plugin", None)
    if factory is None:
        factory = cls
    return factory(params=dict(params or {}), rng=rng, env=env)


def plugin_id_of(obj, fallback: str) -> str:
    pid = getattr(obj, "plugin_id", None) or fallback
    return check_plugin_id(str(pid))


# --------------------------------------------------------------------------- #
# Drift detection on replay (D4 / section 4.3)
# --------------------------------------------------------------------------- #
def verify_lock(lock: dict, *, allow_drift: bool = False) -> list:
    """Re-resolve every entry of a manifest's `plugins.loaded` and compare content hashes.

    Two independent layers, and the discrimination between them is the whole value:
    identity drift is detected HERE, BEFORE the run; behavioural drift is detected AFTER, by the
    pinned goldens and the two-run equality gate. `no identity drift + digest drift` means the
    plugin is nondeterministic (or the interpreter changed); `identity drift + no digest drift`
    means a harmless refactor.

    BUILT-INS are exempt from enforcement (their identity is `dataset_version` plus the pinned
    goldens; enforcing run.py's own file hash would make every manifest unreplayable after any edit
    to the engine). Their hashes are still RECORDED.

    Returns the list of drift descriptions (empty when clean); raises
    :class:`~scms_sim_ref.api.errors.PluginDriftError` on the first one unless `allow_drift`, in
    which case every drift is still reported on stderr -- `allow_drift` downgrades a hard stop to a
    loud one, it never makes the change invisible. The new run's own manifest then records what was
    ACTUALLY loaded, so the two locks differ and the change is diffable after the fact.
    """
    drifts = []
    for entry in (lock or {}).get("loaded", []) or []:
        slot, ref = entry.get("slot"), entry.get("ref")
        if entry.get("resolved_via") == "builtin":
            continue
        if entry.get("isolated"):
            # THE ENTRY RAN OUT OF PROCESS, AND SO DOES ITS DRIFT CHECK. `resolve()` imports, and
            # module-level code is an earlier hook than `__init__`; importing an isolated plugin
            # HERE would run the very code the mode exists to keep out -- and a replay is exactly
            # when an unreviewed third-party detector is most likely to be on the path. The child
            # reports where it loaded from and this process hashes those files itself.
            probed, err = _probe_isolated(slot, ref)
            if probed is None:
                drift = PluginDriftError(slot, ref, "resolution", entry.get("module_sha256"), err)
                if not allow_drift:
                    raise drift
                drifts.append(drift.args[0])
                continue
            for fname in ("module_sha256", "package_sha256", "dist_sha256", "interface_version",
                          "import_closure_sha256"):
                expected, actual = entry.get(fname), probed.get(fname)
                if expected is None or actual is None or expected == actual:
                    continue
                drift = PluginDriftError(slot, ref, fname, expected, actual)
                if not allow_drift:
                    raise drift
                drifts.append(drift.args[0])
            continue
        try:
            obj, how, iv, _shape = resolve(slot, ref)
        except ConfigError as e:
            # AN INTEGRITY FAILURE IS NEVER "DRIFT". `resolve` imports, and its own sentinel raises
            # `IntegrityError` (a `ConfigError`) when the plugin's MODULE-LEVEL code rebound an
            # engine object. Recording that as a drift row would make `--allow-plugin-drift`
            # -- whose whole meaning is "the files changed, proceed and record it" -- silently
            # proceed with a rebound `random.Random`, which is the one thing it must never do.
            from .integrity import IntegrityError
            if isinstance(e, IntegrityError):
                raise
            drift = PluginDriftError(slot, ref, "resolution", entry.get("module_sha256"), str(e))
            if not allow_drift:
                raise drift
            drifts.append(drift.args[0])
            continue
        root = package_root(obj)
        for fname, actual in (("module_sha256", module_sha256(obj)),
                              # WITHOUT this row the lock is blind to an edit of any file other than
                              # the one the class is defined in. Measured: `leaky:LeakyChannel`
                              # inherits its whole physics from `rayleigh.py`; adding 6 dB of
                              # transmit power there left `module_sha256` (leaky.py) and
                              # `dist_sha256` (the installer's RECORD) both unmoved, so
                              # `verify-plugins` printed "no drift" and exited 0 while the replay
                              # produced a DIFFERENT dataset. That is D4's stated failure mode.
                              ("package_sha256", package_sha256(root) if root else None),
                              # ...and WITHOUT this row it is blind to an edit of a module in a
                              # DIFFERENT top-level package, which `package_sha256` cannot reach by
                              # construction. Measured: a channel model importing its physics from a
                              # sibling distribution moved the dataset digest 9ea985e0 -> ec9e476d
                              # while `verify-plugins` reported "no drift" and exited 0.
                              ("import_closure_sha256", import_closure_sha256(obj)),
                              ("dist_sha256", _distribution_for(obj)[2]),
                              ("interface_version", iv)):
            expected = entry.get(fname)
            if expected is None or actual is None:
                continue
            if expected != actual:
                drift = PluginDriftError(slot, ref, fname, expected, actual)
                if not allow_drift:
                    raise drift
                drifts.append(drift.args[0])
    for msg in drifts:
        print(f"[plugins] DRIFT ALLOWED: {msg}", file=sys.stderr)
    return drifts


def _probe_isolated(slot, ref):
    """(hashes, None) for an isolated lock entry, or (None, reason). Never imports the plugin."""
    from . import isolate as _isolate
    try:
        meta = _isolate.probe(str(slot), str(ref))
    except ApiError as e:
        return None, str(e)
    except Exception as e:                                         # pragma: no cover - defensive
        return None, f"{type(e).__name__}: {e}"
    return {"module_sha256": meta.get("module_sha256"),
            "package_sha256": meta.get("package_sha256"),
            "dist_sha256": meta.get("dist_sha256"),
            # Computed by THIS process from the path IT resolves, never from the worker's report.
            "import_closure_sha256": meta.get("import_closure_sha256"),
            "interface_version": meta.get("interface_version")}, None
