/**
 * The simulator, driven through its page, against the real engine.
 *
 * Four things the owner reported or asked for, each as a test:
 *
 *  1. **Applying settings works** — an edit in the form reaches the next run, proven by what the
 *     run produced: its horizon, its output digest (and that the same seed reproduces it), its
 *     fleet, its radio outcome, its message family.
 *  2. **Every transport control does what it says**, and a stopped run's kernel is gone.
 *  3. **Run after run keeps working** — a soak of consecutive runs with edits and scenario switches
 *     in between, with the engine's memory and the tab's heap measured along the way.
 *     `VWP_SOAK_RUNS` sets the count (default 4; the long soak is 30).
 *  4. **The page survives the engine going away** — a reload mid-run reattaches, and a killed and
 *     restarted engine is reconnected to with a plain-language message in between.
 */

import { expect, test, type Page } from "@playwright/test";

import {
  EngineProcess,
  heapBytes,
  open,
  runToEnd,
  setField,
  status,
  streaming,
  writeScenarios,
  type Status,
} from "./support.js";

const scenarios = writeScenarios();
const engine = new EngineProcess(scenarios.a);

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  await engine.start();
});

test.afterAll(async () => {
  await engine.stop();
});

async function apply(page: Page): Promise<string> {
  await page.getByTestId("apply").click();
  const message = page.getByTestId("scenario-message");
  await expect(message).toContainText(/Applied|nothing to change/);
  return message.innerText();
}

test("an edited setting reaches the next run: duration, seed, arrival rate, radio, message family", async ({ page }) => {
  await open(page);

  // --- duration: the horizon of the next run ----------------------------------------------------
  await setField(page, "/time/duration_s", "4");
  expect(await apply(page)).toContain("Applied 1 change");
  await expect(page.getByTestId("staged-note")).toContainText("waiting for the next run");
  const first = await runToEnd(page);
  expect(first.t_end_ns).toBe(4_000_000_000);
  expect(first.t_ns, "the clock stops at the horizon").toBe(4_000_000_000);
  await expect(page.getByTestId("time-state")).toContainText("4.0 s");
  await expect(page.getByTestId("staged-note")).toHaveCount(0);

  // --- seed: the digest moves, and the same seed reproduces it ---------------------------------
  const again = await runToEnd(page);
  expect(again.engine.output_digest, "the same settings and seed reproduce the run").toBe(first.engine.output_digest);
  await setField(page, "/seed", "0x2a");
  await apply(page);
  const seeded = await runToEnd(page);
  expect(seeded.engine.output_digest, "another seed is another run").not.toBe(first.engine.output_digest);
  const seededAgain = await runToEnd(page);
  expect(seededAgain.engine.output_digest).toBe(seeded.engine.output_digest);

  // --- arrival rate: the fleet ------------------------------------------------------------------
  await setField(page, "/time/duration_s", "20");
  await setField(page, "/actors/vehicles/demand/rate_veh_per_h", "30");
  // Run applies unapplied edits itself.
  const sparse = await runToEnd(page);
  expect(sparse.t_end_ns).toBe(20_000_000_000);
  await setField(page, "/actors/vehicles/demand/rate_veh_per_h", "6000");
  const dense = await runToEnd(page);
  expect(dense.engine.stats.actors_seen, "raising the arrival rate spawns more vehicles").toBeGreaterThan(
    sparse.engine.stats.actors_seen * 3,
  );

  // --- radio: the PHY and MAC tiers change the reception outcome --------------------------------
  expect(dense.engine.stats.rx_attempts, "the dense run has receptions to compare").toBeGreaterThan(0);
  await setField(page, "/radio/tiers/phy", "high");
  await setField(page, "/radio/tiers/mac", "high");
  const high = await runToEnd(page);
  expect([high.engine.stats.rx_ok, high.engine.output_digest]).not.toEqual([
    dense.engine.stats.rx_ok,
    dense.engine.output_digest,
  ]);
  // A transmit-power key, when the engine publishes one, must move the received power.
  const txPower = page.locator('[data-testid="setting"][data-pointer*="tx_power"]');
  if ((await txPower.count()) > 0) {
    const pointer = (await txPower.first().getAttribute("data-pointer")) ?? "";
    await setField(page, pointer, "10");
    const quieter = await runToEnd(page);
    expect(quieter.engine.stats.mean_rssi_dbm ?? 0).toBeLessThan((high.engine.stats.mean_rssi_dbm ?? 0) - 5);
  } else {
    test.info().annotations.push({
      type: "not tested",
      description: "this engine publishes no transmit-power setting yet; the PHY/MAC tier is the radio proof",
    });
  }

  // --- message family: what goes on the air ------------------------------------------------------
  await setField(page, "/messages/sets", '["cam"]');
  const cam = await runToEnd(page);
  expect(cam.engine.stats.tx_by_type.cam ?? 0).toBeGreaterThan(0);
  expect(cam.engine.stats.tx_by_type.bsm, "no BSM once the set is [cam]").toBeUndefined();

  // --- a setting the engine does not act on is marked, and an edit to it is called out ---------
  await page.getByTestId("settings-filter").fill("visibility_m");
  const inert = page.locator('[data-testid="setting"][data-pointer="/weather/visibility_m"]');
  await expect(inert.getByTestId("field-status")).toHaveText("not applied");
  await inert.locator("input").fill("250");
  await expect(page.getByTestId("inert-edits")).toContainText("nothing in this build");
  await page.getByTestId("discard-edits").click();
  await page.getByTestId("settings-filter").fill("");

  // --- an edit the loader refuses is said so, and nothing is held -------------------------------
  await setField(page, "/radio/tiers/propagation", "abstract");
  await page.getByTestId("apply").click();
  await expect(page.getByTestId("scenario-message")).toContainText("refused");
  await expect(page.getByTestId("validation-err")).toContainText("propagation");
  await page.getByTestId("discard-edits").click();
});

