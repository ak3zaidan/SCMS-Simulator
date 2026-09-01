"""Whole-run integrity monitoring for in-process plugins. **Detection, never containment.**

Read this before anything else, because everything below is only worth having if it is understood
correctly.

**An in-process Python plugin cannot be sandboxed.** ``sys._getframe``, ``gc.get_referrers``,
``inspect``, module globals and ``__subclasses__`` walking are reachable from any callable the engine
invokes, and no arrangement of frozen dataclasses, wrapper objects or namespaced mappings changes
that. This module does not attempt to prevent anything. It answers a narrower question that the rest
of the seam could not answer at all:

    *Is the engine, at the end of the run, still made of the same objects it was made of at the
    start?*

Two defects made that question load-bearing, and both were measured on this engine.

**1. Attestation ran AFTER construction.** ``build_channel`` instantiated the run's plugin and only
then called the conformance suite on it; the detector slot repeated the ordering. A hostile
``__init__`` therefore executed *before* it was ever gated, so it could rebind
``random.Random.random``, replace ``api.channel.check_outcome``, or monkeypatch the conformance
runner itself -- and then pass attestation, because attestation was being performed by a process the
candidate had already edited. :class:`Sentinel` closes the residue of that ordering: even with
attestation moved in front of construction and moved OUT OF PROCESS, the run's own instance is still
built in this interpreter, so its construction is bracketed and any tamper is fatal *before step 0*,
before an output directory exists.

**2. A time-delayed attack evades every conformance trap.** A ``random.Random`` class rebind
installed at ``frame.step >= 30`` passed all four of C3's traps. That is not a hole in the traps: a
fixed-window contract test can only certify the behaviour it observed, and lengthening the window
just moves the threshold an attacker waits out. **In-process conformance is a sampling check, not a
proof.** The answer is monitoring that holds for the WHOLE run, which is what
:meth:`Sentinel.verify` at the end of the run and :class:`WitnessedRandom` are.

What is watched, and why each entry is here:

* **the RNG primitives** -- ``random.Random`` itself, every generator method on it, the module-level
  bindings, and the hidden module instance. A class rebind reaches every keyed
  ``random.Random(f"{seed}:...")`` construction the engine makes *after* the rebind (about twenty
  sites); a method rebind reaches the engine's own private stream, whose draw count and order are
  load-bearing for every pinned golden.
* **the engine's own gates** -- ``check_outcome``, ``srcgate.gate``, ``registry.resolve``,
  ``registry.verify_lock``, ``run._attest``, ``run._assert_config_unmoved``, ``run._data_digest``,
  ``run._write_manifest``, ``conformance.runner.run_ref``. A plugin that replaces one of these has
  disabled the thing that was supposed to be judging it, and every previous layer of this project was
  blind to exactly that.
* **the objects the boundary is made of** -- ``Observation``, ``NamespacedState``,
  ``LinkOutcome``, ``namespaced_key``, the RSSI bounds and the closed link-state vocabulary.
* **interpreter-wide hooks** -- a trace or profile function INSTALLED during the run, and additions
  to ``sys.meta_path``.
* **a small builtins core** -- ``getattr``, ``setattr``, ``open``, ``round``, ``float``.

And what is NOT claimed, stated as plainly as the rest:

* This is **detection after the fact**, at named checkpoints. A plugin that tampers, reads what it
  wants and restores the binding before the checkpoint is not seen. Closing that needs a boundary
  this process does not have.
* The sentinel is itself an object in the same address space. It watches its own module entry, which
  raises the bar and does not remove it.
* A **passing** verification means "nothing on the watch list moved", never "this plugin is honest".
  Reading the oracle through a frame walk moves nothing at all, and stays perfectly reproducible
  while doing it. Genuinely untrusted detector code needs an out-of-process boundary, where the
  oracle is not in the address space. See `docs/realism/DETECTOR-PLUGIN.md` section 2.

**Armed only when the config declares plugins** (``cfg.plugins`` non-empty). A run with no plugins
runs no third-party code, so there is nothing to monitor and the default path pays nothing -- which
is also why the no-plugin reference digest is untouched by any of this.
"""
from __future__ import annotations

import random
import sys

from .errors import ConfigError


