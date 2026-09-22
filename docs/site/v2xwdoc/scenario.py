"""The scenario schema reference, extracted from the engine's own Rust types.

`crates/v2xw-engine/src/scenario/schema.rs` *is* the schema: the loader deserialises
into those structs, so a field that is not there cannot be written in a scenario file
and a default that is not there is not the default. This module reads that file and
renders every field, its type, its unit and its default, rather than restating them in
prose that would quietly fall behind.

The extraction is deliberately shallow -- a line reader, not a Rust parser. It
understands the shapes the schema file actually uses (doc comments, `#[serde]`
attributes, plain `pub name: Type` fields, `impl Default`, and `default_*` helper
functions returning one literal). Anything it cannot resolve is printed verbatim as
source text and marked as such, which is the honest failure mode: a reader sees the
expression the engine evaluates instead of a confident wrong value.
"""

import os
import re

from . import render
from .render import escape

__all__ = ["extract", "page"]

_STRUCT_RE = re.compile(r"^pub struct (\w+) \{")
_ENUM_RE = re.compile(r"^pub enum (\w+) \{")
_IMPL_RE = re.compile(r"^impl (?:Default for )?(\w+) \{")
_FIELD_RE = re.compile(r"^\s*pub (\w+): (.+?),?\s*$")
_FN_RE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?fn (\w+)\(\)\s*->\s*(.+?)\s*\{\s*$"
)
_VARIANT_RE = re.compile(r"^\s{4}([A-Z]\w*)\s*(\{|\(|,|$)")
_SERDE_DEFAULT_FN = re.compile(r'default\s*=\s*"([^"]+)"')
_SERDE_RENAME = re.compile(r'rename\s*=\s*"([^"]+)"')
_SERDE_RENAME_ALL = re.compile(r'rename_all\s*=\s*"([^"]+)"')

# Unit conventions: the schema module states that a field carries its unit in its name,
# so the unit column is derived from the suffix rather than guessed from the doc text.
_UNIT_SUFFIXES = [
    ("_veh_per_h", "vehicles/hour"),
    ("_per_h", "per hour"),
    ("_per_s", "per second"),
    ("_ms", "milliseconds"),
    ("_us", "microseconds"),
    ("_ns", "nanoseconds"),
    ("_s", "seconds"),
    ("_m", "metres"),
    ("_km", "kilometres"),
    ("_m2", "square metres"),
    ("_dbm", "dBm"),
    ("_dbi", "dBi"),
    ("_db", "dB"),
    ("_hz", "hertz"),
    ("_mhz", "megahertz"),
    ("_ghz", "gigahertz"),
    ("_deg", "degrees"),
    ("_rad", "radians"),
    ("_mps", "metres/second"),
    ("_kph", "kilometres/hour"),
    ("_bytes", "bytes"),
    ("_fraction", "fraction 0..1"),
    ("_pct", "per cent"),
]


def unit_of(name):
    """The unit a field name declares, or an empty string."""
    lowered = name.lower()
    for suffix, unit in _UNIT_SUFFIXES:
        if lowered.endswith(suffix):
            return unit
    if lowered.startswith("min_") or lowered.startswith("max_"):
        return unit_of(lowered[4:])
    return ""


def kebab(name):
    """CamelCase to the kebab-case spelling `serde(rename_all)` produces."""
    out = []
    for index, char in enumerate(name):
        if char.isupper() and index > 0 and not name[index - 1].isupper():
            out.append("-")
        out.append(char.lower())
    return "".join(out)


class Field:
    def __init__(self, name, ty, doc, attrs):
        self.name = name
        self.ty = ty
        self.doc = doc
        self.attrs = attrs

    @property
    def optional(self):
        return self.ty.startswith("Option<")

    @property
    def inner_ty(self):
        ty = self.ty
        for wrapper in ("Option<", "Vec<", "Box<"):
            while ty.startswith(wrapper):
                ty = ty[len(wrapper) : -1]
        if ty.startswith("BTreeMap<") or ty.startswith("HashMap<"):
            ty = ty[ty.index("<") + 1 : -1].split(",")[-1].strip()
        return ty

    def serde(self):
        return " ".join(a for a in self.attrs if a.startswith("#[serde"))


class Struct:
    def __init__(self, name, doc, attrs):
        self.name = name
        self.doc = doc
        self.attrs = attrs
        self.fields = []


class Enum:
    def __init__(self, name, doc, attrs):
        self.name = name
        self.doc = doc
        self.attrs = attrs
        self.variants = []  # (wire name, doc, kind)


class Schema:
    def __init__(self):
        self.structs = {}
        self.enums = {}
        self.default_fns = {}  # "Type::fn" -> expression
        self.default_impls = {}  # Type -> {field: expression}
        self.derived_default = set()
        self.version = ""


