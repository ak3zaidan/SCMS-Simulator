/** §1 transport, §1.4 reconnect/resume, §1.5 backpressure, §6 JSON-RPC — driven by a fake socket. */

import { describe, expect, it, vi } from "vitest";

import {
  type HelloMessage,
  JsonRpcClient,
  JsonRpcError,
  RpcErrorCode,
  SeqRing,
  VWP_METHODS,
  VWP_NOTIFICATIONS,
  VwpClient,
  type WebSocketLike,
  deltaFrame,
  keyframeFrame,
  helloFrame,
  FrameFlags,
  MsgType,
  frameOf,
  encodeTelemetryBody,
  encodeProvenanceBody,
  HelloFlags,
} from "../src/index.js";
import { SPEC_DELTA_HEX, SPEC_HELLO_HEX, SPEC_KEYFRAME_HEX, hexToArrayBuffer } from "./vectors/spec-vectors.js";

class FakeSocket implements WebSocketLike {
  binaryType = "blob";
  readyState = 0;
  sent: string[] = [];
  closed: { code?: number; reason?: string } | null = null;
  #listeners = new Map<string, ((ev: never) => void)[]>();

  constructor(readonly url: string, readonly protocols: string[]) {}

  send(data: string): void {
    this.sent.push(data);
  }
  close(code?: number, reason?: string): void {
    this.closed = { code, reason };
    this.emit("close", { code: code ?? 1000, reason: reason ?? "" });
  }
  addEventListener(type: string, listener: (ev: never) => void): void {
    const list = this.#listeners.get(type) ?? [];
    list.push(listener);
    this.#listeners.set(type, list);
  }
  emit(type: string, ev?: unknown): void {
    for (const l of this.#listeners.get(type) ?? []) (l as (e: unknown) => void)(ev);
  }
  open(): void {
    this.readyState = 1;
    this.emit("open");
  }
  deliverBinary(frame: ArrayBuffer): void {
    this.emit("message", { data: frame });
  }
  deliverText(text: string): void {
    this.emit("message", { data: text });
  }
}

function makeClient(options: { profile?: "full" | "node"; run?: string } = {}): { client: VwpClient; sockets: FakeSocket[] } {
  const sockets: FakeSocket[] = [];
  const client = new VwpClient({
    url: "ws://127.0.0.1:8787",
    compress: "none",
    autoReconnect: false,
    ...options,
    socketFactory: (url, protocols) => {
      const s = new FakeSocket(url, protocols);
      sockets.push(s);
      return s;
    },
  });
  return { client, sockets };
}

describe("§1.1 — the endpoint URL", () => {
  it("carries the documented query parameters and the subprotocol", async () => {
    const { client, sockets } = makeClient({ profile: "node", run: "latest" });
    const connecting = client.connect();
    const s = sockets[0];
    expect(s.protocols).toEqual(["vwp.v1"]);
    const url = new URL(s.url);
    expect(url.pathname).toBe("/vwp/v1");
    expect(url.searchParams.get("run")).toBe("latest");
    expect(url.searchParams.get("profile")).toBe("node");
    expect(url.searchParams.get("compress")).toBe("none");
    expect(url.searchParams.get("v")).toBe("1");
    expect(url.searchParams.get("resume")).toBeNull(); // no resume on a first connection
    expect(s.binaryType).toBe("arraybuffer"); // §1.2
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    client.close();
  });
});

describe("§1.3 — the handshake", () => {
  it("sends nothing before Hello (§10.2 H3) and resolves connect() on it", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    expect(s.sent).toEqual([]);
    expect(client.state).toBe("handshaking");
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    const hello: HelloMessage = await connecting;
    expect(hello.engineVersion).toBe("v2xw 0.4.0+9f0649d");
    expect(client.state).toBe("streaming");
    expect(client.hello).toBe(hello);
    expect(client.isNodeProfile).toBe(false);
    expect(client.strings.get(2)).toBe("single-intersection");
    client.close();
  });

  it("keeps the pose buffer and slot table in step with keyframe and delta", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;

    const seenKeyframes: number[] = [];
    client.onKeyframe((kf) => seenKeyframes.push(kf.actors.count));
    s.deliverBinary(hexToArrayBuffer(SPEC_KEYFRAME_HEX));
    expect(seenKeyframes).toEqual([3]);
    expect(client.poses.count).toBe(3);
    expect(client.slots.actorIdOf(1)).toBe(1);

    s.deliverBinary(hexToArrayBuffer(SPEC_DELTA_HEX));
    expect(client.poses.xMm[0]).toBe(513734);
    expect(client.poses.positionOf(0).x).toBeCloseTo(13.734, 9);
    expect(client.ring.nextSeq).toBe(12n); // one past the delta's seq of 11
    client.close();
  });
});

