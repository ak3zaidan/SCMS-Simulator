/**
 * `VwpClient` — the connection state machine of docs/protocol/vwp-v1.md §1.
 *
 * It owns one WebSocket, routes binary frames to the §3 decoders and text frames to the §6
 * JSON-RPC client, keeps the pose buffer and slot table in step with the stream, detects `seq`
 * gaps (§10.2 H10) and reconnects with the `?resume=` handshake of §1.4.
 */

import {
  FrameFlags,
  MsgType,
  ProtocolError,
  VWP_SUBPROTOCOL,
  isCanonicalMsgType,
  parseFrameHeader,
} from "./frame.js";
import {
  type ByeMessage,
  ByeReason,
  type DecodeOptions,
  type DeltaMessage,
  type ErrorMessage,
  type EventMessage,
  HelloFlags,
  type HelloMessage,
  type KeyframeMessage,
  type MetricSampleMessage,
  type ProvenanceMessage,
  StringTable,
  type TelemetryMessage,
  type VwpMessage,
  type WorldChunkMessage,
  type ZstdDecompress,
  decodeFrameView,
  viewFrame,
} from "./messages.js";
import { PoseBuffer } from "./pose.js";
import { SlotTable } from "./slots.js";
import {
  JsonRpcClient,
  type JsonRpcClientOptions,
  type ParamsOf,
  type ResultOf,
  type StreamDropNotification,
  type VwpMethodName,
  type VwpNotificationName,
  type VwpNotifications,
} from "./rpc.js";

/** The minimum of the `WebSocket` API this client uses; Node's `ws` satisfies it. */
export interface WebSocketLike {
  binaryType: string;
  readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  addEventListener(type: "open", listener: () => void): void;
  addEventListener(type: "message", listener: (ev: { data: unknown }) => void): void;
  addEventListener(type: "close", listener: (ev: { code: number; reason: string }) => void): void;
  addEventListener(type: "error", listener: (ev: unknown) => void): void;
}

/** Creates the socket. Defaults to the platform `WebSocket`; pass `ws` in Node. */
export type WebSocketFactory = (url: string, protocols: string[]) => WebSocketLike;

/** Where the connection state machine of §1 currently is. */
export type VwpConnectionState =
  | "idle"
  | "connecting"
  /** socket open, waiting for the mandatory `Hello` (§1.3: within 1000 ms) */
  | "handshaking"
  | "streaming"
  | "reconnecting"
  | "closed"
  /** gave up: `Bye{reason=0}`, close 4406 or 4404 (§1.4) */
  | "failed";

/** Options for {@link VwpClient}. */
export interface VwpClientOptions {
  /** Base URL of the engine, e.g. `ws://127.0.0.1:8787` or `http://127.0.0.1:8787`. */
  readonly url: string;
  /** `?run=` — a run id or `latest`. */ readonly run?: string;
  /** `?profile=` — immutable for the life of the connection (§5.3). */ readonly profile?: "full" | "node";
  /** `?compress=` — pass `none` unless a `decompress` is supplied (§2.6). */ readonly compress?: "zstd" | "none";
  /** zstd decompressor; without one the client must connect with `compress=none`. */ readonly decompress?: ZstdDecompress;
  /** Reconnect with exponential backoff after an unclean close (§1.4). Default `true`. */ readonly autoReconnect?: boolean;
  /** §1.4 — 250 ms → 8 s with ±20 % jitter. */ readonly backoffMinMs?: number;
  readonly backoffMaxMs?: number;
  readonly backoffJitter?: number;
  /** Keep {@link VwpClient.poses} and {@link VwpClient.slots} in step with the stream. Default `true`. */
  readonly trackPoses?: boolean;
  /** Initial pose/slot capacity; `Hello.actor_capacity` grows it automatically. */ readonly poseCapacity?: number;
  /** How many canonical frames the client-side seq ring retains. Default 4096, as §1.4's server ring. */
  readonly ringFrames?: number;
  /** Byte cap of the client-side seq ring. Default 8 MiB, as §1.4. */ readonly ringBytes?: number;
  readonly socketFactory?: WebSocketFactory;
  readonly rpc?: JsonRpcClientOptions;
}

/** One retained canonical frame in the client-side seq ring (§1.4). */
export interface RingEntry {
  readonly seq: bigint;
  readonly msgType: number;
  readonly frame: ArrayBuffer;
}

