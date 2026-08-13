"""Build-breaking leakage linter.

The core scientific-validity guarantee of this project: **no ground-truth field
ever reaches a feature table.** This linter is run in CI over every MA-visible
output and every feature frame. Any forbidden key -- a real vehicle id, a true
kinematic value, an attack label, or the exact fields F2MD leaked
(`senderRealId`/`reportedRealId`) -- fails the build.

It is deliberately conservative: it inspects keys recursively through nested
dicts and lists, and treats anything matching `is_forbidden_feature_key` as a
violation regardless of nesting depth.
"""

from __future__ import annotations

from typing import Any

from ..schemas.records import ORACLE, is_forbidden_feature_key


class LeakageViolation(AssertionError):
    """Raised when ground-truth data is found in an MA-visible / feature record."""


def find_forbidden_keys(obj: Any, path: str = "") -> list[str]:
    """Return dotted paths of every forbidden key found anywhere in `obj`."""
    hits: list[str] = []
    if isinstance(obj, dict):
        for k, v in obj.items():
            here = f"{path}.{k}" if path else str(k)
            # Check EVERY key, including `_`-prefixed leaves. The predicate strips a
            # leading underscore before matching, so the legitimate `_visibility`
            # metadata field still passes while a disguised `_is_attacker` is caught.
            if isinstance(k, str) and is_forbidden_feature_key(k):
                hits.append(here)
            hits.extend(find_forbidden_keys(v, here))
    elif isinstance(obj, (list, tuple)):
        for idx, v in enumerate(obj):
            hits.extend(find_forbidden_keys(v, f"{path}[{idx}]"))
    return hits


def _as_dict(record: Any) -> dict:
    if hasattr(record, "to_dict"):
        return record.to_dict()
    if isinstance(record, dict):
        return record
    raise TypeError(f"cannot lint object of type {type(record)!r}")


def assert_ma_visible(record: Any, context: str = "") -> None:
    """Assert a single record is safe to expose to the MA / feature pipeline."""
    d = _as_dict(record)
    if d.get("_visibility") == ORACLE:
        raise LeakageViolation(
            f"{context or 'record'} is ORACLE-visibility and must not enter MA-visible data"
        )
    hits = find_forbidden_keys(d)
    if hits:
        raise LeakageViolation(
            f"{context or 'record'} leaks ground-truth field(s): {', '.join(sorted(hits))}"
        )


def lint_ma_dataset(records: list[Any], context: str = "ma_dataset") -> int:
    """Lint a whole MA-visible dataset. Returns the count checked; raises on any leak."""
    for i, rec in enumerate(records):
        assert_ma_visible(rec, context=f"{context}[{i}]")
    return len(records)


def lint_feature_frame(rows: list[dict], context: str = "feature_frame") -> int:
    """Lint ML-ready feature rows (plain dicts). Raises on any forbidden column."""
    for i, row in enumerate(rows):
        hits = find_forbidden_keys(row)
        if hits:
            raise LeakageViolation(
                f"{context}[{i}] contains forbidden feature column(s): {', '.join(sorted(hits))}"
            )
    return len(rows)
