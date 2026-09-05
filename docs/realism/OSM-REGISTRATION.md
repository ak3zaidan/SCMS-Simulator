# The roads inside the buildings: cause, fix, and what it moved

[`CROSS-ENGINE-RADIO.md`](CROSS-ENGINE-RADIO.md) §5 recorded divergence #7 as a measured fact with
no diagnosis: **10.35% of the Python engine's own road length and 9.62% of its vehicle positions
fall inside its own building footprints**, against 2.26% and 0.20% for the InTAS scene under
identical code and the same 3 m raster. It listed the candidate causes and left them untested.

This document tests them. The answer is **RDP simplification**, and it is not a close call.

**Headline.** At the shipped 10 m simplification tolerance, plain Ramer-Douglas-Peucker chords a
curved street straight through a block. Constraining RDP so that a chord may never enter a footprint
the unsimplified way misses takes the halo-free (exact point-in-polygon) road-in-building fraction
from **3.66% to 0.174%**, the vehicle-in-building fraction from **4.374% to 0.113%**, and the raster
figure the cross-engine document quotes from **10.35% to 5.99%** — for **28 extra graph nodes**
(346 → 374, +8.1%). Everything that survives is real: 36.6 of the residual 37.6 m is OSM ways
explicitly tagged `tunnel=building_passage`. The link-state mix moves LOS **0.0598 → 0.0831** at
200 m, which is **5.7% of the composition gap to the InTAS scene** — the size the cross-engine
document predicted, now measured rather than estimated.

Instrument: [`tools/osm_registration.py`](../../tools/osm_registration.py). Fix:
`src/scms_sim_ref/mock_pipeline/osm.py`. Tests: `tests/test_osm_registration.py`.

---

## 1. Two instruments, because they answer two questions

Everything below reports the overlap twice, and the distinction is load-bearing:

* **raster** — the engine's own `run._BuildingRaster` at `GEO_BUILDING_CELL_M = 3.0`. This is the
  number `CROSS-ENGINE-RADIO.md` quotes, and it is the number the LOS classifier actually sees. It
  has a **~1-cell halo**: `_BuildingRaster` stamps every cell a footprint *edge* passes through, so
  a centreline 1.5 m from a facade already reads as "inside". In a European old town that is a lot
  of street.
* **exact** — even-odd point-in-polygon, no cells. This is the number that says whether the road is
  *really* in the building.

Both are sampled every 1 m along every edge. On the shipped map:

| | raster 3 m | raster 2 m | raster 1 m | raster 0.5 m | **exact** |
|---|---|---|---|---|---|
| shipped import | **10.352%** | 7.184% | 5.178% | 5.160% | **3.658%** |
| after the fix | **5.988%** | 2.221% | 0.625% | 0.643% | **0.174%** |

The raster column shrinking toward the exact column as the cell shrinks is the halo, and it is a
property of the classifier, not of the map. The exact column is the defect.

## 2. The cause, measured: RDP, not projection, not rounding, not the source data

### 2.1 Projection and rounding are ruled out by construction and by measurement

`extract_buildings` is already handed `osm_to_network`'s own `info["projection"]` tuple and refuses
to run without it (the alignment gate at `osm.py`). To confirm that gate is doing its job rather
than merely existing, `osm_registration.py raw` re-derives *both* layers from the raw XML in one
frame — raw road way polylines, raw closed `building=*` rings, no simplification anywhere, no
min-area filter, no bbox filter, no coordinate rounding:

```
raw drivable ways 347   raw building rings 1573
22 547 one-metre samples, 39 inside a footprint = 0.173%
```

**0.173%.** If the two layers were on different origins or different scales, this number would be
enormous, not one part in six hundred. Node coordinates are rounded to 0.1 m by `nid()` and building
vertices to 0.01 m by `extract_buildings`; 0.1 m cannot produce a 3.5 pp effect, and the tolerance
sweep below shows the built graph's residual at zero simplification (0.1697%, 37 / 21 808) matching
this raw figure (0.1730%, 39 / 22 547 — the denominators differ only because the graph keeps just
the largest connected component) to **0.003 pp**. Both hypotheses are dead.

