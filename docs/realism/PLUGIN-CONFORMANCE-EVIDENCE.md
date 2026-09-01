# Phase 2 acceptance evidence: an out-of-repo channel plugin

**Status:** phase 2 of `PLUGIN-ARCHITECTURE.md` implemented and green. **Date:** 2026-08-31.
**Host:** Windows Server 2022, CPython 3.12.10, `PYTHONHASHSEED=0`
(`hash_randomization=false`), no shapely/scipy, WSL disabled.
**Suite:** `.\test.ps1 -q` → **833 passed in 739.21 s** at phase 2; **922 passed in 992.65 s** after
the §7 adversarial review, exit 0 both times. All 8 pinned goldens unchanged, and the no-plugin
reference run still digests `b25f2137cf14dd50…`.

This document records the phase-2 gate with the output that was actually produced, not a description
of it. Everything below was run by `C:\Temp\scms_plugin_demo\ACCEPTANCE.ps1`, whose full transcript
is reproducible with one command (§1).

**What phase 2 shipped.** `src/scms_sim_ref/conformance/` (the C1-C13 suite — C13 was added by the
§7 adversarial review; phase 2 itself shipped C1-C12 — delivered both as a
pytest-importable contract and as a CLI), `scms-poc verify-plugins`, `scms-poc conformance`, six new
plugin-lock checks in `tools/verify_data.py`, opt-in in-run attestation
(`plugins.<slot>.conformance = "required"`), and the missing half of `--allow-plugin-drift`
(section 4.3's *"writes the drift into the new manifest"*, deferred out of phase 1). The `plugins`
config field, the resolver and the lock itself landed in phase 1 and are unchanged apart from one
bug fix (§3).

| file | lines | what |
|---|---|---|
| `src/scms_sim_ref/conformance/v1/channel.py` | 924 | `ChannelModelContract` — the fourteen rows (C13 added by §7) |
| `src/scms_sim_ref/conformance/v1/harness.py` | 432 | scenarios, `DrawCounter`, `audit_guard`, `OracleStation`, the `random.Random` surface instrument |
| `src/scms_sim_ref/conformance/runner.py` | 203 | the pytest-free driver and `ConformanceReport` |
| `tests/test_conformance.py` | 1097 | grades the SUITE: one deliberate violator per check, six more for the §7 arms |
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

### (a) It passes all thirteen conformance checks

`scms-poc conformance --ref scms_demo_channel.rayleigh:RayleighChannel` → **exit 0**, fourteen rows
(the thirteen numbered checks; C6 has two arms). Transcribed from the acceptance run of
**2026-08-31 19:56 Z**, the first with the §7 fixes and C13 in place:

```
conformance v1 :: channel_model :: scms_demo_channel.rayleigh:RayleighChannel
  PASS   C1_repeatable
  PASS   C2_call_order_independent
  PASS   C3_global_rng_untouched
  PASS   C4_state_advances_once_per_step
  PASS   C5_no_io
  PASS   C6_no_oracle_leak
  PASS   C6b_oracle_invariance
  PASS   C7_ranges
  PASS   C8_reach_honesty
  PASS   C9_monotone_in_distance    ladder [(25.0, 1.0, -62.49), (60.0, 1.0, -71.62),
                                    (125.0, 0.979, -78.83), (200.0, 0.948, -83.28),
                                    (300.0, 0.844, -86.39), (400.0, 0.698, -87.9),
                                    (475.0, 0.615, -88.81)]
  PASS   C10_fail_fast              bounded field 'pathloss_exponent' rejected 11.0 at construction
  PASS   C11_float_hygiene
  PASS   C12_pipeline_two_run_digest
                                    data_digest=191e71a3171e3f4a1965d76ed5dccb61c4b2b1470011a4984e70ba76661a72bd
  PASS   C13_config_not_mutated     config unmoved across construction and 12 steps
  -- 14 passed, 0 failed, 0 skipped, 0 waived, 0 errored in 0.6s
```

The C9 ladder is printed in full deliberately: it is the number that moved when the dead step term
was fixed (§7 defect 1). Before the fix the same model measured
`1.000 1.000 1.000 1.000 0.969 0.885 0.792`.

The same fourteen also run as pytest tests from outside this repository — that is the whole of what
a plugin author writes:

```python
class TestRayleighConformance(ChannelModelContract):
    REF = "scms_demo_channel.rayleigh:RayleighChannel"
    PARAMS = {"range_m": 500.0}
```

`cd C:\Temp\scms_plugin_demo; python -m pytest tests -q` → **33 passed in 2.66s** (two contract
subclasses × 14, plus five explicit tests; the second subclass resolves the same class through the
installed **entry-point catalogue** instead of a dotted path, and must grade identically). It was
31 before C13 was added.

### (b) Byte-identical output across two runs

Two independent interpreter processes, same config:

```
run a data_digest = be3b5deaa209d07847050e7490f000a7c8b044a0a8fe50ce90e9579c0dab5f94
run b data_digest = be3b5deaa209d07847050e7490f000a7c8b044a0a8fe50ce90e9579c0dab5f94
IDENTICAL         = True
files compared    = 11
differing files   = ['manifest.json']
```

Ten of eleven output files are byte-identical. `manifest.json` differs **only** in `build_utc`, a
timestamp excluded from `data_digest` by construction.

The lock recorded for that run, read back out of `C:\Temp\scms_p2\rayleigh_a\manifest.json`
(`build_utc 2026-08-31T19:56:19Z`):

```
slot               channel_model
ref                scms_demo_channel.rayleigh:RayleighChannel
resolved_via       dotted_path
distribution       scms-demo-channel 0.1.0
dist_sha256        f045cf16284ee86da2e67863780bf9fc1903ec27ccbd28732660374cbc673aa9
module_sha256      f929999fe2a62c6536956ce5ffb42ed827045024531e122fc0a62a1dfa82f164
package_sha256     24eea74f5eeac91abbe2c809faf41ccfc77172caff5f7c796a17df3fb5c0bbb5
interface_version  ChannelModel/1.0
capabilities       ['loss_composition:independent_survival', 'reach', 'rssi', 'stateful']
declared_streams   ['coin', 'fade', 'shadow']
params_sha256      39d565d2f0228f20c35848e8c3b7e37ce470472eca9a0139331281c021d8b0ba
provenance_digest  31f3d7ff80f11d15151d7839c114bf6540fe1927c13a7791b5ed6def47a49aac
```

> **Correction (2026-08-31, adversarial review) — the transcription.** This block once printed
> `dist_sha256 4a99e611f01d80…` and `provenance_digest b7d95b69d43dd3d0…`. **Neither value was in
> the artifact it claimed to be quoting.** The artifact as it stood then (`build_utc
> 2026-08-31T05:27:27Z`) recorded `dist_sha256 f045cf16…` and `provenance_digest 91397ec9…`, its
> single lock entry re-digested to `91397ec9…` (so `verify_data.PL2` passed on it), and four fresh
> runs of `cfg_rayleigh.json`, plus a `--force-reinstall`, all reproduced `f045cf16…`.
>
> The printed pair was not noise, and that is the interesting part: recomputing the digest over the
> artifact's **own** lock entry with **only** `dist_sha256` substituted by `4a99e611…` yields
> `b7d95b69d43dd3d09e433ce2464c5c99942451ca5ea4aad3e24c2ac34ae84aa6` — the document's value,
> exactly. The old block was therefore a **self-consistent snapshot of a different install state**,
> transcribed as if it were `rayleigh_a`'s. §3's headline for the phase-1 `dist_sha256` fix repeated
> the same wrong value and is corrected there too. **A transcribed number that nothing re-derives is
> not evidence**; every digest in this document has since been read back out of the artifact, or
> re-measured, rather than copied out of a terminal buffer.

> **Superseded (2026-08-31, adversarial review) — the numbers themselves.** The block above no
> longer prints `data_digest 9abe9eea…` or `provenance_digest 91397ec9…`, and both movements are
> deliberate:
>
> * `9abe9eea… → be3b5dea…`, because `9abe9eea…` was measured with `RngNamespace.begin_step()`
>   never called. `_step` stayed `-1` for the whole run, every `stream()` key ended `:s-1` (5854
>   distinct `fade` keys, all of them), and this model's advertised per-packet Rayleigh fade was a
>   fixed per-link constant for all 60 s. The adapter now advances the namespace once per step
>   (`api.channel.BatchAdapter.begin_step`). See §7 defect 1.
> * `91397ec9… → 31f3d7ff…`, because the lock entry gained `package_sha256
>   24eea74f5eeac91abbe2c809faf41ccfc77172caff5f7c796a17df3fb5c0bbb5` (§7 defect 2). Adding a field
>   to a hashed record moves the hash by construction; **every third-party `provenance_digest`
>   recorded before 2026-08-31 moves, and no built-in entry does** (built-ins carry no
>   `package_sha256` and the field is omitted, not null, when absent).
>
> The whole gate was then re-run from a clean shell — `pip install --force-reinstall`, all five
> acceptance items, both CLI subcommands, the out-of-repo pytest suite and `tools/verify_data.py` —
> and every number in §2, §3 and §5 was re-transcribed from that single run
> (`build_utc 2026-08-31T19:56:19Z` onwards). The old artifacts were overwritten by it, which is why
> the values recorded above are the ones a reader can reproduce today and the earlier ones are
> quoted only inside these correction blocks.

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
**stale and still says the file is fine**. `module_sha256`, hashed from the file on disk at load
time, sees it; since §7 defect 2 so does `package_sha256`, because `rayleigh.py` is inside the
package tree. All three are recorded precisely because they fail in different circumstances, and
§3's third defect is the case where only the third of them fires.

**And the complementary case.** `--allow-plugin-drift` on the same mutated tree:

```
[plugins] DRIFT ALLOWED: ... module_sha256  expected 'f929999f...' got '109dd695...'
[plugins] DRIFT ALLOWED: ... package_sha256 expected '24eea74f...' got '82d1e2b8...'
exit = 0
digest with drifted source = be3b5deaa209d07847050e7490f000a7c8b044a0a8fe50ce90e9579c0dab5f94
digest identical to run a  = True
new manifest records drift_allowed = True
new manifest module_sha256         = 109dd695a92a2539e4ff0aecff5fd4ab9377a2c4020844614822c329bda1c84c
```

Two drift lines, not one: the docstring byte moves the defining file **and** the package tree it
sits in. Both are reported, both are recorded, and the digest is unchanged — a docstring is not
physics.

That is the design's *"identity drift, no digest drift ⇒ harmless refactor of the plugin"* row,
produced on demand — and the new manifest records both the drift and what actually ran, so the two
locks diff cleanly. `tools/verify_data.py` then FAILs `PL6_no_accepted_plugin_drift` on that
dataset forever, which is correct: the artifact is permanently one whose code did not match the
manifest it replayed.

### (d) A nondeterministic plugin fails C1 *and*, separately, produces two digests

`WallClockChannel` seeds its fade from `time.time()`.

```
FAIL   C1_repeatable                 trace lengths differ: 698 vs 697
FAIL   C12_pipeline_two_run_digest   89673fe9... != a7e4f6e5...
-- 8 passed, 6 failed, 0 skipped, 0 waived, 0 errored ... exit 1
```

and at the CLI, two full runs that **both exit 0**:

```
run a data_digest = 20b74b61b7ecb8c9f5c2e899cc662c7a78406cd3adcc5a608857e2d473dd9b81
run b data_digest = d7681aee5d9f3701db00e1b254c941a871d6090166aaad529046ee121aff93c8
DIFFERENT         = True
```

Those two hex strings are, by construction, **not reproducible** — that is the finding. Four
executions of the same script produced four different pairs:

| run of the script | digest a | digest b |
|---|---|---|
| 1 (2026-08-31 05:27 Z) | `e3c94d7ee242…` | `4275ef8acdec…` |
| 2 | `9b763d204df4…` | `c65b8489af71…` |
| 3 | `d2e892aba500…` | `a67c7fe4bc3e…` |
| 4 | `e83d0ce2d4fd…` | `504b8a208196…` |
| 5 (2026-08-31 19:56 Z, post-fix) | `20b74b61b7ec…` | `d7681aee5d9f…` |

Every other digest quoted in this document is stable across every run of the script; these ten are
the only ones that are not.

Both datasets carry the **same** `provenance_digest` (`46ec62fe3590d60e…`, read back out of
`C:\Temp\scms_p2\wallclock_a\manifest.json` and `wallclock_b\manifest.json`) — no identity drift,
digest drift — which is the design's *"the plugin is nondeterministic (or the interpreter changed)"*
row, and `tools/verify_data.py` reports exactly that pair.

> **Correction (2026-08-31, adversarial review).** This paragraph and §5 previously both printed
> `56dae9f0444c…`. That value was in neither `wallclock_a` nor `wallclock_b`; at the time both
> recorded `225a7781263e857c21ba77f1a26ccc304145a198ff360c373154b2d033c388de`, and since the
> `package_sha256` fix both record `46ec62fe3590d60e09d5c0d3f0da54edc655c73589f30b11daeb77b71769576a`
> (the field was added to the hashed record; see the second correction under (b)). The CLAIM the
> number supports — that the pair shares one `provenance_digest` while carrying different
> `data_digest`s — is true of the artifacts in all three states and is what makes §4.3's diagnosis
> table readable off the audit output; only the transcription was wrong. See the correction under
> (b) for the same failure on `rayleigh_a`.

C2, C6b, C9 and C11 also fail for this model. That is collateral, not additional evidence: three of
them compare two traces, and nothing unrepeatable can pass a comparison, while C9's ladder is a
sampled measurement that a clock-seeded fade perturbs. C1 is the check that names the cause.

### (e) A leaky plugin fails the no-leakage check — and only that

`LeakyChannel` subtracts 12 dB from the received power when `getattr(tx, "is_attacker", False)`.

```
PASS   C1_repeatable                 PASS   C8_reach_honesty
PASS   C2_call_order_independent     PASS   C9_monotone_in_distance
PASS   C3_global_rng_untouched       PASS   C10_fail_fast
PASS   C4_state_advances_once_per_step PASS C11_float_hygiene
PASS   C5_no_io                      PASS   C12_pipeline_two_run_digest
PASS   C6_no_oracle_leak                    data_digest=0c3330a7425d7cef...
PASS   C7_ranges                     PASS   C13_config_not_mutated
FAIL   C6b_oracle_invariance
-- 13 passed, 1 failed, 0 skipped, 0 waived, 0 errored ... exit 1
```

and two CLI runs:

```
run a data_digest = 85fb95ce51c28dcbc2720e8aa2dc75de784be3a28673420d84f2dc979e075b4b
run b data_digest = 85fb95ce51c28dcbc2720e8aa2dc75de784be3a28673420d84f2dc979e075b4b
IDENTICAL         = True
```

**This is the result worth reading twice.** The leaky plugin is perfectly reproducible, so the entire
pinned-golden apparatus — the project's headline correctness mechanism — is *blind* to it. Its output
column is `rssi_dbm`, a name on nobody's forbidden list (it is a legitimately MA-visible,
receiver-measurable quantity), so `datagen/leakage_linter.py` passes it too — and that linter is
name-based, post-hoc, and is never called from `run.py` in the first place. **What (d) + (e)
therefore demonstrate is that the two detection layers are INDEPENDENT: one catches what the other
structurally cannot.**

