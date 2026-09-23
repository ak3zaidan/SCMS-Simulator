/**
 * The replay path with no engine at all: `crates/v2xw-wasm` driven from the Studio.
 *
 * 09-ui §7 fixes the rule this module keeps:
 *
 * > The replay reader is the same Rust code compiled natively (served by
 * > `v2xw serve --replay file.mcap`) or to WASM (static hosting), and emits the identical VWP
 * > stream, so the Studio has no replay-specific code path.
 *
 * So there is no second decoder here. The WebAssembly reader hands back the recorded `Keyframe`
 * and `Delta` frames **byte for byte** as the live engine put them on the wire (`keyframePtr` /
 * `deltaPtr` in `crates/v2xw-wasm/src/js.rs`, vwp-v1 §7.2), and those go through the same
 * `decodeMessage` and the same `PoseBuffer.applyKeyframe` / `applyDelta` the socket path uses. What
 * this module adds is the two things a browser needs and a file system does not: loading the
 * `wasm-bindgen` module, and driving the ask-and-retry range loop.
 *
 * # Why the frames and not the columns
 *
 * The reader also exposes its resolved pose columns as pointers into linear memory, which is
 * cheaper. It is not used here: those columns would need a second copy of the dequantisation rules
 * of §3.2, and two implementations of §3.2 is exactly the thing 09-ui §7 forbids. A recording's
 * keyframe is a few hundred kilobytes and a seek touches one of them, so the copy is affordable and
 * the arithmetic stays in one place. `test/replay.test.ts` pins that the frames, not the columns,
 * are what reaches the pose buffer.
 *
 * # What a recording does not carry
 *
 * §7.1 keeps `WorldChunk` out of a recording, and `ReplayEngine::open` in `crates/v2xw-server`
 * takes the world as a separate argument for the same reason. So a recording opened here has poses
 * and signals but no world geometry until one is supplied — from the run it is being compared
 * against, or from a `.vwb` file beside the recording. {@link LocalReplay} reports that rather than
 * drawing actors over an implied empty city.
 */

import {
  ProtocolError,
  decodeMessage,
  type DeltaApplyResult,
  type KeyframeMessage,
  PoseBuffer,
} from "@vwp/protocol";

/**
 * The `ReplayReader` class of `crates/v2xw-wasm/src/js.rs`, as `wasm-bindgen` exposes it.
 *
 * Every member here is transcribed from that file: a `Vec<f64>` return crosses as a
 * `Float64Array`, a `SimTime` (`u64`) as a `bigint`, and a `*_ptr` getter as a byte offset into the
 * module's linear memory. Nothing is assumed — a name that is not in `js.rs` is not here.
 */
export interface ReplayReaderHandle {
  /** Reads the footer, the summary and the message indexes. Empty once the index is in hand. */
  prepare(): Float64Array;
  /** Hands back a fetched range. */
  supply(offset: number, bytes: Uint8Array): void;
  readonly isOpen: boolean;
  /** Seeks to `seconds`. Empty once the columns hold the state at that instant. */
  seek(seconds: number): Float64Array;
  /** {@link seek} with the target in nanoseconds. */
  seekNs(tNs: bigint): Float64Array;
  readonly keyframeTimeNs: bigint;
  readonly positionNs: bigint;
  readonly deltaCount: number;
  readonly chunksRead: number;
  readonly actorCount: number;
  readonly signalCount: number;
  readonly generation: number;
  readonly origin: Float64Array;
  readonly spanStartNs: bigint;
  readonly spanEndNs: bigint;
  readonly requests: number;
  readonly residentBytes: number;
  readonly totalBytes: number;
  readonly keyframePtr: number;
  readonly keyframeLen: number;
  readonly deltaFrames: number;
  deltaPtr(i: number): number;
  deltaLen(i: number): number;
  /** `wasm-bindgen` puts this on every exported class. */
  free?(): void;
}

/** The module `crates/v2xw-wasm/scripts/build-wasm.sh --web` writes as `v2xw_replay.js`. */
export interface ReplayBindings {
  readonly ReplayReader: {
    fromBytes(bytes: Uint8Array): ReplayReaderHandle;
    ranged(totalBytes: number): ReplayReaderHandle;
  };
  /** The module's `WebAssembly.Memory`; a function because growth detaches a cached buffer. */
  readonly wasmMemory: () => { readonly buffer: ArrayBuffer };
}

/** Where to find the built reader. */
export interface ReplayModuleLocation {
  /** URL of `v2xw_replay.js` from the `--web` build. */
  readonly scriptUrl: string;
  /** URL of `v2xw_replay_bg.wasm`; defaults to the script URL with the name substituted. */
  readonly wasmUrl?: string;
}

/**
 * Default location of the built reader.
 *
 * `scripts/build-wasm.sh --web apps/studio/public/wasm` puts the two files where Vite serves them
 * verbatim, which is the whole deployment story for the no-server path: a static directory.
 */