/**
 * The client's mirror of §1.4's resume ring: the last canonical frames it received, bounded by
 * frame count and bytes, plus the `seq` it expects next (what `?resume=` carries on reconnect).
 */
export class SeqRing {
  #entries: RingEntry[] = [];
  #bytes = 0;
  #maxFrames: number;
  #maxBytes: number;
  #next: bigint | null = null;

  constructor(maxFrames = 4096, maxBytes = 8 * 1024 * 1024) {
    this.#maxFrames = maxFrames;
    this.#maxBytes = maxBytes;
  }

  /** One past the last canonical frame fully applied — the `?resume=` value (§1.4). */
  get nextSeq(): bigint | null {
    return this.#next;
  }

  /** Frames currently retained. */
  get size(): number {
    return this.#entries.length;
  }

  /** Bytes currently retained. */
  get bytes(): number {
    return this.#bytes;
  }

  /** Seed the expected seq from `Hello.resume_seq`. */
  seed(seq: bigint): void {
    this.#next = seq;
  }

  /** Record a canonical frame. Returns the gap size when `seq` skipped ahead (§10.2 H10). */
  push(seq: bigint, msgType: number, frame: ArrayBuffer): { gap: bigint } {
    const expected = this.#next;
    const gap = expected !== null && seq > expected ? seq - expected : 0n;
    if (this.#maxFrames > 0) {
      // A worker that transfers frame buffers on retains nothing here: capacity 0 tracks seq only.
      this.#entries.push({ seq, msgType, frame });
      this.#bytes += frame.byteLength;
      while (this.#entries.length > this.#maxFrames || this.#bytes > this.#maxBytes) {
        const dropped = this.#entries.shift();
        if (!dropped) break;
        this.#bytes -= dropped.frame.byteLength;
      }
    }
    this.#next = seq + 1n;
    return { gap };
  }

  /** Retained frames from `seq` onwards, ascending. */
  since(seq: bigint): RingEntry[] {
    return this.#entries.filter((e) => e.seq >= seq);
  }

  /** Forget everything (a non-resumed `Hello`). */
  clear(): void {
    this.#entries = [];
    this.#bytes = 0;
  }
}

/** A `seq` gap the client detected without a `stream.drop` notification (§10.2 H10). */
export interface SeqGapEvent {
  readonly expected: bigint;
  readonly received: bigint;
  readonly missing: bigint;
}

/** Every event {@link VwpClient} emits. */
export interface VwpClientEvents {
  hello: HelloMessage;
  keyframe: KeyframeMessage;
  delta: DeltaMessage;
  telemetry: TelemetryMessage;
  event: EventMessage;
  metric: MetricSampleMessage;
  provenance: ProvenanceMessage;
  worldchunk: WorldChunkMessage;
  streamerror: ErrorMessage;
  bye: ByeMessage;
  /** any decoded frame, after the specific event */
  message: VwpMessage;
  /** a §6.14 notification, including ones this build does not know */
  notification: { method: string; params: unknown };
  /** §1.5 — a backpressure drop */
  drop: StreamDropNotification;
  /** §10.2 H10 — a `seq` gap detected from the stream itself */
  gap: SeqGapEvent;
  state: VwpConnectionState;
  /** a malformed frame or an unsupported version */
  protocolerror: ProtocolError;
  close: { code: number; reason: string };
}

type Listener<K extends keyof VwpClientEvents> = (payload: VwpClientEvents[K]) => void;

/**
 * The surface both {@link VwpClient} and the worker proxy `VwpWorkerClient` implement, so an app
 * can switch between decoding on the main thread and decoding in a worker without changing code.
 */
export interface VwpClientApi {
  readonly state: VwpConnectionState;
  readonly hello: HelloMessage | null;
  readonly poses: PoseBuffer;
  readonly slots: SlotTable;
  connect(): Promise<HelloMessage>;
  close(code?: number, reason?: string): void;
  request<M extends VwpMethodName>(method: M, params: ParamsOf<M>): Promise<ResultOf<M>>;
  on<K extends keyof VwpClientEvents>(event: K, listener: (payload: VwpClientEvents[K]) => void): () => void;
  onHello(listener: (payload: HelloMessage) => void): () => void;
  onKeyframe(listener: (payload: KeyframeMessage) => void): () => void;
  onDelta(listener: (payload: DeltaMessage) => void): () => void;
  onTelemetry(listener: (payload: TelemetryMessage) => void): () => void;
  onEvent(listener: (payload: EventMessage) => void): () => void;
  onMetric(listener: (payload: MetricSampleMessage) => void): () => void;
  onProvenance(listener: (payload: ProvenanceMessage) => void): () => void;
  onNotification(listener: (payload: { method: string; params: unknown }) => void): () => void;
  onDrop(listener: (payload: StreamDropNotification) => void): () => void;
  onState(listener: (payload: VwpConnectionState) => void): () => void;
}

