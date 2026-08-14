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

from .models import fit_gbdt, predict_gbdt

_DROP = {"split", "time_split", "domain_id", "report_id", "subject_cert_digest",
         "true_vehicle_id", "entity_id",
         # Kept in the tables (for per-subject sequence ordering / graph), but NEVER used as model
         # features: detection_time lets a model exploit absolute wall-clock, and crl_active_at_report
         # is downstream of the MA's OWN revoke decision (revoked==attacker), i.e. target leakage.
         "detection_time", "crl_active_at_report"}


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
    # mid-rank (tie-averaged) Mann–Whitney statistic
    _, inv, counts = np.unique(s, return_inverse=True, return_counts=True)
    avg, start = {}, 0
    for i, c in enumerate(counts):
        avg[i] = (start + 1 + start + c) / 2.0
        start += c
    ranks = np.array([avg[i] for i in np.asarray(inv).ravel()])
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
    tn = int(((pred == 0) & (y == 0)).sum())
    prec = tp / (tp + fp) if tp + fp else 0.0
    rec = tp / (tp + fn) if tp + fn else 0.0
    f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
    tnr = tn / (tn + fp) if tn + fp else 0.0
    return {"precision": round(prec, 3), "recall": round(rec, 3), "f1": round(f1, 3),
            "balanced_accuracy": round(0.5 * (rec + tnr), 3), "tp": tp, "fp": fp, "fn": fn, "tn": tn}


def _bootstrap_ci(y: np.ndarray, s: np.ndarray, metric, n_boot: int = 300,
                  seed: int = 12345, alpha: float = 0.05) -> list | None:
    """Percentile bootstrap CI for a metric of (y, s). Seeded -> reproducible (eval-only, not in the
    dataset digest). Returns [lo, hi] rounded, or None if the metric is undefined on the sample."""
    n = len(y)
    if n < 10:
        return None
    rng = np.random.default_rng(seed)
    vals = []
    for _ in range(n_boot):
        b = rng.integers(0, n, n)
        v = metric(y[b], s[b])
        if v is not None:
            vals.append(v)
    if len(vals) < n_boot // 2:
        return None
    lo, hi = np.quantile(vals, [alpha / 2, 1 - alpha / 2])
    return [round(float(lo), 3), round(float(hi), 3)]


def _model_metrics(yte: np.ndarray, s: np.ndarray, with_ci: bool = True) -> dict:
    m = {
        "roc_auc": None if _roc_auc(yte, s) is None else round(_roc_auc(yte, s), 3),
        "pr_auc": None if _pr_auc(yte, s) is None else round(_pr_auc(yte, s), 3),
        "recall_at_1pct_fpr": None if _recall_at_fpr(yte, s) is None else round(_recall_at_fpr(yte, s), 3),
        "balanced_accuracy": _prf(yte, s)["balanced_accuracy"],
    }
    if with_ci:
        m["roc_auc_ci95"] = _bootstrap_ci(yte, s, _roc_auc)
    return m


def _recall_at_fpr(y: np.ndarray, s: np.ndarray, fpr: float = 0.01) -> float | None:
    """Recall achievable at a fixed false-positive rate — the operating point a real MA cares about
    (few false accusations)."""
    neg, pos = s[y == 0], s[y == 1]
    if len(neg) == 0 or len(pos) == 0:
        return None
    thr = float(np.quantile(neg, 1.0 - fpr))
    return float((pos >= thr).mean())


