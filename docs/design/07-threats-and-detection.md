# 07 — Threats, detection, response, privacy

Status: design draft for review (2026-09-18). Interfaces in `03-interfaces.md` §9; protocol hooks in `05-protocols.md` §2.6; metrics in `08-measurement-and-data.md`.

## 1. Attacker model

An attacker is a plug-in that sees only an `AttackerView` and acts only through an `AttackerApi` (03-interfaces §9). What it can see and do is declared in `Capabilities`; the engine enforces the declaration:

| Capability | Values | Enforcement |
|---|---|---|
| Credentials | `None` (no valid credentials), `Own(n)` (its own n concurrent pseudonyms/ATs; SCMS default 20 per week, ETSI ≤ 100 or 20 under the C2C-CC profile, 05-protocols §2.2), `Stolen(set)` (credentials extracted from k other devices), `CompromisedRsu(id)` | `AttackerApi::use_credential` only accepts handles in the declared set |
| Radio | max power (dBm), can jam (yes/no), channels, RAT | the MAC/PHY reject frames outside the declared envelope |
| Knowledge | public CRL/CTL (yes/no), map (yes/no), neighbors via own receptions (always), sensing/perception (yes/no) | the view only contains what is declared |
| Coordination | `CoalitionId` with a modeled channel (V2X or out-of-band with latency) | coalition messages are ordinary modeled messages |
| Compute | a `HardwareProfile` | signing/flooding rates are bounded by the attacker's own node runtime |
| Position | honest kinematics only; an attacker cannot teleport its body, only its claims | `AttackerApi` has no mobility control beyond driving behaviours available to any driver |

Goals and strategies are the attacker's own logic (safety disruption, tracking, DoS, evasion, framing); the catalog below fixes the mechanisms, and `AttackSchedule` gives every attacker duty cycle, onset jitter, geofence, and wave membership (from the legacy scenario events, 01-inventory §3.3).

Invariants: I-T1 no ground-truth access (compile-time sentinel); I-T3 every action that changes bytes on the air is logged on a GT channel with the true actor id.

## 2. Attack catalog

### 2.1 Ported from the legacy engine (28 types, renderings preserved as defaults)

| Family | Types (legacy names kept) | Mechanism in the new engine |
|---|---|---|
| position | ConstPos, ConstPosOffset, RandomPos, Teleport, SineWavePos | `FalsifyOutgoing` edits the position fields before signing; magnitudes as in the legacy formulas with intensity `k` |
| speed | ConstSpeedOffset, RandomSpeed, StopAndGo | same, speed field |
| heading | ReversedHeading, HeadingOffset | same, heading field |
| combined | Disruptive, PosSpeedInconsistent, PosHeadingInconsistent, EventualStop | several fields |
| timing | DataReplay (own history replay), DelayedMessages, OutOfOrder, DoS, DoSRandom | `Replay`, `Delay`, generation-time edits, rate increase bounded by the node's MAC and DCC (a DoS attacker that disables DCC is a distinct capability) |
| stealth | SlowDrift, AlongRoadOffset | small along-road offsets |
| identity | Sybil (ghost transmissions from concurrent valid pseudonyms), VruImpersonation, VruPositionSpoof | `UseCredential` cycling across the attacker's own concurrent credentials; station-type field edits |
| credential | InvalidSignature, ExpiredCert, NotYetValid | envelope-level edits (wrong key token, expired/not-yet-valid certificate) |
| event | FakeHazard (phantom DENM) | `FalsifyOutgoing` on DENM generation |
| collusion (cross-cutting) | colluding reporters framing victims with fabricated evidence | `ForgeReport` with the legacy fabricated-evidence distributions |
| evasion (cross-cutting) | CRL-aware dormancy | reads the public CRL through the view (declared knowledge), not the MA's internal state |

### 2.2 New types required by the brief

