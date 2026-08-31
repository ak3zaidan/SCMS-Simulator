"""Tests for opt-in gap-acceptance / yielding at UNSIGNALIZED intersections (audit gap #7 remainder).

Without this feature crossing streams at an uncontrolled junction are mutually invisible (the IDM leader
search rejects >45-deg heading differences), so two cross streams drive THROUGH each other with no yield.
The feature adds a deterministic first-come yield rule (the vehicle closest to the node has priority, ties
broken by vehicle id; a lower-priority vehicle treats the node as a virtual stopped leader until the
higher-priority conflicting vehicle clears). It is DEFAULT OFF, so every existing dataset stays
byte-identical; when ON it must produce observable, deterministic slowing/yielding at intersections,
prevent cross streams from crossing at speed simultaneously, avoid deadlock, and NOT collapse revocation
precision. The priority is a strict total order, so the yield relation is acyclic -> no gridlock.
"""

import json
import math
import pathlib

import pytest

import scms_sim_ref.mock_pipeline.run as run_mod
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.datagen import validate as V

# Frozen goldens (recorded on the base commit BEFORE this feature). Default OFF => byte-identical.
# ADR 0002 re-pin (true_speed/true_heading added to gt_emissions_sample):
# superseded 04ae9736f519... (default-config run; behaviour unchanged).
DEFAULT_DIGEST = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"
# ADR 0002 re-pin (true_speed/true_heading added to gt_emissions_sample):
# superseded b0bae9e4fc04... (grid+traffic-lights run; behaviour unchanged).
LIGHTS_DIGEST = "fe1a58002f468b3124aa24fc26681fb9e69bb63df71e543650289e2034f699e6"


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _default_cfg(out_dir, **over):
    kw = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
              grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=out_dir)
    kw.update(over)
    return PipelineConfig(**kw)


def _lights_cfg(out_dir, **over):
    return _default_cfg(out_dir, traffic_lights=True, **over)


# Moderate-density UNSIGNALIZED grid used for the kinematic ON-vs-OFF comparisons. emit_sample_prob=1.0
# emits every benign broadcast (true position + true speed), giving a full deterministic kinematic trace.
def _flow_cfg(out_dir, gap):
    return PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=90,
                          arrival_rate=3.0, grid_w=5, grid_h=5, attacker_pct=0.1,
                          emit_sample_prob=1.0, gap_acceptance=gap, out_dir=out_dir)


def _grid_nodes(cfg):
    b = cfg.grid_block_m
    return [(i * b, j * b) for i in range(cfg.grid_w) for j in range(cfg.grid_h)]


def _kinematics(out_dir, gap):
    """Run once (with the yield telemetry hook) and return the metrics both ON/OFF comparisons need."""
    cfg = _flow_cfg(out_dir, gap)
    yields = []
    run_mod.GAP_YIELD_HOOK = yields.append
    try:
        res = run_pipeline(cfg)
    finally:
        run_mod.GAP_YIELD_HOOK = None
    em = _jsonl(pathlib.Path(res.out_dir) / "ground_truth" / "gt_emissions_sample.jsonl")
    # benign, non-falsified broadcasts -> claimed_speed == true speed, true_x/true_y are exact positions
    ben = [e for e in em if not e["is_attacker"] and not e["is_faulty"] and not e["falsified"]]
    spd = [e["claimed_speed"] for e in ben]
    nodes = _grid_nodes(cfg)

    def nearest_node(x, y):
        n = min(nodes, key=lambda c: math.hypot(x - c[0], y - c[1]))
        return n, math.hypot(x - n[0], y - n[1])

    # "cross streams driving through each other at speed": two DISTINCT benign vehicles within 12 m of the
    # SAME node, within 10 m of each other, BOTH still moving (>3 m/s) at the same timestep. Gap-acceptance
    # serializes conflicting movements (the yielder is slow/stopped), so this must drop sharply when ON.
    by_node_t = {}
    for e in ben:
        node, d = nearest_node(e["true_x"], e["true_y"])
        if d < 12.0:
            by_node_t.setdefault((e["t"], node), []).append(
                (e["true_x"], e["true_y"], e["claimed_speed"], e["true_vehicle_id"]))
    both_moving = 0
    for pts in by_node_t.values():
        for a in range(len(pts)):
            for b in range(a + 1, len(pts)):
                if pts[a][3] == pts[b][3]:
                    continue
                if (math.hypot(pts[a][0] - pts[b][0], pts[a][1] - pts[b][1]) < 10.0
                        and pts[a][2] > 3.0 and pts[b][2] > 3.0):
                    both_moving += 1
    return dict(
        mean_speed=sum(spd) / len(spd),
        near_zero=sum(1 for s in spd if s < 0.5),
        both_moving=both_moving,
        n_yields=len(yields),
        yield_vids=len({y["vid"] for y in yields}),
        n_benign=len(ben),
    )


