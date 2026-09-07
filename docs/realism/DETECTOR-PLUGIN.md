# Writing a detector plugin — the thresholding guide

**Status:** phase 3 shipped and measured. Every command, digest, exit code and metric below was run
on this host (Windows Server 2022, CPython 3.12.10, `PYTHONHASHSEED=0`) against an out-of-repo
distribution installed as its own wheel. Nothing here is illustrative.

**Full engine suite: `995 passed in 1025.57s`, exit 0.** Reference digest
`b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` reproduced with the plugin
distribution installed and with no plugins declared. Reproduce the functional evidence with
`C:\Temp\scms_detector_demo\ACCEPTANCE.ps1`.

> **The out-of-process boundary now exists.** `"isolated": true` on a `check` entry runs it in its
> own interpreter, where the run's ground truth is not present at all —
> [**§2.8**](#28-isolated-mode--the-process-boundary-and-the-only-answer-for-code-you-cannot-review)
> is the section for a detector you did not write. Same seed, same scores, same `data_digest`;
> ~40 µs per delivered message; the hostile frame-walking detector that files 1 910 label-derived
> reports in process files **zero** there. What it does not close is measured in the same section.
>
> **Corrected 2026-09-02.** The first version of §2.8 said this run's own oracle files were
> unreadable while the loop ran; that was false — they were **streamed**, and an isolated detector
> was measured reading them. The engine now withholds every ORACLE output until the last worker has
> been reaped, so the sentence is true of the code and not only of the intent. `ISOLATION-ORACLE-LEAK.md`
> is the whole diagnosis, fix and measurement.

> **Corrected 2026-09-06, after an independent re-run of the whole attack catalogue with plugin code
> written outside this repository.** Eight of ten families no longer land (see
> `PLUGIN-VERIFICATION-FINDINGS.md` round 3). The one that does is the **frame walk taken while the
> plugin is being LOADED** — `api/guard.py` refuses `sys._getframe` from inside CPython, but it is
> armed around the calls the engine makes *into* a plugin, and a module body and an `__init__` are
> not calls. Measured at the default `source_gate`: `b["veh"].is_attacker → True`,
> `b["x"], b["y"] → (466.4527993807002, 120.0)`, run completed, dataset written. It is **not closable
> in this process** and is documented as a limitation rather than papered over —
> [§2.4.1](#241-the-runtime-guard--and-the-window-it-does-not-cover). Two sentences elsewhere in this
> project claiming the walk was "refused however it is spelled" have been withdrawn. **In-process
> plugins are TRUSTED CODE.**

> **The safety claim in this document was wrong until 2026-08-31 and is now stated correctly.**
> An earlier revision of §1.2 called the `Observation` boundary "a boundary you cannot walk around".
> It is not, it cannot be for any in-process plugin, and a detector reading the labels through
> `sys._getframe(1).f_locals["b"]` was measured doing exactly that on this engine. **[§2 is the
> trust model](#2-the-trust-model--read-this-one) and is now the section to read first.** The
> functional claims — zero engine edits, the threshold sweep, the lock, the drift gate — were
> independently verified and are unchanged.

**What this document answers:** *"I have my own misbehaviour detector — a thresholding scheme — how
do I run simulations with it without forking the simulator?"* And, from §2 on: *"what is that
detector allowed to see, really?"*

The short answer, in full:

```powershell
pip install ./my-detectors
python -m scms_sim_ref.mock_pipeline.run --config my_run.json
```

```jsonc
// my_run.json
"plugins": {
  "check": ["@builtins",
            {"ref": "my_detectors.threshold:MyThreshold",
             "params": {"tolerance_mps": 3.0},
             "conformance": "required"}]
}
```

That is the whole integration. No edit to `run.py`, no edit to `featurize.py`, no edit to any test
file — [proven below](#6-the-acceptance-test-zero-engine-edits) by hashing all 64 of those files
before and after a run that emits the plugin's column.

---

## 1. The shape of the seam

The detection layer is split three ways, copied from F2MD's decomposition (`mdChecks/` →
`mdApplications/` → `mdReport/`) and **not** from its registration, which is an integer-indexed enum
plus a `switch`:

| slot | what it does | how many run |
|---|---|---|
| **`check`** | scores one received message on one criterion → a `float` | an ordered vector |
| **`fusion`** | turns the score vector into a report decision | exactly one |
| `report_format` | serialises the decision — **not yet implemented** | — |

Your thresholding scheme is a **`check`**. The engine ships 15 of them (13 always on, 2 behind
feature gates); yours is appended to that vector, or replaces it entirely.

### 1.1 The polarity, which is the one thing people get wrong

> **`detnorm >= 1.0` means VIOLATING.** `0.0` means perfectly consistent. The value is a
> confidence-normalised residual, unbounded above, and `1.0` is the firing point.

**This is the opposite of F2MD.** F2MD's factors are `[0, 1]` where **LOW means implausible**, and
its `ThresholdApp` fires on `getFactor() <= threshold`. A check ported from F2MD without inverting
it scores ~1.0 on a perfectly honest message and ~0.0 on a blatant teleport: it never crashes, it is
never out of range, and every report it produces is exactly backwards.

Conformance check `D4` refuses that, by grading an **honest** observation:

```
FAIL   D4_firing_convention   a self-consistent, correctly-signed, in-range, currently-valid claim
                              scored 1.0 >= 1.0 -- either the check is inverted (F2MD's [0,1]
                              LOW-means-implausible convention, which must be inverted for this
                              engine) or it fires on every honest message
```

### 1.2 What your check is handed — and what is not in it

Your `evaluate` is handed a sealed, slotted `Observation` with 34 fields:

* **the claim** — `cert_digest`, self-declared `station_type`, `claimed_x/y/speed/heading`,
  `pos_conf`, `gen_time`, `msg_count`, `msg_type`, `event_type`, `sig_ok`, `cert_valid_from/to`;
* **the receiver's own measurements** — `rx_x/y` (its GNSS fix), `rx_reach_m` (the channel model's
  *declared* reach), `rssi_dbm`, `link_state`, `t`, `dt`;
* **the per-sender history this receiver already holds** — `first_sight`, `ref_*` (a lagged
  reference fix), `prev_*` (the one-step baseline);
* **derived and aggregate context** — `map_offroad_m` (the receiver's own HD map applied to the
  *claimed* position), `neighbourhood` (e.g. `cell_cert_count`, `cbr`).

It carries **no ground truth**. No `veh` handle, no true position, no `falsified`, no `is_attacker`.
There is no `__dict__` to rummage through (`slots=True`), nothing can be attached to it at runtime,
and no declared field accepts a write through any path reachable by name — `obs.claimed_x = v`,
`setattr(...)` and `object.__setattr__(...)` all raise. That last one matters because **one
`Observation` is shared by every check in the vector**: without it the first plugin in the array
could rewrite the claim the built-ins and every later plugin then score.

You also never receive the engine's global `random.Random(cfg.seed)`. Its draw *count and order* are
load-bearing for every pinned digest, so it is withheld. You get a seeded, namespaced `RngNamespace`
instead. Your `state` is a `NamespacedState` whose mapping interface lands in
`st["plugin:<your_id>"]`; the engine's own `h` / `streak` / `touch` / `kf` keys are readable and
**raise** on write. A third-party **fusion** gets the same wrapper as a third-party check.

**None of that makes a hostile plugin harmless, and section 2 says exactly why.** Read it before you
build anything on top of this seam.

---

## 2. The trust model — read this one

### 2.1 The one-sentence version

> **An in-process detector plugin is TRUSTED code.** It runs inside the engine's interpreter with
> the engine's privileges, exactly like any other installed dependency. The `Observation` boundary
> stops *accidental* leakage and makes *deliberate* leakage deliberate, reviewable and detectable.
> It does not — and cannot — stop a plugin that means to read the labels. **A detector you genuinely
> do not trust runs with `"isolated": true`** ([§2.8](#28-isolated-mode--the-process-boundary-and-the-only-answer-for-code-you-cannot-review)),
> in its own interpreter, where the labels are not present — same scores, same seed, same digest,
> about 40 µs per message.

### 2.2 The vector, stated rather than left for you to find

This is the whole reason the section exists. From inside a `Check.evaluate()`, one line reaches the
reception loop's own stack frame, and the reception loop is holding the oracle:

```python
def evaluate(self, obs, state, params, rng):
    b = sys._getframe(1).f_locals["b"]      # the engine's broadcast dict
    ...
```

Measured on this engine (grid 5×5, 40 s, seed 17), that `b` yields:

| expression | value |
|---|---|
| `sorted(b)` | `['cg','ch','conf','cs','cvf','cvt','cx','cy','digest','falsified','ghost','msg_count','sig_ok','station_type','thdg','tspd','veh','x','y']` |
| `b["veh"]` | `<class 'scms_sim_ref.mock_pipeline.run.Vehicle'>` — with `.is_attacker`, `.attack_type`, `.victims` |
| `b["veh"].is_attacker` | `True` |
| `b["veh"].attack_type` | `'ConstPosOffset'` |
| `b["x"], b["y"]` | the sender's **true** position (`466.45, 120.0`) |
| `b["falsified"]`, `b["ghost"]` | the per-message oracle flags |
| `sys._getframe(1).f_locals["rng"]` | the engine's single global `random.Random` |

A detector that scored `b["veh"].is_attacker and b["falsified"]` fired on **1878 of 2297 reports**.
And it was *perfectly reproducible while doing it*: it passed every digest check, the content-hash
lock, the two-run equality gate and the pinned goldens, because reading the oracle is deterministic.
**Every reproducibility layer in this project is structurally blind to label leakage.** The same
vector exists on the channel slot and was found there independently.

`gc.get_referrers(obs)` reaches the same dict without touching a frame. `from
scms_sim_ref.mock_pipeline import run` reaches it without any reflection at all.

**Both of those spellings, and the one above, are now refused at run time — *if they are executed
inside a call the engine made into the plugin*.** That is the whole of what
[§2.4.1](#241-the-runtime-guard--and-the-window-it-does-not-cover) buys, and it is why the same walk
moved one line up the file rather than disappearing. Taken in the plugin's **module body** — code
`registry.resolve` executes when it imports the module, where no guard is armed and where
`f_back`/`f_locals` raise no audit event at all — the identical capture is a live handle on
`run_pipeline`'s locals for the whole run, and re-measured against HEAD it still returns
`is_attacker = True` and `(466.4527993807002, 120.0)` from inside `evaluate`. Read §2.4.1 before you
form a view about what the gate and the guard are worth.

### 2.3 Why this cannot be fixed in-process, and what was done instead

Python gives every callable in a process the same reflective powers: `sys._getframe`,
`inspect.currentframe`, `gc.get_objects`, `gc.get_referrers`, module globals, `__subclasses__`,
`__closure__`, `ctypes`. No arrangement of frozen dataclasses, wrapper objects or namespaced
mappings changes that, and stacking more wrappers only makes a false claim harder to disprove. So
the engine does four things that *are* true, and claims nothing more:

| layer | what it actually guarantees | what defeats it |
|---|---|---|
| **`Observation`** | no ground truth reachable **by name**; nothing attachable; no declared field writable via `setattr` / `object.__setattr__` / `del` | `type(obs).claimed_x.fget.__self__.__set__(obs, v)` — the saved slot descriptor |
| **`NamespacedState`** | the mapping interface lands in `plugin:<id>`; reserved keys raise on write; `h` comes back a tuple and `streak` a read-only proxy over a **copy** | `object.__getattribute__(ns, "_st")` — the wrapper holds the engine's dict |
| **the source gate** | refuses frame walking, `gc` reflection, trace hooks, `ctypes`, `eval`/`exec`, and engine-internal imports, at load, by line number | `getattr(sys, "_get" + "frame")`; an `eval`; a helper module the gate does not parse |
| **the runtime guard** ([§2.4.1](#241-the-runtime-guard--and-the-window-it-does-not-cover)) | while the engine is *calling into* the plugin, `sys._getframe`, `sys._current_frames`, the `gc` walkers, trace/profile installation and `ctypes` are refused **inside CPython**, so the spelling does not matter | **the guard is armed around plugin CALLS, not around plugin LOADING** — one `sys._getframe` in the module body or in `__init__` is not refused, and `f_back`/`f_locals` are audited by nothing, so the captured frame stays readable for the whole run |
| **the integrity monitor** (§2.6) | the RNG primitives, the engine's own gates and the boundary classes are the **same objects** at the end of the run that they were at the start, and the engine's stream advanced exactly as many words as it drew | a plugin that tampers, uses the tamper, and puts the binding back **before the next checkpoint**; and everything that moves nothing at all — a frame walk moves nothing |

All four residues are **pinned by tests** (`tests/test_detector_trust_boundary.py`,
`tests/test_plugin_runtime_guard.py`, `tests/test_plugin_integrity.py`), so if a future change closes
one, the test fails and this table has to be upgraded rather than left overstating. The guard's
residue is pinned by a test that **passes by the attack succeeding**
(`test_the_frame_walk_STILL_LANDS_when_it_is_taken_while_the_plugin_is_LOADED`) — the only way to
stop a limitation quietly turning into a claim.

The honest boundary is a process boundary: a detector in its own process, with an explicit message
interface, where the oracle is simply not in the address space — **and, since the address space was
never the only place the oracle lived, not on the disk either while that process is alive.** Both
halves exist now — `"isolated": true`,
[§2.8](#28-isolated-mode--the-process-boundary-and-the-only-answer-for-code-you-cannot-review),
measured on the identical hostile class this section describes: in process its frame walk reaches
`run_pipeline`'s locals and it files 1 910 reports built out of `is_attacker`; isolated it reaches
seven frames of the serialiser, finds no `ground_truth/` directory to open, and files none. For
anything you run **in** process, the question "who wrote this detector?" is still the same question
as "who wrote this dependency?".

### 2.4 The source gate

At plugin resolution — before construction, before step 0, before an output directory exists — the
engine parses the module the plugin class is defined in and refuses a list of constructs:

Verbatim, on a 14-line plugin whose `evaluate` is the two lines from §2.2:

```
plugins.check 'demo:FrameWalker': REFUSED by the source gate.
  source: C:\Temp\gatedemo\demo.py
  line 13: f_locals
      frame.f_locals -- reads another function's local variables
  line 13: sys._getframe
      sys._getframe() -- walks to the engine's own stack frame, whose locals carry the broadcast
      dict (veh/.is_attacker, true x/y, falsified, ghost) and the global rng

WHY THIS IS REFUSED. An in-process detector runs inside the engine's interpreter, so the
constructs above reach the engine's own stack frame -- whose locals hold the broadcast dict
(the Vehicle with .is_attacker/.attack_type, the sender's TRUE x/y, `falsified`, `ghost`)
and the global RNG. A detector that reads those is scoring the labels it is supposed to be
predicting, and it stays perfectly deterministic while doing it, so no digest, golden or
content hash in this project can see it.

WHAT THIS GATE IS. A guard rail, not a sandbox. It parses the ONE module the plugin class is
defined in and matches names; it is defeated by getattr(sys, '_get' + 'frame'), by an eval,
by a helper module it does not parse, or by anything resolved at run time. It exists to make
the reach an explicit act rather than an accident, and to make it reviewable. If you need a
detector you genuinely do not trust to be UNABLE to read the labels, run it out of process;
in-process plugins are TRUSTED code, on the same footing as any installed dependency.

IF THIS IS YOUR OWN CODE and the construct is legitimate, say so in the config -- it is
recorded in the manifest and it replays:
    "plugins": {"check": [..., {"ref": "demo:FrameWalker", "source_gate": "off"}]}
See docs/realism/DETECTOR-PLUGIN.md section 2 (the trust model).
```

What it refuses:

* `sys._getframe`, `sys._current_frames`, `inspect.currentframe`, `inspect.stack`, `inspect.trace`,
  `inspect.getouterframes` / `getinnerframes`, and the frame attributes `f_locals`, `f_globals`,
  `f_back`, `tb_frame`, `gi_frame`, `cr_frame`;
* `gc.get_referrers`, `gc.get_referents`, `gc.get_objects`;
* `sys.settrace`, `sys.setprofile`, `threading.settrace`;
* `sys.modules` and `__import__` — reaching an already-imported engine module by string, with no
  import statement for the rule below to see;
* `ctypes`;
* `eval`, `exec`, `compile` — not because they are attacks, but because they make the rest of the
  file unanalysable, and a reviewer should see that;
* any import of `scms_sim_ref.*` other than `scms_sim_ref.api` and `scms_sim_ref.conformance`.
  `import scms_sim_ref.mock_pipeline` reaches the oracle with no reflection at all.

Every finding is reported at once, in line order, each with what it is and why — a gate that stopped
at the first hit would just make an author iterate.

It runs on **both** the `check` and the `fusion` slot. Built-ins are exempt: they *are* the engine.
A module whose source cannot be read is **refused**, not waved through — "the gate saw nothing" and
"there is nothing to see" are different statements.

**The explicit opt-out**, for code you wrote yourself or have audited:

```jsonc
"plugins": {
  "check": ["@builtins",
            {"ref": "my_detectors.threshold:MyThreshold", "source_gate": "off"}]
}
```

It is a config key, not a flag, so it replays and it is written verbatim into
`manifest["config"]["plugins"]`. A dataset built with the gate disabled says so in its own manifest.
`"source_gate"` accepts `"on"` (default) and `"off"`; anything else is an error, never a silent
default.

### 2.4.1 The runtime guard — and the window it does not cover

The gate above is a **static name match**, and its own error message says what that costs:
`getattr(sys, "_get" + "frame")` is the same walk with the name assembled at run time and no gate
that parses source can see it. `api/guard.py` answers the spelling half where spelling has already
stopped existing — a **PEP 578 audit hook**, which CPython raises from inside `sys._getframe` itself,
so `sys._getframe`, `getattr(sys, "_get"+"frame")`, `eval("sys._getframe")`, an alias captured at
import and a helper module the gate never parsed all reach one refusal. It also refuses
`sys._current_frames`, `gc.get_objects` / `get_referrers` / `get_referents`, `sys.settrace` /
`setprofile` and `ctypes.*`. The refusal derives from `BaseException` (so `except Exception:` cannot
swallow it) **and is counted** (so `except BaseException:` buys nothing either — the count is
compared across the call and a move fails the run with no manifest).

`"source_gate": "off"` turns off the static gate **and** this guard for that one plugin, so "code I
wrote or audited" stays one recorded decision rather than two.

#### The window, stated plainly, because it is the whole trust model

**The guard is armed around the calls the engine makes INTO a plugin** — `evaluate`, `decide`,
`encode_cam`, `wire_size_bytes`. It is **not** armed around:

* the plugin's **module-level code**, which `registry.resolve` executes when it imports the module;
* the plugin's **`__init__`**;
* `capabilities()`, `config_fields()`, and any attribute or property the engine reads off the
  instance while loading it.

It cannot be. An honest module body calls `collections.namedtuple`, `typing.NamedTuple` or `logging`
above the enabled level, and every one of those calls `sys._getframe` — arming the hook there would
refuse honest code, and it still would not close anything, because a metaclass, a `__set_name__`, a
descriptor or a property is another unguarded entry point and enumerating them all is exactly what
this project has said twice now that it cannot do.

And **`frame.f_back` and `frame.f_locals` raise no audit event at all**. So one `sys._getframe`,
performed where nothing is listening, yields a live handle on the engine's own frames for the rest of
the run. Measured against HEAD, at the **default** `source_gate`, with a plugin written outside this
repository:

```python
# module scope of a third-party check -- runs inside registry.resolve()'s import
_F = getattr(sys, "_get" + "frame")
FRAMES = []
_f = _F(1)
while _f is not None:
    FRAMES.append(_f)
    _f = getattr(_f, "f_" + "back")

# ...and later, from evaluate(), with no audited operation whatsoever:
b = getattr(FRAMES[k], "f_" + "locals")["b"]
b["veh"].is_attacker      # -> True                        the label being predicted
b["x"], b["y"]            # -> (466.4527993807002, 120.0)  the sender's TRUE position
```

The run **completed and wrote a dataset.** This is pinned by a test that passes by the attack
succeeding, so the paragraph cannot quietly become false in either direction.

#### What *is* closed, because it is arithmetic rather than reachability

The same walk reaches `run_pipeline`'s `rng` — the engine's single global stream, whose draw *count
and order* are load-bearing for every digest in the project. Reading it is a read; **drawing from it
is a measurable act**, and the engine can refuse that even though it cannot refuse the reach:

* the stream refuses every draw for the duration of every guarded plugin call, and
* it refuses every draw for the duration of the whole plugin-**loading** phase — module import,
  `__init__`, `capabilities()`, every attribute read (`api/integrity.stream_closed`).

The engine itself makes no global-stream draw in either window, so this is exact rather than
heuristic. Measured: before the loading-phase closure, a plugin that walked to `rng` in its
constructor and drew **one** number produced a complete, clean run whose digest was `319bb0bd`
against an honest twin's `192286db` — `verify_stream` agreed (it counts what it was *asked* for, not
who asked), nothing on the watch list moved, and the manifest was written. The refusal is an
`Exception`, so it is also **counted**: a plugin that catches it and returns normally still fails the
run, at load time, with no manifest.

#### The one-line summary

> The static gate turns an accident into a named error. The runtime guard turns a deliberate reach
> **inside a plugin call** into a refusal. Neither closes the reach taken while the plugin is being
> **loaded**, and nothing in this process can. **In-process plugins are TRUSTED CODE.** A detector you
> cannot review runs with `"isolated": true`.

### 2.5 When the engine checks a plugin — and why "when" was a defect

Three defects in this seam were about **time**, not about data, and all three were measured on this
engine before they were fixed.

**Attestation used to run AFTER construction.** `build_channel` instantiated the run's plugin and
only then called the conformance suite on it; `build_checks` repeated the ordering. A hostile
`__init__` therefore executed before anything gated it. Measured, on the round-2 code, with a channel
model that genuinely fails **C1, C2, C3 and C6b** and whose `__init__` is one line replacing
`conformance.runner.run_ref` with a function that always says yes:

```
plugins.channel_model = {"ref": "…:AttestSubverterChannel", "conformance": "required"}
  exit 0, manifest written,
  manifest["plugins"]["loaded"][0]["conformance"] = {"passed": 99, "failed": 0, "ok": true}
```

Ninety-nine checks that were never run, recorded in the artifact as evidence of conformance. **An
attestation performed by code the subject can rewrite attests to nothing.** Two changes:

* attestation now runs **before** the run's instance is constructed, and
* it runs in a **child interpreter** (`scms_sim_ref/conformance/attest.py`). Conformance has to build
  the candidate to grade it, so *somewhere* a hostile `__init__` runs; now it runs in a process that
  is discarded a moment later and shares no object with the run. The child brackets the suite with
  its own integrity sentinel and reports what it saw, so a candidate that tampers while being
  attested is refused whatever its check results say.

The same model on this branch: `ConfigError … does not conform: ['C1_repeatable',
'C2_call_order_independent', 'C3_global_rng_untouched', 'C6b_…']`, no output directory, and the
plugin's constructor **never ran in the engine's process at all** — which is the property the test
asserts, because the order is what the defect was.

**A constructor is still arbitrary code**, so every third-party load — channel, check and fusion —
is bracketed by an integrity snapshot. An `__init__` that rebinds `random.Random` or replaces
`api.channel.check_outcome` is fatal **before step 0**, before an output directory exists.

**And `__init__` is not the earliest hook.** `resolve()` calls `importlib.import_module`, so
*module-level* code runs first — `random.Random = Impostor` at module scope is one line, and the
source gate's name list does not contain it (and does not apply to the channel slot at all). Worse,
the import happens inside `validate_config`, which resolves every declared ref. So the run's snapshot
is the **first statement of `run_pipeline`**, taken before validation reads `cfg.plugins` at all.
Checkpoints, in order: plugin resolution (import), after the channel plugin loads, after the
detection layer loads, and the end of the run.

### 2.6 Conformance is a sampling check, not a proof

**Read this before you read a `conformance_report.json`.**

A `random.Random` class rebind guarded on `if frame.step >= 30` passes **all four** of C3's traps.
That is not a hole in the traps — they are correct, and they catch that exact rebind when it is
installed inside the window they drive. It is a property of **any** fixed-window contract test:

> **A conformance suite can only certify the behaviour it observed.**

Lengthening the window moves the number an attacker waits out; it does not change the shape of the
statement, so the window was not lengthened. What was added is monitoring that holds for the **whole
run** (`src/scms_sim_ref/api/integrity.py`), armed whenever `cfg.plugins` declares anything:

| instrument | what it certifies | what it cannot see |
|---|---|---|
| **identity snapshot** — 122 named objects: `random.Random` and every generator method on it, the module-level `random` bindings, `check_outcome`, `srcgate.gate`, `registry.resolve`/`verify_lock`, `run._attest`/`_data_digest`/`_write_manifest`, `Observation`, `NamespacedState`, `LinkOutcome`, a builtins core, and interpreter-wide trace/profile/`meta_path` hooks | at the end of the run the engine is made of the same objects it was made of at the start | a binding that was tampered with, used, and **restored** before the checkpoint |
| **`WitnessedRandom`** — the engine's global stream counts Mersenne-Twister words (`random()` = 2, `getrandbits(k)` = `ceil(k/32)`), and the end state is replayed against a pristine generator | the engine's own stream advanced **exactly** as far as the draws it made would put it | a plugin drawing *through* the counted methods; the count itself is not derived from the engine's arithmetic |

They are complementary, and the delayed attack shows why. Measured, all on the same 40-step
grid-5×5 seed-17 scenario:

| what the plugin does | before the monitor | on this branch |
|---|---|---|
| `random.Random = Impostor` at `frame.step >= 30` | **exit 0**, manifest written, digest `8700549dfa0669ac…` | `IntegrityError … at the END of the run`, **no manifest** |
| the same model with the attack never firing (honest baseline) | digest `8700549dfa0669ac…` | identical |
| `random.Random.random` rebound at step 30 and **put back** at step 34 | exit 0 | `the engine's global random stream is NOT where … counted Mersenne-Twister words … would put it` |

**Read the first two rows together.** `Impostor` is a plain `random.Random` subclass, so the
digest does not move — the attacking run and the honest run produce **the same bytes**. The pinned
goldens, the two-run equality gate and the content-hash lock are all structurally blind to it,
exactly as they are to a frame walk. Owning `random.Random` is a *capability*; what it is worth is
whatever the next version of the plugin does with it, and by then it is twenty keyed
`random.Random(f"{seed}:…")` construction sites deep in the engine.

`manifest["plugins"]["integrity"]` records `armed`, `watched`, `verified_at` and
`engine_rng_words` — present only in a run that declared plugins, so a manifest written without
them is byte-identical to what it always was.

**And the negative claim, because the whole section is worthless without it.** A *passing*
verification means "nothing on the watch list moved". It does not mean the plugin was honest. A
detector reading the labels through `sys._getframe` moves nothing on either list, and stays
perfectly reproducible while doing it. The monitor is detection at named checkpoints, not
containment. Pinned in
`tests/test_plugin_integrity.py::test_the_monitor_is_not_a_sandbox_and_the_module_says_so`.

### 2.7 `check_outcome` validates into a copy

C7's runtime form used to validate by *reading* the caller's object and then hand **the same object**
back. Nothing obliges a channel model to return the real `LinkOutcome` class, and an object whose
`rssi_dbm` is a stateful `property` returns one value to the checker and another to the engine.
Measured on this engine, a model whose outcome serves `-70.0` on its first read and `9999.0` on every
read after it:

| | `ma/ma_reports.jsonl` | digest |
|---|---|---|
| before | **2024 rows carrying `rssi_dbm = 9999.0`** (+9999 dBm ≈ 10³⁷ W, past the declared `[-140, 0]` band) | `2e053ed777edee94…` |
| now | 2024 rows carrying `-70.0`, the value that was validated | `20557d1115f0b5dc…` |

Every field is now read exactly once into a local, the *locals* are validated, and a fresh
`LinkOutcome` built from coerced primitives is what the engine goes on to use. The plugin no longer
holds the object the engine reads.

### 2.8 Isolated mode — the process boundary, and the only answer for code you cannot review

**This is the mode for a third-party submission.** The detector runs in its own interpreter; the
engine serialises the `Observation` — and only the `Observation` — to it and reads back a score. The
broadcast dict, the `Vehicle`, the `PipelineConfig` and the engine's global `random.Random(cfg.seed)`
are not sent, are not referenced by anything that is sent, and **do not exist in that process at
all**. It is one config key:

```jsonc
"plugins": {
  "check": ["@builtins",
            {"ref": "their_detectors.threshold:TheirCheck",
             "params": {"tolerance_mps": 3.0},
             "isolated": true}]
}
```

`isolated` is a config key rather than a flag, so it lands verbatim in
`manifest["config"]["plugins"]`, it replays, and `manifest["plugins"]["loaded"][*]` carries
`"isolated": true` — a dataset produced this way says so in its own lock.

**The property that makes it usable: the same detector gives the same answer.** Measured on this
host, one 300 s / 593-vehicle run (`--flow --road grid --grid 6 --duration 300 --arrival-rate 2
--attacker-pct 0.15 --traffic-lights --seed 42`), the real `scms-demo-detector` at
`tolerance_mps = 3.0`:

| configuration | in-process `data_digest` | isolated `data_digest` | equal |
|---|---|---|---|
| `["@builtins", plugin]` | `81c194edbadecc47b5c437bee584e46135391467f0447d91f0445c66e4a1d78d` | *same* | **yes** |
| plugin alone | `d71b29f0eb4d52900f22a6adb933e3eb3bd30f05824b102600862c66c9f99f57` | *same* | **yes** |

Every count matched too — 7 856 reports / 152 revoked on the first row, 477 / 19 on the second — and
the first row's digest is the one §5.2 already records for the **in-process** run, so isolated mode
reproduces a number this document pinned before the mode existed.

**Three things make that hold, and each one was a decision.**

1. **JSON round-trips a float exactly.** `json.dumps` renders through `float.__repr__`, the shortest
   string that decodes back to the same double, so a score is bit-identical across the pipe. Pinned
   per field in `tests/test_detector_isolation.py` with `repr()` rather than `==`, because `==` would
   pass for two doubles that merely printed the same.
2. **The child derives randomness the same way the engine would have.** `RngNamespace(seed,
   plugin_id)` is a pure function of `(seed, replicate, plugin, label, ids, step)`, so the worker
   builds its own from the two values it is told and draws the identical words. The engine announces
   the step on every `EVAL`; nothing about the stream depends on which process it is in.
3. **The engine keeps owning the state.** The plugin's `state["plugin:<id>"]` crosses on every
   message and comes back, so the engine's own pruning of `last_claimed` still governs its lifetime.
   The cost is that an isolated check's state must be **JSON** — numbers, strings, booleans, lists,
   dicts. A value that will not serialise is named at the first message
   (`state['thing'] is not JSON-serialisable`) rather than dropped, because a silent drop would make
   the two modes disagree on message 2, which is the worst possible way to learn about a constraint.

**Transport: length-prefixed JSON over the child's stdin/stdout.** Four bytes big-endian, then that
many bytes of UTF-8 JSON, strict lockstep, one reply per request, the sequence number echoed and
asserted. The reasons, in order of weight, are in `api/isolate.py`'s docstring and worth repeating
here because they are what makes the mode safe rather than merely separate:

* **JSON is data, not code.** The reply comes from a process the untrusted detector runs inside, so
  the parent must be unable to be harmed by a hostile reply. `pickle` — and therefore
  `multiprocessing`, whose queues pickle — would hand that process arbitrary code execution *in the
  engine*, which is precisely what this mode exists to prevent. A crafted JSON document is at worst
  a wrong number, and every number is range-checked on arrival.
* **No ambient resource.** No port to allocate (no ephemeral-port collision, no loopback firewall
  prompt on Windows, no second process able to connect), no temp file to race. The pipe dies with the
  parent, so an abandoned worker is reaped by the OS and not by a cleanup path that might not run.
* Rejected for the same reasons, plus one each: a loopback socket (connectable by anything);
  shared memory (needs a lock discipline, and a hostile child can corrupt the parent's view);
  per-**step** batching as the channel ABI specifies (§2.3 of `PLUGIN-ARCHITECTURE.md`) — the fusion
  consumes one message's score before the next message is scored, so batching a step means
  restructuring the reception loop and re-pinning every golden.

**Failure is loud, and the timeout is a fail-stop.** A worker that crashes, hangs, desynchronises,
returns a non-number or reports a message count that does not match what it was sent **fails the
run**. There is no branch on which any of those becomes a `0.0`: a dataset in which "the detector was
broken" and "the detector saw nothing" are the same bytes is worse than no dataset. The watchdog is
the only clock in the mode, and it can turn a run into a failure — never into a different run, which
is what keeps the engine independent of the child's scheduling.

```
plugins.check 'their.det:TheirCheck': the isolated worker refused EVAL.
  RuntimeError: detector exploded on purpose
  ...their/det.py, line 41, in evaluate
plugins.check 'their.det:Slow': the isolated detector did not answer within 60s and was killed.
  A hung detector FAILS THE RUN -- there is no path on which a timeout becomes a score, because a
  score that depends on the scheduler is not reproducible.
```

**The overhead, measured, on that same 300 s / 593-vehicle run.** A benchmark is worth a slow mode;
you need the number to decide.

| vector | observations scored | in-process | isolated | factor | per observation |
|---|---:|---:|---:|---:|---:|
| `@builtins` + plugin | 960 509 | 12.37 s | 53.49 s | **4.3×** | **+40.8 µs** |
| plugin alone | 1 210 797 | 9.54 s | 53.85 s | **5.7×** | **+35.7 µs** |

The round trip *is* the cost: 39.18 s of the 41.1 s delta was measured inside the proxy call, so
95 % of the overhead is the exchange itself and essentially none of it is the engine. On a short run
the fixed cost dominates instead — a 40 s grid-5×5 scenario goes 0.44 s → 2.14 s, of which ~1.0 s is
the child interpreter starting. **Budget roughly 40 µs per delivered message per isolated check**,
plus one second of process start.

*An earlier build of this mode measured 50.6 µs, and the difference is worth naming because it is
where the time in a scheme like this actually goes.* It was **not** the pipe: it was four redundant
`json.dumps` calls per message, one per reserved state key, probing "will this serialise?" before
encoding the payload that would have answered the same question. Encoding once and probing per key
only when that fails took 20 % off the per-message cost. Both builds reproduced `81c194ed…`, which is
the property that made the optimisation safe to make at all.

**The proof: the same hostile detector, both ways.** A check whose `evaluate` is
`sys._getframe(1).f_locals["b"]`, run on the 40 s grid-5×5 seed-17 scenario. In process it needs
`"source_gate": "off"` — which is exactly what a reviewer who waved the submission through would
have written.

| | in-process (`source_gate: off`) | **isolated** |
|---|---|---|
| frames above the detector's own | 3: **`scms_sim_ref.mock_pipeline.run.run_pipeline`**, then the caller and the module | 6: `api/isolate.py`'s `_Worker.evaluate`, `serve`, `main`, its module, then `runpy` ×2 |
| the caller's locals | ~300, including `b`, `cfg`, `rng`, `vehicles`, `gt_vehicle`, `last_claimed`, `pseudonym_info`, `revoked_vehicles` | `['_types', 'fields', 'msg', 'obs', 'self', 'state']` — the payload it just decoded, and nothing else |
| `b` | `['cg','ch','conf','cs','cvf','cvt','cx','cy','digest','falsified','ghost','msg_count','sig_ok','station_type','thdg','tspd','veh','x','y']` | **not present in any frame** |
| the oracle | `veh` = `Vehicle`, `is_attacker=True`, `attack_type='ConstPosOffset'`, true `x,y` = `466.45, 120.0`, `falsified`, `ghost` | **unreachable** |
| the engine's global rng | `WitnessedRandom` | **unreachable** |
| `scms_sim_ref` modules in the process | 22 | **8**, every one of them `scms_sim_ref.api.*`; `mock_pipeline` is never imported |
| what it produced | **1 910 reports, every one of them ≥ 1.0 on the label**, 15 vehicles revoked, digest `7faa7aed9a3fe6f7…` | **0 reports**, 0 revoked, digest `9e97f83c0f7139d4…` |

Read the last row twice. In process the label-reading detector produced a complete, perfectly
reproducible dataset built out of the answers. Isolated, the identical class has no signal and files
nothing — not because it was refused, but because there was nothing there to read.

#### The other half: this run's ground truth is not on the DISK either

Process isolation empties the detector's address space. It does not, by itself, empty the
**filesystem**, and for one round of this document that difference was not stated — §2.8 asserted the
run's own oracle files were unreadable while the loop was going, and that was false. `run.py`
**STREAMED** `ground_truth/gt_report_labels.jsonl` as the loop proceeded, so an isolated detector,
declared alongside `@builtins` and probing at its 20 000th message, read this:

```
ground_truth/gt_report_labels.jsonl  -> readable, non-empty:
  {"_visibility":"ORACLE","report_correctness":"correct","report_id":"rpt_00001",
   "reporter_true_id":"veh_000","subject_true_id":"veh_001"}
```

The oracle verdict on every report filed so far in the same run, with the true vehicle ids of
reporter and subject. Not the per-message `falsified` flag — but enough to learn which certificates
the ground truth calls attackers, in the run the detector is being graded on. **That is now closed.**

**When any check is `isolated`, the engine does not write ORACLE output while a worker is alive.**
The two streamed ground-truth tables go to a `_WithheldStream` — which accepts the same writes and
creates nothing — and the workers are FINISHed and reaped *immediately after the step loop*, before
`_write_side_files` runs. So the guarantee covers the whole ground-truth set, `gt_vehicle.jsonl`
(`is_attacker` per vehicle) included, and not merely the two files that used to stream. The identical
probe now measures:

```
out_dir listing (mid-run, message 20 000)  -> ['ma']
ground_truth/                              -> does not exist
ground_truth/gt_report_labels.jsonl        -> FileNotFoundError
child's working directory                  -> a fresh empty temp dir; os.listdir('.') == []
environment variables naming out_dir       -> none
```

`ma/ma_reports.jsonl` is still streamed on purpose, and it stays readable: it is MA-visible — no
`_visibility: ORACLE` row, no true id, no `falsified` flag — so a child that opens it learns the
built-in suite's `detnorm_*` scores for messages already processed, not the answers. Withholding it
too would cost every long run its memory bound for a file that contains no labels. Conversely
`live_interval_s > 0` with an isolated check is now a **`ConfigError`** rather than a silent
downgrade — `live_state.json` is written during the loop and marks every attacker with state byte 1.

**The cost, measured** on the InTAS AM peak (`intas.trace`, 1 188 vehicles, 158 767 vehicle-steps,
300 s, seed 42), run twice with the CPU work held identical:

| | streamed | withheld |
|---|---:|---:|
| `data_digest` | `c4a7cddeb4ef58dd254ad051186911ebfd2c0e143b84115ac8f8d967eb082491` | *same* |
| peak working set | 254.2 MB | **258.3 MB** (+4.1 MB, +1.6 %) |
| oracle bytes held | — | 3.17 MiB, no spill |

Above a shared ceiling of **384 MiB** of withheld output the overflow spills to
`<out_dir>/.withheld/*.sealed`, XORed with a 32-byte `os.urandom` key held only in the engine's
memory; `commit()` unseals it into the real file and deletes it. So the claim has two tiers and both
are true as written: **under the ceiling there is nothing on disk to read; over it there is a file
that holds nothing readable.** The measured cost of the second tier is +0.1 MB of RSS and ~2 s per
212 MiB round trip, byte-exact. `docs/realism/ISOLATION-ORACLE-LEAK.md` has the full table, including
the two defects the measurement itself caught (a 1.44× RSS ratio fixed by compacting rows into 8 MiB
blocks, and a 754 MB commit-time transient fixed by writing one block at a time).

Additionally — **defence in depth, and stated separately because it is weaker** — the worker starts
in a fresh empty working directory rather than the engine's, and `child_env` drops every environment
variable and every `PYTHONPATH` entry that names the output directory (`PATH`-shaped values are
filtered, not deleted). That removes the paths the child is *handed*. It does not remove the paths it
could *find*: `scms_sim_ref.__file__` names this repository, and an absolute path works from any
working directory.

Pinned by `::test_the_child_cannot_read_this_runs_oracle_labels` (told the exact path, base64-encoded
so the env scrub cannot make the measurement vacuous, it finds nothing) and by
`::test_without_an_isolated_detector_the_labels_are_still_streamed`, which keeps the old assertion
alive in process — where streaming still happens and must keep happening.

#### What isolated mode does **not** close

**1. EVERY OTHER DATASET ON THE MACHINE.** The child is an ordinary OS process running as the same
user. It cannot reach the engine's objects and it cannot read this run's labels, because they do not
exist yet — but it reads the **filesystem**:

```
a COMPLETED earlier run's gt_vehicle.jsonl -> readable:
  {"_visibility":"ORACLE","attacker_role":"ConstPos",...,"is_attacker":true,
   "true_vehicle_id":"veh_000","veh_type":"bus"}
this repository's run.py                   -> readable
```

A determined child can walk the disk looking for one, and the empty working directory does not stop
it. The remedy is a file-system remedy and not a Python one:

* give the worker a user that cannot read your dataset directories (Windows: a restricted token or a
  separate account plus ACLs; Linux: a different uid, or a mount namespace);
* or run the benchmark on a host that holds no other ground truth, and publish datasets only after
  the run;
* or wrap the worker in an OS-level sandbox — which this mode makes *possible*, because there is
  exactly one process to wrap and it speaks exactly one pipe.

None of those is implemented here, and none of them is claimed. Asserted in
`tests/test_detector_isolation.py::test_the_child_still_reads_the_filesystem_and_the_docs_say_so`, so
this paragraph cannot quietly become false.

**2. The worker reports its own identity — and for a dotted ref the parent no longer believes it.**
This used to be a live hole, and it was measured: a plugin that set its module's `__file__` to a
decoy made the lock record the decoy's hash (`6d85ce01`) for code that hashed `90272b8b`, and a
replay re-probed the same decoy, so "no drift" was true of a file that never ran.

For a `pkg.mod:Class` ref — which the **parent** owns, because it comes out of the config — the
parent now resolves the module on `sys.path` itself (`registry.static_locate`, which walks the path
the way `FileFinder` does and executes nothing, unlike `find_spec`, which imports parent packages)
and **refuses a worker whose reported path is not the one this process resolves**. `module_sha256`,
`package_sha256` and `import_closure_sha256` are all computed from the parent's path.

The residue is named in the lock rather than left to be assumed: when the parent *cannot* resolve the
name that way — a zipimport, a namespace package, an entry-point ref, an editable install behind a
custom finder — it falls back to the child's path and records `"path_source": "worker"`, and in that
case the old caveat still applies. Read that field before you trust the hashes. The in-process mode
has the strictly larger version of this hole (there the plugin can rewrite `registry.package_sha256`
itself, which is why the integrity monitor watches it).

**3. Config-time schema introspection is given up.** `--dump-config-schema`, the GUI's advanced panel
and the copilot cheat-sheet cannot show an isolated plugin's knobs, because reading `config_fields()`
means running the plugin's code in the process that is asking. Its params are still validated against
the plugin author's own `FieldSpec` bounds and its column still claimed — one phase later, off the
worker's handshake, still before step 0 and before any output directory exists:

```
plugins.check.params.max_range_m=1.0 below minimum 10.0
plugins.check.params: iso_det:ClaimedRangeThreshold declares no field 'nope'; known: [...]
```

**4. It is not a resource limit.** The child can allocate, spin, spawn its own processes and open
sockets. The watchdog turns "does not answer" into a failed run; it does not cap what the detector
does in the meantime. A public competition wants an OS-level sandbox around the worker — which this
mode makes *possible* (there is a single process to wrap, speaking a single pipe) and does not itself
provide.

**5. A built-in cannot be isolated,** and that is a refusal rather than a limitation: a built-in *is*
the engine, its knobs are engine config fields, and a round trip per message would buy nothing.

**6. The source gate defaults OFF in this mode.** The gate refuses `sys._getframe`, `gc.get_referrers`
and friends because *in process* they reach the engine's frame. Out of process they do not, so
refusing a submission for containing the name would be theatre — and turning a real submission away
for a construct that is now harmless is how a safety mode gets switched off. `"source_gate": "on"`
still works and still screens the module the worker named, by **reading** the file rather than
importing it.

### 2.9 What this means if you are running a benchmark

If you are collecting detectors from other people and comparing them:

1. **Read the source.** The gate makes the obvious reaches loud; it does not make reading optional.
   And read the **module body and the `__init__`**, not just `evaluate`: those are the two places the
   runtime guard is not armed ([§2.4.1](#241-the-runtime-guard--and-the-window-it-does-not-cover)),
   and a single `sys._getframe` there is a live handle on the oracle for the whole run.
2. **Pin the content hashes.** `manifest["plugins"]["loaded"][*]` carries `module_sha256`,
   `package_sha256`, `dist_sha256` and `import_closure_sha256` — the last of which is what catches an
   edit to a module in a *different* top-level package the plugin imports its logic from (measured:
   such an edit moved the dataset digest while `module_sha256`, `package_sha256` and `dist_sha256`
   were all unmoved and `verify-plugins` exited 0). §7.1 shows the drift refusal. What you reviewed
   is then what ran.
3. **Do not treat a reproduced digest as evidence of honesty.** It is not. A label-reading detector
   reproduces its digest exactly.
4. **Use the conformance suite's D2 and D3** (§4.1) — and know their blind spot. D2 records every
   attribute a check touches and refuses undeclared ones; D3 scores two streams identical in every
   declared field where one also carries ground truth, and fails the check if the scores differ.
   Together they catch a detector that reads the oracle *off the observation*, at run time, which is
   a complementary instrument to the gate's static one. **Neither sees a frame walk**: measured, a
   frame-walking check passes all six with `ok = True` (§4.1).
5. **For submissions you cannot review, run them isolated** ([§2.8](#28-isolated-mode--the-process-boundary-and-the-only-answer-for-code-you-cannot-review)):
   `"isolated": true` on the config entry, and **this run's labels are neither in the detector's
   process nor on the disk while it runs** — the two ORACLE tables are withheld until the worker has
   been reaped, at a measured +4.1 MB of peak working set on the InTAS AM peak. It costs about 40 µs
   per delivered message (4.3× the wall clock of the run in the measured case) and it is the only
   claim in this document that is a *boundary* rather than a guard rail.
6. **Then read §2.8's "what it does not close" before you build a public competition on it.** The
   **filesystem** is still shared: every *other* dataset on the host is one `open()` away from the
   submitted code, and that is measured rather than hand-waved. Run the benchmark on a machine that
   holds no other ground truth, or put an OS-level sandbox around the worker — isolated mode makes
   that possible (one process, one pipe) and does not provide it.

---

## 3. Write it

A distribution of your own, outside the simulator repo. This is the real
`scms-demo-detector 0.1.0`, abridged — the full source is at `C:\Temp\scms_detector_demo`.

```toml
# pyproject.toml
[project]
name = "my-detectors"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = []          # you import scms_sim_ref.api and NOTHING else

[project.entry-points."scms_sim_ref.check"]      # DISCOVERY only, never activation
my_threshold = "my_detectors.threshold:MyThreshold"
```

```python
# src/my_detectors/threshold.py
import math

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase
from scms_sim_ref.api.fields import FieldSpec


class PositionSpeedThreshold(CheckBase):
    """F2MD's PositionSpeedConsistancy, inverted.

    Between two consecutive claims from the same certificate the sender moved a claimed distance
    over a claimed interval. Is the implied average speed reachable from the speeds it itself
    claimed at the two ends, given bounded acceleration and its own broadcast position confidence?
    """

    interface_version = INTERFACE_VERSION
    plugin_id = "demodet"                 # reserved RNG / config / column namespace
    reason_code = "positionSpeedThreshold"
    precision = 3                         # decimals the engine rounds to BEFORE the >= 1.0 compare
    msg_types = ("cam",)
    vru_suppressed = True                 # pedestrians break vehicle kinematics legitimately

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.tolerance_mps = float(self.params["tolerance_mps"])
        self.max_accel_mps2 = float(self.params["max_accel_mps2"])
        ...
        if self.tolerance_mps <= 0.0:     # FAIL FAST, at construction, never at step k > 0
            raise ValueError("tolerance_mps must be > 0")

    @classmethod
    def config_fields(cls):
        """ONE declaration per knob. It reaches --dump-config-schema, the GUI, the copilot,
        config-time validation and the manifest's params hash."""
        return {
            "tolerance_mps": FieldSpec(
                "float", 3.0,
                "THE THRESHOLD. Implied-speed excess over the plausible band, in m/s, that scores "
                "exactly 1.0. Lower == more sensitive == more reports.",
                lo=0.05, hi=100.0, step=0.05, unit="m/s"),
            "max_accel_mps2": FieldSpec("float", 5.0, "plausible acceleration bound",
                                        lo=0.1, hi=30.0, unit="m/s^2"),
            # ... max_decel_mps2, speed_conf_mps, min_interval_s, pos_conf_weight
        }

    def capabilities(self):
        return frozenset({"history", "stateful"})

    def evaluate(self, obs, state, params, rng):
        """Return a detnorm. >= 1.0 is VIOLATING."""
        if obs.first_sight:
            return 0.0                    # no history to compare: firing here destroys precision
        elapsed = obs.t - obs.prev_t
        if elapsed < self.min_interval_s:
            return 0.0

        implied = math.hypot(obs.claimed_x - obs.prev_x, obs.claimed_y - obs.prev_y) / elapsed

        # F2MD's R: speed confidence plus both position confidences over the interval
        slack = self.speed_conf_mps + self.pos_conf_weight * obs.pos_conf / elapsed
        upper = max(obs.claimed_speed, obs.prev_speed) + self.max_accel_mps2 * elapsed + slack
        lower = max(0.0, min(obs.claimed_speed, obs.prev_speed)
                    - self.max_decel_mps2 * elapsed - slack)

        if implied > upper:
            excess = implied - upper      # a teleport: moved further than any speed allows
        elif implied < lower:
            excess = lower - implied      # a freeze/replay: claims to move, its claims do not
        else:
            excess = 0.0

        state["scored"] = state.get("scored", 0) + 1   # its OWN namespace and nothing else
        return excess / self.tolerance_mps             # tolerance_mps IS the threshold
```

**`tolerance_mps` is the knob**, in the units the residual is measured in: a sender whose implied
speed exceeds the plausible band by exactly `tolerance_mps` scores exactly `1.0` and fires. Halving
it doubles every score.

---

## 4. Install it and check it conforms

```powershell
pip install C:\Temp\scms_detector_demo
python -m scms_sim_ref.mock_pipeline.run conformance --slot check `
    --ref scms_demo_detector.threshold:PositionSpeedThreshold
```

```
conformance v1 :: check :: scms_demo_detector.threshold:PositionSpeedThreshold
  PASS   D1_pure
  PASS   D2_reads_only_ma_visible             10 declared fields read, none written
  PASS   D3_label_invariance
  PASS   D4_firing_convention                 honest score 0.0 < 1.0
  PASS   D5_monotone_in_attack_magnitude      0.000 -> 130.333 over 0 -> 400 m
  SKIP   D6_off_by_default_is_byte_identical  no GOLDEN pinned: ...
  PASS   D7_state_namespacing                 own namespace holds ['scored']
  PASS   D8_reason_code_does_not_collide      column 'x_demodet_positionSpeedThreshold' at index 14,
                                              16 unique columns
  -- 7 passed, 0 failed, 1 skipped, 0 waived, 0 errored in 0.1s
exit = 0
```

Or subclass the contract in your own test suite and let pytest collect the eight checks:

```python
from scms_sim_ref.conformance.v1.detect import CheckContract

class TestMyThreshold(CheckContract):
    REF = "my_detectors.threshold:MyThreshold"
    PARAMS = {"tolerance_mps": 3.0}
    PEERS = ("my_detectors.threshold:MyOtherCheck",)   # D8 grades the family for collisions
    GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"   # D6
    GOLDEN_CONFIG = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                         arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25)
```

Measured on the real distribution: **26 passed, 1 skipped** (the skip is a second contract that
pins no `GOLDEN`), including a `D6` that runs a full pipeline and reproduces `0bd93655…` with the
plugin installed and importable but declared nowhere.

Or let the engine refuse an unattested plugin before step 0, by declaring
`"conformance": "required"` on the config entry. Measured: refusing
`violators:OracleThreshold` that way raises `ConfigError` and **creates no output directory**.

Two things about *when* and *where* that attestation happens, both of which were defects until
2026-08-31 and are set out in full in [§2.5](#25-when-the-engine-checks-a-plugin--and-why-when-was-a-defect):
it runs **before** the engine constructs your plugin, and it runs in a **child interpreter**, so a
constructor cannot rewrite the machinery that judges it. Cost: one interpreter start per attested
plugin (~1.0 s, against ~0.11 s when the suite ran in-process), paid only by a run that asked for it.
The PEP 578 audit hook `C5`/`D5` installs — which can never be removed once added — is now paid by
the child and discarded with it, instead of being left on the engine process for the rest of its life.

### 4.1 What the eight checks are, and what each one catches

| id | what it asserts | measured on a violator |
|---|---|---|
| **D1** | same `(obs, state)` twice → same score | a `time.time_ns()`-seeded check: `two identical evaluations returned 0.4108… and 0.3334…` |
| **D2** | every attribute touched is a declared `Observation` field, and none was written | `read attribute(s) ['falsified', 'is_attacker'] that the Observation does not declare` |
| **D3** | two streams identical in every declared field, one carrying ground truth, score identically | `scores changed when GROUND TRUTH was attached: [0.0, 0.0, 0.0, 0.0, 1.0, 2.0] vs [3.0, 0.0, 3.0, 0.0, 4.0, 2.0] -- this check reads the oracle` |
| **D4** | polarity and range: an honest claim scores `< 1.0`, every score finite and `>= 0` | `honest observation scored a NEGATIVE -3.0`; and, separately, the F2MD-polarity port's `scored 1.0 >= 1.0` |
| **D5** | score non-decreasing in the size of the falsification | `score FELL as the falsification grew: 20.0 m -> 0.2538, 60.0 m -> 0.0586` |
| **D6** | installed-but-not-declared reproduces the golden. Needs a digest from *before* the plugin existed, so the contract subclass supplies it as `GOLDEN`; with none supplied it **skips with a written reason** rather than inventing a comparison that passes by construction | pinned at `0bd93655a2d5bebb…`, the plugin's own suite runs a full pipeline and it **passes** |
| **D7** | writes only `state["plugin:<id>"]` | `ConfigError: state key 'streak' is reserved for the built-in detectors` |
| **D8** | the reason code is well-formed, is not a built-in's name, and is unique in the vector the engine actually builds | `reason_code 'positionSpeedInconsistency' is the BUILT-IN check's name`. For two refs sharing a `(plugin_id, reason_code)` pair, D8's own assertion no longer gets a chance to run: **the engine's loader refuses the pair first**, so D8 reports `ERROR ... both resolve to column 'x_demodet_positionSpeedThreshold'` with `ok = False`. Refused either way, one layer earlier |

**D2 and D3 are the two that matter most**, and they are independent of every other layer of this
project. A detector that reads the oracle is *perfectly byte-reproducible*: it passes D1, D4, D5,
D7, D8, reproduces its digest across processes, and produces a dataset in which the labels leak into
the features. The pinned-golden layer is structurally blind to it. D2 catches the reach, D3 catches
the effect.

**And D2/D3 are not a sandbox either — they are a behavioural probe.** They call `evaluate` with a
recording proxy and with two paired streams, so they catch a check that reads the oracle *off the
observation*. A check that reaches the oracle through `sys._getframe` is invisible to both, because
the conformance harness's own frame carries no broadcast dict: the plugin finds nothing there, falls
back to a plausible residual, and scores identically on both streams. Measured on exactly such a
check:

```
  PASS   D1_pure
  PASS   D2_reads_only_ma_visible         2 declared fields read, none written
  PASS   D3_label_invariance
  PASS   D4_firing_convention             honest score 0.0 < 1.0
  PASS   D5_monotone_in_attack_magnitude  0.000 -> 2.120 over 0 -> 400 m
  PASS   D7_state_namespacing             kept no state
  -- 6 passed, 0 failed ...            ok = True
```

The identical class, inside the engine, reads `b["veh"].is_attacker`. That is why the source gate
(§2.4) exists as a separate, *static* instrument, why §2 is written the way it is, and why "it passed
conformance" is not an answer to "can this detector see the labels". Pinned in
`tests/test_detector_trust_boundary.py::test_the_conformance_suite_is_blind_to_a_frame_walk_and_the_docs_say_so`.

**Measured power of the whole suite** — seven violators, each failing the check that covers its
defect, none of them failing everything:

| violator | exit | checks failed |
|---|---|---|
| `WallClockThreshold` | 1 | D1, *D3, D5* |
| `OracleThreshold` | 1 | **D2, D3** |
| `NegativeScoreThreshold` | 1 | **D4** |
| `F2mdPolarityThreshold` | 1 | **D4** |
| `AntiMonotoneThreshold` | 1 | **D5** |
| `StreakGrabberThreshold` | 1 | **D7** |
| `ColliderThreshold` | 1 | **D8** |
| `PositionSpeedThreshold` (the real one) | **0** | none (D6 skips on the CLI route, which pins no `GOLDEN`; it passes in the plugin's own suite, which does) |
| `RangePlausibilityThreshold` (the real one) | **0** | same |

*`WallClockThreshold`'s extra failures are collateral and **vary between runs** — D1 + D3 + D5 on
one invocation, D1 + D3 on the next. That is the point rather than a flaw in the table:
nondeterminism poisons every comparison a suite can make, non-reproducibly, which is exactly why D1
exists and runs first. Every other violator fails the same single check every time.*

---

## 5. Run simulations with it

### 5.1 Registering is not enabling

With the wheel installed in `site-packages` but **not declared** in any config, the reference run is
byte-identical to its pinned golden:

```powershell
python -m scms_sim_ref.mock_pipeline.run --flow --road grid --grid 6 --duration 300 `
    --arrival-rate 2 --attacker-pct 0.15 --traffic-lights --seed 42 --out C:\Temp\det_eval\ref
```

```
vehicles=593 reports=7823 investigations=152 revoked=152
data_digest=b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815   <- the pinned golden
detection: precision=0.599 recall=0.91 attackers=100 revoked=152 latency_med=4.0s
```

### 5.2 Declared, alongside the built-ins

```jsonc
"plugins": {
  "check": ["@builtins",
            {"ref": "scms_demo_detector.threshold:PositionSpeedThreshold",
             "params": {"tolerance_mps": 3.0}}]
}
```

`"@builtins"` expands **in place** to the run's built-in suite in canonical order, so adding one
detector does not mean transcribing fourteen names. An empty or absent `check` key means the default
suite, which is what keeps every pinned golden intact.

The same thing on the command line, without a config file — **note the PowerShell escaping**, which
is a shell wart and not an engine one: Windows PowerShell 5.1 strips the inner double quotes when it
hands an argument to a native executable, and the engine then reports
`plugins is not valid JSON: Expecting property name enclosed in double quotes`.

```powershell
$p = '{"check": ["@builtins",
                 {"ref": "scms_demo_detector.threshold:PositionSpeedThreshold",
                  "params": {"tolerance_mps": 3.0}}]}' -replace '"', '\"'
python -m scms_sim_ref.mock_pipeline.run --seed 17 --duration 40 --arrival-rate 1.5 `
    --plugins $p --out C:\Temp\det_eval\cli
```

```
vehicles=12 reports=61 investigations=1 revoked=1
data_digest=4d1c4400f8cda0e6224173065bb011b1e03d5d6d0cad9de83022572b361aec04
3p cols = ['detnorm_x_demodet_positionSpeedThreshold']
```

(The baseline for that tiny scenario is `ddeef99cad40ca6d…`; the digest moves even though the check
fires 0 times on 61 reports, because the column is present at `0.0` on every row. Adding a detector
always moves the digest — that is why `plugins` defaults to `{}`.)

```
vehicles=593 reports=7856 investigations=152 revoked=152
data_digest=81c194edbadecc47b5c437bee584e46135391467f0447d91f0445c66e4a1d78d
reports                 = 7856
detnorm columns         = 14                      (13 built-in + 1 third-party)
third-party columns     = ['detnorm_x_demodet_positionSpeedThreshold']
rows carrying the col   = 7856                    (every report)
rows firing (>= 1.0)    = 840
rows it is TOP reason   = 429                     (260 of them correct)
```

**The knobs reach the schema from that one `FieldSpec` declaration.** `config_schema()` goes from
**142 to 148 fields** when the plugin is declared, adding exactly its six knobs under
`plugins.check.<name>` — which is what feeds `--dump-config-schema`, the GUI's advanced panel and the
copilot cheat-sheet:

```jsonc
"plugins.check.tolerance_mps": {
  "type": "float", "default": 3.0, "min": 0.05, "max": 100.0, "step": 0.05, "unit": "m/s",
  "group": "Plugins", "widget": "float",
  "help": "THE THRESHOLD. Implied-speed excess over the plausible band, in m/s, that scores
           exactly 1.0 (the firing point). Lower == more sensitive == more reports."
}
```

and the same declaration is what validates a config, with the plugin's own bounds and messages,
before step 0:

```
plugins.check.params.tolerance_mps=0.0 below minimum 0.05
plugins.check.params.tolerance_mps=1000000.0 above maximum 100.0
plugins.check.params: scms_demo_detector.threshold:PositionSpeedThreshold declares no field
    'nonexistent_knob'; known: ['max_accel_mps2', 'max_decel_mps2', 'min_interval_s',
    'pos_conf_weight', 'speed_conf_mps', 'tolerance_mps']
```

The column reaches the ML tables with no featurizer edit:

```
report_features 3p cols  = ['detnorm_x_demodet_positionSpeedThreshold',
                            'reason_x_demodet_positionSpeedThreshold']
vehicle_features 3p cols = ['detmax_x_demodet_positionSpeedThreshold']
schema: {"desc": "per-report detnorm: third-party check 'positionSpeedThreshold' from plugin
                  'demodet' (>= 1.0 == violating)",
         "dtype": "float64", "kind": "fusion_feature",
         "name": "detnorm_x_demodet_positionSpeedThreshold"}
```

Against the same run with no plugin: attacker recall **0.96 → 0.98** (96 → 98 of 100 attackers
caught), report-level precision 0.7703 → 0.7650. Two attackers the built-in suite missed are caught
by a detector the engine has never heard of.

### 5.3 The knob is real — a threshold sweep

Running the plugin **alone** (`"check": [{...}]` with no `@builtins`) makes the metrics purely its
own. Every row below is one full 300 s, 593-vehicle, 100-attacker run at seed 42, differing only in
`params.tolerance_mps`:

| `tolerance_mps` | reports | attackers caught / 100 | report precision | revocation precision | `data_digest` |
|---:|---:|---:|---:|---:|---|
| 12.0 | 829 | **25** | **0.581** | 0.893 | `bb1015c85acfbbb5…` |
| 6.0 | 1 930 | **51** | **0.503** | 0.736 | `f177dec040f3c7fb…` |
| 3.0 | 3 111 | **64** | **0.398** | 0.488 | `e5655f70ee713d35…` |
| 1.0 | 7 458 | **79** | **0.273** | 0.276 | `ebac67c8a3551527…` |

*(band tightened to `max_accel_mps2=2.0, max_decel_mps2=2.0, speed_conf_mps=0.0,
pos_conf_weight=0.0` so honest GNSS noise can reach the cliff; every one of those is a declared
`FieldSpec` knob arriving from config.)*

**Recall rises and precision falls, monotonically, over a 12× range of the knob.** That is the
sensitivity/specificity trade-off of a threshold detector, produced by editing one number in a JSON
file. Four distinct digests, so the engine genuinely ran four different detection layers.

At the **default, generous band** — every other knob left alone — the same sweep is gentler, and
where it stops is worth understanding:

| `tolerance_mps` | reports | attackers caught / 100 | report precision | innocent vehicles accused |
|---:|---:|---:|---:|---:|
| 20.0 | 334 | 14 | **0.934** | 2 |
| 6.0 | 417 | 18 | **0.842** | 17 |
| 3.0 | 477 | 20 | **0.763** | 21 |
| 1.5 | 520 | 22 | **0.742** | 25 |
| 0.5 | 544 | 21 | **0.688** | 28 |

Precision falls monotonically (0.934 → 0.688) and recall climbs (14 → 22 attackers), but the report
count **saturates** around 520–544 and recall stops at ~0.22 no matter how far the knob is pushed.
That is correct, not a bug: with `pos_conf_weight = 2.0`, the slack term is `2 × pos_conf / dt`,
which at this scenario's 3–7 m position confidence is 6–14 m/s of headroom, so an honest vehicle
essentially never leaves the band and `excess` is either exactly `0.0` or already large. Once the
tolerance is small enough that every positive excess clears the cliff, lowering it recruits nothing
further. **A threshold knob can only trade precision for recall inside the region where the honest
and dishonest distributions actually overlap** — and the band knobs (`max_accel_mps2`,
`speed_conf_mps`, `pos_conf_weight`) are what move that region. That is why the sweep above tightens
them: it is the same detector with the overlap widened, and it is why the tight-band sweep reaches
recall 0.79 where this one stalls at 0.22.

One measured caveat, because it will otherwise look like a bug. Running the plugin **alongside** the
built-ins, the total report count is *not* monotone in the threshold (8 002 → 8 007 → 7 856 → 7 872
at tolerance 20 / 6 / 3 / 1.5). The built-in fusion draws its `report_prob` Bernoulli from the
engine's single global RNG stream, so any change to *which* messages fire reshuffles every
subsequent draw in the run. The plugin's own column is perfectly monotone (429 → 840 firing rows as
the tolerance falls from 20 to 3); the aggregate is dominated by stream reshuffling. **Evaluate a
detector solo, or against a fusion that does not sample.**

---

## 6. The acceptance test: zero engine edits

The claim is that none of the above required touching the engine. Measured by hashing
`run.py`, `featurize.py` and **all 62 `tests/*.py`** immediately before installing the plugin and
again immediately after a run that emits its column and builds the ML tables:

```
protected paths fingerprinted BEFORE = 64 files
  run.py       = 6608DF5508F12BE85767C86F8C643CE7CC78E4D1A44EFBD1D51500ED06C7E0BD
  featurize.py = A6589B2788ABB3E4717AC84FE800BDABD31A9B848812DA04694FE28EAD8C32B8
...
files fingerprinted        = 64
files that MOVED           = (none)
run.py UNCHANGED           = True
featurize.py UNCHANGED     = True
every tests\*.py UNCHANGED = True
engine files mentioning the plugin = 0
```

Grepping `run.py`, `featurize.py` and every test file for `scms_demo_detector` or `demodet` returns
**zero hits**. The engine has never heard of this detector; it resolved a string out of a config
file.

**Why the comparison is per file rather than one rolled-up hash.** On a shared checkout, a single
fingerprint over 64 files cannot tell *"the plugin forced an engine edit"* apart from *"somebody else
was working in this tree while the demonstration ran"* — and the second happened here. A second
complete run of `ACCEPTANCE.ps1` twenty minutes later reported:

```
files that MOVED           = tests\test_plugin_api.py
run.py UNCHANGED           = True
featurize.py UNCHANGED     = True
every tests\*.py UNCHANGED = False   (moved: tests\test_plugin_api.py)
engine files mentioning the plugin      = 0
```

`tests/test_plugin_api.py` grew from 26 919 to 34 729 bytes at 15:52:13 while the run was in
progress, and it contains **zero** references to `scms_demo_detector` or `demodet`: a concurrent
editor, not this plugin. `run.py` and `featurize.py` carry the byte-identical sha256 they had 25
minutes and two full acceptance runs earlier. The per-file form is what makes that distinction
statable instead of guessable, and it is why the rolled-up form was replaced.

Reproduce the whole thing with `C:\Temp\scms_detector_demo\ACCEPTANCE.ps1`.

**`run.py` and `featurize.py` have since moved, and it is worth being exact about why.** The trust
model of §2 required six changes to the loader and the featurizer — `@builtins` expanding from a
fixed tuple, de-duplication by resolved column, the built-in-hijack guard, the source-gate call,
`NamespacedState` on the fusion slot, and a reason-code vocabulary that does not depend on which
codes happened to fire. None of them mentions a plugin, none of them is specific to
`scms_demo_detector`, and the grep above still returns **zero hits**. The claim this section makes is
*"adding a detector does not force an engine edit"*, which is unaffected; the claim it does not make
is *"the engine is finished"*. The sweep in §5.3 was re-run against the changed engine and reproduced
all four digests and all four metric rows byte for byte.

---

## 7. Provenance: what the manifest records

```jsonc
"plugins": {
  "api_version": "1.0",
  "interface_versions": {"ChannelModel": "1.0", "Detector": "1.0"},
  "provenance_digest": "0b6cc8763d8851ddaa8905333ecdb5c91d7c4cb52339e9df49d705c08a02b8da",  // see below
  "loaded": [ /* ... 15 built-ins ... */ {
    "slot": "check", "order": 13,
    "ref": "scms_demo_detector.threshold:PositionSpeedThreshold",
    "resolved_via": "dotted_path",
    "distribution": "scms-demo-detector", "version": "0.1.0",
    "dist_sha256":    "e9f57b3ce55fdf8c2d905a7f4d5a806b43666dcce31cbac249ecdac7dc8a41d0",
    "module_sha256":  "c1feca6e98cdd8a579519d9790860e5021c5439fdf72c114d1f36be444273b9f",
    "package_sha256": "1f6338ac52d49185d3510673feae73030b46e4f7a2d70663e6b54cb3c7136edd",
    "interface_version": "Detector/1.0",
    "capabilities": ["history", "stateful"],
    "params": {"tolerance_mps": 3.0, "max_accel_mps2": 5.0, "max_decel_mps2": 9.0,
               "min_interval_s": 0.5, "pos_conf_weight": 2.0, "speed_conf_mps": 1.0},
    "params_sha256": "6d519030f23512f99eb2fe829b9e930650ecb509cb73c83b4537c655a4f0a6a2",
    "conformance": {"suite": "v1", "passed": 7, "failed": 0, "ok": true,
                    "excluded": ["D6_off_by_default_is_byte_identical"]}
  }]
}
```

`dist_sha256` is the wheel RECORD's own hash — present here because the plugin is a normally
installed distribution rather than a loose file on `sys.path`. `params` is the **resolved** set,
defaults included, so a run that relied on a default is replayable even if the default later changes.

**`provenance_digest` covers the whole `loaded` list, built-ins included, and that is the lock
working rather than noise.** Two otherwise identical runs of the same config eight minutes apart
produced `0b6cc8763d8851dd…` and `7e8ebf611a746913…` while the plugin's own `module_sha256`
(`c1feca6e98cdd8a5…`) and `dist_sha256` stayed byte-identical. The cause was a concurrent editor
touching `mock_pipeline/netimport.py` — a *sibling* module inside the package the built-in checks
live in, which `package_sha256` hashes. That is precisely the hole phase 2's adversarial review
found and closed: hashing one source *file* let an edit to a sibling replay silently wrong with
`verify-plugins` reporting clean. Do not pin `provenance_digest` in a document; pin the per-entry
hashes, which are stable.

### 7.1 Drift is caught before step 0

Mutating **one byte inside a docstring** of the installed module — behaviour provably unchanged —
makes the manifest unreplayable:

```powershell
python -m scms_sim_ref.mock_pipeline.run verify-plugins C:\Temp\det_eval\suite_t3\manifest.json
```

```
  DRIFT   plugin drift in slot 'check' ref 'scms_demo_detector.threshold:PositionSpeedThreshold':
          module_sha256 expected 'c1feca6e98cdd8a579519d9790860e5021c5439fdf72c114d1f36be444273b9f',
          got            'b87e3e078da190c553789931905de9b44b7475a89acc34fbb3e95080eeda0364'.
verify-plugins exit = 2
```

and the replay itself:

```
PLUGIN DRIFT: ... re-run with --allow-plugin-drift to proceed and RECORD the drift.
replay exit = 2
output directory created = False        <- refused BEFORE step 0
```

`--allow-plugin-drift` proceeds **and writes the drift into the new manifest**. It does not silence
it: a silenced drift turns a replay into an unmarked different run, which is the exact failure the
lock exists to prevent.

---

## 8. Reference

### 8.1 The `check` config entry

```jsonc
{"ref": "pkg.module:Class",         // built-in name | entry-point name | dotted path
 "params": {"knob": 1.0},           // validated against the class's own FieldSpec bounds
 "conformance": "off" | "required", // "required" runs D1-D5,D7,D8 before step 0 and refuses failures
 "source_gate": "on" | "off",       // "on" (default) scans the module for engine-internal reach (§2.4)
 "isolated": false | true}          // true runs the check in its OWN interpreter (§2.8) -- the
                                    // labels are not in its process. Refused for a built-in;
                                    // defaults `source_gate` to "off"; state must be JSON;
                                    // costs ~50 us per delivered message.
```

`"@builtins"` as a bare array element expands to **the suite this engine version ships**
(`detectors.BUILTIN_CHECKS`), in canonical order — not to whatever happens to be in the process's
registry, so an installed distribution that calls `register_builtin()` at import time cannot add
itself to a run that never named it, and cannot take a built-in's name. The array's **order is
digest-bearing** (it fixes the fusion's stable-sort tie-break over equal scores), which is why it is
an explicit, replayable input and never a discovered one.

Two entries that **resolve to the same `(plugin_id, reason_code)`** — a subclass, an alias, a
re-export, the same class under two entry-point names — are refused by name, at config time and
again at load. `plugins.fusion` takes the same `source_gate` key.

### 8.2 Class attributes

| attribute | meaning |
|---|---|
| `interface_version` | `"Detector/1.0"`. A mismatched major is refused at load. |
| `plugin_id` | `[a-z0-9_]{2,32}`. Reserves your RNG namespace, config namespace and column prefix. |
| `reason_code` | your check's name. Emitted as `detnorm_x_<plugin_id>_<reason_code>`. |
| `precision` | decimals the engine rounds your score to **before** the `>= 1.0` compare. |
| `msg_types` | `("cam",)`, `("denm",)` or both. A message type you do not declare scores `0.0`. |
| `soft` | `True` → scored and emitted, never fires on its own. |
| `vru_suppressed` | `True` → not applied to a beacon self-declaring `station_type == "vru"`. |
| `gate` | `None`, `"station_type"` or `"denm"` — registered but only in the suite when the run enables that feature. |

### 8.3 Capabilities

Declare `{"stateful", "soft", "history", "rssi", "map", "neighbourhood", "event"}` as they apply.
`{"legacy_global_rng", "legacy_raw_compare"}` are **reserved**: they are grandfathering for the
built-ins (whose digests were pinned before the rules existed) and are refused from any third party.
A plugin declaring one is rejected at load with `CapabilityError`.

### 8.4 Things that will bite you

* **Do not import numpy** on the per-message path. BLAS thread counts and float reduction order
  drift across hosts, and `>= 1.0` is a cliff where a last-ulp difference flips a whole report.
* **Do not `import random` and call the module-level functions.** That is the process-global
  Mersenne Twister; use the `rng` you were handed.
* **Do not fire on `first_sight`.** There is no history; a check that scores the first message from
  every vehicle that drives into range destroys its own precision.
* **Fail at construction, not at step k.** A plugin that raises mid-loop produces a partial dataset
  whose digest matches nothing, and it must not take the SIGINT path that finalises a *valid*
  manifest.
* **Two checks may not share a `(plugin_id, reason_code)` pair.** They claim one column and the
  later one silently overwrites the earlier. The loader now refuses this by resolved column, at
  config time and at load, naming both entries and the pair.
* **Do not reach around the interface.** `sys._getframe`, `gc.get_referrers` and
  `import scms_sim_ref.mock_pipeline` are refused by the source gate (§2.4) and, more to the point,
  they make your detector's results worthless. Read §2 before deciding this rule does not apply to
  you. If your detector is going to be run **isolated** (§2.8) they will simply find nothing.
* **If your detector may be run isolated, keep its state JSON.** `state[...]` crosses a process
  boundary on every message in that mode, so it must hold numbers, strings, booleans, lists and
  dicts. Derived objects belong on `self`, which lives in the worker for the whole run. A value that
  will not serialise is refused by name at the first message, not silently dropped.

---

## 9. Known gaps

1. **~~The out-of-process detector mode does not exist.~~ It exists — `"isolated": true`, §2.8.**
   Same digest as in process on a 593-vehicle 300 s run, both alongside the built-ins and alone; the
   frame walk that reads the labels in process reaches only the serialiser's own frames; a crashing,
   hanging or desynchronised worker fails the run. Cost: **~40 µs per delivered message per isolated
   check** (4.3× the wall clock in the measured case) plus ~1 s of interpreter start.
   *(The earlier estimate that a per-message exchange was "~10⁴ round trips too hot" was an order of
   magnitude pessimistic for a PIPE: it is the right arithmetic for the 2 ms socket RTT
   `PLUGIN-ARCHITECTURE.md` §2.3 uses for an ns-3 backend, and a local pipe on this host is ~40 µs,
   which is 40× cheaper. Per-STEP batching remains the right shape for the CHANNEL slot and the wrong
   one here, because the fusion consumes one message's score before the next message is scored.)*
   **This run's own ground truth is withheld while the worker is alive** (added 2026-09-02, after it
   was measured leaking): the two streamed ORACLE tables are buffered and the workers are reaped
   before any of them is written, so the child finds no `ground_truth/` at all — +4.1 MB of peak
   working set on the InTAS AM peak, same `data_digest`. What is **not** closed by it is set out in
   §2.8: the child shares the filesystem with **every other dataset on the host** (measured — it read
   a completed earlier run's `gt_vehicle.jsonl`), it reports its own module identity, it gives up
   config-time schema introspection, and it is not a resource limit. Those are the next increments,
   and three of the four want an OS-level sandbox rather than more Python.
1b. **The integrity monitor checks at four named points, not continuously** — plugin resolution
   (import), channel load, detection-layer load, end of run. A plugin that tampers, uses the tamper
   and restores the binding before the next checkpoint is not seen by the identity snapshot — only
   by `WitnessedRandom`, and only if what it touched was the engine's own stream. Continuous
   enforcement is not achievable in-process; the next real increment is the same snapshot around
   **each step**, which shrinks the window without closing it, and is a cost decision rather than a
   design one.
2. **The source gate parses one module.** It reads the module the plugin class is *defined in* — not
   the helper modules that module imports, and nothing resolved at run time. Widening it to the whole
   distribution is mechanical (`registry.package_root` already finds the tree, and
   `registry.import_closure` already *hashes* that tree plus every non-stdlib module it imports) and
   is not claimed here. It is also defeated by `getattr(sys, "_get" + "frame")`, by an `eval`, and by
   anything else a name-matching pass cannot see; that is asserted in
   `tests/test_detector_trust_boundary.py::test_the_gate_is_a_guard_rail_and_the_repository_proves_it`
   so this paragraph cannot quietly become false.
2b. **The runtime guard covers plugin CALLS, not plugin LOADING — and this is the largest open gap
   in the in-process model.** `sys._getframe` taken in a module body or in `__init__` is not refused,
   `f_back`/`f_locals` are audited by nothing, and the captured frame is a live handle on
   `run_pipeline`'s locals — the broadcast dict, `veh.is_attacker`, the sender's true position — for
   the whole run. Measured end to end at the default `source_gate`
   ([§2.4.1](#241-the-runtime-guard--and-the-window-it-does-not-cover)) and pinned by a test that
   passes *by the attack succeeding*. It is not closable in-process: arming the hook over a plugin's
   module body would refuse `collections.namedtuple`, `typing.NamedTuple` and `logging`, and would
   still leave metaclasses, `__set_name__`, descriptors and properties open. The *use* of the engine
   RNG reached that way **is** closed (`integrity.stream_closed`); the *read* is not. **In-process
   plugins are trusted code, and a detector you cannot review must run isolated.**
3. **The gate's verdict is not in the manifest's plugin LOCK.** `source_gate: "off"` is recorded,
   because `cfg.plugins` is serialised verbatim into `manifest["config"]`, and it replays. But
   `manifest["plugins"]["loaded"][*]` carries no `source_gate` field of its own, so a consumer
   reading only the lock has to look at the config to see whether a plugin was screened. Adding the
   field means extending `registry.ProvenanceRecord`.
4. **No `Fusion` contract.** `D1`-`D8` grade a `Check`. `plugins.fusion` accepts no `conformance`
   key because there is nothing to run. A `FusionContract` (decide() purity, no global-rng touch,
   decision-shape sanity, and now the state-namespacing property the check slot's `D7` asserts) is
   the obvious next increment.
5. **`report_format` is still empty** — the third seam of the F2MD decomposition. The two shape bugs
   it is meant to fix (`detector_outputs` carrying only `reasons[0]`; `cert_validity` hard-coded
   all-`True` even for an invalid-signature subject) are untouched.
6. **No A/B harness.** `detectors_b: [...]` running a second suite in parallel under a gate — F2MD's
   V1/V2 pipelines — is not implemented. The `check` slot runs exactly one vector per run.
7. **`manifest["plugins"]["runtime"]` is `{}`** in a plugin-bearing run, although
   `verify-plugins` prints the interpreter and platform correctly from elsewhere in the manifest.
   Cosmetic, but §4.2 of the design specifies that block.

---

## 10. Where things live

| | |
|---|---|
| the interfaces | `src/scms_sim_ref/api/detect.py` — `Observation` (and `_seal`, which makes it read-only), `Check`, `Fusion`, `CheckBase`, `NamespacedState`, `VIOLATION_THRESHOLD` |
| **the source gate** | `src/scms_sim_ref/api/srcgate.py` — the refusal list, the message, and the module docstring stating what it is not |
| **the runtime guard** | `src/scms_sim_ref/api/guard.py` — the PEP 578 audit hook, the refused-event list, and the docstring's four named limits, of which the fourth (CALLS, not LOADING) is the one that decides the trust model |
| **the integrity monitor** | `src/scms_sim_ref/api/integrity.py` — `Sentinel` (the identity snapshot, §2.6), `WitnessedRandom` (the word-counting engine stream), `stream_closed` (the loading-phase closure of that stream), and the module docstring stating what it is not |
| **isolated (out-of-process) detectors** | `src/scms_sim_ref/api/isolate.py` — the wire protocol, `IsolatedCheck` (the engine-side proxy), `IsolatedRng`, and the child worker (`python -m scms_sim_ref.api.isolate --serve`). Its docstring states the transport decision and what the mode does not close |
| **out-of-process attestation** | `src/scms_sim_ref/conformance/attest.py` — the child interpreter that grades a candidate before the engine ever constructs it |
| the knobs | `src/scms_sim_ref/api/fields.py` — `FieldSpec` |
| the randomness | `src/scms_sim_ref/api/rng.py` — `RngNamespace` |
| the resolver + lock | `src/scms_sim_ref/api/registry.py` |
| the built-in checks | `src/scms_sim_ref/mock_pipeline/detectors.py` — 15 `Check` classes + `StreakFusion`, and `BUILTIN_CHECKS` / `BUILTIN_CHECK_BY_CODE`, the fixed suite `@builtins` expands from |
| the loader | `src/scms_sim_ref/mock_pipeline/run.py` — `default_check_refs`, `_assert_not_hijacked`, `_claim_column`, `build_checks` |
| the contract | `src/scms_sim_ref/conformance/v1/detect.py` — `CheckContract`, D1–D8 |
| **the trust-boundary tests** | `tests/test_detector_trust_boundary.py` — every claim in §2.1–§2.4, including the two escapes it admits to |
| **the runtime-guard tests** | `tests/test_plugin_runtime_guard.py` — §2.4.1: the obfuscated walk refused inside a call, the swallowed refusal, the `gc` route, the import tripwire, the load-time walk that **still lands** (pinned as a limitation), the loading-phase RNG closure and its honest twin, and the watched-class mutation end to end |
| **the integrity tests** | `tests/test_plugin_integrity.py` — §2.5–§2.7: the hostile constructor on all three slots, the import-time rebind, the attestation ORDER, the step-30 delayed rebind that conformance reports `ok` on, the rebind-then-restore only the stream witness catches, and the stateful `LinkOutcome` |
| **the isolation tests** | `tests/test_detector_isolation.py` — §2.8: identical digests both ways, the payload asserted against `Observation`'s own field list and against the serialised bytes, the same hostile class run BOTH ways, the crash / hang / desync / non-JSON-state refusals, and the filesystem hole |
| the functional tests | `tests/test_detector_plugins.py` — the zero-engine-edits claim and the digests |
| the design | `docs/realism/PLUGIN-ARCHITECTURE.md` §2.2, §3, §4, §5, §7 phase 3 |
| the worked example | `C:\Temp\scms_detector_demo` (`scms-demo-detector 0.1.0`) |
