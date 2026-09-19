/** The worker transport: decoding off the main thread, with transfer lists (zero-copy). */

import { describe, expect, it } from "vitest";

import {
  type TransferredDelta,
  type TransferredEvent,
  type TransferredMetric,
  type TransferredTelemetry,
  type VwpWorkerMessage,
  ChannelId,
  MsgType,
  decodeDelta,
  decodeEvent,
  decodeMetricSample,
  decodeTelemetry,
  encodeEventBody,
  encodeMetricSampleBody,
  encodeTelemetryBody,
  frameOf,
  rehydrateDelta,
  rehydrateEvent,
  rehydrateMetric,
  rehydrateTelemetry,
  startVwpWorker,
  viewFrame,
  type WorkerScopeLike,
  type WorkerLike,
  type HelloMessage,
  HelloFlags,
  VwpWorkerClient,
  decodeHello,
  decodeKeyframe,
  helloFrame,
  keyframeFrame,
} from "../src/index.js";
import { SPEC_DELTA_HEX, hexToArrayBuffer } from "./vectors/spec-vectors.js";

describe("rehydration after a transfer", () => {
  it("a Delta keeps its absolute-block accessors", () => {
    const original = decodeDelta(viewFrame(hexToArrayBuffer(SPEC_DELTA_HEX)));
    const transferred: TransferredDelta = {
      kind: "delta", header: original.header, simTimeNs: original.simTimeNs, gopIndex: original.gopIndex,
      stepIndex: original.stepIndex, moved: original.moved, absCount: original.absolute.count,
      absWords: original.absolute.words, absHalves: original.absolute.halves, lanes: original.lanes,
      spawns: original.spawns, despawns: original.despawns, signals: original.signals,
    };
    const back = rehydrateDelta(transferred);
    expect(back.gopIndex).toBe(1);
    expect(back.stepIndex).toBe(1);
    expect(Array.from(back.moved.dxMm)).toEqual([1389]);
    expect(Array.from(back.lanes)).toEqual([44]);
    expect(back.absolute.count).toBe(0);
  });

  it("a Telemetry keeps record() and strides by the wire record_size", () => {
    const body = encodeTelemetryBody(5n, 1n, [
      {
        storageUsedB: 1n, storageTotalB: 2n, nextTopupNs: 3n, crlBytes: 4n, outboxBytes: 5n, clockOffsetNs: -6n,
        nodeId: 77, ramUsedKib: 8, ramTotalKib: 9, dropRxOverflow: 0, dropVerifyPolicySkip: 0, dropVerifyOverflow: 0,
        dropTxOverflow: 0, dropReassemblyTimeout: 0, dropCrlBacklog: 0, certStored: 0, crlEntries: 0, outboxMsgs: 0,
        peerCacheEntries: 0, p2pcdRequests: 0, fullCertMsgs: 0, msgsInPerS: 1, msgsOutPerS: 2, verificationsPerS: 3,
        verifyWaitP50Ms: 4, verifyWaitP95Ms: 5, gnssHdop: 6, gnssSigmaM: 7, clockDriftPpm: 8, posErrorM: 9,
        airtimeMsPerS: 10, cpuUtilPm: 11, hsmUtilPm: 12, qRxP50: 13, qRxP95: 14, qVerifyP50: 15, qVerifyP95: 16,
        qAppP50: 17, qAppP95: 18, qTxP50: 19, qTxP95: 20, qCrlP50: 21, qCrlP95: 22, dccState: 1, cbrPm: 23,
        txPowerCdbm: 24, nbrTotal: 25, nbrVerified: 26, nbrUnverified: 27, nbrRevoked: 28, certActive: 29,
        crlExpansionPm: 30, unverifiedRatioPm: 31, gnssFix: 2, nodeState: 2, verifyPolicy: 0,
      },
    ]);
    const msg = decodeTelemetry(viewFrame(frameOf(MsgType.Telemetry, 0n, body)));
    const t: TransferredTelemetry = {
      kind: "telemetry", header: msg.header, simTimeNs: msg.simTimeNs, windowNs: msg.windowNs,
      nodeCount: msg.nodeCount, recordSize: msg.recordSize, raw: msg.raw,
    };
    const back = rehydrateTelemetry(t);
    expect(back.nodeCount).toBe(1);
    expect(back.record(0).nodeId).toBe(77);
    expect(back.nodeIdAt(0)).toBe(77);
    expect(back.records()).toHaveLength(1);
  });

  it("an Event keeps payload() over the transferred payload region", () => {
    const payload = new Uint8Array(16);
    new DataView(payload.buffer).setUint32(0, 5, true);
    new DataView(payload.buffer).setFloat32(4, 0.25, true);
    new DataView(payload.buffer).setUint16(8, 172, true);
    const msg = decodeEvent(
      viewFrame(frameOf(MsgType.Event, 0n, encodeEventBody(0n, 1n, [{ simTimeNs: 1n, channelId: ChannelId.MAC_CBR, payload }]))),
    );
    const t: TransferredEvent = {
      kind: "event", header: msg.header, tStartNs: msg.tStartNs, tEndNs: msg.tEndNs, count: msg.count,
      index: msg.index, payloads: msg.payloads,
    };
    const back = rehydrateEvent(t);
    const p = back.payload(0);
    expect(p.channel).toBe("mac.cbr");
    if (p.channel === "mac.cbr") {
      expect(p.nodeId).toBe(5);
      expect(p.cbr).toBeCloseTo(0.25, 6);
      expect(p.channelNumber).toBe(172);
    }
  });

  it("a MetricSample keeps sample()", () => {
    const msg = decodeMetricSample(
      viewFrame(frameOf(MsgType.MetricSample, 0n, encodeMetricSampleBody(9n, 1n, [
        { value: 0.5, strMetric: 3, dimKey: 0, nodeId: 1, count: 2, agg: 1, visibility: 1, provId: 0 },
      ]))),
    );
    const t: TransferredMetric = {
      kind: "metric", header: msg.header, simTimeNs: msg.simTimeNs, binWidthNs: msg.binWidthNs,
      sampleCount: msg.sampleCount, recordSize: msg.recordSize, raw: msg.raw,
    };
    const back = rehydrateMetric(t);
    expect(back.sample(0).value).toBe(0.5);
    expect(back.sample(0).strMetric).toBe(3);
    expect(back.samples()).toHaveLength(1);
  });
});