### 2.2 The tolerance sweep, which is the experiment

Same footprints throughout; only the road graph is rebuilt.

| RDP tol | nodes | edges | raster 3 m | **exact** | | constrained nodes | constrained raster | **constrained exact** |
|---|---|---|---|---|---|---|---|---|
| **10.0 m (shipped)** | 346 | 416 | 10.35% | **3.66%** | | **374** | **5.99%** | **0.17%** |
| 7.5 m | 357 | 427 | 9.31% | 2.47% | | 378 | 5.93% | 0.17% |
| 5.0 m | 376 | 447 | 7.02% | 1.04% | | 391 | 5.70% | 0.17% |
| 2.0 m | 467 | 538 | 4.34% | 0.17% | | 467 | 4.34% | 0.17% |
| 1.0 m | 579 | 650 | 3.76% | 0.17% | | 579 | 3.76% | 0.17% |
| 0.0 m (none) | 1104 | 1175 | 3.67% | 0.17% | | 1104 | 3.67% | 0.17% |

Read three things off it:

1. **The exact overlap is a monotone function of the simplification tolerance** and collapses to a
   0.17% floor. **3.48 of the 3.66 pp — 95.2% of the real road-in-building overlap — is RDP
   displacement**, and the 0.17% floor is the source data (§2.1, to 0.003 pp).
2. **Reaching that floor by lowering the tolerance costs 219% more nodes** (346 → 1104), which the
   importer's `max_nodes = 380` budget cannot pay; it would escalate into dropping `living_street`
   and `residential` classes entirely, which is a worse map, not a better one.
3. **The constraint reaches the same floor at 374 nodes**, +8.1%, and is a *no-op* at tolerances
   where displacement does not occur (tol ≤ 2 m is byte-identical constrained or not). It fires only
   where it is needed.

A 10 m deviation tolerance is a whole building. That is the entire mechanism.

## 3. What is genuinely there: three archways and a fort gate

`osm_registration.py raw` prints every offending way with its tags. All seven, and all four
footprints:

| OSM way | class | metres inside | tags | verdict |
|---|---|---|---|---|
| 24991573 Heydeckstraße | tertiary | 19 | **`tunnel=building_passage`** | real |
| 210464672 Reitschulgasse | living_street | 10 | **`tunnel=building_passage`** | real |
| 103512664 Kreuzstraße | residential | 6 | **`tunnel=building_passage`** | real |
| 77912270 Proviantstraße | residential | 1 | **`tunnel=building_passage`** | real |
| 11014139 Heydeckstraße | tertiary | 1 | untagged | stub at a passage junction |
| 210464673 Reitschulgasse | living_street | 1 | untagged | stub at a passage junction |
| 77912237 Proviantstraße | residential | 1 | untagged | stub at a passage junction |

| footprint | metres of road under it | tags |
|---|---|---|
| 306385396 | 20 | `building=yes`, `layer=1`, **name `Kavalier Heydeck`** |
| 219952920 | 11 | `building=house` |
| 103512658 | 6 | `building=yes`, **`tunnel=building_passage`** |
| 1260744026 | 2 | `building=yes`, `building:levels=2` |

**36 of the 39 metres (92.3%) are on ways OSM explicitly tags as passing through a building.** The
Kavalier Heydeck is a Bavarian fort casemate that the street runs straight through; Reitschulgasse
and Kreuzstraße are old-town archways.

