/**
 * The chase view's message and queue inspector, against the real engine.
 *
 * The owner's ask: "when you go down into a specific chase view for a vehicle, the user should be
 * able to see the broadcast messages being sent out and their content, along with the queue of that
 * specific node." This follows a vehicle the way a click does (`engine.selectActor`, chase view),
 * and checks the page end to end:
 *
 *  * a BSM appears under Sent, and its fields — decoded on the server from the SPDU the node signed —
 *    match the vehicle's pose in the HUD to within the GNSS model's error;
 *  * opening it shows the 1609.2 envelope and the SPDU in hex with its spans, and it stays open while
 *    the stream moves on (the expand race the wave A integrator saw);
 *  * Received lists receptions with their senders and fates;
 *  * Queues shows the five queues and, within a few seconds of a dense run, messages waiting in them.
 */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { expect, test, type Page } from "@playwright/test";

import { EngineProcess, REPO, open, status } from "./support.js";

let engine: EngineProcess;

test.beforeAll(async () => {
  const source = readFileSync(join(REPO, "scenarios/phase1-grid.yaml"), "utf8");
  const dir = join(tmpdir(), `vwp-engine-e2e-feed-${process.pid}`);
  mkdirSync(dir, { recursive: true });
  const path = join(dir, "e2e-feed.yaml");
  writeFileSync(
    path,
    source
      .replace("name: phase1-grid", "name: e2e-feed")
      .replace("duration_s: 60.0", "duration_s: 120.0")
      .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 3000.0"),
  );
  engine = new EngineProcess(path);
  await engine.start();
});

test.afterAll(async () => {
  await engine?.stop();
});

/** A vehicle carrying a radio, from the page's own node table. */
async function anEquippedActor(page: Page): Promise<number> {
  return page.evaluate(() => {
    const e = window.__vwpStudio?.engine as unknown as { nodeByActor: Map<number, number> };
    const actors = [...e.nodeByActor.keys()];
    return actors.length > 0 ? actors[0] : -1;
  });
}

const num = async (page: Page, testid: string): Promise<number> =>
  Number(await page.getByTestId(testid).first().getAttribute("data-value"));

function bearingGap(a: number, b: number): number {
  const d = (((a - b) % 360) + 360) % 360;
  return Math.min(d, 360 - d);
}

/** The engine's projection (`lib/geo.ts`, `v2xw_core::geo`), for comparing two lat/lon pairs in metres. */
function metres(lat0: number, lon0: number, lat1: number, lon1: number): number {
  const phi = (lat0 * Math.PI) / 180;
  const mLat = 111_132.92 - 559.82 * Math.cos(2 * phi) + 1.175 * Math.cos(4 * phi);
  const mLon = 111_412.84 * Math.cos(phi) - 93.5 * Math.cos(3 * phi);
  return Math.hypot((lat1 - lat0) * mLat, (lon1 - lon0) * mLon);
}

