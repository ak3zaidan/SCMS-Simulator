This page says what kind of checking each model has had. It is generated from the
`validation` field of every registered model card, so it cannot drift from the code —
but it can still flatter the code, because the status records the *kind* of check, not
its severity. Read it with two things in mind.

**Self-consistency is not validation.** A model marked `unit-tested` has been checked
against its own author's expectations. Several of this project's crates were then
checked much harder than that — an independent re-implementation in a second language,
a second ASN.1 encoder, a second MCAP parser, a re-derivation from the standards — and
where that happened the evidence column names it. Where it did not happen, the status
is the ceiling on how much the number should be trusted.

**Passing tests and being correct are different claims.** Every defect on the
[defect register](defects.html) was found in code whose own test suite was green. The
most instructive of them, a MAC that stranded every frame queued behind another, passed
127 tests, because no test ever queued two frames before polling.
