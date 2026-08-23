"""GUI realism-knob contract + the one-click "Max realism" preset.

Recent releases shipped several opt-in REALISM knobs on the python-flow engine (MOBIL lane changes,
gap acceptance, log-distance radio propagation, grid arterial/local speed tiers, VRUs). These were
reachable only via the raw "all fields" advanced panel. This suite guards that the GUI now surfaces
them as curated python-flow CONFIG_SPEC entries and that:

  * each entry's `arg` is a REAL run.py CLI flag (mirrors test_gui_cli_contract),
  * each entry's default EQUALS the PipelineConfig default (so a default run stays byte-identical),
  * the launcher actually emits the flags when they are set (mirrors test_gui_parity),
  * a default python-flow run builds a byte-identical PipelineConfig with the new flags present, and
  * the one-click "Max realism" preset validates cleanly -- in particular it does NOT trip the
    arterial/topology guard in validate_config (which rejects nonzero arterial/local caps off a
    grid/ring road): the preset pins a grid road and leaves the arterial caps at 0.
"""
import os
import subprocess
import sys
import types
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "gui"))
sys.path.insert(0, str(REPO / "scms-sim" / "scenarios"))
import server  # noqa: E402
from scms_sim_ref.mock_pipeline import run as runmod  # noqa: E402
from scms_sim_ref.mock_pipeline.run import PipelineConfig  # noqa: E402