def _task(feat: pd.DataFrame, lab: pd.DataFrame, key: str, label_col: str,
          split_col: str = "split", with_gbdt: bool = True) -> dict | None:
    if feat is None or lab is None or label_col not in lab.columns:
        return None
    if split_col in feat.columns and split_col in lab.columns:
        df = feat.merge(lab[[key, label_col, split_col]], on=[key, split_col], how="inner")
    else:
        df = feat.merge(lab[[key, label_col]], on=key, how="inner")
        split_col = None
    tr = df[df[split_col] == "train"] if split_col else df
    te = df[df[split_col] == "test"] if split_col else df
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
    out = {
        "n_train": int(len(tr)), "n_test": int(len(te)),
        "positive_rate_test": round(float(yte.mean()), 3),
        "roc_auc": None if _roc_auc(yte, s) is None else round(_roc_auc(yte, s), 3),
        "pr_auc": None if _pr_auc(yte, s) is None else round(_pr_auc(yte, s), 3),
        "recall_at_1pct_fpr": None if _recall_at_fpr(yte, s) is None else round(_recall_at_fpr(yte, s), 3),
        "roc_auc_ci95": _bootstrap_ci(yte, s, _roc_auc),
        "at_0.5": _prf(yte, s),
        "features": cols,
    }
    if with_gbdt:
        # strong non-linear tabular baseline (numpy GBDT); trees don't need standardization
        try:
            gb = fit_gbdt(xtr, ytr)
            sg = predict_gbdt(gb, xte)
            out["gbdt"] = _model_metrics(yte, sg)
        except Exception as ex:   # never let a baseline crash the whole benchmark
            out["gbdt"] = {"error": str(ex)}
    return out


def _novel_attack(vf: pd.DataFrame | None, vl: pd.DataFrame | None) -> dict | None:
    """Leave-one-attack-family-out: train on benign + every attack family EXCEPT one, then test
    detection of the held-out family (never seen in training). This is the real-world-relevant
    metric — can the model flag a NOVEL attack type? Averaged over held-out families."""
    if vf is None or vl is None or "attack_family" not in vl.columns:
        return None
    df = vf.merge(vl[["entity_id", "label_is_attacker", "attack_family", "split"]],
                  on=["entity_id", "split"], how="inner")
    fams = sorted(set(df[df.label_is_attacker == 1].attack_family) - {"none", "other"})
    per_family = {}
    for fam in fams:
        tr = pd.concat([df[(df.split != "test") & (df.label_is_attacker == 0)],
                        df[(df.split != "test") & (df.label_is_attacker == 1) & (df.attack_family != fam)]])
        te = pd.concat([df[(df.split == "test") & (df.label_is_attacker == 0)],
                        df[(df.split == "test") & (df.label_is_attacker == 1) & (df.attack_family == fam)]])
        ytr = tr["label_is_attacker"].to_numpy(dtype=float)
        yte = te["label_is_attacker"].to_numpy(dtype=float)
        if ytr.sum() < 3 or yte.sum() < 2 or len(te) - yte.sum() < 3:
            continue
        w, b, mu, sd = _fit_logreg(_feature_matrix(tr)[0], ytr)
        s = _predict(_feature_matrix(te)[0], w, b, mu, sd)
        auc = _roc_auc(yte, s)
        if auc is not None:
            per_family[fam] = round(auc, 3)
    if not per_family:
        return None
    return {"per_family_auc": per_family,
            "mean_novel_attack_auc": round(sum(per_family.values()) / len(per_family), 3),
            "note": "trained on benign + all-but-one attack family; tested on the held-out family"}


