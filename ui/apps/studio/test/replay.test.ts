/**
 * The no-engine replay path: `crates/v2xw-wasm` driven from the Studio.
 *
 * The WebAssembly module itself is not built here — it needs a clang with a `wasm32` target, which
 * is a property of the recording container's C codecs and not of this app. What *is* tested is
 * everything on this side of the binding, which is where the mistakes would be:
 *
 *  * the ask-and-retry range loop, including the two ways it can fail to terminate;
 *  * that the recorded `Keyframe` and `Delta` frames reach a `PoseBuffer` through the *same*
 *    decoder the socket path uses — the rule 09-ui §7 states, and the one a "just read the columns"
 *    shortcut would break silently;
 *  * that a seek resets the buffer first, so a backwards seek does not land deltas on the previous
 *    GOP's base (§3.4).
 *
 * The fake reader below mirrors `crates/v2xw-wasm/src/js.rs` member for member, and its "linear
 * memory" is a real `ArrayBuffer` with real frames in it at real offsets, so the pointer arithmetic
 * under test is the arithmetic that runs in the browser.
 */

import { describe, expect, it } from "vitest";

import { deltaFrame, keyframeFrame, type ActorRowInit } from "@vwp/protocol";

import {
  LocalReplay,
  drive,
  httpRangeFetcher,
  loadReplayBindings,
  type ReplayBindings,
  type ReplayReaderHandle,
} from "../src/lib/replay.js";

const NS = 1_000_000_000;

function actor(id: number, xMm: number, yMm: number): ActorRowInit {
  return {
    actorId: id,
    xMm,
    yMm,
    laneId: 7,
    zCm: 120,
    headingBrad: 16_384,
    speedCq: 128 * 12,
    accelCq: 0,
    classIdx: 0,
    state: 1,
    verifiedNeighbors: 3,
  };
}

/** One GOP: a keyframe at `t`, then `steps` deltas that only advance the signal block. */
function recordedGop(tNs: number, actors: readonly ActorRowInit[], steps: number): { keyframe: ArrayBuffer; deltas: ArrayBuffer[] } {
  const keyframe = keyframeFrame(
    {
      simTimeNs: BigInt(tNs),
      originXM: 0,
      originYM: 0,
      originZM: 0,
      gopIndex: 4,
      actors,
      signals: [{ signalId: 11, timeToChangeDs: 50, phase: 1 }],
    },
    1n,
  );
  const deltas: ArrayBuffer[] = [];
  for (let i = 1; i <= steps; i++) {
    deltas.push(
      deltaFrame(
        {
          simTimeNs: BigInt(tNs + i * 100_000_000),
          gopIndex: 4,
          stepIndex: i,
          signals: [{ signalId: 11, timeToChangeDs: 50 - i, phase: 1 }],
        },
        BigInt(1 + i),
      ),
    );
  }
  return { keyframe, deltas };
}

