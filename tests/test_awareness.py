"""Tests for `datagen/awareness.py` -- the like-for-like restatement of cooperative awareness.

Every geometric case is built so the correct answer is known BY CONSTRUCTION rather than by
running the thing under test twice:

  * two vehicles on the same straight street with nothing between them are LOS, always;
  * a wall placed across the segment between them is NLOSb, always, and moving it off the segment
    makes them LOS again;
  * a truck parked exactly on the midpoint of the segment, with no buildings anywhere, is NLOSv --
    and stepping it sideways by more than the blocker half-width makes them LOS;
  * a 2 x N grid of vehicles laid out so that exactly K of the pairs cross a wall has a link-state
    composition of exactly K/total NLOSb.

The propagation model is checked the same way: `propagation_pdr` is compared against an independent
Monte-Carlo of the ENGINE'S OWN draw sequence (`run.GeometricChannel.evaluate_raw`'s arithmetic,
re-rolled here with `random.Random`), so the quadrature is validated against the code it claims to
integrate rather than against a second copy of its own formula.
"""
from __future__ import annotations

import json
import math
import os
import random

import numpy as np
import pytest

from scms_sim_ref.datagen import awareness as aw
from scms_sim_ref.datagen import realism_bench as rb
from scms_sim_ref.mock_pipeline.run import (TR37885_NLOSV, TR37885_SHADOW_SIGMA_DB,
                                            nakagami_m_for_distance, tr37885_nlosv_mu_db,
                                            tr37885_pathloss_db)

COND = aw.load_conditions()


# ==================================================================================================
# the reference's own arithmetic (Boban & d'Orey eq. 3/4)
# ==================================================================================================
def test_nar_pdr_roundtrip():
    for z in (1.0, 2.1365, 4.2768, 5.4579, 8.2886):
        for p in (0.05, 0.2427, 0.5, 0.9):
            assert aw.pdr_for_nar(aw.nar_from_pdr(p, z), z) == pytest.approx(p, abs=1e-8)


def test_nar_saturates_and_the_inverse_says_so():
    """A recorded LIMIT of eq. (4), not a defect: several shots at a high per-packet PDR give a NAR
    of 1.0 to double precision, so NAR carries no information about the channel up there. It is why
    the reference reports the DISTANCE at which NAR falls below 0.90 rather than a NAR value, and
    why this module grades a crossing distance too."""
    assert aw.nar_from_pdr(0.999, 8.2886) == 1.0
    assert aw.pdr_for_nar(1.0, 8.2886) == 1.0


def test_nar_equals_pdr_at_one_shot():
    """Z = 1 is the whole point: a one-CAM-per-window engine's awareness IS its per-packet PDR."""
    for p in (0.01, 0.115, 0.5, 0.9):
        assert aw.nar_from_pdr(p, 1.0) == pytest.approx(p)


def test_published_z_values_invert_to_the_documented_per_packet_levels():
    """NAR 0.90 is a per-packet PDR of about a THIRD, not nine tenths. The four numbers below are
    what make the retired >= 0.90 gate a category error rather than a strict one."""
    assert aw.pdr_for_nar(0.90, 2.1365) == pytest.approx(0.6596, abs=5e-4)
    assert aw.pdr_for_nar(0.90, 4.2768) == pytest.approx(0.4163, abs=5e-4)
    assert aw.pdr_for_nar(0.90, 5.4579) == pytest.approx(0.3442, abs=5e-4)
    assert aw.pdr_for_nar(0.90, 8.2886) == pytest.approx(0.2427, abs=5e-4)


def test_z_for_engine_counts_cams_in_the_window():
    assert aw.z_for_engine(1.0, 1.0) == 1.0            # mock_pipeline default: ONE shot
    assert aw.z_for_engine(0.1, 1.0) == 10.0           # the reference's 10 Hz
    assert aw.z_for_engine(0.1, 1.0, z_cap=5.4579) == pytest.approx(5.4579)
    assert aw.z_for_engine(2.0, 1.0) == 1.0            # never below one shot


def test_link_budget_and_reference_interpolation():
    assert aw.link_budget_db(23.0, -81.0) == 104.0
    assert aw.link_budget_db(15.0, -95.0) == 110.0     # the reference's own 200 m configuration
    # the pinned points must round-trip exactly
    for tx, budget, dist in ((5.0, 100.0, 50.0), (15.0, 110.0, 200.0), (23.0, 118.0, 300.0)):
        d, _ = aw.reference_nar90_distance_m(budget, "urban", COND)
        assert d == pytest.approx(dist, rel=1e-9)
    d, note = aw.reference_nar90_distance_m(104.0, "urban", COND)
    assert d == pytest.approx(87.06, abs=0.1)          # the harness's own 23 dBm / -81 dBm budget
    assert "interpolation" in note


