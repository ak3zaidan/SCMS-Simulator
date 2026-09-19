#!/usr/bin/env python3
"""Compile the SAE J2735 ASN.1 modules into a pycrate runtime module.

This is half of the oracle that validates the hand-written J2735 BSM codec (build
decision D2). It reads the ASN.1 modules from ``$V2XW_J2735_ASN1_DIR`` — which is
outside the repository, because those modules carry an SAE licence forbidding
redistribution (build decision D3) — and writes ``j2735_all.py`` next to itself, or to
``$V2XW_J2735_ORACLE_DIR`` if that is set.

Setting it up from nothing::

    uv venv --python 3.12 /tmp/j2735-oracle/.venv
    VIRTUAL_ENV=/tmp/j2735-oracle/.venv uv pip install pycrate
    V2XW_J2735_ASN1_DIR=/path/to/j2735-2024 \\
    V2XW_J2735_ORACLE_DIR=/tmp/j2735-oracle \\
        /tmp/j2735-oracle/.venv/bin/python compile_j2735.py

``cargo test -p v2xw-msg --test j2735_oracle`` then finds it through
``$V2XW_J2735_ORACLE_DIR`` and runs; without that variable the test skips.
"""

from __future__ import annotations

import glob
import os
import sys
import time

from pycrate_asn1c.asnproc import PycrateGenerator, compile_text, generate_modules


def main() -> int:
    src = os.environ.get("V2XW_J2735_ASN1_DIR")
    if not src:
        print("V2XW_J2735_ASN1_DIR is not set: it must name the directory holding the")
        print("SAE J2735 2024-09 .asn modules (never committed to this repository).")
        return 2
    files = sorted(glob.glob(os.path.join(src, "*.asn")))
    if not files:
        print(f"no .asn files under {src}")
        return 2

    texts = [open(f, encoding="utf-8").read() for f in files]
    started = time.time()
    compile_text(texts, filenames=files)
    print(f"compiled {len(files)} modules in {time.time() - started:.1f}s")

    out_dir = os.environ.get("V2XW_J2735_ORACLE_DIR", os.path.dirname(os.path.abspath(__file__)))
    os.makedirs(out_dir, exist_ok=True)
    out = os.path.join(out_dir, "j2735_all.py")
    generate_modules(PycrateGenerator, out)
    print(f"wrote {out} ({os.path.getsize(out)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
