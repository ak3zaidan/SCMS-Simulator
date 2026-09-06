# The whole city, with its buildings — closing the scene term

`ROADMAP-PERFECT.md` P1 names one number as the largest realism term measured anywhere in this
project: **84.8% of the two engines' 10.3× radio divergence is the SCENE.** The Python engine ran a
2.01 km² RDP-simplified OSM extract of Ingolstadt's old town; the MOSAIC path ran the real 65.96 km²
InTAS city. Correcting the propagation physics — which was worth doing — moved 2.8%.

This document makes the Python engine run the whole city, brings the buildings with it in the same
projection frame, and measures what that buys.

**Headline.** On the full city with InTAS's own 21,717 building footprints and InTAS's own traffic,
the Python engine delivers **0.6026** of packets at 200 m where the same engine on its 2 km² extract
delivers **0.0754** and the MOSAIC radio delivers 0.7798. (Per-packet and *propagation-only* — no
congestion, no collision, no weather — which is the quantity `CROSS-ENGINE-RADIO.md` compares
throughout, because the reference simulation models no interference and is explicitly an upper
bound.) In the log-ratio decomposition
`CROSS-ENGINE-RADIO.md` §1 uses, that is **the whole scene term** — 104.9% of it as published,
100.7% of it once that step is re-measured at matched raster resolution (§6) — and **89.0% of the
whole divergence**. The residual factor against MOSAIC falls from **10.3× to 1.29×**. The
0.90-awareness-equivalent range moves 103.5 m → **338.5 m** against MOSAIC's 504.6 m, and the
unmodified `awareness.py` pointed at MOSAIC's *own* scene and emissions reads 0.5944 / 335.1 m — so
the native whole-city run agrees with that independent instrument to **1.4% and 1.0%**.

**And it is cheaper, not dearer.** The 66 km² city with 333 replayed InTAS vehicles runs at
**104 ms/step and 261 MB peak**; the 2 km² extract with 612 vehicles runs at **426 ms/step and
704 MB peak**. Cost is driven by vehicle *density* — the number of in-range pairs per step — not by
map size, and the whole city is 33× the area at half the fleet.

---

## 1. What was actually missing

The graph was never the problem. `netimport.py` reads a full `.net.xml` cleanly and InTAS imports
3332 junctions / 7941 edges against `roads.CustomNetwork`'s caps of 4000 / 12000. Three things were
missing, and only the second is interesting:

1. **A documented path.** `road_network="sumo"` + `sumo_net=<the .net.xml>` already worked; nothing
   said "this is how you run the whole city", and no measurement had been taken on it.
2. **The buildings.** `road_network="sumo"` loaded **no footprints at all**. `run.py` read the
   `buildings` layer of a *custom-network document* and nothing else, and a SUMO scenario ships its
   footprints as a separate polygon additional-file. So the largest scene in the project would have
   run with roads and no buildings, silently falling back to the synthetic urban-canyon density —
   which is precisely the comparison this whole exercise exists to stop making. Measured below: that
   fallback is worth a factor of **1.60× in PDR at 200 m** in the wrong direction.
3. **A raster big enough to hold it.** `_BuildingRaster` doubles its cell size until the grid fits
   `GEO_BUILDING_MAX_CELLS`, silently, and the whole city did not fit. §6.

### The projection trap, restated because this is where it bites

`osm.py` derives its local equirectangular frame from **road ways only** (`lat0/lon0` = min lat/lon
over those ways). Anything projected into that frame with a re-derived origin — or with a SUMO net's
own UTM offset left in — lands hundreds of metres to kilometres away **while every individual
polygon still looks exactly like a building and every street still looks exactly like a street**.
The geometric channel then computes NLOSb against a city translated off itself, and the resulting
dataset is plausible and wrong.

`netimport.scene_from_net(net_path, poly_path)` is built so that cannot happen: it **reads the net
itself** and builds the transform with the same `_transformer` call `net_to_network` makes, rather
than accepting a transform from the caller. It costs one extra `sumolib` read (0.46 s on InTAS's
16.9 MB net). That is deliberate — the alternative, re-deriving the transform from the `<location>`
element alone, would be a second implementation of the one thing that must not diverge.

## 2. What changed

