/**
 * The scenario timeline, end to end in the page: an event added in the settings panel reaches
 * the next run, is drawn on the time bar, fires, and changes what the traffic does.
 *
 * The check is a road closure, because it is the one whose effect is visible in the stream
 * itself: every actor row carries its lane (§3.3.2, full profile), so the page can count, frame
 * by frame, the vehicles that drive onto a street. The first run, without the closure, finds the
 * busiest street after the closure instant — so the check below can fail — and the second run,
 * with the closure added through the editor, must have nobody drive onto it after it closes.
 *
 * The stream is paced to the page (`run.speed {sync: "client"}`) so no frame is shed under
 * backpressure: a lane change that fell into a dropped delta would otherwise go uncounted.
 */

import { expect, test, type Page } from "@playwright/test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { EngineProcess, REPO, open, runToEnd, status } from "./support.js";

const CLOSE_AT_S = 6;

function scenario(): string {
  const text = readFileSync(join(REPO, "scenarios/phase1-grid.yaml"), "utf8")
    .replace("name: phase1-grid", "name: e2e-events")
    .replace("duration_s: 60.0", "duration_s: 30.0")
    .replace("rate_veh_per_h: 30.0", "rate_veh_per_h: 7200.0");
  const dir = join(tmpdir(), `vwp-engine-events-${process.pid}`);
  mkdirSync(dir, { recursive: true });
  const path = join(dir, "e2e-events.yaml");
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

/** Starts recording every lane change the page sees, with the street each lane belongs to. */
async function recordLaneChanges(page: Page): Promise<void> {
  await page.evaluate(() => {
    const studio = window.__vwpStudio?.engine as unknown as {
      client: {
        poses: { count: number; occupied: Uint8Array; actorId: Uint32Array; laneId: Uint32Array; simTimeNs: bigint };
        onDelta(f: () => void): () => void;
        onKeyframe(f: () => void): () => void;
      };
      world: { lanes: { count: number; laneId: Uint32Array; edgeId: Uint32Array; junctionId: Uint32Array; laneType: Uint8Array } };
    };
    const lanes = studio.world.lanes;
    const edgeOf = new Map<number, number>();
    for (let i = 0; i < lanes.count; i++) {
      // Street lanes that carry traffic: not inside a junction, not a footway or a crossing.
      if (lanes.junctionId[i] !== 0xffffffff || lanes.laneType[i] === 2 || lanes.laneType[i] === 6) continue;
      edgeOf.set(lanes.laneId[i], lanes.edgeId[i]);
    }
    const last = new Map<number, number>();
    const log: { t: number; edge: number }[] = [];
    const sample = (): void => {
      const p = studio.client.poses;
      const t = Number(p.simTimeNs) / 1e9;
      for (let s = 0; s < p.count; s++) {
        if (p.occupied[s] !== 1) continue;
        const actor = p.actorId[s];
        const lane = p.laneId[s];
        const before = last.get(actor);
        last.set(actor, lane);
        if (before === undefined || before === lane) continue;
        const edge = edgeOf.get(lane);
        if (edge !== undefined && edgeOf.get(before) !== edge) log.push({ t, edge });
      }
    };
    const w = window as unknown as { __laneLog?: typeof log; __laneOff?: () => void };
    w.__laneOff?.();
    const offA = studio.client.onDelta(sample);
    const offB = studio.client.onKeyframe(sample);
    w.__laneOff = () => {
      offA();
      offB();
    };
    w.__laneLog = log;
  });
}

async function entriesAfter(page: Page, t: number): Promise<Map<number, number>> {
  const log = await page.evaluate(() => (window as unknown as { __laneLog: { t: number; edge: number }[] }).__laneLog);
  const out = new Map<number, number>();
  for (const e of log) if (e.t > t) out.set(e.edge, (out.get(e.edge) ?? 0) + 1);
  return out;
}

test("a closure added in the settings reaches the next run and traffic avoids the road", async ({ page }) => {
  await open(page);
  // Lossless pacing: the engine waits for the page rather than shedding frames.
  await page.evaluate(async () => {
    const studio = window.__vwpStudio?.engine as unknown as { request(m: string, p: unknown): Promise<unknown> };
    await studio.request("run.speed", { speed: 0, sync: "client" });
  });

  // --- run 1: no closure. Which street is busiest after the closure instant? ---------------------
  await recordLaneChanges(page);
  await runToEnd(page);
  const baseline = await entriesAfter(page, CLOSE_AT_S);
  const [edge, before] = [...baseline.entries()].sort((a, b) => b[1] - a[1] || a[0] - b[0])[0] ?? [NaN, 0];
  expect(before, "the busiest street carries traffic without a closure, so the check can fail").toBeGreaterThanOrEqual(3);

  // --- add the closure in the editor ------------------------------------------------------------
  const timeline = page.getByTestId("events-editor");
  await timeline.scrollIntoViewIfNeeded();
  await page.getByTestId("event-add").selectOption("closure");
  const row = page.getByTestId("event-row").last();
  await row.getByTestId("event-t").fill(String(CLOSE_AT_S));
  // "pick on map" fills the target from a click; the test then names the street it wants.
  await row.getByTestId("event-pick").click();
  const canvas = page.getByTestId("viewer-canvas");
  const box = await canvas.boundingBox();
  if (box) await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await expect(row.getByTestId("event-target")).toHaveValue(/^edge:\d+$/);
  await row.getByTestId("event-target").fill(`edge:${edge}`);
  await page.getByTestId("apply").click();
  await expect(page.getByTestId("scenario-message")).toContainText("Applied");

  // --- run 2: with the closure ----------------------------------------------------------------------
  await recordLaneChanges(page);
  const done = await runToEnd(page);
  const after = await entriesAfter(page, CLOSE_AT_S);
  expect(after.get(edge) ?? 0, `vehicles drove onto the closed street (edge ${edge}); ${before} did without the closure`).toBe(0);
  // Traffic kept flowing elsewhere.
  const elsewhere = [...after.values()].reduce((a, b) => a + b, 0);
  expect(elsewhere).toBeGreaterThan(before);

  // The time bar drew it, and it fired, saying what it did.
  const mark = page.locator('[data-testid="scenario-event-mark"][data-kind="closure"]');
  await expect(mark).toHaveCount(1);
  await expect(mark).toHaveAttribute("data-fired", "true");
  await expect(page.getByTestId("event-fired").first()).toContainText("lanes closed");
  const fired = (done.engine as unknown as { timeline: { kind: string; lanes: number[]; effect: string }[] }).timeline;
  expect(fired.map((e) => e.kind)).toEqual(["closure"]);
  console.log(
    `closure of edge ${edge}: ${before} vehicles drove onto it after ${CLOSE_AT_S} s without the closure, ` +
      `${after.get(edge) ?? 0} with it; ${elsewhere} entries elsewhere. Engine: ${fired[0].effect}`,
  );

  // The status line never showed an error.
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "streaming");
  const s = await status(page);
  expect(s.state).toBe("finished");
});
