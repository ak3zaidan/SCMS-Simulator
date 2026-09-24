/**
 * Regression tests for the Studio's store projection (ui-review-register Q4, Q9, Q10, Q14).
 *
 * The engine header promises "a small, throttled projection of that state into the Zustand store so
 * the panels re-render at a human rate (09-ui §4)". These tests measure that promise the way the
 * reviewer did: they count store notifications and per-selector identity changes, which is exactly
 * what decides whether a `useStudio(selector)` component re-renders, and they count the
 * collaborator calls a disabled overlay should never have provoked.
 */

import { beforeEach, describe, expect, it } from "vitest";

import { createHash } from "node:crypto";

import { ChannelId, decodeHello, encodeWorld, helloFrame, viewFrame } from "@vwp/protocol";
import { DEFAULT_OVERLAYS, StudioEngine } from "../src/state/engine.js";
import { useStudio } from "../src/state/store.js";
import {
  addNode,
  attachFakeClient,
  detObservation,
  fakeEvent,
  fakeMetric,
  fakeViewer,
  metricRecord,
  nodeTx,
  phyRx,
  type FakeEventRow,
} from "./support/fakes.js";

const PRISTINE = useStudio.getState();

/** Count store notifications, and how often each named selector's value changes identity. */
function watch(keys: readonly string[]): { notifications: number; changes: Record<string, number>; stop: () => void } {
  const out = { notifications: 0, changes: {} as Record<string, number>, stop: () => undefined as void };
  const last = new Map<string, unknown>();
  for (const k of keys) {
    out.changes[k] = 0;
    last.set(k, (useStudio.getState() as unknown as Record<string, unknown>)[k]);
  }
  const unsub = useStudio.subscribe((state) => {
    out.notifications++;
    const s = state as unknown as Record<string, unknown>;
    for (const k of keys) {
      if (!Object.is(s[k], last.get(k))) {
        out.changes[k]++;
        last.set(k, s[k]);
      }
    }
  });
  out.stop = unsub;
  return out;
}

function freshEngine(): StudioEngine {
  useStudio.setState({
    telemetry: null,
    telemetryNode: null,
    simTimeNs: 0,
    stats: null,
    frames: { keyframe: 0, delta: 0, telemetry: 0, event: 0, metric: 0 },
    timeline: [],
    metricProvenance: {},
    metricDims: {},
    pseudonym: null,
    seriesTick: 0,
    // Projected from the engine's node table and the followed pose at each flush; a new engine
    // starts from nothing, so the store must too, or its first flush reports the change.
    radios: 0,
    followedPose: null,
    run: PRISTINE.run,
  });
  return new StudioEngine();
}

describe("Q4 — metric samples do not write the React store", () => {
  let engine: StudioEngine;
  beforeEach(() => {
    engine = freshEngine();
  });

  it("1,000 samples with a varying dim_key cause no store notification until the 5 Hz flush", () => {
    attachFakeClient(engine, ["", "pdr"]);
    // §3.8 — the dimension dictionary the Provenance frame carries.
    for (let k = 1; k <= 1000; k++) engine.dims.set(k, `link=${k}`);

    const w = watch(["metricDims", "metricProvenance"]);
    for (let k = 1; k <= 1000; k++) {
      engine.handleMetric(fakeMetric(k * 1_000_000, [metricRecord({ strMetric: 1, value: k, dimKey: k, provId: 7 })]));
    }
    const duringStream = w.notifications;
    engine.flushProjection();
    w.stop();

    expect(duringStream).toBe(0);
    // One flush publishes both maps; nothing else in the flush changed, so the identity of each
    // slice moves exactly once.
    expect(w.changes.metricDims).toBe(1);
    expect(w.changes.metricProvenance).toBe(1);
    // The reviewer measured "metricDims ended up holding exactly 1 key" — still true, and the
    // published value is the last dims string seen.
    expect(Object.keys(useStudio.getState().metricDims)).toEqual(["pdr"]);
    expect(useStudio.getState().metricDims.pdr).toBe("link=1000");
    expect(useStudio.getState().metricProvenance.pdr).toBe(7);
  });

  it("a second flush with no new samples does not change the published maps", () => {
    attachFakeClient(engine, ["", "pdr"]);
    engine.handleMetric(fakeMetric(1, [metricRecord({ strMetric: 1, value: 1, provId: 3 })]));
    engine.flushProjection();
    const first = useStudio.getState().metricProvenance;

    engine.handleMetric(fakeMetric(2, [metricRecord({ strMetric: 1, value: 2, provId: 3 })]));
    engine.flushProjection();
    expect(useStudio.getState().metricProvenance).toBe(first);
  });

  it("20,000 distinct metric names cannot grow the published map without bound", () => {
    const names: string[] = [""];
    for (let k = 1; k <= 20_000; k++) names.push(`metric.${k}`);
    attachFakeClient(engine, names);
    for (let k = 1; k <= 20_000; k++) {
      engine.handleMetric(fakeMetric(k, [metricRecord({ strMetric: k, value: k, provId: k })]));
    }
    engine.flushProjection();
    const published = Object.keys(useStudio.getState().metricProvenance).length;
    expect(published).toBeLessThanOrEqual(2000);
    // The newest names survive the eviction, which is what the "why" tab needs.
    expect(useStudio.getState().metricProvenance["metric.20000"]).toBe(20_000);
  });
});

