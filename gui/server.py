"""SCMS-Simulator control GUI — a dependency-free local web app.

Configure every simulation variable, start/stop runs, and watch live progress + stats.
It drives the tested `run.ps1` pipeline (build -> simulate -> featurize -> validate),
passing configuration through environment variables that the Java back-end/app read.

    python gui\\server.py            # then open http://127.0.0.1:8710
"""

from __future__ import annotations

import csv
import json
import os
import re
import subprocess
import sys
import threading
import time
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RUN_PS1 = REPO / "run.ps1"
LOG_PATH = REPO / "gui" / "last_run.log"
sys.path.insert(0, str(REPO / "src"))
from scms_sim_ref.datagen import validate as validate_mod  # noqa: E402

PORT = 8710

# Every configurable variable: how the form renders it and how it reaches the sim.
# env=None means it is passed as a run.ps1 parameter instead of an environment variable.
CONFIG_SPEC = [
    {"name": "scenario", "label": "Scenario", "type": "select", "default": "smoke",
     "options": ["smoke", "intas"], "env": None,
     "help": "smoke = fast highway (~50 vehicles); intas = real Ingolstadt map (~333)"},
    {"name": "duration", "label": "Duration (intas)", "type": "text", "default": "300s", "env": None,
     "help": "MOSAIC sim duration for the InTAS scenario, e.g. 300s / 600s"},
    {"name": "seed", "label": "Master seed", "type": "int", "default": 20260809, "env": "SCMS_SEED",
     "help": "Deterministic seed — same seed reproduces a byte-identical dataset"},
    {"name": "attacker_pct", "label": "Attacker %", "type": "int", "default": 20, "min": 0, "max": 100,
     "env": "SCMS_ATTACKER_PCT", "help": "Percentage of vehicles that misbehave"},
    {"name": "report_k", "label": "Reporters to revoke (K)", "type": "int", "default": 3, "min": 1, "max": 20,
     "env": "SCMS_REPORT_K", "help": "Distinct reporters before the MA opens an investigation"},
    {"name": "report_prob", "label": "Report probability", "type": "float", "default": 0.9, "min": 0, "max": 1,
     "step": 0.05, "env": "SCMS_REPORT_PROB", "help": "Chance a detection is actually reported (loss/suppression)"},
    {"name": "crl_delay", "label": "CRL propagation (s)", "type": "float", "default": 2.0, "min": 0, "max": 60,
     "step": 0.5, "env": "SCMS_CRL_DELAY", "help": "Delay before a published CRL is enforced by receivers"},
    {"name": "offset_m", "label": "ConstPosOffset shift (m)", "type": "float", "default": 1500.0, "min": 0,
     "max": 5000, "step": 100, "env": "SCMS_OFFSET_M", "help": "Claimed-position offset for the offset attack"},
    {"name": "freeze_updates", "label": "Freeze after N updates", "type": "int", "default": 5, "min": 1, "max": 100,
     "env": "SCMS_FREEZE_UPDATES", "help": "Updates before a ConstPos attacker freezes its claimed position"},
    {"name": "art_max_m", "label": "ART max distance (m)", "type": "float", "default": 1000.0, "min": 100,
     "max": 5000, "step": 100, "env": "SCMS_ART_MAX_M", "help": "Acceptance-Range-Threshold detector cutoff"},
    {"name": "frozen_count", "label": "Frozen CAMs to flag", "type": "int", "default": 3, "min": 1, "max": 50,
     "env": "SCMS_FROZEN_COUNT", "help": "Consecutive frozen CAMs before the frozen-position detector fires"},
    {"name": "cam_interval", "label": "CAM interval (s)", "type": "float", "default": 1.0, "min": 0.1, "max": 10,
     "step": 0.1, "env": "SCMS_CAM_INTERVAL", "help": "Beaconing period (~1 Hz)"},
    {"name": "visualize", "label": "Open MOSAIC 2D map", "type": "bool", "default": False, "env": None,
     "help": "Also open MOSAIC's live 2D web visualizer"},
]

_LOCK = threading.Lock()
RUN = {"proc": None, "logf": None, "out_dir": None, "scenario": None, "config": None,
       "started": None, "finished": None, "returncode": None}


def _defaults() -> dict:
    return {c["name"]: c["default"] for c in CONFIG_SPEC}


