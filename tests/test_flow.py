"""Tests for long-running traffic-flow mode: spawn/despawn, routed trips, streaming, determinism."""

import collections
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


def test_signalized_congestion_does_not_expire_benign_certs(tmp_path):
    """Regression: under traffic lights + congestion a benign trip takes far longer than free-flow;
    its cert validity window must still cover it, so certValidity must NOT mass-fire on benign traffic."""
    _flow(tmp_path, traffic_lights=True, car_following=True, arrival_rate=4.0, grid_w=6, grid_h=6,
          grid_block_m=140.0, duration_s=200.0, attacker_pct=0.0, faulty_pct=0.0,
          trip_speed_min=10.0, trip_speed_max=16.0)
    reasons = collections.Counter()
    for ln in open(tmp_path / "run" / "ma" / "ma_reports.jsonl", encoding="utf-8"):
        reasons.update(json.loads(ln).get("reason_codes", []))
    # no attackers -> a correct benign cert window means (near) zero certValidity firings
    assert reasons.get("certValidity", 0) <= 2, f"benign certs expiring under congestion: {reasons}"


def test_signalized_congested_flow_keeps_reasonable_precision(tmp_path):
    """A signalized rush-hour congested run (no heavy collusion) must still separate attacks from
    congestion -- guards against a congestion/cert-window regression tanking precision under lights."""
    _flow(tmp_path, traffic_lights=True, car_following=True, arrival_rate=3.0, grid_w=6, grid_h=6,
          grid_block_m=140.0, duration_s=200.0, n_lanes=2, demand_profile="rush",
          attacker_pct=0.15, collude_pct=0.0, faulty_pct=0.05)
    s, _ = V.validate(str(tmp_path / "run"))
    assert s["precision"] is not None and s["precision"] >= 0.7, s
    assert s["recall"] is not None and s["recall"] >= 0.5, s


def test_all_new_realism_features_are_deterministic(tmp_path):
    """Every new flow realism knob enabled at once still yields byte-identical output run-to-run."""
    kw = dict(traffic_flow=True, road_network="grid", duration_s=200.0, arrival_rate=2.5,
              grid_w=6, grid_h=6, n_lanes=2, car_following=True, od_model="gravity",
              turn_slowdown=True, traffic_lights=True, demand_profile="rush", fleet="mixed",
              attacker_pct=0.2, attack_duty_cycle=0.4, attack_pulse_period_s=15.0,
              attack_delay_jitter_s=10.0, rotate_period_s=60.0, collude_pct=0.3, weather="rain",
              seed=13)
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), **kw))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), **kw))
    assert a.data_digest == b.data_digest, "combined new-realism config must be deterministic"


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


def test_car_following_produces_congestion(tmp_path):
    """IDM car-following at high density must yield speed variation and queueing (stop-and-go)."""
    import statistics
    _flow(tmp_path, car_following=True, arrival_rate=4.0, grid_w=4, grid_h=4, grid_block_m=100.0,
          duration_s=180.0, trip_speed_min=12.0, trip_speed_max=16.0)
    em = _jsonl(tmp_path / "run" / "ground_truth" / "gt_emissions_sample.jsonl")
    sp = [e["claimed_speed"] for e in em if not e["is_attacker"]]
    assert statistics.pstdev(sp) > 1.5, "car-following should spread speeds (free flow vs queues)"
    assert any(s < 5.0 for s in sp), "some vehicles must be slowed/queued"


def test_flow_detection_realistic_under_car_following(tmp_path):
    """At a normal density the MA still separates attackers from congested benign traffic."""
    _flow(tmp_path, car_following=True, arrival_rate=2.0, grid_w=6, grid_h=6, grid_block_m=140.0,
          duration_s=250.0, attacker_pct=0.15)
    s, _ = V.validate(str(tmp_path / "run"))
    assert s["precision"] is not None and s["precision"] >= 0.75, s
    assert s["recall"] is not None and s["recall"] >= 0.4, s


