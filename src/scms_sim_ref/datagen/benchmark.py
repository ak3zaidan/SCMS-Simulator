"""Baseline ML benchmark over a generated dataset (numpy only, no sklearn).

Trains simple, leakage-safe logistic-regression baselines on the vehicle-disjoint train
split and evaluates on the test split, so the dataset ships with proof that it is learnable
at a realistic — not trivial — difficulty:

  * subject-level: is a certificate's owner an attacker? (features = MA-derived aggregates)
  * report-level:  is a single misbehaviour report a true attack detection?

Reports ROC-AUC, PR-AUC, and precision/recall/F1 at 0.5 per task. Only ``*_features`` tables
feed the model; labels come from the separate ``*_labels`` tables (the featurizer's leakage
firewall already guarantees no ground-truth column leaks into features).
"""

from __future__ import annotations

import json
import os
from typing import Any

import numpy as np
import pandas as pd

_DROP = {"split", "report_id", "subject_cert_digest", "true_vehicle_id"}


def _load(dataset_dir: str, name: str) -> pd.DataFrame | None:
    for ext in ("parquet", "csv"):
        p = os.path.join(dataset_dir, "ml", f"{name}.{ext}")
        if os.path.exists(p):
            try:
                return pd.read_parquet(p) if ext == "parquet" else pd.read_csv(p)
            except Exception:
                continue
    return None


def _feature_matrix(df: pd.DataFrame) -> tuple[np.ndarray, list[str]]:
    # never let a label/id column into the feature matrix (would be target leakage)
    cols = [c for c in df.columns if c not in _DROP and not c.startswith("label_")
            and pd.api.types.is_numeric_dtype(df[c])]
    x = df[cols].to_numpy(dtype=float)
    x = np.nan_to_num(x, nan=0.0, posinf=0.0, neginf=0.0)
    return x, cols


def _fit_logreg(x: np.ndarray, y: np.ndarray, iters: int = 400, lr: float = 0.1,
                l2: float = 1e-3) -> tuple[np.ndarray, float, np.ndarray, np.ndarray]:
    mu, sd = x.mean(0), x.std(0)
    sd[sd == 0] = 1.0
    xs = (x - mu) / sd
    n, d = xs.shape
    w = np.zeros(d)
    b = 0.0
    for _ in range(iters):
        z = xs @ w + b
        p = 1.0 / (1.0 + np.exp(-np.clip(z, -30, 30)))
        g = p - y
        w -= lr * (xs.T @ g / n + l2 * w)
        b -= lr * g.mean()
    return w, b, mu, sd


def _predict(x: np.ndarray, w, b, mu, sd) -> np.ndarray:
    xs = (x - mu) / sd
    return 1.0 / (1.0 + np.exp(-np.clip(xs @ w + b, -30, 30)))


def _roc_auc(y: np.ndarray, s: np.ndarray) -> float | None:
    pos, neg = s[y == 1], s[y == 0]
    if len(pos) == 0 or len(neg) == 0:
        return None
    order = np.argsort(s, kind="mergesort")
    ranks = np.empty(len(s), dtype=float)
    ranks[order] = np.arange(1, len(s) + 1)
    # average ranks for ties
    _, inv, counts = np.unique(s, return_inverse=True, return_counts=True)
    csum = np.cumsum(counts)
    avg = {}
    start = 0
    for i, c in enumerate(counts):
        avg[i] = (start + 1 + start + c) / 2.0
        start += c
    ranks = np.array([avg[i] for i in inv])
    auc = (ranks[y == 1].sum() - len(pos) * (len(pos) + 1) / 2.0) / (len(pos) * len(neg))
    return float(auc)


def _pr_auc(y: np.ndarray, s: np.ndarray) -> float | None:
    if y.sum() == 0:
        return None
    order = np.argsort(-s, kind="mergesort")
    ys = y[order]
    tp = np.cumsum(ys)
    fp = np.cumsum(1 - ys)
    prec = tp / np.maximum(tp + fp, 1)
    rec = tp / max(y.sum(), 1)
    rec = np.concatenate([[0.0], rec])
    prec = np.concatenate([[1.0], prec])
    trapz = getattr(np, "trapezoid", None) or np.trapz
    return float(trapz(prec, rec))


def _prf(y: np.ndarray, s: np.ndarray, thr: float = 0.5) -> dict:
    pred = (s >= thr).astype(int)
    tp = int(((pred == 1) & (y == 1)).sum())
    fp = int(((pred == 1) & (y == 0)).sum())
    fn = int(((pred == 0) & (y == 1)).sum())
    prec = tp / (tp + fp) if tp + fp else 0.0
    rec = tp / (tp + fn) if tp + fn else 0.0
    f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
    return {"precision": round(prec, 3), "recall": round(rec, 3), "f1": round(f1, 3),
            "tp": tp, "fp": fp, "fn": fn}


def _task(feat: pd.DataFrame, lab: pd.DataFrame, key: str, label_col: str) -> dict | None:
    if feat is None or lab is None or label_col not in lab.columns:
        return None
    df = feat.merge(lab[[key, label_col, "split"]], on=[key, "split"], how="inner") \
        if "split" in feat.columns else feat.merge(lab[[key, label_col]], on=key, how="inner")
    tr = df[df["split"] == "train"] if "split" in df.columns else df
    te = df[df["split"] == "test"] if "split" in df.columns else df
    if len(tr) < 20 or len(te) < 5:
        return {"note": "insufficient data for a benchmark", "n_train": int(len(tr)), "n_test": int(len(te))}
    xtr, cols = _feature_matrix(tr)
    xte, _ = _feature_matrix(te)
    ytr = tr[label_col].to_numpy(dtype=float)
    yte = te[label_col].to_numpy(dtype=float)
    if ytr.sum() == 0 or ytr.sum() == len(ytr):
        return {"note": "train split single-class", "positives_train": int(ytr.sum()), "n_train": int(len(ytr))}
    w, b, mu, sd = _fit_logreg(xtr, ytr)
    s = _predict(xte, w, b, mu, sd)
    return {
        "n_train": int(len(tr)), "n_test": int(len(te)),
        "positive_rate_test": round(float(yte.mean()), 3),
        "roc_auc": None if _roc_auc(yte, s) is None else round(_roc_auc(yte, s), 3),
        "pr_auc": None if _pr_auc(yte, s) is None else round(_pr_auc(yte, s), 3),
        "at_0.5": _prf(yte, s),
        "features": cols,
    }


def run(dataset_dir: str) -> dict[str, Any]:
    sf, sl = _load(dataset_dir, "subject_features"), _load(dataset_dir, "subject_labels")
    rf, rl = _load(dataset_dir, "report_features"), _load(dataset_dir, "report_labels")
    out: dict[str, Any] = {"dataset_dir": dataset_dir, "tasks": {}}
    subj = _task(sf, sl, "subject_cert_digest", "label_is_attacker")
    if subj is not None:
        out["tasks"]["subject_is_attacker"] = subj
    rep = _task(rf, rl, "report_id", "label_subject_is_attacker")
    if rep is not None:
        out["tasks"]["report_is_true_detection"] = rep
    return out


def main(argv: list[str] | None = None) -> int:
    import argparse
    p = argparse.ArgumentParser(description="Baseline ML benchmark over a generated dataset.")
    p.add_argument("dataset_dir")
    a = p.parse_args(argv)
    print(json.dumps(run(a.dataset_dir), indent=2, default=str))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
