// Node smoke test: the WebAssembly replay reader against the native one, on one file.
//
//   crates/v2xw-wasm/scripts/build-wasm.sh
//   cargo run -p v2xw-wasm --example mkfixture -- target/wasm-fixture
//   node crates/v2xw-wasm/tests/node/smoke.mjs
//
// `mkfixture` writes a recording and, beside it, the **native** build's answers about it:
// every seek's keyframe time, resolved position, delta count and every resolved pose
// column. This script asks the WebAssembly build the same questions and compares, value
// for value. It is an equality comparison and not a tolerance one, because vwp-v1 §3.2's
// delta reference rule makes keyframe-plus-deltas land exactly on the transmitted
// integers rather than near them.
//
// It runs the ArrayBuffer path and the range-request path, so the second one is held to
// the same answers as the first.

import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import process from 'node:process';

import { views, keyframeFrame, deltaFrames, drive } from '../../js/replay.js';

const require = createRequire(import.meta.url);
const root = path.resolve(import.meta.dirname, '../../../..');
const pkgDir = process.env.V2XW_WASM_PKG ?? path.join(root, 'target/wasm-pkg');
const fixtureDir = process.env.V2XW_WASM_FIXTURE ?? path.join(root, 'target/wasm-fixture');

const bindings = require(path.join(pkgDir, 'v2xw_replay.js'));
const { ReplayReader, wasmMemory } = bindings;

const expected = JSON.parse(readFileSync(path.join(fixtureDir, 'expected.json'), 'utf8'));
const bytes = readFileSync(path.join(fixtureDir, expected.file));

let checks = 0;
let failures = 0;

function fail(message) {
  failures += 1;
  console.error(`  FAIL ${message}`);
}

function eq(got, want, what) {
  checks += 1;
  if (got !== want) fail(`${what}: got ${got}, expected ${want}`);
}

function eqColumn(got, want, what) {
  checks += 1;
  if (got.length !== want.length) {
    fail(`${what}: ${got.length} values, expected ${want.length}`);
    return;
  }
  for (let i = 0; i < want.length; i += 1) {
    if (got[i] !== want[i]) {
      fail(`${what}[${i}]: got ${got[i]}, expected ${want[i]}`);
      return;
    }
  }
}

/** Compares one seek's whole answer with the native build's. */
function compare(reader, want, label) {
  eq(Number(reader.keyframeTimeNs), want.keyframe_time_ns, `${label} keyframe time`);
  eq(Number(reader.positionNs), want.position_ns, `${label} position`);
  eq(reader.deltaCount, want.deltas, `${label} delta count`);
  eq(reader.chunksRead, want.chunks_read, `${label} chunks read`);
  eq(reader.actorCount, want.actor_count, `${label} actor count`);
  eq(reader.signalCount, want.signal_count, `${label} signal count`);

  const origin = reader.origin;
  for (let i = 0; i < 3; i += 1) {
    eq(origin[i], want.origin[i], `${label} origin[${i}]`);
  }

  // Zero copy: every column below is a view over the WebAssembly linear memory at the
  // pointer the reader reported, not a copy handed across the boundary.
  const v = views(reader, wasmMemory());
  eqColumn(v.actorId, want.actor_id, `${label} actor_id`);
  eqColumn(v.xMm, want.x_mm, `${label} x_mm`);
  eqColumn(v.yMm, want.y_mm, `${label} y_mm`);
  eqColumn(v.zMm, want.z_mm, `${label} z_mm`);
  eqColumn(v.laneId, want.lane_id, `${label} lane_id`);
  eqColumn(v.headingBrad, want.heading_brad, `${label} heading_brad`);
  eqColumn(v.speedCq, want.speed_cq, `${label} speed_cq`);
  eqColumn(v.accelCq, want.accel_cq, `${label} accel_cq`);
  eqColumn(v.classIdx, want.class_idx, `${label} class_idx`);
  eqColumn(v.state, want.state, `${label} state`);
  eqColumn(v.verifiedNeighbors, want.verified_neighbors, `${label} verified_neighbors`);
  eqColumn(v.signalId, want.signal_id, `${label} signal_id`);
  eqColumn(v.signalTtcDs, want.signal_ttc_ds, `${label} signal_ttc_ds`);
  eqColumn(v.signalPhase, want.signal_phase, `${label} signal_phase`);

  const kf = keyframeFrame(reader, wasmMemory());
  eq(kf.length, want.keyframe_frame_len, `${label} keyframe frame length`);
  eq(deltaFrames(reader, wasmMemory()).length, want.deltas, `${label} delta frame count`);

  // The views alias linear memory rather than copying out of it: writing through the
  // Rust side is the only writer, so the proof is that the pointer is inside the memory
  // and the length matches the reported actor count. Anything else would be a copy with
  // extra steps.
  checks += 1;
  if (v.xMm.buffer !== wasmMemory().buffer) {
    fail(`${label} x_mm is a copy, not a view over linear memory`);
  }
}

