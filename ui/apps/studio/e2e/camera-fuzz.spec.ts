/**
 * "Sometimes the camera gets broken and it blacks out or goes into the ground."
 *
 * A seeded random walk through everything a user and a run can do to the camera — every mode,
 * following and dropping a vehicle, orbit drags past the horizon, zooming to both ends, seeking,
 * rewinding, running off the end, pausing, reconnecting, resizing the window, docking the HUD —
 * and after every step the four properties that "broken" means, checked against the real page:
 *
 *   1. the camera's transform is finite;
 *   2. it is above the ground under what it is looking at;
 *   3. it is not inside a building that is drawn;
 *   4. the frame is not black: not a single dark colour, and — outside the free camera, which may
 *      legitimately look at the sky — the world is actually in it (hiding the world's geometry must
 *      change the picture: the differential method of `scene-validation.spec.ts`).
 *
 * The seed is printed and fixed per run (`VWP_FUZZ_SEED`), so a failure replays exactly; the log of
 * steps that led to it is in the failure message.
 */

import { expect, test, type Page } from "@playwright/test";

const SEED = Number(process.env.VWP_FUZZ_SEED ?? 20260923);
const STEPS = Number(process.env.VWP_FUZZ_STEPS ?? 36);

function prng(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s / 0x1_0000_0000;
  };
}

interface Check {
  readonly finite: boolean;
  readonly mode: string;
  readonly camera: [number, number, number];
  readonly groundZ: number;
  readonly insideTop: number;
  readonly lumaMean: number;
  readonly lumaStd: number;
  /** Fraction of pixels that changed when the world's geometry was hidden. */
  readonly worldSignal: number;
  readonly followed: number | null;
  /** Near/far, fog, look target and flight state, for the failure message. */
  readonly detail: string;
}