def test_reference_curve_is_not_extrapolated_and_does_not_cross_regimes():
    assert aw.reference_nar90_distance_m(94.0, "urban", COND)[0] is None
    assert aw.reference_nar90_distance_m(140.0, "urban", COND)[0] is None
    # the urban curve must never be applied to a highway scene
    d, note = aw.reference_nar90_distance_m(104.0, "highway", COND)
    assert d is None and "ONE highway point" in note
    assert aw.reference_nar90_distance_m(115.0, "highway", COND)[0] == pytest.approx(400.0)


def test_gray_zone_ratio():
    assert aw.gray_zone_ratio(100.0, 241.0) == pytest.approx(2.41)
    assert aw.gray_zone_ratio(None, 200.0) is None
    assert aw.gray_zone_ratio(0.0, 200.0) is None


# ==================================================================================================
# refdata: the conditions file must actually carry what the module reads out of it
# ==================================================================================================
def test_conditions_refdata_is_present_and_cited():
    assert COND.get("reference_set") == "v2x_awareness_conditions"
    ents = COND["entries"]
    for key in ("nar_definition", "nar_pair_population_measured", "measurement_radio_conditions",
                "nar_90pct_crossing_distance_m", "nar_shot_multiplicity_z", "simulation_conditions",
                "simulated_urban_nar90_by_link_budget", "simulated_highway_nar90",
                "universal_awareness_floor", "harness_comparison_recipe"):
        assert key in ents, key
        assert ents[key].get("cite"), key
        assert ents[key].get("source"), key
    # the single measured urban cell the retired gate rested on
    urban = [p for p in ents["nar_90pct_crossing_distance_m"]["points"] if p[0] == "urban_v2v"]
    assert urban == [["urban_v2v", "Finland", 200.0]]
    # the reference's own receiver, which the harness's -81 dBm is 14 dB short of
    assert ents["simulation_conditions"]["value"]["rx_sensitivity_dbm"] == -95.0
    assert ents["simulation_conditions"]["value"]["interference_modelled"] is False


def test_conditions_refdata_loads_through_the_harness_lookup():
    """The new file has to be visible to `realism_bench.load_refdata`, or the citations vanish."""
    rd = rb.load_refdata()
    assert "v2x_awareness_conditions" in rd["sets"]
    assert "v2x_awareness_conditions.nar_shot_multiplicity_z" in rd["entries"]


# ==================================================================================================
# numerics
# ==================================================================================================
def test_normal_cdf_matches_math_erf():
    xs = np.linspace(-9.0, 9.0, 20001)
    exact = np.array([0.5 * (1.0 + math.erf(x / math.sqrt(2.0))) for x in xs])
    assert np.max(np.abs(aw._norm_cdf(xs) - exact)) < 1e-7


def test_fade_quadrature_is_a_unit_mean_probability_measure():
    for m in (1.0, 1.5, 3.0):
        g, w = aw._fade_quadrature(m)
        assert w.sum() == pytest.approx(1.0, abs=1e-12)           # normalised by construction
        assert float((g * w).sum()) == pytest.approx(1.0, rel=2e-3)   # Gamma(m, 1/m) has mean 1
        assert float((g * g * w).sum() - 1.0) == pytest.approx(1.0 / m, rel=5e-3)   # var = 1/m


def test_nlosv_quadrature_carries_the_censoring_atom():
    mu, sig = TR37885_NLOSV["both_below"]
    nodes, w = aw._nlosv_quadrature(mu, sig)
    assert w.sum() == pytest.approx(1.0, abs=1e-9)
    assert nodes[0] == 0.0
    # P(L = 0) = Phi(-mu/sigma); with mu = 9.0, sigma = 4.5 that is Phi(-2) = 0.02275
    assert w[0] == pytest.approx(0.02275, abs=1e-4)
    assert float((nodes * w).sum()) > mu                          # censoring raises the mean


def _mc_pdr(state, d, tx, floor, env="urban", n=200_000, seed=11):
    """Monte-Carlo of the ENGINE's own draw (run.GeometricChannel.evaluate_raw arithmetic)."""
    rng = random.Random(seed)
    los = "urban_los" if env == "urban" else "highway_los"
    pl0 = tr37885_pathloss_db("urban_nlos" if state == "NLOSb" else los, d)
    sig = TR37885_SHADOW_SIGMA_DB[state]
    m = nakagami_m_for_distance(d)
    mu_base, sv = TR37885_NLOSV["both_below"]
    mu = tr37885_nlosv_mu_db(mu_base, d)
    hits = 0
    for _ in range(n):
        pl = pl0 + (max(0.0, rng.gauss(mu, sv)) if state == "NLOSv" else 0.0)
        rx = (tx - pl + rng.gauss(0.0, sig)
              + 10.0 * math.log10(max(rng.gammavariate(m, 1.0 / m), 1e-12)))
        hits += rx >= floor
    return hits / n