test("every transport control does what it says, and a stopped run leaves no kernel behind", async ({ page }) => {
  await open(page);
  const baseline = engine.threads();
  await setField(page, "/time/duration_s", "30");
  await setField(page, "/actors/vehicles/demand/rate_veh_per_h", "3000");
  await apply(page);
  await page.getByTestId("speed").selectOption("1");
  await expect.poll(async () => (await status(page)).speed).toBe(1);
  await page.getByTestId("run-start").click();
  await expect.poll(async () => (await status(page)).state).toBe("running");
  await expect.poll(async () => (await status(page)).t_ns, { timeout: 20_000 }).toBeGreaterThan(1_000_000_000);
  expect((await status(page)).engine.kernel_threads).toBe(1);

  await page.getByTestId("pause").click();
  await expect.poll(async () => (await status(page)).state).toBe("paused");
  const held = (await status(page)).t_ns;
  await page.waitForTimeout(600);
  expect((await status(page)).t_ns, "a paused clock does not move").toBe(held);

  await page.getByTestId("step").click();
  await expect.poll(async () => (await status(page)).t_ns).toBe(held + 100_000_000);

  await page.getByTestId("step-back").click();
  await expect.poll(async () => (await status(page)).t_ns).toBe(held);

  await page.getByTestId("seek-start").click();
  await expect.poll(async () => (await status(page)).t_ns).toBe(0);

  await page.getByTestId("play").click();
  await expect.poll(async () => (await status(page)).state).toBe("running");
  await expect.poll(async () => (await status(page)).t_ns).toBeGreaterThan(held);

  await page.getByTestId("stop").click();
  await expect.poll(async () => (await status(page)).state).toBe("finished");
  expect((await status(page)).engine.kernel_threads, "run.stop joins the kernel").toBe(0);
  expect(engine.threads(), "the process is back to its thread count without a kernel").toBeLessThanOrEqual(baseline);
  await expect(page.getByTestId("connection-state"), "the page keeps its connection after a stop").toHaveAttribute(
    "data-state",
    "streaming",
  );

  // Restart from the transport bar, at full speed, to the end.
  await page.getByTestId("speed").selectOption("0");
  const restarted = await runToEnd(page, "restart");
  expect(restarted.t_ns).toBe(restarted.t_end_ns);
  // After the end the socket is still open, so the bar can still seek back through the run.
  await page.getByTestId("seek-start").click();
  await expect.poll(async () => (await status(page)).t_ns).toBe(0);
  await expect(page.getByTestId("time-notice")).toHaveCount(0);
});