describe("§1.4 — resume and §10.2 H10 gap detection", () => {
  it("puts the session token and one-past-the-last-applied seq on the next connection", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    sockets[0].open();
    sockets[0].deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    expect(client.sessionToken).toBe("s_7f3a9c21"); // §3.1.1 str_session_token
    client.ring.seed(10n);
    client.ring.push(10n, MsgType.Keyframe, new ArrayBuffer(8));
    client.ring.push(11n, MsgType.Delta, new ArrayBuffer(8));
    const url = new URL(client.endpointUrl());
    expect(url.searchParams.get("session")).toBe("s_7f3a9c21");
    expect(url.searchParams.get("resume")).toBe("12");
    client.close();
  });

  it("sends no ?resume= without a session to resume, because a seq alone names nothing", () => {
    const { client } = makeClient();
    client.ring.seed(10n);
    client.ring.push(10n, MsgType.Keyframe, new ArrayBuffer(8));
    const url = new URL(client.endpointUrl());
    expect(url.searchParams.get("resume")).toBeNull();
    expect(url.searchParams.get("session")).toBeNull();
  });

  it("detects a seq gap from the stream itself, without the stream.drop notification", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;

    const gaps: { expected: bigint; received: bigint; missing: bigint }[] = [];
    client.onGap((g) => gaps.push(g));
    // Hello said resume_seq = 0, so seq 0 is expected next; deliver seq 0 then jump to seq 5.
    s.deliverBinary(keyframeFrame({ simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0, actors: [], signals: [] }, 0n, FrameFlags.RESYNC));
    expect(gaps).toEqual([]);
    s.deliverBinary(keyframeFrame({ simTimeNs: 1n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 5, actors: [], signals: [] }, 5n));
    expect(gaps).toEqual([{ expected: 1n, received: 5n, missing: 4n }]);
    client.close();
  });

  it("refuses to apply deltas across a dropped predecessor", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    s.deliverBinary(hexToArrayBuffer(SPEC_KEYFRAME_HEX)); // gop 1, seq 10
    const before = client.poses.xMm[0];
    // step_index 2 arrives without step 1: §3.4 says drop it until the next keyframe.
    s.deliverBinary(
      deltaFrame(
        { simTimeNs: 1_200_000_000n, gopIndex: 1, stepIndex: 2, moved: [{ slot: 0, dxMm: 999, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags: 0 }] },
        12n,
      ),
    );
    expect(client.poses.xMm[0]).toBe(before);
    client.close();
  });

  it("a non-resumed Hello resets the symbol table, poses and slots (§1.4 case 2)", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    s.deliverBinary(hexToArrayBuffer(SPEC_KEYFRAME_HEX));
    expect(client.poses.count).toBe(3);

    // A fresh run on the same connection (run.start sends a new Hello).
    const fresh = helloFrame(
      {
        helloFlags: 0x01, runId: new Uint8Array(16), scenarioHash: new Uint8Array(32), worldHash: new Uint8Array(32),
        t0WallNs: 0n, simDurationNs: 0n, mobilityStepNs: 100_000_000n, keyframePeriodNs: 1_000_000_000n,
        telemetryPeriodNs: 1_000_000_000n, metricPeriodNs: 1_000_000_000n, resumeSeq: 0n, simTimeNs: 0n,
        originLatDeg: 0, originLonDeg: 0, originAltM: 0, bboxMinXM: 0, bboxMinYM: 0, bboxMaxXM: 1, bboxMaxYM: 1,
        actorCapacity: 16, nodes: [], classes: [], channels: [],
        worldRef: { mode: 2, format: 0, payloadBytes: 0, strUrl: 0 },
        strings: ["", "engine", "scn"], strEngineVersion: 1, strScenarioName: 2, strRunLabel: 0, strSessionToken: 0,
      },
      0n,
    );
    s.deliverBinary(fresh);
    expect(client.poses.count).toBe(0);
    expect(client.slots.count).toBe(0);
    expect(client.strings.size).toBe(3);
    expect(client.strings.get(1)).toBe("engine");
    expect(client.ring.size).toBe(0);
    client.close();
  });

  it("closes with 1002 on a malformed frame and surfaces a typed ProtocolError", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    const errors: string[] = [];
    client.on("protocolerror", (e) => errors.push(e.code));
    const bad = hexToArrayBuffer(SPEC_KEYFRAME_HEX);
    new DataView(bad).setUint32(0, 0, true);
    s.deliverBinary(bad);
    expect(errors).toEqual(["bad_magic"]);
    expect(s.closed?.code).toBe(1002);
  });
});

