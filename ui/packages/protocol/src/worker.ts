/**
 * Web Worker transport — the socket and every §3 decoder run off the main thread.
 *
 * The worker owns a {@link VwpClient}. Decoded messages are posted to the main thread with the
 * frame's `ArrayBuffer` in the **transfer list**: structured clone re-points the typed-array views
 * at the transferred buffer, so the pose columns cross the thread boundary with no copy at all.
 * Messages that carry closures (`Delta.absolute`, `Telemetry.record`, `Event.payload`,
 * `MetricSample.sample`) travel as their data and are rehydrated on the main thread.
 *
 * Two entry points:
 * - {@link startVwpWorker} — call it from a worker module (it runs automatically when this module
 *   is loaded as a worker).
 * - {@link VwpWorkerClient} — the main-thread proxy, which implements {@link VwpClientApi}.
 */

import { type VwpClientApi, type VwpClientEvents, type VwpClientOptions, type VwpConnectionState, VwpClient } from "./client.js";
import { ProtocolError } from "./frame.js";
import {
  type DeltaAbsoluteBlock,
  type DeltaMessage,
  type EventMessage,
  HelloFlags,
  type HelloMessage,
  type KeyframeMessage,
  type MetricRecord,
  METRIC_RECORD_OFFSETS,
  type MetricSampleMessage,
  type NodeTelemetry,
  type ProvenanceMessage,
  type TelemetryMessage,
  decodeEventPayload,
  readNodeTelemetryRecord,
} from "./messages.js";
import { PoseBuffer } from "./pose.js";
import { SlotTable } from "./slots.js";
import type { ParamsOf, ResultOf, StreamDropNotification, VwpMethodName } from "./rpc.js";

/** The `Delta` payload as it crosses the thread boundary (no closures). */
export interface TransferredDelta {
  readonly kind: "delta";
  readonly header: DeltaMessage["header"];
  readonly simTimeNs: bigint;
  readonly gopIndex: number;
  readonly stepIndex: number;
  readonly moved: DeltaMessage["moved"];
  readonly absCount: number;
  readonly absWords: Int32Array;
  readonly absHalves: Int16Array;
  readonly lanes: Uint32Array;
  readonly spawns: DeltaMessage["spawns"];
  readonly despawns: DeltaMessage["despawns"];
  readonly signals: DeltaMessage["signals"];
}
/** The `Telemetry` payload as it crosses the thread boundary. */
export interface TransferredTelemetry {
  readonly kind: "telemetry";
  readonly header: TelemetryMessage["header"];
  readonly simTimeNs: bigint;
  readonly windowNs: bigint;
  readonly nodeCount: number;
  readonly recordSize: number;
  readonly raw: Uint8Array;
}
/** The `Event` payload as it crosses the thread boundary. */
export interface TransferredEvent {
  readonly kind: "event";
  readonly header: EventMessage["header"];
  readonly tStartNs: bigint;
  readonly tEndNs: bigint;
  readonly count: number;
  readonly index: EventMessage["index"];
  readonly payloads: Uint8Array;
}
/** The `MetricSample` payload as it crosses the thread boundary. */
export interface TransferredMetric {
  readonly kind: "metric";
  readonly header: MetricSampleMessage["header"];
  readonly simTimeNs: bigint;
  readonly binWidthNs: bigint;
  readonly sampleCount: number;
  readonly recordSize: number;
  readonly raw: Uint8Array;
}

/** Main thread → worker. */
export type VwpWorkerRequest =
  | { readonly type: "connect"; readonly options: Omit<VwpClientOptions, "socketFactory" | "decompress" | "rpc"> }
  | { readonly type: "rpc"; readonly id: number; readonly method: VwpMethodName; readonly params: unknown }
  | { readonly type: "close"; readonly code?: number; readonly reason?: string };

