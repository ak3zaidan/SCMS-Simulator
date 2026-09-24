/**
 * Two issues the wave A integrator saw on the live page, checked where they were seen.
 *
 * 1. A forward seek reached only as far as the kernel's bounded lead (12.8 s at the defaults):
 *    the scrub bar could not reach most of a run nobody had watched. A seek to the end of a
 *    paused 60 s run standing at 0 s must now land there.
 * 2. The plots kept the previous run's history after "Run again": a 20 s run drew its latency
 *    on a 50–125 s axis. After a 60 s run and then a 16 s one, no plotted series may hold a
 *    sample past 16 s.
 */

import { expect, test } from "@playwright/test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { EngineProcess, REPO, open, runToEnd, setField, status } from "./support.js";

function scenario(): string {
  const text = readFileSync(join(REPO, "scenarios/phase1-grid.yaml"), "utf8")
    .replace("name: phase1-grid", "name: e2e-seek")
    .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 1800.0")
    .replace("cols: 13", "cols: 4")
    .replace("rows: 34", "rows: 4");
  const dir = join(tmpdir(), `vwp-engine-seek-${process.pid}`);
  mkdirSync(dir, { recursive: true });
  const path = join(dir, "e2e-seek.yaml");
  writeFileSync(path, text);
  return path;
}

const engine = new EngineProcess(scenario());

test.beforeAll(async () => {
  await engine.start();
});
test.afterAll(async () => {
  await engine.stop();
});

test("a seek past the kernel's lead lands, and a new run's plots start empty", async ({ page }) => {
  await open(page);
  const before = await status(page);
  expect(before.state).toBe("paused");
  expect(before.t_end_ns).toBe(60_000_000_000);

  // --- 1. the end of the run, from 0 s, with the kernel's lead far shorter ----------------------
  const scrub = page.getByTestId("scrub-range");
  await scrub.focus();
  await page.keyboard.press("End");
  await expect.poll(async () => (await status(page)).t_ns, { timeout: 120_000 }).toBeGreaterThanOrEqual(59_000_000_000);
  await expect(page.getByTestId("time-notice")).toHaveCount(0);
  const landed = (await status(page)).t_ns;
  // eslint-disable-next-line no-console -- the measured numbers are the evidence this test reports
  console.log(`seek to the end of a 60 s run from 0 s landed at ${landed / 1e9} s`);

  // --- 2. a long run, then a short one: the plots hold only the short one ----------------------
  await runToEnd(page);
  const longMax = await page.evaluate(() => {
    const m = (window.__vwpStudio?.engine as unknown as { metrics: { names(): readonly string[]; get(n: string): [number[], unknown[]] } }).metrics;
    return Math.max(0, ...m.names().map((n) => Math.max(0, ...m.get(n)[0])));
  });
  expect(longMax, "the 60 s run plotted samples late in the run").toBeGreaterThan(30);

  await setField(page, "/time/duration_s", "16");
  const done = await runToEnd(page);
  expect(done.t_end_ns).toBe(16_000_000_000);
  const shortMax = await page.evaluate(() => {
    const m = (window.__vwpStudio?.engine as unknown as { metrics: { names(): readonly string[]; get(n: string): [number[], unknown[]] } }).metrics;
    return Math.max(0, ...m.names().map((n) => Math.max(0, ...m.get(n)[0])));
  });
  expect(shortMax, "no plotted sample is later than the 16 s run").toBeLessThanOrEqual(16);
  // eslint-disable-next-line no-console -- the measured numbers are the evidence this test reports
  console.log(`plots: the 60 s run's latest sample at ${longMax} s; after a 16 s run, ${shortMax} s`);
});
