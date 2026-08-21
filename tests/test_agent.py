"""AI copilot: deterministic tool executors, the tool-calling loop (mock LLM), and a live OpenAI run.

The live test is skipped automatically when no OPENAI_API_KEY is available in .env / the environment.
"""
import json
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "gui"))
import agent  # noqa: E402


# ---------------- deterministic tool executors (no LLM) ----------------
def test_set_config_validates_and_merges():
    s = agent.AgentSession()
    r = agent._exec_tool(s, "set_config", {"overrides": {"traffic_flow": True, "attacker_pct": 0.3}})
    assert r["ok"] and s.config["attacker_pct"] == 0.3 and s.config["traffic_flow"] is True
    # invalid enum surfaces a clean error (not a crash), config unchanged
    r2 = agent._exec_tool(s, "set_config", {"overrides": {"road_network": "hyperloop"}})
    assert "error" in r2 and "road_network" not in s.config
    # unknown field is reported, not fatal
    r3 = agent._exec_tool(s, "set_config", {"overrides": {"nonsense_field": 1}})
    assert "nonsense_field" in r3.get("ignored_unknown", [])


def test_apply_preset_translates_dests_to_fields():
    s = agent.AgentSession()
    r = agent._exec_tool(s, "apply_preset", {"name": "urban_rush"})
    assert r["ok"]
    # argparse dests are translated to PipelineConfig field names
    assert s.config.get("traffic_flow") is True          # from "flow"
    assert s.config.get("road_network") == "grid"        # from "road"
    assert "grid_w" in s.config                           # from "grid"
    assert "duration_s" in s.config                       # from "duration"


def test_reset_and_describe_fields():
    s = agent.AgentSession()
    agent._exec_tool(s, "set_config", {"overrides": {"attacker_pct": 0.4}})
    assert agent._exec_tool(s, "reset_config", {})["config"] == {}
    d = agent._exec_tool(s, "describe_fields", {"names": ["rsu_placement", "attacker_pct"]})
    assert d["fields"]["rsu_placement"]["options"] == ["spread", "perimeter", "center", "corners", "all"]
    assert d["fields"]["attacker_pct"]["max"] == 1


def test_run_and_analyze_produces_metrics(tmp_path, monkeypatch):
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession()
    agent._exec_tool(s, "set_config", {"overrides": dict(
        traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=2.0, grid_w=5, grid_h=5,
        attacker_pct=0.3, seed=7)})
    r = agent._exec_tool(s, "run_and_analyze", {})
    a = r["analysis"]
    assert a["precision"] is not None and a["recall"] is not None
    assert a["vehicles"] > 0 and "recall_by_family" in a
    assert s.last_results is a


def _small_grid(s):
    agent._exec_tool(s, "set_config", {"overrides": dict(
        traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5, grid_w=5, grid_h=5,
        attacker_pct=0.25, seed=7)})


def test_sweep_varies_one_field_and_picks_best(tmp_path, monkeypatch):
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession()
    _small_grid(s)
    r = agent._exec_tool(s, "sweep", {"field": "attacker_pct", "values": [0.1, 0.25, 0.4],
                                      "metric": "recall"})
    assert r["field"] == "attacker_pct" and len(r["runs"]) == 3
    assert all("precision" in row and "recall" in row for row in r["runs"])
    # best is the run maximising recall, and the GUI's last run reflects it
    assert r["best"]["recall"] == max(row["recall"] for row in r["runs"])
    assert s.last_results["recall"] == r["best"]["recall"]
    # sweeping does NOT persist the swept field onto the session config
    assert "attacker_pct" in s.config and s.config["attacker_pct"] == 0.25


def test_sweep_guards(tmp_path, monkeypatch):
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession(); _small_grid(s)
    assert "error" in agent._exec_tool(s, "sweep", {"field": "nope", "values": [1]})
    assert "error" in agent._exec_tool(s, "sweep", {"field": "attacker_pct", "values": [0.1] * 7})
    assert "error" in agent._exec_tool(s, "sweep", {"field": "attacker_pct", "values": []})
    assert "error" in agent._exec_tool(s, "sweep", {"field": "attacker_pct", "values": [0.2],
                                                    "metric": "bogus"})