> **Correction (2026-08-31, adversarial review).** This paragraph used to end *"Yet every RSSI it
> emits is a laundered attacker label, and a detector trained against it would score beautifully and
> have learned nothing about the physical world."* **That sentence is false of `leaky_a` and
> `leaky_b`, the very artifacts it sits next to.** Both of `LeakyChannel`'s leak vectors are
> structurally inert on the ENGINE path: the engine builds plain frozen slotted `StationSnapshot`
> objects with no `is_attacker` / `falsified` attribute, and `StepFrame.env` is a
> `MappingProxyType({"buildings", "weather"})`. The leak materialises only inside the conformance
> harness, which deliberately plants an `OracleStation` (`conformance/v1/harness.py`) precisely so
> C6b has something to detect. Proof by measurement: subclassing `LeakyChannel` with `plugin_id`
> forced to `"rayleigh"` — so the RNG namespace, and therefore every draw, matches the honest
> reference — and running `cfg_rayleigh.json` produces a data digest **byte-identical** to the
> honest `RayleighChannel` run. So `leaky_a`/`leaky_b` contain **zero** laundered labels, and
> nothing here is evidence of a laundered dataset the golden apparatus is blind to. What C6b
> demonstrates is the CAPABILITY: a channel model that is handed ground truth will use it, and only
> the conformance layer can see that. The engine's own defence is that it never hands any over —
> which is capability-by-omission working, and worth stating as the result rather than dramatising
> a leak that did not happen.

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

