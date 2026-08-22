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


def test_design_network_tool_activates_custom_map(tmp_path, monkeypatch):
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession()
    nodes = [[0, 0], [0, 300], [300, 300], [300, 0], [600, 150]]
    edges = [[0, 1], [1, 2], [2, 3], [3, 0], [2, 4], [3, 4]]
    r = agent._exec_tool(s, "design_network", {"nodes": nodes, "edges": edges})
    assert r["ok"] and r["network"]["n_nodes"] == 5 and r["network"]["n_edges"] == 6
    assert s.config["road_network"] == "custom" and "custom_network" in s.config
    # an invalid design surfaces a clean, actionable error (the AI's feedback loop)
    bad = agent._exec_tool(s, "design_network",
                           {"nodes": nodes, "edges": [[0, 1]], "auto_connect": False})
    assert "error" in bad and "unreachable" in bad["error"]
    assert s.config["road_network"] == "custom"          # failed design didn't clobber the good one
    # the designed map runs end-to-end
    agent._exec_tool(s, "set_config", {"overrides": dict(
        traffic_flow=True, duration_s=40, arrival_rate=1.5, attacker_pct=0.25, seed=7)})
    rr = agent._exec_tool(s, "run_and_analyze", {})
    assert rr["ok"] and rr["analysis"]["vehicles"] > 0


def test_design_network_auto_connects_islands():
    """An LLM design with an isolated district gets bridged via the shortest link, visibly."""
    s = agent.AgentSession()
    nodes = [[0, 0], [0, 200], [200, 0], [1000, 0], [1000, 200]]   # nodes 3-4 are an island
    edges = [[0, 1], [0, 2], [3, 4]]
    r = agent._exec_tool(s, "design_network", {"nodes": nodes, "edges": edges})
    assert r["ok"] and r["auto_connected"] == [[2, 3]]   # closest pair bridges the river
    assert r["network"]["n_edges"] == 4
    # strict mode refuses instead
    r2 = agent._exec_tool(s, "design_network",
                          {"nodes": nodes, "edges": edges, "auto_connect": False})
    assert "error" in r2 and "unreachable" in r2["error"]


def test_design_network_accepts_double_encoded_arrays():
    """LLMs sometimes send arrays as JSON strings; the executor decodes them transparently."""
    s = agent.AgentSession()
    r = agent._exec_tool(s, "design_network", {
        "nodes": "[[0,0],[0,200],[200,200],[200,0]]",
        "edges": "[[0,1],[1,2],[2,3],[3,0]]"})
    assert r["ok"] and r["network"]["n_nodes"] == 4


def test_get_network_supports_incremental_editing():
    """The copilot reads the current map back (custom + spider), edits, and resubmits."""
    s = agent.AgentSession()
    r0 = agent._exec_tool(s, "get_network", {})
    assert r0["ok"] and r0["road_network"] == "linear" and "nodes" not in r0
    agent._exec_tool(s, "design_network", {
        "nodes": [[0, 0], [0, 200], [200, 200], [200, 0]],
        "edges": [[0, 1], [1, 2], [2, 3], [3, 0]]})
    r1 = agent._exec_tool(s, "get_network", {})
    assert r1["ok"] and len(r1["nodes"]) == 4 and r1["stats"]["n_edges"] == 4
    # edit: add a diagonal shortcut and resubmit
    r2 = agent._exec_tool(s, "design_network",
                          {"nodes": r1["nodes"], "edges": r1["edges"] + [[0, 2]]})
    assert r2["ok"] and r2["network"]["n_edges"] == 5
    # spider is readable as an editable graph too
    agent._exec_tool(s, "set_config", {"overrides": {"road_network": "spider", "grid_w": 6,
                                                     "grid_h": 2, "traffic_flow": True}})
    r3 = agent._exec_tool(s, "get_network", {})
    assert r3["ok"] and r3["road_network"] == "spider" and len(r3["nodes"]) == 1 + 6 * 2


def test_import_osm_activates_a_real_city(tmp_path, monkeypatch):
    """import_osm loads a real street graph as the active custom map (skipped offline)."""
    import urllib.error
    s = agent.AgentSession()
    try:
        r = agent._exec_tool(s, "import_osm", {"city": "ingolstadt"})
    except (urllib.error.URLError, OSError):             # no network and no cache yet
        pytest.skip("no network access for the OSM fetch")
    if "error" in r:                                     # any fetch/convert failure => no usable net
        pytest.skip(f"OSM fetch unavailable: {r['error'][:80]}")
    assert r["ok"] and r["network"]["n_nodes"] > 50      # a real city core, not a toy
    assert s.config["road_network"] == "custom" and s.config["traffic_flow"] is True
    assert r["network"].get("speed_limited_edges", 0) > 10   # real speed limits came through
    bad = agent._exec_tool(s, "import_osm", {})
    assert "error" in bad