test("run after run keeps working, with edits and scenario switches between runs", async ({ page }) => {
  const runs = Number(process.env.VWP_SOAK_RUNS ?? "4");
  await open(page);
  await setField(page, "/time/duration_s", "3");
  await page.getByTestId("speed").selectOption("0");
  const samples: { run: number; rssKb: number; heapMb: number; threads: number }[] = [];
  const sampleAt = new Set([1, 2, 10, 20, 30, runs]);
  let previous: Status | null = null;
  for (let i = 1; i <= runs; i++) {
    if (i % 5 === 0) {
      // A scenario switch: load the other preset, which the next Run runs.
      const target = i % 10 === 0 ? "e2e-grid-a" : "e2e-grid-b";
      await page.locator(`[data-testid="preset-load"][data-preset="${target}"]`).click();
      await expect(page.getByTestId("scenario-message")).toContainText(/Loaded|already running/);
    } else if (i % 3 === 0) {
      await setField(page, "/time/duration_s", String(2 + (i % 2)));
    }
    const done = await runToEnd(page);
    expect(done.t_ns, `run ${i} reached its end`).toBe(done.t_end_ns);
    // The kernel thread ends a moment after its last step is streamed.
    await expect.poll(async () => (await status(page)).engine.kernel_threads, { message: `run ${i}: its kernel ended` }).toBe(0);
    if (previous !== null) expect(done.generation, `run ${i} is a new run`).toBe(previous.generation + 1);
    // It streamed: the page's own pose buffer reached the run's last instant.
    await expect
      .poll(
        async () =>
          page.evaluate(() => Number((window.__vwpStudio?.engine.client?.poses as unknown as { simTimeNs?: bigint } | undefined)?.simTimeNs ?? -1)),
        { message: `run ${i} streamed to its end` },
      )
      .toBe(done.t_end_ns);
    await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "streaming");
    previous = done;
    if (sampleAt.has(i)) {
      samples.push({ run: i, rssKb: engine.rssKb(), heapMb: (await heapBytes(page)) / 2 ** 20, threads: engine.threads() });
    }
  }
  console.warn(`soak of ${runs} runs:\n${samples.map((s) => `  run ${s.run}: engine RSS ${(s.rssKb / 1024).toFixed(1)} MB, tab heap ${s.heapMb.toFixed(1)} MB, threads ${s.threads}`).join("\n")}`);
  if (samples.length >= 2) {
    // Flat: measured from a warm sample — run 10 in the long soak, run 2 in the short one — so
    // the one-off costs of the first run (the world payload, the shaders, the allocator's first
    // arenas, the retained timeline filling) are not counted as growth.
    const warm = samples.find((s) => s.run === 10) ?? samples.find((s) => s.run === 2) ?? samples[0];
    const last = samples[samples.length - 1];
    expect(last.rssKb, "engine memory is flat across runs").toBeLessThan(warm.rssKb * 1.25 + 20_000);
    expect(last.heapMb, "tab memory is flat across runs").toBeLessThan(warm.heapMb * 1.25 + 10);
    expect(last.threads, "no thread accumulates across runs").toBeLessThanOrEqual(warm.threads);
  }
});

test("a reload mid-run reattaches, and a restarted engine is reconnected to", async ({ page }) => {
  await open(page);
  await setField(page, "/time/duration_s", "60");
  await setField(page, "/actors/vehicles/demand/rate_veh_per_h", "6000");
  await apply(page);
  await page.getByTestId("speed").selectOption("1");
  await expect.poll(async () => (await status(page)).speed).toBe(1);
  const before = (await status(page)).generation;
  await page.getByTestId("run-start").click();
  await expect.poll(async () => (await status(page)).generation).toBe(before + 1);
  await expect.poll(async () => JSON.stringify(await status(page))).toContain('"state":"running"');
  await expect.poll(async () => page.evaluate(() => window.__vwpStudio?.actorCount() ?? 0), { timeout: 30_000 }).toBeGreaterThan(0);

  // Reload mid-run: the new page attaches to the run in progress.
  await page.reload();
  await streaming(page);
  expect((await status(page)).state).toBe("running");
  await expect.poll(async () => page.evaluate(() => window.__vwpStudio?.actorCount() ?? 0), { timeout: 15_000 }).toBeGreaterThan(0);

  // Reload a paused run: the page shows where it stands at once, not an empty city.
  await page.getByTestId("pause").click();
  await expect.poll(async () => (await status(page)).state).toBe("paused");
  await page.reload();
  await streaming(page);
  await expect.poll(async () => page.evaluate(() => window.__vwpStudio?.actorCount() ?? 0), { timeout: 15_000 }).toBeGreaterThan(0);

  // The engine dies with the page open.
  await engine.stop("SIGKILL");
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "reconnecting", { timeout: 15_000 });
  await expect(page.getByTestId("status-headline")).toContainText("Lost contact with the engine");
  await expect(page.getByTestId("status-banner")).toContainText("reconnects by itself");

  // It comes back: the page reconnects by itself and can run again.
  await engine.start();
  await streaming(page, 60_000);
  await expect(page.getByText("Lost contact with the engine")).toHaveCount(0);
  const after = await runToEnd(page);
  expect(after.state).toBe("finished");
});
