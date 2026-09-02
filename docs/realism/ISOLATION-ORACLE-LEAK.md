# Out-of-process isolation does not close the oracle leak

**A detector running in the isolated child can read the ground-truth labels of the very run it is
being graded on.** Not from memory — from disk.

This matters because closing that leak was the entire justification for building isolation. The
scenario it exists for is a detector benchmark or competition, where a third party submits a detector
and we need its score to mean something. If the submission can read the answers, the benchmark is
worthless.

## What is and is not closed

**Closed, and genuinely:** the ground truth is not in the child's address space. The child receives
only the serialised Observation. A frame walk inside the child reaches the serialiser's own frames
and nothing else, and the in-process attack — one line of `sys._getframe(1).f_locals["b"]` reaching
`veh.is_attacker`, `.attack_type`, true positions and the `falsified` flag — does not work there.

**Not closed:** the filesystem. `ground_truth/gt_report_labels.jsonl` is **streamed** as the run
proceeds, so by the time a detector has been called twenty thousand times, the file on disk already
holds the oracle verdict for every report filed so far — along with `reporter_true_id` and
`subject_true_id`. The child is an ordinary OS process with ordinary read access to it.

## How this was nearly missed

The first version of the guide asserted the run's own oracle files were unreadable while the loop
was running. That was false, and the way it survived one round of measurement is instructive: the
probe detector fired nothing, so nothing streamed, so the probe read an **empty file and concluded
the hole was closed**.

The test that pins this now deliberately declares the hostile detector alongside the built-ins, so
there is real reporting activity and therefore real labels on disk to find. A measurement of an
absence is only as good as the activity that would have produced a presence.

## What to do about it

The narrow claim that survives — "ground truth is not in the child's address space" — is worth
keeping and is now stated that way. But it is not the claim a benchmark needs.

Options, roughly in order of how much they actually buy:

1. **Do not stream oracle files while a third-party detector is active.** Buffer ground truth and
   write it after the run completes. The child then has nothing to read, because nothing exists yet.
   This is the smallest change that makes the claim true, and it costs memory proportional to the
   run's ground-truth volume.
2. **Deny the child access to the output directory.** Give the worker a restricted working directory
   and an environment that does not name the dataset path, so it cannot trivially find the files.
   Weaker than (1) — a determined child can still search the filesystem — but cheap and additive.
3. **OS-level sandboxing.** The real answer, and the least portable; on Windows this is a job object
   or an AppContainer, which is a substantial piece of work and not obviously worth it here.

Whatever is chosen, the documentation must keep saying plainly what is enforced and what is
convention. This project has already had to withdraw one claim about plugin containment; the value
of the isolation feature depends on its description being exactly true.
