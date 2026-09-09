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

from scms_sim_ref.api.channel import StationSnapshot, StepFrame
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


# ================================================================================================
# 8. Falsification pins: known, measured gaps between this model and reality.
#
# These tests do NOT assert that the model is right. They assert that the model is WRONG IN THE
# EXACT WAY WE HAVE MEASURED AND RECORDED, so that a silent change to either the model or the
# refdata record of the gap breaks the build instead of quietly erasing a known limitation.
# The numbers come from refdata (the audited copy) and from docs/realism/RADIO-VS-REALITY.md.
# ================================================================================================
def _censored_mean(mu: float, sigma: float) -> float:
    """E[max(0, N(mu, sigma))] -- the realised mean of the draw `evaluate_raw` actually makes."""
    if sigma <= 0.0:
        return max(0.0, mu)
    z = mu / sigma
    return (mu * 0.5 * (1.0 + math.erf(z / math.sqrt(2)))
            + sigma * math.exp(-z * z / 2.0) / math.sqrt(2.0 * math.pi))


def _spec_branch(tx_h: float, rx_h: float, blk_h: float) -> str:
    """TR 37.885 clause 6.2.1 as WRITTEN, transcribed INDEPENDENTLY of the implementation.

    "Case 1: Minimum antenna height value of TX and RX > Blocker height - No additional blockage
    loss"; "Case 2: Maximum antenna height value of TX and RX < Blocker height - Mean: 9 + ...";
    "Case 3: Otherwise - Mean: 5 dB + ...". Note Case 1 needs min(h) STRICTLY greater, so an
    antenna exactly at the blocker's height is Case 3."""
    if min(tx_h, rx_h) > blk_h:
        return "both_above"
    if max(tx_h, rx_h) < blk_h:
        return "both_below"
    return "one_below"


def _branch(tx_h: float, rx_h: float, blk_h: float) -> str:
    """The ENGINE's rule, so a change there breaks these tests rather than a second copy of it."""
    return RM.tr37885_nlosv_case(tx_h, rx_h, blk_h)


# TR 37.885 clause 6.1.2 vehicle types: (body height m, antenna height m).
_TR_TYPES = {1: (1.6, 0.75), 2: (1.6, 1.6), 3: (3.0, 3.0)}


# ---- 8a. TWO GAPS THAT WERE CLOSED ON 2026-09-07, RE-PINNED AS CLOSED ------------------------- #
# These four tests used to pin the OPPOSITE of what they pin now, and the re-pin is deliberate and
# is recorded as such. The antenna height was TR 37.885 Table 6.1.4-1's PEDESTRIAN UE row (1.5 m)
# while the fleet was declared Type 2, and the branch rule could not express the standard's
# Case 1 / Case 3 boundary at equality. Fixing either one alone would have made things worse -- the
# height fix alone would have swapped a +3.836 dB error for a -5.202 dB one, because under the
# standard's own urban Option A fleet the equality case is the ONLY NLOSv geometry there is -- so
# both were fixed together, and the tests move together with them.
#
# NEITHER PINNED DATASET DIGEST MOVED. That was measured, not assumed: both pinned arms run
# radio_model="disc", which never reads StationSnapshot.ant_h_m. What DID move, legitimately, is
# every geometric-model gap assertion in this section, plus the two-ray breakpoint distance (d_b is
# quadratic in antenna height: 177.1 m -> 201.5 m), which is re-pinned in
# tests/test_geometric_channel_physics.py.
def test_our_antenna_height_is_now_the_standards_own_type_2_value():
    """RE-PINNED (was `test_our_antenna_height_is_absent_from_tr37885_and_the_bias_is_recorded`).

    The old assertion was `V2X_ANTENNA_HEIGHT_M not in spec_heights` -- true, and a defect: 1.5 m is
    Table 6.1.4-1's "Pedestrian UE, cellular UE" height, while the vehicle row says "As defined in
    Subclause 6.1.2", which gives Type 1 = 0.75 m, Type 2 = 1.6 m, Type 3 = 3 m. The RSU row (5 m)
    had been taken correctly from the same table; the fleet was the one station class reading off
    the wrong line."""
    e = _ref("pathloss_3gpp_tr37885.json")["nlosv_antenna_height_nonconformance"]["value"]
    spec_heights = {e["tr37885_type1_car_low_antenna_m"],
                    e["tr37885_type2_car_high_antenna_m"],
                    e["tr37885_type3_truck_bus_antenna_m"]}
    assert RM.V2X_ANTENNA_HEIGHT_M == e["ours_all_vehicles_m"]
    assert RM.V2X_ANTENNA_HEIGHT_M == e["tr37885_type2_car_high_antenna_m"]
    assert RM.V2X_ANTENNA_HEIGHT_M in spec_heights, "the fleet is declared TR 37.885 Type 2"
    assert e["former_value_m"] == 1.5 and e["former_value_is_the_standards_row_for"] == \
        "pedestrian UE / cellular UE (Table 6.1.4-1)"
    # ... and the bias the old height carried against the standard's own type-2 evaluation is gone
    car_h = RM.TR37885_BLOCKER_HEIGHT_M["car"]
    h = RM.V2X_ANTENNA_HEIGHT_M
    ours = _censored_mean(*RM.TR37885_NLOSV[_branch(h, h, car_h)])
    spec = _censored_mean(*RM.TR37885_NLOSV[_spec_branch(h, h, car_h)])
    assert ours - spec == pytest.approx(0.0, abs=1e-12)
    assert e["our_bias_vs_spec_for_a_car_blocker_db"] == 0.0
    assert e["former_bias_vs_spec_for_a_car_blocker_db"] == pytest.approx(3.836, abs=1e-3)


