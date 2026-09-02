"""`scms_sim_ref.codecs` -- the built-in `message_codec` implementations.

Importing this module REGISTERS the built-ins on the `message_codec` slot; it does not enable
anything. Activation is a config declaration (`plugins.message_codec.ref`), per decision D2:
*discovery may be automatic, activation is always config-declared*. With no declaration the engine
behaves exactly as it does today and every pinned digest holds -- there is no default codec object,
no new column and no new RNG draw.

**Importing this package never imports `asn1tools`.** The registry entries are classes; the
optional dependency is required only when one of the ETSI classes is CONSTRUCTED. That is what
makes the extra genuinely optional: `import scms_sim_ref.codecs` succeeds, `resolve()` succeeds,
`--list-plugins` lists them, and only `EtsiCamCodec(...)` raises -- at load time, with the install
command in the message.

    native_v1                 the engine's own representation, expressed as a codec (default)
    etsi_cam_en302637_2       CAM, real UPER, ETSI EN 302 637-2 V1.4.1
    etsi_denm_en302637_3      DENM, real UPER, ETSI EN 302 637-3 V1.3.1
    etsi_vam_ts103300_3       VAM, real UPER, ETSI TS 103 300-3 V2.3.1
"""
from __future__ import annotations

from ..api import registry as _registry
from .etsi import (CAM_CARRIED_FIELDS, DENM_CARRIED_FIELDS, VAM_CARRIED_FIELDS,
                   CodecDependencyError, EtsiCamCodec, EtsiDenmCodec, EtsiItsCodec, EtsiVamCodec,
                   MODULE_SETS, SECURITY_ENVELOPE_BYTES, compiled, provenance,
                   require_asn1tools)
from .native import NativeV1Codec

#: Registration (display) order. `native_v1` is FIRST and is the default everywhere, so a codec
#: list rendered in registration order reads as "the engine's own, then the standards profiles".
BUILTIN_CODECS = (
    ("native_v1", NativeV1Codec),
    ("etsi_cam_en302637_2", EtsiCamCodec),
    ("etsi_denm_en302637_3", EtsiDenmCodec),
    ("etsi_vam_ts103300_3", EtsiVamCodec),
)

for _name, _cls in BUILTIN_CODECS:
    _registry.register_builtin("message_codec", _name, _cls)
del _name, _cls

#: The same fixed in-tree mapping the `check` and `fusion` slots keep, and for the same reason:
#: `register_builtin` writes into a process-global dict and OVERWRITES per (slot, name), so any
#: imported distribution could otherwise rebind `native_v1` to its own class and have it resolve as
#: a BUILT-IN. Comparing by identity against this tuple turns that into a named refusal.
BUILTIN_CODEC_BY_NAME = dict(BUILTIN_CODECS)


def builtin_codec(name: str):
    """The registered class for a built-in codec name, or None."""
    return _registry.builtin("message_codec", name)


def is_hijacked(name: str) -> bool:
    """True if the live registry entry for `name` is not the class this package registered."""
    return (name in BUILTIN_CODEC_BY_NAME
            and _registry.builtin("message_codec", name) is not BUILTIN_CODEC_BY_NAME[name])


__all__ = [
    "BUILTIN_CODECS", "BUILTIN_CODEC_BY_NAME", "CAM_CARRIED_FIELDS", "DENM_CARRIED_FIELDS",
    "VAM_CARRIED_FIELDS", "CodecDependencyError", "EtsiCamCodec", "EtsiDenmCodec", "EtsiItsCodec",
    "EtsiVamCodec", "MODULE_SETS", "NativeV1Codec", "SECURITY_ENVELOPE_BYTES", "builtin_codec",
    "compiled", "is_hijacked", "provenance", "require_asn1tools",
]
