This page publishes the defect registers produced during the build, in full, as their
reviewers wrote them. Nothing here is summarised away, and nothing is quietly dropped
once fixed: the entry stays, because the useful part of a defect is rarely the defect.

The registers exist because of how this project verifies work. After a crate is built
it is handed to an independent reviewer whose instruction is to **re-derive, not
review** — to work from the standard, the paper or the specification and reproduce the
number rather than read the code and agree with it. Several reviewers wrote their own
tooling for exactly this reason: a second ASN.1 encoder, a second MCAP parser, an
independent implementation of the deterministic-signature standard, a packet-error
model rebuilt from scratch in Python. When the crate and the independent derivation
agree, that agreement means something. When they disagree, one of them is wrong and it
is worth finding out which.

## What the defects teach

Eight patterns account for most of what was found. They are listed here because a
reader who is about to trust a number out of this simulator should know which failure
modes this codebase actually produces, and a reader who is about to *extend* it should
know which mistakes to expect of themselves.

### 1. A check that cannot fail

The single most common defect class in this project is a test that would pass whether
or not the thing under test worked. The vertical-slice audit found that nothing in the
Phase 1 run was encoded or signed — the node returned a size from a model and handed
the engine a byte count — and the entire test suite was green throughout. The tell was
not in the code: a basic safety message and a cooperative awareness message both came
out at **exactly 101 bytes**, and two different formats carrying different content
cannot encode to the same size. One assertion that they differ would have caught it on
the first day.

Elsewhere, a MAC that stranded every frame queued behind another in the same access
category passed all 127 of its tests, because no test ever queued two frames before
polling. A preamble-capture test passed only because it never finished the incumbent
frame.

**The rule this produced:** a check is not a check until it has been made to go red.
Inject the fault, watch the test fail, then fix it back. Four checks in this repository
were found to be incapable of failing; each was found by someone trying to break it, not
by someone reading it.

### 2. Two implementations of one decision, each tested against itself

The engine has a `real` cryptographic backend and a `modeled` one, and invariant I-S1
says a run's verification outcomes must be identical under both. Three separate
divergences were found: signature malleability accepted by one and rejected by the
other, invalid key material accepted by one and rejected by the other, and unsupported
post-quantum primitives. Each backend's own tests passed. The same shape appeared in
the recorder, where the live producer blanked ground-truth fields *before* quantisation
and the replay stripper blanked them *after*, so a stationary vehicle that changed lane
emitted a row in one path and not the other — which leaked exactly the fact the profile
existed to withhold.

**The rule:** when two code paths must agree, the test that matters compares them to
each other on adversarial input, not each to its own expectations.

### 3. Plausible and self-consistent is the failure mode to fear

The run report that described the message layer was internally consistent in every
respect: the recording verified, the digest reproduced across four runs and five thread
counts, the timings were sensible, the byte counts were stable. Everything agreed with
everything else because everything came from the same wrong constant. The project's own
build record now carries the correction rather than an edited history.

**The rule:** a number that agrees with the other numbers is evidence that the numbers
share a source, not evidence that the source is right. Cross-check against something
that did not come out of the same program.

### 4. A cited number can still be the wrong number

The importer's class-default speed limits were SUMO's German values, correctly cited,
and they put 100 km/h limits on 16 % of Manhattan's drivable lanes. A congestion-control
model reproduced a standard's formula with the parentheses in the wrong place, found the
result negative, invented a unit parameter to rescue it, and recorded an "unexplained
2 % residual" against the standard's own worked examples — which the correct grouping
reproduces to 0.01 %. The mis-parse had even acquired a `todo-calibrate` entry, so the
project was carrying calibration debt for a parameter that should never have existed.

**The rule:** a citation says where a number came from, not that it belongs here. When a
model disagrees with its own source's worked example, the model is wrong — reproduce the
example exactly before writing down a residual.

### 5. Unbounded work driven by unauthenticated input

A revocation check walked a hash chain whose length came from a field of a *received,
not yet verified* certificate. At the maximum the field allows, one crafted certificate
costs about 85 minutes of CPU, and the simulation would appear to hang. The check also
ran before any signature was verified.

**The rule:** anything an attacker can set is an adversarial input, in a simulator as
much as in a deployment — and a simulator is a place where "it would appear to hang" is
indistinguishable from "the scenario is slow".

### 6. A dependency's error type is a claim, not a guarantee

The recording crate's documentation promised that malformed input comes back as a named
error variant: "that is a requirement, not a courtesy". A single bit flipped inside a
compressed chunk made the underlying library attempt a seven-exabyte allocation and
abort the process — which `catch_unwind` cannot trap. The crate's own corruption tests
missed it because they only mutated bytes they had computed to be inside the payload.

**The rule:** if your error contract is stronger than your dependency's, you owe the
validation that closes the gap, and a corruption test that only corrupts the parts you
understand is testing your understanding.

### 7. Order dependence hiding inside obvious code

Whether a frame captured the receiver depended on which arrival the engine happened to
finish first, because the arrival was removed from the map before the capture decision
read the map. Ground-truth kinematics records were stamped with the scheduler's current
instant rather than the instant the state described, putting every one of them one
mobility step late.

**The rule:** in a discrete-event engine, "when did this happen" and "when did I notice"
are different times, and any decision that reads a mutable collection is a decision
about evaluation order until proven otherwise.

### 8. Silence where there should be a refusal

An encoder silently truncated brake-status bits that did not fit their five-bit field,
emitting a canonical message that said something other than what the caller asked for.
A channel-busy-ratio meter reported the load measured at its last busy instant for ever
afterwards, so a node that fell silent stayed pinned at maximum restriction. And one of
the register files below contains a stray NUL byte, which makes `grep` treat it as
binary and skip it without a word — a defect register that silently disappears from
searches is a small, perfect example of the whole category.

**The rule:** refuse loudly. A value that cannot be represented, a measurement that is
stale, a file that cannot be read — each should produce an error, not a plausible
answer.

## How to read the register below

Findings are reproduced with their severity as the reviewer assigned it, their file and
line, and the reviewer's own evidence — which is usually a measurement or a
reproduction, not an opinion. A severity label this site does not recognise is shown as
written rather than dropped; filtering findings against a fixed list of severity words
is its own way of losing information.

Fixed and outstanding findings are both here. The register is a record of what this
codebase does wrong, and that record is more useful complete than curated.
