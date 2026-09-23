/**
 * Regression tests for the Studio findings that only a real browser can prove
 * (ui-review-register Q3, Q14, Q15, Q16).
 *
 * The projection-layer findings (Q4, Q9, Q10, Q12) are covered by the Node suite in `test/`, which
 * can count store notifications exactly. These four need the real DOM: React's synthetic `onChange`
 * on an `<input type="range">`, native pointer dragging, the tab order, and `:focus-visible` — none
 * of which survive being simulated.
 *
 * The run is left running at t = 0, because `studio.spec.ts` (which sorts after this file) starts by
 * clicking the pause button.
 */

import { expect, test, type Page } from "@playwright/test";

interface Probe {
  seeks: number;
  inputs: number;
  statusPolls: number;
  methods: string[];
}

declare global {
  interface Window {
    __vwpProbe?: Probe;
  }
}

/**
 * Wrap `engine.request` so every §6 call made from the UI is counted.
 *
 * `StudioEngine.request` is a prototype method, so an own property shadows it for the singleton the
 * components import — nothing in the app is rebuilt and no production code is touched.
 */
async function instrument(page: Page): Promise<void> {
  await page.evaluate(() => {
    const api = window.__vwpStudio;
    if (!api) throw new Error("__vwpStudio is not exposed");
    const engine = api.engine as unknown as { request(method: string, params: unknown): Promise<unknown> };
    const original = engine.request.bind(engine);
    const probe: Probe = { seeks: 0, inputs: 0, statusPolls: 0, methods: [] };
    window.__vwpProbe = probe;
    (engine as unknown as Record<string, unknown>).request = (method: string, params: unknown) => {
      probe.methods.push(method);
      if (method === "run.seek") probe.seeks++;
      if (method === "run.status") probe.statusPolls++;
      return original(method, params);
    };
    document
      .querySelector('[data-testid="scrub-range"]')
      ?.addEventListener("input", () => {
        probe.inputs++;
      });
  });
}

async function streaming(page: Page): Promise<void> {
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "streaming", { timeout: 60_000 });
  await expect(page.getByTestId("scrub-range")).toBeEnabled({ timeout: 30_000 });
  // Each test starts from a running run at t = 0, whatever the previous one left behind: a seek
  // with `pause_after` stops the stream, and a stopped stream sends no Telemetry.
  await page.evaluate(async () => {
    const engine = window.__vwpStudio?.engine as unknown as
      | { request(method: string, params: unknown): Promise<unknown> }
      | undefined;
    await engine?.request("run.resume", {}).catch(() => undefined);
  });
  await expect
    .poll(async () => page.evaluate(() => window.__vwpStudio?.actorCount() ?? 0), { timeout: 60_000 })
    .toBeGreaterThan(10);
}

/**
 * Follow a drawn actor that is backed by a `Hello` node, and wait for the HUD to fill.
 *
 * `__vwpStudio.selectFirstActor()` takes the first occupied pose slot, which is not necessarily an
 * equipped node — and an actor with no node id gets no `Telemetry` subscription (§6.7), so the HUD
 * would stay on its placeholder. The node table decides here instead.
 */
async function followFirstActor(page: Page): Promise<void> {
  const clicked = await page.evaluate(() => {
    const engine = window.__vwpStudio?.engine as unknown as
      | {
          client: { poses: { count: number; occupied: Uint8Array; actorId: Uint32Array } } | null;
          nodeByActor: Map<number, number>;
          selectActor(id: number | null, mode?: string): Promise<void>;
        }
      | undefined;
    const poses = engine?.client?.poses;
    if (!engine || !poses) return null;
    for (let slot = 0; slot < poses.count; slot++) {
      if (poses.occupied[slot] !== 1) continue;
      const id = poses.actorId[slot];
      if (engine.nodeByActor.get(id) === undefined) continue;
      void engine.selectActor(id, "chase");
      return id;
    }
    return null;
  });
  expect(clicked, "no drawn actor is backed by a Hello node").not.toBeNull();
  await expect(page.getByTestId("hud-identity")).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId("hud-cpu_util_pm")).toBeVisible({ timeout: 60_000 });
}

/** Put the mock engine back the way `studio.spec.ts` expects to find it. */
async function restoreRun(page: Page): Promise<void> {
  await page.evaluate(async () => {
    const engine = window.__vwpStudio?.engine as unknown as
      | { request(method: string, params: unknown): Promise<unknown> }
      | undefined;
    if (!engine) return;
    await engine.request("run.seek", { t_ns: 0, pause_after: false }).catch(() => undefined);
    await engine.request("run.speed", { speed: 1 }).catch(() => undefined);
    await engine.request("run.resume", {}).catch(() => undefined);
  });
}

test.afterEach(async ({ page }) => {
  await restoreRun(page).catch(() => undefined);
});

