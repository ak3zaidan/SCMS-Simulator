# What breaks when the run gets long

Everything in this repository was calibrated on 60–300 s windows. Every timescale the domain
actually has is longer than that: a pseudonym rotation period, a CRL's growth, a certificate's
validity, an attacker's duty cycle, a city's demand profile, and the misbehaviour authority's own
accumulation of false positives. `TRAFFIC-PANEL-SURVIVORSHIP.md` is the record of what that costs —
a defect worth about 7% at 300 s that deleted 56.2% of the vehicle-steps over an InTAS peak hour and
**was invisible at every duration anything had been measured at**. This document assumes there are
more like it and goes looking.

The instrument is `tools/long_run_probe.py`. It runs one scenario at a ladder of durations and
reports every quantity as a function of duration: wall clock split into the three phases a run
actually has, peak working set, output bytes, digest reproduction, the full realism scorecard, and
the SCMS curves in simulated time. The scenario is this repository's own reference arm, verbatim —
`--flow --road grid --grid 6 --arrival-rate 2 --attacker-pct 0.15 --traffic-lights --seed 42`, with
`--emit-mobility-oracle` on so the traffic panel reads an un-enforced record — so every rung is
directly comparable with the false-positive sweep in `TRAFFIC-PANEL-SURVIVORSHIP.md` §6 and with the
pinned golden. The 300 s rung reproduces that document exactly: 593 vehicles, 152 revoked, precision
0.5987, survival 0.812644.

**Headline.** Nothing in the engine is wrong at length in the way survivorship was wrong: the digest
reproduces bit-for-bit at every rung and the step loop is very nearly linear. What breaks is *around*
the loop.

* **Finalisation is quadratic and it wins.** An `assert` after the last step re-checks every
  revoked vehicle's certificates against the whole CRL: O(R·C) dictionary work plus O(R²/2)
  hash-chain `CrlLinkageEntry.matches` calls at a measured 8.7–10.3 µs each, with R and C both
  linear in duration. Finalisation is **1.4 s at 300 s and 1,715.2 s at 28800 s** — exponent 1.598,
  and at 8 h the engine **spends longer finishing the run than running it** (finalisation / step
  loop = 1.083, 52% of wall clock).
* **Memory is linear in total vehicles and unbounded.** The whole population is drawn and
  constructed before step 0 and never released. Peak working set is **128 MiB at 300 s and 3,159 MiB
  at 28800 s**, fitting `92.5 MiB + 54.3 KiB × (vehicles ever created)` with residuals under 8 MiB
  across all seven rungs. The controlled test: at a fixed 1800 s, capping the population at 600
  vehicles instead of 3,590 cuts peak working set 284.4 → 131.7 MiB.
* **Graded metrics are computed over a fixed 240-instant sample** (`MAX_TIME_BUCKETS` /
  `HEADWAY_MAX_INSTANTS`), so their coverage falls as 1/duration. `overlap_events` — a **HARD** gate
  — reads **434 at 300 s and 319 at 28800 s** while the trajectory sample behind it grows **109×**
  (42,403 → 4,622,931 segments). Its sensitivity to a rare overlap does not improve with run length,
  and its magnitude cannot be compared across durations at all.
* **Certificate expiry does not exist.** `ma_cert_status` is written with
  `valid_from = 0.0, valid_to = total_time` for **100.0% of certificates at every one of the seven
  rungs**, and the *issued* window is capped forward to the end of the simulation, so its median is
  0.477–0.481 × the run length and its maximum is exactly `duration + dt` at every rung. A
  certificate in this engine expires when the simulation does, and never before.
* **An attacker cannot outlive one trip.** `attack_to = spawn_time + life`; the attack-span
  distribution is stationary from 1800 s on at p50 256 s / p95 484 s / **max 598 s**, unchanged at
  8 h. No run length produces a persistent adversary, and a duty cycle longer than a trip is
  unrepresentable.
* **`--demand rush` / `--demand night` are functions of run *fraction*, not of time**, so under them
  a longer run is a different scenario rather than a longer one.

**Measurement conditions, stated because they matter.** Rungs at 300–1800 s ran on an otherwise idle
16-logical-core / 61 GiB host. From 3600 s onward a parallel workstream was running its own InTAS
ladder and a pytest suite on the same machine — up to eight concurrent CPU-bound Python processes —
so absolute wall clock at the upper rungs is inflated. The step loop's rate reads 38.7 ms/sim-s at
300 s against 57.9 at 28800 s — a **1.5× rate change over a 96× duration change**, and part of even
that is load. Every conclusion below is therefore stated on a contention-robust quantity: the
**ratio** of finalisation to step loop *within one run*, the **scaling exponent** (a constant factor
cannot move it), the **isolated microbenchmark** of the term being blamed, **peak working set**
(unaffected by CPU contention), **output bytes**, and **digests**.

---

# Part I — the synthetic grid, stretched

*Sections 0-8. One scenario, one arrival rate, seven durations from 300 s to 28,800 s: the controlled experiment, because §0 shows the traffic is literally identical at every rung. Part II below is the complementary half — the real city's real day, where the traffic is deliberately different at every hour.*

## 0. The ladder is a controlled experiment — but only under `--demand uniform`

Before any number below means anything, the ladder has to be a ladder: the same traffic seen for
longer, not a different scenario each time. **It is, and this is measured rather than assumed.**

| | |
|---|---|
| `gt_vehicle.jsonl` rows of the 300 s run present and **byte-identical** in the 900 s run | **593 / 593** |
| 900 s run's rows present and byte-identical in the 1800 s run | **1,799 / 1,799** |
| 1800 s run's rows present and byte-identical in the 3600 s run | **3,590 / 3,590** |
| revocations of the 300 s run present in the 900 s run at the **same instant** | **152 / 152** |
| revocations of the 1800 s run present in the 3600 s run at the same instant | **960 / 960** |

A shorter run is an exact prefix of a longer one: same vehicles, same ids, same spawn times, same
attacker assignment, same revocation instants. Every difference between two rungs is the *window*,
never the traffic. **§4's precision table is the independent confirmation and it is the stronger
one**: the cumulative MA precision read out of seven separate runs agrees to four decimals at every
shared instant (0.5987 at t = 300 s in all seven; 0.5594 at t = 1800 s in all five that reach it).
The only disagreements are a single revocation landing on the closing step of the shorter run
(483 against 484 at t = 900 s, 3,730 against 3,731 at t = 7200 s), which is a boundary, not drift.

*(The 1800 → 3600 nesting is also an accidental control on something else. A parallel workstream
edited `run.py` at 20:00 local, between those two rungs, adding `_integrity.stream_closed()` brackets
around the four plugin `build_*` calls. The nesting is exact across that edit, so the change is
behaviour-neutral on this scenario and the ladder remains one experiment.)*

**Prefix nesting is destroyed by `--demand rush` or `--demand night`, and that is a finding in its own
right.** The arrival thinning evaluates `demand_mult(frac)` at `frac = tt / total_time`:

```python
def demand_mult(frac):                 # run.py:5548, flow mode
    if cfg.demand_profile == "rush":   # morning + evening peaks
        peak = math.exp(-((frac - 0.25) / 0.09) ** 2) + math.exp(-((frac - 0.75) / 0.09) ** 2)
        return 0.2 + 0.8 * min(1.0, peak)
    if cfg.demand_profile == "night":
        return 0.15 + 0.25 * frac
    return 1.0
```

so the profile is a **shape stretched to the run**, not a clock. Measured, same seed:

| `--demand rush` | 300 s | 900 s |
|---|---|---|
| vehicles | 278 | 817 |
| rows of the 300 s run **byte-identical** in the 900 s run | — | **11 / 278** |
| spawn histogram, 12 equal buckets | `9 13 49 40 16 8 12 13 41 46 20 11` | `17 55 151 123 43 32 17 51 112 124 66 26` |
| position of the "morning" peak | t ≈ 62–87 s | t ≈ 187–262 s |
| detection precision | 0.780 | 0.477 |

Identical *fractional* shape, completely different clock. The 300 s run's "AM peak" is at 75 s; the
900 s run's at 225 s. Only 11 of 278 vehicles survive the duration change unchanged, so precision
moving 0.780 → 0.477 is **not** a duration effect: the two runs are different scenarios.

**Consequence.** `--demand rush` and `--demand night` cannot express a time-of-day profile at any
duration, and no quantity may be compared across durations under them. The uniform arm is the only
arm where "as a function of duration" is a meaningful phrase. InTAS replay is unaffected — under
`--mobility-source sumo_replay` the engine's own arrival process is not run at all and SUMO's
departure times carry the real clock, which is why the peak-hour numbers in earlier documents are not
in question here.

---

## 1. Cost

| duration | wall s | ms / sim-s | setup s | step loop s | loop ms/sim-s | **finalisation s** | final / loop | peak MiB | output bytes | vehicles | revoked |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 300 s | 12.5 | 41.7 | 0.12 | 11.0 | 38.7 | **1.4** | 0.123 | 128 | 19,808,360 | 593 | 152 |
| 900 s | 40.2 | 44.7 | 0.21 | 35.6 | 41.6 | **4.4** | 0.124 | 193 | 57,149,409 | 1,799 | 483 |
| 1800 s | 85.1 | 47.3 | 0.34 | 73.8 | 43.1 | **11.0** | 0.149 | 284 | 111,302,078 | 3,590 | 960 |
| 3600 s | 196.4 | 54.5 | 0.84 | 153.1 | 44.8 | **42.5** | 0.277 | 466 | 218,778,121 | 7,179 | 1,843 |
| 7200 s | 504.2 | 70.0 | 1.26 | 365.1 | 53.4 | **137.9** | 0.378 | 849 | 441,496,261 | 14,375 | 3,730 |
| 14400 s | 1280.3 | 88.9 | 2.71 | 826.0 | 60.4 | **451.6** | 0.547 | 1633 | 900,157,771 | 28,970 | 7,782 |
| 28800 s | 3303.5 | 114.7 | 5.15 | 1583.2 | 57.9 | **1715.2** | 1.083 | 3159 | 1,794,069,990 | 57,758 | 15,423 |

**Scaling exponents** — the log-log slope against duration over the whole 96× range; 1.0 is linear
and a constant host-load factor cannot move it:

| | wall clock | step loop | **finalisation** | setup | peak working set | output bytes | vehicles | revoked |
|---|---|---|---|---|---|---|---|---|
| exponent | 1.228 | **1.104** | **1.598** | 0.844 | 0.718 * | 0.989 | 1.003 | 1.008 |

\* the memory exponent is misleading because peak is `base + k x vehicles` and the base dominates the
low rungs. The line fits far better: **92.5 MiB + 54.3 KiB per vehicle ever created**, residuals
+3.7, +5.2, +1.2, -7.8, -6.0, +2.9, +0.7 MiB across the seven rungs (§1.3).

### 1.1 The step loop is linear; finalisation is not

A run has three phases and they do not scale alike.

* **setup** (0.12 s to 5.15 s) — the whole arrival process is drawn and every vehicle constructed
  before step 0, so this is linear in the population. It is where the memory of §1.3 is allocated.
* **step loop** — exponent **1.104**, and its rate moves 38.7 → 57.9 ms/sim-s over a 96× range. Some
  of that is host load (see *Measurement conditions*) and the rest is discussed in §1.3; either way
  it is a **1.5× rate change over a 96× duration change**, i.e. very nearly linear.
* **finalisation** — exponent **1.598**, and it does not level off. It is 1.4 s at 300 s and
  **1,715.2 s at 28800 s**: at 8 h the engine spends **longer finishing the run than running it**.

The contention-proof form of the finding is the **ratio within one run**, which no host-load factor
can move:

| duration | 300 s | 900 s | 1800 s | 3600 s | 7200 s | 14400 s | 28800 s |
|---|---|---|---|---|---|---|---|
| finalisation / step loop | 0.123 | 0.124 | 0.149 | 0.277 | 0.378 | 0.547 | **1.083** |
| finalisation share of wall clock | 11% | 11% | 13% | 22% | 27% | 35% | **52%** |

And rung to rung the finalisation multiplier tracks the **square of the revoked count**, not the
duration:

| step | duration | finalisation | revoked | revoked² |
|---|---|---|---|---|
| 300 → 900 s | ×3.00 | ×3.26 | ×3.18 | ×10.10 |
| 900 → 1800 s | ×2.00 | ×2.48 | ×1.99 | ×3.95 |
| 1800 → 3600 s | ×2.00 | ×3.86 | ×1.92 | ×3.69 |
| 3600 → 7200 s | ×2.00 | ×3.25 | ×2.02 | ×4.10 |
| 7200 → 14400 s | ×2.00 | ×3.27 | ×2.09 | ×4.35 |
| 14400 → 28800 s | ×2.00 | ×3.80 | ×1.98 | ×3.93 |

From 1800 s on, doubling the duration multiplies finalisation by 3.25–3.86 against a revoked² of
3.69–4.35. The next section identifies the term and measures it in isolation.

### 1.2 What the quadratic term is, exactly

The `assert` block that runs after the last step and before any output file is written:

```python
# run.py:7616  ---- Real-linkage sanity: the CRL entry must revoke EVERY observed cert ----
for vid in revoked_vehicles:                       # O(R)
    for d in cert_first_seen:                      # O(C) -- a FULL scan, once per revoked vehicle
        info = pseudonym_info[d]
        if info["veh_vid"] != vid:
            continue
        assert any(e.matches(info["i"], info["j"], info["lv"]) for e in crl_entries)   # O(R)
```

`crl_entries` is appended in revocation order and never pruned, so the vehicle revoked *p*-th scans
*p* entries before its own matches: **R²/2 `matches` calls**, each of which is two AES-ECB
Davies-Meyer compressions plus a linkage XOR. Isolated on synthetic devices of the same shape
(`tools/long_run_probe.py --crl-assert-cost`):

| revoked R | certs C | `matches` calls | `matches` s | O(R·C) scan s | **total s** | µs per call | rung's measured finalisation |
|---|---|---|---|---|---|---|---|
| 152 | 623 | 11,628 | 0.103 | 0.002 | **0.105** | 8.851 | 1.4 s |
| 483 | 1,883 | 116,886 | 1.024 | 0.019 | **1.043** | 8.761 | 4.4 s |
| 960 | 3,752 | 461,280 | 4.024 | 0.079 | **4.103** | 8.724 | 11.0 s |
| 1,843 | 7,479 | 1,699,246 | 17.563 | 0.338 | **17.901** | 10.336 | 42.5 s |
| 3,730 | 14,990 | 6,958,315 | 69.501 | 1.371 | **70.872** | 9.988 | 137.9 s |

The (R, C) pairs are the ones the 300 / 900 / 1800 / 3600 / 7200 s rungs actually reached, so the
last two columns are the same rung measured two ways. Call count is exactly R(R+1)/2 and the
per-call cost is flat at 8.7–10.3 µs, so the term is **≈ 4.5 µs × R²**; with R ≈ 0.53 revocations per
simulated second on this arm (0.5067–0.5628 at every rung), that is **≈ 1.2 µs × D²**. Extrapolated
to the 14400 s and 28800 s rungs (R = 7,782 and 15,423) it predicts **306 s** and **1,203 s** against
measured finalisations of 451.6 s and 1,715.2 s — so the CRL self-check alone is **68% and 70%** of
finalisation at the top two rungs, and it is the term that makes the exponent 1.6 instead of 1.

