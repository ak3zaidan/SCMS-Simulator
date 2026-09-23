This page walks **one plug-in end to end**: a local misbehaviour detector, from an empty
file to a model that passes the conformance kit. A detector is the family the roadmap's
Phase 4 acceptance criterion is written about — *"a researcher outside the team adds a
detector plug-in from the tutorial in under a day"* — and it is the family where the
engine's hardest rule lives, so it is the one worth learning first.

The finished article is in the repository at
[`examples/detector-heading-rate/src/lib.rs`](https://github.com/ak3zaidan/SCMS-Simulator/blob/main/examples/detector-heading-rate/src/lib.rs).
**Read this page with that file open.** Everything below is that file, in order, with the
reasoning that is not in the code.

If you want the propagation family instead, [Writing a plug-in](extending.html) is the
same walkthrough for a `Propagation` model.

## Budget

| Step | What | Roughly |
|---|---|---|
| 0 | run the example, watch the suite pass | 15 min (plus the first compile) |
| 1–2 | the card and the state | 1 h |
| 3–4 | the check and its cost | 2 h |
| 5–6 | registration and tests | 1–2 h |
| 7 | the conformance kit | 30 min |
| 8 | wire it into a scenario, and find out what does not work yet | 30 min |

The first compile of the conformance kit is the long pole: it pins the engine, the node
runtime and the server so it can police them.

## Step 0 — run the example before you write anything

```sh
cd examples
cargo test -p v2xw-example-detector
```

Twelve tests and the conformance suite. The suite prints a line per property:

```text
plug-in conformance: example/detector/heading-rate@1.0.0
  [pass] card-validates          id, tiers, parameters and rule R1
  [pass] card-api-version        targets 0.1.0 and this build is compatible
  [pass] card-sources            5 parameter(s), 1 still to calibrate
  [pass] card-tier-contract      tiers [Medium, High], 3 ignores
  [pass] determinism             12 value(s) identical across two runs
  [pass] reordering              each entity's values are the same whichever order …
  [pass] thread-independence     eight threads draw the same values as one
  [pass] quantisation            every value sits on the 0.001 grid
  [pass] card-declares-rng       drew from []
  [pass] stream-hygiene          0 cached stream(s) for 2 entities
  [pass] recorder-discipline     2 record(s), none ground-truth tainted
  [pass] no-owned-rng            1 file(s) clean
  [pass] no-wall-clock           1 file(s) clean
  [pass] no-ground-truth         1 file(s) clean
```

Then **break it on purpose**, because a check you have not seen fail is a check you do
not know the meaning of. Try each of these and put it back:

| Change | What should happen |
|---|---|
| delete the `calibration:` plan from `per_message_us` | `card-validates` fails: rule R1 refuses an uncited default with no plan |
| change `max_heading_change_deg` to `200.0` | `a_reversed_heading_fires_after_the_streak` fails — the check stops catching a reversal |
| replace `math::atan2` with `f64::atan2` | `no-wall-clock` still passes and the *transcendental* rule is not in this suite: see the note at the end of step 3 |
| add `use std::collections::HashMap;` and key the history on one | the suite still passes, and that is a gap you should know about — see step 7 |
| return the raw score instead of the quantised one | `quantisation` fails |

That last table is the most useful fifteen minutes on this page.

## Step 1 — what a detector is handed, and what it cannot ask for

```rust
pub trait Detector: Model {
    fn on_message(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        me: &SelfBelief,
        m: &ObservedMessage,
        env: &dyn LocalEnvironment,
    ) -> Verdict;

    fn cost(&self) -> DetectorCost;
}
```

Four arguments, and the interesting thing about them is what is absent.

- **`me: &SelfBelief`** — the node's *belief* about itself: its id, the time it thinks it
  is, where it thinks it is, and the range its own receiver is configured for. Under GNSS
  spoofing or during an outage this is far from the truth, and that is the point: a
  detector that used the truth here would never see a GNSS attack at all.
- **`m: &ObservedMessage`** — every field a claim or an observation of this receiver's.
  There is no true position, no actor id and no `is_attacker` flag, and there is no
  simulator clock.
- **`env: &dyn LocalEnvironment`** — the node's own map store, if it has one. A map is a
  *declared capability*, not a window on the world; a node without one supplies `NoMap`,
  whose honest answer is "every point is on the road", which makes the map check score
  zero rather than quietly borrowing the world.
- **`ctx: &mut dyn ThreatCtx`** — three capabilities and no more: the host's clock *for
  timestamping a record*, a keyed deterministic RNG stream, and a record sink.

So **invariant I-T2 — a detector reads belief, never ground truth — is a property of the
argument types.** A detector that wanted the truth could not ask for it and would not
compile. That is worth more than any amount of review, and it is why this family is the
one to learn the interface on.

There is one thing the types do not stop, and it is worth knowing: an implementor can
smuggle ground truth in through an associated type or a global. The module documentation
in `v2xw-core` used to claim such a model "fails to compile", which was false; the claim
was corrected and enforcement moved to where it can actually live, the conformance kit's
sentinel (step 7). An honest boundary with a test is worth more than an overstated one
without.

## Step 2 — the card

Write the card **first**, before the check. Not for tidiness: the card is the model's
declared interface, and writing it first is what makes you decide what your thresholds
*are* and where they came from before you have an implementation to rationalise.

```rust
let mut card = ModelCard::new(
    MODEL_ID,                    // "example/detector/heading-rate"
    Family::Detector,
    MODEL_VERSION,               // "1.0.0"
    "Scores the rate of change of a signer's claimed heading against what a vehicle \
     can physically do, from the receiver's own belief only.",
);
card.tier = vec![Tier::Medium, Tier::High];
```

Four rules the registry enforces, and one it does not:

**Every parameter the model reads at run time must be on the card** (invariant I-C3),
with a unit, a default and a source. The example has five: three thresholds, a streak
length and the CPU cost.

**A default with no citation must say so and carry a plan.** This is card rule R1 and the
registry *refuses* a card that breaks it:

```rust
Parameter {
    calibration: Some(
        "Plan: benchmark `Detector::on_message` on the reference laptop over the \
         1,330-message claim trace `v2xw-threat`'s legacy comparison test already \
         holds, divide by the message count, and record the figure with the machine it \
         was measured on. Until then this is an implementer's guess and a \
         saturated-node result is sensitive to it.".to_string(),
    ),
    ..Parameter::new(
        "per_message_us", "us", json!(1.0),
        Source::todo_calibrate("the CPU cost of one check, never measured"),
    )
}
```

A plan is not an intention. "Calibrate later" is an intention; the text above names the
machine, the corpus and the arithmetic. The parameter then appears on the
[calibration debt](calibration.html) page until somebody carries it out.

**A cited value whose *application* you changed is still a design choice.** The example's
90° bound is cited to veins-F2MD, where it bounds the disagreement between a claimed
heading and the heading implied by the claimed positions. Reusing it as a bound on the
heading *change* is a different quantity, so the citation goes in `Source::ref` and the
reuse goes in `Source::note` and in `card.limitations`. Cite the number; do not let the
citation cover a claim it does not support.

**`card.ignores` is read by a human deciding whether your model answers their question.**
Write it as the list of effects that are *not* in the answer. The example lists three,
and the first — "everything but one field's rate of change" — is the one that tells a
reader this is a fusion feature and not a detector on its own.

**The rule the registry does not enforce: `validation.status`.** Nobody checks that you
were honest. The example says `unit-tested`, which means "checked against its own
author's expectations" and nothing more. `literature-checked` is a claim that outputs
were compared against published figures, and the
[validation campaign](campaign.html) page is where a card that claims it without a
passing validation case gets named.

## Step 3 — the check

```rust
impl Detector for HeadingRate {
    fn on_message(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        me: &SelfBelief,
        m: &ObservedMessage,
        _env: &dyn LocalEnvironment,
    ) -> Verdict {
        let subject = m.signer_hex();
        let mut fingerprint = Fingerprint::default();
        let mut fired = Vec::new();
        // … gates, then the score, then the streak, then the record.
    }
}
```

Three rules here matter more than the arithmetic.

**Normalise so that 1.0 is the firing threshold.** Every detector in this project returns
`detnorm`: a score whose 1.0 is *its own* threshold. That is what makes scores from
checks measured in metres, degrees, seconds and counts comparable and fusable at all, and
it is the contract the machine-learning corpus's `detnorm_*` columns depend on. Divide by
your threshold; do not return raw degrees.

**Read the node's own clock, never the simulator's.** Every threshold in the example is
compared against `me.believed_time` and the *claimed* generation times.
`ctx.now()` appears exactly once, as the timestamp on the emitted record, because the
recorder needs every channel on one timeline. Using the host clock for a freshness
decision is precisely how a clock attack becomes invisible.

**Quantise before a threshold comparison.** Build decision D10: a transcendental result
must not drive a comparison whose outcome is compared across engines. The measurement
behind that rule: perturbing every transcendental by one unit in the last place moved one
legacy configuration's report count from 3,082 to 3,135. So:

```rust
pub fn score(&self, previous_heading_rad: f64, heading_rad: f64) -> f64 {
    let change_deg = angle_difference_rad(heading_rad, previous_heading_rad).abs()
        * 180.0 / core::f64::consts::PI;
    math::quantize_to(change_deg / self.params.max_heading_change_deg, SCORE_QUANTUM)
}
```

and the comparison is `if score >= 1.0`, on the quantised value. The number that reaches
the record is the number the comparison used.

**And never a standard-library transcendental.** `v2xw_core::math::sin_cos` and
`math::atan2`, not `f64::sin_cos` and `f64::atan2`: the standard library routes to the
platform's libm, whose precision varies by platform and by Rust version, and two machines
that disagree in the last bit of a score will eventually disagree about whether a
detector fired. The plug-in conformance suite does **not** check this — its three source
scans are for an owned generator, a wall clock and ground truth — but
`v2xw_conformance::firewall::TRANSCENDENTAL_RULES` exists and the engine's own crates are
scanned with it. Scan your own file with it; the rule is real whether or not the suite
you ran enforces it.

One more piece of arithmetic worth copying. The angle difference goes through `atan2` of
the sine and cosine rather than subtracting and wrapping:

```rust
pub fn angle_difference_rad(a: f64, b: f64) -> f64 {
    let (sin_a, cos_a) = math::sin_cos(a);
    let (sin_b, cos_b) = math::sin_cos(b);
    math::atan2(sin_a * cos_b - cos_a * sin_b, cos_a * cos_b + sin_a * sin_b)
}
```

The wrap-around form is the classic place a heading check goes wrong: a sender turning
from 359° to 1° has changed by 2°, not by 358°. `the_zero_two_pi_seam_is_not_a_turn` is
the test.

### Gate before you score

The example refuses to score three kinds of message, and each refusal is a claim:

- **not a beacon** — a different message type is a different check's business;
- **not attributable** — a content check on an unverified or badly signed message is an
  accusation against whoever's digest happened to be on the envelope. The legacy suite
  zeroes every plausibility check on a bad signature for exactly this reason;
- **too slow** — a stationary vehicle's claimed heading is not meaningfully defined.

Use `matches!(m.kind, ObservedKind::Beacon)` rather than a `match` with an arm per
variant. `ObservedKind` gains variants as the message layer grows, and a detector that
only looks at beacons should not have to be edited every time one appears.

## Step 4 — the cost, and why it is not free to get wrong

```rust
fn cost(&self) -> DetectorCost {
    DetectorCost { per_message_us: 1.0 }
}
```

The node runtime charges this against the hardware profile's CPU budget, so a wrong
number changes how many messages a saturated node gets through — which changes what the
detector sees, which changes the result. `DetectorCost` has nowhere to record that the
figure is a guess, which is why the guess is declared on the card as a `todo-calibrate`
parameter with a plan.

## Step 5 — register it

```rust
pub fn register(
    registry: &mut Registry,
) -> Result<ModelRef, RegistryError> {
    let model: ModelHandle = std::sync::Arc::new(HeadingRate::with_defaults());
    registry.register_model(model)
}
```

`register_model` validates the card, hashes its canonical bytes and stores the
registration. It refuses:

| Refusal | Cause |
|---|---|
| card validation error | an empty tier list, an empty purpose, a duplicate parameter name, a default outside its own declared range, or a `todo-calibrate` parameter with no plan |
| duplicate id | two *different* cards under one id. The same card twice is fine |
| licence gate | a copyleft licence declared for an in-process model — GPL and LGPL plug-ins run out of process only |

The handle is `Arc<dyn Model + Send + Sync>`, and the bound is load-bearing: a model that
captured an `Rc`, a `Cell` or a thread-local — the shapes that make behaviour depend on
where the code ran — cannot be stored in the registry at all.

## Step 6 — the tests you owe

Write these before you believe the model.

1. **The card validates.** One line, and it is the cheapest possible guard against a card
   the registry would refuse in the middle of somebody else's run.
2. **The check fires on the thing it is for**, and you have *seen that test go red*. The
   example's comment says exactly what to change: `max_heading_change_deg` to 200. A test
   that fires on any input at all is measuring the plumbing.
3. **The check does not fire on the honest case.** `a_gentle_turn_does_not_fire`: 10° per
   beacon through a junction. Without this one you have a detector with a 100 % false
   positive rate and a passing test suite.
4. **Each gate works.** One test per refusal from step 3.
5. **State is per subject, not per node.** `two_signers_do_not_share_a_streak`:
   alternating signers, each violating once. If the streak were kept per node the third
   beacon would fire.
6. **Every score sits on its grid.**

> **This project has a recurring defect class: checks that cannot fail.** Four were found
> in one review pass, and the message layer being an unsigned stub survived a whole
> vertical-slice audit because every check around it was satisfiable without the thing
> under test working. A cooperative awareness message and a basic safety message both
> encoded to exactly 101 bytes; one assertion that two different formats produce
> different sizes would have caught it on day one. **Inject the fault. Watch the check go
> red. Then fix it back.**

## Step 7 — the conformance kit

This is the step that makes the difference between "it compiles" and "the engine can run
it". The kit lives at `tests/conformance/` and is a **library**, not a test file,
precisely because its users are outside this repository: your model in your crate cannot
be reached by a `#[test]` that lives in ours.

```toml
[dev-dependencies]
v2xw-conformance = { path = "../../tests/conformance" }
```

```rust
use v2xw_conformance::plugin::{PluginUnderTest, ProbeCtx, run_suite};

struct Subject { model: HeadingRate }

impl PluginUnderTest for Subject {
    fn model(&self) -> &dyn Model { &self.model }

    fn entities(&self) -> Vec<EntityRef> {
        vec![EntityRef::Node(NodeId::new(1)), EntityRef::Node(NodeId::new(2))]
    }

    fn exercise(&self, ctx: &mut ProbeCtx, entity: EntityRef) -> Vec<f64> { /* … */ }

    fn sources(&self) -> Vec<std::path::PathBuf> {
        vec![std::path::PathBuf::from(file!())]
    }

    fn quantum(&self) -> f64 { SCORE_QUANTUM }
    fn runs_as_node(&self) -> bool { true }
}

#[test]
fn the_conformance_suite_passes() {
    let report = run_suite(&Subject::new(), 0x5EED);
    assert!(report.complete(), "{report}");
}
```

Five things about that, in order of how much time each will save you.

**`exercise` takes `&self` and `on_message` takes `&mut self`.** The suite runs `exercise`
on eight threads at once, so it cannot hand you a mutable model. Build a fresh detector
*inside* `exercise` and feed it a scripted sequence. That is not a workaround: it makes
the reordering property mean something, because the state under test is the state inside
one call sequence rather than state leaking between entities.

**Make the two entities produce *different* numbers.** The example derives the heading
step from the node index, so node 1 turns 50° per beacon (score 0.556, never fires) and
node 2 turns 100° (score 1.111, fires). Two entities returning identical vectors make the
per-entity comparison a comparison of two copies of the same thing.

**`assert!(report.complete())`, not `report.passed()`.** `passed()` tolerates a check the
suite could not run; `complete()` requires every check to have run *and* passed. A model
that implements `sources()` and `quantum()` can reach `complete()`, and an absent check
must never read as a passed one.

**Name your source files.** A plug-in that supplies none gets no scan and the report
prints `----`. The three scans are textual and therefore blunt — that is the same trade
the node sentinel makes, and for the same reason: the failure they guard against is a
plausible-looking line nobody notices, and a textual rule is one a reviewer can check by
eye. They scan for `thread_rng`, `rand::`, `OsRng`, `StdRng`, `ChaCha`,
`RngRegistry::new`, `SystemTime::now`, `Instant::now`, `UNIX_EPOCH`, `Kinematics`,
`.world()`, `.actors()` and `ActorId`, outside `#[cfg(test)] mod` blocks.

**`runs_as_node: true` is what turns invariant I-C2 into a check.** With it set, every
record your model emitted during `exercise` is inspected, and a `Gt` or `NodeAndGt`
record fails the suite. `DetObservation` is `Visibility::Node`, so the example passes —
and the example *does* emit one, because the firing entity crosses its threshold. A
subject whose exercise emits nothing passes this check vacuously, which is worth knowing
before you trust it.

### What the suite does not check

Say it out loud, because an unchecked property is not a satisfied one:

- **`HashMap` iteration order.** `HASH_ORDER_RULES` exists in
  `v2xw_conformance::firewall` and the plug-in suite does not apply it. Use `BTreeMap`
  and `IndexMap`; nothing here will tell you that you did not.
- **Standard-library transcendentals.** As above: the rules exist, the plug-in suite does
  not run them.
- **That your model is *right*.** Every property the suite checks is a property of the
  *plumbing*. A detector that scores every message 0.0 passes every check on that list.

The kit has two other suites you may want: `firewall` (the ground-truth and wall-clock
scanners, applicable to any file) and `golden` (byte-identical output across two runs and
three platforms).

## Step 8 — select it from a scenario, and find out what does not work yet

Here is the honest state of the wiring, and you want it before you spend an afternoon on
it rather than after.

```yaml
detection:
  local:
    - id: example/detector/heading-rate
```

`v2xw_engine::phase2::Phase2::build` reads `detection.local` and **accepts exactly one
id**:

```rust
for choice in &scenario.detection.local {
    if choice.id != LEGACY_12 {
        return Err(conflict("detection.local", format!(
            "this build ships one local detector suite, {LEGACY_12}; got {}", choice.id)));
    }
}
```

So your detector is registered, hashed, pinned in the manifest and on this site's
[model reference](models.html), and the engine will refuse a scenario that selects it, by
name. It is not a silent failure — which is the important part — but it is a gap. What is
owed is a resolver: `Phase2::build` looking the id up in the registry and building a
`Box<dyn Detector>` from it instead of comparing against a constant. Your model needs no
change when that lands.

Until then, drive your detector from its own tests over a recorded claim trace.
`v2xw-threat`'s legacy comparison test does exactly this with a 1,330-message trace
covering all 28 attack renderings, and it is the right harness for a detector under
development: it is faster than a simulation and it is the same data every time.

## The interface limitation you will hit

`Verdict` names its checks with `DetectorId`, which is a **closed enumeration of the
ported legacy fifteen**. An out-of-tree detector cannot name a new check: it has to report
under the nearest existing id, and the example reports under
`DetectorId::HeadingInconsistency`. That is honest — it *is* a heading inconsistency — but
two different checks then share one column of the fusion fingerprint, and a
machine-learning model cannot tell them apart.

Your detector is visible by name on `det.observation`, which carries the detector as a
string, and invisible in the fingerprint. Closing it properly means either a
`DetectorId::Plugin(..)` variant derived from the model id the way `RngDomain::Plugin`
already is, or a `Verdict` that carries `&'static str` check names beside the
enumeration. It is recorded here rather than left to be discovered.

## Checklist

- [ ] the card validates, and every parameter has a unit, a default and a source
- [ ] every uncited default has a `calibration` plan that names a machine and a method
- [ ] `card.ignores` lists what your answer leaves out
- [ ] `validation.status` is the *kind* of check you actually did
- [ ] scores normalised so 1.0 is the threshold
- [ ] decisions read the node's believed time; `ctx.now()` only timestamps records
- [ ] quantised before every threshold comparison
- [ ] `v2xw_core::math`, never `f64::`, for every transcendental
- [ ] `BTreeMap`, never `HashMap`, for anything that can reach an output
- [ ] a test that fires, a test that does not, and one test you have watched go red
- [ ] `PluginUnderTest` implemented, `sources()` named, `quantum()` declared,
      `runs_as_node()` set, and `report.complete()` asserted
- [ ] you know which properties the suite did **not** check

## Where to go next

- [`examples/`](https://github.com/ak3zaidan/SCMS-Simulator/blob/main/examples/README.md)
  — the other three worked plug-ins: a propagation model, a car-following controller and
  a metric provider. Each demonstrates something this one does not; the table in that
  README says which.
- [Writing a plug-in](extending.html) — the same walkthrough for `Propagation`, including
  what a model that *does* draw random numbers has to declare.
- [Defect register](defects.html) — what independent review found in code whose tests
  were green. Read it before you trust your own suite.
- [Validation campaign](campaign.html) — what has been checked against the world, and
  what has not.
