"""Guard the newly-exposed engine knobs (feat/expose-knobs).

Fourteen previously-hardcoded engine constants are now PipelineConfig fields. This test file locks in
the two properties every such exposure must satisfy:

  1. DETERMINISM CONTRACT -- each field's DEFAULT equals the old hardcoded constant, so the DEFAULT
     config (and configs that only exercise the touched subsystems while leaving the new fields at
     default) produce BYTE-IDENTICAL output. The golden digests are hardcoded below.
  2. REAL WIRING -- each knob, set OFF its default, actually moves engine behaviour in the expected
     direction (counts move monotonically, fixed seeds), is rejected by validate_config when out of
     range, is self-describing in config_schema(), and is reachable through a CLI flag.

The knobs (name / default / group / CLI flag):
  detector_z_threshold        3.0   Detection/MA  --detector-z-threshold
  detector_min_consec         2     Detection/MA  --detector-min-consec
  sybil_min_certs             4     Detection/MA  --sybil-min-certs
  sybil_cell_m                3.0   Detection/MA  --sybil-cell-m
  denm_rate_window_s          100.0 Messages      --denm-rate-window-s
  denm_fake_fallback_rate     40.0  Messages      --denm-fake-fallback-rate
  denm_benign_max_speed_mps   4.0   Messages      --denm-benign-max-speed
  denm_decel_trig_mps2        2.5   Messages      --denm-decel-trig
  denm_implausible_speed_mps  6.0   Messages      --denm-implausible-speed
  vru_max_plausible_speed_mps 10.0  Mobility      --vru-max-plausible-speed
  gps_quality_floor           0.5   GNSS/sensor   --gps-quality-floor
  gps_quality_lambda          1.2   GNSS/sensor   --gps-quality-lambda
  radio_cap_sigma             4.0   Radio         --radio-cap-sigma
  radio_cap_max_mult          6.0   Radio         --radio-cap-max-mult
"""
import json
import math
from collections import Counter

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline, config_schema
from scms_sim_ref.mock_pipeline.run import validate_config, main

# --- golden digests (measured on main a3ab5d4, BEFORE the exposure) ------------------------------- #
GOLDEN_DEFAULT = "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"
GOLDEN_COLLUSION_RSU_LOGDIST = "53abb36711ae0e55ef10f754bb5086d87109af96237eda60916c1688091806f3"
GOLDEN_VRU_DENM = "4628e01edeb30212f9da85a1cd3a5742d60eac5be2e469b1023acea1973144d1"

# every field exposed by this task -> (default value, expected config_schema group)
NEW_FIELDS = {
    "detector_z_threshold": (3.0, "Detection/MA"),
    "detector_min_consec": (2, "Detection/MA"),
    "sybil_min_certs": (4, "Detection/MA"),
    "sybil_cell_m": (3.0, "Detection/MA"),
    "denm_rate_window_s": (100.0, "Messages"),
    "denm_fake_fallback_rate": (40.0, "Messages"),
    "denm_benign_max_speed_mps": (4.0, "Messages"),
    "denm_decel_trig_mps2": (2.5, "Messages"),
    "denm_implausible_speed_mps": (6.0, "Messages"),
    "vru_max_plausible_speed_mps": (10.0, "Mobility"),
    "gps_quality_floor": (0.5, "GNSS/sensor"),
    "gps_quality_lambda": (1.2, "GNSS/sensor"),
    "radio_cap_sigma": (4.0, "Radio"),
    "radio_cap_max_mult": (6.0, "Radio"),
}


# --------------------------------------------------------------------------- helpers ------------- #
def _run(tmp_path, name, **kw):
    """Run a pipeline into a unique subdir and return the RunResult."""
    out = tmp_path / name
    return run_pipeline(PipelineConfig(out_dir=str(out), **kw))

def _reason_counts(res) -> Counter:
    """Total per-reason firing counts across every MA report."""
    c: Counter = Counter()
    with open(f"{res.out_dir}/ma/ma_reports.jsonl", encoding="utf-8") as fh:
        for ln in fh:
            for rc in json.loads(ln)["reason_codes"]:
                c[rc] += 1
    return c

def _denm_rows(res) -> list:
    p = f"{res.out_dir}/ma/ma_denm_log.jsonl"
    try:
        with open(p, encoding="utf-8") as fh:
            return [json.loads(ln) for ln in fh]
    except FileNotFoundError:
        return []

def _mean_benign_gps_error(res) -> float:
    """Mean claimed-vs-true position error over benign, non-faulty sampled emissions."""
    errs = []
    with open(f"{res.out_dir}/ground_truth/gt_emissions_sample.jsonl", encoding="utf-8") as fh:
        for ln in fh:
            r = json.loads(ln)
            if not r["is_attacker"] and not r["is_faulty"]:
                errs.append(math.hypot(r["claimed_x"] - r["true_x"], r["claimed_y"] - r["true_y"]))
    assert errs, "expected benign emission samples"
    return sum(errs) / len(errs)