The rest of finalisation is the O(R·C) dictionary scan around the same assert, the `ma_cert_status`
build, four sorts, seven file writes, and the SHA-256 pass below; at the clean rungs that remainder
is 0.68 s (300 s) → 3.02 s (1800 s), roughly linear.

R is the revoked count, which on this arm is 0.50–0.53 revocations per simulated second at every
duration, so R ≈ 0.51 × D and the term is **≈ 1.1 µs × D²**. Both loops are also O(R·C) in plain
dictionary work, and C (certificates ever seen) is ~1.05 per vehicle with rotation off and ~5 with
`--rotate-period 60`, which multiplies the whole term.

**A second finalisation defect, which turns out to cost memory rather than time.** `_file_sha256`
(run.py:8053) is `h.update(fh.read())` — the **entire file into one `bytes` object** — and it is
called once per file by `_data_digest` (run.py:8124) and again per file by `_write_manifest`, so
every data file is read and hashed **twice**. Timed on the actual datasets
(`--hash-cost`):

| dataset | files | bytes | largest file | engine shape (2 whole-file passes) | one streamed pass | ratio |
|---|---|---|---|---|---|---|
| 3600 s rung | 11 | 218,759,774 | 121,004,180 | 0.285 s | 0.149 s | 1.91× |
| 7200 s rung | 11 | 441,477,907 | 244,937,114 | 0.569 s | 0.294 s | 1.94× |

At 1.5 GB/s out of the page cache the wasted *time* is under a second even at 7200 s, so this is not
what makes finalisation grow. What it does cost is a **single `bytes` allocation the size of the
largest output file, twice** — 245 MiB at 7200 s here, and 1.72 GiB on the InTAS peak hour whose
`gt_emissions_sample.jsonl` was that size. Chunked reads with one hash reused for both call sites is
digest-neutral and removes the allocation entirely.

### 1.3 Memory: what accumulates

Peak working set across all seven rungs fits **`92.5 MiB + 54.3 KiB × (vehicles ever created)`**
with residuals of +3.7, +5.2, +1.2, −7.8, −6.0, +2.9, +0.7 MiB — i.e. it is a function of the *total
population*, not of the concurrent one (which saturates at ~170) and not of the step count. At
28800 s that is **3,159 MiB**, and nothing in the design stops it: a 24 h run of this scenario would
cost ~9.1 GiB, and the same run at InTAS peak density would cost far more.

The controlled test, same 1800 s duration and the same 1,800 steps, differing only in
`--max-total-vehicles`:

| 1800 s | vehicles | peak working set | finalisation |
|---|---|---|---|
| uncapped | 3,590 | **284.36 MiB** | 14.76 s |
| `--max-total-vehicles 600` | 600 | **131.73 MiB** | 0.98 s |

Capping the population at 17% of its natural size cuts peak memory by **53.7%** and reproduces the
300 s rung's footprint (128 MiB at 593 vehicles). Finalisation falls with it, 14.76 s → 0.98 s,
because R falls too (§1.2). The cause is structural: `run_pipeline` draws
the **entire arrival process and constructs every vehicle before step 0** (`while _replay is None:`
at run.py:5568, bounded only by `tt >= total_time`), each with its trip geometry, its pseudonym set,
its `DeviceLinkageContext`, an LA registration, an RA binding and a PCA issuance; and then holds
them for the run in `vehicles`, `vrng`, `digest_to_vehicle`, `pseudonym_info`, `gt_idmap`,
`gt_vehicle`, `cert_first_seen`, `cert_last_seen`, `filed_by` and `received_by`. The pruning that
*does* exist (`prune_state`) targets the per-pair state — `last_claimed`, `subj_events`, per-link
channel state — which would otherwise be quadratic. **Every per-identity structure is retained for
the whole run.**

**The obvious explanation for the loop's residual rate drift is refuted.** Python's generational
collector runs a gen-2 pass on a schedule that does not care how big the live set is, and this
engine's live set grows linearly with duration (§1.3) — which would make total GC time quadratic.
Tested directly, same scenario, `gc.disable()` before the import and nothing else changed:

| 1800 s | wall s | step loop s | finalisation s | peak working set | `data_digest` |
|---|---|---|---|---|---|
| collector **on** | 109.68 | 94.87 | 14.42 | 285.0 MiB | `0fd03e8793e9d377…` |
| collector **off** | 110.61 | 96.03 | 14.19 | 285.2 MiB | `0fd03e8793e9d377…` |

The loop is 1.2% *slower* without the collector (the difference is noise), finalisation is unchanged,
the extra memory is 0.22 MiB, and the digest is identical — which is also a small piece of
determinism evidence: nothing the engine writes depends on when objects are collected. So the loop's
rate drift is **not** garbage collection. What remains is consistent with cache pressure from the
per-identity dictionaries that grow without bound (§1.3) plus host load, and at 1.5× over a 96×
duration range it is not the problem finalisation is.

### 1.4 Output bytes

| file | 300 s | 900 s | 1800 s | 3600 s | 7200 s | 14400 s | 28800 s | B / sim-s (top rung) |
|---|---|---|---|---|---|---|---|---|
| `ground_truth/gt_attacks.jsonl` | 17,970 | 54,064 | 102,617 | 194,820 | 397,244 | 824,105 | 1,642,653 | 57 |
| `ground_truth/gt_emissions_sample.jsonl` | 304,653 | 1,031,659 | 2,112,078 | 4,235,604 | 8,460,771 | 17,140,176 | 34,261,316 | 1,190 |
| `ground_truth/gt_identity_map.jsonl` | 92,337 | 278,930 | 560,361 | 1,119,274 | 2,249,823 | 4,588,651 | 9,180,388 | 319 |
| `ground_truth/gt_linkage_revocation.jsonl` | 17,349 | 55,435 | 110,938 | 213,770 | 434,630 | 912,992 | 1,818,148 | 63 |
| `ground_truth/gt_mobility_oracle.jsonl` | 9,007,810 | 29,580,382 | 60,169,229 | 121,004,180 | 244,937,114 | 500,216,831 | 997,460,722 | 34,634 |
| `ground_truth/gt_report_labels.jsonl` | 1,084,844 | 2,733,846 | 5,042,913 | 9,613,805 | 19,444,798 | 39,776,691 | 79,346,351 | 2,755 |
| `ground_truth/gt_vehicle.jsonl` | 106,249 | 323,558 | 648,379 | 1,299,245 | 2,609,716 | 5,282,464 | 10,564,414 | 367 |
| `ma/ma_cert_status.jsonl` | 119,032 | 362,106 | 729,777 | 1,459,410 | 2,930,262 | 5,966,717 | 11,945,963 | 415 |
| `ma/ma_crl_events.jsonl` | 15,356 | 49,118 | 98,198 | 189,991 | 386,239 | 810,044 | 1,623,197 | 56 |
| `ma/ma_investigations.jsonl` | 45,668 | 145,322 | 290,202 | 558,703 | 1,132,436 | 2,371,675 | 4,723,519 | 164 |
| `ma/ma_reports.jsonl` | 8,978,757 | 22,516,647 | 41,419,043 | 78,870,972 | 158,494,874 | 322,249,070 | 641,484,956 | 22,274 |
| `manifest.json` | 18,335 | 18,342 | 18,343 | 18,347 | 18,354 | 18,355 | 18,363 | 1 |
| **total** | **19,808,360** | **57,149,409** | **111,302,078** | **218,778,121** | **441,496,261** | **900,157,771** | **1,794,069,990** | **62,294** |

Output is **linear** — exponent 0.989, and the marginal rate is 66.0 kB/sim-s over the first 300 s
settling to 59.7–63.7 kB/sim-s at every interval after. `gt_mobility_oracle.jsonl` is 56% of it and
`ma/ma_reports.jsonl` another 36%. The one non-linearity is in the *first* 300 s: the marginal report
rate is **26.08 s⁻¹ over 0–300 s** and **18.00–19.59 s⁻¹ at every interval thereafter**, so a
report-volume figure read at 300 s over-states the steady state by up to **45%**.

At the top rung the run writes **1.79 GB**. A 24 h run of this scenario would write ~5.4 GB, and
`realism_bench` reads the 997 MB oracle record back in 56.8 s.

**Marginal rates between rungs.** Prefix nesting (§0) makes these exact rather than differences of two independent samples:
| interval | marginal reports / sim-s | marginal revocations / sim-s | marginal vehicles / sim-s | marginal bytes / sim-s |
|---|---|---|---|---|
| 0-300 s | 26.08 | 0.5067 | 1.9767 | 66,028 |
| 300-900 s | 19.61 | 0.5517 | 2.0100 | 62,235 |
| 900-1800 s | 18.17 | 0.5300 | 1.9900 | 60,170 |
| 1800-3600 s | 18.00 | 0.4906 | 1.9939 | 59,709 |
| 3600-7200 s | 19.12 | 0.5242 | 1.9989 | 61,866 |
| 7200-14400 s | 19.59 | 0.5628 | 2.0271 | 63,703 |
| 14400-28800 s | 19.06 | 0.5306 | 1.9992 | 62,077 |

**`realism_bench` is linear in the artifact.**

| duration | 300 s | 900 s | 1800 s | 3600 s | 7200 s | 14400 s | 28800 s |
|---|---|---|---|---|---|---|---|
| scorecard wall clock | 0.9 s | 1.7 s | 3.1 s | 7.8 s | 14.6 s | 25.6 s | **56.8 s** |

Traffic source is the un-enforced `oracle` record at every rung, and the top rung reads 997 MB of it.

### 1.5 The withheld-oracle ceiling, in seconds of simulated time

`_WithheldStream` holds the ORACLE streams in memory while an isolated third-party detector is
alive, and past `WITHHELD_MEMORY_BYTES = 384 MiB` it spills XOR-sealed to disk — at which point the
guarantee weakens from "nothing on disk" to "nothing readable on disk". The withheld set is
`gt_report_labels` + `gt_emissions_sample` + `gt_mobility_oracle`, and its rate converges:

| duration | withheld B/sim-s | 384 MiB reached at |
|---|---|---|
| 300 s | 34,658 | 11,618 s (3.23 h) |
| 900 s | 37,051 | 10,868 s (3.02 h) |
| 1800 s | 37,402 | 10,765 s (2.99 h) |
| 3600 s | 37,459 | 10,749 s (2.99 h) |
| 7200 s | 38,116 | 10,564 s (2.93 h) |
| 14400 s | 38,428 | 10,478 s (2.91 h) |
| 28800 s | **38,578** (1,111,068,389 B total) | **10,437 s (2.90 h)** |
| 28800 s, **without** `--emit-mobility-oracle` | 3,945 | 102,065 s (28.4 h) |

So on the reference grid at ~170 concurrent vehicles the in-memory guarantee holds for **2.90 hours
of simulated time** with the oracle record on, and for about a day without it — and the rate has
converged, so that number is stable rather than an extrapolation from a short run. The 8 h rung's
withheld set is **1.11 GB**, 2.76× the ceiling. Scaling by concurrency to the InTAS AM peak (~3,775
concurrent, 22×) puts the spill at **≈ 8 minutes of simulated time**.

These are computed from measured byte rates; **no run here had an isolated worker attached, so the
spill, the XOR seal and the unseal are predicted rather than observed** (§8). The configuration that
would observe it already exists — any run past ~3 h with the oracle on and one isolated check.

---

## 2. Determinism at length

| duration | data_digest (first 16) | reproduces | files compared | files differing | counts identical | peak MiB run A / run B |
|---|---|---|---|---|---|---|
| 300 s | `1c2248f308256e2a` | **YES** | 11 | **0** | True | 128 / 128 |
| 900 s | `23dfbc962b10b56e` | **YES** | 11 | **0** | True | 193 / 193 |
| 1800 s | `0fd03e8793e9d377` | **YES** | 11 | **0** | True | 284 / 285 |
| 3600 s | `bcbf5d6a5c943207` | **YES** | 11 | **0** | True | 466 / 466 |
| 7200 s | `88bb74c464309dfb` | **YES** | 11 | **0** | True | 849 / 850 |
| 14400 s | `625a29446a568606` | **YES** | 11 | **0** | True | 1633 / 1633 |
| 28800 s | `646688ed376dc37f` | *not repeated* | - | - | - | 3159 |

Each rung up to **14400 s (4 h)** was run twice, same seed, same configuration, and **every one of
the 11 output files** (not just the digest) was compared by SHA-256. **Zero files differ at any
rung.** There is no drift: no accumulating floating-point term and no dict whose iteration order
depends on insertion history reaches an output at any duration tested — a 48× longer run reproduces
exactly as reliably as a 300 s one.

The 28800 s rung was **not** repeated (its two runs would have cost 110 min of the budget, almost all
of it in the quadratic finalisation of §1.2), so determinism is *verified* to 4 h and *assumed* at
8 h. Nothing in the mechanisms below is duration-dependent, so there is no reason to expect it to
fail there — but it is not measured, and §8 says so.

The engine earns this structurally rather than by luck, and the two mechanisms are worth naming
because they are exactly the ones a long run usually breaks:

* **The time base is multiplicative, not accumulative.** `t = step * cfg.dt` (run.py:6750). An
  engine that wrote `t += dt` would accumulate a rounding term proportional to the step count and
  would start colliding or skipping `round(t, 3)` timestamps on a long run; this one cannot.
* **Every RNG is keyed, not shared.** Per-vehicle streams are `random.Random(f"{seed}:veh:{vid}")`,
  and the attack, sensor, DENM, onset and pulse streams are separately keyed by name and vid. A
  vehicle's draws therefore do not depend on how many vehicles preceded it, which is also what makes
  the prefix nesting of §0 exact.

Two caveats that the digests do not cover. `manifest.json` is outside `data_digest` by construction
(it carries `build_utc`), so it differs between the two runs of a rung and is excluded from the file
comparison. And the pinned goldens are pinned to a CPython version as much as to a seed —
`gauss`/`uniform`/`shuffle` carry no cross-version reproducibility guarantee — which the manifest's
`runtime` block records; that is a *version* hazard, not a *duration* hazard, and nothing here
touches it.

---

## 3. Metric stability

Because a shorter rung is an exact prefix of a longer one (§0), every movement in this table is the
metric responding to the window, not to different traffic.