class IntegrityError(ConfigError):
    """The engine is no longer made of the objects it was made of.

    A `ConfigError` subclass on purpose: the engine already refuses a `ConfigError` before step 0 and
    before any output directory exists, which is exactly the handling a construction-time tamper
    needs. At the END of a run it is raised before the manifest is written, so the partial dataset is
    claimed by nothing.
    """


# --------------------------------------------------------------------------- #
# The watch list
# --------------------------------------------------------------------------- #
#: Every public generator method on `random.Random`. Watched on the CLASS, which also covers the
#: module-level `random.random()` shims (bound methods of the hidden global instance).
RANDOM_METHODS = (
    "random", "getrandbits", "randbytes", "randrange", "randint", "choice", "choices", "shuffle",
    "sample", "uniform", "triangular", "normalvariate", "gauss", "lognormvariate", "expovariate",
    "vonmisesvariate", "gammavariate", "betavariate", "paretovariate", "weibullvariate",
    "binomialvariate", "seed", "getstate", "setstate", "_randbelow",
)

#: Module-level names in `random` that are bound methods of the hidden instance.
RANDOM_MODULE_NAMES = ("random", "seed", "getstate", "setstate", "getrandbits", "randint",
                       "randrange", "choice", "shuffle", "sample", "uniform", "gauss",
                       "expovariate", "triangular", "betavariate", "gammavariate")

#: sentinel: the attribute is inherited from the C base `_random.Random`, not owned by `random.Random`
_INHERITED = object()

#: module -> attributes. Resolved through `sys.modules` at capture time, so this module imports
#: nothing from the engine and a module that is not loaded is simply not watched.
MODULE_WATCH = (
    ("scms_sim_ref.api.channel",
     ("check_outcome", "sort_outcomes", "_checked_evaluate", "PerLinkAdapter", "BatchAdapter",
      "LinkOutcome", "StepFrame", "StationSnapshot", "RSSI_MIN_DBM", "RSSI_MAX_DBM", "LINK_STATES",
      "RESERVED_CAPABILITIES", "reach_of", "window_of", "loss_composition")),
    ("scms_sim_ref.api.detect",
     ("Observation", "NamespacedState", "namespaced_key", "fires", "VIOLATION_THRESHOLD",
      "RESERVED_STATE_KEYS", "RESERVED_CAPABILITIES", "CheckBase", "FusionBase")),
    ("scms_sim_ref.api.registry",
     ("resolve", "instantiate", "verify_lock", "make_provenance", "check_capabilities",
      "module_sha256", "package_sha256", "package_root", "is_builtin", "register_builtin",
      "plugin_id_of", "canonical_bytes", "provenance_digest")),
    ("scms_sim_ref.api.srcgate", ("gate", "scan_source", "check_mode", "module_source")),
    ("scms_sim_ref.api.rng", ("RngNamespace", "check_plugin_id")),
    ("scms_sim_ref.api.integrity", ("Sentinel", "WitnessedRandom", "IntegrityError")),
    ("scms_sim_ref.conformance.runner", ("run_ref", "run_contract", "ConformanceReport")),
    ("scms_sim_ref.mock_pipeline.run",
     ("_attest", "_round_score", "_assert_config_unmoved", "_data_digest", "_write_manifest",
      "_file_sha256", "plugin_block", "build_channel", "build_checks", "_claim_column",
      "_assert_not_hijacked", "default_check_refs", "ReadOnlyConfig", "_config_dict")),
    ("scms_sim_ref.mock_pipeline.detectors", ("BUILTIN_CHECKS", "BUILTIN_CHECK_BY_CODE",
                                              "BUILTIN_FUSION_BY_NAME")),
    ("scms_sim_ref.datagen.featurize", ("REASON_VOCAB", "DETECTORS", "_observed_reasons",
                                        "_observed_detectors")),
    ("builtins", ("getattr", "setattr", "delattr", "open", "round", "float", "int", "sorted",
                  "isinstance", "__import__")),
    ("hashlib", ("sha256",)),
    ("json", ("dumps",)),
)


def _random_surface() -> dict:
    surface = {("random.Random", n): random.Random.__dict__.get(n, _INHERITED)
               for n in RANDOM_METHODS}
    surface[("random", "Random")] = random.Random
    surface[("random", "SystemRandom")] = getattr(random, "SystemRandom", None)
    surface[("random", "_inst")] = getattr(random, "_inst", None)
    for n in RANDOM_MODULE_NAMES:
        surface[("random", n)] = getattr(random, n, None)
    return surface