@pytest.mark.parametrize("state", ["LOS", "NLOSv", "NLOSb"])
@pytest.mark.parametrize("d", [25.0, 75.0, 175.0, 275.0])
def test_propagation_pdr_matches_the_engines_own_draw(state, d):
    q = aw.propagation_pdr(state, d, tx_power_dbm=23.0, decode_floor_dbm=-81.0)
    mc = _mc_pdr(state, d, 23.0, -81.0)
    assert q == pytest.approx(mc, abs=0.006)          # ~5 MC standard errors at n = 2e5


def test_propagation_pdr_is_monotone_in_distance_and_in_power():
    for state in ("LOS", "NLOSv", "NLOSb"):
        vals = [aw.propagation_pdr(state, d, tx_power_dbm=23.0, decode_floor_dbm=-81.0)
                for d in (10.0, 50.0, 100.0, 200.0, 400.0, 800.0)]
        assert all(a >= b - 1e-9 for a, b in zip(vals, vals[1:])), (state, vals)
    lo = aw.propagation_pdr("NLOSb", 150.0, tx_power_dbm=13.0, decode_floor_dbm=-81.0)
    hi = aw.propagation_pdr("NLOSb", 150.0, tx_power_dbm=33.0, decode_floor_dbm=-81.0)
    assert hi > lo


def test_decode_floor_is_the_engines_floor_not_the_raw_sensitivity():
    assert aw.decode_floor_dbm(-81.0) == -81.0        # sensitivity binds
    assert aw.decode_floor_dbm(-120.0) == -106.0      # noise + SNIR threshold binds


def test_unknown_link_state_is_refused():
    with pytest.raises(ValueError):
        aw.propagation_pdr("TELEPATHY", 100.0, tx_power_dbm=23.0, decode_floor_dbm=-81.0)


# ==================================================================================================
# geometry: synthetic scenes whose composition is known by construction
# ==================================================================================================
def _scenario(emissions, *, buildings=(), veh_types=None, tx=23.0, sens=-81.0,
              env="urban", density=0.0, dt=1.0):
    """A `load_scenario`-shaped dict built in memory, so no dataset directory is needed."""
    from scms_sim_ref.mock_pipeline.run import TR37885_BLOCKER_HEIGHT_M
    heights = {vid: float(TR37885_BLOCKER_HEIGHT_M.get(t, 1.6))
               for vid, t in (veh_types or {}).items()}
    return {"dataset_dir": "<memory>", "config": {}, "buildings": [list(b) for b in buildings],
            "buildings_source": "synthetic", "blocker_height_m": heights,
            "emissions": emissions, "reports": [], "report_labels": [],
            "radio_model": "geometric", "tx_power_dbm": tx, "rx_sensitivity_dbm": sens,
            "radio_env": env, "nlosb_density_per_km": density, "dt_s": dt,
            "emit_sample_prob": 1.0}


def _emit(pairs, t=0.0):
    return [{"t": t, "true_vehicle_id": vid, "true_x": float(x), "true_y": float(y)}
            for vid, x, y in pairs]


def _wall(cx, cy, half_x=1.5, half_y=30.0):
    """An axis-aligned rectangular footprint centred on (cx, cy)."""
    return [(cx - half_x, cy - half_y), (cx + half_x, cy - half_y),
            (cx + half_x, cy + half_y), (cx - half_x, cy + half_y)]


def test_two_vehicles_on_an_empty_street_are_LOS():
    sc = _scenario(_emit([("veh_000", 0.0, 0.0), ("veh_001", 120.0, 0.0)]),
                   veh_types={"veh_000": "car", "veh_001": "car"})
    comp = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=300.0)
    assert comp["n_classified"] == 1
    assert comp["overall"]["LOS"] == pytest.approx(1.0)
    assert comp["count"]["LOS"][2] == 1               # 100-150 m band


def test_a_wall_across_the_segment_makes_the_pair_NLOSb():
    sc = _scenario(_emit([("veh_000", 0.0, 0.0), ("veh_001", 120.0, 0.0)]),
                   buildings=[_wall(60.0, 0.0)],
                   veh_types={"veh_000": "car", "veh_001": "car"})
    comp = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=300.0)
    assert comp["overall"]["NLOSb"] == pytest.approx(1.0)
    assert comp["count"]["NLOSb"][2] == 1


def test_the_same_wall_moved_off_the_segment_restores_LOS():
    sc = _scenario(_emit([("veh_000", 0.0, 0.0), ("veh_001", 120.0, 0.0)]),
                   buildings=[_wall(60.0, 200.0)],
                   veh_types={"veh_000": "car", "veh_001": "car"})
    comp = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=300.0)
    assert comp["overall"]["LOS"] == pytest.approx(1.0)