/** A `ReplayReaderHandle` over a real byte buffer, standing in for WebAssembly linear memory. */
function fakeReader(options: {
  keyframe: ArrayBuffer;
  deltas: readonly ArrayBuffer[];
  spanNs?: [number, number];
  positionNs?: number;
  /** Ranges to demand before the first `prepare()`/`seekNs()` succeeds. */
  needs?: readonly (readonly [number, number])[];
}): { reader: ReplayReaderHandle; bindings: ReplayBindings; supplied: { offset: number; len: number }[] } {
  const frames = [options.keyframe, ...options.deltas];
  const total = frames.reduce((sum, f) => sum + f.byteLength, 0);
  // One buffer, frames laid out back to back on an 8-byte grid, exactly as linear memory would be.
  const stride = 8;
  const offsets: number[] = [];
  let at = 0;
  for (const frame of frames) {
    offsets.push(at);
    at += Math.ceil(frame.byteLength / stride) * stride;
  }
  const memory = { buffer: new ArrayBuffer(at) };
  const bytes = new Uint8Array(memory.buffer);
  frames.forEach((frame, i) => bytes.set(new Uint8Array(frame), offsets[i]));

  const supplied: { offset: number; len: number }[] = [];
  let outstanding = [...(options.needs ?? [])];
  const reader: ReplayReaderHandle = {
    prepare: () => takeNeeds(),
    supply: (offset, chunk) => {
      supplied.push({ offset, len: chunk.length });
    },
    isOpen: true,
    seek: () => takeNeeds(),
    seekNs: () => takeNeeds(),
    keyframeTimeNs: BigInt(options.spanNs?.[0] ?? 0),
    positionNs: BigInt(options.positionNs ?? options.spanNs?.[0] ?? 0),
    deltaCount: options.deltas.length,
    chunksRead: 1,
    actorCount: 2,
    signalCount: 1,
    generation: 1,
    origin: new Float64Array([0, 0, 0]),
    spanStartNs: BigInt(options.spanNs?.[0] ?? 0),
    spanEndNs: BigInt(options.spanNs?.[1] ?? 0),
    requests: supplied.length,
    residentBytes: total,
    totalBytes: total,
    keyframePtr: offsets[0],
    keyframeLen: options.keyframe.byteLength,
    deltaFrames: options.deltas.length,
    deltaPtr: (i) => offsets[i + 1],
    deltaLen: (i) => options.deltas[i]?.byteLength ?? 0,
  };

  function takeNeeds(): Float64Array {
    if (outstanding.length === 0) return new Float64Array(0);
    const flat = outstanding.flatMap(([offset, len]) => [offset, len]);
    outstanding = [];
    return new Float64Array(flat);
  }

  return { reader, bindings: { ReplayReader: { fromBytes: () => reader, ranged: () => reader }, wasmMemory: () => memory }, supplied };
}

describe("drive — the ask-and-retry range loop", () => {
  it("returns 0 rounds when nothing is wanted", async () => {
    const reader = { supply: () => undefined };
    const rounds = await drive(() => new Float64Array(0), () => Promise.resolve(new Uint8Array(1)), reader);
    expect(rounds).toBe(0);
  });

  it("fetches every range it is asked for, then stops", async () => {
    const asked: [number, number][] = [];
    const supplied: number[] = [];
    let round = 0;
    const step = (): Float64Array => (round++ === 0 ? new Float64Array([0, 8, 4096, 64]) : new Float64Array(0));
    const rounds = await drive(
      step,
      (offset, len) => {
        asked.push([offset, len]);
        return Promise.resolve(new Uint8Array(len));
      },
      { supply: (offset) => void supplied.push(offset) },
    );
    expect(rounds).toBe(1);
    expect(asked).toEqual([
      [0, 8],
      [4096, 64],
    ]);
    expect(supplied).toEqual([0, 4096]);
  });

  it("refuses a fetcher that returns nothing, rather than spinning", async () => {
    // Without this the loop would ask for the same range forever: the reader cannot make progress
    // on zero bytes, so it re-asks, and the fetcher re-answers with nothing.
    await expect(
      drive(() => new Float64Array([0, 8]), () => Promise.resolve(new Uint8Array(0)), { supply: () => undefined }),
    ).rejects.toThrow(/returned nothing/);
  });

  it("gives up after the round cap", async () => {
    await expect(
      drive(() => new Float64Array([0, 8]), (_o, len) => Promise.resolve(new Uint8Array(len)), { supply: () => undefined }, 3),
    ).rejects.toThrow(/did not converge in 3 rounds/);
  });

  it("ignores a trailing half-pair, which a malformed range list would produce", async () => {
    const asked: number[] = [];
    let round = 0;
    const step = (): Float64Array => (round++ === 0 ? new Float64Array([16, 32, 64]) : new Float64Array(0));
    await drive(
      step,
      (offset, len) => {
        asked.push(offset);
        return Promise.resolve(new Uint8Array(len));
      },
      { supply: () => undefined },
    );
    expect(asked).toEqual([16]);
  });
});

