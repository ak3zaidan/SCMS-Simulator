"""Tests for the numpy gradient-boosted-trees baseline and its benchmark integration."""

import numpy as np

from scms_sim_ref.datagen.benchmark import _bootstrap_ci, _roc_auc, _best_f1
from scms_sim_ref.datagen.models import fit_gbdt, predict_gbdt


def _xor(n, seed):
    rng = np.random.default_rng(seed)
    x = rng.normal(size=(n, 4))
    y = ((x[:, 0] > 0) ^ (x[:, 1] > 0)).astype(float)
    return x, y


def test_gbdt_learns_nonlinear_structure():
    """GBDT must solve XOR (AUC ~1) where a linear model is at chance (~0.5)."""
    xtr, ytr = _xor(900, 0)
    xte, yte = _xor(400, 1)
    m = fit_gbdt(xtr, ytr)
    auc = _roc_auc(yte, predict_gbdt(m, xte))
    assert auc is not None and auc > 0.9


def test_gbdt_is_deterministic():
    xtr, ytr = _xor(500, 2)
    xte, _ = _xor(200, 3)
    a = predict_gbdt(fit_gbdt(xtr, ytr), xte)
    b = predict_gbdt(fit_gbdt(xtr, ytr), xte)
    assert np.array_equal(a, b)


def test_gbdt_handles_degenerate_inputs():
    # constant features + near-single-class must not raise and stay in (0,1)
    x = np.ones((60, 3))
    y = np.zeros(60)
    y[:3] = 1
    p = predict_gbdt(fit_gbdt(x, y), x)
    assert p.shape == (60,)
    assert np.all((p > 0) & (p < 1))


def test_bootstrap_ci_brackets_the_point_estimate():
    rng = np.random.default_rng(4)
    y = (rng.random(300) > 0.5).astype(float)
    s = y * 0.7 + rng.random(300) * 0.3          # informative score
    point = _roc_auc(y, s)
    ci = _bootstrap_ci(y, s, _roc_auc)
    assert ci is not None and ci[0] <= point <= ci[1]
    assert 0.0 <= ci[0] <= ci[1] <= 1.0


def test_best_f1_finds_perfect_separation():
    """When a threshold separates the classes perfectly, best-F1 reaches 1.0 there."""
    y = np.array([0, 0, 0, 1, 1, 1], dtype=float)
    s = np.array([0.1, 0.2, 0.3, 0.7, 0.8, 0.9])
    op = _best_f1(y, s)
    assert op["f1"] == 1.0 and op["precision"] == 1.0 and op["recall"] == 1.0
    assert 0.3 < op["threshold"] <= 0.7


def test_best_f1_beats_or_matches_half_threshold():
    """The best-F1 point is never worse than the arbitrary 0.5 cut on the same scores."""
    from scms_sim_ref.datagen.benchmark import _prf
    rng = np.random.default_rng(1)
    y = (rng.random(400) < 0.3).astype(float)
    s = 0.35 * y + 0.6 * rng.random(400)             # weak, uncalibrated score (0.5 is a poor cut)
    assert _best_f1(y, s)["f1"] >= _prf(y, s)["f1"] - 1e-9


def test_best_f1_degenerate_and_deterministic():
    assert _best_f1(np.zeros(5), np.random.default_rng(0).random(5)) is None   # no positives
    y = (np.arange(50) % 3 == 0).astype(float)
    s = np.random.default_rng(2).random(50)
    assert _best_f1(y, s) == _best_f1(y, s)          # deterministic
