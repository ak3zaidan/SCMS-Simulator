"""AI copilot for the SCMS-Simulator.

A natural-language agent that controls the whole simulator: it reads the config schema, sets any
configuration, runs the pure-Python generator, analyses the resulting dataset (precision/recall,
per-family/-type difficulty, detector reliability, ML AUCs, latency, RSU contribution), and iterates
toward whatever criteria the user describes.

Implemented with the OpenAI Chat Completions function-calling API over stdlib urllib (no new
dependency). The API key is read from the repo-root .env (OPENAI_API_KEY); the model defaults to
gpt-4o-mini (override with OPENAI_MODEL). openai_chat is injectable (_CHAT_FN) so the tool loop can be
unit-tested with a scripted LLM.
"""
from __future__ import annotations

import json
import math
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "src"))
from scms_sim_ref.mock_pipeline import (PipelineConfig, run_pipeline, config_from_dict,   # noqa: E402
                                        validate_config, config_schema)
from scms_sim_ref.mock_pipeline.run import CLI_PRESETS, EVENT_TYPES, _parse_events        # noqa: E402
from scms_sim_ref.mock_pipeline.roads import CustomNetwork                                # noqa: E402
from scms_sim_ref.mock_pipeline.osm import CITY_BBOXES, import_city                       # noqa: E402
from scms_sim_ref.datagen import validate as validate_mod, benchmark as benchmark_mod, featurize  # noqa: E402
from scms_sim_ref.datagen import foundry                                                   # noqa: E402

AGENT_OUT = REPO / "datasets" / "agent_run"
AGENT_MAX_DURATION = 150.0     # cap traffic-flow seconds per agent run (keep turns interactive)
AGENT_MAX_STEPS = 200          # cap fixed-fleet steps per agent run
DEFAULT_MODEL = "gpt-4o-mini"
_OPENAI_URL = "https://api.openai.com/v1/chat/completions"

# Scenario library: designed worlds/timelines saved as JSON so they can be reloaded by name.
SCENARIO_DIR = REPO / "saved_scenarios"
_SCENARIO_NAME_RE = re.compile(r"^[a-zA-Z0-9_-]{1,40}\Z")   # \Z (not $) so "abc\n" is rejected


def _scenario_path(name: str) -> Path:
    """File path for a saved scenario. Raises ValueError unless the name matches the strict
    allow-list (path-traversal guard) — paths are built ONLY from validated names."""
    if not isinstance(name, str) or not _SCENARIO_NAME_RE.match(name):
        raise ValueError(f"invalid scenario name {name!r}: use 1-40 characters from "
                         f"letters, digits, '_' and '-'")
    return SCENARIO_DIR / f"{name}.json"


def scenario_library() -> list:
    """All saved scenarios as [{name, description, n_fields}], sorted by name (GUI + copilot)."""
    items = []
    if SCENARIO_DIR.is_dir():
        for p in SCENARIO_DIR.glob("*.json"):
            if not _SCENARIO_NAME_RE.match(p.stem):
                continue                                 # ignore files we would refuse to load
            try:
                doc = json.loads(p.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError, UnicodeDecodeError):
                continue                                 # unreadable file: skip, never crash the list
            cfg = doc.get("saved_config")
            items.append({"name": p.stem, "description": str(doc.get("description") or ""),
                          "n_fields": len(cfg) if isinstance(cfg, dict) else 0})
    items.sort(key=lambda d: d["name"])
    return items


# --------------------------------------------------------------------------- #
# environment + OpenAI transport
# --------------------------------------------------------------------------- #
def load_env(path: Path | None = None) -> dict:
    """Parse KEY=VALUE lines from .env (repo root). Values may be quoted. Returns a dict."""
    p = path or (REPO / ".env")
    env = {}
    if p.exists():
        for ln in p.read_text(encoding="utf-8").splitlines():
            ln = ln.strip()
            if not ln or ln.startswith("#") or "=" not in ln:
                continue
            k, _, v = ln.partition("=")
            env[k.strip()] = v.strip().strip('"').strip("'")
    return env


def openai_key() -> str:
    return load_env().get("OPENAI_API_KEY") or os.environ.get("OPENAI_API_KEY", "")


def openai_model() -> str:
    return load_env().get("OPENAI_MODEL") or os.environ.get("OPENAI_MODEL") or DEFAULT_MODEL


