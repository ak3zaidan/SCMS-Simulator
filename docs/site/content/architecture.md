The engine is a discrete-event simulator with a single-threaded event loop and
phase-parallel work inside each step. Seventeen Rust crates sit in one dependency
order, and the order is the architecture: a crate may only reach downwards, so it is
impossible for the radio to depend on message semantics or for a detector to reach the
ground truth.

This page is assembled from the design document rather than restated, so the component
map here and the one the engineers work from are the same text. The whole document is
[`docs/design/02-architecture.md`](docs/design/02-architecture.md).

## The shape in one paragraph

A scenario file names a world source, a demand model, a radio stack, a message set, a
security envelope, hardware profiles, the threats and the metrics. The loader resolves
every one of those names against the **model registry**, which holds a model card per
model; the manifest then pins each card's content hash, so a replay that silently used
a different model is refused rather than quietly producing different numbers. The
kernel advances simulated time in nanoseconds, ordering events by
`(time, priority, sequence)`. Around it, each phase — mobility, propagation, reception,
node work — is a pure map over actors merged back in id order.

{% include docs/design/02-architecture.md#2. System overview | demote=1 %}

## How a step flows

{% include docs/design/02-architecture.md#3. Data flow | demote=1 %}

## Time

{% include docs/design/02-architecture.md#5. Time model | demote=1 %}

## Determinism

The reason the engine can promise that the same scenario, seed and build produce
byte-identical output on three operating systems is that four mechanisms are enforced
in the contract crate rather than left to each model's discipline: counter-based random
streams, pure-Rust transcendentals, fixed event priorities, and id-ordered reductions.
A fifth, writer-side quantisation, protects everything that leaves the process.

{% include docs/design/02-architecture.md#6. Determinism model | demote=1 %}

## The two firewalls

Two invariants are enforced by the type system rather than by convention, and a crate
that works around either is breaking a published guarantee rather than a style rule.

**I-C2, the ground-truth firewall.** A plug-in that runs as a node — a detector, a
message generator, an attacker — receives a `NodeView`, never the simulation context's
world. It therefore cannot read the truth its own output is supposed to be measured
against. During the build, three deliberate violations were written and the conformance
sentinel went red on each, naming file and line; that is the standard of evidence this
project holds a check to, because a check that has never been shown to fail is not a
check.

**Writer-side quantisation.** Every float that reaches a recorded, exported or digested
artefact goes through `v2xw_core::math::quantize_to` first, rounded to its field's
declared quantum. The rule exists because a corpus of golden digests once broke when
exactly one field escaped rounding.

## Plug-ins and version pinning

{% include docs/design/02-architecture.md#8. Plug-in system | demote=1 %}

See [Writing a plug-in](extending.html) for a worked example against one of those
seams, and the [model reference](models.html) for every model currently registered.
