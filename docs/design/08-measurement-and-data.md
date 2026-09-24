# 08 — Measurement, experiments, exporters, dataset compatibility

Status: design draft for review (2026-09-18). Interfaces in `03-interfaces.md` §10 and §14; legacy contract in `01-inventory.md` §5.

## 1. Principles

- Every metric is computed by a `MetricProvider` with a `MetricDef` (name, unit, dimensions, aggregation, visibility, definition text, source). The catalog page is generated from the definitions, so the docs cannot drift from the code.
- Metrics read the typed event channels (03-interfaces §14), never engine internals, so a metric provider written in Python sees exactly what a Rust one sees.
- Visibility tags propagate: a metric derived from a `GT` channel is `GT`; exporters refuse to put `GT` and `NODE` columns in one file unless declared `mixed` and tagged.
- Every exported file carries the manifest hash; every figure carries it in metadata.

## 2. Metric catalog (initial set; each row becomes a `MetricDef`)

Dimensions: `t` (time bin), `node`, `class` (car/truck/bus/moto/vru/rsu), `dist_bin` (25 m), `density_bin` (veh/km per lane or veh/km²), `region`, `protocol`, `rat`, `tier`, `run`.

### 2.1 Radio and network
| Metric | Definition | Unit | Dims | Source channel |
|---|---|---|---|---|
| `pdr` | received frames / frames that were candidate receptions (receiver within the tier's candidate range) | ratio | t, dist_bin, density_bin, rat | `phy.rx` |
| `pdr_by_cause` | share of losses per `LossCause` | ratio | t, dist_bin, cause | `phy.rx` |
| `cbr` | channel busy ratio as measured by the MAC (802.11p: fraction of 100 ms window with CCA busy above −85 dBm; C-V2X: TS 38.215 §5.1.27 / TS 36.214 definition) | ratio | t, node, channel | `mac.cbr` |
| `pir` | packet inter-reception time between successive receptions from the same transmitter at a receiver | s (p50, p95, p99) | t, dist_bin | `phy.rx` |
| `e2e_latency` | generation time to application delivery (includes queueing, air time, verification) | ms | t, node | `node.tx`, `node.verify` |
| `nar` | neighborhood awareness ratio: fraction of transmitters within range r whose last message was received within T (default r = 100–300 m, T = 1 s) | ratio | t, node, r | `phy.rx`, `gt.kinematics` (mixed, GT-tagged) |
| `airtime_per_node` | transmitted air time per node per second | ms/s | t, node | `node.tx` |
| `bytes_air`, `bytes_uu_ul`, `bytes_uu_dl`, `bytes_backhaul`, `bytes_backend` | bytes per accounting bucket | B/s | t, node/link | `node.tx`, `net.*`, `proto.msg` |
| `frag_reassembly_fail` | SDUs not reassembled / SDUs fragmented | ratio | t, node, msg type | `net.frag` |
| `frag_loss_amplification` | P(SDU lost) / P(frame lost) | ratio | t | `net.frag`, `phy.rx` |
| `dcc_state_time` | time share per DCC state | ratio | t, node | `node.tx` (dcc state field) |
| `hidden_terminal_share` | losses with cause `HiddenTerminal` / all collisions (high MAC tier) | ratio | t | `phy.rx` |

### 2.2 Security processing
| Metric | Definition | Unit | Dims |
|---|---|---|---|
| `verify_rate` | verifications completed per second | 1/s | t, node, primitive |
| `verify_queue_depth` | verification queue length (p50/p95/max) | count | t, node |
| `verify_wait` | enqueue → start | ms | t, node |
| `verify_drops` | items dropped by policy or overflow | count | t, node, reason |
| `unverified_ratio` | messages delivered to applications without verification / delivered | ratio | t, node |
| `cert_change_events` | pseudonym/AT changes | count | t, node |
| `topup_bytes`, `topup_latency` | download size and request-to-available time | B, s | t, node |
| `p2pcd_requests`, `p2pcd_responses` | per second | 1/s | t |
| `full_cert_share` | messages carrying a full certificate / all | ratio | t, node |
| `hsm_util`, `cpu_util`, `ram_used`, `storage_used` | node resource accounting | %, B | t, node |

### 2.3 Revocation and residual harm (per protocol stage ids, 05-protocols §8)
| Metric | Definition | Unit |
|---|---|---|
| `crl_entries`, `crl_bytes` | current list size | count, B |
| `crl_download_time`, `crl_processing_time`, `crl_processing_memory` | per node per list | s, s, B |
| `revocation_latency_stage` | time between consecutive stage timestamps for one revocation | s per stage |
| `enforcement_fraction` | fraction of nodes at `enforced(node)` vs time since `decision` | ratio vs t |
| `t95_enforce` | time until 95 % of nodes enforce | s |
| `residual_harm` | messages from a revoked (or should-be-revoked) device accepted by any node after `decision`; also after `issued` and after `published` | count, and count per receiver |
| `passive_starvation_time` | time until the last valid credential of a blocklisted device expires | s |

### 2.4 Detection
| Metric | Definition |
|---|---|
| `det_precision`, `det_recall` | over reports (subject truly misbehaving) and over vehicles (revoked ∩ attacker), as in `validate.py` |
| `time_to_detect` | attack onset → first correct report at the MA |
| `time_to_decision` | onset → MA decision |
| `false_accusations` | benign vehicles with ≥ 1 report / with revocation |
| `detector_reliability` | per detector: precision of reports citing it (legacy definition) |
| `rsu_contribution` | share of reports from RSUs and their precision |

### 2.5 Traffic and safety
| Metric | Definition |
|---|---|
| `flow`, `density`, `mean_speed`, `travel_time`, `queue_length` | per edge/lane/region per minute (GT) |
| `ttc_min`, `pet`, `drac` | surrogate safety measures per conflict (GT); thresholds per 04-models §11 |
| `warning_true`, `warning_false`, `warning_missed` | safety-application outcomes joined with GT (mixed, tagged) |

### 2.6 Privacy (07-threats §6)
`linkability_rate` (fraction of pseudonym changes an observer links correctly), `anonymity_set_size`, `degree_of_anonymity` (TR 103 415 §5.1.2), `tracking_duration` (mean tracked duration, Wiedersheim et al. 2010 method).

### 2.7 Built 2026-09-23: per-attempt evidence, decomposed delay, awareness, load, overhead

What `v2xw-metrics` computes from a real run, and where each number comes from. The
definitions, units, citations and "not accounted for" lists are on each `MetricDef`; this
table is the map.

**Channels.** `node.rx` (NODE+GT): one record per `phy.rx` attempt, followed to exactly one
fate — `delivered` (with `verified`/`unverified`), `lost` with one cause from a closed
vocabulary (the PHY's causes, then `reassembly-failed`, `rx-overflow`, `verify-policy-drop`,
`verify-overflow`, `signature-invalid`, `revoked`, `receiver-off`), or `in-flight` at the end
of the run — carrying received power, SINR, distance, bytes, air time and every stamp of the
message's journey on the true timeline. `msg.latency` (NODE or NODE+GT): a
`LatencyTrace` — contiguous stages from origin to delivery — for any flow that is not a V2V
broadcast (the misbehaviour report's vehicle → RSU → backhaul flow is the first user;
credential top-up, CRL distribution and multi-hop relays plug in with `TraceBuilder`).
`net.bytes`: one record per transfer on a bucket other than the air. `node.tx` gains the
signing and MAC stamps and the frame's per-layer octets; `mac.cbr` gains queue depth, drops
and offered load; `gt.kinematics` names the mounted node.

**The frame.** A PSDU is payload + security envelope + network/transport header (WSMP 5 B for
a BSM; GN SHB + BTP-B 44 B for a CAM) + LLC/SNAP 8 B + 802.11 QoS Data MAC header 26 B + FCS
4 B (`v2xw_net::frame`, 04-models §4.6, §7). Air time is computed over the whole PSDU. The
uncertain sizes (LLC framing under WSMP; the QoS Data clause) are stated on the module.

**End-to-end delay** (`e2e_latency`, p50/p95/p99 per flow and message type) is decomposed
into `sign_queue`, `sign`, `mac_aifs`, `mac_backoff`, `mac_defer`, `airtime`, `propagation`,
`reception`, `verify_queue`, `verify`. The stages tile generation-to-delivery, so they sum to
the total exactly in integer nanoseconds (`latency_stage`, `latency_stage_share`). The
verification queue runs in continuous time: a frame waits from the instant it arrived, and a
backlog outlives the engine's tick.

| Family | Metrics | Sources |
|---|---|---|
| Awareness | `aoi` (time-average age of information), `aoi_peak`, `nar` at 100 and 300 m, `delivery_ratio` (application-level PDR) in 50 m bins to 1 km, `pir` | Kaul et al. SECON 2011 and INFOCOM 2012; Costa et al. IEEE T-IT 2016; Boban and d'Orey IEEE TVT 2016 (from memory; equation number unverified); Martelli et al. INFOCOM 2012 |
| Load | `cbr`, `channel_occupancy` (per-node CR), `channel_load` (offered air time around a node against the channel's one second per second), `offered_load`, `carried_load`, `loss_rate` per cause, `collision_rate`, `half_duplex_rate`, `mac_queue_depth`, `mac_drops`, `mac_access_delay`, `airtime_per_node` | TS 36.214 §5.1.31, TS 103 574; Torrent-Moreno et al. IEEE TVT 2009; EN 302 571 §4.2.10.1; IEEE 802.11-2020 §10.23.2 |
| Overhead | `security_overhead`, `net_header_overhead`, `link_overhead`, `cert_bytes_share`, `air_bytes_per_payload_byte`, `bytes_per_vehicle_hour` per bucket, `bytes_total` | 04-models §4.6, §7, §9.3; IEEE 1609.2 §6.3.4 |

**Consistency, tested on synthetic streams and on a real run** (`crates/v2xw-engine/tests/
measurement.rs`): M-RX1 delivered + lost + in flight = attempts, and a PHY loss is the same
loss on both channels; M-LAT1 stages sum to the total; M-BYTE1 a frame's layers sum to its
octets; M-BYTE2 the buckets sum to `bytes_total`; M-SHARE the stage shares sum to one;
M-RANGE no sample leaves its metric's physical range (a delivery ratio above one, a negative
delay). Each was shown to fail on an injected violation.

**Live view.** A metric streams as its headline, a distribution's p50/p95/p99
(`e2e_latency.p95`), and one series per value of a declared breakdown
(`latency_stage[airtime]`, `loss_rate[collision]`); the Studio's measurement picker lists
every series with its unit, definition and citation. Breakdowns a metric does not declare
(per node, per distance bin of `pdr`, per message type of a stage) are in the recording and
`metrics.json`.

## 3. Plotting

- Live: uPlot sparklines and small multiples fed by `metric.sample` (09-ui §5).
- Post-run: Plotly figures from the Python API (`v2xw.plot.metric("pdr", x="dist_bin", by="rat", runs=[...])`) with SVG/PNG/CSV export; the Studio uses Plotly.js with the same figure JSON so a figure built in the UI reproduces in a notebook.
- Presets for the canonical research questions (§7) ship as figure templates.
- If the repository adopts a data-visualization convention, figures inherit it (none exists today).

## 4. Experiment system

An `experiment` block in the scenario (03-interfaces §13):

```yaml
experiment:
  name: scms-vs-etsi-vs-umbrella-downtown
  base: scenarios/downtown-1km2.yaml
  sweep:
    security.protocol.id: [scms-camp, etsi-ts102941, umbrella-threshold-pq]
    actors.vehicles.demand.target_count: [100, 500, 2000, 5000]
  seeds: {count: 10, master: 0x5eed}
  replications_policy: {ci: 0.95, method: bootstrap}
  outputs: [metrics.parquet, figures/*.svg, comparison.html]
  resources: {parallel: 4, tier_overrides_for_large_n: {radio.phy: medium}}
```

- Runs are cells of the cartesian product × seeds; each cell writes its own manifest and MCAP; the experiment manifest lists all cell hashes.
- Aggregation computes means and confidence intervals across seeds with the declared method; results land in one Parquet table with cell keys as columns; a comparison page renders side-by-side figures and a manifest diff.
- Sweeps over discrete plug-in choices (protocol, RAT, hardware profile, weather, attacker mix) are first-class, replacing the legacy `massive.py` factorial grids, `campaign.py` and the foundry's executor with one mechanism; the legacy grids (`quick`, `medium`, `full`) are shipped as experiment presets.
- Execution: local process pool by default; a `--runner slurm|k8s` hook later; deterministic cell ordering so partial reruns resume.

## 5. Exporters

| Exporter | Files | Visibility | Notes |
|---|---|---|---|
| `ma-dataset` | `ma/*.jsonl`, `ground_truth/*.jsonl`, `ml/*` (parquet + csv + `schema.json`), `manifest.json`, `DATASHEET.md` | MA / ORACLE separated | port of `datagen` (§6) |
| `receiver-logs` | per-receiver Parquet/JSONL of received messages with node-visible fields, plus a GT file keyed by message id (VeReMi-style separation; VeReMi field names mapped where they exist) | NODE + separate GT | `--veremi-json` writes VeReMi-compatible JSON per receiver |
| `telemetry` | `node.telemetry` time series per node (Parquet) | NODE | HUD-equivalent data |
| `net-trace` | every frame with outcome and cause (`phy.rx`, `node.tx`, `mac.cbr`, `net.frag`) as Parquet; optional PCAP-NG with a custom link type carrying the serde-encoded frame record | NODE (+ GT tx id in a separate column file) | "PCAP-like" per the brief |
| `backend-log` | `proto.msg`, `proto.revocation`, entity queue samples | NODE | flows and stages |
| `recording` | MCAP with all channels (ADR 0008) | mixed, channel-tagged | replay; snapshot channels hold the VWP frames verbatim, every other channel holds serde records (03-interfaces §14) |
| `metrics` | `metric.sample` as Parquet | derived | experiments |

Schema versioning: every file carries `schema` (e.g., `v2xw/receiver-logs/1`); migrations are per version step with tests; the manifest lists schema ids. Parquet is the primary format; JSONL is emitted where the legacy contract requires it.

## 6. MA dataset compatibility and migration

Contract preserved (01-inventory §5): file set, field names, canonical JSON bytes, sort orders, id formats, `detnorm_*` vocabulary, `report_correctness` vocabulary, `ml/` tables and `schema.json` kinds, manifest fields, datasheet sections; `verify_data.py` must pass unchanged on v1-profile output.

Two profiles:

- `schema_versions: {ma_visible: 1, ground_truth: 1}` (**v1 profile**, `--legacy-v1`): identical columns to today, including the known defects (constant `valid_from/valid_to`, all-True `cert_validity`, `cert_crl_status="active"`, investigations only on revocation). Value semantics differ because the engine differs (real radio, real queues), which is documented in the datasheet's "Generator" line; downstream consumers that pin the schema keep working.
- **v2 profile** (default): additive columns only. `ma_reports` gains `verification_status`, `verify_latency_ms`, `rx_rssi_dbm`, `rat`; `ma_cert_status` gets real validity windows and the real `crl_status`; `ma_investigations` includes dismissed cases; `ma_crl_events` reports `num_entries_delta` alongside the cumulative field; new tables `ma_crl_downloads` (per node, MA-visible via RA logs) and `ma_report_transport` (submission path, delay). `ground_truth` gains `gt_kinematics_sample` (replaces the legacy `gt_emissions_sample` semantics with speed and heading noise now modeled) and `gt_revocation_stages`. `ml/` tables keep every v1 column and add features for the new MA-visible fields; `schema.json` marks new columns; the benchmark's exclusion set is extended so new metadata never leaks.
- `datagen.featurize`, `benchmark`, `validate`, `calibration`, `verify_data` run unchanged on both profiles (they read files); `datasheet.py` is refactored to read the new manifest keys with fallbacks.
- Migration tool: `v2xw dataset migrate --from v1 --to v2` (adds columns with nulls) and `--to v1` (drops), both preserving digests per file.
- Leakage: the linter runs on every exporter output in CI; the forbidden-key rules are unchanged and extended with `gt_`-prefixed names from the new tables.

## 7. Canonical research questions: end-to-end traces

### RQ1 — SCMS vs umbrella-threshold-PQ in a dense downtown; where fragmentation hurts

1. Scenario: `downtown-1km2` template; `radio.rat: dsrc-80211p`, tiers `propagation: high, phy: high, mac: high`; `messages.codec: uper`; `security.protocol` swept over `scms-camp` (ECDSA-P256, implicit certs, 450 ms attachment) and `umbrella-threshold-pq` (ML-DSA-65 leaf signatures, umbrella certificate fragmented per the protocol's `SignerIdPolicy::Fragment`); `nodes.profiles.default_obu` swept over `cohda-mk5` and `pq-capable-hypothetical`; experiment sweep on vehicle count.
2. Modules: `MessageGenerator` (BSM 10 Hz + J2945/1 rate control), `SecurityEnvelope` (sizes exact), `Fragmenter` (reassembly with loss amplification), `Mac`/`Phy` high tier (airtime, CBR, collisions), `NodeRuntime` (verification queue with cost tables), `Dcc`.
3. Events: `node.tx` (bytes, mcs, airtime), `phy.rx` (outcomes, causes), `net.frag`, `node.verify`, `mac.cbr`, `node.telemetry`.
4. Metrics: `bytes_air`, `airtime_per_node`, `cbr`, `pdr` by distance and density, `frag_reassembly_fail`, `frag_loss_amplification`, `verify_queue_depth`, `unverified_ratio`, `cpu_util`, `hsm_util`.
5. Exporters: `metrics`, `net-trace`, `telemetry`; figure preset `rq1`: PDR vs distance per protocol per density; CBR vs vehicle count; verification queue p95 vs vehicle count; the density at which `frag_reassembly_fail` exceeds 5 % is read off the third panel.

### RQ3 — Revocation scaling with city traffic

1. Scenario: `district` template with `actors.vehicles.demand` swept (1k–10k active), `security.protocol: scms-camp`, `revocation.cadence: daily` and `continuous`, RSU density swept, `cellular.uu_model: measured-lte`, attackers 2 % with `ghost`, `sybil`; `time.duration_s` 7 days with time-dilation windows (radio only during three 20-minute rush windows per day).
2. Modules: `MaPipeline` (legacy-window), `Responder`, SCMS `RevocationMechanism` (linkage seeds; CRL Store + RSU broadcast + optional epidemic), `CellularUu`, `Backhaul`, OBU CRL store with expansion cost (2 SHA-256 + 2·jmax AES per entry per period).
3. Events: `proto.revocation` stages, `net.*` deliveries, `node.verify` (CRL processing tasks), `node.telemetry` (CRL memory), `gt.attack.action`.
4. Metrics: `crl_entries`, `crl_bytes`, `crl_download_time`, `crl_processing_time`, `crl_processing_memory`, `t95_enforce`, `enforcement_fraction`, `residual_harm`.
5. Exporters: `metrics`, `backend-log`, `ma-dataset` (v2); figure preset `rq3`: CRL size vs revoked count; download time by path (RSU vs cellular); t95 vs traffic; residual harm vs cadence.

### RQ4 — Authenticated attacker: time to detect, decide, propagate vs RSU density and cellular coverage

1. Scenario: `downtown-1km2`, one `insider` attacker with valid credentials starting at t = 300 s, `detection.local: legacy-12`, `detection.ma: legacy-window`, sweep `rsus.count` ∈ {0, 4, 12, 24} and `cellular.coverage_fraction` ∈ {0, 0.5, 1.0}; `security.protocol` ∈ {scms-camp, etsi-ts102941}.
2. Modules: detectors, report outbox with store-and-forward, RSU report forwarding over backhaul, RA shuffle batching (ETSI: EA/MA path), MA pipeline, revocation (active CRL vs passive starvation), CRL distribution.
3. Events: `det.observation`, `ma.report`, `ma.case`, `ma.decision`, `proto.revocation`, `node.neighbor`.
4. Metrics: `time_to_detect`, `time_to_decision`, `revocation_latency_stage`, `t95_enforce` (SCMS) and `passive_starvation_time` (ETSI), `residual_harm`, `false_accusations`.
5. Exporters: `ma-dataset` (v2) and `metrics`; figure preset `rq4`: stacked latency by stage vs RSU density, one panel per coverage level, both protocols.

### RQ2 (dataset), RQ5 (RAT comparison), RQ6 (one, two, three vehicles)

RQ2 uses the `ma-dataset` exporter with the attacker mix `ghost`, `sybil`, `evasive-crl-aware` over the `district` template and the foundry to fill blind spots; RQ5 is a sweep on `radio.rat` with identical traffic and security; RQ6 is the Phase 1–2 scenario with `exporters: [recording, net-trace, telemetry]` and the Studio's step mode; the standards conformance checklist (10-roadmap Phase 2 acceptance) is the figure.

## 8. Testing the measurement layer

Metric providers have unit tests with synthetic event streams and known answers; exporters run the leakage linter and `verify_data` in CI; the experiment runner has a determinism test (rerunning a cell reproduces its digest) and a resume test.

## 9. World-data licensing in exports

Research finding (OSM Foundation "Produced Work" guideline, accessed 2026-09-18): a raster map image is a Produced Work, but a machine-readable export that preserves OSM geometry and topology at the same granularity is very likely a **Derivative Database** and stays under ODbL with share-alike and attribution; the guideline itself calls the lane-geometry case unresolved. Overture keeps ODbL on its OSM-derived Buildings and Transportation themes rather than relicensing them; Copernicus GLO-30 requires its attribution and liability text; Microsoft building footprints are CDLA-Permissive-2.0; Google Open Buildings are CC-BY-4.0 or ODbL at the user's choice (R10 §A).

Design consequences:

- Exported datasets separate **simulation data** (messages, telemetry, traces, MA tables: CC-BY-4.0 per ADR 0001) from **world geometry** (a `world/` bundle with its own `LICENSE` derived from `WorldProvenance`: ODbL for OSM- or Overture-derived worlds, CDLA/CC-BY for others, Apache-2.0 for procedural worlds). Simulation data references the world by content hash; positions in simulation data are coordinates, not a copy of the road graph.
- The exporter refuses to write a world bundle without a licence file and writes the required attribution strings (OSM contributors, Overture, DLR/Airbus for Copernicus) into `DATASHEET.md` and the manifest.
- Whether coordinate-only simulation data derived by driving on an OSM network is itself a derivative database is an open legal question (11-open-questions); the conservative default is to publish world bundles under ODbL and simulation data under CC-BY-4.0 with the world bundle attached.