def _graph_baseline(vf: pd.DataFrame | None, vl: pd.DataFrame | None,
                    edges: pd.DataFrame | None) -> dict | None:
    """GCN-lite: does the report GRAPH add signal beyond per-node features? Compares a logistic
    model on node features vs. node features augmented with a 1-hop mean of neighbours' features
    (one message-passing step over graph_edges). numpy only (no torch)."""
    if vf is None or vl is None or edges is None or len(edges) == 0:
        return None
    df = vf.merge(vl[["entity_id", "label_is_attacker", "split"]], on=["entity_id", "split"], how="inner")
    if len(df) < 30:
        return None
    fcols = [c for c in vf.columns if c not in _DROP and pd.api.types.is_numeric_dtype(vf[c])]
    x = np.nan_to_num(df[fcols].to_numpy(dtype=float))
    idx = {e: i for i, e in enumerate(df["entity_id"])}
    nbrs: dict[int, list] = {}
    for s, d in zip(edges["src_entity"], edges["dst_entity"]):
        if s in idx and d in idx:                 # undirected 1-hop neighbourhood
            nbrs.setdefault(idx[d], []).append(idx[s])
            nbrs.setdefault(idx[s], []).append(idx[d])
    agg = np.zeros_like(x)
    for i in range(len(df)):
        ns = nbrs.get(i)
        if ns:
            agg[i] = x[ns].mean(axis=0)
    xg = np.hstack([x, agg])
    tr = (df["split"] != "test").to_numpy()
    te = (df["split"] == "test").to_numpy()
    ytr, yte = df["label_is_attacker"].to_numpy(float)[tr], df["label_is_attacker"].to_numpy(float)[te]
    if ytr.sum() < 3 or yte.sum() < 2 or (len(yte) - yte.sum()) < 3:
        return None

    def _auc(m):
        w, b, mu, sd = _fit_logreg(m[tr], ytr)
        return _roc_auc(yte, _predict(m[te], w, b, mu, sd))

    node_auc, graph_auc = _auc(x), _auc(xg)
    return {"node_only_auc": None if node_auc is None else round(node_auc, 3),
            "node_plus_graph_auc": None if graph_auc is None else round(graph_auc, 3),
            "note": "1-hop mean message passing over the report graph vs node features alone"}


def _oneclass(vf: pd.DataFrame | None, vl: pd.DataFrame | None) -> dict | None:
    """UNSUPERVISED anomaly detection — the realistic MA setting where attack labels are scarce.
    Fit a one-class Mahalanobis model on BENIGN vehicles only (train split), then score every test
    vehicle by its Mahalanobis distance from the benign manifold. No attack label is used in
    training; labels are used only to score the result."""
    if vf is None or vl is None:
        return None
    df = vf.merge(vl[["entity_id", "label_is_attacker", "split"]], on=["entity_id", "split"], how="inner")
    if len(df) < 40:
        return None
    x = np.nan_to_num(df[[c for c in vf.columns if c not in _DROP
                          and pd.api.types.is_numeric_dtype(vf[c])]].to_numpy(dtype=float))
    tr = (df["split"] != "test").to_numpy()
    te = (df["split"] == "test").to_numpy()
    y = df["label_is_attacker"].to_numpy(dtype=float)
    benign = x[tr & (y == 0)]
    if len(benign) < 10 or te.sum() < 5 or y[te].sum() < 2:
        return None
    mu = benign.mean(axis=0)
    cov = np.atleast_2d(np.cov(benign, rowvar=False)) + 1e-6 * np.eye(x.shape[1])
    inv = np.linalg.pinv(cov)
    d = x[te] - mu
    s = np.einsum("ij,jk,ik->i", d, inv, d)   # squared Mahalanobis distance = anomaly score
    yte = y[te]
    return {"roc_auc": None if _roc_auc(yte, s) is None else round(_roc_auc(yte, s), 3),
            "pr_auc": None if _pr_auc(yte, s) is None else round(_pr_auc(yte, s), 3),
            "recall_at_1pct_fpr": None if _recall_at_fpr(yte, s) is None else round(_recall_at_fpr(yte, s), 3),
            "note": "one-class Mahalanobis trained on BENIGN vehicles only (no attack labels in training)"}


