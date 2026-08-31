# SCMS-Simulator — Feature Catalogue

The single source of truth for **"what can this simulate?"** Every entry is derived from the code
of the pure-Python generator (`scms_sim_ref.mock_pipeline.run` and the `datagen` / `gui` packages),
not from memory. Each row gives one line of what it does plus the primary CLI flag(s) and/or
`PipelineConfig` field(s). Run `python -m scms_sim_ref.mock_pipeline.run --help` for the full flag
list (71 flags), or `--dump-config-schema out.json` for the complete config-field surface.

Unless noted, features belong to the pure-Python generator (no MOSAIC toolchain required).

---

## Networks & maps

| Feature | What it does | Flag / field |
|---|---|---|
| Road topology | Choose the network model | `--road {linear,grid,ring,spider,custom}` (`road_network`) |
| Grid network | `w × h` Manhattan grid of intersections | `--grid`, `--grid-h`, `--grid-block` |
| Irregular grid | Randomly remove a fraction of grid roads (kept connected) | `--grid-dropout` |
| Ring network | Circular beltway of `n` intersections | `--road ring --grid n` |
| Spider network | Radial arms + concentric rings ("spider" city) | `--road spider --grid ARMS --grid-h RINGS` |
| Custom map | Arbitrary connected node/edge graph (coords in metres) | `--road custom --custom-network JSON_OR_FILE` (`{"nodes":[[x,y]...],"edges":[[a,b]...]}`) |
| OSM real-city import | Extract a real city's streets from OpenStreetMap to a custom-network JSON | `python -m scms_sim_ref.mock_pipeline.osm --city <name> --out map.json` (11 cities: amsterdam, berlin, chicago, ingolstadt, london, manhattan, munich, paris, rome, sanfrancisco, vienna) or `--bbox minLon,minLat,maxLon,maxLat` |
| Multi-lane roads | Parallel lanes per road (overtaking, less gridlock) | `--lanes`, `--lane-width` |
| Speed limits (grid) | Tiered arterial / local posted limits | `--arterial-every`, `--arterial-speed`, `--local-speed` |
| Speed limit (ring) | Single posted limit for the whole ring | `--arterial-speed` |
| Off-road HD-map check | Distance-to-nearest-road check (feeds the `mapOffRoad` detector) | (automatic on grid/ring/spider/custom) |
| RSUs (infrastructure) | Fixed, always-trusted receivers placed on the map | `--n-rsus`, `--rsu-placement {spread,perimeter,center,corners,all}`, `--rsu-range`, `--rsu-coords "x1,y1;x2,y2;..."` |

## Traffic & mobility

| Feature | What it does | Flag / field |
|---|---|---|
| Traffic-flow mode | Vehicles spawn/despawn over time (routed trips) | `--flow`, `--duration`, `--arrival-rate`, `--max-total-vehicles` |
| Fixed-population mode | A set number of vehicles for a set number of steps | `--vehicles`, `--steps` |
| IDM car-following | Intelligent-Driver-Model queues, congestion, stop-and-go | `--idm-accel`, `--idm-decel`, `--idm-time-headway`, `--idm-min-gap`, `--idm-lookahead`, `--no-car-following` |
| Trip speeds | Desired-speed range per trip | `--trip-speed-min`, `--trip-speed-max` |
| Mixed fleet | Distinct length + kinematics per class (car/motorcycle/truck/bus) | `--fleet {mixed,car,truck,bus,motorcycle}`, `--fleet-mix "car:0.6,truck:0.3,bus:0.1"` |
| Origin–destination model | Uniform, or gravity (distance-decay trip lengths) | `--od-model {uniform,gravity}`, `--od-gravity-scale` |
| Boundary sources/sinks | Trips originate at the network perimeter | `--boundary-origins` |
| Time-of-day demand | Arrival-rate shape over the run | `--demand {uniform,rush,night}` |
| Traffic signals | Signalized intersections with a fixed cycle | `--traffic-lights`, `--light-cycle` |
| Cornering slowdown | Slow into sharp bends | `--turn-slowdown`, `--turn-speed` |
| Weather | GNSS noise, radio loss, and driver-speed effects | `--weather {clear,rain,fog,snow}` |

