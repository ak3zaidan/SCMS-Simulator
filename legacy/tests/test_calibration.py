"""Unit test for the real-world distribution calibration (GNSS error + confidence)."""

import json

import numpy as np

from scms_sim_ref.datagen import calibration


def _write_emissions(tmp_path, sigma: float, conf: float, n: int = 3000):
    gt = tmp_path / "ground_truth"
    gt.mkdir(parents=True)
    rng = np.random.default_rng(1)
    rows = []
    for _ in range(n):
        ex, ey = rng.normal(0, sigma), rng.normal(0, sigma)   # 2-D Gaussian sensor error
        rows.append({"is_attacker": False, "is_faulty": False, "falsified": False,
                     "true_x": 0.0, "true_y": 0.0, "claimed_x": ex, "claimed_y": ey,
                     "pos_conf": conf})
    (gt / "gt_emissions_sample.jsonl").write_text("\n".join(json.dumps(r) for r in rows))


def test_gps_error_and_confidence_calibration(tmp_path):
    sigma = 2.0
    conf = 2.448 * sigma                       # the correct 2-D 95% radius
    _write_emissions(tmp_path, sigma, conf)
    res = calibration.calibrate(str(tmp_path))

    ge = res["gps_error"]
    # CEP of a 2-D Gaussian magnitude is ~1.1774*sigma
    assert abs(ge["fitted_cep_m"] - 1.1774 * sigma) < 0.3
    assert ge["cep_in_real_range"] is True

    cc = res["confidence_calibration"]
    # a correct 2-D 95% radius should empirically cover ~95%
    assert 0.93 <= cc["empirical_coverage_of_95pct_radius"] <= 0.97
    assert cc["well_calibrated"] is True


def test_undersized_confidence_is_flagged(tmp_path):
    sigma = 2.0
    _write_emissions(tmp_path, sigma, conf=1.96 * sigma)   # the OLD (1-D) buggy radius
    res = calibration.calibrate(str(tmp_path))
    # a 1-D z-score radius under-covers a 2-D error -> not the ~0.95 target
    assert res["confidence_calibration"]["empirical_coverage_of_95pct_radius"] < 0.90