def _domain_generalization(vf: pd.DataFrame | None, vl: pd.DataFrame | None) -> dict | None:
    """Leave-one-DOMAIN-out (campaign only): train on all conditions but one, test on the held-out
    condition (a different map/traffic/sensor/radio regime). The core sim-to-real question — does the
    model transfer to an environment it never trained on?"""
    if vf is None or vl is None or "domain_id" not in vf.columns:
        return None
    df = vf.merge(vl[["entity_id", "label_is_attacker"]], on="entity_id", how="inner")
    doms = sorted(df["domain_id"].unique())
    if len(doms) < 3:
        return None
    per = {}
    for dom in doms:
        tr = df[df["domain_id"] != dom]
        te = df[df["domain_id"] == dom]
        ytr, yte = tr["label_is_attacker"].to_numpy(float), te["label_is_attacker"].to_numpy(float)
        if ytr.sum() < 3 or yte.sum() < 2 or (len(yte) - yte.sum()) < 3:
            continue
        w, b, mu, sd = _fit_logreg(_feature_matrix(tr)[0], ytr)
        auc = _roc_auc(yte, _predict(_feature_matrix(te)[0], w, b, mu, sd))
        if auc is not None:
            per[int(dom)] = round(auc, 3)
    if not per:
        return None
    return {"mean_auc": round(sum(per.values()) / len(per), 3), "n_domains_evaluated": len(per),
            "note": "leave-one-domain-out: train on all conditions but one, test on the held-out one"}


def _linkage_quality(vlm: pd.DataFrame | None) -> dict | None:
    """How good is the MA's OWN (co-revocation) pseudonym linkage vs the oracle? A pure link groups
    exactly one true vehicle; link_true_id_count>1 means the MA merged distinct vehicles. Reported so
    the vehicle_is_attacker_ma_linked number is read with its linkage error in view."""
    if vlm is None or "link_true_id_count" not in vlm.columns or len(vlm) == 0:
        return None
    multi = vlm[vlm["link_true_id_count"] > 1]
    n_entities = int(len(vlm))
    n_multi_veh = int(len(multi))
    pure = int((vlm["link_true_id_count"] <= 1).sum())
    return {
        "n_ma_entities": n_entities,
        "entity_purity": round(pure / n_entities, 3) if n_entities else None,
        "n_entities_merging_distinct_vehicles": n_multi_veh,
        "note": "MA co-revocation linkage; purity=1.0 means no entity conflated two true vehicles",
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
    vf, vl = _load(dataset_dir, "vehicle_features"), _load(dataset_dir, "vehicle_labels")
    veh = _task(vf, vl, "entity_id", "label_is_attacker")
    if veh is not None:
        out["tasks"]["vehicle_is_attacker"] = veh   # oracle-linked: POST-LINKAGE UPPER BOUND
    # MA-realistic linkage (grouped only by what the MA actually revoked/linked, no oracle map)
    vfm, vlm = _load(dataset_dir, "vehicle_features_ma"), _load(dataset_dir, "vehicle_labels_ma")
    vehm = _task(vfm, vlm, "entity_id", "label_is_attacker")
    if vehm is not None:
        out["tasks"]["vehicle_is_attacker_ma_linked"] = vehm
    lq = _linkage_quality(vlm)
    if lq is not None:
        out["linkage_quality"] = lq
    novel = _novel_attack(vf, vl)
    if novel is not None:
        out["generalization"] = {"vehicle_novel_attack": novel}
    vt = _task(vf, vl, "entity_id", "label_is_attacker", split_col="time_split")
    if vt is not None and vt.get("roc_auc") is not None:
        out.setdefault("generalization", {})["vehicle_temporal"] = {
            "roc_auc": vt["roc_auc"], "recall_at_1pct_fpr": vt.get("recall_at_1pct_fpr"),
            "note": "train on earlier vehicles, test on later ones (forward-in-time generalization)"}
    gb = _graph_baseline(vf, vl, _load(dataset_dir, "graph_edges"))
    if gb is not None:
        out["graph_baseline"] = gb
    oc = _oneclass(vf, vl)
    if oc is not None:
        out["anomaly_baseline_unsupervised"] = oc
    dg = _domain_generalization(vf, vl)
    if dg is not None:
        out.setdefault("generalization", {})["domain_leave_one_out"] = dg
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
