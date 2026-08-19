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
from . import calibration as calib
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
    cst = _jsonl(os.path.join(ma, "ma_cert_status.jsonl"))

    n_veh = len(veh)
    n_att = sum(bool(v.get("is_attacker")) for v in veh)
    n_fault = sum(bool(v.get("is_faulty")) for v in veh)
    # benign = neither attacker nor faulty (don't assume the two flags are disjoint)
    n_benign = sum(1 for v in veh if not v.get("is_attacker") and not v.get("is_faulty"))
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
    except Exception:
        bfull = {}
    bres = bfull.get("tasks", {})
    bgen = bfull.get("generalization", {})

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
             f"benign {n_benign})")
    L.append(f"- Distinct pseudonym certificates (rotation): **{pseudonyms}** "
             f"({pseudonyms / n_veh:.1f} per vehicle)" if n_veh else "")
    L.append(f"- MA reports: **{len(reps)}**")
    L.append(f"- Report labels: " + ", ".join(f"{k} {v}" for k, v in correctness.most_common()))
    L.append(f"- Attack base behaviours present: **{len(atk_bases)}**; concrete variants: **{len(atk_types)}**")
    if atk_bases:
        L.append("- Attack mix (by base): " + ", ".join(f"{k} {v}" for k, v in atk_bases.most_common(12)))
    from .featurize import _ATTACK_FAMILY
    fam_mix = collections.Counter(_ATTACK_FAMILY.get(b, "other") for b in
                                  (str(a.get("attack_type", "")).split("/")[0] for a in atk))
    if fam_mix:
        L.append("- Attack mix (by family): " + ", ".join(f"{k} {v}" for k, v in fam_mix.most_common()))
    fleet_mix = collections.Counter(v.get("veh_type", "car") for v in veh)
    if len(fleet_mix) > 1:
        L.append("- Fleet composition: " + ", ".join(f"{k} {v}" for k, v in fleet_mix.most_common()))
    L.append("")

    # --- traffic flow & congestion (present for flow-mode / routed runs) ---
    is_flow = bool(cfg.get("traffic_flow")) or (len({round(v.get("spawn_time", 0.0), 1) for v in veh}) > 3)
    if is_flow and cst:
        ev = sorted([(c["first_seen"], 1) for c in cst if "first_seen" in c]
                    + [(c.get("last_seen", c.get("first_seen", 0)), -1) for c in cst])
        cur = peak = 0
        for _t, d in ev:
            cur += d
            peak = max(peak, cur)
        spawns = sorted(v.get("spawn_time", 0.0) for v in veh)
        span = (spawns[-1] - spawns[0]) if len(spawns) > 1 else 0.0
        benign_sp = [float(e.get("claimed_speed", 0) or 0) for e in emit if not e.get("is_attacker")]
        L.append("## Traffic flow & congestion")
        L.append(f"- Total vehicles over the run: **{n_veh}**; peak concurrent (approx): **{peak}** "
                 f"(steady-state turnover)")
        if span > 0:
            L.append(f"- Arrivals span {span:.0f} s ({n_veh / max(1.0, span):.2f} vehicles/s mean)")
        if benign_sp:
            slowed = sum(1 for s in benign_sp if s < 5.0) / len(benign_sp)
            L.append(f"- Benign speed (m/s): {_stats(benign_sp)}; queued (<5 m/s): {100 * slowed:.0f}% "
                     f"(car-following congestion)")
        # which mobility/attack realism features were active for this dataset (self-documenting)
        feats = []
        if cfg.get("n_lanes", 1) > 1: feats.append(f"{cfg['n_lanes']} lanes")
        if cfg.get("traffic_lights"): feats.append("traffic lights")
        if cfg.get("turn_slowdown"): feats.append("corner slowdown")
        if cfg.get("od_model") == "gravity": feats.append("gravity trip lengths")
        if cfg.get("demand_profile", "uniform") != "uniform": feats.append(f"{cfg['demand_profile']} demand")
        if cfg.get("attack_duty_cycle", 1.0) < 1.0: feats.append(f"pulsed attacks (duty {cfg['attack_duty_cycle']})")
        if cfg.get("attack_delay_jitter_s", 0.0) > 0: feats.append(f"onset jitter {cfg['attack_delay_jitter_s']}s")
        if cfg.get("attack_intensity", 1.0) != 1.0: feats.append(f"attack intensity {cfg['attack_intensity']}")
        if feats:
            L.append("- Realism features active: " + ", ".join(feats))
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
    benign_speeds = [float(e.get("claimed_speed", 0) or 0) for e in emit
                     if not e.get("is_attacker") and not e.get("is_faulty")]
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
    # quantitative calibration vs literature reference distributions
    try:
        cal = calib.calibrate(dataset_dir)
    except Exception:
        cal = {}
    ge = cal.get("gps_error")
    if ge:
        ok = "✅" if ge.get("cep_in_real_range") and ge.get("shape_is_rayleigh_like") else "⚠️"
        L.append(f"- {ok} GNSS error fit: CEP50 **{ge['fitted_cep_m']} m** (ref {ge['ref_cep_m']} m), "
                 f"KS vs Rayleigh core **{ge['ks_vs_fitted_rayleigh']}**, KS vs literature model "
                 f"**{ge['ks_vs_reference_rayleigh']}**")
    cc = cal.get("confidence_calibration")
    if cc:
        ok = "✅" if cc.get("well_calibrated") else "⚠️"
        L.append(f"- {ok} Confidence calibration: 95% radius empirically covers "
                 f"**{cc['empirical_coverage_of_95pct_radius']}** of benign error (target 0.95)")
    L.append("")

    L.append("## Leakage-safe splits & revocation quality")
    L.append(f"- Split counts: {man.get('split_counts', vsum.get('split_counts', 'see ml/'))}")
    L.append(f"- Leakage violations: **{vsum.get('leakage_violations', 0)}** (a feature carrying a "
             "ground-truth column fails the build)")
    L.append(f"- Revocation precision **{vsum.get('precision')}** / recall **{vsum.get('recall')}** "
             f"(attackers {vsum.get('attackers')}, revoked {vsum.get('revoked')}, "
             f"false revocations {vsum.get('false_revocations')}, faulty revocations "
             f"{vsum.get('faulty_revocations')})")
    dl = vsum.get("detection_latency_s") or {}
    if dl:
        L.append(f"- Detection latency (onset→revocation): median **{dl.get('median_s')} s**, "
                 f"p90 {dl.get('p90_s')} s, max {dl.get('max_s')} s (n={dl.get('n')})")
    rbf = vsum.get("recall_by_family") or {}
    if rbf:
        L.append("- Recall by attack family (the difficulty profile): "
                 + ", ".join(f"{f} {v}" for f, v in sorted(rbf.items(), key=lambda kv: kv[1])))
    rbt = vsum.get("recall_by_type") or {}
    if rbt:
        hardest = sorted(rbt.items(), key=lambda kv: kv[1]["recall"])[:6]
        L.append("- Hardest attack types (lowest recall): "
                 + ", ".join(f"{t} {d['recall']} (n={d['n']})" for t, d in hardest))
    dr = vsum.get("detector_reliability") or {}
    if dr:
        L.append("- Per-detector reliability (report-level precision @ coverage — which reason codes "
                 "carry trustworthy signal): "
                 + ", ".join(f"{c} {d['precision']}@{d['reports']}"
                             for c, d in sorted(dr.items(), key=lambda kv: -kv[1]["precision"])))
    rc = vsum.get("rsu_contribution") or {}
    if rc:
        L.append(f"- RSU (infrastructure) contribution: {rc['reports']} reports from "
                 f"{rc['rsus_reporting']} RSUs at precision {rc['precision']}")
    L.append("")

    L.append("## Baseline ML benchmark (logreg vs GBDT, train->test, vehicle-disjoint)")
    L.append("*Two baselines per task: logistic regression (linear separability) and gradient-boosted "
             "trees (strong non-linear tabular learner). 95% bootstrap CI on ROC-AUC.*")
    if not bres:
        L.append("- (no benchmark available)")
    for name, t in bres.items():
        if "note" in t:
            L.append(f"- **{name}**: {t['note']}")
            continue
        a5 = t.get("at_0.5", {})
        ci = t.get("roc_auc_ci95")
        cistr = f" (95% CI {ci[0]}–{ci[1]})" if ci else ""
        L.append(f"- **{name}**: logreg ROC-AUC **{t.get('roc_auc')}**{cistr}, PR-AUC {t.get('pr_auc')}, "
                 f"recall@1%FPR {t.get('recall_at_1pct_fpr')}, balanced-acc {a5.get('balanced_accuracy')}; "
                 f"test n={t.get('n_test')}, positive rate {t.get('positive_rate_test')}")
        bf = t.get("at_best_f1")
        if bf:
            L.append(f"    - best-F1 operating point: F1 {bf.get('f1')} "
                     f"(precision {bf.get('precision')}, recall {bf.get('recall')} @ thr {bf.get('threshold')})")
        g = t.get("gbdt")
        if g and g.get("roc_auc") is not None:
            gci = g.get("roc_auc_ci95")
            gcistr = f" (95% CI {gci[0]}–{gci[1]})" if gci else ""
            L.append(f"    - GBDT ROC-AUC **{g.get('roc_auc')}**{gcistr}, PR-AUC {g.get('pr_auc')}, "
                     f"recall@1%FPR {g.get('recall_at_1pct_fpr')}, balanced-acc {g.get('balanced_accuracy')}")
        tf = t.get("top_features")
        if tf:
            L.append("    - Top features (|logreg coef|): "
                     + ", ".join(f"{c} {w:+.2f}" for c, w in tf[:6]))
    oc = bfull.get("anomaly_baseline_unsupervised")
    if oc:
        L.append(f"- **Unsupervised anomaly baseline** (one-class Mahalanobis, trained on BENIGN only): "
                 f"ROC-AUC **{oc.get('roc_auc')}**, PR-AUC {oc.get('pr_auc')}, "
                 f"recall@1%FPR {oc.get('recall_at_1pct_fpr')} — the realistic label-scarce MA setting.")
    tmp = (bgen or {}).get("vehicle_temporal")
    if tmp:
        L.append(f"- **Forward-in-time generalization** (train earlier vehicles → test later): "
                 f"ROC-AUC **{tmp.get('roc_auc')}**, recall@1%FPR {tmp.get('recall_at_1pct_fpr')}.")
    nov = (bgen or {}).get("vehicle_novel_attack")
    if nov:
        L.append(f"- **Novel-attack generalization** (leave-one-attack-family-out): mean ROC-AUC "
                 f"**{nov.get('mean_novel_attack_auc')}** — detecting attack families held out of "
                 f"training. Per family: " + ", ".join(f"{k} {v}" for k, v in nov.get("per_family_auc", {}).items()))
    L.append("\n_Three classification units span a realism spectrum. `subject_is_attacker` "
             "(per-pseudonym) is the MA-realistic FLOOR and is deliberately hard: pseudonym rotation "
             "fragments each attacker across short-lived, often evasive certificates. "
             "`vehicle_is_attacker_ma_linked` groups pseudonyms by what the MA can actually link "
             "(co-revocation only, no oracle) — the realistic headline. `vehicle_is_attacker` uses "
             "the oracle identity map (PERFECT linkage) and is an optimistic UPPER BOUND, not an "
             "MA-achievable number. `report_is_true_detection` is near-saturated because the tuned "
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
        gb = bfull.get("graph_baseline")
        if gb:
            L.append(f"- **Graph baseline** (1-hop message passing vs node features): node-only ROC-AUC "
                     f"**{gb.get('node_only_auc')}** → node+graph **{gb.get('node_plus_graph_auc')}** "
                     f"(does the report-graph topology add signal?).")
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