/** Worker → main thread. */
export type VwpWorkerMessage =
  | { readonly type: "hello"; readonly msg: HelloMessage }
  | { readonly type: "keyframe"; readonly msg: KeyframeMessage }
  | { readonly type: "delta"; readonly msg: TransferredDelta }
  | { readonly type: "telemetry"; readonly msg: TransferredTelemetry }
  | { readonly type: "event"; readonly msg: TransferredEvent }
  | { readonly type: "metric"; readonly msg: TransferredMetric }
  | { readonly type: "provenance"; readonly msg: ProvenanceMessage }
  | { readonly type: "notification"; readonly method: string; readonly params: unknown }
  | { readonly type: "drop"; readonly params: StreamDropNotification }
  | { readonly type: "state"; readonly state: VwpConnectionState }
  | { readonly type: "error"; readonly message: string; readonly code: string }
  | { readonly type: "rpcresult"; readonly id: number; readonly result?: unknown; readonly error?: { code: number; message: string; data?: unknown } };

/** The part of the `Worker` API the proxy needs, so tests can substitute a fake. */
export interface WorkerLike {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  addEventListener(type: "message", listener: (ev: { data: unknown }) => void): void;
  terminate?(): void;
}
/** The part of `DedicatedWorkerGlobalScope` the worker entry needs. */
export interface WorkerScopeLike {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  addEventListener(type: "message", listener: (ev: { data: unknown }) => void): void;
}

/**
 * All non-empty views of a decoded message are views over the one frame `ArrayBuffer`, so
 * transferring that buffer once carries every column across with no copy.
 */
function transferOf(...views: readonly ArrayBufferView[]): Transferable[] {
  for (const v of views) {
    if (v.byteLength > 0) return [v.buffer as ArrayBuffer];
  }
  return [];
}

/**
 * Run the worker side: own the socket, decode, and post results with transfer lists.
 * Returns a disposer.
 */
export function startVwpWorker(scope: WorkerScopeLike): () => void {
  let client: VwpClient | null = null;

  const post = (msg: VwpWorkerMessage, transfer?: Transferable[]): void => {
    scope.postMessage(msg, transfer);
  };

  const onRequest = (ev: { data: unknown }): void => {
    const req = ev.data as VwpWorkerRequest;
    if (!req || typeof req !== "object") return;
    switch (req.type) {
      case "connect": {
        // ringFrames 0: frame buffers are transferred away, so the worker retains seq only.
        client = new VwpClient({ ...req.options, ringFrames: 0 });
        client.onState((state) => post({ type: "state", state }));
        client.on("protocolerror", (e) => post({ type: "error", message: e.message, code: e.code }));
        client.onNotification(({ method, params }) => post({ type: "notification", method, params }));
        client.onDrop((params) => post({ type: "drop", params }));
        client.onHello((msg) => post({ type: "hello", msg }, transferOf(msg.runId)));
        client.onKeyframe((msg) => post({ type: "keyframe", msg }, transferOf(msg.actors.actorId, msg.signals.signalId)));
        client.onDelta((msg) =>
          post(
            {
              type: "delta",
              msg: {
                kind: "delta", header: msg.header, simTimeNs: msg.simTimeNs, gopIndex: msg.gopIndex,
                stepIndex: msg.stepIndex, moved: msg.moved, absCount: msg.absolute.count,
                absWords: msg.absolute.words, absHalves: msg.absolute.halves, lanes: msg.lanes,
                spawns: msg.spawns, despawns: msg.despawns, signals: msg.signals,
              },
            },
            transferOf(msg.moved.slot, msg.spawns.slot, msg.despawns.slot, msg.signals.signalId, msg.lanes),
          ),
        );
        client.onTelemetry((msg) =>
          post(
            { type: "telemetry", msg: { kind: "telemetry", header: msg.header, simTimeNs: msg.simTimeNs,
              windowNs: msg.windowNs, nodeCount: msg.nodeCount, recordSize: msg.recordSize, raw: msg.raw } },
            transferOf(msg.raw),
          ),
        );
        client.onEvent((msg) =>
          post(
            { type: "event", msg: { kind: "event", header: msg.header, tStartNs: msg.tStartNs, tEndNs: msg.tEndNs,
              count: msg.count, index: msg.index, payloads: msg.payloads } },
            transferOf(msg.index.simTimeNs, msg.payloads),
          ),
        );
        client.onMetric((msg) =>
          post(
            { type: "metric", msg: { kind: "metric", header: msg.header, simTimeNs: msg.simTimeNs,
              binWidthNs: msg.binWidthNs, sampleCount: msg.sampleCount, recordSize: msg.recordSize, raw: msg.raw } },
            transferOf(msg.raw),
          ),
        );
        client.onProvenance((msg) => post({ type: "provenance", msg }));
        void client.connect().catch((err: unknown) => {
          post({ type: "error", message: err instanceof Error ? err.message : String(err), code: "connect_failed" });
        });
        break;
      }
      case "rpc": {
        if (!client) {
          post({ type: "rpcresult", id: req.id, error: { code: -32603, message: "worker has no connection" } });
          return;
        }
        client
          .request(req.method, req.params as ParamsOf<VwpMethodName>)
          .then((result) => post({ type: "rpcresult", id: req.id, result }))
          .catch((err: unknown) => {
            const e = err as { code?: number; message?: string; data?: unknown };
            post({ type: "rpcresult", id: req.id, error: { code: e.code ?? -32603, message: e.message ?? String(err), data: e.data } });
          });
        break;
      }
      case "close":
        client?.close(req.code, req.reason);
        client = null;
        break;
      default:
        break;
    }
  };

  scope.addEventListener("message", onRequest);
  return () => {
    client?.close();
    client = null;
  };
}