def start_run(config: dict) -> dict:
    with _LOCK:
        if RUN["proc"] is not None and RUN["proc"].poll() is None:
            return {"ok": False, "error": "A simulation is already running."}
        scenario = str(config.get("scenario", "smoke"))
        out_name = "mosaic_intas" if scenario == "intas" else "mosaic_poc"
        out_dir = REPO / "datasets" / out_name

        env = os.environ.copy()
        for c in CONFIG_SPEC:
            if c["env"] and c["name"] in config and config[c["name"]] != "":
                env[c["env"]] = str(config[c["name"]])

        cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(RUN_PS1),
               "-Scenario", scenario]
        if scenario == "intas" and config.get("duration"):
            cmd += ["-Duration", str(config["duration"])]
        if config.get("visualize"):
            cmd += ["-Visualize"]

        LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
        logf = open(LOG_PATH, "w", encoding="utf-8", errors="replace")
        proc = subprocess.Popen(cmd, cwd=str(REPO), env=env, stdout=logf,
                                stderr=subprocess.STDOUT, text=True)
        RUN.update(proc=proc, logf=logf, out_dir=out_dir, scenario=scenario, config=config,
                   started=time.time(), finished=None, returncode=None)
        return {"ok": True}


def stop_run() -> dict:
    with _LOCK:
        proc = RUN["proc"]
        if proc is None or proc.poll() is not None:
            return {"ok": False, "error": "No simulation is running."}
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True, text=True)
        return {"ok": True}


_STAGE_RE = re.compile(r"==\s*\[(\d)/5\]\s*([^=]+?)\s*==")
_PROG_RE = re.compile(r"-\s*([0-9]+(?:\.[0-9]+)?)%")


def _log_tail(n: int = 40) -> tuple[str, str, float]:
    stage, progress = "", 0.0
    lines: list[str] = []
    try:
        with open(LOG_PATH, encoding="utf-8", errors="replace") as fh:
            lines = fh.read().splitlines()
    except FileNotFoundError:
        return "", "", 0.0
    for ln in lines:
        m = _STAGE_RE.search(ln)
        if m:
            stage = f"[{m.group(1)}/5] {m.group(2).strip()}"
        p = _PROG_RE.search(ln)
        if p and "Simulating" in ln:
            progress = float(p.group(1))
    return "\n".join(lines[-n:]), stage, progress


def _read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    with open(path, encoding="utf-8") as fh:
        return [json.loads(x) for x in fh if x.strip()]


def compute_stats(out_dir: Path) -> dict | None:
    manifest = out_dir / "manifest.json"
    if not manifest.exists():
        return None
    with open(manifest, encoding="utf-8") as fh:
        man = json.load(fh)
    summary, _leaks = validate_mod.validate(str(out_dir))
    attacks = Counter(a.get("attack_type") for a in _read_jsonl(out_dir / "ground_truth" / "gt_attacks.jsonl"))
    reasons = Counter(r["reason_codes"][0] for r in _read_jsonl(out_dir / "ma" / "ma_reports.jsonl")
                      if r.get("reason_codes"))
    splits: Counter = Counter()
    subj_csv = out_dir / "ml" / "subject_labels.csv"
    if subj_csv.exists():
        with open(subj_csv, encoding="utf-8") as fh:
            for row in csv.DictReader(fh):
                splits[row.get("split", "?")] += 1
    return {
        "counts": man.get("counts", {}),
        "seed": man.get("seed"),
        "entities": man.get("scms_entities", []),
        "data_digest": man.get("data_digest_sha256", "")[:16],
        "validate": summary,
        "attack_types": dict(attacks),
        "detector_reasons": dict(reasons),
        "splits": dict(splits),
    }


def status() -> dict:
    proc = RUN["proc"]
    running = proc is not None and proc.poll() is None
    if proc is not None and not running and RUN["finished"] is None:
        RUN["finished"] = time.time()
        RUN["returncode"] = proc.returncode
        if RUN["logf"]:
            try:
                RUN["logf"].flush()
                RUN["logf"].close()
            except Exception:
                pass
    tail, stage, progress = _log_tail()
    st = {
        "running": running,
        "scenario": RUN["scenario"],
        "stage": stage,
        "progress": progress,
        "log_tail": tail,
        "returncode": RUN["returncode"],
        "elapsed": round((time.time() - RUN["started"]) if RUN["started"] else 0, 1),
        "stats": None,
    }
    if not running and RUN["out_dir"] is not None:
        try:
            st["stats"] = compute_stats(RUN["out_dir"])
        except Exception as ex:  # stats are best-effort
            st["stats_error"] = str(ex)
    return st


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass  # quiet

    def do_GET(self):
        if self.path in ("/", "/index.html"):
            html = (REPO / "gui" / "index.html").read_bytes()
            return self._send(200, html, "text/html; charset=utf-8")
        if self.path == "/api/defaults":
            return self._send(200, {"spec": CONFIG_SPEC, "defaults": _defaults()})
        if self.path == "/api/status":
            return self._send(200, status())
        return self._send(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return self._send(400, {"error": "bad json"})
        if self.path == "/api/start":
            return self._send(200, start_run(body))
        if self.path == "/api/stop":
            return self._send(200, stop_run())
        return self._send(404, {"error": "not found"})


def main():
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"SCMS-Simulator control GUI -> http://127.0.0.1:{PORT}  (Ctrl+C to quit)")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\nshutting down")
        srv.shutdown()


if __name__ == "__main__":
    main()