describe("SeqRing", () => {
  it("bounds itself by frames and by bytes, as §1.4's ring does", () => {
    const ring = new SeqRing(3, 1024);
    ring.seed(0n);
    for (let i = 0; i < 5; i++) ring.push(BigInt(i), MsgType.Delta, new ArrayBuffer(16));
    expect(ring.size).toBe(3);
    expect(ring.nextSeq).toBe(5n);
    expect(ring.since(3n).map((e) => e.seq)).toEqual([3n, 4n]);

    const byBytes = new SeqRing(100, 64);
    byBytes.seed(0n);
    for (let i = 0; i < 10; i++) byBytes.push(BigInt(i), MsgType.Delta, new ArrayBuffer(32));
    expect(byBytes.bytes).toBeLessThanOrEqual(64);
  });

  it("with capacity 0 it tracks seq but retains nothing (the worker's mode)", () => {
    const ring = new SeqRing(0, 0);
    ring.seed(7n);
    expect(ring.push(7n, MsgType.Keyframe, new ArrayBuffer(4096)).gap).toBe(0n);
    expect(ring.size).toBe(0);
    expect(ring.nextSeq).toBe(8n);
  });
});

describe("§6 — the JSON-RPC client", () => {
  it("correlates a response to its request by id", async () => {
    const sent: string[] = [];
    const rpc = new JsonRpcClient((t) => sent.push(t));
    const pending = rpc.request("run.status", {});
    const req = JSON.parse(sent[0]) as { jsonrpc: string; method: string; id: number; params: unknown };
    expect(req.jsonrpc).toBe("2.0");
    expect(req.method).toBe("run.status");
    expect(typeof req.id).toBe("number");
    rpc.handleText(
      JSON.stringify({
        jsonrpc: "2.0", id: req.id,
        result: { run_id: "x", state: "running", t_ns: 5, t_end_ns: 600_000_000_000, speed: 1, profile: "full", live: true, unknown_extra: 1 },
      }),
    );
    const result = await pending;
    expect(result.state).toBe("running");
    expect(result.t_ns).toBe(5);
    expect(rpc.pendingCount).toBe(0);
  });

  it("rejects with a typed JsonRpcError carrying the §6.4 code and data", async () => {
    const sent: string[] = [];
    const rpc = new JsonRpcClient((t) => sent.push(t));
    const pending = rpc.request("run.seek", { t_ns: 10 });
    const id = (JSON.parse(sent[0]) as { id: number }).id;
    rpc.handleText(
      JSON.stringify({
        jsonrpc: "2.0", id,
        error: { code: RpcErrorCode.SEEK_OUT_OF_RANGE, message: "out of range", data: { min_ns: 0, max_ns: 100 } },
      }),
    );
    await expect(pending).rejects.toThrowError(JsonRpcError);
    await pending.catch((err: unknown) => {
      const e = err as JsonRpcError;
      expect(e.code).toBe(-32003);
      expect(e.method).toBe("run.seek");
      expect(e.data).toEqual({ min_ns: 0, max_ns: 100 });
    });
  });

  it("dispatches the §6.14 notifications and tolerates unknown ones (§10.7 R9)", () => {
    const unknown: string[] = [];
    const rpc = new JsonRpcClient(() => {}, { onUnknownNotification: (m) => unknown.push(m) });
    const drops: number[] = [];
    rpc.on("stream.drop", (p) => drops.push(p.dropped.delta ?? 0));
    rpc.handleText(
      JSON.stringify({
        jsonrpc: "2.0", method: "stream.drop",
        params: { seq_first: 10422, seq_last: 10461, dropped: { delta: 38, event: 2, telemetry: 0, metric: 0 }, resync_seq: 10462 },
      }),
    );
    expect(drops).toEqual([38]);
    rpc.handleText(JSON.stringify({ jsonrpc: "2.0", method: "some.future.notification", params: {} }));
    expect(unknown).toEqual(["some.future.notification"]);
  });

  it("§6.1 — a batch array and malformed JSON are refused, not parsed", () => {
    const rpc = new JsonRpcClient(() => {});
    expect(rpc.handleText("[{}]")).toBeNull();
    expect(rpc.handleText("{not json")).toBeNull();
  });

  it("times out an outstanding request and can reject them all on close", async () => {
    vi.useFakeTimers();
    const rpc = new JsonRpcClient(() => {}, { timeoutMs: 100 });
    const pending = rpc.request("run.pause", {});
    vi.advanceTimersByTime(101);
    await expect(pending).rejects.toThrow(/timed out/);
    vi.useRealTimers();

    const rpc2 = new JsonRpcClient(() => {});
    const p2 = rpc2.request("run.pause", {});
    rpc2.rejectAll(new Error("socket closed"));
    await expect(p2).rejects.toThrow(/socket closed/);
  });

  it("§6.15 — exactly 32 methods and §6.14 — exactly 8 notifications", () => {
    expect(VWP_METHODS).toHaveLength(32);
    expect(new Set(VWP_METHODS).size).toBe(32);
    expect(VWP_METHODS).toContain("rpc.discover");
    expect(VWP_NOTIFICATIONS).toHaveLength(8);
    expect(VWP_NOTIFICATIONS).toEqual([
      "run.state", "stream.drop", "job.progress", "job.done", "view.changed", "log", "validation", "experiment.progress",
    ]);
  });
});