| file | change |
|---|---|
| `mock_pipeline/netimport.py` | `_poly_rings`, `buildings_from_poly`, `scene_from_net`, `_assert_buildings_aligned`; `--buildings` on the CLI |
| `mock_pipeline/sumo_trace.py` | `frame_for_city()` — ONE definition of the projection tuple, used by `engine_network` for the roads and by the caller for the footprints; `--sumo-arg` on the freeze CLI, without which freezing a scenario's own `.sumocfg` overwrites its committed summary/tripinfo/log files in place |
| `mock_pipeline/run.py` | `sumo_buildings` config field + `--sumo-buildings`; the footprint load in the channel-construction block; `counts.scene_buildings` provenance; `GEO_BUILDING_MAX_CELLS` 6 M → 32 M |
| `datagen/awareness.py` | `load_scenario` re-imports a sumo-path scene through the same `scene_from_net` call the run made |
| `tests/test_full_city_scene.py` | new: the gate, the knob, the raster ceiling, the awareness read-back, the pinned golden |

The footprint layer is **lossless by default** — no minimum area, no RDP simplification, no polygon
cap, and the `type="building"` filter is exactly the selection `org.scms.radio.BuildingIndex.parse`
makes on the Java side. A cross-engine comparison of link-state composition is only a comparison if
both engines hold the same footprints. (`osm.extract_buildings` simplifies at 1 m and caps at 6000
because it is rebuilding rings from raw OSM node refs; here the polygons arrive already resolved.)

## 3. The registration gate, and what each arm can catch

Three arms, all reported whether or not they fire, all thresholds measured by translating the real
21,717-footprint InTAS layer and bisecting.

| arm | statistic | InTAS measures | threshold | headroom |
|---|---|---|---|---|
| 1 | median building centroid vs the **median junction**, per axis | 237.6 m / 199.0 m | 0.10 × road extent = 1358 m / 1109 m | 5.7× / 5.6× |
| 2 | fraction of centroids inside the road bbox | 1.0000 | ≥ 0.60 | 0.40 absolute |
| 3 | fraction of **road junctions inside a footprint** | 0.0027 (9 of 3332) | ≤ 0.05 | 18× |

Arm 1 deliberately anchors on the median junction rather than the road bbox centre: netconvert keeps
motorway stubs far outside the built-up area, so InTAS's road bbox is 13.58 × 11.09 km around an
8.31 × 8.01 km city and its centre sits 1.9 km east of the buildings. A bbox-centre test would fail
a **correct** import.

Arm 3 is the sharp one and the only one that is a statement about *registration* rather than about
*extent*. A city's junctions are on tarmac:

| displacement of the footprint layer | **arm 3**: junctions inside a footprint | context (not gated): centroids within 100 m of a junction |
|---|---|---|
| 0 m | **0.0027** | 0.9151 |
| 10 m | 0.0441 | 0.9141 |
| 25 m | 0.1327 | 0.9131 |
| 100 m | 0.1645 | 0.8949 |
| 400 m | 0.1582 | 0.7754 |
| 1600 m | 0.1185 | 0.5358 |
| 3200 m | 0.0753 | 0.2743 |
| 6400 m | 0.0129 | 0.0573 |

Note the last two rows: a translation large enough to carry the footprints off the city **stops**
putting junctions in walls (the second column shows why — by 3.2 km most footprints are over open
country), so arm 3 alone would let it through. That is what arm 1 is for, and why the two are chosen
as a pair rather than each on its own merits. Bisected on the real layer:

| translation direction | smallest displacement CAUGHT, with junctions | without junctions |
|---|---|---|
| +x | **10.6 m** | 8169.6 m |
| −x | 12.6 m | 4368.5 m |
| +y | 11.1 m | 5868.7 m |
| −y | 12.2 m | 3859.7 m |
| diagonal | 10.6 m | 8210.8 m |

Swept in 100 m steps from 0 to 20 km along +x and along the diagonal, **no displacement passes**.
Without a junction cloud the gate degrades to bbox containment, which tolerates 3.9–8.2 km; that is
stated in the code and in the returned `alignment_anchor_is`, and `scene_from_net` always supplies
junctions.

### The registration defect of `CROSS-ENGINE-RADIO.md` §5, re-measured

That document measured the Python engine's OSM extract running through its own buildings 4.6× more
than InTAS by road length, and its vehicles standing inside walls **48×** more often — a defect it
attributed to the 10 m RDP simplification. Both scenes re-measured here through the Python engine's
own layers, sampling every metre, with each instrument:

