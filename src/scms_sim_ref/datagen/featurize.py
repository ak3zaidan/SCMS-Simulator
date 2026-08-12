"""Turn an MA-visible + ground-truth dataset into ML-ready tables.

Consumes a dataset directory (``ma/*.jsonl`` + ``ground_truth/*.jsonl``) produced by
either the Python reference pipeline or the MOSAIC layer, and produces leakage-safe,
split-assigned ML tables under ``<dataset>/ml/``:

    report_features.parquet   (+ .csv)   MA-derived features per misbehaviour report
    report_labels.parquet                labels + true ids (eval only)
    subject_features.parquet             MA-derived aggregates per subject certificate
    subject_labels.parquet               subject labels (is_attacker, was_revoked) + true id

Design guarantees enforced here:
  * **Physical feature/label separation** — features and labels are written to
    different files; only ``*_features.*`` should feed a model.
  * **Leakage firewall** — every feature column is checked with the leakage linter;
    a ground-truth field name in a feature table raises and fails the build.
  * **Leakage-safe splits** — train/val/test are grouped by *true vehicle*, so no
    vehicle's reports span splits. The split is a deterministic hash of the vehicle id.
  * The MA's own revocation decision (``crl_status``) is deliberately NOT a feature for
    the "is this subject an attacker" task (it would be target leakage); it is kept only
    as an eval-side outcome.
"""

from __future__ import annotations

import hashlib
import json
import os
from typing import Any

import pandas as pd

from .leakage_linter import lint_feature_frame

REASON_VOCAB = [
    "constantPositionFrozen",
    "positionSpeedInconsistency",
    "positionSpeedConsistency",   # mock-pipeline alias
    "acceptanceRangeThreshold",
    "positionJump",
    "sybilCoLocation",
    "headingInconsistency",
    "implausibleAcceleration",
    "staleOrReplay",
    "beaconFrequency",
]


# Attack base -> family (mirrors org.scms.attacks.AttackLib). Used only for LABELS, so a model
# can be evaluated on attack families it never trained on (real-world novel-attack generalization).
_ATTACK_FAMILY = {}
for _fam, _bases in {
    "position": ["ConstPos", "ConstPosOffset", "ConstPosOffsetX", "ConstPosOffsetY", "RandomPos",
                 "RandomPosOffset", "Teleport", "SineWavePos"],
    "speed": ["ConstSpeedZero", "ConstSpeedOffset", "ConstSpeedHigh", "RandomSpeed",
              "RandomSpeedOffset", "StopAndGo"],
    "heading": ["ReversedHeading", "RandomHeading", "PerpendicularHeading", "HeadingOffset"],
    "combined": ["Disruptive", "PosSpeedInconsistent", "PosHeadingInconsistent", "EventualStop"],
    "timing": ["DataReplay", "DelayedMessages", "OutOfOrder", "DoS", "DoSRandom"],
    "identity": ["Sybil"],
    "stealth": ["AlongRoadOffset", "SlowDrift", "LaggingPosition"],
}.items():
    for _b in _bases:
        _ATTACK_FAMILY[_b] = _fam