Re-measured 2026-08-31 with C13 in the suite (`scms-poc conformance --ref <name>`, exit 0 for all
three):

**`disc`** — 12 PASS, 1 SKIP (C4, which only applies to a model declaring `stateful`), 1 WAIVED
(C9's attenuation arm — it is the deliberate unit-disc idealisation and fails that arm by
definition).
**`geometric`** — **14 PASS**, no waivers, no skips. Its `if st["step"] == self.step` guard is the
reference implementation C4 encodes, and C4 confirms it.
**`logdistance`** — 12 PASS, 1 SKIP, 1 WAIVED: it **fails C8 (reach honesty)**, and the failure is
real.

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
case the design actually meant — nothing hashed at all. `dist_sha256` for the demo plugin is
`f045cf16284ee86d…` (`C:\Temp\scms_p2\rayleigh_a\manifest.json`; this line previously printed
`4a99e611f01d…`, which is in no artifact — see the correction under §2 (b)), and `data_digest` is
unchanged, because the plugin block is excluded from it.

**A third defect, found by adversarial review (2026-08-31): the lock hashed one FILE, so a sibling
module was invisible.** `verify_lock` compared `module_sha256` — `sha256(inspect.getsourcefile(obj))`,
the defining file and nothing else — and `dist_sha256`, which is copied out of the wheel `RECORD` and
is therefore a record of what the installer *wrote*, not of what the tree *contains*. Measured:
`scms_demo_channel.leaky:LeakyChannel` is defined in `leaky.py` and inherits every line of its
physics from `rayleigh.py`. Adding 6 dB of transmit power to `rayleigh.py` **in place** moves neither
hash — `module_sha256` is `leaky.py`'s and is provably unchanged (pinned by the regression test
below), and `dist_sha256` is the installer's RECORD, which an in-place edit does not rewrite — and
those, plus `interface_version`, were the only three fields `verify_lock` compared. So the lock
reported clean over a plugin whose transmit power had changed by 6 dB: D4's stated failure mode
(*"a plugin-configured manifest would replay silently wrong"*) with the lock in place.
`api.registry.package_sha256` already existed and was **dead code**: defined, never called from
`src/`, `tools/` or `tests/`. It is now computed for every third-party entry (`package_sha256`, the
top-level package directory walked in sorted order, `__pycache__` excluded) and compared by
`verify_lock`.

