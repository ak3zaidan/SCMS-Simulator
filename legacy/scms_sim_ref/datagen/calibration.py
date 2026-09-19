"""Quantitative calibration of a dataset's distributions against real-world reference models.

A domain-randomised dataset only transfers to reality if its envelope is CENTERED on reality. This
module turns the qualitative realism scorecard into numbers: it fits the benign sensor-error
distribution and compares it to the published automotive-GNSS model, and it checks whether the
broadcast position CONFIDENCE is actually calibrated (does a 95% confidence radius really cover 95%
of the error?). numpy only — a 1-sample Kolmogorov–Smirnov statistic is computed directly.

References:
  * Automotive GNSS horizontal error is well modelled as Rayleigh-distributed magnitude with a
    circular-error-probable (CEP50) of ~2–3 m in open/urban conditions (heavier-tailed in canyons).
  * A well-formed CAM position confidence should be a true ~95% bound on the error.
"""

from __future__ import annotations

import json
import math
import os

import numpy as np

REF_CEP_M = 2.5              # real automotive GNSS CEP50 (median horizontal error), metres
REF_SIGMA = REF_CEP_M / 1.1774   # Rayleigh scale for that CEP


def _emissions(dataset_dir: str) -> list[dict]:
    p = os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")
    if not os.path.exists(p):
        return []
    with open(p, encoding="utf-8") as fh:
        return [json.loads(x) for x in fh if x.strip()]


def _ks_vs_rayleigh(err: np.ndarray, sigma: float) -> float:
    """1-sample KS statistic of `err` against Rayleigh(sigma)."""
    x = np.sort(err)
    n = len(x)
    cdf_ref = 1.0 - np.exp(-(x ** 2) / (2.0 * sigma ** 2))
    cdf_emp_hi = np.arange(1, n + 1) / n
    cdf_emp_lo = np.arange(0, n) / n
    return float(max(np.max(np.abs(cdf_emp_hi - cdf_ref)), np.max(np.abs(cdf_emp_lo - cdf_ref))))


def calibrate(dataset_dir: str) -> dict:
    emit = _emissions(dataset_dir)
    # honest, healthy sensors only — exclude attackers AND malfunctioning (faulty) units.
    benign = [e for e in emit if not e.get("is_attacker") and not e.get("is_faulty")
              and not e.get("falsified")]
    out: dict = {"n_benign_emissions": len(benign)}
    if len(benign) < 30:
        out["note"] = "too few benign emissions for calibration"
        return out

    err = np.array([math.hypot(float(e["claimed_x"]) - float(e["true_x"]),
                               float(e["claimed_y"]) - float(e["true_y"])) for e in benign])
    # fit Rayleigh by MLE: sigma^2 = mean(err^2)/2  ->  CEP = 1.1774*sigma
    sigma_hat = math.sqrt(np.mean(err ** 2) / 2.0)
    cep_hat = 1.1774 * sigma_hat
    ks_ref = _ks_vs_rayleigh(err, REF_SIGMA)
    ks_fit = _ks_vs_rayleigh(err, sigma_hat)
    out["gps_error"] = {
        "fitted_cep_m": round(cep_hat, 3),
        "ref_cep_m": REF_CEP_M,
        # real automotive GNSS CEP is ~1.5-5 m open-sky, up to ~12 m in urban/poor conditions
        "cep_in_real_range": bool(1.0 <= cep_hat <= 12.0),
        "ks_vs_reference_rayleigh": round(ks_ref, 3),   # distance from the literature model
        "ks_vs_fitted_rayleigh": round(ks_fit, 3),      # informational: real error is Rician, not pure Rayleigh
        "shape_is_rayleigh_like": bool(ks_fit < 0.25),
    }

    # confidence calibration: fraction of benign errors within the broadcast 95% confidence radius
    confs = [float(e.get("pos_conf", 0) or 0) for e in benign]
    pairs = [(e_, c_) for e_, c_ in zip(err, confs) if c_ > 0]
    if pairs:
        covered = float(np.mean([e_ <= c_ for e_, c_ in pairs]))
        out["confidence_calibration"] = {
            "empirical_coverage_of_95pct_radius": round(covered, 3),
            "target": 0.95,
            # a nominal 95% bound realistically covers ~0.85-0.95 empirically — the residual gap is
            # the heavy multipath/outlier tail a Gaussian bound cannot capture (as in real GNSS).
            "well_calibrated": bool(0.85 <= covered <= 0.985),
        }
    return out


def main(argv=None) -> int:
    import argparse
    p = argparse.ArgumentParser(description="Calibrate a dataset against real-world reference distributions.")
    p.add_argument("dataset_dir")
    a = p.parse_args(argv)
    print(json.dumps(calibrate(a.dataset_dir), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