def _flush_docs(docs):
    return " ".join(d.strip() for d in docs).strip()


def extract(path):
    """Read `schema.rs` and return a `Schema`."""
    with open(path, "r", encoding="utf-8") as handle:
        lines = handle.read().split("\n")
    schema = Schema()
    docs = []
    attrs = []
    index = 0
    impl_stack = []
    while index < len(lines):
        line = lines[index]
        stripped = line.strip()
        version = re.match(r'^pub const CURRENT_SCHEMA: &str = "([^"]+)";', stripped)
        if version:
            schema.version = version.group(1)
        if stripped.startswith("///") or stripped.startswith("//!"):
            docs.append(stripped.lstrip("/!").strip())
            index += 1
            continue
        if stripped.startswith("#["):
            attrs.append(stripped)
            index += 1
            continue
        struct = _STRUCT_RE.match(line)
        if struct:
            index = _read_struct(schema, lines, index, struct.group(1), docs, attrs)
            docs, attrs = [], []
            continue
        enum = _ENUM_RE.match(line)
        if enum:
            index = _read_enum(schema, lines, index, enum.group(1), docs, attrs)
            docs, attrs = [], []
            continue
        function = _FN_RE.match(line)
        if function and not line.startswith(" "):
            # A module-level helper, which is what `#[serde(default = "…::truth")]`
            # points at. Recorded under its bare name; `_resolve_expr` tries the full
            # path, then the last two segments, then the name.
            body, index = _read_fn_body(lines, index + 1)
            if len(body) == 1:
                schema.default_fns[function.group(1)] = body[0]
            docs, attrs = [], []
            continue
        impl = _IMPL_RE.match(line)
        if impl:
            index = _read_impl(
                schema, lines, index, impl.group(1), line.startswith("impl Default")
            )
            docs, attrs = [], []
            continue
        if stripped:
            docs, attrs = [], []
        index += 1
    return schema


def _read_struct(schema, lines, index, name, docs, attrs):
    struct = Struct(name, _flush_docs(docs), list(attrs))
    if any("derive(" in a and "Default" in a for a in attrs):
        schema.derived_default.add(name)
    schema.structs[name] = struct
    index += 1
    field_docs = []
    field_attrs = []
    while index < len(lines) and lines[index] != "}":
        line = lines[index]
        stripped = line.strip()
        if stripped.startswith("///"):
            field_docs.append(stripped.lstrip("/").strip())
        elif stripped.startswith("#["):
            field_attrs.append(stripped)
        else:
            field = _FIELD_RE.match(line)
            if field:
                struct.fields.append(
                    Field(
                        field.group(1),
                        field.group(2),
                        _flush_docs(field_docs),
                        list(field_attrs),
                    )
                )
            if stripped:
                field_docs, field_attrs = [], []
        index += 1
    return index + 1


def _read_enum(schema, lines, index, name, docs, attrs):
    enum = Enum(name, _flush_docs(docs), list(attrs))
    rename_all = ""
    for attr in attrs:
        found = _SERDE_RENAME_ALL.search(attr)
        if found:
            rename_all = found.group(1)
    schema.enums[name] = enum
    index += 1
    variant_docs = []
    variant_attrs = []
    depth = 0
    while index < len(lines) and lines[index] != "}":
        line = lines[index]
        stripped = line.strip()
        if depth > 0:
            depth += line.count("{") - line.count("}")
            index += 1
            continue
        if stripped.startswith("///"):
            variant_docs.append(stripped.lstrip("/").strip())
        elif stripped.startswith("#["):
            variant_attrs.append(stripped)
        else:
            variant = _VARIANT_RE.match(line)
            if variant:
                wire = variant.group(1)
                explicit = ""
                for attr in variant_attrs:
                    found = _SERDE_RENAME.search(attr)
                    if found:
                        explicit = found.group(1)
                if explicit:
                    wire = explicit
                elif rename_all == "kebab-case":
                    wire = kebab(wire)
                elif rename_all == "lowercase":
                    wire = wire.lower()
                enum.variants.append(
                    (
                        wire,
                        _flush_docs(variant_docs),
                        "fields" if variant.group(2) == "{" else "unit",
                        any("#[default]" in a for a in variant_attrs),
                    )
                )
                depth += line.count("{") - line.count("}")
            if stripped:
                variant_docs, variant_attrs = [], []
        index += 1
    return index + 1


