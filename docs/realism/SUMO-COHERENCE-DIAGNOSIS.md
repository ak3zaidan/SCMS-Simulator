# SUMO replay coherence — why the guard fired, and what it found

The SUMO-backed mobility path refuses to run on InTAS. **This is the coherence guard working as
designed**, and the underlying cause is now isolated.

```
ValueError: SUMO replay is NOT coherent with the engine's network: distance-to-road over 4071
sampled replayed positions is p50=2.203 m, p95=17.434 m, max=95.227 m, against a
sumo_offroad_p95_max_m of 8.0 m.
```

Why this matters more than an ordinary failure: had the guard not existed, the run would have
completed and looked plausible while `mapOffRoad` fired on honest vehicles and the geometric
channel's building blockage sat misaligned. It is the silent-corruption case, caught loudly.

## Cause 1 — internal junction lanes are missing (confirmed by measurement)

SUMO vehicles traverse *internal* lanes while crossing a junction. The import reads the network
without them, so a vehicle mid-junction has no edge to be near. Measured over 2,000 sampled trace
positions against the raw SUMO network:

| | edges | p50 | p95 | max | over 8 m |
|---|---|---|---|---|---|
| without internal lanes | 7,942 | 1.30 m | 4.80 m | 17.30 m | 1.8% |
| **with internal lanes** | 23,648 | 1.30 m | **3.20 m** | **6.39 m** | **0.0%** |

Including internal lanes removes the entire tail — nothing exceeds 8 m, so the guard's own threshold
would pass. On a large signalised junction an internal connection is tens of metres long, which is
exactly the scale of the excursions seen.

## Cause 2 — something further degrades it inside the import

The engine measured p50 **2.203**, p95 **17.434**, max **95.227** — worse than the raw network at
*every* percentile, and 5.5× worse at the maximum than the no-internal-lanes case above. So the
imported representation loses geometry beyond the internal lanes.

What the import reports on this network:

```
net_junctions 3332   net_edges 7942
kept_nodes    3328   kept_edges 4393
strong_component_nodes 3289   strongly_connected false
dropped_classes []   short_edges_dropped 0   speeds_clamped ...
```

The edge count is not a loss: two directed SUMO edges between the same node pair are merged into one
physical two-way edge by design (7942 − 4393 = 3549, consistent with that many two-way pairs). The
node count is nearly complete.

The remaining suspect is **shape simplification**. `netimport._canonicalise_shapes` exists, and its
module docstring describes RDP curve simplification inherited from the OSM path. Straightening a long
curved road moves its centreline far from where a vehicle actually drives — which is the right order
of magnitude for a 95 m excursion, and would not show up in node or edge counts at all.

## The fix

1. **Import internal junction lanes**, or make distance-to-road junction-aware so a vehicle inside a
   junction is measured against the junction rather than against approach centrelines. The
   measurement above shows this alone brings the run inside the existing threshold.
2. **Do not simplify geometry on the netconvert path.** RDP tolerance is appropriate for raw OSM,
   where the node budget is the binding constraint; it is wrong for a netconvert network that is
   already clean and now fits comfortably inside the raised caps (3,332 nodes against a 4,000 limit).
   Verify by re-measuring the percentiles after disabling it.
3. **`strongly_connected = false` needs an answer of its own.** 39 nodes sit outside the largest
   strongly connected component. Trips routed onto them can be entered but not left. Decide whether
   to drop them or to keep them and document the consequence — silently keeping them strands vehicles.

## Do not respond by loosening the threshold

`sumo_offroad_p95_max_m = 8.0` is doing real work. The measurement shows a correct import passes it
with p95 of 3.20 m and a maximum of 6.39 m, so the threshold is not too tight — the import is wrong.
Raising it to make the run proceed would restore exactly the silent corruption the guard prevents.

---

# RESOLVED — what it actually was, measured

The replay runs. On the frozen InTAS AM peak (`intas.trace`, sha256 `95a946f4…`, 158,767 rows /
1,188 vehicles / 0 teleports) against `ingolstadt.net.xml` (sha256 `9f16fd82…`):

```
[sumo replay] 1188 frozen trajectories, 158767 vehicle-steps, sumo_seed=257318856 (SUMO 1.25.0),
              teleports=0 | dist_to_road p50=0.363 p95=3.197 max=4.803 m | trace 95a946f4187ca426
vehicles=1188 reports=14504 investigations=155 revoked=155
data_digest=c4a7cddeb4ef58dd254ad051186911ebfd2c0e143b84115ac8f8d967eb082491
```

`sumo_offroad_p95_max_m` is still 8.0 m. It was never the problem and it was never touched.

## Cause 1 confirmed, and answered with junction AREAS rather than internal lanes

Internal lanes were the right diagnosis; importing them is not available. InTAS has **15,706**
internal edges against `roads.CustomNetwork.MAX_EDGES` of **12,000** — before any of them are given
nodes, against a `MAX_NODES` of 4,000 already 83% consumed by 3,328 real junctions. Those caps are
calibrated by measurement (build time, memory, route time), not by taste.

