"""Opt-in geometric V2X channel model (radio_model="geometric") -- 3GPP TR 37.885.

The model replaces the hard `d <= radio_range_m` disc (and the single-slope logdistance soft range)
with the Phase-2 stack from docs/realism/PHASE2-DESIGN.md:

  1. per-(tx, rx) link classification LOS / NLOSv (a vehicle in the way) / NLOSb (a building),
  2. exact TR 37.885 path loss for that state, plus the standard's NLOSv blockage distribution,
  3. a Gudmundson AR(1) shadowing process carried PER LINK and keyed on the TRUE vehicle ids
     (the old draw was keyed on the cert digest and on `step`: a pseudonym rotation resampled the
     channel and there was no spatial correlation at all -- roadmap G5),
  4. per-packet Nakagami-m fading with distance-dependent m into the 802.11p decode test,
  5. an independent-survival loss product (the additive form it replaces can exceed 1.0),
  6. an MA-visible `rssi_dbm` evidence column computed from TRUE geometry.

These tests grade the implementation against the AUDITED refdata copies of the constants
(datagen/refdata/{pathloss_3gpp_tr37885,nakagami_fading,phy_80211p_profile}.json) rather than
against a second transcription of the same formula, which is what makes them non-tautological.
"""
import json
import math
import os
import statistics

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline, validate_config
from scms_sim_ref.mock_pipeline import run as RM
from scms_sim_ref.schemas.records import is_forbidden_feature_key

REFDIR = os.path.join(os.path.dirname(os.path.abspath(RM.__file__)),
                      "..", "datagen", "refdata")


def _ref(name):
    with open(os.path.normpath(os.path.join(REFDIR, name)), encoding="utf-8") as fh:
        return json.load(fh)["entries"]


# ================================================================================================
# 1. Path-loss conformance -- the ROADMAP Phase-2 gate: 0.01 dB at d = 100 / 500 m
# ================================================================================================
def test_pathloss_matches_published_constants_to_0p01_db():
    e = _ref("pathloss_3gpp_tr37885.json")
    tol = e["pathloss_conformance_tolerance_db"]["max"]
    fc = e["reference_frequency_ghz"]["value"]
    worst = 0.0
    for state, d, expected in e["pathloss_at_reference_distances_db"]["points"]:
        got = RM.tr37885_pathloss_db(state, d, fc)
        worst = max(worst, abs(got - expected))
        assert abs(got - expected) <= tol, f"{state} @ {d} m: {got} vs pinned {expected}"
    assert worst <= tol, worst


def test_pathloss_triples_are_the_refdata_triples():
    """No second copy of the constants: the module table must equal the audited refdata table."""
    e = _ref("pathloss_3gpp_tr37885.json")
    for key in ("urban_los", "urban_nlos", "highway_los"):
        v = e[key]["value"]
        assert RM.TR37885_PATHLOSS[key] == (v["a"], v["b"], v["c"]), key
    assert RM.TR37885_FC_GHZ == e["reference_frequency_ghz"]["value"]
    sig = e["shadowing_sigma_db"]["value"]
    assert RM.TR37885_SHADOW_SIGMA_DB == {"LOS": sig["los"], "NLOSv": sig["nlosv"],
                                          "NLOSb": sig["nlos"]}
    dec = e["shadowing_decorrelation_distance_m"]["value"]
    assert RM.TR37885_SHADOW_DECORR_M == {"LOS": dec["los"], "NLOSv": dec["nlosv"],
                                          "NLOSb": dec["nlos"]}


def test_highway_los_is_not_the_free_space_shortcut():
    """refdata's IMPLEMENTER TRAP: reusing a Friis/FSPL helper is wrong by 0.0478 dB everywhere,
    ~5x the conformance tolerance. The implementation must carry the literal 32.4 intercept."""
    e = _ref("pathloss_3gpp_tr37885.json")
    offset = e["highway_los_free_space_offset_db"]["value"]
    fspl_intercept = 20.0 * math.log10(4.0 * math.pi * 1e9 / 299792458.0)
    for d in (100.0, 500.0, 1000.0):
        fspl = fspl_intercept + 20.0 * math.log10(d) + 20.0 * math.log10(RM.TR37885_FC_GHZ)
        assert abs((fspl - RM.tr37885_pathloss_db("highway_los", d)) - offset) < 1e-4


