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
from .errors import (CapabilityError, ConfigError, InterfaceVersionError, PluginDriftError,
                     SignatureError)
from .rng import check_plugin_id

API_VERSION = "1.0"

#: slot -> {name: class}. Registration order is the DISPLAY order (it is what `_ENUM_OPTIONS` and
#: argparse `choices` publish); every ITERATION for determinism-bearing purposes goes through
#: :func:`builtin_names_sorted`.
_BUILTINS: dict = {"channel_model": {}, "check": {}, "fusion": {}, "message_codec": {},
                   "mobility": {}, "report_format": {}}

SLOTS: tuple = tuple(sorted(_BUILTINS))

#: slot -> (interface name, interface version, {method: (required positional parameter names,)})
INTERFACE: dict = {
    "channel_model": (
        _channel.INTERFACE_NAME, _channel.INTERFACE_VERSION,
        {"capabilities": (),
         "begin_step": ("frame",),
         "evaluate": ("tx", "rx", "d_m", "txn")},
        {"capabilities": (),
         "begin_step": ("frame",),
         "deliver": ("frame", "candidates")},
    ),
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
    return tuple(_BUILTINS[slot])


def builtin_names_sorted(slot: str) -> tuple:
    """Sorted snapshot -- the only order used anywhere a digest can see it."""
    return tuple(sorted(_BUILTINS[slot]))


def builtin(slot: str, name: str):
    return _BUILTINS[slot].get(name)


def is_builtin(slot: str, obj) -> bool:
    return any(o is obj for o in _BUILTINS[slot].values())


# --------------------------------------------------------------------------- #
# Provenance
# --------------------------------------------------------------------------- #
class ProvenanceRecord:
    """One entry of `manifest["plugins"]["loaded"]` -- the lock, not the intent."""

    __slots__ = ("slot", "order", "ref", "resolved_via", "distribution", "version",
                 "dist_sha256", "module_sha256", "interface_version", "capabilities",
                 "declared_streams", "params", "params_sha256", "provenance_incomplete",
                 "conformance")

    def __init__(self, slot, order, ref, resolved_via, distribution, version, dist_sha256,
                 module_sha256, interface_version, capabilities, declared_streams, params,
                 params_sha256, provenance_incomplete, conformance=None):
        self.slot, self.order, self.ref = slot, order, ref
        self.resolved_via, self.distribution, self.version = resolved_via, distribution, version
        self.dist_sha256, self.module_sha256 = dist_sha256, module_sha256
        self.interface_version, self.capabilities = interface_version, capabilities
        self.declared_streams, self.params = declared_streams, params
        self.params_sha256 = params_sha256
        self.provenance_incomplete = provenance_incomplete
        #: The `conformance_report.json` summary, when the config declared
        #: `plugins.<slot>.conformance = "required"` and the suite passed. Absent otherwise --
        #: an absent field means "not attested", never "attested and failed", because a failing
        #: attestation raises before the run and no manifest is written at all.
        self.conformance = conformance

    def to_dict(self) -> dict:
        d = {"slot": self.slot, "order": self.order, "ref": self.ref,
             "resolved_via": self.resolved_via, "distribution": self.distribution,
             "version": self.version, "dist_sha256": self.dist_sha256,
             "module_sha256": self.module_sha256, "interface_version": self.interface_version,
             "capabilities": sorted(self.capabilities), "declared_streams": list(self.declared_streams),
             "params": self.params, "params_sha256": self.params_sha256}
        if self.provenance_incomplete:
            d["provenance_incomplete"] = True
        if self.conformance:
            d["conformance"] = self.conformance
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


def module_sha256(obj):
    """sha256 of the defining source file, or of the package directory walked in SORTED order.

    Always-available fallback for `dist_sha256`. Fails (returns None) for C extensions and
    `exec`-created classes. NEVER hashes `__pycache__`: a `.pyc` embeds mtimes and paths.
    """
    try:
        src = inspect.getsourcefile(obj) or inspect.getfile(obj)
    except (TypeError, OSError):
        return None
    if not src:
        return None
    try:
        st = os.stat(src)
    except OSError:
        return None
    if not os.path.isfile(src):
        return None
    key = (src, st.st_mtime_ns, st.st_size)
    hit = _MODULE_HASH_CACHE.get(key)
    if hit is not None:
        return hit
    try:
        digest = _sha256_file(src)
    except OSError:
        return None
    _MODULE_HASH_CACHE[key] = digest
    return digest


def package_sha256(root: str):
    """sha256 over a package directory: `sorted((relpath, filehash))`, `__pycache__` excluded.

    Structurally the same helper as `_data_digest` (run.py:3779), deliberately.
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
            h.update(rel.encode("utf-8"))
            try:
                h.update(_sha256_file(full).encode())
            except OSError:
                return None
    return h.hexdigest()


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
    want_name, want_iv, *_ = INTERFACE[slot]
    _, want_major, want_minor_max = _parse_version(want_iv, slot)
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
    if minor > _channel.MAX_MINOR:
        raise InterfaceVersionError(
            f"{slot}: interface minor {minor} exceeds this engine's maximum "
            f"{_channel.MAX_MINOR} ({got!r})")
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
    _, _, link_spec, batch_spec = INTERFACE[slot]
    errs = []
    for shape, spec in (("link", link_spec), ("batch", batch_spec)):
        problem = _signature_problem(cls, spec)
        if problem is None:
            return shape
        errs.append(f"as a {shape} model: {problem}")
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
    """Three tiers, in this order, with every failure fatal here rather than at step k > 0."""
    if slot not in _BUILTINS:
        raise ConfigError(f"unknown plugin slot {slot!r}; known: {SLOTS}")
    ref = str(ref)
    if ref in _BUILTINS[slot]:
        obj, how = _BUILTINS[slot][ref], "builtin"
    else:
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
        dist, version, dsha = None, None, None
    else:
        dist, version, dsha = _distribution_for(obj)
    msha = module_sha256(obj)
    incomplete = dsha is None and msha is None
    return ProvenanceRecord(
        slot=slot, order=order, ref=ref, resolved_via=how, distribution=dist, version=version,
        dist_sha256=dsha, module_sha256=msha, interface_version=iv,
        capabilities=frozenset(capabilities), declared_streams=tuple(declared_streams),
        params=dict(params or {}), params_sha256=params_sha256(params),
        provenance_incomplete=incomplete, conformance=conformance)


def check_capabilities(slot, ref, obj, how, caps) -> frozenset:
    """Reserved capabilities are refused from anything that is not a built-in (section 4.1)."""
    caps = frozenset(caps)
    if how != "builtin" or not is_builtin(slot, obj):
        bad = sorted(caps & _channel.RESERVED_CAPABILITIES)
        if bad:
            raise CapabilityError(
                f"{slot} {ref!r} declares reserved capability {bad} -- reserved for built-ins "
                f"(these exist only so `disc`/`logdistance` keep digest 0bd93655...)")
    unknown = sorted(c for c in caps if c not in _channel.KNOWN_CAPABILITIES)
    if unknown:
        raise CapabilityError(f"{slot} {ref!r} declares unknown capability {unknown}; known: "
                              f"{sorted(_channel.KNOWN_CAPABILITIES)}")
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
        try:
            obj, how, iv, _shape = resolve(slot, ref)
        except ConfigError as e:
            drift = PluginDriftError(slot, ref, "resolution", entry.get("module_sha256"), str(e))
            if not allow_drift:
                raise drift
            drifts.append(drift.args[0])
            continue
        for fname, actual in (("module_sha256", module_sha256(obj)),
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
