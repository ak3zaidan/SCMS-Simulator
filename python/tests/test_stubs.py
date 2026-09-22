"""The typed stubs describe the module that exists.

The stubs in ``v2xw/_v2xw.pyi`` are hand-written, because there is no generator that turns
pyo3's macro output into them. A hand-written description of a compiled surface drifts, and
the drift is invisible until someone's editor autocompletes a method that was renamed. This
test compares the two directions that matter:

* every name the stubs declare exists on the module — otherwise the stubs promise something
  that is not there;
* every public name on the module is declared — otherwise the stubs are silently incomplete
  and a type checker calls a real method an error.
"""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

import v2xw
from v2xw import _v2xw as native

STUB = Path(v2xw.__file__).parent / "_v2xw.pyi"


def stub_tree() -> ast.Module:
    if not STUB.exists():  # pragma: no cover - the file is shipped in the package
        pytest.skip(f"no stub file at {STUB}")
    return ast.parse(STUB.read_text())


def declared_top_level() -> set[str]:
    names: set[str] = set()
    for node in stub_tree().body:
        if isinstance(node, (ast.ClassDef, ast.FunctionDef)):
            names.add(node.name)
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            names.add(node.target.id)
    return names


def declared_members(class_name: str) -> set[str]:
    for node in stub_tree().body:
        if isinstance(node, ast.ClassDef) and node.name == class_name:
            out: set[str] = set()
            for item in node.body:
                if isinstance(item, (ast.FunctionDef, ast.ClassDef)):
                    out.add(item.name)
                elif isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
                    out.add(item.target.id)
            return out
    raise AssertionError(f"the stubs declare no class {class_name}")


def public(obj: object) -> set[str]:
    return {n for n in dir(obj) if not n.startswith("_")}


def test_every_stubbed_top_level_name_exists():
    missing = sorted(n for n in declared_top_level() if not hasattr(native, n))
    assert not missing, f"the stubs promise names the module does not have: {missing}"


def test_every_public_module_name_is_stubbed():
    extra = sorted(public(native) - declared_top_level())
    assert not extra, f"the module has public names the stubs do not declare: {extra}"


@pytest.mark.parametrize(
    "name", ["Scenario", "Metrics", "Run", "Recording", "math", "plugins", "_conformance"]
)
def test_every_public_member_is_stubbed(name):
    obj = getattr(native, name)
    declared = declared_members(name)
    # `property` shows up as a plain attribute on the class, and a stub declares it as a
    # decorated method of the same name, so the two sets line up without special handling.
    extra = sorted(public(obj) - declared)
    assert not extra, f"{name} has public members the stubs do not declare: {extra}"
    missing = sorted(n for n in declared if not hasattr(obj, n))
    assert not missing, f"the stubs promise {name} members that do not exist: {missing}"
