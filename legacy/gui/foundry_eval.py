"""Evaluation harness: does the LLM semantic mutation operator BEAT plain random search?

The misbehavior foundry (:mod:`scms_sim_ref.datagen.foundry`) runs a MAP-Elites quality-diversity
search whose ONLY pluggable component is the mutation operator -- ``run_foundry(..., mutation_fn=None)``
uses the built-in deterministic random :func:`foundry.mutate`, while ``mutation_fn=<LLM operator>``
(see :func:`gui.agent.make_llm_mutation_fn`) lets a language model propose the next scenario. Everything
else (parent selection, validation, scoring, the archive's insert-if-better rule) is identical either
way. This module measures whether swapping in the LLM semantic operator actually produces a better
archive -- more coverage, higher QD-score, or a harder hardest-elite -- than random search.

This lives on the GUI side (``gui/foundry_eval.py``) on purpose: it legitimately imports
:mod:`gui.agent` for the LLM operator, and the foundry must never import gui. The engine and the
foundry are imported, never modified.

FAIR COMPARISON
    For each seed S the harness runs the foundry TWICE with the SAME ``(budget, seed, base_duration,
    objective)`` -- once with ``mutation_fn=None`` (random) and once with the LLM operator. The only
    difference is how the next genome is proposed, so any gap isolates the operator's contribution.
    Per-run output dirs are throwaway (we keep only the in-memory :class:`foundry.Archive`) and are
    cleaned up.

METRICS  (per archive)
    * coverage      -- number of filled descriptor cells (:meth:`Archive.coverage`)
    * qd_score      -- sum of elite fitnesses (:meth:`Archive.qd_score`)
    * best_fitness  -- the single hardest elite (max cell fitness)
    * mean_fitness  -- mean elite fitness
    * grid_size     -- the coverage denominator (:func:`foundry.grid_size`)
    * cells         -- the SET of filled cells (sorted 4-tuples -> lists, for JSON)

    Per seed we also report ``delta_coverage`` / ``delta_qd`` / ``delta_best`` (llm - random) and the
    targeted-gap-filling evidence ``llm_only_cells`` / ``random_only_cells`` (cells one operator filled
    that the other did not). Across seeds we report the mean of each metric per operator + a verdict.

DETERMINISM / HONESTY
    The random side is deterministic given its seed. The LLM side calls a live API, so it is NOT
    deterministic across real runs -- results are therefore reported as an AVERAGE over seeds, and a
    null/negative result (the operator does not help) is reported plainly, never fabricated into a win.
    The LLM operator is dependency-INJECTED via ``llm_operator_factory`` so the harness (and its whole
    test suite) runs OFFLINE with a scripted mock and never touches the network.

CLI (real operator; reads the OpenAI key from .env via :func:`agent.openai_key`)::

    python gui/foundry_eval.py --budget 20 --seeds 3,7,11 --duration 30 --objective evade \
        --out datasets/foundry_eval

    -> datasets/foundry_eval/REPORT.md + result.json (and the printed verdict)
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import sys
import tempfile
from pathlib import Path

# Make both the engine (src/) and the sibling gui module importable whether this file is run as a
# script (``python gui/foundry_eval.py``) or imported by the test suite. Mirrors gui/agent.py.
REPO = Path(__file__).resolve().parent.parent
for _p in (REPO / "src", REPO / "gui"):
    if str(_p) not in sys.path:
        sys.path.insert(0, str(_p))

from scms_sim_ref.datagen import foundry  # noqa: E402
import agent  # noqa: E402  (gui sibling: owns the LLM transport + make_llm_mutation_fn)


# --------------------------------------------------------------------------- #
# Operator factory + per-archive metrics
# --------------------------------------------------------------------------- #
def _default_llm_factory(key):
    """Default ``llm_operator_factory``: the REAL LLM semantic operator bound to ``key``.

    Called as ``factory(key)``; binds the key by KEYWORD so it maps to ``make_llm_mutation_fn``'s
    ``key`` parameter (its first positional is ``chat_fn``). Referenced as a module attribute so tests
    can monkeypatch ``agent.make_llm_mutation_fn`` to a mock and have this pick it up.
    """
    return agent.make_llm_mutation_fn(key=key)


def _cells_as_lists(cells) -> list:
    """Descriptor cells (4-tuples of strings) -> a sorted list of JSON-friendly lists."""
    return [list(c) for c in sorted(cells)]


def _archive_metrics(archive) -> dict:
    """Compact, JSON-serializable metric snapshot of one :class:`foundry.Archive`."""
    fits = [c["fitness"] for c in archive.cells.values()]
    return {
        "coverage": archive.coverage(),                                  # filled descriptor cells
        "qd_score": archive.qd_score(),                                  # sum of elite fitnesses
        "best_fitness": round(max(fits), 6) if fits else 0.0,           # hardest elite
        "mean_fitness": round(statistics.mean(fits), 6) if fits else 0.0,
        "grid_size": foundry.grid_size(),                               # coverage denominator
        "cells": _cells_as_lists(archive.cells.keys()),                # the filled cell SET
    }


# --------------------------------------------------------------------------- #
# Comparison
# --------------------------------------------------------------------------- #
def compare_operators(budget, seed, base_duration=30.0, objective="evade", seeds=None,
                      out_dir=None, key=None, llm_operator_factory=None, verbose=False) -> dict:
    """Run the foundry with the random vs the LLM operator under matched budgets and return a verdict.

    For every seed (``seeds`` if given, else the single ``seed``), the foundry is run twice with the
    SAME ``(budget, seed, base_duration, objective)`` -- once with ``mutation_fn=None`` (random) and
    once with ``llm_operator_factory(key)`` (the injected LLM operator). The only difference is the
    operator, so each delta isolates its contribution.

    ``llm_operator_factory`` is a callable ``(key) -> mutation_fn``. The default (``None``) is the REAL
    LLM operator (:func:`agent.make_llm_mutation_fn`); TESTS pass a mock factory so no network is
    touched -- this injection is what keeps the harness offline-testable.

    Returns a JSON-serializable verdict dict (tuples -> lists) with the documented keys::

        {
          "llm_wins_coverage": bool,   # mean llm coverage  > mean random coverage
          "llm_wins_qd":       bool,   # mean llm qd_score  > mean random qd_score
          "llm_wins_best":     bool,   # mean llm best_fit  > mean random best_fit
          "n_seeds": int, "budget": int, "objective": str,
          "mean": {"random": {coverage, qd_score, best_fitness, mean_fitness, grid_size},
                   "llm":    {coverage, qd_score, best_fitness, mean_fitness, grid_size}},
          "per_seed": [ {seed, random:{...metrics...}, llm:{...metrics...},
                         delta_coverage, delta_qd, delta_best,
                         llm_only_cells, random_only_cells}, ... ],
        }
    """
    if llm_operator_factory is None:
        llm_operator_factory = _default_llm_factory
    seed_list = list(seeds) if seeds is not None else [seed]

    # A throwaway root for the per-run foundry outputs; we only keep the returned Archive objects.
    if out_dir is not None:
        os.makedirs(out_dir, exist_ok=True)
        tmp_root = tempfile.mkdtemp(prefix="foundry_eval_", dir=out_dir)
    else:
        tmp_root = tempfile.mkdtemp(prefix="foundry_eval_")

    per_seed: list[dict] = []
    try:
        for s in seed_list:
            # --- RANDOM side (deterministic: mutation_fn=None) --------------------------------- #
            r_dir = os.path.join(tmp_root, f"seed{s}_random")
            random_archive = foundry.run_foundry(
                budget=budget, seed=s, base_duration=base_duration, out_dir=r_dir,
                objective=objective, verbose=verbose, mutation_fn=None)
            random_metrics = _archive_metrics(random_archive)
            random_cells = set(random_archive.cells.keys())
            shutil.rmtree(r_dir, ignore_errors=True)

            # --- LLM side (SAME budget/seed/duration/objective; only the operator differs) ----- #
            l_dir = os.path.join(tmp_root, f"seed{s}_llm")
            llm_archive = foundry.run_foundry(
                budget=budget, seed=s, base_duration=base_duration, out_dir=l_dir,
                objective=objective, verbose=verbose, mutation_fn=llm_operator_factory(key))
            llm_metrics = _archive_metrics(llm_archive)
            llm_cells = set(llm_archive.cells.keys())
            shutil.rmtree(l_dir, ignore_errors=True)

            per_seed.append({
                "seed": s,
                "random": random_metrics,
                "llm": llm_metrics,
                "delta_coverage": llm_metrics["coverage"] - random_metrics["coverage"],
                "delta_qd": round(llm_metrics["qd_score"] - random_metrics["qd_score"], 6),
                "delta_best": round(llm_metrics["best_fitness"] - random_metrics["best_fitness"], 6),
                # targeted-gap-filling evidence: cells one operator reached that the other did not
                "llm_only_cells": _cells_as_lists(llm_cells - random_cells),
                "random_only_cells": _cells_as_lists(random_cells - llm_cells),
            })
    finally:
        shutil.rmtree(tmp_root, ignore_errors=True)

    # --- aggregate: mean of each metric per operator ------------------------------------------- #
    def _mean(op: str, metric: str) -> float:
        return round(statistics.mean([ps[op][metric] for ps in per_seed]), 6)

    grid = foundry.grid_size()
    mean = {op: {"coverage": _mean(op, "coverage"), "qd_score": _mean(op, "qd_score"),
                 "best_fitness": _mean(op, "best_fitness"), "mean_fitness": _mean(op, "mean_fitness"),
                 "grid_size": grid}
            for op in ("random", "llm")}

    return {
        "llm_wins_coverage": mean["llm"]["coverage"] > mean["random"]["coverage"],
        "llm_wins_qd": mean["llm"]["qd_score"] > mean["random"]["qd_score"],
        "llm_wins_best": mean["llm"]["best_fitness"] > mean["random"]["best_fitness"],
        "n_seeds": len(seed_list),
        "budget": budget,
        "objective": objective,
        "mean": mean,
        "per_seed": per_seed,
    }


# --------------------------------------------------------------------------- #
# Report
# --------------------------------------------------------------------------- #
def render_report(result) -> str:
    """Render a :func:`compare_operators` result as a human-readable Markdown report string."""
    mr, ml = result["mean"]["random"], result["mean"]["llm"]

    def _yn(b: bool) -> str:
        return "YES" if b else "NO"

    L: list[str] = []
    L.append("# Foundry operator evaluation: LLM semantic mutation vs random search")
    L.append("")
    L.append("Does the LLM semantic mutation operator beat plain random search in the misbehavior "
             "foundry's MAP-Elites quality-diversity loop? Both sides run the **same budget "
             f"({result['budget']}), the same seed(s), the same duration and the same objective "
             f"(`{result['objective']}`)** -- the ONLY difference is the mutation operator, so every "
             "gap below isolates the operator's contribution.")
    L.append("")
    L.append(f"- Seeds: **{result['n_seeds']}**  |  Budget per run: **{result['budget']}**  |  "
             f"Objective: **{result['objective']}**")
    L.append("")

    L.append("## Per-seed results")
    L.append("")
    L.append("| seed | cov (rand) | cov (llm) | Δcov | QD (rand) | QD (llm) | ΔQD | "
             "best (rand) | best (llm) | Δbest |")
    L.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for ps in result["per_seed"]:
        r, l = ps["random"], ps["llm"]
        L.append(f"| {ps['seed']} | {r['coverage']} | {l['coverage']} | {ps['delta_coverage']:+d} | "
                 f"{r['qd_score']:.4f} | {l['qd_score']:.4f} | {ps['delta_qd']:+.4f} | "
                 f"{r['best_fitness']:.4f} | {l['best_fitness']:.4f} | {ps['delta_best']:+.4f} |")
    L.append("")

    L.append("## Mean across seeds")
    L.append("")
    L.append("| operator | coverage | QD-score | best_fitness | mean_fitness |")
    L.append("|---|---:|---:|---:|---:|")
    L.append(f"| random | {mr['coverage']:.4f} | {mr['qd_score']:.4f} | {mr['best_fitness']:.4f} | "
             f"{mr['mean_fitness']:.4f} |")
    L.append(f"| llm    | {ml['coverage']:.4f} | {ml['qd_score']:.4f} | {ml['best_fitness']:.4f} | "
             f"{ml['mean_fitness']:.4f} |")
    L.append("")

    L.append("## Verdict")
    L.append("")
    L.append(f"- **Coverage** (more descriptor cells filled): {_yn(result['llm_wins_coverage'])} "
             f"-- llm {ml['coverage']} vs random {mr['coverage']} (mean over {result['n_seeds']} "
             f"seed(s)).")
    L.append(f"- **QD-score** (sum of elite fitnesses): {_yn(result['llm_wins_qd'])} "
             f"-- llm {ml['qd_score']} vs random {mr['qd_score']}.")
    L.append(f"- **Hardest elite** (max cell fitness): {_yn(result['llm_wins_best'])} "
             f"-- llm {ml['best_fitness']} vs random {mr['best_fitness']}.")
    L.append("")
    wins = (result["llm_wins_coverage"], result["llm_wins_qd"], result["llm_wins_best"])
    labels = ("coverage", "QD-score", "hardest elite")
    if all(wins):
        L.append("**The LLM semantic operator beats random search on coverage, QD-score AND the "
                 "hardest elite at this budget.** Because both sides share the identical "
                 "budget/seed/duration/objective, the semantic operator is the cause of the gap.")
    elif any(wins):
        won = ", ".join(n for n, w in zip(labels, wins) if w)
        lost = ", ".join(n for n, w in zip(labels, wins) if not w)
        L.append(f"**Mixed result:** the LLM operator wins on {won} but not on {lost} at this "
                 "budget. Reported as a per-seed average because the LLM side is non-deterministic.")
    else:
        L.append("**No advantage detected:** the LLM operator does not beat random search on "
                 "coverage, QD-score or the hardest elite at this budget. This is a valid, honest "
                 "null/negative result -- not a failure to report. A larger budget or more seeds may "
                 "be needed before drawing a conclusion; the harness does not fabricate a win.")
    L.append("")

    L.append("## Notes on method")
    L.append("")
    L.append("- The LLM side is **non-deterministic** (it calls a live API), so the numbers are "
             "reported as an **average over seeds**; the random side is deterministic given its seed.")
    L.append("- Both operators share the **same budget, seed, base duration and objective**, and every "
             "candidate is validated + scored + inserted by the SAME foundry code -- only the proposal "
             "of the next genome differs, so any gap isolates the operator.")
    L.append("- `llm_only_cells` / `random_only_cells` in `result.json` list the descriptor cells each "
             "operator filled that the other did not -- the targeted-gap-filling evidence.")
    L.append("")
    return "\n".join(L)


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Measure whether the LLM semantic mutation operator beats plain random search in "
                    "the SCMS foundry's MAP-Elites QD loop (a fair, budget/seed/duration/objective-"
                    "matched head-to-head; the only difference is the mutation operator).")
    ap.add_argument("--budget", type=int, default=20, help="foundry search iterations PER run")
    ap.add_argument("--seed", type=int, default=7, help="master seed (single-seed mode)")
    ap.add_argument("--seeds", default=None,
                    help="comma-separated seeds, e.g. '3,7,11' (overrides --seed; averaged over them)")
    ap.add_argument("--duration", type=float, default=30.0, help="per-scenario sim seconds (flow)")
    ap.add_argument("--objective", default="evade", help="evade | family:<F> | latency")
    ap.add_argument("--out", default="datasets/foundry_eval",
                    help="output dir (REPORT.md + result.json)")
    ap.add_argument("--verbose", action="store_true", help="print foundry progress")
    a = ap.parse_args(argv)

    seeds = None
    if a.seeds:
        seeds = [int(x.strip()) for x in a.seeds.split(",") if x.strip()]

    out = a.out
    os.makedirs(out, exist_ok=True)
    key = agent.openai_key()                       # real operator; key from repo-root .env
    result = compare_operators(budget=a.budget, seed=a.seed, base_duration=a.duration,
                               objective=a.objective, seeds=seeds, out_dir=out, key=key,
                               verbose=a.verbose)

    with open(os.path.join(out, "REPORT.md"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write(render_report(result))
    with open(os.path.join(out, "result.json"), "w", encoding="utf-8") as fh:
        json.dump(result, fh, indent=2)
        fh.write("\n")

    m = result["mean"]
    print(f"[foundry-eval] objective={result['objective']} seeds={result['n_seeds']} "
          f"budget={result['budget']}")
    print(f"[foundry-eval] coverage: llm={m['llm']['coverage']} vs random={m['random']['coverage']} "
          f"-> llm_wins_coverage={result['llm_wins_coverage']}")
    print(f"[foundry-eval] QD-score: llm={m['llm']['qd_score']} vs random={m['random']['qd_score']} "
          f"-> llm_wins_qd={result['llm_wins_qd']}")
    print(f"[foundry-eval] best:     llm={m['llm']['best_fitness']} vs "
          f"random={m['random']['best_fitness']} -> llm_wins_best={result['llm_wins_best']}")
    if not key:
        print("[foundry-eval] WARNING: no OPENAI_API_KEY found -- the 'llm' side ran on the random "
              "FALLBACK operator, so this is NOT a real LLM comparison (deltas will be ~0).")
    print(f"[foundry-eval] wrote {os.path.join(out, 'REPORT.md')} and "
          f"{os.path.join(out, 'result.json')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
