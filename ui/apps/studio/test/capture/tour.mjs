#!/usr/bin/env node
/**
 * The standard tour: six pictures of the simulator that a human can judge in ten seconds.
 *
 *   node test/capture/tour.mjs            # from ui/apps/studio
 *
 * It brings up its own engine and its own dev server so the pictures are reproducible — the mock
 * engine at a fixed seed, a fixed window size, and a fixed sequence of moves:
 *
 *   1. aerial            the view the app opens on
 *   2. aerial-framed     the same altitude, framed on the traffic (the `f` key)
 *   3. fly-down          mid-flight, one second after the click
 *   4. chase             behind the vehicle, settled, as the app ships it
 *   5. chase-hud-docked  the same frame with the HUD moved out of the viewport
 *   6. dashboard         from the driver's seat
 *   7. back-out          returned to the map with the follow kept
 *
 * And, beside the pictures, the numbers the rendering track is held to (`motion` in tour.json):
 *
 *   - the followed vehicle's frame-to-frame jitter in chase, measured on the real page over three
 *     seconds: the second difference of its screen position (px) and of its heading (degrees);
 *   - signal heads that disagree with the state the stream says held at the drawn instant;
 *   - vehicle samples drawn inside a building footprint, split into the viewer's share and the
 *     engine's (a stream pose already inside it);
 *   - the camera's near plane and the view insets the HUD imposes.
 *
 * Stops 4 and 5 are the pair that settles an argument. The chase camera can be geometrically
 * perfect and the picture can still have no car in it, because the floating OBU HUD is anchored to
 * the bottom of the viewport and a chase camera puts the car it follows just below the centre line.
 * `subjectCoveredBy` in the table says which of the two is happening.
 *
 * Beside each picture it records the numbers that decide whether the picture is right: how many
 * vehicles are live and how many are drawn, where the camera is, where the followed vehicle lands
 * in the frame, and what is on top of it in the DOM. A picture that looks wrong and numbers that
 * say why, in the same artefact.
 *
 * Images are JPEG at quality 62 and 1280 x 800, which is about 90 kB each: this disk has filled to
 * zero twice, and the whole tour has to stay well under a megabyte. The output directory is emptied
 * at the start of every run, so the tour never accumulates.
 *
 * Options
 *   --out <dir>      where to write (default screenshots/tour)
 *   --seed <n>       engine seed (default 20260918)
 *   --actors <n>     traffic size (default 200)
 *   --width/--height window size (default 1280 x 800)
 *   --engine-port    default 8791
 *   --studio-port    default 5177
 *   --url <url>      drive an already-running Studio instead of starting one
 *   --keep           leave the servers running afterwards
 */

import { spawn } from "node:child_process";
import { mkdir, rm, writeFile, readdir, stat } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "@playwright/test";

const HERE = dirname(fileURLToPath(import.meta.url));
const STUDIO = resolve(HERE, "../..");
const UI = resolve(STUDIO, "../..");

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
}
const flag = (name) => process.argv.includes(`--${name}`);

const OPTIONS = {
  out: resolve(STUDIO, arg("out", "screenshots/tour")),
  seed: arg("seed", "20260918"),
  actors: arg("actors", "200"),
  width: Number(arg("width", "1280")),
  height: Number(arg("height", "800")),
  enginePort: arg("engine-port", "8791"),
  studioPort: arg("studio-port", "5177"),
  url: arg("url", null),
  keep: flag("keep"),
};

const children = [];

function start(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: options.cwd,
    env: options.env ?? process.env,
    stdio: "ignore",
    detached: false,
  });
  children.push(child);
  return child;
}

async function waitFor(url, label, budgetMs = 90_000) {
  const deadline = Date.now() + budgetMs;
  for (;;) {
    try {
      const res = await fetch(url);
      if (res.ok) return;
    } catch {
      /* not up yet */
    }
    if (Date.now() > deadline) throw new Error(`${label} did not come up at ${url}`);
    await new Promise((r) => setTimeout(r, 400));
  }
}

function stopAll() {
  for (const child of children) {
    try {
      child.kill("SIGTERM");
    } catch {
      /* already gone */
    }
  }
}