| sampled every 1 m | 2 km² OSM extract | **full city (this work)** | ratio |
|---|---|---|---|
| road length on an occupied **3 m raster** cell | **10.3521%** | **1.9994%** | 5.2× better |
| vehicle emissions on an occupied 3 m raster cell | **9.6160%** | **0.0917%** | 105× better |
| road length inside a footprint, **exact** (`FootprintIndex`, no halo) | 3.6578% | 0.3291% | 11.1× better |
| vehicle emissions inside a footprint, exact | 4.3742% | 0.0217% | 202× better |

The raster rows reproduce `CROSS-ENGINE-RADIO.md` §5's extract figures **to the digit** (10.35% and
9.62%), which is the control that says this is the same measurement. The exact rows are given
alongside because a 3 m raster puts a one-cell halo on every wall and therefore over-reports both
sides; the ratio is what the defect is. The InTAS column is *not* a re-run of that document's InTAS
figures (2.26% / 0.20%): those were taken over MOSAIC's own lane segments and emissions, where these
are the Python engine's imported road surface and its own replayed positions. They land in the same
place (2.00% / 0.09%), which is reassuring, but they are not the same population.

So the whole-city path does not merely *dilute* the registration defect — it removes it. A vehicle
in arm A stands inside a wall 0.09% of the time against the extract's 9.62%, which is what
`GEO_ENDPOINT_CLEAR_M = 6 m` was introduced to paper over. (That clearance is still applied; it is
now doing almost nothing, and `CROSS-ENGINE-RADIO.md` already measured it as worth 0.0001 of the
classification.)

## 4. What the whole city buys

Four arms. Every one starts from the flagship's own config
(`datasets/phase2_geometric/urban_osm_geometric`) — same seed 42, same radio (geometric, 23 dBm,
−81 dBm, 104 dB budget), same 21-attack catalogue, same detectors, same `dt = 1.0`, same 300 s — and
changes only the scene and the mobility. Arm C reproduces the committed flagship dataset's
`data_digest` **byte for byte** (`58dab5096b47f8b2…`), which is the control that says the harness and
the code change did not move the thing being compared against.

| arm | scene | mobility | vehicles |
|---|---|---|---|
| **A** | full 66 km² InTAS net + its 21,717 footprints | SUMO replay of InTAS demand, 300 s from t = 0 | 333 |
| **B** | full 66 km² InTAS net, **no footprints** (canyon λ = 4/km) | the same SUMO replay | 333 |
| **C** | the 2.01 km² RDP-simplified OSM extract + its 1519 footprints | the engine's internal routed IDM | 612 |
| **D** | full 66 km² InTAS net + its 21,717 footprints | the engine's internal routed IDM | 628 |

Arm A's frozen trajectory is InTAS's own `InTAS_buildings.sumocfg` (EIDM, sublane 0.8,
`time-to-teleport 300`, rerouting) sub-stepped 10× so SUMO still integrates at its calibrated 0.1 s
while the artifact samples at the engine's 1 s: **334 vehicles inserted, 78,329 vehicle-steps,
0 teleports, 0 collisions** — the same 334-vehicle window the six MOSAIC arms in
`CROSS-ENGINE-RADIO.md` ran.

### Headline, per arm

| | A full city + buildings | B full city, no buildings | C 2 km² extract | D full city, engine's own traffic |
|---|---|---|---|---|
| footprints | 21,717 | 0 (canyon) | 1,519 | 21,717 |
| LOS at 200 m | **0.4500** | 0.3091 | **0.0598** | 0.3060 |
| NLOSv at 200 m | 0.3028 | 0.1444 | 0.0311 | 0.0383 |
| NLOSb at 200 m | **0.2471** | 0.5466 | **0.9091** | 0.6557 |
| **PDR at 200 m** | **0.6026** | 0.3768 | **0.0754** | 0.3097 |
| PDR 0.90 / 0.50 / 0.20 crossing (m) | 83.1 / 241.3 / 508.1 | 45.3 / 147.6 / 328.3 | 35.2 / 79.7 / 126.4 | 52.4 / 131.8 / 309.9 |
| **NAR-0.90-equivalent range** | **338.5 m** | 216.6 m | **103.5 m** | 183.4 m |
| ratio vs the Boban & d'Orey urban curve at 104 dB (87.06 m) | 3.89× | 2.49× | 1.19× | 2.11× |
| gray-zone ratio d20/d90 | 6.11 | 7.25 | 3.59 | 5.91 |

