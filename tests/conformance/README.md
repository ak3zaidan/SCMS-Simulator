# `tests/conformance` — the conformance kit

This is what lets somebody else trust this simulator, or extend it.

It is a workspace member (`v2xw-conformance`), not a loose directory, because everything in
it is meant to be *run* by someone outside this repository: a contributor checking their own
plug-in, a second implementation checking its wire format, and continuous integration
comparing three platforms. A directory of files that no `cargo test` reaches would be
documentation wearing a test's clothes.

```
cargo test -p v2xw-conformance                 # everything
cargo test -p v2xw-conformance --test vwp      # the 65-item wire checklist
cargo test -p v2xw-conformance --test kit      # plug-in, golden, firewall
cargo run  -p v2xw-conformance --bin v2xw-golden-digest   # the digest CI compares
```

## The four parts

| Part | Library | Driven by |
|---|---|---|
| the VWP v1 wire checklist, one test per item | `src/checklist.rs` | `tests/vwp/` |
| the plug-in conformance suite | `src/plugin.rs` | `tests/kit/plugin_suite.rs` |
| the golden determinism harness | `src/golden.rs` | `tests/kit/golden_suite.rs`, `src/bin/golden_digest.rs` |
| the interface firewall | `src/firewall.rs` | `tests/kit/firewall_suite.rs` |

### 1. The wire suite — `tests/vwp/`

`docs/protocol/vwp-v1.md` §10 is a 65-item checklist whose own text says the ids "match
`tests/conformance/vwp/`". Every test in `tests/vwp/` is named for the item it checks —
`f1_…`, `q4_…`, `r10_…` — so a failure names the clause.

Not every item is mechanised *here*. Most of §10's server-side items are already tested,
better, inside the crate that implements them: `v2xw-record` owns the container items and
`v2xw-server` owns the connection items. Re-implementing them would give two tests that can
disagree, and the weaker one would be the one running against a fixture. So the kit
mechanises what spans crates or what nobody owned, and *points at* the owner of the rest
through `checklist::COVERAGE` — a pointer that `tests/vwp/coverage.rs` resolves by opening
the file and looking for the test. Rename a delegated test and the ledger breaks.

`coverage.rs` also re-reads §10 out of the specification at test time and compares it with
what this build believes §10 says, so a new item, a renamed id or a moved clause is a
failure rather than a silence.

Twelve items cannot be established in process: a 1000 ms deadline on a live socket, a p95
over a hundred seeks, a browser rendering a frame. Each is recorded as a gap **with the
reason**, and the count is pinned so it can only fall. One item — F8, zstd compression — is
not implemented at all; its test pins the current behaviour so the item goes red the day
compression lands.

### 2. The plug-in suite — `src/plugin.rs`

A contributor implements `PluginUnderTest` for their model and calls `run_suite`. The suite
establishes that the plug-in owns no random-number generator, reads no wall clock, reaches
no ground truth, registers a valid card with a source for every default, and is
deterministic under reordering — plus thread independence, output quantisation, stream
hygiene and the declared-RNG-domain comparison §17 asks for.

`ProbeCtx` is the context it runs against: `v2xw_sec::testctx::TestCtx`'s shape, with no
world and no actors, plus a log of every `(domain, entity)` key the plug-in checked out.
That log is what turns "the card declares its RNG domains" from a claim into a comparison.

A check that could not be run reports `NotChecked`, never `Pass`. A plug-in that names no
source files gets no source scan and the report says so.

`tests/kit/plugin_suite.rs` runs the suite against a correct reference plug-in and against
four broken ones — state carried between entities, a draw from an undeclared domain, a raw
unquantised double, and a source file that reaches for `rand::thread_rng`. Every check has
been watched going red.

### 3. The golden harness — `src/golden.rs`

One scenario, one digest, compared against a record committed to `golden/`.

Two runs in one process share a compiler, a platform and a libm, so agreeing proves much
less than it looks like it proves. The comparison that matters is against a committed value
and across three platforms — and the three-platform half cannot be a `#[test]`, because each
CI job only checks its own assertions. `src/bin/golden_digest.rs` prints the digest in a
line-oriented form for a job to publish and a fourth job to `diff`, and it calls the same
function the test asserts on, so the two cannot drift.

**When there is no golden record, the test fails** and names the environment variable that
writes one. A harness that quietly wrote the file and passed would be inventing its own
oracle and agreeing with it for ever.

```
V2XW_CONFORMANCE_BLESS=1 cargo test -p v2xw-conformance --test kit golden
```

### 4. The interface firewall — `src/firewall.rs`

`crates/v2xw-node/src/firewall.rs` holds a sentinel that reads the node crate's source and
fails if the runtime has acquired a way to see the truth. It works and it has been shown to
go red. It has a hole:
`crates/v2xw-node/tests/firewall_sentinel.rs::rust_sources` collects files with one
`std::fs::read_dir` of `src/`, which is one directory deep. Nine crates already have a
`src/` subdirectory. A rule that moved into one would stop being enforced while the sentinel
went on reporting success — a check that passes while looking at less than it claims to.

There is a second, smaller hole: `scan_ground_truth_fields` sets its `in_test` flag at the
first `#[cfg(test)]` and never clears it, so everything after the first test module in a
file goes unchecked.

This module is the general version without either: it recurses, it sorts, it closes the
test-module bracket rather than latching it, and it discards a `#[cfg(test)]` that turns out
not to precede a module. `tests/kit/firewall_suite.rs` demonstrates the difference on a tree
it builds for the occasion rather than asserting it from memory.

It generalises past ground truth to the other textual invariants of the build brief:

| Check | Rule | Exemptions |
|---|---|---|
| `ground-truth` | a node reads beliefs, never truth (I-C2) | the rule table itself |
| `wall-clock` | no engine-facing code reads a real clock | the CLI's `wall.rs`, the server's HTTP task |
| `hash-order` | no `std` hash container, whose iteration order is unspecified | two Arrow metadata maps |
| `transcendental` | no `std` transcendental; always `v2xw_core::math` | none |

Exemptions are data with a reason attached, and the suite fails if an exempted file has been
deleted — an exemption pointing at nothing is a hole nobody notices.

## The rule every part of this kit follows

A check nobody has seen go red is a check that reads as evidence while proving nothing.
`docs/design/findings/slice-verification.md` records four of those in this project. So every
assertion here either has an injected fault beside it, or a control that shows the check can
distinguish the case it is about from the case it is not.