@pytest.fixture(scope="module")
def kin(tmp_path_factory):
    """Run the moderate-density grid once OFF and once ON; share the metrics across tests (fast)."""
    base = tmp_path_factory.mktemp("gap_kin")
    return {False: _kinematics(str(base / "off"), False),
            True: _kinematics(str(base / "on"), True)}


# --------------------------------------------------------------------------- #
# 1. Determinism firewall: default OFF => every existing digest stays byte-identical
# --------------------------------------------------------------------------- #
def test_config_default_off():
    # guards against an accidental default flip that would break every existing dataset
    assert PipelineConfig.gap_acceptance is False


def test_default_golden_digest_unchanged(tmp_path):
    assert run_pipeline(_default_cfg(str(tmp_path / "d"))).data_digest == DEFAULT_DIGEST


def test_gap_off_explicit_matches_default(tmp_path):
    assert run_pipeline(_default_cfg(str(tmp_path / "d"), gap_acceptance=False)).data_digest == DEFAULT_DIGEST


def test_traffic_lights_golden_unchanged(tmp_path):
    assert run_pipeline(_lights_cfg(str(tmp_path / "l"))).data_digest == LIGHTS_DIGEST


def test_traffic_lights_with_gap_defers_to_signals(tmp_path):
    # Interaction: with traffic_lights ON every node is signalized, so gap-acceptance defers ENTIRELY to
    # the signal (no unsignalized node to govern) -> byte-identical to the traffic-lights golden. This is
    # the documented coexistence: gap_acceptance primarily targets the traffic_lights=false case.
    res = run_pipeline(_lights_cfg(str(tmp_path / "lg"), gap_acceptance=True))
    assert res.data_digest == LIGHTS_DIGEST


# --------------------------------------------------------------------------- #
# 2. When ON: vehicles measurably slow / stop near unsignalized intersections
# --------------------------------------------------------------------------- #
def test_gap_slows_vehicles_near_intersections(kin):
    off, on = kin[False], kin[True]
    # mean benign speed drops (yielding + queuing at junctions that used to be driven straight through)
    assert on["mean_speed"] < off["mean_speed"], (on["mean_speed"], off["mean_speed"])
    assert on["mean_speed"] < 0.95 * off["mean_speed"]
    # and many more benign vehicles actually reach ~0 near a node (they stop to yield), vs OFF where the
    # cross streams never stop for one another
    assert on["near_zero"] > 1.5 * off["near_zero"], (on["near_zero"], off["near_zero"])


def test_gap_produces_deterministic_yields(kin):
    off, on = kin[False], kin[True]
    # OFF: the yield code path is never reached -> zero telemetry (proves the off path is untouched)
    assert off["n_yields"] == 0
    # ON: many yields spread across many vehicles
    assert on["n_yields"] > 300, on["n_yields"]
    assert on["yield_vids"] > 30, on["yield_vids"]


# --------------------------------------------------------------------------- #
# 3. No through-each-other: conflicting streams no longer cross at speed simultaneously
# --------------------------------------------------------------------------- #
def test_no_through_each_other(kin):
    off, on = kin[False], kin[True]
    assert off["both_moving"] > 0, "sanity: OFF must exhibit simultaneous cross-traffic crossings"
    # gap-acceptance serializes the conflict -> far fewer simultaneous at-speed crossings at a node
    assert on["both_moving"] < off["both_moving"], (on["both_moving"], off["both_moving"])
    assert on["both_moving"] < 0.9 * off["both_moving"]