### Link-state composition per distance band (fraction of co-present pairs, LOS / NLOSv / NLOSb)

| band | **A: full city + buildings** | C: 2 km² extract | D: full city, engine traffic | MOSAIC/Java on InTAS (transmission-weighted) |
|---|---|---|---|---|
| 0–50 m | 0.734 / 0.260 / 0.005 | 0.620 / 0.159 / 0.222 | 0.909 / 0.038 / 0.053 | 0.9971 / — / 0.0029 |
| 50–100 m | 0.510 / 0.441 / 0.049 | 0.277 / 0.171 / 0.552 | 0.716 / 0.073 / 0.210 | 0.9743 / — / 0.0257 |
| 100–150 m | 0.523 / 0.354 / 0.123 | 0.131 / 0.080 / 0.790 | 0.474 / 0.058 / 0.468 | 0.8992 / — / 0.1008 |
| 150–200 m | 0.481 / 0.319 / 0.201 | 0.075 / 0.039 / 0.886 | 0.352 / 0.039 / 0.608 | 0.8172 / — / 0.1828 |
| **200–250 m** | **0.417 / 0.286 / 0.298** | **0.046 / 0.024 / 0.930** | 0.266 / 0.037 / 0.697 | **0.7409 / — / 0.2591** |
| 250–300 m | 0.332 / 0.254 / 0.413 | 0.034 / 0.011 / 0.955 | 0.249 / 0.034 / 0.716 | 0.6370 / — / 0.3630 |
| 300–350 m | 0.309 / 0.227 / 0.464 | 0.025 / 0.006 / 0.968 | 0.198 / 0.030 / 0.772 | 0.5782 / — / 0.4218 |
| 450–500 m | 0.247 / 0.142 / 0.611 | 0.014 / 0.002 / 0.985 | 0.124 / 0.016 / 0.861 | 0.3783 / — / 0.6217 |
| 650–700 m | 0.151 / 0.075 / 0.774 | 0.004 / 0.000 / 0.996 | 0.059 / 0.007 / 0.934 | — |

The Java column has no NLOSv branch at all (`CROSS-ENGINE-RADIO.md` §4.2), so its LOS share absorbs
every vehicle-blocked link; the Python engine's LOS+NLOSv at 200–250 m is **0.703** against Java's
0.741, and its NLOSb 0.298 against Java's 0.259. **That is the like-for-like comparison, and the two
scenes now agree to within 3.9 pp of NLOSb** where the extract and InTAS were 67.1 pp apart
(0.930 vs 0.259) under the same classifier.

### PDR versus distance (per-packet, propagation only — no congestion, as in the reference)

| band centre (m) | **A** | B | C | D |
|---|---|---|---|---|
| 25 | 0.9994 | 0.9973 | 0.9945 | 0.9987 |
| 75 | 0.9188 | 0.7571 | 0.5307 | 0.8189 |
| 125 | 0.8029 | 0.5650 | 0.2030 | 0.5219 |
| 175 | 0.6594 | 0.4209 | 0.0973 | 0.3600 |
| 225 | 0.5405 | 0.3288 | 0.0557 | 0.2658 |
| 275 | 0.4164 | 0.2574 | 0.0353 | 0.2394 |
| 325 | 0.3591 | 0.2028 | 0.0245 | 0.1830 |
| 375 | 0.3040 | 0.1614 | 0.0179 | 0.1401 |
| 425 | 0.2619 | 0.1276 | 0.0125 | 0.1161 |
| 475 | 0.2272 | 0.0995 | 0.0112 | 0.0993 |
| 525 | 0.1862 | 0.0797 | 0.0110 | 0.0777 |
| 575 | 0.1462 | 0.0630 | 0.0096 | 0.0509 |
| 625 | 0.1434 | 0.0483 | 0.0043 | 0.0427 |
| 675 | 0.1048 | 0.0377 | 0.0027 | 0.0382 |

## 5. How much of the 84.8% actually closes

`CROSS-ENGINE-RADIO.md` computes its split from PDR at 200 m as a log-ratio:
`ln(0.7798/0.0754) = 2.3362` total, of which the scene step
(`ln(0.5464/0.0754) = 1.9805`) is **84.8%**, the classifier 12.4% and the physics 2.8%.

