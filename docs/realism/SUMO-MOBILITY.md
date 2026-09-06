# SUMO-backed mobility for the Python engine (freeze + replay)

**Status: shipped, default OFF.** `mock_pipeline/sumo_trace.py` (new) +
`mock_pipeline/run.py` (the seam) + `tests/test_sumo_mobility.py` (39 tests), plus the road-geometry
fix the first real city forced (`roads.CustomNetwork.set_road_surface`, below).

> **The payoff is measured in `PYTHON-ENGINE-VALIDATION.md`**: the Python engine driven over a full
> clock hour of the InTAS AM peak and graded against real Ingolstadt loop counts, with the adapter
> itself measured at **0.000 m** position error over 709,454 samples. Two default-inert options were
> added to `freeze()` to make a real city's peak hour freezable at all — `warmup_steps` and
> `substeps`; see that document's section 6 for why each was forced.

## Why

There are two engines and only one of them was ever validated against reality.

| | MOSAIC/Java path | **Python path** |
|---|---|---|
| Mobility | real SUMO | hand-rolled IDM over a synthetic/OSM graph |
| Validated against real counts | yes — every GEH number in `GEH-RESULT.md` | **never** |
| Writes the datasets a researcher uses | no | **yes** |

So the datasets come from the unvalidated engine. This closes it: SUMO produces the movement, the
Python engine replays it and keeps its entire SCMS / attack / detector / MA stack on top.

## Why freeze-and-replay, not live in-loop TraCI

Not speed — `libsumo` here is bit-deterministic and fast enough to drive in-loop (141,500
vehicle-steps hash-identical on a fixed seed, 1,335 sim-steps/s at 338 concurrent vehicles;
`PHASE3-PROBE.md`). The problem is on the *other* side of the seam: `run_pipeline` draws packet
loss, `report_prob`, collusion and `net_delay` from one global `random.Random`, and the **count and
order** of those draws is load-bearing for the pinned goldens. Interleaving a live external engine
perturbs that sequence.

Freezing keeps SUMO's nondeterminism **outside the digest boundary**. The artifact's sha256 is
recorded in `manifest["config"]["sumo_trace_sha256"]`, so a re-frozen trajectory is a **detectable
input change** — a refused run with a diagnostic — rather than a silent `data_digest` break that
reads as an engine regression.

## Phase A — the frozen artifact

`sumo_trace.freeze()` runs SUMO **once**, single-threaded (`--threads 1`), teleport-free
(`--time-to-teleport -1`), at a seed derived from the run seed
(`sha256(f"{run_seed}|sumo")[:4] mod 2**31-1`), and writes canonical line-oriented text:

```
#scms-sumo-trace/1
#meta {...}                    one line of JSON, sort_keys, no insignificant space
#vehicles <n>
V <idx> <sumo_id> <first_step> <last_step> <depart_s> <arrive_s> <arrived> <route_len_m>
#rows <n>
<step> <idx> <x> <y> <speed> <angle>       sorted by (step, idx); every value at 3 decimals
```

* `idx` is the **stable id mapping**, assigned in `(first_step, sumo_id)` order and *written into
  the file* — nothing downstream re-derives it.
* `x`/`y`/`angle` are **raw SUMO network coordinates** and SUMO's own angle convention (deg
  clockwise from North). Deliberately not converted at freeze time — see coherence below.
* `meta` records the SUMO version, the pinned version, the derived seed and the derivation, dt,
  begin/end, `step0_sim_time`, teleport and collision counts, and the **exact invocation** with file
  paths reduced to basenames. The inputs are pinned by **content** (`net_sha256`, `routes_sha256`),
  which is strictly stronger than pinning them by path and keeps the artifact's own hash portable.
* `-0.000` is normalised to `0.000` so a sign-of-zero difference can never move the hash.

## Phase B — the replay seam

`SumoReplayMobility` is registered as a built-in on the registry's **previously empty `mobility`
slot** (alongside `InternalMobility`, which names the IDM model so it is *selected* rather than
assumed). It replaces exactly two things in `run_pipeline`:

1. **The spawn schedule.** `while _replay is None:` disables the engine's thinning/`expovariate`
   arrival process; departures, routes and car-following are all SUMO's. Role assignment
   (attacker / faulty / colluder) still happens, from a dedicated `{seed}:sumoflow` stream.
