"""GUI curated detector-strictness / attack-magnitude / GNSS knob contract + detector presets.

A recent engine release added several high-value control knobs -- the detector operating point
(the headline "control detection strictness" controls), per-attack-type falsification-magnitude
scaling, GNSS-quality spread, and the DENM/VRU plausibility bounds. They were already reachable via
the raw "all fields" advanced panel and the copilot, but NOT from the curated python-flow form where
most users look. This suite guards that the GUI now surfaces them as curated python-flow CONFIG_SPEC
entries and that:

  * each entry's `arg` is a REAL run.py CLI flag (mirrors test_gui_cli_contract),
  * each entry's default EQUALS the PipelineConfig default (so a default run stays byte-identical),
  * the launcher actually emits the flags when they are set (mirrors test_gui_parity/realism),
  * the new "Strict detector" / "Lenient detector" presets are exposed and validate cleanly.

NB: the DENM/VRU CLI flags DROP the `_mps` suffix that their PipelineConfig fields keep
(--denm-implausible-speed <- denm_implausible_speed_mps), which these tests pin down explicitly.
"""
import os
import subprocess
import sys
import types
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "gui"))
sys.path.insert(0, str(REPO / "scms-sim" / "scenarios"))
import server  # noqa: E402
from scms_sim_ref.mock_pipeline import run as runmod  # noqa: E402
from scms_sim_ref.mock_pipeline.run import PipelineConfig  # noqa: E402


# CLI flag -> (pf_ CONFIG_SPEC name, control type, PipelineConfig field supplying its default).
_CURATED = {
    "--detector-z-threshold":   ("pf_detector_z",         "float", "detector_z_threshold"),
    "--detector-min-consec":    ("pf_detector_consec",    "int",   "detector_min_consec"),
    "--sybil-min-certs":        ("pf_sybil_min_certs",    "int",   "sybil_min_certs"),
    "--sybil-cell-m":           ("pf_sybil_cell_m",       "float", "sybil_cell_m"),
    "--attack-magnitude-scale": ("pf_attack_magnitude",   "text",  "attack_magnitude_scale"),
    "--gps-quality-floor":      ("pf_gps_quality_floor",  "float", "gps_quality_floor"),
    "--gps-quality-lambda":     ("pf_gps_quality_lambda", "float", "gps_quality_lambda"),
    "--denm-implausible-speed": ("pf_denm_implausible",   "float", "denm_implausible_speed_mps"),
    "--vru-max-plausible-speed": ("pf_vru_max_plausible", "float", "vru_max_plausible_speed_mps"),
}


def _pf_by_arg():
    """python-flow CONFIG_SPEC entries keyed by their CLI arg."""
    return {c["arg"]: c for c in server.CONFIG_SPEC
            if c.get("gen") == "python-flow" and c.get("arg")}


def _run_help():
    env = {**os.environ, "PYTHONPATH": str(REPO / "src")}
    r = subprocess.run([sys.executable, "-m", "scms_sim_ref.mock_pipeline.run", "--help"],
                       capture_output=True, text=True, env=env, cwd=str(REPO))
    assert r.returncode == 0, r.stderr
    return r.stdout


# ---------- CONFIG_SPEC contract ----------

def test_curated_entries_present_and_typed():
    pf = _pf_by_arg()
    for arg, (name, ctype, _field) in _CURATED.items():
        assert arg in pf, f"python-flow CONFIG_SPEC is missing a curated entry for {arg}"
        c = pf[arg]
        assert c["name"] == name, f"{arg} should be control {name!r} (got {c['name']!r})"
        assert c["type"] == ctype, f"{arg} should be a {ctype} control (got {c['type']!r})"
        assert c.get("help"), f"{arg} should carry help text for discoverability"


def test_curated_defaults_match_pipelineconfig():
    """Curated defaults MUST equal PipelineConfig defaults so existing runs stay byte-identical."""
    pf, pc = _pf_by_arg(), PipelineConfig()
    for arg, (_name, _ctype, field) in _CURATED.items():
        want = getattr(pc, field)
        got = pf[arg]["default"]
        assert got == want, f"{arg} default {got!r} != PipelineConfig.{field} {want!r}"


