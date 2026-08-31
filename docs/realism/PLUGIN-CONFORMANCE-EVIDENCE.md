# Phase 2 acceptance evidence: an out-of-repo channel plugin

**Status:** phase 2 of `PLUGIN-ARCHITECTURE.md` implemented and green. **Date:** 2026-08-31.
**Host:** Windows Server 2022, CPython 3.12.10, `PYTHONHASHSEED=0`
(`hash_randomization=false`), no shapely/scipy, WSL disabled.
**Suite:** `.\test.ps1 -q` → **833 passed in 739.21 s**, exit 0. All 8 pinned goldens unchanged.

This document records the phase-2 gate with the output that was actually produced, not a description
of it. Everything below was run by `C:\Temp\scms_plugin_demo\ACCEPTANCE.ps1`, whose full transcript
is reproducible with one command (§1).

**What phase 2 shipped.** `src/scms_sim_ref/conformance/` (the C1-C12 suite, delivered both as a
pytest-importable contract and as a CLI), `scms-poc verify-plugins`, `scms-poc conformance`, six new
plugin-lock checks in `tools/verify_data.py`, opt-in in-run attestation
(`plugins.<slot>.conformance = "required"`), and the missing half of `--allow-plugin-drift`
(section 4.3's *"writes the drift into the new manifest"*, deferred out of phase 1). The `plugins`
config field, the resolver and the lock itself landed in phase 1 and are unchanged apart from one
bug fix (§3).

| file | lines | what |
|---|---|---|
| `src/scms_sim_ref/conformance/v1/channel.py` | 657 | `ChannelModelContract` — the thirteen checks |
| `src/scms_sim_ref/conformance/v1/harness.py` | 363 | scenarios, `DrawCounter`, `audit_guard`, `OracleStation` |
| `src/scms_sim_ref/conformance/runner.py` | 189 | the pytest-free driver and `ConformanceReport` |
| `tests/test_conformance.py` | 613 | grades the SUITE: one deliberate violator per check |
| `tools/verify_data.py` | +103 | `PL1`–`PL6` |
| `src/scms_sim_ref/mock_pipeline/run.py` | ~+310 | the two subcommands, attestation, the drift record, the `logdistance` waiver |

---

## 0. The subject: a plugin that lives outside this repository

`C:\Temp\scms_plugin_demo` is a separate distribution, `scms-demo-channel 0.1.0`, installed as its
own wheel into `site-packages`. It imports `scms_sim_ref.api` and **nothing else** — no
`mock_pipeline`, no `datagen`, no `scms_core`, no `schemas` — and a test in its own suite asserts
that by scanning its source. It is deliberately **not vendored into this repository**: a plugin
demonstration that lived inside the engine repo would prove nothing about forking.

| module | `plugin_id` | role |
|---|---|---|
| `rayleigh.py` | `rayleigh` | the reference model: log-distance path loss + AR(1) log-normal shadowing + Rayleigh fading, hard reach cutoff |
| `nondeterministic.py` | `wallclock` | identical physics, fade seeded from `time.time()` — **meant to be rejected** |
| `leaky.py` | `leaky` | identical physics + one line reading the attacker flag — **meant to be rejected** |

`PYTHONPATH` points at the engine's `src/` only because `scms-sim-api` is not yet split out as its
own distribution (design section 2). When it is, `dependencies = ["scms-sim-api"]` replaces it and
nothing else about the package changes. That is a packaging gap, and it is worth being precise about
what it does and does not weaken: the plugin's *import surface* is already the published ABI (tested,
not asserted), but its *install closure* is not yet, because the ABI is not separately installable.

---

## 1. Reproducing it

```powershell
. C:\Users\Administrator\tools\env.ps1
$env:PYTHONHASHSEED = '0'
$env:PYTHONPATH     = 'C:\Users\Administrator\Documents\SCMS-Simulator\src'
pip install C:\Temp\scms_plugin_demo
C:\Temp\scms_plugin_demo\ACCEPTANCE.ps1                     # the whole gate, ~4 minutes
```

Individual commands:

```powershell
$SCMS = 'scms_sim_ref.mock_pipeline.run'
python -m $SCMS conformance --ref scms_demo_channel.rayleigh:RayleighChannel --report conf.json
python -m $SCMS --config C:\Temp\scms_p2\cfg_rayleigh.json --out C:\Temp\scms_p2\rayleigh_a
python -m $SCMS verify-plugins C:\Temp\scms_p2\rayleigh_a\manifest.json
cd C:\Temp\scms_plugin_demo; python -m pytest tests -q
```

---

## 2. The five acceptance items, measured

### (a) It passes all twelve conformance checks

`scms-poc conformance --ref scms_demo_channel.rayleigh:RayleighChannel` → **exit 0**, thirteen rows
(the twelve numbered checks; C6 has two arms):

```
conformance v1 :: channel_model :: scms_demo_channel.rayleigh:RayleighChannel
  PASS   C1_repeatable                      PASS   C7_ranges
  PASS   C2_call_order_independent          PASS   C8_reach_honesty
  PASS   C3_global_rng_untouched            PASS   C9_monotone_in_distance
  PASS   C4_state_advances_once_per_step    PASS   C10_fail_fast
  PASS   C5_no_io                                   bounded field 'pathloss_exponent'
  PASS   C6_no_oracle_leak                          rejected 11.0 at construction
  PASS   C6b_oracle_invariance              PASS   C11_float_hygiene
                                            PASS   C12_pipeline_two_run_digest
                                                   data_digest=a5bebfd5f418249e09695334b66faffe57d646072e36af5d6c91b3ca91666f85
  -- 13 passed, 0 failed, 0 skipped, 0 waived, 0 errored in 0.5s
```

The same thirteen also run as pytest tests from outside this repository — that is the whole of what
a plugin author writes:

```python
class TestRayleighConformance(ChannelModelContract):
    REF = "scms_demo_channel.rayleigh:RayleighChannel"
    PARAMS = {"range_m": 500.0}
```

`cd C:\Temp\scms_plugin_demo; python -m pytest tests -q` → **31 passed in 2.23s** (two contract
subclasses × 13, plus five explicit tests; the second subclass resolves the same class through the
installed **entry-point catalogue** instead of a dotted path, and must grade identically).

### (b) Byte-identical output across two runs

Two independent interpreter processes, same config:

```
run a data_digest = 9abe9eeac07f947ebb8a527c92351d70cea9ebc7028af4fd741078b168ec458b
run b data_digest = 9abe9eeac07f947ebb8a527c92351d70cea9ebc7028af4fd741078b168ec458b
IDENTICAL         = True
files compared    = 11
differing files   = ['manifest.json']
```

Ten of eleven output files are byte-identical. `manifest.json` differs **only** in `build_utc`, a
timestamp excluded from `data_digest` by construction.

The lock recorded for that run:

```
slot               channel_model
ref                scms_demo_channel.rayleigh:RayleighChannel
resolved_via       dotted_path
distribution       scms-demo-channel 0.1.0
dist_sha256        4a99e611f01d80135c96ff77984c0211b49009d27d5f376421615097b152ee01
module_sha256      f929999fe2a62c6536956ce5ffb42ed827045024531e122fc0a62a1dfa82f164
interface_version  ChannelModel/1.0
capabilities       ['loss_composition:independent_survival', 'reach', 'rssi', 'stateful']
declared_streams   ['coin', 'fade', 'shadow']
params_sha256      39d565d2f0228f20c35848e8c3b7e37ce470472eca9a0139331281c021d8b0ba
provenance_digest  b7d95b69d43dd3d09e433ce2464c5c99942451ca5ea4aad3e24c2ac34ae84aa6
```

`declared_streams` is not decoration: it is D3's third property, that a plugin **declares the
randomness surface it consumes**, captured at the end of the run from what the `RngNamespace`
actually handed out.

### (c) One mutated byte makes the manifest unreplayable, before step 0

One byte changed in the installed `rayleigh.py` — `plainly` → `Plainly`, **inside a docstring**, so
the model's behaviour is bit-for-bit identical:

```
mutated byte offset 115; differing bytes = 1
verify-plugins BEFORE mutation exit = 0
verify-plugins AFTER  mutation exit = 2
replay exit                         = 2
output directory created            = False
```

with

