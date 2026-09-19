"""GUI<->engine config-parity guard (audit finding F4).

The curated CONFIG_SPEC must expose the evasion/collusion knobs the python-flow engine actually
honours, so a user can set them from the main form (not only via the raw "all fields" panel):

    --crl-aware-pct   (PipelineConfig.crl_aware_pct)
    --crl-dormant-s   (PipelineConfig.crl_dormant_s)
    --victim-pct      (PipelineConfig.victim_pct)   -- python-flow path, distinct from the
                                                       Java/MOSAIC SCMS_VICTIM_PCT env control.

These tests assert the curated entries exist, are sanely bounded, and are actually emitted onto the
python-flow command line by the real launcher.
"""
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "gui"))
sys.path.insert(0, str(REPO / "scms-sim" / "scenarios"))
import server  # noqa: E402


# The parity knobs added for finding F4: (CLI flag, engine PipelineConfig default).
_PARITY_ARGS = {
    "--crl-aware-pct": 0.0,
    "--crl-dormant-s": 45.0,
    "--victim-pct": 0.10,
}
_FRACTION_ARGS = {"--crl-aware-pct", "--victim-pct"}   # must live in [0, 1]


def _pf_by_arg():
    """python-flow CONFIG_SPEC entries keyed by their CLI arg."""
    return {c["arg"]: c for c in server.CONFIG_SPEC
            if c.get("gen") == "python-flow" and c.get("arg")}


def test_parity_args_present_for_python_flow():
    pf = _pf_by_arg()
    for arg in _PARITY_ARGS:
        assert arg in pf, f"python-flow CONFIG_SPEC is missing a curated entry for {arg}"
        c = pf[arg]
        assert c["type"] == "float", f"{arg} should be a float control (got {c['type']})"


def test_victim_pct_python_flow_entry_is_separate_from_java_env():
    """The python-flow --victim-pct control must be its own arg-routed entry, not the Java env one."""
    pf = _pf_by_arg()
    victim = pf["--victim-pct"]
    assert victim.get("env") is None, "python-flow victim entry must not carry an env route"
    # The Java/MOSAIC path stays untouched: the env-routed victim_pct entry still exists.
    env_victim = [c for c in server.CONFIG_SPEC if c["name"] == "victim_pct"]
    assert len(env_victim) == 1 and env_victim[0].get("env") == "SCMS_VICTIM_PCT"
    assert env_victim[0].get("gen") != "python-flow" and not env_victim[0].get("arg")


def test_parity_defaults_within_bounds():
    pf = _pf_by_arg()
    for arg, engine_default in _PARITY_ARGS.items():
        c = pf[arg]
        d = c["default"]
        assert d == engine_default, (
            f"{arg} default {d} should match PipelineConfig default {engine_default}")
        if "min" in c:
            assert d >= c["min"], f"{arg} default {d} below declared min {c['min']}"
        if "max" in c:
            assert d <= c["max"], f"{arg} default {d} above declared max {c['max']}"
        if arg in _FRACTION_ARGS:
            assert 0.0 <= d <= 1.0, f"{arg} is a fraction; default {d} not in [0, 1]"
            assert c.get("min") == 0 and c.get("max") == 1, (
                f"{arg} fraction control should be bounded 0..1")


def test_launcher_emits_parity_flags(monkeypatch, tmp_path):
    """Drive the REAL python-flow launcher and confirm the new flags+values reach the command line.

    server._start_python_flow builds the argv from CONFIG_SPEC and hands it to subprocess.Popen;
    we stub Popen (and redirect the log file) so nothing actually spawns, then inspect the argv.
    """
    captured = {}

    class _FakeProc:
        returncode = 0

        def poll(self):
            return 0        # looks already-finished, so it never blocks other code

    def _fake_popen(cmd, *a, **k):
        captured["cmd"] = list(cmd)
        return _FakeProc()

    monkeypatch.setattr(server.subprocess, "Popen", _fake_popen)
    monkeypatch.setattr(server, "LOG_PATH", tmp_path / "last_run.log")

    config = {
        "generator": "python-flow",
        "pf_victim": 0.25,
        "pf_crl_aware": 0.40,
        "pf_crl_dormant": 30.0,
    }
    try:
        res = server._start_python_flow(config)
        assert res["ok"] is True
        cmd = captured["cmd"]
        expected = {"--victim-pct": "0.25", "--crl-aware-pct": "0.4", "--crl-dormant-s": "30.0"}
        for flag, val in expected.items():
            assert flag in cmd, f"launcher did not emit {flag}: {cmd}"
            assert cmd[cmd.index(flag) + 1] == val, (
                f"{flag} carried the wrong value: {cmd[cmd.index(flag) + 1]!r} != {val!r}")
    finally:
        server.RUN["proc"] = None   # don't leave a fake 'running' process for later tests