export const DEFAULT_REPLAY_LOCATION: ReplayModuleLocation = {
  scriptUrl: "/wasm/v2xw_replay.js",
  wasmUrl: "/wasm/v2xw_replay_bg.wasm",
};

/** How many round trips the range loop will make before it gives up. */
const MAX_ROUNDS = 512;

/** The default fetch window: ranges smaller than this are widened, to cut round trips. */
const MIN_RANGE_BYTES = 64 * 1024;

/**
 * Whether a loaded module really is the reader's binding surface.
 *
 * Checked rather than assumed because the URL is a runtime string: a stale file, a 404 page served
 * with a JavaScript content type, or a `--target nodejs` build dropped into the web directory all
 * load without throwing and then fail on first use with something unhelpful. `ReplayReader` is a
 * `wasm-bindgen` class, so it is a function with two statics.
 *
 * Each step narrows from `unknown`, which is the one form of `typeof` narrowing with no ambiguity;
 * asking `typeof x === "function"` about a value already declared as an object type is where this
 * kind of guard gets interesting for the wrong reasons.
 */
function isReplayBindings(value: unknown): value is ReplayBindings {
  if (typeof value !== "object" || value === null) return false;
  const mod = value as Record<string, unknown>;
  if (typeof mod.wasmMemory !== "function") return false;
  const reader = mod.ReplayReader;
  if (typeof reader !== "function") return false;
  const statics = reader as unknown as Record<string, unknown>;
  return typeof statics.fromBytes === "function" && typeof statics.ranged === "function";
}

/**
 * Load the `wasm-bindgen` module and run its initialiser.
 *
 * The import specifier is a runtime URL, so it is marked `@vite-ignore`: the reader is built by
 * `cargo` and dropped into a static directory, and a bundler that tried to resolve it would fail
 * the Studio's build on a machine with no WebAssembly toolchain. That is also why the failure is a
 * plain `Error` with the build command in it — "the reader is not built" is the common case, not a
 * bug.
 *
 * `wasm-bindgen` 0.2.128 takes `{module_or_path}`; older glue takes the URL directly. Both are
 * tried, in that order.
 */
export async function loadReplayBindings(
  location: ReplayModuleLocation = DEFAULT_REPLAY_LOCATION,
  importer: (url: string) => Promise<unknown> = (url) => import(/* @vite-ignore */ url),
): Promise<ReplayBindings> {
  let mod: unknown;
  try {
    mod = await importer(location.scriptUrl);
  } catch (err) {
    throw new Error(
      `the WebAssembly replay reader is not at ${location.scriptUrl} ` +
        `(build it with crates/v2xw-wasm/scripts/build-wasm.sh --web apps/studio/public/wasm): ` +
        `${err instanceof Error ? err.message : String(err)}`,
    );
  }
  const candidate = mod as { default?: unknown; init?: unknown };
  const init = typeof candidate.default === "function" ? candidate.default : candidate.init;
  if (typeof init === "function") {
    const wasmUrl = location.wasmUrl ?? location.scriptUrl.replace(/\.js$/, "_bg.wasm");
    const run = init as (arg?: unknown) => Promise<unknown>;
    try {
      await run({ module_or_path: wasmUrl });
    } catch {
      // Pre-0.2.93 glue, which takes the URL itself. A second failure is the caller's to see.
      await run(wasmUrl);
    }
  }
  if (!isReplayBindings(mod)) {
    throw new Error(`${location.scriptUrl} loaded but exports no ReplayReader/wasmMemory pair`);
  }
  return mod;
}

/** Fetches `len` bytes at `offset`. */
export type RangeFetcher = (offset: number, len: number) => Promise<Uint8Array>;

/**
 * A `Range:` fetcher over a URL, widened to {@link MIN_RANGE_BYTES}.
 *
 * The reader asks for exactly what it needs, which for a footer is 8 bytes; asking the network for
 * 8 bytes eleven times to open one file is the cost the container's chunk index exists to avoid.
 * Widening is safe because `supply` records what arrived at the offset it arrived at, and the
 * reader re-asks for anything it still lacks.
 */
export function httpRangeFetcher(url: string, fetchImpl: typeof fetch = fetch, totalBytes = Number.POSITIVE_INFINITY): RangeFetcher {
  return async (offset, len) => {
    const want = Math.max(len, MIN_RANGE_BYTES);
    const limit = Number.isFinite(totalBytes) ? Math.min(offset + want, totalBytes) : offset + want;
    const end = limit - 1;
    const res = await fetchImpl(url, { headers: { Range: `bytes=${offset}-${end}` } });
    if (res.status !== 206 && res.status !== 200) {
      throw new Error(`${url} answered ${res.status} to a range request, not 206`);
    }
    return new Uint8Array(await res.arrayBuffer());
  };
}