So `dist_to_road` is junction-aware instead. Every junction contributes a **disc** — its own polygon
from the `.net.xml`, reduced to centre plus furthest vertex — and a position inside one is on the
road. Radii over the 3,332 InTAS junctions: p50 **8.97 m**, p90 **12.94 m**, p99 **27.32 m**, max
**70.47 m**. This is not a fudge factor: it is the map's own statement of where the tarmac is, and it
is strictly better evidence than a reconstruction from connections, because it does not depend on
which turns happen to be permitted.

Cost, measured end to end on this import: **+0 graph nodes, +0 graph edges**; `dist_to_road`
**15.9 → 29.6 µs** per COLD call and **0.66 → 0.73 µs** warm — and warm is the case that matters,
because the memo is keyed on the claimed position, so one vehicle-step is computed once however many
receivers hear it. One-off: 324 ms to import the net, 36 ms to build the graph, **64 ms** to install
the surface (23,705 segments + 3,332 discs into a 100 m cell index). The whole 300 s / 1,188-vehicle
InTAS run takes 31.7 s.

## Cause 2 — the suspicion was wrong, and the real one is worse

**There is no RDP on the netconvert path.** `_rdp` exists only in `osm.py`; `netimport.py` never
calls it, and never did. The prime suspect was not guilty.

What is: **`road_network="sumo"` builds `CustomNetwork` from the document's UNDIRECTED `edges`
array, and that array is `[a, b, speed]` triples.** No shape. The curve geometry the importer works
so hard to preserve lives in `directed_edges`, which that path reads only when
`custom_network_directed=true` — off by default. Every curved road in the engine's map was its
straight chord. That is a 95 m error on a long bend, invisible in node and edge counts, and it is
exactly the "worse than the raw net at every percentile" signature.

Second, smaller loss: `_canonicalise_shapes` replaces one carriageway's polyline with the mirror of
the other's, because `CustomNetwork` stores one geometry per undirected node pair and rejects a
second. Measured: **2,165 of 7,941** records substituted, displacing their original vertices by p50
**5.81 m**, p95 **9.61 m**, max **183.69 m**. The tail is not the carriageway offset — it is **35
node pairs joined by more than one distinct physical road**, where one geometry is imposed on the
other.

Both are fixed by separating two questions the routing graph was answering with one object:

| | p50 | p95 | p99 | max | > 8 m |
|---|---|---|---|---|---|
| raw SUMO net, edge centrelines, **no** internal lanes | 1.300 | 4.652 | 8.694 | 18.167 | 1.28% |
| raw SUMO net, **with** internal lanes (the reference) | 1.299 | 3.201 | 4.647 | 6.400 | 0.00% |
| **engine, before**: undirected chords | **2.203** | **17.434** | 47.252 | **95.227** | 12.70% |
| engine + curve geometry on the graph edges | 1.671 | 7.219 | 9.856 | 57.481 | 3.22% |
| engine + per-carriageway geometry, no junctions | 1.300 | 4.776 | 7.963 | 17.173 | 0.66% |
| **engine, shipped**: graph shapes + road surface | **0.363** | **3.197** | 3.203 | **4.803** | **0.00%** |

4,071 sampled positions in every row; the last row is what the run reports. The engine now measures
*inside* the raw-network reference, because the junction polygon covers what an internal lane
centreline only threads.

`netimport.net_to_network` gained two flags, both default-False and both off for every existing
caller: `undirected_shapes=True` (the array in object form, carrying the canonical polyline) and
`surface=True` (`info["road_surface"]`: 7,941 per-carriageway polylines / 23,705 segments, plus the
3,332 junction discs). `roads.CustomNetwork.set_road_surface()` installs the latter, and it is
**geometry, not topology** — no node, no edge, no route and no RNG draw moves because of it.

## Cause 3 — the 39 are dropped

`strong=True` is now unconditional for the engine's SUMO import: **3,328 → 3,289 junctions, 4,393 →
4,344 undirected edges** (39 nodes / 49 edges, 1.2% of the graph).

Keeping them was worse than it looked. It meant the engine's map *depended on
`custom_network_directed`*: off, the router ignored one-ways and the traps were invisible; on,
`run._parse_custom_network` trimmed them silently. Two configurations, two different cities, one
manifest schema describing both. Dropping them in both cases makes the network the manifest
describes the network the run used, and `manifest["mobility"]["network"]` now carries
`nodes_dropped_not_strongly_connected` rather than a bare `strongly_connected: false`.

The roads those junctions carried stay in the **surface** layer. A vehicle SUMO drove down a road
the engine's router declined to use is still on a road, and reporting it `mapOffRoad` would be
precisely the false positive this whole change removes.

## What still guards the gate

The negative arm survives the fix and is tested (`test_a_misregistered_frame_is_refused_WITH_the_
surface_installed`): with the full surface installed, a +250 m / −130 m projection error still puts
p50 well beyond 10 m and p95 past the 8 m gate. A more generous *map* buys coherence for honest
vehicles; it buys nothing for a wrong origin.