def _load_jsonl(path: str) -> list[dict]:
    if not os.path.exists(path):
        return []
    with open(path, encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def _report_fields(r: dict) -> tuple[float, bool, list[str], str]:
    """Normalize report fields across the mock and MOSAIC schemas."""
    score = r.get("detector_score")
    if score is None:
        outs = r.get("detector_outputs") or []
        score = outs[0].get("score") if outs else 0.0
    sig = r.get("sig_valid")
    if sig is None:
        sig = (r.get("cert_validity") or {}).get("sig_valid", True)
    reasons = r.get("reason_codes") or []
    crl = r.get("cert_crl_status", "active")
    return float(score or 0.0), bool(sig), list(reasons), str(crl)


def _split_for(seed: int, vehicle_id: str) -> str:
    bucket = int(hashlib.sha256(f"{seed}|{vehicle_id}".encode()).hexdigest(), 16) % 100
    if bucket < 70:
        return "train"
    if bucket < 85:
        return "val"
    return "test"


def build(dataset_dir: str, split_seed: int = 1234) -> dict[str, Any]:
    ma = os.path.join(dataset_dir, "ma")
    gt = os.path.join(dataset_dir, "ground_truth")
    reports = _load_jsonl(os.path.join(ma, "ma_reports.jsonl"))
    cert_status = _load_jsonl(os.path.join(ma, "ma_cert_status.jsonl"))
    gt_idmap = _load_jsonl(os.path.join(gt, "gt_identity_map.jsonl"))
    gt_vehicle = _load_jsonl(os.path.join(gt, "gt_vehicle.jsonl"))
    gt_report_labels = _load_jsonl(os.path.join(gt, "gt_report_labels.jsonl"))

    # --- ground-truth lookups (used for LABELS + split grouping only) ---
    gt_attacks = _load_jsonl(os.path.join(gt, "gt_attacks.jsonl"))
    attacker_by_vehicle = {v["true_vehicle_id"]: bool(v["is_attacker"]) for v in gt_vehicle}
    faulty_by_vehicle = {v["true_vehicle_id"]: bool(v.get("is_faulty", False)) for v in gt_vehicle}
    base_by_vehicle = {a["true_vehicle_id"]: str(a.get("attack_type", "")).split("/")[0] for a in gt_attacks}
    family_by_vehicle = {v: _ATTACK_FAMILY.get(b, "other") for v, b in base_by_vehicle.items()}
    vehicle_by_digest = {m["pseudonym_cert_digest"]: m["true_vehicle_id"] for m in gt_idmap}
    label_by_report = {r["report_id"]: r for r in gt_report_labels}
    lifetime_by_digest = {
        c["cert_digest"]: (float(c.get("last_seen", 0)) - float(c.get("first_seen", 0)))
        for c in cert_status
    }
    revoked_by_digest = {c["cert_digest"]: (c.get("crl_status") == "revoked") for c in cert_status}

    # --- report-level tables ---
    rf_rows, rl_rows = [], []
    for r in reports:
        score, sig, reasons, crl = _report_fields(r)
        subj_digest = r.get("subject_cert_digest")
        subj_true = vehicle_by_digest.get(subj_digest, "unknown")
        split = _split_for(split_seed, subj_true)
        feat = {
            "report_id": r["report_id"],
            "detection_time": float(r.get("detection_time", 0)),   # for ordering per-subject sequences
            "detector_score": score,
            "detector_score_norm": float(r.get("detector_score_norm", 0.0) or 0.0),
            "sig_valid": int(sig),
            "ingest_delay": round(float(r.get("ingest_time", 0)) - float(r.get("detection_time", 0)), 4),
            "crl_active_at_report": int(crl == "active"),
            "pos_confidence": float(r.get("subject_pos_confidence", 0.0) or 0.0),
            "split": split,
        }
        for name in REASON_VOCAB:
            feat[f"reason_{name}"] = int(name in reasons)
        feat["reason_other"] = int(not any(n in REASON_VOCAB for n in reasons))
        rf_rows.append(feat)

        gl = label_by_report.get(r["report_id"], {})
        rl_rows.append({
            "report_id": r["report_id"],
            "label_correctness": gl.get("report_correctness", "unknown"),
            "label_subject_is_attacker": int(attacker_by_vehicle.get(subj_true, False)),
            "subject_true_id": subj_true,
            "split": split,
        })

    # --- subject-window tables ---
    # The subject population is EVERY certificate the MA observed (from ma_cert_status),
    # not only the reported ones — so benign vehicles appear as 0-report negatives and the
    # subject-level classification task has both classes.
    by_subject: dict[str, list[dict]] = {}
    for r in reports:
        by_subject.setdefault(r["subject_cert_digest"], []).append(r)
    all_digests = [c["cert_digest"] for c in cert_status]
    for d in by_subject:
        if d not in lifetime_by_digest:
            all_digests.append(d)  # reported cert not in cert_status (defensive)
    sf_rows, sl_rows = [], []
    for digest in all_digests:
        rs = by_subject.get(digest, [])
        scores = [_report_fields(r)[0] for r in rs]
        norms = [float(r.get("detector_score_norm", 0.0) or 0.0) for r in rs]
        times = [float(r.get("detection_time", 0)) for r in rs]
        reporters = {r.get("reporter_cert_digest") for r in rs}
        n_reasons = len({c for r in rs for c in (r.get("reason_codes") or ["?"])})
        subj_true = vehicle_by_digest.get(digest, "unknown")
        split = _split_for(split_seed, subj_true)
        sf_rows.append({
            "subject_cert_digest": digest,
            "n_reports": len(rs),
            "n_distinct_reporters": len(reporters),
            "n_distinct_reasons": n_reasons,
            "score_mean": sum(scores) / len(scores) if scores else 0.0,
            "score_max": max(scores) if scores else 0.0,
            "score_norm_mean": sum(norms) / len(norms) if norms else 0.0,
            "score_norm_max": max(norms) if norms else 0.0,
            "report_span_s": (max(times) - min(times)) if times else 0.0,
            "reports_per_reporter": len(rs) / max(1, len(reporters)),
            "cert_lifetime_s": float(lifetime_by_digest.get(digest, 0.0)),
            "split": split,
        })
        sl_rows.append({
            "subject_cert_digest": digest,
            "label_is_attacker": int(attacker_by_vehicle.get(subj_true, False)),
            "label_is_faulty": int(faulty_by_vehicle.get(subj_true, False)),
            "label_was_revoked": int(bool(revoked_by_digest.get(digest, False))),
            "true_vehicle_id": subj_true,
            "split": split,
        })

    # --- vehicle-level tables (the MA's post-linkage decision unit) ---
    # A vehicle's pseudonyms are linked via the LAs, so MA-visible reports are aggregated per true
    # vehicle. Per-pseudonym subjects are ill-posed under rotation (an attacker's evasive or
    # post-revocation pseudonyms carry no signal yet are attacker-labelled). The feature table is
    # keyed by an OPAQUE linked-entity id (never the true id — that would be label leakage); the
    # label table carries the true id for evaluation.
    reports_by_vehicle: dict[str, list[dict]] = {}
    for r in reports:
        tv = vehicle_by_digest.get(r.get("subject_cert_digest"), "unknown")
        if tv != "unknown":
            reports_by_vehicle.setdefault(tv, []).append(r)
    # temporal (forward-in-time) split: each entity is placed by its FIRST report time; the latest
    # ~30% become the temporal test set, so a model can be evaluated on whether it detects
    # misbehaviour in the FUTURE after training on the past (the deployment question).
    first_t = {tv: min(float(r.get("detection_time", 0)) for r in rs)
               for tv, rs in reports_by_vehicle.items() if rs}
    _ts_sorted = sorted(first_t.values())
    thr_t = _ts_sorted[int(0.7 * len(_ts_sorted))] if len(_ts_sorted) > 3 else float("inf")

    vf_rows, vl_rows = [], []
    for v in gt_vehicle:
        tv = v["true_vehicle_id"]
        entity_id = "ent_" + hashlib.sha256(tv.encode()).hexdigest()[:12]
        rs = reports_by_vehicle.get(tv, [])
        time_split = "test" if (tv in first_t and first_t[tv] > thr_t) else "train"
        norms = [float(r.get("detector_score_norm", 0.0) or 0.0) for r in rs]
        times = [float(r.get("detection_time", 0)) for r in rs]
        reporters = {r.get("reporter_cert_digest") for r in rs}
        reasons = {c for r in rs for c in (r.get("reason_codes") or ["?"])}
        # MA-visible pseudonym count: distinct subject certs the MA actually linked from its OWN
        # reports (NOT the oracle gt_identity_map, which would leak Sybil ghosts / unobserved certs).
        n_pseudonyms = len({r.get("subject_cert_digest") for r in rs})
        split = _split_for(split_seed, tv)
        vf_rows.append({
            "entity_id": entity_id,
            "n_reports": len(rs),
            "n_distinct_reporters": len(reporters),
            "n_pseudonyms": n_pseudonyms,
            "n_distinct_reasons": len(reasons) if rs else 0,
            "score_norm_max": max(norms) if norms else 0.0,
            "score_norm_mean": sum(norms) / len(norms) if norms else 0.0,
            "report_span_s": (max(times) - min(times)) if times else 0.0,
            "reports_per_reporter": len(rs) / max(1, len(reporters)),
            "split": split,
            "time_split": time_split,
        })
        vl_rows.append({
            "entity_id": entity_id,
            "label_is_attacker": int(attacker_by_vehicle.get(tv, False)),
            "label_is_faulty": int(faulty_by_vehicle.get(tv, False)),
            "attack_family": family_by_vehicle.get(tv, "none" if not attacker_by_vehicle.get(tv) else "other"),
            "true_vehicle_id": tv,
            "split": split,
            "time_split": time_split,
        })

    # --- graph + temporal export (for GNN / sequence models) ---
    # The MA's global view is a directed report GRAPH (reporter -> subject) over opaque linked
    # entities, plus a per-entity time SEQUENCE of report windows. Both are MA-visible + leakage-safe
    # (opaque entity ids only). This is what lets downstream GNN/transformer models learn the global
    # correlation structure, not just per-node aggregates.
    def _ent(tv: str) -> str:
        return "ent_" + hashlib.sha256(tv.encode()).hexdigest()[:12]

    edge_rows, indeg = [], {}
    reporters_of: dict[str, set] = {}
    for r in reports:
        stv = vehicle_by_digest.get(r.get("subject_cert_digest"))
        rtv = vehicle_by_digest.get(r.get("reporter_cert_digest"))
        if not stv or not rtv:
            continue
        se, re = _ent(stv), _ent(rtv)
        indeg[se] = indeg.get(se, 0) + 1
        reporters_of.setdefault(se, set()).add(re)
        edge_rows.append({
            "src_entity": re, "dst_entity": se,
            "t": round(float(r.get("detection_time", 0)), 3),
            "reason": (r.get("reason_codes") or ["?"])[0],
            "score_norm": round(float(r.get("detector_score_norm", 0.0) or 0.0), 3),
            "split": _split_for(split_seed, stv),
        })
    # graph node feature: mean in-degree of an entity's reporters (a collusion / reporter-reputation
    # signal — are you flagged by entities that are themselves heavily flagged?).
    for row in vf_rows:
        res = reporters_of.get(row["entity_id"], set())
        row["reporter_mean_indegree"] = (sum(indeg.get(re, 0) for re in res) / len(res)) if res else 0.0

    # temporal sequence: per subject-entity, fixed WIN-second windows of report activity.
    WIN = 10.0
    win_acc: dict[tuple, dict] = {}
    for r in reports:
        stv = vehicle_by_digest.get(r.get("subject_cert_digest"))
        if not stv:
            continue
        wk = (_ent(stv), int(float(r.get("detection_time", 0)) // WIN))
        a = win_acc.setdefault(wk, {"n": 0, "sn": 0.0, "reps": set(), "reasons": set(),
                                    "split": _split_for(split_seed, stv)})
        a["n"] += 1
        a["sn"] = max(a["sn"], float(r.get("detector_score_norm", 0.0) or 0.0))
        a["reps"].add(r.get("reporter_cert_digest"))
        a["reasons"].update(r.get("reason_codes") or ["?"])
    window_rows = [{"entity_id": e, "window": w, "n_reports": a["n"], "score_norm_max": a["sn"],
                    "n_distinct_reporters": len(a["reps"]), "n_distinct_reasons": len(a["reasons"]),
                    "split": a["split"]}
                   for (e, w), a in win_acc.items()]

    report_features = pd.DataFrame(rf_rows)
    report_labels = pd.DataFrame(rl_rows)
    subject_features = pd.DataFrame(sf_rows)
    subject_labels = pd.DataFrame(sl_rows)
    vehicle_features = pd.DataFrame(vf_rows)
    vehicle_labels = pd.DataFrame(vl_rows)
    graph_edges = pd.DataFrame(edge_rows)
    subject_windows = pd.DataFrame(window_rows)

    # --- leakage firewall on the FEATURE tables (column-name lineage check) ---
    for name, df in (("report_features", report_features), ("subject_features", subject_features),
                     ("vehicle_features", vehicle_features), ("graph_edges", graph_edges),
                     ("subject_windows", subject_windows)):
        cols = [c for c in df.columns if c not in ("split", "time_split")]  # split assignments, not features
        lint_feature_frame([dict.fromkeys(cols, 0)], context=name)

    # --- write outputs (parquet + csv) ---
    out = os.path.join(dataset_dir, "ml")
    os.makedirs(out, exist_ok=True)
    written = {}
    for name, df in (
        ("report_features", report_features), ("report_labels", report_labels),
        ("subject_features", subject_features), ("subject_labels", subject_labels),
        ("vehicle_features", vehicle_features), ("vehicle_labels", vehicle_labels),
        ("graph_edges", graph_edges), ("subject_windows", subject_windows),
    ):
        pq = os.path.join(out, f"{name}.parquet")
        csv = os.path.join(out, f"{name}.csv")
        if len(df) == 0:
            df = pd.DataFrame()
        df.to_parquet(pq, index=False)
        df.to_csv(csv, index=False)
        written[name] = {"rows": int(len(df)), "parquet": pq}

    # --- split-integrity check: no true vehicle spans multiple splits ---
    split_of_vehicle: dict[str, str] = {}
    leaks = 0
    for _, row in subject_labels.iterrows():
        v, s = row["true_vehicle_id"], row["split"]
        if v in split_of_vehicle and split_of_vehicle[v] != s:
            leaks += 1
        split_of_vehicle[v] = s
    assert leaks == 0, f"split leakage: {leaks} vehicles span multiple splits"

    summary = {
        "dataset_dir": dataset_dir,
        "tables": written,
        "n_reports": len(report_features),
        "n_subjects": len(subject_features),
        "split_counts": subject_labels["split"].value_counts().to_dict() if len(subject_labels) else {},
        "leakage_violations": 0,
        "split_leakage": leaks,
    }
    return summary


def main(argv: list[str] | None = None) -> int:
    import argparse
    p = argparse.ArgumentParser(description="Build ML-ready tables from an MA/ground-truth dataset.")
    p.add_argument("dataset_dir")
    p.add_argument("--split-seed", type=int, default=1234)
    args = p.parse_args(argv)
    s = build(args.dataset_dir, split_seed=args.split_seed)
    print(json.dumps(s, indent=2, default=str))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
