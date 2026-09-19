# 06 — Node models: OBU, RSU, backend, cellular, and hardware profiles

Status: design draft for review (2026-09-18). Interfaces in `03-interfaces.md` §8; costs of primitives in `04-models.md` §9; protocol entities in `05-protocols.md`. Section 7 (initial hardware profiles with sources) is appended from the hardware research sheet; every number there carries a source or a `NOT PUBLISHED` / `TODO: calibrate` tag.

## 1. Hardware profile schema

A hardware profile is a data file (`profiles/hardware/<id>.yaml`) that a node references; it is loaded into the registry like a model and gets a model card with `family: hardware-profile`. Fields:

```yaml
id: obu/cohda-mk5                 # registry id
kind: obu | rsu | vru-device | backend-server | base-station | hsm-appliance
sources: [{kind: datasheet, ref: <url>, accessed: 2026-09-18}, …]
cpu: {cores: 2, clock_hz: 1.0e9, arch: armv7-a, dmips: 4000, source: <ref>}     # any field may be {value: null, status: not-published, calibration: "…"}
ram_bytes: {value: 1073741824, source: <ref>}
flash_bytes: {value: 4294967296, source: <ref>}
hsm:
  kind: secure-element | soc-hsm | none | accelerator
  part: "NXP SXF1700"
  ops:                              # per primitive id; latency and throughput; where the op runs
    ecdsa-p256-verify: {throughput_per_s: 2000, latency_us: null, runs_on: hsm, source: <ref>}
    ecdsa-p256-sign:   {throughput_per_s: null, latency_us: 50000, runs_on: hsm, source: <ref>}
  queue_depth: 64                    # TODO: calibrate
software_crypto:                     # fallback cost table when an op is not in the HSM (per primitive; cycles or µs)
  ecdsa-p256-verify: {us: 645, source: "OpenSSL 1.1.1d on Cortex-A72: 1,550 verify/s"}
  ml-dsa-44-verify: {us: 311, source: "liboqs on Raspberry Pi 4: 3,214/s"}
radio:
  rat: [dsrc-80211p] | [lte-v2x-pc5] | [nr-v2x-pc5] | hybrid
  chipset: "NXP SAF5100"
  tx_power_dbm: {min: 10, max: 23, default: 20, source: <ref>}   # regulatory limits applied by the PHY separately
  sensitivity_dbm: {per_mcs: {…} | null, source: <ref>}
  noise_figure_db: {value: 9, source: "3GPP TR 36.885 assumption" }
  antenna: {gain_dbi: 3, height_m: 1.5, pattern: omni, source: <ref>}
power_w: {value: null, status: not-published}
storage_model: {cert_bytes_per_entry: 120, crl_bytes_per_entry: 40}    # sources in 04-models §9 and 05-protocols §3
cost_table_overrides: {}             # optional per-op overrides measured on this device
```

Rules: (H1) every numeric field has a `source` or a `status: not-published` with a `calibration` note; the registry's `todo-calibrate` page lists them; (H2) a profile without an HSM must declare `hsm.kind: none`, in which case all crypto runs on the CPU using `software_crypto`; (H3) profiles are versioned; the manifest pins them.

## 2. OBU runtime model

The OBU runtime is the node-side counterpart of the protocol and radio plug-ins. It is a set of queues and servers whose parameters come from the hardware profile, plus stores whose sizes grow with real message and certificate sizes. Everything below is inspectable live (HUD, 09-ui §5) and exported on `node.telemetry`.

### 2.1 Queues and servers

```mermaid
flowchart LR
  PHY[PHY rx] --> RXQ[rx queue]
  RXQ --> PARSE[parse + policy<br/>CPU]
  PARSE -->|verify plan| VQ[verify queue<br/>policy: all | on-demand | prioritized]
  VQ --> HSM[HSM server<br/>c_hsm servers, FIFO]
  VQ --> CPUV[CPU crypto<br/>when no HSM or PQ]
  HSM --> APP[app queue<br/>detectors, neighbor table, safety apps]
  CPUV --> APP
  APP --> CPU[CPU server<br/>c cores, processor sharing]
  GEN[generators<br/>BSM/CAM timers] --> SIGN[sign: HSM or CPU]
  SIGN --> TXQ[tx queue] --> MAC[MAC/DCC]
  CRL[CRL processing tasks] --> CPU
  TOPUP[top-up, report outbox] --> NET[Uu / RSU path]
```

- **Tiers** (02-architecture §7): `abstract` = infinite servers (zero service time) but all sizes and counts recorded; `medium` = one CPU server (FIFO) and one HSM server (FIFO) with cost tables; `high` = `c` CPU cores with processor sharing, priority classes, memory accounting, storage growth, HSM with queue depth and latency distribution.
- **Service times** come from the primitive cost tables (04-models §9) scaled by the profile: `t = cycles / clock_hz` when a cycle count exists for the architecture family, else the measured µs on the closest benchmarked platform with the platform named in the provenance record. Application tasks (parse, detector, neighbor update) carry per-model cost classes declared in their model cards (`cost` field) with `TODO: calibrate` defaults measured on the reference laptop and scaled by DMIPS ratio (an explicit, documented assumption).
- **Verification policies** (03-interfaces §6): `verify-all` (FIFO), `on-demand` (verify only messages that a safety application marks relevant; others delivered unverified and flagged), `prioritized` (priority by relevance score and distance; oldest-drop when the queue exceeds `queue_depth`). Each decision is logged on `node.verify` with the policy's reason so `unverified_ratio` and `verify_drops` are exact.
- **Half-duplex and MAC coupling**: the tx queue hands frames to the MAC; DCC gating decisions feed back as `GateDecision` events; a saturated tx queue drops by age with cause `tx_queue_overflow`.

### 2.2 Stores

| Store | Content | Size accounting | Costs |
|---|---|---|---|
| Certificate store | own pseudonyms/ATs with validity windows, private keys or butterfly reconstruction material, enrollment credential, trust anchors | entries × encoded size (04-models §9) | reconstruction on download; expiry sweep per period |
| Peer certificate cache | digest → certificate learned from full-certificate messages or P2PCD | entries × certificate size; LRU with configurable capacity (`TODO: calibrate`) | lookup per message; miss triggers P2PCD request per policy |
| CRL store | list entries per series; expansion table for the current i-period | entries × entry bytes (≈ 40 B SCMS, 05-protocols §3) plus expanded linkage values (jmax × 9 B per entry per period) | per period: 2 SHA-256 + 2·jmax AES per entry (05-protocols §3.2); lookup per message: hash-set probe |
| Trust store | CA certificates, CTL/ECTL, policy files | bytes | signature verifications on update |
| Neighbor table | per sender digest: last messages, kinematic track, verification state, revoked flag, relevance score | entries × ~200 B (`TODO: calibrate`) | update per message; expiry after T_neighbor (default 3 s, `TODO: calibrate` against J2945/1 path-history retention) |
| Evidence buffer | recent messages per sender for detectors and reports | ring per sender, bounded | — |
| Report outbox | pending misbehavior reports with store-and-forward state | bytes; drop policy after 1 week [CAMP-EE §2.2.8] | upload when a path exists |
| Top-up state | next-batch time, downloaded weeks | — | download bytes per 05-protocols §3.2 |

### 2.3 Clock and position

- `ClockModel`: GNSS-locked time when a fix exists; on GNSS loss the believed time drifts at the oscillator's rate (profile field; TCXO ppm values from 04-models §3.8 with sources or `TODO: calibrate`). Generation-time fields in messages use the believed time, so replay and plausibility checks see realistic offsets.
- `PositionEstimate`: from the `GnssModel` (04-models §3.8): OU bias, white noise, outliers, degrade bursts, outages, jamming; the confidence ellipse is computed from the model's own noise parameters, never from the true bias (fixes the legacy defect noted in 01-inventory §3.3).

### 2.4 Resource accounting (`NodeTelemetry`)

`cpu_util` (busy fraction per core), `hsm_util`, `ram_used` (stores + queues + fixed baseline from the profile), `storage_used`, queue depths (rx, verify, app, tx, crl) with p50/p95 over the sampling window, drops by cause (`rx_overflow`, `verify_policy_skip`, `verify_overflow`, `tx_overflow`, `reassembly_timeout`, `crl_processing_backlog`), messages in/out per second, verifications per second, current DCC state, CBR, neighbor counts by verification state, certificate counters (active, stored, next top-up), CRL counters (entries, bytes, expansion progress), GNSS fix quality, clock drift, outbox size.

### 2.5 Node lifecycle

Vehicles power on (cold-start costs: trust store checks, CRL catch-up expansion for missed periods, top-up if due), enter and leave the map, park (radio off after a configurable idle time), and power off; certificates expire mid-run per their validity windows; a node that finds itself on the CRL stops transmitting [CAMP-EE §2.2.10.2].

## 3. RSU model

