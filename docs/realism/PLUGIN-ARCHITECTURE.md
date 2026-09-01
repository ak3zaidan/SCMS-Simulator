# Plugin architecture and standards plan

**Status:** **phases 0, 1 and 2 implemented**; phases 3-6 remain design.

**Out-of-process DETECTORS shipped (2026-08-31), and one number in §2.3 below needs correcting.**
`plugins.check[].isolated = true` runs a third-party check in its own interpreter
(`src/scms_sim_ref/api/isolate.py`): the engine serialises the `Observation` — and only the
`Observation` — over a length-prefixed JSON pipe and reads back a score, so the broadcast dict, the
`Vehicle`, the config object and the global `rng` are not in that process at all. Measured on a 300 s
593-vehicle run: **the same `data_digest` as the in-process path**
(`81c194ed…` alongside the built-ins, `d71b29f0…` solo), at **+40.8 µs per delivered message /
4.3× wall clock** over 960 509 observations. The identical hostile check that reads
`sys._getframe(1).f_locals["b"]` files 1 910 label-derived reports in process and **zero** isolated,
where its walk terminates in the serialiser's own frames. Full evidence, and the four things the mode
does **not** close (the shared filesystem first among them), are in
`docs/realism/DETECTOR-PLUGIN.md` §2.8.
**The correction:** §2.3's round-trip arithmetic ("per-link IPC is ~10⁴× too slow") is right for the
2 ms *socket* RTT it assumes for an ns-3 federate, and wrong by ~40× for a local *pipe*, which
measures ~40 µs on this host. Per-step batching therefore remains the right ABI for the CHANNEL slot
— where an out-of-process backend is a foreign runtime — and is the wrong shape for the DETECTOR
slot, where the fusion consumes one message's score before the next message is scored.