describe("Q4 — event batches do not write the React store", () => {
  let engine: StudioEngine;
  beforeEach(() => {
    engine = freshEngine();
  });

  it("100 Event frames of detections notify once, at the flush", () => {
    const { viewer } = fakeViewer();
    engine.viewer = viewer;
    attachFakeClient(engine);
    addNode(engine, 5, null);

    const w = watch(["timeline"]);
    for (let f = 0; f < 100; f++) {
      engine.handleEvent(
        fakeEvent([{ channelId: ChannelId.DET_OBSERVATION, tNs: f * 100_000_000, payload: detObservation(5, 0.5, 11) }]),
      );
    }
    const duringStream = w.notifications;
    engine.flushProjection();
    w.stop();

    expect(duringStream).toBe(0);
    expect(w.changes.timeline).toBe(1);
    expect(useStudio.getState().timeline).toHaveLength(100);
  });

  it("marks accumulated between flushes keep their order and the 600-mark cap", () => {
    const { viewer } = fakeViewer();
    engine.viewer = viewer;
    attachFakeClient(engine);
    addNode(engine, 5, null);
    for (let f = 0; f < 800; f++) {
      engine.handleEvent(
        fakeEvent([{ channelId: ChannelId.DET_OBSERVATION, tNs: f, payload: detObservation(5, 0.25, 11) }]),
      );
    }
    engine.flushProjection();
    const timeline = useStudio.getState().timeline;
    expect(timeline).toHaveLength(600);
    expect(timeline[0].tNs).toBe(200);
    expect(timeline[599].tNs).toBe(799);
  });
});

describe("Q9 — a flush with nothing new does not change what App subscribes to", () => {
  it("300 flushes (60 s at 5 Hz) move the stats identity once, not 300 times", () => {
    const engine = freshEngine();
    const { viewer } = fakeViewer();
    engine.viewer = viewer;
    attachFakeClient(engine);

    const w = watch(["stats", "frames", "telemetry", "simTimeNs", "seriesTick"]);
    for (let i = 0; i < 300; i++) {
      // An empty Event frame is the cheapest way to set the engine's dirty flag, and it is what a
      // live run does 10x a second: the frame counters advance, nothing else does.
      engine.handleEvent(fakeEvent([]));
      engine.flushProjection();
    }
    w.stop();

    expect(w.changes.stats).toBe(1);
    expect(w.changes.telemetry).toBe(0);
    expect(w.changes.simTimeNs).toBe(0);
    // The frame counters genuinely advance every flush, and the sparkline tick is the 5 Hz beat
    // 09-ui §4 asks for; both are expected to move 300 times.
    expect(w.changes.frames).toBe(300);
    expect(w.changes.seriesTick).toBe(300);
    // Four unconditional `set` calls per flush meant 1,200 notifications. Now: 300 for the frame
    // counters, 300 for the sparkline tick, 1 for the first stats publish.
    expect(w.notifications).toBe(601);
  });

  it("run.status polling with an unchanged status does not change the run identity", () => {
    const engine = freshEngine();
    void engine;
    const store = useStudio.getState();
    store.setRun({ state: "running", tNs: 100, speed: 1 });
    const first = useStudio.getState().run;
    store.setRun({ state: "running", tNs: 100, speed: 1 });
    expect(useStudio.getState().run).toBe(first);
    store.setRun({ tNs: 200 });
    expect(useStudio.getState().run).not.toBe(first);
  });
});

