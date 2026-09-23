# The calibration-issue register and the completeness gate

`docs/design/10-roadmap.md` Phase 6 states one release gate in one line:

> model-card completeness gate (no `todo-calibrate` on a `high`-tier default without a
> calibration issue)

This directory holds the register the gate reads. The gate itself is
`crates/v2xw-metrics/src/gate.rs`, and it is a function over the registry the engine
actually builds — not a review step, not a checklist item somebody ticks.

## What the gate adds to what the card schema already does

Registry rule R1 is enforced by `ModelCard::validate` and therefore by every registration
path: a parameter whose `source.kind` is `todo-calibrate` **must** carry a `calibration`
plan, or the model cannot be registered. That is already stronger than most simulators
manage, and it is not enough for a release, because a plan is a sentence an implementer
wrote about work nobody has agreed to do.

The register records the part a plan cannot: **who**, **how**, and **tracked where**. An
issue has an id, a one-line title, an owner, a state, the measurement that would close it,
and the parameters it covers.

| | Enforced by | Says |
|---|---|---|
| Rule R1 | `ModelCard::validate`, at registration | this number is uncited, and here is how it could be measured |
| The Phase 6 gate | `v2xw_metrics::gate::run`, at release | …and this named person is measuring it, this way |

## The state of the register today

`issues` is empty. Nobody has been assigned a calibration measurement, so the gate fails
on every uncalibrated `high`-tier default in the registry — roughly **159** of them over the
registry the card exporter builds, of which **141** are hardware-profile fields, counted
field by field from the eleven shipped YAML profiles. `docs/RELEASE-CHECKLIST.md` §5 breaks
the figure down and says which parts of it are exact.

That number is not a reason to weaken the gate. It is the reason the gate exists: the
hardware model is the single biggest source of unsourced numbers in the engine, every one
of those numbers sets the compute load of every run that uses the profile, and until now
nothing counted them in one place where a release decision could see them.

## The file format

`issues.json`, schema `v2xw/calibration-issues/1`. Unknown keys are ignored, so a key
beginning with `_` carries prose for a reader without reaching the parser.

```json
{
  "schema": "v2xw/calibration-issues/1",
  "issues": [
    {
      "id": "CAL-001",
      "title": "one line saying what is uncalibrated",
      "owner": "who will measure it",
      "state": "open | in-progress | blocked | closed",
      "covers": ["model-id::parameter", "model-id::prefix*", "model-id::*"],
      "measurement": "the measurement that would close it",
      "tracker": "optional: where the work is tracked",
      "blocked_by": "required reading when state is blocked"
    }
  ]
}
```

### States, and which of them count as coverage

| State | Covers a parameter? | Meaning |
|---|---|---|
| `open` | yes | accepted as work to be done; nobody has started |
| `in-progress` | yes | somebody is measuring it now |
| `blocked` | yes | tracked work that cannot proceed; `blocked_by` says why |
| `closed` | **no** | finished — and a closed issue against a parameter that is *still* `todo-calibrate` is a contradiction, so the gate reports it as one |

A `blocked` issue counts because "the vendor will not release the datasheet under NDA" is
a real answer that a reader of the documentation site needs to see. A `closed` issue does
not, because closing it should have replaced the parameter's `todo-calibrate` source with
a citation; if it did not, either the measurement was never made or the card was never
updated, and both are worth a red build.

### The coverage-pattern grammar

A pattern is `model-id::parameter`:

* the **model half** must be a literal registry id. A `*` there is refused;
* the **parameter half** may be a literal name, a trailing-`*` prefix (`hsm.ops.*`), or
  `*` for every parameter of that model.

A malformed pattern is itself a gate failure. It is not skipped and it is not treated as
coverage, because a pattern is the only thing standing between a tracked number and an
untracked one, and a line that does not parse must not silently become either.

**This is deliberate and it is the point.** `*::*` would satisfy the gate for the entire
engine in one line, so it does not parse. The recurring defect in this repository is a
check that cannot go red; a gate whose register can be written to always pass is that
defect with extra steps. `crates/v2xw-metrics/src/gate.rs`'s test
`a_registry_wide_wildcard_is_refused_and_fails_the_gate` injects exactly that line and
asserts the gate stays red.

## Running it

```sh
cargo run --release --manifest-path docs/site/tools/cardgen/Cargo.toml -- \
    --out docs/site/generated/cards.json \
    --issues docs/calibration/issues.json \
    --gate
```

`--gate` makes the exporter exit non-zero when the gate fails, after printing every
failure with its model, its parameter, its current default and what the default stands
for. Without `--gate` the verdict is still computed and written into the dump, where the
documentation site renders it as the *Completeness gate* page; a build that only looked at
the exit status would hide the work list from the people who have to do the work.

## What the gate does not claim

The exporter builds the registry through the crate-level registration entry points that
exist today, which reach five crates. The dump states that coverage gap in
`coverage.uncovered`, the site prints it, and the gate page repeats it. A partial registry
must not read as a clean one — so the true repository-wide figure is **larger** than the
number the gate prints, and the checklist says so.