| metric | 300 s | 900 s | 1800 s | 3600 s | 7200 s | 14400 s | 28800 s |
|---|---|---|---|---|---|---|---|
| `traffic.trace_segments` | 42,403 | 139,014 | 281,807 | 565,624 | 1,142,263 | 2,324,572 | 4,622,931 |
| `traffic.speed_p50_mps` | 7.8465 **pass** | 7.9010 **pass** | 7.9220 **pass** | 7.9440 **pass** | 7.9120 **pass** | 7.8750 **pass** | 7.9020 **pass** |
| `traffic.speed_p95_mps` | 13.3610 **pass** | 13.2970 **pass** | 13.2260 **pass** | 13.2330 **pass** | 13.2090 **pass** | 13.1580 **pass** | 13.1720 **pass** |
| `traffic.speed_max_mps` | 19.2040 **pass** | 19.7990 **pass** | 19.7990 **pass** | 19.7990 **pass** | 19.7990 **pass** | 19.7990 **pass** | 19.7990 **pass** |
| `traffic.moving_vehicle_frac` | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** |
| `traffic.accel_within_hard_bound_frac` | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** |
| `traffic.accel_within_comfort_frac` | 0.9765 **pass** | 0.9766 **pass** | 0.9773 **pass** | 0.9774 **pass** | 0.9773 **pass** | 0.9771 **pass** | 0.9771 **pass** |
| `traffic.lateral_discontinuity_events` | 0.0000 **pass** | 0.0000 **pass** | 0.0000 **pass** | 0.0000 **pass** | 0.0000 **pass** | 0.0000 **pass** | 0.0000 **pass** |
| `traffic.teleport_events` | 0 **pass** | 0 **pass** | 0 **pass** | 0 **pass** | 0 **pass** | 0 **pass** | 0 **pass** |
| `traffic.overlap_events` | 434 **fail** | 468 **fail** | 465 **fail** | 409 **fail** | 394 **fail** | 309 **fail** | 319 **fail** |
| `traffic.headway_p50_s` | 4.6589 | 4.6324 | 4.7617 | 4.7732 | 4.8638 | 5.0808 | 5.1279 |
| `traffic.headway_below_floor_frac` | 0.0016 **pass** | 0.0007 **pass** | 0.0006 **pass** | 0.0007 **pass** | 0.0007 **pass** | 0.0007 **pass** | 0.0006 **pass** |
| `traffic.headway_ks_shifted_exponential` | 0.1975 **fail** | 0.1964 **fail** | 0.1919 **fail** | 0.1898 **fail** | 0.1843 **fail** | 0.1624 **fail** | 0.1614 **fail** |
| `traffic.fd_capacity_veh_h_lane` | 590.8968 **fail** | 613.7635 **fail** | 601.3883 **fail** | 600.2565 **fail** | 593.9986 **fail** | 597.3481 **fail** | 595.0789 **fail** |
| `traffic.fd_backward_wave_speed_kmh` | -3.4158 **fail** | -3.8977 **fail** | -2.5936 **fail** | -1.3165 **fail** | 0.0085 **fail** | 0.0114 **fail** | -1.7815 **fail** |
| `traffic.survivorship_vehicle_steps_frac` | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** | 1.0000 **pass** |
| `comm.honest_links` | 9 | 29 | 56 | 96 | 185 | 358 | 755 |
| `comm.awareness_ratio_100m` | na | na | 0.2619 | 0.3937 | 0.6440 | 0.9511 | 0.9905 |
| `comm.awareness_ratio_200m` | na | na | 0.2979 | 0.4117 | 0.6249 | 0.8226 | 0.7559 |
| `comm.awareness_ratio_300m` | na | na | 0.3537 | 0.4443 | 0.5727 | 0.8953 | 0.7480 |
| `comm.effective_range_m` | na | na | 65.3543 | 74.2563 | 418.1621 | 494.4322 | 497.6864 |
| `comm.pdr_gray_zone_width_m` | na | na | 421.2841 **pass** | 463.1676 **pass** | 459.0405 **pass** | 68.5854 **fail** | 394.4536 **pass** |
| `comm.pdr_absolute_200m` | 0.4178 | 0.4165 | 0.4133 | 0.4111 | 0.4132 | 0.4083 | 0.4132 |
| `comm.nar90_equivalent_range_m` | 242.7000 | 242.3000 | 244.6000 | 241.5000 | 242.6000 | 241.2000 | 242.1000 |
| `comm.link_state_los_fraction` | 0.3309 | 0.3269 | 0.3241 | 0.3312 | 0.3182 | 0.3266 | 0.3169 |
| `comm.pdr_gray_zone_ratio` | 7.4886 | 7.4643 | 7.5150 | 7.4893 | 7.4944 | 7.5372 | 7.5384 |

### 3.1 Stable

`fd_capacity_veh_h_lane` settles inside ±2% from the first rung on (590.9 / 613.8 / 601.4 / 600.3 /
594.0 / 597.3 / 595.1 over 96×) — which is worth saying explicitly, because it is one of the two
metrics `ROADMAP-PERFECT.md` §P3 flags as failing: **it fails for a real reason and not because of the
window.** `headway_below_floor_frac`, `accel_within_*`, `moving_vehicle_frac`,
`lateral_discontinuity_events` and `teleport_events` do not move at all.

The whole **analytic** half of the comm panel is stable to under 4% across the same 96× range —
`link_state_los_fraction` 0.3309 → 0.3169, `pdr_absolute_200m` 0.4178 → 0.4132,
`nar90_equivalent_range_m` 242.7 → 242.1, `pdr_gray_zone_ratio` 7.4886 → 7.5384. That is expected and
it is the useful control: those are properties of the scene and the configured physics, computed from
the link-state model rather than from observed links. Everything in the comm panel that *is* computed
from observed links is in §3.4 and is not stable at any duration reached here — so the split inside
one panel is exactly the split between "computed" and "sampled", not between "comm" and "traffic".

### 3.2 Drifting monotonically — small, but it is the run length being measured

Five numbers move monotonically with the window and nothing else. None is large, and that is the
point: they are small, systematic and in a fixed direction, which is the signature of a metric
partly measuring the run length rather than the road.

| metric | 300 s | 3600 s | 28800 s | over 96× | why |
|---|---|---|---|---|---|
| `traffic.speed_p95_mps` | 13.3610 | 13.2330 | **13.1720** | **−1.4%**, monotone | the fill transient: an empty network at t = 0 delivers free-flow speeds that a short window over-weights |
| `traffic.headway_ks_shifted_exponential` | 0.1975 | 0.1898 | **0.1614** | **−18.3%**, monotone | same; the gate is 0.15 and it fails at every duration, so no verdict moves, but the number is walking toward it |
| `traffic.headway_p50_s` | 4.6589 | 4.7732 | **5.1279** | **+10.1%**, monotone | ditto, from the other side |
| `traffic.speed_max_mps` | 19.2040 | 19.7990 | 19.7990 | +3.1% then fixed | a record statistic: it can only grow, and it saturates once the run has seen the fastest vehicle type on the longest free stretch |
| `traffic.accel_within_comfort_frac` | 0.9765 | 0.9774 | 0.9771 | +0.1% | the same transient, negligible |

`speed_p50_mps` is the near-miss: 7.8465 → 7.9440 → 7.9020, a 1.2% wobble with no consistent
direction. Treat it as stable.

**`fd_backward_wave_speed_kmh` is not drifting, it is unstable, and it should not be quoted at any
duration on this arm.** Across the ladder it reads −3.4158, −3.8977, −2.5936, −1.3165, **+0.0085**,
**+0.0114**, −1.7815 — it changes sign twice. It is the slope of a regression on the congested branch
of the flow–density scatter, and on a network that is barely congested that branch is a handful of
cells. It fails its 15–20 km/h reference at every duration, so no verdict moves; the *value* is
noise.

The transient behind the monotone rows is quantifiable, and it is the honest reason to distrust a
300 s window: `mean_record_span_s_never_revoked` reads **68.242 s at 300 s** and converges to
**78.594 s** by 28800 s, so a 300 s run understates how long a benign vehicle is actually present by
**13.2%**. And the marginal report rate is **26.08 s⁻¹ over the first 300 s** against **18.0–19.6 s⁻¹
at every interval thereafter**, so a report-volume figure read at 300 s over-states the steady state
by up to 45%.

### 3.3 Broken by the 240-instant caps

`realism_bench` sub-samples deterministically at three places:

```python
MAX_TIME_BUCKETS = 240        # cap on co-presence snapshots examined
HEADWAY_MAX_INSTANTS = 240    # cap on instants scanned for leaders
MAX_VEH_PER_BUCKET = 400      # cap on vehicles per snapshot
```

With `dt = 1.0 s` a 240 s run is examined in full; a 3600 s run is examined at **6.7%** of its
instants and the 28800 s run at **0.83%**. For a *distributional* metric that is harmless — the
instants are evenly spaced across the run, so the sample stays representative in time, which is why
`headway_ks` and the speed quantiles behave. For a *count* it is fatal, and the ladder shows it
end to end:

| | 300 s | 900 s | 1800 s | 3600 s | 7200 s | 14400 s | 28800 s |
|---|---|---|---|---|---|---|---|
| `traffic.trace_segments` (the traffic behind it) | 42,403 | 139,014 | 281,807 | 565,624 | 1,142,263 | 2,324,572 | **4,622,931** |
| instants examined / instants available | 240/300 | 240/900 | 240/1800 | 240/3600 | 240/7200 | 240/14400 | **240/28800** |
| `traffic.overlap_events` **HARD** | 434 | 468 | 465 | 409 | 394 | 309 | **319** |

**The trajectory sample grows 109× and the count falls 26%.** Two things follow. The gate remains a
valid **one-sided test** — an overlap seen is an overlap that happened, so a `fail` is a real fail —
but its **sensitivity to a rare overlap does not improve with run length**, so a long run is not a
better search for the defect than a short one; and its **magnitude must never be compared across
durations**, because 434 → 319 over a 96× longer run is a **131× fall in the underlying rate** that
nothing in the row says out loud. `traffic.teleport_events` reads the same capped instant list and
inherits the same property (it is 0 throughout on this arm, so nothing is visible, which is exactly
the problem).

### 3.4 Not measurable at all below a duration — and not converged above it

`comm.honest_links` is the sample size for every awareness metric, and at the default
`emit_sample_prob = 0.03` it grows linearly with duration, crossing `MIN_SAMPLES = 30` at roughly
**930 s**. Below that the awareness panel is honestly `na`. Above it, it is *reported* — and it moves
by a factor of four:

| | 300 s | 900 s | 1800 s | 3600 s | 7200 s | 14400 s | 28800 s |
|---|---|---|---|---|---|---|---|
| `comm.honest_links` | 9 | 29 | 56 | 96 | 185 | 358 | **755** |
| `comm.awareness_ratio_100m` | na | na | 0.2619 | 0.3937 | 0.6440 | 0.9511 | **0.9905** |
| `comm.awareness_ratio_200m` | na | na | 0.2979 | 0.4117 | 0.6249 | 0.8226 | **0.7559** |
| `comm.awareness_ratio_300m` | na | na | 0.3537 | 0.4443 | 0.5727 | 0.8953 | **0.7480** |
| `comm.effective_range_m` | na | na | 65.35 | 74.26 | 418.16 | 494.43 | **497.69** |
| `comm.pdr_gray_zone_width_m` | na | na | 421.28 **pass** | 463.17 **pass** | 459.04 **pass** | **68.59 FAIL** | 394.45 **pass** |

`awareness_ratio_100m` moves **3.8×** between the point where the harness starts publishing it and
8 h. `effective_range_m` moves **7.6×**. And `pdr_gray_zone_width_m` — which *is* graded — **fails at
14400 s and passes at every other duration**, so on this metric the duration alone decides the
verdict.

The cause is sample size, not physics. 96 links spread over a 20-bin distance curve is about five
links per bin; the normalisation base is whichever near bin first becomes populated, so the base
itself changes as bins fill. **These are the least trustworthy quantities on the scorecard at every
duration reachable here**, and `MIN_SAMPLES = 30` gates the *existence* of the row rather than its
precision. The fix is more links, not more time: raising `emit_sample_prob` costs bytes linearly
(§1.4) and buys sample size immediately.

Everything in the comm panel that is computed analytically rather than from observed links is
rock-stable across the same 96× range — `pdr_absolute_200m` 0.4178 → 0.4132, `nar90_equivalent_range_m`
242.7 → 242.1, `link_state_los_fraction` 0.3309 → 0.3169, `pdr_gray_zone_ratio` 7.4886 → 7.5384. The
split is exactly between what is measured from 755 links and what is computed from the scene.

---

## 4. The SCMS timescales that only exist on long runs

Every curve here is read out of **one** artifact by bucketing on simulated time, which is only
legitimate because of the prefix property of §0 — and the check is that curves from four different
runs must then coincide. They do:

| cumulative MA precision at t = | 300 s run | 900 s run | 1800 s run | 3600 s run | 7200 s run | 14400 s run | 28800 s run |
|---|---|---|---|---|---|---|---|
| **300 s** | 0.5987 (152) | 0.5987 (152) | 0.5987 (152) | 0.5987 (152) | 0.5987 (152) | 0.5987 (152) | 0.5987 (152) |
| **900 s** | - | 0.5839 (483) | 0.5826 (484) | 0.5826 (484) | 0.5826 (484) | 0.5826 (484) | 0.5826 (484) |
| **1800 s** | - | - | 0.5594 (960) | 0.5594 (960) | 0.5594 (960) | 0.5594 (960) | 0.5594 (960) |
| **3600 s** | - | - | - | 0.5513 (1,843) | 0.5513 (1,843) | 0.5513 (1,843) | 0.5513 (1,843) |
| **7200 s** | - | - | - | - | 0.5504 (3,730) | 0.5505 (3,731) | 0.5505 (3,731) |
| **14400 s** | - | - | - | - | - | 0.5428 (7,782) | 0.5428 (7,782) |
| **28800 s** | - | - | - | - | - | - | 0.5412 (15,423) |

| duration | attackers | revocations | TP | FP | precision | FP per TP | recall of all attackers |
|---|---|---|---|---|---|---|---|
| 300 s | 100 | 152 | 91 | 61 | **0.5987** | 0.6703 | 0.9100 |
| 900 s | 298 | 483 | 282 | 201 | **0.5839** | 0.7128 | 0.9463 |
| 1800 s | 560 | 960 | 537 | 423 | **0.5594** | 0.7877 | 0.9589 |
| 3600 s | 1,057 | 1,843 | 1,016 | 827 | **0.5513** | 0.8140 | 0.9612 |
| 7200 s | 2,141 | 3,730 | 2,053 | 1,677 | **0.5504** | 0.8169 | 0.9589 |
| 14400 s | 4,395 | 7,782 | 4,224 | 3,558 | **0.5428** | 0.8423 | 0.9611 |
| 28800 s | 8,692 | 15,423 | 8,347 | 7,076 | **0.5412** | 0.8477 | 0.9603 |

### 4.1 The misbehaviour authority's cumulative precision

Precision falls steeply while the network fills, then plateaus and keeps sliding slowly. **Reading it
at 300 s catches the transient, not the steady state, and the transient is the flattering end.**

| | 300 s | 900 s | 1800 s | 3600 s | 7200 s | 14400 s | 28800 s |
|---|---|---|---|---|---|---|---|
| cumulative precision | 0.5987 | 0.5839 | 0.5594 | 0.5513 | 0.5504 | 0.5428 | **0.5412** |
| FP per TP | 0.6703 | 0.7128 | 0.7877 | 0.8140 | 0.8169 | 0.8423 | **0.8477** |
| recall of all attackers | 0.9100 | 0.9463 | 0.9589 | 0.9612 | 0.9589 | 0.9611 | 0.9603 |
| benign vehicles wrongly revoked | 61 | 201 | 423 | 827 | 1,677 | 3,558 | **7,076** |