```
PLUGIN DRIFT: plugin drift in slot 'channel_model' ref 'scms_demo_channel.rayleigh:RayleighChannel':
module_sha256 expected 'f929999fe2a62c6536956ce5ffb42ed827045024531e122fc0a62a1dfa82f164',
got '109dd695a92a2539e4ff0aecff5fd4ab9377a2c4020844614822c329bda1c84c'.
```

`output directory created = False` is the load-bearing line: the failure is raised during
`config_from_dict`, before `run_pipeline` is entered, so **no partial dataset exists**. Contrast the
pre-phase-1 behaviour, which dropped the unknown key with a stderr warning and exited 0.

Note which hash caught it. `dist_sha256` is derived from the wheel's `RECORD`, a static metadata
file that an edit to an installed `.py` does not touch — so after the mutation the RECORD is
**stale and still says the file is fine**. Only `module_sha256`, hashed from the file on disk at
load time, sees it. Both are recorded precisely because they fail in different circumstances.

**And the complementary case.** `--allow-plugin-drift` on the same mutated tree:

```
[plugins] DRIFT ALLOWED: ... module_sha256 expected 'f929999f...' got '109dd695...'
exit = 0
digest with drifted source = 9abe9eeac07f947ebb8a527c92351d70cea9ebc7028af4fd741078b168ec458b
digest identical to run a  = True
new manifest records drift_allowed = True
new manifest module_sha256         = 109dd695a92a2539e4ff0aecff5fd4ab9377a2c4020844614822c329bda1c84c
```

That is the design's *"identity drift, no digest drift ⇒ harmless refactor of the plugin"* row,
produced on demand — and the new manifest records both the drift and what actually ran, so the two
locks diff cleanly. `tools/verify_data.py` then FAILs `PL6_no_accepted_plugin_drift` on that
dataset forever, which is correct: the artifact is permanently one whose code did not match the
manifest it replayed.

### (d) A nondeterministic plugin fails C1 *and*, separately, produces two digests

`WallClockChannel` seeds its fade from `time.time()`.

```
FAIL   C1_repeatable                 trace lengths differ: 690 vs 694
FAIL   C12_pipeline_two_run_digest   6ae77147... != a8e90089...
-- 8 passed, 5 failed ... exit 1
```

and at the CLI, two full runs that **both exit 0**:

```
run a data_digest = e3c94d7ee2428c620859527d0948695c80f2f47807a56b8e7b2b1da7a5854d8b
run b data_digest = 4275ef8acdec2d58fa8e120a0f71f2507fc0c9b884140be57d7f26b92f54298f
DIFFERENT         = True
```

Those two hex strings are, by construction, **not reproducible** — that is the finding. Four
executions of the same script produced four different pairs:

| run of the script | digest a | digest b |
|---|---|---|
| 1 | `e3c94d7ee242…` | `4275ef8acdec…` |
| 2 | `9b763d204df4…` | `c65b8489af71…` |
| 3 | `d2e892aba500…` | `a67c7fe4bc3e…` |
| 4 | `e83d0ce2d4fd…` | `504b8a208196…` |

Every other digest quoted in this document is stable across every run of the script; these eight are
the only ones that are not.

Both datasets carry the **same** `provenance_digest` (`56dae9f0444c…`) — no identity drift, digest
drift — which is the design's *"the plugin is nondeterministic (or the interpreter changed)"* row,
and `tools/verify_data.py` reports exactly that pair.

C2, C6b and C11 also fail for this model. That is collateral, not additional evidence: every one of
them compares two traces, and nothing unrepeatable can pass a comparison. C1 is the check that
names the cause.

### (e) A leaky plugin fails the no-leakage check — and only that

`LeakyChannel` subtracts 12 dB from the received power when `getattr(tx, "is_attacker", False)`.

```
PASS   C1_repeatable                 PASS   C8_reach_honesty
PASS   C2_call_order_independent     PASS   C9_monotone_in_distance
PASS   C3_global_rng_untouched       PASS   C10_fail_fast
PASS   C4_state_advances_once_per_step PASS C11_float_hygiene
PASS   C5_no_io                      PASS   C12_pipeline_two_run_digest
PASS   C6_no_oracle_leak                    data_digest=2955363be66298f5...
PASS   C7_ranges
FAIL   C6b_oracle_invariance
-- 12 passed, 1 failed ... exit 1
```

and two CLI runs:

```
run a data_digest = 2ea1338d9a4c087fde371bfec154647e60055dded639f7de20b6e6c2c3eaec4e
run b data_digest = 2ea1338d9a4c087fde371bfec154647e60055dded639f7de20b6e6c2c3eaec4e
IDENTICAL         = True
```

**This is the result worth reading twice.** The leaky plugin is perfectly reproducible, so the entire
pinned-golden apparatus — the project's headline correctness mechanism — is *blind* to it. Its output
column is `rssi_dbm`, a name on nobody's forbidden list (it is a legitimately MA-visible,
receiver-measurable quantity), so `datagen/leakage_linter.py` passes it too — and that linter is
name-based, post-hoc, and is never called from `run.py` in the first place. Yet every RSSI it emits
is a laundered attacker label, and a detector trained against it would score beautifully and have
learned nothing about the physical world.

**And the engine refuses it, before step 0, when asked to.** Adding `"conformance": "required"` to
the same config:

```
exit                     = 2
PLUGIN REFUSED: plugins.channel_model.conformance=required and
                'scms_demo_channel.leaky:LeakyChannel' does not conform:
                ['C6b_oracle_invariance'] failed.
  FAIL   C6b_oracle_invariance   the model's output changed when GROUND TRUTH it was never
         promised appeared beside the declared fields...
output directory created = False
```