describe("Q10 — the link and pulse overlays are checked before the work is done", () => {
  let engine: StudioEngine;
  let rows: FakeEventRow[];

  beforeEach(() => {
    engine = freshEngine();
    for (let n = 1; n <= 50; n++) addNode(engine, n, n + 1000);
    rows = [];
    for (let i = 0; i < 500; i++) {
      rows.push({ channelId: ChannelId.PHY_RX, tNs: i, payload: phyRx(1 + (i % 50), 1 + ((i + 7) % 50)) });
    }
  });

  it("with the links overlay off, no position is read and no payload is decoded", () => {
    const { viewer, probe } = fakeViewer(["lane_markings"]);
    engine.viewer = viewer;
    const client = attachFakeClient(engine);

    const msg = fakeEvent(rows);
    engine.handleEvent(msg);

    // The reviewer measured 2 `#nodePosition` calls per PHY_RX row — 1,000 here — all discarded.
    expect(client.positionReads).toBe(0);
    expect(client.slotLookups).toBe(0);
    expect(probe.linkAdds).toBe(0);
    expect(probe.linkBegins).toBe(0);
    expect(msg.payloadCalls).toBe(0);
  });

  it("with the links overlay on, every delivered link is drawn", () => {
    const { viewer, probe } = fakeViewer(["links"]);
    engine.viewer = viewer;
    const client = attachFakeClient(engine);

    engine.handleEvent(fakeEvent(rows));

    expect(probe.linkBegins).toBe(1);
    expect(probe.linkAdds).toBe(500);
    expect(client.positionReads).toBe(1000);
  });

  it("with the pulse overlay off and nothing followed, no node.tx payload is decoded", () => {
    const { viewer, probe } = fakeViewer([]);
    engine.viewer = viewer;
    const client = attachFakeClient(engine);

    const txRows: FakeEventRow[] = [];
    for (let i = 0; i < 400; i++) txRows.push({ channelId: ChannelId.NODE_TX, tNs: i, payload: nodeTx(1 + (i % 50)) });
    const msg = fakeEvent(txRows);
    engine.handleEvent(msg);

    expect(probe.pulseEmits).toBe(0);
    expect(client.positionReads).toBe(0);
    expect(msg.payloadCalls).toBe(0);
  });

  it("with the pulse overlay on, the per-frame pulse budget is still respected", () => {
    const { viewer, probe } = fakeViewer(["tx_pulses"]);
    engine.viewer = viewer;
    attachFakeClient(engine);

    const txRows: FakeEventRow[] = [];
    for (let i = 0; i < 400; i++) txRows.push({ channelId: ChannelId.NODE_TX, tNs: i, payload: nodeTx(1 + (i % 50)) });
    engine.handleEvent(fakeEvent(txRows));

    expect(probe.pulseEmits).toBeGreaterThan(0);
    expect(probe.pulseEmits).toBeLessThanOrEqual(40);
  });
});

describe("Q14 — shape redundancy is on by default (09-ui §10)", () => {
  it("the default overlay set turns on the three non-ground-truth marker channels", () => {
    expect(DEFAULT_OVERLAYS).toContain("reported");
    expect(DEFAULT_OVERLAYS).toContain("revoked");
    expect(DEFAULT_OVERLAYS).toContain("detections");
    // `attackers_gt` stays out: it is ground truth and `lockGroundTruth` must be able to refuse it.
    expect(DEFAULT_OVERLAYS).not.toContain("attackers_gt");
  });
});

