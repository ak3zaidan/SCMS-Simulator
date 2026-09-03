# The oracle leak in isolated detector mode — found, measured, closed

**A detector running in the isolated child could read the ground-truth labels of the very run it was
being graded on.** Not from memory — from disk. This document is the diagnosis, the fix, and the
measurement of both, and it ends with the part that is still convention rather than enforcement.

Closing this was the entire justification for building isolation. The scenario it exists for is a
detector benchmark or competition, where a third party submits a detector and the score has to mean
something. If the submission can read the answers, the benchmark is worthless.

## 1. The defect, as it was

**Closed, and genuinely, from the start:** the ground truth was never in the child's address space.
The child receives only the serialised `Observation`. A frame walk inside it reaches the serialiser's
own frames and nothing else, and the in-process attack — one line of
`sys._getframe(1).f_locals["b"]` reaching `veh.is_attacker`, `.attack_type`, true positions and the
`falsified` flag — does not work there.

**Not closed:** the filesystem. `ground_truth/gt_report_labels.jsonl` was **streamed** as the run
proceeded, so by the time a detector had been called twenty thousand times, the file on disk already
held the oracle verdict for every report filed so far — along with `reporter_true_id` and
`subject_true_id`. The child is an ordinary OS process with ordinary read access to it. Measured, from
inside the child, mid-run:

```
ground_truth/gt_report_labels.jsonl -> readable, non-empty:
  {"_visibility":"ORACLE","report_correctness":"correct","report_id":"rpt_00001",
   "reporter_true_id":"veh_000","subject_true_id":"veh_001"}
```

### How this was nearly missed

The first version of the guide asserted the run's own oracle files were unreadable while the loop was
running. That was false, and the way it survived one round of measurement is instructive: the probe
detector fired nothing, so nothing streamed, so the probe read an **empty file and concluded the hole
was closed**. The test that pinned it therefore declares the hostile detector alongside the built-ins,
so there is real reporting activity and therefore real labels on disk to find. *A measurement of an
absence is only as good as the activity that would have produced a presence.* That property is
preserved in the inverted test, and so is the probe at message 20 000 rather than message 1.

## 2. What is enforced now

### 2.1 The primary fix: ORACLE output is not written while a worker is alive

`mock_pipeline/run.py` streams three tables during the loop to keep long runs memory-bounded:
`ma/ma_reports.jsonl`, `ground_truth/gt_report_labels.jsonl` and
`ground_truth/gt_emissions_sample.jsonl`. **When any check is declared `isolated`, the two
ground-truth tables are not streamed.** They go to a `_WithheldStream`, which accepts exactly the
writes the file handle accepted and creates nothing; the files appear only after the last worker has
been reaped.

Three details make that a real boundary rather than a delay:

* **`suite.close()` moved.** The workers are FINISHed and reaped **immediately after the step loop**,
  before `_write_side_files` runs. That extends the guarantee from the two streamed tables to the
  whole ground-truth set: `gt_vehicle.jsonl` (which carries `is_attacker` for every vehicle),
  `gt_attacks`, `gt_identity_map` and `gt_linkage_revocation` are all written after the child is
  gone. `CheckSuite.close()` is idempotent, so the later call on the normal path is a no-op and every
  error path still reaps.
* **`ma/ma_reports.jsonl` is deliberately still streamed.** It is MA-visible — the detector's own
  output, not the answer key — and a long run's memory bound depends on it.
* **`live_state.json` is refused, not silently disabled.** It is written *during* the loop and marks
  every attacker with state byte 1, so `isolated: true` together with `live_interval_s > 0` is a
  `ConfigError`. Withholding the tables while leaving the live map on would have made the claim false
  again by a different file.

### 2.2 The memory cost, measured

The concern with buffering is obvious: ground truth is the largest thing a run produces. So it was
measured rather than assumed, on this host, with `GetProcessMemoryInfo` peak working set.

**The InTAS AM peak** — `intas.trace`, 1 188 vehicles, 158 767 vehicle-steps, 300 s, `--attacker-pct
0.15 --seed 42` against `ingolstadt.net.xml` — run twice, once streaming and once with the
withholding forced on so the CPU work is identical:

| | streamed | withheld | delta |
|---|---:|---:|---:|
| `data_digest` | `c4a7cddeb4ef58dd254ad051186911ebfd2c0e143b84115ac8f8d967eb082491` | *same* | **byte-identical** |
| peak working set | 254.2 MB | 258.3 MB | **+4.1 MB (+1.6 %)** |
| wall clock | 37.0 s | 36.1 s | noise |
| oracle bytes held | — | 3.17 MiB (2 027 357 B labels + 1 299 994 B emissions) | — |
| spilled to disk | — | none | — |

**Four megabytes.** At the scale the task named, this is not a trade-off; it is free.

It stops being free further up. The heaviest dataset in this repository is a 0.1 s-step InTAS replay
(`datasets/py_intas_300s`, 1 595 741 reports) whose labels are 212.0 MiB and whose emission samples
are another 209.8 MiB. Feeding its real rows through the sink:

| tier | held | peak WS delta | ratio | write | commit | bytes back |
|---|---:|---:|---:|---:|---:|---|
| memory | 212.0 MiB | **+213.7 MB** | **1.008 B/B** | 0.46 s | 0.08 s | byte-exact |
| sealed | 212.0 MiB | **+0.1 MB** | 0.001 B/B | 1.24 s | 0.76 s | byte-exact |

The 1.008 ratio is the result of one decision worth recording: rows arrive as ~140-character `str`s,
and a `str` costs its characters plus a 49-byte header plus a list pointer, so holding 1.6 M of them
individually measured **1.442** bytes of RSS per byte of output. `_WithheldStream` joins them into
8 MiB blocks as they arrive, which brings it to 1.008. The same measurement caught a second defect:
committing with a single `"".join(parts).encode()` peaked at **754.4 MB** for a 212 MiB table,
because it materialised the whole file as a `str` and again as `bytes` on top of what was already
held. `commit()` now writes one compacted block at a time and releases each as it lands — peak
247.0 MB, i.e. the held bytes plus about one block.

### 2.3 Where it degrades, and to what

**At 384 MiB of withheld output** (`WITHHELD_MEMORY_BYTES`, a shared ceiling across both streams, not
a config field — it cannot change an output byte). Below it, nothing whatsoever exists on disk: no
file, no partial file, no `ground_truth/` directory. Above it, the overflow spills to
`<out_dir>/.withheld/<name>.sealed`, XORed with a 32-byte `os.urandom` key held only in the engine's
memory and never written anywhere; `commit()` unseals it into the real file and deletes it.

So the honest statement has two tiers, and both are in the code and in the tests:

* **under 384 MiB of ground truth — there is nothing on disk to read;**
* **over it — there is a file, and it holds nothing readable.**

The key is per-stream, per-run, from `os.urandom`, and never touches a value that reaches the digest
(the plaintext is restored bit-for-bit before anything hashes it), so it cannot move a golden and it
does not draw from any engine RNG. The keystream is `shake_128(key || block)` at 1 MiB per call, so
sealing 212 MiB costs ~200 hash calls and ~2 s for the round trip.

A run that fails between the spill and the commit leaves a sealed file behind; `commit()` runs under
a `finally` that discards it, so this needs a hard kill to happen, and what is left is ciphertext.

### 2.4 Defence in depth: the child is not handed the path

Additive, cheap, and **not a substitute** for §2.1:

* **A fresh empty working directory.** The worker no longer starts in the engine's cwd; it starts in
  its own `tempfile.mkdtemp()`, removed when it is reaped. `os.listdir(".")` in the child returns
  `[]`, and a relative path like `out/run7` resolves against nothing.
* **An environment that does not name the dataset.** `child_env(deny=(out_dir,))` drops every
  environment variable whose value names the output directory or anything inside it, and every
  `PYTHONPATH` entry likewise. `PATH`-shaped variables are *filtered* rather than deleted, because
  deleting `PATH` on Windows breaks the child for reasons that have nothing to do with the dataset.
* Because the child no longer starts in the engine's cwd, `python -m` would have put the empty
  sandbox on its `sys.path` in place of the parent's cwd, so `child_env` carries the parent's cwd
  over explicitly. Nothing that used to import stops importing.

**This is not containment and the tests say so separately from the ones that are.** A determined
child can still walk the filesystem. It does not need to guess: `scms_sim_ref.__file__` names this
repository, and an absolute path works from any working directory.

## 3. What is still open

**Every other dataset on the machine.** The child runs as the same user. A completed earlier run's
`ground_truth/*.jsonl` — the thing a benchmark host most plausibly has lying around — is one
`open()` away, and no amount of Python closes that. Pinned by
`tests/test_detector_isolation.py::test_the_child_still_reads_the_filesystem_and_the_docs_say_so`,
which reads such a file from inside the child on purpose.