describe("httpRangeFetcher", () => {
  it("widens a small ask and clamps it to the file", async () => {
    const seen: string[] = [];
    const fake = ((_url: unknown, init?: { headers?: Record<string, string> }) => {
      seen.push(init?.headers?.Range ?? "");
      return Promise.resolve({ status: 206, arrayBuffer: () => Promise.resolve(new ArrayBuffer(8)) });
    }) as unknown as typeof fetch;
    // 8 bytes wanted, 64 KiB fetched — the footer read alone asks for a handful of bytes, and the
    // container is chunk-indexed precisely so a scrub is a few round trips rather than a dozen.
    const fetcher = httpRangeFetcher("http://x.test/run.mcap", fake, 1000);
    await fetcher(0, 8);
    expect(seen[0]).toBe("bytes=0-999");
  });

  it("refuses a server that answers a range request with something other than 206 or 200", async () => {
    const fake = (() => Promise.resolve({ status: 416, arrayBuffer: () => Promise.resolve(new ArrayBuffer(0)) })) as unknown as typeof fetch;
    await expect(httpRangeFetcher("http://x.test/run.mcap", fake)(0, 8)).rejects.toThrow(/answered 416/);
  });
});

describe("loadReplayBindings", () => {
  const stub = (): ReplayBindings["ReplayReader"] => {
    const ctor = function ReplayReader(): void {
      /* the class is never constructed in these tests */
    } as unknown as ReplayBindings["ReplayReader"];
    (ctor as unknown as { fromBytes: () => void }).fromBytes = () => undefined;
    (ctor as unknown as { ranged: () => void }).ranged = () => undefined;
    return ctor;
  };

  it("runs the initialiser with the 0.2.128 object form", async () => {
    const calls: unknown[] = [];
    const mod = {
      default: (arg?: unknown) => {
        calls.push(arg);
        return Promise.resolve(undefined);
      },
      ReplayReader: stub(),
      wasmMemory: () => ({ buffer: new ArrayBuffer(0) }),
    };
    await loadReplayBindings({ scriptUrl: "/wasm/v2xw_replay.js" }, () => Promise.resolve(mod));
    expect(calls).toEqual([{ module_or_path: "/wasm/v2xw_replay_bg.wasm" }]);
  });

  it("falls back to the bare-URL form for older glue", async () => {
    const calls: unknown[] = [];
    const mod = {
      default: (arg?: unknown) => {
        calls.push(arg);
        if (calls.length === 1) return Promise.reject(new Error("deprecated parameters"));
        return Promise.resolve(undefined);
      },
      ReplayReader: stub(),
      wasmMemory: () => ({ buffer: new ArrayBuffer(0) }),
    };
    await loadReplayBindings({ scriptUrl: "/wasm/v2xw_replay.js", wasmUrl: "/wasm/x.wasm" }, () => Promise.resolve(mod));
    expect(calls).toEqual([{ module_or_path: "/wasm/x.wasm" }, "/wasm/x.wasm"]);
  });

  it("says how to build the reader when the module is not there", async () => {
    await expect(
      loadReplayBindings({ scriptUrl: "/wasm/v2xw_replay.js" }, () => Promise.reject(new Error("404"))),
    ).rejects.toThrow(/build-wasm\.sh --web/);
  });

  it("refuses a module that loaded but exports the wrong thing", async () => {
    await expect(
      loadReplayBindings({ scriptUrl: "/wasm/v2xw_replay.js" }, () => Promise.resolve({ something: "else" })),
    ).rejects.toThrow(/exports no ReplayReader/);
  });
});