def test_nlosv_distance_term_activates_only_beyond_541m():
    e = _ref("pathloss_3gpp_tr37885.json")
    act = e["nlosv_distance_term_activation_m"]["value"]
    both = e["nlosv_extra_loss_db"]["value"]["both_antennas_below_blocker"]
    one = e["nlosv_extra_loss_db"]["value"]["one_antenna_below_blocker"]
    assert RM.TR37885_NLOSV["both_below"] == (both["mu_base_db"], both["sigma_db"])
    assert RM.TR37885_NLOSV["one_below"] == (one["mu_base_db"], one["sigma_db"])
    for d in (1.0, 50.0, 200.0, act - 1.0):
        assert RM.tr37885_nlosv_mu_db(9.0, d) == pytest.approx(9.0)
        assert RM.tr37885_nlosv_mu_db(5.0, d) == pytest.approx(5.0)
    # a test that only probes d < 541 m cannot tell a correct distance term from a missing one
    assert RM.tr37885_nlosv_mu_db(9.0, 1000.0) == pytest.approx(9.0 + 15.0 * 3.0 - 41.0)   # 13.0
    assert RM.tr37885_nlosv_mu_db(9.0, 1000.0) == pytest.approx(13.0)


def test_nlosv_realised_losses_land_in_the_measured_band():
    """Sanity band from the measurement campaigns: a single blocking vehicle costs 5.5-17 dB."""
    e = _ref("pathloss_3gpp_tr37885.json")
    lo, hi = e["nlosv_measured_single_vehicle_loss_db"]["range"]
    import random as _r
    rng = _r.Random(1234)
    mu, sg = RM.TR37885_NLOSV["both_below"]
    draws = sorted(max(0.0, rng.gauss(RM.tr37885_nlosv_mu_db(mu, 150.0), sg)) for _ in range(20000))
    # the "bulk" of the censored draw (the interquartile band) must sit inside the measured range;
    # the tails legitimately spill past it -- N(9, 4.5) has a p10 of 3.2 dB by construction
    p25, p75 = draws[5000], draws[15000]
    assert lo <= p25 and p75 <= hi, (p25, p75)
    assert statistics.fmean(draws) > mu, "a censored Gaussian has a mean strictly above mu"


# ================================================================================================
# 2. Small-scale fading
# ================================================================================================
def test_nakagami_bands_match_the_adopted_refdata_parameterisation():
    pts = _ref("nakagami_fading.json")["m_by_distance_adopted"]["points"]
    assert RM.nakagami_m_for_distance(10.0) == pts[0][1] == 3.0
    assert RM.nakagami_m_for_distance(50.0) == 3.0
    assert RM.nakagami_m_for_distance(50.001) == pts[1][1] == 1.5
    assert RM.nakagami_m_for_distance(150.0) == 1.5
    assert RM.nakagami_m_for_distance(1000.0) == pts[2][1] == 1.0
    # NOT the ns-3 defaults (1.5 / 0.75 / 0.75 at 80 / 200 m) -- they differ by 2x at 100 m
    ns3 = _ref("nakagami_fading.json")["m_by_distance_ns3_default"]["points"]
    assert RM.nakagami_m_for_distance(100.0) != ns3[1][1]


def test_nakagami_gain_has_unit_mean():
    """refdata IMPLEMENTER TRAP: gamma(shape=m, scale=1) has mean m, so an unscaled draw inflates
    received power by +4.8 dB in the near band and 0 dB past 150 m -- a distance-dependent bias
    that looks exactly like a path-loss-exponent error. The channel uses scale = 1/m."""
    import random as _r
    for m in (3.0, 1.5, 1.0):
        rng = _r.Random(7)
        mean = statistics.fmean(rng.gammavariate(m, 1.0 / m) for _ in range(60000))
        assert abs(mean - 1.0) < 0.02, (m, mean)


