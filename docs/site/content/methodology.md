Every family of models — mobility, propagation, PHY, MAC, compute, backend, crypto,
perception — is available at three fidelity tiers, and a scenario picks one tier per
family. The tier is not a quality setting. It is a statement about which physical
effects are represented and which are deliberately left out, and the honest way to read
a result is to read it together with the tier that produced it.

## What a tier means

| Tier | What it is for | What you give up |
|---|---|---|
| `abstract` | City-scale runs, 10,000 nodes, sweeps where the object of study is upstream of the radio | Closed-form or probabilistic stand-ins. No signal-to-interference computation, no contention, no geometry in the path loss |
| `medium` | The default. The models the literature broadly agrees on | Frame-level detail: capture, preamble timing, sensing-based resource reselection, fast fading in some stacks |
| `high` | A focus region or a small scenario where the radio or the MAC *is* the object of study | Cost. It is frame-level and physically detailed, and it does not run at city scale |

A model declares in its card which tiers it implements, and the scenario validator
enforces the combinations that are coherent — `phy: high` requires `mac: high`,
`propagation: abstract` forbids `phy: high` — because a stack whose layers disagree
about how much physics exists produces numbers that look fine and mean nothing.

## The ladder, family by family

The table below is the design document's, included at build time rather than copied.
Read the row for the family you care about, and read the "ignores" wording literally:
it is the list of effects that are *not* in the answer.

{% include docs/design/02-architecture.md#7. Fidelity ladder | demote=1 %}

## Mixing tiers

Mixing a high-fidelity focus region with a cheaper surround is supported, and it is
sound *with a stated bias* rather than sound outright. The rules and the bias are in
the included section above (§7.3). The part worth repeating here is the case where it
cannot be made sound: the back-off state of nodes outside the focus region is not
modelled, so hidden-terminal effects originating outside it are under-represented
inside it. An experiment whose object of study is MAC-level contention must run a
homogeneous `high` stack.

## What "validated" means here

This project distinguishes four levels of evidence, and every model card carries one of
them. The [validation status page](validation.html) lists which models sit where.

| Status | What was done |
|---|---|
| `unvalidated` | Nothing has been checked |
| `unit-tested` | The implementation was tested against itself: invariants, edge cases, determinism. No external number was involved |
| `literature-checked` | Outputs were compared against published figures, tables or reference curves |
| `field-checked` | Outputs were compared against measurements from a real deployment |

Two cautions about reading that table.

**`unit-tested` is not weak, and it is not validation.** A crate's own tests can prove
determinism, bounds and invariants, and several of this project's crates are held to a
standard well beyond the usual — the packet-error model was re-implemented from scratch
in Python and agrees with the crate to 5e-10 dB across all 24 cells; the message
encoder was checked against two independent implementations across 235 vectors in both
directions. But a test written by the same person who wrote the model shares its
assumptions. The [defect register](defects.html) is a list of defects that lived
happily inside passing test suites.

**A cited default is not a correct default.** Every parameter in this engine must name
a source, and the [calibration debt page](calibration.html) lists the ones that cannot.
A value cited to a standard can still be the wrong value for your study: a class-default
speed limit taken from a German table put 100 km/h on Manhattan side streets, and it was
cited the whole time.

## The validation plan

{% include docs/design/04-models.md#13. Validation plan | demote=1 %}

## How to report a run

A run produces a manifest that pins the engine version, the master seed, the world
content hash, and every model card's id, version and content hash. Reporting a result
means reporting that manifest, not the prose description of the setup — the manifest is
what another person can replay. A figure produced from a model whose card says
`unvalidated`, or whose parameters appear on the calibration-debt page, should say so in
its caption.
