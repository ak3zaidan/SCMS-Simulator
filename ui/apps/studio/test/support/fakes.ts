/**
 * Doubles for the Studio's projection tests.
 *
 * The stream handlers under test (`StudioEngine.handleEvent`, `handleMetric`, `flushProjection`)
 * only ever touch four things: the decoded message, `engine.nodes`, the `VwpClient`'s slot/pose
 * lookup and the `Viewer`'s overlay manager. All four are faked here so a test can deliver an
 * `Event` or `MetricSample` batch without a socket, a canvas or WebGL, and can count exactly how
 * many times each collaborator was reached.
 */

import type { EventMessage, EventPayload, MetricRecord, MetricSampleMessage } from "@vwp/protocol";
import type { Viewer } from "@vwp/viewer";

import type { StudioEngine } from "../../src/state/engine.js";

/** One row of a synthetic `Event` batch (§3.6.1 index entry + its payload). */
export interface FakeEventRow {
  readonly channelId: number;
  readonly tNs: number;
  readonly payload: EventPayload;
}

/** A decoded `Event` frame (§3.6) good enough for `handleEvent`, counting `payload()` calls. */
export function fakeEvent(rows: readonly FakeEventRow[]): EventMessage & { payloadCalls: number } {
  const count = rows.length;
  const channelId = new Uint16Array(count);
  const simTimeNs = new BigUint64Array(count);
  for (let i = 0; i < count; i++) {
    channelId[i] = rows[i].channelId;
    simTimeNs[i] = BigInt(rows[i].tNs);
  }
  const msg = {
    kind: "event" as const,
    header: {} as EventMessage["header"],
    tStartNs: count > 0 ? simTimeNs[0] : 0n,
    tEndNs: count > 0 ? simTimeNs[count - 1] : 0n,
    count,
    index: { simTimeNs, payloadOff: new Uint32Array(count), payloadLen: new Uint16Array(count), channelId },
    payloads: new Uint8Array(0),
    payloadCalls: 0,
    payloadView(): DataView {
      throw new Error("not used by the projection");
    },
    payload(i: number): EventPayload {
      msg.payloadCalls++;
      return rows[i].payload;
    },
  };
  return msg;
}

/** A `phy.rx` payload (§3.6.5) with `outcome = 0` (delivered), which is what feeds the link overlay. */
export function phyRx(txNode: number, rxNode: number): EventPayload {
  return {
    channel: "phy.rx",
    tStartNs: 0n,
    tEndNs: 0n,
    rxNode,
    txNode,
    msgId: 1,
    rssiDbm: -70,
    sinrDb: 12,
    distanceM: 80,
    outcome: 0,
    cause: 0,
    losClass: 0,
  };
}

/** A `node.tx` payload (§3.6.4), which is what feeds the transmission-pulse overlay. */
export function nodeTx(nodeId: number): EventPayload {
  return {
    channel: "node.tx",
    nodeId,
    msgId: 2,
    bytesOnAir: 300,
    airtimeMs: 0.4,
    msgType: 2,
    txPowerCdbm: 2000,
    channelNumber: 180,
    mcs: 2,
    accessCategory: 1,
    dccState: 0,
    signerIdType: 0,
    payloadBytes: 200,
    pseudonymDigest: new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]),
    certId: 9,
  };
}

/** A `det.observation` payload (§3.6.9) — a timeline mark with a `prov_id`. */
export function detObservation(nodeId: number, score: number, provId: number): EventPayload {
  return {
    channel: "det.observation",
    nodeId,
    strDetector: 0,
    subjectDigest: new Uint8Array(8),
    score,
    subjectActorId: 0xffffffff,
    evidenceCount: 1,
    detectorKind: 1,
    provId,
  };
}

/** A decoded `MetricSample` frame (§3.7) over plain records. */
export function fakeMetric(simTimeNs: number, samples: readonly MetricRecord[]): MetricSampleMessage {
  return {
    kind: "metric",
    header: {} as MetricSampleMessage["header"],
    simTimeNs: BigInt(simTimeNs),
    binWidthNs: 1_000_000_000n,
    sampleCount: samples.length,
    recordSize: 32,
    raw: new Uint8Array(0),
    sample: (i: number) => samples[i],
    samples: () => [...samples],
  };
}

/** One `MetricSample` record (§3.7). */
export function metricRecord(init: Partial<MetricRecord> & { strMetric: number; value: number }): MetricRecord {
  return {
    dimKey: 0,
    nodeId: 0xffffffff,
    count: 1,
    agg: 0,
    visibility: 0,
    provId: 0,
    ...init,
  };
}

/** Everything the fake viewer counted. */
export interface OverlayProbe {
  pulseEmits: number;
  linkAdds: number;
  linkBegins: number;
  enabled: Set<string>;
}

/** A `Viewer` stand-in: the overlay manager the event path drives, plus a frozen stats snapshot. */
export function fakeViewer(enabled: readonly string[] = []): { viewer: Viewer; probe: OverlayProbe } {
  const probe: OverlayProbe = { pulseEmits: 0, linkAdds: 0, linkBegins: 0, enabled: new Set(enabled) };
  const snapshot = {
    fps: 60, fpsAverage: 60, frameMs: 16.6, p95Ms: 18, cpuMs: 4, drawCalls: 12, triangles: 1000,
    actorInstances: 200, actorCulled: 3, actorLive: 203, buildingsVisible: 40,
  };
  const viewer = {
    renderClockSeconds: 1,
    stats: { snapshot: () => snapshot },
    overlays: {
      isEnabled: (name: string) => probe.enabled.has(name),
      pulses: { emit: () => { probe.pulseEmits++; } },
      links: {
        begin: () => { probe.linkBegins++; },
        add: () => { probe.linkAdds++; },
        end: () => undefined,
      },
    },
  };
  return { viewer: viewer as unknown as Viewer, probe };
}

/** What the fake client counted. */
export interface ClientProbe {
  slotLookups: number;
  positionReads: number;
}

/**
 * A `VwpClient` stand-in for `#nodePosition`: `slots.slotOf` and `poses.positionOf` are the two
 * calls the link and pulse paths make per event, so counting them measures work that an overlay
 * check should have skipped.
 */
export function attachFakeClient(engine: StudioEngine, strings: readonly string[] = []): ClientProbe {
  const probe: ClientProbe = { slotLookups: 0, positionReads: 0 };
  const pos = { x: 10, y: 20, z: 1 };
  const client = {
    strings: { get: (id: number) => strings[id] ?? "" },
    slots: {
      slotOf: (actorId: number) => {
        probe.slotLookups++;
        return actorId;
      },
    },
    poses: {
      positionOf: (slot: number) => {
        probe.positionReads++;
        pos.x = slot;
        return pos;
      },
    },
  };
  (engine as unknown as { client: unknown }).client = client;
  return probe;
}

/** Register a mobile node (one with an actor) in `engine.nodes`, as `Hello` would (§3.1.3). */
export function addNode(engine: StudioEngine, nodeId: number, actorId: number | null): void {
  engine.nodes.set(nodeId, {
    nodeId,
    actorId,
    label: `n${nodeId}`,
    profileId: "obu",
    kind: 1,
    flags: 0,
    classIdx: 0,
    x: nodeId,
    y: nodeId * 2,
    z: 0.5,
  });
  if (actorId !== null) engine.nodeByActor.set(actorId, nodeId);
}