// -----------------------------------------------------------------------------------------------
// In-page probes. Kept as plain functions passed to `page.evaluate`, so they are type-free but
// share their shape with `e2e/scene-validation.spec.ts`, which asserts on the same numbers.
// -----------------------------------------------------------------------------------------------

function readFacts() {
  const engine = window.__vwpStudio.engine;
  const v = engine.viewer;
  const s = v.cameras.state();
  const snap = v.stats.snapshot();
  const it = v.interpolator;
  let ndc = null;
  let topmost = null;
  const id = s.followActorId;
  if (id !== null) {
    let slot = -1;
    for (let i = 0; i < it.count; i++) {
      if (it.outOccupied[i] === 1 && it.outActorId[i] === id >>> 0) {
        slot = i;
        break;
      }
    }
    if (slot >= 0) {
      const cam = v.camera;
      cam.updateMatrixWorld();
      const mul = (m, p) => [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12] * p[3],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13] * p[3],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14] * p[3],
        m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15] * p[3],
      ];
      const world = [it.outPosition[slot * 3], it.outPosition[slot * 3 + 1], it.outPosition[slot * 3 + 2] + 0.7, 1];
      const clip = mul(cam.projectionMatrix.elements, mul(cam.matrixWorldInverse.elements, world));
      const w = clip[3] || 1e-6;
      ndc = [clip[0] / w, clip[1] / w];
      const canvas = document.querySelector('[data-testid="viewer-canvas"]');
      const box = canvas.getBoundingClientRect();
      const el = document.elementsFromPoint(
        box.left + ((ndc[0] + 1) / 2) * box.width,
        box.top + ((1 - ndc[1]) / 2) * box.height,
      )[0];
      const cls = el && typeof el.className === "string" ? String(el.className).split(" ")[0] : "";
      topmost = el ? el.tagName.toLowerCase() + (cls ? `.${cls}` : "") : "none";
    }
  }
  const round = (n) => Math.round(n * 10) / 10;
  return {
    clock: document.querySelector('[data-testid="sim-clock"]')?.textContent ?? null,
    mode: s.mode,
    follow: id,
    camera: [round(s.position.x), round(s.position.y), round(s.position.z)],
    target: [round(s.target.x), round(s.target.y), round(s.target.z)],
    live: v.actors.stats.live,
    drawn: v.actors.stats.drawn,
    culled: v.actors.stats.culled,
    buildings: v.worldRenderer.buildingsVisible,
    fps: Math.round(snap.fpsAverage),
    frameMs: round(snap.meanMs),
    drawCalls: snap.drawCalls,
    subjectNdc: ndc ? [Math.round(ndc[0] * 1000) / 1000, Math.round(ndc[1] * 1000) / 1000] : null,
    subjectCoveredBy: topmost,
    near: round(v.camera.near),
    insets: v.cameras.viewInsets ? `${Math.round(v.cameras.viewInsets.top)}/${Math.round(v.cameras.viewInsets.bottom)}` : null,
  };
}

/**
 * Three seconds of the followed vehicle as drawn: screen-position and heading jitter (second
 * differences, per frame), plus every live vehicle against the building footprints.
 */