Two things are worth separating. **Recall converges** by ~1800 s (0.9100 → 0.9589 and then flat):
that metric simply needs enough attackers to have finished their exposure. **Precision does not
converge** — it falls another 3.3% between 1800 s and 28800 s, and `fp_per_tp` climbs the whole way.
Nothing about the detector changes; what changes is that the false-positive rate per unit exposure is
very slightly the higher of the two accumulating rates, so the ratio keeps sliding.

This confirms and refines `TRAFFIC-PANEL-SURVIVORSHIP.md` §6, whose cross-duration sweep found
precision falling 0.833 → 0.599 up to 300 s and then "plateauing at 0.54–0.58 from 300 s to 2400 s".
Read out of single runs and checked against each other, the plateau is real but it is a **slowly
declining** plateau, and it is still declining at 8 h. **At 8 h the misbehaviour authority has
revoked 7,076 benign vehicles** — 46% of everything on its CRL.

### 4.2 CRL growth — monotone by construction

| duration | CRL entries at end | entries / sim-s | mean entry residency s | residency / duration |
|---|---|---|---|---|
| 300 s | 152 | 0.5067 | 143.5 | 0.4784 |
| 900 s | 483 | 0.5367 | 434.5 | 0.4827 |
| 1800 s | 960 | 0.5333 | 890.0 | 0.4944 |
| 3600 s | 1,843 | 0.5119 | 1,834.3 | 0.5095 |
| 7200 s | 3,730 | 0.5181 | 3,610.2 | 0.5014 |
| 14400 s | 7,782 | 0.5404 | 7,090.6 | 0.4924 |
| 28800 s | 15,423 | 0.5355 | 14,392.4 | 0.4997 |

### 4.3 Certificates: expiry does not happen, and the "lifetime" is the run length

Two separate mechanisms, both of which make certificate expiry unobservable.

**MA-visible.** `_cert_status_row` (run.py:7633) writes every row as

```python
kw = dict(cert_digest=d, first_seen=f, last_seen=cert_last_seen[d],
          valid_from=0.0, valid_to=total_time, issuing_pca="PCA-1", ...)
```

so `ma_cert_status.jsonl` claims `[0, run length]` for **100.0% of certificates at every rung**. A
consumer that trains or gates on `valid_to` is reading the `--duration` flag.

**Issued.** The engine's own windows in `gt_identity_map.jsonl` are real per-vehicle windows —
except that the *final* certificate of every vehicle is capped forward,
`vt = max(vt, total_time + cfg.dt)` (run.py:5373), so that a present benign vehicle can never show
an expired certificate. With `rotate_period_s = 0` (**the default**) every vehicle has exactly one
certificate, which is therefore always the final one, so the cap applies to all of them:

| duration | certs issued | certs / vehicle | issued span p50 s | issued span max s | issued p50 / duration | MA-visible span p50 s | rows with MA span == duration |
|---|---|---|---|---|---|---|---|
| 300 s | 623 | 1.0506 | 315.0 | 600.0 | 1.05 | 300.0 | **1.0** |
| 900 s | 1,883 | 1.0467 | 461.604 | 900.877 | 0.512893 | 900.0 | **1.0** |
| 1800 s | 3,752 | 1.0451 | 862.328 | 1800.877 | 0.479071 | 1800.0 | **1.0** |
| 3600 s | 7,479 | 1.0418 | 1721.225 | 3600.877 | 0.478118 | 3600.0 | **1.0** |
| 7200 s | 14,987 | 1.0426 | 3440.135 | 7200.877 | 0.477797 | 7200.0 | **1.0** |
| 14400 s | 30,224 | 1.0433 | 6864.621 | 14400.877 | 0.47671 | 14400.0 | **1.0** |
| 28800 s | 60,242 | 1.043 | 13850.043 | 28800.877 | 0.480904 | 28800.0 | **1.0** |

The issued-window median tracks half the run length, and the maximum is exactly `duration + dt`. So
`certValidity` as a detection signal cannot fire on a benign vehicle at any duration, which is the
intent — but it also means **no run of any length exercises certificate expiry**, and the only way to
get a finite certificate lifetime is `--rotate-period`, where the cap touches only each vehicle's
last cert.

### 4.4 Pseudonym rotation is a per-VEHICLE timescale, not a per-run one

`n_rot = ceil(life / rotate_period_s)` — the pool is sized by the vehicle's **trip**, so a longer run
gives more vehicles rather than more rotations per vehicle. Measured certificates per vehicle are
**1.0506 / 1.0467 / 1.0451 / 1.0418 / 1.0426 / 1.0433 / 1.0430** across the whole ladder — flat to
0.8% over a 96× duration range — and the surplus over 1.0 is
entirely Sybil ghosts (`sybil_ghosts = 6`), not rotation: rotation is **off by default**. So on the
default arm a pseudonym is a stable identifier for a vehicle's entire life at every duration, and
nothing about linkability changes with run length. The cost of turning rotation on is not in the loop
but in finalisation and in the certificate files, both of which scale with the rotation factor
(§1.2).

### 4.5 Attacker lifetime is capped by the trip, not by the run

`v.attack_to = (spawn_time + life)` in flow mode (run.py:5431). Measured attack spans:

| duration | attackers | span p50 s | span p95 s | **span max s** | max / duration |
|---|---|---|---|---|---|
| 300 s | 100 | 313.0 | 427.0 | **541.0** | 1.803333 |
| 900 s | 298 | 313.0 | 484.0 | **598.0** | 0.664444 |
| 1800 s | 560 | 256.0 | 484.0 | **598.0** | 0.332222 |
| 3600 s | 1,057 | 256.0 | 484.0 | **598.0** | 0.166111 |
| 7200 s | 2,141 | 256.0 | 484.0 | **598.0** | 0.083056 |
| 14400 s | 4,395 | 256.0 | 484.0 | **598.0** | 0.041528 |
| 28800 s | 8,692 | 256.0 | 484.0 | **598.0** | 0.020764 |

From 1800 s on the distribution is stationary — p50 256 s, p95 484 s, **max 598 s** — and it does not
move however long the run gets. (At 300 and 900 s it reads *longer* only because `gt_attacks` is not
clipped to the simulation end, so those rows describe windows that partly do not exist.) Two
consequences: an 8 h run contains ~57,000 attackers each hostile for at most ten minutes, never one
adversary that persists; and `--attack-duty-cycle` with `--attack-pulse-period` can only fit
`598 / period` pulses, so a duty cycle whose period exceeds a trip is unrepresentable at any
duration.

---

## 5. Survivorship

| duration | vehicles | revoked | revoked frac | steps simulated | steps broadcast | **survival** | mean rec span revoked | mean rec span never-revoked | mean sim span revoked |
|---|---|---|---|---|---|---|---|---|---|
| 300 s | 593 | 152 | 0.256324 | 42,993 | 34,938 | **0.812644** | 29.329 | 68.242 | 82.322 |
| 900 s | 1,799 | 483 | 0.268482 | 140,812 | 114,664 | **0.814306** | 28.267 | 75.447 | 82.404 |
| 1800 s | 3,590 | 960 | 0.267409 | 285,396 | 235,067 | **0.823652** | 29.814 | 77.161 | 82.24 |
| 3600 s | 7,179 | 1,843 | 0.256721 | 572,801 | 476,531 | **0.831931** | 29.99 | 77.631 | 82.225 |
| 7200 s | 14,375 | 3,730 | 0.259478 | 1,156,637 | 958,832 | **0.828983** | 30.273 | 78.123 | 83.304 |
| 14400 s | 28,970 | 7,782 | 0.268623 | 2,353,539 | 1,936,250 | **0.822697** | 30.653 | 78.77 | 84.275 |
| 28800 s | 57,758 | 15,423 | 0.267028 | 4,680,687 | 3,853,989 | **0.823381** | 30.417 | 78.594 | 84.018 |

**Run length is not the driver — density is, and this settles a claim in `ROADMAP-PERFECT.md` §P3.**
That document says every flow and headway figure "is low by a factor that grows with run length". On
this arm, at *fixed* density, it does not. Over a **96× duration range** survival moves 0.8126 →
0.8143 → 0.8237 → 0.8319 → 0.8290 → 0.8227 → **0.8234** — it rises through the fill transient and
then sits at 0.823 ± 0.005 forever. The revoked *fraction* is equally flat (0.2563–0.2686). What
grows with run length is the absolute *number* of revoked vehicles (152 → 15,423), not the fraction
of vehicle-steps lost. The peak-hour 0.4378 is a density effect (~3,775 concurrent against ~170
here), exactly as `TRAFFIC-PANEL-SURVIVORSHIP.md` §6 concluded from its two controlled sweeps.

So the §P3 sentence should read **"low by a factor that grows with DENSITY"**. At fixed density the
correction is a constant, and a 300 s measurement of it is right to within 1.3%.

The reason survival *rises* through the transient is a boundary effect, and it is worth stating
because it runs the other way from every other duration bias here: a run ends mid-trip for whoever is
still driving, so a short run clips never-revoked vehicles more than revoked ones.
`mean_record_span_s_never_revoked` is **68.242 s at 300 s** and converges to **78.594 s at 28800 s**
— so **the 300 s window under-reads a benign vehicle's record span by 13.2%** — while the revoked
span (29.329 → 30.417 s) barely moves, because a revoked vehicle's record ends before the run does.
The same asymmetry is visible in `mean_simulated_span_s_revoked`, 82.322 → 84.018 s.

**A reporting hazard.** With `--emit-mobility-oracle` on, the scorecard's graded row
`traffic.survivorship_vehicle_steps_frac` reads **1.0 / pass** at every rung. That is correct on its
own terms — it measures how much of the simulated mobility the *panel's input* contains, and the
oracle record contains all of it — but the *dataset* shipped alongside still lost **17.7%** of its
vehicle-steps to enforcement, and at the 8 h rung that is **826,698 vehicle-steps deleted**. Both
numbers are in the scorecard (`survivorship.vehicle_steps_broadcast` and `...simulated` carry the
manifest tallies verbatim), but only the 1.0 is graded, and the gap between the graded row and the
dataset widens with density. See §6.

---

## 6. What to trust at what duration

**Trustworthy at any duration** — moved under 4% across the whole **96×** range:
`fd_capacity_veh_h_lane` (590.9 → 595.1, ±2%), `speed_p50_mps` (7.85 → 7.90),
`accel_within_hard_bound_frac`, `accel_within_comfort_frac`, `moving_vehicle_frac`,
`lateral_discontinuity_events`, `headway_below_floor_frac`, the whole analytic comm block
(`link_state_los_fraction*` 0.3309 → 0.3169, `pdr_absolute_200m` 0.4178 → 0.4132,
`nar90_equivalent_range_m` 242.7 → 242.1, `pdr_gray_zone_ratio` 7.4886 → 7.5384), the survivorship
tallies in `manifest.counts` (0.8126 → 0.8234), the revoked fraction (0.2563 → 0.2670), the
certificates-per-vehicle count (1.0506 → 1.0430), and every digest.

**Only trustworthy BELOW a duration:**

| quantity | trustworthy up to | why |
|---|---|---|
| `traffic.overlap_events`, `traffic.teleport_events` as **counts** | **240 s** (`MAX_TIME_BUCKETS × dt`) | beyond it the count is a fixed-size sample: coverage 6.7% at 3600 s, **0.83% at 28800 s**; the count falls 434 → 319 while the traffic behind it grows 109× |
| any traffic metric under `--demand rush` / `--demand night`, compared across durations | never comparable | the profile is a function of `t / total_time` (§0) |
| `ma_cert_status.valid_to` as a certificate lifetime | never | it is `total_time` for 100.0% of rows |
| `gt_identity_map.valid_to` as a certificate lifetime, rotation off | never | capped to `total_time + dt`; median ≈ 0.48 × duration |
| `gt_attacks.end_time` | **≥ 1800 s only** | below that it is not clipped to the run and describes windows past the end (span p50 313 s on a 300 s run) |
| `comm.pdr_gray_zone_width_m` as a graded verdict | never on this arm | it is the only metric whose **pass/fail flips with duration alone**: FAIL at 14400 s, pass at all six other rungs |

**Only trustworthy ABOVE a duration:**

| quantity | needs at least | why |
|---|---|---|
| `comm.awareness_ratio_*`, `comm.effective_range_m` | **beyond 28800 s** — *not converged anywhere on this ladder* | `awareness_ratio_100m` moves 0.2619 → 0.9905 (3.8×) and `effective_range_m` 65.4 → 497.7 (7.6×) between the point where the harness starts publishing them and 8 h |
| `comm.honest_links ≥ MIN_SAMPLES` at `emit_sample_prob = 0.03` | ~930 s | linear: 9 / 29 / 56 / 96 / 185 / 358 / 755 |
| `traffic.speed_max_mps` and every other record statistic | ~900 s | 19.204 at 300 s, 19.799 from 900 s on |
| `traffic.headway_ks_shifted_exponential`, `traffic.headway_p50_s` | **not converged at 28800 s** | monotone −18.3% and +10.1% over the ladder, still moving at the top rung |
| `mean_record_span_s_never_revoked` | ~3600 s | 68.242 s at 300 s vs 78.594 s at 28800 s — the run end clips trips |
| MA precision / `fp_per_tp` as a steady-state figure | **~3600 s for 1%, never for 0.1%** | 0.5987 at 300 s, 0.5513 at 3600 s, 0.5412 at 28800 s and still sliding (§4.1) |
| MA recall | ~1800 s | 0.9100 at 300 s, flat at 0.959–0.961 from 1800 s on |
| the marginal report rate | ~600 s | 26.08 s⁻¹ over the first 300 s against 18.00–19.59 s⁻¹ thereafter |

**What the harness should refuse to report rather than report confidently wrong:**

1. **`overlap_events` / `teleport_events` as bare counts when `duration / dt > MAX_TIME_BUCKETS`.**
   Publish the instants examined and the coverage fraction on the row, and grade the rate rather than
   the count — or raise the cap when the run is long enough to afford it. Today a row reading
   "`overlap_events 319 FAIL`" on an 8-hour run means "319 in the 240 instants we looked at, which
   is 0.83% of the run", and that is not what any reader takes from it.
2. **Every awareness metric at fewer than a few hundred honest links.** `MIN_SAMPLES = 30` gates the
   *existence* of the row, not its precision: at the point where the row first appears it is a
   factor of 3.8 away from its 8-hour value, and `comm.pdr_gray_zone_width_m` flips FAIL → pass on
   duration alone. The floor should scale with the number of populated distance bins, and the row
   should carry links-per-bin so a reader can see it.
3. **Any cross-duration comparison on a `--demand rush` / `--demand night` dataset.** The scorecard
   should refuse — or at minimum stamp `demand_profile` into the traffic panel's details so a reader
   cannot line two such runs up.
