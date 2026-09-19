/**
 * The headless smoke and budget test.
 *
 * Node has no WebGL, so the GPU half is stubbed ({@link NullRenderer}) and everything else is the
 * real thing: real `vwp-world/1` bytes decoded by `@vwp/protocol`, real `Keyframe` and `Delta` frames
 * applied to a real `PoseBuffer`, the real interpolator, the real culling and instance-matrix writes,
 * the real camera smoothing and the real picker. What this cannot measure is rasterisation; what it
 * can — and does — measure is the CPU side of the frame, which is the part 09-ui §4 budgets at
 * "1–2 ms in JS" for 5,000 vehicles.
 */

import { describe, expect, it } from "vitest";
import { ActorState } from "@vwp/protocol";
import { Viewer } from "../src/scene.js";
import type { ViewerCanvas } from "../src/types.js";
import { NullRenderer } from "./support/null-renderer.js";
import { SyntheticStream, makeGridWorld } from "./support/fixture.js";

const CANVAS = { width: 1600, height: 900, clientWidth: 1600, clientHeight: 900 } as unknown as ViewerCanvas;

function makeViewer(overrides: { maxActors?: number; shadows?: boolean } = {}): {
  viewer: Viewer;
  renderer: NullRenderer;
} {
  let renderer!: NullRenderer;
  const viewer = new Viewer({
    canvas: CANVAS,
    theme: "dark",
    autoStart: false,
    shadows: overrides.shadows ?? true,
    maxActors: overrides.maxActors ?? 20_000,
    createRenderer: (canvas) => {
      renderer = new NullRenderer(canvas);
      return renderer;
    },
  });
  return { viewer, renderer };
}