An RSU is a node with a fixed antenna (profile `kind: rsu`, height and gain from the profile, site from the world), a backhaul link (`Backhaul` model: fibre/Ethernet, cellular, or none), and roles configured per scenario: CRL/CTL distribution proxy (periodic broadcast per the protocol's distribution path), certificate provisioning proxy (forwards EE ↔ RA traffic), report forwarding (EE → MA path), SPaT and MAP broadcaster (rates from 04-models §8), WSA broadcaster, and optional detector host (RSU-side local detection with the same detector plug-ins). It has the same queue/server structure as the OBU with a larger profile, and failure and compromise states: `down` (no transmissions, backhaul lost), `degraded` (backhaul latency multiplier), `compromised` (an `Attacker` with `CompromisedRsu` capability controls its broadcasts and forwarding).

## 4. Backend entities and their service models

Every backend entity (SCMS: RA, PCA, LA1, LA2, MA, CRLG, PG, DCM, LOP, CRL Store; ETSI: EA, AA, TLM, CPOC, DC, MA; threshold: committee members, coordinator) is a node on the backend network with:

- a hardware profile (`kind: backend-server`; default from the profile set in §7: a cloud VM class with an ECDSA verify/sign rate and an optional HSM appliance),
- a `ServiceModel`: `abstract` = fixed latency per request type; `medium` = M/M/c per entity (arrival stream from the flows, service time exponential with mean from the cost of the request's crypto operations plus a fixed overhead, `c` from the profile) with batching windows (RA shuffle: 10,000 requests or 1 day in the PoC [CAMP-EE §2.2.7]; report shuffle: 10,000 or 1 day [CAMP-EE SCMS-765]; CRL cadence per the revocation mechanism); `high` = per-request service-time distributions (log-normal with parameters `TODO: calibrate`), retries and timeouts, and an availability model (up/down intervals from the scenario events or a two-state Markov model with parameters `TODO: calibrate`),
- storage counters that grow with issued certificates, stored request hashes, reports, and CRL entries (05-protocols §3.1),
- links in the backend network (`BackendNet`: per-link latency and bandwidth; defaults from 04-models §10 with sources).

Nothing in a backend entity is instantaneous: a misbehavior report is decrypted, validated (signature verifications charged), stored, batched, and forwarded; a CRL is assembled, signed, published to the store and to the RSU broadcast path; an investigation is a sequence of round trips whose latency is the sum of link delays and service times (05-protocols §3.2).

## 5. Cellular base stations and backhaul

A base station is a node with a site, an antenna height, a coverage model (04-models §10: `abstract` coverage map, `medium` per-cell capacity with a measured latency distribution and load-dependent scheduling delay, `high` handover interruption and outages), a backhaul link to the core, and a core-network link to the backend. Uplink and downlink bytes are accounted separately (`bytes_uu_ul`, `bytes_uu_dl`). Vehicles out of coverage keep reports and top-up requests in store-and-forward until coverage or an RSU is available.

## 6. Perception model (optional)

`Perception` produces detections of physically present actors from the node's own sensors: `abstract` = detections within a sensor range and field of view with a range-dependent detection probability (parameters and sources in 04-models §12); `medium` = occlusion by buildings and vehicles using the world's obstacle model; `high` = per-sensor error models (range/bearing noise) and false detections. Detections feed the perception cross-check detector and the CPM generator.

## 7. Initial hardware profiles

Appended from the hardware research sheet (see §7.1 onward). Each profile lists every field with its source; fields vendors do not publish are marked `NOT PUBLISHED` with a calibration plan, and the reference OBU choice is justified in 11-open-questions.

Conventions for §7.1–§7.8: every value carries an inline citation of the form `[R7 §X: source]` or `[R5 §B.n: source]` into the research sheets (in-repo copies under `docs/design/research/`, byte-identical to the scratchpad originals; URLs in §7.10). `NOT PUBLISHED` and `UNVERIFIED` tags are carried exactly as the sheets record them. A field with no published value is written `{value: null, status: not-published, calibration: "…"}` per rule H1, and the registry's `todo-calibrate` page must list it. Reciprocals of published ops/s figures are marked `derived: 1/ops_per_s`; no other arithmetic is applied. Fields marked `# ext` are extensions beyond the §1 schema (GNSS, OS, environmental) consumed by `GnssModel` (§2.3) or kept as provenance; the loader ignores unknown keys. Three numbers in §7.1–§7.8 are 3GPP evaluation assumptions rather than device data (vehicle antenna height 1.5 m, UE-type RSU antenna height 5 m, UE receiver noise figure 9 dB; and the macro-cell figures in §7.8); they come from the cellular research sheets R2/R2c/R11, not R7/R5, and are cited as such.

### 7.1 `obu/cohda-mk5` — commercial DSRC OBU

The Cohda MK5 is the unit the roadmap's first vertical slice pins (10-roadmap) and the most widely fielded DSRC OBU in the USDOT pilots' era, so it is profiled first even though its crypto rates are the least documented: Cohda publishes CPU rating, memory, radio and GNSS figures but nothing about SXF1700 sign or verify throughput [R7 §A1: Cohda MK5 OBU brief 2024]. The verify rate is therefore `NOT PUBLISHED` and proxied by the NXP SAF5400 verification engine's documented 2,000 messages/s [R7 §D5: NXP SAF5400 fact sheet] — a *later* RoadLINK baseband than the MK5's SAF5100, so the proxy is flagged in the manifest. The two documented TX-power/sensitivity figure sets (system-level brief vs. bare-module datasheet) are both carried, as R7 §A1 does, rather than reconciled.

```yaml
id: obu/cohda-mk5
kind: obu
version: 2026-09-18.1
sources:
  - {kind: product-brief, ref: "Cohda MK5 OBU product brief (2024) [R7 §A1; URL §7.10 #1]", accessed: 2026-09-18}
  - {kind: datasheet, ref: "Cohda MK5 radio module datasheet v1.2.0 (May 2015), FCC filing [R7 §A1; §7.10 #2]", accessed: 2026-09-18}
  - {kind: fact-sheet, ref: "NXP SAF5400 fact sheet SAF5400V2XFS REV 2 [R7 §D5; §7.10 #12]", accessed: 2026-09-18}
cpu:
  cores: 2                                   # "NXP i.MX6 DL (dual-core)" [R7 §A1: Cohda MK5 OBU brief 2024]
  clock_hz: {value: null, status: not-published, calibration: "brief gives only the DMIPS rating; read the core clock from the NXP i.MX6 DualLite datasheet or /proc/cpuinfo on the device"}
  arch: {value: null, status: not-published, calibration: "brief names the SoC (i.MX6 DL) but not the core; confirm core type and ISA from the NXP i.MX6 DualLite datasheet before choosing a cycle-count table"}
  dmips: 4000                                # "rated 4000 DMIPS" [R7 §A1: Cohda MK5 OBU brief 2024]
  source: "[R7 §A1: pdftxt/cohda_mk5_obu_brief_2024.txt]"
ram_bytes: {value: 1073741824, source: "1 GB SDRAM [R7 §A1: Cohda MK5 OBU brief 2024]"}
flash_bytes: {value: 4294967296, source: "4 GB eMMC; the brief lists a further 4 GB storage, not counted here [R7 §A1: Cohda MK5 OBU brief 2024]"}
os: "Ubuntu 20.04 LTS, Linux 4.14.98 [R7 §A1]"          # ext
hsm:
  kind: secure-element
  part: "NXP SXF1700"                        # [R7 §A1: Cohda MK5 OBU brief 2024]
  ops:
    ecdsa-p256-verify:
      throughput_per_s: {value: 2000, status: proxy, source: "NOT PUBLISHED by Cohda for the SXF1700 [R7 §A1]; PROXY = NXP SAF5400 integrated verification engine, 2000 messages/s, Brainpool or NIST 256-bit [R7 §D5: NXP SAF5400 fact sheet]. SAF5400 is a later baseband than the MK5's SAF5100; flag in manifest."}
      latency_us: {value: null, status: not-published, calibration: "measure on device (Cohda SDK security API timing); until then service time = 1/throughput (500 us, derived) with a single HSM server"}
      runs_on: accelerator                   # on NXP RoadLINK parts verification is in the baseband engine, signing in the SE [R7 §D4 note, §D5]
      source: "[R7 §A1, §D5]"
    ecdsa-p256-sign:
      throughput_per_s: {value: null, status: not-published, calibration: "NOT PUBLISHED for SXF1700 [R7 §A1]; request NXP SXF1700 datasheet under NDA or bench on device"}
      latency_us: {value: null, status: not-published, calibration: "NOT PUBLISHED; nearest documented same-role figure is the Infineon SLI97/SLE97 family '<50 ms' performance-profile requirement [R7 §D2] — use only as a pessimistic placeholder and flag in manifest"}
      runs_on: hsm
      source: "[R7 §A1: HSM verify/sign throughput NOT PUBLISHED by Cohda for the SXF1700]"
  queue_depth: {value: null, status: todo-calibrate, calibration: "no published host-SE queue depth; measure with back-to-back sign requests on device"}
software_crypto:                             # no software ECDSA benchmark on the i.MX6 DL exists in R7/R5
  ecdsa-p256-verify: {us: null, status: not-published, calibration: "run `openssl speed ecdsap256` on the device (Ubuntu 20.04 ships OpenSSL); until then PROXY = Raspberry Pi 3B Cortex-A53 @ 1.2 GHz OpenSSL 775.3 verify/s (1,290 us, derived: 1/ops_per_s) [R7 §D8: dchest gist], chosen as the slowest benchmarked Cortex-A class; optimistic bound Pi 4 Cortex-A72 1,550.7/s (645 us) [R7 §D8: HimaJyun gist]; flag proxy in manifest"}
  ecdsa-p256-sign:   {us: null, status: not-published, calibration: "same measurement; PROXY = Pi 3B 1,631.1 sign/s (613 us, derived) [R7 §D8]"}
  ml-dsa-44-verify:  {us: null, status: not-published, calibration: "liboqs speed_sig on device; no Cortex-A53/A9-class liboqs figure exists in R5 §B.2 (UNVERIFIED / not found)"}
radio:
  rat: [dsrc-80211p]
  chipset: "NXP RoadLINK SAF5100"            # [R7 §A1: Cohda MK5 OBU brief 2024]
  bandwidth_hz: 10.0e6                       # ext; "10 MHz" [R7 §A1]
  tx_power_dbm:
    min: -10                                 # module datasheet: "-10 to +23 dBm/antenna port" [R7 §A1: Cohda MK5 module datasheet]
    max: 22                                  # system spec: "+22 dBm (ETSI Mask C)" [R7 §A1: Cohda MK5 OBU brief 2024]; module spec +23 dBm/port, +26 dBm effective with 2 antennas [R7 §A1: module datasheet] — both genuine, different measurement points, not reconciled
    default: 20                              # J2945/1 constant-rate baseline "20 dBm" used by Rostami et al. [R7 §H3]; scenario may override
    source: "[R7 §A1 (brief and module datasheet); R7 §H3]"
  sensitivity_dbm:
    system: -99                              # "-99 dBm @ 3 Mbps" [R7 §A1: Cohda MK5 OBU brief 2024]
    module: -97                              # "-97 dBm (5.9 GHz)" [R7 §A1: Cohda MK5 module datasheet]
    per_mcs: {value: null, status: not-published, calibration: "Cohda publishes the 3 Mbps point only; PROXY table = Autotalks CRATON (gen-1) per-MCS sensitivities in the Unex OBU-201U FCC filing [R7 §A5: unex_fcc.txt], see §7.2; flag as different-chipset proxy"}
    source: "[R7 §A1]"
  noise_figure_db: {value: 6, source: "NXP RoadLINK 'noise figure 6 dB @ 5.9 GHz', chip-level, listed by R7 for SAF5100/SAF5400 from the SAF5400 fact sheet [R7 §D5, §G: nxp_saf5400.txt]; system NF including front-end NOT PUBLISHED"}
  doppler_tolerance_kmh: 800                 # ext; [R7 §A1]
  delay_spread_tolerance_ns: 1500            # ext; [R7 §A1]
  antenna:
    gain_dbi: {value: null, status: not-published, calibration: "1 DSRC antenna bundled with the OBU kit, gain not stated in the brief [R7 §G]; measure or take the shark-fin datasheet; 3GPP evaluation assumption 3 dBi for a vehicle UE is the fallback [R2c: TR 36.885 Table A.1.1-1]"}
    height_m: {value: 1.5, status: evaluation-assumption, source: "3GPP TR 36.885 Annex A.1.1 vehicle-UE antenna height 1.5 m [R2 Topic A, R2c] — not a Cohda figure; the world's vehicle model may override per vehicle type (TR 37.885 Type 1/2/3: 0.75 / 1.6 / 3 m [R2c])"}
    pattern: omni
    source: "[R7 §G; R2c]"
gnss:                                        # ext, consumed by GnssModel (04-models §3.8)
  navigation_sensitivity_dbm: -167           # [R7 §A1]
  accuracy_m: 2.5                            # [R7 §A1]
power_w: {value: null, status: not-published, calibration: "OBU system draw not stated; radio module alone '4 W max' [R7 §A1: module datasheet]; supply 7–36 V DC [R7 §A1]; measure at 12 V bench supply"}
environment: {operating_temp_c: [-40, 85], dimensions_mm: [130, 120, 35]}   # ext; [R7 §A1]
storage_model: inherit                       # 04-models §9 / 05-protocols §3 defaults
cost_table_overrides: {}
```

What is missing for `obu/cohda-mk5`:
- SXF1700 ECDSA sign latency and throughput, and any SE verify rate — `NOT PUBLISHED` [R7 §A1]; R5 §C additionally records an `UNVERIFIED` search snippet ("35 verifications per second in the case of the software module") that is *not* used here.
- Core clock and ISA of the i.MX6 DL (brief gives DMIPS only) — `not-published`.
- Any software-crypto benchmark on this SoC — `not-published`; Pi 3B/Pi 4 proxies flagged.
- Per-MCS sensitivity beyond the 3 Mbps point; antenna gain; system noise figure; system power draw — `not-published`.
- HSM queue depth — `TODO: calibrate`.

### 7.2 `obu/unex-obu-301-craton2` — reference OBU profile (Autotalks CRATON2)

This is proposed as the **reference OBU profile** (open question A3 in 11-open-questions) because it is the only OBU in the hardware sheet with *all* of the following published: CPU core count, clock and DMIPS; RAM and flash; HSM signing throughput **and** latency; hardware verification throughput; RX threshold, TX power, antenna gain and GNSS sensitivity [R7 §A4, §A10: Unex OBU-301E / OBU-351U information sheets]. The same CRATON2 chipset ships in a DSRC variant (OBU-301E) and a C-V2X variant (OBU-351U) with identical compute and security figures, so one profile serves both RATs [R7 §A10, §B4]. The eHSM internals are further documented by the FIPS 140-2 security policy (Cortex-M0, 32 KB RAM, 128 KB ROM, dedicated accelerator) and the policy clarifies that the >2,500/s verification engine is a separate on-chip block outside the certified HSM boundary [R7 §A4: autotalks_craton2_secton_fips.txt]. Two caveats: Autotalks itself publishes no numeric rates on its product pages (the Unex sheets quote CRATON2 specs verbatim [R7 §A4]; R5 §C records the Autotalks white-paper figures ">2000 NIST / >1500 Brainpool verifications/s" as `UNVERIFIED`), and the 128 MB RAM is an order of magnitude below the Cohda units, which matters for the store-size accounting in §2.2.

```yaml
id: obu/unex-obu-301-craton2
kind: obu
version: 2026-09-18.1
reference_profile: true                      # proposed default OBU (11-open-questions A3)
sources:
  - {kind: info-sheet, ref: "Unex OBU-301E information sheet [R7 §A4, §A10: pdftxt/unex_obu301e.txt; no URL recorded]", accessed: 2026-09-18}
  - {kind: info-sheet, ref: "Unex OBU-351U information sheet [R7 §A4, §A10: pdftxt/unex_obu351u.txt; no URL recorded]", accessed: 2026-09-18}
  - {kind: security-policy, ref: "Autotalks CRATON2/SECTON eHSM FIPS 140-2 Security Policy, CMVP #3556 [R7 §A4; R5 §C; §7.10 #6]", accessed: 2026-09-18}
  - {kind: fcc-filing, ref: "Unex OBU-201U spec v2.03 (CRATON gen-1, per-MCS sensitivity proxy) [R7 §A5: unex_fcc.txt; §7.10 #5]", accessed: 2026-09-18}
cpu:
  cores: 2                                   # "dual 600 MHz ARM Cortex-A7 cores" [R7 §A4: Unex OBU-301E/351U]
  clock_hz: 6.0e8                            # [R7 §A4]
  arch: armv7-a                              # Cortex-A7 [R7 §A4]
  dmips: 2280                                # 1140 DMIPS per core × 2 cores (derived from the per-core rating) [R7 §A4]
  supervisor_core: "ARM Cortex-M3, MPU, ECC-protected memory"   # ext; [R7 §A4]
  source: "[R7 §A4: pdftxt/unex_obu301e.txt, pdftxt/unex_obu351u.txt]"
ram_bytes: {value: 134217728, source: "128 MB DDR3 (as integrated in OBU-301E) [R7 §A4]"}
flash_bytes: {value: 134217728, source: "128 MB NAND [R7 §A4]"}
hsm:
  kind: soc-hsm
  part: "Autotalks CRATON2 eHSM (dedicated ARM Cortex-M0, 32 KB RAM, 128 KB ROM, dedicated crypto accelerator, OTP) [R7 §A4: FIPS security policy]; HW P/N ATK66610 v2.1.2 [R5 §C: CMVP #3556]"
  certification: "FIPS 140-2 overall Level 3 (eHSM boundary) [R7 §A4]"
  curves: [nist-p224, nist-p256, nist-p384, brainpool-p256t1, brainpool-p384t1, brainpool-p256r1, brainpool-p384r1]   # [R7 §A4: FIPS security policy]
  ops:
    ecdsa-p256-sign:
      throughput_per_s: 110                  # ">110 signatures/s" (lower bound) [R7 §A4: Unex OBU-301E/351U]
      latency_us: 9000                       # "<9 ms signing latency" (upper bound) [R7 §A4]
      runs_on: hsm
      source: "[R7 §A4; §D7]"
    ecdsa-p256-verify:
      throughput_per_s: 2500                 # ">2500 ECDSA NIST P-256 verifications/s ('line-rate')" (lower bound) [R7 §A4]
      latency_us: {value: null, status: not-published, calibration: "engine latency not stated; upper bound 400 us per verify from 1/2500 s (derived) if modelled as a single server; measure pipeline depth on device"}
      runs_on: accelerator                   # separate on-chip HW verification engine, explicitly outside the certified eHSM boundary [R7 §A4: FIPS security policy]
      source: "[R7 §A4; §D7]"
    ecies-p256-decrypt:
      throughput_per_s: {value: null, status: not-published, calibration: "ECIES supported by the security subsystem (gen-1 datasheet [R7 §A5]); no CRATON2 rate published; bench on device"}
      latency_us: {value: null, status: not-published, calibration: "as above"}
      runs_on: hsm
      source: "[R7 §A4, §A5]"
  queue_depth: {value: null, status: todo-calibrate, calibration: "not published; measure with back-to-back sign requests"}
software_crypto:                             # no software benchmark on a 600 MHz Cortex-A7 exists in R7/R5
  ecdsa-p256-verify: {us: null, status: not-published, calibration: "run `openssl speed ecdsap256` on the OBU; PROXY = Pi 3B Cortex-A53 @ 1.2 GHz 775.3 verify/s (1,290 us, derived) [R7 §D8], the slowest benchmarked Cortex-A class; flag proxy in manifest"}
  ecdsa-p256-sign:   {us: null, status: not-published, calibration: "same; PROXY = Pi 3B 1,631.1 sign/s (613 us, derived) [R7 §D8]"}
  ml-dsa-44-verify:  {us: null, status: not-published, calibration: "liboqs on device; bracket between Cortex-M7 @ 216 MHz Dilithium-2 verify 6.6 ms [R7 §E1] (pessimistic) and Pi 4 Cortex-A72 ML-DSA-44 3,213.9/s = 311 us [R5 §B.2] (optimistic)"}
  falcon-512-verify: {us: null, status: not-published, calibration: "bracket between Cortex-M7 2.6 ms [R7 §E1] and Pi 4 7,866.3/s = 127 us [R5 §B.2]"}
radio:
  rat: [dsrc-80211p]                         # OBU-301E; variant OBU-351U is lte-v2x-pc5 with the same chipset [R7 §A10]
  chipset: "Autotalks CRATON2 + PLUTON2 RF transceiver"   # [R7 §A4]
  bandwidth_hz: 10.0e6                       # ext; "10 MHz (5/20 MHz by project)" [R7 §A10]
  tx_power_dbm:
    min: {value: null, status: not-published, calibration: "not stated for CRATON2; gen-1 CRATON unit had 4.5–25 dBm dynamic range [R7 §A5], different generation"}
    max: 20                                  # ">+20 dBm, Class C mask" (OBU-301E); "max +20 dBm" (OBU-351U) [R7 §A10]
    default: 20                              # J2945/1 constant-rate baseline [R7 §H3]
    source: "[R7 §A10]"
  sensitivity_dbm:
    system: -92                              # "RX threshold <-92 dBm, SAE J2945-compliant" [R7 §A10]
    per_mcs:                                 # PROXY, previous chipset generation (CRATON gen-1 in Unex OBU-201U) — UNVERIFIED for CRATON2 [R7 §A5: unex_fcc.txt]
      status: proxy-previous-generation
      mbps_3: -97
      mbps_4_5: -97
      mbps_6: -95
      mbps_9: -93
      mbps_12: -90
      mbps_18: -86
      mbps_24: -80
      mbps_27: -78
      note: "values vary slightly by column set in the source table [R7 §A5]"
    fading_10pct_per_6mbps_1000b:            # same gen-1 source [R7 §A5]
      rural_los: -92.5
      highway_los: -91.5
      urban_approach_los: -91.5
      crossing_nlos: -89.5
      highway_nlos: -88.5
    source: "[R7 §A10 (CRATON2 threshold); R7 §A5 (gen-1 per-MCS proxy)]"
  noise_figure_db: {value: 9, status: evaluation-assumption, source: "NOT PUBLISHED for CRATON2/PLUTON2; 3GPP TR 36.885 Annex A.1.1 UE receiver noise figure 9 dB [R2 Topic A; R2c] (schema default, §1)"}
  antenna:
    gain_dbi: 5                              # "2× detachable FAKRA-Z, 5 dBi omni dipole" [R7 §A10: Unex OBU-301E]
    height_m: {value: 1.5, status: evaluation-assumption, source: "3GPP TR 36.885 Annex A.1.1 vehicle-UE antenna height [R2 Topic A; R2c]; override per vehicle type"}
    pattern: omni
    source: "[R7 §A10]"
gnss:                                        # ext; Telit SL869-V3 [R7 §A10]
  module: "Telit SL869-V3"
  acquisition_sensitivity_dbm: -146
  navigation_sensitivity_dbm: -158
  tracking_sensitivity_dbm: -162
  accuracy_m_cep50_sbas: 1.5
power_w: {value: null, status: not-published, calibration: "not stated; input 6–48 V DC [R7 §A10]; measure at 12 V"}
environment: {operating_temp_c: [-40, 85], dimensions_mm: [103, 95, 31]}   # ext; [R7 §A10]
variants:
  obu/unex-obu-351u-craton2:                 # C-V2X sibling, same compute and HSM block [R7 §A10]
    radio: {rat: [lte-v2x-pc5], tx_power_dbm: {max: 20}, sensitivity_dbm: {system: -92, note: "typ. <-92 dBm"}, bandwidth_hz: [10.0e6, 20.0e6]}
storage_model: inherit
cost_table_overrides: {}
```

What is missing for `obu/unex-obu-301-craton2`:
- Verification-engine latency (only the throughput lower bound is published) — `not-published`.
- ECIES/ECDH rates on the eHSM — `not-published`.
- Any software-crypto benchmark on the 600 MHz Cortex-A7 (classical or PQ) — `not-published`; Pi 3B / Pi 4 / Cortex-M7 brackets flagged.
- Per-MCS sensitivity for CRATON2 itself — only the gen-1 CRATON table exists, carried as `proxy-previous-generation`.
- Minimum TX power, noise figure, power draw, HSM queue depth — `not-published` / `TODO: calibrate`.
- Autotalks' own chip-level figures are `UNVERIFIED` (white paper mirror 403) [R5 §C]; the profile relies on the Unex integrator sheets.

### 7.3 `obu/cohda-mk6c-qualcomm-9150` — commercial C-V2X OBU

The MK6C is the C-V2X counterpart of §7.1: Qualcomm 9150 PC5 modem, NXP i.MX 8QXP application processor and an NXP SXF1800 secure element [R7 §A3: Cohda MK6C EVK brief 2024]. Qualcomm never released a public 9150 datasheet, so cores, clock and any modem-side verification engine are `NOT PUBLISHED` [R7 §B1: Qualcomm 9150 press release 2017]; Cohda's EVK brief gives no RAM or flash. What makes this profile valuable anyway is that Twardokus, Bindel, McCarthy and Rahbari measured ECDSA, Falcon, Dilithium, SPHINCS+ and XMSS sign/verify times **on a Cohda MK6** ("Qualcomm ARMv8 V2V chipsets", Botan and liboqs, 1000-execution averages, no ARMv8 optimizations) [R7 §E3; R5 §B.6: NDSS 2024 Table V]; those are the software cost anchors below, with the paper's own caveat that the 0.001 ms ECDSA verify is anomalous and its results are "not expected to be reproducible" without the devices [R5 §B.2]. The production MK6 OBU (i.MX 8 @ 800 MHz, 8544 DMIPS, 1 GB, 16 GB, 2× SAF5400 + Qualcomm SA515) is recorded as a sibling variant, not merged, because it is a different product [R7 §A2: Cohda MK6 OBU brief 2025].

```yaml
id: obu/cohda-mk6c-qualcomm-9150
kind: obu
version: 2026-09-18.1
sources:
  - {kind: product-brief, ref: "Cohda MK6C EVK product brief (2024) [R7 §A3; §7.10 #3]", accessed: 2026-09-18}
  - {kind: press-release, ref: "Qualcomm 9150 C-V2X announcement, 2017-09-01 [R7 §B1: pdftxt/qualcomm_9150_pr_2017.txt]", accessed: 2026-09-18}
  - {kind: product-page, ref: "NXP SXF1800 product page and block diagram [R7 §D4; §7.10 #13]", accessed: 2026-09-18}
  - {kind: paper, ref: "Twardokus, Bindel, Rahbari, McCarthy, NDSS 2024 (ePrint 2022/483), Table V measured on Cohda MK6 [R7 §E3; R5 §B.6; §7.10 #22]", accessed: 2026-09-18}
  - {kind: product-brief, ref: "Cohda MK6 OBU product brief (2025) for the sibling variant [R7 §A2: pdftxt/cohda_mk6_obu_brief_2025.txt; no URL recorded]", accessed: 2026-09-18}
cpu:
  part: "NXP i.MX 8QXP application processor"          # [R7 §A3: Cohda MK6C EVK brief]
  cores: {value: null, status: not-published, calibration: "EVK brief names the SoC only; take core count from the NXP i.MX 8QXP datasheet. Sibling MK6 OBU brief: 'NXP i.MX 8, 8544 DMIPS, operating at 800 MHz' [R7 §A2] — sibling-product proxy, flag in manifest"}
  clock_hz: {value: null, status: not-published, calibration: "as above; sibling proxy 8.0e8 Hz [R7 §A2]"}
  arch: {value: null, status: not-published, calibration: "not restated in the brief; the NDSS 2024 measurements name the platform 'Qualcomm ARMv8' [R7 §E3]; confirm i.MX 8QXP core type from the NXP datasheet"}
  dmips: {value: null, status: not-published, calibration: "sibling MK6 OBU proxy 8544 DMIPS [R7 §A2]"}
  source: "[R7 §A3; sibling R7 §A2]"
ram_bytes: {value: null, status: not-published, calibration: "not in the EVK brief; sibling MK6 OBU 1 GB SDRAM [R7 §A2] as flagged proxy"}
flash_bytes: {value: null, status: not-published, calibration: "not in the EVK brief; sibling MK6 OBU 16 GB eMMC + microSD [R7 §A2] as flagged proxy"}
os: "Linux 4.9.88 [R7 §A3]"                             # ext
hsm:
  kind: secure-element
  part: "NXP SXF1800, FIPS 140-2 Level 3"                # [R7 §A3]
  internals: "Arm SC300 core; 2 MB flash (~1 MB used by software, 1 MB customer data); NXP JCOP with a V2X applet (ECDSA sign, ECIES, key mgmt) and a GS applet (secure generic-data / certificate storage); SPI host interface up to 5 Mbit/s (mode 0); CC EAL5+ platform, CC EAL4+ vs C2C-CC HSM PP v1.4.0, FIPS 140-2 L3 (L4 physical); -40 to +105 C; 15-year data retention [R7 §D4: NXP SXF1800 product page and block diagram]"   # ext
  ops:
    ecdsa-p256-sign:
      throughput_per_s: {value: null, status: not-published, calibration: "NOT PUBLISHED — NXP states only 'signature generation performance exceeding single and dual channel requirements, with low latency' [R7 §D4]; bench on device or request NXP datasheet under NDA"}
      latency_us: {value: null, status: not-published, calibration: "NOT PUBLISHED [R7 §D4, rollup #3]; software fallback on the host measured at 7.820 ms (see software_crypto) is the documented upper bound for the sign path"}
      runs_on: hsm
      source: "[R7 §A3, §D4]"
    ecdsa-p256-verify:
      throughput_per_s: {value: 1000, status: vendor-claim-system-level, source: "NXP marketing copy: 'ultra-fast verification on incoming messages at greater than 1000 messages per second' [R7 §D4: nxp.com/products/SXF1800] — R7 notes this reads as a system-level claim (verification is normally offloaded to the paired baseband engine), not necessarily SE-silicon-alone. On the MK6C the paired modem is the Qualcomm 9150, whose verify engine is NOT PUBLISHED [R7 §B1]; lower bound only"}
      latency_us: {value: null, status: not-published, calibration: "bench on device"}
      runs_on: {value: null, status: not-published, calibration: "unknown whether verification runs on the SXF1800, the 9150 modem, or the i.MX 8QXP host; the Qualcomm reference design 'includes ... an application processor running the ITS V2X stack, and an HSM' [R7 §B1]; determine from the Cohda SDK security configuration"}
      source: "[R7 §D4, §B1]"
    ecies-p256-decrypt:
      throughput_per_s: {value: null, status: not-published, calibration: "ECIES is in the V2X applet [R7 §D4]; no rate published"}
      latency_us: {value: null, status: not-published, calibration: "as above"}
      runs_on: hsm
      source: "[R7 §D4]"
  host_interface_bps: 5.0e6                              # ext; SPI up to 5 Mbit/s [R7 §D4] — a hard ceiling on SE request/response bytes per second
  queue_depth: {value: null, status: todo-calibrate, calibration: "not published; measure"}
software_crypto:                             # NDSS 2024 Table V, measured on a Cohda MK6 ('Qualcomm ARMv8'), Botan (ECDSA, Dilithium, XMSS) and liboqs (Falcon, SPHINCS+), 1000-execution means, no ARMv8 optimizations [R7 §E3; R5 §B.6]. Parameter sets for 'Falcon', 'Dilithium', 'SPHINCS+' are not restated in the R7/R5 extracts; confirm against Table V before mapping to a sized primitive id.
  ecdsa-p256-sign:   {us: 7820, sigma_us: 141, ops_per_s: 128, source: "[R7 §E3: NDSS 2024 Table V]"}
  ecdsa-p256-verify: {us: null, status: not-published, published_value_ms: 0.001, published_rate_hz: 675219, calibration: "as published, flagged ANOMALOUS by R7 §E3 (implies ~1.5 us, unusually fast for software ECDSA verify on an embedded ARMv8 core; possibly Botan precomputation/caching) and by R5 §B.2 ('treat with care'); do not use as a service time. PROXY = Pi 4 Cortex-A72 OpenSSL 1,550.7 verify/s (645 us, derived) [R7 §D8] until measured on device with a cold-cache harness"}
  falcon-sign:       {us: 2152, sigma_us: 36, ops_per_s: 465, source: "[R7 §E3: NDSS 2024 Table V]"}
  falcon-verify:     {us: 446, sigma_us: 23, ops_per_s: 2243, source: "[R7 §E3: NDSS 2024 Table V]"}
  dilithium-sign:    {us: 2634, sigma_us: 1741, ops_per_s: 380, source: "[R7 §E3]; large sigma reflects rejection sampling"}
  dilithium-verify:  {us: 189, sigma_us: 184, ops_per_s: 5299, source: "[R7 §E3]"}
  sphincs-plus-sign: {us: 5485, sigma_us: 2, ops_per_s: 182, source: "[R7 §E3]"}
  sphincs-plus-verify: {us: 5436, sigma_us: 191, ops_per_s: 184, source: "[R7 §E3]"}
  xmss-sign:         {us: 1405408, sigma_us: 31150, ops_per_s: 0.7, source: "[R7 §E3]"}
  xmss-verify:       {us: 2780, sigma_us: 381, ops_per_s: 359, source: "[R7 §E3]"}
radio:
  rat: [lte-v2x-pc5]                         # 3GPP R14 PC5 [R7 §A3, §B1]
  chipset: "Qualcomm 9150 (MDM 9150)"        # [R7 §A3; R5 §C: 'Based on the MDM 9150 chipset']
  bandwidth_hz: 20.0e6                       # ext; "20 MHz" [R7 §A3]
  tx_power_dbm:
    min: {value: null, status: not-published, calibration: "not stated; 3GPP UE Tx-power assumption 23 dBm, 33 dBm not precluded [R11 §A4: TR 37.885 Table 6.1.1-1] is the evaluation default if a range is needed"}
    max: 21.5                                # "C-V2X max TX power Class 3, 21.5 dBm" [R7 §A3]
    default: 20                              # J2945/1 constant-rate baseline [R7 §H3]; scenario may override
    source: "[R7 §A3; R7 §H3]"
  sensitivity_dbm:
    system: -93.4                            # "C-V2X RX sensitivity -93.4 dBm" [R7 §A3]
    per_mcs: {value: null, status: not-published, calibration: "single figure only; PROXY = Quectel AG15 B47/B46D 10 MHz set (Primary/Diversity -93, SIMO -96, 3GPP-SIMO-spec -90.4 dBm) [R7 §B2] or the PHY model's MCS-relative BLER curves (R2e)"}
    source: "[R7 §A3]"
  noise_figure_db: {value: 9, status: evaluation-assumption, source: "NOT PUBLISHED for the 9150; 3GPP TR 36.885 / TR 37.885 UE receiver noise figure 9 dB [R2 Topic A, D.2; R2c]"}
  antenna:
    gain_dbi: {value: null, status: not-published, calibration: "not stated; 3GPP evaluation assumption 3 dBi vehicle UE [R2c]"}
    height_m: {value: 1.5, status: evaluation-assumption, source: "3GPP TR 36.885 Annex A.1.1 [R2 Topic A; R2c]"}
    pattern: omni
    source: "[R2c]"
gnss: {value: null, status: not-published, calibration: "EVK brief gives no GNSS figures; sibling MK6 OBU 'advanced GNSS with RTK capability', no numbers [R7 §A2]"}   # ext
power_w: {value: null, status: not-published, calibration: "not stated; supply 6–24 V [R7 §A3]; sibling MK6 OBU backup draw '<1 mA @ 12 V' [R7 §A2] is standby only; measure"}
environment: {operating_temp_c: [-40, 85]}   # ext; [R7 §A3]
variants:
  obu/cohda-mk6-production:                  # different product; recorded for comparison [R7 §A2: Cohda MK6 OBU brief 2025]
    cpu: {part: "NXP i.MX 8", dmips: 8544, clock_hz: 8.0e8}
    ram_bytes: 1073741824
    flash_bytes: 17179869184                 # 16 GB eMMC (+ microSD)
    hsm: {part: "NXP SXF1800, FIPS 140-2 L3, CC EAL4+ (Mizar TTM2000 for the China variant)"}
    radio:
      rat: hybrid                            # 2× NXP SAF5400 (DSRC) + Qualcomm SA515 (LTE-V2X PC5, R14); 5G NR with LTE Cat 19/3G/2G fallback
      tx_power_dbm: {dsrc_max: 22, cv2x_max: 21.5}
      sensitivity_dbm: {dsrc_3mbps: -99}
      bandwidth_hz: [10.0e6, 20.0e6]
      dsrc_verify_engine: {throughput_per_s: 2000, source: "SAF5400 integrated verification engine [R7 §D5]; the MK6 carries two SAF5400 [R7 §A2]"}
    environment: {operating_temp_c: [-40, 74], dimensions_mm: [172, 168, 51]}
storage_model: inherit
cost_table_overrides: {}
```

What is missing for `obu/cohda-mk6c-qualcomm-9150`:
- i.MX 8QXP core count, clock, ISA and DMIPS; RAM and flash — `not-published` in the EVK brief (sibling MK6 OBU proxies flagged).
- Qualcomm 9150 cores/clock and any modem-side verification engine — `NOT PUBLISHED` [R7 §B1]; product page returned no body text [R5 §C].
- SXF1800 signing latency and throughput — `NOT PUBLISHED` [R7 §D4]; verify figure is a system-level vendor claim.
- Where verification actually runs on this unit — `not-published`.
- A usable software ECDSA verify cost (the published 0.001 ms is anomalous) — `not-published`, Pi 4 proxy flagged.
- PQ parameter sets behind the NDSS labels; min TX power; per-MCS sensitivity; antenna gain; GNSS; power draw — `not-published`.

### 7.4 `obu/generic-automotive-soc-no-hsm` — generic SoC, all crypto in software

This profile exists for scenarios that ask "what if the OBU has no secure element or accelerator" (rule H2: `hsm.kind: none`, everything charged to the CPU). It is anchored on the best-documented Cortex-A72-class software figures — Raspberry Pi 4B OpenSSL 1.1.1d for ECDSA [R7 §D8; R5 §B.5: HimaJyun gist] and liboqs on the same board for PQ [R5 §B.2: arXiv 2503.10238 Table 8] — with the Pi 3B/3B+ Cortex-A53 rows as the slower class and the Pi 5 wolfSSL/mbedTLS rows showing a 25× library spread on identical silicon [R5 §B.5]. All of these are community or paper benchmarks on developer boards, not automotive parts, and are flagged as proxies; commercial OBUs with unpublished HSM numbers (Danlaw AutoLink, Ficosa) supply the RAM/flash/power envelope [R7 §A7, §A8]. The Quectel AG15 modem, which explicitly leaves security to the host processor, supplies the C-V2X radio block [R7 §B2]. An "MCU with HSM" alternative built on the Infineon AURIX TC3xx HSM figures (200 sign/s, 100 verify/s at 100 MHz) follows as a variant [R7 §D1].

```yaml
id: obu/generic-automotive-soc-no-hsm
kind: obu
version: 2026-09-18.1
sources:
  - {kind: benchmark, ref: "Raspberry Pi 4 OpenSSL 1.1.1d `openssl speed` gist (HimaJyun) [R7 §D8; R5 §B.5; §7.10 #17]", accessed: 2026-09-18}
  - {kind: benchmark, ref: "Raspberry Pi 3B/3B+ `openssl speed` gist (dchest) [R7 §D8]", accessed: 2026-09-18}
  - {kind: paper, ref: "Berger, Lemoudden, Buchanan, 'Post Quantum Migration of Tor', arXiv 2503.10238 Tables 7–8 (liboqs on Pi 4/Pi 5) [R5 §B.2; §7.10 #19]", accessed: 2026-09-17}
  - {kind: benchmark, ref: "wolfSSL vs mbedTLS apples-to-apples benchmark, June 2026 (wolfSSL 5.9.1, mbedTLS 3.6.6) [R5 §B.5; §7.10 #18]", accessed: 2026-09-17}
  - {kind: hardware-design, ref: "Quectel AG15 hardware design guide [R7 §B2: pdftxt/quectel_ag15_hwdesign.txt; no URL recorded]", accessed: 2026-09-18}
  - {kind: datasheet, ref: "Danlaw AutoLink OBU [R7 §A7]; Ficosa C-V2X OBU [R7 §A8] (envelope only; local cache, no URL recorded)", accessed: 2026-09-18}
cpu:
  cores: 4                                   # PROXY: Raspberry Pi 4B, BCM2711, 4× Cortex-A72 [R7 §D8]; benchmarks below are single-core
  clock_hz: 1.5e9                            # PROXY: Pi 4B @ 1.5 GHz [R7 §D8]
  arch: armv8-a                              # Cortex-A72 class
  dmips: {value: null, status: not-published, calibration: "no DMIPS rating for BCM2711 in R7; scale application task costs by measured openssl ratio instead"}
  source: "[R7 §D8] — developer-board proxy for an automotive Cortex-A72-class SoC; flag in manifest. Commercial envelope: Danlaw AutoLink 'dual-core @ 800 MHz' [R7 §A7]"
ram_bytes: {value: 1073741824, status: proxy, source: "Danlaw AutoLink 1 GB RAM [R7 §A7] — representative commercial OBU without published HSM figures; scenario may override"}
flash_bytes: {value: 8589934592, status: proxy, source: "Danlaw AutoLink 8 GB eMMC [R7 §A7]"}
hsm:
  kind: none                                 # rule H2: all crypto on the CPU
  part: null
  ops: {}
  queue_depth: null
software_crypto:                             # single-core; us = 1/ops_per_s (derived) unless the source states a time
  # --- default class: Cortex-A72 @ 1.5 GHz, OpenSSL 1.1.1d (OS/bitness not stated in the gist) ---
  ecdsa-p256-sign:   {us: 244,  ops_per_s: 4097.4, source: "OpenSSL reports 0.0002 s/sign, 4097.4 sign/s [R7 §D8; R5 §B.5: HimaJyun gist]"}
  ecdsa-p256-verify: {us: 645,  ops_per_s: 1550.7, source: "OpenSSL reports 0.0006 s/verify, 1550.7 verify/s [R7 §D8; R5 §B.5]"}
  ecdsa-p384-sign:   {us: 8104, ops_per_s: 123.4,  source: "nistp384 123.4 sign/s [R5 §B.5: HimaJyun gist]"}
  ecdsa-p384-verify: {us: 5721, ops_per_s: 174.8,  source: "nistp384 174.8 verify/s [R5 §B.5]"}
  ecdsa-brainpoolp384r1-sign:   {us: 8097, ops_per_s: 123.5, source: "[R5 §B.5]"}
  ecdsa-brainpoolp384r1-verify: {us: 6142, ops_per_s: 162.8, source: "[R5 §B.5]"}
  ed25519-sign:      {us: 340,  ops_per_s: 2939.1, source: "OpenSSL on Pi 4B [R5 §B.2: arXiv 2503.10238 Table 7]"}
  ed25519-verify:    {us: 754,  ops_per_s: 1327.0, source: "[R5 §B.2]"}
  falcon-512-keygen: {us: 42373, ops_per_s: 23.6,   source: "liboqs on Pi 4B, Ubuntu Server 24.04 [R5 §B.2: arXiv 2503.10238 Table 8]"}
  falcon-512-sign:   {us: 1625, ops_per_s: 615.3,  source: "[R5 §B.2]"}
  falcon-512-verify: {us: 127,  ops_per_s: 7866.3, source: "[R5 §B.2]"}
  ml-dsa-44-keygen:  {us: 378,  ops_per_s: 2642.7, source: "[R5 §B.2]"}
  ml-dsa-44-sign:    {us: 3495, ops_per_s: 286.1,  source: "[R5 §B.2]"}
  ml-dsa-44-verify:  {us: 311,  ops_per_s: 3213.9, source: "[R5 §B.2]"}
  ml-dsa-65-verify:  {us: null, status: not-published, calibration: "ML-DSA-65/87, Falcon-1024, SLH-DSA on Pi 4: not found in fetched sources — UNVERIFIED [R5 §B.2]; run liboqs speed_sig"}
  # --- alternative classes (select via cost_table_overrides) ---
  class_cortex_a53_1_2ghz:                   # Raspberry Pi 3B, BCM2837, OpenSSL [R7 §D8: dchest gist]
    ecdsa-p256-sign:   {us: 613,  ops_per_s: 1631.1}
    ecdsa-p256-verify: {us: 1290, ops_per_s: 775.3}
  class_cortex_a53_1_4ghz:                   # Raspberry Pi 3B+, BCM2837B0 [R7 §D8]
    ecdsa-p256-sign:   {us: 522,  ops_per_s: 1914.7}
    ecdsa-p256-verify: {us: 1101, ops_per_s: 908.4}
  class_cortex_a76_2_4ghz:                   # Raspberry Pi 5, library spread on the same silicon [R5 §B.5: wolfSSL benchmark, June 2026]
    ecdsa-p256-verify-wolfssl-5.9.1: {us: 67.0, ops_per_s: 14933}
    ecdsa-p256-verify-mbedtls-3.6.6: {us: 1689, ops_per_s: 592}
    ecdsa-p256-sign: {us: null, status: not-published, calibration: "Pi 5 sign rate not in the fetched rows (sign given for the i9 only) [R5 §B.5]; run openssl speed on Pi 5"}
radio:                                       # C-V2X modem module that leaves security to the host: Quectel AG15 [R7 §B2]
  rat: [lte-v2x-pc5]                         # 3GPP R14 PC5, no SIM needed [R7 §B2]; for a DSRC variant reuse the §7.1 radio block
  chipset: "Quectel AG15 (1.28 GHz Cortex-A7 application processor inside the module; PCIe to the host) [R7 §B2]"
  bands: [B47, B46D]                         # ext; [R7 §B2]
  bandwidth_hz: 10.0e6                       # ext; sensitivity rows are for 10 MHz [R7 §B2]
  tx_power_dbm:
    min: {value: null, status: not-published, calibration: "not stated"}
    max: 23                                  # "Class 3, 23 dBm ± 2 dB" [R7 §B2]
    tolerance_db: 2
    default: 20                              # J2945/1 baseline [R7 §H3]
    source: "[R7 §B2]"
  sensitivity_dbm:
    primary: -93
    diversity: -93
    simo: -96
    spec_3gpp_simo: -90.4
    per_mcs: null
    source: "B47/B46D, 10 MHz, typ. [R7 §B2: Quectel AG15 hardware design]"
  noise_figure_db: {value: 9, status: evaluation-assumption, source: "NOT PUBLISHED for AG15; 3GPP UE receiver NF 9 dB [R2 Topic A; R2c]"}
  antenna:
    gain_dbi: {value: 3, status: evaluation-assumption, source: "3GPP TR 36.885 Table A.1.1-1 vehicle UE 3 dBi [R2c]"}
    height_m: {value: 1.5, status: evaluation-assumption, source: "TR 36.885 Annex A.1.1 [R2 Topic A; R2c]"}
    pattern: omni
    source: "[R2c]"
  max_data_rate_bps: {tx: 26.0e6, rx: 26.0e6}   # ext; C-V2X TDD [R7 §B2]
gnss: {sensitivity_dbm: -155, note: "AG15 reacquisition/tracking sensitivity; GPS, GLONASS, BeiDou, Galileo, QZSS [R7 §B2]"}   # ext
power_w: {value: null, status: not-published, calibration: "no whole-OBU figure for this proxy; envelopes: Ficosa C-V2X OBU '<7 W peak, <3 W typical, <24 mW standby' [R7 §A8]; Danlaw AutoLink 400 mA @ 12/24 V [R7 §A7]; AG15 modem current 240 mA @ 23 dBm (B47), 230 mA (B46D), GNSS tracking 86 mA [R7 §B2]"}
variants:
  obu/generic-automotive-mcu-aurix-tc3xx-hsm:   # "MCU with HSM" alternative: Infineon AURIX TC3xx HSM figures [R7 §D1: AURIX TC3xx HSM quick training]
    kind: obu
    cpu: {cores: null, clock_hz: null, arch: tricore, status: not-published, calibration: "host TriCore core count/clock depend on the TC3xx part; take from that part's datasheet"}
    ram_bytes: {value: null, status: not-published, calibration: "per part"}
    hsm:
      kind: soc-hsm
      part: "Infineon AURIX TC3xx HSM: 32-bit Arm Cortex-M3 up to 100 MHz, 96 KB HSM RAM (40 KB on some TC29x), 128 KB HSM DFlash key partition; PKC ECC-256, SHA-224/256 and AES-128 hardware accelerators, TRNG [R7 §D1]"
      curves: [nist-p192, nist-p224, nist-p256, sect163k1, sect163r2, sect233k1, sect233r1, brainpool-p160r1, brainpool-p192r1, brainpool-p224r1, brainpool-p256r1, curve25519, ed25519]   # all <= 256-bit [R7 §D1]
      ops:
        ecdsa-p256-sign:   {throughput_per_s: 200, latency_us: 5000,  runs_on: hsm, source: "200 signatures/s @ 100 MHz [R7 §D1]; latency derived 1/200 s"}
        ecdsa-p256-verify: {throughput_per_s: 100, latency_us: 10000, runs_on: hsm, source: "100 verifications/s @ 100 MHz [R7 §D1]; latency derived 1/100 s"}
        sha-256-block:     {latency_us: 2, cycles_per_512bit_block: 65, runs_on: hsm, source: "<2 us per 512-bit block; 65 clock cycles/block (~98 MB/s theoretical peak) [R7 §D1]"}
        trng:              {throughput_bps: 360000, runs_on: hsm, source: "~360 kb/s typical @ 100 MHz [R7 §D1]"}
      queue_depth: {value: null, status: todo-calibrate}
    note: "100 verify/s is two orders of magnitude below the >= 1 kHz aggregate verify rate the NDSS 2024 analysis requires for 100 neighbours [R7 §H2]; this variant is only viable with verify-on-demand (§2.1) or with verification on the host cores. R5 §C listed the AURIX training PDF as not fetched (UNVERIFIED); R7 §D1 read the cached text extract, so these figures are sourced."
  obu/generic-discrete-se-optiga-trust-m:    # slowest documented discrete SE, for sensitivity runs [R7 §D3: Infineon OPTIGA Trust M wiki 'Crypto Performance']
    hsm:
      kind: secure-element
      part: "Infineon OPTIGA Trust M (SLS32AIA), I2C Fast Mode 400 kHz, 25 C, 3.3 V, NIST P-256"
      ops:
        ecdsa-p256-sign:   {latency_us: 60000, latency_range_us: [60000, 70000], runs_on: hsm, source: "~60 ms (M1) to ~70 ms (M3, shielded I2C) [R7 §D3]"}
        ecdsa-p256-verify: {latency_us: 85000, latency_range_us: [85000, 95000], runs_on: hsm, source: "~85 ms (M1/M3) to ~95 ms (M3 shielded) [R7 §D3]; ≈11.8/s"}
        ecdh-p256:         {latency_us: 60000, latency_range_us: [55000, 65000], runs_on: hsm, source: "[R7 §D3]"}
        sha-256:           {throughput_bytes_per_s: 12000, runs_on: hsm, source: "~12 kB/s (M1), ~15 kB/s (M3) [R7 §D3]"}
storage_model: inherit
cost_table_overrides: {}
```

What is missing for `obu/generic-automotive-soc-no-hsm`:
- Everything in `cpu`/`ram`/`flash` is a flagged proxy (Pi 4B, Danlaw AutoLink); no automotive Cortex-A72 part is benchmarked in R7/R5.
- OpenSSL version/build flags for the Pi gists are not stated — "indicative, not lab-certified" [R7 §D8].
- ML-DSA-65/87, Falcon-1024 and SLH-DSA on Cortex-A72 — `UNVERIFIED` / not found [R5 §B.2].
- Pi 5 ECDSA sign rate; AG15 minimum TX power; whole-OBU power — `not-published`.
- AURIX variant: host core parameters — `not-published`; Microchip ATECC608 timings (the other common discrete SE) are `NOT PUBLISHED` in every public datasheet variant and are deliberately not used [R7 §D6]; ESCRYPT CycurHSM `NOT PUBLISHED / NOT FOUND` [R7 §D9].

### 7.5 `obu/pq-capable-hypothetical` — PQ-capable OBU anchored in measurements

No vendor-announced automotive-grade PQ accelerator exists in any NXP, Infineon or Autotalks collateral gathered; the only published PQ hardware throughput is a backend appliance (Thales Luna, §7.7) [R7 §E5]. A "PQ-capable OBU" is therefore modelled as a Cortex-A76-class application processor running PQ signatures **in software**, with `hsm.kind: none` for the PQ primitives. The optimistic anchor is liboqs on a Raspberry Pi 5 (Cortex-A76 @ 2.4 GHz): ML-DSA-44 verify 8,139.0/s, Falcon-512 verify 19,831.3/s [R5 §B.2: arXiv 2503.10238 Table 8]; wolfSSL ECDSA P-256 verify on the same board 14,933/s [R5 §B.5]. The pessimistic bound is the NDSS 2024 measurement on production Cohda MK6 hardware without ARMv8 optimizations: Falcon verify 2,243/s, Dilithium verify 5,299/s, SPHINCS+ 184/s [R7 §E3]. The Cortex-M7 and Cortex-M4 figures are kept as the MCU floor [R7 §E1, §E2; R5 §B.3]. The system-level requirement the profile is judged against is >= 1 kHz aggregate verify throughput for 100 neighbours at 10 Hz [R7 §H2: NDSS 2024 §VII-B]; frame limits (DSRC MPDU 2,304 B; C-V2X 437 B per TB in 10 MHz, or 2,481 B at MCS 11 with 10 subchannels) bind before verify time for every lattice scheme [R5 §B.6; R7 §E3 v_max table].

```yaml
id: obu/pq-capable-hypothetical
kind: obu
version: 2026-09-18.1
hypothetical: true                           # no such commercial unit; see R7 §E5
sources:
  - {kind: paper, ref: "Berger, Lemoudden, Buchanan, arXiv 2503.10238 Table 8 — liboqs on Raspberry Pi 5 (Cortex-A76) [R5 §B.2; §7.10 #19]", accessed: 2026-09-17}
  - {kind: benchmark, ref: "wolfSSL vs mbedTLS benchmark, June 2026, Pi 5 row [R5 §B.5; §7.10 #18]", accessed: 2026-09-17}
  - {kind: paper, ref: "Twardokus et al., NDSS 2024 Table V, Cohda MK6 measurements [R7 §E3; R5 §B.6; §7.10 #22]", accessed: 2026-09-18}
  - {kind: paper, ref: "Howe & Westerbaan, NIST 4th PQC conf. 2022, Cortex-M7 @ 216 MHz [R7 §E1; R5 §B.3; §7.10 #24]", accessed: 2026-09-18}
  - {kind: benchmark, ref: "pqm4 benchmarks.csv, Cortex-M4 cycle counts [R7 §E2; R5 §B.3; §7.10 #25]", accessed: 2026-09-17}
  - {kind: gap, ref: "Announced PQ hardware accelerators (NXP/Infineon): NOT PUBLISHED / none found [R7 §E5]", accessed: 2026-09-18}
cpu:
  cores: {value: null, status: not-published, calibration: "Pi 5 core count is not restated in R5/R7 and all rows below are single-core; set per scenario (a 4-core default is a scenario choice, not a sourced figure)"}
  clock_hz: 2.4e9                            # "Raspberry Pi 5 (Cortex-A76 @ 2.4 GHz)" [R5 §B.5: wolfSSL benchmark]
  arch: armv8-a                              # Cortex-A76 class
  dmips: {value: null, status: not-published, calibration: "no DMIPS rating in R5/R7; scale by measured openssl/liboqs ratios"}
  source: "[R5 §B.2, §B.5] — developer-board proxy, flagged; no automotive Cortex-A76 part is benchmarked in R7/R5"
ram_bytes: {value: null, status: not-published, calibration: "hypothetical; scenario parameter. Note PQ store growth: ML-DSA-44 signature 2,420 B, Falcon-512 666 B vs ECDSA 64 B [R5 quick modelling defaults, §A]"}
flash_bytes: {value: null, status: not-published, calibration: "hypothetical; scenario parameter"}
hsm:
  kind: none                                 # NO vendor PQ accelerator found [R7 §E5]; classical ECDSA may optionally be offloaded by composing with a §7.2 or §7.3 hsm block in the manifest
  part: null
  ops: {}
  queue_depth: null
software_crypto:                             # us = 1/ops_per_s (derived) unless a time is published
  # --- optimistic anchor: Cortex-A76 @ 2.4 GHz (Raspberry Pi 5), liboqs / wolfSSL ---
  ecdsa-p256-verify: {us: 67.0, ops_per_s: 14933, source: "wolfSSL 5.9.1 on Pi 5 [R5 §B.5]; mbedTLS 3.6.6 on the same board 592/s (1,689 us) shows the library spread"}
  ecdsa-p256-sign:   {us: null, status: not-published, calibration: "Pi 5 sign rate not in fetched rows [R5 §B.5]; PROXY = Pi 4 OpenSSL 244 us [R7 §D8], flag"}
  ml-dsa-44-keygen:  {us: 111,  ops_per_s: 8986.3,  source: "liboqs on Pi 5 [R5 §B.2: arXiv 2503.10238 Table 8]"}
  ml-dsa-44-sign:    {us: 531,  ops_per_s: 1885.0,  source: "[R5 §B.2]"}
  ml-dsa-44-verify:  {us: 123,  ops_per_s: 8139.0,  source: "[R5 §B.2]"}
  falcon-512-keygen: {us: 10482, ops_per_s: 95.4,   source: "[R5 §B.2]"}
  falcon-512-sign:   {us: 298,  ops_per_s: 3360.6,  source: "[R5 §B.2]"}
  falcon-512-verify: {us: 50.4, ops_per_s: 19831.3, source: "[R5 §B.2]"}
  ml-dsa-65-verify:  {us: null, status: not-published, calibration: "ML-DSA-65/87, Falcon-1024, SLH-DSA on Pi 4/5: not found — UNVERIFIED [R5 §B.2]; run liboqs speed_sig on the board; until then scale ML-DSA-44 by the pqm4 m4f verify cycle ratio 2,415,944 / 1,421,623 [R5 §B.3] and flag as a derived estimate"}
  ml-dsa-87-verify:  {us: null, status: not-published, calibration: "as above; pqm4 m4f ratio 4,193,104 / 1,421,623 [R5 §B.3]"}
  slh-dsa-128s-verify: {us: null, status: not-published, calibration: "not measured on Cortex-A76; NDSS 2024 SPHINCS+ verify 5.436 ms on Cohda MK6 [R7 §E3] is the only V2X-hardware figure"}
  # --- pessimistic bound: production V2X hardware, Cohda MK6 'Qualcomm ARMv8', no ARMv8 optimizations [R7 §E3: NDSS 2024 Table V] ---
  bound_cohda_mk6_measured:
    ecdsa-sign:        {us: 7820,    ops_per_s: 128}
    ecdsa-verify:      {us: null, published_value_ms: 0.001, status: anomalous-as-published, note: "see §7.3"}
    falcon-sign:       {us: 2152,    ops_per_s: 465}
    falcon-verify:     {us: 446,     ops_per_s: 2243}
    dilithium-sign:    {us: 2634,    ops_per_s: 380}
    dilithium-verify:  {us: 189,     ops_per_s: 5299}
    sphincs-plus-sign: {us: 5485,    ops_per_s: 182}
    sphincs-plus-verify: {us: 5436,  ops_per_s: 184}
    xmss-sign:         {us: 1405408, ops_per_s: 0.7}
    xmss-verify:       {us: 2780,    ops_per_s: 359}
  # --- MCU floor: Cortex-M7 @ 216 MHz (STM32F767ZI), Howe & Westerbaan 2022 [R7 §E1]; Cortex-M4 cycles, pqm4 [R7 §E2; R5 §B.3] ---
  bound_cortex_m7_216mhz:
    dilithium-2-keygen: {us: 6700,   kcycles: 1437}
    dilithium-2-sign:   {us: 16900,  kcycles: 3658}
    dilithium-2-verify: {us: 6600,   kcycles: 1429}
    dilithium-3-sign:   {us: 20700,  kcycles: 6009}
    dilithium-3-verify: {us: 11400,  kcycles: 2453}
    dilithium-5-sign:   {us: 37800,  kcycles: 8157}
    dilithium-5-verify: {us: 19800,  kcycles: 4287}
    falcon-512-keygen:  {us: 358700, kcycles: 77475}
    falcon-512-sign:    {us: 22100,  kcycles: 4778, note: "dynamic tree, native FPU"}
    falcon-512-verify:  {us: 2600,   kcycles: 559}
    falcon-1024-sign:   {us: 47400,  kcycles: 10243}
    falcon-1024-verify: {us: 5300,   kcycles: 1136}
  bound_cortex_m4_cycles:                    # cycles only; pqm4 reference-board clock not restated in the CSV [R7 §E2]; R5 §B.3 quotes 24 MHz for NUCLEO-L4R5ZI
    ml-dsa-44-verify:   {cycles: 1421623, impl: m4f, source: "[R7 §E2; R5 §B.3: pqm4 benchmarks.csv]"}
    ml-dsa-44-sign:     {cycles: 3943121, impl: m4f, range: [1812557, 17009165], source: "[R5 §B.3]"}
    ml-dsa-65-verify:   {cycles: 2415944, impl: m4f, source: "[R7 §E2]"}
    ml-dsa-87-verify:   {cycles: 4193104, impl: m4f, source: "[R7 §E2]"}
    falcon-512-verify:  {cycles: 396949, impl: "fndsa_provisional-512 m4f", source: "[R7 §E2: local pqm4 CSV]; NOTE R5 §B.3 reports Falcon rows absent from the current pqm4 master CSV — sheet discrepancy, the R7 local CSV vintage differs"}
    falcon-512-verify-pornin: {cycles: 504051, us: 3000, source: "Pornin ePrint 2019/893 §5.3, STM32F407 Cortex-M4 @ 168 MHz [R5 §B.3]"}
    sphincs-sha2-128f-simple-verify: {cycles: 21923628, impl: clean, source: "[R7 §E2]"}
    ecdsa-p256-verify:  {cycles: 976000, us: 15300, source: "Emill/P256-Cortex-M4 on nRF52840 @ 64 MHz [R5 §B.5]"}
capacity_requirements:                       # ext; system-level checks the profile is judged against
  min_aggregate_verify_hz: 1000              # ">= 100 signatures per 100 ms interval (>= 1 kHz)" for 100 neighbours [R7 §H2: NDSS 2024 §VII-B]
  critical_message_latency_budget_ms: 20     # "as low as 20 ms" [R7 §H4: ePrint 2022/133 citing NHTSA VSC-A]
  v_max_100_vehicle_urban:                   # NDSS 2024 (Erlangen model): frame-duration-constrained / verify-time-constrained [R7 §E3]
    pure-ecdsa:                 {frame: 165, verify: 67521, binding: frame-duration}
    partially-hybrid-falcon:    {frame: 101, verify: 224,   binding: frame-duration}
    partially-hybrid-dilithium: {frame: 53,  verify: 529,   binding: frame-duration}
    partially-hybrid-sphincs:   {frame: 21,  verify: 18,    binding: verify-time}
    partially-hybrid-xmss:      {frame: 49,  verify: 35,    binding: verify-time}
  frame_limits_bytes: {dsrc_mpdu: 2304, cv2x_tb_10mhz_ndss: 437, cv2x_tb_mcs11_10subch_j3161: 2481}   # [R5 §B.6]
radio: {inherit: obu/cohda-mk6c-qualcomm-9150}   # hypothetical unit; reuse the documented C-V2X radio block (§7.3) or the §7.2 DSRC block per scenario
power_w: {value: null, status: not-published, calibration: "hypothetical; measure on the chosen board under sustained liboqs load"}
storage_model: inherit                       # PQ certificate/signature sizes from 04-models §9 / R5 §A
cost_table_overrides: {}
```

What is missing for `obu/pq-capable-hypothetical`:
- Any vendor PQ accelerator — `NOT PUBLISHED` / none found [R7 §E5]; the profile is software-only by construction.
- ML-DSA-65/-87, Falcon-1024, SLH-DSA on Cortex-A76 — `UNVERIFIED` / not found [R5 §B.2]; only pqm4 cycle ratios are available for scaling.
- Pi 5 ECDSA sign; core count; DMIPS; RAM/flash; power — `not-published`.
- The NDSS labels "Falcon"/"Dilithium" are not tied to parameter sets in the R7/R5 extracts.
- The pqm4 Falcon rows differ between the R7 local CSV and the R5 master fetch — sheet discrepancy noted inline.
- Cortex-A53 liboqs numbers — `UNVERIFIED` / not found [R5 §B.2].

### 7.6 RSU profiles

Two RSU profiles: the Commsignia ITS-RS4, whose brief is the most complete public RSU datasheet in the sheet (quad-core i.MX 6, 2 GB, SLI97 HSM with a stated verify figure) [R7 §C2], and the Cohda MK5 RSU, which shares the §7.1 radio but whose brief publishes radio-level figures only [R7 §C1]. The USDOT pilot facts (R7 §C7) give RSU counts and backhaul technology per site but **no mounting heights**, so every RSU antenna height below is `not-published` with the 3GPP UE-type-RSU evaluation assumption (5 m) as the flagged fallback [R2 Topic A: TR 36.885 Annex A.1.1]. The Commsignia brief's "<50 usec signing delay" is carried exactly as extracted and tagged `UNVERIFIED`, with the corroborated cross-vendor SLI97 figure (<50 ms) alongside, as R7 §C2 does.

```yaml
id: rsu/commsignia-its-rs4
kind: rsu
version: 2026-09-18.1
sources:
  - {kind: product-brief, ref: "Commsignia ITS-RS4 Product Brief v0.9.3 (2020-04-22) [R7 §C2; R5 §C; §7.10 #10]", accessed: 2026-09-18}
  - {kind: press-release, ref: "Autotalks/Infineon SLI 97 announcement (signing-latency performance profile) [R7 §D2]", accessed: 2026-09-18}
  - {kind: deployment-facts, ref: "USDOT CV Pilot deployment facts [R7 §C7]", accessed: 2026-09-18}
cpu:
  cores: 4                                   # "NXP i.MX 6, quad-core, 792 MHz" [R7 §C2: Commsignia ITS-RS4 brief]
  clock_hz: 7.92e8                           # [R7 §C2]
  arch: {value: null, status: not-published, calibration: "brief names i.MX 6 only; confirm core type from the NXP i.MX 6 Quad datasheet"}
  dmips: {value: null, status: not-published, calibration: "not stated; derive from the NXP datasheet or measure"}
  trustzone: true                            # ext; "ARM TrustZone (TZ architecture)" [R7 §C2]
  source: "[R7 §C2: pdftxt/commsignia_rs4_brief.txt]"
ram_bytes: {value: 2147483648, source: "2 GB DDR3 SDRAM [R7 §C2]"}
flash_bytes: {value: 4294967296, source: "4 GB eMMC; plus dual micro-SD [R7 §C2]"}
os: "Linux / RTOS (V2X) [R7 §C2]"                        # ext
hsm:
  kind: secure-element
  part: "Infineon SLI97 (32-bit security controller; CC EAL5+ platform) [R7 §C2, §D2]; secure flash up to 1 MB 'SOLID FLASH', EAL6+ [R7 §C2]"
  ops:
    ecdsa-p256-verify:
      throughput_per_s: {value: 2000, status: lower-bound, source: "brief: '>2000 verifications' — per-second implied by context, not stated numerically per second in that exact phrase [R7 §C2]; NIST and Brainpool. R7 §D2: line-rate verification on SLI97-based designs is done by the companion V2X communication processor, not the SLI97 itself, so runs_on is the radio-variant's engine"}
      latency_us: {value: null, status: not-published, calibration: "bench on device"}
      runs_on: accelerator                   # radio-variant dependent [R7 §C2, §D2]
      source: "[R7 §C2, §D2]"
    ecdsa-p256-sign:
      throughput_per_s: {value: null, status: not-published, calibration: "not stated; bench"}
      latency_us: {value: 50000, status: unverified-unit, source: "RS4 brief text as extracted: '<50 usec signing delay' — UNVERIFIED / likely OCR error [R7 §C2]; the corroborated cross-vendor figure for the SLI97/SLE97 family is '<50 ms' ECDSA-256 signing latency (Autotalks SLI 97 press release; Unex OBU-201U FCC filing) [R7 §C2 discrepancy note, §D2]. Modelled as 50,000 us (upper bound) until measured; flag in manifest"}
      runs_on: hsm
      source: "[R7 §C2, §D2]"
  queue_depth: {value: null, status: todo-calibrate}
software_crypto:
  ecdsa-p256-verify: {us: null, status: not-published, calibration: "no benchmark on i.MX 6 quad in R7/R5; PROXY = Pi 3B Cortex-A53 @ 1.2 GHz 1,290 us [R7 §D8], flag"}
  ecdsa-p256-sign:   {us: null, status: not-published, calibration: "PROXY = Pi 3B 613 us [R7 §D8], flag"}
radio:
  rat: hybrid                                # dual-mode; interchangeable radio variants on one board [R7 §C2, §B4]
  chipset: "one of: Autotalks Secton | NXP TEF5100 (RF) + SAF5100 (BB) | Marvell 88W8987PA (SDIO) | Qualcomm 9150 [R7 §C2]"
  tx_power_dbm: {min: null, max: null, default: null, status: not-published, calibration: "radio-variant dependent [R7 §C2]; with the NXP SAF5100 variant use the §7.1 radio block, with Qualcomm 9150 the §7.3 block, with Autotalks the §7.2 block, and flag which in the manifest"}
  sensitivity_dbm: {per_mcs: null, status: not-published, calibration: "as above"}
  noise_figure_db: {value: null, status: not-published, calibration: "as above; NXP variant chip-level 6 dB [R7 §D5]"}
  antenna:
    gain_dbi: {value: null, status: not-published, calibration: "not stated; site-specific antenna; 3GPP UE-type RSU assumption 3 dBi [R2c] as fallback"}
    height_m: {value: 5, status: evaluation-assumption, source: "USDOT pilot sources reviewed give RSU counts and backhaul, not mounting heights [R7 §C7]; fallback = 3GPP TR 36.885 Annex A.1.1 UE-type RSU antenna height 5 m [R2 Topic A]; set per site from the survey or FHWA RSU spec v4.1 mounting guidance"}
    pattern: {value: null, status: not-published, calibration: "site-specific"}
    source: "[R7 §C7; R2 Topic A]"
backhaul:                                    # ext; consumed by the Backhaul model (§3)
  interface: "10/100/1000 Mbps Ethernet, PoE [R7 §C2]"
  deployment_options: "fiber where available, cellular interim (Tampa THEA); AT&T FirstNet cellular + NYCWiN (NYC); Wyoming NOT PUBLISHED [R7 §C7]"
imu: "Bosch 3-axis gyro, BMI160 accel, BMM150 mag [R7 §C2]"   # ext
power_w: {value: null, status: not-published, calibration: "8–32 V DC / PoE [R7 §C2]; draw not stated; Kapsch RIS-9260 '<25 W PoE 802.3at' [R7 §C3] and Yunex RSU '12 W max' [R7 §C5] bracket the RSU class"}
environment: {enclosure: "NEMA4X / IP67"}    # ext; [R7 §C2]
storage_model: inherit
cost_table_overrides: {}
```

```yaml
id: rsu/cohda-mk5-rsu
kind: rsu
version: 2026-09-18.1
sources:
  - {kind: product-brief, ref: "Cohda MK5 RSU product brief [R7 §C1: pdftxt/cohda_mk5_rsu_brief.txt; no URL recorded]", accessed: 2026-09-18}
  - {kind: product-brief, ref: "Cohda MK5 OBU brief (compute/HSM proxy) [R7 §A1; §7.10 #1]", accessed: 2026-09-18}
cpu: {cores: null, clock_hz: null, arch: null, dmips: null, status: not-published, calibration: "NOT PUBLISHED in the RSU brief (module-level radio specs only); presumably shares the MK5 OBU's i.MX6 DL, but the brief does not restate it [R7 §C1] — use the obu/cohda-mk5 cpu block as proxy and flag in manifest"}
ram_bytes: {value: null, status: not-published, calibration: "as above; OBU proxy 1 GB [R7 §A1]"}
flash_bytes: {value: null, status: not-published, calibration: "as above; OBU proxy 4 GB eMMC [R7 §A1]"}
os: "Linux 4.1.15 [R7 §C1]"                              # ext
hsm:
  kind: secure-element
  part: {value: null, status: not-published, calibration: "NOT PUBLISHED in the RSU brief; presumably SXF17xx like the MK5 OBU [R7 §C1]; use the obu/cohda-mk5 hsm block (SAF5400 2,000/s verify proxy, sign NOT PUBLISHED) and flag"}
  ops: {inherit: obu/cohda-mk5}
  queue_depth: {value: null, status: todo-calibrate}
software_crypto: {inherit: obu/cohda-mk5}
radio:
  rat: [dsrc-80211p]
  chipset: "NXP SAF5100-based RoadLINK, same as MK5 OBU [R7 §C1]"
  bandwidth_hz: 10.0e6                       # ext; [R7 §C1]
  data_rates_mbps: [3, 27]                   # ext; "3–27 Mbps" [R7 §C1]
  diversity: "CDD TX diversity, MRC RX diversity [R7 §C1]"   # ext
  tx_power_dbm:
    min: -10                                 # module datasheet [R7 §A1]
    max: 22                                  # "+22 dBm (ETSI Mask C)" [R7 §C1]
    default: 20                              # J2945/1 baseline [R7 §H3]
    source: "[R7 §C1; §A1]"
  sensitivity_dbm: {system: -99, per_mcs: null, source: "-99 dBm @ 3 Mbps [R7 §C1]; per-MCS not published, see §7.1 proxy"}
  noise_figure_db: {value: 6, source: "NXP RoadLINK chip-level 6 dB @ 5.9 GHz [R7 §D5, §G]"}
  antenna:
    gain_dbi: {value: null, status: not-published, calibration: "not stated; site antenna"}
    height_m: {value: 5, status: evaluation-assumption, source: "no mounting height in the USDOT pilot facts [R7 §C7]; 3GPP TR 36.885 UE-type RSU 5 m [R2 Topic A]; set per site"}
    pattern: {value: null, status: not-published, calibration: "site-specific"}
    source: "[R7 §C7; R2 Topic A]"
gnss: {accuracy_m: 2.5, source: "[R7 §C1]"}  # ext
backhaul: {interface: "PoE (regular) [R7 §C1]", deployment_options: "per site, see USDOT pilot table §7.9 [R7 §C7]"}   # ext
power_w: {value: null, status: not-published, calibration: "PoE; draw not stated [R7 §C1]"}
environment: {operating_temp_c: [-40, 85], enclosure: "NEMA 4, 240 × 165 × 67 mm"}   # ext; [R7 §C1]
storage_model: inherit
cost_table_overrides: {}
```

What is missing for the RSU profiles:
- Commsignia RS4: CPU ISA/DMIPS; SLI97 sign throughput and the *unit* of the signing latency (`UNVERIFIED`, carried as 50 ms upper bound); which verification engine the shipped radio variant uses; radio TX/RX figures (variant-dependent); antenna gain/height/pattern; power draw. The Infineon SLI97 full datasheet is `NOT PUBLISHED` [R7 §D2].
- Cohda MK5 RSU: CPU, RAM, flash and HSM part — `NOT PUBLISHED` in the RSU brief [R7 §C1], all inherited from `obu/cohda-mk5` as flagged proxies; antenna gain/height; power.
- RSU mounting heights from the USDOT pilots — not in the sources reviewed [R7 §C7]; 3GPP 5 m assumption used.
- Other RSUs in the sheet are not profiled but appear in §7.9 for comparison: Kapsch RIS-9260/9360 (x86 dual-core 1.33 GHz, 1 GB ECC, HSM verify rate `NOT PUBLISHED`) [R7 §C3, §C4]; Siemens/Yunex (dual-core 800 MHz, 1 GB, unnamed HSM, 12 W max, ~2,500 m open-field LOS range) [R7 §C5]; Danlaw RouteLink `NOT PUBLISHED` [R7 §C6].

### 7.7 Backend profiles

Two backend profiles: a cloud VM class with software crypto only (the host-CPU bound), and a Thales Luna 7 HSM appliance (the bound production RA/PCA/MA nodes normally hit, since HSM throughput, not host CPU, limits them [R7 §F1]). The historical CAMP planning figure — ~1,500 ECDSA-256 sign/s and ~300 verify/s on a 2 GHz processor — is carried as a separate `class_camp_2016_baseline`, exactly as published, with R7's note that its verify-slower-than-sign asymmetry reflects an unoptimized ~2010s implementation [R7 §F4; R5 §B.5: CAMP EE Requirements 2016, p. 75]. Appliance-level RA/PCA/MA transaction rates are `NOT PUBLISHED` anywhere in the sheet [R7 §F4]; the §4 service model composes them from per-op costs.

```yaml
id: backend/cloud-vm-x86
kind: backend-server
version: 2026-09-18.1
sources:
  - {kind: book, ref: "Ristić, OpenSSL Cookbook, 'Performance' chapter [R7 §F1; §7.10 #26]", accessed: 2026-09-18}
  - {kind: benchmark, ref: "wolfSSL vs mbedTLS benchmark, June 2026, Intel i9-11950H rows [R5 §B.5; §7.10 #18]", accessed: 2026-09-17}
  - {kind: spec, ref: "Dilithium round-3 specification Table 1 (Skylake) [R5 §B.1; §7.10 #29]", accessed: 2026-09-17}
  - {kind: paper, ref: "Pornin, ePrint 2019/893 §5.2 (Falcon on Skylake) [R5 §B.1; §7.10 #30]; falcon-sign.info [R5 §B.1; §7.10 #31]", accessed: 2026-09-17}
  - {kind: paper, ref: "arXiv 2306.01989 (Dilithium AVX2, i7-11700F); ePrint 2026/1539 (Falcon verify AVX-512, Zen5) [R7 §F5; §7.10 #32, #33]", accessed: 2026-09-18}
  - {kind: spec, ref: "CAMP SCMS PoC EE Requirements, Release 1.1, 2016-05-04, Crypto Basics p. 75 [R7 §F4; R5 §B.5; §7.10 #35]", accessed: 2026-09-17}
cpu:
  cores: {value: null, status: todo-calibrate, calibration: "scenario parameter (M/M/c 'c' in §4); no source picks a VM size"}
  clock_hz: {value: null, status: not-published, calibration: "OpenSSL Cookbook excerpt does not state the CPU [R7 §F1]; record the VM's CPU model in the manifest and re-run openssl speed"}
  arch: x86-64
  dmips: {value: null, status: not-published, calibration: "not used for backend nodes; scale by openssl ratio"}
  source: "[R7 §F1]"
ram_bytes: {value: null, status: todo-calibrate, calibration: "scenario parameter; storage counters (§4) drive the requirement"}
flash_bytes: {value: null, status: todo-calibrate, calibration: "scenario parameter"}
hsm:
  kind: none                                 # host-only variant; compose with backend/hsm-appliance-luna7 for the HSM-bound service model
  part: null
  ops: {}
  queue_depth: null
software_crypto:                             # per core; us = 1/ops_per_s (derived) unless a time is published
  # --- default class: modern x86, OpenSSL `speed` ecdsap256 (CPU not stated) [R7 §F1: OpenSSL Cookbook] ---
  ecdsa-p256-sign:   {us: 48.8, ops_per_s: 20508.1, source: "[R7 §F1]"}
  ecdsa-p256-verify: {us: 152,  ops_per_s: 6566.2,  source: "[R7 §F1]"}
  # --- optimized-library class: Intel i9-11950H [R5 §B.5: wolfSSL benchmark June 2026] ---
  class_i9_11950h:
    ecdsa-p256-verify-wolfssl-5.9.1: {us: 16.3, ops_per_s: 61357}
    ecdsa-p256-sign-wolfssl-5.9.1:   {us: 15.6, ops_per_s: 64194}
    ecdsa-p256-verify-mbedtls-3.6.6: {us: 804,  ops_per_s: 1244}
    ecdsa-p256-sign-mbedtls-3.6.6:   {us: 237,  ops_per_s: 4227}
  class_community_i7_unverified:             # WebSearch snippet, provenance not re-fetched — UNVERIFIED [R7 §F1]
    ecdsa-p256-sign:   {us: 22.2, ops_per_s: 45069.6, status: unverified}
    ecdsa-p256-verify: {us: 70.6, ops_per_s: 14166.6, status: unverified}
  # --- PQ, x86 ---
  falcon-512-keygen: {us: 8640,  source: "Intel Core i5-8259U @ 2.3 GHz, TurboBoost off [R5 §B.1: falcon-sign.info]"}
  falcon-512-sign:   {us: 168,   ops_per_s: 5948.1,  source: "[R5 §B.1: falcon-sign.info]"}
  falcon-512-verify: {us: 35.8,  ops_per_s: 27933.0, source: "[R5 §B.1: falcon-sign.info]"}
  falcon-1024-keygen: {us: 27450, source: "[R5 §B.1: falcon-sign.info]"}
  falcon-1024-sign:  {us: 343,   ops_per_s: 2913.0,  source: "[R5 §B.1]"}
  falcon-1024-verify: {us: 73.3, ops_per_s: 13650.0, source: "[R5 §B.1]"}
  falcon-512-verify-skylake-avx2: {us: 22.51, cycles: 81036, source: "i7-6567U Skylake 3.6 GHz, avx2 [R5 §B.1: Pornin ePrint 2019/893 §5.2]; sign(dynamic) 948,132 cyc = 263.37 us; fpemu (no FPU) verify 97,416 cyc"}
  falcon-1024-verify-skylake-avx2: {us: 44.61, cycles: 160596, source: "[R5 §B.1: Pornin]"}
  falcon-512-verify-zen5-avx512: {us: 3.6, source: "AMD Zen5, AVX-512 [R7 §F5: ePrint 2026/1539]"}
  dilithium2-verify-skylake-avx2: {cycles: 118412, source: "Skylake AVX2, round-3 [R5 §B.1: Dilithium spec Table 1]; sign 259,172 cyc median / 333,013 avg; ref C verify 327,362 cyc. Round-3 sizes differ slightly from FIPS 204 ML-DSA; cycle counts indicative only"}
  dilithium3-verify-skylake-avx2: {cycles: 179424, source: "[R5 §B.1]; sign 428,587 median"}
  dilithium5-verify-skylake-avx2: {cycles: 279936, source: "[R5 §B.1]; sign 538,986 median"}
  dilithium2-verify-i7-11700f-avx2: {cycles: 107338, source: "[R7 §F5: arXiv 2306.01989]; keygen 106,000 / sign 251,050 cyc; clock not restated — R7's ~43 us at ~2.5 GHz is an order-of-magnitude sanity check, not a cited fact"}
  dilithium3-verify-i7-11700f-avx2: {cycles: 174218, source: "[R7 §F5]; keygen 246,988 / sign 406,248"}
  sphincs-sha2-128s-simple-verify-avx2: {cycles: 861478, source: "Xeon E3-1220 Haswell 3.1 GHz [R5 §B.1: SPHINCS+ r3.1 spec Table 6]; sign 644,740,090 cyc"}
  sphincs-sha2-128f-simple-verify-avx2: {cycles: 2150290, source: "[R5 §B.1: Table 6]; sign 33,651,546 cyc"}
  liboqs-reference-unoptimised:              # host CPU not stated — UNVERIFIED platform [R5 §B.1: arXiv 2503.10238 Table 6]
    falcon-512:   {sign_per_s: 176.55,  verify_per_s: 17246,   status: unverified-platform}
    falcon-1024:  {sign_per_s: 80.56,   verify_per_s: 8341,    status: unverified-platform}
    dilithium2:   {sign_per_s: 2099.33, verify_per_s: 8752.33, status: unverified-platform}
    dilithium3:   {sign_per_s: 1320,    verify_per_s: 5519,    status: unverified-platform}
    sphincs-sha2-128f-s: {sign_per_s: 23.86, verify_per_s: 404.73, status: unverified-platform}
  # --- historical class: CAMP SCMS PoC planning figures, 2 GHz processor, era ~2010s implementation [R7 §F4; R5 §B.5] ---
  class_camp_2016_baseline:
    ecdsa-p256-sign:   {us: 667,  ops_per_s: 1500, source: "'about 1500 signatures per second on a 2 GHz processor' [R5 §B.5: CAMP EE Req. p. 75]"}
    ecdsa-p256-verify: {us: 3333, ops_per_s: 300,  source: "'can verify only about 300 signatures per second' [R5 §B.5]; verify-slower-than-sign asymmetry is era-specific, reported as published [R7 §F4]"}
    aes-encrypt-bytes-per-s: {value: 81.0e6, source: "81 MB/s on a 2 GHz processor [R7 §F4]"}
    mac-100b-ops-per-s: {value: 1000000, source: "1,000,000 100-byte MAC operations/s [R7 §F4]"}
radio: {rat: none}                           # backend node; connectivity via BackendNet links (§4)
power_w: {value: null, status: not-published, calibration: "cloud VM; not modelled"}
storage_model: inherit                       # storage counters per 05-protocols §3.1
service_model_note: "RA/PCA/MA appliance-level transaction rates NOT PUBLISHED [R7 §F4]; §4 composes them from per-op costs plus a fixed overhead (TODO: calibrate)"
cost_table_overrides: {}
```

```yaml
id: backend/hsm-appliance-luna7
kind: hsm-appliance
version: 2026-09-18.1
sources:
  - {kind: product-brief, ref: "Thales Luna Network HSM 7 product brief (Dec 2019) [R7 §F2; R5 §B.5; §7.10 #36]", accessed: 2026-09-18}
  - {kind: product-brief, ref: "Thales Luna PCIe HSM product brief [R7 §F2: pdftxt/thales_luna_pcie.txt; no URL recorded]", accessed: 2026-09-18}
  - {kind: datasheet, ref: "Thales TCT Luna T-series (government) family sheet [R7 §F2: pdftxt/thales_hsm_family_dlt.txt; no URL recorded]", accessed: 2026-09-18}
  - {kind: performance-brief, ref: "Thales Luna HSM for 5G performance brief [R7 §F2: pdftxt/thales_5g_perf.txt; no URL recorded]", accessed: 2026-09-18}
cpu: {cores: null, clock_hz: null, arch: null, dmips: null, status: not-published, calibration: "appliance internals not published; the profile is characterised by ops throughput only"}
ram_bytes: {value: null, status: not-published, calibration: "R7 summary table lists 'up to 64 MB' for the A790 without a row-level source; not carried as a value"}
flash_bytes: {value: null, status: not-published, calibration: "not published"}
hsm:
  kind: accelerator                          # network-attached appliance; the "HSM server" of §4 for RA/PCA/MA/CRLG signing
  part: "Thales Luna Network HSM 7 (A-series standard / S-series multi-factor auth); Luna PCIe HSM has identical per-tier figures [R7 §F2]"
  tier: a750                                 # scenario choice among the three published tiers; no source designates a default
  tiers:                                     # transactions per second, per appliance [R7 §F2: Thales Luna Network HSM 7 brief; R5 §B.5]
    a700_s700_standard:   {ecc_p256_tps: 2000,  rsa_2048_tps: 1000,  aes_gcm_tps: 2000}
    a750_s750_enterprise: {ecc_p256_tps: 10000, rsa_2048_tps: 5000,  aes_gcm_tps: 10000}
    a790_s790_maximum:    {ecc_p256_tps: 22000, rsa_2048_tps: 10000, aes_gcm_tps: 17000}
    headline: "over 20,000 ECC and 10,000 RSA operations/second for high-performance use cases [R7 §F2]"
  ops:                                       # for the selected tier (a750); the brief does not split ECC P-256 tps into sign vs verify — R5 §B.5 labels it 'ECC P-256 sign'; applied to both sign and verify as an appliance rate, flagged
    ecdsa-p256-sign:   {throughput_per_s: 10000, latency_us: {value: null, status: not-published, calibration: "per-op latency not published; service time = 1/tps = 100 us (derived) for a750; 500 us a700; 45.5 us a790; network round trip from BackendNet"}, runs_on: hsm, source: "[R7 §F2; R5 §B.5]"}
    ecdsa-p256-verify: {throughput_per_s: 10000, latency_us: null, runs_on: hsm, status: applied-from-combined-ecc-tps, source: "[R7 §F2]"}
    rsa-2048-sign:     {throughput_per_s: 5000,  latency_us: null, runs_on: hsm, source: "[R7 §F2]"}
    aes-gcm:           {throughput_per_s: 10000, latency_us: null, runs_on: hsm, source: "[R7 §F2]"}
    ml-dsa-87-sign:    {throughput_per_s: {value: null, status: not-published, calibration: "no ML-DSA tps for the A-series; the only published PQ figure in the family is 20 tps on the 'Luna Tablet & Backup' T-series model [R7 §F2] — see tiers_tseries; request Thales PQC performance data"}, latency_us: null, runs_on: hsm, source: "[R7 §F2]"}
    ml-kem-encaps:     {throughput_per_s: {value: null, status: not-published, calibration: "ML-KEM and LMS/HSS supported on the Luna line, no numeric throughput published [R7 §F2]"}, latency_us: null, runs_on: hsm, source: "[R7 §F2]"}
  tiers_tseries:                             # Thales TCT 'Government' T-series, tps [R7 §F2: thales_hsm_family_dlt]
    t2000_standard:   {rsa_2048_tps: 1400,  rsa_4096_tps: 350,  ecc_p256_tps: 3000,  ecc_p384_tps: 2000}
    t5000_enterprise: {rsa_2048_tps: 14000, rsa_4096_tps: 3500, ecc_p256_tps: 16000, ecc_p384_tps: 16000}
    luna_tablet_backup: {rsa_2048_tps: 62, rsa_4096_tps: 8, ecc_p256_tps: 383, ml_dsa_87_tps: 20, note: "use-case specific model; the only published post-quantum (ML-DSA-87) HSM throughput in the sheet [R7 §F2]"}
  cluster_crosscheck: "56,000 TPS ECIES P-256 decrypt with 8 HSMs in a cluster; test host Intel Xeon E5-2640 v4 @ 2.40 GHz × 40 cores, CentOS 8, Luna Network HSM A790 [R7 §F2: Luna HSM for 5G brief] — confirms the A790 rating is reached on real server hardware"
  queue_depth: {value: null, status: todo-calibrate, calibration: "client-library session concurrency; not published"}
software_crypto: {}                          # appliance; host costs come from backend/cloud-vm-x86
radio: {rat: none}
power_w: {value: 100, typical_w: 84, source: "Luna Network HSM: 100 W max / 84 W typical, 1U [R7 §F2]; Luna PCIe: 18 W max / 14 W typical"}
reliability: {mtbf_h: 171308, source: "Luna Network HSM [R7 §F2]; Luna PCIe 997,508 h"}   # ext; availability model in §4 (high tier)
certification: "FIPS 140-2 Level 3 and FIPS 140-3 Level 3; Common Criteria EAL4+ [R7 §F2]"   # ext
alternatives_not_used:
  utimaco: "CryptoServer CSe '100 tps for 256-bit ECC keys'; u.trust GP HSM Se-series 'up to 40,000 RSA-2048 sig/s' — UNVERIFIED (search snippets only, datasheet 404) [R7 §F3]"
  aws_cloudhsm_nshield: "ECDSA rates UNVERIFIED (no numeric spec located) [R5 §B.5]"
storage_model: inherit
cost_table_overrides: {}
```

What is missing for the backend profiles:
- Any per-request RA/PCA/MA/CRLG throughput — `NOT PUBLISHED` [R7 §F4]; the M/M/c parameters of §4 remain `TODO: calibrate`.
- CPU model behind the OpenSSL Cookbook figures; the community i7 row is `UNVERIFIED` [R7 §F1]; the liboqs reference table's host CPU is `UNVERIFIED` [R5 §B.1].
- Luna: sign/verify split of the ECC tps, per-op latency, session concurrency, internal RAM — `not-published`; ML-DSA-87 only on the T-series tablet/backup model (20 tps); ML-KEM, LMS/HSS throughput `not-published` [R7 §F2].
- Utimaco, AWS CloudHSM, nShield — `UNVERIFIED` [R7 §F3; R5 §B.5].

### 7.8 `bs/lte-macro-generic` — cellular macro base station

R7 and R5 contain no base-station data. The profile therefore carries **only** the 3GPP V2X evaluation-methodology assumptions from the cellular research sheets — inter-site distance, macro Tx power, receiver noise figures and the eNB–UE path-loss reference — and leaves every hardware and scheduling parameter `TODO: calibrate`. Note that the TR 36.885 ISD table itself could not be read (corrupted cached `.doc`), so the ISD values come from TR 37.885 / TR 38.913, which TR 37.885 states carry over from TR 36.885 Annex A.1.3 [R11 §A4].

```yaml
id: bs/lte-macro-generic
kind: base-station
version: 2026-09-18.1
sources:
  - {kind: spec, ref: "3GPP TR 37.885 V15.3.0 Tables 6.1.1-1/2, 6.1.3-1/2 [R11 §A4; R2 Topic D.2: tr37885.txt]", accessed: 2026-09-18}
  - {kind: spec, ref: "3GPP TR 38.913 Tables 6.1.8-1, 6.1.9-1 [R11 §A4: tr38913.txt; §7.10 #40]", accessed: 2026-09-18}
  - {kind: spec, ref: "3GPP TR 36.885 V14.0.0 Annex A.1.1, A.1.4 [R2 Topic A, D.1; R2c]", accessed: 2026-09-18}
cpu: {cores: null, clock_hz: null, arch: null, dmips: null, status: todo-calibrate, calibration: "no base-station compute data in R7/R5; the §5 coverage/capacity model does not need it at abstract/medium tier"}
ram_bytes: {value: null, status: todo-calibrate}
flash_bytes: {value: null, status: todo-calibrate}
hsm: {kind: none, part: null, ops: {}, queue_depth: null}   # no security operations modelled at the BS; Uu security is out of scope
software_crypto: {}
radio:
  rat: [lte-uu]                              # ext value; Uu link for top-up/report paths (§5)
  chipset: null
  deployment:                                # ext; 3GPP evaluation assumptions
    isd_m_urban: 500                         # TR 37.885 Table 6.1.3-1 urban macro ISD [R11 §A4]; TR 38.913 urban grid for connected car, macro 500 m [R11 §A4]
    isd_m_highway: 1732                      # TR 37.885 Table 6.1.3-1/2 highway macro ISD (500 m optional) [R11 §A4]; TR 38.913 highway 1732 m (500 optional) [R11 §A4]
    isd_tr36885_status: "UNVERIFIED this session — cached TR 36.885 copy undecodable; TR 37.885 cites TR 36.885 Annex A.1.3 for BS placement, implying the same 500 / 1732 m baseline [R11 §A4]"
    inter_rsu_m_options: [50, 100]           # TR 38.913 highway inter-RSU distance [R11 §A4]
    system_bandwidth_hz_max: {dl_ul: 200.0e6, sl: 100.0e6, note: "TR 37.885 aggregated BW below 6 GHz [R11 §A4]; actual carrier BW is a scenario parameter — TODO: calibrate"}
  tx_power_dbm:
    min: {value: null, status: todo-calibrate}
    max: 49                                  # TR 37.885 Table 6.1.1-1 macro BS Tx power, below 6 GHz [R11 §A4]; above 6 GHz 43 dBm with EIRP <= 78 dBm
    default: 49
    source: "[R11 §A4: TR 37.885 Table 6.1.1-1]"
  ue_tx_power_dbm: {value: 23, note: "UE / UE-type RSU 23 dBm, 33 dBm not precluded [R11 §A4; R2 Topic A: TR 36.885 Annex A.1.1]"}   # ext; used by the Uu uplink budget
  sensitivity_dbm: {per_mcs: null, status: todo-calibrate, calibration: "derive from BS noise figure + bandwidth + MCS SINR thresholds in the PHY model (04-models §10)"}
  noise_figure_db: {value: 5, source: "TR 37.885 §6.1.1 BS RX noise figure 5 dB [R2 Topic D.2]; UE RX noise figure 9 dB [R2 Topic A, D.2; R2c]"}
  pathloss_reference: {model: "PL = 128.1 + 37.6 log10(R[km]) dB, shadowing sigma 8 dB, decorrelation 50 m, SCM NLOS fast fading", source: "TR 36.885 Table A.1.4-2 eNB–UE (Uu) [R2 Topic D.1; R2c]"}   # ext; the propagation plug-in owns this, listed for provenance
  antenna:
    gain_dbi: {value: null, status: todo-calibrate, calibration: "BS antenna gain not extracted from TR 37.885 in R2/R11; take from TR 37.885 §6.1.1 / TR 38.901 UMa tables"}
    height_m: {value: null, status: todo-calibrate, calibration: "BS antenna height not extracted in R2/R11 (TR 38.901 UMa/RMa reused by TR 37.885 Table 6.2.1-2 with hE = 0.25 m [R2c]); take from TR 38.901"}
    pattern: {value: null, status: todo-calibrate, calibration: "sectorised pattern per TR 37.885 §6.1.1; not extracted"}
    source: "[R2c; R11 §A4]"
capacity:                                    # §5 coverage/capacity model parameters
  scheduling_delay_model: {value: null, status: todo-calibrate}
  latency_distribution: {value: null, status: todo-calibrate, calibration: "R11 §A gives MEC/transport-network latency components (e.g. TN latency MEC@gNB mean 0.402 ms) for the backhaul model, not a Uu scheduling delay; measure or take from 04-models §10"}
  handover_interruption_ms: {value: null, status: todo-calibrate}
  outage_model: {value: null, status: todo-calibrate}
backhaul: {latency_ms: null, bandwidth_bps: null, status: todo-calibrate}
power_w: {value: null, status: todo-calibrate}
storage_model: inherit
cost_table_overrides: {}
```

What is missing for `bs/lte-macro-generic`: everything except ISD, macro Tx power, UE Tx power, noise figures and the Uu path-loss reference — BS antenna gain/height/pattern, carrier bandwidth, per-MCS sensitivity, scheduling delay, latency distribution, handover and outage models, backhaul, compute and power are all `TODO: calibrate`; the TR 36.885 ISD table itself is `UNVERIFIED` [R11 §A4].

### 7.9 Comparison tables

**Profiles (and their documented variants).** `NP` = NOT PUBLISHED; `UV` = UNVERIFIED; `EA` = 3GPP evaluation assumption; `proxy` = flagged proxy from another device or board; software ECDSA and PQ columns are single-core software figures on the platform named in the source column.

| Profile | Cores / clock | RAM | HSM verify/s | Sign latency | SW ECDSA-P256 verify/s | PQ verify/s | Radio | Sources |
|---|---|---|---|---|---|---|---|---|
| `obu/cohda-mk5` | 2 (i.MX6 DL, 4000 DMIPS) / clock NP | 1 GB | NP for SXF1700; proxy 2,000 (NXP SAF5400 engine) | NP (SXF1700) | NP; proxy 775.3 (Pi 3B A53) or 1,550.7 (Pi 4 A72) | NP | DSRC, NXP SAF5100, +22 dBm sys / +23 dBm per port, -99 dBm @ 3 Mbps | [R7 §A1, §D5, §D8] |
| `obu/unex-obu-301-craton2` (reference) | 2 × Cortex-A7 @ 600 MHz, 1140 DMIPS/core | 128 MB | >2,500 (on-chip HW engine) | <9 ms, >110/s (eHSM) | NP; proxy 775.3 (Pi 3B) | NP | DSRC (301E) / C-V2X (351U), CRATON2 + PLUTON2, >+20 dBm, <-92 dBm, 5 dBi | [R7 §A4, §A10] |
| `obu/cohda-mk6c-qualcomm-9150` | i.MX 8QXP, cores/clock NP (sibling MK6 OBU: 800 MHz, 8544 DMIPS) | NP (sibling 1 GB) | SXF1800 vendor claim >1,000 (system-level); 9150 engine NP | NP (SXF1800); SW 7.820 ms | 0.001 ms as published, anomalous (Cohda MK6, Botan) | Falcon 2,243; Dilithium 5,299; SPHINCS+ 184; XMSS 359 (Cohda MK6) | C-V2X, Qualcomm 9150, 21.5 dBm, -93.4 dBm | [R7 §A3, §B1, §D4, §E3] |
| `obu/generic-automotive-soc-no-hsm` | proxy 4 × Cortex-A72 @ 1.5 GHz (Pi 4B) | proxy 1 GB (Danlaw) | none (H2) | SW 244 us (OpenSSL) | 1,550.7 (OpenSSL 1.1.1d, Pi 4); A53: 775.3 / 908.4; A76 wolfSSL 14,933 vs mbedTLS 592 | ML-DSA-44 3,213.9; Falcon-512 7,866.3 (liboqs, Pi 4) | C-V2X, Quectel AG15, 23 ± 2 dBm, -93 / -96 (SIMO) dBm | [R7 §D8, §B2, §A7; R5 §B.2, §B.5] |
| variant `…-mcu-aurix-tc3xx-hsm` | host TriCore NP; HSM Cortex-M3 @ 100 MHz | 96 KB HSM RAM | 100 @ 100 MHz | 5 ms (200/s) | — | — | as host | [R7 §D1] |
| variant `…-optiga-trust-m` | — | — | ≈11.8 (~85 ms/op) | ~60–70 ms | — | — | — | [R7 §D3] |
| `obu/pq-capable-hypothetical` | Cortex-A76 @ 2.4 GHz (Pi 5 proxy), cores NP | NP | none — no vendor PQ accelerator found | SW: ML-DSA-44 531 us, Falcon-512 298 us | 14,933 (wolfSSL, Pi 5) | ML-DSA-44 8,139.0; Falcon-512 19,831.3 (liboqs, Pi 5); pessimistic Cohda MK6: Falcon 2,243, Dilithium 5,299 | inherits §7.3 C-V2X block | [R5 §B.2, §B.5; R7 §E3, §E5] |
| `rsu/commsignia-its-rs4` | 4 (i.MX 6) @ 792 MHz | 2 GB DDR3 | ">2000" (units/s implied; radio-variant engine) | "<50 usec" as extracted, UV; 50 ms corroborated (SLI97) | NP | NP | hybrid, variant: Autotalks Secton / NXP SAF5100 / Marvell 88W8987PA / Qualcomm 9150; TX/RX variant-dependent | [R7 §C2, §D2] |
| `rsu/cohda-mk5-rsu` | NP (proxy: MK5 OBU) | NP | NP (proxy §7.1) | NP | NP | NP | DSRC, SAF5100, +22 dBm, -99 dBm @ 3 Mbps, 3–27 Mbps | [R7 §C1] |
| `backend/cloud-vm-x86` | x86-64, cores scenario, CPU NP | scenario | none | SW 48.8 us (20,508.1/s, OpenSSL Cookbook) | 6,566.2 (OpenSSL Cookbook); i9 wolfSSL 61,357; CAMP 2016: 300 | Falcon-512 27,933 (i5-8259U); Dilithium2 118,412 cyc (Skylake AVX2) | none | [R7 §F1, §F4, §F5; R5 §B.1, §B.5] |
| `backend/hsm-appliance-luna7` | appliance, NP | NP | ECC P-256 tps: 2,000 / 10,000 / 22,000 (A700 / A750 / A790); T-2000 3,000; T-5000 16,000 | 1/tps (latency NP) | — | ML-DSA-87 20 tps (T-series tablet/backup only) | none | [R7 §F2; R5 §B.5] |
| `bs/lte-macro-generic` | TODO | TODO | none | — | — | — | LTE Uu; ISD 500 m urban / 1732 m highway (EA), 49 dBm macro Tx (EA), BS NF 5 dB, UE NF 9 dB (EA) | [R11 §A4; R2 Topic A, D.2; R2c] |

**Documented devices not profiled (reference rows; all crypto rates NP unless stated).**

| Device | Class | CPU | RAM / flash | HSM | Verify/s | Max TX / RX sens. | Sources |
|---|---|---|---|---|---|---|---|
| Cohda MK6 OBU (production) | DSRC + C-V2X OBU | NXP i.MX 8, 8544 DMIPS @ 800 MHz + Qualcomm SA515 | 1 GB / 16 GB eMMC | NXP SXF1800 (FIPS 140-2 L3) | 2,000 per SAF5400 engine (×2 fitted) | DSRC +22 / C-V2X +21.5 dBm; -99 dBm @ 3 Mbps | [R7 §A2, §D5] |
| Danlaw AutoLink | DSRC or C-V2X OBU | dual-core @ 800 MHz | 1 GB / 8 GB eMMC | NP | NP | NP; 400 mA @ 12/24 V | [R7 §A7] |
| Ficosa C-V2X OBU | C-V2X OBU | NP (named "Main/HMI/CV2X processors") | NP | NP; Green Hills Integrity OS SCMS client | NP | 20 dBm / -97 dBm typ. @ 6 Mbps; <7 W peak, <3 W typ. | [R7 §A8] |
| Commsignia ITS-OB4 | dual-mode OBU | NP | NP | "built-in tamper-proof HSM" | NP | NP | [R7 §A6] |
| Savari MobiWAVE MW2000 | C-V2X OBU | Qualcomm 9150 | NP | Infineon eSE (SLI97/SLE97 family) | NP | NP | [R7 §A9] |
| Autotalks CRATON (2013) | DSRC chipset (gen-1) | Cortex-R4F @ 240–360 MHz + 2 × ARC 625D | 96 KB SRAM | ext. Infineon SLE97 | 3 engines, <2 ms each; ">2000/s" per Unex OBU-201U | 4.5–25 dBm; per-MCS -97 … -78 dBm | [R7 §A5; R5 §C] |
| Kapsch RIS-9260 / 9360 | DSRC+C-V2X / C-V2X RSU | x86 dual-core @ 1.33 GHz, ECC RAM | 1 GB ECC / 4 GB | HSM, ECC; FIPS 140-2 L3, CC EAL4+ | NP | DSRC 20 dBm / -92 dBm @ 6 Mbps; C-V2X 20 dBm / -95 dBm; <25 W PoE | [R7 §C3, §C4] |
| Siemens / Yunex RSU | DSRC (+C-V2X, Yunex) RSU | dual-core @ 800 MHz | 1 GB / NP | unnamed HSM | NP | NP / -97 dBm; 12 W max; ~2,500 m open-field LOS | [R7 §C5] |
| Quectel AG15 | LTE-V2X module | 1.28 GHz Cortex-A7 (in-module) | NP | none (host) | NP | 23 ± 2 dBm / -93, SIMO -96 dBm | [R7 §B2] |
| Microchip ATECC608 | IoT-class SE | — | 1,400 B EEPROM | is the SE | NP (timing table omitted from all public datasheets) | — | [R7 §D6] |
| Cortex-M7 @ 216 MHz (STM32F767ZI) | PQ MCU reference | Cortex-M7 | 512 KB / 2 MB | — | Falcon-512 385 (2.6 ms); Dilithium-2 152 (6.6 ms) | — | [R7 §E1] |

**USDOT Connected Vehicle Pilot deployment facts** [R7 §C7]. Counts are corroborated across 2–3 independent secondary sources each (conference presentations, ITS JPO/FHWA pages, news coverage) because the primary ITS JPO/FHWA PDF briefings returned HTTP 403 during the research session; treat them as **directionally reliable, not to-the-unit certified**. No source reviewed gives RSU mounting heights.

| Site | RSUs | OBU-equipped vehicles | Backhaul | Sources (accessed 2026-09-18) |
|---|---|---|---|---|
| New York City | ~400–470 (varies by document vintage) | ~3,000 Aftermarket Safety Devices; early planning cited a 10,000-ASD goal that was not the final deployed count | AT&T FirstNet cellular for RSU backhaul; existing NYCWiN wireless also used between RSE and back office | tti.tamu.edu NYC CV Pilot presentation; its.dot.gov; NYU C2SMART NYC CV Pilot page [R7 §C7] |
| Tampa (THEA) | 47 RSU locations (Reversible Express Lane and CBD) | up to 1,000 | fiber where available (existing REL fiber; City of Tampa CBD fiber project); cellular as interim backhaul where fiber was not yet available | its.dot.gov thea_cvp_wireless.htm; FDOT THEA CVP page [R7 §C7] |
| Wyoming (WYDOT, I-80) | ~75 planned; ~76 reported deployed in some sources | ~325 equipped (~170 heavy trucks + ~150 WYDOT fleet); ~400 planned | NOT PUBLISHED in sources reviewed (no fiber/cellular split found); rural RSU encounter frequency drove certificate-lifetime design (R6) | itskrs.its.dot.gov Wyoming executive briefings; traffictechnologytoday.com [R7 §C7] |
| National aggregate (pre-consolidation) | 6,182 DSRC RSUs deployed + 1,916 planned | 15,506 OBU-equipped vehicles + 3,371 planned | — | FCC, "Modernizing the 5.9 GHz Band," First Report and Order, ET Docket 19-138, 2020-10-28, p. 14, citing AASHTO data [R7 §C7: txt/fcc2020factsheet.txt] |

### 7.10 References

Research sheets (in-repo copies are byte-identical to the scratchpad originals; section anchors used above refer to these):
- R7 — `docs/design/research/R7-hardware.md` (V2X hardware fact sheet, compiled 2026-09-18).
- R5 — `docs/design/research/R5-crypto-pq-hsm.md` (§B verification and signing costs, §C automotive HSM throughput).
- R2 — `docs/design/research/R2-cv2x.md` (Topic A, Topic D.1/D.2: TR 36.885 / TR 37.885 assumptions); R2c — `docs/design/research/R2c-3gpp-channel-models.md`; R11 — `docs/design/research/R11-cellular-safety-perception.md` (§A4 ISD, bandwidth, Tx power).
- Local caches cited as `pdftxt/…`, `txt/…`, `*.txt` live under the scratchpad `research/` directory and have no public URL recorded unless listed below.

Primary sources with URLs (all accessed 2026-09-17/18 per the sheets):
1. Cohda Wireless MK5 OBU product brief (2024) — https://cohdawireless.com/wp-content/uploads/2024/06/CW_DL-Product-Brief-sheet-MK5-OBU.pdf [R5 §C]
2. Cohda MK5 radio module datasheet v1.2.0 (May 2015), FCC filing — https://fccid.io/2AEGPMK5RSU/Users-Manual/User-Manual-2618067.pdf [R5 §C]
3. Cohda MK6C EVK product brief — https://cohdawireless.com/wp-content/uploads/2024/06/CW_DL-Product-Brief-sheet-MK6C-EVK.pdf [R5 §C]
4. Cohda MK6 OBU product briefs (2023, 2025) and MK5 RSU brief — local cache only (`pdftxt/cohda_mk6_obu_brief_2025.txt`, `pdftxt/cohda_mk6_obu_brief_2023.txt`, `pdftxt/cohda_mk5_rsu_brief.txt`); URL not recorded in R7.
5. Unex OBU-201U Specification v2.03 (2017-03-06), FCC filing (CRATON gen-1 + SLE97; per-MCS sensitivity) — https://apps.fcc.gov/els/GetAtt.html?id=201591&x=. [R5 §C; R7 `unex_fcc.txt`]
6. Autotalks CRATON2/SECTON eHSM FIPS 140-2 Security Policy, CMVP #3556 — https://csrc.nist.gov/CSRC/media/projects/cryptographic-module-validation-program/documents/security-policies/140sp3556.pdf [R5 §C; R7 §A4]
7. Unex OBU-301E / OBU-351U information sheets — local cache (`pdftxt/unex_obu301e.txt`, `pdftxt/unex_obu351u.txt`); URL not recorded in R7.
8. Autotalks product pages — https://auto-talks.com/products/craton2/ ; https://auto-talks.com/products/secton/ ; https://auto-talks.com/products/tekton3/ [R5 §C; R7 §A4]; Autotalks SLI 97 press release ("…less than 50 milliseconds signing latency"), auto-talks.com via WebSearch [R7 §D2]; Autotalks CRATON datasheet (2013), local cache `pdftxt/autotalks_craton_datasheet.txt`.
9. Commsignia OBU page — https://commsignia.com/products/obu [R7 §A6]
10. Commsignia ITS-RS4 Product Brief v0.9.3 (2020-04-22) — https://omniair.org/wp-content/uploads/2025/10/Commsignia_ITS_RS4_ProductBrief_v.09.3_22042020.pdf [R5 §C; R7 §C2]
11. Qualcomm 9150 C-V2X press release (2017-09-01), local cache `pdftxt/qualcomm_9150_pr_2017.txt`; product page — https://www.qualcomm.com/products/automotive/qualcomm-c-v2x-9150 (no body text retrieved) [R5 §C]
12. NXP SAF5400 fact sheet SAF5400V2XFS REV 2 — https://www.nxp.com/docs/en/fact-sheet/SAF5400V2XFSA4.pdf [R5 §C; R7 §D5]
13. NXP SXF1800 product page — https://www.nxp.com/products/SXF1800 ; block diagram — https://www.nxp.com/assets/block-diagram/en/SXF1800.pdf [R7 §D4]; Security Target, local cache `pdftxt/nxp_sxf1800_security_target.txt`.
14. Quectel AG15 hardware design guide — local cache `pdftxt/quectel_ag15_hwdesign.txt`; Quectel AG520R/AG521R spec (fetch failed 404/timeout) — https://www.quectel.com/wp-content/uploads/2021/05/Quectel_AG520RAG521R_Series_Automotive_Module_Specification_V1.0.pdf [R7 §B3]
15. Infineon AURIX TC3xx HSM quick training — https://www.infineon.com/dgdl/Infineon-AURIX_TC3xx_Hardware_Security_Module_Quick-Training-v01_00-EN.pdf [R5 §C, listed as not fetched there; R7 §D1 read the cached extract `pdftxt/aurix_tc3xx_hsm_training.txt`]; Infineon cybersecurity compendium, local cache `pdftxt/infineon_cybersec_compendium.txt`.
16. Infineon SLI 97 part page — https://www.infineon.com/part/SLI-97CSINFX1M00PE (navigation shell only) [R7 §D2]; OPTIGA Trust M "Crypto Performance" wiki — https://github.com/Infineon/optiga-trust-m [R7 §D3]; Infineon/Savari use case — https://www.infineon.com/dgdl/Infineon-ISPN-Use-Case-Savari-Securing-V2X+communications-ABR-v01_00-EN.pdf [R7 §A9]
17. Raspberry Pi 4 OpenSSL 1.1.1d `openssl speed` gist (HimaJyun) — https://gist.github.com/HimaJyun/f05d3017dfb05a4ccb0def010bb2c91a [R5 §B.5; R7 §D8]; Raspberry Pi 3B/3B+ `openssl speed` gist (github.com/dchest, URL not recorded) [R7 §D8]
18. wolfSSL vs mbedTLS benchmark (June 2026; wolfSSL 5.9.1, mbedTLS 3.6.6) — https://www.wolfssl.com/wolfssl-vs-mbedtls-an-apples-to-apples-benchmark-across-intel-arm-cortex-a-and-cortex-m-and-risc-v-targets/ [R5 §B.5]
19. Berger, Lemoudden, Buchanan, "Post Quantum Migration of Tor", arXiv 2503.10238, Tables 6–8 — https://arxiv.org/pdf/2503.10238 [R5 §B.1, §B.2]
20. Microchip ATECC608B-TFLXTLS datasheet DS40002249B — https://ww1.microchip.com/downloads/aemDocuments/documents/SCBU/ProductDocuments/DataSheets/ATECC608B-TFLXTLS-CryptoAuthentication-Data-DS40002249.pdf [R7 §D6]
21. Danlaw AutoLink, Ficosa C-V2X OBU, Kapsch RIS-9260/9360, Siemens/Yunex RSU datasheets — local cache (`pdftxt/danlaw_autolink.txt`, `pdftxt/ficosa_cv2x_obu.txt`, `pdftxt/kapsch_ris9260.txt`, `pdftxt/kapsch_ris9360.txt`, `pdftxt/siemens_rsu_datasheet.txt`, `pdftxt/yunex_rsu.txt`); URLs not recorded in R7.
22. Twardokus, Bindel, Rahbari, McCarthy, "When Cryptography Needs a Hand: Practical Post-Quantum Authentication for V2V Communications", NDSS 2024 — https://www.ndss-symposium.org/wp-content/uploads/2024-267-paper.pdf ; ePrint — https://eprint.iacr.org/2022/483 [R5 §B.6; R7 §E3]
23. PQ-V2Verifier testbed — https://github.com/twardokus/pq-v2verifier ; VehicleSec 2024 demo — https://www.ndss-symposium.org/wp-content/uploads/vehiclesec2024-7-demo.pdf [R5 §B.6; R7 §E4]
24. Howe & Westerbaan, "Benchmarking and Analysing NIST PQC Lattice-Based Signature Scheme Standards on the ARM Cortex M7", NIST 4th PQC Standardization Conference 2022 — https://csrc.nist.gov/csrc/media/Events/2022/fourth-pqc-standardization-conference/documents/papers/benchmarking-and-analysiing-nist-pqc-lattice-based-pqc2022.pdf [R5 §B.2, §B.3; R7 §E1]
25. pqm4 benchmarks — https://raw.githubusercontent.com/mupq/pqm4/master/benchmarks.csv [R5 §B.3]; R7 §E2 used a local `pqm4_benchmarks.csv` of a different vintage (contains fndsa_provisional rows).
26. Ristić, OpenSSL Cookbook, "Performance" — https://www.feistyduck.com/library/openssl-cookbook/online/openssl-command-line/performance.html [R7 §F1]
27. Emill/P256-Cortex-M4 — https://github.com/Emill/P256-Cortex-M4 [R5 §B.5]
28. wolfSSL benchmarks page (Cortex-M rows, UNVERIFIED) — https://www.wolfssl.com/docs/benchmarks/ [R5 §B.5]
29. CRYSTALS-Dilithium round-3 specification (2021-02-08), Table 1 — https://pq-crystals.org/dilithium/data/dilithium-specification-round3-20210208.pdf [R5 §B.1]
30. Pornin, "New Efficient, Constant-Time Implementations of Falcon", ePrint 2019/893 — https://eprint.iacr.org/2019/893.pdf [R5 §B.1, §B.3]
31. falcon-sign.info reference implementation benchmarks — https://falcon-sign.info/ [R5 §B.1; R7 §F5]
32. "Optimized Vectorization Implementation of CRYSTALS-Dilithium", arXiv 2306.01989 — https://arxiv.org/pdf/2306.01989 [R7 §F5]
33. "Falcon Verify on AVX-512: Speed Records", ePrint 2026/1539 — https://eprint.iacr.org/2026/1539 [R7 §F5]
34. SPHINCS+ round-3.1 specification, Tables 4 and 6 — https://sphincs.org/data/sphincs+-r3.1-specification.pdf [R5 §B.1]
35. CAMP VSC5 Consortium, "SCMS Proof-of-Concept Implementation — EE Requirements and Specifications Supporting SCMS Software Release 1.1" (2016-05-04), Crypto Basics p. 75 — mirror https://ccv.eng.wayne.edu/reference/SCMS_POC_EE_Requirements20160111_1655.pdf [R5 §B.5; R7 §F4 `camp_ee_req.txt`]
36. Thales Luna Network HSM 7 product brief (Dec 2019) — https://cpl.thalesgroup.com/sites/default/files/content/product_briefs/field_document/2020-04/thales-luna-network-7-hsm-pb-a.pdf [R5 §B.5; R7 §F2]; Luna PCIe brief, TCT T-series family sheet, Luna HSM for 5G performance brief — local cache (`pdftxt/thales_luna_pcie.txt`, `pdftxt/thales_hsm_family_dlt.txt`, `pdftxt/thales_5g_perf.txt`).
37. Simplicio et al., "Faster verification of V2X BSM messages via Message Chaining", ePrint 2022/133 — https://eprint.iacr.org/2022/133.pdf [R5 §C.2; R7 §H1, §H4]
38. Krishnan & Weimerskirch, "Verify-on-Demand", SAE Int. J. Passenger Cars — Mech. Syst. 4:536–546, 2011 (cited via ePrint 2022/133 ref. [7]) [R7 §H1]; Rostami, Krishnan, Gruteser, "V2V Safety Communication Scalability Based on the SAE J2945/1 Standard", local cache `pdftxt/rutgers_j2945_scalability.txt` [R7 §H3]
39. FCC, "Modernizing the 5.9 GHz Band", First Report and Order, ET Docket No. 19-138 (2020-10-28), p. 14 — local cache `txt/fcc2020factsheet.txt` [R7 §C7]; USDOT pilot secondary sources: tti.tamu.edu NYC CV Pilot presentation, its.dot.gov (incl. thea_cvp_wireless.htm), NYU C2SMART, FDOT THEA CVP page, itskrs.its.dot.gov Wyoming executive briefings, traffictechnologytoday.com — WebSearch, accessed 2026-09-18; primary ITS JPO/FHWA PDFs returned HTTP 403 [R7 §C7].
40. 3GPP TR 37.885 V15.3.0 (ATIS reprint, local cache `tr37885.txt`) [R11 §A4; R2 Topic D.2]; 3GPP TR 38.913 V14.2.0 — https://www.etsi.org/deliver/etsi_tr/138900_138999/138913/14.02.00_60/tr_138913v140200p.pdf (URL constructed from the ETSI path pattern, not independently re-fetched) [R11 §A4]; 3GPP TR 36.885 V14.0.0 Annex A (local cache `src/tr36885_raw.txt`, verified directly in R2; the R11 copy was undecodable) [R2 Topic A, D.1; R2c].
41. 5GAA "List of C-V2X Devices" (April 2024) — local cache `pdftxt/5gaa_cv2x_devices_2024.txt` [R7 §A4, §B4]; OmniAir certified-product listings — omniair.org (accessed 2026-09-18) [R7 §A6, §A7, §A9].
