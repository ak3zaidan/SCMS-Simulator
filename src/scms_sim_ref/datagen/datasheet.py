"""Generate a 'datasheet for datasets' (DATASHEET.md) for a generated dataset.

Aggregates provenance/config (manifest), composition (vehicle/attacker/faulty counts,
report-label and attack-type breakdowns), key feature distributions, the leakage-safe
split sizes, the revocation precision/recall (validate), and a baseline ML benchmark
(benchmark) into a single human-readable report shipped alongside the data.
"""

from __future__ import annotations

import collections
import json
import os

import numpy as np

from . import benchmark as bench
from . import validate as validate_mod


def _jsonl(path: str) -> list[dict]:
    if not os.path.exists(path):
        return []
    with open(path, encoding="utf-8") as fh:
        return [json.loads(x) for x in fh if x.strip()]


def _stats(xs: list[float]) -> str:
    if not xs:
        return "n/a"
    a = np.array(xs, dtype=float)
    p = np.percentile(a, [5, 50, 95])
    return f"mean {a.mean():.2f}, sd {a.std():.2f}, p5/p50/p95 {p[0]:.2f}/{p[1]:.2f}/{p[2]:.2f}"


def build(dataset_dir: str) -> str:
    man = {}
    mp = os.path.join(dataset_dir, "manifest.json")
    if os.path.exists(mp):
        man = json.load(open(mp, encoding="utf-8"))
    gt = os.path.join(dataset_dir, "ground_truth")
    ma = os.path.join(dataset_dir, "ma")
    veh = _jsonl(os.path.join(gt, "gt_vehicle.jsonl"))
    idm = _jsonl(os.path.join(gt, "gt_identity_map.jsonl"))
    reps = _jsonl(os.path.join(ma, "ma_reports.jsonl"))
    rlbl = _jsonl(os.path.join(gt, "gt_report_labels.jsonl"))
    atk = _jsonl(os.path.join(gt, "gt_attacks.jsonl"))
    emit = _jsonl(os.path.join(gt, "gt_emissions_sample.jsonl"))

    n_veh = len(veh)
    n_att = sum(bool(v.get("is_attacker")) for v in veh)
    n_fault = sum(bool(v.get("is_faulty")) for v in veh)
    pseudonyms = len(idm)
    correctness = collections.Counter(r.get("report_correctness") for r in rlbl)
    atk_types = collections.Counter(a.get("attack_type") for a in atk)
    atk_bases = collections.Counter(str(a.get("attack_type", "")).split("/")[0] for a in atk)

    try:
        vsum, _ = validate_mod.validate(dataset_dir)
    except Exception:
        vsum = {}
    try:
        bfull = bench.run(dataset_dir)
        bres = bfull.get("tasks", {})
        bgen = bfull.get("generalization", {})
    except Exception:
        bres, bgen = {}, {}

    scores = [float(r.get("detector_score", 0) or 0) for r in reps]
    confs = [float(r.get("subject_pos_confidence", 0) or 0) for r in reps]
    speeds = [float(e.get("claimed_speed", 0) or 0) for e in emit]
    falsified = sum(bool(e.get("falsified")) for e in emit)

    L = []
    L.append(f"# Datasheet — SCMS misbehaviour dataset `{man.get('simulation_id', os.path.basename(dataset_dir))}`\n")
    L.append("_Auto-generated datasheet-for-datasets. All figures are reproducible from the "
             "seed + config below._\n")

    L.append("## Provenance")
    L.append(f"- Generator: `{man.get('generator', 'scms_sim_ref (MOSAIC layer)')}`")
    L.append(f"- Seed: `{man.get('seed')}` (same seed + config -> byte-identical data)")
    L.append(f"- Data digest (SHA-256): `{str(man.get('data_digest_sha256', ''))[:32]}`")
    L.append(f"- SCMS entities modelled: {len(man.get('scms_entities', []))}")
    cfg = man.get("config", {})
    if cfg:
        L.append("- Configuration:")
        for k, v in cfg.items():
            L.append(f"    - `{k}` = {v}")
    L.append("")

    L.append("## Composition")
    L.append(f"- Vehicles: **{n_veh}**  (attackers **{n_att}**, faulty **{n_fault}**, "
             f"benign {n_veh - n_att - n_fault})")
    L.append(f"- Distinct pseudonym certificates (rotation): **{pseudonyms}** "
             f"({pseudonyms / n_veh:.1f} per vehicle)" if n_veh else "")
    L.append(f"- MA reports: **{len(reps)}**")
    L.append(f"- Report labels: " + ", ".join(f"{k} {v}" for k, v in correctness.most_common()))
    L.append(f"- Attack base behaviours present: **{len(atk_bases)}**; concrete variants: **{len(atk_types)}**")
    if atk_bases:
        L.append("- Attack mix (by base): " + ", ".join(f"{k} {v}" for k, v in atk_bases.most_common(12)))
    L.append("")

    L.append("## Feature distributions (MA-visible)")
    L.append(f"- Detector score: {_stats(scores)}")
    L.append(f"- Claimed position confidence (m): {_stats(confs)}")
    L.append(f"- Claimed speed on sampled CAMs (m/s): {_stats(speeds)}")
    if emit:
        L.append(f"- Per-message GT sample: {len(emit)} CAMs, {falsified} falsified "
                 f"({100 * falsified / len(emit):.1f}%)")
    L.append("")

    # --- realism scorecard: key distributions vs published real-world reference ranges ---
    L.append("## Realism scorecard (vs real-world reference ranges)")
    total_rep = max(1, len(rlbl))
    fp_rate = correctness.get("false_positive", 0) / total_rep
    benign_speeds = [float(e.get("claimed_speed", 0) or 0) for e in emit if not e.get("is_attacker")]
    checks = [
        ("GNSS 95% position confidence (m)", np.median(confs) if confs else None, 2.0, 20.0,
         "real urban GNSS horizontal accuracy"),
        ("Benign speed p95 (m/s)", float(np.percentile(benign_speeds, 95)) if benign_speeds else None,
         5.0, 45.0, "urban→motorway vehicle speeds"),
        ("Attacker prevalence", (n_att / n_veh) if n_veh else None, 0.03, 0.45,
         "misbehaviour-study attacker fractions"),
        ("Faulty prevalence", (n_fault / n_veh) if n_veh else None, 0.0, 0.20, "sensor-fault rates"),
        ("Report false-positive rate", fp_rate, 0.0, 0.30, "tuned MBD detector FP rates"),
    ]
    for name, val, lo, hi, ref in checks:
        ok = "✅" if (val is not None and lo <= val <= hi) else "⚠️"
        vs = f"{val:.3f}" if val is not None else "n/a"
        L.append(f"- {ok} {name}: **{vs}** (ref {lo}–{hi}; {ref})")
    L.append("")

    L.append("## Leakage-safe splits & revocation quality")
    L.append(f"- Split counts: {man.get('split_counts', vsum.get('split_counts', 'see ml/'))}")
    L.append(f"- Leakage violations: **{vsum.get('leakage_violations', 0)}** (a feature carrying a "
             "ground-truth column fails the build)")
    L.append(f"- Revocation precision **{vsum.get('precision')}** / recall **{vsum.get('recall')}** "
             f"(attackers {vsum.get('attackers')}, revoked {vsum.get('revoked')}, "
             f"false revocations {vsum.get('false_revocations')})")
    L.append("")

    L.append("## Baseline ML benchmark (logistic regression, train->test, vehicle-disjoint)")
    if not bres:
        L.append("- (no benchmark available)")
    for name, t in bres.items():
        if "note" in t:
            L.append(f"- **{name}**: {t['note']}")
            continue
        a5 = t.get("at_0.5", {})
        L.append(f"- **{name}**: ROC-AUC **{t.get('roc_auc')}**, PR-AUC **{t.get('pr_auc')}**, "
                 f"F1@0.5 {a5.get('f1')} (P {a5.get('precision')}/R {a5.get('recall')}); "
                 f"test n={t.get('n_test')}, positive rate {t.get('positive_rate_test')}")
    nov = (bgen or {}).get("vehicle_novel_attack")
    if nov:
        L.append(f"- **Novel-attack generalization** (leave-one-attack-family-out): mean ROC-AUC "
                 f"**{nov.get('mean_novel_attack_auc')}** — detecting attack families held out of "
                 f"training. Per family: " + ", ".join(f"{k} {v}" for k, v in nov.get("per_family_auc", {}).items()))
    L.append("\n_`vehicle_is_attacker` is the primary task — the MA's real post-linkage decision "
             "unit (pseudonyms linked via the LAs). `subject_is_attacker` is per-pseudonym and is "
             "deliberately hard: pseudonym rotation fragments each attacker across short-lived, often "
             "evasive certificates. `report_is_true_detection` is near-saturated because the tuned "
             "detector suite emits mostly-true reports. Recall < 1.0 (evasive attacks) is the main "
             "realistic difficulty._\n")

    edges = os.path.join(dataset_dir, "ml", "graph_edges.csv")
    wins = os.path.join(dataset_dir, "ml", "subject_windows.csv")
    if os.path.exists(edges) or os.path.exists(wins):
        L.append("## Graph & sequence exports (GNN / sequence models)")
        L.append("- `ml/graph_edges.*` — the MA's directed report graph (reporter→subject over opaque "
                 "linked entities, with time/reason/score_norm). Pair with `vehicle_features` as node "
                 "features + `vehicle_labels` as node labels for a GNN.")
        L.append("- `ml/subject_windows.*` — per-entity 10 s report-activity windows for sequence/"
                 "temporal models.")
        L.append("- `vehicle_features.reporter_mean_indegree` — a graph feature (are you flagged by "
                 "entities that are themselves heavily flagged? a collusion / reporter-reputation signal).")
        L.append("")

    L.append("## Intended use & limitations")
    L.append("- **Intended use:** training/evaluating global Misbehaviour-Authority anomaly "
             "detection; benchmarking V2X misbehaviour detectors. Features are MA-visible only.")
    L.append("- **Ground truth** (`ground_truth/`) is for evaluation only and must never be used "
             "as model input; true identities never appear in features.")
    L.append("- **Limitations:** SNS radio is a simplified channel model; demand is synthetic; "
             "attacks are parameterised (not human-adversarial). See README for the full model.")
    return "\n".join(x for x in L if x is not None) + "\n"


def main(argv: list[str] | None = None) -> int:
    import argparse
    p = argparse.ArgumentParser(description="Generate DATASHEET.md for a dataset.")
    p.add_argument("dataset_dir")
    a = p.parse_args(argv)
    md = build(a.dataset_dir)
    out = os.path.join(a.dataset_dir, "DATASHEET.md")
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(md)
    print(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