2. **One line in the step loop**, exactly where `car_follow` sits:
   `elif _replay is not None: _replay.advance(active_list, step, t)` — writing
   `cur_x/cur_y/cur_v/cur_h/s_pos`. `cf_active` is forced off so the IDM never re-integrates on top
   of a frozen trajectory.

The provider draws **zero randomness** (asserted by a test that rebinds
`random.Random.random`/`getrandbits` to raise). Heading is converted once at construction:
`(90 - sumo_angle) mod 360`, into the engine's declared `deg_ccw_from_east`.

## Network coherence — one transform, two consumers

`sumo_trace.engine_network()` reads the `.net.xml` once through `netimport` and returns **both** the
imported graph *and the transform it applied*. The replay provider is constructed with that same
transform, so the vehicles and the roads land in one frame **by construction**, not by agreement.
`sumo_frame_city` re-projects a geo-referenced net into `osm.py`'s exact `(lat0, lon0, kx, ky)` frame
so the net registers with `osm.py` roads and building footprints (the trap `osm.py` documents: an
independently derived origin misaligns by whole city blocks while every street still looks fine).

The run then **asserts** it, before step 0, over a deterministic sample of the replayed positions:

| metric | measured (6×6 netgenerate grid, 150 m blocks, 30,420 vehicle-steps) |
|---|---|
| dist-to-road p50 | **1.600 m** |
| p95 | **1.600 m** |
| p99 | 1.778 m |
| max | **2.955 m** |
| mean | 1.579 m |

1.60 m is exactly the half-width of SUMO's 3.2 m lane: the vehicle is on a *lane centre*, the
engine's edge is the junction-to-junction line. That is what road-following looks like. The gate
(`sumo_offroad_p95_max_m`, default 8 m) is also tested **negatively**: a deliberately misregistered
frame (+250 m, −130 m) is refused.

### The gate fired on a real city, and it was right — `SUMO-COHERENCE-DIAGNOSIS.md`

A procedural grid is straight, single-carriageway and has junctions the size of a car. A real
netconvert city is none of those, and on the frozen InTAS AM peak the gate refused the run at p95
**17.434 m**, max **95.227 m**. It was correct to: the engine's road geometry was wrong, in two
ways that a grid cannot expose.

1. **The engine builds from the document's UNDIRECTED `edges` array, and those are `[a, b, speed]`
   triples — no shape.** Every curved road was its straight chord; the curve geometry the importer
   preserves lives in `directed_edges`, which `custom_network_directed=false` (the default) never
   reads. `netimport.net_to_network(undirected_shapes=True)` now emits that array in object form
   carrying the canonical polyline. *(The suspected cause, RDP simplification, was measured and
   cleared: `_rdp` exists only in `osm.py` and `netimport.py` has never called it.)*
2. **`dist_to_road` answered a MAP question with the ROUTING GRAPH.** One centreline per physical
   road — but a two-way street has two carriageways, and 35 InTAS node pairs carry more than one
   distinct road, so `_canonicalise_shapes` has to impose one geometry on the others (2,165 of
   7,941 records, displacing vertices p50 5.81 m / max 183.69 m). And a graph edge stops at the
   junction *centre*, while a vehicle crossing a signalised junction drives an internal lane for
   tens of metres that no edge covers.

`roads.CustomNetwork.set_road_surface()` splits the two: the graph stays the router's, and
`dist_to_road` measures against the map's **drivable surface** — every edge's own polyline (7,941
polylines / 23,705 segments) plus every junction's own polygon reduced to a disc (3,332 discs;
radius p50 8.97 m, p99 27.32 m, max 70.47 m). Importing SUMO's internal lanes instead was not
available: 15,706 of them against `MAX_EDGES` 12,000.

| InTAS, 4,071 sampled replayed positions | p50 | p95 | max | > 8 m |
|---|---|---|---|---|
| raw SUMO net, no internal lanes | 1.300 | 4.652 | 18.167 | 1.28% |
| raw SUMO net, with internal lanes (reference) | 1.299 | 3.201 | 6.400 | 0.00% |
| engine, before | 2.203 | 17.434 | 95.227 | 12.70% |
| **engine, now** | **0.363** | **3.197** | **4.803** | **0.00%** |