describe("§1.5 — the backpressure notification reaches the app", () => {
  it("re-emits stream.drop as a client event", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    const drops: number[] = [];
    client.onDrop((p) => drops.push(p.seq_last - p.seq_first + 1));
    s.deliverText(
      JSON.stringify({
        jsonrpc: "2.0", method: "stream.drop",
        params: { seq_first: 100, seq_last: 139, dropped: { delta: 38, event: 2 }, resync_seq: 140 },
      }),
    );
    expect(drops).toEqual([40]);
    client.close();
  });

  it("routes Telemetry through onTelemetry", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    const seen: number[] = [];
    client.onTelemetry((t) => seen.push(t.nodeCount));
    s.deliverBinary(frameOf(MsgType.Telemetry, 12n, encodeTelemetryBody(1n, 1n, [])));
    expect(seen).toEqual([0]);
    client.close();
  });
});

describe("§1.4 case 1 / §2.5 / §10.4 C7 — a resumed Hello keeps the symbol table", () => {
  /** SPEC_HELLO with `HELLO_RESUMED` set in `hello_flags` (body @4, frame @28). */
  function resumedHello(): ArrayBuffer {
    const frame = hexToArrayBuffer(SPEC_HELLO_HEX);
    const dv = new DataView(frame);
    dv.setUint32(24 + 4, dv.getUint32(24 + 4, true) | HelloFlags.RESUMED, true);
    return frame;
  }

  /** A `Provenance` frame whose symbol-table extension appends `strings` (§2.5, §3.8). */
  function provenanceAppending(strings: readonly string[], seq: bigint): ArrayBuffer {
    return frameOf(
      MsgType.Provenance,
      seq,
      encodeProvenanceBody(
        1_000_000_000n,
        [{ provId: 1, strModelId: 1, strModelVersion: 2, strParamSetId: 3, strCardUrl: 4, family: 0, subjectKind: 0 }],
        [],
        strings,
      ),
    );
  }

  it("ids appended by a Provenance frame still resolve after a HELLO_RESUMED Hello", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    // §3.1.7 — the Hello established ids 0..16.
    expect(client.strings.size).toBe(17);

    s.deliverBinary(provenanceAppending(["radio/propagation/log-distance-shadowing", "1.2.0"], 20n));
    expect(client.strings.size).toBe(19);
    expect(client.strings.get(17)).toBe("radio/propagation/log-distance-shadowing");
    expect(client.strings.get(18)).toBe("1.2.0");

    // §1.4 case 1: "the client keeps its world, string table, actor slots and camera state".
    s.deliverBinary(resumedHello());
    expect(client.hello?.helloFlags).toBe(HelloFlags.LIVE | HelloFlags.RESUMED);
    expect(client.strings.size).toBe(19);
    expect(client.strings.get(17)).toBe("radio/propagation/log-distance-shadowing");
    expect(client.strings.get(18)).toBe("1.2.0");
    // The ids the Hello itself re-establishes are unchanged, not reassigned.
    expect(client.strings.get(2)).toBe("single-intersection");
    client.close();
  });

  it("a resumed Hello keeps the actor slots and the pose buffer too (§1.4 case 1)", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    s.deliverBinary(hexToArrayBuffer(SPEC_KEYFRAME_HEX));
    expect(client.poses.count).toBe(3);

    s.deliverBinary(resumedHello());
    expect(client.poses.count).toBe(3);
    expect(client.slots.actorIdOf(1)).toBe(1);
    // A delta of the resumed GOP still applies, which it could not if the keyframe were discarded.
    s.deliverBinary(hexToArrayBuffer(SPEC_DELTA_HEX));
    expect(client.poses.xMm[0]).toBe(513734);
    client.close();
  });

  it("a resumed Hello that renumbers a held id is a protocol error, not a silent overwrite", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX));
    await connecting;
    const errors: string[] = [];
    client.on("protocolerror", (e) => errors.push(e.code));

    // Same 17 strings, but id 2 now names a different scenario: §2.5 forbids reassignment.
    const frame = resumedHello();
    const renumbered = helloFrame(
      {
        helloFlags: HelloFlags.LIVE | HelloFlags.RESUMED,
        runId: new Uint8Array(16), scenarioHash: new Uint8Array(32), worldHash: new Uint8Array(32),
        t0WallNs: 0n, simDurationNs: 0n, mobilityStepNs: 100_000_000n, keyframePeriodNs: 1_000_000_000n,
        telemetryPeriodNs: 1_000_000_000n, metricPeriodNs: 1_000_000_000n, resumeSeq: 12n, simTimeNs: 0n,
        originLatDeg: 0, originLonDeg: 0, originAltM: 0, bboxMinXM: 0, bboxMinYM: 0, bboxMaxXM: 1, bboxMaxYM: 1,
        actorCapacity: 4096, nodes: [], classes: [], channels: [],
        worldRef: { mode: 2, format: 0, payloadBytes: 0, strUrl: 0 },
        strings: ["", "v2xw 0.4.0+9f0649d", "a different scenario"],
        strEngineVersion: 1, strScenarioName: 2, strRunLabel: 0, strSessionToken: 0,
      },
      12n,
    );
    expect(frame.byteLength).toBeGreaterThan(0);
    s.deliverBinary(renumbered);
    expect(errors).toEqual(["bad_state"]);
    expect(s.closed?.code).toBe(1002);
    // The held table was not corrupted on the way out.
    expect(client.strings.get(2)).toBe("single-intersection");
  });
});