**Re-measured end to end on the installed distribution, 2026-08-31.** The edit is one line of
`site-packages/scms_demo_channel/rayleigh.py`:

```diff
- self.tx_dbm = float(p.get("tx_power_dbm", 23.0))
+ self.tx_dbm = float(p.get("tx_power_dbm", 23.0)) + 6.0
```

`leaky.py` — the file `LeakyChannel` is *defined* in — is untouched, and the wheel `RECORD` is
untouched:

```
run of scms_demo_channel.leaky:LeakyChannel   data_digest = 85fb95ce51c28dcb…
  module_sha256    a1db16873e93c26a…   <- leaky.py ONLY
  dist_sha256      f045cf16284ee86d…   <- the wheel RECORD
  package_sha256   24eea74f5eeac91a…   <- the whole package tree

verify-plugins BEFORE the sibling edit                       no drift, exit 0
rayleigh.py sha256   f929999fe2a62c65… -> 97b4885833c02231…   (the sibling file DID change)
verify-plugins AFTER the sibling edit
  PLUGIN DRIFT: ... package_sha256 expected '24eea74f5eeac91abbe2c809faf41ccfc77172caff5f7c796a17df3fb5c0bbb5',
                got 'f3cf7cf12d03f2b663da2b80b7d7e43a9b1ef5ab90d3dd13e3e02acdd58cbe26'.     exit 2
replay exit = 2, output directory created = False
digest with the drift ALLOWED = 6d5de639502a8bcc…   (was 85fb95ce51c28dcb…)
verify-plugins after restoring rayleigh.py                   no drift, exit 0
```