The surface is geometry, not topology: no node, no edge, no route and no RNG draw changes. It costs
`dist_to_road` 15.9 → 29.6 µs per cold call (0.66 → 0.73 µs on a memo hit, which is the per-receiver
case) and a one-off 64 ms to install; the whole 300 s / 1,188-vehicle InTAS run takes 31.7 s. The
negative arm is re-tested *with the surface installed* — a +250 m / −130 m frame error is still
refused, because a more generous map buys coherence for honest vehicles and nothing at all for a
wrong origin.

**Strong connectivity is now unconditional on this path** (3,328 → 3,289 junctions, 4,393 → 4,344
edges). Those 39 junctions can be entered and never left; keeping them made the engine's map depend
on `custom_network_directed` — invisible with it off, silently trimmed by `_parse_custom_network`
with it on. The roads they carried remain in the *surface*, because a vehicle on a road the router
declined to use is still on a road.

## Certificate lifetime from the SUMO route

The internal model has to *guess* a trip's duration (`3 × free-flow + half a signal cycle per
intersection + 30 s`) because congestion makes arrival dynamic — and under replay that guess is
computed from `trip_speed_min`, `grid_block_m` and `traffic_lights`, none of which describe a SUMO
scenario at all. SUMO already drove the trip, so the budget is the **exact** span plus
`sumo_cert_slack_s`.

Measured on a congested trace (6×6 grid, `randomTrips -p 0.18`, 1,541 vehicles, median driven route
250 m over a median 142 s presence):

* the internal estimator **under-budgets 610 / 1541 vehicles (39.6 %)**, worst shortfall **148.6 s**
* that costs **414 pseudonym rotations** at `rotate_period_s = 60`
* with the exact budget: **0 `certValidity` reports against honest vehicles**

## Config surface (all default-inert)

| field | default | group | CLI |
|---|---|---|---|
| `mobility_source` | `internal` | Mobility | `--mobility-source` |
| `sumo_trace` | `""` | Mobility | `--sumo-trace` |
| `sumo_trace_sha256` | `""` | Mobility | `--sumo-trace-sha256` |
| `sumo_cert_slack_s` | `30.0` | Mobility | `--sumo-cert-slack` |
| `sumo_offroad_p95_max_m` | `8.0` | Mobility | `--sumo-offroad-p95-max` |
| `sumo_net` | `""` | Network | `--sumo-net` |
| `sumo_frame_city` | `""` | Network | `--sumo-frame-city` |
| `sumo_buildings` | `""` | Network | `--sumo-buildings` |

**`sumo_buildings` is the whole-city scene's other half** and it is not decoration. A `.net.xml`
carries roads and no footprints; a SUMO scenario ships its buildings in a separate polygon
additional-file. Without it, the biggest map in this project runs with no buildings at all and the
geometric channel falls back to the synthetic urban-canyon density — worth **1.60× in PDR at 200 m**
on InTAS, in the wrong direction. The polygons go through the SAME `netimport._transformer` closure
the junctions did (`netimport.scene_from_net` reads the net itself rather than accepting a transform,
so the two layers cannot end up in different frames) and are GATED on landing on those junctions: the
gate fires on an 11 m displacement of the footprint layer, and every alignment statistic it measured
is recorded in `manifest.counts.scene_buildings`. Measurements, thresholds and the displacement
bisection: `docs/realism/FULL-CITY-SCENE.md`.

`freeze()` itself gained two options, both default-inert and both keyed out of `meta` unless
engaged, so no existing artifact's sha256 moves:

| flag | default | what it is for |
|---|---|---|
| `--warmup STEPS` | `0` | steps run from `--begin` WITHOUT recording. `--begin 25200` makes SUMO discard every vehicle departing earlier, so a peak-hour window opened cold starts on an EMPTY city — **36 vehicles ten seconds in**, against ~3,400 with an hour of warm-up ahead of it. |
| `--substeps N` | `1` | step SUMO at `dt/N` and record every Nth state. A scenario is calibrated at a step length; re-integrating InTAS at 1 s is a different model, and SUMO 1.25.0 aborts outright (`Request lateral offset of vehicle … for invalid lane`) after producing 34 collisions the calibrated configuration does not have. |
| `--time-to-teleport S` | `-1` (never) | `-1` is a change to the SCENARIO, not just to the artifact: InTAS's AM peak teleports **365 times in 25,271 vehicles** under its own 300 s policy, and suppressing all of them leaves those vehicles stuck, depressing the very flows a validation measures. Pass the scenario's own value to keep its calibrated mobility. |
| `--split-on-gap` | off | a teleported (or parked) vehicle leaves `getIDList()` and returns elsewhere; the format's contiguity guard refuses that trace. This records the return as a separate trajectory `<id>#<n>` — which is also the better model, since a second vehicle appearing is what a teleport physically resembles, whereas a 500 m one-step jump is what every plausibility detector here is built to flag. |

