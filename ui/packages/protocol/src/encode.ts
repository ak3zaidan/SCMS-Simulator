/**
 * Encoders for every VWP v1 body — docs/protocol/vwp-v1.md §2–§4.
 *
 * The client only ever decodes, but the encoders live here so that there is exactly one
 * transcription of the §3 layout tables in TypeScript, and so the golden test in
 * `test/encode-vectors.test.ts` can assert that encoding the §9 example's stated field values
 * reproduces the specification's bytes **exactly**. The mock engine server writes with these.
 */

import { FRAME_HEADER_BYTES, FrameFlags, MsgType, serialiseFrameHeader } from "./frame.js";
import {
  DELTA_PREFIX_BYTES,
  ERROR_OFFSETS,
  EVENT_PREFIX_BYTES,
  HELLO_PREFIX_BYTES,
  KEYFRAME_PREFIX_BYTES,
  METRIC_PREFIX_BYTES,
  METRIC_RECORD_BYTES_V1,
  PROVENANCE_PREFIX_BYTES,
  TELEMETRY_PREFIX_BYTES,
  TELEMETRY_RECORD_BYTES_V1,
  WORLD_CHUNK_PREFIX_BYTES,
  BYE_OFFSETS,
  DELTA_OFFSETS,
  EVENT_OFFSETS,
  HELLO_OFFSETS,
  KEYFRAME_OFFSETS,
  METRIC_OFFSETS,
  METRIC_RECORD_OFFSETS,
  NODE_TELEMETRY_OFFSETS,
  PROVENANCE_OFFSETS,
  TELEMETRY_OFFSETS,
  WORLD_CHUNK_OFFSETS,
  WORLD_REF_OFFSETS,
} from "./messages.js";
import {
  VWB_DIRECTORY_BYTES,
  VWB_DIRECTORY_OFFSETS,
  VWB_HEADER_BYTES,
  VWB_HEADER_OFFSETS,
  VWB_MAGIC,
  VWB_VERSION,
  VWB_CROSSING_STRIDE,
  VWB_JUNCTION_STRIDE,
  VWB_LANDUSE_STRIDE,
  VWB_SIGNAL_STRIDE,
  VWB_SITE_STRIDE,
} from "./world.js";

const UTF8_ENC = new TextEncoder();
const ceil4 = (n: number): number => (n + 3) & ~3;

// ---------------------------------------------------------------------------
// §2.5 — the symbol table
// ---------------------------------------------------------------------------

/** Interns strings in wire order; id 0 is always `""` (§2.5). */
export class StringTableBuilder {
  #strings: string[] = [""];
  #index = new Map<string, number>([["", 0]]);

  /** Intern a string and return its id. */
  add(value: string): number {
    const found = this.#index.get(value);
    if (found !== undefined) return found;
    const id = this.#strings.length;
    this.#strings.push(value);
    this.#index.set(value, id);
    return id;
  }

  /** Number of ids defined. */
  get size(): number {
    return this.#strings.length;
  }