# --------------------------------------------------------------------------- determinism --------- #
def test_default_golden_unchanged(tmp_path):
    res = _run(tmp_path, "g", seed=7, traffic_flow=True, road_network="grid", duration_s=60,
               arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25)
    assert res.data_digest == GOLDEN_DEFAULT

def test_collusion_rsu_logdistance_golden_unchanged(tmp_path):
    """A run exercising collusion + RSUs + the log-distance radio, new fields left at default."""
    res = _run(tmp_path, "c", seed=11, traffic_flow=True, road_network="grid", duration_s=60,
               arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.3, collude_pct=0.5,
               victim_pct=0.2, n_rsus=3, radio_model="logdistance")
    assert res.data_digest == GOLDEN_COLLUSION_RSU_LOGDIST

def test_vru_denm_golden_unchanged(tmp_path):
    """A run exercising the VRU + DENM subsystems, new fields left at default."""
    res = _run(tmp_path, "v", seed=13, traffic_flow=True, road_network="grid", duration_s=60,
               arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25, vru_pct=0.2,
               vru_speed_mps=1.8, denm_rate=30.0)
    assert res.data_digest == GOLDEN_VRU_DENM

def test_all_knobs_set_is_deterministic(tmp_path):
    """A config with every new knob off-default runs byte-identical twice."""
    cfg = dict(seed=21, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.6,
               grid_w=5, grid_h=5, attacker_pct=0.3, attack_type="Sybil", radio_model="logdistance",
               vru_pct=0.15, vru_speed_mps=1.8, denm_rate=25.0,
               detector_z_threshold=2.5, detector_min_consec=1, sybil_min_certs=3, sybil_cell_m=4.0,
               denm_rate_window_s=80.0, denm_fake_fallback_rate=35.0, denm_benign_max_speed_mps=3.5,
               denm_decel_trig_mps2=2.0, denm_implausible_speed_mps=5.0,
               vru_max_plausible_speed_mps=8.0, gps_quality_floor=0.7, gps_quality_lambda=1.4,
               radio_cap_sigma=3.5, radio_cap_max_mult=5.0)
    d1 = _run(tmp_path, "a1", **cfg).data_digest
    d2 = _run(tmp_path, "a2", **cfg).data_digest
    assert d1 == d2


# --------------------------------------------------------------------------- behaviour ----------- #
# GROUP 1: detector operating point
def test_detector_min_consec_lower_yields_more_reports(tmp_path):
    base = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=50, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
    strict = _run(tmp_path, "mc4", detector_min_consec=4, **base)
    loose = _run(tmp_path, "mc1", detector_min_consec=1, **base)
    assert loose.n_reports > strict.n_reports

def test_detector_z_threshold_lower_yields_more_reports(tmp_path):
    base = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=50, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
    high = _run(tmp_path, "z6", detector_z_threshold=6.0, **base)
    low = _run(tmp_path, "z1", detector_z_threshold=1.0, **base)
    assert low.n_reports > high.n_reports

def test_sybil_min_certs_lower_fires_sybil_more_readily(tmp_path):
    base = dict(seed=5, traffic_flow=True, road_network="grid", duration_s=50, arrival_rate=1.6,
                grid_w=5, grid_h=5, attacker_pct=0.3, attack_type="Sybil")
    strict = _reason_counts(_run(tmp_path, "sm8", sybil_min_certs=8, **base))["sybilCoLocation"]
    loose = _reason_counts(_run(tmp_path, "sm2", sybil_min_certs=2, **base))["sybilCoLocation"]
    assert loose > strict

def test_sybil_cell_larger_fires_sybil_more_readily(tmp_path):
    """A coarser co-location grid bins more certs into one cell -> higher sybil scores."""
    base = dict(seed=5, traffic_flow=True, road_network="grid", duration_s=50, arrival_rate=1.6,
                grid_w=5, grid_h=5, attacker_pct=0.3, attack_type="Sybil")
    fine = _reason_counts(_run(tmp_path, "cs05", sybil_cell_m=0.5, **base))["sybilCoLocation"]
    coarse = _reason_counts(_run(tmp_path, "cs12", sybil_cell_m=12.0, **base))["sybilCoLocation"]
    assert coarse > fine

# GROUP 2: VRU / DENM thresholds
def test_denm_implausible_speed_lower_flags_more_denms(tmp_path):
    base = dict(seed=9, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.6,
                grid_w=5, grid_h=5, attacker_pct=0.3, attack_type="FakeHazard")
    high = _run(tmp_path, "di15", denm_implausible_speed_mps=15.0, **base)
    low = _run(tmp_path, "di2", denm_implausible_speed_mps=2.0, **base)
    assert high.data_digest != low.data_digest       # the stored plausibility score actually changes
    n_hi = sum(1 for r in _denm_rows(high) if r["denm_plausibility"] >= 1.0)
    n_lo = sum(1 for r in _denm_rows(low) if r["denm_plausibility"] >= 1.0)
    assert n_lo > n_hi