/**
 * Run one ask-and-retry step to completion.
 *
 * `step()` returns `[offset, len, …]`: empty means done. Exported because it is the contract the
 * crate's own `js/replay.js` documents, and `test/replay.test.ts` drives it against a fake reader.
 */
export async function drive(
  step: () => Float64Array,
  fetchRange: RangeFetcher,
  reader: Pick<ReplayReaderHandle, "supply">,
  maxRounds = MAX_ROUNDS,
): Promise<number> {
  for (let round = 0; round < maxRounds; round++) {
    const ranges = step();
    if (ranges.length === 0) return round;
    for (let i = 0; i + 1 < ranges.length; i += 2) {
      const offset = ranges[i];
      const len = ranges[i + 1];
      const bytes = await fetchRange(offset, len);
      if (bytes.length === 0) throw new Error(`the fetcher returned nothing for ${len} bytes at ${offset}`);
      reader.supply(offset, bytes);
    }
  }
  throw new Error(`the range loop did not converge in ${maxRounds} rounds`);
}

/** Where a completed seek landed, and what it cost. */
export interface ReplayPosition {
  /** Simulated time the pose buffer is resolved to. */
  readonly tNs: number;
  /** The keyframe the seek started from. */
  readonly keyframeNs: number;
  /** Deltas the reader returned. */
  readonly deltas: number;
  /** Deltas the pose buffer actually applied; a shortfall means a refused frame (§3.4). */
  readonly deltasApplied: number;
  /** Container chunks read and decompressed. */
  readonly chunksRead: number;
  /** Range requests issued so far, cumulative. */
  readonly requests: number;
  readonly residentBytes: number;
  readonly totalBytes: number;
  /** Why a delta was refused, when one was. */
  readonly refused: readonly DeltaApplyResult[];
}

/** The recorded span, in nanoseconds of simulated time. */
export interface ReplaySpan {
  readonly startNs: number;
  readonly endNs: number;
}

/**
 * One recording, open in the browser, with no engine behind it.
 *
 * The pose buffer this exposes is the same {@link PoseBuffer} a live connection fills, so
 * `Viewer.capture(replay.poses)` works unchanged — which is the point of §7.2's byte-identity
 * guarantee.
 */
export class LocalReplay {
  /** Poses at the current seek position, in the same buffer shape the socket path uses. */
  readonly poses: PoseBuffer;

  #reader: ReplayReaderHandle | null = null;
  #bindings: ReplayBindings | null = null;
  #fetchRange: RangeFetcher;
  #label = "";
  #lastPosition: ReplayPosition | null = null;
  #signals: KeyframeMessage["signals"] | null = null;

  /**
   * @param poseCapacity initial slot capacity; a keyframe with more actors grows it (§3.3.1).
   */
  constructor(poseCapacity = 1024) {
    this.poses = new PoseBuffer(poseCapacity);
    this.#fetchRange = () => Promise.reject(new Error("no recording is open"));
  }

  /** Whether a recording is open and indexed. */
  get isOpen(): boolean {
    return this.#reader !== null && this.#reader.isOpen;
  }

  /** The file or URL this reader was opened on. */
  get label(): string {
    return this.#label;
  }

  /** The last completed seek, or `null`. */
  get position(): ReplayPosition | null {
    return this.#lastPosition;
  }

  /** The signal block of the keyframe the last seek started from, for `Viewer.applyKeyframe`. */
  get signals(): KeyframeMessage["signals"] | null {
    return this.#signals;
  }

  /** The recorded span, or `null` before the index is read. */
  get span(): ReplaySpan | null {
    const reader = this.#reader;
    if (reader === null || !reader.isOpen) return null;
    return { startNs: Number(reader.spanStartNs), endNs: Number(reader.spanEndNs) };
  }

  /**
   * Open a recording the browser already holds — an `<input type="file">` pick, or a drop.
   *
   * The whole file is read into WebAssembly memory, which is what `ReplayReader.fromBytes` takes; a
   * reader opened this way never asks for a range, so the same seek path serves both.
   */
  async openBlob(file: { name?: string; arrayBuffer(): Promise<ArrayBuffer> }, bindings: ReplayBindings): Promise<ReplaySpan> {
    const bytes = new Uint8Array(await file.arrayBuffer());
    this.#adopt(bindings, bindings.ReplayReader.fromBytes(bytes), file.name ?? "recording", () =>
      Promise.reject(new Error("a recording opened from bytes should never ask for a range")),
    );
    await drive(() => this.#require().prepare(), this.#fetchRange, this.#require());
    return this.#span();
  }