describe("LocalReplay", () => {
  it("decodes the recorded frames through the protocol decoder into the pose buffer", async () => {
    const gop = recordedGop(12 * NS, [actor(101, 1_500, -2_500), actor(102, 4_000, 4_000)], 3);
    // Integer nanoseconds throughout: `BigInt(12.3 * 1e9)` throws, because 12.3 has no exact
    // double and the product is not an integer. Simulated time is an identity (§6.5), so it is
    // written as one.
    const { bindings } = fakeReader({ ...gop, spanNs: [0, 60 * NS], positionNs: 12_300_000_000 });
    const replay = new LocalReplay();
    const span = await replay.openBlob({ name: "run.mcap", arrayBuffer: () => Promise.resolve(new ArrayBuffer(0)) }, bindings);
    expect(span).toEqual({ startNs: 0, endNs: 60 * NS });

    const position = await replay.seekToNs(12_300_000_000);
    expect(position.deltasApplied).toBe(3);
    expect(position.refused).toEqual([]);
    // The keyframe's actor ids and quantised columns, straight from §3.3.2 — not recomputed here.
    expect(replay.poses.count).toBe(2);
    expect([...replay.poses.actorId.slice(0, 2)]).toEqual([101, 102]);
    expect(replay.poses.xMm[0]).toBe(1_500);
    expect(replay.poses.yMm[1]).toBe(4_000);
    // §3.2 dequantisation is the decoder's, so metres follow from millimetres without a second rule.
    expect(replay.poses.positionOf(0).x).toBeCloseTo(1.5, 6);
    expect(replay.poses.occupied[0]).toBe(1);
    // The signal block of the keyframe the seek started from, for `Viewer.applyKeyframe`.
    expect(replay.signals?.count).toBe(1);
    expect(replay.signals?.signalId[0]).toBe(11);
  });

  it("resets the buffer before each seek, so a second seek cannot inherit the first one's step index", async () => {
    // §3.4: a delta is refused unless `step_index` is exactly one past the last applied one. Without
    // the reset, the second seek's deltas would all be refused — the recording would appear to
    // freeze on its keyframes, which is the subtle wrong behaviour this pins.
    const gop = recordedGop(12 * NS, [actor(1, 0, 0)], 2);
    const { bindings } = fakeReader({ ...gop, spanNs: [0, 60 * NS] });
    const replay = new LocalReplay();
    await replay.openBlob({ arrayBuffer: () => Promise.resolve(new ArrayBuffer(0)) }, bindings);
    const first = await replay.seekToNs(12 * NS);
    const second = await replay.seekToNs(12 * NS);
    expect(first.deltasApplied).toBe(2);
    expect(second.deltasApplied).toBe(2);
  });

  it("drives the range loop when the reader asks for bytes", async () => {
    const gop = recordedGop(NS, [actor(1, 0, 0)], 0);
    const { bindings, supplied } = fakeReader({ ...gop, spanNs: [0, NS], needs: [[0, 4096]] });
    const replay = new LocalReplay();
    const fetched: number[] = [];
    const fake = ((_url: unknown, init?: { method?: string }) => {
      if (init?.method === "HEAD") {
        return Promise.resolve({ ok: true, headers: { get: () => "8192" } });
      }
      fetched.push(1);
      return Promise.resolve({ status: 206, arrayBuffer: () => Promise.resolve(new ArrayBuffer(4096)) });
    }) as unknown as typeof fetch;
    await replay.openUrl("http://x.test/run.mcap", bindings, fake);
    expect(supplied).toEqual([{ offset: 0, len: 4096 }]);
    expect(fetched).toHaveLength(1);
  });

  it("refuses a URL whose server does not report a length, because the footer is read from the end", async () => {
    const gop = recordedGop(NS, [actor(1, 0, 0)], 0);
    const { bindings } = fakeReader({ ...gop, spanNs: [0, NS] });
    const fake = (() => Promise.resolve({ ok: true, headers: { get: () => null } })) as unknown as typeof fetch;
    await expect(new LocalReplay().openUrl("http://x.test/run.mcap", bindings, fake)).rejects.toThrow(/content-length/);
  });

  it("throws when no recording is open", async () => {
    await expect(new LocalReplay().seekToNs(0)).rejects.toThrow(/no recording is open/);
  });

  it("closes cleanly and reports itself closed", async () => {
    const gop = recordedGop(NS, [actor(1, 0, 0)], 0);
    const { bindings } = fakeReader({ ...gop, spanNs: [0, NS] });
    const replay = new LocalReplay();
    await replay.openBlob({ arrayBuffer: () => Promise.resolve(new ArrayBuffer(0)) }, bindings);
    expect(replay.isOpen).toBe(true);
    replay.close();
    expect(replay.isOpen).toBe(false);
    expect(replay.span).toBeNull();
  });
});
