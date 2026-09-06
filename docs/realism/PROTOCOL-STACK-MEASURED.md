# The protocol stack, measured

**Status: measurement only. No file under `src/` was touched, and both pinned digests were
re-derived before the work and again after it.**

| digest | expected | before | after |
|---|---|---|---|
| default golden (seed 7, flow, grid 5×5, 60 s, 1.5/s, atk 0.25) | `0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740` | **match** (0.6 s) | **match** (0.7 s) |
| reference run (`--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15 --traffic-lights --seed 42`) | `b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` | **match** (10.6 s) | **match** (12.4 s) |

The suite was run afterwards on the same tree: **1 553 passed, 1 warning, 1 470.58 s (24:30)** —
`.\test.ps1`, exit 0.

**One thing this work does not control, and must therefore state.** This working tree was being
edited *concurrently* by another agent while the measurements ran: `awareness.py` at 15:48,
`run.py` at 15:59, `sumo_trace.py` at 16:14, `netimport.py` at 16:33, plus a new
`FULL-CITY-SCENE.md` and `test_full_city_scene.py` (which is why the suite collects 1 553 tests
rather than the 1 531 this task was briefed with). Nothing here writes to `src/` — the eight files
this work adds are all under `tools/` and `docs/realism/` — but the later runs read whatever `src/`
was on disk at the time. Three checks say that did not move anything measured:

* **Every row of the cost table's minimum comes from a reading taken before 15:48**, the first of
  those edits. That is not a choice; the minimum-over-readings rule selected them because the
  earlier pass was also the less contended one.
* `base` reproduces `dbe5b061b18a7cae…` at 13:51, at 15:04 **and** at 16:24 — the last of those on
  the edited `run.py`. `ecdsa` reproduces `a62fa7654fa5158d…` at 14:29 and again at 16:16.
* Both pinned goldens re-derive at 16:30, on the edited tree.

The stack has been implemented for a while and had never been measured end to end. This is that
measurement: what each layer does to the traffic, what it costs, and — for two of the five layers —
the finding that at a real city's density it does nothing at all.

Everything below is a number this repository produced on this host. Where a figure comes from the
Java engine it is labelled, and where it comes from a published reference it is cited.

**The seven findings.**

1. With EN 302 637-2 generation rules at dt = 0.1 on a SUMO-backed InTAS run the Python engine
   emits at **3.3077 Hz, mean gap 0.302321 s, 82.59 % dynamics-triggered** (3.0267 Hz / 0.3304 s at
   AM-peak density). The Java engine measures 2.942 Hz / 0.3399 s / 89.58 % on the *same* scenario.
2. **The two engines agree.** The 11 % difference decomposes exactly into (a) `N_GenCam`, clause
   6.1.3's shortened-interval ratchet, which the Java app does not implement, and (b) a
   floating-point defect in the Java floor test (`dtLast >= 0.1` on nanosecond-derived doubles,
   which is false for **60.47 %** of consecutive 100 ms pairs). Replaying the Java rule *including
   its arithmetic* over the same frozen trajectories gives **0.344074 s / 2.9064 Hz / 4.99 %** at the
   floor against its published 0.3399 s / 2.942 Hz / 4.91 %.
3. The engine's shot multiplicity is now a measured **Z = 2.109** on the flagship scene (**3.3121** on
   InTAS) against the Java engine's Z = 2 — but **the earlier awareness conclusions do not change**:
   `nar90_equivalent_range_m` moves 103.5 → 101.5 m, well inside its own 60–117 m band, and the
   verdict stays "consistent".
4. `datagen/awareness.py` derives Z from `dt`, not from the rate. Under generation rules that
   over-states it **2.59×** and the reported NAR at 200 m **2.29×**. (Defect noted, not fixed: it is
   under `src/`.)
5. **Reactive DCC does nothing at any real density here.** InTAS: DCC-on and DCC-off are
   byte-identical. InTAS AM peak, 1 052 stations transmitting per step: `relaxed` for **99.79 %** of
   station-steps. The generation rules are why — they take 69.7 % of the load out before DCC is
   consulted.
6. **A real UPER CAM is 41 octets, always** — a point mass over 14 400 swept claims. On the wire it
   is 134 B (digest) or 260 B (certificate). The Java engine's 300 B assumption is **2.000×** the
   measured PPDU and the Python engine's 919.5 µs constant is **2.131×** the measured frame. The
   2.05× split was not one engine being right; both were high.
7. The whole stack switched on costs **43 %** of what the default costs, because the first layer
   stops transmitting 69.7 % of the frames. Real ECDSA is the expensive one: **+46.3 %** of a
   baseline run on its own, and only 61 % of that is P-256 arithmetic.

---

## 0. The benches

Five scenarios, because "realistic density" and "a density that engages DCC" are not the same
question and neither is answered by a synthetic grid alone.

| bench | mobility | vehicles | radio | what it is for |
|---|---|---|---|---|
| **A — InTAS cold** | `sumo_replay`, `intas_300s_dt01.trace` (`5bfd4791…`), InTAS 0–300 s, SUMO 1.25.0 seed 42, step **0.1 s** | 334 (781 575 vehicle-steps) | `disc`, 709.4 m | the layer-by-layer matrix and the cost table |
| **B — InTAS AM, warm** | `sumo_replay`, `intas_amwarm_dt01.trace` (`cb1e6322…`), begin 25 200 s + **3 000 warm-up steps** → records 25 500–25 800 s; the engine arms run its **first 60 s** | 2 169 present, **1 427.2 concurrent** over the trace (4 281 540 rows / 3 000 steps), 1 teleport, 0 collisions | `disc`, 709.4 m | realistic urban density |
| **B′ — InTAS AM, cold** | `intas_am_dt01.trace` (`9725e2b8…`), begin 25 200 s, **no** warm-up | 1 189 (1 583 351 rows), 0 teleports | — | frozen first, then superseded by B: it enters an empty network and spends its first two minutes filling it. Recorded because **1 189 vehicles** is the dt = 0.1 twin of the published 1 188-vehicle AM run, which is where the fleet size in §5 comes from. No figure below is measured on it. |
| **C — flagship geometric** | the pinned `datasets/phase2_geometric/urban_osm_geometric` config, at dt = 1.0 and dt = 0.1 | 612 | `geometric`, 1 519 OSM buildings, 23 dBm / −81 dBm = **104 dB** | awareness |
| **D — density probe, fixed fleet** | no arrivals, all spawn at t = 0, 30 s at dt = 0.1, radio range 8 km so **every station hears every other** | N ∈ {60, 270, 400} | `disc` | a CBR sweep with density as a dial |
| **D′ — density probe, de-phased** | arrivals at 7.5/s over 120 s at dt = 0.1, same 6 × 6 grid, same 8 km range | 906 present (~330 concurrent) | `disc` | the same, without the phase-lock D suffers |

Bench A reproduces `datasets/py_intas_300s` exactly — 334 vehicles, 1 595 741 reports, 27
investigations, 27 revocations — and it is the **same SUMO realisation as the MOSAIC arm**
`gen_intas_urban_low`. Checked rather than assumed:

* the frozen trace records `net_sha256 = 9f16fd82…`, byte-identical to the `ingolstadt.net.xml`
  hash in the MOSAIC dataset's own manifest;
* MOSAIC drives `InTAS_full_poly.sumocfg` and the trace was frozen from `InTAS_buildings.sumocfg`;
  the two differ in **one line** — which polygon file is loaded as an `additional-file`, which is a
  visualisation input and touches no vehicle;