A clean, explained, non-zero exit — not a traceback that happens to have the right effect. `main`
catches `ConfigError` (the plugin API's own class) and nothing wider, so any *other* exception still
surfaces with its traceback rather than being tidied away behind a summary.

Put (d) and (e) side by side and the design's claim that the two layers are independent stops being
an argument and becomes a measurement:

| | caught by C1 / the suite | caught by the two-run digest |
|---|---|---|
| `wallclock` (nondeterministic) | **yes** | **yes** |
| `leaky` (oracle-reading) | **yes (C6b only)** | **no — digests identical** |

Neither layer subsumes the other. A project with only pinned goldens cannot distinguish an oracle
leak from good physics; a project with only a unit-level suite cannot prove an *artifact* reproduces.

---

## 3. What the suite found in this repository's own code

The suite is only worth having if it can fail, so it was pointed at the three built-ins first.

**`disc`** — 12 PASS, 1 SKIP (C4, which only applies to a model declaring `stateful`).
**`geometric`** — 13 PASS, no waivers. Its `if st["step"] == self.step` guard is the reference
implementation C4 encodes, and C4 confirms it.
**`logdistance`** — **fails C8 (reach honesty)**, and the failure is real.

`logdistance`'s `reach_m` is a **median-range calibration**, not an upper bound: the model is 0 dB at
`d == radio_range_m` by construction and a link closes iff `mean + shadow >= 0`, so a favourable
log-normal shadow legitimately closes links past it. Measured on the v1 harness at `range_m=500`,
`sigma=4 dB`, `n=2.7`:

* **999 of 7071 delivered links (14.13 %) land beyond the declared reach**, worst excess **940.1 m**;
* **296 of them (4.19 % of deliveries) exceed `art_max_m=150 m`** and would therefore score
  `>= 1.0` on `acceptanceRangeThreshold` — for an **honest sender at its true position**.

That is a genuine false-positive channel in a shipped built-in, and it exists because
`art_reach = rx_reach` is the model's declared reach (`run.py:3984`) while the model delivers past
it. It does **not** fire in the default 5×5 / 120 m grid — measured directly: **0** `acceptanceRange`
false positives over 2953 reports at seed 17 — simply because the whole map (max span ≈ 480 m) is
smaller than `reach + tolerance`. A larger map or a longer `rsu_range_m` would surface it.

It is recorded as a **declared waiver on the implementation**, not as a softened check:

```python
class LogDistanceChannel(LinkChannelModelBase):
    conformance_waivers = {"C8_reach_honesty": "reach_m is a MEDIAN-range calibration, not an
        upper bound ... 999 of 7071 delivered links (14.13 %) ... 296 of them (4.19 %) ..."}
```

This is Django's `DatabaseFeatures.django_test_skips` doctrine verbatim: a backend declares what it
legitimately cannot pass as **data it ships**, with a written justification that travels into
`conformance_report.json` and from there into the manifest. A waiver with an empty justification is
refused outright. Closing the underlying issue means either declaring the widened window as the
reach (which moves `939b4faa…`) or giving the model a hard cutoff (which is a different model), so
it is scheduled with the phase-6 re-pin rather than smuggled into a refactor.

**A second defect, in phase 1's own provenance code.** `dist_sha256` — which the design calls the
*strongest* identity — was `null` for **every** normally-installed wheel, not merely for the editable
installs the design calls out. `_distribution_uncached` bailed to `None` on the first `RECORD` row
carrying no hash, and a wheel's `RECORD` always has three kinds of unhashed row: `RECORD` cannot hash
itself, `__pycache__/*.pyc` are written at install time, and `direct_url.json` records where the
install came from. Unhashed rows are now carried as `(path, "")` so the file list is still covered,
`__pycache__` is excluded outright (a `.pyc` embeds mtimes and paths), and `None` is reserved for the
case the design actually meant — nothing hashed at all. `dist_sha256` for the demo plugin is now
`4a99e611f01d…`, and `data_digest` is unchanged, because the plugin block is excluded from it.

---

## 4. The suite itself, and why it is not tautological

`tests/test_conformance.py` grades **the suite**. It defines one honest reference model and nine
deliberate violators — one per check — and asserts that each violator fails *its own* check:

| violator | violates | why it is the realistic mistake |
|---|---|---|
| `UsesGlobalRng` | C3 | reaches for module-level `random.random()` |
| `OrderDependent` | C2 | one shared sequential stream; passes C1 perfectly |
| `TwicePerStep` | C4 | declares `stateful`, advances per call instead of per step |
| `WritesFiles` | C5 | memoises to a temp file |
| `LeakyKeys` | C6 | emits an `extras` column named after ground truth |
| `OracleReader` | **C6b, and nothing else** | deterministic, in range, monotone — and laundering a label |
| `BadUnits` | C7 | returns linear mW through a field documented as dBm |
| `OverReaches` | C8 | delivers past its own declared reach |
| `Antimonotone` | C9 | PDR *rises* with distance |

`OracleReader` carries an extra assertion: it must fail **exactly** `{C6b_oracle_invariance}` and
nothing else, because every other signal a reviewer or a linter could look at is clean.

Three instruments make the checks measurable, and each exists for a specific reason:

* **`DrawCounter`** counts *top-level* `random.Random` calls behind a re-entrancy guard.
  `gammavariate` is rejection sampling and calls `random()` a variable number of times; counting
  primitives would report a correct step guard as a C4 violation. Pinned by a test: 20 `gammavariate`
  calls count as exactly 20, six times running.
* **`build_ladder`** rotates the transmitters around the receiver each step, holding the distance
  *exactly* constant while moving both endpoints tens of metres. Without that, C9's seven rungs would
  be one correlated shadowing trajectory reported as `n_tx × n_steps` independent samples.
* **`OracleStation`** is field-identical to a plain `StationSnapshot` through every declared field —
  asserted by a test, since C6b is only a valid experiment if the two frame sequences really are
  indistinguishable through the interface.

**Deviations from the design memo, stated plainly.**

The memo lists `expected_failures: frozenset` beside `waivers`. It is **not implemented**: it is a
second way to excuse a failing check that carries no justification, which is exactly the mute button
the waiver mechanism exists to avoid being. One mechanism, and it costs a sentence that lands in the
report next to the artifact.

The memo names check C6b
`rssi_tracks_true_geometry_not_claimed` and calls for a verbatim port of
`tests/test_geometric_channel.py::test_rssi_tracks_true_geometry_not_the_claimed_position`. That port
cannot be written against this ABI: `StationSnapshot` carries **no claimed position at all** — it is
oracle-side by necessity and carries true geometry only — so "did the model use the claimed position"
is not a question the interface can pose. What ships is `C6b_oracle_invariance`, the strictly stronger
statement the ABI *can* pose: two frame sequences, identical in every declared field, one of which
additionally carries ground truth, must produce identical traces. It catches a laundered claimed
position, a laundered attacker flag, and anything else. The memo's original dataset-level correlation
test is untouched and still runs.

**No Hypothesis**, deliberately: it is randomised and keeps an example database, and a
nondeterministic conformance suite in a determinism project is self-defeating. Every sequence comes
from a seeded `random.Random`; the whole suite is a pure function of `ChannelModelContract.SEED`.

---

## 5. The lock, audited on disk

`tools/verify_data.py` gains six checks (`PL1`-`PL6`) that read the **lock** (`manifest["plugins"]`
— what was actually loaded, content-addressed) rather than the **intent** (`config.plugins` — what
was asked for): lock shape, a recomputed `provenance_digest`, identity present or explicitly
`provenance_incomplete`, no reserved capability on a third-party entry, the `runtime` block, and no
accepted drift. `PL2` re-derives the digest with its own canonicalisation rather than importing the
engine to ask — an audit that trusts the code it audits is not an audit — so an edited lock is caught
even by a checkout whose `run.py` was edited to write it.

Every check SKIPs on a manifest written before the lock existed, so historical corpora stay green: an
audit tool that retroactively fails old artifacts is one people stop running.

Over the seven acceptance datasets: **182 PASS, 129 SKIP, 1 FAIL** — the one FAIL being
`rayleigh_d`'s `PL6_no_accepted_plugin_drift`, which is the dataset deliberately produced under
`--allow-plugin-drift`. The two `wallclock_*` datasets share `provenance_digest 56dae9f0444c…`
while carrying different `data_digest`s, so §4.3's diagnosis table can be read straight off the audit
output.

---

## 6. What phase 2 does **not** claim

* **In-process plugins are attested and detected, not sandboxed.** C5 uses a PEP 578 audit hook, and
  PEP 578 says of itself that it *"is not sandboxing… does not attempt to prevent malicious
  behavior"*. Hooks fire only in the current interpreter, so a subprocess escapes entirely; a plugin
  can `ctypes` its way out, monkeypatch `sys.modules`, or replace `random.Random` itself. C5 catches
  the model that memoises to a temp file or phones a licence server. It does not contain hostile code.
  Real isolation requires leaving the process, which is affordable only because the ABI is already
  batched per step (design section 2.3), and is phase-4 work.
* **Attestation is opt-in, and C12 is excluded from it.** Section 5's third delivery route — *let
  the engine refuse an unattested plugin* — is implemented as a config declaration, not a flag, so
  it lands in `manifest["config"]` and replays like everything else:

  ```jsonc
  "plugins": {"channel_model": {"ref": "...", "conformance": "required"}}
  ```

  The engine then runs the suite in `build_channel`, before step 0, refuses a failing plugin with
  the failing check ids in the message, and writes the summary into
  `manifest["plugins"]["loaded"][*]["conformance"]` — exactly the shape §4.2 draws. Measured cost:
  **0.111 s per run** (1.268 s vs 1.157 s, median of 3, on the 60 s grid scenario), and the
  `data_digest` is unchanged (`9abe9eea…` either way).

  It is **off by default**, for two reasons that are not timidity: that 0.111 s is on the engine
  path of every run, and C5 installs a PEP 578 audit hook that *can never be removed*, leaving a
  small permanent per-audit-event cost on the process. The honest home for a twelve-check suite is a
  CI gate.

  **C12 is excluded and cannot be otherwise**: it runs two full pipelines, so running it from inside
  `build_channel` — itself inside a pipeline — puts a pipeline inside a pipeline. The exclusion
  happens *before* the suite runs, not by filtering C12's row out of the report afterwards; that
  distinction is worth 0.19 s, which is how it was found. The first implementation filtered
  afterwards and measured **0.30 s**; excluding properly measures **0.111 s**, and the difference is
  exactly the two pipelines C12 was still quietly executing while the summary said "excluded". The
  exclusion is named in the embedded summary (`"excluded": ["C12_pipeline_two_run_digest"]`) rather
  than hidden, so nobody reads `"passed": 12` there and believes the artifact-level check ran.
* **The detector contract (D1-D7) does not exist.** It needs the frozen `Observation` DTO, which is
  phase 3. `conformance.runner.CONTRACTS` has one entry, and the CLI's `--slot` refuses anything else
  rather than pretending.
* **`scms-sim-api` is still one distribution with the engine.** See §0.
* **C12 runs one 30-second grid scenario.** It proves two runs of *that* configuration agree; it is
  not a sweep, and a plugin whose nondeterminism only appears at high density would pass it and be
  caught by C1 instead.