def test_every_curated_flag_is_a_real_cli_flag():
    help_text = _run_help()
    missing = [a for a in _CURATED if a not in help_text]
    assert not missing, f"curated controls reference non-existent run.py flags: {missing}"


def test_curated_bounds_are_sane():
    """Declared min/max must contain the default (a slider/spinner can't start out of range)."""
    pf = _pf_by_arg()
    for arg in _CURATED:
        c = pf[arg]
        if c["type"] in ("bool", "choice", "text"):
            continue
        d = c["default"]
        if "min" in c:
            assert d >= c["min"], f"{arg} default {d} below declared min {c['min']}"
        if "max" in c:
            assert d <= c["max"], f"{arg} default {d} above declared max {c['max']}"


def test_denm_vru_cli_flags_drop_the_mps_suffix():
    """Regression anchor: the DENM/VRU controls must use the SUFFIX-LESS CLI flags even though the
    PipelineConfig fields keep `_mps`. Wiring the field name as the flag would silently break."""
    pf = _pf_by_arg()
    assert "--denm-implausible-speed" in pf and "--denm-implausible-speed-mps" not in pf
    assert "--vru-max-plausible-speed" in pf and "--vru-max-plausible-speed-mps" not in pf


# ---------- launcher emits the flags ----------

class _FakeProc:
    returncode = 0

    def poll(self):
        return 0  # looks already-finished, so it never blocks other code


def _capture_argv(monkeypatch, tmp_path, config):
    """Drive the REAL python-flow launcher (stubbing Popen) and return the CLI argv it built,
    minus the [python, -m, module] prefix -- i.e. exactly what run.main() would parse."""
    captured = {}

    def _fake_popen(cmd, *a, **k):
        captured["cmd"] = list(cmd)
        return _FakeProc()

    monkeypatch.setattr(server.subprocess, "Popen", _fake_popen)
    monkeypatch.setattr(server, "LOG_PATH", tmp_path / "last_run.log")
    try:
        res = server._start_python_flow(config)
        assert res["ok"] is True
    finally:
        server.RUN["proc"] = None  # don't leave a fake 'running' process for later tests
    return captured["cmd"][3:]


def test_launcher_emits_curated_flags_when_set(monkeypatch, tmp_path):
    config = {
        "generator": "python-flow",
        "pf_detector_z": 2.0, "pf_detector_consec": 1,
        "pf_sybil_min_certs": 2, "pf_sybil_cell_m": 5.0,
        "pf_attack_magnitude": "RandomPos:2.0,SlowDrift:0.5",
        "pf_gps_quality_floor": 0.8, "pf_gps_quality_lambda": 0.9,
        "pf_denm_implausible": 4.0, "pf_vru_max_plausible": 8.0,
    }
    cmd = _capture_argv(monkeypatch, tmp_path, config)
    expected = {
        "--detector-z-threshold": "2.0", "--detector-min-consec": "1",
        "--sybil-min-certs": "2", "--sybil-cell-m": "5.0",
        "--attack-magnitude-scale": "RandomPos:2.0,SlowDrift:0.5",
        "--gps-quality-floor": "0.8", "--gps-quality-lambda": "0.9",
        "--denm-implausible-speed": "4.0", "--vru-max-plausible-speed": "8.0",
    }
    for flag, val in expected.items():
        assert flag in cmd, f"launcher did not emit {flag}: {cmd}"
        assert cmd[cmd.index(flag) + 1] == val, (
            f"{flag} carried the wrong value: {cmd[cmd.index(flag) + 1]!r} != {val!r}")


def test_empty_attack_magnitude_is_not_emitted(monkeypatch, tmp_path):
    """The text control defaults to '' (uniform 1.0); an empty value must be OMITTED, so a default
    run stays byte-identical (passing an empty --attack-magnitude-scale would change the argv)."""
    cmd = _capture_argv(monkeypatch, tmp_path, {"generator": "python-flow", "pf_attack_magnitude": ""})
    assert "--attack-magnitude-scale" not in cmd


# ---------- byte-identical default run ----------