| | PDR @ 200 m | moved, in log-ratio | of the SCENE term as published | of the whole divergence | residual factor vs MOSAIC |
|---|---|---|---|---|---|
| C — the 2 km² extract (where we were) | 0.0754 | — | 0% | 0% | **10.34×** |
| D — the full city, engine's own traffic | 0.3097 | 1.4128 | **71.3%** | 60.5% | 2.52× |
| **A — the full city, InTAS traffic** | **0.6026** | **2.0784** | **104.9%** | **89.0%** | **1.29×** |

**The scene term is closed.** The apparent >100% is an artefact of the reference value, not an
overshoot: the 0.5464 that step was measured at came off a **silently coarsened 6 m raster** (§6).
Re-run at 3 m, `datasets/xengine/_shim_geo_p23` — the same shim, the same MOSAIC scene and
emissions, the unmodified `awareness.py` — reads **0.5944** at 200 m and a **335.1 m** range. So:

| | PDR @ 200 m | NAR-0.90 range |
|---|---|---|
| the Python instrument on MOSAIC's own scene, re-measured at 3 m | 0.5944 | 335.1 m |
| **the Python engine running the whole city natively (arm A)** | **0.6026** | **338.5 m** |
| agreement | **1.4%** | **1.0%** |

That is the strongest single result here, and it is an independent cross-check rather than a
restatement: a native Python run of the whole city lands within 1.4% of the same instrument pointed
at MOSAIC's own scene and emissions. Against the corrected split (scene 2.0647 = **88.4%** of the
2.3362 total, classifier 8.8%, physics 2.8%), arm A closes **100.7% of the scene term** — i.e. all
of it, to measurement noise.

Split the two halves of "the scene":

* **The map is 71.3% of it** (C → D: same engine mobility, same everything, 2 km² of RDP-simplified
  OSM core replaced by 66 km² of real SUMO lane network with 21,717 real footprints).
