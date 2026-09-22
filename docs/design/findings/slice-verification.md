# Vertical-slice verification (independent, 2026-09-22)

An auditor re-ran the Phase 1 slice from a clean state, wrote its own MCAP
parser rather than reusing the repository's reader, and injected faults rather
than reading code. 12 findings, one critical.

## The headline: the run is real below the message layer and a stub at it

**CRITICAL — nothing is encoded and nothing is signed.** `ObuRuntime::generate`
never calls a codec or a security backend. `signed_size()` returns a constant of
93 plus the signer identifier over a zero payload, `Transmission` carries a byte
*count* rather than bytes, and every transmit record has null payload and
envelope fields.

The tell, found before reading any code: a J2735 basic safety message and an
ETSI cooperative awareness message both came out at **exactly 101 bytes**. Two
different formats carrying different content cannot encode to the same size. One
assertion that the two differ would have caught this at the start, and its
absence is the same defect class as the four checks this project has already
found to be incapable of failing.

This matters disproportionately because the results the simulator exists to
produce — security overhead as a fraction of airtime, verification cost under
load, and what happens when a node cannot keep up with signing — are all
currently derived from that constant.

**HIGH — the normative binary wire path is never written.** `write_frame` is
called from nowhere, so a recording holds only JSON records: no keyframes, no
deltas. The browser cannot replay a run, and the byte-identity guarantee between
live and replay is unexercised.

**HIGH — ground-truth kinematics records are stamped one mobility step late**,
because the emit path uses the scheduler's current instant rather than the
instant the state describes.

## What the audit confirmed as genuinely correct

Each of these was established by injection or by independent tooling, not by
inspection:

- **Determinism.** The claimed digest reproduced exactly on a fourth run. It
  survived thread counts of 1, 2, 4, 8 and 16 with the parallel phase carrying
  26,794 receptions, and survived a four-second wall-clock freeze injected
  mid-run.
- **No wall-clock reads.** Only two exist in the whole engine path, both in the
  command-line tool behind named helpers, and neither reaches a digest. Proved
  by injection, not only by grep.
- **The ground-truth firewall.** Three separate violations were written and the
  sentinel went red on each, naming file and line.
- **The message and security crates themselves.** A cooperative awareness
  message built and encoded through the real code path decoded correctly against
  the repository's own ASN.1 with independent tooling, and a real elliptic-curve
  signature verified in Python with a negative control. The crates are right;
  they are simply not connected to the run.
- **Recording validity on the JSON path.** Monotonic times with zero inversions
  across 2,189 messages, gaps exactly at the cadence, and the transmit
  end-instant consistent with airtime for all 955 records.

## Scaling, independently measured

| Vehicles | Frames sent | Receptions | Wall per simulated second |
|---:|---:|---:|---:|
| 1 | 955 | 0 | 0.0003 |
| 9 | 5,056 | 6,019 | 0.0021 |
| 113 | 53,997 | 1,048,213 | 0.109 |
| 244 | 114,479 | 4,777,914 | 0.416 |
| 460 | 234,389 | 19,098,454 | 1.350 |
| 997 | 505,580 | 89,259,933 | 12.148 |

Every count matched the builder's table exactly, so the simulation is identical
and only timing differs. The top row is **2.56x slower than the builder
reported**, 728.9 s against 285.1 s, which the auditor caught by re-measuring
rather than accepting.

## The rest

Six scenario keys are validated, documented and hashed but never reach the
engine, so the scenario file overstates what it controls. The run report counts
what the engine emitted rather than what the recorder wrote, so it can contradict
the artefact beside it. The firewall sentinel has a demonstrated scan blind spot.
The manifest is the one artefact that cannot be compared byte for byte, because
two of its fields vary between identical runs. The "engine hash" does not pin the
engine's source, since the commit is recorded as unknown. Container sequence
numbers are all zero, so record loss is undetectable at that level. Only one
phase is actually parallel, which is narrower than the design reads. And
`v2xw-engine` currently fails its own clippy gate.
