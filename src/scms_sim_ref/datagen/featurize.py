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
    "positionSpeedConsistency",
    "acceptanceRangeThreshold",
    "constantPositionOffset",
]


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
    attacker_by_vehicle = {v["true_vehicle_id"]: bool(v["is_attacker"]) for v in gt_vehicle}
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
            "detector_score": score,
            "sig_valid": int(sig),
            "ingest_delay": float(r.get("ingest_time", 0)) - float(r.get("detection_time", 0)),
            "crl_active_at_report": int(crl == "active"),
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
        times = [float(r.get("detection_time", 0)) for r in rs]
        reporters = {r.get("reporter_cert_digest") for r in rs}
        subj_true = vehicle_by_digest.get(digest, "unknown")
        split = _split_for(split_seed, subj_true)
        sf_rows.append({
            "subject_cert_digest": digest,
            "n_reports": len(rs),
            "n_distinct_reporters": len(reporters),
            "score_mean": sum(scores) / len(scores) if scores else 0.0,
            "score_max": max(scores) if scores else 0.0,
            "report_span_s": (max(times) - min(times)) if times else 0.0,
            "cert_lifetime_s": float(lifetime_by_digest.get(digest, 0.0)),
            "split": split,
        })
        sl_rows.append({
            "subject_cert_digest": digest,
            "label_is_attacker": int(attacker_by_vehicle.get(subj_true, False)),
            "label_was_revoked": int(bool(revoked_by_digest.get(digest, False))),
            "true_vehicle_id": subj_true,
            "split": split,
        })

    report_features = pd.DataFrame(rf_rows)
    report_labels = pd.DataFrame(rl_rows)
    subject_features = pd.DataFrame(sf_rows)
    subject_labels = pd.DataFrame(sl_rows)

    # --- leakage firewall on the FEATURE tables (column-name lineage check) ---
    for name, df in (("report_features", report_features), ("subject_features", subject_features)):
        cols = [c for c in df.columns if c != "split"]  # split is an assignment, not a feature/GT
        lint_feature_frame([dict.fromkeys(cols, 0)], context=name)

    # --- write outputs (parquet + csv) ---
    out = os.path.join(dataset_dir, "ml")
    os.makedirs(out, exist_ok=True)
    written = {}
    for name, df in (
        ("report_features", report_features), ("report_labels", report_labels),
        ("subject_features", subject_features), ("subject_labels", subject_labels),
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
