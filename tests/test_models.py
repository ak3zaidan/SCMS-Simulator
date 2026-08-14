"""Tests for the numpy gradient-boosted-trees baseline and its benchmark integration."""

import numpy as np

from scms_sim_ref.datagen.benchmark import _bootstrap_ci, _roc_auc
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
