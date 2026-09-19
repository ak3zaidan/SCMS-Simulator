# 01 — Inventory of the existing repository and disposition

Status: design draft for review (2026-09-18). Every claim below was checked against the working tree at commit `2832d63` (branch `main`); line numbers refer to that tree. The three detailed read-outs this document condenses are kept as working notes and can be regenerated; nothing here is inferred from the README or `docs/FEATURES.md` without checking the code.

Disposition vocabulary (from the brief): **reuse as-is**, **reuse after refactor**, **reference only**, **retire**.

## 1. Headline findings

1. The repository is ~25.5k lines: 3,436 in `mock_pipeline/run.py`, ~5.9k in `datagen/`, ~3.6k in `gui/`, ~2.9k Java + Python under `scms-sim/`, ~10k in tests. The scientific value is concentrated in about 2.5k lines (`scms_core`, `schemas/records.py`, the detector formulas, the attack renderings, `datagen/featurize.py`, `leakage_linter.py`, `verify_data.py`, `foundry.py`).
2. `run_pipeline` (`run.py` L1413–3048) is one 1,636-line function holding the world, GNSS sensor, 28 attack renderings, 15 detectors, the radio model, the MA, the CRL, and the output assembly as nested closures over one `cfg` and ~40 mutable locals. `PipelineConfig` has 134 flat fields; every feature added since the frozen golden digest is opt-in and draws from its own string-keyed `random.Random` stream so that the default `data_digest` stays byte-identical. That constraint, not architecture, shaped the file.
3. There is no channel model beyond a disc or log-distance-plus-shadowing reachability test with a per-step in-range cap; no MAC, no SINR, no data rate, no frame size, no fragmentation, no multi-channel, no C-V2X, no backhaul, no cellular. There is no OBU hardware model; certificate stores and CRLs are data, not costs. Security entities are concrete classes wired into the loop; revocation enforcement is a global time delay (`enforced`, L2176) on a vehicle-level dict, not a distributed CRL.
4. The road model is point nodes plus undirected straight edges with optional per-edge speed caps: no lanes, directions, junction internal lanes, turn restrictions, or signal plans (signals are a 2-colouring of nodes). OSM import keeps only `highway` and `maxspeed`, drops one-way, lanes, buildings, elevation, and is capped at 380 nodes.
5. The MOSAIC/Java layer duplicates the Python engine (32 attack bases, 9 detectors, no real signatures) and runs only from a Windows toolchain at `C:\Users\Administrator\tools`; the VeReMi NextGen submodule is not checked out. ADR 0001's base-stack choice was never exercised for the science; all results came from the Python generator.
6. The dataset contract (`ma/*`, `ground_truth/*`, `ml/*`, `manifest.json`, `DATASHEET.md`) is well specified, machine-audited (`tools/verify_data.py`, 30 invariants), and largely independent of the engine: `featurize`, `benchmark`, `validate`, `calibration`, `verify_data` read files, not engine objects. This is the part to preserve exactly.
7. The engine-independent conformance tests (butterfly and linkage test vectors, leakage rules, ML contract, dataset integrity, end-to-end semantics) are the acceptance suite a new engine must pass. Thirteen test files pin fixed golden digests of the current implementation; those are implementation contracts, not semantic ones, and will not survive a new core.

## 2. Evolve or replace? Evidence and decision

The brief's prior is a new core with the scientific pieces kept as plug-ins. The evidence supports the prior; the strongest reasons are structural, not stylistic:

| Requirement (brief) | What the current engine would need | Verdict |
|---|---|---|
| Frame-level radio with SINR, MAC timing, fragmentation, C-V2X | A discrete-event kernel with µs events; the loop is a 1 s (default `dt=1.0`) fixed step over vehicles with reception decided inside the same pass | rewrite |
| Node hardware, queues, verification cost | Per-node state machines with service queues; `Vehicle` is a god-object mutated by every subsystem (L565–668) | rewrite |
| Lane-level roads, buildings, terrain, 3D | A new world model; `roads.py` has no lanes, directions or junctions | rewrite |
| Protocol plug-ins (SCMS, ETSI, threshold) | An entity/flow abstraction; entities are three tiny classes and revocation is `revoked_vehicles[vid] = t` | rewrite |
| Determinism independent of ordering | Per-entity RNG streams and id-ordered merges; today the global `rng` is consumed inside the reception loop so receiver order is part of the reproducibility contract (L2683–2701) | rewrite |
| 10,000 vehicles at real time | A compiled core; pure Python is bounded far below that (spike, 02-architecture §11) | rewrite |
| Keep MA dataset compatibility | Emit the same files; `datagen` reads files | keep |
| Keep the science (butterfly, linkage, detectors, attacks, foundry) | Port formulas behind interfaces, keep constants as defaults, keep test vectors | keep |