* **Where the traffic is, is the other 33.6%** (D → A: the engine's own uniform-OD trip generator
  replaced by InTAS's calibrated demand). The mechanism is visible in the composition: NLOSv at
  200 m goes 0.038 → 0.303 because InTAS traffic concentrates on arterials in platoons, where the
  engine's own generator spreads 628 vehicles thinly over 66 km². Arm A classifies 38,584 pairs at
  200 m from 333 vehicles; arm D classifies 21,193 from 628.

(Both figures are fractions of the *published* scene term; against the corrected one they are 68.4%
and 32.2%. The two split the achieved move 68 / 32 either way.)

Read in awareness range instead of PDR, the same four arms give a consistent but less flattering
picture, because range is a much flatter function of composition:

| arm | NAR-0.90-equivalent range | closure of the C → MOSAIC range gap |
|---|---|---|
| C | 103.5 m | 0% |
| D | 183.4 m | 36.1% |
| B | 216.6 m | 46.6% |
| **A** | **338.5 m** | **74.8%** |
| MOSAIC measured | 504.6 m | 100% |

**What is left is what the decomposition said would be left**: the classifier (12.4% as published,
**8.8%** corrected — the Python raster over-blocks by ~1.4–8.7 pp per band because a wall is at
least one cell thick, and it applies an NLOSv branch the Java side does not have at all) and the
physics (2.8% — the Java side has no Nakagami fade). Neither is a scene problem, neither is
addressed here, and together they are 11.6% against the 11.0% of residual actually measured
(`ln(0.7798/0.6026) = 0.2578` of 2.3362).

### Buildings are not optional, and the direction matters

Arm B is the same whole city with the footprints removed, falling back to the synthetic urban-canyon
blockage `P(NLOSb) = 1 − exp(−λd)` at the shipped λ = 4/km. It reads PDR 0.3768 at 200 m against
arm A's 0.6026 — **1.60× pessimistic**, and 216.6 m of range against 338.5 m. So a whole-city run
without its footprints would have looked like a 46.6% closure of the gap while actually measuring a
scene that does not exist. That is the failure this document's title clause exists to prevent.

## 6. The 6 m finding — and a correction to `CROSS-ENGINE-RADIO.md`

`_BuildingRaster` auto-coarsens: it doubles the cell size until the grid fits
`GEO_BUILDING_MAX_CELLS`, and says nothing. The 2.01 km² extract needs 0.22 M cells at 3 m. The
66 km² city needs **7.41 M**, against an old ceiling of 6 M — so every full-city classification would
have been done at **6 m, half the extract's resolution**, and the two scenes would not have been
comparable. One cell is one byte, so the ceiling is now 32 M = 32 MB; InTAS at 3 m allocates 7.41 MB.

Measured on arm A's own emissions, same code, only the cell size changed:

| | 3 m (now) | 6 m (the old ceiling would have forced this) |
|---|---|---|
| grid | 2773 × 2674 = 7,415,002 cells | 1388 × 1338 = 1,857,144 |
| occupied fraction | 0.1430 | 0.1802 |
| LOS / NLOSv / NLOSb at 200 m | 0.4500 / 0.3028 / **0.2471** | 0.4264 / 0.2566 / **0.3170** |
| PDR at 200 m | **0.6026** | 0.5532 |
| NAR-0.90-equivalent range | **338.5 m** | 304.7 m |
| classification time (400 k pairs) | 10.0 s | 7.2 s |

**`CROSS-ENGINE-RADIO.md` §6's fourth column — "Python physics and classifier, MOSAIC scene",
0.5464 PDR at 200 m and 306.8 m of range — reproduces the 6 m row to 1.3% and 0.7%.** That column
was measured through `tools/xengine_radio.py`'s shim, which builds the same `_BuildingRaster` over
the same 21,717 InTAS footprints and therefore hit the same silent coarsening. It is not wrong about
what it measured; it was measuring a half-resolution classifier against a full-resolution one.

Re-running that shim dataset (`datasets/xengine/_shim_geo_p23`) unchanged, with the ceiling raised:

| the shim, MOSAIC scene + MOSAIC emissions | as published (6 m) | re-measured (3 m) |
|---|---|---|
| LOS / NLOSv / NLOSb at 200 m | — | 0.4557 / 0.2800 / 0.2643 |
| PDR at 200 m | 0.5464 | **0.5944** |
| NAR-0.90-equivalent range | 306.8 m | **335.1 m** |
| ratio vs the reference curve | 3.52× | 3.85× |

so the corrected decomposition is **scene 88.4% / classifier 8.8% / physics 2.8%**, and the scene
was slightly *under*-stated at 84.8%.

## 7. What it costs

All timings are wall clock on this machine (Windows Server 2022, 8 cores / 16 logical processors,
61.6 GB RAM, CPython 3.12.10, `PYTHONHASHSEED=0`). Every run here is **single-threaded**, one at a
time; a second unrelated single-threaded workstream was active on the box during part of this
measurement, which on 16 logical processors is not expected to move a single-threaded figure and is
recorded here rather than assumed away. "Peak RSS" is the process peak working set over the whole
run including the import.

### One-off import cost, whole city

| step | wall | note |
|---|---|---|
| `sumolib` read of `ingolstadt.net.xml` (16.9 MB) | 0.46 s | |
| `net_to_network` (strong component, shapes, road surface) | 0.33 s | 3289 junctions, 4344 undirected edges, 7891 directed, 23,705 surface segments |
| `scene_from_net` (a second net read + 5.7 MB `buildings.poly.xml` + the gate) | 0.92 s | 21,717 footprints, 110,431 vertices |
| `_BuildingRaster` at 3 m over 21,717 footprints | 0.46 s | 7.41 M cells = 7.41 MB |
| **total added by the whole-city scene** | **~1.7 s** | once per run |

### Per-run cost, the four arms

| arm | map | vehicles | steps | run wall | ms/step | peak RSS | reports |
|---|---|---|---|---|---|---|---|
| **A** full city + buildings, replay | 66 km² | 333 | 300 | **31.3 s** | **104** | **261 MB** | 3222 |
| B full city, no buildings, replay | 66 km² | 333 | 300 | 23.9 s | 80 | 221 MB | 1761 |
| **C** 2 km² extract (the flagship) | 2 km² | 612 | 300 | **127.8 s** | **426** | **704 MB** | 4481 |
| D full city + buildings, engine traffic | 66 km² | 628 | 300 | 39.0 s | 130 | 397 MB | 2589 |

**The full city is 4.1× FASTER than the 2 km² extract and uses 2.7× less memory.** That is not a
paradox: the engine's per-step cost is dominated by candidate transmit/receive pairs, which scale
with vehicle *density*, and 612 vehicles inside a 1.6 × 1.5 km box are far denser than 333 spread
over 9.1 × 8.4 km. The footprint layer itself costs 24 ms/step (A vs B) — the ray march is only run
for pairs that are already in range.

### The scaling, measured

`vehicles` is the total spawned over the run; **mean concurrent** is
`vehicle_steps_simulated / steps`, which is what the per-step cost actually sees. "veh/km²" divides
that by the map's road bbox (2.43 km² for the extract's vehicle extent, 75.9 km² for InTAS).