test("Q3 — dragging the scrub bar across thousands of steps issues one run.seek", async ({ page }) => {
  await streaming(page);
  await instrument(page);

  const scrub = page.getByTestId("scrub-range");
  const box = await scrub.boundingBox();
  expect(box).not.toBeNull();
  if (!box) return;

  const y = box.y + box.height / 2;
  const x0 = box.x + box.width * 0.06;
  const x1 = box.x + box.width * 0.82;
  const clockBefore = await page.getByTestId("sim-clock").textContent();

  await page.mouse.move(x0, y);
  await page.mouse.down();
  // 120 discrete moves across the track. The mock run is 3,600 s at a 0.1 s mobility step, so the
  // thumb crosses ~36,000 steps end to end and every one of these moves lands on a new step.
  const steps = 120;
  let clockMovedMidDrag = false;
  for (let i = 1; i <= steps; i++) {
    await page.mouse.move(x0 + ((x1 - x0) * i) / steps, y);
    if (i === Math.floor(steps / 2)) {
      // The display must follow the thumb while the gesture is still in flight.
      const clockDuring = await page.getByTestId("sim-clock").textContent();
      clockMovedMidDrag = clockDuring !== clockBefore;
      const seeksSoFar = await page.evaluate(() => window.__vwpProbe?.seeks ?? -1);
      expect(seeksSoFar, "no seek may be issued before the thumb is released").toBe(0);
    }
  }
  await page.mouse.up();

  await expect.poll(async () => page.evaluate(() => window.__vwpProbe?.seeks ?? 0), { timeout: 20_000 }).toBe(1);

  const probe = await page.evaluate(() => window.__vwpProbe);
  expect(probe).toBeTruthy();
  // The DOM `input` event is what React reports as `onChange`; one per step crossed is exactly the
  // rate the old handler turned into `run.seek` calls.
  expect(probe!.inputs, "the drag must actually cross many steps").toBeGreaterThan(50);
  expect(probe!.seeks).toBe(1);
  // `TimeControls.call()` polls `run.status` after every request; one seek means one poll.
  expect(probe!.statusPolls).toBeLessThanOrEqual(2);
  expect(clockMovedMidDrag, "the clock must track the thumb during the drag").toBe(true);

  // The seek actually happened: the clock is no longer where it started.
  await expect.poll(async () => page.getByTestId("sim-clock").textContent(), { timeout: 20_000 }).not.toBe(clockBefore);

});

test("Q3 — arrow keys on the scrub bar commit once per gesture, not once per repeat", async ({ page }) => {
  await streaming(page);
  await instrument(page);

  const scrub = page.getByTestId("scrub-range");
  await scrub.focus();
  const before = await scrub.inputValue();

  await page.keyboard.down("ArrowRight");
  // The key is still down: the thumb has moved but nothing has been committed.
  expect(await scrub.inputValue()).not.toBe(before);
  expect(await page.evaluate(() => window.__vwpProbe?.seeks ?? -1)).toBe(0);

  await page.keyboard.up("ArrowRight");
  await expect.poll(async () => page.evaluate(() => window.__vwpProbe?.seeks ?? 0), { timeout: 20_000 }).toBe(1);

  // A second, separate press is a second gesture and so a second seek — that is correct.
  await page.keyboard.press("ArrowRight");
  await expect.poll(async () => page.evaluate(() => window.__vwpProbe?.seeks ?? 0), { timeout: 20_000 }).toBe(2);
});

