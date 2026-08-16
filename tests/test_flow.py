"""Tests for long-running traffic-flow mode: spawn/despawn, routed trips, streaming, determinism."""

import json

from scms_sim_ref.datagen import validate as V
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _flow(tmp_path, **over):
    kw = dict(traffic_flow=True, road_network="grid", duration_s=150.0, arrival_rate=2.0,
              grid_w=5, grid_h=5, grid_block_m=120.0, attacker_pct=0.15, faulty_pct=0.05,
              rotate_period_s=60.0, collude_pct=0.3, radio_range_m=250.0, seed=7)
    kw.update(over)
    return run_pipeline(PipelineConfig(out_dir=str(tmp_path / "run"), **kw))


def test_flow_has_vehicle_turnover_and_bounded_concurrency(tmp_path):
    r = _flow(tmp_path)
    veh = _jsonl(tmp_path / "run" / "ground_truth" / "gt_vehicle.jsonl")
    # continuous arrivals: many vehicles, spread across spawn times (not all at t=0)
    spawns = sorted(v["spawn_time"] for v in veh)
    assert len(veh) > 100, "a 150s run at 2/s should spawn many vehicles"
    assert spawns[0] < 30 and spawns[-1] > 100, "vehicles arrive across the whole run"
    # steady-state concurrency stays far below the total (vehicles despawn at trip end)
    cs = _jsonl(tmp_path / "run" / "ma" / "ma_cert_status.jsonl")
    events = sorted([(c["first_seen"], 1) for c in cs] + [(c["last_seen"], -1) for c in cs])
    cur = peak = 0
    for _t, d in events:
        cur += d
        peak = max(peak, cur)
    assert peak < len(cs), "peak concurrency must be well below the total (turnover happened)"


def test_flow_is_deterministic_and_leakage_free(tmp_path):
    a = _flow(tmp_path / "a")
    b = _flow(tmp_path / "b")
    assert a.data_digest == b.data_digest
    for name in ("ma_reports.jsonl", "ma_cert_status.jsonl", "ma_investigations.jsonl"):
        for row in _jsonl(tmp_path / "a" / "run" / "ma" / name):
            assert find_forbidden_keys(row) == [], f"{name} leaks: {find_forbidden_keys(row)}"


def test_flow_streamed_report_count_matches_manifest(tmp_path):
    r = _flow(tmp_path)
    man = json.loads((tmp_path / "run" / "manifest.json").read_text())
    n_lines = sum(1 for _ in open(tmp_path / "run" / "ma" / "ma_reports.jsonl", encoding="utf-8"))
    assert man["counts"]["reports"] == n_lines == r.n_reports


def test_flow_detection_is_realistic_not_degenerate(tmp_path):
    """The MA must catch attackers without revoking most benign vehicles (precision stays high)."""
    _flow(tmp_path, duration_s=250.0)
    s, _ = V.validate(str(tmp_path / "run"))
    assert s["precision"] is not None and s["precision"] >= 0.6, s
    assert s["recall"] is not None and s["recall"] >= 0.4, s


def test_flow_vehicles_follow_grid_routes(tmp_path):
    """Sampled true positions stay within the grid road network bounds (routed mobility)."""
    _flow(tmp_path, grid_w=5, grid_h=5, grid_block_m=120.0)
    emis = _jsonl(tmp_path / "run" / "ground_truth" / "gt_emissions_sample.jsonl")
    assert emis
    span = 4 * 120.0  # (grid-1) * block
    assert all(-5 <= e["true_x"] <= span + 5 and -5 <= e["true_y"] <= span + 5 for e in emis)