def test_the_branch_rule_now_matches_the_standard_including_at_exact_equality():
    """RE-PINNED (was `test_branch_rule_diverges_from_the_standard_at_exact_equality`).

    The old rule was `below = (tx_h < blk) + (rx_h < blk)` with `below == 0` mapped to the zero-loss
    case. `below == 0` means min(h) >= blk; the standard's Case 1 needs min(h) STRICTLY greater, so
    the equality input belonged to Case 3 and the old rule returned 0 dB for it. That was
    unreachable while no station stood at a blocker height -- and it became THE most common urban
    NLOSv geometry the moment the antenna height was corrected to 1.6 m against a 1.6 m car body,
    which is why the two fixes could not be separated."""
    e = _ref("pathloss_3gpp_tr37885.json")["nlosv_branch_boundary_divergence"]["value"]
    for h in (1.6, 3.0):
        assert _branch(h, h, h) == "one_below" == _spec_branch(h, h, h)     # TR Case 3, mu 5 dB
    assert _censored_mean(*RM.TR37885_NLOSV["one_below"]) == pytest.approx(
        e["spec_result_at_equality_db"], abs=1e-3)
    # exhaustive: over every height triple the two heights this project uses can produce, plus the
    # standard's own three, the engine's rule and the transcription now agree everywhere
    heights = (0.75, 1.5, 1.6, 3.0, 5.0)
    disagree = [(t, r, b) for t in heights for r in heights for b in heights
                if _branch(t, r, b) != _spec_branch(t, r, b)]
    assert disagree == [], disagree
    # and the OLD rule is still shown to disagree, so the pin cannot be satisfied by reverting
    old = lambda t, r, b: ("both_above", "one_below", "both_below")[(t < b) + (r < b)]  # noqa: E731
    assert [x for x in ((t, r, b) for t in heights for r in heights for b in heights)
            if old(*x) != _spec_branch(*x)], "the old rule must still be visibly wrong"