async function measureMotion() {
  const engine = window.__vwpStudio.engine;
  const v = engine.viewer;
  const it = v.interpolator;
  const w = v.worldRenderer;
  const id = v.cameras.followActorId;
  const px = [];
  const hs = [];
  let samples = 0;
  let viewerInside = 0;
  let engineInside = 0;
  const inside = (x, y, z) => {
    const b = w.buildingIndexAt(x, y);
    return b >= 0 && b !== w.ghostBuilding && z < w.buildingTopOf(b);
  };
  const history = [];
  const snap = () => {
    const p = engine.client.poses;
    const m = new Map();
    for (let i = 0; i < p.count; i++) if (p.occupied[i] === 1) m.set(p.actorId[i], [p.positions[i * 3], p.positions[i * 3 + 1], p.positions[i * 3 + 2]]);
    history.push(m);
    if (history.length > 3) history.shift();
  };
  snap();
  const off = engine.client.onDelta(snap);
  const t0 = performance.now();
  while (performance.now() - t0 < 3000) {
    await new Promise((r) => requestAnimationFrame(r));
    for (let i = 0; i < it.count; i++) {
      if (it.outOccupied[i] !== 1) continue;
      const x = it.outPosition[i * 3];
      const y = it.outPosition[i * 3 + 1];
      const z = it.outPosition[i * 3 + 2];
      samples++;
      if (inside(x, y, z)) {
        const aid = it.outActorId[i];
        if (history.some((h) => { const q = h.get(aid); return q !== undefined && inside(q[0], q[1], q[2]); })) engineInside++;
        else viewerInside++;
      }
      if (id !== null && it.outActorId[i] === id >>> 0) {
        const cam = v.camera;
        const m1 = cam.matrixWorldInverse.elements;
        const m2 = cam.projectionMatrix.elements;
        const mul = (m, q) => [m[0] * q[0] + m[4] * q[1] + m[8] * q[2] + m[12] * q[3], m[1] * q[0] + m[5] * q[1] + m[9] * q[2] + m[13] * q[3], m[2] * q[0] + m[6] * q[1] + m[10] * q[2] + m[14] * q[3], m[3] * q[0] + m[7] * q[1] + m[11] * q[2] + m[15] * q[3]];
        const c = mul(m2, mul(m1, [x, y, z + 0.7, 1]));
        px.push([((c[0] / c[3] + 1) / 2) * v.size.width, ((1 - c[1] / c[3]) / 2) * v.size.height]);
        hs.push(it.outHeading[i]);
      }
    }
  }
  off();
  const d2 = [];
  for (let i = 2; i < px.length; i++) d2.push(Math.hypot(px[i][0] - 2 * px[i - 1][0] + px[i - 2][0], px[i][1] - 2 * px[i - 1][1] + px[i - 2][1]));
  const wrap = (d) => d - Math.floor(d / (2 * Math.PI) + 0.5) * 2 * Math.PI;
  const yaw = [];
  for (let i = 2; i < hs.length; i++) yaw.push((Math.abs(wrap(wrap(hs[i] - hs[i - 1]) - wrap(hs[i - 1] - hs[i - 2]))) * 180) / Math.PI);
  const stat = (a) => {
    if (a.length === 0) return null;
    const s = [...a].sort((x, y) => x - y);
    return { rms: Math.round(Math.sqrt(a.reduce((q, x) => q + x * x, 0) / a.length) * 1000) / 1000, p95: Math.round(s[Math.floor(s.length * 0.95)] * 1000) / 1000, max: Math.round(s[s.length - 1] * 1000) / 1000 };
  };
  return { frames: px.length, jitterPx: stat(d2), yawJerkDeg: stat(yaw), vehicleSamples: samples, insideBuildingViewer: viewerInside, insideBuildingEngine: engineInside };
}

/** Signal heads that disagree with the stream's state at the drawn instant (keyframe + deltas). */
async function signalAgreement() {
  const engine = window.__vwpStudio.engine;
  const v = engine.viewer;
  const log = [];
  const take = (keyframe) => (m) => {
    if (!keyframe && m.signals.count === 0) return;
    log.push({ t: Number(m.simTimeNs) / 1e9, keyframe, ids: Array.from(m.signals.signalId.subarray(0, m.signals.count)), phases: Array.from(m.signals.phase.subarray(0, m.signals.count)) });
  };
  const offK = engine.client.onKeyframe(take(true));
  const offD = engine.client.onDelta(take(false));
  let checks = 0;
  let wrong = 0;
  let heads = 0;
  const t0 = performance.now();
  while (performance.now() - t0 < 6000) {
    await new Promise((r) => setTimeout(r, 250));
    const at = v.interpolator.renderSimSeconds;
    let start = -1;
    for (let i = 0; i < log.length; i++) if (log[i].keyframe && log[i].t <= at + 1e-6) start = i;
    if (start < 0) continue;
    const state = new Map();
    for (let i = start; i < log.length && log[i].t <= at + 1e-6; i++) log[i].ids.forEach((id, k) => state.set(id, log[i].phases[k]));
    const s = v.worldRenderer.signals;
    heads = s.count;
    checks++;
    for (let i = 0; i < s.count; i++) {
      const h = s.headState(i);
      const want = state.has(h.signalId) ? state.get(h.signalId) : 0xff;
      if (h.phase !== want) wrong++;
    }
  }
  offK();
  offD();
  return { heads, checks, headChecks: heads * checks, wrong };
}