def test_denm_benign_max_speed_lower_fires_brake_plausibility_more(tmp_path):
    """The brake-implausible bound is DERIVED as denm_benign_max_speed_mps + 0.5; lowering it flags
    more phantom brake DENMs (FakeHazard emits emergencyElectronicBrakeLight)."""
    base = dict(seed=9, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.6,
                grid_w=5, grid_h=5, attacker_pct=0.3, attack_type="FakeHazard")
    high = _reason_counts(_run(tmp_path, "bm20", denm_benign_max_speed_mps=20.0, **base))["denmPlausibility"]
    low = _reason_counts(_run(tmp_path, "bm1", denm_benign_max_speed_mps=1.0, **base))["denmPlausibility"]
    assert low > high

def test_denm_rate_window_smaller_emits_more_denms(tmp_path):
    base = dict(seed=9, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.6,
                grid_w=5, grid_h=5, attacker_pct=0.2, denm_rate=30.0)
    wide = _run(tmp_path, "rw200", denm_rate_window_s=200.0, **base)
    narrow = _run(tmp_path, "rw50", denm_rate_window_s=50.0, **base)
    assert len(_denm_rows(narrow)) > len(_denm_rows(wide))

def test_denm_decel_trig_lower_triggers_more_brake_denms(tmp_path):
    """A permissive deceleration trigger arms far more benign emergency-brake DENMs."""
    base = dict(seed=9, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=2.5,
                grid_w=5, grid_h=5, attacker_pct=0.1, denm_rate=80.0, traffic_lights=True)
    def n_brake(res):
        return sum(1 for r in _denm_rows(res)
                   if r.get("event_type") == "emergencyElectronicBrakeLight")
    strict = n_brake(_run(tmp_path, "dt12", denm_decel_trig_mps2=12.0, **base))
    loose = n_brake(_run(tmp_path, "dt03", denm_decel_trig_mps2=0.3, **base))
    assert loose > strict

def test_denm_fake_fallback_rate_higher_emits_more_phantom_denms(tmp_path):
    """With denm_rate==0 a FakeHazard falls back to this rate; higher -> more phantom DENMs."""
    base = dict(seed=9, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.6,
                grid_w=5, grid_h=5, attacker_pct=0.3, attack_type="FakeHazard")
    low = _run(tmp_path, "ff5", denm_fake_fallback_rate=5.0, **base)
    high = _run(tmp_path, "ff80", denm_fake_fallback_rate=80.0, **base)
    assert len(_denm_rows(high)) > len(_denm_rows(low))

def test_vru_max_plausible_speed_lower_fires_impersonation_more(tmp_path):
    base = dict(seed=3, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.6,
                grid_w=5, grid_h=5, attacker_pct=0.3, attack_type="VruImpersonation",
                vru_pct=0.15, vru_speed_mps=1.8)
    high = _reason_counts(_run(tmp_path, "vm25", vru_max_plausible_speed_mps=25.0, **base))["vruImpersonation"]
    low = _reason_counts(_run(tmp_path, "vm3", vru_max_plausible_speed_mps=3.0, **base))["vruImpersonation"]
    assert low > high

# GROUP 3: per-vehicle GNSS quality spread
def test_gps_quality_floor_higher_raises_mean_gps_error(tmp_path):
    base = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.2, emit_sample_prob=1.0)
    lo = _mean_benign_gps_error(_run(tmp_path, "gf05", gps_quality_floor=0.5, **base))
    hi = _mean_benign_gps_error(_run(tmp_path, "gf6", gps_quality_floor=6.0, **base))
    assert hi > lo

def test_gps_quality_lambda_smaller_raises_mean_gps_error(tmp_path):
    """A smaller rate => heavier exponential tail => larger mean per-vehicle noise scale."""
    base = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.2, emit_sample_prob=1.0)
    heavy = _mean_benign_gps_error(_run(tmp_path, "gl04", gps_quality_lambda=0.4, **base))
    light = _mean_benign_gps_error(_run(tmp_path, "gl3", gps_quality_lambda=3.0, **base))
    assert heavy > light

# GROUP 4: log-distance radio candidate cap
def test_radio_cap_sigma_changes_logdistance_behaviour(tmp_path):
    """Narrowing the candidate cap headroom changes which distant links can close (logdistance only)."""
    base = dict(seed=11, traffic_flow=True, road_network="grid", duration_s=50, arrival_rate=1.6,
                grid_w=6, grid_h=6, attacker_pct=0.25, radio_model="logdistance", radio_range_m=120.0)
    wide = _run(tmp_path, "rcs_def", **base)
    narrow = _run(tmp_path, "rcs_low", radio_cap_sigma=0.5, **base)
    assert wide.data_digest != narrow.data_digest
    assert narrow.n_reports <= wide.n_reports        # a tighter window never adds distant receptions