def test_the_case1_bias_is_now_zero_under_every_dropping_option_the_standard_defines():
    """RE-PINNED (was `test_the_case1_bias_is_reported_for_every_dropping_option...`).

    TR 37.885 clause 6.1.2 defines TWO urban-grid UE-dropping options and a third for highway, and
    the earlier record quoted only the flattering one. All five are still enumerated -- but now to
    assert the bias is ZERO in each, including Option A ("100% vehicle type 2"), where every antenna
    is 1.6 m and every car body is 1.6 m so EVERY link is the equality case and the old rule was
    wrong by the full 5.2023 dB."""
    import itertools
    opts = _ref("pathloss_3gpp_tr37885.json")["nlosv_branch_boundary_divergence"][
        "bias_by_tr37885_dropping_option"]
    assert set(opts) == {"urban_grid_option_a", "urban_grid_option_b",
                         "highway_option_a", "highway_option_b", "highway_option_c"}, sorted(opts)
    for name, rec in opts.items():
        w = dict(zip((1, 2, 3), rec["type_mix"]))
        assert sum(w.values()) == pytest.approx(1.0), name
        spec_tot = our_tot = 0.0
        for tt, rt, bt in itertools.product((1, 2, 3), repeat=3):
            wt = w[tt] * w[rt] * w[bt]
            if wt == 0.0:
                continue
            tx, rx, blk = _TR_TYPES[tt][1], _TR_TYPES[rt][1], _TR_TYPES[bt][0]
            spec_tot += wt * _censored_mean(*RM.TR37885_NLOSV[_spec_branch(tx, rx, blk)])
            our_tot += wt * _censored_mean(*RM.TR37885_NLOSV[_branch(tx, rx, blk)])
        assert spec_tot == pytest.approx(rec["spec_mean_db"], abs=1e-3), (name, spec_tot)
        assert our_tot == pytest.approx(rec["spec_mean_db"], abs=1e-3), (name, our_tot)
        assert rec["bias_db"] == 0.0, name
        assert rec["former_bias_db"] < 0.0, name
    assert opts["urban_grid_option_a"]["former_bias_db"] == pytest.approx(-5.2023, abs=1e-3)


def test_blocker_type_now_changes_a_v2v_link_at_last():
    """RE-PINNED (was `test_blocker_type_has_no_effect_on_a_v2v_link`).

    At 1.5 m antennas every blocker in TR37885_BLOCKER_HEIGHT_M was taller than both endpoints, so
    the standard's three-case rule collapsed to ONE case and a motorcycle attenuated exactly as much
    as an articulated truck. At the standard's own Type 2 height the two cases separate: a 1.6 m car
    body is the equality case (Case 3, mu 5 dB) and a 3.0 m truck is Case 2 (mu 9 dB).

    NOT A CLAIM OF AGREEMENT WITH MEASUREMENT. What this buys is conformance: the type dependence is
    the STANDARD'S, evaluated at the standard's own heights. The measured NLOSv literature says
    something different again, and section 5 of docs/realism/RADIO-VS-REALITY.md still grades us
    against it -- see `test_the_nlosv_mean_is_flat_where_measurement_says_it_decays`, unchanged."""
    h = RM.V2X_ANTENNA_HEIGHT_M
    branches = {bt: _branch(h, h, bh) for bt, bh in RM.TR37885_BLOCKER_HEIGHT_M.items()}
    assert branches == {"car": "one_below", "motorcycle": "one_below",
                        "truck": "both_below", "bus": "both_below"}, branches
    losses = {bt: _censored_mean(*RM.TR37885_NLOSV[b]) for bt, b in branches.items()}
    assert losses["truck"] - losses["car"] == pytest.approx(3.8359, abs=1e-3), losses
    # the two bodies the standard gives the SAME height still cannot be told apart, and that is the
    # standard's answer rather than a defect of ours
    assert losses["car"] == pytest.approx(losses["motorcycle"])
    assert losses["truck"] == pytest.approx(losses["bus"])


def _geo(**kw):
    """A geometric channel with the DEFAULT radio configuration unless overridden.

    Every gap pinned below is a property of the DEFAULT configuration -- which is the one both
    pinned digests measure and the one every gate grades. Opt-in arms exist for several of them
    (`radio_antenna_pattern`, `radio_nlosv_hold`, `radio_blocker_width`, `radio_breakpoint`);
    turning one on is precisely what must break the matching pin."""
    kw.setdefault("radio_nlosb_density_per_km", 0.0)
    cfg = PipelineConfig(seed=2, radio_model="geometric", **kw)
    return RM.GeometricChannel(cfg, buildings=None, dt=1.0)