`package_sha256` is the **only** field that names the drift; `module_sha256` and `dist_sha256` are
absent from the message because neither moved. And the last-but-one line is why it matters: the
edit was a real physics change, so the replay produces a different dataset — which, before the fix,
it did at exit 0 with `verify-plugins` printing "no drift". The `got` value is a function of the
exact edit text above; the `expected` value is a property of the artifact and is in every
`scms_p2` manifest.

Built-ins are exempt and unchanged (their package is the engine itself), so no built-in lock entry
and no pinned golden moves; third-party `provenance_digest` values recorded before 2026-08-31 do
move, by construction.
`tests/test_conformance.py::test_the_lock_sees_an_edit_to_a_SIBLING_module_not_just_the_defining_file`
pins it in-repo against a two-module package built in a tmp dir, and asserts `module_sha256` is
provably **unchanged** across the edit — which is the half that makes the test about the right
property.

---

## 4. The suite itself, and why it is not tautological

`tests/test_conformance.py` grades **the suite**. It defines one honest reference model and ten
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
| `MutatesTheConfigLate` | **C13, and nothing else** | writes `report_prob = 1.0` through `env["config"]` at step 8 |

Six more grade the arms the §7 review added, and each must also fail its check **alone**:

| violator | violates | the vector |
|---|---|---|
| `MonkeypatchesRandom` | C3 (class-surface trap) | rebinds `random.Random.random` — reaches every stream in the process, including the engine's private instance, while every state snapshot stays intact |
| `WalksTheStack` | C3 (decoy-frame trap) | `sys._getframe` walk to a caller's local named `rng`, which is exactly what `run_pipeline` calls its shared stream |
| `RisesWithinAdjacentTolerance` | C9 (cumulative arm) | PDR rises 0.08 per rung — inside the 0.10 per-rung tolerance at every rung, +0.48 end to end |
| `ConstantInDistance` | C9 (attenuation arm) | flat 50 % delivery and a constant −70.0 dBm at 25 m and at 475 m |
| `MutatesTheConfigOnceSettled` | C13 (high-step tail) | `if frame.step >= 30` — the realistic shape; invisible to any contiguous window shorter than 31 steps, caught by the four-frame tail at step 10 000 |
| `MutatesTheConfigBehindTheView` | C13 (snapshot arm) | reaches the real `PipelineConfig` through `ReadOnlyConfig`'s own slot, so **nothing raises** and only the before/after dict comparison sees it |