### A SUMO crash that is seed-dependent, and cost three long runs to attribute

Freezing InTAS 21600–28800 died three times with no Python traceback: SIGSEGV at sim time
**23544.1** with `--time-to-teleport -1`, at **23904.7** with `300`, and at 23904.7 again from the
plain `sumo.exe` (`0xC0000005`) with neither `libsumo` nor `--ignore-route-errors` in play. It is
none of those: **it is the SUMO seed.** The same window at seed 42 completes. If a long freeze dies
without a traceback, re-seed before changing anything else.

plus `road_network="sumo"`. `manifest["mobility"]` (emitted only when the mode is on) carries the
provider, the SUMO version/seed, the network by content hash, and the measured coherence — the
dvc.lock to `config`'s dvc.yaml. **No new config field was added for the geometry fix**: a correct
import is not a mode. What the manifest gained is evidence, under `mobility.network`:

```json
"net_junctions": 3332, "n_nodes": 3289, "strong_component_nodes": 3289,
"nodes_dropped_not_strongly_connected": 39, "parallel_road_keys": 35,
"road_surface": {"graph_segments": 12101, "surface_polylines": 7941,
                 "surface_segments": 23705, "surface_vertices": 31646,
                 "junction_discs": 3332, "junction_radius_max_m": 70.47, "cell_m": 100.0}
```

and `mobility.coherence.measured_against` (`"road_surface"` / `"routing_graph"`), so a reader can
tell which geometry the reported percentiles were taken against.

## Usage

```
python -m scms_sim_ref.mock_pipeline.sumo_trace \
    --net map.net.xml --routes map.rou.xml --steps 300 --dt 1 --run-seed 42 --out map.trace

python -m scms_sim_ref.mock_pipeline.run --flow --duration 300 --seed 42 \
    --road sumo --sumo-net map.net.xml \
    --mobility-source sumo_replay --sumo-trace map.trace --sumo-trace-sha256 <hex> \
    --attacker-pct 0.15 --out datasets/sumo_run
```

**The whole city, with its buildings** — the first-class full-scene path
(`docs/realism/FULL-CITY-SCENE.md`). 66 km², 3289 junctions, 21,717 footprints, InTAS's own demand:

```
$S = "scms-sim/scenarios/gen_intas_urban_low/sumo"
python -m scms_sim_ref.mock_pipeline.run --flow --duration 300 --seed 42 \
    --road sumo --sumo-net $S/ingolstadt.net.xml --sumo-buildings $S/buildings.poly.xml \
    --custom-network-directed \
    --mobility-source sumo_replay --sumo-trace intas_full_300s_dt1.trace \
    --radio-model geometric --radio-env urban --radio-range 500 --radio-cap-max-mult 1.4 \
    --radio-tx-power-dbm 23 --radio-rx-sensitivity-dbm -81 \
    --attacker-pct 0.15 --out datasets/full_city

[sumo scene] buildings.poly.xml -> 21717 footprints (110431 vertices), median centroid offset
             [237.58, -199.04] m from the median junction, 0.0027 of junctions inside a footprint
[geometric radio] env=urban tx=23.0 dBm sens=-81.0 dBm cap=700 m sense=700 m buildings=21717
```

31 s, 104 ms/step, 261 MB peak — cheaper than the 2 km² OSM extract, which costs 426 ms/step and
704 MB because it packs a bigger fleet into 1/33 of the area.

The real-city run, verbatim (InTAS AM peak, 25200–25500 s, `InTAS_buildings.sumocfg`):

