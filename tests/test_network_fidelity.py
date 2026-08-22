"""Network-fidelity tests for two audit gaps:

GAP A -- signal phase is a STABLE per-node 2-colouring (net.node_phase) instead of grid-coordinate
         arithmetic, so traffic lights are coherent on ring/spider/custom maps (not coordinate noise).
GAP B -- GridNetwork/RingNetwork gained an OPT-IN per-edge speed hierarchy (arterial vs local on a
         grid; a single whole-ring cap on a ring). OFF by default -> byte-identical output.

Both features default OFF, so every historical digest must hold. The grid path (including grid
traffic lights) must stay byte-identical; the ring-with-lights digest intentionally changes because
GAP A replaces its previously-meaningless coordinate-derived phase.
"""
import json

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.roads import GridNetwork, RingNetwork, CustomNetwork, spider_graph


def _honest_speeds(out_dir):
    """Claimed speeds from the ground-truth emission sample (all vehicles honest in these runs, so
    claimed_speed tracks the true driven speed)."""
    ems = [json.loads(ln) for ln in
           open(out_dir / "ground_truth" / "gt_emissions_sample.jsonl", encoding="utf-8")]
    return [e["claimed_speed"] for e in ems if e.get("claimed_speed") is not None]


# --------------------------------------------------------------------------- #
# Golden digests -- default & grid signals byte-identical; plain ring byte-identical
# --------------------------------------------------------------------------- #
def test_golden_digests_unchanged(tmp_path):
    default = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp_path / "g"))).data_digest
    assert default == "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"

    grid_lights = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, traffic_lights=True,
        out_dir=str(tmp_path / "gl"))).data_digest
    assert grid_lights == "b0bae9e4fc04a5f5d43e4b8ab2714d23246bfcb502a27a4ed7646c3deb01a0b8"

    ring = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="ring", duration_s=60, arrival_rate=1.5,
        grid_w=8, grid_block_m=120.0, attacker_pct=0.25, out_dir=str(tmp_path / "r"))).data_digest
    assert ring == "ff1cddd82227d7aaa3b6d5f931ef0ee9313257cb1af7eb14752eda5aed4a989b"


def test_ring_lights_digest_intentionally_changed_and_stable(tmp_path):
    """GAP A: ring signals used to be grid-coordinate noise; they now use net.node_phase, so the
    ring-with-lights digest differs from the pre-fix value -- and is itself deterministic."""
    cfg = dict(seed=7, traffic_flow=True, road_network="ring", duration_s=60, arrival_rate=1.5,
               grid_w=8, grid_block_m=120.0, attacker_pct=0.25, traffic_lights=True)
    new = run_pipeline(PipelineConfig(**cfg, out_dir=str(tmp_path / "a"))).data_digest
    again = run_pipeline(PipelineConfig(**cfg, out_dir=str(tmp_path / "b"))).data_digest
    assert new == again                                        # deterministic
    assert new != "460b4cd04b0be927f494f63c1deccaa57cc51d8e098eaf60e998c1a8889675d7"  # pre-fix noise


# --------------------------------------------------------------------------- #
# GAP A -- node_phase 2-colouring per topology
# --------------------------------------------------------------------------- #
def test_grid_node_phase_is_the_historical_checkerboard():
    """The grid phase is exactly (i + j) % 2 recovered from the metre coordinate -- this is what
    keeps grid signal timing byte-identical."""
    g = GridNetwork(4, 4, 100.0)
    for i in range(4):
        for j in range(4):
            assert g.node_phase((i * 100.0, j * 100.0)) == (i + j) % 2
    # deterministic & stable across instances
    assert GridNetwork(4, 4, 100.0).node_phase((200.0, 100.0)) == g.node_phase((200.0, 100.0))


def test_ring_node_phase_alternates_around_the_ring():
    """Adjacent ring intersections get opposite phases (index parity) -> coherent alternation."""
    r = RingNetwork(10, 120.0)                                # even node count
    phases = [r.node_phase(r._coord(i)) for i in r.nodes]
    assert phases == [i % 2 for i in range(10)]
    for i in range(10):                                       # every neighbour pair alternates
        assert r.node_phase(r._coord(i)) != r.node_phase(r._coord((i + 1) % 10))