# ================================================================================================
# 3. Shadowing: AR(1)/Gudmundson, keyed on the TRUE vehicle id, spatially correlated
# ================================================================================================
def _shadow_series(state="LOS", stepm=2.0, n=6000, seed=5):
    """Drive one link at a constant per-step displacement and read back its shadowing series."""
    cfg = PipelineConfig(seed=seed, radio_model="geometric", radio_nlosb_density_per_km=0.0)
    ch = RM.GeometricChannel(cfg, buildings=None, dt=1.0)
    out = []
    x = 0.0
    for k in range(n):
        ch.begin_step(k, [])                    # no vehicle blockers -> pure LOS
        # both endpoints advance stepm/2 so the SUM of endpoint displacements is stepm
        s, sh, _mu, _sg = ch._link_state(1, 2, x, 0.0, x + 300.0, 0.0, 300.0,
                                         RM.V2X_ANTENNA_HEIGHT_M, RM.V2X_ANTENNA_HEIGHT_M)
        assert s == state
        out.append(sh)
        x += stepm / 2.0
    return out


def test_shadowing_efolding_distance_is_10m_within_3m():
    """ROADMAP Phase-2 gate: measured shadowing autocorrelation e-folding distance 10 +/- 3 m.

    The Gudmundson process is driven by the SUM of the two endpoint displacements (the distance the
    link travels through the environment), so the autocorrelation is read off the same measure."""
    stepm = 2.0
    s = _shadow_series(stepm=stepm)
    mu = statistics.fmean(s)
    var = statistics.pvariance(s, mu)
    assert var > 1.0, var
    # first lag whose autocorrelation drops below 1/e, linearly interpolated -> e-folding distance
    prev_lag, prev_r = 0, 1.0
    efold = None
    for lag in range(1, 40):
        r = sum((s[i] - mu) * (s[i + lag] - mu) for i in range(len(s) - lag)) / ((len(s) - lag) * var)
        if r < 1.0 / math.e:
            efold = (prev_lag + (lag - prev_lag) * (prev_r - 1.0 / math.e) / (prev_r - r)) * stepm
            break
        prev_lag, prev_r = lag, r
    assert efold is not None, "autocorrelation never decayed below 1/e"
    assert abs(efold - RM.TR37885_SHADOW_DECORR_M["LOS"]) <= 3.0, efold


def test_shadowing_marginal_matches_the_los_sigma():
    s = _shadow_series(stepm=6.0, n=8000)
    assert abs(statistics.pstdev(s) - RM.TR37885_SHADOW_SIGMA_DB["LOS"]) < 0.35


def test_shadowing_is_reciprocal_and_keyed_on_true_vehicle_ids():
    """The AR(1) state is per UNORDERED pair of TRUE vids and is advanced exactly once per step, so
    A->B and B->A see the SAME shadow -- and no cert/pseudonym value is an input at all, which is
    what stops a pseudonym rotation from resampling the channel."""
    cfg = PipelineConfig(seed=3, radio_model="geometric", radio_nlosb_density_per_km=0.0)
    ch = RM.GeometricChannel(cfg, buildings=None, dt=1.0)
    ch.begin_step(0, [])
    ah = RM.V2X_ANTENNA_HEIGHT_M
    fwd = ch._link_state(4, 9, 0.0, 0.0, 200.0, 0.0, 200.0, ah, ah)
    rev = ch._link_state(9, 4, 200.0, 0.0, 0.0, 0.0, 200.0, ah, ah)
    assert fwd[1] == rev[1]
    other = RM.GeometricChannel(cfg, buildings=None, dt=1.0)
    other.begin_step(0, [])
    assert other._link_state(4, 9, 0.0, 0.0, 200.0, 0.0, 200.0, ah, ah)[1] == fwd[1]
    assert other._link_state(4, 8, 0.0, 0.0, 200.0, 0.0, 200.0, ah, ah)[1] != fwd[1]


