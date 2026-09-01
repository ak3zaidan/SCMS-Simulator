# SUMO-backed mobility for the Python engine (freeze + replay)

**Status: shipped, default OFF.** `mock_pipeline/sumo_trace.py` (new) +
`mock_pipeline/run.py` (the seam) + `tests/test_sumo_mobility.py` (36 tests).

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

plus `road_network="sumo"`. `manifest["mobility"]` (emitted only when the mode is on) carries the
provider, the SUMO version/seed, the network by content hash, and the measured coherence — the
dvc.lock to `config`'s dvc.yaml.

## Usage

```
python -m scms_sim_ref.mock_pipeline.sumo_trace \
    --net map.net.xml --routes map.rou.xml --steps 300 --dt 1 --run-seed 42 --out map.trace

python -m scms_sim_ref.mock_pipeline.run --flow --duration 300 --seed 42 \
    --road sumo --sumo-net map.net.xml \
    --mobility-source sumo_replay --sumo-trace map.trace --sumo-trace-sha256 <hex> \
    --attacker-pct 0.15 --out datasets/sumo_run
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
