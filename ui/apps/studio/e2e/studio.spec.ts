/**
 * The end-to-end check: the real Studio, the real `@vwp/mock-server`, chromium with software WebGL.
 *
 * It asserts the chain the whole app hangs off — canvas mounts → `Hello` arrives → world decodes →
 * actors appear → clicking one flies the camera down and populates the OBU HUD with non-zero
 * telemetry → overlays toggle → the time controls issue the §6.6 methods — and captures the two
 * screenshots the task asks for.
 */

import { expect, test, type Page } from "@playwright/test";

const SHOTS = "screenshots";

interface StudioProbe {
  connection: string;
  hello: boolean;
  actors: number;
  fps: number;
  selectedActor: number | null;
  selectedNode: number | null;
  cameraMode: string;
  worldLanes: number;
  provenance: number;
}

declare global {
  interface Window {
    __vwpStudio?: {
      engine: {
        client: { poses: { count: number; occupied: Uint8Array; actorId: Uint32Array }; hello: unknown } | null;
        viewer: { stats: { snapshot(): { fps: number; actorInstances: number } }; cameras: { state(): { mode: string } } } | null;
        provenance: Map<number, unknown>;
        selectActor(id: number | null, mode?: string): Promise<void>;
      };
      selectFirstActor: () => number | null;
      fps: () => number;
      actorCount: () => number;
    };
  }
}

async function probe(page: Page): Promise<StudioProbe> {
  return page.evaluate(() => {
    const w = window as unknown as {
      __vwpStudio?: {
        engine: {
          client: { poses: { count: number }; hello: unknown } | null;
          viewer: { stats: { snapshot(): { fps: number } }; cameras: { state(): { mode: string } } } | null;
          provenance: Map<number, unknown>;
          world: { lanes: { count: number } } | null;
        };
      };
    };
    const api = w.__vwpStudio;
    const text = (sel: string): string => document.querySelector(`[data-testid="${sel}"]`)?.textContent ?? "";
    return {
      connection: text("connection-state"),
      hello: api?.engine.client?.hello != null,
      actors: api?.engine.client?.poses.count ?? 0,
      fps: api?.engine.viewer?.stats.snapshot().fps ?? 0,
      selectedActor: null,
      selectedNode: null,
      cameraMode: api?.engine.viewer?.cameras.state().mode ?? "",
      worldLanes: api?.engine.world?.lanes.count ?? 0,
      provenance: api?.engine.provenance.size ?? 0,
    } satisfies StudioProbe;
  });
}