def _config_from_argv(monkeypatch, argv):
    """Build the REAL PipelineConfig run.main() would build from argv, validate it, and return it,
    without running the (heavy) pipeline or writing any files."""
    got = {}

    def _fake_pipeline(cfg):
        got["cfg"] = runmod.validate_config(cfg)  # raises ValueError on an invalid config
        return types.SimpleNamespace(out_dir=cfg.out_dir)

    monkeypatch.setattr(runmod, "run_pipeline", _fake_pipeline)
    monkeypatch.setattr(runmod, "_emit_result", lambda *a, **k: None)
    rc = runmod.main(argv)
    assert rc == 0
    return got["cfg"]


def test_default_run_config_is_byte_identical(monkeypatch, tmp_path):
    """A default python-flow run must build an identical PipelineConfig whether or not the newly
    surfaced curated flags are present (each emitted default == the CLI/PipelineConfig default)."""
    argv = _capture_argv(monkeypatch, tmp_path, server._defaults())
    # value flags: everything except the empty-default text control (which is never emitted).
    value_flags = {a for a, (_n, t, _f) in _CURATED.items() if t != "text"}

    def strip(a):
        out, i = [], 0
        while i < len(a):
            tok = a[i]
            if tok in value_flags:
                i += 2
                continue
            out.append(tok)
            i += 1
        return out

    argv_without_new = strip(argv)
    assert any(f in argv for f in value_flags)   # sanity: the defaults really do emit these
    cfg_new = _config_from_argv(monkeypatch, argv)
    cfg_old = _config_from_argv(monkeypatch, argv_without_new)
    assert cfg_new.__dict__ == cfg_old.__dict__, (
        "default run config changed after surfacing curated flags: "
        f"{ {k: (cfg_new.__dict__[k], cfg_old.__dict__.get(k)) for k in cfg_new.__dict__ if cfg_new.__dict__[k] != cfg_old.__dict__.get(k)} }")


# ---------- detector presets ----------

def test_detector_presets_exposed():
    """Both presets are served (via /api/defaults -> PRESETS) and are python-flow, grid-pinned so
    they never hit the arterial/topology validation trap."""
    for name in ("Strict detector", "Lenient detector"):
        assert name in server.PRESETS, f"missing one-click preset {name!r}"
        p = server.PRESETS[name]
        assert p.get("generator") == "python-flow"
        assert p.get("pf_road") == "grid"          # pinned so validate_config never trips
        assert not p.get("pf_arterial_every")      # arterial caps left at 0
        assert not p.get("pf_arterial_speed")
        assert not p.get("pf_local_speed")
    # The two presets sit on opposite sides of the detector operating point (the showcase):
    strict, lenient = server.PRESETS["Strict detector"], server.PRESETS["Lenient detector"]
    assert strict["pf_detector_z"] < lenient["pf_detector_z"]
    assert strict["pf_detector_consec"] <= lenient["pf_detector_consec"]
    assert strict["pf_sybil_min_certs"] == 2       # high-sensitivity Sybil point


def test_strict_detector_preset_validates_without_error(monkeypatch, tmp_path):
    """Applying the preset over the form defaults and running validate_config must NOT raise. Drives
    the real launcher + config builder so it exercises the exact production path a GUI click takes."""
    merged = server._defaults()
    merged.update(server.PRESETS["Strict detector"])
    argv = _capture_argv(monkeypatch, tmp_path, merged)
    cfg = _config_from_argv(monkeypatch, argv)     # calls validate_config; raises on any error
    assert cfg.road_network == "grid" and cfg.traffic_flow is True
    assert cfg.detector_z_threshold == 2.0
    assert cfg.detector_min_consec == 1
    assert cfg.sybil_min_certs == 2


def test_lenient_detector_preset_validates_without_error(monkeypatch, tmp_path):
    merged = server._defaults()
    merged.update(server.PRESETS["Lenient detector"])
    argv = _capture_argv(monkeypatch, tmp_path, merged)
    cfg = _config_from_argv(monkeypatch, argv)     # calls validate_config; raises on any error
    assert cfg.road_network == "grid" and cfg.traffic_flow is True
    assert cfg.detector_z_threshold == 5.0
    assert cfg.detector_min_consec == 3