4. **`traffic.survivorship_vehicle_steps_frac = 1.0 / pass` as the whole survivorship story.** When
   the panel reads the oracle the graded row should either carry the dataset's own
   `vehicle_steps_survival_frac` from `manifest.counts` beside it, or be renamed to say that it grades
   the panel's input. A reader who sees only the graded row on a peak-density dataset will conclude
   nothing was truncated when 56% of it was.
5. **`ma_cert_status.valid_from` / `valid_to`.** As written they are not a certificate profile, they
   are the `--duration` flag. Either emit the real per-certificate window or omit the fields.

---

## 7. Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1
cd C:\Users\Administrator\Documents\SCMS-Simulator
$env:PYTHONPATH = "$PWD\src"; $env:PYTHONHASHSEED = "0"

# the ladder: cost, determinism, scorecard and SCMS curves at every rung
python tools/long_run_probe.py --scenario ref_grid --durations 300,900,1800,3600,7200,14400,28800 `
  --repeat --bench --curves --out-root C:/Temp/longrun `
  --json .realism_cache/longrun/ladder.json --markdown

# what accumulates: same duration, capped vs uncapped population
python tools/long_run_probe.py --memory-ab 1800 --memory-ab-cap 600 --out-root C:/Temp/longrun_ab

# ... and by line of run.py, under tracemalloc, snapshotted at peak
python tools/long_run_probe.py --attribute-memory 1800 --out-root C:/Temp/longrun_tm

# is the loop's growth the cyclic collector?  (identical digest either way)
python tools/long_run_probe.py --gc-ab 3600 --out-root C:/Temp/longrun_gc

# the quadratic finalisation term, isolated from everything else
python tools/long_run_probe.py --crl-assert-cost 152:623,483:1883,960:3752,1843:7479,3730:14990

# run.py's double whole-file SHA-256 against one streamed pass, on a real dataset
python tools/long_run_probe.py --hash-cost C:/Temp/longrun/ref_grid_7200

# the SCMS curves out of artifacts that already exist (no simulation at all)
python tools/long_run_probe.py --postprocess C:/Temp/longrun --durations 300,900,1800,3600,7200,14400

# the demand-profile control: prefix nesting holds under uniform and breaks under rush
python tools/long_run_probe.py --scenario ref_grid_rush --durations 300,900 --curves

# one report from ladders written by separate invocations
python tools/long_run_probe.py --merge .realism_cache/longrun/ladder_lo.json,`
  .realism_cache/longrun/ladder_hi.json,.realism_cache/longrun/ladder_8h.json
```

The ladder above was in fact taken as three invocations (300–3600, then 7200–14400, then 28800 on its
own out-root) so the long rungs could overlap; `--merge` is what puts them back together, and the
per-rung JSONs are in `.realism_cache/longrun/`. Prefix nesting (§0) is what makes that legitimate:
the rungs are the same experiment however they were scheduled.

**Measured wall clock.** The ladder is 12.5 s at 300 s and 3,303.5 s at 28800 s; the seven rungs plus
their repeats plus `realism_bench` at every rung came to roughly 2 h 40 min, on a host that was also
carrying a parallel InTAS workstream. `realism_bench` itself is linear and cheap — 0.9 s at 300 s,
7.8 s at 3600 s, 56.8 s at 28800 s over a 997 MB oracle record. The auxiliary experiments are all
short: the memory A/B is 2 min, the GC A/B 4 min, the CRL-assert sweep 1.5 min, `--hash-cost` under
a second, and `--postprocess` reads existing artifacts with no simulation at all.

---

## 8. Open — and what needs `run.py`

`run.py` is owned by another workstream in this task, so everything below is described rather than
done. Ordered by measured impact. *(Line numbers are as of this writing; that file was edited by the
parallel workstream during this measurement, so grep the quoted marker rather than trusting the
number.)*

1. **The end-of-run CRL sanity check is O(R²) and should be O(R + C)** (run.py:7616, the
   `Real-linkage sanity` comment). It is an
   `assert` over a structure the engine itself built: each revoked vehicle's CRL entry is the one
   appended at its own `resolve_and_revoke`, so the check does not need to scan `crl_entries` at all
   — record the entry index on `revoked_vehicles[vid]` and check that one. Inverting the outer loops
   (iterate `cert_first_seen` once, grouping by `veh_vid`) removes the O(R·C) scan at the same time.
   **Writes nothing, so it cannot move a digest.** This is the single largest cost defect found here.
2. **The demand profile has no clock** (run.py:5548). `demand_mult(frac)` should take absolute
   simulated seconds with an explicit period — `--demand-period-s` (default 86400) and a
   `--start-time-of-day` — so that "the AM peak" is at 08:00 rather than at 25% of whatever the run
   happens to be. Digest-moving, and it is the change that makes "a city's demand profile across a
   day" expressible at all.
3. **Bound the population.** The arrival loop materialises every vehicle before step 0 and nothing is
   ever released, so peak memory is `97.6 MiB + 51.3 KB × vehicles`. Generating a vehicle lazily when
   `tt` passes preserves the draw order exactly (it is one `rrng` stream consumed in order), and
   dropping despawned vehicles from `vehicles` / `vrng` / `digest_to_vehicle` while streaming
   `gt_identity_map` and `ma_cert_status` out as they retire would make memory a function of
   concurrency instead of duration. Digest-sensitive; needs the draw order proved unchanged.
4. **`_file_sha256` should stream, and each file should be hashed once.** `h.update(fh.read())`
   allocates the whole file; `_data_digest` and `_write_manifest` then each call it, so every data
   file is read and hashed twice. Chunked reads plus one hash reused for both is digest-neutral.
5. **`ma_cert_status` should carry the real validity window** instead of `[0.0, total_time]`
   (run.py:7633), and `gt_identity_map`'s forward cap (run.py:5373) should be reconsidered so that
   certificate expiry is exercisable. Digest-moving (both are data files).
6. **An attacker cannot outlive its trip** (run.py:5431). A persistent adversary — one that keeps a
   pseudonym across trips, or re-enters — has no representation, so no run length can study one.
   This is a scenario-model gap, not a bug.
7. **`_evidence_store` is never pruned** (run.py:6127; only populated when a report format carries
   `v2xPduEvidence`). It holds one encoded PDU per certificate digest for the whole run, so on the
   opt-in path it is an unbounded per-identity accumulator on top of the ones in §1.3.
8. **Zero-padding is lost past 10⁴/10⁵.** `rpt_{n:05d}`, `case_{n:04d}`, `crl_{n:04d}`. Ids stay
   unique, but `_write_outputs` sorts `gt_report_labels` by `report_id` as a **string**, so past
   99,999 reports (reached at ~5,500 s on this arm) the file's row order stops being chronological.
   Deterministic, so no digest hazard — just not what the sort intends.

**Not done here, and worth doing:**

* **The withheld-oracle spill is predicted, not observed** (§1.5). Attaching an isolated detector to
  a > 3 h run and confirming the spill, the seal and the unseal is one run and would close it. The
  8 h rung's withheld set is **1.11 GB**, comfortably past the 384 MiB ceiling, so the run to do it
  on already exists as a configuration.
* **Determinism is verified to 4 h, not 8** (§2). Repeating the 28800 s rung costs ~110 min, almost
  all of it in the quadratic finalisation of §1.2 — which is a good argument for fixing that first
  and then measuring this.
* **The by-line memory attribution did not finish.** `--attribute-memory` runs the engine in-process
  under `tracemalloc` and names the top allocators at peak; under this session's host load it had
  not produced a snapshot after 30 min and was abandoned. The attribution in §1.3 rests on the
  controlled A/B and the linear fit instead, which is stronger evidence of *what* grows but does not
  give the per-line breakdown.
* **Only one scenario.** Everything above is the 6×6 grid at 2 veh/s. `ref_grid_dense` (8 veh/s) and
  `ref_grid_rot` (`--rotate-period 60`) are wired into the probe and unrun; the rotation arm is the
  one that would show the finalisation term multiplied by ~5, and the dense arm is where survivorship
  and precision are already at their floor at 300 s.
* **No SUMO-replay arm in Part I.** InTAS replay bypasses the engine's arrival process entirely, so
  §0's demand finding does not apply to it and its cost curve has a different shape — **Part II
  measures exactly that**, and the two halves should be read together. In particular Part I holds
  density constant and finds run length nearly harmless; Part II holds duration constant and moves
  the time of day, and finds the cost envelope moving by two orders of magnitude.
* **`MAX_TIME_BUCKETS` / `HEADWAY_MAX_INSTANTS` = 240 have never been calibrated** — they are a cost
  ceiling, not a statistical one, and §3.3 is the first measurement of what they cost.

---
---

# Part II — the full day of the real city

*Sections 9–18. Part I above is the 6×6 synthetic grid held at constant arrival rate and stretched in
duration: the clean experiment, because the traffic is identical at every rung. Part II is the other
half of the question — the **InTAS day**, 86,400 s of Ingolstadt's own calibrated demand with 185,923
vehicle departures over the whole 65.96 km² network, where the traffic is deliberately NOT the same
at every rung and that is the point. §0 of Part I shows `--demand rush` cannot express a time-of-day
profile at all; SUMO replay can, because SUMO's departure times carry the real clock. Everything
below is measured on this host with `tools/day_run.py`.*

**Headline.** Three separate things, each measured, each invisible at 300 s.

1. **Every published cost number for the full-city engine is a measurement of midnight.**
   `FULL-CITY-SCENE.md` reports "104 ms/step and 261 MB at 333 replayed InTAS vehicles" from
   `--begin 0 --duration 300` — which is 00:00–00:05, and the emptiest five minutes of the day
   (measured mean concurrency over that window: **207.5**). At **2,339.7** concurrent — a 07:00
   window, and still 34% short of what 07:00 actually holds — the same engine costs **6.87 s/step
   and 2.79 GB**: **92× the per-step cost and 12.6× the memory.** Its PDR at 200 m is 20% better at
   midnight than at 09:00, and its revocation precision is 0.667 after two minutes at midnight
   against 0.219 after two minutes at the peak. "The whole city is the cheap option" is true of the
   night and of nothing else.
2. **A full 86,400 s day is not runnable as one process on this host, and the binding constraint is
   not CPU.** The frozen trajectory costs a measured **185–220 heap bytes per vehicle-step**, and the
   geometric channel's per-link state **grows for the whole run with no plateau observed** —
   +6.68 MB per simulated second at 1,807 concurrent, +21.5 MB/s at 2,340. Chunking is the answer and
   it costs something real: SCMS state does not cross a chunk boundary.
3. **The misbehaviour authority's trusted-reporter gate is a LIFETIME cap, not a rate limit, and it
   saturates.** `run.trusted()` requires `filed_by[cert] <= report_budget` (30) where `filed_by` is
   cumulative for the certificate's life and never decays. On the committed InTAS peak hour,
   **52.06% of reporter certificates cross it and 50.62% of all 627,411 reports are filed by stations
   the MA has already stopped counting**. On the committed 300 s dataset (dt = 0.1) it is 95.17% /
   99.40%, and **the MA revokes nothing at all after t = 60 s**: 27 revocations in the first minute
   and zero in the remaining four. Both datasets are in `datasets/`, and neither says so.

---

## 9. The instrument

`tools/day_run.py`. It is a different instrument from `tools/long_run_probe.py` because the question
is different: not "the same traffic for longer" but "what does this city cost, hour by hour, and what
does a day of it need".

| subcommand | what it does |
|---|---|
| `profile` | demand profile from a SUMO `--summary-output`: concurrency, network mean speed, halting share, per phase and per bucket |
| `freeze` | one window's trajectory artifact, with a warmup that fills the network first (parallelisable — SUMO is single-threaded and this host has 16 cores) |
| `window` | freeze + run + instrument one window, in a **child process** so its peak working set is its own |
| `ladder` | the cost envelope: the same engine config at a ladder of times of day, reported against **concurrent vehicles** |
| `day` | the chunked long run — resumable, index rewritten after every window |
| `volume` | output-volume accounting, per-stream, with a day projection whose basis is printed |
| `withheld` | when the 384 MiB isolated-detector budget runs out, in simulated seconds |
| `dynamics` | CRL growth, per-bucket revocation precision, and reporter saturation against simulated time |
| `verify` | re-derives a dataset's `data_digest` from its bytes — tells a graceful interrupt from a killed process |

Two implementation notes that are findings in their own right.

*Peak working set is a high-water mark that never falls*, so running eight windows in one interpreter
reports the eighth window's peak as the maximum of all eight. Every arm below is a fresh child.

*The manifest does not record how many steps a flow run actually took.* `config.n_steps` stays at its
default (40) whenever `duration_s > 0` — the loop count is a local in `run_pipeline` — and
`mobility.provider.n_rows` is the **trace's** row count, which counts steps an interrupted run never
reached. `day_run` counts `PER_STEP_HOOK` calls instead, which is the only reading that stays true
when the run is cut short. `counts.mobility_survivorship.vehicle_steps_simulated` is the one manifest
field that agrees with it.

**Host** (every number below): Windows Server 2022, 16 logical cores, 64 GB RAM, 813 GB free, SUMO
1.25.0, Python 3.12.10. Arms were taken with a full-day SUMO probe and a parallel workstream running
on the same box; a re-measurement of one arm on an idle machine has not been done, so read the
absolute milliseconds as ±10% and the *ratios* — which is what every conclusion here rests on — as
much tighter. **One control worth stating:** `run.py` was last modified at 20:00:00 local and the
first engine arm here started at 20:03, so every rung below ran against the same engine, including
the `_integrity.stream_closed()` change Part I §0 notes landing mid-ladder. The two independent
~1,800-concurrent rungs (§11) agreeing to 5% is the empirical check on that.

## 10. The demand profile is the point

### 10.1 What the day actually looks like

Eight windows, each frozen with a 900 s unrecorded warmup (SUMO discards every departure before
`--begin`, so a window asked for cold starts on an empty city) and 120 recorded steps at the
scenario's own calibrated 0.1 s integration, sampled at dt = 1.0. Concurrency, network mean speed and
halting share are SUMO's own, over the recorded window only.

| clock | phase | concurrent mean | min | max | network mean speed | halting share | **continuous day** |
|---|---|---:|---:|---:|---:|---:|---:|
| 00:00 | night | **206** | 55 | 269 | **14.10 m/s** | 0.091 | 197.8 @ 13.28 m/s |
| 04:00 | night | 308 | 291 | 327 | 10.43 m/s | 0.141 | 318.8 @ 10.19 m/s |
| 06:00 | AM ramp | 2,040 | 1,972 | 2,119 | 8.85 m/s | 0.246 | **2,622.0 @ 7.50 m/s** |
| 07:00 | AM peak | **2,340** | 2,235 | 2,437 | **8.64 m/s** | **0.248** | **3,919.4 @ 5.92 m/s** |
| 09:00 | inter-peak | 1,807 | 1,754 | 1,854 | 9.28 m/s | 0.221 | — |
| 12:00 | inter-peak | 1,796 | 1,724 | 1,865 | 9.29 m/s | 0.210 | — |
| 16:00 | PM peak | 2,213 | 2,129 | 2,297 | 8.79 m/s | 0.243 | — |
| 21:00 | evening | 921 | 900 | 949 | 9.87 m/s | 0.167 | — |