def test_a_wall_at_the_antenna_is_ignored_by_the_endpoint_clearance():
    """`GEO_ENDPOINT_CLEAR_M` exists because the road graph is RDP-simplified and the raster has a
    halo, so a footprint within 6 m of an antenna must not black out the link. Pinning it here
    keeps the composition consistent with the channel that produced the dataset."""
    sc = _scenario(_emit([("veh_000", 0.0, 0.0), ("veh_001", 120.0, 0.0)]),
                   buildings=[_wall(2.0, 0.0, half_x=1.0)],
                   veh_types={"veh_000": "car", "veh_001": "car"})
    comp = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=300.0)
    assert comp["overall"]["LOS"] == pytest.approx(1.0)


def test_a_truck_on_the_segment_makes_the_pair_NLOSv_and_stepping_it_aside_does_not():
    on = _scenario(_emit([("veh_000", 0.0, 0.0), ("veh_001", 120.0, 0.0),
                          ("veh_002", 60.0, 0.0)]),
                   veh_types={"veh_000": "car", "veh_001": "car", "veh_002": "truck"})
    comp = aw.link_state_composition(on, bin_m=50.0, max_dist_m=300.0)
    # 3 pairs: (0,1) blocked by the truck -> NLOSv; (0,2) and (1,2) are 60 m and clear -> LOS
    assert comp["count"]["NLOSv"].sum() == 1
    assert comp["count"]["LOS"].sum() == 2

    off = _scenario(_emit([("veh_000", 0.0, 0.0), ("veh_001", 120.0, 0.0),
                           ("veh_002", 60.0, 5.0)]),
                    veh_types={"veh_000": "car", "veh_001": "car", "veh_002": "truck"})
    comp2 = aw.link_state_composition(off, bin_m=50.0, max_dist_m=300.0)
    assert comp2["count"]["NLOSv"].sum() == 0
    assert comp2["count"]["LOS"].sum() == 3


def test_a_blocker_behind_the_transmitter_does_not_block():
    """`tallest_blocker` requires 0 < s < 1: a truck 30 m BEHIND one antenna is not in the way of
    the 120 m link between the two cars. (It IS in the way of the 150 m truck-to-far-car link,
    which the third assertion pins, so the test also proves the geometry is not simply inert.)"""
    sc = _scenario(_emit([("veh_000", 0.0, 0.0), ("veh_001", 120.0, 0.0),
                          ("veh_002", -30.0, 0.0)]),
                   veh_types={"veh_000": "car", "veh_001": "car", "veh_002": "truck"})
    comp = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=300.0)
    assert int(comp["count"]["LOS"][2]) == 1        # 100-150 m: the two cars, truck behind
    assert int(comp["count"]["NLOSv"][2]) == 0
    assert int(comp["count"]["NLOSv"][3]) == 1      # 150-200 m: veh_000 sits between truck and car


def test_known_composition_of_a_two_row_scene():
    """Two rows of three vehicles either side of one long wall. The decomposition is exact:
    9 cross-row pairs all cross the wall (NLOSb); of the 6 same-row pairs, the 2 end-to-end ones
    have the middle vehicle exactly on the line (NLOSv, since a car roof at 1.6 m is above the
    1.5 m antenna) and the remaining 4 are clear (LOS). 9 + 2 + 4 = 15 = C(6,2)."""
    rows = []
    for i in range(3):
        rows.append((f"veh_n{i}", 20.0 * i, 40.0))
        rows.append((f"veh_s{i}", 20.0 * i, -40.0))
    sc = _scenario(_emit(rows), buildings=[_wall(20.0, 0.0, half_x=200.0, half_y=2.0)],
                   veh_types={v: "car" for v, _, _ in rows})
    comp = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=500.0)
    assert comp["n_classified"] == 15
    assert int(comp["count"]["NLOSb"].sum()) == 9
    assert int(comp["count"]["NLOSv"].sum()) == 2
    assert int(comp["count"]["LOS"].sum()) == 4
    assert comp["overall"]["NLOSb"] == pytest.approx(9 / 15, abs=1e-9)


def test_canyon_fallback_uses_the_analytic_expectation_not_a_counter():
    """With no footprints the engine draws NLOSb from a Poisson process, which cannot be replayed
    pair-by-pair -- but its expectation 1 - exp(-lambda*d) is exact. The old aggregation summed the
    counters instead and reported NLOSb = 0 for every synthetic-map run."""
    sc = _scenario(_emit([("veh_000", 0.0, 0.0), ("veh_001", 225.0, 0.0)]),
                   veh_types={"veh_000": "car", "veh_001": "car"}, density=4.0)
    comp = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=500.0)
    assert comp["nlosb_method"] == "canyon_density_expectation"
    expected = 1.0 - math.exp(-0.004 * 225.0)         # band 200-250 m, centre 225 m
    assert comp["overall"]["NLOSb"] == pytest.approx(expected, abs=1e-9)
    assert comp["overall"]["LOS"] == pytest.approx(1.0 - expected, abs=1e-9)


