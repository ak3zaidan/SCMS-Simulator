This is the validation campaign report the roadmap's Phase 6 asks for: for every model
the engine registers, what has been checked, against what, and with what result.

It is **generated from three inputs, and it reports where they disagree**:

| Input | What it is | What it tells you |
|---|---|---|
| the model registry, via the card dump | every model's declared `validation` status, references and tests | what the code *claims* |
| the validation-run output | one record per `ValidationCase` of 04-models §13, with its target, tolerance and observed value | what was *measured* |
| the defect registers | what independent review found | what was *wrong anyway* |

A hand-written validation report is true on the day it is written. This one is true on
the day it is built, and the contradiction table is where that difference shows up: a
card that claims `literature-checked` while no case measured it, or while a case
measured it and failed, appears at the top of the page with the disagreement named.

## Three things to read this page against

**A status is a kind of check, not its severity.** `unit-tested` means the
implementation was checked against its own author's expectations. Several of this
project's crates were then checked far harder than that — an independent
re-implementation in a second language, a second ASN.1 encoder, a second MCAP parser, a
re-derivation from the standards — and where that happened the evidence column names it.

**Passing tests and being correct are different claims.** Every defect on the
[register](defects.html) was found in code whose own suite was green. The most
instructive of them, a MAC that stranded every frame queued behind another, passed 127
tests, because no test ever queued two frames before polling. That is why the defects
column is on this page at all: it is the one column a status field cannot compute.

**An empty contradiction table is not a clean bill of health when there is no run
document.** If the validation suite has not been run, there is nothing to compare the
cards against, and the page says so where the table would be. Read the banner before the
table.

## What a failing case blocks

04-models §13's rule: a model's `validation.status` may not move to
`literature-checked` while any case naming it fails, and a failure "lists the bins that
missed, the model ids and parameter-set ids involved, and blocks the tier from being
labeled `literature-checked`". This page is where that rule becomes visible, and
`--strict` is where it becomes enforceable.

## Cases with unverified targets are carried, not deleted

Several rows of §13's validation table have targets nobody could retrieve: the Bai and
Krishnan PDR curve, the CBR-versus-density curve, Bazzi's per-distance figures. Those
cases ship **disabled** rather than absent, so the report shows what is missing instead
of showing a shorter list. A `disabled` outcome is a statement about the literature's
availability, not about the model.

**Two sections are worth reading before the tables.** *Models about which no evidence of any
kind exists* names every model with no reference, no test, no validation case and no mention
in any defect register — the models about which this project can say nothing at all. *What
the independent reviewers concluded, in their own words* harvests the verdict sentences out
of the seven defect registers, where the builders' headline claims were re-derived by
somebody who wrote their own tooling to do it. Those sentences are the strongest evidence
this project has about itself, and until they were harvested they existed only inside
1,400 lines of review.