The last column is a **separate single continuous run of the whole day** — one `sumo` process from
t = 0 at the scenario's own 0.1 s step, summary every 10 s — and it is the reference the windowed
freezes are checked against in §10.2. It had reached 07:08 when this was written, where it measures
a peak of **4,061 concurrent vehicles**, and its own phase aggregates are:

| phase | clock | concurrent mean | max | network mean speed | halting share |
|---|---|---:|---:|---:|---:|
| night | 00:00–05:00 | 304 | 1,274 | 10.21 m/s | 0.198 |
| AM ramp | 05:00–07:00 | 2,521 | 3,885 | 7.60 m/s | 0.330 |
| AM peak | 07:00–07:08 | **3,984** | **4,061** | **5.59 m/s** | **0.426** |

**Concurrency spans 20× across the measured part of the day (197.8 → 4,061), network mean speed
falls 58% (13.28 → 5.59 m/s) and the halting share rises 3.6× (0.119 → 0.426).** There is no single
number for "how many vehicles the InTAS scenario has", and a 300 s sample of one phase is not the
city — it is one of at least three qualitatively different traffic states.

### 10.2 The 900 s warmup is enough at night and is NOT enough at the peak

A separate continuous run of the whole day (one `sumo` process, 0.1 s step, `--begin 0`, summary at
10 s) is the reference. Mean concurrency over the same 120 s window:

| clock | continuous day | 900 s-warmup window | ratio | continuous mean speed | windowed mean speed |
|---|---:|---:|---:|---:|---:|
| 00:00 | 197.8 | 206 | **1.04** | 13.28 m/s | 14.10 m/s |
| 04:00 | 318.8 | 308 | **0.97** | 10.19 m/s | 10.43 m/s |
| 06:00 | 2,622.0 | 2,040.6 | **0.78** | 7.50 m/s | 8.85 m/s |
| 07:00 | **3,919.4** | 2,339.7 | **0.60** | **5.92 m/s** | **8.64 m/s** |

At night the windowed freeze agrees with the continuous day to **3–4%**. By 06:00 it is 22% short,
and at the AM peak it is **40% short on concurrency and 46% fast on network speed** (8.64 against
5.92 m/s), with a halting share of 0.248 against 0.387. The full ladder at 07:00, with the committed
peak-hour trace as a third point:

| warmup before the recorded window | concurrency at 07:00 | fraction of the continuous day |
|---|---:|---:|
| 900 s (this ladder) | 2,340 | **0.60** |
| 3,600 s (`intas_hour.trace`, committed) | 3,552 | **0.91** |
| the whole night (continuous run from t = 0) | **3,919** | 1.00 |

Even an hour of warmup is 9% short. Mean trip duration at the peak is ~560 s for a vehicle that is
never revoked, so a warmup buys trip generations, and the AM queue is longer than several of them.
The 1,800 s peak window in §12.6 shows the same thing from the inside: it opens at 2,236 and is still
climbing at 3,264 when it ends.

**A warmup is not a detail of the harness, it is part of the scenario**, and the direction of the
error is the dangerous one: an under-filled window looks *faster and freer* than the city is. Every
high-concurrency arm in this document is therefore labelled by its **measured** concurrency and never
by its clock time, and all of them are **lower bounds** on their clock time's traffic.

**Consequence, and it is a constraint on this document.** Every high-concurrency arm below is
labelled by its **measured** concurrency, never by its clock time, and the concurrencies reached
(2,340 max) are **lower bounds on the true peak** (3,552). The cost at the true peak is therefore
read off the fitted curve in §11 and labelled as such.

### 10.3 The radio is a different radio at 09:00 than at 00:00

`datagen.realism_bench`'s comm panel, unmodified, on the geometric arms (the full 21,717-footprint
scene, 500 m range, `radio_cap_max_mult 1.4`):

| window | concurrent | PDR @ 200 m | awareness @ 200 m | **LOS fraction of co-present pairs** | effective range | NAR-0.90 range | speed p50 | headway p50 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 00:00 | 207.5 | **0.6032** | 0.6529 | **0.3197** | 258.5 m | **327.5 m** | 12.162 m/s | 2.563 s |
| 04:00 | 307.7 | 0.5787 | 0.5327 | 0.2033 | 230.2 m | 314.4 m | 11.996 m/s | 2.635 s |
| 09:00 | 1,806.7 | **0.4836** | 0.5757 | **0.1235** | 233.5 m | **259.0 m** | 10.438 m/s | 3.625 s |

Per-packet PDR at 200 m falls **19.8%** and the NAR-0.90-equivalent range falls **20.9%** between
midnight and the daytime plateau, and the explanatory variable is in the table: the LOS fraction of
the co-present pair population collapses from 0.3197 to **0.1235**, because at 1,807 concurrent there
is a vehicle in the way.

**`FULL-CITY-SCENE.md`'s headline 0.6026 PDR at 200 m is the midnight number.** The 00:00 row above is
an independent re-measurement of it on a different window length (120 s against 300 s) and a
differently frozen trace, and it reads **0.6032** — 0.1% apart, which is what says the two are the
same quantity. It is not wrong; it is the 00:00–00:05 window it was taken on. This is the first
measurement of that quantity at any other time of day, and the answer is that the propagation
environment the flagship number describes is **19.8% better in PDR and 20.9% longer in awareness
range** than the same city at 09:00.

## 11. The cost envelope, against concurrency

Same engine config at every rung; only the window moves. `emit_sample_prob = 1.0` and
`emit_mobility_oracle = true` throughout (the maximal-information dataset configuration — §12.4 has
what the sampling knob is worth). `ms/step (loop)` excludes scene import, footprint load, trace load
and the final digest pass; `ms/step (all)` is what a user waits.

**Geometric radio** — the flagship: `radio_model geometric`, `radio_env urban`, range 500 m,
`radio_cap_max_mult 1.4` (a 700 m candidate window), 23 dBm / −81 dBm, InTAS's own 21,717 footprints.

| concurrent | ms/step (loop) | ms/step (all) | setup s | peak RSS | MB / simulated s | µs / vehicle-step |
|---:|---:|---:|---:|---:|---:|---:|
| 207.5 | **74.6** | 100.2 | 2.94 | 221 MB | 0.122 | 359 |
| 307.7 | 141.0 | 167.5 | 2.99 | 240 MB | 0.174 | 458 |
| 1,795.7 | 4,303.4 | 4,348.7 | 3.78 | 1,852 MB | 1.259 | 2,396 |
| 1,806.7 | **4,096.4** | 4,154.1 | 4.25 | 1,852 MB | 1.289 | 2,267 |
| 2,040.6 | 5,152.3 | 5,198.9 | 3.97 | 2,251 MB | 1.450 | 2,525 |
| 2,339.7 | **6,871.5** | 6,921.1 | 3.94 | **2,790 MB** | 1.700 | 2,937 |

(The two ~1,800-concurrent rungs are different windows of the day — 12:00 and 09:00 — run in
different processes hours apart. They agree to **5%** on per-step cost and to three digits on peak
RSS, which is the closest thing to a repeatability estimate this document has.)

**Disc radio** — the cheap option, and the one the committed `datasets/py_intas_hour` used:
`radio_model disc`, range 709.4 m, `radio_cap_max_mult 6.0`.

| concurrent | ms/step (loop) | ms/step (all) | setup s | peak RSS | MB / simulated s | µs / vehicle-step |
|---:|---:|---:|---:|---:|---:|---:|
| 207.5 | **39.5** | 64.5 | 2.70 | 206 MB | 0.128 | 190 |
| 1,795.7 | 1,038.9 | 1,082.5 | 3.50 | 569 MB | 1.217 | 579 |
| 2,339.7 | **1,533.2** | 1,597.3 | 3.75 | **760 MB** | 1.532 | 655 |

Endpoint fits over the measured range 207.5 → 2,339.7, stated as fits and not as laws:

* geometric: cost ≈ **N^1.87** per step (74.6 → 6,871.5 ms for an 11.28× density change); the LOCAL
  exponent over the four daytime rungs alone (1,796 → 2,340) is **N^1.77**, so the power law is not
  an artefact of anchoring on the night rung
* disc: cost ≈ **N^1.51**

The **exponent difference is the whole story**: the geometric premium is 1.89× at 207 concurrent and
**4.48× at 2,340**. The building-aware channel is not a constant surcharge; it is a surcharge that
grows with the number of in-range pairs, because each one is classified LOS / NLOSv / NLOSb and
ray-cast against the footprint raster.

### 11.1 The projection, its basis and its uncertainty

The whole day's cost is **not measured here.** This is a projection and its basis is stated so it can
be checked or replaced:

1. **Concurrency, second by second**, taken from the continuous full-day SUMO probe wherever it has
   reached (measured, 10 s samples, t ≤ 25,680 s) and beyond that from the §10.1 window probes
   interpolated in time and multiplied by the measured continuous ÷ windowed ratio. That ratio is
   **1.285** at 06:00 and **1.675** at 07:00 (§10.2), and it is applied uniformly, so the two give
   a **low and a high bound** on the unmeasured part of the day rather than one answer.
2. **Per-step engine cost** from the endpoint power-law fits above, evaluated at that concurrency.
3. Integrated over 86,400 s at dt = 1.0.

| quantity | **low** (1.285 correction) | **high** (1.675 correction) | basis |
|---|---:|---:|---|
| day vehicle-steps | 147.9 M | **187.1 M** | measured concurrency × 86,400 s |
| day-mean concurrency | 1,711 | 2,165 | — |
| engine wall clock, **disc**, single core | 26.1 h | **37.5 h** | fit N^1.510 |
| engine wall clock, **geometric**, single core | 116.5 h | **183.2 h** | fit N^1.867 |
| frozen trace, as a file | 6.5 GB | 8.2 GB | measured 43.7 file-bytes/row |
| frozen trace, **resident** | 29.6 GB | **37.4 GB** | measured ~200 heap-bytes/row |
| dataset, disc, `emit 1.0` + oracle | 100 GB | 127 GB | measured 677.5 B/vehicle-step |
| dataset, disc, `emit 0.03` + oracle | 64 GB | 81 GB | measured 430.5 B/vehicle-step |
| dataset, `emit 0.03`, no oracle | 14 GB | 18 GB | measured 95.2 B/vehicle-step |

Cross-check on a quantity that did not enter the calculation: the two bounds imply 795 s and 1,006 s
of mean trip duration over 185,923 departures, against the 694–711 s `meanTravelTime` SUMO itself
reports in the AM ramp (rising through the peak) and the 643 s mean presence of the 1,800 s peak
window. Both bounds are consistent with that; the low one is the better match to the pre-peak hours
and the high one to the peak.

**Uncertainty, honestly.** The cost fit spans 207.5 → 2,339.7 concurrent and the day's measured peak
is already **4,061** (§10.1), well outside it — at N^1.51 that is 2.3× the top measured disc rung and
at N^1.87 it is 2.9× the geometric one, and neither is measured. The fill correction is two data
points, taken where the under-fill is worst, and applying either uniformly is wrong in a known
direction for the phases where 900 s is nearly enough. Memory, not time, is what actually stops a
geometric day (§12.2), and the disc projection assumes the memory plateau in §12.2 holds for 24 h,
which has been observed for 1,800 s and no longer. Read the table as **one significant figure**: a
day is *one to two days* on disc and *about a week* on geometric, single-threaded, on this host.

**What makes a day feasible.** Windows are independent, so the day parallelises trivially: 26–38 h of
single-core engine time across this host's 16 cores is **2–3 hours of wall clock** for 48 half-hour
windows, plus the SUMO freezes (which parallelise the same way — SUMO is single-threaded here by
construction, `--threads 1`, so eight concurrent freezes ran at full speed in §16). That is the
practical answer, and it is why `day_run day` writes one dataset per window rather than one dataset —
the shape that makes the day feasible is the same shape that makes it resumable.

### 11.2 The recommended configuration, run once, at the peak

The whole of §15's recommendation as one arm, so it is a measurement rather than an assembly of
other arms: **600 s of the AM peak** (the 1,800 s peak trace, first 600 steps), disc radio,
`emit_sample_prob 0.03`, `emit_mobility_oracle` on, dt = 1.0, full city with footprints.

| | |
|---|---|
| concurrency | **2,606** mean (the window opens at 2,236 and is still filling) |
| vehicle-steps simulated | 1,563,850 |
| wall clock | **538.8 s** — 12.7 s setup (trace load of 5.3 M rows), 508.9 s loop, 17.2 s finalise |
| per-step cost | **849.6 ms** loop, 897.9 ms all-in — **0.90× realtime**, i.e. this window runs slower than the city it replays |
| peak RSS | 2,901 MB, and the working set is **flat at 2,549 → 2,576 MB from step 50 to step 575** |
| output | **498.6 MB** (318.8 B/vehicle-step) |
| vehicles / revoked | 4,361 / 1,808 = **41.5%** revoked in ten minutes |
| survivorship | **0.6569** — a third of the vehicle-steps are already missing from the broadcast record |
| digest | 11/11 outputs verified, `data_digest` re-derives |

Two things are worth taking from this arm on their own. **The disc radio's memory is bounded and the
bound was reached in 50 steps and held for 525 more** at 2,606 concurrent — which is the measurement
§12.2 needs to make its contrast with the geometric channel a contrast rather than an artefact of
density. And **the survivorship loss at ten minutes and peak density is already 34%**, on the way to
the 56% the full peak hour reaches — this run is a rung between the 120 s windows and the committed
hour, and it sits exactly where those two predict.

## 12. What actually breaks, and at what length

### 12.1 A monolithic day-long trace cannot be loaded, and the number is measured

`sumo_trace.load()` parses the artifact in one pass into four Python lists of floats per vehicle.
Measured in a fresh process, three traces:

| trace | vehicle-steps | file bytes/row | **heap bytes/row** | load rate |
|---|---:|---:|---:|---:|
| 00:00, 120 steps | 24,903 | 43.6 | **184.5** | 1.16 M rows/s |
| 04:00, 120 steps | 36,920 | 43.7 | **198.6** | 1.16 M rows/s |
| 21:00, 120 steps | 110,455 | 43.8 | **219.9** | 0.89 M rows/s |

The committed peak-hour trace is 13,589,568 rows — a 594 MB file and **~2.6 GB resident**, for ONE
hour at one phase. The InTAS day is **148–187 M vehicle-steps** (§11.1), which is a **6.5–8.2 GB text
artifact and 30–37 GB resident** before the engine allocates anything of its own. Freezing it is
worse than loading it: `sumo_trace._write` materialises every row as a Python tuple in one list and
sorts it, so the freeze needs more memory than the run does.

There is no seek index and no windowed reader, so this is not tunable — it is why `day_run day`
chunks. **What chunking costs is real and is recorded in `day_index.json` as
`scms_state_continuous: false`:** pseudonym rotation restarts at every boundary, the CRL is empty
again, and the MA's reputation table and its accumulated false positives are gone. A chunked day is a
valid measurement of traffic, radio and per-window detection across the demand profile. It is **not**
a measurement of CRL growth or MA false-positive accumulation across a day.

