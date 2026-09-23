# `examples/` — one worked plug-in per interface

Four small crates, each a real plug-in against one of the engine's plug-in families.
They exist to be **read and copied**: nothing in the engine depends on any of them, so
you can break them freely, and each one is the smallest thing that is still a genuine
model of its kind rather than a stub.

| Crate | Family | Trait | What it is |
|---|---|---|---|
| [`propagation-log-distance/`](propagation-log-distance/src/lib.rs) | `propagation` | `v2xw_radio::traits::Propagation` | log-distance path loss anchored to Friis, with a per-link log-normal shadowing draw |
| [`carfollowing-constant-time-gap/`](carfollowing-constant-time-gap/src/lib.rs) | `mobility` | `v2xw_mobility::traits::CarFollowing` | the PD controller an adaptive-cruise-control system implements over a constant-time-gap spacing policy |
| [`detector-heading-rate/`](detector-heading-rate/src/lib.rs) | `detector` | `v2xw_threat::Detector` | a heading-rate plausibility check, from the receiver's own belief only |
| [`metric-detector-load/`](metric-detector-load/src/lib.rs) | `metric` | `v2xw_metrics::MetricProvider` | two metrics over `det.observation`: firings per detector, and how much of that is one subject over and over |

**Start with the detector.** `docs/site/content/tutorial.md` walks it end to end — card,
model, trait, registration, tests, the conformance kit and a scenario that selects it —
and it is the family the roadmap's Phase 4 acceptance criterion is written about. The
propagation family gets the same treatment in prose at
`docs/site/content/extending.md`.

## Running them

```sh
cd examples
cargo test                                   # all four
cargo test -p v2xw-example-detector          # just the detector, including the
                                             # conformance suite
```

This is **its own cargo workspace**, not a member of the root one, for two reasons that
matter to a reader:

1. An example must be built the way a contributor's crate is built. A researcher adding
   a detector writes a crate *outside* this repository that depends on `v2xw-core` and
   one family crate; if these were workspace members they would silently inherit the
   root's `[workspace.dependencies]` and lock file, and would compile under conditions
   the reader cannot reproduce. Every dependency in `examples/Cargo.toml` is spelled out
   with a version, as an out-of-tree crate must spell it.
2. Building the examples must not change what the engine's build does. `cargo build
   --workspace` at the repository root does not see this directory at all.

The cost is a second `target/` and a second `Cargo.lock`. `docs/site/tools/cardgen` is
arranged the same way and for the same reason.

## What each one demonstrates, and where

Every example carries a model card, so every one of them demonstrates the card rules.
Beyond that they are deliberately *different* from each other:

| Rule | Where to read it |
|---|---|
| a source for every default | all four; `propagation-log-distance` cites a design-document table, `carfollowing-constant-time-gap` has three uncited gains and therefore three calibration plans |
| `todo-calibrate` plus a plan, and what rule R1 refuses | `carfollowing-constant-time-gap`, `every_uncited_default_carries_a_plan` |
| randomness from a keyed `(domain, entity)` stream | `propagation-log-distance`, `two_links_do_not_share_a_shadowing_draw` |
| a family trait that needs no context at all, because the model is pure | `carfollowing-constant-time-gap` |
| never a standard-library transcendental | `propagation-log-distance` (`math::log10`), `detector-heading-rate` (`math::sin_cos`, `math::atan2`) |
| the ground-truth firewall as a property of the argument types | `detector-heading-rate` |
| a node's decisions read the node's own clock, never the simulator's | `detector-heading-rate` |
| quantise before a threshold comparison (D10) | `detector-heading-rate` |
| reading channels rather than engine state | `metric-detector-load` |
| a decode failure counted rather than swallowed | `metric-detector-load` |
| a proportion gets a Wilson interval or a refusal, never a bare ratio | `metric-detector-load` |
| the plug-in conformance suite of 03-interfaces.md §17, wired up | `detector-heading-rate`, module `conformance` |
| a test that has been *shown to fail* | all four; each one says in its comment what to change to make it go red |

## Two honest notes

**The `examples/` versus `plugins/examples/` question.** ADR 0010 §1 lists
`plugins/examples/` in the repository layout, and a placeholder README stands there
saying "one worked example per plug-in interface". These crates are here instead because
`examples/` is the path the build task assigned, and one set of examples in two places
would be worse than one set in the less canonical place. Reconciling it is `git mv
examples plugins/examples` plus the three path references in the tutorial; no code
depends on the location.

**A registered example is not yet a selectable example.** All four register through
`v2xw_core::registry`, which validates the card, hashes it and puts the model in the run
manifest and on the generated model reference. Selecting one *from a scenario* is a
different matter and depends on the family:

- `detection.local` and `threats.attackers[].id` are read by the engine, but
  `v2xw_engine::phase2::Phase2::build` accepts exactly one detector id
  (`detect/legacy-12`) and refuses anything else by name.
- `radio.models` is in the scenario schema and `v2xw_engine::wiring::build_radio`
  does not read it: the propagation and fading models are selected from the tier alone.
- Mobility models are likewise selected in `wiring`, not from the scenario.
- A metric provider is installed by `v2xw_metrics::register_all` rather than by name.

So an example plug-in today is registered, hashed, documented and unit-tested, and is
run by its own tests rather than by the engine. That is a gap in the wiring, not in the
interfaces, and the tutorial says exactly which function has to learn to read which
field. It is worth knowing before you spend a day expecting a scenario to pick your
model up.