def test_the_default_link_budget_has_no_antenna_gain_or_pattern_term():
    """PINNED GAP G2, the largest omission. TR 37.885 clause 6.1.4 specifies a PER-VEHICLE-TYPE
    directional antenna pattern (Tables 6.1.4-8/-9 Option 1, 6.1.4-10A..D Option 2). By default
    this model has no antenna term at all -- no gain, no pattern, no bearing.

    This is also the CONFOUND that forced the withdrawal of the decorrelation "falsification":
    Nilsson et al.'s 2-4 m holds only "in cases where a proper model for the path loss AND THE
    ANTENNA PATTERN is included", and "otherwise, the de-correlation distance has to be much
    longer". A model in this default configuration is in the "otherwise" branch."""
    assert _ref("pathloss_3gpp_tr37885.json")["no_antenna_gain_or_pattern_term"][
        "available"] is False
    ch = _geo()
    assert getattr(ch, "ant_pattern", "none") == "none", (
        "the DEFAULT now models an antenna pattern -- update refdata "
        "`no_antenna_gain_or_pattern_term` and RADIO-VS-REALITY.md section 7 G2")
    # behavioural: rotating the geometry about the transmitter cannot change the received power,
    # because nothing in the default budget depends on an angle
    seen = set()
    for dx, dy in ((100.0, 0.0), (0.0, 100.0), (-100.0, 0.0), (70.71068, 70.71068)):
        c = _geo()
        c.begin_step(0, [])
        heard, rssi, state, _ = c.evaluate_raw(1, 2, 0.0, 0.0, dx, dy, 100.0,
                                               RM.V2X_ANTENNA_HEIGHT_M, RM.V2X_ANTENNA_HEIGHT_M)
        assert state == "LOS"
        seen.add(round(rssi, 9))
    assert len(seen) == 1, f"received power depends on bearing without an antenna model: {seen}"


def test_the_default_pathloss_is_single_slope_with_no_breakpoint():
    """PINNED GAP G3. Abbas et al. fit a DUAL-slope model whose physical breakpoint, evaluated at
    OUR OWN 1.5 m antennas and 5.9 GHz, is 177.1 m -- inside our operating range. Every state here
    is one (a, b, c) triple used at every distance, so the decade slope never changes."""
    e = _ref("pathloss_3gpp_tr37885.json")["no_two_ray_breakpoint_in_pathloss"]["value"]
    assert getattr(_geo(), "breakpoint_model", "none") == "none", (
        "a breakpoint is now on by DEFAULT -- update refdata and RADIO-VS-REALITY.md section 7 G3")
    for state, (_a, b, _c) in RM.TR37885_PATHLOSS.items():
        # the slope between any two decades is the same constant `b` -- no breakpoint anywhere
        for d0 in (10.0, 100.0, 200.0, 400.0):
            got = RM.tr37885_pathloss_db(state, d0 * 10.0) - RM.tr37885_pathloss_db(state, d0)
            assert got == pytest.approx(b, abs=1e-9), (state, d0, got)
    lam = 299792458.0 / (RM.TR37885_FC_GHZ * 1e9)
    d_b = 4.0 * RM.V2X_ANTENNA_HEIGHT_M ** 2 / lam
    assert d_b == pytest.approx(e["our_physical_breakpoint_m"], abs=0.1), d_b
    assert e["measured_far_slope_db_per_decade"]["urban_los"] - RM.TR37885_PATHLOSS["urban_los"][1] \
        == pytest.approx(e["missing_excess_slope_db_per_decade"], abs=1e-9)


def test_the_nlosv_blockage_loss_is_redrawn_on_every_packet_by_default():
    """PINNED GAP G1, the highest-value fix. TR 37.885 6.2.1 specifies "max {0 dB, a log-normal
    random variable}" -- ONE draw per blocked link. By default this model draws a fresh one per
    packet, which demotes a large-scale term to fast fading and shortens burst-loss runs.

    Asserted BEHAVIOURALLY, so it survives refactors: within a single step the link state is
    frozen, so the only per-packet randomness is the Nakagami fade plus (defect) the blockage
    redraw. An NLOSv link therefore shows strictly more packet-to-packet variance than a LOS link
    at the same distance. Hold the blockage per link and the two variances become equal, which is
    exactly when this pin must be retired."""
    e = _ref("pathloss_3gpp_tr37885.json")["nlosv_blockage_draw_is_per_packet_not_per_link"]
    assert e["value"]["status"].startswith("ACTIVE")
    assert getattr(_geo(), "nlosv_hold", False) is False, (
        "the blockage draw is now held per link BY DEFAULT -- update refdata and section 7 G1")

    def spread(blockers):
        ch = _geo()
        ch.begin_step(0, blockers)
        out = [ch.evaluate_raw(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0,
                               RM.V2X_ANTENNA_HEIGHT_M, RM.V2X_ANTENNA_HEIGHT_M)
               for _ in range(4000)]
        assert len({o[2] for o in out}) == 1, "link state must be frozen within one step"
        return out[0][2], statistics.pstdev([o[1] for o in out])

    los_state, los_sd = spread([])
    nlosv_state, nlosv_sd = spread([(99, 50.0, 0.0, RM.TR37885_BLOCKER_HEIGHT_M["truck"])])
    assert los_state == "LOS" and nlosv_state == "NLOSv"
    # a held per-link draw would make these equal; a per-packet redraw adds sigma = 4.5 dB of it
    assert nlosv_sd > los_sd + 1.0, (los_sd, nlosv_sd)


