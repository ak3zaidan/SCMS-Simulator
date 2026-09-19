# Legacy golden-digest forensics — why `04ae9736…` no longer reproduces

Date: 2026-09-18 · Host: macOS 26.2 (Darwin 25.2.0), arm64 · Repo: `/Users/ahmedzaidan/Developer/SCMS-Simulator`
Engine under test: `/Users/ahmedzaidan/Developer/SCMS-Simulator/legacy/scms_sim_ref` (byte-identical to `HEAD:src/scms_sim_ref`)

---

## 1. ROOT CAUSE (one paragraph)

The dataset contains **exactly one floating-point field that is written unrounded** —
`ma/ma_reports.jsonl → st_bbox`, built at
`/Users/ahmedzaidan/Developer/SCMS-Simulator/legacy/scms_sim_ref/mock_pipeline/run.py:2089`:

```python
st_bbox=[min(cx, px), min(cy, py), max(cx, px), max(cy, py)],
```

Every other float in every output record goes through `round(x, 3)` (39 `round(` sites in `run.py`).
`cx/cy/px/py` are vehicle positions derived through `math.sin` / `math.cos` (lane offsets at
`run.py:1610-1611, 2501-2502`; attack renderings at `run.py:1922, 1957, 1983, 2006`). `sin`, `cos`,
`tan`, `exp` and `pow` are **not correctly-rounded** in any libm, so their last bit is
implementation-specific. The goldens were pinned on **Windows x86-64** (MSVC CRT libm) in
August 2026; we now run **macOS arm64** (Apple libm). Those 1–16-ULP differences land verbatim in
`st_bbox`, change the SHA-256 of `ma/ma_reports.jsonl`, and therefore change `data_digest`.
Nothing else in the dataset differs.

**Proof (necessary and sufficient):** rounding *only* `st_bbox` to 3 decimals — the same rounding the
engine already applies everywhere else — makes `data_digest` **identical across CPU architectures**
for all four configs tested:

| config | digest, arm64 | digest, x86-64 | digest after rounding st_bbox (both) |
|---|---|---|---|
| default (seed 7, grid 5×5) | `2b3a9d25…` | `1a672e13…` | `94fd3634…` |
| collusion+RSU+logdistance (seed 11) | `77719d9d…` | `e1aa1bad…` | `1929baca…` |
| VRU+DENM (seed 13) | `32927515…` | `9e8d275c…` | `a38bec1e…` |
| ring (seed 7) | `bbdc1513…` | `1dd620ab…` | `fb3722eb…` |

---

## 2. Evidence trail

### 2.1 The engine is SELF-CONSISTENT here (the important question)

| experiment | digest |
|---|---|
| default config, run twice in one process | `2b3a9d25…` / `2b3a9d25…` |
| 3 separate processes, `PYTHONHASHSEED=random` | `2b3a9d25…` ×3 |
| different `out_dir` each time | unchanged |

→ **Yes: same seed ⇒ identical digest, byte for byte, every time.** No hash-seed randomisation, no
dict/set-iteration leakage, no path leakage (the `out_dir` appears only in `manifest.json`, which
`_data_digest` deliberately excludes). The engine is a *deterministic* reference on this machine; it
just does not agree with a *different* machine's last float bit.

### 2.2 It is not a code change, and not the repository move

```
diff -rq HEAD:src/scms_sim_ref  legacy/scms_sim_ref   → identical
diff -rq HEAD:tests             legacy/tests          → identical
```

Running the default config with the engine tree checked out at each commit from the one that first
pinned the golden through HEAD, all on this machine:

| commit | date | digest |
|---|---|---|
| `0552532` (first pins `04ae9736`) | 2026-08-21 | `2b3a9d25…` |
| `4e98c22` | 2026-08-21 | `2b3a9d25…` |
| `09d690c` | 2026-08-22 | `2b3a9d25…` |
| `a3ab5d4` (the commit `test_config_knobs` cites) | 2026-08-22 | `2b3a9d25…` |
| `f48a314`, `4ac649a` | | `2b3a9d25…` |
| `b6183cd` (HEAD) | 2026-09-17 | `2b3a9d25…` |

The engine's own invariant ("the default path never moves") **holds perfectly**. The value is
machine-dependent, not commit-dependent. Hypotheses (d) *path leak* and (e) *code regression* are
eliminated.

