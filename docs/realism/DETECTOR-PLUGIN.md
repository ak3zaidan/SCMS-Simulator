# Writing a detector plugin — the thresholding guide

**Status:** phase 3 shipped and measured. Every command, digest, exit code and metric below was run
on this host (Windows Server 2022, CPython 3.12.10, `PYTHONHASHSEED=0`) against an out-of-repo
distribution installed as its own wheel. Nothing here is illustrative.

**Full engine suite: `951 passed in 1033.74s`, exit 0.** Reference digest
`b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` reproduced with the plugin
distribution installed and with no plugins declared. Reproduce the functional evidence with
`C:\Temp\scms_detector_demo\ACCEPTANCE.ps1`.

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
> It does not — and cannot — stop a plugin that means to read the labels. A detector you genuinely
> do not trust must not be run in this mode.

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

### 2.3 Why this cannot be fixed in-process, and what was done instead

Python gives every callable in a process the same reflective powers: `sys._getframe`,
`inspect.currentframe`, `gc.get_objects`, `gc.get_referrers`, module globals, `__subclasses__`,
`__closure__`, `ctypes`. No arrangement of frozen dataclasses, wrapper objects or namespaced
mappings changes that, and stacking more wrappers only makes a false claim harder to disprove. So
the engine does three things that *are* true, and claims nothing more:

| layer | what it actually guarantees | what defeats it |
|---|---|---|
| **`Observation`** | no ground truth reachable **by name**; nothing attachable; no declared field writable via `setattr` / `object.__setattr__` / `del` | `type(obs).claimed_x.fget.__self__.__set__(obs, v)` — the saved slot descriptor |
| **`NamespacedState`** | the mapping interface lands in `plugin:<id>`; reserved keys raise on write; `h` comes back a tuple and `streak` a read-only proxy over a **copy** | `object.__getattribute__(ns, "_st")` — the wrapper holds the engine's dict |
| **the source gate** | refuses frame walking, `gc` reflection, trace hooks, `ctypes`, `eval`/`exec`, and engine-internal imports, at load, by line number | `getattr(sys, "_get" + "frame")`; an `eval`; a helper module the gate does not parse |

Both residues are **pinned by tests** (`tests/test_detector_trust_boundary.py`), so if a future
change closes one, the test fails and this table has to be upgraded rather than left overstating.

The honest boundary is a process boundary: a detector in its own process, with an explicit message
interface, where the oracle is simply not in the address space. That is the next phase, and it is
the only answer for genuinely untrusted code. Until it exists, treat "who wrote this detector?" as
the same question as "who wrote this dependency?".

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

### 2.5 What this means if you are running a benchmark

If you are collecting detectors from other people and comparing them:

1. **Read the source.** The gate makes the obvious reaches loud; it does not make reading optional.
2. **Pin the content hashes.** `manifest["plugins"]["loaded"][*]` carries `module_sha256`,
   `package_sha256` and `dist_sha256`; §7.1 shows the drift refusal. What you reviewed is then what
   ran.
3. **Do not treat a reproduced digest as evidence of honesty.** It is not. A label-reading detector
   reproduces its digest exactly.
4. **Use the conformance suite's D2 and D3** (§4.1) — and know their blind spot. D2 records every
   attribute a check touches and refuses undeclared ones; D3 scores two streams identical in every
   declared field where one also carries ground truth, and fails the check if the scores differ.
   Together they catch a detector that reads the oracle *off the observation*, at run time, which is
   a complementary instrument to the gate's static one. **Neither sees a frame walk**: measured, a
   frame-walking check passes all six with `ok = True` (§4.1).
5. **For submissions you cannot review, wait for the out-of-process mode.**

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
 "source_gate": "on" | "off"}       // "on" (default) scans the module for engine-internal reach (§2.4)
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
  you.

---

## 9. Known gaps

1. **The out-of-process detector mode does not exist.** This is the load-bearing gap and everything
   in §2 depends on it. Until a detector can run in its own process with the oracle outside its
   address space, "untrusted detector" is not a supported configuration, and the guard rails in this
   engine are exactly that.
2. **The source gate parses one module.** It reads the module the plugin class is *defined in* — not
   the helper modules that module imports, and nothing resolved at run time. Widening it to the whole
   distribution is mechanical (`registry.package_root` already finds the tree) and is not claimed
   here. It is also defeated by `getattr(sys, "_get" + "frame")`, by an `eval`, and by anything else
   a name-matching pass cannot see; that is asserted in
   `tests/test_detector_trust_boundary.py::test_the_gate_is_a_guard_rail_and_the_repository_proves_it`
   so this paragraph cannot quietly become false.
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
| the knobs | `src/scms_sim_ref/api/fields.py` — `FieldSpec` |
| the randomness | `src/scms_sim_ref/api/rng.py` — `RngNamespace` |
| the resolver + lock | `src/scms_sim_ref/api/registry.py` |
| the built-in checks | `src/scms_sim_ref/mock_pipeline/detectors.py` — 15 `Check` classes + `StreakFusion`, and `BUILTIN_CHECKS` / `BUILTIN_CHECK_BY_CODE`, the fixed suite `@builtins` expands from |
| the loader | `src/scms_sim_ref/mock_pipeline/run.py` — `default_check_refs`, `_assert_not_hijacked`, `_claim_column`, `build_checks` |
| the contract | `src/scms_sim_ref/conformance/v1/detect.py` — `CheckContract`, D1–D8 |
| **the trust-boundary tests** | `tests/test_detector_trust_boundary.py` — every claim in §2, including the two escapes it admits to |
| the functional tests | `tests/test_detector_plugins.py` — the zero-engine-edits claim and the digests |
| the design | `docs/realism/PLUGIN-ARCHITECTURE.md` §2.2, §3, §4, §5, §7 phase 3 |
| the worked example | `C:\Temp\scms_detector_demo` (`scms-demo-detector 0.1.0`) |