/** Rebuild a `Delta`'s absolute-block accessors after transfer. */
export function rehydrateDelta(t: TransferredDelta): DeltaMessage {
  const absolute: DeltaAbsoluteBlock = {
    count: t.absCount,
    words: t.absWords,
    halves: t.absHalves,
    xMm: (i: number) => t.absWords[i * 3],
    yMm: (i: number) => t.absWords[i * 3 + 1],
    zCm: (i: number) => t.absHalves[i * 6 + 4],
  };
  return {
    kind: "delta", header: t.header, simTimeNs: t.simTimeNs, gopIndex: t.gopIndex, stepIndex: t.stepIndex,
    moved: t.moved, absolute, lanes: t.lanes, spawns: t.spawns, despawns: t.despawns, signals: t.signals,
  };
}

/** Rebuild a `Telemetry`'s record accessors after transfer. */
export function rehydrateTelemetry(t: TransferredTelemetry): TelemetryMessage {
  const read = (i: number): NodeTelemetry => readNodeTelemetryRecord(t.raw, i, t.recordSize);
  return {
    kind: "telemetry", header: t.header, simTimeNs: t.simTimeNs, windowNs: t.windowNs,
    nodeCount: t.nodeCount, recordSize: t.recordSize, raw: t.raw,
    record: read,
    nodeIdAt: (i: number) => read(i).nodeId,
    records: () => {
      const out: NodeTelemetry[] = new Array<NodeTelemetry>(t.nodeCount);
      for (let i = 0; i < t.nodeCount; i++) out[i] = read(i);
      return out;
    },
  };
}

/** Rebuild an `Event`'s payload accessors after transfer. */
export function rehydrateEvent(t: TransferredEvent): EventMessage {
  const payloadView = (i: number): DataView =>
    new DataView(t.payloads.buffer, t.payloads.byteOffset + t.index.payloadOff[i], t.index.payloadLen[i]);
  return {
    kind: "event", header: t.header, tStartNs: t.tStartNs, tEndNs: t.tEndNs, count: t.count, index: t.index,
    payloads: t.payloads,
    payloadView,
    payload: (i: number) => decodeEventPayload(t.index.channelId[i], payloadView(i)),
  };
}

