# Recording and metrics defect register (adversarial verification, 2026-09-19)

An independent verifier re-derived every headline claim rather than trusting
the builders' tests, and drove its own experiments. 12 findings, 3 high.

## Overall

Two strong crates with a small number of real defects, three of which I would fix before this is used to produce evidence anyone relies on.

What the builders claimed and what I found, claim by claim. Byte identity: PASS, independently verified and sound by construction — but the stronger V5 claim built on top of it is false (finding 2). Pose quantisation: PASS, and stronger than claimed — I drove 100,000 steps of a curving, accelerating trajectory in a single GOP and measured 0.0005 m worst case with zero integer mismatches, confirming the delta reference really is the transmitted quantised value. Seek: PASS with a 57x margin; my p95 of 1.763 ms is better than the 3.494 ms headline, and the builder correctly explained the gap as machine load. Statistics: PASS — Wilson matches a 50-digit reference to under one ULP at both boundaries and all three levels, percentiles are type-7 and correctly stated, and every hand-derived metric matched exactly. Invariants: nine of ten detect an injected violation; the tenth is finding 7. Robustness: FAIL.

The three that matter most. (1) A single flipped bit in a recording can abort the process with an exabyte allocation, on every read path, against a module that explicitly promises the opposite — and the crate already holds the value it would need to cross-check. (2) The NODE-only stripper is not the live node producer: a parked actor that changes lane or whose attacker bit toggles gets an all-zero moved row in the blind stream that a live blind run would not emit, which fails conformance V5 and leaks the fact that a hidden field changed. The existing V5 test passes only because every fixture actor moves every step — a good example of a fixture that agrees with the implementation rather than testing it. (3) D9 is enforced on the VWP frame path and not at all on the serde record path, so the crate's own fixture writes seven off-grid float fields into the recording, and `content_digest` hashes them.

