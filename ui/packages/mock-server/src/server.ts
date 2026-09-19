/**
 * The VWP v1 mock engine server — HTTP companions (§1.1), the binary stream (§2, §3) and the
 * JSON-RPC control surface (§6), over one WebSocket per client.
 *
 * It is a development and test fixture, not production code, but the bytes it writes are the
 * specification's bytes: it is what proves the client decoder right.
 */

import { createHash, randomUUID } from "node:crypto";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";

import {
  ChannelId,
  FrameFlags,
  HelloFlags,
  MsgType,
  RpcErrorCode,
  SENTINEL_U32,
  StringTableBuilder,
  VWB_CONTENT_TYPE,
  VWP_METHODS,
  VWP_SUBPROTOCOL,
  encodeByeBody,
  encodeErrorBody,
  encodeEventBody,
  encodeHelloBody,
  encodeMetricSampleBody,
  encodeProvenanceBody,
  encodeTelemetryBody,
  frameOf,
  worldToJson,
  decodeWorld,
  type ChannelRowInit,
  type ClassRowInit,
  type HelloInit,
  type JsonRpcRequest,
  type NodeRowInit,
  type RunState,
} from "@vwp/protocol";
import { WebSocketServer, type WebSocket } from "ws";

import { MANHATTAN_BBOX, generateManhattan, type GeneratedWorld } from "./manhattan.js";
import { ACTOR_CLASSES, ACTOR_NODE_BASE, MockRun, type Profile } from "./sim.js";
import type { GeoBbox } from "./geo.js";

const sha256 = (bytes: Uint8Array): Uint8Array => new Uint8Array(createHash("sha256").update(bytes).digest());

/** Options for {@link MockEngineServer}. */
export interface MockServerOptions {
  readonly port?: number;
  readonly host?: string;
  /** Actors to drive. The default is 200; 5000 is the stress point the UI must handle. */
  readonly actors?: number;
  readonly seed?: number;
  /** Multiple of real time the stream is produced at; 0 means as fast as the timer allows. */
  readonly speed?: number;
  readonly bbox?: GeoBbox;
  /** Start the run paused at t = 0 (`HELLO_PAUSED`). */ readonly paused?: boolean;
  readonly scenarioName?: string;
  readonly runLabel?: string;
  readonly quiet?: boolean;
}

/** §1.5 — the bounded send queue this fixture enforces. */
const MAX_QUEUED_BYTES = 8 * 1024 * 1024;
const RESYNC_DEADLINE_MS = 250;
/** §1.4 — the resume ring. */
const RING_MAX_FRAMES = 4096;
const RING_MAX_BYTES = 8 * 1024 * 1024;

interface RingEntry {
  readonly seq: bigint;
  readonly msgType: number;
  readonly frame: Uint8Array;
  readonly gopIndex: number;
}

interface Conn {
  readonly ws: WebSocket;
  readonly id: number;
  readonly profile: Profile;
  /** Channels this connection subscribed to; §6.12 says none are subscribed by default. */
  channels: Set<number>;
  maxEventsPerStep: number;
  /** Nodes subscribed to `Telemetry` through `view.follow` (§6.7). */
  telemetryNodes: Set<number>;
  following: number | null;
  overlays: Map<string, boolean>;
  helloSent: boolean;
  resyncPending: boolean;
  droppedSinceResync: { delta: number; event: number; telemetry: number; metric: number };
  dropSeqFirst: bigint | null;
  dropSeqLast: bigint | null;
  dropAtMs: number;
  alive: boolean;
  /**
   * §2.5 — the connection's symbol-table size. `Hello` resets it; every extension (`Provenance`,
   * `Error`, `Bye`) appends after it, and ids are never reassigned within the connection.
   */
  stringCount: number;
}

/** A mock engine: one world, one run, many connections. */
export class MockEngineServer {
  readonly world: GeneratedWorld;
  readonly run: MockRun;
  readonly runId = randomUUID();