def test_compare_runs_variants_side_by_side(tmp_path, monkeypatch):
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession(); _small_grid(s)
    r = agent._exec_tool(s, "compare", {"variants": [
        {"label": "clear", "overrides": {"weather": "clear"}},
        {"label": "fog", "overrides": {"weather": "fog"}}]})
    labels = [v["label"] for v in r["variants"]]
    assert labels == ["clear", "fog"]
    assert all("precision" in v and "recall" in v for v in r["variants"])
    # too many variants is rejected
    assert "error" in agent._exec_tool(s, "compare", {"variants": [{"overrides": {}}] * 5})


# ---------------- tool-calling loop with a scripted (mock) LLM ----------------
def _msg(content=None, tool_calls=None):
    m = {"content": content}
    if tool_calls:
        m["tool_calls"] = tool_calls
    return m


def _call(cid, name, args):
    return {"id": cid, "type": "function",
            "function": {"name": name, "arguments": json.dumps(args)}}


def test_agent_loop_executes_scripted_tool_calls(tmp_path, monkeypatch):
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    script = [
        _msg(tool_calls=[_call("c1", "set_config", {"overrides": dict(
            traffic_flow=True, road_network="grid", duration_s=40, grid_w=5, grid_h=5,
            attacker_pct=0.3, seed=5)})]),
        _msg(tool_calls=[_call("c2", "run_and_analyze", {})]),
        _msg(content="Done: I configured a small grid run and it revoked the attackers."),
    ]
    calls = {"i": 0}
    def fake_chat(messages, tools, model, key, timeout=90.0):
        m = script[calls["i"]]; calls["i"] += 1; return m
    monkeypatch.setattr(agent, "_CHAT_FN", fake_chat)

    s = agent.AgentSession()
    out = run = agent.run_agent(s, "make a small grid run with 30% attackers and run it", key="test")
    assert "error" not in out or not out["error"]
    assert out["reply"].startswith("Done")
    assert s.config["attacker_pct"] == 0.3
    assert out["results"] and out["results"]["precision"] is not None
    tools_used = [st["tool"] for st in out["steps"]]
    assert tools_used == ["set_config", "run_and_analyze"]


def test_agent_reports_missing_key():
    s = agent.AgentSession()
    out = agent.run_agent(s, "hi", key="")
    assert out["error"] and "OPENAI_API_KEY" in out["error"]


# ---------------- LIVE OpenAI end-to-end (skipped without a key) ----------------
@pytest.mark.skipif(not agent.openai_key(), reason="no OPENAI_API_KEY in .env/env")
def test_live_agent_configures_runs_and_reports(tmp_path, monkeypatch):
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession()
    out = agent.run_agent(s, "Set up a traffic-flow simulation on a ring road with 12 intersections and "
                             "25% attackers, keep the duration short (about 60 seconds), then run it and "
                             "tell me the revocation precision and recall.", max_steps=8)
    assert not out.get("error"), out.get("error")
    assert s.config.get("road_network") == "ring", s.config
    assert abs(float(s.config.get("attacker_pct", 0)) - 0.25) < 1e-6, s.config
    assert out["results"] and out["results"]["precision"] is not None
    assert isinstance(out["reply"], str) and len(out["reply"]) > 0


@pytest.mark.skipif(not agent.openai_key(), reason="no OPENAI_API_KEY in .env/env")
def test_live_agent_runs_an_experiment(tmp_path, monkeypatch):
    """Given a sensitivity question, the live copilot should use an experiment tool (sweep/compare)
    rather than a single run, and return usable results."""
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession()
    out = agent.run_agent(s, "On a small traffic-flow grid (about 40 seconds), sweep the attacker "
                             "percentage across 0.1, 0.25 and 0.4 and tell me how revocation recall "
                             "changes and which is highest.", max_steps=10)
    assert not out.get("error"), out.get("error")
    tools = [st["tool"] for st in out["steps"]]
    assert "sweep" in tools or "compare" in tools, tools
    sweeps = [st for st in out["steps"] if st["tool"] == "sweep"]
    if sweeps:
        res = sweeps[-1]["result"]
        assert res.get("runs") and res.get("best"), res
    assert isinstance(out["reply"], str) and len(out["reply"]) > 0