* both run SUMO 1.25.0 at a 100 ms step with `randomSeed 42` (`SCMS_SEED = 20260809` in the MOSAIC
  manifest is the attack seed, not SUMO's);
* both admit exactly **334 vehicles**, and their honest claimed-speed distributions agree to 0.14 %
  on the mean (16.427 vs 16.404 m/s) and 1 pp on the share above 40 m/s.

That is what makes the cross-engine CAM comparison in §1 like-for-like rather than approximately so.

Bench B is new. `InTAS_buildings.sumocfg` sets `step-length 0.1`, so **0.1 s is InTAS's own
calibrated integration step** and the dt = 1.0 traces this repository had were the re-integrated
ones, not the other way round. Freezing 25 200 s with a 300 s warm-up and recording the next 300 s
is the first artifact here that enters the AM peak on a loaded network: mean concurrency **1 427.2**
against **260.5** on bench A. The engine arms run its first 60 s, over which 1 052.6 stations are
transmitting per step; the concurrency is still climbing at the end of that window.

Seven tools were written for this work, all under `tools/`, none of them touching `src/`:

| tool | what it does |
|---|---|
| `protocol_stack_measure.py` | runs one named arm in its own process and records wall clock, CPU time, peak working set, the manifest's `counts["protocol"]` block, and the exact gap distribution rebuilt from the emission stream |
| `protocol_stack_report.py` | turns a directory of those records into the tables below |
| `cam_gap_analysis.py` | the gap distribution in the shape the Java engine published its own, with the honest/attacker split that comparison needs |
| `cam_rules_on_trace.py` | replays EN 302 637-2 (and two variants of the Java app's rule) straight over a frozen SUMO trace, with no engine in the way — this is what attributes §1 |
| `codec_size_probe.py` | sweeps 14 400 claims through the shipped ETSI codecs and turns each encoded length into airtime and CBR |
| `ecdsa_cost_probe.py` | times the P-256 primitives, the engine's own sign/verify paths and butterfly provisioning, then costs a stated fleet-hour from them |
| `awareness_shots.py` | re-evaluates the awareness block at the **measured** shot multiplicity beside the one `awareness.py` derives from `dt` |

---

## 1. CAM generation: the engines agree, and the 11 % they did not is two named causes

### What the Python engine now does

Bench A, `--cam-rules`, dt = 0.1, 211 231 CAMs over 210 897 gaps:

| | Python `--cam-rules` (all) | Python (honest only) | **MOSAIC** (honest, same scenario) | MOSAIC as published |
|---|---|---|---|---|
| mean gap | **0.302321 s** | **0.306104 s** | **0.335357 s** | 0.3399 s |
| rate | **3.3077 Hz** | **3.2669 Hz** | **2.9819 Hz** | 2.942 Hz |
| dynamics-triggered | **82.59 %** | — | — | 89.58 % |
| median | 0.3 s | 0.3 s | 0.3 s | 0.300 s |
| p05 / p25 / p75 / p95 / p99 / max | 0.1 / 0.2 / 0.4 / 0.7 / 1.0 / 1.0 s | 0.1 / 0.2 / 0.4 / 0.8 / 1.0 / 1.0 s | 0.1 / 0.2 / 0.4 / 1.0 / 1.0 / 1.1 s | 0.200 / 0.200 / 0.400 / 1.000 / 1.000 / 1.100 s |
| at `T_GenCamMin` (≤ 0.1 s) | **20.62 %** | 19.56 % | 5.39 % | 4.91 % |
| at `T_GenCamMax` (≥ 1.0 s) | 4.63 % | 4.76 % | 5.33 % | 5.51 % |

The two MOSAIC columns are the *same engine* read two ways: the third is
`datasets/mosaic_intas_urban_low_gate_fulltrace` re-measured with `tools/cam_gap_analysis.py`
(175 205 honest gaps), the fourth is the figure published in `CROSS-ENGINE-RADIO.md` §7 from a
different arm of the same scenario (182 106 honest gaps). They agree to 1.3 % on the mean gap and
0.5 pp on the floor share, which is the size of the "same scenario, different arm" noise the
cross-engine comparison carries.

Gap histogram, on the Java table's own bucket edges (shares):

| bucket | < 0.15 | 0.15–0.25 | 0.25–0.35 | 0.35–0.55 | 0.55–0.95 | 0.95–1.05 | ≥ 1.05 |
|---|---|---|---|---|---|---|---|
| Python, all | 0.2062 | 0.1994 | 0.3094 | 0.2183 | 0.0204 | 0.0463 | 0 |
| Python, honest | 0.1956 | 0.1959 | 0.3177 | 0.2233 | 0.0199 | 0.0476 | 0 |
| MOSAIC, honest | 0.0539 | 0.2641 | 0.3621 | 0.2432 | 0.0233 | 0.0532 | 0.0002 |

Triggers (engine counters, first match wins in the standard's own order): `position` 132 112,
`heartbeat` 36 441, `speed` 24 959, `heading` 17 385, `first` 334.

The last histogram column previews the second of the two causes below. The Python engine's longest
gap is **exactly 1.000 s** — `T_GenCamMax` is 1.0 and a station is evaluated at every 0.1 s step, so
nothing can exceed it. The Java engine has 27 gaps **beyond** 1.05 s and a 1.100 s maximum: a
heart-beat its own `dtLast >= CAM_INTERVAL_S` test rejected at 1.0 s and admitted one step later.

Without the rules the engine is exactly what it was: a flat **1.000 s** at dt = 1.0 (bench C: mean,
median and max gap all 1.0, 100 % of gaps at `T_GenCamMax`), and a flat **0.100 s** at dt = 0.1
(bench A base: 709 454 CAMs, every gap 0.1 s). So `--cam-rules` is the difference between one CAM
per simulation step and a CAM service.

The honest/all split is small — 1.2 % on the mean gap — and that is itself a check. The engine
evaluates clause 6.1.3 on the station's **true** ego state, not on the position it claims, so a
position-falsifying attacker cannot trigger itself faster by lying; if it could, the "all" column
would be dramatically faster than the honest one. It is not.

### The engines do not disagree by 11 %. They disagree by two implementation differences.

Rather than report the 0.302 vs 0.340 gap, `tools/cam_rules_on_trace.py` runs the **rules alone**
over the **same frozen trajectories** — no channel, no PKI, no detectors — under three variants:

| rule over `intas_300s_dt01.trace` (334 vehicles, 781 575 vehicle-steps) | CAMs | mean gap | rate | dynamics | ≤ 0.1 s | ≥ 1.0 s |
|---|---|---|---|---|---|---|
| `etsi` — clause 6.1.3 **with** the `N_GenCam` ratchet (what `src/` implements) | 258 601 | 0.302269 s | 3.3083 Hz | 82.56 % | 21.06 % | 4.72 % |
| `java` — `ScmsBeaconApp`'s flat heart-beat, everything else identical | 242 933 | 0.321772 s | 3.1078 Hz | 94.66 % | 17.74 % | 5.28 % |
| `java_fp` — the same, **plus the Java floor test's own arithmetic** | 227 203 | **0.344074 s** | **2.906352 Hz** | 94.29 % | **4.99 %** | 5.65 % |
| MOSAIC, as published | — | 0.3399 s | 2.942 Hz | 89.58 % | 4.91 % | 5.51 % |

`java_fp` lands within **1.2 % of the published mean gap** and within **0.08 pp of the published
`T_GenCamMin` share**, on trajectories that are not bit-identical to MOSAIC's own SUMO run. The
disagreement is therefore fully attributed:

**Cause 1 — `N_GenCam` is not implemented on the Java side.** Clause 6.1.3 says that after a
dynamics trigger `T_GenCam` becomes the elapsed interval and is held there for the next three CAMs,
so the *heart-beat* branch fires at the shortened interval too. `CamGenerationState` does that;
`ScmsBeaconApp.onVehicleUpdated` tests `dtLast >= CAM_INTERVAL_S` against a flat 1.0 s and emits no
shortened heart-beats at all. Worth **0.302 → 0.322 s** on the gap (−6.1 % on the rate) and
**12.1 pp** of the dynamics share — the Python engine's lower dynamics share is the extra
*heart-beats* the ratchet produces, not fewer dynamics triggers.

**Cause 2 — a floating-point defect in the Java floor test.** `ScmsBeaconApp` takes
`double tS = getSimulationTime() / 1e9` and tests `dtLast >= CAM_MIN_S` with no tolerance. Neither
`tS` nor `0.1` is exactly representable, and the difference of two consecutive 100 ms instants falls
**below** 0.1 for **1 814 of the 3 000 step pairs (60.47 %)** in a 300 s run, so the floor rejects a
CAM whose trigger had already fired. `CamGenerationState` guards this with a 1 ns epsilon. Worth
**0.322 → 0.344 s** on the gap (−6.5 % on the rate) and, decisively, **17.74 % → 4.99 %** of gaps at
`T_GenCamMin` — which is the whole of the Java engine's published 4.91 % figure.

That defect is directly visible in the Java engine's own data, in the population where the position
trigger fires every step — the A9 motorway traffic InTAS carries:

| gaps, honest, by the emitter's speed | 0–10 m/s | 10–20 | 20–40 | **> 40 m/s** |
|---|---|---|---|---|
| Python rate / mean gap / share at 0.1 s | 2.400 Hz / 0.4167 s / 12.8 % | 3.2551 Hz / 0.3072 s / 8.2 % | 5.2257 Hz / 0.1914 s / 8.7 % | **10.000 Hz / 0.1000 s / 100 %** |
| MOSAIC rate / mean gap / share at 0.1 s | 2.1284 Hz / 0.4698 s / 2.3 % | 3.0826 Hz / 0.3244 s / 0.9 % | 4.9576 Hz / 0.2017 s / 1.1 % | **6.3249 Hz / 0.1581 s / 41.9 %** |
| predicted by the fp defect alone | | | | **6.2208 Hz / 0.1608 s / 39.3 %** |

A vehicle above 40 m/s covers more than 4 m in 100 ms, so clause 6.1.3's position trigger fires on
every step and the station *should* emit at 10 Hz. The Python engine does. The Java engine realises
6.325 Hz — and an emitter that wants every step but is filtered by `tS − lastSendS >= 0.1` on
ns-derived doubles realises **6.2208 Hz, mean gap 0.1608 s, 39.25 % of gaps at 0.1 s**. Measured
against predicted: 1.7 % on the rate, 2.6 pp on the floor share.

This is not a scenario difference. The two fleets are the same fleet: honest claimed-speed
distribution mean **16.427 m/s (MOSAIC)** vs **16.404 m/s (Python)**, share above 40 m/s **10.01 %**
vs **10.98 %**, both over ~175 k honest CAMs.

### The rate at realistic density

`tools/cam_rules_on_trace.py` over bench B (2 169 vehicles, 1 427.2 concurrent, the warm AM peak):

| rule | CAMs | mean gap | rate | dynamics | ≤ 0.1 s | ≥ 1.0 s | CAMs per vehicle-step |
|---|---|---|---|---|---|---|---|
| `etsi` | 1 295 931 | **0.330396 s** | **3.026667 Hz** | 78.64 % | 17.12 % | 7.32 % | 0.3027 |
| `java_fp` | 1 149 931 | 0.372404 s | 2.685254 Hz | 91.26 % | 3.52 % | 8.70 % | 0.2686 |

The engine itself, run over the same warm AM-peak trace (1 253 vehicles admitted, rules + codec),
measures **3.054134 Hz, mean gap 0.327425 s, 78.72 % dynamics-triggered, 3.053 CAMs per 1 s window**
— the standalone rule replay's 3.0267 Hz / 0.3304 s / 78.64 % to within 0.9 %. The rule layer and the
engine that hosts it are measuring the same thing.

So across two InTAS windows the Python engine's ETSI rate is **3.03–3.31 Hz** against the Java
engine's measured **2.94 Hz**, and the residual is the two causes above. **The two engines agree.**

The duty cycle is the number a channel-load argument needs: the generation rules put **0.3027 CAMs
on the air per vehicle-step**, i.e. they remove **69.7 % of the offered frames** a one-CAM-per-step
engine at dt = 0.1 would have transmitted.

---

## 2. The awareness consequence: the shot multiplicity is fixed, the conclusions are not overturned

The awareness gate was measured with the engine at dt = 1.0, one CAM per step, so its shot
multiplicity was `Z = 1` — its awareness ratio *was* a per-packet PDR — while the Java engine at
2.94 Hz had `Z = 2`. Bench C re-measures the flagship geometric run (`urban_osm_geometric`, 612
vehicles, 1 519 OSM buildings, 104 dB budget) at dt = 0.1 with the rules on, against its own pinned
dt = 1.0 self.

### The multiplicity, counted rather than derived

`tools/awareness_shots.py` counts CAMs per station per whole 1 s window from the emission stream:

| | pinned, dt = 1.0 | dt = 0.1, no rules | **dt = 0.1 + `--cam-rules`** |
|---|---|---|---|
| CAMs per 1 s window: mean / median / p10 / p90 / max | 1.000 / 1 / 1 / 1 / 1 (48 576 windows) | 10.0 / 10 / 10 / 10 / 10 (40 761) | **2.1087** / 2 / 1 / 3 / 10 (43 533) |
| Z the engine actually had | **1.0** | 5.4579 (10, capped at the reference's fitted Z) | **2.1087** |
| Z `awareness.py` computes from `dt` (`min(floor(1/dt), 5.4579)`) | 1.0 | 5.4579 | **5.4579** |
| overstatement | none | none | **2.59×** |

So the multiplicity the earlier work called out is fixed — the Python engine now sits at
**Z = 2.11** against the Java engine's **Z = 2**, which is the like-for-like the cross-engine note
asked for. On bench A (InTAS, faster traffic) the same measurement gives **3.3121 CAMs per 1 s
window** (median 3, p90 5), and on bench B's AM peak **3.0527** (median 3).

**A defect this exposes, in `datagen/awareness.py`, not fixed here because it is under `src/`:**
`z_for_engine(dt_s, 1.0)` derives the multiplicity from the *configured step*, which was exact when
the engine emitted every step and is not exact under generation rules. At dt = 0.1 it reports
Z = 5.4579 where the run delivered 2.1087. The consequence is confined to `anchors[*].
nar_at_engine_rate`, and it is large there:

| anchor | per-packet PDR | NAR at Z = 1 (the old run) | **NAR at the measured Z = 2.109** | NAR at the config-derived Z = 5.458 |
|---|---|---|---|---|
| 100 m | 0.3253 | 0.3253 | **0.5638** | 0.8833 |
| 200 m | 0.0740 | 0.0740 | **0.1497** | 0.3426 |
| 300 m | 0.0314 | 0.0314 | **0.0651** | 0.1597 |

At 200 m the module as written would publish 0.3426 for a run that delivers 0.1497 — a **2.29×**
overstatement.

The blast radius is small and worth stating precisely. `z_used` reaches exactly two places:
`shot_multiplicity.z_engine_effective` and `anchors[*].nar_at_engine_rate`. It does **not** enter
`crossings_m`, and `_verdict` reads only `d_star`, the reference distance and the link-state mix. It
also never reaches the scorecard: `realism_bench.comm_panel` publishes no `nar_at_engine_rate` row.
So the defect mis-states one field of `awareness.py`'s own report and nothing that is gated.

### The comm panel, and the gate

Four arms on the same scene, so the `dt` change, the codec and the rules change are separated:

| `comm.*` | pinned, dt = 1.0 | dt = 0.1, no rules | dt = 0.1, no rules, + codec | **dt = 0.1 + rules + codec** |
|---|---|---|---|---|
| `nar90_equivalent_range_m` | 103.5 m | 97.2 m | 96.6 m | **101.5 m** |
| its Z-sensitivity band | 61.1 – 119.0 | 58.3 – 112.5 | 58.0 – 111.9 | 60.0 – 117.2 |
| verdict / ratio vs the reference's 87.1 m | consistent, 1.1884× | consistent, 1.1168× | consistent, 1.1094× | **consistent, 1.1664×** |
| `pdr_gray_zone_ratio` (gate ≥ 1.9191) | 3.5931 | 3.4631 | 3.4555 | **3.5491** |
| `pdr_gray_zone_width_m` | 114.86 | 122.08 | 126.39 | 134.33 |
| `effective_range_m` (PDR 0.50) | 87.75 | 90.90 | 92.20 | 92.94 |
| `pdr_absolute_200m` (per packet) | 0.0754 | 0.0605 | 0.0593 | 0.0740 |
| `link_state_los_fraction` overall / 200 m | 0.0339 / 0.0598 | 0.0298 / 0.0517 | 0.0293 / 0.0505 | 0.0337 / 0.0601 |
| `awareness_ratio_100/200/300m` | 0.4077 / 0.1102 / 0.0541 | 0.4238 / 0.1224 / 0.0438 | 0.4322 / 0.1336 / 0.0512 | 0.4434 / **0.1629** / 0.0791 |
| `cam_inter_packet_gap_p50_s` | 1.0 | 0.1 | 0.1 | **0.5** |
| `honest_links` (sample size) | 1 661 | 84 302 | 88 275 | 8 153 |
| measured Z | 1.000 | 10 → capped 5.4579 | 10 → capped 5.4579 | **2.1087** |

The largest single move in the gate quantity is the **step**, not the rules: dt = 1.0 → 0.1 costs
6.1 % of `nar90_equivalent_range_m` (103.5 → 97.2 m) and the rules give 5.0 % of it back
(97.2 → 101.5 m). Both moves are inside the metric's own Z-sensitivity band and neither changes the
verdict.

**The earlier awareness conclusions do not change.** `nar90_equivalent_range_m` is a crossing of the
engine's **per-packet** PDR curve at the per-packet level the reference's NAR 0.90 implies at the
*reference's own* Z — no engine rate enters it, and it moves 2 m, well inside its own 60–117 m
Z-sensitivity band. The verdict stays "consistent" on all four arms and the gray-zone ratio stays
far above its 1.9191 threshold on all four.

The link-state composition wobbles by up to 13 % across the four arms (LOS at 200 m 0.0598 → 0.0505
→ 0.0601), and that is **sampling, not scene**: the composition is built from co-presence snapshots
taken one per 1 s bucket, the pair budget caps at 400 000 classified pairs, and a 10 Hz emission
stream fills those buckets from different instants than a 1 Hz one. The scene — 1 519 building
footprints on a fixed road graph — is identical in all four.

What does change is every metric that counts *opportunities per second*:
`comm.awareness_ratio_200m` rises 47.8 % on an unchanged channel, purely because a 1 s bucket now
contains 2.1 shots instead of 1. That row is already ungated for exactly this reason, and this is
the first measurement that shows how much it moves for a reason that has nothing to do with
propagation.

*Recorded, not suppressed:* running the flagship's **internal** car-following at dt = 0.1 is not
free. `traffic.overlap_events` falls from **564 to 13** — a 43× improvement, though both are still
HARD fails against that metric's `<= 0` reference — while `traffic.accel_within_hard_bound_frac`
falls from **1.0 to 0.996897** and becomes a *new* HARD fail: per-step accelerations the 1 s step
used to average away. That is a property of the engine's own
mobility model at a step it was not calibrated at, and it is one more reason the CAM-rate claim in
§1 is made on the SUMO-backed benches, whose trajectories come from SUMO at the scenario's own
calibrated 0.1 s step.

---

## 3. DCC and latency: two layers that, at a real city's density, do nothing

### 3a. Where the CBR actually sits

| scene | offered rate | CBR mean | CBR max | receiver-steps |
|---|---|---|---|---|
| **A** InTAS 0–300 s, 334 veh (260.5 concurrent in the trace), rules on | 3.31 Hz | **0.018706** | 0.12082 | 638 508 |
| **A** the same at one CAM per step | 10 Hz | **0.082510** | 0.254585 | 712 159 |
| **B** InTAS AM peak, warm, first 60 s, **1 052.6 stations transmitting per step**, rules on | 3.05 Hz | **0.072776** | 0.39698 | 631 566 |
| **B** the same at one CAM per step | 10 Hz | **0.268962** | 0.677455 | 663 024 |
| **C** flagship geometric, dt = 0.1, one CAM per step | 10 Hz | 0.045219 | 0.33657 | 415 756 |
| **D** de-phased probe, ~330 concurrent all-in-range, rules on | 2.29 Hz | 0.314092 | 0.444445 | 337 321 |
| **D** fixed fleet, 270 / 400 all-in-range, rules on | 3.13 / 3.16 Hz | 0.349872 / 0.392874 | 1.000 / 1.000 | 81 000 / 120 000 |

The reactive-DCC first breakpoint is **0.30**. The only mean CBRs that reach it are the three
synthetic probes in the last two rows. At the busiest real-city density this repository can
produce — the InTAS AM peak entered warm, over a thousand vehicles transmitting per step in
Ingolstadt — a station running the ETSI generation rules measures a **mean CBR of 0.073**.

**The generation rules are what keep it there.** The same scene at one CAM per 0.1 s step measures
0.269 mean / 0.677 max. Clause 6.1.3 removes 69.7 % of the offered frames before TS 102 687 is
consulted at all, and the two congestion mechanisms are therefore largely redundant: the first one
takes the load out, and the second then has nothing to take.

### 3b. DCC on and off, at low density: bit-identical

Bench A, `--cam-rules --message-codec etsi_cam_en302637_2`, DCC off vs on:

| | DCC off | DCC on |
|---|---|---|
| CAMs | 211 231 | **211 231** |
| mean gap / rate | 0.302321 s / 3.3077 Hz | **0.302321 s / 3.3077 Hz** |
| CBR mean / max | 0.018706 / 0.12082 | **0.018706 / 0.12082** |
| DCC state occupancy | — | **`relaxed` 638 508 / 638 508 (100.00 %)** |
| `data_digest` | `3c9bdeab…` | **`3c9bdeab…`** |

Not "approximately the same": the same dataset, byte for byte. The same holds for the 60-vehicle
control in the density probe (`relaxed` 18 000/18 000, identical digest).

At the **AM peak** (bench B, 1 052.6 stations per step) DCC does leave the idle state, barely, and still
achieves nothing:

| bench B, warm AM peak, rules + codec | DCC off | DCC on |
|---|---|---|
| CAMs | 192 837 | 193 757 (+0.48 %) |
| mean gap / rate | 0.327425 s / 3.0541 Hz | 0.326593 s / 3.0619 Hz |
| CBR mean / max | 0.072776 / 0.39698 | 0.073066 / 0.409925 |
| DCC state occupancy | — | **`relaxed` 631 664 (99.79 %)**, `active_1` 1 313 (0.207 %), `active_2` 3 |

The CAM count moves *up* by 0.48 %, which is not the controller: with 1 313 station-steps in
`active_1` (a 0.200 s floor) and 17 % of gaps below 0.2 s, DCC can have suppressed **at most ~0.1 %**
of CAMs directly. What moved the count is the revocation feedback — a handful of delayed frames
changes which stations get revoked and therefore how many are still transmitting later. The digest
changes (`879e6148…` → `00a136f6…`); the traffic does not.

**Two structural facts worth stating separately**, because both make DCC inert for reasons that are
not about density:

1. **Without a codec, the CBR a `disc` or `logdistance` run measures is identically zero.**
   `LinkChannelModelBase.channel_busy_ratio` returns `0.0` and only `GeometricChannel` overrides it,
   so on the default radio the reactive controller reads 0 forever. Measured: `--cam-rules --dcc`
   with no codec reports CBR mean 0.000000, max 0.000000 over 638 508 samples and 100 % `relaxed`.
   Putting a codec on the path is what makes CBR a real measurement on those models, because the
   engine then integrates `frame_airtime_s(size)` itself.
2. **The first DCC state cannot bind a CAM service whose gaps are already long.** `active_1`
   (CBR 0.30–0.40) permits 5 Hz, i.e. `T_off = 0.200 s`. A separate 270- and 400-station probe run
   at the default 709.4 m range — CBR mean 0.110 / 0.114, max 0.40561 — put DCC in `active_1` for
   **13.4 %** and **19.4 %** of station-steps respectively, and both produced datasets **byte-identical
   to their DCC-off twins** (`a0164440…`, `07b53b08…`), because in that scene the CAM service's
   own minimum gap is 0.3 s and a 0.2 s floor is slack. DCC only starts removing frames at
   `active_2` (`T_off = 0.400 s`, CBR ≥ 0.40) and above.

### 3c. DCC where it does engage

Three probes past the breakpoint. All are synthetic and are labelled as such. The **de-phased**
one — vehicles arriving over 120 s rather than all spawning at t = 0 — is the honest one, and it is
the one to read:

| de-phased flow probe, 906 vehicles (~330 concurrent), all-in-range, rules on | DCC off | DCC on | Δ |
|---|---|---|---|
| CBR mean | 0.314092 | **0.298064** | **−5.10 %** |
| CBR max | 0.444445 | 0.746495 | +68.0 % |
| CAMs emitted | 77 514 | **73 908** | **−4.65 %** |
| mean gap / rate | 0.436015 s / 2.2935 Hz | **0.462917 s / 2.1602 Hz** | **−5.81 % rate** |
| gaps at `T_GenCamMin` (0.1 s) | 12 141 | **3 106** | **−74.4 %** |
| gaps at `T_GenCamMax` (1.0 s) | 3 815 | 3 941 | +3.3 % |
| DCC state occupancy | — | `relaxed` 56.4 %, `active_1` 29.4 %, `active_2` 8.5 %, `active_3` 3.9 %, `restrictive` 1.9 % | |
| MA reports | 105 700 | 95 726 | −9.4 % |
| `data_digest` | `6d33fb4a…` | **`dbe4c979…`** | changed |

The signature is unmistakable and is exactly what a floor does: the number of gaps at
`T_GenCamMin` falls by three quarters while the heart-beat tail barely moves. The rise in CBR *max*
is not a modelling artefact either — with DCC on, 9.4 % fewer reports means fewer revocations, so
more stations stay alive and the busiest instant is busier.

The two fixed-fleet probes are recorded for completeness, with their defect stated:

| fixed fleet, all-in-range | 270, DCC off | 270, DCC on | 400, DCC off | 400, DCC on |
|---|---|---|---|---|
| CBR mean | 0.349872 | 0.337484 (−3.5 %) | 0.392874 | 0.372630 (−5.2 %) |
| CBR max | 1.000 | 1.000 | 1.000 | 1.000 |
| CAMs | 25 350 | 24 822 (−2.08 %) | 37 925 | 37 261 (−1.75 %) |
| rate | 3.1301 Hz | 3.0617 Hz (−2.2 %) | 3.1609 Hz | 3.1028 Hz (−1.8 %) |
| gap histogram | 0.3 s ×20 196, 0.4 ×4 884 | 0.3 ×20 196, 0.4 ×2 178, **0.5 ×2 178** | 0.3 ×31 383, 0.4 ×6 142 | 0.3 ×31 383, 0.4 ×2 739, **0.5 ×2 739** |
| DCC states | — | `relaxed` 66.7 %, `restrictive` 33.3 % | — | `relaxed` 55.7 %, `active_1` 11.0 %, `restrictive` 33.3 % |

Their CBR is **bimodal**: every station spawns at t = 0, so their 0.3 s gaps phase-lock and one step
in three carries the whole fleet at CBR 1.000 while the other two carry none. A station alternates
between `restrictive` and `relaxed` — `restrictive` for exactly 33.3 % of station-steps in both,
which is the phase lock written out — barely visits the intermediate states, and the 1 s floor it
then imposes only ever delays a frame by one step before the channel goes quiet again. That is a
property of the probe, not of the controller, and it is why the de-phased arm above is the one to
read.

For comparison, the Java engine's own dense probe (`cc_dense_dcc0/1`, `gen_grid_12x12`, 720
vehicles) at CBR 0.379 mean: CBR −11.9 %, CAMs −11.7 %, rate −12.5 %, reports +2.8 %. The Python
effect on the de-phased probe (CBR −5.1 %, CAMs −4.7 %, rate −5.8 %, reports −9.4 %) is about half
of it, at a CBR 17 % lower and against a CAM service that was already emitting at 2.29 Hz rather
than the Java run's 2.24 Hz. The report direction differs in sign, and that is a real difference
worth stating: the Java probe *gained* evidence because its CSMA heuristic returned the removed
airtime as deliveries; this engine's `disc` model has no contention term to return, so a suppressed
CAM is simply a CAM that was never heard.

### 3d. Latency

`--net-latency` replaces the engine's uniform `U(0, 2.0) s` report-ingest delay with
`d/c + AIFS + E[backoff]/(1 − CBR) + PPDU(size) + stack`, deterministically. Measured over every
delivered frame:

| arm | frames | mean | min | p50 | p90 | p99 | max |
|---|---|---|---|---|---|---|---|
| A, 10 Hz + codec | 11 224 744 | **4.9274 ms** | 4.9124 | 4.9250 | 4.9430 | 4.9460 | 4.9477 |
| A, rules + codec | 2 774 668 | 4.9163 | 4.9124 | 4.9160 | 4.9180 | 4.9220 | 4.9277 |
| A, the full stack (rules + codec + DCC + ECDSA) | 2 799 813 | 4.9631 | 4.9124 | 4.9170 | 5.0850 | 5.0890 | 5.0975 |
| **B, AM peak, rules + codec + DCC** | 9 052 442 | 4.9241 | 4.9124 | 4.9220 | 4.9320 | 4.9480 | 4.9821 |
| reference: DLR Cohda MK5, ITS WC 2021 (`v2x_awareness.latency_p50_ms_80211p`) | | | **5.0 – 9.0 ms** | | | | |

**The whole modelled spread is 35 µs at InTAS density and 70 µs at the AM peak — 0.7 % and 1.4 % of
the mean.** At either load the latency model is a constant, and the constant sits just below the
reference band's lower edge.

Against the refdata profile, term by term — every component is a pinned entry of
`phy_80211p_profile.json` except the last:

| term | source | value at a 134 B frame |
|---|---|---|
| propagation `d/c` | — | 0.0000 – 0.0024 ms (0 – 709.4 m) |
| AIFS(AC_BE) | `mac_overhead_us.aifs_ac_be_us` | 0.1100 ms |
| mean initial backoff `E[backoff]/(1 − CBR)` | `mac_overhead_us.mean_backoff_us` = 97.5 µs, scaled | 0.0975 – 1.9500 ms |
| PPDU airtime | `frame_airtime_us`, evaluated at the measured MPDU | 0.2240 ms |
| facilities + networking + security stack | `etsi_rules.STACK_LATENCY_S`, derived from `v2x_awareness.latency_p50_ms_80211p` | 4.4805 ms (constant) |
| **total, idle channel** | | **4.9120 ms** |

The last row is the only one that is not a refdata constant, and it is 91 % of the total. That is
the honest shape of this model: **the air interface contributes 8.8 % of the modelled latency and
the stack constant contributes the rest**, which is exactly what the reference entry's own note
predicts ("the measured 5–9 ms is dominated by stack/queueing, not by the air interface").

That the mean sits below the band is not an error, and the arithmetic says why. `STACK_LATENCY_S`
was derived so that a **200 B** frame on an idle channel lands on the band's own 5 ms anchor:

| MPDU | idle | CBR 0.27 (bench B, 10 Hz) | CBR 0.60 | CBR 0.95 (the model's cap) |
|---|---|---|---|---|
| 41 B (unsigned) | 4.7920 ms | 4.8281 | 4.9383 | 6.6445 |
| **134 B (digest CAM)** | **4.9120 ms** | 4.9481 | 5.0583 | 6.7645 |
| 168 B (the full stack's measured mean) | 4.9600 | 4.9961 | 5.1062 | 6.8125 |
| **200 B (the reference's own anchor)** | **5.0000 ms** | 5.0361 | 5.1463 | 6.8525 |
| 260 B (certificate CAM) | 5.0800 | 5.1161 | 5.2263 | 6.9325 |
| 800 B | 5.8000 | 5.8361 | 5.9463 | 7.6525 |

The model reproduces the anchor exactly at the payload the anchor was measured at, and falls **1.8 %
below it** at the payload a real ETSI CAM actually is. Its reachable envelope is **4.79 – 7.65 ms**;
it cannot produce the band's 9 ms tail at any load, because that tail belongs to an 800–1000 B
payload and a stack-latency *distribution* this repository does not have a measurement of. The
propagation term is 2.4 µs at the 709.4 m range cap — 0.05 % of the total, and included anyway
because a latency model that omits propagation is not a latency model.

Unlike DCC, the latency model **does** change the data at every density: it moves the report-ingest
delay from a mean of 1.0 s to a mean of 4.93 ms, and bench A's report count moves 1 595 741 →
1 637 589 (+2.6 %) with revocations 27 → 26.

---

## 4. Codec size: the payload is a constant, and both engines' airtime constants are wrong

`tools/codec_size_probe.py` sweeps **14 400 claims** through `EtsiCamCodec` — four position frames,
six speeds including `unavailable`, five headings, five accelerations, four position confidences,
three vehicle-dimension cases, both station types — and encodes each one with `asn1tools 0.169.0`
UPER.

**Every one of the 14 400 encodes is 41 octets.** There is no size distribution: EN 302 637-2's
CAM with a `basicVehicleContainerHighFrequency` has no optional field the engine varies and every
constrained INTEGER is fixed-width, so the UPER length is a point mass. The engine's own run agrees:
bench A's `codec` arm encoded 751 424 CAMs for 30 808 384 payload octets — **41.000 B exactly**.

| PDU | payload octets |
|---|---|
| CAM, vehicle / VRU | **41** |
| CAM, RSU (`rsuContainerHighFrequency`) | 26 |
| DENM | 43 |
| VAM | 34 |

The variation on the wire is the TS 103 097 signer arm, and only that:

| signer | envelope B | MPDU B | OFDM symbols | PPDU | **frame (PPDU + AIFS + mean backoff)** | CBR per station at 10 Hz |
|---|---|---|---|---|---|---|
| `none` | 0 | 41 | 8 | 104.0 µs | 311.5 µs | 0.003115 |
| `digest` | 93 | **134** | 23 | 224.0 µs | **431.5 µs** | 0.004315 |
| `certificate` | 219 | **260** | 44 | 392.0 µs | **599.5 µs** | 0.005995 |

### What that does to the 2.05× disagreement

| | assumed MPDU | airtime | vs measured |
|---|---|---|---|
| Java (`Dcc.java`, `SignedCam.java`) | 300 B, PPDU only | 448.0 µs | **2.000× the measured digest PPDU** (224.0 µs); 2.239× the measured MPDU |
| Python (`run.py::PHY_FRAME_AIRTIME_S`) | ~500 B + 207.5 µs MAC | 919.5 µs | **2.131× the measured digest frame** (431.5 µs) |
| ratio between the two assumptions | | | 2.0525× |

The 2.05× split was not one engine being right. **Both were high by almost the same factor, and
they disagreed with each other only because one of them omitted the MAC overhead.** Compared like
for like: PPDU against PPDU the Java assumption is **2.000×** the measured 224.0 µs; frame against
frame the Python assumption is **2.131×** the measured 431.5 µs. The coincidence that the Java figure
(448.0 µs, PPDU only) is within 4 % of the measured *frame* time (431.5 µs, PPDU + MAC) is exactly
that: two errors of opposite sign that nearly cancel, and they cancel only at this one frame size.

### The refdata constants that should be corrected

`datagen/refdata/phy_80211p_profile.json` is arithmetically correct everywhere; what is wrong is the
**assumed MPDU size** its guidance and its derived CBR table are built on.

1. **`frame_airtime_us.derivation`** says *"a signed CAM with an attached certificate lands in the
   300–500 B band, which is the row to use for channel-load work."* Measured, a certificate-attached
   CAM is **260 B** and a digest-signed one is **134 B**. Both are below the band, and the 500 B row
   — the one `PHY_FRAME_AIRTIME_S` was built from — is **3.73×** the real digest frame's length. The
   table should gain rows at 134 and 260 B and the guidance should name them.
2. **`cbr_from_load.points`** is evaluated only at 300 B and 500 B. At the measured sizes, and
   counting MAC overhead as that entry's second column does:

   | | 300 B (pinned) | 500 B (pinned) | **134 B measured** | **260 B measured** |
   |---|---|---|---|---|
   | CBR, 80 veh @ 10 Hz, PPDU only | 0.3584 | 0.5696 | **0.1792** | **0.3136** |
   | CBR, 80 veh @ 10 Hz, + MAC overhead | 0.5244 | 0.7356 | **0.3452** | **0.4796** |
   | vehicles for CBR 0.60 @ 10 Hz, PPDU only | 133.93 | 84.27 | **267.86** | **153.06** |
   | vehicles for CBR 0.60 @ 10 Hz, + MAC overhead | 91.53 | 65.25 | **139.05** | **100.08** |
   | CBR, 80 veh @ the measured 3.0267 Hz, + MAC | — | — | **0.1045** | **0.1452** |

   (The pinned entry's two inversion rows, 133.93 and 84.27, are PPDU-only; the with-MAC column is
   computed here for the comparison and is not in the file.)

3. **The ROADMAP Phase-2 gate** — *"at ≥ 80 veh in radio range with DCC off, modeled CBR ≥ 0.55"* —
   is unreachable on the measured frame. 80 stations at 10 Hz on 134 B frames offer CBR **0.345**
   even with AIFS and backoff counted; on 260 B frames, **0.480**. The entry's own note already said
   the gate "is met at 80 vehicles only if MAC overhead is counted … or if the frame is a
   realistically-sized signed CAM of ~500 B". The frame is not ~500 B. It is 134 B, and the gate
   needs restating against a real one.
4. **`run.py::PHY_FRAME_AIRTIME_S = 919.5 µs`** is what a run *without* a codec still charges. It
   over-states the digest frame by **2.131×**, so every codec-less CBR this engine reports is high by
   that factor.

### What the real length does to the channel

On the `disc` and `logdistance` radios it does nothing to the *data*: `collision_loss` is the base
class's `0.0` there, so wire size is bookkeeping. Bench A's `base`, `codec`, `codec_cert` and
`codec_nosig` arms — 41 B, 134 B and 260 B frames — all produce the **identical** `data_digest`
`dbe5b061…`.

On the `geometric` radio, which is the one that implements `collision_loss`, it does. Bench C at
dt = 0.1, the same scene, codec off versus on:

| | no codec (charges 919.5 µs) | codec (charges 431.5 µs) | ratio |
|---|---|---|---|
| **CBR mean** | **0.096178** | **0.045219** | **2.1269×** |
| CBR max | 0.80916 | 0.33657 | 2.4041× |
| CAMs transmitted | 415 159 | 415 756 | |
| MA reports | 258 534 | **271 659** | **+5.08 %** |
| `data_digest` | `5d5515c6…` | **`1029ddd0…`** | |
| `nar90_equivalent_range_m` | 97.2 m | 96.6 m | |

The codec-less arm here is `--protocol-profile etsi_its_g5` with **no layer on**, which is
byte-identical to the plain baseline (same digest, same 415 159 frames, same 258 534 reports) and
exists only so the run publishes the CBR it was already measuring. Its mean CBR is **2.1269×** the
codec arm's, against the **2.1309×** the two constants predict — the 0.2 % residual is the feedback
loop closing (a lower CBR means less collision loss, so slightly more traffic is delivered).

Correcting the frame length is therefore not a cosmetic change to a manifest field: it halves the
modelled channel occupancy, which moves the collision term, which moves deliveries, which moves the
misbehaviour evidence by 5 %.

### The signer mix on the wire, under the real attachment rule

With `--security-model ecdsa` the arm is no longer a config field: TS 103 097 clause 7.1 attaches a
certificate at most once per second and signs with the 8-octet digest between. The measured mean
frame is therefore a **mixture**, and its mix is set by the CAM rate:

| arm | CAM rate | mean wire bytes | implied certificate share | mean frame airtime |
|---|---|---|---|---|
| `--message-signer digest` (config) | 10 Hz | 134.000 | 0 % | 431.5 µs |
| `--message-signer certificate` (config) | 10 Hz | 260.000 | 100 % | 599.5 µs |
| `--security-model ecdsa` + codec | 10 Hz | **147.536** | **10.7 %** | 455.5 µs |
| the full stack (rules + ECDSA + codec) | 3.31 Hz | **168.092** | **27.1 %** | 479.5 µs |

The slower the CAM service, the larger the share of frames that carry a certificate, and the heavier
the average frame: **14 % heavier at 3.31 Hz than at 10 Hz**, and 25 % heavier than the pure-digest
frame a config-set signer would charge. That is a coupling between the generation rules and the
security envelope that neither engine's fixed 300 B or 500 B constant can express — and it runs the
opposite way from intuition, because slowing the CAM service down makes each surviving frame cost
more air.

---

## 5. Cost

Bench A (InTAS 300 s, 334 vehicles, dt = 0.1), one arm per process, serial, on a 16-core Windows
Server 2022 box, Python 3.12.10. **CPU time** is the headline because it is what survives
contention; on every arm here `wall − cpu < 0.1 s`, so the two are interchangeable. Peak working set
is `K32GetProcessMemoryInfo(PeakWorkingSetSize)` for the arm's own process.

**Every arm was measured at least twice, and each row is the MINIMUM.** That rule is forced by the
box: another agent was working in this same tree throughout, running the full test suite on and off
(see the note at the top), and a competing job can only *add* CPU time to a process — it steals
cache and memory bandwidth and never gives any back. The three independent `base` readings say
exactly how much company there was:

| `base`, same config, same digest | CPU s | wall s |
|---|---|---|
| first reading, 13:51 | 259.516 | 259.552 |
| second reading, 15:04 | **259.375** | 259.381 |
| third reading, 16:24 (co-tenant active) | 335.047 | 335.111 |

The first two agree to **0.05 %**, so the instrument is precise; the third is **+29.2 %** with a
`pytest` run alongside it. Measured co-tenancy penalties elsewhere in the pass: `full` 112.062 →
131.578 s (**+17.4 %**) with three of my own arms in flight, `ecdsa` 379.516 → 490.906 s
(**+29.4 %**), `latency` 345.125 → 451.750 s (**+30.9 %**). The last column of the table below gives
the reading count and the spread, so any row whose spread is large is visibly a bound rather than a
measurement — and the three rows with a single reading (`report_v1`, `profile_explicit`,
`report_ts103759`) are upper bounds by the same argument, not tight numbers.

| arm | layers on | CPU s | peak RSS MiB | frames on air | µs/frame | × base | readings / spread |
|---|---|---|---|---|---|---|---|
| `base` | none (the default) | **259.4** | 2 314 | 709 454 | 365.6 | 1.00 | 3 / 29.2 % |
| `cam` | `--cam-rules` | **50.6** | 652 | 211 231 | 239.4 | **0.19** | 2 / 0.7 % |
| `cam_dcc_nocodec` | `--cam-rules --dcc`, no codec | **50.8** | 653 | 211 231 | 240.6 | 0.20 | 2 / 1.3 % |
| `cam_codec` | `--cam-rules` + codec | **67.0** | 672 | 211 231 | 317.3 | 0.26 | 2 / 1.1 % |
| `cam_codec_dcc` | `--cam-rules --dcc` + codec | **68.1** | 672 | 211 231 | 322.4 | 0.26 | 2 / 0.6 % |
| `cam_latency` | `--cam-rules --net-latency` + codec | **76.1** | 676 | 212 220 | 358.4 | 0.29 | 2 / 0.9 % |
| **`full`** | **rules + codec + DCC + latency + ECDSA** | **112.1** | **664** | 212 098 | 528.4 | **0.43** | 2 / 17.4 % |
| `report_v1` | `--report-format ma_report_v1` | 266.6 | 2 314 | 709 454 | 375.8 | 1.03 | 1 |
| `profile_explicit` | codec + `--protocol-profile etsi_its_g5` | 309.9 | 2 334 | 709 454 | 436.8 | 1.19 | 1 |
| `codec` | codec (`etsi_cam_en302637_2`, digest) | **310.1** | 2 334 | 709 454 | 437.1 | 1.20 | 2 / 10.8 % |
| `codec_nosig` | codec, `--message-signer none` | 311.5 | 2 334 | 709 454 | 439.0 | 1.20 | 2 / 18.2 % |
| `codec_cert` | codec, `--message-signer certificate` | 332.7 | 2 334 | 709 454 | 468.9 | 1.28 | 2 / 11.4 % |
| `report_ts103759` | codec + `--report-format ts103759_shape` | 340.5 | **2 794** | 709 454 | 479.9 | 1.31 | 1 |
| `latency` | codec + `--net-latency` | 345.1 | 2 381 | 712 159 | 484.6 | 1.33 | 2 / 30.9 % |
| `ecdsa` | `--security-model ecdsa` | **379.5** | 2 144 | 712 302 | 532.8 | **1.46** | 2 / 29.4 % |
| `ecdsa_codec` | codec + `--security-model ecdsa` | **447.9** | 2 167 | 712 302 | 628.8 | **1.73** | 2 / 19.9 % |

Read the `× base` column carefully: **the generation rules make the engine 5.1× cheaper, not
dearer**, because the first thing they do is stop transmitting 69.7 % of the frames a
one-CAM-per-step run at dt = 0.1 transmits — and they take **1 661 MiB of peak working set** with
them, because the memory is the report stream, not the layer. The per-frame column is what isolates
a layer's own cost.

The `full` row is every layer at once, and it costs **43 % of the default**. On this bench its DCC
sat in `relaxed` for **640 016 of 640 016** station-steps, so one of the five layers it pays for did
nothing at all. Its wire mix is the real TS 103 097 alternation (168.092 B mean), its verification
fan-out 12.66 over 2 795 253 logical verifications, its latency 4.9631 ms mean.

### Per-layer marginal cost

Each row is a difference between two arms that differ in exactly one layer.

| layer | measured as | Δ CPU | Δ per frame | Δ peak RSS |
|---|---|---|---|---|
| EN 302 637-2 generation rules | `cam` − `base` | **−208.8 s (−80.5 %)** | −126.2 µs | −1 661 MiB |
| ASN.1 UPER codec, at 10 Hz | `codec` − `base` | +50.8 s (+19.6 %) | **+71.5 µs** | +21 MiB |
| ASN.1 UPER codec, on the rules arm | `cam_codec` − `cam` | +16.4 s (+32.5 %) | **+77.8 µs** | +20 MiB |
| reactive DCC, with a codec | `cam_codec_dcc` − `cam_codec` | +1.1 s (+1.6 %) | +5.1 µs | −1 MiB |
| reactive DCC, without a codec | `cam_dcc_nocodec` − `cam` | +0.2 s (+0.5 %) | +1.2 µs | +1 MiB |
| per-packet latency, at 10 Hz | `latency` − `codec` | +35.0 s (+11.3 %) | +47.5 µs | +47 MiB |
| per-packet latency, on the rules arm | `cam_latency` − `cam_codec` | +9.0 s (+13.5 %) | +41.1 µs | +4 MiB |
| **real ECDSA, no codec** | `ecdsa` − `base` | **+120.1 s (+46.3 %)** | **+167.2 µs** | −169 MiB |
| **real ECDSA, on top of the codec** | `ecdsa_codec` − `codec` | **+137.8 s (+44.4 %)** | +191.7 µs | −168 MiB |
| certificate signer vs digest | `codec_cert` − `codec` | +22.6 s (+7.3 %) | +31.8 µs | 0 MiB |
| no signer vs digest | `codec_nosig` − `codec` | +1.3 s (+0.4 %) | +1.9 µs | −1 MiB |
| `--protocol-profile` declaration | `profile_explicit` − `codec` | **−0.2 s (−0.1 %)** | −0.3 µs | −1 MiB |
| `--report-format ma_report_v1` | `report_v1` − `base` | +7.2 s (+2.8 %) | +10.2 µs | 0 MiB |
| `--report-format ts103759_shape` | `report_ts103759` − `codec` | +30.3 s (+9.8 %) | +42.8 µs | **+460 MiB** |
| the whole stack | `full` − `base` | **−147.3 s (−56.8 %)** | +162.8 µs | −1 650 MiB |

The codec's own cost is the row to trust twice: measured against the 10 Hz baseline it is
**+71.5 µs per frame**, measured against the rules arm — a completely different frame count and a
different arm pair — it is **+77.8 µs per frame**. Two independent estimates of the same quantity,
9 % apart.

The two negative RSS deltas are not the layers saving memory: peak working set on this bench is
dominated by the misbehaviour-report stream, and any layer that changes how many reports are
produced moves it more than the layer itself does. `--report-format ts103759_shape` is the one
whose memory cost really is its own — **+460 MiB** of evidence octets held to put in the reports.

### Real ECDSA

Measured two independent ways — the primitives, and the run that uses them — and then reconciled
against each other.

**(1) The primitives**, `tools/ecdsa_cost_probe.py` on an idle box, `cryptography` 50.0.1,
`SIGNING_MODE = rfc6979` (deterministic nonces, so runs replay):

| operation | µs | per second |
|---|---|---|
| raw P-256 **sign** (`ecdsa_p256.SigningKey.sign`) | **16.362** | **61 119** |
| raw P-256 **verify** (`VerifyingKey.verify`) | **35.293** | **28 334** |
| `SecurityLayer.sign` — header, TS 103 097 signer alternation, sign | **19.495** | **51 295** |
| `SecurityLayer.verify`, **cold** — certificate, validity window, CRL, verify | **38.296** | **26 112** |
| `SecurityLayer.verify`, **memo hit** — SHA-256 tag lookup, no curve | **2.928** | **341 562** |
| butterfly provisioning, 1 certificate per device | 271 | 3 696 devices/s |
| butterfly provisioning, 20 certificates per device | 4 136 | 242 devices/s |

Verification is **2.16× more expensive than signing** on this curve and backend, which is the shape
that makes the receive path the whole bill. The memo is **13.1×** cheaper than a cold verify. Two
independent runs of the probe agree to 1.1 % on every row.

**(2) The engine**, bench A `--security-model ecdsa` over 300 s and 334 vehicles:

| | measured |
|---|---|
| signatures computed | 746 709 |
| logical verifications (what a real receiver would compute) | 11 242 545 |
| computed verifications (after the memo) | 738 374 |
| **fan-out** (logical / computed) | **15.06** |
| cache hit rate | 0.934323 |
| verdicts | `ok` 11 111 143, `signature_invalid` 131 402, `cert_revoked` 4 439 |
| attacks the PKI **refused to let an attacker mount** | `ExpiredCert` 3 715, `NotYetValid` 6 311 |
| CPU added over the same run without it | **+120.1 s (+46.3 %)** |

**Do the two agree?** Costing the engine's own counts with the primitives' own rates:

| | count | × µs | = |
|---|---|---|---|
| `SecurityLayer.sign` | 746 709 | 19.495 | 14.56 s |
| `SecurityLayer.verify`, computed | 738 374 | 38.296 | 28.28 s |
| `SecurityLayer.verify`, memo hits | 10 504 171 | 2.928 | 30.76 s |
| butterfly provisioning | 334 devices | 271 | 0.09 s |
| **predicted** | | | **73.7 s** |
| **measured** | | | **120.1 s** |

**1.63×.** The primitives account for 61 % of what the layer costs; the remaining **46.4 s** is
engine-side security bookkeeping the microbenchmark does not include — `SignedBroadcast`
construction, per-verdict tallying, certificate validity and CRL lookups on every delivered frame,
and (on this codec-less arm) building the canonical bytes to sign. Stating the ratio is the point:
a reader sizing a fleet from the primitive rates alone would under-count by a third.

### The 1 188-vehicle hour

Arithmetic over the measured rates, not an extrapolated run. Fan-out 15.06 as measured above.

| | at 10 Hz (one CAM per 0.1 s step) | at the measured 3.0267 Hz (generation rules) |
|---|---|---|
| CAMs signed | 42 768 000 | 12 944 590 |
| delivered links (× fan-out 15.06) | 644 086 080 | 194 945 533 |
| butterfly provisioning, one certificate each | 0.3 s | 0.3 s |
| signing | 833.8 s | 251.9 s |
| verification **as this engine computes it** (memoised) | 1 637.8 s | 496.1 s |
| verification **as 1 188 real receivers would compute it** | 24 665.9 s | 7 471.9 s |
| total, primitives only, this engine | 2 471.9 s = 41 min | 748.4 s = 12.5 min |
| **the same × the 1.63 engine overhead measured above** | **4 029 s = 67 min** | **1 220 s = 20 min** |
| **total, a real fleet** (primitives only) | **25 500 s = 7.08 h** | **7 724 s = 2.15 h** |
| real-time factor, this engine, with the 1.63× | **1.12×** | **0.339×** |
| cores for real time, a real fleet | **6.85** | **2.08** |

The 1.63 multiplier is carried into the engine rows because §5 just measured it there; it is **not**
carried into the `real fleet` rows, because a real on-board unit's own bookkeeping is not this
engine's and nothing here measures it. Those rows are the P-256 arithmetic and nothing else.

**The generation rules cut the PKI bill by 70 %**, for the same reason they cut everything else: at
3.0267 Hz there are 3.3× fewer frames to sign and 3.3× fewer to verify. A 1 188-vehicle hour of ETSI
traffic is **20 minutes of this box's CPU** with the stack's own memoisation, and **2.15 core-hours**
of pure curve arithmetic if every receiver really did the work. At 10 Hz the same hour crosses
real time (**1.12×**): the engine could not keep up with a 1 188-vehicle fleet emitting at 10 Hz on
one core, and can keep up comfortably at the ETSI rate.

Two honesty notes on the memo. `VerificationCache` memoises a pure function of
`(key, message, signature)`, so it cannot change a verdict — it changes *who pays*. One CAM is
signed once and verified by every receiver that hears it, so this process computes one verification
per **transmitted frame** while a fleet computes one per **(frame, receiver)** pair; the `real
fleet` rows are the ones a CPU-exhaustion or signature-flooding study must use, and the engine
publishes both counts in the manifest so neither can be mistaken for the other. And the fan-out is
the table's one soft input: 15.06 measured on bench A's 334-vehicle scene, against 14.14 measured on
the 1 188-vehicle AM run at dt = 1.0 — using that instead would cut the `real fleet` rows by 6.1 %.

---

## 6. What to switch on

| layer | what it buys, measured | what it costs | verdict |
|---|---|---|---|
| **`--cam-rules`** (EN 302 637-2) | the only layer that changes the traffic's *shape*: 10 Hz → **3.03–3.31 Hz**, 78.6–82.6 % dynamics-triggered, agreeing with the Java engine once its two defects are accounted for. Sets the awareness shot multiplicity to a measurable number (Z = 2.11 flagship, 3.31 InTAS) instead of 1 or 10. Requires dt ≤ 1.0 and is a no-op at dt = 1.0. | **−80.5 % CPU** — it is the cheapest thing in the file, because it stops transmitting 69.7 % of the frames | **on**, whenever dt < 1.0 |
| **`--message-codec etsi_cam_en302637_2`** | the only source of a real frame length (**41 B payload, 134/260 B on the wire**), which corrects a **2.13×** over-statement of CBR and makes CBR a real measurement at all on the `disc` and `logdistance` radios. On `geometric` it moves deliveries and MA evidence by **+5.1 %**. | **+71.5 - 77.8 us/frame** (+19.6 % at 10 Hz, +32.5 % on the rules arm), **+21 MiB** | **on** whenever channel load, CBR or airtime is part of the question |
| **`--dcc`** (TS 102 687) | at InTAS density: **nothing, byte for byte**. At the AM peak: `relaxed` 99.79 % of station-steps, ≤ 0.1 % of CAMs suppressible. Needs a synthetic ~330-station all-in-range probe before it removes 4.65 % of CAMs. | **+1.6 % CPU** with a codec, **+0.5 %** without | **off** unless a congested channel is the object of study |
| **`--net-latency`** | replaces `U(0, 2.0) s` report ingest with a deterministic **4.91–5.10 ms**, reproducing the DLR 5 ms anchor exactly at the 200 B payload it was measured at. Changes the data at every density (+2.6 % reports on bench A). Draws no random number. | **+41 - 48 us/frame** (+11.3 % at 10 Hz, +13.5 % on the rules arm), +47 MiB | **on** |
| **`--security-model ecdsa`** | `sig_ok` becomes a P-256 verification result: 11 111 143 `ok`, 131 402 `signature_invalid`, 4 439 `cert_revoked` on bench A, and two attack classes become *refusals* an attacker cannot mount (`ExpiredCert` 3 715, `NotYetValid` 6 311) instead of edited flags. Also makes the wire size a real TS 103 097 mixture. | **+167 - 192 us/frame** (+46.3 % alone, +44.4 % on top of the codec) | **on** when the study is about the PKI; it is the single most expensive layer |
| `--protocol-profile etsi_its_g5` | a declaration, not behaviour: with the codec on it runs **309.9 s** against the codec arm's own **310.1 s** and produces the **byte-identical** dataset | **−0.1 %**, i.e. nothing | free; use it so the manifest names the stack |
| `--report-format ma_report_v1` | the historic row through the seam. Byte-identical dataset (`dbe5b061…`), identical 1 595 741 reports | **+2.78 % CPU, +0 MiB** | free enough |
| `--report-format ts103759_shape` | reports stop *referring* to evidence and start *carrying* it: the real UPER octets go into `v2xPduEvidence`, so the digest legitimately changes (`a6599af6…`) | **+9.8 % CPU and +460 MiB over the codec arm** (+31.3 % and +480 MiB over the bare baseline) | the only seam that is not free — pay it when the report shape is the object of study |

### What turned out not to matter

1. **Reactive DCC, at every real density this repository can produce.** InTAS at 260.5 concurrent and
   InTAS's AM peak at 1 052.6 transmitting per step both leave the controller in `relaxed` for ≥ 99.79 % of
   station-steps, and at the lower density the DCC-on and DCC-off datasets are byte-identical. This
   is not a bug: it is the correct behaviour of a controller whose first breakpoint is CBR 0.30 in a
   channel whose measured CBR is 0.073.
2. **The order in which the two congestion mechanisms act.** The generation rules remove 69.7 % of
   the load, which is what puts the CBR below DCC's breakpoint. Turning DCC on *without* the rules
   is refused by the engine, correctly; turning both on gets you one working mechanism and one idle
   one.
3. **The signer arm, on the default radio.** 41 B, 134 B and 260 B frames give the identical
   `data_digest` on `disc`, because that model's `collision_loss` is 0. Frame size only reaches the
   data through `geometric`.
4. **The frame-size *distribution*.** There is not one. A CAM is 41 octets, always. Everything that
   varies is the security envelope, and it takes exactly two values.
5. **The latency model's spread, at realistic density.** 35 µs across 11.2 M frames — 0.7 % of the
   mean. What the layer buys is the *level* (5 ms instead of a 1 s uniform draw), not a distribution.

### What this does not show

* Every CBR number here comes from the zeroth-order estimator `phy_80211p_profile.cbr_from_load`
  pins, evaluated on real frame lengths. It counts every frame in range as heard and adds airtime
  linearly, so it over-counts overlap at high load. The Sepulcre et al. analytical model that
  PHASE2-DESIGN step 6 binds is still not implemented.
* The CBR numerator is frames **decoded**, not frames **sensed** (`CROSS-ENGINE-RADIO.md`
  divergence 11), and that bias is still open — but it does **not** apply to the benches the DCC
  conclusion rests on. Benches A and B run `radio_model = disc`, where `evaluate_link` returns
  DELIVERED for every frame inside 709.4 m and the loss composition is applied afterwards, so the
  CBR numerator is every frame inside the disc: the same convention the Java engine's own CBR uses.
  The bias applies to bench C (`geometric`), where a frame must clear the −81 dBm floor to be
  counted.
* If anything the disc benches **over**-state a real urban CBR: bench C, an OSM extract of
  Ingolstadt's core with TR 37.885 propagation and 1 519 real building footprints, measures
  0.045219 mean at 10 Hz against bench B's 0.268962 at the same rate, because 95 % of urban links
  are building-blocked and never arrive at all. The "DCC does nothing" conclusion is therefore
  measured on the more generous of the two channels.
* Nothing here re-measures propagation, traffic or detection quality. The awareness section reads
  the same PDR curve the awareness gate already reads.
* The density probes in §3c are synthetic grids with no buildings. Their CBR numbers are a
  channel measurement; they are not a claim about any city.
* The 1 188-vehicle hour in §5 is arithmetic over measured per-operation rates, not a run. Its one
  soft input is the fan-out, measured at 15.06 on a 334-vehicle scene; a denser scene has more
  neighbours per frame and the figure would rise.
* The cost table is a **lower bound per row**, not a tight measurement: another agent was working
  in this tree and running the suite through the pass, and taking the minimum over readings is the
  only defensible way to remove company that can add but never subtract. The three arms with one
  reading are labelled as such. Nothing in §§1–4 depends on any of it — those are counts, digests
  and octets, and none of them moves with how busy the box is.
* The two `intas_am_dt01` / `intas_amwarm_dt01` traces this work froze live in `C:/Temp/pstack/`,
  outside the repository, exactly like the `C:/Temp/smob2/` traces the earlier SUMO work left. They
  are reproducible from the one command in *Reproducing it* and their sha256s are recorded in §0, so
  a re-freeze is a checkable operation rather than a re-run of chance.

---

## Reproducing it

```powershell
. C:/Users/Administrator/tools/env.ps1

# the two pinned digests, with every new feature off
python -m scms_sim_ref.mock_pipeline.run --flow --road grid --grid 5 --duration 60 `
    --arrival-rate 1.5 --attacker-pct 0.25 --seed 7 --out C:/Temp/g1     # 0bd93655...
python -m scms_sim_ref.mock_pipeline.run --flow --road grid --grid 6 --duration 300 `
    --arrival-rate 2 --attacker-pct 0.15 --traffic-lights --seed 42 --out C:/Temp/g2   # b25f2137...

# freeze the warm AM peak at InTAS's own calibrated 0.1 s step  (bench B)
cd scms-sim/scenarios/gen_intas_urban_low/sumo
python -m scms_sim_ref.mock_pipeline.sumo_trace --net ingolstadt.net.xml `
    --sumocfg InTAS_buildings.sumocfg --steps 3000 --dt 0.1 --begin 25200 --warmup 3000 `
    --seed 257318856 --out C:/Temp/pstack/intas_amwarm_dt01.trace

# the arm matrix  (see tools/protocol_stack_measure.py --list for the arm table)
python tools/protocol_stack_measure.py --arm cam_codec_dcc `
    --base C:/Temp/smob2/cfg_py300.json --out C:/Temp/pstack/x --json C:/Temp/pstack/x.json
python tools/protocol_stack_report.py --dir C:/Temp/pstack --tag intas --base base

# the CAM-rate attribution: the same trajectories under three rules
python tools/cam_rules_on_trace.py C:/Temp/smob2/intas_300s_dt01.trace --rule etsi
python tools/cam_rules_on_trace.py C:/Temp/smob2/intas_300s_dt01.trace --rule java
python tools/cam_rules_on_trace.py C:/Temp/smob2/intas_300s_dt01.trace --rule java_fp
python tools/cam_gap_analysis.py datasets/mosaic_intas_urban_low_gate_fulltrace

# the wire, the crypto, and the awareness multiplicity
python tools/codec_size_probe.py
python tools/ecdsa_cost_probe.py --fleet 1188 --hours 1 --cam-rate-hz 3.0267 --fanout 15.06
python tools/awareness_shots.py datasets/phase2_geometric/urban_osm_geometric
```
