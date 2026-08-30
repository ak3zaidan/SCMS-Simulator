"""Phase 3 de-risk probe: does libsumo drive SUMO deterministically from Python here?

Builds a tiny grid net with netgenerate + randomTrips, then runs it twice under libsumo
with the same seed and compares per-step trajectory hashes. Also measures throughput.
"""
import hashlib
import os
import re
import subprocess
import sys
import time

SUMO_HOME = os.environ["SUMO_HOME"]
BIN = os.path.join(SUMO_HOME, "bin")
WORK = os.path.dirname(os.path.abspath(__file__))
NET = os.path.join(WORK, "probe.net.xml")
TRIPS = os.path.join(WORK, "probe.trips.xml")
CFG = os.path.join(WORK, "probe.sumocfg")
STEPS = 600


def build_scenario():
    subprocess.run(
        [os.path.join(BIN, "netgenerate.exe"), "--grid", "--grid.number", "6",
         "--grid.length", "150", "--tls.guess", "--tls.join", "-o", NET],
        check=True, capture_output=True)
    # Trips only -- no --validate (it clobbers -o with a duarouter config dump).
    # SUMO routes <trip> elements itself at insertion time.
    subprocess.run(
        [sys.executable, os.path.join(SUMO_HOME, "tools", "randomTrips.py"),
         "-n", NET, "-o", TRIPS, "-e", "600", "-p", "0.5", "--seed", "42"],
        check=True, capture_output=True)
    # randomTrips embeds its own command line in an XML comment; the '--' in the
    # options makes that comment illegal XML, so strip comments out.
    with open(TRIPS) as f:
        body = f.read()
    with open(TRIPS, "w") as f:
        f.write(re.sub(r"<!--.*?-->", "", body, flags=re.S))
    with open(CFG, "w") as f:
        f.write('<configuration><input><net-file value="probe.net.xml"/>'
                '<route-files value="probe.trips.xml"/></input>'
                '<time><begin value="0"/><end value="600"/></time></configuration>\n')


def run_once(seed, steps=STEPS):
    import libsumo
    libsumo.start([os.path.join(BIN, "sumo.exe"), "-c", CFG,
                   "--seed", str(seed), "--no-step-log", "true",
                   "--no-warnings", "true", "--time-to-teleport", "-1",
                   "--ignore-route-errors", "true"])
    h = hashlib.sha256()
    n_veh_total = 0
    peak = 0
    t0 = time.time()
    for _ in range(steps):
        libsumo.simulationStep()
        ids = sorted(libsumo.vehicle.getIDList())
        n_veh_total += len(ids)
        peak = max(peak, len(ids))
        for vid in ids:
            x, y = libsumo.vehicle.getPosition(vid)
            v = libsumo.vehicle.getSpeed(vid)
            a = libsumo.vehicle.getAngle(vid)
            h.update(("%s|%.3f|%.3f|%.3f|%.3f;" % (vid, x, y, v, a)).encode())
    elapsed = time.time() - t0
    teleports = libsumo.simulation.getStartingTeleportNumber()
    libsumo.close()
    return h.hexdigest(), elapsed, n_veh_total, peak, teleports


if __name__ == "__main__":
    build_scenario()
    d1, t1, n1, p1, tp1 = run_once(42)
    d2, t2, n2, p2, tp2 = run_once(42)
    d3, _, n3, _, _ = run_once(99)
    print("run1 digest   :", d1[:32], "veh-steps=%d peak=%d" % (n1, p1))
    print("run2 digest   :", d2[:32], "veh-steps=%d peak=%d" % (n2, p2))
    print("seed99 digest :", d3[:32], "veh-steps=%d" % n3)
    print("DETERMINISTIC same-seed :", d1 == d2)
    print("seed changes output     :", d1 != d3)
    if t1 > 0:
        print("perf: %d veh-steps in %.2fs -> %.0f veh-steps/s, %.0f sim-steps/s"
              % (n1, t1, n1 / t1, STEPS / t1))
    print("teleports:", tp1)