def _module_surface() -> dict:
    out = {}
    for mod_name, attrs in MODULE_WATCH:
        mod = sys.modules.get(mod_name)
        if mod is None:
            continue
        for attr in attrs:
            try:
                out[(mod_name, attr)] = getattr(mod, attr)
            except AttributeError:
                continue
    return out


class Sentinel:
    """A snapshot of the engine's identity-critical objects, and the comparison against it.

    Capture is ~150 attribute reads and costs microseconds; :meth:`verify` costs the same. It is
    therefore affordable at every construction boundary AND at the end of the run, which is the
    point: a bounded conformance window cannot see a step-30 attack, and this can.
    """

    __slots__ = ("_rand", "_mods", "_trace", "_profile", "_meta", "armed")

    def __init__(self, *, armed: bool = True):
        self.armed = bool(armed)
        if not self.armed:
            self._rand = self._mods = {}
            self._trace = self._profile = None
            self._meta = ()
            return
        self._rand = _random_surface()
        self._mods = _module_surface()
        self._trace = sys.gettrace()
        self._profile = sys.getprofile()
        self._meta = tuple(sys.meta_path)

    @property
    def watched(self) -> int:
        """How many named objects this snapshot covers -- the manifest records it, so a reader can
        tell a monitored run from an unmonitored one without trusting a boolean."""
        return (len(self._rand) + len(self._mods) + 3) if self.armed else 0

    # -- comparison --------------------------------------------------------------------------- #
    def drift(self) -> list:
        """Sorted ``(what, detail)`` pairs for everything on the watch list that moved."""
        if not self.armed:
            return []
        out = []
        # The CLASS first. When `random.Random` itself has been rebound, every per-method comparison
        # below is against a different class and would report twenty consequences of one cause, so
        # the cause is reported alone.
        class_moved = random.Random is not self._rand[("random", "Random")]
        if not class_moved:
            for name in RANDOM_METHODS:
                was = self._rand[("random.Random", name)]
                now = random.Random.__dict__.get(name, _INHERITED)
                if now is not was:
                    out.append((f"random.Random.{name}",
                                f"{_describe(was)} -> {_describe(now)}; a method rebind on the class "
                                f"reaches EVERY stream in this interpreter at once, including the "
                                f"engine's own private random.Random(cfg.seed), while leaving every "
                                f"generator STATE a snapshot could compare perfectly intact"))
        for key in (("random", "Random"), ("random", "SystemRandom"), ("random", "_inst")):
            was, now = self._rand[key], getattr(random, key[1], None)
            if now is not was:
                out.append((f"{key[0]}.{key[1]}",
                            f"{_describe(was)} -> {_describe(now)}; the engine constructs keyed "
                            f"streams as random.Random(f'{{seed}}:...') at ~20 sites DURING the "
                            f"run, so a class rebind installed at step k owns every one of them "
                            f"from step k onward"))
        for name in RANDOM_MODULE_NAMES:
            was, now = self._rand[("random", name)], getattr(random, name, None)
            if now is not was:
                out.append((f"random.{name}", f"{_describe(was)} -> {_describe(now)}"))
        for (mod_name, attr), was in sorted(self._mods.items()):
            mod = sys.modules.get(mod_name)
            now = getattr(mod, attr, _MISSING) if mod is not None else _MISSING
            if now is not was:
                out.append((f"{mod_name}.{attr}",
                            f"{_describe(was)} -> {_describe(now)}{_why(mod_name, attr)}"))
        if self._trace is None and sys.gettrace() is not None:
            out.append(("sys.settrace",
                        "a trace hook was INSTALLED during the run; a trace function sees every "
                        "engine frame and its locals, which is the broadcast dict and the oracle"))
        if self._profile is None and sys.getprofile() is not None:
            out.append(("sys.setprofile", "a profile hook was INSTALLED during the run"))
        added = [m for m in sys.meta_path if not any(m is old for old in self._meta)]
        if added:
            out.append(("sys.meta_path",
                        f"{len(added)} import finder(s) added during the run: "
                        f"{[type(m).__name__ for m in added]}; a meta_path finder can substitute "
                        f"any module the engine imports after it"))
        out.sort()
        return out

    # -- restoration -------------------------------------------------------------------------- #
    def restore(self) -> None:
        """Put back everything restorable. A detector that finds a tamper and leaves it installed
        has poisoned the interpreter for whatever runs next, which is worse than not looking."""
        if not self.armed:
            return
        # THE CLASS FIRST, and the order is load-bearing: with `random.Random` rebound to an
        # impostor, the per-method loop below would write the engine's original methods ONTO THE
        # IMPOSTOR and leave the rebind itself in place -- restoring nothing while looking like it had.
        for name in ("Random", "SystemRandom", "_inst") + RANDOM_MODULE_NAMES:
            was = self._rand.get(("random", name), _MISSING)
            if was is not _MISSING and getattr(random, name, None) is not was:
                setattr(random, name, was)
        for name in RANDOM_METHODS:
            was = self._rand[("random.Random", name)]
            if random.Random.__dict__.get(name, _INHERITED) is was:
                continue
            if was is _INHERITED:
                try:
                    delattr(random.Random, name)
                except AttributeError:                            # pragma: no cover
                    pass
            else:
                setattr(random.Random, name, was)
        for (mod_name, attr), was in self._mods.items():
            mod = sys.modules.get(mod_name)
            if mod is not None and getattr(mod, attr, _MISSING) is not was:
                try:
                    setattr(mod, attr, was)
                except Exception:                                 # pragma: no cover
                    pass
        if self._trace is None and sys.gettrace() is not None:
            sys.settrace(None)
        if self._profile is None and sys.getprofile() is not None:
            sys.setprofile(None)

    # -- the gate ----------------------------------------------------------------------------- #
    def verify(self, when: str, *, subject: str = "") -> None:
        """Raise :class:`IntegrityError` if anything on the watch list moved, after restoring it."""
        moved = self.drift()
        if not moved:
            return
        self.restore()
        raise IntegrityError(_message(when, subject, moved))


