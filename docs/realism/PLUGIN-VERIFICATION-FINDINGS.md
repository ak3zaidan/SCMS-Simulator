# Adversarial verification of the plugin seam — findings

Run after the seam shipped without review. **The seam functions — plugins load, run, and require zero
engine edits — but four of its safety properties were not actually enforced.** Every finding below
was measured with reproducible digests, not inferred.

## Critical

**1. The per-step RNG dimension is dead.** `RngNamespace.begin_step()` (`api/rng.py:81`) is never
called anywhere in the tree — the only `begin_step` callers reach the *model*, never the namespace.
So `_step` stays at −1 for an entire run and the documented *stateless* stream collapses to a
per-run constant: every observed key ends `:s-1`, and `sorted(observed step) == [-1]` across 5854
distinct keys. Consequence: the reference plugin's advertised per-packet Rayleigh fade is a **fixed
per-link offset for the whole run — the channel never fades**, visible as PDR quantised to exact
twelfths on a 96-sample ladder. Conformance checks C1/C2/C9/C11 were comparing traces in which the
step dimension carried no information.

**2. The provenance lock hashes one file, not the distribution.** `verify_lock` compares
`module_sha256` (`registry.py:162`, the plugin class's defining file only). Measured: a plugin
defined in `leaky.py` inheriting its physics from sibling `rayleigh.py`; editing `rayleigh.py` to add
**+6 dB of transmit power** left `verify-plugins` printing *no drift* at exit 0, while the manifest
replayed to a different digest (`2ea1338d…` → `97456bb5…`). That is decision D4's stated failure mode
occurring *with the lock in place*. `package_sha256` (`registry.py:192`) already exists and would
close it — it is dead code, never called.

**3. The evidence document's digests do not reproduce.** `PLUGIN-CONFORMANCE-EVIDENCE.md:131` states
`dist_sha256 = 4a99e611…` and `:137` `provenance_digest = b7d95b69…`; the actual artifact records
`f045cf16…` and `91397ec9…`, reproduced on four fresh runs and stable across a force-reinstall.
Recomputing the provenance digest over the artifact's own lock entry with *only* `dist_sha256`
replaced by the document's value yields `b7d95b69` exactly — proving the document transcribed a
different install state as if it were this run's.

**4. Plugins receive the live mutable config.** `run.py:957` sets `env['config'] = cfg` — the actual
dataclass, not a copy or a read-only view (contrast `run.py:2588`, which correctly uses
`MappingProxyType`). Measured: a plugin setting `cfg.report_prob = 1.0` at step 30 ran to exit 0;
because `_write_manifest` serialises `cfg.__dict__` at the *end* of the run, the manifest recorded
1.0 rather than the 0.9 the user asked for; the replay produced a different digest; `verify-plugins`
reported OK; and the plugin passed all 13 conformance checks plus attestation.

## Major

**5. The RNG conformance check fails the harmless cases and passes the harmful ones.** `C3` has two
traps: a `random.getstate()` snapshot and a planted `random.Random`. Measured across seven plugins
with bit-identical physics — module-level `random.random()` and `random.seed(time.time_ns())` both
**fail C3 while leaving the engine digest unchanged** (harmless: the engine's rng is a private
instance). Meanwhile a `sys._getframe` walk to `run_pipeline`'s `rng` local and a
`random.Random.random` monkeypatch both **pass 13/13 and move the digest** (`c9535d59…`,
`a8dbf914…`), perturbing report probability, revocation and emit sampling — precisely the components
decision D3 names.

**6. `check_outcome` is never called.** `api/channel.py:451` is C7's runtime form, but `run.py`
imports only `sort_outcomes`. A plugin returning `LinkOutcome(rssi_dbm=9999.0,
link_state='TELEPATHY')` completed at exit 0 with that value in the MA-visible dataset.

## What this changes

Claims made before this verification that must be withdrawn or qualified:

- *"Plugin identity is content-hash locked so a replay detects drift"* — true only for the class's own
  file. A sibling-module edit in the same distribution passes clean.
- *"A plugin never receives the global rng"* — structurally true for the **accidental** case, and
  false for a **deliberate** one. Capability-by-omission is a real defence against a plugin that
  merely calls `random.random()`; it is not a defence against one that walks stack frames.
- *"The conformance suite proves a plugin is safe"* — it proves a plugin is *well-behaved*, and its
  RNG check was measuring the wrong thing entirely.

The functional claim survives: a third-party channel model does run with zero engine edits, and the
no-plugin reference digest is unaffected by all of this.

## Round 2 — after the fixes (2026-08-31)

**Four of six closed, independently re-measured. Two still open, and one of them is an attack the
fix report declared fixed.**

Closed and re-verified by a separate agent that reverted each fix and measured before/after:

- **Critical 1** — the step term is live: fade variance 28.42 dB² across 4026 links, zero constant
  series (previously *every* series was constant). The C9 PDR ladder moved from
  1.000/1.000/1.000/1.000/0.969/0.885/0.792 to 1.000/1.000/0.979/0.948/0.844/0.698/0.615 — the
  channel now actually fades. **The decisive measurement:** with the fix reverted, the conformance
  CLI still reported 14 passed / 0 failed and the whole 49-test conformance file stayed green; only
  the three newly-written regression tests failed. That is precisely why the defect survived review.
- **Critical 2** — `package_sha256` is wired: the sibling +6 dB edit now takes `verify-plugins` from
  exit 0 to exit 2, naming `package_sha256` alone (module and dist hashes provably unmoved), and the
  replay exits 2 with no output directory.
- **Critical 4** — five config-mutation vectors, including the behind-the-view and nested-dict ones,
  all exit 2 with no manifest; the new `C13_config_not_mutated` check fails alone on a violator.
- **Critical 3** in substance — every artifact-derived digest in the evidence document now reads back
  correctly and its commands re-run.

### Still open

**Attestation runs after construction.** `build_channel` instantiates the plugin
(`run.py:1169`) *before* calling `_attest` (`:1188`). A hostile plugin's `__init__` therefore runs
before it is ever attested, so it can install whatever it likes and then pass the gate. The detector
slot repeats the identical ordering defect (`run.py:1493` instantiates, `:1499` attests).

**A time-delayed attack evades every conformance trap.** A `random.Random` class rebind installed at
`frame.step >= 30` passes all four C3 traps, because conformance exercises a bounded window and the
attack simply waits it out. This is not a hole in the traps; it is a property of any
fixed-window contract test. **A conformance suite can only ever certify behaviour it observed.**
The mitigations are runtime monitoring or a real out-of-process boundary — not a longer window.

Also open: `check_outcome` (`api/channel.py:485`) validates by reading the caller's object and
returning it unchanged, so an outcome whose `rssi_dbm` is a stateful property can return one value
to the checker and another to the engine — a time-of-check/time-of-use gap.

And the evidence document has recurrence of its own failure mode: two digests in the sibling-edit
section do not reproduce from the diff the document itself prints, and one only reproduces because
the acceptance script leaves a `.orig` backup **inside site-packages**, contaminating the package
hash it is meant to verify.

## Round 3 — the whole catalogue re-run against HEAD with fresh external code (2026-09-06)

Ten attack families, re-implemented from scratch as a plugin distribution **outside this repository**
(`C:\Temp\adv\ext`, on `sys.path`, never installed), each measured with a digest against an honest
control on `seed=17, grid 5×5, 40 s, arrival 1.5, attacker 0.30`. Control digest
`2d73c3fb2ab6b87e…`.

### Eight of ten do not land. Each was independently confirmed by reverting its fix.

| # | family | verdict at HEAD | measured |
|---|---|---|---|
| 4 | module-level tamper on the no-run import paths | **BLOCKED** | `verify-plugins`, `--check-config` and the run path all raise `INTEGRITY FAILURE while IMPORTING the check plugin (module-level code)`; `registry.resolve`'s own `Sentinel` fires at the import that performed it |
| 5 | attestation verdict forgery (`atexit` rewriting the child's report + writing JSON on fd 1) | **BLOCKED** | the plugin genuinely failed `['D1_pure','D2_reads_only_ma_visible','D3_label_invariance','D4_firing_convention','D5_monotone_in_attack_magnitude','D7_state_namespacing']` and the run was refused; the forged `{"passed": 99, "ok": true}` reached the parent's stdout and was **not read** — no framing token |
| 6 | cross-package sibling drift | **BLOCKED** | the edit moved the digest `86877363` → `49442da0` with `module_sha256` unmoved and `package_sha256` null, and `import_closure_sha256` moved `b117e35a` → `4c3ff7cf`; `verify-plugins` exit **2** |
| 7 | mutating methods **of** a watched class (obfuscated `getattr(sys,"mod"+"ules")`) | **BLOCKED** | `INTEGRITY FAILURE at the END of the run: 3 watched object(s)`, naming `WitnessedRandom.verify_stream`; no manifest |
| 9 | isolated-mode `__file__` decoy | **BLOCKED** | `IsolationError: the isolated worker reported that it loaded the plugin from <decoy>` — the parent resolves the path itself and refuses the mismatch |
| 10 | lying codec (fabricated `wire_size_bytes`, non-decoding `evidence_pdu`), `net_latency_model` on | **BLOCKED** | `the CAM it emitted does not decode -- decode_cam() raised ValueError`; refused before any manifest |
| — | `random.Random.random` rebind at `__init__` | **BLOCKED** (pinned) | `INTEGRITY FAILURE while LOADING a detector plugin` |
| — | delayed rebind of a watched object, **step-gated and wall-clock-gated** | **BLOCKED** (pinned) | both: `INTEGRITY FAILURE at the END of the run`, no manifest. The wall-clock form is newly pinned — a step-gated attack invites "drive conformance longer", a clock-gated one closes that argument off |
| — | isolated child hunting **this run's** streamed ground truth | **BLOCKED** (pinned) | child found `env_hits=[]`, `cwd_entries=[]`, no `ground_truth/` for this run |

Every one of those has a regression test that **fails when the fix is reverted** — measured by
reverting each in turn: F7 → 2 failed, F4 → 1 failed, F6 → 1 failed, F10 → 3 failed, F5 → 1 failed,
F9 → 2 failed.

### Two land, and one of them is not closable in this process

**1. The frame walk, taken while the plugin is being LOADED.** `api/guard.py` refuses
`sys._getframe` from inside CPython however it is spelled — but it is armed around the calls the
engine makes **into** a plugin. A plugin's module body and its `__init__` are not calls, and
`frame.f_back` / `frame.f_locals` raise no audit event at all. So:

```
capture in evaluate()  -> BLOCKED  (IntegrityError: PLUGIN GUARD REFUSAL, sys._getframe)
capture in __init__    -> LANDS    {"broadcast_dict": "run_pipeline", "locals:vehicles": "run_pipeline"}
capture at module body -> LANDS    {"is_attacker": true, "true_xy": [466.4527993807002, 120.0]}
```

Both landing forms ran to completion at the **default** `source_gate` and wrote a dataset. This is
not closable: arming the hook over a plugin's module body would refuse `collections.namedtuple`,
`typing.NamedTuple` and `logging` — what honest authors do there — and would still leave metaclasses,
`__set_name__`, descriptors and properties open.

**Action taken: documented, not claimed.** `api/guard.py` gains a fourth named limit and a measured
section; `api/srcgate.py` and `api/integrity.py` had their "closed however it is spelled" sentences
**withdrawn** as false; `DETECTOR-PLUGIN.md` §2.3 gains a runtime-guard row and §2.4.1;
`PLUGIN-ARCHITECTURE.md` §10.1 states the posture. Pinned by
`test_the_frame_walk_STILL_LANDS_when_it_is_taken_while_the_plugin_is_LOADED`, which **passes by the
attack succeeding**.

**2. Drawing from the frame-walked global RNG — CLOSED, in the half that is arithmetic.** The same
walk reaches `run_pipeline`'s `rng`. Reading it is a read; drawing from it is an act. The engine
makes no global-stream draw while it is handing control to a plugin's loader or constructor, so the
stream is now closed for the whole plugin-loading phase as well as for every guarded call
(`integrity.stream_closed`).

| form | before | after |
|---|---|---|
| draw in `evaluate` | refused (guard armed) | refused |
| draw in `__init__`, refusal swallowed | **complete clean run**, digest `319bb0bd` vs honest twin `192286db`, `verify_stream` agreeing, manifest written | `IntegrityError ... SWALLOWED the refusal`, no manifest |
| draw in `evaluate`, refusal swallowed | run completed | `28835 draw(s) ... were REFUSED`, no manifest |
| honest twin (walks, does not draw) | `192286db` | `192286db` — the closure costs an honest plugin nothing |

### One CLI defect found on the way

`verify-plugins` detected the family-4 module-level tamper correctly and then let the
`IntegrityError` escape as an **unhandled traceback at exit 1** — which is that command's code for
"the manifest file was unreadable". A CI gate keyed on the exit code therefore filed an actively
hostile plugin under "bad JSON". It now reports `PLUGIN INTEGRITY REFUSAL` at exit **2**, and
`--allow-drift` does not downgrade it.

### Pinned digests

`0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740` (default golden) and
`b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` (reference run) both reproduce
byte-identically after all of the above.

## Lesson

Both of these landed because their verifier agents were lost to a session limit and the work was
committed on implementer report alone. The two earlier failures in this project had the same shape:
a GEH gate that graded a simulation against itself, and a seed-stability gate too loose to catch a
2× regression. **Nothing here counts as done until an independent agent has re-run the attack.**

Round 3 adds a second rule, learned from the two withdrawn containment claims and from the
"however it is spelled" sentence withdrawn above: **a limitation must be pinned by a test that
passes by the attack succeeding.** A limitation asserted only in prose becomes a claim the first time
someone paraphrases it.