/** Render one frame and measure it, in one task (the drawing buffer is not preserved). */
function check(page: Page): Promise<Check> {
  return page.evaluate(() => {
    const engine = (window as unknown as { __vwpStudio: { engine: Record<string, unknown> } }).__vwpStudio.engine;
    const v = engine.viewer as {
      camera: { position: { x: number; y: number; z: number }; quaternion: { x: number; y: number; z: number; w: number }; projectionMatrix: { elements: number[] }; near: number; far: number };
      cameras: { mode: string; followActorId: number | null; look: { x: number; y: number; z: number } };
      interpolator: { count: number; outOccupied: Uint8Array; outActorId: Uint32Array; outPosition: Float32Array };
      worldRenderer: { world: { bbox: { minZM: number } } | null; buildingTopAt(x: number, y: number): number; group: { visible: boolean }; tiles: { visible: boolean }; markings: { visible: boolean }; buildingsGroup: { visible: boolean }; ground: { visible: boolean }; signalsGroup: { visible: boolean }; sitesGroup: { visible: boolean } };
      actors: { group: { visible: boolean } };
      overlays: { group: { visible: boolean } };
      step(dt: number): unknown;
    };
    const canvas = document.querySelector('[data-testid="viewer-canvas"]') as HTMLCanvasElement;
    const gl = (canvas.getContext("webgl2") ?? canvas.getContext("webgl")) as WebGLRenderingContext;
    const cam = v.camera;
    const p = cam.position;
    const q = cam.quaternion;
    const finite = [p.x, p.y, p.z, q.x, q.y, q.z, q.w, cam.near, cam.far, ...cam.projectionMatrix.elements].every(Number.isFinite);

    const w = v.worldRenderer;
    let groundZ = w.world ? w.world.bbox.minZM : 0;
    const id = v.cameras.followActorId;
    if (id !== null && (v.cameras.mode === "chase" || v.cameras.mode === "dashboard")) {
      for (let i = 0; i < v.interpolator.count; i++) {
        if (v.interpolator.outOccupied[i] === 1 && v.interpolator.outActorId[i] === id >>> 0) {
          groundZ = Math.max(groundZ, v.interpolator.outPosition[i * 3 + 2]);
          break;
        }
      }
    }
    const insideTop = w.buildingTopAt(p.x, p.y);

    // Sample the frame on a coarse grid: enough to tell a picture from a wash.
    const W = canvas.width;
    const H = canvas.height;
    const grab = (): Uint8Array => {
      const buf = new Uint8Array(W * H * 4);
      gl.readPixels(0, 0, W, H, gl.RGBA, gl.UNSIGNED_BYTE, buf);
      return buf;
    };
    const overlays = v.overlays.group.visible;
    v.overlays.group.visible = false;
    v.step(0);
    const shown = grab();
    const parts = [w.tiles, w.markings, w.buildingsGroup, w.ground, w.signalsGroup, w.sitesGroup, v.actors.group];
    const was = parts.map((o) => o.visible);
    for (const o of parts) o.visible = false;
    v.step(0);
    const bare = grab();
    parts.forEach((o, i) => (o.visible = was[i]));
    v.overlays.group.visible = overlays;
    v.step(0);

    let sum = 0;
    let sumSq = 0;
    let n = 0;
    let changed = 0;
    const stride = 4 * 7; // every 7th pixel
    for (let i = 0; i < shown.length; i += stride) {
      const y = 0.2126 * shown[i] + 0.7152 * shown[i + 1] + 0.0722 * shown[i + 2];
      sum += y;
      sumSq += y * y;
      n++;
      if (Math.abs(shown[i] - bare[i]) > 8 || Math.abs(shown[i + 1] - bare[i + 1]) > 8 || Math.abs(shown[i + 2] - bare[i + 2]) > 8) changed++;
    }
    const mean = sum / Math.max(1, n);
    return {
      finite,
      mode: v.cameras.mode,
      camera: [p.x, p.y, p.z] as [number, number, number],
      groundZ,
      insideTop,
      lumaMean: mean,
      lumaStd: Math.sqrt(Math.max(0, sumSq / Math.max(1, n) - mean * mean)),
      worldSignal: changed / Math.max(1, n),
      followed: id,
      detail: JSON.stringify({
        look: [v.cameras.look.x, v.cameras.look.y, v.cameras.look.z].map((x) => Math.round(x * 10) / 10),
        near: Math.round(cam.near * 100) / 100,
        far: Math.round(cam.far),
        fog: (v as unknown as { scene: { fog: { near: number; far: number } | null } }).scene.fog
          ? [Math.round((v as unknown as { scene: { fog: { near: number } } }).scene.fog.near), Math.round((v as unknown as { scene: { fog: { far: number } } }).scene.fog.far)]
          : null,
        flying: (v.cameras as unknown as { isFlying: boolean }).isFlying,
        ghost: (w as unknown as { ghostBuilding: number }).ghostBuilding,
      }),
    };
  });
}

function rpc<T>(page: Page, method: string, params: Record<string, unknown> = {}): Promise<T | { error: string }> {
  return page.evaluate(
    async ([m, p]) => {
      const engine = (window as unknown as { __vwpStudio: { engine: { request(m: string, p: unknown): Promise<unknown> } } }).__vwpStudio.engine;
      try {
        return await engine.request(m as string, p);
      } catch (e) {
        return { error: String(e) };
      }
    },
    [method, params] as [string, Record<string, unknown>],
  ) as Promise<T | { error: string }>;
}

async function streaming(page: Page): Promise<void> {
  await page.goto("/");
  await expect(page.getByTestId("viewer-canvas")).toBeVisible();
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "streaming", { timeout: 60_000 });
  const s = (await rpc<{ state: string }>(page, "run.status")) as { state?: string };
  if (s.state === "finished") await rpc(page, "run.start");
  await rpc(page, "run.resume");
  await expect
    .poll(async () => page.evaluate(() => (window as unknown as { __vwpStudio?: { actorCount(): number } }).__vwpStudio?.actorCount() ?? 0), { timeout: 60_000 })
    .toBeGreaterThan(0);
  await page.waitForTimeout(1500);
}