| run | map | mean concurrent | veh/km² | steps | wall | ms/step | **ms per vehicle-step** | peak RSS |
|---|---|---|---|---|---|---|---|---|
| B full city, replay, no buildings | 66 km² | 261.1 | 3.4 | 300 | 23.9 s | 80 | **0.306** | 221 MB |
| **A full city, replay + buildings** | 66 km² | 261.1 | 3.4 | 300 | 31.3 s | 104 | **0.399** | 261 MB |
| D full city, engine traffic | 66 km² | 285.7 | 3.8 | 300 | 39.0 s | 130 | **0.455** | 397 MB |
| A′ full city, replay, **1200 s** | 66 km² | 306.2 | 4.0 | 1200 | 169.6 s | 141 | **0.462** | 569 MB |
| E full city, engine traffic ×3 | 66 km² | 853.5 | 11.2 | 300 | 369.4 s | 1231 | **1.443** | 1598 MB |
| **C the 2 km² extract** | 2 km² | 189.9 | **78.1** | 300 | 127.8 s | 426 | **2.243** | 704 MB |

Read the last two columns together. **Arm C has the FEWEST concurrent vehicles of any run here and
the highest cost per vehicle-step, by 5.6× over arm A.** The per-step work is dominated by candidate
transmitter/receiver pairs, and at 78 veh/km² inside a 1.6 × 1.5 km box essentially every vehicle is
inside every other vehicle's 700 m candidate window, where at 3.4 veh/km² across 9.1 × 8.4 km only a
handful are. Map size costs a one-off 1.7 s of import; the footprint layer costs 40 MB of resident
memory and 24 ms/step (A vs B, both the same 261.1 concurrent vehicles). Neither scales with area.

Freezing the trajectory is a separate one-off and is SUMO's cost, not the engine's: 300 s of InTAS
takes **12.7 s** (real-time factor 26.7) and 1200 s takes **74.0 s**, single-threaded, on the
scenario's own calibrated 0.1 s step.

### So when does it become slow?

**Not because of the map.** On these measurements the answer is a *density*, and it is the same
number whatever the map is under it:

* **Up to ~300 concurrent vehicles** (arms A, B, D, A′ — the whole city at InTAS's own demand, and a
  20-minute window of it) the engine runs at **80–141 ms/step and under 600 MB**. A 300 s dataset
  takes half a minute, a 1200 s one takes under three. This is routine use, and the whole city is
  the cheap option.