Decision (ADR 0002): **new core**, with the current tree frozen under `legacy/scms_sim_ref/` as the validated reference. The legacy package remains importable for parity tests until the ported libraries pass the conformance suite on the new engine, then it is kept read-only as a citation target ("legacy v0.1 defaults").

What this decision does not throw away: the ~2.5k lines of science listed in §1.1 are ported (formula and constant preserved, each with a model card), and the dataset/auditing tools are reused unchanged.

## 3. Module-by-module inventory

### 3.1 `src/scms_sim_ref/scms_core/` — reuse as-is (ported 1:1, test vectors kept)

| File | Content | Disposition | Notes |
|---|---|---|---|
| `linkage.py` (177) | CAMP SCP2 linkage seeds (`ls_x(i) = trunc128(SHA-256(la_id ‖ ls_x(i−1) ‖ 0^112))`), pre-linkage values (AES Davies–Meyer, 72-bit), `lv = plv1 ⊕ plv2`, `CrlLinkageEntry.matches` with forward-only privacy, `crl_contains` | reuse as-is | Port to `v2xw-sec` (Rust) and keep the Python module as the oracle; `tests/test_linkage.py` becomes a cross-language vector test. The per-i-period expansion cost (2 SHA-256 + 2 AES per entry per period per j) is exactly the CRL processing cost the node model must charge (06-node-models §2.6). |
| `butterfly.py` (120) | SCP1 butterfly expansion `f(k, ι)` with AES over `prefix32‖i32‖j32‖0^32`, caterpillar/cocoon keys, explicit-cert variant (`b_ι + c`) | reuse as-is | Port; add the implicit (ECQV) variant the PoC also supports (05-protocols §2). |
| `ec.py` (≈70) | Minimal affine secp256r1 arithmetic, cross-validated against `cryptography` | reuse as-is (Python oracle) | In Rust use `p256` (RustCrypto); keep this file as the test oracle. |
| `crypto_abstract.py` (≈90) | Deterministic Ed25519 stand-in, `canonical_bytes`, `hashed_id8 = SHA-256(...)[-8:]` | reuse after refactor | The "outcome-only" abstraction is right and becomes the `CryptoBackend::Modeled` mode (03-interfaces §6); Ed25519 as a stand-in is replaced by ECDSA-P256 real mode plus modeled mode with size/cost descriptors. `HashedId8` is correct per IEEE 1609.2 (low-order 8 bytes). |

### 3.2 `src/scms_sim_ref/schemas/records.py` — reuse as-is

Dataclasses (not pydantic; `pydantic>=2` is declared but never imported), each with `_visibility ∈ {MA, PUBLIC, ORACLE}` and `to_dict()`. `FORBIDDEN_FEATURE_KEYS` plus `is_forbidden_feature_key()` (prefix rules `true_`, `label_`, `attack_`; suffix rules `real_id`, `realid`, `trueid`, `_true_id`) are the machine-checkable leakage definition. This file *is* the on-disk contract; the new engine's MA exporter targets these names unchanged (08-measurement §6). Records: `MaEvidenceMessage` (never written by the reference run), `MaReport`, `MaCertStatus`, `MaInvestigation`, `MaCrlEvent`, `GtVehicle`, `GtIdentityMap`, `GtAttack`, `GtReportLabel`, `GtLinkageRevocation`; `run.py` adds opt-in subclasses (`is_crl_aware`, `is_vru`, `station_type`).

### 3.3 `src/scms_sim_ref/mock_pipeline/run.py` — reference only as a whole; parts ported