def test_set_events_tool_validates_and_stores():
    s = agent.AgentSession()
    r = agent._exec_tool(s, "set_events", {"events": [
        {"t": 10, "until": 30, "type": "demand", "mult": 2.0},
        {"t": 20, "type": "weather", "value": "fog"}]})
    assert r["ok"] and len(r["events"]) == 2 and "events" in s.config
    bad = agent._exec_tool(s, "set_events", {"events": [{"t": 5, "type": "hurricane"}]})
    assert "error" in bad and "events" in s.config       # bad timeline left the good one in place
    cleared = agent._exec_tool(s, "set_events", {"events": []})
    assert cleared["ok"] and "events" not in s.config


def test_agent_reports_missing_key():
    s = agent.AgentSession()
    out = agent.run_agent(s, "hi", key="")
    assert out["error"] and "OPENAI_API_KEY" in out["error"]


def test_on_event_fires_around_each_tool(tmp_path, monkeypatch):
    """The progress callback emits tool_start/tool_end around every executed tool (live-UI plumbing)."""
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    script = [
        _msg(tool_calls=[_call("c1", "set_config", {"overrides": dict(
            traffic_flow=True, road_network="grid", duration_s=40, grid_w=5, grid_h=5,
            attacker_pct=0.3, seed=5)})]),
        _msg(tool_calls=[_call("c2", "run_and_analyze", {})]),
        _msg(content="done"),
    ]
    calls = {"i": 0}
    def fake_chat(messages, tools, model, key, timeout=90.0):
        m = script[calls["i"]]; calls["i"] += 1; return m
    monkeypatch.setattr(agent, "_CHAT_FN", fake_chat)

    events = []
    s = agent.AgentSession()
    agent.run_agent(s, "configure and run", key="test",
                    on_event=lambda kind, data: events.append((kind, data.get("tool"))))
    assert events == [("tool_start", "set_config"), ("tool_end", "set_config"),
                      ("tool_start", "run_and_analyze"), ("tool_end", "run_and_analyze")]
    # a throwing callback must not break the turn
    s2 = agent.AgentSession(); calls["i"] = 0
    out = agent.run_agent(s2, "again", key="test",
                          on_event=lambda *a: (_ for _ in ()).throw(RuntimeError("boom")))
    assert out["reply"] == "done" and not out.get("error")


def test_cancel_stops_the_loop_before_tools_run(monkeypatch):
    """If should_cancel() is already true, the turn stops immediately (no LLM/tool calls)."""
    chat_calls = {"n": 0}
    def fake_chat(*a, **k):
        chat_calls["n"] += 1
        return _msg(tool_calls=[_call("c1", "run_and_analyze", {})])
    monkeypatch.setattr(agent, "_CHAT_FN", fake_chat)
    s = agent.AgentSession()
    out = agent.run_agent(s, "go", key="test", should_cancel=lambda: True)
    assert out.get("cancelled") and out["reply"] == agent._CANCELLED_REPLY
    assert chat_calls["n"] == 0 and out["steps"] == []
    assert s._cancel is None                      # cleared after the turn


def test_sweep_honours_cancel_between_runs(tmp_path, monkeypatch):
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession()
    _small_grid(s)
    s._cancel = lambda: True                       # cancel before the first run
    r = agent._sweep(s, "attacker_pct", [0.1, 0.25, 0.4], "recall")
    assert r.get("cancelled") and r["runs"] == [] and r["best"] is None


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
    """Given a sensitivity question, the live copilot explores multiple values — ideally via the sweep
    tool, but a manual multi-run path is also acceptable — and returns usable results.

    (The exact tool choice is LLM-dependent, so we assert the behaviour, not the specific call.)"""
    monkeypatch.setattr(agent, "AGENT_OUT", tmp_path / "agent_run")
    s = agent.AgentSession()
    out = agent.run_agent(s, "On a small traffic-flow grid (about 40 seconds), sweep the attacker "
                             "percentage across 0.1, 0.25 and 0.4 and tell me how revocation recall "
                             "changes and which is highest.", max_steps=10)
    assert not out.get("error"), out.get("error")
    tools = [st["tool"] for st in out["steps"]]
    n_runs = tools.count("run_and_analyze")
    # explored multiple values: one sweep/compare call, or several individual runs
    assert "sweep" in tools or "compare" in tools or n_runs >= 2, tools
    sweeps = [st for st in out["steps"] if st["tool"] == "sweep"]
    if sweeps:
        res = sweeps[-1]["result"]
        assert res.get("runs") and res.get("best"), res
    assert out["results"] and out["results"].get("recall") is not None
    assert isinstance(out["reply"], str) and len(out["reply"]) > 0