## Attacks & faults

The default catalog is **21 attack types across 7 families** — position, speed, heading, timing,
stealth, identity, credential. An **opt-in combined family** adds 4 more (mutually-inconsistent
multi-field attacks), for **25 renderable types across 8 families**. The combined family is excluded
from the default round-robin (so the default dataset digest is byte-stable) and is only produced
when explicitly selected via `--attack-mix` or a single `attack_type`.

| Feature | What it does | Flag / field |
|---|---|---|
| Attacker fraction | Fraction (or explicit ids) of the fleet that attacks | `--attacker-pct` (`attacker_ids`) |
| Attack selection | Per-type weights over the catalog (+ combined) | `--attack-mix "ConstPos:0.5,Sybil:0.3,..."` (`attack_type` for a single type) |
| Position family (5) | ConstPos, ConstPosOffset, RandomPos, Teleport, SineWavePos | via `--attack-mix` / round-robin |
| Speed family (3) | ConstSpeedOffset, RandomSpeed, StopAndGo | via `--attack-mix` / round-robin |
| Heading family (2) | ReversedHeading, HeadingOffset | via `--attack-mix` / round-robin |
| Timing family (5) | DataReplay, DoS, DelayedMessages, OutOfOrder, DoSRandom | via `--attack-mix` / round-robin |
| Stealth family (2) | SlowDrift, AlongRoadOffset (low-and-slow) | via `--attack-mix` / round-robin |
| Identity family (1) | Sybil (multiple ghost identities) | via `--attack-mix`; `--sybil-ghosts` sets ghost count |
| Credential family (3) | InvalidSignature, ExpiredCert, NotYetValid | via `--attack-mix` / round-robin |
| Combined family (4, opt-in) | Disruptive, PosSpeedInconsistent, PosHeadingInconsistent, EventualStop | `--attack-mix "Disruptive:1"` etc. |
| Attack magnitude | Scale falsification magnitude (subtle < 1) | `--attack-intensity` |
| Intermittent / pulsed | Falsify only in bursts (evades revocation) | `--attack-duty-cycle`, `--attack-pulse-period` |
| Varied onset | Spread attacker start times | `--attack-delay-jitter` |
| CRL-aware evasion | Watch the public CRL, go dormant after a bust | `--crl-aware-pct`, `--crl-dormant-s` |
| Collusion | Attackers file false reports against benign victims | `--collude-pct`, `--victim-pct` |
| Faulty vehicles | Malfunctioning-sensor class, distinct from attackers | `--faulty-pct` |
| GPS jamming | Per-step probability a benign vehicle loses GNSS fix | `--gps-jam-rate` |

## Detection & MA

| Feature | What it does | Flag / field |
|---|---|---|
| Detector suite | **12 firing detectors + 1 soft signal = 13 `detnorm_*` fingerprint fields** per report: positionSpeedInconsistency, positionJump, headingInconsistency, staleOrReplay, constantPositionFrozen, implausibleAcceleration, sybilCoLocation, acceptanceRangeThreshold, beaconFrequency, signatureVerification, certValidity, mapOffRoad (+ soft kalmanConsistency) | (automatic) |
| Lagged reference | Compare each fix against one ~`detector_lag_s` old (robust to turns/outliers) | `detector_lag_s` (config field) |
| Windowed MA | Misbehavior-Authority correlates reports over a window, investigates, revokes | (automatic) |
| Trusted-reporter gating | Collusion-robust MA gate: reputation + report-budget rate-limit; RSUs always trusted | `--no-ma-defense` disables it (`ma_defense`, `report_budget`, `reputation_max`) |
| Pseudonym rotation | Vehicles rotate pseudonym certs periodically | `--rotate-period` (0 = off) |
| CRL / revocation | Two-Linkage-Authority identity resolution → revocation → CRL → enforcement | (automatic; drives `crl-aware` attackers) |

## Radio & channel