# ================================================================================================
# 4. Loss composition + hidden-terminal congestion
# ================================================================================================
def test_hidden_terminal_fraction_is_bounded_and_monotone():
    R = 500.0
    assert RM.hidden_terminal_fraction(0.0, R) == pytest.approx(0.0)
    assert RM.hidden_terminal_fraction(2 * R, R) == pytest.approx(1.0)
    assert RM.hidden_terminal_fraction(5 * R, R) == pytest.approx(1.0)
    vals = [RM.hidden_terminal_fraction(d, R) for d in range(0, 1001, 50)]
    assert all(0.0 <= v <= 1.0 for v in vals)
    assert all(b >= a - 1e-12 for a, b in zip(vals, vals[1:]))


def test_survival_product_can_never_exceed_one():
    """The additive form it replaces (`packet_loss_base + nlos*(d/rr) + cong + wx`) reaches 1.55 on
    a perfectly ordinary heavy-weather / heavy-congestion configuration."""
    cfg = PipelineConfig(seed=1, radio_model="geometric", radio_range_m=500.0)
    ch = RM.GeometricChannel(cfg, buildings=None, dt=1.0)
    additive_max = 0.0
    for base in (0.0, 0.3, 0.9):
        for nlos in (0.0, 0.5):
            for wx in (0.0, 0.35):
                for load in (0, 40, 4000):
                    cbr = ch.cbr(load)
                    assert 0.0 <= cbr <= 1.0
                    p_col = ch.collision_loss(400.0, cbr)
                    assert 0.0 <= p_col <= 1.0
                    surv = (1 - base) * (1 - nlos) * (1 - p_col) * (1 - wx)
                    assert 0.0 <= surv <= 1.0
                    additive_max = max(additive_max, base + nlos + p_col + wx)
    assert additive_max > 1.0, "the additive form should demonstrably overflow"


def test_modelled_cbr_reproduces_the_refdata_airtime_arithmetic():
    e = _ref("phy_80211p_profile.json")
    ppdu = dict((p[0], p[3]) for p in e["frame_airtime_us"]["points"])[500.0]
    over = e["mac_overhead_us"]["value"]["total_added_us"]
    assert RM.PHY_FRAME_AIRTIME_S == pytest.approx((ppdu + over) * 1e-6)
    cfg = PipelineConfig(seed=1, radio_model="geometric")
    ch = RM.GeometricChannel(cfg, buildings=None, dt=1.0)
    # the refdata row "500 B, 10 Hz, 80 vehicles -> CBR 0.7356 counting PPDU + MAC overhead"
    assert ch.cbr(80 * 10) == pytest.approx(0.7356, abs=5e-4)


# ================================================================================================
# 5. Building geometry (uniform raster; no shapely)
# ================================================================================================
def _square(cx, cy, half):
    return [(cx - half, cy - half), (cx + half, cy - half), (cx + half, cy + half),
            (cx - half, cy + half)]


def test_building_raster_blocks_through_and_passes_around():
    r = RM._BuildingRaster([_square(0.0, 0.0, 30.0)], cell_m=2.0)
    assert r.blocked(-100.0, 0.0, 100.0, 0.0)          # straight through the middle
    assert r.blocked(0.0, -100.0, 0.0, 100.0)
    assert not r.blocked(-100.0, 80.0, 100.0, 80.0)    # clean pass well above it
    assert not r.blocked(-100.0, -80.0, -60.0, -80.0)


def test_building_raster_ignores_hits_next_to_an_antenna():
    """The road graph is RDP-simplified to 10 m and the raster has a ~1-cell halo, so an antenna can
    nominally sit on a building cell; a link must not be declared blocked by its own endpoint."""
    r = RM._BuildingRaster([_square(0.0, 0.0, 4.0)], cell_m=2.0)
    assert not r.blocked(0.0, 0.0, 0.0, 40.0)          # transmitter inside the footprint
    assert r.blocked(0.0, -60.0, 0.0, 60.0)            # crossing it end to end IS blocked


