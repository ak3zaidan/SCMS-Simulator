# Phase 3 de-risk probe — libsumo determinism & throughput (measured 2026-08-30)

Ran a 6×6 netgenerate grid (`--tls.guess --tls.join`) with randomTrips demand (period 0.5 s,
600 s), driven step-by-step through `libsumo` from Python 3.12, hashing every vehicle's
`(id, x, y, speed, angle)` at every step. Probe script: `tools/sumo_probe.py`.

## Results

| Check | Result |
|---|---|
| Same seed → identical trajectory hash | **Yes** — bit-identical over 141,500 vehicle-steps |
| Different seed → different hash | **Yes** (seed control is real, not ignored) |
| Throughput | **1,335 sim-steps/s** at 338 peak concurrent vehicles (314,919 vehicle-steps/s) |
| Teleports | 0 |

A 300 s scenario at dt = 1 s costs ~0.45 s of wall clock; at dt = 0.1 s (needed for 10 Hz CAMs),
~2.2 s. **Performance is not the constraint on Phase 3.**

## What this changes

The roadmap chose frozen-trajectory replay over live in-loop TraCI for determinism safety, not
speed. That reasoning still holds — the risk was never SUMO's throughput, it was that `run.py`
draws from the global RNG in message-reception order, so interleaving a live external engine can
perturb the draw sequence and silently break the golden digest. Freeze-and-replay keeps SUMO's
nondeterminism *outside* the digest boundary, turning it into a detectable input-hash change.

But now we know a live mode is affordable, so it stays on the table as a documented follow-up
rather than something to design around.

## Traps found (both would have cost an agent a debugging cycle)

1. **`--` in any path breaks SUMO's XML tooling.** SUMO tools embed their own command line — including
   output paths — into an XML comment, and `--` is illegal inside an XML comment. Our session
   scratchpad path (`…/c--Users-Administrator-Documents-SCMS-Simulator/…`) contains `--`, so
   `randomTrips.py` silently emitted an empty routes file and every downstream run had zero
   vehicles. **Phase 3 must generate SUMO scenarios into a path free of `--`,** and should strip
   comments from generated route files defensively.
2. **`randomTrips.py --validate` clobbers `-o`** with a duarouter config dump instead of trips.
   Either drop `--validate` and let SUMO route `<trip>` elements at insertion, or route explicitly
   with duarouter into a separate file.
3. **`--tls.guess-signals` is netconvert-only.** `netgenerate` accepts `--tls.guess` and
   `--tls.join` but rejects `--tls.guess-signals`, `--ramps.guess`, and `--junctions.join`.
   The Phase 1 flag set must be split by tool: procedural maps (netgenerate) get
   `--tls.guess --tls.join`; OSM maps (netconvert) get the full set.
