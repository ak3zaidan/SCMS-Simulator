# Release checklist for a 1.0 tag

Status as of **2026-09-22**. Generated-adjacent: every claim below either names the
artefact that supports it or says that none exists. Where a number came from a report
rather than from a file in this tree, the row says so — a figure with no artefact behind it
is not evidence, however plausible it is.

This file is not a plan. It is the list of things that are **not true yet** and what each
one would take. Read the summary, then the rows.

## The short version

**1.0 is not close, and the distance is verification rather than construction.** Nineteen
crates compile, the workspace's own tests compile, and the feature surface is broad. What
does not exist is evidence: the CI determinism gate has never run, one of seven validation
cases has never been executed, the message layer of the end-to-end run is a size model with
no encoder behind it, and measured throughput is 121 times slower than the Phase 3 target.

Three of seven Phase 1 acceptance criteria are met with independently reproduced evidence.
That is the honest headline, and it has not moved because the last waves added capability
rather than evidence.

| Gate | State | The one-line reason |
|---|---|---|
| Determinism across three operating systems | **UNKNOWN** | the gate is built and no CI run has executed it |
| Phase 1 acceptance (7 criteria) | **3 met** | #2, #4, #5; #1 unknown, #3 not met, #6 pending, #7 needs a human |
| Phase 2 chain (report → case → revocation → enforcement) | **NOT MET** | incomplete end to end |
| Real encoding and signing in a run | **NOT MET** | `ObuRuntime::generate` calls no codec and no security backend |
| Performance at scale | **NOT MET** | 12.1 wall-clock s per simulated s at 997 vehicles against a target of 0.1 |
| Model-card completeness gate | **FAILS** | ~159 uncalibrated `high`-tier defaults with no tracked issue, and an empty calibration-issue register |
| Validation campaign | **NOT RUN** | every case in the plan is `not-run` or `disabled`; no run document exists |
| Dataset release with datasheets | **MET** | `DatasetWriter::write` refuses to finish without a clean lint, and ships a datasheet |
| Documentation site | **MET, and it fails `--strict`** | it builds, and it reports the gate and the campaign as red, which is correct |
| Defect register | **118 open-or-recorded findings** | 2 critical, 21 high, 6 major, 43 medium, 7 minor, 32 low, 6 info, 1 retracted |

---

## 1. Determinism and continuous integration

**Must be true.** Two runs of one scenario on macOS, Linux and Windows produce identical
MCAP data sections and identical digests, and CI enforces it on every commit.

**Currently.** The gate exists: CI imports a committed fixture on all three platforms and
fails unless the digests agree. **No CI run has executed it.** Determinism *within* a
platform is repeatedly demonstrated — a double import of the real 30 MB Manhattan extract
is byte-identical across all four artefacts in separate processes, and two recordings of one
run have identical data sections — which is a different and weaker claim than the one the
criterion makes.

**What it would take.** One CI run on three platforms. The work is operational, not
technical, and until it happens the cross-platform claim is a design intention. The risk it
covers is real and named in the risk register as R2: a platform `libm` or an
auto-vectorised reduction diverging in the last bits.

---

## 2. Phase 1 acceptance criteria

The criteria are `docs/design/10-roadmap.md` Phase 1. "Independent" below means a second
agent re-derived the number from the standard or the literature rather than re-running the
builder's test.

| # | Criterion | State | Evidence, or what is missing |
|---|---|---|---|
| 1 | Identical digests on each operating system | **UNKNOWN** | gate built, never run (§1) |
| 2 | Envelope size equals the size model, zero bytes tolerance | **MET** | exactly 93 B for a digest signer, 87 B + certificate for a certificate signer, confirmed octet by octet against a real COER encoding; the derivation's own threshold was wrong and was corrected (D12.1) |
| 3 | Modelled and real crypto produce identical logs | **NOT MET** | a verifier broke equivalence three ways: signature malleability, invalid key material, unsupported post-quantum primitives. Repairs in flight |
| 4 | Seek ≤ 100 ms at the 95th percentile | **MET, 57× margin** | 1.763 ms independently measured on a 9,001-frame, 600-second recording, with a type-7 quantile so the figure is not a ranking artefact |
| 5 | 60 fps with the HUD, every value resolving to a model card | **MET and exceeded** | 604 fps at 5,000 actors on a real GPU; every HUD value keyboard-reachable and opens its provenance; focus indicator at 9.10:1 contrast against a 3:1 floor |
| 6 | The manifest lists engine, plug-in, world and card hashes | **PENDING** | the world hash exists; manifest assembly was owed by `v2xw-engine` and `runs/phase1-manhattan/run-report.json` still carries `recording_file_sha256: null` and `records_written: null` |
| 7 | Manual map-to-chase fly-down | **PENDING A HUMAN** | the automated fly-down passes end to end against the mock engine; nobody has watched it |

