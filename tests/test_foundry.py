"""Tests for the MAP-Elites quality-diversity foundry (src/scms_sim_ref/datagen/foundry.py).

Budgets/durations are kept tiny so the whole file runs in a few minutes while still exercising the
full closed loop (mutate -> run_pipeline -> validate -> insert). Everything is deterministic, so the
assertions below are stable across runs.
"""

from __future__ import annotations

import json
import random

import pytest

from scms_sim_ref.datagen import foundry
from scms_sim_ref.datagen import validate as valmod
from scms_sim_ref.mock_pipeline import config_from_dict, run_pipeline, validate_config


# --------------------------------------------------------------------------- #
# Fast unit tests (no simulation) -- lock the fitness gate + descriptor contracts
# --------------------------------------------------------------------------- #
def test_fitness_validity_gate():
    # no attackers -> invalid (empty scenario)
    assert foundry.fitness("evade", {"attackers": 0, "ma_rows": 100, "recall": 0.0}, 40)[1] is False
    # no reports -> invalid (nothing happening)
    assert foundry.fitness("evade", {"attackers": 5, "ma_rows": 0, "recall": 0.0}, 40)[1] is False
    # VALID and recall==0 -> fitness 1.0: the perfect-evasion jackpot must NOT be gated out
    f, valid = foundry.fitness("evade", {"attackers": 5, "ma_rows": 100, "recall": 0.0}, 40)
    assert valid and abs(f - 1.0) < 1e-9
    # evade fitness == 1 - recall
    f, valid = foundry.fitness("evade", {"attackers": 5, "ma_rows": 100, "recall": 0.3}, 40)
    assert valid and abs(f - 0.7) < 1e-9


def test_fitness_family_and_latency_gates():
    # family objective: family absent -> invalid
    s = {"attackers": 5, "ma_rows": 100, "recall_by_family": {"position": 0.5}}
    assert foundry.fitness("family:stealth", s, 40)[1] is False
    # family present -> fitness = 1 - recall_by_family[F]
    s2 = {"attackers": 5, "ma_rows": 100, "recall_by_family": {"stealth": 0.25}}
    f, valid = foundry.fitness("family:stealth", s2, 40)
    assert valid and abs(f - 0.75) < 1e-9
    # latency: no measured latency (n==0) -> invalid
    s3 = {"attackers": 5, "ma_rows": 100, "detection_latency_s": {}}
    assert foundry.fitness("latency", s3, 40)[1] is False
    # latency: normalized median / duration
    s4 = {"attackers": 5, "ma_rows": 100, "detection_latency_s": {"n": 3, "median_s": 20.0}}
    f, valid = foundry.fitness("latency", s4, 40)
    assert valid and abs(f - 0.5) < 1e-9


def test_descriptor_bins():
    # single stealth family, sparse (30 veh), grid, high attacker (0.4)
    d = foundry.descriptor({"attack_types": ["SlowDrift"], "road_network": "grid",
                            "attacker_pct": 0.4}, {"vehicles": 30})
    assert d == ("stealth", "sparse", "grid", "high")
    # two families -> 'mixed', dense (150 veh), ring, low attacker (0.1)
    d2 = foundry.descriptor({"attack_types": ["SlowDrift", "Sybil"], "road_network": "ring",
                             "attacker_pct": 0.1}, {"vehicles": 150})
    assert d2 == ("mixed", "dense", "ring", "low")


def test_grid_size_and_families():
    # 9 reachable families + 'mixed' = 10 family bins; 10 * 3 * 3 * 3 = 270 cells
    assert set(foundry.ATTACK_FAMILIES) == {"combined", "credential", "event", "heading",
                                            "identity", "position", "speed", "stealth", "timing"}
    assert foundry.grid_size() == 10 * 3 * 3 * 3


# --------------------------------------------------------------------------- #
# Mutation feasibility -- mutate() output ALWAYS passes validate_config
# --------------------------------------------------------------------------- #
def test_mutation_always_feasible():
    rng = random.Random(1234)
    for base in foundry.BASE_GENOMES:
        g = dict(base)
        for _ in range(200):                 # chain mutations (drift), 200 per base = 600 total
            g = foundry.mutate(g, rng)
            assert isinstance(g, dict) and g.get("attack_types")
            try:
                # the exact construction the driver uses -- validate_config is the feasibility oracle
                foundry.build_config(g, seed=1, duration_s=30.0, out_dir="x")
                # and directly, to be explicit about the guarantee the operator makes
                validate_config(config_from_dict({**g, "seed": 1, "duration_s": 30.0,
                                                  "traffic_flow": True, "out_dir": "x"}))
            except Exception as exc:  # noqa: BLE001
                pytest.fail(f"mutate() produced an infeasible genome {g!r}: "
                            f"{type(exc).__name__}: {exc}")


