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
import os
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "src"))
from scms_sim_ref.mock_pipeline import (PipelineConfig, run_pipeline, config_from_dict,   # noqa: E402
                                        validate_config, config_schema)
from scms_sim_ref.mock_pipeline.run import CLI_PRESETS                                     # noqa: E402
from scms_sim_ref.datagen import validate as validate_mod, benchmark as benchmark_mod, featurize  # noqa: E402

AGENT_OUT = REPO / "datasets" / "agent_run"
AGENT_MAX_DURATION = 150.0     # cap traffic-flow seconds per agent run (keep turns interactive)
AGENT_MAX_STEPS = 200          # cap fixed-fleet steps per agent run
DEFAULT_MODEL = "gpt-4o-mini"
_OPENAI_URL = "https://api.openai.com/v1/chat/completions"


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


def openai_chat(messages: list, tools: list | None, model: str, key: str, timeout: float = 90.0) -> dict:
    """One Chat Completions call. Returns the assistant message dict (may carry tool_calls)."""
    body = {"model": model, "messages": messages, "temperature": 0.2}
    if tools:
        body["tools"] = tools
        body["tool_choice"] = "auto"
    req = urllib.request.Request(_OPENAI_URL, data=json.dumps(body).encode("utf-8"),
                                 headers={"Authorization": "Bearer " + key,
                                          "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        data = json.loads(r.read())
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


def _run_and_analyze(session: AgentSession) -> dict:
    cfg = config_from_dict(session.effective_config())
    validate_config(cfg)
    if cfg.duration_s > AGENT_MAX_DURATION:
        cfg.duration_s = AGENT_MAX_DURATION
    if not cfg.traffic_flow and cfg.n_steps > AGENT_MAX_STEPS:
        cfg.n_steps = AGENT_MAX_STEPS
    cfg.out_dir = str(AGENT_OUT)
    if cfg.live_interval_s <= 0:
        cfg.live_interval_s = 2.0            # leave a live-map snapshot for the GUI (not digested)
    res = run_pipeline(cfg)
    featurize.build(cfg.out_dir)
    summary, _ = validate_mod.validate(cfg.out_dir)
    try:
        bench = benchmark_mod.run(cfg.out_dir)
    except Exception:
        bench = {}
    session.last_out_dir = cfg.out_dir
    session.last_results = _compact_analysis(summary, bench, res)
    return session.last_results


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
        return {"error": f"unknown tool {name!r}"}
    except (ValueError, TypeError) as e:
        return {"error": f"{type(e).__name__}: {e}"}


def run_agent(session: AgentSession, user_msg: str, max_steps: int = 8,
              model: str | None = None, key: str | None = None) -> dict:
    """Run one user turn through the tool-calling loop. Returns {reply, steps, config, results, error}."""
    if key is None:
        key = openai_key()
    model = model or openai_model()
    if not key:
        return {"error": "No OPENAI_API_KEY found in .env", "reply": "", "steps": [], "config": session.config}
    session.history.append({"role": "user", "content": user_msg})
    messages = [{"role": "system", "content": system_prompt()}] + session.history
    steps = []
    try:
        for _ in range(max_steps):
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
                except json.JSONDecodeError:
                    fargs = {}
                result = _exec_tool(session, fn, fargs)
                steps.append({"tool": fn, "args": fargs, "result": result})
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