Chunking also repeats the SUMO fill: measured on three consecutive 120 s windows with a 300 s warmup,
the freeze cost 5.36 s, 11.10 s, 18.46 s — each window re-simulates its own warmup.

### 12.2 The geometric channel's per-link state never plateaus

`day_run` samples working set through `run.PER_STEP_HOOK`. Two runs, same engine, different channel:

| | disc, 1,795.7 conc, 120 s | **disc, 2,606 conc, 600 s** | geometric, 1,806.7 conc, 120 s | geometric, 485 conc, **1,800 s** |
|---|---|---|---|---|
| RSS at step 0 | 246.9 MB | 2,103.1 MB | 255.6 MB | 509.5 MB |
| RSS at the end | 508.4 MB | 2,575.6 MB | 1,730.8 MB | 1,039.6 MB |
| behaviour | **flat from step 50** (501.0, 501.0, 501.0, 504.1, 508.4, 508.4) | **flat from step 50** — 2,549 → 2,576 MB over 525 steps, ±0.5% | rising, **+6.68 MB per simulated second**, no plateau | rising, **+0.29 MB/s**, no plateau in 30 min |

At 2,339.7 concurrent the geometric slope is **+21.5 MB per simulated second** (271.6 MB at step 0 →
2,633.8 MB at step 110) and the peak working set is 2.79 GB for a **two-minute** run.

The mechanism is `GeometricChannel._shadow` and `._packet` (run.py:763), keyed by vehicle pair, each
value holding a `random.Random` — and `prune()` (run.py:831) drops a pair only when **both** endpoints
have despawned. In a stable fleet almost no pair ever qualifies, so the store accumulates every pair
that has ever come within the candidate window. That is O(N²) with a several-kilobyte constant.

Linear extrapolation of the measured 1,807-concurrent slope to one hour is ~24 GB and to a day ~577
GB; the true curve must saturate when every pair has been seen, so those figures are an upper bound
on the slope, not a prediction — but nothing observed here saturates, at any of the three densities,
over any of the durations run. **The geometric radio is usable for windows of minutes and is not
usable, unchunked, for hours.** The disc radio is bounded and was measured to be.

### 12.3 The MA's trusted-reporter cap is a lifetime cap, and it saturates

`run.trusted()` (run.py:5968) gates a reporter on
`filed_by[cert] <= report_budget` (30) and `received_by[cert] < reputation_max` (40). Both dicts are
incremented in `file_report` and **never decayed and never windowed**. They are named rate limits and
they are cumulative caps. A report from an over-cap station is still written to
`ma/ma_reports.jsonl` and still labelled in `gt_report_labels.jsonl` — it simply stops counting
toward `report_threshold_k`, silently.

`day_run dynamics`, on datasets that already exist in this repository:

| dataset | dt | concurrent | length | reporter certs **over budget** | **reports from over-budget certs** |
|---|---:|---:|---:|---:|---:|
| `ds_night1800_geo` (this work) | 1.0 | 485 | 1,800 s | 108 / 1,470 = **7.35%** | 1,789 / 19,436 = **9.20%** |
| `datasets/py_intas_hour` (committed) | 1.0 | 3,775 | 3,600 s | 7,165 / 13,762 = **52.06%** | 317,598 / 627,411 = **50.62%** |
| `datasets/py_intas_300s` (committed) | **0.1** | ~331 | 300 s | 315 / 331 = **95.17%** | 1,586,090 / 1,595,741 = **99.40%** |

What exhausts the cap is *reports per second × certificate lifetime*, and both terms grow — with
density, and with `1/dt`. The consequences are measured, not inferred:

`datasets/py_intas_300s`, per 60 s bucket — **the MA stops working after the first minute**:

| t₀ | reports | distinct reporters | from over-budget | revocations | precision |
|---:|---:|---:|---:|---:|---:|
| 0 | 93,776 | 220 | 87,665 | **27** | 0.259 |
| 60 | 314,865 | 242 | 313,316 | **0** | — |
| 120 | 363,649 | 261 | 362,888 | **0** | — |
| 180 | 376,998 | 283 | 376,362 | **0** | — |
| 240 | 446,453 | 293 | 445,859 | **0** | — |

1.5 million reports were written; 1.59 million of the 1.60 million came from stations the MA had
already stopped counting; four fifths of the run produced no revocation at all. At dt = 0.1 a station
reaches 30 filed reports in about three simulated seconds.

`datasets/py_intas_hour`, per 300 s bucket — flow-mode churn keeps it alive but degraded:

| t₀ | reports | distinct reporters | from over-budget | revocations | true positives | precision | CRL |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 105,797 | 4,357 | 16,508 | 2,127 | 835 | **0.393** | 2,127 |
| 600 | 50,824 | 2,437 | 28,878 | 693 | 190 | 0.274 | 3,593 |
| 1,800 | 46,920 | 2,325 | 27,429 | 649 | 195 | 0.300 | 6,214 |
| 3,300 | 46,291 | 2,311 | 25,978 | 634 | 170 | **0.268** | 9,143 |

