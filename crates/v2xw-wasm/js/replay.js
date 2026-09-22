// Ergonomic helpers over the `wasm-bindgen` surface of `v2xw-wasm`.
//
// The generated bindings are deliberately thin: numbers, and pointers into the
// WebAssembly linear memory. This file is the part that would otherwise be written once
// per caller — building typed-array views over those pointers, and driving the
// ask-and-retry loop that fetches a recording a range at a time.
//
// It imports nothing, so the same file serves the `--target web` and `--target nodejs`
// builds; the caller passes in the bindings module and its `memory`.

/** Column layouts: the typed array each pointer should be read through, and its stride. */
const ACTOR_COLUMNS = [
  ['actorId', 'actorIdPtr', Uint32Array],
  ['xMm', 'xMmPtr', Int32Array],
  ['yMm', 'yMmPtr', Int32Array],
  ['zMm', 'zMmPtr', Int32Array],
  ['laneId', 'laneIdPtr', Uint32Array],
  ['headingBrad', 'headingPtr', Uint16Array],
  ['speedCq', 'speedPtr', Int16Array],
  ['accelCq', 'accelPtr', Int16Array],
  ['classIdx', 'classIdxPtr', Uint8Array],
  ['state', 'statePtr', Uint8Array],
  ['verifiedNeighbors', 'verifiedNeighborsPtr', Uint8Array],
];

const SIGNAL_COLUMNS = [
  ['signalId', 'signalIdPtr', Uint32Array],
  ['signalTtcDs', 'signalTtcPtr', Uint16Array],
  ['signalPhase', 'signalPhasePtr', Uint8Array],
];

/**
 * Typed-array views over the reader's pose columns, with no copy.
 *
 * Every view aliases the WebAssembly linear memory directly, which is the whole point of
 * the wire format's struct-of-arrays layout (vwp-v1 §3.3.2): a renderer can hand
 * `xMm`/`yMm` to `bufferSubData` without a parsing step.
 *
 * The views go stale when `reader.generation` changes — a column grew, or linear memory
 * did and detached the old `ArrayBuffer`. `views()` stamps the generation it was built at
 * so a caller can check cheaply; `viewsFor()` rebuilds only when it has to.
 *
 * @param {object} reader a `ReplayReader`
 * @param {WebAssembly.Memory} memory the module's memory
 */
export function views(reader, memory) {
  const buf = memory.buffer;
  const actors = reader.actorCount;
  const signals = reader.signalCount;
  const out = { generation: reader.generation, actorCount: actors, signalCount: signals };
  for (const [name, ptr, Kind] of ACTOR_COLUMNS) {
    out[name] = new Kind(buf, reader[ptr], actors);
  }
  for (const [name, ptr, Kind] of SIGNAL_COLUMNS) {
    out[name] = new Kind(buf, reader[ptr], signals);
  }
  return out;
}

/** True if `v` was built for this reader's current buffers. */
export function viewsFresh(reader, memory, v) {
  return (
    v !== undefined &&
    v.generation === reader.generation &&
    v.actorCount === reader.actorCount &&
    v.signalCount === reader.signalCount &&
    v.actorId.buffer === memory.buffer
  );
}

/** `views(reader, memory)`, reusing `previous` when it is still valid. */
export function viewsFor(reader, memory, previous) {
  return viewsFresh(reader, memory, previous) ? previous : views(reader, memory);
}

/**
 * A zero-copy view of the recorded `Keyframe` frame the last seek returned, header and
 * all, exactly as the live engine put it on the wire (vwp-v1 §7.2).
 */
export function keyframeFrame(reader, memory) {
  return new Uint8Array(memory.buffer, reader.keyframePtr, reader.keyframeLen);
}

/** Zero-copy views of the recorded `Delta` frames, in ascending time order. */
export function deltaFrames(reader, memory) {
  const out = [];
  for (let i = 0; i < reader.deltaFrames; i += 1) {
    out.push(new Uint8Array(memory.buffer, reader.deltaPtr(i), reader.deltaLen(i)));
  }
  return out;
}

/**
 * Runs one ask-and-retry step function to completion.
 *
 * `step()` returns a flat `Float64Array` of `[offset, len, …]`: empty means done,
 * non-empty means fetch those ranges. `fetchRange(offset, len)` returns the bytes.
 *
 * @param {() => Float64Array} step
 * @param {(offset: number, len: number) => Promise<Uint8Array>|Uint8Array} fetchRange
 * @param {object} reader
 * @param {number} [maxRounds] a guard against a fetcher that returns nothing
 */
export async function drive(step, fetchRange, reader, maxRounds = 512) {
  for (let round = 0; round < maxRounds; round += 1) {
    const ranges = step();
    if (ranges.length === 0) return round;
    for (let i = 0; i < ranges.length; i += 2) {
      const offset = ranges[i];
      const len = ranges[i + 1];
      const bytes = await fetchRange(offset, len);
      if (bytes.length === 0) {
        throw new Error(`the fetcher returned nothing for ${len} bytes at ${offset}`);
      }
      reader.supply(offset, bytes);
    }
  }
  throw new Error(`the range loop did not converge in ${maxRounds} rounds`);
}

/** Reads the index, fetching whatever it needs. */
export function open(reader, fetchRange) {
  return drive(() => reader.prepare(), fetchRange, reader);
}

/** Seeks to `seconds` of simulated time, fetching whatever it needs. */
export function seek(reader, seconds, fetchRange) {
  return drive(() => reader.seek(seconds), fetchRange, reader);
}

/**
 * An HTTP range fetcher for a recording served over plain HTTP.
 *
 * The server must answer `Range` requests; the container is chunk-indexed precisely so
 * that it can (vwp-v1 §7.1).
 */
export function httpRangeFetcher(url, fetchImpl = fetch) {
  return async (offset, len) => {
    const end = offset + len - 1;
    const res = await fetchImpl(url, { headers: { Range: `bytes=${offset}-${end}` } });
    if (res.status !== 206) {
      throw new Error(`${url} answered ${res.status} to a range request, not 206`);
    }
    return new Uint8Array(await res.arrayBuffer());
  };
}

/** The recording's size in bytes, from a `HEAD`. */
export async function contentLength(url, fetchImpl = fetch) {
  const res = await fetchImpl(url, { method: 'HEAD' });
  const len = res.headers.get('content-length');
  if (len === null) throw new Error(`${url} did not report a content-length`);
  return Number(len);
}
