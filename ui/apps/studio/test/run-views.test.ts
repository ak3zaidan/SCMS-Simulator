/**
 * Every per-run view resets on a new run, and none of them resets on a resume.
 *
 * The wave A integrator saw the plots strip keep the previous run's history after "Run again" on
 * a short run: `e2e_latency.p95` drawn on a 50–125 s axis for a 20 s run. The metric store was
 * emptied only when the page disconnected, and a new run on the same socket is not a disconnect —
 * it is a non-resumed `Hello` (§6.6). A resumed `Hello` (§1.4 case 1) is the opposite case: the
 * frames after it are the ones this page missed, so throwing the history away there would be the
 * defect in the other direction.
 */

import { describe, expect, it } from "vitest";

import { decodeHello, helloFrame, viewFrame } from "@vwp/protocol";
import { StudioEngine } from "../src/state/engine.js";
import { useStudio } from "../src/state/store.js";
import { attachFakeClient, fakeMetric, metricRecord } from "./support/fakes.js";

function hello(flags: number): ReturnType<typeof decodeHello> {
  return decodeHello(
    viewFrame(
      helloFrame(
        {
          helloFlags: flags, runId: new Uint8Array(16), scenarioHash: new Uint8Array(32),
          worldHash: new Uint8Array(32), t0WallNs: 0n, simDurationNs: 20_000_000_000n,
          mobilityStepNs: 100_000_000n, keyframePeriodNs: 1_000_000_000n,
          telemetryPeriodNs: 1_000_000_000n, metricPeriodNs: 1_000_000_000n, resumeSeq: 0n, simTimeNs: 0n,
          originLatDeg: 0, originLonDeg: 0, originAltM: 0,
          bboxMinXM: -500, bboxMinYM: -500, bboxMaxXM: 500, bboxMaxYM: 500,
          actorCapacity: 16, nodes: [], classes: [], channels: [],
          worldRef: { mode: 2, format: 0, payloadBytes: 0, strUrl: 0 },
          strings: [""], strEngineVersion: 0, strScenarioName: 0, strRunLabel: 0, strSessionToken: 0,
        },
        0n,
      ),
    ),
  );
}

const LIVE = 0x01;
const RESUMED = 0x20;

function runWithHistory(): StudioEngine {
  const engine = new StudioEngine();
  attachFakeClient(engine, ["", "e2e_latency.p95"]);
  engine.handleHello(hello(LIVE));
  for (let t = 1; t <= 125; t++) {
    engine.handleMetric(fakeMetric(t * 1_000_000_000, [metricRecord({ strMetric: 1, value: t, provId: 4 })]));
  }
  useStudio.getState().addTimelineMarks([{ tNs: 5e9, channel: "app.warning", nodeId: 1, label: "warning" }]);
  return engine;
}

describe("per-run views", () => {
  it("a new run on the socket empties the plots, the provenance and the timeline", () => {
    const engine = runWithHistory();
    expect(engine.metrics.get("e2e_latency.p95")[0].length).toBe(125);
    engine.handleHello(hello(LIVE));
    expect(engine.metrics.names()).toEqual([]);
    expect(engine.metrics.get("e2e_latency.p95")[0]).toEqual([]);
    expect(useStudio.getState().timeline).toEqual([]);
    // The next run's first sample starts the axis again.
    engine.handleMetric(fakeMetric(1_000_000_000, [metricRecord({ strMetric: 1, value: 0.2 })]));
    expect(engine.metrics.get("e2e_latency.p95")[0]).toEqual([1]);
  });

  it("a resumed Hello keeps all of it", () => {
    const engine = runWithHistory();
    engine.handleHello(hello(LIVE | RESUMED));
    expect(engine.metrics.get("e2e_latency.p95")[0].length).toBe(125);
    expect(useStudio.getState().timeline.length).toBe(1);
  });
});