def test_vehicle_blocker_index_finds_only_vehicles_on_the_line_between():
    ix = RM._VehicleBlockerIndex(cell_m=25.0)
    ix.rebuild([(10, 50.0, 0.0, 1.6), (11, 50.0, 20.0, 3.0), (12, -50.0, 0.0, 3.0)])
    assert ix.tallest_blocker(0.0, 0.0, 100.0, 0.0, 1, 2) == 1.6       # on the line, between
    assert ix.tallest_blocker(0.0, 0.0, 100.0, 0.0, 1, 10) == 0.0      # excluded endpoint
    assert ix.tallest_blocker(0.0, 0.0, 40.0, 0.0, 1, 2) == 0.0        # beyond the far endpoint
    ix.rebuild([(13, 50.0, 0.0, 3.0)])
    assert ix.tallest_blocker(0.0, 0.0, 100.0, 0.0, 1, 2) == 3.0       # truck-height blocker


def test_both_antennas_above_the_blocker_costs_nothing():
    """TR 37.885: a blocker shorter than both antennas leaves the link in LOS with zero extra loss.
    An RSU antenna (5 m) clears a 3 m truck; two 1.5 m OBUs do not."""
    cfg = PipelineConfig(seed=2, radio_model="geometric", radio_nlosb_density_per_km=0.0)
    ch = RM.GeometricChannel(cfg, buildings=None, dt=1.0)
    ch.begin_step(0, [(99, 50.0, 0.0, 3.0)])
    veh = ch._link_state(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0,
                         RM.V2X_ANTENNA_HEIGHT_M, RM.V2X_ANTENNA_HEIGHT_M)
    assert veh[0] == "NLOSv" and veh[2] == 9.0
    ch2 = RM.GeometricChannel(cfg, buildings=None, dt=1.0)
    ch2.begin_step(0, [(99, 50.0, 0.0, 3.0)])
    rsu = ch2._link_state(1, 3, 0.0, 0.0, 100.0, 0.0, 100.0,
                          RM.RSU_ANTENNA_HEIGHT_M, RM.RSU_ANTENNA_HEIGHT_M)
    assert rsu[0] == "LOS"


# ================================================================================================
# 6. Config-surface plumbing + the byte-identical default path
# ================================================================================================
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"


def test_default_disc_golden_is_untouched_by_the_new_model(tmp_path):
    res = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp_path / "d")))
    assert res.data_digest == DEFAULT_GOLDEN


def test_geometric_knobs_flow_the_whole_config_pipeline():
    from scms_sim_ref.mock_pipeline.run import config_schema
    sch = config_schema()
    for f in ("radio_env", "radio_tx_power_dbm", "radio_rx_sensitivity_dbm",
              "radio_nlosb_density_per_km"):
        assert f in sch, f
        assert sch[f]["group"] == "Radio", (f, sch[f]["group"])
        assert sch[f]["help"], f
    assert sch["radio_model"]["options"] == ["disc", "logdistance", "geometric"]
    assert sch["radio_env"]["options"] == ["urban", "highway"]


def test_validate_config_rejects_bad_geometric_settings():
    for kw, msg in ((dict(radio_model="nope"), "radio_model"),
                    (dict(radio_env="mars"), "radio_env"),
                    (dict(radio_tx_power_dbm=99.0), "radio_tx_power_dbm"),
                    (dict(radio_rx_sensitivity_dbm=5.0), "radio_rx_sensitivity_dbm"),
                    (dict(radio_tx_power_dbm=-20.0, radio_rx_sensitivity_dbm=-10.0), "link budget"),
                    (dict(radio_nlosb_density_per_km=-1.0), "radio_nlosb_density_per_km")):
        with pytest.raises(ValueError) as ei:
            validate_config(PipelineConfig(**kw))
        assert msg in str(ei.value), (kw, str(ei.value))


def test_geometric_run_is_deterministic(tmp_path):
    base = dict(seed=21, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=2.0,
                grid_w=5, grid_h=5, attacker_pct=0.2, radio_model="geometric",
                radio_cap_max_mult=2.0)
    a = run_pipeline(PipelineConfig(**base, out_dir=str(tmp_path / "a")))
    b = run_pipeline(PipelineConfig(**base, out_dir=str(tmp_path / "b")))
    assert a.data_digest == b.data_digest


