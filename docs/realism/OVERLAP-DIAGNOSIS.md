# The residual overlap events — diagnosed

`traffic.overlap_events` is the last HARD gate failing in the traffic panel. Two vehicles occupying
the same point at the same instant is a modelling failure, and it corrupts the dataset directly:
overlapping vehicles emit sensor data no real vehicle could produce.

## Measured

Reference config (grid 6, 300 s, seed 42, flow + lights), full emission trace, with and without the
new directed-carriageway geometry:

| | undirected (default) | `directed_lanes = true` |
|---|---|---|
| `traffic.overlap_events` | **290** | **8** |

A 36× reduction — but the gate requires zero, so it still fails.

## What the residual actually is

Classifying every pair under 1 m at the same timestamp by relative heading:

| class | count |
|---|---|
| opposing (rel. heading > 135°) | **0** |
| same direction (rel. heading < 45°) | **10** |
| crossing (45°–135°) | **3** |

**The directed-carriageway work completely solved the class it targeted.** Opposing traffic no longer
shares geometry, and zero opposing overlaps remain.

The residual is two different, pre-existing defects:

**Same-direction (10).** A follower penetrates its leader. Examples: at t=40 s, `veh_095` at
10.4 m/s sits 0.67 m behind `veh_088` at 2.8 m/s; at t=113 s, `veh_234` at 10.7 m/s is 0.35 m from
`veh_204` at 7.7 m/s. In every case the faster vehicle has closed on a slower one. IDM computes a
*desired* acceleration but nothing enforces a minimum gap as a hard constraint, so when leader
detection misses — a lateral-tolerance or lookahead edge case — the follower simply drives through.

**Crossing (3).** Junction conflicts. At t=72 s two vehicles at 1.8 and 0.1 m/s overlap at 90°.
Gap acceptance exists but is opt-in and off by default, and it only covers unsignalised nodes.

## Two ways to fix it, and they are not equivalent

**Enforce a collision constraint in the internal engine.** After the IDM integration step, clamp a
follower so it cannot pass within a minimum gap of its leader, and treat a detected penetration as a
loud modelling error rather than silently allowing it. This fixes the default engine that most users
will run.

**Or inherit SUMO's collision-free movement.** SUMO enforces this by construction — the MOSAIC path
already measures zero overlaps. The SUMO-backed mobility work removes this entire class for free,
*and* is likely to move the other two soft traffic failures (`fd_capacity`, `headway_ks`), because
those are also properties of the hand-rolled car-following model rather than of the map.

Both are worth doing. The second is strategic and already under way; the first still matters because
the internal engine remains the default and is what runs when no SUMO scenario is available.