**Adversarial review (2026-08-31).** Phase 2 landed without its verifier. Six behavioural defects and
three mis-transcribed digests were found and fixed; every one is reproduced, fixed and re-measured in
**`PLUGIN-CONFORMANCE-EVIDENCE.md` §7**. Three of them share a shape worth naming here, because it is
the failure mode this document is most exposed to: **`RngNamespace.begin_step`, `check_outcome` and
`package_sha256` were each defined, documented, individually tested — and called by nothing.** The
step term in the plugin RNG key was dead (`_step` stayed `-1` for whole runs, so a "stateless
per-packet" stream was a per-run constant); the only outcome-validation on the default third-party
path did not run; and the lock hashed one source FILE, so an edit to a sibling module replayed
silently wrong with `verify-plugins` reporting clean. A function with no caller reads exactly like a
working defence. Also corrected: C3 detected only the two RNG attacks that provably cannot reach the
engine's stream while missing both that do; C9 could not fail a model with a constant delivery
probability; `env["config"]` was the live mutable `PipelineConfig`; and three lock digests quoted in
the evidence document were from a different install state than the artifacts they labelled.
After the review: full suite **922 passed in 992.65 s**, exit 0; the reference run still digests
`b25f2137cf14dd50…` with every count and metric unchanged; all 8 pinned goldens hold. The review's
own findings were then re-verified independently, which added conformance check
**C13_config_not_mutated** (the plugin-side half of the `env["config"]` defect — the engine gate
alone cannot tell an author their model is wrong before they produce an artifact) and the three
regression tests that assert the RNG step term is live, the absence of which is the whole reason
defect 1 survived.

**Phase 2 (2026-08-31).** `src/scms_sim_ref/conformance/` ships the v1 suite (C1-C12, thirteen rows
— C6 has two arms; the adversarial review below added C13 for a fourteenth), delivered both as a
pytest-importable `ChannelModelContract` and as
`scms-poc conformance`; `scms-poc verify-plugins` is a no-simulation CI gate; `tools/verify_data.py`
gains six lock checks (`PL1`-`PL6`); and `--allow-plugin-drift` now **writes the drift into the new
manifest** (`plugins.drift_allowed`), the half of section 4.3 phase 1 deferred. Headline gate, all
five items measured and transcribed in **`PLUGIN-CONFORMANCE-EVIDENCE.md`**: an out-of-repo
distribution (`scms-demo-channel 0.1.0`, installed as its own wheel, importing only
`scms_sim_ref.api`) passes 13/13 checks (14/14 since C13) and reproduces `9abe9eea…` (now `be3b5dea…`; see below)
across two processes with 10 of 11
files byte-identical; one mutated byte of its source makes the manifest unreplayable **before step 0**
(exit 2, no output directory created); a `time.time()`-seeded model fails C1 **and** separately
yields `e3c94d7e…` vs `4275ef8a…` under an identical `provenance_digest`; and an oracle-reading model
fails **only** C6b while being byte-reproducible — proving the two detection layers are independent
rather than redundant. The suite also found two real defects: `logdistance` fails C8 (14.13 % of
delivered links land beyond its declared reach; 4.19 % would score `>= 1.0` on
`acceptanceRangeThreshold` for an honest sender) and now declares a quantified waiver; and phase 1's
`dist_sha256` was `null` for **every** normally-installed wheel, not just editable installs. All 8
pinned goldens unchanged; full suite **833 passed in 739.21 s**, exit 0.

**Phases 0 and 1 (2026-08-30).**
`src/scms_sim_ref/api/` ships the Protocols, `FieldSpec`, `RngNamespace`, the resolver and the
provenance lock; `PipelineConfig.plugins` is live and defaults to `{}`; `disc` / `logdistance` /
`geometric` are registry entries behind `PerLinkAdapter`; `manifest["standards_profile"]` is
corrected, and `manifest["runtime"]` / `manifest["plugins"]` are written. Gate evidence:
all 8 pinned goldens byte-identical (`0bd93655…`, `939b4faa…`, `b3a01d40…`, `48013901…`), the FULL
RNG draw sequence identical for all three built-ins (V2: 45 647 / 383 742 / 384 707 draws, matching
per-method counts and trace sha256), per-link outcomes identical (V3: 83 522 geometric link
decisions, 28 342 delivered, identical trace sha256), `config_schema()` 138 → 139 fields with a
one-block diff (V4), and `plugins.channel_model = {"ref": "geometric"}` byte-identical to
`--radio-model geometric`. **Date:** 2026-08-30.
**Commissioned by:** the directive that the simulator be *very modular, so a user can pick any
network system and run a simulation with it*, and be *standards-compliant* — with the worked
example "the user should be able to integrate thresholding into the simulator", i.e. plug in their
own misbehaviour detector **without forking the code**.

**Inputs.** The four audits in `docs/realism/investigation/modularity/` (radio seam, detector seam,
mobility/attack/PKI, standards) plus `docs/realism/STANDARDS-AUDIT.md`, and a prior-art review of
F2MD, ms-van3t/VaN3Twin, Eclipse MOSAIC, Artery/Vanetza and ns-3, plus an empirical Python-ASN.1
feasibility probe.

**Line numbers.** Every `run.py:N` below was re-verified against the working tree on 2026-08-30
(`run.py` = 4134 lines). Note `audit-radio-seam.json` is offset by roughly −9 lines from the live
tree (it cites `GeometricChannel` at 530, live is 539; global `rng` at 1943, live is 1952). Where
the audits and the live tree disagree, the numbers here are the live ones.

**Hard constraints this design must not break.**

1. **Determinism.** Same seed + config ⇒ byte-identical output, enforced by pinned digests. The
   default golden is `0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740`, pinned in
   at least 7 test files (`test_radio_propagation.py:23`, `test_attack_magnitude.py:28`,
   `test_combined_attacks.py:27`, `test_config_knobs.py:41`, `test_denm.py:50`,
   `test_engine_truth.py:18`, and the VRU/DENM goldens at `test_config_knobs.py:44,47`,
   `test_vru.py:27`, `test_vru_denm_harden.py:58`, `test_vru_spoofing.py:40`).
2. **The manifest replay contract.** `_write_manifest` (run.py:3788) serialises `cfg.__dict__`
   verbatim (run.py:3793); `config_from_dict` (run.py:1900) is its inverse.
3. **The MA-visible / ORACLE firewall** (`schemas/records.py:33-70`, `datagen/leakage_linter.py`).
4. **Windows host**, no shapely/scipy, WSL disabled ⇒ no in-tree ns-3, no C toolchain assumed.

---

## 0. Executive summary — the five decisions

| # | Decision | Rationale in one line |
|---|---|---|
| **D1** | The plugin ABI is a **fixed message vocabulary exchanged once per step**, not a per-link method call. | This is the only shape that lets an out-of-process ns-3/OMNeT++ backend exist at all (§2.3); per-link IPC is ~10⁴× too slow. |
| **D2** | **Discovery may be automatic; activation is always config-declared.** One `plugins` field on `PipelineConfig`, defaulting to `{}`. | Only a config field lands in `manifest["config"]` (run.py:3793) and replays through `config_from_dict`. Entry-point iteration order is machine state, and `DET_KEYS` order is digest-bearing. |
| **D3** | A plugin **never receives the global `rng`** (run.py:1952). It receives a seeded, namespaced stream factory. | The global stream's draw *count and order* are load-bearing across packet loss (3393), `report_prob` (3536), collusion (3566) and net_delay (2620). Capability-by-omission is the only enforceable control. |
| **D4** | Identity is recorded as a **content hash lock** in a new `manifest["plugins"]` block, and replay **fails loudly** on drift. | `config_from_dict` today drops unknown keys with a stderr warning (run.py:1911-1913) and exits 0 — a plugin-configured manifest would replay silently wrong. |
| **D5** | Standards work is **tiered by what ASN.1 the language actually requires**: real UPER for CAM/DENM/VAM, real COER for the 1609.2 envelope, **structural-only** for TS 103 759. | Empirically tested: `asn1tools` compiles the ETSI facilities modules and fails on X.681/X.683; TS 103 759 is built entirely on information object classes. |

**The single most important framing correction.** F2MD and ms-van3t — the two systems closest to
this repo's domain — do **not** solve the "add a component without forking" problem. F2MD selects
detectors with an integer-indexed enum plus a `switch` (`mdEnumTypes/MdAppTypes.h` +
`F2MDVeinsApp.cc`), which is the *identical* anti-pattern to this repo's closed `radio_model` enum
(run.py:809 / 1412 / `_ENUM_OPTIONS` / argparse `choices`) and is *worse*, because selection is by
array index. ms-van3t selects the network stack by **compiling a different scenario main file**.
So: **F2MD is the reference for the detector DECOMPOSITION (checks → score vector → fusion → report
format); Artery, ns-3 and MOSAIC are the references for REGISTRATION.** Any design memo that treats
F2MD as the registry model is copying the problem.

**Second correction, and it is decisive for §6.** `audit-standards.json` §7 and
`STANDARDS-AUDIT.md` describe a TS 103 759 containing `reportMetadataContainer`,
`misbehaviourTypeContainer` and `semanticDetectionReferenceCAM`. **Those types do not exist in the
published standard.** They are from the 2021 pre-standardisation literature (arXiv 2112.02184).
Grepping both published tags (`v2.1.1`, `v2.2.1`) of `forge.etsi.org/rep/ITS/asn1/mrs_ts103759`
returns zero hits for all three. The real structure is three fields (§6.3). I trust the Forge ASN.1
over the audit here without reservation: it is the normative source, it was fetched and grepped,
and the audit's names are traceable to a pre-standard paper. **The audit's substantive conclusion
nevertheless survives its naming error** — see §6.3.

---

## 1. Current extensibility, honestly assessed

The blunt answer: **there is no plugin machinery anywhere in the Python engine.**
`grep -rn "entry_points|importlib.metadata|register_|plugin|Protocol|ABC|abstractmethod"` over
`src/`, `tools/`, `gui/` returns zero hits. Every "seam" below is de-facto, not designed.

### 1.1 Cost to add one component today, per seam

| Seam | Pluggable today? | Files to edit | Distinct edit sites | Pinned digests broken | Can a third party do it without forking? |
|---|---|---|---|---|---|
| **Channel / network** | No — closed 4-way enum + concrete `if` | 1 (`run.py`) | **6** | 0 if the new model is not selected | **No** |
| **Detector (ungated)** | No — inline statements in the receive loop | 3 (`run.py`, `featurize.py`, `CamDetector.java`) | **14** | **8** | **No** |
| **Detector (gated, opt-in)** | No | 3 | **15** | 0 | **No** |
| **Mobility** | No — three inline branches in `Vehicle.true_state`; `car_follow` is a closure | 1–2 | **~8** | depends | **No** |
| **Road network** | **Partially yes** — `road_network="custom"` + a JSON document | 0 | 0 | 0 | **Yes** (the only working case) |
| **Attack behaviour** | No — 20-branch `if/elif`, order-frozen catalog | 3 | **≥9** | 8 if the catalog order moves | **No** |
| **PKI / credential backend** | No — concrete instantiation, no interface | 1 | ~4 | yes | **No** |
| **Message / wire format** | No — one bespoke dict, no codec | 1 | 1 site, unbounded blast radius | 8 | **No** |

**Channel, itemised (6 sites).** `radio_model: str = "disc"` (run.py:809); the `validate_config`
closed-enum raise; `_ENUM_OPTIONS["radio_model"]`; the argparse `choices=[...]`; the construction
`if cfg.radio_model == "geometric": geo_chan = GeometricChannel(...)`; and
`_emit_rssi = (cfg.radio_model == "geometric")` (run.py:1984), which gates the `rssi_dbm` column.
Plus three in-loop branches: `radio_logdist` at the step head, the candidate branch (3334-3364) and
the loss branch (3371-3394).

**Detector, itemised (14 sites, ungated).** `run.py` ×8: a threshold field on `PipelineConfig`; a
`validate_config` bound; a GUI `KNOBS` entry; the `DET_KEYS` tuple (run.py:2579); the compute site
(3458-3475); `MOTION_KEYS` / the VRU-suppression block; an argparse flag; the args→config wiring.
`featurize.py` ×3: `REASON_VOCAB` (:37-53), `DETECTORS` (:57-62), `_DETECTOR_DOCS` (:574-591).
`CamDetector.java` ×3 for engine parity. Then **8 pinned goldens must be recomputed and repinned**,
because a new `detnorm_*` column rewrites every `ma_reports` row (fan-out at run.py:2650-2651).

**The one byte-identical path that exists.** The opt-in gate:
`if _emit_station_type: DET_KEYS = DET_KEYS + ("vruImpersonation",)` (run.py:2588) and
`if _denm_enabled: ... + ("denmPlausibility",)` (run.py:2593). This is the in-tree precedent every
part of this design copies. **A plugin registry that adds a column when no plugin is declared
breaks eight goldens on day one.**

### 1.2 What already exists that is worth building on

1. **`GeometricChannel` is ~90% of a plugin ABI already** (run.py:539-711): `begin_step` (591),
   `cbr` (595), `collision_loss` (604), `evaluate` (688), plus the run-scalar `cap_m` (583-584).
   Critically it already proves the two properties the contract needs — it draws **zero** from the
   global `rng` (documented at run.py:543-554, and the reason `disc` stays byte-identical), and its
   persistent per-link state advances exactly once per step (the `st["step"] == self.step` guard).
   It is selected by an `if`, not a registry. **That `if` is the whole problem.**
2. **`config_schema()` (run.py:1869) + `_FIELD_META` (1663) is ~80% of an ns-3 Attribute system.**
   It emits `{type, default, group, widget, help, options, min, max, step, unit}` for all 138
   fields, and feeds the GUI `/api/schema`, the copilot, `--dump-config-schema` and the manifest
   with zero code per field. Its only defect is that it iterates
   `dataclasses.fields(PipelineConfig)` — a set closed at class-definition time.
3. **The keyed-RNG convention** `random.Random(f"{seed}:{label}:{ids}")` at ~20 sites. CPython
   seeds a Mersenne Twister from `int.from_bytes(a + sha512(a).digest())` for a `str` seed, so it
   is **`PYTHONHASHSEED`-independent and platform-stable** — and, unlike ns-3's index-based
   `AssignStreams`, it is order-free and creation-order-independent *by construction*.
4. **`road_network="custom"` + a `custom_network` JSON document** — the repo's own working proof
   that "supply a document, get behaviour" works end to end including `validate_config`.
5. **`PER_STEP_HOOK` / `LANE_CHANGE_HOOK` / `GAP_YIELD_HOOK`** (run.py:~1938-1946) — the accepted
   "default `None` ⇒ byte-identical" seam idiom. (Caveat: they are process globals; plugin
   *instances* must be per-run objects built inside `run_pipeline`, or in-process multi-run drivers
   like `datagen/foundry.py`, `campaign.py`, `massive.py`, `gui/agent.py` cross-contaminate.)
6. **`CamDetector.java` + `SignedCam.java`** — the Java side already did the extraction the Python
   side needs, and `SignedCam.java:22-31` is **structurally oracle-free by construction**. That DTO
   is the model for §2.2, not the Python broadcast dict.

### 1.3 The two holes that must be closed *before* any plugin ships

- **The leakage firewall does not cover the detector call site.** The broadcast dict `b` iterated
  at run.py:3331 is built at run.py:3195-3198 and carries `veh` (a whole `Vehicle` with
  `.is_attacker`, `.attack_type`, `.victims`), `x`, `y` (TRUE position), `falsified` (literally in
  `FORBIDDEN_FEATURE_KEYS`), `ghost`, `tspd`, `thdg`. A detector writing
  `det["myCheck"] = 3.0 if b["falsified"] else 0.0` is a perfect oracle that passes **every**
  existing check, because the linter is **name-based, post-hoc, and never called from `run.py`**
  (enforcement is only `featurize.py:516` on ML column names, and a non-raising scan in
  `validate.py`). Today the only thing preventing this is that all detector code is in-tree and
  reviewed. **The moment third-party code runs there, the project's stated core scientific-validity
  guarantee is void unless the DTO is the boundary.**
- **`config_from_dict` drops unknown keys with a stderr warning and continues** (run.py:1911-1913).
  A manifest carrying a plugin section would replay as a *different run with exit code 0* — the
  worst possible failure mode for constraint (2).

---

## 2. The interfaces

All interfaces live in a new, **dependency-free** package `scms_sim_ref.api` (later split out as a
separate distribution `scms-sim-api` so a plugin author's install closure does not pull the whole
engine — that is what makes "no fork" true in practice rather than in principle).

**`Protocol`, not `ABC`, as the published contract.** A `Protocol` is structural: the implementer
needs no import of ours, which is the point. But note honestly that *neither* enforces the ABI —
`@runtime_checkable isinstance` checks only member **presence**, never signatures, and an `ABC`
only checks at instantiation. So a third component does the real work: a **load-time
`inspect.signature` validator** against the declared interface version, failing before step 0 with
an error naming the offending parameter. We additionally ship optional convenience ABCs carrying
default implementations (`reach_m`, `capabilities()`, a no-op `begin_step`) for authors who prefer
inheritance.

### 2.1 `ChannelModel` — "pick any network system"

Derived from the **actual** de-facto interface `GeometricChannel` already implements, not invented.
Two levels: a per-link `LinkChannelModel` (what the built-ins are), and a per-step
`BatchChannelModel` (the canonical ABI, and the only one an out-of-process backend can implement).
The engine talks **only** to `BatchChannelModel`; `PerLinkAdapter` wraps a `LinkChannelModel`.

```python
# scms_sim_ref/api/channel.py
from __future__ import annotations
from dataclasses import dataclass
from typing import Iterable, Mapping, Optional, Protocol, Sequence, runtime_checkable

INTERFACE_VERSION = "ChannelModel/1.0"

# ---------------------------------------------------------------------------
# The message vocabulary. This — not a class signature — is the ABI.
# Modelled on Eclipse MOSAIC, where sns / ns3 / omnetpp are interchangeable because they
# subscribe to an IDENTICAL interaction set (RsuRegistration, VehicleUpdates,
# V2xMessageTransmission, AdHocCommunicationConfiguration, ...). See this repo's own
# third_party/veremi-nextgen/Generator/simulation/mosaic/etc/runtime.json:94-154.
# ---------------------------------------------------------------------------

@dataclass(frozen=True, slots=True)
class StationSnapshot:
    """One station's TRUE physical state for this step. ORACLE-side by necessity: channel
    physics must not be steerable by a position-falsifying attacker (run.py:3281-3282,
    RxChannel.java:11-17). A ChannelModel is therefore explicitly INSIDE the oracle boundary
    and MUST NOT be handed to, or be able to write into, the detection layer except through
    the declared LinkOutcome fields."""
    vid: int                      # rotation-stable true id; the persistent-state key
    x: float; y: float            # true position, local metres
    ant_h_m: float                # V2X_ANTENNA_HEIGHT_M or RSU_ANTENNA_HEIGHT_M
    blocker_h_m: float            # TR37885_BLOCKER_HEIGHT_M[veh_type]; 0.0 if not a blocker
    is_rsu: bool
    is_vru: bool
    tx_power_dbm: Optional[float] = None    # None => model default (the seam TPC/DCC needs)

@dataclass(frozen=True, slots=True)
class Transmission:
    """One PDU offered to the channel this step."""
    tx_index: int                 # index into the step's broadcast list; the canonical sort key
    tx_vid: int
    msg_type: str                 # "cam" | "denm" | "vam" | ...
    msg_count: int                # burst multiplier (today a scalar; see §7 phase 5)
    wire_size_bytes: int          # from MessageCodec.wire_size_bytes; feeds airtime -> CBR
    priority: int = 3             # EN 302 663 access category / user priority
    channel_id: str = "CCH"

@dataclass(frozen=True, slots=True)
class StepFrame:
    """Everything the channel is told about this step. Deterministically ordered:
    `transmissions` is sorted by tx_index, `receivers` by vid."""
    step: int
    t: float
    dt: float
    stations: Mapping[int, StationSnapshot]
    transmissions: Sequence[Transmission]
    receivers: Sequence[int]                 # rx vids, sorted
    weather_loss: float                      # WEATHER_RADIO_LOSS drop prob (run.py:138)
    env: Mapping[str, object]                # buildings handle, scenario events, read-only

@dataclass(frozen=True, slots=True)
class LinkOutcome:
    """Result for one (transmission, receiver) pair that the model says was DELIVERED.
    Undelivered links are simply absent — never emitted with heard=False."""
    tx_index: int
    rx_vid: int
    rssi_dbm: Optional[float]     # faded received power; MA-visible, gated column
    link_state: Optional[str]     # "LOS" | "NLOSv" | "NLOSb" | None
    delay_s: float = 0.0          # propagation + queueing; 0.0 == today's same-step delivery
    extras: Mapping[str, float] = ()   # namespaced -> `x_<pluginid>_<key>` columns only

@runtime_checkable
class BatchChannelModel(Protocol):
    """THE canonical channel ABI. One exchange per step."""

    interface_version: str        # must satisfy "ChannelModel/1.x"
    plugin_id: str                # reserved RNG/config/column namespace, [a-z0-9_]{2,32}

    def capabilities(self) -> frozenset[str]:
        """Declared, negotiated at load, recorded in the manifest, and used to gate optional
        output columns exactly as `_emit_rssi` does today (run.py:1984).
        Known: {"rssi", "link_state", "reach", "cbr", "delay", "stateful", "batch",
                "out_of_process", "tx_power", "per_frame"}.
        Reserved for built-ins ONLY, refused from third parties:
                {"legacy_global_rng", "loss_composition:additive_legacy"}."""

    @property
    def reach_m(self) -> float:
        """Run-scalar upper bound on delivery distance. MANDATORY. `GeometricChannel.cap_m`
        (run.py:583-584) today; read by the loop for the candidate window (run.py:3316) AND by
        the acceptanceRangeThreshold detector (`art_reach`, run.py:3469). A model that omits or
        understates it makes that detector wrong for every honest long link."""

    def begin_step(self, frame: StepFrame) -> None:
        """Advance any per-step state EXACTLY ONCE. `GeometricChannel.begin_step` (run.py:591)
        rebuilding the blocker index is the reference implementation."""

    def deliver(self, frame: StepFrame,
                candidates: Sequence[tuple[int, int, float]]) -> Iterable[LinkOutcome]:
        """`candidates` = (tx_index, rx_vid, distance_m), pre-filtered by reach_m and supplied in
        canonical order (tx_index asc within rx_vid asc). Return outcomes in ANY order; the engine
        re-sorts by (rx_vid, tx_index) before use, so an out-of-process backend's internal
        ordering can never affect the digest."""

    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float:
        """Optional (capability "cbr"). Default 0.0. `GeometricChannel.cbr` (run.py:595)."""

    def close(self) -> None:
        """Release subprocess/socket resources. Always called, including on the SIGINT path."""


@runtime_checkable
class LinkChannelModel(Protocol):
    """Convenience shape for a pure in-process analytic model — literally GeometricChannel's
    existing surface. Wrapped by `PerLinkAdapter` into BatchChannelModel."""
    interface_version: str
    plugin_id: str
    reach_m: float

    def capabilities(self) -> frozenset[str]: ...
    def begin_step(self, frame: StepFrame) -> None: ...
    def evaluate(self, tx: StationSnapshot, rx: StationSnapshot,
                 d_m: float, txn: Transmission,
                 ) -> Optional[LinkOutcome]:                     # None == not delivered
        ...
    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float: ...
    def collision_loss(self, dist_m: float, cbr: float) -> float: ...
```

**Loss composition becomes declared, not forked.** Today the choice between the additive sum
`loss = packet_loss_base + nlos_loss*(d/rr) + cong + wx_loss` (run.py:3392, which *can exceed 1.0*)
and the independent-survival product `p_surv = Π(1-pᵢ)` (run.py:3383-3389) is a hard
`if geo_chan is not None`. Under the ABI the model declares
`loss_composition ∈ {"additive_legacy", "independent_survival"}`. **`additive_legacy` is closed to
third parties** and exists only so `disc`/`logdistance` keep digest `0bd93655…`. This is ns-3's
`PropagationLossModel::SetNext` discipline, and we carry over its stated precondition verbatim:
*chaining is only commutative if each model's loss is independent of transmit power.*

**Split condition from loss (ns-3's second good idea).** `GeometricChannel` fuses LOS/NLOSv/NLOSb
determination with the path-loss formula. ns-3 keeps `ChannelConditionModel::GetChannelCondition`
separate from `DoCalcRxPower`, which is why any condition determination pairs with any loss
formula, and exposes the re-draw cadence as a declared `UpdatePeriod` attribute. This repo has the
identical invariant as a private guard (`if st["step"] == self.step: return cached`). Phase 4
promotes it to a declared `condition_update_period` attribute and lets
`ChannelConditionModel` be a separate registry slot. Not phase 1 — it is a refactor of a built-in,
not a blocker for third parties.

### 2.2 `Detector` — the thresholding example

**Two layers, copied from F2MD's decomposition, with two deliberate deviations.**

F2MD splits *checks* (`mdChecks/{Legacy,CaTCh,Experi}Checks` producing a `BsmCheck` — a flat struct
of 18 named doubles) from *applications* (`mdApplications/MDApplication.h`, one pure virtual
`bool CheckNodeForReport(pseudonym, bsm, bsmCheck, nodeTable)`, with `ThresholdApp`,
`BehavioralApp`, `MachineLearningApp` as peer subclasses). That split is exactly right and this
repo welds both together at run.py:3530-3538. We copy it. The two deviations:

- **Polarity is inverted and must be stated in the contract.** F2MD factors are `[0,1]` where
  **LOW = implausible** (`ThresholdApp` fires on `getX() <= Threshold`). This repo's `detnorm` is
  **`>= 1.0` = violating** (run.py:3530-3531). Any check ported from F2MD must be inverted. The
  contract states the polarity; the conformance suite asserts it (`D4`).
- **We do NOT copy F2MD's detector context.** `LegacyChecks.h` holds
  `std::unordered_map<LAddress::L2Type, veins::Coord>* realDynamicMap` — a pointer to the
  **ground-truth positions of all nodes**, used by `PositionPlausibilityCheck`. Handing a
  third-party detector that object voids the ORACLE firewall by construction. Our DTO is modelled
  on `SignedCam.java:22-31` instead.

```python
# scms_sim_ref/api/detect.py
from dataclasses import dataclass
from typing import Mapping, MutableMapping, Optional, Protocol, Sequence, runtime_checkable

INTERFACE_VERSION = "Detector/1.0"

@dataclass(frozen=True, slots=True)
class Observation:
    """THE FIREWALL. Frozen, slotted, MA-visible ONLY. Built at the reception site from the
    broadcast dict but WITHOUT veh / x / y / falsified / ghost / tspd / thdg. Modelled field-for-
    field on SignedCam.java:22-31, which is structurally oracle-free.

    A detector plugin CANNOT reach ground truth through this object. That converts the leakage
    firewall from a name-based post-hoc lint (leakage_linter.py, never called from run.py) into a
    boundary third-party code cannot walk around."""
    # sender, as the MA sees it
    cert_digest: str              # HashedId8 hex; NOT rotation-stable, never a state key
    station_type: Optional[str]   # "vehicle" | "vru" (opt-in) -> ETSI StationType under a codec
    claimed_x: float; claimed_y: float
    claimed_speed: float; claimed_heading: float
    pos_conf: float
    gen_time: float
    msg_count: int
    msg_type: str                 # "cam" | "denm"
    event_type: Optional[str]     # DENM cause-code name
    sig_ok: bool
    cert_valid_from: float; cert_valid_to: float
    # receiver-side, measured
    rx_x: float; rx_y: float
    rx_reach_m: float             # the channel's declared reach_m (fixes run.py:3469)
    rssi_dbm: Optional[float]     # channel-derived, receiver-measurable, legitimately MA-visible
    link_state: Optional[str]
    t: float; dt: float
    # aggregate MA-visible context (never per-vehicle truth)
    neighbourhood: Mapping[str, float]   # e.g. {"cell_cert_count": 4.0, "cbr": 0.31}

@runtime_checkable
class Check(Protocol):
    """LAYER 1: scores one observation. The user's threshold detector is one of these."""
    interface_version: str
    plugin_id: str
    reason_code: str              # emitted as detnorm_x_<plugin_id>_<reason_code>
    soft: bool                    # True => scored and emitted, never fires (cf. SOFT_KEYS)
    precision: int                # decimals the engine rounds to BEFORE the >= 1.0 compare

    def config_fields(self) -> Mapping[str, "FieldSpec"]: ...
    def capabilities(self) -> frozenset[str]: ...

    def evaluate(self, obs: Observation,
                 state: MutableMapping[str, object],   # namespaced: st["plugin:<id>"] only
                 params: Mapping[str, float],
                 rng: "RngNamespace") -> float:
        """Return a detnorm: >= 1.0 == VIOLATING (opposite of F2MD's [0,1] LOW==implausible).
        MUST be a pure function of (obs, state, params) plus rng streams. MUST NOT import or
        touch the global rng. MUST NOT write outside state["plugin:<id>"] — the reserved keys
        'h', 'streak', 'touch', 'kf' belong to the built-ins and are passed as a read-only proxy."""

@dataclass(frozen=True, slots=True)
class ReportDecision:
    fire: bool
    reason_codes: Sequence[str]   # ordered, most-severe first
    top_score: float
    score_norm: float

@runtime_checkable
class Fusion(Protocol):
    """LAYER 2: turns the score vector into a report decision. F2MD's MDApplication, with its
    single pure virtual CheckNodeForReport. The built-in default (`streak_v1`) reproduces
    run.py:3530-3538 exactly: streak >= detector_min_consec, then a report_prob Bernoulli."""
    interface_version: str
    plugin_id: str

    def decide(self, scores: Mapping[str, float],
               state: MutableMapping[str, object],
               obs: Observation,
               params: Mapping[str, float],
               rng: "RngNamespace") -> Optional[ReportDecision]: ...
```

**Report *format* is a third, separate seam.** F2MD gets this right too: `mdReport/` has
`MDReport` + `BasicCheckReport` / `EvidenceReport` / `OneMessageReport` / `ProtocolReport` selected
independently of the detector. That is the correct home for an honest ETSI TS 103 759 profile
(§6.3), and the natural place to fix the two shape bugs the audits flag:
`detector_outputs` carrying only `reasons[0]` (run.py:2626-2627) and `cert_validity` hardcoded
all-`True` (run.py:2628) even for an `InvalidSignature` subject.

**A/B harness, cheap and worth it.** F2MD runs two independent pipelines (V1/V2) with independently
selected check suites and fusion over the same trace. Copy it: `detectors_b: [...]` running in
parallel, emitting `detnorm_b_*` under a gate. That is what makes a detector plugin system
*scientifically* useful ("evaluate my detector against the built-in suite") rather than merely
extensible.

### 2.3 Out-of-process backends — ns-3 / OMNeT++ over a socket or file

**This is the load-bearing requirement behind "pick any network system", and it is why D1 exists.**
We cannot ship such a backend (WSL disabled, no in-tree ns-3), but a third party must be able to
write one against a published contract.

**Why per-step batching is not a style preference.** Take a mid-size run: ~200 receivers × ~30
candidates ≈ 6 000 link decisions per step, ~3 600 steps ⇒ ~2.2 × 10⁷ link decisions. At a
conservative 2 ms local-socket round trip:

| Granularity | Round trips | Wall clock at 2 ms/RTT |
|---|---|---|
| per link | 2.2 × 10⁷ | **~12 hours** |
| **per step (D1)** | 3 600 | **~7 seconds** |

Per-link IPC is ~10⁴× too slow. The ABI must be per-step, and it must be per-step *from the start*,
because retrofitting batching later means re-deriving every pinned digest.

**Synchronisation: strict lockstep, engine-blocking, no wall clock anywhere.**

```
engine                                   backend (ns-3 / OMNeT++ / anything)
  |-- HELLO {api, interface_version, seed, dt, capabilities_requested} -->|
  |<-- HELLO_ACK {plugin_id, version, capabilities, reach_m, streams[]} --|
  |-- CONFIGURE {params, station registry, buildings/refdata by hash} --->|
  |<-- CONFIGURE_ACK {config_hash} --------------------------------------|
  for step in 0..N-1:
    |-- STEP {step, t, dt, stations[], transmissions[], candidates[]} --->|   engine BLOCKS
    |<-- OUTCOMES {step, links[{tx_index, rx_vid, rssi, state, delay}]} --|
  |-- FINISH ------------------------------------------------------------>|
  |<-- SUMMARY {stats, exchange_sha256} ---------------------------------|
```

Transport: length-prefixed newline-delimited JSON (or MessagePack) over the child's stdin/stdout,
or a loopback TCP socket. **The step number is echoed in every reply and asserted** — a
desynchronised backend fails fast rather than silently shifting the dataset. The engine never reads
a clock, never uses a timeout as control flow (a timeout is a hard error, never a "assume no
delivery"), and re-sorts `OUTCOMES` by `(rx_vid, tx_index)` so the backend's internal ordering is
structurally irrelevant.

**Determinism: two modes, and only one of them is reproducible.**

| Mode | `capabilities` | Reproducible? | Manifest records |
|---|---|---|---|
| `live` | `out_of_process` | **No, and the manifest must say so.** Byte-determinism is forfeit the moment output crosses a process boundary into a runtime we do not control. | backend id/version/argv, `config_hash`, `exchange_sha256`, and `reproducible: false` |
| `replay` | `out_of_process`, `frozen` | **Yes.** The `STEP`/`OUTCOMES` exchange from a prior `live` run is captured to a file; `replay` reads it and asserts each step's hash. | the exchange file's `sha256` as a first-class **data-digest input** |

This is the same manoeuvre `docs/realism/PHASE3-PROBE.md` already recommends for a SUMO mobility
backend (frozen trajectories), and it is the honest general answer: **an out-of-process plugin is
reproducible only if its output is captured and hashed.** MOSAIC's own out-of-process federates
(`deploy`/`start`/`dockerImage`/`port` in `runtime.json:94-135`) are the right escape hatch for a
heavy or foreign-runtime plugin and have exactly this property.

**Latency becomes expressible for the first time.** Reception today is same-step and instantaneous
(`detection_time == generation_time == t`, run.py:2620-2622). `LinkOutcome.delay_s` gives an
out-of-process backend somewhere to put propagation and queueing delay. Consuming it requires a
deferred-delivery queue in the receive loop — gated, off by default, phase 5.

### 2.4 `MobilityEngine`

Modelled on ms-van3t's `VDP` (`src/automotive/model/Facilities/vdp.h`, 12 pure virtuals decoupling
the vehicle-data source from everything else), whose `vdpGPSTraceClient` implementation is the
shipped precedent for the determinism-safe frozen-trace variant.

```python
# scms_sim_ref/api/mobility.py
INTERFACE_VERSION = "MobilityEngine/1.0"

@dataclass(frozen=True, slots=True)
class MotionUpdate:
    vid: int
    x: float; y: float
    speed: float                   # m/s
    heading: float                 # deg CCW from East (engine convention; run.py:3798-3800)
    s_pos: float                   # distance along the current edge
    lane_off: float
    finished: bool

@runtime_checkable
class MobilityEngine(Protocol):
    interface_version: str
    plugin_id: str

    def capabilities(self) -> frozenset[str]:
        """{"spawn_plan", "live_spawn", "frozen_trace", "lane_changes", "traffic_lights"}"""

    def plan(self, seed: int, cfg_view: Mapping[str, object],
             net: "RoadNetwork") -> Sequence["SpawnRecord"]:
        """Called ONCE before step 0. The engine constructs the entire fleet before the loop
        (run.py flow-thinning / fixed-fleet paths), so a live engine that creates vehicles mid-run
        must declare `live_spawn`; without it, plan() is the only creation point."""

    def begin_step(self, step: int, t: float, dt: float) -> None: ...
    def advance(self, step: int, t: float, dt: float) -> Iterable[MotionUpdate]:
        """Ordered by vid. The engine writes cur_x/cur_y/cur_v/cur_h/s_pos/lane_off."""
    def trip_for(self, vid: int) -> "Trip":
        """The existing narrow 9-member Trip protocol (roads.py:23-100): length, t1, speed, caps,
        at_distance, state, next_node, next_turn, cap_at."""
    def close(self) -> None: ...
```

`frozen_trace` is the recommended shape for SUMO/CARLA: an external tool produces a trajectory
file, the engine reads it, and the **file's sha256 goes into the manifest** so external
nondeterminism becomes a detectable *input hash change* rather than an undetectable output change.

### 2.5 `MessageCodec` — the standards-profile seam

`STANDARDS-AUDIT.md` is right that the honest path to "standards-compliant" is to make the message
layer a plugin boundary: the internal simulation keeps its private representation, and the wire
format becomes swappable and testable against the real ASN.1 modules.

```python
# scms_sim_ref/api/codec.py
INTERFACE_VERSION = "MessageCodec/1.0"

@runtime_checkable
class MessageCodec(Protocol):
    interface_version: str
    plugin_id: str
    profile_id: str        # "native_v1" (default) | "etsi_cam_en302637_2" | "etsi_cam_ts103900" | ...

    def standards_claim(self) -> Mapping[str, str]:
        """What this profile may honestly assert, verbatim into manifest["standards_profile"].
        MUST name the ASN.1 module + tag when it encodes anything (see §6.5)."""

    def conventions(self) -> Mapping[str, str]:
        """Units and frames. The engine is deg CCW from East / local metres / m/s / float seconds
        (run.py:3798-3800). ETSI is 0.1 deg CW from North / WGS84 1/10 microdeg / 0.01 m/s /
        generationDeltaTime = ms mod 65536. A standards profile is therefore ALSO a unit-conversion
        layer, and this method is what makes that explicit rather than implicit."""

    def encode_cam(self, claim: "Claim", station: "StationView") -> bytes: ...
    def decode_cam(self, blob: bytes) -> "Claim": ...
    def wire_size_bytes(self, claim: "Claim", signer: str) -> int:
        """Feeds airtime -> CBR -> DCC. `signer in {"digest", "certificate"}` because TS 103 097
        signer alternation is worth ~150-200 B and both engines currently hard-code 300 B
        (SignedCam.java:18, Dcc.java:81), making every CBR estimate systematically wrong."""
    def evidence_pdu(self, claim: "Claim") -> bytes:
        """The bytes that go into a TS 103 759 v2xPduEvidence entry. See §6.3 — without this,
        no report is a report."""
```

Siblings, deliberately *not* folded into the codec (different lifetimes, different plugin authors):
**`GenerationRule`** (`should_emit(state, last_sent, t, dcc_floor) -> bool`: EN 302 637-2 /
TS 103 900 triggering, TS 103 300-3 VAM, J2945/1 10 Hz, `fixed_step` as today) and
**`CongestionControl`** (TS 102 687 reactive — `Dcc.java` already implements it competently on the
Java side and is a straight port; adaptive LIMERIC; `none` as today).

---

## 3. Registration and loading

### 3.1 The mechanism, and why

**Chosen: an explicit, ordered, config-declared reference resolved through three tiers, with
entry points used only as a *catalogue*.**

The four candidate mechanisms answer different questions, and only one is compatible with the
manifest replay contract:

| Mechanism | Discoverability | Installability | Type safety | **Reproducibility** | Verdict |
|---|---|---|---|---|---|
| `importlib.metadata` entry points | best | best | none | **worst — disqualifying for activation** | catalogue only |
| decorator registry `@register("geometric")` | good in-tree | n/a | none | good if snapshotted `tuple(sorted(...))` | built-ins only |
| `ABC` / `Protocol` | n/a | n/a | partial (neither checks signatures) | n/a | the *shape*, not the *selection* |
| **dotted path in config `"pkg.mod:Class"`** | none | broadest | none | **best by construction** | **activation** |

Entry points are disqualified as an *activation* mechanism, twice over. The active set becomes a
function of machine state (what is on `sys.path`), and iteration order follows distribution
discovery order, not sort order — pluggy documents this failure mode explicitly, which is why it
ships `tryfirst`/`trylast`. In this repo that is fatal: `DET_KEYS` is an **ordered tuple**
(run.py:2579) whose order fixes `detnorm_*` JSON key insertion order and therefore `data_digest`,
and the tie-break `sorted(fired, key=lambda k: -det[k])` relies on Python's stable sort over that
fixed order. **The Java side already hit and fixed this exact bug class** — `CamDetector.java:235-236`
uses a `LinkedHashMap` with an explicit comment. Entry points remain genuinely useful for
`--list-plugins` and the GUI dropdown, and for mapping a short name to a dotted path; always
materialised as `sorted(eps, key=lambda e: e.name)`.

Dotted-path-in-config is best *by construction*: the string lives in `cfg`, so it flows
automatically into `manifest["config"]` (run.py:3793) and back through `config_from_dict`
(run.py:1900) with **no new plumbing**. Its one weakness — a dotted path is a *mutable name*, and
the same string resolves to different bytes on different machines — is closed by the content hash
in §4.2. This is the same mechanism as `road_network="custom"` + `custom_network`, the repo's own
working precedent.

Shape borrowed from **Artery**, the cleanest declarative registry of the five prior-art systems.
Artery's `Middleware` instantiates service modules dynamically at `initialize()` from
`scenarios/artery/services.xml`:

```xml
<service type="artery.application.CaService">
  <listener port="2001"/>
  <filters><type pattern="artery.inet.Person" match="inverse"/></filters>
</service>
```

A type name plus params plus **filters**, in a config document, with zero core edits. The filters
(`<penetration rate="0.1">`, `<name pattern="flow0\.1.*">`) give per-vehicle partial deployment with
no code — a capability this repo has no equivalent of and should copy verbatim, because
partial-penetration studies are a standard V2X experiment.

### 3.2 The resolver

```python
# scms_sim_ref/api/registry.py
_BUILTINS: dict[str, dict[str, type]] = {"channel_model": {...}, "check": {...}, ...}

def resolve(slot: str, ref: str) -> tuple[type, ProvenanceRecord]:
    if ref in _BUILTINS[slot]:                    # 1. built-in registry (sorted tuple snapshot)
        obj, how = _BUILTINS[slot][ref], "builtin"
    elif (ep := _entry_point(slot, ref)) is not None:   # 2. installed catalogue, sorted by name
        obj, how = ep.load(), "entry_point"
    elif ":" in ref:                              # 3. dotted path "pkg.mod:Class"
        obj, how = _import_ref(ref), "dotted_path"
    else:
        raise ConfigError(f"unknown {slot}: {ref!r}; known: {sorted(_BUILTINS[slot])}")
    _check_interface_version(obj, slot)           # engine major == plugin major, minor <= MAX_MINOR
    _check_signature(obj, INTERFACE[slot])        # neither ABC nor Protocol does this at runtime
    return obj, _provenance(obj, ref, how)
```

Resolution happens **once, before step 0**, and every failure is fatal there. A plugin that raises
mid-loop produces a partial dataset whose digest matches nothing — note the deliberate SIGINT path
(run.py:~1933-1937) finalises a *valid* manifest, and a plugin error must **not** take that path.

### 3.3 One new config field

```python
# PipelineConfig, added at the end of the dataclass so field order is stable
plugins: dict = dataclasses.field(default_factory=dict)   # DEFAULT EMPTY -> all goldens hold
```

```jsonc
"plugins": {
  "channel_model": {"ref": "geometric", "params": {}},
  "checks": [                                    // ARRAY: order is digest-bearing, so it is an
    {"ref": "myorg.det:ProximityThreshold",      // explicit, replayable INPUT, never discovered
     "params": {"max_plausible_speed_mps": 40.0},
     "filters": {"penetration": 1.0}},
    {"ref": "myorg.det:RangeThreshold", "params": {}}
  ],
  "fusion": {"ref": "streak_v1", "params": {}},
  "message_codec": {"ref": "native_v1", "params": {}}
}
```

Empty default ⇒ zero behaviour change ⇒ every pinned digest holds. That is not a happy accident; it
is the requirement that dictated the shape.

### 3.4 How a plugin declares its own config fields

The insight from ns-3, mapped onto machinery this repo **already has**. ns-3's
`GetTypeId().AddAttribute(name, help, initial value, accessor, checker)` makes every plugin
parameter automatically documented, CLI-settable and serialisable. `config_schema()` (run.py:1869)
already emits exactly that metadata for all 138 fields. **The only change needed is to merge
plugin-declared descriptors with `dataclasses.fields(PipelineConfig)`.**

```python
@dataclass(frozen=True, slots=True)
class FieldSpec:                       # deliberately the exact shape config_schema() already emits
    type: str; default: object; help: str
    lo: float | None = None; hi: float | None = None
    step: float | None = None; unit: str | None = None
    options: list[str] | None = None; group: str | None = None

class ProximityThreshold:
    plugin_id = "proximity_threshold"
    reason_code = "proximityPlausibility"
    def config_fields(self):
        return {"max_plausible_speed_mps": FieldSpec("float", 40.0,
                   "speed above which a claim is implausible", lo=1.0, hi=120.0, unit="m/s")}
    def validate(self, params): ...     # own validator; no ifs added to the 200-line block
```

Four small changes make that flow everywhere, and none is deep:

| Consumer | Change | Result |
|---|---|---|
| `config_schema()` (run.py:1869) | merge a **sorted** list of plugin `FieldSpec`s under keys `plugins.<id>.<field>` | GUI advanced panel, copilot cheat-sheet and `--dump-config-schema` get the plugin's knobs **for free** |
| `validate_config` (run.py:~1408-1612) | after its own checks, before the `_PROB_FIELDS` clamp, call each plugin's `validate(params)` | fail-fast at config time with the plugin's own message |
| CLI (run.py:~3824-3990) | generate `--plugin.<id>.<field>` flags from the merged schema | avoids collision with the 138 flat flags; deletes the argparse/threading drift class the test suite currently polices |
| `config_from_dict` (run.py:1900) | **stop silently dropping** the `plugins` key; route each section to its declaring plugin and **fail** when absent or version-mismatched | closes the replay hole |

This collapses the audits' documented **4-to-6 hand-maintained declarations per knob** (dataclass
field + `validate_config` branch + `_FIELD_META` entry + argparse flag + its threading into the
single 65-line `PipelineConfig(...)` call + GUI `CONFIG_SPEC`) down to **one**, for plugin knobs.
It also, as a side effect, makes the ~20 hard-coded magic numbers the detector audit lists
(run.py:2566-2576, 3452, 3475, 3486-3488) reachable without a code edit, once the built-ins move
onto the interface.

---

## 4. Determinism under plugins

### 4.1 Randomness: capability by omission

**A plugin never receives `rng`** (run.py:1952 — the single `random.Random(cfg.seed)` shared by
packet loss at 3393, `report_prob` at 3536, collusion at 3566, net_delay at 2620 and emit sampling).
One extra or missing draw shifts every subsequent value in the run. There is no way to police this
after the fact; the only robust control is to not hand over the object.

```python
class RngNamespace:
    """Handed to every plugin. The plugin's ONLY source of randomness."""
    __slots__ = ("_seed", "_pid", "_cache", "_step", "_counts")

    def stream(self, label: str, *ids) -> random.Random:
        """STATELESS (default, and the only mode unless `stateful` is declared). The key INCLUDES
        the step, the object is discarded: the value is a pure function of (seed, link, step) and
        is immune to call ORDER and call COUNT. Cost: memoryless — no AR(1) correlation."""
        return random.Random(":".join(("%d" % self._seed, "plugin", self._pid, label,
                                       *map(str, ids), "s%d" % self._step)))

    def persistent(self, label: str, *ids) -> random.Random:
        """STATEFUL (capability "stateful"). Key EXCLUDES the step; the FRAMEWORK owns the cache
        and enforces advance-exactly-once-per-step — the guard GeometricChannel already uses. The
        conformance suite asserts the per-key advance count is constant across steps (check C4)."""
```

Three properties, in order of importance:

1. **The `plugin:<id>:` prefix segment is reserved**, so a third-party stream can never collide with
   a core label. That is the string-space equivalent of ns-3 partitioning MRG32k3a into 1.8×10¹⁹
   streams.
2. **String keys are stable.** CPython's `Random.seed(a, version=2)` computes
   `int.from_bytes(a.encode() + sha512(a.encode()).digest())`, so it is `PYTHONHASHSEED`-independent
   and platform-stable — unlike `hash()`. This is already the convention at ~20 sites.
3. **Do not regress to ns-3's mechanism.** ns-3's `AssignStreams` pins fixed *integer* stream
   indices to objects so that adding a random-consuming object elsewhere does not shift the
   numbers. ns-3's own manual concedes automatic index assignment "is sensitive to perturbations of
   the simulation configuration". **This repo's string-keyed construction is structurally stronger**
   — it is order-free and creation-order-independent by construction and needs no `AssignStreams`
   equivalent. Adopt only the *doctrine* (a plugin gets a reserved namespace and **declares the
   stream labels it consumes**, as a manifest field).

**Grandfathering the built-ins, stated honestly.** `disc` and `logdistance` draw the packet-loss
coin from the global `rng` (run.py:3393). That is exactly what the rule forbids. Rather than change
it (which would move `0bd93655…`), the two legacy models declare
`capabilities() ⊇ {"legacy_global_rng", "loss_composition:additive_legacy"}`, both of which the
resolver **refuses from any non-built-in**. The manifest records the grandfathered capability. This
is a deliberate, documented wart, not an oversight: it buys digest continuity, and closing it is a
scheduled item (§7 phase 6), not a hidden one.

**Add the `RngRun` analogue.** ns-3's doctrine is that independent replications come from
**advancing the run number, not changing the seed**. Add `replicate: int = 0`, folded into the
stream prefix **only when non-zero** (`f"{seed}"` if `r == 0` else `f"{seed}.{r}"`). Every existing
digest is untouched; users get cheap statistically independent replications.

**numpy is out on the engine path.** It is imported by six `datagen` modules and declared *nowhere*
(`pyproject.toml` lists only `cryptography` and `pydantic`, and `pydantic` is imported nowhere).
BLAS thread counts and float reduction order drift across hosts, and `det[k] >= 1.0` (run.py:3530)
is a **cliff** — a last-ulp difference flips a whole report. The contract mandates stdlib-only float
math on the per-link and per-report path, and the engine rounds a plugin's returned `detnorm` to the
plugin's declared `precision` **before** the comparison. If a plugin wants numpy for offline work,
the documented derivation is
`np.random.Generator(np.random.PCG64(np.random.SeedSequence(entropy=seed, spawn_key=(...,))))`.

### 4.2 Identity, version and content hash in the manifest

A new top-level `manifest["plugins"]` block. It is **not** part of `data_digest_sha256` (which by
design covers data files only — `_data_digest` at run.py:3779 explicitly excludes the manifest) and
carries its own `provenance_digest`. With no plugins declared the block is empty and every pinned
golden is untouched.

```jsonc
"plugins": {
  "api_version": "1.0",
  "interface_versions": {"ChannelModel": "1.0", "Detector": "1.0", "MessageCodec": "1.0"},
  "loaded": [
    {"slot": "checks", "order": 0,
     "ref": "myorg.det:ProximityThreshold", "resolved_via": "dotted_path",
     "distribution": "myorg-detectors", "version": "0.3.1",
     "dist_sha256": "…", "module_sha256": "…",
     "interface_version": "1.0", "capabilities": ["stateful"],
     "declared_streams": ["prox"],
     "params": {"max_plausible_speed_mps": 40.0}, "params_sha256": "…",
     "conformance": {"suite": "v1", "passed": 19, "waived": ["C9_monotone_in_distance"]}}
  ],
  "provenance_digest": "…",
  "runtime": {"python": "3.12.10", "platform": "win32-10.0.20348",
              "hash_randomization": false,
              "installed": {"cryptography": "42.0.5"}}
}
```

How each hash is computed, with its failure mode:

- **`dist_sha256`** (strongest): `importlib.metadata.distribution(name).files` yields
  `PackagePath`s each carrying `.hash` (the wheel RECORD's base64 sha256); digest over
  `sorted((str(p), p.hash.value))` — structurally the same helper as `_data_digest` (run.py:3779),
  so reuse it. **Absent for editable / `.pth` installs.**
- **`module_sha256`** (always-available fallback): sha256 of `inspect.getsourcefile(cls)`, or the
  package directory walked in sorted order. Fails for C extensions and `exec`-created classes;
  **never hash `__pycache__`** (`.pyc` embeds mtimes and paths).
- **`params_sha256`**: sha256 of the *resolved* params after defaults and validation, canonicalised
  with sorted keys — `crypto_abstract.canonical_bytes` already does exactly this.
- When identity cannot be established (a loose local file), record `null` and set
  `provenance_incomplete: true`. **Never fabricate.**

`runtime.python` is not cosmetic. Python's documented reproducibility guarantee covers **only**
`Random.random()`: *"the generator's random() method will continue to produce the same sequence when
the compatible seeder is given the same seed"* — `gauss()`, `uniform()`, `choice()`, `shuffle()`
carry **no** cross-version guarantee. The pinned goldens depend on `gauss` (the shadowing draw at
run.py:3361) and `uniform` (net_delay). **The digests are therefore pinned to a CPython version as
much as to a seed, and the manifest does not currently say so.** This is orthogonal to plugins but
is *exposed* by them: the first cross-machine plugin bug report will be an interpreter-version digest
mismatch misattributed to the plugin. Recording `sys.version` is a one-line, digest-free fix that
should land regardless of the rest of this document.

### 4.3 Detecting drift on replay

Two independent layers, and the discrimination between them is the whole value:

1. **Identity drift, detected *before* the run.** `config_from_dict(manifest, strict_plugins=True)`
   re-resolves every entry, recomputes `dist_sha256` / `module_sha256` / `params_sha256`, and raises
   `PluginDriftError(slot, id, expected, actual)`. `--allow-plugin-drift` **writes the drift into
   the new manifest**, it does not silence it.
2. **Behavioural drift, detected *after*.** The existing pinned goldens plus the two-run equality
   gate (`test_pipeline.py:77`, `test_end_to_end.py:63-64`).

| identity drift | digest drift | Diagnosis |
|---|---|---|
| no | no | clean replay |
| **yes** | no | harmless refactor of the plugin |
| no | **yes** | **the plugin is nondeterministic** (or the interpreter changed) |
| yes | yes | expected after a real plugin change |

Ship `scms-poc verify-plugins <manifest.json>` (no simulation) as a CI gate, and extend
`tools/verify_data.py`, which today validates per-file digests and the recomputed aggregate but
knows nothing about plugins.

The design pattern is **DVC's intent/lock split**: `cfg.plugins` is `dvc.yaml` (what was asked for),
`manifest["plugins"]` is `dvc.lock` (what was actually loaded, content-addressed). Two supporting
doctrines: Snakemake treats code / params / software-environment changes as first-class **rerun
triggers**, and Nextflow's rule is to **pin a container digest, never a tag** — generalised here to
*never key provenance on a mutable name* (version string, entry-point name, dotted path); always on
content. What none of them gives is byte-identical numerics; provenance is necessary but not
sufficient, and §5 supplies the sufficiency.

### 4.4 The full ordering-hazard checklist

Every one of these is already load-bearing in the engine and must be restated in the contract:

- **Registry iteration is `sorted()` before use.** Determinism already rests on `cand.sort()`
  (run.py:3325), `sorted(active)`, sorted `spawn_order`, sorted relpaths in `_data_digest`
  (run.py:3782), and sorted JSON keys in `canonical_bytes`.
- **Detector order is config-declared and manifest-recorded**, because it reaches the digest twice:
  through `DET_KEYS` → `detnorm_*` insertion order, and through the stable-sort tie-break.
- **New columns are opt-in and namespaced** as `detnorm_x_<plugin_id>_<key>` / `x_<plugin_id>_<key>`.
  The `x_` prefix is reserved so third-party keys can never collide with the standardised vocabulary
  or with a future built-in.
- **Per-`(rx, sender)` state is namespaced**: `st.setdefault(f"plugin:{pid}", {})`. The reserved
  keys `h` / `streak` / `touch` / `kf` are passed as a read-only mapping proxy.
- **Plugin-written files must be declared** as either *data* (digest-bearing, must be deterministic)
  or *side metadata* (excluded, like `live_state.json`), because `_data_digest` hashes every file in
  the `data_files` map.
- **Plugin instances are per-run objects** built inside `run_pipeline`, never module globals — the
  in-process multi-run drivers would otherwise cross-contaminate.

---

## 5. The conformance suite

> **AS IMPLEMENTED (2026-08-31).** `src/scms_sim_ref/conformance/` — `v1/channel.py` (the contract),
> `v1/harness.py` (scenarios, `DrawCounter`, `audit_guard`), `runner.py` (the pytest-free driver and
> `ConformanceReport`). Fourteen rows for the thirteen numbered checks, because C6 has two arms —
> phase 2 shipped C1-C12, and the adversarial review added **C13_config_not_mutated** (the model
> does not write to the run's `PipelineConfig`; three arms — a write at construction, a write during
> a 12-step run, and a before/after snapshot that does not care how the write was performed).
> `tests/test_conformance.py` grades the SUITE, with one deliberate violator per check, each of
> which must fail its own check and (for C6b and both C13 violators) *only* its own check.
>
> **Two deviations, both forced and both stated in the module docstring.**
> **(1)** C6b is `C6b_oracle_invariance`, not `rssi_tracks_true_geometry_not_claimed`. The memo's
> verbatim port cannot be written against this ABI: `StationSnapshot` carries **no claimed position**
> — it is oracle-side by necessity and carries true geometry only — so "did the model use the claimed
> position" is not a question the interface can pose. What ships is the strictly stronger statement
> it *can* pose: two frame sequences identical in every DECLARED field, one additionally carrying
> ground truth (`is_attacker` / `falsified` / `true_x` on the station objects and in `frame.env`),
> must produce identical traces. It catches a laundered claimed position, a laundered attacker flag,
> or anything else. The memo's dataset-level correlation test is untouched and still runs in
> `tests/test_geometric_channel.py`.
> **(2)** C10's first arm (an undeclared parameter name) is enforced by the framework and therefore
> passes for any plugin; the report says so in its detail line rather than hiding it. The second arm
> — a value outside a bound the plugin ITSELF declared through `FieldSpec` — is the plugin's own
> declaration doing the work, and runs only when the plugin declares a bounded field.
>
> Waivers are implemented on BOTH sides of Django's doctrine: a contract subclass may set
> `waivers = {check_id: justification}`, and an implementation may ship
> `conformance_waivers = {...}` as a class attribute (this is `django_test_skips`). A waiver whose
> justification is empty is refused. `LogDistanceChannel` is the first user of it — see
> `PLUGIN-CONFORMANCE-EVIDENCE.md` §3.
>
> The third delivery route ships as a CONFIG DECLARATION, not the memo's `--unconformant` flag:
> `plugins.channel_model.conformance = "required"` makes `build_channel` run the suite before step 0
> and refuse a failing plugin, embedding the summary at
> `manifest["plugins"]["loaded"][*]["conformance"]`. A config field replays; a flag does not. It is
> off by default (measured 0.111 s per run, and C5's PEP 578 hook can never be uninstalled), and
> **C12 is excluded from it BEFORE the suite runs** — C12 runs two pipelines, so running it from
> inside one nests a pipeline in a pipeline. Excluding it properly rather than filtering its row out
> afterwards is worth 0.19 s of the 0.30 s the first implementation cost. The exclusion is named in
> the embedded summary rather than hidden.

Shipped as importable base classes at `scms_sim_ref.conformance.v1`, **versioned with the
interface**, so "passed conformance" is a checkable statement about a specific contract. Delivered
three ways: subclass it in your own test suite; run `scms-poc conformance --slot check --ref
myorg.det:Threshold`; or let the engine refuse an unattested plugin unless `--unconformant`. Either
way a `conformance_report.json` summary is embedded in `manifest["plugins"][*]["conformance"]` — so
*"this dataset was produced by a conformant plugin"* becomes a machine-checkable property of the
artifact.

**Prior art, and the specific thing taken from each.** *Django*: third-party DB backends run
Django's **own** suite and declare what they legitimately cannot pass via
`DatabaseFeatures.django_test_skips` — waivers are **data supplied by the implementation**, not
edits to the suite, and each carries a written justification that lands in the report. *pytest*:
ship the harness (`pytester`), not just the docs. *This repo*: `tests/test_geometric_channel.py`
grades the implementation against the audited `datagen/refdata/*.json` rather than a second
transcription of the same formula — *"which is what makes them non-tautological"* (its own
docstring). Physics checks must be refdata-graded the same way. **Caveat on Hypothesis**: it is
randomised and keeps an example database; run any property-based portion with `derandomize=True` and
a pinned profile, or generate sequences from our own seeded `random.Random`. A nondeterministic
conformance suite in a determinism project is self-defeating.

```python
# scms_sim_ref/conformance/v1/channel.py
class ChannelModelContract:
    """Subclass in YOUR test suite; implement make(). All checks seeded, no network, no I/O.

        class TestRayleigh(ChannelModelContract):
            INTERFACE_VERSION = "ChannelModel/1.0"
            def make(self, **p): return Rayleigh(seed=7, **p)
    """
    expected_failures: frozenset[str] = frozenset()
    waivers: dict[str, str] = {}          # check_id -> justification, copied into the manifest

    # --- determinism -----------------------------------------------------------------
    def test_C1_repeatable(self):
        assert self._trace(self.make()) == self._trace(self.make())

    def test_C2_call_order_independent(self):
        """The ns-3 AssignStreams property, restated for string-keyed streams: a link's outcome
        must not depend on the ORDER in which links were evaluated."""
        a = self._trace(self.make(), order="sorted")
        b = self._trace(self.make(), order="reversed")
        assert dict(a) == dict(b), "per-link results depend on call ORDER -> draws not identity-keyed"

    def test_C3_global_rng_untouched(self):
        random.seed(999); before = random.getstate()
        g = random.Random(1); gs = g.getstate()
        self._trace(self.make(), rng=g)
        assert random.getstate() == before and g.getstate() == gs

    def test_C4_state_advances_once_per_step(self):
        """Only for capabilities() & {"stateful"}. Count internal draws per step per key;
        the count must be CONSTANT. This is GeometricChannel's step guard, asserted."""

    # --- safety ----------------------------------------------------------------------
    def test_C5_no_io(self):
        with audit_guard(deny={"open-write", "socket.connect", "subprocess.Popen", "os.system"}):
            self._trace(self.make())

    def test_C6_no_oracle_leak(self):
        for k in self._output_keys():
            assert not is_forbidden_feature_key(k)
        with pytest.raises((FrozenInstanceError, AttributeError)):
            obs.claimed_x = 0.0                       # the DTO must actually be frozen

    def test_C6b_rssi_tracks_true_geometry_not_claimed(self):
        """Verbatim port of tests/test_geometric_channel.py. This is the ONLY leak test that
        catches a plausible-LOOKING wrong implementation: a channel that keys on claimed
        position is both a physics bug and an oracle-laundering vector."""

    # --- output sanity ---------------------------------------------------------------
    def test_C7_ranges(self):
        """rssi in [-140, 0] dBm or None; link_state in the CLOSED vocabulary
        {LOS, NLOSv, NLOSb}; delay_s >= 0; reach_m finite and > 0; no NaN/inf anywhere."""

    def test_C8_reach_honesty(self):
        """No link is delivered beyond the declared reach_m. This is what keeps the
        acceptanceRangeThreshold detector correct (art_reach, run.py:3469)."""

    # --- physics ---------------------------------------------------------------------
    def test_C9_monotone_in_distance(self):
        """PDR(d) and mean rssi(d) NON-INCREASING over a fixed distance ladder, all else equal,
        within a seeded sample budget. Shape of tests/test_radio_propagation.py::
        test_pathloss_exponent_monotonicity. Waivable WITH a written justification (a model
        with a deliberate near-field or two-ray null legitimately fails it)."""

    def test_C10_fail_fast(self):
        """Invalid params raise at CONSTRUCTION, never at step k > 0."""

    def test_C11_float_hygiene(self):
        """Outputs finite and equal after round(x, DECLARED_PRECISION)."""

    # --- the acceptance test ---------------------------------------------------------
    def test_C12_pipeline_two_run_digest(self, tmp_path):
        a = run_pipeline(self._cfg(tmp_path / "a"))
        b = run_pipeline(self._cfg(tmp_path / "b"))
        assert a.data_digest == b.data_digest
```

```python
# scms_sim_ref/conformance/v1/detect.py — the additional checks for the thresholding case
class CheckContract:
    def test_D1_pure(self):
        """Same (obs, state) twice -> same score."""

    def test_D2_reads_only_ma_visible(self):
        proxy = RecordingProxy(obs)                # __getattr__ records touched attribute names
        self.make().evaluate(proxy, state, params, rng)
        assert proxy.touched <= {f.name for f in dataclasses.fields(Observation)}
        # A CAPABILITY check, not a name check — this is precisely what the existing
        # name-based linter (leakage_linter.py:25-40) provably cannot do.

    def test_D3_label_invariance(self):
        """THE ANTI-LAUNDERING TEST. Two Observation streams identical field-for-field, generated
        from runs whose GROUND TRUTH differs (attacker flags flipped, claims unchanged). A detector
        keying on the oracle changes its output; an honest one CANNOT."""
        assert scores(stream_a) == scores(stream_b)

    def test_D4_firing_convention(self):
        """>= 1.0 == violating (run.py:3530-3531), asserted at the declared rounding precision so
        the cliff is not ulp-sensitive. Catches a check ported from F2MD without inverting its
        [0,1] LOW==implausible polarity."""

    def test_D5_monotone_in_attack_magnitude(self):
        """Score non-decreasing in attack_magnitude_scale — the dial tests/test_attack_magnitude.py
        already exercises. A detector that is not monotone in the thing it detects is broken."""

    def test_D6_off_by_default_is_byte_identical(self):
        """REGISTERING IS NOT ENABLING. Importing/installing the plugin, with no config entry,
        reproduces the golden. Exact pattern of tests/test_radio_propagation.py:78-86."""
        assert _default_run().data_digest == DEFAULT_GOLDEN

    def test_D7_state_namespacing(self):
        """Writes only st["plugin:<id>"]; never h / streak / touch / kf."""
```

---

## 6. Standards plan

### 6.1 Verdict table

Versions are the current *published* ones read from ETSI's `/deliver/` index. **Note the Release 2
renumbering the repo does not yet reflect: EN 302 637-2 is superseded by TS 103 900, and
EN 302 637-3 by TS 103 831.** Citing EN 302 637-2 alone now reads as out of date.

| Standard | Current verdict | Work to a defensible claim | Priority |
|---|---|---|---|
| **ETSI TS 103 759 V2.2.1** (Misbehaviour Reporting) | **PARTIAL, field-names only.** `MaReport` is loosely TS-103-759-*shaped*. The mandatory PDU evidence does not exist: `evidence_msg_refs=[f"{rid}-m"]` (run.py:2629) is a synthetic self-reference and `MaEvidenceMessage` (records.py:77) is **never instantiated**. Reason codes are F2MD names. | (1) **Emit and retain the observed PDUs** — the record class already exists, unused. (2) Adopt the real 3-field envelope. (3) Replace F2MD reason strings with the normative `(tgtId, obsId)` pairs. (4) Decide where `score` goes (the standard has none). Encode as **JSON, structurally conformant** — not ASN.1 (§6.4). | **1 — highest** |
| **CAMP SCP2 linkage** | **COMPLIANT and enforced.** Real seed hash-chain, Davies-Meyer pre-linkage, `lv = plv1 ⊕ plv2`, forward-only matching, asserted at run.py:~3664. | Fix the backward-privacy divergence: Python revokes from period 0 (publishing `ls_x(0)`, linking the device's **entire** history); Java correctly uses `p.i`. **This is a security defect, not a standards gap.** | **2** |
| **IEEE 1609.2-2022 / ETSI TS 103 097 V2.2.1** | **ABSENT except `HashedId8`.** Zero `ca.sign(` / `ca.verify(` calls; `sig_ok` is a synthetic boolean; Java passes literal `true`. Certificates are a hex string plus a validity window. **The manifest claim `"cert": "IEEE 1609.2"` is not supportable.** | (a) **Correct the claim today** (free — §6.5). (b) Wire `ec.py` + `butterfly.py` into provisioning (both correct, both orphaned). (c) COER-encode `Ieee1609Dot2Data`/`SignedData` via pycrate's pre-compiled module. (d) **RFC 6979 deterministic ECDSA is mandatory** or the digest contract dies. | **3** |
| **ETSI EN 302 637-2 V1.4.1 / TS 103 900 V2.3.1** (CAM) | Structure **ABSENT** both engines (~8 of ~40 mandatory HF fields, wrong units, wrong frame). Generation rules **PARTIAL, Java only** (`ScmsBeaconApp.java:229-233`). Python is a rigid 1 Hz per-step loop; the 4 m / 4° / 0.5 m/s constants sit in `refdata/etsi_cam_dcc.json` with **no Python consumer**. | `MessageCodec` + `GenerationRule` plugins. **Real UPER is available now** (§6.4). Note the triggering rule changes the *number of broadcasts*, hence every downstream global-`rng` draw — it must be gated and its own goldens pinned. | **4** |
| **ETSI TS 102 687 V1.2.1** (DCC) | **PARTIAL, Java only, opt-in.** `Dcc.java` is a genuinely competent reactive implementation. **ABSENT in the Python flagship.** | Port `Dcc.java` to Python behind a `CongestionControl` plugin, replicating the drift-free probe boundaries exactly. Warning: DCC is a **feedback loop** (emissions → CBR → rate → emissions); digest stability then depends on the CBR estimator's accumulation order. | 5 |
| **ETSI TS 102 941** (Trust & Privacy) | **ABSENT.** No EA/AA split, no EC/AT distinction, no enrolment or authorization protocol. `ADR 0001:27` promises it "as a config variant"; no such field exists. | A `PkiBackend` plugin with `us_scms` (current) and `etsi_ts102941` profiles. This is also what finally makes `butterfly.py` the live provisioning path. | 6 |
| **ETSI EN 302 637-3 / TS 103 831** (DENM) | **ABSENT.** Two cause-code *names*, no numeric `causeCode`/`subCauseCode`, no `actionID`, no termination, no repetition, no `validityDuration`. | Follows the CAM codec work; low marginal cost once `MessageCodec` exists. | 7 |
| **ETSI TS 103 300-3** (VAM) | **ABSENT.** VRUs emit the same CAM dict with `station_type="vru"`. The comments *"they broadcast VAMs"* are aspirational and **should be corrected now**. | VAM profile under `MessageCodec`; UPER available. | 8 |
| **SAE J2735 / J2945/1** | **ABSENT.** Two comments only. | Deprioritise: **J2735's ASN.1 is not free** (sold as `J2735ASN-2024`), while every ETSI module is BSD-3-Clause on Forge. ETSI-first is both cheaper and better-tooled. Flag **SAE J3287**: TS 103 759 *imports its BSM report module* (`SaeJ3287AsrBsm.asn`, PSID 32), so the two are formally interlocked. | 9 |

### 6.2 ASN.1 UPER versus structural conformance — the decision

**Decision: both, tiered — and the tier boundary is set by what the ASN.1 language requires, not by
appetite.** This corrects the audit in two places.

Empirically tested on this host (Python 3.12.10):

| Module set | `asn1tools` 0.167.0 (MIT) | Verdict |
|---|---|---|
| CAM R1 (EN 302 637-2 v1.4.1) + ITS-Container | **PASS**, 153 types | Tier A |
| DENM R1 (EN 302 637-3 v1.3.1) | **PASS**, 146 types | Tier A |
| CAM R2 (TS 103 900 v2.3.1) + ETSI-ITS-CDD v2.5.1 | **PASS**, 394 types | Tier A |
| DENM R2 (TS 103 831 v2.3.1) | **PASS**, 380 types | Tier A |
| VAM (TS 103 300-3 v2.3.1) | **PASS**, 377 types | Tier A |
| IEEE 1609.2 | **FAIL** — `IEEE1609DOT2-HEADERINFO-CONTRIBUTED-EXTENSION.&Extn not found` | Tier B, via pycrate |
| TS 103 097 | **FAIL** — same | Tier B, via pycrate |
| TS 103 759 (MRS) | **FAIL** — `TypeError: string indices must be integers` | **Tier C** |

The split is structural, not incidental: `asn1tools` does not implement X.681/X.683 (information
object classes, parameterized types, table constraints). **The facilities layer does not need them;
the security layer and TS 103 759 are built entirely on them** —
`C-ASR ::= CLASS {&aid Psid UNIQUE, &Content}`, `TemplateAsr{...}`,
`C-ASR.&Content({SetAsr}{@.aid})`.

Live results worth pinning to: a real CAM UPER-encodes to **41 bytes**, round-trips, and is
**byte-identical across three fresh interpreter processes**. A real `Ieee1609Dot2Data`/`signedData`
(psid=36, `signer=digest`, `ecdsaNistP256Signature`) encodes to **100 bytes of COER** via pycrate's
**pre-compiled** `pycrate_asn1dir.ITS_IEEE1609_2` (compiling the Forge 1609.2 source with pycrate
fails twice; the shipped module works, but lacks `CertIssueExtension` — it is the pre-2022 edition).
**Strongest single result: `asn1tools` and `pycrate` produced byte-identical UPER for the same CAM,
and each decoded the other's bytes.** UPER encoding is deterministic and therefore compatible with
the pinned-digest contract.

**Therefore:**

- **Tier A — CAM / DENM / VAM: real UPER, now.** Blocked by nothing. Claiming only "documented
  structural conformance" when real UPER costs one `pip install` would *understate* what this
  project can do. **This overrides the audit's implicit framing of "UPER or prose".**
- **Tier B — 1609.2 / TS 103 097 envelope: real COER, now,** accepting LGPL-2.1+ for pycrate.
- **Tier C — TS 103 759: structural conformance only.** Neither pure-Python library compiles it,
  and encoding it would need a C toolchain or a commercial compiler. Structural is the honest
  ceiling — and it is *far* stronger than today's position, because the vocabulary is now known.

Two Windows packaging facts found by *testing*, not by reading docs, that must be budgeted for:
`pip install pycrate` **fails** with `OSError [Errno 2]` into deep paths (its CSN.1 filenames exceed
MAX_PATH without long-path support) and installs fine from a short root; and **`asn1tools` is not
pure Python** — it pulls `bitstruct`, which ships a C extension (a prebuilt `cp312-win_amd64` wheel
exists, so it installs cleanly, but the audit's "asn1tools is pure Python" is imprecise).

Every codec is gated behind an opt-in config field defaulting to `native_v1`, following the
`_emit_station_type` / `_denm_enabled` pattern, with the codec name **and the ASN.1 module tag**
recorded in the manifest.

### 6.3 ETSI TS 103 759, correctly

The real structure, verbatim from `EtsiTs103759Core.asn` at tag `v2.2.1`:

```asn1
EtsiTs103759Data ::= SEQUENCE { version Uint8(3), content EtsiTs103759MbrSec }
EtsiTs103759MbrSec ::= CHOICE { plaintext EtsiTs103759Mbr,
                                signed    EtsiTs103759Mbr-Signed,
                                sTE       EtsiTs103759Mbr-STE, ... }
EtsiTs103759Mbr ::= SEQUENCE { generationTime      Time64,
                               observationLocation ThreeDLocation,
                               report              AidSpecificReport }
```

and from `EtsiTs103759BaseTypes.asn`:

```asn1
TemplateAsr{...} ::= SEQUENCE {
  observations      ObservationsByTargetSequence{{ObservationSet}},
  v2xPduEvidence    SEQUENCE (SIZE(1..MAX)) OF V2xPduStream,
  nonV2xPduEvidence NonV2xPduEvidenceItemSequence{{NonV2xPduEvidenceSet}} }
```

**The whole report is three fields.** It is far simpler than the audit assumes — and
`v2xPduEvidence` is `SIZE(1..MAX)`: **mandatory, minimum one.** So the audit's *substantive*
conclusion survives its naming error, and is if anything sharper: **a TS 103 759 report is
structurally impossible without carrying the actual observed PDUs.** `evidence_msg_refs=[f"{rid}-m"]`
cannot satisfy this under any encoding. This remains the single highest-value gap in the repo, and
the record class for it already exists, unused.

**The detector taxonomy is binary observations grouped by target property**, keyed by the pair
`(tgtId, obsId)` — obsIds are unique only *within* a target class, so a bare integer is ambiguous.
CAM target classes: `BeaconCommon=0`, `StaticCommon=1`, `SecurityCommon=2`, `PositionCommon=3`,
`SpeedCommon=4`, `LongAccCommon=5`. There are 33 observation constants plus 18 DENM-side ones — 55
normative identifiers in total. **Most observation types are `::= NULL`: there is no score,
confidence or severity field anywhere in TS 103 759.** That is a direct semantic mismatch with
`detector_outputs[{check_id, score, verdict}]` (`records.py:104`) and the ~13 `detnorm_*` columns,
and it must be resolved *deliberately* — either `score` is a private extension under the `...`
markers, or it is an ML-side artifact kept outside the report proper. It must not be silently
emitted inside a container labelled TS 103 759.

Mapping the repo's 15 detectors onto the normative vocabulary — roughly **6 of 15** have
counterparts:

| Repo detector | Normative `(tgtId, obsId)` |
|---|---|
| `beaconFrequency` | (0, 1) `c-ObsBeacon-IntervalTooSmall` |
| `positionSpeedInconsistency`, `positionJump` | (3, 4) `c-ObsPosition-ChangeTooLarge` |
| `implausibleAcceleration` | (5, 4) `c-ObsLongAcc-ValueTooLarge` / (4, 5) `c-ObsSpeed-ChangeTooLarge` |
| `certValidity` | (2, 5) `c-ObsSecurity-HeaderTimeOutsideCertificateValidity` |
| *(not implemented)* | (4, 3) speed-by-`stationType` — trivial to add, and normatively specified |
| `acceptanceRangeThreshold`, `sybilCoLocation`, `mapOffRoad`, `constantPositionFrozen`, `staleOrReplay`, `headingInconsistency`, `kalmanConsistency`, `vruImpersonation`, `signatureVerification` | **none** — declare as private extensions under `...`, never pass off as standard |

`signatureVerification` has no counterpart *by design*: TS 103 759 assumes signature verification
**precedes** reporting; its `SecurityCommon` observations are semantic inconsistencies, not crypto
failures.

**Normative thresholds, citable, to land in `datagen/refdata/` alongside `etsi_cam_dcc.json`** (from
`EtsiTs103759AsrCam.asn`): beacon interval `< 80 %` of the TS 103 900 value, with the
`generationDeltaTime` wrap rule (add 65 536 ms); speed by `stationType` — `passengerCar(5)`
> 14 000 cm/s, `motorcycle(4)`/`bus(6)`/`lightTruck(7)`/`heavyTruck(8)`/`trailer(9)` > 8 500,
`unknown(0)`/`pedestrian(1)`/`cyclist(2)`/`moped(3)`/`specialVehicles(10)`/`tram(11)` > 3 000,
`roadSideUnit(15)` > 0; `driveDirection = backward(1)` with speed > 3 000; longitudinal acceleration
> 90 dm/s² (stated rationale: µ = 0.9 → 88.2 dm/s²). **These are citable replacements for several of
the repo's invented magic numbers.**

### 6.4 How conformance gets demonstrated

1. **The two-library cross-check, in-tree, offline, free.** `asn1tools` and `pycrate` produced
   byte-identical UPER for the same CAM and each decoded the other's output. Two independent
   implementations agreeing byte-for-byte is a self-contained golden-vector mechanism requiring no
   external tooling and no vendor. **This closes the audit's "no test in the 57-file suite asserts a
   standards property" gap immediately.**
2. **Wireshark's ITS dissector** (`epan/dissectors/packet-its.c`) as an optional third-party check —
   it is auto-generated by `asn2wrs.py` **from the ETSI ASN.1 modules themselves** and dissects
   CAM/DENM/VAM/CPM/MAPEM/SPATEM/IVIM. Feeding generated bytes through `tshark` is a genuine
   independent validation.
3. **Mirror test purposes from `forge.etsi.org/rep/ITS/ttcn/mbr_ts_103759`** — an official, public,
   actively maintained TTCN-3 conformance suite for Misbehaviour Reporting Release 2. **Running it
   is unrealistic** (the ATSs drive a live SUT over BTP/GeoNetworking/G5 through a Test Adapter, and
   a dataset generator has no SUT), but it is the authoritative definition of *conformant* and a
   ready source of test purposes.
4. **Not available and should not be implied**: ETSI Plugtests are attendance-based; there is no
   free normative CAM/DENM byte-vector set.

### 6.5 What the manifest and README may claim **today**

**Change the manifest now. It costs nothing** — `_data_digest` (run.py:3779) excludes
`manifest.json` by construction, so correcting `standards_profile` (run.py:3803) moves **zero
digests**. There is no reason to defer it behind any code work.

```jsonc
"standards_profile": {
  "linkage": "CAMP SCP2 — implemented and enforced (scms_core/linkage.py; asserted in-run)",
  "cert":    "HashedId8 identifiers per IEEE 1609.2 §6.4.3; NOT a 1609.2 certificate profile",
  "security_envelope": "none — sig_ok is a simulated boolean; no signature is computed or verified",
  "message": "native_v1 — engine-private representation; no ASN.1 encoding",
  "report":  "ETSI TS 103 759 V2.2.1: partial field-name correspondence only; not encoded, not signed, and carrying no v2xPduEvidence"
}
```

**Permitted / forbidden, per tier:**

| | MAY say | MAY NOT say |
|---|---|---|
| **Today** | "CAMP SCP2 linkage values are implemented and enforced." "Certificate identifiers are HashedId8 per IEEE 1609.2." "Reports use a TS 103 759-inspired field set." | **"cert: IEEE 1609.2"** (unsupportable — `hashed_id8` alone is not a certificate profile). "TS 103 759 compliant" or "TS 103 759 reports" **while `v2xPduEvidence` is empty** — that field is `SIZE(1..MAX)` and a report without it is not a report. "certificates profiled to IEEE 1609.2" (`DATASHEET.md:33`). "they broadcast VAMs". |
| **Tier A shipped** | "CAMs are encoded to ETSI EN 302 637-2 V1.4.1 / TS 103 900 V2.3.1 using UPER against the published ETSI ASN.1 modules (ETSI Forge, BSD-3-Clause, tag `<T>`); output is validated by an independent decoder and is byte-stable across runs." **MUST state the module version and tag.** | "conformance-tested", "certified", "Plugtests-validated", "interoperable with production ITS stations" — none follows from encoding alone. |
| **Tier B shipped** | "Secured messages are encoded as IEEE 1609.2 `Ieee1609Dot2Data`/`SignedData` in COER, profiled per ETSI TS 103 097 V2.2.1." **MUST name the ECDSA nonce scheme.** | "IEEE 1609.2 compliant" while signatures are simulated — only "1609.2-**structured**, signatures simulated" until `ca.sign`/`ca.verify` are genuinely on the path. |
| **Tier C shipped** | "Misbehaviour reports follow the ETSI TS 103 759 V2.2.1 report structure (`generationTime`, `observationLocation`, `AidSpecificReport` → `TemplateAsr{observations, v2xPduEvidence, nonV2xPduEvidence}`) and use the normative `(target, observation)` identifier pairs; reports are serialised as JSON, not ASN.1, and are not signed or encrypted." | "TS 103 759 compliant". |

Two documentation corrections that are independent of all code work: `DATASHEET.md:32-34`,
`README.md:133`, `docs/FEATURES.md:207` and `records.py:96` should distinguish **IMPLEMENTED**
(CAMP SCP2 linkage; IEEE 1609.2 HashedId8; EN 302 637-2 triggering in the MOSAIC layer; TS 102 687
reactive DCC in the MOSAIC layer, opt-in) from **INSPIRED-BY** (report field names, DENM cause-code
names, station types) from **ABSENT** (all message encodings, all security envelopes, all
certificate structures, VAM, BSM, TS 102 941).

---

## 7. Phased roadmap

Every phase has a **quantitative gate**. No phase ships without its gate green.

### Phase 0 — Free corrections (no code on the engine path)

**Work.** Correct `standards_profile` (run.py:3803) and the four documentation sites. Add
`sys.version` / `platform` / `sys.flags.hash_randomization` to the manifest. Set `PYTHONHASHSEED=0`
in `run.ps1` / `conftest` (it must be set *before* interpreter start).

**Gate.** All 8 pinned goldens unchanged — provable by construction, since the manifest is excluded
from `data_digest`. Test suite green. **Estimated: hours.**

### Phase 1 — The channel seam, in-tree only

**Start here, not at the detector seam, despite the directive's example.** `GeometricChannel`
already satisfies a plausible Protocol, already draws zero from the global `rng`, and its
byte-identity is directly provable against an existing golden. What blocks third parties is *only*
the closed enum in four places plus one `if`. The detector seam additionally requires extracting
nine inline detectors and building the `Observation` DTO — strictly more work, on strictly less
proven ground. Land the resolver + provenance + conformance machinery where it is cheapest to
verify, then apply the identical machinery to the detector seam.

**Work.** Ship `scms_sim_ref.api` (Protocols + `FieldSpec` + `RngNamespace` + resolver). Add
`plugins: dict = field(default_factory=dict)`. Refactor `disc` / `logdistance` / `geometric` onto
`BatchChannelModel` via `PerLinkAdapter`, keeping the identical call order. Replace the closed enum
with the resolver (keeping the enum values as builtin registry keys, so `--radio-model geometric`
still works). Add `manifest["plugins"]`.

**Gate.**
- **`0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740` unchanged**, plus
  `GOLDEN_COLLUSION_RSU_LOGDIST` (`939b4faa…`) and `GOLDEN_VRU_DENM` (`b3a01d40…`).
- `MULTI_ATTACK_GOLDEN` (`48013901…`) unchanged.
- All 8 goldens across 6 test files unchanged.
- Adding a plugin entry with `"channel_model": {"ref": "geometric"}` produces a digest **identical**
  to `--radio-model geometric`.
- `config_schema()` returns the same 138 top-level fields plus exactly one new key (`plugins`).

### Phase 2 — Conformance suite + provenance lock — **IMPLEMENTED 2026-08-31**

**Work.** `scms_sim_ref.conformance.v1` (C1–C12; C13 added by the adversarial review).
`strict_plugins` in `config_from_dict`.
`scms-poc verify-plugins`. `scms-poc conformance`. Extend `tools/verify_data.py`.

**Gate — the headline acceptance test.** Every item below was measured; the transcript, the exit
codes and the digests are in **`PLUGIN-CONFORMANCE-EVIDENCE.md`**, reproducible with one command
(`C:\Temp\scms_plugin_demo\ACCEPTANCE.ps1`).
- ✅ **A reference channel plugin living OUTSIDE the repo** (`scms-demo-channel 0.1.0`, its own
  pyproject and wheel in `C:\Temp\scms_plugin_demo`, importing only `scms_sim_ref.api` — asserted by
  a source scan in its own suite) passes all 12 checks (13 rows, exit 0; 13 checks / 14 rows since
  C13) and **reproduces byte-identical output across two runs**: `9abe9eeac07f947e…`, 10 of 11 files
  byte-identical, `manifest.json` differing only by `build_utc`. (That digest was measured with the
  frozen-RNG-step defect present; on the fixed engine the same config and plugin give
  `be3b5deaa209d078…`, still two-run identical. See `PLUGIN-CONFORMANCE-EVIDENCE.md` §7 defect 1.)
- ✅ Mutating one byte of that plugin's source — inside a docstring, so behaviour is unchanged —
  makes `verify-plugins` exit **2** and the replay exit **2** with `PluginDriftError`, **before
  step 0**: no output directory is created. `--allow-plugin-drift` then reproduces the *identical*
  digest and records the drift in the new manifest, which is the "harmless refactor" row of §4.3's
  table, produced on demand.
- ✅ A deliberately nondeterministic plugin (`time.time()`-seeded fade) **fails C1** and, separately,
  produces `e3c94d7ee242…` vs `4275ef8acdec…` from two runs that both exit 0 — under an **identical**
  `provenance_digest`, which is §4.3's "no identity drift + digest drift ⇒ nondeterministic" row.
- ✅ A deliberately leaky plugin **fails the no-leakage check** while passing C1/C7/C8/C9/C11/C12 and
  producing byte-identical datasets, so the pinned-golden layer is provably blind to it.
  *(Restated from the memo's "fails D3": D3 is a DETECTOR check and the `Observation` DTO is phase 3.
  The channel-side equivalent is C6b, which is the stronger form — see §5's deviation note.)*

**Two defects the gate found**, neither of which was known before the suite existed:
`logdistance` fails C8 and now declares a quantified waiver (14.13 % of delivered links beyond its
declared reach; 4.19 % would score `>= 1.0` on `acceptanceRangeThreshold` for an honest sender at its
true position — 0 in the default 5×5/120 m grid only because the map is smaller than
`reach + art_max_m`); and phase 1's `dist_sha256`, the design's *strongest* identity, was `null` for
**every** normally-installed wheel because the RECORD-hash walk bailed on `RECORD`'s own unhashed row.

### Phase 3 — The detector seam and the user's thresholding case

**Work.** Build the frozen `Observation` DTO. Extract the 9 inline detectors (run.py:3458-3529) into
`Check` classes. Split `Fusion` out of run.py:3530-3538. Derive `featurize.REASON_VOCAB`,
`DETECTORS` and `_DETECTOR_DOCS` from the registry. Namespace `st["plugin:<id>"]`.

**Gate.**
- All 8 goldens unchanged with `plugins.checks == []`.
- **A third-party threshold detector, in a separate distribution, adds `detnorm_x_<id>_<code>` to
  the report with 0 edits to `run.py`, `featurize.py` or any test file.**
- With that detector declared, the golden **changes**; with it installed but not declared, the
  golden **holds** (D6).
- The four-way reason-vocabulary drift is gone: `featurize.py`'s three literal lists are derived,
  and the dead `positionSpeedConsistency` alias (`featurize.py:40`) is removed by construction.
- `Observation` has no attribute whose name is in `FORBIDDEN_FEATURE_KEYS`, asserted by a test.

### Phase 4 — Out-of-process backend contract

**Work.** Publish the wire protocol (§2.3). Ship a **loopback reference backend** — a trivial Python
subprocess reimplementing `disc` over the socket — as the executable specification. Implement
`replay` mode with exchange capture and hashing.

**Gate.**
- The loopback backend, running out-of-process, reproduces the `disc` digest **exactly**.
- A recorded exchange replayed in `replay` mode reproduces the `live` run's digest, and the exchange
  file's sha256 appears in the manifest as a data-digest input.
- Per-step round-trip overhead measured and documented; a 3 600-step run completes with **< 30 s**
  of IPC overhead (per §2.3, ~7 s is the expectation).
- A backend that replies with the wrong `step` number causes a **hard failure**, not a silent
  divergence.

### Phase 5 — Standards Tier A + the TS 103 759 evidence container

**Work.** Vendor the ETSI Forge `.asn` modules at pinned tags. `MessageCodec` with `native_v1`
(default) and `etsi_cam_ts103900`. Two-library cross-check test. **Instantiate
`MaEvidenceMessage`** and write a real evidence table. Adopt the `(tgtId, obsId)` vocabulary and the
normative thresholds into `refdata/`.

**Gate.**
- With `message_codec = native_v1`, all 8 goldens unchanged.
- With `etsi_cam_ts103900`, a CAM UPER round-trips and `asn1tools` and `pycrate` produce
  **byte-identical** output for the same CAM (each decoding the other's bytes).
- **Every filed report carries ≥ 1 real PDU in `v2xPduEvidence`**, and an independent script can
  re-verify the reported claim from the evidence alone.
- The number of tests asserting a standards property goes from **0 to ≥ 10**.

### Phase 6 — Mobility, PKI, real signing, DCC

**Work.** `MobilityEngine` with `frozen_trace`. `PkiBackend` wiring `butterfly.py` + `ec.py` with
RFC 6979 deterministic ECDSA. Port `Dcc.java`. Fix the `ls_x(0)` vs `ls_x(i)` revocation divergence.
Close the `legacy_global_rng` grandfathering by moving `disc`'s loss coin onto a keyed stream —
**which will move `0bd93655…` and must be a deliberate, announced re-pin**, not a side effect.

**Gate.**
- Signing is on the dataset path: `ca.sign` / `ca.verify` call counts > 0, and `sig_ok` reflects an
  **actual** verification.
- Two runs with the same seed produce identical signatures (RFC 6979 proven, not assumed).
- The Python and Java engines agree on the revocation period.
- The re-pin is a single commit that touches only golden constants and states the cause.

---

## 8. Migration: built-ins onto the interfaces, with zero behaviour change

### 8.1 The principle

**Code motion only, in the first step.** The migration must not change *which function is called
with which arguments in which order*. Everything else follows.

### 8.2 Radio models

| Model | Becomes | Declared capabilities | Notes |
|---|---|---|---|
| `disc` | `DiscChannel(LinkChannelModel)` | `reach`, `legacy_global_rng`, `loss_composition:additive_legacy` | The hard-range test `d <= rr` and the additive loss (run.py:3392) move verbatim. |
| `logdistance` | `LogDistanceChannel(LinkChannelModel)` | as above + `link_state:none` | Keeps the per-packet reconstructed stream keyed on the **cert digest** (run.py:3360) — a documented correctness bug (pseudonym rotation resamples the channel) that is *deliberately preserved* here because fixing it moves `939b4faa…`. Fix scheduled with a re-pin, not smuggled into a refactor. |
| `geometric` | `GeometricChannel(LinkChannelModel)` | `rssi`, `link_state`, `reach`, `cbr`, `stateful`, `loss_composition:independent_survival` | Already the right shape. Rename `cap_m` → `reach_m` (keep `cap_m` as an alias for one minor version). |

`PerLinkAdapter` calls `evaluate` inside the existing loop at the existing point, so `cand.sort()`
(run.py:3325), the `geo_meta` index-parallel list and the `in_range` order are all untouched.

### 8.3 Detectors

1. **Move `detectors()` (run.py:2558-2577) verbatim** into five `Check` classes sharing the lagged
   reference. No arithmetic changes.
2. **Move each inline block at run.py:3458-3529 verbatim** into its own `Check`. The loop locals
   each one reads (`b`, `rx`, `rxx`, `rxy`, `st`, `cells`, `cap`, `rr`, `_offroad`, `cfg`, `Z`,
   `ref`, `step`) become `Observation` fields plus `params`. `cells` (the Sybil Counter) becomes
   `obs.neighbourhood["cell_cert_count"]`; `cap`/`rr` become `obs.rx_reach_m` — **which
   incidentally makes `art_reach` (run.py:3469) a declared input instead of a leak.**
3. **`DET_KEYS` becomes `tuple(c.reason_code for c in checks)`**, with the *identical* order,
   including the two conditional appends.
4. **The streak/`report_prob`/ordering block (run.py:3530-3538) becomes `StreakFusion`**, moved
   verbatim.

### 8.4 How zero behaviour change is *verified* — five independent gates

| # | Gate | Catches |
|---|---|---|
| **V1** | All 8 pinned goldens unchanged, across all 6 test files, for all three built-in radio models and both feature-gate combinations. | Any arithmetic or ordering change. |
| **V2** | **Global-RNG draw-count equality.** Instrument `random.Random.random`/`gauss`/`uniform` on the global object; assert the total draw count and the first/last 100 values are identical pre- and post-refactor. | A reordering that happens to produce the same digest today but is fragile. |
| **V3** | **Per-link outcome equality.** Dump `(step, rx_vid, tx_index, heard, rssi, link_state)` for a fixed seed before and after; assert file equality. Same for `(step, rx, sender, reason_code, detnorm)`. | A change invisible in the aggregate digest because a report was filtered later. |
| **V4** | **Schema equality.** `config_schema()` output identical except the one new `plugins` key; `--dump-config-schema` diff is a single line. | Accidental field renames/reordering breaking the GUI/copilot contract. |
| **V5** | **Two-run determinism** (`test_pipeline.py:77` pattern) plus the registering-but-not-enabling test (`test_radio_propagation.py:78-86` pattern) run for **every** built-in now reached through the registry. | Registry construction itself perturbing anything. |

V1 alone is not sufficient — a digest is a lossy summary, and V2/V3 are what make the refactor
reviewable rather than merely lucky.

### 8.5 The cross-engine question — answered once

**Do not build a single ABI across the Python and MOSAIC/Java engines.** They differ structurally,
not cosmetically: Python is **batch and receiver-outer** (one pass per step, the receiver sees the
entire concurrent transmitter set and computes an aggregate `load` before deciding per-packet loss);
Java is **event-driven per message** and never sees the concurrent set. Java's `RxChannel` is a
**post-filter** on frames MOSAIC's SNS already delivered — it can drop a link but can never *add*
one, so it has no candidate-window / `reach_m` concept at all. RNG differs by construction (CPython
sha512-seeded MT vs a 64-bit Java LCG), so numeric parity is impossible.

**What is realistic — and what the repo already does correctly for the physics kernel** (TR 37.885
constants transcribed once into `datagen/refdata/*.json`, mirrored in both engines, with
`tests/test_geometric_channel.py` grading against the **refdata file** rather than a second copy of
the formula):

1. a **versioned JSON parameter/refdata schema**, shared;
2. a **shared output vocabulary** — `rssi_dbm`, the link-state names `LOS`/`NLOSv`/`NLOSb`, manifest
   key naming, and (new) the TS 103 759 `(tgtId, obsId)` reason vocabulary, which also fixes the
   current Python/Java detector divergence;
3. **per-engine conformance tests against the refdata**;
4. **two engine-native plugin ABIs** — the Python `BatchChannelModel` here, and a Java
   `ServiceLoader`-based SPI shaped like `RxChannel` / `CamDetector`.

MOSAIC's own contract is already the model for (4) on the Java side, and it is **in this repo's
tree**: `third_party/veremi-nextgen/Generator/simulation/mosaic/etc/runtime.json:94-154` declares
`sns`, `ns3` and `omnetpp` ambassadors that are interchangeable **because they subscribe to an
identical interaction set** (`RsuRegistration`, `ChargingStationRegistration`,
`TrafficLightRegistration`, `VehicleUpdates`, `V2xMessageTransmission`,
`AdHocCommunicationConfiguration`). "Pick any network system" is implemented there as: *delete one
JSON block, add another.* That is the strongest available demonstration that D1 — an ABI defined as
a fixed message vocabulary rather than a class signature — is the right shape, and we can point at
it in our own tree.

---

## 9. Anti-patterns found in the prior art — do not copy

- **F2MD's integer-index enum + `switch` dispatch** (`MdAppTypes.h` enum + parallel `AppNames`/
  `intApp` arrays + `F2MDParameters.h` + a member + a `switch` case + the `.ned` param). Same
  fork-forcing shape as this repo's closed `radio_model` enum, and worse: selection is by array
  index. Usefully, this demonstrates that the domain's leading MBD framework has the **identical
  unsolved problem** — which is why the registry prior art has to come from Artery/ns-3/MOSAIC.
- **F2MD's `realDynamicMap` ground-truth pointer inside the checks object.** Voids an oracle
  firewall by construction.
- **F2MD's per-BSM construction of the check suite inside the switch.** Fine in C++; the equivalent
  here would forbid the AR(1) persistent per-link state `GeometricChannel` relies on.
- **ns-3's automatic index-based stream assignment**, whose configuration-sensitivity ns-3's own
  manual concedes. This repo's string-keyed streams already avoid it.
- **MOSAIC's env-var / out-of-process configuration without capture.** Any out-of-process plugin
  forfeits byte-determinism unless its output is captured to a file and hashed into the manifest.
  (The Java side already leaks this way: `AttackLib.Cfg` reads `System.getenv` with a silent
  `NumberFormatException` fallback.)

## 10. Safety posture, stated honestly

**In-process plugins are attested and detected, not sandboxed.** Say this in the docs rather than
implying safety.

**Enforceable and worth building:** capability by omission (never pass `rng`, never pass `b` or
`Vehicle`) — the only *strong* control; global-RNG state comparison before/after (always on in
conformance, `--strict-plugins` at runtime); PEP 578 audit hooks for filesystem/network/subprocess
**detection** in the conformance harness; a load-time AST lint for `threading`, `time`, `uuid`,
`os.urandom` and bare `random.` module calls (**warning-level only** — trivially bypassed by
`__import__("ran"+"dom")`); and declared file outputs.

**Not enforceable in-process, at all:** arbitrary code execution (`ctypes`, native extensions,
`sys.modules` monkeypatching — a plugin can replace `random.Random` itself); native-code
nondeterminism (BLAS thread counts, FMA contraction, float reduction order); entropy and clock
access; resource limits (the `resource` module is POSIX-only, **Windows has no RLIMIT equivalent**).
PEP 578 itself states it *"is not sandboxing… does not attempt to prevent malicious behavior"*, and
hooks fire only in the current interpreter, so a subprocess escapes entirely.

**Real isolation requires leaving the process**, and on this host (Windows, WSL disabled) only
subprocess isolation is viable — which is affordable **only** because §2.3 already batches the
interface per step. That is tier 2, for genuinely untrusted code. Build tier 1 now.
