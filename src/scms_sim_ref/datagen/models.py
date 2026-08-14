"""Dependency-free (numpy-only) gradient-boosted decision trees — a STRONG tabular baseline.

The benchmark's logistic-regression baseline measures linear separability; a GBDT measures what a
modern non-linear tabular learner (XGBoost/LightGBM-class) can extract. Shipping both makes the
headline numbers credible: a dataset that a GBDT saturates has no research headroom, while a large
GBDT-over-logreg gap shows non-linear structure worth modelling.

This is a histogram gradient-boosting implementation (LightGBM-style): features are pre-binned into
quantile buckets, and each tree's split search is a vectorised per-bin gradient/hessian histogram
scan. Newton (XGBoost-style) leaf weights on the logistic loss. Fully DETERMINISTIC — no sampling,
no RNG — so the benchmark stays reproducible.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional

import numpy as np


@dataclass
class _Node:
    feature: int = -1
    threshold: float = 0.0      # go left if x[feature] <= threshold
    left: Optional["_Node"] = None
    right: Optional["_Node"] = None
    value: float = 0.0          # leaf log-odds contribution
    is_leaf: bool = False


@dataclass
class GBDT:
    base: float = 0.0
    lr: float = 0.1
    trees: list = field(default_factory=list)
    bin_edges: list = field(default_factory=list)   # kept for reference; predict uses raw thresholds


def _bin_edges(x: np.ndarray, max_bins: int) -> np.ndarray:
    """Quantile bin edges for one feature; deduplicated so constant/low-cardinality cols behave."""
    qs = np.linspace(0.0, 1.0, max_bins + 1)[1:-1]
    edges = np.unique(np.quantile(x, qs)) if len(x) else np.array([])
    return edges


def _build_tree(Xb: np.ndarray, g: np.ndarray, h: np.ndarray, edges: list,
                idx: np.ndarray, depth: int, max_depth: int, lam: float,
                min_child: float, gamma: float, n_bins: int) -> _Node:
    G, H = float(g[idx].sum()), float(h[idx].sum())
    leaf_val = -G / (H + lam)
    if depth >= max_depth or len(idx) < 2 or H < min_child:
        return _Node(value=leaf_val, is_leaf=True)

    best_gain, best_feat, best_bin = 0.0, -1, -1
    parent = (G * G) / (H + lam)
    d = Xb.shape[1]
    for j in range(d):
        nb = len(edges[j]) + 1
        if nb < 2:
            continue
        codes = Xb[idx, j]
        hg = np.bincount(codes, weights=g[idx], minlength=n_bins)[:nb]
        hh = np.bincount(codes, weights=h[idx], minlength=n_bins)[:nb]
        cg, ch = np.cumsum(hg), np.cumsum(hh)
        GL, HL = cg[:-1], ch[:-1]          # left = bins <= split
        GR, HR = G - GL, H - HL
        valid = (HL >= min_child) & (HR >= min_child)
        gain = np.where(valid,
                        (GL * GL) / (HL + lam) + (GR * GR) / (HR + lam) - parent,
                        -np.inf)
        k = int(np.argmax(gain))
        if gain[k] > best_gain:
            best_gain, best_feat, best_bin = float(gain[k]), j, k

    if best_feat < 0 or 0.5 * best_gain - gamma <= 0:
        return _Node(value=leaf_val, is_leaf=True)

    # threshold in RAW feature space = the edge between bin best_bin and best_bin+1
    thr = float(edges[best_feat][best_bin])
    go_left = Xb[idx, best_feat] <= best_bin
    li, ri = idx[go_left], idx[~go_left]
    if len(li) == 0 or len(ri) == 0:
        return _Node(value=leaf_val, is_leaf=True)
    node = _Node(feature=best_feat, threshold=thr, is_leaf=False)
    node.left = _build_tree(Xb, g, h, edges, li, depth + 1, max_depth, lam, min_child, gamma, n_bins)
    node.right = _build_tree(Xb, g, h, edges, ri, depth + 1, max_depth, lam, min_child, gamma, n_bins)
    return node


def _tree_predict(node: _Node, X: np.ndarray) -> np.ndarray:
    out = np.empty(len(X))
    _tree_predict_into(node, X, np.arange(len(X)), out)
    return out


def _tree_predict_into(node: _Node, X: np.ndarray, idx: np.ndarray, out: np.ndarray) -> None:
    if node.is_leaf:
        out[idx] = node.value
        return
    go_left = X[idx, node.feature] <= node.threshold
    _tree_predict_into(node.left, X, idx[go_left], out)
    _tree_predict_into(node.right, X, idx[~go_left], out)


def fit_gbdt(x: np.ndarray, y: np.ndarray, n_estimators: int = 150, max_depth: int = 3,
             lr: float = 0.15, lam: float = 1.0, min_child: float = 1.0, gamma: float = 0.0,
             max_bins: int = 64) -> GBDT:
    x = np.ascontiguousarray(x, dtype=float)
    y = np.asarray(y, dtype=float)
    n, d = x.shape
    edges = [_bin_edges(x[:, j], max_bins) for j in range(d)]
    # bin codes: number of edges strictly less than value == bin index (0..len(edges))
    Xb = np.empty((n, d), dtype=np.int64)
    for j in range(d):
        Xb[:, j] = np.searchsorted(edges[j], x[:, j], side="left") if len(edges[j]) else 0
    n_bins = max_bins + 2
    p0 = float(np.clip(y.mean(), 1e-6, 1 - 1e-6))
    base = float(np.log(p0 / (1 - p0)))
    F = np.full(n, base)
    trees = []
    for _ in range(n_estimators):
        p = 1.0 / (1.0 + np.exp(-np.clip(F, -30, 30)))
        g = p - y
        h = np.maximum(p * (1 - p), 1e-6)
        tree = _build_tree(Xb, g, h, edges, np.arange(n), 0, max_depth, lam, min_child, gamma, n_bins)
        F += lr * _tree_predict(tree, x)
        trees.append(tree)
    return GBDT(base=base, lr=lr, trees=trees, bin_edges=edges)


def predict_gbdt(model: GBDT, x: np.ndarray) -> np.ndarray:
    x = np.ascontiguousarray(x, dtype=float)
    F = np.full(len(x), model.base)
    for tree in model.trees:
        F += model.lr * _tree_predict(tree, x)
    return 1.0 / (1.0 + np.exp(-np.clip(F, -30, 30)))