test.afterEach(async ({ page }) => {
  await rpc(page, "run.seek", { t_ns: 0, pause_after: false });
  await rpc(page, "run.speed", { speed: 1 });
  await rpc(page, "run.resume");
});

test(`the camera survives a random walk through every control (seed ${SEED})`, async ({ page }, testInfo) => {
  test.setTimeout(Math.max(300_000, STEPS * 15_000));
  await page.setViewportSize({ width: 1280, height: 800 });
  await streaming(page);
  const rnd = prng(SEED);
  const pick = <T,>(xs: readonly T[]): T => xs[Math.floor(rnd() * xs.length)];
  const log: string[] = [];
  const failures: string[] = [];
  let shots = 0;
  const canvas = page.getByTestId("viewer-canvas");

  const actions: Record<string, () => Promise<string>> = {
    mode: async () => {
      const m = pick(["map", "chase", "dashboard", "free", "rsu"]);
      await page.evaluate((mode) => (window as unknown as { __vwpStudio: { engine: { setCameraMode(m: string): void } } }).__vwpStudio.engine.setCameraMode(mode), m);
      return `mode ${m}`;
    },
    follow: async () => {
      const id = await page.evaluate((r) => {
        const e = (window as unknown as { __vwpStudio: { engine: { client: { poses: { count: number; occupied: Uint8Array; actorId: Uint32Array } } | null; selectActor(id: number | null, m?: string): Promise<void> } } }).__vwpStudio.engine;
        const p = e.client?.poses;
        if (!p) return null;
        const live: number[] = [];
        for (let s = 0; s < p.count; s++) if (p.occupied[s] === 1) live.push(p.actorId[s]);
        if (live.length === 0) return null;
        const id = live[Math.floor(r * live.length)];
        void e.selectActor(id, "chase");
        return id;
      }, rnd());
      return `follow ${id}`;
    },
    unfollow: async () => {
      await page.evaluate(() => (window as unknown as { __vwpStudio: { engine: { selectActor(id: number | null): Promise<void> } } }).__vwpStudio.engine.selectActor(null));
      return "unfollow";
    },
    drag: async () => {
      const box = await canvas.boundingBox();
      if (!box) return "drag (no canvas)";
      const x = box.x + box.width / 2;
      const y = box.y + box.height / 2;
      const dx = (rnd() - 0.5) * 1200;
      const dy = (rnd() - 0.5) * 1600;
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.move(x + dx / 2, y + dy / 2, { steps: 4 });
      await page.mouse.move(x + dx, y + dy, { steps: 4 });
      await page.mouse.up();
      return `drag ${dx.toFixed(0)},${dy.toFixed(0)}`;
    },
    wheel: async () => {
      const box = await canvas.boundingBox();
      if (!box) return "wheel (no canvas)";
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      const dy = rnd() < 0.5 ? -3000 : 3000;
      for (let i = 0; i < 4; i++) await page.mouse.wheel(0, dy / 4);
      return `wheel ${dy}`;
    },
    seek: async () => {
      const s = (await rpc<{ t_end_ns: number | string }>(page, "run.status")) as { t_end_ns?: number | string };
      const end = Number(s.t_end_ns ?? 0);
      const t = Math.floor(rnd() * Math.max(1, end));
      const pause = rnd() < 0.3;
      await rpc(page, "run.seek", { t_ns: t, pause_after: pause });
      return `seek ${(t / 1e9).toFixed(1)} s${pause ? " paused" : ""}`;
    },
    rewind: async () => {
      await rpc(page, "run.seek", { t_ns: 0, pause_after: false });
      return "rewind";
    },
    runEnd: async () => {
      const s = (await rpc<{ t_end_ns: number | string }>(page, "run.status")) as { t_end_ns?: number | string };
      const end = Number(s.t_end_ns ?? 0);
      await rpc(page, "run.seek", { t_ns: Math.max(0, end - 500_000_000), pause_after: false });
      await rpc(page, "run.resume");
      return "run to the end";
    },
    pause: async () => {
      const s = (await rpc<{ state: string }>(page, "run.status")) as { state?: string };
      if (s.state === "paused") {
        await rpc(page, "run.resume");
        return "resume";
      }
      await rpc(page, "run.pause");
      return "pause";
    },
    speed: async () => {
      const x = pick([0.25, 1, 4]);
      await rpc(page, "run.speed", { speed: x });
      return `speed ${x}`;
    },
    resize: async () => {
      const size = pick([{ width: 1280, height: 800 }, { width: 960, height: 600 }, { width: 1680, height: 1050 }, { width: 800, height: 520 }]);
      await page.setViewportSize(size);
      return `resize ${size.width}x${size.height}`;
    },
    hud: async () => {
      // The control has to be usable at every window size the walk visits: at 960 x 600 the state
      // legend used to sit on it, and at 800 x 520 the HUD itself.
      try {
        await page.getByTestId("hud-dock").click({ timeout: 5000 });
        return "toggle HUD dock";
      } catch {
        const size = page.viewportSize();
        failures.push(`the HUD dock button is not clickable at ${size?.width}x${size?.height}`);
        return "toggle HUD dock (not clickable)";
      }
    },
    reconnect: async () => {
      await page.evaluate(async () => {
        const e = (window as unknown as { __vwpStudio: { engine: { reopenStream(): Promise<void> } } }).__vwpStudio.engine;
        await e.reopenStream().catch(() => undefined);
      });
      return "reconnect";
    },
  };
  const names = Object.keys(actions);

  for (let step = 0; step < STEPS; step++) {
    const name = names[Math.floor(rnd() * names.length)];
    const what = await actions[name]();
    log.push(`${step}: ${what}`);
    // Checked twice: straight after the action (a cut, a resize) and once the camera has had time
    // to fly wherever the action sent it.
    for (const wait of [250, 700 + Math.floor(rnd() * 900)]) {
      await page.waitForTimeout(wait);
      // A run that ran off the end stops streaming; bring it back before measuring.
      const c = await check(page);
      const where = `step ${step} (${what}) +${wait} ms, mode ${c.mode}, camera ${c.camera.map((x) => x.toFixed(1)).join(",")}`;
      if (!c.finite) failures.push(`${where}: non-finite camera transform`);
      if (c.camera[2] < c.groundZ + 0.3) failures.push(`${where}: camera ${(c.camera[2] - c.groundZ).toFixed(2)} m relative to the ground`);
      if (c.insideTop > c.camera[2]) failures.push(`${where}: camera inside a building (roof at ${c.insideTop.toFixed(1)} m)`);
      if (c.lumaStd < 2.5) failures.push(`${where}: flat frame (luma ${c.lumaMean.toFixed(1)} ± ${c.lumaStd.toFixed(1)})`);
      if (c.mode !== "free" && c.worldSignal < 0.02) failures.push(`${where}: the world is not in the frame (${(c.worldSignal * 100).toFixed(1)} % of it depends on it)`);
      if (failures.length > shots && shots < 3) {
        failures[failures.length - 1] += ` ${c.detail}`;
        // eslint-disable-next-line no-console
        console.log(`FUZZ FAILURE ${failures[failures.length - 1]}`);
        await page.screenshot({ path: testInfo.outputPath(`fuzz-failure-${shots}.jpg`), type: "jpeg", quality: 50 });
        shots = failures.length;
      }
    }
  }
  // eslint-disable-next-line no-console
  console.log(`camera fuzz, seed ${SEED}: ${STEPS} steps, ${failures.length} failure(s)\n  ${log.join("\n  ")}`);
  expect(failures, `seed ${SEED}\n${failures.slice(0, 12).join("\n")}\n--- steps ---\n${log.join("\n")}`).toEqual([]);
});