**What #6 would take.** Fill the two null fields in the run report and assert in a test that
neither is null for a completed run. It is a small change and it is load-bearing: a manifest
with a null recording digest cannot tie a dataset to the run it came from, which is the one
job a manifest has.

---

## 3. The message layer is a stub, and it is upstream of the headline results

**Must be true.** A run that reports byte counts has encoded something.

**Currently it has not.** `crates/../ObuRuntime::generate` calls no codec and no security
backend: `signed_size()` returns a constant of 93 plus the signer identifier over a zero
payload, a `Transmission` carries a byte *count* rather than bytes, and every `node.tx`
record leaves `payload_bytes` and `envelope_bytes` null
(`docs/design/findings/slice-verification.md`, critical, 2026-09-22). The tell was that a
J2735 basic safety message and an ETSI cooperative awareness message both came out at
exactly 101 bytes.

**Why it is on the release checklist rather than on a crate's to-do list.** The three results
this simulator exists to produce — security overhead as a fraction of airtime, verification
cost under load, and what happens when a node cannot keep up with signing — are all derived
from that constant. A dataset exported from such a run now says so in its own datasheet:
the *Byte provenance* section counts a message type the engine declared no codec for as
neither real nor modelled, and prints the consequence, that a byte-count or overhead result
from the dataset is not a measurement of an encoding. That makes the stub visible to a
consumer instead of only to an auditor; it does not make the result citable.

**What it would take.** Wire the real ETSI codecs and the security backend into
`ObuRuntime::generate`, have the engine declare `message_encodings` in the run's provenance,
and assert in a test that two different message formats do not encode to the same size. The
missing assertion is the whole defect: one comparison would have caught this at the start.

---

## 4. Performance at scale

**Must be true.** 1,000 vehicles at the `high` tier at ≥ 0.1× real time, and 10,000 at
`abstract` at ≥ 1× real time (roadmap Phase 3, ADR 0011).

**Currently.** The reported measurement is **12.1 wall-clock seconds per simulated second
at 997 vehicles**, against a target of 0.1 — a **121× gap**.

**This figure has no artefact in this repository.** The only committed run,
`runs/phase1-manhattan/`, is the single-vehicle slice: 1 actor, 585 transmissions, zero
reception attempts. So the performance gap is both *failing* and *unreproducible from the
tree*, and the second half of that sentence is the more urgent one.

**What it would take, in order.**

1. A committed benchmark scenario at 1,000 vehicles and a recorded run artefact, so the
   number can be reproduced and regressions can be attributed. Without this, any tuning
   claim is unfalsifiable.
2. A profile. A 121× gap is not a constant-factor problem; it is a shape problem, and the
   one honest thing to say about it is that nobody has yet published where the time goes.
3. Only then, tuning. This is the one item on this checklist that should **not** be promised
   to fall out of optimisation: the analytical budget in 02-architecture §11.2 may simply be
   wrong, in which case the right outcome is a revised, documented target rather than a
   missed one.

---

## 5. The model-card completeness gate

**Must be true.** No parameter carries a `todo-calibrate` marker on a `high`-tier default
without a matching calibration issue (roadmap Phase 6).

**Currently it fails.** The gate is implemented in `crates/v2xw-metrics/src/gate.rs` and run
by `docs/site/tools/cardgen --gate` (`just --justfile docs/site/justfile gate`). The
calibration-issue register, `docs/calibration/issues.json`, holds **zero issues**, so every
uncalibrated `high`-tier default fails it.

Over the registry that exporter builds, the counts are:

