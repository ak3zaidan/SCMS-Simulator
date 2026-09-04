# The two radios, measured against each other

This project ships two engines with two different radios and they had never been compared. The
Python engine (`src/scms_sim_ref/mock_pipeline`) has an opt-in geometric channel measured in
[`AWARENESS-GATE.md`](AWARENESS-GATE.md); the MOSAIC/Java path (`scms-sim/mosaic-apps/scms-app`)
has `org/scms/radio/` plus a real ETSI TS 102 687 DCC and EN 302 637-2 CAM generation rules, and
**nobody had ever run the comm panel against it**. This document does, with the same instruments,
and reports where the two disagree and why.

**Headline.** At a matched 104 dB link budget on real Ingolstadt buildings, the MOSAIC radio
delivers **0.780** of packets at 200 m where the Python engine delivers **0.0754** — a factor of
**10.3**. The decomposition is not what the framing "two different radios" suggests:

| step (each swaps exactly one thing) | PDR @ 200 m | factor |
|---|---|---|
| MOSAIC measured: InTAS scene, Java classifier, Java physics | **0.7798** | — |
| swap the PHYSICS only (Python propagation, Java's own measured LOS/NLOSb mix) | 0.7307 | 0.94x |
| swap the CLASSIFIER too (Python raster + NLOSv, same InTAS scene) | 0.5464 | 0.75x |
| swap the SCENE (the Python engine's own 1.6 km OSM extract) | 0.0754 | **0.14x** |

Splitting the total log-ratio across those three steps: **the scene is 85% of the divergence, the
link-state classifier 12%, the propagation physics 3%.** The radios differ by only 6.3% at 200 m in
aggregate — but that aggregate hides three specific, named model differences (§4) that are large per
link state, and the Java side is missing two of the Python side's three propagation terms.

**DCC and CAM triggering.** On the InTAS urban scenario, DCC does *nothing*: the measured channel
busy ratio is **0.027 mean / 0.115 max** against a first breakpoint of 0.30, and the DCC-on and
DCC-off runs are byte-identical on every counter (§7). Pushed to CBR 0.379 on a dense probe, DCC
removes **12.0%** of transmissions and **13.2%** of contention losses while delivering **+0.04%**
packets — i.e. it buys back the whole 12% of offered load for free, and yields **+2.8%** more
misbehaviour reports. CAM triggering, by contrast, is not marginal at any density: MOSAIC's mean
CAM gap is **0.3399 s (2.94 Hz)** against the Python engine's flat **1.000 s (1 Hz)**, a **2.9x**
difference in every rate-dependent quantity the project publishes.

---

## 1. What was run

Six MOSAIC arms plus the Python engine's existing flagship. Every MOSAIC arm uses the *same*
generated scenario, read-only — `scms-sim/scenarios/gen_intas_urban_low`, the GEH-validated InTAS
traffic input, 300 s, seed 20260809, 334 vehicles + 8 RSUs, 234 040 emissions with
`SCMS_EMIT_SAMPLE=1.0` — so the only thing that changes between arms is the `SCMS_*` radio
configuration.

| arm | model | tx dBm | budget | DCC | scenario | dataset |
|---|---|---|---|---|---|---|
| `geo_p23_dcc0` | geometric | 23.0 | 104.0 dB | off | InTAS urban | `datasets/xengine/geo_p23_dcc0` |
| `geo_p23_dcc1` | geometric | 23.0 | 104.0 dB | **on** | InTAS urban | `datasets/xengine/geo_p23_dcc1` |
| `geo_p13_dcc0` | geometric | 13.01 | 94.01 dB | off | InTAS urban | `datasets/xengine/geo_p13_dcc0` |
| `sns_p23_dcc0` | **sns** (the shipped default) | — | — | off | InTAS urban | `datasets/xengine/sns_p23_dcc0` |
| `cc_dense_dcc0` | geometric | 23.0 | 104.0 dB | off | dense grid probe | `datasets/xengine/cc_dense_dcc0` |
| `cc_dense_dcc1` | geometric | 23.0 | 104.0 dB | **on** | dense grid probe | `datasets/xengine/cc_dense_dcc1` |

The Python side is `datasets/phase2_geometric/urban_osm_geometric`, unchanged, re-measured here and
reproducing `AWARENESS-GATE.md` exactly (103.5 m NAR-0.90-equivalent range, 1.19x, LOS 0.060 at
200 m).

## 2. The instruments

Everything on both sides is read by `src/scms_sim_ref/datagen/awareness.py`, unmodified. The
reference arithmetic — the ≥1-of-Z shot-multiplicity conversion (`pdr_for_nar`), the Boban & d'Orey
urban curve interpolated **at our own budget** (`reference_nar90_distance_m`), the annulus crossing
estimator (`crossing_m`), the Python engine's own propagation model (`propagation_pdr`) — is
imported, never re-derived. Two new pieces of plumbing were needed and both are additive:

* **`org.scms.radio.LinkTrace`** (new, `scms-sim/mosaic-apps/scms-app/.../radio/LinkTrace.java`) —
  an opt-in CSV of every reception decision the Java radio took:
  `t_s,rx,dist_m,state,rssi_dbm,outcome,rx_x,rx_y,tx_x,tx_y`. Off unless `SCMS_LINK_TRACE` names a
  path. The per-frame sampling draw comes from a dedicated `Random` seeded off the scenario seed
  (`streamSeed("link-trace", unitId)`), never the channel RNG, so tracing cannot move a delivery
  verdict — **verified byte-for-byte**: the same arm run with `SCMS_LINK_TRACE` unset produces
  `manifest.data_digest_sha256 = a7d5dada86a6c202e4b8be87…`, identical to the traced run's.
* **`RxChannel`'s CBR meter now always runs.** Before this, `Dcc` was constructed only when
  `SCMS_DCC=1`, so a DCC-off run reported *no* CBR at all and the on/off delta would have been a
  difference between a number and a blank. `DCC_ENABLED` now gates only whether the resulting
  interval floor is *applied* (`dccMinIntervalS` returns 0.0 when off, exactly as before); the
  measurement runs either way.
* **`tools/xengine_radio.py`** (new) — bins the trace, checks it against the Java model's own
  closed-form delivery rule, evaluates the Python physics on the same measured mix, and writes an
  `awareness`-readable *shim* view of a MOSAIC dataset so the unmodified
  `python -m scms_sim_ref.datagen.awareness <shim>` runs over MOSAIC data.

**Trace fidelity.** The empirical per-band PDR reproduces the Java model's own rule
(`Φ((tx + 2·gain − PL(d) − sens)/σ)` mixed by the band's measured LOS/NLOSb fractions) to
**max |diff| 0.0016, weighted mean 0.0001 over 15 bands**. The measurement is measuring the model,
not the instrument.

## 3. The link budget each engine actually used — this is where a 6 dB moved a comparison before

| | MOSAIC/Java default | MOSAIC/Java, matched arm | Python engine flagship |
|---|---|---|---|
| tx power | 13.01 dBm (20 mW, VeReMi-NextGen `omnetpp.ini`) | 23.0 dBm | 23.0 dBm |
| antenna gain | 0 dBi | 0 dBi | not modelled |
| rx sensitivity | −81 dBm | −81 dBm | −81 dBm |
| decode floor | −81 dBm (`rssi >= RX_SENSITIVITY_DBM`) | −81 dBm | −81 dBm = `max(−81, −110 + 4)` |
| **link budget** | **94.01 dB** | **104.0 dB** | **104.0 dB** |
| range cap | SNS `singlehopRadius` **709.4 m**, hard | 709.4 m | `radio_range_m` 500 m + fading headroom |
| reference urban NAR-0.90 distance at that budget | *undefined* (below the pinned 100 dB floor) | 87.06 m | 87.06 m |

The two engines' **shipped defaults differ by 10 dB**. Everything in §4–§6 is at the matched 104 dB
so that difference is removed rather than absorbed. The 94.01 dB arm is reported separately in §6.

The Java decode floor is the bare sensitivity; the Python floor is `max(sensitivity, noise + SNIR)`
with `PHY_NOISE_DBM = −110`, `PHY_SNIR_THRESHOLD_DB = 4`. At −81 dBm sensitivity these coincide
(−81 > −106), so the floors agree here — but they diverge for any sensitivity below −106 dBm, and
the Java side would then be the optimistic one.

## 4. The radio with no scene in it

Per-state per-packet delivery at the matched 104 dB budget, both models evaluated analytically. This
is the pure radio comparison: no geometry, no population weighting.

| d (m) | LOS java | LOS python | NLOSb java | NLOSb python | NLOSv java | NLOSv python |
|---|---|---|---|---|---|---|
| 50 | 1.0000 | 1.0000 | 0.6565 | 0.5766 | **absent** | 0.9830 |
| 100 | 1.0000 | 0.9951 | 0.0318 | 0.0492 | **absent** | 0.8559 |
| 200 | 1.0000 | 0.9364 | 0.0000 | 0.0004 | **absent** | 0.5898 |
| 300 | 0.9995 | 0.8807 | 0.0000 | 0.0000 | **absent** | 0.4334 |
| 500 | 0.9795 | 0.7518 | 0.0000 | 0.0000 | **absent** | 0.2454 |

Three divergences, all attributable to a named model difference:

1. **No small-scale fading on the Java side.** `RxChannel.geometricDeliver` computes
   `rssi = tx + 2·gain − PL − shadow` and nothing else. The Python engine draws a Nakagami-m fade
   per packet (`m = 3 / 1.5 / 1.0` over 0–50 / 50–150 / >150 m). On a LOS link the fade is almost
   pure loss because the mean is already well above the floor: **1.0000 vs 0.9364 at 200 m,
   0.9795 vs 0.7518 at 500 m — 23 pp at 500 m.**
2. **No NLOSv branch on the Java side at all.** `PathLoss.nlosvMeanDb` exists and is never called;
   the class comment states why ("MOSAIC's SNS gives no per-link vehicle occupancy and the app has
   no fleet-wide rectangle index, so claiming an NLOSv classification would be fabricated"). Every
   vehicle-blocked link is therefore treated as clear LOS. This is not a corner case: on the InTAS
   core the Python classifier puts **48.6% of pairs at 200 m** in NLOSv, where the Python model
   applies 9–13 dB of extra loss (0.5898 vs 0.9364) and the Java model applies **0 dB**.
3. **The fade cuts both ways on NLOSb, and the sign flips with distance.** At 50 m the Java model is
   *more* optimistic (0.6565 vs 0.5766: the mean is above the floor, so a bad fade only hurts); at
   100 m it is *more* pessimistic (0.0318 vs 0.0492: the mean is far below the floor, so only a
   favourable fade delivers anything and the Java model has none to give).

Everything else agrees exactly: both use the same TR 37.885 constants (urban LOS 38.77/16.7/18.2,
urban NLOS 36.85/30.0/18.9), the same σ (3 dB LOS, 4 dB NLOSb), the same Gudmundson AR(1)
decorrelation (10 m / 13 m), the same 5.9 GHz carrier. `PathLoss.java` and
`refdata/pathloss_3gpp_tr37885.json` are one audited set of constants, and this measurement confirms
they are actually the same on both sides.

## 5. The scene, and the two classifiers on identical links

`gen_intas_urban_low` carries **21 717 building footprints** (110 431 vertices), indexed on a 50 m
grid, `aligned=true`, bbox 8269 × 7977 m. The Python flagship carries **1519** OSM polygons over
1538 × 1307 m. Both are "real Ingolstadt buildings"; they are not the same experiment.

| | Python flagship extract | MOSAIC / InTAS (whole city) | MOSAIC / InTAS densest 2 km box |
|---|---|---|---|
| footprints | 1519 | 21 717 | 4022 |
| extent | 1.54 × 1.31 km = 2.01 km² | 8.27 × 7.98 km = 65.96 km² | 4.00 km² |
| footprint density | 755 /km² | 329 /km² | 1006 /km² |
| median footprint | 153 m² | 157 m² | — |
| **built-area fraction** | **0.203** (over the vehicle bbox) | 0.123 | **0.187** |
| vehicles spread over | 1.62 × 1.50 km | 9.06 × 8.38 km | 8.8% of all emissions |

**The classifiers agree; the scenes do not.** Running the Python engine's own `_BuildingRaster` over
the *identical segments* the Java `BuildingIndex` judged (139 495 traced links inside the 2 km core
box, 3 m raster):

| | Java `BuildingIndex` | Python `_BuildingRaster` | Python, endpoint clearance off |
|---|---|---|---|
| NLOSb fraction | 0.4561 | 0.4839 | 0.4840 |

**97.22% agreement**, and the disagreement is entirely one-directional — `java_nlosb & py_los = 0`,
`java_los & py_nlosb = 3881`. The Python raster over-blocks by 1.4–8.7 pp per band, exactly as its
3 m occupancy cells predict (a wall is at least one cell thick, whereas the Java test intersects the
polygon edges exactly). `GEO_ENDPOINT_CLEAR_M = 6 m` changes the answer by **0.0001**. So the
geometry tests are *not* the cause of the cross-engine divergence, and the composition difference is
a property of the two maps:

| band | Java on InTAS (transmission-weighted) | Python on InTAS (co-presence) | Python on its own OSM extract |
|---|---|---|---|
| 0–50 m | LOS 0.9971 / NLOSb 0.0029 | LOS 0.674 / NLOSv 0.306 / NLOSb 0.020 | LOS 0.620 / NLOSv 0.159 / NLOSb 0.222 |
| 50–100 m | LOS 0.9743 / NLOSb 0.0257 | LOS 0.463 / NLOSv 0.434 / NLOSb 0.104 | LOS 0.277 / NLOSv 0.171 / NLOSb 0.552 |
| 100–150 m | LOS 0.8992 / NLOSb 0.1008 | LOS 0.456 / NLOSv 0.330 / NLOSb 0.214 | LOS 0.131 / NLOSv 0.080 / NLOSb 0.790 |
| 150–200 m | LOS 0.8172 / NLOSb 0.1828 | LOS 0.454 / NLOSv 0.254 / NLOSb 0.292 | LOS 0.075 / NLOSv 0.039 / NLOSb 0.886 |
| **200–250 m** | **LOS 0.7409 / NLOSb 0.2591** | LOS 0.411 / NLOSv 0.214 / NLOSb 0.375 | **LOS 0.046 / NLOSv 0.024 / NLOSb 0.930** |
| 300–350 m | LOS 0.5782 / NLOSb 0.4218 | LOS 0.292 / NLOSv 0.181 / NLOSb 0.526 | LOS 0.025 / NLOSv 0.007 / NLOSb 0.968 |
| 450–500 m | LOS 0.3783 / NLOSb 0.6217 | LOS 0.239 / NLOSv 0.118 / NLOSb 0.643 | LOS 0.014 / NLOSv 0.002 / NLOSb 0.985 |

Restricting the InTAS scene to its densest 2 km box — a *higher* footprint density than the Python
extract (1006 vs 755 /km²) at a comparable built fraction (0.187 vs 0.203) — the Python instrument
still reports **LOS 0.1995 / NLOSv 0.4857 / NLOSb 0.3148** at 200 m against the Python flagship's
own **0.060 / 0.031 / 0.909**. Same code, same raster resolution, same city, 2.9x apart in NLOSb.

### A registration defect on the Python side — real, but not the explanation

Sampling the two road networks every metre against their own footprints with the identical 3 m
raster:

| | Python flagship | MOSAIC / InTAS core |
|---|---|---|
| **road length inside a building footprint** | **10.35%** (2199 of 21 242 1 m samples) | **2.26%** (4741 of 209 656) |
| road edges crossing a building | 20.2% of 416 edges (24.0% with no clearance) | 3.3% of 12 514 lane segments (4.8%) |
| **vehicle emissions standing on a building cell** | **9.62%** (4788 of 49 792) | **0.20%** (41 of 20 482) |
| road-graph *nodes* inside a building | 4.34% of 346 | — |
| median vehicle clearance to nearest footprint | 8.5 m (p10 3.0 m = one cell) | 12.7 m (p10 6.7 m) |

The Python engine's OSM road graph runs through its own buildings 4.6x more (by road length) and
its vehicles stand inside walls **48x** more often. The `GEO_ENDPOINT_CLEAR_M = 6 m` clearance was
added for exactly this ("the road graph is RDP-simplified (10 m) and the raster has a ~1-cell halo,
so an antenna can nominally land on a building cell") and it only clears the first 6 m of a link.

**But measure it before blaming it.** Excluding every pair with an endpoint on a building cell moves
the Python flagship's NLOSb at 200–250 m from **0.9064 to 0.8819** — 2.4 pp against a 53 pp gap to
the InTAS core's 0.3764 in the same band under the same classifier, i.e. **about 5% of it**. The
registration defect is real and worth fixing; it is *not* why the two scenes disagree.

What is left is structural: 346 nodes and 416 edges of RDP-simplified skeleton (median edge 37.6 m)
over a 1.6 km old-town extract, versus a real SUMO lane network (median lane segment 4.3 m) over
66 km² whose traffic concentrates on arterials — only 6.5% of MOSAIC emissions fall within 1 km of
the densest core, and 40.8% within 3 km. A 200 m pair drawn from the Python extract is almost always
cross-block; one drawn from InTAS is usually along an arterial.

## 6. The MOSAIC radio, measured

`geo_p23_dcc0`, 1 466 479 traced reception decisions sampled at p = 0.3 from 4 884 834 total.
"PDR(prop)" excludes the CSMA contention drop (the reference simulation models no interference);
"java-cf" is the closed form; "py-phys" is the Python propagation model on this same measured mix.

| band | n | LOS | NLOSb | PDR(prop) | PDR(+contention) | java-cf | py-phys |
|---|---|---|---|---|---|---|---|
| 0–50 | 144 965 | 0.9971 | 0.0029 | 0.9997 | 0.9992 | 1.0000 | 0.9999 |
| 50–100 | 111 815 | 0.9743 | 0.0257 | 0.9789 | 0.9785 | 0.9789 | 0.9765 |
| 100–150 | 102 313 | 0.8992 | 0.1008 | 0.8998 | 0.8991 | 0.8997 | 0.8929 |
| 150–200 | 106 038 | 0.8172 | 0.1828 | 0.8172 | 0.8165 | 0.8172 | 0.7754 |
| 200–250 | 101 956 | 0.7409 | 0.2591 | 0.7409 | 0.7397 | 0.7408 | 0.6841 |
| 250–300 | 93 686 | 0.6370 | 0.3630 | 0.6369 | 0.6361 | 0.6368 | 0.5704 |
| 300–350 | 96 781 | 0.5782 | 0.4218 | 0.5776 | 0.5764 | 0.5777 | 0.5004 |
| 350–400 | 95 780 | 0.4823 | 0.5177 | 0.4807 | 0.4794 | 0.4808 | 0.4022 |
| 400–450 | 92 477 | 0.4008 | 0.5992 | 0.3980 | 0.3975 | 0.3978 | 0.3212 |
| 450–500 | 91 124 | 0.3783 | 0.6217 | 0.3732 | 0.3727 | 0.3726 | 0.2907 |
| 500–550 | 94 409 | 0.3333 | 0.6667 | 0.3243 | 0.3239 | 0.3242 | 0.2450 |
| 550–600 | 99 543 | 0.2770 | 0.7230 | 0.2647 | 0.2639 | 0.2647 | 0.1944 |
| 600–650 | 99 640 | 0.2810 | 0.7190 | 0.2625 | 0.2618 | 0.2624 | 0.1881 |
| 650–700 | 116 715 | 0.2991 | 0.7009 | 0.2710 | 0.2705 | 0.2710 | 0.1905 |
| 700–750 | 19 237 | 0.2362 | 0.7638 | 0.2080 | 0.2080 | 0.2064 | 0.1430 |

Aggregate over every frame SNS handed the app: **4 884 834 sensed, 2 859 862 delivered = 0.585**,
of which 2 021 568 lost to obstruction and only **3404 to contention**. NLOSb over all links 0.409.

### Crossings, both engines, at the matched 104 dB budget

| | MOSAIC measured | MOSAIC closed form | Python physics on Java's measured mix | Python physics **and** classifier, MOSAIC scene | Python engine, own scene |
|---|---|---|---|---|---|
| PDR 0.90 crossing | 124.9 m | 124.8 m | 120.8 m | 64.9 m | 35.2 m |
| PDR 0.50 crossing | 365.0 m | 365.1 m | 325.2 m | 223.2 m | 79.7 m |
| PDR 0.20 crossing | *truncated* | 726.5 m | 569.5 m | 500.7 m | 126.4 m |
| gray-zone ratio d20/d90 | *truncated* | **5.822** | 4.716 | 7.720 | **3.593** |
| **NAR-0.90-equivalent range** | **504.6 m** | 504.4 m | 410.8 m | 306.8 m | **103.5 m** |
| Z bracket (source's fitted Z 2.14–8.29) | 264.1 – 697.6 m | 264.0 – 697.0 m | 235.8 – 527.4 m | 148.5 – 414.6 m | 61.1 – 119.0 m |
| reference at this budget | 87.06 m | 87.06 m | 87.06 m | 87.06 m | 87.06 m |
| **ratio vs the reference** | **5.80x** | 5.79x | 4.72x | 3.52x | **1.19x — consistent** |

The fourth column is the unmodified `awareness.py` CLI run over a shim view of the MOSAIC dataset
(§2), i.e. the *whole* Python instrument — its raster, its NLOSv classification and its propagation —
pointed at MOSAIC's scene and emissions. It sits between the two engines exactly as the headline
decomposition predicts.

*Truncated*: the MOSAIC curve cannot be read past 709.4 m because SNS's `singlehopRadius` is a hard
disc that stops handing frames to the app there; the PDR at the last full band (700–750 m) is still
0.208 > 0.20. The closed-form column supplies d20 and the gray-zone ratio for that arm.

**The MOSAIC radio is 5.80x more permissive than Boban & d'Orey's own urban curve at the same link
budget, where the Python engine is 1.19x and passes.** Swapping only the physics moves 5.80x → 4.72x
(a 1.23x range effect, the radio); swapping the scene moves 4.72x → 1.19x (a 3.97x range effect).
Both engines' gray-zone ratios clear the 1.9191 floor.

### The 94.01 dB arm — what the MOSAIC path does by default

`geo_p13_dcc0`, same scene, tx 13.01 dBm: delivery **0.383** (vs 0.585), PDR 0.9288 / 0.6318 /
0.2965 at 100 / 200 / 300 m, d90/d50/d20 = 114.4 / 232.8 / 341.9 m, gray-zone ratio 2.988,
NAR-0.90-equivalent range 282.0 m. The reference curve is **not** evaluated for it: 94.01 dB is
below the pinned 100 dB floor of the three Fig. 18 points and `awareness.py` refuses to
extrapolate, so no ratio is quoted. MA reports fall from 14 038 to 10 893 (−22.4%).

### What the traffic flagship actually shipped

`sns_p23_dcc0` is the default `SCMS_RADIO_MODEL=sns` path: MOSAIC's SNS
(`SophisticatedAdhocTransmissionModel`, `singlehopRadius` 709.4 m, `lossProbability` 0.0,
`singlehopDelay` `SimpleRandomDelay` 0.4–2.4 ms in 5 steps), with `SCMS_NLOS=0` so the legacy
distance-ramp heuristic is off. Measured: **4 884 834 sensed, 4 863 958 delivered = 0.996**, zero
obstruction losses, 20 876 contention losses, **28 004 MA reports**. That is a 709.4 m unit disc.
Turning the geometric radio on at the same budget halves delivery (0.996 → 0.585) and halves the
evidence (28 004 → 14 038 reports).

## 7. What DCC and CAM triggering buy

### DCC on the real scenario: exactly nothing, and the reason is measurable

`geo_p23_dcc0` and `geo_p23_dcc1` are identical on every counter — 4 884 834 sensed, 2 859 862
delivered, 234 040 CAMs, 14 038 reports, 51 revocations, 1 466 479 trace rows, **0 CAMs suppressed**
— and identical at the byte level: both write
`manifest.data_digest_sha256 = a7d5dada86a6c202e4b8be87…`, the same digest the trace-off control
run produces. Turning ETSI DCC on here changes not one row of the dataset. (The dense probe's two
arms do differ: `24b445964cdbe08c…` vs `fb9f1c1b99d56bf6…`.)

The CBR meter (now always on, §2) explains it. Measured **mean 0.027, max 0.115** over 78 168 DCC
sampling instants, against a first breakpoint of **0.30**. Converting through the pinned airtime:

| | frames/s sensed | neighbours inside the 709.4 m radius at the measured 2.94 Hz |
|---|---|---|
| measured mean CBR 0.027 | 60.3 | 20.5 |
| measured max CBR 0.115 | 256.7 | 87.3 |
| **first DCC step (CBR 0.30 → 10 Hz cap)** | **670** | **228** (67 at ETSI's 10 Hz reference rate) |
| CBR 0.40 → 5 Hz | 893 | 304 |
| CBR 0.50 → 2.5 Hz | 1116 | 379 |
| CBR 0.60 → 1 Hz | 1339 | 455 |

This holds under **all three** CBR conventions the project pins, not just the default:

| convention | airtime | mean CBR | max CBR |
|---|---|---|---|
| PPDU only, 300 B @ 6 Mb/s (Java default) | 448.0 µs | 0.0270 | 0.1150 |
| PPDU + AIFS_BE + mean backoff | 655.5 µs | 0.0395 | 0.1683 |
| 500 B signed CAM + overhead (the Python engine's `PHY_FRAME_AIRTIME_S`) | 919.5 µs | 0.0554 | **0.2360** |

Even at the most generous constant the busiest receiver-second in the city never reaches 0.30. The
InTAS urban window is a 300 s **cold start** on a multi-hour demand file: vehicles enter at ~1.1/s
and the network holds 334 of them across 66 km², i.e. ~20 neighbours inside the 709.4 m radius. **At
that density porting DCC to the Python engine would change nothing at all** — and the Python engine
is *further* from engaging it than MOSAIC is, because its CAM rate is 2.94x lower: the same 670
frames/s that 228 MOSAIC neighbours produce at 2.94 Hz would need 670 neighbours at the Python
engine's 1 Hz (326 if the port also adopted the Python engine's larger 919.5 µs airtime constant).
That is a real answer to "what does DCC buy", and it is worth knowing.

The congestion probe is a synthetic grid rather than a denser InTAS for a mechanical reason worth
recording: SUMO's `--scale` is **incompatible with MOSAIC**. It clones vehicles onto derived route
ids and `SumoAmbassador` then aborts the run with *"Could not retrieve route edges for route
'!randUni20724:1#1'"*. Density has to be raised by inserting more distinct trips
(`mapgen.build(period=...)`, which is what `tools/make_cc_probe_scenario.py` does), not by scaling.

### DCC where it does engage: 12% of the load for free

`cc_dense_dcc0/1`, a deliberate congestion probe (`gen_grid_12x12`, 720 vehicles, `period=0.25`, no
buildings — a channel measurement, not a claim about any city), at CBR 0.379 mean / 1.000 max:

| | DCC off | DCC on | delta |
|---|---|---|---|
| CBR mean | 0.379 | 0.334 | **−11.9%** |
| CBR max | 1.000 | 0.807 | −19.3% |
| CAMs emitted | 142 931 | 126 201 | −11.7% |
| CAMs suppressed by DCC | 0 | 46 890 | — |
| mean CAM gap / rate (honest) | 0.4463 s / 2.241 Hz | 0.5100 s / 1.961 Hz | −12.5% rate |
| gaps at T_GenCamMax (1.0 s) | 18.1% | 22.4% | +4.3 pp |
| frames sensed | 53 062 385 | 46 713 434 | −12.0% |
| **frames delivered** | 4 586 285 | 4 588 061 | **+0.04%** |
| delivery ratio | 0.086 | 0.098 | +14.0% relative |
| dropped to contention | 47 061 168 | 40 865 547 | −13.2% |
| **MA reports** | 201 047 | 206 650 | **+2.8%** |

DCC removed 12% of transmissions and 12% of channel occupancy at **zero cost in delivered packets**,
and produced 2.8% *more* misbehaviour evidence. Caveat, stated plainly: the delivery side of this
number is governed by the app's CSMA model (`SCMS_CHAN_CAPACITY=25` frames per 100 ms with a linear
drop ramp), not a real 802.11p MAC, so "delivery unchanged" is a statement about that heuristic. The
CBR, CAM-rate and suppression columns are model-independent.

### CAM triggering: 2.9x, at every density

| | MOSAIC (EN 302 637-2 rules) | Python engine |
|---|---|---|
| mean gap (honest vehicles) | **0.3399 s → 2.942 Hz** | **1.000 s → 1.000 Hz** |
| mean gap (all vehicles, DoS bursts included) | 0.3338 s → 2.996 Hz | 1.000 s |
| median | 0.300 s | 1.000 s (`comm.cam_inter_packet_gap_p50_s`) |
| p05 / p25 / p75 / p95 / p99 / max | 0.200 / 0.200 / 0.400 / 1.000 / 1.000 / 1.100 s | 1.0 throughout |
| at T_GenCamMin (0.1 s) | 4.91% | — |
| dynamics-triggered (between the floor and the heartbeat) | **89.58%** | 0% |
| at T_GenCamMax (1.0 s heartbeat) | 5.51% | 100% |
| gap histogram (s) | <0.15: 8950; 0.15–0.25: 45 893; 0.25–0.35: 67 129; 0.35–0.55: 45 797; 0.55–0.95: 4328; 0.95–1.05: 10 009 | one spike at 1.0 |

The Python engine emits exactly one CAM per vehicle per step and `dt = 1.0`. Nine CAMs in ten on the
MOSAIC side are fired by a dynamics trigger that does not exist on the Python side at all.

This has a direct consequence for the awareness metric, and `awareness.py` already handles it: the
shot multiplicity `Z ≤ N` is **1** for the Python engine (one shot per 1 s window, so its awareness
ratio *is* a per-packet PDR) and **2** for MOSAIC at 2.94 Hz. Any NAR-style metric compared across
the two engines without that conversion is off by `1 − (1 − PDR)^2` versus `PDR`.

*Note on a prior figure:* the mean CAM interval measured here is **0.3399 s**, not the 0.202 s quoted
in the task framing. 0.202 s is not reproduced by `gen_intas_urban_low` at seed 20260809 under any
of the six arms; all six give 0.3399 s honest / 0.3338 s including attackers.

## 8. Every divergence, with a cause

| # | divergence | magnitude | cause |
|---|---|---|---|
| 1 | shipped link budget | 94.01 dB vs 104.0 dB | different `tx_power` defaults: `RxChannel.TX_POWER_DBM` = 20 mW from VeReMi-NextGen's `omnetpp.ini`; the Python flagship config sets `radio_tx_power_dbm = 23.0`. **Configuration, not model.** |
| 2 | LOS delivery at range | 1.0000 vs 0.9364 @200 m, 0.9795 vs 0.7518 @500 m | **Java has no small-scale fading.** The Python engine applies Nakagami-m (3/1.5/1.0 by band); `RxChannel.geometricDeliver` applies path loss + shadowing only. |
| 3 | vehicle-blocked links | 48.6% of core pairs at 200 m get 9–13 dB on one side and 0 dB on the other | **Java has no NLOSv branch.** `PathLoss.nlosvMeanDb` exists and is never called; documented as deliberate (no per-link vehicle occupancy available from SNS). |
| 4 | NLOSb delivery, sign flips with distance | +0.080 @50 m, −0.017 @100 m | same missing fade as #2, acting in the opposite direction where the mean sits below the floor |
| 5 | LOS/NLOSb classification on identical links | 0.4561 vs 0.4839 (**97.22% agreement**, one-directional) | Python rasterises footprints at 3 m so walls are ≥1 cell thick; Java intersects polygon edges exactly. The 6 m endpoint clearance contributes **0.0001**. |
| 6 | link-state composition | LOS 0.780 vs 0.060 at 200 m | **the scenes**: a 66 km² real SUMO city with arterial traffic vs a 2.01 km² RDP-simplified OSM core extract. Confirmed by running one classifier over both. |
| 7 | road/footprint registration | 10.35% vs 2.26% of road length inside buildings; 9.62% vs 0.20% of vehicles | **defect on the Python side** (§5). Accounts for 2.4 pp of the 59 pp composition gap — real, but minor. |
| 8 | range cap | SNS 709.4 m hard disc vs a 500 m candidate window + fading headroom | SNS `singlehopRadius`. Truncates the MOSAIC PDR curve; no effect inside 700 m at this budget. |
| 9 | congestion control | ETSI TS 102 687 reactive DCC vs none | present on Java, absent on Python. Worth **0.0%** at InTAS density, **12%** of offered load at CBR 0.379. |
| 10 | CAM generation | 2.942 Hz, 89.6% dynamics-triggered vs a flat 1.000 Hz | EN 302 637-2 rules in `ScmsBeaconApp` vs one CAM per step in `run.py`. |
| 11 | CBR numerator | frames **sensed** vs frames **decoded** | **defect on the Python side.** `RxChannel.deliver` calls `dcc.sense(t)` first, before any decode-side drop — ETSI's "medium sensed busy". `run.py` builds `load` from `in_range`, which is appended only when `evaluate_link` returns non-`None`, i.e. only for frames that cleared the decode floor. A frame that fails to decode still occupies the medium, so the Python CBR is biased **low**, by roughly the delivery ratio (0.585 on the MOSAIC arm; far smaller on a scene that is 95% NLOSb). |
| 12 | CBR airtime constant | 448.0 µs vs 919.5 µs | Java: 300 B PPDU at 6 Mb/s, PPDU-only (`Dcc.java`). Python: 500 B + 207.5 µs AIFS/backoff (`PHY_FRAME_AIRTIME_S`). **2.05x on the same modelled quantity.** Both readings are pinned in `refdata/phy_80211p_profile.json`; the two engines simply chose different ones. |
| 13 | carrier-sense population | every frame inside the 709.4 m SNS disc counts as sensed | the Java CBR ignores RSSI, so a building-blocked frame arriving at −120 dBm is still counted busy. Biases the Java CBR **high** against a real −85 dBm `PHY_CS_THRESHOLD_DBM` probe. Direction is opposite to #11. |
| 14 | per-packet latency | 0.4–2.4 ms (SNS `SimpleRandomDelay`, 5 steps) vs none | the Python engine models no channel latency at all. |
| 15 | MA report ingest delay | `0.15·(0.5+U)` s = 0.075–0.225 s vs `U(0, 2.0)` s | `ScmsBackend.INGEST_DELAY_S` vs `cfg.net_delay_max`. **Mean 0.15 s vs 1.00 s — 6.7x** on a quantity that is not the channel on either side. |

Not divergent, and worth recording as such: the TR 37.885 constants, the shadowing σ and
decorrelation distances, the weather-loss table (`RxChannel.WEATHER_RADIO_LOSS` is byte-identical to
`run.WEATHER_RADIO_LOSS`), and the decode floor at −81 dBm sensitivity.

## 9. What this does not show

* Nothing here re-measures **traffic**. The MOSAIC arms reuse `gen_intas_urban_low` read-only and
  the GEH work is untouched.
* The **congestion probe** (`cc_dense_*`) is a synthetic grid with no buildings. Its delivery
  numbers say nothing about urban propagation, and its DCC delta is bounded by the app's CSMA
  heuristic (§7).
* The **709.4 m truncation** means MOSAIC's d20 and gray-zone ratio come from the closed form, which
  the trace validates only inside 750 m.
* The Python flagship's scene is what it is. Whether an OSM extract of Ingolstadt's core *should*
  read 0.909 NLOSb at 200 m is not settled here; what is settled is that the InTAS scene reads
  0.315 under the same code, that 2.4 pp of the difference is the registration defect, and that the
  classifier accounts for none of it.
* `src/scms_sim_ref/mock_pipeline/run.py` was being edited by a parallel workstream while these
  measurements ran. The Python-side numbers reproduce `AWARENESS-GATE.md` to the digit (103.5 m,
  1.19x, LOS 0.060 / NLOSv 0.031 / NLOSb 0.909 at 200 m), so the imported constants and the raster
  were stable across the window; anything re-run later should be checked against those four values
  first.

## 10. What would close the gaps

1. **Port Nakagami fading to `RxChannel`.** One line in `geometricDeliver`, the constants already
   exist in `refdata/nakagami_fading.json`. Worth 6.4 pp at 200 m LOS and 23 pp at 500 m.
2. **Fix the Python CBR numerator** (#11): count candidate frames, not decoded ones. Until then the
   two engines' CBRs are not the same quantity even before the 2.05x airtime difference (#12).
3. **Pick one airtime convention** and record it in one place. `refdata/phy_80211p_profile.json`
   pins both; the engines should not each pick a different one silently.
4. **Fix the OSM import's registration** (#7) — 10.35% of road length inside buildings is a defect
   regardless of how little of the composition gap it explains.
5. **Do not port DCC to the Python engine for realism at current densities** — it is worth 0.0%
   there. Port it when a scenario is built that actually loads the channel, and port the CAM
   triggering *first*: at 1 Hz the Python engine cannot reach a DCC breakpoint even at 228
   neighbours.

---

## Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1        # JDK 17, SUMO 1.25.0, MOSAIC 25.2
$env:PYTHONPATH = "$PWD\src"

# 1. build the app (adds LinkTrace + the always-on CBR meter)
.\scms-sim\mosaic-apps\scms-app\build.ps1

# 2. the six MOSAIC arms (each ~60 s except the congestion probe at ~115 s)
.\tools\run_mosaic_radio_bench.ps1 -Arm geo_p23_dcc0 -TxPowerDbm 23      -Dcc 0
.\tools\run_mosaic_radio_bench.ps1 -Arm geo_p23_dcc1 -TxPowerDbm 23      -Dcc 1
.\tools\run_mosaic_radio_bench.ps1 -Arm geo_p13_dcc0 -TxPowerDbm 13.0103 -Dcc 0
.\tools\run_mosaic_radio_bench.ps1 -Arm sns_p23_dcc0 -RadioModel sns -Dcc 0 -TraceProb 0

python tools\make_cc_probe_scenario.py --key grid_12x12 --period 0.25 --duration 180s
.\tools\run_mosaic_radio_bench.ps1 -Arm cc_dense_dcc0 -Scenario gen_grid_12x12 -Dcc 0 -TraceProb 0.05
.\tools\run_mosaic_radio_bench.ps1 -Arm cc_dense_dcc1 -Scenario gen_grid_12x12 -Dcc 1 -TraceProb 0.05

# 2b. the non-perturbation control: same arm, SCMS_LINK_TRACE unset. Its
#     manifest.data_digest_sha256 must equal geo_p23_dcc0's (a7d5dada86a6c202e4b8be87...).
#     datasets\xengine\_notrace_check is that run.

# 3. measure each arm with the shared instruments
python tools\xengine_radio.py measure datasets\xengine\geo_p23_dcc0 --markdown `
       --json datasets\xengine\geo_p23_dcc0\radio_panel.json

# 4. the Python engine's own dataset, and the SAME instrument on the MOSAIC scene
python -m scms_sim_ref.datagen.awareness datasets\phase2_geometric\urban_osm_geometric --markdown `
       --json datasets\xengine\_py_flagship_awareness.json
python tools\xengine_radio.py shim datasets\xengine\geo_p23_dcc0 `
       --buildings scms-sim\scenarios\gen_intas_urban_low\sumo\buildings.poly.xml `
       --out datasets\xengine\_shim_geo_p23
python -m scms_sim_ref.datagen.awareness datasets\xengine\_shim_geo_p23 --markdown `
       --json datasets\xengine\_shim_geo_p23\awareness.json

# 5. the two classifiers on IDENTICAL links, in the densest 2 km of InTAS
python tools\xengine_radio.py classify-ab datasets\xengine\geo_p23_dcc0 `
       --buildings scms-sim\scenarios\gen_intas_urban_low\sumo\buildings.poly.xml `
       --bbox 213107,448041,215107,450041 --cell 3.0 --max-links 300000 `
       --json datasets\xengine\_classify_ab_core.json

# 6. the cross-engine tables
python tools\xengine_radio.py compare `
       "mosaic_p23=datasets\xengine\geo_p23_dcc0" "mosaic_p13=datasets\xengine\geo_p13_dcc0" `
       --python-awareness datasets\xengine\_py_flagship_awareness.json `
       --python-on-mosaic-scene datasets\xengine\_shim_geo_p23\awareness.json
```

`run_mosaic_radio_bench.ps1` deliberately does **not** go through `run.ps1`: `run.ps1` regenerates
the scenario first, which would rewrite `gen_intas_urban_low` and move the GEH-validated traffic
input's hashes. Here the scenario is read-only and only the jar is refreshed into it.
