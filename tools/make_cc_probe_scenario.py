"""Build the CONGESTION-CONTROL probe scenario for the cross-engine radio benchmark.

WHY. ETSI reactive DCC does nothing below a channel busy ratio of 0.30. The InTAS urban window this
benchmark uses for the radio comparison measures CBR 0.027 mean / 0.115 max, so a DCC on/off
comparison there is a comparison of two identical runs -- which is itself a result, but it does not
answer "what would DCC buy where it does engage". This builds a dense procedural grid whose only
purpose is to push the channel past the first DCC breakpoint, so the on/off delta can be measured
rather than asserted.

It is a CHANNEL probe and nothing else: a synthetic grid with no buildings, so every link is LOS and
the delivery numbers from it say nothing about urban propagation. Density is raised through
``mapgen.build(period=...)``, i.e. by inserting MORE DISTINCT TRIPS -- not through SUMO's ``--scale``,
which clones vehicles onto derived route ids that MOSAIC's SumoAmbassador cannot resolve
("Could not retrieve route edges for route '!randUni...:1#1'") and which kills the run at start-up.

    python tools/make_cc_probe_scenario.py [--key grid_12x12] [--period 0.25] [--duration 180s]
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "scms-sim" / "scenarios"))

import mapgen   # noqa: E402  (path set above)


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--key", default="grid_12x12")
    p.add_argument("--period", type=float, default=0.25,
                   help="randomTrips insertion period in seconds; smaller is denser (default 1.5)")
    p.add_argument("--duration", default="180s")
    p.add_argument("--seed", type=int, default=42)
    a = p.parse_args(argv)
    dst = REPO / "scms-sim" / "scenarios" / f"gen_{a.key}"
    out = mapgen.build(a.key, dst, duration=a.duration, seed=a.seed, period=a.period)
    print(f"scenario   {dst}")
    print(f"config     {out.get('scenario_config', dst / 'scenario_config.json')}")
    print(f"period     {a.period} s   duration {a.duration}   seed {a.seed}")
    rou = dst / "sumo" / "map.rou.xml"
    if rou.exists():
        n = rou.read_text(encoding="utf-8", errors="ignore").count("<vehicle ")
        print(f"vehicles   {n} trips in the route file")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