| Part (lines) | Disposition | Reason |
|---|---|---|
| `PipelineConfig` (267–510), `validate_config` (900–1091), `_FIELD_META`, `config_schema`, `config_from_dict`, `CLI_PRESETS` | reuse after refactor | 134 knobs and ~60 cross-field rules are the knob catalogue; they become per-subsystem sections of the scenario schema (03-interfaces §13) with the same defaults; CLI flags are generated, not hand-mirrored (~95 `add_argument` calls today). |
| `LinkageAuthority`, `PseudonymCA`, `RegistrationAuthority` (514–559), `resolve_and_revoke` linkage steps (2149–2174) | reuse as-is (as the SCMS plug-in's entity logic) | Trust-boundary-correct and tiny; only the vehicle-level bookkeeping around them (`revoked_vehicles[vid]`) is replaced by real CRL objects. |
| `Vehicle` dataclass (565–668) | retire | Fusion point of every coupling: credentials, mobility, GNSS, attack state, collusion, MA outcome in one object. Replaced by actor kinematics, node runtime, credential store, attacker state as separate objects. |
| GNSS `measure` (1874–1897) | reuse after refactor | OU bias + white noise + outliers + degrade bursts + jam is a sound model (04-models §3.8). Fix: `conf` uses the vehicle's true bias (an oracle quantity, L1896); weather must be injected; speed/heading are currently noise-free (L2597). |
| IDM `_idm_accel` (2275–2283), signals `_light_green` (2267), gap acceptance (2418–2440), turn slowdown (2479) | reuse as-is (pure functions) | Already pure over a snapshot. Note IDM uses per-class `v.idm_a/idm_b`, not `cfg.idm_accel/decel` (dead knobs); the hard floor `−6 m/s²` (L2283) becomes a cited parameter. |
| MOBIL `_mobil_decide` / `_lane_step` (2285–2401) | reuse after refactor | Standard MOBIL with smoothstep lateral transition; neighbour classification duplicates the IDM leader search and must share one neighbour query. |
| `car_follow` orchestration (2403–2503) | reference only | Documents precedence (leader < red light < gap yield < turn cap < edge cap); rewritten as a composable longitudinal controller. |
| Spawn/demand/OD pre-pass (1699–1750), `demand_mult` | reuse after refactor | Thinned-Poisson arrivals with rush/night shapes; role coins (attacker/faulty/colluder) move out of the traffic generator into the threat layer. |
| Radio block (2673–2752): disc, log-distance+shadowing, congestion cap, NLOS, weather loss | reuse after refactor → becomes the **abstract radio tier** | Formulas are compact; the per-link shadow RNG (`f"{seed}:shadow:{digest}:{rx}:{step}"`) and the global-rng loss draw move into per-link streams. The `chan_capacity` cap is the abstract stand-in for CBR; calibrated against the high tier in 04-models §4.9. |
| Spatial bucketing (2239, 2685) | reuse as-is | Grid hash with cell = range; same design in the new engine. |
| Detector formulas (2018–2037, 2815–2868) and Kalman feature | reuse after refactor | The `detnorm ≈ 1 at threshold` design is the ML contract; keep formulas, move `last_claimed` state into a per-receiver tracker, drop the exact-float-equality frozen check (L2034). Ported as the `legacy-12` detector plug-in (07-threats §4). |
| Streak/report gating, `file_report` (2075–2118) | reuse after refactor | Keep gate semantics; split MA-visible report construction from GT labelling; stop hardcoding `cert_validity` all-True and `cert_crl_status="active"`. |
| MA windowing, `trusted`, revocation rule (2061–2073, 2941–2955) | reference only | Operating point (k=3 reporters, 4 distinct seconds, 3 s span, 15 s window, budget 30, reputation 40) is preserved as the `legacy-window` MA pipeline defaults, but the code is keyed on `Vehicle` identity (`digest_to_vehicle`) and has no cross-pseudonym correlation. |
| CRL enforcement (`enforced`, `revoked_vehicles`, CRL-aware adversary) | retire | Global delay, not a distributed CRL; attackers read the MA's internal dict (L2568). |
| RSU (`_rsu_spots`, RSU-as-Vehicle) | reuse after refactor | Placement helpers kept; RSU becomes a node type. |
| VRU actor (1642–1697) | reference only | Straight-line wander with no bounds; replaced by a sidewalk/crossing model. |
| DENM layer (2120–2147, 2606–2632, 2754–2783) | reuse after refactor | Triggers and plausibility check kept; DENM becomes a message type in the message layer. |
| Pseudonym scheduling (1546–1580), `active_pseudonym` | reuse after refactor | (i, j) assignment and validity windows are correct; the cert-life estimate that peeks at trip length and light cycle is a leak from mobility into provisioning and is replaced by an explicit provisioning policy. |
| Sybil ghosts, collusion pass (1617–1628, 2873–2939) | reuse after refactor | Move into the attacker plug-in; fabricated-evidence distributions kept. |
| Scenario events (`_parse_events`, `EVENT_TYPES`, lookups) | reuse as-is | Becomes the events timeline of the scenario schema with the same five types plus new ones. |
| Streaming/prune/live-state/SIGINT | reference only | Shows which tables stream; the recorder streams everything. |
| Output writers, `_data_digest`, `_write_manifest` | reuse as-is | Canonical JSONL + per-file SHA-256 + aggregate digest is kept verbatim in the MA exporter. |
| Byte-identical gating (schema and `DET_KEYS` depend on flags) | retire | Output schemas become fixed; flags change values, never column sets. |

Attack catalogue (28 known types; 21 default round-robin, plus combined 4, identity-spoof 2, DENM 1) with exact renderings is preserved as the specification of the ported attackers (07-threats §3), including `k`-scaling, duty cycle, onset jitter, CRL-aware dormancy, and the `falsified` labelling rule (L2606–2610).

### 3.4 `src/scms_sim_ref/mock_pipeline/roads.py` and `osm.py`

| File | Disposition | Notes |
|---|---|---|
| `roads.py` `Trip` (23–99) | reuse after refactor | Sound arc-length kinematics; heading convention (math, 0 = east) is kept as the engine's ENU convention; must become lane-aware. |
| `CustomNetwork` (373–680) | reuse after refactor | Validation, Dijkstra, spatial index, stats are good; needs direction, lanes, connections; 400-node cap removed. The `{"nodes":[[x,y]], "edges":[[a,b,speed?]]}` JSON format is kept as an importer (`json-legacy`). |
| `GridNetwork`, `RingNetwork`, `spider_graph` | reference only | Become procedural generators producing the new lane-level graph. |
| `osm.py` (249) | reuse after refactor | Overpass map-bbox fetch, XML cache, RDP simplification, largest-component filtering reusable; must read `oneway`, `lanes`, `junction`, restrictions, `traffic_signals`, buildings, and emit a real projection (today equirectangular from the bbox corner with no metadata). The 11 city bboxes are kept as presets. |

### 3.5 `src/scms_sim_ref/datagen/` and `tools/verify_data.py` — the dataset toolchain

| Module | Disposition | Notes |
|---|---|---|
| `leakage_linter.py` | reuse as-is | Pure function of `records.is_forbidden_feature_key`; build-breaking at three points (featurize, validate, verify_data). |
| `featurize.py` (684) | reuse as-is | Reads only the `ma/` + `ground_truth/` JSONL contract; writes the ten `ml/` tables + `schema.json`; splits by hash of true vehicle id (70/15/15) and forward-in-time; graph edges and subject windows. Requires the engine to emit `detnorm_*`, `detector_score`, `detector_score_norm`, `subject_pos_confidence`, `cert_crl_status`, optional `station_type`, `ma_denm_log`. |
| `benchmark.py`, `models.py` | reuse as-is | NumPy-only logistic regression and histogram GBDT; leave-one-family-out, forward-in-time, leave-one-domain-out, graph baseline, unsupervised baseline; `EXCLUDED_FEATURE_COLUMNS` is a shared contract. |
| `validate.py` | reuse as-is | Precision/recall, latency, per-family/type recall, detector reliability, RSU contribution from files; it is the foundry's fitness signal. |
| `calibration.py` | reuse as-is | Rayleigh fit of GNSS error and confidence coverage from `gt_emissions_sample`. |
| `datasheet.py` | reuse after refactor | Hardcodes 14 config key names and three MOSAIC-era manifest keys; section set and the strings tests assert are kept. |
| `corpus_report.py` | reuse after refactor | Imports `run.ATTACK_CATALOG` / `KNOWN_ATTACK_TYPES`; the type space moves behind the attacker registry. |
| `massive.py` | reuse after refactor | Factorial grids, randomized worlds, curriculum plan, `domain_id` merging are the experiment system's ancestors (08-measurement §4); bound to ~35 `PipelineConfig` kwargs and the `attack_type=""` sentinel. |
| `campaign.py` | retire | Windows PowerShell driver of the MOSAIC path. |
| `foundry.py`, `foundry_corpus.py` | reuse after refactor | MAP-Elites archive (270 cells: family × density × topology × attacker band), objectives (`evade`, `family:<F>`, `latency`), 13 mutation operators, LLM operator hook, corpus export and novelty comparison. Needs only: a config object, `validate_config`, `config_schema`, a run function, and `validate.validate`. Ported over the new attacker and scenario interfaces (07-threats §3.6). |
| `tools/verify_data.py` (677) | reuse as-is | Thirty invariants (L1–L3 leakage, R1–R3 referential, C1–C6 label correctness, CNT1–4 counts, S1–S2 splits, V1–V4 value sanity, I1–I2 digests, E3 graph, SCHEMA1, N1 world provenance, VRU1–2, DENM1–2). Runs on any directory with a `manifest.json`. |

### 3.6 `gui/` — retire as code, keep the API contract and the copilot tool set

| File | Disposition | Notes |
|---|---|---|
| `server.py` (1066) | retire (route list and `compute_stats` shape as reference) | Stdlib polling server, process-global state, Windows `taskkill` and PowerShell launch; the 16 routes map onto the new JSON-RPC surface (09-ui §8). |
| `agent.py` (1087) | reuse after refactor | The 16 copilot tools (`set_config`, `reset_config`, `apply_preset`, `run_and_analyze`, `get_config`, `describe_fields`, `sweep`, `compare`, `design_network`, `import_osm`, `get_network`, `set_events`, `save_scenario`, `load_scenario`, `list_scenarios`, `run_foundry`) with validation and caps are the seed of the agent API; the OpenAI-only transport and in-process run on the request thread go. The foundry LLM operator (L619–844) is reusable as-is. |
| `index.html` (1112) | retire (dashboard KPI mapping as reference) | Single-file, dark-only, polling; the "congestion map" is a state scatter (benign/attacker/reported/revoked). |
| `foundry_eval.py` (340) | reuse as-is | Pure A/B harness for mutation operators. |

### 3.7 `scms-sim/` (Eclipse MOSAIC layer) — retire; two pieces kept as reference

`LinkageEngine.java` is a verified port of `linkage.py` (10 shared vectors) and may be kept as a reference implementation for anyone integrating a JVM stack. `ScmsBeaconApp` contains a correct ETSI EN 302 637-2 CAM trigger block (heading > 4°, position > 4 m, speed > 0.5 m/s, 100 ms–1 s) worth citing when the CAM generator is written. `mapgen.py` carries usable `netconvert`/`netgenerate`/`randomTrips` recipes for the SUMO tier. Everything else (`ScmsBackend`, `AttackLib` 32 bases × profiles = 355 variants, `Scms` stubs, `SignedCam` with `sigValid` always true, the four `.ps1` scripts) is a second, diverging implementation with hard-coded `C:\Users\Administrator` paths, `.exe` binaries and NTFS junctions, and is retired.

### 3.8 `saved_scenarios/`, `docs/FEATURES.md`, packaging

- `saved_scenarios/<name>.json` = `{name, description, saved_config: {overrides}}` with `events` and `custom_network` stored as JSON strings: reuse after refactor as a scenario overlay (`base:` + overrides), with objects instead of strings.
- `docs/FEATURES.md`: reuse after refactor; stale counts (71 flags vs 98; 15 tools vs 16; "congestion map"; linear as a topology; custom-map format missing the speed field).
- `pyproject.toml`/`requirements.txt`: retired in favour of the monorepo layout (ADR 0010); `conftest.py` path hack goes with an installable package.
- `.gitmodules` references `third_party/veremi-nextgen` (not checked out). Not needed by the new design (VeReMi compatibility is an exporter format, 08-measurement §5.2); the submodule is removed in Phase 0.

## 4. Test suite classification

| Class | Files | Fate |
|---|---|---|
| Engine-independent conformance (must pass on the new engine) | `test_butterfly`, `test_linkage`, `test_leakage`, `test_featurize`, `test_ml_contract`, `test_models`, `test_generalization`, `test_corpus_report`, `test_calibration`, `test_validate_detectors`, `test_dataset_integrity`, and the semantic assertions of `test_pipeline`, `test_end_to_end`, `test_realism` | kept verbatim or as behaviour specs; run against both legacy and new outputs |
| Fixed golden digests of the current implementation (13 files pin `04ae97…cee38`, plus lights/multilane/ring digests) | `test_attack_magnitude`, `test_combined_attacks`, `test_config_knobs`, `test_denm`, `test_engine_truth`, `test_evasive_attackers`, `test_gap_acceptance`, `test_network_fidelity`, `test_lane_changes`, `test_radio_propagation`, `test_vru_spoofing`, `test_vru`, `test_vru_denm_harden` | digest constants dropped; behavioural assertions (e.g., "pulsed duty cycle lowers recall", "InvalidSignature → `signatureVerification` with `sig_valid=False`") carried into the new golden suite with new digests |
| Engine-knob and topology specifics | `test_flow`, `test_custom_networks`, `test_roads`, `test_osm_import`, `test_config*`, `test_interrupt`, `test_rsu`, `test_radio`, `test_massive*`, `test_curriculum`, `test_foundry*` | rewritten against the new interfaces; the behaviours are the spec |
| GUI/copilot | `test_agent`, `test_gui_*`, `test_foundry_eval`, `test_foundry_llm` | replaced by API-contract tests of the JSON-RPC surface and the copilot tool registry |

## 5. Backward-compatibility surface to preserve (summary; details in 08-measurement §6)

Files: `ma/ma_reports.jsonl`, `ma/ma_investigations.jsonl`, `ma/ma_crl_events.jsonl`, `ma/ma_cert_status.jsonl`, `ma/ma_denm_log.jsonl` (opt-in), `ground_truth/gt_vehicle.jsonl`, `gt_identity_map.jsonl`, `gt_attacks.jsonl`, `gt_report_labels.jsonl`, `gt_linkage_revocation.jsonl`, `gt_emissions_sample.jsonl`, `gt_denm_emissions.jsonl` (opt-in); `ml/` ten tables (parquet + csv) + `schema.json`; `manifest.json` (`dataset_version`, `build_utc`, `generator`, `seed`, `config`, `schema_versions`, `standards_profile`, `data_digest_sha256`, `outputs[]`, `counts{}`); `DATASHEET.md` section set. Row ordering rules, canonical JSON bytes, id formats (`rpt_%05d`, `case_%04d`, `crl_%04d`, `veh_%03d`, `atk_<vid>`, `emt_%08d`), the `detnorm_*` vocabulary and the `report_correctness` vocabulary are part of the contract.

Known contract defects to fix in schema v2 (additively, with the v1 profile keeping the old behaviour): `ma_cert_status.valid_from/valid_to` are constants (0 and `total_time`), `cert_validity` on reports is hardcoded all-True even for bad signatures, `cert_crl_status` is always `"active"`, investigations are written only on revocation (no "opened, dismissed" cases), and `ma_crl_events.num_entries` is cumulative.

## 6. Cross-platform debt (all removed by ADR 0010)

Absolute Windows paths (`C:\Users\Administrator\tools\env.ps1`, MOSAIC and SUMO defaults), `.exe` binaries, `mosaic.bat`, NTFS junctions, `powershell -File run.ps1` from the GUI, `taskkill`, and `gui/server.py`'s docstring `python gui\server.py`. Nothing under `scms-sim/` runs from a fresh clone on any OS without that toolchain.