describe("the worker entry", () => {
  it("wires a scope and reports an rpc call made before there is a connection", () => {
    const posted: VwpWorkerMessage[] = [];
    const handlers: ((ev: { data: unknown }) => void)[] = [];
    const scope: WorkerScopeLike = {
      postMessage: (m) => posted.push(m as VwpWorkerMessage),
      addEventListener: (_type, l) => {
        handlers.push(l);
      },
    };
    const dispose = startVwpWorker(scope);
    expect(handlers).toHaveLength(1);
    handlers[0]({ data: { type: "rpc", id: 1, method: "run.status", params: {} } });
    expect(posted).toHaveLength(1);
    const first = posted[0];
    expect(first.type).toBe("rpcresult");
    if (first.type === "rpcresult") expect(first.error?.code).toBe(-32603);
    dispose();
  });
});

describe("§1.4 case 1 — the worker proxy keeps its mirrored pose state across a resumed Hello", () => {
  /** A `WorkerLike` whose `deliver` pushes a worker→main message into the proxy. */
  function fakeWorker(): { worker: WorkerLike; deliver: (m: VwpWorkerMessage) => void } {
    const listeners: ((ev: { data: unknown }) => void)[] = [];
    return {
      worker: {
        postMessage: () => undefined,
        addEventListener: (_type, l) => {
          listeners.push(l);
        },
      },
      deliver: (m) => {
        for (const l of listeners) l({ data: m });
      },
    };
  }

  const helloMsg = (helloFlags: number): HelloMessage =>
    decodeHello(
      viewFrame(
        helloFrame(
          {
            helloFlags, runId: new Uint8Array(16), scenarioHash: new Uint8Array(32), worldHash: new Uint8Array(32),
            t0WallNs: 0n, simDurationNs: 0n, mobilityStepNs: 100_000_000n, keyframePeriodNs: 1_000_000_000n,
            telemetryPeriodNs: 1_000_000_000n, metricPeriodNs: 1_000_000_000n, resumeSeq: 0n, simTimeNs: 0n,
            originLatDeg: 0, originLonDeg: 0, originAltM: 0, bboxMinXM: 0, bboxMinYM: 0, bboxMaxXM: 1, bboxMaxYM: 1,
            actorCapacity: 64, nodes: [], classes: [], channels: [],
            worldRef: { mode: 2, format: 0, payloadBytes: 0, strUrl: 0 },
            strings: ["", "engine"], strEngineVersion: 1, strScenarioName: 0, strRunLabel: 0, strSessionToken: 0,
          },
          0n,
        ),
      ),
    );

  it("a HELLO_RESUMED Hello does not discard the keyframe the resumed deltas need", () => {
    const { worker, deliver } = fakeWorker();
    const proxy = new VwpWorkerClient(worker, { url: "ws://127.0.0.1:8787" });
    deliver({ type: "hello", msg: helloMsg(HelloFlags.LIVE) });
    deliver({
      type: "keyframe",
      msg: decodeKeyframe(
        viewFrame(
          keyframeFrame(
            {
              simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0,
              actors: [{ actorId: 1, xMm: 1000, yMm: 2000, laneId: 0xffffffff, zCm: 10, headingBrad: 0, speedCq: 0, accelCq: 0, classIdx: 0, state: 8, verifiedNeighbors: 0, flags8: 0 }],
              signals: [],
            },
            0n,
          ),
        ),
      ),
    });
    expect(proxy.poses.count).toBe(1);
    expect(proxy.poses.hasKeyframe).toBe(true);

    deliver({ type: "hello", msg: helloMsg(HelloFlags.LIVE | HelloFlags.RESUMED) });
    expect(proxy.poses.count).toBe(1);
    expect(proxy.poses.hasKeyframe).toBe(true);
    expect(proxy.poses.xMm[0]).toBe(1000);

    // A non-resumed Hello still discards everything (§1.4 case 2).
    deliver({ type: "hello", msg: helloMsg(HelloFlags.LIVE) });
    expect(proxy.poses.count).toBe(0);
    expect(proxy.poses.hasKeyframe).toBe(false);
  });
});