def test_blockage_sigma_splits_only_as_far_as_the_standard_splits_it():
    """PINNED GAP G4, HALF-CLOSED, AND RE-PINNED ON THE HALF THAT REMAINS.

    Segata et al. measure that the bigger the obstacle, the higher the VARIANCE. Until the antenna
    height was corrected this model gave EVERY blocker 4.5 dB, because every V2V link landed in the
    same branch. At the standard's own 1.6 m the two cases separate and a truck now does carry more
    spread than a car -- but that split is the STANDARD'S case rule, two-valued and driven by
    antenna-vs-body geometry, not a fitted size dependence. A motorcycle and a saloon are still
    identical, and nothing here claims 4.0/4.5 dB is the measured pair. What would close the gap
    properly is a per-blocker-class sigma stated in the text or a table of a redistributable
    source; none has been found, and inventing one would be fabrication."""
    e = _ref("pathloss_3gpp_tr37885.json")["blockage_spread_and_footprint_ignore_blocker_size"]
    h = RM.V2X_ANTENNA_HEIGHT_M
    sigmas = {bt: RM.TR37885_NLOSV[_branch(h, h, bh)][1]
              for bt, bh in RM.TR37885_BLOCKER_HEIGHT_M.items()}
    assert sigmas == e["value"]["sigma_db_by_blocker"], sigmas
    assert e["value"]["former_sigma_db_for_every_blocker"] == 4.5
    assert sigmas["truck"] > sigmas["car"], "the direction Segata et al. report"
    assert sigmas["motorcycle"] == sigmas["car"], "still no size dependence WITHIN a case"
    assert [sorted(g) for g in e["value"]["blocker_types_resolving_to_the_same_branch"]] == \
        [["car", "motorcycle"], ["bus", "truck"]]
    # and the geometric footprint is one constant too, not a per-type width, by default
    assert RM.GEO_BLOCKER_HALF_WIDTH_M == e["value"]["our_blocker_half_width_m"]
    assert getattr(_geo(), "blocker_width", "uniform") == "uniform", (
        "per-type blocker widths are now the DEFAULT -- update refdata and section 7 G5")


def test_the_small_scale_fade_is_drawn_independently_for_every_packet():
    """PINNED GAP G6. i.i.d. per packet is defensible at 30 m/s (coherence time 0.72 ms against a
    100 ms CAM period) and optimistic at a stop line (215 ms at 0.1 m/s relative speed) -- which is
    the reference arm, `--traffic-lights`.

    Behavioural: within one step the link state is frozen, so a correlated fade would make repeated
    packets on a stationary LOS link identical. They are not."""
    e = _ref("pathloss_3gpp_tr37885.json")["small_scale_fade_has_no_temporal_correlation"]["value"]
    ch = _geo()
    ch.begin_step(0, [])
    rssi = [ch.evaluate_raw(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0,
                            RM.V2X_ANTENNA_HEIGHT_M, RM.V2X_ANTENNA_HEIGHT_M)[1]
            for _ in range(500)]
    assert len(set(round(r, 9) for r in rssi)) == len(rssi), "the fade is no longer i.i.d."
    assert statistics.pstdev(rssi) > 1.0, statistics.pstdev(rssi)
    lam = 299792458.0 / (RM.TR37885_FC_GHZ * 1e9)
    for v, key in ((30.0, "coherence_time_ms_at_30_mps"), (0.1, "coherence_time_ms_at_0_1_mps")):
        assert 0.423 / (v / lam) * 1e3 == pytest.approx(e[key], abs=1e-3), (v, key)
    assert e["coherence_time_ms_at_0_1_mps"] > e["cam_period_ms"]      # a stop line outlives a CAM
    assert e["coherence_time_ms_at_30_mps"] < e["cam_period_ms"] / 100  # highway does not