_MISSING = object()


def _why(mod_name: str, attr: str) -> str:
    if mod_name.startswith("scms_sim_ref"):
        return (f"; {mod_name}.{attr} is an ENGINE object -- replacing it disables or rewrites the "
                f"very machinery that grades the plugin, and it is deterministic while doing so, so "
                f"no digest, golden or content hash in this project can see it")
    return "; a stdlib primitive the engine's gates are built out of"


def _describe(obj) -> str:
    if obj is _INHERITED:
        return "<inherited from _random.Random>"
    if obj is _MISSING:
        return "<deleted>"
    mod = getattr(obj, "__module__", None)
    name = getattr(obj, "__qualname__", None) or getattr(obj, "__name__", None)
    if name:
        return f"{mod}.{name}" if mod else str(name)
    return f"{type(obj).__name__}({obj!r})"[:120]


def _message(when: str, subject: str, moved) -> str:
    head = f"INTEGRITY FAILURE {when}"
    if subject:
        head += f" ({subject})"
    lines = [head + f": {len(moved)} watched object(s) are no longer what they were:"]
    for what, detail in moved:
        lines.append(f"  {what}")
        lines.append(f"      {detail}")
    lines += [
        "",
        "WHAT THIS MEANS. Only plugin code can do this: nothing in the engine rebinds any of the",
        "objects above between the start of a run and its end. The dataset is therefore produced by",
        "an engine that is not the engine the manifest, the goldens and the content-hash lock",
        "describe, so no manifest is written and the run is refused.",
        "",
        "WHY CONFORMANCE DID NOT CATCH IT. The conformance suite exercises a BOUNDED WINDOW of",
        "steps. A side effect guarded by `if frame.step >= 30` is invisible to any window shorter",
        "than that, and lengthening the window only moves the number an attacker waits out. An",
        "in-process contract suite is a SAMPLING CHECK, not a proof; this monitor is the part that",
        "holds for the whole run.",
        "",
        "WHAT THIS IS NOT. It is not a sandbox and it is not proof of honesty. A plugin that reads",
        "the labels through a frame walk moves nothing on this list at all. Genuinely untrusted",
        "detector code needs an out-of-process boundary, where the oracle is not in the address",
        "space. See docs/realism/DETECTOR-PLUGIN.md section 2.",
        "",
        "The watched bindings have been RESTORED, so this interpreter is usable again.",
    ]
    return "\n".join(lines)