  /**
   * Open a recording served over HTTP, a range at a time.
   *
   * The size comes from a `HEAD`, which is what `ReplayReader.ranged` needs: the container is read
   * back-to-front from its footer.
   */
  async openUrl(url: string, bindings: ReplayBindings, fetchImpl: typeof fetch = fetch): Promise<ReplaySpan> {
    const head = await fetchImpl(url, { method: "HEAD" });
    if (!head.ok) throw new Error(`${url} answered ${head.status} to a HEAD`);
    const header = head.headers.get("content-length");
    if (header === null) throw new Error(`${url} did not report a content-length, so it cannot be range-read`);
    const total = Number(header);
    if (!Number.isFinite(total) || total <= 0) throw new Error(`${url} reported a content-length of ${header}`);
    this.#adopt(bindings, bindings.ReplayReader.ranged(total), url, httpRangeFetcher(url, fetchImpl, total));
    await drive(() => this.#require().prepare(), this.#fetchRange, this.#require());
    return this.#span();
  }

  /**
   * Seek to `tNs` and leave {@link poses} holding the state at that instant.
   *
   * The pose buffer is reset first, so the keyframe seeds a buffer with no keyframe rather than one
   * from a different GOP: `applyDelta` refuses a `gop_index` mismatch (§3.4) and a stale
   * `step_index` would make every delta after a backwards seek a no-op.
   */
  async seekToNs(tNs: number): Promise<ReplayPosition> {
    const reader = this.#require();
    const target = BigInt(Math.max(0, Math.round(tNs)));
    await drive(() => reader.seekNs(target), this.#fetchRange, reader);

    const memory = this.#bindings?.wasmMemory();
    if (memory === undefined) throw new Error("no recording is open");

    this.poses.reset();
    this.#signals = null;
    const kfLen = reader.keyframeLen;
    if (kfLen === 0) throw new ProtocolError("bad_state", `the reader returned no Keyframe for t = ${tNs} ns`, { field: "keyframe" });
    const keyframe = decodeMessage(copyOut(memory.buffer, reader.keyframePtr, kfLen));
    if (keyframe.kind !== "keyframe") {
      throw new ProtocolError("bad_state", `the reader's keyframe frame decoded as ${keyframe.kind}`, { field: "keyframe" });
    }
    this.poses.applyKeyframe(keyframe);
    this.#signals = keyframe.signals;

    const refused: DeltaApplyResult[] = [];
    let applied = 0;
    const frames = reader.deltaFrames;
    for (let i = 0; i < frames; i++) {
      const len = reader.deltaLen(i);
      if (len === 0) continue;
      const message = decodeMessage(copyOut(memory.buffer, reader.deltaPtr(i), len));
      if (message.kind !== "delta") {
        throw new ProtocolError("bad_state", `delta frame ${i} decoded as ${message.kind}`, { field: "delta" });
      }
      const outcome = this.poses.applyDelta(message);
      if (outcome.applied) applied++;
      else refused.push(outcome);
    }

    const position: ReplayPosition = {
      tNs: Number(reader.positionNs),
      keyframeNs: Number(reader.keyframeTimeNs),
      deltas: reader.deltaCount,
      deltasApplied: applied,
      chunksRead: reader.chunksRead,
      requests: reader.requests,
      residentBytes: reader.residentBytes,
      totalBytes: reader.totalBytes,
      refused,
    };
    this.#lastPosition = position;
    return position;
  }

  /** Release the reader. The pose buffer is kept, so the last frame stays on screen. */
  close(): void {
    try {
      this.#reader?.free?.();
    } catch {
      /* a double free is the binding's problem, not ours */
    }
    this.#reader = null;
    this.#bindings = null;
    this.#label = "";
    this.#lastPosition = null;
    this.#signals = null;
    this.#fetchRange = () => Promise.reject(new Error("no recording is open"));
  }

  #adopt(bindings: ReplayBindings, reader: ReplayReaderHandle, label: string, fetchRange: RangeFetcher): void {
    this.close();
    this.#bindings = bindings;
    this.#reader = reader;
    this.#label = label;
    this.#fetchRange = fetchRange;
  }

  #require(): ReplayReaderHandle {
    const reader = this.#reader;
    if (reader === null) throw new Error("no recording is open");
    return reader;
  }

  #span(): ReplaySpan {
    const span = this.span;
    if (span === null) throw new Error("the recording's index was read but reports no span");
    return span;
  }
}

/**
 * Copy `len` bytes out of WebAssembly memory into an `ArrayBuffer` of exactly that length.
 *
 * `decodeMessage` takes an `ArrayBuffer` and reads the frame from offset 0, and a view into linear
 * memory has neither property. The copy is also what makes the decode safe against a later `supply`
 * growing memory and detaching the view.
 */
function copyOut(buffer: ArrayBuffer, pointer: number, len: number): ArrayBuffer {
  const out = new Uint8Array(len);
  out.set(new Uint8Array(buffer, pointer, len));
  return out.buffer;
}