# --------------------------------------------------------------------------- #
# 4. No deadlock: a dense run still flows (vehicles despawn; traffic keeps moving; run terminates)
# --------------------------------------------------------------------------- #
def test_no_deadlock_dense_run(tmp_path):
    cfg = PipelineConfig(seed=11, traffic_flow=True, road_network="grid", duration_s=120,
                         arrival_rate=5.0, grid_w=5, grid_h=5, attacker_pct=0.1,
                         emit_sample_prob=1.0, gap_acceptance=True, out_dir=str(tmp_path / "dense"))
    res = run_pipeline(cfg)                       # returns normally within the step budget (no hang)
    em = _jsonl(pathlib.Path(res.out_dir) / "ground_truth" / "gt_emissions_sample.jsonl")
    ben = [e for e in em if not e["is_attacker"] and not e["is_faulty"] and not e["falsified"]]
    assert ben, "dense run produced no benign traffic"
    # throughput > 0: benign vehicles whose last emission is well before the end have despawned (completed)
    last = {}
    for e in ben:
        last[e["true_vehicle_id"]] = max(last.get(e["true_vehicle_id"], 0.0), e["t"])
    completed = sum(1 for tmax in last.values() if tmax < cfg.duration_s - 15)
    assert completed > 30, f"throughput collapsed (deadlock?): only {completed} completed trips"
    # and traffic is still MOVING late in the run (not frozen in permanent gridlock)
    late = [e["claimed_speed"] for e in ben if e["t"] > cfg.duration_s - 30]
    assert late and sum(late) / len(late) > 0.5, "late-run traffic is frozen (gridlock)"


# --------------------------------------------------------------------------- #
# 5. No precision collapse: benign-heavy run keeps revocation precision high with gap-acceptance ON
# --------------------------------------------------------------------------- #
def _precision_and_fp(out_dir, gap):
    cfg = PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=200,
                         arrival_rate=2.0, grid_w=6, grid_h=6, grid_block_m=140.0, n_lanes=3,
                         attacker_pct=0.1, faulty_pct=0.0, radio_range_m=250.0,
                         gap_acceptance=gap, out_dir=out_dir)
    res = run_pipeline(cfg)
    stats, _ = V.validate(res.out_dir)
    veh = _jsonl(pathlib.Path(res.out_dir) / "ground_truth" / "gt_vehicle.jsonl")
    attackers = {v["true_vehicle_id"] for v in veh if v.get("is_attacker")}
    revoked = {r["true_vehicle_id"] for r in
               _jsonl(pathlib.Path(res.out_dir) / "ground_truth" / "gt_linkage_revocation.jsonl")}
    return stats["precision"], stats["recall"], len(revoked - attackers)


def test_gap_does_not_collapse_revocation_precision(tmp_path):
    p_off, r_off, fp_off = _precision_and_fp(str(tmp_path / "off"), False)
    p_on, r_on, fp_on = _precision_and_fp(str(tmp_path / "on"), True)
    print(f"\n[gap_acceptance] benign-heavy precision  OFF={p_off:.3f} ON={p_on:.3f}  "
          f"(recall {r_off:.3f}->{r_on:.3f}, benign FP {fp_off}->{fp_on})")
    # precision stays high with the realistic slowing/yielding ON ...
    assert p_on >= 0.9, f"precision collapsed with gap-acceptance: {p_on} (off={p_off})"
    # ... and no worse than with it off (the sustained-evidence MA gate absorbs the benign transient)
    assert p_on >= p_off - 0.03, f"gap-acceptance degraded precision: on={p_on} off={p_off}"
    assert fp_on <= fp_off + 1, f"gap-acceptance added benign false revocations: on={fp_on} off={fp_off}"


# --------------------------------------------------------------------------- #
# 6. Determinism with the feature ON (same seed + config -> byte-identical)
# --------------------------------------------------------------------------- #
def test_gap_on_is_deterministic(tmp_path):
    a = run_pipeline(_flow_cfg(str(tmp_path / "a"), True)).data_digest
    b = run_pipeline(_flow_cfg(str(tmp_path / "b"), True)).data_digest
    assert a == b


def test_yield_hook_does_not_change_the_digest(tmp_path):
    # The telemetry seam must be side-effect free on the dataset (like PER_STEP_HOOK / LANE_CHANGE_HOOK).
    events = []
    run_mod.GAP_YIELD_HOOK = events.append
    try:
        with_hook = run_pipeline(_flow_cfg(str(tmp_path / "h"), True)).data_digest
    finally:
        run_mod.GAP_YIELD_HOOK = None
    without = run_pipeline(_flow_cfg(str(tmp_path / "n"), True)).data_digest
    assert with_hook == without
    assert events, "expected the hook to fire on an ON run"


# --------------------------------------------------------------------------- #
# 7. Validation: gap-acceptance is only meaningful with flow + a routed network
# --------------------------------------------------------------------------- #
def test_gap_requires_traffic_flow(tmp_path):
    with pytest.raises(ValueError, match="traffic_flow"):
        run_pipeline(PipelineConfig(seed=7, traffic_flow=False, gap_acceptance=True,
                                    out_dir=str(tmp_path / "x")))


def test_gap_rejects_linear_network(tmp_path):
    with pytest.raises(ValueError, match="routed network"):
        run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="linear",
                                    gap_acceptance=True, out_dir=str(tmp_path / "y")))
