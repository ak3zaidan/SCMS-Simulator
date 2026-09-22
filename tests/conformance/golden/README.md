# Golden determinism records

One JSON file per case in `v2xw_conformance::golden::CASES`. Each holds the digests and
counts one run of that scenario produced: no float, no path and no timestamp, because a
golden record whose fields can differ between two correct runs is a record that gets
re-blessed until it means nothing.

A missing record makes `tests/kit/golden_suite.rs` **fail**, on purpose. The first failure is
where a human looks at the numbers and decides they are right. To write one:

```
V2XW_CONFORMANCE_BLESS=1 cargo test -p v2xw-conformance --test kit golden
```

Re-blessing after a deliberate change to the engine's output is the same command. Say in the
commit message what changed and why; a record re-blessed without an explanation is how a
determinism gate stops being one.

The same numbers are printed by `cargo run -p v2xw-conformance --bin v2xw-golden-digest`,
which is what a CI job publishes for the three-platform comparison.