Note: `9db4e04` on `origin/feat/realism` *does* deliberately re-pin digests (ADR "ground-truth
kinematics and digest repin"), but it is **not an ancestor of HEAD** and its code is not in this tree.

### 2.3 Python version: not the cause

| interpreter | arch | cryptography | digest |
|---|---|---|---|
| CPython 3.11.15 | arm64 | 50.0.1 | `2b3a9d25…` |
| CPython 3.12.8 (the repo venv) | arm64 | 50.0.1 | `2b3a9d25…` |
| CPython 3.13.14 | arm64 | 44.0.0 | `2b3a9d25…` |

### 2.4 `cryptography`: not the cause

RFC 8032 Ed25519 test vector 1 under the installed `cryptography` 50.0.1:

```
pub  d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a   (== RFC)
sig  e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b   (== RFC)
```

Exact match, and cryptography 44 vs 50 give the same dataset digest (§2.3). numpy/pandas/pyarrow are
irrelevant: they are not imported on the default pipeline path and do not write any file in the
digest's file set.

### 2.5 CPU architecture: **this is the cause**

Same OS, same source, same seed, only the CPU/libm changed:

```
CPython 3.13.14 macOS arm64   → 2b3a9d25857318b443348830664c1a1836fa849bd2edaca66d47ea03097e327b
CPython 3.13.14 macOS x86_64  → 1a672e13431719a179af16bd6b4af1017fd4585d980af5ae49f4b6ba8c575f1a
```

Direct libm probe (20 000 random arguments per function, hashed):

| function | arm64 | x86_64 | agree? |
|---|---|---|---|
| `sin` | `e0ee317b7d4eeece` | `4bcffe3b63e8756a` | **no** |
| `cos` | `aa908394d8730984` | `1d333af6408cdf3c` | **no** |
| `tan` | `4858f002d5aa7fdd` | `ab192ca8b4089d2c` | **no** |
| `exp` | `7e6646c3117266f4` | `2a673d048d9bcdb5` | **no** |
| `pow` | `cba2eaef3421e3fa` | `da8556024372914f` | **no** |
| `log`, `log10`, `sqrt`, `atan2`, `hypot` | identical | identical | yes |

A concrete 1-ULP case:

```
math.sin(3.9999999999999996)
  arm64  : -0x1.837b9dddc1eacp-1   (-0.756802495307928 )
  x86_64 : -0x1.837b9dddc1eabp-1   (-0.7568024953079279)
```

### 2.6 The goldens were pinned on Windows

At the pinning commit `0552532` the repository was a Windows project: `run.ps1`, `gui.ps1`,
`scms-sim/**/*.ps1`, and a `.gitignore`/README documenting the toolchain under
`C:\Users\Administrator\tools` with PowerShell activation. So the golden libm is the MSVC CRT's —
a third implementation, different again from both Apple libms. (This matches the note already in
`legacy/README.md`: "pinned on the original Windows/Python environment and already failed on this
machine before the Phase 0 restructure.")

---

## 3. Which file(s) and which field(s) changed

Cross-architecture file-by-file comparison, four configs:

| config | files differing | rows differing | fields differing |
|---|---|---|---|
| default | `ma/ma_reports.jsonl` (+`manifest.json`, which embeds the hashes) | 290 / 1375 | `st_bbox` only |
| collusion+RSU+logdistance | `ma/ma_reports.jsonl` | 383 / 3869 | `st_bbox` only |
| VRU+DENM | `ma/ma_reports.jsonl` | 83 / 2374 | `st_bbox` only |
| ring | `ma/ma_reports.jsonl` | 111 / 3082 | `st_bbox` only |

All nine other data files (`gt_vehicle`, `gt_identity_map`, `gt_attacks`, `gt_report_labels`,
`gt_linkage_revocation`, `gt_emissions_sample`, `ma_cert_status`, `ma_crl_events`,
`ma_investigations`) are **byte-identical**, as are row counts, row order and report ids.

Concrete row diff (default config, `rpt_00270`):

```
arm64 : "st_bbox":[17.88213340810603 ,167.16370574051686,22.726246249193643,193.66090952095914]
x86_64: "st_bbox":[17.882133408106025,167.16370574051686,22.726246249193643,193.66090952095914]
                            ^^^^^^^^^  (1 ULP)
```

Magnitude of the deviation:

| config | max abs Δ | max relative Δ | max ULPs |
|---|---|---|---|
| default | 2.8e-14 m | 1.9e-15 | 16 |
| collusion | 5.7e-14 m | 4.5e-16 | 4 |
| ring | 5.7e-14 m | 1.9e-16 | 1 |

i.e. ≤ 57 femtometres of "position error". A full scan of every JSONL record in all four datasets for
any float `f` with `round(f,3) != f` returns **only** `ma/ma_reports.jsonl → st_bbox[]`.

`st_bbox` is **written and schema-declared and read by nothing**: the only references in the whole
package are `run.py:2089` (write) and `schemas/records.py:102` (declaration). No detector, no
`datagen/featurize.py` feature, no `tools/verify_data.py` check consumes it. The field that breaks
27 tests is inert to every piece of downstream logic.

---

## 4. Are the 437 passing tests trustworthy?

**Yes, with one caveat worth stating.**

- Across the four cross-architecture pairs, `n_vehicles`, `n_reports` and `n_revoked` are *identical*
  (73/1375/12, 92/3869/20, 129/2374/24, 95/3082/17). No behavioural drift.
- The only changed bytes are ≤ 5.7e-14 m in a field nothing reads, three orders of magnitude below
  the 1e-3 quantisation of every other emitted float.
- The passing suite asserts behaviour — detector formulas, separability, ranges, invariants, schema,
  leakage rules, linkage/butterfly vectors, ML featurisation stability — none of which is affected by
  a sub-picometre perturbation of an unread field.

Caveat: the engine is *not provably* float-portable, only empirically so. A stress test that pushed
**every** `sin/cos/tan/exp/log/log10/pow/atan2/hypot` result by 1 ULP changed the ring config's report
count from 3 082 to 3 135 — detector-threshold and event-ordering decisions can flip under a large
enough FP perturbation. The real cross-platform delta is far smaller and did not trigger this, but
"same semantics on any machine" is an observation, not a guarantee.

---

## 5. Recommendation for the Rust re-implementation

The finding is mostly *reassuring*, and it changes the validation strategy rather than the port list.

1. **Do not port `st_bbox` as-is, and do not chase the legacy digests.** The 27 failures are an
   artefact of one unrounded, unread field. `04ae9736…` is not recoverable on any non-Windows host,
   and is not worth recovering. ADR 0002's decision to retire the fixed digests is correct; this
   analysis supplies the *reason*, which should be recorded: **a golden digest over raw IEEE-754
   doubles is not a portable conformance artefact.**

2. **Port the semantics, and validate them with tolerances, not hashes.** Port the detector formulas,
   the attack renderings (`run.py:1892-2006`), the MA decision rule (`trusted()` / windowed gate at
   `run.py:2044-2058`) and the report-filing path. Validate the Rust port against *regenerated*
   Python outputs with:
   - exact equality for integers, ids, enums, counts, orderings and revocation sets;
   - `|Δ| ≤ 1e-9` (or the legacy 1e-3 quantum) for floats;
   - distributional equality for rates (precision/recall/report volume), which is what the 437
     semantic tests already encode.
   Those 437 tests are the conformance suite; the 27 digest asserts are not.

3. **In the new engine, make byte-determinism a property of the format, not of libm.** If V2X World
   still wants a `data_digest` gate (ADR 0004's determinism rules), quantise **every** emitted float
   at serialisation time — one central `round_to(x, 1e-3)` / fixed-point encoder in the writer, with a
   test that scans outputs for any value not on the quantisation grid (the scan in §3 is 20 lines and
   catches exactly this class of bug). Then the digest is stable across x86-64/aarch64,
   macOS/Linux/Windows, and Rust vs Python.

4. **Independently, do not let raw floats drive control flow across engines.** Rust's `f64::sin` is
   its own libm (`libm`/`std`), which will differ from CPython's again. Anywhere a detector compares a
   transcendental-derived value to a threshold (`detector_z_threshold`, `sybil_cell_m` bucketing,
   plausibility gates), either quantise before comparing or accept that boundary cases may flip — and
   make the cross-engine test tolerant of exactly those boundary flips rather than asserting equality.
   The ring-config stress result (§4) shows this is a real, not theoretical, sensitivity.

5. **One cheap piece of hygiene:** `st_bbox` is dead weight — declared, written, never read. If the new
   schema keeps a spatio-temporal bounding box, give it a consumer or drop it; do not reproduce a
   field whose only observable effect was to break the reference's own reproducibility contract.

---

## 6. Measured suite result (full run, 11m28s)

`cd legacy && .venv/bin/python -m pytest tests -q` → **26 failed, 437 passed, 4 skipped, 1 deselected**
(the brief said 27; the measured number on this host is 26). All 26 failures are golden-digest
asserts in 13 files: `test_attack_magnitude` (4), `test_combined_attacks` (1), `test_config_knobs` (3),
`test_denm` (2), `test_engine_truth` (1), `test_evasive_attackers` (1), `test_gap_acceptance` (4),
`test_lane_changes` (3), `test_network_fidelity` (2), `test_radio_propagation` (1), `test_vru` (2),
`test_vru_denm_harden` (1), `test_vru_spoofing` (1). Zero semantic failures.

The digest pytest reports for the default config is exactly the value reproduced standalone in §2.1:

```
E  assert '2b3a9d258573...7ea03097e327b' == '04ae9736f519...9f754a69cee38'
```

---

## Appendix — reproduction

Artifacts in `/private/tmp/legacy-forensics/`: `gen.py`, `gen2.py` (config variants),
`perturb.py` (1-ULP libm emulation), `libm_probe.py`, and the generated datasets
(`runA`, `run_py313_x86`, `{collusion,vrudenm,ring}_{plain,x86}`, `out_<commit>`).
Interpreters: `v311/` (3.11 arm64), `v313arm/`, `v313x86/`. Nothing under `legacy/` was modified.