# --------------------------------------------------------------------------- #
# Determinism -- same (budget, seed, objective, duration) -> byte-identical archive.json
# --------------------------------------------------------------------------- #
def test_determinism_byte_identical(tmp_path):
    o1, o2 = tmp_path / "a", tmp_path / "b"
    a1 = foundry.run_foundry(budget=8, seed=7, base_duration=28.0, out_dir=str(o1), objective="evade")
    a2 = foundry.run_foundry(budget=8, seed=7, base_duration=28.0, out_dir=str(o2), objective="evade")
    # archive.json is byte-for-byte identical (no timestamps / absolute paths inside)
    assert (o1 / "archive.json").read_bytes() == (o2 / "archive.json").read_bytes()
    assert (o1 / "FOUNDRY_REPORT.md").read_bytes() == (o2 / "FOUNDRY_REPORT.md").read_bytes()
    # ...and the in-memory archive matches cell-for-cell (cells, fitnesses, genomes)
    assert set(a1.cells) == set(a2.cells)
    for k in a1.cells:
        assert a1.cells[k]["fitness"] == a2.cells[k]["fitness"]
        assert a1.cells[k]["genome"] == a2.cells[k]["genome"]


# --------------------------------------------------------------------------- #
# Shared evade archive (module-scoped) for the improvement / diversity / round-trip tests
# --------------------------------------------------------------------------- #
@pytest.fixture(scope="module")
def evade_archive(tmp_path_factory):
    out = tmp_path_factory.mktemp("foundry_evade")
    arch = foundry.run_foundry(budget=14, seed=11, base_duration=28.0,
                               out_dir=str(out), objective="evade")
    return arch, str(out)


def test_objective_improvement(evade_archive):
    """The search finds something at least as hard as the best base genome (usually harder)."""
    arch, _ = evade_archive
    assert arch.cells, "archive should not be empty"
    assert arch.meta["best_fitness"] >= arch.meta["base_best_fitness"]
    assert 0.0 <= arch.meta["best_fitness"] <= 1.0


def test_diversity_multiple_cells(evade_archive):
    """Diverse by construction: more than one descriptor cell is filled."""
    arch, _ = evade_archive
    assert arch.coverage() > 1
    # the 3 base genomes alone occupy >=3 distinct topology cells
    assert len({cell[2] for cell in arch.cells}) >= 2


def test_no_degenerate_elites(evade_archive):
    """Every archived elite is a real misbehavior scenario (attackers + reports)."""
    arch, _ = evade_archive
    for cell, elite in arch.cells.items():
        assert elite["metrics"]["attackers"] > 0, cell
        assert elite["metrics"]["ma_rows"] > 0, cell


def test_roundtrip_reproducibility(evade_archive, tmp_path):
    """An archived elite, re-run from its stored config, reproduces its recorded recall exactly."""
    arch, _ = evade_archive
    elite = max(arch.cells.values(), key=lambda c: c["fitness"])
    replay = tmp_path / "replay"
    cfg = config_from_dict({**elite["config"], "out_dir": str(replay)})
    run_pipeline(cfg)
    summary = valmod.validate(str(replay))[0]
    assert summary["recall"] == elite["metrics"]["recall"]
    assert summary["attackers"] > 0 and summary["ma_rows"] > 0


# --------------------------------------------------------------------------- #
# Objective modes -- family:<F> and latency
# --------------------------------------------------------------------------- #
def test_objective_family_mode(tmp_path):
    """family:stealth targets stealth: every archived cell has stealth present and fitness wired to it."""
    arch = foundry.run_foundry(budget=12, seed=3, base_duration=28.0,
                               out_dir=str(tmp_path), objective="family:stealth")
    assert arch.cells
    for elite in arch.cells.values():
        rbf = elite["metrics"]["recall_by_family"]
        assert "stealth" in rbf                                   # validity gate: family present
        assert abs(elite["fitness"] - (1.0 - rbf["stealth"])) < 1e-6   # fitness == 1 - recall[F]


def test_objective_latency_mode(tmp_path):
    """latency objective: every cell has a measured latency and fitness == median/duration (clamped)."""
    arch = foundry.run_foundry(budget=10, seed=5, base_duration=28.0,
                               out_dir=str(tmp_path), objective="latency")
    assert arch.cells
    for elite in arch.cells.values():
        lat = elite["metrics"]["detection_latency_s"]
        assert lat and lat.get("n", 0) > 0
        exp = max(0.0, min(1.0, lat["median_s"] / elite["duration_s"]))
        assert abs(elite["fitness"] - exp) < 1e-6


# --------------------------------------------------------------------------- #
# CLI smoke -- main() writes archive.json + FOUNDRY_REPORT.md with the documented keys
# --------------------------------------------------------------------------- #
def test_cli_smoke(tmp_path):
    out = tmp_path / "cli"
    rc = foundry.main(["--budget", "3", "--seed", "1", "--duration", "25",
                       "--objective", "evade", "--out", str(out)])
    assert rc == 0
    archive_json = out / "archive.json"
    report_md = out / "FOUNDRY_REPORT.md"
    assert archive_json.exists() and report_md.exists()

    doc = json.loads(archive_json.read_text(encoding="utf-8"))
    for key in ("objective", "seed", "budget", "base_duration_s", "descriptor_axes", "fitness",
                "grid_size", "coverage_cells", "coverage_pct", "qd_score",
                "base_best_fitness", "best_fitness", "cells"):
        assert key in doc, f"archive.json missing {key!r}"
    assert doc["cells"], "bases should fill at least one cell"
    for key in ("cell", "descriptor", "fitness", "seed", "duration_s", "genome", "config", "metrics"):
        assert key in doc["cells"][0], f"cell missing {key!r}"

    text = report_md.read_text(encoding="utf-8")
    assert "blind-spot map" in text
    assert "Coverage" in text and "QD-score" in text