| | Count | How it was arrived at |
|---|---:|---|
| Uncalibrated `high`-tier defaults — **the gate's denominator** | ~**159** | 141 hardware-profile fields, 11 safety-application thresholds, 2 verification-policy thresholds, 2 metric thresholds, 2 GNSS noise terms, 1 demand mapping |
| …covered by a tracked issue | **0** | the register is empty |
| Uncalibrated defaults on cards that do **not** declare the `high` tier | ~**16** | 11 in the ETSI TS 102 941 protocol card (`abstract` only), 1 in the SCMS card, 4 in the mobility car-following, lane-change, pedestrian and legacy-GNSS cards |
| Uncalibrated defaults, all tiers | ~**175** | the two rows above |

The **141** is exact and is the number worth arguing about: it was counted field by field
from the eleven shipped YAML profiles under `crates/v2xw-node/profiles/hardware/`, as the
fields carrying `value: null` with a status other than `not-applicable`. Excluding the
VRU-device profile, which has no device data at all, it is the 128 across ten profiles that
an earlier report quoted. The other rows are static counts of the `todo-calibrate`
construction sites in each card builder, and the gate prints the exact figure when it runs;
they are marked `~` for that reason.

The repository-wide figure is larger than any of these. The exporter reaches five crates;
the radio, message, network, threat, world and record crates publish their cards through
per-model constructors that nothing registers, and a static count of their
`todo-calibrate` construction sites adds at least another **90** — 37 in the world importers
alone. The dump states that coverage gap and the gate page repeats it, because a partial
registry must not read as a clean one.

**What it would take.** Not a code change — the gate works and its own tests inject a
registry-wide wildcard and assert that it stays red. It takes somebody accepting the
measurements: an issue per parameter family with an owner, a state and the measurement that
would close it. The eleven hardware profiles are the bulk, and they are mostly one
measurement each: bench the device through its vendor SDK and read the service times off it.
Until an owner exists, filling the register would turn the gate green without a measurement,
which is the failure the gate exists to prevent.

**A legitimate alternative to closing it.** Declare, in writing and signed by the project
owner, that 1.0 ships with the gate red and with the count stated in the release notes. That
is a defensible decision for a research tool. What is not defensible is shipping with the
gate absent, which is where this repository was a week ago.

---

## 6. The validation campaign

**Must be true.** For every model: what it was checked against, with what result, and where
the evidence is — generated, not written.

**Currently.** The report is generated (`docs/site/v2xwdoc/campaign.py`, the *Validation
campaign* page) and it joins three inputs: the model registry, the validation suite's run
document, and the seven defect registers. Two of the three exist.

* **The registry**: present. Validation status is a field on every card, so it cannot drift
  from the code — but it records the *kind* of check, not its severity.
* **The run document**: **does not exist.** `docs/site/validation-runs.template.json` holds
  the cases `04-models.md` §13 defines, and every one of them is `not-run` or `disabled`.
  Nothing has been measured. The template says so in its own header, at length, because
  filling in an `observed` value nobody measured would be fabricating a validation result.
* **The registers**: present, and they are the strongest evidence this project has.
  118 findings across 7 registers, and 38 sentences in which an independent reviewer
  recorded a verdict — PASS, FAIL, CONFIRMED or UNVERIFIED — on a claim they re-derived
  themselves. The campaign page now harvests those sentences and reproduces them as written.

**What it would take.** Run the suite and write
`docs/site/generated/validation-runs.json`. The cases whose *targets* are themselves
UNVERIFIED (PDR versus distance, CBR versus density) stay `disabled` until somebody obtains
the source figures; the ones with real targets — the Sjöberg PER curve, the flow–density
relation — can be measured today. Note that a failing case then **blocks** its models from
being labelled checked, and the site's `--strict` build fails on the contradiction, which is
the point.

---

## 7. Dataset release

**Must be true.** A published dataset carries its own provenance: the model cards that
produced it, the seed, the scenario, the world hash, the leakage-lint result, and the
validation status of every model involved.

**Currently: met.** `DatasetWriter::write` writes the tables, lints the bytes on disk,
**refuses to finish** if the leakage linter finds anything, then writes the manifest (whose
digests cover the tables) and the datasheet (which carries the lint verdict and the digests).
The datasheet now states the validation status of every model behind the dataset, how many of
them were compared against anything outside the engine, how many uncalibrated defaults they
carry between them, and which of the dataset's byte counts came from real bytes rather than
from a size model.