def test_geometric_produces_a_gray_zone_the_disc_cannot(tmp_path):
    """The point of the whole model: reception must degrade over a wide band of distances instead of
    stopping dead at radio_range_m. Measured on the raw honest-link distances."""
    base = dict(seed=31, traffic_flow=True, road_network="grid", duration_s=90, arrival_rate=2.5,
                grid_w=6, grid_h=6, grid_block_m=120.0, attacker_pct=0.15, radio_range_m=300.0,
                emit_sample_prob=1.0)
    disc = run_pipeline(PipelineConfig(**base, out_dir=str(tmp_path / "disc")))
    geo = run_pipeline(PipelineConfig(**base, radio_model="geometric", radio_cap_max_mult=3.0,
                                      radio_nlosb_density_per_km=2.0, out_dir=str(tmp_path / "geo")))

    def heard(out):
        labs = {}
        with open(f"{out}/ground_truth/gt_report_labels.jsonl", encoding="utf-8") as fh:
            for ln in fh:
                r = json.loads(ln)
                labs[r["report_id"]] = r
        emis = {}
        with open(f"{out}/ground_truth/gt_emissions_sample.jsonl", encoding="utf-8") as fh:
            for ln in fh:
                e = json.loads(ln)
                emis[(e["true_vehicle_id"], round(e["t"], 3))] = (e["true_x"], e["true_y"])
        ds = []
        with open(f"{out}/ma/ma_reports.jsonl", encoding="utf-8") as fh:
            for ln in fh:
                r = json.loads(ln)
                lab = labs.get(r["report_id"])
                if not lab or lab["report_correctness"] == "malicious_false_report":
                    continue
                t = round(r["detection_time"], 3)
                a = emis.get((lab["reporter_true_id"], t))
                b = emis.get((lab["subject_true_id"], t))
                if a and b:
                    ds.append(math.hypot(a[0] - b[0], a[1] - b[1]))
        return sorted(ds)

    dd, gd = heard(disc.out_dir), heard(geo.out_dir)
    assert len(dd) > 50 and len(gd) > 50, (len(dd), len(gd))
    # the disc cannot deliver past its range (only GNSS noise on the claim can push it over)
    assert dd[-1] <= 300.0 * 1.05
    # ... the geometric model routinely does, and its far tail is much longer
    assert gd[-1] > 400.0, gd[-1]
    assert sum(1 for d in gd if d > 300.0) > 5


# ================================================================================================
# 7. rssi_dbm -- MA-visible, but derived from TRUE geometry
# ================================================================================================
def _geo_dataset(tmp_path):
    return run_pipeline(PipelineConfig(
        seed=42, traffic_flow=True, road_network="grid", duration_s=90, arrival_rate=2.0,
        grid_w=6, grid_h=6, grid_block_m=120.0, traffic_lights=True, attacker_pct=0.2,
        attack_type="ConstPos", attack_intensity=1.0, collude_pct=0.5, victim_pct=0.15,
        radio_range_m=500.0, radio_model="geometric", radio_cap_max_mult=2.0,
        emit_sample_prob=1.0, out_dir=str(tmp_path / "rssi")))


def _rows(out, name):
    with open(f"{out}/{name}", encoding="utf-8") as fh:
        return [json.loads(ln) for ln in fh if ln.strip()]


def test_rssi_is_ma_visible_not_ground_truth():
    """rssi_dbm is measured by the receiver's own PHY, so it is legitimately MA-visible and must NOT
    be in the leakage registry -- while every true_* companion must be."""
    assert not is_forbidden_feature_key("rssi_dbm")
    assert is_forbidden_feature_key("true_x") and is_forbidden_feature_key("true_speed")