def test_composition_fractions_sum_to_one_per_populated_band():
    rows = [(f"veh_{i:03d}", 17.0 * i, 11.0 * (i % 5)) for i in range(12)]
    sc = _scenario(_emit(rows), buildings=[_wall(80.0, 0.0, half_x=2.0, half_y=100.0)],
                   veh_types={v: ("truck" if i % 4 == 0 else "car")
                              for i, (v, _, _) in enumerate(rows)})
    comp = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=500.0)
    for b in range(comp["edges"].size - 1):
        if comp["n_pairs"][b] <= 0:
            continue
        tot = sum(comp["fraction"][s][b] for s in ("LOS", "NLOSv", "NLOSb"))
        assert tot == pytest.approx(1.0, abs=1e-9)


def test_pair_budget_truncates_deterministically():
    rows = [(f"veh_{i:03d}", 3.0 * i, 0.0) for i in range(60)]
    sc = _scenario(_emit(rows), veh_types={v: "car" for v, _, _ in rows})
    a = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=500.0, max_pairs=100)
    b = aw.link_state_composition(sc, bin_m=50.0, max_dist_m=500.0, max_pairs=100)
    assert a["truncated"] and a["n_classified"] == 100
    assert np.array_equal(a["count"]["LOS"], b["count"]["LOS"])


# ==================================================================================================
# the mixed curve and the crossing
# ==================================================================================================
def _uniform_comp(fraction_map, edges=None, n=1000):
    edges = np.arange(0.0, 1050.0, 50.0) if edges is None else edges
    nb = edges.size - 1
    return {"edges": edges, "bin_m": 50.0, "max_dist_m": float(edges[-1]),
            "n_pairs": np.full(nb, n, dtype=np.int64),
            "count": {s: np.full(nb, int(n * fraction_map[s]), dtype=np.int64) for s in aw._STATES},
            "fraction": {s: np.full(nb, fraction_map[s], dtype=float) for s in aw._STATES},
            "overall": dict(fraction_map), "n_classified": n * nb, "truncated": False,
            "snapshots": 1, "nlosb_method": "synthetic", "n_buildings": 0}


def test_all_LOS_curve_reaches_much_further_than_all_NLOSb():
    los = aw.mixed_pdr_curve(_uniform_comp({"LOS": 1.0, "NLOSv": 0.0, "NLOSb": 0.0}),
                             tx_power_dbm=23.0, rx_sensitivity_dbm=-81.0)
    nlosb = aw.mixed_pdr_curve(_uniform_comp({"LOS": 0.0, "NLOSv": 0.0, "NLOSb": 1.0}),
                               tx_power_dbm=23.0, rx_sensitivity_dbm=-81.0)
    d_los, d_nb = aw.crossing_m(los, 0.5), aw.crossing_m(nlosb, 0.5)
    assert d_los > 5.0 * d_nb
    # the pinned "~100 m urban NLOS effective range" anchor, from the physics alone
    assert 40.0 < d_nb < 120.0


def test_mixed_curve_is_the_weighted_sum_of_its_states():
    mix = {"LOS": 0.2, "NLOSv": 0.3, "NLOSb": 0.5}
    c = aw.mixed_pdr_curve(_uniform_comp(mix), tx_power_dbm=23.0, rx_sensitivity_dbm=-81.0)
    i = 3                                             # 150-200 m band, centre 175 m
    expect = sum(mix[s] * c["per_state_pdr"][s][i] for s in aw._STATES)
    assert c["pdr"][i] == pytest.approx(expect, abs=1e-12)


def test_thin_bands_are_left_empty_rather_than_guessed():
    comp = _uniform_comp({"LOS": 1.0, "NLOSv": 0.0, "NLOSb": 0.0}, n=5)
    c = aw.mixed_pdr_curve(comp, tx_power_dbm=23.0, rx_sensitivity_dbm=-81.0)
    assert np.all(np.isnan(c["pdr"]))
    assert aw.crossing_m(c, 0.5) is None


def test_crossing_matches_a_hand_built_curve():
    edges = np.arange(0.0, 250.0, 50.0)
    c = {"edges": edges, "centres": np.array([25.0, 75.0, 125.0, 175.0]),
         "pdr": np.array([1.0, 0.8, 0.4, 0.1]), "n_pairs": np.full(4, 100)}
    # 0.5 lies between 0.8 @75 and 0.4 @125 -> 75 + 50*(0.8-0.5)/0.4 = 112.5
    assert aw.crossing_m(c, 0.5) == pytest.approx(112.5)
    assert aw.crossing_m(c, 0.9) == pytest.approx(50.0)
    assert aw.crossing_m(c, 1.5) is None               # never above the level