test("follow a vehicle: its BSMs, what it hears and its queues", async ({ page }) => {
  await open(page);
  await page.getByTestId("speed").selectOption("1");
  await page.getByTestId("play").click();
  await expect.poll(async () => (await status(page)).state, { timeout: 30_000 }).toBe("running");

  // Wait for a vehicle with a radio, then follow it the way a click in the viewport does.
  let actor = -1;
  await expect
    .poll(
      async () => {
        actor = await anEquippedActor(page);
        return actor;
      },
      { timeout: 60_000 },
    )
    .toBeGreaterThanOrEqual(0);
  await page.evaluate(async (a) => {
    const e = window.__vwpStudio?.engine as unknown as { selectActor(a: number, m: string): Promise<void> };
    await e.selectActor(a, "chase");
  }, actor);

  // Chase view opens the Messages tab on its own; the panel sits in the right-hand column, beside
  // the viewport, never over it.
  const panel = page.getByTestId("message-panel");
  await expect(panel).toBeVisible();
  const box = await panel.boundingBox();
  const view = await page.getByTestId("viewstack").boundingBox();
  expect(box && view && box.x >= view.x + view.width - 1, "the panel is outside the viewport").toBe(true);

  // --- Sent: a BSM with decoded fields ------------------------------------------------------------
  await page.getByTestId("feed-tab-sent").click();
  await expect(page.getByTestId("feed-row-sent").first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("feed-row-sent").first()).toContainText("BSM");

  // Freeze the run so the HUD pose and the newest BSM describe one instant (the BSM is at most a
  // tenth of a second plus one push older than the pose).
  await page.getByTestId("pause").click();
  await expect.poll(async () => (await status(page)).state, { timeout: 15_000 }).toBe("paused");
  await page.waitForTimeout(800);
  await expect(page.getByTestId("hud-pose")).toBeVisible();
  await page.getByTestId("feed-row-sent").first().click();
  const detail = page.getByTestId("feed-detail");
  await expect(detail).toBeVisible();
  for (const k of ["msg_cnt", "temp_id", "sec_mark", "lat", "lon", "elev", "speed", "heading", "accel_long", "brakes_wheels", "width", "length"]) {
    await expect(detail.getByTestId(`feed-field-${k}`), `decoded field ${k}`).toHaveCount(1);
  }
  const bsm = {
    lat: await num(page, "feed-field-lat"),
    lon: await num(page, "feed-field-lon"),
    speed: await num(page, "feed-field-speed"),
    heading: await num(page, "feed-field-heading"),
  };
  const hud = {
    lat: await num(page, "hud-pose-lat"),
    lon: await num(page, "hud-pose-lon"),
    speed: await num(page, "hud-pose-speed"),
    heading: await num(page, "hud-pose-heading"),
  };
  const gap = metres(hud.lat, hud.lon, bsm.lat, bsm.lon);
  // The BSM carries the vehicle's GNSS belief of its reference point, the HUD the stream's body
  // centre: a few metres of GNSS error (bursts to ~20 m) and half a car apart. A BSM of another
  // vehicle, or one read at the wrong scale, is tens to thousands of metres away.
  expect(gap, `BSM ${JSON.stringify(bsm)} vs HUD ${JSON.stringify(hud)}`).toBeLessThan(30);
  expect(Math.abs(bsm.speed - hud.speed), `speed ${bsm.speed} vs ${hud.speed}`).toBeLessThan(2.5);
  if (hud.speed > 2) expect(bearingGap(bsm.heading, hud.heading), `heading ${bsm.heading} vs ${hud.heading}`).toBeLessThan(20);

  // The envelope and the octets.
  await expect(detail.getByTestId("feed-security")).toBeVisible();
  await expect(detail.getByTestId("feed-hashedid8")).toHaveText(/^[0-9a-f]{16}$/);
  await expect(detail.getByTestId("feed-hex")).toBeVisible();
  for (const span of ["span-1609-2-header", "span-payload", "span-signature"]) {
    await expect(detail.getByTestId(`feed-${span}`)).toHaveCount(1);
  }

  // The opened message stays open while the stream moves on.
  const opened = await detail.getAttribute("data-msg");
  await page.getByTestId("play").click();
  await page.waitForTimeout(3_000);
  await expect(detail).toBeVisible();
  await expect(detail).toHaveAttribute("data-msg", opened ?? "");
  await expect(detail.getByTestId("feed-field-lat")).toHaveCount(1);

  // --- Received: senders and fates -----------------------------------------------------------------
  await page.getByTestId("feed-tab-received").click();
  await expect(page.getByTestId("feed-row-received").first()).toBeVisible({ timeout: 30_000 });
  const from = (await page.getByTestId("feed-from").first().textContent()) ?? "";
  expect(from).toMatch(/^node \d+$/);
  await expect(page.locator('[data-testid="feed-row-received"][data-outcome="delivered"]').first()).toBeVisible();
  await page.locator('[data-testid="feed-row-received"][data-outcome="delivered"]').first().click();
  await expect(detail.getByTestId("feed-fate")).toContainText("delivered");
  await expect(detail.getByTestId("feed-field-temp_id")).toHaveCount(1);

  // --- Queues: the five, and messages waiting in them ------------------------------------------------
  await page.getByTestId("feed-tab-queues").click();
  for (const q of ["rx", "verify", "app", "tx", "crl"]) await expect(page.getByTestId(`queue-${q}`)).toBeVisible();
  await expect.poll(async () => page.getByTestId("queue-entry").count(), { timeout: 60_000, intervals: [200] }).toBeGreaterThan(0);
  const entry = (await page.getByTestId("queue-entry").first().textContent()) ?? "";
  expect(entry).toMatch(/ms/);

  // Pause and resume the panel itself.
  await page.getByTestId("feed-tab-sent").click();
  await page.getByTestId("feed-pause").click();
  const frozen = await page.getByTestId("feed-row-sent").first().getAttribute("data-msg");
  await page.waitForTimeout(1_500);
  expect(await page.getByTestId("feed-row-sent").first().getAttribute("data-msg")).toBe(frozen);
  await expect(page.getByTestId("feed-pause")).toContainText("Resume");
  await page.getByTestId("feed-pause").click();
  await expect.poll(async () => page.getByTestId("feed-row-sent").first().getAttribute("data-msg"), { timeout: 10_000 }).not.toBe(frozen);
});
