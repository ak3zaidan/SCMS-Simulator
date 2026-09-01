"""`scms_sim_ref.api` -- the published, dependency-free plugin contract.

Stdlib only, and deliberately so: it is intended to split out later as a separate distribution
(`scms-sim-api`) so a plugin author's install closure does not pull the whole engine. That is what
makes "no fork" true in practice rather than in principle.

What phase 1 ships (PLUGIN-ARCHITECTURE.md section 7):

* :mod:`~scms_sim_ref.api.channel` -- the channel message vocabulary (`StationSnapshot`,
  `Transmission`, `StepFrame`, `LinkOutcome`), the `BatchChannelModel` / `LinkChannelModel`
  Protocols, and `PerLinkAdapter`.
* :mod:`~scms_sim_ref.api.detect` (phase 3) -- the detector contract: the frozen, MA-visible-only
  `Observation`, the `Check` / `Fusion` Protocols and their convenience bases, `ReportDecision`, and
  `NamespacedState`. **Polarity: a score `>= 1.0` is VIOLATING** -- the opposite of F2MD's
  `[0,1]` LOW-means-implausible convention, and the single most common porting mistake.
* :mod:`~scms_sim_ref.api.rng` -- `RngNamespace`. A plugin NEVER receives the engine's global
  `random.Random(cfg.seed)`; capability by omission is the only enforceable control (D3).
* :mod:`~scms_sim_ref.api.fields` -- `FieldSpec`, deliberately the exact shape `config_schema()`
  already emits.
* :mod:`~scms_sim_ref.api.registry` -- the three-tier resolver and the content-hash provenance lock.
* :mod:`~scms_sim_ref.api.errors` -- every failure is a LOAD-TIME failure.

Safety posture, stated honestly (section 10): in-process plugins are **attested and detected, not
sandboxed**. Capability by omission is the only strong control. Arbitrary code execution, native
nondeterminism, entropy/clock access and resource limits are NOT enforceable in-process at all --
and Windows has no `RLIMIT` equivalent. Real isolation requires leaving the process, which is
affordable only because the ABI is already batched per step.
"""
from __future__ import annotations

from .channel import (BatchChannelModel, DELIVERED, INTERFACE_VERSION, LinkChannelModel,
                      LinkChannelModelBase, LinkOutcome, LINK_STATES, PerLinkAdapter,
                      StationSnapshot, StepFrame, Transmission, UNBOUND)
from .detect import (Check, CheckBase, Fusion, FusionBase, NamespacedState, Observation,
                     ReportDecision, VIOLATION_THRESHOLD, fires, namespaced_key)
from .errors import (ApiError, CapabilityError, ConfigError, InterfaceVersionError,
                     PluginDriftError, SignatureError)
from .fields import FieldSpec
from .registry import API_VERSION, SLOTS, builtin_names, builtin_names_sorted, resolve
from .rng import RngNamespace

__all__ = [
    "API_VERSION", "INTERFACE_VERSION", "SLOTS", "UNBOUND", "VIOLATION_THRESHOLD",
    "ApiError", "BatchChannelModel", "CapabilityError", "Check", "CheckBase", "ConfigError",
    "DELIVERED", "FieldSpec", "Fusion", "FusionBase", "InterfaceVersionError", "LINK_STATES",
    "LinkChannelModel", "LinkChannelModelBase", "LinkOutcome", "NamespacedState", "Observation",
    "PerLinkAdapter", "PluginDriftError", "ReportDecision", "RngNamespace", "SignatureError",
    "StationSnapshot", "StepFrame", "Transmission",
    "builtin_names", "builtin_names_sorted", "fires", "namespaced_key", "resolve",
]