| Feature | What it does | Flag / field |
|---|---|---|
| Reception range | Range-limited message reception | `--radio-range` |
| Packet loss | Baseline per-message loss | `--packet-loss` |
| NLOS obstruction | Distance-growing obstruction loss | `--nlos` |
| Channel congestion | In-range CAMs/step before congestion loss | `--chan-capacity` |
| Weather-driven loss | Rain/fog/snow add radio loss on top of the baseline | `--weather` (see WEATHER_RADIO_LOSS) |
| RSU range | Separate radio range for RSU receivers | `--rsu-range` |

## Scenario events

A JSON `--events` timeline (inline or file) applies deterministic mid-run dynamics; an empty
timeline leaves the run byte-identical. **5 event types:**

| Event type | What it does | Required keys |
|---|---|---|
| `demand` | Arrival-rate multiplier while active (surge) | `t`, `until`, `mult` |
| `weather` | Change weather at `t` (sensor noise, radio loss, driver speed) | `t`, `value` (clear/rain/fog/snow) |
| `close_edge` | Close a road to new trips (navigation avoidance) | `t`, `edge` `[a,b]`, `[until]` (needs grid/spider/custom) |
| `attack_wave` | Attackers only falsify inside wave windows | `t`, `until` |
| `attack_zone` | Attackers only falsify while inside a geofenced zone | `t`, `x`, `y`, `radius`, `[until]` |

Flag: `--events JSON_OR_FILE`.

## Datasets & ML outputs

| Feature | What it does | Flag / command |
|---|---|---|
| MA-visible data | `ma/*.jsonl` — the features an MA actually sees | (every run) |
| Ground truth | `ground_truth/*.jsonl` — oracle-only labels, kept strictly separate | (every run) |
| ML tables | `ml/` train/val/test tables (parquet + csv) + `schema.json`: report_features, report_labels, subject_features, subject_labels, graph_edges, subject_windows | `--featurize` |
| Graph + temporal export | Report graph edges (incl. opaque RSU nodes) and per-subject windows for GNN / sequence models | `--featurize` |
| Datasheet | `DATASHEET.md` describing the generated dataset | (every run) |
| Benchmark metrics | ROC-AUC, PR-AUC, precision/recall/F1, best-F1 operating point, recall@1%FPR, bootstrap CIs, numpy GBDT baseline | `scms_sim_ref.datagen.benchmark` (surfaced in the GUI) |
| Novel-attack generalization | Per-family held-out AUC (evaluate on families never trained on) | `scms_sim_ref.datagen.benchmark` |
| Calibration | Score-calibration reporting | `scms_sim_ref.datagen.calibration` |
| Validation | Detector / dataset validation checks | `scms_sim_ref.datagen.validate` |
| Leakage linter | Guards against identity/label leakage into MA-visible features | `scms_sim_ref.datagen.leakage_linter` |
| Training corpus | Factorial "massive" corpus: every scenario × permutation, merged with `domain_id`; `--flow` samples randomized worlds (topology + events) | `python -m scms_sim_ref.datagen.massive --grid {quick,medium,full} [--flow] [--dry-run] [--parquet]` |

## Realism benchmark

Measures whether the generated traffic and radio actually look real, against reference summaries
pinned **with citations** in `datagen/refdata/` (10 sets / 98 entries at the time of writing — the
loader picks up whatever `refdata/*.json` is on disk; every entry carries a `source`, a short `cite`
and a `confidence` ∈ {anchored, coarse, unavailable}, and the suite asserts all three exist). Every
number in a scorecard is either measured or `na`; nothing is assumed. Read-only, deterministic,
numpy-only — it never writes into a dataset and takes no RNG draws.