# ---------------- scenario library (save / load / list) ----------------
def _scenario_lib(tmp_path, monkeypatch):
    lib = tmp_path / "saved_scenarios"
    monkeypatch.setattr(agent, "SCENARIO_DIR", lib)
    return lib


def test_scenario_round_trip_preserves_network_and_events(tmp_path, monkeypatch):
    _scenario_lib(tmp_path, monkeypatch)
    s = agent.AgentSession()
    agent._exec_tool(s, "design_network", {"nodes": [[0, 0], [0, 200], [200, 200], [200, 0]],
                                           "edges": [[0, 1], [1, 2], [2, 3], [3, 0]]})
    agent._exec_tool(s, "set_events", {"events": [{"t": 10, "type": "weather", "value": "fog"}]})
    agent._exec_tool(s, "set_config", {"overrides": {"duration_s": 60, "arrival_rate": 1.5,
                                                     "attacker_pct": 0.2, "seed": 7}})
    saved_cfg = dict(s.config)
    r = agent._exec_tool(s, "save_scenario", {"name": "river-town", "description": "test world"})
    assert r["ok"] and r["name"] == "river-town" and r["n_fields"] == len(saved_cfg)
    # a FRESH session reloads exactly the same overrides (custom map + timeline included)
    s2 = agent.AgentSession()
    r2 = agent._exec_tool(s2, "load_scenario", {"name": "river-town"})
    assert r2["ok"] and r2["name"] == "river-town"
    assert s2.config == saved_cfg
    assert s2.config["custom_network"] == saved_cfg["custom_network"]
    assert s2.config["events"] == saved_cfg["events"]
    # list is sorted by name and carries description + n_fields
    agent._exec_tool(s2, "save_scenario", {"name": "a-first", "description": "sorts first"})
    ls = agent._exec_tool(s2, "list_scenarios", {})
    assert ls["ok"] and [x["name"] for x in ls["scenarios"]] == ["a-first", "river-town"]
    by_name = {x["name"]: x for x in ls["scenarios"]}
    assert by_name["river-town"]["description"] == "test world"
    assert by_name["river-town"]["n_fields"] == len(saved_cfg)


def test_scenario_bad_names_rejected(tmp_path, monkeypatch):
    lib = _scenario_lib(tmp_path, monkeypatch)
    s = agent.AgentSession()
    agent._exec_tool(s, "set_config", {"overrides": {"attacker_pct": 0.3}})
    for bad in ("../evil", "a b", "a" * 50, "", None, "dot.dot"):
        r = agent._exec_tool(s, "save_scenario", {"name": bad, "description": "x"})
        assert "error" in r and "name" in r["error"], (bad, r)
        r2 = agent._exec_tool(s, "load_scenario", {"name": bad})
        assert "error" in r2, (bad, r2)
    assert not lib.exists() or not list(lib.iterdir())   # nothing was ever written
    # a well-formed name that simply doesn't exist is a clean error too
    missing = agent._exec_tool(s, "load_scenario", {"name": "no-such-scenario"})
    assert "error" in missing and "no-such-scenario" in missing["error"]


def test_load_scenario_with_invalid_value_errors_and_preserves_config(tmp_path, monkeypatch):
    lib = _scenario_lib(tmp_path, monkeypatch)
    lib.mkdir(parents=True)
    (lib / "broken.json").write_text(json.dumps({
        "name": "broken", "description": "invalid enum inside",
        "saved_config": {"road_network": "hyperloop", "attacker_pct": 0.4}}), encoding="utf-8")
    s = agent.AgentSession()
    agent._exec_tool(s, "set_config", {"overrides": {"attacker_pct": 0.3, "traffic_flow": True}})
    before = dict(s.config)
    r = agent._exec_tool(s, "load_scenario", {"name": "broken"})
    assert "error" in r and "road_network" in r["error"]
    assert s.config == before                            # failed load left the session untouched


def test_shipped_example_scenario_loads():
    """saved_scenarios/spider-rush-fog.json (committed) loads through the real executor."""
    s = agent.AgentSession()
    ls = agent._exec_tool(s, "list_scenarios", {})
    assert ls["ok"] and "spider-rush-fog" in [x["name"] for x in ls["scenarios"]]
    r = agent._exec_tool(s, "load_scenario", {"name": "spider-rush-fog"})
    assert r["ok"], r
    assert s.config["road_network"] == "spider" and s.config["grid_w"] == 8
    assert s.config["grid_h"] == 3 and s.config["traffic_flow"] is True
    assert s.config["attacker_pct"] == 0.25 and s.config["demand_profile"] == "rush"
    ev = json.loads(s.config["events"])
    assert ev == [{"t": 45, "type": "weather", "value": "fog"}]