describe("S3 / §10.5 W3 — the Studio verifies the world it is served", () => {
  const sha256 = (bytes: Uint8Array): Uint8Array => new Uint8Array(createHash("sha256").update(bytes).digest());
  /** The smallest well-formed `vwp-world/1` payload: §4.2's directory and nothing else. */
  const worldOf = (bboxMaxXM: number): Uint8Array =>
    encodeWorld(
      {
        originLatDeg: 40.75, originLonDeg: -73.98, originAltM: 10,
        bboxMinXM: -500, bboxMinYM: -500, bboxMaxXM, bboxMaxYM: 500, bboxMinZM: 0, bboxMaxZM: 80,
        lanes: [], buildings: [], junctions: [], signals: [], sites: [], crossings: [], landuse: [],
        strings: [""], provenanceJson: "",
      },
      sha256,
    );
  const asBuffer = (b: Uint8Array): ArrayBuffer => b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength) as ArrayBuffer;
  const hashOf = (b: Uint8Array): Uint8Array => b.subarray(16, 48);

  /** Deliver a `Hello` promising `worldHash`, built and decoded through the real wire path. */
  function helloPromising(engine: StudioEngine, worldHash: Uint8Array): void {
    engine.handleHello(
      decodeHello(
        viewFrame(
          helloFrame(
            {
              helloFlags: 0x01, runId: new Uint8Array(16), scenarioHash: new Uint8Array(32),
              worldHash, t0WallNs: 0n, simDurationNs: 0n, mobilityStepNs: 100_000_000n,
              keyframePeriodNs: 1_000_000_000n, telemetryPeriodNs: 1_000_000_000n,
              metricPeriodNs: 1_000_000_000n, resumeSeq: 0n, simTimeNs: 0n,
              originLatDeg: 0, originLonDeg: 0, originAltM: 0,
              bboxMinXM: -500, bboxMinYM: -500, bboxMaxXM: 500, bboxMaxYM: 500,
              actorCapacity: 16, nodes: [], classes: [], channels: [],
              // mode 2: "the client already has it", so nothing is fetched behind the test's back.
              worldRef: { mode: 2, format: 0, payloadBytes: 0, strUrl: 0 },
              strings: [""], strEngineVersion: 0, strScenarioName: 0, strRunLabel: 0, strSessionToken: 0,
            },
            0n,
          ),
        ),
      ),
    );
  }

  it("adopts the world whose payload digest is the one Hello promised", async () => {
    const engine = freshEngine();
    const file = worldOf(500);
    helloPromising(engine, hashOf(file));
    await expect(engine.loadWorldPayload(asBuffer(file), "/world/x.vwb")).resolves.toBe(true);
    expect(engine.world).not.toBeNull();
    expect(engine.world?.bbox.maxXM).toBe(500);
    expect(useStudio.getState().logs[0]).toMatchObject({ level: "info", target: "world" });
  });

  it("refuses a different world, keeps the one it had, and reports a typed hash_mismatch", async () => {
    const engine = freshEngine();
    const served = worldOf(500);
    const promised = worldOf(501); // same shape, different bytes, different payload digest.
    expect(Array.from(hashOf(promised))).not.toEqual(Array.from(hashOf(served)));

    helloPromising(engine, hashOf(served));
    await engine.loadWorldPayload(asBuffer(served), "/world/served.vwb");
    const adopted = engine.world;
    expect(adopted).not.toBeNull();

    // Now a second Hello promises a world the server does not serve: the payload must be refused.
    helloPromising(engine, hashOf(promised));
    await expect(engine.loadWorldPayload(asBuffer(served), "/world/wrong.vwb")).resolves.toBe(false);
    expect(engine.world).toBe(adopted); // W3: refused, not silently swapped in.
    expect(useStudio.getState().logs[0]).toMatchObject({ level: "error", target: "world" });
    expect(useStudio.getState().logs[0].message).toContain("hash_mismatch");
  });

  it("refuses a payload that arrives before any Hello has promised a digest", async () => {
    const engine = freshEngine();
    await expect(engine.loadWorldPayload(asBuffer(worldOf(500)), "/world/early.vwb")).resolves.toBe(false);
    expect(engine.world).toBeNull();
  });
});