A pattern worth naming: three of the defects (V5, the nested-float D9 hole, the D9 invariant's swallowed guard) are places where a check exists, passes, and does not check what its documentation says. That is more dangerous than an absent check, because it buys confidence. The fix in each case is to make the test adversarial rather than confirmatory — a stationary actor for V5, a nested payload for the exporters, a wrong-grid value for D9.

Everything else I probed held up well. The wire implementation reproduces §9's three hex vectors byte for byte, and does so by parsing the dump out of the specification file rather than from a transcription, which is the right way to write that test. Alignment satisfies §2.2 throughout. The decoders bound-check before allocating, so a body declaring four billion records returns `Truncated` rather than exhausting memory — the container layer is where that discipline lapses, not the wire layer. The metrics crate's refusal to put a Wilson interval on a ratio of sums or on F1 is exactly right and unusually disciplined, the insufficiency machinery works at every level, and `tests/discipline.rs` scanning the crate's own sources for a clock read is a good pattern the record crate could adopt.

I changed no crate source. All temporary instrumentation is removed; both suites pass (v2xw-record 65 tests, v2xw-metrics 188) and `cargo clippy -p <crate> --all-targets -- -D warnings` is clean for both. No workspace dependency was added.

## Byte identity

PASS — independently verified, and the mechanism is sound by construction. I built my own 53-frame stream (5 actors, 45 steps, spawn at step 7, despawn at step 30, a +250 m teleport at step 23 that exercises the absolute escape, a signal changing phase, a zero-length Event payload, and a plug-in channel id 4242 that first appears at step 20), set FLAG_RESYNC on every frame as a sender would, recorded it, replayed it and compared `live.canonical().as_bytes() == replayed.frame.as_bytes()` for all 53 frames. Exact equality, at both a 4 MiB chunk target (1 chunk) and a 1-byte chunk target (5 chunks), with 5 frames differing only in a TRANSPORT_FLAG_MASK bit and no stored frame carrying any transport bit (P3). The guarantee holds by construction: `write_frame` stores what it is handed after a single `with_flags(flags & CANONICAL_FLAG_MASK)`, `replay` returns the stored bytes, and there is no decode-and-re-emit path.

Break attempts, all four of which byte identity survived: a channel added mid-run (a plug-in event channel first written at step 20 — new MCAP channel opened inside a live chunk, round-trips); a zero-length Event payload (`off_payloads` written as 0 per §2.2, decodes back to an empty payload); a delta with no preceding keyframe (replays byte-identically; `verify` correctly reports "delta before any keyframe: it is rooted in nothing (§3.4)" and `seek` correctly returns SeekOutOfRange rather than panicking); a run whose origin changes mid-stream (all 10 frames replay byte-identically — note `verify` catches this only incidentally, via the gop_index reset, not by checking §3.3.1's "origin is constant for the whole run"); and a chunk boundary forced between every GOP.

One caveat that is a finding in its own right: the builder's *stronger* claim — that a `full` recording replayed under the `node` profile is byte-identical to a live `node` run (conformance V5) — is FALSE. See finding 2.

## Pose quantisation

PASS. Independently measured worst-case drift over 100,000 steps in a SINGLE GOP (keyframe period 10,000 s, 100 ms step, so exactly one keyframe and 99,999 deltas — nothing re-anchors the reference): max |decoded − true| = 0.000499998 m (x), 0.000499997 m (y), 0.000499999 m (z). That is half a millimetre, the quantisation bound, and it is bounded rather than accumulating.

Trajectory: curving and accelerating throughout — a circle of radius 400 ± 120 m whose angular rate itself grows (ω = 0.01 + 1e-5·t), with a linear drift superimposed so no coordinate is periodic, and z on a separate sinusoid. Max per-step displacement 9,811 mm, zero escapes.

The delta reference is genuinely the previously TRANSMITTED quantised value, not the true value: I reconstructed the receiver state by hand from the wire bytes only (keyframe integers, then integer accumulation of dx/dy/dz) and compared it at every one of the 100,000 steps with `quant::x_mm(true_x, origin)`. Zero integer mismatches. A crate differencing true positions would have accumulated ~0.4 mm per step and been metres out; this one is exact.

i16 millimetre range: 130 km/h = 36.111 m/s, which at the configured 100 ms mobility step is 3,611 mm per step — 8.9x inside the 32,000 mm escape threshold and 9.1x inside i16::MAX. Ample. (Worth noting for a scenario author: at a 1 s mobility step the same speed gives 36,111 mm per step, ABOVE the escape threshold, so every moving actor would take the absolute escape every step. That is correct behaviour, not a defect, but it makes the delta encoding pointless at that cadence.) Exceeding the range triggers the escape rather than wrapping: `PoseRef::step` checks `|d| > 32_000` before the `as i16` cast, so dx = 32,000 is carried literally and dx = 32,001 sets MFLAG_ABSOLUTE with an absolute-block entry and d_mm = [0,0,0]. Driven end to end through the encoder: a 32.001 m jump in one step produced exactly one absolute-block entry with x_mm = 64,001 and reconstructed exactly (conformance Q3).

## Seek latency, independently measured

p95 = 1.763 ms warm (target 100 ms) — a 57x margin. PASS, and the builder's headline is conservative rather than wrong.

My own measurement, `cargo test --release` on the pinned 1.98.1, macOS arm64, 8 cores, on a quiet machine, with my own recording, my own LCG for the targets and type-7 interpolation for the quantile (not nearest-rank, so the number is not a ranking artefact):

  recording: 9,001 frames, 7.3 MiB on disk (49.1 MiB uncompressed), 13 chunks, 600 keyframes, 400 actors, 600 s of simulated time, written in 420 ms
  warm, one paged reader, 200 targets: p50 1.620 ms, p95 1.763 ms, p99 2.240 ms, max 3.314 ms
  cold, a fresh `open_paged` per seek, 30 targets: p50 1.727 ms, p95 1.961 ms, max 2.009 ms
  resident (`Reader::open`), 200 targets: p50 1.576 ms, p95 1.691 ms, max 2.220 ms

Against §7.4 and conformance P2: warm p95 1.763 ms ≤ 100 ms, warm max 3.314 ms ≤ 200 ms, cold max 2.009 ms ≤ 150 ms. All three pass with two orders of magnitude to spare.

Whose number is right: both are, and the builder was honest about why they differ. They reported warm p95 = 3.494 ms taken while 15 other cargo processes were building, and noted a quiet-machine run at p95 1.854 ms. My 1.763 ms is within noise of their quiet figure, which confirms the 3.494 ms was scheduler contention rather than anything algorithmic. Reporting the loaded figure as the headline was the right call; the true steady-state p95 is a little under 2 ms.

Two structural claims I checked rather than took on trust. The seek path is genuinely paged: `Reader::open_paged` reads the 20-byte footer, the summary section and each chunk's message-index records, then preads only the chunk bytes a seek needs, and the cold variant re-does all of that per seek yet costs only ~0.2 ms more than the warm one — consistent with a small summary, not with reading a 7 MiB file. And chunk locality is real: every one of the 200 warm seeks read exactly 1 chunk (max 1, mean 1.00), because the recorder closes a chunk before each keyframe, so §7.3 step 7's two-chunk case never arises for files this crate writes; the reader still handles it and is capped at two. Conformance P4 also holds: max 9 deltas returned against the cadence's budget of 10, and every `keyframe_time ≤ t`.

## Robustness

FAIL. This is the one area where the crate's stated guarantee does not hold. `error.rs`'s module documentation says "Nothing here panics on malformed input: a truncated file, a frame whose `body_len` lies, a chunk index pointing past the end of the file and a zstd frame that will not decompress all come back as a named variant. That is a requirement, not a courtesy." Two of those five cases abort the process.

What DOES work, all verified: an empty, one-byte or non-MCAP file; bad magic at either end ("malformed mcap file: the file does not start and end with the MCAP magic"); truncation at 31 evenly spaced cut points (all rejected, none panicking); a footer whose `summary_start` is zero (`NoSummary`), absurd, or inside the data section; a chunk record's length prefix lying by ±1, ±1e6 or truncated to nothing (no panic); a chunk's `uncompressed_size` set to u64::MAX, 2^62, 2^40 or 0 (all clean errors: "MCAP file ended in the middle of a record", "Chunk CRC failed"); a bad `uncompressed_crc`; a `compression` string length of u32::MAX or 0; a `records_size` of u64::MAX, 2^40 or 0; a zstd payload truncated to half ("Error during decompression: Data corruption detected"); a VWP Event carrying an unknown plug-in channel id (recorded on `vwp/event/plugin.4242`, round-trips byte-identically, C3 satisfied); an MCAP message on a channel the summary does not declare (`RecordError::Malformed("message on undeclared channel N")` by inspection); a delta with no preceding keyframe (clean `Inconsistent`, and `seek` gives `SeekOutOfRange`); and every wire decoder I fed an absurd count — Telemetry, MetricSample, Event, Hello and StrTable all bound-check before touching memory and return `Truncated` for a 32-byte body declaring 4,294,967,295 records.

What does NOT: see finding 1. One bit flipped inside a chunk's zstd payload, or a chunk header's `uncompressed_size` set to 1, reaches mcap 0.25's `decompress_inner` unvalidated and produces either an unrecoverable 7-to-8-exabyte allocation failure (SIGABRT, not catchable) or an arithmetic overflow panic. This is exactly the failure mode the module documentation calls out as unacceptable — "a recording is read after a crash more often than before one" — and it is not an upstream problem the crate cannot address, because the chunk index already carries the authoritative `uncompressed_size` the crate could cross-check against.

## Metrics and statistics

MOSTLY PASS — the statistics are correct and honest; two discipline claims are weaker than stated.

Wilson score interval: CORRECT. I computed reference bounds at 50 decimal digits independently (Python `decimal`, `centre = (p̂ + z²/2n)/(1 + z²/n)`, `half = z/(1 + z²/n)·sqrt(p̂(1−p̂)/n + z²/4n²)`) for eight cases spanning all three levels and both boundaries: 25/100 P95, 0/10 P95, 10/10 P95, 3/10 P95, 1/3 P95, 7/13 P90, 2/47 P99, 500/1000 P95. Worst absolute deviation 1.110e-16, i.e. under one ULP. The z constants are the published two-sided quantiles. The clamp to [0,1] is real and documented; 10/10 gives 0.99999999999999989, which the 1e-6 probability grid resolves to 1.0.

Percentiles: CORRECT AND STATED. Hyndman & Fan type 7 at h = (n−1)q, re-derived by hand on an 11-value sample: p50, p95 and p99 all match to under 1e-15, mean is exactly 44/11, and the rule is named in every `DistributionSummary` and in the Arrow `interpolation` column.

Hand-derived metrics on my own fixtures, all exact: comms — 40 candidate receptions, 30 ok / 10 lost (6 collision, 4 sinr) gives pdr = 0.75 (n=40), per = 0.25, pdr_by_cause collision = 0.6 and sinr = 0.4, and the pdr interval (0.59806, 0.858129) equals `wilson_interval(30, 40, P95)` quantised on the probability grid; cbr over {0.1,0.2,0.3,0.4} gives mean 0.25, p50 0.25, p95 0.385 — all type-7 correct. Detection — a declared population of 20 with 5 acting attackers, 4 of them reported plus 3 benign, gives tp=4 fp=3 fn=1 tn=12, recall 0.8, fpr 0.2, precision 0.5714 (4/7 on the 1e-4 grid), accuracy 0.8, F1 0.6667 (8/12), and F1 is an `Estimate` with no interval attached, as the design argues it must be.

Division by zero, NaN, single samples: mostly closed. Zero, NaN and infinite denominators and empty proportions all report `Insufficient` rather than dividing; 1/1 at the default threshold of 30 reports `Insufficient { trials: 1, required: 30 }` and never a delivery ratio of one; a whole empty run's 50 samples carry 5 floats, none non-finite. The one hole is a non-finite NUMERATOR (finding 9).

Diagnostics: the three documented mechanisms work — `DigestSet::partition` removes them, `metric_digest` refuses the list by name, and changing a diagnostic's value does not move `RunSummary::digest`. The fourth path, `RunSummary::file_digest`, does not (finding 5).

Invariants: I injected one violation for each of the ten checks. I-R3 (a loss with no cause), I-N1 (one frame in two buckets), I-M1 (mobility out of ActorId order), I-P4 (`published` before `issued`), I-T2 (a GT-tainted record on a NODE channel), I-T3 (an on-air attack naming no message), I-S1 (a size disagreement between crypto modes), I-C1 (a one-byte record difference) and I-N2 (a fragmenter card with no declared timeout) all detect theirs, and all hold on the clean fixture. D9 detects a raw float but not a value quantised to the wrong grid (finding 7).

Binning: PASS. All eight ULPs either side of the 25 m edge land in bin 1 together with 25.0 exactly; 0.1+24.9, 5×5 and 75/3 all agree; a value genuinely below the edge (24.9994) falls to bin 0 while one that rounds onto the grid point (24.9995) falls to bin 1, which is the documented D10 semantics. TimeBins is pure integer nanosecond arithmetic.

## Findings

### F1 [HIGH] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/index.rs:464

**Problem.** A corrupt recording ABORTS THE PROCESS or panics. The crate's error module promises "Nothing here panics on malformed input ... a zstd frame that will not decompress all come back as a named variant. That is a requirement, not a courtesy". It is false. `for_each_message_in_chunk` passes the chunk record's own header straight to `mcap::read::ChunkReader::new` without validating it, and mcap 0.25's `decompress_inner` then (a) calls `dest_buf.reserve_exact(n)` with a wire-controlled `uncompressed_size` and (b) does `*compressed_remaining -= res.consumed as u64` unchecked. Two proved reproductions on a `fixture::write_recording(RunShape::new(4,30))` file: (1) `bytes[457] ^= 0x01` — a single bit flip INSIDE the first chunk's zstd payload (payload starts at file offset 355) — then `Reader::open_bytes(bytes)?.verify()` gives `memory allocation of 7016996765293425217 bytes failed` -> SIGABRT, which `catch_unwind` cannot trap; (2) setting the chunk record's `uncompressed_size` (file offset 327..335) to 1 gives `attempt to subtract with overflow` at mcap-0.25.0/src/sans_io/linear_reader.rs:864 in debug and `memory allocation of 8104075796848577545 bytes failed` in release. Reachable from every read path: `verify`, `replay`, `records`, `seek`, `content_digest`, on both MemorySource and FileSource. An exhaustive single-byte-flip sweep over the whole file aborts the test binary. `tests/corrupt.rs` misses it because it only mutates bytes it computes to be inside the compressed payload region and never touches the chunk record header, and because in the few payload bytes it does touch the CRC happens to fire first.

**Fix.** Before calling `ChunkReader::new`, validate the chunk record's decoded `header.uncompressed_size` against the value the chunk index already gave you (`ChunkSpan::uncompressed_size`, in hand at that point) and against a ceiling derived from the file size or `chunk_target_bytes`; reject a mismatch with `RecordError::Malformed`. Do the same for the chunk's `records_size` against the bytes actually read. Add a fuzz-style test that flips every byte of a fixture recording and asserts every read path returns a `RecordError`.

**Proved:** True

### F2 [HIGH] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/profile.rs:140

**Problem.** Conformance V5 is NOT satisfied, and the `NODE-only` stripper leaks ground truth. The live node producer blanks BEFORE quantisation (`SnapshotEncoder::blank_input`), so lane and ST_ATTACKER are already gone when `encode_delta`'s `changed` predicate (encoder.rs:484) decides whether to emit a moved row. `NodeProfileStripper::blank_delta` blanks AFTER, and keeps any moved row whose slot is occupied. Proved: a parked actor whose quantised pose never changes, whose lane changes at step 3 and whose ST_ATTACKER bit is set at step 6. Live `node` run emits 0 moved rows at those steps; the stripper emits 1 moved row with dx=dy=dz=0, mflags=0 and every other field equal to the previous frame. 'parked actor, lane change only': 1 of 10 frames differ; 'attacker bit only': 1 of 10; 'both': 2 of 10. Two consequences: (a) the builder's claim that a `full` recording replayed under `node` is byte-identical to a live `node` run is false in general — it holds only because every fixture actor moves every step; (b) the surviving all-zero row carries no information EXCEPT 'a §5.2 ground-truth field changed at this step', which is exactly what the profile exists to withhold, so it also weakens V1. The same divergence applies to any actor stopped at a red light that changes lane, or to any actor whose attack status toggles while stationary.

**Fix.** Give `NodeProfileStripper` a per-slot blanked reference (it already tracks `occupied`) and, after blanking a moved row, drop it when it is a no-op against that reference: dx=dy=dz==0, mflags==0, and state/verified_neighbors/heading_brad/speed_cq/accel_cq equal to the previously transmitted blanked values. Extend `tests/node_profile.rs` with a stationary actor whose lane and ST_ATTACKER change.

**Proved:** True

### F3 [HIGH] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/writer.rs:330

**Problem.** Build decision D9 is violated inside the recording itself. `write_frame` calls `crate::grid::scan_frame` (writer.rs:256); `write_record` calls nothing — it stores `record.json` verbatim. D9 says 'No floating-point value reaches a RECORDED, exported or digested artefact in raw IEEE-754 form.' Proved by reading a fixture recording back with `Reader::records(None)` and scanning: 7 distinct off-grid float fields are stored raw, all from the crate's own `fixture::records` — phy.rx.distance_m = 123.4567891 (declared grid 1e-3), phy.rx.rssi_dbm = -78.123456 (1e-2), phy.rx.sinr_db = 12.987654 (1e-2), mac.cbr.cbr = 0.123456789 (1e-4), node.tx.airtime_ms = 0.5263571 (1e-3), det.observation.score = 0.87654321 (1e-6), metric.sample.value = 0.9123456789 (1e-6). `Reader::content_digest` hashes those same bytes, so the third D9 category ('digested') is affected too. The exporters quantise on the way out, which hides the problem from `export::scan`, but the recording is itself a recorded artefact and is what a replay, a re-export and a run digest are taken from.

**Fix.** In `write_record`, run the record's JSON through the same declared-grid quantiser the exporters use (`export::schema::declared_quantum` per key, walking nested values), or refuse an off-grid record with `RecordError::OffGrid` exactly as `write_frame` refuses an off-grid frame. Fix `fixture::records` to emit on-grid values once the gate exists.

**Proved:** True

### F4 [MEDIUM] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/export/table.rs:88

**Problem.** Nested and array floats bypass the writer-side quantiser in all three exporters, and the grid scan is structurally blind to them, so `scan_export` reports OK. `TableSchema::infer` types a JSON object or array as `ColumnKind::Text`; `build_batch` then writes `other.to_string()` (table.rs:88) and `jsonl::write` writes `v.clone()` (jsonl.rs:62), both copying the raw digits through. `scan_batches` (scan.rs:53) and `scan_jsonl` (scan.rs:139) both `continue` on any column that is not `ColumnKind::Float`. Proved with a `node.tx` record {"airtime_ms":0.5263571234, "detail":{"distance_m":123.4567891234}, "samples_m":[1.234567891,2.345678912]}: the exported JSONL row is {"airtime_ms":0.526,"detail":{"distance_m":123.4567891234},"node_id":1,"samples_m":[1.234567891,2.345678912],"sim_time_ns":0} — the flat float is quantised to 1e-3, the nested ones are raw — and `scan_export` returned ScanReport { files: 1, rows: 3, values: 3 } with no error for Parquet, Arrow IPC and JSONL alike. ADR 0004 §7 asks for 'a test scans every output file for any value that is off its grid'; this scan checks only the columns the schema happened to type as Float.

**Fix.** Recurse into objects and arrays in `build_batch` and `jsonl::write` and quantise every float found (naming the grid from the innermost key), and make `scan_batches`/`scan_jsonl` parse Text columns' JSON and check their numbers too. Alternatively refuse a record that infers a Text column containing numbers, so the hole cannot open silently.

**Proved:** True

### F5 [MEDIUM] `v2xw-metrics` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-metrics/src/summary.rs:278

**Problem.** A runtime diagnostic DOES reach a digested artefact, through `RunSummary::file_digest`. The module documents the exclusion as 'structural, not a convention' and names three mechanisms (private `DigestSet` field, `DigestSet::partition`, `metric_digest`'s refusal) — all three work. But `RunSummary` serialises both `metrics` and `diagnostics`, and `file_digest` (summary.rs:278) hashes `to_canonical_json(self)` (summary.rs:269), the whole document, into a `v2xw_core::manifest::FileDigest` — which is precisely the digest a run manifest records for the summary file. Proved: two `RunSummary`s identical except for `events_per_second` (1.0 vs 987654.0) have the same protected `.digest`, and `file_digest("metrics/summary.json").sha256` = 731af2c9e1d85bc58214a1a6395eeaca89a4d4135c0bec69e53f3ee17bd89540 vs fd25580b35b336d241326041712260904d425b217196faa8b27c56fb590cd793. Two machines running the same scenario will disagree on the manifest's FileDigest for a reason that has nothing to do with the simulation — the exact failure the module's own documentation says it prevents.

**Fix.** Either compute `file_digest` over the digested subset only (a second canonical encoding of `{schema, metrics, digest, insufficient}`), or write the diagnostics to a separate sidecar whose digest is recorded separately, or state on `file_digest`'s docstring that it is not comparable across machines and give callers a `digested_file_digest` that is.

**Proved:** True

### F6 [MEDIUM] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/wire/mod.rs:278

**Problem.** Forward compatibility is refused rather than ignored, breaking conformance F6 and N1 and §8.6. Two defects. (1) `Frame::header()` returns `RecordError::Malformed` when the header's reserved u16 is non-zero. F6 requires 'Reserved header and prefix bytes are written as zero and IGNORED ON READ', and §8.5 says a minor version adds fields 'in bytes that a v1 reader is already required to ignore: a `reserved` field'. Every read path goes through `header()`, so a v1.1 frame that uses the header's reserved word is unreadable rather than degraded. Proved: setting bytes 14..16 of a well-formed Telemetry frame to 0x0001 makes `Frame::from_bytes` fail with 'malformed vwp frame header: reserved = 1, must be written as zero'. (2) `RecordingWriter::write_frame` (writer.rs:221) refuses an unknown `msg_type` ('message type 0x0009 is unknown to this build') and `Reader::verify` (reader.rs:375) turns the same condition into `RecordError::Inconsistent`. §8.4 says 'A new message type id in the reserved range. Readers ignore unknown msg_type' and §8.6 says the replay reader 'accepts a higher minor, ignoring what it does not know'. `tests/conformance.rs::a_synthetic_v1_1_stream_parses_and_loses_only_what_is_new` only checks that such a FRAME decodes; it never puts one through the recorder or the reader, so it passes while N1 fails end to end.

**Fix.** Make the reserved-header check read-tolerant (keep writing zero, drop the read-side rejection, or demote it to a counted warning in `verify`). Have `write_frame` store an unknown `msg_type` on a `vwp/unknown.<id>` topic instead of refusing it, and have `verify` count it as `unknown_frames` rather than failing. Then extend the N1 test to a full write/read round trip.

**Proved:** True

### F7 [MEDIUM] `v2xw-metrics` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-metrics/src/invariants.rs:1071

**Problem.** `check_d9_quantisation` does not check what it says it checks. The guard is `if !s.quantum.holds(f) && !Quantum::PROBABILITY.holds(f)`. `Quantum::PROBABILITY` is 1e-6, the FINEST grid in the crate, and any value on a coarser declared grid is also on the 1e-6 grid, so the second disjunct swallows the first and the check degenerates to 'is this on the 1e-6 grid'. The exemption was intended for confidence bounds; its effect is to disable the declared-grid check for every float in every sample. Proved by injecting a violation for each of the ten invariant checks: I-R3, I-N1, I-M1, I-P4, I-T2, I-T3, I-S1, I-C1, I-N2 and D9-raw-float all detect theirs; a `pdr` sample (declared Quantum::RATIO = 1e-4) whose value is 0.333333 is off its declared grid and the check holds. A genuinely raw 1.0/3.0 is still caught, so the check is not useless — it just cannot detect the failure mode 'a provider quantised to the wrong grid', which is the most likely one once `MetricSample::new` is bypassed.

**Fix.** Exempt only the two bounds the type already distinguishes rather than every float: have `SampleValue::floats` return `(f64, Quantum)` pairs, giving `RatioEstimate::Proportion::ci_lo`/`ci_hi` Quantum::PROBABILITY and everything else the sample's own quantum, then drop the disjunction.

**Proved:** True

### F8 [LOW] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/writer.rs:444

**Problem.** `RecordingSummary::chunk_count` is short by one whenever the recording ends with an attachment or a metadata record. `attach` (writer.rs:422) and `write_manifest` set `bytes_since_flush = 0` without incrementing `chunk_count`, and mcap 0.25's `attach`/`write_metadata` both finish the current chunk first; `finish()` (writer.rs:444) then only counts a final chunk when `bytes_since_flush > 0`. Proved: `RunShape { actors: 60, steps: 600 }` with `chunk_target_bytes = 64 KiB` — the summary reports 11 chunks while `SeekIndex::chunks` holds 12. The 9001-frame seek fixture reports 12 against 13. Cosmetic in itself, but the summary is the number a caller would report or assert on.

**Fix.** Increment `chunk_count` in `attach` and `write_manifest` when `bytes_since_flush > 0` before resetting it, or drop the field and derive the count from the chunk index on read.

**Proved:** True

### F9 [LOW] `v2xw-metrics` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-metrics/src/stats.rs:513

**Problem.** `ratio_of_sums` guards the denominator but not the numerator, so a `RatioEstimate` can carry `point: NaN`. Proved: `ratio_of_sums(f64::NAN, 2.0, 10, 1)` returns `RatioOfSums { point: NaN, numerator: NaN, denominator: 2.0, n: 10 }`. `MetricSample::new` then quantises it — `quantize_to` passes non-finite through by design — and `check_d9_quantisation` accepts it, because `is_on_grid` is true for every non-finite value. The module's rule 5 reads 'Nothing divides by zero silently ... both answer Insufficient for a zero (or non-finite) denominator', and `Distribution::observe` does refuse non-finite inputs, so the guard is inconsistent rather than absent. I could not reach it from any provider in the crate (every numerator is an integer count or a `sum_ordered` over already-finite samples), which is why this is low rather than medium; the builder's claim that a whole run's output is scanned for NaN covers the fixture, not this API.

**Fix.** Add `|| !numerator.is_finite()` to the guard in `ratio_of_sums`, and have `Estimate::Value`/`RatioEstimate::Proportion` refuse non-finite points at construction the way `Distribution::observe` already does.

**Proved:** True

### F10 [LOW] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/fixture.rs:355

**Problem.** Conformance C5 ('every prov_id referenced by a MetricSample or an event payload has been delivered in a Provenance frame before it is first referenced') is neither implemented nor satisfied by the crate's own fixture. `Reader::verify` does not check it. `fixture::metric_frame` emits MetricSample rows carrying `prov_id: 1` and `prov_id: 2`, and `fixture::live_frames` never writes a single Provenance frame; `verify()` returns Ok. C5 is marked `S` (server) in §10.4, so the check may legitimately live above this crate, but the fixture currently models a non-conformant producer and is the thing the conformance kit and v2xw-server's replay tests are told to reuse.

**Fix.** Either add a Provenance frame to `fixture::live_frames` before the first MetricSample that references a prov_id, or set the fixture's prov_id to 0 ('none'). Add a `verify` check that every referenced prov_id was delivered earlier in the stream, and say in the report which conformance items this crate does and does not cover.

**Proved:** True

### F11 [LOW] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/writer.rs:256

**Problem.** `chunk_target_bytes` is not an upper bound on chunk size, because the writer ends a chunk only immediately before a keyframe and `bound_chunk`'s safety valve is disabled the moment a chunk contains one. A chunk is therefore at least one whole GOP, and grows linearly with `keyframe_period / mobility_step`. Measured at 400 actors and a 100 ms step: a 1 s keyframe period gives a 4.0 MiB largest chunk (on target), a 60 s period gives 4.7 MiB, and a 600 s period would give roughly 48 MiB. §7.4's latency budget is written against 'zstd decompress 1-2 chunks ... 2 x 4 MiB', and §7.3 step 7 explicitly contemplates a GOP spanning a chunk boundary ('if the GOP spans a chunk boundary, the next chunk'), so §7.1's actual requirement — 'a chunk MUST NOT start in the middle of a GOP's keyframe' — does not demand the stronger rule the writer enforces. Harmless at the default 1 s cadence; a scenario that lengthens the keyframe period silently leaves the budget.

**Fix.** Relax the rule to §7.1's literal one — never split a keyframe record — and allow a chunk to end between deltas once it exceeds the target, since the reader already handles a two-chunk GOP. Or keep the rule and clamp it: refuse a cadence whose GOP would exceed `chunk_target_bytes` at the declared actor capacity.

**Proved:** True

### F12 [INFO] `v2xw-record` /Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-record/src/export/schema.rs:163

**Problem.** `TableSchema::infer` types a column as `ColumnKind::Int` whenever every observed value happens to be integral, because the test is `n.is_f64() && !n.is_i64() && !n.is_u64()`. A `cbr` column whose window happened to contain only 0 and 1 is exported as Int64 with `quantum: None` and drops out of the grid scan entirely, while the same channel in the next window exports as Float64. No D9 violation follows (every integer sits on every 1e-N grid), but a consumer reading two windows of the same channel gets two different Parquet schemas. The docstring already argues for inference over a hard-coded field list; this is the cost of that choice and is not currently stated.

**Fix.** Either seed the inferred kind from `declared_quantum`'s unit suffix (a `_m`, `_db`, `_ratio` or `_mps` name is a float column whatever values a window happened to hold), or document the instability on `TableSchema::infer` and in the exported schema.json.

**Proved:** True

