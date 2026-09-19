"""Dataset-robustness controls in datagen.massive: stratified attacks, class balancing, curriculum
ordering, and the additive corpus balance/coverage report.

Most assertions are made against the PURE, deterministic domain plan (``massive.build_domain_plan``)
or the difficulty proxies, so they run no pipeline. Only the report / off-by-default tests do tiny
end-to-end runs (2 short domains) to exercise the real merged-corpus output.
"""

import json
import os
import tempfile
from pathlib import Path

import pandas as pd

from scms_sim_ref.datagen import corpus_report, massive
from scms_sim_ref.mock_pipeline import PipelineConfig


def test_all_scenario_keeps_full_catalog_not_collapsed_by_f2_sentinel():
    """Regression: the F2 attack_type sentinel must not collapse massive's rich 'ALL' mixed cells.
    An 'ALL' cell must yield attack_type='' (sentinel) + the default full attack_types, so the
    pipeline's narrowing (attack_type truthy AND attack_types==default) does NOT fire; a single-
    scenario cell keeps its 1-tuple so it still narrows."""
    tmp = Path(tempfile.mkdtemp())
    base = dict(weather="clear", rotate_period_s=0.0, collude_pct=0.0, faulty_pct=0.05,
                attacker_pct=0.3, n_vehicles=30)
    cfg_all = massive.cell_config({"scenario": "ALL", **base}, 0, 7, 120, tmp, flow=False)
    assert cfg_all.attack_type == ""                                      # sentinel, not "ConstPos"
    assert tuple(cfg_all.attack_types) == PipelineConfig().attack_types    # full catalog preserved
    assert not (cfg_all.attack_type and tuple(cfg_all.attack_types) == PipelineConfig().attack_types)
    cfg_one = massive.cell_config({"scenario": "RandomSpeed", **base}, 1, 7, 120, tmp, flow=False)
    assert cfg_one.attack_types == ("RandomSpeed",)


# ---------------------------------------------------------------------------- #
# helpers
# ---------------------------------------------------------------------------- #
def _cells(scenarios, attacker_pct=0.2, **over):
    base = dict(weather="clear", rotate_period_s=0.0, collude_pct=0.0,
                faulty_pct=0.05, attacker_pct=attacker_pct, n_vehicles=40)
    base.update(over)
    return [{"scenario": s, **base} for s in scenarios]


def _rel_files(root):
    out = set()
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            out.add(os.path.relpath(os.path.join(dirpath, f), root).replace("\\", "/"))
    return out


# ---------------------------------------------------------------------------- #
# 1. --stratify-attacks : every family covered + deterministic (pure)
# ---------------------------------------------------------------------------- #
def test_stratify_attacks_plan_covers_every_family_including_combined():
    # 10 arbitrary cells -> the stratified schedule forces types so every family appears.
    cells = _cells(["ConstPos", "ALL", "RandomSpeed", "Sybil", "DoS",
                    "ReversedHeading", "InvalidSignature", "HeadingOffset",
                    "ConstPosOffset", "SlowDrift"])
    plan = massive.build_domain_plan(cells, base_seed=7, stratify=True)

    fams = {e["strat_family"] for e in plan}
    assert fams == set(corpus_report.ALL_FAMILIES), (fams, corpus_report.ALL_FAMILIES)
    # the opt-in "combined" family is explicitly included when stratifying
    assert "combined" in fams and set(corpus_report.OPT_IN_FAMILIES) <= fams
    # every forced type is a real renderable type (single source of truth)
    assert all(e["strat_type"] in corpus_report.ALL_TYPES for e in plan)

    # determinism: same args -> byte-identical plan twice
    again = massive.build_domain_plan(cells, base_seed=7, stratify=True)
    assert json.dumps(plan, sort_keys=True) == json.dumps(again, sort_keys=True)


def test_stratify_schedule_hits_all_families_first_then_all_types():
    sched = massive._stratified_schedule()
    n_fam = len(corpus_report.ALL_FAMILIES)
    # first len(families) entries cover every family exactly once
    assert {corpus_report.TYPE_TO_FAMILY[t] for t in sched[:n_fam]} == set(corpus_report.ALL_FAMILIES)
    # the whole schedule covers every renderable type
    assert set(sched) == set(corpus_report.ALL_TYPES) and len(sched) == len(corpus_report.ALL_TYPES)


