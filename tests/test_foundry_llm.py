"""Tests for the LLM semantic mutation operator wired into the misbehavior foundry.

Everything here is OFFLINE and deterministic -- the OpenAI transport is always a scripted mock
(never a real call). Coverage:

  1. the foundry's OPTIONAL ``mutation_fn`` hook leaves the deterministic random path untouched:
     ``mutation_fn=None`` (the default) is byte-identical to omitting it, and a hook that merely
     delegates to the built-in ``mutate`` consumes the RNG identically -> same archive.json bytes;
  2. a deterministic MOCK ``mutation_fn`` is honoured end-to-end (injection works with no LLM at all)
     and fills the descriptor cell it targets;
  3. ``agent.llm_mutation_fn`` proposes a VALID genome from a scripted ``_CHAT_FN``, and FALLS BACK to
     the deterministic random ``mutate(parent, rng)`` on garbage replies / raised errors / a missing
     key -- so the search loop is never broken;
  4. the in-GUI copilot ``run_foundry`` tool wires up (executor routed to ``run_foundry_llm``).
"""
from __future__ import annotations

import json
import random
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "src"))
sys.path.insert(0, str(REPO / "gui"))
import agent  # noqa: E402
from scms_sim_ref.datagen import foundry  # noqa: E402


# --------------------------------------------------------------------------- #
# 1) Determinism preserved: the hook default does NOT perturb the existing path
# --------------------------------------------------------------------------- #
def test_default_and_delegating_hook_are_byte_identical(tmp_path):
    """run_foundry with NO mutation_fn, with an explicit mutation_fn=None, and with a hook that
    delegates straight to the built-in mutate() all produce a byte-identical archive.json. The last
    case is the sharp one: because the hook path only adds archive.summary() (which draws no RNG)
    before calling mutate(parent, rng), the RNG draw sequence is preserved exactly -- proving the
    injection point cannot change the deterministic default behaviour."""
    kw = dict(budget=4, seed=7, base_duration=22.0, objective="evade")
    da, dc, dd = tmp_path / "a", tmp_path / "c", tmp_path / "d"
    foundry.run_foundry(out_dir=str(da), **kw)                               # default (omitted)
    foundry.run_foundry(out_dir=str(dc), mutation_fn=None, **kw)             # explicit None
    foundry.run_foundry(out_dir=str(dd),                                     # delegating hook
                        mutation_fn=lambda parent, summary, rng: foundry.mutate(parent, rng), **kw)
    ba = (da / "archive.json").read_bytes()
    assert ba == (dc / "archive.json").read_bytes()          # omitted == explicit None
    assert ba == (dd / "archive.json").read_bytes()          # default == delegating hook (RNG preserved)
    assert (da / "FOUNDRY_REPORT.md").read_bytes() == (dd / "FOUNDRY_REPORT.md").read_bytes()


# --------------------------------------------------------------------------- #
# 2) Injection works with a deterministic MOCK mutation_fn (no LLM involved)
# --------------------------------------------------------------------------- #
# A fixed single-family ('speed') genome. The 3 base genomes are all multi-family ('mixed'), so a
# filled 'speed' cell can ONLY have come from the injected operator -- a clean discriminator.
_FIXED_SPEED_GENOME = {
    "traffic_flow": True, "car_following": True, "n_lanes": 2, "arrival_rate": 2.5,
    "grid_block_m": 130.0, "road_network": "grid", "grid_w": 5, "grid_h": 5,
    "attacker_pct": 0.4, "attack_types": ("RandomSpeed", "StopAndGo"),
}