# --------------------------------------------------------------------------- #
# The engine's own stream, and the proof that it advanced exactly as much as it was asked to
# --------------------------------------------------------------------------- #
class WitnessedRandom(random.Random):
    """The engine's global `random.Random(cfg.seed)`, counting Mersenne-Twister WORDS.

    **Bit-identical to `random.Random`.** Only `random()` and `getrandbits()` are overridden, each
    adds one integer increment and delegates to the same C primitive, and every other generator
    method in `random.py` is built on those two -- so the VALUES, the draw order and therefore every
    pinned digest are unchanged. Measured on this host: 0.016 s of extra cost per 400 000 draws
    (40 ns each), and `tests/test_detector_plugins.py` asserts that a run declaring the built-ins
    explicitly (which arms this) is byte-identical to one that does not (which does not).

    **What the count buys.** MT19937 consumes exactly two 32-bit words per `random()` and
    `ceil(k/32)` per `getrandbits(k)`, and its state after W words from a known start state is fully
    determined. So :meth:`verify_stream` replays W words against a pristine generator seeded the same
    way and compares the 625-tuple. The two can only disagree if something OTHER than a counted draw
    moved -- or failed to move -- the engine's stream: a `random.Random.random` rebind (the override
    still counts 2 words while the attacker's function advances nothing), a `setstate()` jump, or a
    C-level poke. That is the second half of the whole-run answer, and it is orthogonal to the
    identity snapshot above: the identity check catches a rebind that has not been *used* yet, and
    this catches one that was used and then removed before the checkpoint.

    Honest about the residue: a plugin that draws from this object *through* the counted methods is
    counted, so the states still agree -- what gives that away is the draw count itself moving away
    from what the engine's own arithmetic explains, which nothing here can compute. The declared
    property is "the stream advanced exactly as many words as it was ASKED for", not "only the engine
    asked".
    """

    __slots__ = ("_words", "_state0")

    def __init__(self, seed=None):
        self._words = 0
        super().__init__(seed)
        self._state0 = super().getstate()

    @property
    def words(self) -> int:
        return self._words

    def random(self):
        self._words += 2
        return super().random()

    def getrandbits(self, k):
        self._words += (int(k) + 31) >> 5
        return super().getrandbits(k)

    def verify_stream(self, when: str) -> dict:
        """Raise :class:`IntegrityError` unless the stream is exactly `words` words along."""
        probe = random.Random()
        probe.setstate(self._state0)
        if self._words:
            # `getrandbits(32*W)` consumes exactly W words in ONE C call (CPython computes
            # `words = (k-1)//32 + 1` and calls genrand_uint32 that many times), so the replay is
            # a few milliseconds even for the ~10^6-word streams a long run produces.
            probe.getrandbits(32 * self._words)
        expected = probe.getstate()[1]
        actual = super().getstate()[1]
        if expected != actual:
            raise IntegrityError(
                f"INTEGRITY FAILURE {when}: the engine's global random stream is NOT where "
                f"{self._words} counted Mersenne-Twister words from seed state would put it.\n"
                f"  Every draw the engine makes is counted here, and MT19937's state after W words "
                f"from a fixed start is fully determined, so the two can only disagree if something "
                f"moved the stream outside the counted methods (a setstate() jump) or made a counted "
                f"call NOT advance it (a random.Random.random rebind -- the classic form, which "
                f"leaves the generator state a snapshot would compare perfectly intact).\n"
                f"  Only plugin code can do this. No manifest is written.")
        return {"words": self._words}


def engine_random(seed, *, armed: bool):
    """The run's global stream: witnessed when plugins are declared, a plain `random.Random` when
    they are not. Bit-identical either way; see :class:`WitnessedRandom`."""
    return WitnessedRandom(seed) if armed else random.Random(seed)


def armed_for(cfg) -> bool:
    """True when the config DECLARES plugins -- the only way third-party code enters a run.

    A config-level decision, made before anything is resolved or constructed, so the arming is a
    property of the replayable input rather than of what happened to be importable.
    """
    plugins = getattr(cfg, "plugins", None)
    return bool(plugins)
