"""Regression guard: every committed dataset must pass the full data-correctness audit.

This runs the same invariant checks as `tools/verify_data.py` (leakage/privacy,
referential integrity, label correctness, count reconciliation, split integrity,
value sanity, and per-file digest integrity) over every dataset under `datasets/`.
Any FAIL fails the build; SKIPs (a check not applicable to an older dataset) are fine.
"""
from __future__ import annotations

import importlib.util
from collections import Counter
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DATASETS = ROOT / "datasets"

_spec = importlib.util.spec_from_file_location("verify_data", ROOT / "tools" / "verify_data.py")
verify_data = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(verify_data)


@pytest.mark.skipif(not DATASETS.exists(), reason="no datasets/ directory")
def test_all_datasets_pass_integrity_audit():
    results = verify_data.run_audit(DATASETS)
    assert results, "audit produced no results — datasets/ empty or unreadable"
    fails = [r for r in results if r[2] == "FAIL"]
    by_status = Counter(r[2] for r in results)
    msg = "\n".join(f"[{ds}] {check}: {detail}" for ds, check, _, detail in fails)
    assert not fails, f"{by_status['FAIL']} data-integrity failures:\n{msg}"
