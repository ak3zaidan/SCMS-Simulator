import { defineConfig, devices } from "@playwright/test";

/**
 * End-to-end configuration: chromium only, driving the real Studio against the real mock engine.
 *
 * Two servers come up for every run — the VWP mock engine (`@vwp/mock-server`) and the Vite dev
 * server that proxies `/vwp/v1`, `/rpc` and `/world` onto it. `VWP_ACTORS` picks the traffic size,
 * which is how the 200-actor and 5,000-actor frame-rate measurements are taken.
 */
const actors = process.env.VWP_ACTORS ?? "200";
// Deliberately not 8787/5173: a test run must not collide with a dev session on the default ports.
const enginePort = process.env.VWP_ENGINE_PORT ?? "8788";
const studioPort = process.env.VWP_STUDIO_PORT ?? "5174";

export default defineConfig({
  testDir: "./e2e",
  timeout: 120_000,
  expect: { timeout: 30_000 },
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${studioPort}`,
    viewport: { width: 1600, height: 1000 },
    trace: "off",
    video: "off",
    launchOptions: {
      // Headless chromium has no GPU here, so WebGL runs on SwiftShader — correct, but a software
      // rasteriser. `VWP_HEADED=1` launches a real window instead, which uses the machine's GPU
      // through ANGLE/Metal and is the only way to get a presentation rate worth quoting.
      headless: process.env.VWP_HEADED !== "1",
      args:
        process.env.VWP_HEADED === "1"
          ? ["--disable-gpu-vsync", "--disable-frame-rate-limit"]
          : ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"],
    },
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1680, height: 1050 } } }],
  webServer: [
    {
      command: `node ../../packages/mock-server/dist/index.js --actors ${actors} --port ${enginePort} --quiet`,
      url: `http://127.0.0.1:${enginePort}/healthz`,
      reuseExistingServer: false,
      stdout: "ignore",
      stderr: "pipe",
      timeout: 120_000,
    },
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
