/**
 * "Sometimes I see the light turn green and sometimes not."
 *
 * Three causes, one test each, and a fourth for the picture itself:
 *
 * 1. the world has one record per *head* and several heads share a controller's `signal_id`; the
 *    old renderer mapped id → head and let each head overwrite the last, so one head per junction
 *    changed colour and the others stayed dark;
 * 2. `Viewer.attachClient` applied keyframe signal blocks and dropped the delta ones, so a lamp
 *    changed up to a keyframe period after the engine said it did;
 * 3. nothing reset a head the stream stopped mentioning, so after a seek a lamp could keep the
 *    colour of a different stretch of the run.
 */

import { describe, expect, it } from "vitest";
import { Color } from "three";
import type { DeltaMessage, HelloMessage, KeyframeMessage, PoseBuffer, SignalBlock, VwpClientApi } from "@vwp/protocol";
import { WorldRenderer } from "../src/world-render.js";
import { PHASE_NO_DATA, aspectOf } from "../src/signals.js";
import { DARK_THEME } from "../src/theme.js";
import { Viewer } from "../src/scene.js";
import { NullRenderer } from "./support/null-renderer.js";
import type { ViewerCanvas } from "../src/types.js";
import { SyntheticStream, makeGridWorld } from "./support/fixture.js";

const grid = makeGridWorld({ blocks: 4, blockM: 120, controllerSignals: true });

function block(rows: readonly [number, number][]): SignalBlock {
  return {
    count: rows.length,
    signalId: Uint32Array.from(rows.map((r) => r[0])),
    timeToChangeDs: Uint16Array.from(rows.map(() => 50)),
    phase: Uint8Array.from(rows.map((r) => r[1])),
    reserved: new Uint8Array(rows.length),
  };
}

function headsOf(w: WorldRenderer, signalId: number): number[] {
  const out: number[] = [];
  for (let i = 0; i < w.signals.count; i++) if (w.signals.headState(i)?.signalId === signalId) out.push(i);
  return out;
}

describe("signal heads", () => {
  it("lights every head of a controller, not just the last one", () => {
    const w = new WorldRenderer({ theme: DARK_THEME });
    w.setWorld(grid.world);
    const heads = headsOf(w, 0);
    expect(heads.length).toBe(4);
    w.applySignalKeyframe(block([[0, 6], [1, 3]]));
    for (const h of heads) expect(w.signals.headState(h)?.aspect.name, `head ${h}`).toBe("green");
    for (const h of headsOf(w, 1)) expect(w.signals.headState(h)?.aspect.name, `head ${h}`).toBe("red");
    w.dispose();
  });

  it("treats a keyframe as the whole state and a delta as a change on top of it", () => {
    const w = new WorldRenderer({ theme: DARK_THEME });
    w.setWorld(grid.world);
    w.applySignalKeyframe(block([[0, 6], [1, 3], [2, 8]]));
    w.applySignalDelta(block([[0, 8]]));
    expect(w.signals.headState(headsOf(w, 0)[0])?.aspect.name).toBe("amber");
    expect(w.signals.headState(headsOf(w, 1)[0])?.aspect.name).toBe("red");
    // A keyframe after a seek that does not mention controllers 0 and 1: they have no data now,
    // not whatever they showed at another time.
    w.applySignalKeyframe(block([[2, 3]]));
    expect(w.signals.headState(headsOf(w, 0)[0])?.phase).toBe(PHASE_NO_DATA);
    expect(w.signals.headState(headsOf(w, 1)[0])?.phase).toBe(PHASE_NO_DATA);
    expect(w.signals.headState(headsOf(w, 2)[0])?.aspect.name).toBe("red");
    w.dispose();
  });

  it("draws one lit aspect in its fixed position and the other two dark", () => {
    const w = new WorldRenderer({ theme: DARK_THEME });
    w.setWorld(grid.world);
    const h = headsOf(w, 0)[0];
    const lit = (c: [number, number, number] | null): number => (c ? c[0] + c[1] + c[2] : 0);
    const cases: [number, number][] = [[3, 0], [8, 1], [6, 2]];
    for (const [phase, litIndex] of cases) {
      w.applySignalKeyframe(block([[0, phase]]));
      const values = [0, 1, 2].map((k) => lit(w.signals.lampColor(h, k)));
      for (let k = 0; k < 3; k++) {
        if (k === litIndex) expect(values[k], `phase ${phase} lamp ${k}`).toBeGreaterThan(5 * Math.max(...values.filter((_, j) => j !== k)));
      }
    }
    // The lit green is the theme's green exactly (unlit material, no tone mapping).
    w.applySignalKeyframe(block([[0, 6]]));
    const g = new Color(DARK_THEME.signalGreen);
    const c = w.signals.lampColor(h, 2)!;
    expect(c[0]).toBeCloseTo(g.r, 5);
    expect(c[1]).toBeCloseTo(g.g, 5);
    expect(c[2]).toBeCloseTo(g.b, 5);
    // No data: nothing lit at all.
    w.applySignalKeyframe(null);
    const dark = [0, 1, 2].map((k) => lit(w.signals.lampColor(h, k)));
    const redLit = new Color(DARK_THEME.signalRed);
    expect(dark[0]).toBeLessThan((redLit.r + redLit.g + redLit.b) * 0.2);
    w.dispose();
  });

  it("flashes the flashing states at 1 Hz", () => {
    const w = new WorldRenderer({ theme: DARK_THEME });
    w.setWorld(grid.world);
    const h = headsOf(w, 0)[0];
    w.applySignalKeyframe(block([[0, 9]]));
    const amber = (t: number): number => {
      w.signals.update(t);
      const c = w.signals.lampColor(h, 1)!;
      return c[0] + c[1] + c[2];
    };
    expect(amber(0.1)).toBeGreaterThan(amber(0.6) * 5);
    expect(amber(1.1)).toBeGreaterThan(amber(1.6) * 5);
    expect(aspectOf(2).flashing).toBe(true);
    expect(aspectOf(3).flashing).toBe(false);
    w.dispose();
  });
});

