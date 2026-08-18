"""Unit tests for the GUI server's pure backend logic (no subprocess): config replay validation."""
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "gui"))
sys.path.insert(0, str(REPO / "scms-sim" / "scenarios"))
import server  # noqa: E402


def test_config_replay_rejects_empty_and_invalid_configs():
    # empty / missing payload -> clean error, no process launched
    r = server._start_config_replay({"config_json": {}})
    assert r["ok"] is False and "config_json" in r["error"]
    assert server.RUN["proc"] is None or server.RUN["proc"].poll() is not None
    # a structurally-valid dict with an impossible value -> validation error surfaced (not a crash)
    r = server._start_config_replay({"config_json": {"dt": 0.0}})
    assert r["ok"] is False and "invalid config" in r["error"]


def test_last_effective_config_none_without_a_run(monkeypatch):
    monkeypatch.setitem(server.RUN, "out_dir", None)
    assert server.last_effective_config() is None


def test_config_replay_validation_uses_the_pipeline_validator():
    # bad enum -> surfaced as an invalid-config error, proving config_from_dict+validate_config run
    r = server._start_config_replay({"config_json": {"weather": "hail"}})
    assert r["ok"] is False and "invalid config" in r["error"]