def test_injected_mutation_fn_fills_targeted_cell(tmp_path):
    """A deterministic fake mutation_fn returning a fixed 'speed' genome is called once per budget
    iteration, receives the archive_summary, and drives a 'speed' cell into the archive -- proving
    the hook works without any LLM."""
    seen = {"n": 0}

    def fake_mutation_fn(parent, summary, rng):
        seen["n"] += 1
        assert {"axes", "empty_cells", "hardest_cells", "grid_size"} <= set(summary)  # gets the summary
        return dict(_FIXED_SPEED_GENOME)

    arch = foundry.run_foundry(budget=5, seed=3, base_duration=22.0, out_dir=str(tmp_path),
                               objective="evade", mutation_fn=fake_mutation_fn)
    assert seen["n"] == 5                                    # hook invoked once per iteration (budget)
    assert "speed" in {cell[0] for cell in arch.cells}, sorted(arch.cells)   # targeted cell filled
    for cell, elite in arch.cells.items():                   # still real, validated scenarios
        assert elite["metrics"]["attackers"] > 0 and elite["metrics"]["ma_rows"] > 0, cell


# --------------------------------------------------------------------------- #
# 3) agent.llm_mutation_fn: scripted-LLM success + robust fallback (no real network)
# --------------------------------------------------------------------------- #
def _empty_summary() -> dict:
    return foundry.Archive().summary()


def test_llm_mutation_fn_success_via_mocked_chat(monkeypatch):
    """A scripted _CHAT_FN returning a JSON genome aimed at an empty cell yields a VALID genome that
    passes validate_config and carries the model's chosen overrides (i.e. the LLM path, not the
    random fallback)."""
    parent = dict(foundry.BASE_GENOMES[0])                   # a 'grid' base genome
    stealth_types = list(foundry.FAMILY_TO_TYPES["stealth"])

    def scripted(messages, tools, model, key, timeout=90.0):
        assert tools is None                                 # plain completion, no tool-calling
        assert any("empty_cells_to_fill" in (m.get("content") or "") for m in messages)
        return {"content": json.dumps({"attack_types": stealth_types, "road_network": "ring",
                                       "grid_w": 12, "grid_h": 6, "attacker_pct": 0.45})}

    monkeypatch.setattr(agent, "_CHAT_FN", scripted)         # inject via the module-level hook
    genome = agent.llm_mutation_fn(parent, _empty_summary(), random.Random(1), key="sk-test")

    assert genome["road_network"] == "ring" and genome["attacker_pct"] == 0.45   # LLM overrides applied
    assert set(genome["attack_types"]) == set(stealth_types)
    assert foundry._family_bin(genome) == "stealth"          # lands in the targeted family cell
    foundry.build_config(genome, seed=1, duration_s=20.0, out_dir="x")   # validates like the foundry


def test_llm_mutation_fn_falls_back_on_garbage_exceptions_and_missing_key(monkeypatch):
    """On a non-JSON reply, a raising transport, OR a missing key, the operator falls back to the
    DETERMINISTIC random mutate(parent, rng): the returned genome is byte-equal to a direct
    foundry.mutate() call on the same seed, and is always feasible. The loop never breaks."""
    parent = dict(foundry.BASE_GENOMES[1])                   # a 'ring' base genome
    summary = _empty_summary()
    expected = foundry.mutate(dict(parent), random.Random(99))   # the deterministic fallback result

    # (a) garbage, non-JSON content -> parse fails -> fallback
    monkeypatch.setattr(agent, "_CHAT_FN",
                        lambda messages, tools, model, key, timeout=90.0: {"content": "sorry, no JSON"})
    g_garbage = agent.llm_mutation_fn(parent, summary, random.Random(99), key="sk-test")
    assert g_garbage == expected

    # (b) transport raises (network / HTTP error) -> fallback
    def boom(*a, **k):
        raise RuntimeError("network down")
    monkeypatch.setattr(agent, "_CHAT_FN", boom)
    g_raise = agent.llm_mutation_fn(parent, summary, random.Random(99), key="sk-test")
    assert g_raise == expected

    # (c) no key -> transport is never called, still a valid random mutation
    called = {"n": 0}
    def must_not_call(*a, **k):
        called["n"] += 1
        raise AssertionError("transport must not be called without a key")
    monkeypatch.setattr(agent, "_CHAT_FN", must_not_call)
    g_nokey = agent.llm_mutation_fn(parent, summary, random.Random(99), key="")
    assert g_nokey == expected and called["n"] == 0

    for g in (g_garbage, g_raise, g_nokey):                  # every fallback is feasible
        foundry.build_config(g, seed=1, duration_s=20.0, out_dir="x")