def test_radio_cap_max_mult_changes_logdistance_behaviour(tmp_path):
    base = dict(seed=11, traffic_flow=True, road_network="grid", duration_s=50, arrival_rate=1.6,
                grid_w=6, grid_h=6, attacker_pct=0.25, radio_model="logdistance", radio_range_m=120.0)
    wide = _run(tmp_path, "rcm_def", **base)
    capped = _run(tmp_path, "rcm_1", radio_cap_max_mult=1.0, **base)
    assert wide.data_digest != capped.data_digest
    assert capped.n_reports < wide.n_reports         # clamping cap to 1x range drops distant links

def test_radio_cap_knobs_do_not_affect_disc_model(tmp_path):
    """disc (default) consults neither cap knob -> byte-identical regardless of their values."""
    base = dict(seed=11, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)               # radio_model defaults to "disc"
    ref = _run(tmp_path, "disc_ref", **base)
    off = _run(tmp_path, "disc_off", radio_cap_sigma=1.0, radio_cap_max_mult=2.0, **base)
    assert ref.data_digest == off.data_digest


# --------------------------------------------------------------------------- validation ---------- #
@pytest.mark.parametrize("field,bad", [
    ("detector_z_threshold", 0.0),
    ("detector_min_consec", 0),
    ("sybil_min_certs", 1),
    ("sybil_cell_m", 0.0),
    ("denm_rate_window_s", 0.0),
    ("denm_fake_fallback_rate", -1.0),
    ("denm_benign_max_speed_mps", 0.0),
    ("denm_decel_trig_mps2", 0.0),
    ("denm_implausible_speed_mps", 0.0),
    ("vru_max_plausible_speed_mps", 0.0),
    ("gps_quality_floor", -1.0),
    ("gps_quality_lambda", 0.0),
    ("radio_cap_sigma", 0.0),
    ("radio_cap_max_mult", 0.5),
])
def test_validate_config_rejects_out_of_range(field, bad):
    with pytest.raises(ValueError, match=field):
        validate_config(PipelineConfig(**{field: bad}))

def test_validate_config_accepts_defaults():
    validate_config(PipelineConfig())            # the all-default config must stay valid


# --------------------------------------------------------------------------- schema / CLI -------- #
def test_new_fields_are_self_describing_in_schema():
    sch = config_schema()
    for name, (default, group) in NEW_FIELDS.items():
        assert name in sch, f"{name} missing from config_schema()"
        meta = sch[name]
        assert meta["default"] == default, (name, meta["default"], default)
        assert meta["group"] == group, (name, meta["group"], group)
        assert meta["help"], f"{name} has no help string"

def test_every_new_cli_flag_parses_and_wires_through(tmp_path):
    """Set every new flag off-default, dump the effective config, and confirm each value landed."""
    cfg_path = tmp_path / "eff.json"
    out_dir = tmp_path / "cli"
    argv = [
        "--steps", "2", "--vehicles", "10", "--out", str(out_dir), "--dump-config", str(cfg_path),
        "--detector-z-threshold", "2.25",
        "--detector-min-consec", "3",
        "--sybil-min-certs", "5",
        "--sybil-cell-m", "2.5",
        "--denm-rate-window-s", "120.0",
        "--denm-fake-fallback-rate", "33.0",
        "--denm-benign-max-speed", "3.0",
        "--denm-decel-trig", "1.75",
        "--denm-implausible-speed", "7.0",
        "--vru-max-plausible-speed", "9.0",
        "--gps-quality-floor", "0.6",
        "--gps-quality-lambda", "1.4",
        "--radio-cap-sigma", "3.0",
        "--radio-cap-max-mult", "5.5",
    ]
    assert main(argv) == 0
    eff = json.loads(cfg_path.read_text(encoding="utf-8"))
    expected = {
        "detector_z_threshold": 2.25, "detector_min_consec": 3, "sybil_min_certs": 5,
        "sybil_cell_m": 2.5, "denm_rate_window_s": 120.0, "denm_fake_fallback_rate": 33.0,
        "denm_benign_max_speed_mps": 3.0, "denm_decel_trig_mps2": 1.75,
        "denm_implausible_speed_mps": 7.0, "vru_max_plausible_speed_mps": 9.0,
        "gps_quality_floor": 0.6, "gps_quality_lambda": 1.4, "radio_cap_sigma": 3.0,
        "radio_cap_max_mult": 5.5,
    }
    for k, v in expected.items():
        assert eff[k] == v, (k, eff.get(k), v)
