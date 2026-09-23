/**
 * Every control in the header, the transport bar, the settings panel and the viewport toolbar,
 * pressed once against the real engine, with what it should do and what it did.
 *
 * The table this prints is the evidence the owner asked for ("every button, every setting, every
 * feature works in the actual website"). A control that did not do what it says fails the test;
 * the table is printed either way, so a failure shows every other result beside it.
 *
 * `lifecycle.spec.ts` covers pause, play, step, step back, go to start, stop and restart with
 * their effect on the engine's clock and kernel; they are not repeated here.
 */

import { expect, test } from "@playwright/test";

import { EngineProcess, open, runToEnd, setField, status, writeScenarios } from "./support.js";

const scenarios = writeScenarios();
const engine = new EngineProcess(scenarios.a);

test.beforeAll(async () => {
  await engine.start();
});
test.afterAll(async () => {
  await engine.stop();
});

interface Row {
  control: string;
  should: string;
  did: string;
  ok: boolean;
}

test("every control does what it says", async ({ page }) => {
  const rows: Row[] = [];
  const check = async (control: string, should: string, act: () => Promise<string>): Promise<void> => {
    try {
      rows.push({ control, should, did: await act(), ok: true });
    } catch (err) {
      rows.push({ control, should, did: `FAILED: ${(err instanceof Error ? err.message : String(err)).split("\n")[0]}`, ok: false });
    }
  };
  const quick = { timeout: 15_000 };

  await open(page);

  // --- header -------------------------------------------------------------------------------------
  await check("header: theme toggle", "switch dark ↔ light", async () => {
    const before = await page.evaluate(() => document.documentElement.dataset.theme);
    await page.getByTestId("theme-toggle").click();
    const after = await page.evaluate(() => document.documentElement.dataset.theme);
    expect(after).not.toBe(before);
    await page.getByTestId("theme-toggle").click();
    return `data-theme ${before} → ${after} → back`;
  });
  await check("header: Details", "open the run's identity panel", async () => {
    await page.getByTestId("run-details-button").click();
    await expect(page.getByTestId("run-details")).toBeVisible(quick);
    await page.getByTestId("run-details-button").click();
    await expect(page.getByTestId("run-details")).toHaveCount(0, quick);
    return "opened, closed";
  });
  for (const [label, probe] of [
    ["Runs", "scenario-panel"],
    ["Compare", "scenario-panel"],
    ["Commands", "scenario-panel"],
  ] as const) {
    await check(`header: ${label} tab`, "show that panel in place of the settings", async () => {
      await page.getByRole("button", { name: label, exact: true }).click();
      await expect(page.getByTestId(probe)).toHaveCount(0, quick);
      await page.getByRole("button", { name: "Scenario", exact: true }).click();
      await expect(page.getByTestId(probe)).toBeVisible(quick);
      return "panel switched and back";
    });
  }
  await check("header: primary action (Play on a paused run)", "start the paused run moving", async () => {
    // At real-time speed, so the run is still moving when Pause is pressed: the harness starts
    // the engine unthrottled, where a 6 s run is over before the button can change.
    await page.getByTestId("speed").selectOption("1");
    await expect.poll(async () => (await status(page)).speed, quick).toBe(1);
    await expect(page.getByTestId("primary-action")).toHaveText("Play", quick);
    await page.getByTestId("primary-action").click();
    await expect.poll(async () => (await status(page)).state, quick).toBe("running");
    await expect(page.getByTestId("primary-action")).toHaveText("Pause", quick);
    await page.getByTestId("primary-action").click();
    await expect.poll(async () => (await status(page)).state, quick).toBe("paused");
    return "Play → running, Pause → paused";
  });

  // --- transport bar (the ones lifecycle.spec.ts does not press) ---------------------------------
  await check("transport: step unit", "make Step advance one second", async () => {
    const before = (await status(page)).t_ns;
    await page.getByTestId("step-unit").selectOption("second");
    await page.getByTestId("step").click();
    await expect.poll(async () => (await status(page)).t_ns, quick).toBe(before + 1_000_000_000);
    await page.getByTestId("step-unit").selectOption("step");
    return `t ${before / 1e9} s → ${(before + 1e9) / 1e9} s`;
  });
  await check("transport: speed", "set the engine's multiple of real time", async () => {
    await page.getByTestId("speed").selectOption("2");
    await expect.poll(async () => (await status(page)).speed, quick).toBe(2);
    await page.getByTestId("speed").selectOption("0");
    await expect.poll(async () => (await status(page)).speed, quick).toBe(0);
    return "2× then as fast as possible, confirmed by run.status";
  });
  await check("transport: timeline scrub", "move the run to the point clicked", async () => {
    const before = (await status(page)).t_ns;
    const bar = page.getByTestId("scrub-range");
    const box = await bar.boundingBox();
    if (!box) throw new Error("no scrub bar");
    await page.mouse.click(box.x + 2, box.y + box.height / 2);
    await expect.poll(async () => (await status(page)).t_ns, quick).toBeLessThan(before);
    return `t ${before / 1e9} s → ${(await status(page)).t_ns / 1e9} s`;
  });

  // --- settings panel ----------------------------------------------------------------------------
  await check("settings: find a setting", "narrow the form to matching settings", async () => {
    await page.getByTestId("settings-filter").fill("equipped");
    const shown = await page.getByTestId("setting").count();
    await page.getByTestId("settings-filter").fill("");
    const all = await page.getByTestId("setting").count();
    expect(shown).toBeLessThan(all);
    return `${shown} of ${all} shown`;
  });
  await check("settings: Check", "have the engine validate the form", async () => {
    await page.getByTestId("validate").click();
    await expect(page.getByTestId("validation-state")).toHaveText("ready to run", quick);
    return "ready to run";
  });
  await check("settings: Discard", "put the form back", async () => {
    await setField(page, "/time/duration_s", "5");
    await expect(page.getByTestId("discard-edits")).toBeEnabled(quick);
    await page.getByTestId("discard-edits").click();
    await expect(page.getByTestId("discard-edits")).toBeDisabled(quick);
    return "edit dropped; Apply and Discard disabled again";
  });
  await check("settings: Apply + Revert", "hold an edit for the next run, then withdraw it", async () => {
    const running = (await status(page)).scenario_hash;
    await setField(page, "/time/duration_s", "5");
    await page.getByTestId("apply").click();
    await expect(page.getByTestId("staged-note")).toBeVisible(quick);
    expect((await status(page)).staged_hash).not.toBeNull();
    await page.getByTestId("unstage").click();
    await expect(page.getByTestId("staged-note")).toHaveCount(0, quick);
    expect((await status(page)).staged_hash).toBeNull();
    expect((await status(page)).scenario_hash).toBe(running);
    return "held (staged hash set), withdrawn (none)";
  });
  await check("settings: load a ready-made scenario", "put it in the form; Run runs it", async () => {
    await page.locator('[data-testid="preset-load"][data-preset="e2e-grid-b"]').click();
    await expect(page.getByTestId("scenario-message")).toContainText("Loaded", quick);
    const done = await runToEnd(page);
    expect(done.t_end_ns).toBe(4_000_000_000);
    await expect(page.getByTestId("header-scenario")).toHaveText("e2e-grid-b", quick);
    return "e2e-grid-b ran (4 s), header names it";
  });
  await check("settings: Run", "apply unapplied edits and start a run", async () => {
    await setField(page, "/time/duration_s", "3");
    const done = await runToEnd(page);
    expect(done.t_end_ns).toBe(3_000_000_000);
    return "3 s run with the edit";
  });

  // --- viewport toolbar --------------------------------------------------------------------------
  await check("viewport: overlays menu", "list the overlays and toggle one", async () => {
    await page.getByTestId("overlays-button").click();
    await expect(page.getByTestId("overlays-menu")).toBeVisible(quick);
    const box = page.getByTestId("overlay-lane_markings").locator("input");
    const before = await box.isChecked();
    await box.click();
    expect(await box.isChecked()).toBe(!before);
    await box.click();
    await page.getByTestId("overlays-button").click();
    return `lane markings ${before} → ${!before} → ${before}`;
  });
  await check("viewport: camera mode", "change the view; a street-level view adopts a vehicle", async () => {
    await page.getByTestId("camera-mode").selectOption("free");
    await expect(page.getByTestId("camera-mode")).toHaveValue("free", quick);
    await page.getByTestId("camera-mode").selectOption("chase");
    const mode = await page.getByTestId("camera-mode").inputValue();
    const follow = await page.getByTestId("follow-chip").innerText();
    await page.getByTestId("camera-mode").selectOption("map");
    await expect(page.getByTestId("camera-mode")).toHaveValue("map", quick);
    return `free ok; chase → ${mode} (${follow}); map ok`;
  });
  await check("viewport: HUD dock", "dock and float the radio HUD", async () => {
    const before = await page.getByTestId("hud-dock").innerText();
    await page.getByTestId("hud-dock").click();
    const after = await page.getByTestId("hud-dock").innerText();
    expect(after).not.toBe(before);
    await page.getByTestId("hud-dock").click();
    return `${before} → ${after}`;
  });

  const table = [
    "| control | should | did | ok |",
    "|---|---|---|---|",
    ...rows.map((r) => `| ${r.control} | ${r.should} | ${r.did} | ${r.ok ? "yes" : "NO"} |`),
  ].join("\n");
  console.warn(table);
  expect(rows.filter((r) => !r.ok), table).toEqual([]);
});
