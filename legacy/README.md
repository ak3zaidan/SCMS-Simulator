# `legacy/` — the frozen Python reference (ADR 0002)

This tree is the original Python implementation of the SCMS simulator
(`scms_sim_ref`, ~10k lines) together with its tests, GUI and tools. It is
**frozen**: it is no longer developed, and no new feature lands here.

Per [ADR 0002](../docs/adr/0002-supersede-base-stack-new-rust-core.md) it stays
in the repository for two reasons:

1. **It is the validated reference.** Every formula and constant the new Rust
   engine implements is ported from this code, and each model card cites the
   file and line range it came from (`kind: code`, see
   [`docs/design/01-inventory.md`](../docs/design/01-inventory.md) §3.3).
2. **Its engine-independent tests are the conformance suite** the new engine
   must reproduce. The fixed golden digests of the old implementation are
   retired; the behavioural vectors are not.

## The conformance vectors

| Suite | File | What the new engine must reproduce |
|---|---|---|
| Butterfly key expansion | `tests/test_butterfly.py` | certificate batch derivation and expansion values |
| Linkage values | `tests/test_linkage.py` | `la1`/`la2` chains, linkage seeds, HashedId8 |
| Leakage rules | `tests/test_leakage.py` | no ground-truth or oracle column reaches a feature set |
| ML contract | `tests/test_ml_contract.py` | feature schema, `detnorm ≈ 1` at threshold, byte-identical featurization across runs |
| Dataset integrity | `tests/test_dataset_integrity.py` | record schema, invariants, recursive dataset scan |

These five run in CI on every push (`.github/workflows/ci.yml`, job
`legacy-conformance`); the rest of the suite is kept for reference and runs
locally.

## Running it

The package is a normal editable install; `conftest.py` also puts this
directory on `sys.path` so the tests import `scms_sim_ref` without one.

```bash
# from the repository root — creates legacy/.venv and installs the package
just legacy-setup

# the five conformance suites
just legacy-conformance

# everything
just legacy-test
```

Without `just`:

```bash
uv venv --python 3.12 legacy/.venv
uv pip install --python legacy/.venv -e "./legacy[test]"
cd legacy && .venv/bin/python -m pytest tests -q
```

The test extras pull `pytest`, `numpy`, `pandas` and `pyarrow` (the dataset and
ML-contract suites read and write Parquet). Python 3.11+ is required.

### What to expect

- `just legacy-conformance` — **38 passed, 1 skipped** (~12 s). This is the gate.
- `just legacy-test` — **437 passed, 26 failed, 4 skipped** (~11 min on an M-series
  laptop). Every one of the 26 failures is a `*_golden_*` / byte-identical
  **fixed digest** of the old implementation (`test_attack_magnitude`,
  `test_combined_attacks`, `test_config_knobs`, `test_denm`, `test_engine_truth`,
  `test_evasive_attackers`, `test_gap_acceptance`, `test_lane_changes`,
  `test_network_fidelity`, `test_radio_propagation`, `test_vru`,
  `test_vru_denm_harden`, `test_vru_spoofing`). They were pinned on the original
  Windows/Python environment and already failed on this machine before the
  Phase 0 restructure (verified against commit `b6183cd`). ADR 0002 §2 retires
  those digests; the new golden suite re-pins them under the determinism rules
  of ADR 0004.
- The GUI modules (`test_gui_backend`, `test_gui_curated_knobs`, `test_gui_parity`,
  `test_gui_realism`, `test_gui_cli_contract`, and one case in
  `test_audit_w10_followups`) are skipped by `addopts` in `pyproject.toml`: they
  import `gui/server.py`, which loads `mapgen` from the deleted
  `scms-sim/scenarios/` tooling. They are replaced by API-contract tests of the
  JSON-RPC surface (`01-inventory.md` §4).

## Layout

| Path | Contents |
|---|---|
| `scms_sim_ref/` | the frozen package: `scms_core` (butterfly, linkage, HashedId8), `datagen`, `mock_pipeline`, `schemas` |
| `tests/` | 52 test modules — the classification of each is in `docs/design/01-inventory.md` §4 |
| `gui/` | the old HTTP GUI and agent; superseded by the Studio UI (ADR 0009) |
| `tools/` | `verify_data.py`, the dataset verifier |
| `saved_scenarios/` | old scenario presets; migrated into `scenarios/` as overlays |
| `DATASHEET.md` | the datasheet of the legacy MA dataset family |
| `pyproject.toml` | packaging for the frozen package only — the new Python package lives in `python/v2xw/` |

The retired Windows/MOSAIC layer (`scms-sim/`, `run.ps1`, `gui.ps1`, the
`third_party/veremi-nextgen` submodule) was deleted in Phase 0, not moved here:
it never produced results and ran only from a Windows toolchain
(`01-inventory.md` §3.7, §3.8, §6).
