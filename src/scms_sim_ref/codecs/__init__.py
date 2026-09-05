"""`scms_sim_ref.codecs` -- the built-in PROTOCOL implementations: codecs, profiles and report formats.

Importing this module REGISTERS the built-ins on the `message_codec`, `protocol_profile` and
`report_format` slots; it does not enable anything. Activation is a config declaration
(`plugins.<slot>.ref` or the matching enum field), per decision D2:
*discovery may be automatic, activation is always config-declared*. With no declaration the engine
behaves exactly as it does today and every pinned digest holds -- there is no default codec object,
no new column and no new RNG draw.

**Importing this package never imports `asn1tools`.** The registry entries are classes; the
optional dependency is required only when one of the ETSI classes is CONSTRUCTED. That is what
makes the extra genuinely optional: `import scms_sim_ref.codecs` succeeds, `resolve()` succeeds,
`--list-plugins` lists them, and only `EtsiCamCodec(...)` raises -- at load time, with the install
command in the message.

    message_codec:
      native_v1                 the engine's own representation, expressed as a codec (default)
      etsi_cam_en302637_2       CAM, real UPER, ETSI EN 302 637-2 V1.4.1
      etsi_denm_en302637_3      DENM, real UPER, ETSI EN 302 637-3 V1.3.1
      etsi_vam_ts103300_3       VAM, real UPER, ETSI TS 103 300-3 V2.3.1
    protocol_profile:
      etsi_its_g5               codec + EN 302 637-2 generation + TS 102 687 DCC + 802.11p airtime
    report_format:
      ma_report_v1              the engine's historic misbehaviour-report row
      ts103759_shape            the TS 103 759 TemplateAsr SHAPE, with real v2xPduEvidence octets
"""
from __future__ import annotations

from ..api import registry as _registry
from .etsi import (CAM_CARRIED_FIELDS, DENM_CARRIED_FIELDS, VAM_CARRIED_FIELDS,
                   CodecDependencyError, EtsiCamCodec, EtsiDenmCodec, EtsiItsCodec, EtsiVamCodec,
                   MODULE_SETS, SECURITY_ENVELOPE_BYTES, compiled, provenance,
                   require_asn1tools)
from .native import NativeV1Codec
from .profiles import EtsiItsG5Profile
from .reports import MaReportV1Format, Ts103759ShapeFormat

#: Registration (display) order. `native_v1` is FIRST and is the default everywhere, so a codec
#: list rendered in registration order reads as "the engine's own, then the standards profiles".
BUILTIN_CODECS = (
    ("native_v1", NativeV1Codec),
    ("etsi_cam_en302637_2", EtsiCamCodec),
    ("etsi_denm_en302637_3", EtsiDenmCodec),
    ("etsi_vam_ts103300_3", EtsiVamCodec),
)

#: The shipped protocol profile. ONE entry, and that is the point: `etsi_its_g5` is not the engine's
#: hard-coded behaviour with a name attached, it is a plugin on the `protocol_profile` slot that the
#: engine resolves exactly as it resolves a third party's.
BUILTIN_PROFILES = (
    ("etsi_its_g5", EtsiItsG5Profile),
)

#: The shipped report formats. `ma_report_v1` is the engine's historic row; `ts103759_shape` is the
#: same content in the TS 103 759 `TemplateAsr` shape, carrying the real evidence octets.
BUILTIN_REPORT_FORMATS = (
    ("ma_report_v1", MaReportV1Format),
    ("ts103759_shape", Ts103759ShapeFormat),
)

#: slot -> the shipped (name, class) tuple for that slot. One loop registers all three, and one
#: mapping backs the name-hijack refusal for all three.
BUILTINS_BY_SLOT = {
    "message_codec": BUILTIN_CODECS,
    "protocol_profile": BUILTIN_PROFILES,
    "report_format": BUILTIN_REPORT_FORMATS,
}

for _slot, _entries in BUILTINS_BY_SLOT.items():
    for _name, _cls in _entries:
        _registry.register_builtin(_slot, _name, _cls)
del _slot, _entries, _name, _cls

#: The same fixed in-tree mapping the `check` and `fusion` slots keep, and for the same reason:
#: `register_builtin` writes into a process-global dict and OVERWRITES per (slot, name), so any
#: imported distribution could otherwise rebind `native_v1` -- or `etsi_its_g5`, or `ma_report_v1` --
#: to its own class and have it resolve as a BUILT-IN, inheriting the built-in exemption from the
#: source gate, from outcome validation and from the conformance requirement. Comparing by identity
#: against these tuples turns that into a named refusal.
BUILTIN_CODEC_BY_NAME = dict(BUILTIN_CODECS)
BUILTIN_BY_SLOT_NAME = {slot: dict(entries) for slot, entries in BUILTINS_BY_SLOT.items()}


def builtin_codec(name: str):
    """The registered class for a built-in codec name, or None."""
    return _registry.builtin("message_codec", name)


def shipped(slot: str, name: str):
    """The class THIS PACKAGE registered under `(slot, name)`, or None if it ships no such name."""
    return (BUILTIN_BY_SLOT_NAME.get(slot) or {}).get(name)


def is_hijacked(name: str, slot: str = "message_codec") -> bool:
    """True if the live registry entry for `(slot, name)` is not the class this package registered.

    `slot` defaults to `message_codec` so every existing caller keeps its meaning; the `codecs`
    package now owns three slots and the check is identical on all of them.
    """
    own = shipped(slot, name)
    return own is not None and _registry.builtin(slot, name) is not own


__all__ = [
    "BUILTINS_BY_SLOT", "BUILTIN_BY_SLOT_NAME", "BUILTIN_CODECS", "BUILTIN_CODEC_BY_NAME",
    "BUILTIN_PROFILES", "BUILTIN_REPORT_FORMATS", "CAM_CARRIED_FIELDS", "DENM_CARRIED_FIELDS",
    "VAM_CARRIED_FIELDS", "CodecDependencyError", "EtsiCamCodec", "EtsiDenmCodec", "EtsiItsCodec",
    "EtsiItsG5Profile", "EtsiVamCodec", "MODULE_SETS", "MaReportV1Format", "NativeV1Codec",
    "SECURITY_ENVELOPE_BYTES", "Ts103759ShapeFormat", "builtin_codec",
    "compiled", "is_hijacked", "provenance", "require_asn1tools", "shipped",
]