def _openai_post(body: dict, key: str, timeout: float) -> dict:
    req = urllib.request.Request(_OPENAI_URL, data=json.dumps(body).encode("utf-8"),
                                 headers={"Authorization": "Bearer " + key,
                                          "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def openai_chat(messages: list, tools: list | None, model: str, key: str, timeout: float = 90.0) -> dict:
    """One Chat Completions call. Returns the assistant message dict (may carry tool_calls).

    Model-agnostic: some newer tiers (reasoning / 'pro' models) reject a non-default ``temperature`` or
    an ``unsupported_value`` on it. On a 400 that names ``temperature`` we retry once WITHOUT it, so the
    copilot works across gpt-4o-mini .. gpt-5.x without per-model wiring. Models that accept temperature
    keep the historic 0.2 (byte-identical request), so nothing changes for them.
    """
    body = {"model": model, "messages": messages, "temperature": 0.2}
    if tools:
        body["tools"] = tools
        body["tool_choice"] = "auto"
    try:
        data = _openai_post(body, key, timeout)
    except urllib.error.HTTPError as e:
        detail = ""
        try:
            detail = e.read().decode("utf-8", "replace")
        except Exception:  # noqa: BLE001
            pass
        if e.code == 400 and "temperature" in detail and "temperature" in body:
            body.pop("temperature", None)                # this model wants the default temperature
            data = _openai_post(body, key, timeout)
        else:
            raise
    return data["choices"][0]["message"]


_CHAT_FN = openai_chat   # injectable for tests


# --------------------------------------------------------------------------- #
# config cheat-sheet + system prompt (generated from the live schema)
# --------------------------------------------------------------------------- #
def _cheat_sheet() -> str:
    """Compact, grouped list of every knob with its type/options/range — kept in sync with the schema."""
    sch = config_schema()
    groups: dict[str, list] = {}
    for name, m in sch.items():
        groups.setdefault(m["group"], []).append((name, m))
    lines = []
    for grp in sorted(groups):
        parts = []
        for name, m in groups[grp]:
            if m["options"]:
                spec = "|".join(map(str, m["options"]))
            elif m["widget"] == "bool":
                spec = "true|false"
            elif m["min"] is not None or m["max"] is not None:
                spec = f"{m['min'] if m['min'] is not None else '−inf'}..{m['max'] if m['max'] is not None else 'inf'}"
                if m["unit"]:
                    spec += m["unit"]
            else:
                spec = m["widget"]
            parts.append(f"{name}({spec})")
        lines.append(f"[{grp}] " + ", ".join(parts))
    return "\n".join(lines)


def system_prompt() -> str:
    return (
        "You are the copilot for the SCMS-Simulator, a pure-Python generator of SCMS-aware V2X "
        "misbehaviour-detection datasets. A Misbehavior Authority (MA) collects local detector reports "
        "over a simulated road network, correlates them, resolves identities via two Linkage "
        "Authorities, and revokes attackers. You configure the simulator, run it, read the analysis, "
        "and iterate toward the user's stated criteria.\n\n"
        "WORKFLOW: (1) call set_config with only the fields you want to change (merged onto the current "
        "config); (2) call run_and_analyze to generate + analyse a dataset; (3) read the metrics and, if "
        "the user's criteria are not met, adjust config and run again. Prefer traffic_flow=true with "
        "road_network=grid for realistic runs. Runs are capped for interactivity "
        f"(duration_s<= {int(AGENT_MAX_DURATION)}, fixed-fleet n_steps<= {AGENT_MAX_STEPS}); keep durations "
        "modest (60-150s) while iterating. Use apply_preset for a quick realistic starting point, but "
        "ALWAYS honour values the user names explicitly: apply the preset FIRST, then call set_config to "
        "override every field the user specified (e.g. road_network='ring' when they ask for a ring road, "
        "the exact attacker_pct, intersection count, weather, duration). Never let a preset silently "
        "replace an explicit request.\n\n"
        "Key metrics you get back: precision/recall of revocation, recall_by_family and hardest attack "
        "types, detector_reliability (report-level precision per detector), vehicle/subject ML AUCs, "
        "detection latency, and rsu_contribution. Higher attacker_pct/intensity = easier; stealth/"
        "pulsed/low-intensity = harder; RSUs help in sparse traffic; collusion lowers precision.\n\n"
        "MAP DESIGN: you can create ANY environment -- including REAL cities: import_osm loads an "
        "actual OpenStreetMap street graph (real geometry + speed limits) for the named city cores "
        "or any small bbox; prefer it whenever the user names a real place. Built-ins: "
        "road_network=grid (grid_w x grid_h, "
        "grid_block_m spacing, grid_dropout for irregularity), ring (grid_w nodes), spider (radial "
        "city: grid_w arms x grid_h rings). For everything else use the design_network tool to submit "
        "your own map: nodes = [[x,y], ...] intersection coordinates in METRES (60-250 m spacing is "
        "realistic), edges = [[a,b], ...] node-index pairs -- or [a,b,speed_mps] to give a road a "
        "speed limit (33 ~ highway 120 km/h, 14 ~ arterial 50 km/h, 8.3 ~ residential 30 km/h; "
        "mixing fast and slow roads makes traffic, and detection, far more realistic) -- and the "
        "graph MUST be connected. Design "
        "what the user describes -- a river town with two bridges, a highway with on-ramps feeding a "
        "downtown grid, an airport loop -- dead ends and bounding-box-rim nodes become traffic "
        "sources/sinks, and the map is drawn live in the GUI. Node 0's index matters only as a "
        "reference; the centre-most node receives rush-hour commute bias automatically. Design "
        "CAREFULLY on the first attempt: list the nodes, then write edges checking every index "
        "exists (0..n-1) and every node appears in at least one edge. If validation fails, fix "
        "EXACTLY the reported problem (e.g. connect the listed unreachable nodes) instead of "
        "redesigning from scratch. Prefer 10-40 nodes unless the user asks for more. Custom and "
        "spider maps REQUIRE traffic_flow=true (routed trips) with an arrival_rate (~1-3/s) and a "
        "duration_s -- set those with set_config in the same turn.\n\n"
        "SCENARIO TIMELINE: use set_events for mid-run dynamics (all deterministic): demand surges "
        "({t,until,mult} -- stadium emptying, rush pulse), weather fronts ({t,value} -- fog rolls in, "
        "degrading GNSS + radio and slowing NEW drivers), road closures ({t,until,edge} -- new trips "
        "divert around the closure; bridges that would disconnect the map are refused), attack "
        "waves ({t,until} -- attackers only falsify inside wave windows: coordinated campaigns), and "
        "attack zones ({t,x,y,radius,[until]} -- geofenced campaigns: attackers only falsify while "
        "physically inside an active zone, e.g. 'spoofing near the stadium'). "
        "Combine map + timeline for realistic story scenarios. SCENARIO LIBRARY: after a "
        "successful designed run, offer save_scenario(name, description) so the world/timeline can "
        "be reloaded later, and when the user names a saved scenario, check list_scenarios and use "
        "load_scenario to restore it.\n\n"
        "EXPERIMENTS: when the user asks to vary/sweep/try ONE field across specific values, or asks how "
        "a metric changes with a field, you MUST use the sweep tool in a SINGLE call (never emulate it "
        "with repeated set_config+run_and_analyze) — it varies the field, runs each, and returns the "
        "metric per value plus the best. Use compare for A/B questions (up to 4 labelled override sets, "
        "e.g. clear vs foggy, with vs without RSUs). When the user gives a numeric target (e.g. "
        "'recall >= 0.85'), pick the field most likely to move it, sweep it, adopt the best value with "
        "set_config, then confirm with run_and_analyze; iterate a couple of times if the target is not "
        "yet met, and say so honestly if it cannot be reached within the caps.\n\n"
        "Always finish with a short, plain-language summary of what you set, what happened (key numbers), "
        "and what you'd try next. Be concise. Every configurable field (use exact names):\n" + _cheat_sheet()
    )


# --------------------------------------------------------------------------- #
# tools (OpenAI function specs)
# --------------------------------------------------------------------------- #
def tool_specs() -> list:
    return [
        {"type": "function", "function": {
            "name": "set_config",
            "description": "Merge these field overrides onto the current simulation config and validate "
                           "them. Use exact field names from the cheat-sheet. Returns the effective "
                           "config or a validation error.",
            "parameters": {"type": "object", "properties": {
                "overrides": {"type": "object", "description": "field name -> value (e.g. "
                              "{\"traffic_flow\": true, \"road_network\": \"grid\", \"attacker_pct\": 0.3})"}},
                "required": ["overrides"]}}},
        {"type": "function", "function": {
            "name": "reset_config",
            "description": "Reset the config to defaults.",
            "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {
            "name": "apply_preset",
            "description": "Load a named scenario preset as the base config (then you can tweak it).",
            "parameters": {"type": "object", "properties": {
                "name": {"type": "string", "enum": list(CLI_PRESETS)}}, "required": ["name"]}}},
        {"type": "function", "function": {
            "name": "run_and_analyze",
            "description": "Run the simulator with the current config and return a compact analysis "
                           "(counts, precision/recall, per-family & hardest-type recall, detector "
                           "reliability, ML AUCs, latency, RSU contribution).",
            "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {
            "name": "get_config",
            "description": "Return the current (non-default) config fields.",
            "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {
            "name": "describe_fields",
            "description": "Get full metadata (type, options, range, unit, help) for specific config fields.",
            "parameters": {"type": "object", "properties": {
                "names": {"type": "array", "items": {"type": "string"}}}, "required": ["names"]}}},
        {"type": "function", "function": {
            "name": "sweep",
            "description": "Vary ONE field across up to 6 values (holding all other current-config "
                           "fields fixed), run each, and return precision/recall + the chosen metric "
                           "for every value plus the best. Use this to answer sensitivity questions "
                           "('how does recall change as attacker_pct rises?') and to optimise toward a "
                           "numeric target in one call.",
            "parameters": {"type": "object", "properties": {
                "field": {"type": "string", "description": "config field to vary (exact name)"},
                "values": {"type": "array", "description": "up to 6 values to try for that field"},
                "metric": {"type": "string", "enum": list(_METRICS),
                           "description": "metric to optimise (default recall; latency is lower-better)"}},
                "required": ["field", "values"]}}},
        {"type": "function", "function": {
            "name": "compare",
            "description": "Run up to 4 labelled scenario variants (each a set of overrides merged onto "
                           "the CURRENT config) and return their metrics side by side. Use for A/B "
                           "questions ('clear vs foggy', 'with vs without RSUs').",
            "parameters": {"type": "object", "properties": {
                "variants": {"type": "array", "description": "list of {label, overrides:{field:value}}",
                             "items": {"type": "object", "properties": {
                                 "label": {"type": "string"},
                                 "overrides": {"type": "object"}}}}},
                "required": ["variants"]}}},
        {"type": "function", "function": {
            "name": "design_network",
            "description": "DESIGN A CUSTOM ROAD MAP: submit any connected road graph (nodes in metres, "
                           "undirected edges by node index). Validates the design, activates it "
                           "(road_network=custom), and returns design stats (road length, bbox, dead "
                           "ends, boundary). Isolated parts are auto-connected via the shortest link "
                           "and reported back (set auto_connect=false for strict validation). Use for "
                           "any geography the built-in topologies can't express: highways with "
                           "on-ramps, river towns with bridges, radial avenues... Keep it <= 400 nodes.",
            "parameters": {"type": "object", "properties": {
                "nodes": {"type": "array", "description": "[[x,y], ...] intersection coordinates in "
                          "METRES (typical spacing 60-250 m)",
                          "items": {"type": "array", "items": {"type": "number"}}},
                "edges": {"type": "array", "description": "[[a,b], ...] road segments as node-index "
                          "pairs; the graph must be CONNECTED",
                          "items": {"type": "array", "items": {"type": "integer"}}}},
                "required": ["nodes", "edges"]}}},
        {"type": "function", "function": {
            "name": "import_osm",
            "description": "Import a REAL city's street network from OpenStreetMap as the active "
                           "map (real geometry + real speed limits). Named city cores: "
                           + ", ".join(sorted(CITY_BBOXES)) + "; or pass bbox "
                           "[minLon,minLat,maxLon,maxLat] (keep it a city-core-sized area). "
                           "Cached after the first download.",
            "parameters": {"type": "object", "properties": {
                "city": {"type": "string", "enum": sorted(CITY_BBOXES)},
                "bbox": {"type": "array", "items": {"type": "number"},
                         "description": "[minLon, minLat, maxLon, maxLat] (alternative to city)"}},
                }}},
        {"type": "function", "function": {
            "name": "get_network",
            "description": "Read back the CURRENT road map (nodes/edges + stats) so you can edit it "
                           "incrementally -- add a bypass, close a district, retune speed limits -- "
                           "then resubmit the modified design via design_network.",
            "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {
            "name": "set_events",
            "description": "Set the scenario TIMELINE: deterministic mid-run events. Types: "
                           + "; ".join(f"{k}: {v}" for k, v in EVENT_TYPES.items())
                           + ". Replaces the whole timeline; [] clears it.",
            "parameters": {"type": "object", "properties": {
                "events": {"type": "array", "description": "chronological list of event objects, e.g. "
                           '[{"t":120,"until":240,"type":"demand","mult":3}, '
                           '{"t":60,"type":"weather","value":"fog"}, '
                           '{"t":90,"until":150,"type":"close_edge","edge":[3,7]}, '
                           '{"t":100,"until":200,"type":"attack_wave"}]',
                           "items": {"type": "object"}}},
                "required": ["events"]}}},
        {"type": "function", "function": {
            "name": "save_scenario",
            "description": "Save the CURRENT config overrides (map, timeline, everything set so "
                           "far) to the scenario library under a name so they can be reloaded "
                           "later. Overwrites an existing scenario with the same name.",
            "parameters": {"type": "object", "properties": {
                "name": {"type": "string", "description": "1-40 chars: letters, digits, '_', '-' "
                         "(e.g. river-town-fog)"},
                "description": {"type": "string",
                                "description": "one-line human-readable summary of the scenario"}},
                "required": ["name", "description"]}}},
        {"type": "function", "function": {
            "name": "load_scenario",
            "description": "Load a saved scenario by name: validates its saved config and "
                           "REPLACES the current config with it (then run_and_analyze to execute).",
            "parameters": {"type": "object", "properties": {
                "name": {"type": "string"}}, "required": ["name"]}}},
        {"type": "function", "function": {
            "name": "list_scenarios",
            "description": "List every saved scenario in the library (name, description, number "
                           "of saved config fields), sorted by name.",
            "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {
            "name": "run_foundry",
            "description": "Launch the misbehavior FOUNDRY: a detector-in-the-loop MAP-Elites "
                           "quality-diversity search that builds an archive of DIVERSE + HARD-to-"
                           "detect attack scenarios (one elite per attack_family x density x "
                           "topology x attacker_band cell). It uses an AI semantic mutation operator "
                           "to fill empty cells and intensify the detector's hardest (lowest-recall) "
                           "cells, falling back to a random operator when the AI is unavailable. "
                           "Returns coverage, QD-score and the hardest cells found. This is a heavy, "
                           "MULTI-RUN search -- keep the budget small (<=12) when iterating live.",
            "parameters": {"type": "object", "properties": {
                "budget": {"type": "integer", "description": "mutation/evaluation iterations "
                           "(1-40; each runs a full scenario). Keep <=12 for interactive use."},
                "seed": {"type": "integer", "description": "master seed (deterministic random path)"},
                "objective": {"type": "string", "description": "evade | family:<F> (e.g. "
                              "family:stealth) | latency (default evade)"},
                "duration_s": {"type": "number", "description": "per-scenario sim seconds "
                               "(default 30; capped for interactivity)"}}}}},
    ]


# --------------------------------------------------------------------------- #
# session + tool executors
# --------------------------------------------------------------------------- #
class AgentSession:
    def __init__(self):
        self.config: dict = {}          # overrides on top of PipelineConfig defaults
        self.history: list = []         # OpenAI message list (excluding system)
        self.last_results: dict | None = None
        self.last_out_dir: str | None = None
        self._cancel = None             # callable() -> bool, set for the duration of a turn

    def cancelled(self) -> bool:
        try:
            return bool(self._cancel and self._cancel())
        except Exception:
            return False

    def effective_config(self) -> dict:
        d = {k: (list(v) if isinstance(v, tuple) else v)
             for k, v in PipelineConfig().__dict__.items()}
        d.update(self.config)
        return d


def _defaults() -> dict:
    return {k: (list(v) if isinstance(v, tuple) else v) for k, v in PipelineConfig().__dict__.items()}


# CLI_PRESETS keys are argparse dests; translate to PipelineConfig field names for the agent.
_DEST_TO_FIELD = {"flow": "traffic_flow", "road": "road_network", "grid": "grid_w",
                  "duration": "duration_s", "rotate_period": "rotate_period_s",
                  "radio": "radio_range_m", "radio_range": "radio_range_m",
                  "lanes": "n_lanes", "demand": "demand_profile",
                  "attack_delay_jitter": "attack_delay_jitter_s"}


def _preset_config(name: str) -> dict:
    known = set(_defaults())
    out = {}
    for k, v in CLI_PRESETS[name].items():
        f = _DEST_TO_FIELD.get(k, k)
        if f in known:                       # drop argparse-only dests that aren't config fields
            out[f] = v
    return out


def _compact_analysis(summary: dict, bench: dict, res) -> dict:
    tasks = bench.get("tasks", {})
    def auc(k):
        t = tasks.get(k) or {}
        return t.get("roc_auc")
    rbt = summary.get("recall_by_type", {})
    hardest = sorted(rbt.items(), key=lambda kv: kv[1]["recall"])[:5]
    dr = summary.get("detector_reliability", {})
    gen = bench.get("generalization", {})
    return {
        "vehicles": summary.get("vehicles"), "attackers": summary.get("attackers"),
        "reports": res.n_reports, "revoked": summary.get("revoked"),
        "precision": summary.get("precision"), "recall": summary.get("recall"),
        "false_revocations": summary.get("false_revocations"),
        "recall_by_family": summary.get("recall_by_family"),
        "hardest_types": {t: d["recall"] for t, d in hardest},
        "detector_reliability": {k: v["precision"] for k, v in dr.items()},
        "vehicle_auc": auc("vehicle_is_attacker") or auc("subject_is_attacker"),
        "report_auc": auc("report_is_true_detection"),
        "novel_attack_auc": (gen.get("vehicle_novel_attack") or {}).get("mean_novel_attack_auc"),
        "detection_latency_s": summary.get("detection_latency_s"),
        "rsu_contribution": summary.get("rsu_contribution"),
        "data_digest": res.data_digest[:16],
    }


def _run_config(config: dict, out_dir: str, max_duration: float = AGENT_MAX_DURATION) -> dict:
    """Generate + analyse one dataset from a raw config dict. Pure: no session mutation."""
    cfg = config_from_dict({**_defaults(), **config})
    validate_config(cfg)
    if cfg.duration_s > max_duration:
        cfg.duration_s = max_duration
    if not cfg.traffic_flow and cfg.n_steps > AGENT_MAX_STEPS:
        cfg.n_steps = AGENT_MAX_STEPS
    cfg.out_dir = out_dir
    if cfg.live_interval_s <= 0:
        cfg.live_interval_s = 2.0            # leave a live-map snapshot for the GUI (not digested)
    res = run_pipeline(cfg)
    featurize.build(cfg.out_dir)
    summary, _ = validate_mod.validate(cfg.out_dir)
    try:
        bench = benchmark_mod.run(cfg.out_dir)
    except Exception:
        bench = {}
    return _compact_analysis(summary, bench, res)


def _run_and_analyze(session: AgentSession) -> dict:
    out = str(AGENT_OUT)
    session.last_results = _run_config(session.effective_config(), out)
    session.last_out_dir = out
    return session.last_results


# a few metrics a sweep/compare can be judged on (higher is better unless noted)
_METRICS = ("recall", "precision", "vehicle_auc", "report_auc", "novel_attack_auc",
            "detection_latency_s")


def _metric_of(analysis: dict, metric: str):
    return (analysis or {}).get(metric)


def _sweep(session: AgentSession, field: str, values: list, metric: str = "recall") -> dict:
    """Run the current config once per value of `field`, holding everything else fixed.
    Returns each run's key metrics + the value that best optimises `metric`."""
    if field not in _defaults():
        return {"error": f"unknown field {field!r}"}
    if not isinstance(values, list) or not values:
        return {"error": "values must be a non-empty list"}
    if len(values) > 6:
        return {"error": f"too many values ({len(values)}); use at most 6 per sweep"}
    if metric not in _METRICS:
        return {"error": f"unknown metric {metric!r}; choose from {list(_METRICS)}"}
    base = session.effective_config()
    cap = min(float(base.get("duration_s", AGENT_MAX_DURATION) or AGENT_MAX_DURATION), 90.0)
    runs, best, best_dir = [], None, None
    for i, v in enumerate(values):
        if session.cancelled():
            return {"field": field, "metric": metric, "runs": runs, "best": None,
                    "cancelled": True}
        out = str(AGENT_OUT.parent / f"agent_sweep_{i}")
        try:
            a = _run_config({**base, field: v}, out, max_duration=cap)
        except (ValueError, TypeError) as e:
            runs.append({"value": v, "error": f"{type(e).__name__}: {e}"})
            continue
        row = {"value": v, "precision": a.get("precision"), "recall": a.get("recall"),
               metric: _metric_of(a, metric)}
        runs.append(row)
        m = _metric_of(a, metric)
        lower_better = metric == "detection_latency_s"
        if m is not None and (best is None or (m < best if lower_better else m > best)):
            best, best_dir = m, out
            session.last_results = a          # GUI reflects the current best run
    if best_dir:
        session.last_out_dir = best_dir
    best_row = None
    ok = [r for r in runs if "error" not in r and r.get(metric) is not None]
    if ok:
        lower_better = metric == "detection_latency_s"
        best_row = (min if lower_better else max)(ok, key=lambda r: r[metric])
    return {"field": field, "metric": metric, "runs": runs, "best": best_row}


def _compare(session: AgentSession, variants: list) -> dict:
    """Run several named override sets (each merged onto the CURRENT config) and return their
    metrics side by side. variants = [{"label": str, "overrides": {field: value}}, ...]."""
    if not isinstance(variants, list) or not variants:
        return {"error": "variants must be a non-empty list"}
    if len(variants) > 4:
        return {"error": f"too many variants ({len(variants)}); use at most 4"}
    base = session.effective_config()
    known = set(_defaults())
    cap = min(float(base.get("duration_s", AGENT_MAX_DURATION) or AGENT_MAX_DURATION), 90.0)
    rows = []
    for i, var in enumerate(variants):
        if session.cancelled():
            return {"variants": rows, "cancelled": True}
        if not isinstance(var, dict):
            return {"error": "each variant must be an object with overrides"}
        ov = var.get("overrides") if isinstance(var.get("overrides"), dict) else \
            {k: v for k, v in var.items() if k != "label"}
        unknown = [k for k in ov if k not in known]
        clean = {k: v for k, v in ov.items() if k in known}
        label = var.get("label") or (", ".join(f"{k}={v}" for k, v in clean.items()) or f"variant {i+1}")
        out = str(AGENT_OUT.parent / f"agent_cmp_{i}")
        try:
            a = _run_config({**base, **clean}, out, max_duration=cap)
        except (ValueError, TypeError) as e:
            rows.append({"label": label, "overrides": clean, "error": f"{type(e).__name__}: {e}"})
            continue
        rows.append({"label": label, "overrides": clean, "ignored_unknown": unknown,
                     "precision": a.get("precision"), "recall": a.get("recall"),
                     "vehicle_auc": a.get("vehicle_auc"), "report_auc": a.get("report_auc"),
                     "detection_latency_s": a.get("detection_latency_s")})
        session.last_out_dir = out
        session.last_results = a
    return {"variants": rows}


def _maybe_json(v):
    """LLMs sometimes double-encode array arguments as JSON strings; decode transparently."""
    if isinstance(v, str):
        try:
            return json.loads(v)
        except json.JSONDecodeError:
            return v
    return v


def _auto_connect(nodes, edges) -> tuple[list, list]:
    """Bridge disconnected components with the geometrically shortest links (deterministic:
    distance then lowest indices). Only called on structurally-valid edges. Returns
    (edges_with_bridges, added_edges) -- the additions are reported back to the designer."""
    n = len(nodes)
    adj: dict[int, set] = {i: set() for i in range(n)}
    for a, b in edges:
        adj[int(a)].add(int(b))
        adj[int(b)].add(int(a))
    comps, seen = [], set()
    for i in range(n):
        if i in seen:
            continue
        comp, q = [i], [i]
        seen.add(i)
        while q:
            for m in adj[q.pop()]:
                if m not in seen:
                    seen.add(m)
                    comp.append(m)
                    q.append(m)
        comps.append(sorted(comp))
    edges = [list(e) for e in edges]
    added = []
    main = list(comps[0])
    for comp in comps[1:]:
        best = None
        for a in main:
            for b in comp:
                d = math.dist((float(nodes[a][0]), float(nodes[a][1])),
                              (float(nodes[b][0]), float(nodes[b][1])))
                if best is None or (d, a, b) < best:
                    best = (d, a, b)
        added.append([best[1], best[2]])
        edges.append([best[1], best[2]])
        main.extend(comp)
    return edges, added


# --------------------------------------------------------------------------- #
# LLM semantic mutation operator for the misbehavior foundry (datagen/foundry.py)
#
# This is where the LLM transport legitimately lives (gui/agent.py owns openai_chat/_CHAT_FN and
# already imports datagen). The foundry stays LLM-agnostic: it exposes a generic
# ``mutation_fn(parent_genome, archive_summary, rng) -> genome`` hook, and this module supplies an
# LLM-backed operator for it. The operator proposes MEANINGFUL scenarios that fill the archive's
# empty descriptor cells and intensify the detector's hardest (lowest-recall) cells -- then validates
# them exactly like set_config; on ANY failure it falls back to foundry's deterministic random
# mutate() so the search loop is never broken. (This is OFF the default foundry path.)
# --------------------------------------------------------------------------- #

# The knobs the operator may propose: adversary / environment / topology only -- mirrors the search
# space of foundry.mutate and the 4 descriptor axes (family<-attack_types, density<-arrival_rate,
# topology<-road_network, attacker_band<-attacker_pct) plus within-cell difficulty dials. The
# detector is NEVER weakened (no thresholds / revocation gates / ma_defense here).
_FOUNDRY_KNOBS = ("attack_types", "attacker_pct", "arrival_rate", "road_network", "grid_w", "grid_h",
                  "attack_intensity", "attack_duty_cycle", "crl_aware_pct", "radio_range_m",
                  "weather", "faulty_pct", "gps_degrade_rate", "n_lanes")


def _foundry_knob_sheet() -> str:
    """Compact 'field(spec)' cheat-sheet for just the foundry's mutable knobs (from the live schema)."""
    sch = config_schema()
    parts = []
    for name in _FOUNDRY_KNOBS:
        m = sch.get(name)
        if not m:
            continue
        if m["options"]:
            spec = "|".join(map(str, m["options"]))
        elif m["widget"] == "bool":
            spec = "true|false"
        elif m["min"] is not None or m["max"] is not None:
            lo = m["min"] if m["min"] is not None else "-inf"
            hi = m["max"] if m["max"] is not None else "inf"
            spec = f"{lo}..{hi}" + (m["unit"] or "")
        else:
            spec = m["widget"]
        parts.append(f"{name}({spec})")
    return ", ".join(parts)


def _pick_target_cell(archive_summary: dict):
    """Choose ONE concrete descriptor cell for this mutation to aim at, favouring COVERAGE.

    Returns ``(target_cell, mode)``. Picks an as-yet-EMPTY cell (rotating through the reported empty
    cells by the current coverage count, so successive calls aim at DIFFERENT gaps and the operator
    does not fixate on one region -- the empirically-observed failure mode where the LLM over-exploited
    the hardest cell and collapsed diversity). Only when no empty cell is reported does it fall to the
    hardest filled cell (``mode="intensify"``). Deterministic -- it does NOT draw from the search rng,
    so the fallback path's rng stays pristine (the deterministic-fallback contract is preserved).
    """
    empty = archive_summary.get("empty_cells") or []
    if empty:
        idx = int(archive_summary.get("coverage_cells", 0) or 0) % len(empty)
        return empty[idx], "fill"
    hardest = archive_summary.get("hardest_cells") or []
    if hardest:
        return hardest[0], "intensify"
    return None, "free"


def _foundry_mutation_messages(parent_genome: dict, archive_summary: dict,
                               target_cell=None, mode: str = "fill") -> list:
    """Build the COMPACT chat prompt for one semantic mutation, aimed at ONE specific target cell.

    The operator's PRIMARY objective is COVERAGE (illumination): the model is told to produce a
    scenario that lands EXACTLY in ``target_cell`` -- a specific not-yet-covered cell chosen by
    :func:`_pick_target_cell` -- rather than being free to keep intensifying the hardest region (which
    collapses diversity: measured coverage 4.3 vs random 8.3 on the free-choice prompt). Only when no
    empty cell exists is ``mode == "intensify"``. Inputs given to the model: the target cell + goal,
    the descriptor axes, the attack family->types map, the parent genome, and the valid config fields.
    """
    axes = archive_summary.get("axes", {})
    fam_types = {f: list(ts) for f, ts in foundry.FAMILY_TO_TYPES.items()}
    payload = {
        "target_cell": target_cell,                    # the ONE cell to produce a scenario for
        "goal": ("fill_this_empty_cell" if mode == "fill"
                 else "make_this_hard_cell_harder" if mode == "intensify" else "explore"),
        "descriptor_axes": axes,
        "attack_family_to_types": fam_types,
        "coverage": {"filled": archive_summary.get("coverage_cells"),
                     "empty": archive_summary.get("empty_count"),
                     "grid_size": archive_summary.get("grid_size")},
        "parent_genome": parent_genome,
        "config_fields": _foundry_knob_sheet(),
    }
    system = (
        "You are a red-team scenario designer for a V2X misbehavior-detection foundry running a "
        "MAP-Elites quality-diversity search. The search is scored on ILLUMINATION -- covering as many "
        "distinct descriptor cells (attack_family x density_band x topology x attacker_band) as "
        "possible, each with a hard-to-detect scenario. Your PRIMARY job is COVERAGE: produce ONE new "
        "scenario, expressed as config overrides, that lands EXACTLY in the given target_cell (a cell "
        "not yet covered) so the search reaches a NEW region. Diversity matters MORE than making any "
        "single cell extreme -- do NOT just re-create the parent or the hardest scenario.\n\n"
        "Map target_cell -> config: (a) attack_family -> pick attack_types from attack_family_to_types "
        "for that family (for 'mixed', draw types from 2+ families); (b) topology -> road_network; "
        "(c) attacker_band -> attacker_pct (low<0.15<=med<0.35<=high); (d) density_band -> arrival_rate "
        "(higher = denser). Keep it a REALISTIC, valid misbehavior scenario and NEVER weaken the "
        "detector.\n\n"
        "Rules: (1) reply with ONLY a JSON object of config overrides -- no prose, no markdown fences. "
        "(2) use ONLY the listed config_fields, within their bounds/options. (3) HIT the target_cell's "
        "family/topology/attacker_band exactly. (4) if goal is make_this_hard_cell_harder, keep that "
        "cell's family/topology/attacker_band and push the scenario to lower the detector's recall."
    )
    user = ("Target and constraints (JSON):\n"
            + json.dumps(payload, separators=(",", ":"), default=str)
            + "\n\nReturn ONE JSON object of config overrides that lands in target_cell.")
    return [{"role": "system", "content": system}, {"role": "user", "content": user}]


def _extract_json_object(text: str):
    """Best-effort: pull the first top-level JSON object out of an LLM reply (tolerating markdown
    fences / surrounding prose). Returns the parsed dict, or None if none is found."""
    if not isinstance(text, str) or not text.strip():
        return None
    t = text.strip()
    try:
        return json.loads(t)                       # clean JSON is the common case
    except json.JSONDecodeError:
        pass
    start = t.find("{")                            # otherwise scan for the first balanced {...}
    while start != -1:
        depth = 0
        for i in range(start, len(t)):
            if t[i] == "{":
                depth += 1
            elif t[i] == "}":
                depth -= 1
                if depth == 0:
                    try:
                        return json.loads(t[start:i + 1])
                    except json.JSONDecodeError:
                        break
        start = t.find("{", start + 1)
    return None


def _parse_genome_reply(msg: dict, parent_genome: dict):
    """Parse the LLM reply into a VALIDATED genome, or return None if unusable.

    Keeps only known config fields (exactly like set_config), merges the overrides onto a copy of the
    parent genome (so the rest of the scenario carries over, like foundry.mutate does), and validates
    via config_from_dict + validate_config. Returns the merged genome dict on success, else None."""
    overrides = _extract_json_object((msg or {}).get("content") or "")
    if not isinstance(overrides, dict) or not overrides:
        return None
    known = set(_defaults())
    clean = {k: v for k, v in overrides.items() if k in known}     # drop unknown fields
    if not clean:
        return None
    genome = dict(parent_genome)
    genome.update(clean)
    if isinstance(genome.get("attack_types"), list):
        genome["attack_types"] = tuple(genome["attack_types"])
    cfg = config_from_dict({**_defaults(), **genome})              # same coercion path as set_config
    validate_config(cfg)                                           # raises on infeasible -> caller falls back
    return genome


def llm_mutation_fn(parent_genome: dict, archive_summary: dict, rng, *,
                    chat_fn=None, model: str | None = None, key: str | None = None,
                    timeout: float = 60.0) -> dict:
    """LLM-backed semantic mutation operator matching foundry's ``mutation_fn`` hook signature.

    Aims at ONE specific target cell chosen by :func:`_pick_target_cell` (a not-yet-covered cell for
    COVERAGE, or the hardest cell to intensify only when none are empty), builds a compact prompt via
    :func:`_foundry_mutation_messages`, asks the model (via the injectable ``_CHAT_FN`` / openai_chat)
    for a JSON config-overrides genome that lands in that cell, then parses + filters + validates it
    (config_from_dict + validate_config) and returns it.

    On ANY failure -- no API key, network/HTTP error, non-JSON reply, unknown or invalid config, or
    any exception -- it FALLS BACK to foundry's deterministic random ``mutate(parent, rng)`` so the
    search loop is never broken. ``chat_fn`` / ``model`` / ``key`` default to the module transport +
    .env; tests inject a scripted ``chat_fn`` and an explicit ``key``.

    Determinism: the LLM call is non-deterministic, so LLM-driven foundry runs are NOT byte-identical
    across runs (that is expected). The rng is used only on the deterministic fallback path; the
    default ``mutation_fn=None`` in run_foundry remains the fully deterministic path.
    """
    chat = chat_fn or _CHAT_FN
    model = model or openai_model()
    key = key if key is not None else openai_key()
    try:
        if not key:
            raise RuntimeError("no OPENAI_API_KEY")
        target_cell, mode = _pick_target_cell(archive_summary)     # coverage-first: aim at ONE empty cell
        messages = _foundry_mutation_messages(parent_genome, archive_summary, target_cell, mode)
        msg = chat(messages, None, model, key, timeout)           # tools=None: plain completion
        genome = _parse_genome_reply(msg, parent_genome)
        if genome is None:
            raise ValueError("no usable genome in the LLM reply")
        return genome
    except Exception:  # noqa: BLE001 -- ANY failure => deterministic random fallback (never break loop)
        return foundry.mutate(parent_genome, rng)


def make_llm_mutation_fn(chat_fn=None, model: str | None = None, key: str | None = None):
    """Bind an LLM mutation operator to a (chat_fn, model, key) and return a 3-arg callable matching
    foundry's ``mutation_fn(parent_genome, archive_summary, rng)`` hook (which passes only 3 args)."""
    def _fn(parent_genome, archive_summary, rng):
        return llm_mutation_fn(parent_genome, archive_summary, rng,
                               chat_fn=chat_fn, model=model, key=key)
    return _fn


def run_foundry_llm(budget: int = 60, seed: int = 7, base_duration: float = 40.0,
                    out_dir: str | None = None, objective: str = "evade", verbose: bool = False,
                    model: str | None = None, key: str | None = None, chat_fn=None):
    """Headless entry: run the foundry MAP-Elites search driven by the LLM semantic mutation operator.

    Reads the OpenAI key from .env unless one is passed. This is OFF the default foundry path -- it
    calls ``foundry.run_foundry(mutation_fn=<LLM operator>)``. Because the LLM call is
    non-deterministic the resulting archive is NOT byte-identical across runs (unlike the default
    random operator); every candidate the LLM fails to produce validly falls back to the
    deterministic random mutation, so the search always completes even with no key / no network.
    Returns the ``foundry.Archive``.
    """
    key = key if key is not None else openai_key()
    model = model or openai_model()
    out_dir = out_dir or str(REPO / "datasets" / "foundry_llm")
    mfn = make_llm_mutation_fn(chat_fn=chat_fn, model=model, key=key)
    return foundry.run_foundry(budget=budget, seed=seed, base_duration=base_duration,
                               out_dir=out_dir, objective=objective, verbose=verbose,
                               mutation_fn=mfn)


def _exec_tool(session: AgentSession, name: str, args: dict) -> dict:
    """Run one tool call; returns a JSON-serialisable result (or {error:...})."""
    try:
        if name == "set_config":
            # accept both {"overrides": {...}} and a flat {field: value} (LLMs vary)
            overrides = args.get("overrides") if isinstance(args.get("overrides"), dict) else args
            if not isinstance(overrides, dict):
                return {"error": "overrides must be an object of field->value"}
            known = set(_defaults())
            unknown = [k for k in overrides if k not in known]
            clean = {k: v for k, v in overrides.items() if k in known}   # keep only real fields
            for jf in ("events", "custom_network"):      # JSON-typed fields may arrive as objects
                if jf in clean and not isinstance(clean[jf], str):
                    clean[jf] = json.dumps(clean[jf], separators=(",", ":"))
            merged = {**session.config, **clean}
            cfg = config_from_dict({**_defaults(), **merged})   # coerces types
            validate_config(cfg)                                # raises on bad values
            session.config = merged
            return {"ok": True, "applied": clean,
                    "ignored_unknown": unknown, "config": session.config}
        if name == "reset_config":
            session.config = {}
            return {"ok": True, "config": {}}
        if name == "apply_preset":
            pname = args.get("name")
            if pname not in CLI_PRESETS:
                return {"error": f"unknown preset {pname!r}; valid: {list(CLI_PRESETS)}"}
            session.config = _preset_config(pname)
            return {"ok": True, "preset": pname, "config": session.config}
        if name == "run_and_analyze":
            return {"ok": True, "analysis": _run_and_analyze(session)}
        if name == "get_config":
            return {"ok": True, "config": session.config}
        if name == "describe_fields":
            sch = config_schema()
            return {"ok": True, "fields": {n: sch[n] for n in (args.get("names") or []) if n in sch}}
        if name == "sweep":
            return _sweep(session, args.get("field"), args.get("values"),
                          args.get("metric") or "recall")
        if name == "compare":
            return _compare(session, args.get("variants"))
        if name == "design_network":
            nodes = _maybe_json(args.get("nodes"))       # LLMs sometimes double-encode arrays
            edges = _maybe_json(args.get("edges"))
            added: list = []
            try:
                net = CustomNetwork(nodes, edges)        # raises with design-actionable feedback
            except ValueError as e:
                if args.get("auto_connect", True) and "unreachable" in str(e):
                    edges, added = _auto_connect(nodes, edges)   # bridge isolated parts, visibly
                    net = CustomNetwork(nodes, edges)
                else:
                    raise
            doc = json.dumps({"nodes": nodes, "edges": edges}, separators=(",", ":"))
            session.config = {**session.config, "road_network": "custom", "custom_network": doc,
                              "traffic_flow": True}      # custom maps require routed flow
            out = {"ok": True, "network": net.stats(),
                   "note": "map activated (road_network=custom, traffic_flow=true); it will draw "
                           "under the live map -- still set arrival_rate/duration_s as needed"}
            if added:
                out["auto_connected"] = added
                out["note"] += (f"; {len(added)} edge(s) auto-added to connect isolated parts: "
                                f"{added} -- adjust if that is not the design intent")
            return out
        if name == "import_osm":
            target = args.get("city") or _maybe_json(args.get("bbox"))
            if not target:
                return {"error": "pass city (one of the named cores) or bbox "
                                 "[minLon,minLat,maxLon,maxLat]"}
            nodes, edges, info = import_city(target, str(REPO / "datasets" / "_osmcache"))
            net = CustomNetwork(nodes, edges)
            doc = json.dumps({"nodes": nodes, "edges": edges}, separators=(",", ":"))
            session.config = {**session.config, "road_network": "custom", "custom_network": doc,
                              "traffic_flow": True}
            return {"ok": True, "source": info, "network": net.stats(),
                    "note": "real OSM street graph activated (road_network=custom, "
                            "traffic_flow=true) -- set arrival_rate/duration_s and run"}
        if name == "get_network":
            eff = session.effective_config()
            rn = eff.get("road_network")
            if rn == "custom" and eff.get("custom_network"):
                doc = json.loads(eff["custom_network"])
                net = CustomNetwork(doc["nodes"], doc["edges"])
                return {"ok": True, "road_network": "custom", "nodes": doc["nodes"],
                        "edges": doc["edges"], "stats": net.stats()}
            if rn == "spider":
                from scms_sim_ref.mock_pipeline.roads import spider_graph
                nd, ed = spider_graph(eff.get("grid_w", 6), eff.get("grid_h", 6),
                                      eff.get("grid_block_m", 120.0))
                net = CustomNetwork(nd, ed)
                return {"ok": True, "road_network": "spider", "nodes": nd, "edges": ed,
                        "stats": net.stats(),
                        "note": "resubmit via design_network to customise it"}
            return {"ok": True, "road_network": rn,
                    "note": "procedural topology (grid_w/grid_h/grid_block_m control it); use "
                            "design_network to switch to a fully custom map"}
        if name == "set_events":
            evs = _maybe_json(args.get("events"))
            if not isinstance(evs, list):
                return {"error": "events must be a list of event objects"}
            doc = json.dumps(evs, separators=(",", ":"))
            parsed = _parse_events(doc)                                  # raises on a malformed event
            if not parsed:
                session.config = {k: v for k, v in session.config.items() if k != "events"}
                return {"ok": True, "events": [], "note": "timeline cleared"}
            session.config = {**session.config, "events": doc}
            return {"ok": True, "events": parsed,
                    "note": f"{len(parsed)} event(s) scheduled (validated against the timeline rules)"}
        if name == "save_scenario":
            sname = args.get("name")
            path = _scenario_path(sname)                 # raises on a bad name (traversal guard)
            SCENARIO_DIR.mkdir(parents=True, exist_ok=True)
            doc = {"name": sname, "description": str(args.get("description") or ""),
                   "saved_config": dict(session.config)}
            with open(path, "w", encoding="utf-8", newline="\n") as fh:
                json.dump(doc, fh, indent=2)
            return {"ok": True, "name": sname, "n_fields": len(session.config)}
        if name == "load_scenario":
            sname = args.get("name")
            path = _scenario_path(sname)                 # raises on a bad name (traversal guard)
            if not path.exists():
                return {"error": f"no saved scenario named {sname!r}; call list_scenarios to see "
                                 f"the library"}
            doc = json.loads(path.read_text(encoding="utf-8"))
            saved = doc.get("saved_config")
            if not isinstance(saved, dict):
                return {"error": f"scenario {sname!r} is malformed (saved_config must be an object)"}
            known = set(_defaults())
            clean = {k: v for k, v in saved.items() if k in known}
            for jf in ("events", "custom_network"):      # JSON-typed fields may be stored as objects
                if jf in clean and not isinstance(clean[jf], str):
                    clean[jf] = json.dumps(clean[jf], separators=(",", ":"))
            cfg = config_from_dict({**_defaults(), **clean})   # same validation path as set_config
            validate_config(cfg)                         # raises -> {error}; session.config untouched
            session.config = clean                       # REPLACE (not merge): the scenario is the world
            return {"ok": True, "name": sname, "config": session.config}
        if name == "list_scenarios":
            return {"ok": True, "scenarios": scenario_library()}
        if name == "run_foundry":
            budget = max(1, min(int(args.get("budget") or 8), 40))       # cap: a heavy multi-run search
            seed = int(args.get("seed") or 7)
            objective = str(args.get("objective") or "evade")
            duration = max(8.0, min(float(args.get("duration_s") or 30.0), AGENT_MAX_DURATION))
            out = str(AGENT_OUT.parent / "agent_foundry")
            key = openai_key()
            archive = run_foundry_llm(budget=budget, seed=seed, base_duration=duration,
                                      out_dir=out, objective=objective, key=key)
            m = archive.meta
            hardest = sorted(archive.cells.values(), key=lambda c: -c["fitness"])[:5]
            return {"ok": True, "objective": m["objective"], "seed": m["seed"], "budget": m["budget"],
                    "coverage_cells": m["coverage_cells"], "grid_size": m["grid_size"],
                    "coverage_pct": m["coverage_pct"], "qd_score": m["qd_score"],
                    "best_fitness": m["best_fitness"], "out_dir": out,
                    "ai_operator": bool(key),           # False -> ran on the random-fallback operator
                    "hardest_cells": [{"descriptor": c["descriptor"], "fitness": c["fitness"],
                                       "recall": (c.get("metrics") or {}).get("recall")}
                                      for c in hardest]}
        return {"error": f"unknown tool {name!r}"}
    except Exception as e:                       # noqa: BLE001 - the tool boundary must never kill a
        return {"error": f"{type(e).__name__}: {e}"}   # turn; the error is the LLM's repair signal


_CANCELLED_REPLY = "(stopped at your request)"


def run_agent(session: AgentSession, user_msg: str, max_steps: int = 14,
              model: str | None = None, key: str | None = None, on_event=None,
              should_cancel=None) -> dict:
    """Run one user turn through the tool-calling loop. Returns {reply, steps, config, results, error}.

    on_event(kind, data) is an optional progress callback (for live UI): kind is
    "tool_start" ({tool, args}) or "tool_end" (the finished step dict).
    should_cancel() -> bool is polled between steps (and inside sweep/compare) to stop early."""
    def emit(kind, data):
        if on_event:
            try:
                on_event(kind, data)
            except Exception:                               # never let UI plumbing break a turn
                pass
    if key is None:
        key = openai_key()
    model = model or openai_model()
    if not key:
        return {"error": "No OPENAI_API_KEY found in .env", "reply": "", "steps": [], "config": session.config}
    session._cancel = should_cancel
    session.history.append({"role": "user", "content": user_msg})
    messages = [{"role": "system", "content": system_prompt()}] + session.history
    steps = []
    try:
        for _ in range(max_steps):
            if session.cancelled():
                return {"reply": _CANCELLED_REPLY, "steps": steps, "config": session.config,
                        "results": session.last_results, "cancelled": True}
            msg = _CHAT_FN(messages, tool_specs(), model, key)
            # normalise to a plain dict we can append back
            asst = {"role": "assistant", "content": msg.get("content")}
            if msg.get("tool_calls"):
                asst["tool_calls"] = msg["tool_calls"]
            messages.append(asst)
            session.history.append(asst)
            calls = msg.get("tool_calls") or []
            if not calls:
                reply = msg.get("content") or ""
                return {"reply": reply, "steps": steps, "config": session.config,
                        "results": session.last_results}
            for call in calls:
                fn = call["function"]["name"]
                try:
                    fargs = json.loads(call["function"].get("arguments") or "{}")
                except json.JSONDecodeError as je:
                    fargs = {}
                    emit("tool_start", {"tool": fn, "args": fargs})
                    result = {"error": f"your tool-call arguments were not valid JSON ({je}); "
                                       f"resend the COMPLETE call with well-formed JSON"}
                    step = {"tool": fn, "args": fargs, "result": result}
                    steps.append(step)
                    emit("tool_end", step)
                    tool_msg = {"role": "tool", "tool_call_id": call["id"],
                                "content": json.dumps(result)}
                    messages.append(tool_msg)
                    session.history.append(tool_msg)
                    continue
                emit("tool_start", {"tool": fn, "args": fargs})
                result = _exec_tool(session, fn, fargs)
                step = {"tool": fn, "args": fargs, "result": result}
                steps.append(step)
                emit("tool_end", step)
                tool_msg = {"role": "tool", "tool_call_id": call["id"],
                            "content": json.dumps(result, default=str)}
                messages.append(tool_msg)
                session.history.append(tool_msg)
        return {"reply": "(stopped after the step limit — ask me to continue)", "steps": steps,
                "config": session.config, "results": session.last_results}
    except urllib.error.HTTPError as e:
        detail = e.read().decode("utf-8", "replace")[:300] if hasattr(e, "read") else str(e)
        return {"error": f"OpenAI HTTP {e.code}: {detail}", "steps": steps, "config": session.config,
                "reply": ""}
    except Exception as e:                                  # noqa: BLE001 - surface to the UI
        return {"error": f"{type(e).__name__}: {e}", "steps": steps, "config": session.config, "reply": ""}
    finally:
        session._cancel = None