```
python -m scms_sim_ref.mock_pipeline.run --flow --duration 300 --seed 42 \
    --road sumo --sumo-net C:/Temp/smob/ingolstadt.net.xml \
    --mobility-source sumo_replay --sumo-trace C:/Temp/smob/intas.trace \
    --sumo-trace-sha256 95a946f4187ca4262d0a76c95aa6f6696c273ffe1779fffaff1bb41650f598c2 \
    --attacker-pct 0.15 --out datasets/sumo_run

[sumo net] ingolstadt.net.xml -> 3289 junctions (2045 deg>=3), 7891 directed edges, oneway 0.1097,
           109 signalised junctions, 4344 graph edges (35 node-pairs carry >1 physical road)
           | surface 23705 segments + 3332 junction discs
[sumo replay] 1188 frozen trajectories, 158767 vehicle-steps, sumo_seed=257318856 (SUMO 1.25.0),
              teleports=0 | dist_to_road p50=0.363 p95=3.197 max=4.803 m | trace 95a946f4187ca426
vehicles=1188 reports=14504 investigations=155 revoked=155
data_digest=c4a7cddeb4ef58dd254ad051186911ebfd2c0e143b84115ac8f8d967eb082491
detection: precision=0.955 recall=0.822 attackers=180 revoked=155 latency_med=6.0s
```

## Toolchain traps honoured

* No SUMO scenario file may live under a path containing `--` (SUMO embeds its command line in an
  XML comment; `--` is illegal there → a silently empty routes file). `freeze()` refuses such a path,
  and it refuses **before** loading `libsumo`, so the refusal does not depend on an optional heavy
  dependency being importable.
* `randomTrips.py --validate` clobbers `-o`; not used.
* `netgenerate` needs `-j traffic_light` — `--tls.guess` signalises *nothing* on a demand-free grid.
* Generated scenario directories are regenerated by other tooling; the test suite builds its own.

### NEW, found here: `libsumo` and `pyarrow` fight over native DLLs, first import wins

Measured on this toolchain (Windows, SUMO 1.25.0, pandas 3.0.5):

| order | result |
|---|---|
| `pandas` → `libsumo` | `ImportError: DLL load failed while importing _libsumo` |
| `libsumo` → `pandas` | both import, libsumo runs — but pandas' **parquet** engine dies with `Unable to find a usable engine` |

There is no import order that satisfies both, so the fix is process separation, not ordering.
`sumo_trace.py` imports `libsumo` **lazily inside `freeze()`** and nothing else in the module needs
it — `load()`, `SumoReplayMobility` and `engine_network()` run on `sumolib`, which is pure Python.
**Anyone freezing a trajectory from a pandas-using process must shell out**
(`python -m scms_sim_ref.mock_pipeline.sumo_trace ...`).

This was nearly a silent failure of the worst kind. The natural skip guard for these tests is
`import libsumo`; in a full-suite run an earlier module has already pulled pandas in through
`datagen/featurize.py`, so **every SUMO test would have skipped while the suite stayed green** and
running the one file alone passed. `tests/test_sumo_mobility.py` therefore guards on `sumolib` only
and freezes through the CLI in a fresh interpreter.

## Not done (deliberate)

* SUMO's **traffic-light programs** are not imported into the engine's `node_phase` model. Under
  replay it does not matter (the signals are already baked into the frozen trajectory and
  `car_follow` is off), but a *non-replay* `road_network="sumo"` run still uses the engine's
  2-colouring.
* A live in-loop TraCI mode stays a documented follow-up (`PHASE3-PROBE.md`); it needs the global
  RNG stream to be split first.
* `buildings` are not extracted from a `.net.xml` (netconvert discards them); a geometric-channel
  run on a SUMO net still needs `osm.py --buildings` in the same frame via `sumo_frame_city`.
* **The GRAPH still holds one geometry per node pair.** `_canonicalise_shapes` still substitutes a
  shape on 2,165 of 7,941 InTAS records, and `netimport`'s CLI still writes the plain triple form.
  That no longer affects `dist_to_road` — the surface layer carries the real per-carriageway
  polylines — but it does still decide where an INTERNAL (non-replay) vehicle drives on a SUMO net.
  Fixing it properly means letting `CustomNetwork` hold parallel edges, which is a topology change,
  not a geometry one.
* **The surface is not in the on-disk document.** `osm.network_document` does not carry
  `road_surface`, so it exists only on the in-process import path (`sumo_trace.engine_network` →
  `run_pipeline`). A `--out net.json` document reloaded later gets the graph and not the map.