def test_curve_value_at_averages_the_annulus_not_a_single_bin():
    edges = np.arange(0.0, 250.0, 50.0)
    c = {"edges": edges, "centres": np.array([25.0, 75.0, 125.0, 175.0]),
         "pdr": np.array([1.0, 0.8, 0.4, 0.1]), "n_pairs": np.full(4, 100)}
    # 100 m sits on a bin EDGE: the reference's 50 m annulus spans 75-125, i.e. both neighbours
    assert aw.curve_value_at(c, 100.0, 50.0) == pytest.approx(0.6)


# ==================================================================================================
# end to end on a synthetic dataset directory
# ==================================================================================================
def _write_dataset(root, *, buildings, n_veh=10, n_steps=12, tx=23.0, sens=-81.0):
    os.makedirs(os.path.join(root, "ground_truth"), exist_ok=True)
    os.makedirs(os.path.join(root, "ma"), exist_ok=True)
    emissions = []
    for step in range(n_steps):
        for j in range(n_veh):
            side = 40.0 if j % 2 == 0 else -40.0
            emissions.append({"_visibility": "ORACLE", "t": float(step),
                              "true_vehicle_id": f"veh_{j:03d}",
                              "true_x": 25.0 * (j // 2) + 2.0 * step, "true_y": side,
                              "claimed_x": 25.0 * (j // 2) + 2.0 * step, "claimed_y": side})
    with open(os.path.join(root, "ground_truth", "gt_emissions_sample.jsonl"), "w",
              encoding="utf-8", newline="\n") as fh:
        for e in emissions:
            fh.write(json.dumps(e, sort_keys=True) + "\n")
    with open(os.path.join(root, "ground_truth", "gt_vehicle.jsonl"), "w",
              encoding="utf-8", newline="\n") as fh:
        for j in range(n_veh):
            fh.write(json.dumps({"_visibility": "ORACLE", "true_vehicle_id": f"veh_{j:03d}",
                                 "veh_type": "car"}, sort_keys=True) + "\n")
    net = {"nodes": [[0.0, 40.0], [400.0, 40.0], [0.0, -40.0], [400.0, -40.0]],
           "edges": [[0, 1], [2, 3]], "buildings": [[list(p) for p in b] for b in buildings]}
    man = {"generator": "scms_sim_ref.mock_pipeline (pre-MOSAIC reference, realistic v2)",
           "dataset_version": "test", "seed": 1,
           "config": {"emit_sample_prob": 1.0, "road_network": "custom", "dt": 1.0,
                      "radio_model": "geometric", "radio_env": "urban",
                      "radio_tx_power_dbm": tx, "radio_rx_sensitivity_dbm": sens,
                      "radio_nlosb_density_per_km": 0.0, "radio_range_m": 500.0,
                      "art_max_m": 150.0, "n_lanes": 1,
                      "custom_network": json.dumps(net)}}
    with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(man, fh, indent=2)
    return root


def test_report_on_a_wall_separated_scene(tmp_path):
    """Two parallel streets with a continuous wall between them. Same-street pairs are LOS,
    cross-street pairs are NLOSb -- and the report has to say so, in that order of magnitude."""
    root = _write_dataset(str(tmp_path / "ds"),
                          buildings=[_wall(200.0, 0.0, half_x=400.0, half_y=3.0)])
    rep = aw.awareness_report(root)
    assert rep["geometry"]["n_buildings"] == 1
    assert rep["geometry"]["nlosb_method"] == "building_raster"
    mix = rep["link_state_mix_overall"]
    # 10 vehicles, 5 per street: 5*5 = 25 cross-street pairs (all NLOSb) out of C(10,2) = 45.
    assert mix["NLOSb"] == pytest.approx(25 / 45, abs=1e-3)
    assert mix["LOS"] + mix["NLOSv"] == pytest.approx(20 / 45, abs=1e-3)
    # the report rounds each share to 4 dp, so the sum is 1 to within that rounding, not to 1e-6
    assert mix["LOS"] + mix["NLOSv"] + mix["NLOSb"] == pytest.approx(1.0, abs=5e-4)
    assert rep["config"]["link_budget_db"] == 104.0
    assert rep["shot_multiplicity"]["engine_cams_per_1s_window"] == 1.0
    assert rep["shot_multiplicity"]["z_engine_effective"] == 1.0
    assert rep["reference"]["nar90_distance_m_at_our_budget"] == pytest.approx(87.1, abs=0.2)
    assert rep["reference"]["measured_arm_reproducible"] is False
    assert rep["pdr_model"]["applies_to_this_run"] is True


def test_report_labels_a_non_geometric_run_as_counterfactual(tmp_path):
    root = _write_dataset(str(tmp_path / "ds2"), buildings=[_wall(200.0, 0.0, half_x=400.0)])
    with open(os.path.join(root, "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    man["config"]["radio_model"] = "disc"
    with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(man, fh, indent=2)
    rep = aw.awareness_report(root)
    assert rep["pdr_model"]["applies_to_this_run"] is False
    assert "counterfactual" in rep["pdr_model"]["note"]
    # the SCENE composition is still valid for a disc run -- that is the point of measuring it
    assert rep["link_state_mix_overall"]["NLOSb"] > 0.0
    # ... but the modelled PDR curve is NOT this run's, so nothing derived from it may be graded.
    # Grading it would be the same category error this module exists to remove, one level down.
    rd = rb.load_refdata()
    rows = {r["id"]: r for r in aw.panel_rows(rep, rb._metric, lambda k: rb._ref(rd, k))}
    for mid in ("comm.nar90_equivalent_range_m", "comm.pdr_gray_zone_ratio"):
        assert rows[mid]["status"] == "na", mid
        assert "COUNTERFACTUAL" in rows[mid]["reason"], mid
    assert rows["comm.link_state_los_fraction"]["value"] is not None


def test_a_scene_without_building_polygons_says_its_los_share_is_an_upper_bound(tmp_path):
    """A MOSAIC-layer dataset carries no footprints, so NLOSb is structurally zero there. Publishing
    that LOS share without the caveat would invite exactly the cross-scene comparison this module
    exists to stop -- 0.76 LOS on a no-buildings import against 0.03 on real Ingolstadt geometry."""
    root = _write_dataset(str(tmp_path / "ds_nb"), buildings=[])
    rep = aw.awareness_report(root)
    assert rep["geometry"]["nlosb_method"] == "no_blockage_model"
    assert rep["link_state_mix_overall"]["NLOSb"] == 0.0
    rd = rb.load_refdata()
    rows = {r["id"]: r for r in aw.panel_rows(rep, rb._metric, lambda k: rb._ref(rd, k))}
    assert "UPPER BOUND" in rows["comm.link_state_los_fraction"]["reason"]


def test_a_10hz_engine_is_capped_by_the_sources_own_fitted_Z(tmp_path):
    """eq. (4) says Z <= N. Ten CAMs in the window are worth 2-8 independent chances, not ten,
    because CAM losses arrive in bursts -- so a faster engine must be capped at the fitted Z, never
    credited with N. Getting this backwards would let a 10 Hz run claim awareness the source's own
    measurements say is unreachable."""
    root = _write_dataset(str(tmp_path / "fast"), buildings=[_wall(200.0, 0.0, half_x=400.0)])
    with open(os.path.join(root, "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    man["config"]["dt"] = 0.1
    with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(man, fh, indent=2)
    s = aw.awareness_report(root)["shot_multiplicity"]
    assert s["engine_cams_per_1s_window"] == 10.0
    assert s["z_engine_effective"] == pytest.approx(5.4579)     # the urban fit, not 10
    assert s["z_engine_effective"] < s["engine_cams_per_1s_window"]


def test_report_is_deterministic(tmp_path):
    root = _write_dataset(str(tmp_path / "ds3"), buildings=[_wall(200.0, 0.0, half_x=400.0)])
    a = json.dumps(aw.awareness_report(root), sort_keys=True, default=str)
    b = json.dumps(aw.awareness_report(root), sort_keys=True, default=str)
    assert a == b


def test_higher_power_raises_awareness_at_a_fixed_distance(tmp_path):
    """The reference's central finding is 'power beats rate' -- awareness must respond to EIRP.
    Read at a fixed 100 m rather than at a crossing, because a crossing can leave the measured
    range entirely at one end of a 20 dB sweep and then compares None to a float."""
    lo = _write_dataset(str(tmp_path / "lo"), buildings=[_wall(200.0, 0.0, half_x=400.0)], tx=13.0)
    hi = _write_dataset(str(tmp_path / "hi"), buildings=[_wall(200.0, 0.0, half_x=400.0)], tx=33.0)
    a = aw.awareness_report(lo)["anchors"][100]["pdr_per_packet"]
    b = aw.awareness_report(hi)["anchors"][100]["pdr_per_packet"]
    assert b > a
    # ... and sensitivity must trade against power dB for dB, as the source states it does
    same = aw.propagation_pdr("NLOSb", 150.0, tx_power_dbm=13.0, decode_floor_dbm=-91.0)
    other = aw.propagation_pdr("NLOSb", 150.0, tx_power_dbm=23.0, decode_floor_dbm=-81.0)
    assert same == pytest.approx(other, abs=1e-9)


def test_panel_rows_plug_into_the_harness(tmp_path):
    root = _write_dataset(str(tmp_path / "ds4"), buildings=[_wall(200.0, 0.0, half_x=400.0)])
    rep = aw.awareness_report(root)
    rd = rb.load_refdata()
    rows = aw.panel_rows(rep, rb._metric, lambda k: rb._ref(rd, k))
    ids = [r["id"] for r in rows]
    assert "comm.link_state_los_fraction" in ids
    assert "comm.link_state_los_fraction_200m" in ids
    assert "comm.pdr_absolute_200m" in ids
    assert "comm.nar90_equivalent_range_m" in ids
    assert "comm.pdr_gray_zone_ratio" in ids
    for r in rows:
        assert r["panel"] == "comm"
        assert r["status"] in ("pass", "fail", "na")
    # the gray-zone RATIO must now carry a real threshold instead of degrading to "na"
    gz = next(r for r in rows if r["id"] == "comm.pdr_gray_zone_ratio")
    assert gz["reference"]["min"] == pytest.approx(1.9191, abs=1e-4)


def test_the_gate_band_is_derived_from_the_sources_own_Z_range_not_chosen():
    """The band on `comm.nar90_equivalent_range_m` must be exactly "the reference distance lies
    inside the model's Z-sensitivity bracket", rewritten as a band on the measured value. Any round
    +/- percentage would be a tolerance invented by us, and inventing the tolerance is how a gate
    stops being a measurement."""
    ref = aw._synth_ref(87.1, "cite", "note", 103.5, [61.1, 119.0], [2.1365, 8.2886])
    assert ref["min"] == pytest.approx(87.1 * 103.5 / 119.0, abs=0.05)
    assert ref["max"] == pytest.approx(87.1 * 103.5 / 61.1, abs=0.05)
    # the measured value passes iff the reference sits inside the bracket -- check both directions
    assert ref["min"] <= 103.5 <= ref["max"]
    far = aw._synth_ref(244.9, "cite", "note", 155.8, [106.3, 173.7], [2.1365, 8.2886])
    assert not (far["min"] <= 155.8 <= far["max"])
    assert aw._synth_ref(None, "c", "n", 100.0, [50.0, 150.0], [2.0, 8.0]) is None
    assert aw._synth_ref(87.1, "c", "n", None, [50.0, 150.0], [2.0, 8.0]) is None
    assert aw._synth_ref(87.1, "c", "n", 100.0, [None, None], [2.0, 8.0]) is None


def test_scorecard_emits_the_new_rows_and_no_longer_gates_the_old_one(tmp_path):
    root = _write_dataset(str(tmp_path / "ds5"), buildings=[_wall(200.0, 0.0, half_x=400.0)])
    card = rb.scorecard(root)
    rows = {m["id"]: m for m in card["panels"]["comm"]}
    assert "comm.link_state_los_fraction_200m" in rows
    assert "comm.nar90_equivalent_range_m" in rows
    old = rows["comm.awareness_ratio_200m"]
    assert old.get("reference") is None, "the 0.90 anchor must no longer gate the all-pairs ratio"
    assert old["status"] == "na"
    assert old["details"]["retired_reference"] == "v2x_awareness.awareness_ratio_200m_urban_min"
    assert "conditions mismatch" in old["details"]["retired_because"]
    # and no comm metric anywhere may still be graded against the retired anchor
    for m in card["panels"]["comm"]:
        assert (m.get("reference") or {}).get("ref_id") != \
            "v2x_awareness.awareness_ratio_200m_urban_min"


def test_scorecard_survives_a_dataset_with_no_geometry(tmp_path):
    """A MOSAIC-layer dataset carries no `custom_network`; the panel must degrade, not explode."""
    root = str(tmp_path / "ds6")
    os.makedirs(os.path.join(root, "ground_truth"), exist_ok=True)
    with open(os.path.join(root, "ground_truth", "gt_emissions_sample.jsonl"), "w",
              encoding="utf-8", newline="\n") as fh:
        for step in range(5):
            for j in range(3):
                fh.write(json.dumps({"t": float(step), "true_vehicle_id": f"veh_{j:03d}",
                                     "true_x": 10.0 * j, "true_y": 0.0}, sort_keys=True) + "\n")
    with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump({"generator": "MOSAIC", "config": {"dt": 1.0}}, fh)
    card = rb.scorecard(root)
    ids = {m["id"] for m in card["panels"]["comm"]}
    assert "comm.link_state_los_fraction" in ids


def test_cli_markdown_runs(tmp_path, capsys):
    root = _write_dataset(str(tmp_path / "ds7"), buildings=[_wall(200.0, 0.0, half_x=400.0)])
    out_json = str(tmp_path / "aw.json")
    assert aw.main([root, "--markdown", "--json", out_json]) == 0
    text = capsys.readouterr().out
    assert "LINK-STATE COMPOSITION" in text
    assert "SHOT MULTIPLICITY" in text
    assert "VERDICT" in text
    with open(out_json, encoding="utf-8") as fh:
        assert json.load(fh)["config"]["link_budget_db"] == 104.0