describe("§3.4.5 / §3.1.1 — a Delta spawn slot cannot drive unbounded allocation", () => {
  it("rejects a spawn slot beyond the Hello actor-capacity bound with a typed error and closes 1002", async () => {
    const { client, sockets } = makeClient();
    const connecting = client.connect();
    const s = sockets[0];
    s.open();
    s.deliverBinary(hexToArrayBuffer(SPEC_HELLO_HEX)); // actor_capacity = 4096
    await connecting;
    s.deliverBinary(hexToArrayBuffer(SPEC_KEYFRAME_HEX)); // gop 1, step 0
    const capacityBefore = client.poses.capacity;
    const errors: string[] = [];
    client.on("protocolerror", (e) => errors.push(e.code));

    s.deliverBinary(
      deltaFrame(
        {
          simTimeNs: 1_100_000_000n,
          gopIndex: 1,
          stepIndex: 1,
          spawns: [
            {
              slot: 3_000_000_000, actorId: 9, nodeId: 0xffffffff, xMm: 0, yMm: 0, laneId: 0xffffffff,
              zCm: 0, headingBrad: 0, speedCq: 0, cause: 0xffff, classIdx: 0, state: 8, verifiedNeighbors: 0,
            },
          ],
        },
        12n,
      ),
    );
    expect(errors).toEqual(["bad_offset"]);
    expect(s.closed?.code).toBe(1002);
    // Nothing was allocated: the probe measured capacity 4,294,967,296 before this check existed.
    expect(client.poses.capacity).toBe(capacityBefore);
    expect(client.slots.capacity).toBeLessThanOrEqual(8192);
  });
});