/** Rebuild a `MetricSample`'s record accessors after transfer. */
export function rehydrateMetric(t: TransferredMetric): MetricSampleMessage {
  const dv = new DataView(t.raw.buffer, t.raw.byteOffset, t.raw.byteLength);
  const R = METRIC_RECORD_OFFSETS;
  const read = (i: number): MetricRecord => {
    const b = i * t.recordSize;
    return {
      value: dv.getFloat64(b + R.value, true),
      strMetric: dv.getUint32(b + R.strMetric, true),
      dimKey: dv.getUint32(b + R.dimKey, true),
      nodeId: dv.getUint32(b + R.nodeId, true),
      count: dv.getUint32(b + R.count, true),
      agg: dv.getUint16(b + R.agg, true),
      visibility: dv.getUint8(b + R.visibility),
      provId: dv.getUint32(b + R.provId, true),
    };
  };
  return {
    kind: "metric", header: t.header, simTimeNs: t.simTimeNs, binWidthNs: t.binWidthNs,
    sampleCount: t.sampleCount, recordSize: t.recordSize, raw: t.raw,
    sample: read,
    samples: () => {
      const out: MetricRecord[] = new Array<MetricRecord>(t.sampleCount);
      for (let i = 0; i < t.sampleCount; i++) out[i] = read(i);
      return out;
    },
  };
}

/**
 * Main-thread proxy with the same surface as {@link VwpClient}: the socket and the decoders live in
 * a worker, and this object mirrors the pose state and re-emits the events.
 */
export class VwpWorkerClient implements VwpClientApi {
  readonly poses: PoseBuffer;
  readonly slots = new SlotTable();

  #worker: WorkerLike;
  #options: VwpClientOptions;
  #state: VwpConnectionState = "idle";
  #hello: HelloMessage | null = null;
  #listeners = new Map<string, Set<(payload: never) => void>>();
  #nextRpcId = 1;
  #pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  #trackPoses: boolean;

  constructor(worker: WorkerLike, options: VwpClientOptions) {
    this.#worker = worker;
    this.#options = options;
    this.#trackPoses = options.trackPoses ?? true;
    this.poses = new PoseBuffer(options.poseCapacity ?? 1024);
    worker.addEventListener("message", (ev) => this.#onMessage(ev.data as VwpWorkerMessage));
  }

  get state(): VwpConnectionState {
    return this.#state;
  }

  get hello(): HelloMessage | null {
    return this.#hello;
  }

  connect(): Promise<HelloMessage> {
    return new Promise<HelloMessage>((resolve, reject) => {
      const offHello = this.on("hello", (h) => {
        offHello();
        offErr();
        resolve(h);
      });
      const offErr = this.on("protocolerror", (e) => {
        offHello();
        offErr();
        reject(e);
      });
      const { socketFactory: _sf, decompress: _dc, rpc: _rpc, ...rest } = this.#options;
      void _sf;
      void _dc;
      void _rpc;
      this.#worker.postMessage({ type: "connect", options: rest } satisfies VwpWorkerRequest);
    });
  }

  close(code?: number, reason?: string): void {
    this.#worker.postMessage({ type: "close", code, reason } satisfies VwpWorkerRequest);
    this.#worker.terminate?.();
    this.#setState("closed");
  }

  request<M extends VwpMethodName>(method: M, params: ParamsOf<M>): Promise<ResultOf<M>> {
    const id = this.#nextRpcId++;
    return new Promise<ResultOf<M>>((resolve, reject) => {
      this.#pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.#worker.postMessage({ type: "rpc", id, method, params } satisfies VwpWorkerRequest);
    });
  }

  on<K extends keyof VwpClientEvents>(event: K, listener: (payload: VwpClientEvents[K]) => void): () => void {
    let set = this.#listeners.get(event);
    if (!set) {
      set = new Set();
      this.#listeners.set(event, set);
    }
    set.add(listener as (payload: never) => void);
    return () => {
      set?.delete(listener as (payload: never) => void);
    };
  }

  onHello(l: (p: HelloMessage) => void): () => void { return this.on("hello", l); }
  onKeyframe(l: (p: KeyframeMessage) => void): () => void { return this.on("keyframe", l); }
  onDelta(l: (p: DeltaMessage) => void): () => void { return this.on("delta", l); }
  onTelemetry(l: (p: TelemetryMessage) => void): () => void { return this.on("telemetry", l); }
  onEvent(l: (p: EventMessage) => void): () => void { return this.on("event", l); }
  onMetric(l: (p: MetricSampleMessage) => void): () => void { return this.on("metric", l); }
  onProvenance(l: (p: ProvenanceMessage) => void): () => void { return this.on("provenance", l); }
  onNotification(l: (p: { method: string; params: unknown }) => void): () => void { return this.on("notification", l); }
  onDrop(l: (p: StreamDropNotification) => void): () => void { return this.on("drop", l); }
  onState(l: (p: VwpConnectionState) => void): () => void { return this.on("state", l); }

  #setState(state: VwpConnectionState): void {
    if (this.#state === state) return;
    this.#state = state;
    this.#emit("state", state);
  }

  #emit<K extends keyof VwpClientEvents>(event: K, payload: VwpClientEvents[K]): void {
    const set = this.#listeners.get(event);
    if (!set) return;
    for (const l of set) (l as (p: VwpClientEvents[K]) => void)(payload);
  }