# ---------------------------------------------------------------------------- #
# 2. --balance-classes : attacker_pct biased toward the target (pure)
# ---------------------------------------------------------------------------- #
def test_balance_classes_biases_attacker_pct_toward_target():
    # unstratified draw is the flat grid value 0.2; the target is 0.45.
    cells = _cells(["ConstPos", "ALL", "RandomSpeed", "Sybil", "DoS", "SlowDrift"],
                   attacker_pct=0.2)
    target = 0.45

    off = massive.build_domain_plan(cells, base_seed=7)
    balanced = massive.build_domain_plan(cells, base_seed=7, balance_target=target)

    off_mean = sum(e["attacker_pct"] for e in off) / len(off)
    bal_mean = sum(e["attacker_pct"] for e in balanced) / len(balanced)
    assert all(e["attacker_pct"] == 0.2 for e in off)        # unchanged (uniform grid value)
    # the balanced corpus lands MUCH nearer the target than the uniform draw
    assert abs(bal_mean - target) < abs(off_mean - target)
    assert abs(bal_mean - target) < 0.05
    # domains still vary (it biases, it does not collapse to a constant)
    assert len({e["attacker_pct"] for e in balanced}) > 1

    # deterministic + seed-sensitive
    assert massive.build_domain_plan(cells, 7, balance_target=target) == balanced
    assert massive.build_domain_plan(cells, 8, balance_target=target) != balanced


# ---------------------------------------------------------------------------- #
# 3. difficulty proxies: stealth / low-intensity / pulsed / sparse = harder (pure)
# ---------------------------------------------------------------------------- #
def test_difficulty_proxies_rank_stealth_and_pulsed_hardest():
    def tier(entry, flow=False):
        return massive._difficulty_tier(massive._difficulty_score(entry, flow))

    blatant_dense = {"scenario": "ConstPos", "attacker_pct": 0.3}      # easy
    stealth = {"scenario": "SlowDrift", "attacker_pct": 0.3}          # stealth family
    sparse = {"scenario": "ConstPos", "attacker_pct": 0.05}           # few positives
    pulsed = {"scenario": "ConstPos", "attacker_pct": 0.3, "intensity": 1.0, "duty": 0.4}
    subtle = {"scenario": "ConstPos", "attacker_pct": 0.3, "intensity": 0.5, "duty": 1.0}

    assert tier(blatant_dense) == 0                     # blatant + dense = easiest tier
    assert tier(stealth) > tier(blatant_dense)          # stealth is harder
    assert tier(sparse) > tier(blatant_dense)           # fewer attackers is harder
    assert tier(pulsed, flow=True) > tier(blatant_dense)   # pulsed/evasive is harder
    assert tier(subtle, flow=True) > tier(blatant_dense)   # low-intensity is harder
    # tiers stay within the documented 0..3 band
    for e, f in [(blatant_dense, False), (stealth, False), (sparse, False),
                 (pulsed, True), (subtle, True)]:
        assert 0 <= tier(e, f) <= 3


# ---------------------------------------------------------------------------- #
# 4. --curriculum : plan/catalog carry difficulty_tier spanning >1 tier, ordered easy->hard
# ---------------------------------------------------------------------------- #
def test_curriculum_plan_orders_easy_to_hard_and_spans_tiers():
    cells = _cells(["SlowDrift", "ConstPos", "AlongRoadOffset", "Sybil"], attacker_pct=0.3)
    plan = massive.build_domain_plan(cells, base_seed=7, curriculum=True)

    tiers = [e["difficulty_tier"] for e in plan]
    assert all(0 <= t <= 3 for t in tiers)
    assert len(set(tiers)) > 1, "the curriculum must span more than one difficulty tier"
    assert tiers == sorted(tiers), "domains are ordered easy(low tier) -> hard(high tier)"
    # the two blatant scenarios sort ahead of (are easier than) the two stealth scenarios
    blatant = [e["difficulty_tier"] for e in plan if e["scenario"] in ("ConstPos", "Sybil")]
    stealth = [e["difficulty_tier"] for e in plan if e["scenario"] in ("SlowDrift", "AlongRoadOffset")]
    assert max(blatant) < min(stealth)


def test_curriculum_records_difficulty_tier_in_domain_catalog(tmp_path, monkeypatch):
    """End-to-end (tiny): --curriculum stamps difficulty_tier into domain_catalog.json rows."""
    tiny = {"scenario": ["ConstPos", "SlowDrift"], "weather": ["clear"], "rotate_period_s": [0.0],
            "collude_pct": [0.0], "faulty_pct": [0.0], "attacker_pct": [0.3], "n_vehicles": [40]}
    monkeypatch.setitem(massive.GRIDS, "tiny", tiny)
    out = tmp_path / "curr"
    rc = massive.main(["--grid", "tiny", "--curriculum", "--seed", "3", "--steps", "50",
                       "--no-report", "--out", str(out)])
    assert rc == 0
    cat = json.loads((out / "domain_catalog.json").read_text())
    assert len(cat) == 2
    assert all("difficulty_tier" in c for c in cat)
    tiers = [c["difficulty_tier"] for c in cat]
    assert len(set(tiers)) > 1                         # spans >1 tier
    assert tiers == sorted(tiers)                      # rows emitted easy -> hard
    # the blatant ConstPos domain is strictly easier than the stealth SlowDrift domain
    by_scen = {c["scenario"]: c["difficulty_tier"] for c in cat}
    assert by_scen["ConstPos"] < by_scen["SlowDrift"]