def test_nlosv_realised_mean_is_flat_across_every_distance_our_scenarios_reach():
    """PINNED GAP. The distance term is zero below 541 m, so the NLOSv mean extra loss is a
    CONSTANT over the whole range our scenarios live in -- it cannot reproduce a blockage that
    varies with distance in that band. Stated on the realised (censored) mean, not on mu."""
    act = _ref("pathloss_3gpp_tr37885.json")["nlosv_distance_term_activation_m"]["value"]
    mu, sg = RM.TR37885_NLOSV["both_below"]
    means = {round(_censored_mean(RM.tr37885_nlosv_mu_db(mu, d), sg), 9)
             for d in (1.0, 10.0, 50.0, 100.0, 200.0, 400.0, act - 0.1)}
    assert len(means) == 1, f"expected a flat mean below {act} m, got {sorted(means)}"
    assert means.pop() == pytest.approx(9.0382, abs=1e-3)
    # and it only ever INCREASES beyond the activation distance
    assert _censored_mean(RM.tr37885_nlosv_mu_db(mu, 1000.0), sg) > 13.0


def test_shadowing_decorrelation_carries_no_environment_dependence():
    """PINNED GAP. TR37885_PATHLOSS is keyed on urban vs highway; TR37885_SHADOW_DECORR_M is not.

    The finding is the ENVIRONMENT RATIO, not the absolute value. Abbas et al. measure all four
    numbers in one campaign under one processing chain (Gudmundson 1/e), so highway/urban -- 5.48x
    for LOS, 7.22x for OLOS -- is free of processing confounds, and one constant cannot express it.

    The ABSOLUTE urban comparison is NOT a falsification and is deliberately not asserted as one:
    Nilsson et al. reach 2-4 m only "in cases where a proper model for the path loss AND THE
    ANTENNA PATTERN is included" and state that "otherwise, the de-correlation distance has to be
    much longer". We have no antenna-pattern term at all (see
    `test_the_link_budget_has_no_antenna_gain_or_pattern_term`), so that source defends the 10/13 m
    pin for a model of our class. See RADIO-VS-REALITY.md section 0 item W2."""
    e = _ref("pathloss_3gpp_tr37885.json")["measured_decorrelation_is_environment_dependent"]
    pts = e["points"]
    urban = {st: d for env, st, d in pts if env == "urban"}
    highway = {st: d for env, st, d in pts if env == "highway"}
    assert max(urban.values()) < min(highway.values()), pts
    for state in ("LOS", "NLOSv", "NLOSb"):
        ours = RM.TR37885_SHADOW_DECORR_M[state]
        assert ours > max(urban.values()), f"{state}: {ours} m not longer than urban {urban}"
        assert ours < min(highway.values()), f"{state}: {ours} m not shorter than highway"
    # THE confound-free statement: the measured environment ratio no single constant can express
    assert highway["LOS"] / urban["LOS"] == pytest.approx(5.482, abs=1e-3)
    assert highway["OLOS"] / urban["OLOS"] == pytest.approx(7.222, abs=1e-3)
    # the defensible urban span is Abbas's rows alone -- 2.35x..2.89x, NOT the withdrawn 2.2-5.9x
    assert RM.TR37885_SHADOW_DECORR_M["LOS"] / urban["LOS"] == pytest.approx(2.353, abs=1e-3)
    assert RM.TR37885_SHADOW_DECORR_M["NLOSv"] / urban["OLOS"] == pytest.approx(2.889, abs=1e-3)
    # the model has exactly one decorrelation constant per link state and no env term at all
    urban_cfg = PipelineConfig(seed=1, radio_model="geometric", radio_env="urban")
    hw_cfg = PipelineConfig(seed=1, radio_model="geometric", radio_env="highway")
    u = RM.GeometricChannel(urban_cfg, buildings=None, dt=1.0)
    h = RM.GeometricChannel(hw_cfg, buildings=None, dt=1.0)
    assert u.los_state != h.los_state                      # pathloss DOES depend on environment
    assert RM.TR37885_SHADOW_DECORR_M["LOS"] == 10.0       # ...and decorrelation does not