def test_rssi_absent_by_default_present_and_never_null_under_geometric(tmp_path):
    """A NULL (or missing) rssi on the colluder path would be a perfect oracle for a fabricated
    accusation -- the colluder never receives a frame. Every row must carry a plausible value."""
    disc = run_pipeline(PipelineConfig(
        seed=42, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=2.0,
        grid_w=5, grid_h=5, attacker_pct=0.2, collude_pct=0.5, out_dir=str(tmp_path / "disc")))
    assert all("rssi_dbm" not in r for r in _rows(disc.out_dir, "ma/ma_reports.jsonl"))

    geo = _geo_dataset(tmp_path)
    rows = _rows(geo.out_dir, "ma/ma_reports.jsonl")
    labs = {r["report_id"]: r for r in _rows(geo.out_dir, "ground_truth/gt_report_labels.jsonl")}
    assert rows and all("rssi_dbm" in r for r in rows)
    assert all(r["rssi_dbm"] is not None for r in rows), "a NULL rssi is a fabrication oracle"
    fab = [r["rssi_dbm"] for r in rows
           if labs[r["report_id"]]["report_correctness"] == "malicious_false_report"]
    hon = [r["rssi_dbm"] for r in rows
           if labs[r["report_id"]]["report_correctness"] != "malicious_false_report"]
    assert len(fab) > 20 and len(hon) > 20, (len(fab), len(hon))
    # decodable window: no row may sit below the decode floor, and none may exceed the strongest
    # link the model can produce (highway LOS at 1 m) plus generous shadow/fade headroom
    floor = -81.0
    ceil_ = 23.0 - RM.tr37885_pathloss_db("highway_los", 1.0) + 40.0
    assert all(floor - 1e-6 <= v <= ceil_ for v in fab + hon), (min(fab + hon), max(fab + hon))
    # ... and no point mass exactly at the floor on the fabricated side (the clamp fingerprint)
    at_floor = sum(1 for v in fab if abs(v - floor) < 0.005)
    assert at_floor <= 0.02 * len(fab), (at_floor, len(fab))


def test_rssi_tracks_true_geometry_not_the_claimed_position(tmp_path):
    """The load-bearing property: RSSI is computed on the channel side from the transmitter's TRUE
    position. That is exactly what makes an RSSI-vs-claimed-distance detector possible -- a Sybil
    ghost or a position-falsifying attacker carries the RSSI of where it REALLY is. An
    implementation that used the claimed position would leave the column looking perfectly
    plausible, so a range assertion cannot catch it; correlation against both distances can."""
    import numpy as np
    geo = _geo_dataset(tmp_path)
    rows = _rows(geo.out_dir, "ma/ma_reports.jsonl")
    labs = {r["report_id"]: r for r in _rows(geo.out_dir, "ground_truth/gt_report_labels.jsonl")}
    pos = {}
    for e in _rows(geo.out_dir, "ground_truth/gt_emissions_sample.jsonl"):
        pos[(e["true_vehicle_id"], round(e["t"], 3))] = e
    d_true, d_claim, rssi = [], [], []
    for r in rows:
        lab = labs.get(r["report_id"])
        if not lab or lab["report_correctness"] == "malicious_false_report":
            continue
        t = round(r["detection_time"], 3)
        a = pos.get((lab["reporter_true_id"], t))
        b = pos.get((lab["subject_true_id"], t))
        if not a or not b or not b.get("falsified"):
            continue                       # only falsified beacons separate true from claimed
        dt_ = math.hypot(a["true_x"] - b["true_x"], a["true_y"] - b["true_y"])
        dc_ = math.hypot(a["true_x"] - b["claimed_x"], a["true_y"] - b["claimed_y"])
        if dt_ < 1.0 or dc_ < 1.0 or abs(math.log10(dc_ / dt_)) < 0.15:
            continue                       # the two hypotheses must actually differ
        d_true.append(math.log10(dt_))
        d_claim.append(math.log10(dc_))
        rssi.append(r["rssi_dbm"])
    assert len(rssi) >= 50, f"need falsified-beacon report links to test on (got {len(rssi)})"
    ct = float(np.corrcoef(rssi, d_true)[0, 1])
    cc = float(np.corrcoef(rssi, d_claim)[0, 1])
    assert ct < -0.35, f"rssi must fall with TRUE log-distance (r={ct:.3f})"
    assert ct < cc - 0.2, f"rssi tracks the CLAIMED position (true r={ct:.3f}, claimed r={cc:.3f})"