/** A client the viewer can attach to, fed by hand. */
class FakeClient implements Pick<VwpClientApi, "poses" | "onHello" | "onKeyframe" | "onDelta"> {
  readonly poses: PoseBuffer;
  #hello: ((h: HelloMessage) => void)[] = [];
  #kf: ((k: KeyframeMessage) => void)[] = [];
  #delta: ((d: DeltaMessage) => void)[] = [];
  constructor(poses: PoseBuffer) {
    this.poses = poses;
  }
  onHello(l: (h: HelloMessage) => void): () => void {
    this.#hello.push(l);
    return () => undefined;
  }
  onKeyframe(l: (k: KeyframeMessage) => void): () => void {
    this.#kf.push(l);
    return () => undefined;
  }
  onDelta(l: (d: DeltaMessage) => void): () => void {
    this.#delta.push(l);
    return () => undefined;
  }
  keyframe(simSeconds: number, signals: SignalBlock): void {
    for (const l of this.#kf) l({ simTimeNs: BigInt(Math.round(simSeconds * 1e9)), signals } as unknown as KeyframeMessage);
  }
  delta(simSeconds: number, signals: SignalBlock): void {
    for (const l of this.#delta) l({ simTimeNs: BigInt(Math.round(simSeconds * 1e9)), signals } as unknown as DeltaMessage);
  }
}

describe("Viewer signal timing", () => {
  function setup(): { viewer: Viewer; client: FakeClient; stream: SyntheticStream; heads: number[] } {
    const canvas = { width: 800, height: 600, clientWidth: 800, clientHeight: 600 } as unknown as ViewerCanvas;
    const viewer = new Viewer({ canvas, autoStart: false, createRenderer: (c) => new NullRenderer(c) });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(8, grid);
    const client = new FakeClient(stream.poses);
    viewer.attachClient(client as unknown as VwpClientApi);
    return { viewer, client, stream, heads: headsOf(viewer.worldRenderer, 0) };
  }

  it("applies delta signal rows, at the instant the vehicles are drawn at", () => {
    const { viewer, client, stream, heads } = setup();
    stream.keyframe();
    client.keyframe(0, block([[0, 3]]));
    viewer.step(1 / 60);
    expect(viewer.worldRenderer.signals.headState(heads[0])?.aspect.name).toBe("red");
    // Ten steps of stream; the light turns green at t = 0.5 s in a delta.
    let changedAtRender = Number.NaN;
    for (let k = 1; k <= 20; k++) {
      stream.advance(0.1);
      stream.delta();
      client.delta(k * 0.1, k === 5 ? block([[0, 6]]) : block([]));
      for (let f = 0; f < 6; f++) {
        viewer.step(1 / 60);
        const name = viewer.worldRenderer.signals.headState(heads[0])?.aspect.name;
        if (name === "green" && !Number.isFinite(changedAtRender)) changedAtRender = viewer.interpolator.renderSimSeconds;
      }
    }
    // It changed (the old attachClient dropped delta signal rows, so it never did)…
    expect(viewer.worldRenderer.signals.headState(heads[0])?.aspect.name).toBe("green");
    // …and it changed when the drawn vehicles reached t = 0.5 s, not when the frame arrived.
    expect(changedAtRender).toBeGreaterThanOrEqual(0.5 - 1e-6);
    expect(changedAtRender).toBeLessThan(0.5 + 0.05);
    viewer.dispose();
  });

  it("takes a seek's keyframe as the whole state at once", () => {
    const { viewer, client, stream, heads } = setup();
    stream.keyframe();
    client.keyframe(30, block([[0, 6], [1, 6]]));
    viewer.step(1 / 60);
    // Seek back to t = 10 s: the keyframe there says controller 0 is red and does not mention 1.
    client.keyframe(10, block([[0, 3]]));
    viewer.step(1 / 60);
    expect(viewer.worldRenderer.signals.headState(heads[0])?.aspect.name).toBe("red");
    const other = headsOf(viewer.worldRenderer, 1)[0];
    expect(viewer.worldRenderer.signals.headState(other)?.phase).toBe(PHASE_NO_DATA);
    viewer.dispose();
  });
});
