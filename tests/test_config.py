"""Tests for PipelineConfig validation and robustness on degenerate configs."""

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.run import validate_config


@pytest.mark.parametrize("kw", [
    dict(dt=0.0),
    dict(idm_accel=0.0),
    dict(idm_decel=0.0),
    dict(jmax=0),
    dict(radio_range_m=0.0),
    dict(weather="hail"),
    dict(demand_profile="chaos"),
    dict(fleet="spaceship"),
    dict(road_network="hyperloop"),
    dict(traffic_flow=True, road_network="grid", grid_w=1, grid_h=1),
])
def test_invalid_config_raises_clear_error(kw):
    with pytest.raises(ValueError):
        validate_config(PipelineConfig(**kw))


def test_probabilities_are_clamped():
    cfg = PipelineConfig(attacker_pct=1.5, faulty_pct=-0.2, report_prob=9.0, nlos_loss=-1.0)
    validate_config(cfg)
    assert cfg.attacker_pct == 1.0 and cfg.faulty_pct == 0.0
    assert cfg.report_prob == 1.0 and cfg.nlos_loss == 0.0


def test_degenerate_but_valid_runs_do_not_crash(tmp_path):
    # a truly empty run, a tiny grid flow, and a no-attacker run — all must complete, not raise
    r0 = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "e"), n_vehicles=2, n_steps=0,
                                     attacker_ids=()))
    assert r0.n_reports == 0
    r = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "z"), traffic_flow=True, road_network="grid",
                                    duration_s=30.0, grid_w=2, grid_h=2, arrival_rate=1.0))
    assert r.n_vehicles >= 0
    r2 = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "s"), n_vehicles=3, n_steps=5,
                                     attacker_ids=()))
    assert r2.n_vehicles == 3


def test_cli_preset_applies_and_flags_override(tmp_path):
    """--preset seeds a full scenario; an explicit flag still overrides the preset value."""
    import json
    from scms_sim_ref.mock_pipeline.run import main, CLI_PRESETS

    out = tmp_path / "p"
    rc = main(["--preset", "urban_rush", "--duration", "20", "--attacker-pct", "0.0",
               "--out", str(out)])
    assert rc == 0
    cfg = json.loads((out / "manifest.json").read_text())["config"]
    # preset values took effect...
    assert cfg["traffic_flow"] is True and cfg["demand_profile"] == "rush"
    assert cfg["traffic_lights"] is True and cfg["od_model"] == "gravity" and cfg["n_lanes"] == 2
    # ...but the explicit flags won
    assert cfg["duration_s"] == 20.0 and cfg["attacker_pct"] == 0.0


def test_all_cli_presets_run(tmp_path):
    """Every named preset produces a valid dataset (short duration override for speed)."""
    import json
    from scms_sim_ref.mock_pipeline.run import main, CLI_PRESETS

    for name in CLI_PRESETS:
        out = tmp_path / name
        assert main(["--preset", name, "--duration", "15", "--out", str(out)]) == 0
        assert (out / "manifest.json").exists()
        assert json.loads((out / "manifest.json").read_text())["config"]["traffic_flow"] is True