* **At ~850 concurrent** (arm E, InTAS's map loaded 3× over) it is **1.23 s/step and 1.6 GB** — a
  300-step run is 6 minutes and a 3600-step one would be about 75. That is the point where a
  parameter sweep stops being interactive.
* Extrapolating the last two points, cost per vehicle-step rises roughly with the neighbour count
  (0.40 ms at 5.3 expected neighbours inside the 700 m window, 1.44 ms at 17.3), so the per-step
  term is close to quadratic in density and roughly **N × density**. Doubling density from arm E
  costs about 4× again.

Two practical consequences. First, if a run is slow, the lever is `radio_range_m` /
`radio_cap_max_mult` (which set the candidate window) or the fleet size — not the map. Second, **the
2 km² extract is not a cheaper option and never was**: it is the most expensive configuration
measured here per unit of simulated vehicle time, because its whole point was to pack a fleet into a
small box.

## 8. Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1        # SUMO 1.25.0
cd C:\Users\Administrator\Documents\SCMS-Simulator
$env:PYTHONPATH = "$PWD\src"
$S = "scms-sim/scenarios/gen_intas_urban_low/sumo"

# 1. the whole city as a custom-network document, footprints included (gate runs here)
python -m scms_sim_ref.mock_pipeline.netimport --net $S/ingolstadt.net.xml --strong --no-geo `
       --buildings $S/buildings.poly.xml --out intas_scene.json --stats

# 2. InTAS's own 300 s window, frozen at the engine's dt while SUMO keeps its calibrated 0.1 s step.
#    --output-prefix keeps SUMO's own summary/tripinfo/log outputs off the scenario's committed
#    measurement files, which the sumocfg would otherwise overwrite in place. 12.7 s.
python -m scms_sim_ref.mock_pipeline.sumo_trace --net $S/ingolstadt.net.xml `
       --sumocfg $S/InTAS_buildings.sumocfg --out intas_full_300s_dt1.trace `
       --run-seed 42 --steps 300 --dt 1.0 --substeps 10 --time-to-teleport 300 --split-on-gap `
       --sumo-arg=--output-prefix --sumo-arg=p1scene_
# -> 333 trajectories, 78329 vehicle-steps, 0 teleports, 0 collisions
#    sumo_trace_sha256 = a8f63743ec5b8501591811cd08cff3144ecfb90eeb0783434ccffb2e20a3e184

# 3. the run: the whole city, its buildings, its traffic.
#    --radio-cap-max-mult 1.4 is NOT cosmetic: it is what the flagship config uses, and the CLI
#    default of 6.0 would open a 3000 m candidate window instead of 700 m -- a different amount of
#    work per step and a different set of classified pairs.
python -m scms_sim_ref.mock_pipeline.run --out ds_full_city --seed 42 --flow --duration 300 `
       --road sumo --sumo-net $S/ingolstadt.net.xml --sumo-buildings $S/buildings.poly.xml `
       --custom-network-directed --mobility-source sumo_replay `
       --sumo-trace intas_full_300s_dt1.trace --attacker-pct 0.15 `
       --radio-model geometric --radio-env urban --radio-range 500 --radio-cap-max-mult 1.4 `
       --radio-tx-power-dbm 23 --radio-rx-sensitivity-dbm -81
# [geometric radio] ... cap=700 m sense=700 m buildings=21717   <- check this line

# 4. measure it with the unmodified shared instrument
python -m scms_sim_ref.datagen.awareness ds_full_city --markdown
```

Arm C is `datasets/phase2_geometric/urban_osm_geometric`'s own config re-run unchanged; it must
reproduce `data_digest_sha256 = 58dab5096b47f8b25e06f48ab7ccb5d5f2885b142c8a20b0a40db555360a72cd`.

## 9. What this does NOT show

* **Nothing here validates the radio against measured radio data.** `ROADMAP-PERFECT.md` P7 is
  untouched. The comparison is against another simulation (MOSAIC/Java) and against Boban & d'Orey's
  published curve evaluated at our own link budget.
* **The remaining 11% is attributed, not measured.** The residual 1.29× against MOSAIC is consistent
  with the classifier (8.8% corrected) and physics (2.8%) terms the earlier decomposition isolated —
  11.6% predicted against 11.0% measured — but this document re-ran only the *classifier-and-scene*
  step at 3 m, not the physics-only one. Until that is redone the residual is apportioned by
  arithmetic, not by an experiment.
* **The two engines' link-state numbers are not weighted the same way.** The Java column is
  transmission-weighted (one row per reception decision the radio took) and the Python column is
  co-presence-weighted (one row per co-present pair per 1 s snapshot). They agree to 3.9 pp at
  200–250 m, which is the headline here, but that agreement is between two differently weighted
  populations and should not be read as tighter than it is.
* **Nothing here re-measures traffic.** The frozen trajectory is InTAS's own demand through its own
  `sumocfg`; `DEMAND-CALIBRATION.md`'s verdict (all four FHWA gates fail, −27.1% held-out) is
  untouched, and this window is a cold start rather than a graded peak hour in any case.
* **Arm A's traffic is a 300 s cold start.** InTAS from t = 0 holds 334 vehicles across 66 km²; that
  is the same window every MOSAIC arm used, which is what makes them comparable, but it is a low
  density and every congestion-dependent quantity (CBR, DCC, contention) is measured at it.
  `CROSS-ENGINE-RADIO.md` §7 already establishes that DCC does nothing at this density on either
  engine.
* **The gate's thresholds are calibrated on one city.** InTAS is the only SUMO scenario in this
  repository that ships building polygons, so arms 1–3 have one real calibration point each. The
  displacement bisection is what makes them defensible rather than the headroom alone.
* **Arm D's mobility is the engine's own uniform-OD generator**, which is not a claim about
  Ingolstadt. It exists only to split "the map" from "where the traffic is"; neither half of that
  split should be quoted as a property of the city.
* **The extract is not deleted.** It remains the flagship dataset and the pinned
  `AWARENESS-GATE.md` reference (103.5 m, 1.19×, LOS 0.060 at 200 m), all four of which reproduce
  exactly in arm C.