/**
 * A VWP v1 client.
 *
 * Usage:
 * ```ts
 * const client = new VwpClient({ url: "ws://127.0.0.1:8787", compress: "none" });
 * client.onHello((h) => console.log(h.engineVersion, h.nodes.count));
 * client.onKeyframe((kf) => draw(client.poses.positions, client.poses.count));
 * await client.connect();
 * const status = await client.rpc.request("run.status", {});
 * ```
 */
export class VwpClient implements VwpClientApi {
  readonly rpc: JsonRpcClient;
  /** The per-connection symbol table (§2.5). */ readonly strings = new StringTable();
  /** Quantised actor state, kept in step with keyframes and deltas (§3.3/§3.4). */ readonly poses: PoseBuffer;
  /** The slot model of §3.3.1. */ readonly slots = new SlotTable();
  /** The client side of §1.4's resume ring. */ readonly ring: SeqRing;

  #options: VwpClientOptions;
  #socket: WebSocketLike | null = null;
  #state: VwpConnectionState = "idle";
  #listeners = new Map<string, Set<(payload: never) => void>>();
  #hello: HelloMessage | null = null;
  #attempt = 0;
  #reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  #closedByUs = false;
  #decodeOptions: DecodeOptions;
  #trackPoses: boolean;
  #helloTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(options: VwpClientOptions) {
    this.#options = options;
    this.#decodeOptions = options.decompress ? { decompress: options.decompress } : {};
    this.#trackPoses = options.trackPoses ?? true;
    this.poses = new PoseBuffer(options.poseCapacity ?? 1024);
    this.ring = new SeqRing(options.ringFrames ?? 4096, options.ringBytes ?? 8 * 1024 * 1024);
    this.rpc = new JsonRpcClient((text) => this.#sendText(text), {
      ...options.rpc,
      onUnknownNotification: (method, params) => {
        options.rpc?.onUnknownNotification?.(method, params);
        this.#emit("notification", { method, params });
      },
    });
    for (const n of ["run.state", "stream.drop", "job.progress", "job.done", "view.changed", "log", "validation", "experiment.progress"] as const) {
      this.rpc.on(n, (params) => {
        this.#emit("notification", { method: n, params });
        if (n === "stream.drop") this.#emit("drop", params as StreamDropNotification);
      });
    }
  }

  /** Where the state machine of §1 is. */
  get state(): VwpConnectionState {
    return this.#state;
  }

  /** The `Hello` of the current connection, or `null` before the handshake completes. */
  get hello(): HelloMessage | null {
    return this.#hello;
  }

  /** §3.1.2 — is this a `node`-profile connection? */
  get isNodeProfile(): boolean {
    return this.#hello !== null && (this.#hello.helloFlags & HelloFlags.NODE_ONLY) !== 0;
  }

  /** The URL this client will open, including the `?resume=` of §1.4 when resuming. */
  endpointUrl(): string {
    const base = this.#options.url.replace(/^http/, "ws").replace(/\/+$/, "");
    const params = new URLSearchParams();
    if (this.#options.run) params.set("run", this.#options.run);
    const resume = this.ring.nextSeq;
    if (resume !== null) params.set("resume", resume.toString());
    if (this.#options.profile) params.set("profile", this.#options.profile);
    params.set("compress", this.#options.compress ?? (this.#options.decompress ? "zstd" : "none"));
    params.set("v", "1");
    return `${base}/vwp/v1?${params.toString()}`;
  }

  /** Open the connection and resolve when `Hello` has arrived (§1.3). */
  connect(): Promise<HelloMessage> {
    return new Promise<HelloMessage>((resolve, reject) => {
      const offHello = this.on("hello", (h) => {
        offHello();
        offErr();
        offClose();
        resolve(h);
      });
      const offErr = this.on("protocolerror", (e) => {
        offHello();
        offErr();
        offClose();
        reject(e);
      });
      const offClose = this.on("close", (c) => {
        if (this.#hello) return;
        offHello();
        offErr();
        offClose();
        reject(new Error(`socket closed before Hello (code ${c.code})`));
      });
      this.#open();
    });
  }

  /** Close the connection and stop reconnecting. */
  close(code = 1000, reason = "client closed"): void {
    this.#closedByUs = true;
    if (this.#reconnectTimer) clearTimeout(this.#reconnectTimer);
    this.#reconnectTimer = null;
    if (this.#helloTimer) clearTimeout(this.#helloTimer);
    this.#helloTimer = null;
    this.rpc.rejectAll(new Error("connection closed"));
    try {
      this.#socket?.close(code, reason);
    } catch {
      /* already closed */
    }
    this.#socket = null;
    this.#setState("closed");
  }

  /** Subscribe to an event; returns an unsubscribe function. */
  on<K extends keyof VwpClientEvents>(event: K, listener: Listener<K>): () => void {
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

  /** §3.1 — the connection `Hello`. */
  onHello(listener: Listener<"hello">): () => void { return this.on("hello", listener); }
  /** §3.3 */
  onKeyframe(listener: Listener<"keyframe">): () => void { return this.on("keyframe", listener); }
  /** §3.4 */
  onDelta(listener: Listener<"delta">): () => void { return this.on("delta", listener); }
  /** §3.5 */
  onTelemetry(listener: Listener<"telemetry">): () => void { return this.on("telemetry", listener); }
  /** §3.6 */
  onEvent(listener: Listener<"event">): () => void { return this.on("event", listener); }
  /** §3.7 */
  onMetric(listener: Listener<"metric">): () => void { return this.on("metric", listener); }
  /** §3.8 */
  onProvenance(listener: Listener<"provenance">): () => void { return this.on("provenance", listener); }
  /** §3.9 */
  onWorldChunk(listener: Listener<"worldchunk">): () => void { return this.on("worldchunk", listener); }
  /** §3.10 */
  onStreamError(listener: Listener<"streamerror">): () => void { return this.on("streamerror", listener); }
  /** §3.11 */
  onBye(listener: Listener<"bye">): () => void { return this.on("bye", listener); }
  /** §6.14 — any notification, known or not. */
  onNotification(listener: Listener<"notification">): () => void { return this.on("notification", listener); }
  /** §1.5 — a backpressure drop. */
  onDrop(listener: Listener<"drop">): () => void { return this.on("drop", listener); }
  /** §10.2 H10 — a `seq` gap. */
  onGap(listener: Listener<"gap">): () => void { return this.on("gap", listener); }
  /** Connection-state transitions. */
  onState(listener: Listener<"state">): () => void { return this.on("state", listener); }

  /** Typed JSON-RPC call (§6). */
  request<M extends VwpMethodName>(method: M, params: ParamsOf<M>): Promise<ResultOf<M>> {
    return this.rpc.request(method, params);
  }

  /** Subscribe to one §6.14 notification with its typed params. */
  onRpcNotification<N extends VwpNotificationName>(method: N, handler: (params: VwpNotifications[N]) => void): () => void {
    return this.rpc.on(method, handler);
  }

  /** Feed a binary frame directly — the path a worker or a test harness uses. */
  handleBinaryFrame(frame: ArrayBuffer): VwpMessage | null {
    let msg: VwpMessage;
    try {
      const view = viewFrame(frame, this.#decodeOptions);
      if (isCanonicalMsgType(view.header.msgType)) {
        const { gap } = this.ring.push(view.header.seq, view.header.msgType, frame);
        if (gap > 0n) {
          this.#emit("gap", { expected: view.header.seq - gap, received: view.header.seq, missing: gap });
        }
      }
      msg = decodeFrameView(view, this.strings);
    } catch (err) {
      if (err instanceof ProtocolError) {
        this.#emit("protocolerror", err);
        // §2.1 / Appendix A: a malformed frame or a bad version closes the socket.
        this.close(err.closeCode, err.message);
        return null;
      }
      throw err;
    }
    try {
      this.#route(msg);
    } catch (err) {
      // Applying a frame can fail the same way decoding it can — a delta that names a slot beyond
      // the §3.1.1 capacity bound, a resumed `Hello` that contradicts the symbol table it is
      // resuming. Those are protocol violations, so they close the socket with the same close code
      // instead of escaping the socket's message handler.
      if (err instanceof ProtocolError) {
        this.#emit("protocolerror", err);
        this.close(err.closeCode, err.message);
        return null;
      }
      throw err;
    }
    return msg;
  }

  /** Feed a text frame directly. */
  handleTextFrame(text: string): void {
    this.rpc.handleText(text);
  }

  // -------------------------------------------------------------------------

  #setState(state: VwpConnectionState): void {
    if (this.#state === state) return;
    this.#state = state;
    this.#emit("state", state);
  }

  #emit<K extends keyof VwpClientEvents>(event: K, payload: VwpClientEvents[K]): void {
    const set = this.#listeners.get(event);
    if (!set) return;
    for (const l of set) (l as Listener<K>)(payload);
  }

  #sendText(text: string): void {
    const sock = this.#socket;
    if (!sock) throw new Error("not connected");
    sock.send(text);
  }

  #defaultFactory(): WebSocketFactory {
    const ctor = (globalThis as { WebSocket?: new (url: string, protocols?: string | string[]) => WebSocketLike }).WebSocket;
    if (!ctor) throw new Error("no WebSocket implementation available; pass options.socketFactory");
    return (url, protocols) => new ctor(url, protocols);
  }

  #open(): void {
    this.#closedByUs = false;
    this.#setState(this.#attempt === 0 ? "connecting" : "reconnecting");
    const factory = this.#options.socketFactory ?? this.#defaultFactory();
    const sock = factory(this.endpointUrl(), [VWP_SUBPROTOCOL]);
    sock.binaryType = "arraybuffer";
    this.#socket = sock;

    sock.addEventListener("open", () => {
      this.#setState("handshaking");
      // §1.3 — the server must send Hello within 1000 ms of the upgrade.
      this.#helloTimer = setTimeout(() => {
        if (!this.#hello) {
          this.#emit("protocolerror", new ProtocolError("bad_state", "no Hello within 1000 ms of the upgrade (§1.3)"));
          this.#socket?.close(1002, "no Hello");
        }
      }, 1000);
    });

    sock.addEventListener("message", (ev) => {
      const data = ev.data;
      if (typeof data === "string") {
        this.handleTextFrame(data);
      } else if (data instanceof ArrayBuffer) {
        this.handleBinaryFrame(data);
      } else if (ArrayBuffer.isView(data)) {
        const view = data as ArrayBufferView;
        this.handleBinaryFrame(view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength) as ArrayBuffer);
      }
    });

    sock.addEventListener("close", (ev) => {
      if (this.#helloTimer) clearTimeout(this.#helloTimer);
      this.#helloTimer = null;
      this.#socket = null;
      this.rpc.rejectAll(new Error(`socket closed (${ev.code})`));
      this.#emit("close", { code: ev.code, reason: ev.reason });
      if (this.#closedByUs) {
        this.#setState("closed");
        return;
      }
      // §1.4 — stop retrying after 4406 (unsupported version) or 4404 (unknown run).
      if (ev.code === 4406 || ev.code === 4404 || ev.code === 4401) {
        this.#setState("failed");
        return;
      }
      if (this.#options.autoReconnect === false) {
        this.#setState("closed");
        return;
      }
      this.#scheduleReconnect();
    });

    sock.addEventListener("error", () => {
      /* the close event carries the outcome */
    });
  }

  #scheduleReconnect(): void {
    const min = this.#options.backoffMinMs ?? 250;
    const max = this.#options.backoffMaxMs ?? 8000;
    const jitter = this.#options.backoffJitter ?? 0.2;
    const base = Math.min(max, min * 2 ** this.#attempt);
    const delay = Math.max(0, base * (1 + (Math.random() * 2 - 1) * jitter));
    this.#attempt += 1;
    this.#setState("reconnecting");
    this.#reconnectTimer = setTimeout(() => this.#open(), delay);
  }

  /**
   * §1.4 case 1 / §2.5 — reconcile a resumed `Hello`'s table with the one the client already holds.
   *
   * The held entries win, because ids are never reassigned within a connection. The Hello's copy of
   * ids `0..n-1` must agree with what is held — a disagreement means the server renumbered the
   * table, which would shift every later string resolution, so it is a protocol error rather than a
   * silent overwrite. Ids the client does not hold yet (a client resuming from persisted state) are
   * appended so no id is left undefined.
   */
  #mergeResumedStrings(strings: readonly string[]): void {
    const held = this.strings.size;
    const shared = Math.min(held, strings.length);
    for (let id = 1; id < shared; id++) {
      if (this.strings.get(id) !== strings[id]) {
        throw new ProtocolError(
          "bad_state",
          `resumed Hello redefines string id ${id} from "${this.strings.get(id)}" to "${strings[id]}"; §2.5 says ids are never reassigned within a connection`,
          { offset: id, expected: this.strings.get(id), actual: strings[id], field: "Hello.strings" },
        );
      }
    }
    if (strings.length > held) this.strings.append(strings.slice(held));
  }

  #route(msg: VwpMessage): void {
    switch (msg.kind) {
      case "hello": {
        if (this.#helloTimer) clearTimeout(this.#helloTimer);
        this.#helloTimer = null;
        this.#attempt = 0;
        this.#hello = msg;
        const resumed = (msg.helloFlags & HelloFlags.RESUMED) !== 0;
        if (!resumed) {
          // §1.4 case 2 — discard all stream state and reset the symbol table.
          this.strings.reset(msg.strings);
          this.poses.reset();
          this.slots.reset();
          this.ring.clear();
        } else {
          // §1.4 case 1 / §2.5 — on a successful resume the client KEEPS its world, string table,
          // actor slots and camera state, and ids are never reassigned within a connection. Only a
          // non-resumed `Hello` resets the table. Resetting here would truncate it to the n ids the
          // Hello re-establishes and discard every id a `Provenance` frame appended, so every later
          // `str_*` pointing above n would render blank.
          this.#mergeResumedStrings(msg.strings);
        }
        this.ring.seed(msg.resumeSeq);
        if (this.#trackPoses && msg.actorCapacity > 0) {
          const capacity = Math.min(msg.actorCapacity, 1 << 20);
          this.poses.ensureCapacity(capacity);
          this.slots.ensureCapacity(capacity);
          // §3.1.1 / §3.4.5 — bound the slot ids a `Delta` may name (see PoseBuffer.slotLimit).
          this.poses.setSlotBound(capacity);
          this.slots.setSlotBound(capacity);
        }
        this.#setState("streaming");
        this.#emit("hello", msg);
        break;
      }
      case "keyframe":
        if (this.#trackPoses) {
          this.poses.applyKeyframe(msg);
          this.slots.adoptKeyframe(msg.actors.actorId, msg.gopIndex);
        }
        this.#emit("keyframe", msg);
        break;
      case "delta":
        if (this.#trackPoses) {
          this.poses.applyDelta(msg);
          for (let i = 0; i < msg.spawns.count; i++) this.slots.adoptSpawn(msg.spawns.slot[i], msg.spawns.actorId[i]);
          for (let i = 0; i < msg.despawns.count; i++) this.slots.release(msg.despawns.slot[i]);
        }
        this.#emit("delta", msg);
        break;
      case "telemetry": this.#emit("telemetry", msg); break;
      case "event": this.#emit("event", msg); break;
      case "metric": this.#emit("metric", msg); break;
      case "provenance":
        if (msg.stringExtension.length > 0) this.strings.append(msg.stringExtension);
        this.#emit("provenance", msg);
        break;
      case "world-chunk": this.#emit("worldchunk", msg); break;
      case "error":
        // §3.10/§3.11 carry a "symbol-table extension" like §3.8's, so it extends the connection
        // table and its ids are never reassigned.
        if (msg.stringExtension.length > 0) this.strings.append(msg.stringExtension);
        this.#emit("streamerror", msg);
        break;
      case "bye":
        if (msg.stringExtension.length > 0) this.strings.append(msg.stringExtension);
        this.#emit("bye", msg);
        // §1.4 — never retry after run-complete.
        if (msg.reason === ByeReason.RUN_COMPLETE) {
          this.#closedByUs = true;
          this.#setState("failed");
        }
        break;
      default:
        break; // §2.1 — ignore an unknown msg_type and keep the connection.
    }
    this.#emit("message", msg);
  }
}

/** Re-exported so callers can test flags without importing `frame.js` separately. */
export { FrameFlags, MsgType, parseFrameHeader };