**The two remaining caveats, and they are about inputs rather than about the exporter.**

1. The engine does not yet fill `RunProvenance::models` or
   `RunProvenance::message_encodings`. Until it does, a real run's datasheet says "the engine
   supplied names and versions only" and treats every byte count as being of unknown
   provenance. Both are honest and neither is publishable-grade.
2. A world bundle derived from OSM data may not be published without its licence file, and
   the exporter refuses to write one without it. Whether coordinate-only simulation data
   driven on an OSM network is itself a derivative database is an open legal question
   (11-open-questions A5) and is not a question this repository can close.

---

## 8. Documentation

**Must be true.** The site is generated from the registry, not written, and it publishes the
project's own gaps.

**Currently: met, and the `--strict` build fails.** That is the correct behaviour and not a
defect: `just --justfile docs/site/justfile check` treats a red completeness gate and a
card-versus-campaign contradiction as build warnings, and a rule nothing enforces is a
preference. The honesty pages — calibration debt, the completeness gate, validation status,
the validation campaign, the defect register — are all generated from the card dump and the
registers.

**What it would take to make the strict build green.** Close §5 and §6. Nothing in the
documentation tooling.

---

## 9. Defects

118 findings are recorded across seven registers, every one of them found in code whose own
test suite was green.

| Register | Findings | Worst |
|---|---:|---|
| `ui-review-register.md` | 34 | 1 critical, 7 high |
| `physical-layer-register.md` | 28 | 6 high |
| `world-review-register.md` | 27 | 5 high |
| `record-metrics-register.md` | 12 | 3 high |
| `msg-sec-register.md` | 11 | 1 critical, 4 major |
| `world-import-defects.md` | 6 | 2 major |
| `slice-verification.md` | — | 1 critical, in prose rather than as numbered findings |

**Must be true for 1.0.** Every critical and high finding either fixed with a test that
would have caught it, or recorded as a known limitation in the release notes with its
consequence stated.

**One cheap item that should not wait.** `docs/design/findings/ui-review-register.md`
contains a stray NUL byte, which makes `grep` treat it as binary and skip it **without
saying so**. The documentation build reads the file as bytes and strips the NUL, so the site
is complete; every grep-driven search over the registers silently misses that register's 34
findings until the byte is removed.

**The pattern worth naming, because it recurs.** Several of the recorded defects are places
where a check exists, passes, and does not check what its documentation says: the V5
conformance test that passed because every fixture actor moved every step; D9 enforced on the
frame path and not on the record path; two message formats encoding to the same size with no
assertion that they differ. The fix in each case is the same and it is not more tests — it is
**adversarial** tests: inject the fault and watch the check go red before trusting it.

---

## 10. What "1.0" should mean

The scope above is a 1.0 of *the simulator*, and on the evidence it is one to two more waves
of verification away — with the performance item the only one that might not close at all.

There is a narrower and defensible tag available now, and it is worth naming so the decision
is made deliberately rather than by drift:

**A 1.0 of the recording, replay and dataset toolchain.** Byte identity between live and
replay is independently verified and sound by construction. Pose quantisation is measured at
half a millimetre worst case over 100,000 steps in one GOP. Seek is 57× inside its target.
The leakage linter refuses, provably, by seven injected routes. The dataset exporter will not
finish without a clean lint and ships a datasheet that states its own provenance. That is a
publishable, citable artefact, and it does not depend on the message layer, the performance
gap or the calibration register.

What such a tag must **not** claim is a validated V2X simulator. The distinction is the whole
subject of this file.

---

## How to re-derive every row above

```sh
# The completeness gate, with its full work list (writes the dump, then fails).
just --justfile docs/site/justfile gate

# The honesty pages, failing on the gate and on any card-versus-campaign contradiction.
just --justfile docs/site/justfile check

# The defect registers, as the site reads them.
ls docs/design/findings/
```

Nothing in this file is hand-maintained where a generator could produce it: the calibration
count comes from `v2xw_metrics::gate`, the validation matrix from the card dump, and the
defect history from the registers. The rows that *are* hand-written are the ones that state a
judgement — what a gap would take, and whether it will close — and they carry a date for that
reason.