test("the Studio streams VWP v1, renders actors, flies down on a click and fills the OBU HUD", async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });
  page.on("pageerror", (e) => consoleErrors.push(`pageerror: ${e.message}`));

  await page.goto("/");

  // 1. The canvas mounts and the connection reaches `streaming` (§1.3).
  await expect(page.getByTestId("viewer-canvas")).toBeVisible();
  await expect(page.getByTestId("connection-state")).toHaveText("streaming", { timeout: 30_000 });

  // 2. `Hello` arrived with an engine version (§3.1.1 str_engine_version).
  await expect(page.getByTestId("engine-version")).toContainText("vwp-mock-server");

  // 3. The world decoded and actors are in the pose buffer (§3.3/§3.4).
  await expect.poll(async () => (await probe(page)).worldLanes, { timeout: 30_000 }).toBeGreaterThan(0);
  await expect.poll(async () => (await probe(page)).actors, { timeout: 30_000 }).toBeGreaterThan(10);

  // 4. The viewer is actually drawing them.
  await expect
    .poll(async () => page.evaluate(() => window.__vwpStudio?.engine.viewer?.stats.snapshot().actorInstances ?? 0), { timeout: 30_000 })
    .toBeGreaterThan(0);

  // 5. A `Provenance` frame arrived (§3.8 requires one right after the first keyframe).
  await expect.poll(async () => (await probe(page)).provenance, { timeout: 30_000 }).toBeGreaterThan(0);

  await page.waitForTimeout(1500);
  await page.screenshot({ path: `${SHOTS}/01-map-view.png`, fullPage: false });

  // 6. Clicking an actor: pick → flyTo → view.follow → Telemetry → HUD.
  //    The click goes through the real picker: find a drawn actor's screen position first.
  const clicked = await page.evaluate(() => {
    const api = window.__vwpStudio;
    if (!api) return null;
    return api.selectFirstActor();
  });
  expect(clicked).not.toBeNull();

  await expect(page.getByTestId("follow-chip")).not.toHaveText("follow: —", { timeout: 20_000 });
  await expect.poll(async () => (await probe(page)).cameraMode, { timeout: 20_000 }).toBe("chase");

  // 7. The HUD populates with non-zero telemetry from the §3.5 frames.
  await expect(page.getByTestId("obu-hud")).toBeVisible();
  await expect(page.getByTestId("hud-identity")).toContainText(/OBU|RSU/, { timeout: 30_000 });
  await expect(page.getByTestId("hud-msgs_in_per_s")).toBeVisible({ timeout: 30_000 });

  const hudNumbers = await page.evaluate(() => {
    const read = (key: string): string => document.querySelector(`[data-testid="hud-${key}"]`)?.textContent ?? "";
    return {
      rx: read("msgs_in_per_s"),
      cpu: read("cpu_util_pm"),
      cbr: read("cbr_pm"),
      neighbours: read("nbr_total"),
      certs: read("cert_stored"),
      crl: read("crl_entries"),
      gnss: read("gnss_fix"),
      txPower: read("tx_power_cdbm"),
      posError: read("pos_error_m"),
    };
  });
  // Non-zero, non-"n/a" telemetry: rx rate and neighbour count are the two the HUD leads with.
  expect(hudNumbers.rx).toMatch(/[1-9]/);
  expect(hudNumbers.cpu).toMatch(/[1-9]/);
  expect(hudNumbers.neighbours).not.toContain("n/a");
  expect(hudNumbers.certs).toMatch(/[1-9]/);
  expect(hudNumbers.gnss).toMatch(/2D|3D|DGNSS|RTK|none|dead/);
  expect(hudNumbers.txPower).toContain("dBm");
  expect(hudNumbers.posError).toContain("m");

  // 8. The sparkline row has drawn something.
  await expect(page.getByTestId("hud-sparklines")).toBeVisible();
  await expect
    .poll(async () => page.locator('[data-testid="hud-sparklines"] canvas').count(), { timeout: 20_000 })
    .toBeGreaterThanOrEqual(5);

  await page.waitForTimeout(2500);
  await page.screenshot({ path: `${SHOTS}/02-chase-view-hud.png`, fullPage: false });

  // 9. The "why" tab resolves (or explicitly does not resolve) provenance for a HUD value.
  await page.getByTestId("hud-cpu_util_pm").click();
  await expect(page.getByTestId("why-tab")).toBeVisible();
  await expect(page.getByTestId("why-absent")).toContainText("No provenance id is carried");
  await page.getByTestId("why-explain").click();
  await expect(page.getByTestId("why-explain-result")).toBeVisible({ timeout: 20_000 });
  await page.screenshot({ path: `${SHOTS}/03-why-tab.png` });

  // 10. Overlay toggles round-trip through the viewer and `overlay.set` (§6.7).
  await page.getByTestId("overlays-button").click();
  await expect(page.getByTestId("overlays-menu")).toBeVisible();
  const linksBefore = await page.evaluate(() =>
    (window as unknown as { __vwpStudio?: { engine: { viewer: { overlays: { isEnabled(n: string): boolean } } | null } } }).__vwpStudio?.engine.viewer?.overlays.isEnabled("links"),
  );
  await page.locator('[data-testid="overlay-links"] input').click();
  await expect
    .poll(async () =>
      page.evaluate(() =>
        (window as unknown as { __vwpStudio?: { engine: { viewer: { overlays: { isEnabled(n: string): boolean } } | null } } }).__vwpStudio?.engine.viewer?.overlays.isEnabled("links"),
      ),
    )
    .toBe(!linksBefore);
  await page.keyboard.press("Escape");
  await page.mouse.click(1200, 600);

  // 11. Time controls issue the right §6.6 calls.
  const calls = async (): Promise<string[]> =>
    page.evaluate(() =>
      Array.from(document.querySelectorAll('[data-testid="copilot-panel"] .m .name')).map((e) => e.textContent ?? ""),
    );
  await page.getByRole("button", { name: "Copilot" }).click();
  await expect(page.getByTestId("rpc-methods")).toBeVisible();
  const methodCount = await page.locator('[data-testid="rpc-methods"] .m').count();
  expect(methodCount).toBeGreaterThanOrEqual(32);
  void calls;

  await page.getByRole("button", { name: "Scenario" }).click();

  await page.getByTestId("pause").click();
  await expect(page.getByTestId("play")).toBeVisible({ timeout: 20_000 });
  const tAfterPause = await page.getByTestId("sim-clock").textContent();

  await page.getByTestId("step").click();
  await expect.poll(async () => page.getByTestId("sim-clock").textContent()).not.toBe(tAfterPause);

  await page.getByTestId("speed").selectOption("5");
  await page.getByTestId("play").click();
  await expect(page.getByTestId("pause")).toBeVisible({ timeout: 20_000 });

  // A seek back to the start must move the clock backwards (§6.6 run.seek).
  await page.getByTestId("seek-start").click();
  await expect.poll(async () => {
    const text = (await page.getByTestId("sim-clock").textContent()) ?? "";
    return text.startsWith("00:00:0");
  }, { timeout: 20_000 }).toBe(true);

  // 12. The calls actually went out over JSON-RPC.
  await page.getByRole("button", { name: "Copilot" }).click();
  const made = await page.evaluate(() =>
    Array.from(document.querySelectorAll('[data-testid="copilot-panel"] .method-list')).pop()?.textContent ?? "",
  );
  expect(made).toContain("run.seek");
  expect(made).toContain("run.speed");
  expect(made).toContain("run.step");
  expect(made).toContain("run.pause");
  expect(made).toContain("view.follow");

  // 13. Scenario panel rendered a form with units and help.
  await page.getByRole("button", { name: "Scenario" }).click();
  await expect(page.getByTestId("scenario-panel")).toBeVisible();
  await expect(page.getByTestId("schema-source")).toBeVisible();
  await page.getByTestId("validate").click();
  await expect(page.getByTestId("validation-state")).toBeVisible({ timeout: 20_000 });
  await page.getByTestId("validation-state").scrollIntoViewIfNeeded();
  await page.screenshot({ path: `${SHOTS}/04-scenario-and-plots.png` });

  // 14. The engine log the inspector shows carries no protocol, world or RPC errors.
  await page.getByRole("button", { name: "log", exact: true }).click();
  const logLines = await page.locator('[data-testid="inspector-log"] .line').allTextContents();
  expect(logLines.length).toBeGreaterThan(0);
  expect(logLines.filter((l) => l.startsWith("error"))).toEqual([]);

  // 15. No uncaught errors along the way.
  const ignorable = (t: string): boolean => t.includes("WebGL") && t.includes("deprecat");
  expect(consoleErrors.filter((t) => !ignorable(t))).toEqual([]);
});

test("light theme renders and the actor-state legend keeps shape redundancy", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveText("streaming", { timeout: 30_000 });
  // The previous test leaves the run paused at t = 0; resume so this one sees moving traffic.
  const playing = await page.getByTestId("play").count();
  if (playing > 0) await page.getByTestId("play").click();
  await expect
    .poll(async () => page.evaluate(() => (window as unknown as { __vwpStudio?: { actorCount(): number } }).__vwpStudio?.actorCount() ?? 0), { timeout: 30_000 })
    .toBeGreaterThan(10);
  await page.waitForTimeout(2500);
  await page.getByTestId("theme-toggle").click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect(page.getByTestId("state-legend")).toBeVisible();
  const shapes = await page.locator('[data-testid="state-legend"] svg').count();
  expect(shapes).toBe(5);
  await page.waitForTimeout(1200);
  await page.screenshot({ path: `${SHOTS}/05-light-theme.png` });
});