def test_custom_node_phase_is_a_proper_2colouring_of_bipartite_map():
    """On a bipartite custom map (a 4x4 grid graph) every edge joins opposite phases."""
    nodes, edges = [], []
    idx = {}
    for i in range(4):
        for j in range(4):
            idx[(i, j)] = len(nodes)
            nodes.append([i * 100.0, j * 100.0])
    for i in range(4):
        for j in range(4):
            if i < 3:
                edges.append([idx[(i, j)], idx[(i + 1, j)]])
            if j < 3:
                edges.append([idx[(i, j)], idx[(i, j + 1)]])
    net = CustomNetwork(nodes, edges)
    for a, b in net.edges:
        assert net.node_phase(net.coords[a]) != net.node_phase(net.coords[b])


def test_spider_node_phase_and_lights_run_completes(tmp_path):
    """A spider (radial) map is not bipartite, but node_phase is still a stable deterministic
    2-colouring in which the BFS-tree edges alternate, and a lit spider run completes cleanly."""
    net = CustomNetwork(*spider_graph(6, 2, 100.0))
    colour = net._colouring()
    # every node coloured, values are 0/1, and it is reproducible
    assert set(colour.values()) <= {0, 1} and len(colour) == len(net.nodes)
    assert net._colouring() == colour
    # spider radial spokes (centre -> ring) always alternate (they are BFS-tree edges from centre)
    centre = 0
    for nb, _w in net.adj[centre]:
        assert net.node_phase(net.coords[centre]) != net.node_phase(net.coords[nb])
    res = run_pipeline(PipelineConfig(
        seed=3, traffic_flow=True, road_network="spider", grid_w=6, grid_h=2, grid_block_m=120.0,
        duration_s=40, arrival_rate=1.5, attacker_pct=0.0, traffic_lights=True,
        out_dir=str(tmp_path / "sp")))
    assert res.data_digest                                    # completed, produced a dataset


def test_ring_traffic_lights_cause_stops_and_slow_traffic(tmp_path):
    """GAP A payoff: on a ring, turning lights ON (vs OFF) measurably lowers mean speed and makes
    some vehicles halt at intersections -- a coherent signal effect, not coordinate noise."""
    base = dict(seed=5, traffic_flow=True, road_network="ring", grid_w=16, grid_block_m=140.0,
                duration_s=90, arrival_rate=0.8, attacker_pct=0.0,
                trip_speed_min=12.0, trip_speed_max=16.0)
    run_pipeline(PipelineConfig(**base, out_dir=str(tmp_path / "off")))
    run_pipeline(PipelineConfig(**base, traffic_lights=True, light_cycle_s=24.0,
                                out_dir=str(tmp_path / "on")))
    off = _honest_speeds(tmp_path / "off")
    on = _honest_speeds(tmp_path / "on")
    mean_off = sum(off) / len(off)
    mean_on = sum(on) / len(on)
    stopped_on = sum(1 for s in on if s < 0.5)
    assert mean_on < mean_off - 1.0, (mean_on, mean_off)      # signals slow traffic
    assert stopped_on >= 1                                    # some vehicles halt at red signals


# --------------------------------------------------------------------------- #
# GAP B -- opt-in per-edge speed limits
# --------------------------------------------------------------------------- #
def test_arterial_feature_off_is_byte_identical(tmp_path):
    """The grid speed hierarchy is gated on arterial_every>0: setting the speeds but leaving
    arterial_every=0 leaves output byte-identical to a plain grid."""
    plain = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp_path / "p"))).data_digest
    speeds_but_off = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25,
        arterial_every=0, arterial_speed_mps=20.0, local_speed_mps=5.0,
        out_dir=str(tmp_path / "q"))).data_digest
    assert plain == speeds_but_off == \
        "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"
    # ring cap off (arterial_speed_mps=0) is byte-identical to the plain ring
    ring_off = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="ring", duration_s=60, arrival_rate=1.5,
        grid_w=8, grid_block_m=120.0, attacker_pct=0.25, arterial_speed_mps=0.0,
        out_dir=str(tmp_path / "ro"))).data_digest
    assert ring_off == "ff1cddd82227d7aaa3b6d5f931ef0ee9313257cb1af7eb14752eda5aed4a989b"


def test_grid_arterial_caps_are_attached_and_respected():
    """Model level: with the feature on, random_trip attaches per-edge caps; local edges carry the
    local limit and arterial edges the arterial limit. With it off, no caps are attached."""
    import random
    off = GridNetwork(6, 6, 120.0)
    assert off.random_trip(random.Random(1), 15.0, 0.0).caps is None      # OFF -> no caps
    on = GridNetwork(6, 6, 120.0, arterial_every=2, arterial_speed=16.0, local_speed=5.0)
    seen = set()
    for k in range(200):
        caps = on.random_trip(random.Random(k), 15.0, 0.0).caps
        if caps:
            seen.update(caps)
    assert 5.0 in seen and 16.0 in seen                                    # both tiers appear
    assert seen <= {5.0, 16.0}                                             # and only those limits
    # edge classification: a horizontal edge on an arterial row is arterial; an off-row local edge
    # is local (arterial_every=2 -> rows/cols 0,2,4 are arterials)
    assert on._edge_cap((0, 0), (1, 0)) == 16.0        # along row 0 (arterial)
    assert on._edge_cap((1, 1), (2, 1)) == 5.0         # along row 1 (local)
    assert on._edge_cap((0, 0), (0, 1)) == 16.0        # along column 0 (arterial)