def _read_impl(schema, lines, index, name, is_default_impl):
    index += 1
    while index < len(lines) and lines[index] != "}":
        line = lines[index]
        function = _FN_RE.match(line)
        if function and function.group(1) != "default":
            body, index = _read_fn_body(lines, index + 1)
            if len(body) == 1:
                schema.default_fns[name + "::" + function.group(1)] = body[0]
            continue
        if is_default_impl and re.match(r"^\s*fn default\(\) -> Self \{", line):
            index = _read_default_body(schema, lines, index + 1, name)
            continue
        index += 1
    return index + 1


def _read_fn_body(lines, index):
    body = []
    depth = 1
    while index < len(lines):
        line = lines[index]
        depth += line.count("{") - line.count("}")
        if depth <= 0:
            return body, index + 1
        if line.strip():
            body.append(line.strip())
        index += 1
    return body, index


def _read_default_body(schema, lines, index, name):
    fields = {}
    depth = 1
    pending = ""
    while index < len(lines):
        line = lines[index]
        depth += line.count("{") - line.count("}")
        if depth <= 0:
            break
        stripped = line.strip()
        field = re.match(r"^(\w+):\s*(.+?),?$", stripped)
        if field and not stripped.startswith("//"):
            fields[field.group(1)] = field.group(2).rstrip(",")
        index += 1
        pending = stripped
    schema.default_impls[name] = fields
    return index + 1


_LITERAL_MAP = {
    "Vec::new()": "[] (empty)",
    "vec![]": "[] (empty)",
    "BTreeMap::new()": "{} (empty)",
    "None": "null (absent)",
    "serde_json::Value::Null": "null",
    "String::new()": '"" (empty)',
    "Default::default()": "the type's own defaults",
}


def resolve_default(schema, owner, field):
    """The default a field takes, as `(text, resolved)`.

    `resolved` is False when the extractor could not reduce the expression to a value,
    in which case `text` is the Rust source and the page says so.
    """
    serde = field.serde()
    named = _SERDE_DEFAULT_FN.search(serde)
    if named:
        return _resolve_expr(schema, named.group(1) + "()")
    if "skip_serializing_if" in serde and field.optional:
        return ("null (absent)", True)
    if "default" in serde:
        impl = schema.default_impls.get(owner)
        if impl and field.name in impl:
            return _resolve_expr(schema, impl[field.name])
        return ("the field type's own default", True)
    return ("required", True)


def _resolve_expr(schema, expr, depth=0):
    expr = expr.strip().rstrip(",")
    if depth > 3:
        return (expr, False)
    if expr in _LITERAL_MAP:
        return (_LITERAL_MAP[expr], True)
    literal = re.match(r'^"(.*)"\.to_string\(\)$|^"(.*)"$', expr)
    if literal:
        value = literal.group(1) if literal.group(1) is not None else literal.group(2)
        return ('"' + value + '"', True)
    if re.match(r"^-?\d[\d_]*(\.\d+)?(e-?\d+)?(f64|f32|u\d+|i\d+|usize)?$", expr):
        return (expr, True)
    if expr in ("true", "false"):
        return (expr, True)
    call = re.match(r"^([\w:]+)\(\)$", expr)
    if call:
        path = call.group(1).split("::")
        candidates = [call.group(1), path[-1]]
        if len(path) >= 2:
            candidates.insert(1, path[-2] + "::" + path[-1])
        for key in candidates:
            if key in schema.default_fns:
                return _resolve_expr(schema, schema.default_fns[key], depth + 1)
        if path[-1] == "default":
            return ("the type's own defaults", True)
    variant = re.match(r"^(\w+)::(\w+)$", expr)
    if variant:
        enum = schema.enums.get(variant.group(1))
        if enum:
            for wire, _doc, _kind, _is_default in enum.variants:
                if wire == kebab(variant.group(2)) or wire == variant.group(2).lower():
                    return (wire, True)
        return (kebab(variant.group(2)), True)
    if expr.startswith("Some("):
        inner, ok = _resolve_expr(schema, expr[5:-1], depth + 1)
        return (inner, ok)
    return (expr, False)


# --- the page -----------------------------------------------------------------------


def _type_html(schema, field):
    inner = field.inner_ty
    if inner in schema.structs:
        return (
            '<a href="#t-'
            + inner.lower()
            + '"><code>'
            + escape(field.ty)
            + "</code></a>"
        )
    if inner in schema.enums:
        return (
            '<a href="#e-'
            + inner.lower()
            + '"><code>'
            + escape(field.ty)
            + "</code></a>"
        )
    return "<code>" + escape(field.ty) + "</code>"


