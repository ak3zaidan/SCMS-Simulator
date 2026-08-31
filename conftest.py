"""Make the src/ layout importable during tests, and pin PYTHONHASHSEED=0 for the whole suite."""

import os
import pathlib
import sys

_SRC = pathlib.Path(__file__).parent / "src"
if str(_SRC) not in sys.path:
    sys.path.insert(0, str(_SRC))


# --------------------------------------------------------------------------- #
# PYTHONHASHSEED=0 -- HOW THIS IS HANDLED, because it is not a one-liner.
#
# `PYTHONHASHSEED` is read by the interpreter AT STARTUP, before any user code -- including this
# conftest -- runs. Assigning it to `os.environ` here therefore CANNOT change the hash seed of the
# process doing the assigning, and `[tool.pytest.ini_options]` cannot help either (pytest's own
# config is parsed after startup). Only the launcher can set it. So:
#
#   1. HERE: export it, which makes every SUBPROCESS the suite spawns (CLI round-trips, the GUI
#      backend, tools/*) start with a fixed hash seed. Always happens, costs nothing.
#   2. IN THE LAUNCHER: `test.ps1` sets `$env:PYTHONHASHSEED = '0'` before invoking python -- the
#      same mechanism `run.ps1` and `gui.ps1` now use for the engine and the GUI. That is the ONLY
#      place it can take effect for the interpreter running the tests.
#
# Re-execing this interpreter from here was tried and REJECTED: pytest installs its global capture
# on file descriptors 1 and 2 BEFORE the rootdir conftest is imported, so a child launched here
# writes into a capture buffer the parent then discards -- the suite's entire output disappears
# while the exit code still says 0. That is a worse failure than an unpinned hash seed.
#
# WHY IT MATTERS, precisely: the engine's ~20 keyed RNG streams use `random.Random(str)`, and
# CPython seeds those from `int.from_bytes(a + sha512(a).digest())`, which is
# PYTHONHASHSEED-INDEPENDENT -- so this does NOT protect, and cannot move, any pinned digest. What
# it buys is (a) `manifest["runtime"]["hash_randomization"]` becomes a meaningful pinned provenance
# field rather than a per-invocation accident, and (b) set/dict iteration order stops being a latent
# source of run-to-run variation in any path not yet audited for it.
# See docs/realism/PLUGIN-ARCHITECTURE.md phase 0.
# --------------------------------------------------------------------------- #
if os.environ.get("PYTHONHASHSEED") != "0":
    os.environ["PYTHONHASHSEED"] = "0"                       # inherited by every child process
    if sys.flags.hash_randomization and os.environ.get("SCMS_QUIET_HASHSEED") != "1":
        print("[conftest] this interpreter started with hash randomization ON; child processes are "
              "pinned to PYTHONHASHSEED=0, but to pin THIS one run the suite via .\\test.ps1 "
              "(or set PYTHONHASHSEED=0 before launching python).", file=sys.stderr)
