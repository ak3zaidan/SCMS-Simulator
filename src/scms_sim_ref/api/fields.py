"""`FieldSpec` -- how a plugin declares its own config knobs.

The insight is ns-3's `GetTypeId().AddAttribute(name, help, initial value, accessor, checker)`:
one declaration makes a parameter documented, CLI-settable and serialisable. This repo already has
that machinery -- `config_schema()` (run.py:1869) emits
`{type, default, group, widget, help, options, min, max, step, unit}` for every field and feeds the
GUI `/api/schema`, the copilot cheat-sheet, `--dump-config-schema` and the manifest with zero code
per field. Its only defect is that it iterates `dataclasses.fields(PipelineConfig)`, a set closed at
class-definition time.

`FieldSpec` is therefore DELIBERATELY the exact shape `config_schema()` already emits, so merging a
plugin's declarations into the schema is a dict update and nothing else.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Optional


@dataclass(frozen=True, slots=True)
class FieldSpec:
    """One plugin-declared config knob.

    `type` is the python type NAME ("float", "int", "bool", "str"); `default` must be JSON-safe.
    `lo`/`hi`/`step`/`unit`/`options`/`group` mirror `_FIELD_META`'s lo/hi/st/u + `_ENUM_OPTIONS`.
    """
    type: str
    default: object
    help: str = ""
    lo: Optional[float] = None
    hi: Optional[float] = None
    step: Optional[float] = None
    unit: Optional[str] = None
    options: Optional[tuple] = None
    group: Optional[str] = None

    def widget(self) -> str:
        """The widget name `config_schema()` would pick for this field."""
        if isinstance(self.default, bool) or self.type == "bool":
            return "bool"
        if self.options:
            return "select"
        if self.type.startswith("int"):
            return "int"
        if self.type.startswith("float"):
            return "float"
        return "text"

    def to_schema(self, group: Optional[str] = None) -> dict:
        """Render into exactly the per-field dict `config_schema()` emits."""
        return {"type": self.type, "default": self.default,
                "group": self.group or group or "Plugins", "widget": self.widget(),
                "help": self.help, "options": list(self.options) if self.options else None,
                "min": self.lo, "max": self.hi, "step": self.step, "unit": self.unit}

    def validate(self, name: str, value):
        """Range/enum check for one supplied value. Returns the value; raises ConfigError."""
        from .errors import ConfigError
        if self.options is not None and value not in self.options:
            raise ConfigError(f"{name}={value!r} not in {list(self.options)}")
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return value
        if self.lo is not None and value < self.lo:
            raise ConfigError(f"{name}={value} below minimum {self.lo}")
        if self.hi is not None and value > self.hi:
            raise ConfigError(f"{name}={value} above maximum {self.hi}")
        return value