The other three metres are not a fourth cause. OSM splits a street into several ways where a tag
changes, so the archway's footprint laps a few decimetres onto the *untagged* way next door: each of
those 1 m samples is on a way that **shares a graph node with a tagged passage way and enters the
same footprint**. Verified on the built graph — undirected edge 4 (way 11014139) shares node 3 with
edge 7 (way 24991573, `tunnel=building_passage`); edge 22 (way 24692328) shares node 16 with edge 24
(way 210464672) and enters the identical ring 219952920. `_overlap_report` therefore lets such a
stub inherit its neighbour's reason and records the `via_way` it inherited from, so **`n_untagged`
on the fixed Ingolstadt map is 0**: 37.6 m of 37.6 m accounted for.

**Split of the shipped 3.658% exact overlap: 95.2% RDP displacement (a bug, fixed), 4.8% real
archway geometry (kept, flagged, and 100% attributable to `tunnel=building_passage`).**

## 4. The fix

`osm_to_network(..., avoid_polygons=<rings in this extract's frame>)` swaps `_rdp` for `_rdp_avoid`:

```
if the chord's deviation is within tol:
    chord_rings = footprints the CHORD enters
    if chord_rings and (chord_rings - footprints the ORIGINAL CHAIN enters):
        split at the max-deviation vertex anyway and recurse
```

Three properties worth naming:

* **It is a set difference, not a boolean.** A chain that legitimately runs through an archway keeps
  running through it; only footprints the chain did not already enter can force a split. Without
  that, a `tunnel=building_passage` way would explode back into every one of its source vertices.
  (`test_rdp_avoid_does_not_explode_a_road_that_really_goes_through`.)
* **It terminates.** The forced split is always at `imax`, strictly interior, so the worst case is
  the original polyline. (`test_rdp_avoid_terminates_on_a_pathological_chain`.)
* **The containment test is exact, not rastered** (`FootprintIndex`, grid-bucketed proper segment
  intersection plus point-in-polygon). Using the 3 m raster here would let the classifier's halo
  drive the importer into pinning a vertex beside every wall in the city. Touching is deliberately
  *not* entering — a passage way shares its end nodes with the wall it pierces.

On the Ingolstadt extract it forces **28 splits** and keeps **843 vertices where plain RDP kept
815**.

What survives is reported, never deleted. `info["road_building_overlap"]` and the document's
additive `through_building_edges` key carry each surviving edge with the OSM reason
(`through_building_reason`: `tunnel=*`, `covered=*`, `man_made=tunnel`, negative `layer`) or
`"untagged"`, plus `inside_m` — the length that is *actually* inside, measured by walking the edge,
not the edge's whole length. On the fixed map that is 5 edges, **3 directly tagged
`tunnel=building_passage` (36.6 m) plus 2 sub-metre junction stubs that inherit it via `via_way`
(1.0 m)** — 37.6 m of 21 074 m = 0.178%, `n_untagged` **0**.

## 5. Before and after, on the flagship

`osm_registration.py flagship` rebuilds `datasets/phase2_geometric/urban_osm_geometric`'s own run on
both maps, taking every other knob verbatim from its manifest. The BEFORE arm reproduces the
shipped dataset **byte-for-byte** — `data_digest_sha256 = 58dab5096b47f8b2…`, 612 vehicles, 4481
reports, 49 792 emissions — so the comparison is the map and nothing else.

| | shipped map | registered map |
|---|---|---|
| graph | 346 nodes / 416 edges | 374 nodes / 444 edges |
| **road length inside a footprint, raster 3 m** | **10.352%** (2199 / 21 242) | **5.988%** (1275 / 21 294) |
| **road length inside a footprint, exact** | **3.658%** (777) | **0.174%** (37) |
| edges with any interior sample | 30 of 416 | 5 of 444 |
| median depth inside the footprint | 1.02 m (p90 2.89 m) | 3.03 m (p90 7.86 m) — i.e. real passages |
| **vehicle emissions on a building cell, raster 3 m** | **9.616%** (4788 / 49 792) | **5.498%** (2776 / 50 490) |
| **vehicle emissions inside a footprint, exact** | **4.374%** (2178) | **0.113%** (57) |
| dataset digest | `58dab5096b47f8b2…` | `7f267a434b684bda…` |

