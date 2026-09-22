The V2X World Simulator simulates vehicle-to-everything traffic on a real street
network, at any scale from one vehicle to a congested city, with every node behaving
the way a deployed one would: ground-truth traffic, frame-level radio, node hardware
limits (CPU, hardware security module, queues, storage), real or modelled cryptography,
and pluggable credential-management protocols.

It is built as a research instrument rather than a demonstration. That has one
practical consequence which runs through this entire site: **a result is only worth as
much as the statement of what produced it.** So every model states its equations, its
parameters with units, the citation behind each default, and what its fidelity tier
does not model. Runs are deterministic and reproducible across macOS, Linux and
Windows, recorded to MCAP, and replayable.

## Read this first

This documentation deliberately leads with what is missing. Three of the pages in the
sidebar exist for nothing else:

- [Calibration debt](calibration.html) — every default that is still an implementer's
  guess rather than a cited value, generated from the model registry so it cannot go
  stale.
- [Validation status](validation.html) — which models have been checked against
  something outside this repository, and which have only been checked against
  themselves.
- [Defect register](defects.html) — what independent review found in code that had
  already passed its own tests, and what the pattern of those defects should teach a
  reader about where to be sceptical.

A tool that hides its own bug history is harder to trust, not easier.

## What is actually built

The engine runs, deterministically, and the layers below messaging are verified. The
messaging layer is not yet what the rest of the stack implies, and the project's own
build record says so:

{% include docs/design/BUILD-STATUS.md#Correction, 2026-09-22 | demote=2 %}

The living version of that record, including the crate-by-crate state and the Phase 1
acceptance criteria, is [`docs/design/BUILD-STATUS.md`](docs/design/BUILD-STATUS.md).

## The rules the code is held to

Five rules explain most of the design decisions a reader will meet. They are not style
preferences; each exists because breaking it produced a wrong answer somewhere.

| Rule | Why |
|---|---|
| Every transcendental goes through `v2xw_core::math`, never the platform libm | The standard library documents its precision as varying by platform and even within one execution, which would make two machines disagree about a simulation |
| Every random draw comes from a stream keyed by `(domain, entity)` | One entity's draws then never depend on another entity's activity, on event order, or on the thread count |
| No wall-clock reads in engine-facing code | Time is `SimTime`; a run that read a real clock could not be reproduced |
| Every exported float is quantised at the writer | A digest then survives a change of compiler, math library or target. The rule exists because exactly one unquantised field once broke a corpus of golden digests |
| A node reads its own beliefs, never ground truth | A detector that can see the truth it is supposed to infer is a detector that always works. A conformance sentinel enforces the separation |

And one rule about documentation, which is why this site is generated rather than
written: **every model card cites a source for every default, and a default with no
source must say so and carry a plan to fix it.**

## Where to go next

| If you want to | Read |
|---|---|
| Understand the shape of the engine | [Architecture](architecture.html) |
| Know what a fidelity tier does and does not model | [Methodology](methodology.html) |
| Look up a model, its equations and its defaults | [Model reference](models.html) |
| Write a scenario file | [Scenario schema](scenario.html) |
| Add your own model, detector or attacker | [Writing a plug-in](extending.html) |
| Decide how much to trust a number | [Validation status](validation.html) and the [defect register](defects.html) |