test("Q16/Q15 — every HUD value is a keyboard-reachable button with a visible focus ring", async ({ page }) => {
  await streaming(page);

  // The HUD only populates for a followed node (§6.7 `view.follow` is the Telemetry subscription).
  await followFirstActor(page);

  // 1. Every clickable value is a real button, named, described and in the tab order.
  //    Counted, not estimated: the HUD renders 37 `Value` components (6 + 6 + 8 + 8 + 6 + 3 across
  //    its six rows — the register's "around 45" was an estimate) plus exactly three `.hud-field`
  //    spans that are genuinely not interactive and have no click handler: "dropped", "evidence
  //    buffer" and "window".
  const shape = await page.evaluate(() => {
    const fields = Array.from(document.querySelectorAll('[data-testid="obu-hud"] .hud-field'));
    const clickable = fields.filter((e) => e.tagName === "BUTTON");
    return {
      total: fields.length,
      buttons: clickable.length,
      spans: fields.filter((e) => e.tagName !== "BUTTON").length,
      labelled: clickable.filter((e) => (e.getAttribute("aria-label") ?? "") !== "").length,
      described: clickable.filter((e) => {
        const id = e.getAttribute("aria-describedby");
        return id !== null && document.getElementById(id) !== null;
      }).length,
      tabbable: clickable.filter((e) => (e as HTMLButtonElement).tabIndex >= 0).length,
      testids: clickable.filter((e) => (e.getAttribute("data-testid") ?? "").startsWith("hud-")).length,
    };
  });
  expect(shape.buttons).toBe(37);
  expect(shape.spans).toBe(3);
  expect(shape.labelled).toBe(37);
  expect(shape.described).toBe(37);
  expect(shape.tabbable).toBe(37);
  expect(shape.testids).toBe(37);

  // 2. Tab reaches the next one, which is what "in the tab order" means.
  await page.getByTestId("hud-msgs_in_per_s").focus();
  await page.keyboard.press("Tab");
  const afterTab = await page.evaluate(() => document.activeElement?.getAttribute("data-testid") ?? "");
  expect(afterTab).toBe("hud-msgs_out_per_s");

  // 3. The focus ring is actually painted (WCAG 2.1 SC 2.4.7).
  const outline = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el) return null;
    const cs = getComputedStyle(el);
    return { style: cs.outlineStyle, width: cs.outlineWidth, matches: el.matches(":focus-visible") };
  });
  expect(outline).not.toBeNull();
  expect(outline!.matches).toBe(true);
  expect(outline!.style).not.toBe("none");
  expect(Number.parseFloat(outline!.width)).toBeGreaterThanOrEqual(2);

  // 4. The keyboard activates it, and that is what opens the "why" tab the explainability story
  //    depends on (09-ui §7).
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("why-tab")).toBeVisible({ timeout: 10_000 });
  await expect(page.getByTestId("why-value")).toContainText("tx =");

  // 5. Space works too, on a different field, and re-targets the tab.
  await page.getByTestId("hud-cpu_util_pm").focus();
  await page.keyboard.press("Space");
  await expect(page.getByTestId("why-value")).toContainText("CPU =", { timeout: 10_000 });

});

test("Q15 — the inspector's field labels keep a visible focus indicator", async ({ page }) => {
  await streaming(page);
  await followFirstActor(page);

  await page.getByRole("button", { name: "state", exact: true }).click();
  await expect(page.getByTestId("state-field-cpu_util_pm")).toBeVisible({ timeout: 30_000 });

  // No inline style may compete with the stylesheet's :focus-visible rule.
  const inline = await page.evaluate(
    () => document.querySelector('[data-testid="state-field-cpu_util_pm"]')?.getAttribute("style") ?? "",
  );
  expect(inline).toBe("");

  // Shift+Tab from one field label must land on the previous one — the DOM order of the state tab
  // follows `hudGroups`, not the HUD's own row order, so the assertion is on the shape of the id.
  await page.getByTestId("state-field-ram_used_kib").focus();
  await page.keyboard.press("Shift+Tab");
  const focused = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el) return null;
    const cs = getComputedStyle(el);
    return {
      testid: el.getAttribute("data-testid") ?? "",
      style: cs.outlineStyle,
      width: cs.outlineWidth,
      matches: el.matches(":focus-visible"),
    };
  });
  expect(focused).not.toBeNull();
  expect(focused!.testid).toMatch(/^state-field-/);
  expect(focused!.matches).toBe(true);
  expect(focused!.style).not.toBe("none");
  expect(Number.parseFloat(focused!.width)).toBeGreaterThanOrEqual(2);

  await page.keyboard.press("Enter");
  await expect(page.getByTestId("why-tab")).toBeVisible({ timeout: 10_000 });

});

test("Q14 — the marker overlay's shape channels are on from the first frame", async ({ page }) => {
  await streaming(page);
  // No overlay menu was touched: this is the state the app opens in.
  const state = await page.evaluate(() => {
    const viewer = window.__vwpStudio?.engine.viewer as unknown as
      | {
          overlays: {
            isEnabled(n: string): boolean;
            markers: { isChannelEnabled(c: string): boolean };
          };
        }
      | null
      | undefined;
    if (!viewer) return null;
    return {
      reported: viewer.overlays.isEnabled("reported"),
      revoked: viewer.overlays.isEnabled("revoked"),
      detections: viewer.overlays.isEnabled("detections"),
      attackersGt: viewer.overlays.isEnabled("attackers_gt"),
      chReported: viewer.overlays.markers.isChannelEnabled("reported"),
      chRevoked: viewer.overlays.markers.isChannelEnabled("revoked"),
      chDetection: viewer.overlays.markers.isChannelEnabled("detection"),
    };
  });
  expect(state).not.toBeNull();
  // 09-ui §10: "colour-blind-safe categorical palette for actor states … with shape redundancy".
  expect(state!.reported).toBe(true);
  expect(state!.revoked).toBe(true);
  expect(state!.detections).toBe(true);
  expect(state!.chReported).toBe(true);
  expect(state!.chRevoked).toBe(true);
  expect(state!.chDetection).toBe(true);
  // Ground truth stays off so `lockGroundTruth` (blind evaluation, 09-ui §6) still means something.
  expect(state!.attackersGt).toBe(false);

});