async function main() {
  await rm(OPTIONS.out, { recursive: true, force: true });
  await mkdir(OPTIONS.out, { recursive: true });

  let base = OPTIONS.url;
  if (!base) {
    start("node", [
      join(UI, "packages/mock-server/dist/index.js"),
      "--actors", OPTIONS.actors,
      "--port", OPTIONS.enginePort,
      "--seed", OPTIONS.seed,
      "--quiet",
    ], { cwd: UI });
    await waitFor(`http://127.0.0.1:${OPTIONS.enginePort}/healthz`, "the mock engine");

    start("pnpm", ["exec", "vite", "--port", OPTIONS.studioPort, "--strictPort"], {
      cwd: STUDIO,
      env: { ...process.env, VWP_ENGINE: `http://127.0.0.1:${OPTIONS.enginePort}`, VWP_STUDIO_PORT: OPTIONS.studioPort },
    });
    base = `http://127.0.0.1:${OPTIONS.studioPort}`;
    await waitFor(base, "the Studio dev server");
  }

  const browser = await chromium.launch({
    headless: true,
    args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"],
  });
  const page = await browser.newPage({
    viewport: { width: OPTIONS.width, height: OPTIONS.height },
    deviceScaleFactor: 1,
  });
  const problems = [];
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  page.on("console", (m) => {
    if (m.type() === "error" && !(m.text().includes("WebGL") && m.text().includes("deprecat"))) {
      problems.push(`console: ${m.text()}`);
    }
  });

  const stops = [];
  const shot = async (name, note) => {
    const facts = await page.evaluate(readFacts);
    const file = `${String(stops.length + 1).padStart(2, "0")}-${name}.jpg`;
    await page.screenshot({ path: join(OPTIONS.out, file), type: "jpeg", quality: 62 });
    stops.push({ stop: name, note, file, ...facts });
    return facts;
  };

  const rpc = (method, params = {}) =>
    page.evaluate(
      ([m, p]) => window.__vwpStudio.engine.request(m, p).catch((e) => ({ error: String(e) })),
      [method, params],
    );

  /** Wait until the camera stops moving; the fly-down is an exponential on the frame clock. */
  const settle = async (budgetMs = 45_000) => {
    const deadline = Date.now() + budgetMs;
    let previous = (await page.evaluate(readFacts)).camera;
    for (;;) {
      await page.waitForTimeout(600);
      const now = (await page.evaluate(readFacts)).camera;
      const moved = Math.hypot(now[0] - previous[0], now[1] - previous[1], now[2] - previous[2]);
      previous = now;
      if (moved < 6 || Date.now() > deadline) return moved;
    }
  };

  await page.goto(base, { waitUntil: "domcontentloaded" });
  await page.waitForSelector('[data-testid="viewer-canvas"]', { timeout: 60_000 });
  await page.waitForFunction(
    () => document.querySelector('[data-testid="connection-state"]')?.getAttribute("data-state") === "streaming",
    undefined,
    { timeout: 60_000 },
  );
  await rpc("run.seek", { t_ns: 0, pause_after: false });
  await rpc("run.resume");
  await page.waitForFunction(() => (window.__vwpStudio?.actorCount() ?? 0) > 0, undefined, { timeout: 60_000 });
  await page.waitForTimeout(4000);

  // 1. The view the app opens on.
  await shot("aerial", "the view the app opens on, without touching anything");

  // 2. The same altitude, framed on the traffic. `Viewer.frameActors` is what the `f` key does, and
  //    the difference between this picture and the one before it is the aerial-framing question.
  await page.evaluate(() => window.__vwpStudio.engine.viewer.frameActors());
  await page.evaluate(() => window.__vwpStudio.engine.viewer.cameras.snap());
  await page.waitForTimeout(2000);
  await shot("aerial-framed", "the same altitude, framed on the live traffic");

  // 3–4. Pick a vehicle that has a radio, so the HUD and inspector fill, then fly down.
  const followed = await page.evaluate(() => {
    const engine = window.__vwpStudio.engine;
    const poses = engine.client?.poses;
    if (!poses) return null;
    let fallback = null;
    for (let slot = 0; slot < poses.count; slot++) {
      if (poses.occupied[slot] !== 1) continue;
      const id = poses.actorId[slot];
      if (fallback === null) fallback = id;
      if (engine.nodeByActor.get(id) !== undefined) {
        void engine.selectActor(id, "chase");
        return id;
      }
    }
    if (fallback !== null) void engine.selectActor(fallback, "chase");
    return fallback;
  });
  await page.waitForTimeout(1000);
  await shot("fly-down", `one second into the flight down to vehicle ${followed}`);

  const chaseResidual = await settle();
  await shot("chase", `behind vehicle ${followed}, settled (camera moving ${chaseResidual.toFixed(1)} m per 600 ms)`);
  const motion = await page.evaluate(measureMotion);
  const signals = await page.evaluate(signalAgreement);

  // 5. The same frame with the HUD out of the way, through the app's own "HUD dock" control rather
  //    than by hiding DOM from underneath it: if the car appears here and not in the picture
  //    before, the renderer is right and the panel is in front of it.
  await page.getByTestId("hud-dock").click();
  await page.waitForTimeout(1200);
  await shot("chase-hud-docked", "the same chase frame with the OBU HUD docked into the inspector");
  await page.getByTestId("hud-dock").click();
  await page.waitForTimeout(600);

  // 6. The driver's seat.
  await page.evaluate(() => window.__vwpStudio.engine.viewer.cameras.setMode("dashboard", false));
  await settle();
  await shot("dashboard", "from the driver's seat of the same vehicle");

  // 7. Back out to the map with the follow kept.
  await page.evaluate(() => window.__vwpStudio.engine.viewer.cameras.setMode("map", false));
  await settle();
  await shot("back-out", "returned to the map, still following the same vehicle");

  const manifest = {
    capturedAt: new Date().toISOString(),
    engine: OPTIONS.url ? `external (${OPTIONS.url})` : `vwp-mock-server --actors ${OPTIONS.actors} --seed ${OPTIONS.seed}`,
    window: `${OPTIONS.width}x${OPTIONS.height}`,
    followed,
    problems,
    motion,
    signals,
    stops,
  };
  await writeFile(join(OPTIONS.out, "tour.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");

  // A one-screen summary on stdout, because the numbers are what turn the pictures into evidence.
  const columns = ["file", "mode", "live", "drawn", "culled", "subjectNdc", "subjectCoveredBy", "near", "insets", "fps"];
  const rows = stops.map((s) => columns.map((c) => String(s[c] ?? "—")));
  const widths = columns.map((c, i) => Math.max(c.length, ...rows.map((r) => r[i].length)));
  const line = (cells) => cells.map((cell, i) => cell.padEnd(widths[i])).join("  ");
  console.log(line(columns));
  console.log(widths.map((w) => "-".repeat(w)).join("  "));
  for (const row of rows) console.log(line(row));

  console.log(`\nchase motion over 3 s (${motion.frames} frames): jitter ${JSON.stringify(motion.jitterPx)} px, heading ${JSON.stringify(motion.yawJerkDeg)} deg`);
  console.log(`vehicles inside buildings: ${motion.insideBuildingViewer} viewer-caused, ${motion.insideBuildingEngine} engine-placed, of ${motion.vehicleSamples} samples`);
  console.log(`signal heads: ${signals.wrong} wrong of ${signals.headChecks} head-checks (${signals.heads} heads, ${signals.checks} checks)`);

  let bytes = 0;
  for (const name of await readdir(OPTIONS.out)) bytes += (await stat(join(OPTIONS.out, name))).size;
  console.log(`\n${stops.length} stops in ${OPTIONS.out} (${(bytes / 1024).toFixed(0)} kB total)`);
  if (problems.length > 0) console.log(`\n${problems.length} page problem(s):\n  ${problems.slice(0, 8).join("\n  ")}`);

  await browser.close();
}

try {
  await main();
} finally {
  if (!OPTIONS.keep) stopAll();
}