describe("Viewer — headless smoke", () => {
  it("constructs one scene, one camera and one renderer", () => {
    const { viewer, renderer } = makeViewer();
    expect(viewer.renderer).toBe(renderer);
    expect(viewer.camera.up.toArray()).toEqual([0, 0, 1]);
    expect(viewer.scene.children).toContain(viewer.worldRenderer.group);
    expect(viewer.scene.children).toContain(viewer.actors.group);
    expect(viewer.scene.children).toContain(viewer.overlays.group);
    expect(renderer.size).toEqual({ width: 1600, height: 900 });
    expect(renderer.pixelRatio).toBeLessThanOrEqual(1.5);
    viewer.dispose();
  });

  it("builds a synthetic world, loads 5,000 actors and steps 100 frames", () => {
    const { viewer, renderer } = makeViewer();
    const grid = makeGridWorld({ blocks: 16, blockM: 120, buildingsPerBlock: 3 });
    viewer.setWorld(grid.world);

    const report = viewer.worldRenderer.report;
    expect(report.lanes).toBeGreaterThan(0);
    expect(report.buildings).toBe(grid.world.buildings.count);
    expect(["batched", "merged"]).toContain(report.buildingBackend);

    const stream = new SyntheticStream(5000, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    expect(stream.poses.count).toBe(5000);

    const FRAMES = 100;
    const FRAME_MS = 1000 / 60;
    const DELTA_EVERY = 6; // 10 Hz deltas against a 60 fps loop
    let t = 0;

    const run = (frames: number): { drawn: number; culled: number } => {
      let drawn = 0;
      let culled = 0;
      for (let i = 0; i < frames; i++) {
        t += FRAME_MS;
        if (i % DELTA_EVERY === 0) {
          stream.advance(DELTA_EVERY / 60);
          stream.delta();
          viewer.capture(stream.poses, t / 1000);
        }
        const r = viewer.renderFrame(t);
        drawn += r.actorsDrawn;
        culled += r.actorsCulled;
      }
      return { drawn: drawn / frames, culled: culled / frames };
    };

    // --- Case A: a plausible map view, most of the crowd off screen. ---
    viewer.cameras.focusOn(0, 0, 0);
    viewer.cameras.altitudeM = 450;
    viewer.cameras.snap();
    run(30); // warm-up: every instance bucket reaches its steady-state capacity here
    viewer.stats.reset();
    const capacityBefore = viewer.actors.allocatedCapacity;
    const heapBefore = process.memoryUsage().heapUsed;
    const wallA0 = performance.now();
    const caseA = run(FRAMES);
    const wallA = performance.now() - wallA0;
    const snapA = viewer.stats.snapshot();
    const heapAfter = process.memoryUsage().heapUsed;
    const capacityAfter = viewer.actors.allocatedCapacity;

    // --- Case B: the budget case — the whole 5,000-vehicle crowd inside the frustum. ---
    viewer.cameras.altitudeM = 2400;
    viewer.cameras.snap();
    run(12); // the LOD-2 buckets grow to hold the whole crowd here, once
    viewer.stats.reset();
    const capacityBeforeB = viewer.actors.allocatedCapacity;
    const wallB0 = performance.now();
    const caseB = run(FRAMES);
    const wallB = performance.now() - wallB0;
    const snapB = viewer.stats.snapshot();
    const capacityAfterB = viewer.actors.allocatedCapacity;

    // eslint-disable-next-line no-console
    console.log(
      [
        "",
        "--- world ---------------------------------------------------------------",
        `  ${report.lanes} lanes, ${report.buildings} buildings (${report.buildingBackend}), ` +
        `${report.junctions} junctions, ${report.signals} signals, ${report.sites} sites`,
        `  built in ${report.buildMs.toFixed(1)} ms; ${report.drawables} static drawables, ` +
        `${report.surfaceVertices.toLocaleString()} surface + ${report.buildingVertices.toLocaleString()} building vertices`,
        "--- case A: map view at 450 m, 5,000 actors live ------------------------",
        `  ${caseA.drawn.toFixed(0)} drawn / ${caseA.culled.toFixed(0)} culled per frame`,
        `  wall ${(wallA / FRAMES).toFixed(3)} ms/frame, viewer CPU mean ${snapA.cpuMeanMs.toFixed(3)} ms, ` +
        `p95 ${snapA.p95Ms.toFixed(3)} ms`,
        `  ${snapA.drawCalls} draw calls`,
        "--- case B: whole crowd in frustum (the 5,000-vehicle budget) -----------",
        `  ${caseB.drawn.toFixed(0)} drawn / ${caseB.culled.toFixed(0)} culled per frame`,
        `  wall ${(wallB / FRAMES).toFixed(3)} ms/frame, viewer CPU mean ${snapB.cpuMeanMs.toFixed(3)} ms, ` +
        `p95 ${snapB.p95Ms.toFixed(3)} ms`,
        `  ${snapB.drawCalls} draw calls, ${snapB.triangles.toLocaleString()} triangles submitted`,
        "--- allocation ----------------------------------------------------------",
        `  instance capacity, case A ${capacityBefore} → ${capacityAfter}; ` +
        `case B ${capacityBeforeB} → ${capacityAfterB} (no growth after warm-up)`,
        `  heap delta over ${FRAMES} frames: ${((heapAfter - heapBefore) / 1024 / 1024).toFixed(2)} MB`,
        "",
      ].join("\n"),
    );

    expect(renderer.renders).toBe(30 + FRAMES + 12 + FRAMES);
    expect(caseA.drawn).toBeGreaterThan(0);
    expect(caseB.drawn).toBeGreaterThan(4900);
    expect(viewer.lastFrame.stalled).toBe(false);
    // After warm-up no bucket should have to grow again.
    expect(capacityAfter).toBe(capacityBefore);
    expect(capacityAfterB).toBe(capacityBeforeB);
    expect(capacityAfterB).toBeGreaterThan(5000);
    // No unbounded allocation: 100 frames of a 5,000-actor scene must not churn the heap.
    expect((heapAfter - heapBefore) / 1024 / 1024).toBeLessThan(24);
    expect(snapB.drawCalls).toBeGreaterThan(0);
    viewer.dispose();
  });

  it("CPU frustum culling cuts the instances submitted", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 10, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(4000, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.cameras.focusOn(0, 0, 0);
    viewer.cameras.altitudeM = 260;
    viewer.cameras.snap();
    viewer.renderFrame(16);

    const ctx = {
      position: viewer.interpolator.outPosition,
      heading: viewer.interpolator.outHeading,
      classIdx: viewer.interpolator.outClassIdx,
      state: viewer.interpolator.outState,
      occupied: viewer.interpolator.outOccupied,
      actorId: viewer.interpolator.outActorId,
      count: viewer.interpolator.count,
      camera: viewer.camera,
    };
    const culled = viewer.actors.update({ ...ctx, cull: true });
    const all = viewer.actors.update({ ...ctx, cull: false });

    // eslint-disable-next-line no-console
    console.log(`culling: ${culled.drawn} drawn of ${all.drawn} live (${culled.culled} culled)`);
    expect(all.drawn).toBe(4000);
    expect(culled.drawn).toBeLessThan(all.drawn);
    expect(culled.drawn + culled.culled).toBe(all.drawn);
    viewer.dispose();
  });

  it("flies from the map into a chase view continuously, with no cut", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 8, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(300, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.cameras.focusOn(0, 0, 0);
    viewer.cameras.altitudeM = 700;
    viewer.cameras.setMode("map", true);
    viewer.renderFrame(16);

    const startZ = viewer.camera.position.z;
    expect(startZ).toBeGreaterThan(500);

    const actorId = stream.poses.actorId[10];
    viewer.flyTo(actorId, "chase");
    expect(viewer.cameras.mode).toBe("chase");
    expect(viewer.selectedActorId).toBe(actorId);

    let t = 16;
    let prevX = viewer.camera.position.x;
    let prevY = viewer.camera.position.y;
    let prevZ = viewer.camera.position.z;
    let maxJump = 0;
    const gap0 = Math.hypot(prevX - stream.poses.positions[30], prevY - stream.poses.positions[31], prevZ);
    for (let i = 0; i < 180; i++) {
      t += 1000 / 60;
      if (i % 6 === 0) {
        stream.advance(0.1);
        stream.delta();
        viewer.capture(stream.poses, t / 1000);
      }
      viewer.renderFrame(t);
      const jump = Math.hypot(
        viewer.camera.position.x - prevX,
        viewer.camera.position.y - prevY,
        viewer.camera.position.z - prevZ,
      );
      if (jump > maxJump) maxJump = jump;
      prevX = viewer.camera.position.x;
      prevY = viewer.camera.position.y;
      prevZ = viewer.camera.position.z;
    }

    const slot = viewer.followSlot;
    expect(slot).toBeGreaterThanOrEqual(0);
    const ax = viewer.interpolator.outPosition[slot * 3];
    const ay = viewer.interpolator.outPosition[slot * 3 + 1];
    const distance = Math.hypot(viewer.camera.position.x - ax, viewer.camera.position.y - ay);

    // eslint-disable-next-line no-console
    console.log(
      `fly-down: z ${startZ.toFixed(0)} m → ${viewer.camera.position.z.toFixed(1)} m, ` +
      `settled ${distance.toFixed(1)} m behind the vehicle, largest single-frame move ${maxJump.toFixed(1)} m ` +
      `(initial gap ${gap0.toFixed(0)} m)`,
    );

    expect(viewer.camera.position.z).toBeLessThan(20);
    expect(distance).toBeLessThan(20);
    // Continuity: no frame may cover more than a sixth of the original gap.
    expect(maxJump).toBeLessThan(gap0 / 6);
    viewer.dispose();
  });

  it("keeps the camera out of buildings", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 6, blockM: 120 });
    viewer.setWorld(grid.world);
    const b = grid.world.buildings;
    const ring = grid.world.ringPoints;
    const off = b.ringOff[0];
    const cx = (ring.x[off] + ring.x[off + 2]) / 2;
    const cy = (ring.y[off] + ring.y[off + 2]) / 2;
    const top = b.baseZM[0] + b.heightM[0];
    expect(viewer.worldRenderer.buildingTopAt(cx, cy)).toBeCloseTo(top, 3);

    viewer.cameras.setMode("chase", true);
    const pos = new (viewer.camera.position.constructor as new (x: number, y: number, z: number) => typeof viewer.camera.position)(cx, cy, 1);
    const look = new (viewer.camera.position.constructor as new (x: number, y: number, z: number) => typeof viewer.camera.position)(cx + 60, cy, 1);
    const moved = viewer.cameras.keepCameraOutsideBuildings(pos, look);
    expect(moved).toBe(true);
    expect(pos.z).toBeGreaterThanOrEqual(top);
    viewer.dispose();
  });

  it("picks the actor under the cursor", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 6, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(500, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.cameras.focusOn(0, 0, 0);
    viewer.cameras.altitudeM = 300;
    viewer.cameras.snap();
    viewer.renderFrame(16);

    // Aim straight down at an actor that nothing else overlaps, so the expected hit is unambiguous:
    // a top-down ray legitimately reports the nearest box, and two actors can share a lane position.
    const p = viewer.interpolator.outPosition;
    const n = viewer.interpolator.count;
    let slot = -1;
    for (let i = 0; i < n && slot < 0; i++) {
      if (viewer.interpolator.outOccupied[i] !== 1) continue;
      let isolated = true;
      for (let j = 0; j < n && isolated; j++) {
        if (j === i || viewer.interpolator.outOccupied[j] !== 1) continue;
        const dx = p[j * 3] - p[i * 3];
        const dy = p[j * 3 + 1] - p[i * 3 + 1];
        if (dx * dx + dy * dy < 30 * 30) isolated = false;
      }
      if (isolated) slot = i;
    }
    expect(slot).toBeGreaterThanOrEqual(0);
    const target = { x: p[slot * 3], y: p[slot * 3 + 1], z: p[slot * 3 + 2] + 0.7 };
    viewer.cameras.focusOn(target.x, target.y, 0);
    viewer.cameras.altitudeM = 120;
    viewer.cameras.snap();
    viewer.renderFrame(32);

    const hit = viewer.pickAtPixel(800, 450);
    expect(hit).not.toBeNull();
    expect(hit?.kind).toBe("actor");
    if (hit?.kind === "actor") {
      expect(hit.actorId).toBe(viewer.interpolator.outActorId[slot]);
      expect(hit.distanceM).toBeGreaterThan(0);
    }

    const miss = viewer.picker.pickAtNdc(0.999, -0.999);
    expect(miss === null || miss.kind === "ground" || miss.kind === "actor" || miss.kind === "site").toBe(true);
    viewer.dispose();
  });

  it("colours actors by state and honours the ground-truth lock", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 5, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(200, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.renderFrame(16);

    let attackers = 0;
    for (let i = 0; i < stream.poses.count; i++) {
      if (stream.poses.state[i] & ActorState.ATTACKER) attackers++;
    }
    expect(attackers).toBeGreaterThan(0);

    viewer.overlays.set("attackers_gt", true);
    expect(viewer.overlays.isEnabled("attackers_gt")).toBe(true);
    viewer.overlays.lockGroundTruth(true);
    expect(viewer.overlays.isEnabled("attackers_gt")).toBe(false);
    expect(viewer.overlays.set("attackers_gt", true)).toBe(false);
    viewer.actors.showGroundTruth = false;
    viewer.renderFrame(32);
    expect(viewer.overlays.catalogue().find((o) => o.name === "attackers_gt")?.label).toBe("Attackers (GT)");
    viewer.dispose();
  });

  it("toggles every overlay the catalogue advertises without throwing", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 5, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(120, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);

    for (const entry of viewer.overlays.catalogue()) {
      viewer.overlays.set(entry.name, true);
      viewer.overlays.setOpacity(entry.name, 0.8);
    }
    viewer.overlays.pulses.emit(0, 0, 1, 250, 0);
    viewer.overlays.pulses.emit(50, 50, 1, 180, 0.05);
    viewer.overlays.links.begin();
    viewer.overlays.links.add(0, 0, 1, 40, 40, 1, true, 0.9);
    viewer.overlays.links.add(0, 0, 1, -40, 30, 1, false, 0.4);
    viewer.overlays.links.end();
    viewer.overlays.heatmap.setCell(4, 4, 0.8);
    viewer.overlays.heatmap.commit();
    viewer.renderFrame(16);
    viewer.renderFrame(32);
    expect(viewer.overlays.pulses.count).toBe(2);
    expect(viewer.overlays.links.count).toBe(2);

    for (const entry of viewer.overlays.catalogue()) viewer.overlays.set(entry.name, false);
    viewer.renderFrame(48);
    viewer.dispose();
  });

  it("survives a stalled stream without flinging actors off the map", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 6, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(200, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    stream.advance(0.1);
    stream.delta();
    viewer.capture(stream.poses, 0.1);
    viewer.renderFrame(0);
    viewer.renderFrame(100);

    const snapshot = Float32Array.from(viewer.interpolator.outPosition.subarray(0, 600));
    // Ten seconds of silence.
    for (let t = 200; t <= 10_200; t += 100) viewer.renderFrame(t);
    let maxDrift = 0;
    for (let i = 0; i < 600; i++) {
      maxDrift = Math.max(maxDrift, Math.abs(viewer.interpolator.outPosition[i] - snapshot[i]));
    }
    // A further minute of silence must add nothing: the render clock has come to rest, it is not
    // still integrating (the failure the clamp exists to prevent).
    const atRest = Float32Array.from(viewer.interpolator.outPosition.subarray(0, 600));
    for (let t = 10_300; t <= 70_000; t += 100) viewer.renderFrame(t);
    let extraDrift = 0;
    for (let i = 0; i < 600; i++) {
      extraDrift = Math.max(extraDrift, Math.abs(viewer.interpolator.outPosition[i] - atRest[i]));
    }

    // The bound is the documented one rather than a round number: one render delay of
    // interpolation plus the extrapolation budget, at the fastest actor's speed.
    const interp = viewer.interpolator;
    let fastest = 0;
    for (let i = 0; i < 200; i++) fastest = Math.max(fastest, interp.outSpeed[i]);
    const bound = (interp.delaySteps + interp.maxExtrapolationSteps) * interp.intervalSeconds * fastest;
    // eslint-disable-next-line no-console
    console.log(
      `stall clamp: ${maxDrift.toFixed(3)} m of drift over 10 s of silence (bound ${bound.toFixed(3)} m), `
      + `${(extraDrift * 1000).toFixed(2)} mm more over the next 60 s`,
    );
    expect(viewer.lastFrame.stalled).toBe(true);
    expect(maxDrift).toBeLessThanOrEqual(bound + 1e-3);
    expect(extraDrift).toBeLessThan(0.01);
    viewer.dispose();
  });
});