  /** The interned strings in id order. */
  toArray(): string[] {
    return [...this.#strings];
  }

  /** Serialise as a `StrTable` (§2.5). */
  build(): Uint8Array {
    return encodeStrTable(this.#strings);
  }
}

/** Serialise a `StrTable` (§2.5): `n`, `blob_bytes`, `offsets[n+1]`, then the padded UTF-8 blob. */
export function encodeStrTable(strings: readonly string[]): Uint8Array {
  const blobs = strings.map((s) => UTF8_ENC.encode(s));
  const blobBytes = blobs.reduce((n, b) => n + b.byteLength, 0);
  const total = 8 + 4 * (strings.length + 1) + ceil4(blobBytes);
  const out = new Uint8Array(total);
  const dv = new DataView(out.buffer);
  dv.setUint32(0, strings.length, true);
  dv.setUint32(4, blobBytes, true);
  let cursor = 0;
  for (let i = 0; i < strings.length; i++) {
    dv.setUint32(8 + 4 * i, cursor, true);
    cursor += blobs[i].byteLength;
  }
  dv.setUint32(8 + 4 * strings.length, blobBytes, true);
  let at = 8 + 4 * (strings.length + 1);
  for (const b of blobs) {
    out.set(b, at);
    at += b.byteLength;
  }
  return out;
}

// ---------------------------------------------------------------------------
// §3.1 — Hello
// ---------------------------------------------------------------------------

/** One row of the §3.1.3 node table. */
export interface NodeRowInit {
  nodeId: number;
  actorId: number;
  posXM: number;
  posYM: number;
  posZM: number;
  strLabel: number;
  strProfileId: number;
  flags: number;
  kind: number;
  classIdx: number;
}
/** One row of the §3.1.4 actor-class table. */
export interface ClassRowInit {
  strName: number;
  lengthM: number;
  widthM: number;
  heightM: number;
  colorRgba: number;
  category: number;
}
/** One row of the §3.1.5 channel table. */
export interface ChannelRowInit {
  strId: number;
  channelId: number;
  visibility: number;
  enabled: number;
}
/** Everything the §3.1 `Hello` body needs. */
export interface HelloInit {
  versionMinor?: number;
  helloFlags: number;
  runId: Uint8Array;
  scenarioHash: Uint8Array;
  worldHash: Uint8Array;
  t0WallNs: bigint;
  simDurationNs: bigint;
  mobilityStepNs: bigint;
  keyframePeriodNs: bigint;
  telemetryPeriodNs: bigint;
  metricPeriodNs: bigint;
  resumeSeq: bigint;
  simTimeNs: bigint;
  originLatDeg: number;
  originLonDeg: number;
  originAltM: number;
  bboxMinXM: number;
  bboxMinYM: number;
  bboxMaxXM: number;
  bboxMaxYM: number;
  actorCapacity: number;
  nodes: readonly NodeRowInit[];
  classes: readonly ClassRowInit[];
  channels: readonly ChannelRowInit[];
  worldRef: { mode: number; format: number; payloadBytes: number; strUrl: number };
  strings: readonly string[];
  strEngineVersion: number;
  strScenarioName: number;
  strRunLabel: number;
  strSessionToken: number;
}

/** Encode a §3.1 `Hello` body. */
export function encodeHelloBody(init: HelloInit): Uint8Array {
  const N = init.nodes.length;
  const C = init.classes.length;
  const K = init.channels.length;
  const offNodes = HELLO_PREFIX_BYTES;
  const offClasses = offNodes + 32 * N;
  const offChannels = offClasses + 24 * C;
  const offWorldRef = offChannels + 8 * K;
  const offStrings = offWorldRef + 16;
  const strTable = encodeStrTable(init.strings);
  const body = new Uint8Array(offStrings + strTable.byteLength);
  const dv = new DataView(body.buffer);
  const O = HELLO_OFFSETS;

  dv.setUint16(O.versionMajor, 1, true);
  dv.setUint16(O.versionMinor, init.versionMinor ?? 0, true);
  dv.setUint32(O.helloFlags, init.helloFlags, true);
  body.set(init.runId.subarray(0, 16), O.runId);
  body.set(init.scenarioHash.subarray(0, 32), O.scenarioHash);
  body.set(init.worldHash.subarray(0, 32), O.worldHash);
  dv.setBigInt64(O.t0WallNs, init.t0WallNs, true);
  dv.setBigUint64(O.simDurationNs, init.simDurationNs, true);
  dv.setBigUint64(O.mobilityStepNs, init.mobilityStepNs, true);
  dv.setBigUint64(O.keyframePeriodNs, init.keyframePeriodNs, true);
  dv.setBigUint64(O.telemetryPeriodNs, init.telemetryPeriodNs, true);
  dv.setBigUint64(O.metricPeriodNs, init.metricPeriodNs, true);
  dv.setBigUint64(O.resumeSeq, init.resumeSeq, true);
  dv.setBigUint64(O.simTimeNs, init.simTimeNs, true);
  dv.setFloat64(O.originLatDeg, init.originLatDeg, true);
  dv.setFloat64(O.originLonDeg, init.originLonDeg, true);
  dv.setFloat64(O.originAltM, init.originAltM, true);
  dv.setFloat64(O.bboxMinXM, init.bboxMinXM, true);
  dv.setFloat64(O.bboxMinYM, init.bboxMinYM, true);
  dv.setFloat64(O.bboxMaxXM, init.bboxMaxXM, true);
  dv.setFloat64(O.bboxMaxYM, init.bboxMaxYM, true);
  dv.setUint32(O.actorCapacity, init.actorCapacity, true);
  dv.setUint32(O.nodeCount, N, true);
  dv.setUint16(O.classCount, C, true);
  dv.setUint16(O.channelCount, K, true);
  dv.setUint32(O.offNodes, N > 0 ? offNodes : 0, true);
  dv.setUint32(O.offClasses, C > 0 ? offClasses : 0, true);
  dv.setUint32(O.offChannels, K > 0 ? offChannels : 0, true);
  dv.setUint32(O.offWorldRef, offWorldRef, true);
  dv.setUint32(O.offStrings, offStrings, true);
  dv.setUint32(O.strEngineVersion, init.strEngineVersion, true);
  dv.setUint32(O.strScenarioName, init.strScenarioName, true);
  dv.setUint32(O.strRunLabel, init.strRunLabel, true);
  dv.setUint32(O.strSessionToken, init.strSessionToken, true);

  for (let i = 0; i < N; i++) {
    const n = init.nodes[i];
    dv.setUint32(offNodes + 0 * N + 4 * i, n.nodeId, true);
    dv.setUint32(offNodes + 4 * N + 4 * i, n.actorId, true);
    dv.setFloat32(offNodes + 8 * N + 4 * i, n.posXM, true);
    dv.setFloat32(offNodes + 12 * N + 4 * i, n.posYM, true);
    dv.setFloat32(offNodes + 16 * N + 4 * i, n.posZM, true);
    dv.setUint32(offNodes + 20 * N + 4 * i, n.strLabel, true);
    dv.setUint32(offNodes + 24 * N + 4 * i, n.strProfileId, true);
    dv.setUint16(offNodes + 28 * N + 2 * i, n.flags, true);
    dv.setUint8(offNodes + 30 * N + i, n.kind);
    dv.setUint8(offNodes + 31 * N + i, n.classIdx);
  }
  for (let i = 0; i < C; i++) {
    const c = init.classes[i];
    dv.setUint32(offClasses + 0 * C + 4 * i, c.strName, true);
    dv.setFloat32(offClasses + 4 * C + 4 * i, c.lengthM, true);
    dv.setFloat32(offClasses + 8 * C + 4 * i, c.widthM, true);
    dv.setFloat32(offClasses + 12 * C + 4 * i, c.heightM, true);
    dv.setUint32(offClasses + 16 * C + 4 * i, c.colorRgba, true);
    dv.setUint16(offClasses + 20 * C + 2 * i, 0, true);
    dv.setUint8(offClasses + 22 * C + i, c.category);
    dv.setUint8(offClasses + 23 * C + i, 0);
  }
  for (let i = 0; i < K; i++) {
    const k = init.channels[i];
    dv.setUint32(offChannels + 0 * K + 4 * i, k.strId, true);
    dv.setUint16(offChannels + 4 * K + 2 * i, k.channelId, true);
    dv.setUint8(offChannels + 6 * K + i, k.visibility);
    dv.setUint8(offChannels + 7 * K + i, k.enabled);
  }
  dv.setUint8(offWorldRef + WORLD_REF_OFFSETS.mode, init.worldRef.mode);
  dv.setUint8(offWorldRef + WORLD_REF_OFFSETS.format, init.worldRef.format);
  dv.setUint32(offWorldRef + WORLD_REF_OFFSETS.payloadBytes, init.worldRef.payloadBytes, true);
  dv.setUint32(offWorldRef + WORLD_REF_OFFSETS.strUrl, init.worldRef.strUrl, true);
  body.set(strTable, offStrings);
  return body;
}

// ---------------------------------------------------------------------------
// §3.3 — Keyframe
// ---------------------------------------------------------------------------

/** One actor row of §3.3.2, already quantised. */
export interface ActorRowInit {
  actorId: number;
  xMm: number;
  yMm: number;
  laneId: number;
  zCm: number;
  headingBrad: number;
  speedCq: number;
  accelCq: number;
  classIdx: number;
  state: number;
  verifiedNeighbors: number;
  flags8?: number;
}
/** One signal row of §3.3.3. */
export interface SignalRowInit {
  signalId: number;
  timeToChangeDs: number;
  phase: number;
}
/** Everything the §3.3 `Keyframe` body needs. */
export interface KeyframeInit {
  simTimeNs: bigint;
  originXM: number;
  originYM: number;
  originZM: number;
  gopIndex: number;
  profile?: number;
  actors: readonly ActorRowInit[];
  signals: readonly SignalRowInit[];
}

/** Encode a §3.3 `Keyframe` body: `64 + 28·A + 8·S` bytes. */
export function encodeKeyframeBody(init: KeyframeInit): Uint8Array {
  const A = init.actors.length;
  const S = init.signals.length;
  const offActors = KEYFRAME_PREFIX_BYTES;
  const offSignals = offActors + 28 * A;
  const body = new Uint8Array(offSignals + 8 * S);
  const dv = new DataView(body.buffer);
  const O = KEYFRAME_OFFSETS;

  dv.setBigUint64(O.simTimeNs, init.simTimeNs, true);
  dv.setFloat64(O.originXM, init.originXM, true);
  dv.setFloat64(O.originYM, init.originYM, true);
  dv.setFloat64(O.originZM, init.originZM, true);
  dv.setUint32(O.actorCount, A, true);
  dv.setUint32(O.signalCount, S, true);
  dv.setUint32(O.offActors, A > 0 ? offActors : 0, true);
  dv.setUint32(O.offSignals, S > 0 ? offSignals : 0, true);
  dv.setUint32(O.gopIndex, init.gopIndex, true);
  dv.setUint16(O.profile, init.profile ?? 0, true);

  for (let i = 0; i < A; i++) {
    const a = init.actors[i];
    dv.setUint32(offActors + 0 * A + 4 * i, a.actorId, true);
    dv.setInt32(offActors + 4 * A + 4 * i, a.xMm, true);
    dv.setInt32(offActors + 8 * A + 4 * i, a.yMm, true);
    dv.setUint32(offActors + 12 * A + 4 * i, a.laneId, true);
    dv.setInt16(offActors + 16 * A + 2 * i, a.zCm, true);
    dv.setUint16(offActors + 18 * A + 2 * i, a.headingBrad, true);
    dv.setInt16(offActors + 20 * A + 2 * i, a.speedCq, true);
    dv.setInt16(offActors + 22 * A + 2 * i, a.accelCq, true);
    dv.setUint8(offActors + 24 * A + i, a.classIdx);
    dv.setUint8(offActors + 25 * A + i, a.state);
    dv.setUint8(offActors + 26 * A + i, a.verifiedNeighbors);
    dv.setUint8(offActors + 27 * A + i, a.flags8 ?? 0);
  }
  writeSignals(dv, offSignals, init.signals);
  return body;
}

function writeSignals(dv: DataView, off: number, signals: readonly SignalRowInit[]): void {
  const S = signals.length;
  for (let i = 0; i < S; i++) {
    dv.setUint32(off + 0 * S + 4 * i, signals[i].signalId, true);
    dv.setUint16(off + 4 * S + 2 * i, signals[i].timeToChangeDs, true);
    dv.setUint8(off + 6 * S + i, signals[i].phase);
    dv.setUint8(off + 7 * S + i, 0);
  }
}

// ---------------------------------------------------------------------------
// §3.4 — Delta
// ---------------------------------------------------------------------------

/** One moved row of §3.4.2. */
export interface MovedRowInit {
  slot: number;
  dxMm: number;
  dyMm: number;
  dzMm: number;
  headingBrad: number;
  speedCq: number;
  accelCq: number;
  state: number;
  verifiedNeighbors: number;
  mflags: number;
}
/** One entry of the §3.4.3 absolute block. */
export interface AbsoluteRowInit {
  xMm: number;
  yMm: number;
  zCm: number;
}
/** One spawn row of §3.4.5. */
export interface SpawnRowInit {
  slot: number;
  actorId: number;
  nodeId: number;
  xMm: number;
  yMm: number;
  laneId: number;
  zCm: number;
  headingBrad: number;
  speedCq: number;
  cause: number;
  classIdx: number;
  state: number;
  verifiedNeighbors: number;
}
/** One despawn row of §3.4.6. */
export interface DespawnRowInit {
  slot: number;
  cause: number;
}
/** Everything the §3.4 `Delta` body needs. */
export interface DeltaInit {
  simTimeNs: bigint;
  gopIndex: number;
  stepIndex: number;
  moved?: readonly MovedRowInit[];
  absolute?: readonly AbsoluteRowInit[];
  lanes?: readonly number[];
  spawns?: readonly SpawnRowInit[];
  despawns?: readonly DespawnRowInit[];
  signals?: readonly SignalRowInit[];
}

/** Encode a §3.4 `Delta` body: `64 + 20·M + 12·abs + 4·lanes + 36·P + 8·D + 8·S`. */
export function encodeDeltaBody(init: DeltaInit): Uint8Array {
  const moved = init.moved ?? [];
  const absolute = init.absolute ?? [];
  const lanes = init.lanes ?? [];
  const spawns = init.spawns ?? [];
  const despawns = init.despawns ?? [];
  const signals = init.signals ?? [];
  const M = moved.length;
  const Ab = absolute.length;
  const Ln = lanes.length;
  const P = spawns.length;
  const D = despawns.length;
  const S = signals.length;

  let at = DELTA_PREFIX_BYTES;
  const offMoved = M > 0 ? at : 0;
  at += 20 * M;
  const offAbs = Ab > 0 ? at : 0;
  at += 12 * Ab;
  const offLanes = Ln > 0 ? at : 0;
  at += 4 * Ln;
  const offSpawns = P > 0 ? at : 0;
  at += 36 * P;
  const offDespawns = D > 0 ? at : 0;
  at += 8 * D;
  const offSignals = S > 0 ? at : 0;
  at += 8 * S;

  const body = new Uint8Array(at);
  const dv = new DataView(body.buffer);
  const O = DELTA_OFFSETS;
  dv.setBigUint64(O.simTimeNs, init.simTimeNs, true);
  dv.setUint32(O.gopIndex, init.gopIndex, true);
  dv.setUint32(O.stepIndex, init.stepIndex, true);
  dv.setUint32(O.movedCount, M, true);
  dv.setUint32(O.absCount, Ab, true);
  dv.setUint32(O.laneCount, Ln, true);
  dv.setUint32(O.spawnCount, P, true);
  dv.setUint32(O.despawnCount, D, true);
  dv.setUint32(O.signalCount, S, true);
  dv.setUint32(O.offMoved, offMoved, true);
  dv.setUint32(O.offAbs, offAbs, true);
  dv.setUint32(O.offLanes, offLanes, true);
  dv.setUint32(O.offSpawns, offSpawns, true);
  dv.setUint32(O.offDespawns, offDespawns, true);
  dv.setUint32(O.offSignals, offSignals, true);

  for (let i = 0; i < M; i++) {
    const m = moved[i];
    dv.setUint32(offMoved + 0 * M + 4 * i, m.slot, true);
    dv.setInt16(offMoved + 4 * M + 2 * i, m.dxMm, true);
    dv.setInt16(offMoved + 6 * M + 2 * i, m.dyMm, true);
    dv.setInt16(offMoved + 8 * M + 2 * i, m.dzMm, true);
    dv.setUint16(offMoved + 10 * M + 2 * i, m.headingBrad, true);
    dv.setInt16(offMoved + 12 * M + 2 * i, m.speedCq, true);
    dv.setInt16(offMoved + 14 * M + 2 * i, m.accelCq, true);
    dv.setUint8(offMoved + 16 * M + i, m.state);
    dv.setUint8(offMoved + 17 * M + i, m.verifiedNeighbors);
    dv.setUint8(offMoved + 18 * M + i, m.mflags);
    dv.setUint8(offMoved + 19 * M + i, 0);
  }
  for (let i = 0; i < Ab; i++) {
    const base = offAbs + 12 * i;
    dv.setInt32(base + 0, absolute[i].xMm, true);
    dv.setInt32(base + 4, absolute[i].yMm, true);
    dv.setInt16(base + 8, absolute[i].zCm, true);
    dv.setUint16(base + 10, 0, true);
  }
  for (let i = 0; i < Ln; i++) dv.setUint32(offLanes + 4 * i, lanes[i], true);
  for (let i = 0; i < P; i++) {
    const s = spawns[i];
    dv.setUint32(offSpawns + 0 * P + 4 * i, s.slot, true);
    dv.setUint32(offSpawns + 4 * P + 4 * i, s.actorId, true);
    dv.setUint32(offSpawns + 8 * P + 4 * i, s.nodeId, true);
    dv.setInt32(offSpawns + 12 * P + 4 * i, s.xMm, true);
    dv.setInt32(offSpawns + 16 * P + 4 * i, s.yMm, true);
    dv.setUint32(offSpawns + 20 * P + 4 * i, s.laneId, true);
    dv.setInt16(offSpawns + 24 * P + 2 * i, s.zCm, true);
    dv.setUint16(offSpawns + 26 * P + 2 * i, s.headingBrad, true);
    dv.setInt16(offSpawns + 28 * P + 2 * i, s.speedCq, true);
    dv.setUint16(offSpawns + 30 * P + 2 * i, s.cause, true);
    dv.setUint8(offSpawns + 32 * P + i, s.classIdx);
    dv.setUint8(offSpawns + 33 * P + i, s.state);
    dv.setUint8(offSpawns + 34 * P + i, s.verifiedNeighbors);
    dv.setUint8(offSpawns + 35 * P + i, 0);
  }
  for (let i = 0; i < D; i++) {
    dv.setUint32(offDespawns + 0 * D + 4 * i, despawns[i].slot, true);
    dv.setUint16(offDespawns + 4 * D + 2 * i, despawns[i].cause, true);
    dv.setUint16(offDespawns + 6 * D + 2 * i, 0, true);
  }
  writeSignals(dv, offSignals, signals);
  return body;
}

// ---------------------------------------------------------------------------
// §3.5 — Telemetry
// ---------------------------------------------------------------------------

/** Every field of the §3.5.2 208-byte record, as plain numbers. */
export interface NodeTelemetryInit {
  storageUsedB: bigint; storageTotalB: bigint; nextTopupNs: bigint; crlBytes: bigint; outboxBytes: bigint;
  clockOffsetNs: bigint; nodeId: number; ramUsedKib: number; ramTotalKib: number;
  dropRxOverflow: number; dropVerifyPolicySkip: number; dropVerifyOverflow: number; dropTxOverflow: number;
  dropReassemblyTimeout: number; dropCrlBacklog: number; certStored: number; crlEntries: number;
  outboxMsgs: number; peerCacheEntries: number; p2pcdRequests: number; fullCertMsgs: number;
  msgsInPerS: number; msgsOutPerS: number; verificationsPerS: number; verifyWaitP50Ms: number;
  verifyWaitP95Ms: number; gnssHdop: number; gnssSigmaM: number; clockDriftPpm: number; posErrorM: number;
  airtimeMsPerS: number; cpuUtilPm: number; hsmUtilPm: number; qRxP50: number; qRxP95: number;
  qVerifyP50: number; qVerifyP95: number; qAppP50: number; qAppP95: number; qTxP50: number; qTxP95: number;
  qCrlP50: number; qCrlP95: number; dccState: number; cbrPm: number; txPowerCdbm: number;
  nbrTotal: number; nbrVerified: number; nbrUnverified: number; nbrRevoked: number; certActive: number;
  crlExpansionPm: number; unverifiedRatioPm: number; gnssFix: number; nodeState: number; verifyPolicy: number;
}

/** Encode a §3.5 `Telemetry` body: `32 + record_size·N`. */
export function encodeTelemetryBody(
  simTimeNs: bigint,
  windowNs: bigint,
  records: readonly NodeTelemetryInit[],
): Uint8Array {
  const N = records.length;
  const offRecords = TELEMETRY_PREFIX_BYTES;
  const body = new Uint8Array(offRecords + TELEMETRY_RECORD_BYTES_V1 * N);
  const dv = new DataView(body.buffer);
  const O = TELEMETRY_OFFSETS;
  dv.setBigUint64(O.simTimeNs, simTimeNs, true);
  dv.setBigUint64(O.windowNs, windowNs, true);
  dv.setUint32(O.nodeCount, N, true);
  dv.setUint32(O.offRecords, offRecords, true);
  dv.setUint32(O.recordSize, TELEMETRY_RECORD_BYTES_V1, true);
  const T = NODE_TELEMETRY_OFFSETS;
  for (let i = 0; i < N; i++) {
    const b = offRecords + TELEMETRY_RECORD_BYTES_V1 * i;
    const r = records[i];
    dv.setBigUint64(b + T.storageUsedB, r.storageUsedB, true);
    dv.setBigUint64(b + T.storageTotalB, r.storageTotalB, true);
    dv.setBigUint64(b + T.nextTopupNs, r.nextTopupNs, true);
    dv.setBigUint64(b + T.crlBytes, r.crlBytes, true);
    dv.setBigUint64(b + T.outboxBytes, r.outboxBytes, true);
    dv.setBigInt64(b + T.clockOffsetNs, r.clockOffsetNs, true);
    dv.setUint32(b + T.nodeId, r.nodeId, true);
    dv.setUint32(b + T.ramUsedKib, r.ramUsedKib, true);
    dv.setUint32(b + T.ramTotalKib, r.ramTotalKib, true);
    dv.setUint32(b + T.dropRxOverflow, r.dropRxOverflow, true);
    dv.setUint32(b + T.dropVerifyPolicySkip, r.dropVerifyPolicySkip, true);
    dv.setUint32(b + T.dropVerifyOverflow, r.dropVerifyOverflow, true);
    dv.setUint32(b + T.dropTxOverflow, r.dropTxOverflow, true);
    dv.setUint32(b + T.dropReassemblyTimeout, r.dropReassemblyTimeout, true);
    dv.setUint32(b + T.dropCrlBacklog, r.dropCrlBacklog, true);
    dv.setUint32(b + T.certStored, r.certStored, true);
    dv.setUint32(b + T.crlEntries, r.crlEntries, true);
    dv.setUint32(b + T.outboxMsgs, r.outboxMsgs, true);
    dv.setUint32(b + T.peerCacheEntries, r.peerCacheEntries, true);
    dv.setUint32(b + T.p2pcdRequests, r.p2pcdRequests, true);
    dv.setUint32(b + T.fullCertMsgs, r.fullCertMsgs, true);
    dv.setFloat32(b + T.msgsInPerS, r.msgsInPerS, true);
    dv.setFloat32(b + T.msgsOutPerS, r.msgsOutPerS, true);
    dv.setFloat32(b + T.verificationsPerS, r.verificationsPerS, true);
    dv.setFloat32(b + T.verifyWaitP50Ms, r.verifyWaitP50Ms, true);
    dv.setFloat32(b + T.verifyWaitP95Ms, r.verifyWaitP95Ms, true);
    dv.setFloat32(b + T.gnssHdop, r.gnssHdop, true);
    dv.setFloat32(b + T.gnssSigmaM, r.gnssSigmaM, true);
    dv.setFloat32(b + T.clockDriftPpm, r.clockDriftPpm, true);
    dv.setFloat32(b + T.posErrorM, r.posErrorM, true);
    dv.setFloat32(b + T.airtimeMsPerS, r.airtimeMsPerS, true);
    dv.setUint16(b + T.cpuUtilPm, r.cpuUtilPm, true);
    dv.setUint16(b + T.hsmUtilPm, r.hsmUtilPm, true);
    dv.setUint16(b + T.qRxP50, r.qRxP50, true);
    dv.setUint16(b + T.qRxP95, r.qRxP95, true);
    dv.setUint16(b + T.qVerifyP50, r.qVerifyP50, true);
    dv.setUint16(b + T.qVerifyP95, r.qVerifyP95, true);
    dv.setUint16(b + T.qAppP50, r.qAppP50, true);
    dv.setUint16(b + T.qAppP95, r.qAppP95, true);
    dv.setUint16(b + T.qTxP50, r.qTxP50, true);
    dv.setUint16(b + T.qTxP95, r.qTxP95, true);
    dv.setUint16(b + T.qCrlP50, r.qCrlP50, true);
    dv.setUint16(b + T.qCrlP95, r.qCrlP95, true);
    dv.setUint16(b + T.dccState, r.dccState, true);
    dv.setUint16(b + T.cbrPm, r.cbrPm, true);
    dv.setInt16(b + T.txPowerCdbm, r.txPowerCdbm, true);
    dv.setUint16(b + T.nbrTotal, r.nbrTotal, true);
    dv.setUint16(b + T.nbrVerified, r.nbrVerified, true);
    dv.setUint16(b + T.nbrUnverified, r.nbrUnverified, true);
    dv.setUint16(b + T.nbrRevoked, r.nbrRevoked, true);
    dv.setUint16(b + T.certActive, r.certActive, true);
    dv.setUint16(b + T.crlExpansionPm, r.crlExpansionPm, true);
    dv.setUint16(b + T.unverifiedRatioPm, r.unverifiedRatioPm, true);
    dv.setUint8(b + T.gnssFix, r.gnssFix);
    dv.setUint8(b + T.nodeState, r.nodeState);
    dv.setUint8(b + T.verifyPolicy, r.verifyPolicy);
  }
  return body;
}

// ---------------------------------------------------------------------------
// §3.6 — Event
// ---------------------------------------------------------------------------

/** One event to encode: its time, channel and already-serialised payload bytes. */
export interface EventRecordInit {
  simTimeNs: bigint;
  channelId: number;
  payload: Uint8Array;
}

/**
 * Encode a §3.6 `Event` body. The index is sorted by `(sim_time_ns, channel_id)` and every payload
 * is placed 8-aligned and zero-padded to a multiple of 8, as §3.6.1 requires.
 */
export function encodeEventBody(tStartNs: bigint, tEndNs: bigint, events: readonly EventRecordInit[]): Uint8Array {
  const sorted = [...events].sort((a, b) =>
    a.simTimeNs === b.simTimeNs ? a.channelId - b.channelId : a.simTimeNs < b.simTimeNs ? -1 : 1,
  );
  const E = sorted.length;
  const offIndex = EVENT_PREFIX_BYTES;
  const offPayloads = offIndex + 16 * E;
  const padded = sorted.map((e) => (e.payload.byteLength + 7) & ~7);
  const payloadBytes = padded.reduce((n, p) => n + p, 0);
  const body = new Uint8Array(offPayloads + payloadBytes);
  const dv = new DataView(body.buffer);
  const O = EVENT_OFFSETS;
  dv.setBigUint64(O.tStartNs, tStartNs, true);
  dv.setBigUint64(O.tEndNs, tEndNs, true);
  dv.setUint32(O.eventCount, E, true);
  dv.setUint32(O.offIndex, E > 0 ? offIndex : 0, true);
  dv.setUint32(O.offPayloads, E > 0 ? offPayloads : 0, true);
  dv.setUint32(O.payloadBytes, payloadBytes, true);
  let cursor = 0;
  for (let i = 0; i < E; i++) {
    dv.setBigUint64(offIndex + 0 * E + 8 * i, sorted[i].simTimeNs, true);
    dv.setUint32(offIndex + 8 * E + 4 * i, cursor, true);
    dv.setUint16(offIndex + 12 * E + 2 * i, padded[i], true);
    dv.setUint16(offIndex + 14 * E + 2 * i, sorted[i].channelId, true);
    body.set(sorted[i].payload, offPayloads + cursor);
    cursor += padded[i];
  }
  return body;
}

// ---------------------------------------------------------------------------
// §3.7 — MetricSample
// ---------------------------------------------------------------------------

/** One §3.7 sample record. */
export interface MetricRecordInit {
  value: number;
  strMetric: number;
  dimKey: number;
  nodeId: number;
  count: number;
  agg: number;
  visibility: number;
  provId: number;
}

/** Encode a §3.7 `MetricSample` body: `32 + 32·M`. */
export function encodeMetricSampleBody(
  simTimeNs: bigint,
  binWidthNs: bigint,
  samples: readonly MetricRecordInit[],
): Uint8Array {
  const M = samples.length;
  const offSamples = METRIC_PREFIX_BYTES;
  const body = new Uint8Array(offSamples + METRIC_RECORD_BYTES_V1 * M);
  const dv = new DataView(body.buffer);
  const O = METRIC_OFFSETS;
  dv.setBigUint64(O.simTimeNs, simTimeNs, true);
  dv.setBigUint64(O.binWidthNs, binWidthNs, true);
  dv.setUint32(O.sampleCount, M, true);
  dv.setUint32(O.offSamples, offSamples, true);
  dv.setUint32(O.recordSize, METRIC_RECORD_BYTES_V1, true);
  const R = METRIC_RECORD_OFFSETS;
  for (let i = 0; i < M; i++) {
    const b = offSamples + METRIC_RECORD_BYTES_V1 * i;
    const s = samples[i];
    dv.setFloat64(b + R.value, s.value, true);
    dv.setUint32(b + R.strMetric, s.strMetric, true);
    dv.setUint32(b + R.dimKey, s.dimKey, true);
    dv.setUint32(b + R.nodeId, s.nodeId, true);
    dv.setUint32(b + R.count, s.count, true);
    dv.setUint16(b + R.agg, s.agg, true);
    dv.setUint8(b + R.visibility, s.visibility);
    dv.setUint32(b + R.provId, s.provId, true);
  }
  return body;
}

// ---------------------------------------------------------------------------
// §3.8 — Provenance
// ---------------------------------------------------------------------------

/** One §3.8 provenance entry. */
export interface ProvenanceEntryInit {
  provId: number;
  strModelId: number;
  strModelVersion: number;
  strParamSetId: number;
  strCardUrl: number;
  family: number;
  subjectKind: number;
}

/** Encode a §3.8 `Provenance` body. */
export function encodeProvenanceBody(
  simTimeNs: bigint,
  entries: readonly ProvenanceEntryInit[],
  dims: readonly { dimKey: number; strDims: number }[] = [],
  stringExtension: readonly string[] = [],
  flags = 0,
): Uint8Array {
  const P = entries.length;
  const Dk = dims.length;
  const offEntries = PROVENANCE_PREFIX_BYTES;
  const offDims = offEntries + 24 * P;
  const offStrings = offDims + 8 * Dk;
  const strTable = stringExtension.length > 0 ? encodeStrTable(stringExtension) : new Uint8Array(0);
  const body = new Uint8Array(offStrings + strTable.byteLength);
  const dv = new DataView(body.buffer);
  const O = PROVENANCE_OFFSETS;
  dv.setBigUint64(O.simTimeNs, simTimeNs, true);
  dv.setUint32(O.entryCount, P, true);
  dv.setUint32(O.offEntries, P > 0 ? offEntries : 0, true);
  dv.setUint32(O.dimCount, Dk, true);
  dv.setUint32(O.offDims, Dk > 0 ? offDims : 0, true);
  dv.setUint32(O.offStrings, strTable.byteLength > 0 ? offStrings : 0, true);
  dv.setUint32(O.flags, flags, true);
  for (let i = 0; i < P; i++) {
    const e = entries[i];
    dv.setUint32(offEntries + 0 * P + 4 * i, e.provId, true);
    dv.setUint32(offEntries + 4 * P + 4 * i, e.strModelId, true);
    dv.setUint32(offEntries + 8 * P + 4 * i, e.strModelVersion, true);
    dv.setUint32(offEntries + 12 * P + 4 * i, e.strParamSetId, true);
    dv.setUint32(offEntries + 16 * P + 4 * i, e.strCardUrl, true);
    dv.setUint16(offEntries + 20 * P + 2 * i, e.family, true);
    dv.setUint16(offEntries + 22 * P + 2 * i, e.subjectKind, true);
  }
  for (let i = 0; i < Dk; i++) {
    dv.setUint32(offDims + 0 * Dk + 4 * i, dims[i].dimKey, true);
    dv.setUint32(offDims + 4 * Dk + 4 * i, dims[i].strDims, true);
  }
  if (strTable.byteLength > 0) body.set(strTable, offStrings);
  return body;
}

// ---------------------------------------------------------------------------
// §3.9 — WorldChunk, §3.10 — Error, §3.11 — Bye
// ---------------------------------------------------------------------------

/** Encode a §3.9 `WorldChunk` body. */
export function encodeWorldChunkBody(init: {
  worldHash: Uint8Array;
  totalBytes: bigint;
  chunkIndex: number;
  chunkCount: number;
  format: number;
  payload: Uint8Array;
}): Uint8Array {
  const offPayload = WORLD_CHUNK_PREFIX_BYTES;
  const body = new Uint8Array(offPayload + init.payload.byteLength);
  const dv = new DataView(body.buffer);
  const O = WORLD_CHUNK_OFFSETS;
  body.set(init.worldHash.subarray(0, 32), O.worldHash);
  dv.setBigUint64(O.totalBytes, init.totalBytes, true);
  dv.setUint32(O.chunkIndex, init.chunkIndex, true);
  dv.setUint32(O.chunkCount, init.chunkCount, true);
  dv.setUint32(O.offPayload, offPayload, true);
  dv.setUint32(O.payloadLen, init.payload.byteLength, true);
  dv.setUint8(O.format, init.format);
  body.set(init.payload, offPayload);
  return body;
}

/** Encode a §3.10 `Error` body; `message` and `detail` are appended as a string extension. */
export function encodeErrorBody(init: {
  simTimeNs: bigint;
  code: number;
  fatal: boolean;
  message: string;
  detail?: string;
  firstStringId: number;
}): Uint8Array {
  const strings = [init.message, init.detail ?? ""];
  const strTable = encodeStrTable(strings);
  const offStrings = 32;
  const body = new Uint8Array(offStrings + strTable.byteLength);
  const dv = new DataView(body.buffer);
  const O = ERROR_OFFSETS;
  dv.setBigUint64(O.simTimeNs, init.simTimeNs, true);
  dv.setInt32(O.code, init.code, true);
  dv.setUint32(O.offStrings, offStrings, true);
  dv.setUint32(O.strMessage, init.firstStringId + 0, true);
  dv.setUint32(O.strDetail, init.firstStringId + 1, true);
  dv.setUint8(O.fatal, init.fatal ? 1 : 0);
  body.set(strTable, offStrings);
  return body;
}

/** Encode a §3.11 `Bye` body. */
export function encodeByeBody(init: {
  simTimeNs: bigint;
  canonicalFrames: bigint;
  reason: number;
  detail?: string;
  firstStringId: number;
}): Uint8Array {
  const strTable = encodeStrTable([init.detail ?? ""]);
  const offStrings = 32;
  const body = new Uint8Array(offStrings + strTable.byteLength);
  const dv = new DataView(body.buffer);
  const O = BYE_OFFSETS;
  dv.setBigUint64(O.simTimeNs, init.simTimeNs, true);
  dv.setBigUint64(O.canonicalFrames, init.canonicalFrames, true);
  dv.setUint8(O.reason, init.reason);
  dv.setUint32(O.offStrings, offStrings, true);
  dv.setUint32(O.strDetail, init.firstStringId, true);
  body.set(strTable, offStrings);
  return body;
}

// ---------------------------------------------------------------------------
// Frame assembly
// ---------------------------------------------------------------------------

/** Wrap a body in a §2.1 frame header. */
export function frameOf(msgType: number, seq: bigint, body: Uint8Array, flags = 0): ArrayBuffer {
  const out = new ArrayBuffer(FRAME_HEADER_BYTES + body.byteLength);
  serialiseFrameHeader(out, 0, { msgType, bodyLen: body.byteLength, seq, flags });
  new Uint8Array(out, FRAME_HEADER_BYTES).set(body);
  return out;
}

/**
 * `Hello` frame. §1.3 rule 1: it must be the first frame and MUST NOT be compressed, so
 * `FLAG_COMPRESSED` is masked out of `flags` here rather than trusted from the caller — this
 * package is the writer for the mock engine server, and §10.1 F7 is a server obligation.
 */
export const helloFrame = (init: HelloInit, seq: bigint, flags = 0): ArrayBuffer =>
  frameOf(MsgType.Hello, seq, encodeHelloBody(init), flags & ~FrameFlags.COMPRESSED);
/** `Keyframe` frame. */
export const keyframeFrame = (init: KeyframeInit, seq: bigint, flags = 0): ArrayBuffer =>
  frameOf(MsgType.Keyframe, seq, encodeKeyframeBody(init), flags);
/** `Delta` frame. */
export const deltaFrame = (init: DeltaInit, seq: bigint, flags = 0): ArrayBuffer =>
  frameOf(MsgType.Delta, seq, encodeDeltaBody(init), flags);

// ---------------------------------------------------------------------------
// §4 — the world payload
// ---------------------------------------------------------------------------

/** A lane for {@link encodeWorld}; `points` is `[x, y, z]` per centreline point, in travel order. */
export interface WorldLaneInit {
  laneId: number;
  edgeId: number;
  junctionId: number;
  strName: number;
  widthM: number;
  speedLimitMps: number;
  allowedClasses: number;
  laneType: number;
  indexInEdge: number;
  points: readonly (readonly [number, number, number])[];
}
/** A building for {@link encodeWorld}; `ring` is the CCW, unclosed outer ring. */
export interface WorldBuildingInit {
  buildingId: number;
  heightM: number;
  baseZM: number;
  strName: number;
  material: number;
  lodHint: number;
  levels: number;
  ring: readonly (readonly [number, number])[];
}
/** Everything {@link encodeWorld} needs (§4.2–§4.5). */
export interface WorldInit {
  originLatDeg: number;
  originLonDeg: number;
  originAltM: number;
  bboxMinXM: number;
  bboxMinYM: number;
  bboxMaxXM: number;
  bboxMaxYM: number;
  bboxMinZM: number;
  bboxMaxZM: number;
  lanes: readonly WorldLaneInit[];
  buildings: readonly WorldBuildingInit[];
  junctions: readonly { junctionId: number; strName: number; xM: number; yM: number; zM: number; control: number; laneCount: number }[];
  signals: readonly { signalId: number; junctionId: number; laneId: number; xM: number; yM: number; zM: number; kind: number; group: number }[];
  sites: readonly { siteId: number; nodeId: number; xM: number; yM: number; zM: number; antennaHeightM: number; antennaGainDbi: number; kind: number }[];
  crossings: readonly { crossingId: number; junctionId: number; x1M: number; y1M: number; x2M: number; y2M: number; widthM: number }[];
  landuse: readonly { landuseId: number; classIdx: number; ring: readonly (readonly [number, number])[] }[];
  strings: readonly string[];
  provenanceJson: string;
}

/**
 * Encode a `vwp-world/1` file (§4). `sha256` computes the content hash of the body, which §4.2
 * requires to equal the URL hash and `Hello.world_hash`; it is injected so this package keeps no
 * runtime dependency (Node: `createHash("sha256")`, browser: `crypto.subtle`).
 */
export function encodeWorld(init: WorldInit, sha256: (bytes: Uint8Array) => Uint8Array): Uint8Array {
  const L = init.lanes.length;
  const B = init.buildings.length;
  const lanePointTotal = init.lanes.reduce((n, l) => n + l.points.length, 0);
  const buildingRingTotal = init.buildings.reduce((n, b) => n + b.ring.length, 0);
  const landuseRingTotal = init.landuse.reduce((n, l) => n + l.ring.length, 0);
  const ringPointTotal = buildingRingTotal + landuseRingTotal;
  const J = init.junctions.length;
  const S = init.signals.length;
  const Si = init.sites.length;
  const Cr = init.crossings.length;
  const Lu = init.landuse.length;
  const strTable = encodeStrTable(init.strings);
  const provBytes = UTF8_ENC.encode(init.provenanceJson);

  let at = VWB_DIRECTORY_BYTES;
  const offLanes = L > 0 ? at : 0;
  at += 36 * L;
  const offLanePoints = lanePointTotal > 0 ? at : 0;
  at += 12 * lanePointTotal;
  const offBuildings = B > 0 ? at : 0;
  at += 28 * B;
  const offRingPoints = ringPointTotal > 0 ? at : 0;
  at += 8 * ringPointTotal;
  const offJunctions = J > 0 ? at : 0;
  at += VWB_JUNCTION_STRIDE * J;
  const offSignals = S > 0 ? at : 0;
  at += VWB_SIGNAL_STRIDE * S;
  const offSites = Si > 0 ? at : 0;
  at += VWB_SITE_STRIDE * Si;
  const offCrossings = Cr > 0 ? at : 0;
  at += VWB_CROSSING_STRIDE * Cr;
  const offLanduse = Lu > 0 ? at : 0;
  at += VWB_LANDUSE_STRIDE * Lu;
  const offStrings = at;
  at += strTable.byteLength;
  const offProv = provBytes.byteLength > 0 ? at : 0;
  at += ceil4(provBytes.byteLength);
  const bodyLen = at;

  const file = new Uint8Array(VWB_HEADER_BYTES + bodyLen);
  const fdv = new DataView(file.buffer);
  fdv.setUint32(VWB_HEADER_OFFSETS.magic, VWB_MAGIC, true);
  fdv.setUint16(VWB_HEADER_OFFSETS.version, VWB_VERSION, true);
  fdv.setUint32(VWB_HEADER_OFFSETS.bodyLen, bodyLen, true);
  fdv.setUint16(VWB_HEADER_OFFSETS.flags, 0, true);

  const body = file.subarray(VWB_HEADER_BYTES);
  const dv = new DataView(file.buffer, VWB_HEADER_BYTES, bodyLen);
  const D = VWB_DIRECTORY_OFFSETS;
  dv.setFloat64(D.originLatDeg, init.originLatDeg, true);
  dv.setFloat64(D.originLonDeg, init.originLonDeg, true);
  dv.setFloat64(D.originAltM, init.originAltM, true);
  dv.setFloat64(D.bboxMinXM, init.bboxMinXM, true);
  dv.setFloat64(D.bboxMinYM, init.bboxMinYM, true);
  dv.setFloat64(D.bboxMaxXM, init.bboxMaxXM, true);
  dv.setFloat64(D.bboxMaxYM, init.bboxMaxYM, true);
  dv.setFloat32(D.bboxMinZM, init.bboxMinZM, true);
  dv.setFloat32(D.bboxMaxZM, init.bboxMaxZM, true);
  dv.setUint32(D.laneCount, L, true);
  dv.setUint32(D.lanePointTotal, lanePointTotal, true);
  dv.setUint32(D.buildingCount, B, true);
  dv.setUint32(D.ringPointTotal, ringPointTotal, true);
  dv.setUint32(D.offLanes, offLanes, true);
  dv.setUint32(D.offLanePoints, offLanePoints, true);
  dv.setUint32(D.offBuildings, offBuildings, true);
  dv.setUint32(D.offRingPoints, offRingPoints, true);
  dv.setUint32(D.junctionCount, J, true);
  dv.setUint32(D.offJunctions, offJunctions, true);
  dv.setUint32(D.signalCount, S, true);
  dv.setUint32(D.offSignals, offSignals, true);
  dv.setUint32(D.siteCount, Si, true);
  dv.setUint32(D.offSites, offSites, true);
  dv.setUint32(D.offStrings, offStrings, true);
  dv.setUint32(D.crossingCount, Cr, true);
  dv.setUint32(D.offCrossings, offCrossings, true);
  dv.setUint32(D.landuseCount, Lu, true);
  dv.setUint32(D.offLanduse, offLanduse, true);
  dv.setUint32(D.offProvenanceJson, offProv, true);
  dv.setUint32(D.provenanceJsonBytes, provBytes.byteLength, true);

  let pointCursor = 0;
  for (let i = 0; i < L; i++) {
    const l = init.lanes[i];
    dv.setUint32(offLanes + 0 * L + 4 * i, l.laneId, true);
    dv.setUint32(offLanes + 4 * L + 4 * i, pointCursor, true);
    dv.setUint32(offLanes + 8 * L + 4 * i, l.points.length, true);
    dv.setUint32(offLanes + 12 * L + 4 * i, l.edgeId, true);
    dv.setUint32(offLanes + 16 * L + 4 * i, l.junctionId, true);
    dv.setUint32(offLanes + 20 * L + 4 * i, l.strName, true);
    dv.setFloat32(offLanes + 24 * L + 4 * i, l.widthM, true);
    dv.setFloat32(offLanes + 28 * L + 4 * i, l.speedLimitMps, true);
    dv.setUint16(offLanes + 32 * L + 2 * i, l.allowedClasses, true);
    dv.setUint8(offLanes + 34 * L + i, l.laneType);
    dv.setUint8(offLanes + 35 * L + i, l.indexInEdge);
    for (const p of l.points) {
      dv.setFloat32(offLanePoints + 0 * lanePointTotal + 4 * pointCursor, p[0], true);
      dv.setFloat32(offLanePoints + 4 * lanePointTotal + 4 * pointCursor, p[1], true);
      dv.setFloat32(offLanePoints + 8 * lanePointTotal + 4 * pointCursor, p[2], true);
      pointCursor += 1;
    }
  }

  let ringCursor = 0;
  const writeRing = (ring: readonly (readonly [number, number])[]): number => {
    const start = ringCursor;
    for (const p of ring) {
      dv.setFloat32(offRingPoints + 0 * ringPointTotal + 4 * ringCursor, p[0], true);
      dv.setFloat32(offRingPoints + 4 * ringPointTotal + 4 * ringCursor, p[1], true);
      ringCursor += 1;
    }
    return start;
  };
  for (let i = 0; i < B; i++) {
    const b = init.buildings[i];
    const start = writeRing(b.ring);
    dv.setUint32(offBuildings + 0 * B + 4 * i, b.buildingId, true);
    dv.setUint32(offBuildings + 4 * B + 4 * i, start, true);
    dv.setUint32(offBuildings + 8 * B + 4 * i, b.ring.length, true);
    dv.setFloat32(offBuildings + 12 * B + 4 * i, b.heightM, true);
    dv.setFloat32(offBuildings + 16 * B + 4 * i, b.baseZM, true);
    dv.setUint32(offBuildings + 20 * B + 4 * i, b.strName, true);
    dv.setUint8(offBuildings + 24 * B + i, b.material);
    dv.setUint8(offBuildings + 25 * B + i, b.lodHint);
    dv.setUint16(offBuildings + 26 * B + 2 * i, b.levels, true);
  }

  for (let i = 0; i < J; i++) {
    const j = init.junctions[i];
    const b = offJunctions + VWB_JUNCTION_STRIDE * i;
    dv.setUint32(b + 0, j.junctionId, true);
    dv.setUint32(b + 4, j.strName, true);
    dv.setFloat32(b + 8, j.xM, true);
    dv.setFloat32(b + 12, j.yM, true);
    dv.setFloat32(b + 16, j.zM, true);
    dv.setUint8(b + 20, j.control);
    dv.setUint16(b + 22, j.laneCount, true);
  }
  for (let i = 0; i < S; i++) {
    const s = init.signals[i];
    const b = offSignals + VWB_SIGNAL_STRIDE * i;
    dv.setUint32(b + 0, s.signalId, true);
    dv.setUint32(b + 4, s.junctionId, true);
    dv.setUint32(b + 8, s.laneId, true);
    dv.setFloat32(b + 12, s.xM, true);
    dv.setFloat32(b + 16, s.yM, true);
    dv.setFloat32(b + 20, s.zM, true);
    dv.setUint8(b + 24, s.kind);
    dv.setUint16(b + 26, s.group, true);
  }
  for (let i = 0; i < Si; i++) {
    const s = init.sites[i];
    const b = offSites + VWB_SITE_STRIDE * i;
    dv.setUint32(b + 0, s.siteId, true);
    dv.setUint32(b + 4, s.nodeId, true);
    dv.setFloat32(b + 8, s.xM, true);
    dv.setFloat32(b + 12, s.yM, true);
    dv.setFloat32(b + 16, s.zM, true);
    dv.setFloat32(b + 20, s.antennaHeightM, true);
    dv.setFloat32(b + 24, s.antennaGainDbi, true);
    dv.setUint8(b + 28, s.kind);
  }
  for (let i = 0; i < Cr; i++) {
    const c = init.crossings[i];
    const b = offCrossings + VWB_CROSSING_STRIDE * i;
    dv.setUint32(b + 0, c.crossingId, true);
    dv.setUint32(b + 4, c.junctionId, true);
    dv.setFloat32(b + 8, c.x1M, true);
    dv.setFloat32(b + 12, c.y1M, true);
    dv.setFloat32(b + 16, c.x2M, true);
    dv.setFloat32(b + 20, c.y2M, true);
    dv.setFloat32(b + 24, c.widthM, true);
  }
  for (let i = 0; i < Lu; i++) {
    const l = init.landuse[i];
    const start = writeRing(l.ring);
    const b = offLanduse + VWB_LANDUSE_STRIDE * i;
    dv.setUint32(b + 0, l.landuseId, true);
    dv.setUint32(b + 4, start, true);
    dv.setUint32(b + 8, l.ring.length, true);
    dv.setUint8(b + 12, l.classIdx);
  }

  body.set(strTable, offStrings);
  if (offProv !== 0) body.set(provBytes, offProv);

  // §4.2 — content_hash is the SHA-256 of the body. The field lives inside the body, so it is
  // hashed while still zero; see computeWorldContentHash for the same rule on the reading side.
  const digest = sha256(body);
  body.set(digest.subarray(0, 32), D.contentHash);
  return file;
}
