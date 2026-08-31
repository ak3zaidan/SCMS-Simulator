# SCMS-Simulator — Realism Gap Analysis & 3-Day Roadmap

## 1. Current-State Summary

### 1.1 Traffic (mobility)
Two engines, split personality:

- **Pure-Python mock pipeline** (`src/scms_sim_ref/mock_pipeline/run.py`, 3437 lines) — the *feature* flagship (RSUs, VRUs, DENMs, events timeline, 355→134-knob config, GUI default). Default mode is **no traffic physics** (straight-line kinematics + sinusoidal wander, run.py:650-668). Opt-in flow mode gives IDM car-following (run.py:2274-2283), MOBIL lane changes (off by default), toy 2-phase all-or-nothing signals (24 s cycle, checkerboard offsets), first-come gap acceptance, Poisson demand thinned by a run-fraction "rush" profile, single frozen shortest-path routing (BFS hop-count on grids — not even metric). No collision detection, no head-on interaction (undirected centerlines, opposing streams pass through each other via the ≥45° heading filter at run.py:2458).
- **MOSAIC/SUMO layer** (`scms-sim/`, run.ps1 6-stage pipeline) — the *traffic* flagship: real SUMO microsimulation, real InTAS Ingolstadt (4,690 junctions, 196 real TLS programs, calibrated 24 h demand via VeReMi-NextGen submodule), 68 OSM city keys, 355 attack variants, and it already writes the exact same dataset contract (`datasets/smoke` proves the round trip). But: Krauss car-following (not EIDM), homogeneous fleet (gen_scenario.py:140-145 overwrites all 45 InTAS prototypes with one uniform SCMS_VEH_* set, destroying NextGen's 10/80/10 driver profiles), 1000 ms MOSAIC↔SUMO sync on the most realistic maps (neuters ETSI 1–10 Hz CAMs), randomTrips uniform OD on procedural maps, RSUs/TLS/servers stripped from mapping.

### 1.2 Road network
Python side: undirected 2-D straight-segment graphs, global `n_lanes` as a perpendicular offset, MAX 400 nodes/1600 edges, OSM import (~1–2 km², ≤380 nodes) keeps only `highway` + `maxspeed` — oneway, lanes, turn restrictions, traffic_signals, roundabouts, buildings all dropped. MOSAIC side: real netconvert networks with correct one-ways/lanes/junction internals/TLS, but no bridge back to the Python engine, and procedural netgenerate maps get zero traffic lights.

### 1.3 Communication
- Python: 1 CAM/vehicle/step (1 Hz at dt=1), hard unit-disc default; opt-in log-distance + **i.i.d. per-step** log-normal shadowing keyed on cert digest (pseudonym rotation resamples the channel!, run.py:2742). Loss composition is *additive* (can exceed 1.0, run.py:2750), congestion is a per-receiver linear ramp, no MAC, no fading, no CBR/DCC, no latency, no RSSI anywhere in the schema (records.py:71-87).
- MOSAIC: SNS unit-disk (709.4 m radius, 0.4–2.4 ms delay, flat loss) + hand-rolled receiver-side Bernoulli hacks in ScmsBeaconApp — including an NLOS ramp computed from the **attacker-controlled claimed position** (ScmsBeaconApp.java:216, a realism bug). The vendored VeReMi-NextGen 802.11p/INET omnetpp.ini configs sit unused (OMNeT++ not installed on Windows; gen_scenario forces `sns:true`).
- Nothing anywhere uses the InTAS `buildings.poly.xml` (5.7 MB of real footprints) for LOS — a place we can *exceed* the upstream SOTA, which explicitly lacks obstacle loss.

### 1.4 Benchmarking
~0% of a realism harness exists. All benchmarking is detection-quality (ROC/PR/recall@FPR). Only calibration: GNSS CEP vs a literature constant. No fundamental diagram, no headway/accel distributions, no GEH, no PDR-vs-distance, no CBR validation. Reusable assets: `calibration.py` pattern (numpy KS, JSON out, datasheet-embedded), digest-neutral hooks (`PER_STEP_HOOK`, `LANE_CHANGE_HOOK`, `GAP_YIELD_HOOK`), `emit_sample_prob=1.0` full traces, InTAS induction loops (`InTAS_E1.add.xml`) already copied into every generated scenario.

### 1.5 Hard invariants (must-not-break)
1. **Golden digest contract**: default config byte-identical (digest `0bd93655a2d5…` pinned in 11 test files; **was `04ae9736f519…` until ADR 0002 re-pinned it on 2026-08-30** when `true_speed`/`true_heading` entered the ground-truth record). All new realism = opt-in + dedicated string-keyed RNG streams (`random.Random(f"{seed}:label:id")`), zero draws when off.
2. **Manifest replay**: `config_from_dict` round-trip; every knob through PipelineConfig → validate_config → _FIELD_META → argparse → (optional) CONFIG_SPEC pf_* — the 9-step checklist in the gui-config-surface report.
3. **Schema firewall**: new MA-visible fields must pass `FORBIDDEN_FEATURE_KEYS` / leakage linter.
4. Windows host; MOSAIC ns-3/OMNeT++ federates are Linux-only (Dockerfiles) — **not** available natively.

---

## 2. Gap List (ranked by realism impact)

| # | Gap | Layer | Impact | Cost to close |
|---|-----|-------|--------|---------------|
| G1 | No realism measurement at all — cannot even quantify progress; datasheet "scorecard" bands are 5–45 m/s wide | Bench | Blocks everything else | Low (harness exists as pattern) |
| G2 | Python-engine mobility is synthetic (no directional roads, no conflict physics, static routing) while SUMO-grade mobility already exists next door with the same dataset contract | Traffic | Highest single gap vs "real city" | Medium (seam is narrow: `cur_x/cur_y/cur_v/cur_h`, one `car_follow` call site) |
| G3 | Radio has no geometry, no correlated shadowing, no fading, no MAC/CBR/DCC, no latency, no RSSI observable — on **both** engines | Comm | Second-highest; dominates V2X dataset realism | Medium (pure-Python analytic stack is literature-validated) |
| G4 | MOSAIC 1000 ms sync + Krauss + homogeneous fleet wastes InTAS's calibrated realism; NextGen driver profiles & SensorErrorModel discarded | Traffic | High — cheap wins on the flagship path | Low |
| G5 | Shadowing keyed on cert digest + i.i.d. per step; additive loss composition; claimed-position NLOS bug (Java) | Comm | High (correctness-level bugs) | Very low |
| G6 | No building-aware NLOS despite footprints on disk (InTAS poly + OSM `building=*` fetchable) | Comm | High — leapfrogs VeReMi-NextGen itself | Medium |
| G7 | Undirected/laneless Python network; OSM import drops oneway/lanes/signals/restrictions; 380-node cap | Network | High for python-flow "real city" claims | Medium |
| G8 | Demand: run-fraction Poisson (Python) / uniform randomTrips (MOSAIC procedural); no counts-calibrated OD | Traffic | Medium-high | Medium (routeSampler ships with SUMO) |
| G9 | 1 Hz beaconing, no ETSI CAM triggering, no DCC in the dataset-producing Python layer | Comm | Medium-high | Low-medium |
| G10 | No per-driver heterogeneity in Python engine (identical IDM params per class) | Traffic | Medium | Low |
| G11 | Weather affects only spawn-time desired speed; no friction/headway coupling | Traffic | Medium | Low |
| G12 | Signals: no yellow/all-red, no per-node plans, no actuation; OSM signal placement ignored | Traffic | Medium | Medium |
| G13 | VRUs are off-road random walkers; InTAS pedestrians un-instrumented | Traffic | Medium | Medium |
| G14 | No latency model; detection_time==generation_time | Comm | Medium | Low |
| G15 | Attack/label parity: unconditional attack labeling vs NextGen's conditional standard; no VeReMi-format export/CaTCH benchmark | Dataset | Medium (credibility) | Medium |
| G16 | Despawn teleportation, no parking/incidents/stops | Traffic | Low-medium | Low (virtual-leader mechanism reusable) |

---

## 3. Recommended Architecture

### 3.1 Mobility engine: **SUMO, two ways**
- **Track A (MOSAIC path = flagship "real city")**: keep MOSAIC 25.2 + SUMO 1.25.0 as-is architecturally; fix its realism regressions (100 ms sync default, EIDM `carFollowModel` + `vTypeDistribution` heterogeneity, port NextGen DriverProfile + SensorErrorModel into ScmsBeaconApp, restore RSUs). This is "integrate proven tools" at its purest — InTAS is already the SOTA calibrated scenario.
- **Track B (Python engine gets SUMO mobility)**: new opt-in `mobility_engine="sumo_trace"` in PipelineConfig. **Frozen-trajectory pattern** (not live TraCI in-loop) to preserve determinism: Phase A runs `sumo.exe` once (seed = `sha256(f"{seed}|sumo")`, single-threaded, pinned 1.25.0, libsumo pip-pinned `libsumo==traci==sumolib==eclipse-sumo==1.25.0` — the verified DLL-mismatch fix) and freezes per-step states to a canonical sorted/rounded `.jsonl`; Phase B replays that artifact through the existing step loop, writing only `cur_x/cur_y/cur_v/cur_h` + spawn/despawn, deleting nothing. The trajectory file's sha256 goes into the manifest config → SUMO nondeterminism becomes a *detectable input-hash change*, not a silent digest break. The entire SCMS/attack/detector/MA stack downstream is untouched (the seam is exactly `Vehicle.true_state()` + one `car_follow` call site at run.py:2541).
- Rationale vs live libsumo in-loop: measured 560 steps/s makes live viable, but the global-RNG interleaving fragility (run.py:1416 draws in reception order) makes freeze-and-replay the only 3-day-safe route; live mode is a documented follow-up.

### 3.2 Channel model: **pure-Python GEMV²-lite, standards-anchored**
New opt-in `radio_model="geometric"` in the reception loop (single choke point, run.py:2673-2752):
1. **Pathloss**: exact 3GPP TR 37.885 — Urban LOS `38.77+16.7·log10(d)+18.2·log10(f)`, Urban NLOS `36.85+30·log10(d)+18.9·log10(f)`, Highway LOS `32.4+20·log10(d)+20·log10(f)`; NLOSv extra loss `max{0,N(μ,σ)}`, μ=5..9+max(0,15·log10(d)−41).
2. **Link classification**: LOS/NLOSb via segment-vs-building-footprint test (buildings fetched by extended `osm.py` from the same cached Overpass extract; spatial-hash like run.py:2689); fallback per-wall Sommer model (9 dB/wall + 0.4 dB/m) when only wall counts are cheap; NLOSv from vehicle rectangles on the same edge.
3. **Shadowing**: AR(1)/Gudmundson per-link process, σ=3 dB LOS / 4 dB NLOS, decorrelation 10–13 m, keyed on **true vid** (`f"{seed}:shadow2:{tx_vid}:{rx_vid}"` + state carried per link) — fixes G5.
4. **Fading**: per-packet Nakagami-m, m = 3/1.5/1.0 over 0–50/50–150/>150 m, keyed stream.
5. **Loss composition**: independent-survival product `p_deliver = Π(1−p_i)` — fixes the >1.0 additive bug.
6. **MAC/congestion**: closed-form Sepulcre-2022-style CBR from per-receiver load × frame airtime (300 B @ 6 Mb/s ≈ 0.4 ms); **ETSI reactive DCC** table (10/5/2.5/2/1 Hz at CBR 0.30/0.40/0.50/0.60) gating the broadcast pre-pass; ETSI CAM dynamics triggering (Δpos>4 m, Δhdg>4°, Δv>0.5 m/s) ported from ScmsBeaconApp.java:140-155 for dt<1 runs.
7. **RSSI observable**: `rssi_dbm` on the evidence row (already have mean_db+shadow at run.py:2739) — Sybil ghosts inherit the attacker's true-position RSSI; register in schema + FORBIDDEN_FEATURE_KEYS review (it's MA-visible, derived from true geometry but legitimately measurable — allowed).
- **Java side**: same model family in ScmsBeaconApp — parse `buildings.poly.xml` once, fix claimed-position bug (use backend oracle's true sender position), add DCC. Do **not** attempt OMNeT++/ns-3 federation on this Windows host (Docker/WSL-only, dated pins); instead treat the vendored NextGen omnetpp.ini physics (20 mW, −81 dBm, SNIR 4 dB) as calibration constants.

### 3.3 How they plug in
- Every knob follows the established 9-step config pipeline (PipelineConfig → validate_config → _FIELD_META → CLI → schema → GUI advanced panel auto-surfaces → optional pf_* curated entry). New enum values: `radio_model: disc|logdistance|geometric`, `mobility_engine: internal|sumo_trace`, `road_network: …|sumo_net`.
- New `sumo_net` CustomNetwork importer via `sumolib` reads `.net.xml` (netconvert output: one-ways, per-edge lanes, TLS nodes, shapes) → Python engine inherits netconvert-cleaned topology without reimplementing OSM parsing (kills most of G7 in one step).
- All benchmark instrumentation rides the digest-neutral hooks — read-only, default-off.

---

## 4. Phased Roadmap (3 days, multi-agent)

### Phase 0 — Realism benchmark harness first (Day 1 AM; 1 agent, parallel with Phase 1)
**Goal**: measure before moving; every later phase gates on this.
**Tasks**:
- `src/scms_sim_ref/datagen/realism_bench.py` (numpy-only, `calibration.py` shape): traffic panel (speed/accel/headway distributions via per-vehicle finite differences over `gt_emissions_sample` at `emit_sample_prob=1.0`; edge-binned fundamental diagram; teleport/overlap counts) + comm panel (PDR-vs-distance via the heard-distance reconstruction from `detnorm_acceptanceRangeThreshold` per test_radio_propagation.py:58-72; CBR; awareness ratio at 100/200/300 m).
- `src/scms_sim_ref/datagen/refdata/` — pinned JSON reference summaries: highD headway/accel percentiles, pNEUMA urban speed/headway, FD anchors (capacity 1800–2400 veh/h/ln, wave speed 15–20 km/h), Boban awareness curve points, FLOURISH PDR-vs-RSSI anchor, 37.885 pathloss constants. Committed as data files with citations.
- SUMO-side: `tools/sumo_realism.py` computing GEH over InTAS `InTAS_E1.add.xml` virtual loops from SUMO detector output; accel-plausibility gate.
- Wire into `datasheet.py` scorecard (replace wide bands), `corpus_report.py` warnings (free CI gate), and a new `tests/test_realism_bench.py`.
**Files**: `datagen/realism_bench.py` (new), `datagen/refdata/*` (new), `datagen/datasheet.py`, `datagen/corpus_report.py`, `tools/sumo_realism.py` (new), tests.
**Benchmark gate**: harness runs on `datasets/smoke` + one fresh python-flow run and emits a complete scorecard JSON; baseline numbers recorded (expected failures documented — that *is* the baseline). Sanity: KS implementation reproduces `calibration.py` GNSS result within 1e-9.
**Risk**: low. Reference-summary curation is the only judgment call — cite every number.

### Phase 1 — MOSAIC/SUMO flagship realism (Day 1; 2 agents)
**Goal**: make the already-best path actually SOTA — traffic first, since it feeds Phase 2's channel.
**Tasks**:
- Flip 100 ms MOSAIC↔SUMO sync to default-on for curated + InTAS scenarios (gen_scenario.py:163-170 opt-out, write `updateInterval` into copied sumo_config.json) → activates real ETSI 1–10 Hz CAMs + the 100 ms congestion window.
- Driver heterogeneity: `carFollowModel="EIDM"` + `speedFactor="normc(1,0.1,0.7,1.3)"` per vType in mapgen.fleet(); port NextGen `DriverProfile` (10/80/10 aggressive/normal/passive via `requestVehicleParametersUpdate`) and `SensorErrorModel` (temporally correlated GPS, relative speed error, speed-decaying heading error; ~85 lines, EPL-2.0 with attribution; **fix the wall-clock pseudonym bug** — use sim time) into ScmsBeaconApp; make gen_scenario.py:140-145 apply SCMS_VEH_* as per-prototype jittered distributions, not one uniform set.
- Wire **LuST** and BeST-compatible registration into gen_scenario REG (route-kind entries like InTAS); restore RSU units with an RSU reporter app (un-strip at gen_scenario.py:118 — featurize already supports `_rsu_node`).
- Demand on procedural/OSM maps: gravity-weighted OD (node-degree/centrality) + per-hour depart profiles in mapgen; `--tls.guess-signals --tls.join --ramps.guess --junctions.join` for netconvert/netgenerate.
- MOSAIC manifest parity: record all effective SCMS_* env + scenario input-file sha256s (net.xml, rou.xml, scenario_config) in the Java-written manifest (ScmsBackend.java:632-642) → replayable MOSAIC runs.
**Files**: `scms-sim/scenarios/gen_scenario.py`, `mapgen.py`, `mosaic-apps/scms-app/src/.../ScmsBeaconApp.java`, `SignedCam.java`, `ScmsBackend.java`, `gui/server.py` (CONFIG_SPEC env entries for new knobs), `run.ps1`.
**Benchmark gate** (via Phase 0 harness on a fresh `intas_urban_rush` run):
- GEH < 5 on ≥ 85% of InTAS induction-loop stations (reference: InTAS calibrated demand, its own 24-station validation lineage).
- Acceleration plausibility: 100% of benign accels in [−8, +4] m/s²; ≥ 95% within ±3 m/s²; Wasserstein distance of speed distribution vs InTAS-native run (pre-change prototypes) reduced or neutral while headway KS vs highD-derived urban reference improves ≥ 20% relative to Day-0 baseline.
- CAM rate histogram spans 1–10 Hz (was a 1 Hz spike); mean CAM interval under free flow ≤ 400 ms.
- SUMO health: 0 teleports, 0 emergency-brake removals at default scale.
**Risk**: EIDM has a documented cross-platform libm determinism caveat — acceptable because MOSAIC-layer determinism is internal-reproducibility only (its digest is already non-comparable to Python's); pin SUMO 1.25.0 and record version in manifest. 100 ms sync grows runtime ~10× on InTAS — mitigate with SUMO `--scale` and shorter default windows; gate measures a 300 s slice.

### Phase 2 — Channel realism, both engines (Day 2; 2–3 agents)
**Goal**: replace unit-disk/heuristic radio with the geometric+analytic stack of §3.2.
**Tasks (Python)**:
- Fix G5 first in an opt-in versioned change: link-keyed AR(1) shadowing (true vid), independent-survival composition; move per-packet loss draw off the global RNG onto a link-keyed stream (`f"{seed}:pkt:{tx_vid}:{rx_vid}:{step}"`) — prerequisite for any later vectorization.
- `radio_model="geometric"`: 37.885 pathloss + LOS/NLOSb building test (extend `osm.py` to fetch/cache `building=*` from the same Overpass extract; store beside custom-network JSON; synthetic-grid fallback = per-edge urban-canyon density) + NLOSv + Nakagami-m + SINR→PER for QPSK-1/2 6 Mb/s.
- CBR observable logged per receiver; ETSI reactive DCC (opt-in `dcc_enabled`) modulating beacon emission in the pre-pass; ETSI CAM triggering rules for dt ≤ 0.5 runs; per-packet latency model (propagation + load-dependent MAC delay) buffered across steps, expressed via the existing `detection_time`/`generation_time` schema slots.
- `rssi_dbm` on evidence rows + `MaEvidenceMessage`; Sybil ghosts inherit attacker true-position RSSI; RSSI-vs-claimed-distance detector added to the detector suite (new reason code, gated).
- Rate-semantics re-normalization: `dos_burst`/`freq_max`/`chan_capacity` documented per-second with dt scaling; `revoke_min_seconds` window-bucketed.
**Tasks (Java)**: buildings.poly.xml LOS attenuation in ScmsBeaconApp; **fix claimed-position NLOS bug** (true sender pos from ScmsBackend oracle); reactive DCC; unify weather-loss tables with Python's; generate the missing `sns/` dir for scms_smoke.
**Files**: `mock_pipeline/run.py` (reception loop 2673-2752, pre-pass 2544-2650, config block + validate + _FIELD_META + CLI), `mock_pipeline/osm.py`, `schemas/records.py`, `datagen/featurize.py` (new column), `ScmsBeaconApp.java`, `gui/server.py` (pf_* entries: radio_model, dcc_enabled, building density), tests (`test_radio_propagation.py` additions; existing goldens untouched).
**Benchmark gate** (Phase 0 comm panel, urban OSM scenario + highway scenario):
- Awareness: ≥ 90% neighbor-awareness at 200 m urban and at 500 m highway under low load (reference: Boban & d'Orey 2015 measurement campaign).
- Gray zone exists: distance band where PDR falls 90%→20% spans ≥ 100 m (step-function models fail this; reference: Bai/Stancil/Krishnan MobiCom 2010 shape).
- Pathloss unit test: implemented 37.885 formulas match published constants to 0.01 dB at d=100/500 m.
- Shadowing correlation: empirical link-level autocorrelation e-folding distance 10±3 m (self-test against Gudmundson target).
- CBR/DCC: at ≥ 80 veh in radio range with DCC off, modeled CBR ≥ 0.55 (ETSI congestion regime); with DCC on, steady-state CBR ≤ 0.68 and CAM rate steps down per the ETSI table.
- Determinism: default-config golden digest `0bd93655a2d5…` byte-identical (full pytest suite green). *(Post-ADR-0002 value; `04ae9736f519…` was the pre-2026-08-30 pin.)*
**Risk**: building fetch adds an online dependency — mitigated by the existing sha256 disk-cache pattern (osm.py:193-209) and synthetic fallback. RSSI field leakage review — it is receiver-measurable, hence MA-visible-legitimate, but must be asserted in `test_dataset_integrity` and derived from *true* geometry only on the channel side.

### Phase 3 — SUMO-backed Python mobility + network fidelity + surfacing (Day 3; 3 agents)
**Goal**: close G2/G7 and wire everything through config/GUI/CI.
**Tasks**:
- **Agent A — sumo_trace mobility**: `mock_pipeline/sumo_trace.py` (new): Phase-A runner (subprocess `sumo.exe`, derived seed, `CREATE_NEW_PROCESS_GROUP` for Ctrl-C safety, netconvert'd map from the shared cached OSM extract or an InTAS slice) → canonical trajectory `.jsonl` (sorted `(step, veh_id)`, 3-dp rounds, stable vid mapping recorded) → Phase-B provider populating `cur_*` + spawn/despawn at the run.py:2541 seam; trajectory + net.xml sha256 into manifest config; `dist_to_road` backed by sumolib lanes; cert lifetime from SUMO route length. Config: `mobility_engine`, `sumo_net_file`, `sumo_demand` knobs through the full 9-step pipeline; GUI pf_* + generator stays `python-flow`.
- **Agent B — network fidelity (pure Python)**: directed edges + `oneway`/per-edge `lanes` in CustomNetwork schema (`[a,b,speed,lanes,oneway]`, opt-in); edge shape-points decoupled from graph nodes (Trip already consumes polylines) freeing the node budget; raise MAX_NODES→2000/MAX_EDGES→8000; osm.py parses `oneway`, `lanes`, `junction=roundabout`, `traffic_signals` nodes (signalize only tagged nodes when `traffic_lights="osm"`); `sumolib`-based `.net.xml → CustomNetwork` importer sharing mapgen's netconvert output; grid routing BFS→length/time Dijkstra (opt-in `route_metric`).
- **Agent C — integration/CI**: cross-fidelity harness (`tools/cross_fidelity.py`): same OSM extract through mock_pipeline vs MOSAIC/SUMO, diff realism-metric vectors; conditional attack labeling (NextGen rules — label attacker msgs only when materially falsified) as opt-in `label_mode="conditional"`; realism scores into `validate.py`/`campaign.py` per-domain records + foundry validity gate; GUI presets updated ("Max realism" gains geometric radio + sumo_trace); docs (FEATURES.md, DATASHEET.md known-unrealisms section).
**Files**: `mock_pipeline/sumo_trace.py` (new), `run.py` (dispatch 1459-1475, seam 2541, config), `roads.py`, `osm.py`, `tools/cross_fidelity.py` (new), `datagen/validate.py`, `datagen/foundry.py`, `gui/server.py`, `gui/index.html` (syncKind gating), `docs/*`, tests (`test_sumo_trace.py`, `test_directed_networks.py` new).
**Benchmark gate**:
- sumo_trace mode: replaying the same frozen trajectory twice → byte-identical `data_digest`; changing SUMO seed → manifest input-hash change detected by `--check-config`-style validation.
- Cross-fidelity: mock_pipeline (sumo_trace) vs MOSAIC on the same map — speed-distribution Wasserstein distance ≤ 0.5 m/s and headway KS ≤ 0.1 (they share the mobility engine; this validates the adapter).
- Network fidelity: imported OSM city (sumo_net path) reproduces reference one-way share ±5 pp and node-degree distribution KS ≤ 0.1 vs the netconvert net (ground truth by construction); intersection count within 10% of the raw extract.
- Directed-edge mode: zero physically-overlapping opposing vehicles (the test_gap_acceptance "no through-each-other" metric extended to head-on pairs).
- Full suite: all pinned golden digests unchanged; `tests/test_config_io.py` schema-coverage green; GUI parity suites green.
**Risk**: highest-complexity day. Fallback ladder: if the live seam fights back, sumo_trace ships as trajectory-replay only (still meets the gate); directed-edge work is independent and de-riskable; cross-fidelity harness only needs both datasets to exist.

---

## 5. Benchmark Harness Design

**Location**: `src/scms_sim_ref/datagen/realism_bench.py` (CLI: `python -m scms_sim_ref.datagen.realism_bench <dataset_dir> [--refdata <dir>] [--json out]`), reference data in `src/scms_sim_ref/datagen/refdata/`, SUMO-side GEH in `tools/sumo_realism.py`, cross-engine diff in `tools/cross_fidelity.py`. Read-only over dataset dirs; numpy-only; deterministic; embedded into `datasheet.py` scorecard and `corpus_report.py` CI exit codes.

**Traffic panel** (from `gt_emissions_sample` @ `emit_sample_prob=1.0`, or PER_STEP_HOOK collector for digest-neutral in-run capture; SUMO detector XML for MOSAIC runs):

| Metric | Threshold | Reference |
|---|---|---|
| GEH per virtual/real detector | <5 for ≥85% of stations; <4 on total flow; total ±5% | FHWA Toolbox Vol III; InTAS E1 loops |
| Accel plausibility | 100% in [−8,+4] m/s²; ≥95% in ±3 | NGSIM-critique gate (Punzo/Coifman) |
| Headway distribution | KS vs reference ≤ 0.15 (urban), ≤ 0.10 (highway) | highD (highway), pNEUMA (urban) pinned percentile summaries |
| Speed distribution | Wasserstein vs reference, tracked + regression ≤ +10% per commit | pNEUMA/UTD19-derived |
| Fundamental diagram | capacity 1800–2400 veh/h/ln; wave speed 15–20 km/h; capacity drop 5–15% | Cassidy & Bertini anchors |
| Sim health | teleports=0, overlap events=0, jam removals=0 | SUMO statistics / internal conflict metric |

**Comm panel** (from `ma_reports` heard-distance reconstruction + emissions):

| Metric | Threshold | Reference |
|---|---|---|
| Awareness ratio @200 m urban / @500 m hwy | ≥90% low-load | Boban & d'Orey 2015 |
| PDR gray-zone width (90%→20%) | ≥100 m | Bai/Stancil/Krishnan 2010 shape |
| Pathloss formula conformance | ≤0.01 dB vs 37.885 constants | 3GPP TR 37.885 |
| Shadowing decorrelation distance | 10±3 m | ETSI TR 103 257-1 |
| CBR @ high density (no DCC) | ≥0.55; with DCC steady ≤0.68 | ETSI TS 102 687 |
| PDR-vs-RSSI curve | RMSE tracked vs FLOURISH Bristol anchor | DOI 10.5523/bris.eupowp7h3jl525yxhm3521f57 |
| Latency p50 (802.11p profile) | 5–9 ms band when latency model on | DLR MK5 field data |

**Cross-fidelity panel**: mock_pipeline vs MOSAIC realism-metric vector diff (Wasserstein/KS per metric) — quantifies Python-engine fidelity drift; run in CI weekly-tier, not per-commit.

**CI wiring**: `corpus_report.py` gains realism warnings (nonzero exit already gates); `tests/test_realism_bench.py` pins the Phase 1–3 gates on small fast scenarios; golden-digest suite remains the determinism gate; datasheet embeds the scorecard so every published dataset carries audited realism numbers plus a "known unrealisms" section (sim-to-real gap, single-stack radio, no real RF interference) per the VeReMi-critique checklist.