# The realism knobs surfaced into the curated python-flow form:
#   CLI flag            -> (pf_ CONFIG_SPEC name, control type, PipelineConfig field for its default)
_REALISM = {
    "--lane-changes":       ("pf_lane_changes",     "bool",   "lane_changes"),
    "--lane-change-time":   ("pf_lane_change_time", "float",  "lane_change_time_s"),
    "--gap-acceptance":     ("pf_gap",              "bool",   "gap_acceptance"),
    "--radio-model":        ("pf_radio_model",      "choice", "radio_model"),
    "--pathloss-exponent":  ("pf_pathloss",         "float",  "pathloss_exponent"),
    "--shadowing-sigma-db": ("pf_shadowing",        "float",  "shadowing_sigma_db"),
    "--arterial-every":     ("pf_arterial_every",   "int",    "arterial_every"),
    "--arterial-speed":     ("pf_arterial_speed",   "float",  "arterial_speed_mps"),
    "--local-speed":        ("pf_local_speed",      "float",  "local_speed_mps"),
    "--vru-pct":            ("pf_vru_pct",          "float",  "vru_pct"),
    "--vru-speed":          ("pf_vru_speed",        "float",  "vru_speed_mps"),
    # turn_slowdown was already surfaced (pf_turn); assert it stays wired.
    "--turn-slowdown":      ("pf_turn",             "bool",   "turn_slowdown"),
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


def _preset_name():
    """The one-click 'Max realism' preset key (case-insensitive match)."""
    names = [k for k in server.PRESETS if "max realism" in k.lower()]
    assert len(names) == 1, f"expected exactly one 'Max realism' preset, found {names}"
    return names[0]


# ---------- CONFIG_SPEC contract ----------

def test_realism_entries_present_and_typed():
    pf = _pf_by_arg()
    for arg, (name, ctype, _field) in _REALISM.items():
        assert arg in pf, f"python-flow CONFIG_SPEC is missing a curated entry for {arg}"
        c = pf[arg]
        assert c["name"] == name, f"{arg} should be control {name!r} (got {c['name']!r})"
        assert c["type"] == ctype, f"{arg} should be a {ctype} control (got {c['type']!r})"
    # radio_model must offer both propagation models.
    assert set(pf["--radio-model"]["options"]) == {"disc", "logdistance"}


def test_realism_defaults_match_pipelineconfig():
    """Curated defaults MUST equal PipelineConfig defaults so existing runs stay byte-identical."""
    pf, pc = _pf_by_arg(), PipelineConfig()
    for arg, (_name, _ctype, field) in _REALISM.items():
        want = getattr(pc, field)
        got = pf[arg]["default"]
        assert got == want, f"{arg} default {got!r} != PipelineConfig.{field} {want!r}"


def test_every_realism_flag_is_a_real_cli_flag():
    help_text = _run_help()
    missing = [a for a in _REALISM if a not in help_text]
    assert not missing, f"realism controls reference non-existent run.py flags: {missing}"


def test_realism_bounds_are_sane():
    """Declared min/max must contain the default (a slider/spinner can't start out of range)."""
    pf = _pf_by_arg()
    for arg in _REALISM:
        c = pf[arg]
        if c["type"] in ("bool", "choice"):
            continue
        d = c["default"]
        if "min" in c:
            assert d >= c["min"], f"{arg} default {d} below declared min {c['min']}"
        if "max" in c:
            assert d <= c["max"], f"{arg} default {d} above declared max {c['max']}"


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


def test_launcher_emits_realism_flags_when_set(monkeypatch, tmp_path):
    config = {
        "generator": "python-flow",
        "pf_lane_changes": True, "pf_lane_change_time": 3.5,
        "pf_gap": True,
        "pf_radio_model": "logdistance", "pf_pathloss": 3.1, "pf_shadowing": 6.0,
        "pf_arterial_every": 3, "pf_arterial_speed": 30.0, "pf_local_speed": 12.0,
        "pf_vru_pct": 0.15, "pf_vru_speed": 4.0,
    }
    cmd = _capture_argv(monkeypatch, tmp_path, config)
    # store_true bool flags appear bare when on:
    for flag in ("--lane-changes", "--gap-acceptance"):
        assert flag in cmd, f"launcher did not emit {flag}: {cmd}"
    # value/choice flags carry their value:
    expected = {
        "--lane-change-time": "3.5", "--radio-model": "logdistance",
        "--pathloss-exponent": "3.1", "--shadowing-sigma-db": "6.0",
        "--arterial-every": "3", "--arterial-speed": "30.0", "--local-speed": "12.0",
        "--vru-pct": "0.15", "--vru-speed": "4.0",
    }
    for flag, val in expected.items():
        assert flag in cmd, f"launcher did not emit {flag}: {cmd}"
        assert cmd[cmd.index(flag) + 1] == val, (
            f"{flag} carried the wrong value: {cmd[cmd.index(flag) + 1]!r} != {val!r}")


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
    surfaced realism flags are present (each emitted default == the CLI/PipelineConfig default)."""
    argv = _capture_argv(monkeypatch, tmp_path, server._defaults())
    value_flags = {a for a, (_n, t, _f) in _REALISM.items() if t not in ("bool",)}
    bool_flags = {a for a, (_n, t, _f) in _REALISM.items() if t == "bool"}

    def strip(a):
        out, i = [], 0
        while i < len(a):
            tok = a[i]
            if tok in bool_flags:
                i += 1
                continue
            if tok in value_flags:
                i += 2
                continue
            out.append(tok)
            i += 1
        return out

    argv_without_new = strip(argv)
    # sanity: the default form really does emit the surfaced value flags
    assert any(f in argv for f in value_flags)
    cfg_new = _config_from_argv(monkeypatch, argv)
    cfg_old = _config_from_argv(monkeypatch, argv_without_new)
    assert cfg_new.__dict__ == cfg_old.__dict__, (
        "default run config changed after surfacing realism flags: "
        f"{ {k: (cfg_new.__dict__[k], cfg_old.__dict__.get(k)) for k in cfg_new.__dict__ if cfg_new.__dict__[k] != cfg_old.__dict__.get(k)} }")


# ---------- Max realism preset ----------

def test_max_realism_preset_exposed_and_coherent():
    """The preset is exposed via server.PRESETS (served by /api/defaults) and sets the intended
    high-realism bundle using the real curated field names."""
    p = server.PRESETS[_preset_name()]
    assert p.get("generator") == "python-flow"
    assert p.get("pf_lane_changes") is True
    assert p.get("pf_gap") is True
    assert p.get("pf_radio_model") == "logdistance"
    assert p.get("pf_turn") is True                 # turn_slowdown
    assert p.get("pf_lights") is True               # traffic lights
    assert p.get("pf_demand") == "rush"
    assert 0.0 < p.get("pf_vru_pct", 0.0) <= 0.2    # a modest VRU share
    # Guard the arterial/topology validation trap: pin a grid road and keep arterial caps at 0.
    assert p.get("pf_road") == "grid"
    assert not p.get("pf_arterial_every")           # 0 / unset
    assert not p.get("pf_arterial_speed")
    assert not p.get("pf_local_speed")


def test_max_realism_preset_validates_without_error(monkeypatch, tmp_path):
    """Applying the preset over the form defaults and running validate_config must NOT raise --
    especially it must not hit the arterial/topology guard. Drives the real launcher + config
    builder so it exercises the exact production path a GUI click takes."""
    merged = server._defaults()
    merged.update(server.PRESETS[_preset_name()])
    argv = _capture_argv(monkeypatch, tmp_path, merged)
    cfg = _config_from_argv(monkeypatch, argv)  # calls validate_config; raises on any error
    # The realism bundle actually landed in the built config:
    assert cfg.road_network == "grid"
    assert cfg.traffic_flow is True
    assert cfg.n_lanes > 1 and cfg.lane_changes is True   # lane_changes precondition satisfied
    assert cfg.gap_acceptance is True
    assert cfg.radio_model == "logdistance"
    assert cfg.vru_pct > 0.0 and cfg.vru_speed_mps > 0.0  # VRU speed>0 precondition satisfied
    # arterial trap avoided (all caps 0 -> validate_config's arterial block is skipped):
    assert cfg.arterial_every == 0
    assert cfg.arterial_speed_mps == 0.0
    assert cfg.local_speed_mps == 0.0


def test_max_realism_on_a_nongrid_road_would_trip_arterial_guard():
    """Regression anchor documenting the trap the preset avoids: a nonzero arterial cap on a
    non-grid/ring road is rejected by validate_config. (The preset never does this.)"""
    with pytest.raises(ValueError):
        runmod.validate_config(PipelineConfig(
            traffic_flow=True, road_network="linear", arterial_speed_mps=30.0))