**Vehicles standing inside a wall fall 38.7x by the exact measure** (4.374% → 0.113%), and the
residual is vehicles driving through the Kavalier Heydeck, which is what vehicles there do.

The raster figure only halves, because 5.8 of its remaining 5.99 pp is the 3 m halo (§1): road
centrelines 1–3 m from a facade, which in the Ingolstadt Altstadt is an ordinary street. Comparing
that 5.99% against InTAS's 2.26% is comparing a 2 km² old-town core against a 66 km² city whose
traffic is on arterials, exactly as `CROSS-ENGINE-RADIO.md` §5 says.

## 6. What it does to the link-state mix

Measured with the unmodified `scms_sim_ref.datagen.awareness` CLI on both arms. The BEFORE column
reproduces `AWARENESS-GATE.md` to the digit (103.5 m, ratio 1.1884, LOS 0.060 / NLOSv 0.031 /
NLOSb 0.909 at 200 m).

| band (m) | LOS / NLOSv / NLOSb before | LOS / NLOSv / NLOSb after | ΔLOS | PDR/packet |
|---|---|---|---|---|
| 0–50 | 0.6198 / 0.1586 / 0.2216 | 0.6373 / 0.1518 / 0.2109 | +0.0175 | 0.9945 → 0.9947 |
| 50–100 | 0.2770 / 0.1711 / 0.5519 | 0.2841 / 0.1713 / 0.5447 | +0.0071 | 0.5307 → 0.5366 |
| 100–150 | 0.1306 / 0.0796 / 0.7898 | 0.1464 / 0.0936 / 0.7600 | +0.0158 | 0.2030 → 0.2295 |
| 150–200 | 0.0751 / 0.0391 / 0.8858 | 0.1063 / 0.0585 / 0.8352 | **+0.0312** | 0.0973 → 0.1392 |
| **200–250** | **0.0461 / 0.0239 / 0.9300** | **0.0638 / 0.0274 / 0.9088** | **+0.0177** | 0.0557 → 0.0740 |
| 250–300 | 0.0336 / 0.0112 / 0.9553 | 0.0502 / 0.0147 / 0.9350 | +0.0166 | 0.0353 → 0.0519 |
| 300–350 | 0.0253 / 0.0065 / 0.9682 | 0.0430 / 0.0087 / 0.9484 | +0.0177 | 0.0245 → 0.0407 |
| 350–400 | 0.0202 / 0.0032 / 0.9766 | 0.0310 / 0.0041 / 0.9649 | +0.0108 | 0.0179 → 0.0273 |
| 400–450 | 0.0150 / 0.0015 / 0.9834 | 0.0199 / 0.0027 / 0.9774 | +0.0049 | 0.0125 → 0.0168 |
| 450–500 | 0.0141 / 0.0015 / 0.9845 | 0.0173 / 0.0031 / 0.9796 | +0.0032 | 0.0112 → 0.0141 |
| overall | 0.0339 / 0.0129 / 0.9532 | 0.0420 / 0.0156 / 0.9424 | +0.0081 | — |

At the published 200 m anchor: **LOS 0.0598 → 0.0831, NLOSv 0.0311 → 0.0415, NLOSb 0.9091 →
0.8754**, per-packet PDR **0.0754 → 0.1036 (+37.4%)**.

Headline awareness numbers, both arms, same 104 dB budget:

| | before | after |
|---|---|---|
| NAR-0.90-equivalent range | 103.5 m | **106.3 m** |
| d90 / d50 / d20 | 35.2 / 79.7 / 126.4 m | 35.3 / 81.0 / 141.3 m |
| gray-zone ratio d20/d90 (floor 1.9191) | 3.5931 | 3.9991 |
| reference at this budget | 87.06 m | 87.06 m |
| **ratio vs the reference** | **1.1884 consistent** | **1.2214 consistent** |