| Feature | What it does | Flag / command |
|---|---|---|
| Realism scorecard | 22 metrics (15 traffic + 7 comm) with pass/fail/na, each carrying its reference range, citation and — when `na` — a machine-readable reason. `card["kinematics_source"]` (and every affected metric's `details.kinematics_source`) records *which estimator produced the number* | `python -m scms_sim_ref.datagen.realism_bench <dataset> [--markdown] [--json out]` |
| HARD physical gate | Acceleration inside [−8, +4] m/s², zero teleports, zero overlapping vehicles, **plus a liveness gate** (≥ 50 % of the fleet actually moves — the other three are impossibility checks a frozen dataset passes) — the CI gate; everything else warns | `--fail-on-hard` (exit 1 on any HARD failure) |
| Traffic panel | Speed percentiles, acceleration plausibility + comfort band, lane-change (lateral) discontinuity rate, teleports (scanned across *every* consecutive sample pair, not just short gaps), vehicle overlap, moving-vehicle fraction, time-headway median / sub-floor fraction / KS vs a fitted Cowan-M3 shape (leader found in the follower's own lane, so adjacent-lane traffic cannot fake a near-zero headway and no headway is censored by a grouping cell), Edie fundamental diagram over *directional* cells with a measured per-lane divisor (capacity + backward wave speed) | (automatic) |
| Lane-change continuity | `traffic.lateral_discontinuity_events` — sideways steps of at least half a lane width, at over 2 m/s, taken while the direction of travel does **not** turn (so cornering is not counted), per vehicle-km. SOFT, reference max **0.0** ev/veh-km (SUMO 1.25.0 defaults `--lanechange.duration 0`, i.e. an instantaneous centreline snap). This is the artefact the acceleration screen removes, published rather than deleted | (automatic) |
| Sampling-gap discipline | Every finite difference is taken across sample pairs no wider than `MAX_FD_DT_S` (2 s) **and normalised over exactly that subset**; the lateral counter additionally requires the ±3-step window its heading is inferred from to be inside the ceiling. A sub-sampled trace therefore reports `na` with a reason instead of measuring where a vehicle got to unobserved | (automatic) |
| Ground-truth kinematics | Consumes `true_speed` / `true_heading` from the ground-truth emission record **when present** (ADR 0002), which turns acceleration into a first difference of a measured quantity; falls back to position differencing for older datasets. The heading convention is *detected* against the observed chord bearings and both fields are rejected if they do not corroborate — a wrong unit or a 90° convention error can never silently rotate the decomposition | (automatic) |
| Comm panel | Neighbour-awareness ratio at 100 / 200 / 300 m, PDR gray-zone width (90 %→20 %), effective range, CAM inter-packet gap | (automatic) |
| Engine-agnostic | Auto-detects the pure-Python and MOSAIC/SUMO producers and their differing `acceptanceRangeThreshold` conventions; needs a full trace (`emit_sample_prob=1.0` / `SCMS_EMIT_SAMPLE=1.0`) to score the distribution metrics | `--regime {auto,urban,highway}` to override |
| Corpus realism gate | Folds the scorecard into the corpus report as a separate section + warning list; HARD failures make the CLI exit non-zero | `python -m scms_sim_ref.datagen.corpus_report --corpus <dir> --realism` |
| Per-domain realism | Records the scorecard summary alongside precision/recall in the campaign / massive per-domain catalog | `campaign` (automatic), `massive --realism` |
| Foundry realism gate | Rejects evolved scenarios that "evade" only by being kinematically absurd | `python -m scms_sim_ref.datagen.foundry --realism-gate` |
| SUMO run-to-run stability | GEH over SUMO E1 induction loops between **two runs of the same scenario** — `comparison_kind: seed_stability`, thresholds derived in-tool from the exact conditional null, on window-native counts. Reproducibility only; **not** traffic validation and never graded against the FHWA criteria | `python tools/sumo_realism.py --det-out <after.xml> --ref-det-out <before.xml> --det-add <E1.add.xml>` |
| SUMO FHWA validation | The same tool's `--ref-counts` mode (`comparison_kind: fhwa_validation`) is the only one that applies the FHWA criteria. **Blocked in this repo: no measured counts exist on disk** — see the note below | `python tools/sumo_realism.py --det-out <out.xml> --det-add <E1.add.xml> --ref-counts <measured.json>` |
| SUMO acceleration gate | Acceleration plausibility from SUMO's *own* reported speed in an fcd/emission trace (no position double-differencing, no reference counts needed) | `python tools/sumo_realism.py --fcd <fcd.xml>` |

> **FHWA validation is BLOCKED, not met and not failed.** No real-world measured loop counts are
> vendored anywhere in the tree: all 15 `InTAS_Detectors_Output.xml` copies under
> `third_party/veremi-nextgen` are config-echo stubs with zero `<interval>` rows, and the InTAS route
> files are *demand* (model input), so grading counts against them is circular. The loop **geometry**
> is genuine (196 `e1Detector`s, 25 named station groups) — geometry is not counts. What
> `--ref-det-out` produces is a **seed-stability** check of the simulator against itself; its gate ids
> are `seed_stability.*` and its report carries a leading `warning` key saying so. The roadmap gate
> "GEH < 5 on ≥ 85 % of InTAS loop stations" must not be quoted. Supplying counts through
> `--ref-counts` unblocks it — schema at
> `src/scms_sim_ref/datagen/refdata/geh_reference_counts.README.md`.

## MOSAIC/SUMO realism (Java + scenario layer)

Applies to the `mosaic` generator only (needs the SUMO/MOSAIC toolchain). Everything is reversible
through `SCMS_*` environment variables and recorded in `<dataset>/scenario_provenance.json`.

| Feature | What it does | Env knob |
|---|---|---|
| 100 ms MOSAIC↔SUMO sync | Default on every map, so the ETSI EN 302 637-2 CAM rules fire at 1–10 Hz instead of a 1 Hz spike. ~10× the MOSAIC steps. MOSAIC launches SUMO with `--step-length <sync>`, which beats the sumocfg — so this knob is also the SUMO integration step, every generated sumocfg is rewritten to match, and `resolved.sumo_step_ms` records what SUMO really ran | `SCMS_SYNC_MS` (`1000` restores the old behaviour) |
| EIDM car-following | Human-like extended IDM instead of Krauss, on generated, curated and InTAS maps alike | `SCMS_CF_MODEL` |
| Sublane lane changes | SUMO's sublane model on by default (`--lateral-resolution 0.8`, which auto-selects SL2015) so a lane change is a continuous ~3 s lateral traverse instead of a single-step snap across a whole 3.2 m lane — the exact jump a V2X position-plausibility detector keys on. 0.8 m splits SUMO's default lane into 4 sublanes and stays under the narrowest motorised vehicle (motorcycle, 0.9 m). Measured on a matched InTAS pair (300 s, seed 42, `emit_p` 1.0, 334 vehicles, identical but for this knob): `traffic.lateral_discontinuity_events` **0.5851 → 0.1302** ev/veh-km (4.49× fewer), full-lane-width lateral steps 134 → 5 (26.8× fewer), max single-step lateral offset 6.42 → 4.50 m. It does **not** fix the acceleration gate and marginally worsens it (0.998671 → 0.998233), because that residual is longitudinal. The 0.0 ev/veh-km target is still not met on any SUMO run | `SCMS_LATERAL_RES` (`off` restores instant snapping), `SCMS_LATERAL_SPEED` |
| Driver heterogeneity | Per-driver `speedFactor` distribution + jittered `tau`/`accel`/`decel`/`minGap`/`length` prototypes (MOSAIC otherwise hard-writes `speedDev="0.0"`) | `SCMS_SPEED_DEV`, `SCMS_VTYPE_SAMPLES`, `SCMS_VTYPE_JITTER` |
| Driver profiles | VeReMi-NextGen 10/80/10 aggressive/normal/passive, applied per vehicle at runtime; keyed on (seed, vehicle id) so the fleet mix is reproducible | `SCMS_DRIVER_PROFILES`, `SCMS_DRIVER_AGGRESSIVE_PCT`, `SCMS_DRIVER_PASSIVE_PCT` |
| NextGen sensor-error model | Temporally-correlated GNSS error, relative speed error, speed-decaying heading error. The CAM carries only an ETSI-style 95 % confidence radius — never the realised error vector — and that radius is **quantised onto a coarse fleet-shared ladder** so a constant per-vehicle confidence cannot act as a cross-pseudonym linkage key | `SCMS_SENSOR_MODEL=nextgen`, `SCMS_SENSOR_*` |
| Distance-based pseudonym change | NextGen's privacy model (800–1500 m driven, then distance **and** 120–360 s), re-based on simulation time. Makes `SCMS_ROTATE_PERIOD` inert | `SCMS_PSEUDONYM_POLICY=distance`, `SCMS_PSN_*` |
| Road-side units | `org.scms.app.ScmsRsuApp`: static, always-trusted receivers running the same detector suite as vehicles, placed on real junctions with correct WGS-84 positions | `SCMS_RSUS`, `SCMS_RSU_PLACEMENT`, `SCMS_RSU_APP` |
| Gravity OD + departure profiles | Capacity-weighted origin/destination sampling and per-interval departure-rate shapes on generated maps | `SCMS_OD`, `SCMS_DEPART_PROFILE` |
| Traffic-signal guessing | `--tls.guess`/`-signals`, `--tls.join`, `--junctions.join`, `--ramps.guess` on procedural/OSM imports | `SCMS_TLS` |
| Scenario provenance | `scenario_provenance.json` (effective env + resolved knobs + SHA-256 per input) and a `scms_inputs.json` side-car the Java back-end inlines into `manifest.inputs` | (automatic) |

> The two ported VeReMi-NextGen components (`org.scms.realism.DriverProfile`,
> `org.scms.realism.SensorErrorModel`) are **EPL-2.0** — see `THIRD_PARTY_LICENSES.md`.

## GUI & Copilot

Launch with `.\gui.ps1` → `http://127.0.0.1:8710`. Dependency-free web panel (Python stdlib server).

| Feature | What it does |
|---|---|
| Generator selector | `python-flow` (built-in routed sim, default) or `mosaic` (Java/SUMO via `run.ps1`) |
| Presets & fields | One-click presets, plus a **"⚙ Advanced: all fields"** panel exposing every config field (runs the exact config via replay) |
| Live map | Live congestion map while a run is in progress (`--live-interval` writes `live_state.json`) |
| Results dashboard | Precision/recall, per-task ROC-AUC with GBDT + CIs, calibration, generalization |
| **AI Copilot** | Drives the panel from plain language via a fixed tool set (needs `OPENAI_API_KEY` in `.env`; model `gpt-4o-mini` by default, `OPENAI_MODEL` to override). Tools: `set_config`, `reset_config`, `apply_preset`, `get_config`, `describe_fields`, `run_and_analyze`, `sweep`, `compare`, `design_network`, `import_osm`, `get_network`, `set_events`, `save_scenario`, `load_scenario`, `list_scenarios` |
| Copilot — design maps | Build a custom road graph (`design_network`), read/edit the current map (`get_network`), import a real city (`import_osm`) |
| Copilot — timelines | Author a scenario-events timeline (`set_events`) |
| Copilot — run & analyze | Run and return a compact analysis (`run_and_analyze`); sweep one field (`sweep`); compare labelled variants (`compare`) |
| Copilot — scenario library | Save / load / list named scenarios (`save_scenario`, `load_scenario`, `list_scenarios`) |

## Determinism & reproducibility

| Feature | What it does | Flag / field |
|---|---|---|
| Deterministic runs | Same seed + config → byte-identical data (`data_digest_sha256`) | `--seed` |
| Manifest | `manifest.json` records seed, full config, per-file SHA-256, data digest, schema versions, and a standards profile (report: ETSI TS 103 759 shape; cert: IEEE 1609.2; linkage: CAMP SCP2) | (every run) |
| Exact replay | Re-run a past run byte-for-byte from its manifest (ignores other flags) | `--config manifest.json` |
| Config dump | Write the effective config to JSON, then run | `--dump-config out.json` |
| Config schema | Write the field schema (name/type/default) and exit | `--dump-config-schema out.json` |
| Config validation | Validate a config/manifest without running | `--check-config cfg.json` |
| Presets | Named scenario presets (flags still override) | `--preset {urban_rush,highway,night_rain,gridlock,stealth_hard}`, `--list-presets` |
| Interruptible | Ctrl-C finalizes a valid partial dataset (valid manifest + digest) | (automatic) |
| Memory-bounded | Streaming output keeps memory bounded on long runs | (automatic) |

---

*Counts verified against the code: 21 catalog attack types (`ATTACK_CATALOG`) + 4 combined
(`COMBINED_ATTACKS`) = 25 across 8 families; 12 firing detectors + 1 soft signal; 5 road topologies;
11 OSM cities; 5 scenario-event types; 5 presets; 71 CLI flags; 15 Copilot tools.*