  #onMessage(msg: VwpWorkerMessage): void {
    switch (msg.type) {
      case "state":
        this.#setState(msg.state);
        break;
      case "hello": {
        this.#hello = msg.msg;
        if (this.#trackPoses) {
          // §1.4 — case 2 (not resumed) discards all stream state; case 1 (resumed) keeps the
          // world, string table, actor slots and camera state, so the mirrored pose buffer and
          // slot table must survive a `HELLO_RESUMED` Hello too.
          if ((msg.msg.helloFlags & HelloFlags.RESUMED) === 0) {
            this.poses.reset();
            this.slots.reset();
          }
          const capacity = Math.max(1, Math.min(msg.msg.actorCapacity, 1 << 20));
          this.poses.ensureCapacity(capacity);
          this.slots.ensureCapacity(capacity);
          // §3.1.1 / §3.4.5 — bound the slot ids a `Delta` may name.
          this.poses.setSlotBound(capacity);
          this.slots.setSlotBound(capacity);
        }
        this.#emit("hello", msg.msg);
        break;
      }
      case "keyframe":
        if (this.#trackPoses) {
          this.poses.applyKeyframe(msg.msg);
          this.slots.adoptKeyframe(msg.msg.actors.actorId, msg.msg.gopIndex);
        }
        this.#emit("keyframe", msg.msg);
        break;
      case "delta": {
        const delta = rehydrateDelta(msg.msg);
        if (this.#trackPoses) {
          this.poses.applyDelta(delta);
          for (let i = 0; i < delta.spawns.count; i++) this.slots.adoptSpawn(delta.spawns.slot[i], delta.spawns.actorId[i]);
          for (let i = 0; i < delta.despawns.count; i++) this.slots.release(delta.despawns.slot[i]);
        }
        this.#emit("delta", delta);
        break;
      }
      case "telemetry": this.#emit("telemetry", rehydrateTelemetry(msg.msg)); break;
      case "event": this.#emit("event", rehydrateEvent(msg.msg)); break;
      case "metric": this.#emit("metric", rehydrateMetric(msg.msg)); break;
      case "provenance": this.#emit("provenance", msg.msg); break;
      case "notification": this.#emit("notification", { method: msg.method, params: msg.params }); break;
      case "drop": this.#emit("drop", msg.params); break;
      case "rpcresult": {
        const pending = this.#pending.get(msg.id);
        if (!pending) return;
        this.#pending.delete(msg.id);
        if (msg.error) pending.reject(new Error(`${msg.error.message} (${msg.error.code})`));
        else pending.resolve(msg.result);
        break;
      }
      case "error":
        this.#emit("protocolerror", new ProtocolError("bad_state", msg.message));
        break;
      default:
        break;
    }
  }
}

// Auto-start when this module is loaded as a dedicated worker.
const maybeScope = globalThis as unknown as { DedicatedWorkerGlobalScope?: unknown; postMessage?: unknown; addEventListener?: unknown };
if (
  typeof maybeScope.DedicatedWorkerGlobalScope !== "undefined" &&
  typeof maybeScope.postMessage === "function" &&
  typeof maybeScope.addEventListener === "function"
) {
  startVwpWorker(globalThis as unknown as WorkerScopeLike);
}
