// The seek-latency benchmark for the WebAssembly replay reader.
//
//   cargo run -p v2xw-wasm --release --example mkbench -- target/wasm-bench
//   crates/v2xw-wasm/scripts/build-wasm.sh
//   node crates/v2xw-wasm/bench/seek.mjs
//
// vwp-v1 §7.4 and 09-ui §7 set the budget: 100 ms at the 95th percentile, held by a
// benchmark rather than by an argument, because no published seek-latency numbers exist
// for MCAP viewers. `mkbench` writes the recording, measures the *native* build over a
// fixed list of 200 targets and records both in `bench.json`; this script measures the
// WebAssembly build over the same targets on the same file, so the two numbers differ
// only by the compilation.
//
// The clock is `performance.now()`, and it lives here rather than in the library on
// purpose: nothing in `v2xw-wasm` reads a wall clock, because a wall-clock read in
// engine-facing code destroys reproducibility (ADR 0004). A latency benchmark is the
// exception that proves the rule.

import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { performance } from 'node:perf_hooks';

import { views } from '../js/replay.js';

const require = createRequire(import.meta.url);
const root = path.resolve(import.meta.dirname, '../../..');
const pkgDir = process.env.V2XW_WASM_PKG ?? path.join(root, 'target/wasm-pkg');
const benchDir = process.env.V2XW_WASM_BENCH ?? path.join(root, 'target/wasm-bench');

const { ReplayReader, wasmMemory } = require(path.join(pkgDir, 'v2xw_replay.js'));
const manifest = JSON.parse(readFileSync(path.join(benchDir, 'bench.json'), 'utf8'));
const bytes = readFileSync(path.join(benchDir, manifest.file));

/** Nearest-rank, which is what a pass criterion over a fixed sample means. */
function quantileNearest(sorted, q) {
  if (sorted.length === 0) return 0;
  const rank = Math.round(q * (sorted.length - 1));
  return sorted[Math.min(rank, sorted.length - 1)];
}

/** Hyndman & Fan type 7, the interpolating quantile this project reports. */
function quantileType7(sorted, q) {
  if (sorted.length === 0) return 0;
  const h = q * (sorted.length - 1);
  const lo = Math.floor(h);
  const hi = Math.min(lo + 1, sorted.length - 1);
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (h - lo);
}

const targets = manifest.targets_ns.map((t) => BigInt(t));

// ── Cold: the first seek on a reader that has only just read its index ────────────────
const coldStart = performance.now();
const cold = ReplayReader.fromBytes(bytes);
const coldOpenMs = performance.now() - coldStart;
const coldSeekStart = performance.now();
if (cold.seekNs(targets[0]).length !== 0) throw new Error('a resident recording waited');
const coldSeekMs = performance.now() - coldSeekStart;

// ── Warm: the same 200 targets the native benchmark measured ──────────────────────────
const reader = ReplayReader.fromBytes(bytes);
for (let i = 0; i < 20; i += 1) reader.seekNs(targets[i]);

const samples = new Float64Array(targets.length);
let sink = 0;
for (let i = 0; i < targets.length; i += 1) {
  const at = performance.now();
  const pending = reader.seekNs(targets[i]);
  const took = performance.now() - at;
  if (pending.length !== 0) throw new Error('a resident recording waited for bytes');
  samples[i] = took;
  // Touch the result so no engine can decide the seek was dead code.
  sink += reader.actorCount + Number(reader.positionNs % 7n);
}
samples.sort();

// A column read is the work a renderer does next; measured separately because it is the
// zero-copy claim, not the seek.
const viewStart = performance.now();
let columnSink = 0;
for (let i = 0; i < 100; i += 1) {
  const v = views(reader, wasmMemory());
  columnSink += v.xMm[v.actorCount - 1] + v.state[0];
}
const viewMs = (performance.now() - viewStart) / 100;

const p95Nearest = quantileNearest(samples, 0.95);
const p95Type7 = quantileType7(samples, 0.95);
const p50 = quantileType7(samples, 0.5);
const max = samples[samples.length - 1];
const budgetMs = 100;

const report = {
  file: manifest.file,
  bytes: manifest.bytes,
  chunks: manifest.chunks,
  keyframes: manifest.keyframes,
  seeks: samples.length,
  wasm_p50_ms: Number(p50.toFixed(4)),
  wasm_p95_nearest_ms: Number(p95Nearest.toFixed(4)),
  wasm_p95_type7_ms: Number(p95Type7.toFixed(4)),
  wasm_max_ms: Number(max.toFixed(4)),
  wasm_cold_open_ms: Number(coldOpenMs.toFixed(4)),
  wasm_cold_seek_ms: Number(coldSeekMs.toFixed(4)),
  wasm_column_view_ms: Number(viewMs.toFixed(4)),
  native_p95_nearest_ms: manifest.native_p95_nearest_ms,
  native_p95_type7_ms: manifest.native_p95_type7_ms,
  native_max_ms: manifest.native_max_ms,
  budget_ms: budgetMs,
  margin: Number((budgetMs / p95Nearest).toFixed(1)),
  slowdown_vs_native: Number((p95Nearest / manifest.native_p95_nearest_ms).toFixed(2)),
  node: process.version,
  sink: sink + columnSink,
};

console.log(JSON.stringify(report, null, 2));
if (p95Nearest > budgetMs) {
  console.error(`FAIL p95 ${p95Nearest.toFixed(3)} ms is over the ${budgetMs} ms budget`);
  process.exit(1);
}