console.log(`wasm package: ${pkgDir}`);
console.log(
  `recording:    ${expected.file} (${expected.bytes} bytes, ${expected.chunks} chunks, ${expected.seeks.length} seek targets)`,
);
eq(expected.chunks > 4, true, 'the fixture is more than a handful of chunks');

// ── 1. The ArrayBuffer path ───────────────────────────────────────────────────────────
{
  const reader = ReplayReader.fromBytes(bytes);
  eq(reader.isOpen, true, 'a resident recording is open on construction');
  eq(Number(reader.spanStartNs), expected.span_start_ns, 'span start');
  eq(Number(reader.spanEndNs), expected.span_end_ns, 'span end');
  eq(reader.prepare().length, 0, 'a resident recording never waits for bytes');

  for (const want of expected.seeks) {
    const pending = reader.seekNs(BigInt(want.t_ns));
    eq(pending.length, 0, `resident seek to ${want.t_ns} did not wait`);
    compare(reader, want, `resident t=${want.t_ns}`);
  }
  console.log('  resident path: ok');
}

// ── 2. The range-request path ─────────────────────────────────────────────────────────
{
  const reader = ReplayReader.ranged(bytes.length);
  let requests = 0;
  let fetched = 0;
  const fetchRange = (offset, len) => {
    requests += 1;
    fetched += Math.min(len, bytes.length - offset);
    // Exactly what an HTTP 206 would hand back for `Range: bytes=offset-(offset+len-1)`.
    return bytes.subarray(offset, Math.min(offset + len, bytes.length));
  };

  await drive(() => reader.prepare(), fetchRange, reader);
  eq(reader.isOpen, true, 'the ranged reader opened');
  const afterOpen = fetched;

  for (const want of expected.seeks) {
    await drive(() => reader.seekNs(BigInt(want.t_ns)), fetchRange, reader);
    compare(reader, want, `ranged t=${want.t_ns}`);
  }

  // The claim: a scrub is a set of lookups, not a download. `residentBytes` is the
  // distinct bytes held, so an overlapping re-fetch cannot inflate it.
  checks += 1;
  if (reader.residentBytes >= bytes.length) {
    fail(
      `the range path holds ${reader.residentBytes} of ${bytes.length} bytes, which is the whole file`,
    );
  }
  console.log(
    `  range path:    ok (${requests} requests, ${reader.residentBytes} of ${bytes.length} bytes resident,`
      + ` ${afterOpen} fetched to open the index)`,
  );
}

// ── 3. A refusal is a refusal ─────────────────────────────────────────────────────────
{
  checks += 1;
  try {
    ReplayReader.fromBytes(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]));
    fail('eight arbitrary bytes were accepted as a recording');
  } catch (e) {
    if (!/mcap|magic|truncat/i.test(String(e))) {
      fail(`a bad file was refused, but with an unhelpful message: ${e}`);
    }
  }
  console.log('  refusals:      ok');
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