# ---------------------------------------------------------------------------- #
# 5. --report (default ON): CORPUS_REPORT.md written with headers; imbalance -> warnings
# ---------------------------------------------------------------------------- #
def test_report_is_written_with_headers_and_warns_on_imbalanced_corpus(tmp_path, monkeypatch):
    tiny = {"scenario": ["ConstPos", "ALL"], "weather": ["clear"], "rotate_period_s": [0.0],
            "collude_pct": [0.0], "faulty_pct": [0.0], "attacker_pct": [0.25], "n_vehicles": [40]}
    monkeypatch.setitem(massive.GRIDS, "tiny", tiny)
    out = tmp_path / "rep"
    rc = massive.main(["--grid", "tiny", "--seed", "3", "--steps", "50", "--out", str(out)])
    assert rc == 0

    md_path = out / "CORPUS_REPORT.md"
    assert md_path.exists(), "reporting is ON by default"
    md = md_path.read_text(encoding="utf-8")
    for header in ("# Corpus balance & coverage report", "## Class balance", "## Attack coverage",
                   "## Domain diversity", "## Difficulty spread", "## Size", "## Warnings"):
        assert header in md, header

    # a deliberately imbalanced tiny corpus (only 1-2 families present of 8) must raise warnings.
    rep = corpus_report.build_report(str(out))          # read-only re-run for a clean assertion
    assert rep["warnings"], "an imbalanced tiny corpus must produce balance/coverage warnings"
    assert "None -- the corpus is well-balanced" not in md


# ---------------------------------------------------------------------------- #
# 6. OFF-by-default: default plan is unchanged; reporting is purely additive
# ---------------------------------------------------------------------------- #
def test_defaults_leave_the_domain_plan_unchanged():
    """With every opt-in off, the plan equals the cells verbatim -- no new axis alters it."""
    cells = _cells(["ConstPos", "ALL", "SlowDrift", "Sybil"], attacker_pct=0.2)
    plan = massive.build_domain_plan(cells, base_seed=7)
    assert plan == [dict(c) for c in cells]             # identical values + order
    # none of the opt-in fields leak into the default plan
    for e in plan:
        assert "strat_type" not in e and "strat_family" not in e and "difficulty_tier" not in e


def test_reporting_is_additive_and_default_run_is_unchanged(tmp_path, monkeypatch):
    """Default (report ON) vs --no-report differ ONLY by CORPUS_REPORT.md: same data digest, same
    catalog, and no dataset_robustness provenance leaks into the default manifest."""
    tiny = {"scenario": ["ConstPos", "ALL"], "weather": ["clear"], "rotate_period_s": [0.0],
            "collude_pct": [0.0], "faulty_pct": [0.0], "attacker_pct": [0.25], "n_vehicles": [40]}
    monkeypatch.setitem(massive.GRIDS, "tiny", tiny)

    on = tmp_path / "on"
    off = tmp_path / "off"
    assert massive.main(["--grid", "tiny", "--seed", "3", "--steps", "50", "--out", str(on)]) == 0
    assert massive.main(["--grid", "tiny", "--seed", "3", "--steps", "50", "--no-report",
                         "--out", str(off)]) == 0

    man_on = json.loads((on / "manifest.json").read_text())
    man_off = json.loads((off / "manifest.json").read_text())
    # the corpus itself is byte-identical whether or not the report is produced
    assert man_on["data_digest_sha256"] == man_off["data_digest_sha256"]
    # no opt-in robustness provenance appears in a plain run's manifest
    assert "dataset_robustness" not in man_on and "dataset_robustness" not in man_off

    # the ONLY difference in the output file set is the additive report file
    assert _rel_files(on) - _rel_files(off) == {"CORPUS_REPORT.md"}
    assert _rel_files(off) - _rel_files(on) == set()

    # a plain run's catalog carries none of the new opt-in fields
    cat = json.loads((on / "domain_catalog.json").read_text())
    for c in cat:
        assert "strat_type" not in c and "difficulty_tier" not in c
    # and the two runs' catalogs are identical
    assert cat == json.loads((off / "domain_catalog.json").read_text())