`OracleReader` and both C13 violators carry an extra assertion: each must fail **exactly** its own
check and nothing else, because every other signal a reviewer or a linter could look at is clean.
`MutatesTheConfigLate` writes at step 8 for that reason — past the horizon of every other scenario
in the suite (the longest is C9's 8-step ladder) and inside C13's own 12-step window. A model that
writes at step 0 makes every check error out, which is correct and proves nothing about which check
saw it.

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

Over the seven acceptance datasets, re-measured on the 2026-08-31 19:56 Z run: **182 PASS, 129 SKIP,
1 FAIL** — the one FAIL being `rayleigh_d`'s `PL6_no_accepted_plugin_drift`, which is the dataset
deliberately produced under `--allow-plugin-drift`. The two `wallclock_*` datasets share
`provenance_digest 46ec62fe3590d60e…` (corrected 2026-08-31; this line and §2 (d) both printed
`56dae9f0444c…`, which was in neither artifact, and the shared value has since moved from
`225a7781…` because `package_sha256` joined the hashed record) while carrying different
`data_digest`s, so §4.3's diagnosis table can be read straight off the audit output.

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
  **0.111 s per run** for the twelve-check suite (1.268 s vs 1.157 s, median of 3, on the 60 s grid
  scenario) and **0.153 s** re-measured with C13 in it (1.355 s vs 1.202 s, median of 3, same
  scenario, `geometric`). The `data_digest` is unchanged whether or not attestation runs
  (`9abe9eea…` either way when the first cost was measured; `be3b5dea…` either way since the §7
  defect-1 fix; `c2a64a1b…` either way for the `geometric` re-measurement — the point is that
  attesting does not perturb the run, and it still does not).

  It is **off by default**, for two reasons that are not timidity: that 0.111 s is on the engine
  path of every run, and C5 installs a PEP 578 audit hook that *can never be removed*, leaving a
  small permanent per-audit-event cost on the process. The honest home for a thirteen-check suite is
  a CI gate.

  **C12 is excluded and cannot be otherwise**: it runs two full pipelines, so running it from inside
  `build_channel` — itself inside a pipeline — puts a pipeline inside a pipeline. The exclusion
  happens *before* the suite runs, not by filtering C12's row out of the report afterwards; that
  distinction is worth 0.19 s, which is how it was found. The first implementation filtered
  afterwards and measured **0.30 s**; excluding properly measures **0.111 s**, and the difference is
  exactly the two pipelines C12 was still quietly executing while the summary said "excluded". The
  exclusion is named in the embedded summary (`"excluded": ["C12_pipeline_two_run_digest"]`) rather
  than hidden, so nobody reads `"passed": 13` there and believes the artifact-level check ran.
* **What is structurally enforced, and what is merely conventional.** Worth stating as a table,
  because the §7 review found the document had been treating the second column as if it were the
  first:

  | statement | how it is enforced | what it is worth against a determined plugin |
  |---|---|---|
  | a plugin never receives the engine's `rng` | **structural** — the object is never passed | real, but it is not the only route to it |
  | a plugin does not *reach* the engine's `rng` | **conventional**, detected by C3's four traps | catches the realistic vectors (module state, a planted `Random`, a `random.Random` class rebind, a frame walk to a local named `rng`); does **not** close `gc.get_objects()` |
  | a plugin does not write the run's config | `ReadOnlyConfig` is structural for the *reference it is handed*; the enforceable statement is the engine's before/after snapshot, which **refuses to write a manifest** if the config moved | the snapshot holds regardless of route; C13 is the pre-flight form of the same test |
  | a plugin's outcomes are in range | **structural** — `check_outcome` on every delivered link of a third-party model | complete for the declared fields |
  | a plugin's identity is locked | **structural** — `module_sha256` + `package_sha256` + `dist_sha256` + `interface_version`, all compared before step 0 | complete for on-disk source; a plugin that rewrites its own bytecode at import is out of scope |
  | a plugin does no I/O | **conventional**, detected by a PEP 578 hook | catches accidental I/O; a subprocess escapes entirely |

  The pattern: everything in the "conventional" rows is a *detection* layer, and the artifact-level
  statement — the pinned goldens plus the two-run digest gate — is what actually holds in all of
  them. That is not a weakness to be apologised for; it is the reason there are two layers.
* **The detector contract (D1-D7) does not exist.** It needs the frozen `Observation` DTO, which is
  phase 3. `conformance.runner.CONTRACTS` has one entry, and the CLI's `--slot` refuses anything else
  rather than pretending.
* **`scms-sim-api` is still one distribution with the engine.** See §0.
* **C12 runs one 30-second grid scenario.** It proves two runs of *that* configuration agree; it is
  not a sweep, and a plugin whose nondeterminism only appears at high density would pass it and be
  caught by C1 instead.

---

## 7. Adversarial review, 2026-08-31 — what did not survive it

Phase 2 landed without its verifier agent. What follows is the review's findings, each reproduced
before it was fixed, with the fix and the re-measurement. The transcription corrections are inline
in §2 and §3; these are the behavioural ones.

| # | Defect | Reproduction | Fix |
|---|---|---|---|
| 1 | **`RngNamespace.begin_step()` was called by nobody.** `_step` stayed `-1` for an entire run, so the documented "pure function of (seed, replicate, plugin, label, ids, **step**)" had a dead `step` term and the STATELESS stream degenerated to a per-run constant. | Instrumenting the certified acceptance run recorded `sorted(observed step) == [-1]` and 5854 distinct `fade` keys, **every one** ending `:s-1` (e.g. `17:plugin:rayleigh:fade:0:10:s-1`). The reference plugin's advertised per-packet Rayleigh fade was a fixed per-link offset for all 60 s: the channel never faded. C9 was grading a different model from the one the engine runs — measured on the same ladder, frozen vs advanced: PDR `1.000 1.000 1.000 1.000 0.969 0.885 0.792` against `1.000 1.000 0.979 0.948 0.844 0.698 0.615`. Freezing the fade flattens the whole curve and hides 0.18 of range loss at the far rung. | The **adapter** advances the namespace in `begin_step`, so the engine loop, `harness.trace`, C4 and C8 all get it from the one place a plugin cannot reach. `data_digest` for the demo plugin moves `9abe9eea… → be3b5dea…`; **no built-in touches `RngNamespace`, so no pinned golden moves.** The defect was invisible because **nothing asserted the step term was live**, so three regression tests now do, at the three levels it can be asserted at: `test_both_adapters_advance_the_rng_namespace_once_per_step` (both adapters, parametrised), `test_a_frozen_step_turns_a_per_packet_draw_into_a_per_link_constant` (the mechanism, as a number: 8 steps, 1 distinct value frozen vs 8 live) and `test_the_engine_gives_a_plugin_a_LIVE_per_step_rng_dimension` (a real `run_pipeline`, reading `ns.step` and the full key set back out of the plugin). Measured with the two adapter call sites reverted to their pre-fix form: `sorted(ns.step) == [-1]`, 3166 distinct stream keys over **1** step term, digest `cbda4553f26b42f2…`; with the fix, 40 distinct steps `0…39`, 30006 distinct keys over 39 step terms, digest `6a351ac1438ec67d…`. All three regression tests fail on the reverted tree; the **entire** conformance suite still passes 14/14 on it, which is exactly why the defect survived a review. |
| 2 | **The lock hashed one FILE.** A behaviour-changing edit to any sibling module of the same distribution passed `verify-plugins` clean and replayed to a different digest. | See §3, third defect. `verify-plugins` printed "no drift", exit 0, after +6 dB was added to an inherited `rayleigh.py`. | `package_sha256` (already present, previously **dead code**) is recorded and compared for third-party entries. Re-measured on the installed distribution (full transcript in §3): the same `+6.0` on `rayleigh.py` now gives `PLUGIN DRIFT: … package_sha256 expected '24eea74f5eeac91a…', got 'f3cf7cf12d03f2b6…'`, `verify-plugins` **exit 2**, replay **exit 2** with no output directory — while `module_sha256` (`a1db1687…`, `leaky.py`) and `dist_sha256` (`f045cf16…`, the RECORD) both stay put and are absent from the message. With the drift allowed, the dataset it would have produced is `6d5de639…` against the honest `85fb95ce…`: a real physics change, previously accepted at exit 0. |
| 3 | **`env["config"]` was the live mutable `PipelineConfig`.** `_write_manifest` serialises `cfg.__dict__` at the END of the run, so a plugin writing `env["config"].report_prob = 1.0` at step 30 silently rewrote the config the artifact claims produced it. | A plugin with physics identical to the reference, writing `report_prob = 1.0` at step 30 of a 60-step run: **exit 0**, valid manifest, `data_digest be3b5dea…` — **byte-identical to the honest control**, so the pinned-golden layer is blind to it — and `manifest["config"]["report_prob"] = 1.0`, not the `0.9` the config file asked for. Then the test that matters: replaying that manifest honestly, at the `1.0` it records, yields **`ea9d6da1…` ≠ `be3b5dea…`**. **The manifest does not replay to the dataset it describes** — D4's stated failure mode, with the lock in place and reporting clean. | Two layers. `env["config"]` is a `ReadOnlyConfig` view, so the write is a loud error at the moment it happens; and — the half that actually holds, since Python cannot make a reference unforgeable — `run_pipeline` snapshots the config before step 0 (deep-copied, so a nested `plugins[...]["params"]` write is caught too), re-checks it after plugin construction and again before the manifest write, and **refuses to write a manifest at all** if it moved. `manifest["config"]` is now that snapshot, never a late read of the live object. And a **third** layer, because the first two are engine-side and conformance is what a plugin author runs *before* producing an artifact: check **C13_config_not_mutated** — three arms (a write at construction, a write during the run, and a before/after dict comparison that does not care how the write was performed). Its scenario is twelve contiguous steps **plus a four-frame tail at step 10 000**, because the realistic shape of this bug is `if frame.step >= 30` and no short contiguous window can see that; one jump in the step label catches every threshold below it for the price of four frames. Verified against the out-of-repo `LateConfigMutator` (physics identical to the reference plugin, writes `report_prob = 1.0` at step 30): **13 passed, 1 failed — C13 alone**. It is the fourteenth row of the suite and the only numbered check added after phase 2. Re-measured end to end: the same `LateConfigMutator` config that used to exit 0 with a rewritten manifest now exits **2** with `PLUGIN REFUSED: env['config'] is READ-ONLY: a plugin may not write 'report_prob' (or any other field) on the run's configuration`, and **no manifest is written**. |
| 4 | **`check_outcome` — C7's runtime form — was called by nobody.** Conformance is off by default, so the default third-party path had no outcome validation at all. Its own bounds `[-200, 50]` dBm also disagreed with C7's `[-140, 0]`. | A plugin returning `LinkOutcome(rssi_dbm=9999.0, link_state="TELEPATHY")` on every delivered link completed at **exit 0** with a valid manifest; all **3012** rows of `ma/ma_reports.jsonl` carried `"rssi_dbm": 9999.0` and no other value (+9999 dBm ≈ 10⁹⁹⁷ W), and `link_state` carried a word outside the closed vocabulary. Re-measured with the gate restored, on the real engine: **exit 2**, `ConfigError: LinkOutcome.rssi_dbm out of range: 9999.0 … the declared bound is [-140.0, 0.0] dBm`, **no `manifest.json` written** and no `ma/ma_reports.jsonl`. Honest caveat: the failure can only be raised when a link is evaluated, so the output directory does exist with partial `ground_truth/` and `ma/` content — but with no manifest, which is what every downstream consumer keys on. | The adapter runs `check_outcome` on every delivered link of a **third-party** model (built-ins are graded by the goldens and this is a ~10⁷-link loop). The bounds are now **one** definition in `api.channel`, imported by C7 rather than restated. |
| 5 | **C3 detected only the two RNG attacks that are provably harmless.** Its two traps were a module-`random` state snapshot and a `Random` planted in `frame.env`. | Five models subclassing the reference with `plugin_id` forced to `rayleigh`, so physics and namespace are bit-identical and any movement is the ENGINE's stream. Control `be3b5dea…`, 3012 reports / 25 investigations / 25 CRL events / 67 emissions. Module `random.random()` → **fails C3**, digest **unchanged**. `random.seed(time_ns())` → **fails C3**, digest **unchanged**. `sys._getframe` walk to `run_pipeline`'s `rng` local → **old C3 clean** (module state and the `frame.env` probe both untouched), digest **moved** `433dd921…` (reports 3012→3000, emissions 67→64). `random.Random.random` rebound as a passthrough → **old C3 clean**, digest unchanged — a wrapper that only forwards consumes the same draws, so the danger is the capability, not the rebinding. The same rebinding biased ×0.5 → digest **moved** `9cdbed88…`, reports 3012→**46356**, investigations and CRL events 25→100, emissions 67→359. Under the new suite each of these fails **C3 and nothing else** (`tests/test_conformance.py`), and since `_attest` refuses only on `FAIL`/`ERROR`, a clean C3 was exactly what `conformance="required"` accepted. **The check was detecting exactly the two vectors that cannot reach the engine.** | Two more traps: the `random.Random` **class surface** compared against a baseline `run_contract` takes before any plugin code runs (an evasive patch is idempotent, so a snapshot taken inside C3 would already contain it), and the trace driven from a frame carrying a decoy local named `rng`. `run_contract` **restores** the surface in `finally` — a check that detects a tamper and leaves it installed is worse than no check. **Re-measured 2026-08-31 across eight hostile models with bit-identical physics** (`plugin_id` forced to `rayleigh`, same config, two runs each; control `be3b5dea…`): `H1_ModuleRandom` and `H2_TimeSeed` leave the digest **unchanged**; `H3_StackWalk` → **`433dd9218b3843d8…`**, `H4_MonkeyPatch` → **`9cdbed886a2469ab…`**, `H5_GcHunt` (re-seeds every `random.Random` it finds through `gc.get_objects()`) → **`6610a0adaebe6a56…`**; `H6_ImportEngine` and `H7_EnvConfig` unchanged. All five that touch a stream now fail **`C3_global_rng_untouched` and nothing else** (13 passed / 1 failed each); the three that do not are 14/14. Honest limit, unchanged and now sharpened: `H5` is caught because it sprays *every* `Random` in the process, including the module-level one — a gc walk that identified the engine's instance and touched only that would evade all four traps. Capability-by-omission is structural against the accidental case and not enforceable against a determined one; the artifact-level statement (pinned goldens + two-run digest) is what actually holds. |
| 6 | **C9 could not fail a model that is constant in distance**, and compared only adjacent rungs with 0.10 PDR / 1.5 dB slack — never first rung against last, so a monotone rise of 0.60 PDR and 9 dB across the ladder was invisible by construction. Two models, both graded on the real ladder. A flat 50 % delivery probability with a constant −70.0 dBm at 25 m **and** at 475 m: PDR `0.500` at all seven rungs, 0.0 dB of attenuation — **passes the old C9**, because every arm was a `<=` comparison and equality satisfies all of them. A model whose PDR creeps up by exactly 0.08 per rung (`0.302 … 0.781`, **+0.479 end to end**) while its rssi falls honestly: every adjacent step inside the 0.10 tolerance, so it **also passes the old C9**. Control (the reference plugin) `1.000 … 0.615`, 27.2 dB of attenuation. | Three arms: adjacent (unchanged), **cumulative** (first rung vs last, bounded by ONE slack, not six), and **attenuation** (over a 19× distance ratio the model must fall by ≥ 3 dB, or ≥ 0.05 PDR if it reports no rssi — against ≈ 21 dB for the shallowest path-loss exponent any `FieldSpec` here admits). `disc` declares a waiver, because it is the deliberate unit-disc idealisation and fails that arm **by definition**. Two new fixtures in `tests/test_conformance.py` grade the two new arms and must fail C9 **alone**. |

**What this changes about the phase-2 claim.** Items (a)-(e) still hold: an out-of-repo distribution
passes 14/14, reproduces across two processes, is made unreplayable by one mutated byte, is caught by
C1 when it reads a clock, and fails only C6b when it reads the oracle. The digests for the two
plugin-configured runs moved (defect 1), the leak in (e) was over-claimed (see the correction there),
and three of the mechanisms the document presented as working — the step term in the RNG key,
`check_outcome`, `package_sha256` — were **not called by anything**. That is the pattern worth
carrying forward: a function that exists, is tested in isolation and has no caller reads exactly like
a working defence in a review.

**And the second-order lesson, which is about this document rather than the code.** Three lock
digests here were transcribed from a different install state than the artifacts they labelled
(§2 (b), §2 (d), §3), and one narrative claim — that `leaky_a`/`leaky_b` contain laundered labels —
was false of the very artifacts it sat beside (§2 (e)). None of the four was caught by any test,
because a document is not executable. The rule adopted in response: **every digest in this file is
either read back out of the artifact it names, or re-measured by a command printed next to it.**
Where a value depends on something the document does not pin down — the `got` side of a drift
message depends on the exact edit text — the edit is shown, or the value is not quoted as evidence.

**Gates after the review.** Full suite **922 passed in 992.65 s**, exit 0 — 833 before phase 2's own
run, 912 after the review's first pass, and **+10** for the C13 check and the three CRITICAL-1
regression tests added when this section was verified independently. The reference run
(`--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15 --traffic-lights
--seed 42`) still digests
`b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` — measured again after every
change in this section — with every count and metric unchanged, all 8 pinned goldens hold, and
`tools/verify_data.py` over the seven acceptance datasets is `PL1`-`PL5` clean with the one expected
`PL6` failure on `rayleigh_d` (the deliberate `--allow-plugin-drift` artifact).

Nothing here moves a **built-in** digest: no built-in channel model touches `RngNamespace`,
built-ins are exempt from `package_sha256`, and `check_outcome` runs only on the third-party path.
Digests of runs that DO declare a plugin move, and that is the correct outcome rather than a
regression — defect 1 means every such digest recorded before 2026-08-31 was produced by a channel
whose "per-packet" randomness was a per-link constant. The two that are pinned in this document are
re-stated at their new values (`9abe9eea… → be3b5dea…`, `2ea1338d… → 85fb95ce…`).
