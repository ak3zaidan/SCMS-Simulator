"""Guard the GUI<->CLI contract: every python-flow GUI control maps to a real run.py CLI flag.

The GUI launches the pure-Python generator by translating pf_* form fields into `run.py` command-line
flags. If a field's `arg` drifts from the actual CLI option (rename/typo), the GUI would silently
build a broken command. This test catches that at build time.
"""
import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]


def _run_help():
    env = {**os.environ, "PYTHONPATH": str(REPO / "src")}
    r = subprocess.run([sys.executable, "-m", "scms_sim_ref.mock_pipeline.run", "--help"],
                       capture_output=True, text=True, env=env, cwd=str(REPO))
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_every_python_flow_gui_arg_is_a_real_cli_flag():
    sys.path.insert(0, str(REPO / "gui"))
    sys.path.insert(0, str(REPO / "scms-sim" / "scenarios"))
    import server

    help_text = _run_help()
    pf_args = [c["arg"] for c in server.CONFIG_SPEC
               if c.get("gen") == "python-flow" and c.get("arg")]
    assert pf_args, "expected python-flow GUI controls with CLI args"
    missing = [a for a in pf_args if a not in help_text]
    assert not missing, f"GUI controls reference non-existent run.py flags: {missing}"