def test_llm_operator_drives_a_real_foundry_run(tmp_path, monkeypatch):
    """End-to-end: run_foundry_llm builds the LLM operator (make_llm_mutation_fn) and drives
    foundry.run_foundry with it. A scripted _CHAT_FN returns a valid 'speed' genome, so the search
    completes and fills a 'speed' cell -- with NO network. Proves the headless entry point works."""
    speed_types = list(foundry.FAMILY_TO_TYPES["speed"])

    def scripted(messages, tools, model, key, timeout=90.0):
        return {"content": json.dumps({"attack_types": speed_types, "road_network": "grid",
                                       "grid_w": 5, "grid_h": 5, "attacker_pct": 0.4,
                                       "arrival_rate": 2.5})}

    monkeypatch.setattr(agent, "_CHAT_FN", scripted)
    arch = agent.run_foundry_llm(budget=3, seed=4, base_duration=22.0, out_dir=str(tmp_path),
                                 objective="evade", key="sk-test")
    assert arch.cells and arch.meta["coverage_cells"] >= 1
    assert "speed" in {cell[0] for cell in arch.cells}, sorted(arch.cells)
    assert (tmp_path / "archive.json").exists()


# --------------------------------------------------------------------------- #
# 4) copilot 'run_foundry' tool wires up (non-live executor test; run_foundry_llm stubbed)
# --------------------------------------------------------------------------- #
def test_run_foundry_tool_is_registered():
    names = [t["function"]["name"] for t in agent.tool_specs()]
    assert "run_foundry" in names


def test_run_foundry_tool_executor_routes_and_caps(monkeypatch):
    """_exec_tool('run_foundry', ...) routes to run_foundry_llm (budget capped at 40), and returns a
    compact summary (coverage / QD-score / hardest cells). run_foundry_llm is stubbed so the test
    stays fast and offline."""
    captured = {}

    class _FakeArchive:
        meta = {"objective": "evade", "seed": 5, "budget": 40, "coverage_cells": 2,
                "grid_size": foundry.grid_size(), "coverage_pct": 0.74,
                "qd_score": 1.5, "best_fitness": 0.9}
        cells = {
            ("speed", "sparse", "grid", "high"): {
                "fitness": 0.9,
                "descriptor": {"attack_family": "speed", "density_band": "sparse",
                               "topology": "grid", "attacker_band": "high"},
                "metrics": {"recall": 0.1}},
            ("stealth", "dense", "ring", "med"): {
                "fitness": 0.5,
                "descriptor": {"attack_family": "stealth", "density_band": "dense",
                               "topology": "ring", "attacker_band": "med"},
                "metrics": {"recall": 0.5}},
        }

    def fake_run_foundry_llm(**kwargs):
        captured.update(kwargs)
        return _FakeArchive()

    monkeypatch.setattr(agent, "run_foundry_llm", fake_run_foundry_llm)
    # Force the keyless (random-fallback) path so the ai_operator assertion is environment-independent:
    # a real checkout has a repo-root .env, a worktree does not -- without this the field flips with env.
    monkeypatch.setattr(agent, "openai_key", lambda: "")
    s = agent.AgentSession()
    r = agent._exec_tool(s, "run_foundry", {"budget": 999, "seed": 5, "objective": "evade",
                                            "duration_s": 30})
    assert r["ok"] and r["coverage_cells"] == 2 and r["qd_score"] == 1.5
    assert r["best_fitness"] == 0.9 and r["grid_size"] == foundry.grid_size()
    assert r["out_dir"].endswith("agent_foundry")
    assert r["ai_operator"] is False                         # forced keyless above -> random-fallback
    assert len(r["hardest_cells"]) == 2 and r["hardest_cells"][0]["fitness"] == 0.9   # hardest first
    # the executor caps a runaway budget and forwards the other knobs
    assert captured["budget"] == 40 and captured["seed"] == 5
    assert captured["objective"] == "evade" and captured["base_duration"] == 30.0
