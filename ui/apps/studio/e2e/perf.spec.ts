/**
 * Frame-rate measurement, run once per traffic size (`VWP_ACTORS=200`, then `VWP_ACTORS=5000`).
 *
 * The number is the viewer's own `FrameStats` (09-ui §4's budget instrument), sampled after the
 * scene has settled, plus an independent `requestAnimationFrame` count over a fixed wall-clock
 * window so the two can be compared. Headless chromium here runs on SwiftShader, so these are
 * software-rasteriser numbers and a floor, not a GPU figure.
 */

import { test, expect } from "@playwright/test";
import { appendFileSync } from "node:fs";

const actors = process.env.VWP_ACTORS ?? "200";
const mode = process.env.VWP_HEADED === "1" ? "headed-gpu" : "headless-swiftshader";

test(`frame rate with ${actors} actors`, async ({ page }) => {
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveText("streaming", { timeout: 60_000 });

  // Wait until the pose buffer holds roughly the requested traffic and the world is built.
  await expect
    .poll(
      async () =>
        page.evaluate(() => (window as unknown as { __vwpStudio?: { actorCount(): number } }).__vwpStudio?.actorCount() ?? 0),
      { timeout: 90_000, intervals: [1000] },
    )
    .toBeGreaterThan(Number(actors) * 0.5);

  // Let the LOD and shadow work settle, then measure.
  await page.waitForTimeout(6000);

  const result = await page.evaluate(async () => {
    const api = (window as unknown as {
      __vwpStudio?: {
        engine: {
          viewer: {
            stats: {
              reset(): void;
              snapshot(): {
                fps: number; fpsAverage: number; frameMs: number; p50Ms: number; p95Ms: number; p99Ms: number;
                cpuMs: number; renderMs: number; drawCalls: number; triangles: number;
                actorInstances: number; actorCulled: number; actorLive: number; buildingsVisible: number;
              };
            };
          } | null;
          client: { poses: { count: number } } | null;
        };
      };
    }).__vwpStudio;
    const viewer = api?.engine.viewer;
    viewer?.stats.reset();

    // Independent rAF count over 5 s of wall clock.
    const frames = await new Promise<number>((resolve) => {
      let n = 0;
      const t0 = performance.now();
      const tick = (): void => {
        n++;
        if (performance.now() - t0 >= 5000) resolve(n);
        else requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });

    const snap = viewer?.stats.snapshot();
    return {
      rafFps: frames / 5,
      poseCount: api?.engine.client?.poses.count ?? 0,
      ...(snap ?? {}),
    };
  });

  const line = `mode=${mode} actors=${actors} rafFps=${result.rafFps.toFixed(1)} statsFps=${(result.fps ?? 0).toFixed(1)} avg=${(result.fpsAverage ?? 0).toFixed(1)} frameMs=${(result.frameMs ?? 0).toFixed(2)} p95Ms=${(result.p95Ms ?? 0).toFixed(2)} cpuMs=${(result.cpuMs ?? 0).toFixed(2)} draws=${result.drawCalls ?? 0} tris=${result.triangles ?? 0} drawn=${result.actorInstances ?? 0} culled=${result.actorCulled ?? 0} live=${result.actorLive ?? 0} buildings=${result.buildingsVisible ?? 0} poses=${result.poseCount}`;
  // eslint-disable-next-line no-console
  console.log(`FPS-RESULT ${line}`);
  appendFileSync("screenshots/fps.txt", `${line}\n`);

  await page.screenshot({ path: `screenshots/perf-${actors}-actors-${mode}.png` });
  expect(result.rafFps).toBeGreaterThan(0);
});
