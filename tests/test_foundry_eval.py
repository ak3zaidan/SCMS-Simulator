"""Tests for the foundry operator evaluation harness (gui/foundry_eval.py).

Everything here is OFFLINE and deterministic. The LLM operator is NEVER exercised for real: every
test injects a deterministic MOCK ``llm_operator_factory`` (or monkeypatches
``agent.make_llm_mutation_fn`` to a mock), so no network call is ever made. As belt-and-suspenders,
``agent._CHAT_FN`` is monkeypatched to a function that RAISES on any call -- so if the real transport
were ever reached the test would fail loudly instead of hitting the network.

Budgets/durations are kept tiny (a single small run is ~1s) while still exercising the full
compare -> aggregate -> verdict -> report path.

Coverage:
  1. compare_operators returns the documented verdict keys; per_seed has one entry per seed; the
     metrics are numbers and the cell sets are lists; and the speed-targeting mock makes the harness
     detect targeted gap-filling (a 'speed' cell in llm_only_cells the random side lacked);
  2. multi-seed aggregation (per_seed length == n_seeds; the mean is the per-seed average);
  3. the harness is DETERMINISTIC given a deterministic mock (same args + mock -> identical dict);
  4. render_report is honest about a null/negative result, and the CLI writes REPORT.md + result.json
     fully offline (no key, mock operator).
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "src"))
sys.path.insert(0, str(REPO / "gui"))
import agent            # noqa: E402  (gui sibling: owns the real LLM transport)
import foundry_eval     # noqa: E402  (the harness under test)
from scms_sim_ref.datagen import foundry  # noqa: E402


# --------------------------------------------------------------------------- #
# Deterministic mock operator: always proposes a fixed single-'speed'-family genome.
# The 3 base genomes are all multi-family ('mixed'), so any filled 'speed' cell can ONLY have come
# from the injected operator -- a clean discriminator for targeted gap-filling.
# --------------------------------------------------------------------------- #
_FIXED_SPEED_GENOME = {
    "traffic_flow": True, "car_following": True, "n_lanes": 2, "arrival_rate": 2.5,
    "grid_block_m": 130.0, "road_network": "grid", "grid_w": 5, "grid_h": 5,
    "attacker_pct": 0.4, "attack_types": ("RandomSpeed", "StopAndGo"),   # both in FAMILY_TO_TYPES['speed']
}


def _mock_factory(key):
    """A mock ``llm_operator_factory``: ``(key) -> mutation_fn`` that always returns the fixed 'speed'
    genome (ignoring parent/summary/rng), so the 'llm' side deterministically targets 'speed' cells."""
    def _op(parent, summary, rng):
        return dict(_FIXED_SPEED_GENOME)
    return _op


def _boom(*args, **kwargs):
    """Stand-in for the real transport: raises so any accidental network call fails the test."""
    raise AssertionError("real LLM transport (_CHAT_FN) must never be called in tests")


# --------------------------------------------------------------------------- #
# 1) verdict keys + metrics + targeted gap-filling detection
# --------------------------------------------------------------------------- #
def test_compare_operators_verdict_metrics_and_gap_filling(monkeypatch):
    monkeypatch.setattr(agent, "_CHAT_FN", _boom)              # guard: prove no network is touched
    # sanity: the mock's attack types really are the 'speed' family (self-documenting guard)
    assert all(t in foundry.FAMILY_TO_TYPES["speed"] for t in _FIXED_SPEED_GENOME["attack_types"])

    res = foundry_eval.compare_operators(budget=5, seed=3, base_duration=22.0, objective="evade",
                                         llm_operator_factory=_mock_factory, key="unused-mock-key")

    # --- documented verdict keys are all present, with the right types -------------------------- #
    for k in ("llm_wins_coverage", "llm_wins_qd", "llm_wins_best", "n_seeds", "budget",
              "objective", "mean", "per_seed"):
        assert k in res, f"verdict missing key {k!r}"
    for k in ("llm_wins_coverage", "llm_wins_qd", "llm_wins_best"):
        assert isinstance(res[k], bool)
    assert res["n_seeds"] == 1 and res["budget"] == 5 and res["objective"] == "evade"

    # --- mean has both operators with all metric keys ------------------------------------------ #
    assert set(res["mean"]) == {"random", "llm"}
    for op in ("random", "llm"):
        for mk in ("coverage", "qd_score", "best_fitness", "mean_fitness", "grid_size"):
            assert mk in res["mean"][op], f"mean[{op}] missing {mk!r}"

    # --- per_seed: exactly one entry (single seed), documented keys, numeric metrics, list cells - #
    assert len(res["per_seed"]) == res["n_seeds"] == 1
    ps = res["per_seed"][0]
    for k in ("seed", "random", "llm", "delta_coverage", "delta_qd", "delta_best",
              "llm_only_cells", "random_only_cells"):
        assert k in ps, f"per_seed entry missing key {k!r}"
    assert ps["seed"] == 3
    for op in ("random", "llm"):
        m = ps[op]
        assert isinstance(m["coverage"], int)
        assert isinstance(m["qd_score"], (int, float))
        assert isinstance(m["best_fitness"], (int, float))
        assert isinstance(m["mean_fitness"], (int, float))
        assert isinstance(m["cells"], list)
        for cell in m["cells"]:
            assert isinstance(cell, list) and len(cell) == 4     # 4-tuple descriptor -> list, JSON-safe
    assert isinstance(ps["llm_only_cells"], list) and isinstance(ps["random_only_cells"], list)

    # --- the harness DETECTS targeted gap-filling: the speed-only mock fills a 'speed' cell the
    #     random side never reached (base genomes are all 'mixed', so it must be the operator) ---- #
    speed_only = [c for c in ps["llm_only_cells"] if c[0] == "speed"]
    assert speed_only, ("expected the speed-targeting mock to fill a 'speed' cell the random side "
                        f"lacked; llm_only_cells={ps['llm_only_cells']}")

    # --- fully JSON-serializable (tuples were converted to lists) ------------------------------- #
    json.dumps(res)


# --------------------------------------------------------------------------- #
# 2) multi-seed aggregation: one per_seed entry per seed; mean == per-seed average
# --------------------------------------------------------------------------- #
def test_multi_seed_aggregation(monkeypatch):
    monkeypatch.setattr(agent, "_CHAT_FN", _boom)
    res = foundry_eval.compare_operators(budget=2, seed=999, base_duration=18.0, objective="evade",
                                         seeds=[3, 7], llm_operator_factory=_mock_factory, key="k")
    assert res["n_seeds"] == 2
    assert [ps["seed"] for ps in res["per_seed"]] == [3, 7]     # one entry per seed, in order
    # the mean is exactly the average of the per-seed metrics
    for op in ("random", "llm"):
        for mk in ("coverage", "qd_score", "best_fitness", "mean_fitness"):
            expected = round((res["per_seed"][0][op][mk] + res["per_seed"][1][op][mk]) / 2, 6)
            assert res["mean"][op][mk] == expected, f"mean[{op}][{mk}] != per-seed average"


# --------------------------------------------------------------------------- #
# 3) determinism of the harness given a deterministic mock
# --------------------------------------------------------------------------- #
def test_harness_is_deterministic_with_deterministic_mock(monkeypatch):
    monkeypatch.setattr(agent, "_CHAT_FN", _boom)
    kw = dict(budget=3, seed=5, base_duration=20.0, objective="evade",
              llm_operator_factory=_mock_factory, key="k")
    r1 = foundry_eval.compare_operators(**kw)
    r2 = foundry_eval.compare_operators(**kw)
    assert r1 == r2                                            # identical dict (both sides deterministic)


# --------------------------------------------------------------------------- #
# 4a) render_report is honest about a null/negative result (fast: hand-built dict, no simulation)
# --------------------------------------------------------------------------- #
def _fake_result(win: bool) -> dict:
    m = {"coverage": (6.0 if win else 8.0), "qd_score": 4.0, "best_fitness": 0.8,
         "mean_fitness": 0.5, "grid_size": foundry.grid_size()}
    rand = {"coverage": 8.0, "qd_score": 5.0, "best_fitness": 0.9, "mean_fitness": 0.6,
            "grid_size": foundry.grid_size()}
    llm = {"coverage": (9.0 if win else 6.0), "qd_score": (7.0 if win else 4.0),
           "best_fitness": (0.95 if win else 0.8), "mean_fitness": 0.6, "grid_size": foundry.grid_size()}
    ps = {"seed": 3, "random": {**rand, "cells": []}, "llm": {**llm, "cells": []},
          "delta_coverage": int(llm["coverage"] - rand["coverage"]),
          "delta_qd": round(llm["qd_score"] - rand["qd_score"], 6),
          "delta_best": round(llm["best_fitness"] - rand["best_fitness"], 6),
          "llm_only_cells": [], "random_only_cells": []}
    return {"llm_wins_coverage": win, "llm_wins_qd": win, "llm_wins_best": win,
            "n_seeds": 1, "budget": 10, "objective": "evade",
            "mean": {"random": rand, "llm": llm}, "per_seed": [ps]}


def test_render_report_honest_null_and_win():
    null = foundry_eval.render_report(_fake_result(win=False))
    assert isinstance(null, str)
    assert "## Verdict" in null and "Per-seed results" in null and "Mean across seeds" in null
    assert "No advantage detected" in null                     # honest negative result, not fabricated
    assert "| random |" in null and "| llm" in null            # both operators in the mean table

    won = foundry_eval.render_report(_fake_result(win=True))
    assert "beats random search" in won                        # the clear-win branch


# --------------------------------------------------------------------------- #
# 4b) CLI writes REPORT.md + result.json fully offline (no key; mock operator via make_llm_mutation_fn)
# --------------------------------------------------------------------------- #
def test_cli_writes_report_offline(tmp_path, monkeypatch):
    # default factory -> agent.make_llm_mutation_fn(key=...); patch it to return our mock operator
    monkeypatch.setattr(agent, "make_llm_mutation_fn",
                        lambda chat_fn=None, model=None, key=None: _mock_factory(key))
    monkeypatch.setattr(agent, "openai_key", lambda: "")       # no key -> no real call is even attempted
    monkeypatch.setattr(agent, "_CHAT_FN", _boom)              # guard: fail loudly on any network use

    rc = foundry_eval.main(["--budget", "2", "--seed", "1", "--duration", "18",
                            "--objective", "evade", "--out", str(tmp_path)])
    assert rc == 0
    report_md = tmp_path / "REPORT.md"
    result_json = tmp_path / "result.json"
    assert report_md.exists() and result_json.exists()

    text = report_md.read_text(encoding="utf-8")
    assert "# Foundry operator evaluation" in text and "## Verdict" in text

    doc = json.loads(result_json.read_text(encoding="utf-8"))
    for k in ("llm_wins_coverage", "llm_wins_qd", "llm_wins_best", "n_seeds", "budget",
              "objective", "mean", "per_seed"):
        assert k in doc
    assert doc["n_seeds"] == 1 and len(doc["per_seed"]) == 1