def _walk(schema, name, path, seen, out):
    struct = schema.structs.get(name)
    if struct is None or name in seen:
        return
    seen.add(name)
    rows = []
    children = []
    for field in struct.fields:
        default, resolved = resolve_default(schema, name, field)
        default_html = "<code>" + escape(default) + "</code>"
        if not resolved:
            default_html = (
                "<code>"
                + escape(default)
                + '</code> <small class="dim">(source expression; the generator could '
                "not reduce it to a value)</small>"
            )
        elif default == "required":
            default_html = render.badge("todo", "required")
        rows.append(
            [
                "<code>" + escape((path + "." if path else "") + field.name) + "</code>",
                _type_html(schema, field),
                escape(unit_of(field.name)) or '<span class="dim">—</span>',
                default_html,
                escape(field.doc),
            ]
        )
        if field.inner_ty in schema.structs:
            children.append(
                (field.inner_ty, (path + "." if path else "") + field.name)
            )
    out.append(
        '<h3 id="t-'
        + name.lower()
        + '"><code>'
        + escape(path or "(root)")
        + "</code> — <code>"
        + escape(name)
        + '</code><a class="anchor" href="#t-'
        + name.lower()
        + '">#</a></h3>'
    )
    if struct.doc:
        out.append("<p>" + escape(struct.doc) + "</p>")
    out.append(
        render.table(
            ["Field", "Type", "Unit", "Default", "What it means"], rows, "schema"
        )
    )
    for child_name, child_path in children:
        _walk(schema, child_name, child_path, seen, out)


def page(schema, source_rel):
    """The whole schema reference: the field tree, then the enumerations."""
    parts = []
    parts.append(
        "<p>The schema version this build writes and loads is <code>"
        + escape(schema.version or "unknown")
        + "</code>. A file states it in its top-level <code>schema:</code> key; the "
        "loader migrates older versions by that string and nothing else.</p>"
    )
    parts.append(
        '<div class="callout"><p><strong>Units live in field names.</strong> '
        "<code>duration_s</code>, <code>mobility_step_ms</code>, "
        "<code>rate_veh_per_h</code>: the unit column below is read off the name, "
        "because a name is the one place a unit cannot be separated from its value. A "
        "field with no unit suffix is dimensionless — a count, an id, a fraction or a "
        "choice.</p></div>"
    )
    parts.append(
        '<div class="callout"><p><strong>Lenient to parse, strict to validate.</strong> '
        "Most fields have a default, so a short file is a valid file; the scenario "
        "validator then enforces the cross-field rules (tier combinations, fractions "
        "summing to one, a model id that the registry knows). A default in this table "
        "is what you get when you omit the field, not a promise that the combination "
        "you end up with is accepted.</p></div>"
    )
    seen = set()
    body = []
    _walk(schema, "Scenario", "", seen, body)
    parts.append('<h2 id="fields">Fields<a class="anchor" href="#fields">#</a></h2>')
    parts.extend(body)

    parts.append(
        '<h2 id="enums">Enumerations<a class="anchor" href="#enums">#</a></h2>'
    )
    parts.append(
        "<p>The spellings below are the ones a scenario file writes — the wire names "
        "after <code>serde</code> renaming, not the Rust identifiers.</p>"
    )
    for name in sorted(schema.enums):
        enum = schema.enums[name]
        if not enum.variants:
            continue
        parts.append(
            '<h3 id="e-'
            + name.lower()
            + '"><code>'
            + escape(name)
            + '</code><a class="anchor" href="#e-'
            + name.lower()
            + '">#</a></h3>'
        )
        if enum.doc:
            parts.append("<p>" + escape(enum.doc) + "</p>")
        rows = []
        for wire, doc, kind, is_default in enum.variants:
            label = "<code>" + escape(wire) + "</code>"
            if is_default:
                label += " " + render.badge("tier", "default")
            if kind == "fields":
                label += ' <small class="dim">(carries fields)</small>'
            rows.append([label, escape(doc)])
        parts.append(render.table(["Value", "Meaning"], rows, "enums"))

    unresolved = []
    for name, struct in sorted(schema.structs.items()):
        for field in struct.fields:
            text, resolved = resolve_default(schema, name, field)
            if not resolved:
                unresolved.append(
                    [
                        "<code>" + escape(name + "." + field.name) + "</code>",
                        "<code>" + escape(text) + "</code>",
                    ]
                )
    if unresolved:
        parts.append(
            '<h2 id="unresolved">Defaults this page could not reduce'
            '<a class="anchor" href="#unresolved">#</a></h2>'
        )
        parts.append(
            "<p>The extractor reads Rust as text, not as a compiler would. For these "
            "fields it found the expression but not a value, so the expression is "
            "shown verbatim in the table above and repeated here. Read <code>"
            + escape(source_rel)
            + "</code> for the real answer.</p>"
        )
        parts.append(render.table(["Field", "Expression"], unresolved))
    return "".join(parts)
