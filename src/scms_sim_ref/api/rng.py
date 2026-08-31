"""`RngNamespace` -- capability by omission (PLUGIN-ARCHITECTURE.md D3 / section 4.1).

**A plugin never receives the engine's global `rng`** (`random.Random(cfg.seed)` at run.py:1952).
That single stream's draw COUNT AND ORDER are load-bearing across packet loss (run.py:3393),
`report_prob` (3536), collusion (3566) and net_delay (2620); one extra or missing draw shifts every
subsequent value in the run. There is no way to police that after the fact, so the only robust
control is to not hand over the object. A plugin gets this instead.

Why string keys. CPython's `Random.seed(a, version=2)` for a `str` computes
`int.from_bytes(a.encode() + sha512(a.encode()).digest(), "big")`, so it is
**`PYTHONHASHSEED`-independent and platform-stable** -- unlike `hash()`. This is already the
convention at ~20 sites in the engine (`random.Random(f"{seed}:{label}:{ids}")`). It is
structurally stronger than ns-3's `AssignStreams`, whose integer stream indices ns-3's own manual
concedes are "sensitive to perturbations of the simulation configuration": string keys are order-free
and creation-order-independent BY CONSTRUCTION, so no `AssignStreams` equivalent is needed.
"""
from __future__ import annotations

import random
import re

#: A plugin id is a reserved namespace segment. Kept narrow so it can never need quoting inside a
#: colon-joined stream key, and so it is a legal JSON key, python identifier and column suffix.
PLUGIN_ID_RE = re.compile(r"^[a-z0-9_]{2,32}$")

#: The reserved key segment. `plugin:<id>:` can never collide with a core label such as
#: "shadow2", "geo", "shadow", "denm" or "lc" -- the string-space equivalent of ns-3 partitioning
#: MRG32k3a into 1.8e19 streams.
RESERVED_PREFIX = "plugin"


def check_plugin_id(plugin_id: str) -> str:
    from .errors import ConfigError
    if not isinstance(plugin_id, str) or not PLUGIN_ID_RE.match(plugin_id):
        raise ConfigError(f"plugin_id {plugin_id!r} must match {PLUGIN_ID_RE.pattern}")
    return plugin_id


class RngNamespace:
    """Handed to every plugin. The plugin's ONLY source of randomness.

    Two modes, and the difference is the whole point:

    * :meth:`stream` -- STATELESS, the default and the only mode unless the plugin declares the
      ``stateful`` capability. The key INCLUDES the step and the object is discarded, so the value
      is a pure function of ``(seed, link, step)`` and is immune to call ORDER and call COUNT. Cost:
      memoryless -- no AR(1) correlation is expressible.
    * :meth:`persistent` -- STATEFUL. The key EXCLUDES the step; this object owns the cache and
      records the per-key advance count so the conformance suite can assert it is CONSTANT across
      steps (check C4). That is exactly `GeometricChannel`'s `if st["step"] == self.step` guard,
      promoted from a private convention to an enforceable one.

    `replicate` is ns-3's `RngRun` doctrine -- independent replications come from advancing the RUN
    NUMBER, not from changing the seed. It is folded into the prefix ONLY when non-zero, so every
    existing digest is untouched. (Phase 1 ships the mechanism; no `replicate` config field is added,
    because the Phase 1 gate requires `config_schema()` to grow exactly one key.)
    """

    __slots__ = ("_seed", "_pid", "_cache", "_step", "_counts", "_labels", "_replicate", "_prefix")

    def __init__(self, seed: int, plugin_id: str, replicate: int = 0):
        self._seed = int(seed)
        self._pid = check_plugin_id(plugin_id)
        self._replicate = int(replicate)
        self._prefix = ("%d" % self._seed) if self._replicate == 0 else ("%d.%d" % (self._seed,
                                                                                    self._replicate))
        self._cache: dict = {}
        self._counts: dict = {}
        self._labels: set = set()
        self._step = -1

    # -- engine-side ------------------------------------------------------------------------- #
    @property
    def plugin_id(self) -> str:
        return self._pid

    @property
    def step(self) -> int:
        return self._step

    def begin_step(self, step: int) -> None:
        """Called by the ENGINE once per step, never by the plugin."""
        self._step = int(step)

    def declared_streams(self) -> tuple:
        """Sorted labels this namespace has actually produced -- recorded in the manifest so a
        plugin's randomness surface is a declared, diffable property of the dataset."""
        return tuple(sorted(self._labels))

    def advance_counts(self) -> dict:
        """key -> number of times :meth:`persistent` handed that key out this run (check C4)."""
        return dict(self._counts)

    # -- plugin-side ------------------------------------------------------------------------- #
    def key(self, label: str, *ids, with_step: bool) -> str:
        parts = [self._prefix, RESERVED_PREFIX, self._pid, str(label)]
        parts.extend(str(i) for i in ids)
        if with_step:
            parts.append("s%d" % self._step)
        return ":".join(parts)

    def stream(self, label: str, *ids) -> random.Random:
        """STATELESS. Pure function of (seed, replicate, plugin, label, ids, step)."""
        self._labels.add(str(label))
        return random.Random(self.key(label, *ids, with_step=True))

    def persistent(self, label: str, *ids) -> random.Random:
        """STATEFUL (capability ``stateful``). The FRAMEWORK owns the cache."""
        self._labels.add(str(label))
        k = self.key(label, *ids, with_step=False)
        r = self._cache.get(k)
        if r is None:
            r = self._cache[k] = random.Random(k)
        self._counts[k] = self._counts.get(k, 0) + 1
        return r

    def forget(self, label: str, *ids) -> None:
        """Drop one persistent stream (the engine prunes streams for despawned stations, exactly as
        run.py:2764-2767 prunes `GeometricChannel._shadow` / `_packet`)."""
        self._cache.pop(self.key(label, *ids, with_step=False), None)

    # numpy is OUT on the engine path (section 4.1): it is undeclared in pyproject.toml, BLAS thread
    # counts and float reduction order drift across hosts, and `det[k] >= 1.0` is a CLIFF. This is
    # the documented derivation for a plugin that wants numpy for OFFLINE work only.
    def numpy_seed_sequence(self, label: str, *ids):
        """`numpy.random.SeedSequence` derived from this namespace. Offline use only."""
        import numpy  # noqa: PLC0415 - deliberately lazy; numpy is not an engine dependency
        k = self.key(label, *ids, with_step=False)
        return numpy.random.SeedSequence(entropy=self._seed,
                                         spawn_key=tuple(b for b in k.encode("utf-8")))

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return f"RngNamespace(seed={self._seed}, plugin_id={self._pid!r}, step={self._step})"


def make_namespace(seed: int, plugin_id: str, replicate: int = 0) -> RngNamespace:
    return RngNamespace(seed, plugin_id, replicate)


def legacy_keyed(seed: int, label: str, *ids) -> random.Random:
    """The engine's own pre-plugin keyed-stream convention `random.Random(f"{seed}:{label}:{ids}")`.

    Exposed so a BUILT-IN moved onto the interface can keep its exact historical key -- which is
    what keeps `939b4faa...` (logdistance) and the geometric channel's streams byte-identical -- and
    so third parties can see precisely what they are NOT allowed to reach into.
    """
    return random.Random(":".join((str(seed), str(label), *(str(i) for i in ids))))