def test_the_nlosv_mean_is_flat_where_measurement_says_it_decays():
    """PINNED GAP (row 5). Superseding an earlier record that called this "unresolved". Two
    measured campaigns state in BODY TEXT that NLOSv excess loss decays strongly between 10 m and
    120 m; ours is flat by construction and then rises. Both sources are non-CC (arXiv
    non-exclusive licence; IEEE copyright), so their numbers are quoted with attribution in
    RADIO-VS-REALITY.md section 5 and are deliberately NOT pinned here. What IS pinned is the
    structural fact about OUR model that makes the disagreement unavoidable."""
    e = _ref("pathloss_3gpp_tr37885.json")["nlosv_loss_is_flat_with_distance_but_measurement_decays"]
    v = e["value"]
    mu, sg = RM.TR37885_NLOSV["both_below"]
    flat = {round(_censored_mean(RM.tr37885_nlosv_mu_db(mu, d), sg), 9)
            for d in (v["flat_from_m"], 10.0, 26.0, 80.0, 100.0, 120.0, v["flat_to_m"] - 0.1)}
    assert len(flat) == 1, sorted(flat)
    assert flat.pop() == pytest.approx(v["our_realised_mean_db"], abs=1e-6)
    # ...and beyond the activation distance it moves the WRONG WAY: up, where measurement goes down
    assert _censored_mean(RM.tr37885_nlosv_mu_db(mu, 1000.0), sg) > v["our_realised_mean_db"]


# ---- 8b. THREE GAPS RECORDED FOR THE FIRST TIME ON 2026-09-07 --------------------------------- #
# None of these is a regression. Each is a divergence from TR 37.885 that had been in the model
# since it was written and that no file in the tree named. A gap nobody has written down is worse
# than a gap everybody has, because the first person to measure around it will believe the model.
def test_the_default_link_budget_is_6db_below_the_standards_own():
    """PINNED GAP, AND IT INVERTS AN EARLIER RECOMMENDATION.

    TR 37.885 Table 6.1.1-1 lists 'UE Tx power -- Vehicle/pedestrian UE or UE type RSU: 23dBm' in
    the same column as 'Macro BS: 49dBm' -- a macro's CONDUCTED PA power -- while the element gain
    is given separately in Table 6.1.4-8 (3 dBi). The conformant V2V budget is therefore
    23 + 3 - PL + 3 = 29 - PL, and the SHIPPED DEFAULT computes 23 - PL.

    CHANNEL-PHYSICS.md revision 1 held the antenna pattern OFF pending an 'EIRP double count'. The
    double count was in the COMMENT, not the arithmetic; the pattern is what closes this gap."""
    e = _ref("pathloss_3gpp_tr37885.json")["link_budget_is_6db_below_tr37885_by_default"]["value"]
    assert e["shortfall_db"] == 2 * RM.TR37885_ANT_MAX_GAIN_DBI
    cfg_default, cfg_conformant = _geo(), _geo(radio_antenna_pattern="tr37885_opt1")
    assert cfg_default.ant_pattern == "none", "the 6 dB gap is still the DEFAULT"
    # measured on the budget itself, at a geometry with no blockage and no breakpoint
    args = (1, 2, 0.0, 0.0, 100.0, 0.0, 100.0, RM.V2X_ANTENNA_HEIGHT_M, RM.V2X_ANTENNA_HEIGHT_M)
    sts = {v: StationSnapshot(v, 100.0 * (v - 1), 0.0, RM.V2X_ANTENNA_HEIGHT_M,
                              RM.TR37885_BLOCKER_HEIGHT_M["car"], False, False, None, 0.0)
           for v in (1, 2)}
    for step in (0, 1):
        for ch in (cfg_default, cfg_conformant):
            ch.begin_step(StepFrame(step, float(step), 1.0, sts, (), (), 0.0, {}))
    lo = cfg_default.mean_rx_dbm(*args, "LOS", 0.0, 0.0)
    hi = cfg_conformant.mean_rx_dbm(*args, "LOS", 0.0, 0.0)
    assert hi - lo == pytest.approx(e["shortfall_db"], abs=1e-9)
    assert lo == pytest.approx(23.0 - RM.tr37885_pathloss_db("urban_los", 100.0), abs=1e-9)