Revocations per 300 s fall 3.4× after the first bucket and precision falls from 0.393 to ~0.27, while
**55–65% of steady-state report traffic is from over-budget reporters**. In flow mode the trusted
pool is replenished only by vehicle turnover: a fresh trip is a fresh certificate and a fresh budget.
A fixed fleet, a long trip (InTAS's buses run for hours) or a small `dt` removes that replenishment,
and the 300 s dataset above is what that looks like.

**And at peak density it does not take an hour — it takes ninety seconds.** The same instrument on
two 120 s geometric windows from this ladder, bucketed at 30 s:

| | 00:00, **207.5** concurrent | | | 07:00, **2,339.7** concurrent | | |
|---|---:|---:|---:|---:|---:|---:|
| bucket | reports | over-budget | **precision** | reports | over-budget | **precision** |
| 0–30 s | 306 | 0 | 0.923 | 37,283 | 3,051 | **0.912** |
| 30–60 s | 223 | 0 | 0.889 | 5,790 | 2,026 | 0.554 |
| 60–90 s | 496 | 128 | 0.875 | 4,353 | 1,547 | 0.379 |
| 90–120 s | 487 | 226 | 0.667 | 3,405 | 1,135 | **0.219** |
| certs over budget, whole window | 3 / 189 = **1.6%** | | | 562 / 2,413 = **23.3%** | | |

Two minutes at the AM peak takes revocation precision from 0.912 to **0.219**; two minutes at
midnight leaves it at 0.667. A 300 s validation window at 00:00 — which is what every existing
full-city measurement is — sees the top-left corner of this table and nothing else.

Ten minutes of it, from the §11.2 arm at 2,606 concurrent, per 60 s:

| t₀ | 0 | 60 | 120 | 180 | 240 | 300 | 360 | 420 | 480 | 540 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| revocations | 478 | 200 | 170 | 150 | 146 | 119 | 146 | 134 | 128 | 137 |
| **precision** | **0.707** | **0.145** | 0.165 | 0.180 | 0.212 | 0.193 | 0.233 | 0.239 | 0.281 | 0.299 |
| reports from over-budget certs | 1,270 | 1,286 | 1,569 | 1,962 | 2,857 | 2,543 | 3,355 | 4,015 | 3,663 | 4,262 |
| CRL entries | 478 | 678 | 848 | 998 | 1,144 | 1,263 | 1,409 | 1,543 | 1,671 | 1,808 |

Precision drops **4.9×** between the first minute and the second and never recovers past 0.30; the
share of report traffic coming from already-distrusted stations rises monotonically; and the CRL
grows to 1,808 entries — **41.5% of the 4,361 vehicles that existed** — in ten simulated minutes. A
harness that measured only the first bucket would report a precision of 0.707 for this scenario.

This is not a claim that the gate is wrong — a rate limit on reporters is exactly right. It is a
claim that **it is not a rate limit**, that its bindingness is a function of run length, density and
`dt`, and that two committed datasets are affected and say nothing about it.

### 12.4 Output volume, per stream, and the two knobs that move it

Measured, total dataset bytes per **simulated** vehicle-step:

| arm | radio | emit_sample_prob | oracle | B / vehicle-step | MB / simulated s |
|---|---|---:|---|---:|---:|
| 00:00, 207.5 conc | geometric | **0.03** | off | **95.2** | 0.020 |
| 00:00, 207.5 conc | geometric | 1.0 | on | 586.5 | 0.122 |
| 09:00, 1,806.7 conc | geometric | 1.0 | on | 713.7 | 1.289 |
| 07:00, 2,339.7 conc | geometric | 1.0 | on | 726.5 | 1.700 |
| 12:00, 1,795.7 conc | disc | **0.03** | on | 430.5 | 0.773 |
| 12:00, 1,795.7 conc | disc | 1.0 | on | 677.5 | 1.217 |
| `datasets/py_intas_hour` | disc | 1.0 | off | 197.4 | 0.745 |

Per stream at 1,795.7 concurrent, disc, `emit_sample_prob = 0.03` + oracle:

| stream | B / vehicle-step | scales with |
|---|---:|---|
| `ground_truth/gt_mobility_oracle.jsonl` | **219.9** | vehicle-steps |
| `ma/ma_reports.jsonl` | **175.1** | vehicle-steps |
| `ground_truth/gt_report_labels.jsonl` | 21.4 | vehicle-steps |
| `ground_truth/gt_emissions_sample.jsonl` | 7.5 | vehicle-steps |
| everything else (7 files) | 6.5 total | **vehicles** |

Two things follow. **`emit_sample_prob` is worth 6.2× on the whole dataset** (95.2 → 586.5 B per
vehicle-step at the same density, and 41× on the emission stream alone), so it is the first knob to
reach for. And once emissions are sampled down, **the un-enforced oracle record is the largest file in
the dataset** — which is the price of the `TRAFFIC-PANEL-SURVIVORSHIP.md` fix, and worth paying,
because the alternative is a traffic panel that measures enforcement.

Scaled to the day's 148–187 M vehicle-steps (§11.1) that is **100–127 GB** at full emission sampling,
**64–81 GB** at `emit 0.03` with the oracle on, and **14–18 GB** at `emit 0.03` with it off.

`day_run volume` does the projection **per stream**, because the streams do not scale the same way —
four grow with vehicle-steps and seven with departures — and scaling the whole directory by one
factor overstates a day by whichever term the measured window happened to be dominated by. On the
§11.2 arm (the recommended configuration, at the peak):

```
PROJECTION -- per-vehicle-step streams x 119.6 (187,066,702 day vehicle-steps / 1,563,850 measured)
              per-vehicle streams      x  42.6 (185,923 day departures / 4,361 measured)
    gt_mobility_oracle.jsonl     41.30 GB      gt_emissions_sample.jsonl   1.16 GB
    ma_reports.jsonl             14.93 GB      the other 8 files           0.14 GB
    gt_report_labels.jsonl        1.85 GB      WHOLE DAY                  59.38 GB
```

**59.4 GB for the recommended day**, of which the un-enforced oracle record is 70% and `ma_reports`
is 25%. That number is taken at the peak, where both the report rate and the density are at their
highest, so it is an over-estimate of the day's average — the honest reading is "under 60 GB". Disk
is not the binding constraint on this host (813 GB free); **usability** is. A 15 GB
`ma_reports.jsonl` is a different object from 48 files of 310 MB, and that is another reason chunking
is the right shape.

### 12.5 Survivorship grows with density as well as with duration — and the fix holds

`TRAFFIC-PANEL-SURVIVORSHIP.md` establishes that `gt_emissions_sample.jsonl` loses vehicle-steps at
revocation and that the loss grows with run length. On the InTAS day it grows with **density** too,
and at a comparable rate — which matters because a chunked day holds duration constant and varies
density by 11×.

| window | radio | concurrent | length | **survival fraction** | revoked fraction |
|---|---|---:|---:|---:|---:|
| 00:00 | geometric | 207.5 | 120 s | **0.9025** | 0.133 |
| 04:00 | geometric | 307.7 | 120 s | 0.9056 | 0.113 |
| 09:00 | geometric | 1,806.7 | 120 s | 0.8654 | 0.170 |
| 12:00 | disc | 1,795.7 | 120 s | 0.8217 | 0.232 |
| 07:00 | disc | 2,339.7 | 120 s | **0.8156** | 0.254 |
| 04:00–04:30 | geometric | 485 | **1,800 s** | 0.8208 | 0.198 |
| 07:00–07:10 | disc | 2,606 | **600 s** | **0.6569** | 0.415 |
| `py_intas_hour` | disc | 3,775 | **3,600 s** | **0.4378** | 0.614 |

At fixed 120 s the survival fraction falls 0.9025 → 0.8156 as concurrency rises 11×, and at fixed
low density it falls 0.9056 → 0.8208 as duration rises 15×. Both terms are real and they compound —
the 600 s peak window at 0.6569 sits exactly between the 120 s peak windows and the committed hour,
and the hour is the product of both terms and keeps 43.78%.

**The fix holds at length.** `emit_mobility_oracle` writes the un-enforced record, and on every arm
here `counts.mobility_survivorship.oracle_rows` equals `vehicle_steps_simulated` **exactly** —
872,920 of 872,920 on the 1,800 s run, 215,486 of 215,486 on the 120 s peak-density one. The oracle
stream is complete at every duration and density measured, which is what licenses reading the traffic
panel off it (`realism_bench` reports `Traffic panel read oracle, survivorship 1.0000`).

### 12.6 SUMO's own cost, and the crash that is already documented

Freezing is not free and on a day it is comparable to running. Measured:

| freeze | SUMO-seconds simulated | wall | rate |
|---|---:|---:|---:|
| the first 1,800 s of the day, cold `sumo`, no recording | 1,800 | **58.4 s** | 31× realtime |
| 120 s window at 00:00, no warmup (`day_run freeze`) | 120 | 4.5 s | — |
| 1,800 s window at 07:00 + 900 s warmup, recorded | 2,700 | **1,595.9 s** | **1.69× realtime** |

The last row is the one that matters: at AM-peak density, freezing costs **0.59 s of wall per
simulated second**, so the 48 freezes of a chunked day are of the same order as the engine runs
themselves. That freeze produced 8,243 trajectories and 5,302,895 vehicle-steps in a 241 MB artifact
(45.5 file-bytes/row) with 58 teleports and 30 gap splits. Each `day_run` window also pays a fresh
scenario load (~25 s, 22 route files) on top.

Its concurrency also settles the §10.2 warmup question from the other side: the window opens at
**2,236** and closes at **3,264** — still climbing after 1,800 s of recording, toward the 3,552 the
3,600 s-warmup trace starts at. The AM peak takes more than an hour to fill.

`sumo_trace.freeze`'s own docstring records the trap that matters most for a long freeze, and it is
not ours to fix: on InTAS from 21,600 s, SUMO 1.25.0 dies with a Windows ACCESS_VIOLATION
(0xC0000005) at sim time 23,544.1 under `--time-to-teleport -1` and at 23,904.7 under `300`, at the
same seed — and the same window runs clean at a different SUMO seed. A day-long single freeze walks
straight into that; 48 half-hour freezes lose one window and can re-seed it.

## 13. Interruption: what survives, and what does not

Both arms are a real signal to a real child process running a 120 s window at 1,795.7 concurrent, not
a test seam.

| | **Ctrl-Break** (SIGBREAK) | **Ctrl-C** (SIGINT) |
|---|---|---|
| child exit | `3221225786` = 0xC000013A `STATUS_CONTROL_C_EXIT` | **0** |
| engine output | — | `[interrupted at step 17 -> finalizing partial dataset]` |
| files left | 3 streamed `.jsonl`, **no `manifest.json`** | 11 declared outputs + manifest |
| `day_run verify` | **INVALID** — "this is a killed run's leftovers, not a dataset" | **VALID**, 11/11 outputs verified, `data_digest` re-derived **MATCH** |

The engine's graceful path works and produces a real dataset for the steps it ran — including the
survivorship block, which correctly reports **29,419** simulated vehicle-steps rather than the
trace's 215,486, and a `data_digest` that re-derives from the bytes on disk. Two practical notes.
Windows maps Ctrl-Break to SIGBREAK and the engine installs its handler on **SIGINT only**, so
Ctrl-Break is a hard kill; and a child created with `CREATE_NEW_PROCESS_GROUP` inherits Ctrl-C
*disabled*, so a harness that wants to test this must re-enable it
(`SetConsoleCtrlHandler(NULL, FALSE)`) or it will measure nothing — the first attempt here used
Ctrl-Break and measured the hard-kill column while believing it was measuring the other one.

This is not a hypothetical failure mode; it happened to this document. A 1,800 s AM-peak window had
its process tree killed 20 minutes in by the harness running it, and left **1,532,325,425 bytes
across four `.jsonl` files and no manifest** — `day_run verify` calls it what it is:
`INVALID ... this is a killed run's leftovers, not a dataset (4 files present)`. 1.5 GB of correct
rows, unusable, because nothing on disk says how much of the run they cover, what config produced
them, or which four of the eleven expected files these are.

`day_run day` adds the other half: the chunk index is rewritten after every window and a re-run skips
every window whose dataset is already complete (measured: `[skip w0000_t00000] already complete` ×3,
0 s of re-simulation). On a day-long job that is the difference between losing a window and losing
the day — and the reason the recommended shape in §15 is 48 windows rather than one run.

## 14. Isolated detectors at length

`run.WITHHELD_MEMORY_BYTES` = 402,653,184 (384 MiB) is a **shared** budget across
`gt_report_labels` + `gt_emissions_sample` + `gt_mobility_oracle`; past it the overflow spills
XOR-sealed to `<out>/.withheld/*.sealed`. `day_run withheld` solves the measured per-stream rate for
the run length at which the budget is exhausted:

| operating point | withheld rate | **budget exhausted at** |
|---|---:|---:|
| 207.5 concurrent, `emit 0.03`, no oracle | 3.51 kB/s | **114,739 s** (31.9 h) |
| 1,795.7 concurrent, `emit 0.03`, **oracle on** | 446.9 kB/s | **901 s** |
| 2,606 concurrent, `emit 0.03`, oracle on (§11.2) | 617.3 kB/s | **652 s** |
| 3,775 concurrent, `emit 1.0`, no oracle (`py_intas_hour`) | 539.5 kB/s | **746 s** |
| 1,806.7 concurrent, `emit 1.0`, oracle on | 924.1 kB/s | **436 s** |
| 2,339.7 concurrent, `emit 1.0`, oracle on | 1,192.6 kB/s | **338 s** |

So the answer to "are isolated detectors viable at day length" is **only at low emission sampling and
with the oracle record off**. At night density with `emit_sample_prob 0.03` and no oracle the budget
outlasts a day by 33%. At AM-peak density the same settings give **~9,600 s** of headroom
(recombining the §11.2 arm's measured per-stream rates with the oracle stream removed: 41.9 kB/s —
arithmetic on measurements, not a new measurement), which is five 1,800 s windows and nothing like a
day. Turn the oracle on and it is 652 s — **one third of a single recommended window**. At full
emission sampling the budget is gone in **under six minutes** and a day would spill ~100 GB sealed to
disk and unseal it at the end.

**The practical rule this gives:** an isolated third-party detector is a per-window instrument, not a
per-day one, and the window it can cover without spilling is `384 MiB ÷ (measured withheld kB/s)` —
which `day_run withheld` prints for any finished dataset. The spill is not a failure (it is sealed,
unsealed on commit, and the digest is unchanged) but it turns a memory-resident buffer into GB of
disk write and read, which on a day is 50–100 GB of each.

Note that `ma/ma_reports.jsonl` is deliberately **not** withheld (it is MA-visible), and §12.4 shows
it is 175 B/vehicle-step — so an isolated detector on a long run has an ever-growing readable file
beside it regardless. That is the documented trade in `api/isolate.py`, restated here because at day
length it is 27 GB.

## 15. The recommended configuration

For someone who wants a realistic long run of the real city on a host like this one:

| knob | recommendation | why, in one measured number |
|---|---|---|
| **duration** | the whole 86,400 s day, **chunked into 1,800 s windows** | a monolithic day trace is 18–22 GB resident before the engine starts (§12.1) |
| **window overlap** | 900 s warmup per window, and **verify the fill** against a continuous `profile` run | 900 s is within 4% at night and 1.52× short at the peak (§10.2) |
| **step size** | `dt = 1.0` with `--substeps 10` | InTAS is calibrated at 0.1 s; re-integrating at 1 s is a different model, and `dt = 0.1` in the engine exhausts `report_budget` in ~3 simulated seconds (§12.3) |
| **radio** | **disc for the day, geometric for chosen windows** | geometric memory grows +21.5 MB per simulated second at 2,340 concurrent with no plateau; disc is flat by step 50 (§12.2). Geometric costs 4.48× disc at that density |
| **emission sampling** | `emit_sample_prob 0.03`, and **`emit_mobility_oracle` ON** | 6.2× less output; the oracle is what makes the traffic panel a traffic panel (survivorship 0.4378 at the peak hour without it) |
| **isolated detectors** | viable per-window, **not** across a day; drop the oracle if one is attached | 384 MiB budget lasts 652 s at 2,606 concurrent with the oracle on, ~9,600 s without (§14) |
| **interruption** | Ctrl-C, once; never Ctrl-Break | SIGINT finalises a valid dataset, SIGBREAK leaves unusable fragments (§13) |
| **parallelism** | run windows across cores | 48 windows × ~35 min ÷ 16 workers ≈ 3 h for a disc-radio day |

**Measured, on one window of exactly this configuration** (§11.2): 600 s of the AM peak at 2,606
concurrent costs **539 s of wall clock, 2.9 GB of peak working set and 499 MB of output**, with the
working set flat from step 50. Scaled to a 1,800 s window that is ~27 min and ~1.5 GB, which is the
unit the day is built from.

**What the whole day costs**: on this host, a disc-radio 86,400 s day at `emit 0.03` + oracle in 48
half-hour chunks is a projected **26–38 h of single-core engine time** (2–3 h across 16 workers) plus the SUMO
freezes — 48 × (scenario load + 900 s warmup + 1,800 s window), which at the measured 0.59 s of wall
per simulated second is of the same order again — producing **under 60 GB** across 48 window datasets
(the per-stream projection off the §11.2 arm, §12.4) plus 6.5–8.2 GB of intermediate traces that
`--drop-traces` deletes as it goes. Drop the oracle and it is ~18 GB, at the cost of the traffic
panel. With the geometric radio the engine time is 117–183 h and the memory ceiling, not the clock,
is what stops it.

## 16. Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1        # SUMO 1.25.0
cd C:\Users\Administrator\Documents\SCMS-Simulator
$env:PYTHONPATH = "$PWD\src"
$S = "scms-sim/scenarios/gen_intas_urban_low/sumo"
$N = "$S/ingolstadt.net.xml"; $C = "$S/InTAS_buildings.sumocfg"; $B = "$S/buildings.poly.xml"

# the demand profile. NOTE the four explicit output overrides: the sumocfg names its outputs
# RELATIVE TO ITSELF, so without them this overwrites the scenario's committed measurement files
# in place. `--output-prefix` is SUMO's usual answer and does not work with an absolute path.
sumo -c $C --begin 0 --end 86400 --seed 42 --threads 1 --no-step-log true --no-warnings true `
     --summary-output day.summary.xml --summary-output.period 10 `
     --tripinfo-output day.tripinfo.xml --statistic-output day.stat.xml --log day.log `
     --additional-files "BusStations.add.xml,InTAS_E1.add.xml"
python tools/day_run.py profile --summary day.summary.xml --bucket 1800 --buckets

# the cost envelope. Freeze first, in parallel, one process per core; `ladder` then reuses the
# artifacts it finds (the whole ladder in one call would freeze them one at a time).
foreach ($t in 0,14400,21600,25200,32400,43200,57600,75600) {
  Start-Process python -ArgumentList "tools/day_run.py","freeze","--net",$N,"--sumocfg",$C,
     "--at","$t","--steps","120","--warmup",[Math]::Min(900,$t),"--work","work_geo","--tag",("t{0:D5}" -f $t)
}
python tools/day_run.py ladder --net $N --sumocfg $C --buildings $B --base base_ladder.json `
       --at 0,14400,21600,25200,32400,43200,57600,75600 --steps 120 --work work_geo

# the recommended configuration, one window (section 11.2)
python tools/day_run.py window --net $N --sumocfg $C --buildings $B --base base_disc003.json `
       --at 25200 --duration 600 --warmup 900 --out ds_peak600 --work work_peak --sample-every 25

# the long run, chunked and resumable. Re-running it skips every finished window.
python tools/day_run.py day --net $N --sumocfg $C --buildings $B --base base_disc003.json `
       --begin 0 --duration 86400 --window 1800 --warmup 900 --drop-traces --out datasets/intas_day

# the graceful-interrupt check (a REAL Ctrl-C to the child, 20 wall-seconds in)
python tools/day_run.py window --net $N --sumocfg $C --buildings $B --base base_disc003.json `
       --at 43200 --duration 120 --out ds_sigint --work work_sig --sigint-at 20
python tools/day_run.py verify ds_sigint          # VALID, digest re-derived

# the analyses
python tools/day_run.py volume   ds_peak600 --day-vehicle-steps 187066702 --day-departures 185923
python tools/day_run.py withheld ds_peak600
python tools/day_run.py dynamics ds_peak600 --bucket-s 60
python tools/day_run.py dynamics datasets/py_intas_hour --bucket-s 300
python tools/day_run.py dynamics datasets/py_intas_300s --bucket-s 60
```

`base_ladder.json` is `{"seed":42,"attacker_pct":0.15,"faulty_pct":0.05,"radio_model":"geometric",
"radio_env":"urban","radio_range_m":500,"radio_cap_max_mult":1.4,"radio_tx_power_dbm":23,
"radio_rx_sensitivity_dbm":-81,"emit_sample_prob":1.0,"emit_mobility_oracle":true}`;
`base_disc003.json` swaps `"radio_model":"disc","radio_range_m":709.4,"radio_cap_max_mult":6.0,
"emit_sample_prob":0.03`. `emit_sample_prob` and `emit_mobility_oracle` have no CLI flags on
`scms-poc` (§18.4), which is why every arm here goes through a base-config file.

## 17. What Part II does NOT show

* **No 86,400 s run was completed.** The longest engine run here is 1,800 s (at 485 concurrent) and
  the longest at peak density is 600 s; the day figures in §11.1 and §12.4 are projections and are
  labelled as such at every use. A 1,800 s peak-density run was started and lost to a harness kill at
  20 minutes (§13) — its trace is frozen and the run is one command away from being redone.
* **The true AM peak was never run through the engine.** Every high arm is a 900 s-warmup window
  topping out at 2,340 concurrent, against a continuous-day peak of **4,061** measured by SUMO
  (§10.1). Costs at the true peak are read off a fit extrapolated 1.7× past its top rung.
* **The full-day SUMO probe did not finish** inside this session — it reached 07:08 of 24:00 — so
  §10.2's continuous-day column exists for 00:00, 04:00, 06:00 and 07:00 and the PM peak's true
  concurrency is unmeasured. The §11.1 projection covers that gap with two bounds, not one answer.
* **The geometric memory curve was never seen to saturate**, which means the extrapolations in §12.2
  are upper bounds on a slope, not predictions of a ceiling. What that ceiling is has not been
  measured.
* **The reporter-saturation finding is a measurement of two committed datasets and one new run.** It
  has not been shown that fixing `report_budget` to a windowed rate changes precision — only that the
  gate stops passing anything and that the datasets do not say so.
* **The arms shared a host with other work.** Ratios are safe; absolute milliseconds are ±10%.
* **Nothing here re-validates the radio against measured radio data.** §10.3's phase dependence is a
  statement about this engine's channel model at different densities, not about reality.

## 18. What needs `run.py` (owned elsewhere — described, not done)

1. **`GeometricChannel.prune` cannot bound a long run** (run.py:831). It drops a pair only when
   *both* endpoints have despawned. A live-pair sweep — drop any key with an endpoint not in
   `live_vids`, and re-seed it deterministically from `(seed, tx, rx)` if the pair reappears, which is
   exactly what `_link_state` already does on a miss — would make the store O(concurrent pairs)
   instead of O(pairs ever seen). Digest-sensitive: re-seeding on reappearance restarts the AR(1)
   shadowing walk, so it changes the channel for a pair that leaves range and returns.
2. **`report_budget` / `reputation_max` should be windowed** (run.py:5968, `filed_by` / `received_by`
   at run.py:6099). A decay or a sliding window over `revoke_window_s` would make them the rate limits
   their names claim. Digest-moving. Until then the manifest should at least publish
   `reporters_over_budget` and `reports_from_over_budget_reporters` in `counts`, so a dataset says
   what fraction of its report table the MA ignored — that part is a `counts` field and moves no
   digest.
3. **The manifest does not record the steps actually run.** `config.n_steps` is the fixed-fleet count
   and stays at its default under `duration_s > 0`, and it is not updated on the SIGINT path either,
   so an interrupted dataset cannot say how much of the run it covers without counting rows. A
   `counts["steps_run"]` written from the loop variable is one line, in `counts`, digest-neutral.
4. **`emit_sample_prob` and `emit_mobility_oracle` have no CLI flags** — they are reachable only
   through `--dump-config` / `--config`. They are the two knobs that move output volume by 6.2× and
   they are the two a long run must set. `--emit-sample-prob` alongside the existing
   `--emit-mobility-oracle` would close it.
5. **A windowed trace reader would remove the chunking constraint.** `sumo_trace.load` has no seek
   index and `_write` sorts every row in memory. A format that carries a per-step byte offset, plus a
   reader that keeps only the live window, would make a monolithic day replay possible at O(concurrent
   × window) instead of O(all rows) — and would remove the SCMS-state discontinuity that chunking
   costs (§12.1).
