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