def test_grid_arterial_lowers_mean_speed_end_to_end(tmp_path):
    """GAP B payoff: enabling low local-road limits measurably lowers mean claimed speed of honest
    vehicles across the grid (kinematics actually change)."""
    base = dict(seed=5, traffic_flow=True, road_network="grid", grid_w=6, grid_h=6,
                duration_s=90, arrival_rate=2.0, attacker_pct=0.0,
                trip_speed_min=12.0, trip_speed_max=16.0)
    run_pipeline(PipelineConfig(**base, out_dir=str(tmp_path / "base")))
    run_pipeline(PipelineConfig(**base, arterial_every=2, arterial_speed_mps=16.0,
                                local_speed_mps=5.0, out_dir=str(tmp_path / "art")))
    mean_base = sum(_honest_speeds(tmp_path / "base")) / len(_honest_speeds(tmp_path / "base"))
    art = _honest_speeds(tmp_path / "art")
    mean_art = sum(art) / len(art)
    assert mean_art < mean_base - 1.0, (mean_art, mean_base)
    # local roads capped at 5 m/s -> a real population of low-speed samples the base run lacks
    assert sum(1 for s in art if s <= 6.0) > sum(1 for s in _honest_speeds(tmp_path / "base")
                                                 if s <= 6.0)


def test_ring_speed_cap_lowers_mean_speed(tmp_path):
    """A whole-ring speed cap (the ring analogue of the arterial rule) measurably slows traffic."""
    base = dict(seed=5, traffic_flow=True, road_network="ring", grid_w=16, grid_block_m=140.0,
                duration_s=90, arrival_rate=0.8, attacker_pct=0.0,
                trip_speed_min=12.0, trip_speed_max=16.0)
    run_pipeline(PipelineConfig(**base, out_dir=str(tmp_path / "free")))
    run_pipeline(PipelineConfig(**base, arterial_speed_mps=6.0, out_dir=str(tmp_path / "cap")))
    mean_free = sum(_honest_speeds(tmp_path / "free")) / len(_honest_speeds(tmp_path / "free"))
    capped = _honest_speeds(tmp_path / "cap")
    mean_cap = sum(capped) / len(capped)
    assert mean_cap < mean_free - 1.0, (mean_cap, mean_free)
    assert max(capped) <= 6.5                                  # nothing meaningfully exceeds the cap


# --------------------------------------------------------------------------- #
# Determinism -- a lights + arterial run is byte-identical when repeated
# --------------------------------------------------------------------------- #
def test_lights_plus_arterial_run_is_deterministic(tmp_path):
    cfg = dict(seed=9, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
               grid_w=6, grid_h=6, attacker_pct=0.2, traffic_lights=True,
               arterial_every=2, arterial_speed_mps=16.0, local_speed_mps=6.0)
    a = run_pipeline(PipelineConfig(**cfg, out_dir=str(tmp_path / "d1"))).data_digest
    b = run_pipeline(PipelineConfig(**cfg, out_dir=str(tmp_path / "d2"))).data_digest
    assert a == b


def test_arterial_config_validation():
    """validate_config rejects out-of-range speed limits and negative spacing."""
    import pytest
    from scms_sim_ref.mock_pipeline.run import validate_config
    with pytest.raises(ValueError, match="arterial_every"):
        validate_config(PipelineConfig(arterial_every=-1))
    with pytest.raises(ValueError, match="arterial_speed_mps"):
        validate_config(PipelineConfig(arterial_speed_mps=100.0))
    with pytest.raises(ValueError, match="local_speed_mps"):
        validate_config(PipelineConfig(local_speed_mps=0.5))
    # a valid arterial config validates OK -- but the speed caps only apply to a grid road (the
    # topology that models arterials); on the default 'linear' road they are a rejected dead knob.
    validate_config(PipelineConfig(road_network="grid", arterial_every=2, arterial_speed_mps=16.0,
                                   local_speed_mps=5.0))
