"""Errors raised by the plugin API. Stdlib-only, no engine imports.

Every one of these is a LOAD-TIME failure by design (PLUGIN-ARCHITECTURE.md section 3.2): resolution
happens once, before step 0, and every failure is fatal there. A plugin that raises mid-loop
produces a partial dataset whose digest matches nothing -- and it must NOT take the deliberate
SIGINT finalisation path (run.py:~3096-3099), which writes a *valid* manifest for a partial run.
"""
from __future__ import annotations


class ApiError(Exception):
    """Base for every plugin-API failure."""


class ConfigError(ApiError, ValueError):
    """A plugin reference, slot or parameter set that cannot be honoured.

    Subclasses ValueError so `validate_config`'s existing contract (and every test that asserts
    `pytest.raises(ValueError)` around a bad config) keeps holding.
    """


class InterfaceVersionError(ConfigError):
    """The plugin declares an interface version this engine cannot speak."""


class SignatureError(ConfigError):
    """The plugin's method signature does not match the declared interface.

    Neither `Protocol` nor `ABC` checks signatures at runtime -- `@runtime_checkable isinstance`
    tests member PRESENCE only. This is raised by the load-time `inspect.signature` validator that
    does the real work, and it always names the offending parameter.
    """


class CapabilityError(ConfigError):
    """A third party declared a capability reserved for built-ins.

    `legacy_global_rng` and `loss_composition:additive_legacy` exist only so that `disc` and
    `logdistance` keep digest 0bd93655...; they are refused from anything that did not come out of
    the built-in registry (PLUGIN-ARCHITECTURE.md section 4.1).
    """


class PluginDriftError(ApiError):
    """A replayed manifest's plugin lock does not match what is installed now (D4).

    Carries the slot, the reference, and the expected/actual hashes so the operator can tell an
    innocent refactor from a genuine behavioural change (section 4.3).
    """

    def __init__(self, slot: str, ref: str, field: str, expected, actual):
        self.slot, self.ref, self.field = slot, ref, field
        self.expected, self.actual = expected, actual
        super().__init__(
            f"plugin drift in slot {slot!r} ref {ref!r}: {field} expected {expected!r}, "
            f"got {actual!r}. The manifest records what was actually loaded; re-run with "
            f"allow_plugin_drift=True (--allow-plugin-drift) to proceed and RECORD the drift.")