**So yes, the corrected map changes the awareness number, and by 2.7%** (103.5 → 106.3 m). The gate
verdict is unchanged: still `consistent`, still 1.2x rather than the 5.8x the MOSAIC radio reads.

**How much of the cross-engine composition gap does it close?** Under the same Python classifier the
InTAS core reads NLOSb 0.3148 at 200 m against this scene's 0.9091. The fix moves it to 0.8754:
**3.37 pp of a 59.4 pp gap = 5.7%.** `CROSS-ENGINE-RADIO.md` estimated "about 5%" by excluding pairs
with an endpoint on a building cell; that estimate is confirmed by construction. **The registration
defect was never the explanation for the two engines' scene divergence, and fixing it does not make
it one.** It is fixed because a road inside a building is wrong on its face.

## 7. What is byte-identical, and what is not

* **`osm_to_network` with `avoid_polygons=None` is unchanged**, and `_rdp` itself was not touched.
  Proof: `python -m scms_sim_ref.mock_pipeline.osm --city ingolstadt --buildings
  --no-building-aware-roads` reproduces the shipped `datasets/_osmcache/ingolstadt_net.json`
  **byte-for-byte** (sha256 `868f03d1fce3829dd19b9bacda67226f…`, 163 182 bytes), and the flagship
  BEFORE arm reproduces `58dab5096b47f8b2…`.
* **Both pinned digests are unaffected**, as expected — they are synthetic grids and never touch
  the OSM importer. Re-measured on this tree: the reference run
  (`--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15
  --traffic-lights --seed 42`) → **`b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815`**,
  and the default golden **`0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740`** via
  `tests/test_config_knobs.py`, `test_radio_propagation.py`, `test_conformance.py`.
* **Zero new RNG.** `_rdp_avoid` and `FootprintIndex` are pure geometry; no stream is created,
  consumed or reordered on any path.
* **What does change:** a map imported *with* `--buildings` from now on. That is the point of the
  fix and is stated rather than hidden. `datasets/_osmcache/ingolstadt_net.json` is left exactly as
  it was; the corrected map is written beside it as
  **`datasets/_osmcache/ingolstadt_net_registered.json`** (sha256 `fd31f4e5b861bc4e0355d13e7d03bfdc…`,
  374 nodes / 444 edges / 1519 footprints /
  5 flagged `through_building_edges`). Adopting it into
  `datasets/phase2_geometric/urban_osm_geometric` is a dataset re-pin, deliberately not done here.

## Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1
$env:PYTHONPATH = "$PWD\src"

# 1. the cause: the tolerance sweep, constrained and unconstrained
python tools\osm_registration.py sweep --city ingolstadt --tols "10,7.5,5,2,1,0" --fixed

# 2. what is genuinely in the source data, with tags
python tools\osm_registration.py raw --city ingolstadt

# 3. before/after overlap, both instruments, plus the raster-halo split by cell size
python tools\osm_registration.py overlap --city ingolstadt --fixed --cells `
       --dataset datasets\phase2_geometric\urban_osm_geometric

# 4. the flagship run on both maps (the BEFORE arm must digest 58dab5096b47f8b2...)
python tools\osm_registration.py flagship --city ingolstadt `
       --reference datasets\phase2_geometric\urban_osm_geometric --out datasets\_osmreg

# 5. the link-state mix, unmodified instrument, on each arm
python -m scms_sim_ref.datagen.awareness datasets\_osmreg\before --markdown
python -m scms_sim_ref.datagen.awareness datasets\_osmreg\after  --markdown

# 6. the corrected map, and the byte-identity control
python -m scms_sim_ref.mock_pipeline.osm --city ingolstadt --buildings `
       --out datasets\_osmcache\ingolstadt_net_registered.json
python -m scms_sim_ref.mock_pipeline.osm --city ingolstadt --buildings --no-building-aware-roads `
       --out legacy.json     # must equal datasets\_osmcache\ingolstadt_net.json byte for byte
```