def test_demand_profiles_shape_arrivals(tmp_path):
    """'rush' concentrates arrivals into peaks; 'uniform' spreads them evenly."""
    import statistics

    def per_minute(profile):
        r = _flow(tmp_path / profile, demand_profile=profile, arrival_rate=4.0, duration_s=300.0)
        veh = _jsonl(tmp_path / profile / "run" / "ground_truth" / "gt_vehicle.jsonl")
        buckets = [0] * 5
        for v in veh:
            b = min(4, int(v["spawn_time"] // 60))
            buckets[b] += 1
        return buckets

    uni, rush = per_minute("uniform"), per_minute("rush")
    # coefficient of variation across the run is higher for rush (peaky) than uniform (flat)
    cv = lambda b: statistics.pstdev(b) / max(1e-9, statistics.mean(b))
    assert cv(rush) > cv(uni), f"rush should be peakier than uniform: rush={rush} uni={uni}"


def test_heterogeneous_fleet_types_and_kinematics(tmp_path):
    """A 'mixed' fleet spawns multiple vehicle classes; heavier classes drive slower."""
    import collections
    import statistics
    _flow(tmp_path, fleet="mixed", arrival_rate=3.0, duration_s=200.0, attacker_pct=0.1)
    veh = _jsonl(tmp_path / "run" / "ground_truth" / "gt_vehicle.jsonl")
    mix = collections.Counter(v.get("veh_type") for v in veh)
    assert len(mix) >= 3, f"mixed fleet should include several classes, got {mix}"
    type_of = {v["true_vehicle_id"]: v.get("veh_type") for v in veh}
    speeds = collections.defaultdict(list)
    for e in _jsonl(tmp_path / "run" / "ground_truth" / "gt_emissions_sample.jsonl"):
        if not e["is_attacker"]:
            speeds[type_of.get(e["true_vehicle_id"])].append(e["claimed_speed"])
    if speeds.get("car") and speeds.get("truck"):
        assert statistics.mean(speeds["truck"]) < statistics.mean(speeds["car"]) + 1.0


def test_map_offroad_detector_catches_position_offsets(tmp_path):
    """On a known road grid, a constant position OFFSET puts the claim off-road -> now detectable
    (it evaded the relative detectors), while honest on-road traffic is not flagged."""
    r = _flow(tmp_path, attack_type="ConstPosOffset", attack_types=("ConstPosOffset",),
              attacker_pct=0.2, faulty_pct=0.0, duration_s=200.0, arrival_rate=2.5)
    reasons = collections.Counter()
    for ln in open(tmp_path / "run" / "ma" / "ma_reports.jsonl", encoding="utf-8"):
        reasons.update(json.loads(ln).get("reason_codes", []))
    assert reasons["mapOffRoad"] > 0, "off-road offset should trip the map detector"
    s = V.validate(str(tmp_path / "run"))[0]
    assert s["recall"] is not None and s["recall"] >= 0.6, s   # was ~0 before the map detector


def test_multilane_reduces_gridlock(tmp_path):
    """At high density, parallel lanes let faster vehicles overtake slow ones -> higher mean speed
    than a single-lane road (which serializes behind the slowest vehicle)."""
    import statistics

    def mean_speed(lanes):
        _flow(tmp_path / str(lanes), n_lanes=lanes, car_following=True, arrival_rate=5.0,
              duration_s=150.0, grid_w=5, grid_h=5, grid_block_m=120.0, attacker_pct=0.1,
              radio_range_m=200.0)
        em = _jsonl(tmp_path / str(lanes) / "run" / "ground_truth" / "gt_emissions_sample.jsonl")
        sp = [e["claimed_speed"] for e in em if not e["is_attacker"]]
        return statistics.mean(sp) if sp else 0.0

    assert mean_speed(3) > mean_speed(1) + 1.0, "multi-lane should relieve single-lane gridlock"


def test_turn_slowdown_dips_speed_at_corners(tmp_path):
    """Cornering (opt-in) makes vehicles slow into sharp bends. At LOW density (no congestion to
    confound it), honest speeds dip toward the turn cap only when it is enabled."""
    def low_speeds(turn):
        # arrival_rate low + fast free speed: without turns honest traffic runs near free speed,
        # so any slow honest samples come from the corner cap, not from queueing.
        _flow(tmp_path / str(turn), turn_slowdown=turn, car_following=True, arrival_rate=0.6,
              duration_s=220.0, grid_w=6, grid_h=6, grid_block_m=140.0, attacker_pct=0.0,
              faulty_pct=0.0, trip_speed_min=16.0, trip_speed_max=20.0)
        em = _jsonl(tmp_path / str(turn) / "run" / "ground_truth" / "gt_emissions_sample.jsonl")
        sp = [e["claimed_speed"] for e in em if not e["is_attacker"]]
        frac_slow = sum(1 for s in sp if s < 10.0) / max(1, len(sp))
        return frac_slow, (min(sp) if sp else 99.0)

    on_frac, on_min = low_speeds(True)
    off_frac, off_min = low_speeds(False)
    assert on_frac > off_frac + 0.05, f"cornering should add slow samples: on={on_frac} off={off_frac}"
    assert on_min < off_min - 1.0, f"cornering should push a lower min speed: on={on_min} off={off_min}"


def test_traffic_lights_create_stops_at_intersections(tmp_path):
    """Signalized intersections make vehicles fully stop at reds (queues), unlike free-flow."""
    def stopped_fraction(lights):
        _flow(tmp_path / str(lights), traffic_lights=lights, car_following=True, arrival_rate=3.0,
              duration_s=180.0, grid_w=6, grid_h=6, attacker_pct=0.1)
        em = _jsonl(tmp_path / str(lights) / "run" / "ground_truth" / "gt_emissions_sample.jsonl")
        sp = [e["claimed_speed"] for e in em if not e["is_attacker"]]
        return sum(1 for s in sp if s < 1.0) / max(1, len(sp))

    assert stopped_fraction(True) > stopped_fraction(False) + 0.03, "reds should fully stop vehicles"


def test_flow_vehicles_follow_grid_routes(tmp_path):
    """Sampled true positions stay within the grid road network bounds (routed mobility)."""
    _flow(tmp_path, grid_w=5, grid_h=5, grid_block_m=120.0)
    emis = _jsonl(tmp_path / "run" / "ground_truth" / "gt_emissions_sample.jsonl")
    assert emis
    span = 4 * 120.0  # (grid-1) * block
    assert all(-5 <= e["true_x"] <= span + 5 and -5 <= e["true_y"] <= span + 5 for e in emis)