The honest mitigations are operational, and none of them is implemented here:

* run the benchmark on a host whose other datasets the submitter's process cannot reach, or publish
  the dataset only after the run;
* a separate user or a restricted token, plus ACLs on the output directory;
* an OS-level sandbox around the worker — the real answer and the least portable (on Windows a job
  object or an AppContainer). Isolated mode makes it *possible*, because there is exactly one process
  to wrap and it speaks exactly one pipe; it does not provide it.

**`ma/ma_reports.jsonl` is still streamed, deliberately, and it is readable.** It is MA-visible — no
`_visibility: ORACLE` row, no true id, no `falsified` flag — so a child that opens it learns the
built-in suite's `detnorm_*` scores and the MA's own report decisions for messages already processed,
not the labels. That is a weaker thing to leak than ground truth and it is not free: a submitted
detector could copy the built-ins' verdicts on senders it is about to score again. If a benchmark
cares, the answer is the same one as for §3's first paragraph — an OS-level sandbox — because the
alternative, withholding the MA stream too, costs every long run its memory bound for a file that
contains no answers.

**It is also not a resource limit.** The child can allocate, spin, spawn processes and open sockets.
The watchdog turns "does not answer" into a failed run; it does not cap what the detector does in the
meantime.

## 4. The tests, and which claim each one carries

| test | claim |
|---|---|
| `test_the_child_cannot_read_this_runs_oracle_labels` | **the fix.** Told the exact output path (base64-encoded, so the env scrub cannot make the measurement vacuous), probing at message 20 000, alongside `@builtins` so there are real labels to find: no `ground_truth/` directory, `FileNotFoundError`, `out_dir` contains only `ma`. Then asserts the file is complete and correct after the run. |
| `test_without_an_isolated_detector_the_labels_are_still_streamed` | **the regression guard.** The identical probe, in process, still reads ORACLE rows out of the streamed file. A change that started withholding unconditionally would cost every long run its memory bound, and fails here. |
| `test_every_worker_has_exited_before_the_first_ground_truth_byte_is_written` | the reordering, asserted on the OS: at the moment any ground-truth file is created — including `gt_vehicle.jsonl`, which the withholding does not touch — every worker already has an exit status from `Popen.poll()`. A revert that moves `suite.close()` back after the digest fails only here. |
| `test_isolated_and_in_process_agree_bit_for_bit` | the withheld write is byte-identical to the streamed one — at the level of the whole dataset. |
| `test_the_withheld_stream_reproduces_the_streamed_bytes_exactly` | the same, at the level of the bytes, across all three tiers (memory, spilled, half-and-half). |
| `test_the_spill_holds_no_readable_oracle_and_is_removed` | above the ceiling: the plaintext is absent from the sealed bytes and the sealed file is gone afterwards. |
| `test_the_memory_ceiling_is_shared_across_the_withheld_streams` | one budget per run, not one per stream. |
| `test_child_env_drops_every_path_that_names_the_output_directory` | the defence-in-depth half, asserted on the function. |
| `test_a_live_map_and_an_isolated_detector_are_refused_together` | the other file that would have leaked the oracle mid-run. |
| `test_a_frame_walk_in_the_child_reaches_only_the_serialiser` | the original address-space claim, unweakened. |
| `test_the_child_still_reads_the_filesystem_and_the_docs_say_so` | **what is still open**, asserted so the documentation cannot quietly stop saying it. |
| `test_the_documentation_states_the_two_tiers_and_the_residue` | this file and `DETECTOR-PLUGIN.md` still state all three: what is enforced, where it degrades, what is convention — including the ceiling in MiB, read off `WITHHELD_MEMORY_BYTES` so the prose cannot drift from the constant. |
| `test_the_module_states_what_it_does_not_close` | the same three, in `api/isolate.py`'s own docstring. |

## 5. Provenance of this document

The claim in `DETECTOR-PLUGIN.md` §2.8 was rewritten twice. The first version said the run's own
oracle files were unreadable while the loop was running, which was false. The second withdrew that
and said only that the ground truth is not in the child's address space, which was true but was not
the claim a benchmark needs. This is the third, and it is the first one where the code does what the
sentence says. This project has already had to withdraw one claim about plugin containment; the value
of the isolation feature depends on its description being exactly true.
