import { defineConfig, devices } from "@playwright/test";

/**
 * End-to-end against the **real engine** (`crates/v2xw-server`), not the mock.
 *
 * `playwright.config.ts` drives the Studio against `@vwp/mock-server`, which is right for the
 * rendering and projection checks and says nothing about whether the simulator works: the mock
 * has no scenario to edit, no kernel to stop and no run to start again. The owner's reports —
 * applied settings doing nothing, a run that would not start after a few — are all about the real
 * engine, so this suite runs the page against it.
 *
 * Only the page is a `webServer`. The engine is started and killed by the tests themselves
 * (`e2e-engine/support.ts`), because one of them kills it with the page open and starts it again.
 *
 *   VWP_ENGINE_BIN   the `v2xw-server` binary (default: <repo>/target/debug/v2xw-server)
 *   VWP_SOAK_RUNS    how many runs the soak drives (default 4; the long soak is 30)
 *   VWP_ENGINE_PORT, VWP_STUDIO_PORT   ports (defaults 8789 / 5175)
 *
 *   pnpm exec playwright test -c playwright.engine.config.ts
 */
const enginePort = process.env.VWP_ENGINE_PORT ?? "8789";
const studioPort = process.env.VWP_STUDIO_PORT ?? "5175";

export default defineConfig({
  testDir: "./e2e-engine",
  timeout: 30 * 60_000,
  expect: { timeout: 30_000 },
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${studioPort}`,
    viewport: { width: 1500, height: 950 },
    trace: "off",
    video: "off",
    // A control that never becomes clickable is a failure to report, not a reason to wait out
    // the whole test's budget.
    actionTimeout: 30_000,
    screenshot: "only-on-failure",
    launchOptions: {
      headless: process.env.VWP_HEADED !== "1",
      args: [
        "--use-gl=angle",
        "--use-angle=swiftshader",
        "--enable-unsafe-swiftshader",
        "--ignore-gpu-blocklist",
        // The soak measures the tab's heap; these make `performance.memory` exact and let the
        // test collect garbage before each reading, so growth is growth and not a lazy GC.
        "--enable-precise-memory-info",
        "--js-flags=--expose-gc",
      ],
    },
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1500, height: 950 } } }],
  webServer: [
    {
      command: `pnpm exec vite --port ${studioPort} --strictPort`,
      url: `http://127.0.0.1:${studioPort}`,
      reuseExistingServer: false,
      stdout: "ignore",
      stderr: "pipe",
      timeout: 120_000,
      env: { VWP_ENGINE: `http://127.0.0.1:${enginePort}`, VWP_STUDIO_PORT: studioPort },
    },
  ],
});