  #options: MockServerOptions;
  #http: Server;
  #wss: WebSocketServer;
  #conns = new Map<number, Conn>();
  #nextConnId = 1;
  #seq = 0n;
  #ring: RingEntry[] = [];
  #ringBytes = 0;
  #timer: ReturnType<typeof setInterval> | null = null;
  #pingTimer: ReturnType<typeof setInterval> | null = null;
  #state: RunState = "running";
  #speed: number;
  #sync: "free" | "client" = "free";
  #stepsSinceTelemetry = 0;
  #stepsSinceMetric = 0;
  #worldJson: string;
  #scenario: Record<string, unknown>;
  #scenarioHash: Uint8Array;
  #runIdBytes: Uint8Array;
  #strings: StringTableBuilder;
  #strIds: {
    engineVersion: number; scenarioName: number; runLabel: number; sessionToken: number; worldUrl: number;
    classNames: number[]; channelNames: Map<number, number>; metricNames: Map<string, number>;
    profileObu: number; profileRsu: number; profileVru: number;
  };
  #stepCount = 0;
  /**
   * §2.5 — the connection symbol table is append-only and ids are never reassigned. `Hello`
   * establishes `0..n-1`; each `Provenance`, `Error` and `Bye` extension appends after that, so
   * the server tracks how far past the `Hello` table the shared id space has grown.
   */
  /** Node labels, interned once so the symbol table is frozen after construction (see below). */
  #labelIds = new Map<number, number>();

  constructor(options: MockServerOptions = {}) {
    this.#options = options;
    this.#speed = options.speed ?? 1;
    this.#state = options.paused ? "paused" : "running";

    this.world = generateManhattan({ bbox: options.bbox ?? MANHATTAN_BBOX, seed: options.seed ?? 20260918 }, sha256);
    this.run = new MockRun({ world: this.world, actors: options.actors ?? 200, seed: (options.seed ?? 20260918) + 7 });
    this.#worldJson = JSON.stringify(
      worldToJson(decodeWorld(this.world.vwb.buffer.slice(this.world.vwb.byteOffset, this.world.vwb.byteOffset + this.world.vwb.byteLength) as ArrayBuffer)),
    );

    this.#scenario = {
      schema: "v2xw/scenario/1",
      meta: { name: options.scenarioName ?? "manhattan-midtown-mock", description: "Synthetic midtown grid served by the VWP mock server" },
      time: { t0: "2027-03-04T07:00:00Z", duration_s: Number(this.run.timing.durationNs / 1_000_000_000n), step_ms: 100 },
      world: { source: "synthetic", bbox: [...(options.bbox ?? MANHATTAN_BBOX)], buildings: true },
      traffic: { actors: options.actors ?? 200, classes: ACTOR_CLASSES.map((c) => c.name) },
      radio: { tiers: { phy: "abstract", mac: "abstract" } },
      security: { profile: "scms", attackers: { share: 0.03 } },
      seed: options.seed ?? 20260918,
    };
    this.#scenarioHash = sha256(new TextEncoder().encode(JSON.stringify(this.#scenario)));
    this.#runIdBytes = Uint8Array.from(this.runId.replace(/-/g, "").match(/.{2}/g)?.map((h) => Number.parseInt(h, 16)) ?? []);

    // The symbol table is laid out deliberately: ids 1 and 2 are the app and detector ids the
    // event payloads in sim.ts reference, so the two agree without a lookup at emit time.
    this.#strings = new StringTableBuilder();
    this.#strings.add("fcw"); // 1
    this.#strings.add("plausibility/speed-consistency"); // 2
    const engineVersion = this.#strings.add("vwp-mock-server 0.1.0");
    const scenarioName = this.#strings.add(String((this.#scenario.meta as { name: string }).name));
    const runLabel = this.#strings.add(options.runLabel ?? "mock");
    const sessionToken = this.#strings.add("");
    const worldUrl = this.#strings.add(`/world/${this.world.contentHashHex}.vwb`);
    const classNames = ACTOR_CLASSES.map((c) => this.#strings.add(c.name));
    const profileObu = this.#strings.add("obu/cohda-mk5");
    const profileRsu = this.#strings.add("rsu/cohda-mk5-rsu");
    const profileVru = this.#strings.add("vru/phone-generic");
    const channelNames = new Map<number, number>();
    for (const [id, name] of Object.entries(CHANNEL_NAMES)) channelNames.set(Number(id), this.#strings.add(name));
    const metricNames = new Map<string, number>();
    for (const name of MockRun.metricNames()) metricNames.set(name, this.#strings.add(name));
    this.#strIds = {
      engineVersion, scenarioName, runLabel, sessionToken, worldUrl,
      classNames, channelNames, metricNames, profileObu, profileRsu, profileVru,
    };
    // §2.5 — a non-resumed `Hello` resets the connection table, and ids are never reassigned
    // within a connection. Every `Hello` must therefore carry the *same* table, so every string
    // this server will ever reference is interned here and the table is frozen afterwards. Node
    // labels are part of that, so they are interned for the actors alive at construction; an actor
    // that respawns later carries `str_label = 0` ("") and is identified by its ids instead.
    for (const site of this.world.init.sites) {
      this.#labelIds.set(site.nodeId, this.#strings.add(`rsu_${String(site.siteId).padStart(3, "0")}`));
    }
    for (const nodeId of this.run.nodeIds()) {
      this.#labelIds.set(nodeId, this.#strings.add(`veh_${String(nodeId - ACTOR_NODE_BASE).padStart(5, "0")}`));
    }
    Object.freeze(this.#labelIds);

    this.#http = createServer((req, res) => this.#handleHttp(req, res));
    this.#wss = new WebSocketServer({
      noServer: true,
      handleProtocols: (protocols) => (protocols.has(VWP_SUBPROTOCOL) ? VWP_SUBPROTOCOL : false),
    });
    this.#http.on("upgrade", (req, socket, head) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname !== "/vwp/v1") {
        socket.destroy();
        return;
      }
      // §1.1 — a client that does not speak vwp.v1 must fail the upgrade with HTTP 426.
      const offered = (req.headers["sec-websocket-protocol"] ?? "").toString().split(",").map((s) => s.trim());
      if (offered.length > 0 && offered[0] !== "" && !offered.includes(VWP_SUBPROTOCOL)) {
        socket.write(`HTTP/1.1 426 Upgrade Required\r\nUpgrade: ${VWP_SUBPROTOCOL}\r\n\r\n`);
        socket.destroy();
        return;
      }
      this.#wss.handleUpgrade(req, socket, head, (ws) => this.#onConnection(ws, url));
    });
  }

  /** The `vwp-world/1` content hash, which is also `Hello.world_hash` and the `.vwb` URL. */
  get worldHashHex(): string {
    return this.world.contentHashHex;
  }

  /** Start listening. Resolves with the bound address. */
  start(): Promise<{ host: string; port: number; wsUrl: string; httpUrl: string }> {
    const port = this.#options.port ?? 8787;
    const host = this.#options.host ?? "127.0.0.1";
    return new Promise((resolve, reject) => {
      this.#http.once("error", reject);
      this.#http.listen(port, host, () => {
        const address = this.#http.address();
        const boundPort = typeof address === "object" && address !== null ? address.port : port;
        // Latch the run's opening keyframe: §3.2's delta reference state must exist before the
        // first delta, and §1.3 rule 2 wants a RESYNC keyframe first anyway.
        this.#emitKeyframe(FrameFlags.RESYNC);
        this.#startPump();
        resolve({
          host,
          port: boundPort,
          wsUrl: `ws://${host}:${boundPort}/vwp/v1`,
          httpUrl: `http://${host}:${boundPort}`,
        });
      });
    });
  }

  /** Stop the pump, say goodbye to every client and close the listener. */
  async stop(): Promise<void> {
    if (this.#timer) clearInterval(this.#timer);
    if (this.#pingTimer) clearInterval(this.#pingTimer);
    this.#timer = null;
    this.#pingTimer = null;
    for (const conn of this.#conns.values()) {
      this.#sendFrame(conn, MsgType.Bye, encodeByeBody({
        simTimeNs: this.run.simTimeNs, canonicalFrames: this.#seq, reason: 2, detail: "server shutdown",
        firstStringId: this.#nextStringId(conn, 1),
      }));
      conn.ws.close(1000, "server shutdown");
    }
    this.#conns.clear();
    await new Promise<void>((resolve) => {
      this.#wss.close(() => resolve());
    });
    await new Promise<void>((resolve) => {
      this.#http.close(() => resolve());
    });
  }

  // -------------------------------------------------------------------------
  // HTTP (§1.1)
  // -------------------------------------------------------------------------

  #crossOriginHeaders(): Record<string, string> {
    // §1.1 — required on every HTTP response and on the upgrade, for SharedArrayBuffer.
    return {
      "cross-origin-opener-policy": "same-origin",
      "cross-origin-embedder-policy": "require-corp",
      "cross-origin-resource-policy": "same-origin",
    };
  }

  #handleHttp(req: IncomingMessage, res: ServerResponse): void {
    const url = new URL(req.url ?? "/", "http://localhost");
    const headers = this.#crossOriginHeaders();

    if (url.pathname === "/healthz") {
      this.#json(res, 200, { ok: true, engine: "vwp-mock-server 0.1.0", runs: [this.runId] });
      return;
    }

    const worldMatch = /^\/world\/([0-9a-f]{64})\.(vwb|json)$/.exec(url.pathname);
    if (worldMatch) {
      const [, hash, ext] = worldMatch;
      if (hash !== this.world.contentHashHex) {
        this.#json(res, 404, { error: "world_not_found", have: this.world.contentHashHex });
        return;
      }
      const immutable = { "cache-control": "public, max-age=31536000, immutable", etag: `"${hash}"` };
      if (ext === "vwb") {
        res.writeHead(200, { ...headers, ...immutable, "content-type": VWB_CONTENT_TYPE, "content-length": String(this.world.vwb.byteLength) });
        res.end(Buffer.from(this.world.vwb));
      } else {
        const body = Buffer.from(this.#worldJson, "utf8");
        res.writeHead(200, { ...headers, ...immutable, "content-type": "application/json", "content-length": String(body.byteLength) });
        res.end(body);
      }
      return;
    }

    if (url.pathname === "/rpc/schema") {
      this.#json(res, 200, this.#openRpcDocument());
      return;
    }

    if (url.pathname === "/rpc" && req.method === "POST") {
      const chunks: Buffer[] = [];
      req.on("data", (c: Buffer) => chunks.push(c));
      req.on("end", () => {
        let parsed: unknown;
        try {
          parsed = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        } catch {
          this.#json(res, 200, { jsonrpc: "2.0", id: null, error: { code: RpcErrorCode.PARSE_ERROR, message: "invalid JSON" } });
          return;
        }
        if (Array.isArray(parsed)) {
          // §6.1 — batch arrays are not supported.
          this.#json(res, 200, { jsonrpc: "2.0", id: null, error: { code: RpcErrorCode.INVALID_REQUEST, message: "batch requests are not supported" } });
          return;
        }
        const request = parsed as JsonRpcRequest;
        // §6.2 — connection-scoped methods are not available over HTTP.
        if (["view.follow", "view.camera", "overlay.set"].includes(request.method)) {
          this.#json(res, 200, {
            jsonrpc: "2.0", id: request.id ?? null,
            error: { code: RpcErrorCode.NOT_SUPPORTED_HERE, message: `${request.method} is connection-scoped`, data: { why: "HTTP has no connection state" } },
          });
          return;
        }
        const reply = this.#dispatchRpc(null, request);
        this.#json(res, 200, reply ?? { jsonrpc: "2.0", id: request.id ?? null, result: null });
      });
      return;
    }

    this.#json(res, 404, { error: "not_found", paths: ["/healthz", "/world/{hash}.vwb", "/world/{hash}.json", "/rpc", "/rpc/schema", "/vwp/v1"] });
  }

  #json(res: ServerResponse, status: number, value: unknown): void {
    const body = Buffer.from(JSON.stringify(value), "utf8");
    res.writeHead(status, { ...this.#crossOriginHeaders(), "content-type": "application/json", "content-length": String(body.byteLength) });
    res.end(body);
  }

  // -------------------------------------------------------------------------
  // WebSocket (§1.2–§1.4)
  // -------------------------------------------------------------------------

  #onConnection(ws: WebSocket, url: URL): void {
    const profile: Profile = url.searchParams.get("profile") === "node" ? "node" : "full";
    const resumeParam = url.searchParams.get("resume");
    const id = this.#nextConnId++;
    const conn: Conn = {
      ws, id, profile,
      channels: new Set(),
      maxEventsPerStep: 5000,
      telemetryNodes: new Set(),
      following: null,
      overlays: new Map(),
      helloSent: false,
      resyncPending: false,
      droppedSinceResync: { delta: 0, event: 0, telemetry: 0, metric: 0 },
      dropSeqFirst: null,
      dropSeqLast: null,
      dropAtMs: 0,
      alive: true,
      stringCount: 0,
    };
    this.#conns.set(id, conn);

    ws.on("message", (data: Buffer | ArrayBuffer | Buffer[], isBinary: boolean) => {
      if (isBinary) {
        // §1.2 — a conforming client never sends binary in v1.
        this.#sendFrame(conn, MsgType.Error, encodeErrorBody({
          simTimeNs: this.run.simTimeNs, code: RpcErrorCode.INVALID_REQUEST, fatal: false,
          message: "clients must not send binary frames in v1", firstStringId: this.#nextStringId(conn, 2),
        }));
        return;
      }
      const text = Array.isArray(data) ? Buffer.concat(data).toString("utf8") : Buffer.from(data as Buffer).toString("utf8");
      this.#onText(conn, text);
    });
    ws.on("pong", () => {
      conn.alive = true;
    });
    ws.on("close", () => this.#conns.delete(id));
    ws.on("error", () => this.#conns.delete(id));

    // §1.3 — the server sends exactly one Hello immediately, uncompressed, before anything else.
    let resumed = false;
    let resumeFrom: bigint | null = null;
    if (resumeParam !== null && profile === "full") {
      try {
        const want = BigInt(resumeParam);
        const entry = this.#ring.find((e) => e.seq === want);
        const keyframeForGop = entry ? this.#ring.find((e) => e.msgType === MsgType.Keyframe && e.gopIndex === entry.gopIndex) : undefined;
        if (entry && keyframeForGop && keyframeForGop.seq <= want) {
          resumed = true;
          resumeFrom = want;
        }
      } catch {
        resumed = false;
      }
    }

    this.#sendHello(conn, resumed, resumeFrom);
    conn.helloSent = true;

    if (resumed && resumeFrom !== null) {
      for (const entry of this.#ring.filter((e) => e.seq >= resumeFrom)) {
        this.#sendRaw(conn, entry.frame);
      }
    } else {
      // §1.3 rule 2 — the first canonical frame after a non-resumed Hello is a RESYNC keyframe.
      // §1.5: an out-of-band keyframe still consumes a `seq` and is a valid GOP boundary, so it
      // goes to every connection; a keyframe is an idempotent snapshot, so the extra one is free.
      this.#emitKeyframe(FrameFlags.RESYNC);
      this.#sendProvenance();
    }
  }

  #helloInit(conn: Conn, resumed: boolean, resumeFrom: bigint | null): HelloInit {
    const nodes: NodeRowInit[] = [];
    for (const site of this.world.init.sites) {
      nodes.push({
        nodeId: site.nodeId, actorId: SENTINEL_U32, posXM: site.xM, posYM: site.yM,
        posZM: site.zM + site.antennaHeightM,
        strLabel: this.#labelIds.get(site.nodeId) ?? 0,
        strProfileId: this.#strIds.profileRsu,
        flags: 0b0000_0001 | 0b0000_1000, // HAS_HSM | HAS_BACKHAUL
        kind: 2, classIdx: 0xff,
      });
    }
    for (const nodeId of this.run.nodeIds()) {
      const actorId = nodeId - ACTOR_NODE_BASE;
      const info = this.run.actorForNode(nodeId);
      const isVru = info !== null && ACTOR_CLASSES[info.classIdx].category === 1;
      nodes.push({
        nodeId,
        actorId,
        posXM: info?.xM ?? 0,
        posYM: info?.yM ?? 0,
        posZM: 1.5,
        strLabel: this.#labelIds.get(nodeId) ?? 0,
        strProfileId: isVru ? this.#strIds.profileVru : this.#strIds.profileObu,
        // §5.2 — IS_ATTACKER is ground truth and must be 0 in the node profile.
        flags: 0b0000_0001 | (conn.profile === "full" && info !== null && (info.state & 0x01) !== 0 ? 0b0000_0010 : 0),
        kind: isVru ? 1 : 0,
        classIdx: info?.classIdx ?? 0,
      });
    }

    const classes: ClassRowInit[] = ACTOR_CLASSES.map((c, i) => ({
      strName: this.#strIds.classNames[i],
      lengthM: c.lengthM, widthM: c.widthM, heightM: c.heightM, colorRgba: c.colorRgba, category: c.category,
    }));

    // §3.1.5 / §5.2 — GT channels are omitted entirely in the node profile, not merely disabled.
    const channels: ChannelRowInit[] = [];
    for (const [idText, name] of Object.entries(CHANNEL_NAMES)) {
      const channelId = Number(idText);
      const visibility = CHANNEL_VISIBILITY[channelId] ?? 1;
      if (conn.profile === "node" && visibility === 0) continue;
      channels.push({
        strId: this.#strIds.channelNames.get(channelId) ?? 0,
        channelId, visibility, enabled: conn.channels.has(channelId) ? 1 : 0,
      });
    }

    let flags = HelloFlags.LIVE | HelloFlags.WRITABLE;
    if (conn.profile === "node") flags |= HelloFlags.NODE_ONLY;
    if (this.#state === "paused") flags |= HelloFlags.PAUSED;
    if (resumed) flags |= HelloFlags.RESUMED;
    if (this.#ring.some((e) => e.msgType === MsgType.Keyframe)) flags |= HelloFlags.SEEKABLE;

    return {
      helloFlags: flags,
      runId: this.#runIdBytes,
      scenarioHash: this.#scenarioHash,
      worldHash: sha256Hex(this.world.contentHashHex),
      t0WallNs: 1804143600000000000n,
      simDurationNs: this.run.timing.durationNs,
      mobilityStepNs: this.run.timing.mobilityStepNs,
      keyframePeriodNs: this.run.timing.keyframePeriodNs,
      telemetryPeriodNs: this.run.timing.telemetryPeriodNs,
      metricPeriodNs: this.run.timing.metricPeriodNs,
      resumeSeq: resumed && resumeFrom !== null ? resumeFrom : this.#seq,
      simTimeNs: this.run.simTimeNs,
      originLatDeg: this.world.projection.originLatDeg,
      originLonDeg: this.world.projection.originLonDeg,
      originAltM: this.world.projection.originAltM,
      bboxMinXM: this.world.bboxM.minX,
      bboxMinYM: this.world.bboxM.minY,
      bboxMaxXM: this.world.bboxM.maxX,
      bboxMaxYM: this.world.bboxM.maxY,
      actorCapacity: Math.max(1024, Math.ceil(this.run.slotCount * 1.5)),
      nodes,
      classes,
      channels,
      worldRef: { mode: 0, format: 0, payloadBytes: this.world.vwb.byteLength, strUrl: this.#strIds.worldUrl },
      strings: this.#strings.toArray(),
      strEngineVersion: this.#strIds.engineVersion,
      strScenarioName: this.#strIds.scenarioName,
      strRunLabel: this.#strIds.runLabel,
      strSessionToken: this.#strIds.sessionToken,
    };
  }

  #sendHello(conn: Conn, resumed: boolean, resumeFrom: bigint | null): void {
    // §2.6 — Hello is never compressed; this server never compresses anything, and clients
    // therefore must connect with compress=none (§1.1).
    this.#sendFrame(conn, MsgType.Hello, encodeHelloBody(this.#helloInit(conn, resumed, resumeFrom)));
    // §2.5 — a non-resumed Hello resets the table to the ids it establishes.
    if (!resumed) conn.stringCount = this.#strings.size;
  }

  /**
   * Emit one keyframe to every connection: it consumes a `seq`, latches the §3.2 reference state
   * and opens a new GOP, so `step_index` restarts at 1 for the deltas that follow it.
   */
  #emitKeyframe(extraFlags = 0): bigint {
    const seq = this.#seq;
    this.#seq += 1n;
    const fullBody = this.run.buildKeyframeBody("full", true);
    const frame = new Uint8Array(frameOf(MsgType.Keyframe, seq, fullBody, extraFlags));
    this.#pushRing({ seq, msgType: MsgType.Keyframe, frame, gopIndex: this.run.gopIndex });
    let nodeBody: Uint8Array | null = null;
    for (const conn of this.#conns.values()) {
      if (conn.profile === "node") {
        nodeBody = nodeBody ?? this.run.buildKeyframeBody("node", false);
        this.#sendFrame(conn, MsgType.Keyframe, nodeBody, extraFlags | FrameFlags.NODE_ONLY, seq);
      } else {
        this.#sendRaw(conn, frame);
      }
      if (conn.resyncPending) {
        conn.resyncPending = false;
        if (conn.dropSeqFirst !== null) {
          // §1.5 — the drop is reported as a JSON-RPC notification, never in a binary field.
          this.#notify(conn, "stream.drop", {
            seq_first: Number(conn.dropSeqFirst),
            seq_last: Number(conn.dropSeqLast ?? conn.dropSeqFirst),
            dropped: { ...conn.droppedSinceResync },
            resync_seq: Number(seq),
          });
          conn.dropSeqFirst = null;
          conn.dropSeqLast = null;
          conn.droppedSinceResync = { delta: 0, event: 0, telemetry: 0, metric: 0 };
        }
      }
    }
    return seq;
  }

  // -------------------------------------------------------------------------
  // The frame pump
  // -------------------------------------------------------------------------

  #startPump(): void {
    const stepMs = Number(this.run.timing.mobilityStepNs) / 1e6;
    const wallMs = this.#speed > 0 ? stepMs / this.#speed : 5;
    this.#timer = setInterval(() => this.#tick(), Math.max(2, wallMs));
    // §1.2 — Ping every 15 s, close 1001 after 30 s without a Pong.
    this.#pingTimer = setInterval(() => {
      for (const conn of this.#conns.values()) {
        if (!conn.alive) {
          conn.ws.close(1001, "no pong within 30 s");
          this.#conns.delete(conn.id);
          continue;
        }
        conn.alive = false;
        try {
          conn.ws.ping();
        } catch {
          /* closing */
        }
      }
    }, 15_000);
  }

  #tick(): void {
    if (this.#state !== "running") return;
    this.#produceStep();
  }

  /** Advance one mobility step and emit the frames it produces. */
  #produceStep(): void {
    this.run.advance();
    this.#stepCount += 1;
    const keyframeDue = this.run.simTimeNs % this.run.timing.keyframePeriodNs === 0n;

    if (keyframeDue) {
      this.#emitKeyframe(0);
    } else {
      const fullBody = this.run.buildDeltaBody("full", true);
      this.#broadcastDelta(fullBody);
    }

    // §3.6 — one Event batch per mobility step, subscribed channels only. Event content depends on
    // the connection's subscription, so one `seq` covers the whole batch (see the note in README).
    const subscribers = [...this.#conns.values()].filter((c) => c.channels.size > 0);
    if (subscribers.length > 0) {
      const eventSeq = this.#seq;
      let emitted = false;
      for (const conn of subscribers) {
        const events = this.run.buildEvents(conn.channels, conn.profile, conn.maxEventsPerStep);
        if (events.length === 0) continue;
        const body = encodeEventBody(this.run.simTimeNs - this.run.timing.mobilityStepNs, this.run.simTimeNs, events);
        this.#sendFrame(conn, MsgType.Event, body, conn.profile === "node" ? FrameFlags.NODE_ONLY : 0, eventSeq);
        emitted = true;
      }
      if (emitted) this.#seq += 1n;
    }

    this.#stepsSinceTelemetry += 1;
    const telemetrySteps = Number(this.run.timing.telemetryPeriodNs / this.run.timing.mobilityStepNs);
    if (this.#stepsSinceTelemetry >= telemetrySteps) {
      this.#stepsSinceTelemetry = 0;
      this.#sendTelemetry();
    }

    this.#stepsSinceMetric += 1;
    const metricSteps = Number(this.run.timing.metricPeriodNs / this.run.timing.mobilityStepNs);
    if (this.#stepsSinceMetric >= metricSteps) {
      this.#stepsSinceMetric = 0;
      this.#sendMetrics();
    }
  }

  /**
   * Send one `Delta` to every connection, honouring §1.5: a delta is droppable all-or-nothing when
   * the socket is over its byte cap, and a dropped delta schedules a RESYNC keyframe.
   */
  #broadcastDelta(fullBody: Uint8Array): void {
    const seq = this.#seq;
    this.#seq += 1n;
    const gopIndex = this.run.gopIndex;
    let cachedNodeBody: Uint8Array | null = null;

    // The ring retains the full-profile frame, which is the canonical stream (§7.2).
    const fullFrame = new Uint8Array(frameOf(MsgType.Delta, seq, fullBody, 0));
    this.#pushRing({ seq, msgType: MsgType.Delta, frame: fullFrame, gopIndex });

    let resyncWanted = false;
    for (const conn of this.#conns.values()) {
      if (!conn.helloSent) continue;
      if (conn.ws.bufferedAmount > MAX_QUEUED_BYTES) {
        // §1.5 step 1 — drop every queued delta and mark a resync pending.
        conn.droppedSinceResync.delta += 1;
        conn.dropSeqFirst = conn.dropSeqFirst ?? seq;
        conn.dropSeqLast = seq;
        if (!conn.resyncPending) conn.dropAtMs = Date.now();
        conn.resyncPending = true;
        continue;
      }
      if (conn.profile === "node") {
        cachedNodeBody = cachedNodeBody ?? this.run.buildDeltaBody("node", false);
        this.#sendFrame(conn, MsgType.Delta, cachedNodeBody, FrameFlags.NODE_ONLY, seq);
      } else {
        this.#sendRaw(conn, fullFrame);
      }
      if (conn.resyncPending && Date.now() - conn.dropAtMs > RESYNC_DEADLINE_MS) resyncWanted = true;
    }
    // §1.5 — synthesise the RESYNC keyframe if the next boundary is more than 250 ms away.
    if (resyncWanted) this.#emitKeyframe(FrameFlags.RESYNC);
  }

  #pushRing(entry: RingEntry): void {
    this.#ring.push(entry);
    this.#ringBytes += entry.frame.byteLength;
    while (this.#ring.length > RING_MAX_FRAMES || this.#ringBytes > RING_MAX_BYTES) {
      const dropped = this.#ring.shift();
      if (!dropped) break;
      this.#ringBytes -= dropped.frame.byteLength;
    }
  }

  #sendTelemetry(): void {
    const seq = this.#seq;
    let emitted = false;
    for (const conn of this.#conns.values()) {
      if (conn.telemetryNodes.size === 0) continue;
      const records = [...conn.telemetryNodes].map((nodeId) => {
        const record = this.run.telemetryFor(nodeId, this.run.timing.telemetryPeriodNs);
        return conn.profile === "node" ? MockRun.blankTelemetryForNodeProfile(record) : record;
      });
      const body = encodeTelemetryBody(this.run.simTimeNs, this.run.timing.telemetryPeriodNs, records);
      this.#sendFrame(conn, MsgType.Telemetry, body, conn.profile === "node" ? FrameFlags.NODE_ONLY : 0, seq);
      emitted = true;
    }
    if (emitted) this.#seq += 1n;
  }

  #sendMetrics(): void {
    if (this.#conns.size === 0) return;
    const seq = this.#seq;
    for (const conn of this.#conns.values()) {
      const samples = this.run.buildMetrics(this.#strIds.metricNames, conn.profile);
      const body = encodeMetricSampleBody(this.run.simTimeNs, this.run.timing.metricPeriodNs, samples);
      this.#sendFrame(conn, MsgType.MetricSample, body, conn.profile === "node" ? FrameFlags.NODE_ONLY : 0, seq);
    }
    this.#seq += 1n;
  }

  /**
   * §3.8 — "The server MUST send at least one `Provenance` frame immediately after the first
   * `Keyframe`, covering every `prov_id` it will reference before the next one." That is per
   * connection, so a client joining mid-run gets one too; a repeat is explicitly allowed
   * ("after `Hello`, then on demand").
   */
  #sendProvenance(): void {
    const extension = [
      "radio/propagation/abstract-unit-disc", "0.1.0", "b3:mock-radio", "/cards/radio.html",
      "detector/plausibility/speed-consistency", "0.1.0", "b3:mock-detector", "/cards/detector.html",
      "rat=dsrc",
    ];
    const build = (base: number): Uint8Array =>
      encodeProvenanceBody(
        this.run.simTimeNs,
        [
          { provId: 1, strModelId: base + 0, strModelVersion: base + 1, strParamSetId: base + 2, strCardUrl: base + 3, family: 0, subjectKind: 4 },
          { provId: 2, strModelId: base + 4, strModelVersion: base + 5, strParamSetId: base + 6, strCardUrl: base + 7, family: 5, subjectKind: 4 },
        ],
        [{ dimKey: 1, strDims: base + 8 }],
        extension,
        0,
      );

    const seq = this.#seq;
    this.#seq += 1n;
    // The ring keeps the canonical form, whose base is the `Hello` table size — which is what a
    // resuming client's table holds, since its `Hello` re-establishes exactly that table.
    this.#pushRing({
      seq, msgType: MsgType.Provenance,
      frame: new Uint8Array(frameOf(MsgType.Provenance, seq, build(this.#strings.size), 0)),
      gopIndex: this.run.gopIndex,
    });
    for (const conn of this.#conns.values()) {
      const body = build(this.#nextStringId(conn, extension.length));
      this.#sendFrame(conn, MsgType.Provenance, body, conn.profile === "node" ? FrameFlags.NODE_ONLY : 0, seq);
    }
  }

  /** The id this connection's next symbol-table extension starts at (§2.5). */
  #nextStringId(conn: Conn | null, appended: number): number {
    if (conn === null) return this.#strings.size;
    const first = conn.stringCount;
    conn.stringCount += appended;
    return first;
  }

  #sendFrame(conn: Conn, msgType: number, body: Uint8Array, flags = 0, seq: bigint = this.#seq): void {
    this.#sendRaw(conn, new Uint8Array(frameOf(msgType, seq, body, flags)));
  }

  #sendRaw(conn: Conn, frame: Uint8Array): void {
    if (conn.ws.readyState !== conn.ws.OPEN) return;
    conn.ws.send(frame, { binary: true });
  }

  #notify(conn: Conn, method: string, params: unknown): void {
    if (conn.ws.readyState !== conn.ws.OPEN) return;
    conn.ws.send(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }

  // -------------------------------------------------------------------------
  // JSON-RPC (§6)
  // -------------------------------------------------------------------------

  #onText(conn: Conn, text: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      this.#reply(conn, null, undefined, { code: RpcErrorCode.PARSE_ERROR, message: "invalid JSON" });
      return;
    }
    if (Array.isArray(parsed)) {
      // §6.1 / §10.7 R10.
      this.#reply(conn, null, undefined, { code: RpcErrorCode.INVALID_REQUEST, message: "batch requests are not supported" });
      return;
    }
    const request = parsed as JsonRpcRequest;
    const reply = this.#dispatchRpc(conn, request);
    if (reply && request.id !== undefined) conn.ws.send(JSON.stringify(reply));
  }

  #reply(conn: Conn, id: string | number | null, result?: unknown, error?: { code: number; message: string; data?: unknown }): void {
    conn.ws.send(JSON.stringify({ jsonrpc: "2.0", id, ...(error ? { error } : { result }) }));
  }

  #dispatchRpc(conn: Conn | null, request: JsonRpcRequest): { jsonrpc: "2.0"; id: string | number | null; result?: unknown; error?: { code: number; message: string; data?: unknown } } | null {
    const id = request.id ?? null;
    const ok = (result: unknown) => ({ jsonrpc: "2.0" as const, id, result });
    const err = (code: number, message: string, data?: unknown) => ({ jsonrpc: "2.0" as const, id, error: { code, message, data } });
    const params = (request.params ?? {}) as Record<string, unknown>;
    const tNs = Number(this.run.simTimeNs);

    switch (request.method) {
      case "run.start": {
        this.#state = params.paused === true ? "paused" : "running";
        for (const c of this.#conns.values()) this.#sendHello(c, false, null);
        this.#emitKeyframe(FrameFlags.RESYNC);
        return ok({
          run_id: this.runId, state: this.#state,
          world_hash: this.world.contentHashHex,
          scenario_hash: Buffer.from(this.#scenarioHash).toString("hex"),
          t_end_ns: Number(this.run.timing.durationNs),
        });
      }
      case "run.pause": {
        if (this.#state !== "running") return err(RpcErrorCode.RUN_NOT_RUNNING, "run is not running");
        this.#state = "paused";
        this.#broadcastNotify("run.state", { state: this.#state, t_ns: Number(this.run.simTimeNs), run_id: this.runId });
        return ok({ state: this.#state, t_ns: Number(this.run.simTimeNs) });
      }
      case "run.resume": {
        if (this.#state !== "paused") return err(RpcErrorCode.RUN_NOT_RUNNING, "run is not paused");
        this.#state = "running";
        this.#broadcastNotify("run.state", { state: this.#state, t_ns: Number(this.run.simTimeNs), run_id: this.runId });
        return ok({ state: this.#state, t_ns: Number(this.run.simTimeNs) });
      }
      case "run.step": {
        const unit = (params.unit as string) ?? "step";
        const count = Math.max(1, Math.min(100_000, Number(params.count ?? 1)));
        const steps = unit === "keyframe" ? count * Number(this.run.timing.keyframePeriodNs / this.run.timing.mobilityStepNs)
          : unit === "second" ? count * Number(1_000_000_000n / this.run.timing.mobilityStepNs)
          : count;
        // §6.6 — every frame the step produced must be flushed before the reply.
        for (let i = 0; i < steps; i++) this.#produceStep();
        return ok({ state: this.#state, t_ns: Number(this.run.simTimeNs), stepped: steps });
      }
      case "run.seek": {
        const endNs = Number(this.run.timing.durationNs);
        let target: number;
        if (typeof params.t_ns === "number") target = params.t_ns;
        else if (typeof params.fraction === "number") target = Math.round(params.fraction * endNs);
        else return err(RpcErrorCode.INVALID_PARAMS, "one of t_ns, fraction or event is required", [{ path: "/t_ns", message: "required" }]);
        if (target < 0 || target > endNs) return err(RpcErrorCode.SEEK_OUT_OF_RANGE, "seek target outside the run", { min_ns: 0, max_ns: endNs });
        const started = Date.now();
        const stepNs = Number(this.run.timing.mobilityStepNs);
        const aligned = BigInt(Math.floor(target / stepNs) * stepNs);
        this.run.seekTo(aligned);
        // §6.6 ordering guarantee: the keyframe (RESYNC | SEEK_RESULT) goes out before the reply.
        // §6.6 ordering guarantee: the keyframe goes out before this reply, and it carries
        // FLAG_SEEK_RESULT | FLAG_RESYNC.
        const keyframeSeq = this.#emitKeyframe(FrameFlags.RESYNC | FrameFlags.SEEK_RESULT);
        if (params.pause_after !== false) this.#state = "paused";
        return ok({
          t_ns: Number(this.run.simTimeNs), keyframe_seq: Number(keyframeSeq), deltas_applied: 0,
          elapsed_ms: Date.now() - started, state: this.#state,
        });
      }
      case "run.speed": {
        const speed = Number(params.speed);
        if (!Number.isFinite(speed) || speed < 0 || speed > 100) {
          return err(RpcErrorCode.INVALID_PARAMS, "speed must be in [0, 100]", [{ path: "/speed", message: "out of range", hint: "0 means unthrottled" }]);
        }
        this.#speed = speed;
        this.#sync = params.sync === "client" ? "client" : "free";
        if (this.#timer) clearInterval(this.#timer);
        this.#startPumpTimerOnly();
        return ok({ speed: this.#speed, sync: this.#sync });
      }
      case "run.stop": {
        this.#state = "finished";
        for (const c of this.#conns.values()) {
          this.#sendFrame(c, MsgType.Bye, encodeByeBody({
            simTimeNs: this.run.simTimeNs, canonicalFrames: this.#seq, reason: 1, detail: "client requested stop",
            firstStringId: this.#nextStringId(c, 1),
          }));
        }
        return ok({ state: this.#state, t_ns: Number(this.run.simTimeNs) });
      }
      case "run.status":
        return ok({
          run_id: this.runId, state: this.#state, t_ns: tNs, t_end_ns: Number(this.run.timing.durationNs),
          speed: this.#speed, sync: this.#sync, profile: conn?.profile ?? "full", live: true,
          wall_elapsed_s: this.#stepCount * (Number(this.run.timing.mobilityStepNs) / 1e9),
          realtime_factor: this.#speed, actors: this.run.actorCount, nodes: this.run.nodeIds().length,
          events_per_s: 0, seq: Number(this.#seq),
          dropped: conn ? { ...conn.droppedSinceResync } : { delta: 0, event: 0, telemetry: 0, metric: 0 },
        });

      case "view.follow": {
        if (!conn) return err(RpcErrorCode.NOT_SUPPORTED_HERE, "view.follow is connection-scoped", { why: "HTTP has no connection state" });
        if (params.clear === true) {
          conn.following = null;
          conn.telemetryNodes.clear();
          return ok({ following: null, subscribed_nodes: [] });
        }
        let nodeId = typeof params.node === "number" ? params.node : null;
        if (nodeId === null && typeof params.actor === "number") nodeId = ACTOR_NODE_BASE + params.actor;
        if (nodeId === null) return err(RpcErrorCode.INVALID_PARAMS, "node or actor is required", [{ path: "/node", message: "required" }]);
        const info = this.run.actorForNode(nodeId);
        if (!info && !this.world.siteNodeIds.includes(nodeId)) return err(RpcErrorCode.UNKNOWN_ID, "unknown node", { kind: "node", id: nodeId });
        conn.following = nodeId;
        if (params.telemetry !== false) conn.telemetryNodes.add(nodeId);
        const radius = Number(params.radius_m ?? 0);
        if (radius > 0 && info) {
          for (const other of this.run.nodeIds()) {
            const o = this.run.actorForNode(other);
            if (o && Math.hypot(o.xM - info.xM, o.yM - info.yM) <= radius) conn.telemetryNodes.add(other);
          }
        }
        return ok({
          following: nodeId,
          camera: (params.camera as string) ?? "chase",
          subscribed_nodes: [...conn.telemetryNodes],
        });
      }
      case "view.camera": {
        if (!conn) return err(RpcErrorCode.NOT_SUPPORTED_HERE, "view.camera is connection-scoped", { why: "HTTP has no connection state" });
        const mode = params.mode as string;
        const result = {
          mode, position: (params.position as unknown) ?? { x: 0, y: 0, z: 400 },
          target: (params.target as unknown) ?? { x: (this.world.bboxM.minX + this.world.bboxM.maxX) / 2, y: (this.world.bboxM.minY + this.world.bboxM.maxY) / 2, z: 0 },
          fov_deg: Number(params.fov_deg ?? 55), projection: (params.projection as string) ?? "perspective",
        };
        this.#broadcastNotify("view.changed", { ...result, following: conn.following });
        return ok(result);
      }
      case "overlay.set": {
        if (!conn) return err(RpcErrorCode.NOT_SUPPORTED_HERE, "overlay.set is connection-scoped", { why: "HTTP has no connection state" });
        const catalogue = OVERLAY_CATALOGUE.map((o) => ({
          name: o.name, visibility: o.visibility, available: !(conn.profile === "node" && o.visibility === "GT"),
          description: o.description, needs_channels: o.needsChannels,
        }));
        if (params.list === true) return ok({ overlays: Object.fromEntries(conn.overlays), catalogue });
        const requested = (params.overlays ?? {}) as Record<string, boolean>;
        for (const [name, on] of Object.entries(requested)) {
          const entry = OVERLAY_CATALOGUE.find((o) => o.name === name);
          if (!entry) return err(RpcErrorCode.INVALID_PARAMS, `unknown overlay ${name}`, [{ path: `/overlays/${name}`, message: "unknown overlay" }]);
          if (conn.profile === "node" && entry.visibility === "GT" && on) {
            // §5.3 — enabling a GT overlay under the node profile is visibility_denied.
            return err(RpcErrorCode.VISIBILITY_DENIED, `${name} is ground truth`, { field: name, visibility: "GT" });
          }
          conn.overlays.set(name, on);
        }
        return ok({ overlays: Object.fromEntries(conn.overlays), catalogue });
      }

      case "inspect.node": {
        const nodeId = Number(params.node);
        const info = this.run.actorForNode(nodeId);
        const isSite = this.world.siteNodeIds.includes(nodeId);
        if (!info && !isSite) return err(RpcErrorCode.UNKNOWN_ID, "unknown node", { kind: "node", id: nodeId });
        const raw = this.run.telemetryFor(nodeId, this.run.timing.telemetryPeriodNs);
        const record = conn?.profile === "node" ? MockRun.blankTelemetryForNodeProfile(raw) : raw;
        return ok({
          node: nodeId, t_ns: tNs,
          kind: isSite ? "rsu" : info && ACTOR_CLASSES[info.classIdx].category === 1 ? "vru-device" : "obu",
          label: isSite ? `rsu_${String(nodeId).padStart(3, "0")}` : `veh_${String(nodeId - ACTOR_NODE_BASE).padStart(5, "0")}`,
          profile_id: isSite ? "rsu/cohda-mk5-rsu" : "obu/cohda-mk5",
          ...(info ? { actor: info.actorId } : {}),
          telemetry: {
            ...Object.fromEntries(Object.entries(record).map(([k, v]) => [k, typeof v === "bigint" ? Number(v) : v])),
            units: { storage_used_b: "bytes", airtime_ms_per_s: "ms/s", cbr_pm: "per-mille", tx_power_cdbm: "centi-dBm" },
          },
          queues: {
            rx: { depth: record.qRxP50, p50: record.qRxP50, p95: record.qRxP95, policy: "drop-oldest", drops: { overflow: record.dropRxOverflow } },
            verify: { depth: record.qVerifyP50, p50: record.qVerifyP50, p95: record.qVerifyP95, policy: "prioritized", drops: { overflow: record.dropVerifyOverflow, policy_skip: record.dropVerifyPolicySkip } },
            tx: { depth: record.qTxP50, p50: record.qTxP50, p95: record.qTxP95, policy: "dcc-gated", drops: { overflow: record.dropTxOverflow } },
          },
          neighbors: Array.from({ length: Math.min(Number(params.limit ?? 50), record.nbrTotal) }, (_, i) => ({
            digest: (BigInt(nodeId) * 2654435761n + BigInt(i)).toString(16).padStart(16, "0").slice(-16),
            verify_state: i < record.nbrVerified ? "verified" : i < record.nbrVerified + record.nbrUnverified ? "unverified" : "revoked",
            last_seen_ns: tNs - i * 100_000_000,
            relevance: Math.max(0, 1 - i / Math.max(1, record.nbrTotal)),
            messages: 10 + i,
          })),
          provenance: [{ prov_id: 1, model_id: "radio/propagation/abstract-unit-disc", model_version: "0.1.0", param_set_id: "b3:mock-radio" }],
        });
      }
      case "inspect.link": {
        const tx = Number(params.tx);
        const rx = Number(params.rx);
        const a = this.run.actorForNode(tx);
        const b = this.run.actorForNode(rx);
        if (!a || !b) return err(RpcErrorCode.UNKNOWN_ID, "unknown node", { kind: "node", id: !a ? tx : rx });
        const distance = Math.hypot(a.xM - b.xM, a.yM - b.yM);
        const base = { kind: "radio" as const, t_ns: tNs, path_loss_db: 40 + 20 * Math.log10(Math.max(1, distance)), rx_power_dbm: 20 - (40 + 20 * Math.log10(Math.max(1, distance))), pdr: Math.max(0, 1 - distance / 700), frames: 10, bytes: 3200 };
        // §5.2 — distance_m and the LOS class are ground truth.
        return ok(conn?.profile === "node" ? base : { ...base, distance_m: distance, los: { class: distance > 200 ? "NLOSb" : "LOS", walls_crossed: distance > 200 ? 1 : 0 } });
      }
      case "inspect.entity": {
        const entity = String(params.entity ?? "");
        if (!["ra", "pca", "ma", "crlg", "ea"].includes(entity)) return err(RpcErrorCode.UNKNOWN_ID, "unknown entity", { kind: "entity", id: entity });
        return ok({
          entity, t_ns: tNs, role: entity,
          state: { queue_depth: 3, processed: this.#stepCount, batch_window_ns: 1_000_000_000 },
          queue: { depth: 3, servers: 4, utilisation: 0.42 },
          storage_bytes: 1_048_576, open_cases: 2, decisions: 7,
        });
      }
      case "explain": {
        const subject = params.subject as { kind?: string; id?: string } | undefined;
        if (!subject || typeof subject.kind !== "string") {
          return err(RpcErrorCode.INVALID_PARAMS, "subject is required", [{ path: "/subject", message: "required" }]);
        }
        if (conn?.profile === "node" && typeof subject.id === "string" && ["mean_speed", "ttc_min", "det_precision"].includes(subject.id)) {
          return err(RpcErrorCode.VISIBILITY_DENIED, "subject is ground truth", { field: subject.id, visibility: "GT" });
        }
        return ok({
          subject,
          value: subject.id === "pdr" ? 0.94 : undefined,
          unit: subject.id === "pdr" ? "ratio" : undefined,
          chain: [
            { prov_id: 1, model_id: "radio/propagation/abstract-unit-disc", model_version: "0.1.0", param_set_id: "b3:mock-radio", family: "radio", card_url: "/cards/radio.html", assumptions: ["unit-disc reception inside 300 m", "no fading"] },
          ],
          definition_md: "Packet delivery ratio: received / (sent x eligible receivers) over the bin.",
          caveats: ["the mock server's radio is a unit disc, not a calibrated model"],
        });
      }

      case "scenario.get": {
        const path = typeof params.path === "string" ? params.path : "";
        let value: unknown = this.#scenario;
        if (path !== "") {
          for (const part of path.split("/").filter((s) => s !== "")) {
            value = (value as Record<string, unknown>)?.[part];
          }
        }
        return ok({ scenario: value ?? null, hash: Buffer.from(this.#scenarioHash).toString("hex") });
      }
      case "scenario.list":
        return ok({
          items: [
            { id: "manhattan-midtown-mock", kind: "preset", name: "Manhattan midtown (mock)", description: "The synthetic grid this fixture serves", tags: ["mock", "urban"] },
          ],
        });
      case "scenario.validate":
        return ok({ valid: true, errors: [], warnings: [{ path: "/radio/tiers/phy", message: "the mock server's radio is not calibrated", severity: "warning" }] });

      case "events.set": {
        if (!conn) return err(RpcErrorCode.NOT_SUPPORTED_HERE, "events.set is connection-scoped", { why: "HTTP has no connection state" });
        const resolve = (names: unknown): number[] => {
          if (!Array.isArray(names)) return [];
          const ids: number[] = [];
          for (const name of names) {
            const id = Object.entries(CHANNEL_NAMES).find(([, n]) => n === name)?.[0];
            if (id !== undefined) ids.push(Number(id));
          }
          return ids;
        };
        const deny = (ids: number[]): { code: number; message: string; data: unknown } | null => {
          for (const id of ids) {
            if (conn.profile === "node" && (CHANNEL_VISIBILITY[id] ?? 1) === 0) {
              return { code: RpcErrorCode.VISIBILITY_DENIED, message: `${CHANNEL_NAMES[id]} is ground truth`, data: { field: CHANNEL_NAMES[id], visibility: "GT" } };
            }
          }
          return null;
        };
        if (params.list === true) {
          return ok({
            subscribed: [...conn.channels].map((id) => ({ channel: CHANNEL_NAMES[id], channel_id: id, visibility: VISIBILITY_NAMES[CHANNEL_VISIBILITY[id] ?? 1] })),
            available: Object.entries(CHANNEL_NAMES)
              .filter(([id]) => !(conn.profile === "node" && (CHANNEL_VISIBILITY[Number(id)] ?? 1) === 0))
              .map(([id, name]) => ({ channel: name, channel_id: Number(id), visibility: VISIBILITY_NAMES[CHANNEL_VISIBILITY[Number(id)] ?? 1] })),
          });
        }
        if (Array.isArray(params.only)) {
          const ids = resolve(params.only);
          const denied = deny(ids);
          if (denied) return err(denied.code, denied.message, denied.data);
          conn.channels = new Set(ids);
        }
        const subscribe = resolve(params.subscribe);
        const denied = deny(subscribe);
        if (denied) return err(denied.code, denied.message, denied.data);
        for (const id of subscribe) conn.channels.add(id);
        for (const id of resolve(params.unsubscribe)) conn.channels.delete(id);
        if (typeof params.max_events_per_step === "number") conn.maxEventsPerStep = params.max_events_per_step;
        return ok({
          subscribed: [...conn.channels].map((id) => ({
            channel: CHANNEL_NAMES[id], channel_id: id,
            visibility: VISIBILITY_NAMES[CHANNEL_VISIBILITY[id] ?? 1],
            est_rate_per_s: id === ChannelId.PHY_RX ? this.run.actorCount * 40 : this.run.actorCount * 10,
          })),
        });
      }
      case "metrics.query": {
        const names = Array.isArray(params.metrics) ? (params.metrics as string[]) : [];
        if (names.length === 0) {
          return ok({
            columns: [], rows: [],
            catalogue: MockRun.metricNames().map((name) => ({
              name, unit: name === "pdr" ? "ratio" : name === "cbr" ? "ratio" : name.endsWith("_ms") ? "ms" : "1",
              dims: ["t"], agg: "mean",
              visibility: ["mean_speed", "ttc_min", "det_precision"].includes(name) ? "GT" : "NODE",
              definition_md: `The mock server's synthetic ${name}.`,
            })),
          });
        }
        const unknownMetric = names.find((n) => !MockRun.metricNames().includes(n));
        if (unknownMetric) return err(RpcErrorCode.UNKNOWN_METRIC, "unknown metric", { metric: unknownMetric, did_you_mean: MockRun.metricNames().slice(0, 3) });
        const gt = names.find((n) => ["mean_speed", "ttc_min", "det_precision"].includes(n));
        if (conn?.profile === "node" && gt) return err(RpcErrorCode.VISIBILITY_DENIED, "metric is ground truth", { field: gt, visibility: "GT" });
        const samples = this.run.buildMetrics(this.#strIds.metricNames, conn?.profile ?? "full");
        const wanted = new Set(names.map((n) => this.#strIds.metricNames.get(n)));
        return ok({
          columns: [
            { name: "t_ns", type: "time_ns", unit: "ns" },
            { name: "metric", type: "string" },
            { name: "value", type: "float" },
          ],
          rows: samples.filter((s) => wanted.has(s.strMetric)).map((s) => [tNs, names.find((n) => this.#strIds.metricNames.get(n) === s.strMetric) ?? "", s.value]),
        });
      }
      case "rpc.discover":
        return ok(this.#openRpcDocument());
      default:
        if (VWP_METHODS.includes(request.method as (typeof VWP_METHODS)[number])) {
          return err(RpcErrorCode.NOT_SUPPORTED_HERE, `${request.method} is not implemented by the mock server`, { why: "development fixture" });
        }
        return err(RpcErrorCode.METHOD_NOT_FOUND, `unknown method ${request.method}`);
    }
  }

  #startPumpTimerOnly(): void {
    const stepMs = Number(this.run.timing.mobilityStepNs) / 1e6;
    const wallMs = this.#speed > 0 ? stepMs / this.#speed : 5;
    this.#timer = setInterval(() => this.#tick(), Math.max(2, wallMs));
  }

  #broadcastNotify(method: string, params: unknown): void {
    for (const conn of this.#conns.values()) this.#notify(conn, method, params);
  }

  /** §6.3 — a minimal but well-formed OpenRPC 1.3.2 document naming all 32 methods. */
  #openRpcDocument(): Record<string, unknown> {
    return {
      openrpc: "1.3.2",
      info: { title: "VWP v1 (mock engine)", version: "1.0.0", description: "The mock server's subset of docs/protocol/vwp-v1.md §6" },
      methods: VWP_METHODS.map((name) => ({
        name,
        summary: `${name} (see docs/protocol/vwp-v1.md §6)`,
        params: [{ name: "params", schema: { type: "object" } }],
        result: { name: "result", schema: { type: "object" } },
      })),
      components: { schemas: {} },
    };
  }
}

function sha256Hex(hex: string): Uint8Array {
  return Uint8Array.from(hex.match(/.{2}/g)?.map((h) => Number.parseInt(h, 16)) ?? []);
}

/** §3.6.2 — the channels this fixture can emit, by id. */
const CHANNEL_NAMES: Record<number, string> = {
  [ChannelId.GT_KINEMATICS]: "gt.kinematics",
  [ChannelId.NODE_TX]: "node.tx",
  [ChannelId.PHY_RX]: "phy.rx",
  [ChannelId.MAC_CBR]: "mac.cbr",
  [ChannelId.SEC_CERT]: "sec.cert",
  [ChannelId.PROTO_REVOCATION]: "proto.revocation",
  [ChannelId.DET_OBSERVATION]: "det.observation",
  [ChannelId.APP_WARNING]: "app.warning",
};

/** §3.6.2 — 0 GT, 1 NODE, 2 PUBLIC, 3 MIXED. */
const CHANNEL_VISIBILITY: Record<number, number> = {
  [ChannelId.GT_KINEMATICS]: 0,
  [ChannelId.NODE_TX]: 1,
  [ChannelId.PHY_RX]: 3,
  [ChannelId.MAC_CBR]: 1,
  [ChannelId.SEC_CERT]: 1,
  [ChannelId.PROTO_REVOCATION]: 2,
  [ChannelId.DET_OBSERVATION]: 1,
  [ChannelId.APP_WARNING]: 1,
};

const VISIBILITY_NAMES = ["GT", "NODE", "PUBLIC", "MIXED", "DERIVED", "META"] as const;

/** §6.7 — the overlay catalogue, with the `_gt` ones marked ground truth. */
const OVERLAY_CATALOGUE = [
  { name: "tx_pulses", visibility: "NODE", description: "a pulse where a node transmitted", needsChannels: ["node.tx"] },
  { name: "links", visibility: "NODE", description: "reception arcs", needsChannels: ["phy.rx"] },
  { name: "cbr_heatmap", visibility: "NODE", description: "channel busy ratio", needsChannels: ["mac.cbr"] },
  { name: "coverage", visibility: "NODE", description: "RSU coverage discs", needsChannels: [] },
  { name: "attackers_gt", visibility: "GT", description: "true attackers", needsChannels: [] },
  { name: "revoked", visibility: "PUBLIC", description: "revoked actors", needsChannels: [] },
  { name: "reported", visibility: "NODE", description: "reported actors", needsChannels: [] },
  { name: "detections", visibility: "NODE", description: "detector observations", needsChannels: ["det.observation"] },
  { name: "backend_flows", visibility: "NODE", description: "backend protocol flows", needsChannels: ["proto.msg"] },
  { name: "focus_region", visibility: "META", description: "the focus region outline", needsChannels: [] },
  { name: "lane_markings", visibility: "PUBLIC", description: "lane centrelines", needsChannels: [] },
  { name: "buildings", visibility: "PUBLIC", description: "extruded footprints", needsChannels: [] },
  { name: "labels", visibility: "PUBLIC", description: "actor labels", needsChannels: [] },
  { name: "trajectories_gt", visibility: "GT", description: "true trajectories", needsChannels: ["gt.kinematics"] },
  { name: "belief_vs_truth_gt", visibility: "GT", description: "believed against true position", needsChannels: ["gt.kinematics"] },
  { name: "signal_state", visibility: "PUBLIC", description: "signal heads", needsChannels: [] },
  { name: "rsu_range", visibility: "NODE", description: "RSU range rings", needsChannels: [] },
  { name: "density", visibility: "DERIVED", description: "actor density", needsChannels: [] },
] as const;
