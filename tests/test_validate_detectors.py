"""Per-detector reliability diagnostic in validate: report-level precision per reason code."""

from scms_sim_ref.datagen import validate as V
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def test_detector_reliability_is_reported_and_sane(tmp_path):
    out = str(tmp_path / "run")
    # a clear (non-colluding) run: every reason code should almost always target a real attacker
    run_pipeline(PipelineConfig(seed=7, n_vehicles=80, n_steps=120, attacker_pct=0.2,
                                faulty_pct=0.05, collude_pct=0.0, out_dir=out))
    s, _ = V.validate(out)
    dr = s["detector_reliability"]
    assert dr, "expected at least one firing detector"
    for code, d in dr.items():
        assert d["reports"] > 0
        assert 0.0 <= d["precision"] <= 1.0
    # the diagnostic must distinguish trustworthy detectors: the position/constant/frequency checks
    # are high-precision signals (this is exactly the reliability ranking the table is for).
    reliable = [c for c, d in dr.items() if d["precision"] >= 0.9 and d["reports"] >= 10]
    assert reliable, f"expected some high-precision detectors, got {dr}"


def test_collusion_lowers_a_detectors_report_precision(tmp_path):
    """Colluders file plausible false reports against benign victims -> report-level detector
    precision drops vs a collusion-free run (the diagnostic reflects gameability)."""
    def mean_precision(collude):
        out = str(tmp_path / f"c{collude}")
        run_pipeline(PipelineConfig(seed=11, n_vehicles=80, n_steps=140, attacker_pct=0.2,
                                    faulty_pct=0.0, collude_pct=collude, victim_pct=0.3,
                                    ma_defense=False, out_dir=out))
        dr = V.validate(out)[0]["detector_reliability"]
        total = sum(d["reports"] for d in dr.values())
        return sum(d["precision"] * d["reports"] for d in dr.values()) / max(1, total)

    assert mean_precision(0.6) < mean_precision(0.0)