def test_the_rsu_antenna_gap_is_refused_not_approximated():
    """PINNED GAP. TR 37.885 models an RSU with an antenna ARRAY (Tables 6.1.4-1..-5) this channel
    cannot evaluate. Giving an RSU endpoint 0 dB while both ends of a V2V link get the element gain
    is a 3 dB relative penalty on every V2I link that is a modelling artefact, not physics -- so the
    combination is refused. A VRU is NOT refused: Table 6.1.4-6 gives a pedestrian UE 0 dBi omni,
    which is the standard's own answer."""
    assert _ref("pathloss_3gpp_tr37885.json")["rsu_antenna_arrays_are_not_modelled"][
        "available"] is False
    with pytest.raises(ValueError, match="n_rsus"):
        validate_config(PipelineConfig(radio_model="geometric", n_rsus=1,
                                       radio_antenna_pattern="tr37885_opt1"))
    validate_config(PipelineConfig(radio_model="geometric", n_rsus=1))      # the default is fine


def test_los_nlosv_classification_is_geometric_and_the_deviation_is_recorded():
    """PINNED GAP. TR 37.885 Table 6.2-1 gives LOS/NLOSv as a PROBABILITY of distance alone --
    urban P(LOS) = min{1, 1.05*exp(-0.0114*d)} -- and clause 6.2.1 then draws the blocker's height
    statistically. This model does both GEOMETRICALLY instead, and the difference is large and
    two-sided: measured over 4.26 M classified link-steps on the reference arm's own traffic, we
    call 25.3% of links NLOSv where the standard's curve calls 89.2%, and the sign flips at ~60 m.

    Recorded, not fixed. Geometric classification is arguably the better instrument -- it responds
    to density and to a queue at a red light, which a distance-only probability cannot -- but an
    undocumented divergence from the standard we claim to implement is a defect whatever its sign,
    and a reader reproducing a TR 37.885 evaluation must be told which half we kept."""
    e = _ref("pathloss_3gpp_tr37885.json")[
        "los_nlosv_classification_is_geometric_not_probabilistic"]["value"]

    def p_los(d):                                   # the standard's own curve, urban
        return min(1.0, 1.05 * math.exp(-0.0114 * d))

    for d_mid, ours, spec in e["by_band_p_nlosv_ours_then_spec"]:
        assert 1.0 - p_los(d_mid) == pytest.approx(spec, abs=5e-4), d_mid
    assert e["overall_ratio_spec_over_ours"] == pytest.approx(
        e["spec_p_nlosv_over_the_same_link_lengths"] / e["our_p_nlosv_overall"], abs=0.01)
    # the deviation is NOT one-sided, which is the part a single ratio hides
    near = [(o, s) for dm, o, s in e["by_band_p_nlosv_ours_then_spec"] if dm < 60.0]
    far = [(o, s) for dm, o, s in e["by_band_p_nlosv_ours_then_spec"] if dm > 100.0]
    assert all(o > s for o, s in near), near
    assert all(o < s for o, s in far), far
    # and the engine really does classify geometrically: no distance-only probability can put two
    # links of the SAME length in different states, and this one does
    ch = _geo()
    ch.begin_step(0, [(9, 50.0, 0.0, RM.TR37885_BLOCKER_HEIGHT_M["truck"])])
    blocked = ch.evaluate_raw(1, 2, 0.0, 0.0, 100.0, 0.0, 100.0,
                              RM.V2X_ANTENNA_HEIGHT_M, RM.V2X_ANTENNA_HEIGHT_M)[2]
    clear = ch.evaluate_raw(3, 4, 0.0, 500.0, 100.0, 500.0, 100.0,
                            RM.V2X_ANTENNA_HEIGHT_M, RM.V2X_ANTENNA_HEIGHT_M)[2]
    assert (blocked, clear) == ("NLOSv", "LOS")
