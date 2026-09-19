# Design brief — V2X World Simulator (SCMS-Simulator, next generation)

## 0. Your role, your deliverable, and how to work

You are the lead architect for the next generation of this repository. Your deliverable is a **design**, not an implementation. We are deliberately spending a long time in the planning phase, so be exhaustive, cite sources, and make every decision explicit. Throwaway spikes are allowed only when they de-risk a decision (for example, benchmarking a candidate core language or checking whether a library exists); nothing you write in a spike is production code.

Work in this order:

1. **Study the existing repository before anything else.** Read `README.md`, `docs/FEATURES.md`, `docs/adr/0001-stack-and-approach.md`, `DATASHEET.md`, everything under `src/scms_sim_ref/` (especially `mock_pipeline/run.py`, `scms_core/`, `schemas/records.py`), `gui/`, `scms-sim/` (the Java MOSAIC layer), `saved_scenarios/`, and skim `tests/`. Produce an inventory that classifies every module as: reuse as-is, reuse after refactor, keep as reference only, or retire. Section 3 tells you what I already know about it.
2. **Research the domain.** Use the standards, simulators, datasets, and hardware references in section 19. Do not invent parameter values. Every default in the design must carry a citation (standard clause, paper, datasheet) or an explicit `TODO: calibrate` marker with a plan for how it will be calibrated.
3. **Make the architecture decisions in section 16** with decision matrices. Each decision must list at least three alternatives, weighted criteria, the score, and the consequences of the choice.
4. **Write the design documents described in section 18** into `docs/design/` and new ADRs into `docs/adr/`.
5. **Finish with an open-questions list for me**, grouped by how much each answer changes the design. Do not block on them. State the assumption you proceeded with for each.

Hard rules:

- **No black box.** Every model must be explainable to a researcher who did not write it: the equations, the parameters, the defaults, the source of the defaults, the assumptions, and the known limitations. This is a research instrument, and reviewers of papers built on it will ask exactly how each number was produced.
- **Modular at every layer.** Anything a researcher might want to swap must be a plug-in behind a documented interface: maps, mobility models, propagation models, PHY/MAC stacks, message sets, security protocols, credential-management systems, crypto primitives, node hardware profiles, attackers, detectors, metrics, exporters, renderers.
- **Realism first, but with a fidelity ladder.** Real-world behavior is the target, but you cannot run a full PHY for ten thousand vehicles in a browser. Every model family must offer selectable fidelity tiers (for example: abstract, medium, high) that share one interface and are documented with what each tier ignores.
- **Licensing.** The code is Apache-2.0 and datasets are CC-BY-4.0 (ADR 0001). Do not base the design on GPL code (Veins, Artery, F2MD, ns-3 linked in-process). Reading them as references and re-implementing ideas is fine. Say explicitly how any external tool is integrated (in-process library, separate process, optional) and whether its license is compatible.
- **Cross-platform.** Development is now on macOS; the README's PowerShell scripts and `C:\Users\Administrator` toolchain paths are stale. The design must have a one-command dev setup on macOS, Linux, and Windows.
- **Do not implement.** When the design is done, stop and hand it to me for review.

## 1. Vision

A simulator that can model V2X traffic on any world, at any scale from one vehicle to a congested city, where **every node behaves as it would in a real deployed network**, and where researchers can plug in any security or credential-management protocol (SCMS, ETSI ITS PKI, threshold and umbrella-threshold schemes, post-quantum variants, or something invented next year), run it against realistic radio, traffic, hardware, and attacker conditions, watch it happen in 2D and 3D, measure everything, and export publishable datasets and figures.

Concretely, a user should be able to:

1. Pick or generate a world: import a real city from OpenStreetMap with buildings and elevation, load a SUMO network, generate a procedural city (grid, radial, suburban, highway, mixed), or hand-draw and edit one.
2. Populate it: vehicles of several classes, pedestrians and cyclists, roadside units, traffic signals, backend infrastructure, cellular coverage, weather, time of day.
3. Choose the communication technology (IEEE 802.11p / ITS-G5 DSRC, LTE-V2X PC5, NR-V2X PC5, hybrid, plus cellular Uu and wired backhaul) and the security protocol, each from a registry of plug-ins.
4. Inject attackers of many kinds, including authenticated insiders with valid credentials.
5. Run it live or headless. Watch a bird's-eye 2D view with vehicles moving, click any vehicle and animate down into a 3D world view following that car, with a live HUD of everything its on-board unit is doing: packets per second in and out, verification queue, CPU and RAM use, HSM load, certificate store, CRL size, neighbor table, current pseudonym, and so on.
6. Measure and plot network efficiency, protocol behavior, security outcomes (time to detect, time to revoke, time for revocation to propagate, residual harm), compute load, and traffic effects, then compare runs side by side.
7. Export datasets (for example, the misbehavior authority's global view for anomaly-detection research, per-receiver message logs, network traces, per-node telemetry) with ground truth kept strictly separate.
8. Scale from one vehicle to two to three to thousands and observe how every part of the system responds.

## 2. Non-negotiable principles

1. **Every node is a real network node.** Vehicles (on-board units), RSUs, VRU devices, cellular base stations, backhaul links, and every backend entity (RA, PCA, LA1, LA2, MA, CRL generator, ECA, ICA, root CA, policy generator, device configuration manager, location obscurer proxy, or their ETSI/threshold equivalents) are modeled with interfaces, queues, processing capacity, storage, clocks, addresses, protocols, and failure modes. Backend entities are not instantaneous oracles; a misbehavior report travels over a real path and a CRL is produced, signed, distributed, downloaded, and processed with delays and costs at each hop.
2. **Ground truth is separate from node belief.** The simulator knows the truth; each node only knows what it received and verified. Detectors, the MA, and exported features see only node-visible data. The existing leakage linter philosophy carries over.
3. **Deterministic and reproducible.** Same scenario file plus seed gives byte-identical event logs and datasets on any platform. Every output carries a manifest: config hash, seeds, git commit, plug-in versions, model versions, and per-file digests.
4. **Headless first; the UI attaches.** The engine runs without a UI, in batch, for sweeps and Monte Carlo replications. The UI can attach to a live run or replay a recorded run with scrubbing.
5. **Fidelity ladder.** Each model family has tiers sharing one interface. A run may mix tiers (for example, full PHY for the followed vehicle's neighborhood and abstract PHY elsewhere) if the design can make that sound; if it cannot, say so and why.
6. **Byte-accurate where bytes matter.** Message sizes, certificate sizes, signature sizes, fragmentation, and channel occupancy must be computed from real encodings (ASN.1 UPER for J2735 and ETSI messages, IEEE 1609.2 / ETSI TS 103 097 security envelopes), not from a constant.
7. **Cost-accurate where compute matters.** Signature verification, CRL expansion, certificate chain checks, and encryption consume modeled CPU time and memory on the node's hardware profile, and queue when the node is saturated.
8. **Extensible by outsiders.** A researcher who did not write the engine can add a protocol, an attack, a detector, or a metric by implementing a documented interface, registering it, and referencing it from a scenario file, without touching engine code. Ship an example plug-in of each kind as a tutorial.
9. **Agent-friendly.** The current GUI has an LLM copilot driving it through a tool API. Every UI action in the new system must be reachable through the same programmatic API (CLI, Python API, HTTP/WebSocket), so copilots, notebooks, and CI can do anything a human can.

## 3. The existing codebase and what to do with it

What exists today (verify this yourself):

- A pure-Python reference simulator, `src/scms_sim_ref/` (about 10k lines, most of it in a single 3,400-line `mock_pipeline/run.py`): IDM car-following with MOBIL-style lane changes, grid/ring/spider/custom/OSM road graphs, signals, time-of-day demand, mixed fleet, weather, VRU actors, an opt-in DENM layer, 25 attack types across 8 families, 12 local detectors plus a windowed MA with two-linkage-authority identity resolution, pseudonym rotation, CRL with propagation delay, CRL-aware evasive attackers, colluding reporters, RSUs, a scenario-events timeline, and dataset export with a leakage linter, datasheets, benchmarks, and a quality-diversity "foundry" that searches for hard attacks.
- A faithful SCMS scientific core in `scms_core/`: butterfly key expansion, linkage seeds and linkage values (CAMP SCP2), IEEE 1609.2 HashedId8, deterministic Ed25519 as a stand-in signing primitive with an abstract crypto interface.
- Radio is abstract: a disc or log-distance-plus-shadowing reachability test, a baseline packet-loss probability, a distance-growing NLOS loss, a per-step in-range message cap standing in for congestion, and weather loss. There is no channel access, no interference or SINR, no data rate, no frame size, no fragmentation, no multi-channel, no C-V2X, no backhaul, and no cellular model.
- There is no OBU hardware model: no CPU, RAM, storage, HSM, verification throughput, or queueing. Certificate stores and CRLs exist as data, not as costs.
- Security entities (RA, PCA, LA1, LA2, MA) are concrete Python classes wired directly into the pipeline; there is no protocol plug-in abstraction and no ETSI variant beyond report shaping (TS 103 759).
- There is no 3D. The GUI is a single HTML page with a 2D canvas congestion map, served by a stdlib HTTP server, plus an OpenAI-backed copilot with 15 tools.
- A thin Java Eclipse MOSAIC layer (`scms-sim/`) exists per ADR 0001, which chose MOSAIC + VeReMi NextGen as the base stack. In practice nearly all work happened in the Python generator. The VeReMi NextGen submodule is not checked out.

What I want from you regarding it:

1. Decide, with evidence, whether the new engine evolves this code or is a new core that treats this code as a validated reference and asset library. My prior is a new core, keeping the scientific pieces (butterfly, linkage, detector suite, attack catalog, dataset schemas, leakage linter, datasheet, benchmark, foundry) as plug-ins or ported libraries whose test vectors must still pass. Challenge that if you disagree.
2. Revisit ADR 0001 explicitly. The MOSAIC decision was made for a dataset generator, not a 3D world simulator with a custom PHY. Write a superseding ADR if the base-stack decision changes; keep the licensing and primary-SCMS-model decisions unless you have a reason.
3. Preserve backward compatibility of the MA dataset outputs (`ma/*`, `ground_truth/*`, `ml/*`, manifest, datasheet) or provide a documented migration, because downstream ML work depends on them.

## 4. World and scene engine

1. **Sources:** OpenStreetMap import (roads with lanes, turn restrictions, speed limits, signals, crossings, building footprints and heights, land use, trees where available), SUMO network import, procedural generation (Manhattan grid, European radial and irregular, suburban cul-de-sac, highway with interchanges, mixed), hand-drawn maps with an editor, and the existing JSON node/edge format. Terrain elevation from a DEM (for example SRTM or Copernicus) and its effect on line of sight and propagation.
2. **Lane-level road model:** lanes, connections, junction internal lanes, signal phases and plans, stop and yield rules, roundabouts, speed limits, parking, bus stops, pedestrian crossings and sidewalks, bike lanes. Decide whether the canonical internal format is SUMO's network format, OpenDRIVE, or your own, and how conversions work.
3. **3D scene:** buildings (extruded footprints with heights and simple materials, with LOD), roads with lane markings, signals, signs, trees, terrain, sky and lighting by time of day and weather. The same geometry feeds the propagation model (obstacles) and the renderer, so there is exactly one world model.
4. **Scenario templates:** a library of ready-made worlds at several scales (single intersection, one square kilometer downtown, highway stretch, full district) with recommended demand levels, so experiments are comparable across papers.
5. **World provenance:** every world records where it came from, bounding box, import date, and transformations applied.

## 5. Mobility and actors

1. **Vehicles:** microscopic car-following (IDM, Krauss, Wiedemann, Gipps as options), lane change (MOBIL, SUMO LC2013 equivalents), intersection behavior (signals, priority, gap acceptance, roundabouts), routing (shortest path, dynamic rerouting, congestion-aware), origin–destination demand with time-of-day profiles, vehicle classes (car, motorcycle, truck, bus, emergency, connected-automated with different behavior), parking and stops.
2. **Vulnerable road users:** pedestrians and cyclists with their own mobility model (social force or striping), sidewalks and crossings, and optional carried devices transmitting PSM (SAE J2735) or VAM (ETSI).
3. **Weather and surface:** rain, fog, snow, and ice affect desired speed, headway, braking, visibility, GNSS quality, and radio; each effect is a separately documented model.
4. **SUMO co-simulation decision:** decide whether SUMO (EPL-2.0) through TraCI or libsumo is the high-fidelity mobility tier behind the Mobility interface, with the native engine as the built-in tier. Consider determinism, step-time overhead, OSM import maturity, pedestrian support, and deployment friction.
5. **Ground truth kinematics** at a configurable step (10 to 100 ms), with a GNSS error model (urban canyon multipath, outages, jamming) producing the position each vehicle believes and transmits.

## 6. Communication stack

Each layer is a plug-in with fidelity tiers. Specify interfaces and at least the following models.

1. **Radio access technologies:** IEEE 802.11p / ITS-G5 OCB mode at 5.9 GHz (10 MHz channels, control and service channels, data rates and modulation-coding schemes, EDCA access categories, CSMA/CA with hidden terminals, capture effect); LTE-V2X PC5 Mode 4 and NR-V2X PC5 Mode 2 (resource pools, subchannels, semi-persistent scheduling, half-duplex, sensing-based resource selection, in-band emissions); a hybrid option. Cite 3GPP and IEEE/ETSI clauses for every parameter.
2. **Propagation:** free space, two-ray ground, log-distance with log-normal shadowing, Nakagami-m fast fading, environment presets (urban, suburban, highway, rural) from the VANET literature, obstacle shadowing from buildings and terrain using the world geometry, vehicles as obstacles (trucks blocking cars), antenna height and pattern, receiver sensitivity, weather attenuation at 5.9 GHz.
3. **Reception:** SINR from all concurrent transmitters, packet error rate curves per modulation-coding scheme and frame length, preamble capture, and the resulting per-frame outcome. The abstract tier may collapse this to a distance-and-load-based probability, but the interface stays the same.
4. **Congestion control:** ETSI DCC (TS 102 687, TS 103 175) and SAE J2945/1 rate and power adaptation driven by measured channel busy ratio, plus the option to disable them to observe raw collapse.
5. **Network and transport:** WSMP (IEEE 1609.3) and GeoNetworking/BTP (ETSI EN 302 636), with their real headers and size accounting; single-hop broadcast plus optional geocast and multi-hop forwarding.
6. **Fragmentation and large messages:** determine, layer by layer, whether real stacks fragment at all (802.11 OCB broadcast, WSMP, GeoNetworking) and model the consequences honestly: application-layer fragmentation with reassembly semantics and loss amplification, certificate omission and caching strategies (full certificate once per second, digest otherwise), certificate compression or hash-based distribution, and message splitting schemes proposed for post-quantum V2X. A researcher must be able to see exactly how a 6 KB signed message behaves on a channel whose frames carry about 2 KB.
7. **Infrastructure connectivity:** RSU wired or cellular backhaul with bandwidth and latency; cellular Uu coverage maps with per-cell capacity, latency distributions, handover, and outages; store-and-forward on vehicles out of coverage; a backend network model with per-link latency and per-entity service capacity. This is the path for certificate provisioning, top-ups, misbehavior reports, and CRL distribution.
8. **Channel occupancy accounting:** per-channel busy ratio, per-node airtime, and bytes over the air and over cellular, so cost comparisons between protocols are possible.

## 7. Message layer and security envelope

1. **Message sets:** SAE J2735 BSM (Part I and Part II, path history and prediction), ETSI CAM and DENM, SPaT and MAP from RSUs, PSM and VAM, CPM (collective perception), SRM/SSM, WSA. Encoded with real ASN.1 UPER so sizes are exact. Decide whether to use a real ASN.1 toolchain (Apache-compatible) or a validated size model per message type, and justify.
2. **Generation rules:** BSM at 10 Hz with J2945/1 congestion-driven interval and power adaptation; CAM generation triggers (heading, position, speed deltas, T_GenCam bounds) with DCC limits; DENM repetition; SPaT rates. All parameters cited.
3. **Security envelope:** IEEE 1609.2 signed data and ETSI TS 103 097 secured messages, with signer identifier policy (digest vs. full certificate vs. chain), signature algorithm agility (ECDSA P-256, Brainpool, ECQV implicit certificates, post-quantum ML-DSA and Falcon, hybrid), encryption for backend exchanges, and generation time and location fields used by replay and plausibility checks.
4. **Verification policy on receivers:** verify all, verify on demand (only messages relevant to a safety application), prioritized verification, and the effect of each on the HSM queue and on missed detections.
5. **Safety applications** consuming messages (forward collision warning, emergency electronic brake light, intersection movement assist, VRU warning) at least at an abstract level, so that security failures can be measured as safety outcomes (false warnings from ghost vehicles, missed warnings from dropped or unverified messages).

## 8. Credential-management protocols as plug-ins

Define a protocol interface rich enough that the following are all implementations of it, and prove it by sketching each one against the interface:

1. **US SCMS** (CAMP design, IEEE 1609.2 and 1609.2.1): the full entity set, enrollment, butterfly-key pseudonym provisioning in batches, concurrent weekly pseudonym certificates with overlapping validity, top-up downloads, pseudonym change strategies, misbehavior reports, MA correlation and investigation, LA1/LA2 linkage-value resolution, CRL entries as linkage seeds, CRL series and epochs, CRL distribution through RSUs and cellular, per-i-period CRL expansion on the OBU, RA blacklisting, and certificate expiry as passive revocation. Reuse the existing faithful butterfly and linkage code.
2. **ETSI ITS security (TS 102 941, TS 103 097, TS 103 759):** root CA, enrollment authority, authorization authority, enrollment credentials, authorization tickets with short validity, CTL and ECTL, CRLs for CA certificates, misbehavior reporting, and passive revocation of vehicles by refusing new authorization tickets. The interface must express both active revocation (CRLs) and passive revocation (credential starvation), because revocation latency means different things in the two systems.
3. **Threshold and umbrella-threshold schemes, and post-quantum variants.** These are my own research protocols. `[FILL IN: paste or link the specifications for the thresholding and umbrella thresholding schemes, including entity roles, key generation, signing rounds, credential formats, revocation mechanism, and expected signature/key/certificate sizes.]` Independently of the details, the interface must support: multi-party interactive protocols among backend entities (distributed key generation, t-of-n signing rounds, share refresh) that take real network round trips; credential formats with variable and large key, signature, and certificate sizes; multiple signature verifications per message; hierarchical or umbrella credentials covering many pseudonyms; and post-quantum primitives (ML-DSA, Falcon, SLH-DSA, hybrids) with their sizes and verification costs taken from the specifications and from a benchmarked library such as liboqs. Verify exact sizes against FIPS 204 and FIPS 205; do not rely on memory.
4. **Interface contents (at minimum):** entities and their trust boundaries; credential types and lifecycles; issuance and top-up flows as message sequences over the modeled network; signing and verification hooks per message type; signer identifier and certificate distribution policy; revocation mechanism, distribution, and receiver-side processing cost; misbehavior reporting format and transport; trust anchors and their updates; cryptographic primitive descriptors with size and cost tables per hardware profile; per-protocol metrics and per-protocol attack hooks.
5. **Crypto modes:** real cryptography (correctness tests, small runs, PQ through liboqs) and modeled cryptography (cost tables per hardware profile for large runs), selectable per run, with the guarantee that recorded outcomes are identical between modes.

## 9. Node models

1. **On-board unit hardware profiles** as data files, each citing a datasheet or a published benchmark: CPU cores and clock, RAM, flash, HSM type and its signing and verification throughput and latency, crypto accelerators, radio chipset, antenna. Start with profiles for at least one commercial DSRC unit, one C-V2X unit, one generic automotive-grade SoC without an HSM, and one hypothetical post-quantum-capable unit. Mark every number with its source.
2. **OBU runtime model:** receive queue, verification queue with the chosen policy, application processing, transmit scheduler, certificate store (pseudonyms, private keys or butterfly reconstruction, enrollment credential, trust anchors), peer certificate cache (digest to certificate), CRL store and per-epoch expansion cost, neighbor table with belief state, misbehavior evidence buffer, report outbox with store-and-forward, clock (GNSS time, drift when GNSS is lost), position estimate, and resource accounting (CPU percent, RAM, storage, HSM utilization, dropped messages by cause). All of this is inspectable live in the UI and exportable.
3. **RSU model:** fixed antenna, backhaul, roles (CRL distribution, certificate provisioning proxy, report forwarding, SPaT and MAP, WSA), its own hardware profile and queues, failure and compromise states.
4. **Backend entities:** each with service time distributions, concurrency limits, batching behavior (RA batching of requests, CRL generation cadence), storage growth (CRL size, report volume), and availability. Cellular base stations and backhaul links likewise.
5. **Node lifecycle:** vehicles power on and off, enter and leave the map, park; RSUs fail; backend entities go down; certificates expire mid-run.
6. **Optional perception model:** radar or camera detections of physically present neighbors, so local detectors can cross-check a claimed position against a sensed object (essential for ghost-vehicle research). Abstract tier is fine.

## 10. Threats, detection, and response

1. **Attacker model:** capabilities (valid credentials or not, number of credentials, compromised RSU, radio power, knowledge of the CRL, coordination with others, compute), goals, and strategies including adaptive and evasive behavior. Attackers are plug-ins with access only to what a real attacker in that position would have.
2. **Attack catalog:** port the existing 25 types, then add at least: ghost vehicles, Sybil with concurrent valid pseudonyms, replay and delay, message suppression, PHY-layer jamming and flooding, oversized-message flooding against PQ or threshold verification budgets, compromised RSU broadcasting false SPaT or MAP or CRLs, insider with valid credentials, misbehavior-report poisoning against honest vehicles, false DENM or CPM events, certificate misuse across regions, location tracking (a privacy attack against pseudonym change strategies), and coordinated multi-attacker campaigns. Keep the quality-diversity foundry as a generator over the attacker interface.
3. **Detection:** local plausibility and consistency checks (port the 12-detector suite), perception cross-checks, and the misbehavior authority's global correlation, investigation, and decision pipeline, all pluggable so researchers can drop in their own detectors or ML models and get scored.
4. **Response:** report transport, MA decision, revocation issuance, distribution, download, processing, and enforcement, each timestamped so the end-to-end revocation latency can be decomposed by stage.
5. **Privacy metrics:** linkability of pseudonyms under an observer with a given RSU density, anonymity set sizes at pseudonym change, and the effect of change strategies (time, distance, mix zones, silent periods).

## 11. Measurement, plots, and experiments

1. **Metric catalog** with definitions and units, computed by pluggable metric providers. At minimum: packet delivery ratio by distance and density; channel busy ratio; packet inter-reception time; end-to-end latency; neighborhood awareness ratio; collision and interference loss; airtime per node; fragmentation and reassembly failure rates; cellular and backhaul bytes; verifications per second, verification queue depth and drops, unverified-message ratio; certificate change events and top-up traffic; CRL size, download time, processing time and memory; revocation latency by stage and fraction of the network enforcing over time; residual harm (messages from a revoked attacker still accepted); detection precision, recall, time to detect, false accusations; CPU, RAM, storage, HSM utilization per node; traffic flow, density, speed, travel time, queue length, time-to-collision and safety-application outcomes; privacy metrics.
2. **Plotting:** live and post-run, any metric against time, distance, density, or any config variable; per-node, per-class, and network-wide aggregation; export as SVG, PNG, CSV, and a notebook-friendly Python API. Follow the repo's data-visualization conventions if any exist.
3. **Experiments:** a first-class experiment definition that sweeps variables (vehicle count, attacker fraction, RSU density, protocol, radio technology, weather, hardware profile), runs seeds for confidence intervals, stores results with provenance, and renders side-by-side comparisons. Example: the same city, same demand, SCMS vs. ETSI vs. umbrella-threshold-PQ, at 100, 500, 2,000, and 5,000 vehicles.

## 12. Datasets and exports

1. Preserve the current MA-perspective dataset family (features and labels separated, leakage linter, datasheet, benchmark, splits) as one exporter.
2. Add exporters for: per-receiver message logs with ground truth (VeReMi-compatible where sensible), per-node telemetry time series, network traces (a PCAP-like or Parquet event log of every frame with outcome and cause), backend event logs, and a full deterministic event log that the UI can replay.
3. Schema versioning, Parquet plus JSONL, and a manifest that lets any run be reproduced byte for byte.

## 13. Visualization and UI

1. **2D bird's-eye view:** the map, all actors moving smoothly, signals, RSUs with coverage, overlays toggled independently (transmission pulses, communication links, channel busy ratio heatmap, revoked and attacker markers, detection events, backend flows), and time controls (play, pause, step, speed, scrub).
2. **3D world view:** click a vehicle in 2D and the camera animates down to a follow or first-person view of that vehicle driving through the 3D city, with a HUD showing the full OBU state from section 9.2 and live sparklines. Provide chase, dashboard, and free-fly cameras and a way to jump between vehicles and RSUs. Three.js is my default suggestion; instanced rendering and LOD are expected. Use `https://github.com/standardagents/jevpilot` as a reference for how a Three.js browser driving simulator keeps a kinematic vehicle moving smoothly: it renders with Three.js and Vite, uses a kinematic model rather than a physics engine, generates candidate paths and route searches in a background worker, and has the renderer smoothly blend sampled poses. Its code has no declared license, so use it for ideas only.
3. **Inspector panels:** for any node, protocol entity, or link, show current state plus a "why" panel that names the model and parameters that produced each displayed value (the transparency requirement made visible).
4. **Configuration editor and comparison views:** every scenario field editable with validation and help text, presets, an events timeline editor, and side-by-side comparison of two or more runs.
5. **Performance budget:** propose and justify targets, for example 60 frames per second with 5,000 rendered vehicles, sub-100 ms seek when scrubbing a recording, and a rendering pipeline that survives 10,000 actors by aggregating what is off screen.
6. **Engine-to-UI protocol:** a versioned streaming protocol (likely WebSocket with binary snapshot deltas) used identically by the live engine and by the replay reader.

## 14. Configuration and scenarios

1. One scenario file format (YAML or JSON) that completely determines a run: world, actors, demand, radio, protocol, node profiles, attackers, detectors, metrics, exporters, fidelity tiers, seeds, and an events timeline (demand surges, weather fronts, road closures, attack waves, geofenced attack zones, RSU and backend outages, protocol parameter changes at time t).
2. A published JSON Schema with help text and units for every field, presets, validation with actionable errors, and migration between schema versions.
3. Equivalent access through CLI, Python API, and HTTP/WebSocket API, so copilots and notebooks are first-class users.

## 15. Methodology and transparency

1. **Model registry and model cards.** Every model (mobility, propagation, MAC, hardware profile, protocol, attacker, detector, metric) registers with a card: purpose, equations or algorithm, parameters with defaults, units, sources, assumptions, limitations, fidelity tier, validation status, version. The docs site is generated from the registry so it cannot drift from the code.
2. **Provenance per value.** Any number shown in the UI or written to a dataset can be traced to the model and parameters that produced it.
3. **Validation plan.** For each model family, name the published results it will be checked against (for example packet delivery ratio versus distance curves for 802.11p in urban settings, channel busy ratio versus density, SUMO-equivalent flow-density relationships, verification throughput versus datasheet numbers) and how the checks are automated.
4. **Testing strategy.** Unit tests per model, golden determinism tests, interface conformance tests that any plug-in must pass, validation tests against literature, and performance benchmarks with tracked regressions.
5. **Documentation.** Architecture guide, methodology guide, extension tutorial with one example plug-in per interface, and a glossary of standards terms.

## 16. Architecture decisions you must make

Provide a decision matrix and an ADR for each.

1. **Core engine language and runtime.** Candidates should include at least: Python (current) with acceleration; Rust core with Python bindings and a WebAssembly build for in-browser runs; C++ core; TypeScript core sharing a language with the UI. Criteria: determinism across platforms, performance at 10,000 nodes, plug-in authoring ergonomics for researchers who mostly write Python, browser story, cross-platform builds, testability, licensing.
2. **Time model.** Discrete-event simulation for the network and backend with microsecond resolution, fixed-step integration for mobility, and how the two are scheduled together deterministically; parallelism strategy; spatial indexing for neighbor and interference queries.
3. **Mobility provider.** Native engine vs. SUMO co-simulation vs. both behind one interface.
4. **Network fidelity source.** Native PHY/MAC implementations validated against literature vs. integration with ns-3 or OMNeT++/INET as separate processes (licensing and complexity) vs. both.
5. **Plug-in mechanism.** In-process (which languages), out-of-process over a protocol, or both; how plug-ins declare their model cards; how versions are pinned in manifests.
6. **Recording and replay format**, and the engine-to-UI protocol.
7. **UI stack.** Three.js for 3D; the 2D layer (canvas, WebGL, deck.gl, or Three.js orthographic); plotting library; application framework; how the UI is served and packaged.
8. **Repository layout and build**, including monorepo vs. multiple packages, one-command setup, and CI on three platforms.
9. **Performance targets** with the evidence that the chosen architecture can meet them, such as at least 10,000 vehicles at real time or faster in the abstract tier headless on a laptop, and about 1,000 vehicles with full PHY/MAC at a tenth of real time; push back on these with numbers if they are wrong.

## 17. Canonical research questions the design must trace end to end

For at least three of these, walk through exactly which modules, plug-ins, events, metrics, and exporters are involved from scenario file to figure, to prove the architecture closes the loop:

1. How does network load and OBU compute load change when replacing SCMS with an umbrella-threshold post-quantum scheme in a dense downtown, and where does fragmentation start to hurt packet delivery?
2. From the misbehavior authority's global viewpoint, produce a labeled dataset for anomaly detection in SCMS under a mix of ghost-vehicle, Sybil, and evasive attackers.
3. How does revocation scale as an urban city's traffic grows: CRL size, download time, processing cost on the OBU, time until 95 percent of vehicles enforce, and residual harm.
4. For an authenticated attacker, how long until detection, how long until the MA issues a revocation, and how long until it propagates, as a function of RSU density and cellular coverage.
5. DSRC vs. LTE-V2X vs. NR-V2X under identical traffic and security configuration.
6. One vehicle, then two, then three: what does each node do second by second, and does every behavior match the standards.

## 18. Deliverables and format

Write into `docs/design/` (this brief is `00-design-brief.md`) and `docs/adr/`:

1. `01-inventory.md`: the existing-code inventory and disposition from section 0, step 1.
2. `02-architecture.md`: system overview, component diagram, data flow, time model, determinism model, fidelity ladder, plug-in system, engine-to-UI protocol, repository layout.
3. `03-interfaces.md`: every plug-in interface with method signatures, data types, invariants, and the model-card schema; the scenario schema outline; the event log schema.
4. `04-models.md`: the model catalog with, for each model, its tiers, equations, parameters, defaults with sources or `TODO: calibrate`, and validation references.
5. `05-protocols.md`: the protocol interface and the sketches of SCMS, ETSI, and the threshold/umbrella/PQ family against it, including the message sequence diagrams over the modeled network.
6. `06-node-models.md`: OBU, RSU, backend, and cellular models and the initial hardware profiles with sources.
7. `07-threats-and-detection.md`: attacker model, catalog, detection and response pipeline, privacy metrics.
8. `08-measurement-and-data.md`: metric catalog, experiment system, exporters, dataset compatibility and migration.
9. `09-ui.md`: 2D, 3D, HUD, inspector, config editor, comparison, performance budget, with wireframes as ASCII or SVG.
10. `10-roadmap.md`: phased plan with acceptance criteria per phase. Phase 1 must be a thin vertical slice that proves the architecture: one vehicle on a small real-city map with buildings, native mobility, one radio tier, real 1609.2-style signed BSMs at 10 Hz, one hardware profile with the OBU HUD live in 2D and 3D, a recorded run replayed in the UI, and one metric plotted. Phase 2 adds a second vehicle, an RSU, and the SCMS backend path with a revocation. Later phases scale up fidelity, actors, protocols, attackers, and experiments. Include a risk register and an estimate of effort per phase.
11. `11-open-questions.md`: grouped by impact, each with the assumption you proceeded under.
12. ADRs for each decision in section 16, plus one superseding or confirming ADR 0001.
13. A short executive summary at the top of `02-architecture.md` that a new collaborator can read in five minutes.

Diagrams are expected wherever they clarify (component, sequence, state, data flow). Prefer Mermaid or SVG in the markdown.

## 19. Reference material to consult

Verify everything against primary sources; the lists below are starting points, not facts to copy.

- **Standards:** IEEE 1609.2, 1609.2.1, 1609.3, 1609.4; SAE J2735, J2945/1, J3161/1; ETSI EN 302 637-2 (CAM), EN 302 637-3 (DENM), TS 103 097, TS 102 941, TS 102 940, TS 103 759 (misbehavior reporting), TS 102 687 and TS 103 175 (DCC), EN 302 663 (ITS-G5 access layer), EN 302 636 (GeoNetworking/BTP), TS 103 300 (VRU/VAM), TS 103 324 (CPM); 3GPP Release 14 and 16 sidelink (LTE-V2X Mode 4, NR-V2X Mode 2); CAMP SCMS proof-of-concept design documents and the USDOT SCMS documentation; FIPS 204 (ML-DSA), FIPS 205 (SLH-DSA), Falcon specification.
- **Simulators and stacks to mine for models (respect licenses):** SUMO (EPL-2.0), Eclipse MOSAIC (EPL-2.0), Veins and its obstacle-shadowing model (GPL), Artery and Vanetza (GPL/LGPL), ns-3 WAVE and ms-van3t (GPL), OMNeT++/INET, F2MD (no license; reference only), PLEXE, OpenCDA and CARLA for 3D co-simulation ideas, WiLabV2Xsim / LTEV2Vsim for C-V2X PC5 models.
- **Datasets:** VeReMi and VeReMi NextGen (the submodule path exists but is not checked out), the existing MA dataset family in this repo.
- **Hardware:** public datasheets and benchmarks for commercial OBUs and RSUs (DSRC and C-V2X units), automotive HSMs, and published ECDSA and post-quantum verification throughput on embedded ARM cores; liboqs and PQClean benchmark tables.
- **Maps and 3D data:** OpenStreetMap and the Overpass API, osmnx, OSM building heights, open building-footprint datasets, SRTM or Copernicus DEM, SUMO netconvert and osmWebWizard.
- **Rendering:** Three.js (instancing, LOD, orthographic cameras), deck.gl or PixiJS for the 2D layer, uPlot, ECharts, or Plotly for plots; the jevpilot repository as described in section 13.2.
- **In this repo:** ADR 0001 links a prior design document as a claude.ai artifact; read it if you can reach it, otherwise proceed from the ADR.

## 20. Placeholders I still owe you

- The thresholding and umbrella-thresholding protocol specifications (section 8.3).
- Any specific hardware platform I want as the reference OBU profile (section 9.1); if I do not provide one, pick a well-documented commercial unit and say why.
- Priority order among the canonical research questions (section 17); if I do not provide one, assume questions 1, 3, and 4 come first.