| Type | Mechanism | Needs |
|---|---|---|
| Ghost vehicles | transmit BSMs for non-existent vehicles from one radio using extra valid pseudonyms (Sybil) or from an RSU-class attacker; positions generated along lanes (map knowledge) so map-off-road checks pass | `Own(n)` or `Stolen`, map |
| Sybil with concurrent valid pseudonyms | as above, with per-ghost consistent kinematics and rotation strategies | `Own(n)` |
| Replay and delay | store received messages, retransmit later or elsewhere (relay attack between two attackers) | radio, coalition for wormhole |
| Message suppression | selective non-forwarding (GN multi-hop, DENM keep-alive, CRL/CTL rebroadcast) | RSU or forwarding role |
| PHY jamming and flooding | `TransmitRaw` with a jammer profile: constant, reactive (triggered by CCA busy), random duty cycle, power up to declared max; modeled as an interferer in the SINR sums (04-models §12.3) | can jam |
| Oversized-message flooding | high-rate transmissions of maximum-size SPDUs (PQ or threshold profiles) to exhaust receivers' verification budgets and reassembly buffers | valid or invalid credentials; DCC disabled as a capability |
| Compromised RSU | false SPaT/MAP, false CRL/CTL (signed with the RSU's real credentials or unsigned), report suppression or poisoning at the forwarding stage | `CompromisedRsu` |
| Insider with valid credentials | any falsification while holding a fully valid credential set; combined with evasion and rotation | `Own(n)` |
| Misbehavior-report poisoning | forged reports against honest vehicles with plausible evidence; report floods to exhaust MA budgets | valid credentials |
| False DENM / CPM events | phantom hazards, phantom perceived objects in CPM | valid credentials |
| Certificate misuse across regions | using credentials outside their validity region (1609.2 `region`, ETSI AT region) | `Own(n)` |
| Location tracking (privacy attacker) | passive observer with RSU-like receivers at configurable density; links pseudonyms across changes using kinematic continuity (Kalman/MHT) | knowledge: none beyond receptions |
| Coordinated campaigns | coalition of any of the above with a modeled coordination channel (V2X or cellular with latency) | coalition |

### 2.3 Foundry

The MAP-Elites foundry (01-inventory §3.5) is ported over the new `Attacker` interface: the genome becomes a scenario overlay (attack mix, magnitudes, density, topology template, weather, realism toggles, hardware profile), the descriptor keeps the four legacy axes (family, density band, topology, attacker band) and gains `rat` and `protocol`, the objective reads `det_recall`, `time_to_detect`, or `residual_harm` from the metrics, and the LLM mutation operator hook is unchanged. Elite replay is a scenario file plus a seed.

## 3. Detection

### 3.1 Local detectors (on-node)

The legacy 12-detector suite plus the Kalman soft feature is ported as the `legacy-12` plug-in with the same formulas and normalisation (`detnorm ≈ 1 at threshold`, 01-inventory §3.3), because the MA dataset's ML contract depends on those signals. Changes: per-receiver tracker state instead of a global dict; float-equality frozen check replaced by a tolerance; inputs are the node's verified messages, its own position estimate and its neighbor table; costs are charged to the node CPU per message (`Detector::cost`).

New detector families (each a plug-in with a model card): TS 103 759 observation classes 1–5 (implausible values; inconsistency with previous messages from the same station; with the local environment/LDM including map and signal state; with on-board sensors, i.e., perception cross-check; with other stations' messages); F2MD-style checks re-implemented from the paper descriptions (range, position, speed, consistency, sudden appearance, beacon frequency) with their thresholds as cited defaults in 04-models §14; perception cross-check (claimed position vs sensed objects within the sensor FOV, with occlusion in the medium tier) for ghost-vehicle research; CPM consistency (perceived objects vs own perception).

### 3.2 Misbehavior authority pipeline

`MaPipeline` receives reports through the protocol's reporting transport (real delays and batching) and runs: ingestion and validation (re-verify evidence signatures, charge the MA's service model), correlation (per-subject windows; the legacy operating point k = 3 trusted reporters, 4 distinct seconds, 3 s span, 15 s window, reporter budget 30 and reputation cap 40 ships as `legacy-window`), investigation (protocol-specific identity resolution as a flow with round trips: SCMS PCA + LA queries; ETSI EA lookup), decision (revoke, dismiss, suspend, alert), and hand-off to the `Responder`, which issues the protocol's revocation flow. Researchers can replace any stage with a Python plug-in (e.g., an ML model over the MA dataset features) and get scored by the same metrics.

### 3.3 Response

The responder emits the revocation flow and the engine timestamps every stage (05-protocols §8), so revocation latency decomposes into report transport, MA processing, resolution round trips, issuance, distribution per path, download, processing, and enforcement, per node.

## 4. Ground truth, labels, and the leakage firewall

Attack actions are recorded on `gt.attack.action` with the true actor, the fields changed and the magnitude, which yields per-message `falsified` labels (legacy rule: position error > 1 m, speed > 1 m/s, heading > 5°, extra messages, stale generation time, bad signature, bad certificate, or false station type) and per-vehicle labels. Detectors, the MA and all exporters marked `NODE` never see those channels; the leakage linter runs on every exported feature table.

## 5. Safety outcomes

Safety applications (FCW, EEBL, IMA, VRU warning, 04-models §11) consume the neighbor table; their warnings are joined with ground truth offline to count true, false (ghost-induced) and missed warnings, and surrogate safety measures (TTC, PET, DRAC) quantify the physical consequence of attacker-induced braking or lane changes when the mobility tier lets drivers react to warnings (a scenario option).

## 6. Privacy metrics

- **Observer model:** passive receivers at RSU sites with configurable density and coverage; the observer's tracker is a plug-in (default: Kalman-filter multi-hypothesis tracking as in Wiedersheim et al. 2010, whose results showed that at 1 Hz beacons a change interval ≥ 4 s and 20 % penetration already yields near-100 % tracking success [WONS 2010]).
- **Linkability rate:** fraction of pseudonym changes the observer links correctly, by change strategy (time, distance, mix zone, silent period), density and RSU density.
- **Anonymity set at change:** Ψ = target plus vehicles within the observer's confusion region at the change instant; effective size S = −Σ p_u log₂ p_u and degree of anonymity d = S / log₂|Ψ| with p_u the observer's posterior (ETSI TR 103 415 §5.1.2).
- **Tracking duration:** mean correctly tracked time per vehicle (WONS 2010 method).
- **Strategy defaults to compare:** SCMS 5 min / 2 km; C2C-CC BSP 10–30 min random after a 1-minute change at ignition; C2C-CC segment strategy (800–1,500 m then ≥ 800 m and 2–6 min); PRESERVE's 120 s plus 3–13 s silent period (05-protocols §2.4, all cited there).
- Concurrent-pseudonym count is reported alongside, since TR 103 415 §8 notes that the Sybil surface grows with it.

## 7. Conformance and evaluation harness

Every attacker ships a model card and a "minimal reproduction" scenario; every detector ships expected precision/recall on the Phase 2 reference scenario; the evaluation harness (`v2xw eval detectors`) runs a detector set against the attack catalog with seeds and produces the same tables the legacy `validate.py` and `benchmark.py` produce, so results remain comparable with the current repository's numbers.
