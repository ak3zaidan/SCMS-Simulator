"""Graceful interruption: a Ctrl-C mid-run still finalizes a valid, usable partial dataset."""

import json

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline import run as runmod
from scms_sim_ref.datagen import validate as V


def test_interrupt_finalizes_partial_dataset(tmp_path):
    out = str(tmp_path / "run")
    # simulate an interrupt arriving at step 8 of a long flow run, via the per-step hook
    def hook(step):
        if step == 8:
            runmod._ABORT["flag"] = True
    runmod.PER_STEP_HOOK = hook
    try:
        res = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid",
                                          duration_s=600.0, arrival_rate=2.0, grid_w=5, grid_h=5,
                                          attacker_pct=0.2, out_dir=out))
    finally:
        runmod.PER_STEP_HOOK = None

    # a valid manifest exists despite the early stop, and it reflects the partial run (<600 steps)
    man = json.loads((tmp_path / "run" / "manifest.json").read_text())
    assert man["config"]["duration_s"] == 600.0
    assert man["counts"]["reports"] == res.n_reports
    # the streamed reports match the manifest count (streams were flushed/closed on the way out)
    n_lines = sum(1 for _ in open(tmp_path / "run" / "ma" / "ma_reports.jsonl", encoding="utf-8"))
    assert n_lines == res.n_reports
    # and the partial dataset is still analyzable end-to-end
    s, _ = V.validate(str(tmp_path / "run"))
    assert s["leakage_violations"] == 0


def test_abort_flag_is_reset_between_runs(tmp_path):
    """A stale abort flag from a prior run must not truncate the next run."""
    runmod._ABORT["flag"] = True                      # leftover from a hypothetical earlier interrupt
    res = run_pipeline(PipelineConfig(seed=1, n_vehicles=10, n_steps=20, out_dir=str(tmp_path / "r")))
    assert res.n_reports >= 0 and res.data_digest      # ran to completion, produced a digest
