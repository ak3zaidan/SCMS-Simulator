"""Make the frozen legacy reference importable during tests without an install.

After the ADR 0002 restructure the package lives at ``legacy/scms_sim_ref/``
(it used to be ``src/scms_sim_ref/``), so the directory to put on ``sys.path``
is the one holding this file.
"""

import pathlib
import sys

_ROOT = pathlib.Path(__file__).parent
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))
